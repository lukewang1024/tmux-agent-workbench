//! Attachment history belongs to the tmux server, independent of daemon endpoints.
//! Server options survive daemon restarts and disappear with the server, so pane
//! IDs can never be replayed into a new server incarnation.
use std::process::{Command, Stdio};

use sha2::{Digest, Sha256};

use crate::{model::TmuxTarget, server::ServerIdentity};

pub const LAST: &str = "@workbench-resume-last";

pub fn attachment_key(attachment: &str) -> String {
    format!(
        "@workbench-attachment-{:x}",
        Sha256::digest(attachment.as_bytes())
    )
}

pub fn forget(server: &ServerIdentity, key: &str) {
    let _ = output(server, &["set-option", "-gu", key]);
}

pub fn terminal_key(device: &str, terminal: Option<&str>) -> Option<String> {
    terminal.map(|terminal| {
        let hash = Sha256::digest(format!("{device}\0{terminal}").as_bytes());
        format!("@workbench-resume-{hash:x}")
    })
}

fn output(server: &ServerIdentity, args: &[&str]) -> Option<String> {
    let result = Command::new("tmux")
        .args(server.tmux_args())
        .args(args)
        .stderr(Stdio::null())
        .output()
        .ok()?;
    result
        .status
        .success()
        .then(|| String::from_utf8_lossy(&result.stdout).trim().to_owned())
}

pub fn pane_target(server: &ServerIdentity, pane: &str) -> Option<TmuxTarget> {
    if !valid_id(pane, '%') {
        return None;
    }
    let format = "#{session_id}\u{1f}#{session_name}\u{1f}#{window_id}\u{1f}#{window_index}\u{1f}#{window_name}\u{1f}#{pane_id}\u{1f}#{pane_index}";
    let value = output(server, &["display-message", "-p", "-t", pane, format])?;
    let fields: Vec<_> = value.split('\u{1f}').collect();
    if fields.len() != 7 || fields[5] != pane {
        return None;
    }
    Some(TmuxTarget {
        session_id: fields[0].into(),
        session_name: fields[1].into(),
        window_id: fields[2].into(),
        window_index: fields[3].parse().ok()?,
        window_name: fields[4].into(),
        pane_id: fields[5].into(),
        pane_index: fields[6].parse().ok()?,
    })
}

/// Preserve the client's session when a window is linked into several sessions.
pub fn pane_target_in_session(
    server: &ServerIdentity,
    pane: &str,
    session: &str,
) -> Option<TmuxTarget> {
    if !valid_id(session, '$') {
        return None;
    }
    let mut target = pane_target(server, pane)?;
    let selector = format!("{}:{}.{}", session, target.window_id, pane);
    let value = output(
        server,
        &[
            "display-message",
            "-p",
            "-t",
            &selector,
            "#{session_id}\u{1f}#{session_name}",
        ],
    )?;
    let (actual, name) = value.split_once('\u{1f}')?;
    if actual != session {
        return None;
    }
    target.session_id = actual.into();
    target.session_name = name.into();
    Some(target)
}

pub fn remember(server: &ServerIdentity, key: &str, target: &TmuxTarget) {
    if let Ok(value) = serde_json::to_string(target) {
        // Avoid server writes on every detector tick when nothing has changed.
        if output(server, &["show-options", "-gqv", key]).as_deref() != Some(&value) {
            let _ = output(server, &["set-option", "-g", key, &value]);
        }
    }
}

pub fn recalled(server: &ServerIdentity, key: &str) -> Option<TmuxTarget> {
    serde_json::from_str(&output(server, &["show-options", "-gqv", key])?).ok()
}

fn valid_id(value: &str, prefix: char) -> bool {
    value.starts_with(prefix) && value.len() > 1 && value[1..].bytes().all(|b| b.is_ascii_digit())
}

/// Validate ownership as well as existence; linked windows must not send a
/// reconnect into a different session. Explicit sessions never call this path.
pub fn restore(server: &ServerIdentity, target: &TmuxTarget) -> Option<String> {
    if !valid_id(&target.session_id, '$')
        || !valid_id(&target.window_id, '@')
        || !valid_id(&target.pane_id, '%')
    {
        return None;
    }
    let window = format!("{}:{}", target.session_id, target.window_id);
    let session = output(
        server,
        &[
            "display-message",
            "-p",
            "-t",
            &target.session_id,
            "#{session_id}",
        ],
    )?;
    if session != target.session_id {
        return None;
    }
    if output(
        server,
        &["display-message", "-p", "-t", &window, "#{window_id}"],
    )
    .as_deref()
        == Some(&target.window_id)
    {
        output(server, &["select-window", "-t", &window])?;
        let pane = format!("{window}.{}", target.pane_id);
        if output(
            server,
            &["display-message", "-p", "-t", &pane, "#{pane_id}"],
        )
        .as_deref()
            == Some(&target.pane_id)
        {
            output(server, &["select-pane", "-t", &pane])?;
        }
    }
    Some(session)
}

/// Request a redraw only after responsive layout has settled. This does not
/// change the terminal dimensions and is an optional compatibility workaround.
pub fn refresh(server: &ServerIdentity, client: &str) {
    let tmux = crate::tmux::Tmux::new(server.clone());
    if let Some(value) = tmux
        .client_format(client, "#{pane_id} #{session_id}")
        .ok()
        .flatten()
    {
        let mut fields = value.split_whitespace();
        if let (Some(pane), Some(session)) = (fields.next(), fields.next()) {
            if let Some(target) = pane_target_in_session(server, pane, session) {
                remember(server, &attachment_key(client), &target);
            }
        }
    }
    if output(
        server,
        &["show-options", "-gqv", "@workbench-attach-refresh"],
    )
    .as_deref()
        == Some("off")
    {
        return;
    }
    let Some(before) = tmux
        .client_format(client, "#{pane_id} #{pane_pid}")
        .ok()
        .flatten()
    else {
        return;
    };
    if output(server, &["refresh-client", "-t", client]).is_none() {
        return;
    }
    let Some(after) = tmux
        .client_format(client, "#{pane_id} #{pane_pid}")
        .ok()
        .flatten()
    else {
        return;
    };
    // Client navigation during the refresh invalidates the foreground target.
    if before != after {
        return;
    }
    let Some(pid) = after
        .split_whitespace()
        .nth(1)
        .and_then(|pid| pid.parse::<u32>().ok())
    else {
        return;
    };
    let Ok(group) = Command::new("ps")
        .args(["-o", "tpgid=", "-p", &pid.to_string()])
        .output()
    else {
        return;
    };
    if group.status.success() {
        if let Ok(group) = String::from_utf8_lossy(&group.stdout).trim().parse::<i32>() {
            if group > 1 && group != unsafe { libc::getpgrp() } {
                unsafe {
                    libc::kill(-group, libc::SIGWINCH);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn history_is_scoped_to_device_and_terminal_and_legacy_has_no_identity() {
        assert_eq!(
            terminal_key("a", Some("one")),
            terminal_key("a", Some("one"))
        );
        assert_ne!(
            terminal_key("a", Some("one")),
            terminal_key("a", Some("two"))
        );
        assert_ne!(
            terminal_key("a", Some("one")),
            terminal_key("b", Some("one"))
        );
        assert_eq!(terminal_key("a", None), None);
    }
    #[test]
    fn target_ids_are_exact_and_cannot_contain_tmux_target_expressions() {
        assert!(valid_id("%123", '%'));
        for id in ["%", "%1;kill-server", "%1.2", "foo", "%١"] {
            assert!(!valid_id(id, '%'));
        }
    }
}
