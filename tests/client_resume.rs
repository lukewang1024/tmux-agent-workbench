use std::process::Command;
use tmux_agent_workbench::{attachment, server::ServerIdentity};

struct Server {
    directory: tempfile::TempDir,
    identity: ServerIdentity,
}
impl Server {
    fn new() -> Self {
        let directory = tempfile::tempdir().unwrap();
        let identity = ServerIdentity::from_socket(directory.path().join("tmux.sock")).unwrap();
        let server = Self {
            directory,
            identity,
        };
        server.tmux(&[
            "-f",
            "/dev/null",
            "new-session",
            "-d",
            "-s",
            "alpha",
            "sleep 120",
        ]);
        server
    }
    fn tmux(&self, args: &[&str]) -> String {
        let output = Command::new("tmux")
            .args(self.identity.tmux_args())
            .args(args)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8_lossy(&output.stdout).trim().to_owned()
    }
    fn pane(&self, session: &str) -> String {
        self.tmux(&["display-message", "-p", "-t", session, "#{pane_id}"])
    }
}
impl Drop for Server {
    fn drop(&mut self) {
        let _ = Command::new("tmux")
            .args(self.identity.tmux_args())
            .arg("kill-server")
            .status();
        let _ = &self.directory;
    }
}

#[test]
fn terminal_histories_are_independent_and_survive_registry_replacement() {
    let server = Server::new();
    let a = attachment::pane_target(&server.identity, &server.pane("alpha")).unwrap();
    server.tmux(&["new-session", "-d", "-s", "beta", "sleep 120"]);
    let b = attachment::pane_target(&server.identity, &server.pane("beta")).unwrap();
    let key_a = attachment::terminal_key("phone", Some("tab-a")).unwrap();
    let key_b = attachment::terminal_key("phone", Some("tab-b")).unwrap();
    attachment::remember(&server.identity, &key_a, &a);
    attachment::remember(&server.identity, &key_b, &b);
    attachment::remember(&server.identity, attachment::LAST, &b);
    assert_eq!(
        attachment::recalled(&server.identity, &key_a)
            .unwrap()
            .pane_id,
        a.pane_id
    );
    assert_eq!(
        attachment::recalled(&server.identity, &key_b)
            .unwrap()
            .pane_id,
        b.pane_id
    );
    // No daemon or endpoint object participates in loading the server history.
    let rediscovered = ServerIdentity::from_socket(server.identity.socket_path.clone()).unwrap();
    assert_eq!(
        attachment::recalled(&rediscovered, &key_a).unwrap().pane_id,
        a.pane_id
    );
    let other = Server::new();
    assert!(attachment::recalled(&other.identity, &key_a).is_none());
}

#[test]
fn restored_target_keeps_linked_session_and_falls_back_when_targets_close() {
    let server = Server::new();
    let first = server.pane("alpha");
    server.tmux(&["split-window", "-h", "-t", "alpha", "sleep 120"]);
    let pane = server.pane("alpha");
    let session = server.tmux(&["display-message", "-p", "-t", "alpha", "#{session_id}"]);
    server.tmux(&["new-session", "-d", "-s", "beta", "sleep 120"]);
    server.tmux(&["link-window", "-s", "alpha:0", "-t", "beta:1"]);
    let target = attachment::pane_target_in_session(&server.identity, &pane, &session).unwrap();
    server.tmux(&["select-pane", "-t", &first]);
    assert_eq!(
        attachment::restore(&server.identity, &target),
        Some(session.clone())
    );
    assert_eq!(server.pane("alpha"), pane);
    server.tmux(&["kill-pane", "-t", &pane]);
    assert_eq!(
        attachment::restore(&server.identity, &target),
        Some(session.clone())
    );
    assert_eq!(server.pane("alpha"), first);
    server.tmux(&["new-window", "-d", "-t", "alpha", "sleep 120"]);
    server.tmux(&["kill-window", "-t", &target.window_id]);
    assert_eq!(
        attachment::restore(&server.identity, &target),
        Some(session)
    );
    server.tmux(&["kill-session", "-t", "alpha"]);
    assert!(attachment::restore(&server.identity, &target).is_none());
}

#[test]
fn refresh_notifies_foreground_application_without_changing_dimensions() {
    use std::{
        fs,
        process::Stdio,
        thread,
        time::{Duration, Instant},
    };
    let server = Server::new();
    let events = server.directory.path().join("winch-events");
    let script = server.directory.path().join("foreground.sh");
    fs::write(&script, format!(
        "#!/bin/sh\ntrap 'printf winch\\\\n >> {}' WINCH\nprintf ready > {}\nwhile :; do sleep 0.1; done\n",
        events.display(), events.display()
    )).unwrap();
    server.tmux(&[
        "respawn-pane",
        "-k",
        "-t",
        "alpha:0",
        "sh",
        script.to_str().unwrap(),
    ]);
    let deadline = Instant::now() + Duration::from_secs(4);
    while !events.exists() {
        assert!(
            Instant::now() < deadline,
            "foreground application did not start"
        );
        thread::sleep(Duration::from_millis(10));
    }
    let mut client = Command::new("tmux")
        .args(server.identity.tmux_args())
        .args(["-C", "attach-session", "-t", "alpha"])
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let name = loop {
        let name = server.tmux(&["list-clients", "-F", "#{client_name}"]);
        if !name.is_empty() {
            break name;
        }
        assert!(Instant::now() < deadline, "control client did not attach");
        thread::sleep(Duration::from_millis(10));
    };
    // A later client makes beta the implicit current context. Refreshing alpha
    // must still notify alpha, never whichever session tmux currently prefers.
    let beta_events = server.directory.path().join("beta-events");
    let beta_script = server.directory.path().join("beta.sh");
    fs::write(
        &beta_script,
        fs::read_to_string(&script)
            .unwrap()
            .replace(events.to_str().unwrap(), beta_events.to_str().unwrap()),
    )
    .unwrap();
    server.tmux(&[
        "new-session",
        "-d",
        "-s",
        "beta",
        "sh",
        beta_script.to_str().unwrap(),
    ]);
    let mut other_client = Command::new("tmux")
        .args(server.identity.tmux_args())
        .args(["-C", "attach-session", "-t", "beta"])
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    while !beta_events.exists()
        || server
            .tmux(&["list-clients", "-F", "#{client_name}"])
            .lines()
            .count()
            != 2
    {
        assert!(Instant::now() < deadline, "second client did not attach");
        thread::sleep(Duration::from_millis(10));
    }
    let query = tmux_agent_workbench::tmux::Tmux::new(server.identity.clone());
    assert_eq!(
        query
            .client_format(&name, "#{session_name}")
            .unwrap()
            .as_deref(),
        Some("alpha")
    );
    assert!(
        query
            .client_format("missing-client", "#{pane_id}")
            .unwrap()
            .is_none()
    );
    thread::sleep(Duration::from_millis(150));
    let beta_before = fs::read(&beta_events).unwrap().len();
    let dimensions = server.tmux(&[
        "display-message",
        "-p",
        "-t",
        "alpha",
        "#{pane_width}x#{pane_height}",
    ]);
    let before = fs::read(&events).unwrap().len();
    attachment::refresh(&server.identity, &name);
    while fs::read(&events).unwrap().len() <= before {
        assert!(
            Instant::now() < deadline,
            "foreground application did not receive SIGWINCH"
        );
        thread::sleep(Duration::from_millis(10));
    }
    assert_eq!(
        server.tmux(&[
            "display-message",
            "-p",
            "-t",
            "alpha",
            "#{pane_width}x#{pane_height}"
        ]),
        dimensions
    );
    assert_eq!(
        fs::read(&beta_events).unwrap().len(),
        beta_before,
        "refresh must not signal the other client's pane"
    );
    server.tmux(&["set-option", "-g", "@workbench-attach-refresh", "off"]);
    let before = fs::read(&events).unwrap().len();
    attachment::refresh(&server.identity, &name);
    thread::sleep(Duration::from_millis(150));
    assert_eq!(fs::read(&events).unwrap().len(), before);
    let _ = client.kill();
    let _ = client.wait();
    let _ = other_client.kill();
    let _ = other_client.wait();
}
