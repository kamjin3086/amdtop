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

use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::Path;

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

    /// Column to the right. Clamps on the last column instead of wrapping, so
    /// holding the key never jumps back to the start.
    pub fn next_column(self) -> Self {
        Self::ALL[(self.index() + 1).min(Self::ALL.len() - 1)]
    }

    /// Column to the left; `None` when stepping left of the first column,
    /// which means "back to the unsorted, PID-ordered view".
    pub fn prev_column(self) -> Option<Self> {
        let index = self.index();
        (index > 0).then(|| Self::ALL[index - 1])
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
    pub cpu: i64,
}

impl GpuProc {
    pub(crate) fn absorb(&mut self, other: &GpuProc) {
        self.vram_kb = self.vram_kb.saturating_add(other.vram_kb);
        self.gtt_kb = self.gtt_kb.saturating_add(other.gtt_kb);
        self.gfx = self.gfx.max(other.gfx);
        self.compute = self.compute.max(other.compute);
        self.dma = self.dma.max(other.dma);
        self.cpu = self.cpu.max(other.cpu);
    }
}

/// One row of the PROCESSES table.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ProcRow {
    pub pid: i32,
    pub name: String,
    /// Resident set size in KiB (`VmRSS`).
    pub mem_kb: u64,
    /// Swapped-out pages in KiB (`VmSwap`); they live compressed in zram.
    pub swap_kb: u64,
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

    pub fn cpu(&self) -> i64 {
        self.gpu.as_ref().map_or(0, |g| g.cpu)
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
            SortKey::Cpu => self.cpu().max(0) as u64,
        }
    }
}

/// The parts of `/proc/<pid>/status` this panel needs.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ProcStatus {
    pub name: String,
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
/// `gpu_only` keeps the upstream behaviour (DRM clients only); otherwise every
/// live process is kept so memory hogs without a GPU context remain visible.
pub fn merge_rows(
    statuses: Vec<(i32, ProcStatus)>,
    gpu: &HashMap<i32, GpuProc>,
    gpu_only: bool,
) -> Vec<ProcRow> {
    let mut rows = Vec::with_capacity(statuses.len());
    let mut seen = HashSet::with_capacity(statuses.len());

    for (pid, status) in statuses {
        seen.insert(pid);
        let gpu = gpu.get(&pid).cloned();
        if gpu_only && gpu.is_none() {
            continue;
        }
        // Kernel threads / zombies have no address space: they cannot own
        // memory, so listing them is pure noise in a memory panel.
        if !status.has_mm && gpu.is_none() {
            continue;
        }
        rows.push(ProcRow {
            pid,
            name: status.name,
            mem_kb: status.rss_kb,
            swap_kb: status.swap_kb,
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

/// Sort rows in place. Largest value first; ties fall back to ascending PID so
/// the list does not jitter when several processes report the same value.
pub fn sort_rows(rows: &mut [ProcRow], key: SortKey) {
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

/// Read `/proc` and return the model used by the panel.
pub fn collect(gpu: &HashMap<i32, GpuProc>, gpu_only: bool) -> Vec<ProcRow> {
    collect_from(Path::new("/proc"), gpu, gpu_only)
}

fn collect_from(root: &Path, gpu: &HashMap<i32, GpuProc>, gpu_only: bool) -> Vec<ProcRow> {
    let Ok(entries) = fs::read_dir(root) else {
        return merge_rows(Vec::new(), gpu, gpu_only);
    };

    let mut statuses = Vec::new();
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
        statuses.push((pid, parse_status(&status)));
    }

    merge_rows(statuses, gpu, gpu_only)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn gpu_proc(vram_kb: u64, gtt_kb: u64) -> GpuProc {
        GpuProc {
            gpu_index: 0,
            vram_kb,
            gtt_kb,
            ..GpuProc::default()
        }
    }

    fn status(name: &str, rss_kb: u64, swap_kb: u64) -> ProcStatus {
        ProcStatus {
            name: name.into(),
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
            name: "x".into(),
            mem_kb: 100,
            swap_kb: 200,
            gpu: Some(gpu_proc(300, 400)),
        };

        assert_eq!(row.total_kb(), 1000);
    }

    #[test]
    fn total_kb_is_just_mem_plus_swap_without_a_gpu_context() {
        let row = ProcRow {
            pid: 1,
            name: "hindsight-api".into(),
            mem_kb: 32_000,
            swap_kb: 1_400_000,
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

        let rows = merge_rows(statuses, &gpu, false);

        assert_eq!(rows.len(), 2);
        assert_eq!(rows.iter().find(|r| r.pid == 2).unwrap().total_kb(), 550);
    }

    #[test]
    fn merge_rows_gpu_only_drops_processes_without_a_gpu_context() {
        let statuses = vec![
            (1, status("llama-server", 100, 0)),
            (2, status("python", 50, 500)),
        ];
        let gpu = HashMap::from([(1, gpu_proc(10, 20))]);

        let rows = merge_rows(statuses, &gpu, true);

        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].pid, 1);
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

        let rows = merge_rows(statuses, &HashMap::new(), false);

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

        let rows = merge_rows(Vec::new(), &gpu, true);

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
            &HashMap::from([(1, gpu)]),
            false,
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
    fn right_arrow_walks_columns_left_to_right_and_clamps_on_the_last_one() {
        let mut key = SortKey::Total;
        let mut seen = vec![key];
        for _ in 1..SortKey::ALL.len() {
            key = key.next_column();
            seen.push(key);
        }

        assert_eq!(seen, SortKey::ALL.to_vec());
        assert_eq!(SortKey::Cpu.next_column(), SortKey::Cpu);
    }

    #[test]
    fn left_arrow_walks_back_and_clears_before_the_first_column() {
        assert_eq!(SortKey::Total.prev_column(), None);
        assert_eq!(SortKey::Mem.prev_column(), Some(SortKey::Total));
        assert_eq!(SortKey::Cpu.prev_column(), Some(SortKey::Dma));
    }

    #[test]
    fn sort_key_ids_round_trip() {
        for key in SortKey::ALL {
            assert_eq!(SortKey::from_id(key.id()), Some(key));
        }
        assert_eq!(SortKey::from_id("nope"), None);
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
        let rows = collect_from(&root, &gpu, false);

        assert_eq!(rows.len(), 2);
        let second = rows.iter().find(|r| r.pid == 200).unwrap();
        assert_eq!(second.total_kb(), 53);

        fs::remove_dir_all(root).unwrap();
    }
}
