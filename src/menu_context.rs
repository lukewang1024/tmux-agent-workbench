//! Live pane identity for menus. Never infer an agent from a title alone.
use crate::{
    ipc::{self, Request},
    manifest::ManifestSet,
    model::{BaseState, Snapshot},
    paths::Paths,
    process::{AgentProcess, ProcessSource, ProcessTree, accepts_terminal_input},
    server::ServerIdentity,
};
use sha2::{Digest, Sha256};
use std::{
    path::PathBuf,
    process::Command,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

pub(crate) type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

pub(crate) struct Context {
    pub pane: String,
    pub session: String,
    pub window: String,
    pub cwd: String,
    pub guard: String,
    pub agent: Option<AgentProcess>,
    pub state: BaseState,
    pub input_available: bool,
    pub team: Vec<Member>,
    pub role: Option<String>,
}

pub(crate) struct Member {
    pub pane: String,
    pub label: String,
}

pub(crate) fn tmux(args: &[&str]) -> Result<String> {
    let mut cmd = Command::new("tmux");
    if let Ok(server) = ServerIdentity::discover() {
        cmd.args(server.tmux_args());
    }
    let output = cmd.args(args).output()?;
    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr)
            .trim()
            .to_string()
            .into());
    }
    Ok(String::from_utf8(output.stdout)?
        .trim_end_matches('\n')
        .into())
}

impl Context {
    pub fn read(paths: &Paths, pane: &str) -> Result<Self> {
        let info = tmux(&[
            "display-message",
            "-p",
            "-t",
            pane,
            "#{pane_id}\n#{pane_pid}\n#{session_id}\n#{window_id}\n#{pane_current_path}\n#{pane_in_mode}\n#{pane_dead}\n#{@agent_team}",
        ])?;
        let lines: Vec<_> = info.split('\n').collect();
        if lines.len() < 7 || lines[0] != pane {
            return Err("pane disappeared".into());
        }
        let root: u32 = lines[1].parse()?;
        let manifests = ManifestSet::load(&paths.manifests_dir())?;
        let agent = ProcessTree::default()
            .agents_for_roots(&[root], &manifests.aliases())
            .remove(&root)
            .filter(|a| foreground(a.fingerprint.pid) && accepts_terminal_input(a));
        let mut state = BaseState::Unknown;
        if let Some(agent) = &agent {
            let content = tmux(&["capture-pane", "-p", "-t", pane, "-S", "-40"])?;
            let title = tmux(&["display-message", "-p", "-t", pane, "#{pane_title}"])?;
            state = manifests.get(agent.kind).classify(&content, &title).state;
            if let Ok(server) = ServerIdentity::discover() {
                let snapshot = ipc::call(
                    &paths.socket_for_server(&server.key),
                    &Request::new("snapshot.get", serde_json::Value::Null),
                    Duration::from_millis(150),
                )
                .ok()
                .and_then(|value| serde_json::from_value::<Snapshot>(value).ok());
                if let Some(snapshot) = snapshot {
                    let now = SystemTime::now().duration_since(UNIX_EPOCH)?.as_millis() as u64;
                    if now.saturating_sub(snapshot.observed_at_unix_ms) < 5000 {
                        if let Some(row) = snapshot.agents.iter().find(|row| {
                            row.target.pane_id == pane
                                && row.process.as_ref() == Some(&agent.fingerprint)
                                && !row.exited
                                && !row.stale
                        }) {
                            // A currently visible approval must override an older idle hook.
                            if state != BaseState::Blocked && row.base_state != BaseState::Unknown {
                                state = row.base_state;
                            }
                        }
                    }
                }
            }
        }
        let ident = lines.get(7).copied().unwrap_or("");
        let (team, role) = team_members(paths, ident, pane, lines[3]);
        let guard = format!(
            "{:x}",
            Sha256::digest(format!(
                "{pane}:{root}:{:?}:{ident}:{}:{}:{}",
                agent.as_ref().map(|a| &a.fingerprint),
                lines[2],
                lines[3],
                lines[4]
            ))
        );
        Ok(Self {
            pane: pane.into(),
            session: lines[2].into(),
            window: lines[3].into(),
            cwd: lines[4].into(),
            guard,
            agent,
            state,
            input_available: lines[5] == "0" && lines[6] == "0",
            team,
            role,
        })
    }
}

// ps is available on both Linux and macOS. A suspended/background CLI must not
// turn an ordinary foreground shell into an agent command target. Fail closed.
fn foreground(pid: u32) -> bool {
    Command::new("ps")
        .args(["-o", "pgid=,tpgid=,stat=", "-p", &pid.to_string()])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .is_some_and(|o| foreground_row(&String::from_utf8_lossy(&o.stdout)))
}
fn foreground_row(row: &str) -> bool {
    let fields: Vec<_> = row.split_whitespace().collect();
    fields.len() == 3
        && fields[0].parse::<u32>().is_ok_and(|id| id > 0)
        && fields[0] == fields[1]
        && !fields[2].contains(['T', 'Z'])
}

fn team_members(
    paths: &Paths,
    id: &str,
    pane: &str,
    window: &str,
) -> (Vec<Member>, Option<String>) {
    if id.is_empty()
        || !id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        return (vec![], None);
    }
    let state = paths
        .state_dir
        .parent()
        .unwrap()
        .join("agent-team")
        .join(id)
        .join("state.json");
    let state = std::fs::metadata(&state)
        .ok()
        .filter(|m| m.len() < 4 * 1024 * 1024)
        .and_then(|_| std::fs::read(state).ok())
        .and_then(|data| serde_json::from_slice::<serde_json::Value>(&data).ok());
    let state = state.filter(|s| {
        s["id"] == id
            && s["window"] == window
            && s["closed"] != true
            && ServerIdentity::discover().ok().is_some_and(|server| {
                s["socket"]
                    .as_str()
                    .is_some_and(|p| PathBuf::from(p) == server.socket_path)
            })
    });
    let mut role = None;
    let members = tmux(&[
        "list-panes",
        "-t",
        window,
        "-F",
        "#{pane_id}\t#{@agent_team}\t#{pane_dead}",
    ])
    .unwrap_or_default()
    .lines()
    .filter_map(|line| {
        let fields: Vec<_> = line.split('\t').collect();
        if fields.len() != 3 || fields[1] != id || fields[2] != "0" {
            return None;
        }
        let detail = state
            .as_ref()
            .and_then(|s| s["members"].as_object())
            .and_then(|members| members.iter().find(|(_, m)| m["pane"] == fields[0]));
        let label = if let Some((name, data)) = detail {
            if fields[0] == pane {
                role = Some(name.clone());
            }
            format!(
                "{name} · {} · {}",
                data["status"].as_str().unwrap_or("unknown"),
                fields[0]
            )
        } else {
            format!("Member {}", fields[0])
        };
        Some(Member {
            pane: fields[0].into(),
            label,
        })
    })
    .collect();
    (members, role)
}

pub(crate) fn executable(name: &str) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::env::var_os("PATH").is_some_and(|path| {
        std::env::split_paths(&path).any(|dir| {
            std::fs::metadata(dir.join(name))
                .is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
        })
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn only_live_foreground_processes_own_input() {
        assert!(foreground_row(" 102 102 S+"));
        for row in [
            "102 103 S",
            "102 102 T",
            "102 102 Z+",
            "0 0 S",
            "102 -1 S",
            "",
            "102 102",
        ] {
            assert!(!foreground_row(row), "{row}");
        }
    }
}
