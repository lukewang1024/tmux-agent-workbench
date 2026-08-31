use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BaseState {
    Working,
    Blocked,
    Idle,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DisplayState {
    Working,
    Blocked,
    Done,
    Idle,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StateSource {
    Hook,
    Process,
    Screen,
    None,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StateConfidence {
    High,
    Low,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HookHealth {
    Healthy,
    Missing,
    Stale,
    Conflict,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentKind {
    Codex,
    Claude,
    Trae,
    Opencode,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentEventType {
    SessionStart,
    Working,
    Activity,
    Permission,
    Stop,
    Error,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentEventReport {
    pub version: u32,
    pub event_id: String,
    pub agent: AgentKind,
    pub pane_id: String,
    pub tmux_session_id: String,
    pub session_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_label: Option<String>,
    pub agent_pid: u32,
    pub event: AgentEventType,
    pub occurred_at_unix_ms: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason_category: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TmuxTarget {
    pub session_id: String,
    pub session_name: String,
    pub window_id: String,
    pub window_index: u32,
    pub window_name: String,
    pub pane_id: String,
    pub pane_index: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProcessFingerprint {
    pub pid: u32,
    pub started_at_ticks: u64,
    pub executable: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AttentionKind {
    Blocked,
    Done,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AttentionEvent {
    pub id: String,
    pub kind: AttentionKind,
    pub seen: bool,
    pub since_unix_ms: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attention_seq: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub seen_seq: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentSnapshot {
    pub instance_id: String,
    pub kind: AgentKind,
    pub label: String,
    pub target: TmuxTarget,
    pub process: Option<ProcessFingerprint>,
    pub base_state: BaseState,
    pub display_state: DisplayState,
    #[serde(default = "default_state_source")]
    pub state_source: StateSource,
    #[serde(default = "default_confidence")]
    pub confidence: StateConfidence,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub estimated_state: Option<BaseState>,
    #[serde(default = "default_hook_health")]
    pub hook_health: HookHealth,
    pub reason_category: Option<String>,
    pub attention: Option<AttentionEvent>,
    pub stale: bool,
    pub visible: bool,
    pub manifest_version: u32,
    pub rule_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hook_session_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub relay_focus: Option<RelayFocus>,
    pub exited: bool,
    pub exited_at_unix_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub conversations: Vec<ConversationSnapshot>,
}

fn default_state_source() -> StateSource {
    StateSource::Screen
}
fn default_confidence() -> StateConfidence {
    StateConfidence::Low
}
fn default_hook_health() -> HookHealth {
    HookHealth::Missing
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConversationRole {
    Main,
    Side,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConversationSnapshot {
    pub id: String,
    pub role: ConversationRole,
    pub label: String,
    pub base_state: BaseState,
    pub display_state: DisplayState,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason_category: Option<String>,
    #[serde(default, skip_serializing_if = "is_false")]
    pub active: bool,
    pub stale: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RelayFocus {
    pub remote_id: String,
    pub tmux_socket: String,
    pub session_id: String,
    pub pane_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Snapshot {
    pub schema_version: u32,
    pub server: String,
    pub generation: u64,
    pub observed_at_unix_ms: u64,
    pub sessions: Vec<SessionSnapshot>,
    pub agents: Vec<AgentSnapshot>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub clients: Vec<ClientSnapshot>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClientSnapshot {
    pub device_label: String,
    pub kind: String,
    pub capabilities: Vec<String>,
    pub presence: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attachment: Option<String>,
    pub focus: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active_target: Option<TmuxTarget>,
    pub activity_unix_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionSnapshot {
    pub session_id: String,
    pub session_name: String,
    pub rollup_state: DisplayState,
    pub agent_count: usize,
    pub attention_count: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub current_path: Option<String>,
    #[serde(default, skip_serializing_if = "is_false")]
    pub active: bool,
    pub last_active_window_id: Option<String>,
    pub last_active_pane_id: Option<String>,
}

fn is_false(value: &bool) -> bool {
    !*value
}

impl Snapshot {
    pub fn empty(server: impl Into<String>, observed_at_unix_ms: u64) -> Self {
        Self {
            schema_version: crate::SNAPSHOT_SCHEMA_VERSION,
            server: server.into(),
            generation: 0,
            observed_at_unix_ms,
            sessions: Vec::new(),
            agents: Vec::new(),
            clients: Vec::new(),
        }
    }
}

pub fn session_rollup<'a>(states: impl IntoIterator<Item = &'a DisplayState>) -> DisplayState {
    let mut best = DisplayState::Unknown;
    for state in states {
        if rank(*state) < rank(best) {
            best = *state;
        }
    }
    best
}

fn rank(state: DisplayState) -> u8 {
    match state {
        DisplayState::Blocked => 0,
        DisplayState::Done => 1,
        DisplayState::Working => 2,
        DisplayState::Idle => 3,
        DisplayState::Unknown => 4,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rollup_uses_attention_priority() {
        let states = [
            DisplayState::Idle,
            DisplayState::Working,
            DisplayState::Done,
        ];
        assert_eq!(session_rollup(states.iter()), DisplayState::Done);
    }

    #[test]
    fn snapshot_v1_matches_golden_and_accepts_additive_fields() {
        let golden = include_str!("../tests/golden/snapshot-v1.json");
        let snapshot: Snapshot = serde_json::from_str(golden).unwrap();
        assert_eq!(snapshot.schema_version, 1);
        assert_eq!(
            serde_json::to_string_pretty(&snapshot).unwrap() + "\n",
            golden
        );

        let mut value: serde_json::Value = serde_json::from_str(golden).unwrap();
        value["future_top_level"] = serde_json::json!(true);
        value["agents"][0]["future_agent_field"] = serde_json::json!("allowed");
        assert!(serde_json::from_value::<Snapshot>(value).is_ok());
    }
}
