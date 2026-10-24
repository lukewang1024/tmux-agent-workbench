use crate::ui_language::text as tr;
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
    Launch,
    Configure,
    AgentTarget,
    More,
    Team,
    Botmux,
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
    Team,
}
impl Preset {
    fn is_team(self) -> bool {
        matches!(self, Self::Team)
    }
    fn available(self, _tool: Tool) -> bool {
        executable("agent-team")
    }
    fn name(self) -> &'static str {
        match self {
            Self::Single => "single",
            Self::Team => "team",
        }
    }
    fn label(self) -> &'static str {
        match self {
            Self::Single => tr("Single", "单人 · Single"),
            Self::Team => tr("Team", "团队 · Team"),
        }
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum, Default)]
pub enum Scene {
    #[default]
    Daily,
    Hard,
    Batch,
}
impl Scene {
    fn name(self) -> &'static str {
        match self {
            Self::Daily => "daily",
            Self::Hard => "hard",
            Self::Batch => "batch",
        }
    }
    fn label(self) -> &'static str {
        match self {
            Self::Daily => tr("Daily development · daily", "日常开发 · daily"),
            Self::Hard => tr("Complex tasks · hard", "复杂攻坚 · hard"),
            Self::Batch => tr("Batch work · batch", "批量执行 · batch"),
        }
    }
}

#[derive(Debug, Clone, Args, Default)]
pub struct MenuOptions {
    #[arg(long, value_enum, default_value = "daily")]
    pub scene: Scene,
    #[arg(long, hide = true, value_parser = decode_launch_error)]
    pub launch_error: Option<String>,
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
    #[arg(long, value_parser = ["mouse", "touch", "release"])]
    pub input_mode: Option<String>,
    #[arg(long, value_parser = ["en", "zh"])]
    pub language: Option<String>,
    #[arg(long)]
    pub focus_agent: bool,
}
impl MenuOptions {
    fn route(&self) -> String {
        format!(
            "--view {} --tool {} --preset {} --scene {}{}{} --language {}{}",
            self.view.to_possible_value().unwrap().get_name(),
            self.tool.name(),
            self.preset.name(),
            self.scene.name(),
            if self.panes { " --panes" } else { "" },
            self.input_mode
                .as_ref()
                .map(|mode| format!(" --input-mode {mode}"))
                .unwrap_or_default(),
            self.language
                .as_deref()
                .unwrap_or(crate::ui_language::name()),
            self.launch_error
                .as_ref()
                .map(|error| format!(" --launch-error={}", encode_launch_error(error)))
                .unwrap_or_default()
        )
    }

    fn at(&self, view: View) -> Self {
        Self {
            view,
            launch_error: (view == View::Configure)
                .then(|| self.launch_error.clone())
                .flatten(),
            ..self.clone()
        }
    }
}

// Callbacks pass through tmux format expansion as well as the shell. Keep
// external diagnostics opaque in that path; quoting alone does not protect #{}.
fn encode_launch_error(error: &str) -> String {
    error.bytes().map(|byte| format!("{byte:02x}")).collect()
}

fn decode_launch_error(value: &str) -> std::result::Result<String, String> {
    if !value.len().is_multiple_of(2) || !value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err("invalid encoded launch error".into());
    }
    let bytes = (0..value.len())
        .step_by(2)
        .map(|index| u8::from_str_radix(&value[index..index + 2], 16).unwrap())
        .collect();
    String::from_utf8(bytes).map_err(|_| "launch error is not UTF-8".into())
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
    NewSession,
    Agent(String),
    Usage,
    Host(String),
    Botmux(String),
    Launch,
    Focus(String),
    AgentTarget(String),
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

fn launch_context(kind: StatusMenuKind, view: View) -> bool {
    kind == StatusMenuKind::Tmux
        || matches!(view, View::Launch | View::Configure | View::AgentTarget)
}

fn agent_targets(window: &str) -> Result<Vec<(String, bool)>> {
    Ok(tmux(&[
        "list-panes",
        "-t",
        window,
        "-F",
        "#{pane_id}|#{@workbench_agent_actions}|#{pane_last}",
    ])?
    .lines()
    .filter_map(|line| {
        let fields = line.split('|').collect::<Vec<_>>();
        (fields.len() == 3 && fields[1] == "1").then(|| (fields[0].to_string(), fields[2] == "1"))
    })
    .collect())
}

fn open_agent_status(
    pane: &str,
    client: &str,
    stay_open: bool,
    options: &MenuOptions,
) -> Result<()> {
    // The active pane wins even before the detector publishes its next tick.
    let direct = Context::read(&Paths::discover()?, pane)?;
    if direct.agent.is_some() {
        return show_agent_menu(&direct, client, stay_open, options);
    }
    let context = Context::read_launch(pane)?;
    let targets = agent_targets(&context.window)?;
    let remembered = tmux(&[
        "show-option",
        "-wqv",
        "-t",
        &context.window,
        "@workbench_last_agent_pane",
    ])?;
    let selected = targets
        .iter()
        .find(|(target, _)| target == pane)
        .or_else(|| targets.iter().find(|(target, _)| target == &remembered))
        .or_else(|| targets.iter().find(|(_, last)| *last))
        .or_else(|| (targets.len() == 1).then(|| &targets[0]));
    if let Some((target, _)) = selected {
        return focus_agent_menu(target, &context.window, client, stay_open, options);
    }
    if targets.is_empty() {
        return message(
            client,
            tr(
                "No interactive agent in this window",
                "当前窗口没有可交互的 agent",
            ),
        );
    }
    let next = MenuOptions {
        view: View::AgentTarget,
        focus_agent: false,
        guard: Some(context.guard.clone()),
        ..options.clone()
    };
    run_with_context(
        StatusMenuKind::Agent,
        pane,
        client,
        None,
        0,
        stay_open,
        &next,
        &context,
    )
}

fn focus_agent_menu(
    pane: &str,
    window: &str,
    client: &str,
    stay_open: bool,
    options: &MenuOptions,
) -> Result<()> {
    validate_pane(pane)?;
    let context = Context::read(&Paths::discover()?, pane)?;
    if context.window != window || context.agent.is_none() {
        return message(
            client,
            tr(
                "Agent changed. Reopen the menu.",
                "Agent 已变化，请重新打开菜单。",
            ),
        );
    }
    show_agent_menu(&context, client, stay_open, options)
}

fn show_agent_menu(
    context: &Context,
    client: &str,
    stay_open: bool,
    options: &MenuOptions,
) -> Result<()> {
    let pane = &context.pane;
    let window = &context.window;
    tmux(&["select-pane", "-t", pane])?;
    tmux(&[
        "set-option",
        "-wq",
        "-t",
        window,
        "@workbench_last_agent_pane",
        pane,
    ])?;
    let next = MenuOptions {
        view: View::Root,
        focus_agent: false,
        guard: Some(context.guard.clone()),
        ..options.clone()
    };
    run_with_context(
        StatusMenuKind::Agent,
        pane,
        client,
        None,
        0,
        stay_open,
        &next,
        &context,
    )
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
    crate::ui_language::set_language(options.language.as_deref());
    validate_pane(pane)?;
    if kind == StatusMenuKind::Agent && options.focus_agent && action.is_none() {
        return open_agent_status(pane, client, stay_open, options);
    }
    let paths = Paths::discover()?;
    let context = match if launch_context(kind, options.view) {
        Context::read_launch(pane)
    } else {
        Context::read(&paths, pane)
    } {
        Ok(context) => context,
        Err(error) => return message(client, &format!("Menu unavailable: {error}")),
    };
    run_with_context(
        kind, pane, client, action, page, stay_open, options, &context,
    )
}

#[allow(clippy::too_many_arguments)]
fn run_with_context(
    kind: StatusMenuKind,
    pane: &str,
    client: &str,
    action: Option<&str>,
    page: usize,
    stay_open: bool,
    options: &MenuOptions,
    context: &Context,
) -> Result<()> {
    if options
        .guard
        .as_ref()
        .is_some_and(|guard| guard != &context.guard)
    {
        return message(client, "Pane or agent changed. Reopen the menu.");
    }
    let (title, mut actions) = actions(kind, options, context);
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
            if launch_context(kind, options.view) != launch_context(*next_kind, next.view) {
                // Validate the old scope above before entering the new one.
                let paths = Paths::discover()?;
                let context = if launch_context(*next_kind, next.view) {
                    Context::read_launch(pane)?
                } else {
                    Context::read(&paths, pane)?
                };
                let next = MenuOptions {
                    guard: Some(context.guard.clone()),
                    ..next.clone()
                };
                return run_with_context(
                    *next_kind, pane, client, None, 0, stay_open, &next, &context,
                );
            }
            return run_with_context(*next_kind, pane, client, None, 0, stay_open, next, context);
        }
        if let ActionCommand::AgentTarget(target) = &selected.command {
            return focus_agent_menu(target, &context.window, client, stay_open, options);
        }
        if let Err(error) = execute_action(selected, context, client, options) {
            // This is an interactive callback: report the failure in the
            // client's status line instead of leaving a run-shell output pager.
            if matches!(selected.command, ActionCommand::Launch) {
                let next = MenuOptions {
                    view: View::Configure,
                    launch_error: Some(error.to_string()),
                    ..options.clone()
                };
                return run(kind, pane, client, None, 0, stay_open, &next);
            }
            return message(client, &format!("Menu: {error}"));
        }
        return Ok(());
    }
    if options.view == View::Configure {
        let index = actions
            .iter()
            .position(|action| action.id == "summary")
            .expect("launch summary slot");
        let summary = match launch_summary(options) {
            Ok(lines) => lines
                .into_iter()
                .map(|line| Action::note(&line))
                .collect::<Vec<_>>(),
            Err(error) => {
                if let Some(start) = actions.iter_mut().find(|action| action.id == "start") {
                    start.command = ActionCommand::Disabled;
                }
                actions.retain(|action| action.id != "launch-error");
                actions.push(Action {
                    id: "launch-error".into(),
                    ..Action::note(&format!(
                        "{}: {error}",
                        tr("Configuration unavailable", "配置不可用")
                    ))
                });
                vec![]
            }
        };
        actions.splice(index..=index, summary);
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
    let footer = MenuFooter::new(&actions);
    let items: Vec<_> = actions
        .iter()
        .filter(|action| !matches!(action.id.as_str(), "back" | "start" | "launch-error"))
        .collect();
    let page_size = size.1.saturating_sub(footer.reserved_rows()).max(1);
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
    let server = crate::server::ServerIdentity::discover()?;
    command.args(server.tmux_args());
    // The source pane remains the action target, but a native menu needs a
    // pane visible to the destination client to obtain its drawing screen.
    let display_pane = crate::tmux::Tmux::new(server)
        .client_format(client, "#{pane_id}")?
        .ok_or("Workbench client disappeared")?;
    command.args(["display-menu", "-M"]);
    // -O selects on press and can send the same click's release to a new menu.
    // Keep it only for old tmux pointer clients whose hover handling needs it.
    if stay_open {
        command.arg("-O");
    }
    command.args([
        "-C",
        "0",
        "-c",
        client,
        "-t",
        &display_pane,
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
    footer.render(
        &mut command,
        &base,
        size.0,
        page,
        start + page_size < items.len(),
    );
    match command.status()?.code() {
        Some(0 | 2) => Ok(()),
        _ => Err("could not display status menu".into()),
    }
}

struct MenuFooter<'a> {
    back: Option<&'a Action>,
    launch: Option<&'a Action>,
    error: Option<&'a Action>,
}

impl<'a> MenuFooter<'a> {
    fn new(actions: &'a [Action]) -> Self {
        Self {
            back: actions.iter().find(|action| action.id == "back"),
            launch: actions.iter().find(|action| action.id == "start"),
            error: actions.iter().find(|action| action.id == "launch-error"),
        }
    }

    fn reserved_rows(&self) -> usize {
        // Borders, separator, pagination and Close, plus the optional rows.
        7 + usize::from(self.back.is_some())
            + usize::from(self.launch.is_some())
            + usize::from(self.error.is_some())
    }

    fn render(&self, command: &mut Command, base: &str, width: usize, page: usize, has_next: bool) {
        command.arg("");
        for (show, label, key, next) in [
            (
                page > 0,
                tr("Previous page", "上一页"),
                "[",
                page.saturating_sub(1),
            ),
            (has_next, tr("Next page", "下一页"), "]", page + 1),
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
        for action in [self.error, self.launch, self.back].into_iter().flatten() {
            render_action(command, action, base, width);
        }
        command.args(crate::menu_footer::close_item(self.back.is_none()));
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
            crate::menu_footer::item_label(&menu_label(&action.label, width), action.key),
            crate::menu_footer::key_name(action.key),
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
        StatusMenuKind::Host => {
            let hostname = tmux(&["display-message", "-p", "-t", &context.pane, "#{host}"])
                .unwrap_or_default();
            format!("{} · {hostname}", tr("Host", "主机"))
        }
        StatusMenuKind::Tmux => "tmux".into(),
        StatusMenuKind::Agent => agent_title(context),
    };
    let mut back = View::Root;
    match options.view {
        View::AgentTarget => {
            title = tr("Choose agent", "选择 agent").into();
            if let Ok(targets) = agent_targets(&context.window) {
                for (index, (pane, _)) in targets.iter().take(9).enumerate() {
                    actions.push(Action::new(
                        &format!("target:{pane}"),
                        &format!("Agent {pane}"),
                        char::from_digit((index + 1) as u32, 10).unwrap(),
                        ActionCommand::AgentTarget(pane.clone()),
                    ));
                }
            }
        }
        View::Launch => {
            title = tr("Launch · choose agent", "启动 · 选择 agent").into();
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
                            scene: Scene::Daily,
                            launch_error: None,
                            ..options.at(View::Configure)
                        },
                    ));
                }
            }
            if actions.is_empty() {
                actions.push(Action::note(tr(
                    "No coding agents found on PATH",
                    "PATH 中未找到 coding agent",
                )));
            }
        }
        View::Configure => {
            title = format!("{} {}", tr("Launch", "启动"), options.tool.label());
            back = View::Launch;
            actions.push(Action::note(&context.cwd));
            if matches!(options.tool, Tool::Codex | Tool::Traex) {
                actions.push(Action::note(tr("Scene", "场景")));
                for (scene, key) in [(Scene::Daily, 'd'), (Scene::Hard, 'h'), (Scene::Batch, 'b')] {
                    actions.push(nav(
                        scene.name(),
                        &choice_label(scene.label(), options.scene == scene),
                        key,
                        kind,
                        MenuOptions {
                            scene,
                            launch_error: None,
                            ..options.at(View::Configure)
                        },
                    ));
                }
            }
            actions.push(Action::note(tr("Mode", "模式")));
            for (preset, key) in [(Preset::Single, 's'), (Preset::Team, 't')] {
                actions.push(nav(
                    preset.name(),
                    &choice_label(preset.label(), options.preset == preset),
                    key,
                    kind,
                    MenuOptions {
                        preset,
                        launch_error: None,
                        ..options.at(View::Configure)
                    },
                ));
            }
            if options.preset.is_team() {
                actions.push(Action::note(tr("Collaboration", "协作方式")));
                for (panes, key, label) in [
                    (false, 'n', tr("Native subagents", "原生子代理")),
                    (true, 'm', tr("tmux panes", "tmux 窗格")),
                ] {
                    actions.push(nav(
                        if panes { "panes" } else { "native" },
                        &choice_label(label, options.panes == panes),
                        key,
                        kind,
                        MenuOptions {
                            panes,
                            launch_error: None,
                            ..options.at(View::Configure)
                        },
                    ));
                }
            }
            actions.push(Action {
                id: "summary".into(),
                ..Action::note("")
            });
            if let Some(error) = &options.launch_error {
                actions.push(Action {
                    id: "launch-error".into(),
                    ..Action::note(&format!("{}: {error}", tr("Launch failed", "启动失败")))
                });
            }
            if executable(options.tool.name()) && options.preset.available(options.tool) {
                actions.push(Action::new(
                    "start",
                    tr("▶ Launch", "▶ 启动"),
                    '\r',
                    ActionCommand::Launch,
                ));
            } else {
                actions.push(Action::note(tr(
                    "Required CLI / agent-team is unavailable",
                    "所需 CLI / agent-team 不可用",
                )));
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
        View::Botmux => {
            title = "Botmux".into();
            actions.push(Action::note("Connect by pasting /adopt in a Feishu topic"));
            if context.agent.is_some() {
                if context.role.as_deref().is_some_and(|role| role != "lead") {
                    if let Some(lead) = context.team.iter().find(|m| m.label.starts_with("lead")) {
                        actions.push(Action::new(
                            "botmux-lead",
                            "Go to team lead (recommended)",
                            'l',
                            ActionCommand::Focus(lead.pane.clone()),
                        ));
                    }
                }
                actions.push(Action::new(
                    "botmux-connect",
                    "Prepare this pane's connect command...",
                    'c',
                    ActionCommand::Botmux("connect".into()),
                ));
            }
            for (id, label, key) in [
                ("status", "Daemon status...", 's'),
                ("list", "List connections / disconnect help...", 'i'),
                ("dashboard", "Dashboard URL...", 'd'),
                ("start", "Start botmux...", 'r'),
                ("setup", "Configure Feishu bot...", 'u'),
            ] {
                actions.push(Action::new(
                    &format!("botmux-{id}"),
                    label,
                    key,
                    ActionCommand::Botmux(id.into()),
                ));
            }
        }
        View::More => {
            actions.extend(agent_commands(context, true));
        }
        View::Root => match kind {
            StatusMenuKind::Tmux => {
                actions.extend([
                    tmux_action(
                        "windows",
                        tr("Switch window", "切换窗口"),
                        'w',
                        &["choose-tree", "-Zw"],
                    ),
                    tmux_action(
                        "sessions",
                        tr("Switch session", "切换会话"),
                        's',
                        &["choose-tree", "-Zs"],
                    ),
                    Action::new(
                        "new-session",
                        tr("New session...", "新建会话..."),
                        'n',
                        ActionCommand::NewSession,
                    ),
                    tmux_action("new", "New window", 'c', &["new-window"]),
                    nav(
                        "launch",
                        tr("Launch agent...", "启动 agent..."),
                        'a',
                        kind,
                        options.at(View::Launch),
                    ),
                    nav(
                        "panes",
                        "Panes & layout...",
                        'p',
                        kind,
                        options.at(View::Panes),
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
    if options.view == View::Root
        && matches!(kind, StatusMenuKind::Agent)
        && executable("botmux")
        && executable("python3")
        && executable("workbench-botmux")
    {
        actions.push(nav(
            "botmux",
            "Botmux...",
            'f',
            kind,
            options.at(View::Botmux),
        ));
    }
    if options.view == View::Root && matches!(kind, StatusMenuKind::Agent) {
        actions.push(Action::new(
            "usage",
            tr("Usage summary...", "使用量总结..."),
            'u',
            ActionCommand::Usage,
        ));
    }
    if options.view != View::Root {
        actions.push(nav(
            "back",
            crate::menu_footer::back_label(),
            crate::menu_footer::BACK_KEY,
            kind,
            options.at(back),
        ));
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

fn choice_label(label: &str, selected: bool) -> String {
    format!("{} {label}", if selected { "●" } else { "○" })
}

// Preview only the fields shown in the panel from the launcher's local JSON.
// No CLI startup, process discovery, model catalog lookup, or cache on this path.
// The launcher remains responsible for validation and the actual launch plan.
fn launch_summary(options: &MenuOptions) -> Result<Vec<String>> {
    let launcher = std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default())
        .map(|dir| dir.join("agent-team"))
        .find(|path| path.is_file())
        .and_then(|path| path.canonicalize().ok());
    let config = launcher
        .as_ref()
        .and_then(|path| path.parent())
        .and_then(|dir| dir.parent())
        .map(|root| root.join("config/teams.json"));
    let Some(config) = config.filter(|path| path.is_file()) else {
        return Ok(vec![
            tr(
                "Model settings resolved at launch",
                "模型配置将在启动时解析",
            )
            .into(),
        ]);
    };
    let defaults: serde_json::Value = serde_json::from_slice(&std::fs::read(config)?)?;
    let paths = Paths::discover()?;
    let personal_path = paths
        .config_dir
        .parent()
        .unwrap()
        .join("agent-team/config.json");
    let personal: serde_json::Value = match std::fs::read(personal_path) {
        Ok(bytes) => serde_json::from_slice(&bytes)?,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => serde_json::json!({}),
        Err(error) => return Err(error.into()),
    };
    let tool = options.tool.name();
    let team = options.preset.is_team();
    let scene_tool = matches!(options.tool, Tool::Codex | Tool::Traex);
    if !scene_tool && !team {
        return summary_lines(&serde_json::json!({}), false);
    }
    let (base, extra) = if scene_tool {
        (
            &defaults[tool]["scenes"][options.scene.name()],
            &personal[tool]["scenes"][options.scene.name()],
        )
    } else {
        (&defaults[tool]["team"], &personal[tool]["team"])
    };
    let mut config = base
        .as_object()
        .ok_or("agent-team configuration is missing")?
        .clone();
    // All displayed fields are scalars, so a shallow overlay matches the
    // launcher's recursive merge without reproducing its launch machinery.
    if let Some(extra) = extra.as_object() {
        config.extend(extra.clone());
    }
    if scene_tool && team {
        for (lead, single) in [("lead_model", "model"), ("lead_effort", "effort")] {
            if let Some(value) = config.get(lead).cloned() {
                config.insert(single.into(), value);
            }
        }
    }
    summary_lines(&serde_json::Value::Object(config), team)
}

fn summary_lines(config: &serde_json::Value, team: bool) -> Result<Vec<String>> {
    let model = config.get("model").and_then(|value| value.as_str());
    if model.is_none()
        && ["effort", "worker_model", "worker_effort"]
            .iter()
            .all(|key| config.get(key).and_then(|value| value.as_str()).is_none())
    {
        return Ok(vec![
            tr(
                "Model / thinking: tool defaults",
                "模型 / thinking: 工具默认配置",
            )
            .into(),
        ]);
    }
    let model = model.unwrap_or(tr("tool default", "工具默认"));
    let effort = config
        .get("effort")
        .and_then(|value| value.as_str())
        .unwrap_or("default");
    let mut lines = vec![format!(
        "{}: {model} · {effort}",
        if team {
            tr("Lead", "主代理")
        } else {
            tr("Single", "单人")
        }
    )];
    if team {
        let worker = config
            .get("worker_model")
            .and_then(|value| value.as_str())
            .unwrap_or(model);
        let effort = config
            .get("worker_effort")
            .and_then(|value| value.as_str())
            .unwrap_or(effort);
        lines.push(format!("{}: {worker} · {effort}", tr("Worker", "子代理")));
    }
    if let Some(fast) = config.get("fast").and_then(|value| value.as_bool()) {
        lines.push(format!(
            "Fast: {}",
            if fast {
                tr("on", "开启")
            } else {
                tr("off", "关闭")
            }
        ));
    }
    Ok(lines)
}

fn launch_argv(options: &MenuOptions) -> Result<Vec<String>> {
    let mut args = vec![
        "agent-team".into(),
        "start".into(),
        options.tool.name().into(),
    ];
    if matches!(options.tool, Tool::Codex | Tool::Traex) {
        args.extend(["--scene".into(), options.scene.name().into()]);
    }
    if options.preset.is_team() {
        args.push("--team".into());
        if options.panes {
            args.push("--tmux".into());
        }
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
        ActionCommand::NewSession => {
            // The prompt is launched by a helper without a terminal. Create
            // detached, then switch the client that opened this menu.
            tmux(&[
                "command-prompt",
                "-t",
                client,
                "-p",
                tr("New session name:", "新会话名称："),
                &format!(
                    "new-session -d -s \"%%%\" -c {} ; switch-client -c {} -t \"=%1%\"",
                    shell_quote(&context.cwd),
                    shell_quote(client)
                ),
            ])?;
        }
        ActionCommand::Agent(text) => {
            tmux(&["send-keys", "-t", &context.pane, "-l", &format!("{text} ")])?;
        }
        ActionCommand::Usage => {
            let status = Command::new("workbench-agent-usage")
                .args(["menu", client])
                .status()?;
            if !status.success() {
                return Err("could not open usage summary".into());
            }
        }
        ActionCommand::Botmux(operation) => {
            let socket = tmux(&[
                "display-message",
                "-p",
                "-t",
                &context.pane,
                "#{socket_path}",
            ])?;
            let root = tmux(&["display-message", "-p", "-t", &context.pane, "#{pane_pid}"])?;
            let pid = context
                .agent
                .as_ref()
                .map(|a| a.fingerprint.pid.to_string())
                .unwrap_or_default();
            let command = [
                "workbench-botmux",
                operation,
                &socket,
                &context.pane,
                &root,
                &pid,
            ]
            .iter()
            .map(|v| shell_quote(v))
            .collect::<Vec<_>>()
            .join(" ");
            tmux(&[
                "display-popup",
                "-E",
                "-c",
                client,
                "-t",
                &context.pane,
                "-d",
                &context.cwd,
                "-w",
                "85%",
                "-h",
                "75%",
                &command,
            ])?;
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
            if matches!(options.tool, Tool::Codex | Tool::Traex) {
                let output = Command::new(&args[0])
                    .args(&args[1..])
                    .arg("--check")
                    .env_remove("AGENT_TEAM_DIR")
                    .env_remove("AGENT_TEAM_ROLE")
                    .current_dir(&context.cwd)
                    .output()?;
                if !output.status.success() {
                    return Err(String::from_utf8_lossy(&output.stderr)
                        .trim()
                        .to_string()
                        .into());
                }
            }
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
        ActionCommand::Navigate(..) | ActionCommand::AgentTarget(..) | ActionCommand::Disabled => {
            unreachable!()
        }
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
                resume_session_id: None,
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
            for preset in [Preset::Single, Preset::Team] {
                for panes in [false, true] {
                    let args = launch_argv(&MenuOptions {
                        tool,
                        preset,
                        panes,
                        ..Default::default()
                    })
                    .unwrap();
                    assert_eq!(args.contains(&"--tmux".into()), panes && preset.is_team());
                    assert!(
                        args.iter()
                            .all(|a| !a.contains("approve") && !a.contains("permission"))
                    );
                }
            }
        }
    }
    #[test]
    fn codex_has_three_scenes_two_modes_without_adjustments() {
        for scene in [Scene::Daily, Scene::Hard, Scene::Batch] {
            for preset in [Preset::Single, Preset::Team] {
                let args = launch_argv(&MenuOptions {
                    tool: Tool::Codex,
                    scene,
                    preset,
                    ..Default::default()
                })
                .unwrap();
                assert_eq!(
                    &args[..5],
                    ["agent-team", "start", "codex", "--scene", scene.name()]
                );
                assert_eq!(args.contains(&"--team".into()), preset == Preset::Team);
            }
        }
    }

    #[test]
    fn simple_tools_have_only_two_modes_and_no_scene_arguments() {
        for tool in [Tool::Claude, Tool::Opencode] {
            for preset in [Preset::Single, Preset::Team] {
                let args = launch_argv(&MenuOptions {
                    tool,
                    preset,
                    scene: Scene::Hard,
                    ..Default::default()
                })
                .unwrap();
                assert_eq!(&args[..3], ["agent-team", "start", tool.name()]);
                assert!(!args.iter().any(|arg| {
                    ["--scene", "--adjust", "--model", "--effort"].contains(&arg.as_str())
                }));
            }
            let (_, menu) = actions(
                StatusMenuKind::Tmux,
                &MenuOptions {
                    tool,
                    view: View::Configure,
                    ..Default::default()
                },
                &context(None, BaseState::Idle),
            );
            assert!(
                !menu
                    .iter()
                    .any(|a| a.label.contains("Budget") || a.label.contains("launch.toml"))
            );
        }
    }

    #[test]
    fn launch_choices_stay_in_the_same_panel_and_clear_errors() {
        for preset in [Preset::Single, Preset::Team] {
            let (_, choices) = actions(
                StatusMenuKind::Tmux,
                &MenuOptions {
                    view: View::Configure,
                    preset,
                    launch_error: Some("unavailable".into()),
                    ..Default::default()
                },
                &context(None, BaseState::Idle),
            );
            for action in &choices {
                if let ActionCommand::Navigate(_, next) = &action.command {
                    if action.id != "back" {
                        assert_eq!(next.view, View::Configure);
                        assert!(next.launch_error.is_none());
                    }
                }
            }
            assert_eq!(choices.iter().any(|a| a.id == "panes"), preset.is_team());
            assert!(
                !choices
                    .iter()
                    .any(|a| a.label.contains("临时") || a.label.contains("快速"))
            );
        }
        let summary = summary_lines(&serde_json::json!({"model":"lead", "effort":"high", "worker_model":"worker", "worker_effort":"max", "fast":false}), true).unwrap();
        assert_eq!(
            summary,
            [
                tr("Lead: lead · high", "主代理: lead · high"),
                tr("Worker: worker · max", "子代理: worker · max"),
                tr("Fast: off", "Fast: 关闭")
            ]
        );
        let route = MenuOptions {
            launch_error: Some("untrusted #{pane_id} ' text".into()),
            ..Default::default()
        }
        .route();
        assert!(!route.contains("untrusted"));
        let encoded = route.split(" --launch-error=").nth(1).unwrap();
        assert_eq!(
            decode_launch_error(encoded).unwrap(),
            "untrusted #{pane_id} ' text"
        );
        assert!(decode_launch_error("f").is_err());
        assert!(decode_launch_error("zz").is_err());
        assert!(decode_launch_error("ff").is_err());
        assert_eq!(
            decode_launch_error(&encode_launch_error("模型不可用")),
            Ok("模型不可用".into())
        );
        assert_eq!(
            summary_lines(
                &serde_json::json!({"worker_model":"custom", "worker_effort":"max"}),
                true
            )
            .unwrap(),
            [
                tr("Lead: tool default · default", "主代理: 工具默认 · default"),
                tr("Worker: custom · max", "子代理: custom · max")
            ]
        );
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
                    View::Launch,
                    View::Configure,
                    View::AgentTarget,
                    View::More,
                    View::Team,
                    View::Botmux,
                ] {
                    let (_, actions) = actions(
                        kind,
                        &MenuOptions {
                            view,
                            ..Default::default()
                        },
                        &context(Some(agent), BaseState::Idle),
                    );
                    let mut seen = std::collections::HashSet::from(['\x1b', '[', ']']);
                    for a in actions {
                        if !matches!(a.command, ActionCommand::Disabled) {
                            assert_eq!(a.key == crate::menu_footer::BACK_KEY, a.id == "back");
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
