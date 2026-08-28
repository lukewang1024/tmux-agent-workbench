use std::env;
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum ServerError {
    #[error("not inside tmux (TMUX is unset)")]
    NotInTmux,
    #[error("invalid TMUX value: {0}")]
    InvalidTmux(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServerIdentity {
    pub socket_path: PathBuf,
    pub key: String,
}

impl ServerIdentity {
    pub fn discover() -> Result<Self, ServerError> {
        if let Some(path) = env::var_os("TMUX_AGENT_WORKBENCH_TMUX_SOCKET") {
            return Self::from_socket(PathBuf::from(path));
        }
        let value = env::var("TMUX").map_err(|_| ServerError::NotInTmux)?;
        let socket = value
            .split_once(',')
            .map(|(socket, _)| socket)
            .filter(|socket| !socket.is_empty())
            .ok_or_else(|| ServerError::InvalidTmux(value.clone()))?;
        Self::from_socket(PathBuf::from(socket))
    }

    pub fn from_socket(socket_path: PathBuf) -> Result<Self, ServerError> {
        if socket_path.as_os_str().is_empty() || !socket_path.is_absolute() {
            return Err(ServerError::InvalidTmux(socket_path.display().to_string()));
        }
        let canonical = socket_path
            .canonicalize()
            .unwrap_or_else(|_| socket_path.clone());
        let mut hasher = Sha256::new();
        hasher.update(canonical.as_os_str().as_encoded_bytes());
        let key = format!("{:x}", hasher.finalize());
        Ok(Self {
            socket_path,
            key: key[..16].to_owned(),
        })
    }

    pub fn tmux_args(&self) -> [&Path; 2] {
        [Path::new("-S"), &self.socket_path]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn key_is_stable_and_server_specific() {
        let a = ServerIdentity::from_socket(PathBuf::from("/tmp/tmux-a")).unwrap();
        let again = ServerIdentity::from_socket(PathBuf::from("/tmp/tmux-a")).unwrap();
        let b = ServerIdentity::from_socket(PathBuf::from("/tmp/tmux-b")).unwrap();
        assert_eq!(a.key, again.key);
        assert_ne!(a.key, b.key);
        assert_eq!(a.key.len(), 16);
    }
}
