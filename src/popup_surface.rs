//! A bounded panel and its dismissible backdrop share one client-local surface.
//! tmux's native popup does not forward outside mouse events. Keep drawing and
//! list coordinates local to the card while capturing those events here.
use std::process::Command;

use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEventKind};
use ratatui::backend::{Backend, ClearType, WindowSize};
use ratatui::buffer::{Buffer, Cell};
use ratatui::layout::{Position, Rect, Size};
use ratatui::style::{Color, Style};
use ratatui::text::Span;
use ratatui::widgets::{Block, Clear, Paragraph, Widget};

use crate::popup::{Dimensions, Sizing};
use crate::server::ServerIdentity;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Geometry {
    screen: Size,
    card: Rect,
    content: Rect,
}

impl Geometry {
    fn new(screen: Size, sizing: Sizing) -> Self {
        let size = Dimensions::new(screen.width, screen.height, sizing);
        // Match tmux's C/C popup position, including its one-cell rounding.
        let x = (screen.width.saturating_sub(1) / 2).saturating_sub(size.width / 2);
        let y = (screen.height.saturating_sub(1) / 2 + size.height / 2).saturating_sub(size.height);
        let card = Rect::new(x, y, size.width, size.height);
        let margin = u16::from(size.border);
        let content = Rect::new(
            x + margin,
            y + margin,
            size.width.saturating_sub(2 * margin),
            size.height.saturating_sub(2 * margin),
        );
        Self {
            screen,
            card,
            content,
        }
    }
}

/// Backdrop text remains in memory only. It is deliberately muted while the
/// modal menu is open; dismissing restores tmux's live screen normally.
#[derive(Default)]
struct Backdrop {
    panes: Vec<(i32, i32, u16, u16, String)>,
    status: Vec<(u16, String)>,
}

impl Backdrop {
    fn capture(screen: Size) -> Self {
        let Ok(server) = ServerIdentity::discover() else {
            return Self::default();
        };
        let Ok(pane) = std::env::var("WORKBENCH_SOURCE_PANE") else {
            return Self::default();
        };
        let Ok(client) = std::env::var("WORKBENCH_TARGET_CLIENT") else {
            return Self::default();
        };
        let output = |args: &[&str]| {
            Command::new("tmux")
                .args(server.tmux_args())
                .args(args)
                .output()
                .ok()
                .filter(|result| result.status.success())
                .map(|result| String::from_utf8_lossy(&result.stdout).into_owned())
        };
        let context = crate::tmux::Tmux::new(server.clone()).client_format(
            &client,
            "#{status}\t#{status-position}\t#{window_offset_x}\t#{window_offset_y}\t#{T:status-left}\t#{W:#{T:window-status-format}}\t#{T:status-right}",
        ).ok().flatten().unwrap_or_default();
        let fields: Vec<_> = context.splitn(7, '\t').collect();
        let status_lines = match fields.first().copied().unwrap_or("off") {
            "on" => 1,
            "off" => 0,
            number => number.parse::<u16>().unwrap_or(0).min(screen.height),
        };
        let top_status = fields.get(1) == Some(&"top");
        let offset_x = fields
            .get(2)
            .and_then(|value| value.parse::<i32>().ok())
            .unwrap_or(0);
        let offset_y = fields
            .get(3)
            .and_then(|value| value.parse::<i32>().ok())
            .unwrap_or(0);
        let mut result = Self::default();
        if status_lines > 0 {
            let y = if top_status {
                0
            } else {
                screen.height.saturating_sub(status_lines)
            };
            result.status.push((
                y,
                fields
                    .iter()
                    .skip(4)
                    .map(|value| crate::menu::plain_label(value))
                    .collect::<String>(),
            ));
        }
        let Some(panes) = output(&[
            "list-panes",
            "-t",
            &pane,
            "-F",
            "#{pane_id}\t#{pane_left}\t#{pane_top}\t#{pane_width}\t#{pane_height}\t#{pane_active}\t#{window_zoomed_flag}",
        ]) else {
            return result;
        };
        for line in panes.lines() {
            let fields: Vec<_> = line.split('\t').collect();
            if fields.len() != 7 || (fields[6] == "1" && fields[5] != "1") {
                continue;
            }
            let (Ok(x), Ok(y), Ok(width), Ok(height)) = (
                fields[1].parse::<i32>(),
                fields[2].parse::<i32>(),
                fields[3].parse::<u16>(),
                fields[4].parse::<u16>(),
            ) else {
                continue;
            };
            if let Some(text) = output(&["capture-pane", "-p", "-t", fields[0]]) {
                result.panes.push((
                    x - offset_x,
                    y - offset_y
                        + if top_status {
                            i32::from(status_lines)
                        } else {
                            0
                        },
                    width,
                    height,
                    text,
                ));
            }
        }
        result
    }

    fn render(&self, geometry: Geometry) -> Buffer {
        let area = Rect::new(0, 0, geometry.screen.width, geometry.screen.height);
        let mut buffer = Buffer::empty(area);
        let muted = Style::default().fg(Color::DarkGray);
        for (x, y, width, height, text) in &self.panes {
            let left = (*x).max(0).min(i32::from(area.width)) as u16;
            let top = (*y).max(0).min(i32::from(area.height)) as u16;
            let right = (x + i32::from(*width)).max(0).min(i32::from(area.width)) as u16;
            let bottom = (y + i32::from(*height)).max(0).min(i32::from(area.height)) as u16;
            let rect = Rect::new(
                left,
                top,
                right.saturating_sub(left),
                bottom.saturating_sub(top),
            );
            Paragraph::new(text.as_str())
                .style(muted)
                .scroll(((-y).max(0) as u16, (-x).max(0) as u16))
                .render(rect, &mut buffer);
        }
        for (row, text) in &self.status {
            Paragraph::new(text.as_str())
                .style(muted)
                .render(Rect::new(0, *row, area.width, 1), &mut buffer);
        }
        // A wide backdrop glyph starting just outside the card can suppress
        // its first border cell in Buffer::diff. Clip that overlapping glyph.
        if geometry.card.x > 0 {
            for row in geometry.card.y..geometry.card.bottom() {
                let cell = &mut buffer[(geometry.card.x - 1, row)];
                if Span::raw(cell.symbol()).width() > 1 {
                    cell.reset();
                }
            }
        }
        Clear.render(geometry.card, &mut buffer);
        if geometry.card != geometry.content {
            Block::bordered().render(geometry.card, &mut buffer);
        }
        buffer
    }
}

pub(crate) struct Surface<B> {
    backend: B,
    sizing: Option<Sizing>,
    geometry: Option<Geometry>,
    event_geometry: Option<Geometry>,
    content: Buffer,
    backdrop: Backdrop,
    outside_press: Option<bool>,
}

impl<B: Backend> Surface<B> {
    pub fn from_env(backend: B) -> Result<Self, B::Error> {
        let sizing = std::env::var("WORKBENCH_POPUP_SURFACE")
            .or_else(|_| std::env::var("WORKBENCH_MENU_SURFACE"))
            .ok()
            .and_then(|value| {
                if value == "sidebar" {
                    return Some(Sizing::Sidebar);
                }
                let list = value.starts_with("list,");
                let value = value.strip_prefix("list,").unwrap_or(&value);
                value.split_once(',').and_then(|(width, height)| {
                    let width = width.parse().ok()?;
                    let height = height.parse().ok()?;
                    Some(if list {
                        Sizing::List { width, height }
                    } else {
                        Sizing::Menu { width, height }
                    })
                })
            });
        let backdrop = if sizing.is_some() {
            Backdrop::capture(backend.size()?)
        } else {
            Backdrop::default()
        };
        Ok(Self::new(backend, sizing, backdrop))
    }

    fn new(backend: B, sizing: Option<Sizing>, backdrop: Backdrop) -> Self {
        Self {
            backend,
            sizing,
            geometry: None,
            event_geometry: None,
            content: Buffer::empty(Rect::default()),
            backdrop,
            outside_press: None,
        }
    }

    fn current(&self) -> Result<Option<Geometry>, B::Error> {
        self.sizing
            .map(|sizing| self.backend.size().map(|size| Geometry::new(size, sizing)))
            .transpose()
    }

    /// A complete click on the backdrop dismisses. Hover, wheel, and a drag
    /// starting inside the card never dismiss or leak to the underlying pane.
    pub fn input(&mut self, event: Event) -> Result<Option<Event>, B::Error> {
        let Some(geometry) = self.current()? else {
            return Ok(Some(event));
        };
        if self
            .event_geometry
            .is_some_and(|previous| previous != geometry)
            || matches!(event, Event::Resize(..))
        {
            self.event_geometry = Some(geometry);
            self.outside_press = None;
            return Ok(Some(Event::FocusLost));
        }
        self.event_geometry = Some(geometry);
        let Event::Mouse(mut mouse) = event else {
            self.outside_press = None;
            return Ok(Some(event));
        };
        let point = Position::new(mouse.column, mouse.row);
        let outside = !geometry.card.contains(point);
        match mouse.kind {
            MouseEventKind::Down(MouseButton::Left) => {
                self.outside_press = outside.then_some(false)
            }
            MouseEventKind::Drag(_) | MouseEventKind::ScrollUp | MouseEventKind::ScrollDown => {
                if let Some(dragged) = &mut self.outside_press {
                    *dragged = true;
                }
            }
            MouseEventKind::Up(MouseButton::Left) => {
                if let Some(dragged) = self.outside_press.take() {
                    return Ok(Some(if outside && !dragged {
                        Event::Key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE))
                    } else {
                        Event::FocusLost
                    }));
                }
            }
            _ => {}
        }
        if !geometry.content.contains(point) {
            return Ok(Some(Event::FocusLost));
        }
        // A press that started on the backdrop must not turn into an action
        // when its release lands on a button.
        if self.outside_press.is_some() {
            return Ok(Some(Event::FocusLost));
        }
        mouse.column -= geometry.content.x;
        mouse.row -= geometry.content.y;
        Ok(Some(Event::Mouse(mouse)))
    }
}

impl<B: Backend> Backend for Surface<B> {
    type Error = B::Error;

    fn draw<'a, I>(&mut self, cells: I) -> Result<(), Self::Error>
    where
        I: Iterator<Item = (u16, u16, &'a Cell)>,
    {
        let Some(geometry) = self.current()? else {
            return self.backend.draw(cells);
        };
        let area = Rect::new(0, 0, geometry.content.width, geometry.content.height);
        if self.content.area != area {
            self.content = Buffer::empty(area);
        }
        let cells: Vec<_> = cells.collect();
        for (x, y, cell) in &cells {
            if let Some(target) = self.content.cell_mut((*x, *y)) {
                *target = (*cell).clone();
            }
        }
        if self.geometry != Some(geometry) {
            self.geometry = Some(geometry);
            let background = self.backdrop.render(geometry);
            self.backend.clear()?;
            self.backend
                .draw(Buffer::empty(background.area).diff(&background).into_iter())?;
            self.backend.draw(
                Buffer::empty(area)
                    .diff(&self.content)
                    .into_iter()
                    .map(|(x, y, cell)| (x + geometry.content.x, y + geometry.content.y, cell)),
            )
        } else {
            self.backend.draw(
                cells
                    .into_iter()
                    .map(|(x, y, cell)| (x + geometry.content.x, y + geometry.content.y, cell)),
            )
        }
    }

    fn hide_cursor(&mut self) -> Result<(), Self::Error> {
        self.backend.hide_cursor()
    }
    fn show_cursor(&mut self) -> Result<(), Self::Error> {
        self.backend.show_cursor()
    }
    fn get_cursor_position(&mut self) -> Result<Position, Self::Error> {
        let mut position = self.backend.get_cursor_position()?;
        if let Some(geometry) = self.current()? {
            position.x = position.x.saturating_sub(geometry.content.x);
            position.y = position.y.saturating_sub(geometry.content.y);
        }
        Ok(position)
    }
    fn set_cursor_position<P: Into<Position>>(&mut self, position: P) -> Result<(), Self::Error> {
        let mut position = position.into();
        if let Some(geometry) = self.current()? {
            position.x += geometry.content.x;
            position.y += geometry.content.y;
        }
        self.backend.set_cursor_position(position)
    }
    fn clear(&mut self) -> Result<(), Self::Error> {
        if self.sizing.is_some() {
            self.geometry = None;
            self.content.reset();
        }
        self.backend.clear()
    }
    fn clear_region(&mut self, clear_type: ClearType) -> Result<(), Self::Error> {
        if self.sizing.is_some() {
            self.clear()
        } else {
            self.backend.clear_region(clear_type)
        }
    }
    fn size(&self) -> Result<Size, Self::Error> {
        match self.current()? {
            Some(geometry) => Ok(geometry.content.as_size()),
            None => self.backend.size(),
        }
    }
    fn window_size(&mut self) -> Result<WindowSize, Self::Error> {
        let mut size = self.backend.window_size()?;
        size.columns_rows = self.size()?;
        Ok(size)
    }
    fn flush(&mut self) -> Result<(), Self::Error> {
        self.backend.flush()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::MouseEvent;
    use ratatui::{Terminal, backend::TestBackend};

    fn surface() -> Surface<TestBackend> {
        Surface::new(
            TestBackend::new(80, 30),
            Some(Sizing::Menu {
                width: 30,
                height: 12,
            }),
            Backdrop::default(),
        )
    }
    fn mouse(kind: MouseEventKind, x: u16, y: u16) -> Event {
        Event::Mouse(MouseEvent {
            kind,
            column: x,
            row: y,
            modifiers: KeyModifiers::NONE,
        })
    }
    fn outside(kind: MouseEventKind) -> Event {
        mouse(kind, 0, 0)
    }

    #[test]
    fn backdrop_requires_a_complete_click_and_ignores_hover_wheel_drag() {
        let mut surface = surface();
        for kind in [
            MouseEventKind::Moved,
            MouseEventKind::ScrollUp,
            MouseEventKind::ScrollDown,
            MouseEventKind::Up(MouseButton::Left),
        ] {
            assert_eq!(
                surface.input(outside(kind)).unwrap(),
                Some(Event::FocusLost)
            );
        }
        surface
            .input(outside(MouseEventKind::Down(MouseButton::Left)))
            .unwrap();
        assert!(
            matches!(surface.input(outside(MouseEventKind::Up(MouseButton::Left))).unwrap(), Some(Event::Key(key)) if key.code == KeyCode::Esc)
        );
        for cancel in [
            MouseEventKind::Drag(MouseButton::Left),
            MouseEventKind::ScrollDown,
        ] {
            surface
                .input(outside(MouseEventKind::Down(MouseButton::Left)))
                .unwrap();
            surface.input(outside(cancel)).unwrap();
            assert_eq!(
                surface
                    .input(outside(MouseEventKind::Up(MouseButton::Left)))
                    .unwrap(),
                Some(Event::FocusLost)
            );
        }
    }

    #[test]
    fn card_and_backdrop_crossings_neither_activate_nor_dismiss() {
        let mut surface = surface();
        let rect = surface.current().unwrap().unwrap().content;
        let inside = |kind| mouse(kind, rect.x + 1, rect.y + 1);
        assert!(
            matches!(surface.input(inside(MouseEventKind::Down(MouseButton::Left))).unwrap(), Some(Event::Mouse(event)) if (event.column, event.row) == (1, 1))
        );
        surface
            .input(outside(MouseEventKind::Drag(MouseButton::Left)))
            .unwrap();
        assert_eq!(
            surface
                .input(outside(MouseEventKind::Up(MouseButton::Left)))
                .unwrap(),
            Some(Event::FocusLost)
        );
        surface
            .input(outside(MouseEventKind::Down(MouseButton::Left)))
            .unwrap();
        assert_eq!(
            surface
                .input(inside(MouseEventKind::Up(MouseButton::Left)))
                .unwrap(),
            Some(Event::FocusLost)
        );
        surface
            .input(outside(MouseEventKind::Down(MouseButton::Left)))
            .unwrap();
        surface.backend.resize(100, 35);
        assert_eq!(
            surface
                .input(outside(MouseEventKind::Up(MouseButton::Left)))
                .unwrap(),
            Some(Event::FocusLost)
        );
    }

    #[test]
    fn card_rendering_and_mouse_coordinates_agree_after_resize() {
        let mut terminal = Terminal::new(surface()).unwrap();
        let draw = |frame: &mut ratatui::Frame| {
            frame.render_widget(Paragraph::new("菜单 action"), frame.area())
        };
        terminal.draw(draw).unwrap();
        for (width, height) in [(100, 35), (24, 8), (80, 30)] {
            terminal.backend_mut().backend.resize(width, height);
            terminal.draw(draw).unwrap();
            let surface = terminal.backend_mut();
            let geometry = surface.current().unwrap().unwrap();
            surface.input(Event::Resize(width, height)).unwrap();
            assert_eq!(
                surface.backend.buffer()[(geometry.content.x, geometry.content.y)].symbol(),
                "菜"
            );
            assert!(
                matches!(surface.input(mouse(MouseEventKind::Moved, geometry.content.x, geometry.content.y)).unwrap(), Some(Event::Mouse(event)) if (event.column, event.row) == (0, 0))
            );
        }
    }

    #[test]
    fn wide_backdrop_characters_do_not_erase_card_borders() {
        let text = ("中".repeat(40) + "\n").repeat(30);
        let mut terminal = Terminal::new(Surface::new(
            TestBackend::new(80, 30),
            Some(Sizing::Menu {
                width: 30,
                height: 12,
            }),
            Backdrop {
                panes: vec![(0, 0, 80, 30, text)],
                status: vec![],
            },
        ))
        .unwrap();
        terminal
            .draw(|frame| {
                frame.render_widget(Paragraph::new("菜单 action"), frame.area());
            })
            .unwrap();
        let surface = terminal.backend();
        let card = surface.current().unwrap().unwrap().card;
        let buffer = surface.backend.buffer();
        for row in card.y + 1..card.bottom() - 1 {
            assert_eq!(
                buffer[(card.x, row)].symbol(),
                "│",
                "left border missing at row {row}"
            );
            assert_eq!(buffer[(card.right() - 1, row)].symbol(), "│");
        }
    }
}
