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
    ensure_daemon(paths)?;
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
            version: 1,
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
                            version: 1,
                            event_id: event.id.clone(),
                            category: event.category.name().into(),
                            title: event.title.clone(),
                            body: event.body.clone(),
                        },
                    )?;
                }
                write_frame(
                    std::io::stdout().lock(),
                    &ClientMessage::HeartbeatAck {
                        version: 1,
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
            Ok(ClientMessage::ClipboardWrite {
                request_id, text, ..
            }) => {
                let result = platform_clipboard_write(&text).err();
                write_frame(
                    std::io::stdout().lock(),
                    &ClientMessage::ClipboardResult {
                        version: 1,
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
                        version: 1,
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

fn client_status(paths: &Paths) -> Result<(), Box<dyn std::error::Error>> {
    println!("client-protocol-v1\ndevice-id: {}", device_id(paths)?);
    println!(
        "notification: {}",
        if cfg!(target_os = "macos") || cfg!(target_os = "linux") || cfg!(target_os = "windows") {
            "available"
        } else {
            "unknown"
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
        .arg(host)
        .args(["tmux-agent-workbench", "client", "serve"])
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
            version: 1,
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
    let mut remote = format!("tmux-agent-workbench client attach-pty --bind {attachment_token}");
    if let Some(session) = session {
        remote.push_str(" --session ");
        remote.push_str(session);
    }
    let mut pty = ProcessCommand::new("ssh")
        .args(["-t", host, &remote])
        .spawn()?;
    let status = loop {
        if let Some(status) = pty.try_wait()? {
            break status;
        }
        thread::sleep(Duration::from_secs(15));
        if let Some(stdin) = control.stdin.as_mut() {
            if write_frame(
                &mut *stdin,
                &ClientMessage::Heartbeat {
                    version: 1,
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
                        ..
                    } => {
                        platform_notify(&event_id, &category, &title, &body)?;
                        let _ = write_frame(
                            &mut *stdin,
                            &ClientMessage::EventAccepted {
                                version: 1,
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
        let _ = write_frame(stdin, &ClientMessage::Goodbye { version: 1 });
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
    ensure_daemon(paths)?;
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
    event_id: &str,
    category: &str,
    title: &str,
    body: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    validate_safe_name(event_id, "event id")?;
    let status = if std::env::var_os("TERMUX_VERSION").is_some() {
        ProcessCommand::new("termux-notification")
            .args([
                "--id",
                event_id,
                "--title",
                title,
                "--content",
                body,
                "--action",
                &format!("wb attach --target {event_id}"),
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
                "client-protocol-v1",
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
        } => focus_target(
            &session,
            window.as_deref(),
            pane.as_deref(),
            source_pane.as_deref(),
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
    )
}

fn focus_target(
    session: &str,
    window: Option<&str>,
    pane: Option<&str>,
    source_pane: Option<&str>,
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
    activate_terminal();
    let client_output = ProcessCommand::new("tmux")
        .arg("-S")
        .arg(&server.socket_path)
        .args([
            "list-clients",
            "-F",
            "#{client_name}\u{1f}#{client_activity}",
        ])
        .output()?;
    let client = source_client.or_else(|| {
        String::from_utf8_lossy(&client_output.stdout)
            .lines()
            .filter_map(|line| {
                let (name, activity) = line.split_once('\u{1f}')?;
                Some((activity.parse::<u64>().ok()?, name.to_owned()))
            })
            .max_by_key(|(activity, _)| *activity)
            .map(|(_, name)| name)
    });
    let mut switch = vec!["switch-client"];
    if let Some(client) = client.as_deref() {
        switch.extend(["-c", client]);
    }
    switch.extend(["-t", session]);
    let mut commands = vec![switch];
    if let Some(window) = window {
        commands.push(vec!["select-window", "-t", window]);
    }
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
    Ok(())
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
        if response.protocol_version == tmux_agent_workbench::IPC_PROTOCOL_VERSION
            && version == tmux_agent_workbench::ENGINE_VERSION
        {
            let pid = response
                .result
                .as_ref()
                .and_then(|value| value.get("pid"))
                .map(ToString::to_string)
                .unwrap_or_else(|| "unknown".into());
            println!("daemon already running (pid {pid})");
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
            println!("daemon running (pid {})", value["pid"]);
            return Ok(());
        }
        if Instant::now() >= deadline {
            return Err("daemon did not become ready within 3 seconds".into());
        }
        thread::sleep(Duration::from_millis(25));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
