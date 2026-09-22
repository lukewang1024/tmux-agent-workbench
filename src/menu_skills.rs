//! Pinned skill shortcuts use each CLI's discovery paths and invocation syntax.
use crate::model::AgentKind;
use std::path::{Path, PathBuf};

pub(crate) fn shortcuts(kind: AgentKind, cwd: &str) -> Vec<(&'static str, char, String)> {
    let Some(home) = dirs::home_dir() else {
        return vec![];
    };
    let config = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| home.join(".config"));
    let mut roots = Vec::new();
    for dir in Path::new(cwd).ancestors() {
        match kind {
            AgentKind::Codex | AgentKind::Trae => roots.push(dir.join(".agents/skills")),
            AgentKind::Claude => roots.push(dir.join(".claude/skills")),
            AgentKind::Opencode => {
                for subdir in [".opencode/skills", ".claude/skills", ".agents/skills"] {
                    roots.push(dir.join(subdir));
                }
            }
        }
        if dir.join(".git").exists() {
            break;
        }
    }
    match kind {
        AgentKind::Codex | AgentKind::Trae => {
            roots.push(home.join(".agents/skills"));
            let (variable, fallback) = if kind == AgentKind::Codex {
                ("CODEX_HOME", ".codex")
            } else {
                ("TRAE_HOME", ".trae")
            };
            roots.push(
                std::env::var_os(variable)
                    .map(PathBuf::from)
                    .unwrap_or_else(|| home.join(fallback))
                    .join("skills"),
            );
        }
        AgentKind::Claude => roots.push(
            std::env::var_os("CLAUDE_CONFIG_DIR")
                .map(PathBuf::from)
                .unwrap_or_else(|| home.join(".claude"))
                .join("skills"),
        ),
        AgentKind::Opencode => {
            roots.extend([
                config.join("opencode/skills"),
                home.join(".claude/skills"),
                home.join(".agents/skills"),
            ]);
        }
    }
    discover(kind, &roots)
}

fn discover(kind: AgentKind, roots: &[PathBuf]) -> Vec<(&'static str, char, String)> {
    [("grill-me", 'q'), ("handoff", 'h')]
        .into_iter()
        .filter_map(|(name, key)| {
            roots
                .iter()
                .find(|root| root.join(name).join("SKILL.md").is_file())?;
            let input = match kind {
                AgentKind::Codex | AgentKind::Trae => format!("${name}"),
                AgentKind::Claude => format!("/{name}"),
                AgentKind::Opencode => format!("Use the {name} skill."),
            };
            Some((name, key, input))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn only_installed_skills_are_pinned_with_provider_syntax() {
        let temp = tempfile::tempdir().unwrap();
        let roots = vec![temp.path().to_path_buf()];
        assert!(discover(AgentKind::Codex, &roots).is_empty());
        std::fs::create_dir(temp.path().join("grill-me")).unwrap();
        std::fs::write(
            temp.path().join("grill-me/SKILL.md"),
            "---\nname: grill-me\n---\n",
        )
        .unwrap();
        for (kind, input) in [
            (AgentKind::Codex, "$grill-me"),
            (AgentKind::Trae, "$grill-me"),
            (AgentKind::Claude, "/grill-me"),
            (AgentKind::Opencode, "Use the grill-me skill."),
        ] {
            assert_eq!(
                discover(kind, &roots),
                vec![("grill-me", 'q', input.into())]
            );
        }
    }
}
