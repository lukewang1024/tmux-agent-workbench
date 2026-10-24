//! Compact action menus. Providers supply content and tmux callbacks; interaction
//! is shared with Sidebar through NavigationList. Callbacks run after closing
//! the popup, so commands may safely open another menu, prompt or application.
use crate::{
    navigation_list::{Hit, NavigationList, Screen},
    paths::Paths,
    server::ServerIdentity,
    ui_language::text as tr,
};
use crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use ratatui::{
    Terminal,
    backend::CrosstermBackend,
    layout::{Alignment, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Paragraph},
};
use serde::{Deserialize, Serialize};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::{
    fs,
    io::{self, Write, stdout},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) enum Role {
    Body,
    Back,
    Primary,
    Error,
    Close,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct Item {
    pub id: String,
    pub label: String,
    pub key: String,
    pub command: String,
    pub enabled: bool,
    pub role: Role,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct Menu {
    pub title: String,
    pub items: Vec<Item>,
}

#[derive(Serialize, Deserialize)]
struct Request {
    menu: Menu,
    language: String,
    page: usize,
}

struct Exchange {
    request: PathBuf,
    reply: PathBuf,
}
impl Drop for Exchange {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.request);
        let _ = fs::remove_file(&self.reply);
    }
}

pub(crate) fn display(menu: Menu, pane: &str, client: &str, page: usize) -> Result<()> {
    let paths = Paths::discover()?;
    let nonce = SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos();
    let request = paths
        .runtime_dir
        .join(format!("menu-{}-{nonce}.json", std::process::id()));
    let exchange = Exchange {
        reply: request.with_extension("reply"),
        request,
    };
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&exchange.request)?;
    serde_json::to_writer(
        &mut file,
        &Request {
            menu: menu.clone(),
            language: crate::ui_language::name().into(),
            page,
        },
    )?;
    file.flush()?;
    let width = menu
        .items
        .iter()
        .filter(|item| item.enabled && item.role == Role::Body || item.role == Role::Error)
        .map(|item| Span::raw(&item.label).width() + Span::raw(&item.key).width() + 5)
        .max()
        .unwrap_or(28)
        .clamp(28, 56) as u16;
    let height = menu
        .items
        .iter()
        .map(|item| match item.role {
            Role::Body => row_count(item, width),
            Role::Primary | Role::Error => 2,
            _ => 0,
        })
        .sum::<usize>()
        .saturating_add(3)
        .min(u16::MAX as usize) as u16;
    crate::popup::open(
        &ServerIdentity::discover()?,
        pane,
        client,
        &["menu-panel", &exchange.request.to_string_lossy()],
        crate::popup::Sizing::Menu { width, height },
    )?;
    let selected: Option<usize> = match fs::read(&exchange.reply) {
        Ok(bytes) => serde_json::from_slice(&bytes)?,
        Err(error) if error.kind() == io::ErrorKind::NotFound => None,
        Err(error) => return Err(error.into()),
    };
    if let Some(item) = selected
        .and_then(|index| menu.items.get(index))
        .filter(|item| item.enabled && !item.command.is_empty())
    {
        let mut callback = Command::new("tmux")
            .args(ServerIdentity::discover()?.tmux_args())
            .args(["source-file", "-t", pane, "-"])
            .stdin(Stdio::piped())
            .spawn()?;
        if let Some(mut input) = callback.stdin.take() {
            writeln!(input, "{}", item.command)?;
        }
        let status = callback.wait()?;
        if !status.success() {
            return Err("menu action failed".into());
        }
    }
    Ok(())
}

pub fn run_request(path: &Path) -> Result<()> {
    let paths = Paths::discover()?;
    let metadata = fs::symlink_metadata(path)?;
    if path.parent() != Some(paths.runtime_dir.as_path())
        || !metadata.is_file()
        || metadata.uid() != unsafe { libc::geteuid() }
        || metadata.mode() & 0o077 != 0
    {
        return Err("menu request must be a private runtime file".into());
    }
    let request: Request = serde_json::from_slice(&fs::read(path)?)?;
    crate::ui_language::set_language(Some(&request.language));
    let selected = interact(&request.menu, request.page)?;
    let mut reply = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path.with_extension("reply"))?;
    serde_json::to_writer(&mut reply, &selected)?;
    Ok(())
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Action {
    Choose(usize),
    Previous,
    Next,
    Close,
}

#[derive(Clone, Copy)]
struct Layout {
    width: u16,
    height: u16,
    header: u16,
    footer: u16,
    primary: u16,
    error: u16,
    body: Rect,
}
impl Layout {
    fn new(width: u16, height: u16, menu: &Menu) -> Self {
        let header = 1.min(height);
        let footer = (if height >= 4 { 2 } else { 1 }).min(height.saturating_sub(header));
        let mut available = height.saturating_sub(header + footer);
        let primary = if menu.items.iter().any(|item| item.role == Role::Primary) {
            available.saturating_sub(1).min(2)
        } else {
            0
        };
        available -= primary;
        let error = if menu.items.iter().any(|item| item.role == Role::Error) {
            available.saturating_sub(1).min(2)
        } else {
            0
        };
        available -= error;
        Self {
            width,
            height,
            header,
            footer,
            primary,
            error,
            body: Rect::new(0, header, width, available),
        }
    }

    fn actions(self, menu: &Menu, rows: usize) -> Vec<(Rect, Action)> {
        let back = menu
            .items
            .iter()
            .position(|item| item.role == Role::Back && item.enabled);
        let mut footer = vec![];
        if let Some(index) = back {
            footer.push(Action::Choose(index));
        }
        if rows > usize::from(self.body.height) {
            footer.extend([Action::Previous, Action::Next]);
        }
        footer.push(Action::Close);
        let count = footer.len() as u32;
        let mut actions: Vec<_> = footer
            .into_iter()
            .enumerate()
            .map(|(index, action)| {
                let left = u32::from(self.width) * index as u32 / count;
                let right = u32::from(self.width) * (index as u32 + 1) / count;
                (
                    Rect::new(
                        left as u16,
                        self.height - self.footer,
                        (right - left) as u16,
                        self.footer,
                    ),
                    action,
                )
            })
            .collect();
        if let Some(index) = menu
            .items
            .iter()
            .position(|item| item.role == Role::Primary && item.enabled)
        {
            actions.push((
                Rect::new(
                    0,
                    self.height - self.footer - self.primary,
                    self.width,
                    self.primary,
                ),
                Action::Choose(index),
            ));
        }
        actions
    }
}

fn clean(text: &str) -> String {
    text.chars()
        .map(|character| {
            if character.is_control() {
                ' '
            } else {
                character
            }
        })
        .collect()
}

fn split_text(text: &str, width: usize) -> (String, String) {
    let mut result = String::new();
    for character in text.chars() {
        result.push(character);
        if Span::raw(&result).width() > width {
            result.pop();
            break;
        }
    }
    let remainder = text[result.len()..].to_owned();
    (result, remainder)
}

fn row_count(item: &Item, width: u16) -> usize {
    if item.enabled {
        return 2;
    }
    let mut rest = clean(&item.label);
    let mut count = 0;
    loop {
        rest = split_text(&rest, usize::from(width.saturating_sub(2)).max(1)).1;
        count += 1;
        if rest.is_empty() || count == 4 {
            return count;
        }
    }
}

fn row_line(item: &Item, part: usize, width: u16, selected: bool) -> Line<'static> {
    let hint = if item.key.is_empty() {
        String::new()
    } else {
        format!(
            "({})",
            match item.key.as_str() {
                "Enter" => "enter",
                "BSpace" => "back",
                "Escape" => "esc",
                key => key,
            }
        )
    };
    let reserved = if item.enabled {
        Span::raw(&hint).width() + 1
    } else {
        0
    };
    let mut rest = clean(&item.label);
    let mut text = String::new();
    for line in 0..=part {
        let budget = usize::from(width).saturating_sub(2 + if line == 0 { reserved } else { 0 });
        (text, rest) = split_text(&rest, budget);
    }
    let style = if selected {
        Style::default()
            .fg(Color::White)
            .bg(Color::DarkGray)
            .add_modifier(Modifier::BOLD)
    } else if item.enabled {
        Style::default()
    } else {
        Style::default().fg(Color::DarkGray)
    };
    let hint = if part == 0 && item.enabled {
        hint
    } else {
        String::new()
    };
    let prefix = if width > 0 { " " } else { "" };
    let gap = usize::from(width)
        .saturating_sub(Span::raw(&text).width() + Span::raw(&hint).width() + prefix.len());
    Line::from(vec![
        Span::styled(prefix, style),
        Span::styled(text, style),
        Span::styled(" ".repeat(gap), style),
        Span::styled(hint, style),
    ])
}

fn render(
    frame: &mut ratatui::Frame,
    menu: &Menu,
    layout: Layout,
    navigation: &NavigationList<Action>,
    rows: &[(usize, usize)],
    actions: &[(Rect, Action)],
) {
    frame.render_widget(
        Paragraph::new(clean(&menu.title)).style(Style::default().add_modifier(Modifier::BOLD)),
        Rect::new(0, 0, layout.width, layout.header),
    );
    navigation.render(frame, |index, selected| {
        let (item, part) = rows[index];
        row_line(&menu.items[item], part, layout.width, selected)
    });
    if let Some(error) = menu.items.iter().find(|item| item.role == Role::Error) {
        let y = layout.height - layout.footer - layout.primary - layout.error;
        let (first, rest) = split_text(&clean(&error.label), usize::from(layout.width));
        frame.render_widget(
            Paragraph::new(vec![Line::from(first), Line::from(rest)])
                .style(Style::default().fg(Color::Red)),
            Rect::new(0, y, layout.width, layout.error),
        );
    }
    if let Some((index, item)) = menu
        .items
        .iter()
        .enumerate()
        .find(|(_, item)| item.role == Role::Primary)
    {
        let y = layout.height - layout.footer - layout.primary;
        let pressed = navigation.is_pressed(&Hit::Action(Action::Choose(index)));
        for row in 0..layout.primary {
            frame.render_widget(
                Paragraph::new(row_line(item, usize::from(row), layout.width, pressed)),
                Rect::new(0, y + row, layout.width, 1),
            );
        }
    }
    for (area, action) in actions {
        if matches!(action, Action::Choose(index) if menu.items[*index].role == Role::Primary) {
            continue;
        }
        let (label, hint) = match action {
            Action::Choose(_) => (tr("← Back", "← 返回"), "(back)"),
            Action::Previous => (tr("↑ prev", "↑ 上页"), "["),
            Action::Next => (tr("↓ next", "↓ 下页"), "]"),
            Action::Close => (tr("× close", "× 关闭"), "(esc)"),
        };
        let disabled = matches!(action, Action::Previous) && navigation.scroll == 0
            || matches!(action, Action::Next) && navigation.visible_rows().end == rows.len();
        let mut style = if disabled {
            Style::default().fg(Color::DarkGray)
        } else {
            Style::default()
        };
        if navigation.is_pressed(&Hit::Action(*action)) {
            style = style.bg(Color::DarkGray);
        }
        frame.render_widget(Block::default().style(style), *area);
        let label = if area.height == 1 && *action == Action::Close {
            format!("{label} {hint}")
        } else {
            label.into()
        };
        frame.render_widget(
            Paragraph::new(label)
                .alignment(Alignment::Center)
                .style(style),
            Rect::new(area.x, area.y, area.width, 1.min(area.height)),
        );
        if area.height > 1 {
            frame.render_widget(
                Paragraph::new(hint)
                    .alignment(Alignment::Center)
                    .style(style),
                Rect::new(area.x, area.y + 1, area.width, 1),
            );
        }
    }
}

fn interact(menu: &Menu, page: usize) -> Result<Option<usize>> {
    let _screen = Screen::enter()?;
    let mut terminal = Terminal::new(CrosstermBackend::new(stdout()))?;
    let mut navigation = NavigationList::<Action>::default();
    let mut rows = vec![];
    let mut row_width = None;
    let mut first_frame = true;
    let mut dirty = true;
    loop {
        let size = terminal.size()?;
        if row_width != Some(size.width) {
            rows = menu
                .items
                .iter()
                .enumerate()
                .filter(|(_, item)| item.role == Role::Body)
                .flat_map(|(index, item)| {
                    (0..row_count(item, size.width)).map(move |part| (index, part))
                })
                .collect();
            navigation.replace_rows(
                rows.iter()
                    .map(|(index, _)| {
                        menu.items[*index]
                            .enabled
                            .then(|| menu.items[*index].id.clone())
                    })
                    .collect(),
            );
            row_width = Some(size.width);
        }
        let layout = Layout::new(size.width, size.height, menu);
        navigation.set_viewport(layout.body);
        if first_frame {
            for _ in 0..page.min(rows.len()) {
                navigation.page(1);
            }
            first_frame = false;
        }
        let actions = layout.actions(menu, rows.len());
        if dirty {
            terminal.draw(|frame| render(frame, menu, layout, &navigation, &rows, &actions))?;
            dirty = false;
        }
        if !event::poll(Duration::from_millis(50))? {
            continue;
        }
        let mut chosen = None;
        match event::read()? {
            Event::Key(key) if key.kind == KeyEventKind::Press => {
                navigation.cancel_press();
                if key.code == KeyCode::Esc
                    || key.modifiers.contains(KeyModifiers::CONTROL)
                        && matches!(key.code, KeyCode::Char('c' | 'd'))
                {
                    return Ok(None);
                }
                let name = match key.code {
                    KeyCode::Enter => Some("Enter".into()),
                    KeyCode::Backspace => Some("BSpace".into()),
                    KeyCode::Char(character) => Some(character.to_string()),
                    _ => None,
                };
                chosen = name.as_ref().and_then(|name| {
                    menu.items
                        .iter()
                        .position(|item| item.enabled && item.key == *name)
                });
                if chosen.is_none() {
                    match key.code {
                        KeyCode::Backspace | KeyCode::Char('q') => return Ok(None),
                        KeyCode::Enter => {
                            chosen = rows.get(navigation.selected).map(|(index, _)| *index)
                        }
                        KeyCode::Char('[') => navigation.page(-1),
                        KeyCode::Char(']') => navigation.page(1),
                        code => {
                            navigation.navigate(code);
                        }
                    }
                }
            }
            Event::Mouse(mouse) => match navigation.mouse(mouse, &actions) {
                Some(Hit::Item(id)) => chosen = menu.items.iter().position(|item| item.id == id),
                Some(Hit::Action(Action::Choose(index))) => chosen = Some(index),
                Some(Hit::Action(Action::Close)) => return Ok(None),
                Some(Hit::Action(Action::Previous)) => navigation.page(-1),
                Some(Hit::Action(Action::Next)) => navigation.page(1),
                None => {}
            },
            Event::FocusLost => navigation.blur(),
            Event::FocusGained => navigation.selection_visible = true,
            _ => {}
        }
        if chosen.is_some_and(|index| {
            menu.items
                .get(index)
                .is_some_and(|item| item.role == Role::Close)
        }) {
            return Ok(None);
        }
        if let Some(index) = chosen.filter(|index| {
            menu.items
                .get(*index)
                .is_some_and(|item| item.enabled && item.role != Role::Close)
        }) {
            return Ok(Some(index));
        }
        dirty = true;
    }
}

fn plain_label(text: &str) -> String {
    let mut result = String::new();
    let mut characters = text.chars().peekable();
    while let Some(character) = characters.next() {
        if character == '#' && characters.peek() == Some(&'#') {
            result.push('#');
            characters.next();
        } else if character == '#' && characters.peek() == Some(&'[') {
            characters.next();
            let mut style = String::new();
            for character in characters.by_ref() {
                if character == ']' {
                    break;
                }
                style.push(character);
            }
            if style.contains("align=right") && !result.trim().is_empty() {
                result.push_str("  ");
            }
        } else {
            result.push(character);
        }
    }
    clean(&result).trim().to_owned()
}

pub(crate) fn display_legacy(args: &[String]) -> Result<()> {
    let mut title = tr("Menu", "菜单").to_owned();
    let mut client = std::env::var("WORKBENCH_TARGET_CLIENT").ok();
    let mut pane = std::env::var("WORKBENCH_SOURCE_PANE")
        .or_else(|_| std::env::var("TMUX_PANE"))
        .ok()
        .filter(|pane| !pane.is_empty());
    let mut cursor = 0;
    while let Some(flag) = args.get(cursor) {
        match flag.as_str() {
            "--" => {
                cursor += 1;
                break;
            }
            "-M" | "-O" => cursor += 1,
            "-c" | "-t" | "-T" | "-C" | "-x" | "-y" | "-b" | "-s" | "-S" | "-H" => {
                let value = args.get(cursor + 1).ok_or("menu flag requires a value")?;
                match flag.as_str() {
                    "-c" => client = Some(value.clone()),
                    "-t" => pane = Some(value.clone()),
                    "-T" => title = value.clone(),
                    _ => {}
                }
                cursor += 2;
            }
            _ => break,
        }
    }
    let client = client.ok_or("menu client unavailable")?;
    let tmux = crate::tmux::Tmux::new(ServerIdentity::discover()?);
    let pane = match pane {
        Some(pane) => pane,
        None => tmux
            .client_format(&client, "#{pane_id}")?
            .ok_or("menu source pane unavailable")?,
    };
    let mut items = vec![];
    while let Some(label) = args.get(cursor) {
        if label.is_empty() {
            cursor += 1;
            continue;
        }
        let key = args
            .get(cursor + 1)
            .ok_or("menu item requires a key")?
            .clone();
        let command = args
            .get(cursor + 2)
            .ok_or("menu item requires an action")?
            .clone();
        let label = if label.contains("#{") {
            tmux.client_format(&client, &clean(label))?
                .ok_or("menu client disappeared")?
        } else {
            label.clone()
        };
        let enabled = !label.starts_with('-') && (!command.is_empty() || !key.is_empty());
        let mut label = plain_label(label.strip_prefix('-').unwrap_or(&label));
        let role = if key == "Escape"
            || key == "BSpace" && command.is_empty()
            || label.starts_with('×') && command.is_empty()
        {
            Role::Close
        } else if key == "BSpace" {
            Role::Back
        } else if key == "Enter" {
            Role::Primary
        } else {
            Role::Body
        };
        if role == Role::Close {
            label = tr("× close", "× 关闭").into();
        }
        items.push(Item {
            id: format!("item:{}", items.len()),
            label,
            key,
            command,
            enabled,
            role,
        });
        cursor += 3;
    }
    if let Ok(command) = std::env::var("WORKBENCH_MENU_PARENT")
        && !command.is_empty()
        && !items.iter().any(|item| item.role == Role::Back)
    {
        items.push(Item {
            id: "parent".into(),
            label: crate::menu_footer::back_label().into(),
            key: "BSpace".into(),
            command,
            enabled: true,
            role: Role::Back,
        });
    }
    title = plain_label(&tmux.client_format(&client, &title)?.unwrap_or(title));
    display(Menu { title, items }, &pane, &client, 0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::{MouseButton, MouseEvent, MouseEventKind};

    fn item(id: &str, role: Role) -> Item {
        Item {
            id: id.into(),
            label: id.into(),
            key: String::new(),
            command: String::new(),
            enabled: role != Role::Error,
            role,
        }
    }

    #[test]
    fn menu_body_and_pinned_controls_keep_their_entire_touch_area() {
        let menu = Menu {
            title: "test".into(),
            items: vec![
                item("body", Role::Body),
                item("launch", Role::Primary),
                item("back", Role::Back),
                item("error", Role::Error),
            ],
        };
        for (width, height) in [(24, 8), (32, 12), (50, 25)] {
            let layout = Layout::new(width, height, &menu);
            let mut navigation = NavigationList::default();
            navigation.replace_rows(vec![Some("body".into()); 2]);
            navigation.set_viewport(layout.body);
            let actions = layout.actions(&menu, 30);
            for (area, action) in &actions {
                assert!(area.y >= layout.body.bottom());
                for row in area.y..area.bottom() {
                    for column in area.x..area.right() {
                        assert_eq!(
                            navigation.hit(column, row, &actions),
                            Some(Hit::Action(*action))
                        );
                    }
                }
            }
            let mouse = |kind, row| MouseEvent {
                kind,
                column: 1,
                row,
                modifiers: KeyModifiers::NONE,
            };
            for row in layout.body.y..layout.body.bottom().min(layout.body.y + 2) {
                assert_eq!(
                    navigation.mouse(
                        mouse(MouseEventKind::Down(MouseButton::Left), row),
                        &actions
                    ),
                    None
                );
                assert_eq!(
                    navigation.mouse(mouse(MouseEventKind::Up(MouseButton::Left), row), &actions),
                    Some(Hit::Item("body".into()))
                );
            }
        }
    }

    #[test]
    fn long_labels_use_both_button_rows_without_ellipses_or_split_wide_characters() {
        let mut button = item("中文会话名称很长需要换行", Role::Body);
        button.key = "a".into();
        let first = row_line(&button, 0, 18, false).to_string();
        let second = row_line(&button, 1, 18, false).to_string();
        for line in [&first, &second] {
            assert!(Span::raw(line).width() <= 18);
            assert!(!line.contains('…') && !line.contains("..."));
        }
        assert!(first.contains("中文会话名") && first.contains("(a)"));
        let label: String = (first.replace("(a)", "") + &second)
            .chars()
            .filter(|character| !character.is_whitespace())
            .collect();
        assert_eq!(label, button.label);
        let note = Item {
            enabled: false,
            ..button
        };
        assert!(row_count(&note, 10) > 1);
        assert!(!row_line(&note, 0, 10, false).to_string().contains("(a)"));
    }

    #[test]
    fn legacy_styles_and_control_characters_are_plain_text() {
        assert_eq!(
            plain_label("#[align=centre] × close #[align=right](esc)"),
            "× close   (esc)"
        );
        assert_eq!(
            plain_label("##[literal] #{host}\nname"),
            "#[literal] #{host} name"
        );
    }
}
