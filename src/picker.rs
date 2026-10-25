use std::process::Command;
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
    let title = match kind {
        PickerKind::Session => crate::ui_language::text("Switch sessions", "切换会话"),
        PickerKind::Agent => crate::ui_language::text("Switch agents", "切换 Agent"),
    };
    let labels: Vec<_> = lines
        .iter()
        .map(|line| line.split(DELIMITER).next().unwrap_or("").to_owned())
        .collect();
    let (pane, client) = context()?;
    let Some(index) = choose_at(title, &labels, &pane, &client)? else {
        return Ok(());
    };
    let selected = &lines[index];
    match kind {
        PickerKind::Session => activate_session(selected, &pane, &client)?,
        PickerKind::Agent => activate_agent(paths, server, &snapshot, selected, &pane, &client)?,
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

/// The same list/menu renderer handles project names, repository paths and
/// snapshot pickers. Values stay with the caller; only an index crosses the UI.
pub fn choose(title: &str, labels: &[String]) -> Result<Option<usize>, Box<dyn std::error::Error>> {
    let (pane, client) = context()?;
    choose_at(title, labels, &pane, &client)
}

fn choose_at(
    title: &str,
    labels: &[String],
    pane: &str,
    client: &str,
) -> Result<Option<usize>, Box<dyn std::error::Error>> {
    let menu = crate::menu::Menu {
        title: title.into(),
        items: labels
            .iter()
            .enumerate()
            .map(|(index, label)| crate::menu::Item {
                id: index.to_string(),
                label: label.clone(),
                key: String::new(),
                command: String::new(),
                enabled: true,
                role: crate::menu::Role::Body,
            })
            .collect(),
        pager: None,
        searchable: true,
        prompt: None,
    };
    Ok(crate::menu::select(&menu, pane, client, 0)?.map(|selected| selected.index))
}

pub fn prompt(title: &str, label: &str) -> Result<Option<String>, Box<dyn std::error::Error>> {
    let (pane, client) = context()?;
    let menu = crate::menu::Menu {
        title: title.into(),
        items: vec![crate::menu::Item {
            id: "submit".into(),
            label: crate::ui_language::text("Create", "创建").into(),
            key: "Enter".into(),
            command: String::new(),
            enabled: true,
            role: crate::menu::Role::Primary,
        }],
        pager: None,
        searchable: false,
        prompt: Some(label.into()),
    };
    Ok(crate::menu::select(&menu, &pane, &client, 0)?.map(|selected| selected.input))
}

fn context() -> Result<(String, String), Box<dyn std::error::Error>> {
    let server = ServerIdentity::discover()?;
    let tmux = crate::tmux::Tmux::new(server.clone());
    let pane = std::env::var("WORKBENCH_SOURCE_PANE")
        .or_else(|_| std::env::var("TMUX_PANE"))
        .ok();
    if let Ok(client) = std::env::var("WORKBENCH_TARGET_CLIENT") {
        let pane = pane
            .or(tmux.client_format(&client, "#{pane_id}")?)
            .ok_or("picker client disappeared")?;
        return Ok((pane, client));
    }
    let clients = Command::new("tmux")
        .args(server.tmux_args())
        .args(["list-clients", "-F", "#{client_name}\t#{pane_id}"])
        .output()?;
    let clients = String::from_utf8(clients.stdout)?;
    let candidates: Vec<_> = clients
        .lines()
        .filter_map(|line| line.split_once('\t'))
        .filter(|(_, target)| pane.as_deref().is_none_or(|pane| pane == *target))
        .collect();
    if candidates.len() != 1 {
        return Err("picker needs WORKBENCH_TARGET_CLIENT for an unambiguous tmux client".into());
    }
    Ok((
        pane.unwrap_or_else(|| candidates[0].1.into()),
        candidates[0].0.into(),
    ))
}

fn activate_session(
    selected: &str,
    source: &str,
    client: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    let fields: Vec<_> = selected.split(DELIMITER).collect();
    if fields.len() != 4 {
        return Err("invalid session picker result".into());
    }
    spawn_focus(fields[1], fields[2], fields[3], source, client)
}

fn activate_agent(
    paths: &Paths,
    server: &ServerIdentity,
    snapshot: &Snapshot,
    selected: &str,
    source: &str,
    client: &str,
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
            .args(["display-message", "-c", client, &summary])
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
        source,
        client,
    )
}

fn spawn_focus(
    session: &str,
    window: &str,
    pane: &str,
    source: &str,
    client: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    let status = Command::new(std::env::current_exe()?)
        .args([
            "focus",
            "--responsive",
            "--session",
            session,
            "--window",
            window,
            "--pane",
            pane,
            "--source-pane",
            source,
            "--client",
            client,
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
                current_path: None,
                active: false,
                last_active_window_id: Some("@1".into()),
                last_active_pane_id: Some("%1".into()),
            }],
            agents: vec![],
            clients: vec![],
        };
        assert_eq!(session_lines(&snapshot).len(), 1);
    }
}
