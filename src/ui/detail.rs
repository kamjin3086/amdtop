//! Detail pane under the process table.
//!
//! Shows what the row cannot: full command line, executable, cwd, the systemd
//! unit (with the command that manages it), io counters and process lifetime.

use ratatui::Frame;
use ratatui::layout::{Alignment, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph};

use super::processes::fmt_kib;
use crate::app::App;
use crate::processes::{ProcDetail, format_age, shorten_unit};

/// Content lines, plus the separator drawn above them.
pub(super) const LINES: u16 = 8;
pub(super) const HEIGHT: u16 = LINES + 1;

pub(super) fn draw(f: &mut Frame, area: Rect, app: &App, detail: &ProcDetail) {
    let dim = Style::default().fg(app.theme.proc_misc());
    let value = Style::default().fg(app.theme.main_fg());
    let wide = area.width as usize;

    let mut lines = Vec::with_capacity(LINES as usize);
    lines.push(Line::from(vec![
        Span::styled(" state ", dim),
        Span::styled(detail.state.to_string(), value),
        Span::styled("   uid ", dim),
        Span::styled(detail.uid.to_string(), value),
        Span::styled("   threads ", dim),
        Span::styled(detail.threads.to_string(), value),
        Span::styled("   ppid ", dim),
        Span::styled(format!("{} ({})", detail.ppid, detail.parent), value),
        Span::styled("   age ", dim),
        Span::styled(
            detail
                .age_secs
                .map(format_age)
                .unwrap_or_else(|| "-".into()),
            value,
        ),
        Span::styled("   fds ", dim),
        Span::styled(
            detail
                .fds
                .map(|fds| fds.to_string())
                .unwrap_or_else(|| "-".into()),
            value,
        ),
    ]));
    lines.push(field(
        "exe",
        detail.exe.clone().unwrap_or_else(|| "-".into()),
        dim,
        value,
    ));
    lines.push(field(
        "cwd",
        detail.cwd.clone().unwrap_or_else(|| "-".into()),
        dim,
        value,
    ));
    lines.push(field(
        "cmd",
        clip(&detail.cmd, wide.saturating_sub(7)),
        dim,
        value,
    ));
    lines.push(field(
        "mem",
        format!(
            "RSS {}   PSS {}   HWM {}   SWAP {}   VmSize {}",
            fmt_kib(Some(detail.rss_kb)),
            detail
                .pss_kb
                .map(|pss| fmt_kib(Some(pss)))
                .unwrap_or_else(|| "-".into()),
            fmt_kib(Some(detail.hwm_kb)),
            fmt_kib(Some(detail.swap_kb)),
            fmt_kib(Some(detail.vm_size_kb)),
        ),
        dim,
        value,
    ));
    lines.push(field(
        "gpu",
        match &detail.gpu {
            Some(gpu) => format!(
                "GTT {}   VRAM {}   GFX {}%   COMP {}%   DMA {}%   CPU {}%",
                fmt_kib(Some(gpu.gtt_kb)),
                fmt_kib(Some(gpu.vram_kb)),
                gpu.gfx,
                gpu.compute,
                gpu.dma,
                gpu.cpu
            ),
            None => "-".into(),
        },
        dim,
        value,
    ));
    lines.push(field(
        "unit",
        match &detail.unit_hint {
            Some(hint) => format!("{}   stop with: {hint}", shorten_unit(&detail.unit)),
            None => unit_or_dash(&detail.unit),
        },
        dim,
        value,
    ));
    lines.push(field(
        "io",
        match detail.io {
            Some((read, write)) => {
                format!("read {}   write {}", fmt_io(read), fmt_io(write))
            }
            None => "-".into(),
        },
        dim,
        value,
    ));

    let title = format!(" detail · {} {} ", detail.pid, detail.name);
    let block = Block::default()
        .borders(Borders::TOP)
        .border_style(Style::default().fg(app.theme.proc_misc()))
        .title_top(Span::styled(
            title,
            Style::default()
                .fg(app.theme.hi_fg())
                .add_modifier(Modifier::BOLD),
        ))
        .title_top(Line::from(Span::styled(" d close ", dim)).alignment(Alignment::Right));

    f.render_widget(Paragraph::new(lines).block(block), area);
}

fn field(
    label: &'static str,
    text: String,
    label_style: Style,
    value_style: Style,
) -> Line<'static> {
    Line::from(vec![
        Span::styled(format!(" {label:<4} "), label_style),
        Span::styled(text, value_style),
    ])
}

/// Byte counters are often in the kilobyte range, where `fmt_kib` would round
/// everything to `0M`; keep one more digit for those.
fn fmt_io(bytes: u64) -> String {
    let kib = bytes / 1024;
    if kib < 1024 {
        format!("{kib}K")
    } else {
        fmt_kib(Some(kib))
    }
}

fn unit_or_dash(unit: &str) -> String {
    if unit.is_empty() {
        "-".to_string()
    } else {
        shorten_unit(unit)
    }
}

/// Keep a long command line on one row; the full text is rarely readable in a
/// single pane line anyway, and truncation keeps the remaining fields visible.
fn clip(text: &str, max_chars: usize) -> String {
    if max_chars == 0 {
        return String::new();
    }
    let mut clipped: String = text.chars().take(max_chars).collect();
    if text.chars().count() > max_chars {
        clipped.pop();
        clipped.push('…');
    }
    clipped
}

#[cfg(test)]
mod tests {
    use super::{clip, fmt_io};

    #[test]
    fn io_counters_keep_kilobyte_resolution() {
        assert_eq!(fmt_io(0), "0K");
        assert_eq!(fmt_io(2048), "2K");
        assert_eq!(fmt_io(5 << 20), "5M");
    }

    #[test]
    fn long_text_is_clipped_with_an_ellipsis() {
        assert_eq!(clip("abcdef", 10), "abcdef");
        assert_eq!(clip("abcdef", 4), "abc…");
        assert_eq!(clip("abcdef", 0), "");
    }
}
