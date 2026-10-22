//! Application state: `libamdgpu_top` apps, samplers, history, and UI state.

use std::collections::HashMap;
use std::fs;
use std::os::fd::AsRawFd;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, RecvTimeoutError, Sender};
use std::time::{Duration, Instant};

use libamdgpu_top::app::{AppAmdgpuTop, AppOption};
use libamdgpu_top::{DevicePath, stat};
use ratatui::widgets::TableState;

use crate::config::CollapseState;
use crate::cpu::{CpuSampler, SystemMem, cpu_model};
use crate::history::History;
use crate::kill::{self, KillTarget};
use crate::processes::{self, GpuProc, ProcDetail, ProcRow, ProcessCpuSampler, SortKey};
use crate::theme::{DEFAULT_THEME, Theme};

const HISTORY_CAPACITY: usize = 80;
/// How long a one-line action result stays in the panel title.
const NOTICE_TTL: Duration = Duration::from_secs(4);
const PROCESS_INDEX_REFRESH_SECS: u64 = 5;

#[derive(PartialEq, Eq, Copy, Clone)]
pub enum Section {
    Cpu,
    Gpu,
    Npu,
    Processes,
}

impl Section {
    pub const ALL: [Section; 4] = [Section::Cpu, Section::Gpu, Section::Npu, Section::Processes];
}

pub struct GpuDevice {
    pub device_path: DevicePath,
    pub app: Option<AppAmdgpuTop>,
    pub hist_gpu: History,
    pub hist_mem: History,
}

impl GpuDevice {
    fn new(device_path: DevicePath, app: Option<AppAmdgpuTop>) -> Self {
        Self {
            device_path,
            app,
            hist_gpu: History::new(HISTORY_CAPACITY),
            hist_mem: History::new(HISTORY_CAPACITY),
        }
    }

    pub fn is_sleeping(&self) -> bool {
        !self.device_path.check_if_device_is_active()
    }

    fn try_activate(&mut self) -> bool {
        let is_awake = self.device_path.check_if_device_is_active();
        let device_path = self.device_path.clone();
        activate_if_awake(&mut self.app, is_awake, || {
            let amdgpu_dev = device_path.init().ok()?;
            AppAmdgpuTop::new(amdgpu_dev, device_path, &AppOption::default())
        })
    }
}

pub struct App {
    pub gpus: Vec<GpuDevice>,
    pub cpu: CpuSampler,
    pub mem: SystemMem,
    /// Rows behind the PROCESSES panel; every process, GPU client or not.
    pub proc_rows: Vec<ProcRow>,
    /// Baselines for the per-process CPU column, one entry per live pid.
    pub proc_cpu: ProcessCpuSampler,
    /// Every process of the last `/proc` sample, before the `a` filter.
    proc_snapshot: Vec<ProcRow>,
    /// `None` = unsorted (PID order). Sorting is always descending; the
    /// left/right arrows walk the columns.
    pub proc_sort: Option<SortKey>,
    /// `true`: list every process. `false`: only DRM clients.
    pub proc_all: bool,
    /// Selected row + scroll offset for the PROCESSES table.
    pub proc_state: TableState,
    /// Non-empty while the "stop process?" confirmation is on screen.
    pub kill_prompt: Option<KillTarget>,
    /// `Some` while the detail pane under the table is open.
    pub proc_detail: Option<ProcDetail>,
    /// Transient result of the last action (SIGTERM sent, refusal, …).
    notice: Option<(String, Instant)>,
    pub collapse: CollapseState,
    pub section: Section,
    pub hist_cpu: History,
    pub hist_npu: History,
    pub hist_cores: Vec<History>, // per logical CPU
    pub npu_info: Option<NpuInfo>,
    pub has_npu: bool,
    pub theme: Theme,
    pub theme_name: String,
    pub themes: Vec<String>,
    pub block_style: usize,
    pub cpu_model: String,
    process_index_sender: Sender<Vec<DevicePath>>,
}

#[derive(Clone, Debug)]
pub struct NpuInfo {
    pub name: String,
    pub bdf: String,
    pub fw_version: Option<String>,
    pub fdinfo_supported: bool,
}

struct DiscoveredDevices {
    gpus: Vec<GpuDevice>,
    process_device_paths: Vec<DevicePath>,
    npu_info: Option<NpuInfo>,
}

impl App {
    pub fn init() -> Self {
        let DiscoveredDevices {
            gpus,
            process_device_paths,
            npu_info,
        } = discover_devices();
        let has_npu = npu_info.is_some();
        let process_index_sender = spawn_process_index_thread(process_device_paths);

        let collapse = CollapseState::load();
        let theme_name = if collapse.theme.is_empty() {
            DEFAULT_THEME.to_string()
        } else {
            collapse.theme.clone()
        };
        let theme = Theme::load(&theme_name);
        let block_style = collapse.block_style % crate::gauge::BLOCK_STYLES.len();
        let proc_sort = SortKey::from_id(&collapse.proc_sort);
        let proc_all = collapse.proc_all;

        Self {
            gpus,
            cpu: CpuSampler::default(),
            mem: SystemMem::default(),
            proc_rows: Vec::new(),
            proc_cpu: ProcessCpuSampler::default(),
            proc_snapshot: Vec::new(),
            proc_sort,
            proc_all,
            proc_state: TableState::default(),
            kill_prompt: None,
            proc_detail: None,
            notice: None,
            collapse,
            section: Section::Gpu,
            hist_cpu: History::new(HISTORY_CAPACITY),
            hist_npu: History::new(HISTORY_CAPACITY),
            hist_cores: Vec::new(),
            npu_info,
            has_npu,
            theme,
            theme_name,
            themes: Theme::list_available(),
            block_style,
            cpu_model: cpu_model(),
            process_index_sender,
        }
    }

    pub fn refresh_devices(&mut self) {
        let DiscoveredDevices {
            gpus,
            process_device_paths,
            npu_info,
        } = discover_devices();
        let previous_gpus = std::mem::take(&mut self.gpus);

        self.gpus = pair_by_key(
            gpus,
            previous_gpus,
            |gpu| {
                let device = &gpu.device_path;
                (device.pci, device.device_id, device.revision_id)
            },
            |gpu| {
                let device = &gpu.device_path;
                (device.pci, device.device_id, device.revision_id)
            },
        )
        .into_iter()
        .map(|(mut refreshed, previous)| {
            if let Some(previous) = previous {
                refreshed.hist_gpu = previous.hist_gpu;
                refreshed.hist_mem = previous.hist_mem;
            }
            refreshed
        })
        .collect();

        if !same_npu_device(self.npu_info.as_ref(), npu_info.as_ref()) {
            self.hist_npu = History::new(HISTORY_CAPACITY);
        }
        self.npu_info = npu_info;
        self.has_npu = self.npu_info.is_some();
        let _ = self.process_index_sender.send(process_device_paths);
    }

    pub fn cycle_theme(&mut self, forward: bool) -> std::io::Result<()> {
        if self.themes.is_empty() {
            return Ok(());
        }
        let idx = self
            .themes
            .iter()
            .position(|t| t == &self.theme_name)
            .unwrap_or(0);
        let len = self.themes.len();
        let next = if forward {
            (idx + 1) % len
        } else {
            (idx + len - 1) % len
        };
        self.theme_name = self.themes[next].clone();
        self.theme = Theme::load(&self.theme_name);
        self.collapse.theme = self.theme_name.clone();
        self.save_state()
    }

    pub fn cycle_block(&mut self, forward: bool) -> std::io::Result<()> {
        let len = crate::gauge::BLOCK_STYLES.len();
        self.block_style = if forward {
            (self.block_style + 1) % len
        } else {
            (self.block_style + len - 1) % len
        };
        self.collapse.block_style = self.block_style;
        self.save_state()
    }

    pub fn block_style_name(&self) -> &'static str {
        crate::gauge::block_style(self.block_style).name
    }

    pub fn active_apps(&self) -> impl Iterator<Item = &AppAmdgpuTop> {
        self.gpus.iter().filter_map(|gpu| gpu.app.as_ref())
    }

    pub fn sample(&mut self, interval: Duration) {
        let mut device_activated = false;
        for gpu in &mut self.gpus {
            device_activated |= gpu.try_activate();
            if let Some(app) = gpu.app.as_mut() {
                app.update(interval);
            }
        }
        send_process_targets_after_activation(&self.process_index_sender, device_activated, || {
            process_device_paths(&self.gpus)
        });
        self.sample_processes(interval);
        self.cpu.tick();
        self.mem.tick();

        // CPU history
        self.hist_cpu.push(self.cpu.cpu_percent.round() as u64);

        // per-core history
        if self.hist_cores.len() != self.cpu.per_core_percent.len() {
            self.hist_cores = (0..self.cpu.per_core_percent.len())
                .map(|_| History::new(HISTORY_CAPACITY))
                .collect();
        }
        for (i, p) in self.cpu.per_core_percent.iter().enumerate() {
            self.hist_cores[i].push(p.round() as u64);
        }

        // GPU / MEM history per device. Leave sleeping-device histories
        // untouched so sleep is not misrepresented as measured zero activity.
        for gpu in &mut self.gpus {
            if gpu.is_sleeping() {
                continue;
            }
            if let Some(app) = gpu.app.as_ref() {
                let gfx = app.stat.activity.gfx.unwrap_or(0) as u64;
                gpu.hist_gpu.push(gfx);
                let memory = gpu_mem_info(app);
                gpu.hist_mem.push(memory.percent.round() as u64);
            }
        }

        // NPU aggregate (sum of per-context npu%, clamped)
        let npu_sum = self
            .active_apps()
            .flat_map(|app| &app.stat.xdna_fdinfo.proc_usage)
            .map(|process| process.usage.npu.max(0))
            .sum::<i64>();
        self.hist_npu.push(npu_sum.clamp(0, 100) as u64);
    }

    fn sample_processes(&mut self, interval: Duration) {
        let mut gpu_map: HashMap<i32, GpuProc> = HashMap::new();
        for (gpu_index, gpu) in self.gpus.iter().enumerate() {
            let Some(app) = gpu.app.as_ref() else {
                continue;
            };
            for usage in &app.stat.fdinfo.proc_usage {
                let entry = GpuProc {
                    gpu_index,
                    vram_kb: usage.usage.vram_usage,
                    gtt_kb: usage.usage.gtt_usage,
                    gfx: usage.usage.gfx,
                    compute: usage.usage.compute,
                    dma: usage.usage.dma,
                };
                gpu_map
                    .entry(usage.pid)
                    .and_modify(|existing| existing.absorb(&entry))
                    .or_insert(entry);
            }
        }

        let rows = processes::collect(&mut self.proc_cpu, &gpu_map, interval);
        self.proc_snapshot = rows;
        self.rebuild_proc_view();
    }

    /// Derive the visible table from the last `/proc` snapshot. The filter is a
    /// pure view on top of it, so `a` does not need a second read of `/proc`
    /// whose CPU delta would cover a fraction of the sampling interval.
    fn rebuild_proc_view(&mut self) {
        self.proc_rows = processes::filter_rows(&self.proc_snapshot, !self.proc_all);
        self.sort_proc_rows();
        self.clamp_proc_selection();
        self.sync_proc_detail();
    }

    fn sort_proc_rows(&mut self) {
        match self.proc_sort {
            Some(key) => processes::sort_rows(&mut self.proc_rows, key),
            None => processes::sort_by_pid(&mut self.proc_rows),
        }
    }

    fn clamp_proc_selection(&mut self) {
        if self.proc_rows.is_empty() {
            self.proc_state.select(None);
            return;
        }
        let selected = self
            .proc_state
            .selected()
            .unwrap_or(0)
            .min(self.proc_rows.len() - 1);
        self.proc_state.select(Some(selected));
    }

    fn persist_proc_prefs(&mut self) {
        self.collapse.proc_sort = self
            .proc_sort
            .map(|key| key.id().to_string())
            .unwrap_or_default();
        self.collapse.proc_all = self.proc_all;
    }

    /// Right arrow: move the column cursor right and sort that column
    /// biggest-first. From "no sort" the cursor enters the cycle at TOTAL and
    /// wraps around after the last column.
    pub fn proc_sort_right(&mut self) {
        self.set_proc_sort(match self.proc_sort {
            None => SortKey::Total,
            Some(key) => key.next_column(),
        });
    }

    /// Left arrow: same ring, walked backwards. From "no sort" it also enters
    /// at TOTAL, then wraps to the last column.
    pub fn proc_sort_left(&mut self) {
        self.set_proc_sort(match self.proc_sort {
            None => SortKey::Total,
            Some(key) => key.prev_column(),
        });
    }

    fn set_proc_sort(&mut self, key: SortKey) {
        self.proc_sort = Some(key);
        self.proc_state.select(Some(0));
        self.sort_proc_rows();
        self.persist_proc_prefs();
    }

    /// Esc closes the detail pane when it is open.
    pub fn close_proc_detail(&mut self) -> bool {
        self.proc_detail.take().is_some()
    }

    /// Esc unwinds one layer per press: detail pane, then column sort.
    /// Returns false when there is nothing left to dismiss (so Esc can quit).
    pub fn dismiss_top_layer(&mut self) -> bool {
        if self.close_proc_detail() {
            return true;
        }
        self.cancel_proc_sort()
    }

    /// Esc: drop the column sort and return to the PID-ordered view.
    /// Returns false when there was nothing to cancel (so Esc can still quit).
    pub fn cancel_proc_sort(&mut self) -> bool {
        if self.proc_sort.is_none() {
            return false;
        }
        self.proc_sort = None;
        self.proc_state.select(Some(0));
        self.sort_proc_rows();
        self.persist_proc_prefs();
        true
    }

    /// `a`: every process vs. GPU clients only.
    pub fn toggle_proc_filter(&mut self) {
        self.proc_all = !self.proc_all;
        self.proc_state.select(Some(0));
        self.rebuild_proc_view();
        self.persist_proc_prefs();
    }

    pub fn proc_scroll(&mut self, delta: isize) {
        if self.proc_rows.is_empty() {
            self.proc_state.select(None);
            return;
        }
        let last = (self.proc_rows.len() - 1) as isize;
        let current = self.proc_state.selected().unwrap_or(0) as isize;
        self.proc_state
            .select(Some((current + delta).clamp(0, last) as usize));
        self.sync_proc_detail();
    }

    pub fn proc_select_edge(&mut self, last: bool) {
        if self.proc_rows.is_empty() {
            self.proc_state.select(None);
            return;
        }
        let index = if last { self.proc_rows.len() - 1 } else { 0 };
        self.proc_state.select(Some(index));
        self.sync_proc_detail();
    }

    /// GPU memory that cannot be attributed to a readable process, summed over
    /// every device (VRAM + GTT).
    pub fn gpu_unattributed_bytes(&self) -> u64 {
        let mut system_used = 0u64;
        let mut attributed_kib = 0u64;
        for gpu in &self.gpus {
            let Some(app) = gpu.app.as_ref() else {
                continue;
            };
            let usage = &app.stat.vram_usage.0;
            system_used += usage.vram.heap_usage + usage.gtt.heap_usage;
            for process in &app.stat.fdinfo.proc_usage {
                attributed_kib += process.usage.vram_usage + process.usage.gtt_usage;
            }
        }
        unattributed_gpu_bytes(system_used, attributed_kib)
    }

    /// Row currently under the cursor.
    pub fn selected_proc(&self) -> Option<&ProcRow> {
        self.proc_state
            .selected()
            .and_then(|index| self.proc_rows.get(index))
    }

    /// `d`: show / hide the detail pane for the selected process.
    pub fn toggle_proc_detail(&mut self) {
        if self.proc_detail.is_some() {
            self.proc_detail = None;
            return;
        }
        if let Some(row) = self.selected_proc() {
            self.proc_detail = processes::collect_detail(row.pid);
        }
    }

    /// Re-read the detail pane for whatever row is selected now.
    fn sync_proc_detail(&mut self) {
        if self.proc_detail.is_none() {
            return;
        }
        let Some(row) = self.selected_proc() else {
            self.proc_detail = None;
            return;
        };
        self.proc_detail = processes::collect_detail(row.pid);
    }

    /// `x`: open the confirmation dialog for the selected process.
    pub fn request_kill(&mut self) {
        let Some(row) = self.selected_proc() else {
            self.set_notice("no process selected".to_string());
            return;
        };
        let unit = processes::cgroup_info(row.pid);
        let target = KillTarget {
            pid: row.pid,
            name: row.name.clone(),
            total_kb: row.total_kb(),
            mem_kb: row.mem_kb,
            swap_kb: row.swap_kb,
            unit: unit.may_restart().then_some(unit.unit),
        };
        match kill::check_target(row.pid, row.uid, kill::effective_uid(), kill::self_pid()) {
            Ok(()) => self.kill_prompt = Some(target),
            Err(refusal) => self.set_notice(format!("{refusal}: {}", target.name)),
        }
    }

    /// `y` / Enter in the dialog: send SIGTERM (never SIGKILL).
    pub fn confirm_kill(&mut self) {
        let Some(target) = self.kill_prompt.take() else {
            return;
        };
        let message = match kill::send_sigterm(target.pid) {
            Ok(()) => format!("SIGTERM sent to {} ({})", target.name, target.pid),
            Err(error) => format!(
                "{}: {} ({})",
                kill::describe_error(&error),
                target.name,
                target.pid
            ),
        };
        self.set_notice(message);
    }

    /// `n` / Esc / q in the dialog.
    pub fn cancel_kill(&mut self) {
        self.kill_prompt = None;
    }

    fn set_notice(&mut self, message: String) {
        self.notice = Some((message, Instant::now()));
    }

    /// Fresh status message for the panel title, if it has not expired yet.
    pub fn notice(&self) -> Option<&str> {
        self.notice
            .as_ref()
            .and_then(|(message, at)| (at.elapsed() < NOTICE_TTL).then_some(message.as_str()))
    }

    pub fn save_state(&self) -> std::io::Result<()> {
        self.collapse.save()
    }

    pub fn next_section(&mut self) {
        let order: &[Section] = if self.has_npu {
            &Section::ALL
        } else {
            &[Section::Cpu, Section::Gpu, Section::Processes]
        };
        let idx = order.iter().position(|&s| s == self.section).unwrap_or(0);
        self.section = order[(idx + 1) % order.len()];
    }

    pub fn prev_section(&mut self) {
        let order: &[Section] = if self.has_npu {
            &Section::ALL
        } else {
            &[Section::Cpu, Section::Gpu, Section::Processes]
        };
        let idx = order.iter().position(|&s| s == self.section).unwrap_or(0);
        self.section = order[(idx + order.len() - 1) % order.len()];
    }

    pub fn toggle_collapse(&mut self) -> std::io::Result<()> {
        match self.section {
            Section::Cpu => self.collapse.cpu = !self.collapse.cpu,
            Section::Gpu => self.collapse.gpu = !self.collapse.gpu,
            Section::Npu => self.collapse.npu = !self.collapse.npu,
            Section::Processes => self.collapse.processes = !self.collapse.processes,
        }
        self.save_state()
    }

    pub fn is_collapsed(&self, s: Section) -> bool {
        match s {
            Section::Cpu => self.collapse.cpu,
            Section::Gpu => self.collapse.gpu,
            Section::Npu => self.collapse.npu,
            Section::Processes => self.collapse.processes,
        }
    }
}

fn discover_devices() -> DiscoveredDevices {
    let mut device_paths = DevicePath::get_device_path_list();
    // libamdgpu_top discovers devices through read_dir(), whose order is
    // unspecified. PCI order is deterministic and matches the physical
    // ordering used by ROCm's system-management tools.
    device_paths.sort_by_key(|device| {
        let pci = device.pci;
        (pci.domain, pci.bus, pci.dev, pci.func)
    });
    for device_path in &mut device_paths {
        device_path.fill_amdgpu_device_name();
    }

    // Initialize only devices that are already awake. The backend's
    // suspended-list helper wakes one device when every GPU is asleep,
    // which is undesirable for a system monitor.
    let active_apps: Vec<AppAmdgpuTop> = device_paths
        .iter()
        .filter(|device_path| device_path.check_if_device_is_active())
        .filter_map(|device_path| {
            let amdgpu_device = device_path.init().ok()?;
            AppAmdgpuTop::new(amdgpu_device, device_path.clone(), &AppOption::default())
        })
        .collect();
    let npu_info = detect_npu(&active_apps);
    let gpus = pair_by_key(
        device_paths,
        active_apps,
        |device_path| device_path.pci,
        |app| app.device_path.pci,
    )
    .into_iter()
    .map(|(device_path, app)| GpuDevice::new(device_path, app))
    .collect::<Vec<_>>();

    let process_device_paths = process_device_paths(&gpus);

    DiscoveredDevices {
        gpus,
        process_device_paths,
        npu_info,
    }
}

fn process_device_paths(gpus: &[GpuDevice]) -> Vec<DevicePath> {
    let mut device_paths = Vec::with_capacity(gpus.len() * 2);
    device_paths.extend(gpus.iter().map(|gpu| gpu.device_path.clone()));
    device_paths.extend(
        gpus.iter()
            .filter_map(|gpu| gpu.app.as_ref()?.xdna_device_path.clone()),
    );
    device_paths
}

fn send_process_targets_after_activation<T>(
    sender: &Sender<T>,
    device_activated: bool,
    targets: impl FnOnce() -> T,
) {
    if device_activated {
        let _ = sender.send(targets());
    }
}

// Keep one process-index worker for the application's lifetime, but let device
// activation or manual refresh replace its targets. This avoids leaking a
// permanent backend worker whenever the target set changes.
fn spawn_process_index_thread(initial_device_paths: Vec<DevicePath>) -> Sender<Vec<DevicePath>> {
    let (sender, receiver) = mpsc::channel();

    std::thread::spawn(move || {
        let mut device_paths = initial_device_paths;
        let mut index = Vec::new();
        let interval = Duration::from_secs(PROCESS_INDEX_REFRESH_SECS);

        loop {
            if !device_paths.is_empty() {
                let all_processes = stat::get_process_list();

                for device_path in &device_paths {
                    let paths: &[&PathBuf] = if device_path.is_amdgpu() {
                        &[&device_path.render, &device_path.card]
                    } else {
                        &[&device_path.accel]
                    };
                    stat::update_index_by_all_proc(&mut index, paths, &all_processes);

                    if let Ok(mut current_index) = device_path.arc_proc_index.lock() {
                        current_index.clone_from(&index);
                    }
                }
            }

            match receiver.recv_timeout(interval) {
                Ok(refreshed_device_paths) => {
                    device_paths = refreshed_device_paths;
                    while let Ok(newer_device_paths) = receiver.try_recv() {
                        device_paths = newer_device_paths;
                    }
                }
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => return,
            }
        }
    });

    sender
}

fn pair_by_key<L, R, K: PartialEq>(
    left: Vec<L>,
    mut right: Vec<R>,
    left_key: impl Fn(&L) -> K,
    right_key: impl Fn(&R) -> K,
) -> Vec<(L, Option<R>)> {
    left.into_iter()
        .map(|left_item| {
            let key = left_key(&left_item);
            let right_item = right
                .iter()
                .position(|right_item| right_key(right_item) == key)
                .map(|position| right.remove(position));
            (left_item, right_item)
        })
        .collect()
}

fn same_npu_device(previous: Option<&NpuInfo>, refreshed: Option<&NpuInfo>) -> bool {
    match (previous, refreshed) {
        (Some(previous), Some(refreshed)) => {
            previous.bdf == refreshed.bdf && previous.name == refreshed.name
        }
        (None, None) => true,
        _ => false,
    }
}

fn activate_if_awake<T>(
    app: &mut Option<T>,
    is_awake: bool,
    initialize: impl FnOnce() -> Option<T>,
) -> bool {
    if app.is_some() || !is_awake {
        return false;
    }

    *app = initialize();
    app.is_some()
}

fn detect_npu(apps: &[AppAmdgpuTop]) -> Option<NpuInfo> {
    if let Some(info) = apps.iter().find_map(|app| {
        app.xdna_device_path.as_ref().map(|device_path| {
            NpuInfo::from_device_path(device_path, app.xdna_fw_version.clone(), true)
        })
    }) {
        return Some(info);
    }

    detect_npu_from_sysfs()
}

impl NpuInfo {
    fn from_device_path(
        device_path: &DevicePath,
        fw_version: Option<String>,
        fdinfo_supported: bool,
    ) -> Self {
        let name = if device_path.device_name.trim().is_empty() {
            read_trim(device_path.sysfs_path.join("vbnv")).unwrap_or_else(|| "XDNA NPU".to_string())
        } else {
            device_path.device_name.clone()
        };
        let fw_version = fw_version.or_else(|| device_path.get_xdna_fw_version().ok());

        Self {
            name,
            bdf: device_path.pci.to_string(),
            fw_version,
            fdinfo_supported,
        }
    }
}

fn detect_npu_from_sysfs() -> Option<NpuInfo> {
    let entries = fs::read_dir("/sys/class/accel").ok()?;

    for entry in entries.flatten() {
        let accel_name = entry.file_name();
        let accel_path = PathBuf::from("/dev/accel").join(&accel_name);
        let Ok(sysfs_path) = fs::canonicalize(entry.path().join("device")) else {
            continue;
        };

        // AMD XDNA accel devices expose these attributes via amdxdna. This is a
        // presence check only; fdinfo telemetry is optional and may be missing on
        // newer kernels even when the NPU itself is usable.
        if !(sysfs_path.join("device_type").exists() && sysfs_path.join("vbnv").exists()) {
            continue;
        }

        let name = read_trim(sysfs_path.join("vbnv")).unwrap_or_else(|| "XDNA NPU".to_string());
        let bdf = sysfs_path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("unknown")
            .to_string();
        let fw_version = read_trim(sysfs_path.join("fw_version"));
        let fdinfo_supported = fdinfo_has_drm_driver(&accel_path);

        return Some(NpuInfo {
            name,
            bdf,
            fw_version,
            fdinfo_supported,
        });
    }

    None
}

fn read_trim(path: impl AsRef<Path>) -> Option<String> {
    fs::read_to_string(path)
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

fn fdinfo_has_drm_driver(accel_path: &Path) -> bool {
    let Ok(file) = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(accel_path)
    else {
        return false;
    };
    let fd = file.as_raw_fd();

    fs::read_to_string(format!("/proc/self/fdinfo/{fd}"))
        .is_ok_and(|s| s.lines().any(|line| line.starts_with("drm-driver")))
}

/// APU-aware memory usage. APUs use the GTT (system RAM) pool; discrete GPUs
/// use VRAM.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct MemInfo {
    pub percent: f64,
    pub used_bytes: u64,
    pub total_bytes: u64,
}

/// GPU memory the kernel reports for a device minus what per-process fdinfo
/// accounts for. A large remainder means a holder is not readable at this
/// privilege level: root processes (including containers) do not expose
/// `/proc/<pid>/fdinfo`, so their GTT/VRAM cannot be attributed.
pub fn unattributed_gpu_bytes(system_used_bytes: u64, attributed_kib: u64) -> u64 {
    system_used_bytes.saturating_sub(attributed_kib.saturating_mul(1024))
}

pub fn gpu_mem_info(app: &AppAmdgpuTop) -> MemInfo {
    let usage = &app.stat.vram_usage.0;
    memory_info(
        app.device_info.is_apu,
        (usage.vram.heap_usage, usage.vram.usable_heap_size),
        (usage.gtt.heap_usage, usage.gtt.usable_heap_size),
    )
}

fn memory_info(is_apu: bool, vram: (u64, u64), gtt: (u64, u64)) -> MemInfo {
    let (used_bytes, total_bytes) = if is_apu { gtt } else { vram };
    let percent = if total_bytes == 0 {
        0.0
    } else {
        (used_bytes as f64 / total_bytes as f64 * 100.0).clamp(0.0, 100.0)
    };
    MemInfo {
        percent,
        used_bytes,
        total_bytes,
    }
}

#[cfg(test)]
mod tests {
    use std::sync::mpsc::{self, TryRecvError};

    use super::{
        NpuInfo, activate_if_awake, memory_info, pair_by_key, same_npu_device,
        send_process_targets_after_activation,
    };

    #[test]
    fn device_merge_retains_sleeping_gpus_in_discovery_order() {
        let devices = vec![1, 2, 3];
        let active = vec![(3, "gpu3"), (1, "gpu1")];

        let merged = pair_by_key(devices, active, |device| *device, |app| app.0);

        assert_eq!(
            merged,
            vec![(1, Some((1, "gpu1"))), (2, None), (3, Some((3, "gpu3"))),]
        );
    }

    #[test]
    fn device_refresh_preserves_state_only_for_the_same_hardware() {
        let discovered = vec![(1, 10, 1), (2, 20, 1), (3, 30, 1), (4, 40, 1)];
        let previous = vec![
            (1, 10, 1, "gpu1-history"),
            (2, 99, 1, "different-device"),
            (3, 30, 2, "different-revision"),
            (5, 50, 1, "removed"),
        ];

        let paired = pair_by_key(
            discovered,
            previous,
            |device| (device.0, device.1, device.2),
            |device| (device.0, device.1, device.2),
        );

        assert_eq!(
            paired,
            vec![
                ((1, 10, 1), Some((1, 10, 1, "gpu1-history"))),
                ((2, 20, 1), None),
                ((3, 30, 1), None),
                ((4, 40, 1), None),
            ]
        );
    }

    #[test]
    fn npu_refresh_preserves_state_only_for_the_same_hardware() {
        let previous = NpuInfo {
            name: "Ryzen AI".into(),
            bdf: "0000:c4:00.1".into(),
            fw_version: Some("1".into()),
            fdinfo_supported: false,
        };
        let same_npu_new_firmware = NpuInfo {
            fw_version: Some("2".into()),
            fdinfo_supported: true,
            ..previous.clone()
        };
        let replacement = NpuInfo {
            name: "Different NPU".into(),
            ..previous.clone()
        };

        assert!(same_npu_device(
            Some(&previous),
            Some(&same_npu_new_firmware)
        ));
        assert!(!same_npu_device(Some(&previous), Some(&replacement)));
        assert!(!same_npu_device(Some(&previous), None));
        assert!(same_npu_device(None, None));
    }

    #[test]
    fn sleeping_gpu_is_initialized_only_after_it_wakes() {
        let mut app = None;
        let mut initialization_count = 0;

        assert!(!activate_if_awake(&mut app, false, || {
            initialization_count += 1;
            Some("active")
        }));
        assert_eq!(app, None);
        assert_eq!(initialization_count, 0);

        assert!(activate_if_awake(&mut app, true, || {
            initialization_count += 1;
            Some("active")
        }));
        assert_eq!(app, Some("active"));
        assert_eq!(initialization_count, 1);

        assert!(!activate_if_awake(&mut app, true, || {
            initialization_count += 1;
            Some("replacement")
        }));
        assert_eq!(app, Some("active"));
        assert_eq!(initialization_count, 1);
    }

    #[test]
    fn awake_gpu_initialization_can_be_retried_after_a_failure() {
        let mut app = None;

        assert!(!activate_if_awake(&mut app, true, || None::<&str>));
        assert_eq!(app, None);
        assert!(activate_if_awake(&mut app, true, || Some("active")));
        assert_eq!(app, Some("active"));
    }

    #[test]
    fn device_activation_replaces_process_index_targets() {
        let (sender, receiver) = mpsc::channel();
        let mut collected_targets = false;

        send_process_targets_after_activation(&sender, false, || {
            collected_targets = true;
            vec![1, 2]
        });
        assert!(!collected_targets);
        assert_eq!(receiver.try_recv(), Err(TryRecvError::Empty));

        send_process_targets_after_activation(&sender, true, || {
            collected_targets = true;
            vec![1, 2]
        });
        assert!(collected_targets);
        assert_eq!(receiver.recv().unwrap(), vec![1, 2]);
    }

    #[test]
    fn memory_info_selects_gtt_for_apus_and_vram_for_discrete_gpus() {
        let apu = memory_info(true, (10, 100), (20, 200));
        let discrete = memory_info(false, (10, 100), (20, 200));

        assert_eq!(
            (apu.used_bytes, apu.total_bytes, apu.percent),
            (20, 200, 10.0)
        );
        assert_eq!(
            (discrete.used_bytes, discrete.total_bytes, discrete.percent),
            (10, 100, 10.0)
        );
    }

    #[test]
    fn memory_info_handles_zero_and_overcommitted_pools() {
        assert_eq!(memory_info(false, (10, 0), (0, 0)).percent, 0.0);
        assert_eq!(memory_info(false, (200, 100), (0, 0)).percent, 100.0);
    }
    #[test]
    fn gpu_unattributed_is_the_gap_between_kernel_and_fdinfo() {
        // nothing attributed: the whole device usage is a gap
        assert_eq!(super::unattributed_gpu_bytes(10 << 30, 0), 10 << 30);
        // 4 GiB reported by fdinfo (in KiB) out of 10 GiB used
        assert_eq!(super::unattributed_gpu_bytes(10 << 30, 4 << 20), 6 << 30);
        // fdinfo may slightly exceed the device counter (shared accounting)
        assert_eq!(super::unattributed_gpu_bytes(1 << 30, 2 << 20), 0);
    }
}
