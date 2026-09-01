use std::env;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone)]
pub struct Paths {
    pub config_dir: PathBuf,
    pub state_dir: PathBuf,
    pub cache_dir: PathBuf,
    pub runtime_dir: PathBuf,
}

impl Paths {
    pub fn discover() -> io::Result<Self> {
        let home =
            dirs::home_dir().ok_or_else(|| io::Error::other("home directory unavailable"))?;
        let config_root = env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| home.join(".config"));
        let state_root = env::var_os("XDG_STATE_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| home.join(".local/state"));
        let cache_root = env::var_os("XDG_CACHE_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| home.join(".cache"));
        let uid = unsafe { libc::geteuid() };
        let preferred_runtime_dir = canonical_runtime_dir(uid);
        // macOS permits only 104 bytes for sockaddr_un.sun_path. Keep enough
        // room for `/daemon-<16 hex>.sock`; long per-user TMPDIR values are
        // common under /var/folders.
        let runtime_dir = if preferred_runtime_dir.as_os_str().as_encoded_bytes().len() > 70 {
            PathBuf::from("/tmp").join(format!("tmux-agent-workbench-{}", unsafe {
                libc::geteuid()
            }))
        } else {
            preferred_runtime_dir
        };
        let paths = Self {
            config_dir: config_root.join("tmux-agent-workbench"),
            state_dir: state_root.join("tmux-agent-workbench"),
            cache_dir: cache_root.join("tmux-agent-workbench"),
            runtime_dir,
        };
        paths.ensure_private_runtime()?;
        Ok(paths)
    }

    pub fn config_file(&self) -> PathBuf {
        self.config_dir.join("config.toml")
    }

    pub fn manifests_dir(&self) -> PathBuf {
        self.config_dir.join("manifests")
    }

    pub fn relay_file(&self) -> PathBuf {
        self.config_dir.join("relay.toml")
    }

    pub fn socket_for_server(&self, server_key: &str) -> PathBuf {
        self.runtime_dir.join(format!("daemon-{server_key}.sock"))
    }

    pub fn lock_for_server(&self, server_key: &str) -> PathBuf {
        self.runtime_dir.join(format!("daemon-{server_key}.lock"))
    }

    pub fn log_for_server(&self, server_key: &str) -> PathBuf {
        self.state_dir.join(format!("daemon-{server_key}.log"))
    }

    pub fn spool_for_server(&self, server_key: &str) -> PathBuf {
        self.runtime_dir.join(format!("spool-{server_key}"))
    }

    pub fn checkpoint_for_server(&self, server_key: &str) -> PathBuf {
        self.state_dir
            .join("servers")
            .join(format!("{server_key}.json"))
    }

    pub fn workspaces_dir(&self) -> PathBuf {
        let data_root = env::var_os("XDG_DATA_HOME")
            .map(PathBuf::from)
            .or_else(|| dirs::home_dir().map(|home| home.join(".local/share")))
            .expect("home was validated while discovering paths");
        data_root.join("tmux-agent-workbench/workspaces")
    }

    fn ensure_private_runtime(&self) -> io::Result<()> {
        if let Ok(metadata) = fs::symlink_metadata(&self.runtime_dir) {
            if metadata.file_type().is_symlink() || !metadata.is_dir() {
                return Err(io::Error::other("runtime path must be a real directory"));
            }
            #[cfg(unix)]
            {
                use std::os::unix::fs::MetadataExt;
                if metadata.uid() != unsafe { libc::geteuid() } {
                    return Err(io::Error::new(
                        io::ErrorKind::PermissionDenied,
                        "runtime directory belongs to another user",
                    ));
                }
            }
        }
        fs::create_dir_all(&self.runtime_dir)?;
        set_mode_0700(&self.runtime_dir)
    }
}

fn canonical_runtime_dir(uid: u32) -> PathBuf {
    // tmux keeps the environment captured when the server was created while
    // shells attached later may or may not export XDG_RUNTIME_DIR. Resolve the
    // standard Linux user runtime directory independently of that inherited
    // environment so one tmux server cannot acquire two daemon sockets.
    if let Some(root) = env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .filter(|path| !path.exists() || owned_directory(path, uid))
    {
        return root.join("tmux-agent-workbench");
    }
    #[cfg(target_os = "linux")]
    {
        let standard = PathBuf::from(format!("/run/user/{uid}"));
        if owned_directory(&standard, uid) {
            return standard.join("tmux-agent-workbench");
        }
    }
    env::temp_dir().join(format!("tmux-agent-workbench-{uid}"))
}

fn owned_directory(path: &Path, uid: u32) -> bool {
    let Ok(metadata) = fs::symlink_metadata(path) else {
        return false;
    };
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        metadata.uid() == uid
    }
    #[cfg(not(unix))]
    {
        let _ = uid;
        true
    }
}

#[cfg(unix)]
fn set_mode_0700(path: &Path) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(0o700))
}

#[cfg(not(unix))]
fn set_mode_0700(_path: &Path) -> io::Result<()> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn runtime_directory_is_private_and_uid_scoped() {
        let paths = Paths::discover().unwrap();
        assert!(owned_directory(&paths.runtime_dir, unsafe {
            libc::geteuid()
        }));
        assert!(
            paths
                .runtime_dir
                .file_name()
                .is_some_and(|name| name.to_string_lossy().starts_with("tmux-agent-workbench"))
        );
    }
}
