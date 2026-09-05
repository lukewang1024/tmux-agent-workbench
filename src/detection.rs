use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::config::Config;
use crate::manifest::ManifestSet;
use crate::model::{
    AgentEventReport, AgentKind, AgentSnapshot, BaseState, ConversationRole,
    DetachedAgentEventReport, DisplayState, SessionSnapshot, session_rollup,
};
use crate::process::{AgentProcess, ProcessSource, ProcessTree};
use crate::server::ServerIdentity;
use crate::state_machine::{Observation, StateMachine};
use crate::tmux::{Pane, Tmux, TmuxError, TmuxSource};

pub struct Detector {
    tmux: Tmux,
    processes: ProcessTree,
    machine: StateMachine,
    panes: HashMap<String, Pane>,
    agents: HashMap<u32, AgentProcess>,
    pane_instances: HashMap<String, String>,
    last_classification: HashMap<String, CachedClassification>,
    last_capture_revision: HashMap<String, String>,
    next_capture: HashMap<String, Instant>,
    next_process_scan: Instant,
    metadata: HashMap<String, MetadataRecord>,
}

#[derive(Debug, Clone)]
struct CachedClassification {
    state: BaseState,
    reason_category: Option<String>,
    rule_id: Option<String>,
    evidence: Option<Vec<u8>>,
    strong_visible_signal: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MetadataReport {
    pub pane_id: String,
    pub kind: Option<AgentKind>,
    pub label: Option<String>,
    pub session_id: Option<String>,
    pub reason_hint: Option<String>,
    pub conversation_id: Option<String>,
    pub conversation_role: Option<ConversationRole>,
    pub conversation_label: Option<String>,
    pub conversation_state: Option<DisplayState>,
    #[serde(default)]
    pub conversation_active: bool,
    pub ttl_ms: u64,
}

#[derive(Debug, Clone)]
struct MetadataRecord {
    report: MetadataReport,
    expires_at_ms: u64,
}

impl Detector {
    pub fn new(server: ServerIdentity) -> Self {
        Self {
            tmux: Tmux::new(server),
            processes: ProcessTree::default(),
            machine: StateMachine::default(),
            panes: HashMap::new(),
            agents: HashMap::new(),
            pane_instances: HashMap::new(),
            last_classification: HashMap::new(),
            last_capture_revision: HashMap::new(),
            next_capture: HashMap::new(),
            next_process_scan: Instant::now(),
            metadata: HashMap::new(),
        }
    }

    pub fn tick(
        &mut self,
        config: &Config,
        manifests: &ManifestSet,
        now_ms: u64,
    ) -> Result<Vec<AgentSnapshot>, TmuxError> {
        let now = Instant::now();
        if now >= self.next_process_scan {
            if self.scan_processes(config, manifests, now_ms).is_err() {
                let instances: Vec<_> = self.pane_instances.values().cloned().collect();
                for instance in instances {
                    self.machine.mark_capture_failure(
                        &instance,
                        now_ms,
                        config.detection.stale_grace_ms,
                    );
                }
                self.next_process_scan =
                    now + Duration::from_millis(config.detection.process_interval_ms);
                self.machine.prune_tombstones(now_ms);
                return Ok(self.machine.snapshots());
            }
            self.next_process_scan =
                now + Duration::from_millis(config.detection.process_interval_ms);
        }

        let panes: Vec<_> = self.panes.values().cloned().collect();
        for pane in panes {
            let Some(process) = self.agents.get(&pane.root_pid).cloned() else {
                continue;
            };
            let due = self
                .next_capture
                .get(&pane.target.pane_id)
                .is_none_or(|due| now >= *due);
            let unchanged_idle = self
                .last_classification
                .get(&pane.target.pane_id)
                .is_some_and(|cached| cached.state == BaseState::Idle)
                && self
                    .last_capture_revision
                    .get(&pane.target.pane_id)
                    .is_some_and(|revision| revision == &pane.content_revision);
            if due && !unchanged_idle {
                self.capture(&pane, process, config, manifests, now, now_ms);
            } else if let Some(instance) = self.pane_instances.get(&pane.target.pane_id) {
                self.machine.set_visibility(instance, pane.visible);
            }
        }
        self.machine.prune_tombstones(now_ms);
        self.metadata.retain(|_, item| item.expires_at_ms > now_ms);
        Ok(self.decorated_snapshots())
    }

    pub fn acknowledge(&mut self, event_id: &str) -> bool {
        self.machine.acknowledge(event_id)
    }

    pub fn wake(&mut self) {
        self.next_process_scan = Instant::now();
        for due in self.next_capture.values_mut() {
            *due = Instant::now();
        }
    }

    pub fn machine_snapshots(&self) -> Vec<AgentSnapshot> {
        self.decorated_snapshots()
    }

    pub fn restore_checkpoints(
        &mut self,
        checkpoints: &[crate::checkpoint::RuntimeCheckpoint],
        restored_at_ms: u64,
    ) -> Vec<crate::checkpoint::RuntimeCheckpoint> {
        let mut unmatched = Vec::new();
        for checkpoint in checkpoints {
            if !self.machine.restore_checkpoint(checkpoint, restored_at_ms) {
                unmatched.push(checkpoint.clone());
            }
        }
        unmatched
    }

    pub fn checkpoint_metadata(
        &self,
        instance_id: &str,
    ) -> Option<(String, u64, u64, Option<String>)> {
        self.machine.checkpoint_metadata(instance_id)
    }

    pub fn sessions(&self) -> Vec<SessionSnapshot> {
        let agents = self.decorated_snapshots();
        let mut by_session: HashMap<String, Vec<&Pane>> = HashMap::new();
        for pane in self.panes.values() {
            by_session
                .entry(pane.target.session_id.clone())
                .or_default()
                .push(pane);
        }
        let mut sessions: Vec<_> = by_session
            .into_iter()
            .map(|(session_id, panes)| {
                let session_agents: Vec<_> = agents
                    .iter()
                    .filter(|agent| agent.target.session_id == session_id)
                    .collect();
                // tmux retains one active window/pane per session even while
                // that session is detached. Visibility only describes current
                // clients and loses the restore target for inactive sessions.
                let active = panes
                    .iter()
                    .copied()
                    .find(|pane| pane.window_active && pane.pane_active)
                    // When the Workbench sidebar owns focus it has already
                    // been filtered from this inventory. tmux marks the main
                    // pane that preceded it as pane_last; retain that target
                    // so the first Session click cannot land on the sidebar or
                    // on a stale window.
                    .or_else(|| {
                        panes
                            .iter()
                            .copied()
                            .find(|pane| pane.window_active && pane.pane_last)
                    });
                let agent_pane = session_agents.iter().find_map(|agent| {
                    panes
                        .iter()
                        .copied()
                        .find(|pane| pane.target.pane_id == agent.target.pane_id)
                });
                SessionSnapshot {
                    session_id,
                    session_name: panes[0].target.session_name.clone(),
                    rollup_state: session_rollup(
                        session_agents.iter().map(|agent| &agent.display_state),
                    ),
                    agent_count: session_agents.iter().filter(|agent| !agent.exited).count(),
                    attention_count: session_agents
                        .iter()
                        .filter(|agent| agent.attention.as_ref().is_some_and(|event| !event.seen))
                        .count(),
                    current_path: agent_pane
                        .or(active)
                        .map(|pane| pane.current_path.clone())
                        .or_else(|| panes.first().map(|pane| pane.current_path.clone())),
                    active: panes.iter().any(|pane| pane.session_visible),
                    last_active_window_id: active.map(|pane| pane.target.window_id.clone()),
                    last_active_pane_id: active.map(|pane| pane.target.pane_id.clone()),
                }
            })
            .collect();
        sessions.sort_by_key(|session| tmux_numeric_id(&session.session_id));
        sessions
    }

    pub fn next_attention(&self) -> Option<AgentSnapshot> {
        self.machine.next_attention_agent()
    }

    pub fn explain(
        &self,
        pane_id: &str,
        show_content: bool,
        config: &Config,
    ) -> Result<Value, String> {
        if !valid_pane_id(pane_id) {
            return Err("invalid pane id".into());
        }
        let agent = self
            .machine
            .snapshots()
            .into_iter()
            .find(|agent| agent.target.pane_id == pane_id)
            .ok_or_else(|| "agent pane not found".to_owned())?;
        let content = if show_content && !agent.exited {
            Some(
                self.tmux
                    .capture_bottom(
                        pane_id,
                        config.detection.capture_lines,
                        config.detection.capture_bytes,
                    )
                    .map_err(|error| error.to_string())?,
            )
        } else {
            None
        };
        Ok(json!({"agent": agent, "content": content}))
    }

    pub fn report_metadata(&mut self, report: MetadataReport, now_ms: u64) -> Result<(), String> {
        validate_metadata(&report)?;
        let pane = report.pane_id.clone();
        self.metadata.insert(
            pane.clone(),
            MetadataRecord {
                expires_at_ms: now_ms.saturating_add(report.ttl_ms),
                report,
            },
        );
        self.next_process_scan = Instant::now();
        self.next_capture.insert(pane, Instant::now());
        Ok(())
    }

    pub fn report_agent_event(
        &mut self,
        report: &AgentEventReport,
    ) -> Result<AgentSnapshot, String> {
        if report.version != 1 || !valid_pane_id(&report.pane_id) {
            return Err("invalid agent event envelope".into());
        }
        if report.session_label.as_deref().is_some_and(|label| {
            label.is_empty() || label.len() > 128 || label.chars().any(char::is_control)
        }) {
            return Err("invalid agent session label".into());
        }
        let pane = self
            .panes
            .get(&report.pane_id)
            .ok_or("event pane is not live")?;
        if pane.target.session_id != report.tmux_session_id {
            return Err("event tmux session does not match pane".into());
        }
        let process = self
            .agents
            .get(&pane.root_pid)
            .ok_or("event pane has no live agent")?;
        if process.kind != report.agent {
            return Err("event agent kind does not match process".into());
        }
        if report.agent_pid != 0 && process.fingerprint.pid != report.agent_pid {
            return Err("event process identity does not match".into());
        }
        let instance = self
            .pane_instances
            .get(&report.pane_id)
            .ok_or("event pane has no tracked instance")?
            .clone();
        self.machine.report_event(&instance, report, pane.visible)
    }

    pub fn resolve_agent_event(
        &mut self,
        detached: &DetachedAgentEventReport,
    ) -> Result<(AgentEventReport, AgentSnapshot), String> {
        let snapshots = self.machine.snapshots();
        let bound: Vec<_> = snapshots
            .iter()
            .filter(|agent| {
                !agent.exited
                    && agent.kind == detached.agent
                    && agent.hook_session_id.as_deref() == Some(&detached.session_id)
            })
            .collect();
        let chosen = if bound.len() == 1 {
            bound[0]
        } else if bound.is_empty() {
            let mut candidates: Vec<_> = snapshots
                .iter()
                .filter(|agent| {
                    !agent.exited
                        && agent.kind == detached.agent
                        && agent.hook_session_id.is_none()
                        && detached.cwd.as_deref().is_none_or(|cwd| {
                            self.panes
                                .get(&agent.target.pane_id)
                                .is_some_and(|pane| pane.current_path == cwd)
                        })
                })
                .collect();
            candidates.sort_by_key(|agent| {
                agent
                    .process
                    .as_ref()
                    .map(|process| process.started_at_ticks)
                    .unwrap_or_default()
            });
            candidates
                .last()
                .copied()
                .ok_or("no matching live agent pane")?
        } else {
            return Err("agent thread is associated with multiple panes".into());
        };
        let pane = self
            .panes
            .get(&chosen.target.pane_id)
            .ok_or("event pane is not live")?;
        let report = AgentEventReport {
            version: detached.version,
            event_id: detached.event_id.clone(),
            agent: detached.agent,
            pane_id: chosen.target.pane_id.clone(),
            tmux_session_id: pane.target.session_id.clone(),
            session_id: detached.session_id.clone(),
            session_label: detached.session_label.clone(),
            agent_pid: 0,
            event: detached.event,
            occurred_at_unix_ms: detached.occurred_at_unix_ms,
            reason_category: detached.reason_category.clone(),
        };
        let snapshot = self.report_agent_event(&report)?;
        Ok((report, snapshot))
    }

    fn decorated_snapshots(&self) -> Vec<AgentSnapshot> {
        // Match Herdr's pane model: /btw and /side are alternate foreground
        // views of one Agent, not independently rendered child runs. Keep the
        // schema field for snapshot-v1 compatibility, but expose only the state
        // inferred from the currently visible pane content/title.
        self.machine.snapshots()
    }

    fn scan_processes(
        &mut self,
        config: &Config,
        manifests: &ManifestSet,
        now_ms: u64,
    ) -> Result<(), TmuxError> {
        let panes = self.tmux.panes()?;
        let roots: Vec<_> = panes.iter().map(|pane| pane.root_pid).collect();
        let agents = self
            .processes
            .agents_for_roots(&roots, &manifests.aliases());
        for pane in &panes {
            let Some(agent) = agents.get(&pane.root_pid) else {
                continue;
            };
            let replaced = self
                .pane_instances
                .get(&pane.target.pane_id)
                .and_then(|instance| {
                    self.machine
                        .snapshots()
                        .into_iter()
                        .find(|snapshot| &snapshot.instance_id == instance)
                })
                .and_then(|snapshot| snapshot.process)
                .is_some_and(|old| old != agent.fingerprint);
            if replaced {
                self.next_capture
                    .insert(pane.target.pane_id.clone(), Instant::now());
            }
        }
        let live_panes: HashSet<_> = panes
            .iter()
            .filter(|pane| agents.contains_key(&pane.root_pid))
            .map(|pane| pane.target.pane_id.clone())
            .collect();

        let exited: Vec<_> = self
            .pane_instances
            .iter()
            .filter(|(pane, _)| !live_panes.contains(*pane))
            .map(|(pane, instance)| (pane.clone(), instance.clone()))
            .collect();
        for (pane, instance) in exited {
            self.machine.process_exited(&instance, now_ms);
            self.pane_instances.remove(&pane);
            self.last_classification.remove(&pane);
            self.last_capture_revision.remove(&pane);
            self.next_capture.remove(&pane);
        }

        self.panes = panes
            .into_iter()
            .map(|pane| (pane.target.pane_id.clone(), pane))
            .collect();
        self.agents = agents;
        self.next_process_scan =
            Instant::now() + Duration::from_millis(config.detection.process_interval_ms);
        Ok(())
    }

    fn capture(
        &mut self,
        pane: &Pane,
        process: AgentProcess,
        config: &Config,
        manifests: &ManifestSet,
        now: Instant,
        now_ms: u64,
    ) {
        let pane_id = pane.target.pane_id.clone();
        let title_result = manifests.get(process.kind).classify("", &pane.title);
        // A busy TUI can make capture-pane hit its short timeout. Pane titles are
        // already present in list-panes, so publish positive title signals before
        // attempting a capture. In particular, Codex keeps its spinner in the
        // title while a turn is active; ignoring it here made working detection
        // depend on the slowest and least reliable observation path.
        if matches!(title_result.state, BaseState::Working | BaseState::Blocked)
            && !title_needs_capture(process.kind, title_result.state)
            && !self.machine.has_pending_codex_permission(&pane_id)
            && !title_result.skip_state_update
        {
            let cached = CachedClassification {
                state: title_result.state,
                reason_category: title_result.reason_category,
                rule_id: title_result.rule_id,
                evidence: title_result.evidence,
                strong_visible_signal: title_result.strong_visible_signal,
            };
            self.publish_observation(pane, process, cached.clone(), now_ms);
            self.last_classification.insert(pane_id.clone(), cached);
            self.last_capture_revision
                .insert(pane_id.clone(), pane.content_revision.clone());
            self.next_capture.insert(
                pane_id,
                now + Duration::from_millis(config.detection.active_capture_interval_ms),
            );
            return;
        }
        match self.tmux.capture_bottom(
            &pane_id,
            config.detection.capture_lines.min(40),
            config.detection.capture_bytes,
        ) {
            Ok(content) => {
                let result = manifests.get(process.kind).classify(&content, &pane.title);
                let cached = if result.skip_state_update {
                    self.last_classification.get(&pane_id).cloned().unwrap_or(
                        CachedClassification {
                            state: BaseState::Unknown,
                            reason_category: None,
                            rule_id: result.rule_id,
                            evidence: result.evidence,
                            strong_visible_signal: result.strong_visible_signal,
                        },
                    )
                } else {
                    CachedClassification {
                        state: result.state,
                        reason_category: result.reason_category,
                        rule_id: result.rule_id,
                        evidence: result.evidence,
                        strong_visible_signal: result.strong_visible_signal,
                    }
                };
                let snapshot = self.publish_observation(pane, process, cached.clone(), now_ms);
                self.last_classification.insert(pane_id.clone(), cached);
                self.last_capture_revision
                    .insert(pane_id.clone(), pane.content_revision.clone());
                let interval = if snapshot.base_state == BaseState::Working
                    && result.state == BaseState::Idle
                {
                    100
                } else if snapshot.base_state == BaseState::Idle {
                    config.detection.idle_capture_interval_ms
                } else {
                    config.detection.active_capture_interval_ms
                };
                self.next_capture
                    .insert(pane_id, now + Duration::from_millis(interval));
            }
            Err(_) => {
                if let Some(instance) = self.pane_instances.get(&pane_id) {
                    self.machine.mark_capture_failure(
                        instance,
                        now_ms,
                        config.detection.stale_grace_ms,
                    );
                }
                self.next_capture.insert(
                    pane_id,
                    now + Duration::from_millis(config.detection.active_capture_interval_ms),
                );
            }
        }
    }

    #[allow(dead_code)]
    fn observe_without_capture(
        &mut self,
        pane: &Pane,
        process: AgentProcess,
        config: &Config,
        manifests: &ManifestSet,
        now: Instant,
        now_ms: u64,
    ) {
        let pane_id = pane.target.pane_id.clone();
        let title_result = manifests.get(process.kind).classify("", &pane.title);
        let cached = if matches!(title_result.state, BaseState::Working | BaseState::Blocked)
            && !title_needs_capture(process.kind, title_result.state)
            && !self.machine.has_pending_codex_permission(&pane_id)
            && !title_result.skip_state_update
        {
            CachedClassification {
                state: title_result.state,
                reason_category: title_result.reason_category,
                rule_id: title_result.rule_id,
                evidence: title_result.evidence,
                strong_visible_signal: title_result.strong_visible_signal,
            }
        } else {
            self.last_classification
                .get(&pane_id)
                .cloned()
                .unwrap_or(CachedClassification {
                    state: BaseState::Unknown,
                    reason_category: None,
                    rule_id: None,
                    evidence: None,
                    strong_visible_signal: false,
                })
        };
        self.publish_observation(pane, process, cached.clone(), now_ms);
        self.last_classification.insert(pane_id.clone(), cached);
        self.last_capture_revision
            .insert(pane_id.clone(), pane.content_revision.clone());
        self.next_capture.insert(
            pane_id,
            now + Duration::from_millis(config.detection.active_capture_interval_ms),
        );
    }

    fn publish_observation(
        &mut self,
        pane: &Pane,
        process: AgentProcess,
        cached: CachedClassification,
        now_ms: u64,
    ) -> AgentSnapshot {
        let metadata = self
            .metadata
            .get(&pane.target.pane_id)
            .filter(|item| item.expires_at_ms > now_ms)
            .map(|item| &item.report);
        let kind = metadata.and_then(|item| item.kind).unwrap_or(process.kind);
        let label = metadata
            .and_then(|item| item.label.as_ref())
            .filter(|label| !label.is_empty())
            .cloned()
            .unwrap_or_else(|| {
                if meaningful_window_label(&pane.target.window_name) {
                    pane.target.window_name.clone()
                } else {
                    kind_label(kind).to_owned()
                }
            });
        let reason_category = cached.reason_category.or_else(|| {
            (cached.state == BaseState::Blocked)
                .then(|| metadata.and_then(|item| item.reason_hint.clone()))
                .flatten()
        });
        let snapshot = self.machine.observe_estimate(Observation {
            kind,
            target: pane.target.clone(),
            process: process.fingerprint,
            label,
            state: cached.state,
            reason_category,
            rule_id: cached.rule_id,
            evidence: cached.evidence,
            strong_visible_signal: cached.strong_visible_signal,
            visible: pane.visible,
            manifest_version: 1,
            hook_session_id: metadata.and_then(|item| item.session_id.clone()),
            observed_at_ms: now_ms,
        });
        if let Some(previous) = self
            .pane_instances
            .insert(pane.target.pane_id.clone(), snapshot.instance_id.clone())
        {
            if previous != snapshot.instance_id {
                self.machine.process_exited(&previous, now_ms);
            }
        }
        snapshot
    }
}

fn valid_pane_id(value: &str) -> bool {
    value
        .strip_prefix('%')
        .is_some_and(|rest| !rest.is_empty() && rest.bytes().all(|byte| byte.is_ascii_digit()))
}

fn validate_metadata(report: &MetadataReport) -> Result<(), String> {
    if !valid_pane_id(&report.pane_id) {
        return Err("pane_id must match %<digits>".into());
    }
    if report.ttl_ms == 0 || report.ttl_ms > 300_000 {
        return Err("ttl_ms must be in 1..=300000".into());
    }
    if report.conversation_id.is_some()
        != (report.conversation_role.is_some() && report.conversation_state.is_some())
    {
        return Err(
            "conversation_id, conversation_role and conversation_state must be supplied together"
                .into(),
        );
    }
    for (name, value, limit) in [
        ("label", report.label.as_deref(), 128),
        ("session_id", report.session_id.as_deref(), 256),
        ("conversation_id", report.conversation_id.as_deref(), 128),
        (
            "conversation_label",
            report.conversation_label.as_deref(),
            128,
        ),
    ] {
        if value.is_some_and(|value| value.len() > limit || value.chars().any(char::is_control)) {
            return Err(format!("{name} is invalid"));
        }
    }
    if report.reason_hint.as_deref().is_some_and(|value| {
        value.is_empty()
            || value.len() > 64
            || !value
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
    }) {
        return Err("reason_hint must be a category identifier".into());
    }
    Ok(())
}

fn kind_label(kind: AgentKind) -> &'static str {
    match kind {
        AgentKind::Codex => "Codex",
        AgentKind::Claude => "Claude",
        AgentKind::Trae => "Trae",
        AgentKind::Opencode => "OpenCode",
    }
}

// Codex uses Action Required for both human prompts and automatic review.
// Consult the live overlay before publishing blocked attention.
fn title_needs_capture(kind: AgentKind, state: BaseState) -> bool {
    kind == AgentKind::Codex && state == BaseState::Blocked
}

fn meaningful_window_label(label: &str) -> bool {
    !label.trim().is_empty()
        && !matches!(
            label.trim().to_ascii_lowercase().as_str(),
            "agent" | "shell" | "terminal" | "zsh" | "bash" | "fish" | "sh"
        )
}

fn tmux_numeric_id(value: &str) -> u64 {
    value
        .get(1..)
        .and_then(|digits| digits.parse().ok())
        .unwrap_or(u64::MAX)
}

#[allow(dead_code)]
fn _display_interval(state: DisplayState, config: &Config) -> u64 {
    match state {
        DisplayState::Idle => config.detection.idle_capture_interval_ms,
        _ => config.detection.active_capture_interval_ms,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codex_action_required_needs_overlay_capture() {
        assert!(title_needs_capture(AgentKind::Codex, BaseState::Blocked));
        assert!(!title_needs_capture(AgentKind::Codex, BaseState::Working));
        assert!(!title_needs_capture(AgentKind::Claude, BaseState::Blocked));
    }

    #[test]
    fn unmatched_checkpoint_remains_pending_until_process_discovery() {
        let server = ServerIdentity::from_socket("/tmp/workbench-checkpoint-test".into()).unwrap();
        let mut detector = Detector::new(server);
        let checkpoint = crate::checkpoint::RuntimeCheckpoint {
            version: 1,
            server_incarnation: "server:1".into(),
            runtime_id: "runtime".into(),
            process_fingerprint: "42:100:/bin/codex".into(),
            previous_state: "working".into(),
            attention_seq: 0,
            seen_seq: 0,
            hook_session_id: Some("thread".into()),
            delivered_event_ids: Vec::new(),
            pending: Vec::new(),
            recent_endpoint: None,
        };

        assert_eq!(
            detector.restore_checkpoints(std::slice::from_ref(&checkpoint), 500),
            vec![checkpoint]
        );
    }

    #[test]
    fn metadata_requires_bounded_ttl_and_safe_fields() {
        let base = MetadataReport {
            pane_id: "%1".into(),
            kind: Some(AgentKind::Codex),
            label: Some("build".into()),
            session_id: Some("session-1".into()),
            reason_hint: Some("approval".into()),
            conversation_id: None,
            conversation_role: None,
            conversation_label: None,
            conversation_state: None,
            conversation_active: false,
            ttl_ms: 5_000,
        };
        assert!(validate_metadata(&base).is_ok());
        let mut invalid = base.clone();
        invalid.pane_id = "%1;run-shell".into();
        assert!(validate_metadata(&invalid).is_err());
        let mut invalid = base;
        invalid.ttl_ms = 300_001;
        assert!(validate_metadata(&invalid).is_err());

        let mut prompt_text = MetadataReport {
            pane_id: "%1".into(),
            kind: None,
            label: None,
            session_id: None,
            reason_hint: Some("please approve this command".into()),
            conversation_id: None,
            conversation_role: None,
            conversation_label: None,
            conversation_state: None,
            conversation_active: false,
            ttl_ms: 1_000,
        };
        assert!(validate_metadata(&prompt_text).is_err());
        prompt_text.reason_hint = Some("approval".into());
        assert!(validate_metadata(&prompt_text).is_ok());
    }

    #[test]
    fn generic_window_names_do_not_hide_agent_kind() {
        assert!(!meaningful_window_label("agent"));
        assert!(!meaningful_window_label("shell"));
        assert!(meaningful_window_label("release-fix"));
    }

    #[test]
    fn conversation_metadata_requires_complete_identity_and_state() {
        let mut report = MetadataReport {
            pane_id: "%1".into(),
            kind: Some(AgentKind::Codex),
            label: None,
            session_id: None,
            reason_hint: None,
            conversation_id: Some("thread-main".into()),
            conversation_role: None,
            conversation_label: None,
            conversation_state: Some(DisplayState::Working),
            conversation_active: true,
            ttl_ms: 5_000,
        };
        assert!(validate_metadata(&report).is_err());
        report.conversation_role = Some(ConversationRole::Main);
        assert!(validate_metadata(&report).is_ok());
    }
}
