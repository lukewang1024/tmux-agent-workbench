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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum NotificationCategory {
    TaskComplete,
    InputRequired,
    SessionStart,
    TaskError,
}

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

#[derive(Debug, Clone)]
struct Pending {
    due_ms: u64,
    category: NotificationCategory,
    agent: AgentSnapshot,
    requires_attention: bool,
}

#[derive(Default)]
pub struct NotificationScheduler {
    pending: HashMap<String, Pending>,
    delivered: HashSet<String>,
}

pub trait NotificationBackend {
    fn sound(&mut self, category: NotificationCategory, config: &Config) -> Result<(), String>;
    fn desktop(&mut self, agent: &AgentSnapshot, style: NotificationStyle) -> Result<(), String>;
}

impl NotificationScheduler {
    pub fn observe(&mut self, now_ms: u64, agents: &[AgentSnapshot]) {
        for agent in agents {
            let Some(event) = &agent.attention else {
                continue;
            };
            if !self.delivered.contains(&event.id) {
                self.pending.entry(event.id.clone()).or_insert(Pending {
                    due_ms: now_ms.saturating_add(RECHECK_MS),
                    category: category_for_attention(agent, event.kind),
                    agent: agent.clone(),
                    requires_attention: true,
                });
            }
        }
    }

    pub fn observe_session_start(&mut self, now_ms: u64, event_id: &str, agent: &AgentSnapshot) {
        if self.delivered.contains(event_id) {
            return;
        }
        self.pending.entry(event_id.to_owned()).or_insert(Pending {
            due_ms: now_ms,
            category: NotificationCategory::SessionStart,
            agent: agent.clone(),
            requires_attention: false,
        });
    }

    pub fn observe_task_error(&mut self, now_ms: u64, event_id: &str, agent: &AgentSnapshot) {
        if self.delivered.contains(event_id) {
            return;
        }
        self.pending.entry(event_id.to_owned()).or_insert(Pending {
            due_ms: now_ms,
            category: NotificationCategory::TaskError,
            agent: agent.clone(),
            requires_attention: false,
        });
    }

    pub fn deliver_due<B: NotificationBackend>(
        &mut self,
        now_ms: u64,
        agents: &[AgentSnapshot],
        config: &Config,
        backend: &mut B,
    ) -> Vec<AgentSnapshot> {
        let mut delivered_agents = Vec::new();
        let due: Vec<_> = self
            .pending
            .iter()
            .filter(|(_, pending)| pending.due_ms <= now_ms)
            .map(|(id, _)| id.clone())
            .collect();
        for event_id in due {
            let Some(pending) = self.pending.remove(&event_id) else {
                continue;
            };
            let (agent, seen) = if pending.requires_attention {
                let Some((agent, event)) = agents.iter().find_map(|agent| {
                    agent
                        .attention
                        .as_ref()
                        .filter(|event| event.id == event_id)
                        .map(|event| (agent, event))
                }) else {
                    continue;
                };
                let valid = match pending.category {
                    NotificationCategory::TaskError => {
                        agent.base_state == BaseState::Idle
                            && agent.reason_category.as_deref() == Some("task_error")
                    }
                    _ => match event.kind {
                        AttentionKind::Blocked => agent.base_state == BaseState::Blocked,
                        AttentionKind::Done => agent.display_state == DisplayState::Done,
                    },
                };
                if !valid {
                    continue;
                }
                (agent.clone(), event.seen)
            } else {
                (pending.agent.clone(), false)
            };

            if config.notifications.sound {
                let muted = match pending.category {
                    NotificationCategory::TaskComplete | NotificationCategory::TaskError => {
                        config.notifications.mute_done
                    }
                    NotificationCategory::InputRequired => config.notifications.mute_request,
                    NotificationCategory::SessionStart => false,
                };
                let should_sound = match pending.category {
                    NotificationCategory::TaskComplete => true,
                    NotificationCategory::InputRequired | NotificationCategory::TaskError => true,
                    NotificationCategory::SessionStart => false,
                };
                if !muted && should_sound {
                    if let Err(error) = backend.sound(pending.category, config) {
                        eprintln!("tmux-agent-workbench: sound delivery failed: {error}");
                    }
                }
            }
            let should_desktop =
                pending.category != NotificationCategory::SessionStart && !agent.visible && !seen;
            if config.notifications.enabled && should_desktop {
                if let Err(error) = backend.desktop(&agent, config.notifications.style) {
                    eprintln!("tmux-agent-workbench: desktop delivery failed: {error}");
                }
            }
            self.delivered.insert(event_id);
            if pending.requires_attention && !agent.visible && !seen {
                delivered_agents.push(agent);
            }
        }
        delivered_agents
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

    fn desktop(&mut self, agent: &AgentSnapshot, style: NotificationStyle) -> Result<(), String> {
        let Some((title, body)) = notification_text(agent) else {
            return Ok(());
        };
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

fn notification_text(agent: &AgentSnapshot) -> Option<(String, String)> {
    let title = format!("Workbench · {}", agent.label);
    if agent.reason_category.as_deref() == Some("task_error") {
        return Some((
            title,
            format!("Task failed · {}", agent.target.session_name),
        ));
    }
    let body = match agent.attention.as_ref()?.kind {
        AttentionKind::Done => format!("Task complete · {}", agent.target.session_name),
        AttentionKind::Blocked => format!(
            "Input required · {} · {}",
            agent.target.session_name,
            agent.reason_category.as_deref().unwrap_or("blocked")
        ),
    };
    Some((title, body))
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
            _agent: &AgentSnapshot,
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

    #[test]
    fn rechecks_for_one_second_sounds_visible_done_and_suppresses_desktop() {
        let mut scheduler = NotificationScheduler::default();
        let mut backend = FakeBackend::default();
        let config = Config::default();
        let agents = vec![agent(AttentionKind::Done, false, false)];
        scheduler.observe(0, &agents);
        scheduler.deliver_due(999, &agents, &config, &mut backend);
        assert!(backend.sounds.is_empty());
        let visible = vec![agent(AttentionKind::Done, true, true)];
        scheduler.deliver_due(1_000, &visible, &config, &mut backend);
        assert_eq!(backend.sounds, vec![NotificationCategory::TaskComplete]);
        assert_eq!(backend.desktops, 0);
    }

    #[test]
    fn visible_blocked_plays_request_sound_without_desktop() {
        let mut scheduler = NotificationScheduler::default();
        let mut backend = FakeBackend::default();
        let config = Config::default();
        let agents = vec![agent(AttentionKind::Blocked, true, true)];
        scheduler.observe(0, &agents);
        scheduler.deliver_due(1_000, &agents, &config, &mut backend);
        assert_eq!(backend.sounds, vec![NotificationCategory::InputRequired]);
        assert_eq!(backend.desktops, 0);
    }

    #[test]
    fn background_done_delivers_once_at_the_one_second_recheck() {
        let mut scheduler = NotificationScheduler::default();
        let mut backend = FakeBackend::default();
        let config = Config::default();
        let agents = vec![agent(AttentionKind::Done, false, false)];
        scheduler.observe(0, &agents);
        scheduler.deliver_due(999, &agents, &config, &mut backend);
        assert!(backend.sounds.is_empty());
        scheduler.deliver_due(1_000, &agents, &config, &mut backend);
        assert_eq!(backend.sounds, vec![NotificationCategory::TaskComplete]);
        assert_eq!(backend.desktops, 1);

        scheduler.observe(1_001, &agents);
        scheduler.deliver_due(3_000, &agents, &config, &mut backend);
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
        let (title, body) = notification_text(&blocked).unwrap();
        assert_eq!(title, "Workbench · build");
        assert_eq!(body, "Input required · s · blocked");
        assert!(!body.contains('%'));
    }

    #[test]
    fn session_start_is_fully_silent_without_attention() {
        let mut scheduler = NotificationScheduler::default();
        let mut backend = FakeBackend::default();
        let config = Config::default();
        let mut started = agent(AttentionKind::Done, true, true);
        started.attention = None;
        scheduler.observe_session_start(10, "start-1", &started);
        scheduler.deliver_due(10, &[], &config, &mut backend);
        assert!(backend.sounds.is_empty());
        assert_eq!(backend.desktops, 0);
        scheduler.observe_session_start(20, "start-1", &started);
        scheduler.deliver_due(20, &[], &config, &mut backend);
        assert!(backend.sounds.is_empty());
    }

    #[test]
    fn task_error_uses_error_category_even_when_visible() {
        let mut scheduler = NotificationScheduler::default();
        let mut backend = FakeBackend::default();
        let config = Config::default();
        let mut failed = agent(AttentionKind::Done, true, true);
        failed.base_state = BaseState::Working;
        failed.display_state = DisplayState::Working;
        failed.reason_category = Some("task_error".into());
        failed.attention = None;
        scheduler.observe_task_error(0, "error-1", &failed);
        scheduler.deliver_due(0, &[], &config, &mut backend);
        assert_eq!(backend.sounds, vec![NotificationCategory::TaskError]);
        assert_eq!(backend.desktops, 0);
    }
}
