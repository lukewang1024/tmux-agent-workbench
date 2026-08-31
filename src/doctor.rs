use std::process::Command;
use std::time::Duration;

use crate::config::Config;
use crate::ipc::{Request, call};
use crate::manifest::ManifestSet;
use crate::paths::Paths;
use crate::server::ServerIdentity;

pub fn run(paths: &Paths) -> Result<(), Box<dyn std::error::Error>> {
    let mut hard_failures = 0;
    hard_failures += check_tmux();
    hard_failures += check_command("fzf-tmux", true, "install fzf (which provides fzf-tmux)");
    check_platform_delivery();
    match Config::load(&paths.config_file()).and_then(|_| {
        ManifestSet::load(&paths.manifests_dir()).map_err(|e| {
            crate::config::ConfigError::Validation(format!("manifest validation failed: {e}"))
        })
    }) {
        Ok(_) => println!("ok: configuration and manifests valid"),
        Err(error) => {
            hard_failures += 1;
            println!("error: {error}");
        }
    }
    println!("ok: config path {}", paths.config_file().display());
    println!("ok: state path {}", paths.state_dir.display());
    println!("ok: runtime path {}", paths.runtime_dir.display());
    for (target, status) in crate::hooks::check_all() {
        if status == "ok" {
            println!("ok: {} native hooks", target.label());
        } else {
            println!("warning: {} native hooks: {status}", target.label());
            println!(
                "  repair with: tmux-agent-workbench hooks install {}",
                target.label()
            );
        }
    }

    match ServerIdentity::discover() {
        Ok(server) => {
            println!("ok: tmux server {}", server.socket_path.display());
            match call(
                &paths.socket_for_server(&server.key),
                &Request::new("daemon.status", serde_json::Value::Null),
                Duration::from_millis(500),
            ) {
                Ok(status) => println!("ok: daemon pid {}", status["pid"]),
                Err(error) => {
                    hard_failures += 1;
                    println!("error: daemon unavailable: {error}");
                }
            }
            let legacy = tmux_output(&server, &["show-option", "-gqv", "@agent_sidebar_bin"])
                .unwrap_or_default();
            let legacy_hooks = tmux_output(&server, &["show-hooks", "-g"])
                .unwrap_or_default()
                .contains("tmux-agent-sidebar");
            let legacy_panes = tmux_output(
                &server,
                &["list-panes", "-a", "-F", "#{pane_start_command}"],
            )
            .unwrap_or_default()
            .contains("tmux-agent-sidebar");
            if !legacy.trim().is_empty() || legacy_hooks || legacy_panes {
                hard_failures += 1;
                println!("error: legacy tmux-agent-sidebar integration is still loaded");
                println!(
                    "  remove the old TPM item, old hook installer/updater entries, tmux hooks containing tmux-agent-sidebar, and obsolete @sidebar_notifications/@sidebar_ansi_theme options; then reload tmux"
                );
            } else {
                println!("ok: no legacy tmux-agent-sidebar loaded");
            }
        }
        Err(_) => println!("info: outside tmux; live daemon and legacy-plugin checks skipped"),
    }

    if hard_failures == 0 {
        Ok(())
    } else {
        Err(format!("doctor found {hard_failures} blocking issue(s)").into())
    }
}

fn check_tmux() -> usize {
    let output = Command::new("tmux").arg("-V").output();
    let Ok(output) = output else {
        println!("error: tmux not found (requires >= 3.2)");
        return 1;
    };
    let version = String::from_utf8_lossy(&output.stdout).trim().to_owned();
    let number = version.split_whitespace().nth(1).unwrap_or("");
    let parts: Vec<_> = number
        .split(|ch: char| !ch.is_ascii_digit())
        .filter(|part| !part.is_empty())
        .collect();
    let major = parts
        .first()
        .and_then(|part| part.parse::<u32>().ok())
        .unwrap_or(0);
    let minor = parts
        .get(1)
        .and_then(|part| part.parse::<u32>().ok())
        .unwrap_or(0);
    if (major, minor) < (3, 2) {
        println!("error: {version}; requires tmux >= 3.2");
        1
    } else {
        println!("ok: {version}");
        0
    }
}

fn check_command(name: &str, hard: bool, hint: &str) -> usize {
    if command_exists(name) {
        println!("ok: {name}");
        0
    } else {
        println!(
            "{}: {name} not found; {hint}",
            if hard { "error" } else { "warning" }
        );
        usize::from(hard)
    }
}

fn check_platform_delivery() {
    if std::env::var_os("TERMUX_VERSION").is_some() {
        let available = [
            "termux-notification",
            "termux-clipboard-get",
            "termux-clipboard-set",
        ]
        .iter()
        .all(|name| command_exists(name));
        if available {
            println!("ok: Termux:API notification and clipboard capabilities");
        } else {
            println!(
                "warning: Termux:API helpers missing; notification, sound, and clipboard are reduced, while SSH, popup, and navigation remain available"
            );
        }
        return;
    }
    if std::env::var_os("WSL_DISTRO_NAME").is_some() {
        if command_exists("wb-client.exe") {
            println!("ok: Windows companion available through WSL interop");
        } else {
            println!(
                "warning: Windows companion missing; run `tmux-agent-workbench client setup windows` explicitly"
            );
        }
        return;
    }
    #[cfg(target_os = "macos")]
    {
        check_command("osascript", false, "macOS overlays unavailable");
        check_command("afplay", false, "built-in sounds unavailable");
    }
    #[cfg(target_os = "linux")]
    {
        check_command("notify-send", false, "desktop notifications unavailable");
        if !["pw-play", "paplay", "aplay"]
            .iter()
            .any(|name| command_exists(name))
        {
            println!("warning: no supported audio player (pw-play, paplay, or aplay)");
        } else {
            println!("ok: Linux audio backend available");
        }
    }
}

fn command_exists(name: &str) -> bool {
    std::env::var_os("PATH")
        .is_some_and(|paths| std::env::split_paths(&paths).any(|path| path.join(name).is_file()))
}

fn tmux_output(server: &ServerIdentity, args: &[&str]) -> Result<String, std::io::Error> {
    let output = Command::new("tmux")
        .arg("-S")
        .arg(&server.socket_path)
        .args(args)
        .output()?;
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}
