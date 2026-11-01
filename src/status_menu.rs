use crate::ui_language::text as tr;
use crate::{
    menu_context::{Context, Result, executable, tmux},
    model::{AgentKind, BaseState},
    paths::Paths,
};
use clap::{Args, ValueEnum};
use serde::{Deserialize, Serialize};
use std::os::unix::fs::OpenOptionsExt;
use std::path::PathBuf;
use std::process::Command;

const CODEX_RADAR_MENU_CATEGORY_LIMIT: usize = 4;

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum StatusMenuKind {
    Menu,
    Sidebar,
    Host,
    Tmux,
    Agent,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum, Default)]
pub enum View {
    #[default]
    Root,
    Interface,
    Switch,
    Extract,
    Create,
    CreatePane,
    Kill,
    KillAgent,
    KillWindow,
    KillPane,
    KillSession,
    Panes,
    Launch,
    Configure,
    AgentTarget,
    More,
    ResetCard,
    Team,
    Botmux,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
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
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
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
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Scene {
    #[default]
    Daily,
    Hard,
    Batch,
    Computer,
}
impl Scene {
    fn name(self) -> &'static str {
        match self {
            Self::Daily => "daily",
            Self::Hard => "hard",
            Self::Batch => "batch",
            Self::Computer => "computer",
        }
    }
    fn label(self) -> &'static str {
        match self {
            Self::Daily => tr("Daily development · daily", "日常开发 · daily"),
            Self::Hard => tr("Complex tasks · hard", "复杂攻坚 · hard"),
            Self::Batch => tr("Batch work · batch", "批量执行 · batch"),
            Self::Computer => tr("Computer automation · computer", "电脑自动化 · computer"),
        }
    }

    fn choices(tool: Tool) -> &'static [(Self, char)] {
        match tool {
            Tool::Codex => &[],
            Tool::Traex => &[(Self::Daily, 'd'), (Self::Hard, 'h'), (Self::Batch, 'b')],
            _ => &[],
        }
    }
}

#[derive(Debug, Clone, Args, Default)]
pub struct MenuOptions {
    /// Read the menu model and guarded callbacks without opening a popup.
    #[arg(long, hide = true)]
    pub json: bool,
    #[arg(long, hide = true)]
    pub from_menu: bool,
    #[arg(long, hide = true)]
    radar_prepared: bool,
    #[arg(long, value_enum, default_value = "daily")]
    pub scene: Scene,
    #[arg(long, hide = true, value_parser = parse_recommendation_id)]
    pub recommendation: Option<String>,
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
        let mut route = format!(
            "--view {} --tool {} --preset {}{}{}{}{} --language {}{}",
            self.view.to_possible_value().unwrap().get_name(),
            self.tool.name(),
            self.preset.name(),
            if self.tool == Tool::Traex {
                format!(" --scene {}", self.scene.name())
            } else {
                String::new()
            },
            self.recommendation
                .as_deref()
                .filter(|_| self.tool == Tool::Codex)
                .filter(|value| valid_recommendation_id(value))
                .map(|value| format!(" --recommendation {value}"))
                .unwrap_or_default(),
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
        );
        if self.from_menu {
            route.push_str(" --from-menu");
        }
        if self.radar_prepared {
            route.push_str(" --radar-prepared");
        }
        route
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

// Remember choices, never a cwd, session, prompt or shell command. Quick Agent
// starts a fresh session in the invoking pane using the launcher's configuration.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct LaunchSettings {
    tool: Tool,
    preset: Preset,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    scene: Option<Scene>,
    #[serde(default)]
    recommendation: Option<String>,
    panes: bool,
}

impl LaunchSettings {
    fn from_options(options: &MenuOptions) -> Self {
        Self {
            tool: options.tool,
            preset: options.preset,
            scene: (options.tool == Tool::Traex).then_some(options.scene),
            recommendation: if options.tool == Tool::Codex {
                options.recommendation.clone()
            } else {
                None
            },
            panes: options.preset.is_team() && options.panes,
        }
    }

    fn options(&self, options: &MenuOptions) -> MenuOptions {
        MenuOptions {
            tool: self.tool,
            preset: self.preset,
            scene: self.scene.unwrap_or_default(),
            recommendation: self.recommendation.clone(),
            panes: self.panes,
            launch_error: None,
            ..options.clone()
        }
    }

    fn load(paths: &Paths) -> Result<Option<Self>> {
        match std::fs::read(paths.state_dir.join("last-agent-launch.json")) {
            Ok(bytes) => {
                let settings: Self = serde_json::from_slice(&bytes)?;
                launch_argv(&settings.options(&MenuOptions::default()))?;
                Ok(Some(settings))
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(error.into()),
        }
    }

    fn save(&self, paths: &Paths) -> Result<()> {
        std::fs::create_dir_all(&paths.state_dir)?;
        let temporary = paths
            .state_dir
            .join(format!(".last-agent-launch-{}.json", uuid::Uuid::new_v4()));
        let result = (|| -> Result<()> {
            let file = std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(&temporary)?;
            serde_json::to_writer(file, self)?;
            std::fs::rename(&temporary, paths.state_dir.join("last-agent-launch.json"))?;
            Ok(())
        })();
        if result.is_err() {
            let _ = std::fs::remove_file(&temporary);
        }
        result
    }

    fn description(&self) -> String {
        let mut parts = vec![self.tool.label().to_owned(), self.preset.label().to_owned()];
        if self.tool == Tool::Codex {
            parts.insert(
                1,
                self.recommendation
                    .as_deref()
                    .and_then(|id| codex_radar_recommendation(id))
                    .and_then(|item| {
                        item.get("label")
                            .and_then(serde_json::Value::as_str)
                            .map(str::to_owned)
                    })
                    .unwrap_or_else(|| tr("Codex defaults", "Codex 默认值").into()),
            );
        } else if self.tool == Tool::Traex {
            if let Some(scene) = self.scene {
                parts.insert(1, scene.name().into());
            }
        }
        if self.preset.is_team() {
            parts.push(if self.panes { "tmux" } else { "Native" }.into());
        }
        parts.push("Auto approval".into());
        parts.join(" · ")
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
    subtitle: Option<String>,
    key: char,
    command: ActionCommand,
}
#[derive(Debug)]
enum ActionCommand {
    Navigate(StatusMenuKind, MenuOptions),
    Tmux(Vec<String>),
    NewSession,
    Switch(crate::switcher::Kind),
    Kill(KillTarget),
    SetUi(crate::ui::Mode),
    Sidebar,
    Metrics,
    Extract(&'static str),
    Agent(String),
    Usage,
    Host(String),
    Botmux(String),
    Launch(LaunchSettings),
    Focus(String),
    AgentTarget(String),
    Disabled,
}

#[derive(Debug, Clone, Copy)]
enum KillTarget {
    Agent,
    Window,
    Pane,
    Session,
}

impl KillTarget {
    fn label(self) -> &'static str {
        match self {
            Self::Agent => "Agent",
            Self::Window => "Window",
            Self::Pane => "Pane",
            Self::Session => "Session",
        }
    }
}
impl Action {
    fn new(id: &str, label: &str, key: char, command: ActionCommand) -> Self {
        Self {
            id: id.into(),
            label: label.into(),
            subtitle: None,
            key,
            command,
        }
    }
    fn note(label: &str) -> Self {
        Self::new("", label, ' ', ActionCommand::Disabled)
    }
}

fn quick_agent(settings: Result<Option<LaunchSettings>>) -> Action {
    let (subtitle, command) = match settings {
        Ok(Some(settings)) => {
            let available =
                executable(settings.tool.name()) && settings.preset.available(settings.tool);
            let subtitle = settings.description();
            (
                subtitle,
                if available {
                    ActionCommand::Launch(settings)
                } else {
                    ActionCommand::Disabled
                },
            )
        }
        Ok(None) => (
            tr("Create an Agent first", "先创建一次 Agent 即可复用").into(),
            ActionCommand::Disabled,
        ),
        Err(_) => (
            tr(
                "Saved combination unavailable; choose Agent",
                "上次配置不可用，请选择 Agent",
            )
            .into(),
            ActionCommand::Disabled,
        ),
    };
    Action {
        subtitle: Some(subtitle),
        ..Action::new("quick-agent", "Quick Agent", 'q', command)
    }
}

fn launch_context(kind: StatusMenuKind, view: View) -> bool {
    view != View::KillAgent
        && (kind != StatusMenuKind::Agent
            || matches!(view, View::Launch | View::Configure | View::AgentTarget))
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
        if options.from_menu {
            let next = MenuOptions {
                focus_agent: false,
                guard: Some(direct.guard.clone()),
                ..options.clone()
            };
            return run_with_context(
                StatusMenuKind::Agent,
                pane,
                client,
                None,
                0,
                stay_open,
                &next,
                &direct,
            );
        }
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
    if options.json && (action.is_some() || options.focus_agent) {
        return Err("JSON menu reads cannot execute an action or change focus".into());
    }
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

pub fn open_root(pane: Option<&str>, client: Option<&str>) -> Result<()> {
    let pane = pane
        .map(str::to_owned)
        .or_else(|| std::env::var("TMUX_PANE").ok())
        .ok_or("open the menu inside tmux")?;
    let client = client
        .map(str::to_owned)
        .or_else(|| std::env::var("WORKBENCH_TARGET_CLIENT").ok());
    let client = match client {
        Some(client) => client,
        None => {
            let clients = tmux(&["list-clients", "-F", "#{client_name}\t#{pane_id}"])?;
            let candidates: Vec<_> = clients
                .lines()
                .filter_map(|line| line.split_once('\t'))
                .filter(|(_, target)| *target == pane)
                .map(|(name, _)| name.to_owned())
                .collect();
            if candidates.len() != 1 {
                return Err("menu needs an explicit client; use the tmux menu binding".into());
            }
            candidates[0].clone()
        }
    };
    run(
        StatusMenuKind::Menu,
        &pane,
        &client,
        None,
        0,
        false,
        &MenuOptions::default(),
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
    let prepared_options = if !options.json
        && options.view == View::Configure
        && options.tool == Tool::Codex
        && !options.radar_prepared
    {
        let preparation = Command::new("agent-team")
            .args(["recommendations", "prepare"])
            .output();
        let mut next = options.clone();
        next.radar_prepared = true;
        match preparation {
            Ok(output) if output.status.success() => {
                let recommendations = codex_radar_menu_recommendations();
                let recommendation_is_available = next.recommendation.as_deref().is_some_and(|id| {
                    recommendations.iter().any(|item| {
                        item.get("id").and_then(serde_json::Value::as_str) == Some(id)
                    })
                });
                if !recommendation_is_available {
                    next.recommendation = recommendations
                        .first()
                        .and_then(|item| item.get("id"))
                        .and_then(serde_json::Value::as_str)
                        .map(str::to_owned);
                }
            }
            Ok(output) => {
                let detail = String::from_utf8_lossy(&output.stderr).trim().to_string();
                next.launch_error = Some(if detail.is_empty() {
                    "Could not prepare Codex recommendations".into()
                } else {
                    detail
                });
            }
            Err(error) => next.launch_error = Some(error.to_string()),
        }
        Some(next)
    } else {
        None
    };
    let options = prepared_options.as_ref().unwrap_or(options);
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
            if *next_kind == StatusMenuKind::Agent && next.focus_agent {
                return open_agent_status(pane, client, stay_open, next);
            }
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
        if let Err(error) = execute_action(selected, context, client, options, kind) {
            // This is an interactive callback: report the failure in the
            // client's status line instead of leaving a run-shell output pager.
            if let ActionCommand::Launch(settings) = &selected.command {
                let next = MenuOptions {
                    view: View::Configure,
                    launch_error: Some(error.to_string()),
                    ..settings.options(options)
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
                .enumerate()
                .map(|(index, line)| Action {
                    id: format!("summary:{index}"),
                    ..Action::note(&line)
                })
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
    let executable = std::env::current_exe()?;
    let base = format!(
        "{} status-menu {} --pane {} --client {} {} --guard {}",
        shell_quote(&executable.to_string_lossy()),
        kind.to_possible_value().unwrap().get_name(),
        shell_quote(pane),
        shell_quote(client),
        options.route(),
        shell_quote(&context.guard)
    );
    let items = actions
        .iter()
        .enumerate()
        .map(|(index, action)| {
            let enabled = !matches!(action.command, ActionCommand::Disabled);
            crate::menu::Item {
                id: if action.id.is_empty() {
                    format!("note:{index}")
                } else {
                    action.id.clone()
                },
                label: action.label.clone(),
                subtitle: action.subtitle.clone(),
                key: if enabled && action.key != '\0' {
                    crate::menu_footer::key_name(action.key)
                } else {
                    String::new()
                },
                command: if enabled {
                    format!(
                        "run-shell -b {}",
                        shell_quote(&format!("{base} --action {}", shell_quote(&action.id)))
                    )
                } else {
                    String::new()
                },
                enabled,
                role: match action.id.as_str() {
                    id if id.starts_with("summary:") => crate::menu::Role::Summary,
                    "back" => crate::menu::Role::Back,
                    "start" | "confirm-kill" => crate::menu::Role::Primary,
                    "launch-error" => crate::menu::Role::Error,
                    _ => crate::menu::Role::Body,
                },
            }
        })
        .collect();
    let menu = crate::menu::Menu {
        title,
        items,
        searchable: false,
        prompt: None,
        pager: Some(crate::menu::Pager {
            before_page: base,
            after_page: String::new(),
        }),
    };
    if options.json {
        println!("{}", serde_json::to_string(&menu)?);
        return Ok(());
    }
    crate::menu::display(menu, pane, client, page)
}

fn nav(id: &str, label: &str, key: char, kind: StatusMenuKind, options: MenuOptions) -> Action {
    Action::new(id, label, key, ActionCommand::Navigate(kind, options))
}
fn list_shortcut(index: usize) -> char {
    "123456789abcdefghijklmnopqrstuvwxyz"
        .chars()
        .nth(index)
        .unwrap_or('\0')
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
        StatusMenuKind::Menu => tr("Menu", "菜单").into(),
        StatusMenuKind::Sidebar => "Sidebar".into(),
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
        View::Extract => {
            title = tr("Extract", "提取 · Extract").into();
            for (filter, label, key) in [
                ("all", tr("General text", "通用文本"), 'a'),
                ("paragraph", tr("Paragraphs", "段落"), 'p'),
                ("url", "URL", 'u'),
                ("path", tr("File paths", "文件路径"), 'f'),
                ("line", tr("Lines", "整行"), 'l'),
                ("word", tr("Words / identifiers", "单词 / 标识符"), 'w'),
                ("quote", tr("Double-quoted text", "双引号文本"), 'd'),
                ("s-quote", tr("Single-quoted text", "单引号文本"), 's'),
            ] {
                let mut action = Action::new(
                    &format!("extract:{filter}"),
                    label,
                    key,
                    ActionCommand::Extract(filter),
                );
                action.subtitle = Some(
                    if filter == "url" {
                        tr("Enter: open on this device", "回车：在当前设备打开")
                    } else {
                        tr("Enter: copy · Tab: insert", "回车：复制 · Tab：插入")
                    }
                    .into(),
                );
                actions.push(action);
            }
        }
        View::Switch => {
            title = tr("Switch", "切换 · Switch").into();
            for (target, id, label, key) in [
                (crate::switcher::Kind::Agent, "agents", "Agent", 'a'),
                (crate::switcher::Kind::Window, "windows", "Window", 'w'),
                (crate::switcher::Kind::Pane, "switch-panes", "Pane", 'p'),
                (crate::switcher::Kind::Session, "sessions", "Session", 's'),
            ] {
                actions.push(Action::new(id, label, key, ActionCommand::Switch(target)));
            }
        }
        View::Create => {
            title = tr("Create", "创建 · Create").into();
            actions.push(quick_agent((|| LaunchSettings::load(&Paths::discover()?))()));
            actions.extend([
                nav("launch", "Agent", 'a', kind, options.at(View::Launch)),
                tmux_action("new", "Window", 'w', &["new-window"]),
                nav(
                    "create-pane",
                    "Pane",
                    'p',
                    kind,
                    options.at(View::CreatePane),
                ),
                Action::new("new-session", "Session", 's', ActionCommand::NewSession),
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
        }
        View::CreatePane => {
            title = tr("Create Pane", "创建 Pane").into();
            back = View::Create;
            actions.extend([
                tmux_action(
                    "below",
                    tr("Split below", "向下分割"),
                    '-',
                    &["split-window", "-v"],
                ),
                tmux_action(
                    "right",
                    tr("Split right", "向右分割"),
                    '|',
                    &["split-window", "-h"],
                ),
            ]);
        }
        View::Kill => {
            title = "Kill".into();
            for (view, id, label, key) in [
                (View::KillAgent, "kill-agent", "Agent", 'a'),
                (View::KillWindow, "kill-window", "Window", 'w'),
                (View::KillPane, "kill-pane", "Pane", 'p'),
                (View::KillSession, "kill-session", "Session", 's'),
            ] {
                actions.push(nav(id, label, key, kind, options.at(view)));
            }
        }
        View::KillAgent | View::KillWindow | View::KillPane | View::KillSession => {
            back = View::Kill;
            let (target, detail) = match options.view {
                View::KillAgent => (KillTarget::Agent, agent_title(context)),
                View::KillWindow => (KillTarget::Window, tmux(&["display-message", "-p", "-t", &context.pane, "Window #{window_index}:#{window_name} (#{window_id}) · Session #{session_name}"]).unwrap_or_else(|_| context.window.clone())),
                View::KillPane => (KillTarget::Pane, tmux(&["display-message", "-p", "-t", &context.pane, "Pane #{pane_index} (#{pane_id}) · #{pane_current_command} · Window #{window_index}:#{window_name}"]).unwrap_or_else(|_| context.pane.clone())),
                View::KillSession => (KillTarget::Session, tmux(&["display-message", "-p", "-t", &context.pane, "Session #{session_name} (#{session_id})"]).unwrap_or_else(|_| context.session.clone())),
                _ => unreachable!(),
            };
            title = format!("Kill {}?", target.label());
            actions.push(Action {
                id: "summary:target".into(),
                ..Action::note(&detail)
            });
            if !matches!(target, KillTarget::Agent) || context.agent.is_some() {
                actions.push(Action::new(
                    "confirm-kill",
                    &format!("Kill {}", target.label()),
                    'y',
                    ActionCommand::Kill(target),
                ));
            }
        }
        View::Interface => {
            let current = crate::ui::Mode::current().unwrap_or_default();
            for (mode, id, label, key) in [
                (
                    crate::ui::Mode::Old,
                    "ui-old",
                    tr("Classic · tmux menus", "旧版 · tmux 菜单"),
                    'o',
                ),
                (
                    crate::ui::Mode::New,
                    "ui-new",
                    tr("New · list menus", "新版 · 列表菜单"),
                    'n',
                ),
            ] {
                actions.push(Action::new(
                    id,
                    &format!("{} {label}", if current == mode { "●" } else { "○" }),
                    key,
                    ActionCommand::SetUi(mode),
                ));
            }
        }
        View::AgentTarget => {
            title = tr("Choose agent", "选择 agent").into();
            if let Ok(targets) = agent_targets(&context.window) {
                for (index, (pane, _)) in targets.iter().enumerate() {
                    actions.push(Action::new(
                        &format!("target:{pane}"),
                        &format!("Agent {pane}"),
                        list_shortcut(index),
                        ActionCommand::AgentTarget(pane.clone()),
                    ));
                }
            }
        }
        View::Launch => {
            title = tr("Launch · choose agent", "启动 · 选择 agent").into();
            if kind == StatusMenuKind::Tmux {
                back = View::Create;
            }
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
                            recommendation: None,
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
            if options.tool == Tool::Codex {
                let recommendations = codex_radar_menu_recommendations();
                if recommendations.is_empty() {
                    actions.push(Action::note(tr(
                        "CodexRadar recommendations unavailable · using Codex defaults",
                        "CodexRadar 推荐暂不可用 · 将使用 Codex 默认值",
                    )));
                    actions.push(nav(
                        "recommendation:default",
                        &choice_label(
                            tr(
                                "Codex defaults · native model and effort",
                                "Codex 默认值 · 使用原生模型与档位",
                            ),
                            true,
                        ),
                        list_shortcut(0),
                        kind,
                        MenuOptions {
                            recommendation: None,
                            launch_error: None,
                            ..options.at(View::Configure)
                        },
                    ));
                } else {
                    actions.push(Action::note(tr(
                        "CodexRadar recommendations",
                        "CodexRadar 站长推荐",
                    )));
                }
                for (index, recommendation) in recommendations.iter().enumerate() {
                    let Some(id) = recommendation.get("id").and_then(serde_json::Value::as_str)
                    else {
                        continue;
                    };
                    let Some(label) = recommendation
                        .get("label")
                        .and_then(serde_json::Value::as_str)
                    else {
                        continue;
                    };
                    if !valid_recommendation_id(id) {
                        continue;
                    }
                    let key = list_shortcut(index + 1);
                    if key == '\0' {
                        continue;
                    }
                    actions.push(nav(
                        &format!("recommendation:{id}"),
                        &choice_label(
                            &sanitize_remote_label(label),
                            options.recommendation.as_deref() == Some(id),
                        ),
                        key,
                        kind,
                        MenuOptions {
                            recommendation: Some(id.into()),
                            launch_error: None,
                            ..options.at(View::Configure)
                        },
                    ));
                }
            } else if options.tool == Tool::Traex {
                actions.push(Action::note(tr("Scene", "场景")));
                for &(scene, key) in Scene::choices(options.tool) {
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
            actions.push(Action::note("Auto approval"));
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
                    ActionCommand::Launch(LaunchSettings::from_options(options)),
                ));
            } else {
                actions.push(Action::note(tr(
                    "Required CLI / agent-team is unavailable",
                    "所需 CLI / agent-team 不可用",
                )));
            }
        }
        View::Panes => {
            title = tr("Layout", "布局 · Layout").into();
            actions.extend([
                tmux_action("zoom", "Zoom / unzoom", 'z', &["resize-pane", "-Z"]),
                tmux_action(
                    "even-horizontal",
                    "Even horizontal",
                    'h',
                    &["select-layout", "even-horizontal"],
                ),
                tmux_action(
                    "even-vertical",
                    "Even vertical",
                    'v',
                    &["select-layout", "even-vertical"],
                ),
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
            for (index, member) in context.team.iter().enumerate() {
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
                    list_shortcut(index),
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
            if kind == StatusMenuKind::Agent {
                back = View::More;
            }
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
            if kind == StatusMenuKind::Agent && botmux_available() {
                actions.push(nav(
                    "botmux",
                    "Botmux...",
                    'b',
                    kind,
                    options.at(View::Botmux),
                ));
            }
        }
        View::ResetCard => {
            title = tr("Codex reset cards", "Codex 重置卡").into();
            if kind == StatusMenuKind::Agent {
                actions.extend(reset_card_actions(context));
            }
        }
        View::Root => match kind {
            StatusMenuKind::Menu => {
                for (next_kind, id, label, key) in [
                    (StatusMenuKind::Agent, "agent", "Agent", 'a'),
                    (StatusMenuKind::Sidebar, "sidebar", "Sidebar", 's'),
                    (StatusMenuKind::Tmux, "tmux", "Tmux", 't'),
                    (
                        StatusMenuKind::Host,
                        "host",
                        tr("Host / SSH", "主机 / SSH"),
                        'h',
                    ),
                ] {
                    actions.push(nav(
                        id,
                        label,
                        key,
                        next_kind,
                        MenuOptions {
                            from_menu: true,
                            focus_agent: next_kind == StatusMenuKind::Agent,
                            ..options.at(View::Root)
                        },
                    ));
                }
                actions.push(nav(
                    "extract",
                    tr("Extract", "提取 · Extract"),
                    'e',
                    kind,
                    options.at(View::Extract),
                ));
                actions.push(Action::new(
                    "metrics",
                    tr("CPU / resources", "CPU / 资源"),
                    'c',
                    ActionCommand::Metrics,
                ));
                actions.push(nav(
                    "interface",
                    tr("Interface · classic / new", "交互 · 旧版 / 新版"),
                    'i',
                    kind,
                    options.at(View::Interface),
                ));
            }
            StatusMenuKind::Sidebar => {
                actions.push(Action::new(
                    "overview",
                    tr("Open Sidebar", "打开 Sidebar"),
                    'b',
                    ActionCommand::Sidebar,
                ));
                for (target, id, label, key) in [
                    (
                        crate::switcher::Kind::Session,
                        "sessions",
                        tr("Switch session", "切换会话"),
                        's',
                    ),
                    (
                        crate::switcher::Kind::Agent,
                        "agents",
                        tr("Switch agent", "切换 Agent"),
                        'a',
                    ),
                    (
                        crate::switcher::Kind::Window,
                        "windows",
                        tr("Switch window", "切换窗口"),
                        'w',
                    ),
                    (
                        crate::switcher::Kind::Pane,
                        "panes",
                        tr("Switch pane", "切换窗格"),
                        'p',
                    ),
                ] {
                    actions.push(Action::new(id, label, key, ActionCommand::Switch(target)));
                }
            }
            StatusMenuKind::Tmux => {
                actions.extend([
                    nav(
                        "create",
                        tr("Create", "创建 · Create"),
                        'c',
                        kind,
                        options.at(View::Create),
                    ),
                    nav(
                        "switch",
                        tr("Switch", "切换 · Switch"),
                        's',
                        kind,
                        options.at(View::Switch),
                    ),
                    nav(
                        "extract",
                        tr("Extract", "提取 · Extract"),
                        'e',
                        kind,
                        options.at(View::Extract),
                    ),
                    nav(
                        "panes",
                        tr("Layout", "布局 · Layout"),
                        'l',
                        kind,
                        options.at(View::Panes),
                    ),
                    nav("kill", "Kill", 'k', kind, options.at(View::Kill)),
                    tmux_action("detach", "Detach", 'd', &["detach-client"]),
                ]);
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
                actions.push(Action::new(
                    "usage",
                    if reset_card_installed(context) {
                        tr("Usage & reset cards...", "用量与重置卡...")
                    } else {
                        tr("Usage summary...", "使用量总结...")
                    },
                    'u',
                    ActionCommand::Usage,
                ));
                if !agent_commands(context, true).is_empty() || botmux_available() {
                    actions.push(nav(
                        "more",
                        tr("More...", "更多..."),
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
                            .enumerate()
                        {
                            actions.push(Action::new(
                                &format!("host:{host}"),
                                host,
                                list_shortcut(index),
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
        actions.push(nav(
            "back",
            crate::menu_footer::back_label(),
            crate::menu_footer::BACK_KEY,
            kind,
            options.at(back),
        ));
    } else if options.from_menu && kind != StatusMenuKind::Menu {
        actions.push(nav(
            "back",
            crate::menu_footer::back_label(),
            crate::menu_footer::BACK_KEY,
            StatusMenuKind::Menu,
            MenuOptions {
                from_menu: false,
                ..options.at(View::Root)
            },
        ));
    }
    (title, actions)
}

fn botmux_available() -> bool {
    executable("botmux") && executable("python3") && executable("workbench-botmux")
}

fn reset_card_installed(context: &Context) -> bool {
    context.agent.as_ref().is_some_and(|agent| {
        agent.kind == AgentKind::Codex
            && crate::menu_skills::installed(agent.kind, &context.cwd, "codex-reset-card")
    })
}

fn reset_card_actions(context: &Context) -> Vec<Action> {
    if !context.input_available
        || !matches!(context.state, BaseState::Idle | BaseState::Working)
        || !reset_card_installed(context)
    {
        return vec![];
    }
    [
        (
            "reset-card-schedule",
            tr("Schedule card · 5 min before expiry", "到期前 5 分钟用卡"),
            's',
            tr(
                "Schedule the earliest-expiring available Codex reset card for 5 minutes before its expiration. If a task already exists for this card, verify and reuse it. Schedule only this one card, and report the execution time, expiration and task status.",
                "查询可用 Codex 重置卡，将最早到期的一张安排在到期前 5 分钟使用；如果已有该卡的定时任务，核对并复用。只安排这一张，完成后报告执行时间、到期时间和任务状态。",
            ),
        ),
        (
            "reset-card-manage",
            tr("View / adjust scheduled tasks", "查看／调整定时任务"),
            'a',
            tr(
                "Inspect existing Codex reset-card timers and redemption receipts. List each task's name, selected card, execution time and current status so I can cancel or reschedule it.",
                "查询现有 Codex 重置卡定时任务及执行回执，列出任务名称、对应卡片、执行时间和当前状态，以便我取消或改期。",
            ),
        ),
    ]
    .into_iter()
    .map(|(id, label, key, request)| {
        Action::new(
            id,
            label,
            key,
            ActionCommand::Agent(format!("$codex-reset-card {request}")),
        )
    })
    .collect()
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
            ("/fast", 'f', true, false),
            ("/plan", 'p', false, true),
            ("/compact", 'c', false, true),
            ("/fork", 'f', false, true),
            ("/diff", 'd', true, true),
            ("/status", 'i', true, true),
            ("/model", 'm', true, true),
            ("/permissions", 'a', true, true),
        ],
        AgentKind::Claude => &[
            ("/btw", 'b', true, false),
            ("/plan", 'p', false, true),
            ("/compact", 'c', false, true),
            ("/status", 'i', false, true),
            ("/model", 'm', false, true),
            ("/permissions", 'a', false, true),
            ("/resume", 'r', false, true),
            ("/help", 'h', false, true),
        ],
        // TraeX distinguishes a tool-free /btw answer from a /side thread.
        // Keep the common quick question in front and the separate thread in More.
        AgentKind::Trae => &[
            ("/goal", 'g', false, false),
            ("/btw", 'b', true, false),
            ("/plan", 'p', false, true),
            ("/compact", 'c', false, true),
            ("/side", 's', false, true),
            ("/help", 'h', false, true),
        ],
        AgentKind::Opencode => &[
            ("/sessions", 's', false, false),
            ("/compact", 'c', false, true),
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

// Preview fields from the launcher's local JSON and the active CodexRadar cache.
// Preparing the cache happens once when the interactive Codex Configure view opens.
// The launcher remains responsible for validation and the actual launch plan.
fn launch_summary(options: &MenuOptions) -> Result<Vec<String>> {
    let team = options.preset.is_team();
    if options.tool == Tool::Codex {
        let mut config = serde_json::Map::new();
        if let Some(id) = options.recommendation.as_deref() {
            if let Some(recommendation) = codex_radar_recommendation(id) {
                for key in ["model", "effort"] {
                    if let Some(value) = recommendation.get(key).and_then(serde_json::Value::as_str)
                    {
                        config.insert(key.into(), serde_json::Value::String(value.into()));
                    }
                }
                if team {
                    for (target, source) in [("worker_model", "model"), ("worker_effort", "effort")]
                    {
                        if let Some(value) = recommendation
                            .get(source)
                            .and_then(serde_json::Value::as_str)
                        {
                            config.insert(target.into(), serde_json::Value::String(value.into()));
                        }
                    }
                }
            }
        }
        config.insert("fast".into(), serde_json::Value::Bool(false));
        let missing = resolve_codex_summary_tiers(&mut config, team);
        let mut lines = summary_lines(&serde_json::Value::Object(config), team)?;
        if missing {
            lines.push(
                tr(
                    "No CodexRadar tier selected; using Codex defaults",
                    "未选择可用的 CodexRadar 档位；将使用 Codex 默认值",
                )
                .into(),
            );
        }
        return Ok(lines);
    }

    let scene_tool = options.tool == Tool::Traex;
    if !scene_tool && !team {
        return summary_lines(&serde_json::json!({}), false);
    }
    let launcher = std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default())
        .map(|dir| dir.join("agent-team"))
        .find(|path| path.is_file())
        .and_then(|path| path.canonicalize().ok());
    let teams_path = launcher
        .as_ref()
        .and_then(|path| path.parent())
        .and_then(|dir| dir.parent())
        .map(|root| root.join("config/teams.json"))
        .ok_or("agent-team configuration is missing")?;
    let defaults: serde_json::Value = serde_json::from_slice(&std::fs::read(teams_path)?)?;
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
    let (base, extra) = if scene_tool {
        (
            defaults
                .get(tool)
                .and_then(|value| value.get("scenes"))
                .and_then(|value| value.get(options.scene.name())),
            personal
                .get(tool)
                .and_then(|value| value.get("scenes"))
                .and_then(|value| value.get(options.scene.name())),
        )
    } else {
        (
            defaults.get(tool).and_then(|value| value.get("team")),
            personal.get(tool).and_then(|value| value.get("team")),
        )
    };
    let mut config = base
        .and_then(serde_json::Value::as_object)
        .ok_or("agent-team configuration is missing")?
        .clone();
    if let Some(extra) = extra.and_then(serde_json::Value::as_object) {
        config.extend(extra.clone());
    }
    summary_lines(&serde_json::Value::Object(config), team)
}

fn valid_recommendation_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 96
        && value.as_bytes()[0].is_ascii_alphanumeric()
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"._-".contains(&byte))
}

fn parse_recommendation_id(value: &str) -> std::result::Result<String, String> {
    if valid_recommendation_id(value) {
        Ok(value.to_owned())
    } else {
        Err("invalid CodexRadar recommendation id".into())
    }
}

fn sanitize_remote_label(value: &str) -> String {
    value
        .chars()
        .filter(|character| !character.is_control() && *character != '#')
        .take(120)
        .collect()
}

fn codex_radar_cache() -> Option<serde_json::Value> {
    let cache_root = std::env::var_os("XDG_CACHE_HOME")
        .map(PathBuf::from)
        .or_else(|| dirs::home_dir().map(|home| home.join(".cache")))?;
    let cache_path = cache_root.join("agent-team/codexradar-recommendations.json");
    let bytes = std::fs::read(cache_path).ok()?;
    let cache: serde_json::Value = serde_json::from_slice(&bytes).ok()?;
    (cache.get("version").and_then(serde_json::Value::as_u64) == Some(2)).then_some(cache)
}

fn codex_radar_recommendations() -> Vec<serde_json::Value> {
    codex_radar_cache()
        .and_then(|cache| cache.get("recommendations").cloned())
        .and_then(|recommendations| recommendations.get("items").cloned())
        .and_then(|items| items.as_array().cloned())
        .unwrap_or_default()
        .into_iter()
        .filter(|item| {
            let id = item.get("id").and_then(serde_json::Value::as_str);
            let label = item.get("label").and_then(serde_json::Value::as_str);
            id.is_some_and(valid_recommendation_id)
                && label.is_some()
                && item
                    .get("model")
                    .and_then(serde_json::Value::as_str)
                    .is_some()
                && item
                    .get("effort")
                    .and_then(serde_json::Value::as_str)
                    .is_some()
        })
        .collect()
}

fn codex_radar_model_version(model: &str) -> Vec<u64> {
    let bytes = model.as_bytes();
    let Some(mut end) = bytes.iter().position(u8::is_ascii_digit) else {
        return Vec::new();
    };
    let start = end;
    while end < bytes.len() {
        if bytes[end].is_ascii_digit() {
            end += 1;
        } else if bytes[end] == b'.'
            && end + 1 < bytes.len()
            && bytes[end + 1].is_ascii_digit()
        {
            end += 1;
        } else {
            break;
        }
    }
    model[start..end]
        .split('.')
        .filter_map(|part| part.parse().ok())
        .collect()
}

fn codex_radar_menu_recommendations() -> Vec<serde_json::Value> {
    let mut categories: Vec<(String, Vec<serde_json::Value>)> = Vec::new();
    for recommendation in codex_radar_recommendations() {
        let category = recommendation
            .get("source_key")
            .and_then(serde_json::Value::as_str)
            .filter(|key| !key.is_empty())
            .or_else(|| recommendation.get("id").and_then(serde_json::Value::as_str));
        let Some(category) = category else {
            continue;
        };
        if let Some(index) = categories
            .iter()
            .position(|(key, _)| key.as_str() == category)
        {
            categories[index].1.push(recommendation);
        } else {
            categories.push((category.to_owned(), vec![recommendation]));
        }
    }

    categories
        .into_iter()
        .take(CODEX_RADAR_MENU_CATEGORY_LIMIT)
        .filter_map(|(_, choices)| {
            choices
                .into_iter()
                .enumerate()
                .max_by_key(|(index, recommendation)| {
                    let version = recommendation
                        .get("model")
                        .and_then(serde_json::Value::as_str)
                        .map(codex_radar_model_version)
                        .unwrap_or_default();
                    (version, *index)
                })
                .map(|(_, recommendation)| recommendation)
        })
        .collect()
}

fn codex_radar_recommendation(id: &str) -> Option<serde_json::Value> {
    if !valid_recommendation_id(id) {
        return None;
    }
    codex_radar_recommendations()
        .into_iter()
        .find(|item| item.get("id").and_then(serde_json::Value::as_str) == Some(id))
}

fn has_summary_tier_pair(
    config: &serde_json::Map<String, serde_json::Value>,
    model: &str,
    effort: &str,
) -> bool {
    config
        .get(model)
        .and_then(serde_json::Value::as_str)
        .is_some_and(|value| !value.is_empty())
        && config
            .get(effort)
            .and_then(serde_json::Value::as_str)
            .is_some_and(|value| !value.is_empty())
}

fn resolve_codex_summary_tiers(
    config: &mut serde_json::Map<String, serde_json::Value>,
    team: bool,
) -> bool {
    let selected_pair = if team && has_summary_tier_pair(config, "lead_model", "lead_effort") {
        Some(("lead_model", "lead_effort"))
    } else if has_summary_tier_pair(config, "model", "effort") {
        Some(("model", "effort"))
    } else {
        None
    };
    if let Some((model, effort)) = selected_pair {
        if team {
            for (source, target) in [(model, "model"), (effort, "effort")] {
                if let Some(value) = config.get(source).cloned() {
                    config.insert(target.into(), value);
                }
            }
        }
        if !has_summary_tier_pair(config, "worker_model", "worker_effort") {
            config.remove("worker_model");
            config.remove("worker_effort");
        }
    } else {
        for key in [
            "model",
            "effort",
            "lead_model",
            "lead_effort",
            "worker_model",
            "worker_effort",
        ] {
            config.remove(key);
        }
    }
    config.remove("lead_model");
    config.remove("lead_effort");
    config
        .entry("fast")
        .or_insert(serde_json::Value::Bool(false));
    selected_pair.is_none()
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
    if options.tool == Tool::Codex {
        if let Some(recommendation) = options.recommendation.as_deref() {
            if !valid_recommendation_id(recommendation) {
                return Err("Invalid CodexRadar recommendation id".into());
            }
            args.extend(["--recommendation".into(), recommendation.into()]);
        }
    } else if options.tool == Tool::Traex {
        if !Scene::choices(options.tool)
            .iter()
            .any(|(scene, _)| *scene == options.scene)
        {
            return Err(format!(
                "Unknown {} scene: {}",
                options.tool.name(),
                options.scene.name()
            )
            .into());
        }
        args.extend(["--scene".into(), options.scene.name().into()]);
    }
    if options.preset.is_team() {
        args.push("--team".into());
        if options.panes {
            args.push("--tmux".into());
        }
    }
    // agent-team otherwise defaults Team to YOLO, which conflicts with Codex's
    // automatic reviewer flag. Select auto explicitly for every tool and mode.
    args.push("--no-yolo".into());
    args.extend(match options.tool {
        Tool::Codex => vec!["--approve-for-me".into()],
        Tool::Claude | Tool::Traex => vec!["--permission-mode".into(), "auto".into()],
        Tool::Opencode => vec!["--auto".into()],
    });
    Ok(args)
}

fn execute_action(
    action: &Action,
    context: &Context,
    client: &str,
    options: &MenuOptions,
    kind: StatusMenuKind,
) -> Result<()> {
    match &action.command {
        ActionCommand::Kill(target) => {
            match target {
                KillTarget::Agent => {
                    // Context and its process fingerprint have just been read
                    // and compared with the confirmation's identity guard.
                    let agent = context.agent.as_ref().ok_or("Agent is no longer running")?;
                    let pid = libc::pid_t::try_from(agent.fingerprint.pid)?;
                    if pid <= 0 || unsafe { libc::kill(pid, libc::SIGTERM) } != 0 {
                        return Err(std::io::Error::last_os_error().into());
                    }
                }
                KillTarget::Window => {
                    tmux(&["kill-window", "-t", &context.window])?;
                }
                KillTarget::Pane => {
                    tmux(&["kill-pane", "-t", &context.pane])?;
                }
                KillTarget::Session => {
                    tmux(&["kill-session", "-t", &context.session])?;
                }
            }
        }
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
        ActionCommand::Switch(target_kind) => {
            crate::switcher::run(
                &Paths::discover()?,
                &crate::server::ServerIdentity::discover()?,
                *target_kind,
                &context.pane,
                client,
                false,
                Some(crate::ui_language::name()),
                if options.from_menu {
                    if kind == StatusMenuKind::Sidebar {
                        crate::switcher::ReturnTo::SidebarMenu
                    } else {
                        crate::switcher::ReturnTo::MenuFromMain
                    }
                } else {
                    crate::switcher::ReturnTo::Menu
                },
                0,
            )?;
        }
        ActionCommand::SetUi(mode) => {
            mode.set()?;
            run(
                StatusMenuKind::Menu,
                &context.pane,
                client,
                None,
                0,
                false,
                &MenuOptions {
                    language: options.language.clone(),
                    ..Default::default()
                },
            )?;
        }
        ActionCommand::Sidebar => {
            crate::sidebar::open_popup(
                &crate::server::ServerIdentity::discover()?,
                &context.pane,
                client,
            )?;
        }
        ActionCommand::Extract(filter) => {
            if !executable("tmux-extrakto") {
                return message(
                    client,
                    tr(
                        "Extract needs the tmux-extrakto helper installed on this host",
                        "提取功能需要在当前主机安装 tmux-extrakto",
                    ),
                );
            }
            let mut command = Command::new("tmux-extrakto");
            command.args(["--client", client]);
            if *filter == "url" {
                command.arg("--urls");
            } else {
                command.arg("--filter").arg(filter);
            }
            let output = command
                .arg(&context.pane)
                .current_dir(&context.cwd)
                .output()?;
            if !output.status.success() {
                let detail = String::from_utf8_lossy(&output.stderr);
                let detail = detail.trim().chars().take(512).collect::<String>();
                return message(
                    client,
                    if detail.is_empty() {
                        tr("Could not open Extract", "无法打开提取器")
                    } else {
                        &detail
                    },
                );
            }
        }
        ActionCommand::Metrics => {
            let plugin = tmux(&["show-option", "-gqv", "@workbench_plugin_dir"])?;
            let script = std::path::Path::new(&plugin).join("bin/workbench-host-metrics-menu");
            if plugin.is_empty() || !script.is_file() {
                return Err("resource menu unavailable; reload Workbench".into());
            }
            let parent = format!(
                "run-shell -b {}",
                shell_quote(&format!(
                    "{} status-menu menu --pane {} --client {} --language {}",
                    shell_quote(&std::env::current_exe()?.to_string_lossy()),
                    shell_quote(&context.pane),
                    shell_quote(client),
                    crate::ui_language::name()
                ))
            );
            if !Command::new(script)
                .args([client, &context.pane])
                .env("WORKBENCH_MENU_PARENT", parent)
                .status()?
                .success()
            {
                return Err("could not open resource menu".into());
            }
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
            let parent = format!(
                "run-shell -b {}",
                shell_quote(&format!(
                    "{} status-menu {} --pane {} --client {} {}",
                    shell_quote(&std::env::current_exe()?.to_string_lossy()),
                    kind.to_possible_value().unwrap().get_name(),
                    shell_quote(&context.pane),
                    shell_quote(client),
                    options.at(View::Root).route()
                ))
            );
            let mut command = Command::new("workbench-agent-usage");
            command
                .args(["menu", client])
                .env("WORKBENCH_MENU_PARENT", parent)
                .env("WORKBENCH_SOURCE_PANE", &context.pane)
                .env("WORKBENCH_USAGE_GUARD", &context.guard)
                .env("TMUX_AGENT_WORKBENCH_LANGUAGE", crate::ui_language::name());
            if let Some(agent) = &context.agent {
                command.env(
                    "WORKBENCH_USAGE_SOURCE",
                    match agent.kind {
                        AgentKind::Codex => "codex",
                        AgentKind::Claude => "claude",
                        AgentKind::Trae => "trae",
                        AgentKind::Opencode => "opencode",
                    },
                );
            }
            let status = command.status()?;
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
        ActionCommand::Launch(settings) => {
            let options = settings.options(options);
            let args = launch_argv(&options)?;
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
            if let Err(error) = (|| settings.save(&Paths::discover()?))() {
                // The new window already exists. Do not reopen Launch and
                // encourage an accidental duplicate when only saving failed.
                message(
                    client,
                    &format!("Agent started; could not save Quick Agent: {error}"),
                )?;
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
pub(crate) fn shell_quote(value: &str) -> String {
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
        assert!(cmds.iter().any(|a| a.id == "/fast"));
        assert!(!cmds.iter().any(|a| a.id == "/plan" || a.id == "/compact"));
        assert!(!cmds.iter().any(|a| a.id == "/fork"));
    }
    #[test]
    fn infrequent_commands_live_in_more_and_fast_is_codex_only() {
        for kind in [
            AgentKind::Codex,
            AgentKind::Claude,
            AgentKind::Trae,
            AgentKind::Opencode,
        ] {
            let context = context(Some(kind), BaseState::Idle);
            let main = agent_commands(&context, false);
            assert!(!main.iter().any(|a| a.id == "/plan" || a.id == "/compact"));
            assert_eq!(
                main.iter().any(|a| a.id == "/fast"),
                kind == AgentKind::Codex
            );
            let more = agent_commands(&context, true);
            assert!(more.iter().any(|a| a.id == "/compact"));
            if kind != AgentKind::Opencode {
                assert!(more.iter().any(|a| a.id == "/plan"));
            }
        }
    }
    #[test]
    fn reset_cards_require_a_codex_prompt_that_accepts_input() {
        for kind in [
            None,
            Some(AgentKind::Claude),
            Some(AgentKind::Trae),
            Some(AgentKind::Opencode),
        ] {
            assert!(reset_card_actions(&context(kind, BaseState::Idle)).is_empty());
        }
        for state in [BaseState::Blocked, BaseState::Unknown] {
            assert!(reset_card_actions(&context(Some(AgentKind::Codex), state)).is_empty());
        }
        let mut copied = context(Some(AgentKind::Codex), BaseState::Idle);
        copied.input_available = false;
        assert!(reset_card_actions(&copied).is_empty());
    }
    #[test]
    fn botmux_back_returns_to_its_parent() {
        for (kind, parent) in [
            (StatusMenuKind::Agent, View::More),
            (StatusMenuKind::Tmux, View::Root),
        ] {
            let (_, actions) = actions(
                kind,
                &MenuOptions {
                    view: View::Botmux,
                    ..Default::default()
                },
                &context(Some(AgentKind::Codex), BaseState::Idle),
            );
            let back = actions.iter().find(|action| action.id == "back").unwrap();
            assert!(matches!(
                &back.command,
                ActionCommand::Navigate(next, options) if *next == kind && options.view == parent
            ));
        }
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
    fn every_launch_combination_uses_automatic_approval_without_yolo() {
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
                    assert!(args.contains(&"--no-yolo".into()));
                    assert!(!args.contains(&"--yolo".into()));
                    let flags: &[&str] = match tool {
                        Tool::Codex => &["--approve-for-me"],
                        Tool::Claude | Tool::Traex => &["--permission-mode", "auto"],
                        Tool::Opencode => &["--auto"],
                    };
                    assert_eq!(&args[args.len() - flags.len()..], flags);
                }
            }
        }
    }

    #[test]
    fn quick_agent_remembers_choices_without_conversation_or_directory() {
        use std::os::unix::fs::PermissionsExt;
        let directory = tempfile::tempdir().unwrap();
        let paths = Paths {
            config_dir: directory.path().join("config"),
            state_dir: directory.path().join("state"),
            cache_dir: directory.path().join("cache"),
            runtime_dir: directory.path().join("runtime"),
        };
        assert!(LaunchSettings::load(&paths).unwrap().is_none());
        assert!(matches!(
            quick_agent(Ok(None)).command,
            ActionCommand::Disabled
        ));
        let options = MenuOptions {
            tool: Tool::Codex,
            scene: Scene::Computer,
            preset: Preset::Team,
            panes: true,
            launch_error: Some("old failure".into()),
            ..Default::default()
        };
        let settings = LaunchSettings::from_options(&options);
        settings.save(&paths).unwrap();
        assert_eq!(
            LaunchSettings::load(&paths).unwrap(),
            Some(settings.clone())
        );
        assert!(settings.options(&options).launch_error.is_none());
        let saved = paths.state_dir.join("last-agent-launch.json");
        assert_eq!(
            std::fs::metadata(&saved).unwrap().permissions().mode() & 0o777,
            0o600
        );
        let json: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&saved).unwrap()).unwrap();
        assert_eq!(json.as_object().unwrap().len(), 4);
        assert!(settings.description().contains("Codex"));
        assert!(settings.description().contains("tmux"));
        std::fs::write(
            &saved,
            r#"{"tool":"codex","preset":"team","scene":"unknown","panes":true}"#,
        )
        .unwrap();
        assert!(LaunchSettings::load(&paths).is_err());
        assert!(matches!(
            quick_agent(LaunchSettings::load(&paths)).command,
            ActionCommand::Disabled
        ));
        std::fs::write(
            &saved,
            r#"{"tool":"traex","preset":"single","scene":"computer","panes":false}"#,
        )
        .unwrap();
        assert!(LaunchSettings::load(&paths).is_err());
    }
    #[test]
    fn codex_uses_only_dynamic_recommendation_ids_or_native_defaults() {
        assert!(Scene::choices(Tool::Codex).is_empty());
        let default = launch_argv(&MenuOptions::default()).unwrap();
        assert_eq!(&default[..3], ["agent-team", "start", "codex"]);
        assert!(!default.contains(&"--scene".into()));

        let selected = launch_argv(&MenuOptions {
            tool: Tool::Codex,
            recommendation: Some("daily-development-a1b2c3d4".into()),
            preset: Preset::Team,
            ..Default::default()
        })
        .unwrap();
        assert_eq!(
            &selected[3..6],
            ["--recommendation", "daily-development-a1b2c3d4", "--team"]
        );
        assert!(
            launch_argv(&MenuOptions {
                recommendation: Some("bad #{id}".into()),
                ..Default::default()
            })
            .is_err()
        );
    }

    #[test]
    fn missing_codex_tiers_resolve_to_native_defaults_and_fast_off() {
        let mut config = serde_json::json!({
            "worker_model": "stale-worker",
            "worker_effort": "max",
            "fast": false
        })
        .as_object()
        .unwrap()
        .clone();
        assert!(resolve_codex_summary_tiers(&mut config, true));
        assert!(!config.contains_key("model"));
        assert!(!config.contains_key("effort"));
        assert!(!config.contains_key("worker_model"));
        assert!(!config.contains_key("worker_effort"));
        assert_eq!(config.get("fast"), Some(&serde_json::Value::Bool(false)));
        assert_eq!(
            summary_lines(&serde_json::Value::Object(config), true).unwrap(),
            vec![tr(
                "Model / thinking: tool defaults",
                "模型 / thinking: 工具默认配置"
            )]
        );
    }

    #[test]
    fn no_cached_codex_data_still_has_a_default_launch_choice() {
        assert!(Scene::choices(Tool::Codex).is_empty());
        assert_eq!(
            launch_argv(&MenuOptions {
                tool: Tool::Codex,
                preset: Preset::Team,
                ..Default::default()
            })
            .unwrap()
            .iter()
            .filter(|arg| arg.as_str() == "--scene")
            .count(),
            0
        );
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
        for kind in [
            StatusMenuKind::Menu,
            StatusMenuKind::Sidebar,
            StatusMenuKind::Tmux,
            StatusMenuKind::Agent,
        ] {
            for agent in [
                AgentKind::Codex,
                AgentKind::Claude,
                AgentKind::Trae,
                AgentKind::Opencode,
            ] {
                for view in [
                    View::Root,
                    View::Interface,
                    View::Switch,
                    View::Extract,
                    View::Create,
                    View::CreatePane,
                    View::Kill,
                    View::KillAgent,
                    View::KillWindow,
                    View::KillPane,
                    View::KillSession,
                    View::Panes,
                    View::Launch,
                    View::Configure,
                    View::AgentTarget,
                    View::More,
                    View::ResetCard,
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
    fn collection_routes_back_from_each_submenu() {
        let context = context(None, BaseState::Idle);
        let (_, root) = actions(StatusMenuKind::Menu, &MenuOptions::default(), &context);
        assert_eq!(
            root.iter()
                .map(|action| action.id.as_str())
                .collect::<Vec<_>>(),
            [
                "agent",
                "sidebar",
                "tmux",
                "host",
                "extract",
                "metrics",
                "interface"
            ]
        );
        for kind in [
            StatusMenuKind::Agent,
            StatusMenuKind::Sidebar,
            StatusMenuKind::Tmux,
            StatusMenuKind::Host,
        ] {
            let options = MenuOptions {
                from_menu: true,
                ..Default::default()
            };
            assert!(options.route().contains("--from-menu"));
            let (_, submenu) = actions(kind, &options, &context);
            assert!(submenu.iter().any(|action| matches!(
                &action.command,
                ActionCommand::Navigate(StatusMenuKind::Menu, options)
                    if action.id == "back" && !options.from_menu
            )));
        }
    }
    #[test]
    fn tmux_groups_share_object_order_and_return_to_their_parent() {
        let context = context(None, BaseState::Idle);
        let (_, root) = actions(StatusMenuKind::Tmux, &MenuOptions::default(), &context);
        assert_eq!(
            root.iter()
                .map(|action| action.id.as_str())
                .collect::<Vec<_>>(),
            ["create", "switch", "extract", "panes", "kill", "detach"]
        );
        for view in [View::Switch, View::Create, View::Kill] {
            let (_, group) = actions(
                StatusMenuKind::Tmux,
                &MenuOptions {
                    view,
                    ..Default::default()
                },
                &context,
            );
            assert_eq!(
                group
                    .iter()
                    .filter(|action| action.id != "quick-agent")
                    .take(4)
                    .map(|action| action.label.as_str())
                    .collect::<Vec<_>>(),
                ["Agent", "Window", "Pane", "Session"]
            );
            if view == View::Create {
                assert_eq!(group[0].id, "quick-agent");
                assert!(group[0].subtitle.is_some());
            }
            assert!(group.iter().any(|action| matches!(&action.command,
                ActionCommand::Navigate(StatusMenuKind::Tmux, next) if action.id == "back" && next.view == View::Root)));
            assert!(
                !group
                    .iter()
                    .any(|action| matches!(action.command, ActionCommand::Kill(_)))
            );
        }
        for (view, parent) in [
            (View::CreatePane, View::Create),
            (View::Launch, View::Create),
            (View::KillAgent, View::Kill),
            (View::KillWindow, View::Kill),
            (View::KillPane, View::Kill),
            (View::KillSession, View::Kill),
        ] {
            let (_, group) = actions(
                StatusMenuKind::Tmux,
                &MenuOptions {
                    view,
                    ..Default::default()
                },
                &context,
            );
            assert!(group.iter().any(|action| matches!(&action.command,
                ActionCommand::Navigate(StatusMenuKind::Tmux, next) if action.id == "back" && next.view == parent)));
            if view == View::KillAgent {
                assert!(
                    !group
                        .iter()
                        .any(|action| matches!(action.command, ActionCommand::Kill(_)))
                );
                assert!(!launch_context(StatusMenuKind::Tmux, view));
            }
        }
    }
    #[test]
    fn targets_and_labels_are_not_shell_or_tmux_code() {
        assert!(validate_pane("%12").is_ok());
        assert!(validate_pane("%12;kill-server").is_err());
        assert_eq!(shell_quote("dev'box"), "'dev'\\''box'");
    }
}
