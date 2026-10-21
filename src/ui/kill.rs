//! "Stop process?" confirmation popup.
//!
//! Deliberately small and explicit: it names the process, shows what it holds,
//! and states that the process is *asked* to exit (SIGTERM), so it can flush
//! state. There is no force-kill option here.

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::symbols::border;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph};

use crate::app::App;
use crate::kill::KillTarget;
use crate::processes::shorten_unit;

use super::processes::fmt_kib;

const WIDTH: u16 = 64;

pub(super) fn draw(f: &mut Frame, area: Rect, app: &App, target: &KillTarget) {
    let height = 9 + u16::from(target.unit.is_some());
    if area.width < 32 || area.height < height + 2 {
        return;
    }
    let width = WIDTH.min(area.width - 2);
    let rect = Rect::new(
        area.x + (area.width - width) / 2,
        area.y + (area.height - height) / 2,
        width,
        height,
    );
    f.render_widget(Clear, rect);

    let accent = Style::default()
        .fg(app.theme.hi_fg())
        .add_modifier(Modifier::BOLD);
    let text = Style::default().fg(app.theme.main_fg());
    let dim = Style::default().fg(app.theme.graph_text());
    let key = Style::default()
        .fg(app.theme.selected_fg())
        .bg(app.theme.selected_bg())
        .add_modifier(Modifier::BOLD);

    let name: String = target.name.chars().take(28).collect();
    let mut lines = vec![
        Line::from(vec![
            Span::styled(name, accent),
            Span::styled(format!("   pid {}", target.pid), text),
        ]),
        Line::from(Span::styled(
            format!(
                "TOTAL {}   MEM {}   SWAP {}",
                fmt_kib(Some(target.total_kb)),
                fmt_kib(Some(target.mem_kb)),
                if target.swap_kb == 0 {
                    "-".to_string()
                } else {
                    fmt_kib(Some(target.swap_kb))
                },
            ),
            text,
        )),
        Line::from(""),
        Line::from(Span::styled(
            "Sends SIGTERM; the process can clean up and exit on its own.",
            dim,
        )),
    ];
    if let Some(unit) = &target.unit {
        lines.push(Line::from(Span::styled(
            format!(
                "managed by {}: a restart policy may restart it",
                shorten_unit(unit)
            ),
            dim,
        )));
    }
    lines.push(Line::from(""));
    lines.push(Line::from(vec![
        Span::styled(" y / Enter ", key),
        Span::styled("stop      ", text),
        Span::styled(" n / Esc ", key),
        Span::styled("cancel", text),
    ]));

    let block = Block::default()
        .borders(Borders::ALL)
        .border_set(border::ROUNDED)
        .border_style(Style::default().fg(app.theme.hi_fg()))
        .title(Span::styled(" stop process? ", accent));

    f.render_widget(Paragraph::new(lines).block(block), rect);
}
