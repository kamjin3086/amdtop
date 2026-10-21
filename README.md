# amdtop

**amdtop is an independent terminal system monitor for AMD systems.** It is
inspired by the modern TUI visual style of
[nvitop](https://github.com/XuehaiPan/nvitop) and
[btop](https://github.com/aristocratos/btop), and leverages the published
[`libamdgpu_top`](https://crates.io/crates/libamdgpu_top) crate for AMD GPU
telemetry. It monitors CPUs, AMD GPUs (discrete and APUs), and Strix Halo XDNA
NPUs.

![amdtop screenshot](docs/screenshot.png)

## Why

`nvitop` is great but NVIDIA-only. On AMD — especially a Strix Halo box with an
APU **and** an XDNA NPU, or a workstation with multiple AMD cards there was no 
single TUI with that look-and-feel. amdtop fills that gap by combining its own
CPU/system monitoring with `libamdgpu_top` telemetry in a modern, themeable
layout.

## Features

- **Collapsible CPU / GPU / NPU / Processes sections** (state persists across runs)
- **CPU section, btop-style**: per-core grid with braille history mini-graphs,
  package temp/power and load average folded into the box title, MEM and SWP
  sharing one row, and a core grid that uses more columns on wide terminals
  (32 cores = 4 rows instead of 8)
- **Multi-GPU**: one band per device, labeled by index + PCI bus-id
- **APU-aware memory**: shows the real GTT (unified system RAM) pool on APUs,
  VRAM on discrete cards — both labeled `MEM`
- **Adaptive memory-bandwidth telemetry**: shows memory-controller utilization
  where available alongside SoC DRAM read/write throughput on supported APUs
- **XDNA NPU**: presence detection on `/sys/class/accel`; utilization + per-context table when the `amdxdna` driver exposes DRM fdinfo telemetry, or two compact rows when no context is active
- **Space-saving layout**: every row saved above goes to the process table —
  the CPU/GPU/NPU sections keep only what they need and PROCESSES fills the
  rest of the screen.
- **Process table with unified-memory totals**: every process (not just DRM
  clients), with `MEM` + `SWAP` + `VRAM` + `GTT` rolled into a `TOTAL`
  column; walk columns with `←`/`→` to sort each one biggest-first
- **41 native bundled themes**: available without btop or external data files;
  cycle live with `t`/`T`. Defaults to `tokyo-night`.
- nvitop-style fixed-track gauges with aligned numeric columns; braille area
  graphs with theme-gradient fills

## Install

Requires **Rust 1.88 or newer** and **`libdrm` development headers**, plus an
AMD GPU/APU running the `amdgpu` kernel driver.

Install from crates.io:

```sh
cargo install amdtop
```

On Arch Linux and derivatives, install the
[`amdtop`](https://aur.archlinux.org/packages/amdtop) AUR package
([pkg](https://github.com/lhl/amdtop-aur)):

```sh
yay -S amdtop
```

Or install the latest development version from Git:

```sh
cargo install --git https://github.com/lhl/amdtop
```

This installs the `amdtop` binary. The published `libamdgpu_top` crate is
pulled from crates.io and compiled automatically. The only runtime dependency
is `libdrm_amdgpu.so.1`, which is present on systems with AMD drivers.

Distro packages for the `libdrm` build headers:

| Distro | Package |
|---|---|
| Arch | `libdrm` |
| Debian/Ubuntu | `libdrm-dev` |
| Fedora | `libdrm-devel` |

### NPU telemetry requirements

`amdtop` detects AMD XDNA/Ryzen AI NPUs through the Linux accel class
(`/sys/class/accel` and `/dev/accel/accel*`). Device detection is enough to show
identity and firmware information, but not live utilization.

Utilization and per-context monitoring require a cumulative `drm-engine-*`
busy-time counter from the loaded `amdxdna` kernel module. A `drm-driver` line
or an installed DKMS package alone is not sufficient. XDNA driver trees also
currently emit more than one engine-key name, and released telemetry parsers do
not recognize all of them.

> **Temporary Arch/AUR caveat (checked 2026-07-22):** the examined
> `amdxdna-dkms` package's kernel allowlist permits only Linux 6.17, 6.18, and
> 6.19. On other kernel versions, installing the package may leave its source in
> `/usr/src` without building or loading a DKMS module. Check `dkms status` and
> the loaded module rather than treating package installation as success.

See the [XDNA NPU telemetry and workload guide](docs/NPU.md) for direct fdinfo
checks, the current driver/parser compatibility matrix, matched XRT/driver build
instructions, Arch package caveats, memlock setup, validation workloads, and
troubleshooting.

## Usage

```sh
amdtop
```

### GPU numbering

amdtop lists all physical GPUs in PCI BDF order and shows each BDF beside its
index. This normally matches the physical ordering from `rocm-smi` and
`rocminfo`. `HIP_VISIBLE_DEVICES` and `ROCR_VISIBLE_DEVICES` can hide or remap
GPU ordinals inside a particular compute process; they do not change amdtop's
system-wide numbering. Use the displayed PCI BDF as the authoritative mapping.

### Keybindings

| Key | Action |
|---|---|
| `q` / `Esc` / `Ctrl+C` | quit |
| `Tab` / `Shift+Tab` | move between sections |
| `Space` / `Enter` | collapse / expand the focused section |
| `t` / `T` | next / previous theme |
| `b` / `B` | next / previous gauge block style |
| `←` / `→` | move the sort cursor across columns; wraps around, either arrow enters the cycle at TOTAL |
| `Esc` | unwind one layer: close the detail pane, then clear the sort, then quit |
| `a` | (PROCESSES) show all processes / GPU clients only |
| `d` | (PROCESSES) show / hide the detail pane for the selected process |
| `x` | (PROCESSES) stop the selected process — asks first, sends `SIGTERM` |
| `j` `k`, `PgUp` `PgDn`, `Home` `End` | (PROCESSES) scroll the list |

Section collapse state, the selected theme, the gauge block style, and the
process-table sort/filter all persist across runs.

### Process table: sorting by real unified-memory use

The panel can list **every process**, not only the ones holding a DRM
context, because a process that never touches the GPU can still pin several
gigabytes of unified memory through RSS and swap (containers, language
servers, build daemons). Kernel threads and zombies, which have no address
space, are filtered out.

Columns, left to right:

```text
TOTAL = MEM + SWAP + VRAM + GTT
```

- `MEM` is resident set size (`VmRSS`).
- `SWAP` is `VmSwap` — pages that were compressed into zram. They still
  occupy physical memory, but they disappear from `MEM`, which is why a
  heavy process can look tiny in a plain `MEM`-only list.
- `VRAM` / `GTT` come from `drm-resident-*` in `/proc/<pid>/fdinfo`.

Sorting is a **column cursor**, not a mode:

1. `Tab` to the PROCESSES section (the bottom one).
2. Press `→` once: the cursor lands on `TOTAL` and the list sorts
   biggest-first. Press `→` again to move to `MEM`, then `SWAP`, and so on
   to the right.
3. Press `←` to step back through the columns.
4. Both arrows wrap around: `→` past `CPU%` returns to `TOTAL`, `←` past
   `TOTAL` jumps to `CPU%`. From the unsorted view either arrow enters at
   `TOTAL` (the most useful column).
5. Press `Esc` to drop the sort and return to the PID-ordered view.

The cursor is drawn as an inverted header cell with a `▼` marker, and the
current state is shown in the panel border (`sort TOTAL▼ · all 321`).

`TOTAL` adds shared pages once per process, so adding the column up across
rows over-counts memory that several processes map. Use it to rank
processes, not to compute the system total — the CPU section's MEM/SWP bars
remain the authoritative system figures.

### Detail pane

Press `d` (the footer lists it while PROCESSES is focused) to open a
full-width pane directly under the table; `d` or `Esc` closes it again. It
follows the selection, so `j`/`k` walks processes while the pane stays in sync:

```text
 detail · 3155842 llama-server ─────────────────────────── d close
 info uid 1000 · state sleeping · threads 38 · ppid 1587 (lemond) · up 5d5h · fds 7
 exe  /opt/local/.../vulkan/bin/llama-server
 cwd  /home/kamjin
 cmd  /opt/local/.../llama-server -m .../LFM2.5-2.6B-heretic-Q8_0.gguf --ctx-size 6144
 mem  RSS 197M · PSS 192M · HWM 724M · SWAP 843M · VmSize 8.2G
 unit lemond.service  →  systemctl --user stop lemond
 io   read 0K · write 4K
```

GPU numbers are deliberately not repeated here: the selected row right above
already shows VRAM/GTT and the engine percentages. Values use one aligned
gutter and `·` separators so a line can be scanned without counting spaces;
long command lines get a second, indented row.

The `unit` line is the answer to "how do I really stop this?" — amdtop shows
the owning systemd unit or container scope and the command that manages it,
but never runs service managers itself (see below).

### Stopping a process (graceful only)

Press `x` on the selected row to open a confirmation dialog. `y` or `Enter`
sends **SIGTERM**; `n` or `Esc` cancels.

- Only `SIGTERM` is ever sent: the process is asked to exit and can flush
  state, finish writes, and shut down its children. There is deliberately no
  force-kill key.
- The dialog names the process, its PID, and its `TOTAL` / `MEM` / `SWAP`
  footprint, so a mis-selected row is caught before anything happens.
- Targets that cannot be signalled safely are refused with a one-line notice:
  init (PID 1), amdtop itself, and other users' processes (unless running as
  root).
- The result is reported in the panel border (`SIGTERM sent to …`,
  `permission denied`, `process already exited`) for a few seconds, and the
  row disappears on the next sample.
- If the process belongs to a supervised unit (a `.service`, or a Docker
  scope), the dialog says so: the supervisor's restart policy may bring it
  straight back.

amdtop deliberately does **not** call `systemctl`, `docker` or similar.
`systemctl stop` sends its own `SIGTERM` and then a `SIGKILL` after
`TimeoutStopSec`, and `docker stop` force-kills after 10s by default — both
break the "ask, never force" rule. Stopping a *unit* is also a different
intent from stopping a *process* (restart policies, dependencies, socket
activation). So the panel names the unit and the command; you decide.

## Configuration

amdtop stores its UI state in:

```text
$XDG_CONFIG_HOME/amdtop/state.json
```

If `XDG_CONFIG_HOME` is unset, it uses `~/.config/amdtop/state.json`.

### GPU power management

A GPU that is already runtime-suspended when amdtop starts remains listed as
`sleeping`; amdtop does not wake it merely to collect telemetry. When another
workload wakes the GPU, amdtop initializes its telemetry in place without
changing the GPU's index.

For GPUs that are awake when monitoring begins, amdtop keeps discrete-GPU
device handles open. This avoids stale utilization and sensor readings after a
low-power transition or system resume. To allow runtime D3Hot power management
while amdtop is running, set `AGT_NO_DROP=0`; doing so may make telemetry
unavailable until amdtop is restarted.

### Gauge block styles

Cycle the bar fill glyph with `b`/`B`:
`3/4` (▊, default), `smooth` (precise fractional █), `dotmatrix` (⣿ LED cell),
`lines` (━/─), `squares` (■/□), `rects` (▮/▯), `pills` (▰/▱).

## Themes

amdtop embeds 41 themes in its binary, so theme selection does not depend on
btop being installed. Native custom themes use amdtop's versioned TOML format
and can independently style CPU, GPU, memory, NPU, processes, borders, clocks,
power, fans, bandwidth, and positioned gradient stops.

User themes are loaded from `$XDG_CONFIG_HOME/amdtop/themes/` and
`~/.config/amdtop/themes/`; system themes may be installed under
`/usr/local/share/amdtop/themes/` or `/usr/share/amdtop/themes/`. A native file
with the same name as a bundled theme overrides it. See
[the native theme format](docs/THEMES.md) for the complete schema and examples.

## Credits

- AMD GPU telemetry: [`libamdgpu_top`](https://crates.io/crates/libamdgpu_top) by Umio-Yasuno
- Inspiration: [nvitop](https://github.com/XuehaiPan/nvitop), [btop](https://github.com/aristocratos/btop)
- Themes from btop and original theme authors; see [THIRD_PARTY.md](THIRD_PARTY.md)

## License

Apache-2.0
