use std::collections::HashSet;
use std::io;
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use thiserror::Error;

use crate::model::TmuxTarget;
use crate::server::ServerIdentity;

const FIELD_SEPARATOR: char = '\u{1f}';

#[derive(Debug, Error)]
pub enum TmuxError {
    #[error("failed to execute tmux: {0}")]
    Io(#[from] io::Error),
    #[error("tmux command failed: {0}")]
    Command(String),
    #[error("tmux command timed out after {0} ms")]
    Timeout(u64),
    #[error("invalid tmux inventory row: {0:?}")]
    InvalidRow(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pane {
    pub target: TmuxTarget,
    pub root_pid: u32,
    pub title: String,
    pub current_command: String,
    pub current_path: String,
    pub role: Option<String>,
    pub visible: bool,
    pub window_active: bool,
    pub pane_active: bool,
    pub pane_last: bool,
    pub session_visible: bool,
    pub content_revision: String,
}

pub trait TmuxSource {
    fn panes(&self) -> Result<Vec<Pane>, TmuxError>;
    fn capture_bottom(
        &self,
        pane_id: &str,
        lines: usize,
        bytes: usize,
    ) -> Result<String, TmuxError>;
    fn server_alive(&self) -> bool;
}

#[derive(Debug, Clone)]
pub struct Tmux {
    server: ServerIdentity,
}

impl Tmux {
    pub fn new(server: ServerIdentity) -> Self {
        Self { server }
    }

    fn output(&self, args: &[&str]) -> Result<String, TmuxError> {
        let output = Command::new("tmux")
            .arg("-S")
            .arg(&self.server.socket_path)
            .args(args)
            .output()?;
        if !output.status.success() {
            return Err(TmuxError::Command(
                String::from_utf8_lossy(&output.stderr).trim().to_owned(),
            ));
        }
        Ok(String::from_utf8_lossy(&output.stdout).into_owned())
    }

    fn output_with_timeout(&self, args: &[&str], timeout: Duration) -> Result<String, TmuxError> {
        let mut child = Command::new("tmux")
            .arg("-S")
            .arg(&self.server.socket_path)
            .args(args)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()?;
        let deadline = Instant::now() + timeout;
        loop {
            if child.try_wait()?.is_some() {
                let output = child.wait_with_output()?;
                if !output.status.success() {
                    return Err(TmuxError::Command(
                        String::from_utf8_lossy(&output.stderr).trim().to_owned(),
                    ));
                }
                return Ok(String::from_utf8_lossy(&output.stdout).into_owned());
            }
            if Instant::now() >= deadline {
                let _ = child.kill();
                let _ = child.wait();
                return Err(TmuxError::Timeout(timeout.as_millis() as u64));
            }
            thread::sleep(Duration::from_millis(5));
        }
    }

    fn visible_panes(&self) -> Result<HashSet<String>, TmuxError> {
        let output = self.output(&["list-clients", "-F", "#{pane_id}\u{1f}#{client_flags}\u{1f}#{@workbench_overlay_visible}\u{1f}#{@workbench_selected_implies_focused}"])?;
        Ok(output
            .lines()
            .filter_map(|line| {
                let mut fields = line.split('\u{1f}');
                let pane = fields.next()?;
                let flags = fields.next().unwrap_or_default();
                let overlay = fields.next().unwrap_or_default();
                let selected_compat = fields.next().unwrap_or_default();
                (pane.starts_with('%')
                    && (flags.split(',').any(|flag| flag == "focused") || selected_compat == "1")
                    && overlay != "1")
                    .then(|| pane.to_owned())
            })
            .collect())
    }
}

impl TmuxSource for Tmux {
    fn panes(&self) -> Result<Vec<Pane>, TmuxError> {
        let visible = self.visible_panes().unwrap_or_default();
        let format = [
            "#{session_id}",
            "#{session_name}",
            "#{window_id}",
            "#{window_index}",
            "#{window_name}",
            "#{pane_id}",
            "#{pane_index}",
            "#{pane_pid}",
            "#{pane_title}",
            "#{pane_current_command}",
            "#{@pane_role}",
            "#{window_active}",
            "#{pane_active}",
            "#{cursor_x}",
            "#{cursor_y}",
            "#{history_size}",
            "#{pane_last}",
            "#{pane_current_path}",
        ]
        .join(&FIELD_SEPARATOR.to_string());
        let output = self.output(&["list-panes", "-a", "-F", &format])?;
        let mut panes: Vec<_> = output
            .lines()
            .filter(|line| !line.is_empty())
            .map(|line| parse_pane(line, &visible))
            .collect::<Result<_, _>>()?;
        let visible_sessions: HashSet<_> = panes
            .iter()
            .filter(|pane| pane.visible)
            .map(|pane| pane.target.session_id.clone())
            .collect();
        let visible_sidebar_windows: HashSet<_> = panes
            .iter()
            .filter(|pane| pane.visible && pane.role.as_deref() == Some("sidebar"))
            .map(|pane| pane.target.window_id.clone())
            .collect();
        for pane in &mut panes {
            pane.session_visible = visible_sessions.contains(&pane.target.session_id);
            if pane.pane_last && visible_sidebar_windows.contains(&pane.target.window_id) {
                pane.visible = true;
            }
        }
        panes.retain(|pane| pane.role.as_deref() != Some("sidebar"));
        Ok(panes)
    }

    fn capture_bottom(
        &self,
        pane_id: &str,
        lines: usize,
        bytes: usize,
    ) -> Result<String, TmuxError> {
        if !valid_pane_id(pane_id) {
            return Err(TmuxError::InvalidRow(pane_id.to_owned()));
        }
        let lines = lines.clamp(1, 200);
        // capture-pane can monopolize the entire tmux server when a busy TUI
        // continuously redraws. Never let Agent observation stall interactive
        // tmux commands; the detector's stale grace handles a missed sample.
        let output = self.output_with_timeout(
            &[
                "capture-pane",
                "-p",
                "-t",
                pane_id,
                "-S",
                &format!("-{lines}"),
            ],
            Duration::from_millis(250),
        )?;
        Ok(tail_utf8(tail_lines(&output, lines), bytes.min(65_536)))
    }

    fn server_alive(&self) -> bool {
        self.server.socket_path.exists()
            && Command::new("tmux")
                .arg("-S")
                .arg(&self.server.socket_path)
                .arg("has-session")
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .status()
                .is_ok_and(|status| status.success())
    }
}

fn parse_pane(line: &str, visible: &HashSet<String>) -> Result<Pane, TmuxError> {
    let fields: Vec<_> = line.split(FIELD_SEPARATOR).collect();
    if fields.len() != 18 {
        return Err(TmuxError::InvalidRow(line.to_owned()));
    }
    let pane_id = fields[5].to_owned();
    if !valid_pane_id(&pane_id) {
        return Err(TmuxError::InvalidRow(line.to_owned()));
    }
    Ok(Pane {
        target: TmuxTarget {
            session_id: fields[0].to_owned(),
            session_name: sanitize_text(fields[1], 128),
            window_id: fields[2].to_owned(),
            window_index: fields[3]
                .parse()
                .map_err(|_| TmuxError::InvalidRow(line.to_owned()))?,
            window_name: sanitize_text(fields[4], 128),
            pane_id: pane_id.clone(),
            pane_index: fields[6]
                .parse()
                .map_err(|_| TmuxError::InvalidRow(line.to_owned()))?,
        },
        root_pid: fields[7]
            .parse()
            .map_err(|_| TmuxError::InvalidRow(line.to_owned()))?,
        title: sanitize_text(fields[8], 256),
        current_command: sanitize_text(fields[9], 256),
        current_path: sanitize_text(fields[17], 1024),
        role: (!fields[10].is_empty()).then(|| fields[10].to_owned()),
        visible: visible.contains(&pane_id),
        window_active: fields[11] == "1",
        pane_active: fields[12] == "1",
        pane_last: fields[16] == "1",
        session_visible: false,
        content_revision: format!("{}:{}:{}:{}", fields[8], fields[13], fields[14], fields[15]),
    })
}

fn sanitize_text(value: &str, max_bytes: usize) -> String {
    let mut result = String::new();
    for character in value.chars().filter(|character| !character.is_control()) {
        if result.len() + character.len_utf8() > max_bytes {
            break;
        }
        result.push(character);
    }
    result
}

fn valid_pane_id(value: &str) -> bool {
    value
        .strip_prefix('%')
        .is_some_and(|rest| !rest.is_empty() && rest.bytes().all(|byte| byte.is_ascii_digit()))
}

fn tail_lines(value: &str, max_lines: usize) -> &str {
    if max_lines == 0 {
        return "";
    }
    let mut newlines = 0;
    for (index, byte) in value.bytes().enumerate().rev() {
        if byte == b'\n' {
            newlines += 1;
            if newlines > max_lines {
                return &value[index + 1..];
            }
        }
    }
    value
}

fn tail_utf8(value: &str, max_bytes: usize) -> String {
    if value.len() <= max_bytes {
        return value.to_owned();
    }
    let mut start = value.len() - max_bytes;
    while !value.is_char_boundary(start) {
        start += 1;
    }
    value[start..].to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_inventory_and_visibility() {
        let row = "$1\u{1f}task\u{1f}@2\u{1f}3\u{1f}agent\u{1f}%4\u{1f}0\u{1f}123\u{1f}title\u{1f}codex\u{1f}\u{1f}1\u{1f}1\u{1f}8\u{1f}9\u{1f}10\u{1f}0\u{1f}/tmp/task";
        let pane = parse_pane(row, &HashSet::from(["%4".into()])).unwrap();
        assert_eq!(pane.target.session_name, "task");
        assert_eq!(pane.root_pid, 123);
        assert!(pane.visible);
        assert_eq!(pane.content_revision, "title:8:9:10");
        assert_eq!(pane.current_path, "/tmp/task");
    }

    #[test]
    fn rejects_injected_pane_target() {
        assert!(!valid_pane_id("%1; run-shell evil"));
    }

    #[test]
    fn byte_limit_preserves_utf8_boundary() {
        assert_eq!(tail_utf8("abc你好", 4), "好");
    }

    #[test]
    fn line_limit_keeps_only_bottom_lines() {
        assert_eq!(tail_lines("one\ntwo\nthree\nfour\n", 2), "three\nfour\n");
    }

    #[test]
    fn display_metadata_is_control_free_and_utf8_bounded() {
        assert_eq!(sanitize_text("build\u{1b}[31m", 32), "build[31m");
        assert_eq!(sanitize_text("你好world", 7), "你好w");
    }
}
