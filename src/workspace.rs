use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::Command;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RepoRecord {
    pub name: String,
    pub main_checkout: PathBuf,
    pub worktree: PathBuf,
    pub branch: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskRecord {
    pub name: String,
    pub root: PathBuf,
    pub repos: Vec<RepoRecord>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspaceRecord {
    pub version: u32,
    pub id: Uuid,
    pub name: String,
    pub root: PathBuf,
    pub session: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub profile: Option<String>,
    #[serde(default)]
    pub repos: Vec<RepoRecord>,
    #[serde(default)]
    pub tasks: Vec<TaskRecord>,
    pub created_unix_ms: u64,
    pub updated_unix_ms: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retained_reason: Option<String>,
}

#[derive(Debug, Clone)]
pub struct Registry {
    root: PathBuf,
}

impl Registry {
    pub fn new(root: PathBuf) -> Self {
        Self { root }
    }

    pub fn list(&self) -> io::Result<Vec<WorkspaceRecord>> {
        let mut records = Vec::new();
        let entries = match fs::read_dir(&self.root) {
            Ok(entries) => entries,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(records),
            Err(error) => return Err(error),
        };
        for entry in entries {
            let path = entry?.path();
            if path.extension().and_then(|part| part.to_str()) != Some("toml") {
                continue;
            }
            let text = fs::read_to_string(path)?;
            records.push(toml::from_str(&text).map_err(io::Error::other)?);
        }
        records.sort_by(|a, b| a.name.cmp(&b.name).then_with(|| a.root.cmp(&b.root)));
        Ok(records)
    }

    pub fn find_by_root(&self, root: &Path) -> io::Result<Option<WorkspaceRecord>> {
        let root = canonical_or_owned(root);
        Ok(self
            .list()?
            .into_iter()
            .find(|record| canonical_or_owned(&record.root) == root))
    }

    pub fn save(&self, record: &WorkspaceRecord) -> io::Result<()> {
        fs::create_dir_all(&self.root)?;
        fs::set_permissions(&self.root, fs::Permissions::from_mode(0o700))?;
        let path = self.root.join(format!("{}.toml", record.id));
        let temporary = self.root.join(format!(".{}.tmp", Uuid::new_v4()));
        let mut file = OpenOptions::new()
            .create_new(true)
            .write(true)
            .mode(0o600)
            .open(&temporary)?;
        file.write_all(
            toml::to_string_pretty(record)
                .map_err(io::Error::other)?
                .as_bytes(),
        )?;
        file.sync_all()?;
        fs::rename(&temporary, &path)?;
        fs::set_permissions(path, fs::Permissions::from_mode(0o600))
    }

    pub fn remove(&self, id: Uuid) -> io::Result<()> {
        match fs::remove_file(self.root.join(format!("{id}.toml"))) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error),
        }
    }

    pub fn lazy_migrate(
        &self,
        workspace_root: &Path,
        now_ms: u64,
    ) -> io::Result<Vec<WorkspaceRecord>> {
        let mut existing = self.list()?;
        for mut record in existing.clone() {
            if !record.root.exists() {
                self.remove(record.id)?;
            } else {
                let repos = discover_repos(&record.root)?;
                let tasks = discover_tasks(&record.root)?;
                if record.repos != repos || record.tasks != tasks {
                    record.repos = repos;
                    record.tasks = tasks;
                    record.updated_unix_ms = now_ms;
                    self.save(&record)?;
                }
            }
        }
        existing = self.list()?;
        let entries = match fs::read_dir(workspace_root) {
            Ok(entries) => entries,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(existing),
            Err(error) => return Err(error),
        };
        for entry in entries {
            let entry = entry?;
            if !entry.file_type()?.is_dir() {
                continue;
            }
            let root = canonical_or_owned(&entry.path());
            if existing
                .iter()
                .any(|record| canonical_or_owned(&record.root) == root)
            {
                continue;
            }
            let name = entry.file_name().to_string_lossy().into_owned();
            let repos = discover_repos(&root)?;
            let tasks = discover_tasks(&root)?;
            let record = WorkspaceRecord {
                version: 1,
                id: Uuid::new_v4(),
                name: name.clone(),
                root,
                session: name,
                profile: None,
                repos,
                tasks,
                created_unix_ms: now_ms,
                updated_unix_ms: now_ms,
                retained_reason: None,
            };
            self.save(&record)?;
            existing.push(record);
        }
        existing.sort_by(|a, b| a.name.cmp(&b.name).then_with(|| a.root.cmp(&b.root)));
        Ok(existing)
    }
}

fn discover_repos(root: &Path) -> io::Result<Vec<RepoRecord>> {
    let mut candidates = Vec::new();
    if root.join(".git").exists() {
        candidates.push(root.to_path_buf());
    } else {
        for entry in fs::read_dir(root)? {
            let entry = entry?;
            if entry.file_type()?.is_dir() && entry.path().join(".git").exists() {
                candidates.push(entry.path());
            }
        }
    }
    let mut repos = Vec::new();
    for path in candidates {
        let worktree = canonical_or_owned(&path);
        let branch = git_output(&worktree, &["branch", "--show-current"]).unwrap_or_default();
        let common = git_output(
            &worktree,
            &["rev-parse", "--path-format=absolute", "--git-common-dir"],
        )
        .map(PathBuf::from)
        .unwrap_or_default();
        let main_checkout = common
            .parent()
            .map(canonical_or_owned)
            .unwrap_or_else(|| worktree.clone());
        repos.push(RepoRecord {
            name: path
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .into(),
            main_checkout,
            worktree,
            branch,
        });
    }
    repos.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(repos)
}

fn discover_tasks(root: &Path) -> io::Result<Vec<TaskRecord>> {
    let mut tasks = Vec::new();
    let directory = root.join("tasks");
    if directory.is_symlink() || !directory.is_dir() {
        return Ok(tasks);
    }
    for entry in fs::read_dir(directory)? {
        let entry = entry?;
        if !entry.file_type()?.is_dir() {
            continue;
        }
        tasks.push(TaskRecord {
            name: entry.file_name().to_string_lossy().into(),
            root: canonical_or_owned(&entry.path()),
            repos: discover_repos(&entry.path())?,
        });
    }
    tasks.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(tasks)
}

fn git_output(cwd: &Path, args: &[&str]) -> Option<String> {
    let output = Command::new("git")
        .args(args)
        .current_dir(cwd)
        .output()
        .ok()?;
    output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).trim().to_owned())
}

fn canonical_or_owned(path: &Path) -> PathBuf {
    fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn registry_round_trips_and_keeps_same_names_distinct() {
        let temp = tempfile::tempdir().unwrap();
        let registry = Registry::new(temp.path().join("registry"));
        for suffix in ["a", "b"] {
            let record = WorkspaceRecord {
                version: 1,
                id: Uuid::new_v4(),
                name: "same".into(),
                root: temp.path().join(suffix),
                session: "same".into(),
                profile: None,
                repos: vec![],
                tasks: vec![],
                created_unix_ms: 1,
                updated_unix_ms: 1,
                retained_reason: None,
            };
            registry.save(&record).unwrap();
        }
        assert_eq!(registry.list().unwrap().len(), 2);
    }

    #[test]
    fn refresh_discovers_tasks_added_after_workspace_registration() {
        let temp = tempfile::tempdir().unwrap();
        let pool = temp.path().join("workspaces");
        let root = pool.join("direction");
        fs::create_dir_all(&root).unwrap();
        let registry = Registry::new(temp.path().join("registry"));
        let original = registry.lazy_migrate(&pool, 1).unwrap().remove(0);
        let repo = root.join("tasks/first/app");
        fs::create_dir_all(&repo).unwrap();
        assert!(
            Command::new("git")
                .args(["init", "-q", "-b", "main"])
                .current_dir(&repo)
                .status()
                .unwrap()
                .success()
        );
        let updated = registry.lazy_migrate(&pool, 2).unwrap().remove(0);
        assert_eq!(updated.id, original.id);
        assert!(updated.repos.is_empty());
        assert_eq!(updated.tasks.len(), 1);
        assert_eq!(updated.tasks[0].name, "first");
        assert_eq!(
            updated.tasks[0].repos[0].worktree,
            canonical_or_owned(&repo)
        );
        assert_eq!(updated.updated_unix_ms, 2);
        fs::remove_dir_all(root.join("tasks/first")).unwrap();
        assert!(registry.lazy_migrate(&pool, 3).unwrap()[0].tasks.is_empty());
    }
}
