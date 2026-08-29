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
        let preferred_runtime_dir = match env::var_os("XDG_RUNTIME_DIR") {
            Some(root) => PathBuf::from(root).join("tmux-agent-workbench"),
            None => env::temp_dir().join(format!("tmux-agent-workbench-{}", unsafe {
                libc::geteuid()
            })),
        };
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

#[cfg(unix)]
fn set_mode_0700(path: &Path) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(0o700))
}

#[cfg(not(unix))]
fn set_mode_0700(_path: &Path) -> io::Result<()> {
    Ok(())
}
