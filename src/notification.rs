use std::collections::{HashMap, HashSet};
use std::fs;
use std::io;
use std::path::Path;
use std::process::{Command, Stdio};

use serde::Deserialize;

use crate::config::{Config, NotificationStyle};
use crate::model::{AgentSnapshot, AttentionKind, BaseState, DisplayState};
use crate::paths::Paths;

const DONE_WAV: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/done.wav"));
const REQUEST_WAV: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/request.wav"));
const RECHECK_MS: u64 = 1_000;

// One category vocabulary for lifecycle, routing and every transport.
pub use crate::semantic::SemanticCategory as NotificationCategory;
use crate::semantic::SemanticEvent;

#[derive(Debug, Deserialize)]
struct OpenPeonManifest {
    categories: HashMap<String, OpenPeonCategory>,
}

#[derive(Debug, Deserialize)]
struct OpenPeonCategory {
    #[serde(default)]
    sounds: Vec<OpenPeonSound>,
}

#[derive(Debug, Deserialize)]
struct OpenPeonSound {
    file: String,
}

/// A notification has already passed lifecycle validation. Transports consume
/// this value without interpreting hooks, terminal text, or agent state.
#[derive(Debug, Clone)]
pub struct Notification {
    pub event: SemanticEvent,
    pub agent: AgentSnapshot,
}

#[derive(Debug, Clone)]
struct Pending {
    due_ms: u64,
    notification: Notification,
    requires_attention: bool,
}

/// The sole lifecycle gate, shared by local, client and relay delivery.
/// A ready event remains available until the common router accepts it; this
/// permits switching transports without restarting its deadline or debounce.
#[derive(Default)]
pub struct NotificationScheduler {
    pending: HashMap<String, Pending>,
}

pub trait NotificationBackend {
    fn sound(&mut self, category: NotificationCategory, config: &Config) -> Result<(), String>;
    fn desktop(
        &mut self,
        notification: &Notification,
        style: NotificationStyle,
    ) -> Result<(), String>;
}

impl NotificationScheduler {
    pub fn observe(&mut self, now_ms: u64, agents: &[AgentSnapshot]) {
        let mut active = HashSet::new();
        for agent in agents {
            let Some(attention) = &agent.attention else {
                continue;
            };
            let valid = match attention.kind {
                AttentionKind::Blocked => agent.display_state == DisplayState::Blocked,
                AttentionKind::Done => {
                    agent.display_state == DisplayState::Done
                        || (attention.seen && agent.base_state == BaseState::Idle)
                }
            };
            if !valid {
                continue;
            }
            let category = category_for_attention(agent, attention.kind);
            let notification = Notification::new(
                attention.id.clone(),
                category,
                attention.since_unix_ms,
                agent.clone(),
            );
            if now_ms > notification.event.deadline_unix_ms {
                continue;
            }
            active.insert(attention.id.clone());
            let pending = self.pending.entry(attention.id.clone()).or_insert(Pending {
                due_ms: now_ms.saturating_add(RECHECK_MS),
                notification: notification.clone(),
                requires_attention: true,
            });
            pending.notification = notification;
        }
        self.pending.retain(|id, pending| {
            now_ms <= pending.notification.event.deadline_unix_ms
                && (!pending.requires_attention || active.contains(id))
        });
    }

    pub fn observe_session_start(&mut self, _now_ms: u64, _event_id: &str, _agent: &AgentSnapshot) {
        // Session startup is intentionally silent on every transport.
    }

    pub fn observe_task_error(&mut self, now_ms: u64, event_id: &str, agent: &AgentSnapshot) {
        self.pending.entry(event_id.into()).or_insert(Pending {
            due_ms: now_ms,
            notification: Notification::new(
                event_id.into(),
                NotificationCategory::TaskError,
                now_ms,
                agent.clone(),
            ),
            requires_attention: false,
        });
    }

    pub fn ready(&self, now_ms: u64) -> Vec<Notification> {
        self.pending
            .values()
            .filter(|pending| {
                pending.due_ms <= now_ms && now_ms <= pending.notification.event.deadline_unix_ms
            })
            .map(|pending| pending.notification.clone())
            .collect()
    }

    pub fn pending_events(&self) -> Vec<SemanticEvent> {
        self.pending
            .values()
            .map(|p| p.notification.event.clone())
            .collect()
    }

    pub fn restore(&mut self, event: SemanticEvent, agent: &AgentSnapshot, now_ms: u64) {
        // Attention events are rebuilt from reconciled live snapshots, so an
        // obsolete checkpoint can never manufacture a blocked notification.
        if event.category == NotificationCategory::TaskError && now_ms <= event.deadline_unix_ms {
            self.pending.entry(event.id.clone()).or_insert(Pending {
                due_ms: now_ms,
                notification: Notification {
                    event,
                    agent: agent.clone(),
                },
                requires_attention: false,
            });
        }
    }
}

impl Notification {
    pub(crate) fn new(
        id: String,
        category: NotificationCategory,
        at: u64,
        agent: AgentSnapshot,
    ) -> Self {
        let title = format!("Workbench · {}", agent.label);
        let body = match category {
            NotificationCategory::TaskComplete => {
                format!("Task complete · {}", agent.target.session_name)
            }
            NotificationCategory::TaskError => {
                format!("Task failed · {}", agent.target.session_name)
            }
            NotificationCategory::InputRequired => format!(
                "Input required · {} · {}",
                agent.target.session_name,
                agent.reason_category.as_deref().unwrap_or("blocked")
            ),
            NotificationCategory::SessionStart => {
                format!("Session started · {}", agent.target.session_name)
            }
        };
        Self {
            event: SemanticEvent {
                id,
                category,
                target: agent.target.clone(),
                created_unix_ms: at,
                deadline_unix_ms: at.saturating_add(category.horizon_ms()),
                title,
                body,
            },
            agent,
        }
    }

    pub fn desktop_allowed(&self) -> bool {
        !self.agent.visible && !self.agent.attention.as_ref().is_some_and(|a| a.seen)
    }
}

/// Platform output only: no lifecycle parsing, timers, or deduplication here.
pub fn deliver_local<B: NotificationBackend>(
    notification: &Notification,
    desktop: bool,
    config: &Config,
    backend: &mut B,
) -> Result<(), String> {
    let category = notification.event.category;
    let muted = match category {
        NotificationCategory::TaskComplete | NotificationCategory::TaskError => {
            config.notifications.mute_done
        }
        NotificationCategory::InputRequired => config.notifications.mute_request,
        NotificationCategory::SessionStart => true,
    };
    let mut errors = Vec::new();
    if config.notifications.sound && !muted {
        if let Err(error) = backend.sound(category, config) {
            errors.push(error);
        }
    }
    if config.notifications.enabled && desktop {
        if let Err(error) = backend.desktop(notification, config.notifications.style) {
            errors.push(error);
        }
    }
    if errors.is_empty() {
        Ok(())
    } else {
        Err(errors.join("; "))
    }
}

pub struct SystemBackend {
    paths: Paths,
    last_pack_sound: HashMap<NotificationCategory, std::path::PathBuf>,
    selection_counter: usize,
}

impl SystemBackend {
    pub fn new(paths: &Paths) -> Self {
        Self {
            paths: paths.clone(),
            last_pack_sound: HashMap::new(),
            selection_counter: 0,
        }
    }

    fn sound_path(&self, category: NotificationCategory) -> Result<std::path::PathBuf, io::Error> {
        fs::create_dir_all(&self.paths.cache_dir)?;
        let (name, bytes) = match category {
            NotificationCategory::TaskComplete | NotificationCategory::SessionStart => {
                ("done.wav", DONE_WAV)
            }
            NotificationCategory::InputRequired | NotificationCategory::TaskError => {
                ("request.wav", REQUEST_WAV)
            }
        };
        let path = self.paths.cache_dir.join(name);
        if !path.exists() {
            fs::write(&path, bytes)?;
        }
        Ok(path)
    }

    fn selected_sound(
        &mut self,
        category: NotificationCategory,
        config: &Config,
    ) -> Result<std::path::PathBuf, io::Error> {
        if let Some(path) = self.resolve_pack_sound(category, config) {
            Ok(path)
        } else {
            self.sound_path(category)
        }
    }

    fn resolve_pack_sound(
        &mut self,
        category: NotificationCategory,
        config: &Config,
    ) -> Option<std::path::PathBuf> {
        let active = config.openpeon.active_pack.as_deref()?;
        if !safe_component(active) {
            return None;
        }
        let packs = config
            .openpeon
            .packs_dir
            .as_ref()
            .map(std::path::PathBuf::from)
            .or_else(|| dirs::home_dir().map(|home| home.join(".openpeon/packs")))?;
        let pack = packs.join(active);
        let manifest: OpenPeonManifest =
            serde_json::from_slice(&fs::read(pack.join("openpeon.json")).ok()?).ok()?;
        let category_name = match category {
            NotificationCategory::TaskComplete => "task.complete",
            NotificationCategory::InputRequired => "input.required",
            NotificationCategory::SessionStart => "session.start",
            NotificationCategory::TaskError => "task.error",
        };
        let candidates: Vec<_> = manifest
            .categories
            .get(category_name)?
            .sounds
            .iter()
            .filter_map(|sound| {
                let relative = Path::new(&sound.file);
                if relative.is_absolute()
                    || relative.components().any(|part| {
                        matches!(
                            part,
                            std::path::Component::ParentDir
                                | std::path::Component::RootDir
                                | std::path::Component::Prefix(_)
                        )
                    })
                {
                    return None;
                }
                let candidate = pack.join(relative);
                candidate.is_file().then_some(candidate)
            })
            .collect();
        if candidates.is_empty() {
            return None;
        }
        let mut index = self.selection_counter % candidates.len();
        self.selection_counter = self.selection_counter.wrapping_add(1);
        if config.notifications.no_repeat
            && candidates.len() > 1
            && self.last_pack_sound.get(&category) == Some(&candidates[index])
        {
            index = (index + 1) % candidates.len();
        }
        let selected = candidates[index].clone();
        self.last_pack_sound.insert(category, selected.clone());
        Some(selected)
    }
}

impl NotificationBackend for SystemBackend {
    fn sound(&mut self, category: NotificationCategory, config: &Config) -> Result<(), String> {
        let path = self
            .selected_sound(category, config)
            .map_err(|error| error.to_string())?;
        spawn_audio(&path, config.notifications.volume)
    }

    fn desktop(
        &mut self,
        notification: &Notification,
        style: NotificationStyle,
    ) -> Result<(), String> {
        let agent = &notification.agent;
        let title = &notification.event.title;
        let body = &notification.event.body;
        #[cfg(target_os = "macos")]
        {
            match style {
                NotificationStyle::Overlay => spawn_macos_overlay(&title, &body, agent),
                NotificationStyle::System => spawn_macos_system(&title, &body),
            }
        }
        #[cfg(target_os = "linux")]
        {
            let _ = style;
            spawn_linux_notification(&title, &body, agent)
        }
        #[cfg(not(any(target_os = "macos", target_os = "linux")))]
        Err("desktop notifications are unsupported on this platform".into())
    }
}

fn category_for_attention(agent: &AgentSnapshot, kind: AttentionKind) -> NotificationCategory {
    match kind {
        AttentionKind::Blocked => NotificationCategory::InputRequired,
        AttentionKind::Done if agent.reason_category.as_deref() == Some("task_error") => {
            NotificationCategory::TaskError
        }
        AttentionKind::Done => NotificationCategory::TaskComplete,
    }
}

fn spawn_audio(path: &Path, volume: f32) -> Result<(), String> {
    #[cfg(target_os = "macos")]
    let mut command = {
        let mut command = Command::new("afplay");
        command.arg("-v").arg(volume.to_string()).arg(path);
        command
    };
    #[cfg(target_os = "linux")]
    let mut command = {
        let mut command = if command_exists("pw-play") {
            let mut value = Command::new("pw-play");
            value
                .arg("--media-role=Notification")
                .arg(format!("--volume={volume}"));
            value
        } else if command_exists("paplay") {
            let mut value = Command::new("paplay");
            value.arg(format!(
                "--volume={}",
                (volume.clamp(0.0, 1.0) * 65_536.0).round() as u32
            ));
            value
        } else if command_exists("aplay") {
            if volume != 1.0 {
                return Err(
                    "aplay cannot apply notification volume; install pw-play or paplay".into(),
                );
            }
            Command::new("aplay")
        } else {
            return Err("no supported audio player found".into());
        };
        command.arg(path);
        command
    };
    #[cfg(target_os = "android")]
    let mut command = {
        let _ = volume;
        let mut command = Command::new("termux-media-player");
        command.arg("play").arg(path);
        command
    };
    command
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map(|_| ())
        .map_err(|error| error.to_string())
}

fn safe_component(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value != "."
        && value != ".."
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
}

#[cfg(target_os = "linux")]
fn command_exists(name: &str) -> bool {
    std::env::var_os("PATH")
        .is_some_and(|paths| std::env::split_paths(&paths).any(|path| path.join(name).is_file()))
}

#[cfg(target_os = "linux")]
fn spawn_linux_notification(title: &str, body: &str, agent: &AgentSnapshot) -> Result<(), String> {
    if std::env::var_os("WSL_DISTRO_NAME").is_some() {
        let local = Command::new("cmd.exe")
            .args(["/d", "/c", "echo", "%LOCALAPPDATA%"])
            .output()
            .map_err(|error| error.to_string())?;
        if local.status.success() {
            let windows = String::from_utf8_lossy(&local.stdout).trim().to_owned();
            let converted = Command::new("wslpath")
                .args(["-u", &windows])
                .output()
                .map_err(|error| error.to_string())?;
            let helper =
                std::path::PathBuf::from(String::from_utf8_lossy(&converted.stdout).trim())
                    .join("tmux-agent-workbench/wb-client.exe");
            if helper.is_file() {
                let event_id = agent
                    .attention
                    .as_ref()
                    .map(|event| event.id.as_str())
                    .unwrap_or(&agent.instance_id);
                return Command::new(helper)
                    .args(["notify", event_id, title, body])
                    .stdin(Stdio::null())
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .spawn()
                    .map(|_| ())
                    .map_err(|error| error.to_string());
            }
        }
    }
    let supports_action = Command::new("notify-send")
        .arg("--help")
        .output()
        .is_ok_and(|output| String::from_utf8_lossy(&output.stdout).contains("--action"));
    if !supports_action {
        return Command::new("notify-send")
            .args(["--app-name", "Workbench", title, body])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .map(|_| ())
            .map_err(|error| error.to_string());
    }

    let executable = std::env::current_exe().map_err(|error| error.to_string())?;
    let title = title.to_owned();
    let body = body.to_owned();
    let target = agent.target.clone();
    let relay = agent.relay_focus.clone();
    std::thread::spawn(move || {
        let clicked = Command::new("notify-send")
            .args([
                "--app-name",
                "Workbench",
                "--action=default=Open",
                "--wait",
                &title,
                &body,
            ])
            .output()
            .is_ok_and(|output| String::from_utf8_lossy(&output.stdout).trim() == "default");
        if !clicked {
            return;
        }
        let mut command = Command::new(executable);
        if let Some(relay) = relay {
            command.args([
                "relay",
                "focus-click",
                "--remote-id",
                &relay.remote_id,
                "--tmux-socket",
                &relay.tmux_socket,
                "--session-id",
                &relay.session_id,
                "--pane-id",
                &relay.pane_id,
            ]);
        } else {
            command.args([
                "focus",
                "--session",
                &target.session_id,
                "--window",
                &target.window_id,
                "--pane",
                &target.pane_id,
            ]);
        }
        let _ = command.status();
    });
    Ok(())
}

#[cfg(target_os = "macos")]
fn spawn_macos_system(title: &str, body: &str) -> Result<(), String> {
    let script =
        "on run argv\n display notification (item 2 of argv) with title (item 1 of argv)\nend run";
    Command::new("osascript")
        .args(["-e", script, title, body])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map(|_| ())
        .map_err(|error| error.to_string())
}

#[cfg(target_os = "macos")]
fn spawn_macos_overlay(title: &str, body: &str, agent: &AgentSnapshot) -> Result<(), String> {
    let script = include_str!("../assets/macos-overlay.js");
    let executable = std::env::current_exe().map_err(|error| error.to_string())?;
    Command::new("osascript")
        .args(["-l", "JavaScript", "-e", script])
        .arg(title)
        .arg(body)
        .arg(executable)
        .arg(&agent.target.session_id)
        .arg(&agent.target.window_id)
        .arg(&agent.target.pane_id)
        .arg(
            agent
                .relay_focus
                .as_ref()
                .map(|focus| focus.remote_id.as_str())
                .unwrap_or(""),
        )
        .arg(
            agent
                .relay_focus
                .as_ref()
                .map(|focus| focus.tmux_socket.as_str())
                .unwrap_or(""),
        )
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map(|_| ())
        .map_err(|error| error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{
        AttentionEvent, HookHealth, ProcessFingerprint, StateConfidence, StateSource, TmuxTarget,
    };

    #[derive(Default)]
    struct FakeBackend {
        sounds: Vec<NotificationCategory>,
        desktops: usize,
    }
    impl NotificationBackend for FakeBackend {
        fn sound(
            &mut self,
            category: NotificationCategory,
            _config: &Config,
        ) -> Result<(), String> {
            self.sounds.push(category);
            Ok(())
        }
        fn desktop(
            &mut self,
            _notification: &Notification,
            _style: NotificationStyle,
        ) -> Result<(), String> {
            self.desktops += 1;
            Ok(())
        }
    }

    fn agent(kind: AttentionKind, visible: bool, seen: bool) -> AgentSnapshot {
        AgentSnapshot {
            instance_id: "agent-1".into(),
            kind: crate::model::AgentKind::Codex,
            label: "build".into(),
            target: TmuxTarget {
                session_id: "$1".into(),
                session_name: "s".into(),
                window_id: "@1".into(),
                window_index: 0,
                window_name: "w".into(),
                pane_id: "%1".into(),
                pane_index: 0,
            },
            process: Some(ProcessFingerprint {
                pid: 1,
                started_at_ticks: 1,
                executable: "codex".into(),
            }),
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
            reason_category: None,
            attention: Some(AttentionEvent {
                id: "event-1".into(),
                kind,
                seen,
                since_unix_ms: 0,
                attention_seq: None,
                seen_seq: None,
            }),
            stale: false,
            visible,
            manifest_version: 1,
            rule_id: None,
            hook_session_id: None,
            relay_focus: None,
            exited: false,
            exited_at_unix_ms: None,
            conversations: Vec::new(),
        }
    }

    struct TestPipeline {
        pipeline: crate::notification_pipeline::NotificationPipeline,
        clients: crate::client::ClientRegistry,
        relay: crate::relay::RelaySender,
        _temp: tempfile::TempDir,
    }

    impl Default for TestPipeline {
        fn default() -> Self {
            let temp = tempfile::tempdir().unwrap();
            let paths = Paths {
                config_dir: temp.path().into(),
                state_dir: temp.path().into(),
                cache_dir: temp.path().into(),
                runtime_dir: temp.path().into(),
            };
            fs::write(paths.relay_file(), "[outbound]\nremote_id = 'fixture'\ntoken = 'test'\nendpoint = 'http://127.0.0.1:1'\n").unwrap();
            let server = crate::server::ServerIdentity {
                socket_path: "/tmp/test.sock".into(),
                key: "test".into(),
            };
            Self {
                pipeline: Default::default(),
                clients: Default::default(),
                relay: crate::relay::RelaySender::new(&paths, &server),
                _temp: temp,
            }
        }
    }

    impl TestPipeline {
        fn observe(&mut self, at: u64, agents: &[AgentSnapshot]) {
            self.pipeline.scheduler.observe(at, agents);
        }
        fn observe_session_start(&mut self, at: u64, id: &str, agent: &AgentSnapshot) {
            self.pipeline.scheduler.observe_session_start(at, id, agent);
        }
        fn observe_task_error(&mut self, at: u64, id: &str, agent: &AgentSnapshot) {
            self.pipeline.scheduler.observe_task_error(at, id, agent);
        }
        fn dispatch(
            &mut self,
            at: u64,
            agents: &[AgentSnapshot],
            config: &Config,
            backend: &mut FakeBackend,
        ) {
            self.pipeline.dispatch(
                at,
                agents,
                config,
                crate::notification_pipeline::DeliveryTargets {
                    clients: &mut self.clients,
                    relay: &mut self.relay,
                    local: backend,
                },
            );
        }
        fn connect(&mut self, at: u64) -> String {
            self.clients
                .register(
                    "device".into(),
                    "phone".into(),
                    "termux".into(),
                    vec!["notification".into()],
                    at,
                )
                .0
        }
    }

    #[test]
    fn every_transport_uses_the_same_recheck_and_automatic_review_cancellation() {
        for remote in [false, true] {
            let mut h = TestPipeline::default();
            let endpoint = remote.then(|| h.connect(0));
            let mut backend = FakeBackend::default();
            let config = Config::default();
            let blocked = agent(AttentionKind::Blocked, false, false);
            h.dispatch(0, &[blocked.clone()], &config, &mut backend);
            h.dispatch(999, &[blocked.clone()], &config, &mut backend);
            assert!(h.clients.pending_events().is_empty());
            assert!(h.relay.pending_event_ids().is_empty());
            assert!(backend.sounds.is_empty());
            let mut review = blocked.clone();
            review.attention = None;
            review.display_state = DisplayState::Working;
            h.dispatch(1_000, &[review], &config, &mut backend);
            // Same ID returning after review needs a fresh recheck window.
            h.dispatch(10_000, &[blocked.clone()], &config, &mut backend);
            h.dispatch(10_999, &[blocked.clone()], &config, &mut backend);
            assert!(h.clients.pending_events().is_empty());
            assert!(h.relay.pending_event_ids().is_empty());
            assert!(backend.sounds.is_empty());
            h.dispatch(11_000, &[blocked.clone()], &config, &mut backend);
            if let Some(endpoint) = endpoint {
                let queued = h.clients.take_pending(&endpoint, 11_000).unwrap();
                assert_eq!(queued.len(), 1);
                assert_eq!(queued[0].category, NotificationCategory::InputRequired);
                assert_eq!(queued[0].id, "event-1");
                assert!(backend.sounds.is_empty());
            } else {
                assert_eq!(backend.sounds, vec![NotificationCategory::InputRequired]);
                assert_eq!(backend.desktops, 1);
                assert_eq!(h.relay.pending_event_ids(), vec!["event-1"]);
            }
        }
    }

    #[test]
    fn revokes_client_and_relay_queues_when_attention_disappears_or_is_seen() {
        for remote in [false, true] {
            for seen in [false, true] {
                let mut h = TestPipeline::default();
                if remote {
                    h.connect(0);
                }
                let mut backend = FakeBackend::default();
                let config = Config::default();
                let mut blocked = agent(AttentionKind::Blocked, false, false);
                h.dispatch(0, &[blocked.clone()], &config, &mut backend);
                h.dispatch(1_000, &[blocked.clone()], &config, &mut backend);
                assert_eq!(
                    h.clients.pending_events().len() + h.relay.pending_event_ids().len(),
                    1
                );
                if seen {
                    blocked.attention.as_mut().unwrap().seen = true;
                } else {
                    blocked.attention = None;
                    blocked.display_state = DisplayState::Working;
                }
                // The same refresh is used immediately before client dequeue.
                h.pipeline
                    .refresh(1_001, &[blocked], &mut h.clients, &mut h.relay);
                assert!(h.clients.pending_events().is_empty());
                assert!(h.relay.pending_event_ids().is_empty());
            }
        }
    }

    #[test]
    fn transport_switch_shares_acceptance_and_does_not_restart_debounce() {
        let mut h = TestPipeline::default();
        let mut backend = FakeBackend::default();
        let config = Config::default();
        let agents = [agent(AttentionKind::Done, false, false)];
        h.dispatch(0, &agents, &config, &mut backend);
        let endpoint = h.connect(500);
        h.dispatch(1_000, &agents, &config, &mut backend);
        assert_eq!(h.clients.take_pending(&endpoint, 1_000).unwrap().len(), 1);
        h.pipeline.router.accepted("event-1", &endpoint);
        h.clients = Default::default();
        h.dispatch(1_001, &agents, &config, &mut backend);
        assert!(backend.sounds.is_empty());
        assert!(h.relay.pending_event_ids().is_empty());
        // Restart restores the same global acceptance, regardless of channel.
        let accepted = h.pipeline.router.accepted_event_ids();
        h.pipeline = Default::default();
        h.pipeline.router.restore_accepted(accepted);
        h.dispatch(2_000, &agents, &config, &mut backend);
        h.dispatch(3_000, &agents, &config, &mut backend);
        assert!(backend.sounds.is_empty());
    }

    #[test]
    fn local_delivery_is_not_replayed_when_a_client_connects() {
        let mut h = TestPipeline::default();
        let mut backend = FakeBackend::default();
        let config = Config::default();
        let agents = [agent(AttentionKind::Done, false, false)];
        h.dispatch(0, &agents, &config, &mut backend);
        h.dispatch(1_000, &agents, &config, &mut backend);
        h.connect(1_001);
        h.dispatch(2_000, &agents, &config, &mut backend);
        assert!(h.clients.pending_events().is_empty());
        assert_eq!(backend.desktops, 1);
    }

    #[test]
    fn expired_or_mismatched_attention_never_reaches_any_transport() {
        for remote in [false, true] {
            let mut h = TestPipeline::default();
            if remote {
                h.connect(0);
            }
            let mut backend = FakeBackend::default();
            let config = Config::default();
            let mut blocked = agent(AttentionKind::Blocked, false, false);
            blocked.display_state = DisplayState::Working;
            h.dispatch(0, &[blocked.clone()], &config, &mut backend);
            h.dispatch(2_000, &[blocked.clone()], &config, &mut backend);
            assert!(h.clients.pending_events().is_empty());
            assert!(backend.sounds.is_empty());
            blocked.display_state = DisplayState::Blocked;
            h.dispatch(300_001, &[blocked.clone()], &config, &mut backend);
            h.dispatch(302_000, &[blocked], &config, &mut backend);
            assert!(h.clients.pending_events().is_empty());
            assert!(h.relay.pending_event_ids().is_empty());
            assert!(backend.sounds.is_empty());
        }
    }

    #[test]
    fn errors_share_category_and_deduplication_with_client_delivery() {
        let mut h = TestPipeline::default();
        let endpoint = h.connect(0);
        let mut backend = FakeBackend::default();
        let config = Config::default();
        let mut failed = agent(AttentionKind::Done, false, false);
        failed.attention = None;
        failed.display_state = DisplayState::Working;
        h.observe_task_error(10, "error-1", &failed);
        h.dispatch(10, &[], &config, &mut backend);
        let events = h.clients.take_pending(&endpoint, 10).unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].category, NotificationCategory::TaskError);
        assert_eq!(events[0].body, "Task failed · s");
        h.pipeline.router.accepted("error-1", &endpoint);
        h.clients = Default::default();
        h.dispatch(20, &[], &config, &mut backend);
        assert!(backend.sounds.is_empty());
    }

    #[test]
    fn recovery_revalidates_attention_and_preserves_error_deadlines() {
        let mut h = TestPipeline::default();
        let blocked = agent(AttentionKind::Blocked, false, false);
        let old = Notification::new(
            "event-1".into(),
            NotificationCategory::InputRequired,
            0,
            blocked.clone(),
        );
        h.pipeline.scheduler.restore(old.event, &blocked, 2_000);
        assert!(h.pipeline.scheduler.ready(3_000).is_empty());
        let error = Notification::new(
            "error-1".into(),
            NotificationCategory::TaskError,
            0,
            blocked.clone(),
        );
        h.pipeline.scheduler.restore(error.event, &blocked, 59_999);
        assert_eq!(h.pipeline.scheduler.ready(60_000).len(), 1);
        assert!(h.pipeline.scheduler.ready(60_001).is_empty());
    }

    #[test]
    fn rechecks_for_one_second_sounds_visible_done_and_suppresses_desktop() {
        let mut scheduler = TestPipeline::default();
        let mut backend = FakeBackend::default();
        let config = Config::default();
        let agents = vec![agent(AttentionKind::Done, false, false)];
        scheduler.observe(0, &agents);
        scheduler.dispatch(999, &agents, &config, &mut backend);
        assert!(backend.sounds.is_empty());
        let visible = vec![agent(AttentionKind::Done, true, true)];
        scheduler.dispatch(1_000, &visible, &config, &mut backend);
        assert_eq!(backend.sounds, vec![NotificationCategory::TaskComplete]);
        assert_eq!(backend.desktops, 0);
    }

    #[test]
    fn visible_blocked_plays_request_sound_without_desktop() {
        let mut scheduler = TestPipeline::default();
        let mut backend = FakeBackend::default();
        let config = Config::default();
        let agents = vec![agent(AttentionKind::Blocked, true, true)];
        scheduler.observe(0, &agents);
        scheduler.dispatch(1_000, &agents, &config, &mut backend);
        assert_eq!(backend.sounds, vec![NotificationCategory::InputRequired]);
        assert_eq!(backend.desktops, 0);
    }

    #[test]
    fn background_done_delivers_once_at_the_one_second_recheck() {
        let mut scheduler = TestPipeline::default();
        let mut backend = FakeBackend::default();
        let config = Config::default();
        let agents = vec![agent(AttentionKind::Done, false, false)];
        scheduler.observe(0, &agents);
        scheduler.dispatch(999, &agents, &config, &mut backend);
        assert!(backend.sounds.is_empty());
        scheduler.dispatch(1_000, &agents, &config, &mut backend);
        assert_eq!(backend.sounds, vec![NotificationCategory::TaskComplete]);
        assert_eq!(backend.desktops, 1);

        scheduler.observe(1_001, &agents);
        scheduler.dispatch(3_000, &agents, &config, &mut backend);
        assert_eq!(backend.sounds, vec![NotificationCategory::TaskComplete]);
        assert_eq!(backend.desktops, 1);
    }

    #[test]
    fn pack_name_rejects_path_traversal() {
        assert!(safe_component("my-pack_1.0"));
        assert!(!safe_component("../pack"));
        assert!(!safe_component("pack/name"));
    }

    #[test]
    fn openpeon_selection_honors_no_repeat_and_rejects_escape() {
        let temp = tempfile::tempdir().unwrap();
        let pack = temp.path().join("fixture");
        fs::create_dir_all(pack.join("sounds")).unwrap();
        fs::write(pack.join("sounds/one.wav"), b"one").unwrap();
        fs::write(pack.join("sounds/two.wav"), b"two").unwrap();
        fs::write(
            pack.join("openpeon.json"),
            br#"{"categories":{"task.complete":{"sounds":[{"file":"sounds/one.wav"},{"file":"../escape.wav"},{"file":"sounds/two.wav"}]}}}"#,
        )
        .unwrap();
        let mut config = Config::default();
        config.openpeon.packs_dir = Some(temp.path().display().to_string());
        config.openpeon.active_pack = Some("fixture".into());
        let paths = Paths {
            config_dir: temp.path().join("config"),
            state_dir: temp.path().join("state"),
            cache_dir: temp.path().join("cache"),
            runtime_dir: temp.path().join("runtime"),
        };
        let mut backend = SystemBackend::new(&paths);
        let first = backend
            .resolve_pack_sound(NotificationCategory::TaskComplete, &config)
            .unwrap();
        let second = backend
            .resolve_pack_sound(NotificationCategory::TaskComplete, &config)
            .unwrap();
        assert_ne!(first, second);
        assert!(first.starts_with(&pack));
        assert!(second.starts_with(&pack));
    }

    #[test]
    fn desktop_text_contains_only_agent_session_and_reason_metadata() {
        let blocked = agent(AttentionKind::Blocked, false, false);
        let notification = Notification::new(
            "event-1".into(),
            NotificationCategory::InputRequired,
            0,
            blocked,
        );
        let (title, body) = (notification.event.title, notification.event.body);
        assert_eq!(title, "Workbench · build");
        assert_eq!(body, "Input required · s · blocked");
        assert!(!body.contains('%'));
    }

    #[test]
    fn session_start_is_fully_silent_without_attention() {
        let mut scheduler = TestPipeline::default();
        let mut backend = FakeBackend::default();
        let config = Config::default();
        let mut started = agent(AttentionKind::Done, true, true);
        started.attention = None;
        scheduler.observe_session_start(10, "start-1", &started);
        scheduler.dispatch(10, &[], &config, &mut backend);
        assert!(backend.sounds.is_empty());
        assert_eq!(backend.desktops, 0);
        scheduler.observe_session_start(20, "start-1", &started);
        scheduler.dispatch(20, &[], &config, &mut backend);
        assert!(backend.sounds.is_empty());
    }

    #[test]
    fn task_error_uses_error_category_even_when_visible() {
        let mut scheduler = TestPipeline::default();
        let mut backend = FakeBackend::default();
        let config = Config::default();
        let mut failed = agent(AttentionKind::Done, true, true);
        failed.base_state = BaseState::Working;
        failed.display_state = DisplayState::Working;
        failed.reason_category = Some("task_error".into());
        failed.attention = None;
        scheduler.observe_task_error(0, "error-1", &failed);
        scheduler.dispatch(0, &[], &config, &mut backend);
        assert_eq!(backend.sounds, vec![NotificationCategory::TaskError]);
        assert_eq!(backend.desktops, 0);
    }
}
