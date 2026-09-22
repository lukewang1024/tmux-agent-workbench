use crate::{
    menu_context::{Context, Result, executable, tmux},
    model::{AgentKind, BaseState},
    paths::Paths,
};
use clap::{Args, ValueEnum};
use std::process::Command;

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum StatusMenuKind {
    Host,
    Tmux,
    Agent,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum, Default)]
pub enum View {
    #[default]
    Root,
    Panes,
    Switch,
    Launch,
    Preset,
    Presentation,
    Ready,
    More,
    Team,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum, Default)]
pub enum Tool {
    #[default]
    Codex,
    Claude,
    Traex,
    Opencode,
}
impl Tool {
    fn name(self) -> &'static str {
        match self {
            Self::Codex => "codex",
            Self::Claude => "claude",
            Self::Traex => "traex",
            Self::Opencode => "opencode",
        }
    }
    fn label(self) -> &'static str {
        match self {
            Self::Codex => "Codex",
            Self::Claude => "Claude Code",
            Self::Traex => "Trae",
            Self::Opencode => "OpenCode",
        }
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum, Default)]
pub enum Preset {
    #[default]
    Single,
    SingleBudget,
    Team,
    TeamBudget,
}
impl Preset {
    fn is_team(self) -> bool {
        matches!(self, Self::Team | Self::TeamBudget)
    }
    fn available(self, tool: Tool) -> bool {
        match self {
            Self::Single => true,
            Self::SingleBudget => {
                crate::menu_launch::budget_argv(tool.name()).is_ok_and(|args| args.is_some())
            }
            Self::Team | Self::TeamBudget => executable("agent-team"),
        }
    }
    fn name(self) -> &'static str {
        match self {
            Self::Single => "single",
            Self::SingleBudget => "single-budget",
            Self::Team => "team",
            Self::TeamBudget => "team-budget",
        }
    }
    fn label(self) -> &'static str {
        match self {
            Self::Single => "Single",
            Self::SingleBudget => "Single Budget",
            Self::Team => "Team",
            Self::TeamBudget => "Team Budget",
        }
    }
}
#[derive(Debug, Clone, Args, Default)]
pub struct MenuOptions {
    #[arg(long, value_enum, default_value = "root")]
    pub view: View,
    #[arg(long, value_enum, default_value = "codex")]
    pub tool: Tool,
    #[arg(long, value_enum, default_value = "single")]
    pub preset: Preset,
    #[arg(long)]
    pub panes: bool,
    #[arg(long)]
    pub guard: Option<String>,
}
impl MenuOptions {
    fn route(&self) -> String {
        format!(
            "--view {} --tool {} --preset {}{}",
            self.view.to_possible_value().unwrap().get_name(),
            self.tool.name(),
            self.preset.name(),
            if self.panes { " --panes" } else { "" }
        )
    }
    fn at(&self, view: View) -> Self {
        Self {
            view,
            ..self.clone()
        }
    }
}
#[derive(Debug)]
struct Action {
    id: String,
    label: String,
    key: char,
    command: ActionCommand,
}
#[derive(Debug)]
enum ActionCommand {
    Navigate(StatusMenuKind, MenuOptions),
    Tmux(Vec<String>),
    Agent(String),
    Host(String),
    Launch,
    Focus(String),
    Disabled,
}
impl Action {
    fn new(id: &str, label: &str, key: char, command: ActionCommand) -> Self {
        Self {
            id: id.into(),
            label: label.into(),
            key,
            command,
        }
    }
    fn note(label: &str) -> Self {
        Self::new("", label, ' ', ActionCommand::Disabled)
    }
}

pub fn run(
    kind: StatusMenuKind,
    pane: &str,
    client: &str,
    action: Option<&str>,
    page: usize,
    stay_open: bool,
    options: &MenuOptions,
) -> Result<()> {
    validate_pane(pane)?;
    let paths = Paths::discover()?;
    let context = match Context::read(&paths, pane) {
        Ok(context) => context,
        Err(error) => return message(client, &format!("Menu unavailable: {error}")),
    };
    if options
        .guard
        .as_ref()
        .is_some_and(|guard| guard != &context.guard)
    {
        return message(client, "Pane or agent changed. Reopen the menu.");
    }
    let (title, actions) = actions(kind, options, &context);
    if let Some(id) = action {
        let Some(selected) = actions
            .iter()
            .find(|a| a.id == id && !matches!(a.command, ActionCommand::Disabled))
        else {
            return message(client, "Action no longer available. Reopen the menu.");
        };
        // Direct action invocations also require a guard, not just callbacks.
        if options.guard.is_none() {
            return Err("menu action requires a pane identity guard".into());
        }
        if let ActionCommand::Navigate(next_kind, next) = &selected.command {
            return run(*next_kind, pane, client, None, 0, stay_open, next);
        }
        if let Err(error) = execute_action(selected, &context, client, options) {
            // This is an interactive callback: report the failure in the
            // client's status line instead of leaving a run-shell output pager.
            return message(client, &format!("Menu: {error}"));
        }
        return Ok(());
    }
    let clients = tmux(&[
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
    let back = actions.iter().find(|action| action.id == "back");
    let items: Vec<_> = actions
        .iter()
        .filter(|action| action.id != "back")
        .collect();
    let page_size = size
        .1
        .saturating_sub(if back.is_some() { 8 } else { 7 })
        .max(1);
    let page = page.min(items.len().saturating_sub(1) / page_size);
    let start = page * page_size;
    let executable = std::env::current_exe()?;
    let kind_name = kind.to_possible_value().unwrap();
    let base = format!(
        "{} status-menu {} --pane {} --client {} {} --guard {}",
        shell_quote(&executable.to_string_lossy()),
        kind_name.get_name(),
        shell_quote(pane),
        shell_quote(client),
        options.route(),
        shell_quote(&context.guard)
    );
    let mut command = Command::new("tmux");
    if let Ok(server) = crate::server::ServerIdentity::discover() {
        command.args(server.tmux_args());
    }
    command.args(["display-menu", "-M"]);
    if stay_open {
        command.arg("-O");
    }
    command.args([
        "-C",
        "0",
        "-c",
        client,
        "-t",
        pane,
        "-b",
        "rounded",
        "-T",
        &format!(" {} ", menu_label(&title, size.0)),
        "-x",
        "C",
        "-y",
        "C",
    ]);
    command.arg("--");
    for action in items.iter().skip(start).take(page_size) {
        render_action(&mut command, action, &base, size.0);
    }
    // Navigation stays in the footer on every page, alongside Close.
    command.arg("");
    for (show, label, key, next) in [
        (page > 0, "Previous page", "[", page.saturating_sub(1)),
        (start + page_size < items.len(), "Next page", "]", page + 1),
    ] {
        if show {
            command.args([
                label.into(),
                key.into(),
                format!(
                    "run-shell -b {}",
                    shell_quote(&format!("{base} --page {next}"))
                ),
            ]);
        }
    }
    if let Some(back) = back {
        render_action(&mut command, back, &base, size.0);
    }
    // Escape already dismisses menus. Pad its custom label to the same column
    // as native key labels; inline styles trigger tmux's byte-length truncation
    // on narrow clients even when the visible label fits.
    let mut footer_width = ratatui::text::Line::from(format!(
        " {} ",
        menu_label(&title, size.0).replace("##", "#")
    ))
    .width();
    let menu_args: Vec<_> = command
        .get_args()
        .skip_while(|arg| *arg != "--")
        .skip(1)
        .collect();
    let mut index = 0;
    while index < menu_args.len() {
        let label = menu_args[index].to_string_lossy();
        if label.is_empty() {
            index += 1;
            continue;
        }
        let key = menu_args[index + 1].to_string_lossy();
        let label = label.strip_prefix('-').unwrap_or(&label).replace("##", "#");
        let width = ratatui::text::Line::from(label).width()
            + if key.is_empty() { 0 } else { key.len() + 3 };
        footer_width = footer_width.max(width);
        index += 3;
    }
    let close = format!(
        "× close{}(ESC)",
        " ".repeat(footer_width.saturating_sub(12).max(1))
    );
    command.args([close.as_str(), "", ""]);
    match command.status()?.code() {
        Some(0 | 2) => Ok(()),
        _ => Err("could not display status menu".into()),
    }
}

fn render_action(command: &mut Command, action: &Action, base: &str, width: usize) {
    if matches!(action.command, ActionCommand::Disabled) {
        command.args([
            format!("-{}", menu_label(&action.label, width)),
            String::new(),
            String::new(),
        ]);
    } else {
        command.args([
            menu_label(&action.label, width),
            action.key.to_string(),
            format!(
                "run-shell -b {}",
                shell_quote(&format!("{base} --action {}", shell_quote(&action.id)))
            ),
        ]);
    }
}

fn nav(id: &str, label: &str, key: char, kind: StatusMenuKind, options: MenuOptions) -> Action {
    Action::new(id, label, key, ActionCommand::Navigate(kind, options))
}
fn tmux_action(id: &str, label: &str, key: char, args: &[&str]) -> Action {
    Action::new(
        id,
        label,
        key,
        ActionCommand::Tmux(args.iter().map(|v| (*v).into()).collect()),
    )
}
fn actions(
    kind: StatusMenuKind,
    options: &MenuOptions,
    context: &Context,
) -> (String, Vec<Action>) {
    let mut actions = vec![];
    let mut title = match kind {
        StatusMenuKind::Host => "SSH".into(),
        StatusMenuKind::Tmux => "tmux".into(),
        StatusMenuKind::Agent => agent_title(context),
    };
    let mut back = View::Root;
    match options.view {
        View::Launch => {
            title = "Launch · choose agent".into();
            for (tool, key) in [
                (Tool::Codex, 'x'),
                (Tool::Claude, 'c'),
                (Tool::Traex, 't'),
                (Tool::Opencode, 'o'),
            ] {
                if executable(tool.name()) {
                    actions.push(nav(
                        tool.name(),
                        tool.label(),
                        key,
                        kind,
                        MenuOptions {
                            tool,
                            preset: Preset::Single,
                            panes: false,
                            ..options.at(View::Preset)
                        },
                    ));
                }
            }
            if actions.is_empty() {
                actions.push(Action::note("No coding agents found on PATH"));
            }
        }
        View::Preset => {
            title = format!("{} · choose preset", options.tool.label());
            back = View::Launch;
            for (preset, key) in [
                (Preset::Single, 's'),
                (Preset::SingleBudget, 'b'),
                (Preset::Team, 't'),
                (Preset::TeamBudget, 'T'),
            ] {
                if preset.available(options.tool) {
                    actions.push(nav(
                        preset.name(),
                        preset.label(),
                        key,
                        kind,
                        MenuOptions {
                            preset,
                            panes: false,
                            ..options.at(if !preset.is_team() {
                                View::Ready
                            } else {
                                View::Presentation
                            })
                        },
                    ));
                }
            }
            match crate::menu_launch::budget_argv(options.tool.name()) {
                Ok(None) => actions.push(Action::note("Single Budget: configure launch.toml")),
                Err(_) => actions.push(Action::note("Single Budget: invalid launch.toml")),
                Ok(Some(_)) => (),
            }
            if !executable("agent-team") {
                actions.push(Action::note("Install agent-team to enable Team"));
            }
        }
        View::Presentation => {
            title = format!("{} · {}", options.tool.label(), options.preset.label());
            back = View::Preset;
            actions.push(nav(
                "native",
                "Native subagents (default)",
                'n',
                kind,
                MenuOptions {
                    panes: false,
                    ..options.at(View::Ready)
                },
            ));
            actions.push(nav(
                "panes",
                "Interactive tmux panes",
                't',
                kind,
                MenuOptions {
                    panes: true,
                    ..options.at(View::Ready)
                },
            ));
        }
        View::Ready => {
            title = format!(
                "Launch {} · {}",
                options.tool.label(),
                options.preset.label()
            );
            back = if !options.preset.is_team() {
                View::Preset
            } else {
                View::Presentation
            };
            actions.push(Action::note(&context.cwd));
            actions.push(Action::note(if !options.preset.is_team() {
                "Interactive session · new window"
            } else if options.panes {
                "Interactive team · one tmux window"
            } else {
                "Native subagents · new window"
            }));
            actions.push(Action::note("Permissions: existing CLI / team config"));
            if executable(options.tool.name()) && options.preset.available(options.tool) {
                actions.push(Action::new("start", "Start", 's', ActionCommand::Launch));
            } else {
                actions.push(Action::note("Required CLI is no longer available"));
            }
        }
        View::Panes => {
            title = "Panes & layout".into();
            actions.extend([
                tmux_action("below", "Split below", '-', &["split-window", "-v"]),
                tmux_action("right", "Split right", '|', &["split-window", "-h"]),
                tmux_action("zoom", "Zoom / unzoom", 'z', &["resize-pane", "-Z"]),
                tmux_action(
                    "layout",
                    "Main vertical (prefix Alt+4)",
                    '4',
                    &["select-layout", "main-vertical"],
                ),
                tmux_action("tiled", "Tile panes", 't', &["select-layout", "tiled"]),
            ]);
        }
        View::Switch => {
            title = "Switch".into();
            actions.extend([
                tmux_action("windows", "Choose window", 'w', &["choose-tree", "-Zw"]),
                tmux_action("sessions", "Choose session", 's', &["choose-tree", "-Zs"]),
            ]);
        }
        View::Team => {
            title = format!("Team · {}", context.role.as_deref().unwrap_or("members"));
            for (index, member) in context.team.iter().enumerate().take(35) {
                let label = format!(
                    "{}{}",
                    if member.pane == context.pane {
                        "Current: "
                    } else {
                        "Go to "
                    },
                    member.label
                );
                actions.push(Action::new(
                    &format!("member:{}", member.pane),
                    &label,
                    "123456789abcdefghijklmnopqrstuvwxyz"
                        .chars()
                        .nth(index)
                        .unwrap(),
                    ActionCommand::Focus(member.pane.clone()),
                ));
            }
            if !context.team.is_empty() {
                actions.push(tmux_action(
                    "layout",
                    "Restore team layout (Alt+4)",
                    'L',
                    &["select-layout", "main-vertical"],
                ));
            }
        }
        View::More => {
            actions.extend(agent_commands(context, true));
        }
        View::Root => match kind {
            StatusMenuKind::Tmux => {
                actions.extend([
                    nav(
                        "launch",
                        "Launch agent...",
                        'a',
                        kind,
                        options.at(View::Launch),
                    ),
                    tmux_action("new", "New window", 'c', &["new-window"]),
                    nav(
                        "panes",
                        "Panes & layout...",
                        'p',
                        kind,
                        options.at(View::Panes),
                    ),
                    nav(
                        "switch",
                        "Switch window / session...",
                        'w',
                        kind,
                        options.at(View::Switch),
                    ),
                ]);
                if executable("code") {
                    actions.push(tmux_action(
                        "editor",
                        "Open project in VS Code",
                        'v',
                        &[
                            "new-window",
                            "-n",
                            "code",
                            "code .; exec ${SHELL:-/bin/sh} -l",
                        ],
                    ));
                }
                actions.push(tmux_action(
                    "detach",
                    "Detach this client",
                    'd',
                    &["detach-client"],
                ));
            }
            StatusMenuKind::Agent => {
                actions.extend(agent_commands(context, false));
                if context.input_available
                    && matches!(context.state, BaseState::Idle | BaseState::Working)
                {
                    if let Some(agent) = &context.agent {
                        for (name, key, input) in
                            crate::menu_skills::shortcuts(agent.kind, &context.cwd)
                        {
                            actions.push(Action::new(
                                &format!("skill:{name}"),
                                &format!("{name} · Skill"),
                                key,
                                ActionCommand::Agent(input),
                            ));
                        }
                    }
                }
                if !agent_commands(context, true).is_empty() {
                    actions.push(nav(
                        "more",
                        "More commands...",
                        'm',
                        kind,
                        options.at(View::More),
                    ));
                }
                if !context.team.is_empty() {
                    actions.push(nav(
                        "team",
                        "Team members & progress...",
                        't',
                        kind,
                        options.at(View::Team),
                    ));
                }
                actions.push(nav(
                    "launch",
                    "Launch agent...",
                    'a',
                    kind,
                    options.at(View::Launch),
                ));
            }
            StatusMenuKind::Host => {
                if let Ok(output) = Command::new("ssh-connect").args(["hosts", "list"]).output() {
                    if output.status.success() {
                        for (index, host) in String::from_utf8_lossy(&output.stdout)
                            .lines()
                            .filter(|h| !h.is_empty())
                            .take(35)
                            .enumerate()
                        {
                            actions.push(Action::new(
                                &format!("host:{host}"),
                                host,
                                "123456789abcdefghijklmnopqrstuvwxyz"
                                    .chars()
                                    .nth(index)
                                    .unwrap(),
                                ActionCommand::Host(host.into()),
                            ));
                        }
                    }
                }
                if actions.is_empty() {
                    actions.push(Action::note("No SSH hosts available"));
                }
            }
        },
    }
    if options.view != View::Root {
        actions.push(nav("back", "← Back", 'B', kind, options.at(back)));
    }
    (title, actions)
}

fn agent_title(context: &Context) -> String {
    let Some(agent) = &context.agent else {
        return format!("No foreground agent · {}", context.pane);
    };
    let name = match agent.kind {
        AgentKind::Codex => "Codex",
        AgentKind::Claude => "Claude",
        AgentKind::Trae => "Trae",
        AgentKind::Opencode => "OpenCode",
    };
    let state = match context.state {
        BaseState::Idle => "idle",
        BaseState::Working => "working",
        BaseState::Blocked => "needs input",
        BaseState::Unknown => "state unknown",
    };
    format!(
        "{name} · {state} · {}{}",
        context.pane,
        context
            .role
            .as_ref()
            .map(|r| format!(" · {r}"))
            .unwrap_or_default()
    )
}

// Provider-specific, conservative built-ins. No generic /side or /fork fallback.
// Busy-safe Codex commands follow SlashCommand::available_during_task.
fn catalog(kind: AgentKind) -> &'static [(&'static str, char, bool, bool)] {
    match kind {
        AgentKind::Codex => &[
            ("/goal", 'g', true, false),
            ("/side", 's', true, false),
            ("/plan", 'p', false, false),
            ("/compact", 'c', false, false),
            ("/fork", 'f', false, true),
            ("/diff", 'd', true, true),
            ("/status", 'i', true, true),
            ("/model", 'm', true, true),
            ("/permissions", 'p', true, true),
        ],
        AgentKind::Claude => &[
            ("/btw", 'b', true, false),
            ("/plan", 'p', false, false),
            ("/compact", 'c', false, false),
            ("/status", 'i', false, true),
            ("/model", 'm', false, true),
            ("/permissions", 'p', false, true),
            ("/resume", 'r', false, true),
            ("/help", 'h', false, true),
        ],
        // TraeX distinguishes a tool-free /btw answer from a /side thread.
        // Keep the common quick question in front and the separate thread in More.
        AgentKind::Trae => &[
            ("/goal", 'g', false, false),
            ("/btw", 'b', true, false),
            ("/plan", 'p', false, false),
            ("/compact", 'c', false, false),
            ("/side", 's', false, true),
            ("/help", 'h', false, true),
        ],
        AgentKind::Opencode => &[
            ("/sessions", 's', false, false),
            ("/compact", 'c', false, false),
            ("/models", 'm', false, true),
            ("/help", 'h', false, true),
            ("/details", 'd', false, true),
            ("/thinking", 't', false, true),
        ],
    }
}

fn agent_commands(context: &Context, more: bool) -> Vec<Action> {
    let Some(agent) = &context.agent else {
        return vec![];
    };
    if !context.input_available || matches!(context.state, BaseState::Blocked | BaseState::Unknown)
    {
        return if more {
            vec![]
        } else {
            vec![Action::note("Return to the agent prompt, then reopen")]
        };
    }
    let mut actions: Vec<_> = catalog(agent.kind)
        .iter()
        .filter(|(_, _, busy, extra)| *extra == more && (context.state == BaseState::Idle || *busy))
        .map(|(command, key, _, _)| {
            Action::new(
                command,
                command,
                *key,
                ActionCommand::Agent((*command).into()),
            )
        })
        .collect();
    if !more {
        actions.insert(
            0,
            Action::note("Prefill only · press Enter in agent to send"),
        );
    }
    actions
}

fn launch_argv(options: &MenuOptions) -> Result<Vec<String>> {
    if options.preset == Preset::Single {
        return Ok(vec![options.tool.name().into()]);
    }
    if options.preset == Preset::SingleBudget {
        return crate::menu_launch::budget_argv(options.tool.name())?
            .ok_or_else(|| "Single Budget is not configured in launch.toml".into());
    }
    let mut args = vec!["agent-team".into(), options.tool.name().into()];
    if options.preset == Preset::TeamBudget {
        args.push("--team-budget".into());
    }
    if options.panes {
        args.push("--tmux".into());
    }
    Ok(args)
}

fn execute_action(
    action: &Action,
    context: &Context,
    client: &str,
    options: &MenuOptions,
) -> Result<()> {
    match &action.command {
        ActionCommand::Tmux(args) => {
            if args[0] == "select-layout" {
                // Use the same sidebar-preserving path as prefix Alt+4.
                let output = Command::new("workbench-layout")
                    .args([&args[1], &context.pane])
                    .output()?;
                if !output.status.success() {
                    return Err(String::from_utf8_lossy(&output.stderr)
                        .trim()
                        .to_string()
                        .into());
                }
                return Ok(());
            }
            let mut full = vec![args[0].clone()];
            if args[0] == "detach-client" {
                full.extend(["-t".into(), client.into()]);
            } else if args[0] == "new-window" {
                full.extend([
                    "-t".into(),
                    context.session.clone(),
                    "-c".into(),
                    context.cwd.clone(),
                ]);
            } else {
                full.extend(["-t".into(), context.pane.clone()]);
                if args[0] == "split-window" {
                    full.extend(["-c".into(), context.cwd.clone()]);
                }
            }
            full.extend_from_slice(&args[1..]);
            tmux(&full.iter().map(String::as_str).collect::<Vec<_>>())?;
        }
        ActionCommand::Agent(text) => {
            tmux(&["send-keys", "-t", &context.pane, "-l", &format!("{text} ")])?;
        }
        ActionCommand::Host(host) => {
            tmux(&[
                "new-window",
                "-t",
                &context.session,
                "-c",
                &context.cwd,
                "-n",
                host,
                &format!("exec ssh-connect connect {}", shell_quote(host)),
            ])?;
        }
        ActionCommand::Launch => {
            let args = launch_argv(options)?;
            if options.panes && options.preset.is_team() {
                // agent-team owns creation of its entire interactive team window.
                // Never create an intermediate launcher pane or mix team modes.
                let output = Command::new(&args[0])
                    .args(&args[1..])
                    .env_remove("AGENT_TEAM_DIR")
                    .env_remove("AGENT_TEAM_ROLE")
                    .env("TMUX_PANE", &context.pane)
                    .current_dir(&context.cwd)
                    .output()?;
                if !output.status.success() {
                    return Err(String::from_utf8_lossy(&output.stderr)
                        .trim()
                        .to_string()
                        .into());
                }
            } else {
                let command = format!(
                    "exec env -u AGENT_TEAM_DIR -u AGENT_TEAM_ROLE {}",
                    args.iter()
                        .map(|a| shell_quote(a))
                        .collect::<Vec<_>>()
                        .join(" ")
                );
                tmux(&[
                    "new-window",
                    "-t",
                    &context.session,
                    "-c",
                    &context.cwd,
                    "-n",
                    options.tool.name(),
                    &command,
                ])?;
            }
        }
        ActionCommand::Focus(pane) => {
            tmux(&["switch-client", "-c", client, "-t", &context.session])?;
            tmux(&["select-window", "-t", &context.window])?;
            tmux(&["select-pane", "-t", pane])?;
        }
        ActionCommand::Navigate(..) | ActionCommand::Disabled => unreachable!(),
    }
    Ok(())
}
fn message(client: &str, text: &str) -> Result<()> {
    tmux(&[
        "display-message",
        "-c",
        client,
        "--",
        &text.replace('#', "##"),
    ])?;
    Ok(())
}
fn menu_label(label: &str, width: usize) -> String {
    label
        .chars()
        .filter(|c| !c.is_control())
        .take(width.saturating_sub(10).max(1))
        .collect::<String>()
        .replace('#', "##")
}
fn validate_pane(pane: &str) -> Result<()> {
    if pane
        .strip_prefix('%')
        .is_some_and(|v| !v.is_empty() && v.chars().all(|c| c.is_ascii_digit()))
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
    use crate::{model::ProcessFingerprint, process::AgentProcess};
    fn context(kind: Option<AgentKind>, state: BaseState) -> Context {
        Context {
            pane: "%1".into(),
            session: "$1".into(),
            window: "@1".into(),
            cwd: "/tmp".into(),
            guard: "test".into(),
            agent: kind.map(|kind| AgentProcess {
                kind,
                fingerprint: ProcessFingerprint {
                    pid: 42,
                    started_at_ticks: 1,
                    executable: "agent".into(),
                },
            }),
            state,
            input_available: true,
            team: vec![],
            role: None,
        }
    }
    #[test]
    fn shell_and_approval_never_get_slash_commands() {
        for ctx in [
            context(None, BaseState::Idle),
            context(Some(AgentKind::Codex), BaseState::Blocked),
            context(Some(AgentKind::Codex), BaseState::Unknown),
        ] {
            for more in [false, true] {
                assert!(
                    !agent_commands(&ctx, more)
                        .iter()
                        .any(|a| matches!(a.command, ActionCommand::Agent(_)))
                );
            }
        }
    }
    #[test]
    fn busy_codex_can_ask_side_question_but_cannot_fork() {
        let cmds = agent_commands(&context(Some(AgentKind::Codex), BaseState::Working), false);
        assert!(cmds.iter().any(|a| a.id == "/side"));
        assert!(cmds.iter().any(|a| a.id == "/goal"));
        assert!(!cmds.iter().any(|a| a.id == "/plan" || a.id == "/compact"));
        assert!(!cmds.iter().any(|a| a.id == "/fork"));
    }
    #[test]
    fn catalogs_are_provider_specific() {
        for kind in [AgentKind::Claude, AgentKind::Opencode] {
            assert!(
                !catalog(kind)
                    .iter()
                    .any(|(cmd, _, _, _)| *cmd == "/side" || *cmd == "/fork")
            );
        }
        assert!(
            catalog(AgentKind::Opencode)
                .iter()
                .any(|(cmd, _, _, _)| *cmd == "/models")
        );
    }
    #[test]
    fn launch_combinations_are_explicit_and_do_not_override_permissions() {
        for tool in [Tool::Codex, Tool::Claude, Tool::Traex, Tool::Opencode] {
            for preset in [
                Preset::Single,
                Preset::SingleBudget,
                Preset::Team,
                Preset::TeamBudget,
            ] {
                if preset == Preset::SingleBudget && tool != Tool::Codex {
                    continue;
                }
                for panes in [false, true] {
                    let args = launch_argv(&MenuOptions {
                        tool,
                        preset,
                        panes,
                        ..Default::default()
                    })
                    .unwrap();
                    assert_eq!(args.contains(&"--tmux".into()), panes && preset.is_team());
                    assert_eq!(
                        args.contains(&"--team-budget".into()),
                        preset == Preset::TeamBudget
                    );
                    assert!(
                        args.iter()
                            .all(|a| !a.contains("approve") && !a.contains("permission"))
                    );
                }
            }
        }
    }
    #[test]
    fn all_views_have_unique_shortcuts() {
        for kind in [StatusMenuKind::Tmux, StatusMenuKind::Agent] {
            for agent in [
                AgentKind::Codex,
                AgentKind::Claude,
                AgentKind::Trae,
                AgentKind::Opencode,
            ] {
                for view in [
                    View::Root,
                    View::Panes,
                    View::Switch,
                    View::Launch,
                    View::Preset,
                    View::Presentation,
                    View::Ready,
                    View::More,
                    View::Team,
                ] {
                    let (_, actions) = actions(
                        kind,
                        &MenuOptions {
                            view,
                            ..Default::default()
                        },
                        &context(Some(agent), BaseState::Idle),
                    );
                    let mut seen = std::collections::HashSet::new();
                    for a in actions {
                        if !matches!(a.command, ActionCommand::Disabled) {
                            assert!(seen.insert(a.key), "{kind:?} {agent:?} {view:?}: {}", a.key);
                        }
                    }
                }
            }
        }
    }
    #[test]
    fn targets_and_labels_are_not_shell_or_tmux_code() {
        assert!(validate_pane("%12").is_ok());
        assert!(validate_pane("%12;kill-server").is_err());
        assert_eq!(shell_quote("dev'box"), "'dev'\\''box'");
        assert_eq!(menu_label("a#{host}\nb", 80), "a##{host}b");
    }
}
