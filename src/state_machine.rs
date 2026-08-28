use std::collections::HashMap;

use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::model::{
    AgentEventReport, AgentEventType, AgentKind, AgentSnapshot, AttentionEvent, AttentionKind,
    BaseState, DisplayState, HookHealth, ProcessFingerprint, StateConfidence, StateSource,
    TmuxTarget,
};

const IDLE_CONFIRMATIONS: u8 = 3;
const IDLE_MAX_DELAY_MS: u64 = 700;
const WORKING_CONFIRMATIONS: u8 = 3;
const WORKING_MAX_DELAY_MS: u64 = 700;
const TOMBSTONE_TTL_MS: u64 = 24 * 60 * 60 * 1_000;

#[derive(Debug, Clone)]
pub struct Observation {
    pub kind: AgentKind,
    pub target: TmuxTarget,
    pub process: ProcessFingerprint,
    pub label: String,
    pub state: BaseState,
    pub reason_category: Option<String>,
    pub rule_id: Option<String>,
    pub evidence: Option<Vec<u8>>,
    pub strong_visible_signal: bool,
    pub visible: bool,
    pub manifest_version: u32,
    pub hook_session_id: Option<String>,
    pub observed_at_ms: u64,
}

#[derive(Debug, Clone)]
struct Tracked {
    snapshot: AgentSnapshot,
    idle_candidate_since: Option<u64>,
    idle_confirmations: u8,
    working_candidate_since: Option<u64>,
    working_confirmations: u8,
    stale_since: Option<u64>,
    reason_fingerprint: Option<[u8; 32]>,
    last_hook_at_ms: Option<u64>,
    last_hook_event_at_ms: u64,
    active_hook_session_id: Option<String>,
    conflict_since_ms: Option<u64>,
    seen_event_ids: Vec<String>,
}

#[derive(Debug, Default)]
pub struct StateMachine {
    agents: HashMap<String, Tracked>,
}

impl StateMachine {
    pub fn observe(&mut self, observation: Observation) -> AgentSnapshot {
        let instance_id = instance_id(&observation.target.pane_id, &observation.process);
        let new_reason = reason_fingerprint(
            observation.reason_category.as_deref(),
            observation.rule_id.as_deref(),
            observation.evidence.as_deref(),
        );

        let tracked = self.agents.entry(instance_id.clone()).or_insert_with(|| {
            let attention = (observation.state == BaseState::Blocked).then(|| {
                attention(
                    AttentionKind::Blocked,
                    observation.visible,
                    observation.observed_at_ms,
                )
            });
            Tracked {
                snapshot: AgentSnapshot {
                    instance_id: instance_id.clone(),
                    kind: observation.kind,
                    label: observation.label.clone(),
                    target: observation.target.clone(),
                    process: Some(observation.process.clone()),
                    base_state: observation.state,
                    display_state: display_for(observation.state),
                    state_source: StateSource::Screen,
                    confidence: StateConfidence::Low,
                    estimated_state: Some(observation.state),
                    hook_health: HookHealth::Missing,
                    reason_category: observation.reason_category.clone(),
                    attention,
                    stale: false,
                    visible: observation.visible,
                    manifest_version: observation.manifest_version,
                    rule_id: observation.rule_id.clone(),
                    hook_session_id: observation.hook_session_id.clone(),
                    relay_focus: None,
                    exited: false,
                    exited_at_unix_ms: None,
                    conversations: Vec::new(),
                },
                idle_candidate_since: None,
                idle_confirmations: 0,
                working_candidate_since: None,
                working_confirmations: 0,
                stale_since: None,
                reason_fingerprint: new_reason,
                last_hook_at_ms: None,
                last_hook_event_at_ms: 0,
                active_hook_session_id: None,
                conflict_since_ms: None,
                seen_event_ids: Vec::new(),
            }
        });

        let old_state = tracked.snapshot.base_state;
        let reason_changed = tracked.reason_fingerprint != new_reason;
        let mut publish_state = observation.state;

        if matches!(old_state, BaseState::Idle | BaseState::Unknown)
            && observation.state == BaseState::Working
            && !observation.strong_visible_signal
        {
            let since = tracked
                .working_candidate_since
                .get_or_insert(observation.observed_at_ms);
            tracked.working_confirmations = tracked.working_confirmations.saturating_add(1);
            if tracked.working_confirmations < WORKING_CONFIRMATIONS
                && observation.observed_at_ms.saturating_sub(*since) < WORKING_MAX_DELAY_MS
            {
                publish_state = old_state;
            }
        } else {
            tracked.working_candidate_since = None;
            tracked.working_confirmations = 0;
        }

        if old_state == BaseState::Working
            && observation.state == BaseState::Idle
            && !observation.strong_visible_signal
        {
            let since = tracked
                .idle_candidate_since
                .get_or_insert(observation.observed_at_ms);
            tracked.idle_confirmations = tracked.idle_confirmations.saturating_add(1);
            if tracked.idle_confirmations < IDLE_CONFIRMATIONS
                && observation.observed_at_ms.saturating_sub(*since) < IDLE_MAX_DELAY_MS
            {
                publish_state = BaseState::Working;
            }
        } else {
            tracked.idle_candidate_since = None;
            tracked.idle_confirmations = 0;
        }

        let stable_done = old_state == BaseState::Working && publish_state == BaseState::Idle;
        let new_blocked = publish_state == BaseState::Blocked
            && (old_state != BaseState::Blocked || reason_changed);

        tracked.snapshot.kind = observation.kind;
        tracked.snapshot.label = observation.label;
        tracked.snapshot.target = observation.target;
        tracked.snapshot.process = Some(observation.process);
        tracked.snapshot.visible = observation.visible;
        tracked.snapshot.stale = false;
        tracked.stale_since = None;
        tracked.snapshot.manifest_version = observation.manifest_version;
        tracked.snapshot.hook_session_id = observation.hook_session_id;

        if publish_state != BaseState::Working || observation.state != BaseState::Idle {
            tracked.snapshot.base_state = publish_state;
            tracked.snapshot.display_state = display_for(publish_state);
            tracked.snapshot.reason_category = observation.reason_category;
            tracked.snapshot.rule_id = observation.rule_id;
            tracked.reason_fingerprint = new_reason;
        }

        let stale_attention = tracked.snapshot.attention.as_ref().is_some_and(|event| {
            matches!(
                (event.kind, publish_state),
                (AttentionKind::Blocked, state) if state != BaseState::Blocked
            ) || matches!(
                (event.kind, publish_state),
                (AttentionKind::Done, state) if state != BaseState::Idle
            )
        });
        if stale_attention {
            tracked.snapshot.attention = None;
        }

        if stable_done {
            if observation.visible {
                tracked.snapshot.display_state = DisplayState::Idle;
                tracked.snapshot.attention = None;
            } else {
                tracked.snapshot.display_state = DisplayState::Done;
                tracked.snapshot.attention = Some(attention(
                    AttentionKind::Done,
                    false,
                    observation.observed_at_ms,
                ));
            }
            tracked.idle_candidate_since = None;
            tracked.idle_confirmations = 0;
        } else if new_blocked {
            tracked.snapshot.attention = Some(attention(
                AttentionKind::Blocked,
                observation.visible,
                observation.observed_at_ms,
            ));
        } else if observation.visible {
            if let Some(event) = &mut tracked.snapshot.attention {
                event.seen = true;
            }
        }

        refresh_attention_display(&mut tracked.snapshot);

        tracked.snapshot.clone()
    }

    /// Screen/title evidence is deliberately non-authoritative. It can render
    /// a dim estimate, but can never create blocked/done attention.
    pub fn observe_estimate(&mut self, observation: Observation) -> AgentSnapshot {
        let instance_id = instance_id(&observation.target.pane_id, &observation.process);
        let has_hook = self
            .agents
            .get(&instance_id)
            .is_some_and(|tracked| tracked.last_hook_at_ms.is_some());
        if !has_hook {
            let snapshot = self.observe(observation.clone());
            let tracked = self
                .agents
                .get_mut(&snapshot.instance_id)
                .expect("just observed");
            tracked.snapshot.state_source = StateSource::Screen;
            tracked.snapshot.confidence = StateConfidence::Low;
            tracked.snapshot.estimated_state = Some(observation.state);
            tracked.snapshot.hook_health = HookHealth::Missing;
            tracked.snapshot.attention = None;
            tracked.snapshot.display_state = display_for(tracked.snapshot.base_state);
            return tracked.snapshot.clone();
        }

        let tracked = self.agents.get_mut(&instance_id).expect("hook tracked");
        tracked.snapshot.target = observation.target;
        tracked.snapshot.process = Some(observation.process);
        tracked.snapshot.visible = observation.visible;
        tracked.snapshot.estimated_state = Some(observation.state);
        if observation.state != tracked.snapshot.base_state {
            let since = *tracked
                .conflict_since_ms
                .get_or_insert(observation.observed_at_ms);
            if observation.observed_at_ms.saturating_sub(since) >= 3_000 {
                tracked.snapshot.hook_health = HookHealth::Conflict;
                // Screen may only revoke an obsolete attention item after the
                // same continuous conflict window. It never manufactures a
                // replacement or changes canonical lifecycle.
                if tracked.snapshot.attention.as_ref().is_some_and(|event| {
                    event.kind == AttentionKind::Blocked && observation.state != BaseState::Blocked
                }) {
                    tracked.snapshot.attention = None;
                }
            }
        } else {
            tracked.conflict_since_ms = None;
            tracked.snapshot.hook_health = HookHealth::Healthy;
        }
        tracked.snapshot.clone()
    }

    pub fn report_event(
        &mut self,
        instance_id: &str,
        report: &AgentEventReport,
        visible: bool,
    ) -> Result<AgentSnapshot, String> {
        let tracked = self
            .agents
            .get_mut(instance_id)
            .ok_or("agent instance not found")?;
        if tracked
            .seen_event_ids
            .iter()
            .any(|id| id == &report.event_id)
        {
            return Ok(tracked.snapshot.clone());
        }
        if report.occurred_at_unix_ms < tracked.last_hook_event_at_ms {
            return Err("out-of-order event".into());
        }
        let activates = matches!(
            report.event,
            AgentEventType::SessionStart | AgentEventType::Working
        );
        if activates {
            tracked.active_hook_session_id = Some(report.session_id.clone());
        } else if tracked.active_hook_session_id.as_deref() != Some(&report.session_id) {
            return Err("event is not for the active foreground session".into());
        }
        tracked.seen_event_ids.push(report.event_id.clone());
        if tracked.seen_event_ids.len() > 128 {
            tracked.seen_event_ids.drain(..64);
        }
        tracked.last_hook_at_ms = Some(report.occurred_at_unix_ms);
        tracked.last_hook_event_at_ms = report.occurred_at_unix_ms;
        tracked.snapshot.hook_session_id = Some(report.session_id.clone());
        if let Some(label) = &report.session_label {
            tracked.snapshot.label = label.clone();
        }
        tracked.snapshot.state_source = StateSource::Hook;
        tracked.snapshot.confidence = StateConfidence::High;
        tracked.snapshot.hook_health = HookHealth::Healthy;
        tracked.snapshot.visible = visible;
        tracked.snapshot.exited = false;
        tracked.snapshot.exited_at_unix_ms = None;

        let next = match report.event {
            AgentEventType::SessionStart | AgentEventType::Working | AgentEventType::Activity => {
                BaseState::Working
            }
            AgentEventType::Permission => BaseState::Blocked,
            AgentEventType::Stop => BaseState::Idle,
            AgentEventType::Error => BaseState::Working,
        };
        let old = tracked.snapshot.base_state;
        tracked.snapshot.base_state = next;
        tracked.snapshot.display_state = display_for(next);
        tracked.snapshot.reason_category = report.reason_category.clone();
        tracked.snapshot.rule_id = None;
        if next != BaseState::Blocked
            && tracked
                .snapshot
                .attention
                .as_ref()
                .is_some_and(|e| e.kind == AttentionKind::Blocked)
        {
            tracked.snapshot.attention = None;
        }
        match report.event {
            AgentEventType::Permission => {
                tracked.snapshot.attention = Some(attention(
                    AttentionKind::Blocked,
                    visible,
                    report.occurred_at_unix_ms,
                ));
            }
            AgentEventType::Stop if old != BaseState::Idle => {
                if visible {
                    tracked.snapshot.attention = None;
                } else {
                    tracked.snapshot.display_state = DisplayState::Done;
                    tracked.snapshot.attention = Some(attention(
                        AttentionKind::Done,
                        false,
                        report.occurred_at_unix_ms,
                    ));
                }
            }
            AgentEventType::Error => {}
            _ => {}
        }
        refresh_attention_display(&mut tracked.snapshot);
        Ok(tracked.snapshot.clone())
    }

    pub fn mark_capture_failure(
        &mut self,
        instance_id: &str,
        now_ms: u64,
        stale_grace_ms: u64,
    ) -> Option<AgentSnapshot> {
        let tracked = self.agents.get_mut(instance_id)?;
        let stale_since = *tracked.stale_since.get_or_insert(now_ms);
        tracked.snapshot.stale = true;
        if now_ms.saturating_sub(stale_since) >= stale_grace_ms {
            tracked.snapshot.base_state = BaseState::Unknown;
            tracked.snapshot.display_state = DisplayState::Unknown;
            tracked.snapshot.reason_category = None;
            tracked.snapshot.rule_id = None;
            tracked.snapshot.attention = None;
        }
        Some(tracked.snapshot.clone())
    }

    pub fn process_exited(&mut self, instance_id: &str, now_ms: u64) -> Option<AgentSnapshot> {
        let pane_id = self
            .agents
            .get(instance_id)?
            .snapshot
            .target
            .pane_id
            .clone();
        // A TUI may replace its foreground process more than once while keeping
        // the same pane. The user completed one pane, not one task per PID, so
        // retain only the newest unread completion for that pane.
        self.agents.retain(|id, tracked| {
            id == instance_id
                || !tracked.snapshot.exited
                || tracked.snapshot.target.pane_id != pane_id
        });
        let tracked = self.agents.get_mut(instance_id)?;
        tracked.snapshot.exited = true;
        tracked.snapshot.exited_at_unix_ms = Some(now_ms);
        tracked.snapshot.process = None;
        tracked.snapshot.base_state = BaseState::Idle;
        tracked.snapshot.display_state = DisplayState::Done;
        tracked.snapshot.state_source = StateSource::Process;
        tracked.snapshot.confidence = StateConfidence::High;
        tracked.snapshot.attention = Some(attention(
            AttentionKind::Done,
            tracked.snapshot.visible,
            now_ms,
        ));
        Some(tracked.snapshot.clone())
    }

    pub fn acknowledge(&mut self, event_id: &str) -> bool {
        for tracked in self.agents.values_mut() {
            if let Some(event) = &mut tracked.snapshot.attention {
                if event.id == event_id {
                    event.seen = true;
                    refresh_attention_display(&mut tracked.snapshot);
                    return true;
                }
            }
        }
        false
    }

    pub fn set_visibility(&mut self, instance_id: &str, visible: bool) -> Option<AgentSnapshot> {
        let tracked = self.agents.get_mut(instance_id)?;
        tracked.snapshot.visible = visible;
        if visible {
            if let Some(event) = &mut tracked.snapshot.attention {
                event.seen = true;
            }
        }
        refresh_attention_display(&mut tracked.snapshot);
        Some(tracked.snapshot.clone())
    }

    pub fn prune_tombstones(&mut self, now_ms: u64) {
        self.agents.retain(|_, tracked| {
            if !tracked.snapshot.exited {
                return true;
            }
            let seen = tracked
                .snapshot
                .attention
                .as_ref()
                .is_none_or(|event| event.seen);
            let expired = tracked
                .snapshot
                .exited_at_unix_ms
                .is_some_and(|exited| now_ms.saturating_sub(exited) >= TOMBSTONE_TTL_MS);
            !seen && !expired
        });
    }

    pub fn snapshots(&self) -> Vec<AgentSnapshot> {
        let mut values: Vec<_> = self
            .agents
            .values()
            .map(|item| item.snapshot.clone())
            .collect();
        values.sort_by(|a, b| stable_order_key(a).cmp(&stable_order_key(b)));
        values
    }

    pub fn next_attention(&self) -> Option<&AttentionEvent> {
        self.agents
            .values()
            .filter_map(|tracked| tracked.snapshot.attention.as_ref())
            .filter(|event| !event.seen)
            .min_by_key(|event| {
                let priority = match event.kind {
                    AttentionKind::Blocked => 0,
                    AttentionKind::Done => 1,
                };
                (priority, event.since_unix_ms)
            })
    }

    pub fn next_attention_agent(&self) -> Option<AgentSnapshot> {
        self.agents
            .values()
            .filter(|tracked| {
                tracked
                    .snapshot
                    .attention
                    .as_ref()
                    .is_some_and(|event| !event.seen)
            })
            .min_by_key(|tracked| {
                let event = tracked.snapshot.attention.as_ref().expect("filtered");
                let priority = match event.kind {
                    AttentionKind::Blocked => 0,
                    AttentionKind::Done => 1,
                };
                (priority, event.since_unix_ms)
            })
            .map(|tracked| tracked.snapshot.clone())
    }
}

fn attention(kind: AttentionKind, seen: bool, since_unix_ms: u64) -> AttentionEvent {
    AttentionEvent {
        id: Uuid::new_v4().to_string(),
        kind,
        seen,
        since_unix_ms,
    }
}

fn display_for(state: BaseState) -> DisplayState {
    match state {
        BaseState::Working => DisplayState::Working,
        BaseState::Blocked => DisplayState::Blocked,
        BaseState::Idle => DisplayState::Idle,
        BaseState::Unknown => DisplayState::Unknown,
    }
}

fn refresh_attention_display(snapshot: &mut AgentSnapshot) {
    let unseen_done = snapshot
        .attention
        .as_ref()
        .is_some_and(|event| event.kind == AttentionKind::Done && !event.seen);
    if snapshot.base_state == BaseState::Idle && unseen_done {
        snapshot.display_state = DisplayState::Done;
    } else if snapshot.display_state == DisplayState::Done {
        snapshot.display_state = display_for(snapshot.base_state);
    }
}

fn instance_id(pane_id: &str, process: &ProcessFingerprint) -> String {
    let mut hash = Sha256::new();
    hash.update(pane_id.as_bytes());
    hash.update(process.pid.to_le_bytes());
    hash.update(process.started_at_ticks.to_le_bytes());
    hash.update(process.executable.as_bytes());
    format!("agent-{:x}", hash.finalize())[..30].to_owned()
}

fn reason_fingerprint(
    category: Option<&str>,
    rule_id: Option<&str>,
    evidence: Option<&[u8]>,
) -> Option<[u8; 32]> {
    if category.is_none() && rule_id.is_none() && evidence.is_none() {
        return None;
    }
    let mut hash = Sha256::new();
    hash.update(category.unwrap_or_default().as_bytes());
    hash.update([0]);
    hash.update(rule_id.unwrap_or_default().as_bytes());
    hash.update([0]);
    hash.update(evidence.unwrap_or_default());
    Some(hash.finalize().into())
}

fn stable_order_key(agent: &AgentSnapshot) -> (u64, u32, u32) {
    (
        agent
            .target
            .session_id
            .get(1..)
            .and_then(|digits| digits.parse().ok())
            .unwrap_or(u64::MAX),
        agent.target.window_index,
        agent.target.pane_index,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn observation(state: BaseState, now: u64) -> Observation {
        Observation {
            kind: AgentKind::Codex,
            target: TmuxTarget {
                session_id: "$1".into(),
                session_name: "task".into(),
                window_id: "@1".into(),
                window_index: 0,
                window_name: "agent".into(),
                pane_id: "%1".into(),
                pane_index: 0,
            },
            process: ProcessFingerprint {
                pid: 100,
                started_at_ticks: 9,
                executable: "codex".into(),
            },
            label: "task".into(),
            state,
            reason_category: None,
            rule_id: None,
            evidence: None,
            strong_visible_signal: false,
            visible: false,
            manifest_version: 1,
            hook_session_id: None,
            observed_at_ms: now,
        }
    }

    #[test]
    fn initial_idle_does_not_create_done() {
        let mut machine = StateMachine::default();
        let result = machine.observe(observation(BaseState::Idle, 0));
        assert_eq!(result.display_state, DisplayState::Idle);
        assert!(result.attention.is_none());
    }

    #[test]
    fn working_to_idle_requires_three_confirmations() {
        let mut machine = StateMachine::default();
        machine.observe(observation(BaseState::Working, 0));
        assert_eq!(
            machine
                .observe(observation(BaseState::Idle, 100))
                .base_state,
            BaseState::Working
        );
        assert_eq!(
            machine
                .observe(observation(BaseState::Idle, 200))
                .base_state,
            BaseState::Working
        );
        let done = machine.observe(observation(BaseState::Idle, 300));
        assert_eq!(done.base_state, BaseState::Idle);
        assert_eq!(done.display_state, DisplayState::Done);
        assert_eq!(done.attention.unwrap().kind, AttentionKind::Done);
    }

    #[test]
    fn strong_visible_idle_bypasses_plain_idle_hold() {
        let mut machine = StateMachine::default();
        machine.observe(observation(BaseState::Working, 0));
        let mut idle = observation(BaseState::Idle, 100);
        idle.strong_visible_signal = true;
        let result = machine.observe(idle);
        assert_eq!(result.base_state, BaseState::Idle);
    }

    #[test]
    fn idle_to_working_requires_three_confirmations() {
        let mut machine = StateMachine::default();
        machine.observe(observation(BaseState::Idle, 0));
        assert_eq!(
            machine
                .observe(observation(BaseState::Working, 100))
                .base_state,
            BaseState::Idle
        );
        assert_eq!(
            machine
                .observe(observation(BaseState::Idle, 200))
                .base_state,
            BaseState::Idle
        );
        for now in [300, 400] {
            assert_eq!(
                machine
                    .observe(observation(BaseState::Working, now))
                    .base_state,
                BaseState::Idle
            );
        }
        assert_eq!(
            machine
                .observe(observation(BaseState::Working, 500))
                .base_state,
            BaseState::Working
        );
    }

    #[test]
    fn visible_completion_does_not_create_done_attention() {
        let mut machine = StateMachine::default();
        let mut working = observation(BaseState::Working, 0);
        working.visible = true;
        machine.observe(working);
        let mut result = machine.snapshots()[0].clone();
        for now in [100, 200, 300] {
            let mut idle = observation(BaseState::Idle, now);
            idle.visible = true;
            result = machine.observe(idle);
        }
        assert_eq!(result.display_state, DisplayState::Idle);
        assert!(result.attention.is_none());
    }

    #[test]
    fn unseen_done_persists_across_idle_samples_until_acknowledged() {
        let mut machine = StateMachine::default();
        machine.observe(observation(BaseState::Working, 0));
        for now in [100, 200, 300] {
            machine.observe(observation(BaseState::Idle, now));
        }
        let still_done = machine.observe(observation(BaseState::Idle, 2_300));
        assert_eq!(still_done.display_state, DisplayState::Done);
        let event_id = still_done.attention.unwrap().id;
        assert!(machine.acknowledge(&event_id));
        assert_eq!(machine.snapshots()[0].display_state, DisplayState::Idle);
    }

    #[test]
    fn attention_is_removed_when_its_underlying_state_is_no_longer_valid() {
        let mut machine = StateMachine::default();
        machine.observe(observation(BaseState::Blocked, 0));
        assert!(machine.next_attention().is_some());
        machine.observe(observation(BaseState::Working, 100));
        assert!(machine.next_attention().is_none());

        for now in [200, 300, 400] {
            machine.observe(observation(BaseState::Idle, now));
        }
        assert_eq!(machine.next_attention().unwrap().kind, AttentionKind::Done);
        for now in [500, 600, 700] {
            machine.observe(observation(BaseState::Working, now));
        }
        assert!(machine.next_attention().is_none());
    }

    #[test]
    fn initial_blocked_attention_is_seen_when_visible() {
        let mut machine = StateMachine::default();
        let mut input = observation(BaseState::Blocked, 10);
        input.visible = true;
        let result = machine.observe(input);
        assert!(result.attention.unwrap().seen);
        assert_eq!(result.base_state, BaseState::Blocked);
    }

    #[test]
    fn becoming_visible_acknowledges_blocked_without_changing_base_state() {
        let mut machine = StateMachine::default();
        let blocked = machine.observe(observation(BaseState::Blocked, 10));
        assert!(machine.next_attention().is_some());
        let visible = machine.set_visibility(&blocked.instance_id, true).unwrap();
        assert_eq!(visible.base_state, BaseState::Blocked);
        assert_eq!(visible.display_state, DisplayState::Blocked);
        assert!(visible.attention.unwrap().seen);
        assert!(machine.next_attention().is_none());
    }

    #[test]
    fn blocked_reason_change_creates_new_attention() {
        let mut machine = StateMachine::default();
        let mut first = observation(BaseState::Blocked, 10);
        first.reason_category = Some("approval".into());
        first.rule_id = Some("approval".into());
        first.evidence = Some(b"one".to_vec());
        let first_id = machine.observe(first).attention.unwrap().id;
        let mut second = observation(BaseState::Blocked, 20);
        second.reason_category = Some("approval".into());
        second.rule_id = Some("approval".into());
        second.evidence = Some(b"two".to_vec());
        let second_id = machine.observe(second).attention.unwrap().id;
        assert_ne!(first_id, second_id);
    }

    #[test]
    fn stale_grace_preserves_then_publishes_unknown() {
        let mut machine = StateMachine::default();
        let id = machine
            .observe(observation(BaseState::Working, 0))
            .instance_id;
        assert_eq!(
            machine
                .mark_capture_failure(&id, 10, 3_000)
                .unwrap()
                .base_state,
            BaseState::Working
        );
        assert_eq!(
            machine
                .mark_capture_failure(&id, 3_009, 3_000)
                .unwrap()
                .base_state,
            BaseState::Working
        );
        assert_eq!(
            machine
                .mark_capture_failure(&id, 3_010, 3_000)
                .unwrap()
                .base_state,
            BaseState::Unknown
        );
    }

    #[test]
    fn direct_exit_creates_and_prunes_tombstone() {
        let mut machine = StateMachine::default();
        let id = machine
            .observe(observation(BaseState::Working, 0))
            .instance_id;
        let tombstone = machine.process_exited(&id, 100).unwrap();
        assert!(tombstone.exited);
        assert_eq!(tombstone.display_state, DisplayState::Done);
        machine.prune_tombstones(100 + TOMBSTONE_TTL_MS);
        assert!(machine.snapshots().is_empty());
    }

    #[test]
    fn visible_exit_tombstone_is_removed_immediately() {
        let mut machine = StateMachine::default();
        let mut input = observation(BaseState::Working, 0);
        input.visible = true;
        let id = machine.observe(input).instance_id;
        machine.process_exited(&id, 100);
        machine.prune_tombstones(100);
        assert!(machine.snapshots().is_empty());
    }

    #[test]
    fn process_replacement_creates_distinct_instance_and_exit_tombstone() {
        let mut machine = StateMachine::default();
        let old = machine.observe(observation(BaseState::Working, 0));
        let mut replacement = observation(BaseState::Idle, 100);
        replacement.process.pid = 101;
        replacement.process.started_at_ticks = 10;
        let new = machine.observe(replacement);
        assert_ne!(old.instance_id, new.instance_id);
        machine.process_exited(&old.instance_id, 100).unwrap();
        let snapshots = machine.snapshots();
        assert_eq!(snapshots.len(), 2);
        assert!(snapshots.iter().any(|item| item.exited));
        assert!(snapshots.iter().any(|item| !item.exited));
    }

    #[test]
    fn repeated_process_replacement_keeps_one_completion_per_pane() {
        let mut machine = StateMachine::default();
        let old = machine.observe(observation(BaseState::Working, 0));
        machine.process_exited(&old.instance_id, 100).unwrap();

        let mut replacement = observation(BaseState::Working, 200);
        replacement.process.pid = 101;
        replacement.process.started_at_ticks = 10;
        let new = machine.observe(replacement);
        machine.process_exited(&new.instance_id, 300).unwrap();

        let snapshots = machine.snapshots();
        assert_eq!(snapshots.len(), 1);
        assert_eq!(snapshots[0].instance_id, new.instance_id);
        assert!(snapshots[0].exited);
    }

    #[test]
    fn next_attention_prefers_blocked_then_oldest() {
        let mut machine = StateMachine::default();
        let mut done_source = observation(BaseState::Working, 0);
        done_source.target.pane_id = "%2".into();
        done_source.process.pid = 102;
        machine.observe(done_source.clone());
        done_source.state = BaseState::Idle;
        done_source.observed_at_ms = 100;
        machine.observe(done_source.clone());
        done_source.observed_at_ms = 200;
        machine.observe(done_source.clone());
        done_source.observed_at_ms = 300;
        machine.observe(done_source);

        let mut blocked = observation(BaseState::Blocked, 400);
        blocked.target.pane_id = "%3".into();
        blocked.process.pid = 103;
        machine.observe(blocked);
        assert_eq!(machine.next_attention_agent().unwrap().target.pane_id, "%3");
    }

    #[test]
    fn next_attention_orders_same_kind_oldest_first() {
        let mut machine = StateMachine::default();
        let mut oldest = observation(BaseState::Blocked, 100);
        oldest.target.pane_id = "%8".into();
        oldest.process.pid = 108;
        machine.observe(oldest);
        let mut newer = observation(BaseState::Blocked, 200);
        newer.target.pane_id = "%9".into();
        newer.process.pid = 109;
        machine.observe(newer);
        assert_eq!(machine.next_attention_agent().unwrap().target.pane_id, "%8");
    }

    fn event(id: &str, session: &str, kind: AgentEventType, at: u64) -> AgentEventReport {
        AgentEventReport {
            version: 1,
            event_id: id.into(),
            agent: AgentKind::Codex,
            pane_id: "%1".into(),
            tmux_session_id: "$1".into(),
            session_id: session.into(),
            session_label: None,
            agent_pid: 100,
            event: kind,
            occurred_at_unix_ms: at,
            reason_category: match kind {
                AgentEventType::Permission => Some("approval".into()),
                AgentEventType::Error => Some("task_error".into()),
                _ => None,
            },
        }
    }

    #[test]
    fn screen_estimates_never_create_attention_or_done() {
        let mut machine = StateMachine::default();
        let working = machine.observe_estimate(observation(BaseState::Working, 0));
        let id = working.instance_id;
        for at in [100, 200, 800] {
            machine.observe_estimate(observation(BaseState::Idle, at));
        }
        let idle = machine
            .snapshots()
            .into_iter()
            .find(|s| s.instance_id == id)
            .unwrap();
        assert_eq!(idle.state_source, StateSource::Screen);
        assert_eq!(idle.confidence, StateConfidence::Low);
        assert_ne!(idle.display_state, DisplayState::Done);
        assert!(idle.attention.is_none());

        let blocked = machine.observe_estimate(observation(BaseState::Blocked, 900));
        assert_eq!(blocked.display_state, DisplayState::Blocked);
        assert!(blocked.attention.is_none());
    }

    #[test]
    fn native_events_are_authoritative_deduplicated_and_thread_fenced() {
        let mut machine = StateMachine::default();
        let initial = machine.observe_estimate(observation(BaseState::Idle, 0));
        machine
            .report_event(
                &initial.instance_id,
                &event("w", "front", AgentEventType::Working, 10),
                false,
            )
            .unwrap();
        let blocked = machine
            .report_event(
                &initial.instance_id,
                &event("p", "front", AgentEventType::Permission, 20),
                false,
            )
            .unwrap();
        assert_eq!(blocked.display_state, DisplayState::Blocked);
        let attention_id = blocked.attention.unwrap().id;
        let duplicate = machine
            .report_event(
                &initial.instance_id,
                &event("p", "front", AgentEventType::Permission, 20),
                false,
            )
            .unwrap();
        assert_eq!(duplicate.attention.unwrap().id, attention_id);

        machine
            .report_event(
                &initial.instance_id,
                &event("new", "front-2", AgentEventType::Working, 30),
                false,
            )
            .unwrap();
        assert!(
            machine
                .report_event(
                    &initial.instance_id,
                    &event("late", "front", AgentEventType::Stop, 40),
                    false
                )
                .is_err()
        );
        let current = machine
            .snapshots()
            .into_iter()
            .find(|s| s.instance_id == initial.instance_id)
            .unwrap();
        assert_eq!(current.base_state, BaseState::Working);
        assert_eq!(current.state_source, StateSource::Hook);
        assert!(current.attention.is_none());
    }

    #[test]
    fn screen_conflict_is_only_advisory_after_three_seconds() {
        let mut machine = StateMachine::default();
        let initial = machine.observe_estimate(observation(BaseState::Idle, 0));
        machine
            .report_event(
                &initial.instance_id,
                &event("w", "front", AgentEventType::Working, 10),
                false,
            )
            .unwrap();
        machine.observe_estimate(observation(BaseState::Idle, 100));
        let conflict = machine.observe_estimate(observation(BaseState::Idle, 3_101));
        assert_eq!(conflict.base_state, BaseState::Working);
        assert_eq!(conflict.estimated_state, Some(BaseState::Idle));
        assert_eq!(conflict.hook_health, HookHealth::Conflict);
        assert!(conflict.attention.is_none());
    }

    #[test]
    fn task_error_is_authoritative_activity_without_false_completion() {
        let mut machine = StateMachine::default();
        let initial = machine.observe_estimate(observation(BaseState::Working, 0));
        machine
            .report_event(
                &initial.instance_id,
                &event("w", "front", AgentEventType::Working, 10),
                false,
            )
            .unwrap();
        let failed = machine
            .report_event(
                &initial.instance_id,
                &event("error", "front", AgentEventType::Error, 20),
                false,
            )
            .unwrap();
        assert_eq!(failed.base_state, BaseState::Working);
        assert_eq!(failed.display_state, DisplayState::Working);
        assert_eq!(failed.reason_category.as_deref(), Some("task_error"));
        assert_eq!(failed.state_source, StateSource::Hook);
        assert!(failed.attention.is_none());
    }
}
