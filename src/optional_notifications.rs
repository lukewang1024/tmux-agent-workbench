use std::io;
use std::os::unix::process::CommandExt;
use std::process::{Command, ExitStatus, Stdio};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
    mpsc,
};
use std::thread;
use std::time::{Duration, Instant};

// A bounded queue keeps desktop APIs out of the SSH/heartbeat loop.
pub(crate) struct Worker<T> {
    sender: Option<mpsc::SyncSender<T>>,
    stopped: Arc<AtomicBool>,
    thread: Option<thread::JoinHandle<()>>,
}

impl<T: Send + 'static> Worker<T> {
    pub(crate) fn new(mut deliver: impl FnMut(T) -> Result<(), String> + Send + 'static) -> Self {
        let (sender, receiver) = mpsc::sync_channel(16);
        let stopped = Arc::new(AtomicBool::new(false));
        let stop = stopped.clone();
        let thread = thread::spawn(move || {
            while let Ok(event) = receiver.recv() {
                if stop.load(Ordering::Acquire) {
                    break;
                }
                if let Err(error) = deliver(event) {
                    eprintln!("tmux-agent-workbench: notification skipped: {error}");
                }
            }
        });
        Self {
            sender: Some(sender),
            stopped,
            thread: Some(thread),
        }
    }

    pub(crate) fn submit(&self, event: T) {
        if self.sender.as_ref().unwrap().try_send(event).is_err() {
            eprintln!("tmux-agent-workbench: notification skipped: queue unavailable");
        }
    }
}

impl<T> Drop for Worker<T> {
    fn drop(&mut self) {
        self.stopped.store(true, Ordering::Release);
        self.sender.take();
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

pub(crate) fn run(command: &mut Command, timeout: Duration) -> io::Result<ExitStatus> {
    // Termux scripts spawn termux-api: terminate the entire helper group.
    let mut child = command
        .process_group(0)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()?;
    let deadline = Instant::now() + timeout;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return Ok(status),
            Ok(None) if Instant::now() < deadline => thread::sleep(Duration::from_millis(10)),
            result => {
                unsafe {
                    libc::kill(-(child.id() as libc::pid_t), libc::SIGKILL);
                }
                let _ = child.wait();
                return match result {
                    Err(error) => Err(error),
                    _ => Err(io::Error::new(
                        io::ErrorKind::TimedOut,
                        "notification helper timed out",
                    )),
                };
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn failed_delivery_does_not_stop_later_notifications() {
        let (tx, rx) = mpsc::channel();
        let worker = Worker::new(move |value: u8| {
            tx.send(value).unwrap();
            Err("permission denied".into())
        });
        worker.submit(1);
        worker.submit(2);
        assert_eq!(rx.recv_timeout(Duration::from_secs(1)).unwrap(), 1);
        assert_eq!(rx.recv_timeout(Duration::from_secs(1)).unwrap(), 2);
    }

    #[test]
    fn blocked_delivery_does_not_block_connection_or_grow_queue() {
        let (started, ready) = mpsc::channel();
        let (release, wait) = mpsc::channel();
        let worker = Worker::new(move |_: u8| {
            started.send(()).unwrap();
            wait.recv().unwrap();
            Ok(())
        });
        worker.submit(1);
        ready.recv_timeout(Duration::from_secs(1)).unwrap();
        for _ in 0..100 {
            worker.submit(2);
        }
        worker.stopped.store(true, Ordering::Release);
        release.send(()).unwrap();
        drop(worker);
    }

    #[test]
    fn missing_failed_and_hung_helpers_are_bounded() {
        assert!(
            run(
                &mut Command::new("/nonexistent/workbench-notifier"),
                Duration::from_secs(1)
            )
            .is_err()
        );
        assert!(
            !run(
                Command::new("sh").args(["-c", "exit 7"]),
                Duration::from_secs(1)
            )
            .unwrap()
            .success()
        );
        let start = Instant::now();
        let error = run(
            Command::new("sh").args(["-c", "sleep 30 & wait"]),
            Duration::from_millis(100),
        )
        .unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::TimedOut);
        assert!(start.elapsed() < Duration::from_secs(2));
    }
}
