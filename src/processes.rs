//! Per-process memory model for the PROCESSES panel.
//!
//! Upstream amdtop only lists DRM clients, i.e. processes that hold GPU
//! memory. That hides processes which never touch the GPU but still pin
//! unified memory through RSS or swap (containers, LSP servers, …). This
//! module merges `/proc/<pid>/status` with the fdinfo numbers so the panel
//! can rank *every* process by its real footprint:
//!
//! `TOTAL = MEM + SWAP + VRAM + GTT`
//!
//! `MEM` is RSS and `SWAP` is `VmSwap`, so a process whose pages were
//! compressed into zram is still visible instead of silently dropping to a
//! few megabytes.

use std::cmp::Ordering;
use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::Path;
use std::time::Duration;

/// Sortable column, in the left-to-right order the cursor walks them.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum SortKey {
    /// MEM + SWAP + VRAM + GTT, the "how much unified memory does this pin" view.
    #[default]
    Total,
    Mem,
    Swap,
    Vram,
    Gtt,
    Gfx,
    Compute,
    Dma,
    Cpu,
}

impl SortKey {
    pub const ALL: [SortKey; 9] = [
        SortKey::Total,
        SortKey::Mem,
        SortKey::Swap,
        SortKey::Vram,
        SortKey::Gtt,
        SortKey::Gfx,
        SortKey::Compute,
        SortKey::Dma,
        SortKey::Cpu,
    ];

    fn index(self) -> usize {
        Self::ALL.iter().position(|key| *key == self).unwrap_or(0)
    }

    /// Column to the right, wrapping past the last one.
    pub fn next_column(self) -> Self {
        Self::ALL[(self.index() + 1) % Self::ALL.len()]
    }

    /// Column to the left, wrapping past the first one.
    pub fn prev_column(self) -> Self {
        Self::ALL[(self.index() + Self::ALL.len() - 1) % Self::ALL.len()]
    }

    /// Short header label (without the direction arrow).
    pub fn label(self) -> &'static str {
        match self {
            SortKey::Total => "TOTAL",
            SortKey::Mem => "MEM",
            SortKey::Swap => "SWAP",
            SortKey::Vram => "VRAM",
            SortKey::Gtt => "GTT",
            SortKey::Gfx => "GFX%",
            SortKey::Compute => "COMP%",
            SortKey::Dma => "DMA%",
            SortKey::Cpu => "CPU%",
        }
    }

    /// Stable identifier used in `state.json`.
    pub fn id(self) -> &'static str {
        match self {
            SortKey::Total => "total",
            SortKey::Mem => "mem",
            SortKey::Swap => "swap",
            SortKey::Vram => "vram",
            SortKey::Gtt => "gtt",
            SortKey::Gfx => "gfx",
            SortKey::Compute => "compute",
            SortKey::Dma => "dma",
            SortKey::Cpu => "cpu",
        }
    }

    pub fn from_id(id: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|key| key.id() == id)
    }
}

/// GPU side of a process, lifted from `libamdgpu_top`'s fdinfo snapshot.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct GpuProc {
    pub gpu_index: usize,
    pub vram_kb: u64,
    pub gtt_kb: u64,
    pub gfx: i64,
    pub compute: i64,
    pub dma: i64,
}

impl GpuProc {
    pub(crate) fn absorb(&mut self, other: &GpuProc) {
        self.vram_kb = self.vram_kb.saturating_add(other.vram_kb);
        self.gtt_kb = self.gtt_kb.saturating_add(other.gtt_kb);
        self.gfx = self.gfx.max(other.gfx);
        self.compute = self.compute.max(other.compute);
        self.dma = self.dma.max(other.dma);
    }
}

/// `/proc/<pid>/stat` CPU counters, the raw material for a per-interval share.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CpuTicks {
    /// `utime + stime`, i.e. the CPU time of every thread in clock ticks.
    pub ticks: u64,
    /// Field 22 of `stat`; a reused pid starts a new process lifetime.
    pub start_ticks: u64,
}

/// Turns the absolute CPU counters in `/proc/<pid>/stat` into "how much CPU did
/// this process use during the last sampling interval".
///
/// Upstream amdtop only knows the CPU time that `libamdgpu_top` derives while it
/// walks the DRM fdinfo of GPU clients, so every other process had no CPU number
/// at all. Reading the counters for every pid keeps the column honest and
/// independent of holding a GPU context.
#[derive(Debug, Default)]
pub struct ProcessCpuSampler {
    previous: HashMap<i32, CpuTicks>,
}

impl ProcessCpuSampler {
    /// Compare this round's counters with the previous round.
    ///
    /// A process without a usable baseline is reported as absent instead of `0`:
    /// a first sample cannot tell "idle" from "not measured yet", and printing
    /// `0` for both is the misleading behaviour this replaces. The same applies
    /// to a pid that was reused or that disappeared for a round, so a sampling
    /// gap never turns into a bogus spike.
    pub fn sample(
        &mut self,
        current: HashMap<i32, CpuTicks>,
        interval: Duration,
        hz: u64,
    ) -> HashMap<i32, f32> {
        let mut usage = HashMap::with_capacity(current.len());

        // `interval` is the elapsed wall time between the two reads; a zero one
        // would divide the share into infinity.
        if !interval.is_zero() && hz > 0 {
            for (&pid, ticks) in &current {
                let Some(previous) = self.previous.get(&pid) else {
                    continue;
                };
                if previous.start_ticks != ticks.start_ticks {
                    continue;
                }
                let delta = ticks.ticks.saturating_sub(previous.ticks);
                let seconds = delta as f32 / hz as f32;
                usage.insert(pid, seconds * 100.0 / interval.as_secs_f32());
            }
        }

        self.previous = current;
        usage
    }
}

/// One row of the PROCESSES table.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ProcRow {
    pub pid: i32,
    /// Real UID of the process; used to refuse cross-user signals.
    pub uid: u32,
    pub name: String,
    /// Resident set size in KiB (`VmRSS`).
    pub mem_kb: u64,
    /// Swapped-out pages in KiB (`VmSwap`); they live compressed in zram.
    pub swap_kb: u64,
    /// CPU used during the last sampling interval, in percent of one core.
    /// Sums every thread, so a busy process can exceed 100. `None` until the
    /// process has been sampled twice.
    pub cpu_percent: Option<f32>,
    /// `None` for processes that do not hold GPU memory.
    pub gpu: Option<GpuProc>,
}

impl ProcRow {
    /// The number the user cares about: everything this process pins in
    /// unified memory, whether it is resident, compressed or GPU-mapped.
    pub fn total_kb(&self) -> u64 {
        let gpu = self.gpu.as_ref();
        self.mem_kb
            .saturating_add(self.swap_kb)
            .saturating_add(gpu.map_or(0, |g| g.vram_kb))
            .saturating_add(gpu.map_or(0, |g| g.gtt_kb))
    }

    pub fn has_gpu(&self) -> bool {
        self.gpu.is_some()
    }

    pub fn gpu_index(&self) -> Option<usize> {
        self.gpu.as_ref().map(|g| g.gpu_index)
    }

    pub fn vram_kb(&self) -> Option<u64> {
        self.gpu.as_ref().map(|g| g.vram_kb)
    }

    pub fn gtt_kb(&self) -> Option<u64> {
        self.gpu.as_ref().map(|g| g.gtt_kb)
    }

    pub fn gfx(&self) -> i64 {
        self.gpu.as_ref().map_or(0, |g| g.gfx)
    }

    pub fn compute(&self) -> i64 {
        self.gpu.as_ref().map_or(0, |g| g.compute)
    }

    pub fn dma(&self) -> i64 {
        self.gpu.as_ref().map_or(0, |g| g.dma)
    }

    pub fn cpu(&self) -> Option<f32> {
        self.cpu_percent
    }

    /// Highest GPU engine utilization in percent, used for row tinting.
    pub fn gpu_activity(&self) -> i64 {
        self.gfx().max(self.compute()).max(self.dma()).clamp(0, 100)
    }

    fn numeric_sort_value(&self, key: SortKey) -> u64 {
        match key {
            SortKey::Total => self.total_kb(),
            SortKey::Mem => self.mem_kb,
            SortKey::Swap => self.swap_kb,
            SortKey::Vram => self.vram_kb().unwrap_or(0),
            SortKey::Gtt => self.gtt_kb().unwrap_or(0),
            SortKey::Gfx => self.gfx().max(0) as u64,
            SortKey::Compute => self.compute().max(0) as u64,
            SortKey::Dma => self.dma().max(0) as u64,
            // `sort_rows` orders CPU by `Option` so unsampled rows sink; this
            // arm only exists to keep the match exhaustive.
            SortKey::Cpu => 0,
        }
    }
}

/// The parts of `/proc/<pid>/status` this panel needs.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ProcStatus {
    pub name: String,
    pub uid: u32,
    pub rss_kb: u64,
    pub swap_kb: u64,
    /// False for kernel threads and zombies: they have no `mm`, so `status`
    /// carries no `Vm*` block and they can never hold user memory.
    pub has_mm: bool,
}

fn parse_kb(value: &str) -> Option<u64> {
    let mut fields = value.split_whitespace();
    let amount = fields.next()?.parse::<u64>().ok()?;
    match fields.next() {
        None | Some("kB") => Some(amount),
        _ => None,
    }
}

/// Parse the interesting fields of a `/proc/<pid>/status` file.
pub fn parse_status(status: &str) -> ProcStatus {
    let mut parsed = ProcStatus::default();
    for line in status.lines() {
        let Some((key, value)) = line.split_once(':') else {
            continue;
        };
        let value = value.trim();
        match key {
            "Name" => parsed.name = value.to_string(),
            "Uid" => {
                parsed.uid = value
                    .split_whitespace()
                    .next()
                    .and_then(|v| v.parse().ok())
                    .unwrap_or(0)
            }
            "VmRSS" => {
                parsed.has_mm = true;
                parsed.rss_kb = parse_kb(value).unwrap_or(0);
            }
            "VmSwap" => parsed.swap_kb = parse_kb(value).unwrap_or(0),
            _ => {}
        }
    }
    parsed
}

/// Merge procfs rows with the GPU snapshot.
///
/// Every live process is kept so memory hogs without a GPU context remain
/// visible; the `a` filter is a separate view on top of this (`filter_rows`).
pub fn merge_rows(
    statuses: Vec<(i32, ProcStatus)>,
    cpu: &HashMap<i32, f32>,
    gpu: &HashMap<i32, GpuProc>,
) -> Vec<ProcRow> {
    let mut rows = Vec::with_capacity(statuses.len());
    let mut seen = HashSet::with_capacity(statuses.len());

    for (pid, status) in statuses {
        seen.insert(pid);
        let gpu = gpu.get(&pid).cloned();
        // Kernel threads / zombies have no address space: they cannot own
        // memory, so listing them is pure noise in a memory panel.
        if !status.has_mm && gpu.is_none() {
            continue;
        }
        rows.push(ProcRow {
            pid,
            uid: status.uid,
            name: status.name,
            mem_kb: status.rss_kb,
            swap_kb: status.swap_kb,
            cpu_percent: cpu.get(&pid).copied(),
            gpu,
        });
    }

    // A GPU client can disappear from /proc between the two snapshots; keep it
    // visible with its GPU numbers rather than dropping the row.
    for (&pid, gpu_proc) in gpu {
        if seen.insert(pid) {
            rows.push(ProcRow {
                pid,
                name: format!("pid {pid}"),
                gpu: Some(gpu_proc.clone()),
                ..ProcRow::default()
            });
        }
    }

    rows
}

/// The `a` key: keep every process, or DRM clients only.
pub fn filter_rows(rows: &[ProcRow], gpu_only: bool) -> Vec<ProcRow> {
    if gpu_only {
        rows.iter().filter(|row| row.has_gpu()).cloned().collect()
    } else {
        rows.to_vec()
    }
}

/// Sort rows in place. Largest value first; ties fall back to ascending PID so
/// the list does not jitter when several processes report the same value.
pub fn sort_rows(rows: &mut [ProcRow], key: SortKey) {
    if key == SortKey::Cpu {
        // CPU is the one column with a "not measured yet" state, which has to
        // sink below a real 0% instead of sorting into the middle of the list.
        rows.sort_by(|a, b| {
            let order = match (a.cpu_percent, b.cpu_percent) {
                (Some(a), Some(b)) => b.total_cmp(&a),
                (Some(_), None) => Ordering::Less,
                (None, Some(_)) => Ordering::Greater,
                (None, None) => Ordering::Equal,
            };
            order.then_with(|| a.pid.cmp(&b.pid))
        });
        return;
    }

    rows.sort_by(|a, b| {
        b.numeric_sort_value(key)
            .cmp(&a.numeric_sort_value(key))
            .then_with(|| a.pid.cmp(&b.pid))
    });
}

/// The unsorted view: ascending PID, like `ps`.
pub fn sort_by_pid(rows: &mut [ProcRow]) {
    rows.sort_by_key(|row| row.pid);
}

/// Everything the detail pane shows about one process.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ProcDetail {
    pub pid: i32,
    pub name: String,
    pub state: char,
    pub uid: u32,
    pub ppid: i32,
    pub parent: String,
    pub threads: u32,
    pub exe: Option<String>,
    pub cwd: Option<String>,
    pub cmd: String,
    pub vm_size_kb: u64,
    pub rss_kb: u64,
    pub hwm_kb: u64,
    pub swap_kb: u64,
    pub pss_kb: Option<u64>,
    /// systemd unit or container scope, plus the command that manages it.
    pub unit: String,
    pub unit_hint: Option<String>,
    pub io: Option<(u64, u64)>,
    pub fds: Option<usize>,
    /// Seconds since the process started.
    pub age_secs: Option<u64>,
}

/// Parsed `/proc/<pid>/stat` fields the detail pane needs.
pub fn parse_stat_fields(stat: &str) -> Option<(char, i32, u32, u64)> {
    let fields = stat_fields(stat)?;
    let state = fields.get(STAT_STATE)?.chars().next()?;
    let ppid = fields.get(STAT_PPID)?.parse().ok()?;
    let threads = fields.get(STAT_THREADS)?.parse().ok()?;
    let start_ticks = fields.get(STAT_STARTTIME)?.parse().ok()?;
    Some((state, ppid, threads, start_ticks))
}

/// `/proc/<pid>/stat` field indices, counted from the field after `comm`, so
/// index `n` is field `n + 3` of the line.
const STAT_STATE: usize = 0;
const STAT_PPID: usize = 1;
const STAT_UTIME: usize = 11;
const STAT_STIME: usize = 12;
const STAT_THREADS: usize = 17;
const STAT_STARTTIME: usize = 19;

/// `comm` (field 2) may contain spaces and parentheses, so the split starts
/// after the last `)`.
fn stat_fields(stat: &str) -> Option<Vec<&str>> {
    let close = stat.rfind(')')?;
    Some(stat[close + 1..].split_whitespace().collect())
}

/// Join a NUL-separated `/proc/<pid>/cmdline` into one readable line.
pub fn parse_cmdline(bytes: &[u8]) -> String {
    let mut command = String::new();
    for part in bytes.split(|byte| *byte == 0) {
        if part.is_empty() {
            continue;
        }
        if !command.is_empty() {
            command.push(' ');
        }
        command.push_str(&String::from_utf8_lossy(part));
    }
    command
}

/// `read_bytes` / `write_bytes` from `/proc/<pid>/io`.
pub fn parse_io(text: &str) -> Option<(u64, u64)> {
    let mut read = None;
    let mut write = None;
    for line in text.lines() {
        if let Some(value) = line.strip_prefix("read_bytes:") {
            read = value.trim().parse().ok();
        } else if let Some(value) = line.strip_prefix("write_bytes:") {
            write = value.trim().parse().ok();
        }
    }
    Some((read?, write?))
}

/// systemd unit (or container scope) responsible for a cgroup path.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct UnitInfo {
    pub unit: String,
    /// Command a human would use to manage it, e.g. `systemctl --user stop foo`.
    pub hint: Option<String>,
}

/// `0::/user.slice/.../app-foo.scope` -> the deepest `.service` / `.scope`
/// plus a hint for the right stop command.
pub fn parse_cgroup(text: &str) -> UnitInfo {
    let path = text
        .lines()
        .next()
        .map(|line| line.rsplit("::").next().unwrap_or(line))
        .unwrap_or_default();
    let unit = path
        .split('/')
        .rev()
        .find(|part| part.ends_with(".service") || part.ends_with(".scope"))
        .unwrap_or_default()
        .to_string();

    if unit.is_empty() {
        return UnitInfo::default();
    }

    let hint = if let Some(id) = unit
        .strip_prefix("docker-")
        .or_else(|| unit.strip_prefix("libpod-"))
    {
        let id = id.split('.').next().unwrap_or(id);
        Some(format!("docker stop {}", &id[..id.len().min(12)]))
    } else if unit.ends_with(".service") {
        let name = &unit[..unit.len() - ".service".len()];
        if path.contains("user@") || path.contains("/user.slice/") {
            Some(format!("systemctl --user stop {name}"))
        } else {
            Some(format!("systemctl stop {name}"))
        }
    } else {
        None
    };

    UnitInfo { unit, hint }
}

impl UnitInfo {
    /// Units whose supervisor is known to restart processes on its own.
    pub fn may_restart(&self) -> bool {
        self.unit.ends_with(".service")
            || self.unit.starts_with("docker-")
            || self.unit.starts_with("libpod-")
    }
}

/// Short display form: container scopes are 76 characters of hex, which is
/// useless in a table, so show `docker:<id>` instead.
pub fn shorten_unit(unit: &str) -> String {
    for prefix in ["docker-", "libpod-"] {
        if let Some(id) = unit.strip_prefix(prefix) {
            return format!("docker:{}", &id[..id.len().min(12)]);
        }
    }
    unit.to_string()
}

/// Human-readable process age: `45s`, `12m`, `3h07m`, `2d4h`.
pub fn format_age(secs: u64) -> String {
    let days = secs / 86_400;
    let hours = (secs % 86_400) / 3600;
    let minutes = (secs % 3600) / 60;
    if days > 0 {
        format!("{days}d{hours}h")
    } else if hours > 0 {
        format!("{hours}h{minutes:02}m")
    } else if minutes > 0 {
        format!("{minutes}m")
    } else {
        format!("{}s", secs % 60)
    }
}

/// The subset of `/proc/<pid>/status` used by the detail pane.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DetailStatus {
    pub name: String,
    pub uid: u32,
    pub vm_size_kb: u64,
    pub rss_kb: u64,
    pub hwm_kb: u64,
    pub swap_kb: u64,
}

pub fn parse_detail_status(text: &str) -> DetailStatus {
    let mut parsed = DetailStatus::default();
    for line in text.lines() {
        let Some((key, value)) = line.split_once(':') else {
            continue;
        };
        let value = value.trim();
        match key {
            "Name" => parsed.name = value.to_string(),
            "Uid" => {
                parsed.uid = value
                    .split_whitespace()
                    .next()
                    .and_then(|uid| uid.parse().ok())
                    .unwrap_or(0);
            }
            "VmSize" => parsed.vm_size_kb = parse_kb(value).unwrap_or(0),
            "VmRSS" => parsed.rss_kb = parse_kb(value).unwrap_or(0),
            "VmHWM" => parsed.hwm_kb = parse_kb(value).unwrap_or(0),
            "VmSwap" => parsed.swap_kb = parse_kb(value).unwrap_or(0),
            _ => {}
        }
    }
    parsed
}

fn parse_pss(text: &str) -> Option<u64> {
    text.lines().find_map(|line| {
        let value = line.strip_prefix("Pss:")?;
        parse_kb(value.trim())
    })
}

/// Unit information for one live process.
pub fn cgroup_info(pid: i32) -> UnitInfo {
    fs::read_to_string(format!("/proc/{pid}/cgroup"))
        .map(|text| parse_cgroup(&text))
        .unwrap_or_default()
}

/// Collect the detail pane for one process.
pub fn collect_detail(pid: i32) -> Option<ProcDetail> {
    collect_detail_at(Path::new("/proc"), pid, now_secs())
}

pub fn collect_detail_at(root: &Path, pid: i32, now: u64) -> Option<ProcDetail> {
    let dir = root.join(pid.to_string());
    let status = fs::read_to_string(dir.join("status")).ok()?;
    let status = parse_detail_status(&status);
    let stat = fs::read_to_string(dir.join("stat"))
        .ok()
        .and_then(|text| parse_stat_fields(&text));
    let (state, ppid, threads, start_ticks) = stat.unwrap_or(('?', 0, 0, 0));

    let age_secs = if start_ticks > 0 {
        let hz = clock_ticks_per_second();
        boot_time(root)
            .map(|boot| boot + start_ticks / hz)
            .and_then(|started| now.checked_sub(started))
    } else {
        None
    };

    let parent = if ppid > 0 {
        fs::read_to_string(root.join(ppid.to_string()).join("comm"))
            .map(|name| name.trim().to_string())
            .unwrap_or_default()
    } else {
        String::new()
    };

    let cmd = fs::read(dir.join("cmdline"))
        .map(|bytes| parse_cmdline(&bytes))
        .unwrap_or_default();
    let cmd = if cmd.is_empty() {
        format!("[{}]", status.name)
    } else {
        cmd
    };

    let unit = fs::read_to_string(dir.join("cgroup"))
        .map(|text| parse_cgroup(&text))
        .unwrap_or_default();

    Some(ProcDetail {
        pid,
        name: status.name,
        state,
        uid: status.uid,
        ppid,
        parent,
        threads,
        exe: fs::read_link(dir.join("exe"))
            .ok()
            .map(|p| p.display().to_string()),
        cwd: fs::read_link(dir.join("cwd"))
            .ok()
            .map(|p| p.display().to_string()),
        cmd,
        vm_size_kb: status.vm_size_kb,
        rss_kb: status.rss_kb,
        hwm_kb: status.hwm_kb,
        swap_kb: status.swap_kb,
        pss_kb: fs::read_to_string(dir.join("smaps_rollup"))
            .ok()
            .and_then(|text| parse_pss(&text)),
        unit: unit.unit,
        unit_hint: unit.hint,
        io: fs::read_to_string(dir.join("io"))
            .ok()
            .and_then(|text| parse_io(&text)),
        fds: fs::read_dir(dir.join("fd"))
            .ok()
            .map(|entries| entries.count()),
        age_secs,
    })
}

fn clock_ticks_per_second() -> u64 {
    // SAFETY: sysconf has no preconditions.
    let hz = unsafe { libc::sysconf(libc::_SC_CLK_TCK) };
    if hz > 0 { hz as u64 } else { 100 }
}

fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn boot_time(root: &Path) -> Option<u64> {
    let text = fs::read_to_string(root.join("stat")).ok()?;
    text.lines().find_map(|line| {
        let value = line.strip_prefix("btime ")?;
        value.trim().parse().ok()
    })
}

/// `utime + stime` and `starttime` from `/proc/<pid>/stat`.
pub fn parse_cpu_ticks(stat: &str) -> Option<CpuTicks> {
    let fields = stat_fields(stat)?;
    let utime = fields.get(STAT_UTIME)?.parse::<u64>().ok()?;
    let stime = fields.get(STAT_STIME)?.parse::<u64>().ok()?;
    let start_ticks = fields.get(STAT_STARTTIME)?.parse::<u64>().ok()?;

    Some(CpuTicks {
        ticks: utime.saturating_add(stime),
        start_ticks,
    })
}

/// Read `/proc` and return the model used by the panel.
pub fn collect(
    cpu: &mut ProcessCpuSampler,
    gpu: &HashMap<i32, GpuProc>,
    interval: Duration,
) -> Vec<ProcRow> {
    collect_from(Path::new("/proc"), cpu, gpu, interval)
}

fn collect_from(
    root: &Path,
    cpu: &mut ProcessCpuSampler,
    gpu: &HashMap<i32, GpuProc>,
    interval: Duration,
) -> Vec<ProcRow> {
    let mut statuses = Vec::new();
    let mut counters = HashMap::new();

    if let Ok(entries) = fs::read_dir(root) {
        for entry in entries.flatten() {
            let Ok(file_name) = entry.file_name().into_string() else {
                continue;
            };
            let Ok(pid) = file_name.parse::<i32>() else {
                continue;
            };
            let Ok(status) = fs::read_to_string(entry.path().join("status")) else {
                continue;
            };
            let status = parse_status(&status);
            // Kernel threads can never become a row, so their counters are not
            // worth a second read; a process that exits between the two reads
            // simply has no counters this round.
            if status.has_mm
                && let Some(ticks) = fs::read_to_string(entry.path().join("stat"))
                    .ok()
                    .and_then(|text| parse_cpu_ticks(&text))
            {
                counters.insert(pid, ticks);
            }
            statuses.push((pid, status));
        }
    }

    let usage = cpu.sample(counters, interval, clock_ticks_per_second());
    merge_rows(statuses, &usage, gpu)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Nominal sampling interval used by the panel.
    const TICK: Duration = Duration::from_secs(1);
    /// USER_HZ: `_SC_CLK_TCK` is 100 on every Linux userspace ABI.
    const HZ: u64 = 100;

    fn gpu_proc(vram_kb: u64, gtt_kb: u64) -> GpuProc {
        GpuProc {
            gpu_index: 0,
            vram_kb,
            gtt_kb,
            ..GpuProc::default()
        }
    }

    fn ticks(ticks: u64, start_ticks: u64) -> CpuTicks {
        CpuTicks { ticks, start_ticks }
    }

    /// A `/proc/<pid>/stat` line with the fields this module reads.
    fn stat_line(pid: i32, utime: u64, stime: u64, start_ticks: u64) -> String {
        format!(
            "{pid} (proc) S 1 {pid} {pid} 0 -1 4194304 100 0 0 0 \
             {utime} {stime} 0 0 20 0 7 0 {start_ticks} 0"
        )
    }

    fn status(name: &str, rss_kb: u64, swap_kb: u64) -> ProcStatus {
        ProcStatus {
            name: name.into(),
            uid: 1000,
            rss_kb,
            swap_kb,
            has_mm: true,
        }
    }

    #[test]
    fn parse_status_reads_name_rss_and_swap() {
        let text = "\
Name:\tllama-server
Umask:\t0022
VmRSS:\t 168244 kB
VmSwap:\t  872952 kB
Threads:\t37
";

        let parsed = parse_status(text);

        assert_eq!(parsed.name, "llama-server");
        assert_eq!(parsed.rss_kb, 168_244);
        assert_eq!(parsed.swap_kb, 872_952);
    }

    #[test]
    fn parse_status_ignores_non_kib_units_and_missing_fields() {
        let parsed = parse_status("Name:\tx\nVmRSS:\t1536 MB\n");

        assert_eq!(parsed.rss_kb, 0);
        assert_eq!(parsed.swap_kb, 0);
        assert_eq!(parse_status("VmRSS:\tinvalid kB\n").rss_kb, 0);
    }

    #[test]
    fn total_kb_adds_resident_swapped_and_gpu_memory() {
        let row = ProcRow {
            pid: 1,
            uid: 1000,
            name: "x".into(),
            mem_kb: 100,
            swap_kb: 200,
            cpu_percent: None,
            gpu: Some(gpu_proc(300, 400)),
        };

        assert_eq!(row.total_kb(), 1000);
    }

    #[test]
    fn total_kb_is_just_mem_plus_swap_without_a_gpu_context() {
        let row = ProcRow {
            pid: 1,
            uid: 1000,
            name: "hindsight-api".into(),
            mem_kb: 32_000,
            swap_kb: 1_400_000,
            cpu_percent: None,
            gpu: None,
        };

        assert_eq!(row.total_kb(), 1_432_000);
        assert!(!row.has_gpu());
        assert_eq!(row.vram_kb(), None);
        assert_eq!(row.gtt_kb(), None);
    }

    #[test]
    fn merge_rows_keeps_processes_without_a_gpu_context() {
        let statuses = vec![
            (1, status("llama-server", 100, 0)),
            (2, status("python", 50, 500)),
        ];
        let gpu = HashMap::from([(1, gpu_proc(10, 20))]);

        let rows = merge_rows(statuses, &HashMap::new(), &gpu);

        assert_eq!(rows.len(), 2);
        assert_eq!(rows.iter().find(|r| r.pid == 2).unwrap().total_kb(), 550);
    }

    #[test]
    fn the_gpu_only_filter_drops_processes_without_a_gpu_context() {
        let statuses = vec![
            (1, status("llama-server", 100, 0)),
            (2, status("python", 50, 500)),
        ];
        let gpu = HashMap::from([(1, gpu_proc(10, 20))]);
        let rows = merge_rows(statuses, &HashMap::new(), &gpu);

        let filtered = filter_rows(&rows, true);

        assert_eq!(filtered.len(), 1);
        assert_eq!(filtered[0].pid, 1);
        assert_eq!(filter_rows(&rows, false), rows);
    }

    #[test]
    fn merge_rows_skips_kernel_threads_without_an_address_space() {
        let statuses = vec![
            (
                2,
                ProcStatus {
                    name: "kthreadd".into(),
                    ..ProcStatus::default()
                },
            ),
            (100, status("real", 10, 0)),
        ];

        let rows = merge_rows(statuses, &HashMap::new(), &HashMap::new());

        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].pid, 100);
    }

    #[test]
    fn parse_status_marks_processes_without_an_mm() {
        assert!(!parse_status("Name:\tkworker/R-sync_wq\n").has_mm);
        assert!(parse_status("Name:\tx\nVmRSS:\t0 kB\n").has_mm);
    }

    #[test]
    fn merge_rows_keeps_gpu_clients_whose_status_disappeared() {
        let gpu = HashMap::from([(42, gpu_proc(10, 20))]);

        let rows = merge_rows(Vec::new(), &HashMap::new(), &gpu);

        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].pid, 42);
        assert_eq!(rows[0].total_kb(), 30);
    }

    #[test]
    fn merge_rows_sums_every_gpu_the_process_touches() {
        let mut gpu = gpu_proc(10, 20);
        gpu.absorb(&gpu_proc(1, 2));
        let rows = merge_rows(
            vec![(1, status("x", 0, 0))],
            &HashMap::new(),
            &HashMap::from([(1, gpu)]),
        );

        assert_eq!(rows[0].vram_kb(), Some(11));
        assert_eq!(rows[0].gtt_kb(), Some(22));
    }

    #[test]
    fn sort_rows_descending_total_puts_the_biggest_first() {
        let mut rows = vec![
            ProcRow {
                pid: 1,
                name: "small".into(),
                mem_kb: 10,
                ..ProcRow::default()
            },
            ProcRow {
                pid: 2,
                name: "huge".into(),
                mem_kb: 9_000,
                ..ProcRow::default()
            },
            ProcRow {
                pid: 3,
                name: "swap".into(),
                swap_kb: 5_000,
                ..ProcRow::default()
            },
        ];

        sort_rows(&mut rows, SortKey::Total);

        assert_eq!(
            rows.iter().map(|r| r.pid).collect::<Vec<_>>(),
            vec![2, 3, 1]
        );
    }

    #[test]
    fn sort_rows_breaks_ties_by_pid_so_the_list_does_not_jitter() {
        let mut rows = vec![
            ProcRow {
                pid: 30,
                name: "b".into(),
                mem_kb: 100,
                ..ProcRow::default()
            },
            ProcRow {
                pid: 10,
                name: "a".into(),
                mem_kb: 100,
                ..ProcRow::default()
            },
            ProcRow {
                pid: 20,
                name: "c".into(),
                mem_kb: 200,
                ..ProcRow::default()
            },
        ];

        sort_rows(&mut rows, SortKey::Mem);

        assert_eq!(
            rows.iter().map(|r| r.pid).collect::<Vec<_>>(),
            vec![20, 10, 30]
        );
    }

    #[test]
    fn sort_by_pid_is_the_unsorted_view() {
        let mut rows = vec![
            ProcRow {
                pid: 300,
                name: "c".into(),
                mem_kb: 9_000,
                ..ProcRow::default()
            },
            ProcRow {
                pid: 100,
                name: "a".into(),
                ..ProcRow::default()
            },
            ProcRow {
                pid: 200,
                name: "b".into(),
                ..ProcRow::default()
            },
        ];

        sort_by_pid(&mut rows);

        assert_eq!(
            rows.iter().map(|r| r.pid).collect::<Vec<_>>(),
            vec![100, 200, 300]
        );
    }

    #[test]
    fn sort_rows_by_gpu_memory_ranks_processes_without_gpu_context_last() {
        let mut rows = vec![
            ProcRow {
                pid: 1,
                name: "cpu".into(),
                mem_kb: 9_000,
                ..ProcRow::default()
            },
            ProcRow {
                pid: 2,
                name: "gpu".into(),
                gpu: Some(gpu_proc(0, 5_000)),
                ..ProcRow::default()
            },
        ];

        sort_rows(&mut rows, SortKey::Gtt);

        assert_eq!(rows[0].pid, 2);
    }

    #[test]
    fn right_arrow_cycles_through_every_column_and_wraps_to_total() {
        let mut key = SortKey::Total;
        let mut seen = vec![key];
        for _ in 1..SortKey::ALL.len() {
            key = key.next_column();
            seen.push(key);
        }

        assert_eq!(seen, SortKey::ALL.to_vec());
        assert_eq!(SortKey::Cpu.next_column(), SortKey::Total);
    }

    #[test]
    fn left_arrow_cycles_backwards_and_wraps_to_the_last_column() {
        assert_eq!(SortKey::Total.prev_column(), SortKey::Cpu);
        assert_eq!(SortKey::Mem.prev_column(), SortKey::Total);
        assert_eq!(SortKey::Cpu.prev_column(), SortKey::Dma);
        // stepping left then right returns to where we started
        for key in SortKey::ALL {
            assert_eq!(key.prev_column().next_column(), key);
            assert_eq!(key.next_column().prev_column(), key);
        }
    }

    #[test]
    fn sort_key_ids_round_trip() {
        for key in SortKey::ALL {
            assert_eq!(SortKey::from_id(key.id()), Some(key));
        }
        assert_eq!(SortKey::from_id("nope"), None);
    }

    #[test]
    fn parse_stat_handles_a_comm_with_spaces_and_parentheses() {
        let stat =
            "123 (my (weird) name) S 1 123 123 0 -1 4194304 100 0 0 0 1 2 0 0 20 0 7 0 5000 0";

        let (state, ppid, threads, start) = parse_stat_fields(stat).expect("parse stat");

        assert_eq!(state, 'S');
        assert_eq!(ppid, 1);
        assert_eq!(threads, 7);
        assert_eq!(start, 5000);
    }

    #[test]
    fn cmdline_is_joined_into_one_line() {
        assert_eq!(
            parse_cmdline(b"python3\0-c\0print(1)\0"),
            "python3 -c print(1)"
        );
        assert_eq!(parse_cmdline(b""), "");
    }

    #[test]
    fn io_reads_byte_counters() {
        let text = "rchar: 10\nread_bytes: 2048\nwrite_bytes: 4096\n";
        assert_eq!(parse_io(text), Some((2048, 4096)));
    }

    #[test]
    fn cgroup_paths_map_to_a_unit_and_a_management_hint() {
        let user = parse_cgroup(
            "0::/user.slice/user-1000.slice/user@1000.service/app.slice/pi-web.service\n",
        );
        assert_eq!(user.unit, "pi-web.service");
        assert_eq!(user.hint.as_deref(), Some("systemctl --user stop pi-web"));

        let system = parse_cgroup("0::/system.slice/sshd.service\n");
        assert_eq!(system.unit, "sshd.service");
        assert_eq!(system.hint.as_deref(), Some("systemctl stop sshd"));

        let docker = parse_cgroup(
            "0::/system.slice/docker-1ecf1a2d9d964c679a818f7b4043394ce9eb82638939ad5ad82335f161f76cb.scope\n",
        );
        assert_eq!(
            docker.unit,
            "docker-1ecf1a2d9d964c679a818f7b4043394ce9eb82638939ad5ad82335f161f76cb.scope"
        );
        assert_eq!(docker.hint.as_deref(), Some("docker stop 1ecf1a2d9d96"));

        assert_eq!(parse_cgroup("0::/\n").unit, "");
        assert_eq!(parse_cgroup("").unit, "");
    }

    #[test]
    fn only_supervised_units_are_flagged_as_restartable() {
        assert!(parse_cgroup("0::/system.slice/sshd.service").may_restart());
        assert!(parse_cgroup("0::/system.slice/docker-1ecf1a2d9d96.scope").may_restart());
        assert!(!parse_cgroup("0::/user.slice/app-gnome-foo-123.scope").may_restart());
        assert!(!parse_cgroup("0::/init.scope").may_restart());
        assert!(!parse_cgroup("0::/").may_restart());
    }

    #[test]
    fn container_units_are_shortened_for_display() {
        assert_eq!(
            shorten_unit("docker-1ecf1a2d9d96aa.scope"),
            "docker:1ecf1a2d9d96"
        );
        assert_eq!(shorten_unit("lemond.service"), "lemond.service");
        assert_eq!(shorten_unit(""), "");
    }

    #[test]
    fn ages_are_rendered_compactly() {
        assert_eq!(format_age(45), "45s");
        assert_eq!(format_age(12 * 60), "12m");
        assert_eq!(format_age(3 * 3600 + 7 * 60), "3h07m");
        assert_eq!(format_age(2 * 86_400 + 4 * 3600), "2d4h");
    }

    #[test]
    fn detail_collects_command_line_unit_io_and_age() {
        use std::os::unix::fs::symlink;

        let root = std::env::temp_dir().join(format!("amdtop-detail-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let pid_dir = root.join("123");
        fs::create_dir_all(pid_dir.join("fd")).unwrap();
        fs::write(root.join("stat"), "cpu 0 0 0 0 0 0 0 0 0 0\nbtime 1000\n").unwrap();
        fs::create_dir_all(root.join("1")).unwrap();
        fs::write(root.join("1/comm"), "systemd\n").unwrap();
        fs::write(
            pid_dir.join("status"),
            "Name:\tpython3\nUid:\t1000\t1000\t1000\t1000\nVmSize:\t1200000 kB\nVmRSS:\t900000 kB\nVmHWM:\t905000 kB\nVmSwap:\t1234 kB\n",
        )
        .unwrap();
        fs::write(
            pid_dir.join("stat"),
            "123 (python3) S 1 123 123 0 -1 4194304 100 0 0 0 1 2 0 0 20 0 7 0 5000 0",
        )
        .unwrap();
        fs::write(pid_dir.join("cmdline"), b"python3\0-c\0print(1)\0").unwrap();
        fs::write(
            pid_dir.join("cgroup"),
            "0::/user.slice/user-1000.slice/user@1000.service/app.slice/lemond.service\n",
        )
        .unwrap();
        fs::write(pid_dir.join("io"), "read_bytes: 2048\nwrite_bytes: 4096\n").unwrap();
        fs::write(pid_dir.join("smaps_rollup"), "Pss:\t880000 kB\n").unwrap();
        fs::write(pid_dir.join("fd/0"), "").unwrap();
        fs::write(pid_dir.join("fd/1"), "").unwrap();
        symlink("/usr/bin/python3", pid_dir.join("exe")).unwrap();
        symlink("/home/kamjin", pid_dir.join("cwd")).unwrap();

        let detail = collect_detail_at(&root, 123, 2_000).expect("collect detail");
        let hz = clock_ticks_per_second();
        let expected_age = 2_000 - (1_000 + 5_000 / hz);

        assert_eq!(detail.name, "python3");
        assert_eq!(detail.state, 'S');
        assert_eq!(detail.ppid, 1);
        assert_eq!(detail.parent, "systemd");
        assert_eq!(detail.threads, 7);
        assert_eq!(detail.cmd, "python3 -c print(1)");
        assert_eq!(detail.exe.as_deref(), Some("/usr/bin/python3"));
        assert_eq!(detail.cwd.as_deref(), Some("/home/kamjin"));
        assert_eq!(detail.unit, "lemond.service");
        assert_eq!(
            detail.unit_hint.as_deref(),
            Some("systemctl --user stop lemond")
        );
        assert_eq!(detail.rss_kb, 900_000);
        assert_eq!(detail.hwm_kb, 905_000);
        assert_eq!(detail.swap_kb, 1_234);
        assert_eq!(detail.vm_size_kb, 1_200_000);
        assert_eq!(detail.pss_kb, Some(880_000));
        assert_eq!(detail.io, Some((2048, 4096)));
        assert_eq!(detail.fds, Some(2));
        assert_eq!(detail.age_secs, Some(expected_age));

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn kernel_thread_detail_falls_back_to_a_bracketed_name() {
        let root = std::env::temp_dir().join(format!("amdtop-detail-k{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let pid_dir = root.join("42");
        fs::create_dir_all(&pid_dir).unwrap();
        fs::write(
            pid_dir.join("status"),
            "Name:\tkworker/R-sync_wq\nUid:\t0\t0\t0\t0\n",
        )
        .unwrap();

        let detail = collect_detail_at(&root, 42, 2_000).expect("collect detail");

        assert_eq!(detail.cmd, "[kworker/R-sync_wq]");
        assert_eq!(detail.age_secs, None);
        assert_eq!(detail.exe, None);

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn collect_from_reads_every_pid_directory() {
        let root = std::env::temp_dir().join(format!("amdtop-procs-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        for (pid, name, rss, swap) in [(100, "a", 10, 0), (200, "b", 20, 30)] {
            let dir = root.join(pid.to_string());
            fs::create_dir_all(&dir).unwrap();
            fs::write(
                dir.join("status"),
                format!("Name:\t{name}\nVmRSS:\t{rss} kB\nVmSwap:\t{swap} kB\n"),
            )
            .unwrap();
        }
        fs::create_dir_all(root.join("not-a-pid")).unwrap();

        let gpu = HashMap::from([(200, gpu_proc(1, 2))]);
        let rows = collect_from(&root, &mut ProcessCpuSampler::default(), &gpu, TICK);

        assert_eq!(rows.len(), 2);
        let second = rows.iter().find(|r| r.pid == 200).unwrap();
        assert_eq!(second.total_kb(), 53);

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn parse_cpu_ticks_reads_user_plus_system_time_and_start_time() {
        let ticks = parse_cpu_ticks(&stat_line(123, 5, 7, 4_242)).expect("parse ticks");

        assert_eq!(ticks.ticks, 12);
        assert_eq!(ticks.start_ticks, 4_242);
        assert_eq!(parse_cpu_ticks("not a stat line"), None);
        assert_eq!(parse_cpu_ticks("123 (x) S 1 2"), None);
    }

    #[test]
    fn a_process_sampled_for_the_first_time_has_no_cpu_share_yet() {
        let mut sampler = ProcessCpuSampler::default();

        let first = sampler.sample(HashMap::from([(10, ticks(100, 1))]), TICK, HZ);
        assert!(first.is_empty(), "a baseline sample cannot report a share");

        let second = sampler.sample(HashMap::from([(10, ticks(150, 1))]), TICK, HZ);
        assert_eq!(second.get(&10), Some(&50.0));
    }

    #[test]
    fn cpu_share_is_scaled_by_the_actual_sampling_interval() {
        let mut sampler = ProcessCpuSampler::default();
        sampler.sample(HashMap::from([(1, ticks(0, 5))]), TICK, HZ);

        let share = sampler.sample(
            HashMap::from([(1, ticks(25, 5))]),
            Duration::from_millis(500),
            HZ,
        );

        assert_eq!(share.get(&1), Some(&50.0));
    }

    #[test]
    fn cpu_share_counts_every_thread_so_it_can_exceed_one_core() {
        let mut sampler = ProcessCpuSampler::default();
        sampler.sample(HashMap::from([(1, ticks(0, 5))]), TICK, HZ);

        let share = sampler.sample(HashMap::from([(1, ticks(900, 5))]), TICK, HZ);

        assert_eq!(share.get(&1), Some(&900.0));
    }

    #[test]
    fn a_reused_pid_is_not_measured_against_the_old_process() {
        let mut sampler = ProcessCpuSampler::default();
        sampler.sample(HashMap::from([(7, ticks(9_000, 100))]), TICK, HZ);

        let share = sampler.sample(HashMap::from([(7, ticks(10, 200))]), TICK, HZ);

        assert_eq!(share.get(&7), None);
    }

    #[test]
    fn a_process_that_vanished_and_returned_starts_over() {
        let mut sampler = ProcessCpuSampler::default();
        sampler.sample(HashMap::from([(3, ticks(0, 9))]), TICK, HZ);
        sampler.sample(HashMap::new(), TICK, HZ);

        let share = sampler.sample(HashMap::from([(3, ticks(500, 9))]), TICK, HZ);

        assert_eq!(share.get(&3), None, "a gap must not look like a 500% spike");
    }

    #[test]
    fn a_zero_interval_reports_nothing_instead_of_infinity() {
        let mut sampler = ProcessCpuSampler::default();
        sampler.sample(HashMap::from([(1, ticks(0, 5))]), TICK, HZ);

        let share = sampler.sample(HashMap::from([(1, ticks(50, 5))]), Duration::ZERO, HZ);

        assert!(share.is_empty());
    }

    #[test]
    fn sorting_by_cpu_puts_unsampled_rows_below_every_measured_row() {
        let mut rows = vec![
            ProcRow {
                pid: 1,
                cpu_percent: None,
                ..ProcRow::default()
            },
            ProcRow {
                pid: 2,
                cpu_percent: Some(12.0),
                ..ProcRow::default()
            },
            ProcRow {
                pid: 3,
                cpu_percent: Some(1_400.5),
                ..ProcRow::default()
            },
            ProcRow {
                pid: 4,
                cpu_percent: None,
                ..ProcRow::default()
            },
        ];

        sort_rows(&mut rows, SortKey::Cpu);

        assert_eq!(
            rows.iter().map(|r| r.pid).collect::<Vec<_>>(),
            vec![3, 2, 1, 4]
        );
    }

    #[test]
    fn collect_from_reports_cpu_for_processes_without_a_gpu_context() {
        let root = std::env::temp_dir().join(format!("amdtop-cpu-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        for (pid, name) in [(100, "chrome"), (200, "llama-server")] {
            let dir = root.join(pid.to_string());
            fs::create_dir_all(&dir).unwrap();
            fs::write(
                dir.join("status"),
                format!("Name:\t{name}\nVmRSS:\t10 kB\n"),
            )
            .unwrap();
            fs::write(dir.join("stat"), stat_line(pid, 0, 0, pid as u64)).unwrap();
        }

        let mut sampler = ProcessCpuSampler::default();
        let gpu = HashMap::new();
        let first = collect_from(&root, &mut sampler, &gpu, TICK);
        assert!(
            first.iter().all(|row| row.cpu_percent.is_none()),
            "the first sample only records a baseline"
        );

        // One second later: chrome burned 0.5 CPU-seconds, llama-server 2.0.
        fs::write(root.join("100/stat"), stat_line(100, 50, 0, 100)).unwrap();
        fs::write(root.join("200/stat"), stat_line(200, 120, 80, 200)).unwrap();
        let second = collect_from(&root, &mut sampler, &gpu, TICK);

        let cpu_of = |pid: i32| {
            second
                .iter()
                .find(|row| row.pid == pid)
                .expect("row")
                .cpu_percent
        };
        assert_eq!(cpu_of(100), Some(100.0 * 50.0 / HZ as f32));
        assert_eq!(cpu_of(200), Some(100.0 * 200.0 / HZ as f32));
        assert!(
            second.iter().all(|row| !row.has_gpu()),
            "CPU usage must not depend on holding a GPU context"
        );

        fs::remove_dir_all(root).unwrap();
    }
}
