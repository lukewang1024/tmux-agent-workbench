use std::io::{self, stdout};
use std::process::Command;

use clap::ValueEnum;
use crossterm::{
    event::{
        self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyEventKind, MouseButton,
        MouseEventKind,
    },
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use ratatui::{
    Terminal,
    backend::CrosstermBackend,
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{List, ListItem, ListState, Paragraph},
};

#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum StatusMenuKind {
    Host,
    Tmux,
    Agent,
}

#[derive(Debug)]
struct Action {
    label: String,
    key: char,
    command: ActionCommand,
}

#[derive(Debug)]
enum ActionCommand {
    Tmux(Vec<String>),
    Agent(String),
    Host(String),
}

pub fn run(kind: StatusMenuKind, pane: &str) -> Result<(), Box<dyn std::error::Error>> {
    validate_pane(pane)?;
    let (title, actions) = actions(kind)?;
    if actions.is_empty() {
        return Err("no menu entries".into());
    }

    enable_raw_mode()?;
    let mut output = stdout();
    execute!(output, EnterAlternateScreen, EnableMouseCapture)?;
    let backend = CrosstermBackend::new(output);
    let mut terminal = Terminal::new(backend)?;
    let selected = event_loop(&mut terminal, title, &actions);
    disable_raw_mode()?;
    execute!(
        terminal.backend_mut(),
        DisableMouseCapture,
        LeaveAlternateScreen
    )?;
    terminal.show_cursor()?;

    if let Some(index) = selected? {
        execute_action(&actions[index], pane)?;
    }
    Ok(())
}

fn event_loop(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    _title: &str,
    actions: &[Action],
) -> Result<Option<usize>, Box<dyn std::error::Error>> {
    let mut selected = 0;
    loop {
        terminal.draw(|frame| {
            let area = frame.area();
            let list_area = Rect::new(0, 0, area.width, area.height.saturating_sub(1));
            let items = actions.iter().map(|action| {
                ListItem::new(Line::from(vec![
                    Span::raw(" "),
                    Span::styled(&action.label, primary_style()),
                    Span::raw("  "),
                    Span::styled(format!("[{}]", action.key), muted_style()),
                ]))
            });
            let list = List::new(items)
                .highlight_style(
                    Style::default()
                        .bg(Color::DarkGray)
                        .fg(Color::Rgb(235, 235, 245))
                        .add_modifier(Modifier::BOLD),
                )
                .highlight_symbol("›");
            let mut state = ListState::default().with_selected(Some(selected));
            frame.render_stateful_widget(list, list_area, &mut state);
            let close = Line::from(vec![Span::raw(" "), Span::styled("× close", muted_style())])
                .right_aligned();
            frame.render_widget(
                Paragraph::new(close),
                Rect::new(0, area.height.saturating_sub(1), area.width, 1),
            );
        })?;

        match event::read()? {
            Event::Key(key) if key.kind == KeyEventKind::Press => match key.code {
                KeyCode::Esc | KeyCode::Char('q') => return Ok(None),
                KeyCode::Up => selected = selected.checked_sub(1).unwrap_or(actions.len() - 1),
                KeyCode::Down => selected = (selected + 1) % actions.len(),
                KeyCode::Enter => return Ok(Some(selected)),
                KeyCode::Char(ch) => {
                    if let Some(index) = actions.iter().position(|action| action.key == ch) {
                        return Ok(Some(index));
                    }
                }
                _ => {}
            },
            Event::Mouse(mouse) => match mouse.kind {
                MouseEventKind::ScrollDown => selected = (selected + 1) % actions.len(),
                MouseEventKind::ScrollUp => {
                    selected = selected.checked_sub(1).unwrap_or(actions.len() - 1)
                }
                MouseEventKind::Moved if usize::from(mouse.row) < actions.len() => {
                    selected = usize::from(mouse.row)
                }
                MouseEventKind::Down(MouseButton::Left) => {
                    let row = usize::from(mouse.row);
                    if row < actions.len() {
                        return Ok(Some(row));
                    }
                    if row + 1 == usize::from(terminal.size()?.height)
                        && usize::from(mouse.column) + 8 >= usize::from(terminal.size()?.width)
                    {
                        return Ok(None);
                    }
                }
                _ => {}
            },
            _ => {}
        }
    }
}

fn primary_style() -> Style {
    Style::default().fg(Color::Rgb(235, 235, 245))
}

fn muted_style() -> Style {
    // Keep status popups on the same semantic palette as Agent Sidebar:
    // bright neutral content, ANSI bright-black for secondary controls.
    Style::default().fg(Color::DarkGray)
}

fn actions(
    kind: StatusMenuKind,
) -> Result<(&'static str, Vec<Action>), Box<dyn std::error::Error>> {
    Ok(match kind {
        StatusMenuKind::Tmux => (
            "tmux",
            vec![
                tmux_action("New window", 'c', &["new-window"]),
                tmux_action("Split below", '-', &["split-window", "-v"]),
                tmux_action("Split right", '|', &["split-window", "-h"]),
                tmux_action("Choose window", 'w', &["choose-tree", "-Zw"]),
                tmux_action("Choose session", 's', &["choose-tree", "-Zs"]),
                tmux_action("Detach", 'd', &["detach-client"]),
            ],
        ),
        StatusMenuKind::Agent => (
            "Agent",
            vec![
                agent_action("/side", 's'),
                agent_action("/btw", 'b'),
                agent_action("/fork", 'f'),
            ],
        ),
        StatusMenuKind::Host => {
            let output = Command::new("ssh-connect")
                .args(["hosts", "list"])
                .output()?;
            if !output.status.success() {
                return Err("ssh-connect hosts list failed".into());
            }
            let actions = String::from_utf8(output.stdout)?
                .lines()
                .filter(|host| !host.is_empty())
                .take(35)
                .enumerate()
                .map(|(index, host)| Action {
                    label: host.to_owned(),
                    key: "123456789abcdefghijklmnopqrstuvwxyz"
                        .chars()
                        .nth(index)
                        .unwrap(),
                    command: ActionCommand::Host(host.to_owned()),
                })
                .collect();
            ("SSH", actions)
        }
    })
}

fn tmux_action(label: &str, key: char, args: &[&str]) -> Action {
    Action {
        label: label.into(),
        key,
        command: ActionCommand::Tmux(args.iter().map(|v| (*v).into()).collect()),
    }
}

fn agent_action(label: &str, key: char) -> Action {
    Action {
        label: label.into(),
        key,
        command: ActionCommand::Agent(label.into()),
    }
}

fn execute_action(action: &Action, pane: &str) -> Result<(), Box<dyn std::error::Error>> {
    let path = tmux_output(&["display-message", "-p", "-t", pane, "#{pane_current_path}"])?;
    let status = match &action.command {
        ActionCommand::Tmux(args) => {
            let mut command = Command::new("tmux");
            command.args(args);
            if matches!(
                args.first().map(String::as_str),
                Some("new-window" | "split-window")
            ) {
                command.args(["-c", path.trim()]);
            }
            command.status()?
        }
        ActionCommand::Agent(text) => Command::new("tmux")
            .args(["send-keys", "-t", pane, "-l", text])
            .status()
            .and_then(|status| {
                if status.success() {
                    Command::new("tmux")
                        .args(["send-keys", "-t", pane, "Enter"])
                        .status()
                } else {
                    Ok(status)
                }
            })?,
        ActionCommand::Host(host) => Command::new("tmux")
            .args([
                "new-window",
                "-c",
                path.trim(),
                "-n",
                host,
                &format!("exec ssh-connect connect {}", shell_quote(host)),
            ])
            .status()?,
    };
    if status.success() {
        Ok(())
    } else {
        Err(format!("menu action failed: {}", action.label).into())
    }
}

fn tmux_output(args: &[&str]) -> Result<String, Box<dyn std::error::Error>> {
    let output = Command::new("tmux").args(args).output()?;
    if !output.status.success() {
        return Err("tmux command failed".into());
    }
    Ok(String::from_utf8(output.stdout)?)
}

fn validate_pane(pane: &str) -> Result<(), Box<dyn std::error::Error>> {
    if pane
        .strip_prefix('%')
        .is_some_and(|value| !value.is_empty() && value.chars().all(|ch| ch.is_ascii_digit()))
    {
        Ok(())
    } else {
        Err("invalid pane target".into())
    }
}

fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pane_targets_are_strict() {
        assert!(validate_pane("%12").is_ok());
        assert!(validate_pane("%12;kill-server").is_err());
    }

    #[test]
    fn shell_quotes_hosts() {
        assert_eq!(shell_quote("dev'box"), "'dev'\\''box'");
    }
}
