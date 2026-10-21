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

#[derive(Default)]
pub(crate) struct DaemonSockets {
    pub sockets: Vec<PathBuf>,
    pub permission_error: Option<String>,
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
        let runtime_dir = bounded_runtime_dir(uid);
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

    /// Tool runners may strip XDG_RUNTIME_DIR as well as TMUX. Recover runtime
    /// locations from the caller's ancestors and tmux server environment, then
    /// let the daemon verify the agent identity. Explicit runtime roots remain
    /// authoritative, including isolated test and client environments.
    pub fn daemon_sockets(&self) -> Vec<PathBuf> {
        self.discover_daemon_sockets().sockets
    }

    pub(crate) fn discover_daemon_sockets(&self) -> DaemonSockets {
        use std::collections::BTreeSet;
        use std::process::Command;
        use sysinfo::{Pid, ProcessRefreshKind, ProcessesToUpdate, System, UpdateKind};

        let uid = unsafe { libc::geteuid() };
        let mut discovery = DaemonSockets::default();
        let mut directories = BTreeSet::from([self.runtime_dir.clone()]);
        if env::var_os("XDG_RUNTIME_DIR").is_none() && self.runtime_dir == bounded_runtime_dir(uid)
        {
            let mut add_root = |root: &Path| {
                let directory = root.join("tmux-agent-workbench");
                if root.is_absolute() && owned_directory(&directory, uid) {
                    directories.insert(directory);
                }
            };
            let mut servers = BTreeSet::new();
            if let Ok(server) = crate::server::ServerIdentity::discover() {
                servers.insert(server.socket_path);
            }
            let mut system = System::new();
            let mut pid = Pid::from_u32(std::process::id());
            for _ in 0..64 {
                system.refresh_processes_specifics(
                    ProcessesToUpdate::Some(&[pid]),
                    true,
                    ProcessRefreshKind::nothing()
                        .without_tasks()
                        .with_environ(UpdateKind::OnlyIfNotSet),
                );
                let Some(process) = system.process(pid) else {
                    break;
                };
                for value in process.environ() {
                    let value = value.to_string_lossy();
                    if let Some(root) = value.strip_prefix("XDG_RUNTIME_DIR=") {
                        add_root(Path::new(root));
                    }
                    if let Some(tmux) = value.strip_prefix("TMUX=") {
                        if let Some((socket, _)) = tmux.split_once(',') {
                            if Path::new(socket).is_absolute() {
                                servers.insert(PathBuf::from(socket));
                            }
                        }
                    }
                }
                let Some(parent) = process.parent() else {
                    break;
                };
                if parent == pid {
                    break;
                }
                pid = parent;
            }
            // Discover metadata only; the default server's focused window is
            // never used to select a naming target.
            let targets = if servers.is_empty() {
                vec![None]
            } else {
                servers.into_iter().map(Some).collect()
            };
            for server in targets {
                let mut command = Command::new("tmux");
                command.arg("-u");
                if let Some(socket) = server {
                    command.arg("-S").arg(socket);
                }
                if let Ok(output) = command
                    .args(["show-environment", "-g", "XDG_RUNTIME_DIR"])
                    .env("LC_ALL", "C")
                    .output()
                {
                    if output.status.success() {
                        let value = String::from_utf8_lossy(&output.stdout);
                        if let Some(root) = value.trim_end().strip_prefix("XDG_RUNTIME_DIR=") {
                            add_root(Path::new(root));
                        }
                    } else {
                        let error = String::from_utf8_lossy(&output.stderr);
                        if permission_denied_message(&error) {
                            discovery.permission_error = Some(error.trim().into());
                        }
                    }
                }
            }
        }
        let explicit_server = env::var_os("TMUX_AGENT_WORKBENCH_TMUX_SOCKET")
            .and_then(|socket| crate::server::ServerIdentity::from_socket(socket.into()).ok())
            .map(|server| format!("daemon-{}.sock", server.key));
        for directory in directories {
            if !owned_directory(&directory, uid) {
                continue;
            }
            if let Ok(entries) = fs::read_dir(directory) {
                for entry in entries.flatten() {
                    let name = entry.file_name();
                    let name = name.to_string_lossy();
                    if name.starts_with("daemon-")
                        && name.ends_with(".sock")
                        && explicit_server
                            .as_deref()
                            .is_none_or(|expected| name == expected)
                    {
                        discovery.sockets.push(entry.path());
                    }
                }
            }
        }
        discovery.sockets.sort();
        discovery
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

pub(crate) fn permission_denied_message(message: &str) -> bool {
    let message = message.to_ascii_lowercase();
    message.contains("permission denied") || message.contains("operation not permitted")
}

fn bounded_runtime_dir(uid: u32) -> PathBuf {
    let preferred = canonical_runtime_dir(uid);
    // Leave room for `/daemon-<16 hex>.sock` in macOS's 104-byte sun_path.
    if preferred.as_os_str().as_encoded_bytes().len() > 70 {
        PathBuf::from("/tmp").join(format!("tmux-agent-workbench-{uid}"))
    } else {
        preferred
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
