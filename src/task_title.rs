//! Task titles are assigned once to a verified agent window. Lifecycle hooks
//! request a semantic title; tmux owns the durable owner and repair budget.
use std::fs::{File, OpenOptions};
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;
use std::process::Command;
use std::time::{Duration, Instant};

use fs2::FileExt;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sysinfo::{Pid, ProcessRefreshKind, ProcessesToUpdate, System};

use crate::ipc::{Request, call};
use crate::model::{AgentKind, AgentSnapshot, ProcessFingerprint, Snapshot};
use crate::paths::Paths;
use crate::server::ServerIdentity;

const STATE_OPTION: &str = "@workbench_task_title";

fn socket_access_error(detail: &str) -> String {
    format!(
        "Workbench socket access denied (sandbox/OS permissions): {detail}. Keep the current pane. If your agent permits approval, request approved execution of this exact title command through its permission mechanism. Otherwise skip naming and continue the task; do not change sandbox/network policy or recreate the pane."
    )
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct Owner {
    pane: String,
    session: Option<String>,
    process: ProcessFingerprint,
}

impl Owner {
    fn of(agent: &AgentSnapshot) -> Result<Self, String> {
        Ok(Self {
            pane: agent.target.pane_id.clone(),
            session: agent.hook_session_id.clone(),
            process: agent.process.clone().ok_or("agent process unavailable")?,
        })
    }

    fn same_task(&self, other: &Self) -> bool {
        self.pane == other.pane
            && match (&self.session, &other.session) {
                (Some(a), Some(b)) => a == b,
                _ => self.process == other.process,
            }
    }
}

#[derive(Debug, Serialize, Deserialize)]
struct TitleState {
    owner: Owner,
    initial_name: String,
    title: Option<String>,
    #[serde(default)]
    name_from_command: bool,
    task_seen: bool,
    repair_used: bool,
    #[serde(default)]
    original_options: Option<TitleOptions>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    visible_session: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
struct TitleOptions {
    // None means the window inherited its automatic-rename setting.
    automatic_rename: Option<bool>,
}

struct Window {
    id: String,
    name: String,
    protected: bool,
    automatic_rename: bool,
    task_name: Option<String>,
}

fn tmux(server: &ServerIdentity, args: &[&str]) -> Result<String, String> {
    let output = Command::new("tmux")
        .arg("-u")
        .args(server.tmux_args())
        .args(args)
        .env("LC_ALL", "C")
        .output()
        .map_err(|error| error.to_string())?;
    if !output.status.success() {
        let error = String::from_utf8_lossy(&output.stderr);
        return Err(if crate::paths::permission_denied_message(&error) {
            socket_access_error(error.trim())
        } else {
            error.trim().into()
        });
    }
    Ok(String::from_utf8_lossy(&output.stdout)
        .trim_end_matches('\n')
        .into())
}

fn window(server: &ServerIdentity, agent: &AgentSnapshot) -> Result<Window, String> {
    let (pane, window) = window_at(server, &agent.target.pane_id)?;
    if pane != agent.target.pane_id || window.id != agent.target.window_id {
        return Err("agent pane moved or expired; retry with its current identity".into());
    }
    Ok(window)
}

fn window_at(server: &ServerIdentity, target: &str) -> Result<(String, Window), String> {
    let value = tmux(
        server,
        &[
            "display-message",
            "-p",
            "-t",
            target,
            "#{pane_id}\u{1f}#{window_id}\u{1f}#{window_name}\u{1f}#{@workbench_task}\u{1f}#{@workbench_project_root}\u{1f}#{@pane_role}\u{1f}#{@workbench-window-automatic-rename}\u{1f}#{automatic-rename}\u{1f}#{@workbench_task_root}\u{1f}#{@workbench_task_name}",
        ],
    )?;
    let fields: Vec<_> = value.split('\u{1f}').collect();
    if fields.len() != 10 || !fields[1].starts_with('@') {
        return Err("agent pane moved or expired; retry with its current identity".into());
    }
    Ok((
        fields[0].into(),
        Window {
            id: fields[1].into(),
            name: fields[2].into(),
            protected: (!fields[3].is_empty() && fields[3] != "0" && fields[8].is_empty())
                || !fields[4].is_empty()
                || (target.starts_with('%') && matches!(fields[5], "sidebar" | "task"))
                || fields[6] == "on",
            automatic_rename: fields[7] == "1",
            task_name: (!fields[8].is_empty()).then(|| fields[9].to_owned()),
        },
    ))
}

fn generic_name(name: &str) -> bool {
    let name = name.trim().to_ascii_lowercase();
    name.is_empty()
        || matches!(
            name.as_str(),
            "tmux"
                | "agent"
                | "shell"
                | "terminal"
                | "sh"
                | "zsh"
                | "bash"
                | "dash"
                | "fish"
                | "ksh"
                | "nu"
                | "codex"
                | "claude"
                | "trae"
                | "traex"
                | "opencode"
                | "node"
                | "python"
        )
        || name.strip_prefix("python").is_some_and(|suffix| {
            !suffix.is_empty() && suffix.bytes().all(|b| b.is_ascii_digit() || b == b'.')
        })
}

fn validate_title(title: &str) -> Result<&str, String> {
    let title = title.trim();
    if generic_name(title)
        || title.chars().count() > 64
        || title.chars().any(char::is_control)
        || title.contains("#{")
        || title.contains("#[")
    {
        return Err(
            "use a short task name (1–64 characters), without controls or tmux formats".into(),
        );
    }
    Ok(title)
}

// Both tmux command strings and the shell accept this literal single quoting.
fn quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

fn format_literal(value: &str) -> String {
    value
        .replace('#', "##")
        .replace(',', "#,")
        .replace('}', "#}")
}

fn automatic_rename(server: &ServerIdentity, window: &str) -> Result<Option<bool>, String> {
    match tmux(
        server,
        &["show-option", "-wqv", "-t", window, "automatic-rename"],
    )?
    .as_str()
    {
        "" => Ok(None),
        "on" => Ok(Some(true)),
        "off" => Ok(Some(false)),
        _ => Err("invalid window automatic-rename setting".into()),
    }
}

fn window_lock(paths: &Paths, server: &ServerIdentity, window: &str) -> Result<File, String> {
    window_lock_wait(paths, server, window, Duration::ZERO)
}

fn window_lock_wait(
    paths: &Paths,
    server: &ServerIdentity,
    window: &str,
    wait: Duration,
) -> Result<File, String> {
    let lock_name = format!(
        "task-title-{}-{}.lock",
        server.key,
        window.trim_start_matches('@')
    );
    let lock = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .mode(0o600)
        .open(paths.runtime_dir.join(lock_name))
        .map_err(|error| error.to_string())?;
    let started = Instant::now();
    loop {
        match lock.try_lock_exclusive() {
            Ok(()) => return Ok(lock),
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                if started.elapsed() >= wait {
                    return Err("task title is being checked; retry shortly".into());
                }
                std::thread::sleep(Duration::from_millis(10));
            }
            Err(error) => return Err(error.to_string()),
        }
    }
}

fn state(server: &ServerIdentity, window: &Window) -> Result<Option<TitleState>, String> {
    let value = tmux(
        server,
        &["show-option", "-wqv", "-t", &window.id, STATE_OPTION],
    )?;
    if value.is_empty() {
        return Ok(None);
    }
    serde_json::from_str(&value)
        .map(Some)
        .map_err(|error| format!("invalid task title state: {error}"))
}

fn save(server: &ServerIdentity, window: &Window, state: &TitleState) -> Result<(), String> {
    let value = serde_json::to_string(state).map_err(|error| error.to_string())?;
    tmux(
        server,
        &["set-option", "-w", "-t", &window.id, STATE_OPTION, &value],
    )?;
    Ok(())
}

/// The detector's latest hook session can belong to a helper thread launched
/// inside the same CLI. Keep an issued naming binding attached to its process;
/// a visible Codex thread ID can also recover bindings written by older builds.
fn foreground_agent(
    snapshot: &Snapshot,
    agent: &AgentSnapshot,
    requested: Option<&str>,
    clear: bool,
) -> Result<AgentSnapshot, String> {
    let server = ServerIdentity::from_socket(snapshot.server.clone().into())
        .map_err(|error| error.to_string())?;
    let current = window(&server, agent)?;
    let previous = state(&server, &current)?;
    let bound = previous.as_ref().filter(|state| {
        state.owner.pane == agent.target.pane_id
            && Some(&state.owner.process) == agent.process.as_ref()
    });
    let title = if agent.kind == AgentKind::Codex {
        tmux(
            &server,
            &[
                "display-message",
                "-p",
                "-t",
                &agent.target.pane_id,
                "#{pane_title}",
            ],
        )?
    } else {
        String::new()
    };
    let visible = crate::state_machine::codex_title_identity(&title);
    let remembered = bound.and_then(|state| state.owner.session.as_deref());
    let session = requested.or_else(|| {
        remembered
            .filter(|id| visible.is_none_or(|prefix| id.starts_with(prefix)))
            .or(agent.hook_session_id.as_deref())
    });
    let Some(session) = session else {
        return Ok(agent.clone());
    };
    let latest = agent.hook_session_id.as_deref() == Some(session);
    // SessionStart(clear) runs before Codex redraws its new thread's OSC title.
    // The accepted hook already binds this session to the live CLI process.
    let verified = if clear && latest {
        true
    } else if let Some(prefix) = visible {
        session.starts_with(prefix) && (bound.is_some() || latest)
    } else if let Some(remembered) = remembered {
        remembered == session || (clear && latest)
    } else {
        latest
    };
    if !verified {
        return Err(format!(
            "session does not match the verified foreground task in pane {}; an auxiliary or stale session cannot rename its window",
            agent.target.pane_id
        ));
    }
    let mut foreground = agent.clone();
    foreground.hook_session_id = Some(session.into());
    Ok(foreground)
}

fn owner_state(
    snapshot: &Snapshot,
    agent: &AgentSnapshot,
    window: &Window,
    previous: Option<TitleState>,
) -> Result<TitleState, String> {
    let owners: Vec<_> = snapshot
        .agents
        .iter()
        .filter(|candidate| {
            !candidate.exited
                && candidate.relay_focus.is_none()
                && candidate.target.window_id == window.id
                && candidate.process.is_some()
        })
        .filter(|candidate| {
            let process = crate::process::AgentProcess {
                kind: candidate.kind,
                fingerprint: candidate.process.clone().unwrap(),
                resume_session_id: None,
            };
            crate::process::accepts_terminal_input(&process)
        })
        .map(|candidate| {
            Owner::of(if candidate.instance_id == agent.instance_id {
                agent
            } else {
                candidate
            })
        })
        .collect::<Result<_, _>>()?;
    if let Some(mut previous) = previous {
        if let Some(owner) = owners.iter().find(|owner| previous.owner.same_task(owner)) {
            previous.owner = owner.clone();
            return Ok(previous);
        }
    }
    let owner = owners
        .into_iter()
        .min_by_key(|owner| (owner.process.started_at_ticks, owner.process.pid))
        .ok_or("no live interactive agent owns this window")?;
    // The caller must itself be a member, even when another agent owns the title.
    Owner::of(agent)?;
    Ok(TitleState {
        visible_session: owner.session.clone(),
        owner,
        initial_name: window.name.clone(),
        title: None,
        name_from_command: false,
        task_seen: false,
        repair_used: false,
        original_options: None,
    })
}

fn with_window<T>(
    paths: &Paths,
    snapshot: &Snapshot,
    agent: &AgentSnapshot,
    wait: Duration,
    action: impl FnOnce(&ServerIdentity, Window, TitleState, bool) -> Result<T, String>,
) -> Result<T, String> {
    let server = ServerIdentity::from_socket(snapshot.server.clone().into())
        .map_err(|error| error.to_string())?;
    let initial = window(&server, agent)?;
    let _lock = window_lock_wait(paths, &server, &initial.id, wait)?;
    let current = window(&server, agent)?;
    let previous = state(&server, &current)?;
    let preserved = current.protected
        || !generic_name(&current.name)
        || previous.as_ref().is_some_and(|state| {
            current.name != state.title.as_deref().unwrap_or(&state.initial_name)
        });
    let state = owner_state(snapshot, agent, &current, previous)?;
    action(&server, current, state, preserved)
}

/// Synchronize a verified thread's human name, retaining title ownership and
/// respecting manual renames. Task IDs and worktree paths never change here.
fn sync_thread_name(
    paths: &Paths,
    snapshot: &Snapshot,
    agent: &AgentSnapshot,
    allow_session_change: bool,
) -> Result<(), String> {
    let Some(title) = agent.thread_name.as_deref().map(str::trim) else {
        return Ok(());
    };
    if title.is_empty()
        || title.chars().count() > 128
        || title.chars().any(char::is_control)
        || title.contains("#{")
        || title.contains("#[")
        || agent.exited
        || agent.relay_focus.is_some()
    {
        return Ok(());
    }
    if agent.target.window_name == title {
        return Ok(());
    }
    let server = ServerIdentity::from_socket(snapshot.server.clone().into())
        .map_err(|error| error.to_string())?;
    let initial = window(&server, agent)?;
    let _lock = window_lock(paths, &server, &initial.id)?;
    let current = window(&server, agent)?;
    if current.protected || current.name == title {
        return Ok(());
    }
    let mut previous = state(&server, &current)?;
    if let Some(previous) = previous.as_mut() {
        if previous.name_from_command && previous.title.as_deref() != Some(title) {
            return Ok(());
        }
        if previous.owner.pane == agent.target.pane_id
            && Some(&previous.owner.process) == agent.process.as_ref()
        {
            if !previous.owner.same_task(&Owner::of(agent)?) && !allow_session_change {
                return Ok(());
            }
            if allow_session_change {
                previous.owner = Owner::of(agent)?;
            }
        }
        if current.name != previous.title.as_deref().unwrap_or(&previous.initial_name) {
            return Ok(());
        }
    } else if !generic_name(&current.name) && current.task_name.as_deref() != Some(&current.name) {
        return Ok(());
    }
    let mut state = owner_state(snapshot, agent, &current, previous)?;
    if !state.owner.same_task(&Owner::of(agent)?) {
        return Ok(());
    }
    if state.original_options.is_none() {
        state.original_options = Some(TitleOptions {
            automatic_rename: automatic_rename(&server, &current.id)?,
        });
    }
    state.title = Some(title.into());
    let encoded = serde_json::to_string(&state).map_err(|error| error.to_string())?;
    let condition = format!("#{{==:#{{window_name}},{}}}", format_literal(&current.name));
    let commands = format!(
        "rename-window -t {} -- {} ; set-option -w -t {} {} {}",
        current.id,
        quote(title),
        current.id,
        STATE_OPTION,
        quote(&encoded)
    );
    tmux(
        &server,
        &[
            "if-shell",
            "-F",
            "-t",
            &agent.target.pane_id,
            &condition,
            &commands,
        ],
    )?;
    Ok(())
}

/// Restore only a title we still own, guarding against concurrent manual edits.
/// Both task changes and process exits must restore the original naming options.
fn restore_title(
    server: &ServerIdentity,
    current: &Window,
    state: &TitleState,
    encoded: &str,
    finish: &str,
) -> Result<(), String> {
    let id = &current.id;
    let unchanged = format!("#{{==:#{{{STATE_OPTION}}},{}}}", format_literal(encoded));
    let (condition, commands) = if let Some(title) = state
        .title
        .as_deref()
        .filter(|title| current.name == *title && !current.protected)
    {
        let mut commands = format!("rename-window -t {id} -- {} ; ", quote(&state.initial_name));
        let automatic = automatic_rename(server, id)?;
        let restore_automatic = if automatic == Some(false) {
            state
                .original_options
                .as_ref()
                .map(|options| options.automatic_rename)
        } else {
            // Preserve an automatic-rename change made after our rename.
            Some(automatic)
        };
        if let Some(automatic) = restore_automatic {
            match automatic {
                Some(enabled) => commands.push_str(&format!(
                    "set-option -w -t {id} automatic-rename {} ; ",
                    if enabled { "on" } else { "off" }
                )),
                None => commands.push_str(&format!("set-option -wu -t {id} automatic-rename ; ")),
            }
        }
        commands.push_str(finish);
        (
            format!(
                "#{{&&:{unchanged},#{{&&:#{{==:#{{window_name}},{}}},#{{==:#{{automatic-rename}},{}}}}}}}",
                format_literal(title),
                if current.automatic_rename { "1" } else { "0" }
            ),
            commands,
        )
    } else {
        (unchanged, finish.into())
    };
    tmux(server, &["if-shell", "-F", "-t", id, &condition, &commands])?;
    Ok(())
}

fn reset_for_session_change(
    paths: &Paths,
    snapshot: &Snapshot,
    agent: &AgentSnapshot,
    visible_session: Option<&str>,
) -> Result<(), String> {
    let server = ServerIdentity::from_socket(snapshot.server.clone().into())
        .map_err(|error| error.to_string())?;
    let initial = window(&server, agent)?;
    let _lock = window_lock(paths, &server, &initial.id)?;
    let current = window(&server, agent)?;
    let encoded = tmux(
        &server,
        &["show-option", "-wqv", "-t", &current.id, STATE_OPTION],
    )?;
    if encoded.is_empty() {
        return Ok(());
    }
    let mut previous: TitleState =
        serde_json::from_str(&encoded).map_err(|error| error.to_string())?;
    let owner = Owner::of(agent)?;
    if previous.owner.pane != owner.pane
        || previous.owner.process != owner.process
        || current.protected
        || current.name != previous.title.as_deref().unwrap_or(&previous.initial_name)
    {
        return Ok(());
    }
    // A truncated OSC UUID can identify a cleared thread before its first
    // prompt produces the full SessionStart identity. Complete that binding
    // without resetting a title or repair budget already assigned to it.
    let completes_visible_session = previous
        .owner
        .session
        .as_deref()
        .zip(owner.session.as_deref())
        .is_some_and(|(previous, next)| previous.len() >= 24 && next.starts_with(previous));
    if previous.owner.same_task(&owner) || completes_visible_session {
        previous.owner = owner;
        if let Some(visible) = visible_session {
            previous.visible_session = Some(visible.into());
        }
        return save(&server, &current, &previous);
    }
    let fresh = TitleState {
        owner,
        initial_name: previous.initial_name.clone(),
        title: None,
        name_from_command: false,
        task_seen: false,
        repair_used: false,
        original_options: None,
        // A clear hook can arrive before the OSC redraw. Retain the last
        // visible identity so the daemon does not interpret it as a switch back.
        visible_session: visible_session
            .map(str::to_owned)
            .or(previous.visible_session.clone())
            .or(previous.owner.session.clone()),
    };
    let finish = format!(
        "set-option -w -t {} {} {}",
        current.id,
        STATE_OPTION,
        quote(&serde_json::to_string(&fresh).map_err(|error| error.to_string())?)
    );
    restore_title(&server, &current, &previous, &encoded, &finish)
}

fn instruction(agent: &AgentSnapshot) -> String {
    let session = agent.hook_session_id.as_deref().unwrap_or_default();
    let codex = if agent.kind == AgentKind::Codex {
        " For Codex exec_command, use sandbox_permissions=\"require_escalated\" with a naming-specific justification when the tool schema and approval policy allow it."
    } else {
        ""
    };
    format!(
        "The task's tmux window still has a default name. Choose a short meaningful task name (2–4 words or 6–12 Chinese characters) and run `tmux-agent-workbench title set --pane {} --session-id {} '<task name>'` before continuing. Workbench verifies the owner and preserves existing task names. This command needs local Unix-socket access. Choose execution permissions BEFORE the first invocation using your session's current sandbox and approval policy. When that policy blocks local sockets (including the default restricted workspace-write/read-only policy) and approvals are allowed, request approval for this exact command on the FIRST invocation.{codex} Do not first run a restricted naming attempt as a permission probe. If these sockets are already permitted, run normally. If socket access is blocked and approval is forbidden or unavailable, skip naming and continue the task. Keep this pane and its sandbox/network policy.",
        quote(&agent.target.pane_id),
        quote(session)
    )
}

/// An accepted lifecycle report has already resolved the owning pane. Ask its
/// daemon for the socket and live process inventory, never use inherited focus.
pub fn hook_output(
    paths: &Paths,
    socket: &Path,
    pane: &str,
    kind: AgentKind,
    session: &str,
    event: &str,
    payload: &Value,
) -> Value {
    if !matches!(kind, AgentKind::Codex | AgentKind::Claude)
        || !matches!(event, "SessionStart" | "UserPromptSubmit" | "Stop")
    {
        return json!({});
    }
    let result = (|| -> Result<Value, String> {
        let value = call(
            socket,
            &Request::new("snapshot.get", Value::Null),
            Duration::from_millis(750),
        )
        .map_err(|error| error.to_string())?;
        let snapshot: Snapshot =
            serde_json::from_value(value).map_err(|error| error.to_string())?;
        let agents: Vec<_> = snapshot
            .agents
            .iter()
            .filter(|agent| !agent.exited && agent.kind == kind && agent.target.pane_id == pane)
            .collect();
        let [agent] = agents.as_slice() else {
            return Err("hook has no unique live agent".into());
        };
        let clear = event == "SessionStart"
            && payload.get("source").and_then(Value::as_str) == Some("clear");
        let agent = foreground_agent(&snapshot, agent, Some(session), clear)?;
        let mut bound_paths = paths.clone();
        bound_paths.runtime_dir = socket
            .parent()
            .ok_or("daemon directory unavailable")?
            .into();
        if clear {
            reset_for_session_change(&bound_paths, &snapshot, &agent, None)?;
        }
        if agent.thread_name.is_some() {
            sync_thread_name(&bound_paths, &snapshot, &agent, clear)?;
            return Ok(json!({}));
        }
        with_window(
            &bound_paths,
            &snapshot,
            &agent,
            Duration::ZERO,
            |server, current, mut state, preserved| {
                if preserved || !state.owner.same_task(&Owner::of(&agent)?) {
                    return Ok(json!({}));
                }
                match event {
                    "SessionStart" | "UserPromptSubmit" => {
                        state.task_seen |= event == "UserPromptSubmit";
                        save(server, &current, &state)?;
                        Ok(
                            json!({"hookSpecificOutput": {"hookEventName": event, "additionalContext": instruction(&agent)}}),
                        )
                    }
                    "Stop"
                        if state.task_seen
                            && !state.repair_used
                            && payload.get("stop_hook_active").and_then(Value::as_bool)
                                != Some(true) =>
                    {
                        // Persist before asking for continuation. Resume, compaction,
                        // daemon restart and duplicate Stop deliveries share this cap.
                        state.repair_used = true;
                        save(server, &current, &state)?;
                        Ok(
                            json!({"decision": "block", "reason": format!("{} Then finish the response. This naming repair is requested only once for this task.", instruction(&agent))}),
                        )
                    }
                    _ => Ok(json!({})),
                }
            },
        )
    })();
    // A missing daemon, expired pane, or unavailable lock must not block work.
    result.unwrap_or_else(|_| json!({}))
}

fn ancestors() -> Vec<u32> {
    let mut pids = Vec::new();
    let mut system = System::new();
    let mut pid = Pid::from_u32(std::process::id());
    for _ in 0..64 {
        pids.push(pid.as_u32());
        system.refresh_processes_specifics(
            ProcessesToUpdate::Some(&[pid]),
            true,
            ProcessRefreshKind::nothing().without_tasks(),
        );
        let Some(parent) = system.process(pid).and_then(|process| process.parent()) else {
            break;
        };
        if parent == pid {
            break;
        }
        pid = parent;
    }
    pids
}

/// Resolve an ordinary tool call through process ancestry. Explicit hook-provided
/// pane + session identity also works behind a shared backend or PID sandbox.
pub fn for_caller(
    paths: &Paths,
    pane: Option<&str>,
    session: Option<&str>,
    title: Option<&str>,
) -> Result<Value, String> {
    for_caller_action(paths, pane, session, title, false)
}

pub fn rename_for_caller(
    paths: &Paths,
    pane: Option<&str>,
    session: Option<&str>,
    title: &str,
) -> Result<Value, String> {
    for_caller_action(paths, pane, session, Some(title), true)
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct TitleRequest {
    pub instance_id: String,
    pub process: ProcessFingerprint,
    pub pane_id: String,
    pub session_id: Option<String>,
    pub title: Option<String>,
    #[serde(default)]
    pub rename: bool,
}

fn for_caller_action(
    paths: &Paths,
    pane: Option<&str>,
    session: Option<&str>,
    title: Option<&str>,
    rename: bool,
) -> Result<Value, String> {
    if let Some(title) = title {
        validate_title(title)?;
    }
    if pane.is_some_and(|pane| {
        !pane
            .strip_prefix('%')
            .is_some_and(|s| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit()))
    }) {
        return Err("pane must be a verified %<digits> identity".into());
    }
    let pids = ancestors();
    let mut candidates = Vec::new();
    let discovery = paths.discover_daemon_sockets();
    let mut denied = discovery.permission_error;
    let mut unavailable = None;
    let mut snapshots_seen = 0;
    for path in discovery.sockets {
        let value = match call(
            &path,
            &Request::new("snapshot.get", Value::Null),
            Duration::from_millis(750),
        ) {
            Ok(value) => value,
            Err(error) => {
                if matches!(&error,
                    crate::ipc::ClientError::Connect { source, .. }
                        | crate::ipc::ClientError::Io(source)
                        if source.kind() == std::io::ErrorKind::PermissionDenied)
                {
                    denied = Some(error.to_string());
                }
                unavailable = Some(error.to_string());
                continue;
            }
        };
        let Ok(snapshot) = serde_json::from_value::<Snapshot>(value) else {
            unavailable = Some("daemon returned an invalid snapshot".into());
            continue;
        };
        snapshots_seen += 1;
        for agent in &snapshot.agents {
            if !agent.exited && agent.relay_focus.is_none() && agent.process.is_some() {
                candidates.push((path.clone(), snapshot.clone(), agent.clone()));
            }
        }
    }
    let explicit = pane.is_some() && session.is_some();
    candidates.retain(|(_, _, agent)| {
        let ancestor = agent
            .process
            .as_ref()
            .is_some_and(|process| pids.contains(&process.pid));
        (explicit || ancestor) && pane.is_none_or(|pane| agent.target.pane_id == pane)
    });
    // Prefer the current runtime when an older daemon is still discoverable.
    candidates.sort_by_key(|(socket, _, _)| socket.parent() != Some(paths.runtime_dir.as_path()));
    let mut verified = Vec::new();
    let mut identities = Vec::new();
    let mut reason = None;
    for (socket, snapshot, agent) in candidates {
        match foreground_agent(&snapshot, &agent, session, false) {
            Ok(agent) => {
                // Several daemons can observe one caller. Collapse only exact
                // verified identities, including the server and full process
                // fingerprint; separate servers, sessions and reused PIDs must
                // remain ambiguous rather than choosing an arbitrary owner.
                let server = ServerIdentity::from_socket(snapshot.server.clone().into())
                    .map_err(|error| error.to_string())?;
                let identity = (
                    server.key,
                    agent.kind,
                    agent.target.window_id.clone(),
                    Owner::of(&agent)?,
                );
                if !identities.contains(&identity) {
                    identities.push(identity);
                    verified.push((socket, snapshot, agent));
                }
            }
            Err(error) => reason = Some(error),
        }
    }
    let candidates = verified;
    let [(socket, _, agent)] = candidates.as_slice() else {
        if candidates.is_empty() {
            if let Some(reason) = reason {
                return Err(reason);
            }
            if snapshots_seen == 0 {
                if let Some(denied) = denied {
                    return Err(socket_access_error(&denied));
                }
                if let Some(unavailable) = unavailable {
                    return Err(format!("cannot read Workbench identity: {unavailable}"));
                }
            }
        }
        return Err("cannot uniquely verify the caller; use --pane and --session-id from the Workbench hook, never the focused window".into());
    };
    call(
        socket,
        &Request::new(
            "task.title",
            serde_json::to_value(TitleRequest {
                instance_id: agent.instance_id.clone(),
                process: agent.process.clone().ok_or("agent process unavailable")?,
                pane_id: agent.target.pane_id.clone(),
                session_id: agent.hook_session_id.clone(),
                title: title.map(str::to_owned),
                rename,
            })
            .map_err(|error| error.to_string())?,
        ),
        Duration::from_secs(3),
    )
    .map_err(|error| error.to_string())
}

/// The daemon serializes this tmux update with the corresponding shared-name
/// update and snapshot publication before replying to the command.
pub(crate) fn apply(
    paths: &Paths,
    snapshot: &Snapshot,
    request: &TitleRequest,
) -> Result<Value, String> {
    let agent = snapshot
        .agents
        .iter()
        .find(|agent| {
            agent.instance_id == request.instance_id
                && !agent.exited
                && agent.relay_focus.is_none()
                && agent.target.pane_id == request.pane_id
                && agent.process.as_ref() == Some(&request.process)
        })
        .ok_or("agent process changed before naming; retry with its current identity")?;
    let agent = foreground_agent(snapshot, agent, request.session_id.as_deref(), false)?;
    with_window(
        paths,
        snapshot,
        &agent,
        Duration::from_millis(500),
        |server, current, mut state, preserved| {
            let owner = state.owner.same_task(&Owner::of(&agent)?);
            let owned_title = !current.protected
                && owner
                && state.title.as_deref() == Some(current.name.as_str());
            let preserved = preserved && !(request.rename && owned_title);
            let status = if preserved {
                "preserved"
            } else if !owner {
                "not_owner"
            } else {
                "pending"
            };
            let Some(title) = request.title.as_deref() else {
                return Ok(
                    json!({"status":status,"window_id":current.id,"title":current.name,
                "owner_pane":state.owner.pane,"repair_used":state.repair_used}),
                );
            };
            let title = validate_title(title)?;
            if preserved || !owner {
                return Ok(json!({"status":status,"window_id":current.id,"title":current.name}));
            }
            state.title = Some(title.into());
            state.name_from_command = true;
            if state.original_options.is_none() {
                state.original_options = Some(TitleOptions {
                    automatic_rename: automatic_rename(server, &current.id)?,
                });
            }
            let encoded = serde_json::to_string(&state).map_err(|error| error.to_string())?;
            // Guard the rename inside tmux as well as under our lock. A manual
            // rename between the read and this command always wins.
            let condition = format!(
                "#{{&&:#{{==:#{{window_id}},{}}},#{{==:#{{window_name}},{}}}}}",
                current.id,
                format_literal(&current.name)
            );
            let commands = format!(
                "rename-window -t {} -- {} ; set-option -w -t {} {} {}",
                current.id,
                quote(title),
                current.id,
                STATE_OPTION,
                quote(&encoded)
            );
            tmux(
                server,
                &[
                    "if-shell",
                    "-F",
                    "-t",
                    &agent.target.pane_id,
                    &condition,
                    &commands,
                ],
            )?;
            let actual = window(server, &agent)?;
            if actual.name != title {
                return Err(format!(
                    "window title changed concurrently; preserved {:?}",
                    actual.name
                ));
            }
            let stored =
                crate::task_title::state(server, &actual)?.ok_or("title verification failed")?;
            if stored.title.as_deref() != Some(title)
                || !stored.owner.same_task(&Owner::of(&agent)?)
            {
                return Err("title ownership verification failed".into());
            }
            Ok(json!({"status":"renamed","window_id":actual.id,"title":actual.name}))
        },
    )
}

/// The daemon owns exit cleanup: Stop hooks end turns, and a killed CLI cannot
/// deliver a shutdown hook. Check OS identity rather than inferred screen state.
pub(crate) struct TitleReaper {
    system: System,
    next_scan: Instant,
}

/// Project the detector's canonical session into tmux. Never infer a second
/// foreground session here: sidebar, notifications and checkpoints consume the
/// very same AgentSnapshot as window naming.
fn sync_session(
    paths: &Paths,
    snapshot: &Snapshot,
    agent: &AgentSnapshot,
    encoded: &str,
    pane_title: &str,
) {
    if agent.exited || agent.kind != AgentKind::Codex || agent.relay_focus.is_some() {
        return;
    }
    let Ok(state) = serde_json::from_str::<TitleState>(encoded) else {
        return;
    };
    // An OSC title inherited by a replacement process is insufficient. Only
    // the already-bound CLI can switch its own window to another thread.
    if state.owner.pane != agent.target.pane_id
        || Some(&state.owner.process) != agent.process.as_ref()
    {
        return;
    }
    let visible = crate::state_machine::codex_title_identity(pane_title).filter(|visible| {
        agent
            .hook_session_id
            .as_deref()
            .is_some_and(|id| id.starts_with(visible))
    });
    if (visible.is_some() || agent.reason_category.as_deref() == Some("clear"))
        && (state.owner.session != agent.hook_session_id
            || visible.is_some_and(|id| state.visible_session.as_deref() != Some(id)))
    {
        let _ = reset_for_session_change(paths, snapshot, agent, visible);
    }
}

impl Default for TitleReaper {
    fn default() -> Self {
        Self {
            system: System::new(),
            next_scan: Instant::now(),
        }
    }
}

impl TitleReaper {
    pub(crate) fn tick(&mut self, paths: &Paths, server: &ServerIdentity, snapshot: &Snapshot) {
        if Instant::now() < self.next_scan {
            return;
        }
        self.next_scan = Instant::now() + Duration::from_millis(500);
        let Ok(windows) = tmux(
            server,
            &[
                "list-panes",
                "-a",
                "-F",
                "#{window_id}\u{1f}#{@workbench_task_title}\u{1f}#{pane_id}\u{1f}#{pane_title}",
            ],
        ) else {
            return;
        };
        let panes: Vec<_> = windows
            .lines()
            .filter_map(|line| {
                let fields: Vec<_> = line.splitn(4, '\u{1f}').collect();
                (fields.len() == 4).then_some(fields)
            })
            .collect();
        for agent in &snapshot.agents {
            if let Some(fields) = panes
                .iter()
                .find(|fields| fields[2] == agent.target.pane_id)
            {
                sync_session(paths, snapshot, agent, fields[1], fields[3]);
            }
            let _ = sync_thread_name(paths, snapshot, agent, false);
        }
        let mut seen = std::collections::HashSet::new();
        for fields in panes {
            let (id, encoded) = (fields[0], fields[1]);
            if !seen.insert(id) {
                continue;
            }
            let Ok(state) = serde_json::from_str::<TitleState>(encoded) else {
                continue;
            };
            // An unnamed task's repair budget still survives resume. Only an
            // actual rename creates something to undo when its owner exits.
            if state.title.is_some() && self.exited(&state.owner.process) {
                let _ = self.restore_window(paths, server, snapshot, id);
            }
        }
    }

    fn exited(&mut self, expected: &ProcessFingerprint) -> bool {
        let Ok(raw_pid) = libc::pid_t::try_from(expected.pid) else {
            return false;
        };
        if raw_pid <= 0 {
            return false;
        }
        let pid = Pid::from_u32(expected.pid);
        self.system.refresh_processes_specifics(
            ProcessesToUpdate::Some(&[pid]),
            true,
            ProcessRefreshKind::nothing()
                .without_tasks()
                .with_exe(sysinfo::UpdateKind::Always),
        );
        if let Some(process) = self.system.process(pid) {
            if process.status() == sysinfo::ProcessStatus::Zombie {
                return true;
            }
            if process.start_time() == 0 || expected.started_at_ticks == 0 {
                return false;
            }
            if process.start_time() != expected.started_at_ticks {
                return true;
            }
            // An exec can replace the CLI without changing its PID/start time.
            // Missing OS metadata is uncertainty, never evidence of an exit.
            if Path::new(&expected.executable).is_absolute() {
                return process
                    .exe()
                    .is_some_and(|path| path != Path::new(&expected.executable));
            }
            return false;
        }
        (unsafe { libc::kill(raw_pid, 0) }) == -1
            && std::io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH)
    }

    fn restore_window(
        &mut self,
        paths: &Paths,
        server: &ServerIdentity,
        snapshot: &Snapshot,
        id: &str,
    ) -> Result<(), String> {
        let _lock = window_lock(paths, server, id)?;
        let (_, current) = window_at(server, id)?;
        let encoded = tmux(server, &["show-option", "-wqv", "-t", id, STATE_OPTION])?;
        let mut state: TitleState =
            serde_json::from_str(&encoded).map_err(|error| error.to_string())?;
        let Some(title) = state.title.as_deref() else {
            return Ok(());
        };
        if !self.exited(&state.owner.process) {
            return Ok(());
        }
        let owns_title = current.name == title && !current.protected;
        if owns_title {
            // A CLI re-exec/resume that is already verified in the same pane
            // continues the same task. Returning to a shell has no replacement.
            if let Some(replacement) = snapshot.agents.iter().find(|agent| {
                !agent.exited
                    && agent.relay_focus.is_none()
                    && agent.target.window_id == id
                    && Owner::of(agent).is_ok_and(|owner| state.owner.same_task(&owner))
            }) {
                let owner = Owner::of(replacement)?;
                if !self.exited(&owner.process) {
                    state.owner = owner;
                    return save(server, &current, &state);
                }
            }
        }
        let clear = format!("set-option -wu -t {id} {STATE_OPTION}");
        restore_title(server, &current, &state, &encoded, &clear)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exit_detection_checks_process_identity_and_actual_termination() {
        let mut child = std::process::Command::new("/bin/sleep")
            .arg("30")
            .spawn()
            .unwrap();
        let mut reaper = TitleReaper::default();
        let probe = ProcessFingerprint {
            pid: child.id(),
            started_at_ticks: 0,
            executable: String::new(),
        };
        let deadline = Instant::now() + Duration::from_secs(5);
        let identity = loop {
            reaper.exited(&probe);
            if let Some(process) = reaper.system.process(Pid::from_u32(child.id())) {
                if let Some(path) = process.exe() {
                    // The fork/exec metadata may still describe the parent.
                    // Wait for the final executable before fingerprinting it.
                    if path.file_name() == Some(std::ffi::OsStr::new("sleep"))
                        && process.start_time() != 0
                        && process.status() != sysinfo::ProcessStatus::Zombie
                    {
                        break Some(ProcessFingerprint {
                            pid: child.id(),
                            started_at_ticks: process.start_time(),
                            executable: path.display().to_string(),
                        });
                    }
                }
            }
            if Instant::now() >= deadline {
                break None;
            }
            std::thread::sleep(Duration::from_millis(10));
        };
        let observations = identity.as_ref().map(|identity| {
            let live = !reaper.exited(identity);
            let mut reused = identity.clone();
            reused.started_at_ticks += 1;
            let replaced_pid = reaper.exited(&reused);
            let mut replaced = identity.clone();
            replaced.executable = "/workbench-test/replaced-executable".into();
            (live, replaced_pid, reaper.exited(&replaced))
        });
        // Reap before assertions so a failed test cannot leave a child behind.
        let _ = child.kill();
        child.wait().unwrap();
        assert_eq!(observations, Some((true, true, true)), "{identity:?}");
        assert!(reaper.exited(identity.as_ref().unwrap()));
    }

    #[test]
    fn task_names_accept_unicode_and_punctuation_but_not_default_or_terminal_codes() {
        for title in [
            "光标复现与验收",
            "Fix O'Reilly's $PATH; parsing",
            "Repair title ownership",
        ] {
            assert!(validate_title(title).is_ok());
        }
        for title in [
            "",
            "zsh",
            "python3.12",
            "\u{1b}]2;bad",
            "line\nbreak",
            "#{window_id}",
        ] {
            assert!(validate_title(title).is_err(), "{title:?}");
        }
    }

    #[test]
    fn resumed_task_keeps_ownership_but_new_session_and_reused_pid_do_not() {
        let owner = Owner {
            pane: "%1".into(),
            session: Some("thread".into()),
            process: ProcessFingerprint {
                pid: 42,
                started_at_ticks: 10,
                executable: "codex".into(),
            },
        };
        let mut resumed = owner.clone();
        resumed.process.pid = 43;
        assert!(owner.same_task(&resumed));
        resumed.session = Some("new-task".into());
        assert!(!owner.same_task(&resumed));
        let mut unbound = owner.clone();
        unbound.session = None;
        unbound.process.started_at_ticks += 1;
        assert!(!owner.same_task(&unbound));
    }
}
