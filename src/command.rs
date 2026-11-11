//! Start frequently used system commands without amplifying macOS process events.
//!
//! macOS `posix_spawnp` can create a short-lived process for every unsuccessful
//! PATH entry. Resolve the same first executable in-process before spawning;
//! preserve PATH ordering and re-evaluate it on every call so shims, permission
//! changes, and relative entries continue to work.

use std::process::Command;

pub fn new(program: &str) -> Command {
    // Debug-only accounting for the isolated idle-budget test. Record command
    // construction rather than wrapping the executable in another script,
    // which itself changes endpoint-security workload and timing. Construction
    // is an upper bound on the number of children actually started.
    #[cfg(all(debug_assertions, unix))]
    if let Some(path) = std::env::var_os("WORKBENCH_TEST_SPAWN_TRACE") {
        use std::io::Write;
        use std::os::unix::fs::OpenOptionsExt;
        if let Ok(mut file) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .mode(0o600)
            .open(path)
        {
            let _ = file.write_all(format!("{}\t{}\n", std::process::id(), program).as_bytes());
        }
    }
    #[cfg(target_os = "macos")]
    if matches!(program, "tmux" | "ps") {
        use std::os::unix::process::CommandExt;

        if let Some(path) = std::env::var_os("PATH").and_then(|path| {
            let cwd = std::env::current_dir().ok()?;
            resolve_in_path(program, &path, Some(&cwd))
        }) {
            let mut command = Command::new(path);
            command.arg0(program);
            return command;
        }
    }

    // Preserve the platform's normal missing-command and unset-PATH behavior.
    Command::new(program)
}

#[cfg(any(target_os = "macos", test))]
fn resolve_in_path(
    program: &str,
    path: &std::ffi::OsStr,
    cwd: Option<&std::path::Path>,
) -> Option<std::path::PathBuf> {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;

    std::env::split_paths(path).find_map(|directory| {
        let directory = if directory.is_absolute() {
            directory
        } else {
            cwd?.join(directory)
        };
        let candidate = directory.join(program);
        if !candidate.is_file() {
            return None;
        }
        let c_path = CString::new(candidate.as_os_str().as_bytes()).ok()?;
        // SAFETY: c_path is NUL-terminated and lives throughout the call.
        let executable = unsafe {
            libc::faccessat(
                libc::AT_FDCWD,
                c_path.as_ptr(),
                libc::X_OK,
                libc::AT_EACCESS,
            ) == 0
        };
        executable.then_some(candidate)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::os::unix::fs::PermissionsExt;

    fn executable(path: &std::path::Path, message: &str) {
        fs::write(path, format!("#!/bin/sh\nprintf '%s' '{message}'\n")).unwrap();
        fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
    }

    #[test]
    fn path_search_respects_priority_permissions_and_later_changes() {
        let root = tempfile::tempdir().unwrap();
        let first = root.path().join("first");
        let second = root.path().join("second");
        fs::create_dir(&first).unwrap();
        fs::create_dir(&second).unwrap();
        fs::write(first.join("tmux"), "not executable").unwrap();
        fs::set_permissions(first.join("tmux"), fs::Permissions::from_mode(0o644)).unwrap();
        executable(&second.join("tmux"), "second");
        let path = std::env::join_paths([&first, &second]).unwrap();

        let selected = resolve_in_path("tmux", &path, Some(root.path())).unwrap();
        assert_eq!(selected, second.join("tmux"));
        assert_eq!(Command::new(selected).output().unwrap().stdout, b"second");

        executable(&first.join("tmux"), "first");
        let selected = resolve_in_path("tmux", &path, Some(root.path())).unwrap();
        assert_eq!(selected, first.join("tmux"));
        assert_eq!(Command::new(selected).output().unwrap().stdout, b"first");
        assert!(resolve_in_path("missing", &path, Some(root.path())).is_none());
    }

    #[test]
    fn path_search_supports_empty_and_relative_entries() {
        let root = tempfile::tempdir().unwrap();
        let relative = root.path().join("relative");
        fs::create_dir(&relative).unwrap();
        executable(&relative.join("ps"), "relative");
        executable(&root.path().join("tmux"), "cwd");
        let path = std::ffi::OsStr::new(":relative");

        assert_eq!(
            resolve_in_path("tmux", path, Some(root.path())),
            Some(root.path().join("tmux"))
        );
        assert_eq!(
            resolve_in_path("ps", path, Some(root.path())),
            Some(relative.join("ps"))
        );
        assert!(resolve_in_path("ps", path, None).is_none());
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn macos_command_resolves_the_child_process_path() {
        const CHILD: &str = "WORKBENCH_COMMAND_PATH_TEST_CHILD";
        if let Some(expected) = std::env::var_os(CHILD) {
            let mut tmux = new("tmux");
            assert_eq!(tmux.get_program(), expected);
            assert_eq!(tmux.output().unwrap().stdout, b"chosen");

            let mut missing = new("ps");
            assert_eq!(missing.get_program(), "ps");
            assert_eq!(
                missing.output().unwrap_err().kind(),
                std::io::ErrorKind::NotFound
            );
            return;
        }

        let root = tempfile::tempdir().unwrap();
        let missing = root.path().join("missing");
        let chosen = root.path().join("chosen");
        fs::create_dir(&chosen).unwrap();
        executable(&chosen.join("tmux"), "chosen");
        let path = std::env::join_paths([&missing, &chosen]).unwrap();
        let output = Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "command::tests::macos_command_resolves_the_child_process_path",
            ])
            .env("PATH", path)
            .env(CHILD, chosen.join("tmux"))
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "child failed: {}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }
}
