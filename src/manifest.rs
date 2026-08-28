use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::Path;

use regex::Regex;
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::model::{AgentKind, BaseState};

#[derive(Debug, Error)]
pub enum ManifestError {
    #[error("invalid manifest TOML: {0}")]
    Parse(#[from] toml::de::Error),
    #[error("invalid rule {rule}: {message}")]
    Rule { rule: String, message: String },
    #[error("manifest version must be 1")]
    Version,
    #[error("invalid or unsupported min_engine_version {0}")]
    EngineVersion(String),
    #[error("cannot read manifest {path}: {source}")]
    Read {
        path: String,
        source: std::io::Error,
    },
    #[error(
        "unknown manifest filename {0}; expected codex.toml, claude.toml, trae.toml, or opencode.toml"
    )]
    UnknownAgent(String),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Manifest {
    pub version: u32,
    pub min_engine_version: Option<String>,
    #[serde(default)]
    pub aliases: Vec<String>,
    #[serde(default)]
    pub rules: Vec<Rule>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Rule {
    pub id: String,
    #[serde(default)]
    pub priority: i32,
    pub state: Option<BaseState>,
    pub reason_category: Option<String>,
    #[serde(default)]
    pub region: Region,
    #[serde(default)]
    pub visible: Visibility,
    #[serde(default)]
    pub skip_state_update: bool,
    pub matcher: Matcher,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Region {
    #[default]
    WholeRecent,
    TopLines,
    BottomLines,
    BottomNonEmptyLines,
    PromptBox,
    AfterLastPrompt,
    AfterLastRule,
    PaneTitle,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Visibility {
    #[default]
    Always,
    Idle,
    Blocker,
    Working,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub enum Matcher {
    Contains { value: String },
    Regex { value: String },
    LineRegex { value: String },
    All { items: Vec<Matcher> },
    Any { items: Vec<Matcher> },
    Not { item: Box<Matcher> },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Classification {
    pub state: BaseState,
    pub reason_category: Option<String>,
    pub rule_id: Option<String>,
    pub evidence: Option<Vec<u8>>,
    pub skip_state_update: bool,
    pub strong_visible_signal: bool,
}

#[derive(Debug, Clone)]
pub struct ManifestSet {
    manifests: HashMap<AgentKind, Manifest>,
}

impl Manifest {
    pub fn parse(input: &str) -> Result<Self, ManifestError> {
        let mut manifest: Self = toml::from_str(input)?;
        manifest
            .rules
            .sort_by_key(|rule| std::cmp::Reverse(rule.priority));
        manifest.validate()?;
        Ok(manifest)
    }

    pub fn validate(&self) -> Result<(), ManifestError> {
        if self.version != 1 {
            return Err(ManifestError::Version);
        }
        if let Some(minimum) = &self.min_engine_version {
            let minimum = semver::Version::parse(minimum)
                .map_err(|_| ManifestError::EngineVersion(minimum.clone()))?;
            let engine = semver::Version::parse(crate::ENGINE_VERSION)
                .map_err(|_| ManifestError::EngineVersion(crate::ENGINE_VERSION.into()))?;
            if minimum > engine {
                return Err(ManifestError::EngineVersion(minimum.to_string()));
            }
        }
        for alias in &self.aliases {
            if alias.is_empty()
                || alias.len() > 128
                || !alias
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
            {
                return Err(ManifestError::Rule {
                    rule: "aliases".into(),
                    message: format!("invalid alias {alias:?}"),
                });
            }
        }
        let mut rule_ids = HashSet::new();
        for rule in &self.rules {
            if !valid_identifier(&rule.id, 128) {
                return Err(ManifestError::Rule {
                    rule: rule.id.clone(),
                    message: "id must be a 1..=128 character identifier".into(),
                });
            }
            if !rule_ids.insert(&rule.id) {
                return Err(ManifestError::Rule {
                    rule: rule.id.clone(),
                    message: "duplicate rule id".into(),
                });
            }
            if rule
                .reason_category
                .as_ref()
                .is_some_and(|reason| !valid_identifier(reason, 64))
            {
                return Err(ManifestError::Rule {
                    rule: rule.id.clone(),
                    message: "reason_category must be a 1..=64 character identifier".into(),
                });
            }
            validate_matcher(&rule.id, &rule.matcher)?;
        }
        Ok(())
    }

    pub fn classify(&self, content: &str, title: &str) -> Classification {
        for rule in &self.rules {
            let region = extract_region(content, title, &rule.region);
            if matcher_matches(&rule.matcher, region) {
                return Classification {
                    state: rule.state.unwrap_or(match rule.visible {
                        Visibility::Idle => BaseState::Idle,
                        Visibility::Blocker => BaseState::Blocked,
                        Visibility::Working => BaseState::Working,
                        Visibility::Always => BaseState::Unknown,
                    }),
                    reason_category: rule.reason_category.clone(),
                    rule_id: Some(rule.id.clone()),
                    evidence: Some(region.as_bytes().to_vec()),
                    skip_state_update: rule.skip_state_update,
                    strong_visible_signal: matches!(
                        (&rule.visible, state_for_rule(rule)),
                        (Visibility::Idle, BaseState::Idle)
                            | (Visibility::Blocker, BaseState::Blocked)
                            | (Visibility::Working, BaseState::Working)
                    ),
                };
            }
        }
        Classification {
            state: BaseState::Unknown,
            reason_category: None,
            rule_id: None,
            evidence: None,
            skip_state_update: false,
            strong_visible_signal: false,
        }
    }
}

fn state_for_rule(rule: &Rule) -> BaseState {
    rule.state.unwrap_or(match rule.visible {
        Visibility::Idle => BaseState::Idle,
        Visibility::Blocker => BaseState::Blocked,
        Visibility::Working => BaseState::Working,
        Visibility::Always => BaseState::Unknown,
    })
}

impl ManifestSet {
    pub fn load(overrides_dir: &Path) -> Result<Self, ManifestError> {
        let mut manifests = HashMap::from([
            (
                AgentKind::Codex,
                Manifest::parse(include_str!("../manifests/codex.toml"))?,
            ),
            (
                AgentKind::Claude,
                Manifest::parse(include_str!("../manifests/claude.toml"))?,
            ),
            (
                AgentKind::Trae,
                Manifest::parse(include_str!("../manifests/trae.toml"))?,
            ),
            (
                AgentKind::Opencode,
                Manifest::parse(include_str!("../manifests/opencode.toml"))?,
            ),
        ]);
        if !overrides_dir.exists() {
            return Ok(Self { manifests });
        }
        let entries = fs::read_dir(overrides_dir).map_err(|source| ManifestError::Read {
            path: overrides_dir.display().to_string(),
            source,
        })?;
        for entry in entries {
            let entry = entry.map_err(|source| ManifestError::Read {
                path: overrides_dir.display().to_string(),
                source,
            })?;
            let path = entry.path();
            if path.extension().and_then(|value| value.to_str()) != Some("toml") {
                continue;
            }
            let stem = path
                .file_stem()
                .and_then(|value| value.to_str())
                .unwrap_or("");
            let kind = kind_for_stem(stem)
                .ok_or_else(|| ManifestError::UnknownAgent(path.display().to_string()))?;
            let input = fs::read_to_string(&path).map_err(|source| ManifestError::Read {
                path: path.display().to_string(),
                source,
            })?;
            manifests.insert(kind, Manifest::parse(&input)?);
        }
        let mut aliases = HashMap::new();
        for (kind, manifest) in &manifests {
            for alias in &manifest.aliases {
                if aliases
                    .insert(alias.to_ascii_lowercase(), *kind)
                    .is_some_and(|previous| previous != *kind)
                {
                    return Err(ManifestError::Rule {
                        rule: "aliases".into(),
                        message: format!("alias {alias:?} belongs to more than one Agent"),
                    });
                }
            }
        }
        Ok(Self { manifests })
    }

    pub fn get(&self, kind: AgentKind) -> &Manifest {
        self.manifests
            .get(&kind)
            .expect("all supported manifests are built in")
    }

    pub fn aliases(&self) -> HashMap<String, AgentKind> {
        self.manifests
            .iter()
            .flat_map(|(kind, manifest)| {
                manifest
                    .aliases
                    .iter()
                    .map(move |alias| (alias.to_ascii_lowercase(), *kind))
            })
            .collect()
    }
}

fn valid_identifier(value: &str, max: usize) -> bool {
    !value.is_empty()
        && value.len() <= max
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
}

fn validate_matcher(rule: &str, matcher: &Matcher) -> Result<(), ManifestError> {
    match matcher {
        Matcher::Contains { value } if value.is_empty() => Err(ManifestError::Rule {
            rule: rule.into(),
            message: "contains value cannot be empty".into(),
        }),
        Matcher::Regex { value } | Matcher::LineRegex { value } => Regex::new(value)
            .map(|_| ())
            .map_err(|error| ManifestError::Rule {
                rule: rule.into(),
                message: error.to_string(),
            }),
        Matcher::All { items } | Matcher::Any { items } if items.is_empty() => {
            Err(ManifestError::Rule {
                rule: rule.into(),
                message: "matcher list cannot be empty".into(),
            })
        }
        Matcher::All { items } | Matcher::Any { items } => items
            .iter()
            .try_for_each(|item| validate_matcher(rule, item)),
        Matcher::Not { item } => validate_matcher(rule, item),
        Matcher::Contains { .. } => Ok(()),
    }
}

fn matcher_matches(matcher: &Matcher, input: &str) -> bool {
    match matcher {
        Matcher::Contains { value } => input.contains(value),
        Matcher::Regex { value } => Regex::new(value).is_ok_and(|regex| regex.is_match(input)),
        Matcher::LineRegex { value } => {
            Regex::new(value).is_ok_and(|regex| input.lines().any(|line| regex.is_match(line)))
        }
        Matcher::All { items } => items.iter().all(|item| matcher_matches(item, input)),
        Matcher::Any { items } => items.iter().any(|item| matcher_matches(item, input)),
        Matcher::Not { item } => !matcher_matches(item, input),
    }
}

fn extract_region<'a>(content: &'a str, title: &'a str, region: &Region) -> &'a str {
    match region {
        Region::PaneTitle => title,
        Region::WholeRecent => content,
        Region::TopLines => lines_range(content, 0, 30),
        Region::BottomLines => tail_lines(content, 30, false),
        Region::BottomNonEmptyLines => tail_lines(content, 30, true),
        Region::PromptBox => prompt_box(content),
        Region::AfterLastPrompt => after_last_prompt(content),
        Region::AfterLastRule => after_last_horizontal_rule(content),
    }
}

fn lines_range(value: &str, start: usize, count: usize) -> &str {
    let mut offset = 0;
    let mut begin = value.len();
    let mut end = value.len();
    for (index, line) in value.split_inclusive('\n').enumerate() {
        if index == start {
            begin = offset;
        }
        offset += line.len();
        if index + 1 == start + count {
            end = offset;
            break;
        }
    }
    if begin == value.len() && start == 0 {
        begin = 0;
    }
    &value[begin..end]
}

fn tail_lines(value: &str, count: usize, non_empty: bool) -> &str {
    let mut starts = Vec::new();
    let mut offset = 0;
    for line in value.split_inclusive('\n') {
        if !non_empty || !line.trim().is_empty() {
            starts.push(offset);
        }
        offset += line.len();
    }
    let start = starts
        .get(starts.len().saturating_sub(count))
        .copied()
        .unwrap_or(0);
    &value[start..]
}

fn prompt_box(value: &str) -> &str {
    let lines: Vec<_> = value.lines().collect();
    let start = lines
        .iter()
        .rposition(|line| {
            let line = line.trim_start();
            line.starts_with('╭') || line.starts_with('┌')
        })
        .unwrap_or_else(|| lines.len().saturating_sub(12));
    nth_line_offset(value, start)
}

fn after_last_prompt(value: &str) -> &str {
    let lines: Vec<_> = value.lines().collect();
    let start = lines
        .iter()
        .rposition(|line| {
            let line = line.trim_start();
            line.starts_with('❯') || line.starts_with('›') || line.starts_with("> ")
        })
        .unwrap_or(0);
    nth_line_offset(value, start)
}

fn after_last_horizontal_rule(value: &str) -> &str {
    let mut offset = 0;
    let mut start = 0;
    for line in value.split_inclusive('\n') {
        offset += line.len();
        let trimmed = line.trim();
        let total = trimmed.chars().count();
        let rule_glyphs = trimmed
            .chars()
            .filter(|character| {
                matches!(
                    character,
                    '-' | '_' | '─' | '━' | '═' | '—' | '┄' | '┅' | '┈' | '┉'
                )
            })
            .count();
        if total >= 3 && rule_glyphs * 5 >= total * 4 {
            start = offset;
        }
    }
    &value[start..]
}

fn nth_line_offset(value: &str, line_index: usize) -> &str {
    let offset = value
        .split_inclusive('\n')
        .take(line_index)
        .map(str::len)
        .sum();
    &value[offset..]
}

fn kind_for_stem(stem: &str) -> Option<AgentKind> {
    match stem {
        "codex" => Some(AgentKind::Codex),
        "claude" => Some(AgentKind::Claude),
        "trae" => Some(AgentKind::Trae),
        "opencode" => Some(AgentKind::Opencode),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_lookaround_from_rust_regex_subset() {
        let input = r#"version = 1
[[rules]]
id = "unsafe"
state = "blocked"
matcher = { regex = { value = "(?=approval)" } }"#;
        assert!(Manifest::parse(input).is_err());
    }

    #[test]
    fn higher_priority_rule_wins() {
        let input = r#"version = 1
[[rules]]
id = "idle"
priority = 10
state = "idle"
matcher = { contains = { value = "ready" } }
[[rules]]
id = "blocked"
priority = 100
state = "blocked"
reason_category = "approval"
matcher = { contains = { value = "approval ready" } }"#;
        let result = Manifest::parse(input)
            .unwrap()
            .classify("approval ready", "");
        assert_eq!(result.state, BaseState::Blocked);
        assert_eq!(result.rule_id.as_deref(), Some("blocked"));
    }

    #[test]
    fn builtin_manifests_parse() {
        let set = ManifestSet::load(Path::new("/does/not/exist")).unwrap();
        assert_eq!(set.get(AgentKind::Codex).version, 1);
        assert_eq!(set.aliases().get("codex-cli"), Some(&AgentKind::Codex));
    }

    #[test]
    fn codex_osc_title_is_a_strong_stable_signal() {
        let set = ManifestSet::load(Path::new("/does/not/exist")).unwrap();
        let codex = set.get(AgentKind::Codex);
        let working = codex.classify("", "⠹ project");
        assert_eq!(working.state, BaseState::Working);
        assert!(working.strong_visible_signal);
        let idle = codex.classify("", "project");
        assert_eq!(idle.state, BaseState::Idle);
        assert!(!idle.strong_visible_signal);
        assert_eq!(
            codex.classify("", "Action Required").state,
            BaseState::Blocked
        );
    }

    #[test]
    fn claude_btw_overlay_tracks_only_the_foreground_turn() {
        let set = ManifestSet::load(Path::new("/does/not/exist")).unwrap();
        let claude = set.get(AgentKind::Claude);
        let result = claude.classify("/btw investigate this\nesc to close", "");
        assert_eq!(result.state, BaseState::Working);
        assert_eq!(
            result.rule_id.as_deref(),
            Some("claude-btw-overlay-working")
        );
    }

    #[test]
    fn rejects_future_engine_and_honors_visible_state() {
        let future = r#"version = 1
min_engine_version = "99.0.0"
aliases = ["future-agent"]
rules = []"#;
        assert!(Manifest::parse(future).is_err());

        let visible = r#"version = 1
aliases = ["fixture"]
[[rules]]
id = "idle"
visible = "idle"
matcher = { contains = { value = "ready" } }"#;
        let result = Manifest::parse(visible).unwrap().classify("ready", "");
        assert_eq!(result.state, BaseState::Idle);
    }

    #[test]
    fn rejects_duplicate_rule_ids_and_cross_agent_aliases() {
        let duplicate = r#"version = 1
[[rules]]
id = "same"
state = "idle"
matcher = { contains = { value = "one" } }
[[rules]]
id = "same"
state = "working"
matcher = { contains = { value = "two" } }"#;
        assert!(Manifest::parse(duplicate).is_err());

        let temp = tempfile::tempdir().unwrap();
        fs::write(
            temp.path().join("claude.toml"),
            "version = 1\naliases = [\"codex\"]\n",
        )
        .unwrap();
        assert!(ManifestSet::load(temp.path()).is_err());
    }

    #[test]
    fn after_last_rule_ignores_stale_content_above_horizontal_separator() {
        let input = r#"version = 1
[[rules]]
id = "current"
state = "blocked"
region = "after_last_rule"
matcher = { contains = { value = "approval required" } }"#;
        let manifest = Manifest::parse(input).unwrap();
        let stale = "approval required\n────────────\nready for input";
        assert_eq!(manifest.classify(stale, "").state, BaseState::Unknown);

        let current = "old response\n----\napproval required";
        assert_eq!(manifest.classify(current, "").state, BaseState::Blocked);
    }

    #[test]
    fn four_agent_fixture_matrix_classifies_states_and_false_positives() {
        let set = ManifestSet::load(Path::new("/does/not/exist")).unwrap();
        let cases = [
            (
                AgentKind::Codex,
                "• Working on tests\nesc to interrupt",
                BaseState::Working,
            ),
            (
                AgentKind::Codex,
                "Would you like to run this command?",
                BaseState::Blocked,
            ),
            (AgentKind::Codex, "› Ask anything", BaseState::Idle),
            (AgentKind::Codex, "ordinary output", BaseState::Unknown),
            (
                AgentKind::Codex,
                "let text = \"Would you like to run this command?\";",
                BaseState::Unknown,
            ),
            (
                AgentKind::Codex,
                "◐ Analyzing updated TUI",
                BaseState::Working,
            ),
            (
                AgentKind::Claude,
                "✶ Thinking\nesc to interrupt",
                BaseState::Working,
            ),
            (
                AgentKind::Claude,
                "╭ Permission\nDo you want to proceed?\nEsc to cancel",
                BaseState::Blocked,
            ),
            (AgentKind::Claude, "❯", BaseState::Idle),
            (AgentKind::Claude, "plain shell", BaseState::Unknown),
            (
                AgentKind::Claude,
                "println!(\"Do you want to proceed?\");",
                BaseState::Unknown,
            ),
            (
                AgentKind::Claude,
                "✻ Generating new TUI",
                BaseState::Working,
            ),
            (AgentKind::Trae, "● Working on task", BaseState::Working),
            (AgentKind::Trae, "Waiting for approval", BaseState::Blocked),
            (AgentKind::Trae, "›", BaseState::Idle),
            (AgentKind::Trae, "shell output", BaseState::Unknown),
            (
                AgentKind::Trae,
                "const x = 'Waiting for approval';",
                BaseState::Unknown,
            ),
            (
                AgentKind::Trae,
                "• Thinking in revised TUI",
                BaseState::Working,
            ),
            (
                AgentKind::Opencode,
                "● Generating response",
                BaseState::Working,
            ),
            (
                AgentKind::Opencode,
                "╭ Permission\nWaiting for permission",
                BaseState::Blocked,
            ),
            (AgentKind::Opencode, ">", BaseState::Idle),
            (AgentKind::Opencode, "normal output", BaseState::Unknown),
            (
                AgentKind::Opencode,
                "value = 'Waiting for permission'",
                BaseState::Unknown,
            ),
            (
                AgentKind::Opencode,
                "• Thinking in new TUI",
                BaseState::Working,
            ),
        ];
        for (kind, content, expected) in cases {
            assert_eq!(
                set.get(kind).classify(content, "").state,
                expected,
                "fixture failed for {kind:?}: {content:?}"
            );
        }
    }
}
