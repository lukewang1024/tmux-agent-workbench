//! Keep independent interactive agents in separate windows. Session names group
//! task directions; moving a task never changes its process, pane or checkout.
use std::collections::{BTreeMap, HashMap, HashSet};

use crate::model::{ProcessFingerprint, TmuxTarget};
use crate::process::AgentProcess;
use crate::server::ServerIdentity;
use crate::task_title::tmux;
use crate::tmux::Pane;

const SEP: char = '\u{1f}';

pub(crate) fn validate_session(name: &str) -> Result<&str, String> {
    let name = name.trim();
    if name.is_empty()
        || name.chars().count() > 64
        || name.starts_with('-')
        || name
            .chars()
            .any(|c| c.is_control() || matches!(c, '.' | ':'))
        || name.contains("#{")
        || name.contains("#[")
    {
        return Err("use a session direction (1–64 characters), without dots, colons, controls or tmux formats".into());
    }
    Ok(name)
}

pub(crate) fn target(server: &ServerIdentity, pane: &str) -> Result<TmuxTarget, String> {
    let row = tmux(
        server,
        &[
            "display-message",
            "-p",
            "-t",
            pane,
            "#{session_id}\u{1f}#{session_name}\u{1f}#{window_id}\u{1f}#{window_index}\u{1f}#{window_name}\u{1f}#{pane_id}\u{1f}#{pane_index}",
        ],
    )?;
    let fields: Vec<_> = row.split(SEP).collect();
    if fields.len() != 7 || fields[5] != pane {
        return Err("task pane expired during placement".into());
    }
    Ok(TmuxTarget {
        session_id: fields[0].into(),
        session_name: fields[1].into(),
        window_id: fields[2].into(),
        window_index: fields[3].parse().map_err(|_| "invalid window index")?,
        window_name: fields[4].into(),
        pane_id: fields[5].into(),
        pane_index: fields[6].parse().map_err(|_| "invalid pane index")?,
    })
}

fn clients(
    server: &ServerIdentity,
    panes: &HashSet<String>,
) -> Result<Vec<(String, String)>, String> {
    Ok(tmux(
        server,
        &["list-clients", "-F", "#{client_name}\u{1f}#{pane_id}"],
    )?
    .lines()
    .filter_map(|row| row.split_once(SEP))
    .filter(|(_, pane)| panes.contains(*pane))
    .map(|(client, pane)| (client.into(), pane.into()))
    .collect())
}

fn follow(server: &ServerIdentity, clients: &[(String, String)]) {
    for (client, pane) in clients {
        if let Ok(at) = target(server, pane) {
            // A client can disconnect while a placement is in progress.
            let _ = tmux(
                server,
                &[
                    "switch-client",
                    "-c",
                    client,
                    "-t",
                    &at.session_id,
                    ";",
                    "select-window",
                    "-t",
                    &at.window_id,
                    ";",
                    "select-pane",
                    "-t",
                    pane,
                ],
            );
        }
    }
}

pub(crate) fn isolate(
    server: &ServerIdentity,
    panes: &[Pane],
    agents: &HashMap<u32, AgentProcess>,
    interactive: &HashSet<u32>,
) -> bool {
    let mut windows = BTreeMap::<&str, Vec<&Pane>>::new();
    for pane in panes
        .iter()
        .filter(|pane| interactive.contains(&pane.root_pid))
    {
        windows
            .entry(&pane.target.window_id)
            .or_default()
            .push(pane);
    }
    let mut changed = false;
    for (window, mut members) in windows.into_iter().filter(|(_, members)| members.len() > 1) {
        // Explicit agent-team layouts own their panes and are not independent tasks.
        if members.iter().any(|pane| {
            tmux(
                server,
                &[
                    "display-message",
                    "-p",
                    "-t",
                    &pane.target.pane_id,
                    "#{@agent_team}",
                ],
            )
            .is_ok_and(|team| !team.is_empty())
        }) {
            continue;
        }
        let owner = tmux(
            server,
            &["show-option", "-wqv", "-t", window, "@workbench_task_title"],
        )
        .ok()
        .and_then(|value| serde_json::from_str::<serde_json::Value>(&value).ok())
        .and_then(|value| {
            let pane = value["owner"]["pane"].as_str()?;
            let expected: ProcessFingerprint =
                serde_json::from_value(value["owner"]["process"].clone()).ok()?;
            members
                .iter()
                .any(|member| {
                    member.target.pane_id == pane
                        && agents[&member.root_pid].fingerprint == expected
                })
                .then(|| pane.to_owned())
        });
        members.sort_by_key(|pane| {
            let process = &agents[&pane.root_pid].fingerprint;
            (
                owner.as_deref() != Some(pane.target.pane_id.as_str()),
                process.started_at_ticks,
                process.pid,
            )
        });
        for pane in members.into_iter().skip(1) {
            let result = (|| {
                let at = target(server, &pane.target.pane_id)?;
                if at.window_id != window {
                    return Ok(());
                }
                let viewers = clients(server, &HashSet::from([at.pane_id.clone()]))?;
                tmux(
                    server,
                    &["break-pane", "-d", "-s", &at.pane_id, "-n", "agent"],
                )?;
                changed = true;
                follow(server, &viewers);
                Ok::<_, String>(())
            })();
            if let Err(error) = result {
                eprintln!(
                    "tmux-agent-workbench: cannot separate agent pane {}: {error}",
                    pane.target.pane_id
                );
            }
        }
    }
    changed
}

/// Reuse an exact session name. Never rename the launching session or match a
/// prefix, and remove the temporary shell window after moving the task.
pub(crate) fn route(server: &ServerIdentity, pane: &str, name: &str) -> Result<TmuxTarget, String> {
    let name = validate_session(name)?;
    let source = target(server, pane)?;
    if source.session_name == name {
        return Ok(source);
    }
    let sessions = tmux(
        server,
        &["list-sessions", "-F", "#{session_id}\u{1f}#{session_name}"],
    )?;
    let existing = sessions
        .lines()
        .filter_map(|row| row.split_once(SEP))
        .find(|(_, session)| *session == name)
        .map(|(id, _)| id.to_owned());
    let pane_ids = tmux(
        server,
        &["list-panes", "-t", &source.window_id, "-F", "#{pane_id}"],
    )?
    .lines()
    .map(str::to_owned)
    .collect();
    let viewers = clients(server, &pane_ids)?;
    let mut placeholder = None;
    let destination = match existing {
        Some(id) => id,
        None => {
            let cwd = tmux(
                server,
                &["display-message", "-p", "-t", pane, "#{pane_current_path}"],
            )?;
            let created = tmux(
                server,
                &[
                    "new-session",
                    "-d",
                    "-s",
                    name,
                    "-c",
                    &cwd,
                    "-P",
                    "-F",
                    "#{session_id}\u{1f}#{window_id}",
                ],
            )?;
            let (id, window) = created
                .split_once(SEP)
                .ok_or("cannot locate created task session")?;
            placeholder = Some(window.to_owned());
            id.to_owned()
        }
    };
    let result = (|| {
        // Window metadata retains the original workspace even when its display
        // group changes. Supporting panes travel with their task window.
        let workspace = tmux(
            server,
            &[
                "display-message",
                "-p",
                "-t",
                pane,
                "#{@workbench_workspace_root}",
            ],
        )?;
        if !workspace.is_empty() {
            tmux(
                server,
                &[
                    "set-option",
                    "-w",
                    "-t",
                    &source.window_id,
                    "@workbench_workspace_root",
                    &workspace,
                ],
            )?;
        }
        // Moving a session's last window destroys that session. Attach its
        // viewers to the destination first so tmux cannot detach them.
        for (client, _) in &viewers {
            tmux(server, &["switch-client", "-c", client, "-t", &destination])?;
        }
        tmux(
            server,
            &[
                "if-shell",
                "-F",
                "-t",
                pane,
                &format!("#{{==:#{{window_id}},{}}}", source.window_id),
                &format!("move-window -s {} -t {destination}:", source.window_id),
            ],
        )?;
        let actual = target(server, pane)?;
        if actual.session_id != destination {
            return Err("task session verification failed".into());
        }
        Ok(actual)
    })();
    follow(server, &viewers);
    if let Some(window) = placeholder {
        // On failure this also removes the otherwise empty task-owned session.
        let _ = tmux(server, &["kill-window", "-t", &window]);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn session_names_are_exact_safe_labels() {
        assert_eq!(validate_session("  终端工具  ").unwrap(), "终端工具");
        assert!(validate_session("Office O'Reilly").is_ok());
        for invalid in ["", "a:b", "a.b", "-target", "#{session_name}", "a\nb"] {
            assert!(validate_session(invalid).is_err(), "{invalid:?}");
        }
    }
}
