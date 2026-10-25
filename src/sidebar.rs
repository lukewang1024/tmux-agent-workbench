use std::ffi::{OsStr, OsString};
use std::io::{self, Write, stdout};
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use crossterm::event::{self, Event, KeyCode, KeyEventKind, MouseButton, MouseEventKind};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

use crate::config::{AgentSort, Config};
use crate::ipc::{ClientError, Request, call};
use crate::model::{
    AgentSnapshot, ConversationSnapshot, DisplayState, SessionSnapshot, Snapshot, StateSource,
};
use crate::navigation_list::{Hit, NavigationList, Screen};
use crate::paths::Paths;
use crate::server::ServerIdentity;

#[derive(Debug, Clone)]
enum Row {
    Section(&'static str),
    AgentSection(AgentSort),
    AgentGroup(String),
    Session(SessionSnapshot),
    SessionSub(SessionSnapshot),
    Agent(AgentSnapshot),
    AgentSub(AgentSnapshot, AgentSort),
    Detail(String, String),
    Conversation(AgentSnapshot, ConversationSnapshot),
    Spacer,
}

/// Shared geometry for rendering and hit testing. Status, overflow and popup
/// footer lines and the pinned action bar never map to an agent row.
#[derive(Debug, Clone, Copy)]
struct SidebarViewport {
    first_row: usize,
    row_height: usize,
}

impl SidebarViewport {
    fn new(content_height: usize, total: usize, notice: Option<&str>) -> Self {
        // Actions and the Agents/sort heading stay above the scrolling body.
        let first_row = 3 + usize::from(notice.is_some());
        let available = content_height.saturating_sub(first_row);
        let row_height = if notice == Some("disconnected") {
            0
        } else if total > available {
            available.saturating_sub(1)
        } else {
            available
        };
        Self {
            first_row,
            row_height,
        }
    }

    #[cfg(test)]
    fn row_at(self, screen_row: u16, scroll: usize, total: usize) -> Option<usize> {
        let mut list = NavigationList::<()>::default();
        list.replace_rows((0..total).map(|index| Some(index.to_string())).collect());
        list.set_viewport(Rect::new(
            0,
            self.first_row as u16,
            1,
            self.row_height as u16,
        ));
        list.scroll = scroll;
        match list.hit(0, screen_row, &[]) {
            Some(Hit::Item(key)) => key.parse().ok(),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Action {
    New,
    Menu,
    Close,
    Sort,
}

pub fn run(paths: &Paths, server: &ServerIdentity) -> Result<(), Box<dyn std::error::Error>> {
    // Resolve the first frame before switching to the alternate screen. Some
    // remote clients do not paint an initially empty fullscreen popup until a
    // later input event; entering with real rows makes the UI visible at once.
    let initial_snapshot = fetch_snapshot(&paths.socket_for_server(&server.key));
    let _screen = Screen::enter()?;
    let mut terminal = Terminal::new(crate::popup_surface::Surface::from_env(
        CrosstermBackend::new(stdout()),
    )?)?;
    let result = event_loop(&mut terminal, paths, server, initial_snapshot);
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

pub fn open_popup(
    server: &ServerIdentity,
    pane: &str,
    client: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    popup(server, pane, client, &["sidebar"])
}

pub(crate) fn popup(
    server: &ServerIdentity,
    pane: &str,
    client: &str,
    args: &[&str],
) -> Result<(), Box<dyn std::error::Error>> {
    crate::popup::open(server, pane, client, args, crate::popup::Sizing::Sidebar)
}

fn switch_return_to() -> crate::switcher::ReturnTo {
    if popup_mode() {
        crate::switcher::ReturnTo::SidebarPopup
    } else {
        crate::switcher::ReturnTo::SidebarPane
    }
}

fn switch_list(kind: &str) -> Result<bool, Box<dyn std::error::Error>> {
    let pane = source_pane().ok_or("sidebar source pane unavailable")?;
    let client = current_client()?;
    run_workbench(
        &[
            "switch",
            kind,
            "--pane",
            &pane,
            "--client",
            &client,
            "--return-to",
            switch_return_to().name(),
            "--language",
            crate::ui_language::name(),
        ],
        true,
    )
}

fn refresh_interval(visible: bool) -> Duration {
    Duration::from_secs(if visible { 1 } else { 5 })
}

fn event_loop(
    terminal: &mut Terminal<crate::popup_surface::Surface<CrosstermBackend<io::Stdout>>>,
    paths: &Paths,
    server: &ServerIdentity,
    initial_snapshot: Result<Snapshot, Box<dyn std::error::Error>>,
) -> Result<(), Box<dyn std::error::Error>> {
    let socket = paths.socket_for_server(&server.key);
    let fetch_socket = socket.clone();
    let (refresh_tx, refresh_rx) = mpsc::sync_channel::<bool>(1);
    let visibility_tmux = crate::tmux::Tmux::new(server.clone());
    let visibility_pane = std::env::var("TMUX_PANE").ok();
    let is_popup = popup_mode();
    let (snapshot_tx, snapshot_rx) = mpsc::sync_channel(1);
    std::thread::spawn(move || {
        let mut visible = true;
        let mut next_visibility_check = Instant::now();
        while let Ok(interactive) = refresh_rx.recv() {
            if !is_popup && (interactive || Instant::now() >= next_visibility_check) {
                visible = visibility_pane
                    .as_deref()
                    .and_then(|pane| visibility_tmux.sidebar_visible(pane).ok())
                    .unwrap_or(true);
                next_visibility_check = Instant::now() + Duration::from_secs(5);
            }
            let result = fetch_snapshot(&fetch_socket).map_err(|error| error.to_string());
            let _ = snapshot_tx.try_send((result, visible));
        }
    });
    let mut detailed = false;
    let mut agent_sort = Config::load(&paths.config_file())
        .map(|config| config.sidebar.agent_sort)
        .unwrap_or_default();
    if let Some(saved) = persisted_agent_sort(paths) {
        agent_sort = saved;
    }
    let mut snapshot = initial_snapshot.ok();
    let mut rows = snapshot
        .as_ref()
        .map(|snapshot| build_rows(snapshot, detailed, agent_sort))
        .unwrap_or_default();
    let mut footer_hover = None;
    let mut navigation = NavigationList::<Action>::default();
    navigation.replace_rows(rows.iter().map(selection_key).collect());
    if let Some(snapshot) = &snapshot
        && let Some(index) = initial_row(&rows, snapshot, server, is_popup)
    {
        navigation.select(index);
    }
    let mut disconnected = snapshot.is_none();
    let mut last_success = snapshot.as_ref().map(|_| Instant::now());
    let mut last_failure_log = Instant::now() - Duration::from_secs(10);
    let mut visible = true;
    let mut next_refresh = Instant::now();
    let mut next_sort_sync = Instant::now() + Duration::from_millis(200);
    let mut dirty = true;
    let mut initial_selection = snapshot.is_none();
    let mut last_content_height = usize::MAX;
    loop {
        if let Ok((result, now_visible)) = snapshot_rx.try_recv() {
            visible = now_visible;
            next_refresh = Instant::now() + refresh_interval(visible);
            match result {
                Ok(fetched) => {
                    rows = build_rows(&fetched, detailed, agent_sort);
                    navigation.replace_rows(rows.iter().map(selection_key).collect());
                    disconnected = false;
                    last_success = Some(Instant::now());
                    if initial_selection {
                        initial_selection = false;
                        navigation
                            .select(initial_row(&rows, &fetched, server, is_popup).unwrap_or(0));
                    }
                    snapshot = Some(fetched);
                }
                Err(error) => {
                    // Keep the last good snapshot through a short scan/IPC
                    // hiccup. A genuinely unavailable daemon still becomes
                    // visible after the stale grace instead of flashing on
                    // every isolated timeout.
                    disconnected = last_success
                        .is_none_or(|success| success.elapsed() >= Duration::from_secs(3));
                    if last_failure_log.elapsed() >= Duration::from_secs(10) {
                        append_sidebar_log(
                            paths,
                            &format!("snapshot-error socket={} error={error}", socket.display()),
                        );
                        last_failure_log = Instant::now();
                    }
                }
            }
            dirty = true;
        }
        if Instant::now() >= next_refresh {
            let _ = refresh_tx.try_send(false);
            next_refresh = Instant::now() + refresh_interval(visible);
        }
        if Instant::now() >= next_sort_sync {
            next_sort_sync = Instant::now()
                + if visible {
                    Duration::from_millis(200)
                } else {
                    Duration::from_secs(5)
                };
            if let Some(saved) = persisted_agent_sort(paths)
                && saved != agent_sort
            {
                agent_sort = saved;
                if let Some(snapshot) = &snapshot {
                    rows = build_rows(snapshot, detailed, agent_sort);
                    navigation.replace_rows(rows.iter().map(selection_key).collect());
                }
                dirty = true;
            }
        }

        let size = terminal.size()?;
        let body_height = usize::from(size.height).max(1);
        let footer_height = 2 * usize::from(popup_mode());
        let content_height = body_height.saturating_sub(footer_height);
        if content_height != last_content_height {
            last_content_height = content_height;
            dirty = true;
        }
        let notice = connection_notice(disconnected, snapshot.is_some());
        let viewport = SidebarViewport::new(content_height, rows.len(), notice);
        navigation.set_viewport(Rect::new(
            0,
            viewport.first_row as u16,
            size.width,
            viewport.row_height as u16,
        ));
        if dirty {
            terminal.draw(|frame| {
                let area = frame.area();
                let mut header = vec![
                    render_actions(area.width, footer_hover),
                    render_row(
                        &Row::AgentSection(agent_sort),
                        footer_hover == Some(Action::Sort),
                        area.width,
                    ),
                ];
                if let Some(notice) = notice {
                    header.push(Line::from(Span::styled(notice, muted_style())));
                }
                frame.render_widget(Paragraph::new(header), area);
                crate::navigation_list::separator(
                    frame,
                    Rect::new(0, (viewport.first_row - 1) as u16, area.width, 1),
                );
                navigation.render(frame, |index, selected| {
                    render_row(&rows[index], selected, area.width)
                });
                if rows.is_empty() && notice != Some("disconnected") {
                    frame.render_widget(
                        Paragraph::new("no sessions").style(muted_style()),
                        Rect::new(
                            0,
                            viewport.first_row as u16,
                            area.width,
                            1.min(area.height.saturating_sub(viewport.first_row as u16)),
                        ),
                    );
                }
                let shown = navigation.visible_rows().len();
                let hidden = rows.len().saturating_sub(shown);
                if hidden > 0 && viewport.first_row + viewport.row_height < content_height {
                    frame.render_widget(
                        Paragraph::new(format!("↕ {hidden} hidden")),
                        Rect::new(
                            0,
                            (viewport.first_row + viewport.row_height) as u16,
                            area.width,
                            1,
                        ),
                    );
                }
                if popup_mode() && area.height > 0 {
                    crate::navigation_list::separator(
                        frame,
                        Rect::new(
                            0,
                            area.height.saturating_sub(2),
                            area.width,
                            u16::from(area.height > 1),
                        ),
                    );
                    frame.render_widget(
                        Paragraph::new(render_close(area.width, footer_hover)),
                        Rect::new(0, area.height - 1, area.width, 1),
                    );
                }
            })?;
            dirty = false;
        }

        if !event::poll(if visible {
            Duration::from_millis(50)
        } else {
            Duration::from_millis(250)
        })? {
            continue;
        }
        let Some(input) = terminal.backend_mut().input(event::read()?)? else {
            continue;
        };
        if matches!(input, Event::FocusGained) || (!visible && !matches!(input, Event::FocusLost)) {
            let _ = refresh_tx.try_send(true);
            visible = true;
            next_refresh = Instant::now() + refresh_interval(true);
            next_sort_sync = Instant::now();
        }
        dirty = true;
        match input {
            Event::FocusGained => {
                // tmux focuses this pane before forwarding a mouse click. Keep
                // the selection and viewport stable until that click is handled;
                // selecting the current agent here can scroll a different row
                // underneath the pending mouse coordinates. Initial selection
                // is resolved when the first snapshot arrives above.
                navigation.selection_visible = true;
            }
            Event::Key(key) if key.kind == KeyEventKind::Press => {
                navigation.cancel_press();
                if popup_mode()
                    && (key.code == KeyCode::Esc
                        || (key.modifiers.contains(event::KeyModifiers::CONTROL)
                            && matches!(key.code, KeyCode::Char('c' | 'd'))))
                {
                    return Ok(());
                }
                match key.code {
                    KeyCode::Char('?') => {
                        if show_help()? {
                            return Ok(());
                        }
                    }
                    _ if navigation.navigate(key.code) => {}
                    KeyCode::Enter => {
                        let navigated = activate(&rows, navigation.selected)?;
                        if popup_mode() && navigated {
                            return Ok(());
                        }
                    }
                    KeyCode::Char('m') => {
                        if show_row_menu(
                            &rows,
                            navigation.selected,
                            Some((
                                0,
                                (viewport.first_row
                                    + navigation.selected.saturating_sub(navigation.scroll))
                                    as u16,
                            )),
                        )? {
                            return Ok(());
                        }
                    }
                    KeyCode::Char('d') => {
                        detailed = !detailed;
                        if let Some(snapshot) = &snapshot {
                            rows = build_rows(snapshot, detailed, agent_sort);
                            navigation.replace_rows(rows.iter().map(selection_key).collect());
                        }
                    }
                    KeyCode::Char('N') => {
                        if run_session_picker()? {
                            return Ok(());
                        }
                    }
                    KeyCode::Char('i') => {
                        if run_public_workbench(&["pick", "repo"], true)? {
                            return Ok(());
                        }
                    }
                    KeyCode::Char('W') => {
                        if run_public_workbench(&["new", "--prompt"], true)? {
                            return Ok(());
                        }
                    }
                    KeyCode::Char('T') if navigation.selection_visible => {
                        if new_task_selected(&rows, navigation.selected)? {
                            return Ok(());
                        }
                    }
                    KeyCode::Char('R') => {
                        run_public_workbench(&["projects", "rebuild"], false)?;
                    }
                    KeyCode::Char('n') => {
                        if run_workbench(&["attention", "next"], true)? {
                            return Ok(());
                        }
                    }
                    KeyCode::Char('s') => {
                        if switch_list("session")? {
                            return Ok(());
                        }
                    }
                    KeyCode::Char('a') => {
                        if switch_list("agent")? {
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
                let actions = action_regions(
                    Rect::new(0, 0, size.width, size.height),
                    agent_sort,
                    popup_mode(),
                );
                let hit = navigation.mouse(mouse, &actions);
                let hit = if mouse.kind == MouseEventKind::Down(MouseButton::Right) {
                    navigation
                        .hit(mouse.column, mouse.row, &actions)
                        .filter(|hit| matches!(hit, Hit::Item(_)))
                } else {
                    hit
                };
                footer_hover = match navigation.hovered {
                    Some(Hit::Action(action)) => Some(action),
                    _ => None,
                };
                match hit {
                    Some(Hit::Action(Action::Close)) => return Ok(()),
                    Some(Hit::Action(Action::New)) => {
                        if run_session_picker()? {
                            return Ok(());
                        }
                    }
                    Some(Hit::Action(Action::Menu)) => {
                        if show_global_menu(false, Some((mouse.column, mouse.row)))? {
                            return Ok(());
                        }
                    }
                    Some(Hit::Action(Action::Sort)) => {
                        agent_sort = match agent_sort {
                            AgentSort::Grouped => AgentSort::Prioritized,
                            AgentSort::Prioritized => AgentSort::Grouped,
                        };
                        persist_agent_sort(paths, agent_sort)?;
                        if let Some(snapshot) = &snapshot {
                            rows = build_rows(snapshot, detailed, agent_sort);
                            navigation = NavigationList::default();
                            navigation.replace_rows(rows.iter().map(selection_key).collect());
                        }
                    }
                    Some(Hit::Item(_)) => {
                        let navigated = if mouse.kind == MouseEventKind::Down(MouseButton::Right) {
                            show_row_menu(
                                &rows,
                                navigation.selected,
                                Some((mouse.column, mouse.row)),
                            )?
                        } else {
                            activate(&rows, navigation.selected)?
                        };
                        if popup_mode() && navigated {
                            return Ok(());
                        }
                    }
                    None => {}
                }
            }
            Event::FocusLost => {
                navigation.blur();
                footer_hover = None;
            }
            _ => {}
        }
    }
}

fn append_sidebar_log(paths: &Paths, message: &str) {
    let log = paths.state_dir.join("sidebar.log");
    if let Ok(mut file) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(log)
    {
        let _ = writeln!(file, "{message}");
    }
}

fn connection_notice(disconnected: bool, has_snapshot: bool) -> Option<&'static str> {
    match (disconnected, has_snapshot) {
        (true, false) => Some("disconnected"),
        (true, true) => Some("stale · reconnecting"),
        (false, _) => None,
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
    let mut rows = vec![];
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
    let mut previous_session = None;
    for agent in agents {
        if agent_sort == AgentSort::Grouped {
            let session = (
                agent.target.session_name.clone(),
                agent.target.session_id.clone(),
            );
            if previous_session.as_ref() != Some(&session) {
                rows.push(Row::AgentGroup(session.0.clone()));
                previous_session = Some(session);
            }
        }
        rows.push(Row::Agent(agent.clone()));
        rows.push(Row::AgentSub(agent.clone(), agent_sort));
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
    rows.push(Row::Spacer);
    rows.push(Row::Section("sessions"));
    rows.push(Row::Spacer);
    let mut sessions = snapshot.sessions.clone();
    sessions.sort_by_key(stable_session_key);
    for session in sessions {
        rows.push(Row::Session(session.clone()));
        rows.push(Row::SessionSub(session));
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

fn stable_agent_key(agent: &AgentSnapshot) -> (String, String, u32, u32, String) {
    (
        agent.target.session_name.clone(),
        agent.target.session_id.clone(),
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

fn initial_row(
    rows: &[Row],
    snapshot: &Snapshot,
    server: &ServerIdentity,
    popup: bool,
) -> Option<usize> {
    if popup {
        // An overview opens at the top, including the first Session group.
        // Following a lower current Agent used to hide the heading and begin
        // in the middle of a group as soon as the popup opened.
        rows.iter().position(|row| selection_key(row).is_some())
    } else {
        current_agent_selection(rows, snapshot, server)
    }
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
            muted_style().add_modifier(Modifier::BOLD),
        )),
        Row::AgentSection(sort) => aligned_line(
            "agents".into(),
            match sort {
                AgentSort::Grouped => "grouped",
                AgentSort::Prioritized => "prioritized",
            }
            .into(),
            width,
            muted_style().add_modifier(Modifier::BOLD),
            muted_style(),
            selected,
        ),
        Row::AgentGroup(name) => Line::from(Span::styled(
            fit_text(name, usize::from(width)),
            muted_style().add_modifier(Modifier::BOLD),
        )),
        Row::Session(session) => icon_title_line(
            glyph(session.rollup_state),
            &session.session_name,
            width,
            Style::default().fg(state_color(session.rollup_state)),
            if session.active {
                Style::default()
                    .fg(Color::Rgb(235, 235, 245))
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(Color::Rgb(235, 235, 245))
            },
            selected,
        ),
        Row::SessionSub(session) => context_line(&session_context(session), width, selected),
        Row::Agent(agent) => {
            let title = if agent.visible && agent.state_source != StateSource::Screen {
                Style::default()
                    .fg(Color::Rgb(235, 235, 245))
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(Color::Rgb(235, 235, 245))
            };
            icon_title_line(
                glyph(agent.display_state),
                agent_topic(agent),
                width,
                Style::default().fg(state_color(agent.display_state)),
                title,
                selected,
            )
        }
        Row::AgentSub(agent, sort) => {
            let mut context = agent
                .task_progress
                .map(|progress| format!("{}/{}", progress.completed, progress.total))
                .unwrap_or_default();
            if *sort == AgentSort::Prioritized {
                if !context.is_empty() {
                    context.push_str(" · ");
                }
                context.push_str(&agent.target.session_name);
            }
            aligned_line(
                format!("   {context}"),
                format!("{} · {}", agent_status(agent), agent_kind_name(agent.kind)),
                width,
                if selected {
                    Style::default().fg(Color::Rgb(235, 235, 245))
                } else {
                    Style::default().fg(Color::Rgb(150, 150, 170))
                },
                Style::default()
                    .fg(state_color(agent.display_state))
                    .add_modifier(Modifier::DIM),
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

fn source_pane() -> Option<String> {
    std::env::var("WORKBENCH_SOURCE_PANE")
        .or_else(|_| std::env::var("TMUX_PANE"))
        .ok()
        .filter(|pane| safe_target(pane, '%'))
}

fn button_style(button: Action, hovered: Option<Action>) -> Style {
    let style = muted_style();
    if hovered == Some(button) {
        style.add_modifier(Modifier::REVERSED)
    } else {
        style
    }
}

fn muted_style() -> Style {
    // ANSI bright-black is the theme-pack `term-muted` semantic color. Keep it
    // undimmed: DIM makes muted text illegible, while an accent such as Cyan
    // gives structural labels and secondary actions too much visual weight.
    Style::default().fg(Color::DarkGray)
}

fn render_actions(width: u16, hovered: Option<Action>) -> Line<'static> {
    let gap = usize::from(width).saturating_sub(11);
    Line::from(vec![
        Span::styled("+ new", button_style(Action::New, hovered)),
        Span::raw(" ".repeat(gap)),
        Span::styled("⋯ menu", button_style(Action::Menu, hovered)),
    ])
}

fn render_close(width: u16, hovered: Option<Action>) -> Line<'static> {
    let gap = usize::from(width).saturating_sub(7);
    Line::from(vec![
        Span::raw(" ".repeat(gap)),
        Span::styled("× close", button_style(Action::Close, hovered)),
    ])
}

fn action_regions(area: Rect, agent_sort: AgentSort, is_popup: bool) -> Vec<(Rect, Action)> {
    let mut actions = vec![];
    if area.height > 0 {
        actions.push((Rect::new(0, 0, area.width.min(5), 1), Action::New));
        actions.push((
            Rect::new(area.width.saturating_sub(6), 0, area.width.min(6), 1),
            Action::Menu,
        ));
        if is_popup {
            actions.push((
                Rect::new(
                    area.width.saturating_sub(7),
                    area.height - 1,
                    area.width.min(7),
                    1,
                ),
                Action::Close,
            ));
        }
    }
    if area.height > 1 {
        let width = match agent_sort {
            AgentSort::Grouped => 7,
            AgentSort::Prioritized => 11,
        }
        .min(area.width);
        actions.push((
            Rect::new(area.width.saturating_sub(width), 1, width, 1),
            Action::Sort,
        ));
    }
    actions
}

fn show_help() -> Result<bool, Box<dyn std::error::Error>> {
    let items = [
        "-j/k ↑/↓ · move",
        "-Enter · open / focus",
        "-m · item menu",
        "-N · projects",
        "-i · temporary Git tool",
        "-W · new workspace",
        "-T · new task in selected session",
        "-R · rebuild projects",
        "-s / a · sessions / agents",
        "-n · next attention",
        "-d · details",
        "-r · reload",
        "-Escape / Ctrl-C / Ctrl-D · close popup",
    ]
    .map(|label| (label, "", String::new()));
    show_menu("Shortcuts", None, false, &items)
}

pub(crate) fn agent_kind_name(kind: crate::model::AgentKind) -> &'static str {
    match kind {
        crate::model::AgentKind::Codex => "codex",
        crate::model::AgentKind::Claude => "claude",
        crate::model::AgentKind::Trae => "trae",
        crate::model::AgentKind::Opencode => "opencode",
    }
}

pub(crate) fn agent_topic(agent: &AgentSnapshot) -> &str {
    if let Some(name) = agent
        .thread_name
        .as_deref()
        .map(str::trim)
        .filter(|name| !name.is_empty())
    {
        return name;
    }
    // Older daemons stored hook thread names only in label.
    if agent.hook_session_id.is_some() && meaningful_topic(&agent.label) {
        return agent.label.trim();
    }
    let topic = agent.target.window_name.trim();
    if !meaningful_topic(topic) {
        &agent.target.session_name
    } else {
        topic
    }
}

fn meaningful_topic(topic: &str) -> bool {
    !topic.trim().is_empty()
        && !matches!(
            topic.trim().to_ascii_lowercase().as_str(),
            "tmux"
                | "agent"
                | "codex"
                | "claude"
                | "trae"
                | "traex"
                | "opencode"
                | "shell"
                | "terminal"
                | "sh"
                | "zsh"
                | "bash"
                | "fish"
        )
}

pub(crate) fn agent_status(agent: &AgentSnapshot) -> String {
    if agent.display_state == DisplayState::Blocked
        && agent.base_state != crate::model::BaseState::Blocked
        && agent.hook_health == crate::model::HookHealth::Conflict
    {
        return "input !".to_owned();
    }
    let mut status = if agent.display_state == DisplayState::Idle
        && agent.rule_id.as_deref() == Some("codex-queued-question-idle")
    {
        "idle ?"
    } else if agent.display_state == DisplayState::Blocked {
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
    if agent.stale {
        status.push_str(" · stale");
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

fn fit_text(text: &str, width: usize) -> String {
    if Span::raw(text).width() <= width {
        return text.to_owned();
    }
    if width == 0 {
        return String::new();
    }
    let mut result = String::new();
    for character in text.chars() {
        result.push(character);
        if Span::raw(result.as_str()).width() > width {
            result.pop();
            break;
        }
    }
    result
}

pub(crate) fn context_line(text: &str, width: u16, selected: bool) -> Line<'static> {
    let style = Style::default().fg(if selected {
        Color::Rgb(235, 235, 245)
    } else {
        Color::Rgb(150, 150, 170)
    });
    aligned_line(
        format!("   {text}"),
        String::new(),
        width,
        style,
        style,
        selected,
    )
}

fn aligned_line(
    left: String,
    right: String,
    width: u16,
    mut left_style: Style,
    mut right_style: Style,
    selected: bool,
) -> Line<'static> {
    let width = usize::from(width);
    let right = fit_text(&right, width);
    let reserved = Span::raw(right.as_str()).width() + usize::from(!right.is_empty());
    let left_limit = width.saturating_sub(reserved);
    let left = fit_text(&left, left_limit);
    let gap =
        width.saturating_sub(Span::raw(left.as_str()).width() + Span::raw(right.as_str()).width());
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

pub(crate) fn icon_title_line(
    icon: &str,
    title: &str,
    width: u16,
    mut icon_style: Style,
    mut title_style: Style,
    selected: bool,
) -> Line<'static> {
    let width = usize::from(width);
    let prefix = format!(" {icon} ");
    let prefix = fit_text(&prefix, width);
    let title_limit = width.saturating_sub(Span::raw(prefix.as_str()).width());
    let title = fit_text(title, title_limit);
    let gap = width
        .saturating_sub(Span::raw(prefix.as_str()).width() + Span::raw(title.as_str()).width());
    if selected {
        icon_style = icon_style
            .remove_modifier(Modifier::DIM)
            .bg(Color::DarkGray);
        title_style = title_style
            .remove_modifier(Modifier::DIM)
            .add_modifier(Modifier::BOLD)
            .bg(Color::DarkGray);
    }
    Line::from(vec![
        Span::styled(prefix, icon_style),
        Span::styled(title, title_style),
        Span::styled(" ".repeat(gap), title_style),
    ])
}

pub(crate) fn state_color(state: DisplayState) -> Color {
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

pub(crate) fn glyph(state: DisplayState) -> &'static str {
    match state {
        DisplayState::Blocked => "●",
        DisplayState::Done => "●",
        DisplayState::Working => "●",
        DisplayState::Idle => "○",
        DisplayState::Unknown => "·",
    }
}

fn selection_key(row: &Row) -> Option<String> {
    match row {
        Row::Session(session) => Some(format!("session:{}", session.session_id)),
        Row::SessionSub(session) => Some(format!("session:{}", session.session_id)),
        Row::Agent(agent) => Some(format!("agent:{}", agent.instance_id)),
        Row::AgentSub(agent, _) => Some(format!("agent:{}", agent.instance_id)),
        Row::Conversation(agent, conversation) => Some(format!(
            "conversation:{}:{}",
            agent.instance_id, conversation.id
        )),
        Row::Section(_)
        | Row::AgentSection(_)
        | Row::AgentGroup(_)
        | Row::Detail(_, _)
        | Row::Spacer => None,
    }
}

fn acknowledge_attention(socket: &std::path::Path, event_id: &str) -> Result<(), ClientError> {
    match call(
        socket,
        &Request::new("attention.ack", serde_json::json!({"event_id": event_id})),
        Duration::from_secs(1),
    ) {
        // A rendered row can outlive its event when another client acknowledges
        // it or the daemon expires it. The desired state is already reached.
        Err(ClientError::Remote { code, message })
            if code == "request_failed" && message == "attention event not found" =>
        {
            Ok(())
        }
        Ok(_) => Ok(()),
        Err(error) => Err(error),
    }
}

fn activate(rows: &[Row], selected: usize) -> Result<bool, Box<dyn std::error::Error>> {
    match rows.get(selected) {
        Some(Row::Session(session) | Row::SessionSub(session)) => {
            let mut command = Command::new(std::env::current_exe()?);
            command.args(["focus", "--responsive", "--session", &session.session_id]);
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
        Some(Row::Agent(agent) | Row::AgentSub(agent, _)) if agent.exited => {
            if let Some(event) = &agent.attention {
                let server = ServerIdentity::discover()?;
                let paths = Paths::discover()?;
                acknowledge_attention(&paths.socket_for_server(&server.key), &event.id)?;
            }
        }
        Some(Row::Agent(agent) | Row::AgentSub(agent, _)) => {
            let mut command = Command::new(std::env::current_exe()?);
            command.args([
                "focus",
                "--responsive",
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
                "--responsive",
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
                        format!(
                            "switch-client -c {} -t {}",
                            shell_quote(&current_client()?),
                            session.session_id
                        ),
                    ),
                    (
                        "New task",
                        "n",
                        format!(
                            "run-shell -b {}",
                            shell_quote(&format!(
                                "WORKBENCH_SESSION={} TMUX_PANE={} WORKBENCH_TARGET_CLIENT={} tmux-agent-workbench task new --prompt",
                                session.session_id,
                                source_pane().ok_or("sidebar source pane unavailable")?,
                                shell_quote(&current_client()?)
                            ))
                        ),
                    ),
                    (
                        "Rename",
                        "r",
                        format!(
                            "command-prompt -t {} -p 'Rename session:' \"rename-session -t {} '%%'\"",
                            shell_quote(&current_client()?),
                            session.session_id
                        ),
                    ),
                    (
                        "Close",
                        "x",
                        format!(
                            "confirm-before -t {} -p 'Close session?' 'kill-session -t {}'",
                            shell_quote(&current_client()?),
                            session.session_id
                        ),
                    ),
                ],
            )
        }
        Some(Row::Agent(agent) | Row::AgentSub(agent, _))
            if !agent.exited && safe_target(&agent.target.pane_id, '%') =>
        {
            let executable = std::env::current_exe()?;
            let source = source_pane().ok_or("sidebar source pane unavailable")?;
            let focus = format!(
                "{} focus --responsive --session {} --window {} --pane {} --source-pane {}",
                shell_quote(&executable.to_string_lossy()),
                agent.target.session_id,
                agent.target.window_id,
                agent.target.pane_id,
                source,
            );
            show_menu(
                "agent",
                anchor,
                false,
                &[
                    ("Focus", "f", format!("run-shell {}", shell_quote(&focus))),
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
                            "confirm-before -t {} -p 'Close pane?' 'kill-pane -t {}'",
                            shell_quote(&current_client()?),
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
    let navigation = |kind: &str| -> Result<String, Box<dyn std::error::Error>> {
        Ok(format!(
            "run-shell -b {}",
            shell_quote(&format!(
                "{} switch {} --pane {} --client {} --return-to {} --language {}",
                executable,
                kind,
                shell_quote(&sidebar_pane),
                shell_quote(&current_client()?),
                switch_return_to().name(),
                crate::ui_language::name()
            ))
        ))
    };
    let mut items = vec![
        (
            "New",
            "N",
            format!(
                "run-shell -b {}",
                shell_quote(&format!(
                    "WORKBENCH_SOURCE_PANE={sidebar_pane} WORKBENCH_TARGET_CLIENT={} tmux-agent-workbench pick project",
                    shell_quote(&current_client()?)
                ))
            ),
        ),
        (
            "Temporary Git tool",
            "i",
            format!(
                "run-shell -b {}",
                shell_quote(&format!(
                    "TMUX_PANE={sidebar_pane} WORKBENCH_TARGET_CLIENT={} tmux-agent-workbench pick repo",
                    shell_quote(&current_client()?)
                ))
            ),
        ),
        (
            "New workspace",
            "W",
            format!(
                "run-shell -b {}",
                shell_quote(&format!(
                    "TMUX_PANE={sidebar_pane} WORKBENCH_TARGET_CLIENT={} tmux-agent-workbench new --prompt",
                    shell_quote(&current_client()?)
                ))
            ),
        ),
        (
            "New task",
            "T",
            format!(
                "run-shell -b {}",
                shell_quote(&format!(
                    "env -u WORKBENCH_SESSION -u WORKBENCH_TASK -u WORKBENCH_FEATURE TMUX_PANE={sidebar_pane} WORKBENCH_TARGET_CLIENT={} tmux-agent-workbench task new --prompt",
                    shell_quote(&current_client()?)
                ))
            ),
        ),
        (
            "Rebuild projects",
            "R",
            "run-shell -b tmux-agent-workbench projects rebuild".into(),
        ),
        ("Details", "d", format!("send-keys -t {sidebar_pane} d")),
        ("Sessions", "s", navigation("session")?),
        ("Agents", "a", navigation("agent")?),
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
        items.retain(|(label, _, _)| *label != "Details");
    }
    show_menu("workbench", anchor, anchor.is_some(), &items)
}

pub(crate) fn run_workbench(
    args: &[&str],
    clear_popup: bool,
) -> Result<bool, Box<dyn std::error::Error>> {
    let executable = std::env::current_exe()?;
    dispatch_command(&executable, args.iter().map(OsStr::new), clear_popup)
}

fn run_session_picker() -> Result<bool, Box<dyn std::error::Error>> {
    run_public_workbench(&["pick", "project"], true)
}

fn run_public_workbench(
    args: &[&str],
    clear_popup: bool,
) -> Result<bool, Box<dyn std::error::Error>> {
    dispatch_command(
        "tmux-agent-workbench",
        args.iter().map(OsStr::new),
        clear_popup,
    )
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
        let pane = source_pane().ok_or("sidebar source pane unavailable")?;
        if !safe_target(&pane, '%') {
            return Err("invalid sidebar pane id".into());
        }
        let mut deferred = format!(
            "TMUX_PANE={} WORKBENCH_TARGET_CLIENT={} workbench-popup-action {} {}",
            shell_quote(&pane),
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
        .env("WORKBENCH_TARGET_CLIENT", current_client()?)
        .env(
            "WORKBENCH_SOURCE_PANE",
            source_pane().ok_or("sidebar source pane unavailable")?,
        )
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

fn new_task_selected(rows: &[Row], selected: usize) -> Result<bool, Box<dyn std::error::Error>> {
    let pane = match rows.get(selected) {
        Some(Row::Agent(agent) | Row::AgentSub(agent, _)) | Some(Row::Conversation(agent, _)) => {
            Some(agent.target.pane_id.as_str())
        }
        Some(Row::Session(session) | Row::SessionSub(session)) => {
            session.last_active_pane_id.as_deref()
        }
        _ => None,
    };
    let Some(pane) = pane.filter(|pane| safe_target(pane, '%')) else {
        return Ok(false);
    };
    dispatch_command(
        "env",
        [
            "-u".to_owned(),
            "WORKBENCH_SESSION".to_owned(),
            "-u".to_owned(),
            "WORKBENCH_FEATURE".to_owned(),
            "-u".to_owned(),
            "WORKBENCH_TASK".to_owned(),
            format!("TMUX_PANE={pane}"),
            format!("WORKBENCH_TARGET_CLIENT={}", current_client()?),
            "tmux-agent-workbench".to_owned(),
            "task".to_owned(),
            "new".to_owned(),
            "--prompt".to_owned(),
        ],
        true,
    )
}

fn show_menu(
    title: &str,
    anchor: Option<(u16, u16)>,
    pointer_bottom_right: bool,
    items: &[(&'static str, &'static str, String)],
) -> Result<bool, Box<dyn std::error::Error>> {
    let pane = source_pane().ok_or("sidebar source pane unavailable")?;
    if !safe_target(&pane, '%') {
        return Err("invalid sidebar pane id".into());
    }
    let client = current_client()?;
    let (x, y) = menu_position(anchor, pointer_bottom_right);
    let mut args = vec![
        "display-menu".to_owned(),
        "-M".to_owned(),
        "-O".to_owned(),
        "-c".to_owned(),
        client.clone(),
        "-T".to_owned(),
        title.to_owned(),
        "-t".to_owned(),
        pane.clone(),
        "-x".to_owned(),
        x,
        "-y".to_owned(),
        y,
    ];
    for (label, key, command) in items {
        args.extend([(*label).to_owned(), (*key).to_owned(), command.clone()]);
    }
    args.push(String::new());
    if popup_mode() {
        let parent = format!(
            "{} sidebar --popup --pane {} --client {}",
            shell_quote(&std::env::current_exe()?.to_string_lossy()),
            shell_quote(&pane),
            shell_quote(&client)
        );
        args.extend([
            crate::menu_footer::back_label().into(),
            "BSpace".into(),
            format!("run-shell -b {}", shell_quote(&parent)),
        ]);
    }
    args.extend(crate::menu_footer::close_item(!popup_mode()));
    let mut command_args = vec!["display-menu".to_owned(), "--".to_owned()];
    command_args.extend(args.into_iter().skip(1));
    run_workbench(
        &command_args.iter().map(String::as_str).collect::<Vec<_>>(),
        true,
    )
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
    #[test]
    fn acknowledge_attention_tolerates_expired_events_but_preserves_other_errors() {
        use crate::ipc::{Response, read_request, write_response};
        use std::os::unix::net::UnixListener;

        for failure in [
            None,
            Some("attention event not found"),
            Some("permission denied"),
        ] {
            let dir = tempfile::tempdir().unwrap();
            let socket = dir.path().join("ack.sock");
            let listener = UnixListener::bind(&socket).unwrap();
            let worker = std::thread::spawn(move || {
                let (stream, _) = listener.accept().unwrap();
                let request = read_request(&stream).unwrap();
                assert_eq!(request.method, "attention.ack");
                assert_eq!(request.params["event_id"], "expired-event");
                let response = match failure {
                    Some(message) => Response::error(request.id, "request_failed", message),
                    None => {
                        Response::success(request.id, serde_json::json!({"acknowledged": true}))
                    }
                };
                write_response(&stream, &response).unwrap();
            });
            let result = super::acknowledge_attention(&socket, "expired-event");
            worker.join().unwrap();
            assert_eq!(result.is_ok(), failure != Some("permission denied"));
        }
        let dir = tempfile::tempdir().unwrap();
        assert!(matches!(
            super::acknowledge_attention(&dir.path().join("missing.sock"), "event"),
            Err(crate::ipc::ClientError::Connect { .. })
        ));
    }

    use super::*;

    #[test]
    fn sidebar_labels_use_the_full_cell_budget_without_ellipsis() {
        assert_eq!(fit_text("中文字符", 5), "中文");
        assert_eq!(fit_text("abcdefgh", 5), "abcde");
        assert_eq!(fit_text("中文字符", 0), "");
        let line = context_line("abcdefgh", 8, false);
        assert_eq!(line.to_string(), "   abcde");
    }

    #[test]
    fn stale_banner_clicks_stay_on_the_rendered_agent() {
        let rows = ["agent A", "A subtitle", "agent B", "B subtitle"];
        let viewport = SidebarViewport::new(10, rows.len(), Some("stale · reconnecting"));
        assert_eq!(viewport.row_at(0, 0, rows.len()), None);
        assert_eq!(
            rows[viewport.row_at(5, 0, rows.len()).unwrap()],
            "A subtitle"
        );
        assert_eq!(rows[viewport.row_at(6, 0, rows.len()).unwrap()], "agent B");
        assert_eq!(viewport.row_at(8, 0, rows.len()), None);
    }

    #[test]
    fn scrolled_viewport_excludes_banner_overflow_and_footer() {
        // Eight content lines: actions + heading + banner + divider + three rows + overflow;
        // the popup footer follows and must not activate any hidden row.
        let viewport = SidebarViewport::new(8, 20, Some("stale · reconnecting"));
        assert_eq!(viewport.row_height, 3);
        assert_eq!(viewport.row_at(0, 7, 20), None);
        assert_eq!(viewport.row_at(1, 7, 20), None);
        assert_eq!(viewport.row_at(2, 7, 20), None);
        assert_eq!(viewport.row_at(3, 7, 20), None);
        assert_eq!(viewport.row_at(4, 7, 20), Some(7));
        assert_eq!(viewport.row_at(6, 7, 20), Some(9));
        for row in [7, 8, 9] {
            assert_eq!(viewport.row_at(row, 7, 20), None);
        }
    }

    #[test]
    fn banner_uses_space_even_when_rows_previously_fitted() {
        let normal = SidebarViewport::new(7, 4, None);
        assert_eq!(normal.row_at(6, 0, 4), Some(3));
        let stale = SidebarViewport::new(7, 4, Some("stale · reconnecting"));
        assert_eq!(stale.row_height, 2);
        assert_eq!(stale.row_at(6, 0, 4), None);
        let overflow = SidebarViewport::new(6, 10, None);
        assert_eq!(overflow.row_at(4, 0, 10), Some(1));
        assert_eq!(overflow.row_at(5, 0, 10), None);
    }

    #[test]
    fn empty_disconnected_and_tiny_viewports_have_no_agent_targets() {
        for height in [0, 1] {
            let viewport = SidebarViewport::new(height, 10, Some("stale · reconnecting"));
            for row in 0..3 {
                assert_eq!(viewport.row_at(row, 0, 10), None);
            }
        }
        assert_eq!(SidebarViewport::new(10, 0, None).row_at(0, 0, 0), None);
        let disconnected = SidebarViewport::new(10, 4, Some("disconnected"));
        assert_eq!(disconnected.row_at(1, 0, 4), None);
    }

    #[test]
    fn connection_notice_preserves_a_stale_snapshot() {
        assert_eq!(connection_notice(true, false), Some("disconnected"));
        assert_eq!(connection_notice(true, true), Some("stale · reconnecting"));
        assert_eq!(connection_notice(false, true), None);
    }
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
    fn agents_precede_sessions_with_one_blank_separator() {
        let snapshot: Snapshot =
            serde_json::from_str(include_str!("../tests/golden/snapshot-v1.json")).unwrap();
        let rows = build_rows(&snapshot, false, AgentSort::Grouped);
        let sessions = rows
            .iter()
            .position(|row| matches!(row, Row::Section("sessions")))
            .unwrap();
        assert!(matches!(rows.first(), Some(Row::AgentGroup(_))));
        assert!(!rows.iter().any(|row| matches!(row, Row::AgentSection(_))));
        assert!(matches!(rows.get(sessions - 1), Some(Row::Spacer)));
        assert!(!matches!(rows.get(sessions - 2), Some(Row::Spacer)));
    }

    #[test]
    fn top_actions_span_left_and_right_without_help() {
        let list = NavigationList::<Action>::default();
        let actions = action_regions(Rect::new(0, 0, 40, 20), AgentSort::Grouped, true);
        assert_eq!(list.hit(0, 0, &actions), Some(Hit::Action(Action::New)));
        assert_eq!(list.hit(20, 0, &actions), None);
        assert_eq!(list.hit(34, 0, &actions), Some(Hit::Action(Action::Menu)));
        assert_eq!(list.hit(33, 19, &actions), Some(Hit::Action(Action::Close)));
        assert_eq!(list.hit(38, 1, &actions), Some(Hit::Action(Action::Sort)));
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
                | Row::AgentGroup(_)
                | Row::SessionSub(_)
                | Row::Agent(_)
                | Row::AgentSub(_, _)
                | Row::Detail(_, _)
                | Row::Conversation(_, _)
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

        let rows = build_rows(&snapshot, false, AgentSort::Grouped);
        let groups: Vec<_> = rows
            .iter()
            .filter_map(|row| match row {
                Row::AgentGroup(name) => Some(name.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(groups, ["alpha", "zeta"]);
        let agents: Vec<_> = rows
            .iter()
            .filter_map(|row| match row {
                Row::Agent(agent) => Some(agent.instance_id.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(agents, ["first", "pane", "late"]);
        for row in &rows {
            if let Row::AgentSub(agent, _) = row {
                let line = render_row(row, false, 40);
                let text: String = line
                    .spans
                    .iter()
                    .map(|span| span.content.as_ref())
                    .collect();
                assert!(!text.contains(&agent.target.session_name));
                assert_eq!(
                    selection_key(row),
                    Some(format!("agent:{}", agent.instance_id))
                );
            } else if matches!(row, Row::AgentGroup(_)) {
                assert!(selection_key(row).is_none());
            }
        }
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
        let rows = build_rows(&snapshot, false, AgentSort::Prioritized);
        assert!(!rows.iter().any(|row| matches!(row, Row::AgentGroup(_))));
        let agents: Vec<_> = rows
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
            [Row::Agent(first), Row::AgentSub(second, _)] if first.instance_id == second.instance_id
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
        for row in [
            Row::Agent(agent.clone()),
            Row::AgentSub(agent, AgentSort::Grouped),
        ] {
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
    fn agent_card_shows_thread_name_with_progress_session_and_status_below() {
        let snapshot: Snapshot =
            serde_json::from_str(include_str!("../tests/golden/snapshot-v1.json")).unwrap();
        let mut agent = snapshot.agents[0].clone();
        agent.target.session_name = "Word".into();
        agent.target.window_name = "Formula".into();
        agent.thread_name = Some("Readable thread".into());
        agent.task_progress = Some(crate::model::TaskProgress {
            completed: 2,
            total: 5,
        });
        let first = render_row(&Row::Agent(agent.clone()), false, 40);
        let second = render_row(
            &Row::AgentSub(agent.clone(), AgentSort::Prioritized),
            false,
            40,
        );
        let text = |line: Line<'_>| {
            line.spans
                .iter()
                .map(|span| span.content.as_ref())
                .collect::<String>()
        };
        let first = text(first);
        let second = text(second);
        assert!(first.contains("Readable thread"));
        assert!(!first.contains(&agent.target.window_name));
        assert!(second.contains(&agent.target.session_name));
        assert!(second.contains("2/5"));
        assert!(!first.contains(agent_kind_name(agent.kind)));
        assert!(second.contains(agent_kind_name(agent.kind)));
        assert!(second.contains(&agent_status(&agent)));
    }

    #[test]
    fn long_chinese_topics_and_session_names_fit_narrow_agent_cards() {
        let snapshot: Snapshot =
            serde_json::from_str(include_str!("../tests/golden/snapshot-v1.json")).unwrap();
        let mut agent = snapshot.agents[0].clone();
        agent.target.session_name = "文档体验优化方向".into();
        agent.target.window_name = "修复公式插入时的布局问题".into();
        for width in [12, 26, 36, 80] {
            for row in [
                Row::Agent(agent.clone()),
                Row::AgentSub(agent.clone(), AgentSort::Grouped),
            ] {
                assert_eq!(render_row(&row, false, width).width(), usize::from(width));
                assert_eq!(render_row(&row, true, width).width(), usize::from(width));
            }
        }
        agent.target.window_name.clear();
        assert_eq!(agent_topic(&agent), agent.target.session_name);
    }

    #[test]
    fn card_titles_are_white_while_status_stays_on_icons_and_agent_status() {
        let snapshot: Snapshot =
            serde_json::from_str(include_str!("../tests/golden/snapshot-v1.json")).unwrap();
        let title_color = Color::Rgb(235, 235, 245);

        let mut session = snapshot.sessions[0].clone();
        for agent_count in [0, 2] {
            session.agent_count = agent_count;
            let line = render_row(&Row::Session(session.clone()), false, 40);
            assert_eq!(
                line.spans[0].style.fg,
                Some(state_color(session.rollup_state))
            );
            assert_eq!(line.spans[1].style.fg, Some(title_color));
        }

        let agent = snapshot.agents[0].clone();
        let title = render_row(&Row::Agent(agent.clone()), false, 40);
        assert_eq!(
            title.spans[0].style.fg,
            Some(state_color(agent.display_state))
        );
        assert_eq!(title.spans[1].style.fg, Some(title_color));

        let subtitle = render_row(&Row::AgentSub(agent.clone(), AgentSort::Grouped), false, 40);
        assert_eq!(
            subtitle.spans[2].style.fg,
            Some(state_color(agent.display_state))
        );

        for row in [Row::Session(session), Row::Agent(agent)] {
            let selected = render_row(&row, true, 40);
            assert!(
                !selected.spans[0]
                    .style
                    .add_modifier
                    .contains(Modifier::BOLD)
            );
            assert!(
                selected.spans[1]
                    .style
                    .add_modifier
                    .contains(Modifier::BOLD)
            );
        }
    }

    #[test]
    fn structural_labels_and_secondary_actions_use_theme_muted_without_dim() {
        let section = render_row(&Row::Section("sessions"), false, 40);
        let agent_section = render_row(&Row::AgentSection(AgentSort::Prioritized), false, 40);
        let actions = render_actions(40, None);

        for line in [section, agent_section, actions] {
            for span in line
                .spans
                .iter()
                .filter(|span| !span.content.trim().is_empty())
            {
                assert_eq!(span.style.fg, Some(Color::DarkGray));
                assert!(!span.style.add_modifier.contains(Modifier::DIM));
            }
        }
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
        let mut list = NavigationList::<()>::default();
        list.replace_rows(rows.iter().map(selection_key).collect());
        list.select(2);
        list.replace_rows(rows.iter().map(selection_key).collect());
        assert_eq!(selection_key(&rows[list.selected]), Some(key));
        assert_eq!(list.selected, 2);
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
        let line = render_row(
            &Row::AgentSub(snapshot.agents[0].clone(), AgentSort::Grouped),
            false,
            40,
        );
        let rendered: String = line
            .spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect();
        assert!(rendered.contains(" ! · "));
        assert!(!rendered.contains("conflict"));
    }

    #[test]
    fn blocked_display_fallback_uses_compact_hook_conflict_marker() {
        let mut snapshot: Snapshot =
            serde_json::from_str(include_str!("../tests/golden/snapshot-v1.json")).unwrap();
        let agent = &mut snapshot.agents[0];
        agent.base_state = crate::model::BaseState::Working;
        agent.display_state = DisplayState::Blocked;
        agent.hook_health = crate::model::HookHealth::Conflict;
        assert_eq!(agent_status(agent), "input !");
    }

    #[test]
    fn queued_question_uses_compact_idle_marker() {
        let mut snapshot: Snapshot =
            serde_json::from_str(include_str!("../tests/golden/snapshot-v1.json")).unwrap();
        let agent = &mut snapshot.agents[0];
        agent.display_state = DisplayState::Idle;
        agent.rule_id = Some("codex-queued-question-idle".into());
        assert!(agent_status(agent).starts_with("idle ?"));
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
