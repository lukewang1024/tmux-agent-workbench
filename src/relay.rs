use std::collections::HashMap;
use std::fs::{self, OpenOptions};
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::Path;
use std::process::Command;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use fs2::FileExt;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::config::Config;
use crate::model::{
    AgentKind, AgentSnapshot, AttentionEvent, AttentionKind, BaseState, DisplayState, HookHealth,
    RelayFocus, StateConfidence, StateSource, TmuxTarget,
};
use crate::notification::{NotificationBackend, NotificationCategory, SystemBackend};
use crate::paths::Paths;
use crate::server::ServerIdentity;

const MAX_PAYLOAD: usize = 16 * 1024;
const MAX_HEADERS: usize = 8 * 1024;

#[derive(Debug, Clone)]
struct PendingOutbound {
    event: RelayEvent,
    next_attempt_ms: u64,
    deadline_ms: u64,
    delay_ms: u64,
}

impl PendingOutbound {
    fn record_failure(&mut self, now_ms: u64) -> bool {
        if now_ms >= self.deadline_ms {
            return false;
        }
        self.next_attempt_ms = now_ms.saturating_add(self.delay_ms);
        self.delay_ms = (self.delay_ms.saturating_mul(2)).min(8_000);
        true
    }
}

pub struct RelaySender {
    store_path: std::path::PathBuf,
    tmux_socket: String,
    pending: HashMap<String, PendingOutbound>,
}

impl RelaySender {
    pub fn new(paths: &Paths, server: &ServerIdentity) -> Self {
        Self {
            store_path: paths.relay_file(),
            tmux_socket: server.socket_path.display().to_string(),
            pending: HashMap::new(),
        }
    }

    pub fn enqueue(&mut self, agents: &[AgentSnapshot], now_ms: u64) {
        let Ok(store) = load_store(&self.store_path) else {
            return;
        };
        let Some(outbound) = store.outbound else {
            return;
        };
        for agent in agents {
            let Some(attention) = &agent.attention else {
                continue;
            };
            self.pending
                .entry(attention.id.clone())
                .or_insert_with(|| PendingOutbound {
                    event: RelayEvent {
                        event_id: attention.id.clone(),
                        event_type: match attention.kind {
                            AttentionKind::Done => "task.complete",
                            AttentionKind::Blocked => "input.required",
                        }
                        .into(),
                        remote_id: outbound.remote_id.clone(),
                        agent_kind: format!("{:?}", agent.kind).to_ascii_lowercase(),
                        agent_label: agent.label.clone(),
                        session: agent.target.session_name.clone(),
                        reason_category: agent.reason_category.clone(),
                        focus: Some(RelayEventFocus {
                            tmux_socket: self.tmux_socket.clone(),
                            session_id: agent.target.session_id.clone(),
                            pane_id: agent.target.pane_id.clone(),
                        }),
                    },
                    next_attempt_ms: now_ms,
                    deadline_ms: now_ms.saturating_add(60_000),
                    delay_ms: 250,
                });
        }
    }

    pub fn tick(&mut self, now_ms: u64) {
        let Ok(store) = load_store(&self.store_path) else {
            return;
        };
        let Some(outbound) = store.outbound else {
            self.pending.clear();
            return;
        };
        let due: Vec<String> = self
            .pending
            .iter()
            .filter(|(_, item)| item.next_attempt_ms <= now_ms)
            .map(|(id, _)| id.clone())
            .collect();
        for id in due {
            let Some(item) = self.pending.get_mut(&id) else {
                continue;
            };
            if send_event(&outbound, &item.event).is_ok() {
                self.pending.remove(&id);
            } else if !item.record_failure(now_ms) {
                eprintln!("tmux-agent-workbench: relay event {id} dropped after 60 seconds");
                self.pending.remove(&id);
            }
        }
    }
}

fn send_event(outbound: &Outbound, event: &RelayEvent) -> Result<(), String> {
    let port = parse_loopback_endpoint(&outbound.endpoint)?;
    let body = serde_json::to_vec(event).map_err(|error| error.to_string())?;
    if body.len() > MAX_PAYLOAD {
        return Err("relay payload too large".into());
    }
    let address = format!("127.0.0.1:{port}")
        .parse()
        .map_err(|_| "invalid relay address")?;
    let mut stream = TcpStream::connect_timeout(&address, Duration::from_millis(500))
        .map_err(|error| error.to_string())?;
    stream
        .set_read_timeout(Some(Duration::from_secs(1)))
        .map_err(|e| e.to_string())?;
    write!(stream, "POST /v1/events HTTP/1.1\r\nHost: 127.0.0.1\r\nAuthorization: Bearer {}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", outbound.token, body.len()).map_err(|e| e.to_string())?;
    stream.write_all(&body).map_err(|e| e.to_string())?;
    let mut response = [0_u8; 64];
    let count = stream.read(&mut response).map_err(|e| e.to_string())?;
    let status = std::str::from_utf8(&response[..count]).map_err(|e| e.to_string())?;
    if status.starts_with("HTTP/1.1 202 ") {
        Ok(())
    } else {
        Err("relay rejected event".into())
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct RelayStore {
    pub pairings: Vec<Pairing>,
    pub outbound: Option<Outbound>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Pairing {
    pub remote_id: String,
    pub ssh_host: String,
    pub token: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Outbound {
    pub remote_id: String,
    pub token: String,
    pub endpoint: String,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RelayEvent {
    pub event_id: String,
    #[serde(rename = "event")]
    pub event_type: String,
    pub remote_id: String,
    pub agent_kind: String,
    pub agent_label: String,
    pub session: String,
    pub reason_category: Option<String>,
    pub focus: Option<RelayEventFocus>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RelayEventFocus {
    pub tmux_socket: String,
    pub session_id: String,
    pub pane_id: String,
}

struct TokenBucket {
    tokens: f64,
    last: Instant,
}

impl TokenBucket {
    fn new() -> Self {
        Self {
            tokens: 10.0,
            last: Instant::now(),
        }
    }
    fn allow(&mut self, now: Instant) -> bool {
        self.tokens = (10.0_f64).min(self.tokens + now.duration_since(self.last).as_secs_f64());
        self.last = now;
        if self.tokens < 1.0 {
            false
        } else {
            self.tokens -= 1.0;
            true
        }
    }
}

pub fn serve(paths: &Paths, config: &Config) -> Result<(), Box<dyn std::error::Error>> {
    if load_store(&paths.relay_file())?.pairings.is_empty() {
        return Err("relay has no pairings; run relay pair <ssh-host> first".into());
    }
    let lock_path = paths.runtime_dir.join("relay.lock");
    let lock = OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .mode(0o600)
        .open(lock_path)?;
    lock.try_lock_exclusive()
        .map_err(|_| "relay already running")?;
    let listener = TcpListener::bind((config.relay.bind.as_str(), config.relay.port))?;
    let mut limits: HashMap<String, TokenBucket> = HashMap::new();
    let mut seen: HashMap<String, u64> = HashMap::new();
    let mut backend = SystemBackend::new(paths);
    for stream in listener.incoming() {
        let response = match stream {
            Ok(mut stream) => handle_connection(
                &mut stream,
                &paths.relay_file(),
                &mut limits,
                &mut seen,
                config,
                &mut backend,
            ),
            Err(error) => {
                eprintln!("tmux-agent-workbench relay: accept failed: {error}");
                continue;
            }
        };
        if let Err(error) = response {
            eprintln!("tmux-agent-workbench relay: {error}");
        }
    }
    drop(lock);
    Ok(())
}

fn handle_connection<B: NotificationBackend>(
    stream: &mut TcpStream,
    store_path: &Path,
    limits: &mut HashMap<String, TokenBucket>,
    seen: &mut HashMap<String, u64>,
    config: &Config,
    backend: &mut B,
) -> Result<(), String> {
    stream
        .set_read_timeout(Some(Duration::from_secs(2)))
        .map_err(|e| e.to_string())?;
    stream
        .set_write_timeout(Some(Duration::from_secs(2)))
        .map_err(|e| e.to_string())?;
    let request = read_http(stream).map_err(|error| {
        let _ = write_http(stream, error.status, &error.message);
        error.message
    })?;
    if request.method != "POST" || request.path != "/v1/events" {
        write_http(stream, 404, "not found").map_err(|e| e.to_string())?;
        return Ok(());
    }
    let store = load_store(store_path).map_err(|error| error.to_string())?;
    let Some(pairing) = store
        .pairings
        .iter()
        .find(|pairing| constant_time_eq(&pairing.token, &request.token))
    else {
        write_http(stream, 401, "unauthorized").map_err(|e| e.to_string())?;
        return Ok(());
    };
    if !limits
        .entry(pairing.remote_id.clone())
        .or_insert_with(TokenBucket::new)
        .allow(Instant::now())
    {
        write_http(stream, 429, "rate limited").map_err(|e| e.to_string())?;
        return Ok(());
    }
    let event: RelayEvent = match serde_json::from_slice(&request.body) {
        Ok(event) => event,
        Err(error) => {
            write_http(stream, 400, "invalid JSON").map_err(|e| e.to_string())?;
            return Err(error.to_string());
        }
    };
    if let Err(error) = validate_event(&event, &pairing.remote_id) {
        write_http(stream, 400, &error).map_err(|e| e.to_string())?;
        return Ok(());
    }
    let now = now_ms();
    seen.retain(|_, observed| now.saturating_sub(*observed) < 24 * 60 * 60 * 1_000);
    if seen.contains_key(&event.event_id) {
        write_http(stream, 202, "duplicate").map_err(|e| e.to_string())?;
        return Ok(());
    }
    let event_id = event.event_id.clone();
    deliver_event(event, config, backend)?;
    seen.insert(event_id, now);
    write_http(stream, 202, "accepted").map_err(|e| e.to_string())?;
    Ok(())
}

fn deliver_event<B: NotificationBackend>(
    event: RelayEvent,
    config: &Config,
    backend: &mut B,
) -> Result<(), String> {
    let kind = if event.event_type == "task.complete" {
        AttentionKind::Done
    } else {
        AttentionKind::Blocked
    };
    let focus = event.focus.map(|focus| RelayFocus {
        remote_id: event.remote_id.clone(),
        tmux_socket: focus.tmux_socket,
        session_id: focus.session_id.clone(),
        pane_id: focus.pane_id.clone(),
    });
    let agent = AgentSnapshot {
        instance_id: format!("relay-{}", event.event_id),
        kind: parse_kind(&event.agent_kind).unwrap_or(AgentKind::Codex),
        label: event.agent_label,
        target: TmuxTarget {
            session_id: focus
                .as_ref()
                .map(|f| f.session_id.clone())
                .unwrap_or_else(|| "$0".into()),
            session_name: event.session,
            window_id: "@0".into(),
            window_index: 0,
            window_name: String::new(),
            pane_id: focus
                .as_ref()
                .map(|f| f.pane_id.clone())
                .unwrap_or_else(|| "%0".into()),
            pane_index: 0,
        },
        process: None,
        base_state: if kind == AttentionKind::Blocked {
            BaseState::Blocked
        } else {
            BaseState::Idle
        },
        display_state: if kind == AttentionKind::Blocked {
            DisplayState::Blocked
        } else {
            DisplayState::Done
        },
        state_source: StateSource::Hook,
        confidence: StateConfidence::High,
        estimated_state: None,
        hook_health: HookHealth::Healthy,
        reason_category: event.reason_category,
        attention: Some(AttentionEvent {
            id: event.event_id,
            kind,
            seen: false,
            since_unix_ms: now_ms(),
            attention_seq: None,
            seen_seq: None,
        }),
        stale: false,
        visible: false,
        manifest_version: 1,
        rule_id: None,
        hook_session_id: None,
        relay_focus: focus,
        exited: false,
        exited_at_unix_ms: None,
        conversations: Vec::new(),
    };
    if config.notifications.sound {
        let muted = (kind == AttentionKind::Done && config.notifications.mute_done)
            || (kind == AttentionKind::Blocked && config.notifications.mute_request);
        if !muted {
            backend.sound(
                if kind == AttentionKind::Done {
                    NotificationCategory::TaskComplete
                } else {
                    NotificationCategory::InputRequired
                },
                config,
            )?;
        }
    }
    if config.notifications.enabled {
        backend.desktop(&agent, config.notifications.style)?;
    }
    Ok(())
}

pub fn pair(paths: &Paths, ssh_host: &str) -> Result<(), Box<dyn std::error::Error>> {
    validate_ssh_host(ssh_host)?;
    let mut store = load_store(&paths.relay_file())?;
    let remote_id = format!(
        "remote-{}",
        &uuid::Uuid::new_v4().simple().to_string()[..12]
    );
    let token = generate_token();
    let port = Config::load(&paths.config_file())?.relay.port;
    let endpoint = format!("http://127.0.0.1:{port}/v1/events");
    let status = Command::new("ssh")
        .args([
            ssh_host,
            "tmux-agent-workbench",
            "relay",
            "accept-pair",
            "--remote-id",
            &remote_id,
            "--token",
            &token,
            "--endpoint",
            &endpoint,
        ])
        .status()?;
    if !status.success() {
        return Err("remote pairing command failed".into());
    }
    store
        .pairings
        .retain(|pairing| pairing.ssh_host != ssh_host);
    store.pairings.push(Pairing {
        remote_id: remote_id.clone(),
        ssh_host: ssh_host.into(),
        token,
    });
    save_store(&paths.relay_file(), &store)?;
    println!("paired {ssh_host} as {remote_id}");
    println!("add to Host {ssh_host}: RemoteForward {port} localhost:{port}");
    Ok(())
}

pub fn accept_pair(paths: &Paths, outbound: Outbound) -> Result<(), Box<dyn std::error::Error>> {
    validate_remote_id(&outbound.remote_id)?;
    validate_token(&outbound.token)?;
    parse_loopback_endpoint(&outbound.endpoint)?;
    let mut store = load_store(&paths.relay_file())?;
    store.outbound = Some(outbound);
    save_store(&paths.relay_file(), &store)
}

pub fn revoke(paths: &Paths, ssh_host: &str) -> Result<(), Box<dyn std::error::Error>> {
    validate_ssh_host(ssh_host)?;
    let mut store = load_store(&paths.relay_file())?;
    let pairing = store
        .pairings
        .iter()
        .find(|pairing| pairing.ssh_host == ssh_host)
        .cloned()
        .ok_or("pairing not found")?;
    let _ = Command::new("ssh")
        .args([
            ssh_host,
            "tmux-agent-workbench",
            "relay",
            "revoke-local",
            "--remote-id",
            &pairing.remote_id,
        ])
        .status();
    store.pairings.retain(|item| item.ssh_host != ssh_host);
    save_store(&paths.relay_file(), &store)?;
    println!("revoked {ssh_host}");
    Ok(())
}

pub fn revoke_local(paths: &Paths, remote_id: &str) -> Result<(), Box<dyn std::error::Error>> {
    validate_remote_id(remote_id)?;
    let mut store = load_store(&paths.relay_file())?;
    if store
        .outbound
        .as_ref()
        .is_some_and(|item| item.remote_id == remote_id)
    {
        store.outbound = None;
    }
    save_store(&paths.relay_file(), &store)
}

pub fn rotate(paths: &Paths, ssh_host: &str) -> Result<(), Box<dyn std::error::Error>> {
    validate_ssh_host(ssh_host)?;
    let mut store = load_store(&paths.relay_file())?;
    let pairing = store
        .pairings
        .iter_mut()
        .find(|item| item.ssh_host == ssh_host)
        .ok_or("pairing not found")?;
    let old = pairing.token.clone();
    let remote_id = pairing.remote_id.clone();
    let token = generate_token();
    let port = Config::load(&paths.config_file())?.relay.port;
    let endpoint = format!("http://127.0.0.1:{port}/v1/events");
    let status = Command::new("ssh")
        .args([
            ssh_host,
            "tmux-agent-workbench",
            "relay",
            "accept-pair",
            "--remote-id",
            &remote_id,
            "--token",
            &token,
            "--endpoint",
            &endpoint,
        ])
        .status()?;
    if !status.success() {
        return Err("remote token rotation failed".into());
    }
    pairing.token = token;
    let _ = pairing;
    if let Err(error) = save_store(&paths.relay_file(), &store) {
        let _ = Command::new("ssh")
            .args([
                ssh_host,
                "tmux-agent-workbench",
                "relay",
                "accept-pair",
                "--remote-id",
                &remote_id,
                "--token",
                &old,
                "--endpoint",
                &endpoint,
            ])
            .status();
        return Err(error);
    }
    println!("rotated token for {ssh_host}");
    Ok(())
}

pub fn doctor(paths: &Paths, ssh_host: Option<&str>) -> Result<(), Box<dyn std::error::Error>> {
    let store = load_store(&paths.relay_file())?;
    let port = Config::load(&paths.config_file())?.relay.port;
    let mut failures = 0;
    let mode = fs::metadata(paths.relay_file())
        .map(|m| m.permissions().mode() & 0o777)
        .unwrap_or(0);
    println!(
        "relay config: {} (mode {:o})",
        paths.relay_file().display(),
        mode
    );
    if paths.relay_file().exists() && mode != 0o600 {
        failures += 1;
        println!("  error: relay config must have mode 600");
    }
    if store.pairings.is_empty() && store.outbound.is_none() {
        failures += 1;
        println!("  error: no relay pairing configured");
    }
    let mut matched_pairings = 0;
    for pairing in &store.pairings {
        if ssh_host.is_none_or(|host| host == pairing.ssh_host) {
            matched_pairings += 1;
            println!("{} ({})", pairing.ssh_host, pairing.remote_id);
            println!("  RemoteForward {port} localhost:{port}");
            let ssh_ok = Command::new("ssh")
                .args(["-G", &pairing.ssh_host])
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .status()
                .is_ok_and(|status| status.success());
            println!("  ssh alias: {}", if ssh_ok { "ok" } else { "invalid" });
            if !ssh_ok {
                failures += 1;
                continue;
            }
            let endpoint = format!("http://127.0.0.1:{port}/v1/events");
            let tunnel_ok = Command::new("ssh")
                .args([
                    &pairing.ssh_host,
                    "tmux-agent-workbench",
                    "relay",
                    "probe",
                    "--endpoint",
                    &endpoint,
                ])
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .status()
                .is_ok_and(|status| status.success());
            println!(
                "  reverse tunnel: {}",
                if tunnel_ok { "ok" } else { "unreachable" }
            );
            if !tunnel_ok {
                failures += 1;
            }
        }
    }
    if let Some(host) = ssh_host {
        if matched_pairings == 0 {
            failures += 1;
            println!("error: no pairing found for SSH host {host}");
        }
    }
    if ssh_host.is_none() {
        if let Some(outbound) = &store.outbound {
            let reachable = probe(&outbound.endpoint).is_ok();
            println!(
                "outbound {}: {}",
                outbound.remote_id,
                if reachable {
                    "reachable"
                } else {
                    "unreachable"
                }
            );
            if !reachable {
                failures += 1;
            }
        }
    }
    if failures == 0 {
        Ok(())
    } else {
        Err(format!("relay doctor found {failures} issue(s)").into())
    }
}

pub fn probe(endpoint: &str) -> Result<(), Box<dyn std::error::Error>> {
    let port = parse_loopback_endpoint(endpoint)?;
    let address = format!("127.0.0.1:{port}").parse()?;
    TcpStream::connect_timeout(&address, Duration::from_secs(2))?;
    println!("relay endpoint reachable");
    Ok(())
}

pub fn focus_click(
    paths: &Paths,
    remote_id: &str,
    tmux_socket: &str,
    session_id: &str,
    pane_id: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    validate_remote_id(remote_id)?;
    validate_tmux_socket(tmux_socket)?;
    validate_target(session_id, '$')?;
    validate_target(pane_id, '%')?;
    let store = load_store(&paths.relay_file())?;
    let pairing = store
        .pairings
        .iter()
        .find(|pairing| pairing.remote_id == remote_id)
        .ok_or("relay pairing not found")?;
    validate_ssh_host(&pairing.ssh_host)?;
    activate_terminal();
    let status = Command::new("ssh")
        .args([
            &pairing.ssh_host,
            "tmux-agent-workbench",
            "relay",
            "focus-target",
            "--tmux-socket",
            tmux_socket,
            "--session-id",
            session_id,
            "--pane-id",
            pane_id,
        ])
        .status()?;
    if status.success() {
        Ok(())
    } else {
        show_target_expired();
        Err("remote target expired".into())
    }
}

#[cfg(target_os = "macos")]
fn activate_terminal() {
    let bundle = match std::env::var("TERM_PROGRAM").as_deref() {
        Ok("iTerm.app") => Some("com.googlecode.iterm2"),
        Ok("Apple_Terminal") => Some("com.apple.Terminal"),
        Ok("ghostty") => Some("com.mitchellh.ghostty"),
        Ok("WarpTerminal") => Some("dev.warp.Warp-Stable"),
        _ => None,
    };
    if let Some(bundle) = bundle {
        let _ = Command::new("open")
            .args(["-b", bundle])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn();
    }
}

#[cfg(not(target_os = "macos"))]
fn activate_terminal() {}

#[cfg(target_os = "macos")]
fn show_target_expired() {
    let _ = Command::new("osascript")
        .args([
            "-e",
            "display notification \"The tmux target no longer exists\" with title \"Workbench · target expired\"",
        ])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn();
}

#[cfg(target_os = "linux")]
fn show_target_expired() {
    let _ = Command::new("notify-send")
        .args([
            "--app-name",
            "Workbench",
            "Workbench · target expired",
            "The tmux target no longer exists",
        ])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn();
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
fn show_target_expired() {}

pub fn focus_target(
    tmux_socket: &str,
    session_id: &str,
    pane_id: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    validate_tmux_socket(tmux_socket)?;
    validate_target(session_id, '$')?;
    validate_target(pane_id, '%')?;
    let tmux = |args: &[&str]| {
        Command::new("tmux")
            .args(["-S", tmux_socket])
            .args(args)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
    };
    let clients = Command::new("tmux")
        .args([
            "-S",
            tmux_socket,
            "list-clients",
            "-F",
            "#{client_name}\u{1f}#{client_activity}",
        ])
        .output()?;
    if !clients.status.success() {
        return Err("remote tmux server has no attached client".into());
    }
    let client = most_recent_client(&String::from_utf8_lossy(&clients.stdout))
        .ok_or("remote tmux server has no attached client")?;
    let panes = Command::new("tmux")
        .args(["-S", tmux_socket, "list-panes", "-a", "-F", "#{pane_id}"])
        .output()?;
    let pane_exists = panes.status.success()
        && String::from_utf8_lossy(&panes.stdout)
            .lines()
            .any(|candidate| candidate == pane_id);
    if pane_exists {
        if !tmux(&["switch-client", "-c", &client, "-t", pane_id])?.success() {
            return Err("remote pane could not be focused".into());
        }
        return Ok(());
    }
    if tmux(&["has-session", "-t", session_id])?.success() {
        if tmux(&["switch-client", "-c", &client, "-t", session_id])?.success() {
            return Ok(());
        }
    }
    Err("remote target expired".into())
}

fn most_recent_client(output: &str) -> Option<String> {
    output
        .lines()
        .filter_map(|line| {
            let (name, activity) = line.split_once('\u{1f}')?;
            let activity = activity.parse::<u64>().ok()?;
            (!name.is_empty()).then(|| (activity, name.to_owned()))
        })
        .max_by_key(|(activity, _)| *activity)
        .map(|(_, name)| name)
}

pub fn load_store(path: &Path) -> Result<RelayStore, Box<dyn std::error::Error>> {
    if !path.exists() {
        return Ok(RelayStore::default());
    }
    Ok(toml::from_str(&fs::read_to_string(path)?)?)
}

fn save_store(path: &Path, store: &RelayStore) -> Result<(), Box<dyn std::error::Error>> {
    let parent = path.parent().ok_or("relay config has no parent")?;
    fs::create_dir_all(parent)?;
    let temporary = parent.join(format!(".relay.toml.{}", std::process::id()));
    let mut file = OpenOptions::new()
        .create(true)
        .truncate(true)
        .write(true)
        .mode(0o600)
        .open(&temporary)?;
    file.write_all(toml::to_string_pretty(store)?.as_bytes())?;
    file.sync_all()?;
    fs::set_permissions(&temporary, fs::Permissions::from_mode(0o600))?;
    fs::rename(temporary, path)?;
    Ok(())
}

struct HttpRequest {
    method: String,
    path: String,
    token: String,
    body: Vec<u8>,
}
struct HttpError {
    status: u16,
    message: String,
}

fn read_http(stream: &mut TcpStream) -> Result<HttpRequest, HttpError> {
    let mut bytes = Vec::new();
    let mut chunk = [0_u8; 2048];
    let header_end;
    loop {
        let count = stream.read(&mut chunk).map_err(|e| HttpError {
            status: 400,
            message: e.to_string(),
        })?;
        if count == 0 {
            return Err(HttpError {
                status: 400,
                message: "incomplete request".into(),
            });
        }
        bytes.extend_from_slice(&chunk[..count]);
        if let Some(index) = find_bytes(&bytes, b"\r\n\r\n") {
            header_end = index + 4;
            if header_end > MAX_HEADERS {
                return Err(HttpError {
                    status: 431,
                    message: "headers too large".into(),
                });
            }
            break;
        }
        if bytes.len() > MAX_HEADERS {
            return Err(HttpError {
                status: 431,
                message: "headers too large".into(),
            });
        }
    }
    let headers = std::str::from_utf8(&bytes[..header_end]).map_err(|_| HttpError {
        status: 400,
        message: "headers must be UTF-8".into(),
    })?;
    let mut lines = headers.split("\r\n");
    let mut request_line = lines.next().unwrap_or("").split_whitespace();
    let method = request_line.next().unwrap_or("").to_owned();
    let path = request_line.next().unwrap_or("").to_owned();
    let mut length = None;
    let mut token = None;
    for line in lines {
        let Some((name, value)) = line.split_once(':') else {
            continue;
        };
        if name.eq_ignore_ascii_case("content-length") {
            length = value.trim().parse::<usize>().ok();
        }
        if name.eq_ignore_ascii_case("authorization") {
            token = value.trim().strip_prefix("Bearer ").map(str::to_owned);
        }
    }
    let length = length.ok_or_else(|| HttpError {
        status: 411,
        message: "Content-Length required".into(),
    })?;
    if length > MAX_PAYLOAD {
        return Err(HttpError {
            status: 413,
            message: "payload too large".into(),
        });
    }
    while bytes.len() < header_end + length {
        let count = stream.read(&mut chunk).map_err(|e| HttpError {
            status: 400,
            message: e.to_string(),
        })?;
        if count == 0 {
            return Err(HttpError {
                status: 400,
                message: "incomplete body".into(),
            });
        }
        bytes.extend_from_slice(&chunk[..count]);
        if bytes.len() > header_end + MAX_PAYLOAD {
            return Err(HttpError {
                status: 413,
                message: "payload too large".into(),
            });
        }
    }
    Ok(HttpRequest {
        method,
        path,
        token: token.unwrap_or_default(),
        body: bytes[header_end..header_end + length].to_vec(),
    })
}

fn write_http(stream: &mut TcpStream, status: u16, message: &str) -> std::io::Result<()> {
    let reason = match status {
        202 => "Accepted",
        400 => "Bad Request",
        401 => "Unauthorized",
        404 => "Not Found",
        411 => "Length Required",
        413 => "Payload Too Large",
        429 => "Too Many Requests",
        431 => "Request Header Fields Too Large",
        _ => "Error",
    };
    let body = serde_json::json!({"message": message}).to_string();
    write!(
        stream,
        "HTTP/1.1 {status} {reason}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    )
}

fn validate_event(event: &RelayEvent, expected_remote: &str) -> Result<(), String> {
    if !matches!(
        event.event_type.as_str(),
        "task.complete" | "input.required"
    ) {
        return Err("unsupported CESP event".into());
    }
    if event.remote_id != expected_remote {
        return Err("remote_id does not match bearer token".into());
    }
    validate_identifier(&event.event_id, 128, "event_id")?;
    if parse_kind(&event.agent_kind).is_none() {
        return Err("invalid agent_kind".into());
    }
    validate_text(&event.agent_label, 128, "agent_label")?;
    validate_text(&event.session, 128, "session")?;
    if let Some(reason) = &event.reason_category {
        validate_identifier(reason, 64, "reason_category")?;
    }
    if let Some(focus) = &event.focus {
        validate_tmux_socket(&focus.tmux_socket)?;
        validate_target(&focus.session_id, '$')?;
        validate_target(&focus.pane_id, '%')?;
    }
    Ok(())
}

fn validate_ssh_host(value: &str) -> Result<(), String> {
    validate_identifier(value, 128, "ssh host")?;
    if value.starts_with('-') {
        Err("invalid ssh host".into())
    } else {
        Ok(())
    }
}
fn validate_remote_id(value: &str) -> Result<(), String> {
    validate_identifier(value, 128, "remote_id")
}
fn validate_token(value: &str) -> Result<(), String> {
    if value.len() == 64 && value.bytes().all(|b| b.is_ascii_hexdigit()) {
        Ok(())
    } else {
        Err("invalid token".into())
    }
}
fn validate_identifier(value: &str, max: usize, name: &str) -> Result<(), String> {
    if !value.is_empty()
        && value.len() <= max
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'))
    {
        Ok(())
    } else {
        Err(format!("invalid {name}"))
    }
}
fn validate_text(value: &str, max: usize, name: &str) -> Result<(), String> {
    if !value.is_empty() && value.len() <= max && !value.chars().any(char::is_control) {
        Ok(())
    } else {
        Err(format!("invalid {name}"))
    }
}
fn validate_target(value: &str, prefix: char) -> Result<(), String> {
    if value
        .strip_prefix(prefix)
        .is_some_and(|rest| !rest.is_empty() && rest.bytes().all(|b| b.is_ascii_digit()))
    {
        Ok(())
    } else {
        Err("invalid tmux target".into())
    }
}
fn validate_tmux_socket(value: &str) -> Result<(), String> {
    let safe_components = Path::new(value).components().all(|component| {
        matches!(
            component,
            std::path::Component::RootDir | std::path::Component::Normal(_)
        )
    });
    if value.starts_with('/')
        && value.len() <= 256
        && safe_components
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'/' | b'-' | b'_' | b'.'))
    {
        Ok(())
    } else {
        Err("invalid tmux socket".into())
    }
}
fn parse_loopback_endpoint(value: &str) -> Result<u16, String> {
    value
        .strip_prefix("http://127.0.0.1:")
        .and_then(|rest| rest.strip_suffix("/v1/events"))
        .and_then(|port| port.parse::<u16>().ok())
        .filter(|port| *port != 0)
        .ok_or_else(|| "invalid relay endpoint".to_owned())
}
fn parse_kind(value: &str) -> Option<AgentKind> {
    match value.to_ascii_lowercase().as_str() {
        "codex" => Some(AgentKind::Codex),
        "claude" => Some(AgentKind::Claude),
        "trae" => Some(AgentKind::Trae),
        "opencode" => Some(AgentKind::Opencode),
        _ => None,
    }
}
fn generate_token() -> String {
    let mut hash = Sha256::new();
    hash.update(uuid::Uuid::new_v4().as_bytes());
    hash.update(uuid::Uuid::new_v4().as_bytes());
    format!("{:x}", hash.finalize())
}
fn constant_time_eq(a: &str, b: &str) -> bool {
    if a.len() != b.len() {
        return false;
    }
    a.bytes()
        .zip(b.bytes())
        .fold(0_u8, |diff, (x, y)| diff | (x ^ y))
        == 0
}
fn find_bytes(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}
fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Default)]
    struct FakeBackend {
        desktops: usize,
        sounds: usize,
    }

    impl NotificationBackend for FakeBackend {
        fn sound(
            &mut self,
            _category: NotificationCategory,
            _config: &Config,
        ) -> Result<(), String> {
            self.sounds += 1;
            Ok(())
        }

        fn desktop(
            &mut self,
            _agent: &AgentSnapshot,
            _style: crate::config::NotificationStyle,
        ) -> Result<(), String> {
            self.desktops += 1;
            Ok(())
        }
    }

    fn event() -> RelayEvent {
        RelayEvent {
            event_id: "evt-1".into(),
            event_type: "task.complete".into(),
            remote_id: "remote-1".into(),
            agent_kind: "codex".into(),
            agent_label: "Codex build".into(),
            session: "build".into(),
            reason_category: None,
            focus: Some(RelayEventFocus {
                tmux_socket: "/tmp/tmux-1/default".into(),
                session_id: "$1".into(),
                pane_id: "%2".into(),
            }),
        }
    }

    fn relay_store(path: &Path) -> String {
        let token = "a".repeat(64);
        save_store(
            path,
            &RelayStore {
                pairings: vec![Pairing {
                    remote_id: "remote-1".into(),
                    ssh_host: "fixture".into(),
                    token: token.clone(),
                }],
                outbound: None,
            },
        )
        .unwrap();
        token
    }

    fn http_request(token: &str, body: &[u8]) -> Vec<u8> {
        let mut request = format!(
            "POST /v1/events HTTP/1.1\r\nHost: localhost\r\nAuthorization: Bearer {token}\r\nContent-Length: {}\r\n\r\n",
            body.len()
        )
        .into_bytes();
        request.extend_from_slice(body);
        request
    }

    fn round_trip(
        request: Vec<u8>,
        store_path: &Path,
        limits: &mut HashMap<String, TokenBucket>,
        seen: &mut HashMap<String, u64>,
        backend: &mut FakeBackend,
    ) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let client = std::thread::spawn(move || {
            let mut stream = TcpStream::connect(address).unwrap();
            stream.write_all(&request).unwrap();
            let mut response = String::new();
            stream.read_to_string(&mut response).unwrap();
            response
        });
        let (mut stream, _) = listener.accept().unwrap();
        let mut config = Config::default();
        config.notifications.sound = false;
        let _ = handle_connection(&mut stream, store_path, limits, seen, &config, backend);
        drop(stream);
        client.join().unwrap()
    }

    #[test]
    fn validates_whitelist_and_injection_fields() {
        assert!(validate_event(&event(), "remote-1").is_ok());
        let mut invalid = event();
        invalid.event_type = "task.error".into();
        assert!(validate_event(&invalid, "remote-1").is_err());
        let mut invalid = event();
        invalid.focus.as_mut().unwrap().pane_id = "%2;run-shell".into();
        assert!(validate_event(&invalid, "remote-1").is_err());
        let mut invalid = event();
        invalid.remote_id = "other".into();
        assert!(validate_event(&invalid, "remote-1").is_err());
        let mut invalid = event();
        invalid.focus.as_mut().unwrap().tmux_socket = "/tmp/../victim".into();
        assert!(validate_event(&invalid, "remote-1").is_err());
        assert!(validate_ssh_host("-V").is_err());
    }

    #[test]
    fn token_bucket_has_burst_ten_then_refills() {
        let mut bucket = TokenBucket::new();
        let now = Instant::now();
        for _ in 0..10 {
            assert!(bucket.allow(now));
        }
        assert!(!bucket.allow(now));
        assert!(bucket.allow(now + Duration::from_secs(1)));
    }

    #[test]
    fn store_is_written_mode_0600() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("relay.toml");
        save_store(&path, &RelayStore::default()).unwrap();
        assert_eq!(
            fs::metadata(path).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }

    #[test]
    fn outbound_failure_uses_bounded_exponential_backoff() {
        let mut pending = PendingOutbound {
            event: event(),
            next_attempt_ms: 1_000,
            deadline_ms: 61_000,
            delay_ms: 250,
        };
        assert!(pending.record_failure(1_000));
        assert_eq!((pending.next_attempt_ms, pending.delay_ms), (1_250, 500));
        assert!(pending.record_failure(1_250));
        assert_eq!((pending.next_attempt_ms, pending.delay_ms), (1_750, 1_000));
        for now in [2_000, 4_000, 8_000, 16_000] {
            assert!(pending.record_failure(now));
        }
        assert_eq!(pending.delay_ms, 8_000);
        assert!(!pending.record_failure(61_000));
    }

    #[test]
    fn remote_focus_chooses_most_recent_attached_client() {
        let clients = "client-old\u{1f}10\nclient-new\u{1f}42\n";
        assert_eq!(most_recent_client(clients).as_deref(), Some("client-new"));
        assert_eq!(most_recent_client(""), None);
    }

    #[test]
    fn http_relay_authenticates_delivers_and_deduplicates_events() {
        let temp = tempfile::tempdir().unwrap();
        let store_path = temp.path().join("relay.toml");
        let token = relay_store(&store_path);
        let body = serde_json::to_vec(&event()).unwrap();
        let mut limits = HashMap::new();
        let mut seen = HashMap::new();
        let mut backend = FakeBackend::default();

        let unauthorized = round_trip(
            http_request(&"b".repeat(64), &body),
            &store_path,
            &mut limits,
            &mut seen,
            &mut backend,
        );
        assert!(unauthorized.starts_with("HTTP/1.1 401"));
        assert_eq!(backend.desktops, 0);

        let accepted = round_trip(
            http_request(&token, &body),
            &store_path,
            &mut limits,
            &mut seen,
            &mut backend,
        );
        assert!(accepted.starts_with("HTTP/1.1 202"));
        assert_eq!(backend.desktops, 1);

        let duplicate = round_trip(
            http_request(&token, &body),
            &store_path,
            &mut limits,
            &mut seen,
            &mut backend,
        );
        assert!(duplicate.starts_with("HTTP/1.1 202"));
        assert_eq!(backend.desktops, 1);
    }

    #[test]
    fn http_relay_rejects_payload_over_16_kib() {
        let temp = tempfile::tempdir().unwrap();
        let store_path = temp.path().join("relay.toml");
        let token = relay_store(&store_path);
        let mut limits = HashMap::new();
        let mut seen = HashMap::new();
        let mut backend = FakeBackend::default();
        let mut oversized = http_request(&token, &vec![b'x'; MAX_PAYLOAD + 1]);
        let header_end = find_bytes(&oversized, b"\r\n\r\n").unwrap() + 4;
        oversized.truncate(header_end);
        let response = round_trip(oversized, &store_path, &mut limits, &mut seen, &mut backend);
        assert!(response.starts_with("HTTP/1.1 413"));
        assert_eq!(backend.desktops, 0);
    }
}
