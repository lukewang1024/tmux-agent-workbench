//! Single-session budget arguments; independent from agent-team's team presets.
use crate::{menu_context::Result, paths::Paths};
use serde::Deserialize;
use std::{collections::BTreeMap, path::Path};

#[derive(Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct LaunchConfig {
    #[serde(default)]
    single_budget: BTreeMap<String, Vec<String>>,
}

pub(crate) fn budget_argv(tool: &str) -> Result<Option<Vec<String>>> {
    load_budget(&Paths::discover()?.config_dir.join("launch.toml"), tool)
}

fn load_budget(path: &Path, tool: &str) -> Result<Option<Vec<String>>> {
    let config: LaunchConfig = match std::fs::read_to_string(path) {
        Ok(text) => toml::from_str(&text)?,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => LaunchConfig::default(),
        Err(error) => return Err(error.into()),
    };
    let args = match config.single_budget.get(tool) {
        Some(args) if args.is_empty() => return Ok(None), // Explicit opt-out.
        Some(args) => args.clone(),
        None if tool == "codex" => vec![
            "--model".into(),
            "gpt-5.6-luna".into(),
            "-c".into(),
            "model_reasoning_effort=\"high\"".into(),
        ],
        None => return Ok(None),
    };
    if args.iter().any(|arg| {
        arg.contains('\0')
            || match tool {
                "claude" => {
                    matches!(arg.as_str(), "-p" | "--print" | "--background")
                        || arg.starts_with("--print=")
                }
                "opencode" => matches!(arg.as_str(), "run" | "serve" | "web"),
                _ => matches!(arg.as_str(), "exec" | "app-server" | "mcp-server"),
            }
    }) {
        return Err("Single Budget requires interactive CLI arguments in launch.toml".into());
    }
    Ok(Some(std::iter::once(tool.to_owned()).chain(args).collect()))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn defaults_overrides_and_opt_out_are_distinct() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("launch.toml");
        let codex = load_budget(&path, "codex").unwrap().unwrap();
        assert_eq!(
            codex,
            [
                "codex",
                "--model",
                "gpt-5.6-luna",
                "-c",
                "model_reasoning_effort=\"high\""
            ]
        );
        assert!(load_budget(&path, "claude").unwrap().is_none());
        std::fs::write(
            &path,
            "[single_budget]\ncodex = []\nclaude = ['--model', 'sonnet']\n",
        )
        .unwrap();
        assert!(load_budget(&path, "codex").unwrap().is_none());
        assert_eq!(
            load_budget(&path, "claude").unwrap().unwrap(),
            ["claude", "--model", "sonnet"]
        );
        std::fs::write(&path, "[single_budget]\ncodex = ['exec']\n").unwrap();
        assert!(load_budget(&path, "codex").is_err());
        std::fs::write(&path, "invalid [").unwrap();
        assert!(load_budget(&path, "codex").is_err());
    }
}
