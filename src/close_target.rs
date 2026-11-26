use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use crate::ipc::{Request, call};
use crate::manifest::ManifestSet;
use crate::menu::{Item, Menu, Role, select};
use crate::model::{AgentSnapshot, BaseState, DisplayState, HookHealth, Snapshot, StateSource};
use crate::paths::Paths;
use crate::process::{AgentProcess, ProcessSource, ProcessTree, accepts_terminal_input};
use crate::server::ServerIdentity;
use crate::sidebar::shell_quote;
use crate::tmux::{Tmux, TmuxSource, valid_pane_id};
use crate::ui_language::text as tr;

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

const CHANGED: &str = "The close target changed. Refresh the list and try again.";
pub(crate) const UNVERIFIED: &str = "Only idle or done agents can be closed.";

const PANE_FORMAT: &str =
    "#{pid}|#{start_time}|#{pane_id}|#{pane_pid}|#{pane_dead}|#{@pane_role}|#{pane_title}";

struct LivePane {
    key: String,
    root: u32,
    title: String,
}

pub struct Target<'a> {
    pub pane: &'a str,
    pub key: &'a str,
    pub agent: Option<&'a str>,
}

impl Target<'_> {
    /// Recheck the pane identity and the agent state on the machine that owns them.
    fn validate(&self, paths: &Paths, tmux: &Tmux, observe: bool) -> Result<Option<AgentProcess>> {
        let live = read_pane(tmux, self.pane)?;
        if live.key != self.key {
            return Err(CHANGED.into());
        }
        let manifests = ManifestSet::load(&paths.manifests_dir())?;
        let aliases = manifests.aliases();
        let mut tree = ProcessTree::default();
        let agent = tree
            .agents_for_roots(&[live.root], &aliases)
            .remove(&live.root);
        if !tree.only_agent_in_pane(live.root, &aliases, agent.as_ref()) {
            return Err(UNVERIFIED.into());
        }
        if self.agent.is_some_and(|id| {
            agent.as_ref().is_none_or(|agent| {
                crate::state_machine::instance_id(self.pane, &agent.fingerprint) != id
            })
        }) {
            return Err(CHANGED.into());
        }
        let mut hook_idle = false;
        if !observe {
            let value = call(
                &paths.socket_for_server(&tmux.server().key),
                &Request::new("snapshot.get", serde_json::Value::Null),
                Duration::from_millis(500),
            )
            .map_err(|_| UNVERIFIED)?;
            let snapshot: Snapshot = serde_json::from_value(value)?;
            let now = SystemTime::now().duration_since(UNIX_EPOCH)?.as_millis() as u64;
            if now.saturating_sub(snapshot.observed_at_unix_ms) > 5000 {
                return Err(UNVERIFIED.into());
            }
            let rows: Vec<_> = snapshot
                .agents
                .iter()
                .filter(|row| !row.exited && row.target.pane_id == self.pane)
                .collect();
            match (agent.as_ref(), rows.as_slice()) {
                (None, []) => {}
                (Some(live), [row])
                    if agent_closable(row) && row.process.as_ref() == Some(&live.fingerprint) =>
                {
                    hook_idle = row.state_source == StateSource::Hook
                        && row.hook_health == HookHealth::Healthy;
                }
                _ => return Err(UNVERIFIED.into()),
            }
        }
        if let Some(agent) = &agent {
            let content = tmux
                .capture_bottom(self.pane, 40, 16_384)
                .map_err(|_| CHANGED)?;
            let title = read_pane(tmux, self.pane)?;
            if title.key != live.key {
                return Err(CHANGED.into());
            }
            let screen = manifests.get(agent.kind).classify(&content, &title.title);
            let visible_prompt = screen.state == BaseState::Idle
                && screen.strong_visible_signal
                && !screen.skip_state_update
                && content.lines().any(|line| !line.trim().is_empty());
            if matches!(screen.state, BaseState::Working | BaseState::Blocked)
                || !(hook_idle || visible_prompt)
                || !tree
                    .interactive_foreground_roots(&[(live.root, agent.clone())].into())
                    .contains(&live.root)
                || !accepts_terminal_input(agent)
            {
                return Err(UNVERIFIED.into());
            }
        }
        Ok(agent)
    }

    fn execute(&self, paths: &Paths, server: &ServerIdentity, observe: bool) -> Result<()> {
        let tmux = Tmux::new(server.clone());
        let agent = self.validate(paths, &tmux, observe)?;
        if self.agent.is_some() {
            let agent = agent.ok_or(CHANGED)?;
            let pid = libc::pid_t::try_from(agent.fingerprint.pid)?;
            if pid <= 0
                || !accepts_terminal_input(&agent)
                || read_pane(&tmux, self.pane)?.key != self.key
            {
                return Err(CHANGED.into());
            }
            // The process fingerprint belongs to the confirmed live pane.
            if unsafe { libc::kill(pid, libc::SIGTERM) } != 0 {
                return Err(std::io::Error::last_os_error().into());
            }
        } else {
            let fields: Vec<_> = self.key.split('.').collect();
            let condition = format!(
                "#{{&&:#{{==:#{{pid}}.#{{start_time}}.#{{pane_id}}.#{{pane_pid}},{}.{}.{}.{}}},#{{&&:#{{==:#{{pane_dead}},0}},#{{!=:#{{@pane_role}},sidebar}}}}}}",
                fields[0], fields[1], self.pane, fields[3],
            );
            let result = tmux.output_with_timeout(
                &[
                    "if-shell",
                    "-F",
                    "-t",
                    self.pane,
                    &condition,
                    &format!("kill-pane -t {}", self.pane),
                    "display-message -p changed",
                ],
                Duration::from_secs(2),
            )?;
            if !result.is_empty() {
                return Err(CHANGED.into());
            }
        }
        Ok(())
    }

    fn confirm(&self, paths: &Paths, server: &ServerIdentity, client: &str) -> Result<()> {
        self.validate(paths, &Tmux::new(server.clone()), false)?;
        let prompt = if self.agent.is_some() {
            tr("Close agent", "关闭智能体")
        } else {
            tr("Close pane", "关闭窗格")
        };
        let menu = Menu {
            title: format!("{prompt} {}?", self.pane),
            items: [
                ("cancel", tr("Cancel", "取消"), "n"),
                ("confirm", prompt, "y"),
            ]
            .into_iter()
            .map(|(id, label, key)| Item {
                id: id.into(),
                label: label.into(),
                subtitle: None,
                key: key.into(),
                command: String::new(),
                enabled: true,
                role: Role::Body,
            })
            .collect(),
            searchable: false,
            prompt: None,
            copy_key: None,
            wrap_items: false,
        };
        if select(&menu, self.pane, client, 0)?.is_some_and(|choice| choice.index == 1) {
            self.execute(paths, server, false)?;
        }
        Ok(())
    }

    fn native_args(&self, client: &str) -> Vec<String> {
        let mut args = vec![
            "close-target".into(),
            "--pane".into(),
            self.pane.into(),
            "--key".into(),
            self.key.into(),
            "--client".into(),
            client.into(),
            "--language".into(),
            crate::ui_language::name().into(),
            "--confirm".into(),
        ];
        if let Some(agent) = self.agent {
            args.extend(["--agent".into(), agent.into()]);
        }
        args
    }
}

pub fn run(
    target: Target<'_>,
    paths: &Paths,
    server: &ServerIdentity,
    observe: bool,
    client: Option<&str>,
    confirm: bool,
    language: Option<&str>,
) -> Result<()> {
    crate::ui_language::set_language(language);
    let result = if confirm {
        target.confirm(paths, server, client.ok_or("tmux client is required")?)
    } else {
        target.execute(paths, server, observe)
    };
    if let (Err(error), Some(client)) = (&result, client) {
        report(server, client, &error.to_string());
    }
    result
}

pub(crate) fn report(server: &ServerIdentity, client: &str, error: &str) {
    let message = match error {
        CHANGED => tr(CHANGED, "关闭目标已变化，请刷新列表后重试。"),
        UNVERIFIED => tr(UNVERIFIED, "只能关闭空闲或已完成的智能体。"),
        _ => tr("Could not close the target.", "无法关闭所选目标。"),
    };
    let _ = Tmux::new(server.clone()).output_with_timeout(
        &["display-message", "-c", client, "-d", "3000", message],
        Duration::from_secs(2),
    );
}

fn read_pane(tmux: &Tmux, pane: &str) -> Result<LivePane> {
    if !valid_pane_id(pane) {
        return Err(CHANGED.into());
    }
    let text = tmux
        .output_with_timeout(
            &["display-message", "-p", "-t", pane, PANE_FORMAT],
            Duration::from_secs(2),
        )
        .map_err(|_| CHANGED)?;
    let parts: Vec<_> = text.trim_end_matches('\n').splitn(7, '|').collect();
    let [server, start, id, root, "0", role, title] = parts.as_slice() else {
        return Err(CHANGED.into());
    };
    if *id != pane
        || *role == "sidebar"
        || server.parse::<u32>().is_err()
        || start.parse::<u64>().is_err()
    {
        return Err(CHANGED.into());
    }
    Ok(LivePane {
        key: format!("{server}.{start}.{}.{root}", &pane[1..]),
        root: root.parse()?,
        title: (*title).to_owned(),
    })
}

pub(crate) fn agent_closable(agent: &AgentSnapshot) -> bool {
    !agent.exited
        && !agent.stale
        && agent.process.is_some()
        && agent.base_state == BaseState::Idle
        && matches!(agent.display_state, DisplayState::Idle | DisplayState::Done)
}

pub(crate) fn is_shortcut(key: KeyEvent) -> bool {
    !key.modifiers
        .intersects(KeyModifiers::ALT | KeyModifiers::CONTROL)
        && (key.code == KeyCode::Char('D')
            || key.code == KeyCode::Char('d') && key.modifiers.contains(KeyModifiers::SHIFT))
}

pub(crate) fn confirmation_args(
    server: &ServerIdentity,
    pane: &str,
    agent: Option<&str>,
    client: &str,
) -> Result<Vec<String>> {
    let live = read_pane(&Tmux::new(server.clone()), pane)?;
    Ok(Target {
        pane,
        key: &live.key,
        agent,
    }
    .native_args(client))
}

pub(crate) fn shell_command(args: &[String]) -> Result<String> {
    let executable = std::env::current_exe()?;
    let mut command = shell_quote(&executable.to_string_lossy());
    for argument in args {
        command.push(' ');
        command.push_str(&shell_quote(argument));
    }
    Ok(command)
}
