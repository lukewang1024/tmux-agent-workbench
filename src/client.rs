use std::collections::{HashMap, HashSet};

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::model::{ClientSnapshot, TmuxTarget};
use crate::semantic::SemanticEvent;

pub const HEARTBEAT_SECONDS: u64 = 15;
pub const OFFLINE_AFTER_MS: u64 = 45_000;
pub const DETACH_GRACE_MS: u64 = 5 * 60_000;
pub const ATTACHMENT_TOKEN_TTL_MS: u64 = 60_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FocusState {
    Focused,
    Unfocused,
    Unknown,
}

#[derive(Debug, Clone)]
pub struct Endpoint {
    pub id: String,
    pub device_id: String,
    pub terminal_id: Option<String>,
    pub device_label: String,
    pub kind: String,
    pub capabilities: HashSet<String>,
    pub focus: FocusState,
    pub overlay_visible: bool,
    pub active_target: Option<TmuxTarget>,
    pub attachment: Option<String>,
    previous_attachment: Option<String>,
    pub last_activity_ms: u64,
    pub detached_at_ms: Option<u64>,
    pending: Vec<SemanticEvent>,
}

#[derive(Debug, Clone)]
struct BindToken {
    endpoint_id: String,
    expires_ms: u64,
    used: bool,
}

#[derive(Debug, Default)]
pub struct ClientRegistry {
    endpoints: HashMap<String, Endpoint>,
    bind_tokens: HashMap<String, BindToken>,
}

impl ClientRegistry {
    /// Rebuild an endpoint from independently locked control and PTY leases.
    /// The daemon checks the current terminal owner before calling this.
    pub fn recover(
        &mut self,
        record: &crate::client_lease::ControlRecord,
        attachment: Option<String>,
        now_ms: u64,
    ) {
        if self.endpoints.contains_key(&record.endpoint_id) {
            return;
        }
        // Retire stale registry entries, but do not detach a tty during
        // recovery: a transport address can have been reused in the meantime.
        self.replace_terminal(&record.device_id, record.terminal_id.as_deref());
        if let Some(attachment) = &attachment {
            self.retire_attachment(attachment, &record.endpoint_id);
        }
        self.endpoints.insert(
            record.endpoint_id.clone(),
            Endpoint {
                id: record.endpoint_id.clone(),
                device_id: record.device_id.clone(),
                terminal_id: record.terminal_id.clone(),
                device_label: record.device_label.clone(),
                kind: record.kind.clone(),
                capabilities: record.capabilities.iter().cloned().collect(),
                focus: FocusState::Unknown,
                overlay_visible: false,
                active_target: None,
                attachment: attachment.clone(),
                previous_attachment: None,
                last_activity_ms: now_ms,
                detached_at_ms: None,
                pending: Vec::new(),
            },
        );
        if attachment.is_none() {
            self.bind_tokens.insert(
                record.token.clone(),
                BindToken {
                    endpoint_id: record.endpoint_id.clone(),
                    expires_ms: record
                        .registered_at_ms
                        .saturating_add(ATTACHMENT_TOKEN_TTL_MS),
                    used: false,
                },
            );
        }
    }

    pub fn register(
        &mut self,
        device_id: String,
        device_label: String,
        kind: String,
        capabilities: Vec<String>,
        now_ms: u64,
    ) -> (String, String) {
        self.register_terminal(device_id, None, device_label, kind, capabilities, now_ms)
    }

    pub fn register_terminal(
        &mut self,
        device_id: String,
        terminal_id: Option<String>,
        device_label: String,
        kind: String,
        capabilities: Vec<String>,
        now_ms: u64,
    ) -> (String, String) {
        let endpoint_id = Uuid::new_v4().to_string();
        let token = Uuid::new_v4().to_string();
        self.register_terminal_with_ids(
            device_id,
            terminal_id,
            device_label,
            kind,
            capabilities,
            now_ms,
            endpoint_id.clone(),
            token.clone(),
        );
        (endpoint_id, token)
    }

    #[allow(clippy::too_many_arguments)]
    pub fn register_terminal_with_ids(
        &mut self,
        device_id: String,
        terminal_id: Option<String>,
        device_label: String,
        kind: String,
        capabilities: Vec<String>,
        now_ms: u64,
        endpoint_id: String,
        token: String,
    ) {
        self.endpoints.insert(
            endpoint_id.clone(),
            Endpoint {
                id: endpoint_id.clone(),
                device_id,
                terminal_id,
                device_label,
                kind,
                capabilities: capabilities.into_iter().collect(),
                focus: FocusState::Unknown,
                overlay_visible: false,
                active_target: None,
                attachment: None,
                previous_attachment: None,
                last_activity_ms: now_ms,
                detached_at_ms: None,
                pending: Vec::new(),
            },
        );
        self.bind_tokens.insert(
            token.clone(),
            BindToken {
                endpoint_id: endpoint_id.clone(),
                expires_ms: now_ms + ATTACHMENT_TOKEN_TTL_MS,
                used: false,
            },
        );
    }

    pub fn bind(&mut self, token: &str, attachment: String, now_ms: u64) -> Result<String, String> {
        let bind = self
            .bind_tokens
            .get(token)
            .ok_or("unknown attachment token")?;
        if bind.used || now_ms > bind.expires_ms {
            return Err("expired or used attachment token".into());
        }
        let endpoint_id = bind.endpoint_id.clone();
        self.retire_attachment(&attachment, &endpoint_id);
        let endpoint = self
            .endpoints
            .get_mut(&endpoint_id)
            .ok_or("endpoint is no longer online")?;
        self.bind_tokens.get_mut(token).expect("token exists").used = true;
        endpoint.attachment = Some(attachment);
        endpoint.detached_at_ms = None;
        endpoint.last_activity_ms = now_ms;
        Ok(endpoint.id.clone())
    }

    fn retire_attachment(&mut self, attachment: &str, endpoint_id: &str) {
        // PTY names are reusable transport addresses, not terminal identities.
        // A later bind must retire stale associations before a detach hook can
        // attribute this tty's last pane to another terminal's history.
        for (id, old) in &mut self.endpoints {
            if id != endpoint_id {
                if old.previous_attachment.as_deref() == Some(attachment) {
                    old.previous_attachment = None;
                }
                if old.attachment.as_deref() == Some(attachment) {
                    old.attachment = None;
                    old.active_target = None;
                    old.focus = FocusState::Unknown;
                }
            }
        }
    }

    pub fn heartbeat(&mut self, endpoint_id: &str, now_ms: u64) -> Result<(), String> {
        let endpoint = self
            .endpoints
            .get_mut(endpoint_id)
            .ok_or("unknown endpoint")?;
        endpoint.last_activity_ms = now_ms;
        Ok(())
    }

    pub fn detach(&mut self, endpoint_id: &str, now_ms: u64) -> Result<(), String> {
        let endpoint = self
            .endpoints
            .get_mut(endpoint_id)
            .ok_or("unknown endpoint")?;
        endpoint.previous_attachment = endpoint.attachment.take();
        endpoint.detached_at_ms = Some(now_ms);
        endpoint.focus = FocusState::Unknown;
        endpoint.active_target = None;
        Ok(())
    }

    /// Reconnect only replaces this terminal, never another terminal on the device.
    /// Legacy clients without a terminal identity cannot safely claim an old one.
    pub fn replace_terminal(&mut self, device_id: &str, terminal_id: Option<&str>) -> Vec<String> {
        let Some(terminal_id) = terminal_id else {
            return Vec::new();
        };
        let replaced_ids: Vec<_> = self
            .endpoints
            .iter()
            .filter(|(_, endpoint)| {
                endpoint.device_id == device_id
                    && endpoint.terminal_id.as_deref() == Some(terminal_id)
            })
            .map(|(id, _)| id.clone())
            .collect();
        let mut attachments = Vec::new();
        for endpoint_id in &replaced_ids {
            if let Some(endpoint) = self.endpoints.remove(endpoint_id) {
                if let Some(attachment) = endpoint.attachment.or(endpoint.previous_attachment) {
                    if !attachments.contains(&attachment) {
                        attachments.push(attachment);
                    }
                }
            }
        }
        self.bind_tokens
            .retain(|_, token| !replaced_ids.contains(&token.endpoint_id));
        attachments
    }

    pub fn resume_key(&self, endpoint_id: &str) -> Option<String> {
        let endpoint = self.endpoints.get(endpoint_id)?;
        crate::attachment::terminal_key(&endpoint.device_id, endpoint.terminal_id.as_deref())
    }

    pub fn resume_keys_for_attachment(&self, attachment: &str) -> Vec<String> {
        self.endpoints
            .values()
            .filter(|endpoint| {
                endpoint.attachment.as_deref() == Some(attachment)
                    || endpoint.previous_attachment.as_deref() == Some(attachment)
            })
            .filter_map(|endpoint| {
                crate::attachment::terminal_key(
                    &endpoint.device_id,
                    endpoint.terminal_id.as_deref(),
                )
            })
            .collect()
    }

    pub fn attachment(&self, endpoint_id: &str) -> Result<Option<&str>, String> {
        self.endpoints
            .get(endpoint_id)
            .map(|endpoint| endpoint.attachment.as_deref())
            .ok_or_else(|| "unknown endpoint".into())
    }

    pub fn queue(&mut self, endpoint_id: &str, event: SemanticEvent) -> Result<(), String> {
        let endpoint = self
            .endpoints
            .get_mut(endpoint_id)
            .ok_or("unknown endpoint")?;
        if !endpoint.pending.iter().any(|queued| queued.id == event.id) {
            endpoint.pending.push(event);
        }
        Ok(())
    }

    pub fn retain_events(&mut self, valid: &HashSet<String>) {
        for endpoint in self.endpoints.values_mut() {
            endpoint.pending.retain(|event| valid.contains(&event.id));
        }
    }

    pub fn take_pending(
        &mut self,
        endpoint_id: &str,
        now_ms: u64,
    ) -> Result<Vec<SemanticEvent>, String> {
        let endpoint = self
            .endpoints
            .get_mut(endpoint_id)
            .ok_or("unknown endpoint")?;
        endpoint
            .pending
            .retain(|event| now_ms <= event.deadline_unix_ms);
        Ok(std::mem::take(&mut endpoint.pending))
    }

    pub fn update_focus(
        &mut self,
        endpoint_id: &str,
        focus: FocusState,
        overlay_visible: bool,
        target: Option<TmuxTarget>,
        now_ms: u64,
    ) -> Result<(), String> {
        let endpoint = self
            .endpoints
            .get_mut(endpoint_id)
            .ok_or("unknown endpoint")?;
        endpoint.focus = focus;
        endpoint.overlay_visible = overlay_visible;
        endpoint.active_target = target;
        endpoint.last_activity_ms = now_ms;
        Ok(())
    }

    pub fn update_attachment_focus(
        &mut self,
        attachment: &str,
        focus: FocusState,
        overlay_visible: bool,
        target: Option<TmuxTarget>,
        now_ms: u64,
    ) {
        for endpoint in self
            .endpoints
            .values_mut()
            .filter(|endpoint| endpoint.attachment.as_deref() == Some(attachment))
        {
            endpoint.focus = focus;
            endpoint.overlay_visible = overlay_visible;
            endpoint.active_target = target.clone();
            endpoint.last_activity_ms = now_ms;
        }
    }

    pub fn focused_viewer(&self, pane_id: &str, now_ms: u64) -> Option<&Endpoint> {
        self.endpoints
            .values()
            .filter(|endpoint| {
                now_ms.saturating_sub(endpoint.last_activity_ms) <= OFFLINE_AFTER_MS
                    && endpoint.kind != "termux"
                    && endpoint.focus == FocusState::Focused
                    && !endpoint.overlay_visible
                    && endpoint
                        .active_target
                        .as_ref()
                        .is_some_and(|target| target.pane_id == pane_id)
            })
            .max_by_key(|endpoint| endpoint.last_activity_ms)
    }

    pub fn ranked(&self, capability: &str, now_ms: u64) -> Vec<&Endpoint> {
        let mut endpoints: Vec<_> = self
            .endpoints
            .values()
            .filter(|endpoint| {
                now_ms.saturating_sub(endpoint.last_activity_ms) <= OFFLINE_AFTER_MS
                    && endpoint.capabilities.contains(capability)
            })
            .collect();
        endpoints.sort_by_key(|endpoint| std::cmp::Reverse(endpoint.last_activity_ms));
        endpoints
    }

    pub fn prune(&mut self, now_ms: u64) {
        self.bind_tokens
            .retain(|_, token| !token.used && now_ms <= token.expires_ms);
        self.endpoints.retain(|_, endpoint| {
            let heartbeat_live =
                now_ms.saturating_sub(endpoint.last_activity_ms) <= OFFLINE_AFTER_MS;
            let detach_live = endpoint
                .detached_at_ms
                .is_some_and(|at| now_ms.saturating_sub(at) <= DETACH_GRACE_MS);
            heartbeat_live || detach_live
        });
    }

    pub fn snapshots(&self, now_ms: u64) -> Vec<ClientSnapshot> {
        let mut result: Vec<_> = self
            .endpoints
            .values()
            .map(|endpoint| ClientSnapshot {
                device_label: endpoint.device_label.clone(),
                kind: endpoint.kind.clone(),
                capabilities: endpoint.capabilities.iter().cloned().collect(),
                presence: if endpoint.detached_at_ms.is_some() {
                    "detached"
                } else if now_ms.saturating_sub(endpoint.last_activity_ms) <= OFFLINE_AFTER_MS {
                    "online"
                } else {
                    "grace"
                }
                .into(),
                attachment: endpoint.attachment.clone(),
                focus: format!("{:?}", endpoint.focus).to_lowercase(),
                active_target: endpoint.active_target.clone(),
                activity_unix_ms: endpoint.last_activity_ms,
            })
            .collect();
        result.sort_by_key(|client| std::cmp::Reverse(client.activity_unix_ms));
        result
    }

    pub fn pending_events(&self) -> Vec<SemanticEvent> {
        let mut seen = HashSet::new();
        self.endpoints
            .values()
            .flat_map(|endpoint| endpoint.pending.iter())
            .filter(|event| seen.insert(event.id.clone()))
            .cloned()
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn target(pane: &str) -> TmuxTarget {
        TmuxTarget {
            session_id: "$1".into(),
            session_name: "s".into(),
            window_id: "@1".into(),
            window_index: 1,
            window_name: "w".into(),
            pane_id: pane.into(),
            pane_index: 1,
        }
    }

    #[test]
    fn seen_requires_focus_target_and_no_overlay() {
        let mut registry = ClientRegistry::default();
        let (id, token) = registry.register(
            "d".into(),
            "phone".into(),
            "macos".into(),
            vec!["sound".into()],
            0,
        );
        registry.bind(&token, "/dev/pts/1".into(), 1).unwrap();
        registry
            .update_focus(&id, FocusState::Focused, true, Some(target("%1")), 2)
            .unwrap();
        assert!(registry.focused_viewer("%1", 3).is_none());
        registry
            .update_focus(&id, FocusState::Focused, false, Some(target("%1")), 4)
            .unwrap();
        assert_eq!(registry.focused_viewer("%1", 5).unwrap().id, id);
        assert!(registry.focused_viewer("%2", 5).is_none());
    }

    #[test]
    fn termux_is_not_treated_as_visible_when_android_is_backgrounded() {
        let mut registry = ClientRegistry::default();
        let (id, token) = registry.register(
            "d".into(),
            "phone".into(),
            "termux".into(),
            vec!["notification".into()],
            0,
        );
        registry.bind(&token, "/dev/pts/1".into(), 1).unwrap();
        registry
            .update_focus(&id, FocusState::Focused, false, Some(target("%1")), 2)
            .unwrap();
        assert!(registry.focused_viewer("%1", 3).is_none());
    }

    #[test]
    fn bind_token_is_single_use_and_expires() {
        let mut registry = ClientRegistry::default();
        let (_, token) = registry.register("d".into(), "mac".into(), "macos".into(), vec![], 0);
        registry.bind(&token, "tty".into(), 1).unwrap();
        assert!(registry.bind(&token, "tty2".into(), 2).is_err());
        let (_, old) = registry.register("e".into(), "linux".into(), "linux".into(), vec![], 0);
        assert!(
            registry
                .bind(&old, "tty".into(), ATTACHMENT_TOKEN_TTL_MS + 1)
                .is_err()
        );
    }

    #[test]
    fn resume_identity_is_retained_on_detach_without_crossing_terminal_boundaries() {
        let mut registry = ClientRegistry::default();
        let (a, token_a) = registry.register_terminal(
            "phone".into(),
            Some("a".into()),
            "phone".into(),
            "termux".into(),
            vec![],
            0,
        );
        let (b, token_b) = registry.register_terminal(
            "phone".into(),
            Some("b".into()),
            "phone".into(),
            "termux".into(),
            vec![],
            0,
        );
        registry.bind(&token_a, "/dev/pts/1".into(), 1).unwrap();
        registry.bind(&token_b, "/dev/pts/2".into(), 1).unwrap();
        assert_ne!(registry.resume_key(&a), registry.resume_key(&b));
        assert_eq!(
            registry.resume_keys_for_attachment("/dev/pts/1"),
            vec![registry.resume_key(&a).unwrap()]
        );
        registry.detach(&a, 2).unwrap();
        assert_eq!(
            registry.resume_keys_for_attachment("/dev/pts/1"),
            vec![registry.resume_key(&a).unwrap()]
        );
        assert!(
            registry
                .resume_keys_for_attachment("/dev/pts/missing")
                .is_empty()
        );
    }

    #[test]
    fn reused_pty_cannot_overwrite_another_terminals_history_or_detach_it() {
        let mut registry = ClientRegistry::default();
        let (a, token_a) = registry.register_terminal(
            "phone".into(),
            Some("a".into()),
            "phone".into(),
            "termux".into(),
            vec![],
            0,
        );
        registry.bind(&token_a, "/dev/pts/1".into(), 1).unwrap();
        registry.detach(&a, 2).unwrap();
        let (b, token_b) = registry.register_terminal(
            "phone".into(),
            Some("b".into()),
            "phone".into(),
            "termux".into(),
            vec![],
            3,
        );
        registry.bind(&token_b, "/dev/pts/1".into(), 4).unwrap();
        assert_eq!(
            registry.resume_keys_for_attachment("/dev/pts/1"),
            vec![registry.resume_key(&b).unwrap()]
        );
        assert!(registry.replace_terminal("phone", Some("a")).is_empty());
        assert_eq!(registry.attachment(&b).unwrap(), Some("/dev/pts/1"));
    }

    #[test]
    fn attachment_is_available_only_after_binding() {
        let mut registry = ClientRegistry::default();
        let (id, token) = registry.register_terminal(
            "d".into(),
            Some("terminal-a".into()),
            "phone".into(),
            "termux".into(),
            vec![],
            0,
        );
        assert_eq!(registry.attachment(&id).unwrap(), None);
        registry.bind(&token, "/dev/pts/7".into(), 1).unwrap();
        assert_eq!(registry.attachment(&id).unwrap(), Some("/dev/pts/7"));
    }

    #[test]
    fn expired_endpoint_can_recover_its_live_binding_without_crossing_ttys() {
        let mut registry = ClientRegistry::default();
        let (id, token) = registry.register_terminal(
            "device".into(),
            Some("terminal".into()),
            "phone".into(),
            "termux".into(),
            vec!["notification".into()],
            0,
        );
        registry.bind(&token, "/dev/pts/7".into(), 1).unwrap();
        registry.prune(OFFLINE_AFTER_MS + 2);
        assert!(registry.heartbeat(&id, OFFLINE_AFTER_MS + 3).is_err());
        let (other, other_token) = registry.register(
            "other".into(),
            "other".into(),
            "macos".into(),
            vec![],
            OFFLINE_AFTER_MS + 3,
        );
        registry
            .bind(&other_token, "/dev/pts/7".into(), OFFLINE_AFTER_MS + 4)
            .unwrap();
        let record = crate::client_lease::ControlRecord {
            endpoint_id: id.clone(),
            device_id: "device".into(),
            terminal_id: Some("terminal".into()),
            device_label: "phone".into(),
            kind: "termux".into(),
            capabilities: vec!["notification".into()],
            token,
            registered_at_ms: 0,
        };
        registry.recover(&record, Some("/dev/pts/7".into()), OFFLINE_AFTER_MS + 5);
        assert_eq!(registry.attachment(&id).unwrap(), Some("/dev/pts/7"));
        assert_eq!(registry.attachment(&other).unwrap(), None);
        assert_eq!(
            registry.ranked("notification", OFFLINE_AFTER_MS + 5)[0].id,
            id
        );
    }

    #[test]
    fn reconnect_replaces_the_same_terminal_and_recovers_its_attachment() {
        let mut registry = ClientRegistry::default();
        let (old_id, token) = registry.register_terminal(
            "d".into(),
            Some("terminal-a".into()),
            "phone".into(),
            "termux".into(),
            vec![],
            0,
        );
        registry.bind(&token, "/dev/pts/7".into(), 1).unwrap();
        registry.detach(&old_id, 2).unwrap();

        assert_eq!(
            registry.replace_terminal("d", Some("terminal-a")),
            vec!["/dev/pts/7"]
        );
        assert!(registry.attachment(&old_id).is_err());
    }

    #[test]
    fn reconnect_does_not_replace_another_device() {
        let mut registry = ClientRegistry::default();
        let (id, token) =
            registry.register("other".into(), "mac".into(), "macos".into(), vec![], 0);
        registry.bind(&token, "/dev/pts/8".into(), 1).unwrap();

        assert!(
            registry
                .replace_terminal("d", Some("terminal-a"))
                .is_empty()
        );
        assert_eq!(registry.attachment(&id).unwrap(), Some("/dev/pts/8"));
    }

    #[test]
    fn reconnect_preserves_sibling_terminal_and_invalidates_only_old_tokens() {
        let mut registry = ClientRegistry::default();
        let (a, a_token) = registry.register_terminal(
            "phone".into(),
            Some("a".into()),
            "phone".into(),
            "termux".into(),
            vec![],
            0,
        );
        let (b, b_token) = registry.register_terminal(
            "phone".into(),
            Some("b".into()),
            "phone".into(),
            "termux".into(),
            vec![],
            0,
        );
        registry.bind(&a_token, "/dev/pts/1".into(), 1).unwrap();
        assert_eq!(
            registry.replace_terminal("phone", Some("a")),
            vec!["/dev/pts/1"]
        );
        assert!(registry.bind(&a_token, "/dev/pts/3".into(), 2).is_err());
        registry.bind(&b_token, "/dev/pts/2".into(), 2).unwrap();
        let (new_a, token) = registry.register_terminal(
            "phone".into(),
            Some("a".into()),
            "phone".into(),
            "termux".into(),
            vec![],
            3,
        );
        registry.bind(&token, "/dev/pts/3".into(), 4).unwrap();
        assert!(registry.detach(&a, 5).is_err());
        assert_eq!(registry.attachment(&b).unwrap(), Some("/dev/pts/2"));
        assert_eq!(registry.attachment(&new_a).unwrap(), Some("/dev/pts/3"));
    }

    #[test]
    fn legacy_connections_never_evict_other_terminals() {
        let mut registry = ClientRegistry::default();
        let (id, token) =
            registry.register("phone".into(), "phone".into(), "termux".into(), vec![], 0);
        registry.bind(&token, "/dev/pts/1".into(), 1).unwrap();
        assert!(registry.replace_terminal("phone", None).is_empty());
        assert!(registry.replace_terminal("phone", Some("new")).is_empty());
        assert_eq!(registry.attachment(&id).unwrap(), Some("/dev/pts/1"));
    }
}
