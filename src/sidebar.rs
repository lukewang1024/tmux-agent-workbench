use std::collections::{HashMap, HashSet};
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
    AgentGroup(String),
    Session(SessionSnapshot),
    SessionSub(SessionSnapshot),
    Agent(AgentSnapshot),
    AgentSub(AgentSnapshot, AgentSort),
    Team(AgentSnapshot, TeamSummary),
    TeamSub(AgentSnapshot, AgentSort, TeamSummary),
    TeamWorker(AgentSnapshot, WorkerDisplay),
    TeamWorkerSub(AgentSnapshot, WorkerDisplay),
    Detail(String, String),
    Conversation(AgentSnapshot, ConversationSnapshot),
    Spacer,
}

#[derive(Debug, Clone)]
struct TeamSummary {
    key: String,
    expanded: bool,
    workers: usize,
    blocked: usize,
    done: usize,
}

#[derive(Debug, Clone)]
struct TeamMember {
    key: String,
    window: String,
    name: String,
    brief: Option<String>,
    assignment: Option<String>,
    pane_missing: bool,
    team_closed: bool,
}

#[derive(Debug, Clone)]
struct WorkerDisplay {
    name: String,
    title: String,
}

type TeamIndex = HashMap<String, TeamMember>;

#[derive(Debug, Clone)]
struct FocusCard {
    window: FocusWindow,
    agent: Option<AgentSnapshot>,
    team: Option<TeamSummary>,
}

#[derive(Debug, Clone)]
struct FocusWindow {
    id: String,
    session_id: String,
    session_name: String,
    name: String,
    index: u32,
    pane: String,
    state: DisplayState,
    assistant: bool,
}

type FocusWindows = HashMap<String, FocusWindow>;

fn assistant_window(paths: &Paths, snapshot: &Snapshot) -> Option<String> {
    let path = paths.state_dir.join("personal-assistant.json");
    let metadata = std::fs::symlink_metadata(&path).ok()?;
    if !metadata.is_file() || metadata.len() > 8192 {
        return None;
    }
    let value: serde_json::Value = serde_json::from_slice(&std::fs::read(path).ok()?).ok()?;
    if value.get("schema_version")?.as_u64()? != 1 {
        return None;
    }
    let kind = value.get("kind")?.as_str()?;
    let thread = value.get("thread_id")?.as_str()?;
    if !matches!(kind, "codex" | "claude" | "trae" | "opencode")
        || thread.is_empty()
        || thread.len() > 256
        || thread.chars().any(char::is_control)
    {
        return None;
    }
    let windows: HashSet<_> = snapshot
        .agents
        .iter()
        .filter(|agent| {
            !agent.exited
                && !agent.stale
                && agent.process.is_some()
                && agent.relay_focus.is_none()
                && agent_kind_name(agent.kind) == kind
                && agent.hook_session_id.as_deref() == Some(thread)
        })
        .map(|agent| agent.target.window_id.clone())
        .collect();
    (windows.len() == 1)
        .then(|| windows.into_iter().next())
        .flatten()
}

// Only explicit, already received assignments replace the original thread
// title. Ordinary follow-ups and pending messages need not change the task.
fn received_assignment(state: &serde_json::Value, name: &str, cursor: u64) -> Option<String> {
    state["messages"]
        .as_array()?
        .iter()
        .rev()
        .find_map(|message| {
            if message["from"] != "lead"
                || message["to"] != name
                || message["type"] != "send"
                || message["id"].as_u64().is_none_or(|id| id > cursor)
            {
                return None;
            }
            let text = message["text"].as_str()?.trim();
            let content = [
                "新只读任务：",
                "新只读任务:",
                "新任务：",
                "新任务:",
                "New task:",
                "NEW TASK:",
            ]
            .iter()
            .find_map(|prefix| text.strip_prefix(prefix))?;
            task_title(content)
        })
}

fn task_title(text: &str) -> Option<String> {
    let text = text.trim();
    let text = text
        .strip_prefix("目标：")
        .or_else(|| text.strip_prefix("目标:"))
        .unwrap_or(text)
        .trim();
    let first = text.split(['\n', '。', '；', ';']).next()?.trim();
    let clean: String = first
        .chars()
        .filter(|c| !c.is_control())
        .take(160)
        .collect();
    (!clean.is_empty()).then_some(clean)
}

fn worker_display(agent: &AgentSnapshot, member: &TeamMember) -> WorkerDisplay {
    let thread = agent
        .thread_name
        .as_deref()
        .map(str::trim)
        .filter(|s| meaningful_topic(s));
    let title = member
        .assignment
        .clone()
        .or_else(|| thread.and_then(task_title))
        .or_else(|| member.brief.as_deref().and_then(task_title))
        .unwrap_or_else(|| member.name.clone());
    WorkerDisplay {
        name: member.name.clone(),
        title,
    }
}

/// Live grouping requires a pane marker and matching state. Retained snapshots
/// of missing worker panes can also be matched against validated team state.
fn read_inventory(
    paths: &Paths,
    server: &ServerIdentity,
    snapshot: &Snapshot,
) -> Option<(TeamIndex, FocusWindows)> {
    let inventory = crate::tmux::Tmux::new(server.clone())
        .output_with_timeout(
            &[
                "list-panes",
                "-a",
                "-F",
                "#{window_id}\t#{pane_id}\t#{@agent_team}\t#{pane_dead}\t#{@agent_focus}\t#{session_id}\t#{session_name}\t#{window_name}\t#{window_index}\t#{pane_active}\t#{@workbench_window_state}\t#{@pane_role}\t#{@workbench_assistant}",
            ],
            Duration::from_millis(250),
        )
        .ok()?;
    let mut marked: HashMap<(String, String), HashSet<String>> = HashMap::new();
    let mut all_panes = HashSet::new();
    let mut focused = FocusWindows::new();
    let saved_assistant = assistant_window(paths, snapshot);
    for line in inventory.lines() {
        let fields: Vec<_> = line.split('\t').collect();
        if fields.len() == 13 {
            all_panes.insert(fields[1].to_owned());
            let assistant = fields[12] == "1" || saved_assistant.as_deref() == Some(fields[0]);
            if (fields[4] == "1" || assistant)
                && fields[11] != "sidebar"
                && safe_target(fields[0], '@')
                && safe_target(fields[1], '%')
                && safe_target(fields[5], '$')
            {
                let state = match fields[10] {
                    "working" => DisplayState::Working,
                    "blocked" => DisplayState::Blocked,
                    "done" => DisplayState::Done,
                    "idle" => DisplayState::Idle,
                    _ => DisplayState::Unknown,
                };
                let window = focused
                    .entry(fields[0].to_owned())
                    .or_insert_with(|| FocusWindow {
                        id: fields[0].to_owned(),
                        pane: fields[1].to_owned(),
                        session_id: fields[5].to_owned(),
                        session_name: fields[6].to_owned(),
                        name: fields[7].to_owned(),
                        index: fields[8].parse().unwrap_or(0),
                        state,
                        assistant,
                    });
                if fields[9] == "1" && window.session_id == fields[5] {
                    window.pane = fields[1].to_owned();
                }
            }
        }
        if fields.len() != 13
            || fields[3] != "0"
            || fields[2].is_empty()
            || !fields[2]
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
        {
            continue;
        }
        marked
            .entry((fields[0].into(), fields[2].into()))
            .or_default()
            .insert(fields[1].into());
    }
    let missing: HashSet<_> = snapshot
        .agents
        .iter()
        .filter(|agent| !all_panes.contains(&agent.target.pane_id))
        .map(|agent| (agent.target.window_id.clone(), agent.target.pane_id.clone()))
        .collect();
    let root = paths.state_dir.parent()?.join("agent-team");
    let mut candidates: HashSet<String> = marked.keys().map(|(_, id)| id.clone()).collect();
    if !missing.is_empty() {
        if let Ok(entries) = std::fs::read_dir(&root) {
            for entry in entries.flatten() {
                if let Some(id) = entry.file_name().to_str().filter(|id| {
                    !id.is_empty()
                        && id
                            .bytes()
                            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
                }) {
                    candidates.insert(id.to_owned());
                }
            }
        }
    }
    let mut index = TeamIndex::new();
    // A random server-lifetime token lets a deliberately rebound team retain
    // provenance even after its last window disappears.
    let restore_token = crate::tmux::Tmux::new(server.clone())
        .output_with_timeout(
            &["show-option", "-gqv", "@workbench_team_restore_token"],
            Duration::from_millis(250),
        )
        .unwrap_or_default();
    let restore_token = restore_token.trim();
    for id in candidates {
        let file = root.join(&id).join("state.json");
        let state = std::fs::metadata(&file)
            .ok()
            .filter(|m| m.len() < 4 * 1024 * 1024)
            .and_then(|_| std::fs::read(file).ok())
            .and_then(|data| serde_json::from_slice::<serde_json::Value>(&data).ok());
        let Some(state) = state else { continue };
        // A tmux socket path and pane IDs can be reused after a server restart.
        // Old team directories must not claim snapshots from the new server.
        if let (Ok(team_created), Ok(socket_created)) = (
            std::fs::metadata(root.join(&id)).and_then(|m| m.created()),
            std::fs::metadata(&server.socket_path).and_then(|m| m.created()),
        ) {
            if team_created < socket_created {
                // A restored team is rebound explicitly before its assistants
                // restart; old directories alone must not claim reused IDs.
                let restored = !restore_token.is_empty()
                    && state["workbench_restore_token"].as_str() == Some(restore_token);
                if !restored {
                    continue;
                }
            }
        }
        let Some(window) = state["window"].as_str() else {
            continue;
        };
        if state["id"] != id
            || state["socket"].as_str().map(std::path::Path::new)
                != Some(server.socket_path.as_path())
        {
            continue;
        }
        let Some(members) = state["members"].as_object() else {
            continue;
        };
        let Some(lead) = members.get("lead").and_then(|m| m["pane"].as_str()) else {
            continue;
        };
        let panes = marked.get(&(window.to_owned(), id.clone()));
        let lead_present = panes.is_some_and(|panes| panes.contains(lead));
        let closed = state["closed"] == true;
        // A duplicate pane makes ownership ambiguous: never hide that team.
        let mut seen = HashSet::new();
        if members
            .values()
            .filter_map(|m| m["pane"].as_str())
            .filter(|pane| !pane.is_empty())
            .any(|pane| !seen.insert(pane))
        {
            continue;
        }
        let key = format!("{window}:{id}");
        for (name, member) in members {
            let Some(pane) = member["pane"].as_str().filter(|pane| !pane.is_empty()) else {
                continue;
            };
            let pane_missing = !all_panes.contains(pane);
            let marked_member = panes.is_some_and(|panes| panes.contains(pane));
            if (lead_present && !closed && marked_member)
                || (name != "lead"
                    && ((closed && marked_member)
                        || (pane_missing
                            && missing.contains(&(window.to_owned(), pane.to_owned())))))
            {
                index.insert(
                    pane.into(),
                    TeamMember {
                        key: key.clone(),
                        window: window.to_owned(),
                        name: name.clone(),
                        brief: member["brief"].as_str().map(str::to_owned),
                        assignment: received_assignment(
                            &state,
                            name,
                            member["cursor"].as_u64().unwrap_or(0),
                        ),
                        pane_missing,
                        team_closed: closed,
                    },
                );
            }
        }
    }
    Some((index, focused))
}

/// Shared geometry for rendering and hit testing. Status, overflow and popup
/// footer lines and the pinned action bar never map to an agent row.
#[derive(Debug, Clone, Copy)]
struct SidebarViewport {
    first_row: usize,
    row_height: usize,
}

impl SidebarViewport {
    fn new(content_height: usize, total: usize, notice: Option<&str>, focus_cards: usize) -> Self {
        // The status bar and pinned cards stay above the scrolling body.
        let first_row = 2 + focus_cards * 2 + usize::from(notice.is_some());
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

#[derive(Debug, Clone, PartialEq, Eq)]
enum Action {
    Agents,
    Pinned,
    Menu,
    Close,
    Sort,
    Focus(String),
}

pub fn run(paths: &Paths, server: &ServerIdentity) -> Result<(), Box<dyn std::error::Error>> {
    if let Ok(config) = Config::load(&paths.config_file()) {
        crate::ui_language::set_configured_language(&config.ui.language);
    }
    let runtime_identity = crate::layout::executable_identity()?;
    // Resolve the first frame before switching to the alternate screen. Some
    // remote clients do not paint an initially empty fullscreen popup until a
    // later input event; entering with real rows makes the UI visible at once.
    let initial_snapshot = fetch_snapshot(&paths.socket_for_server(&server.key));
    let _screen = Screen::enter()?;
    let mut terminal = Terminal::new(crate::popup_surface::Surface::from_env(
        CrosstermBackend::new(stdout()),
    )?)?;
    let result = event_loop(
        &mut terminal,
        paths,
        server,
        initial_snapshot,
        &runtime_identity,
    );
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
    runtime_identity: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    let socket = paths.socket_for_server(&server.key);
    let fetch_socket = socket.clone();
    let (refresh_tx, refresh_rx) = mpsc::sync_channel::<bool>(1);
    let visibility_tmux = crate::tmux::Tmux::new(server.clone());
    let visibility_pane = std::env::var("TMUX_PANE").ok();
    let is_popup = popup_mode();
    let (snapshot_tx, snapshot_rx) = mpsc::sync_channel(1);
    let team_paths = paths.clone();
    let team_server = server.clone();
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
            let result = fetch_snapshot(&fetch_socket)
                .map_err(|error| error.to_string())
                .and_then(|snapshot| sidebar_state(&team_paths, &team_server, snapshot));
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
    let initial_state = initial_snapshot
        .map_err(|error| error.to_string())
        .and_then(|snapshot| sidebar_state(paths, server, snapshot));
    let (mut snapshot, mut teams, mut focused) = match initial_state {
        Ok((snapshot, teams, focused)) => (Some(snapshot), teams, focused),
        Err(_) => (None, TeamIndex::new(), FocusWindows::new()),
    };
    let mut expanded = HashSet::new();
    let mut rows = snapshot
        .as_ref()
        .map(|snapshot| build_team_rows(snapshot, detailed, agent_sort, &teams, &expanded))
        .unwrap_or_default();
    let mut focus_cards = build_focus_cards(&rows, &focused);
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
    let mut runtime_registered = is_popup;
    loop {
        if let Ok((result, now_visible)) = snapshot_rx.try_recv() {
            visible = now_visible;
            next_refresh = Instant::now() + refresh_interval(visible);
            match result {
                Ok((fetched, fetched_teams, fetched_focused)) => {
                    teams = fetched_teams;
                    focused = fetched_focused;
                    rows = build_team_rows(&fetched, detailed, agent_sort, &teams, &expanded);
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
                    // Keep the snapshot and its verified team identities together
                    // through a scan, IPC, or tmux inventory hiccup. Never mix
                    // a new snapshot with an empty or obsolete team index.
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
                    rows = build_team_rows(snapshot, detailed, agent_sort, &teams, &expanded);
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
        if dirty {
            focus_cards = build_focus_cards(&rows, &focused);
        }
        let shown_focus = visible_focus_cards(content_height, notice, focus_cards.len());
        let viewport = SidebarViewport::new(content_height, rows.len(), notice, shown_focus);
        navigation.set_viewport(Rect::new(
            0,
            viewport.first_row as u16,
            size.width,
            viewport.row_height as u16,
        ));
        if dirty {
            terminal.draw(|frame| {
                let area = frame.area();
                let mut header = vec![render_header(
                    area.width,
                    agent_sort,
                    focus_cards.len(),
                    shown_focus,
                    footer_hover.as_ref(),
                )];
                for (index, card) in focus_cards.iter().take(shown_focus).enumerate() {
                    let highlighted =
                        footer_hover.as_ref() == Some(&Action::Focus(card.window.id.clone()));
                    header.extend(render_focus_card(card, index + 1, highlighted, area.width));
                }
                if let Some(notice) = notice {
                    let notice = match notice {
                        "disconnected" => crate::ui_language::text("disconnected", "已断开"),
                        "stale · reconnecting" => {
                            crate::ui_language::text("stale · reconnecting", "数据过期 · 正在重连")
                        }
                        _ => notice,
                    };
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
                        Paragraph::new(crate::ui_language::text("no sessions", "暂无会话"))
                            .style(muted_style()),
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
                        Paragraph::new(format!(
                            "↕ {hidden} {}",
                            crate::ui_language::text("hidden", "项已隐藏")
                        )),
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
                        Paragraph::new(render_close(area.width, footer_hover.as_ref())),
                        Rect::new(0, area.height - 1, area.width, 1),
                    );
                }
            })?;
            if !runtime_registered {
                crate::layout::register_sidebar_runtime(server, runtime_identity)?;
                runtime_registered = true;
            }
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
                if key.code != KeyCode::Char('p') {
                    // Once the user navigates with keys, act on the keyboard
                    // selection instead of a previously hovered Focus card.
                    footer_hover = None;
                }
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
                    KeyCode::Char(number @ '1'..='9') => {
                        let index = number as usize - '1' as usize;
                        if let Some(card) = focus_cards.iter().take(shown_focus).nth(index)
                            && activate_focus(card)?
                            && popup_mode()
                        {
                            return Ok(());
                        }
                    }
                    KeyCode::Char('p') => {
                        let window = match footer_hover.as_ref() {
                            Some(Action::Focus(window)) => Some(window.as_str()),
                            _ if navigation.selection_visible => {
                                selected_window(&rows, navigation.selected)
                            }
                            _ => None,
                        };
                        if let Some(window) = window {
                            let assistant = focused.get(window).is_some_and(|item| item.assistant);
                            if let Err(error) = toggle_focus(server, window, assistant) {
                                // A task can close between rendering and the keypress.
                                // Refresh the list instead of terminating the sidebar.
                                append_sidebar_log(paths, &format!("focus-toggle error={error}"));
                            }
                            let _ = refresh_tx.try_send(true);
                            next_refresh = Instant::now();
                        }
                    }
                    _ if navigation.navigate(key.code) => {}
                    KeyCode::Char(' ' | 'h' | 'l') | KeyCode::Left | KeyCode::Right => {
                        if let Some(Row::Team(_, summary) | Row::TeamSub(_, _, summary)) =
                            rows.get(navigation.selected)
                        {
                            let team_key = summary.key.clone();
                            let open =
                                match key_event_direction(key.code, expanded.contains(&team_key)) {
                                    Some(open) => open,
                                    None => expanded.contains(&team_key),
                                };
                            if open {
                                expanded.insert(team_key);
                            } else {
                                expanded.remove(&team_key);
                            }
                            if let Some(snapshot) = &snapshot {
                                rows = build_team_rows(
                                    snapshot, detailed, agent_sort, &teams, &expanded,
                                );
                                navigation.replace_rows(rows.iter().map(selection_key).collect());
                            }
                        }
                    }
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
                            &focused,
                        )? {
                            return Ok(());
                        }
                    }
                    KeyCode::Char('d') => {
                        detailed = !detailed;
                        if let Some(snapshot) = &snapshot {
                            rows =
                                build_team_rows(snapshot, detailed, agent_sort, &teams, &expanded);
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
                    &focus_cards[..shown_focus],
                    focus_cards.len(),
                );
                let hit = navigation.mouse(mouse, &actions);
                let hit = if mouse.kind == MouseEventKind::Down(MouseButton::Right) {
                    navigation
                        .hit(mouse.column, mouse.row, &actions)
                        .filter(|hit| matches!(hit, Hit::Item(_) | Hit::Action(Action::Focus(_))))
                } else {
                    hit
                };
                footer_hover = match &navigation.hovered {
                    Some(Hit::Action(action)) => Some(action.clone()),
                    _ => None,
                };
                match hit {
                    Some(Hit::Action(Action::Close)) => return Ok(()),
                    Some(Hit::Action(Action::Agents)) => {
                        navigation.navigate(KeyCode::Home);
                        navigation.scroll = 0;
                    }
                    Some(Hit::Action(Action::Pinned)) => {
                        if show_pinned_menu(&focus_cards, Some((mouse.column, mouse.row)))? {
                            return Ok(());
                        }
                    }
                    Some(Hit::Action(Action::Menu)) => {
                        if show_global_menu(Some((mouse.column, mouse.row)))? {
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
                            rows =
                                build_team_rows(snapshot, detailed, agent_sort, &teams, &expanded);
                            navigation = NavigationList::default();
                            navigation.replace_rows(rows.iter().map(selection_key).collect());
                        }
                    }
                    Some(Hit::Action(Action::Focus(window))) => {
                        if let Some(card) = focus_cards
                            .iter()
                            .take(shown_focus)
                            .find(|card| card.window.id == window)
                        {
                            let navigated =
                                if mouse.kind == MouseEventKind::Down(MouseButton::Right) {
                                    show_focus_menu(card, Some((mouse.column, mouse.row)))?
                                } else {
                                    activate_focus(card)?
                                };
                            if popup_mode() && navigated {
                                return Ok(());
                            }
                        }
                    }
                    Some(Hit::Item(_)) => {
                        let clicked_row = navigation.scroll
                            + usize::from(mouse.row.saturating_sub(viewport.first_row as u16));
                        if mouse.kind == MouseEventKind::Up(MouseButton::Left) && mouse.column <= 2
                        {
                            if let Some(Row::Team(_, summary)) = rows.get(clicked_row) {
                                let key = summary.key.clone();
                                if !expanded.remove(&key) {
                                    expanded.insert(key);
                                }
                                if let Some(snapshot) = &snapshot {
                                    rows = build_team_rows(
                                        snapshot, detailed, agent_sort, &teams, &expanded,
                                    );
                                    navigation
                                        .replace_rows(rows.iter().map(selection_key).collect());
                                }
                                continue;
                            }
                        }
                        let navigated = if mouse.kind == MouseEventKind::Down(MouseButton::Right) {
                            show_row_menu(
                                &rows,
                                navigation.selected,
                                Some((mouse.column, mouse.row)),
                                &focused,
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

// A snapshot and its team identities form one display frame. A temporary
// tmux inventory failure must not promote retained workers to ordinary agents.
fn sidebar_state(
    paths: &Paths,
    server: &ServerIdentity,
    snapshot: Snapshot,
) -> Result<(Snapshot, TeamIndex, FocusWindows), String> {
    let (teams, focused) = read_inventory(paths, server, &snapshot)
        .ok_or_else(|| "team pane inventory unavailable".to_owned())?;
    Ok((snapshot, teams, focused))
}

#[cfg(test)]
fn build_rows(snapshot: &Snapshot, detailed: bool, agent_sort: AgentSort) -> Vec<Row> {
    build_team_rows(
        snapshot,
        detailed,
        agent_sort,
        &TeamIndex::new(),
        &HashSet::new(),
    )
}

fn key_event_direction(key: KeyCode, is_open: bool) -> Option<bool> {
    match key {
        KeyCode::Char(' ') => Some(!is_open),
        KeyCode::Left | KeyCode::Char('h') => Some(false),
        KeyCode::Right | KeyCode::Char('l') => Some(true),
        _ => None,
    }
}

fn build_team_rows(
    snapshot: &Snapshot,
    detailed: bool,
    agent_sort: AgentSort,
    teams: &TeamIndex,
    expanded: &HashSet<String>,
) -> Vec<Row> {
    let mut agents = snapshot.agents.clone();
    agents.sort_by_key(stable_agent_key);
    // Only group live agents when the validated lead is present. Unmatched
    // agents remain visible independently.
    let mut leads = HashMap::new();
    for agent in &agents {
        if !agent.exited && !agent.stale {
            if let Some(member) = teams
                .get(&agent.target.pane_id)
                .filter(|m| m.name == "lead" && m.window == agent.target.window_id)
            {
                leads.insert(member.key.clone(), agent.instance_id.clone());
            }
        }
    }
    let mut workers: HashMap<String, Vec<(AgentSnapshot, WorkerDisplay)>> = HashMap::new();
    for agent in &agents {
        if !agent.exited && !agent.stale {
            if let Some(member) = teams.get(&agent.target.pane_id).filter(|m| {
                m.name != "lead"
                    && !m.pane_missing
                    && m.window == agent.target.window_id
                    && leads.contains_key(&m.key)
            }) {
                workers
                    .entry(member.key.clone())
                    .or_default()
                    .push((agent.clone(), worker_display(agent, member)));
            }
        }
    }
    let grouped_keys: HashSet<String> = workers.keys().cloned().collect();
    let mut entries: Vec<(
        AgentSnapshot,
        Option<(String, Vec<(AgentSnapshot, WorkerDisplay)>)>,
    )> = Vec::new();
    for agent in agents {
        // A verified team state retains worker pane IDs after stop. Suppress
        // their cached snapshots instead of promoting them to top-level rows.
        if teams.get(&agent.target.pane_id).is_some_and(|member| {
            member.name != "lead"
                && member.window == agent.target.window_id
                && (agent.exited || member.pane_missing || member.team_closed)
        }) {
            continue;
        }
        let member = (!agent.exited && !agent.stale)
            .then(|| teams.get(&agent.target.pane_id))
            .flatten()
            .filter(|m| m.window == agent.target.window_id);
        if let Some(member) = member {
            if member.name != "lead" && grouped_keys.contains(&member.key) {
                continue;
            }
            if member.name == "lead" && leads.get(&member.key) == Some(&agent.instance_id) {
                let children = workers.remove(&member.key).unwrap_or_default();
                entries.push((agent, Some((member.key.clone(), children))));
                continue;
            }
        }
        entries.push((agent, None));
    }
    if agent_sort == AgentSort::Prioritized {
        entries.sort_by_key(|(agent, team)| {
            let members = std::iter::once(agent).chain(
                team.iter()
                    .flat_map(|(_, children)| children.iter().map(|(a, _)| a)),
            );
            let rank = members
                .map(|a| {
                    (
                        agent_attention_priority(a),
                        a.state_entered_at_unix_ms,
                        a.attention
                            .as_ref()
                            .and_then(|e| e.attention_seq)
                            .unwrap_or(0),
                    )
                })
                .max()
                .unwrap();
            std::cmp::Reverse(rank)
        });
    }
    let mut rows = vec![];
    let mut previous_session = None;
    for (agent, team) in entries {
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
        if let Some((key, children)) = team {
            let summary = TeamSummary {
                expanded: expanded.contains(&key),
                key,
                workers: children.len(),
                blocked: children
                    .iter()
                    .filter(|(a, _)| a.display_state == DisplayState::Blocked)
                    .count(),
                done: children
                    .iter()
                    .filter(|(a, _)| a.display_state == DisplayState::Done)
                    .count(),
            };
            rows.push(Row::Team(agent.clone(), summary.clone()));
            rows.push(Row::TeamSub(agent.clone(), agent_sort, summary.clone()));
            append_agent_details(&mut rows, &agent, detailed);
            if summary.expanded {
                for (worker, display) in children {
                    rows.push(Row::TeamWorker(worker.clone(), display.clone()));
                    rows.push(Row::TeamWorkerSub(worker, display));
                }
            }
        } else {
            rows.push(Row::Agent(agent.clone()));
            rows.push(Row::AgentSub(agent.clone(), agent_sort));
            append_agent_details(&mut rows, &agent, detailed);
        }
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

fn build_focus_cards(rows: &[Row], focused: &FocusWindows) -> Vec<FocusCard> {
    let mut cards: Vec<_> = focused
        .values()
        .cloned()
        .map(|window| FocusCard {
            window,
            agent: None,
            team: None,
        })
        .collect();
    cards.sort_by(|a, b| {
        (
            !a.window.assistant,
            &a.window.session_name,
            a.window.index,
            &a.window.id,
        )
            .cmp(&(
                !b.window.assistant,
                &b.window.session_name,
                b.window.index,
                &b.window.id,
            ))
    });
    for row in rows {
        let (agent, team) = match row {
            Row::Team(agent, summary) => (agent, Some(summary)),
            Row::Agent(agent) => (agent, None),
            _ => continue,
        };
        if agent.exited {
            continue;
        }
        if let Some(card) = cards
            .iter_mut()
            .find(|card| card.window.id == agent.target.window_id)
            && (card.agent.is_none() || (team.is_some() && card.team.is_none()))
        {
            card.agent = Some(agent.clone());
            card.team = team.cloned();
        }
    }
    cards
}

fn visible_focus_cards(content_height: usize, notice: Option<&str>, count: usize) -> usize {
    // Keep the regular list reachable on small terminals. The complete list is
    // still available through the existing focus picker when it overflows.
    count
        .min(9)
        .min(content_height.saturating_sub(3 + usize::from(notice.is_some())) / 2)
}

fn render_focus_card(
    card: &FocusCard,
    number: usize,
    selected: bool,
    width: u16,
) -> [Line<'static>; 2] {
    let mut title = if let Some(agent) = &card.agent {
        let mut agent = agent.clone();
        if card.window.assistant {
            agent.thread_name =
                Some(crate::ui_language::text("Personal assistant", "个人助理").into());
        }
        render_row(&Row::Agent(agent), selected, width.saturating_sub(2))
    } else {
        icon_title_line(
            glyph(card.window.state),
            if card.window.assistant {
                crate::ui_language::text("Personal assistant", "个人助理")
            } else {
                &card.window.name
            },
            width.saturating_sub(2),
            Style::default().fg(state_color(card.window.state)),
            Style::default().fg(Color::Rgb(235, 235, 245)),
            selected,
        )
    };
    title
        .spans
        .insert(0, Span::styled(format!("{number} "), muted_style()));
    let subtitle = if let Some(agent) = &card.agent {
        if let Some(team) = &card.team {
            render_row(
                &Row::TeamSub(agent.clone(), AgentSort::Prioritized, team.clone()),
                selected,
                width,
            )
        } else {
            render_row(
                &Row::AgentSub(agent.clone(), AgentSort::Prioritized),
                selected,
                width,
            )
        }
    } else {
        aligned_line(
            format!("   {}", card.window.session_name),
            state_name(card.window.state).into(),
            width,
            muted_style(),
            Style::default().fg(state_color(card.window.state)),
            selected,
        )
    };
    [title, subtitle]
}

fn append_agent_details(rows: &mut Vec<Row>, agent: &AgentSnapshot, detailed: bool) {
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
            format!(
                "{} {}",
                crate::ui_language::text("rule", "规则"),
                agent.rule_id.as_deref().unwrap_or("—")
            ),
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
        .rposition(|row| matches!(row, Row::Agent(agent) | Row::Team(agent, _) | Row::TeamWorker(agent, _) if agent.instance_id == instance))
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

fn sort_label(sort: AgentSort) -> &'static str {
    match sort {
        AgentSort::Grouped => crate::ui_language::text("grouped", "分组"),
        AgentSort::Prioritized => crate::ui_language::text("prioritized", "按优先级"),
    }
}

fn render_row(row: &Row, selected: bool, width: u16) -> Line<'static> {
    match row {
        Row::Section(label) => Line::from(Span::styled(
            if *label == "sessions" {
                crate::ui_language::text("sessions", "会话")
            } else {
                *label
            },
            muted_style().add_modifier(Modifier::BOLD),
        )),
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
        Row::Team(agent, summary) => icon_title_line(
            if summary.expanded { "▾" } else { "▸" },
            agent_topic(agent),
            width,
            Style::default().fg(state_color(agent.display_state)),
            Style::default().fg(Color::Rgb(235, 235, 245)),
            selected,
        ),
        Row::TeamSub(agent, sort, summary) => {
            let mut text = if crate::ui_language::name() == "zh" {
                format!("   {} 个工作代理", summary.workers)
            } else {
                format!(
                    "   {} worker{}",
                    summary.workers,
                    if summary.workers == 1 { "" } else { "s" }
                )
            };
            if *sort == AgentSort::Prioritized {
                text.push_str(&format!(" · {}", agent.target.session_name));
            }
            let mut status = agent_status(agent);
            if summary.blocked > 0 {
                status.push_str(&format!(" !{}", summary.blocked));
            }
            if summary.done > 0 {
                status.push_str(&format!(" ✓{}", summary.done));
            }
            status.push_str(&format!(" · {}", agent_kind_name(agent.kind)));
            aligned_line(
                text,
                status,
                width,
                muted_style(),
                Style::default().fg(state_color(agent.display_state)),
                selected,
            )
        }
        Row::TeamWorker(agent, display) => {
            let title = format!("   └ {} {}", glyph(agent.display_state), display.title);
            aligned_line(
                title,
                String::new(),
                width,
                Style::default().fg(Color::Rgb(235, 235, 245)),
                muted_style(),
                selected,
            )
        }
        Row::TeamWorkerSub(agent, display) => aligned_line(
            format!("       {}", display.name),
            agent_status(agent),
            width,
            muted_style(),
            Style::default().fg(state_color(agent.display_state)),
            selected,
        ),
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
                reason_name(conversation.reason_category.as_deref().unwrap_or("blocked"))
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

fn button_style(button: Action, hovered: Option<&Action>) -> Style {
    let style = muted_style();
    if hovered == Some(&button) {
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

/// One layout drives both the Powerline spans and their mouse/touch hit areas.
struct HeaderLayout {
    labels: [String; 4],
    gap: usize,
}

impl HeaderLayout {
    fn new(width: u16, sort: AgentSort, total: usize, shown: usize) -> Self {
        let agents = crate::ui_language::text("Agents", "智能体");
        let menu = crate::ui_language::text("menu", "菜单");
        let icon = match sort {
            AgentSort::Grouped => "",
            AgentSort::Prioritized => "",
        };
        let count = if shown < total {
            format!("{shown}/{total}")
        } else {
            total.to_string()
        };
        let short_sort = match sort {
            AgentSort::Grouped => sort_label(sort),
            AgentSort::Prioritized => crate::ui_language::text("priority", "优先"),
        };
        let candidates = [
            [
                format!(" 󰚩 {agents} "),
                format!("  {count} "),
                format!(" {icon} {} ", sort_label(sort)),
                format!("  {menu} "),
            ],
            [
                format!(" 󰚩 {agents} "),
                format!("  {count} "),
                format!(" {icon} {} ", sort_label(sort)),
                "  ".into(),
            ],
            [
                format!(" 󰚩 {agents} "),
                format!("  {count} "),
                format!(" {icon} {short_sort} "),
                "  ".into(),
            ],
            [
                " 󰚩 ".into(),
                format!("  {total} "),
                format!(" {icon} "),
                "  ".into(),
            ],
            ["󰚩".into(), format!("{total}"), icon.into(), "".into()],
        ];
        let required = |labels: &[String; 4]| {
            labels
                .iter()
                .map(|label| Span::raw(label.as_str()).width())
                .sum::<usize>()
                + 4
        };
        let labels = candidates
            .iter()
            .find(|labels| required(labels) <= usize::from(width))
            .cloned()
            .unwrap_or_else(|| candidates.last().unwrap().clone());
        let gap = usize::from(width).saturating_sub(required(&labels));
        Self { labels, gap }
    }

    fn regions(&self, area: Rect) -> Vec<(Rect, Action)> {
        if area.height == 0 {
            return Vec::new();
        }
        let widths: Vec<_> = self
            .labels
            .iter()
            .map(|label| Span::raw(label.as_str()).width() + 1)
            .collect();
        let starts = [
            0,
            widths[0],
            widths[0] + widths[1] + self.gap,
            widths[0] + widths[1] + self.gap + widths[2],
        ];
        [Action::Agents, Action::Pinned, Action::Sort, Action::Menu]
            .into_iter()
            .enumerate()
            .filter_map(|(index, action)| {
                let start = starts[index].min(usize::from(area.width));
                let end = (starts[index] + widths[index]).min(usize::from(area.width));
                (end > start).then(|| {
                    (
                        Rect::new(area.x + start as u16, area.y, (end - start) as u16, 1),
                        action,
                    )
                })
            })
            .collect()
    }
}

fn render_header(
    width: u16,
    sort: AgentSort,
    total: usize,
    shown: usize,
    hovered: Option<&Action>,
) -> Line<'static> {
    let layout = HeaderLayout::new(width, sort, total, shown);
    // Match the tmux status bar: ANSI green, One Dark surfaces, and its
    // left/right Powerline separators. Hover keeps the separators connected.
    let background = Color::Rgb(40, 44, 52);
    let foreground = Color::Rgb(170, 178, 191);
    let surface = Color::Rgb(62, 68, 82);
    let raised = Color::Rgb(75, 82, 99);
    let highlight = Color::Rgb(92, 99, 112);
    let selected = |action: Action, color: Color| {
        if hovered == Some(&action) {
            highlight
        } else {
            color
        }
    };
    let first = if hovered == Some(&Action::Agents) {
        Color::LightGreen
    } else {
        Color::Green
    };
    let pinned = selected(Action::Pinned, surface);
    let sorting = selected(Action::Sort, surface);
    let menu = selected(Action::Menu, raised);
    let [agents, count, sort, menu_label] = layout.labels;
    Line::from(vec![
        Span::styled(
            agents,
            Style::default()
                .fg(background)
                .bg(first)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled("", Style::default().fg(first).bg(pinned)),
        Span::styled(count, Style::default().fg(foreground).bg(pinned)),
        Span::styled("", Style::default().fg(pinned).bg(background)),
        Span::styled(" ".repeat(layout.gap), Style::default().bg(background)),
        Span::styled("", Style::default().fg(sorting).bg(background)),
        Span::styled(sort, Style::default().fg(foreground).bg(sorting)),
        Span::styled("", Style::default().fg(menu).bg(sorting)),
        Span::styled(menu_label, Style::default().fg(foreground).bg(menu)),
    ])
}

fn render_close(width: u16, hovered: Option<&Action>) -> Line<'static> {
    let close = crate::ui_language::text("× close", "× 关闭");
    let gap = usize::from(width).saturating_sub(Span::raw(close).width());
    Line::from(vec![
        Span::raw(" ".repeat(gap)),
        Span::styled(close, button_style(Action::Close, hovered)),
    ])
}

fn action_regions(
    area: Rect,
    agent_sort: AgentSort,
    is_popup: bool,
    focus_cards: &[FocusCard],
    total_focus: usize,
) -> Vec<(Rect, Action)> {
    let mut actions = vec![];
    if area.height > 0 {
        if is_popup {
            actions.push((
                Rect::new(
                    area.width.saturating_sub(
                        Span::raw(crate::ui_language::text("× close", "× 关闭")).width() as u16,
                    ),
                    area.height - 1,
                    area.width.min(
                        Span::raw(crate::ui_language::text("× close", "× 关闭")).width() as u16,
                    ),
                    1,
                ),
                Action::Close,
            ));
        }
    }
    actions.extend(
        HeaderLayout::new(area.width, agent_sort, total_focus, focus_cards.len()).regions(area),
    );
    for (index, card) in focus_cards.iter().enumerate() {
        let y = 1 + 2 * index as u16;
        if y < area.height {
            actions.push((
                Rect::new(0, y, area.width, 2.min(area.height - y)),
                Action::Focus(card.window.id.clone()),
            ));
        }
    }
    actions
}

fn show_help() -> Result<bool, Box<dyn std::error::Error>> {
    let items = [
        crate::ui_language::text("-j/k ↑/↓ · move", "-j/k ↑/↓ · 移动"),
        crate::ui_language::text("-Enter · open / focus", "-Enter · 打开 / 切换"),
        crate::ui_language::text(
            "-Space / ←/→ / h/l · collapse / expand team",
            "-Space / ←/→ / h/l · 收起 / 展开团队",
        ),
        crate::ui_language::text(
            "-click team arrow · collapse / expand",
            "-点击团队箭头 · 收起 / 展开",
        ),
        crate::ui_language::text("-p · pin / unpin task", "-p · 置顶 / 取消置顶任务"),
        crate::ui_language::text("-1–9 · open a pinned task", "-1–9 · 打开置顶任务"),
        crate::ui_language::text("-m · item menu", "-m · 项目菜单"),
        crate::ui_language::text("-N · projects", "-N · 项目"),
        crate::ui_language::text("-i · temporary Git tool", "-i · 临时 Git 工具"),
        crate::ui_language::text("-W · new workspace", "-W · 新建工作区"),
        crate::ui_language::text(
            "-T · new task in selected session",
            "-T · 在选中会话中新建任务",
        ),
        crate::ui_language::text("-R · rebuild projects", "-R · 重建项目列表"),
        crate::ui_language::text("-s / a · sessions / agents", "-s / a · 会话 / 智能体"),
        crate::ui_language::text("-n · next attention", "-n · 下一条提醒"),
        crate::ui_language::text("-d · details", "-d · 详情"),
        crate::ui_language::text("-r · reload", "-r · 重新加载"),
        crate::ui_language::text(
            "-Escape / Ctrl-C / Ctrl-D · close popup",
            "-Escape / Ctrl-C / Ctrl-D · 关闭弹窗",
        ),
    ]
    .map(|label| (label, "", String::new()));
    show_menu(
        crate::ui_language::text("Shortcuts", "快捷键"),
        None,
        false,
        &items,
    )
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
        crate::ui_language::text("Blank session", "空白会话")
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
        crate::ui_language::text("idle ?", "空闲 ?")
    } else if agent.display_state == DisplayState::Blocked {
        reason_name(agent.reason_category.as_deref().unwrap_or("blocked"))
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
        status.push_str(crate::ui_language::text(" · stale", " · 已过期"));
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
    parts.push(if crate::ui_language::name() == "zh" {
        format!("{} 个智能体", session.agent_count)
    } else {
        format!(
            "{} agent{}",
            session.agent_count,
            if session.agent_count == 1 { "" } else { "s" }
        )
    });
    parts.join(" · ")
}

fn source_health(agent: &AgentSnapshot) -> String {
    let source = match agent.state_source {
        StateSource::Hook => crate::ui_language::text("hook", "钩子"),
        StateSource::Process => crate::ui_language::text("process", "进程"),
        StateSource::Screen => crate::ui_language::text("screen", "屏幕"),
        StateSource::None => crate::ui_language::text("none", "无"),
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
        DisplayState::Blocked => crate::ui_language::text("blocked", "等待中"),
        DisplayState::Done => crate::ui_language::text("done", "已完成"),
        DisplayState::Working => crate::ui_language::text("working", "进行中"),
        DisplayState::Idle => crate::ui_language::text("idle", "空闲"),
        DisplayState::Unknown => crate::ui_language::text("checking", "检测中"),
    }
}

fn reason_name(reason: &str) -> &str {
    match reason {
        "blocked" => crate::ui_language::text("blocked", "等待中"),
        "approval" => crate::ui_language::text("approval", "等待批准"),
        "permission" => crate::ui_language::text("permission", "等待权限"),
        "input" => crate::ui_language::text("input", "等待输入"),
        "question" => crate::ui_language::text("question", "等待回答"),
        "task_error" => crate::ui_language::text("task_error", "任务出错"),
        _ => reason,
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
        Row::AgentSub(agent, _)
        | Row::Team(agent, _)
        | Row::TeamSub(agent, _, _)
        | Row::TeamWorker(agent, _)
        | Row::TeamWorkerSub(agent, _) => Some(format!("agent:{}", agent.instance_id)),
        Row::Conversation(agent, conversation) => Some(format!(
            "conversation:{}:{}",
            agent.instance_id, conversation.id
        )),
        Row::Section(_) | Row::AgentGroup(_) | Row::Detail(_, _) | Row::Spacer => None,
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

fn selected_window(rows: &[Row], selected: usize) -> Option<&str> {
    match rows.get(selected)? {
        Row::Agent(agent)
        | Row::AgentSub(agent, _)
        | Row::Team(agent, _)
        | Row::TeamSub(agent, _, _)
        | Row::TeamWorker(agent, _)
        | Row::TeamWorkerSub(agent, _)
        | Row::Conversation(agent, _) => Some(&agent.target.window_id),
        Row::Session(session) | Row::SessionSub(session) => {
            session.last_active_window_id.as_deref()
        }
        _ => None,
    }
}

fn toggle_focus(
    server: &ServerIdentity,
    window: &str,
    assistant: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    if !safe_target(window, '@') {
        return Err("invalid focus window".into());
    }
    if assistant {
        let _ = crate::tmux::Tmux::new(server.clone()).output_with_timeout(
            &[
                "display-message",
                crate::ui_language::text("Personal assistant stays pinned", "个人助理始终置顶"),
            ],
            Duration::from_millis(500),
        );
        return Ok(());
    }
    let unset = format!("set-option -w -u -t {window} @agent_focus");
    let set = format!("set-option -w -t {window} @agent_focus 1");
    crate::tmux::Tmux::new(server.clone()).output_with_timeout(
        &[
            "if-shell",
            "-F",
            "-t",
            window,
            "#{==:#{@agent_focus},1}",
            &unset,
            &set,
        ],
        Duration::from_millis(500),
    )?;
    Ok(())
}

fn activate_focus(card: &FocusCard) -> Result<bool, Box<dyn std::error::Error>> {
    let pane = card
        .agent
        .as_ref()
        .map(|agent| agent.target.pane_id.as_str())
        .unwrap_or(&card.window.pane);
    if !safe_target(&card.window.session_id, '$')
        || !safe_target(&card.window.id, '@')
        || !safe_target(pane, '%')
    {
        return Ok(false);
    }
    let mut command = Command::new(std::env::current_exe()?);
    command.args([
        "focus",
        "--responsive",
        "--session",
        &card.window.session_id,
        "--window",
        &card.window.id,
        "--pane",
        pane,
    ]);
    if let Some(source) = source_pane() {
        command.args(["--source-pane", &source]);
    }
    Ok(command
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()?
        .success())
}

fn show_pinned_menu(
    cards: &[FocusCard],
    anchor: Option<(u16, u16)>,
) -> Result<bool, Box<dyn std::error::Error>> {
    let executable = std::env::current_exe()?;
    let source = source_pane().ok_or("sidebar source pane unavailable")?;
    let mut owned = Vec::new();
    for card in cards {
        let pane = card
            .agent
            .as_ref()
            .map(|agent| agent.target.pane_id.as_str())
            .unwrap_or(&card.window.pane);
        if !safe_target(&card.window.session_id, '$')
            || !safe_target(&card.window.id, '@')
            || !safe_target(pane, '%')
        {
            continue;
        }
        let title = if card.window.assistant {
            crate::ui_language::text("Personal assistant", "个人助理")
        } else {
            card.agent
                .as_ref()
                .and_then(|agent| agent.thread_name.as_deref())
                .filter(|title| !title.is_empty())
                .unwrap_or(&card.window.name)
        };
        // tmux expands # formats in menu labels; do not interpret task titles
        // as formats, and keep arbitrary control characters out of the menu.
        let title: String = title.chars().filter(|ch| !ch.is_control()).collect();
        let session: String = card
            .window
            .session_name
            .chars()
            .filter(|ch| !ch.is_control())
            .collect();
        let label = format!(
            " {} · {}",
            title.replace('#', "##"),
            session.replace('#', "##")
        );
        let key = if owned.len() < 9 {
            (owned.len() + 1).to_string()
        } else {
            String::new()
        };
        let focus = format!(
            "{} focus --responsive --session {} --window {} --pane {} --source-pane {}",
            shell_quote(&executable.to_string_lossy()),
            shell_quote(&card.window.session_id),
            card.window.id,
            pane,
            source,
        );
        owned.push((label, key, format!("run-shell {}", shell_quote(&focus))));
    }
    if owned.is_empty() {
        owned.push((
            crate::ui_language::text("-No pinned agents", "-没有置顶的智能体").into(),
            String::new(),
            String::new(),
        ));
    }
    let items: Vec<_> = owned
        .iter()
        .map(|(label, key, command)| (label.as_str(), key.as_str(), command.clone()))
        .collect();
    show_menu(
        crate::ui_language::text("Pinned agents", "置顶的智能体"),
        anchor,
        false,
        &items,
    )
}

fn show_focus_menu(
    card: &FocusCard,
    anchor: Option<(u16, u16)>,
) -> Result<bool, Box<dyn std::error::Error>> {
    if !safe_target(&card.window.id, '@') {
        return Ok(false);
    }
    if card.window.assistant {
        return show_menu(
            crate::ui_language::text("Personal assistant", "个人助理"),
            anchor,
            false,
            &[(
                crate::ui_language::text("-Always pinned", "-始终置顶"),
                "",
                String::new(),
            )],
        );
    }
    show_menu(
        crate::ui_language::text("Pinned", "置顶"),
        anchor,
        false,
        &[(
            crate::ui_language::text("Unpin task", "取消置顶"),
            "p",
            format!("set-option -w -u -t {} @agent_focus", card.window.id),
        )],
    )
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
        Some(
            Row::Agent(agent)
            | Row::AgentSub(agent, _)
            | Row::Team(agent, _)
            | Row::TeamSub(agent, _, _)
            | Row::TeamWorker(agent, _)
            | Row::TeamWorkerSub(agent, _),
        ) if agent.exited => {
            if let Some(event) = &agent.attention {
                let server = ServerIdentity::discover()?;
                let paths = Paths::discover()?;
                acknowledge_attention(&paths.socket_for_server(&server.key), &event.id)?;
            }
        }
        Some(
            Row::Agent(agent)
            | Row::AgentSub(agent, _)
            | Row::Team(agent, _)
            | Row::TeamSub(agent, _, _)
            | Row::TeamWorker(agent, _)
            | Row::TeamWorkerSub(agent, _),
        ) => {
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
    focused: &FocusWindows,
) -> Result<bool, Box<dyn std::error::Error>> {
    match rows.get(selected) {
        Some(Row::Session(session) | Row::SessionSub(session))
            if safe_target(&session.session_id, '$') =>
        {
            show_menu(
                crate::ui_language::text("session", "会话"),
                anchor,
                false,
                &[
                    (
                        crate::ui_language::text("Switch", "切换"),
                        "s",
                        format!(
                            "switch-client -c {} -t {}",
                            shell_quote(&current_client()?),
                            session.session_id
                        ),
                    ),
                    (
                        crate::ui_language::text("New task", "新建任务"),
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
                        crate::ui_language::text("Rename", "重命名"),
                        "r",
                        format!(
                            "command-prompt -t {} -p '{}' \"rename-session -t {} '%%'\"",
                            shell_quote(&current_client()?),
                            crate::ui_language::text("Rename session:", "重命名会话："),
                            session.session_id
                        ),
                    ),
                    (
                        crate::ui_language::text("Close", "关闭"),
                        "x",
                        format!(
                            "confirm-before -t {} -p '{}' 'kill-session -t {}'",
                            shell_quote(&current_client()?),
                            crate::ui_language::text("Close session?", "关闭会话？"),
                            session.session_id
                        ),
                    ),
                ],
            )
        }
        Some(
            Row::Agent(agent)
            | Row::AgentSub(agent, _)
            | Row::Team(agent, _)
            | Row::TeamSub(agent, _, _)
            | Row::TeamWorker(agent, _)
            | Row::TeamWorkerSub(agent, _),
        ) if !agent.exited && safe_target(&agent.target.pane_id, '%') => {
            let executable = std::env::current_exe()?;
            let source = source_pane().ok_or("sidebar source pane unavailable")?;
            let focus = format!(
                "{} focus --responsive --session {} --window {} --pane {} --source-pane {}",
                shell_quote(&executable.to_string_lossy()),
                shell_quote(&agent.target.session_id),
                agent.target.window_id,
                agent.target.pane_id,
                source,
            );
            if !safe_target(&agent.target.window_id, '@') {
                return Ok(false);
            }
            let (focus_label, focus_key, focus_command) = if focused
                .get(&agent.target.window_id)
                .is_some_and(|item| item.assistant)
            {
                (
                    crate::ui_language::text(
                        "-Personal assistant stays pinned",
                        "-个人助理始终置顶",
                    ),
                    "",
                    String::new(),
                )
            } else if focused.contains_key(&agent.target.window_id) {
                (
                    crate::ui_language::text("Unpin task", "取消置顶"),
                    "p",
                    format!(
                        "set-option -w -u -t {} @agent_focus",
                        agent.target.window_id
                    ),
                )
            } else {
                (
                    crate::ui_language::text("Pin task", "置顶任务"),
                    "p",
                    format!("set-option -w -t {} @agent_focus 1", agent.target.window_id),
                )
            };
            show_menu(
                crate::ui_language::text("agent", "智能体"),
                anchor,
                false,
                &[
                    (
                        crate::ui_language::text("Focus", "切换到任务"),
                        "f",
                        format!("run-shell {}", shell_quote(&focus)),
                    ),
                    (focus_label, focus_key, focus_command),
                    (
                        crate::ui_language::text("Rename pane", "重命名窗格"),
                        "r",
                        format!(
                            "command-prompt -p '{}' \"select-pane -Z -t {} -T '%%'\"",
                            crate::ui_language::text("Rename pane:", "重命名窗格："),
                            agent.target.pane_id
                        ),
                    ),
                    (
                        crate::ui_language::text("Split right", "向右分屏"),
                        "h",
                        format!("split-window -h -t {}", agent.target.pane_id),
                    ),
                    (
                        crate::ui_language::text("Split down", "向下分屏"),
                        "v",
                        format!("split-window -v -t {}", agent.target.pane_id),
                    ),
                    (
                        crate::ui_language::text("Zoom", "缩放窗格"),
                        "z",
                        format!("resize-pane -Z -t {}", agent.target.pane_id),
                    ),
                    (
                        crate::ui_language::text("Close pane", "关闭窗格"),
                        "x",
                        format!(
                            "confirm-before -t {} -p '{}' 'kill-pane -t {}'",
                            shell_quote(&current_client()?),
                            crate::ui_language::text("Close pane?", "关闭窗格？"),
                            agent.target.pane_id
                        ),
                    ),
                ],
            )
        }
        _ => Ok(false),
    }
}

fn show_global_menu(anchor: Option<(u16, u16)>) -> Result<bool, Box<dyn std::error::Error>> {
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
            crate::ui_language::text("New", "新建"),
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
            crate::ui_language::text("Temporary Git tool", "临时 Git 工具"),
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
            crate::ui_language::text("New workspace", "新建工作区"),
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
            crate::ui_language::text("New task", "新建任务"),
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
            crate::ui_language::text("Rebuild projects", "重建项目列表"),
            "R",
            "run-shell -b tmux-agent-workbench projects rebuild".into(),
        ),
        (
            crate::ui_language::text("Details", "详情"),
            "d",
            format!("send-keys -t {sidebar_pane} d"),
        ),
        (
            crate::ui_language::text("Sessions", "会话"),
            "s",
            navigation("session")?,
        ),
        (
            crate::ui_language::text("Agents", "智能体"),
            "a",
            navigation("agent")?,
        ),
        (
            crate::ui_language::text("Next", "下一条提醒"),
            "n",
            format!("run-shell \"{} attention next\"", executable),
        ),
        (
            crate::ui_language::text("Reload", "重新加载"),
            "r",
            format!("run-shell \"{} reload\"", executable),
        ),
    ];
    if popup_mode() {
        // Once this popup closes there is no sidebar pane to receive these
        // synthetic keys. The direct keyboard shortcuts remain available in
        // the TUI; omit only the menu entries whose target would be stale.
        items.retain(|(label, _, _)| *label != crate::ui_language::text("Details", "详情"));
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
        Some(
            Row::Agent(agent)
            | Row::AgentSub(agent, _)
            | Row::Team(agent, _)
            | Row::TeamSub(agent, _, _)
            | Row::TeamWorker(agent, _)
            | Row::TeamWorkerSub(agent, _),
        )
        | Some(Row::Conversation(agent, _)) => Some(agent.target.pane_id.as_str()),
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
    items: &[(&str, &str, String)],
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
        let viewport = SidebarViewport::new(10, rows.len(), Some("stale · reconnecting"), 0);
        assert_eq!(viewport.row_at(0, 0, rows.len()), None);
        assert_eq!(
            rows[viewport.row_at(4, 0, rows.len()).unwrap()],
            "A subtitle"
        );
        assert_eq!(rows[viewport.row_at(5, 0, rows.len()).unwrap()], "agent B");
        assert_eq!(viewport.row_at(9, 0, rows.len()), None);
    }

    #[test]
    fn scrolled_viewport_excludes_banner_overflow_and_footer() {
        // Eight lines include the header, a banner, divider, rows and overflow;
        // the popup footer follows and must not activate any hidden row.
        let viewport = SidebarViewport::new(8, 20, Some("stale · reconnecting"), 0);
        assert_eq!(viewport.row_height, 4);
        assert_eq!(viewport.row_at(0, 7, 20), None);
        assert_eq!(viewport.row_at(1, 7, 20), None);
        assert_eq!(viewport.row_at(2, 7, 20), None);
        assert_eq!(viewport.row_at(3, 7, 20), Some(7));
        assert_eq!(viewport.row_at(4, 7, 20), Some(8));
        assert_eq!(viewport.row_at(5, 7, 20), Some(9));
        assert_eq!(viewport.row_at(6, 7, 20), Some(10));
        for row in [7, 8, 9] {
            assert_eq!(viewport.row_at(row, 7, 20), None);
        }
    }

    #[test]
    fn banner_uses_space_even_when_rows_previously_fitted() {
        let normal = SidebarViewport::new(8, 6, None, 0);
        assert_eq!(normal.row_at(7, 0, 6), Some(5));
        let stale = SidebarViewport::new(8, 6, Some("stale · reconnecting"), 0);
        assert_eq!(stale.row_height, 4);
        assert_eq!(stale.row_at(7, 0, 6), None);
        let overflow = SidebarViewport::new(6, 10, None, 0);
        assert_eq!(overflow.row_at(2, 0, 10), Some(0));
        assert_eq!(overflow.row_at(5, 0, 10), None);
    }

    #[test]
    fn empty_disconnected_and_tiny_viewports_have_no_agent_targets() {
        for height in [0, 1] {
            let viewport = SidebarViewport::new(height, 10, Some("stale · reconnecting"), 0);
            for row in 0..3 {
                assert_eq!(viewport.row_at(row, 0, 10), None);
            }
        }
        assert_eq!(SidebarViewport::new(10, 0, None, 0).row_at(0, 0, 0), None);
        let disconnected = SidebarViewport::new(10, 4, Some("disconnected"), 0);
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

    fn team_fixture() -> (Snapshot, TeamIndex) {
        let mut snapshot: Snapshot =
            serde_json::from_str(include_str!("../tests/golden/snapshot-v1.json")).unwrap();
        let template = snapshot.agents[0].clone();
        let agent = |name: &str, pane: &str, index: u32, state: DisplayState| {
            let mut agent = template.clone();
            agent.instance_id = name.into();
            agent.target.pane_id = pane.into();
            agent.target.window_id = "@9".into();
            agent.target.pane_index = index;
            agent.display_state = state;
            agent.exited = false;
            agent.stale = false;
            agent
        };
        // A child can appear before or after its lead in tmux order.
        snapshot.agents = vec![
            agent("worker-a", "%1", 0, DisplayState::Blocked),
            agent("lead", "%2", 1, DisplayState::Working),
            agent("worker-b", "%3", 2, DisplayState::Done),
            agent("other", "%4", 3, DisplayState::Working),
        ];
        let mut teams = TeamIndex::new();
        for (pane, name) in [("%1", "worker_a"), ("%2", "lead"), ("%3", "worker_b")] {
            teams.insert(
                pane.into(),
                TeamMember {
                    key: "@9:test".into(),
                    window: "@9".into(),
                    name: name.into(),
                    brief: None,
                    assignment: None,
                    pane_missing: false,
                    team_closed: false,
                },
            );
        }
        (snapshot, teams)
    }

    #[test]
    fn focus_cards_keep_teams_together_and_show_unobserved_windows() {
        let (snapshot, teams) = team_fixture();
        let agent = &snapshot.agents[1];
        let marked = FocusWindow {
            id: "@9".into(),
            session_id: "$1".into(),
            session_name: "zzz direction".into(),
            name: "team task".into(),
            index: 1,
            pane: "%2".into(),
            state: DisplayState::Working,
            assistant: true,
        };
        let shell = FocusWindow {
            id: "@10".into(),
            session_id: "$2".into(),
            session_name: "other direction".into(),
            name: "shell task".into(),
            index: 2,
            pane: "%10".into(),
            state: DisplayState::Idle,
            assistant: false,
        };
        let focused = FocusWindows::from([(marked.id.clone(), marked), (shell.id.clone(), shell)]);
        for sort in [AgentSort::Grouped, AgentSort::Prioritized] {
            let rows = build_team_rows(&snapshot, false, sort, &teams, &HashSet::new());
            let cards = build_focus_cards(&rows, &focused);
            assert_eq!(cards.len(), 2);
            assert_eq!(
                cards[0].agent.as_ref().unwrap().instance_id,
                agent.instance_id
            );
            assert_eq!(cards[0].team.as_ref().unwrap().workers, 2);
            crate::ui_language::set_language(Some("en"));
            assert!(
                render_focus_card(&cards[0], 1, false, 50)[0]
                    .to_string()
                    .contains("Personal assistant")
            );
            crate::ui_language::set_language(None);
            assert!(cards[1].agent.is_none());
            let [title, subtitle] = render_focus_card(&cards[1], 2, false, 50);
            assert!(title.to_string().contains("shell task"));
            assert!(subtitle.to_string().contains("other direction"));
        }
    }

    #[test]
    fn focus_actions_stay_at_the_top_when_the_ordinary_list_scrolls() {
        let (snapshot, teams) = team_fixture();
        let agent = snapshot.agents[1].clone();
        let window = FocusWindow {
            id: "@9".into(),
            session_id: "$1".into(),
            session_name: "direction".into(),
            name: "team task".into(),
            index: 1,
            pane: "%2".into(),
            state: DisplayState::Working,
            assistant: false,
        };
        let focused = FocusWindows::from([(window.id.clone(), window)]);
        let rows = build_team_rows(
            &snapshot,
            false,
            AgentSort::Grouped,
            &teams,
            &HashSet::new(),
        );
        let cards = build_focus_cards(&rows, &focused);
        let actions = action_regions(
            Rect::new(0, 0, 36, 16),
            AgentSort::Grouped,
            false,
            &cards,
            cards.len(),
        );
        let viewport = SidebarViewport::new(16, 30, None, cards.len());
        let mut list = NavigationList::<Action>::default();
        list.replace_rows((0..30).map(|i| Some(i.to_string())).collect());
        list.set_viewport(Rect::new(
            0,
            viewport.first_row as u16,
            36,
            viewport.row_height as u16,
        ));
        list.scroll_by(12);
        for row in [1, 2] {
            assert_eq!(
                list.hit(1, row, &actions),
                Some(Hit::Action(Action::Focus("@9".into())))
            );
        }
        assert_eq!(list.hit(1, 0, &actions), Some(Hit::Action(Action::Agents)));
        assert_eq!(list.hit(35, 0, &actions), Some(Hit::Action(Action::Menu)));
        assert_eq!(selected_window(&[Row::Agent(agent)], 0), Some("@9"));
        assert_eq!(visible_focus_cards(12, None, 3), 3);
        assert!(visible_focus_cards(6, None, 3) < 3);
    }

    #[test]
    fn pinned_header_and_sort_use_one_language_and_match_hit_regions() {
        for (language, agents, sort) in [
            ("en", "Agents", "prioritized"),
            ("zh-CN", "智能体", "按优先级"),
            ("ja-JP", "Agents", "prioritized"),
        ] {
            crate::ui_language::set_language(Some(language));
            let heading = render_header(48, AgentSort::Prioritized, 2, 2, None).to_string();
            assert!(
                heading.contains(agents) && heading.contains(" 2") && heading.contains(sort),
                "{heading}"
            );
            assert!(heading.contains('') && heading.contains(''), "{heading}");
            for width in [18, 26, 36, 48] {
                let actions = action_regions(
                    Rect::new(0, 0, width, 10),
                    AgentSort::Prioritized,
                    false,
                    &[],
                    2,
                );
                let mut list = NavigationList::<Action>::default();
                list.replace_rows(vec![]);
                for expected in [Action::Agents, Action::Pinned, Action::Sort, Action::Menu] {
                    let (area, _) = actions
                        .iter()
                        .find(|(_, action)| *action == expected)
                        .unwrap();
                    assert_eq!(
                        list.hit(area.x + area.width / 2, 0, &actions),
                        Some(Hit::Action(expected))
                    );
                }
                assert_eq!(
                    list.hit(width - 1, 0, &actions),
                    Some(Hit::Action(Action::Menu))
                );
            }
        }
        crate::ui_language::set_language(None);
    }

    #[test]
    fn saved_assistant_identity_matches_only_one_live_window() {
        let temp = tempfile::tempdir().unwrap();
        let paths = test_paths(temp.path());
        std::fs::create_dir_all(&paths.state_dir).unwrap();
        let path = paths.state_dir.join("personal-assistant.json");
        std::fs::write(
            &path,
            r#"{"schema_version":1,"kind":"codex","thread_id":"test-thread"}"#,
        )
        .unwrap();
        let mut snapshot: Snapshot =
            serde_json::from_str(include_str!("../tests/golden/snapshot-v1.json")).unwrap();
        snapshot.agents[0].hook_session_id = Some("test-thread".into());
        assert_eq!(
            assistant_window(&paths, &snapshot).as_deref(),
            Some(snapshot.agents[0].target.window_id.as_str())
        );
        let mut duplicate = snapshot.agents[0].clone();
        duplicate.target.window_id = "@99".into();
        snapshot.agents.push(duplicate);
        assert!(assistant_window(&paths, &snapshot).is_none());
        snapshot.agents[1].exited = true;
        assert!(assistant_window(&paths, &snapshot).is_some());
        std::fs::write(path, "{}").unwrap();
        assert!(assistant_window(&paths, &snapshot).is_none());
    }

    #[test]
    fn stopped_workers_disappear_without_hiding_done_or_independent_agents() {
        let (mut snapshot, mut teams) = team_fixture();
        snapshot.agents[0].exited = true;
        snapshot.agents[3].exited = true;
        let rows = build_team_rows(
            &snapshot,
            false,
            AgentSort::Grouped,
            &teams,
            &HashSet::from(["@9:test".into()]),
        );
        assert!(!rows.iter().any(|row| matches!(row, Row::Agent(a) | Row::TeamWorker(a, _) if a.instance_id == "worker-a")));
        assert!(
            rows.iter()
                .any(|row| matches!(row, Row::TeamWorker(a, _) if a.instance_id == "worker-b"))
        );
        assert!(
            rows.iter()
                .any(|row| matches!(row, Row::Agent(a) if a.instance_id == "other"))
        );

        // The pane can disappear before the next agent snapshot marks exit.
        snapshot.agents[0].exited = false;
        teams.get_mut("%1").unwrap().pane_missing = true;
        let rows = build_team_rows(
            &snapshot,
            false,
            AgentSort::Grouped,
            &teams,
            &HashSet::new(),
        );
        let summary = rows
            .iter()
            .find_map(|row| match row {
                Row::Team(_, summary) => Some(summary),
                _ => None,
            })
            .unwrap();
        assert_eq!(summary.workers, 1);
        assert!(!rows.iter().any(|row| matches!(row, Row::Agent(a) | Row::TeamWorker(a, _) if a.instance_id == "worker-a")));
    }

    #[test]
    fn verified_lead_remains_a_team_with_no_live_workers() {
        let (mut snapshot, mut teams) = team_fixture();
        snapshot
            .agents
            .retain(|agent| !agent.instance_id.starts_with("worker-"));
        teams.retain(|_, member| member.name == "lead");
        for sort in [AgentSort::Grouped, AgentSort::Prioritized] {
            let rows = build_team_rows(&snapshot, false, sort, &teams, &HashSet::new());
            let summary = rows
                .iter()
                .find_map(|row| match row {
                    Row::Team(agent, summary) if agent.instance_id == "lead" => Some(summary),
                    _ => None,
                })
                .unwrap();
            assert_eq!(summary.workers, 0);
            assert!(!rows.iter().any(|row| matches!(row, Row::TeamWorker(..))));
        }
    }

    #[test]
    fn closed_team_workers_stay_hidden_after_lead_exits() {
        let (mut snapshot, mut teams) = team_fixture();
        snapshot.agents[1].exited = true;
        snapshot.agents[0].exited = true;
        teams.get_mut("%1").unwrap().pane_missing = true;
        teams.get_mut("%3").unwrap().team_closed = true;
        // A live agent in another window must not be claimed by a stale team.
        snapshot.agents[3].target.pane_id = "%5".into();
        let mut unrelated = teams["%1"].clone();
        unrelated.window = "@old".into();
        teams.insert("%5".into(), unrelated);
        for sort in [AgentSort::Grouped, AgentSort::Prioritized] {
            let rows = build_team_rows(&snapshot, false, sort, &teams, &HashSet::new());
            assert!(!rows.iter().any(|row| matches!(row, Row::Agent(a) | Row::TeamWorker(a, _) if a.instance_id.starts_with("worker-"))));
            assert!(
                rows.iter()
                    .any(|row| matches!(row, Row::Agent(a) if a.instance_id == "other"))
            );
        }
    }

    #[test]
    fn team_collapses_by_default_and_expands_once_in_tmux_order() {
        let (snapshot, teams) = team_fixture();
        for sort in [AgentSort::Grouped, AgentSort::Prioritized] {
            let rows = build_team_rows(&snapshot, false, sort, &teams, &HashSet::new());
            assert_eq!(
                rows.iter().filter(|r| matches!(r, Row::Team(..))).count(),
                1
            );
            assert!(!rows.iter().any(|r| matches!(r, Row::TeamWorker(..))));
            assert_eq!(
                rows.iter().filter(|r| matches!(r, Row::Agent(..))).count(),
                1
            );
            let summary = rows
                .iter()
                .find_map(|r| match r {
                    Row::Team(_, s) => Some(s),
                    _ => None,
                })
                .unwrap();
            assert_eq!((summary.workers, summary.blocked, summary.done), (2, 1, 1));
            let subtitle = rows.iter().find(|r| matches!(r, Row::TeamSub(..))).unwrap();
            let line = render_row(subtitle, false, 24);
            let text: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
            assert!(text.contains("!1") && text.contains("✓1"), "{text}");
            let expanded = HashSet::from(["@9:test".into()]);
            let rows = build_team_rows(&snapshot, false, sort, &teams, &expanded);
            let lead = rows
                .iter()
                .position(|r| matches!(r, Row::Team(..)))
                .unwrap();
            let members: Vec<_> = rows
                .iter()
                .filter_map(|r| match r {
                    Row::TeamWorker(a, display) => {
                        Some((a.instance_id.as_str(), display.name.as_str()))
                    }
                    _ => None,
                })
                .collect();
            assert_eq!(
                members,
                [("worker-a", "worker_a"), ("worker-b", "worker_b")]
            );
            assert!(matches!(&rows[lead+2], Row::TeamWorker(a, _) if a.instance_id == "worker-a"));
            assert!(matches!(&rows[lead+4], Row::TeamWorker(a, _) if a.instance_id == "worker-b"));
            if sort == AgentSort::Prioritized {
                assert!(
                    lead < rows
                        .iter()
                        .position(|r| matches!(r, Row::Agent(..)))
                        .unwrap()
                );
            }
            assert_eq!(
                selection_key(&rows[lead + 2]).as_deref(),
                Some("agent:worker-a")
            );
        }
        assert_eq!(key_event_direction(KeyCode::Char(' '), false), Some(true));
        assert_eq!(key_event_direction(KeyCode::Left, true), Some(false));
        assert_eq!(key_event_direction(KeyCode::Right, false), Some(true));
        assert_eq!(key_event_direction(KeyCode::Char('h'), true), Some(false));
        assert_eq!(key_event_direction(KeyCode::Char('l'), false), Some(true));
    }

    #[test]
    fn team_status_color_follows_lead_with_worker_counts() {
        let (mut snapshot, teams) = team_fixture();
        for lead_state in [
            DisplayState::Blocked,
            DisplayState::Done,
            DisplayState::Working,
            DisplayState::Idle,
            DisplayState::Unknown,
        ] {
            snapshot.agents[1].display_state = lead_state;
            for worker_states in [
                [DisplayState::Working, DisplayState::Working],
                [DisplayState::Done, DisplayState::Working],
                [DisplayState::Blocked, DisplayState::Done],
            ] {
                snapshot.agents[0].display_state = worker_states[0];
                snapshot.agents[2].display_state = worker_states[1];
                for sort in [AgentSort::Grouped, AgentSort::Prioritized] {
                    let rows = build_team_rows(&snapshot, false, sort, &teams, &HashSet::new());
                    let subtitle = rows
                        .iter()
                        .find(|row| matches!(row, Row::TeamSub(..)))
                        .unwrap();
                    let line = render_row(subtitle, false, 80);
                    assert_eq!(line.spans[2].style.fg, Some(state_color(lead_state)));
                    let text: String = line
                        .spans
                        .iter()
                        .map(|span| span.content.as_ref())
                        .collect();
                    assert_eq!(
                        text.contains("!1"),
                        worker_states.contains(&DisplayState::Blocked)
                    );
                    assert_eq!(
                        text.contains("✓1"),
                        worker_states.contains(&DisplayState::Done)
                    );
                }
            }
        }
    }

    #[test]
    fn team_subtitle_shows_the_lead_agent_kind() {
        let (mut snapshot, teams) = team_fixture();
        for kind in [
            crate::model::AgentKind::Codex,
            crate::model::AgentKind::Claude,
            crate::model::AgentKind::Trae,
            crate::model::AgentKind::Opencode,
        ] {
            snapshot.agents[1].kind = kind;
            for sort in [AgentSort::Grouped, AgentSort::Prioritized] {
                let rows = build_team_rows(&snapshot, false, sort, &teams, &HashSet::new());
                let subtitle = rows
                    .iter()
                    .find(|row| matches!(row, Row::TeamSub(..)))
                    .unwrap();
                let line = render_row(subtitle, false, 36);
                let text: String = line
                    .spans
                    .iter()
                    .map(|span| span.content.as_ref())
                    .collect();
                assert!(text.contains(agent_kind_name(kind)), "{text}");
                assert!(text.contains("!1") && text.contains("✓1"), "{text}");
            }
        }
    }

    #[test]
    fn worker_titles_follow_received_explicit_assignments_then_thread_and_brief() {
        let (snapshot, mut teams) = team_fixture();
        let mut agent = snapshot.agents[0].clone();
        agent.thread_name = Some("核对 CCM worker 状态并建议命名".into());
        let member = teams.get_mut("%1").unwrap();
        member.brief = Some("目标：核对注册信息；禁止修改资源".into());
        let state = serde_json::json!({"messages": [
            {"id": 1, "from": "lead", "to": "worker_a", "type": "send", "text": "新任务：检查 Remote 发布情况。报告结论。"},
            {"id": 2, "from": "lead", "to": "worker_a", "type": "send", "text": "谢谢，请继续核查。"},
            {"id": 3, "from": "lead", "to": "worker_a", "type": "send", "text": "新任务：待接收的任务。"},
            {"id": 4, "from": "worker_b", "to": "worker_a", "type": "send", "text": "新任务：其他成员的消息。"}
        ]});
        member.assignment = received_assignment(&state, "worker_a", 2);
        assert_eq!(worker_display(&agent, member).title, "检查 Remote 发布情况");
        member.assignment = received_assignment(&state, "worker_a", 0);
        assert_eq!(
            worker_display(&agent, member).title,
            "核对 CCM worker 状态并建议命名"
        );
        agent.thread_name = None;
        assert_eq!(worker_display(&agent, member).title, "核对注册信息");
        member.brief = None;
        assert_eq!(worker_display(&agent, member).title, "worker_a");
        assert_eq!(
            task_title("目标：安全\u{1b}标题；其他内容").as_deref(),
            Some("安全标题")
        );
    }

    #[test]
    fn uncertain_or_stale_team_members_remain_independent() {
        let (mut snapshot, mut teams) = team_fixture();
        teams.remove("%2");
        let rows = build_team_rows(
            &snapshot,
            false,
            AgentSort::Grouped,
            &teams,
            &HashSet::new(),
        );
        assert_eq!(
            rows.iter().filter(|r| matches!(r, Row::Agent(..))).count(),
            4
        );
        let (fresh_snapshot, fresh_teams) = team_fixture();
        snapshot = fresh_snapshot;
        teams = fresh_teams;
        snapshot.agents[0].target.window_id = "@other".into();
        snapshot.agents[2].stale = true;
        let rows = build_team_rows(
            &snapshot,
            false,
            AgentSort::Grouped,
            &teams,
            &HashSet::new(),
        );
        let summary = rows
            .iter()
            .find_map(|r| match r {
                Row::Team(_, summary) => Some(summary),
                _ => None,
            })
            .unwrap();
        assert_eq!(summary.workers, 0);
        assert!(!rows.iter().any(|r| matches!(r, Row::TeamWorker(..))));
        assert_eq!(
            rows.iter().filter(|r| matches!(r, Row::Agent(..))).count(),
            3
        );
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
        assert!(matches!(rows.get(sessions - 1), Some(Row::Spacer)));
        assert!(!matches!(rows.get(sessions - 2), Some(Row::Spacer)));
    }

    #[test]
    fn top_actions_span_left_and_right_without_help() {
        let list = NavigationList::<Action>::default();
        let actions = action_regions(Rect::new(0, 0, 40, 20), AgentSort::Grouped, true, &[], 0);
        assert_eq!(list.hit(0, 0, &actions), Some(Hit::Action(Action::Agents)));
        assert_eq!(list.hit(20, 1, &actions), None);
        assert_eq!(list.hit(39, 0, &actions), Some(Hit::Action(Action::Menu)));
        assert_eq!(list.hit(33, 19, &actions), Some(Hit::Action(Action::Close)));
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
                | Row::AgentGroup(_)
                | Row::SessionSub(_)
                | Row::Agent(_)
                | Row::AgentSub(_, _)
                | Row::Team(_, _)
                | Row::TeamSub(_, _, _)
                | Row::TeamWorker(_, _)
                | Row::TeamWorkerSub(_, _)
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
    fn unnamed_agent_cards_show_a_blank_session() {
        let snapshot: Snapshot =
            serde_json::from_str(include_str!("../tests/golden/snapshot-v1.json")).unwrap();
        let mut agent = snapshot.agents[0].clone();
        agent.target.session_name = "Terminal tools".into();
        agent.label = "Codex".into();
        for hook_session_id in [None, Some("thread-id".to_owned())] {
            agent.hook_session_id = hook_session_id;
            for window in ["", " ", "codex", "Claude", "agent", "zsh", "shell"] {
                agent.target.window_name = window.into();
                let first = render_row(&Row::Agent(agent.clone()), false, 40);
                let text = first
                    .spans
                    .iter()
                    .map(|span| span.content.as_ref())
                    .collect::<String>();
                assert!(text.contains("Blank session"), "{window:?}: {text:?}");
                assert!(!text.contains(&agent.target.session_name));
            }
        }

        agent.target.window_name = "Custom window".into();
        assert_eq!(agent_topic(&agent), "Custom window");
        agent.label = "Legacy thread".into();
        assert_eq!(agent_topic(&agent), "Legacy thread");
        agent.thread_name = Some("Named thread".into());
        assert_eq!(agent_topic(&agent), "Named thread");
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
        assert_eq!(agent_topic(&agent), "Blank session");
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
        for line in [section] {
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
