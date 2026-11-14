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

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
enum SidebarView {
    Agents,
    Sessions,
}

impl SidebarView {
    fn toggle(self) -> Self {
        match self {
            Self::Agents => Self::Sessions,
            Self::Sessions => Self::Agents,
        }
    }
}

const FILTERED_AGENT_STATES: [DisplayState; 4] = [
    DisplayState::Blocked,
    DisplayState::Done,
    DisplayState::Working,
    DisplayState::Idle,
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct AgentStateFilter {
    visible: [bool; 4],
}

impl Default for AgentStateFilter {
    fn default() -> Self {
        Self { visible: [true; 4] }
    }
}

impl AgentStateFilter {
    fn is_visible(self, state: DisplayState) -> bool {
        FILTERED_AGENT_STATES
            .iter()
            .position(|candidate| *candidate == state)
            .map(|index| self.visible[index])
            .unwrap_or(true)
    }

    fn toggle(&mut self, state: DisplayState) {
        if let Some(index) = FILTERED_AGENT_STATES
            .iter()
            .position(|candidate| *candidate == state)
        {
            self.visible[index] = !self.visible[index];
        }
    }

    fn enabled(self, state: DisplayState) -> bool {
        FILTERED_AGENT_STATES
            .iter()
            .position(|candidate| *candidate == state)
            .is_some_and(|index| self.visible[index])
    }
}

fn agent_state_counts(snapshot: Option<&Snapshot>) -> [usize; 4] {
    let mut counts = [0; 4];
    if let Some(snapshot) = snapshot {
        for agent in &snapshot.agents {
            if let Some(index) = FILTERED_AGENT_STATES
                .iter()
                .position(|state| *state == agent.display_state)
            {
                counts[index] += 1;
            }
        }
    }
    counts
}

#[derive(Debug, Clone)]
enum PinnedCard {
    Agent(FocusCard),
    Session(SessionSnapshot),
}

impl PinnedCard {
    fn action(&self) -> Action {
        match self {
            Self::Agent(card) => Action::Focus(card.window.id.clone()),
            Self::Session(session) => Action::FocusSession(session.session_id.clone()),
        }
    }
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
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
type FocusSessions = HashSet<String>;

/// One daemon-computed view shared by every sidebar on the same server.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub(crate) struct SharedSidebarData {
    teams: TeamIndex,
    focused: FocusWindows,
    sessions: FocusSessions,
    live_panes: HashSet<String>,
    branches: HashMap<String, String>,
    selection_targets: HashMap<String, String>,
    view: Option<SidebarView>,
    sort: Option<AgentSort>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct SidebarPalette {
    background: Color,
    foreground: Color,
    selection: Color,
    muted: Color,
    raised: Color,
    hover: Color,
    header_foreground: Color,
    header_hover_foreground: Color,
    accent: Color,
    warning: Color,
    alert: Color,
    info: Color,
}

impl SidebarPalette {
    fn from_surface(surface: &crate::tmux::SurfaceInventory) -> Self {
        let fallback = if surface.appearance == "light" {
            Self::light()
        } else {
            Self::dark()
        };
        if let Some(palette) = Self::from_json(&surface.palette, fallback) {
            return palette;
        }
        let mut palette = fallback;
        if surface.mode == "full" {
            palette.accent = tmux_color(&surface.accent).unwrap_or(Color::Indexed(2));
            palette.warning = Color::Indexed(3);
            palette.alert = Color::Indexed(1);
            palette.info = Color::Indexed(4);
        }
        palette
    }

    fn light() -> Self {
        Self {
            background: Color::Rgb(250, 250, 250),
            foreground: Color::Rgb(56, 58, 66),
            selection: Color::Rgb(229, 229, 230),
            muted: Color::Rgb(98, 102, 111),
            raised: Color::Rgb(208, 210, 216),
            hover: Color::Rgb(98, 102, 111),
            header_foreground: Color::Rgb(56, 58, 66),
            header_hover_foreground: Color::White,
            accent: Color::Rgb(80, 161, 79),
            warning: Color::Rgb(193, 132, 1),
            alert: Color::Rgb(228, 86, 73),
            info: Color::Rgb(64, 120, 242),
        }
    }

    fn dark() -> Self {
        Self {
            background: Color::Rgb(40, 44, 52),
            foreground: Color::Rgb(170, 178, 191),
            selection: Color::Rgb(62, 68, 82),
            muted: Color::Rgb(141, 149, 163),
            raised: Color::Rgb(75, 82, 99),
            hover: Color::Rgb(92, 99, 112),
            header_foreground: Color::White,
            header_hover_foreground: Color::White,
            accent: Color::Rgb(152, 195, 121),
            warning: Color::Rgb(229, 192, 123),
            alert: Color::Rgb(224, 108, 117),
            info: Color::Rgb(97, 175, 239),
        }
    }

    fn from_json(serialized: &str, fallback: Self) -> Option<Self> {
        let value: serde_json::Value = serde_json::from_str(serialized).ok()?;
        if value.get("version")?.as_u64()? != 1 {
            return None;
        }
        let color = |name: &str, default| {
            value
                .get(name)
                .and_then(serde_json::Value::as_str)
                .and_then(tmux_color)
                .unwrap_or(default)
        };
        Some(Self {
            background: color("background", fallback.background),
            foreground: color("foreground", fallback.foreground),
            selection: color("selection", fallback.selection),
            muted: color("muted", fallback.muted),
            raised: color("raised", fallback.raised),
            hover: color("hover", fallback.hover),
            header_foreground: color("header_foreground", fallback.header_foreground),
            header_hover_foreground: color(
                "header_hover_foreground",
                fallback.header_hover_foreground,
            ),
            accent: color("accent", fallback.accent),
            warning: color("warning", fallback.warning),
            alert: color("alert", fallback.alert),
            info: color("info", fallback.info),
        })
    }
}

impl Default for SidebarPalette {
    fn default() -> Self {
        Self::dark()
    }
}

fn tmux_color(value: &str) -> Option<Color> {
    let value = value.trim();
    if let Some(hex) = value.strip_prefix('#')
        && hex.len() == 6
    {
        let channel = |start| u8::from_str_radix(&hex[start..start + 2], 16).ok();
        return Some(Color::Rgb(channel(0)?, channel(2)?, channel(4)?));
    }
    let lower = value.to_ascii_lowercase();
    if let Some(index) = lower
        .strip_prefix("colour")
        .or_else(|| lower.strip_prefix("color"))
        .and_then(|index| index.parse::<u8>().ok())
    {
        return Some(Color::Indexed(index));
    }
    Some(match lower.as_str() {
        "default" => Color::Reset,
        "black" => Color::Black,
        "red" => Color::Red,
        "green" => Color::Green,
        "yellow" => Color::Yellow,
        "blue" => Color::Blue,
        "magenta" => Color::Magenta,
        "cyan" => Color::Cyan,
        "white" => Color::White,
        "brightblack" | "bright-black" => Color::DarkGray,
        "brightred" | "bright-red" => Color::LightRed,
        "brightgreen" | "bright-green" => Color::LightGreen,
        "brightyellow" | "bright-yellow" => Color::LightYellow,
        "brightblue" | "bright-blue" => Color::LightBlue,
        "brightmagenta" | "bright-magenta" => Color::LightMagenta,
        "brightcyan" | "bright-cyan" => Color::LightCyan,
        "brightwhite" | "bright-white" => Color::White,
        _ => return None,
    })
}

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
pub(crate) struct CachedSidebarInventory {
    topology: Vec<(String, String, bool)>,
    expires: Instant,
    rows: String,
    restore_token: String,
}

fn read_inventory(
    paths: &Paths,
    server: &ServerIdentity,
    snapshot: &Snapshot,
    cache: &mut Option<CachedSidebarInventory>,
    shared: Option<&crate::tmux::SurfaceInventory>,
) -> Option<(TeamIndex, FocusWindows, FocusSessions, HashSet<String>)> {
    let topology = snapshot
        .agents
        .iter()
        .map(|agent| {
            (
                agent.target.pane_id.clone(),
                agent.target.window_id.clone(),
                agent.exited,
            )
        })
        .collect::<Vec<_>>();
    let expired = cache
        .as_ref()
        .is_none_or(|cached| cached.topology != topology || Instant::now() >= cached.expires);
    if let Some(surface) = shared {
        if !expired {
            let cached = cache.as_mut()?;
            cached.rows.clone_from(&surface.rows);
            cached.restore_token.clone_from(&surface.restore_token);
        } else {
            *cache = Some(CachedSidebarInventory {
                topology,
                expires: Instant::now() + Duration::from_secs(45),
                rows: surface.rows.clone(),
                restore_token: surface.restore_token.clone(),
            });
        }
    } else if expired {
        // Keep the last rendered frame until the daemon can supply a fresh
        // shared inventory. A sidebar must not amplify a core failure into
        // one tmux query per resident UI.
        return None;
    }
    let inventory = cache.as_ref()?.rows.clone();
    let mut marked: HashMap<(String, String), HashSet<String>> = HashMap::new();
    let mut all_panes = HashSet::new();
    let mut focused = FocusWindows::new();
    let mut focused_sessions = FocusSessions::new();
    let saved_assistant = assistant_window(paths, snapshot);
    for line in inventory.lines() {
        let fields: Vec<_> = line.split('\t').collect();
        if fields.len() == 14 {
            all_panes.insert(fields[1].to_owned());
            if fields[13] == "1" && safe_target(fields[5], '$') {
                focused_sessions.insert(fields[5].to_owned());
            }
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
        if fields.len() != 14
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
    let restore_token = cache.as_ref()?.restore_token.trim();
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
    Some((index, focused, focused_sessions, all_panes))
}

pub(crate) fn shared_sidebar_data(
    paths: &Paths,
    server: &ServerIdentity,
    snapshot: &Snapshot,
    cache: &mut Option<CachedSidebarInventory>,
    surface: &crate::tmux::SurfaceInventory,
) -> Option<SharedSidebarData> {
    let (teams, focused, sessions, live_panes) =
        read_inventory(paths, server, snapshot, cache, Some(surface))?;
    let session_paths: HashSet<_> = snapshot
        .sessions
        .iter()
        .filter_map(|session| session.current_path.as_deref())
        .filter(|path| !path.is_empty())
        .collect();
    let branches = session_paths
        .into_iter()
        .filter_map(|path| {
            git_branch(std::path::Path::new(path)).map(|branch| (path.into(), branch))
        })
        .collect();
    let selection_targets = surface
        .rows
        .lines()
        .filter_map(|row| {
            let fields: Vec<_> = row.split('\t').collect();
            let window = *fields.first()?;
            let pane = *fields.get(1)?;
            // A sidebar only represents the previous pane while it is the
            // active pane. In a window with multiple panes, pane_last can
            // otherwise refer to an unrelated, inactive sidebar.
            if fields.get(11) == Some(&"sidebar") && fields.get(9) != Some(&"1") {
                return None;
            }
            Some((pane.to_owned(), surface.last_panes.get(window)?.clone()))
        })
        .collect();
    Some(SharedSidebarData {
        teams,
        focused,
        sessions,
        live_panes,
        branches,
        selection_targets,
        // An absent preference means the default view. Send it explicitly so
        // a newly opened sidebar can discard a stale shared Sessions value.
        view: Some(persisted_sidebar_view(paths).unwrap_or(SidebarView::Agents)),
        sort: persisted_agent_sort(paths),
    })
}

/// Shared geometry for rendering and hit testing. Status, overflow and popup
/// footer lines and the pinned action bar never map to an agent row.
#[derive(Debug, Clone, Copy)]
struct SidebarViewport {
    first_row: usize,
    row_height: usize,
}

impl SidebarViewport {
    fn new(content_height: usize, notice: Option<&str>, focus_cards: usize) -> Self {
        // The status bar and pinned cards stay above the scrolling body.
        let first_row = 2 + focus_cards * 2 + usize::from(notice.is_some());
        let available = content_height.saturating_sub(first_row);
        let row_height = if notice == Some("disconnected") {
            0
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
    View,
    Pinned,
    Menu,
    Close,
    Sort,
    Filter(DisplayState),
    Focus(String),
    FocusSession(String),
    ToggleSession(String),
}

pub fn run(paths: &Paths, server: &ServerIdentity) -> Result<(), Box<dyn std::error::Error>> {
    run_with_details(paths, server, false)
}

pub fn run_with_details(
    paths: &Paths,
    server: &ServerIdentity,
    initial_details: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    if let Ok(config) = Config::load(&paths.config_file()) {
        crate::ui_language::set_configured_language(&config.ui.language);
    }
    let runtime_identity = crate::layout::executable_identity()?;
    // Resolve the first frame before switching to the alternate screen. Some
    // remote clients do not paint an initially empty fullscreen popup until a
    // later input event; entering with real rows makes the UI visible at once.
    let initial_snapshot =
        fetch_snapshot_with_surface(&paths.socket_for_server(&server.key), false);
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
        initial_details,
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

pub fn open_popup_with_details(
    server: &ServerIdentity,
    pane: &str,
    client: &str,
    details: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    if details {
        popup(server, pane, client, &["sidebar", "--details"])
    } else {
        open_popup(server, pane, client)
    }
}

pub fn open_popup_detailed(
    server: &ServerIdentity,
    pane: &str,
    client: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    open_popup_with_details(server, pane, client, true)
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

fn switch_sidebar_view(
    paths: &Paths,
    view: &mut SidebarView,
    snapshot: Option<&Snapshot>,
    detailed: bool,
    sort: AgentSort,
    teams: &TeamIndex,
    expanded: &HashSet<String>,
    state_filter: AgentStateFilter,
    rows: &mut Vec<Row>,
    navigation: &mut NavigationList<Action>,
) -> io::Result<()> {
    *view = view.toggle();
    persist_sidebar_view(paths, *view)?;
    *rows = snapshot
        .map(|value| view_rows(value, detailed, sort, teams, expanded, *view, state_filter))
        .unwrap_or_default();
    *navigation = NavigationList::default();
    navigation.replace_rows(rows.iter().map(selection_key).collect());
    Ok(())
}

fn apply_shared_preference<T: Copy + Eq>(
    current: &mut T,
    pending: &mut Option<(T, Instant)>,
    shared: Option<T>,
) -> bool {
    if let Some((requested, at)) = *pending {
        if shared == Some(requested) || at.elapsed() >= Duration::from_secs(3) {
            *pending = None;
        } else {
            return false;
        }
    }
    if let Some(value) = shared
        && value != *current
    {
        *current = value;
        return true;
    }
    false
}

fn event_loop(
    terminal: &mut Terminal<crate::popup_surface::Surface<CrosstermBackend<io::Stdout>>>,
    paths: &Paths,
    server: &ServerIdentity,
    initial_snapshot: Result<
        (Snapshot, Option<crate::tmux::SurfaceInventory>),
        Box<dyn std::error::Error>,
    >,
    runtime_identity: &str,
    initial_details: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    let socket = paths.socket_for_server(&server.key);
    let fetch_socket = socket.clone();
    let (refresh_tx, refresh_rx) = mpsc::sync_channel::<bool>(1);
    let visibility_pane = std::env::var("TMUX_PANE").ok();
    let is_popup = popup_mode();
    let (snapshot_tx, snapshot_rx) = mpsc::sync_channel(1);
    let low_process_churn = Config::load(&paths.config_file())
        .map(|config| config.detection.low_process_churn)
        .unwrap_or(false);
    std::thread::spawn(move || {
        let mut visible = true;
        while let Ok(interactive) = refresh_rx.recv() {
            let fetched = match fetch_snapshot_with_surface(&fetch_socket, interactive) {
                Ok(fetched) => fetched,
                Err(error) => {
                    // Retain the last frame if the shared core is temporarily busy.
                    let _ = snapshot_tx.try_send((Err(error.to_string()), visible, None));
                    continue;
                }
            };
            let surface = fetched.1.as_ref();
            if let Some(now_visible) = surface.and_then(|surface| {
                visibility_pane
                    .as_ref()
                    .and_then(|pane| surface.visibility.get(pane))
            }) {
                visible = *now_visible;
            }
            let shared_palette = surface.map(SidebarPalette::from_surface);
            let (snapshot, surface) = fetched;
            let result = sidebar_state(snapshot, surface.as_ref());
            let _ = snapshot_tx.try_send((result, visible, shared_palette));
        }
    });
    let mut detailed = initial_details;
    let mut agent_sort = Config::load(&paths.config_file())
        .map(|config| config.sidebar.agent_sort)
        .unwrap_or_default();
    let mut view = SidebarView::Agents;
    let initial_palette = initial_snapshot
        .as_ref()
        .ok()
        .and_then(|(_, surface)| surface.as_ref())
        .map(SidebarPalette::from_surface);
    let initial_state = initial_snapshot
        .map_err(|error| error.to_string())
        .and_then(|(snapshot, surface)| sidebar_state(snapshot, surface.as_ref()));
    let (mut snapshot, initial_data) = match initial_state {
        Ok((snapshot, data)) => (Some(snapshot), data),
        Err(_) => (None, SharedSidebarData::default()),
    };
    if let Some(saved) = initial_data.sort {
        agent_sort = saved;
    }
    if let Some(saved) = initial_data.view {
        view = saved;
    }
    let mut teams = initial_data.teams;
    let mut focused = initial_data.focused;
    let mut focused_sessions = initial_data.sessions;
    let mut branches = initial_data.branches;
    let mut selection_targets = initial_data.selection_targets;
    let mut pending_view = None;
    let mut pending_sort = None;
    let mut expanded = HashSet::new();
    let mut state_filter = AgentStateFilter::default();
    let mut rows = snapshot
        .as_ref()
        .map(|snapshot| {
            view_rows(
                snapshot,
                detailed,
                agent_sort,
                &teams,
                &expanded,
                view,
                state_filter,
            )
        })
        .unwrap_or_default();
    let mut focus_cards = build_pinned_cards(
        view,
        detailed,
        agent_sort,
        &teams,
        &expanded,
        &focused,
        &focused_sessions,
        snapshot.as_ref(),
    );
    let mut footer_hover = None;
    let mut navigation = NavigationList::<Action>::default();
    navigation.replace_rows(rows.iter().map(selection_key).collect());
    let mut startup_selection = None;
    if let Some(snapshot) = &snapshot
        && let Some(index) = initial_row(&rows, snapshot, &selection_targets, is_popup)
    {
        if !is_popup {
            startup_selection = selection_key(&rows[index]);
        }
        navigation.select(index);
    }
    let mut disconnected = snapshot.is_none();
    let mut last_success = snapshot.as_ref().map(|_| Instant::now());
    let mut last_failure_log = Instant::now() - Duration::from_secs(10);
    let mut visible = true;
    let mut next_refresh = Instant::now();
    let mut palette = initial_palette.unwrap_or_default();
    let mut dirty = true;
    let mut initial_selection = snapshot.is_none();
    let mut last_content_height = usize::MAX;
    let mut runtime_registered = is_popup;
    loop {
        if let Ok((result, now_visible, shared_palette)) = snapshot_rx.try_recv() {
            if let Some(updated) = shared_palette {
                if updated != palette {
                    palette = updated;
                }
            }
            visible = now_visible;
            // Low-churn mode avoids subprocesses using a cached inventory;
            // keep IPC fresh even if a window becomes visible without a
            // focus event, rather than waiting for the slow visibility probe.
            next_refresh = Instant::now() + refresh_interval(visible || low_process_churn);
            match result {
                Ok((fetched, data)) => {
                    let view_changed =
                        apply_shared_preference(&mut view, &mut pending_view, data.view);
                    apply_shared_preference(&mut agent_sort, &mut pending_sort, data.sort);
                    teams = data.teams;
                    focused = data.focused;
                    focused_sessions = data.sessions;
                    branches = data.branches;
                    selection_targets = data.selection_targets;
                    rows = view_rows(
                        &fetched,
                        detailed,
                        agent_sort,
                        &teams,
                        &expanded,
                        view,
                        state_filter,
                    );
                    if view_changed {
                        navigation = NavigationList::default();
                    }
                    navigation.replace_rows(rows.iter().map(selection_key).collect());
                    disconnected = false;
                    last_success = Some(Instant::now());
                    if initial_selection {
                        initial_selection = false;
                        let index = initial_row(&rows, &fetched, &selection_targets, is_popup);
                        if !is_popup {
                            startup_selection = index.and_then(|index| selection_key(&rows[index]));
                        }
                        navigation.select(index.unwrap_or(0));
                    } else if let Some(selected) = startup_selection.as_ref() {
                        let current = current_agent_selection(&rows, &fetched, &selection_targets)
                            .and_then(|index| selection_key(&rows[index]));
                        if current.as_ref() != Some(selected) {
                            // The sidebar can start while it is temporarily
                            // focused, before tmux returns to the main pane.
                            // Do not retain that stale automatic scroll after
                            // a newer shared frame observes the focus change.
                            navigation = NavigationList::default();
                            navigation.replace_rows(rows.iter().map(selection_key).collect());
                            startup_selection = None;
                        }
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
            next_refresh = Instant::now() + refresh_interval(visible || low_process_churn);
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
            focus_cards = build_pinned_cards(
                view,
                detailed,
                agent_sort,
                &teams,
                &expanded,
                &focused,
                &focused_sessions,
                snapshot.as_ref(),
            );
        }
        let shown_focus = visible_focus_cards(content_height, notice, focus_cards.len());
        let viewport = SidebarViewport::new(content_height, notice, shown_focus);
        navigation.set_viewport(Rect::new(
            0,
            viewport.first_row as u16,
            size.width,
            viewport.row_height as u16,
        ));
        let status_counts = agent_state_counts(snapshot.as_ref());
        let leading: Vec<_> = action_regions(
            Rect::new(0, 0, size.width, size.height),
            view,
            agent_sort,
            popup_mode(),
            status_counts,
            &focus_cards[..shown_focus],
            focus_cards.len(),
        )
        .into_iter()
        .filter(|(_, action)| matches!(action, Action::Focus(_) | Action::FocusSession(_)))
        .collect();
        navigation.set_leading_actions(&leading);
        if dirty {
            terminal.draw(|frame| {
                let area = frame.area();
                let mut header = vec![render_header_with_palette(
                    area.width,
                    view,
                    agent_sort,
                    focus_cards.len(),
                    shown_focus,
                    status_counts,
                    state_filter,
                    footer_hover.as_ref(),
                    palette,
                )];
                for card in focus_cards.iter().take(shown_focus) {
                    let action = card.action();
                    let highlighted = footer_hover.as_ref() == Some(&action)
                        || navigation.is_highlighted(&Hit::Action(action));
                    header.extend(render_pinned_card_with_palette(
                        card,
                        highlighted,
                        area.width,
                        palette,
                        &branches,
                    ));
                }
                if let Some(notice) = notice {
                    let notice = match notice {
                        "disconnected" => crate::ui_language::text("disconnected", "已断开"),
                        "stale · reconnecting" => {
                            crate::ui_language::text("stale · reconnecting", "数据过期 · 正在重连")
                        }
                        _ => notice,
                    };
                    header.push(Line::from(Span::styled(
                        notice,
                        muted_style_with_palette(palette),
                    )));
                }
                frame.render_widget(Paragraph::new(header), area);
                crate::navigation_list::separator_with_color(
                    frame,
                    Rect::new(0, (viewport.first_row - 1) as u16, area.width, 1),
                    palette.muted,
                );
                navigation.render(frame, |index, selected| {
                    let row = &rows[index];
                    if let (SidebarView::Sessions, Row::Session(session)) = (view, row)
                        && area.width >= 8
                    {
                        let mut line = render_sidebar_row_with_palette(
                            row,
                            selected,
                            area.width - 3,
                            &focused,
                            palette,
                            &branches,
                        );
                        let mut style = Style::default().fg(
                            if focused_sessions.contains(&session.session_id) {
                                palette.accent
                            } else {
                                palette.muted
                            },
                        );
                        if selected {
                            style = style.bg(palette.selection);
                        }
                        line.spans.push(Span::styled("  ", style));
                        line
                    } else {
                        render_sidebar_row_with_palette(
                            row, selected, area.width, &focused, palette, &branches,
                        )
                    }
                });
                if rows.is_empty() && notice != Some("disconnected") {
                    frame.render_widget(
                        Paragraph::new(match view {
                            SidebarView::Agents => {
                                crate::ui_language::text("no agents", "暂无智能体")
                            }
                            SidebarView::Sessions => {
                                crate::ui_language::text("no sessions", "暂无会话")
                            }
                        })
                        .style(muted_style_with_palette(palette)),
                        Rect::new(
                            0,
                            viewport.first_row as u16,
                            area.width,
                            1.min(area.height.saturating_sub(viewport.first_row as u16)),
                        ),
                    );
                }
                if popup_mode() && area.height > 0 {
                    crate::navigation_list::separator_with_color(
                        frame,
                        Rect::new(
                            0,
                            area.height.saturating_sub(2),
                            area.width,
                            u16::from(area.height > 1),
                        ),
                        palette.muted,
                    );
                    frame.render_widget(
                        Paragraph::new(render_close_with_palette(
                            area.width,
                            footer_hover.as_ref(),
                            palette,
                        )),
                        Rect::new(0, area.height - 1, area.width, 1),
                    );
                }
            })?;
            if !runtime_registered && snapshot.is_some() {
                crate::layout::register_sidebar_runtime(server, runtime_identity)?;
                runtime_registered = true;
            }
            dirty = false;
        }

        let input = if let Some(input) = terminal.backend_mut().take_pending_input() {
            input
        } else {
            if !event::poll(if visible {
                Duration::from_millis(50)
            } else {
                Duration::from_millis(250)
            })? {
                continue;
            }
            event::read()?
        };
        let Some(input) = terminal.backend_mut().input(input)? else {
            continue;
        };
        if matches!(input, Event::FocusGained | Event::Key(_) | Event::Mouse(_)) {
            // Never move the viewport underneath a pending click or undo the
            // user's own selection when the next shared frame arrives.
            startup_selection = None;
        }
        if matches!(input, Event::FocusGained) || (!visible && !matches!(input, Event::FocusLost)) {
            let _ = refresh_tx.try_send(true);
            visible = true;
            next_refresh = Instant::now() + refresh_interval(true);
        }
        dirty = true;
        match input {
            Event::FocusGained => {
                // tmux focuses this pane before forwarding a mouse click. Keep
                // the selection and viewport stable until that click is handled;
                // selecting the current agent here can scroll a different row
                // underneath the pending mouse coordinates. Initial selection
                // is resolved when the first snapshot arrives above.
                if navigation.focused_action().is_none() {
                    navigation.selection_visible = true;
                }
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
                            && activate_pinned(card)?
                            && popup_mode()
                        {
                            return Ok(());
                        }
                    }
                    KeyCode::Char('p') => {
                        let result = match navigation.focused_action().or(footer_hover.as_ref()) {
                            Some(Action::Focus(window)) => {
                                let assistant =
                                    focused.get(window).is_some_and(|item| item.assistant);
                                Some(toggle_focus(server, window, assistant))
                            }
                            Some(Action::FocusSession(session)) => {
                                Some(toggle_session_focus(server, session))
                            }
                            _ if navigation.selection_visible => {
                                match rows.get(navigation.selected) {
                                    Some(Row::Session(session) | Row::SessionSub(session)) => {
                                        Some(toggle_session_focus(server, &session.session_id))
                                    }
                                    _ => {
                                        selected_window(&rows, navigation.selected).map(|window| {
                                            let assistant = focused
                                                .get(window)
                                                .is_some_and(|item| item.assistant);
                                            toggle_focus(server, window, assistant)
                                        })
                                    }
                                }
                            }
                            _ => None,
                        };
                        if let Some(result) = result {
                            if let Err(error) = result {
                                append_sidebar_log(paths, &format!("focus-toggle error={error}"));
                            }
                            let _ = refresh_tx.try_send(true);
                            next_refresh = Instant::now();
                        }
                    }
                    KeyCode::Tab => {
                        switch_sidebar_view(
                            paths,
                            &mut view,
                            snapshot.as_ref(),
                            detailed,
                            agent_sort,
                            &teams,
                            &expanded,
                            state_filter,
                            &mut rows,
                            &mut navigation,
                        )?;
                        pending_view = Some((view, Instant::now()));
                        let _ = refresh_tx.try_send(true);
                    }
                    _ if navigation.navigate(key.code) => {}
                    KeyCode::Char(' ' | 'h' | 'l') | KeyCode::Left | KeyCode::Right => {
                        if navigation.selection_visible
                            && let Some(Row::Team(_, summary) | Row::TeamSub(_, _, summary)) =
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
                                rows = view_rows(
                                    snapshot,
                                    detailed,
                                    agent_sort,
                                    &teams,
                                    &expanded,
                                    view,
                                    state_filter,
                                );
                                navigation.replace_rows(rows.iter().map(selection_key).collect());
                            }
                        }
                    }
                    KeyCode::Enter => {
                        let navigated = match navigation.focused_action() {
                            Some(action @ (Action::Focus(_) | Action::FocusSession(_))) => {
                                focus_cards
                                    .iter()
                                    .take(shown_focus)
                                    .find(|card| card.action() == *action)
                                    .map(activate_pinned)
                                    .transpose()?
                                    .unwrap_or(false)
                            }
                            _ => activate(&rows, navigation.selected)?,
                        };
                        if popup_mode() && navigated {
                            return Ok(());
                        }
                    }
                    KeyCode::Char('m') => {
                        let shown = match navigation.focused_action() {
                            Some(action @ (Action::Focus(_) | Action::FocusSession(_))) => {
                                if let Some((index, card)) = focus_cards
                                    .iter()
                                    .take(shown_focus)
                                    .enumerate()
                                    .find(|(_, card)| card.action() == *action)
                                {
                                    show_pinned_card_menu(card, Some((0, 1 + 2 * index as u16)))?
                                } else {
                                    false
                                }
                            }
                            _ => show_row_menu(
                                &rows,
                                navigation.selected,
                                Some((
                                    0,
                                    (viewport.first_row
                                        + navigation.selected.saturating_sub(navigation.scroll))
                                        as u16,
                                )),
                                &focused,
                                &focused_sessions,
                            )?,
                        };
                        if shown {
                            return Ok(());
                        }
                    }
                    KeyCode::Char('d') => {
                        detailed = !detailed;
                        if let Some(snapshot) = &snapshot {
                            rows = view_rows(
                                snapshot,
                                detailed,
                                agent_sort,
                                &teams,
                                &expanded,
                                view,
                                state_filter,
                            );
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
                let mut actions = action_regions(
                    Rect::new(0, 0, size.width, size.height),
                    view,
                    agent_sort,
                    popup_mode(),
                    status_counts,
                    &focus_cards[..shown_focus],
                    focus_cards.len(),
                );
                if view == SidebarView::Sessions && size.width >= 8 {
                    for index in navigation.visible_rows() {
                        if let Some(Row::Session(session)) = rows.get(index) {
                            let y = viewport.first_row + index - navigation.scroll;
                            actions.push((
                                Rect::new(size.width - 3, y as u16, 3, 1),
                                Action::ToggleSession(session.session_id.clone()),
                            ));
                        }
                    }
                }
                let hit = navigation.mouse(mouse, &actions);
                let hit = if mouse.kind == MouseEventKind::Down(MouseButton::Right) {
                    navigation
                        .hit(mouse.column, mouse.row, &actions)
                        .filter(|hit| {
                            matches!(
                                hit,
                                Hit::Item(_)
                                    | Hit::Action(Action::Focus(_) | Action::FocusSession(_))
                            )
                        })
                } else {
                    hit
                };
                footer_hover = match &navigation.hovered {
                    Some(Hit::Action(action)) => Some(action.clone()),
                    _ => None,
                };
                match hit {
                    Some(Hit::Action(Action::Close)) => return Ok(()),
                    Some(Hit::Action(Action::View)) => {
                        switch_sidebar_view(
                            paths,
                            &mut view,
                            snapshot.as_ref(),
                            detailed,
                            agent_sort,
                            &teams,
                            &expanded,
                            state_filter,
                            &mut rows,
                            &mut navigation,
                        )?;
                        pending_view = Some((view, Instant::now()));
                        let _ = refresh_tx.try_send(true);
                    }
                    Some(Hit::Action(Action::Pinned)) => {
                        if show_pinned_menu(&focus_cards, view, Some((mouse.column, mouse.row)))? {
                            return Ok(());
                        }
                    }
                    Some(Hit::Action(Action::ToggleSession(session))) => {
                        if let Err(error) = toggle_session_focus(server, &session) {
                            append_sidebar_log(paths, &format!("focus-toggle error={error}"));
                        }
                        let _ = refresh_tx.try_send(true);
                        next_refresh = Instant::now();
                    }
                    Some(Hit::Action(Action::Menu)) => {
                        if show_sidebar_menu()? {
                            return Ok(());
                        }
                    }
                    Some(Hit::Action(Action::Filter(state))) => {
                        let selected = rows.get(navigation.selected).and_then(selection_key);
                        state_filter.toggle(state);
                        if let Some(snapshot) = &snapshot {
                            rows = view_rows(
                                snapshot,
                                detailed,
                                agent_sort,
                                &teams,
                                &expanded,
                                view,
                                state_filter,
                            );
                            navigation = NavigationList::default();
                            navigation.replace_rows(rows.iter().map(selection_key).collect());
                            if let Some(index) = selected.as_ref().and_then(|selected| {
                                rows.iter()
                                    .position(|row| selection_key(row).as_ref() == Some(selected))
                            }) {
                                navigation.select(index);
                            }
                        }
                    }
                    Some(Hit::Action(Action::Sort)) => {
                        agent_sort = match agent_sort {
                            AgentSort::Grouped => AgentSort::Prioritized,
                            AgentSort::Prioritized => AgentSort::Grouped,
                        };
                        persist_agent_sort(paths, agent_sort)?;
                        pending_sort = Some((agent_sort, Instant::now()));
                        let _ = refresh_tx.try_send(true);
                        if let Some(snapshot) = &snapshot {
                            rows = view_rows(
                                snapshot,
                                detailed,
                                agent_sort,
                                &teams,
                                &expanded,
                                view,
                                state_filter,
                            );
                            navigation = NavigationList::default();
                            navigation.replace_rows(rows.iter().map(selection_key).collect());
                        }
                    }
                    Some(Hit::Action(action @ (Action::Focus(_) | Action::FocusSession(_)))) => {
                        if let Some(card) = focus_cards
                            .iter()
                            .take(shown_focus)
                            .find(|card| card.action() == action)
                        {
                            let navigated =
                                if mouse.kind == MouseEventKind::Down(MouseButton::Right) {
                                    show_pinned_card_menu(card, Some((mouse.column, mouse.row)))?
                                } else {
                                    activate_pinned(card)?
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
                                    rows = view_rows(
                                        snapshot,
                                        detailed,
                                        agent_sort,
                                        &teams,
                                        &expanded,
                                        view,
                                        state_filter,
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
                                &focused_sessions,
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

#[cfg(test)]
fn fetch_snapshot(socket: &std::path::Path) -> Result<Snapshot, Box<dyn std::error::Error>> {
    Ok(fetch_snapshot_with_surface(socket, false)?.0)
}

fn fetch_snapshot_with_surface(
    socket: &std::path::Path,
    refresh_surface: bool,
) -> Result<(Snapshot, Option<crate::tmux::SurfaceInventory>), Box<dyn std::error::Error>> {
    let value = call(
        socket,
        &Request::new(
            "snapshot.get",
            serde_json::json!({
                "include_surface": true,
                "refresh_surface": refresh_surface,
            }),
        ),
        Duration::from_millis(500),
    )?;
    let surface = value
        .get("_workbench_surface")
        .cloned()
        .map(serde_json::from_value)
        .transpose()?;
    Ok((serde_json::from_value(value)?, surface))
}

// A snapshot and its team identities form one display frame. A temporary
// tmux inventory failure must not promote retained workers to ordinary agents.
fn sidebar_state(
    mut snapshot: Snapshot,
    shared: Option<&crate::tmux::SurfaceInventory>,
) -> Result<(Snapshot, SharedSidebarData), String> {
    let data = shared
        .and_then(|surface| surface.sidebar.as_ref())
        .ok_or_else(|| "shared sidebar inventory unavailable".to_owned())?;
    prune_closed_pane_tombstones(&mut snapshot, &data.live_panes);
    Ok((snapshot, data.clone()))
}

fn prune_closed_pane_tombstones(snapshot: &mut Snapshot, live_panes: &HashSet<String>) {
    let active_panes: HashSet<_> = snapshot
        .agents
        .iter()
        .filter(|agent| !agent.exited)
        .map(|agent| agent.target.pane_id.clone())
        .collect();
    snapshot.agents.retain(|agent| {
        !agent.exited
            || (live_panes.contains(&agent.target.pane_id)
                && !active_panes.contains(&agent.target.pane_id))
    });
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

#[cfg(test)]
fn build_team_rows(
    snapshot: &Snapshot,
    detailed: bool,
    agent_sort: AgentSort,
    teams: &TeamIndex,
    expanded: &HashSet<String>,
) -> Vec<Row> {
    build_team_rows_filtered(
        snapshot,
        detailed,
        agent_sort,
        teams,
        expanded,
        AgentStateFilter::default(),
    )
}

fn build_team_rows_filtered(
    snapshot: &Snapshot,
    detailed: bool,
    agent_sort: AgentSort,
    teams: &TeamIndex,
    expanded: &HashSet<String>,
    state_filter: AgentStateFilter,
) -> Vec<Row> {
    let mut agents = snapshot.agents.clone();
    agents.retain(|agent| state_filter.is_visible(agent.display_state));
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

fn view_rows(
    snapshot: &Snapshot,
    detailed: bool,
    sort: AgentSort,
    teams: &TeamIndex,
    expanded: &HashSet<String>,
    view: SidebarView,
    state_filter: AgentStateFilter,
) -> Vec<Row> {
    if view == SidebarView::Agents {
        let mut rows =
            build_team_rows_filtered(snapshot, detailed, sort, teams, expanded, state_filter);
        if let Some(section) = rows
            .iter()
            .position(|row| matches!(row, Row::Section("sessions")))
        {
            rows.truncate(section.saturating_sub(1));
        }
        rows
    } else {
        let mut sessions = snapshot.sessions.clone();
        sessions.sort_by_key(|session| {
            let priority = if sort == AgentSort::Prioritized {
                std::cmp::Reverse(agent_attention_priority_state(session.rollup_state))
            } else {
                std::cmp::Reverse(0)
            };
            (priority, stable_session_key(session))
        });
        sessions
            .into_iter()
            .flat_map(|session| [Row::Session(session.clone()), Row::SessionSub(session)])
            .collect()
    }
}

fn build_pinned_cards(
    view: SidebarView,
    detailed: bool,
    sort: AgentSort,
    teams: &TeamIndex,
    expanded: &HashSet<String>,
    focused: &FocusWindows,
    focused_sessions: &FocusSessions,
    snapshot: Option<&Snapshot>,
) -> Vec<PinnedCard> {
    match view {
        SidebarView::Agents => {
            let rows = snapshot
                .map(|value| {
                    view_rows(
                        value,
                        detailed,
                        sort,
                        teams,
                        expanded,
                        view,
                        AgentStateFilter::default(),
                    )
                })
                .unwrap_or_default();
            build_focus_cards(&rows, focused)
                .into_iter()
                .map(PinnedCard::Agent)
                .collect()
        }
        SidebarView::Sessions => {
            let mut sessions: Vec<_> = snapshot
                .into_iter()
                .flat_map(|value| value.sessions.iter())
                .filter(|session| focused_sessions.contains(&session.session_id))
                .cloned()
                .collect();
            sessions.sort_by_key(stable_session_key);
            sessions.into_iter().map(PinnedCard::Session).collect()
        }
    }
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

#[cfg(test)]
fn render_focus_card(card: &FocusCard, selected: bool, width: u16) -> [Line<'static>; 2] {
    render_focus_card_with_palette(card, selected, width, SidebarPalette::default())
}

fn render_focus_card_with_palette(
    card: &FocusCard,
    selected: bool,
    width: u16,
    palette: SidebarPalette,
) -> [Line<'static>; 2] {
    let title = if let Some(agent) = &card.agent {
        let mut agent = agent.clone();
        if card.window.assistant {
            agent.thread_name =
                Some(crate::ui_language::text("Personal assistant", "个人助理").into());
        }
        render_row_with_palette(&Row::Agent(agent), selected, width, palette)
    } else {
        let title = if card.window.assistant {
            crate::ui_language::text("Personal assistant", "个人助理")
        } else if meaningful_topic(&card.window.name) {
            &card.window.name
        } else if !card.window.session_name.trim().is_empty() {
            card.window.session_name.trim()
        } else {
            crate::ui_language::text("Blank session", "空白会话")
        };
        icon_title_line_with_palette(
            glyph(card.window.state),
            title,
            width,
            Style::default().fg(state_color_with_palette(card.window.state, palette)),
            Style::default().fg(palette.foreground),
            selected,
            palette,
        )
    };
    let subtitle = if let Some(agent) = &card.agent {
        if let Some(team) = &card.team {
            render_row_with_palette(
                &Row::TeamSub(agent.clone(), AgentSort::Prioritized, team.clone()),
                selected,
                width,
                palette,
            )
        } else {
            render_row_with_palette(
                &Row::AgentSub(agent.clone(), AgentSort::Prioritized),
                selected,
                width,
                palette,
            )
        }
    } else {
        aligned_line_with_palette(
            format!("   {}", card.window.session_name),
            state_name(card.window.state).into(),
            width,
            muted_style_with_palette(palette),
            Style::default().fg(state_color_with_palette(card.window.state, palette)),
            selected,
            palette,
        )
    };
    [title, subtitle]
}

fn render_pinned_card_with_palette(
    card: &PinnedCard,
    selected: bool,
    width: u16,
    palette: SidebarPalette,
    branches: &HashMap<String, String>,
) -> [Line<'static>; 2] {
    match card {
        PinnedCard::Agent(card) => render_focus_card_with_palette(card, selected, width, palette),
        PinnedCard::Session(session) => {
            let title =
                render_row_with_palette(&Row::Session(session.clone()), selected, width, palette);
            let subtitle = render_row_with_branches(
                &Row::SessionSub(session.clone()),
                selected,
                width,
                palette,
                Some(branches),
            );
            [title, subtitle]
        }
    }
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
    agent_attention_priority_state(agent.display_state)
}

fn agent_attention_priority_state(state: DisplayState) -> u8 {
    match state {
        DisplayState::Blocked => 4,
        DisplayState::Done => 3,
        DisplayState::Working => 2,
        DisplayState::Idle => 1,
        DisplayState::Unknown => 0,
    }
}

fn persist_sidebar_view(paths: &Paths, view: SidebarView) -> io::Result<()> {
    std::fs::create_dir_all(&paths.state_dir)?;
    std::fs::write(
        paths.state_dir.join("sidebar-view"),
        match view {
            SidebarView::Agents => "agents\n",
            SidebarView::Sessions => "sessions\n",
        },
    )
}

fn persisted_sidebar_view(paths: &Paths) -> Option<SidebarView> {
    match std::fs::read_to_string(paths.state_dir.join("sidebar-view"))
        .ok()?
        .trim()
    {
        "agents" => Some(SidebarView::Agents),
        "sessions" => Some(SidebarView::Sessions),
        _ => None,
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
    selection_targets: &HashMap<String, String>,
) -> Option<usize> {
    let instance = current_agent_instance(snapshot, selection_targets)?;
    // Prefer the canonical Agent row when restoring focus after a refresh.
    rows.iter()
        .rposition(|row| matches!(row, Row::Agent(agent) | Row::Team(agent, _) | Row::TeamWorker(agent, _) if agent.instance_id == instance))
}

fn initial_row(
    rows: &[Row],
    snapshot: &Snapshot,
    selection_targets: &HashMap<String, String>,
    popup: bool,
) -> Option<usize> {
    if popup {
        // An overview opens at the top, including the first Session group.
        // Following a lower current Agent used to hide the heading and begin
        // in the middle of a group as soon as the popup opened.
        rows.iter().position(|row| selection_key(row).is_some())
    } else {
        current_agent_selection(rows, snapshot, selection_targets)
    }
}

fn current_agent_instance(
    snapshot: &Snapshot,
    selection_targets: &HashMap<String, String>,
) -> Option<String> {
    let source = source_pane()?;
    if let Some(agent) = snapshot
        .agents
        .iter()
        .find(|agent| agent.target.pane_id == source)
    {
        return Some(agent.instance_id.clone());
    }

    let previous = selection_targets.get(&source)?;
    snapshot
        .agents
        .iter()
        .find(|agent| agent.target.pane_id == *previous)
        .map(|agent| agent.instance_id.clone())
}

#[cfg(test)]
fn render_row(row: &Row, selected: bool, width: u16) -> Line<'static> {
    render_row_with_palette(row, selected, width, SidebarPalette::default())
}

fn render_row_with_palette(
    row: &Row,
    selected: bool,
    width: u16,
    palette: SidebarPalette,
) -> Line<'static> {
    render_row_with_branches(row, selected, width, palette, None)
}

fn render_row_with_branches(
    row: &Row,
    selected: bool,
    width: u16,
    palette: SidebarPalette,
    branches: Option<&HashMap<String, String>>,
) -> Line<'static> {
    match row {
        Row::Section(label) => Line::from(Span::styled(
            if *label == "sessions" {
                crate::ui_language::text("sessions", "会话")
            } else {
                *label
            },
            muted_style_with_palette(palette).add_modifier(Modifier::BOLD),
        )),
        Row::AgentGroup(name) => Line::from(Span::styled(
            fit_text(name, usize::from(width)),
            muted_style_with_palette(palette).add_modifier(Modifier::BOLD),
        )),
        Row::Session(session) => icon_title_line_with_palette(
            glyph(session.rollup_state),
            &session.session_name,
            width,
            Style::default().fg(state_color_with_palette(session.rollup_state, palette)),
            if session.active {
                Style::default()
                    .fg(palette.foreground)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(palette.foreground)
            },
            selected,
            palette,
        ),
        Row::SessionSub(session) => {
            let branch = session
                .current_path
                .as_ref()
                .and_then(|path| branches.and_then(|branches| branches.get(path)))
                .map(String::as_str);
            context_line_with_palette(&session_context(session, branch), width, selected, palette)
        }
        Row::Team(agent, summary) => icon_title_line_with_palette(
            if summary.expanded { "▾" } else { "▸" },
            agent_topic(agent),
            width,
            Style::default().fg(state_color_with_palette(agent.display_state, palette)),
            Style::default().fg(palette.foreground),
            selected,
            palette,
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
            aligned_line_with_palette(
                text,
                status,
                width,
                muted_style_with_palette(palette),
                Style::default().fg(state_color_with_palette(agent.display_state, palette)),
                selected,
                palette,
            )
        }
        Row::TeamWorker(agent, display) => {
            let title = format!("   └ {} {}", glyph(agent.display_state), display.title);
            aligned_line_with_palette(
                title,
                String::new(),
                width,
                Style::default().fg(palette.foreground),
                muted_style_with_palette(palette),
                selected,
                palette,
            )
        }
        Row::TeamWorkerSub(agent, display) => aligned_line_with_palette(
            format!("       {}", display.name),
            agent_status(agent),
            width,
            muted_style_with_palette(palette),
            Style::default().fg(state_color_with_palette(agent.display_state, palette)),
            selected,
            palette,
        ),
        Row::Agent(agent) => {
            let title = if agent.visible && agent.state_source != StateSource::Screen {
                Style::default()
                    .fg(palette.foreground)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(palette.foreground)
            };
            icon_title_line_with_palette(
                glyph(agent.display_state),
                agent_topic(agent),
                width,
                Style::default().fg(state_color_with_palette(agent.display_state, palette)),
                title,
                selected,
                palette,
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
            aligned_line_with_palette(
                format!("   {context}"),
                format!("{} · {}", agent_status(agent), agent_kind_name(agent.kind)),
                width,
                if selected {
                    Style::default().fg(palette.foreground)
                } else {
                    muted_style_with_palette(palette)
                },
                Style::default().fg(state_color_with_palette(agent.display_state, palette)),
                selected,
                palette,
            )
        }
        Row::Conversation(_, conversation) => {
            let status = if conversation.display_state == DisplayState::Blocked {
                reason_name(conversation.reason_category.as_deref().unwrap_or("blocked"))
            } else {
                state_name(conversation.display_state)
            };
            aligned_line_with_palette(
                format!(
                    "   {} {}",
                    glyph(conversation.display_state),
                    conversation.label
                ),
                status.to_owned(),
                width,
                if conversation.active {
                    Style::default()
                        .fg(state_color_with_palette(
                            conversation.display_state,
                            palette,
                        ))
                        .add_modifier(Modifier::BOLD)
                } else {
                    Style::default().fg(state_color_with_palette(
                        conversation.display_state,
                        palette,
                    ))
                },
                Style::default().fg(state_color_with_palette(
                    conversation.display_state,
                    palette,
                )),
                selected,
                palette,
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
                muted_style_with_palette(palette),
            ))
        }
        Row::Spacer => Line::default(),
    }
}

fn row_agent(row: &Row) -> Option<&AgentSnapshot> {
    match row {
        Row::Agent(agent)
        | Row::AgentSub(agent, _)
        | Row::Team(agent, _)
        | Row::TeamSub(agent, _, _)
        | Row::TeamWorker(agent, _)
        | Row::TeamWorkerSub(agent, _)
        | Row::Conversation(agent, _) => Some(agent),
        _ => None,
    }
}

fn row_agent_mut(row: &mut Row) -> Option<&mut AgentSnapshot> {
    match row {
        Row::Agent(agent)
        | Row::AgentSub(agent, _)
        | Row::Team(agent, _)
        | Row::TeamSub(agent, _, _)
        | Row::TeamWorker(agent, _)
        | Row::TeamWorkerSub(agent, _)
        | Row::Conversation(agent, _) => Some(agent),
        _ => None,
    }
}

fn render_sidebar_row_with_palette(
    row: &Row,
    selected: bool,
    width: u16,
    focused: &FocusWindows,
    palette: SidebarPalette,
    branches: &HashMap<String, String>,
) -> Line<'static> {
    let assistant = row_agent(row)
        .and_then(|agent| focused.get(&agent.target.window_id))
        .is_some_and(|window| window.assistant);
    if !assistant {
        return render_row_with_branches(row, selected, width, palette, Some(branches));
    }

    let mut row = row.clone();
    if let Some(agent) = row_agent_mut(&mut row) {
        agent.thread_name = Some(crate::ui_language::text("Personal assistant", "个人助理").into());
    }
    render_row_with_branches(&row, selected, width, palette, Some(branches))
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

fn button_style_with_palette(
    button: Action,
    hovered: Option<&Action>,
    palette: SidebarPalette,
) -> Style {
    if hovered == Some(&button) {
        Style::default()
            .fg(palette.header_hover_foreground)
            .bg(palette.hover)
    } else {
        muted_style_with_palette(palette)
    }
}

fn muted_style_with_palette(palette: SidebarPalette) -> Style {
    // Use the theme's explicit muted token; terminal DIM varies by palette and
    // makes small labels difficult to read in light mode.
    Style::default().fg(palette.muted)
}

#[derive(Clone)]
struct HeaderPart {
    text: String,
    style: Style,
    action: Option<Action>,
}

impl HeaderPart {
    fn width(&self) -> usize {
        Span::raw(&self.text).width()
    }

    fn background(&self, fallback: Color) -> Color {
        self.style.bg.unwrap_or(fallback)
    }
}

#[derive(Clone, Copy)]
enum HeaderSeparator {
    PowerlineForward,
    PowerlineReverse,
    ThinMuted,
}

struct HeaderSection {
    items: Vec<HeaderPart>,
    separator: HeaderSeparator,
    spaced: bool,
}

impl HeaderSection {
    fn parts(&self, palette: SidebarPalette) -> Vec<HeaderPart> {
        join_header_items(&self.items, self.separator, self.spaced, palette)
    }
}

/// One configured part sequence drives rendering, width selection and hit regions.
struct HeaderLayout {
    parts: Vec<HeaderPart>,
}

impl HeaderLayout {
    fn new(
        width: u16,
        view: SidebarView,
        sort: AgentSort,
        total: usize,
        shown: usize,
        counts: [usize; 4],
        state_filter: AgentStateFilter,
        hovered: Option<&Action>,
        palette: SidebarPalette,
    ) -> Self {
        let (label, view_icon) = match view {
            SidebarView::Agents => (crate::ui_language::text("Agents", "智能体"), "󰚩"),
            SidebarView::Sessions => (crate::ui_language::text("Sessions", "会话"), ""),
        };
        let sort_icon = match sort {
            AgentSort::Grouped => "",
            AgentSort::Prioritized => "",
        };
        let count = if shown < total {
            format!("{shown}/{total}")
        } else {
            total.to_string()
        };
        let view_candidates = [
            format!(" {view_icon} {label}"),
            format!(" {view_icon}"),
            view_icon.into(),
        ];
        let pinned_candidates = [
            format!(" {count}"),
            format!("{shown}"),
            "".into(),
            String::new(),
        ];
        let wants_states = view == SidebarView::Agents;
        let try_layout = |show_states| {
            let right = header_right_parts(
                show_states,
                sort_icon,
                counts,
                state_filter,
                hovered,
                palette,
            );
            let right_width = header_parts_width(&right);
            for view_text in &view_candidates {
                for pinned_text in &pinned_candidates {
                    let left = header_left_parts(view_text, pinned_text, hovered, palette);
                    if header_parts_width(&left) + right_width < usize::from(width) {
                        return Some((left, right));
                    }
                }
            }
            None
        };

        let (left, right) = if wants_states && let Some((left, right)) = try_layout(true) {
            (left, right)
        } else if let Some((left, right)) = try_layout(false) {
            (left, right)
        } else {
            let left = header_left_parts(view_icon, "", hovered, palette);
            let right =
                header_right_parts(false, sort_icon, counts, state_filter, hovered, palette);
            (left, right)
        };
        let used_width = header_parts_width(&left) + header_parts_width(&right);
        let gap = usize::from(width).saturating_sub(used_width);
        let mut parts = left;
        if gap > 0 {
            parts.push(header_spacer(gap, palette.background));
        }
        parts.extend(right);
        Self { parts }
    }

    fn regions(&self, area: Rect) -> Vec<(Rect, Action)> {
        if area.height == 0 {
            return Vec::new();
        }
        let width = usize::from(area.width);
        let mut cursor = 0usize;
        let mut regions = Vec::new();
        for part in &self.parts {
            let part_width = part.width();
            if let Some(action) = &part.action {
                let start = cursor.min(width);
                let end = cursor.saturating_add(part_width).min(width);
                if start < end {
                    regions.push((
                        Rect::new(area.x + start as u16, area.y, (end - start) as u16, 1),
                        action.clone(),
                    ));
                }
            }
            cursor = cursor.saturating_add(part_width);
        }
        regions
    }
}

fn header_parts_width(parts: &[HeaderPart]) -> usize {
    parts.iter().map(HeaderPart::width).sum()
}

fn header_spacer(width: usize, background: Color) -> HeaderPart {
    HeaderPart {
        text: " ".repeat(width),
        style: Style::default().bg(background),
        action: None,
    }
}

fn header_button(text: &str, action: Action, foreground: Color, background: Color) -> HeaderPart {
    HeaderPart {
        text: text.into(),
        style: Style::default().fg(foreground).bg(background),
        action: Some(action),
    }
}

fn header_separator_part(
    separator: HeaderSeparator,
    left: Color,
    right: Color,
    palette: SidebarPalette,
) -> HeaderPart {
    let (text, foreground, background) = match separator {
        HeaderSeparator::PowerlineForward => ("", left, right),
        HeaderSeparator::PowerlineReverse => ("", right, left),
        HeaderSeparator::ThinMuted => ("", palette.muted, left),
    };
    HeaderPart {
        text: text.into(),
        style: Style::default().fg(foreground).bg(background),
        action: None,
    }
}

fn append_header_separator(
    parts: &mut Vec<HeaderPart>,
    separator: HeaderSeparator,
    left: Color,
    right: Color,
    spaced: bool,
    palette: SidebarPalette,
) {
    if spaced {
        parts.push(header_spacer(1, left));
    }
    parts.push(header_separator_part(separator, left, right, palette));
    if spaced {
        parts.push(header_spacer(1, right));
    }
}

fn join_header_items(
    items: &[HeaderPart],
    separator: HeaderSeparator,
    spaced: bool,
    palette: SidebarPalette,
) -> Vec<HeaderPart> {
    let mut parts = Vec::new();
    let mut previous_background = None;
    for item in items {
        let background = item.background(palette.background);
        if let Some(previous) = previous_background {
            append_header_separator(&mut parts, separator, previous, background, spaced, palette);
        }
        parts.push(item.clone());
        previous_background = Some(background);
    }
    parts
}

fn join_header_sections(
    sections: &[HeaderSection],
    separator: HeaderSeparator,
    spaced: bool,
    palette: SidebarPalette,
) -> Vec<HeaderPart> {
    let mut parts = Vec::new();
    let mut previous_background = None;
    for section in sections {
        let section_parts = section.parts(palette);
        let Some(first) = section_parts.first() else {
            continue;
        };
        let first_background = first.background(palette.background);
        if let Some(previous) = previous_background {
            append_header_separator(
                &mut parts,
                separator,
                previous,
                first_background,
                spaced,
                palette,
            );
        }
        previous_background = section_parts
            .last()
            .map(|part| part.background(palette.background));
        parts.extend(section_parts);
    }
    parts
}

fn header_left_parts(
    view_text: &str,
    pinned_text: &str,
    hovered: Option<&Action>,
    palette: SidebarPalette,
) -> Vec<HeaderPart> {
    let view_background = if hovered == Some(&Action::View) {
        palette.hover
    } else {
        palette.raised
    };
    let view_foreground = if hovered == Some(&Action::View) {
        palette.header_hover_foreground
    } else {
        palette.header_foreground
    };
    let pinned_background = if hovered == Some(&Action::Pinned) {
        palette.hover
    } else {
        palette.selection
    };
    let pinned_foreground = if hovered == Some(&Action::Pinned) {
        palette.header_hover_foreground
    } else {
        palette.header_foreground
    };
    let mut items = vec![header_button(
        view_text,
        Action::View,
        view_foreground,
        view_background,
    )];
    items[0].style = items[0].style.add_modifier(Modifier::BOLD);
    if !pinned_text.is_empty() {
        let mut pinned = header_button(
            pinned_text,
            Action::Pinned,
            pinned_foreground,
            pinned_background,
        );
        pinned.style = pinned.style.add_modifier(Modifier::BOLD);
        items.push(pinned);
    }
    let mut parts = HeaderSection {
        items,
        separator: HeaderSeparator::PowerlineForward,
        spaced: true,
    }
    .parts(palette);
    let background = parts
        .last()
        .map(|part| part.background(palette.background))
        .unwrap_or(palette.background);
    parts.push(header_spacer(1, background));
    parts.push(header_separator_part(
        HeaderSeparator::PowerlineForward,
        background,
        palette.background,
        palette,
    ));
    parts
}

fn header_right_parts(
    show_states: bool,
    sort_icon: &str,
    counts: [usize; 4],
    state_filter: AgentStateFilter,
    hovered: Option<&Action>,
    palette: SidebarPalette,
) -> Vec<HeaderPart> {
    let sorting = if hovered == Some(&Action::Sort) {
        palette.hover
    } else {
        palette.raised
    };
    let menu = if hovered == Some(&Action::Menu) {
        palette.hover
    } else {
        palette.selection
    };
    let sort_foreground = if hovered == Some(&Action::Sort) {
        palette.header_hover_foreground
    } else {
        palette.header_foreground
    };
    let menu_foreground = if hovered == Some(&Action::Menu) {
        palette.header_hover_foreground
    } else {
        palette.header_foreground
    };
    let controls = HeaderSection {
        items: vec![
            header_button(sort_icon, Action::Sort, sort_foreground, sorting),
            header_button("", Action::Menu, menu_foreground, menu),
        ],
        separator: HeaderSeparator::ThinMuted,
        spaced: true,
    };
    let mut sections = Vec::new();
    if show_states {
        let states_background = palette.selection;
        let items = FILTERED_AGENT_STATES
            .iter()
            .enumerate()
            .map(|(index, state)| {
                let action = Action::Filter(*state);
                let background = if hovered == Some(&action) {
                    palette.hover
                } else {
                    states_background
                };
                let foreground = if hovered == Some(&action) {
                    palette.header_hover_foreground
                } else {
                    state_color_with_palette(*state, palette)
                };
                HeaderPart {
                    text: counts[index].to_string(),
                    style: Style::default().fg(foreground).bg(background).add_modifier(
                        if state_filter.enabled(*state) {
                            Modifier::BOLD
                        } else {
                            Modifier::DIM
                        },
                    ),
                    action: Some(action),
                }
            })
            .collect();
        sections.push(HeaderSection {
            items,
            separator: HeaderSeparator::ThinMuted,
            spaced: true,
        });
    }
    sections.push(controls);
    let content = join_header_sections(&sections, HeaderSeparator::PowerlineReverse, true, palette);
    let first_background = content
        .first()
        .map(|part| part.background(palette.background))
        .unwrap_or(palette.background);
    let mut parts = vec![header_separator_part(
        HeaderSeparator::PowerlineReverse,
        palette.background,
        first_background,
        palette,
    )];
    parts.push(header_spacer(1, first_background));
    parts.extend(content);
    parts
}

#[cfg(test)]
fn render_header(
    width: u16,
    view: SidebarView,
    sort: AgentSort,
    total: usize,
    shown: usize,
    hovered: Option<&Action>,
) -> Line<'static> {
    render_header_with_palette(
        width,
        view,
        sort,
        total,
        shown,
        [0; 4],
        AgentStateFilter::default(),
        hovered,
        SidebarPalette::default(),
    )
}

fn render_header_with_palette(
    width: u16,
    view: SidebarView,
    sort: AgentSort,
    total: usize,
    shown: usize,
    counts: [usize; 4],
    state_filter: AgentStateFilter,
    hovered: Option<&Action>,
    palette: SidebarPalette,
) -> Line<'static> {
    let layout = HeaderLayout::new(
        width,
        view,
        sort,
        total,
        shown,
        counts,
        state_filter,
        hovered,
        palette,
    );
    Line::from(
        layout
            .parts
            .into_iter()
            .map(|part| Span::styled(part.text, part.style))
            .collect::<Vec<_>>(),
    )
}

fn render_close_with_palette(
    width: u16,
    hovered: Option<&Action>,
    palette: SidebarPalette,
) -> Line<'static> {
    let close = crate::ui_language::text("× close", "× 关闭");
    let gap = usize::from(width).saturating_sub(Span::raw(close).width());
    Line::from(vec![
        Span::raw(" ".repeat(gap)),
        Span::styled(
            close,
            button_style_with_palette(Action::Close, hovered, palette),
        ),
    ])
}

fn action_regions(
    area: Rect,
    view: SidebarView,
    agent_sort: AgentSort,
    is_popup: bool,
    counts: [usize; 4],
    focus_cards: &[PinnedCard],
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
        HeaderLayout::new(
            area.width,
            view,
            agent_sort,
            total_focus,
            focus_cards.len(),
            counts,
            AgentStateFilter::default(),
            None,
            SidebarPalette::default(),
        )
        .regions(area),
    );
    for (index, card) in focus_cards.iter().enumerate() {
        let y = 1 + 2 * index as u16;
        if y < area.height {
            actions.push((
                Rect::new(0, y, area.width, 2.min(area.height - y)),
                card.action(),
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
        crate::ui_language::text(
            "-Tab · switch Agents / Sessions",
            "-Tab · 切换智能体 / 会话",
        ),
        crate::ui_language::text("-p · pin / unpin item", "-p · 置顶 / 取消置顶"),
        crate::ui_language::text("-1–9 · open a pinned item", "-1–9 · 打开置顶项"),
        crate::ui_language::text(
            "-click blocked/done/working/idle counts · filter Agents",
            "-点击阻塞/完成/进行中/空闲数字 · 筛选智能体",
        ),
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
        .filter(|name| meaningful_topic(name))
    {
        return name;
    }
    // Older daemons stored hook thread names only in label.
    if agent.hook_session_id.is_some() && meaningful_topic(&agent.label) {
        return agent.label.trim();
    }
    let topic = agent.target.window_name.trim();
    if meaningful_topic(topic) {
        return topic;
    }
    let session = agent.target.session_name.trim();
    if !session.is_empty() {
        return session;
    }
    crate::ui_language::text("Blank session", "空白会话")
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

fn git_branch(path: &std::path::Path) -> Option<String> {
    git_branch_from(path, std::env::var_os("GIT_DIR").as_deref())
}

fn git_branch_from(path: &std::path::Path, override_dir: Option<&OsStr>) -> Option<String> {
    let path = path.canonicalize().ok()?;
    if !path.is_dir() {
        return None;
    }
    let path = path.as_path();
    fn small_file(path: &std::path::Path) -> Option<String> {
        use std::io::Read;
        let mut file = std::fs::File::open(path).ok()?.take(4097);
        let mut text = String::new();
        file.read_to_string(&mut text).ok()?;
        (text.len() <= 4096).then_some(text)
    }

    let git_dir = if let Some(directory) = override_dir {
        let directory = std::path::PathBuf::from(directory);
        if directory.as_os_str().is_empty() {
            return None;
        }
        if directory.is_absolute() {
            directory
        } else {
            path.join(directory)
        }
    } else {
        let mut found = None;
        for ancestor in path.ancestors() {
            let marker = ancestor.join(".git");
            if marker.is_dir() {
                found = Some(marker);
                break;
            }
            if marker.is_file() {
                let text = small_file(&marker)?;
                let directory = text.trim().strip_prefix("gitdir:")?.trim();
                if directory.is_empty() {
                    return None;
                }
                let directory = std::path::Path::new(directory);
                found = Some(if directory.is_absolute() {
                    directory.to_owned()
                } else {
                    ancestor.join(directory)
                });
                break;
            }
            // Git also accepts a bare repository as the working path.
            if ancestor.join("HEAD").is_file()
                && ancestor.join("objects").is_dir()
                && ancestor.join("refs").is_dir()
            {
                found = Some(ancestor.to_owned());
                break;
            }
        }
        found?
    };
    let head = small_file(&git_dir.join("HEAD"))?;
    let branch = head
        .trim()
        .strip_prefix("ref:")?
        .trim()
        .strip_prefix("refs/heads/")?;
    (!branch.is_empty() && !branch.chars().any(char::is_control)).then(|| branch.to_owned())
}

fn session_context(session: &SessionSnapshot, branch: Option<&str>) -> String {
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
        if let Some(branch) = branch {
            if !branch.is_empty()
                && !branch.eq_ignore_ascii_case(&session.session_name)
                && !parts.iter().any(|part| part.eq_ignore_ascii_case(&branch))
            {
                parts.push(branch.to_owned());
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
    context_line_with_palette(text, width, selected, SidebarPalette::default())
}

fn context_line_with_palette(
    text: &str,
    width: u16,
    selected: bool,
    palette: SidebarPalette,
) -> Line<'static> {
    let style = Style::default().fg(if selected {
        palette.foreground
    } else {
        palette.muted
    });
    aligned_line_with_palette(
        format!("   {text}"),
        String::new(),
        width,
        style,
        style,
        selected,
        palette,
    )
}

fn aligned_line_with_palette(
    left: String,
    right: String,
    width: u16,
    mut left_style: Style,
    mut right_style: Style,
    selected: bool,
    palette: SidebarPalette,
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
            .bg(palette.selection);
        right_style = right_style
            .remove_modifier(Modifier::DIM)
            .bg(palette.selection);
        if left_style.fg == Some(palette.muted) {
            left_style = left_style.fg(palette.foreground);
        }
        if right_style.fg == Some(palette.muted) {
            right_style = right_style.fg(palette.foreground);
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
    icon_style: Style,
    title_style: Style,
    selected: bool,
) -> Line<'static> {
    icon_title_line_with_palette(
        icon,
        title,
        width,
        icon_style,
        title_style,
        selected,
        SidebarPalette::default(),
    )
}

fn icon_title_line_with_palette(
    icon: &str,
    title: &str,
    width: u16,
    mut icon_style: Style,
    mut title_style: Style,
    selected: bool,
    palette: SidebarPalette,
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
            .bg(palette.selection);
        title_style = title_style
            .remove_modifier(Modifier::DIM)
            .add_modifier(Modifier::BOLD)
            .bg(palette.selection);
    }
    Line::from(vec![
        Span::styled(prefix, icon_style),
        Span::styled(title, title_style),
        Span::styled(" ".repeat(gap), title_style),
    ])
}

pub(crate) fn state_color(state: DisplayState) -> Color {
    state_color_with_palette(state, SidebarPalette::default())
}

fn state_color_with_palette(state: DisplayState, palette: SidebarPalette) -> Color {
    match state {
        DisplayState::Blocked => palette.alert,
        DisplayState::Working => palette.warning,
        DisplayState::Done => palette.info,
        DisplayState::Idle => palette.accent,
        DisplayState::Unknown => palette.muted,
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

fn toggle_session_focus(
    server: &ServerIdentity,
    session: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    if !safe_target(session, '$') {
        return Ok(());
    }
    let target = shell_quote(session);
    let unset = format!("set-option -u -t {target} @workbench_session_focus");
    let set = format!("set-option -t {target} @workbench_session_focus 1");
    crate::tmux::Tmux::new(server.clone()).output_with_timeout(
        &[
            "if-shell",
            "-F",
            "-t",
            session,
            "#{==:#{@workbench_session_focus},1}",
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

fn activate_pinned(card: &PinnedCard) -> Result<bool, Box<dyn std::error::Error>> {
    match card {
        PinnedCard::Agent(card) => activate_focus(card),
        PinnedCard::Session(session) => activate(&[Row::Session(session.clone())], 0),
    }
}

fn show_pinned_menu(
    cards: &[PinnedCard],
    view: SidebarView,
    anchor: Option<(u16, u16)>,
) -> Result<bool, Box<dyn std::error::Error>> {
    let executable = std::env::current_exe()?;
    let source = source_pane().ok_or("sidebar source pane unavailable")?;
    let mut owned = Vec::new();
    for pinned in cards {
        if let PinnedCard::Session(session) = pinned {
            if !safe_target(&session.session_id, '$') {
                continue;
            }
            let label: String = session
                .session_name
                .chars()
                .filter(|ch| !ch.is_control())
                .collect();
            let label = format!(" {}", label.replace('#', "##"));
            let key = if owned.len() < 9 {
                (owned.len() + 1).to_string()
            } else {
                String::new()
            };
            let mut focus = format!(
                "{} focus --responsive --session {} --source-pane {}",
                shell_quote(&executable.to_string_lossy()),
                shell_quote(&session.session_id),
                source
            );
            if let Some(window) = session
                .last_active_window_id
                .as_ref()
                .filter(|id| safe_target(id, '@'))
            {
                focus.push_str(&format!(" --window {window}"));
            }
            if let Some(pane) = session
                .last_active_pane_id
                .as_ref()
                .filter(|id| safe_target(id, '%'))
            {
                focus.push_str(&format!(" --pane {pane}"));
            }
            owned.push((label, key, format!("run-shell {}", shell_quote(&focus))));
            continue;
        }
        let PinnedCard::Agent(card) = pinned else {
            unreachable!()
        };
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
        } else if let Some(agent) = &card.agent {
            agent_topic(agent)
        } else if meaningful_topic(&card.window.name) {
            &card.window.name
        } else if !card.window.session_name.trim().is_empty() {
            card.window.session_name.trim()
        } else {
            crate::ui_language::text("Blank session", "空白会话")
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
            match view {
                SidebarView::Agents => {
                    crate::ui_language::text("-No pinned agents", "-没有置顶的智能体")
                }
                SidebarView::Sessions => {
                    crate::ui_language::text("-No pinned sessions", "-没有置顶的会话")
                }
            }
            .into(),
            String::new(),
            String::new(),
        ));
    }
    let items: Vec<_> = owned
        .iter()
        .map(|(label, key, command)| (label.as_str(), key.as_str(), command.clone()))
        .collect();
    show_menu(
        match view {
            SidebarView::Agents => crate::ui_language::text("Pinned agents", "置顶的智能体"),
            SidebarView::Sessions => crate::ui_language::text("Pinned sessions", "置顶的会话"),
        },
        anchor,
        false,
        &items,
    )
}

fn show_pinned_card_menu(
    card: &PinnedCard,
    anchor: Option<(u16, u16)>,
) -> Result<bool, Box<dyn std::error::Error>> {
    match card {
        PinnedCard::Agent(card) => show_focus_menu(card, anchor),
        PinnedCard::Session(session) if safe_target(&session.session_id, '$') => show_menu(
            crate::ui_language::text("Pinned session", "置顶会话"),
            anchor,
            false,
            &[(
                crate::ui_language::text("Unpin session", "取消置顶会话"),
                "p",
                format!(
                    "set-option -u -t {} @workbench_session_focus",
                    shell_quote(&session.session_id)
                ),
            )],
        ),
        _ => Ok(false),
    }
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
    focused_sessions: &FocusSessions,
) -> Result<bool, Box<dyn std::error::Error>> {
    match rows.get(selected) {
        Some(Row::Session(session) | Row::SessionSub(session))
            if safe_target(&session.session_id, '$') =>
        {
            let pinned = focused_sessions.contains(&session.session_id);
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
                        if pinned {
                            crate::ui_language::text("Unpin session", "取消置顶会话")
                        } else {
                            crate::ui_language::text("Pin session", "置顶会话")
                        },
                        "p",
                        if pinned {
                            format!(
                                "set-option -u -t {} @workbench_session_focus",
                                shell_quote(&session.session_id)
                            )
                        } else {
                            format!(
                                "set-option -t {} @workbench_session_focus 1",
                                shell_quote(&session.session_id)
                            )
                        },
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

fn show_sidebar_menu() -> Result<bool, Box<dyn std::error::Error>> {
    let sidebar_pane = source_pane().ok_or("sidebar source pane unavailable")?;
    if !safe_target(&sidebar_pane, '%') {
        return Err("invalid sidebar pane id".into());
    }
    let client = current_client()?;
    run_workbench(
        &[
            "status-menu",
            "sidebar",
            "--pane",
            &sidebar_pane,
            "--client",
            &client,
        ],
        true,
    )
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
        crate::command::new("tmux")
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
    let output = crate::command::new("tmux")
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
    command_args.extend(args);
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
        let viewport = SidebarViewport::new(10, Some("stale · reconnecting"), 0);
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
        // Eight lines include the header, a banner, divider and rows;
        // the popup footer follows and must not activate any hidden row.
        let viewport = SidebarViewport::new(8, Some("stale · reconnecting"), 0);
        assert_eq!(viewport.row_height, 5);
        assert_eq!(viewport.row_at(0, 7, 20), None);
        assert_eq!(viewport.row_at(1, 7, 20), None);
        assert_eq!(viewport.row_at(2, 7, 20), None);
        assert_eq!(viewport.row_at(3, 7, 20), Some(7));
        assert_eq!(viewport.row_at(4, 7, 20), Some(8));
        assert_eq!(viewport.row_at(5, 7, 20), Some(9));
        assert_eq!(viewport.row_at(6, 7, 20), Some(10));
        assert_eq!(viewport.row_at(7, 7, 20), Some(11));
        for row in [8, 9] {
            assert_eq!(viewport.row_at(row, 7, 20), None);
        }
    }

    #[test]
    fn banner_uses_space_even_when_rows_previously_fitted() {
        let normal = SidebarViewport::new(8, None, 0);
        assert_eq!(normal.row_at(7, 0, 6), Some(5));
        let stale = SidebarViewport::new(8, Some("stale · reconnecting"), 0);
        assert_eq!(stale.row_height, 5);
        assert_eq!(stale.row_at(7, 0, 6), Some(4));
        let overflow = SidebarViewport::new(6, None, 0);
        assert_eq!(overflow.row_at(2, 0, 10), Some(0));
        assert_eq!(overflow.row_at(5, 0, 10), Some(3));
        assert_eq!(overflow.row_at(6, 0, 10), None);
    }

    #[test]
    fn empty_disconnected_and_tiny_viewports_have_no_agent_targets() {
        for height in [0, 1] {
            let viewport = SidebarViewport::new(height, Some("stale · reconnecting"), 0);
            for row in 0..3 {
                assert_eq!(viewport.row_at(row, 0, 10), None);
            }
        }
        assert_eq!(SidebarViewport::new(10, None, 0).row_at(0, 0, 0), None);
        let disconnected = SidebarViewport::new(10, Some("disconnected"), 0);
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
                render_focus_card(&cards[0], false, 50)[0]
                    .to_string()
                    .contains("Personal assistant")
            );
            crate::ui_language::set_language(None);
            assert!(cards[1].agent.is_none());
            let [title, subtitle] = render_focus_card(&cards[1], false, 50);
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
        let cards: Vec<_> = build_focus_cards(&rows, &focused)
            .into_iter()
            .map(PinnedCard::Agent)
            .collect();
        let actions = action_regions(
            Rect::new(0, 0, 36, 16),
            SidebarView::Agents,
            AgentSort::Grouped,
            false,
            [0; 4],
            &cards,
            cards.len(),
        );
        let viewport = SidebarViewport::new(16, None, cards.len());
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
        assert_eq!(list.hit(1, 0, &actions), Some(Hit::Action(Action::View)));
        assert_eq!(list.hit(35, 0, &actions), Some(Hit::Action(Action::Menu)));
        assert_eq!(selected_window(&[Row::Agent(agent)], 0), Some("@9"));
        assert_eq!(visible_focus_cards(12, None, 3), 3);
        assert!(visible_focus_cards(6, None, 3) < 3);
    }

    #[test]
    fn pinned_header_and_sort_use_one_language_and_match_hit_regions() {
        for (language, agents, _sort) in [
            ("en", "Agents", "prioritized"),
            ("zh-CN", "智能体", "按优先级"),
            ("ja-JP", "Agents", "prioritized"),
        ] {
            crate::ui_language::set_language(Some(language));
            let heading =
                render_header(48, SidebarView::Agents, AgentSort::Prioritized, 2, 2, None)
                    .to_string();
            assert!(
                heading.contains(agents) && heading.contains(" 2"),
                "{heading}"
            );
            assert!(heading.contains('') && heading.contains(''), "{heading}");
            assert!(heading.contains("") && !heading.contains("menu"));
            assert!(!heading.contains("prioritized") && !heading.contains("按优先级"));
            for width in [18, 26, 32, 36, 48] {
                let layout = HeaderLayout::new(
                    width,
                    SidebarView::Agents,
                    AgentSort::Prioritized,
                    2,
                    2,
                    [1, 2, 3, 4],
                    AgentStateFilter::default(),
                    None,
                    SidebarPalette::default(),
                );
                let show_states = layout
                    .parts
                    .iter()
                    .any(|part| matches!(part.action.as_ref(), Some(Action::Filter(_))));
                let actions = action_regions(
                    Rect::new(0, 0, width, 10),
                    SidebarView::Agents,
                    AgentSort::Prioritized,
                    false,
                    [1, 2, 3, 4],
                    &[],
                    2,
                );
                let mut list = NavigationList::<Action>::default();
                list.replace_rows(vec![]);
                for expected in [
                    Action::View,
                    Action::Sort,
                    Action::Menu,
                    Action::Filter(DisplayState::Blocked),
                    Action::Filter(DisplayState::Done),
                    Action::Filter(DisplayState::Working),
                    Action::Filter(DisplayState::Idle),
                ] {
                    let area = actions
                        .iter()
                        .find(|(_, action)| *action == expected)
                        .map(|(area, _)| area);
                    if matches!(expected, Action::Filter(_)) && !show_states {
                        assert!(area.is_none());
                        continue;
                    }
                    let area = area.unwrap();
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
    fn status_counts_filter_agent_rows_and_dim_disabled_states() {
        let snapshot: Snapshot =
            serde_json::from_str(include_str!("../tests/golden/snapshot-v1.json")).unwrap();
        let counts = agent_state_counts(Some(&snapshot));
        assert_eq!(counts, [1, 0, 0, 0]);

        let all_rows = view_rows(
            &snapshot,
            false,
            AgentSort::Grouped,
            &TeamIndex::new(),
            &HashSet::new(),
            SidebarView::Agents,
            AgentStateFilter::default(),
        );
        assert!(all_rows.iter().any(|row| {
            row_agent(row).is_some_and(|agent| agent.display_state == DisplayState::Blocked)
        }));

        let mut filter = AgentStateFilter::default();
        filter.toggle(DisplayState::Blocked);
        let filtered_rows = view_rows(
            &snapshot,
            false,
            AgentSort::Grouped,
            &TeamIndex::new(),
            &HashSet::new(),
            SidebarView::Agents,
            filter,
        );
        assert!(!filtered_rows.iter().any(|row| {
            row_agent(row).is_some_and(|agent| agent.display_state == DisplayState::Blocked)
        }));
        filter.toggle(DisplayState::Blocked);
        let restored_rows = view_rows(
            &snapshot,
            false,
            AgentSort::Grouped,
            &TeamIndex::new(),
            &HashSet::new(),
            SidebarView::Agents,
            filter,
        );
        assert!(restored_rows.iter().any(|row| {
            row_agent(row).is_some_and(|agent| agent.display_state == DisplayState::Blocked)
        }));
        filter.toggle(DisplayState::Blocked);

        let heading = render_header_with_palette(
            48,
            SidebarView::Agents,
            AgentSort::Grouped,
            1,
            1,
            counts,
            AgentStateFilter::default(),
            None,
            SidebarPalette::default(),
        );
        assert!(heading.to_string().contains("1  0  0  0"));
        let blocked = heading
            .spans
            .iter()
            .find(|span| span.content == "1")
            .unwrap();
        assert_eq!(blocked.style.fg, Some(SidebarPalette::default().alert));

        let dimmed = render_header_with_palette(
            48,
            SidebarView::Agents,
            AgentSort::Grouped,
            1,
            1,
            counts,
            filter,
            None,
            SidebarPalette::default(),
        );
        let blocked = dimmed
            .spans
            .iter()
            .find(|span| span.content == "1")
            .unwrap();
        assert_eq!(blocked.style.fg, Some(SidebarPalette::default().alert));
        assert!(blocked.style.add_modifier.contains(Modifier::DIM));
    }

    #[test]
    fn hidden_agent_state_keeps_pinned_name_and_restores_list_rows() {
        let mut snapshot: Snapshot =
            serde_json::from_str(include_str!("../tests/golden/snapshot-v1.json")).unwrap();
        snapshot.agents[0].thread_name = Some("Pinned Codex task".into());
        let agent = snapshot.agents[0].clone();
        let window = FocusWindow {
            id: agent.target.window_id.clone(),
            session_id: agent.target.session_id.clone(),
            session_name: agent.target.session_name.clone(),
            name: agent.target.window_name.clone(),
            index: agent.target.window_index,
            pane: agent.target.pane_id.clone(),
            state: agent.display_state,
            assistant: false,
        };
        let focused = FocusWindows::from([(window.id.clone(), window)]);
        let teams = TeamIndex::new();
        let expanded = HashSet::new();
        let mut filter = AgentStateFilter::default();
        filter.toggle(DisplayState::Blocked);

        let filtered_rows = view_rows(
            &snapshot,
            false,
            AgentSort::Grouped,
            &teams,
            &expanded,
            SidebarView::Agents,
            filter,
        );
        assert!(!filtered_rows.iter().any(|row| {
            row_agent(row).is_some_and(|row_agent| row_agent.instance_id == agent.instance_id)
        }));

        let pinned = build_pinned_cards(
            SidebarView::Agents,
            false,
            AgentSort::Grouped,
            &teams,
            &expanded,
            &focused,
            &FocusSessions::new(),
            Some(&snapshot),
        );
        let [PinnedCard::Agent(card)] = pinned.as_slice() else {
            panic!("expected the focused agent to remain pinned");
        };
        assert_eq!(
            card.agent
                .as_ref()
                .map(|pinned_agent| pinned_agent.instance_id.as_str()),
            Some(agent.instance_id.as_str())
        );
        assert!(
            render_focus_card(card, false, 48)[0]
                .to_string()
                .contains("Pinned Codex task")
        );

        filter.toggle(DisplayState::Blocked);
        let restored_rows = view_rows(
            &snapshot,
            false,
            AgentSort::Grouped,
            &teams,
            &expanded,
            SidebarView::Agents,
            filter,
        );
        assert!(restored_rows.iter().any(|row| {
            row_agent(row).is_some_and(|row_agent| row_agent.instance_id == agent.instance_id)
        }));
    }

    #[test]
    fn sort_and_menu_icons_use_a_full_height_powerline_separator() {
        let palette = SidebarPalette::default();
        let header = render_header_with_palette(
            48,
            SidebarView::Agents,
            AgentSort::Grouped,
            1,
            1,
            [1, 0, 0, 0],
            AgentStateFilter::default(),
            None,
            palette,
        );
        let menu = header
            .spans
            .iter()
            .position(|span| span.content == "")
            .unwrap();
        let separator = &header.spans[menu - 2];
        assert_eq!(header.spans[menu - 1].content, " ");
        assert_eq!(header.spans[menu - 3].content, " ");
        assert_eq!(separator.content, "");
        assert_eq!(separator.style.fg, Some(palette.muted));
        assert_eq!(separator.style.bg, Some(palette.raised));
        assert!(header.to_string().contains("1  0  0  0"));
        assert!(header.to_string().contains("0  "));
        assert!(header.to_string().contains("  "));
    }

    #[test]
    fn header_hit_regions_stay_inside_narrow_terminal_widths() {
        for width in 0..=3 {
            let actions = action_regions(
                Rect::new(0, 0, width, 1),
                SidebarView::Agents,
                AgentSort::Grouped,
                false,
                [1, 2, 3, 4],
                &[],
                0,
            );
            assert!(
                actions
                    .iter()
                    .all(|(area, _)| area.width > 0 && area.x + area.width <= width)
            );
        }
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
        let actions = action_regions(
            Rect::new(0, 0, 40, 20),
            SidebarView::Agents,
            AgentSort::Grouped,
            true,
            [0; 4],
            &[],
            0,
        );
        assert_eq!(list.hit(0, 0, &actions), Some(Hit::Action(Action::View)));
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
        assert_eq!(session_context(&session, None), "1 agent");

        session.current_path = Some("/tmp/dotfiles".into());
        assert_eq!(session_context(&session, None), "dotfiles · 1 agent");
        assert_eq!(
            session_context(&session, Some("topic")),
            "dotfiles · topic · 1 agent"
        );
    }

    #[test]
    fn session_branch_reads_git_head_and_worktrees_without_a_process() {
        let root = tempfile::tempdir().unwrap();
        let repo = root.path().join("repo");
        let git = repo.join(".git");
        let nested = repo.join("src/deep");
        let worktree = root.path().join("worktree");
        let bare = root.path().join("bare");
        let alias = root.path().join("alias");
        std::fs::create_dir_all(&git).unwrap();
        std::fs::create_dir_all(&nested).unwrap();
        std::fs::create_dir_all(&worktree).unwrap();
        std::os::unix::fs::symlink(repo.join("src"), &alias).unwrap();
        std::fs::create_dir_all(bare.join("objects")).unwrap();
        std::fs::create_dir_all(bare.join("refs")).unwrap();
        std::fs::write(bare.join("HEAD"), "ref: refs/heads/bare-branch\n").unwrap();
        std::fs::write(git.join("HEAD"), "ref: refs/heads/feature/one\n").unwrap();
        std::fs::write(worktree.join(".git"), "gitdir: ../repo/.git\n").unwrap();
        assert_eq!(
            git_branch_from(&nested, None).as_deref(),
            Some("feature/one")
        );
        assert_eq!(
            git_branch_from(&alias.join("deep"), None).as_deref(),
            Some("feature/one")
        );
        assert_eq!(
            git_branch_from(&worktree, None).as_deref(),
            Some("feature/one")
        );
        assert_eq!(git_branch_from(&bare, None).as_deref(), Some("bare-branch"));

        std::fs::write(git.join("HEAD"), "ref: refs/heads/other\n").unwrap();
        assert_eq!(git_branch_from(&worktree, None).as_deref(), Some("other"));
        std::fs::write(
            git.join("HEAD"),
            "0000000000000000000000000000000000000000\n",
        )
        .unwrap();
        assert_eq!(git_branch_from(&nested, None), None);
        assert_eq!(git_branch_from(&worktree, Some(git.as_os_str())), None);
    }

    #[test]
    fn selected_card_uses_adaptive_background_across_both_rows() {
        let snapshot: Snapshot =
            serde_json::from_str(include_str!("../tests/golden/snapshot-v1.json")).unwrap();
        let agent = snapshot.agents[0].clone();
        let palette = SidebarPalette::default();
        for row in [
            Row::Agent(agent.clone()),
            Row::AgentSub(agent, AgentSort::Grouped),
        ] {
            let line = render_row(&row, true, 40);
            assert!(
                line.spans
                    .iter()
                    .all(|span| span.style.bg == Some(palette.selection))
            );
        }
        let session = snapshot.sessions[0].clone();
        let line = render_row(&Row::SessionSub(session), true, 40);
        assert!(line.spans.iter().all(|span| {
            span.style.bg == Some(palette.selection) && span.style.fg == Some(palette.foreground)
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
    fn unnamed_agent_cards_fall_back_to_the_tmux_session_name() {
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
                assert!(
                    text.contains(&agent.target.session_name),
                    "{window:?}: {text:?}"
                );
                assert!(!text.contains("Blank session"));
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
        assert_eq!(agent_topic(&agent), "文档体验优化方向");
    }

    #[test]
    fn card_titles_are_white_while_status_stays_on_icons_and_agent_status() {
        let snapshot: Snapshot =
            serde_json::from_str(include_str!("../tests/golden/snapshot-v1.json")).unwrap();
        let title_color = SidebarPalette::default().foreground;

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
                assert_eq!(span.style.fg, Some(SidebarPalette::default().muted));
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
