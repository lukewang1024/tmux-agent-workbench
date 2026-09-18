use super::*;
use serde_json::json;
#[test]
fn identical_payloads() {
    let p=json!({"session_id":"front","thread_name":"fixture"});
    let a=detached_report_from_payload(AgentKind::Codex,"Stop",&p).unwrap();
    let b=detached_report_from_payload(AgentKind::Codex,"Stop",&p).unwrap();
    let passed=a.event_id!=b.event_id;
    println!("HOOK_CASE_RESULT {}",json!({"case":"identical_payloads","passed":passed,"observed":{"distinct_event_ids":passed}}));
    assert!(passed);
}
#[test]
fn delayed_ack() {
    use std::os::unix::net::UnixListener;
    let temp=tempfile::tempdir().unwrap();
    let paths=Paths {config_dir:temp.path().into(),state_dir:temp.path().into(),cache_dir:temp.path().into(),runtime_dir:temp.path().into()};
    let listener=UnixListener::bind(paths.socket_for_server("fixture")).unwrap();
    let receiver=std::thread::spawn(move || {
        let (stream,_)=listener.accept().unwrap();
        let r=crate::ipc::read_request(&stream).unwrap();
        std::thread::sleep(Duration::from_millis(1100));
        let _=crate::ipc::write_response(&stream,&crate::ipc::Response::success(r.id,json!({"accepted":true})));
    });
    let result=ingest_detached(&paths,AgentKind::Codex,"Stop",
        br#"{"session_id":"front","thread_name":"fixture","event_id":"stop"}"#);
    receiver.join().unwrap();
    let dir=paths.spool_for_server("fixture").join("detached");
    let queued=std::fs::read_dir(dir).ok().into_iter().flatten().filter_map(Result::ok).any(|e| {
        std::fs::read(e.path()).ok().and_then(|b|serde_json::from_slice::<serde_json::Value>(&b).ok())
            .is_some_and(|v|v["event_id"]=="stop" && v["session_id"]=="front")
    });
    let passed=result.is_ok() && queued;
    println!("HOOK_CASE_RESULT {}",json!({"case":"delayed_ack","passed":passed,"observed":{"ingest_ok":result.is_ok(),"original_event_queued":queued,"ack_delay_ms":1100}}));
    assert!(passed);
}
