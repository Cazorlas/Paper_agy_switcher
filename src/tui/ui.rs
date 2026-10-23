use crossterm::event::KeyCode;
use ratatui::{
    Frame,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Cell, Paragraph, Row, Table, TableState},
};

use super::app::{App, Tab, UsageStatus};
use super::hitmap::OverlayClick;
use super::keymap;
use super::popup;
use super::theme::{
    BG, C_BLUE, C_CYAN, C_GRAY, C_GREEN, C_MAGENTA, C_RED, C_WHITE, C_YELLOW, DIM, base, highlight,
};
use crate::output::{
    format_local_time, format_reset_time,
};
use crate::usage::UsageInfo;

fn status_message_color(is_error: bool) -> Color {
    if is_error { C_RED } else { C_CYAN }
}

pub fn render(f: &mut Frame, app: &mut App) {
    let area = f.area();
    app.hitmap.clear();

    // Paint the entire area with a solid background first
    f.render_widget(Block::default().style(base()), area);

    let status_height = status_bar_height(app, area.width);

    let vertical = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1),                    // tab bar
            Constraint::Min(6),                       // active tab content
            Constraint::Length(status_height as u16), // status bar
        ])
        .split(area);

    render_tab_bar(f, app, vertical[0]);

    match app.active_tab {
        Tab::Accounts => {
            let content = vertical[1];
            let detail_height = if app.detail_visible {
                detail_panel_height(app).min(content.height.saturating_sub(6))
            } else {
                0
            };
            let rows = Layout::default()
                .direction(Direction::Vertical)
                .constraints([
                    Constraint::Min(6),                // account list
                    Constraint::Length(detail_height), // detail panel
                ])
                .split(content);
            render_account_table(f, app, rows[0]);
            if app.detail_visible {
                render_detail_panel(f, app, rows[1]);
            }
        }
        Tab::Settings => {
            super::settings::render_settings_tab(
                f,
                &app.settings,
                vertical[1],
                &mut app.hitmap,
                None,
            )
        }
        Tab::Logs => render_logs(f, app, vertical[1]),
    }

    render_status_bar(f, app, vertical[2]);

    // Overlays (rendered last, on top of everything).
    // Help popup takes top priority since the user invoked it explicitly.
    let active_tab = app.active_tab;
    if let Some(state) = app.help_popup.as_mut() {
        let panel = render_help_popup(f, state, active_tab, area, None);
        app.hitmap.overlay = match panel {
            Some(panel) => super::hitmap::OverlayHit::Dismissible { panel },
            None => super::hitmap::OverlayHit::Modal,
        };
    } else if app.menu.is_some() {
        let rendered = app.menu.as_mut().and_then(|menu| menu.render(f, area));
        app.hitmap.menu_actions = rendered
            .as_ref()
            .map(|menu| menu.actions.clone())
            .unwrap_or_default();
        app.hitmap.overlay = match rendered {
            Some(menu) => super::hitmap::OverlayHit::Dismissible { panel: menu.panel },
            None => super::hitmap::OverlayHit::Modal,
        };
    } else if app.confirm.is_some()
        || app.rename.is_some()
        || app.search_active
        || (app.active_tab == Tab::Settings && app.settings.is_editing())
    {
        app.hitmap.overlay = super::hitmap::OverlayHit::Modal;
    }
}

fn render_logs(f: &mut Frame, app: &mut App, area: Rect) {
    app.hitmap.logs = Some(area);
    let inner_width = area.width.saturating_sub(2).max(1);
    let previous_revision = (app.log_render_width == inner_width)
        .then_some(app.log_render_revision)
        .flatten();
    if let Some((revision, lines)) = app.log_writer.lines_if_changed(previous_revision) {
        app.log_render_revision = Some(revision);
        app.log_render_width = inner_width;
        app.log_visual_lines = if lines.is_empty() {
            vec!["No logs in this session.".to_string()]
        } else {
            hard_wrap_log_lines(&lines, inner_width)
        };
    }
    let visual_lines = &app.log_visual_lines;
    let visible_rows = usize::from(area.height.saturating_sub(2)).max(1);
    let max_scroll = visual_lines.len().saturating_sub(visible_rows);
    let bottom_offset = usize::from(app.log_scroll).min(max_scroll);
    app.log_scroll = u16::try_from(bottom_offset).unwrap_or(u16::MAX);
    let end = visual_lines.len().saturating_sub(bottom_offset);
    let start = end.saturating_sub(visible_rows);
    let text = visual_lines[start..end].join("\n");
    let block = Block::default()
        .title(" Session logs ")
        .borders(Borders::ALL)
        .border_style(base().fg(C_BLUE))
        .style(base());
    f.render_widget(
        Paragraph::new(text).style(base().fg(C_GRAY)).block(block),
        area,
    );
}

fn hard_wrap_log_lines(lines: &[String], width: u16) -> Vec<String> {
    let width = usize::from(width.max(1));
    let mut wrapped = Vec::new();
    for line in lines {
        if line.is_empty() {
            wrapped.push(String::new());
            continue;
        }
        let mut row = String::new();
        let mut row_width: usize = 0;
        for ch in line.chars() {
            let ch_width = display_width(ch.encode_utf8(&mut [0; 4]));
            if row_width > 0 && row_width.saturating_add(ch_width) > width {
                wrapped.push(std::mem::take(&mut row));
                row_width = 0;
            }
            row.push(ch);
            row_width = row_width.saturating_add(ch_width);
        }
        wrapped.push(row);
    }
    wrapped
}

fn render_help_popup(
    f: &mut Frame,
    state: &mut popup::PopupState,
    active_tab: Tab,
    area: ratatui::layout::Rect,
    upgrade_instructions: Option<&[String]>,
) -> Option<Rect> {
    let mut lines: Vec<Line<'static>> = Vec::new();
    let key_style = base().fg(C_YELLOW).add_modifier(Modifier::BOLD);
    let label_style = base().fg(C_WHITE);
    let heading_style = base().fg(C_CYAN).add_modifier(Modifier::BOLD);
    let dim_style = base().fg(DIM);

    if let Some(instructions) = upgrade_instructions {
        for instruction in instructions {
            lines.push(Line::from(Span::styled(
                instruction.clone(),
                base().fg(C_YELLOW).add_modifier(Modifier::BOLD),
            )));
        }
        lines.push(Line::from(""));
    }

    // Compute key column width for alignment within section
    let active_section = match active_tab {
        Tab::Accounts => keymap::Section::Account,
        Tab::Settings => keymap::Section::Settings,
        Tab::Logs => keymap::Section::Logs,
    };
    let groups = keymap::help_sections_for(active_section);
    let key_col = groups
        .iter()
        .flat_map(|(_, items)| items.iter())
        .map(|(k, _)| display_width(k))
        .max()
        .unwrap_or(8);


    for (i, (heading, items)) in groups.iter().enumerate() {
        if i > 0 {
            lines.push(Line::from(""));
        }
        lines.push(Line::from(Span::styled(
            (*heading).to_string(),
            heading_style,
        )));
        for (k, label) in items {
            let pad = key_col.saturating_sub(display_width(k));
            let mut spans: Vec<Span<'static>> = Vec::new();
            spans.push(Span::styled("  ", base()));
            spans.push(Span::styled((*k).to_string(), key_style));
            if pad > 0 {
                spans.push(Span::styled(" ".repeat(pad), base()));
            }
            spans.push(Span::styled("  ", base()));
            spans.push(Span::styled((*label).to_string(), label_style));
            lines.push(Line::from(spans));
        }
    }
    if active_tab == Tab::Accounts {
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled(
            "Commands (run in a terminal)".to_string(),
            heading_style,
        )));
        for (cmd, what) in [
            ("list", "usage of every account, numbered"),
            ("use [n|alias]", "switch to the best account, or to number n / alias"),
            ("login [alias]", "save the current agy login as an account"),
            ("auto", "switch automatically near the limit; stop with Ctrl+C or close the window"),
            ("launch [alias]", "switch to the account, then start agy"),
            ("delete / restore", "remove an account / bring a deleted one back"),
            ("self-update", "update to the newest version"),
            ("uninstall", "remove the program (asks before deleting your data)"),
        ] {
            let mut spans: Vec<Span<'static>> = Vec::new();
            spans.push(Span::styled("  ", base()));
            spans.push(Span::styled(cmd.to_string(), key_style));
            spans.push(Span::styled("  ", base()));
            spans.push(Span::styled(what.to_string(), label_style));
            lines.push(Line::from(spans));
        }
    }
    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        "  esc / q / h to close \u{2022} j k arrows / PgUp PgDn / mouse wheel to scroll",
        dim_style,
    )));

    popup::render_popup(f, "Help", &lines, state, area).map(|layout| layout.panel)
}

fn display_width(s: &str) -> usize {
    Line::from(s).width()
}

#[derive(Debug, PartialEq, Eq)]
struct TableTextWidths {
    alias: u16,
    email: u16,
    plan: u16,
}

fn table_text_widths(
    total_width: u16,
    aliases: &[&str],
    emails: &[&str],
    plans: &[&str],
    show_5h: bool,
) -> TableTextWidths {
    let desired = |header: &str, values: &[&str]| {
        values
            .iter()
            .map(|value| u16::try_from(display_width(value)).unwrap_or(u16::MAX))
            .chain(std::iter::once(
                u16::try_from(display_width(header)).unwrap_or(u16::MAX),
            ))
            .max()
            .unwrap_or(0)
    };
    let mut widths = TableTextWidths {
        alias: desired("Alias", aliases).max(5),
        email: desired("Email", emails).max(5),
        plan: desired("Plan", plans).max(4),
    };

    // Base borders, spacing, marker and fixed columns consume 55 cells. The
    // optional 5h pair and Credits column add their widths plus spacing.
    let fixed_width = 47 + u16::from(show_5h) * 20;
    let budget = total_width.saturating_sub(fixed_width).max(14);
    let total = u32::from(widths.alias) + u32::from(widths.email) + u32::from(widths.plan);
    let mut excess = total.saturating_sub(u32::from(budget));
    for (width, minimum) in [
        (&mut widths.email, 5_u16),
        (&mut widths.plan, 4_u16),
        (&mut widths.alias, 5_u16),
    ] {
        let shrink = excess.min(u32::from(width.saturating_sub(minimum)));
        *width -= shrink as u16;
        excess -= shrink;
    }
    widths
}

const QUOTA_HEADERS: [&str; 4] = ["Claude/GPT 5h", "Claude/GPT weekly", "Gemini 5h", "Gemini weekly"];

fn render_account_table(f: &mut Frame, app: &mut App, area: Rect) {
    let mut title = format!(" Accounts ({}) sort:{} quota: {} ", app.accounts.len(), app.sort_mode.as_str(), crate::config::get().quota.source.label());
    if let Some(search) = &app.search { title.push_str(&format!(" /{} ", search.query)); }
    if app.loading_count() > 0 { title.push_str(&format!("fetching {}... ", app.loading_count())); }
    if let Some(seconds) = app.auto_refresh_remaining_secs() {
        title.push_str(&format!("auto:{} ", format_auto_refresh_remaining(seconds)));
    }
    let block = Block::default().title(title).borders(Borders::ALL)
        .border_style(base().fg(C_BLUE)).style(base());
    if app.accounts.is_empty() {
        f.render_widget(Paragraph::new("No accounts yet. Press a to save an agy login, or q to quit.")
            .block(block).style(base()), area);
        return;
    }
    let aliases: Vec<_> = app.view_indices.iter().map(|&i| app.accounts[i].alias.as_str()).collect();
    let emails: Vec<_> = app.view_indices.iter().map(|&i| app.accounts[i].info.email.as_deref().unwrap_or("--")).collect();
    let tiers: Vec<_> = app.view_indices.iter().map(|&i| match &app.accounts[i].usage {
        UsageStatus::Loaded(value) => value.plan_type.as_deref().unwrap_or("--"),
        _ => "--",
    }).collect();
    let widths = table_text_widths(area.width.saturating_sub(50), &aliases, &emails, &tiers, true);
    let mut rows = Vec::new();
    let mut cards = Vec::new();
    // Each quota column is as wide as its longest text, so "100% (resets MM-DD HH:MM)" is never clipped.
    let mut quota_widths = QUOTA_HEADERS.map(|h| h.chars().count() as u16);
    for (view, &index) in app.view_indices.iter().enumerate() {
        let entry = &app.accounts[index];
        let marker = if app.marked.contains(&entry.alias) { ">" } else if entry.is_current { "*" } else { " " };
        let selected = view == app.selected;
        let style = if entry.is_current { base().fg(C_GREEN) } else { base() };
        let (windows, status) = match &entry.usage {
            UsageStatus::Loaded(value) => {
                let gemini = crate::usage::gemini_pool(value);
                ([value.primary.as_ref(), value.secondary.as_ref(),
                    gemini.and_then(|p| p.primary.as_ref()), gemini.and_then(|p| p.secondary.as_ref())],
                    if app.is_refreshing(&entry.alias) { "refreshing" } else { "OK" }.to_owned())
            }
            UsageStatus::Error(error) => ([None; 4], error.summary.clone()),
            UsageStatus::Loading => ([None; 4], "loading...".to_owned()),
            UsageStatus::Idle => ([None; 4], "--".to_owned()),
        };
        let quota: Vec<_> = windows.iter().map(|window| crate::usage::window_text(*window)).collect();
        for (width, text) in quota_widths.iter_mut().zip(&quota) {
            *width = (*width).max(text.chars().count() as u16);
        }
        let status_style = if matches!(entry.usage, UsageStatus::Error(_)) { base().fg(C_RED) } else { base().fg(DIM) };
        let mut cells = vec![Cell::from(marker).style(style), Cell::from(entry.alias.clone()),
            Cell::from(emails[view]), Cell::from(tiers[view]).style(plan_color(Some(tiers[view]), selected))];
        for (window, text) in windows.iter().zip(&quota) {
            let percent = window.and_then(|w| w.used_percent).map(|p| format!("{p:.0}%")).unwrap_or_default();
            let quota_style = if window.is_some_and(|w| w.disabled) { base().fg(DIM) } else { usage_pct_style(&percent, selected) };
            cells.push(Cell::from(text.clone()).style(quota_style));
        }
        cells.push(Cell::from(status.clone()).style(status_style));
        rows.push(Row::new(cells).style(style));
        cards.push(vec![
            Line::from(Span::styled(format!("{marker} {}. {} [{}]", view + 1, entry.alias, tiers[view]), style)),
            Line::from(format!("  {}", emails[view])),
            Line::from(format!("  Claude/GPT 5h {}", quota[0])),
            Line::from(format!("             7d {}", quota[1])),
            Line::from(format!("  Gemini 5h {}", quota[2])),
            Line::from(format!("         7d {}", quota[3])),
            Line::from(Span::styled(format!("  {status}"), status_style)),
            Line::from(""),
        ]);
    }
    let quota_total: u16 = quota_widths.iter().sum();
    let constraints = [Constraint::Length(1), Constraint::Length(widths.alias),
        Constraint::Length(widths.email), Constraint::Length(widths.plan),
        Constraint::Length(quota_widths[0]), Constraint::Length(quota_widths[1]),
        Constraint::Length(quota_widths[2]), Constraint::Length(quota_widths[3]),
        Constraint::Min(16)];
    let natural = widths.alias + widths.email + widths.plan + quota_total + 27;
    if natural > area.width {
        let inner = block.inner(area);
        f.render_widget(block, area);
        // Keep the selected account visible when the terminal uses stacked cards.
        let visible = (usize::from(inner.height) / 8).max(1);
        let first = app.selected.saturating_sub(visible - 1);
        let lines: Vec<_> = cards.into_iter().skip(first).flatten().collect();
        f.render_widget(Paragraph::new(lines).style(base()), inner);
        app.hitmap.account_list = None;
    } else {
        let [h5, hw, g5, gw] = QUOTA_HEADERS;
        let header = Row::new(["", "Alias", "Email", "Tier", h5, hw, g5, gw, "Status / error"])
            .style(base().fg(C_CYAN).add_modifier(Modifier::BOLD));
        let mut state = TableState::default().with_selected(app.selected);
        f.render_stateful_widget(Table::new(rows, constraints).header(header).block(block)
            .row_highlight_style(highlight()).style(base()), area, &mut state);
        app.hitmap.account_list = Some(super::hitmap::ListHit {
            rows_area: super::hitmap::table_rows_area(area), offset: state.offset(), row_count: app.view_indices.len(),
        });
    }
}

fn usage_gauges_height(usage: &UsageInfo) -> u16 {
    let multi_pool = !usage.additional_limits.is_empty();
    let mut height = 0u16;
    let mut pool_count = 0u16;
    let mut add_pool = |primary: bool, secondary: bool| {
        if pool_count > 0 {
            height = height.saturating_add(1);
        }
        if multi_pool {
            height = height.saturating_add(1);
        }
        height = height.saturating_add(u16::from(primary) * 2);
        height = height.saturating_add(u16::from(secondary) * 2);
        if !primary && !secondary {
            height = height.saturating_add(1);
        }
        pool_count = pool_count.saturating_add(1);
    };
    add_pool(usage.primary.is_some(), usage.secondary.is_some());
    for pool in &usage.additional_limits {
        add_pool(pool.primary.is_some(), pool.secondary.is_some());
    }
    height.max(1)
}

fn detail_panel_height(app: &App) -> u16 {
    let gauges = app
        .selected_account_idx()
        .and_then(|idx| app.accounts.get(idx))
        .and_then(|entry| match &entry.usage {
            UsageStatus::Loaded(usage) => Some(usage_gauges_height(usage)),
            _ => None,
        })
        .unwrap_or(4);
    gauges.saturating_add(4)
}

fn render_detail_panel(f: &mut Frame, app: &App, area: Rect) {
    let entry = match app
        .selected_account_idx()
        .and_then(|idx| app.accounts.get(idx))
    {
        Some(e) => e,
        None => return,
    };

    let title = if entry.is_current {
        format!(" * {} (active) ", entry.alias)
    } else {
        format!(" {} ", entry.alias)
    };

    let block = Block::default()
        .title(title)
        .borders(Borders::ALL)
        .border_style(base().fg(if entry.is_current { C_GREEN } else { C_BLUE }))
        .style(base());

    let inner = block.inner(area);
    f.render_widget(block, area);

    let layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(1)])
        .margin(1)
        .split(inner);

    // Usage area
    match &entry.usage {
        UsageStatus::Idle => {
            let p = Paragraph::new("Press r to refresh usage").style(base().fg(DIM));
            f.render_widget(p, layout[0]);
        }
        UsageStatus::Loading => {
            let p = Paragraph::new("Fetching usage...").style(base().fg(C_YELLOW));
            f.render_widget(p, layout[0]);
        }
        UsageStatus::Error(e) => {
            let p = Paragraph::new(format!("Error: {}", e.detail)).style(base().fg(C_RED));
            f.render_widget(p, layout[0]);
        }
        UsageStatus::Loaded(u) => {
            render_usage_gauges(f, u, layout[0]);
        }
    }
}

pub(super) fn render_usage_gauges(f: &mut Frame, u: &UsageInfo, area: Rect) {
    let now = crate::auth::now_unix_secs();
    let multi_pool = !u.additional_limits.is_empty();
    let mut y = area.y;
    let mut render_pool = |f: &mut Frame,
                           name: &str,
                           primary: Option<&crate::usage::WindowUsage>,
                           secondary: Option<&crate::usage::WindowUsage>,
                           unavailable: bool| {
        if y > area.y {
            y = y.saturating_add(1);
        }
        if multi_pool && y < area.bottom() {
            let title = if unavailable {
                format!("{name}  unavailable")
            } else {
                name.to_string()
            };
            f.render_widget(
                Paragraph::new(title).style(base().fg(if unavailable { C_RED } else { C_CYAN })),
                Rect {
                    x: area.x,
                    y,
                    width: area.width,
                    height: 1,
                },
            );
            y = y.saturating_add(1);
        }
        if let Some(window) = primary
            && y < area.bottom()
        {
            let (label, window_secs) =
                quota_window_display(window, "5h", crate::usage::WINDOW_5H_SECS);
            render_usage_gauge(
                f,
                window,
                &label,
                window_secs,
                now,
                Rect {
                    x: area.x,
                    y,
                    width: area.width,
                    height: 2,
                },
            );
            y = y.saturating_add(2);
        }
        if let Some(window) = secondary
            && y < area.bottom()
        {
            let (label, window_secs) =
                quota_window_display(window, "7d", crate::usage::WINDOW_7D_SECS);
            render_usage_gauge(
                f,
                window,
                &label,
                window_secs,
                now,
                Rect {
                    x: area.x,
                    y,
                    width: area.width,
                    height: 2,
                },
            );
            y = y.saturating_add(2);
        }
        if primary.is_none() && secondary.is_none() && y < area.bottom() {
            f.render_widget(
                Paragraph::new("No active window").style(base().fg(DIM)),
                Rect {
                    x: area.x,
                    y,
                    width: area.width,
                    height: 1,
                },
            );
            y = y.saturating_add(1);
        }
    };

    render_pool(f, "Claude and GPT models", u.primary.as_ref(), u.secondary.as_ref(), false);
    for pool in &u.additional_limits {
        render_pool(
            f,
            pool.limit_name.as_deref().unwrap_or("Additional"),
            pool.primary.as_ref(),
            pool.secondary.as_ref(),
            pool.allowed == Some(false) || pool.limit_reached == Some(true),
        );
    }
}

fn quota_window_display(
    window: &crate::usage::WindowUsage,
    fallback_label: &str,
    fallback_secs: i64,
) -> (String, i64) {
    match window.window_minutes {
        Some(minutes) if minutes % 1_440 == 0 => {
            (format!("{}d", minutes / 1_440), minutes.saturating_mul(60))
        }
        Some(minutes) if minutes % 60 == 0 => {
            (format!("{}h", minutes / 60), minutes.saturating_mul(60))
        }
        Some(minutes) => (format!("{minutes}m"), minutes.saturating_mul(60)),
        None => (fallback_label.to_string(), fallback_secs),
    }
}









/// Top tab bar: Accounts, Providers, Settings, Logs.
fn render_tab_bar(f: &mut Frame, app: &mut App, area: Rect) {
    let active = base().fg(BG).bg(C_CYAN).add_modifier(Modifier::BOLD);
    let inactive = base().fg(C_GRAY);
    let style = |tab: Tab| {
        if app.active_tab == tab {
            active
        } else {
            inactive
        }
    };

    let accounts = format!(" Accounts ({}) ", app.accounts.len());
    let settings = " Settings ".to_string();
    let logs = " Logs ".to_string();
    let gap_w = 2u16;

    let mut x = area.x;
    let tab_specs = [
        (accounts.as_str(), Tab::Accounts),
        (settings.as_str(), Tab::Settings),
        (logs.as_str(), Tab::Logs),
    ];
    for (i, (label, tab)) in tab_specs.iter().enumerate() {
        if i > 0 {
            x = x.saturating_add(gap_w);
        }
        let width = label.chars().count() as u16;
        app.hitmap.tabs.push((
            Rect {
                x,
                y: area.y,
                width,
                height: area.height.max(1),
            },
            *tab,
        ));
        x = x.saturating_add(width);
    }

    let line = Line::from(vec![
        Span::styled(accounts, style(Tab::Accounts)),
        Span::styled("  ", base()),
        Span::styled("  ", base()),
        Span::styled(settings, style(Tab::Settings)),
        Span::styled("  ", base()),
        Span::styled(logs, style(Tab::Logs)),
        Span::styled("   Tab to switch", base().fg(DIM)),
    ]);
    f.render_widget(Paragraph::new(line).style(base()), area);
}







#[derive(Clone, Copy)]
struct FooterActionSpec {
    key: &'static str,
    label: &'static str,
    code: KeyCode,
    enabled: bool,
}

type FooterHit = (usize, usize, usize, KeyCode);

fn normal_footer_specs(app: &App) -> Vec<FooterActionSpec> {
    match app.active_tab {
        Tab::Logs => vec![
            FooterActionSpec {
                key: "tab",
                label: "switch tabs",
                code: KeyCode::Tab,
                enabled: true,
            },
            FooterActionSpec {
                key: "↑",
                label: "up",
                code: KeyCode::Up,
                enabled: true,
            },
            FooterActionSpec {
                key: "↓",
                label: "down",
                code: KeyCode::Down,
                enabled: true,
            },
            FooterActionSpec {
                key: "end",
                label: "latest",
                code: KeyCode::End,
                enabled: true,
            },
            FooterActionSpec {
                key: "q",
                label: "quit",
                code: KeyCode::Char('q'),
                enabled: true,
            },
        ],

        Tab::Settings => vec![
            FooterActionSpec {
                key: "↑",
                label: "previous",
                code: KeyCode::Up,
                enabled: true,
            },
            FooterActionSpec {
                key: "↓",
                label: "next",
                code: KeyCode::Down,
                enabled: true,
            },
            FooterActionSpec {
                key: "enter",
                label: "edit",
                code: KeyCode::Enter,
                enabled: true,
            },
            FooterActionSpec {
                key: "s",
                label: "save",
                code: KeyCode::Char('s'),
                enabled: true,
            },
            FooterActionSpec {
                key: "h",
                label: "help",
                code: KeyCode::Char('h'),
                enabled: true,
            },
            FooterActionSpec {
                key: "q",
                label: "quit",
                code: KeyCode::Char('q'),
                enabled: true,
            },
        ],
        Tab::Accounts => keymap::status_bar_items()
            .into_iter()
            .flat_map(|(key, label)| {
                if key == "j / k / ↑ ↓" {
                    return vec![
                        FooterActionSpec {
                            key: "↑",
                            label: "previous",
                            code: KeyCode::Up,
                            enabled: app.view_indices.len() > 1,
                        },
                        FooterActionSpec {
                            key: "↓",
                            label: "next",
                            code: KeyCode::Down,
                            enabled: app.view_indices.len() > 1,
                        },
                    ];
                }
                vec![FooterActionSpec {
                    key,
                    label: short_label(label),
                    code: account_footer_code(key),
                    enabled: account_footer_enabled(app, key),
                }]
            })
            .collect(),
    }
}

fn render_status_bar(f: &mut Frame, app: &mut App, area: Rect) {
    // Rename input takes top priority
    if app.rename.is_some() {
        let input = app.rename.as_ref().expect("rename").input.clone();
        let shown = format!(" Rename: {input}#  (Enter confirm / Esc cancel)");
        let line = Line::from(vec![
            Span::styled(" Rename: ", base().fg(C_CYAN).add_modifier(Modifier::BOLD)),
            Span::styled(input, base().fg(C_WHITE).add_modifier(Modifier::BOLD)),
            Span::styled("#", base().fg(C_GRAY)),
            Span::styled("  (Enter confirm / Esc cancel)", base().fg(DIM)),
        ]);
        f.render_widget(Paragraph::new(line).style(base()), area);
        app.hitmap.overlay_panel = Some(area);
        if let Some(pos) = shown.find("Enter") {
            register_overlay_key(app, area, pos, 5, KeyCode::Enter);
        }
        if let Some(pos) = shown.find("Esc") {
            register_overlay_key(app, area, pos, 3, KeyCode::Esc);
        }
        return;
    }

    // Confirmation prompt
    if app.confirm.is_some() {
        let msg = match app.confirm.as_ref().expect("confirm") {
            super::app::ConfirmAction::DiscardSettings => {
                "Discard unsaved settings and quit? (y/n)".to_string()
            }
            super::app::ConfirmAction::Delete(alias) => {
                format!("Delete profile '{alias}'? (y/n)")
            }
            super::app::ConfirmAction::BatchDelete(aliases) => {
                format!("Delete {} marked profile(s)? (y/n)", aliases.len())
            }

        };
        let line = Line::from(Span::styled(
            msg.clone(),
            base().fg(C_RED).add_modifier(Modifier::BOLD),
        ));
        f.render_widget(Paragraph::new(line).style(base()), area);
        app.hitmap.overlay_panel = Some(area);
        if let Some(pos) = msg.find("(y/n)") {
            // Columns, not bytes: the prompt may hold a multi-byte `·`.
            let pos = msg[..pos].chars().count();
            register_overlay_key(app, area, pos + 1, 1, KeyCode::Char('y'));
            register_overlay_key(app, area, pos + 3, 1, KeyCode::Char('n'));
        } else if let Some(pos) = msg.find("y to use") {
            register_overlay_key(app, area, pos, 1, KeyCode::Char('y'));
            if pos > 0 {
                register_overlay_key(app, area, 0, pos, KeyCode::Esc);
            }
            let after = pos + 1;
            if after < msg.chars().count() {
                register_overlay_key(
                    app,
                    area,
                    after,
                    msg.chars().count().saturating_sub(after),
                    KeyCode::Esc,
                );
            }
        }
        return;
    }

    if app.search_active {
        let Some(query) = app.search.as_ref().map(|s| s.query.clone()) else {
            return;
        };
        let shown = format!(" /{query}#  (Enter accept / Esc clear)");
        let line = Line::from(vec![
            Span::styled(" /", base().fg(C_CYAN).add_modifier(Modifier::BOLD)),
            Span::styled(query, base().fg(C_WHITE).add_modifier(Modifier::BOLD)),
            Span::styled("#", base().fg(C_GRAY)),
            Span::styled("  (Enter accept / Esc clear)", base().fg(DIM)),
        ]);
        f.render_widget(Paragraph::new(line).style(base()), area);
        app.hitmap.overlay_panel = Some(area);
        if let Some(pos) = shown.find("Enter") {
            register_overlay_key(app, area, pos, 5, KeyCode::Enter);
        }
        if let Some(pos) = shown.find("Esc") {
            register_overlay_key(app, area, pos, 3, KeyCode::Esc);
        }
        return;
    }

    let load_warning = app
        .profile_load_error
        .as_deref()
        .map(|error| format!("Account data stale/incomplete: {error}"))
        .unwrap_or_default();
    if !load_warning.is_empty() {
        let warning_color = C_RED;
        let msg = Line::from(Span::styled(
            load_warning,
            base().fg(warning_color).add_modifier(Modifier::BOLD),
        ));
        f.render_widget(Paragraph::new(msg).style(base()), area);
    } else if let Some(s) = &app.status_msg {
        let msg = Line::from(Span::styled(
            s.as_str(),
            base().fg(status_message_color(app.status_is_error)),
        ));
        f.render_widget(Paragraph::new(msg).style(base()), area);
    } else if !app.marked.is_empty() {
        let prefix = format!(" {} selected \u{2014} ", app.marked.len());
        let enter_label = "for batch";
        let esc_label = "to clear";
        let enter_x = prefix.chars().count();
        let esc_x = enter_x + "enter".chars().count() + 1 + enter_label.chars().count() + 3;
        let line = Line::from(vec![
            Span::styled(prefix, base().fg(C_YELLOW)),
            Span::styled("enter", base().fg(C_YELLOW).add_modifier(Modifier::BOLD)),
            Span::styled(format!(" {enter_label} \u{2502} "), base().fg(DIM)),
            Span::styled("esc", base().fg(C_YELLOW).add_modifier(Modifier::BOLD)),
            Span::styled(format!(" {esc_label}"), base().fg(DIM)),
        ]);
        f.render_widget(Paragraph::new(line).style(base()), area);
        register_footer_hit(app, area, 0, enter_x, "enter".len(), KeyCode::Enter);
        register_footer_hit(app, area, 0, esc_x, "esc".len(), KeyCode::Esc);
    } else {
        let specs = normal_footer_specs(app);
        render_footer_specs(f, app, area, &specs);
    }

    // Version indicator — always rendered at bottom-right corner
    let ver_spans = version_spans(app);
    if let Some(ver_area) = version_area(app, area) {
        f.render_widget(
            Paragraph::new(Line::from(ver_spans)).style(base()),
            ver_area,
        );
    }
}

fn render_footer_specs(f: &mut Frame, app: &mut App, area: Rect, specs: &[FooterActionSpec]) {
    let (lines, hits) = build_footer_layout(footer_content_width(app, area.width), specs);
    f.render_widget(Paragraph::new(lines).style(base()), area);
    for (line, x, width, code) in hits {
        register_footer_hit(app, area, line, x, width, code);
    }
}

fn footer_content_width(app: &App, width: u16) -> usize {
    let version_width: usize = version_spans(app).iter().map(Span::width).sum();
    usize::from(width).saturating_sub(version_width)
}

fn build_footer_layout(
    width: usize,
    specs: &[FooterActionSpec],
) -> (Vec<Line<'static>>, Vec<FooterHit>) {
    let separator = " \u{2502} ";
    let mut lines = Vec::new();
    let mut hits = Vec::new();
    let mut spans = vec![Span::styled(" ", base())];
    let mut used = 1usize;
    let mut line = 0usize;

    for (idx, spec) in specs.iter().enumerate() {
        let key_width = spec.key.chars().count();
        let label_width = spec.label.chars().count();
        let separator_width = usize::from(idx + 1 < specs.len()) * separator.chars().count();
        let item_width = key_width + 1 + label_width + separator_width;
        if used + item_width > width && used > 1 {
            lines.push(Line::from(spans));
            spans = vec![Span::styled(" ", base())];
            used = 1;
            line += 1;
        }

        if spec.enabled {
            hits.push((line, used, key_width + 1 + label_width, spec.code));
        }
        spans.push(Span::styled(
            spec.key,
            base().fg(C_YELLOW).add_modifier(Modifier::BOLD),
        ));
        spans.push(Span::styled(" ", base()));
        spans.push(Span::styled(spec.label, base().fg(C_GRAY)));
        if idx + 1 < specs.len() {
            spans.push(Span::styled(separator, base().fg(DIM)));
        }
        used += item_width;
    }
    if spans.len() > 1 || lines.is_empty() {
        lines.push(Line::from(spans));
    }
    (lines, hits)
}

fn register_overlay_key(app: &mut App, area: Rect, x: usize, width: usize, code: KeyCode) {
    let Ok(x) = u16::try_from(x) else {
        return;
    };
    let Ok(width) = u16::try_from(width) else {
        return;
    };
    if width == 0 || x >= area.width {
        return;
    }
    app.hitmap.overlay_clicks.push((
        Rect {
            x: area.x.saturating_add(x),
            y: area.y,
            width: width.min(area.width.saturating_sub(x)),
            height: 1,
        },
        OverlayClick::Key(code),
    ));
}

fn register_footer_hit(
    app: &mut App,
    area: Rect,
    line: usize,
    x: usize,
    width: usize,
    code: KeyCode,
) {
    let Ok(line) = u16::try_from(line) else {
        return;
    };
    let Ok(x) = u16::try_from(x) else {
        return;
    };
    let Ok(width) = u16::try_from(width) else {
        return;
    };
    let y = area.y.saturating_add(line);
    if y >= area.y.saturating_add(area.height) || x >= area.width {
        return;
    }
    let mut right = x.saturating_add(width).min(area.width);
    if let Some(version) = version_area(app, area)
        && y == version.y
        && version.x > area.x
    {
        right = right.min(version.x.saturating_sub(area.x));
    }
    if right > x {
        app.hitmap.footer_actions.push((
            Rect {
                x: area.x.saturating_add(x),
                y,
                width: right - x,
                height: 1,
            },
            code,
        ));
    }
}

fn account_footer_code(key: &str) -> KeyCode {
    match key {
        "j / k / ↑ ↓" => KeyCode::Char('j'),
        "/" => KeyCode::Char('/'),
        "enter" => KeyCode::Enter,
        "o" => KeyCode::Char('o'),
        "u" => KeyCode::Char('u'),
        "a" => KeyCode::Char('a'),
        "r" => KeyCode::Char('r'),
        "i" => KeyCode::Char('i'),
        "h" => KeyCode::Char('h'),
        "q" => KeyCode::Char('q'),
        _ => KeyCode::Null,
    }
}

fn account_footer_enabled(app: &App, key: &str) -> bool {
    let has_selected = app.selected_account_idx().is_some() && app.marked.is_empty();
    match key {
        "j / k / ↑ ↓" => app.view_indices.len() > 1,
        "/" | "a" | "h" | "q" => true,
        "enter" | "o" | "u" | "i" => has_selected,
        "r" => !app.view_indices.is_empty(),
        _ => false,
    }
}

fn version_spans(app: &App) -> Vec<Span<'static>> {
    let version = crate::update::current_version();
    if let Some(latest) = &app.update_available {
        vec![
            Span::styled(" \u{2502} ", base().fg(DIM)),
            Span::styled(format!("v{version}"), base().fg(DIM)),
            Span::styled(format!(" -> v{latest} "), base().fg(C_YELLOW)),
        ]
    } else {
        vec![
            Span::styled(" \u{2502} ", base().fg(DIM)),
            Span::styled(format!("v{version} "), base().fg(DIM)),
        ]
    }
}

fn version_area(app: &App, area: Rect) -> Option<Rect> {
    let width: u16 = version_spans(app)
        .iter()
        .map(|span| span.width())
        .try_fold(0u16, |acc, width| {
            u16::try_from(width)
                .ok()
                .and_then(|width| acc.checked_add(width))
        })?;
    if area.width <= width {
        return None;
    }
    Some(Rect {
        x: area.x + area.width - width,
        y: area.y + area.height.saturating_sub(1),
        width,
        height: 1,
    })
}

/// Render a single usage gauge (5h or 7d) with block chars and pace marker.
fn render_usage_gauge(
    f: &mut Frame,
    w: &crate::usage::WindowUsage,
    label: &str,
    window_secs: i64,
    now: i64,
    area: Rect,
) {
    if w.disabled {
        let reset = crate::output::format_reset_relative(w.resets_at, now);
        f.render_widget(Paragraph::new(format!("{label}  off ↻{reset}")).style(base().fg(DIM)), area);
        return;
    }
    let used = w.used_percent.unwrap_or(0.0).min(100.0);
    let remaining_pct = (100.0 - used).max(0.0);
    let pace = crate::usage::visible_pace_percent(w, window_secs);
    let over = used >= 10.0 && pace.is_some_and(|p| used > p);
    let reset_str = w
        .resets_at
        .map(format_reset_time)
        .unwrap_or_else(|| "--".into());
    let remaining_secs = w.resets_at.map(|ts| ts - now).unwrap_or(0);

    // Row 1: block-char bar  "5h  ████████░░|░░░░░░░  25% used  75% left"
    let gauge_area = Rect { height: 1, ..area };
    let label_text = format!("{label}  ");
    let suffix = format!("  {used:.0}% used  {remaining_pct:.0}% left");
    let bar_width = (gauge_area.width as usize)
        .saturating_sub(label_text.len())
        .saturating_sub(suffix.len());

    let used_color = if used >= 90.0 {
        C_RED
    } else if over || used >= 70.0 {
        C_YELLOW
    } else {
        C_GREEN
    };
    let used_style = base().fg(used_color);
    let remaining_style = base().fg(remaining_color(remaining_pct));
    let pace_style = base().fg(C_WHITE).add_modifier(Modifier::BOLD);

    // L2: if bar_width is 0 (extremely narrow terminal), skip bar rendering entirely
    if bar_width == 0 {
        let reset_area = Rect {
            y: area.y + 1,
            height: 1,
            ..area
        };
        let reset_text = format!("resets in {reset_str}");
        f.render_widget(
            Paragraph::new(reset_text).style(base().fg(reset_color(remaining_secs))),
            reset_area,
        );
        return;
    }

    let pace_pos = pace.map(|p| {
        ((p / 100.0) * bar_width as f64)
            .round()
            .clamp(0.0, bar_width.saturating_sub(1) as f64) as usize
    });
    let used_pos = ((used / 100.0) * bar_width as f64)
        .round()
        .clamp(0.0, bar_width as f64) as usize;

    let mut spans = vec![Span::styled(label_text.clone(), base().fg(C_WHITE))];

    if let Some(pp) = pace_pos {
        let before_used = pp.min(used_pos);
        let before_remaining = pp.saturating_sub(used_pos);
        let after_used = used_pos.saturating_sub(pp + 1);
        let after_remaining = bar_width.saturating_sub(pp + 1 + after_used);

        if before_used > 0 {
            spans.push(Span::styled("█".repeat(before_used), used_style));
        }
        if before_remaining > 0 {
            spans.push(Span::styled("░".repeat(before_remaining), remaining_style));
        }
        spans.push(Span::styled("|", pace_style));
        if after_used > 0 {
            spans.push(Span::styled("█".repeat(after_used), used_style));
        }
        if after_remaining > 0 {
            spans.push(Span::styled("░".repeat(after_remaining), remaining_style));
        }
    } else {
        if used_pos > 0 {
            spans.push(Span::styled("█".repeat(used_pos), used_style));
        }
        if bar_width > used_pos {
            spans.push(Span::styled(
                "░".repeat(bar_width - used_pos),
                remaining_style,
            ));
        }
    }

    let suffix_color = if over { C_YELLOW } else { DIM };
    spans.push(Span::styled(suffix, base().fg(suffix_color)));

    f.render_widget(Paragraph::new(Line::from(spans)).style(base()), gauge_area);

    // Row 2: "started HH:MM" left, "↑ pace" at pace position, "resets in ..." right
    let reset_area = Rect {
        y: area.y + 1,
        height: 1,
        ..area
    };
    let reset_text = format!("resets in {reset_str}");
    let reset_style = base().fg(reset_color(remaining_secs));
    let started_text = w
        .resets_at
        .map(|ts| format!("started {}", format_local_time(ts - window_secs)))
        .unwrap_or_default();
    let started_len = started_text.len();

    let total_width = reset_area.width as usize;
    let reset_start = total_width.saturating_sub(reset_text.len());

    let row2 = if let Some(pp) = pace_pos {
        let arrow_offset = label_text.len() + pp;
        let pace_label = "\u{2191} pace"; // ↑ pace  (display width = 6, byte len = 8)
        const PACE_LABEL_DISPLAY_WIDTH: usize = 6;
        let pace_end = arrow_offset + PACE_LABEL_DISPLAY_WIDTH;

        // Try to fit: started ... ↑ pace ... resets in ...
        if !started_text.is_empty()
            && started_len + 2 <= arrow_offset
            && pace_end + 2 <= reset_start
        {
            Line::from(vec![
                Span::styled(&started_text, base().fg(DIM)),
                Span::styled(" ".repeat(arrow_offset - started_len), base()),
                Span::styled(pace_label, base().fg(DIM)),
                Span::styled(" ".repeat(reset_start - pace_end), base()),
                Span::styled(reset_text, reset_style),
            ])
        } else if pace_end + 2 <= reset_start {
            // No room for started, show pace + reset
            Line::from(vec![
                Span::styled(" ".repeat(arrow_offset), base()),
                Span::styled(pace_label, base().fg(DIM)),
                Span::styled(" ".repeat(reset_start - pace_end), base()),
                Span::styled(reset_text, reset_style),
            ])
        } else {
            // Tight: started left, reset right
            let mut spans = Vec::new();
            if !started_text.is_empty() && started_len + 2 <= reset_start {
                spans.push(Span::styled(&started_text, base().fg(DIM)));
                spans.push(Span::styled(" ".repeat(reset_start - started_len), base()));
            } else {
                spans.push(Span::styled(" ".repeat(reset_start), base()));
            }
            spans.push(Span::styled(reset_text, reset_style));
            Line::from(spans)
        }
    } else {
        // No pace marker: started left, reset after label offset
        let mut spans = Vec::new();
        if !started_text.is_empty() {
            spans.push(Span::styled(&started_text, base().fg(DIM)));
            let gap = reset_start.saturating_sub(started_len);
            spans.push(Span::styled(" ".repeat(gap), base()));
        } else {
            spans.push(Span::styled(" ".repeat(label_text.len()), base()));
        }
        spans.push(Span::styled(reset_text, reset_style));
        Line::from(spans)
    };

    f.render_widget(Paragraph::new(row2).style(base()), reset_area);
}

// ── Style helpers ─────────────────────────────────────────

/// Color for remaining percentage: green > 30%, yellow > 10%, red <= 10%
fn remaining_color(remaining_pct: f64) -> Color {
    if remaining_pct > 30.0 {
        C_GREEN
    } else if remaining_pct > 10.0 {
        C_YELLOW
    } else {
        C_RED
    }
}

fn plan_color(plan: Option<&str>, is_selected: bool) -> Style {
    let color = match plan {
        Some("g1-pro-tier") => C_YELLOW,
        Some("g1-ultra-tier") => C_MAGENTA,
        _ => C_GRAY,
    };
    let style = base().fg(color);
    if is_selected { style.add_modifier(Modifier::BOLD) } else { style }
}

fn reset_color(remaining_secs: i64) -> Color {
    if remaining_secs < 3600 {
        C_GREEN
    } else if remaining_secs < 14400 {
        C_YELLOW
    } else {
        C_RED
    }
}

/// Style for a used-percent cell (e.g. `82%`, `82%!` when ahead of pace):
/// colored by what is left, like the detail bars.
fn usage_pct_style(used_pct_str: &str, is_selected: bool) -> Style {
    let over_pace = used_pct_str.ends_with('!');
    let clean = used_pct_str.trim_end_matches('!');
    let fg = if over_pace {
        C_RED
    } else {
        match clean.trim_end_matches('%').parse::<f64>() {
            Ok(used) => remaining_color((100.0 - used).max(0.0)),
            Err(_) => DIM,
        }
    };
    let s = base().fg(fg);
    if is_selected {
        s.add_modifier(Modifier::BOLD)
    } else {
        s
    }
}

/// Compress verbose keymap labels for status bar.
fn short_label(label: &str) -> &str {
    match label {
        "move selection" => "nav",
        "search" => "search",
        "use (switch to) selected account" => "use",
        "add new account" => "add",
        "refresh visible accounts" => "refresh",
        "show / hide account detail panel" => "detail",
        "use (switch to)" => "use",
        "show this help (main view)" => "help",
        "quit (main view)" => "quit",
        "launch agy" => "launch",
        other => other,
    }
}

fn format_auto_refresh_remaining(secs: u64) -> String {
    if secs == 0 {
        return "now".to_string();
    }
    if secs < 60 {
        return format!("{secs}s");
    }
    let mins = secs / 60;
    let rem = secs % 60;
    if rem == 0 {
        format!("{mins}m")
    } else {
        format!("{mins}m{rem}s")
    }
}

fn status_bar_height(app: &App, width: u16) -> usize {
    if app.status_msg.is_some()
        || app.profile_load_error.is_some()
        || app.rename.is_some()
        || app.confirm.is_some()
        || app.search_active
        || !app.marked.is_empty()
    {
        return 1;
    }
    build_footer_layout(footer_content_width(app, width), &normal_footer_specs(app))
        .0
        .len()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::usage::AccountInfo;
    use crate::tui::app::{AccountEntry, App, Tab, UsageStatus};
    use crate::usage::{AdditionalRateLimit, UsageInfo, WindowUsage};
    use ratatui::{Terminal, backend::TestBackend, layout::Rect};
    use std::io::Write;
    use tracing_subscriber::fmt::MakeWriter;

    fn row_text(backend: &TestBackend, y: u16) -> String {
        let area = backend.buffer().area;
        (0..area.width)
            .map(|x| {
                backend
                    .buffer()
                    .cell((x, y))
                    .expect("cell inside test buffer")
                    .symbol()
            })
            .collect()
    }

    #[test]
    fn table_usage_cells_count_up_and_turn_red_near_the_limit() {
        // Cells show the used percent, like the detail bars.
        assert_eq!(usage_pct_style("5%", false).fg, Some(remaining_color(95.0)));
        assert_eq!(usage_pct_style("95%", false).fg, Some(remaining_color(5.0)));
        assert_eq!(usage_pct_style("95%", false).fg, Some(C_RED));
        assert_eq!(usage_pct_style("20%!", false).fg, Some(C_RED), "ahead of pace");
    }

    #[test]
    fn status_message_color_distinguishes_errors_from_information() {
        assert_eq!(status_message_color(false), C_CYAN);
        assert_eq!(status_message_color(true), C_RED);
    }

    #[test]
    fn persistent_load_warning_overrides_transient_status_until_reload_succeeds() {
        let mut app = App::new();
        app.profile_load_error = Some("could not read profile directory".into());
        app.status_msg = Some("A later informational message".into());
        app.status_is_error = false;
        assert_eq!(status_bar_height(&app, 100), 1);

        let mut terminal = Terminal::new(TestBackend::new(100, 1)).unwrap();
        terminal
            .draw(|frame| render_status_bar(frame, &mut app, frame.area()))
            .unwrap();
        let rendered = row_text(terminal.backend(), 0);
        assert!(
            rendered.contains("Account data stale/incomplete"),
            "{rendered}"
        );
        assert!(
            !rendered.contains("A later informational message"),
            "{rendered}"
        );

        app.profile_load_error = None;
        terminal
            .draw(|frame| render_status_bar(frame, &mut app, frame.area()))
            .unwrap();
        assert!(row_text(terminal.backend(), 0).contains("A later informational message"));
    }







    #[test]
    fn version_area_is_absent_when_the_terminal_is_narrower_than_the_version() {
        assert!(version_area(&App::new(), Rect::new(0, 0, 0, 0)).is_none());
    }









    #[test]
    fn logs_render_in_their_own_tab() {
        let mut app = App::new();
        app.log_writer = crate::logging::TuiLogWriter::new();
        app.active_tab = Tab::Logs;
        app.log_writer
            .make_writer()
            .write_all(b"tui-tab-test-error\n")
            .unwrap();
        let mut terminal = Terminal::new(TestBackend::new(80, 12)).unwrap();

        terminal
            .draw(|frame| super::render(frame, &mut app))
            .unwrap();

        let screen = (0..12)
            .map(|y| row_text(terminal.backend(), y))
            .collect::<Vec<_>>()
            .join("\n");
        assert!(screen.contains("Accounts"));
        assert!(screen.contains("Logs"));
        assert!(screen.contains("tui-tab-test-error"));
    }

    #[test]
    fn logs_tab_starts_at_the_wrapped_tail_of_long_errors() {
        let mut app = App::new();
        app.log_writer = crate::logging::TuiLogWriter::new();
        app.active_tab = Tab::Logs;
        let error = format!("{}LATEST_TAIL\n", "x".repeat(200));
        app.log_writer
            .make_writer()
            .write_all(error.as_bytes())
            .unwrap();
        let mut terminal = Terminal::new(TestBackend::new(40, 8)).unwrap();

        terminal
            .draw(|frame| super::render(frame, &mut app))
            .unwrap();

        let screen = (0..8)
            .map(|y| row_text(terminal.backend(), y))
            .collect::<Vec<_>>()
            .join("\n");
        assert!(screen.contains("LATEST_TAIL"));
    }

    #[test]
    fn logs_tab_clamps_scroll_before_rendering_a_short_log() {
        let mut app = App::new();
        app.log_writer = crate::logging::TuiLogWriter::new();
        app.active_tab = Tab::Logs;
        app.log_scroll = 10;
        app.log_writer
            .make_writer()
            .write_all(b"short-log-marker\n")
            .unwrap();
        let mut terminal = Terminal::new(TestBackend::new(40, 8)).unwrap();

        terminal
            .draw(|frame| super::render(frame, &mut app))
            .unwrap();

        let screen = (0..8)
            .map(|y| row_text(terminal.backend(), y))
            .collect::<Vec<_>>()
            .join("\n");
        assert!(screen.contains("short-log-marker"));
        assert_eq!(app.log_scroll, 0);
    }



    #[test]
    fn additional_quota_pool_expands_the_main_detail_panel() {
        let window = WindowUsage {
            used_percent: Some(25.0),
            resets_at: Some(1_000_000),
            window_minutes: Some(300),
            ..Default::default()
        };
        let usage = UsageInfo {
            primary: Some(window.clone()),
            secondary: Some(window.clone()),
            additional_limits: vec![AdditionalRateLimit {
                limit_name: Some("Fable".to_string()),
                metered_feature: Some("fable".to_string()),
                primary: Some(window.clone()),
                secondary: Some(window),
                ..Default::default()
            }],
            ..Default::default()
        };

        assert_eq!(usage_gauges_height(&usage), 11);
    }

    #[test]
    fn additional_primary_slot_uses_its_real_seven_day_window_for_label_and_pace() {
        let usage = UsageInfo {
            additional_limits: vec![AdditionalRateLimit {
                limit_name: Some("Opus".to_string()),
                metered_feature: Some("opus".to_string()),
                primary: Some(WindowUsage {
                    used_percent: Some(8.0),
                    resets_at: Some(crate::auth::now_unix_secs() + 6 * 24 * 60 * 60),
                    window_minutes: Some(7 * 24 * 60),
                    ..Default::default()
                }),
                ..Default::default()
            }],
            ..Default::default()
        };
        let backend = TestBackend::new(100, 10);
        let mut terminal = Terminal::new(backend).unwrap();

        terminal
            .draw(|frame| render_usage_gauges(frame, &usage, frame.area()))
            .unwrap();

        let row = (0..10)
            .map(|y| row_text(terminal.backend(), y))
            .find(|line| line.contains("7d"))
            .expect("the real seven-day window label must be rendered");
        assert!(!row.contains("5h"));
        let label_x = row.find("7d").unwrap();
        let pace_x = row.find('|').expect("pace marker");
        assert!(
            pace_x > label_x + 6,
            "seven-day pace must not be clamped to the start of the bar: {row}"
        );
    }

    #[test]
    fn plan_color_uses_antigravity_tiers() {
        assert_eq!(plan_color(Some("g1-pro-tier"), false).fg, Some(C_YELLOW));
        assert_eq!(plan_color(Some("g1-ultra-tier"), false).fg, Some(C_MAGENTA));
        assert_eq!(plan_color(Some("future-tier"), false).fg, Some(C_GRAY));
    }

    #[test]
    fn account_table_columns_fit_an_eighty_column_terminal() {
        let widths = table_text_widths(
            80,
            &["a-very-long-account-alias"],
            &["a-very-long-address@example.com"],
            &["Team - NightCity Workspace"],
            true,
        );

        assert!(widths.alias + widths.email + widths.plan <= 16);
    }

    #[test]
    fn account_table_columns_use_extra_space_beyond_the_old_caps() {
        let alias = "a".repeat(45);
        let email = format!("{}@example.com", "e".repeat(40));
        let plan = format!("Team - {}", "Workspace".repeat(5));
        let widths = table_text_widths(260, &[&alias], &[&email], &[&plan], true);

        assert_eq!(widths.alias, alias.len() as u16);
        assert_eq!(widths.email, email.len() as u16);
        assert_eq!(widths.plan, plan.len() as u16);
    }



    #[test]
    fn narrow_window_stacks_one_block_per_account() {
        let mut app = App::new();
        for alias in ["paperengineer05", "paperengineer07"] {
            app.accounts.push(AccountEntry {
                alias: alias.into(),
                info: AccountInfo {
                    email: Some(format!("{alias}@gmail.com")),
                    ..AccountInfo::default()
                },
                usage: UsageStatus::Loaded(Box::default()),
                is_current: alias.ends_with('5'),
            });
            app.view_indices.push(app.view_indices.len());
        }

        let render = |app: &mut App, width: u16| {
            let backend = TestBackend::new(width, 16);
            let mut terminal = Terminal::new(backend).unwrap();
            terminal
                .draw(|frame| render_account_table(frame, app, frame.area()))
                .unwrap();
            (0..16)
                .map(|y| row_text(terminal.backend(), y))
                .collect::<Vec<_>>()
                .join("
")
        };

        let narrow = render(&mut app, 50);
        assert!(narrow.contains("1. paperengineer05"), "{narrow}");
        assert!(narrow.contains("paperengineer07@gmail.com"), "{narrow}");
        assert!(narrow.contains("5h ") && narrow.contains("7d "), "{narrow}");
        assert!(!narrow.contains("Alias"), "no table header when stacked: {narrow}");

        let wide = render(&mut app, 160);
        assert!(wide.contains("Alias") && wide.contains("Email"), "{wide}");
    }

    #[test]
    fn disabled_usage_gauge_is_dim_and_off() {
        let window = WindowUsage {
            used_percent: Some(53.1232), disabled: true, resets_at: Some(1_000_000 + 3600),
            ..Default::default()
        };
        let mut terminal = Terminal::new(TestBackend::new(80, 3)).unwrap();
        terminal.draw(|frame| render_usage_gauge(frame, &window, "5h", 18000, 1_000_000, frame.area())).unwrap();
        let text = row_text(terminal.backend(), 0);
        assert!(text.contains("off ↻1h0m"), "{text}");
        assert!(!text.contains('%'), "{text}");
        assert_eq!(terminal.backend().buffer()[(0, 0)].fg, DIM);
    }

    #[test]
    fn accounts_show_both_quota_groups_and_row_errors() {
        let mut app = App::new();
        let window = |used| crate::usage::WindowUsage { used_percent: Some(used), resets_at: Some(crate::auth::now_unix_secs() + 3600), window_minutes: Some(300), ..Default::default() };
        app.accounts.push(AccountEntry {
            alias: "saved".into(), info: AccountInfo { email: Some("saved@example.com".into()), ..Default::default() },
            usage: UsageStatus::Loaded(Box::new(UsageInfo {
                primary: Some(window(12.0)), secondary: Some(window(34.0)), plan_type: Some("g1-pro-tier".into()),
                additional_limits: vec![crate::usage::AdditionalRateLimit { limit_name: Some("Gemini".into()), primary: Some(window(56.0)), secondary: Some(window(78.0)), ..Default::default() }],
                ..Default::default()
            })), is_current: true,
        });
        app.accounts.push(AccountEntry {
            alias: "expired".into(), info: AccountInfo::default(),
            usage: UsageStatus::Error(crate::usage::UsageError { summary: "sign-in expired".into(), detail: "sign-in expired".into() }), is_current: false,
        });
        app.view_indices = vec![0, 1];
        let mut terminal = Terminal::new(TestBackend::new(220, 16)).unwrap();
        terminal.draw(|frame| render_account_table(frame, &mut app, frame.area())).unwrap();
        let text = (0..16).map(|y| row_text(terminal.backend(), y)).collect::<Vec<_>>().join("\n");
        for expected in ["saved@example.com", "g1-pro-tier", "Claude/GPT", "Gemini", "12%", "34%", "56%", "78%", "resets", "sign-in expired"] {
            assert!(text.contains(expected), "missing {expected}: {text}");
        }
    }
}
