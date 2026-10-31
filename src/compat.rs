//! Compatibility for the Linux interfaces missing from older iSH releases.
use std::io::{self, Read, Write};
use std::net::TcpStream;
use std::os::fd::{AsRawFd, RawFd};
use std::os::unix::net::UnixStream;
use std::time::{Duration, Instant};

pub fn is_ish() -> bool {
    #[cfg(target_os = "linux")]
    {
        let mut info = std::mem::MaybeUninit::<libc::utsname>::uninit();
        if unsafe { libc::uname(info.as_mut_ptr()) } == 0 {
            let info = unsafe { info.assume_init() };
            let release = unsafe { std::ffi::CStr::from_ptr(info.release.as_ptr()) };
            return release.to_bytes().windows(4).any(|part| part == b"-ish");
        }
    }
    false
}

/// Native streams retain their OS timeouts. iSH uses nonblocking I/O and poll:
/// its kernel rejects both the old and time64 socket timeout options.
pub struct TimedIo<T> {
    inner: T,
    fd: RawFd,
    deadline: Option<Instant>,
}

pub fn unix_io(stream: &mut UnixStream, timeout: Duration) -> io::Result<TimedIo<&mut UnixStream>> {
    if is_ish() {
        return polling_unix(stream, timeout);
    }
    stream.set_nonblocking(false)?;
    stream.set_read_timeout(Some(timeout))?;
    stream.set_write_timeout(Some(timeout))?;
    Ok(TimedIo {
        fd: stream.as_raw_fd(),
        inner: stream,
        deadline: None,
    })
}

fn polling_unix(
    stream: &mut UnixStream,
    timeout: Duration,
) -> io::Result<TimedIo<&mut UnixStream>> {
    let deadline = deadline(timeout)?;
    stream.set_nonblocking(true)?;
    Ok(TimedIo {
        fd: stream.as_raw_fd(),
        inner: stream,
        deadline: Some(deadline),
    })
}

pub fn tcp_io(
    stream: &mut TcpStream,
    read: Duration,
    write: Option<Duration>,
) -> io::Result<TimedIo<&mut TcpStream>> {
    let deadline = if is_ish() {
        let deadline = deadline(read)?;
        stream.set_nonblocking(true)?;
        Some(deadline)
    } else {
        stream.set_read_timeout(Some(read))?;
        if let Some(write) = write {
            stream.set_write_timeout(Some(write))?;
        }
        None
    };
    Ok(TimedIo {
        fd: stream.as_raw_fd(),
        inner: stream,
        deadline,
    })
}

fn deadline(timeout: Duration) -> io::Result<Instant> {
    if timeout.is_zero() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "socket timeout must be nonzero",
        ));
    }
    Instant::now().checked_add(timeout).ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "socket timeout exceeds clock range",
        )
    })
}

impl<T> TimedIo<T> {
    fn remaining(&self) -> io::Result<Option<Duration>> {
        match self.deadline {
            Some(deadline) => deadline
                .checked_duration_since(Instant::now())
                .filter(|value| !value.is_zero())
                .map(Some)
                .ok_or_else(|| io::Error::new(io::ErrorKind::TimedOut, "socket I/O timed out")),
            None => Ok(None),
        }
    }

    fn wait(&self, events: libc::c_short) -> io::Result<()> {
        loop {
            let Some(remaining) = self.remaining()? else {
                return Ok(());
            };
            let millis = remaining
                .as_millis()
                .saturating_add(1)
                .min(i32::MAX as u128) as i32;
            let mut fd = libc::pollfd {
                fd: self.fd,
                events,
                revents: 0,
            };
            let result = unsafe { libc::poll(&mut fd, 1, millis) };
            if result > 0 {
                return Ok(());
            }
            if result == -1 {
                let error = io::Error::last_os_error();
                if error.kind() != io::ErrorKind::Interrupted {
                    return Err(error);
                }
            }
        }
    }
}

impl<T: Read> Read for TimedIo<T> {
    fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
        loop {
            self.remaining()?;
            match self.inner.read(bytes) {
                Err(error)
                    if self.deadline.is_some() && error.kind() == io::ErrorKind::WouldBlock =>
                {
                    self.wait(libc::POLLIN)?
                }
                Err(error)
                    if self.deadline.is_some() && error.kind() == io::ErrorKind::Interrupted =>
                {
                    continue;
                }
                result => return result,
            }
        }
    }
}

impl<T: Write> Write for TimedIo<T> {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        loop {
            self.remaining()?;
            match self.inner.write(bytes) {
                Err(error)
                    if self.deadline.is_some() && error.kind() == io::ErrorKind::WouldBlock =>
                {
                    self.wait(libc::POLLOUT)?
                }
                Err(error)
                    if self.deadline.is_some() && error.kind() == io::ErrorKind::Interrupted =>
                {
                    continue;
                }
                result => return result,
            }
        }
    }
    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}

impl<T> AsRawFd for TimedIo<T> {
    fn as_raw_fd(&self) -> RawFd {
        self.fd
    }
}

/// iSH implements nanosleep but raises SIGSYS for clock_nanosleep.
pub fn sleep(duration: Duration) {
    #[cfg(all(target_os = "linux", target_arch = "x86"))]
    if is_ish() {
        legacy_sleep(duration);
        return;
    }
    std::thread::sleep(duration);
}

#[cfg(all(target_os = "linux", target_arch = "x86"))]
fn legacy_sleep(mut duration: Duration) {
    #[repr(C)]
    #[derive(Clone, Copy)]
    struct Timespec32 {
        seconds: i32,
        nanos: i32,
    }
    while !duration.is_zero() {
        let seconds = duration.as_secs().min(i32::MAX as u64);
        let nanos = if seconds == duration.as_secs() {
            duration.subsec_nanos()
        } else {
            0
        };
        let chunk = Duration::new(seconds, nanos);
        let mut requested = Timespec32 {
            seconds: seconds as i32,
            nanos: nanos as i32,
        };
        loop {
            let mut remainder = Timespec32 {
                seconds: 0,
                nanos: 0,
            };
            let result = unsafe {
                libc::syscall(
                    libc::SYS_nanosleep,
                    &requested as *const Timespec32,
                    &mut remainder as *mut Timespec32,
                )
            };
            if result == 0 {
                break;
            }
            let error = io::Error::last_os_error();
            if error.kind() != io::ErrorKind::Interrupted {
                panic!("nanosleep failed: {error}");
            }
            requested = remainder;
        }
        duration -= chunk;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn native_or_ish_timeout_keeps_unanswered_reads_bounded() {
        let (mut reader, _writer) = UnixStream::pair().unwrap();
        let mut stream = unix_io(&mut reader, Duration::from_millis(20)).unwrap();
        let error = stream.read(&mut [0]).unwrap_err();
        assert!(matches!(
            error.kind(),
            io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
        ));
    }

    #[test]
    fn polling_deadline_keeps_unanswered_reads_bounded() {
        let (mut reader, _writer) = UnixStream::pair().unwrap();
        let mut stream = polling_unix(&mut reader, Duration::from_millis(20)).unwrap();
        assert_eq!(
            stream.read(&mut [0]).unwrap_err().kind(),
            io::ErrorKind::TimedOut
        );
        assert!(polling_unix(&mut reader, Duration::ZERO).is_err());
    }

    #[test]
    fn polling_preserves_large_frames_and_partial_io() {
        let (mut client, mut server) = UnixStream::pair().unwrap();
        let peer = std::thread::spawn(move || {
            let mut io = polling_unix(&mut server, Duration::from_secs(5)).unwrap();
            let mut received = vec![0; 256 * 1024];
            io.read_exact(&mut received).unwrap();
            assert!(received.iter().all(|byte| *byte == 42));
            io.write_all(b"ok").unwrap();
        });
        let mut io = polling_unix(&mut client, Duration::from_secs(5)).unwrap();
        io.write_all(&vec![42; 256 * 1024]).unwrap();
        let mut reply = [0; 2];
        io.read_exact(&mut reply).unwrap();
        assert_eq!(&reply, b"ok");
        peer.join().unwrap();
    }

    #[test]
    #[cfg(all(target_os = "linux", target_arch = "x86"))]
    fn nanosleep_fallback_waits_for_the_requested_duration() {
        let started = Instant::now();
        legacy_sleep(Duration::from_millis(20));
        assert!(started.elapsed() >= Duration::from_millis(20));
        legacy_sleep(Duration::ZERO);
    }
}
