//! Short-lived, process-owned records for rebuilding a daemon's client registry.
//!
//! The control and PTY processes each hold an advisory lock for their lifetime.
//! A file left behind by a crashed process is therefore never proof that the
//! associated terminal is still connected.
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use fs2::FileExt;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::paths::Paths;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ControlRecord {
    pub endpoint_id: String,
    pub device_id: String,
    pub terminal_id: Option<String>,
    pub device_label: String,
    pub kind: String,
    pub capabilities: Vec<String>,
    pub token: String,
    pub registered_at_ms: u64,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct BindingRecord {
    endpoint_id: String,
    attachment: String,
}

pub struct Lease {
    path: PathBuf,
    lock: File,
}

impl Drop for Lease {
    fn drop(&mut self) {
        // Tokens are unique and never reused, so nobody can acquire a new
        // lease at this name while the old process still owns it.
        let _ = fs::remove_file(&self.path);
        let _ = FileExt::unlock(&self.lock);
    }
}

fn record_path(paths: &Paths, server_key: &str, kind: &str, token: &str) -> io::Result<PathBuf> {
    uuid::Uuid::parse_str(token).map_err(io::Error::other)?;
    if !server_key.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(io::Error::other("invalid server key"));
    }
    Ok(paths
        .runtime_dir
        .join(format!("client-{kind}-{server_key}-{token}.lease")))
}

fn owner_path(paths: &Paths, server_key: &str, device_id: &str, terminal_id: &str) -> PathBuf {
    let hash = Sha256::digest(format!("{device_id}\0{terminal_id}").as_bytes());
    paths
        .runtime_dir
        .join(format!("client-owner-{server_key}-{hash:x}"))
}

/// Called while the daemon holds its registry lock, so a later registration
/// always fences an older control channel for the same terminal.
pub fn mark_owner(
    paths: &Paths,
    server_key: &str,
    device_id: &str,
    terminal_id: Option<&str>,
    endpoint_id: &str,
) -> io::Result<()> {
    let Some(terminal_id) = terminal_id else {
        return Ok(());
    };
    let path = owner_path(paths, server_key, device_id, terminal_id);
    let temporary = path.with_extension(format!("tmp-{}", uuid::Uuid::new_v4()));
    let result = (|| {
        let mut file = OpenOptions::new()
            .create_new(true)
            .write(true)
            .mode(0o600)
            .open(&temporary)?;
        file.write_all(endpoint_id.as_bytes())?;
        fs::rename(&temporary, &path)
    })();
    if result.is_err() {
        let _ = fs::remove_file(temporary);
    }
    result
}

fn create_record(path: PathBuf, value: &impl Serialize) -> io::Result<Lease> {
    let mut file = OpenOptions::new()
        .create_new(true)
        .read(true)
        .write(true)
        .mode(0o600)
        .open(&path)?;
    file.try_lock_exclusive()?;
    if let Err(error) = serde_json::to_writer(&mut file, value)
        .map_err(io::Error::other)
        .and_then(|()| file.flush())
    {
        let _ = fs::remove_file(&path);
        return Err(error);
    }
    Ok(Lease { path, lock: file })
}

pub fn control(paths: &Paths, server_key: &str, record: &ControlRecord) -> io::Result<Lease> {
    cleanup_stale(paths, server_key);
    create_record(
        record_path(paths, server_key, "control", &record.token)?,
        record,
    )
}

pub fn binding(
    paths: &Paths,
    server_key: &str,
    token: &str,
    endpoint_id: &str,
    attachment: &str,
) -> io::Result<Lease> {
    create_record(
        record_path(paths, server_key, "binding", token)?,
        &BindingRecord {
            endpoint_id: endpoint_id.to_owned(),
            attachment: attachment.to_owned(),
        },
    )
}

fn read_live<T: for<'de> Deserialize<'de>>(path: &Path) -> io::Result<Option<T>> {
    let file = match File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    };
    match file.try_lock_exclusive() {
        Ok(()) => {
            // The owning process is gone. Never restore an unlocked record.
            let _ = fs::remove_file(path);
            let _ = FileExt::unlock(&file);
            Ok(None)
        }
        Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
            let mut bytes = Vec::new();
            file.take(64 * 1024).read_to_end(&mut bytes)?;
            serde_json::from_slice(&bytes)
                .map(Some)
                .map_err(io::Error::other)
        }
        Err(error) => Err(error),
    }
}

/// Read both independently locked records. A current owner marker prevents a
/// superseded control channel from claiming a newer terminal's attachment.
pub fn recover(
    paths: &Paths,
    server_key: &str,
    token: &str,
) -> io::Result<Option<(ControlRecord, Option<String>)>> {
    let Some(control): Option<ControlRecord> =
        read_live(&record_path(paths, server_key, "control", token)?)?
    else {
        return Ok(None);
    };
    if control.token != token
        || uuid::Uuid::parse_str(&control.endpoint_id).is_err()
        || uuid::Uuid::parse_str(&control.device_id).is_err()
    {
        return Err(io::Error::other("invalid client lease"));
    }
    if let Some(terminal_id) = &control.terminal_id {
        uuid::Uuid::parse_str(terminal_id).map_err(io::Error::other)?;
        let owner = fs::read_to_string(owner_path(
            paths,
            server_key,
            &control.device_id,
            terminal_id,
        ))?;
        if owner != control.endpoint_id {
            return Err(io::Error::other("client terminal has a newer owner"));
        }
    }
    let binding: Option<BindingRecord> =
        read_live(&record_path(paths, server_key, "binding", token)?)?;
    let attachment = match binding {
        Some(binding) if binding.endpoint_id == control.endpoint_id => Some(binding.attachment),
        Some(_) => {
            return Err(io::Error::other(
                "client binding does not match control lease",
            ));
        }
        None => None,
    };
    Ok(Some((control, attachment)))
}

fn cleanup_stale(paths: &Paths, server_key: &str) {
    let prefixes = [
        format!("client-control-{server_key}-"),
        format!("client-binding-{server_key}-"),
    ];
    let Ok(entries) = fs::read_dir(&paths.runtime_dir) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if !prefixes.iter().any(|prefix| name.starts_with(prefix)) || !name.ends_with(".lease") {
            continue;
        }
        let old = entry
            .metadata()
            .and_then(|m| m.modified())
            .ok()
            .and_then(|m| SystemTime::now().duration_since(m).ok())
            .is_some_and(|age| age > Duration::from_secs(300));
        if !old {
            continue;
        }
        if let Ok(file) = OpenOptions::new().read(true).write(true).open(entry.path()) {
            if file.try_lock_exclusive().is_ok() {
                let _ = fs::remove_file(entry.path());
                let _ = FileExt::unlock(&file);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recovery_requires_live_leases_and_the_latest_terminal_owner() {
        let temp = tempfile::tempdir().unwrap();
        let paths = Paths {
            config_dir: temp.path().into(),
            state_dir: temp.path().into(),
            cache_dir: temp.path().into(),
            runtime_dir: temp.path().into(),
        };
        let record = ControlRecord {
            endpoint_id: uuid::Uuid::new_v4().to_string(),
            device_id: uuid::Uuid::new_v4().to_string(),
            terminal_id: Some(uuid::Uuid::new_v4().to_string()),
            device_label: "phone".into(),
            kind: "termux".into(),
            capabilities: vec!["notification".into()],
            token: uuid::Uuid::new_v4().to_string(),
            registered_at_ms: 1,
        };
        let key = "abc123";
        mark_owner(
            &paths,
            key,
            &record.device_id,
            record.terminal_id.as_deref(),
            &record.endpoint_id,
        )
        .unwrap();
        let control = control(&paths, key, &record).unwrap();
        let binding = binding(
            &paths,
            key,
            &record.token,
            &record.endpoint_id,
            "/dev/pts/7",
        )
        .unwrap();
        let (recovered, attachment) = recover(&paths, key, &record.token).unwrap().unwrap();
        assert_eq!(recovered.endpoint_id, record.endpoint_id);
        assert_eq!(attachment.as_deref(), Some("/dev/pts/7"));
        mark_owner(
            &paths,
            key,
            &record.device_id,
            record.terminal_id.as_deref(),
            &uuid::Uuid::new_v4().to_string(),
        )
        .unwrap();
        assert!(recover(&paths, key, &record.token).is_err());
        mark_owner(
            &paths,
            key,
            &record.device_id,
            record.terminal_id.as_deref(),
            &record.endpoint_id,
        )
        .unwrap();
        drop(binding);
        assert_eq!(
            recover(&paths, key, &record.token).unwrap().unwrap().1,
            None
        );
        drop(control);
        assert!(recover(&paths, key, &record.token).unwrap().is_none());
    }
}
