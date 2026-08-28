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

fn run(cli: Cli) -> Result<(), Box<dyn std::error::Error>> {
    let paths = Paths::discover()?;
    match cli.command {
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
            let server = ServerIdentity::discover()?;
            let agent = parse_agent_kind(&agent)?;
            let input = tmux_agent_workbench::hooks::read_stdin()?;
            tmux_agent_workbench::hooks::ingest(&paths, &server, agent, &event, &input)?;
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
    let source_client = source_pane
        .and_then(|pane| tmux_format(&server, pane, "#{client_name}").ok())
        .filter(|client| !client.is_empty());
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
        commands.push(vec!["select-pane", "-t", pane]);
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
            .args(["select-pane", "-t", source_pane, "-l"])
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
    fn sidebar_jump_restores_only_when_leaving_its_window() {
        assert!(should_restore_source_window("@1", "@2"));
        assert!(!should_restore_source_window("@1", "@1"));
        assert!(!should_restore_source_window("", "@2"));
        assert!(!should_restore_source_window("@1", ""));
    }
}
