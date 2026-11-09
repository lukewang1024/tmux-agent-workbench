//! Queue full-direction requests for the machine-wide manager session.
use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use fs2::FileExt;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use crate::config::{AutoSplitConfig, SplitStrategy};
use crate::model::{AgentKind, AgentSnapshot, Snapshot};
use crate::paths::Paths;
use crate::server::ServerIdentity;

const FAILURE_BACKOFF_MS: u64 = 10 * 60 * 1000;
const MAX_HISTORY: usize = 64;

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Ticket {
    id: String,
    server_pid: u32,
    #[serde(default)]
    server_socket: String,
    source_session: String,
    fingerprint: String,
    owner_instance: String,
    owner_session: String,
    #[serde(default)]
    owner_kind: Option<AgentKind>,
    issued_at_ms: u64,
    threshold: usize,
    strategy: SplitStrategy,
    #[serde(default)]
    manager_managed: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Attempt {
    #[serde(default)]
    ticket_id: String,
    fingerprint: String,
    owner_session: String,
    result: String,
    finished_at_ms: u64,
}

#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(default)]
struct Ledger {
    server_pid: Option<u32>,
    active: Option<Ticket>,
    history: Vec<Attempt>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Candidate {
    session: String,
    fingerprint: String,
    count: usize,
}

fn valid_id(value: &str, prefix: char) -> bool {
    value
        .strip_prefix(prefix)
        .is_some_and(|rest| !rest.is_empty() && rest.bytes().all(|byte| byte.is_ascii_digit()))
}

fn candidates(snapshot: &Snapshot, config: &AutoSplitConfig) -> Vec<Candidate> {
    let (true, Some(threshold), Some(strategy)) =
        (config.enabled, config.threshold, config.strategy)
    else {
        return vec![];
    };
    let known_sessions: BTreeSet<_> = snapshot
        .sessions
        .iter()
        .map(|session| session.session_id.as_str())
        .filter(|session| valid_id(session, '$'))
        .collect();
    let mut windows: BTreeMap<&str, BTreeSet<&str>> = BTreeMap::new();
    let mut titles: BTreeMap<&str, BTreeSet<(&str, &str, &str)>> = BTreeMap::new();
    for agent in &snapshot.agents {
        if agent.exited
            || agent.stale
            || agent.process.is_none()
            || agent.relay_focus.is_some()
            || !known_sessions.contains(agent.target.session_id.as_str())
            || !valid_id(&agent.target.window_id, '@')
        {
            continue;
        }
        windows
            .entry(&agent.target.session_id)
            .or_default()
            .insert(&agent.target.window_id);
        titles.entry(&agent.target.session_id).or_default().insert((
            &agent.target.window_id,
            &agent.target.window_name,
            agent.thread_name.as_deref().unwrap_or(""),
        ));
    }
    let mut found = Vec::new();
    for (session, ids) in windows {
        if snapshot.sessions.iter().any(|candidate| {
            candidate.session_id == session
                && candidate
                    .session_name
                    .starts_with(crate::manager_session::SESSION_PREFIX)
        }) {
            continue;
        }
        if ids.len() < threshold {
            continue;
        }
        let mut hash = Sha256::new();
        hash.update(session.as_bytes());
        hash.update(format!("\0{threshold}\0{strategy:?}").as_bytes());
        for id in &ids {
            hash.update(b"\0");
            hash.update(id.as_bytes());
        }
        if let Some(signals) = titles.get(session) {
            for (window, window_name, thread_name) in signals {
                for value in [window, window_name, thread_name] {
                    hash.update(b"\0");
                    hash.update(value.as_bytes());
                }
            }
        }
        found.push(Candidate {
            session: session.into(),
            fingerprint: format!("{:x}", hash.finalize()),
            count: ids.len(),
        });
    }
    found.sort_by(|left, right| {
        right
            .count
            .cmp(&left.count)
            .then_with(|| left.session.cmp(&right.session))
    });
    found
}

fn private_dir(path: &Path) -> io::Result<()> {
    fs::create_dir_all(path)?;
    fs::set_permissions(path, fs::Permissions::from_mode(0o700))
}

fn ledger_dir(paths: &Paths, server: &ServerIdentity) -> PathBuf {
    paths.state_dir.join("auto-split").join(&server.key)
}

/// Wait briefly for ledger writers, including a child process finishing an
/// inherited descriptor handoff. Persistent contention still fails closed.
fn try_lock_for(file: &File, timeout: Duration) -> io::Result<bool> {
    let started = Instant::now();
    loop {
        match file.try_lock_exclusive() {
            Ok(()) => return Ok(true),
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                if started.elapsed() >= timeout {
                    return Ok(false);
                }
                crate::compat::sleep(Duration::from_millis(10));
            }
            Err(error) => return Err(error),
        }
    }
}

/// Serialize client-side capacity checks and placement requests while an
/// older daemon is kept alive. The daemon still performs its own check after
/// it is eventually upgraded; the two locks protect different operations.
pub(crate) fn lock_placement(paths: &Paths, server: &ServerIdentity) -> io::Result<File> {
    let dir = ledger_dir(paths, server);
    private_dir(&dir)?;
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .open(dir.join("placement.lock"))?;
    if !try_lock_for(&file, Duration::from_secs(4))? {
        return Err(io::Error::new(
            io::ErrorKind::TimedOut,
            "task placement is busy; retry shortly",
        ));
    }
    Ok(file)
}

fn load_ledger(dir: &Path) -> io::Result<Ledger> {
    let path = dir.join("state.json");
    match fs::read(path) {
        Ok(bytes) => serde_json::from_slice(&bytes).map_err(io::Error::other),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(Ledger::default()),
        Err(error) => Err(error),
    }
}

fn save_ledger(dir: &Path, ledger: &Ledger) -> io::Result<()> {
    let temp = dir.join(format!(".state-{}.tmp", uuid::Uuid::new_v4()));
    let result = (|| {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&temp)?;
        serde_json::to_writer(&mut file, ledger).map_err(io::Error::other)?;
        file.write_all(b"\n")?;
        file.sync_all()?;
        fs::rename(&temp, dir.join("state.json"))
    })();
    if result.is_err() {
        let _ = fs::remove_file(temp);
    }
    result
}

fn remember(ledger: &mut Ledger, ticket: Ticket, result: &str, now: u64) {
    ledger.history.push(Attempt {
        ticket_id: ticket.id,
        fingerprint: ticket.fingerprint,
        owner_session: ticket.owner_session,
        result: result.into(),
        finished_at_ms: now,
    });
    if ledger.history.len() > MAX_HISTORY {
        ledger.history.drain(..ledger.history.len() - MAX_HISTORY);
    }
}

fn claim(
    paths: &Paths,
    server: &ServerIdentity,
    server_pid: u32,
    snapshot: &Snapshot,
    owner: &AgentSnapshot,
    owner_session: &str,
    source_session: &str,
    config: &AutoSplitConfig,
    now: u64,
) -> io::Result<Option<Ticket>> {
    let dir = ledger_dir(paths, server);
    private_dir(&dir)?;
    let lock = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .mode(0o600)
        .open(dir.join("lock"))?;
    if !try_lock_for(&lock, Duration::from_millis(500))? {
        return Ok(None);
    }
    let mut ledger = load_ledger(&dir)?;
    let previous_pid = ledger
        .server_pid
        .or_else(|| ledger.active.as_ref().map(|ticket| ticket.server_pid));
    if previous_pid.is_some_and(|pid| pid != server_pid) {
        // A restarted server cannot share the old windows or an active worker.
        ledger = Ledger::default();
    }
    ledger.server_pid = Some(server_pid);
    let candidate = candidates(snapshot, config).into_iter().find(|candidate| {
        candidate.session == source_session
            && !ledger.history.iter().rev().any(|attempt| {
                attempt.fingerprint == candidate.fingerprint
                    && match attempt.result.as_str() {
                        "completed" | "no_change" => true,
                        "unavailable" => {
                            now.saturating_sub(attempt.finished_at_ms) < FAILURE_BACKOFF_MS
                                && attempt.owner_session == owner_session
                        }
                        _ => now.saturating_sub(attempt.finished_at_ms) < FAILURE_BACKOFF_MS,
                    }
            })
    });
    let Some(candidate) = candidate else {
        save_ledger(&dir, &ledger)?;
        return Ok(None);
    };
    if let Some(active) = &ledger.active {
        // Re-dispatch only this exact manager ticket. Concurrent claimants
        // share its ID, and the manager's locked submitted-ticket ledger
        // deduplicates queue delivery while allowing retries to repair a
        // manager session whose runtime profile changed.
        return Ok((active.manager_managed
            && active.source_session == candidate.session
            && active.fingerprint == candidate.fingerprint)
            .then(|| active.clone()));
    }
    let ticket = Ticket {
        id: uuid::Uuid::new_v4().to_string(),
        server_pid,
        server_socket: server.socket_path.to_string_lossy().into_owned(),
        source_session: candidate.session,
        fingerprint: candidate.fingerprint,
        owner_instance: owner.instance_id.clone(),
        owner_session: owner_session.into(),
        owner_kind: Some(owner.kind),
        issued_at_ms: now,
        threshold: config.threshold.unwrap(),
        strategy: config.strategy.unwrap(),
        manager_managed: true,
    };
    ledger.active = Some(ticket.clone());
    save_ledger(&dir, &ledger)?;
    Ok(Some(ticket))
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .min(u128::from(u64::MAX)) as u64
}

fn instruction(ticket: &Ticket) -> String {
    format!(
        "Placing this task would exceed the configured limit of {} tasks in session {}. \
         Workbench has sent ticket {} to its single machine-wide coding-agent manager session, \
         which serializes automatic tmux reorganisation. Do not spawn or delegate to another \
         agent, create a worker pane, or move windows. Keep this task in its current window, continue \
         the user's work, and retry title placement after the manager handles the ticket. Check \
         `tmux-agent-workbench auto-split status` for current ticket and manager status. The ticket \
         is scoped to server socket {:?}, PID is {}, and source session {}; the manager must preserve \
         unrelated sessions, panes, clients, and manual focus marks. Before inspecting or changing \
         tmux, read `manager.session_name` and `manager.server_socket` from auto-split status and \
         verify that exact reserved session with `tmux -S \"$manager_socket\" display-message -t \
         \"$manager_session\" -p '#{{session_name}}'`. Confirm exactly one `codex` pane there. Do not \
         rely on targetless commands or `TMUX_PANE`, which may be absent in tool subprocesses. If \
         the manager session cannot be verified, leave the ticket unsettled and do not touch layout.",
        ticket.threshold,
        ticket.source_session,
        ticket.id,
        ticket.server_socket,
        ticket.server_pid,
        ticket.source_session,
    )
}

fn spawn_dispatch(ticket_id: &str) -> io::Result<()> {
    let executable = std::env::current_exe()?;
    let mut command = Command::new(executable);
    command
        .args(["auto-split", "dispatch", "--ticket", ticket_id])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .process_group(0);
    command.spawn()?;
    Ok(())
}

/// Check the chosen direction before moving this task. Only the selected
/// direction can create a request; an unrelated busy session is untouched.
pub(crate) fn request_placement(
    paths: &Paths,
    snapshot: &Snapshot,
    owner: &AgentSnapshot,
    destination: &str,
    config: &AutoSplitConfig,
) -> Result<Option<Value>, String> {
    let Some(limit) = config.threshold.filter(|_| config.enabled) else {
        return Ok(None);
    };
    let Some(target) = snapshot
        .sessions
        .iter()
        .find(|s| s.session_name == destination)
    else {
        return Ok(None);
    };
    let Some(candidate) = candidates(snapshot, config)
        .into_iter()
        .find(|candidate| candidate.session == target.session_id)
    else {
        return Ok(None);
    };
    let incoming = usize::from(owner.target.session_id != target.session_id);
    if candidate.count + incoming <= limit {
        return Ok(None);
    }
    let server = ServerIdentity::from_socket(PathBuf::from(&snapshot.server))
        .map_err(|error| error.to_string())?;
    let info = crate::tmux::Tmux::new(server.clone())
        .output_with_timeout(
            &[
                "display-message",
                "-p",
                "-t",
                &owner.target.pane_id,
                "#{pid}\t#{@agent_team}\t#{@agent_team_role}",
            ],
            Duration::from_millis(250),
        )
        .map_err(|error| error.to_string())?;
    let mut fields = info.trim_end_matches('\n').splitn(3, '\t');
    let (Some(pid), Some(team), Some(role)) = (fields.next(), fields.next(), fields.next()) else {
        return Err("could not verify the target tmux server".into());
    };
    if !team.is_empty() || !role.is_empty() || std::env::var_os("AGENT_TEAM_DIR").is_some() {
        return Ok(None);
    }
    let server_pid = pid.parse().map_err(|_| "invalid tmux server PID")?;
    let owner_session = owner
        .hook_session_id
        .as_deref()
        .unwrap_or(&owner.instance_id);
    let ticket = claim(
        paths,
        &server,
        server_pid,
        snapshot,
        owner,
        owner_session,
        &target.session_id,
        config,
        now_ms(),
    )
    .map_err(|error| error.to_string())?;
    Ok(Some(match ticket {
        Some(ticket) => {
            let status = if spawn_dispatch(&ticket.id).is_err() {
                let _ = finish(paths, &ticket.id, "failed");
                crate::manager_session::record_error(
                    paths,
                    "could not start the manager dispatcher",
                );
                "manager_unavailable"
            } else {
                "manager_pending"
            };
            json!({
                "status":status, "destination":destination,
                "current_tasks":candidate.count, "limit":limit,
                "source_session":target.session_id, "ticket":ticket.id,
                "instruction":instruction(&ticket),
            })
        }
        None => json!({
            "status":"split_pending", "destination":destination,
            "current_tasks":candidate.count, "limit":limit,
            "instruction":"This direction has an existing pending split request. Check auto-split status; keep this task in its current window and continue the user task. Do not start another agent or move existing windows.",
        }),
    }))
}

fn active_ticket(paths: &Paths, ticket_id: &str) -> io::Result<Option<Ticket>> {
    let root = paths.state_dir.join("auto-split");
    if !root.is_dir() {
        return Ok(None);
    }
    for entry in fs::read_dir(root)? {
        let entry = entry?;
        if !entry.file_type()?.is_dir() {
            continue;
        }
        let dir = entry.path();
        let lock = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .open(dir.join("lock"))?;
        FileExt::lock_shared(&lock)?;
        let active = load_ledger(&dir)?.active;
        if let Some(ticket) = active.filter(|ticket| ticket.id == ticket_id) {
            return Ok(Some(ticket));
        }
    }
    Ok(None)
}

/// Run the detached dispatcher after title placement has returned to its caller.
pub fn dispatch(paths: &Paths, ticket_id: &str) -> io::Result<()> {
    let Some(ticket) = active_ticket(paths, ticket_id)? else {
        return Ok(());
    };
    if !ticket.manager_managed {
        return Ok(());
    }
    let result = (|| {
        let server = ServerIdentity::from_socket(PathBuf::from(&ticket.server_socket))
            .map_err(|error| io::Error::other(error.to_string()))?;
        let pid = crate::tmux::Tmux::new(server.clone())
            .output_with_timeout(&["display-message", "-p", "#{pid}"], Duration::from_secs(2))
            .map_err(io::Error::other)?
            .trim()
            .parse::<u32>()
            .map_err(io::Error::other)?;
        if pid != ticket.server_pid {
            return Err(io::Error::other("ticket tmux server PID changed"));
        }
        let metadata = serde_json::to_string(&ticket).map_err(io::Error::other)?;
        crate::manager_session::enqueue(paths, &server, pid, &ticket.id, &metadata)
            .map_err(io::Error::other)
    })();
    if let Err(error) = result {
        let _ = finish(paths, &ticket.id, "failed");
        crate::manager_session::record_error(paths, &error.to_string());
    }
    Ok(())
}

pub fn finish(paths: &Paths, ticket_id: &str, result: &str) -> io::Result<()> {
    if uuid::Uuid::parse_str(ticket_id).is_err()
        || !matches!(result, "completed" | "no_change" | "unavailable" | "failed")
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "invalid ticket or result",
        ));
    }
    let root = paths.state_dir.join("auto-split");
    if !root.is_dir() {
        return Err(io::Error::new(io::ErrorKind::NotFound, "ticket not found"));
    }
    for entry in fs::read_dir(root)? {
        let entry = entry?;
        if !entry.file_type()?.is_dir() {
            continue;
        }
        let dir = entry.path();
        let lock = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .mode(0o600)
            .open(dir.join("lock"))?;
        lock.lock_exclusive()?;
        let mut ledger = load_ledger(&dir)?;
        if ledger
            .active
            .as_ref()
            .is_some_and(|ticket| ticket.id == ticket_id)
        {
            let ticket = ledger.active.take().unwrap();
            remember(&mut ledger, ticket, result, now_ms());
            return save_ledger(&dir, &ledger);
        }
        if let Some(previous) = ledger
            .history
            .iter()
            .find(|attempt| attempt.ticket_id == ticket_id)
        {
            return if previous.result == result {
                Ok(())
            } else {
                Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "ticket already settled with another result",
                ))
            };
        }
    }
    Err(io::Error::new(io::ErrorKind::NotFound, "ticket not found"))
}

/// Clear the retry barrier for one failed or unavailable ticket after its cause is fixed.
pub fn retry(paths: &Paths, ticket_id: &str) -> io::Result<()> {
    if uuid::Uuid::parse_str(ticket_id).is_err() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "invalid ticket ID",
        ));
    }
    let root = paths.state_dir.join("auto-split");
    if !root.is_dir() {
        return Err(io::Error::new(io::ErrorKind::NotFound, "ticket not found"));
    }
    for entry in fs::read_dir(root)? {
        let entry = entry?;
        if !entry.file_type()?.is_dir() {
            continue;
        }
        let dir = entry.path();
        let lock = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .open(dir.join("lock"))?;
        FileExt::lock_exclusive(&lock)?;
        let mut ledger = load_ledger(&dir)?;
        if ledger
            .active
            .as_ref()
            .is_some_and(|ticket| ticket.id == ticket_id)
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "ticket is still pending",
            ));
        }
        if let Some(index) = ledger
            .history
            .iter()
            .position(|attempt| attempt.ticket_id == ticket_id)
        {
            if !matches!(
                ledger.history[index].result.as_str(),
                "failed" | "unavailable"
            ) {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "only failed or unavailable tickets can be retried",
                ));
            }
            ledger.history.remove(index);
            return save_ledger(&dir, &ledger);
        }
    }
    Err(io::Error::new(io::ErrorKind::NotFound, "ticket not found"))
}

fn pending_tickets(paths: &Paths) -> io::Result<Vec<Ticket>> {
    let root = paths.state_dir.join("auto-split");
    let mut pending = Vec::new();
    if root.is_dir() {
        for entry in fs::read_dir(root)? {
            let entry = entry?;
            if !entry.file_type()?.is_dir() {
                continue;
            }
            let dir = entry.path();
            let lock = OpenOptions::new()
                .read(true)
                .write(true)
                .create(true)
                .mode(0o600)
                .open(dir.join("lock"))?;
            FileExt::lock_shared(&lock)?;
            if let Some(ticket) = load_ledger(&dir)?.active {
                pending.push(ticket);
            }
        }
    }
    Ok(pending)
}

pub(crate) fn pending_manager_tickets(paths: &Paths) -> io::Result<Vec<(String, String)>> {
    pending_tickets(paths)?
        .into_iter()
        .filter(|ticket| ticket.manager_managed)
        .map(|ticket| {
            let id = ticket.id.clone();
            serde_json::to_string(&ticket)
                .map(|metadata| (id, metadata))
                .map_err(io::Error::other)
        })
        .collect()
}

pub fn status(paths: &Paths) -> io::Result<Value> {
    Ok(json!({
        "pending": pending_tickets(paths)?,
        "manager": crate::manager_session::status(paths),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(count: usize) -> Snapshot {
        let mut snapshot: Snapshot =
            serde_json::from_str(include_str!("../tests/golden/snapshot-v1.json")).unwrap();
        let template = snapshot.agents[0].clone();
        snapshot.agents = (0..count)
            .map(|index| {
                let mut agent = template.clone();
                agent.instance_id = format!("agent-{index}");
                agent.target.session_id = "$1".into();
                agent.target.window_id = format!("@{}", index + 1);
                agent.target.pane_id = format!("%{}", index + 1);
                agent.exited = false;
                agent.stale = false;
                agent.relay_focus = None;
                agent.hook_session_id = Some(format!("thread-{index}"));
                agent
            })
            .collect();
        snapshot
    }

    fn paths(root: &Path) -> Paths {
        Paths {
            config_dir: root.join("config"),
            state_dir: root.join("state"),
            cache_dir: root.join("cache"),
            runtime_dir: root.join("runtime"),
        }
    }

    fn enabled(threshold: usize) -> AutoSplitConfig {
        AutoSplitConfig {
            enabled: true,
            threshold: Some(threshold),
            strategy: Some(SplitStrategy::Semantic),
        }
    }

    #[test]
    fn counts_live_task_windows_not_panes_or_stale_agents() {
        let mut snapshot = fixture(3);
        snapshot.agents[1].target.window_id = snapshot.agents[0].target.window_id.clone();
        snapshot.agents[2].stale = true;
        assert!(candidates(&snapshot, &enabled(2)).is_empty());
        snapshot.agents[2].stale = false;
        let found = candidates(&snapshot, &enabled(2));
        assert_eq!(found.len(), 1);
        assert_eq!((found[0].session.as_str(), found[0].count), ("$1", 2));
        snapshot.agents[0].thread_name = Some("new task context".into());
        assert_ne!(
            found[0].fingerprint,
            candidates(&snapshot, &enabled(2))[0].fingerprint
        );
        assert!(candidates(&snapshot, &AutoSplitConfig::default()).is_empty());
    }

    #[test]
    fn one_claim_per_server_and_unavailable_does_not_trigger_same_owner_again() {
        let temp = tempfile::tempdir().unwrap();
        let paths = paths(temp.path());
        let server =
            ServerIdentity::from_socket(PathBuf::from("/tmp/auto-split-test.sock")).unwrap();
        let mut snapshot = fixture(3);
        let owner = snapshot.agents[0].clone();
        let config = enabled(3);
        let first = claim(
            &paths, &server, 17, &snapshot, &owner, "thread-0", "$1", &config, 100,
        )
        .unwrap()
        .unwrap();
        snapshot.agents.push({
            let mut a = owner.clone();
            a.target.window_id = "@99".into();
            a
        });
        assert!(
            claim(
                &paths, &server, 17, &snapshot, &owner, "thread-0", "$1", &config, 101
            )
            .unwrap()
            .is_none()
        );
        snapshot.agents.pop();
        finish(&paths, &first.id, "unavailable").unwrap();
        assert!(
            claim(
                &paths, &server, 17, &snapshot, &owner, "thread-0", "$1", &config, 102
            )
            .unwrap()
            .is_none()
        );
        assert!(
            claim(
                &paths,
                &server,
                17,
                &snapshot,
                &snapshot.agents[1],
                "thread-1",
                "$1",
                &config,
                103
            )
            .unwrap()
            .is_some()
        );
    }

    #[test]
    fn explicit_retry_reopens_one_unavailable_ticket() {
        let temp = tempfile::tempdir().unwrap();
        let paths = paths(temp.path());
        let server =
            ServerIdentity::from_socket(PathBuf::from("/tmp/auto-split-retry.sock")).unwrap();
        let snapshot = fixture(3);
        let owner = snapshot.agents[0].clone();
        let config = enabled(3);
        let first = claim(
            &paths, &server, 17, &snapshot, &owner, "thread-0", "$1", &config, 100,
        )
        .unwrap()
        .unwrap();
        finish(&paths, &first.id, "unavailable").unwrap();
        assert!(
            claim(
                &paths, &server, 17, &snapshot, &owner, "thread-0", "$1", &config, 101,
            )
            .unwrap()
            .is_none()
        );

        retry(&paths, &first.id).unwrap();
        assert!(
            claim(
                &paths, &server, 17, &snapshot, &owner, "thread-0", "$1", &config, 102,
            )
            .unwrap()
            .is_some()
        );
    }

    #[test]
    fn claim_waits_for_a_short_lived_ledger_lock() {
        let temp = tempfile::tempdir().unwrap();
        let paths = paths(temp.path());
        let server =
            ServerIdentity::from_socket(PathBuf::from("/tmp/auto-split-lock.sock")).unwrap();
        let dir = ledger_dir(&paths, &server);
        private_dir(&dir).unwrap();
        let lock = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(dir.join("lock"))
            .unwrap();
        lock.lock_exclusive().unwrap();
        let (tx, rx) = std::sync::mpsc::channel();
        let thread = std::thread::spawn(move || {
            let snapshot = fixture(3);
            tx.send(()).unwrap();
            claim(
                &paths,
                &server,
                17,
                &snapshot,
                &snapshot.agents[0],
                "thread-0",
                "$1",
                &enabled(3),
                100,
            )
            .unwrap()
        });
        rx.recv().unwrap();
        std::thread::sleep(Duration::from_millis(50));
        FileExt::unlock(&lock).unwrap();
        assert!(thread.join().unwrap().is_some());
    }

    #[test]
    fn concurrent_claims_only_emit_one_manager_ticket() {
        let temp = tempfile::tempdir().unwrap();
        let paths = paths(temp.path());
        let server =
            ServerIdentity::from_socket(PathBuf::from("/tmp/auto-split-concurrent.sock")).unwrap();
        let snapshot = fixture(4);
        let threads: Vec<_> = (0..12)
            .map(|_| {
                let paths = paths.clone();
                let server = server.clone();
                let snapshot = snapshot.clone();
                std::thread::spawn(move || {
                    claim(
                        &paths,
                        &server,
                        19,
                        &snapshot,
                        &snapshot.agents[0],
                        "thread-0",
                        "$1",
                        &enabled(4),
                        100,
                    )
                    .unwrap()
                })
            })
            .collect();
        let tickets: Vec<_> = threads
            .into_iter()
            .filter_map(|thread| thread.join().unwrap())
            .collect();
        assert_eq!(tickets.len(), 12);
        assert_eq!(
            tickets
                .into_iter()
                .map(|ticket| ticket.id)
                .collect::<BTreeSet<_>>()
                .len(),
            1
        );
        assert_eq!(
            status(&paths).unwrap()["pending"].as_array().unwrap().len(),
            1
        );
    }

    #[test]
    fn pending_claim_is_not_released_when_owner_disappears_or_time_passes() {
        let temp = tempfile::tempdir().unwrap();
        let paths = paths(temp.path());
        let server =
            ServerIdentity::from_socket(PathBuf::from("/tmp/auto-split-orphan.sock")).unwrap();
        let mut snapshot = fixture(3);
        let owner = snapshot.agents[0].clone();
        let config = enabled(2);
        let first = claim(
            &paths, &server, 44, &snapshot, &owner, "thread-0", "$1", &config, 100,
        )
        .unwrap()
        .unwrap();
        snapshot.agents.remove(0);
        assert!(
            claim(
                &paths,
                &server,
                44,
                &snapshot,
                &snapshot.agents[0],
                "thread-1",
                "$1",
                &config,
                u64::MAX
            )
            .unwrap()
            .is_none()
        );
        finish(&paths, &first.id, "failed").unwrap();
        assert!(finish(&paths, &first.id, "failed").is_ok());
        assert!(finish(&paths, &first.id, "completed").is_err());
        assert!(
            claim(
                &paths,
                &server,
                45,
                &snapshot,
                &snapshot.agents[0],
                "thread-1",
                "$1",
                &config,
                200
            )
            .unwrap()
            .is_some()
        );
    }

    #[test]
    fn instruction_routes_to_singleton_manager_with_exact_scope() {
        let temp = tempfile::tempdir().unwrap();
        let paths = paths(temp.path());
        let server =
            ServerIdentity::from_socket(PathBuf::from("/tmp/auto-split-instruction.sock")).unwrap();
        let snapshot = fixture(2);
        let ticket = claim(
            &paths,
            &server,
            22,
            &snapshot,
            &snapshot.agents[0],
            "thread-0",
            "$1",
            &enabled(2),
            100,
        )
        .unwrap()
        .unwrap();
        let text = instruction(&ticket);
        assert!(text.contains("single machine-wide coding-agent manager session"));
        assert!(text.contains("Do not spawn or delegate to another agent"));
        assert!(text.contains("source session $1"));
        assert!(text.contains("/tmp/auto-split-instruction.sock"));
        assert!(text.contains("PID is 22"));
        assert!(text.contains(&ticket.id));
        // Pending tickets written before manager dispatch remain readable.
        let mut legacy = serde_json::to_value(&ticket).unwrap();
        legacy.as_object_mut().unwrap().remove("owner_kind");
        legacy.as_object_mut().unwrap().remove("manager_managed");
        let legacy: Ticket = serde_json::from_value(legacy).unwrap();
        assert!(!legacy.manager_managed);
    }
}
