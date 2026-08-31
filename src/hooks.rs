use std::fs::{self, OpenOptions};
use std::io::{self, Read, Write};
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use toml_edit::{ArrayOfTables, DocumentMut, Item, Table, Value as TomlValue};

use crate::ipc::{Request, call};
use crate::model::{AgentEventReport, AgentEventType, AgentKind};
use crate::paths::Paths;
use crate::server::ServerIdentity;

pub const HOOK_ID: &str = "tmux-agent-workbench-v1";
pub const SPOOL_TTL_MS: u64 = 30_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HookTarget {
    Claude,
    Codex,
    Traex,
    Opencode,
}

impl HookTarget {
    pub fn all() -> [Self; 4] {
        [Self::Claude, Self::Codex, Self::Traex, Self::Opencode]
    }
    pub fn parse(value: &str) -> Result<Vec<Self>, String> {
        match value.to_ascii_lowercase().as_str() {
            "claude" => Ok(vec![Self::Claude]),
            "codex" => Ok(vec![Self::Codex]),
            "traex" | "trae" => Ok(vec![Self::Traex]),
            "opencode" => Ok(vec![Self::Opencode]),
            "all" => Ok(Self::all().to_vec()),
            _ => Err(format!("unknown hook target: {value}")),
        }
    }
    pub fn label(self) -> &'static str {
        match self {
            Self::Claude => "claude",
            Self::Codex => "codex",
            Self::Traex => "traex",
            Self::Opencode => "opencode",
        }
    }
}

pub fn ingest(
    paths: &Paths,
    server: &ServerIdentity,
    agent: AgentKind,
    event_name: &str,
    input: &[u8],
) -> Result<(), Box<dyn std::error::Error>> {
    let pane_id = std::env::var("TMUX_PANE").map_err(|_| "TMUX_PANE is unavailable")?;
    let (tmux_session_id, pane_pid) = pane_identity(server, &pane_id)?;
    let payload: Value = if input.iter().all(u8::is_ascii_whitespace) {
        Value::Null
    } else {
        serde_json::from_slice(input)?
    };
    let report = report_from_payload(
        agent,
        event_name,
        pane_id,
        tmux_session_id,
        pane_pid,
        &payload,
    )?;
    let request = Request::new("agent.event.report", serde_json::to_value(&report)?);
    if call(
        &paths.socket_for_server(&server.key),
        &request,
        Duration::from_millis(150),
    )
    .is_err()
    {
        spool(paths, server, &report)?;
    }
    Ok(())
}

fn report_from_payload(
    agent: AgentKind,
    event_name: &str,
    pane_id: String,
    tmux_session_id: String,
    _pane_pid: u32,
    payload: &Value,
) -> Result<AgentEventReport, String> {
    let mut event = map_event(event_name)?;
    if event == AgentEventType::Activity && payload_indicates_failure(payload, 0) {
        event = AgentEventType::Error;
    }
    let session_id = string_field(
        payload,
        &["session_id", "sessionId", "thread_id", "conversation_id"],
    )
    .unwrap_or_else(|| format!("pane-{pane_id}"));
    let event_id =
        string_field(payload, &["event_id", "eventId", "hook_event_id"]).unwrap_or_else(|| {
            let mut hash = Sha256::new();
            hash.update(pane_id.as_bytes());
            hash.update(event_name.as_bytes());
            hash.update(session_id.as_bytes());
            hash.update(serde_json::to_vec(payload).unwrap_or_default());
            format!("hook-{:x}", hash.finalize())[..37].to_owned()
        });
    let agent_pid = number_field(payload, &["agent_pid", "agentPid", "pid"]).unwrap_or(0) as u32;
    let session_label = string_field(payload, &["thread_name", "threadName", "session_name"])
        .or_else(|| {
            (agent == AgentKind::Codex)
                .then(|| codex_thread_name(&session_id))
                .flatten()
        });
    let reason_category = match event {
        AgentEventType::Permission => string_field(payload, &["reason_category", "reasonCategory"])
            .or_else(|| Some("approval".into())),
        AgentEventType::Error => Some("task_error".into()),
        AgentEventType::SessionStart
            if string_field(payload, &["source", "reason"])
                .is_some_and(|value| value.eq_ignore_ascii_case("compact")) =>
        {
            Some("compact".into())
        }
        _ => None,
    };
    Ok(AgentEventReport {
        version: 1,
        event_id,
        agent,
        pane_id,
        tmux_session_id,
        session_id,
        session_label,
        agent_pid,
        event,
        occurred_at_unix_ms: number_field(
            payload,
            &["occurred_at_unix_ms", "timestamp_ms", "timestampMs"],
        )
        .unwrap_or_else(now_ms),
        reason_category,
    })
}

fn codex_thread_name(session_id: &str) -> Option<String> {
    let path = codex_home().ok()?.join("session_index.jsonl");
    let content = fs::read_to_string(path).ok()?;
    content.lines().rev().find_map(|line| {
        let value: Value = serde_json::from_str(line).ok()?;
        (value.get("id")?.as_str()? == session_id)
            .then(|| value.get("thread_name")?.as_str().map(str::to_owned))
            .flatten()
    })
}

fn map_event(name: &str) -> Result<AgentEventType, String> {
    let normalized = name.to_ascii_lowercase().replace(['_', '.', '-'], "");
    match normalized.as_str() {
        "sessionstart" => Ok(AgentEventType::SessionStart),
        "userpromptsubmit" | "busy" | "working" | "sessionbusy" => Ok(AgentEventType::Working),
        "posttooluse" | "activity" | "toolafter" => Ok(AgentEventType::Activity),
        "posttoolusefailure" | "stopfailure" | "sessionerror" | "error" => {
            Ok(AgentEventType::Error)
        }
        "permissionrequest" | "permissionasked" | "permission" => Ok(AgentEventType::Permission),
        "stop" | "idle" | "done" | "sessionidle" => Ok(AgentEventType::Stop),
        _ => Err(format!("unsupported hook event: {name}")),
    }
}

fn payload_indicates_failure(value: &Value, depth: u8) -> bool {
    if depth > 5 {
        return false;
    }
    match value {
        Value::Object(map) => {
            if map.iter().any(|(key, value)| {
                (matches!(key.as_str(), "is_error" | "isError" | "failed")
                    && value.as_bool() == Some(true))
                    || (key == "success" && value.as_bool() == Some(false))
                    || (matches!(key.as_str(), "status" | "outcome" | "result")
                        && value.as_str().is_some_and(|text| {
                            matches!(
                                text.to_ascii_lowercase().as_str(),
                                "error" | "failed" | "failure"
                            )
                        }))
            }) {
                return true;
            }
            map.values()
                .any(|item| payload_indicates_failure(item, depth + 1))
        }
        Value::Array(items) => items
            .iter()
            .any(|item| payload_indicates_failure(item, depth + 1)),
        _ => false,
    }
}

fn string_field(value: &Value, names: &[&str]) -> Option<String> {
    names
        .iter()
        .find_map(|name| value.get(*name)?.as_str().map(str::to_owned))
}
fn number_field(value: &Value, names: &[&str]) -> Option<u64> {
    names.iter().find_map(|name| value.get(*name)?.as_u64())
}

fn pane_identity(server: &ServerIdentity, pane: &str) -> Result<(String, u32), String> {
    let output = Command::new("tmux")
        .arg("-S")
        .arg(&server.socket_path)
        .args([
            "display-message",
            "-p",
            "-t",
            pane,
            "#{session_id}\u{1f}#{pane_pid}",
        ])
        .output()
        .map_err(|e| e.to_string())?;
    if !output.status.success() {
        return Err("tmux pane is unavailable".into());
    }
    let text = String::from_utf8_lossy(&output.stdout);
    let (session, pid) = text
        .trim()
        .split_once('\u{1f}')
        .ok_or("invalid tmux pane identity")?;
    Ok((
        session.to_owned(),
        pid.parse().map_err(|_| "invalid tmux pane pid")?,
    ))
}

fn spool(paths: &Paths, server: &ServerIdentity, report: &AgentEventReport) -> io::Result<()> {
    let dir = paths.spool_for_server(&server.key);
    fs::create_dir_all(&dir)?;
    fs::set_permissions(&dir, fs::Permissions::from_mode(0o700))?;
    let path = dir.join(format!(
        "{}-{}.json",
        report.occurred_at_unix_ms, report.event_id
    ));
    let mut file = OpenOptions::new()
        .create_new(true)
        .write(true)
        .mode(0o600)
        .open(path)?;
    serde_json::to_writer(&mut file, report).map_err(io::Error::other)?;
    file.write_all(b"\n")
}

pub fn drain_spool(paths: &Paths, server_key: &str, now: u64) -> Vec<AgentEventReport> {
    let dir = paths.spool_for_server(server_key);
    let Ok(entries) = fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut reports = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        let parsed = fs::read(&path)
            .ok()
            .and_then(|bytes| serde_json::from_slice::<AgentEventReport>(&bytes).ok());
        if let Some(report) =
            parsed.filter(|r| now.saturating_sub(r.occurred_at_unix_ms) <= SPOOL_TTL_MS)
        {
            reports.push(report);
        }
        let _ = fs::remove_file(path);
    }
    reports.sort_by_key(|r| r.occurred_at_unix_ms);
    reports
}

pub fn manage(action: &str, target: &str) -> Result<(), Box<dyn std::error::Error>> {
    let targets = HookTarget::parse(target)?;
    let mut check_failures = Vec::new();
    for target in targets {
        match action {
            "install" => install_one(target)?,
            "remove" => remove_one(target)?,
            "check" => {
                let report = check_one(target)?;
                println!("{}: {}", target.label(), report);
                if report != "ok" {
                    check_failures.push(format!("{} hooks: {report}", target.label()));
                }
            }
            _ => return Err(format!("unknown hooks action: {action}").into()),
        }
    }
    if check_failures.is_empty() {
        Ok(())
    } else {
        Err(check_failures.join("; ").into())
    }
}

pub fn check_all() -> Vec<(HookTarget, String)> {
    HookTarget::all()
        .into_iter()
        .map(|target| {
            let value = check_one(target).unwrap_or_else(|error| format!("invalid: {error}"));
            (target, value)
        })
        .collect()
}

fn install_one(target: HookTarget) -> Result<(), Box<dyn std::error::Error>> {
    match target {
        HookTarget::Claude => merge_json_hooks(
            &home()?.join(".claude/settings.json"),
            "claude",
            false,
            true,
        ),
        HookTarget::Traex => {
            merge_json_hooks(&home()?.join(".trae/cli/hooks.json"), "trae", false, true)
        }
        HookTarget::Codex => merge_codex(&codex_home()?.join("config.toml"), false),
        HookTarget::Opencode => {
            install_opencode(&config_home()?.join("opencode/plugins/tmux-agent-workbench.js"))
        }
    }
}

fn remove_one(target: HookTarget) -> Result<(), Box<dyn std::error::Error>> {
    match target {
        HookTarget::Claude => merge_json_hooks(
            &home()?.join(".claude/settings.json"),
            "claude",
            true,
            false,
        ),
        HookTarget::Traex => {
            merge_json_hooks(&home()?.join(".trae/cli/hooks.json"), "trae", true, false)
        }
        HookTarget::Codex => merge_codex(&codex_home()?.join("config.toml"), true),
        HookTarget::Opencode => {
            let path = config_home()?.join("opencode/plugins/tmux-agent-workbench.js");
            if path.exists() {
                fs::remove_file(path)?;
            }
            Ok(())
        }
    }
}

fn check_one(target: HookTarget) -> Result<String, Box<dyn std::error::Error>> {
    let (path, needle) = match target {
        HookTarget::Claude => (
            home()?.join(".claude/settings.json"),
            format!("hook ingest claude"),
        ),
        HookTarget::Traex => (
            home()?.join(".trae/cli/hooks.json"),
            format!("hook ingest trae"),
        ),
        HookTarget::Codex => (
            codex_home()?.join("config.toml"),
            "hook ingest codex".into(),
        ),
        HookTarget::Opencode => (
            config_home()?.join("opencode/plugins/tmux-agent-workbench.js"),
            HOOK_ID.into(),
        ),
    };
    if !path.exists() {
        return Ok("missing (run `tmux-agent-workbench hooks install all`)".into());
    }
    let content = fs::read_to_string(&path)?;
    if matches!(target, HookTarget::Claude | HookTarget::Traex) {
        let root: Value = serde_json::from_str(&content)?;
        let agent = if target == HookTarget::Claude {
            "claude"
        } else {
            "trae"
        };
        let Some(hooks) = root.get("hooks").and_then(Value::as_object) else {
            return Ok("missing hooks object".into());
        };
        let exact = json_events(agent).iter().all(|(event, _)| {
            hooks
                .get(*event)
                .and_then(Value::as_array)
                .is_some_and(|items| {
                    items
                        .iter()
                        .filter(|item| {
                            item.to_string().contains(&format!(
                                "tmux-agent-workbench hook ingest {agent} {event}"
                            ))
                        })
                        .count()
                        == 1
                })
        });
        if !exact {
            return Ok("missing or duplicate Workbench entries".into());
        }
    }
    if target == HookTarget::Codex {
        let doc = content.parse::<DocumentMut>()?;
        if let Some(issue) = codex_hooks_issue(&doc, &path) {
            return Ok(issue.into());
        }
    }
    let count = content.matches(&needle).count();
    Ok(match count {
        0 => "missing (run `tmux-agent-workbench hooks install all`)".into(),
        1..=8 if target == HookTarget::Opencode && count != 1 => {
            "duplicate Workbench entries".into()
        }
        1..=8 => "ok".into(),
        _ => "duplicate Workbench entries".into(),
    })
}

const EVENTS: [(&str, &str); 5] = [
    ("SessionStart", "SessionStart"),
    ("UserPromptSubmit", "UserPromptSubmit"),
    ("PermissionRequest", "PermissionRequest"),
    ("PostToolUse", "PostToolUse"),
    ("Stop", "Stop"),
];
const CLAUDE_FAILURE_EVENTS: [(&str, &str); 1] = [("PostToolUseFailure", "PostToolUseFailure")];

fn json_events(agent: &str) -> Vec<(&'static str, &'static str)> {
    let mut events = EVENTS.to_vec();
    if agent == "claude" {
        events.extend(CLAUDE_FAILURE_EVENTS);
    }
    events
}

fn merge_json_hooks(
    path: &Path,
    agent: &str,
    remove: bool,
    install: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut root = if path.exists() {
        serde_json::from_slice::<Value>(&fs::read(path)?)?
    } else {
        json!({})
    };
    let object = root
        .as_object_mut()
        .ok_or("hook config root must be an object")?;
    let hooks = object
        .entry("hooks")
        .or_insert_with(|| json!({}))
        .as_object_mut()
        .ok_or("hooks must be an object")?;
    for (key, event) in json_events(agent) {
        let list = hooks
            .entry(key)
            .or_insert_with(|| json!([]))
            .as_array_mut()
            .ok_or("hook event must be an array")?;
        list.retain(|entry| {
            !entry
                .to_string()
                .contains("tmux-agent-workbench hook ingest")
        });
        if install && !remove {
            list.push(json!({"matcher":"*","hooks":[{"type":"command","command":format!("tmux-agent-workbench hook ingest {agent} {event}"),"timeout":3}],"_workbench":HOOK_ID}));
        }
    }
    atomic_json(path, &root)
}

fn merge_codex(path: &Path, remove: bool) -> Result<(), Box<dyn std::error::Error>> {
    let text = if path.exists() {
        fs::read_to_string(path)?
    } else {
        String::new()
    };
    let mut doc = text.parse::<DocumentMut>()?;
    let hooks = doc
        .entry("hooks")
        .or_insert(Item::Table(Table::new()))
        .as_table_mut()
        .ok_or("hooks must be a table")?;
    // Remove an early pre-release representation if it is present.
    hooks.remove(HOOK_ID);
    for (key, event) in EVENTS {
        let item = hooks
            .entry(key)
            .or_insert(Item::ArrayOfTables(ArrayOfTables::new()));
        let entries = item
            .as_array_of_tables_mut()
            .ok_or("Codex hook event must be an array of tables")?;
        entries.retain(|entry| !codex_entry_is_workbench(entry));
        if !remove {
            let mut outer = Table::new();
            if key == "SessionStart" {
                outer["matcher"] = Item::Value(TomlValue::from("startup|resume|clear"));
            }
            let mut commands = ArrayOfTables::new();
            let mut command = Table::new();
            command["type"] = Item::Value(TomlValue::from("command"));
            command["command"] = Item::Value(TomlValue::from(format!(
                "tmux-agent-workbench hook ingest codex {event}"
            )));
            command["timeout"] = Item::Value(TomlValue::from(3));
            commands.push(command);
            outer["hooks"] = Item::ArrayOfTables(commands);
            entries.push(outer);
        }
    }
    atomic_write(path, doc.to_string().as_bytes())
}

fn codex_hooks_issue(doc: &DocumentMut, path: &Path) -> Option<&'static str> {
    let Some(hooks) = doc.get("hooks").and_then(Item::as_table) else {
        return Some("installed but not trusted");
    };
    let Some(state) = hooks.get("state").and_then(Item::as_table) else {
        return Some("installed but not trusted");
    };
    for (event, _) in EVENTS {
        let Some(entries) = hooks.get(event).and_then(Item::as_array_of_tables) else {
            return Some("installed but not trusted");
        };
        let Some(index) = entries.iter().position(codex_entry_is_workbench) else {
            return Some("installed but not trusted");
        };
        let snake = event
            .chars()
            .enumerate()
            .fold(String::new(), |mut output, (index, ch)| {
                if ch.is_ascii_uppercase() && index > 0 {
                    output.push('_');
                }
                output.push(ch.to_ascii_lowercase());
                output
            });
        let key = format!("{}:{snake}:{index}:0", path.display());
        let Some(entry) = state.get(&key).and_then(Item::as_table) else {
            return Some("installed but not trusted");
        };
        if entry.get("trusted_hash").and_then(Item::as_str).is_none() {
            return Some("installed but not trusted");
        }
        if entry.get("enabled").and_then(Item::as_bool) == Some(false) {
            return Some("installed but disabled");
        }
    }
    None
}

fn codex_entry_is_workbench(entry: &Table) -> bool {
    entry
        .get("hooks")
        .and_then(Item::as_array_of_tables)
        .is_some_and(|commands| {
            commands.iter().any(|command| {
                command
                    .get("command")
                    .and_then(Item::as_str)
                    .is_some_and(|value| value.contains("tmux-agent-workbench hook ingest"))
            })
        })
}

fn install_opencode(path: &Path) -> Result<(), Box<dyn std::error::Error>> {
    let content = format!(
        r#"// {HOOK_ID}
const {{ spawn }} = require("node:child_process");
module.exports = async () => ({{ event }}) => {{
  const names = {{"session.created":"SessionStart","session.busy":"busy","session.idle":"idle","session.error":"session-error","permission.asked":"permission-asked","tool.after":"activity"}};
  const mapped = names[event?.type]; if (!mapped) return;
  const child = spawn("tmux-agent-workbench", ["hook","ingest","opencode",mapped], {{stdio:["pipe","ignore","ignore"], env:process.env}});
  child.stdin.end(JSON.stringify(event));
}};
"#
    );
    atomic_write(path, content.as_bytes())
}

fn atomic_json(path: &Path, value: &Value) -> Result<(), Box<dyn std::error::Error>> {
    let mut bytes = serde_json::to_vec_pretty(value)?;
    bytes.push(b'\n');
    atomic_write(path, &bytes)
}
fn atomic_write(path: &Path, bytes: &[u8]) -> Result<(), Box<dyn std::error::Error>> {
    let parent = path.parent().ok_or("path has no parent")?;
    fs::create_dir_all(parent)?;
    let temp = parent.join(format!(
        ".tmux-agent-workbench-{}.tmp",
        uuid::Uuid::new_v4()
    ));
    let mut file = OpenOptions::new()
        .create_new(true)
        .write(true)
        .mode(0o600)
        .open(&temp)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    fs::rename(temp, path)?;
    Ok(())
}
fn home() -> Result<PathBuf, String> {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .ok_or("HOME unavailable".into())
}
fn config_home() -> Result<PathBuf, String> {
    Ok(std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .unwrap_or(home()?.join(".config")))
}
fn codex_home() -> Result<PathBuf, String> {
    Ok(std::env::var_os("CODEX_HOME")
        .map(PathBuf::from)
        .unwrap_or(home()?.join(".codex")))
}
fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

pub fn read_stdin() -> io::Result<Vec<u8>> {
    let mut bytes = Vec::new();
    io::stdin().take(64 * 1024).read_to_end(&mut bytes)?;
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn json_install_is_idempotent_preserves_foreign_hooks_and_removes_exactly_ours() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("settings.json");
        atomic_write(&path, br#"{"theme":"dark","hooks":{"Stop":[{"hooks":[{"type":"command","command":"peon-ping"}]}]}}"#).unwrap();
        merge_json_hooks(&path, "claude", false, true).unwrap();
        merge_json_hooks(&path, "claude", false, true).unwrap();
        let installed = fs::read_to_string(&path).unwrap();
        assert_eq!(installed.matches("hook ingest claude Stop").count(), 1);
        assert!(installed.contains("peon-ping"));
        assert!(installed.contains("\"theme\": \"dark\""));
        merge_json_hooks(&path, "claude", true, false).unwrap();
        let removed = fs::read_to_string(&path).unwrap();
        assert!(!removed.contains("tmux-agent-workbench hook ingest"));
        assert!(removed.contains("peon-ping"));
    }

    #[test]
    fn codex_merge_preserves_unknown_config_and_is_idempotent() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("config.toml");
        atomic_write(&path, b"model = \"gpt-test\"\n[features]\nfoo = true\n").unwrap();
        merge_codex(&path, false).unwrap();
        merge_codex(&path, false).unwrap();
        let installed = fs::read_to_string(&path).unwrap();
        assert_eq!(installed.matches("hook ingest codex Stop").count(), 1);
        assert_eq!(installed.matches("hook ingest codex").count(), 5);
        assert_eq!(
            installed
                .matches("matcher = \"startup|resume|clear\"")
                .count(),
            1
        );
        assert!(installed.contains("model = \"gpt-test\""));
        assert!(installed.contains("foo = true"));
        merge_codex(&path, true).unwrap();
        let removed = fs::read_to_string(&path).unwrap();
        assert!(!removed.contains(HOOK_ID));
        assert!(removed.contains("model = \"gpt-test\""));
    }

    #[test]
    fn codex_checker_distinguishes_disabled_from_untrusted_hooks() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("config.toml");
        merge_codex(&path, false).unwrap();
        let mut doc = fs::read_to_string(&path)
            .unwrap()
            .parse::<DocumentMut>()
            .unwrap();
        let hooks = doc["hooks"].as_table_mut().unwrap();
        let mut state = Table::new();
        for (event, _) in EVENTS {
            let snake = event
                .chars()
                .enumerate()
                .fold(String::new(), |mut output, (index, ch)| {
                    if ch.is_ascii_uppercase() && index > 0 {
                        output.push('_');
                    }
                    output.push(ch.to_ascii_lowercase());
                    output
                });
            let key = format!("{}:{snake}:0:0", path.display());
            let mut entry = Table::new();
            entry["trusted_hash"] = Item::Value(TomlValue::from("sha256:test"));
            state[&key] = Item::Table(entry);
        }
        hooks["state"] = Item::Table(state);
        assert_eq!(codex_hooks_issue(&doc, &path), None);

        let key = format!("{}:post_tool_use:0:0", path.display());
        doc["hooks"]["state"][&key]["enabled"] = Item::Value(TomlValue::from(false));
        assert_eq!(
            codex_hooks_issue(&doc, &path),
            Some("installed but disabled")
        );

        doc["hooks"]["state"][&key]
            .as_table_mut()
            .unwrap()
            .remove("trusted_hash");
        assert_eq!(
            codex_hooks_issue(&doc, &path),
            Some("installed but not trusted")
        );
    }

    #[test]
    fn malformed_config_fails_without_replacing_it() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("settings.json");
        atomic_write(&path, b"{broken").unwrap();
        assert!(merge_json_hooks(&path, "claude", false, true).is_err());
        assert_eq!(fs::read(&path).unwrap(), b"{broken");
    }

    #[test]
    fn event_mapping_covers_all_native_lifecycle_sources() {
        assert_eq!(
            map_event("UserPromptSubmit").unwrap(),
            AgentEventType::Working
        );
        assert_eq!(
            map_event("permission.asked").unwrap(),
            AgentEventType::Permission
        );
        assert_eq!(map_event("PostToolUse").unwrap(), AgentEventType::Activity);
        assert_eq!(map_event("session.idle").unwrap(), AgentEventType::Stop);
        assert_eq!(map_event("session.error").unwrap(), AgentEventType::Error);
        assert!(payload_indicates_failure(
            &serde_json::json!({"tool_response":{"success":false}}),
            0
        ));
        assert!(!payload_indicates_failure(
            &serde_json::json!({"tool_response":{"success":true}}),
            0
        ));
    }
}
