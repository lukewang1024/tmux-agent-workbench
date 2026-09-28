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

    /// A sidebar can be visible without being focused. Zoomed-away panes and
    /// windows in detached sessions do not need foreground refresh cadence.
    pub fn sidebar_visible(&self, pane: &str) -> Result<bool, TmuxError> {
        let output = self.output_with_timeout(
            &[
                "display-message",
                "-p",
                "-t",
                pane,
                "#{window_active}:#{session_attached}:#{window_zoomed_flag}:#{pane_active}",
            ],
            Duration::from_millis(250),
        )?;
        Ok(sidebar_is_visible(output.trim()))
    }

    fn visible_panes(&self) -> Result<ClientPanes, TmuxError> {
        let output = self.output(&["list-clients", "-F", "#{pane_id}\u{1f}#{client_flags}\u{1f}#{@workbench_overlay_visible}\u{1f}#{@workbench_selected_implies_focused}"])?;
        Ok(parse_client_panes(&output))
    }
}

#[derive(Debug, Default)]
struct ClientPanes {
    focused: HashSet<String>,
    overlays: HashSet<String>,
}

impl ClientPanes {
    fn unobscured(&self) -> HashSet<String> {
        self.focused.difference(&self.overlays).cloned().collect()
    }
}

impl TmuxSource for Tmux {
    fn panes(&self) -> Result<Vec<Pane>, TmuxError> {
        let clients = self.visible_panes().unwrap_or_default();
        let visible = clients.unobscured();
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
        apply_client_visibility(&mut panes, &clients);
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

fn parse_client_panes(output: &str) -> ClientPanes {
    let mut clients = ClientPanes::default();
    for line in output.lines() {
        let mut fields = line.split('\u{1f}');
        let Some(pane) = fields.next() else {
            continue;
        };
        let flags = fields.next().unwrap_or_default();
        let overlay = fields.next().unwrap_or_default();
        let selected_compat = fields.next().unwrap_or_default();
        if !pane.starts_with('%')
            || (!flags.split(',').any(|flag| flag == "focused") && selected_compat != "1")
        {
            continue;
        }
        clients.focused.insert(pane.to_owned());
        if overlay == "1" {
            clients.overlays.insert(pane.to_owned());
        }
    }
    clients
}

fn apply_client_visibility(panes: &mut [Pane], clients: &ClientPanes) {
    let visible_sidebar_windows: HashSet<_> = panes
        .iter()
        // An overlay sidebar is intentionally absent from `visible`, but its
        // focused client still identifies the window whose pane_last target
        // remains visible underneath it.
        .filter(|pane| {
            clients.focused.contains(&pane.target.pane_id)
                && pane.role.as_deref() == Some("sidebar")
        })
        .map(|pane| pane.target.window_id.clone())
        .collect();
    for pane in panes.iter_mut() {
        // pane_active is the authoritative result of a tmux focus change. It
        // must reach the detector even when the client still carries the
        // responsive overlay marker; otherwise focusing a done agent from
        // the sidebar cannot acknowledge its attention.
        if pane.window_active
            && pane.pane_active
            && clients.focused.contains(&pane.target.pane_id)
            && pane.role.as_deref() != Some("sidebar")
        {
            pane.visible = true;
        }
        if pane.pane_last && visible_sidebar_windows.contains(&pane.target.window_id) {
            pane.visible = true;
        }
    }
    let visible_sessions: HashSet<_> = panes
        .iter()
        .filter(|pane| pane.visible)
        .map(|pane| pane.target.session_id.clone())
        .collect();
    for pane in panes.iter_mut() {
        pane.session_visible = visible_sessions.contains(&pane.target.session_id);
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

fn sidebar_is_visible(state: &str) -> bool {
    let fields: Vec<_> = state.split(':').collect();
    fields.len() == 4
        && fields[0] == "1"
        && fields[1].parse::<u32>().is_ok_and(|attached| attached > 0)
        && (fields[2] == "0" || fields[3] == "1")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sidebar_visibility_includes_unfocused_but_excludes_hidden_panes() {
        assert!(sidebar_is_visible("1:1:0:0"));
        assert!(sidebar_is_visible("1:2:1:1"));
        assert!(!sidebar_is_visible("0:1:0:1"));
        assert!(!sidebar_is_visible("1:0:0:1"));
        assert!(!sidebar_is_visible("1:1:1:0"));
        assert!(!sidebar_is_visible(""));
    }

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
    fn focused_overlay_sidebar_keeps_pane_last_agent_visible() {
        let clients = parse_client_panes("%8\u{1f}attached,focused,UTF-8\u{1f}1\u{1f}\n");
        assert_eq!(clients.focused, HashSet::from(["%8".into()]));
        assert_eq!(clients.unobscured(), HashSet::new());

        let sidebar = parse_pane(
            "$1\u{1f}task\u{1f}@2\u{1f}1\u{1f}agent\u{1f}%8\u{1f}0\u{1f}123\u{1f}sidebar\u{1f}tmux-agent-workbench\u{1f}sidebar\u{1f}1\u{1f}1\u{1f}0\u{1f}0\u{1f}0\u{1f}0\u{1f}/tmp/task",
            &clients.unobscured(),
        )
        .unwrap();
        let agent = parse_pane(
            "$1\u{1f}task\u{1f}@2\u{1f}1\u{1f}agent\u{1f}%4\u{1f}1\u{1f}456\u{1f}title\u{1f}codex\u{1f}\u{1f}1\u{1f}0\u{1f}8\u{1f}9\u{1f}10\u{1f}1\u{1f}/tmp/task",
            &clients.unobscured(),
        )
        .unwrap();
        let mut panes = vec![sidebar, agent];

        apply_client_visibility(&mut panes, &clients);

        assert!(!panes[0].visible);
        assert!(panes[1].visible);
        assert!(panes[1].session_visible);
    }

    #[test]
    fn active_pane_is_visible_even_with_responsive_overlay_marker() {
        let clients = parse_client_panes("%4\u{1f}attached,focused,UTF-8\u{1f}1\u{1f}\n");
        let mut panes = vec![
            parse_pane(
                "$1\u{1f}task\u{1f}@2\u{1f}1\u{1f}agent\u{1f}%4\u{1f}0\u{1f}456\u{1f}title\u{1f}codex\u{1f}\u{1f}1\u{1f}1\u{1f}8\u{1f}9\u{1f}10\u{1f}0\u{1f}/tmp/task",
                &clients.unobscured(),
            )
            .unwrap(),
        ];

        apply_client_visibility(&mut panes, &clients);

        assert!(panes[0].visible);
        assert!(panes[0].session_visible);
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
