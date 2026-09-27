//! Drawing. Uses the terminal's own 16-color palette so it fits light and dark themes.

use ratatui::layout::{Constraint, Flex, Layout, Rect};
use ratatui::style::{Color, Modifier, Style, Stylize};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Block, Cell, Clear, List, ListItem, Paragraph, Row, Table, Tabs, Wrap};
use ratatui::Frame;

use super::ansi;
use super::app::{App, Category, DiffMode, Focus, Gens, RepoState, Sort, TabKind};
use super::detail::Side;
use super::loader::JobState;
use crate::gc;
use crate::git::Link;
use crate::range::back_label;
use crate::render::{plain_versions, render_bytes};
use crate::sources::Generation;
use crate::store_path::{name, parse_name};

const LIST_WIDTH: u16 = 52;
/// Extra list width for the commit column.
const LINK_WIDTH: u16 = 10;

fn category_color(category: Category) -> Color {
    match category {
        Category::Upgraded => Color::Cyan,
        Category::Downgraded => Color::Yellow,
        Category::Changed | Category::Selection => Color::Magenta,
        Category::Added => Color::Green,
        Category::Removed => Color::Red,
        Category::Rebuilt => Color::DarkGray,
    }
}

fn pane_block(title: Line<'_>, focused: bool) -> Block<'_> {
    let block = Block::bordered().title(title);
    if focused {
        block.border_style(Style::new().fg(Color::Blue))
    } else {
        block.border_style(Style::new().fg(Color::DarkGray))
    }
}

pub fn draw(f: &mut Frame, app: &mut App) {
    let [tabs_area, main, status] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Fill(1),
        Constraint::Length(1),
    ])
    .areas(f.area());
    let has_links = app
        .tab()
        .generations()
        .iter()
        .any(|g| app.link(g).is_some());
    let list_width = (LIST_WIDTH + if has_links { LINK_WIDTH } else { 0 }).min(main.width / 2);
    let [list_area, diff_area] =
        Layout::horizontal([Constraint::Length(list_width), Constraint::Fill(1)]).areas(main);

    draw_tabs(f, app, tabs_area);
    draw_list(f, app, list_area);
    draw_diff(f, app, diff_area);
    draw_status(f, app, status);
    if app.detail.is_some() {
        draw_detail(f, app);
    }
    if app.delete_confirm.is_some() {
        draw_delete_confirm(f, app);
    }
    if app.gc_confirm {
        draw_gc_confirm(f);
    }
    if app.show_help {
        draw_help(f);
    }
}

fn draw_gc_confirm(f: &mut Frame) {
    let lines = vec![
        Line::from("Collect garbage?").bold(),
        Line::raw(""),
        Line::from(vec![
            Span::raw("Runs: "),
            Span::styled(gc::command_line().join(" "), Style::new().bold()),
        ]),
        Line::from("Deletes every store path no GC root uses; they must be downloaded or rebuilt"),
        Line::from("if needed again. The space freed is shown afterwards, and covers all such"),
        Line::from("garbage, not only deleted generations."),
        Line::raw(""),
        Line::from(vec![
            Span::styled("y", Style::new().bold()),
            Span::raw(" collects, any other key cancels."),
        ]),
    ];
    let paragraph = Paragraph::new(lines).wrap(Wrap { trim: false });
    let [area] = Layout::horizontal([Constraint::Length(84)])
        .flex(Flex::Center)
        .areas(f.area());
    let rows = paragraph.line_count(area.width.saturating_sub(2));
    let height = u16::try_from(rows + 2).unwrap_or(u16::MAX);
    let [area] = Layout::vertical([Constraint::Length(height)])
        .flex(Flex::Center)
        .areas(area);
    let block = Block::bordered()
        .title(" Garbage collection ")
        .border_style(Style::new().fg(Color::Yellow));
    f.render_widget(Clear, area);
    f.render_widget(paragraph.block(block), area);
}

fn draw_delete_confirm(f: &mut Frame, app: &App) {
    let Some(c) = &app.delete_confirm else {
        return;
    };
    let g = &c.generation;
    let tab = &app.tabs[c.tab];
    let number = g.number.to_string();
    let mut lines = vec![
        Line::from(vec![
            Span::raw("Delete "),
            Span::styled(
                format!("{} generation {number}", tab.title),
                Style::new().fg(Color::Red).bold(),
            ),
            Span::raw(format!(
                "  ({}, {})?",
                g.created_label(),
                describe(g, tab.kind)
            )),
        ]),
        Line::from(g.store_path.clone()).fg(Color::DarkGray),
        Line::raw(""),
        Line::from(vec![
            Span::raw("Runs: "),
            Span::styled(c.command.clone(), Style::new().bold()),
        ]),
        Line::from(
            "Only the profile link is removed; you'll be offered garbage collection to free the space.",
        ),
    ];
    lines.extend(
        c.warnings
            .iter()
            .map(|w| Line::from(format!("Note: {w}")).fg(Color::Yellow)),
    );
    lines.push(Line::raw(""));
    lines.push(Line::from(vec![
        Span::raw("Type "),
        Span::styled(number, Style::new().bold()),
        Span::raw(" and press enter to delete; esc cancels."),
    ]));
    lines.push(Line::from(vec![
        Span::styled("> ", Style::new().fg(Color::Red).bold()),
        Span::raw(c.input.clone()),
        Span::styled("█", Style::new().fg(Color::Red)),
    ]));

    // Size to the wrapped text, so a long command can't push the prompt out of view.
    let [area] = Layout::horizontal([Constraint::Percentage(80)])
        .flex(Flex::Center)
        .areas(f.area());
    let paragraph = Paragraph::new(lines).wrap(Wrap { trim: false });
    let rows = paragraph.line_count(area.width.saturating_sub(2));
    let height = u16::try_from(rows + 2).unwrap_or(u16::MAX);
    let [area] = Layout::vertical([Constraint::Length(height)])
        .flex(Flex::Center)
        .areas(area);
    let block = Block::bordered()
        .title(" Delete generation ")
        .border_style(Style::new().fg(Color::Red));
    f.render_widget(Clear, area);
    f.render_widget(paragraph.block(block), area);
}

/// A job's output (ANSI colors kept), or its progress / failure as dim text.
fn job_text(job: Option<&JobState>, running: &str) -> Text<'static> {
    match job {
        Some(JobState::Done(out)) => ansi::to_text(out),
        Some(JobState::Failed(e)) => Text::from(e.clone()).fg(Color::Red),
        Some(JobState::Running) | None => Text::from(running.to_owned()).fg(Color::DarkGray),
    }
}

fn draw_tabs(f: &mut Frame, app: &App, area: Rect) {
    let titles = app
        .tabs
        .iter()
        .enumerate()
        .map(|(i, t)| format!("{} {}", i + 1, t.title));
    let tabs = Tabs::new(titles)
        .select(app.active)
        .highlight_style(Style::new().add_modifier(Modifier::REVERSED | Modifier::BOLD))
        .divider(" ");
    f.render_widget(tabs, area);
}

/// Short description for a generation row: the NixOS version for systems, the version in
/// the name for other profiles, else the start of the store hash.
fn describe(g: &Generation, kind: TabKind) -> String {
    if kind == TabKind::Paths {
        return g.path.display().to_string();
    }
    let file = g.store_path.rsplit('/').next().unwrap_or("");
    // `<hash>-nixos-system-<host>-<version>`: hostnames can contain `-<digit>`, which trips
    // up the generic name/version split, but the version never contains `-`.
    if file.contains("-nixos-system-") {
        if let Some((_, version)) = file.rsplit_once('-') {
            return version.to_owned();
        }
    }
    match parse_name(&g.store_path) {
        (_, Some(version)) => version.to_owned(),
        _ => file.chars().take(8).collect(),
    }
}

fn draw_list(f: &mut Frame, app: &mut App, area: Rect) {
    let focused = app.focus == Focus::List;
    let pair = app.tab().pair();
    let links: Vec<Option<String>> = app
        .tab()
        .generations()
        .iter()
        .map(|g| app.link(g).map(Link::label))
        .collect();
    let tab = &mut app.tabs[app.active];
    let block = pane_block(Line::from(" Generations "), focused);
    let gens = match &tab.gens {
        Gens::Ready(gens) => gens,
        Gens::Loading => {
            f.render_widget(Paragraph::new("Loading…").block(block), area);
            return;
        }
        Gens::Failed(e) => {
            let p = Paragraph::new(e.as_str())
                .wrap(Wrap { trim: true })
                .block(block);
            f.render_widget(p, area);
            return;
        }
    };
    let width = gens
        .iter()
        .map(|g| g.number_label().len())
        .max()
        .unwrap_or(0);
    let items: Vec<ListItem> = gens
        .iter()
        .enumerate()
        .map(|(i, g)| {
            let (label, color, pinned) = match pair {
                Some((old, _)) if old == i => ("old", Color::Red, tab.base == Some(i)),
                Some((_, new)) if new == i => ("new", Color::Green, tab.target == Some(i)),
                _ => ("   ", Color::Reset, false),
            };
            let mut marker = Style::new().fg(color).bold();
            if pinned {
                marker = marker.add_modifier(Modifier::UNDERLINED);
            }
            ListItem::new(Line::from(vec![
                Span::styled(label, marker),
                Span::raw(if g.current { " * " } else { "   " }),
                Span::styled(format!("{:>width$}", g.number_label()), Style::new().bold()),
                Span::raw("  "),
                Span::styled(g.created_label(), Style::new().fg(Color::DarkGray)),
                Span::raw("  "),
                Span::raw(describe(g, tab.kind)),
                Span::styled(
                    links[i]
                        .as_ref()
                        .map_or(String::new(), |l| format!("  {l}")),
                    Style::new().fg(Color::Yellow),
                ),
            ]))
        })
        .collect();
    let highlight = if focused {
        Style::new().add_modifier(Modifier::REVERSED)
    } else {
        Style::new().add_modifier(Modifier::BOLD)
    };
    let list = List::new(items).block(block).highlight_style(highlight);
    tab.list_state.select(Some(tab.cursor));
    app.list_height = area.height.saturating_sub(2) as usize;
    f.render_stateful_widget(list, area, &mut tab.list_state);
}

fn message(f: &mut Frame, area: Rect, block: Block, text: String) {
    let p = Paragraph::new(text).wrap(Wrap { trim: true }).block(block);
    f.render_widget(p, area);
}

fn draw_diff(f: &mut Frame, app: &mut App, area: Rect) {
    let focused = app.focus == Focus::Diff;
    let tab = app.tab();
    let Some((old, new)) = app.pair_paths() else {
        let text = match tab.gens {
            Gens::Ready(_) => {
                "No earlier generation to compare with. Pin another as old with space.".to_owned()
            }
            _ => String::new(),
        };
        message(f, area, pane_block(Line::from(" Diff "), focused), text);
        return;
    };
    // The pair as a `:` range, when both sides are at or before the current generation.
    let relative = tab.pair().and_then(|(o, n)| {
        let gens = tab.generations();
        Some(format!(
            " ({}:{})",
            back_label(gens, o)?,
            back_label(gens, n)?
        ))
    });
    let title = Line::from(vec![
        Span::raw(format!(" {} ", tab.title)),
        Span::styled(old.number_label(), Style::new().fg(Color::Red).bold()),
        Span::raw(" → "),
        Span::styled(new.number_label(), Style::new().fg(Color::Green).bold()),
        Span::styled(
            relative.unwrap_or_default(),
            Style::new().fg(Color::DarkGray),
        ),
        Span::raw(match app.diff_mode {
            DiffMode::Packages => " ",
            DiffMode::Nvd => " · nvd ",
            DiffMode::Commits => " · commits ",
        }),
    ]);
    let block = pane_block(title, focused);

    if old.store_path == new.store_path {
        let text = "Old and new are the same closure.".to_owned();
        message(f, area, block, text);
        return;
    }
    if app.diff_mode == DiffMode::Commits {
        let text = commits_text(app, old, new);
        let scroll = app.text_scroll as u16;
        app.diff_height = area.height.saturating_sub(2) as usize;
        f.render_widget(
            Paragraph::new(text)
                .block(block)
                .wrap(Wrap { trim: false })
                .scroll((scroll, 0)),
            area,
        );
        return;
    }
    if app.diff_mode == DiffMode::Nvd {
        let text = job_text(app.nvd_job(), "Running nvd diff…");
        let scroll = app.text_scroll as u16;
        app.diff_height = area.height.saturating_sub(2) as usize;
        f.render_widget(Paragraph::new(text).block(block).scroll((scroll, 0)), area);
        return;
    }
    for g in [old, new] {
        if let Some(e) = app.errors.get(&g.store_path) {
            let text = format!("Couldn't load generation {}: {e}", g.number_label());
            message(f, area, block, text);
            return;
        }
    }
    let Some(current) = app.shown_diff() else {
        message(f, area, block, "Loading closures…".to_owned());
        return;
    };

    let d = &current.diff;
    let mut counts: Vec<Span> = Vec::new();
    for category in Category::ALL {
        let hidden = app.hidden.contains(&category.toggle_key());
        let style = if hidden {
            Style::new()
                .fg(Color::DarkGray)
                .add_modifier(Modifier::CROSSED_OUT)
        } else {
            Style::new().fg(category_color(category))
        };
        counts.push(Span::styled(
            format!("{} {}", current.count(category), category.name()),
            style,
        ));
        counts.push(Span::raw("  "));
    }
    let size_delta = i128::from(d.right_size) - i128::from(d.left_size);
    let mut summary = vec![
        Span::raw(format!(
            "paths {} → {} (+{} −{})  disk ",
            d.left_path_count, d.right_path_count, d.paths_added, d.paths_removed
        )),
        Span::styled(render_bytes(size_delta), Style::new().bold()),
        Span::raw(format!(
            "  sort: {}",
            match app.sort {
                Sort::Name => "name",
                Sort::Size => "size",
            }
        )),
    ];
    if let Some((old, new)) = app.pair_links() {
        summary.push(Span::raw("  commits "));
        summary.push(Span::styled(
            format!("{} → {}", old.label(), new.label()),
            Style::new().fg(Color::Yellow),
        ));
    }
    if !app.filter.is_empty() {
        summary.push(Span::styled(
            format!("  filter: {}", app.filter),
            Style::new().fg(Color::Yellow),
        ));
    }

    let rows = app.visible_rows();
    let pname_width = rows
        .iter()
        .map(|r| r.pname.chars().count())
        .max()
        .unwrap_or(7)
        .clamp(7, 32) as u16;
    let table_rows: Vec<Row> = rows
        .iter()
        .map(|r| {
            let color = category_color(r.category);
            let mut pname = Style::new();
            if r.selection.selected_anywhere() {
                pname = pname.bold();
            }
            let size = if r.size_delta == 0 {
                String::new()
            } else {
                render_bytes(i128::from(r.size_delta))
            };
            Row::new(vec![
                Cell::from(Span::styled(
                    format!("{}{}", r.category.marker(), r.selection.marker()),
                    Style::new().fg(color).bold(),
                )),
                Cell::from(Span::styled(r.pname.clone(), pname)),
                Cell::from(Span::styled(
                    plain_versions(&r.left),
                    Style::new().fg(Color::Red),
                )),
                Cell::from(Span::styled(
                    plain_versions(&r.right),
                    Style::new().fg(Color::Green),
                )),
                Cell::from(Line::from(size).right_aligned()),
            ])
        })
        .collect();
    let empty = table_rows.is_empty();
    let header = Row::new(["", "Package", "Old", "New", "Size"]).style(Style::new().bold());
    let table = Table::new(
        table_rows,
        [
            Constraint::Length(2),
            Constraint::Length(pname_width),
            Constraint::Fill(1),
            Constraint::Fill(1),
            Constraint::Length(10),
        ],
    )
    .header(header)
    .row_highlight_style(if focused {
        Style::new().add_modifier(Modifier::REVERSED)
    } else {
        Style::new()
    });

    let inner = block.inner(area);
    f.render_widget(block, area);
    let [counts_area, summary_area, table_area] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Fill(1),
    ])
    .areas(inner);
    f.render_widget(Paragraph::new(Line::from(counts)), counts_area);
    f.render_widget(Paragraph::new(Line::from(summary)), summary_area);
    if empty {
        let text = if current.rows.is_empty() {
            "No package changes."
        } else {
            "No packages match the filter and category toggles."
        };
        f.render_widget(Paragraph::new(text).fg(Color::DarkGray), table_area);
    } else {
        app.diff_height = table_area.height.saturating_sub(1) as usize;
        f.render_stateful_widget(table, table_area, &mut app.diff_state);
    }
}

fn draw_status(f: &mut Frame, app: &App, area: Rect) {
    let line = if let Some(input) = &app.range_input {
        Line::from(vec![
            Span::styled(":", Style::new().fg(Color::Yellow).bold()),
            Span::raw(input.as_str()),
            Span::styled("█", Style::new().fg(Color::Yellow)),
            Span::styled(
                "  OLD:NEW or OLD (vs current), e.g. -1  -2:-1  40:43   enter: go  esc: cancel",
                Style::new().fg(Color::DarkGray),
            ),
        ])
    } else if let Some(message) = &app.message {
        Line::from(message.as_str()).fg(Color::Red)
    } else if app.editing_filter {
        Line::from(vec![
            Span::styled("/", Style::new().fg(Color::Yellow).bold()),
            Span::raw(app.filter.as_str()),
            Span::styled("█", Style::new().fg(Color::Yellow)),
            Span::styled(
                "  enter: keep  esc: clear",
                Style::new().fg(Color::DarkGray),
            ),
        ])
    } else {
        let key = |k: &'static str| Span::styled(k, Style::new().bold());
        let hint = |h: &'static str| Span::styled(h, Style::new().fg(Color::DarkGray));
        // Most useful first: narrow terminals cut the end off.
        Line::from(vec![
            key("?"),
            hint(" help  "),
            key(":"),
            hint(" range  "),
            key("space"),
            hint(" pin old  "),
            key("enter"),
            hint(if app.focus == Focus::Diff {
                " details  "
            } else {
                " pin new  "
            }),
            key("esc"),
            hint(" unpin  "),
            key("tab"),
            hint(" pane  "),
            key("/"),
            hint(" filter  "),
            key("s"),
            hint(" sort  "),
            key("n"),
            hint(" nvd  "),
            key("L"),
            hint(" commits  "),
            key("udcarb"),
            hint(" toggle  "),
            key("q"),
            hint(" quit"),
        ])
    };
    f.render_widget(Paragraph::new(line), area);
}

/// The commits view: which commit each side came from, then `git log` between them.
fn commits_text(app: &App, old: &Generation, new: &Generation) -> Text<'static> {
    let dim = |s: &str| Text::from(s.to_owned()).fg(Color::DarkGray);
    if app.tab().kind == TabKind::Paths {
        return dim("Ad-hoc paths aren't linked to commits.");
    }
    match &app.repo {
        RepoState::Unconfigured => {
            return dim(
                "No configuration repo. Start nixi with --repo PATH (or set NIXI_REPO) \
                 to link generations to commits.",
            )
        }
        RepoState::Loading => return dim("Reading git history…"),
        RepoState::Failed(e) => return Text::from(e.clone()).fg(Color::Red),
        RepoState::Ready { .. } => {}
    }
    let (Some(old_link), Some(new_link)) = (app.link(old), app.link(new)) else {
        return dim("No commit was made before one of these generations.");
    };
    let width = old.number_label().len().max(new.number_label().len());
    let side = |heading: &str, color: Color, g: &Generation, link: &Link| {
        Line::from(vec![
            Span::styled(
                format!("{heading} {:>width$}", g.number_label()),
                Style::new().fg(color).bold(),
            ),
            Span::raw("  "),
            Span::styled(link.label(), Style::new().fg(Color::Yellow)),
            Span::raw(format!(" {}", link.commit.subject)),
        ])
    };
    let mut lines = vec![
        side("old", Color::Red, old, old_link),
        side("new", Color::Green, new, new_link),
        Line::from("= recorded   ≈ newest commit before, same nixpkgs   ? newest commit before")
            .fg(Color::DarkGray),
    ];
    if old_link.commit.hash == new_link.commit.hash {
        lines.push(Line::raw(""));
        lines.push(Line::from(
            "Both come from the same commit: the difference is from outside the repo's \
             history (uncommitted edits, or other inputs).",
        ));
        return Text::from(lines);
    }
    if old_link.commit.time > new_link.commit.time {
        lines.push(Line::from("Going back in time: these commits are undone.").fg(Color::Yellow));
    }
    lines.push(Line::raw(""));
    lines.extend(job_text(app.commits_job(), "Running git log…").lines);
    Text::from(lines)
}

/// A rectangle of the given percentage size, centered in `area`.
fn centered(area: Rect, width: u16, height: u16) -> Rect {
    let [area] = Layout::horizontal([Constraint::Percentage(width)])
        .flex(Flex::Center)
        .areas(area);
    let [area] = Layout::vertical([Constraint::Percentage(height)])
        .flex(Flex::Center)
        .areas(area);
    area
}

fn draw_detail(f: &mut Frame, app: &mut App) {
    let Some(d) = &app.detail else {
        return;
    };
    let version_width = d
        .paths
        .iter()
        .map(|p| p.version.to_string().chars().count())
        .max()
        .unwrap_or(0)
        .clamp(8, 40);
    let row = &d.row;
    let color = category_color(row.category);
    let (old_selected, new_selected) = d.selected_on();
    let yes_no = |b: bool| if b { "yes" } else { "no" };
    let mut lines = vec![
        Line::from(vec![
            Span::styled(
                format!("{}{} ", row.category.marker(), row.selection.marker()),
                Style::new().fg(color).bold(),
            ),
            Span::styled(row.category.name(), Style::new().fg(color)),
            Span::raw(format!(
                "   size {}   directly selected: old {}, new {}",
                render_bytes(i128::from(row.size_delta)),
                yes_no(old_selected),
                yes_no(new_selected),
            )),
        ]),
        Line::raw(""),
    ];
    for (side, label, heading, heading_color) in [
        (Side::Old, &d.old_label, "Old", Color::Red),
        (Side::New, &d.new_label, "New", Color::Green),
    ] {
        lines.push(Line::styled(
            format!("{heading} — {label}"),
            Style::new().fg(heading_color).bold(),
        ));
        let mut present = false;
        for (i, p) in d.paths.iter().enumerate().filter(|(_, p)| p.side == side) {
            present = true;
            let size = render_bytes(i128::from(p.nar_size));
            let line = Line::from(vec![
                Span::raw(if i == d.cursor { "▸ " } else { "  " }),
                Span::styled(
                    format!("{:<version_width$}", p.version.to_string()),
                    Style::new().fg(Color::Yellow),
                ),
                Span::raw(format!(" {:>9}  ", size.trim_start_matches('+'))),
                Span::raw(p.path.clone()),
            ]);
            lines.push(if i == d.cursor {
                line.style(Style::new().add_modifier(Modifier::REVERSED))
            } else {
                line
            });
        }
        if !present {
            lines.push(Line::from("  (not in this closure)").fg(Color::DarkGray));
        }
    }

    lines.push(Line::raw(""));
    let side = match d.referrers_side {
        Side::Old => "old",
        Side::New => "new",
    };
    lines.push(Line::styled(
        format!("Directly required by ({}, {side} side)", d.referrers.len()),
        Style::new().bold(),
    ));
    lines.push(if d.referrers.is_empty() {
        Line::from("  nothing: it's the root").fg(Color::DarkGray)
    } else {
        Line::from(format!("  {}", d.referrers.join(", ")))
    });

    lines.push(Line::raw(""));
    let why_line = lines.len() as u16;
    match &d.why {
        None => lines.push(
            Line::from("Press w to run nix why-depends on the selected path.").fg(Color::DarkGray),
        ),
        Some((root, path)) => {
            lines.push(Line::styled(
                format!("nix why-depends {} {}", name(root), name(path)),
                Style::new().bold(),
            ));
            lines.extend(job_text(app.why_job(), "Running…").lines);
        }
    }

    let block = Block::bordered()
        .title(format!(" {} ", row.pname))
        .title_bottom(
            Line::from(" j/k select path · w why-depends · PgUp/PgDn scroll · esc close ")
                .right_aligned(),
        )
        .border_style(Style::new().fg(Color::Blue));
    let Some(d) = &mut app.detail else {
        return;
    };
    if d.jump_to_why {
        // Approximate when earlier lines wrap; PgUp/PgDn cover the rest.
        d.scroll = why_line;
        d.jump_to_why = false;
    }
    let area = centered(f.area(), 90, 80);
    f.render_widget(Clear, area);
    f.render_widget(
        Paragraph::new(lines)
            .block(block)
            .wrap(Wrap { trim: false })
            .scroll((d.scroll, 0)),
        area,
    );
}

fn draw_help(f: &mut Frame) {
    // One line per key, so it fits a 20-row terminal.
    let lines = [
        ("j/k ↑/↓", "move; g/G top/bottom; PgUp/PgDn page"),
        ("tab h/l ←/→", "switch pane; 1-9 switch tab"),
        (
            ":",
            "compare a range: -1 (= -1:0), -2:-1, 40:43 (pins both)",
        ),
        ("D", "delete the highlighted generation (asks to confirm)"),
        ("C", "collect garbage and show the space freed (asks first)"),
        (
            "space / enter",
            "pin old / new to the highlighted generation",
        ),
        ("esc", "clear the filter, else unpin both sides"),
        (
            "enter (diff)",
            "package details: store paths, what requires it",
        ),
        ("w", "why-depends for the selected package"),
        ("n", "nvd's own output instead of the table"),
        ("L", "config commits between the two (needs --repo)"),
        ("/", "filter packages by name"),
        ("s", "sort by name or by size change"),
        (
            "u d c a r b",
            "show/hide upgraded, downgraded, changed, added,",
        ),
        ("", "removed, rebuilt (rebuilt hidden by default)"),
        ("q ctrl-c", "quit"),
    ];
    let text: Vec<Line> = lines
        .iter()
        .map(|(k, v)| {
            Line::from(vec![
                Span::styled(format!("{k:>14}  "), Style::new().bold()),
                Span::raw(*v),
            ])
        })
        .collect();
    let [area] = Layout::horizontal([Constraint::Length(74)])
        .flex(Flex::Center)
        .areas(f.area());
    let [area] = Layout::vertical([Constraint::Length(text.len() as u16 + 2)])
        .flex(Flex::Center)
        .areas(area);
    f.render_widget(Clear, area);
    f.render_widget(
        Paragraph::new(text).block(Block::bordered().title(" Keys (any key closes) ")),
        area,
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::app::tests::app;
    use crate::ui::loader::{JobKey, Msg};
    use ratatui::backend::TestBackend;
    use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use ratatui::Terminal;

    fn screen(app: &mut App) -> String {
        let mut terminal = Terminal::new(TestBackend::new(140, 20)).unwrap();
        terminal.draw(|f| draw(f, app)).unwrap();
        let buffer = terminal.backend().buffer();
        (0..buffer.area.height)
            .map(|y| {
                (0..buffer.area.width)
                    .map(|x| buffer[(x, y)].symbol())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn draws_generations_and_diff() {
        let mut app = app();
        let s = screen(&mut app);
        assert!(s.contains("1 system"), "{s}");
        assert!(s.contains("2 home (bob)"), "{s}");
        assert!(s.contains("old   42"), "{s}");
        assert!(s.contains("new * 43"), "{s}");
        assert!(s.contains("system 42 → 43 (-1:0)"), "{s}");
        assert!(s.contains("1 upgraded"), "{s}");
        // The fixture roots reference every package, so all of them count as selected.
        assert!(s.contains("U* firefox"), "{s}");
        assert!(s.contains("141.0.2"), "{s}");
        assert!(s.contains("R- htop"), "{s}");
        assert!(s.contains("-20.5KiB"), "{s}");
    }

    #[test]
    fn draws_detail_popup() {
        let mut app = app();
        app.focus = Focus::Diff;
        app.on_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        let s = screen(&mut app);
        assert!(s.contains(" firefox "), "{s}");
        assert!(s.contains("Old — system 42"), "{s}");
        assert!(s.contains("▸ 141.0.2"), "{s}");
        assert!(s.contains("Directly required by (1, new side)"), "{s}");
        assert!(s.contains("nixos-system"), "{s}");
        assert!(s.contains("Press w"), "{s}");
    }

    #[test]
    fn draws_nvd_output() {
        let mut app = app();
        app.on_key(KeyEvent::new(KeyCode::Char('n'), KeyModifiers::NONE));
        assert!(screen(&mut app).contains("Running nvd diff…"));
        let (old, new) = app
            .pair_paths()
            .map(|(o, n)| (o.store_path.clone(), n.store_path.clone()))
            .unwrap();
        app.on_msg(Msg::Job {
            key: JobKey::Nvd { old, new },
            result: Ok("\x1b[1mVersion changes:\x1b[0m\n[U*]  #1  firefox".into()),
        });
        let s = screen(&mut app);
        assert!(s.contains("system 42 → 43 (-1:0) · nvd"), "{s}");
        assert!(s.contains("Version changes:"), "{s}");
        assert!(s.contains("[U*]  #1  firefox"), "{s}");
    }

    #[test]
    fn draws_commits_view() {
        use crate::git::{Commit, Confidence, Link, Repo};
        use crate::ui::app::RepoState;
        use std::collections::HashMap;
        use std::sync::Arc;

        let mut app = app();
        app.on_key(KeyEvent::new(KeyCode::Char('L'), KeyModifiers::NONE));
        assert!(screen(&mut app).contains("No configuration repo"));

        let commit = |hash: &str, time, subject: &str| Commit {
            hash: hash.into(),
            time,
            subject: subject.into(),
            nixpkgs: None,
        };
        let (a, b) = (
            commit("aaaaaaa111", 100, "Flake Update"),
            commit("bbbbbbb222", 200, "Add ripgrep"),
        );
        let link = |c: &Commit| Link {
            commit: c.clone(),
            confidence: Confidence::Likely,
        };
        let links = HashMap::from([(41, link(&a)), (42, link(&a)), (43, link(&b))]);
        app.repo = RepoState::Ready {
            repo: Arc::new(Repo {
                path: "/nonexistent".into(),
                commits: vec![b.clone(), a.clone()],
            }),
            links: HashMap::from([(0, links)]),
        };
        app.sync();
        let s = screen(&mut app);
        assert!(s.contains("system 42 → 43 (-1:0) · commits"), "{s}");
        assert!(s.contains("old 42  ≈aaaaaaa Flake Update"), "{s}");
        assert!(s.contains("new 43  ≈bbbbbbb Add ripgrep"), "{s}");
        assert!(s.contains("≈aaaaaaa"), "list shows links: {s}");

        // 41 and 42 share a commit.
        app.on_key(KeyEvent::new(KeyCode::Char('k'), KeyModifiers::NONE));
        assert!(screen(&mut app).contains("Both come from the same commit"));

        // Packages view summarizes the link range.
        app.on_key(KeyEvent::new(KeyCode::Char('L'), KeyModifiers::NONE));
        app.on_key(KeyEvent::new(KeyCode::Char('j'), KeyModifiers::NONE));
        assert!(screen(&mut app).contains("commits ≈aaaaaaa → ≈bbbbbbb"));
    }

    #[test]
    fn draws_delete_confirmation() {
        let mut app = app();
        for c in ['k', 'D', '4'] {
            app.on_key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE));
        }
        let s = screen(&mut app);
        assert!(s.contains("Delete generation"), "{s}");
        assert!(s.contains("Delete system generation 42"), "{s}");
        assert!(s.contains("--delete-generations 42"), "{s}");
        assert!(s.contains("Type 42 and press enter to delete"), "{s}");
        assert!(s.contains("> 4"), "{s}");

        // In a narrow terminal the command wraps; the prompt must still be visible.
        let mut terminal = Terminal::new(TestBackend::new(70, 30)).unwrap();
        terminal.draw(|f| draw(f, &mut app)).unwrap();
        let buffer = terminal.backend().buffer();
        let narrow: String = (0..buffer.area.height)
            .flat_map(|y| (0..buffer.area.width).map(move |x| (x, y)))
            .map(|(x, y)| buffer[(x, y)].symbol().to_owned())
            .collect();
        assert!(narrow.contains("> 4"), "prompt cut off when wrapped");
    }

    #[test]
    fn draws_gc_confirmation_and_help_fits() {
        let mut app = app();
        app.on_key(KeyEvent::new(KeyCode::Char('C'), KeyModifiers::NONE));
        let s = screen(&mut app);
        assert!(s.contains("Garbage collection"), "{s}");
        assert!(s.contains("--gc"), "{s}");
        assert!(s.contains("y collects, any other key cancels"), "{s}");

        // The help popup must fit a 20-row terminal, its last line included.
        app.on_key(KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE));
        app.show_help = true;
        let mut terminal = Terminal::new(TestBackend::new(100, 20)).unwrap();
        terminal.draw(|f| draw(f, &mut app)).unwrap();
        let buffer = terminal.backend().buffer();
        let text: String = (0..buffer.area.height)
            .flat_map(|y| (0..buffer.area.width).map(move |x| (x, y)))
            .map(|(x, y)| buffer[(x, y)].symbol().to_owned())
            .collect();
        assert!(text.contains("q ctrl-c"), "help cut off at 20 rows");
    }

    #[test]
    fn draws_help_and_empty_states() {
        let mut app = app();
        app.show_help = true;
        assert!(screen(&mut app).contains("Keys (any key closes)"));
        app.show_help = false;
        app.filter = "zzz".into();
        assert!(screen(&mut app).contains("No packages match"));
        app.tabs[0].cursor = 0;
        assert!(screen(&mut app).contains("No earlier generation"));
        app.active = 1;
        assert!(screen(&mut app).contains("Loading…"));
    }
}
