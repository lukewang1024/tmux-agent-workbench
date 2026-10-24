//! Scoped, touch-friendly navigation. Inventory comes from tmux; Agent labels
//! and state use the same snapshot and presentation as the Sidebar.
use std::collections::{BTreeMap, HashSet};
use std::io::stdout;
use std::process::Command;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use clap::ValueEnum;
use crossterm::event::{
    self, DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste, EnableMouseCapture,
    Event, KeyCode, KeyEventKind, KeyModifiers, MouseButton, MouseEventKind,
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
use ratatui::widgets::{Block, Paragraph, Wrap};

use crate::ipc::{Request, call};
use crate::model::{DisplayState, Snapshot, TmuxTarget};
use crate::paths::Paths;
use crate::server::ServerIdentity;
use crate::sidebar::{agent_kind_name, agent_status, agent_topic, glyph, state_color};
use crate::tmux::{Pane, Tmux, TmuxSource};
use crate::ui_language::text as tr;

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum Kind {
    Window,
    Session,
    Agent,
    Pane,
}

#[derive(Debug, Clone, Copy, Default, ValueEnum)]
pub enum ReturnTo {
    #[default]
    Menu,
    SidebarPane,
    SidebarPopup,
}

impl ReturnTo {
    pub(crate) fn name(self) -> &'static str {
        match self {
            Self::Menu => "menu",
            Self::SidebarPane => "sidebar-pane",
            Self::SidebarPopup => "sidebar-popup",
        }
    }
}

impl Kind {
    fn name(self) -> &'static str {
        match self {
            Self::Window => "window",
            Self::Session => "session",
            Self::Agent => "agent",
            Self::Pane => "pane",
        }
    }

    fn title(self) -> &'static str {
        match self {
            Self::Window => tr("Switch windows", "切换窗口"),
            Self::Session => tr("Switch sessions", "切换会话"),
            Self::Agent => tr("Switch agents", "切换 Agent"),
            Self::Pane => tr("Switch panes", "切换窗格"),
        }
    }
}

#[derive(Clone)]
struct Scope {
    target: TmuxTarget,
}

impl Scope {
    fn read(server: &ServerIdentity, pane: &str, client: &str) -> Result<Self> {
        if !pane
            .strip_prefix('%')
            .is_some_and(|v| !v.is_empty() && v.bytes().all(|b| b.is_ascii_digit()))
        {
            return Err("invalid source pane".into());
        }
        let format = "#{session_id}\u{1f}#{session_name}\u{1f}#{window_id}\u{1f}#{window_index}\u{1f}#{window_name}\u{1f}#{pane_id}\u{1f}#{pane_index}";
        let output = Command::new("tmux")
            .args(server.tmux_args())
            .args(["display-message", "-p", "-t", pane, format])
            .output()?;
        let text = String::from_utf8(output.stdout)?;
        let fields: Vec<_> = text.trim_end_matches('\n').split('\u{1f}').collect();
        if !output.status.success() || fields.len() != 7 || fields[5] != pane {
            return Err("source pane closed".into());
        }
        let mut scope = Self {
            target: TmuxTarget {
                session_id: fields[0].into(),
                session_name: fields[1].into(),
                window_id: fields[2].into(),
                window_index: fields[3].parse()?,
                window_name: fields[4].into(),
                pane_id: pane.into(),
                pane_index: fields[6].parse()?,
            },
        };
        // Pane IDs are shared by linked windows. Resolve their Session and
        // window index in the invoking client's Session whenever linked there.
        let tmux = Tmux::new(server.clone());
        if let Some(session) = tmux.client_format(client, "#{session_id}")?
            && let Some(linked) = tmux.panes()?.into_iter().find(|candidate| {
                candidate.target.session_id == session
                    && candidate.target.window_id == scope.target.window_id
            })
        {
            scope.target.session_id = linked.target.session_id;
            scope.target.session_name = linked.target.session_name;
            scope.target.window_index = linked.target.window_index;
        }
        Ok(scope)
    }

    fn label(&self, kind: Kind) -> String {
        match kind {
            Kind::Window => self.target.session_name.clone(),
            Kind::Pane => format!("{} · {}", self.target.session_name, self.target.window_name),
            _ => tr("All sessions", "所有会话").into(),
        }
    }
}

#[derive(Debug, Clone)]
struct Entry {
    key: String,
    title: String,
    detail: String,
    target: TmuxTarget,
    state: DisplayState,
    current: bool,
}

fn clean(value: &str) -> String {
    value.chars().filter(|c| !c.is_control()).collect()
}

fn path_label(path: &str) -> &str {
    std::path::Path::new(path)
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or(path)
}

fn entries(kind: Kind, scope: &Scope, panes: &[Pane], snapshot: Option<&Snapshot>) -> Vec<Entry> {
    let live: HashSet<_> = panes
        .iter()
        .map(|pane| pane.target.pane_id.as_str())
        .collect();
    let agents: Vec<_> = snapshot
        .into_iter()
        .flat_map(|s| &s.agents)
        .filter(|agent| !agent.exited && live.contains(agent.target.pane_id.as_str()))
        .collect();
    let state = |group: &[&Pane]| {
        agents
            .iter()
            .filter(|agent| {
                group
                    .iter()
                    .any(|pane| pane.target.pane_id == agent.target.pane_id)
            })
            .map(|agent| agent.display_state)
            .min_by_key(|state| match state {
                DisplayState::Blocked => 0,
                DisplayState::Working => 1,
                DisplayState::Done => 2,
                DisplayState::Idle => 3,
                DisplayState::Unknown => 4,
            })
            .unwrap_or(DisplayState::Unknown)
    };
    let mut result = vec![];
    match kind {
        Kind::Window | Kind::Session => {
            let mut groups = BTreeMap::<String, Vec<&Pane>>::new();
            for pane in panes {
                if kind == Kind::Window && pane.target.session_id != scope.target.session_id {
                    continue;
                }
                let key = if kind == Kind::Session {
                    &pane.target.session_id
                } else {
                    &pane.target.window_id
                };
                groups.entry(key.clone()).or_default().push(pane);
            }
            for (key, group) in groups {
                let active = group
                    .iter()
                    .max_by_key(|pane| (pane.window_active, pane.pane_active))
                    .unwrap();
                let windows: HashSet<_> = group.iter().map(|pane| &pane.target.window_id).collect();
                let count = agents
                    .iter()
                    .filter(|agent| {
                        group
                            .iter()
                            .any(|pane| pane.target.pane_id == agent.target.pane_id)
                    })
                    .count();
                let detail = if kind == Kind::Session {
                    let agent_count = if snapshot.is_some() {
                        format!(" · {count} {}", tr("agents", "个 Agent"))
                    } else {
                        String::new()
                    };
                    format!(
                        "{} {}{} · {}",
                        windows.len(),
                        tr("windows", "个窗口"),
                        agent_count,
                        path_label(&active.current_path)
                    )
                } else {
                    format!(
                        "{} {} · {} · {}",
                        group.len(),
                        tr("panes", "个窗格"),
                        active.current_command,
                        path_label(&active.current_path)
                    )
                };
                result.push(Entry {
                    key,
                    title: if kind == Kind::Session {
                        active.target.session_name.clone()
                    } else {
                        format!(
                            "{}  {}",
                            active.target.window_index, active.target.window_name
                        )
                    },
                    detail,
                    target: active.target.clone(),
                    state: state(&group),
                    current: if kind == Kind::Session {
                        active.target.session_id == scope.target.session_id
                    } else {
                        active.target.window_id == scope.target.window_id
                    },
                });
            }
            if kind == Kind::Session {
                result.sort_by(|a, b| a.title.cmp(&b.title).then(a.key.cmp(&b.key)));
            } else {
                result.sort_by_key(|entry| entry.target.window_index);
            }
        }
        Kind::Pane => {
            for pane in panes.iter().filter(|pane| {
                pane.target.session_id == scope.target.session_id
                    && pane.target.window_id == scope.target.window_id
            }) {
                let agent = agents
                    .iter()
                    .find(|agent| agent.target.pane_id == pane.target.pane_id);
                let title = agent.map(|agent| agent_topic(agent)).unwrap_or_else(|| {
                    if pane.title.is_empty() {
                        &pane.current_command
                    } else {
                        &pane.title
                    }
                });
                result.push(Entry {
                    key: pane.target.pane_id.clone(),
                    title: format!("{}  {title}", pane.target.pane_index),
                    detail: format!("{} · {}", pane.current_command, pane.current_path),
                    target: pane.target.clone(),
                    state: state(&[pane]),
                    current: pane.target.pane_id == scope.target.pane_id,
                });
            }
            result.sort_by_key(|entry| entry.target.pane_index);
        }
        Kind::Agent => {
            for agent in agents {
                let pane = panes.iter().find(|pane| {
                    pane.target.pane_id == agent.target.pane_id
                        && pane.target.session_id == agent.target.session_id
                });
                let Some(pane) = pane else {
                    continue;
                };
                result.push(Entry {
                    key: agent.instance_id.clone(),
                    title: agent_topic(agent).into(),
                    detail: format!(
                        "{} · {} · {}:{}",
                        agent_status(agent),
                        agent_kind_name(agent.kind),
                        pane.target.session_name,
                        pane.target.window_index
                    ),
                    target: pane.target.clone(),
                    state: agent.display_state,
                    current: pane.target.pane_id == scope.target.pane_id,
                });
            }
            result.sort_by(|a, b| {
                (
                    &a.target.session_name,
                    a.target.window_index,
                    a.target.pane_index,
                )
                    .cmp(&(
                        &b.target.session_name,
                        b.target.window_index,
                        b.target.pane_index,
                    ))
            });
        }
    }
    for entry in &mut result {
        entry.title = clean(&entry.title);
        entry.detail = clean(&entry.detail);
    }
    result
}

fn load(paths: &Paths, server: &ServerIdentity, kind: Kind, scope: &Scope) -> Result<Vec<Entry>> {
    let panes = Tmux::new(server.clone()).panes()?;
    let snapshot = call(
        &paths.socket_for_server(&server.key),
        &Request::new("snapshot.get", serde_json::Value::Null),
        Duration::from_millis(500),
    )
    .ok()
    .and_then(|value| serde_json::from_value::<Snapshot>(value).ok());
    if kind == Kind::Agent && snapshot.is_none() {
        return Err(tr(
            "Agent status unavailable · retrying",
            "Agent 状态暂不可用 · 正在重试",
        )
        .into());
    }
    Ok(entries(kind, scope, &panes, snapshot.as_ref()))
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Hit {
    Item(String),
    Back,
    Previous,
    Next,
    Close,
    Search,
}

#[derive(Clone)]
struct Press {
    hit: Hit,
    row: u16,
    dragged: bool,
}

#[derive(Debug, Clone, Copy)]
struct Geometry {
    width: u16,
    height: u16,
    header: u16,
    footer: u16,
    card: u16,
    capacity: usize,
}

impl Geometry {
    fn new(width: u16, height: u16) -> Self {
        let header = if height >= 8 { 3 } else { 1 }.min(height);
        let footer = if height >= 6 { 2 } else { 1 }.min(height.saturating_sub(header));
        let body = height.saturating_sub(header + footer);
        let card = body.clamp(1, 3);
        Self {
            width,
            height,
            header,
            footer,
            card,
            capacity: usize::from(body / card).max(1),
        }
    }

    fn footer_rect(self, index: u16) -> Rect {
        let left = u32::from(self.width) * u32::from(index) / 4;
        let right = u32::from(self.width) * u32::from(index + 1) / 4;
        Rect::new(
            left as u16,
            self.height - self.footer,
            (right - left) as u16,
            self.footer,
        )
    }
}

#[derive(Default)]
struct List {
    entries: Vec<Entry>,
    visible: Vec<usize>,
    selected: usize,
    scroll: usize,
    query: String,
    searching: bool,
    pressed: Option<Press>,
    error: Option<String>,
}

impl List {
    fn replace(&mut self, entries: Vec<Entry>) {
        let key = self
            .visible
            .get(self.selected)
            .map(|index| self.entries[*index].key.clone());
        self.entries = entries;
        self.filter();
        self.selected = key
            .and_then(|key| {
                self.visible
                    .iter()
                    .position(|index| self.entries[*index].key == key)
            })
            .unwrap_or_else(|| self.selected.min(self.visible.len().saturating_sub(1)));
    }

    fn filter(&mut self) {
        let query = self.query.to_lowercase();
        self.visible = self
            .entries
            .iter()
            .enumerate()
            .filter(|(_, entry)| {
                format!("{} {}", entry.title, entry.detail)
                    .to_lowercase()
                    .contains(&query)
            })
            .map(|(index, _)| index)
            .collect();
        self.selected = self.selected.min(self.visible.len().saturating_sub(1));
        self.scroll = self.scroll.min(self.visible.len().saturating_sub(1));
    }

    fn search(&mut self) {
        self.filter();
        self.selected = 0;
        self.scroll = 0;
        self.pressed = None;
    }

    fn move_by(&mut self, delta: i32, capacity: usize) {
        self.selected = self
            .selected
            .saturating_add_signed(delta as isize)
            .min(self.visible.len().saturating_sub(1));
        if self.selected < self.scroll {
            self.scroll = self.selected;
        }
        if self.selected >= self.scroll + capacity {
            self.scroll = self.selected.saturating_sub(capacity - 1);
        }
    }

    fn page(&mut self, delta: i32, capacity: usize) {
        self.scroll = self
            .scroll
            .saturating_add_signed(delta as isize)
            .min(self.visible.len().saturating_sub(capacity));
        self.selected = self.scroll;
    }

    fn hit(&self, geometry: Geometry, column: u16, row: u16) -> Option<Hit> {
        if column >= geometry.width || row >= geometry.height {
            return None;
        }
        if row >= geometry.height - geometry.footer {
            return (0..4)
                .find(|index| {
                    let rect = geometry.footer_rect(*index);
                    column >= rect.x && column < rect.right()
                })
                .map(|index| {
                    [Hit::Back, Hit::Previous, Hit::Next, Hit::Close][index as usize].clone()
                });
        }
        if geometry.header == 3 && row == 2 {
            return Some(Hit::Search);
        }
        let offset = row.checked_sub(geometry.header)? / geometry.card;
        if usize::from(offset) >= geometry.capacity {
            return None;
        }
        self.visible
            .get(self.scroll + usize::from(offset))
            .map(|index| Hit::Item(self.entries[*index].key.clone()))
    }
}

struct Screen;
impl Screen {
    fn enter() -> Result<Self> {
        enable_raw_mode()?;
        let guard = Self;
        execute!(
            stdout(),
            EnterAlternateScreen,
            EnableMouseCapture,
            EnableBracketedPaste
        )?;
        Ok(guard)
    }
}
impl Drop for Screen {
    fn drop(&mut self) {
        let _ = disable_raw_mode();
        let _ = execute!(
            stdout(),
            LeaveAlternateScreen,
            DisableMouseCapture,
            DisableBracketedPaste,
            crossterm::cursor::Show
        );
    }
}

fn draw(frame: &mut ratatui::Frame, kind: Kind, scope: &Scope, list: &List, geometry: Geometry) {
    let muted = Style::default().fg(Color::DarkGray);
    let end = (list.scroll + geometry.capacity).min(list.visible.len());
    let count = if list.visible.is_empty() {
        "0".into()
    } else {
        format!("{}–{} / {}", list.scroll + 1, end, list.visible.len())
    };
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(
            format!(" {}   {count}", kind.title()),
            Style::default().add_modifier(Modifier::BOLD),
        ))),
        Rect::new(0, 0, geometry.width, 1),
    );
    if geometry.header == 3 {
        frame.render_widget(
            Paragraph::new(format!(
                " {}",
                clean(list.error.as_deref().unwrap_or(&scope.label(kind)))
            ))
            .style(muted),
            Rect::new(0, 1, geometry.width, 1),
        );
        let query = if list.query.is_empty() && !list.searching {
            tr("/ search · tap here or type", "/ 搜索 · 点击此处或输入").into()
        } else {
            format!("/ {}", list.query)
        };
        frame.render_widget(
            Paragraph::new(query).style(muted),
            Rect::new(0, 2, geometry.width, 1),
        );
        if list.searching {
            let column = (2 + Span::raw(&list.query).width())
                .min(usize::from(geometry.width.saturating_sub(1)));
            frame.set_cursor_position((column as u16, 2));
        }
    }
    for (offset, index) in list
        .visible
        .iter()
        .skip(list.scroll)
        .take(geometry.capacity)
        .enumerate()
    {
        let entry = &list.entries[*index];
        let row = geometry.header + offset as u16 * geometry.card;
        let height = geometry
            .card
            .min((geometry.height - geometry.footer).saturating_sub(row));
        if height == 0 {
            continue;
        }
        let selected = list.scroll + offset == list.selected;
        let style = if selected {
            Style::default().bg(Color::DarkGray)
        } else {
            Style::default()
        };
        frame.render_widget(
            Block::default().style(style),
            Rect::new(0, row, geometry.width, height),
        );
        let current = if entry.current {
            tr(" · current", " · 当前")
        } else {
            ""
        };
        let title = Line::from(vec![
            Span::styled(
                format!(" {} ", glyph(entry.state)),
                style.fg(state_color(entry.state)),
            ),
            Span::styled(
                format!("{}{current}", entry.title),
                style.add_modifier(Modifier::BOLD),
            ),
        ]);
        let title_height = if height >= 3 { 2 } else { 1 };
        frame.render_widget(
            Paragraph::new(title).wrap(Wrap { trim: false }),
            Rect::new(0, row, geometry.width, title_height),
        );
        if height >= 2 {
            frame.render_widget(
                Paragraph::new(format!("   {}", entry.detail)).style(if selected {
                    style
                } else {
                    muted
                }),
                Rect::new(0, row + height - 1, geometry.width, 1),
            );
        }
    }
    if list.visible.is_empty() {
        let empty = if !list.query.is_empty() {
            tr("No matching results", "没有匹配项")
        } else if list.error.is_some() {
            tr("Waiting for live status", "正在等待实时状态")
        } else if kind == Kind::Agent {
            tr("No running agents", "没有运行中的 Agent")
        } else {
            tr("No available targets", "没有可切换的目标")
        };
        frame.render_widget(
            Paragraph::new(empty).style(muted),
            Rect::new(
                1.min(geometry.width),
                geometry.header,
                geometry.width.saturating_sub(1),
                geometry
                    .height
                    .saturating_sub(geometry.header + geometry.footer),
            ),
        );
    }
    let labels = if geometry.width < 32 {
        [
            tr("‹back", "‹返回"),
            tr("↑prev", "↑上页"),
            tr("↓next", "↓下页"),
            tr("×close", "×关闭"),
        ]
    } else {
        [
            tr("‹ back", "‹ 返回"),
            tr("↑ prev", "↑ 上页"),
            tr("↓ next", "↓ 下页"),
            tr("× close", "× 关闭"),
        ]
    };
    for (index, label) in labels.iter().enumerate() {
        let rect = geometry.footer_rect(index as u16);
        let disabled =
            (index == 1 && list.scroll == 0) || (index == 2 && end == list.visible.len());
        let mut style = if disabled {
            muted
        } else {
            Style::default().add_modifier(Modifier::BOLD)
        };
        if list.pressed.as_ref().is_some_and(|press| {
            !press.dragged && press.hit == [Hit::Back, Hit::Previous, Hit::Next, Hit::Close][index]
        }) {
            style = style.bg(Color::DarkGray);
        }
        frame.render_widget(Block::default().style(style), rect);
        frame.render_widget(
            Paragraph::new(*label)
                .style(style)
                .alignment(Alignment::Center),
            Rect::new(
                rect.x,
                rect.y + u16::from(rect.height > 1),
                rect.width,
                1.min(rect.height),
            ),
        );
    }
}

fn activate(
    paths: &Paths,
    server: &ServerIdentity,
    kind: Kind,
    scope: &Scope,
    key: &str,
) -> Result<TmuxTarget> {
    let current = load(paths, server, kind, scope)?;
    let entry = current
        .into_iter()
        .find(|entry| entry.key == key)
        .ok_or(tr(
            "Target closed · choose another",
            "目标已关闭 · 请选择其他项",
        ))?;
    if kind == Kind::Agent {
        let context = crate::menu_context::Context::read(paths, &entry.target.pane_id)?;
        let snapshot = call(
            &paths.socket_for_server(&server.key),
            &Request::new("snapshot.get", serde_json::Value::Null),
            Duration::from_millis(500),
        )?;
        let snapshot: Snapshot = serde_json::from_value(snapshot)?;
        if !snapshot.agents.iter().any(|agent| {
            agent.instance_id == key
                && !agent.exited
                && context
                    .agent
                    .as_ref()
                    .is_some_and(|live| agent.process.as_ref() == Some(&live.fingerprint))
        }) {
            return Err(tr("Agent changed · choose again", "Agent 已变化 · 请重新选择").into());
        }
    }
    Ok(entry.target)
}

enum Outcome {
    Close,
    Back,
    Focus(TmuxTarget),
}

fn event_loop(
    paths: &Paths,
    server: &ServerIdentity,
    kind: Kind,
    scope: &Scope,
) -> Result<Outcome> {
    let mut list = List::default();
    match load(paths, server, kind, scope) {
        Ok(entries) => list.replace(entries),
        Err(error) => list.error = Some(error.to_string()),
    }
    list.selected = list
        .visible
        .iter()
        .position(|index| list.entries[*index].current)
        .unwrap_or(0);
    let (requests, receiver) = mpsc::sync_channel(1);
    let (sender, updates) = mpsc::sync_channel(1);
    let (background_paths, background_server, background_scope) =
        (paths.clone(), server.clone(), scope.clone());
    let language = crate::ui_language::name().to_owned();
    std::thread::spawn(move || {
        crate::ui_language::set_language(Some(&language));
        while receiver.recv().is_ok() {
            let result = load(
                &background_paths,
                &background_server,
                kind,
                &background_scope,
            )
            .map_err(|e| e.to_string());
            if sender.send(result).is_err() {
                break;
            }
        }
    });
    let _screen = Screen::enter()?;
    let mut terminal = Terminal::new(CrosstermBackend::new(stdout()))?;
    let size = terminal.size()?;
    let geometry = Geometry::new(size.width, size.height);
    list.move_by(0, geometry.capacity);
    let mut next_refresh = Instant::now() + Duration::from_secs(1);
    let mut dirty = true;
    loop {
        if let Ok(update) = updates.try_recv() {
            match update {
                Ok(entries) => {
                    list.replace(entries);
                    list.error = None;
                }
                Err(error) => list.error = Some(error),
            }
            dirty = true;
        }
        if Instant::now() >= next_refresh {
            let _ = requests.try_send(());
            next_refresh = Instant::now() + Duration::from_secs(1);
        }
        let size = terminal.size()?;
        let geometry = Geometry::new(size.width, size.height);
        list.scroll = list
            .scroll
            .min(list.visible.len().saturating_sub(geometry.capacity));
        if dirty {
            terminal.draw(|frame| draw(frame, kind, scope, &list, geometry))?;
            dirty = false;
        }
        if !event::poll(Duration::from_millis(50))? {
            continue;
        }
        let input = event::read()?;
        let mut action = None;
        match input {
            Event::Key(key) if key.kind == KeyEventKind::Press => match key.code {
                KeyCode::Esc => return Ok(Outcome::Close),
                KeyCode::Char('c' | 'd') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                    return Ok(Outcome::Close);
                }
                KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                    list.query.clear();
                    list.search();
                }
                KeyCode::Char('q') if !list.searching => return Ok(Outcome::Close),
                KeyCode::Down => list.move_by(1, geometry.capacity),
                KeyCode::Up => list.move_by(-1, geometry.capacity),
                KeyCode::Char('j') if !list.searching => list.move_by(1, geometry.capacity),
                KeyCode::Char('k') if !list.searching => list.move_by(-1, geometry.capacity),
                KeyCode::PageDown => list.page(geometry.capacity as i32, geometry.capacity),
                KeyCode::PageUp => list.page(-(geometry.capacity as i32), geometry.capacity),
                KeyCode::Enter => {
                    action = list
                        .visible
                        .get(list.selected)
                        .map(|index| Hit::Item(list.entries[*index].key.clone()))
                }
                KeyCode::Backspace if list.query.is_empty() && !list.searching => {
                    return Ok(Outcome::Back);
                }
                KeyCode::Backspace => {
                    list.query.pop();
                    list.search();
                }
                KeyCode::Char('/') if !list.searching => list.searching = true,
                KeyCode::Char(character)
                    if !key
                        .modifiers
                        .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
                {
                    list.searching = true;
                    list.query.push(character);
                    list.search();
                }
                _ => {}
            },
            Event::Paste(text) => {
                list.searching = true;
                list.query.push_str(&clean(&text));
                list.search();
            }
            Event::Mouse(mouse) => match mouse.kind {
                MouseEventKind::Down(MouseButton::Left) => {
                    if let Some(Hit::Item(key)) = list.hit(geometry, mouse.column, mouse.row) {
                        list.selected = list
                            .visible
                            .iter()
                            .position(|index| list.entries[*index].key == key)
                            .unwrap_or(list.selected);
                    }
                    list.pressed = list
                        .hit(geometry, mouse.column, mouse.row)
                        .map(|hit| Press {
                            hit,
                            row: mouse.row,
                            dragged: false,
                        })
                }
                MouseEventKind::Drag(MouseButton::Left) => {
                    if let Some(press) = &mut list.pressed {
                        press.dragged = true;
                        let delta = (i32::from(press.row) - i32::from(mouse.row))
                            / i32::from(geometry.card);
                        if delta != 0 {
                            press.row = mouse.row;
                            list.page(delta, geometry.capacity);
                        }
                    }
                }
                MouseEventKind::Up(MouseButton::Left) => {
                    if let Some(press) = list.pressed.take()
                        && !press.dragged
                        && list.hit(geometry, mouse.column, mouse.row).as_ref() == Some(&press.hit)
                    {
                        action = Some(press.hit);
                    }
                }
                MouseEventKind::ScrollDown => {
                    list.pressed = None;
                    list.page(1, geometry.capacity);
                }
                MouseEventKind::ScrollUp => {
                    list.pressed = None;
                    list.page(-1, geometry.capacity);
                }
                _ => {}
            },
            _ => {}
        }
        match action {
            Some(Hit::Item(key)) => match activate(paths, server, kind, scope, &key) {
                Ok(target) => return Ok(Outcome::Focus(target)),
                Err(error) => {
                    list.error = Some(clean(&error.to_string()));
                    let _ = requests.try_send(());
                }
            },
            Some(Hit::Back) => return Ok(Outcome::Back),
            Some(Hit::Close) => return Ok(Outcome::Close),
            Some(Hit::Previous) => list.page(-(geometry.capacity as i32), geometry.capacity),
            Some(Hit::Next) => list.page(geometry.capacity as i32, geometry.capacity),
            Some(Hit::Search) => list.searching = true,
            None => {}
        }
        dirty = true;
    }
}

#[allow(clippy::too_many_arguments)]
pub fn run(
    paths: &Paths,
    server: &ServerIdentity,
    kind: Kind,
    pane: &str,
    client: &str,
    inside: bool,
    language: Option<&str>,
    return_to: ReturnTo,
) -> Result<Option<TmuxTarget>> {
    crate::ui_language::set_language(language);
    let scope = Scope::read(server, pane, client)?;
    if !inside {
        popup(
            server,
            pane,
            client,
            &[
                "switch",
                kind.name(),
                "--inside",
                "--pane",
                pane,
                "--client",
                client,
                "--language",
                crate::ui_language::name(),
                "--return-to",
                return_to.name(),
            ],
        )?;
        return Ok(None);
    }
    match event_loop(paths, server, kind, &scope)? {
        Outcome::Close => Ok(None),
        Outcome::Focus(target) => Ok(Some(target)),
        Outcome::Back => {
            if matches!(return_to, ReturnTo::SidebarPane) {
                return Ok(None);
            }
            if matches!(return_to, ReturnTo::SidebarPopup) {
                crate::sidebar::run_workbench(
                    &["sidebar", "--popup", "--pane", pane, "--client", client],
                    true,
                )?;
                return Ok(None);
            }
            crate::sidebar::run_workbench(
                &[
                    "status-menu",
                    "tmux",
                    "--pane",
                    pane,
                    "--client",
                    client,
                    "--language",
                    crate::ui_language::name(),
                ],
                true,
            )?;
            Ok(None)
        }
    }
}

pub(crate) fn popup(
    server: &ServerIdentity,
    pane: &str,
    client: &str,
    args: &[&str],
) -> Result<()> {
    Scope::read(server, pane, client)?;
    if Tmux::new(server.clone())
        .client_format(client, "#{session_id}")?
        .is_none()
    {
        return Err("switching client disappeared".into());
    }
    let quote = |text: &str| format!("'{}'", text.replace('\'', "'\\''"));
    let command = format!(
        "WORKBENCH_POPUP=1 WORKBENCH_TARGET_CLIENT={} WORKBENCH_SOURCE_PANE={} TMUX_AGENT_WORKBENCH_TMUX_SOCKET={} {} {}",
        quote(client),
        quote(pane),
        quote(&server.socket_path.to_string_lossy()),
        quote(&std::env::current_exe()?.to_string_lossy()),
        args.iter()
            .map(|arg| quote(arg))
            .collect::<Vec<_>>()
            .join(" "),
    );
    let status = Command::new("tmux")
        .args(server.tmux_args())
        .args([
            "display-popup",
            "-B",
            "-E",
            "-c",
            client,
            "-t",
            pane,
            "-w",
            "100%",
            "-h",
            "100%",
            &command,
        ])
        .status()?;
    if !status.success() && !matches!(status.code(), Some(129 | 130)) {
        return Err("could not open navigation panel".into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::backend::TestBackend;

    fn pane(session: &str, window: &str, index: u32, id: &str, pane_index: u32) -> Pane {
        Pane {
            target: TmuxTarget {
                session_id: session.into(),
                session_name: session.into(),
                window_id: window.into(),
                window_index: index,
                window_name: format!("Window {index}"),
                pane_id: id.into(),
                pane_index,
            },
            root_pid: 1,
            title: format!("Pane {pane_index}"),
            current_command: "sh".into(),
            current_path: "/project".into(),
            role: None,
            visible: false,
            window_active: index == 0,
            pane_active: pane_index == 0,
            pane_last: false,
            session_visible: false,
            content_revision: "".into(),
        }
    }

    fn inventory() -> Vec<Pane> {
        vec![
            pane("$1", "@1", 0, "%1", 0),
            pane("$1", "@1", 0, "%2", 1),
            pane("$1", "@2", 1, "%3", 0),
            pane("$2", "@3", 0, "%4", 0),
        ]
    }

    #[test]
    fn lists_have_distinct_scopes_and_include_shell_sessions() {
        let panes = inventory();
        let scope = Scope {
            target: panes[0].target.clone(),
        };
        for (kind, expected) in [
            (Kind::Window, vec!["@1", "@2"]),
            (Kind::Pane, vec!["%1", "%2"]),
            (Kind::Session, vec!["$1", "$2"]),
        ] {
            let rows = entries(kind, &scope, &panes, None);
            assert_eq!(
                rows.iter().map(|row| row.key.as_str()).collect::<Vec<_>>(),
                expected
            );
            assert_eq!(rows.iter().filter(|row| row.current).count(), 1);
        }
    }

    #[test]
    fn agent_view_excludes_exited_agents_and_closed_panes() {
        let panes = inventory();
        let scope = Scope {
            target: panes[0].target.clone(),
        };
        let mut snapshot: Snapshot =
            serde_json::from_str(include_str!("../tests/golden/snapshot-v1.json")).unwrap();
        let mut agent = snapshot.agents[0].clone();
        agent.target = panes[2].target.clone();
        agent.instance_id = "live".into();
        agent.exited = false;
        let mut exited = agent.clone();
        exited.instance_id = "exited".into();
        exited.exited = true;
        let mut closed = agent.clone();
        closed.instance_id = "closed".into();
        closed.target.pane_id = "%999".into();
        snapshot.agents = vec![agent, exited, closed];
        assert_eq!(
            entries(Kind::Agent, &scope, &panes, Some(&snapshot))
                .iter()
                .map(|row| row.key.as_str())
                .collect::<Vec<_>>(),
            vec!["live"]
        );
    }

    #[test]
    fn all_card_lines_and_footer_bands_have_matching_hit_regions() {
        let panes = inventory();
        let scope = Scope {
            target: panes[0].target.clone(),
        };
        let mut list = List::default();
        list.replace(entries(Kind::Window, &scope, &panes, None));
        for (width, height) in [(40, 20), (32, 12), (24, 8)] {
            let geometry = Geometry::new(width, height);
            for offset in 0..geometry.card {
                assert_eq!(
                    list.hit(geometry, width - 1, geometry.header + offset),
                    Some(Hit::Item("@1".into()))
                );
            }
            for index in 0..4 {
                let rect = geometry.footer_rect(index);
                for row in rect.y..rect.bottom() {
                    assert_eq!(
                        list.hit(geometry, rect.x, row),
                        Some(
                            [Hit::Back, Hit::Previous, Hit::Next, Hit::Close][index as usize]
                                .clone()
                        )
                    );
                }
            }
            for row in 0..geometry.header {
                assert!(!matches!(list.hit(geometry, 1, row), Some(Hit::Item(_))));
            }
        }
    }

    #[test]
    fn held_row_identity_survives_refresh_without_retargeting() {
        let panes = inventory();
        let scope = Scope {
            target: panes[0].target.clone(),
        };
        let mut list = List::default();
        list.replace(entries(Kind::Window, &scope, &panes, None));
        let geometry = Geometry::new(40, 20);
        let pressed = list.hit(geometry, 5, 3);
        list.replace(entries(Kind::Window, &scope, &panes[2..], None));
        assert_ne!(list.hit(geometry, 5, 3), pressed);
    }

    #[test]
    fn small_render_wraps_titles_and_preserves_both_footer_lines() {
        let panes = inventory();
        let scope = Scope {
            target: panes[0].target.clone(),
        };
        let mut list = List::default();
        list.replace(entries(Kind::Window, &scope, &panes, None));
        list.entries[0].title = "标题非常长需要折行显示但不加省略号".into();
        for (width, height) in [(40, 20), (32, 12), (24, 8), (12, 4)] {
            let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
            let geometry = Geometry::new(width, height);
            terminal
                .draw(|frame| draw(frame, Kind::Window, &scope, &list, geometry))
                .unwrap();
            let buffer = terminal.backend().buffer();
            let rendered = buffer
                .content()
                .iter()
                .map(|cell| cell.symbol())
                .collect::<String>();
            assert!(!rendered.contains('…'));
            assert!(rendered.contains("back") || width < 24);
            assert!(rendered.contains("close") || width < 24);
        }
    }
}
