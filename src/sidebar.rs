use std::io::{self, stdout};
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use crossterm::event::{
    self, DisableFocusChange, DisableMouseCapture, EnableFocusChange, EnableMouseCapture, Event,
    KeyCode, KeyEventKind, MouseButton, MouseEventKind,
};
use crossterm::execute;
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Alignment, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph};

use crate::ipc::{Request, call};
use crate::model::{
    AgentSnapshot, ConversationSnapshot, DisplayState, SessionSnapshot, Snapshot, StateSource,
};
use crate::paths::Paths;
use crate::server::ServerIdentity;

#[derive(Debug, Clone)]
enum Row {
    Section(&'static str),
    Session(SessionSnapshot),
    Agent(AgentSnapshot),
    Detail(String, String),
    Conversation(AgentSnapshot, ConversationSnapshot),
    Spacer,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FooterButton {
    New,
    Help,
    Menu,
    Close,
}

pub fn run(paths: &Paths, server: &ServerIdentity) -> Result<(), Box<dyn std::error::Error>> {
    enable_raw_mode()?;
    let mut output = stdout();
    execute!(
        output,
        EnterAlternateScreen,
        EnableMouseCapture,
        EnableFocusChange
    )?;
    let backend = CrosstermBackend::new(output);
    let mut terminal = Terminal::new(backend)?;
    let result = event_loop(&mut terminal, paths, server);
    disable_raw_mode()?;
    execute!(
        terminal.backend_mut(),
        LeaveAlternateScreen,
        DisableMouseCapture,
        DisableFocusChange
    )?;
    terminal.show_cursor()?;
    result
}

fn event_loop(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    paths: &Paths,
    server: &ServerIdentity,
) -> Result<(), Box<dyn std::error::Error>> {
    let socket = paths.socket_for_server(&server.key);
    let (refresh_tx, refresh_rx) = mpsc::sync_channel::<()>(1);
    let (snapshot_tx, snapshot_rx) = mpsc::sync_channel(1);
    std::thread::spawn(move || {
        while refresh_rx.recv().is_ok() {
            let result = fetch_snapshot(&socket).map_err(|error| error.to_string());
            let _ = snapshot_tx.try_send(result);
        }
    });
    let mut rows = Vec::new();
    let mut snapshot = None;
    let mut detailed = false;
    let mut help_visible = false;
    let mut footer_hover = None;
    let mut selected = 0_usize;
    let mut selection_visible = true;
    let mut scroll = 0_usize;
    let mut disconnected = true;
    let mut last_success = None;
    let mut next_refresh = Instant::now();
    let mut dirty = true;
    loop {
        if let Ok(result) = snapshot_rx.try_recv() {
            match result {
                Ok(fetched) => {
                    rows = build_rows(&fetched, detailed);
                    snapshot = Some(fetched);
                    disconnected = false;
                    last_success = Some(Instant::now());
                    selected = nearest_selectable(&rows, selected).unwrap_or(0);
                }
                Err(_) => {
                    // Keep the last good snapshot through a short scan/IPC
                    // hiccup. A genuinely unavailable daemon still becomes
                    // visible after the stale grace instead of flashing on
                    // every isolated timeout.
                    disconnected = last_success
                        .is_none_or(|success| success.elapsed() >= Duration::from_secs(3));
                }
            }
            dirty = true;
        }
        if Instant::now() >= next_refresh {
            let _ = refresh_tx.try_send(());
            next_refresh = Instant::now() + Duration::from_secs(1);
        }

        let size = terminal.size()?;
        let body_height = usize::from(size.height).max(1);
        let content_height = body_height.saturating_sub(1);
        let viewport_height = if rows.len() > content_height {
            content_height.saturating_sub(1)
        } else {
            content_height
        };
        if dirty {
            keep_visible(selected, viewport_height, rows.len(), &mut scroll);
            terminal.draw(|frame| {
                let area = frame.area();
                let mut lines = Vec::new();
                if disconnected {
                    lines.push(Line::from(Span::styled(
                        "disconnected",
                        Style::default().add_modifier(Modifier::DIM),
                    )));
                } else if rows.is_empty() {
                    lines.push(Line::from(Span::styled(
                        "no sessions",
                        Style::default().add_modifier(Modifier::DIM),
                    )));
                } else {
                    for (index, row) in rows.iter().enumerate().skip(scroll).take(viewport_height) {
                        lines.push(render_row(
                            row,
                            selection_visible && index == selected,
                            area.width,
                        ));
                    }
                    let shown = rows.len().saturating_sub(scroll).min(viewport_height);
                    let hidden = rows.len().saturating_sub(shown);
                    if hidden > 0 {
                        lines.push(Line::from(format!("↕ {hidden} hidden")));
                    }
                }
                while lines.len() < content_height {
                    lines.push(Line::default());
                }
                lines.push(render_footer(area.width, footer_hover, popup_mode()));
                frame.render_widget(Paragraph::new(lines), area);
                if help_visible {
                    render_help(frame, area);
                }
            })?;
            dirty = false;
        }

        if !event::poll(Duration::from_millis(50))? {
            continue;
        }
        let input = event::read()?;
        dirty = true;
        match input {
            Event::Key(key) if key.kind == KeyEventKind::Press => {
                if popup_mode()
                    && !help_visible
                    && (key.code == KeyCode::Esc
                        || (key.modifiers.contains(event::KeyModifiers::CONTROL)
                            && matches!(key.code, KeyCode::Char('c' | 'd'))))
                {
                    return Ok(());
                }
                match key.code {
                    KeyCode::Char('?') => help_visible = !help_visible,
                    KeyCode::Esc if help_visible => help_visible = false,
                    _ if help_visible => {}
                    KeyCode::Char('j') | KeyCode::Down => {
                        selection_visible = true;
                        move_selection(&rows, &mut selected, 1);
                    }
                    KeyCode::Char('k') | KeyCode::Up => {
                        selection_visible = true;
                        move_selection(&rows, &mut selected, -1);
                    }
                    KeyCode::Enter => {
                        let navigated = activate(&rows, selected)?;
                        if popup_mode() && navigated {
                            return Ok(());
                        }
                    }
                    KeyCode::Char('m') => show_row_menu(
                        &rows,
                        selected,
                        Some((0, selected.saturating_sub(scroll) as u16)),
                    )?,
                    KeyCode::Char('d') => {
                        let selected_key = rows.get(selected).and_then(selection_key);
                        detailed = !detailed;
                        if let Some(snapshot) = &snapshot {
                            rows = build_rows(snapshot, detailed);
                            selected = selected_key
                                .as_deref()
                                .and_then(|key| {
                                    rows.iter()
                                        .position(|row| selection_key(row).as_deref() == Some(key))
                                })
                                .or_else(|| nearest_selectable(&rows, selected))
                                .unwrap_or(0);
                        }
                    }
                    KeyCode::Char('N') => run_session_picker()?,
                    KeyCode::Char('i') => run_command("mux-inspect-pick")?,
                    KeyCode::Char('W') => run_command("ws-new-prompt")?,
                    KeyCode::Char('P') if selection_visible => promote_selected(&rows, selected)?,
                    KeyCode::Char('R') => run_command("gen-tmuxinator-configs")?,
                    KeyCode::Char('n') => run_workbench(&["attention", "next"])?,
                    KeyCode::Char('s') => run_workbench(&["pick", "session"])?,
                    KeyCode::Char('a') => run_workbench(&["pick", "agent"])?,
                    KeyCode::Char('r') => run_workbench(&["reload"])?,
                    KeyCode::Char('c') if key.modifiers.contains(event::KeyModifiers::CONTROL) => {
                        return Ok(());
                    }
                    _ => {}
                }
            }
            Event::Mouse(mouse) => {
                if help_visible {
                    if matches!(mouse.kind, MouseEventKind::Down(_))
                        && !rect_contains(
                            help_rect(Rect::new(0, 0, size.width, size.height)),
                            mouse.column,
                            mouse.row,
                        )
                    {
                        help_visible = false;
                    }
                    continue;
                }
                match mouse.kind {
                    MouseEventKind::Moved => {
                        footer_hover = if usize::from(mouse.row) == body_height.saturating_sub(1) {
                            footer_button(size.width, mouse.column)
                        } else {
                            None
                        };
                        let row = scroll + usize::from(mouse.row);
                        if matches!(
                            rows.get(row),
                            Some(Row::Session(_) | Row::Agent(_) | Row::Conversation(_, _))
                        ) {
                            selected = row;
                            selection_visible = true;
                        } else {
                            selection_visible = false;
                        }
                    }
                    MouseEventKind::ScrollDown => {
                        selection_visible = true;
                        move_selection(&rows, &mut selected, 1);
                    }
                    MouseEventKind::ScrollUp => {
                        selection_visible = true;
                        move_selection(&rows, &mut selected, -1);
                    }
                    MouseEventKind::Down(MouseButton::Left) => {
                        if usize::from(mouse.row) == body_height.saturating_sub(1) {
                            match footer_button(size.width, mouse.column) {
                                Some(FooterButton::New) => run_session_picker()?,
                                Some(FooterButton::Help) => help_visible = true,
                                Some(FooterButton::Menu) => {
                                    show_global_menu(false, Some((mouse.column, mouse.row)))?
                                }
                                Some(FooterButton::Close) => return Ok(()),
                                None => {}
                            }
                            continue;
                        }
                        let clicked = scroll + usize::from(mouse.row);
                        if matches!(
                            rows.get(clicked),
                            Some(Row::Session(_) | Row::Agent(_) | Row::Conversation(_, _))
                        ) {
                            selected = clicked;
                            let navigated = activate(&rows, selected)?;
                            if popup_mode() && navigated {
                                return Ok(());
                            }
                        }
                    }
                    MouseEventKind::Down(MouseButton::Right) => {
                        let clicked = scroll + usize::from(mouse.row);
                        if matches!(
                            rows.get(clicked),
                            Some(Row::Session(_) | Row::Agent(_) | Row::Conversation(_, _))
                        ) {
                            selected = clicked;
                            show_row_menu(&rows, selected, Some((mouse.column, mouse.row)))?;
                        }
                    }
                    _ => {}
                }
            }
            Event::FocusGained => selection_visible = true,
            Event::FocusLost => {
                selection_visible = false;
                footer_hover = None;
            }
            _ => {}
        }
    }
}

fn fetch_snapshot(socket: &std::path::Path) -> Result<Snapshot, Box<dyn std::error::Error>> {
    let value = call(
        socket,
        &Request::new("snapshot.get", serde_json::Value::Null),
        Duration::from_millis(500),
    )?;
    Ok(serde_json::from_value(value)?)
}

fn build_rows(snapshot: &Snapshot, detailed: bool) -> Vec<Row> {
    if std::env::var_os("WORKBENCH_POPUP").is_some() {
        return build_popup_rows(snapshot, detailed);
    }
    let mut rows = vec![Row::Section("sessions")];
    let mut sessions = snapshot.sessions.clone();
    sessions.sort_by_key(|session| session.attention_count == 0);
    for session in sessions {
        rows.push(Row::Session(session));
    }
    rows.push(Row::Spacer);
    rows.push(Row::Section("agents"));
    let mut agents = snapshot.agents.clone();
    agents.sort_by_key(|agent| {
        (
            agent.attention.as_ref().is_none_or(|event| event.seen),
            agent.target.session_name.clone(),
            agent.target.window_index,
            agent.target.pane_index,
        )
    });
    for agent in agents {
        rows.push(Row::Agent(agent.clone()));
        if detailed {
            let pid = agent
                .process
                .as_ref()
                .map(|process| process.pid.to_string())
                .unwrap_or_else(|| "—".into());
            rows.push(Row::Detail(
                format!(
                    "   {} · {}:{} · pid {pid}",
                    agent_kind_name(agent.kind),
                    agent.target.window_index,
                    agent.target.pane_index
                ),
                String::new(),
            ));
            rows.push(Row::Detail(
                format!("   {}", source_health(&agent)),
                format!("rule {}", agent.rule_id.as_deref().unwrap_or("—")),
            ));
        }
        rows.extend(
            agent
                .conversations
                .iter()
                .cloned()
                .map(|conversation| Row::Conversation(agent.clone(), conversation)),
        );
    }
    rows
}

fn build_popup_rows(snapshot: &Snapshot, detailed: bool) -> Vec<Row> {
    let mut rows = vec![Row::Section("attention")];
    let mut agents = snapshot.agents.clone();
    agents.sort_by_key(|agent| {
        (
            agent.attention.as_ref().is_none_or(|event| event.seen),
            agent
                .attention
                .as_ref()
                .map(|event| event.since_unix_ms)
                .unwrap_or(u64::MAX),
            agent.target.session_name.clone(),
            agent.target.window_index,
            agent.target.pane_index,
        )
    });
    for agent in agents
        .iter()
        .filter(|agent| agent.attention.as_ref().is_some_and(|event| !event.seen))
    {
        rows.push(Row::Agent(agent.clone()));
    }
    rows.push(Row::Spacer);
    rows.push(Row::Section("agents"));
    for agent in agents {
        rows.push(Row::Agent(agent.clone()));
        if detailed {
            rows.push(Row::Detail(
                format!(
                    "   {} · {}:{}",
                    agent_kind_name(agent.kind),
                    agent.target.window_index,
                    agent.target.pane_index
                ),
                source_health(&agent),
            ));
        }
    }
    rows.push(Row::Spacer);
    rows.push(Row::Section("sessions"));
    let mut sessions = snapshot.sessions.clone();
    sessions.sort_by_key(|session| (session.attention_count == 0, session.session_name.clone()));
    rows.extend(sessions.into_iter().map(Row::Session));
    rows
}

fn render_row(row: &Row, selected: bool, width: u16) -> Line<'static> {
    match row {
        Row::Section(label) => Line::from(Span::styled(
            *label,
            Style::default()
                .add_modifier(Modifier::BOLD)
                .add_modifier(Modifier::DIM),
        )),
        Row::Session(session) => {
            let suffix = if session.attention_count > 0 {
                format!("{} · {}!", session.agent_count, session.attention_count)
            } else {
                session.agent_count.to_string()
            };
            aligned_line(
                format!(" {} {}", glyph(session.rollup_state), session.session_name),
                suffix,
                width,
                if session.active {
                    Style::default()
                        .fg(state_color(session.rollup_state))
                        .add_modifier(Modifier::BOLD)
                } else {
                    Style::default().fg(state_color(session.rollup_state))
                },
                Style::default().add_modifier(Modifier::DIM),
                selected,
            )
        }
        Row::Agent(agent) => {
            let mut status = if agent.display_state == DisplayState::Blocked {
                agent.reason_category.as_deref().unwrap_or("blocked")
            } else {
                state_name(agent.display_state)
            }
            .to_owned();
            if agent.hook_health == crate::model::HookHealth::Conflict {
                status.push_str(" !");
            }
            let estimated = agent.state_source == StateSource::Screen;
            let primary = if agent.visible && !estimated {
                Style::default()
                    .fg(state_color(agent.display_state))
                    .add_modifier(Modifier::BOLD)
            } else if estimated {
                Style::default()
                    .fg(state_color(agent.display_state))
                    .add_modifier(Modifier::DIM)
            } else {
                Style::default().fg(state_color(agent.display_state))
            };
            aligned_line(
                format!(
                    " {} {}·{}",
                    glyph(agent.display_state),
                    agent.target.session_name,
                    agent.label
                ),
                status,
                width,
                primary,
                if agent.visible && !estimated {
                    Style::default()
                        .fg(state_color(agent.display_state))
                        .add_modifier(Modifier::BOLD)
                } else {
                    Style::default()
                        .fg(state_color(agent.display_state))
                        .add_modifier(Modifier::DIM)
                },
                selected,
            )
        }
        Row::Conversation(_, conversation) => {
            let status = if conversation.display_state == DisplayState::Blocked {
                conversation.reason_category.as_deref().unwrap_or("blocked")
            } else {
                state_name(conversation.display_state)
            };
            aligned_line(
                format!(
                    "   {} {}",
                    glyph(conversation.display_state),
                    conversation.label
                ),
                status.to_owned(),
                width,
                if conversation.active {
                    Style::default()
                        .fg(state_color(conversation.display_state))
                        .add_modifier(Modifier::BOLD)
                } else {
                    Style::default().fg(state_color(conversation.display_state))
                },
                Style::default()
                    .fg(state_color(conversation.display_state))
                    .add_modifier(Modifier::DIM),
                selected,
            )
        }
        Row::Detail(left, right) => {
            let text = if right.is_empty() {
                left.clone()
            } else {
                format!("{left} · {right}")
            };
            Line::from(Span::styled(
                text.chars().take(usize::from(width)).collect::<String>(),
                Style::default().add_modifier(Modifier::DIM),
            ))
        }
        Row::Spacer => Line::default(),
    }
}

fn popup_mode() -> bool {
    std::env::var_os("WORKBENCH_POPUP").is_some()
}

fn render_footer(width: u16, hovered: Option<FooterButton>, popup: bool) -> Line<'static> {
    let button_style = |button| {
        let style = Style::default().fg(Color::Cyan);
        if hovered == Some(button) {
            style.add_modifier(Modifier::REVERSED)
        } else {
            style
        }
    };
    let fixed_width = if popup { 26 } else { 18 };
    let gap = usize::from(width).saturating_sub(fixed_width);
    let mut spans = vec![
        Span::styled("+ new", button_style(FooterButton::New)),
        Span::raw(" ".repeat(gap)),
        Span::styled("? help", button_style(FooterButton::Help)),
        Span::raw(" "),
        Span::styled("⋯ menu", button_style(FooterButton::Menu)),
    ];
    if popup {
        spans.push(Span::raw(" "));
        spans.push(Span::styled("× close", button_style(FooterButton::Close)));
    }
    Line::from(spans)
}

fn footer_button(width: u16, column: u16) -> Option<FooterButton> {
    if column < 5 {
        return Some(FooterButton::New);
    }
    let popup = popup_mode();
    let close_start = width.saturating_sub(7);
    if popup && column >= close_start {
        return Some(FooterButton::Close);
    }
    let right_offset = if popup { 8 } else { 0 };
    let help_start = width.saturating_sub(13 + right_offset);
    let menu_start = width.saturating_sub(6 + right_offset);
    if column >= menu_start {
        Some(FooterButton::Menu)
    } else if column >= help_start && column < help_start.saturating_add(6) {
        Some(FooterButton::Help)
    } else {
        None
    }
}

fn render_help(frame: &mut ratatui::Frame<'_>, area: Rect) {
    let popup = help_rect(area);
    let text = vec![
        Line::from("j/k ↑/↓  move"),
        Line::from("⏎        open / focus"),
        Line::from("m        item menu"),
        Line::from("N        projects"),
        Line::from("i        inspect repo"),
        Line::from("W        new workspace"),
        Line::from("P        promote selected"),
        Line::from("R        rebuild projects"),
        Line::from("s / a    sessions / agents"),
        Line::from("n        next attention"),
        Line::from("d        details"),
        Line::from("r        reload"),
        Line::from(""),
        Line::from("? / esc  close help"),
        Line::from("esc ^C ^D close popup"),
    ];
    frame.render_widget(Clear, popup);
    frame.render_widget(
        Paragraph::new(text)
            .block(Block::default().title(" shortcuts ").borders(Borders::ALL))
            .alignment(Alignment::Left),
        popup,
    );
}

fn help_rect(area: Rect) -> Rect {
    let width = area.width.saturating_sub(2).min(34).max(1);
    let height = area.height.saturating_sub(2).min(18).max(1);
    Rect::new(
        area.x + area.width.saturating_sub(width) / 2,
        area.y + area.height.saturating_sub(height) / 2,
        width,
        height,
    )
}

fn rect_contains(rect: Rect, column: u16, row: u16) -> bool {
    column >= rect.x
        && column < rect.x.saturating_add(rect.width)
        && row >= rect.y
        && row < rect.y.saturating_add(rect.height)
}

fn agent_kind_name(kind: crate::model::AgentKind) -> &'static str {
    match kind {
        crate::model::AgentKind::Codex => "codex",
        crate::model::AgentKind::Claude => "claude",
        crate::model::AgentKind::Trae => "trae",
        crate::model::AgentKind::Opencode => "opencode",
    }
}

fn source_health(agent: &AgentSnapshot) -> String {
    let source = match agent.state_source {
        StateSource::Hook => "hook",
        StateSource::Process => "process",
        StateSource::Screen => "screen",
        StateSource::None => "none",
    };
    let marker = match agent.hook_health {
        crate::model::HookHealth::Healthy => "",
        crate::model::HookHealth::Missing => " ~",
        crate::model::HookHealth::Stale => " !",
        crate::model::HookHealth::Conflict => " !",
    };
    format!("{source}{marker}")
}

fn aligned_line(
    mut left: String,
    right: String,
    width: u16,
    mut left_style: Style,
    mut right_style: Style,
    selected: bool,
) -> Line<'static> {
    let width = usize::from(width);
    let reserved = right.chars().count().saturating_add(1);
    let left_limit = width.saturating_sub(reserved);
    if left.chars().count() > left_limit {
        left = left.chars().take(left_limit.saturating_sub(1)).collect();
        left.push('…');
    }
    let gap = width.saturating_sub(left.chars().count() + right.chars().count());
    if selected {
        left_style = left_style
            .remove_modifier(Modifier::DIM)
            .add_modifier(Modifier::REVERSED);
        right_style = right_style
            .remove_modifier(Modifier::DIM)
            .add_modifier(Modifier::REVERSED);
    }
    Line::from(vec![
        Span::styled(left, left_style),
        Span::styled(" ".repeat(gap), left_style),
        Span::styled(right, right_style),
    ])
}

fn state_color(state: DisplayState) -> Color {
    match state {
        DisplayState::Blocked => Color::Red,
        DisplayState::Working => Color::Yellow,
        DisplayState::Done => Color::Cyan,
        DisplayState::Idle => Color::Green,
        DisplayState::Unknown => Color::DarkGray,
    }
}

fn state_name(state: DisplayState) -> &'static str {
    match state {
        DisplayState::Blocked => "blocked",
        DisplayState::Done => "done",
        DisplayState::Working => "working",
        DisplayState::Idle => "idle",
        DisplayState::Unknown => "checking",
    }
}

fn glyph(state: DisplayState) -> &'static str {
    match state {
        DisplayState::Blocked => "●",
        DisplayState::Done => "●",
        DisplayState::Working => "●",
        DisplayState::Idle => "○",
        DisplayState::Unknown => "·",
    }
}

fn nearest_selectable(rows: &[Row], start: usize) -> Option<usize> {
    (start.min(rows.len().saturating_sub(1))..rows.len())
        .find(|index| {
            matches!(
                rows[*index],
                Row::Session(_) | Row::Agent(_) | Row::Conversation(_, _)
            )
        })
        .or_else(|| {
            (0..start.min(rows.len())).rev().find(|index| {
                matches!(
                    rows[*index],
                    Row::Session(_) | Row::Agent(_) | Row::Conversation(_, _)
                )
            })
        })
}

fn selection_key(row: &Row) -> Option<String> {
    match row {
        Row::Session(session) => Some(format!("session:{}", session.session_id)),
        Row::Agent(agent) => Some(format!("agent:{}", agent.instance_id)),
        Row::Conversation(agent, conversation) => Some(format!(
            "conversation:{}:{}",
            agent.instance_id, conversation.id
        )),
        Row::Section(_) | Row::Detail(_, _) | Row::Spacer => None,
    }
}

fn move_selection(rows: &[Row], selected: &mut usize, direction: i32) {
    let mut index = *selected as i32 + direction;
    while index >= 0 && (index as usize) < rows.len() {
        if matches!(
            rows[index as usize],
            Row::Session(_) | Row::Agent(_) | Row::Conversation(_, _)
        ) {
            *selected = index as usize;
            return;
        }
        index += direction;
    }
}

fn keep_visible(selected: usize, height: usize, total: usize, scroll: &mut usize) {
    if selected < *scroll {
        *scroll = selected;
    }
    if selected >= *scroll + height {
        *scroll = selected + 1 - height;
    }
    *scroll = (*scroll).min(total.saturating_sub(height));
}

fn activate(rows: &[Row], selected: usize) -> Result<bool, Box<dyn std::error::Error>> {
    match rows.get(selected) {
        Some(Row::Session(session)) => {
            let mut command = Command::new(std::env::current_exe()?);
            command.args(["focus", "--session", &session.session_id]);
            if let Ok(source) = std::env::var("TMUX_PANE") {
                command.args(["--source-pane", &source]);
            }
            if let Some(window) = &session.last_active_window_id {
                command.args(["--window", window]);
            }
            if let Some(pane) = &session.last_active_pane_id {
                command.args(["--pane", pane]);
            }
            let status = command
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()?;
            if !status.success() {
                return Err("could not focus selected session".into());
            }
            return Ok(true);
        }
        Some(Row::Agent(agent)) if agent.exited => {
            if let Some(event) = &agent.attention {
                let server = ServerIdentity::discover()?;
                let paths = Paths::discover()?;
                call(
                    &paths.socket_for_server(&server.key),
                    &Request::new("attention.ack", serde_json::json!({"event_id": event.id})),
                    Duration::from_secs(1),
                )?;
            }
        }
        Some(Row::Agent(agent)) => {
            let mut command = Command::new(std::env::current_exe()?);
            command.args([
                "focus",
                "--session",
                &agent.target.session_id,
                "--window",
                &agent.target.window_id,
                "--pane",
                &agent.target.pane_id,
            ]);
            if let Ok(source) = std::env::var("TMUX_PANE") {
                command.args(["--source-pane", &source]);
            }
            let status = command
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()?;
            if !status.success() {
                return Err("could not focus selected Agent".into());
            }
            return Ok(true);
        }
        Some(Row::Conversation(agent, _)) => {
            let mut command = Command::new(std::env::current_exe()?);
            command.args([
                "focus",
                "--session",
                &agent.target.session_id,
                "--window",
                &agent.target.window_id,
                "--pane",
                &agent.target.pane_id,
            ]);
            if let Ok(source) = std::env::var("TMUX_PANE") {
                command.args(["--source-pane", &source]);
            }
            let status = command
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()?;
            if !status.success() {
                return Err("could not focus selected conversation".into());
            }
            return Ok(true);
        }
        _ => {}
    }
    Ok(false)
}

fn show_row_menu(
    rows: &[Row],
    selected: usize,
    anchor: Option<(u16, u16)>,
) -> Result<(), Box<dyn std::error::Error>> {
    match rows.get(selected) {
        Some(Row::Session(session)) if safe_target(&session.session_id, '$') => show_menu(
            "session",
            anchor,
            false,
            &[
                (
                    "Switch",
                    "s",
                    format!("switch-client -t {}", session.session_id),
                ),
                (
                    "New window",
                    "n",
                    format!("new-window -t {}", session.session_id),
                ),
                (
                    "Rename",
                    "r",
                    format!(
                        "command-prompt -p 'Rename session:' \"rename-session -t {} '%%'\"",
                        session.session_id
                    ),
                ),
                (
                    "Close",
                    "x",
                    format!(
                        "confirm-before -p 'Close session?' 'kill-session -t {}'",
                        session.session_id
                    ),
                ),
            ],
        ),
        Some(Row::Agent(agent)) if !agent.exited && safe_target(&agent.target.pane_id, '%') => {
            show_menu(
                "agent",
                anchor,
                false,
                &[
                    (
                        "Focus",
                        "f",
                        format!(
                            "switch-client -t {}; select-window -t {}; select-pane -t {}",
                            agent.target.session_id, agent.target.window_id, agent.target.pane_id
                        ),
                    ),
                    (
                        "Rename pane",
                        "r",
                        format!(
                            "command-prompt -p 'Rename pane:' \"select-pane -t {} -T '%%'\"",
                            agent.target.pane_id
                        ),
                    ),
                    (
                        "Split right",
                        "h",
                        format!("split-window -h -t {}", agent.target.pane_id),
                    ),
                    (
                        "Split down",
                        "v",
                        format!("split-window -v -t {}", agent.target.pane_id),
                    ),
                    (
                        "Zoom",
                        "z",
                        format!("resize-pane -Z -t {}", agent.target.pane_id),
                    ),
                    (
                        "Close pane",
                        "x",
                        format!(
                            "confirm-before -p 'Close pane?' 'kill-pane -t {}'",
                            agent.target.pane_id
                        ),
                    ),
                ],
            )
        }
        _ => Ok(()),
    }
}

fn show_global_menu(
    new_only: bool,
    anchor: Option<(u16, u16)>,
) -> Result<(), Box<dyn std::error::Error>> {
    if new_only {
        return run_session_picker();
    }
    let executable = std::env::current_exe()?.display().to_string();
    let executable = shell_quote(&executable);
    let sidebar_pane = std::env::var("TMUX_PANE")?;
    if !safe_target(&sidebar_pane, '%') {
        return Err("invalid sidebar pane id".into());
    }
    let mut items = vec![
        ("New", "N", "run-shell -b workbench-session-pick".into()),
        ("Inspect repo", "i", "run-shell -b mux-inspect-pick".into()),
        ("New workspace", "W", "run-shell -b ws-new-prompt".into()),
        (
            "Promote selected",
            "P",
            format!("send-keys -t {sidebar_pane} P"),
        ),
        (
            "Rebuild projects",
            "R",
            "run-shell -b gen-tmuxinator-configs".into(),
        ),
        ("Details", "d", format!("send-keys -t {sidebar_pane} d")),
        (
            "Sessions",
            "s",
            format!("run-shell \"{} pick session\"", executable),
        ),
        (
            "Agents",
            "a",
            format!("run-shell \"{} pick agent\"", executable),
        ),
        (
            "Next",
            "n",
            format!("run-shell \"{} attention next\"", executable),
        ),
        (
            "Reload",
            "r",
            format!("run-shell \"{} reload\"", executable),
        ),
    ];
    if popup_mode() {
        items.push((
            "Close popup",
            "q",
            format!("send-keys -t {sidebar_pane} Escape"),
        ));
    }
    show_menu("workbench", anchor, anchor.is_some(), &items)
}

fn run_workbench(args: &[&str]) -> Result<(), Box<dyn std::error::Error>> {
    Command::new(std::env::current_exe()?)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()?;
    Ok(())
}

fn run_session_picker() -> Result<(), Box<dyn std::error::Error>> {
    Command::new("workbench-session-pick")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()?;
    Ok(())
}

fn run_command(program: &str) -> Result<(), Box<dyn std::error::Error>> {
    Command::new(program)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()?;
    Ok(())
}

fn promote_selected(rows: &[Row], selected: usize) -> Result<(), Box<dyn std::error::Error>> {
    let pane = match rows.get(selected) {
        Some(Row::Agent(agent)) | Some(Row::Conversation(agent, _)) => {
            Some(agent.target.pane_id.as_str())
        }
        Some(Row::Session(session)) => session.last_active_pane_id.as_deref(),
        _ => None,
    };
    let Some(pane) = pane.filter(|pane| safe_target(pane, '%')) else {
        return Ok(());
    };
    Command::new("ws-promote")
        .env("WORKBENCH_PROMOTE_PANE", pane)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()?;
    Ok(())
}

fn show_menu(
    title: &str,
    anchor: Option<(u16, u16)>,
    pointer_bottom_right: bool,
    items: &[(&'static str, &'static str, String)],
) -> Result<(), Box<dyn std::error::Error>> {
    let server = ServerIdentity::discover()?;
    let pane = std::env::var("TMUX_PANE")?;
    if !safe_target(&pane, '%') {
        return Err("invalid sidebar pane id".into());
    }
    let (x, y) = menu_position(anchor, pointer_bottom_right);
    let mut args = vec![
        "display-menu".to_owned(),
        // The sidebar, rather than a tmux mouse binding, received the click.
        // Tell tmux to handle subsequent mouse events and ignore the opening
        // button's release so the menu remains available for a normal click.
        "-M".to_owned(),
        "-O".to_owned(),
        "-T".to_owned(),
        title.to_owned(),
        "-t".to_owned(),
        pane,
        "-x".to_owned(),
        x,
        "-y".to_owned(),
        y,
    ];
    for (label, key, command) in items {
        args.extend([(*label).to_owned(), (*key).to_owned(), command.clone()]);
    }
    let refs: Vec<_> = args.iter().map(String::as_str).collect();
    tmux_ui(&server, &refs)
}

fn menu_position(anchor: Option<(u16, u16)>, pointer_bottom_right: bool) -> (String, String) {
    match anchor {
        Some((column, row)) if pointer_bottom_right => (
            format!("#{{e|+:#{{e|-:#{{e|+:#{{popup_pane_left}},{column}}},#{{popup_width}}}},1}}"),
            format!("#{{e|+:#{{e|-:#{{e|+:#{{popup_pane_top}},{row}}},#{{popup_height}}}},1}}"),
        ),
        // Sidebar panes are full-height. Keep x pinned to the pane's left edge
        // while placing a contextual menu beside the selected viewport row.
        Some((_, row)) => ("P".into(), row.to_string()),
        // Global menus are right-aligned inside the sidebar pane. pane_right
        // is inclusive, hence the final +1 after subtracting popup_width.
        None => (
            "#{e|+:#{e|-:#{popup_pane_right},#{popup_width}},1}".into(),
            "P".into(),
        ),
    }
}

fn tmux_ui(server: &ServerIdentity, args: &[&str]) -> Result<(), Box<dyn std::error::Error>> {
    let output = Command::new("tmux")
        .args(server.tmux_args())
        .args(args)
        .output()?;
    if output.status.success() {
        Ok(())
    } else {
        Err(String::from_utf8_lossy(&output.stderr)
            .trim()
            .to_owned()
            .into())
    }
}

fn safe_target(value: &str, prefix: char) -> bool {
    value
        .strip_prefix(prefix)
        .is_some_and(|rest| !rest.is_empty() && rest.bytes().all(|byte| byte.is_ascii_digit()))
}

fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ipc::{Response, read_request, write_response};
    use crate::model::SessionSnapshot;
    use std::os::unix::net::UnixListener;

    #[test]
    fn sessions_without_agents_remain_navigable() {
        let snapshot = Snapshot {
            schema_version: crate::SNAPSHOT_SCHEMA_VERSION,
            server: "s".into(),
            generation: 1,
            observed_at_unix_ms: 1,
            sessions: vec![SessionSnapshot {
                session_id: "$1".into(),
                session_name: "shell".into(),
                rollup_state: DisplayState::Unknown,
                agent_count: 0,
                attention_count: 0,
                active: false,
                last_active_window_id: None,
                last_active_pane_id: None,
            }],
            agents: vec![],
            clients: vec![],
        };
        assert!(
            build_rows(&snapshot, false)
                .iter()
                .any(|row| matches!(row, Row::Session(session) if session.session_id == "$1"))
        );
    }

    #[test]
    fn attention_partition_preserves_tmux_session_order() {
        let session = |id: &str, attention_count| SessionSnapshot {
            session_id: id.into(),
            session_name: id.into(),
            rollup_state: DisplayState::Unknown,
            agent_count: 1,
            attention_count,
            active: false,
            last_active_window_id: None,
            last_active_pane_id: None,
        };
        let snapshot = Snapshot {
            schema_version: crate::SNAPSHOT_SCHEMA_VERSION,
            server: "s".into(),
            generation: 1,
            observed_at_unix_ms: 1,
            sessions: vec![
                session("$2", 0),
                session("$3", 1),
                session("$10", 0),
                session("$11", 2),
            ],
            agents: vec![],
            clients: vec![],
        };
        let sessions: Vec<_> = build_rows(&snapshot, false)
            .into_iter()
            .filter_map(|row| match row {
                Row::Session(session) => Some(session.session_id),
                Row::Section(_)
                | Row::Agent(_)
                | Row::Detail(_, _)
                | Row::Conversation(_, _)
                | Row::Spacer => None,
            })
            .collect();
        assert_eq!(sessions, ["$3", "$11", "$2", "$10"]);
    }

    #[test]
    fn refresh_uses_only_the_snapshot_socket_contract() {
        let temp = tempfile::tempdir().unwrap();
        let socket = temp.path().join("daemon.sock");
        let listener = UnixListener::bind(&socket).unwrap();
        let server = std::thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            let request = read_request(&stream).unwrap();
            assert_eq!(request.method, "snapshot.get");
            let snapshot = Snapshot::empty("fixture", 1);
            write_response(
                &stream,
                &Response::success(request.id, serde_json::to_value(snapshot).unwrap()),
            )
            .unwrap();
        });
        let snapshot = fetch_snapshot(&socket).unwrap();
        assert_eq!(snapshot.server, "fixture");
        server.join().unwrap();
    }

    #[test]
    fn orphan_exit_tombstone_remains_visible_without_a_live_session() {
        let mut snapshot: Snapshot =
            serde_json::from_str(include_str!("../tests/golden/snapshot-v1.json")).unwrap();
        snapshot.sessions.clear();
        snapshot.agents[0].exited = true;
        snapshot.agents[0].process = None;
        let rows = build_rows(&snapshot, false);
        assert!(rows.iter().any(|row| matches!(row, Row::Agent(_))));
    }

    #[test]
    fn hook_conflict_uses_a_compact_warning_marker() {
        let mut snapshot: Snapshot =
            serde_json::from_str(include_str!("../tests/golden/snapshot-v1.json")).unwrap();
        snapshot.agents[0].hook_health = crate::model::HookHealth::Conflict;
        let line = render_row(&Row::Agent(snapshot.agents[0].clone()), false, 40);
        let rendered: String = line
            .spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect();
        assert!(rendered.ends_with(" !"));
        assert!(!rendered.contains("conflict"));
    }

    #[test]
    fn estimated_marker_is_detail_only() {
        let mut snapshot: Snapshot =
            serde_json::from_str(include_str!("../tests/golden/snapshot-v1.json")).unwrap();
        let agent = &mut snapshot.agents[0];
        agent.state_source = StateSource::Screen;
        agent.hook_health = crate::model::HookHealth::Missing;

        let compact = render_row(&Row::Agent(agent.clone()), false, 40);
        let compact_text: String = compact
            .spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect();
        assert!(!compact_text.ends_with(" ~"));
        assert_eq!(source_health(agent), "screen ~");
    }

    #[test]
    fn detail_mode_expands_safe_agent_metadata() {
        let snapshot: Snapshot =
            serde_json::from_str(include_str!("../tests/golden/snapshot-v1.json")).unwrap();
        let compact = build_rows(&snapshot, false);
        let detailed = build_rows(&snapshot, true);
        let location = format!(
            "{}:{}",
            snapshot.agents[0].target.window_index, snapshot.agents[0].target.pane_index
        );
        assert!(detailed.len() > compact.len());
        assert!(detailed.iter().any(
            |row| matches!(row, Row::Detail(left, right) if left.contains(&location) && left.contains("pid") && right.is_empty())
        ));
        assert!(detailed.iter().any(
            |row| matches!(row, Row::Detail(left, right) if ["hook", "process", "screen", "none"].iter().any(|source| left.contains(source)) && right.starts_with("rule "))
        ));
    }

    #[test]
    fn conversations_expand_below_their_single_agent() {
        let mut snapshot: Snapshot =
            serde_json::from_str(include_str!("../tests/golden/snapshot-v1.json")).unwrap();
        snapshot.agents[0].conversations = vec![ConversationSnapshot {
            id: "side-1".into(),
            role: crate::model::ConversationRole::Side,
            label: "side".into(),
            base_state: crate::model::BaseState::Working,
            display_state: DisplayState::Working,
            reason_category: None,
            active: true,
            stale: false,
        }];
        let rows = build_rows(&snapshot, false);
        assert_eq!(
            rows.iter()
                .filter(|row| matches!(row, Row::Agent(_)))
                .count(),
            1
        );
        assert_eq!(
            rows.iter()
                .filter(|row| matches!(row, Row::Conversation(_, _)))
                .count(),
            1
        );
    }

    #[test]
    fn menu_targets_reject_command_injection() {
        assert!(safe_target("$12", '$'));
        assert!(safe_target("%34", '%'));
        assert!(!safe_target("%1; kill-server", '%'));
    }

    #[test]
    fn contextual_menu_tracks_the_selected_viewport_row() {
        assert_eq!(
            menu_position(Some((7, 12)), false),
            ("P".into(), "12".into())
        );
        assert_eq!(
            menu_position(Some((7, 12)), true),
            (
                "#{e|+:#{e|-:#{e|+:#{popup_pane_left},7},#{popup_width}},1}".into(),
                "#{e|+:#{e|-:#{e|+:#{popup_pane_top},12},#{popup_height}},1}".into()
            )
        );
        assert_eq!(
            menu_position(None, false),
            (
                "#{e|+:#{e|-:#{popup_pane_right},#{popup_width}},1}".into(),
                "P".into()
            )
        );
    }
}
