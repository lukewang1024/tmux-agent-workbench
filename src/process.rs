use std::collections::{HashMap, HashSet};
use std::ffi::OsStr;
use std::path::Path;

use sysinfo::{Pid, Process, ProcessRefreshKind, ProcessesToUpdate, System, UpdateKind};

use crate::model::{AgentKind, ProcessFingerprint};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentProcess {
    pub kind: AgentKind,
    pub fingerprint: ProcessFingerprint,
    pub resume_session_id: Option<String>,
}

/// Noninteractive jobs may still contribute status, but cannot accept TUI commands.
pub(crate) fn accepts_terminal_input(agent: &AgentProcess) -> bool {
    let pid = Pid::from_u32(agent.fingerprint.pid);
    let mut system = System::new();
    system.refresh_processes_specifics(
        ProcessesToUpdate::Some(&[pid]),
        true,
        ProcessRefreshKind::nothing()
            .without_tasks()
            .with_cmd(UpdateKind::Always),
    );
    system
        .process(pid)
        .is_some_and(|process| interactive_command(agent.kind, process.cmd()))
}

fn interactive_command(kind: AgentKind, args: &[std::ffi::OsString]) -> bool {
    !args.iter().skip(1).any(|arg| {
        let arg = arg.to_string_lossy();
        match kind {
            AgentKind::Codex | AgentKind::Trae => {
                matches!(arg.as_ref(), "exec" | "app-server" | "mcp-server")
            }
            AgentKind::Claude => {
                matches!(arg.as_ref(), "-p" | "--print") || arg.starts_with("--print=")
            }
            AgentKind::Opencode => matches!(arg.as_ref(), "run" | "serve" | "web"),
        }
    })
}

fn foreground_ids(rows: &str) -> HashSet<u32> {
    rows.lines()
        .filter_map(|row| {
            let fields: Vec<_> = row.split_whitespace().collect();
            if fields.len() != 4 || fields[1] != fields[2] || fields[3].contains(['T', 'Z']) {
                return None;
            }
            let group: u32 = fields[1].parse().ok()?;
            if group == 0 {
                return None;
            }
            fields[0].parse().ok()
        })
        .collect()
}

impl ProcessTree {
    /// Reuse the detector's existing process inventory. One batched foreground
    /// query replaces per-menu discovery; status formats read pane-local flags.
    pub(crate) fn interactive_foreground_roots(
        &self,
        agents: &HashMap<u32, AgentProcess>,
    ) -> HashSet<u32> {
        if agents.is_empty() {
            return HashSet::new();
        }
        let pids = agents
            .values()
            .map(|agent| agent.fingerprint.pid.to_string())
            .collect::<Vec<_>>()
            .join(",");
        let rows = std::process::Command::new("ps")
            .args(["-o", "pid=,pgid=,tpgid=,stat=", "-p", &pids])
            .output();
        let foreground = rows
            .ok()
            .map(|output| foreground_ids(&String::from_utf8_lossy(&output.stdout)))
            .unwrap_or_default();
        agents
            .iter()
            .filter_map(|(root, agent)| {
                (foreground.contains(&agent.fingerprint.pid)
                    && self
                        .system
                        .process(Pid::from_u32(agent.fingerprint.pid))
                        .is_some_and(|process| interactive_command(agent.kind, process.cmd())))
                .then_some(*root)
            })
            .collect()
    }
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
        // Detection needs identity and ancestry, not resource counters or every
        // kernel thread. Keep command lines for Node/Bun/Deno agent wrappers.
        self.system.refresh_processes_specifics(
            ProcessesToUpdate::All,
            true,
            ProcessRefreshKind::nothing()
                .without_tasks()
                .with_exe(UpdateKind::OnlyIfNotSet)
                .with_cmd(UpdateKind::OnlyIfNotSet),
        );
        // A pane shell can exec its agent without changing PID/start time.
        // Refresh command identity only inside tracked pane trees; unrelated
        // system processes retain the cheaper cached identity path.
        let root_ids = roots
            .iter()
            .copied()
            .map(Pid::from_u32)
            .collect::<HashSet<_>>();
        let related = self
            .system
            .processes()
            .keys()
            .copied()
            .filter(|pid| nearest_root(&self.system, *pid, &root_ids).is_some())
            .collect::<Vec<_>>();
        if !related.is_empty() {
            self.system.refresh_processes_specifics(
                ProcessesToUpdate::Some(&related),
                true,
                ProcessRefreshKind::nothing()
                    .without_tasks()
                    .with_exe(UpdateKind::Always)
                    .with_cmd(UpdateKind::Always),
            );
        }
        find_agents(&self.system, roots, aliases)
    }
}

/// Recover a hook's owning CLI without relying on sandbox-filtered tmux env.
pub fn current_agent_ancestor(kind: AgentKind) -> Option<u32> {
    let aliases = HashMap::from([
        ("codex".into(), AgentKind::Codex),
        ("codex-cli".into(), AgentKind::Codex),
        ("claude".into(), AgentKind::Claude),
        ("trae".into(), AgentKind::Trae),
        ("traex".into(), AgentKind::Trae),
        ("opencode".into(), AgentKind::Opencode),
    ]);
    let mut system = System::new();
    let mut pid = Pid::from_u32(std::process::id());
    for _ in 0..64 {
        system.refresh_processes_specifics(
            ProcessesToUpdate::Some(&[pid]),
            true,
            ProcessRefreshKind::nothing()
                .without_tasks()
                .with_exe(UpdateKind::OnlyIfNotSet)
                .with_cmd(UpdateKind::OnlyIfNotSet),
        );
        let process = system.process(pid)?;
        if identify(process, &aliases) == Some(kind) {
            // A TUI can proxy to one app-server shared by several panes. That
            // backend PID is not evidence of the owning terminal's identity.
            if is_shared_backend(kind, process.cmd()) {
                return None;
            }
            return Some(pid.as_u32());
        }
        let parent = process.parent()?;
        if parent == pid {
            return None;
        }
        pid = parent;
    }
    None
}

fn is_shared_backend(kind: AgentKind, args: &[std::ffi::OsString]) -> bool {
    kind == AgentKind::Codex && args.iter().any(|arg| arg == "app-server")
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
                    resume_session_id: (kind == AgentKind::Codex)
                        .then(|| explicit_codex_resume(process.cmd()))
                        .flatten(),
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

fn explicit_codex_resume(args: &[std::ffi::OsString]) -> Option<String> {
    let pair = args.get(args.len().checked_sub(2)?..)?;
    if pair[0] != "resume" {
        return None;
    }
    let id = pair[1].to_str()?;
    uuid::Uuid::parse_str(id).ok()?;
    Some(id.to_ascii_lowercase())
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
    #[test]
    fn foreground_filter_excludes_background_suspended_and_defunct_jobs() {
        let rows = "10 10 10 S+\n11 11 10 S\n12 12 12 T\n13 13 13 Z\n14 0 0 S\ninvalid";
        assert_eq!(
            super::foreground_ids(rows),
            std::collections::HashSet::from([10])
        );
    }
    #[test]
    fn command_filter_excludes_noninteractive_agents() {
        for (kind, args) in [
            (crate::model::AgentKind::Codex, vec!["codex", "exec"]),
            (
                crate::model::AgentKind::Claude,
                vec!["claude", "--print=hi"],
            ),
            (crate::model::AgentKind::Opencode, vec!["opencode", "serve"]),
        ] {
            let args = args
                .into_iter()
                .map(std::ffi::OsString::from)
                .collect::<Vec<_>>();
            assert!(!super::interactive_command(kind, &args));
        }
        assert!(super::interactive_command(
            crate::model::AgentKind::Codex,
            &["codex".into()]
        ));
    }
    use super::*;

    #[test]
    fn pane_exec_refreshes_identity_without_a_new_pid() {
        use std::io::Write;
        use std::process::{Command, Stdio};
        use std::time::{Duration, Instant};
        let mut child = Command::new("/bin/sh")
            .args(["-c", "read ready; exec sleep 30"])
            .stdin(Stdio::piped())
            .spawn()
            .unwrap();
        let aliases = HashMap::from([("sleep".into(), AgentKind::Claude)]);
        let mut tree = ProcessTree::default();
        let before = tree.agents_for_roots(&[child.id()], &aliases);
        child.stdin.take().unwrap().write_all(b"go\n").unwrap();
        let deadline = Instant::now() + Duration::from_secs(2);
        let found = loop {
            if tree
                .agents_for_roots(&[child.id()], &aliases)
                .contains_key(&child.id())
            {
                break true;
            }
            if Instant::now() >= deadline {
                break false;
            }
            std::thread::sleep(Duration::from_millis(10));
        };
        let _ = child.kill();
        let _ = child.wait();
        assert!(before.is_empty());
        assert!(found, "cached shell identity survived exec into an agent");
    }

    #[test]
    fn theme_resume_requires_an_explicit_uuid_target() {
        let id = "01a153e9-75cb-7151-99f4-2ab4d9ffeed7";
        for (args, expected) in [
            (vec!["codex", "--yolo", "resume", id], Some(id)),
            (vec!["codex", "resume", "--last"], None),
            (vec!["codex", "resume", "not-a-thread"], None),
            (vec!["codex", "--yolo"], None),
        ] {
            let args: Vec<_> = args.into_iter().map(std::ffi::OsString::from).collect();
            assert_eq!(explicit_codex_resume(&args).as_deref(), expected);
        }
    }

    #[test]
    fn shared_codex_backend_is_not_a_terminal_owner() {
        let shared = [
            "codex",
            "-c",
            "features.code_mode_host=true",
            "app-server",
            "--listen",
            "unix://",
        ]
        .map(std::ffi::OsString::from);
        assert!(is_shared_backend(AgentKind::Codex, &shared));
        assert!(!is_shared_backend(
            AgentKind::Codex,
            &["codex".into(), "--yolo".into()]
        ));
    }

    #[test]
    fn minimal_refresh_keeps_identity_and_removes_exited_agents() {
        let mut child = std::process::Command::new("sleep")
            .arg("30")
            .spawn()
            .unwrap();
        let pid = child.id();
        let mut tree = ProcessTree::default();
        let aliases = HashMap::from([("sleep".into(), AgentKind::Codex)]);
        // Process creation and OS command-line visibility are not atomic (seen
        // on Linux ARM release runners). Wait for a usable identity snapshot.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        let found = loop {
            let found = tree.agents_for_roots(&[pid], &aliases);
            let ready = found.contains_key(&pid)
                && tree
                    .system
                    .process(Pid::from_u32(pid))
                    .is_some_and(|process| !process.cmd().is_empty());
            if ready || std::time::Instant::now() >= deadline {
                break found;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        };
        // Always reap the child, including when an assertion below fails.
        child.kill().unwrap();
        child.wait().unwrap();
        let agent = found.get(&pid).expect("native agent identity is retained");
        assert_eq!(agent.fingerprint.pid, pid);
        assert!(agent.fingerprint.executable.ends_with("sleep"));
        assert!(
            !tree
                .system
                .process(Pid::from_u32(pid))
                .unwrap()
                .cmd()
                .is_empty()
        );
        assert!(tree.agents_for_roots(&[pid], &aliases).is_empty());
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn minimal_refresh_does_not_inventory_worker_threads() {
        let (tx, rx) = std::sync::mpsc::channel();
        let (done_tx, done_rx) = std::sync::mpsc::channel();
        let worker = std::thread::spawn(move || {
            tx.send(unsafe { libc::syscall(libc::SYS_gettid) } as u32)
                .unwrap();
            let _ = done_rx.recv();
        });
        let tid = rx.recv().unwrap();
        let mut tree = ProcessTree::default();
        tree.agents_for_roots(&[std::process::id()], &HashMap::new());
        let inventoried = tree.system.process(Pid::from_u32(tid)).is_some();
        done_tx.send(()).unwrap();
        worker.join().unwrap();
        assert!(!inventoried);
    }

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
