//! Shared Sidebar list interaction. Views supply one stable key per rendered
//! line (None for headings), an actual content rectangle, and action regions.
//! Rendering and hit testing therefore use the same physical terminal rows.
use std::{io::stdout, ops::Range};

use crossterm::event::{
    DisableBracketedPaste, DisableFocusChange, DisableMouseCapture, EnableBracketedPaste,
    EnableFocusChange, EnableMouseCapture, KeyCode, MouseButton, MouseEvent, MouseEventKind,
};
use crossterm::execute;
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use ratatui::layout::Rect;
use ratatui::text::Line;
use ratatui::widgets::Paragraph;

const WHEEL_ROWS: isize = 2;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Hit<A> {
    Item(String),
    Action(A),
}

struct Press<A> {
    hit: Hit<A>,
    row: u16,
    dragged: bool,
}

pub(crate) struct NavigationList<A> {
    rows: Vec<Option<String>>,
    area: Rect,
    pub selected: usize,
    pub scroll: usize,
    pub selection_visible: bool,
    pub hovered: Option<Hit<A>>,
    pressed: Option<Press<A>>,
    follow_selection: bool,
}

impl<A: Clone + Eq> Default for NavigationList<A> {
    fn default() -> Self {
        Self {
            rows: vec![],
            area: Rect::default(),
            selected: 0,
            scroll: 0,
            selection_visible: true,
            hovered: None,
            pressed: None,
            follow_selection: true,
        }
    }
}

impl<A: Clone + Eq> NavigationList<A> {
    fn start(&self, index: usize) -> usize {
        let Some(Some(key)) = self.rows.get(index) else {
            return index;
        };
        let mut start = index;
        while start > 0 && self.rows[start - 1].as_ref() == Some(key) {
            start -= 1;
        }
        start
    }

    fn nearest(&self, key: &str, previous: usize) -> Option<usize> {
        self.rows
            .iter()
            .enumerate()
            .filter(|(_, row)| row.as_deref() == Some(key))
            .min_by_key(|(index, _)| index.abs_diff(previous))
            .map(|(index, _)| index)
    }

    pub fn replace_rows(&mut self, rows: Vec<Option<String>>) {
        let selected_key = self.rows.get(self.selected).cloned().flatten();
        let anchor = self.rows.get(self.scroll).cloned().flatten().map(|key| {
            let start = self.start(self.scroll);
            (key, start, self.scroll - start)
        });
        self.rows = rows;
        self.selected = selected_key
            .as_deref()
            .and_then(|key| self.nearest(key, self.selected))
            .or_else(|| self.rows.iter().position(Option::is_some))
            .map(|index| self.start(index))
            .unwrap_or(0);
        if let Some((key, previous, offset)) = anchor
            && let Some(index) = self.nearest(&key, previous)
        {
            let start = self.start(index);
            self.scroll = start
                + (0..=offset)
                    .take_while(|offset| {
                        self.rows.get(start + offset).and_then(Option::as_deref) == Some(&key)
                    })
                    .count()
                    .saturating_sub(1);
        }
        self.clamp_scroll();
    }

    pub fn set_viewport(&mut self, area: Rect) {
        if self.area != area {
            self.pressed = None;
            self.hovered = None;
            self.area = area;
            if self.follow_selection {
                self.reveal_selection();
            }
        }
        self.clamp_scroll();
    }

    fn clamp_scroll(&mut self) {
        self.scroll = self.scroll.min(
            self.rows
                .len()
                .saturating_sub(usize::from(self.area.height)),
        );
    }

    fn reveal_selection(&mut self) {
        let height = usize::from(self.area.height);
        if height == 0 {
            return;
        }
        let Some(Some(key)) = self.rows.get(self.selected) else {
            return;
        };
        let end = self
            .rows
            .iter()
            .skip(self.selected)
            .take_while(|row| row.as_ref() == Some(key))
            .count()
            .min(height)
            + self.selected;
        if self.selected < self.scroll {
            self.scroll = self.selected;
        } else if end > self.scroll + height {
            self.scroll = end.saturating_sub(height);
        }
        self.clamp_scroll();
    }

    pub fn select(&mut self, index: usize) {
        if self.rows.get(index).is_some_and(Option::is_some) {
            self.selected = self.start(index);
            self.selection_visible = true;
            self.follow_selection = true;
            self.reveal_selection();
        }
    }

    pub fn navigate(&mut self, key: KeyCode) -> bool {
        let next = match key {
            KeyCode::Down | KeyCode::Char('j') => (self.selected + 1..self.rows.len())
                .find(|index| self.rows[*index].is_some() && self.start(*index) == *index),
            KeyCode::Up | KeyCode::Char('k') => (0..self.selected)
                .rev()
                .find(|index| self.rows[*index].is_some() && self.start(*index) == *index),
            KeyCode::Home => self.rows.iter().position(Option::is_some),
            KeyCode::End => self.rows.iter().rposition(Option::is_some),
            KeyCode::PageDown | KeyCode::PageUp => {
                self.page(if key == KeyCode::PageDown { 1 } else { -1 });
                return true;
            }
            _ => return false,
        };
        self.pressed = None;
        if let Some(index) = next {
            self.select(index);
        }
        true
    }

    pub fn scroll_by(&mut self, delta: isize) {
        self.follow_selection = false;
        self.scroll = self.scroll.saturating_add_signed(delta);
        self.clamp_scroll();
    }

    pub fn page(&mut self, direction: isize) {
        self.cancel_press();
        self.scroll_by(direction * isize::try_from(self.area.height).unwrap_or(0));
        if let Some(index) = self
            .visible_rows()
            .find(|index| self.rows[*index].is_some())
        {
            self.selected = self.start(index);
            self.selection_visible = true;
        }
    }

    pub fn cancel_press(&mut self) {
        self.pressed = None;
    }

    pub fn blur(&mut self) {
        self.selection_visible = false;
        self.hovered = None;
        self.cancel_press();
    }

    pub fn visible_rows(&self) -> Range<usize> {
        self.scroll..(self.scroll + usize::from(self.area.height)).min(self.rows.len())
    }

    pub fn is_selected(&self, row: usize) -> bool {
        self.selection_visible
            && self
                .rows
                .get(row)
                .and_then(Option::as_ref)
                .is_some_and(|key| {
                    self.rows.get(self.selected).and_then(Option::as_ref) == Some(key)
                })
    }

    pub fn render(
        &self,
        frame: &mut ratatui::Frame,
        mut row: impl FnMut(usize, bool) -> Line<'static>,
    ) {
        let lines: Vec<_> = self
            .visible_rows()
            .map(|index| row(index, self.is_selected(index)))
            .collect();
        frame.render_widget(Paragraph::new(lines), self.area);
    }

    pub fn hit(&self, column: u16, row: u16, actions: &[(Rect, A)]) -> Option<Hit<A>> {
        if let Some((_, action)) = actions
            .iter()
            .find(|(area, _)| area.contains((column, row).into()))
        {
            return Some(Hit::Action(action.clone()));
        }
        if !self.area.contains((column, row).into()) {
            return None;
        }
        self.rows
            .get(self.scroll + usize::from(row - self.area.y))
            .and_then(Option::as_ref)
            .cloned()
            .map(Hit::Item)
    }

    pub fn is_pressed(&self, hit: &Hit<A>) -> bool {
        self.pressed
            .as_ref()
            .is_some_and(|press| !press.dragged && &press.hit == hit)
    }

    pub fn mouse(&mut self, mouse: MouseEvent, actions: &[(Rect, A)]) -> Option<Hit<A>> {
        let hit = self.hit(mouse.column, mouse.row, actions);
        match mouse.kind {
            MouseEventKind::Down(MouseButton::Left) => {
                if let Some(Hit::Item(key)) = &hit
                    && let Some(index) = self.nearest(
                        key,
                        self.scroll + usize::from(mouse.row.saturating_sub(self.area.y)),
                    )
                {
                    self.selected = self.start(index);
                    self.selection_visible = true;
                }
                self.follow_selection = false;
                self.pressed = hit.map(|hit| Press {
                    hit,
                    row: mouse.row,
                    dragged: false,
                });
            }
            MouseEventKind::Drag(MouseButton::Left) => {
                if let Some(press) = &mut self.pressed {
                    press.dragged = true;
                    let delta = isize::try_from(press.row).unwrap_or(0)
                        - isize::try_from(mouse.row).unwrap_or(0);
                    press.row = mouse.row;
                    self.scroll_by(delta);
                }
            }
            MouseEventKind::Up(MouseButton::Left) => {
                if let Some(press) = self.pressed.take()
                    && !press.dragged
                    && hit.as_ref() == Some(&press.hit)
                {
                    return hit;
                }
            }
            MouseEventKind::ScrollDown | MouseEventKind::ScrollUp => {
                self.cancel_press();
                self.scroll_by(if mouse.kind == MouseEventKind::ScrollDown {
                    WHEEL_ROWS
                } else {
                    -WHEEL_ROWS
                });
            }
            MouseEventKind::Moved => {
                self.hovered = hit.clone();
                self.selection_visible = matches!(hit, Some(Hit::Item(_)));
                if let Some(Hit::Item(key)) = hit
                    && let Some(index) = self.nearest(
                        &key,
                        self.scroll + usize::from(mouse.row.saturating_sub(self.area.y)),
                    )
                {
                    self.selected = self.start(index);
                }
            }
            MouseEventKind::Down(MouseButton::Right) => {
                self.cancel_press();
                if let Some(Hit::Item(key)) = hit
                    && let Some(index) = self.nearest(
                        &key,
                        self.scroll + usize::from(mouse.row.saturating_sub(self.area.y)),
                    )
                {
                    self.selected = self.start(index);
                    self.selection_visible = true;
                    return Some(Hit::Item(key));
                }
            }
            _ => {}
        }
        None
    }
}

pub(crate) struct Screen;
impl Screen {
    pub fn enter() -> Result<Self, Box<dyn std::error::Error>> {
        enable_raw_mode()?;
        let guard = Self;
        execute!(
            stdout(),
            EnterAlternateScreen,
            EnableMouseCapture,
            EnableFocusChange,
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
            DisableFocusChange,
            DisableBracketedPaste,
            crossterm::cursor::Show
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::KeyModifiers;

    fn list() -> NavigationList<&'static str> {
        let mut list = NavigationList::default();
        list.replace_rows(
            [
                None,
                Some("a"),
                Some("a"),
                None,
                Some("b"),
                Some("b"),
                Some("c"),
                Some("c"),
            ]
            .into_iter()
            .map(|key| key.map(str::to_owned))
            .collect(),
        );
        list.set_viewport(Rect::new(1, 2, 24, 4));
        list.select(1);
        list
    }

    fn mouse(kind: MouseEventKind, row: u16) -> MouseEvent {
        MouseEvent {
            kind,
            column: 5,
            row,
            modifiers: KeyModifiers::NONE,
        }
    }

    #[test]
    fn wheel_scrolls_without_activating_or_recentering_on_refresh() {
        let mut list = list();
        assert_eq!(list.mouse(mouse(MouseEventKind::ScrollDown, 0), &[]), None);
        assert_eq!(list.scroll, 2);
        assert_eq!(list.selected, 1);
        list.replace_rows(list.rows.clone());
        list.set_viewport(Rect::new(1, 2, 24, 4));
        assert_eq!(list.scroll, 2);
        for _ in 0..20 {
            list.mouse(mouse(MouseEventKind::ScrollDown, 3), &[]);
        }
        assert_eq!(list.scroll, 4);
        for _ in 0..20 {
            list.mouse(mouse(MouseEventKind::ScrollUp, 3), &[]);
        }
        assert_eq!(list.scroll, 0);
    }

    #[test]
    fn both_entry_lines_activate_on_release_and_headers_do_not() {
        for row in [3, 4] {
            let mut list = list();
            assert_eq!(
                list.mouse(mouse(MouseEventKind::Down(MouseButton::Left), row), &[]),
                None
            );
            assert_eq!(
                list.mouse(mouse(MouseEventKind::Up(MouseButton::Left), row), &[]),
                Some(Hit::Item("a".into()))
            );
        }
        let list = list();
        assert_eq!(list.hit(5, 2, &[]), None);
        assert_eq!(list.hit(5, 6, &[]), None);
        assert_eq!(list.hit(0, 3, &[]), None);
    }

    #[test]
    fn dragging_wheeling_and_resizing_cancel_held_activation() {
        for interruption in 0..3 {
            let mut list = list();
            list.mouse(mouse(MouseEventKind::Down(MouseButton::Left), 4), &[]);
            match interruption {
                0 => {
                    list.mouse(mouse(MouseEventKind::Drag(MouseButton::Left), 2), &[]);
                }
                1 => {
                    list.mouse(mouse(MouseEventKind::ScrollDown, 4), &[]);
                }
                _ => list.set_viewport(Rect::new(1, 2, 24, 3)),
            }
            assert_eq!(
                list.mouse(mouse(MouseEventKind::Up(MouseButton::Left), 4), &[]),
                None
            );
        }
    }

    #[test]
    fn refresh_preserves_the_browsed_item_and_rejects_replaced_touch_targets() {
        let mut list = list();
        list.scroll_by(4);
        let mut rows = list.rows.clone();
        rows.insert(0, None);
        list.replace_rows(rows);
        assert_eq!(list.scroll, 5);
        assert_eq!(list.selected, 2);
        list.mouse(mouse(MouseEventKind::Down(MouseButton::Left), 2), &[]);
        list.replace_rows(
            list.rows
                .iter()
                .map(|key| key.as_ref().map(|_| "replacement".into()))
                .collect(),
        );
        assert_eq!(
            list.mouse(mouse(MouseEventKind::Up(MouseButton::Left), 2), &[]),
            None
        );
    }

    #[test]
    fn keyboard_skips_secondary_rows_and_reveals_whole_entries() {
        let mut list = list();
        list.navigate(KeyCode::Down);
        assert_eq!(list.selected, 4);
        assert_eq!(list.scroll, 2);
        list.navigate(KeyCode::Down);
        assert_eq!(list.selected, 6);
        assert_eq!(list.scroll, 4);
        list.navigate(KeyCode::Home);
        assert_eq!(list.selected, 1);
        assert_eq!(list.scroll, 1);
        list.navigate(KeyCode::End);
        assert_eq!(list.selected, 6);
        list.page(-1);
        assert_eq!(list.scroll, 0);
        assert_eq!(list.selected, 1);
    }

    #[test]
    fn actions_share_release_and_drag_rules_with_list_entries() {
        let mut list = list();
        let actions = [(Rect::new(1, 0, 24, 2), "close")];
        list.mouse(mouse(MouseEventKind::Down(MouseButton::Left), 0), &actions);
        assert_eq!(
            list.mouse(mouse(MouseEventKind::Up(MouseButton::Left), 1), &actions),
            Some(Hit::Action("close"))
        );
        list.mouse(mouse(MouseEventKind::Down(MouseButton::Left), 0), &actions);
        list.mouse(mouse(MouseEventKind::Drag(MouseButton::Left), 1), &actions);
        assert_eq!(
            list.mouse(mouse(MouseEventKind::Up(MouseButton::Left), 1), &actions),
            None
        );
    }
}
