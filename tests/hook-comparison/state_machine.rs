use super::*;
use serde_json::{json, Value};

fn check(case: &str, observed: Value, passed: bool) {
    println!("HOOK_CASE_RESULT {}", json!({"case":case,"observed":observed,"passed":passed}));
    assert!(passed, "{case}: {observed}");
}
fn observation(state: BaseState, at: u64) -> Observation {
    Observation { kind: AgentKind::Codex, target: TmuxTarget {
        session_id:"$1".into(), session_name:"test".into(), window_id:"@1".into(),
        window_index:0, window_name:"agent".into(), pane_id:"%1".into(), pane_index:0 },
        process: ProcessFingerprint {pid:100, started_at_ticks:9, executable:"codex".into()},
        label:"test".into(), state, reason_category:None, rule_id:None, evidence:None,
        strong_visible_signal:true, visible:false, manifest_version:1, hook_session_id:None,
        observed_at_ms:at }
}
fn checkpoint(state: &str, session: Option<&str>) -> crate::checkpoint::RuntimeCheckpoint {
    // The exact same legacy wire input is accepted by both implementations.
    serde_json::from_value(json!({"version":1,"server_incarnation":"test", "runtime_id":"runtime",
        "process_fingerprint":"100:9:codex","previous_state":state,"attention_seq":0,"seen_seq":0,
        "hook_session_id":session,"delivered_event_ids":[],"pending":[]})).unwrap()
}
fn event(id: &str, session: &str, event: AgentEventType, at: u64) -> AgentEventReport {
    AgentEventReport {version:1,event_id:id.into(),agent:AgentKind::Codex,pane_id:"%1".into(),
        tmux_session_id:"$1".into(),session_id:session.into(),session_label:None,agent_pid:100,
        event,occurred_at_unix_ms:at,reason_category:None}
}
#[test]
fn screen_checkpoint() {
    let mut m=StateMachine::default();
    m.observe_estimate(observation(BaseState::Working,0));
    m.restore_checkpoint(&checkpoint("idle",None),100);
    let a=&m.snapshots()[0];
    check("screen_checkpoint",json!({"state":a.base_state,"source":a.state_source,"health":a.hook_health}),
        a.base_state==BaseState::Working && a.state_source==StateSource::Screen && a.attention.is_none());
}
#[test]
fn legacy_unknown() {
    let mut m=StateMachine::default();
    m.observe_estimate(observation(BaseState::Idle,0));
    m.restore_checkpoint(&checkpoint("unknown",Some("front")),100);
    let a=m.observe_estimate(observation(BaseState::Idle,200));
    check("legacy_unknown",json!({"state":a.base_state,"source":a.state_source,"health":a.hook_health,"thread":a.hook_session_id}),
        a.base_state==BaseState::Idle && a.state_source==StateSource::Screen && a.hook_session_id.as_deref()==Some("front"));
}
#[test]
fn late_checkpoint() {
    let mut m=StateMachine::default();
    let a=m.observe_estimate(observation(BaseState::Idle,0));
    m.report_event(&a.instance_id,&event("new","front",AgentEventType::Working,200),false).unwrap();
    m.restore_checkpoint(&checkpoint("idle",Some("front")),300);
    let a=&m.snapshots()[0];
    check("late_checkpoint",json!({"state":a.base_state}),a.base_state==BaseState::Working);
}
#[test]
fn explicit_duplicate_control() {
    let mut m=StateMachine::default();
    let a=m.observe_estimate(observation(BaseState::Idle,0));
    let e=event("same","front",AgentEventType::Permission,100);
    let first=m.report_event(&a.instance_id,&e,false).unwrap();
    let second=m.report_event(&a.instance_id,&e,false).unwrap();
    let same=first.attention.as_ref().map(|a| &a.id)==second.attention.as_ref().map(|a| &a.id);
    check("explicit_duplicate_control",json!({"same_attention_id":same}),same && second.base_state==BaseState::Blocked);
}
#[test]
fn lifecycle_control() {
    let mut m=StateMachine::default();
    let a=m.observe_estimate(observation(BaseState::Idle,0));
    let mut states=Vec::new();
    for (i,kind) in [AgentEventType::Working,AgentEventType::Permission,AgentEventType::Stop].into_iter().enumerate() {
        states.push(m.report_event(&a.instance_id,&event(&i.to_string(),"front",kind,100+i as u64),false).unwrap().base_state);
    }
    check("lifecycle_control",json!({"states":states}),states==[BaseState::Working,BaseState::Blocked,BaseState::Idle]);
}
#[test]
fn thread_fence_control() {
    let mut m=StateMachine::default();
    let a=m.observe_estimate(observation(BaseState::Idle,0));
    m.report_event(&a.instance_id,&event("start","front",AgentEventType::Working,100),false).unwrap();
    let rejected=m.report_event(&a.instance_id,&event("child","background",AgentEventType::Stop,200),false).is_err();
    check("thread_fence_control",json!({"rejected":rejected,"state":m.snapshots()[0].base_state}),
        rejected && m.snapshots()[0].base_state==BaseState::Working);
}
