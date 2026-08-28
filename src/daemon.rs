use std::fs::{self, File, OpenOptions};
use std::io;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::os::unix::net::UnixListener;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, RwLock};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use fs2::FileExt;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use thiserror::Error;

use crate::config::{Config, ConfigError};
use crate::detection::{Detector, MetadataReport};
use crate::ipc::{Request, Response, read_request, write_response};
use crate::manifest::{ManifestError, ManifestSet};
use crate::model::{AgentEventReport, Snapshot};
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
}

struct State {
    config: Config,
    manifests: ManifestSet,
    snapshot: Snapshot,
    detector: Detector,
    notifier: NotificationScheduler,
    notification_backend: SystemBackend,
    relay_sender: RelaySender,
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
    let state = Arc::new(RwLock::new(State {
        config,
        manifests,
        snapshot: Snapshot::empty(server.socket_path.display().to_string(), now_unix_ms()),
        detector: Detector::new(server.clone()),
        notifier: NotificationScheduler::default(),
        notification_backend: SystemBackend::new(paths),
        relay_sender: RelaySender::new(paths, server),
    }));
    let snapshot = Arc::new(RwLock::new(
        state.read().expect("state poisoned").snapshot.clone(),
    ));
    let shutdown = Arc::new(AtomicBool::new(false));

    let outcome = serve_loop(listener, state, snapshot, shutdown, paths, server);
    let _ = fs::remove_file(&socket_path);
    drop(lock);
    outcome
}

fn serve_loop(
    listener: UnixListener,
    state: Arc<RwLock<State>>,
    snapshot: Arc<RwLock<Snapshot>>,
    shutdown: Arc<AtomicBool>,
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
                    // A malformed, timed-out, or already-disconnected UI client is
                    // connection-local. It must never tear down the server-wide
                    // daemon (macOS can surface EINVAL here for a closing socket).
                    let _ = stream.set_read_timeout(Some(Duration::from_secs(2)));
                    let _ = stream.set_write_timeout(Some(Duration::from_secs(2)));
                    let response = match read_request(&stream) {
                        Ok(request) => handle(request, &state, &snapshot, &shutdown, paths, server),
                        Err(error) => {
                            Response::error(String::new(), "invalid_request", error.to_string())
                        }
                    };
                    let _ = write_response(&stream, &response);
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
        if changed {
            state.snapshot.generation = state.snapshot.generation.saturating_add(1);
        }
        state.snapshot.observed_at_unix_ms = now;
        let agents = state.snapshot.agents.clone();
        state.notifier.observe(now, &agents);
        let mut notifier = std::mem::take(&mut state.notifier);
        let delivered =
            notifier.deliver_due(now, &agents, &config, &mut state.notification_backend);
        state.notifier = notifier;
        state.relay_sender.enqueue(&delivered, now);
        *published.write().expect("snapshot poisoned") = state.snapshot.clone();
    }
    state.relay_sender.tick(now);
}

fn handle(
    request: Request,
    state: &RwLock<State>,
    snapshot: &RwLock<Snapshot>,
    shutdown: &AtomicBool,
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
            })
            .map_err(|error| error.to_string())
        }
        "snapshot.get" => serde_json::to_value(&*snapshot.read().expect("snapshot poisoned"))
            .map_err(|error| error.to_string()),
        "config.reload" => Config::load(&paths.config_file())
            .and_then(|config| {
                ManifestSet::load(&paths.manifests_dir())
                    .map(|manifests| (config, manifests))
                    .map_err(|error| ConfigError::Validation(error.to_string()))
            })
            .map(|(config, manifests)| {
                let mut state = state.write().expect("state poisoned");
                state.config = config;
                state.manifests = manifests;
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
                }
                state.snapshot.agents = state.detector.machine_snapshots();
                state.snapshot.generation = state.snapshot.generation.saturating_add(1);
                *snapshot.write().expect("snapshot poisoned") = state.snapshot.clone();
                Ok(json!({"accepted": true}))
            })
        }
        _ => Err(format!("unknown method: {}", request.method)),
    };
    match result {
        Ok(result) => Response::success(request.id, result),
        Err(error) => Response::error(request.id, "request_failed", error),
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
