use std::collections::{HashMap, HashSet};
use std::fs::{self, File, OpenOptions};
use std::io;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::os::unix::net::UnixListener;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, RwLock, RwLockWriteGuard, mpsc};
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
use crate::model::{AgentEventReport, DetachedAgentEventReport, Snapshot, TmuxTarget};
use crate::notification::SystemBackend;
use crate::notification_pipeline::NotificationPipeline;
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
    pub runtime_revision: u32,
    pub protocol_version: u32,
    pub server: String,
    pub pid: u32,
    pub generation: u64,
    pub ipc_in_flight: u64,
    pub ipc_peak_in_flight: u64,
    pub ipc_accepted: u64,
    pub ipc_slow: u64,
    pub detector_recoveries: u64,
}

#[derive(Default)]
struct IpcMetrics {
    in_flight: AtomicU64,
    peak_in_flight: AtomicU64,
    accepted: AtomicU64,
    slow: AtomicU64,
    detector_recoveries: AtomicU64,
}

struct State {
    config: Config,
    manifests: ManifestSet,
    snapshot: Snapshot,
    detector: Detector,
    notifications: NotificationPipeline,
    notification_backend: SystemBackend,
    relay_sender: RelaySender,
    clients: ClientRegistry,
    client_actions: crate::client_actions::Actions,
    server_incarnation: String,
    recovered_pending: Vec<crate::semantic::SemanticEvent>,
    recovered_runtimes: Vec<crate::checkpoint::RuntimeCheckpoint>,
    attached_focus: AttachedFocus,
    status_projection: Option<Vec<(String, crate::model::DisplayState, bool)>>,
    sidebar_sender: mpsc::SyncSender<SidebarRequest>,
    shared_surface: Arc<SharedSurfaceCache>,
    next_sidebar_surface: Instant,
    last_sidebar_sample: Option<Instant>,
}

struct SidebarRequest {
    snapshot: Snapshot,
    surface: crate::tmux::SurfaceInventory,
    sampled_at: Instant,
}

struct CachedSharedSurface {
    snapshot: Snapshot,
    surface: crate::tmux::SurfaceInventory,
    sampled_at: Instant,
}

#[derive(Default)]
struct SharedSurfaceCache {
    value: RwLock<Option<CachedSharedSurface>>,
    refresh_requested: AtomicBool,
}

fn start_sidebar_worker(
    paths: &Paths,
    server: &ServerIdentity,
) -> (mpsc::SyncSender<SidebarRequest>, Arc<SharedSurfaceCache>) {
    let (sender, receiver) = mpsc::sync_channel::<SidebarRequest>(1);
    let shared = Arc::new(SharedSurfaceCache::default());
    let output = Arc::clone(&shared);
    let paths = paths.clone();
    let server = server.clone();
    thread::spawn(move || {
        let mut inventory_cache = None;
        while let Ok(request) = receiver.recv() {
            let mut surface = request.surface;
            surface.sidebar = crate::sidebar::shared_sidebar_data(
                &paths,
                &server,
                &request.snapshot,
                &mut inventory_cache,
                &surface,
            );
            if surface.sidebar.is_some() && request.sampled_at.elapsed() < Duration::from_secs(3) {
                *output.value.write().expect("sidebar cache poisoned") =
                    Some(CachedSharedSurface {
                        snapshot: request.snapshot,
                        surface,
                        sampled_at: request.sampled_at,
                    });
            }
        }
    });
    (sender, shared)
}

fn cached_sidebar_surface(
    cache: &SharedSurfaceCache,
) -> Option<(Snapshot, crate::tmux::SurfaceInventory)> {
    cache
        .value
        .read()
        .expect("sidebar cache poisoned")
        .as_ref()
        .filter(|cached| cached.sampled_at.elapsed() < Duration::from_secs(3))
        .map(|cached| (cached.snapshot.clone(), cached.surface.clone()))
}

#[derive(Default)]
struct AttachedFocus {
    last_sync: Option<Instant>,
    remembered: HashMap<String, (TmuxTarget, Vec<String>)>,
}

impl AttachedFocus {
    fn due(&mut self, now: Instant, interval: Duration) -> bool {
        if self
            .last_sync
            .is_some_and(|last| now.duration_since(last) < interval)
        {
            return false;
        }
        self.last_sync = Some(now);
        true
    }

    fn invalidate(&mut self, attachment: &str) {
        self.remembered.remove(attachment);
        self.last_sync = None;
    }
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
    #[serde(default)]
    terminal_id: Option<String>,
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
    // Client and daemon clocks may differ. Presence is measured locally.
    #[serde(rename = "activity_unix_ms")]
    _activity_unix_ms: u64,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ClientRecoverParams {
    token: String,
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

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ClientAttachmentParams {
    endpoint_id: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct OpenUrlParams {
    attachment: String,
    url: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct OpenUrlStatusParams {
    attachment: String,
    request_id: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct OpenUrlResultParams {
    endpoint_id: String,
    request_id: String,
    error: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ClientRememberParams {
    attachment: String,
    pane: String,
    session: String,
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
    let (sidebar_sender, shared_surface) = start_sidebar_worker(paths, server);
    let interactive_waiters = Arc::new(AtomicUsize::new(0));
    let mut detector = Detector::new(server.clone());
    detector.set_interactive_waiters(Arc::clone(&interactive_waiters));
    let state = Arc::new(RwLock::new(State {
        config,
        manifests,
        snapshot: Snapshot::empty(server.socket_path.display().to_string(), now_unix_ms()),
        detector,
        notifications: NotificationPipeline::new(semantic_router),
        notification_backend: SystemBackend::new(paths),
        relay_sender: RelaySender::new(paths, server),
        clients: ClientRegistry::default(),
        client_actions: crate::client_actions::Actions::default(),
        server_incarnation,
        recovered_pending,
        recovered_runtimes,
        attached_focus: AttachedFocus::default(),
        status_projection: None,
        sidebar_sender,
        shared_surface,
        next_sidebar_surface: Instant::now(),
        last_sidebar_sample: None,
    }));
    if state
        .read()
        .expect("state poisoned")
        .config
        .detection
        .low_process_churn
    {
        configure_focus_wake_hooks(server, true);
    }
    if state
        .read()
        .expect("state poisoned")
        .config
        .clients
        .selected_implies_focused
    {
        let _ = crate::command::new("tmux")
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
        interactive_waiters,
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
    interactive_waiters: Arc<AtomicUsize>,
    paths: &Paths,
    server: &ServerIdentity,
) -> Result<(), DaemonError> {
    let scan_state = Arc::clone(&state);
    let scan_snapshot = Arc::clone(&snapshot);
    let scan_shutdown = Arc::clone(&shutdown);
    let scan_interactive_waiters = Arc::clone(&interactive_waiters);
    let sidebar_cache = Arc::clone(&state.read().expect("state poisoned").shared_surface);
    let scan_sidebar_cache = Arc::clone(&sidebar_cache);
    let scan_server = server.clone();
    let scan_paths = paths.clone();
    let status_sender = crate::status_metrics::start(paths, server);
    let _scanner = thread::spawn(move || {
        let tmux = Tmux::new(scan_server.clone());
        let mut title_reaper = crate::task_title::TitleReaper::default();
        let mut last_status_settings = None;
        let mut next_liveness_check = Instant::now();
        while !scan_shutdown.load(Ordering::Relaxed) && scan_server.socket_path.exists() {
            if Instant::now() >= next_liveness_check {
                if !tmux.server_alive() {
                    scan_shutdown.store(true, Ordering::Relaxed);
                    break;
                }
                let state = scan_state.read().expect("state poisoned");
                let quiet = state.config.detection.low_process_churn && state.detector.is_idle();
                next_liveness_check = Instant::now()
                    + if quiet {
                        Duration::from_secs(30)
                    } else {
                        Duration::from_secs(1)
                    };
            }
            update_snapshot(
                &scan_state,
                &scan_snapshot,
                &scan_paths,
                &scan_server,
                &scan_sidebar_cache,
                &scan_interactive_waiters,
            );
            let snapshot = scan_snapshot.read().expect("snapshot poisoned").clone();
            let (quiet, low_churn, attached, surface) = {
                let state = scan_state.read().expect("state poisoned");
                let quiet = state.config.detection.low_process_churn && state.detector.is_idle();
                (
                    quiet,
                    state.config.detection.low_process_churn,
                    state
                        .detector
                        .client_inventory()
                        .is_some_and(|clients| !clients.trim().is_empty()),
                    state.detector.surface_inventory().cloned(),
                )
            };
            if let Some(surface) = &surface {
                let settings = crate::status_metrics::Settings::from_surface(
                    surface, low_churn, quiet, attached,
                );
                if last_status_settings.as_ref() != Some(&settings)
                    && status_sender.try_send(settings.clone()).is_ok()
                {
                    last_status_settings = Some(settings);
                }
            }
            title_reaper.tick(
                &scan_paths,
                &scan_server,
                &snapshot,
                quiet,
                surface.as_ref(),
            );
            crate::compat::sleep(Duration::from_millis(50));
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
                    let interactive_waiters = Arc::clone(&interactive_waiters);
                    let sidebar_cache = Arc::clone(&sidebar_cache);
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
                        let mut stream = stream;
                        let response = match crate::ipc::prepare_connection(&mut stream) {
                            Ok(mut request_stream) => {
                                Some(match read_request(&mut request_stream) {
                                    Ok(request) => handle(
                                        request,
                                        &state,
                                        &snapshot,
                                        &shutdown,
                                        &metrics,
                                        &interactive_waiters,
                                        &paths,
                                        &server,
                                        &sidebar_cache,
                                    ),
                                    Err(error) => Response::error(
                                        String::new(),
                                        "invalid_request",
                                        error.to_string(),
                                    ),
                                })
                            }
                            Err(_) => None,
                        };
                        if let Some(response) = response
                            && let Ok(mut response_stream) =
                                crate::ipc::prepare_response_connection(&mut stream)
                        {
                            let _ = write_response(&mut response_stream, &response);
                        }
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
        crate::compat::sleep(Duration::from_millis(50));
    }
    shutdown.store(true, Ordering::Relaxed);
    configure_focus_wake_hooks(server, false);
    Ok(())
}

// Install only into unused array slots. The marker lets reload and shutdown
// remove our own hooks without touching a user's or another plugin's hooks.
fn configure_focus_wake_hooks(server: &ServerIdentity, enabled: bool) {
    const TAG: &str = "workbench-low-churn-focus-wake";
    const HOOKS: &[&str] = &[
        "client-attached",
        "client-detached",
        "client-session-changed",
        "session-window-changed",
        "window-pane-changed",
        "client-focus-in",
        "client-focus-out",
    ];
    let Ok(existing) = crate::command::new("tmux")
        .arg("-S")
        .arg(&server.socket_path)
        .args(["show-hooks", "-g"])
        .output()
    else {
        return;
    };
    if !existing.status.success() {
        return;
    }
    let existing = String::from_utf8_lossy(&existing.stdout);
    let Ok(executable) = std::env::current_exe() else {
        return;
    };
    let shell = format!(
        "{} daemon wake-if-running >/dev/null 2>&1 # {TAG}",
        crate::status_menu::shell_quote(&executable.to_string_lossy())
    );
    let hook_command = format!("run-shell -b {}", crate::status_menu::shell_quote(&shell));
    let mut args = Vec::<String>::new();
    for hook in HOOKS {
        let slot = format!("{hook}[921]");
        let found = existing
            .lines()
            .find(|line| line.split_whitespace().next() == Some(slot.as_str()));
        if enabled && found.is_none_or(|line| line.contains(TAG)) {
            if !args.is_empty() {
                args.push(";".into());
            }
            args.extend(["set-hook".into(), "-g".into(), slot, hook_command.clone()]);
        } else if !enabled && found.is_some_and(|line| line.contains(TAG)) {
            if !args.is_empty() {
                args.push(";".into());
            }
            args.extend(["set-hook".into(), "-gu".into(), slot]);
        }
    }
    if !args.is_empty() {
        let _ = crate::command::new("tmux")
            .arg("-S")
            .arg(&server.socket_path)
            .args(&args)
            .output();
    }
}

fn expected_detached_hook_rejection(error: &str) -> bool {
    matches!(
        error,
        "event is not for the active foreground session"
            | "out-of-order event"
            | "no matching live agent pane"
            | "ambiguous live agent panes; refusing to guess"
    )
}

fn update_snapshot(
    state: &RwLock<State>,
    published: &RwLock<Snapshot>,
    paths: &Paths,
    server: &ServerIdentity,
    sidebar_cache: &SharedSurfaceCache,
    interactive_waiters: &AtomicUsize,
) {
    let server_key = server.key.as_str();
    let mut state = state.write().expect("state poisoned");
    if interactive_waiters.load(Ordering::SeqCst) != 0 {
        return;
    }
    if sidebar_cache
        .refresh_requested
        .swap(false, Ordering::Relaxed)
    {
        state.detector.request_surface_refresh();
    }
    let config = state.config.clone();
    let manifests = state.manifests.clone();
    let now = now_unix_ms();
    state.detector.set_sampling_interruptible(true);
    let sampled = state.detector.tick(&config, &manifests, now);
    state.detector.set_sampling_interruptible(false);
    if interactive_waiters.load(Ordering::SeqCst) != 0 {
        return;
    }
    if sampled.is_ok() {
        if !state.recovered_runtimes.is_empty() {
            let checkpoints = std::mem::take(&mut state.recovered_runtimes);
            state.recovered_runtimes = state.detector.restore_checkpoints(&checkpoints, now);
        }
        for report in crate::hooks::drain_spool(paths, server_key, now) {
            if let Ok(agent) = state.detector.report_agent_event(&report) {
                if report.event == crate::model::AgentEventType::SessionStart
                    && report.reason_category.as_deref() != Some("compact")
                {
                    state.notifications.scheduler.observe_session_start(
                        report.occurred_at_unix_ms,
                        &report.event_id,
                        &agent,
                    );
                }
                if report.event == crate::model::AgentEventType::Error {
                    state.notifications.scheduler.observe_task_error(
                        report.occurred_at_unix_ms,
                        &report.event_id,
                        &agent,
                    );
                }
            }
        }
        for report in crate::hooks::drain_detached_spool(paths, server_key, now) {
            match state.detector.resolve_agent_event(&report) {
                Ok((report, agent)) => {
                    if report.event == crate::model::AgentEventType::SessionStart
                        && report.reason_category.as_deref() != Some("compact")
                    {
                        state.notifications.scheduler.observe_session_start(
                            report.occurred_at_unix_ms,
                            &report.event_id,
                            &agent,
                        );
                    }
                    if report.event == crate::model::AgentEventType::Error {
                        state.notifications.scheduler.observe_task_error(
                            report.occurred_at_unix_ms,
                            &report.event_id,
                            &agent,
                        );
                    }
                }
                Err(error) if !expected_detached_hook_rejection(&error) => {
                    eprintln!("tmux-agent-workbench: queued detached hook rejected: {error}")
                }
                // Detached hooks are best-effort. Late events and hooks that
                // no longer identify one live foreground pane are normal
                // drops, not daemon errors; logging every one floods stderr.
                Err(_) => {}
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
            let projection = status_projection(&state.snapshot);
            if state.status_projection.as_ref() != Some(&projection) {
                publish_status_fragments(&state.snapshot);
                state.status_projection = Some(projection);
            }
        }
        state.snapshot.observed_at_unix_ms = now;
        let accepted_before = state.notifications.router.accepted_event_ids().len();
        restore_notifications(&mut state, now);
        dispatch_notifications(&mut state, now);
        if changed || state.notifications.router.accepted_event_ids().len() != accepted_before {
            persist_checkpoint(paths, server_key, &state);
        }
        *published.write().expect("snapshot poisoned") = state.snapshot.clone();
        if Instant::now() >= state.next_sidebar_surface {
            if let (Some(surface), Some(sampled_at)) = (
                state.detector.surface_inventory().cloned(),
                state.detector.surface_sampled_at(),
            ) && state
                .last_sidebar_sample
                .is_none_or(|previous| sampled_at > previous)
            {
                // Filesystem reads run in one bounded worker, outside the
                // detector lock. Keep this snapshot paired with the inventory
                // it describes, even if a later tmux query fails.
                if state
                    .sidebar_sender
                    .try_send(SidebarRequest {
                        snapshot: state.snapshot.clone(),
                        surface,
                        sampled_at,
                    })
                    .is_ok()
                {
                    state.last_sidebar_sample = Some(sampled_at);
                    state.next_sidebar_surface = Instant::now() + Duration::from_secs(1);
                }
            }
        }
    }
    state.relay_sender.tick(now);
}

fn restore_notifications(state: &mut State, now_ms: u64) {
    let mut unresolved = Vec::new();
    for event in std::mem::take(&mut state.recovered_pending) {
        if now_ms > event.deadline_unix_ms {
            continue;
        }
        if let Some(agent) = state
            .snapshot
            .agents
            .iter()
            .find(|a| a.target.pane_id == event.target.pane_id)
        {
            state.notifications.scheduler.restore(event, agent, now_ms);
        } else {
            unresolved.push(event);
        }
    }
    state.recovered_pending = unresolved;
}

fn refresh_notifications(state: &mut State, now_ms: u64) {
    let agents = state.detector.machine_snapshots();
    state
        .notifications
        .refresh(now_ms, &agents, &mut state.clients, &mut state.relay_sender);
}

fn dispatch_notifications(state: &mut State, now_ms: u64) {
    let agents = state.detector.machine_snapshots();
    let seen = state.notifications.dispatch(
        now_ms,
        &agents,
        &state.config,
        crate::notification_pipeline::DeliveryTargets {
            clients: &mut state.clients,
            relay: &mut state.relay_sender,
            local: &mut state.notification_backend,
        },
    );
    for id in seen {
        let _ = state.detector.acknowledge(&id, now_ms);
    }
}

fn sync_attached_client_focus(state: &mut State, now_ms: u64) {
    let quiet = state.config.detection.low_process_churn && state.detector.is_idle();
    let interval = if quiet {
        Duration::from_millis(state.config.detection.idle_capture_interval_ms.max(2_000))
    } else {
        Duration::from_millis(250)
    };
    if !state.attached_focus.due(Instant::now(), interval) {
        return;
    }
    let server = match ServerIdentity::discover() {
        Ok(server) => server,
        Err(_) => return,
    };
    let Some(output) = state.detector.client_inventory().map(str::to_owned) else {
        return;
    };
    let mut seen = HashSet::new();
    for line in output.lines() {
        let Some((attachment, focus, overlay_visible, observed)) = attached_client_row(line) else {
            continue;
        };
        seen.insert(attachment.clone());
        let mut resume_keys = state.clients.resume_keys_for_attachment(&attachment);
        resume_keys.sort();
        resume_keys.dedup();
        // One client inventory carries the complete session-relative target.
        // Stable attachments need no per-pane queries or option reads/writes.
        let changed = state
            .attached_focus
            .remembered
            .get(&attachment)
            .is_none_or(|(target, keys)| target != &observed || keys != &resume_keys);
        if changed {
            let mut remembered = crate::attachment::remember(
                &server,
                &crate::attachment::attachment_key(&attachment),
                &observed,
            );
            for key in &resume_keys {
                remembered &= crate::attachment::remember(&server, key, &observed);
            }
            if remembered {
                state
                    .attached_focus
                    .remembered
                    .insert(attachment.clone(), (observed.clone(), resume_keys.clone()));
            }
        }
        let target = if resume_keys.is_empty() {
            state
                .snapshot
                .agents
                .iter()
                .find(|agent| agent.target.pane_id == observed.pane_id)
                .map(|agent| agent.target.clone())
        } else {
            Some(observed)
        };
        state
            .clients
            .update_attachment_focus(&attachment, focus, overlay_visible, target, now_ms);
    }
    state
        .attached_focus
        .remembered
        .retain(|attachment, _| seen.contains(attachment));
}

fn attached_client_row(line: &str) -> Option<(String, FocusState, bool, TmuxTarget)> {
    let fields: Vec<_> = line.split('\u{1f}').collect();
    if fields.len() != 10
        || fields[0].is_empty()
        || !crate::attachment::valid_id(fields[3], '$')
        || !crate::attachment::valid_id(fields[5], '@')
        || !crate::attachment::valid_id(fields[8], '%')
    {
        return None;
    }
    let focus = if fields[1].split(',').any(|flag| flag == "focused") {
        FocusState::Focused
    } else {
        FocusState::Unknown
    };
    Some((
        fields[0].into(),
        focus,
        fields[2] == "1",
        TmuxTarget {
            session_id: fields[3].into(),
            session_name: fields[4].into(),
            window_id: fields[5].into(),
            window_index: fields[6].parse().ok()?,
            window_name: fields[7].into(),
            pane_id: fields[8].into(),
            pane_index: fields[9].parse().ok()?,
        },
    ))
}

fn server_incarnation(server: &ServerIdentity) -> String {
    let output = crate::command::new("tmux")
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
    static LAST_CHECKPOINT_FAILURE_LOG: std::sync::Mutex<Option<Instant>> =
        std::sync::Mutex::new(None);
    match save_checkpoint(paths, server_key, state) {
        Ok(()) => {
            *LAST_CHECKPOINT_FAILURE_LOG
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner()) = None;
        }
        Err(error) => {
            let mut last = LAST_CHECKPOINT_FAILURE_LOG
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            if last
                .as_ref()
                .is_none_or(|reported| reported.elapsed() >= Duration::from_secs(60))
            {
                *last = Some(Instant::now());
                eprintln!(
                    "tmux-agent-workbench: checkpoint write failed (repeated failures are logged once per minute): {error}"
                );
            }
        }
    }
}

fn save_checkpoint(paths: &Paths, server_key: &str, state: &State) -> io::Result<()> {
    let mut pending = state.notifications.scheduler.pending_events();
    pending.extend(state.recovered_pending.clone());
    let delivered_event_ids = state.notifications.router.accepted_event_ids();
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
            let runtime = state
                .detector
                .checkpoint_metadata(&agent.instance_id)
                .unwrap_or_else(|| crate::state_machine::RuntimeMetadata {
                    runtime_id: agent.instance_id.clone(),
                    ..Default::default()
                });
            Some(crate::checkpoint::RuntimeCheckpoint {
                version: 1,
                server_incarnation: state.server_incarnation.clone(),
                runtime_id: runtime.runtime_id,
                process_fingerprint: format!(
                    "{}:{}:{}",
                    process.pid, process.started_at_ticks, process.executable
                ),
                previous_state: format!("{:?}", agent.base_state).to_lowercase(),
                state_entered_at_unix_ms: runtime.state_entered_at_unix_ms,
                state_source: Some(agent.state_source),
                last_hook_event_at_ms: state.detector.hook_replay_guard(&agent.instance_id).0,
                hook_event_ids: state.detector.hook_replay_guard(&agent.instance_id).1,
                attention_seq: runtime.attention_seq,
                seen_seq: runtime.seen_seq,
                hook_session_id: runtime.hook_session_id,
                visible_session_id: runtime.visible_session_id,
                previous_session_id: runtime.previous_session_id,
                thread_name: agent.thread_name.clone(),
                name_from_command: runtime.name_from_command,
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
    crate::checkpoint::store_server(&paths.checkpoint_for_server(server_key), &checkpoint)
}

fn status_projection(snapshot: &Snapshot) -> Vec<(String, crate::model::DisplayState, bool)> {
    snapshot
        .agents
        .iter()
        .map(|agent| {
            (
                agent.target.window_id.clone(),
                agent.display_state,
                agent.attention.as_ref().is_some_and(|event| !event.seen),
            )
        })
        .collect()
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
    let mut command = crate::command::new("tmux");
    command.arg("-S").arg(&server.socket_path);
    for (index, (name, value)) in fragments.into_iter().enumerate() {
        if index > 0 {
            command.arg(";");
        }
        command.args(["set-option", "-gq", name, &value]);
    }
    let _ = command.status();
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

    let output = match crate::command::new("tmux")
        .arg("-S")
        .arg(&server.socket_path)
        .args(["list-windows", "-a", "-F", "#{window_id}"])
        .output()
    {
        Ok(output) if output.status.success() => output,
        _ => return,
    };
    let windows = String::from_utf8_lossy(&output.stdout);
    let mut command = crate::command::new("tmux");
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

fn interactive_state<'a>(
    state: &'a RwLock<State>,
    waiters: &AtomicUsize,
) -> RwLockWriteGuard<'a, State> {
    struct Waiting<'a>(&'a AtomicUsize);
    impl Drop for Waiting<'_> {
        fn drop(&mut self) {
            self.0.fetch_sub(1, Ordering::SeqCst);
        }
    }
    waiters.fetch_add(1, Ordering::SeqCst);
    let waiting = Waiting(waiters);
    let state = state.write().expect("state poisoned");
    drop(waiting);
    state
}

fn handle(
    request: Request,
    state: &RwLock<State>,
    snapshot: &RwLock<Snapshot>,
    shutdown: &AtomicBool,
    ipc_metrics: &IpcMetrics,
    interactive_waiters: &AtomicUsize,
    paths: &Paths,
    server: &ServerIdentity,
    sidebar_cache: &SharedSurfaceCache,
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
                runtime_revision: crate::DAEMON_RUNTIME_REVISION,
                protocol_version: crate::IPC_PROTOCOL_VERSION,
                server: server.socket_path.display().to_string(),
                pid: std::process::id(),
                generation,
                ipc_in_flight: ipc_metrics.in_flight.load(Ordering::Relaxed),
                ipc_peak_in_flight: ipc_metrics.peak_in_flight.load(Ordering::Relaxed),
                ipc_accepted: ipc_metrics.accepted.load(Ordering::Relaxed),
                ipc_slow: ipc_metrics.slow.load(Ordering::Relaxed),
                detector_recoveries: ipc_metrics.detector_recoveries.load(Ordering::Relaxed),
            })
            .map_err(|error| error.to_string())
        }
        "snapshot.get" => (|| -> Result<Value, String> {
            let cached = if request.params.get("include_surface").and_then(Value::as_bool) == Some(true) {
                if request.params.get("refresh_surface").and_then(Value::as_bool) == Some(true) {
                    // Coalesce requests without making any sidebar wait for
                    // process discovery or another tmux query to finish.
                    sidebar_cache.refresh_requested.store(true, Ordering::Relaxed);
                }
                cached_sidebar_surface(sidebar_cache)
            } else { None };
            if let Some((cached_snapshot, surface)) = cached {
                let mut result = serde_json::to_value(cached_snapshot).map_err(|error| error.to_string())?;
                result["_workbench_surface"] = serde_json::to_value(surface).map_err(|error| error.to_string())?;
                Ok(result)
            } else {
                serde_json::to_value(&*snapshot.read().expect("snapshot poisoned"))
                    .map_err(|error| error.to_string())
            }
        })(),
        "task.title" => parse_params::<crate::task_title::TitleRequest>(request.params).and_then(|params| {
            let mut state = interactive_state(state, interactive_waiters);
            state.detector.refresh_targets().map_err(|error| error.to_string())?;
            let mut current = state.snapshot.clone();
            current.agents = state.detector.machine_snapshots();
            current.sessions = state.detector.sessions();
            let result = crate::task_title::apply(paths, &current, &params, &state.config.auto_split);
            // Publish the actual layout even if a later naming/verification step
            // failed. No consumer should retain a pre-move target.
            let refreshed = state.detector.refresh_targets().map_err(|error| error.to_string());
            if let Ok(result) = &result {
                if result.get("status").and_then(Value::as_str) == Some("renamed") {
                    let name = result.get("title").and_then(Value::as_str)
                    .ok_or("name update did not return its verified title")?.to_owned();
                    state.detector.set_task_name(&params.instance_id, name)
                    .ok_or("named agent disappeared")?;
                }
            }
            let agents = state.detector.machine_snapshots();
            let sessions = state.detector.sessions();
            if state.snapshot.agents != agents || state.snapshot.sessions != sessions
                || (result.is_ok() && (params.title.is_some() || params.tmux_session.is_some())) {
                state.snapshot.agents = agents;
                state.snapshot.sessions = sessions;
                state.snapshot.generation = state.snapshot.generation.saturating_add(1);
                state.snapshot.observed_at_unix_ms = now_unix_ms();
                let saved = save_checkpoint(paths, &server.key, &state);
                publish_status_fragments(&state.snapshot);
                *snapshot.write().expect("snapshot poisoned") = state.snapshot.clone();
                saved.map_err(|error| format!("task name updated but could not save recovery state: {error}"))?;
            }
            refreshed?;
            result
        }),
        "client.register" => parse_params::<ClientRegisterParams>(request.params).and_then(|params| {
            uuid::Uuid::parse_str(&params.device_id).map_err(|_| "invalid device id")?;
            if let Some(id) = &params.terminal_id { uuid::Uuid::parse_str(id).map_err(|_| "invalid terminal id")?; }
            let (endpoint_id, attachment_token, replaced_attachments) = {
                let mut state = state.write().expect("state poisoned");
                let endpoint_id = uuid::Uuid::new_v4().to_string();
                let attachment_token = uuid::Uuid::new_v4().to_string();
                // Fence the old channel before changing or detaching its state.
                crate::client_lease::mark_owner(paths, &server.key, &params.device_id, params.terminal_id.as_deref(), &endpoint_id).map_err(|error| format!("cannot record client owner: {error}"))?;
                let replaced_attachments = state.clients.replace_terminal(&params.device_id, params.terminal_id.as_deref());
                state.clients.register_terminal_with_ids(params.device_id, params.terminal_id, params.device_label, params.kind, params.capabilities, now_unix_ms(), endpoint_id.clone(), attachment_token.clone());
                (endpoint_id, attachment_token, replaced_attachments)
            };
            for attachment in replaced_attachments {
                let _ = crate::command::new("tmux")
                    .arg("-S")
                    .arg(&server.socket_path)
                    .args(["detach-client", "-t", &attachment])
                    .stderr(std::process::Stdio::null())
                    .status();
            }
            Ok(json!({"endpoint_id": endpoint_id, "attachment_token": attachment_token, "heartbeat_seconds": 15}))
        }),
        "client.recover" => parse_params::<ClientRecoverParams>(request.params).and_then(|params| {
            let mut state = state.write().expect("state poisoned");
            let (record, attachment) = crate::client_lease::recover(paths, &server.key, &params.token)
                .map_err(|error| format!("client recovery unavailable: {error}"))?
                .ok_or("client control is no longer active")?;
            let now = now_unix_ms();
            if attachment.is_none() && now.saturating_sub(record.registered_at_ms) > crate::client::ATTACHMENT_TOKEN_TTL_MS {
                return Err("client attachment is no longer active".into());
            }
            let endpoint_id = record.endpoint_id.clone();
            state.clients.recover(&record, attachment, now);
            Ok(json!({"endpoint_id": endpoint_id, "recovered": true}))
        }),
        "client.bind" => parse_params::<ClientBindParams>(request.params).and_then(|params| {
            let mut state = state.write().expect("state poisoned");
            state.attached_focus.invalidate(&params.attachment);
            crate::attachment::forget(server, &crate::attachment::attachment_key(&params.attachment));
            let endpoint_id = state.clients.bind(&params.token, params.attachment, now_unix_ms())?;
            let resume_target = state.clients.resume_key(&endpoint_id)
                .and_then(|key| crate::attachment::recalled(server, &key));
            Ok(json!({"endpoint_id": endpoint_id, "bound": true, "resume_target": resume_target}))
        }),
        "client.remember" => parse_params::<ClientRememberParams>(request.params).and_then(|params| {
            // A client can detach without an associated session/pane (or after
            // that target has closed). This is a lifecycle no-op, not a failed
            // command: preserve existing history and never infer another pane.
            // tmux 3.7 detaches the client before expanding hook_pane and
            // hook_session. Use only that attachment's last live observation;
            // the server's implicit/default pane may belong to another client.
            let key = crate::attachment::attachment_key(&params.attachment);
            let target = crate::attachment::pane_target_in_session(server, &params.pane, &params.session)
                .or_else(|| {
                    (params.pane.is_empty() && params.session.is_empty())
                        .then(|| crate::attachment::recalled(server, &key))
                        .flatten()
                        .and_then(|target| crate::attachment::pane_target_in_session(server, &target.pane_id, &target.session_id))
                });
            crate::attachment::forget(server, &key);
            let mut state = state.write().expect("state poisoned");
            state.attached_focus.invalidate(&params.attachment);
            let Some(target) = target else {
                return Ok(json!({"remembered": false, "reason": "detach_target_unavailable"}));
            };
            crate::attachment::remember(server, crate::attachment::LAST, &target);
            for key in state.clients.resume_keys_for_attachment(&params.attachment) {
                crate::attachment::remember(server, &key, &target);
            }
            Ok(json!({"remembered": true}))
        }),
        "client.attachment" => parse_params::<ClientAttachmentParams>(request.params).and_then(|params| {
            let state = state.read().expect("state poisoned");
            let attachment = state.clients.attachment(&params.endpoint_id)?;
            Ok(json!({"attachment": attachment}))
        }),
        "client.heartbeat" => parse_params::<ClientHeartbeatParams>(request.params).and_then(|params| {
            let mut state = state.write().expect("state poisoned");
            state.clients.heartbeat(&params.endpoint_id, now_unix_ms())?;
            refresh_notifications(&mut state, now_unix_ms());
            let events = state.clients.take_pending(&params.endpoint_id, now_unix_ms())?;
            let attachment = state.clients.attachment(&params.endpoint_id)?.map(str::to_owned);
            let open_urls = state.client_actions.take(&params.endpoint_id, attachment.as_deref(), now_unix_ms());
            Ok(json!({"accepted": true, "events": events, "open_urls": open_urls}))
        }),
        "client.open_url" => parse_params::<OpenUrlParams>(request.params).and_then(|params| {
            let mut state = state.write().expect("state poisoned");
            let endpoint = state.clients.opener_for_attachment(&params.attachment, now_unix_ms())?;
            let id = state.client_actions.enqueue(&endpoint, &params.attachment, params.url, now_unix_ms())?;
            Ok(json!({"request_id": id}))
        }),
        "client.open_url_status" => parse_params::<OpenUrlStatusParams>(request.params).and_then(|params| {
            let state = state.read().expect("state poisoned");
            serde_json::to_value(state.client_actions.status(&params.attachment, &params.request_id, now_unix_ms())?)
                .map_err(|error| error.to_string())
        }),
        "client.open_url_result" => parse_params::<OpenUrlResultParams>(request.params).and_then(|params| {
            state.write().expect("state poisoned").client_actions.complete(&params.endpoint_id, &params.request_id, params.error, now_unix_ms())?;
            Ok(json!({"accepted": true}))
        }),
        "client.focus" => parse_params::<ClientFocusParams>(request.params).and_then(|params| {
            let focus = match params.focused { Some(true) => FocusState::Focused, Some(false) => FocusState::Unfocused, None => FocusState::Unknown };
            state.write().expect("state poisoned").clients.update_focus(&params.endpoint_id, focus, params.overlay_visible, params.target, now_unix_ms())?;
            Ok(json!({"accepted": true}))
        }),
        "client.accepted" => parse_params::<ClientAcceptedParams>(request.params).map(|params| {
            let mut state = state.write().expect("state poisoned");
            state.notifications.router.accepted(&params.event_id, &params.endpoint_id);
            persist_checkpoint(paths, &server.key, &state);
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
                let low_churn = config.detection.low_process_churn;
                let changed = low_churn != state.config.detection.low_process_churn;
                state.config = config;
                state.manifests = manifests;
                if changed {
                    state.detector.wake();
                    state.attached_focus.last_sync = None;
                }
                drop(state);
                if changed { configure_focus_wake_hooks(server, low_churn); }
                let mut command = crate::command::new("tmux");
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
        },
        "daemon.wake" => {
            let mut state = interactive_state(state, interactive_waiters);
            state.detector.wake();
            state.attached_focus.last_sync = None;
            Ok(json!({"woken": true}))
        }
        "attention.ack" => parse_params::<AckParams>(request.params).and_then(|params| {
            let mut state = interactive_state(state, interactive_waiters);
            if state.detector.acknowledge(&params.event_id, now_unix_ms()) {
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
                let mut state = interactive_state(state, interactive_waiters);
                let agent = state.detector.report_agent_event(&report)?;
                if report.event == crate::model::AgentEventType::SessionStart
                    && report.reason_category.as_deref() != Some("compact")
                {
                    state.notifications.scheduler.observe_session_start(
                        report.occurred_at_unix_ms,
                        &report.event_id,
                        &agent,
                    );
                }
                if report.event == crate::model::AgentEventType::Error {
                    state.notifications.scheduler.observe_task_error(
                        report.occurred_at_unix_ms,
                        &report.event_id,
                        &agent,
                    );
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
                let mut state = interactive_state(state, interactive_waiters);
                let resolved = state.detector.resolve_agent_event(&detached);
                let (report, agent) = match resolved {
                    Ok(resolved) => resolved,
                    Err(first_error) => {
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
                        match state.detector.resolve_agent_event(&detached) {
                            Ok(resolved) => resolved,
                            Err(scan_error) => {
                                // Refresh discovery without erasing other panes' hook
                                // bindings, ordering guards, or notification identities.
                                state.detector.refresh_process_inventory();
                                state.detector.tick(&config, &manifests, now)
                                    .map_err(|error| error.to_string())?;
                                state.detector.resolve_agent_event(&detached).map_err(|error| {
                                    format!("detached hook association failed: {first_error}; retry: {scan_error}; refresh: {error}")
                                })?
                            }
                        }
                    }
                };
                if report.event == crate::model::AgentEventType::SessionStart
                    && report.reason_category.as_deref() != Some("compact")
                {
                    state.notifications.scheduler.observe_session_start(
                        report.occurred_at_unix_ms,
                        &report.event_id,
                        &agent,
                    );
                }
                if report.event == crate::model::AgentEventType::Error {
                    state.notifications.scheduler.observe_task_error(
                        report.occurred_at_unix_ms,
                        &report.event_id,
                        &agent,
                    );
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

#[cfg(test)]
mod focus_tests {
    use super::*;

    #[test]
    fn detached_hook_filter_keeps_unexpected_failures_visible() {
        for error in [
            "event is not for the active foreground session",
            "out-of-order event",
            "no matching live agent pane",
            "ambiguous live agent panes; refusing to guess",
        ] {
            assert!(expected_detached_hook_rejection(error));
        }
        assert!(!expected_detached_hook_rejection("permission denied"));
        assert!(!expected_detached_hook_rejection(
            "unexpected detector failure"
        ));
    }

    #[test]
    fn fast_detector_ticks_only_poll_clients_four_times_per_second() {
        let start = Instant::now();
        let mut focus = AttachedFocus::default();
        let polls = (0..20)
            .filter(|tick| {
                focus.due(
                    start + Duration::from_millis(tick * 50),
                    Duration::from_millis(250),
                )
            })
            .count();
        assert_eq!(polls, 4);
        focus.invalidate("/dev/tty1");
        assert!(focus.due(
            start + Duration::from_millis(960),
            Duration::from_millis(250)
        ));
    }

    #[test]
    fn batched_client_target_keeps_linked_session_and_rejects_missing_context() {
        let row = [
            "/dev/tty1",
            "attached,focused",
            "1",
            "$9",
            "linked session",
            "@4",
            "7",
            "window name",
            "%6",
            "2",
        ]
        .join("\u{1f}");
        let (attachment, focus, overlay, target) = attached_client_row(&row).unwrap();
        assert_eq!(attachment, "/dev/tty1");
        assert!(matches!(focus, FocusState::Focused));
        assert!(overlay);
        assert_eq!(target.session_id, "$9");
        assert_eq!(target.session_name, "linked session");
        assert_eq!(target.window_index, 7);
        assert_eq!(target.pane_index, 2);
        assert!(attached_client_row(&row.replace("$9", "")).is_none());
        assert!(attached_client_row(&row.replace("%6", "garbage")).is_none());
    }
}
