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
  package temp/power, load average, plus system MEM/SWP
- **Multi-GPU**: one band per device, labeled by index + PCI bus-id
- **APU-aware memory**: shows the real GTT (unified system RAM) pool on APUs,
  VRAM on discrete cards — both labeled `MEM`
- **Adaptive memory-bandwidth telemetry**: shows memory-controller utilization
  where available alongside SoC DRAM read/write throughput on supported APUs
- **XDNA NPU**: presence detection on `/sys/class/accel`; utilization + per-context table when the `amdxdna` driver exposes DRM fdinfo telemetry
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
| `←` / `→` | (PROCESSES focused) move the sort cursor across columns |
| `Esc` | (PROCESSES) clear the sort; otherwise quit |
| `a` | (PROCESSES) show all processes / GPU clients only |
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
4. Press `Esc` to drop the sort and return to the PID-ordered view.

The cursor is drawn as an inverted header cell with a `▼` marker, and the
current state is shown in the panel border (`sort TOTAL▼ · all 321`).

`TOTAL` adds shared pages once per process, so adding the column up across
rows over-counts memory that several processes map. Use it to rank
processes, not to compute the system total — the CPU section's MEM/SWP bars
remain the authoritative system figures.

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
