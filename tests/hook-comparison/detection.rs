use super::*;
use serde_json::json;
    fn fixture(pids: &[u32]) -> Detector {
        let mut detector =
            Detector::new(ServerIdentity::from_socket("/tmp/routing-test".into()).unwrap());
        let golden: crate::model::Snapshot =
            serde_json::from_str(include_str!("../tests/golden/snapshot-v1.json")).unwrap();
        for pid in pids.iter().copied() {
            let mut target = golden.agents[0].target.clone();
            target.pane_id = format!("%{pid}");
            let process = crate::model::ProcessFingerprint {
                pid,
                started_at_ticks: pid as u64,
                executable: "codex".into(),
            };
            let observation = Observation {
                kind: AgentKind::Codex,
                target: target.clone(),
                process: process.clone(),
                label: "test".into(),
                state: BaseState::Idle,
                reason_category: None,
                rule_id: None,
                evidence: None,
                strong_visible_signal: true,
                visible: false,
                manifest_version: 1,
                hook_session_id: None,
                observed_at_ms: 0,
            };
            let agent = detector.machine.observe_estimate(observation);
            detector
                .pane_instances
                .insert(target.pane_id.clone(), agent.instance_id);
            detector.agents.insert(
                pid,
                AgentProcess {
                    kind: AgentKind::Codex,
                    fingerprint: process,
                },
            );
            detector.panes.insert(
                target.pane_id.clone(),
                Pane {
                    target,
                    root_pid: pid,
                    title: "test".into(),
                    current_command: "codex".into(),
                    current_path: "/tmp".into(),
                    role: None,
                    visible: false,
                    window_active: false,
                    pane_active: false,
                    pane_last: false,
                    session_visible: false,
                    content_revision: "1".into(),
                },
            );
        }
        detector
    }


fn report(cwd: &str) -> DetachedAgentEventReport {
    serde_json::from_value(json!({"version":1,"event_id":"start","agent":"codex",
        "session_id":"front","event":"working","occurred_at_unix_ms":100,"cwd":cwd})).unwrap()
}
#[test]
fn ambiguous_cwd() {
    let mut d=fixture(&[42,43]);
    let result=d.resolve_agent_event(&report("/tmp"));
    let pane=result.as_ref().ok().map(|(_,a)|a.target.pane_id.clone());
    let passed=result.is_err() && d.machine_snapshots().iter().all(|a|a.hook_session_id.is_none());
    println!("HOOK_CASE_RESULT {}",json!({"case":"ambiguous_cwd","passed":passed,"observed":{"rejected":result.is_err(),"assigned_pane":pane}}));
    assert!(passed);
}
#[test]
fn cwd_alias() {
    let temp=tempfile::tempdir().unwrap();
    let real=temp.path().join("real");let alias=temp.path().join("alias");
    std::fs::create_dir(&real).unwrap();std::os::unix::fs::symlink(&real,&alias).unwrap();
    let mut d=fixture(&[42]);d.panes.get_mut("%42").unwrap().current_path=real.to_string_lossy().into();
    let result=d.resolve_agent_event(&report(alias.to_str().unwrap()));
    let pane=result.as_ref().ok().map(|(_,a)|a.target.pane_id.clone());
    let passed=pane.as_deref()==Some("%42");
    println!("HOOK_CASE_RESULT {}",json!({"case":"cwd_alias","passed":passed,"observed":{"assigned_pane":pane}}));
    assert!(passed);
}
