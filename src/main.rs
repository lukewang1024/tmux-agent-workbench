use std::collections::HashMap;
use std::fs::{self, OpenOptions};
use std::os::unix::fs::OpenOptionsExt;
use std::process::{Command as ProcessCommand, ExitCode, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use clap::{Parser, Subcommand};
use tmux_agent_workbench::config::Config;
use tmux_agent_workbench::daemon;
use tmux_agent_workbench::ipc::{Request, call, exchange};
use tmux_agent_workbench::manifest::ManifestSet;
use tmux_agent_workbench::model::{AgentKind, AgentSnapshot, ConversationRole, DisplayState};
use tmux_agent_workbench::paths::Paths;
use tmux_agent_workbench::server::ServerIdentity;

#[derive(Debug, Parser)]
#[command(name = "tmux-agent-workbench", version, about)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    Status {
        #[arg(long, default_value = "text")]
        format: String,
    },
    Client {
        #[command(subcommand)]
        command: ClientCommand,
    },
    Daemon {
        #[command(subcommand)]
        command: DaemonCommand,
    },
    Snapshot {
        #[arg(long)]
        json: bool,
    },
    Sidebar,
    #[command(hide = true)]
    StatusMenu {
        kind: tmux_agent_workbench::status_menu::StatusMenuKind,
        #[arg(long)]
        pane: String,
    },
    Pick {
        #[command(subcommand)]
        target: PickTarget,
    },
    Attention {
        #[command(subcommand)]
        command: AttentionCommand,
    },
    Agent {
        #[command(subcommand)]
        command: AgentCommand,
    },
    Metadata {
        #[command(subcommand)]
        command: MetadataCommand,
    },
    Config {
        #[command(subcommand)]
        command: ConfigCommand,
    },
    Reload,
    Doctor,
    Workspace {
        #[command(subcommand)]
        command: WorkspaceCommand,
    },
    Hooks {
        #[command(subcommand)]
        command: HooksCommand,
    },
    Hook {
        #[command(subcommand)]
        command: HookCommand,
    },
    Relay {
        #[command(subcommand)]
        command: RelayCommand,
    },
    #[command(hide = true)]
    Focus {
        #[arg(long)]
        session: String,
        #[arg(long)]
        window: Option<String>,
        #[arg(long)]
        pane: Option<String>,
        #[arg(long)]
        source_pane: Option<String>,
        #[arg(long)]
        responsive: bool,
    },
    #[command(hide = true)]
    SidebarControl {
        action: String,
        target: Option<String>,
        #[arg(long)]
        create_only: bool,
    },
}

#[derive(Debug, Subcommand)]
enum ClientCommand {
    Serve,
    Status,
    Setup {
        platform: Option<String>,
    },
    Attach {
        ssh_host: Option<String>,
        #[arg(long)]
        session: Option<String>,
        #[arg(long)]
        target: Option<String>,
    },
    #[command(hide = true)]
    AttachPty {
        #[arg(long)]
        bind: String,
        #[arg(long)]
        session: Option<String>,
    },
}

#[derive(Debug, Subcommand)]
enum WorkspaceCommand {
    Status {
        #[arg(long)]
        json: bool,
    },
}

#[derive(Debug, Subcommand)]
enum DaemonCommand {
    Ensure,
    Status,
    Stop,
    #[command(hide = true)]
    Wake,
    #[command(hide = true)]
    Run,
}

#[derive(Debug, Subcommand)]
enum PickTarget {
    Session,
    Agent,
}

#[derive(Debug, Subcommand)]
enum AttentionCommand {
    Next,
}

#[derive(Debug, Subcommand)]
enum AgentCommand {
    Explain {
        pane: String,
        #[arg(long)]
        show_content: bool,
    },
}

#[derive(Debug, Subcommand)]
enum MetadataCommand {
    Report {
        #[arg(long)]
        pane: Option<String>,
        #[arg(long)]
        kind: Option<String>,
        #[arg(long)]
        label: Option<String>,
        #[arg(long = "session-id")]
        session_id: Option<String>,
        #[arg(long = "reason-hint")]
        reason_hint: Option<String>,
        #[arg(long = "conversation-id")]
        conversation_id: Option<String>,
        #[arg(long = "conversation-role")]
        conversation_role: Option<String>,
        #[arg(long = "conversation-label")]
        conversation_label: Option<String>,
        #[arg(long = "conversation-state")]
        conversation_state: Option<String>,
        #[arg(long = "conversation-active", default_value_t = false)]
        conversation_active: bool,
        #[arg(long, default_value_t = 5_000)]
        ttl_ms: u64,
    },
}

#[derive(Debug, Subcommand)]
enum ConfigCommand {
    Check,
}

#[derive(Debug, Subcommand)]
enum HooksCommand {
    Install {
        #[arg(default_value = "all")]
        target: String,
    },
    Check {
        #[arg(default_value = "all")]
        target: String,
    },
    Remove {
        #[arg(default_value = "all")]
        target: String,
    },
}

#[derive(Debug, Subcommand)]
enum HookCommand {
    Ingest { agent: String, event: String },
}

#[derive(Debug, Subcommand)]
enum RelayCommand {
    Serve,
    Pair {
        ssh_host: String,
    },
    Revoke {
        ssh_host: String,
    },
    Rotate {
        ssh_host: String,
    },
    Doctor {
        ssh_host: Option<String>,
    },
    #[command(hide = true)]
    AcceptPair {
        #[arg(long)]
        remote_id: String,
        #[arg(long)]
        token: String,
        #[arg(long)]
        endpoint: String,
    },
    #[command(hide = true)]
    RevokeLocal {
        #[arg(long)]
        remote_id: String,
    },
    #[command(hide = true)]
    FocusClick {
        #[arg(long)]
        remote_id: String,
        #[arg(long)]
        tmux_socket: String,
        #[arg(long)]
        session_id: String,
        #[arg(long)]
        pane_id: String,
    },
    #[command(hide = true)]
    FocusTarget {
        #[arg(long)]
        tmux_socket: String,
        #[arg(long)]
        session_id: String,
        #[arg(long)]
        pane_id: String,
    },
    #[command(hide = true)]
    Probe {
        #[arg(long)]
        endpoint: String,
    },
}

fn main() -> ExitCode {
    match run(Cli::parse()) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("tmux-agent-workbench: {error}");
            ExitCode::FAILURE
        }
    }
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

fn client_serve(paths: &Paths) -> Result<(), Box<dyn std::error::Error>> {
    use tmux_agent_workbench::client_protocol::{ClientMessage, read_frame, write_frame};
    let hello = read_frame(std::io::stdin().lock())?;
    let ClientMessage::Hello {
        device_id,
        device_label,
        kind,
        capabilities,
        ..
    } = hello
    else {
        return Err("client channel must begin with hello".into());
    };
    uuid::Uuid::parse_str(&device_id).map_err(|_| "invalid device id")?;
    ensure_daemon_quiet(paths)?;
    let registration = ipc_call(
        paths,
        "client.register",
        serde_json::json!({"device_id": device_id, "device_label": device_label, "kind": kind, "capabilities": capabilities}),
    )?;
    let endpoint_id = registration
        .get("endpoint_id")
        .and_then(|value| value.as_str())
        .ok_or("daemon returned no endpoint id")?
        .to_owned();
    let token = registration
        .get("attachment_token")
        .and_then(|value| value.as_str())
        .ok_or("daemon returned no attachment token")?
        .to_owned();
    write_frame(
        std::io::stdout().lock(),
        &ClientMessage::Welcome {
            version: tmux_agent_workbench::CLIENT_PROTOCOL_VERSION,
            endpoint_id: endpoint_id.clone(),
            heartbeat_seconds: 15,
            attachment_token: token.clone(),
        },
    )?;
    loop {
        match read_frame(std::io::stdin().lock()) {
            Ok(ClientMessage::Goodbye { .. }) => {
                let _ = ipc_call(
                    paths,
                    "client.detach",
                    serde_json::json!({"endpoint_id": endpoint_id}),
                );
                break;
            }
            Ok(ClientMessage::Heartbeat {
                activity_unix_ms, ..
            }) => {
                let result = ipc_call(
                    paths,
                    "client.heartbeat",
                    serde_json::json!({"endpoint_id": endpoint_id, "activity_unix_ms": activity_unix_ms}),
                )?;
                let events: Vec<tmux_agent_workbench::semantic::SemanticEvent> =
                    serde_json::from_value(
                        result
                            .get("events")
                            .cloned()
                            .unwrap_or_else(|| serde_json::json!([])),
                    )?;
                for event in &events {
                    write_frame(
                        std::io::stdout().lock(),
                        &ClientMessage::EventDelivery {
                            version: tmux_agent_workbench::CLIENT_PROTOCOL_VERSION,
                            event_id: event.id.clone(),
                            category: event.category.name().into(),
                            title: event.title.clone(),
                            body: event.body.clone(),
                            target: event.target.clone(),
                        },
                    )?;
                }
                write_frame(
                    std::io::stdout().lock(),
                    &ClientMessage::HeartbeatAck {
                        version: tmux_agent_workbench::CLIENT_PROTOCOL_VERSION,
                        events: events.len() as u32,
                    },
                )?;
            }
            Ok(ClientMessage::FocusResult {
                focused,
                active_pane,
                ..
            }) => {
                let target = active_pane.and_then(|pane_id| current_target_for_pane(&pane_id).ok());
                ipc_call(
                    paths,
                    "client.focus",
                    serde_json::json!({"endpoint_id": endpoint_id, "focused": focused, "overlay_visible": false, "target": target}),
                )?;
            }
            Ok(ClientMessage::EventAccepted { event_id, .. }) => {
                ipc_call(
                    paths,
                    "client.accepted",
                    serde_json::json!({"endpoint_id": endpoint_id, "event_id": event_id}),
                )?;
            }
            Ok(ClientMessage::FocusTarget {
                event_id, target, ..
            }) => {
                if let Err(error) = focus_attached_client(paths, &endpoint_id, &event_id, &target) {
                    eprintln!("tmux-agent-workbench: notification focus failed: {error}");
                }
            }
            Ok(ClientMessage::ClipboardWrite {
                request_id, text, ..
            }) => {
                let result = platform_clipboard_write(&text).err();
                write_frame(
                    std::io::stdout().lock(),
                    &ClientMessage::ClipboardResult {
                        version: tmux_agent_workbench::CLIENT_PROTOCOL_VERSION,
                        request_id,
                        text: None,
                        error: result,
                    },
                )?;
            }
            Ok(ClientMessage::ClipboardRead { request_id, .. }) => {
                let (text, error) = match platform_clipboard_read() {
                    Ok(text) => (Some(text), None),
                    Err(error) => (None, Some(error)),
                };
                write_frame(
                    std::io::stdout().lock(),
                    &ClientMessage::ClipboardResult {
                        version: tmux_agent_workbench::CLIENT_PROTOCOL_VERSION,
                        request_id,
                        text,
                        error,
                    },
                )?;
            }
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::UnexpectedEof => break,
            Err(error) => return Err(error.into()),
        }
    }
    Ok(())
}

fn focus_attached_client(
    paths: &Paths,
    endpoint_id: &str,
    event_id: &str,
    target: &tmux_agent_workbench::model::TmuxTarget,
) -> Result<(), Box<dyn std::error::Error>> {
    validate_safe_name(event_id, "event id")?;
    validate_target(&target.session_id, '$')?;
    validate_target(&target.window_id, '@')?;
    validate_target(&target.pane_id, '%')?;
    let attachment = ipc_call(
        paths,
        "client.attachment",
        serde_json::json!({"endpoint_id": endpoint_id}),
    )?
    .get("attachment")
    .and_then(|value| value.as_str())
    .ok_or("client attachment is unavailable")?
    .to_owned();
    let server = ServerIdentity::discover()?;
    for args in [
        vec![
            "switch-client",
            "-c",
            attachment.as_str(),
            "-t",
            target.window_id.as_str(),
        ],
        vec!["select-pane", "-Z", "-t", target.pane_id.as_str()],
    ] {
        if !ProcessCommand::new("tmux")
            .arg("-S")
            .arg(&server.socket_path)
            .args(args)
            .status()?
            .success()
        {
            return Err(format!("tmux notification target expired: {event_id}").into());
        }
    }
    Ok(())
}

fn click_path(paths: &Paths, event_id: &str) -> std::path::PathBuf {
    paths.runtime_dir.join("clicks").join(event_id)
}

fn termux_click_action(
    paths: &Paths,
    event_id: &str,
    am: &std::path::Path,
) -> Result<std::path::PathBuf, Box<dyn std::error::Error>> {
    validate_safe_name(event_id, "event id")?;
    let click = click_path(paths, event_id);
    fs::create_dir_all(click.parent().ok_or("click directory is unavailable")?)?;
    let directory = paths.runtime_dir.join("notification-actions");
    fs::create_dir_all(&directory)?;
    let action = directory.join(event_id);
    let script = format!(
        "#!/system/bin/sh\n{} start --user 0 -n com.termux/.app.TermuxActivity >/dev/null 2>&1\n: > {}\n",
        shell_quote(&am.to_string_lossy()),
        shell_quote(&click.to_string_lossy())
    );
    fs::write(&action, script)?;
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(&action, fs::Permissions::from_mode(0o700))?;
    Ok(action)
}

fn client_status(paths: &Paths) -> Result<(), Box<dyn std::error::Error>> {
    println!("client-protocol-v2\ndevice-id: {}", device_id(paths)?);
    let termux_notification = std::env::var_os("TERMUX_VERSION").is_some()
        && ProcessCommand::new("termux-notification")
            .arg("--help")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .is_ok_and(|status| status.success());
    println!(
        "notification: {}",
        if termux_notification
            || (std::env::var_os("TERMUX_VERSION").is_none()
                && (cfg!(target_os = "macos")
                    || cfg!(target_os = "linux")
                    || cfg!(target_os = "windows")))
        {
            "available"
        } else {
            "unavailable"
        }
    );
    println!(
        "clipboard: {}",
        if platform_clipboard_read().is_ok() {
            "available"
        } else {
            "unavailable"
        }
    );
    Ok(())
}

fn client_setup(paths: &Paths, platform: Option<&str>) -> Result<(), Box<dyn std::error::Error>> {
    let platform = platform.unwrap_or(std::env::consts::OS);
    match platform {
        "windows" => {
            if std::env::var_os("WSL_DISTRO_NAME").is_none() && !cfg!(target_os = "windows") {
                return Err("Windows setup must run from WSL or Windows".into());
            }
            fs::create_dir_all(&paths.cache_dir)?;
            let script = paths.cache_dir.join("setup-windows.ps1");
            fs::write(&script, include_str!("../assets/setup-windows.ps1"))?;
            let windows_path = if cfg!(target_os = "windows") {
                script.display().to_string()
            } else {
                let output = ProcessCommand::new("wslpath")
                    .args(["-w", script.to_str().ok_or("setup path is not UTF-8")?])
                    .output()?;
                if !output.status.success() {
                    return Err("could not convert setup path for Windows".into());
                }
                String::from_utf8(output.stdout)?.trim().into()
            };
            let status = ProcessCommand::new("powershell.exe")
                .args([
                    "-NoProfile",
                    "-ExecutionPolicy",
                    "Bypass",
                    "-File",
                    &windows_path,
                    tmux_agent_workbench::ENGINE_VERSION,
                ])
                .status()?;
            if status.success() {
                println!("Windows companion installed under %LOCALAPPDATA%\\tmux-agent-workbench");
                Ok(())
            } else {
                Err("Windows companion setup failed".into())
            }
        }
        "termux" | "android" => {
            if ProcessCommand::new("termux-notification")
                .arg("--help")
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()
                .is_ok_and(|status| status.success())
            {
                println!("Termux:API capabilities available");
            } else {
                println!("Termux:API not found; SSH, popup, and navigation remain available");
            }
            let properties = dirs::home_dir()
                .ok_or("home directory unavailable")?
                .join(".termux/termux.properties");
            if !termux_external_apps_enabled(&properties) {
                println!(
                    "Warning: set allow-external-apps=true in {} and run termux-reload-settings for notification clicks",
                    properties.display()
                );
            }
            Ok(())
        }
        "macos" | "linux" => {
            println!("No privileged setup required for {platform}");
            Ok(())
        }
        _ => Err(format!("unsupported client setup platform: {platform}").into()),
    }
}

fn client_attach(
    paths: &Paths,
    ssh_host: Option<&str>,
    session: Option<&str>,
    target: Option<&str>,
) -> Result<(), Box<dyn std::error::Error>> {
    if let Some(value) = session {
        validate_safe_name(value, "session")?;
    }
    if let Some(value) = target {
        validate_safe_name(value, "event target")?;
    }
    let Some(host) = ssh_host else {
        let mut command = ProcessCommand::new("tmux");
        command.arg("attach-session");
        if let Some(session) = session {
            command.args(["-t", session]);
        }
        return command
            .status()
            .and_then(|status| {
                if status.success() {
                    Ok(())
                } else {
                    Err(std::io::Error::other("tmux attach failed"))
                }
            })
            .map_err(Into::into);
    };
    validate_ssh_host(host)?;
    let mut control = ProcessCommand::new("ssh")
        .args(["-o", "ClearAllForwardings=yes"])
        .arg(host)
        .arg("tmux_socket=$(tmux display-message -p '#{socket_path}') && export TMUX_AGENT_WORKBENCH_TMUX_SOCKET=\"$tmux_socket\" && exec tmux-agent-workbench client serve")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()?;
    use tmux_agent_workbench::client_protocol::{ClientMessage, read_frame, write_frame};
    let id = device_id(paths)?;
    let kind = if cfg!(target_os = "macos") {
        "macos"
    } else if std::env::var_os("TERMUX_VERSION").is_some() {
        "termux"
    } else if std::env::var_os("WSL_DISTRO_NAME").is_some() {
        "wsl"
    } else {
        std::env::consts::OS
    };
    let label = std::env::var("HOSTNAME").unwrap_or_else(|_| kind.into());
    write_frame(
        control.stdin.as_mut().ok_or("control stdin unavailable")?,
        &ClientMessage::Hello {
            version: tmux_agent_workbench::CLIENT_PROTOCOL_VERSION,
            device_id: id,
            device_label: label,
            kind: kind.into(),
            capabilities: vec![
                "notification".into(),
                "sound".into(),
                "clipboard".into(),
                "focus".into(),
            ],
        },
    )?;
    let welcome = read_frame(
        control
            .stdout
            .as_mut()
            .ok_or("control stdout unavailable")?,
    )?;
    let ClientMessage::Welcome {
        attachment_token, ..
    } = welcome
    else {
        return Err("remote rejected client hello".into());
    };
    validate_safe_name(&attachment_token, "attachment token")?;
    let mut remote = format!(
        "tmux_socket=$(tmux display-message -p '#{{socket_path}}') && export TMUX_AGENT_WORKBENCH_TMUX_SOCKET=\"$tmux_socket\" && exec tmux-agent-workbench client attach-pty --bind {attachment_token}"
    );
    if let Some(session) = session {
        remote.push_str(" --session ");
        remote.push_str(session);
    }
    let mut pty = ProcessCommand::new("ssh")
        .args(["-o", "ClearAllForwardings=yes", "-t", host, &remote])
        .spawn()?;
    let mut event_targets: HashMap<String, tmux_agent_workbench::model::TmuxTarget> =
        HashMap::new();
    let mut next_heartbeat = Instant::now() + Duration::from_secs(15);
    let status = loop {
        if let Some(status) = pty.try_wait()? {
            break status;
        }
        if let Some(stdin) = control.stdin.as_mut() {
            for (event_id, target) in &event_targets {
                if fs::remove_file(click_path(paths, event_id)).is_ok() {
                    write_frame(
                        &mut *stdin,
                        &ClientMessage::FocusTarget {
                            version: tmux_agent_workbench::CLIENT_PROTOCOL_VERSION,
                            event_id: event_id.clone(),
                            target: target.clone(),
                        },
                    )?;
                }
            }
        }
        if Instant::now() < next_heartbeat {
            thread::sleep(Duration::from_millis(100));
            continue;
        }
        next_heartbeat = Instant::now() + Duration::from_secs(15);
        if let Some(stdin) = control.stdin.as_mut() {
            if write_frame(
                &mut *stdin,
                &ClientMessage::Heartbeat {
                    version: tmux_agent_workbench::CLIENT_PROTOCOL_VERSION,
                    activity_unix_ms: now_ms(),
                },
            )
            .is_err()
            {
                break pty.wait()?;
            }
            loop {
                match read_frame(
                    control
                        .stdout
                        .as_mut()
                        .ok_or("control stdout unavailable")?,
                )? {
                    ClientMessage::EventDelivery {
                        event_id,
                        category,
                        title,
                        body,
                        target,
                        ..
                    } => {
                        event_targets.insert(event_id.clone(), target);
                        platform_notify(paths, &event_id, &category, &title, &body)?;
                        let _ = write_frame(
                            &mut *stdin,
                            &ClientMessage::EventAccepted {
                                version: tmux_agent_workbench::CLIENT_PROTOCOL_VERSION,
                                event_id,
                            },
                        );
                    }
                    ClientMessage::HeartbeatAck { .. } => break,
                    _ => {}
                }
            }
        }
    };
    if let Some(stdin) = control.stdin.as_mut() {
        let _ = write_frame(
            stdin,
            &ClientMessage::Goodbye {
                version: tmux_agent_workbench::CLIENT_PROTOCOL_VERSION,
            },
        );
    }
    let _ = control.wait();
    if status.success() {
        Ok(())
    } else {
        Err("remote tmux attach failed".into())
    }
}

fn client_attach_pty(
    paths: &Paths,
    token: &str,
    session: Option<&str>,
) -> Result<(), Box<dyn std::error::Error>> {
    uuid::Uuid::parse_str(token).map_err(|_| "invalid attachment token")?;
    if let Some(value) = session {
        validate_safe_name(value, "session")?;
    }
    ensure_daemon_quiet(paths)?;
    let tty = std::env::var("SSH_TTY")
        .or_else(|_| std::env::var("TTY"))
        .unwrap_or_else(|_| "unknown".into());
    ipc_call(
        paths,
        "client.bind",
        serde_json::json!({"token": token, "attachment": tty}),
    )?;
    let mut command = ProcessCommand::new("tmux");
    command.arg("attach-session");
    if let Some(session) = session {
        command.args(["-t", session]);
    }
    let status = command.status()?;
    if status.success() {
        Ok(())
    } else {
        Err("tmux attach failed".into())
    }
}

fn current_target_for_pane(
    pane_id: &str,
) -> Result<tmux_agent_workbench::model::TmuxTarget, Box<dyn std::error::Error>> {
    validate_target(pane_id, '%')?;
    let server = ServerIdentity::discover()?;
    let format = "#{session_id}\u{1f}#{session_name}\u{1f}#{window_id}\u{1f}#{window_index}\u{1f}#{window_name}\u{1f}#{pane_id}\u{1f}#{pane_index}";
    let output = ProcessCommand::new("tmux")
        .arg("-S")
        .arg(&server.socket_path)
        .args(["display-message", "-p", "-t", pane_id, format])
        .output()?;
    if !output.status.success() {
        return Err("pane is not live".into());
    }
    let value = String::from_utf8(output.stdout)?;
    let fields: Vec<_> = value.trim().split('\u{1f}').collect();
    if fields.len() != 7 {
        return Err("invalid tmux target response".into());
    }
    Ok(tmux_agent_workbench::model::TmuxTarget {
        session_id: fields[0].into(),
        session_name: fields[1].into(),
        window_id: fields[2].into(),
        window_index: fields[3].parse()?,
        window_name: fields[4].into(),
        pane_id: fields[5].into(),
        pane_index: fields[6].parse()?,
    })
}

fn device_id(paths: &Paths) -> Result<String, Box<dyn std::error::Error>> {
    let data_root = std::env::var_os("XDG_DATA_HOME")
        .map(std::path::PathBuf::from)
        .or_else(|| dirs::home_dir().map(|home| home.join(".local/share")))
        .ok_or("home directory unavailable")?;
    let directory = data_root.join("tmux-agent-workbench/client");
    fs::create_dir_all(&directory)?;
    let path = directory.join("device-id");
    if let Ok(value) = fs::read_to_string(&path) {
        uuid::Uuid::parse_str(value.trim()).map_err(|_| "invalid stored device id")?;
        return Ok(value.trim().into());
    }
    let value = uuid::Uuid::new_v4().to_string();
    let mut file = OpenOptions::new()
        .create_new(true)
        .write(true)
        .mode(0o600)
        .open(path)?;
    use std::io::Write;
    writeln!(file, "{value}")?;
    let _ = paths;
    Ok(value)
}

fn validate_safe_name(value: &str, label: &str) -> Result<(), Box<dyn std::error::Error>> {
    if value.is_empty()
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
    validate_safe_name(host, "SSH host")
}

fn platform_clipboard_write(text: &str) -> Result<(), String> {
    tmux_agent_workbench::client_protocol::validate_clipboard(text)?;
    let (program, args): (&str, &[&str]) = if std::env::var_os("TERMUX_VERSION").is_some() {
        ("termux-clipboard-set", &[])
    } else if cfg!(target_os = "macos") {
        ("pbcopy", &[])
    } else if std::env::var_os("WSL_DISTRO_NAME").is_some() {
        ("clip.exe", &[])
    } else {
        ("xclip", &["-selection", "clipboard"])
    };
    let mut child = ProcessCommand::new(program)
        .args(args)
        .stdin(Stdio::piped())
        .spawn()
        .map_err(|error| error.to_string())?;
    use std::io::Write;
    child
        .stdin
        .as_mut()
        .ok_or("clipboard stdin unavailable")?
        .write_all(text.as_bytes())
        .map_err(|error| error.to_string())?;
    if child.wait().map_err(|error| error.to_string())?.success() {
        Ok(())
    } else {
        Err("clipboard helper failed".into())
    }
}

fn platform_clipboard_read() -> Result<String, String> {
    let (program, args): (&str, &[&str]) = if std::env::var_os("TERMUX_VERSION").is_some() {
        ("termux-clipboard-get", &[])
    } else if cfg!(target_os = "macos") {
        ("pbpaste", &[])
    } else if std::env::var_os("WSL_DISTRO_NAME").is_some() {
        (
            "powershell.exe",
            &["-NoProfile", "-Command", "Get-Clipboard -Raw"],
        )
    } else {
        ("xclip", &["-selection", "clipboard", "-o"])
    };
    let output = ProcessCommand::new(program)
        .args(args)
        .output()
        .map_err(|error| error.to_string())?;
    if !output.status.success() {
        return Err("clipboard helper failed".into());
    }
    let text = String::from_utf8(output.stdout).map_err(|_| "clipboard is not UTF-8")?;
    tmux_agent_workbench::client_protocol::validate_clipboard(&text)?;
    Ok(text)
}

fn platform_notify(
    paths: &Paths,
    event_id: &str,
    category: &str,
    title: &str,
    body: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    validate_safe_name(event_id, "event id")?;
    let status = if std::env::var_os("TERMUX_VERSION").is_some() {
        let am = command_path("am").ok_or("Android activity manager is unavailable")?;
        let action = termux_click_action(paths, event_id, &am)?;
        ProcessCommand::new("termux-notification")
            .args([
                "--id",
                event_id,
                "--title",
                title,
                "--content",
                body,
                "--action",
                &action.to_string_lossy(),
            ])
            .status()?
    } else if std::env::var_os("WSL_DISTRO_NAME").is_some() {
        ProcessCommand::new("wb-client.exe")
            .args(["notify", event_id, title, body])
            .status()?
    } else if cfg!(target_os = "macos") {
        let script = "on run argv\n display notification (item 3 of argv) with title (item 2 of argv)\nend run";
        ProcessCommand::new("osascript")
            .args(["-e", script, event_id, title, body])
            .status()?
    } else {
        let _ = paths;
        ProcessCommand::new("notify-send")
            .args(["--app-name", "Workbench", title, body])
            .status()?
    };
    if status.success() {
        Ok(())
    } else {
        Err(format!("notification delivery failed for {category}").into())
    }
}

fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\"'\"'"))
}

fn command_path(name: &str) -> Option<std::path::PathBuf> {
    std::env::var_os("PATH").and_then(|paths| {
        std::env::split_paths(&paths)
            .map(|directory| directory.join(name))
            .find(|path| path.is_file())
    })
}

fn termux_external_apps_enabled(path: &std::path::Path) -> bool {
    fs::read_to_string(path).is_ok_and(|contents| {
        contents.lines().any(|line| {
            let line = line.trim();
            let Some((key, value)) = line.split_once('=') else {
                return false;
            };
            key.trim() == "allow-external-apps" && value.trim() == "true"
        })
    })
}

fn run(cli: Cli) -> Result<(), Box<dyn std::error::Error>> {
    let paths = Paths::discover()?;
    match cli.command {
        Command::Client { command } => match command {
            ClientCommand::Serve => client_serve(&paths)?,
            ClientCommand::Status => client_status(&paths)?,
            ClientCommand::Setup { platform } => client_setup(&paths, platform.as_deref())?,
            ClientCommand::Attach {
                ssh_host,
                session,
                target,
            } => client_attach(
                &paths,
                ssh_host.as_deref(),
                session.as_deref(),
                target.as_deref(),
            )?,
            ClientCommand::AttachPty { bind, session } => {
                client_attach_pty(&paths, &bind, session.as_deref())?
            }
        },
        Command::Status { format } => {
            let home = dirs::home_dir().ok_or("home directory unavailable")?;
            let registry = tmux_agent_workbench::workspace::Registry::new(paths.workspaces_dir());
            let workspaces = registry.lazy_migrate(&home.join("Workspace"), now_ms())?;
            let capabilities = [
                "client-protocol-v2",
                "status-fragments-v1",
                "responsive-popup-v1",
                "workspace-registry-v1",
            ];
            if format == "json" {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&serde_json::json!({
                        "version": tmux_agent_workbench::ENGINE_VERSION,
                        "snapshot_schema": 1,
                        "client_protocol": 1,
                        "capabilities": capabilities,
                        "workspaces": workspaces,
                    }))?
                );
            } else if format == "text" {
                println!(
                    "Workbench {} · {} workspaces",
                    tmux_agent_workbench::ENGINE_VERSION,
                    workspaces.len()
                );
                for workspace in workspaces {
                    println!(
                        "{}\t{}\t{}",
                        workspace.id,
                        workspace.name,
                        workspace.root.display()
                    );
                }
            } else {
                return Err("--format must be text or json".into());
            }
        }
        Command::Hooks { command } => {
            let (action, target) = match command {
                HooksCommand::Install { target } => ("install", target),
                HooksCommand::Check { target } => ("check", target),
                HooksCommand::Remove { target } => ("remove", target),
            };
            tmux_agent_workbench::hooks::manage(action, &target)?;
        }
        Command::Hook {
            command: HookCommand::Ingest { agent, event },
        } => {
            let agent = parse_agent_kind(&agent)?;
            let input = tmux_agent_workbench::hooks::read_stdin()?;
            match ServerIdentity::discover() {
                Ok(server) => {
                    tmux_agent_workbench::hooks::ingest(&paths, &server, agent, &event, &input)?
                }
                Err(tmux_agent_workbench::server::ServerError::NotInTmux)
                    if agent == tmux_agent_workbench::model::AgentKind::Codex =>
                {
                    tmux_agent_workbench::hooks::ingest_detached(&paths, agent, &event, &input)?
                }
                Err(error) => return Err(error.into()),
            }
            println!("{{}}");
        }
        Command::Config {
            command: ConfigCommand::Check,
        } => {
            Config::load(&paths.config_file())?;
            ManifestSet::load(&paths.manifests_dir())?;
            println!("configuration valid: {}", paths.config_file().display());
        }
        Command::Snapshot { json } => {
            let value = ipc_call(&paths, "snapshot.get", serde_json::Value::Null)?;
            if json {
                println!("{}", serde_json::to_string_pretty(&value)?);
            } else {
                let snapshot: tmux_agent_workbench::model::Snapshot =
                    serde_json::from_value(value)?;
                println!(
                    "{} agents, generation {}",
                    snapshot.agents.len(),
                    snapshot.generation
                );
            }
        }
        Command::Sidebar => {
            let server = ServerIdentity::discover()?;
            tmux_agent_workbench::sidebar::run(&paths, &server)?;
        }
        Command::StatusMenu { kind, pane } => {
            tmux_agent_workbench::status_menu::run(kind, &pane)?;
        }
        Command::Pick { target } => {
            let server = ServerIdentity::discover()?;
            let kind = match target {
                PickTarget::Session => tmux_agent_workbench::picker::PickerKind::Session,
                PickTarget::Agent => tmux_agent_workbench::picker::PickerKind::Agent,
            };
            tmux_agent_workbench::picker::run(&paths, &server, kind)?;
        }
        Command::Daemon {
            command: DaemonCommand::Run,
        } => {
            let server = ServerIdentity::discover()?;
            daemon::serve(&paths, &server)?;
        }
        Command::Daemon {
            command: DaemonCommand::Ensure,
        } => ensure_daemon(&paths)?,
        Command::Daemon {
            command: DaemonCommand::Status,
        } => {
            let value = ipc_call(&paths, "daemon.status", serde_json::Value::Null)?;
            println!("{}", serde_json::to_string_pretty(&value)?);
        }
        Command::Daemon {
            command: DaemonCommand::Stop,
        } => {
            let value = ipc_call(&paths, "daemon.stop", serde_json::Value::Null)?;
            println!("{}", serde_json::to_string(&value)?);
        }
        Command::Daemon {
            command: DaemonCommand::Wake,
        } => {
            // tmux lifecycle hooks can race the asynchronous `daemon ensure`
            // started while the plugin is loading, and the daemon may also
            // have exited between hooks. Make wake self-healing instead of
            // surfacing a harmless run-shell failure to the user.
            if ipc_call(&paths, "daemon.wake", serde_json::Value::Null).is_err() {
                ensure_daemon(&paths)?;
                ipc_call(&paths, "daemon.wake", serde_json::Value::Null)?;
            }
        }
        Command::Reload => {
            let value = ipc_call(&paths, "config.reload", serde_json::Value::Null)?;
            let _ = tmux_agent_workbench::layout::control(
                tmux_agent_workbench::layout::Action::Configure,
                None,
                false,
            );
            let _ = tmux_agent_workbench::layout::control(
                tmux_agent_workbench::layout::Action::EnsureAll,
                None,
                false,
            );
            println!("{}", serde_json::to_string(&value)?);
        }
        Command::Doctor => tmux_agent_workbench::doctor::run(&paths)?,
        Command::Workspace {
            command: WorkspaceCommand::Status { json },
        } => {
            let home = dirs::home_dir().ok_or("home directory unavailable")?;
            let registry = tmux_agent_workbench::workspace::Registry::new(paths.workspaces_dir());
            let workspaces = registry.lazy_migrate(&home.join("Workspace"), now_ms())?;
            if json {
                println!("{}", serde_json::to_string_pretty(&workspaces)?);
            } else {
                for workspace in workspaces {
                    println!(
                        "{}\t{}\t{}",
                        workspace.id,
                        workspace.name,
                        workspace.root.display()
                    );
                }
            }
        }
        Command::Relay { command } => match command {
            RelayCommand::Serve => {
                let config = Config::load(&paths.config_file())?;
                tmux_agent_workbench::relay::serve(&paths, &config)?;
            }
            RelayCommand::Pair { ssh_host } => {
                tmux_agent_workbench::relay::pair(&paths, &ssh_host)?
            }
            RelayCommand::Revoke { ssh_host } => {
                tmux_agent_workbench::relay::revoke(&paths, &ssh_host)?
            }
            RelayCommand::Rotate { ssh_host } => {
                tmux_agent_workbench::relay::rotate(&paths, &ssh_host)?
            }
            RelayCommand::Doctor { ssh_host } => {
                tmux_agent_workbench::relay::doctor(&paths, ssh_host.as_deref())?
            }
            RelayCommand::AcceptPair {
                remote_id,
                token,
                endpoint,
            } => tmux_agent_workbench::relay::accept_pair(
                &paths,
                tmux_agent_workbench::relay::Outbound {
                    remote_id,
                    token,
                    endpoint,
                },
            )?,
            RelayCommand::RevokeLocal { remote_id } => {
                tmux_agent_workbench::relay::revoke_local(&paths, &remote_id)?
            }
            RelayCommand::FocusClick {
                remote_id,
                tmux_socket,
                session_id,
                pane_id,
            } => tmux_agent_workbench::relay::focus_click(
                &paths,
                &remote_id,
                &tmux_socket,
                &session_id,
                &pane_id,
            )?,
            RelayCommand::FocusTarget {
                tmux_socket,
                session_id,
                pane_id,
            } => tmux_agent_workbench::relay::focus_target(&tmux_socket, &session_id, &pane_id)?,
            RelayCommand::Probe { endpoint } => tmux_agent_workbench::relay::probe(&endpoint)?,
        },
        Command::Agent {
            command: AgentCommand::Explain { pane, show_content },
        } => {
            let value = ipc_call(
                &paths,
                "agent.explain",
                serde_json::json!({"pane_id": pane, "show_content": show_content}),
            )?;
            println!("{}", serde_json::to_string_pretty(&value)?);
        }
        Command::Attention {
            command: AttentionCommand::Next,
        } => attention_next(&paths)?,
        Command::Metadata {
            command:
                MetadataCommand::Report {
                    pane,
                    kind,
                    label,
                    session_id,
                    reason_hint,
                    conversation_id,
                    conversation_role,
                    conversation_label,
                    conversation_state,
                    conversation_active,
                    ttl_ms,
                },
        } => {
            let pane = pane
                .or_else(|| std::env::var("TMUX_PANE").ok())
                .ok_or("--pane is required when TMUX_PANE is unavailable")?;
            let kind = kind.map(|value| parse_agent_kind(&value)).transpose()?;
            let conversation_role = conversation_role
                .map(|value| parse_conversation_role(&value))
                .transpose()?;
            let conversation_state = conversation_state
                .map(|value| parse_display_state(&value))
                .transpose()?;
            ipc_call(
                &paths,
                "metadata.report",
                serde_json::json!({
                    "pane_id": pane,
                    "kind": kind,
                    "label": label,
                    "session_id": session_id,
                    "reason_hint": reason_hint,
                    "conversation_id": conversation_id,
                    "conversation_role": conversation_role,
                    "conversation_label": conversation_label,
                    "conversation_state": conversation_state,
                    "conversation_active": conversation_active,
                    "ttl_ms": ttl_ms
                }),
            )?;
        }
        Command::Focus {
            session,
            window,
            pane,
            source_pane,
            responsive,
        } => focus_target(
            &session,
            window.as_deref(),
            pane.as_deref(),
            source_pane.as_deref(),
            responsive,
        )?,
        Command::SidebarControl {
            action,
            target,
            create_only,
        } => {
            let action = match action.as_str() {
                "configure" => tmux_agent_workbench::layout::Action::Configure,
                "toggle" => tmux_agent_workbench::layout::Action::Toggle,
                "toggle-all" => tmux_agent_workbench::layout::Action::ToggleAll,
                "ensure-all" => tmux_agent_workbench::layout::Action::EnsureAll,
                "maintain" => tmux_agent_workbench::layout::Action::Maintain,
                "remember" => tmux_agent_workbench::layout::Action::Remember,
                _ => return Err(format!("unknown sidebar action: {action}").into()),
            };
            tmux_agent_workbench::layout::control(action, target.as_deref(), create_only)?;
        }
    }
    Ok(())
}

fn attention_next(paths: &Paths) -> Result<(), Box<dyn std::error::Error>> {
    let value = ipc_call(paths, "attention.next", serde_json::Value::Null)?;
    let Some(agent) = serde_json::from_value::<Option<AgentSnapshot>>(value)? else {
        tmux_message("Workbench: no unseen attention")?;
        return Ok(());
    };
    if !agent.exited {
        focus_agent(&agent)?;
    } else {
        tmux_message(&format!(
            "Workbench: completed · {} · {}",
            agent.label,
            agent.reason_category.as_deref().unwrap_or("done")
        ))?;
    }
    if let Some(event) = agent.attention {
        ipc_call(
            paths,
            "attention.ack",
            serde_json::json!({"event_id": event.id}),
        )?;
    }
    Ok(())
}

fn tmux_message(message: &str) -> Result<(), Box<dyn std::error::Error>> {
    let server = ServerIdentity::discover()?;
    let status = ProcessCommand::new("tmux")
        .arg("-S")
        .arg(&server.socket_path)
        .args(["display-message", message])
        .status()?;
    if status.success() {
        Ok(())
    } else {
        Err("could not display tmux message".into())
    }
}

fn focus_agent(agent: &AgentSnapshot) -> Result<(), Box<dyn std::error::Error>> {
    focus_target(
        &agent.target.session_id,
        Some(&agent.target.window_id),
        Some(&agent.target.pane_id),
        None,
        true,
    )
}

fn focus_target(
    session: &str,
    window: Option<&str>,
    pane: Option<&str>,
    source_pane: Option<&str>,
    responsive: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    validate_target(session, '$')?;
    if let Some(window) = window {
        validate_target(window, '@')?;
    }
    if let Some(pane) = pane {
        validate_target(pane, '%')?;
    }
    if let Some(source_pane) = source_pane {
        validate_target(source_pane, '%')?;
    }
    let server = ServerIdentity::discover()?;
    let source_client = source_pane.and_then(|pane| client_for_pane(&server, pane));
    if let (Some(source_pane), Some(target_pane)) = (source_pane, pane) {
        restore_source_pane_before_jump(&server, source_pane, target_pane)?;
    }
    let client_output = ProcessCommand::new("tmux")
        .arg("-S")
        .arg(&server.socket_path)
        .args([
            "list-clients",
            "-F",
            "#{client_name}\u{1f}#{client_activity}\u{1f}#{session_id}\u{1f}#{client_tty}",
        ])
        .output()?;
    let clients = parse_focus_clients(&String::from_utf8_lossy(&client_output.stdout));
    let client = source_client
        .as_deref()
        .and_then(|name| clients.iter().find(|client| client.name == name))
        .or_else(|| {
            clients
                .iter()
                .filter(|client| client.session_id == session)
                .max_by_key(|client| client.activity)
        })
        .or_else(|| clients.iter().max_by_key(|client| client.activity));
    let mut switch = vec!["switch-client"];
    if let Some(client) = client {
        switch.extend(["-c", &client.name]);
    }
    switch.extend(["-t", window.unwrap_or(session)]);
    let mut commands = vec![switch];
    if let Some(pane) = pane {
        commands.push(select_pane_command(pane));
    }
    for args in commands {
        let status = ProcessCommand::new("tmux")
            .arg("-S")
            .arg(&server.socket_path)
            .args(args)
            .status()?;
        if !status.success() {
            return Err(format!("tmux focus target expired: {}", pane.unwrap_or(session)).into());
        }
    }
    if responsive {
        let focused_window = if let Some(client) = client {
            tmux_format_for_client(&server, &client.name, "#{window_id}")?
        } else if let Some(pane) = pane {
            tmux_format(&server, pane, "#{window_id}")?
        } else {
            tmux_format(&server, window.unwrap_or(session), "#{window_id}")?
        };
        tmux_agent_workbench::layout::maintain_responsive_focus(
            client.map(|client| client.name.as_str()),
            &focused_window,
        )?;
    }
    if !client.is_some_and(|client| tmux_agent_workbench::terminal::focus_tty(&client.tty)) {
        activate_terminal();
    }
    Ok(())
}

#[derive(Debug, PartialEq, Eq)]
struct FocusClient {
    name: String,
    activity: u64,
    session_id: String,
    tty: String,
}

fn parse_focus_clients(output: &str) -> Vec<FocusClient> {
    output
        .lines()
        .filter_map(|line| {
            let mut fields = line.split('\u{1f}');
            Some(FocusClient {
                name: fields.next()?.to_owned(),
                activity: fields.next()?.parse().ok()?,
                session_id: fields.next()?.to_owned(),
                tty: fields.next()?.to_owned(),
            })
        })
        .collect()
}

fn select_pane_command(pane: &str) -> Vec<&str> {
    // Selecting another pane normally expands a zoomed tmux window. Preserve
    // responsive single-pane mode while remaining a no-op when unzoomed.
    vec!["select-pane", "-Z", "-t", pane]
}

fn client_for_pane(server: &ServerIdentity, pane: &str) -> Option<String> {
    let output = ProcessCommand::new("tmux")
        .arg("-S")
        .arg(&server.socket_path)
        .args([
            "list-clients",
            "-F",
            "#{client_name}\u{1f}#{pane_id}\u{1f}#{client_activity}",
        ])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter_map(|line| {
            let mut fields = line.split('\u{1f}');
            let client = fields.next()?;
            let active_pane = fields.next()?;
            let activity = fields.next()?.parse::<u64>().ok()?;
            (active_pane == pane).then(|| (activity, client.to_owned()))
        })
        .max_by_key(|(activity, _)| *activity)
        .map(|(_, client)| client)
}

fn restore_source_pane_before_jump(
    server: &ServerIdentity,
    source_pane: &str,
    target_pane: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    let source_window = tmux_format(server, source_pane, "#{window_id}")?;
    let target_window = tmux_format(server, target_pane, "#{window_id}")?;
    if should_restore_source_window(&source_window, &target_window) {
        // A sidebar click makes the sidebar tmux's remembered active pane.
        // Restore the previous pane before leaving this window so returning
        // later does not unexpectedly land back in the sidebar.
        let _ = ProcessCommand::new("tmux")
            .arg("-S")
            .arg(&server.socket_path)
            .args(["select-pane", "-Z", "-t", source_pane, "-l"])
            .status();
    }
    Ok(())
}

fn tmux_format(
    server: &ServerIdentity,
    target: &str,
    format: &str,
) -> Result<String, Box<dyn std::error::Error>> {
    let output = ProcessCommand::new("tmux")
        .arg("-S")
        .arg(&server.socket_path)
        .args(["display-message", "-p", "-t", target, format])
        .output()?;
    if !output.status.success() {
        return Err("tmux target expired".into());
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_owned())
}

fn tmux_format_for_client(
    server: &ServerIdentity,
    client: &str,
    format: &str,
) -> Result<String, Box<dyn std::error::Error>> {
    let output = ProcessCommand::new("tmux")
        .arg("-S")
        .arg(&server.socket_path)
        .args(["display-message", "-p", "-c", client, format])
        .output()?;
    if !output.status.success() {
        return Err("tmux client expired".into());
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_owned())
}

fn should_restore_source_window(source_window: &str, target_window: &str) -> bool {
    !source_window.is_empty() && !target_window.is_empty() && source_window != target_window
}

fn validate_target(value: &str, prefix: char) -> Result<(), String> {
    if value
        .strip_prefix(prefix)
        .is_some_and(|rest| !rest.is_empty() && rest.bytes().all(|byte| byte.is_ascii_digit()))
    {
        Ok(())
    } else {
        Err(format!("invalid tmux target: {value}"))
    }
}

#[cfg(target_os = "macos")]
fn activate_terminal() {
    let bundle = match std::env::var("TERM_PROGRAM").as_deref() {
        Ok("iTerm.app") => Some("com.googlecode.iterm2"),
        Ok("Apple_Terminal") => Some("com.apple.Terminal"),
        Ok("ghostty") => Some("com.mitchellh.ghostty"),
        Ok("WarpTerminal") => Some("dev.warp.Warp-Stable"),
        _ => None,
    };
    if let Some(bundle) = bundle {
        let _ = ProcessCommand::new("open")
            .args(["-b", bundle])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn();
    }
}

#[cfg(not(target_os = "macos"))]
fn activate_terminal() {}

fn parse_agent_kind(value: &str) -> Result<AgentKind, String> {
    match value.to_ascii_lowercase().as_str() {
        "codex" => Ok(AgentKind::Codex),
        "claude" => Ok(AgentKind::Claude),
        "trae" | "traex" => Ok(AgentKind::Trae),
        "opencode" => Ok(AgentKind::Opencode),
        _ => Err(format!("unknown agent kind: {value}")),
    }
}

fn parse_conversation_role(value: &str) -> Result<ConversationRole, String> {
    match value.to_ascii_lowercase().as_str() {
        "main" => Ok(ConversationRole::Main),
        "side" => Ok(ConversationRole::Side),
        _ => Err(format!("unknown conversation role: {value}")),
    }
}

fn parse_display_state(value: &str) -> Result<DisplayState, String> {
    match value.to_ascii_lowercase().as_str() {
        "working" => Ok(DisplayState::Working),
        "blocked" => Ok(DisplayState::Blocked),
        "done" => Ok(DisplayState::Done),
        "idle" => Ok(DisplayState::Idle),
        "unknown" | "checking" => Ok(DisplayState::Unknown),
        _ => Err(format!("unknown conversation state: {value}")),
    }
}

fn ipc_call(
    paths: &Paths,
    method: &str,
    params: serde_json::Value,
) -> Result<serde_json::Value, Box<dyn std::error::Error>> {
    let server = ServerIdentity::discover()?;
    let socket = paths.socket_for_server(&server.key);
    Ok(call(
        &socket,
        &Request::new(method, params),
        Duration::from_secs(2),
    )?)
}

fn ensure_daemon(paths: &Paths) -> Result<(), Box<dyn std::error::Error>> {
    ensure_daemon_with_announcement(paths, true)
}

fn ensure_daemon_quiet(paths: &Paths) -> Result<(), Box<dyn std::error::Error>> {
    ensure_daemon_with_announcement(paths, false)
}

fn ensure_daemon_with_announcement(
    paths: &Paths,
    announce: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    use std::os::unix::process::CommandExt;

    let server = ServerIdentity::discover()?;
    let socket = paths.socket_for_server(&server.key);
    if let Ok(response) = exchange(
        &socket,
        &Request::new("daemon.status", serde_json::Value::Null),
        Duration::from_millis(500),
    ) {
        let version = response
            .result
            .as_ref()
            .and_then(|value| value.get("engine_version"))
            .and_then(|value| value.as_str())
            .unwrap_or_default();
        let daemon_pid = response
            .result
            .as_ref()
            .and_then(|value| value.get("pid"))
            .and_then(|value| value.as_u64())
            .and_then(|value| u32::try_from(value).ok());
        if response.protocol_version == tmux_agent_workbench::IPC_PROTOCOL_VERSION
            && version == tmux_agent_workbench::ENGINE_VERSION
            && daemon_pid.is_some_and(running_executable_matches)
        {
            let pid = daemon_pid
                .map(|value| value.to_string())
                .unwrap_or_else(|| "unknown".into());
            if announce {
                println!("daemon already running (pid {pid})");
            }
            return Ok(());
        }
        let _ = exchange(
            &socket,
            &Request {
                protocol_version: response.protocol_version,
                id: uuid::Uuid::new_v4().to_string(),
                method: "daemon.stop".into(),
                params: serde_json::Value::Null,
            },
            Duration::from_millis(500),
        );
        let deadline = Instant::now() + Duration::from_secs(2);
        while socket.exists() && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(25));
        }
        if socket.exists() {
            return Err("old daemon did not exit during upgrade handoff".into());
        }
    }

    let executable = std::env::current_exe()?;
    fs::create_dir_all(&paths.state_dir)?;
    let log = OpenOptions::new()
        .create(true)
        .append(true)
        .mode(0o600)
        .open(paths.log_for_server(&server.key))?;
    let mut command = ProcessCommand::new(executable);
    command
        .args(["daemon", "run"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::from(log));
    // tmux owns and reaps run-shell jobs. Put the long-lived daemon in its own
    // session so it survives the short `daemon ensure` launcher job.
    unsafe {
        command.pre_exec(|| {
            if libc::setsid() == -1 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    command.spawn()?;

    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        if let Ok(value) = call(
            &socket,
            &Request::new("daemon.status", serde_json::Value::Null),
            Duration::from_millis(250),
        ) {
            if announce {
                println!("daemon running (pid {})", value["pid"]);
            }
            return Ok(());
        }
        if Instant::now() >= deadline {
            return Err("daemon did not become ready within 3 seconds".into());
        }
        thread::sleep(Duration::from_millis(25));
    }
}

#[cfg(any(target_os = "linux", target_os = "android"))]
fn running_executable_matches(pid: u32) -> bool {
    use std::os::unix::fs::MetadataExt;

    let running = fs::metadata(format!("/proc/{pid}/exe"));
    let current = std::env::current_exe().and_then(fs::metadata);
    matches!((running, current), (Ok(running), Ok(current)) if
        running.dev() == current.dev() && running.ino() == current.ino())
}

#[cfg(not(any(target_os = "linux", target_os = "android")))]
fn running_executable_matches(_pid: u32) -> bool {
    // Engine/protocol checks remain the portable fallback. Platforms without
    // procfs should bump ENGINE_VERSION when the daemon method table changes.
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(any(target_os = "linux", target_os = "android"))]
    #[test]
    fn detects_the_current_and_missing_daemon_executable() {
        assert!(running_executable_matches(std::process::id()));
        assert!(!running_executable_matches(u32::MAX));
    }

    #[test]
    fn pane_selection_preserves_existing_zoom() {
        assert_eq!(
            select_pane_command("%42"),
            ["select-pane", "-Z", "-t", "%42"]
        );
    }

    #[test]
    fn sidebar_jump_restores_only_when_leaving_its_window() {
        assert!(should_restore_source_window("@1", "@2"));
        assert!(!should_restore_source_window("@1", "@1"));
        assert!(!should_restore_source_window("", "@2"));
        assert!(!should_restore_source_window("@1", ""));
    }

    #[test]
    fn parses_tmux_clients_with_session_and_terminal_tty() {
        assert_eq!(
            parse_focus_clients("/dev/ttys001\u{1f}42\u{1f}$3\u{1f}/dev/ttys001\n"),
            [FocusClient {
                name: "/dev/ttys001".into(),
                activity: 42,
                session_id: "$3".into(),
                tty: "/dev/ttys001".into(),
            }]
        );
    }

    #[test]
    fn shell_quotes_notification_click_executable() {
        assert_eq!(
            shell_quote("/tmp/work bench's/core"),
            "'/tmp/work bench'\"'\"'s/core'"
        );
    }

    #[test]
    fn termux_notification_action_is_self_contained() {
        let root = tempfile::tempdir().unwrap();
        let paths = Paths {
            config_dir: root.path().join("config"),
            state_dir: root.path().join("state"),
            cache_dir: root.path().join("cache"),
            runtime_dir: root.path().join("runtime"),
        };
        fs::create_dir_all(&paths.runtime_dir).unwrap();
        let action = termux_click_action(
            &paths,
            "codex.42",
            std::path::Path::new("/data/data/com.termux/files/usr/bin/am"),
        )
        .unwrap();
        let script = fs::read_to_string(action).unwrap();
        assert!(script.starts_with("#!/system/bin/sh\n"));
        assert!(script.contains("/data/data/com.termux/files/usr/bin/am"));
        assert!(script.contains(&click_path(&paths, "codex.42").display().to_string()));
    }

    #[test]
    fn detects_termux_external_app_permission_with_spacing() {
        let root = tempfile::tempdir().unwrap();
        let properties = root.path().join("termux.properties");
        fs::write(&properties, "# disabled\nallow-external-apps = true\n").unwrap();
        assert!(termux_external_apps_enabled(&properties));
        fs::write(&properties, "allow-external-apps=false\n").unwrap();
        assert!(!termux_external_apps_enabled(&properties));
    }
}
