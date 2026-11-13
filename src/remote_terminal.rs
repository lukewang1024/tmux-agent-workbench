//! TLS reverse terminal support for SSH-inaccessible tmux servers.
//!
//! The embedded Python helper uses only the standard library. Python's `ssl`
//! provides TLS and `pty` provides the Mac-side controlling terminal. The
//! helper is embedded in this binary so installation remains a single file.

use std::collections::BTreeMap;
use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;

use serde::{Deserialize, Serialize};
use tmux_agent_workbench::paths::Paths;

const CONFIG_FILE: &str = "remote-terminal.json";
const ROUTES_FILE: &str = "remote-routes.json";

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct RelayConfig {
    #[serde(default)]
    server: Option<ServerSettings>,
    #[serde(default)]
    targets: BTreeMap<String, TargetSettings>,
    #[serde(default)]
    tmux_socket: Option<PathBuf>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ServerSettings {
    bind: String,
    certificate: PathBuf,
    private_key: PathBuf,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct TargetSettings {
    endpoint: String,
    server_name: String,
    ca_certificate: PathBuf,
    token_file: PathBuf,
    #[serde(default)]
    machine_fabric_tunnel: Option<MachineFabricTunnel>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct MachineFabricTunnel {
    executor_id: String,
    ssh_host: String,
    tunnel_id: String,
    forward_to: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Route {
    pub via: String,
    pub target: String,
    #[serde(default)]
    pub remote_core: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Routes {
    #[serde(default)]
    routes: BTreeMap<String, Route>,
}

pub fn add_target(
    paths: &Paths,
    target: &str,
    endpoint: &str,
    server_name: &str,
    ca_certificate: &Path,
    token_file: Option<&Path>,
    machine_fabric_executor: Option<&str>,
    ssh_host: Option<&str>,
    forward_to: Option<&str>,
) -> Result<(), Box<dyn std::error::Error>> {
    validate_name(target, "target")?;
    validate_endpoint(endpoint)?;
    validate_name(server_name, "TLS server name")?;
    if !ca_certificate.is_absolute() {
        return Err("CA certificate path must be absolute".into());
    }
    let config_path = config_path(paths);
    let mut config = load_config(&config_path)?;
    let token_file = token_file.map(Path::to_path_buf).unwrap_or_else(|| {
        paths
            .config_dir
            .join("remote-terminal")
            .join(format!("{target}.token"))
    });
    if !token_file.is_absolute() {
        return Err("token file path must be absolute".into());
    }
    let machine_fabric_tunnel = match (machine_fabric_executor, ssh_host, forward_to) {
        (None, None, None) => None,
        (Some(executor_id), Some(ssh_host), Some(forward_to)) => {
            validate_name(executor_id, "Machine Fabric executor")?;
            validate_ssh_host(ssh_host)?;
            let (local_host, _) = split_endpoint(endpoint)?;
            let (forward_host, _) = split_endpoint(forward_to)?;
            if !matches!(local_host, "127.0.0.1" | "localhost") {
                return Err("Machine Fabric tunnel endpoint must bind to loopback".into());
            }
            if !matches!(forward_host, "127.0.0.1" | "localhost") {
                return Err("Machine Fabric tunnel target must use Devbox loopback".into());
            }
            let tunnel_id = config
                .targets
                .get(target)
                .and_then(|settings| settings.machine_fabric_tunnel.as_ref())
                .map(|settings| settings.tunnel_id.clone())
                .unwrap_or_else(|| format!("workbench-{}", uuid::Uuid::new_v4().simple()));
            Some(MachineFabricTunnel {
                executor_id: executor_id.to_owned(),
                ssh_host: ssh_host.to_owned(),
                tunnel_id,
                forward_to: forward_to.to_owned(),
            })
        }
        _ => {
            return Err(
                "--machine-fabric-executor, --ssh-host, and --forward-to must be supplied together"
                    .into(),
            );
        }
    };
    config.targets.insert(
        target.to_owned(),
        TargetSettings {
            endpoint: endpoint.to_owned(),
            server_name: server_name.to_owned(),
            ca_certificate: ca_certificate.to_path_buf(),
            token_file,
            machine_fabric_tunnel,
        },
    );
    save_json(&config_path, &config)?;
    println!(
        "configured TLS target {target} in {}",
        config_path.display()
    );
    Ok(())
}

pub fn configure_server(
    paths: &Paths,
    bind: &str,
    certificate: &Path,
    private_key: &Path,
) -> Result<(), Box<dyn std::error::Error>> {
    validate_endpoint(bind)?;
    if !certificate.is_absolute() || !private_key.is_absolute() {
        return Err("certificate and key paths must be absolute".into());
    }
    let config_path = config_path(paths);
    let mut config = load_config(&config_path)?;
    config.server = Some(ServerSettings {
        bind: bind.to_owned(),
        certificate: certificate.to_path_buf(),
        private_key: private_key.to_path_buf(),
    });
    save_json(&config_path, &config)?;
    println!("configured TLS listener in {}", config_path.display());
    Ok(())
}

pub fn pair_target(paths: &Paths, target: &str) -> Result<(), Box<dyn std::error::Error>> {
    validate_name(target, "target")?;
    let config = load_config(&config_path(paths))?;
    let settings = config.targets.get(target).ok_or_else(|| {
        format!("target {target} is not configured; run client stream target-add first")
    })?;
    let token = uuid::Uuid::new_v4().to_string();
    write_private(&settings.token_file, token.as_bytes())?;
    println!(
        "paired {target}; token stored at {}",
        settings.token_file.display()
    );
    Ok(())
}

pub fn add_route(
    paths: &Paths,
    alias: &str,
    via: &str,
    target: &str,
    remote_core: Option<&str>,
) -> Result<(), Box<dyn std::error::Error>> {
    validate_name(alias, "route alias")?;
    validate_name(target, "target")?;
    validate_ssh_host(via)?;
    if let Some(remote_core) = remote_core {
        if !Path::new(remote_core).is_absolute() || remote_core.contains('\0') {
            return Err("remote core path must be an absolute path on the route host".into());
        }
    }
    let path = routes_path(paths);
    let mut routes = load_routes(&path)?;
    routes.routes.insert(
        alias.to_owned(),
        Route {
            via: via.to_owned(),
            target: target.to_owned(),
            remote_core: remote_core.map(str::to_owned),
        },
    );
    save_json(&path, &routes)?;
    if let Some(remote_core) = remote_core {
        println!("route {alias} -> {target} via {via} using {remote_core}");
    } else {
        println!("route {alias} -> {target} via {via}");
    }
    Ok(())
}

pub fn remove_route(paths: &Paths, alias: &str) -> Result<(), Box<dyn std::error::Error>> {
    let path = routes_path(paths);
    let mut routes = load_routes(&path)?;
    if routes.routes.remove(alias).is_none() {
        return Err(format!("route alias not found: {alias}").into());
    }
    save_json(&path, &routes)?;
    println!("removed route {alias}");
    Ok(())
}

pub fn list_routes(paths: &Paths) -> Result<(), Box<dyn std::error::Error>> {
    for (alias, route) in load_routes(&routes_path(paths))?.routes {
        if let Some(remote_core) = route.remote_core {
            println!(
                "{alias}\t{}\tvia {}\tcore {}",
                route.target, route.via, remote_core
            );
        } else {
            println!("{alias}\t{}\tvia {}", route.target, route.via);
        }
    }
    Ok(())
}

pub fn route_for(paths: &Paths, alias: &str) -> Result<Option<Route>, Box<dyn std::error::Error>> {
    Ok(load_routes(&routes_path(paths))?.routes.remove(alias))
}

pub fn serve(paths: &Paths) -> Result<(), Box<dyn std::error::Error>> {
    invoke_helper(paths, ["serve"])
}

pub fn attach(
    paths: &Paths,
    target: &str,
    session: Option<&str>,
) -> Result<(), Box<dyn std::error::Error>> {
    validate_name(target, "target")?;
    if let Some(session) = session {
        validate_name(session, "session")?;
    }
    let mut args = vec![
        "attach".to_owned(),
        "--target".to_owned(),
        target.to_owned(),
    ];
    if let Some(session) = session {
        args.extend(["--session".to_owned(), session.to_owned()]);
    }
    invoke_helper(paths, args)
}

pub fn agent(
    paths: &Paths,
    target: &str,
    session: Option<&str>,
) -> Result<(), Box<dyn std::error::Error>> {
    validate_name(target, "target")?;
    if let Some(session) = session {
        validate_name(session, "session")?;
    }
    ensure_machine_fabric_tunnel(paths, target)?;
    let mut args = vec!["agent".to_owned(), "--target".to_owned(), target.to_owned()];
    if let Some(session) = session {
        args.extend(["--session".to_owned(), session.to_owned()]);
    }
    invoke_helper(paths, args)
}

pub fn agent_session(
    paths: &Paths,
    target: &str,
    attachment: &str,
    session: Option<&str>,
) -> Result<(), Box<dyn std::error::Error>> {
    validate_name(target, "target")?;
    if attachment.len() != 32
        || !attachment
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Err("invalid terminal attachment id".into());
    }
    if let Some(session) = session {
        validate_name(session, "session")?;
    }
    let mut args = vec![
        "agent-session".to_owned(),
        "--target".to_owned(),
        target.to_owned(),
        "--attachment".to_owned(),
        attachment.to_owned(),
    ];
    if let Some(session) = session {
        args.extend(["--session".to_owned(), session.to_owned()]);
    }
    invoke_helper(paths, args)
}

pub fn stop_tunnel(paths: &Paths, target: &str) -> Result<(), Box<dyn std::error::Error>> {
    validate_name(target, "target")?;
    let config = load_config(&config_path(paths))?;
    let settings = config.targets.get(target).ok_or_else(|| {
        format!("target {target} is not configured; run client stream target-add first")
    })?;
    let tunnel = settings
        .machine_fabric_tunnel
        .as_ref()
        .ok_or_else(|| format!("target {target} has no Machine Fabric SSH tunnel configured"))?;
    machine_fabric_call(
        &tunnel.executor_id,
        "tunnel.stop",
        serde_json::json!({"tunnelId": tunnel.tunnel_id}),
    )?;
    println!("stopped Machine Fabric SSH tunnel for {target}");
    Ok(())
}

fn ensure_machine_fabric_tunnel(
    paths: &Paths,
    target: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    let config = load_config(&config_path(paths))?;
    let settings = config.targets.get(target).ok_or_else(|| {
        format!("target {target} is not configured; run client stream target-add first")
    })?;
    let Some(tunnel) = settings.machine_fabric_tunnel.as_ref() else {
        return Ok(());
    };
    let state_dir = paths.state_dir.join("remote-terminal-tunnels");
    fs::create_dir_all(&state_dir)?;
    let params = tunnel_ensure_params(&settings.endpoint, tunnel, &state_dir)?;
    let result = machine_fabric_call(&tunnel.executor_id, "tunnel.ensure", params)?;
    let readiness = result
        .get("readiness")
        .and_then(|readiness| readiness.get("state"))
        .and_then(serde_json::Value::as_str);
    if result.get("state").and_then(serde_json::Value::as_str) != Some("running")
        || (readiness != Some("ready")
            && result
                .get("observedState")
                .and_then(serde_json::Value::as_str)
                != Some("ready"))
    {
        return Err(format!(
            "Machine Fabric tunnel {} did not become ready",
            tunnel.tunnel_id
        )
        .into());
    }
    eprintln!(
        "tmux-agent-workbench: Machine Fabric SSH tunnel {} is ready",
        tunnel.tunnel_id
    );
    Ok(())
}

fn tunnel_ensure_params(
    endpoint: &str,
    tunnel: &MachineFabricTunnel,
    state_dir: &Path,
) -> Result<serde_json::Value, Box<dyn std::error::Error>> {
    let (bind_host, bind_port) = split_endpoint(endpoint)?;
    let (target_host, target_port) = split_endpoint(&tunnel.forward_to)?;
    Ok(serde_json::json!({
        "tunnelId": tunnel.tunnel_id,
        "sshHost": tunnel.ssh_host,
        "direction": "local-forward",
        "bindHost": bind_host,
        "bindPort": bind_port,
        "targetHost": target_host,
        "targetPort": target_port,
        "cwd": state_dir,
        "logPath": state_dir.join(format!("{}.log", tunnel.tunnel_id)),
    }))
}

fn machine_fabric_call(
    executor_id: &str,
    action: &str,
    params: serde_json::Value,
) -> Result<serde_json::Value, Box<dyn std::error::Error>> {
    let request = serde_json::json!({
        "executorId": executor_id,
        "action": action,
        "params": params,
    });
    let binary = machine_fabric_binary();
    let mut command = Command::new(&binary);
    command.args(["call", "executor.call", &request.to_string()]);
    // The sidecar may use isolated XDG paths for its TLS configuration. The
    // Machine Fabric CLI is installed in the user's regular XDG data tree, so
    // don't let the sidecar's paths hide the CLI's current installation.
    if let Some(home) = std::env::var_os("HOME").map(PathBuf::from) {
        command
            .env("XDG_CONFIG_HOME", home.join(".config"))
            .env("XDG_DATA_HOME", home.join(".local/share"))
            .env("XDG_STATE_HOME", home.join(".local/state"));
    }
    let output = command.output().map_err(|error| {
        format!(
            "could not run Machine Fabric CLI {}: {error}",
            binary.display()
        )
    })?;
    if !output.status.success() {
        return Err(format!(
            "Machine Fabric call {action} failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        )
        .into());
    }
    let response: serde_json::Value = serde_json::from_slice(&output.stdout)?;
    if !response
        .get("ok")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false)
    {
        let error = response
            .pointer("/error/message")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("unknown Machine Fabric error");
        return Err(format!("Machine Fabric call {action} failed: {error}").into());
    }
    Ok(response
        .get("result")
        .cloned()
        .unwrap_or(serde_json::Value::Null))
}

fn machine_fabric_binary() -> PathBuf {
    if let Some(home) = std::env::var_os("HOME") {
        let candidate = PathBuf::from(home).join(".local/bin/machine-fabric");
        if candidate.is_file() {
            return candidate;
        }
    }
    PathBuf::from("machine-fabric")
}

fn invoke_helper<I, S>(paths: &Paths, args: I) -> Result<(), Box<dyn std::error::Error>>
where
    I: IntoIterator<Item = S>,
    S: AsRef<std::ffi::OsStr>,
{
    let executable = std::env::current_exe()?;
    let status = Command::new("python3")
        .env("TMUX_AGENT_WORKBENCH_EXECUTABLE", executable)
        .arg("-c")
        .arg(include_str!("remote_terminal.py"))
        .arg("--config")
        .arg(config_path(paths))
        .args(args)
        .status()?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("remote terminal helper exited with {status}").into())
    }
}

fn config_path(paths: &Paths) -> PathBuf {
    paths.config_dir.join(CONFIG_FILE)
}

fn routes_path(paths: &Paths) -> PathBuf {
    paths.config_dir.join(ROUTES_FILE)
}

fn load_config(path: &Path) -> Result<RelayConfig, Box<dyn std::error::Error>> {
    match fs::read(path) {
        Ok(bytes) => Ok(serde_json::from_slice(&bytes)?),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(RelayConfig::default()),
        Err(error) => Err(error.into()),
    }
}

fn load_routes(path: &Path) -> Result<Routes, Box<dyn std::error::Error>> {
    match fs::read(path) {
        Ok(bytes) => Ok(serde_json::from_slice(&bytes)?),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(Routes::default()),
        Err(error) => Err(error.into()),
    }
}

fn save_json(path: &Path, value: &impl Serialize) -> io::Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| io::Error::other("config path has no parent"))?;
    fs::create_dir_all(parent)?;
    let temp = parent.join(format!(
        ".{}.{}.tmp",
        path.file_name().unwrap_or_default().to_string_lossy(),
        uuid::Uuid::new_v4()
    ));
    let mut file = OpenOptions::new()
        .create_new(true)
        .write(true)
        .mode(0o600)
        .open(&temp)?;
    serde_json::to_writer_pretty(&mut file, value).map_err(io::Error::other)?;
    file.write_all(b"\n")?;
    file.sync_all()?;
    fs::rename(temp, path)
}

fn write_private(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| io::Error::other("token path has no parent"))?;
    fs::create_dir_all(parent)?;
    let temp = parent.join(format!(".token.{}.tmp", uuid::Uuid::new_v4()));
    let mut file = OpenOptions::new()
        .create_new(true)
        .write(true)
        .mode(0o600)
        .open(&temp)?;
    file.write_all(bytes)?;
    file.write_all(b"\n")?;
    file.sync_all()?;
    fs::rename(temp, path)
}

fn validate_name(value: &str, label: &str) -> Result<(), Box<dyn std::error::Error>> {
    if value.is_empty()
        || value.len() > 128
        || !value.bytes().all(|byte| {
            byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b':' | b'@' | b'%')
        })
    {
        return Err(format!("invalid {label}").into());
    }
    Ok(())
}

fn validate_ssh_host(host: &str) -> Result<(), Box<dyn std::error::Error>> {
    if host.starts_with('-') {
        return Err("SSH host must not begin with '-'".into());
    }
    validate_name(host, "SSH host")
}

fn validate_endpoint(value: &str) -> Result<(), Box<dyn std::error::Error>> {
    split_endpoint(value)?;
    Ok(())
}

fn split_endpoint(value: &str) -> Result<(&str, u16), Box<dyn std::error::Error>> {
    if value.is_empty() || value.len() > 512 || value.chars().any(char::is_control) {
        return Err("invalid TCP endpoint".into());
    }
    let (host, port) = value
        .rsplit_once(':')
        .ok_or("TCP endpoint must be host:port")?;
    if host.is_empty()
        || !host
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b'_'))
    {
        return Err("invalid TCP endpoint host".into());
    }
    let port = port.parse::<u16>()?;
    if port == 0 {
        return Err("TCP port must be nonzero".into());
    }
    Ok((host, port))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn route_config_serializes_and_rejects_ssh_option_injection() {
        let routes = Routes {
            routes: BTreeMap::from([(
                "Mac".into(),
                Route {
                    via: "devbox-alias".into(),
                    target: "MacBook-Pro".into(),
                    remote_core: None,
                },
            )]),
        };
        let json = serde_json::to_vec(&routes).unwrap();
        assert_eq!(
            serde_json::from_slice::<Routes>(&json).unwrap().routes,
            routes.routes
        );
        let legacy = br#"{"routes":{"Mac":{"via":"devbox-alias","target":"MacBook-Pro"}}}"#;
        assert_eq!(
            serde_json::from_slice::<Routes>(legacy)
                .unwrap()
                .routes
                .get("Mac")
                .unwrap()
                .remote_core
                .as_deref(),
            None
        );
        assert!(validate_ssh_host("-oProxyCommand=evil").is_err());
        assert!(validate_endpoint("host:0").is_err());
        assert!(validate_endpoint("host name:22").is_err());
        assert_eq!(
            split_endpoint("127.0.0.1:48765").unwrap(),
            ("127.0.0.1", 48765)
        );
    }

    #[test]
    fn target_config_and_machine_fabric_forward_round_trip() {
        let target = TargetSettings {
            endpoint: "127.0.0.1:48766".into(),
            server_name: "workbench-relay".into(),
            ca_certificate: "/tmp/ca.pem".into(),
            token_file: "/tmp/mac.token".into(),
            machine_fabric_tunnel: Some(MachineFabricTunnel {
                executor_id: "mac-executor".into(),
                ssh_host: "devbox-alias".into(),
                tunnel_id: "workbench-test".into(),
                forward_to: "127.0.0.1:48765".into(),
            }),
        };
        let encoded = serde_json::to_vec(&target).unwrap();
        assert_eq!(
            serde_json::from_slice::<TargetSettings>(&encoded).unwrap(),
            target
        );

        let params = tunnel_ensure_params(
            &target.endpoint,
            target.machine_fabric_tunnel.as_ref().unwrap(),
            Path::new("/tmp/workbench-state"),
        )
        .unwrap();
        assert_eq!(params["direction"], "local-forward");
        assert_eq!(params["bindHost"], "127.0.0.1");
        assert_eq!(params["bindPort"], 48766);
        assert_eq!(params["targetHost"], "127.0.0.1");
        assert_eq!(params["targetPort"], 48765);
        assert!(
            params["logPath"]
                .as_str()
                .unwrap()
                .ends_with("workbench-test.log")
        );
    }
}
