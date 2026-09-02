use std::process::{Command, Stdio};

#[cfg(target_os = "macos")]
pub fn focus_tty(tty: &str) -> bool {
    if !valid_tty(tty) {
        return false;
    }
    let iterm = r#"on run argv
set targetTTY to item 1 of argv
tell application "iTerm2"
  repeat with w in windows
    repeat with t in tabs of w
      repeat with s in sessions of t
        if (tty of s as text) is targetTTY then
          select w
          select t
          select s
          activate
          return "found"
        end if
      end repeat
    end repeat
  end repeat
end tell
return "missing"
end run"#;
    if run_osascript(iterm, tty) {
        return true;
    }
    let terminal = r#"on run argv
set targetTTY to item 1 of argv
tell application "Terminal"
  repeat with w in windows
    repeat with t in tabs of w
      if (tty of t as text) is targetTTY then
        set selected of t to true
        set index of w to 1
        activate
        return "found"
      end if
    end repeat
  end repeat
end tell
return "missing"
end run"#;
    run_osascript(terminal, tty)
}

#[cfg(not(target_os = "macos"))]
pub fn focus_tty(_tty: &str) -> bool {
    false
}

#[cfg(target_os = "macos")]
fn run_osascript(script: &str, tty: &str) -> bool {
    Command::new("osascript")
        .args(["-e", script, tty])
        .stdin(Stdio::null())
        .output()
        .is_ok_and(|output| output.status.success() && output.stdout == b"found\n")
}

#[cfg(target_os = "macos")]
fn valid_tty(tty: &str) -> bool {
    tty.strip_prefix("/dev/").is_some_and(|name| {
        !name.is_empty()
            && name
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
    })
}

#[cfg(target_os = "macos")]
pub fn interactive_ssh_tty(host: &str) -> Option<String> {
    let output = Command::new("ps")
        .args(["-axo", "tty=,command="])
        .output()
        .ok()?;
    output
        .status
        .success()
        .then(|| parse_ssh_tty(&String::from_utf8_lossy(&output.stdout), host))
        .flatten()
}

#[cfg(not(target_os = "macos"))]
pub fn interactive_ssh_tty(_host: &str) -> Option<String> {
    None
}

fn parse_ssh_tty(output: &str, host: &str) -> Option<String> {
    output
        .lines()
        .filter_map(|line| {
            let line = line.trim();
            let split = line.find(char::is_whitespace)?;
            let tty = &line[..split];
            if tty == "??" || tty == "?" {
                return None;
            }
            let args: Vec<_> = line[split..].split_whitespace().collect();
            let ssh = args.iter().position(|arg| {
                std::path::Path::new(arg)
                    .file_name()
                    .is_some_and(|name| name == "ssh")
            })?;
            args[ssh + 1..]
                .contains(&host)
                .then(|| format!("/dev/{tty}"))
        })
        .next_back()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_interactive_ssh_tty_and_ignores_background_processes() {
        let ps = "?? /usr/bin/ssh -T cndevbox worker\n".to_owned()
            + "ttys003 ssh cndevbox\n"
            + "ttys004 ssh other\n";
        assert_eq!(
            parse_ssh_tty(&ps, "cndevbox").as_deref(),
            Some("/dev/ttys003")
        );
    }
}
