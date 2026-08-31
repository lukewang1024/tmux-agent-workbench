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
    pub detection: DetectionConfig,
    pub sidebar: SidebarConfig,
    pub notifications: NotificationConfig,
    pub relay: RelayConfig,
    pub openpeon: OpenPeonConfig,
    pub clients: ClientsConfig,
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
pub struct DetectionConfig {
    pub process_interval_ms: u64,
    pub active_capture_interval_ms: u64,
    pub idle_capture_interval_ms: u64,
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
            max_width: 36,
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

impl Config {
    pub fn load(path: &Path) -> Result<Self, ConfigError> {
        if !path.exists() {
            return Ok(Self::default());
        }
        let input = fs::read_to_string(path).map_err(|source| ConfigError::Read {
            path: path.display().to_string(),
            source,
        })?;
        let value: Self = toml::from_str(&input)?;
        value.validate()?;
        Ok(value)
    }

    pub fn validate(&self) -> Result<(), ConfigError> {
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
        assert_eq!(c.detection.process_interval_ms, 1_000);
        assert_eq!(c.detection.active_capture_interval_ms, 500);
        assert_eq!(c.detection.idle_capture_interval_ms, 2_000);
        assert_eq!(c.sidebar.width, 26);
        assert_eq!(c.sidebar.main_min_width, 80);
        assert_eq!(c.relay.port, 19_999);
    }

    #[test]
    fn preserves_forward_compatibility_for_unknown_keys() {
        let config = toml::from_str::<Config>("mystery = true").unwrap();
        assert!(!config.clients.selected_implies_focused);
    }

    #[test]
    fn validates_capture_bounds() {
        let mut c = Config::default();
        c.detection.capture_lines = 201;
        assert!(c.validate().is_err());
    }
}
