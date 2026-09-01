use std::fs::{self, File, OpenOptions};
use std::io;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::os::unix::net::UnixListener;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, RwLock};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use fs2::FileExt;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use thiserror::Error;

use crate::client::{ClientRegistry, FocusState};
use crate::config::{Config, ConfigError};
use crate::detection::{Detector, MetadataReport};
use crate::ipc::{Request, Response, read_request, write_response};
use crate::manifest::{ManifestError, ManifestSet};
use crate::model::{AgentEventReport, DetachedAgentEventReport, Snapshot};
use crate::notification::{NotificationScheduler, SystemBackend};
use crate::paths::Paths;
use crate::relay::RelaySender;
use crate::server::ServerIdentity;
use crate::tmux::{Tmux, TmuxSource};

#[derive(Debug, Error)]
pub enum DaemonError {
    #[error("daemon already running for tmux server {0}")]
    AlreadyRunning(String),
    #[error(transparent)]
    Io(#[from] io::Error),
    #[error(transparent)]
    Config(#[from] ConfigError),
    #[error(transparent)]
    Manifest(#[from] ManifestError),
}

#[derive(Debug, Clone, Serialize)]
pub struct Status {
    pub engine_version: &'static str,
    pub protocol_version: u32,
    pub server: String,
    pub pid: u32,
    pub generation: u64,
    pub ipc_in_flight: u64,
    pub ipc_peak_in_flight: u64,
    pub ipc_accepted: u64,
    pub ipc_slow: u64,
}

#[derive(Default)]
struct IpcMetrics {
    in_flight: AtomicU64,
    peak_in_flight: AtomicU64,
    accepted: AtomicU64,
    slow: AtomicU64,
}

struct State {
    config: Config,
    manifests: ManifestSet,
    snapshot: Snapshot,
    detector: Detector,
    notifier: NotificationScheduler,
    notification_backend: SystemBackend,
    relay_sender: RelaySender,
    clients: ClientRegistry,
    server_incarnation: String,
    semantic_router: crate::semantic::SemanticRouter,
    recovered_pending: Vec<crate::semantic::SemanticEvent>,
    recovered_runtimes: Vec<crate::checkpoint::RuntimeCheckpoint>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ExplainParams {
    pane_id: String,
    #[serde(default)]
    show_content: bool,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct AckParams {
    event_id: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ClientRegisterParams {
    device_id: String,
    device_label: String,
    kind: String,
    capabilities: Vec<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ClientBindParams {
    token: String,
    attachment: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ClientHeartbeatParams {
    endpoint_id: String,
    activity_unix_ms: u64,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ClientFocusParams {
    endpoint_id: String,
    focused: Option<bool>,
    overlay_visible: bool,
    target: Option<crate::model::TmuxTarget>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ClientAcceptedParams {
    endpoint_id: String,
    event_id: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ClientDetachParams {
    endpoint_id: String,
}

pub fn serve(paths: &Paths, server: &ServerIdentity) -> Result<(), DaemonError> {
    let lock_path = paths.lock_for_server(&server.key);
    let lock = open_lock(&lock_path)?;
    lock.try_lock_exclusive()
        .map_err(|_| DaemonError::AlreadyRunning(server.key.clone()))?;

    let socket_path = paths.socket_for_server(&server.key);
    remove_stale_socket(&socket_path)?;
    let listener = UnixListener::bind(&socket_path)?;
    set_socket_mode(&socket_path)?;
    listener.set_nonblocking(true)?;

    let config = Config::load(&paths.config_file())?;
    let manifests = ManifestSet::load(&paths.manifests_dir())?;
    let server_incarnation = server_incarnation(server);
    let checkpoint_path = paths.checkpoint_for_server(&server.key);
    let recovered_checkpoint = match crate::checkpoint::load_server(&checkpoint_path) {
        Ok(Some(checkpoint)) if checkpoint.server_incarnation == server_incarnation => {
            Some(checkpoint)
        }
        Ok(Some(_)) => {
            let _ = fs::remove_file(&checkpoint_path);
            None
        }
        Ok(None) => None,
        Err(error) => {
            eprintln!("tmux-agent-workbench: ignoring corrupt checkpoint: {error}");
            None
        }
    };
    let mut semantic_router = crate::semantic::SemanticRouter::default();
    let mut recovered_pending = Vec::new();
    let mut recovered_runtimes = Vec::new();
    if let Some(checkpoint) = recovered_checkpoint {
        let mut seen = std::collections::HashSet::new();
        let mut delivered = Vec::new();
        for runtime in checkpoint.runtimes {
            recovered_runtimes.push(runtime.clone());
            delivered.extend(runtime.delivered_event_ids);
            recovered_pending.extend(runtime.pending.into_iter().filter(|event| {
                now_unix_ms() <= event.deadline_unix_ms && seen.insert(event.id.clone())
            }));
        }
        semantic_router.restore_accepted(delivered);
    }
    let state = Arc::new(RwLock::new(State {
        config,
        manifests,
        snapshot: Snapshot::empty(server.socket_path.display().to_string(), now_unix_ms()),
        detector: Detector::new(server.clone()),
        notifier: NotificationScheduler::default(),
        notification_backend: SystemBackend::new(paths),
        relay_sender: RelaySender::new(paths, server),
        clients: ClientRegistry::default(),
        server_incarnation,
        semantic_router,
        recovered_pending,
        recovered_runtimes,
    }));
    if state
        .read()
        .expect("state poisoned")
        .config
        .clients
        .selected_implies_focused
    {
        let _ = std::process::Command::new("tmux")
            .arg("-S")
            .arg(&server.socket_path)
            .args([
                "set-option",
                "-gq",
                "@workbench_selected_implies_focused",
                "1",
            ])
            .status();
    }
    let snapshot = Arc::new(RwLock::new(
        state.read().expect("state poisoned").snapshot.clone(),
    ));
    let shutdown = Arc::new(AtomicBool::new(false));
    let ipc_metrics = Arc::new(IpcMetrics::default());

    let outcome = serve_loop(
        listener,
        state,
        snapshot,
        shutdown,
        ipc_metrics,
        paths,
        server,
    );
    let _ = fs::remove_file(&socket_path);
    drop(lock);
    outcome
}

fn serve_loop(
    listener: UnixListener,
    state: Arc<RwLock<State>>,
    snapshot: Arc<RwLock<Snapshot>>,
    shutdown: Arc<AtomicBool>,
    ipc_metrics: Arc<IpcMetrics>,
    paths: &Paths,
    server: &ServerIdentity,
) -> Result<(), DaemonError> {
    let scan_state = Arc::clone(&state);
    let scan_snapshot = Arc::clone(&snapshot);
    let scan_shutdown = Arc::clone(&shutdown);
    let scan_server = server.clone();
    let scan_paths = paths.clone();
    let _scanner = thread::spawn(move || {
        let tmux = Tmux::new(scan_server.clone());
        let mut next_liveness_check = Instant::now();
        while !scan_shutdown.load(Ordering::Relaxed) && scan_server.socket_path.exists() {
            if Instant::now() >= next_liveness_check {
                if !tmux.server_alive() {
                    scan_shutdown.store(true, Ordering::Relaxed);
                    break;
                }
                next_liveness_check = Instant::now() + Duration::from_secs(1);
            }
            update_snapshot(&scan_state, &scan_snapshot, &scan_paths, &scan_server.key);
            thread::sleep(Duration::from_millis(50));
        }
    });

    while !shutdown.load(Ordering::Relaxed) && server.socket_path.exists() {
        // A server can have one sidebar per window. Drain the ready queue in a
        // batch so several UIs refreshing together cannot saturate a loop that
        // used to accept only one client every 50 ms.
        for _ in 0..64 {
            match listener.accept() {
                Ok((stream, _)) => {
                    let state = Arc::clone(&state);
                    let snapshot = Arc::clone(&snapshot);
                    let shutdown = Arc::clone(&shutdown);
                    let metrics = Arc::clone(&ipc_metrics);
                    let paths = paths.clone();
                    let server = server.clone();
                    let in_flight = metrics.in_flight.fetch_add(1, Ordering::Relaxed) + 1;
                    metrics.accepted.fetch_add(1, Ordering::Relaxed);
                    metrics
                        .peak_in_flight
                        .fetch_max(in_flight, Ordering::Relaxed);
                    thread::spawn(move || {
                        let started = Instant::now();
                        // A stalled or malformed client is connection-local and
                        // must not hold up snapshots for every other sidebar.
                        let _ = stream.set_read_timeout(Some(Duration::from_secs(2)));
                        let _ = stream.set_write_timeout(Some(Duration::from_secs(2)));
                        let response = match read_request(&stream) {
                            Ok(request) => handle(
                                request, &state, &snapshot, &shutdown, &metrics, &paths, &server,
                            ),
                            Err(error) => {
                                Response::error(String::new(), "invalid_request", error.to_string())
                            }
                        };
                        let _ = write_response(&stream, &response);
                        metrics.in_flight.fetch_sub(1, Ordering::Relaxed);
                        if started.elapsed() >= Duration::from_millis(250) {
                            metrics.slow.fetch_add(1, Ordering::Relaxed);
                        }
                    });
                }
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => break,
                Err(error) => return Err(error.into()),
            }
        }
        thread::sleep(Duration::from_millis(50));
    }
    shutdown.store(true, Ordering::Relaxed);
    Ok(())
}

fn update_snapshot(
    state: &RwLock<State>,
    published: &RwLock<Snapshot>,
    paths: &Paths,
    server_key: &str,
) {
    let mut state = state.write().expect("state poisoned");
    let config = state.config.clone();
    let manifests = state.manifests.clone();
    let now = now_unix_ms();
    if state.detector.tick(&config, &manifests, now).is_ok() {
        if !state.recovered_runtimes.is_empty() {
            let checkpoints = std::mem::take(&mut state.recovered_runtimes);
            state.recovered_runtimes = state.detector.restore_checkpoints(&checkpoints, now);
        }
        for report in crate::hooks::drain_spool(paths, server_key, now) {
            if let Ok(agent) = state.detector.report_agent_event(&report) {
                if report.event == crate::model::AgentEventType::SessionStart
                    && report.reason_category.as_deref() != Some("compact")
                {
                    state.notifier.observe_session_start(
                        report.occurred_at_unix_ms,
                        &report.event_id,
                        &agent,
                    );
                }
                if report.event == crate::model::AgentEventType::Error {
                    state.notifier.observe_task_error(
                        report.occurred_at_unix_ms,
                        &report.event_id,
                        &agent,
                    );
                    route_instant_event(&mut state, &report, &agent, now);
                }
            }
        }
        let agents = state.detector.machine_snapshots();
        let sessions = state.detector.sessions();
        let changed = agents != state.snapshot.agents || sessions != state.snapshot.sessions;
        if agents != state.snapshot.agents {
            state.snapshot.agents = agents;
        }
        if sessions != state.snapshot.sessions {
            state.snapshot.sessions = sessions;
        }
        state.clients.prune(now);
        sync_attached_client_focus(&mut state, now);
        let clients = state.clients.snapshots(now);
        if clients != state.snapshot.clients {
            state.snapshot.clients = clients;
            state.snapshot.generation = state.snapshot.generation.saturating_add(1);
        }
        if changed {
            state.snapshot.generation = state.snapshot.generation.saturating_add(1);
            publish_status_fragments(&state.snapshot);
            persist_checkpoint(paths, server_key, &state);
        }
        state.snapshot.observed_at_unix_ms = now;
        let agents = state.snapshot.agents.clone();
        route_new_attention(&mut state, now);
        route_recovered_events(&mut state, now);
        state.notifier.observe(now, &agents);
        let mut notifier = std::mem::take(&mut state.notifier);
        let delivered = if state.clients.ranked("notification", now).is_empty() {
            notifier.deliver_due(now, &agents, &config, &mut state.notification_backend)
        } else {
            Vec::new()
        };
        state.notifier = notifier;
        state.relay_sender.enqueue(&delivered, now);
        *published.write().expect("snapshot poisoned") = state.snapshot.clone();
    }
    state.relay_sender.tick(now);
}

fn route_recovered_events(state: &mut State, now_ms: u64) {
    let events = std::mem::take(&mut state.recovered_pending);
    for event in events {
        if now_ms > event.deadline_unix_ms {
            continue;
        }
        match state.semantic_router.route(&event, &state.clients, now_ms) {
            crate::semantic::RouteDecision::Deliver { endpoints, .. } => {
                if let Some(endpoint) = endpoints.first() {
                    let _ = state.clients.queue(endpoint, event.clone());
                }
                state.recovered_pending.push(event);
            }
            crate::semantic::RouteDecision::Watched { .. }
            | crate::semantic::RouteDecision::Silent => {}
            crate::semantic::RouteDecision::Expired => {}
        }
    }
}

fn sync_attached_client_focus(state: &mut State, now_ms: u64) {
    let server = match ServerIdentity::discover() {
        Ok(server) => server,
        Err(_) => return,
    };
    let output = match std::process::Command::new("tmux")
        .arg("-S")
        .arg(&server.socket_path)
        .args([
            "list-clients",
            "-F",
            "#{client_tty}\u{1f}#{client_flags}\u{1f}#{pane_id}\u{1f}#{@workbench_overlay_visible}",
        ])
        .output()
    {
        Ok(output) if output.status.success() => output,
        _ => return,
    };
    for line in String::from_utf8_lossy(&output.stdout).lines() {
        let fields: Vec<_> = line.split('\u{1f}').collect();
        if fields.len() != 4 {
            continue;
        }
        let focus = if fields[1].split(',').any(|flag| flag == "focused") {
            FocusState::Focused
        } else {
            FocusState::Unknown
        };
        let target = state
            .snapshot
            .agents
            .iter()
            .find(|agent| agent.target.pane_id == fields[2])
            .map(|agent| agent.target.clone());
        state
            .clients
            .update_attachment_focus(fields[0], focus, fields[3] == "1", target, now_ms);
    }
}

fn route_new_attention(state: &mut State, now_ms: u64) {
    let new_events: Vec<_> = state
        .snapshot
        .agents
        .iter()
        .filter_map(|agent| {
            let attention = agent.attention.as_ref()?;
            if attention.seen {
                return None;
            }
            let category = match attention.kind {
                crate::model::AttentionKind::Done => {
                    crate::semantic::SemanticCategory::TaskComplete
                }
                crate::model::AttentionKind::Blocked => {
                    crate::semantic::SemanticCategory::InputRequired
                }
            };
            Some(crate::semantic::SemanticEvent {
                id: attention.id.clone(),
                category,
                target: agent.target.clone(),
                created_unix_ms: attention.since_unix_ms,
                deadline_unix_ms: attention
                    .since_unix_ms
                    .saturating_add(category.horizon_ms()),
                title: format!("Workbench · {}", agent.label),
                body: format!("{} · {}", category.name(), agent.target.session_name),
            })
        })
        .collect();
    for event in new_events {
        match state.semantic_router.route(&event, &state.clients, now_ms) {
            crate::semantic::RouteDecision::Watched {
                mark_seen: true, ..
            } => {
                let _ = state.detector.acknowledge(&event.id);
            }
            crate::semantic::RouteDecision::Deliver { endpoints, .. } => {
                if let Some(endpoint) = endpoints.first() {
                    let _ = state.clients.queue(endpoint, event);
                }
            }
            _ => {}
        }
    }
}

fn server_incarnation(server: &ServerIdentity) -> String {
    let output = std::process::Command::new("tmux")
        .arg("-S")
        .arg(&server.socket_path)
        .args(["display-message", "-p", "#{pid}"])
        .output();
    let pid = output
        .ok()
        .filter(|output| output.status.success())
        .map(|output| String::from_utf8_lossy(&output.stdout).trim().to_owned())
        .unwrap_or_default();
    format!("{}:{pid}", server.key)
}

fn persist_checkpoint(paths: &Paths, server_key: &str, state: &State) {
    let pending = state.clients.pending_events();
    let delivered_event_ids = state.semantic_router.accepted_event_ids();
    let recent_endpoint = state
        .clients
        .ranked("notification", now_unix_ms())
        .first()
        .map(|endpoint| endpoint.id.clone());
    let runtimes = state
        .snapshot
        .agents
        .iter()
        .filter_map(|agent| {
            let process = agent.process.as_ref()?;
            let (runtime_id, attention_seq, seen_seq, hook_session_id) = state
                .detector
                .checkpoint_metadata(&agent.instance_id)
                .unwrap_or_else(|| (agent.instance_id.clone(), 0, 0, None));
            Some(crate::checkpoint::RuntimeCheckpoint {
                version: 1,
                server_incarnation: state.server_incarnation.clone(),
                runtime_id,
                process_fingerprint: format!(
                    "{}:{}:{}",
                    process.pid, process.started_at_ticks, process.executable
                ),
                previous_state: format!("{:?}", agent.base_state).to_lowercase(),
                attention_seq,
                seen_seq,
                hook_session_id,
                delivered_event_ids: delivered_event_ids.clone(),
                pending: pending.clone(),
                recent_endpoint: recent_endpoint.clone(),
            })
        })
        .collect();
    let checkpoint = crate::checkpoint::ServerCheckpoint {
        version: 1,
        server_incarnation: state.server_incarnation.clone(),
        updated_unix_ms: now_unix_ms(),
        runtimes,
    };
    if let Err(error) =
        crate::checkpoint::store_server(&paths.checkpoint_for_server(server_key), &checkpoint)
    {
        eprintln!("tmux-agent-workbench: checkpoint write failed: {error}");
    }
}

fn publish_status_fragments(snapshot: &Snapshot) {
    let blocked = snapshot
        .agents
        .iter()
        .filter(|agent| agent.display_state == crate::model::DisplayState::Blocked)
        .count();
    let done = snapshot
        .agents
        .iter()
        .filter(|agent| agent.display_state == crate::model::DisplayState::Done)
        .count();
    let working = snapshot
        .agents
        .iter()
        .filter(|agent| agent.display_state == crate::model::DisplayState::Working)
        .count();
    let unseen = snapshot
        .agents
        .iter()
        .filter(|agent| agent.attention.as_ref().is_some_and(|event| !event.seen))
        .count();
    let fragments = [
        (
            "@workbench_status_tiny",
            if unseen > 0 {
                format!("A:{unseen}")
            } else {
                String::new()
            },
        ),
        ("@workbench_status_compact", format!("A {working}/{unseen}")),
        (
            "@workbench_status_normal",
            format!("Agents {working} work · {blocked} block · {done} done"),
        ),
        (
            "@workbench_status_wide",
            format!(
                "Agents {} · {working} working · {blocked} blocked · {done} done · {unseen} unseen",
                snapshot.agents.len()
            ),
        ),
    ];
    let server = match ServerIdentity::discover() {
        Ok(server) => server,
        Err(_) => return,
    };
    for (name, value) in fragments {
        let _ = std::process::Command::new("tmux")
            .arg("-S")
            .arg(&server.socket_path)
            .args(["set-option", "-gq", name, &value])
            .status();
    }
    publish_window_statuses(snapshot, &server);
}

fn publish_window_statuses(snapshot: &Snapshot, server: &ServerIdentity) {
    use std::collections::HashMap;

    let mut by_window: HashMap<&str, Vec<&crate::model::AgentSnapshot>> = HashMap::new();
    for agent in &snapshot.agents {
        by_window
            .entry(agent.target.window_id.as_str())
            .or_default()
            .push(agent);
    }

    let output = match std::process::Command::new("tmux")
        .arg("-S")
        .arg(&server.socket_path)
        .args(["list-windows", "-a", "-F", "#{window_id}"])
        .output()
    {
        Ok(output) if output.status.success() => output,
        _ => return,
    };
    let windows = String::from_utf8_lossy(&output.stdout);
    let mut command = std::process::Command::new("tmux");
    command.arg("-S").arg(&server.socket_path);
    let mut first = true;
    for window in windows.lines().filter(|window| window.starts_with('@')) {
        if !first {
            command.arg(";");
        }
        first = false;
        command.args(["set-option", "-wu", "-t", window, "@workbench_window_state"]);
        command.arg(";");
        command.args(["set-option", "-wu", "-t", window, "@workbench_window_label"]);
        if let Some(agents) = by_window.get(window) {
            let (state, count) = window_rollup(agents);
            let label = if count > 1 {
                format!("{} {count}", state.to_uppercase())
            } else {
                state.to_uppercase()
            };
            command.arg(";");
            command.args([
                "set-option",
                "-wq",
                "-t",
                window,
                "@workbench_window_state",
                state,
            ]);
            command.arg(";");
            command.args([
                "set-option",
                "-wq",
                "-t",
                window,
                "@workbench_window_label",
                &label,
            ]);
        }
    }
    if !first {
        let _ = command.status();
    }
}

fn window_rollup(agents: &[&crate::model::AgentSnapshot]) -> (&'static str, usize) {
    let blocked = agents
        .iter()
        .filter(|agent| agent.display_state == crate::model::DisplayState::Blocked)
        .count();
    if blocked > 0 {
        return ("blocked", blocked);
    }
    let working = agents
        .iter()
        .filter(|agent| agent.display_state == crate::model::DisplayState::Working)
        .count();
    if working > 0 {
        return ("working", working);
    }
    let done = agents
        .iter()
        .filter(|agent| {
            agent.display_state == crate::model::DisplayState::Done
                && agent.attention.as_ref().is_some_and(|event| !event.seen)
        })
        .count();
    if done > 0 {
        return ("done", done);
    }
    let unknown = agents
        .iter()
        .filter(|agent| agent.display_state == crate::model::DisplayState::Unknown)
        .count();
    if unknown > 0 {
        return ("unknown", unknown);
    }
    ("idle", agents.len())
}

fn handle(
    request: Request,
    state: &RwLock<State>,
    snapshot: &RwLock<Snapshot>,
    shutdown: &AtomicBool,
    ipc_metrics: &IpcMetrics,
    paths: &Paths,
    server: &ServerIdentity,
) -> Response {
    if request.protocol_version != crate::IPC_PROTOCOL_VERSION {
        return Response::error(
            request.id,
            "protocol_mismatch",
            format!(
                "expected {}, got {}",
                crate::IPC_PROTOCOL_VERSION,
                request.protocol_version
            ),
        );
    }
    let result: Result<Value, String> = match request.method.as_str() {
        "daemon.status" => {
            let generation = snapshot.read().expect("snapshot poisoned").generation;
            serde_json::to_value(Status {
                engine_version: crate::ENGINE_VERSION,
                protocol_version: crate::IPC_PROTOCOL_VERSION,
                server: server.socket_path.display().to_string(),
                pid: std::process::id(),
                generation,
                ipc_in_flight: ipc_metrics.in_flight.load(Ordering::Relaxed),
                ipc_peak_in_flight: ipc_metrics.peak_in_flight.load(Ordering::Relaxed),
                ipc_accepted: ipc_metrics.accepted.load(Ordering::Relaxed),
                ipc_slow: ipc_metrics.slow.load(Ordering::Relaxed),
            })
            .map_err(|error| error.to_string())
        }
        "snapshot.get" => serde_json::to_value(&*snapshot.read().expect("snapshot poisoned"))
            .map_err(|error| error.to_string()),
        "client.register" => parse_params::<ClientRegisterParams>(request.params).and_then(|params| {
            uuid::Uuid::parse_str(&params.device_id).map_err(|_| "invalid device id")?;
            let mut state = state.write().expect("state poisoned");
            let (endpoint_id, attachment_token) = state.clients.register(params.device_id, params.device_label, params.kind, params.capabilities, now_unix_ms());
            Ok(json!({"endpoint_id": endpoint_id, "attachment_token": attachment_token, "heartbeat_seconds": 15}))
        }),
        "client.bind" => parse_params::<ClientBindParams>(request.params).and_then(|params| {
            let mut state = state.write().expect("state poisoned");
            let endpoint_id = state.clients.bind(&params.token, params.attachment, now_unix_ms())?;
            Ok(json!({"endpoint_id": endpoint_id, "bound": true}))
        }),
        "client.heartbeat" => parse_params::<ClientHeartbeatParams>(request.params).and_then(|params| {
            let mut state = state.write().expect("state poisoned");
            state.clients.heartbeat(&params.endpoint_id, params.activity_unix_ms)?;
            let events = state.clients.take_pending(&params.endpoint_id, now_unix_ms())?;
            Ok(json!({"accepted": true, "events": events}))
        }),
        "client.focus" => parse_params::<ClientFocusParams>(request.params).and_then(|params| {
            let focus = match params.focused { Some(true) => FocusState::Focused, Some(false) => FocusState::Unfocused, None => FocusState::Unknown };
            state.write().expect("state poisoned").clients.update_focus(&params.endpoint_id, focus, params.overlay_visible, params.target, now_unix_ms())?;
            Ok(json!({"accepted": true}))
        }),
        "client.accepted" => parse_params::<ClientAcceptedParams>(request.params).map(|params| {
            state.write().expect("state poisoned").semantic_router.accepted(&params.event_id, &params.endpoint_id);
            json!({"accepted": true})
        }),
        "client.detach" => parse_params::<ClientDetachParams>(request.params).and_then(|params| {
            state.write().expect("state poisoned").clients.detach(&params.endpoint_id, now_unix_ms())?;
            Ok(json!({"detached": true}))
        }),
        "config.reload" => Config::load(&paths.config_file())
            .and_then(|config| {
                ManifestSet::load(&paths.manifests_dir())
                    .map(|manifests| (config, manifests))
                    .map_err(|error| ConfigError::Validation(error.to_string()))
            })
            .map(|(config, manifests)| {
                let mut state = state.write().expect("state poisoned");
                let compat_focus = config.clients.selected_implies_focused;
                state.config = config;
                state.manifests = manifests;
                let mut command = std::process::Command::new("tmux");
                command.arg("-S").arg(&server.socket_path);
                if compat_focus { command.args(["set-option", "-gq", "@workbench_selected_implies_focused", "1"]); }
                else { command.args(["set-option", "-gu", "@workbench_selected_implies_focused"]); }
                let _ = command.status();
                json!({"reloaded": true})
            })
            .map_err(|error| error.to_string()),
        "daemon.stop" => {
            shutdown.store(true, Ordering::Relaxed);
            Ok(json!({"stopping": true}))
        }
        "daemon.wake" => {
            state.write().expect("state poisoned").detector.wake();
            Ok(json!({"woken": true}))
        }
        "attention.ack" => parse_params::<AckParams>(request.params).and_then(|params| {
            let mut state = state.write().expect("state poisoned");
            if state.detector.acknowledge(&params.event_id) {
                let agents = state.detector.machine_snapshots();
                state.snapshot.agents = agents;
                state.snapshot.generation = state.snapshot.generation.saturating_add(1);
                *snapshot.write().expect("snapshot poisoned") = state.snapshot.clone();
                Ok(json!({"acknowledged": true}))
            } else {
                Err("attention event not found".into())
            }
        }),
        "attention.next" => serde_json::to_value(
            state
                .read()
                .expect("state poisoned")
                .detector
                .next_attention(),
        )
        .map_err(|error| error.to_string()),
        "agent.explain" => parse_params::<ExplainParams>(request.params).and_then(|params| {
            let state = state.read().expect("state poisoned");
            state
                .detector
                .explain(&params.pane_id, params.show_content, &state.config)
        }),
        "metadata.report" => parse_params::<MetadataReport>(request.params).and_then(|report| {
            state
                .write()
                .expect("state poisoned")
                .detector
                .report_metadata(report, now_unix_ms())?;
            Ok(json!({"accepted": true}))
        }),
        "agent.event.report" => {
            parse_params::<AgentEventReport>(request.params).and_then(|report| {
                let now = now_unix_ms();
                if now.saturating_sub(report.occurred_at_unix_ms) > crate::hooks::SPOOL_TTL_MS {
                    return Err("agent event expired".into());
                }
                if report.occurred_at_unix_ms > now.saturating_add(5_000) {
                    return Err("agent event timestamp is in the future".into());
                }
                let mut state = state.write().expect("state poisoned");
                let agent = state.detector.report_agent_event(&report)?;
                if report.event == crate::model::AgentEventType::SessionStart
                    && report.reason_category.as_deref() != Some("compact")
                {
                    state.notifier.observe_session_start(
                        report.occurred_at_unix_ms,
                        &report.event_id,
                        &agent,
                    );
                }
                if report.event == crate::model::AgentEventType::Error {
                    state.notifier.observe_task_error(
                        report.occurred_at_unix_ms,
                        &report.event_id,
                        &agent,
                    );
                    route_instant_event(&mut state, &report, &agent, now);
                }
                state.snapshot.agents = state.detector.machine_snapshots();
                state.snapshot.generation = state.snapshot.generation.saturating_add(1);
                persist_checkpoint(paths, &server.key, &state);
                *snapshot.write().expect("snapshot poisoned") = state.snapshot.clone();
                Ok(json!({"accepted": true}))
            })
        }
        "agent.event.ingest" => {
            parse_params::<DetachedAgentEventReport>(request.params).and_then(|detached| {
                let now = now_unix_ms();
                if now.saturating_sub(detached.occurred_at_unix_ms) > crate::hooks::SPOOL_TTL_MS {
                    return Err("agent event expired".into());
                }
                if detached.occurred_at_unix_ms > now.saturating_add(5_000) {
                    return Err("agent event timestamp is in the future".into());
                }
                let mut state = state.write().expect("state poisoned");
                let resolved = state.detector.resolve_agent_event(&detached);
                let (report, agent) = match resolved {
                    Ok(resolved) => resolved,
                    Err(_) => {
                        // SessionStart can beat the daemon's periodic process scan
                        // during a fresh or resumed TUI startup. Treat a detached
                        // hook as a discovery signal and retry after one immediate
                        // scan instead of surfacing a spurious hook failure.
                        let config = state.config.clone();
                        let manifests = state.manifests.clone();
                        state.detector.wake();
                        state
                            .detector
                            .tick(&config, &manifests, now)
                            .map_err(|error| error.to_string())?;
                        state.detector.resolve_agent_event(&detached)?
                    }
                };
                if report.event == crate::model::AgentEventType::SessionStart
                    && report.reason_category.as_deref() != Some("compact")
                {
                    state.notifier.observe_session_start(
                        report.occurred_at_unix_ms,
                        &report.event_id,
                        &agent,
                    );
                }
                if report.event == crate::model::AgentEventType::Error {
                    state.notifier.observe_task_error(
                        report.occurred_at_unix_ms,
                        &report.event_id,
                        &agent,
                    );
                    route_instant_event(&mut state, &report, &agent, now);
                }
                state.snapshot.agents = state.detector.machine_snapshots();
                state.snapshot.generation = state.snapshot.generation.saturating_add(1);
                persist_checkpoint(paths, &server.key, &state);
                *snapshot.write().expect("snapshot poisoned") = state.snapshot.clone();
                Ok(json!({"accepted": true, "pane_id": report.pane_id}))
            })
        }
        _ => Err(format!("unknown method: {}", request.method)),
    };
    match result {
        Ok(result) => Response::success(request.id, result),
        Err(error) => Response::error(request.id, "request_failed", error),
    }
}

fn route_instant_event(
    state: &mut State,
    report: &AgentEventReport,
    agent: &crate::model::AgentSnapshot,
    now_ms: u64,
) {
    let category = match report.event {
        crate::model::AgentEventType::Error => crate::semantic::SemanticCategory::TaskError,
        crate::model::AgentEventType::SessionStart => {
            crate::semantic::SemanticCategory::SessionStart
        }
        _ => return,
    };
    let event = crate::semantic::SemanticEvent {
        id: report.event_id.clone(),
        category,
        target: agent.target.clone(),
        created_unix_ms: report.occurred_at_unix_ms,
        deadline_unix_ms: report
            .occurred_at_unix_ms
            .saturating_add(category.horizon_ms()),
        title: format!("Workbench · {}", agent.label),
        body: format!("{} · {}", category.name(), agent.target.session_name),
    };
    match state.semantic_router.route(&event, &state.clients, now_ms) {
        crate::semantic::RouteDecision::Deliver { endpoints, .. } => {
            if let Some(endpoint) = endpoints.first() {
                let _ = state.clients.queue(endpoint, event);
            }
        }
        crate::semantic::RouteDecision::Watched {
            sound_endpoint: Some(endpoint),
            ..
        } => {
            let _ = state.clients.queue(&endpoint, event);
        }
        _ => {}
    }
}

fn parse_params<T: for<'de> Deserialize<'de>>(value: Value) -> Result<T, String> {
    serde_json::from_value(value).map_err(|error| error.to_string())
}

fn open_lock(path: &PathBuf) -> io::Result<File> {
    let file = OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .mode(0o600)
        .open(path)?;
    file.set_permissions(fs::Permissions::from_mode(0o600))?;
    Ok(file)
}

fn remove_stale_socket(path: &PathBuf) -> io::Result<()> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

#[cfg(unix)]
fn set_socket_mode(path: &PathBuf) -> io::Result<()> {
    fs::set_permissions(path, fs::Permissions::from_mode(0o600))
}

fn now_unix_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}
