use std::collections::{HashMap, HashSet};
use std::io::{self, Read};
use std::os::unix::process::CommandExt;
use std::process::Stdio;
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};
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
    #[error("background tmux sampling interrupted by an interactive request")]
    Interrupted,
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
    interactive_waiters: Option<Arc<AtomicUsize>>,
    sampling_interruptible: bool,
}

pub(crate) struct PaneProbe {
    pub panes: Vec<Pane>,
    pub clients: String,
    pub captures: HashMap<String, String>,
    pub surface: Option<SurfaceInventory>,
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub(crate) struct SurfaceInventory {
    pub rows: String,
    pub titles: String,
    pub visibility: HashMap<String, bool>,
    pub palette: String,
    pub appearance: String,
    pub mode: String,
    pub accent: String,
    #[serde(default)]
    pub restore_token: String,
    #[serde(default)]
    pub last_panes: HashMap<String, String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sidebar: Option<crate::sidebar::SharedSidebarData>,
}

impl Tmux {
    pub(crate) fn server(&self) -> &ServerIdentity {
        &self.server
    }

    pub fn new(server: ServerIdentity) -> Self {
        Self {
            server,
            interactive_waiters: None,
            sampling_interruptible: false,
        }
    }

    pub(crate) fn set_interactive_waiters(&mut self, waiters: Arc<AtomicUsize>) {
        self.interactive_waiters = Some(waiters);
    }

    pub(crate) fn set_sampling_interruptible(&mut self, enabled: bool) {
        self.sampling_interruptible = enabled;
    }

    fn interactive_waiting(&self) -> bool {
        self.sampling_interruptible
            && self
                .interactive_waiters
                .as_ref()
                .is_some_and(|waiters| waiters.load(Ordering::SeqCst) != 0)
    }

    /// `display-message -c` selects a message destination, not a pane context.
    /// Evaluate client-relative formats inside list-clients, then match exactly.
    pub fn client_format(&self, name: &str, format: &str) -> Result<Option<String>, TmuxError> {
        let separator = format!("__workbench_client_{}__", uuid::Uuid::new_v4().simple());
        let format = format!("#{{client_name}}{separator}{format}");
        let rows = self.output(&["list-clients", "-F", &format])?;
        Ok(rows
            .lines()
            .filter_map(|row| row.split_once(&separator))
            .find(|(client, _)| *client == name)
            .map(|(_, value)| value.to_owned()))
    }

    pub(crate) fn publish_agent_actions(
        &self,
        panes: &[Pane],
        roots: &HashSet<u32>,
    ) -> Result<(), TmuxError> {
        if panes.is_empty() {
            return Ok(());
        }
        let mut args = Vec::<String>::new();
        for pane in panes {
            if !args.is_empty() {
                args.push(";".into());
            }
            args.extend([
                "set-option".into(),
                "-pq".into(),
                "-t".into(),
                pane.target.pane_id.clone(),
                "@workbench_agent_actions".into(),
                if roots.contains(&pane.root_pid) {
                    "1"
                } else {
                    "0"
                }
                .into(),
            ]);
        }
        let mut windows = std::collections::BTreeMap::<&str, bool>::new();
        for pane in panes {
            *windows.entry(&pane.target.window_id).or_default() |= roots.contains(&pane.root_pid);
        }
        for (window, has_agent) in windows {
            args.extend([
                ";".into(),
                "set-option".into(),
                "-wq".into(),
                "-t".into(),
                window.into(),
                "@workbench_window_has_agent".into(),
                if has_agent { "1" } else { "0" }.into(),
            ]);
        }
        for pane in panes
            .iter()
            .filter(|pane| pane.pane_active && roots.contains(&pane.root_pid))
        {
            args.extend([
                ";".into(),
                "set-option".into(),
                "-wq".into(),
                "-t".into(),
                pane.target.window_id.clone(),
                "@workbench_last_agent_pane".into(),
                pane.target.pane_id.clone(),
            ]);
        }
        self.output(&args.iter().map(String::as_str).collect::<Vec<_>>())?;
        Ok(())
    }

    fn output(&self, args: &[&str]) -> Result<String, TmuxError> {
        let output = crate::command::new("tmux")
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

    pub(crate) fn output_with_timeout(
        &self,
        args: &[&str],
        timeout: Duration,
    ) -> Result<String, TmuxError> {
        if self.interactive_waiting() {
            return Err(TmuxError::Interrupted);
        }
        let mut child = crate::command::new("tmux")
            .arg("-S")
            .arg(&self.server.socket_path)
            .args(args)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .process_group(0)
            .spawn()?;
        // Drain both pipes while waiting. A batched screen capture may exceed
        // the OS pipe capacity, and waiting before reading would deadlock.
        let mut stdout = child.stdout.take().expect("stdout piped");
        let mut stderr = child.stderr.take().expect("stderr piped");
        let stdout_reader = std::thread::spawn(move || {
            let mut bytes = Vec::new();
            stdout.read_to_end(&mut bytes).map(|_| bytes)
        });
        let stderr_reader = std::thread::spawn(move || {
            let mut bytes = Vec::new();
            stderr.read_to_end(&mut bytes).map(|_| bytes)
        });
        let deadline = Instant::now() + timeout;
        loop {
            if let Some(status) = child.try_wait()? {
                let stdout = stdout_reader.join().expect("stdout reader")?;
                let stderr = stderr_reader.join().expect("stderr reader")?;
                if !status.success() {
                    return Err(TmuxError::Command(
                        String::from_utf8_lossy(&stderr).trim().to_owned(),
                    ));
                }
                return Ok(String::from_utf8_lossy(&stdout).into_owned());
            }
            let interrupted = self.interactive_waiting();
            if interrupted || Instant::now() >= deadline {
                // The child owns a process group so wrappers cannot leave a
                // descendant holding the output pipes open after cancellation.
                unsafe {
                    libc::kill(-(child.id() as libc::pid_t), libc::SIGKILL);
                }
                let _ = child.kill();
                let _ = child.wait();
                let _ = stdout_reader.join();
                let _ = stderr_reader.join();
                return Err(if interrupted {
                    TmuxError::Interrupted
                } else {
                    TmuxError::Timeout(timeout.as_millis() as u64)
                });
            }
            crate::compat::sleep(Duration::from_millis(5));
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

    /// One tmux client supplies focus, pane inventory and all due captures.
    /// The number of foreground agents therefore does not multiply idle forks.
    pub(crate) fn probe(
        &self,
        capture_panes: &[String],
        lines: usize,
        bytes: usize,
        with_surface: bool,
    ) -> Result<PaneProbe, TmuxError> {
        self.probe_with_timeout(
            capture_panes,
            lines,
            bytes,
            with_surface,
            Some(Duration::from_secs(1)),
        )
    }

    /// Share a bounded periodic inventory with sidebars while an agent is
    /// active. Explicit user operations still use the normal `panes` query.
    pub(crate) fn surface_probe(&self) -> Result<PaneProbe, TmuxError> {
        self.probe_with_timeout(&[], 1, 1, true, Some(Duration::from_secs(1)))
    }

    /// After a timed-out inventory, allow bounded recovery queries more time.
    /// They carry no screen captures and still share the global cadence.
    pub(crate) fn recovery_surface_probe(&self) -> Result<PaneProbe, TmuxError> {
        self.probe_with_timeout(&[], 1, 1, true, Some(Duration::from_secs(2)))
    }

    fn probe_with_timeout(
        &self,
        capture_panes: &[String],
        lines: usize,
        bytes: usize,
        with_surface: bool,
        timeout: Option<Duration>,
    ) -> Result<PaneProbe, TmuxError> {
        for pane in capture_panes {
            if !valid_pane_id(pane) {
                return Err(TmuxError::InvalidRow(pane.clone()));
            }
        }
        let lines = lines.clamp(1, 200);
        // tmux 3.4 may escape control bytes in batched command output. Use a
        // per-query printable separator on the wire and normalize internally.
        let separator = format!("__workbench_field_{}__", uuid::Uuid::new_v4().simple());
        let client_format = format!(
            "C{separator}{}",
            [
                "#{client_tty}",
                "#{client_flags}",
                "#{@workbench_overlay_visible}",
                "#{session_id}",
                "#{session_name}",
                "#{window_id}",
                "#{window_index}",
                "#{window_name}",
                "#{pane_id}",
                "#{pane_index}",
                "#{@workbench_selected_implies_focused}",
            ]
            .join(&separator)
        );
        let pane_format = [
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
        .join(&separator);
        // The same pane inventory feeds detection and all sidebars. A unique
        // separator keeps sidebar-only fields distinct from the original pane
        // row without a second list-panes query.
        let surface_marker = format!("__workbench_surface_{}__", uuid::Uuid::new_v4().simple());
        let format = if with_surface {
            let extra = [
                "#{@agent_team}",
                "#{pane_dead}",
                "#{@agent_focus}",
                "#{@workbench_window_state}",
                "#{@workbench_assistant}",
                "#{@workbench_session_focus}",
                "#{@workbench_task_title}",
                "#{session_attached}",
                "#{window_zoomed_flag}",
            ]
            .join(&separator);
            format!("P{separator}{pane_format}{surface_marker}{extra}")
        } else {
            format!("P{separator}{pane_format}")
        };
        let marker = format!("__workbench_capture_{}__", uuid::Uuid::new_v4().simple());
        let mut args = vec![
            "list-clients".to_owned(),
            "-F".into(),
            client_format,
            ";".into(),
            "list-panes".into(),
            "-a".into(),
            "-F".into(),
            format,
        ];
        if with_surface {
            let theme_format = format!(
                "T{separator}{}",
                [
                    "#{@adaptive_palette}",
                    "#{@adaptive_appearance}",
                    "#{@adaptive_mode}",
                    "#{@adaptive_accent}",
                    "#{@workbench_team_restore_token}",
                ]
                .join(&separator)
            );
            args.extend([
                ";".into(),
                "display-message".into(),
                "-p".into(),
                theme_format,
            ]);
        }
        for pane in capture_panes {
            args.extend([
                ";".into(),
                "display-message".into(),
                "-p".into(),
                // display-message applies strftime expansion: a raw pane id
                // such as %1 would lose its percent sign here.
                format!("{marker}{}", &pane[1..]),
                ";".into(),
                "capture-pane".into(),
                "-p".into(),
                "-t".into(),
                pane.clone(),
                "-S".into(),
                format!("-{lines}"),
            ]);
        }
        let args = args.iter().map(String::as_str).collect::<Vec<_>>();
        let output = match timeout {
            Some(timeout) => self.output_with_timeout(&args, timeout),
            None => self.output(&args),
        }?;
        let mut client_rows = String::new();
        let mut visibility_rows = String::new();
        let mut pane_rows = Vec::new();
        let mut captures = HashMap::<String, String>::new();
        let mut surface_valid = true;
        let mut surface = with_surface.then(|| SurfaceInventory {
            rows: String::new(),
            titles: String::new(),
            visibility: HashMap::new(),
            palette: String::new(),
            appearance: String::new(),
            mode: String::new(),
            accent: String::new(),
            restore_token: String::new(),
            last_panes: HashMap::new(),
            sidebar: None,
        });
        let mut current = None;
        for line in output.split_inclusive('\n') {
            let value = line.trim_end_matches('\n');
            if let Some(suffix) = value.strip_prefix(&marker) {
                let pane = format!("%{suffix}");
                if !valid_pane_id(&pane) {
                    return Err(TmuxError::InvalidRow(value.to_owned()));
                }
                current = Some(pane.clone());
                captures.insert(pane, String::new());
            } else if let Some(pane) = &current {
                captures
                    .get_mut(pane)
                    .expect("capture present")
                    .push_str(line);
            } else if let Some(row) = value.strip_prefix(&format!("C{separator}")) {
                let fields: Vec<_> = row.split(&separator).collect();
                if fields.len() != 11 {
                    return Err(TmuxError::InvalidRow(row.to_owned()));
                }
                visibility_rows.push_str(&format!(
                    "{}{FIELD_SEPARATOR}{}{FIELD_SEPARATOR}{}{FIELD_SEPARATOR}{}\n",
                    fields[8], fields[1], fields[2], fields[10]
                ));
                client_rows.push_str(&fields[..10].join(&FIELD_SEPARATOR.to_string()));
                client_rows.push('\n');
            } else if let Some(row) = value.strip_prefix(&format!("P{separator}")) {
                if let Some(surface) = surface.as_mut() {
                    let Some((pane_row, extra_row)) = row.split_once(&surface_marker) else {
                        return Err(TmuxError::InvalidRow(row.to_owned()));
                    };
                    let pane: Vec<_> = pane_row.split(&separator).collect();
                    let extra: Vec<_> = extra_row.split(&separator).collect();
                    pane_rows.push(pane.join(&FIELD_SEPARATOR.to_string()));
                    if pane.len() != 18 || extra.len() != 9 {
                        // Preserve detection when a sidebar-only option
                        // cannot be represented by the inventory format.
                        surface_valid = false;
                        continue;
                    }
                    surface.rows.push_str(
                        &[
                            pane[2], pane[5], extra[0], extra[1], extra[2], pane[0], pane[1],
                            pane[4], pane[3], pane[12], extra[3], pane[10], extra[4], extra[5],
                        ]
                        .join("\t"),
                    );
                    surface.rows.push('\n');
                    surface.titles.push_str(&format!(
                        "{}{FIELD_SEPARATOR}{}{FIELD_SEPARATOR}{}{FIELD_SEPARATOR}{}\n",
                        pane[2], extra[6], pane[5], pane[8]
                    ));
                    surface.visibility.insert(
                        pane[5].to_owned(),
                        sidebar_is_visible(&[pane[11], extra[7], extra[8], pane[12]].join(":")),
                    );
                    if pane[10] != "sidebar" && pane[16] == "1" {
                        surface
                            .last_panes
                            .insert(pane[2].to_owned(), pane[5].to_owned());
                    }
                } else {
                    pane_rows.push(
                        row.split(&separator)
                            .collect::<Vec<_>>()
                            .join(&FIELD_SEPARATOR.to_string()),
                    );
                }
            } else if let Some(row) = value.strip_prefix(&format!("T{separator}")) {
                let fields: Vec<_> = row.split(&separator).collect();
                if fields.len() != 5 {
                    return Err(TmuxError::InvalidRow(row.to_owned()));
                }
                let surface = surface.as_mut().expect("surface requested");
                surface.palette = fields[0].to_owned();
                surface.appearance = fields[1].to_owned();
                surface.mode = fields[2].to_owned();
                surface.accent = fields[3].to_owned();
                surface.restore_token = fields[4].to_owned();
            } else if !value.is_empty() {
                return Err(TmuxError::InvalidRow(value.to_owned()));
            }
        }
        let clients = parse_client_panes(&visibility_rows);
        let visible = clients.unobscured();
        let mut panes: Vec<_> = pane_rows
            .into_iter()
            .map(|row| parse_pane(&row, &visible))
            .collect::<Result<_, _>>()?;
        apply_client_visibility(&mut panes, &clients);
        panes.retain(|pane| pane.role.as_deref() != Some("sidebar"));
        for content in captures.values_mut() {
            *content = tail_utf8(tail_lines(content, lines), bytes.min(65_536));
        }
        if !surface_valid {
            surface = None;
        }
        Ok(PaneProbe {
            panes,
            clients: client_rows,
            captures,
            surface,
        })
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
        // Explicit user operations also refresh pane targets. Preserve their
        // original inventory behavior on busy hosts; the one-second limit applies
        // only to the best-effort idle screen-capture probe.
        Ok(self.probe_with_timeout(&[], 1, 1, false, None)?.panes)
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
            && crate::command::new("tmux")
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
    fn interactive_inventory_does_not_use_idle_probe_timeout() {
        use std::os::unix::fs::PermissionsExt;

        const CHILD: &str = "WORKBENCH_SLOW_TMUX_TEST_CHILD";
        if let Some(directory) = std::env::var_os(CHILD) {
            let socket = std::path::PathBuf::from(directory).join("tmux.sock");
            let tmux = Tmux::new(ServerIdentity::from_socket(socket).unwrap());
            assert!(tmux.panes().unwrap().is_empty());
            assert!(matches!(
                tmux.probe(&[], 1, 1, false),
                Err(TmuxError::Timeout(1_000))
            ));
            return;
        }

        let directory = tempfile::tempdir().unwrap();
        let executable = directory.path().join("tmux");
        std::fs::write(&executable, "#!/bin/sh\nexec /bin/sleep 1.2\n").unwrap();
        std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o755)).unwrap();
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "tmux::tests::interactive_inventory_does_not_use_idle_probe_timeout",
            ])
            .env("PATH", directory.path())
            .env(CHILD, directory.path())
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "child failed: {}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }

    #[test]
    fn batched_probe_captures_multiple_real_panes() {
        if crate::command::new("tmux").arg("-V").output().is_err() {
            return;
        }
        let dir = tempfile::Builder::new()
            .prefix("wb-probe-")
            .tempdir_in("/tmp")
            .unwrap();
        let socket = dir.path().join("tmux.sock");
        struct ServerCleanup(std::path::PathBuf);
        impl Drop for ServerCleanup {
            fn drop(&mut self) {
                let _ = crate::command::new("tmux")
                    .arg("-S")
                    .arg(&self.0)
                    .arg("kill-server")
                    .output();
            }
        }
        let _cleanup = ServerCleanup(socket.clone());
        let run = |args: &[&str]| {
            let output = crate::command::new("tmux")
                .arg("-S")
                .arg(&socket)
                .args(args)
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
            String::from_utf8(output.stdout).unwrap().trim().to_owned()
        };
        let first = run(&[
            "-f",
            "/dev/null",
            "new-session",
            "-d",
            "-P",
            "-F",
            "#{pane_id}",
            "-s",
            "probe",
            "printf 'workbench-first\\n'; sleep 30",
        ]);
        let second = run(&[
            "new-window",
            "-d",
            "-P",
            "-F",
            "#{pane_id}",
            "-t",
            "probe:",
            "printf 'workbench-second\\n'; sleep 30",
        ]);
        run(&["set-option", "-p", "-t", &first, "@agent_team", "team-one"]);
        run(&["set-option", "-p", "-t", &first, "@agent_focus", "1"]);
        run(&[
            "set-option",
            "-g",
            "@workbench_team_restore_token",
            "restore-test",
        ]);
        let tmux = Tmux::new(ServerIdentity::from_socket(socket.clone()).unwrap());
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let probe = tmux
                .probe(&[first.clone(), second.clone()], 40, 4096, true)
                .unwrap();
            if probe
                .captures
                .get(&first)
                .is_some_and(|s| s.contains("workbench-first"))
                && probe
                    .captures
                    .get(&second)
                    .is_some_and(|s| s.contains("workbench-second"))
            {
                let surface = probe.surface.as_ref().expect("shared surface");
                assert!(surface.rows.lines().any(|row| {
                    let fields: Vec<_> = row.split('\t').collect();
                    fields.len() == 14
                        && fields[1] == first
                        && fields[2] == "team-one"
                        && fields[4] == "1"
                }));
                assert_eq!(surface.restore_token, "restore-test");
                assert_eq!(probe.panes.len(), 2);
                break;
            }
            assert!(
                Instant::now() < deadline,
                "batched pane captures did not arrive"
            );
            std::thread::sleep(Duration::from_millis(25));
        }

        // Only the actual previous non-sidebar pane is a selection target.
        // The active pane must not become an implicit fallback when the last
        // pane is the sidebar, otherwise a newly opened sidebar jumps down.
        let window = run(&["display-message", "-p", "-t", &first, "#{window_id}"]);
        let sidebar = run(&[
            "split-window",
            "-d",
            "-P",
            "-F",
            "#{pane_id}",
            "-t",
            &first,
            "sleep 30",
        ]);
        run(&["set-option", "-p", "-t", &sidebar, "@pane_role", "sidebar"]);
        run(&["select-pane", "-t", &sidebar]);
        run(&["select-pane", "-t", &first]);
        let surface = tmux.surface_probe().unwrap().surface.unwrap();
        assert!(!surface.last_panes.contains_key(&window));
        run(&["select-pane", "-t", &sidebar]);
        let surface = tmux.surface_probe().unwrap().surface.unwrap();
        assert_eq!(surface.last_panes.get(&window), Some(&first));

        assert!(
            tmux.probe(&["%1;kill-server".into()], 40, 4096, true)
                .is_err()
        );
    }

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
