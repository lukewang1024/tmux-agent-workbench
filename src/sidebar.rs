use std::ffi::{OsStr, OsString};
use std::io::{self, Write, stdout};
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

use crate::config::{AgentSort, Config};
use crate::ipc::{Request, call};
use crate::model::{
    AgentSnapshot, ConversationSnapshot, DisplayState, SessionSnapshot, Snapshot, StateSource,
};
use crate::paths::Paths;
use crate::server::ServerIdentity;

#[derive(Debug, Clone)]
enum Row {
    Section(&'static str),
    AgentSection(AgentSort),
    Session(SessionSnapshot),
    SessionSub(SessionSnapshot),
    Agent(AgentSnapshot),
    AgentSub(AgentSnapshot),
    Detail(String, String),
    Conversation(AgentSnapshot, ConversationSnapshot),
    Actions,
    Spacer,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FooterButton {
    New,
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
    if let Err(error) = &result {
        let log = paths.state_dir.join("popup-actions.log");
        if let Ok(mut file) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(log)
        {
            let client =
                std::env::var("WORKBENCH_TARGET_CLIENT").unwrap_or_else(|_| "unknown".into());
            let _ = writeln!(file, "sidebar-error client={client} error={error}");
        }
    }
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
    let mut agent_sort = Config::load(&paths.config_file())
        .map(|config| config.sidebar.agent_sort)
        .unwrap_or_default();
    if let Some(saved) = persisted_agent_sort(paths) {
        agent_sort = saved;
    }
    let mut help_visible = false;
    let mut footer_hover = None;
    let mut selected = 0_usize;
    let mut selection_visible = true;
    let mut scroll = 0_usize;
    let mut disconnected = true;
    let mut last_success = None;
    let mut next_refresh = Instant::now();
    let mut next_sort_sync = Instant::now() + Duration::from_millis(200);
    let mut dirty = true;
    let mut initial_selection = true;
    let mut last_content_height = usize::MAX;
    loop {
        if let Ok(result) = snapshot_rx.try_recv() {
            match result {
                Ok(fetched) => {
                    let selected_key = rows.get(selected).and_then(selection_key);
                    rows = build_rows(&fetched, detailed, agent_sort);
                    if last_content_height != usize::MAX {
                        rows = balance_sections(rows, last_content_height);
                    }
                    disconnected = false;
                    last_success = Some(Instant::now());
                    selected = if initial_selection {
                        initial_selection = false;
                        current_agent_selection(&rows, &fetched, server)
                            .or_else(|| nearest_selectable(&rows, 0))
                            .unwrap_or(0)
                    } else {
                        selected_key
                            .as_deref()
                            .and_then(|key| nearest_matching_key(&rows, key, selected))
                            .or_else(|| nearest_selectable(&rows, selected))
                            .unwrap_or(0)
                    };
                    snapshot = Some(fetched);
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
        if Instant::now() >= next_sort_sync {
            next_sort_sync = Instant::now() + Duration::from_millis(200);
            if let Some(saved) = persisted_agent_sort(paths)
                && saved != agent_sort
            {
                let selected_key = rows.get(selected).and_then(selection_key);
                agent_sort = saved;
                if let Some(snapshot) = &snapshot {
                    rows = balance_sections(
                        build_rows(snapshot, detailed, agent_sort),
                        last_content_height,
                    );
                    selected = selected_key
                        .as_deref()
                        .and_then(|key| nearest_matching_key(&rows, key, selected))
                        .or_else(|| nearest_selectable(&rows, selected))
                        .unwrap_or(0);
                }
                dirty = true;
            }
        }

        let size = terminal.size()?;
        let body_height = usize::from(size.height).max(1);
        let footer_height = usize::from(popup_mode());
        let content_height = body_height.saturating_sub(footer_height);
        if content_height != last_content_height {
            last_content_height = content_height;
            if let Some(snapshot) = &snapshot {
                let selected_key = rows.get(selected).and_then(selection_key);
                rows = balance_sections(build_rows(snapshot, detailed, agent_sort), content_height);
                selected = selected_key
                    .as_deref()
                    .and_then(|key| nearest_matching_key(&rows, key, selected))
                    .or_else(|| nearest_selectable(&rows, selected))
                    .unwrap_or(0);
                dirty = true;
            }
        }
        let viewport_height = if rows.len() > content_height {
            content_height.saturating_sub(1)
        } else {
            content_height
        };
        if dirty {
            keep_visible(selected, viewport_height, rows.len(), &mut scroll);
            let selected_key = selection_visible
                .then(|| rows.get(selected).and_then(selection_key))
                .flatten();
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
                    for row in rows.iter().skip(scroll).take(viewport_height) {
                        lines.push(if matches!(row, Row::Actions) {
                            render_actions(area.width, footer_hover)
                        } else {
                            render_row(
                                row,
                                selected_key
                                    .as_deref()
                                    .is_some_and(|key| selection_key(row).as_deref() == Some(key)),
                                area.width,
                            )
                        });
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
                if popup_mode() {
                    lines.push(render_close(area.width, footer_hover));
                }
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
            Event::FocusGained => {
                selection_visible = true;
                if let Some(snapshot) = &snapshot
                    && let Some(current) = current_agent_selection(&rows, snapshot, server)
                {
                    selected = current;
                }
            }
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
                    KeyCode::Char('m') => {
                        if show_row_menu(
                            &rows,
                            selected,
                            Some((0, selected.saturating_sub(scroll) as u16)),
                        )? {
                            return Ok(());
                        }
                    }
                    KeyCode::Char('d') => {
                        let selected_key = rows.get(selected).and_then(selection_key);
                        detailed = !detailed;
                        if let Some(snapshot) = &snapshot {
                            rows = balance_sections(
                                build_rows(snapshot, detailed, agent_sort),
                                content_height,
                            );
                            selected = selected_key
                                .as_deref()
                                .and_then(|key| nearest_matching_key(&rows, key, selected))
                                .or_else(|| nearest_selectable(&rows, selected))
                                .unwrap_or(0);
                        }
                    }
                    KeyCode::Char('N') => {
                        if run_session_picker()? {
                            return Ok(());
                        }
                    }
                    KeyCode::Char('i') => {
                        if run_command("mux-inspect-pick", true)? {
                            return Ok(());
                        }
                    }
                    KeyCode::Char('W') => {
                        if run_command("ws-new-prompt", true)? {
                            return Ok(());
                        }
                    }
                    KeyCode::Char('P') if selection_visible => promote_selected(&rows, selected)?,
                    KeyCode::Char('R') => {
                        run_command("gen-tmuxinator-configs", false)?;
                    }
                    KeyCode::Char('n') => {
                        if run_workbench(&["attention", "next"], true)? {
                            return Ok(());
                        }
                    }
                    KeyCode::Char('s') => {
                        if run_workbench(&["pick", "session"], true)? {
                            return Ok(());
                        }
                    }
                    KeyCode::Char('a') => {
                        if run_workbench(&["pick", "agent"], true)? {
                            return Ok(());
                        }
                    }
                    KeyCode::Char('r') => {
                        run_workbench(&["reload"], false)?;
                    }
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
                        let row = scroll + usize::from(mouse.row);
                        footer_hover = if matches!(rows.get(row), Some(Row::Actions)) {
                            action_button(size.width, mouse.column)
                        } else if popup_mode()
                            && usize::from(mouse.row) == body_height.saturating_sub(1)
                        {
                            close_button(size.width, mouse.column)
                        } else {
                            None
                        };
                        if matches!(
                            rows.get(row),
                            Some(
                                Row::Session(_)
                                    | Row::SessionSub(_)
                                    | Row::Agent(_)
                                    | Row::AgentSub(_)
                                    | Row::Conversation(_, _)
                            )
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
                        if popup_mode()
                            && usize::from(mouse.row) == body_height.saturating_sub(1)
                            && close_button(size.width, mouse.column).is_some()
                        {
                            return Ok(());
                        }
                        let clicked = scroll + usize::from(mouse.row);
                        if let Some(Row::AgentSection(sort)) = rows.get(clicked)
                            && agent_sort_button(size.width, *sort, mouse.column)
                        {
                            agent_sort = match agent_sort {
                                AgentSort::Grouped => AgentSort::Prioritized,
                                AgentSort::Prioritized => AgentSort::Grouped,
                            };
                            persist_agent_sort(paths, agent_sort)?;
                            if let Some(snapshot) = &snapshot {
                                rows = balance_sections(
                                    build_rows(snapshot, detailed, agent_sort),
                                    content_height,
                                );
                                selected = nearest_selectable(&rows, selected).unwrap_or(0);
                            }
                            continue;
                        }
                        if matches!(rows.get(clicked), Some(Row::Actions)) {
                            match action_button(size.width, mouse.column) {
                                Some(FooterButton::New) => {
                                    if run_session_picker()? {
                                        return Ok(());
                                    }
                                }
                                Some(FooterButton::Menu) => {
                                    if show_global_menu(false, Some((mouse.column, mouse.row)))? {
                                        return Ok(());
                                    }
                                }
                                _ => {}
                            }
                            continue;
                        }
                        if matches!(
                            rows.get(clicked),
                            Some(
                                Row::Session(_)
                                    | Row::SessionSub(_)
                                    | Row::Agent(_)
                                    | Row::AgentSub(_)
                                    | Row::Conversation(_, _)
                            )
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
                            Some(
                                Row::Session(_)
                                    | Row::SessionSub(_)
                                    | Row::Agent(_)
                                    | Row::AgentSub(_)
                                    | Row::Conversation(_, _)
                            )
                        ) {
                            selected = clicked;
                            if show_row_menu(&rows, selected, Some((mouse.column, mouse.row)))? {
                                return Ok(());
                            }
                        }
                    }
                    _ => {}
                }
            }
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

fn build_rows(snapshot: &Snapshot, detailed: bool, agent_sort: AgentSort) -> Vec<Row> {
    let mut rows = vec![Row::Section("sessions")];
    let mut sessions = snapshot.sessions.clone();
    sessions.sort_by_key(stable_session_key);
    for session in sessions {
        rows.push(Row::Session(session.clone()));
        rows.push(Row::SessionSub(session));
    }
    rows.push(Row::Spacer);
    rows.push(Row::Actions);
    rows.push(Row::Spacer);
    rows.push(Row::AgentSection(agent_sort));
    let mut agents = snapshot.agents.clone();
    agents.sort_by_key(stable_agent_key);
    if agent_sort == AgentSort::Prioritized {
        agents.sort_by_key(|agent| {
            (
                std::cmp::Reverse(agent_attention_priority(agent)),
                std::cmp::Reverse(
                    agent
                        .attention
                        .as_ref()
                        .and_then(|event| event.attention_seq)
                        .unwrap_or(0),
                ),
            )
        });
    }
    for agent in agents {
        rows.push(Row::Agent(agent.clone()));
        rows.push(Row::AgentSub(agent.clone()));
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

fn balance_sections(mut rows: Vec<Row>, content_height: usize) -> Vec<Row> {
    let Some(actions_index) = rows.iter().position(|row| matches!(row, Row::Actions)) else {
        return rows;
    };
    let actions_start = content_height / 2;
    if actions_index < actions_start {
        rows.splice(
            actions_index..actions_index,
            std::iter::repeat_n(Row::Spacer, actions_start - actions_index),
        );
    }
    rows
}

fn agent_attention_priority(agent: &AgentSnapshot) -> u8 {
    match agent.display_state {
        DisplayState::Blocked => 4,
        DisplayState::Done => 3,
        DisplayState::Working => 2,
        DisplayState::Idle => 1,
        DisplayState::Unknown => 0,
    }
}

fn persist_agent_sort(paths: &Paths, sort: AgentSort) -> io::Result<()> {
    std::fs::create_dir_all(&paths.state_dir)?;
    std::fs::write(
        paths.state_dir.join("sidebar-agent-sort"),
        match sort {
            AgentSort::Grouped => "grouped\n",
            AgentSort::Prioritized => "prioritized\n",
        },
    )
}

fn persisted_agent_sort(paths: &Paths) -> Option<AgentSort> {
    let saved = std::fs::read_to_string(paths.state_dir.join("sidebar-agent-sort")).ok()?;
    match saved.trim() {
        "grouped" => Some(AgentSort::Grouped),
        "prioritized" => Some(AgentSort::Prioritized),
        _ => None,
    }
}

fn stable_agent_key(agent: &AgentSnapshot) -> (String, u32, u32, String) {
    (
        agent.target.session_name.clone(),
        agent.target.window_index,
        agent.target.pane_index,
        agent.instance_id.clone(),
    )
}

fn stable_session_key(session: &SessionSnapshot) -> (String, String) {
    (session.session_name.clone(), session.session_id.clone())
}

fn current_agent_selection(
    rows: &[Row],
    snapshot: &Snapshot,
    server: &ServerIdentity,
) -> Option<usize> {
    let instance = current_agent_instance(snapshot, server)?;
    // Prefer the canonical Agent row when restoring focus after a refresh.
    rows.iter()
        .rposition(|row| matches!(row, Row::Agent(agent) if agent.instance_id == instance))
}

fn current_agent_instance(snapshot: &Snapshot, server: &ServerIdentity) -> Option<String> {
    let source = source_pane()?;
    if let Some(agent) = snapshot
        .agents
        .iter()
        .find(|agent| agent.target.pane_id == source)
    {
        return Some(agent.instance_id.clone());
    }

    let output = Command::new("tmux")
        .arg("-S")
        .arg(&server.socket_path)
        .args([
            "list-panes",
            "-t",
            &source,
            "-F",
            "#{pane_id}\u{1f}#{pane_last}\u{1f}#{@pane_role}",
        ])
        .stderr(Stdio::null())
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let previous = String::from_utf8_lossy(&output.stdout)
        .lines()
        .find_map(|line| {
            let mut fields = line.split('\u{1f}');
            let pane = fields.next()?;
            let was_last = fields.next()? == "1";
            let role = fields.next().unwrap_or_default();
            (was_last && role != "sidebar").then(|| pane.to_owned())
        })?;
    snapshot
        .agents
        .iter()
        .find(|agent| agent.target.pane_id == previous)
        .map(|agent| agent.instance_id.clone())
}

fn render_row(row: &Row, selected: bool, width: u16) -> Line<'static> {
    match row {
        Row::Section(label) => Line::from(Span::styled(
            *label,
            Style::default()
                .add_modifier(Modifier::BOLD)
                .add_modifier(Modifier::DIM),
        )),
        Row::AgentSection(sort) => aligned_line(
            "agents".into(),
            match sort {
                AgentSort::Grouped => "grouped",
                AgentSort::Prioritized => "prioritized",
            }
            .into(),
            width,
            Style::default()
                .add_modifier(Modifier::BOLD)
                .add_modifier(Modifier::DIM),
            Style::default().fg(Color::Cyan),
            false,
        ),
        Row::Session(session) => aligned_line(
            format!(" {} {}", glyph(session.rollup_state), session.session_name),
            String::new(),
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
        ),
        Row::SessionSub(session) => aligned_line(
            format!("   {}", session_context(session)),
            String::new(),
            width,
            if selected {
                Style::default().fg(Color::Rgb(235, 235, 245))
            } else {
                Style::default().fg(Color::Rgb(150, 150, 170))
            },
            if selected {
                Style::default().fg(Color::Rgb(235, 235, 245))
            } else {
                Style::default().fg(Color::Rgb(150, 150, 170))
            },
            selected,
        ),
        Row::Agent(agent) => {
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
                    " {} {}",
                    glyph(agent.display_state),
                    agent.target.session_name
                ),
                String::new(),
                width,
                primary,
                Style::default(),
                selected,
            )
        }
        Row::AgentSub(agent) => aligned_line(
            format!(
                "   {} · {}",
                agent_status(agent),
                agent_kind_name(agent.kind)
            ),
            String::new(),
            width,
            Style::default().fg(state_color(agent.display_state)),
            Style::default()
                .fg(state_color(agent.display_state))
                .add_modifier(Modifier::DIM),
            selected,
        ),
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
        Row::Actions | Row::Spacer => Line::default(),
    }
}

fn popup_mode() -> bool {
    std::env::var_os("WORKBENCH_POPUP").is_some()
}

fn source_pane() -> Option<String> {
    std::env::var("WORKBENCH_SOURCE_PANE")
        .or_else(|_| std::env::var("TMUX_PANE"))
        .ok()
        .filter(|pane| safe_target(pane, '%'))
}

fn button_style(button: FooterButton, hovered: Option<FooterButton>) -> Style {
    let style = Style::default().fg(Color::Cyan);
    if hovered == Some(button) {
        style.add_modifier(Modifier::REVERSED)
    } else {
        style
    }
}

fn render_actions(width: u16, hovered: Option<FooterButton>) -> Line<'static> {
    let gap = usize::from(width).saturating_sub(11);
    Line::from(vec![
        Span::styled("+ new", button_style(FooterButton::New, hovered)),
        Span::raw(" ".repeat(gap)),
        Span::styled("⋯ menu", button_style(FooterButton::Menu, hovered)),
    ])
}

fn render_close(width: u16, hovered: Option<FooterButton>) -> Line<'static> {
    let gap = usize::from(width).saturating_sub(7);
    Line::from(vec![
        Span::raw(" ".repeat(gap)),
        Span::styled("× close", button_style(FooterButton::Close, hovered)),
    ])
}

fn action_button(width: u16, column: u16) -> Option<FooterButton> {
    if column < 5 {
        Some(FooterButton::New)
    } else if column >= width.saturating_sub(6) {
        Some(FooterButton::Menu)
    } else {
        None
    }
}

fn agent_sort_button(width: u16, sort: AgentSort, column: u16) -> bool {
    let label_width = match sort {
        AgentSort::Grouped => 7,
        AgentSort::Prioritized => 11,
    };
    column >= width.saturating_sub(label_width)
}

fn close_button(width: u16, column: u16) -> Option<FooterButton> {
    (column >= width.saturating_sub(7)).then_some(FooterButton::Close)
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

fn agent_status(agent: &AgentSnapshot) -> String {
    let mut status = if agent.display_state == DisplayState::Blocked {
        agent.reason_category.as_deref().unwrap_or("blocked")
    } else {
        state_name(agent.display_state)
    }
    .to_owned();
    match agent.hook_health {
        crate::model::HookHealth::Missing => status.push_str(" ~"),
        crate::model::HookHealth::Stale | crate::model::HookHealth::Conflict => {
            status.push_str(" !")
        }
        crate::model::HookHealth::Healthy => {}
    }
    status
}

fn session_context(session: &SessionSnapshot) -> String {
    let mut parts = Vec::new();
    if let Some(path) = session
        .current_path
        .as_deref()
        .filter(|path| !path.is_empty())
    {
        let display = std::path::Path::new(path)
            .file_name()
            .and_then(|name| name.to_str())
            .filter(|name| !name.is_empty())
            .unwrap_or(path);
        if !display.eq_ignore_ascii_case(&session.session_name) {
            parts.push(display.to_owned());
        }
        if let Ok(output) = Command::new("git")
            .args(["-C", path, "branch", "--show-current"])
            .stderr(Stdio::null())
            .output()
            && output.status.success()
        {
            let branch = String::from_utf8_lossy(&output.stdout).trim().to_owned();
            if !branch.is_empty()
                && !branch.eq_ignore_ascii_case(&session.session_name)
                && !parts.iter().any(|part| part.eq_ignore_ascii_case(&branch))
            {
                parts.push(branch);
            }
        }
    }
    parts.push(format!(
        "{} agent{}",
        session.agent_count,
        if session.agent_count == 1 { "" } else { "s" }
    ));
    parts.join(" · ")
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
            .bg(Color::DarkGray);
        right_style = right_style
            .remove_modifier(Modifier::DIM)
            .bg(Color::DarkGray);
        if left_style.fg == Some(Color::DarkGray) {
            left_style = left_style.fg(Color::White);
        }
        if right_style.fg == Some(Color::DarkGray) {
            right_style = right_style.fg(Color::White);
        }
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
        Row::SessionSub(session) => Some(format!("session:{}", session.session_id)),
        Row::Agent(agent) => Some(format!("agent:{}", agent.instance_id)),
        Row::AgentSub(agent) => Some(format!("agent:{}", agent.instance_id)),
        Row::Conversation(agent, conversation) => Some(format!(
            "conversation:{}:{}",
            agent.instance_id, conversation.id
        )),
        Row::Section(_) | Row::AgentSection(_) | Row::Detail(_, _) | Row::Actions | Row::Spacer => {
            None
        }
    }
}

fn nearest_matching_key(rows: &[Row], key: &str, previous: usize) -> Option<usize> {
    rows.iter()
        .enumerate()
        .filter(|(_, row)| selection_key(row).as_deref() == Some(key))
        .min_by_key(|(index, _)| index.abs_diff(previous))
        .map(|(index, _)| index)
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
        Some(Row::Session(session) | Row::SessionSub(session)) => {
            let mut command = Command::new(std::env::current_exe()?);
            command.args(["focus", "--session", &session.session_id]);
            if let Some(source) = source_pane() {
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
        Some(Row::Agent(agent) | Row::AgentSub(agent)) if agent.exited => {
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
        Some(Row::Agent(agent) | Row::AgentSub(agent)) => {
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
            if let Some(source) = source_pane() {
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
            if let Some(source) = source_pane() {
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
) -> Result<bool, Box<dyn std::error::Error>> {
    match rows.get(selected) {
        Some(Row::Session(session) | Row::SessionSub(session))
            if safe_target(&session.session_id, '$') =>
        {
            show_menu(
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
            )
        }
        Some(Row::Agent(agent) | Row::AgentSub(agent))
            if !agent.exited && safe_target(&agent.target.pane_id, '%') =>
        {
            show_menu(
                "agent",
                anchor,
                false,
                &[
                    (
                        "Focus",
                        "f",
                        format!(
                            "switch-client -t {}; select-window -t {}; select-pane -Z -t {}",
                            agent.target.session_id, agent.target.window_id, agent.target.pane_id
                        ),
                    ),
                    (
                        "Rename pane",
                        "r",
                        format!(
                            "command-prompt -p 'Rename pane:' \"select-pane -Z -t {} -T '%%'\"",
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
        _ => Ok(false),
    }
}

fn show_global_menu(
    new_only: bool,
    anchor: Option<(u16, u16)>,
) -> Result<bool, Box<dyn std::error::Error>> {
    if new_only {
        return run_session_picker();
    }
    let executable = std::env::current_exe()?.display().to_string();
    let executable = shell_quote(&executable);
    let sidebar_pane = source_pane().ok_or("sidebar source pane unavailable")?;
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
        // Once this popup closes there is no sidebar pane to receive these
        // synthetic keys. The direct keyboard shortcuts remain available in
        // the TUI; omit only the menu entries whose target would be stale.
        items.retain(|(label, _, _)| !matches!(*label, "Promote selected" | "Details"));
    }
    show_menu("workbench", anchor, anchor.is_some(), &items)
}

fn run_workbench(args: &[&str], clear_popup: bool) -> Result<bool, Box<dyn std::error::Error>> {
    let executable = std::env::current_exe()?;
    dispatch_command(&executable, args.iter().map(OsStr::new), clear_popup)
}

fn run_session_picker() -> Result<bool, Box<dyn std::error::Error>> {
    dispatch_command("workbench-session-pick", std::iter::empty::<&OsStr>(), true)
}

fn run_command(program: &str, clear_popup: bool) -> Result<bool, Box<dyn std::error::Error>> {
    dispatch_command(program, std::iter::empty::<&OsStr>(), clear_popup)
}

fn dispatch_command<I, S>(
    program: impl AsRef<OsStr>,
    args: I,
    clear_popup: bool,
) -> Result<bool, Box<dyn std::error::Error>>
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    let args: Vec<OsString> = args
        .into_iter()
        .map(|arg| arg.as_ref().to_os_string())
        .collect();
    if popup_mode() && clear_popup {
        // tmux permits only one popup/menu per client. Let this process exit so
        // the responsive sidebar popup is gone, then launch the requested UI
        // in the same inherited tmux client context. Positional shell arguments
        // avoid quoting command paths or user-controlled values.
        let client = current_client()?;
        let mut deferred = format!(
            "WORKBENCH_TARGET_CLIENT={} workbench-popup-action {} {}",
            shell_quote(&client),
            std::process::id(),
            shell_quote(&program.as_ref().to_string_lossy()),
        );
        for arg in &args {
            deferred.push(' ');
            deferred.push_str(&shell_quote(&arg.to_string_lossy()));
        }
        // tmux owns this background job, so closing the current popup cannot
        // kill the deferred action with the popup's process group.
        Command::new("tmux")
            .args(["run-shell", "-b", &deferred])
            .status()?
            .success()
            .then_some(())
            .ok_or("could not queue popup action")?;
        return Ok(true);
    }
    Command::new(program.as_ref())
        .args(&args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()?;
    Ok(false)
}

fn current_client() -> Result<String, Box<dyn std::error::Error>> {
    if let Ok(client) = std::env::var("WORKBENCH_TARGET_CLIENT")
        && !client.is_empty()
    {
        return Ok(client);
    }
    let output = Command::new("tmux")
        .args(["display-message", "-p", "#{client_name}"])
        .output()?;
    let client = String::from_utf8(output.stdout)?.trim().to_owned();
    if !output.status.success() || client.is_empty() {
        return Err("could not resolve popup client".into());
    }
    Ok(client)
}

fn promote_selected(rows: &[Row], selected: usize) -> Result<(), Box<dyn std::error::Error>> {
    let pane = match rows.get(selected) {
        Some(Row::Agent(agent) | Row::AgentSub(agent)) | Some(Row::Conversation(agent, _)) => {
            Some(agent.target.pane_id.as_str())
        }
        Some(Row::Session(session) | Row::SessionSub(session)) => {
            session.last_active_pane_id.as_deref()
        }
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
) -> Result<bool, Box<dyn std::error::Error>> {
    let server = ServerIdentity::discover()?;
    let pane = source_pane().ok_or("sidebar source pane unavailable")?;
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
        "-c".to_owned(),
        current_client()?,
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
    if popup_mode() {
        let mut command_args: Vec<OsString> = server
            .tmux_args()
            .into_iter()
            .map(|arg| arg.as_os_str().to_os_string())
            .collect();
        command_args.extend(args.into_iter().map(OsString::from));
        dispatch_command("tmux", &command_args, true)
    } else {
        let refs: Vec<_> = args.iter().map(String::as_str).collect();
        tmux_ui(&server, &refs)?;
        Ok(false)
    }
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

    fn test_paths(root: &std::path::Path) -> Paths {
        Paths {
            config_dir: root.join("config"),
            state_dir: root.join("state"),
            cache_dir: root.join("cache"),
            runtime_dir: root.join("runtime"),
        }
    }

    #[test]
    fn agent_sort_state_is_shared_between_sidebar_instances() {
        let temp = tempfile::tempdir().unwrap();
        let first = test_paths(temp.path());
        let second = test_paths(temp.path());

        persist_agent_sort(&first, AgentSort::Prioritized).unwrap();
        assert_eq!(persisted_agent_sort(&second), Some(AgentSort::Prioritized));

        persist_agent_sort(&second, AgentSort::Grouped).unwrap();
        assert_eq!(persisted_agent_sort(&first), Some(AgentSort::Grouped));
    }

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
                current_path: None,
                active: false,
                last_active_window_id: None,
                last_active_pane_id: None,
            }],
            agents: vec![],
            clients: vec![],
        };
        assert!(
            build_rows(&snapshot, false, AgentSort::Grouped)
                .iter()
                .any(|row| matches!(row, Row::Session(session) if session.session_id == "$1"))
        );
    }

    #[test]
    fn sessions_are_above_midpoint_actions_and_agents_follow() {
        let snapshot: Snapshot =
            serde_json::from_str(include_str!("../tests/golden/snapshot-v1.json")).unwrap();
        let rows = balance_sections(build_rows(&snapshot, false, AgentSort::Grouped), 20);
        let sessions = rows
            .iter()
            .position(|row| matches!(row, Row::Section("sessions")))
            .unwrap();
        let agents = rows
            .iter()
            .position(|row| matches!(row, Row::AgentSection(_)))
            .unwrap();
        let actions = rows
            .iter()
            .position(|row| matches!(row, Row::Actions))
            .unwrap();
        assert_eq!(sessions, 0);
        assert_eq!(actions, 10);
        assert_eq!(agents, 12);
    }

    #[test]
    fn midpoint_actions_span_left_and_right_without_help() {
        assert_eq!(action_button(40, 0), Some(FooterButton::New));
        assert_eq!(action_button(40, 20), None);
        assert_eq!(action_button(40, 34), Some(FooterButton::Menu));
        assert_eq!(close_button(40, 33), Some(FooterButton::Close));
    }

    #[test]
    fn sessions_match_tmux_name_order() {
        let session = |id: &str, name: &str, attention_count| SessionSnapshot {
            session_id: id.into(),
            session_name: name.into(),
            rollup_state: DisplayState::Unknown,
            agent_count: 1,
            attention_count,
            current_path: None,
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
                session("$2", "zeta", 0),
                session("$3", "alpha", 1),
                session("$10", "gamma", 0),
                session("$11", "beta", 2),
            ],
            agents: vec![],
            clients: vec![],
        };
        let sessions: Vec<_> = build_rows(&snapshot, false, AgentSort::Grouped)
            .into_iter()
            .filter_map(|row| match row {
                Row::Session(session) => Some(session.session_id),
                Row::Section(_)
                | Row::AgentSection(_)
                | Row::SessionSub(_)
                | Row::Agent(_)
                | Row::AgentSub(_)
                | Row::Detail(_, _)
                | Row::Conversation(_, _)
                | Row::Actions
                | Row::Spacer => None,
            })
            .collect();
        assert_eq!(sessions, ["$3", "$11", "$10", "$2"]);
    }

    #[test]
    fn agents_use_stable_tmux_location_order() {
        let mut snapshot: Snapshot =
            serde_json::from_str(include_str!("../tests/golden/snapshot-v1.json")).unwrap();
        let template = snapshot.agents[0].clone();
        let agent = |instance: &str, session: &str, window, pane| {
            let mut agent = template.clone();
            agent.instance_id = instance.into();
            agent.target.session_id = session.into();
            agent.target.window_index = window;
            agent.target.pane_index = pane;
            agent
        };
        snapshot.agents = vec![
            agent("late", "$11", 1, 0),
            agent("pane", "$2", 1, 3),
            agent("first", "$2", 1, 1),
        ];
        snapshot.agents[0].target.session_name = "zeta".into();
        snapshot.agents[1].target.session_name = "alpha".into();
        snapshot.agents[2].target.session_name = "alpha".into();

        let agents: Vec<_> = build_rows(&snapshot, false, AgentSort::Grouped)
            .into_iter()
            .filter_map(|row| match row {
                Row::Agent(agent) => Some(agent.instance_id),
                _ => None,
            })
            .collect();
        assert_eq!(agents, ["first", "pane", "late"]);
    }

    #[test]
    fn prioritized_agents_follow_herdr_attention_order() {
        let mut snapshot: Snapshot =
            serde_json::from_str(include_str!("../tests/golden/snapshot-v1.json")).unwrap();
        let template = snapshot.agents[0].clone();
        let agent = |instance: &str, state| {
            let mut agent = template.clone();
            agent.instance_id = instance.into();
            agent.display_state = state;
            agent
        };
        snapshot.agents = vec![
            agent("idle", DisplayState::Idle),
            agent("working", DisplayState::Working),
            agent("done", DisplayState::Done),
            agent("blocked", DisplayState::Blocked),
            agent("unknown", DisplayState::Unknown),
        ];
        let agents: Vec<_> = build_rows(&snapshot, false, AgentSort::Prioritized)
            .into_iter()
            .filter_map(|row| match row {
                Row::Agent(agent) => Some(agent.instance_id),
                _ => None,
            })
            .collect();
        assert_eq!(agents, ["blocked", "done", "working", "idle", "unknown"]);
    }

    #[test]
    fn session_and_agent_cards_have_two_clickable_rows() {
        let snapshot: Snapshot =
            serde_json::from_str(include_str!("../tests/golden/snapshot-v1.json")).unwrap();
        let rows = build_rows(&snapshot, false, AgentSort::Grouped);
        assert!(rows.windows(2).any(|pair| matches!(
            pair,
            [Row::Session(first), Row::SessionSub(second)] if first.session_id == second.session_id
        )));
        assert!(rows.windows(2).any(|pair| matches!(
            pair,
            [Row::Agent(first), Row::AgentSub(second)] if first.instance_id == second.instance_id
        )));
    }

    #[test]
    fn session_context_omits_repeated_cwd_name() {
        let snapshot: Snapshot =
            serde_json::from_str(include_str!("../tests/golden/snapshot-v1.json")).unwrap();
        let mut session = snapshot.sessions[0].clone();
        session.session_name = "word-formula".into();
        session.current_path = Some("/tmp/word-formula".into());
        session.agent_count = 1;
        assert_eq!(session_context(&session), "1 agent");

        session.current_path = Some("/tmp/dotfiles".into());
        assert_eq!(session_context(&session), "dotfiles · 1 agent");
    }

    #[test]
    fn selected_card_uses_one_background_across_both_rows() {
        let snapshot: Snapshot =
            serde_json::from_str(include_str!("../tests/golden/snapshot-v1.json")).unwrap();
        let agent = snapshot.agents[0].clone();
        for row in [Row::Agent(agent.clone()), Row::AgentSub(agent)] {
            let line = render_row(&row, true, 40);
            assert!(
                line.spans
                    .iter()
                    .all(|span| span.style.bg == Some(Color::DarkGray))
            );
        }
        let session = snapshot.sessions[0].clone();
        let line = render_row(&Row::SessionSub(session), true, 40);
        assert!(line.spans.iter().all(|span| {
            span.style.bg == Some(Color::DarkGray)
                && span.style.fg == Some(Color::Rgb(235, 235, 245))
        }));
    }

    #[test]
    fn agent_card_places_session_first_and_kind_with_status_second() {
        let snapshot: Snapshot =
            serde_json::from_str(include_str!("../tests/golden/snapshot-v1.json")).unwrap();
        let agent = snapshot.agents[0].clone();
        let first = render_row(&Row::Agent(agent.clone()), false, 40);
        let second = render_row(&Row::AgentSub(agent.clone()), false, 40);
        let text = |line: Line<'_>| {
            line.spans
                .iter()
                .map(|span| span.content.as_ref())
                .collect::<String>()
        };
        let first = text(first);
        let second = text(second);
        assert!(first.contains(&agent.target.session_name));
        assert!(!first.contains(agent_kind_name(agent.kind)));
        assert!(second.contains(agent_kind_name(agent.kind)));
        assert!(second.contains(&agent_status(&agent)));
    }

    #[test]
    fn duplicate_agent_key_stays_in_its_nearest_section() {
        let snapshot: Snapshot =
            serde_json::from_str(include_str!("../tests/golden/snapshot-v1.json")).unwrap();
        let agent = snapshot.agents[0].clone();
        let rows = vec![
            Row::Agent(agent.clone()),
            Row::Spacer,
            Row::Agent(agent.clone()),
        ];
        let key = selection_key(&Row::Agent(agent)).unwrap();
        assert_eq!(nearest_matching_key(&rows, &key, 2), Some(2));
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
        let rows = build_rows(&snapshot, false, AgentSort::Grouped);
        assert!(rows.iter().any(|row| matches!(row, Row::Agent(_))));
    }

    #[test]
    fn hook_conflict_uses_a_compact_warning_marker() {
        let mut snapshot: Snapshot =
            serde_json::from_str(include_str!("../tests/golden/snapshot-v1.json")).unwrap();
        snapshot.agents[0].hook_health = crate::model::HookHealth::Conflict;
        let line = render_row(&Row::AgentSub(snapshot.agents[0].clone()), false, 40);
        let rendered: String = line
            .spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect();
        assert!(rendered.contains(" ! · "));
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
        let compact = build_rows(&snapshot, false, AgentSort::Grouped);
        let detailed = build_rows(&snapshot, true, AgentSort::Grouped);
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
        let rows = build_rows(&snapshot, false, AgentSort::Grouped);
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
