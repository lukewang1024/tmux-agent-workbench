use std::fs;
use std::path::Path;

use serde::{Deserialize, Serialize};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum ConfigError {
    #[error("cannot read {path}: {source}")]
    Read {
        path: String,
        source: std::io::Error,
    },
    #[error("invalid configuration: {0}")]
    Parse(#[from] toml::de::Error),
    #[error("invalid configuration: {0}")]
    Validation(String),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct Config {
    pub ui: UiConfig,
    pub detection: DetectionConfig,
    pub sidebar: SidebarConfig,
    pub auto_split: AutoSplitConfig,
    pub notifications: NotificationConfig,
    pub relay: RelayConfig,
    pub openpeon: OpenPeonConfig,
    pub clients: ClientsConfig,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct AutoSplitConfig {
    pub enabled: bool,
    pub threshold: Option<usize>,
    pub strategy: Option<SplitStrategy>,
}

impl Default for AutoSplitConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            threshold: Some(5),
            strategy: Some(SplitStrategy::Semantic),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SplitStrategy {
    Semantic,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct ClientsConfig {
    /// Compatibility escape hatch. Unknown focus remains unseen by default.
    pub selected_implies_focused: bool,
    pub devices: std::collections::HashMap<String, DeviceClientConfig>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct DeviceClientConfig {
    pub selected_implies_focused: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct UiConfig {
    /// `auto` follows the system display language; other BCP 47 tags select a
    /// language family, with Chinese supported and English as the fallback.
    pub language: String,
}

impl Default for UiConfig {
    fn default() -> Self {
        Self {
            language: "auto".into(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct DetectionConfig {
    pub process_interval_ms: u64,
    pub active_capture_interval_ms: u64,
    pub idle_capture_interval_ms: u64,
    /// Trade slower hookless idle discovery for substantially fewer processes.
    pub low_process_churn: bool,
    pub capture_lines: usize,
    pub capture_bytes: usize,
    pub stale_grace_ms: u64,
}

impl Default for DetectionConfig {
    fn default() -> Self {
        Self {
            process_interval_ms: 1_000,
            active_capture_interval_ms: 500,
            idle_capture_interval_ms: 2_000,
            low_process_churn: false,
            capture_lines: 40,
            capture_bytes: 65_536,
            stale_grace_ms: 3_000,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct SidebarConfig {
    pub width: u16,
    pub min_width: u16,
    pub max_width: u16,
    pub main_min_width: u16,
    pub position: SidebarPosition,
    pub auto_create: bool,
    pub agent_sort: AgentSort,
}

impl Default for SidebarConfig {
    fn default() -> Self {
        Self {
            width: 26,
            min_width: 18,
            max_width: 64,
            main_min_width: 80,
            position: SidebarPosition::Left,
            auto_create: true,
            agent_sort: AgentSort::Grouped,
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentSort {
    #[default]
    Grouped,
    Prioritized,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SidebarPosition {
    Left,
    Right,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct NotificationConfig {
    pub enabled: bool,
    pub sound: bool,
    pub style: NotificationStyle,
    pub volume: f32,
    pub no_repeat: bool,
    pub mute_done: bool,
    pub mute_request: bool,
}

impl Default for NotificationConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            sound: true,
            style: NotificationStyle::Overlay,
            volume: 1.0,
            no_repeat: true,
            mute_done: false,
            mute_request: false,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NotificationStyle {
    Overlay,
    System,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct RelayConfig {
    pub bind: String,
    pub port: u16,
}

impl Default for RelayConfig {
    fn default() -> Self {
        Self {
            bind: "127.0.0.1".into(),
            port: 19_999,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(default, deny_unknown_fields)]
pub struct OpenPeonConfig {
    pub packs_dir: Option<String>,
    pub active_pack: Option<String>,
}

fn valid_language_tag(value: &str) -> bool {
    if value.eq_ignore_ascii_case("auto") {
        return true;
    }
    let mut subtags = value.split('-');
    let Some(language) = subtags.next() else {
        return false;
    };
    (2..=8).contains(&language.len())
        && language.bytes().all(|byte| byte.is_ascii_alphabetic())
        && subtags.all(|subtag| {
            !subtag.is_empty()
                && subtag.len() <= 8
                && subtag.bytes().all(|byte| byte.is_ascii_alphanumeric())
        })
}

impl Config {
    pub fn load(path: &Path) -> Result<Self, ConfigError> {
        let mut value: Self = if path.exists() {
            let input = fs::read_to_string(path).map_err(|source| ConfigError::Read {
                path: path.display().to_string(),
                source,
            })?;
            toml::from_str(&input)?
        } else {
            Self::default()
        };
        // A standalone opt-in is ignored by older binaries, so a local
        // rollback does not break strict parsing of the shared config file.
        if path
            .parent()
            .is_some_and(|dir| dir.join("enable-low-process-churn").is_file())
        {
            value.detection.low_process_churn = true;
        }
        value.validate()?;
        Ok(value)
    }

    pub fn validate(&self) -> Result<(), ConfigError> {
        if self.auto_split.enabled
            && (self.auto_split.threshold.is_none() || self.auto_split.strategy.is_none())
        {
            return Err(ConfigError::Validation(
                "auto_split requires an explicit threshold and strategy when enabled".into(),
            ));
        }
        if self
            .auto_split
            .threshold
            .is_some_and(|threshold| !(2..=999).contains(&threshold))
        {
            return Err(ConfigError::Validation(
                "auto_split.threshold must be in 2..=999".into(),
            ));
        }
        if !valid_language_tag(&self.ui.language) {
            return Err(ConfigError::Validation(
                "ui.language must be 'auto' or a BCP 47 language tag, for example zh-CN or en-US"
                    .into(),
            ));
        }
        let d = &self.detection;
        if d.process_interval_ms == 0
            || d.active_capture_interval_ms == 0
            || d.idle_capture_interval_ms == 0
            || d.stale_grace_ms == 0
        {
            return Err(ConfigError::Validation(
                "detection intervals must be positive".into(),
            ));
        }
        if d.capture_lines == 0 || d.capture_lines > 200 {
            return Err(ConfigError::Validation(
                "capture_lines must be in 1..=200".into(),
            ));
        }
        if d.capture_bytes == 0 || d.capture_bytes > 65_536 {
            return Err(ConfigError::Validation(
                "capture_bytes must be in 1..=65536".into(),
            ));
        }
        if self.sidebar.min_width > self.sidebar.width
            || self.sidebar.min_width == 0
            || self.sidebar.main_min_width == 0
            || self.sidebar.width > self.sidebar.max_width
            || self.sidebar.max_width > 64
        {
            return Err(ConfigError::Validation(
                "sidebar widths must satisfy min <= width <= max <= 64".into(),
            ));
        }
        if !(0.0..=1.0).contains(&self.notifications.volume) {
            return Err(ConfigError::Validation(
                "notifications.volume must be in 0..=1".into(),
            ));
        }
        if self.relay.bind.parse::<std::net::IpAddr>().is_err() {
            return Err(ConfigError::Validation(
                "relay.bind must be an IP address".into(),
            ));
        }
        if self.relay.port == 0 {
            return Err(ConfigError::Validation(
                "relay.port must be in 1..=65535".into(),
            ));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_match_specification() {
        let c = Config::default();
        assert_eq!(c.ui.language, "auto");
        assert_eq!(c.detection.process_interval_ms, 1_000);
        assert_eq!(c.detection.active_capture_interval_ms, 500);
        assert_eq!(c.detection.idle_capture_interval_ms, 2_000);
        assert_eq!(c.sidebar.width, 26);
        assert_eq!(c.sidebar.main_min_width, 80);
        assert_eq!(c.relay.port, 19_999);
    }

    #[test]
    fn local_low_churn_marker_is_optional_and_independent_of_toml() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("config.toml");
        let marker = directory.path().join("enable-low-process-churn");
        assert!(!Config::load(&path).unwrap().detection.low_process_churn);
        std::fs::write(&marker, "").unwrap();
        assert!(Config::load(&path).unwrap().detection.low_process_churn);
        std::fs::write(&path, "[ui]\nlanguage = 'en-US'\n").unwrap();
        assert!(Config::load(&path).unwrap().detection.low_process_churn);
        std::fs::remove_file(&marker).unwrap();
        assert!(!Config::load(&path).unwrap().detection.low_process_churn);
        std::fs::write(&path, "[detection]\nlow_process_churn = true\n").unwrap();
        assert!(Config::load(&path).unwrap().detection.low_process_churn);
    }

    #[test]
    fn preserves_forward_compatibility_for_unknown_keys() {
        let config = toml::from_str::<Config>("mystery = true").unwrap();
        assert!(!config.clients.selected_implies_focused);
    }

    #[test]
    fn accepts_language_tags_and_rejects_invalid_values() {
        for language in ["auto", "zh-CN", "en-US", "ja-JP"] {
            let input = format!("[ui]\nlanguage = {language:?}\n");
            let config = toml::from_str::<Config>(&input).unwrap();
            assert!(config.validate().is_ok(), "{language}");
            assert_eq!(config.ui.language, language);
        }
        let config = Config {
            ui: UiConfig {
                language: "not a locale".into(),
            },
            ..Config::default()
        };
        assert!(config.validate().is_err());
    }

    #[test]
    fn validates_capture_bounds() {
        let mut c = Config::default();
        c.detection.capture_lines = 201;
        assert!(c.validate().is_err());
    }

    #[test]
    fn auto_split_defaults_to_a_five_task_semantic_limit_when_enabled() {
        let config = Config::default();
        assert!(!config.auto_split.enabled);
        assert_eq!(config.auto_split.threshold, Some(5));
        assert_eq!(config.auto_split.strategy, Some(SplitStrategy::Semantic));
        assert!(
            toml::from_str::<Config>("[auto_split]\nenabled = true\n")
                .unwrap()
                .validate()
                .is_ok()
        );
        let invalid =
            toml::from_str::<Config>("[auto_split]\nenabled = true\nthreshold = 1\n").unwrap();
        assert!(invalid.validate().is_err());
        assert!(toml::from_str::<Config>("[auto_split]\nstrategy = 'count'\n").is_err());
    }
}
