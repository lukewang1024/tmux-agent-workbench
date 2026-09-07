//! One lifecycle and routing pipeline for all outgoing notification transports.
use std::collections::HashSet;

use crate::client::ClientRegistry;
use crate::config::Config;
use crate::model::AgentSnapshot;
use crate::notification::{
    Notification, NotificationBackend, NotificationScheduler, deliver_local,
};
use crate::relay::RelaySender;
use crate::semantic::{RouteDecision, SemanticRouter};

#[derive(Default)]
pub struct NotificationPipeline {
    pub(crate) scheduler: NotificationScheduler,
    pub(crate) router: SemanticRouter,
}

pub struct DeliveryTargets<'a, B> {
    pub clients: &'a mut ClientRegistry,
    pub relay: &'a mut RelaySender,
    pub local: &'a mut B,
}

impl NotificationPipeline {
    pub fn new(router: SemanticRouter) -> Self {
        Self {
            scheduler: NotificationScheduler::default(),
            router,
        }
    }

    /// Also called immediately before a client dequeues its pending events.
    pub fn refresh(
        &mut self,
        now_ms: u64,
        agents: &[AgentSnapshot],
        clients: &mut ClientRegistry,
        relay: &mut RelaySender,
    ) -> Vec<Notification> {
        self.scheduler.observe(now_ms, agents);
        let ready = self.scheduler.ready(now_ms);
        let valid: HashSet<_> = ready
            .iter()
            .filter(|n| !n.agent.attention.as_ref().is_some_and(|a| a.seen))
            .map(|n| n.event.id.clone())
            .collect();
        let relay_valid = ready
            .iter()
            .filter(|n| n.desktop_allowed())
            .map(|n| n.event.id.clone())
            .collect();
        relay.retain_events(&relay_valid);
        let client_valid = valid
            .into_iter()
            .filter(|id| !self.router.is_accepted(id))
            .filter(|id| {
                ready.iter().find(|n| &n.event.id == id).is_some_and(|n| {
                    !n.event.category.queued()
                        || clients
                            .focused_viewer(&n.event.target.pane_id, now_ms)
                            .is_none()
                })
            })
            .collect();
        clients.retain_events(&client_valid);
        ready
    }

    /// Returns attention IDs watched by a focused client for the state machine
    /// to acknowledge. Output adapters never create or classify attention.
    pub fn dispatch<B: NotificationBackend>(
        &mut self,
        now_ms: u64,
        agents: &[AgentSnapshot],
        config: &Config,
        targets: DeliveryTargets<'_, B>,
    ) -> Vec<String> {
        let mut seen = Vec::new();
        for notification in self.refresh(now_ms, agents, targets.clients, targets.relay) {
            let event = &notification.event;
            match self.router.route(event, targets.clients, now_ms) {
                RouteDecision::Watched {
                    mark_seen,
                    sound_endpoint,
                } => {
                    if !mark_seen {
                        if let Some(endpoint) = sound_endpoint {
                            let _ = targets.clients.queue(&endpoint, event.clone());
                            continue;
                        }
                    }
                    if mark_seen {
                        seen.push(event.id.clone());
                    }
                    self.router.accepted(&event.id, "watched");
                }
                RouteDecision::Deliver { endpoints, .. } => {
                    if let Some(endpoint) = endpoints.first() {
                        if !notification
                            .agent
                            .attention
                            .as_ref()
                            .is_some_and(|a| a.seen)
                        {
                            let _ = targets.clients.queue(endpoint, event.clone());
                        } else {
                            self.router.accepted(&event.id, "seen");
                        }
                    } else {
                        let desktop = notification.desktop_allowed();
                        // Relay is an optional fallback transport alongside local
                        // output, independent of whether desktop rendering succeeds.
                        if desktop {
                            targets.relay.enqueue(&notification, now_ms);
                        }
                        match deliver_local(&notification, desktop, config, targets.local) {
                            Ok(()) => self.router.accepted(&event.id, "local"),
                            Err(error) => eprintln!(
                                "tmux-agent-workbench: notification delivery failed: {error}"
                            ),
                        }
                    }
                }
                _ => {}
            }
        }
        seen
    }
}
