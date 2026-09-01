use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::semantic::SemanticEvent;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RuntimeCheckpoint {
    pub version: u32,
    pub server_incarnation: String,
    pub runtime_id: String,
    pub process_fingerprint: String,
    pub previous_state: String,
    pub attention_seq: u64,
    pub seen_seq: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hook_session_id: Option<String>,
    #[serde(default)]
    pub delivered_event_ids: Vec<String>,
    #[serde(default)]
    pub pending: Vec<SemanticEvent>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recent_endpoint: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ServerCheckpoint {
    pub version: u32,
    pub server_incarnation: String,
    pub updated_unix_ms: u64,
    #[serde(default)]
    pub runtimes: Vec<RuntimeCheckpoint>,
}

impl RuntimeCheckpoint {
    pub fn reconciles(
        &self,
        server_incarnation: &str,
        process_fingerprint: &str,
        pane_live: bool,
    ) -> bool {
        pane_live
            && self.server_incarnation == server_incarnation
            && self.process_fingerprint == process_fingerprint
    }

    pub fn prune_expired(&mut self, now_ms: u64) {
        self.pending
            .retain(|event| now_ms <= event.deadline_unix_ms);
    }
}

pub fn load(path: &Path) -> io::Result<Option<RuntimeCheckpoint>> {
    let bytes = match fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    };
    serde_json::from_slice(&bytes)
        .map(Some)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))
}

pub fn store(path: &Path, checkpoint: &RuntimeCheckpoint) -> io::Result<()> {
    store_value(path, checkpoint)
}

pub fn load_server(path: &Path) -> io::Result<Option<ServerCheckpoint>> {
    let bytes = match fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    };
    serde_json::from_slice(&bytes)
        .map(Some)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))
}

pub fn store_server(path: &Path, checkpoint: &ServerCheckpoint) -> io::Result<()> {
    store_value(path, checkpoint)
}

fn store_value(path: &Path, checkpoint: &impl Serialize) -> io::Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| io::Error::other("checkpoint path has no parent"))?;
    fs::create_dir_all(parent)?;
    fs::set_permissions(parent, fs::Permissions::from_mode(0o700))?;
    let temporary = path.with_extension(format!("tmp-{}", uuid::Uuid::new_v4()));
    let mut file = OpenOptions::new()
        .create_new(true)
        .write(true)
        .mode(0o600)
        .open(&temporary)?;
    serde_json::to_writer(&mut file, checkpoint).map_err(io::Error::other)?;
    file.write_all(b"\n")?;
    file.sync_all()?;
    fs::rename(&temporary, path)?;
    fs::set_permissions(path, fs::Permissions::from_mode(0o600))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn round_trip_is_private_and_reconcile_is_strict() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("servers/a.json");
        let checkpoint = RuntimeCheckpoint {
            version: 1,
            server_incarnation: "i".into(),
            runtime_id: "r".into(),
            process_fingerprint: "p".into(),
            previous_state: "working".into(),
            attention_seq: 3,
            seen_seq: 2,
            hook_session_id: Some("thread-1".into()),
            delivered_event_ids: vec![],
            pending: vec![],
            recent_endpoint: None,
        };
        store(&path, &checkpoint).unwrap();
        assert_eq!(
            fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        assert_eq!(load(&path).unwrap().unwrap(), checkpoint);
        assert!(checkpoint.reconciles("i", "p", true));
        assert!(!checkpoint.reconciles("other", "p", true));
    }

    #[test]
    fn corrupt_checkpoint_is_reported() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("bad.json");
        fs::write(&path, b"not json").unwrap();
        assert_eq!(load(&path).unwrap_err().kind(), io::ErrorKind::InvalidData);
    }
}
