use std::io::Write;
use std::os::unix::net::UnixListener;
use std::process::{Command, Stdio};

use tmux_agent_workbench::ipc::{Response, read_request, write_response};

fn run_hook(root: &std::path::Path, agent: &str, event: &str) {
    let mut child = Command::new(env!("CARGO_BIN_EXE_tmux-agent-workbench"))
        .args(["hook", "ingest", agent, event])
        .env_remove("TMUX")
        .env_remove("TMUX_PANE")
        .env_remove("TMUX_AGENT_WORKBENCH_TMUX_SOCKET")
        .env("XDG_RUNTIME_DIR", root)
        .env("XDG_CONFIG_HOME", root.join("config"))
        .env("XDG_STATE_HOME", root.join("state"))
        .env("XDG_CACHE_HOME", root.join("cache"))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(br#"{"session_id":"outside-tmux-test","cwd":"/tmp","thread_name":"test"}"#)
        .unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "{agent}/{event}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(output.stderr.is_empty());
    assert_eq!(output.stdout, b"{}\n");
}

#[test]
fn global_hooks_succeed_without_tmux_or_daemon_for_every_agent() {
    let temp = tempfile::tempdir_in("/tmp").unwrap();
    for agent in ["codex", "claude", "trae", "opencode"] {
        for event in [
            "SessionStart",
            "UserPromptSubmit",
            "PostToolUse",
            "PermissionRequest",
            "Stop",
        ] {
            run_hook(temp.path(), agent, event);
        }
    }
}

#[test]
fn detached_codex_hook_tolerates_unmatched_pane_and_still_delivers_when_matched() {
    for matched in [false, true] {
        let temp = tempfile::tempdir_in("/tmp").unwrap();
        let runtime = temp.path().join("tmux-agent-workbench");
        std::fs::create_dir(&runtime).unwrap();
        let listener = UnixListener::bind(runtime.join("daemon-test.sock")).unwrap();
        let server = std::thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            let request = read_request(&stream).unwrap();
            assert_eq!(request.method, "agent.event.ingest");
            assert_eq!(request.params["session_id"], "outside-tmux-test");
            let response = if matched {
                Response::success(request.id, serde_json::json!({}))
            } else {
                Response::error(request.id, "not_found", "no matching live pane")
            };
            write_response(&stream, &response).unwrap();
        });
        run_hook(temp.path(), "codex", "Stop");
        server.join().unwrap();
    }
}
