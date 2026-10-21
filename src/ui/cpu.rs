use ratatui::Frame;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

use crate::app::{App, Section};
use crate::gauge::{self, Kind};
use crate::theme::{SectionBox, UtilKind};

use super::{render_bar, section_block};

const CPU_VALUE_WIDTH: usize = 15;

pub(super) fn draw(f: &mut Frame, area: Rect, app: &App) {
    let title = format!("CPU  {}", short_model(&app.cpu_model));

    // Frequency/temp/power/load ride along in the box title instead of
    // spending a whole content row on them.
    let cpu_sensors = app
        .active_apps()
        .find(|app| app.device_info.is_apu)
        .and_then(|app| app.stat.sensors.as_ref());
    let tctl = cpu_sensors.and_then(|sensors| sensors.tctl);
    let freq = cpu_sensors
        .and_then(|sensors| {
            sensors
                .all_cpu_core_freq_info
                .iter()
                .map(|core| core.cur)
                .max()
        })
        .unwrap_or(0);
    let package_power = cpu_sensors
        .and_then(|sensors| sensors.average_power.as_ref())
        .map(|power| power.value);
    let stats = Line::from(vec![
        Span::styled(
            format!(" {:.2} GHz ", f64::from(freq) / 1000.0),
            Style::default().fg(app.theme.clock()),
        ),
        Span::styled(
            format!(
                " {} ",
                tctl.map_or_else(|| "—".into(), |temp| format!("{}°C", temp / 1000))
            ),
            Style::default().fg(app.theme.temp().sample(0.6)),
        ),
        Span::styled(
            format!(
                " {} ",
                package_power.map_or_else(|| "—".into(), |watts| format!("{watts}W"))
            ),
            Style::default().fg(app.theme.power()),
        ),
        Span::styled(
            format!(
                " load {:.2} {:.2} {:.2} ",
                app.mem.load1, app.mem.load5, app.mem.load15
            ),
            Style::default().fg(app.theme.graph_text()),
        ),
    ]);

    let block = section_block(app, Section::Cpu, &title, SectionBox::Cpu).title_top(stats);
    let inner = block.inner(area);
    f.render_widget(block, area);

    if app.is_collapsed(Section::Cpu) {
        let line = Line::from(vec![
            Span::styled(
                format!(" {:>3.0}% ", app.cpu.cpu_percent.round()),
                Style::default().fg(app.theme.util_color(app.cpu.cpu_percent, UtilKind::Cpu)),
            ),
            Span::styled(
                format!(
                    "load {:.2} {:.2} {:.2}  ",
                    app.mem.load1, app.mem.load5, app.mem.load15
                ),
                Style::default().fg(app.theme.graph_text()),
            ),
            Span::styled(
                format!(
                    "MEM {:.1}G/{:.0}G  SWP {:.1}G/{:.0}G",
                    app.mem.mem_used_gb(),
                    app.mem.mem_total_gb(),
                    app.mem.swap_used_gb(),
                    app.mem.swap_total_gb()
                ),
                Style::default().fg(app.theme.proc_misc()),
            ),
        ]);
        f.render_widget(Paragraph::new(line), inner);
        return;
    }

    let w = inner.width as usize;
    let rl = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1), // CPU bar
            Constraint::Length(1), // MEM + SWP side by side
            Constraint::Min(1),    // per-core grid
        ])
        .split(inner);

    // CPU aggregate bar (reserve same value field as MEM/SWP so tracks align)
    f.render_widget(
        Paragraph::new(render_bar(
            app,
            gauge::Bar::new("CPU", Some(app.cpu.cpu_percent), w, Kind::Cpu)
                .with_value("", CPU_VALUE_WIDTH),
        )),
        rl[0],
    );

    // MEM + SWP share one row: half a bar each is still plenty to see a trend,
    // and it buys a row for the process table.
    let halves = Layout::default()
        .direction(Direction::Horizontal)
        .spacing(2)
        .constraints([Constraint::Ratio(1, 2), Constraint::Ratio(1, 2)])
        .split(rl[1]);
    let mem_val = format!(
        "{} / {}",
        fmt_gb(app.mem.mem_used_gb()),
        fmt_gb(app.mem.mem_total_gb())
    );
    f.render_widget(
        Paragraph::new(render_bar(
            app,
            gauge::Bar::new(
                "MEM",
                Some(app.mem.mem_used_pct()),
                halves[0].width as usize,
                Kind::Mem,
            )
            .with_value(&mem_val, CPU_VALUE_WIDTH),
        )),
        halves[0],
    );
    let swp_val = format!(
        "{} / {}",
        fmt_gb(app.mem.swap_used_gb()),
        fmt_gb(app.mem.swap_total_gb())
    );
    f.render_widget(
        Paragraph::new(render_bar(
            app,
            gauge::Bar::new(
                "SWP",
                Some(app.mem.swap_used_pct()),
                halves[1].width as usize,
                Kind::Mem,
            )
            .with_value(&swp_val, CPU_VALUE_WIDTH),
        )),
        halves[1],
    );

    // per-core grid (btop-style)
    draw_core_grid(f, rl[2], app, inner.width);
}

/// Grid dimensions. A core cell needs about 24 columns before the bar gets
/// too short, so wide terminals use more columns and fewer rows instead of
/// always spending eight rows on the core list.
pub(super) fn grid_dimensions(n: usize, width: u16) -> (usize, usize) {
    if n == 0 {
        return (1, 1);
    }
    // ~21 columns per core cell (label + bar + percent) before the bar gets
    // too short to read.
    let max_cols = ((width as usize) / 21).max(1);
    let cols = n.div_ceil(4).clamp(1, max_cols);
    let rows = n.div_ceil(cols);
    (cols, rows)
}

fn draw_core_grid(f: &mut Frame, area: Rect, app: &App, width: u16) {
    let n = app.cpu.per_core_percent.len();
    if n == 0 || area.height == 0 {
        return;
    }
    let (cols, rows) = grid_dimensions(n, width);
    let col_areas = Layout::default()
        .direction(Direction::Horizontal)
        .spacing(2) // gutter between core columns
        .constraints(
            (0..cols)
                .map(|_| Constraint::Ratio(1, cols as u32))
                .collect::<Vec<_>>(),
        )
        .split(area);

    for c in 0..cols {
        let row_areas = Layout::default()
            .direction(Direction::Vertical)
            .constraints((0..rows).map(|_| Constraint::Length(1)).collect::<Vec<_>>())
            .split(col_areas[c]);
        for r in 0..rows {
            let idx = c * rows + r; // column-major like btop
            if idx >= n {
                continue;
            }
            let pct = app.cpu.per_core_percent[idx];
            let label = format!("C{idx}");
            let pct_s = format!("{pct:>3.0}%");
            let cell_w = col_areas[c].width as usize;
            // reserved: label field(4) + space + pct(4)
            let graph_w = cell_w.saturating_sub(9).max(1);
            let mut spans: Vec<Span<'static>> = vec![Span::styled(
                format!("{label:<3} "),
                Style::default().fg(app.theme.graph_text()),
            )];
            if let Some(g) = app.hist_cores[idx]
                .braille_graph(graph_w, 1, app.theme.cpu())
                .into_iter()
                .next()
            {
                spans.extend(g.spans);
            }
            spans.push(Span::styled(
                format!(" {pct_s}"),
                Style::default().fg(app.theme.util_color(pct, UtilKind::Cpu)),
            ));
            f.render_widget(Paragraph::new(Line::from(spans)), row_areas[r]);
        }
    }
}

fn fmt_gb(gb: f64) -> String {
    format!("{gb:.1}G")
}

fn short_model(s: &str) -> String {
    let s = s.split(" w/").next().unwrap_or(s);
    s.replace("AMD ", "")
        .replace("Processor", "")
        .trim()
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::{fmt_gb, grid_dimensions, short_model};

    #[test]
    fn core_grid_uses_more_columns_when_the_terminal_is_wide() {
        assert_eq!(grid_dimensions(0, 200), (1, 1));
        // narrow: never more than ~8 rows
        assert_eq!(grid_dimensions(8, 80), (2, 4));
        assert_eq!(grid_dimensions(32, 100), (4, 8));
        // wide: 8 columns for 32 cores, four rows
        assert_eq!(grid_dimensions(32, 200), (8, 4));
        // very wide: more columns, still at most eight rows
        let (cols, rows) = grid_dimensions(64, 200);
        assert!(cols >= 8, "expected >= 8 columns, got {cols}");
        assert!(rows <= 8, "expected <= 8 rows, got {rows}");
    }

    #[test]
    fn gigabyte_formatting_uses_one_decimal_place() {
        assert_eq!(fmt_gb(1.25), "1.2G");
    }

    #[test]
    fn cpu_names_are_shortened_for_the_layout() {
        assert_eq!(
            short_model("AMD Ryzen AI MAX+ 395 w/ Radeon Graphics"),
            "Ryzen AI MAX+ 395"
        );
    }
}
