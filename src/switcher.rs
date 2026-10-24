//! Scoped, touch-friendly navigation. Inventory comes from tmux; Agent labels
//! and state use the same snapshot and presentation as the Sidebar.
use std::collections::{BTreeMap, HashSet};
use std::io::stdout;
use std::process::Command;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use clap::ValueEnum;
use crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

use crate::ipc::{Request, call};
use crate::model::{DisplayState, Snapshot, TmuxTarget};
use crate::navigation_list::{Hit, NavigationList, Screen};
use crate::paths::Paths;
use crate::server::ServerIdentity;
use crate::sidebar::{
    agent_kind_name, agent_status, agent_topic, context_line, glyph, icon_title_line, state_color,
};
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
    MenuFromMain,
    SidebarMenu,
    SidebarPane,
    SidebarPopup,
}

impl ReturnTo {
    pub(crate) fn name(self) -> &'static str {
        match self {
            Self::Menu => "menu",
            Self::MenuFromMain => "menu-from-main",
            Self::SidebarMenu => "sidebar-menu",
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Action {
    Back,
    Close,
    Search,
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
        let footer = if height >= 10 { 4 } else { 2 }.min(height.saturating_sub(header));
        let body = height.saturating_sub(header + footer);
        let card = body.clamp(1, 2);
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
        let top = self.height - self.footer + self.footer * index / 2;
        let bottom = self.height - self.footer + self.footer * (index + 1) / 2;
        Rect::new(0, top, self.width, bottom - top)
    }
}

#[derive(Default)]
struct List {
    entries: Vec<Entry>,
    visible: Vec<usize>,
    navigation: NavigationList<Action>,
    card: u16,
    query: String,
    searching: bool,
    error: Option<String>,
}

impl List {
    fn replace(&mut self, entries: Vec<Entry>) {
        self.entries = entries;
        self.filter();
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
        self.sync_rows();
    }

    fn sync_rows(&mut self) {
        self.navigation.replace_rows(
            self.visible
                .iter()
                .flat_map(|index| {
                    std::iter::repeat_n(
                        Some(self.entries[*index].key.clone()),
                        usize::from(self.card.max(1)),
                    )
                })
                .collect(),
        );
    }

    fn search(&mut self) {
        self.filter();
        self.navigation.scroll = 0;
        self.navigation.select(0);
        self.navigation.cancel_press();
    }

    fn layout(&mut self, geometry: Geometry) {
        if self.card != geometry.card {
            self.card = geometry.card;
            self.sync_rows();
        }
        self.navigation.set_viewport(Rect::new(
            0,
            geometry.header,
            geometry.width,
            (geometry.capacity as u16 * geometry.card).min(
                geometry
                    .height
                    .saturating_sub(geometry.header + geometry.footer),
            ),
        ));
        self.navigation.set_actions(&[
            (geometry.footer_rect(0), Action::Back),
            (geometry.footer_rect(1), Action::Close),
        ]);
    }

    fn actions(&self, geometry: Geometry) -> Vec<(Rect, Action)> {
        let mut actions: Vec<_> = [Action::Back, Action::Close]
            .into_iter()
            .enumerate()
            .map(|(index, action)| (geometry.footer_rect(index as u16), action))
            .collect();
        if geometry.header == 3 {
            actions.push((Rect::new(0, 2, geometry.width, 1), Action::Search));
        }
        actions
    }

    fn selected_entry(&self) -> Option<&Entry> {
        self.visible
            .get(self.navigation.selected / usize::from(self.card.max(1)))
            .map(|index| &self.entries[*index])
    }
}

fn draw(frame: &mut ratatui::Frame, kind: Kind, scope: &Scope, list: &List, geometry: Geometry) {
    let muted = Style::default().fg(Color::DarkGray);
    let end = list
        .navigation
        .visible_rows()
        .end
        .div_ceil(usize::from(geometry.card))
        .min(list.visible.len());
    let count = if list.visible.is_empty() {
        "0".into()
    } else {
        format!(
            "{}–{} / {}",
            list.navigation.scroll / usize::from(geometry.card) + 1,
            end,
            list.visible.len()
        )
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
    list.navigation.render(frame, |row, selected| {
        let entry = &list.entries[list.visible[row / usize::from(geometry.card)]];
        if row % usize::from(geometry.card) == 0 {
            let title_style = Style::default().fg(Color::Rgb(235, 235, 245));
            icon_title_line(
                glyph(entry.state),
                &entry.title,
                geometry.width,
                Style::default().fg(state_color(entry.state)),
                if entry.current {
                    title_style.add_modifier(Modifier::BOLD)
                } else {
                    title_style
                },
                selected,
            )
        } else {
            let detail = if entry.current {
                format!("{} · {}", tr("current", "当前"), entry.detail)
            } else {
                entry.detail.clone()
            };
            context_line(&detail, geometry.width, selected)
        }
    });
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
    for (index, (label, key, action)) in [
        (tr("← Back", "← 返回"), "BSpace", Action::Back),
        (tr("× close", "× 关闭"), "Escape", Action::Close),
    ]
    .iter()
    .enumerate()
    {
        let rect = geometry.footer_rect(index as u16);
        for row in 0..rect.height {
            frame.render_widget(
                Paragraph::new(crate::menu::action_line(
                    label,
                    key,
                    usize::from(row),
                    rect.width,
                    list.navigation.is_highlighted(&Hit::Action(*action)),
                )),
                Rect::new(rect.x, rect.y + row, rect.width, 1),
            );
        }
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

pub fn select(
    paths: &Paths,
    server: &ServerIdentity,
    kind: Kind,
    pane: &str,
    client: &str,
    key: &str,
) -> Result<TmuxTarget> {
    activate(
        paths,
        server,
        kind,
        &Scope::read(server, pane, client)?,
        key,
    )
}

fn native_picker(
    paths: &Paths,
    server: &ServerIdentity,
    kind: Kind,
    scope: &Scope,
    pane: &str,
    client: &str,
    return_to: ReturnTo,
    page: usize,
) -> Result<()> {
    use crate::menu::{Item, Menu, Pager, Role};
    use crate::status_menu::shell_quote as quote;
    let executable = quote(&std::env::current_exe()?.to_string_lossy());
    let base = format!(
        "{executable} switch {} --pane {} --client {} --language {} --return-to {}",
        kind.name(),
        quote(pane),
        quote(client),
        crate::ui_language::name(),
        return_to.name()
    );
    let mut items: Vec<_> = load(paths, server, kind, scope)?
        .into_iter()
        .enumerate()
        .map(|(index, entry)| Item {
            id: entry.key.clone(),
            label: format!(
                "{}{} · {}",
                if entry.current { "● " } else { "" },
                entry.title,
                entry.detail
            ),
            key: "123456789abcdefghijklmnopqrstuvwxyz"
                .chars()
                .nth(index)
                .map(|key| key.to_string())
                .unwrap_or_default(),
            command: format!(
                "run-shell -b {}",
                quote(&format!("{base} --select {}", quote(&entry.key)))
            ),
            enabled: true,
            role: Role::Body,
        })
        .collect();
    if items.is_empty() {
        items.push(Item {
            id: "empty".into(),
            label: tr("No entries", "没有可切换的项目").into(),
            key: String::new(),
            command: String::new(),
            enabled: false,
            role: Role::Body,
        });
    }
    let destination = match return_to {
        ReturnTo::SidebarPane => String::new(),
        ReturnTo::SidebarPopup => format!(
            "run-shell -b {}",
            quote(&format!(
                "{executable} sidebar --popup --pane {} --client {}",
                quote(pane),
                quote(client)
            ))
        ),
        _ => format!(
            "run-shell -b {}",
            quote(&format!(
                "{executable} status-menu {} --pane {} --client {} --language {}{}",
                if matches!(return_to, ReturnTo::SidebarMenu) {
                    "sidebar"
                } else {
                    "tmux"
                },
                quote(pane),
                quote(client),
                crate::ui_language::name(),
                if matches!(return_to, ReturnTo::MenuFromMain | ReturnTo::SidebarMenu) {
                    " --from-menu"
                } else {
                    ""
                }
            ))
        ),
    };
    items.push(Item {
        id: "back".into(),
        label: crate::menu_footer::back_label().into(),
        key: "BSpace".into(),
        command: destination,
        enabled: true,
        role: Role::Back,
    });
    crate::menu::display(
        Menu {
            title: format!("{} · {}", kind.title(), scope.label(kind)),
            items,
            pager: Some(Pager {
                before_page: base,
                after_page: String::new(),
            }),
        },
        pane,
        client,
        page,
    )
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
    let current = list
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
    list.layout(geometry);
    list.navigation.select(current * usize::from(geometry.card));
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
        list.layout(geometry);
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
            Event::Key(key) if key.kind == KeyEventKind::Press => {
                if (!list.searching || !matches!(key.code, KeyCode::Char('j' | 'k')))
                    && list.navigation.navigate(key.code)
                {
                    dirty = true;
                    continue;
                }
                match key.code {
                    KeyCode::Esc => return Ok(Outcome::Close),
                    KeyCode::Char('c' | 'd') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                        return Ok(Outcome::Close);
                    }
                    KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                        list.query.clear();
                        list.search();
                    }
                    KeyCode::Char('q') if !list.searching => return Ok(Outcome::Close),
                    KeyCode::Enter => {
                        action = list
                            .navigation
                            .focused_action()
                            .copied()
                            .map(Hit::Action)
                            .or_else(|| {
                                list.selected_entry()
                                    .map(|entry| Hit::Item(entry.key.clone()))
                            });
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
                }
            }
            Event::Paste(text) => {
                list.searching = true;
                list.query.push_str(&clean(&text));
                list.search();
            }
            Event::Mouse(mouse) => {
                let actions = list.actions(geometry);
                action = list.navigation.mouse(mouse, &actions);
            }
            Event::FocusLost => list.navigation.blur(),
            Event::FocusGained => list.navigation.selection_visible = true,
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
            Some(Hit::Action(Action::Back)) => return Ok(Outcome::Back),
            Some(Hit::Action(Action::Close)) => return Ok(Outcome::Close),
            Some(Hit::Action(Action::Search)) => list.searching = true,
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
    page: usize,
) -> Result<Option<TmuxTarget>> {
    crate::ui_language::set_language(language);
    let scope = Scope::read(server, pane, client)?;
    if !inside && crate::ui::Mode::current()? == crate::ui::Mode::Old {
        native_picker(paths, server, kind, &scope, pane, client, return_to, page)?;
        return Ok(None);
    }
    if !inside {
        crate::sidebar::popup(
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
            let mut destination = vec![
                "status-menu",
                if matches!(return_to, ReturnTo::SidebarMenu) {
                    "sidebar"
                } else {
                    "tmux"
                },
                "--pane",
                pane,
                "--client",
                client,
                "--language",
                crate::ui_language::name(),
            ];
            if matches!(return_to, ReturnTo::MenuFromMain | ReturnTo::SidebarMenu) {
                destination.push("--from-menu");
            }
            crate::sidebar::run_workbench(&destination, true)?;
            Ok(None)
        }
    }
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
            list.layout(geometry);
            for offset in 0..geometry.card {
                assert_eq!(
                    list.navigation.hit(
                        width - 1,
                        geometry.header + offset,
                        &list.actions(geometry)
                    ),
                    Some(Hit::Item("@1".into()))
                );
            }
            for index in 0..2 {
                let rect = geometry.footer_rect(index);
                for row in rect.y..rect.bottom() {
                    assert_eq!(
                        list.navigation.hit(rect.x, row, &list.actions(geometry)),
                        Some(Hit::Action([Action::Back, Action::Close][index as usize]))
                    );
                }
            }
            for row in 0..geometry.header {
                assert!(!matches!(
                    list.navigation.hit(1, row, &list.actions(geometry)),
                    Some(Hit::Item(_))
                ));
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
        list.layout(geometry);
        let pressed = list.navigation.hit(5, 3, &list.actions(geometry));
        list.replace(entries(Kind::Window, &scope, &panes[2..], None));
        assert_ne!(list.navigation.hit(5, 3, &list.actions(geometry)), pressed);
    }

    #[test]
    fn small_render_uses_sidebar_labels_and_preserves_both_footer_lines() {
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
            list.layout(geometry);
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
