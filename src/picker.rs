use std::io::Write;
use std::process::{Command, Stdio};
use std::time::Duration;

use crate::ipc::{Request, call};
use crate::model::{DisplayState, Snapshot};
use crate::paths::Paths;
use crate::server::ServerIdentity;

const DELIMITER: char = '\u{1f}';

#[derive(Debug, Clone, Copy)]
pub enum PickerKind {
    Session,
    Agent,
}

pub fn run(
    paths: &Paths,
    server: &ServerIdentity,
    kind: PickerKind,
) -> Result<(), Box<dyn std::error::Error>> {
    let value = call(
        &paths.socket_for_server(&server.key),
        &Request::new("snapshot.get", serde_json::Value::Null),
        Duration::from_secs(2),
    )?;
    let snapshot: Snapshot = serde_json::from_value(value)?;
    let lines = match kind {
        PickerKind::Session => session_lines(&snapshot),
        PickerKind::Agent => agent_lines(&snapshot),
    };
    if lines.is_empty() {
        return Err("no picker entries".into());
    }
    let selected = fzf(&lines)?;
    let Some(selected) = selected else {
        return Ok(());
    };
    match kind {
        PickerKind::Session => activate_session(&selected)?,
        PickerKind::Agent => activate_agent(paths, server, &snapshot, &selected)?,
    }
    Ok(())
}

fn session_lines(snapshot: &Snapshot) -> Vec<String> {
    let mut sessions = snapshot.sessions.clone();
    sessions.sort_by_key(|session| session.attention_count == 0);
    sessions
        .into_iter()
        .map(|session| {
            format!(
                "{} {}  {} agents · {} attention{d}{}{d}{}{d}{}",
                glyph(session.rollup_state),
                session.session_name,
                session.agent_count,
                session.attention_count,
                session.session_id,
                session.last_active_window_id.unwrap_or_default(),
                session.last_active_pane_id.unwrap_or_default(),
                d = DELIMITER,
            )
        })
        .collect()
}

fn agent_lines(snapshot: &Snapshot) -> Vec<String> {
    let session_order: std::collections::HashMap<_, _> = snapshot
        .sessions
        .iter()
        .enumerate()
        .map(|(index, session)| (session.session_id.as_str(), index))
        .collect();
    let mut agents = snapshot.agents.clone();
    agents.sort_by_key(|agent| {
        (
            agent.attention.as_ref().is_none_or(|event| event.seen),
            agent
                .attention
                .as_ref()
                .map(|event| event.since_unix_ms)
                .unwrap_or(u64::MAX),
            session_order
                .get(agent.target.session_id.as_str())
                .copied()
                .unwrap_or(usize::MAX),
            agent.target.window_index,
            agent.target.pane_index,
        )
    });
    agents
        .into_iter()
        .map(|agent| {
            let suffix = if agent.exited { " · exited" } else { "" };
            format!(
                "{} {} · {}{}{d}{}",
                glyph(agent.display_state),
                agent.label,
                agent.target.session_name,
                suffix,
                agent.instance_id,
                d = DELIMITER,
            )
        })
        .collect()
}

fn fzf(lines: &[String]) -> Result<Option<String>, Box<dyn std::error::Error>> {
    let mut child = Command::new("fzf-tmux")
        .args([
            "-p",
            "80%,70%",
            "--",
            "--delimiter",
            "\u{1f}",
            "--with-nth",
            "1",
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()?;
    {
        let mut stdin = child.stdin.take().ok_or("fzf-tmux stdin unavailable")?;
        for line in lines {
            writeln!(stdin, "{line}")?;
        }
    }
    let output = child.wait_with_output()?;
    if !output.status.success() {
        return Ok(None);
    }
    Ok(Some(
        String::from_utf8(output.stdout)?.trim_end().to_owned(),
    ))
}

fn activate_session(selected: &str) -> Result<(), Box<dyn std::error::Error>> {
    let fields: Vec<_> = selected.split(DELIMITER).collect();
    if fields.len() != 4 {
        return Err("invalid session picker result".into());
    }
    spawn_focus(fields[1], fields[2], fields[3])
}

fn activate_agent(
    paths: &Paths,
    server: &ServerIdentity,
    snapshot: &Snapshot,
    selected: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    let id = selected
        .split_once(DELIMITER)
        .map(|(_, id)| id)
        .ok_or("invalid agent picker result")?;
    let agent = snapshot
        .agents
        .iter()
        .find(|agent| agent.instance_id == id)
        .ok_or("agent picker target expired")?;
    if agent.exited {
        let summary = format!(
            "Workbench: completed · {} · {}",
            agent.label,
            agent.reason_category.as_deref().unwrap_or("done")
        );
        let _ = Command::new("tmux")
            .arg("-S")
            .arg(&server.socket_path)
            .args(["display-message", &summary])
            .status();
        if let Some(event) = &agent.attention {
            call(
                &paths.socket_for_server(&server.key),
                &Request::new("attention.ack", serde_json::json!({"event_id": event.id})),
                Duration::from_secs(1),
            )?;
        }
        return Ok(());
    }
    spawn_focus(
        &agent.target.session_id,
        &agent.target.window_id,
        &agent.target.pane_id,
    )
}

fn spawn_focus(session: &str, window: &str, pane: &str) -> Result<(), Box<dyn std::error::Error>> {
    let status = Command::new(std::env::current_exe()?)
        .args([
            "focus",
            "--session",
            session,
            "--window",
            window,
            "--pane",
            pane,
        ])
        .status()?;
    if status.success() {
        Ok(())
    } else {
        Err("focus target expired".into())
    }
}

fn glyph(state: DisplayState) -> &'static str {
    match state {
        DisplayState::Blocked => "!",
        DisplayState::Done => "✓",
        DisplayState::Working => "●",
        DisplayState::Idle => "○",
        DisplayState::Unknown => "?",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::SessionSnapshot;

    #[test]
    fn session_picker_keeps_no_agent_sessions() {
        let snapshot = Snapshot {
            schema_version: crate::SNAPSHOT_SCHEMA_VERSION,
            server: "s".into(),
            generation: 1,
            observed_at_unix_ms: 1,
            sessions: vec![SessionSnapshot {
                session_id: "$1".into(),
                session_name: "shell".into(),
                rollup_state: DisplayState::Unknown,
                agent_count: 0,
                attention_count: 0,
                active: false,
                last_active_window_id: Some("@1".into()),
                last_active_pane_id: Some("%1".into()),
            }],
            agents: vec![],
        };
        assert_eq!(session_lines(&snapshot).len(), 1);
    }
}
