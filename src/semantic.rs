use std::collections::{HashMap, HashSet};

use serde::{Deserialize, Serialize};

use crate::client::ClientRegistry;
use crate::model::TmuxTarget;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SemanticCategory {
    TaskComplete,
    InputRequired,
    TaskError,
    SessionStart,
}

impl SemanticCategory {
    pub fn name(self) -> &'static str {
        match self {
            Self::TaskComplete => "task.complete",
            Self::InputRequired => "input.required",
            Self::TaskError => "task.error",
            Self::SessionStart => "session.start",
        }
    }
    pub fn horizon_ms(self) -> u64 {
        match self {
            Self::TaskComplete | Self::InputRequired => 5 * 60_000,
            Self::TaskError => 60_000,
            Self::SessionStart => 0,
        }
    }
    pub fn queued(self) -> bool {
        matches!(self, Self::TaskComplete | Self::InputRequired)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SemanticEvent {
    pub id: String,
    pub category: SemanticCategory,
    pub target: TmuxTarget,
    pub created_unix_ms: u64,
    pub deadline_unix_ms: u64,
    pub title: String,
    pub body: String,
}

impl SemanticEvent {
    pub fn new(
        runtime_id: &str,
        sequence: u64,
        category: SemanticCategory,
        target: TmuxTarget,
        now_ms: u64,
        title: String,
        body: String,
    ) -> Self {
        Self {
            id: format!("{runtime_id}.{sequence}"),
            category,
            target,
            created_unix_ms: now_ms,
            deadline_unix_ms: now_ms.saturating_add(category.horizon_ms()),
            title,
            body,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RouteDecision {
    Silent,
    Watched {
        sound_endpoint: Option<String>,
        mark_seen: bool,
    },
    Deliver {
        endpoints: Vec<String>,
        desktop: bool,
        sound: bool,
    },
    Expired,
}

#[derive(Debug, Default)]
pub struct SemanticRouter {
    accepted: HashMap<String, HashSet<String>>,
    accepted_events: HashSet<String>,
}

impl SemanticRouter {
    pub fn route(
        &self,
        event: &SemanticEvent,
        clients: &ClientRegistry,
        now_ms: u64,
    ) -> RouteDecision {
        if self.accepted_events.contains(&event.id) {
            return RouteDecision::Silent;
        }
        if event.category == SemanticCategory::SessionStart {
            return RouteDecision::Silent;
        }
        if now_ms > event.deadline_unix_ms {
            return RouteDecision::Expired;
        }
        if let Some(viewer) = clients.focused_viewer(&event.target.pane_id, now_ms) {
            let sound_endpoint = viewer
                .capabilities
                .contains("sound")
                .then(|| viewer.id.clone());
            return RouteDecision::Watched {
                sound_endpoint,
                mark_seen: event.category.queued(),
            };
        }
        let endpoints = clients
            .ranked("notification", now_ms)
            .into_iter()
            .filter(|endpoint| {
                !self
                    .accepted
                    .get(&event.id)
                    .is_some_and(|ids| ids.contains(&endpoint.id))
            })
            .map(|endpoint| endpoint.id.clone())
            .collect();
        RouteDecision::Deliver {
            endpoints,
            desktop: true,
            sound: true,
        }
    }

    pub fn accepted(&mut self, event_id: &str, endpoint_id: &str) {
        self.accepted
            .entry(event_id.into())
            .or_default()
            .insert(endpoint_id.into());
        self.accepted_events.insert(event_id.into());
    }

    pub fn accepted_event_ids(&self) -> Vec<String> {
        self.accepted_events.iter().cloned().collect()
    }

    pub fn restore_accepted(&mut self, event_ids: impl IntoIterator<Item = String>) {
        self.accepted_events.extend(event_ids);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::client::{ClientRegistry, FocusState};
    fn target() -> TmuxTarget {
        TmuxTarget {
            session_id: "$1".into(),
            session_name: "s".into(),
            window_id: "@1".into(),
            window_index: 1,
            window_name: "w".into(),
            pane_id: "%1".into(),
            pane_index: 1,
        }
    }

    #[test]
    fn watched_suppresses_desktop_but_preserves_sound() {
        let mut clients = ClientRegistry::default();
        let (id, _) = clients.register(
            "d".into(),
            "mac".into(),
            "macos".into(),
            vec!["notification".into(), "sound".into()],
            1,
        );
        clients
            .update_focus(&id, FocusState::Focused, false, Some(target()), 2)
            .unwrap();
        let event = SemanticEvent::new(
            "runtime",
            1,
            SemanticCategory::TaskComplete,
            target(),
            2,
            "t".into(),
            "b".into(),
        );
        assert_eq!(
            SemanticRouter::default().route(&event, &clients, 3),
            RouteDecision::Watched {
                sound_endpoint: Some(id),
                mark_seen: true
            }
        );
    }

    #[test]
    fn ranks_recent_endpoint_and_does_not_treat_transport_ack_as_seen() {
        let mut clients = ClientRegistry::default();
        let (old, _) = clients.register(
            "a".into(),
            "old".into(),
            "linux".into(),
            vec!["notification".into()],
            1,
        );
        let (new, _) = clients.register(
            "b".into(),
            "new".into(),
            "termux".into(),
            vec!["notification".into()],
            2,
        );
        let event = SemanticEvent::new(
            "r",
            2,
            SemanticCategory::InputRequired,
            target(),
            2,
            "t".into(),
            "b".into(),
        );
        let mut router = SemanticRouter::default();
        assert_eq!(
            router.route(&event, &clients, 3),
            RouteDecision::Deliver {
                endpoints: vec![new.clone(), old],
                desktop: true,
                sound: true
            }
        );
        router.accepted(&event.id, &new);
        assert_eq!(router.route(&event, &clients, 3), RouteDecision::Silent);
    }

    #[test]
    fn deadlines_do_not_reset() {
        let clients = ClientRegistry::default();
        let event = SemanticEvent::new(
            "r",
            1,
            SemanticCategory::TaskError,
            target(),
            10,
            "t".into(),
            "b".into(),
        );
        assert_eq!(
            SemanticRouter::default().route(&event, &clients, 60_011),
            RouteDecision::Expired
        );
    }
}
