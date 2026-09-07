use std::process::Child;
use std::thread;
use std::time::{Duration, Instant};

/// A connection subprocess must never outlive the attach invocation that owns
/// its terminal. Dropping std::process::Child alone leaves it running.
pub(crate) struct AttachedProcess {
    pub(crate) child: Child,
}

impl AttachedProcess {
    pub(crate) fn new(child: Child) -> Self {
        Self { child }
    }
}

impl Drop for AttachedProcess {
    fn drop(&mut self) {
        if matches!(self.child.try_wait(), Ok(Some(_))) {
            return;
        }
        // Give ssh a chance to restore the local terminal's termios settings.
        // This child is still ours and has not been reaped, so its PID cannot
        // have been reused for an unrelated process.
        unsafe { libc::kill(self.child.id() as libc::pid_t, libc::SIGTERM) };
        let deadline = Instant::now() + Duration::from_secs(1);
        while Instant::now() < deadline {
            if matches!(self.child.try_wait(), Ok(Some(_))) {
                return;
            }
            thread::sleep(Duration::from_millis(10));
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{BufRead, BufReader};
    use std::process::{Command, Stdio};

    fn connection(ignore_term: bool) -> AttachedProcess {
        let script = if ignore_term {
            "trap '' TERM; printf 'ready\\n'; exec sleep 30"
        } else {
            "printf 'ready\\n'; exec sleep 30"
        };
        let mut process = AttachedProcess::new(
            Command::new("sh")
                .args(["-c", script])
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .spawn()
                .unwrap(),
        );
        let mut ready = String::new();
        BufReader::new(process.child.stdout.take().unwrap())
            .read_line(&mut ready)
            .unwrap();
        assert_eq!(ready, "ready\n");
        process
    }

    fn assert_reaped(pid: u32) {
        let mut status = 0;
        assert_eq!(
            unsafe { libc::waitpid(pid as libc::pid_t, &mut status, libc::WNOHANG) },
            -1
        );
        assert_eq!(
            std::io::Error::last_os_error().raw_os_error(),
            Some(libc::ECHILD)
        );
    }

    #[test]
    fn error_return_closes_both_connections() {
        fn fail(pids: &mut Vec<u32>) -> Result<(), &'static str> {
            let control = connection(false);
            pids.push(control.child.id());
            let pty = connection(false);
            pids.push(pty.child.id());
            Err("control frame failed")?;
            Ok(())
        }
        let mut pids = Vec::new();
        assert!(fail(&mut pids).is_err());
        for pid in pids {
            assert_reaped(pid);
        }
    }

    #[test]
    fn unresponsive_connection_is_killed_and_reaped() {
        let process = connection(true);
        let pid = process.child.id();
        drop(process);
        assert_reaped(pid);
    }

    #[test]
    fn already_waited_connection_can_be_dropped() {
        let mut process = AttachedProcess::new(Command::new("true").spawn().unwrap());
        let pid = process.child.id();
        assert!(process.child.wait().unwrap().success());
        drop(process);
        assert_reaped(pid);
    }
}
