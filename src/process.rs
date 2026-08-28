use std::collections::HashMap;
use std::ffi::OsStr;
use std::path::Path;

use sysinfo::{Pid, Process, ProcessesToUpdate, System};

use crate::model::{AgentKind, ProcessFingerprint};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentProcess {
    pub kind: AgentKind,
    pub fingerprint: ProcessFingerprint,
}

pub trait ProcessSource {
    fn agents_for_roots(
        &mut self,
        roots: &[u32],
        aliases: &HashMap<String, AgentKind>,
    ) -> HashMap<u32, AgentProcess>;
}

pub struct ProcessTree {
    system: System,
}

impl Default for ProcessTree {
    fn default() -> Self {
        Self {
            system: System::new(),
        }
    }
}

impl ProcessSource for ProcessTree {
    fn agents_for_roots(
        &mut self,
        roots: &[u32],
        aliases: &HashMap<String, AgentKind>,
    ) -> HashMap<u32, AgentProcess> {
        self.system.refresh_processes(ProcessesToUpdate::All, true);
        roots
            .iter()
            .filter_map(|root| find_agent(&self.system, *root, aliases).map(|agent| (*root, agent)))
            .collect()
    }
}

fn find_agent(
    system: &System,
    root: u32,
    aliases: &HashMap<String, AgentKind>,
) -> Option<AgentProcess> {
    let root = Pid::from_u32(root);
    let mut best: Option<(usize, &Process, AgentKind)> = None;
    for process in system.processes().values() {
        let Some(kind) = identify(process, aliases) else {
            continue;
        };
        let Some(depth) = descendant_depth(system, process.pid(), root) else {
            continue;
        };
        if best.is_none_or(|(best_depth, _, _)| depth >= best_depth) {
            best = Some((depth, process, kind));
        }
    }
    best.map(|(_, process, kind)| AgentProcess {
        kind,
        fingerprint: ProcessFingerprint {
            pid: process.pid().as_u32(),
            started_at_ticks: process.start_time(),
            executable: process
                .exe()
                .unwrap_or_else(|| Path::new(process.name()))
                .display()
                .to_string(),
        },
    })
}

fn descendant_depth(system: &System, mut pid: Pid, root: Pid) -> Option<usize> {
    for depth in 0..128 {
        if pid == root {
            return Some(depth);
        }
        pid = system.process(pid)?.parent()?;
    }
    None
}

fn identify(process: &Process, aliases: &HashMap<String, AgentKind>) -> Option<AgentKind> {
    let name = process.name();
    let exe_name = process.exe().and_then(Path::file_name).unwrap_or(name);
    identify_token(exe_name, aliases).or_else(|| {
        let runtime = exe_name.to_string_lossy().to_ascii_lowercase();
        let runtime = runtime.strip_suffix(".exe").unwrap_or(&runtime);
        if !matches!(runtime, "node" | "bun" | "deno") {
            return None;
        }
        process.cmd().iter().find_map(|arg| {
            identify_token(
                Path::new(arg).file_name().unwrap_or(arg.as_os_str()),
                aliases,
            )
        })
    })
}

fn identify_token(token: &OsStr, aliases: &HashMap<String, AgentKind>) -> Option<AgentKind> {
    let value = token.to_string_lossy().to_ascii_lowercase();
    let stem = value
        .strip_suffix(".exe")
        .or_else(|| value.strip_suffix(".js"))
        .unwrap_or(&value);
    aliases.get(stem).copied()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exact_aliases_do_not_treat_shell_text_as_agent() {
        let aliases = HashMap::from([
            ("codex".into(), AgentKind::Codex),
            ("claude".into(), AgentKind::Claude),
        ]);
        assert_eq!(
            identify_token(OsStr::new("codex"), &aliases),
            Some(AgentKind::Codex)
        );
        assert_eq!(
            identify_token(OsStr::new("claude.js"), &aliases),
            Some(AgentKind::Claude)
        );
        assert_eq!(identify_token(OsStr::new("my-codex-notes"), &aliases), None);
        assert_eq!(identify_token(OsStr::new("zsh"), &aliases), None);
    }
}
