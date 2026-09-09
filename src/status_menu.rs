use std::process::Command;

use clap::ValueEnum;
#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum StatusMenuKind {
    Host,
    Tmux,
    Agent,
}

#[derive(Debug)]
struct Action {
    label: String,
    key: char,
    command: ActionCommand,
}

#[derive(Debug)]
enum ActionCommand {
    Tmux(Vec<String>),
    Agent(String),
    Host(String),
}

pub fn run(
    kind: StatusMenuKind,
    pane: &str,
    client: &str,
    action: Option<&str>,
    page: usize,
) -> Result<(), Box<dyn std::error::Error>> {
    validate_pane(pane)?;
    let (title, actions) = actions(kind)?;
    if let Some(label) = action {
        let selected = actions
            .iter()
            .find(|action| action.label == label)
            .ok_or("menu action no longer available")?;
        return execute_action(selected, pane);
    }
    let clients = tmux_output(&[
        "list-clients",
        "-F",
        "#{client_name} #{client_width} #{client_height}",
    ])?;
    let size = clients
        .lines()
        .find_map(|line| {
            let mut fields = line.split_whitespace();
            (fields.next()? == client).then(|| {
                Some((
                    fields.next()?.parse::<usize>().ok()?,
                    fields.next()?.parse::<usize>().ok()?,
                ))
            })?
        })
        .ok_or("Workbench client disappeared")?;
    let page_size = size.1.saturating_sub(7).max(1);
    let page = page.min(actions.len().saturating_sub(1) / page_size);
    let start = page * page_size;
    let executable = std::env::current_exe()?;
    let kind_name = match kind {
        StatusMenuKind::Host => "host",
        StatusMenuKind::Tmux => "tmux",
        StatusMenuKind::Agent => "agent",
    };
    let base = format!(
        "{} status-menu {} --pane {} --client {}",
        shell_quote(&executable.to_string_lossy()),
        kind_name,
        shell_quote(pane),
        shell_quote(client)
    );
    let mut command = Command::new("tmux");
    command.args([
        "display-menu",
        "-M",
        "-O",
        "-C",
        "0",
        "-c",
        client,
        "-t",
        pane,
        "-b",
        "rounded",
        "-T",
        &format!(" {title} "),
        "-x",
        "C",
        "-y",
        "C",
    ]);
    for action in actions.iter().skip(start).take(page_size) {
        command.args([
            menu_label(&action.label, size.0),
            action.key.to_string(),
            format!(
                "run-shell -b {}",
                shell_quote(&format!("{base} --action {}", shell_quote(&action.label)))
            ),
        ]);
    }
    if page > 0 {
        command.args([
            "Previous page".into(),
            "[".into(),
            format!(
                "run-shell -b {}",
                shell_quote(&format!("{base} --page {}", page - 1))
            ),
        ]);
    }
    if start + page_size < actions.len() {
        command.args([
            "Next page".into(),
            "]".into(),
            format!(
                "run-shell -b {}",
                shell_quote(&format!("{base} --page {}", page + 1))
            ),
        ]);
    }
    // Escape already dismisses native menus; render its short name explicitly.
    command.args(["", "× close (Esc)", "", ""]);
    let status = command.status()?;
    match status.code() {
        Some(0 | 2) => Ok(()), // Repeated opening clicks may race with an existing menu.
        _ => Err("could not display status menu".into()),
    }
}

fn menu_label(label: &str, client_width: usize) -> String {
    // Leave room for the native border and shortcut. Escape tmux formats so
    // dynamic host names remain labels rather than executable format strings.
    label
        .chars()
        .take(client_width.saturating_sub(10).max(1))
        .collect::<String>()
        .replace('#', "##")
}

fn actions(
    kind: StatusMenuKind,
) -> Result<(&'static str, Vec<Action>), Box<dyn std::error::Error>> {
    Ok(match kind {
        StatusMenuKind::Tmux => (
            "tmux",
            vec![
                tmux_action("New window", 'c', &["new-window"]),
                tmux_action(
                    "Codex Auto",
                    'x',
                    &["new-window", "-n", "codex", "exec codex --approve-for-me"],
                ),
                tmux_action(
                    "Claude Auto",
                    'a',
                    &[
                        "new-window",
                        "-n",
                        "claude",
                        "exec claude --permission-mode auto",
                    ],
                ),
                tmux_action(
                    "Trae Auto",
                    't',
                    &[
                        "new-window",
                        "-n",
                        "trae",
                        "exec traex --permission-mode auto",
                    ],
                ),
                tmux_action(
                    "OpenCode",
                    'o',
                    &["new-window", "-n", "opencode", "exec opencode --auto"],
                ),
                tmux_action(
                    "VS Code",
                    'v',
                    &[
                        "new-window",
                        "-n",
                        "code",
                        "code .; exec ${SHELL:-/bin/sh} -l",
                    ],
                ),
                tmux_action("Split below", '-', &["split-window", "-v"]),
                tmux_action("Split right", '|', &["split-window", "-h"]),
                tmux_action("Choose window", 'w', &["choose-tree", "-Zw"]),
                tmux_action("Choose session", 's', &["choose-tree", "-Zs"]),
                tmux_action("Detach", 'd', &["detach-client"]),
            ],
        ),
        StatusMenuKind::Agent => (
            "Agent",
            vec![
                agent_action("/side", 's'),
                agent_action("/btw", 'b'),
                agent_action("/fork", 'f'),
            ],
        ),
        StatusMenuKind::Host => {
            let output = Command::new("ssh-connect")
                .args(["hosts", "list"])
                .output()?;
            if !output.status.success() {
                return Err("ssh-connect hosts list failed".into());
            }
            let actions = String::from_utf8(output.stdout)?
                .lines()
                .filter(|host| !host.is_empty())
                .take(35)
                .enumerate()
                .map(|(index, host)| Action {
                    label: host.to_owned(),
                    key: "123456789abcdefghijklmnopqrstuvwxyz"
                        .chars()
                        .nth(index)
                        .unwrap(),
                    command: ActionCommand::Host(host.to_owned()),
                })
                .collect();
            ("SSH", actions)
        }
    })
}

fn tmux_action(label: &str, key: char, args: &[&str]) -> Action {
    Action {
        label: label.into(),
        key,
        command: ActionCommand::Tmux(args.iter().map(|v| (*v).into()).collect()),
    }
}

fn agent_action(label: &str, key: char) -> Action {
    Action {
        label: label.into(),
        key,
        command: ActionCommand::Agent(label.into()),
    }
}

fn execute_action(action: &Action, pane: &str) -> Result<(), Box<dyn std::error::Error>> {
    let path = tmux_output(&["display-message", "-p", "-t", pane, "#{pane_current_path}"])?;
    let status = match &action.command {
        ActionCommand::Tmux(args) => {
            let mut command = Command::new("tmux");
            if matches!(
                args.first().map(String::as_str),
                Some("new-window" | "split-window")
            ) {
                command.arg(&args[0]);
                command.args(["-c", path.trim()]);
                command.args(&args[1..]);
            } else {
                command.args(args);
            }
            command.status()?
        }
        ActionCommand::Agent(text) => Command::new("tmux")
            .args(["send-keys", "-t", pane, "-l", &agent_input(text)])
            .status()?,
        ActionCommand::Host(host) => Command::new("tmux")
            .args([
                "new-window",
                "-c",
                path.trim(),
                "-n",
                host,
                &format!("exec ssh-connect connect {}", shell_quote(host)),
            ])
            .status()?,
    };
    if status.success() {
        Ok(())
    } else {
        Err(format!("menu action failed: {}", action.label).into())
    }
}

fn agent_input(command: &str) -> String {
    format!("{command} ")
}

fn tmux_output(args: &[&str]) -> Result<String, Box<dyn std::error::Error>> {
    let output = Command::new("tmux").args(args).output()?;
    if !output.status.success() {
        return Err("tmux command failed".into());
    }
    Ok(String::from_utf8(output.stdout)?)
}

fn validate_pane(pane: &str) -> Result<(), Box<dyn std::error::Error>> {
    if pane
        .strip_prefix('%')
        .is_some_and(|value| !value.is_empty() && value.chars().all(|ch| ch.is_ascii_digit()))
    {
        Ok(())
    } else {
        Err("invalid pane target".into())
    }
}

fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pane_targets_are_strict() {
        assert!(validate_pane("%12").is_ok());
        assert!(validate_pane("%12;kill-server").is_err());
    }

    #[test]
    fn shell_quotes_hosts() {
        assert_eq!(shell_quote("dev'box"), "'dev'\\''box'");
    }

    #[test]
    fn agent_shortcuts_prefill_without_submitting() {
        assert_eq!(agent_input("/side"), "/side ");
    }
}
