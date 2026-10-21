//! Detail pane under the process table.
//!
//! Shows what the row cannot: the full command line, executable, cwd, the
//! systemd unit (with the command that manages it), io counters and lifetime.
//! GPU numbers are intentionally absent — the selected row already shows
//! VRAM/GTT/engine percentages, and repeating them just adds noise.

use ratatui::Frame;
use ratatui::layout::{Alignment, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph};

use super::processes::fmt_kib;
use crate::app::App;
use crate::processes::{ProcDetail, format_age, shorten_unit};

/// Label gutter: `" unit "` = one space, four characters, one space.
const GUTTER: usize = 6;

/// Build the pane content. The caller derives the pane height from the number
/// of lines, so a wrapped command line gets a second row for free.
pub(super) fn lines(app: &App, detail: &ProcDetail, width: u16) -> Vec<Line<'static>> {
    let dim = Style::default().fg(app.theme.proc_misc());
    let value = Style::default().fg(app.theme.main_fg());
    let accent = Style::default()
        .fg(app.theme.hi_fg())
        .add_modifier(Modifier::BOLD);
    let available = (width as usize).saturating_sub(GUTTER);

    let mut lines = Vec::with_capacity(8);

    // Identity: everything that describes *what* this process is.
    let mut identity = vec![
        ("uid", detail.uid.to_string()),
        ("state", state_word(detail.state).to_string()),
        ("threads", detail.threads.to_string()),
    ];
    if detail.ppid > 0 {
        let parent = if detail.parent.is_empty() {
            String::new()
        } else {
            format!(" ({})", detail.parent)
        };
        identity.push(("ppid", format!("{}{parent}", detail.ppid)));
    }
    if let Some(age) = detail.age_secs {
        identity.push(("up", format_age(age)));
    }
    if let Some(fds) = detail.fds {
        identity.push(("fds", fds.to_string()));
    }
    lines.push(field_pairs("info", &identity, dim, value));

    lines.push(field(
        "exe",
        clip(detail.exe.as_deref().unwrap_or("-"), available),
        dim,
        value,
    ));
    lines.push(field(
        "cwd",
        clip(detail.cwd.as_deref().unwrap_or("-"), available),
        dim,
        value,
    ));

    // The command line is the reason this pane exists, so it may use two rows.
    let cmd_rows = wrap(&detail.cmd, available, 2);
    lines.push(field("cmd", cmd_rows[0].clone(), dim, value));
    if let Some(continuation) = cmd_rows.get(1) {
        lines.push(Line::from(vec![
            Span::raw(" ".repeat(GUTTER)),
            Span::styled(continuation.clone(), value),
        ]));
    }

    lines.push(field_pairs(
        "mem",
        &[
            ("RSS", fmt_kib(Some(detail.rss_kb))),
            (
                "PSS",
                detail
                    .pss_kb
                    .map(|pss| fmt_kib(Some(pss)))
                    .unwrap_or_else(|| "-".into()),
            ),
            ("HWM", fmt_kib(Some(detail.hwm_kb))),
            ("SWAP", fmt_kib(Some(detail.swap_kb))),
            ("VmSize", fmt_kib(Some(detail.vm_size_kb))),
        ],
        dim,
        value,
    ));

    // Unit line: the unit plus the command a human would use to manage it.
    let unit = if detail.unit.is_empty() {
        "-".to_string()
    } else {
        shorten_unit(&detail.unit)
    };
    let mut unit_line = vec![
        Span::styled(format!(" {:<4} ", "unit"), dim),
        Span::styled(unit, value),
    ];
    if let Some(hint) = &detail.unit_hint {
        unit_line.push(Span::styled("  →  ", dim));
        unit_line.push(Span::styled(hint.clone(), accent));
    }
    lines.push(Line::from(unit_line));

    lines.push(field_pairs(
        "io",
        &match detail.io {
            Some((read, write)) => vec![("read", fmt_io(read)), ("write", fmt_io(write))],
            None => vec![("read", "-".to_string()), ("write", "-".to_string())],
        },
        dim,
        value,
    ));

    lines
}

pub(super) fn draw(
    f: &mut Frame,
    area: Rect,
    app: &App,
    detail: &ProcDetail,
    content: Vec<Line<'static>>,
) {
    let dim = Style::default().fg(app.theme.proc_misc());
    let block = Block::default()
        .borders(Borders::TOP)
        .border_style(dim)
        .title_top(Span::styled(
            format!(" detail · {} {} ", detail.pid, detail.name),
            Style::default()
                .fg(app.theme.hi_fg())
                .add_modifier(Modifier::BOLD),
        ))
        .title_top(Line::from(Span::styled(" d/Esc close ", dim)).alignment(Alignment::Right));

    f.render_widget(Paragraph::new(content).block(block), area);
}

/// `label  key value · key value …` with the label gutter kept constant.
fn field_pairs(
    label: &'static str,
    items: &[(&'static str, String)],
    dim: Style,
    value: Style,
) -> Line<'static> {
    let mut spans = vec![Span::styled(format!(" {label:<4} "), dim)];
    for (index, (key, text)) in items.iter().enumerate() {
        if index > 0 {
            spans.push(Span::styled(" · ", dim));
        }
        spans.push(Span::styled(format!("{key} "), dim));
        spans.push(Span::styled(text.clone(), value));
    }
    Line::from(spans)
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

fn state_word(state: char) -> &'static str {
    match state {
        'R' => "running",
        'S' => "sleeping",
        'D' => "disk sleep",
        'Z' => "zombie",
        'T' => "stopped",
        't' => "tracing",
        'I' => "idle",
        'X' => "dead",
        _ => "unknown",
    }
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

/// Keep a long value on one row, marking the cut with an ellipsis.
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

/// Split text into at most `max_rows` rows of `width` characters.
fn wrap(text: &str, width: usize, max_rows: usize) -> Vec<String> {
    let chars: Vec<char> = text.chars().collect();
    if width == 0 || chars.is_empty() {
        return vec![String::new()];
    }
    let mut rows = Vec::new();
    let mut start = 0;
    while start < chars.len() && rows.len() < max_rows {
        let end = (start + width).min(chars.len());
        let mut row: String = chars[start..end].iter().collect();
        if end < chars.len() && rows.len() + 1 == max_rows {
            row.pop();
            row.push('…');
        }
        rows.push(row);
        start = end;
    }
    rows
}

#[cfg(test)]
mod tests {
    use super::{clip, fmt_io, state_word, wrap};

    #[test]
    fn long_text_is_clipped_with_an_ellipsis() {
        assert_eq!(clip("abcdef", 10), "abcdef");
        assert_eq!(clip("abcdef", 4), "abc…");
        assert_eq!(clip("abcdef", 0), "");
    }

    #[test]
    fn command_lines_wrap_to_at_most_two_rows() {
        assert_eq!(wrap("abc", 10, 2), vec!["abc"]);
        assert_eq!(wrap("abcdefgh", 4, 2), vec!["abcd", "efgh"]);
        assert_eq!(wrap("abcdefghij", 4, 2), vec!["abcd", "efg…"]);
        assert_eq!(wrap("", 4, 2), vec![""]);
    }

    #[test]
    fn io_counters_keep_kilobyte_resolution() {
        assert_eq!(fmt_io(0), "0K");
        assert_eq!(fmt_io(2048), "2K");
        assert_eq!(fmt_io(5 << 20), "5M");
    }

    #[test]
    fn process_states_get_words_instead_of_letters() {
        assert_eq!(state_word('R'), "running");
        assert_eq!(state_word('S'), "sleeping");
        assert_eq!(state_word('Z'), "zombie");
        assert_eq!(state_word('?'), "unknown");
    }
}
