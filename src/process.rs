use std::collections::{HashMap, HashSet};
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
        find_agents(&self.system, roots, aliases)
    }
}

fn find_agents(
    system: &System,
    roots: &[u32],
    aliases: &HashMap<String, AgentKind>,
) -> HashMap<u32, AgentProcess> {
    let roots: HashSet<_> = roots.iter().copied().map(Pid::from_u32).collect();
    let mut best: HashMap<Pid, (usize, &Process, AgentKind)> = HashMap::new();
    for process in system.processes().values() {
        let Some(kind) = identify(process, aliases) else {
            continue;
        };
        if !is_process_group_leader(process.pid().as_u32()) {
            continue;
        }
        let Some((root, depth)) = nearest_root(system, process.pid(), &roots) else {
            continue;
        };
        if best.get(&root).is_none_or(|(best_depth, current, _)| {
            process_candidate_precedes(
                (depth, process.start_time(), process.pid().as_u32()),
                (*best_depth, current.start_time(), current.pid().as_u32()),
            )
        }) {
            best.insert(root, (depth, process, kind));
        }
    }
    best.into_iter()
        .map(|(root, (_, process, kind))| {
            (
                root.as_u32(),
                AgentProcess {
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
                },
            )
        })
        .collect()
}

fn nearest_root(system: &System, mut pid: Pid, roots: &HashSet<Pid>) -> Option<(Pid, usize)> {
    for depth in 0..128 {
        if roots.contains(&pid) {
            return Some((pid, depth));
        }
        pid = system.process(pid)?.parent()?;
    }
    None
}

#[cfg(target_os = "linux")]
fn is_process_group_leader(pid: u32) -> bool {
    std::fs::read_to_string(format!("/proc/{pid}/status"))
        .ok()
        .and_then(|status| linux_tgid_from_status(&status))
        .is_none_or(|tgid| tgid == pid)
}

#[cfg(not(target_os = "linux"))]
fn is_process_group_leader(_pid: u32) -> bool {
    true
}

#[cfg(target_os = "linux")]
fn linux_tgid_from_status(status: &str) -> Option<u32> {
    status.lines().find_map(|line| {
        line.strip_prefix("Tgid:")
            .and_then(|value| value.trim().parse().ok())
    })
}

fn process_candidate_precedes(candidate: (usize, u64, u32), current: (usize, u64, u32)) -> bool {
    candidate < current
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

    #[test]
    fn closest_stable_agent_process_wins_over_transient_descendants() {
        assert!(process_candidate_precedes((1, 100, 10), (2, 101, 11)));
        assert!(process_candidate_precedes((1, 100, 10), (1, 101, 11)));
        assert!(!process_candidate_precedes((2, 99, 9), (1, 100, 10)));
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn worker_threads_are_not_agent_process_candidates() {
        assert_eq!(
            linux_tgid_from_status("Name:\tcodex\nTgid:\t42\nPid:\t42\n"),
            Some(42)
        );
        assert!(is_process_group_leader(std::process::id()));
    }
}
