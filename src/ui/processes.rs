use ratatui::Frame;
use ratatui::layout::{Alignment, Constraint, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Cell, Paragraph, Row, Table};

use crate::app::{App, Section};
use crate::processes::{ProcRow, SortKey};
use crate::theme::SectionBox;

use super::detail;
use super::section_block;

/// One display column. Identity columns are not sortable; the memory block
/// starts at TOTAL so the first right-arrow press is the useful one.
struct Column {
    header: &'static str,
    sort: Option<SortKey>,
    width: Constraint,
    align: Alignment,
}

const COLUMNS: [Column; 12] = [
    Column {
        header: "GPU",
        sort: None,
        width: Constraint::Length(3),
        align: Alignment::Right,
    },
    Column {
        header: "PID",
        sort: None,
        width: Constraint::Length(7),
        align: Alignment::Right,
    },
    Column {
        header: "NAME",
        sort: None,
        width: Constraint::Min(14),
        align: Alignment::Left,
    },
    Column {
        header: "TOTAL",
        sort: Some(SortKey::Total),
        width: Constraint::Length(8),
        align: Alignment::Right,
    },
    Column {
        header: "MEM",
        sort: Some(SortKey::Mem),
        width: Constraint::Length(7),
        align: Alignment::Right,
    },
    Column {
        header: "SWAP",
        sort: Some(SortKey::Swap),
        width: Constraint::Length(7),
        align: Alignment::Right,
    },
    Column {
        header: "VRAM",
        sort: Some(SortKey::Vram),
        width: Constraint::Length(7),
        align: Alignment::Right,
    },
    Column {
        header: "GTT",
        sort: Some(SortKey::Gtt),
        width: Constraint::Length(7),
        align: Alignment::Right,
    },
    Column {
        header: "GFX%",
        sort: Some(SortKey::Gfx),
        width: Constraint::Length(5),
        align: Alignment::Right,
    },
    Column {
        header: "COMP%",
        sort: Some(SortKey::Compute),
        width: Constraint::Length(6),
        align: Alignment::Right,
    },
    Column {
        header: "DMA%",
        sort: Some(SortKey::Dma),
        width: Constraint::Length(5),
        align: Alignment::Right,
    },
    Column {
        header: "CPU%",
        sort: Some(SortKey::Cpu),
        width: Constraint::Length(5),
        align: Alignment::Right,
    },
];

/// Columns shown on a wide terminal.
const WIDE: &[usize] = &[0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11];
/// Medium terminals: drop the per-engine columns, keep CPU%.
const MEDIUM: &[usize] = &[0, 1, 2, 3, 4, 5, 6, 7, 8, 11];
/// Narrow terminals keep identity + memory only.
const NARROW: &[usize] = &[1, 2, 3, 4, 5, 6, 7];

/// Columns that fit the terminal. The active sort column is always kept so the
/// list never looks sorted by an invisible key.
fn visible_columns(width: u16, sort: Option<SortKey>) -> Vec<usize> {
    let mut columns = if width >= 104 {
        WIDE.to_vec()
    } else if width >= 88 {
        MEDIUM.to_vec()
    } else {
        NARROW.to_vec()
    };
    if let Some(key) = sort
        && let Some(index) = COLUMNS.iter().position(|column| column.sort == Some(key))
        && !columns.contains(&index)
    {
        columns.push(index);
        columns.sort_unstable();
    }
    columns
}

pub(super) fn draw(f: &mut Frame, area: Rect, app: &mut App) {
    let columns = visible_columns(area.width, app.proc_sort);
    let block = process_block(app, area.width);
    let inner = block.inner(area);
    f.render_widget(block, area);

    if app.is_collapsed(Section::Processes) {
        f.render_widget(
            Paragraph::new(Line::from(Span::styled(
                format!(" {} processes ", app.proc_rows.len()),
                Style::default().fg(app.theme.graph_text()),
            ))),
            inner,
        );
        return;
    }

    if app.proc_rows.is_empty() {
        f.render_widget(
            Paragraph::new(Line::from(Span::styled(
                " no processes ",
                Style::default().fg(app.theme.graph_text()),
            ))),
            inner,
        );
        return;
    }

    let header = header_row(app, &columns);
    let rows: Vec<Row> = app
        .proc_rows
        .iter()
        .map(|row| {
            // GPU clients are tinted by engine activity; CPU-only processes
            // keep a neutral tint so a big MEM+SWAP row stays readable.
            let color = if row.has_gpu() {
                app.theme
                    .process()
                    .sample(row.gpu_activity() as f64 / 100.0)
            } else {
                app.theme.proc_misc()
            };
            let cells = columns
                .iter()
                .map(|column| cell_for(row, *column, app))
                .collect::<Vec<_>>();
            Row::new(cells).style(Style::default().fg(color))
        })
        .collect();

    let widths = columns.iter().map(|column| COLUMNS[*column].width);
    // Detail pane sits directly under the table, full width, only while open.
    // Its height follows the content so a wrapped command line fits.
    let detail_lines = app
        .proc_detail
        .as_ref()
        .map(|detail| detail::lines(app, detail, inner.width));
    let detail_height = detail_lines.as_ref().map_or(0, |lines| {
        ((lines.len() + 1) as u16).min(inner.height.saturating_sub(3))
    });
    let parts =
        Layout::vertical([Constraint::Min(3), Constraint::Length(detail_height)]).split(inner);
    let table_area = parts[0];

    let table = Table::new(rows, widths)
        .header(header)
        .column_spacing(1)
        .row_highlight_style(
            Style::default()
                .bg(app.theme.selected_bg())
                .fg(app.theme.selected_fg())
                .add_modifier(Modifier::BOLD),
        );

    f.render_stateful_widget(table, table_area, &mut app.proc_state);

    if detail_height > 0
        && let Some(lines) = detail_lines
        && let Some(detail) = app.proc_detail.as_ref()
    {
        detail::draw(f, parts[1], app, detail, lines);
    }
}

/// Box title carries the current sort column, direction and filter, plus the
/// key hints — the panel is self-describing instead of relying on the footer.
fn process_block(app: &App, width: u16) -> Block<'static> {
    let sort = match app.proc_sort {
        Some(key) => format!("{}{}", key.label(), SORT_ARROW),
        None => "none".to_string(),
    };
    if let Some(notice) = app.notice() {
        return section_block(app, Section::Processes, "PROCESSES", SectionBox::Proc).title_top(
            Line::from(Span::styled(
                format!(" {notice} "),
                Style::default()
                    .fg(app.theme.hi_fg())
                    .add_modifier(Modifier::BOLD),
            ))
            .alignment(Alignment::Right),
        );
    }
    let scope = if app.proc_all { "all" } else { "gpu" };
    // Only advertise the keys while the panel is focused; the arrows do
    // nothing from other sections.
    // Keep the border short: the footer carries the key hints while this
    // section is focused, and long titles get clipped by the border corners.
    let hints = if app.section == Section::Processes {
        // The sort is a mode; say how to leave it while it is active.
        if app.proc_sort.is_some() {
            " · Esc clear "
        } else {
            ""
        }
    } else if width < 96 {
        ""
    } else {
        " · tab to focus "
    };
    let info = format!(
        " sort {} · {} {}{}",
        sort,
        scope,
        app.proc_rows.len(),
        hints
    );

    section_block(app, Section::Processes, "PROCESSES", SectionBox::Proc).title_top(
        Line::from(Span::styled(
            info,
            Style::default().fg(app.theme.proc_misc()),
        ))
        .alignment(Alignment::Right),
    )
}

fn header_row(app: &App, columns: &[usize]) -> Row<'static> {
    let base = Style::default()
        .fg(app.theme.proc_misc())
        .add_modifier(Modifier::BOLD);
    // The active column is the cursor: inverted so it reads as a selection.
    let active = Style::default()
        .fg(app.theme.selected_fg())
        .bg(app.theme.selected_bg())
        .add_modifier(Modifier::BOLD);

    let cells = columns.iter().map(|column| {
        let spec = &COLUMNS[*column];
        let is_active = spec.sort.is_some() && spec.sort == app.proc_sort;
        let label = if is_active {
            format!("{}{SORT_ARROW}", spec.header)
        } else {
            spec.header.to_string()
        };
        let style = if is_active { active } else { base };
        Cell::from(Line::from(label).alignment(spec.align)).style(style)
    });

    Row::new(cells)
}

fn cell_for(row: &ProcRow, column: usize, app: &App) -> Cell<'static> {
    // Placeholders are dimmed so real numbers stand out.
    let dim = Style::default().fg(app.theme.inactive_fg());
    match column {
        0 => match row.gpu_index() {
            Some(index) => right(index.to_string()),
            None => dimmed("-".to_string(), dim),
        },
        1 => right(row.pid.to_string()),
        2 => Cell::from(row.name.chars().take(24).collect::<String>()),
        3 => right(fmt_kib(Some(row.total_kb()))),
        4 => right(fmt_kib(Some(row.mem_kb))),
        5 => {
            let text = fmt_opt_kib(row.swap_kb);
            if row.swap_kb == 0 {
                dimmed(text, dim)
            } else {
                right(text)
            }
        }
        6 => match row.vram_kb() {
            Some(kb) => right(fmt_kib(Some(kb))),
            None => dimmed("-".to_string(), dim),
        },
        7 => match row.gtt_kb() {
            Some(kb) => right(fmt_kib(Some(kb))),
            None => dimmed("-".to_string(), dim),
        },
        8 => right(row.gfx().to_string()),
        9 => right(row.compute().to_string()),
        10 => right(row.dma().to_string()),
        _ => {
            let percent = row.cpu();
            let text = fmt_cpu(percent);
            if percent.is_some() {
                right(text)
            } else {
                dimmed(text, dim)
            }
        }
    }
}

/// CPU column: whole percent of one core (already summed over every thread),
/// or `-` while the process has no second sample to compare against.
fn fmt_cpu(percent: Option<f32>) -> String {
    match percent {
        Some(percent) => format!("{percent:.0}"),
        None => "-".to_string(),
    }
}

fn dimmed(text: String, style: Style) -> Cell<'static> {
    Cell::from(Line::from(text).alignment(Alignment::Right)).style(style)
}

fn right(text: String) -> Cell<'static> {
    Cell::from(Line::from(text).alignment(Alignment::Right))
}

/// Sorting is always descending (biggest first), so a single arrow is enough.
const SORT_ARROW: &str = "▼";

pub(super) fn fmt_kib(kib: Option<u64>) -> String {
    const KIB_PER_MIB: u64 = 1 << 10;
    const KIB_PER_GIB: u64 = 1 << 20;

    let Some(kib) = kib else {
        return "-".to_string();
    };
    if kib >= KIB_PER_GIB {
        format!("{:.1}G", kib as f64 / KIB_PER_GIB as f64)
    } else {
        format!("{}M", kib / KIB_PER_MIB)
    }
}

/// Like `fmt_kib`, but an exact zero renders as `-` (e.g. no swap used).
fn fmt_opt_kib(kib: u64) -> String {
    if kib == 0 {
        "-".to_string()
    } else {
        fmt_kib(Some(kib))
    }
}

#[cfg(test)]
mod tests {
    use super::{fmt_cpu, fmt_kib, fmt_opt_kib};

    #[test]
    fn cpu_column_shows_a_dash_until_a_second_sample_exists() {
        assert_eq!(fmt_cpu(None), "-");
        assert_eq!(fmt_cpu(Some(0.0)), "0");
        assert_eq!(fmt_cpu(Some(0.4)), "0");
        assert_eq!(fmt_cpu(Some(1_501.4)), "1501");
    }

    #[test]
    fn memory_columns_use_compact_binary_units() {
        assert_eq!(fmt_kib(Some(512 << 10)), "512M");
        assert_eq!(fmt_kib(Some(3 << 20)), "3.0G");
        assert_eq!(fmt_kib(None), "-");
    }

    #[test]
    fn zero_optional_columns_render_as_a_dash() {
        assert_eq!(fmt_opt_kib(0), "-");
        assert_eq!(fmt_opt_kib(2 << 10), "2M");
    }
}
