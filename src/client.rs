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
    pub device_label: String,
    pub kind: String,
    pub capabilities: HashSet<String>,
    pub focus: FocusState,
    pub overlay_visible: bool,
    pub active_target: Option<TmuxTarget>,
    pub attachment: Option<String>,
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
    pub fn register(
        &mut self,
        device_id: String,
        device_label: String,
        kind: String,
        capabilities: Vec<String>,
        now_ms: u64,
    ) -> (String, String) {
        let endpoint_id = Uuid::new_v4().to_string();
        let token = Uuid::new_v4().to_string();
        self.endpoints.insert(
            endpoint_id.clone(),
            Endpoint {
                id: endpoint_id.clone(),
                device_id,
                device_label,
                kind,
                capabilities: capabilities.into_iter().collect(),
                focus: FocusState::Unknown,
                overlay_visible: false,
                active_target: None,
                attachment: None,
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
        (endpoint_id, token)
    }

    pub fn bind(&mut self, token: &str, attachment: String, now_ms: u64) -> Result<String, String> {
        let bind = self
            .bind_tokens
            .get_mut(token)
            .ok_or("unknown attachment token")?;
        if bind.used || now_ms > bind.expires_ms {
            return Err("expired or used attachment token".into());
        }
        let endpoint = self
            .endpoints
            .get_mut(&bind.endpoint_id)
            .ok_or("endpoint is no longer online")?;
        bind.used = true;
        endpoint.attachment = Some(attachment);
        endpoint.detached_at_ms = None;
        endpoint.last_activity_ms = now_ms;
        Ok(endpoint.id.clone())
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
        endpoint.attachment = None;
        endpoint.detached_at_ms = Some(now_ms);
        endpoint.focus = FocusState::Unknown;
        endpoint.active_target = None;
        Ok(())
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
    fn attachment_is_available_only_after_binding() {
        let mut registry = ClientRegistry::default();
        let (id, token) = registry.register("d".into(), "phone".into(), "termux".into(), vec![], 0);
        assert_eq!(registry.attachment(&id).unwrap(), None);
        registry.bind(&token, "/dev/pts/7".into(), 1).unwrap();
        assert_eq!(registry.attachment(&id).unwrap(), Some("/dev/pts/7"));
    }
}
