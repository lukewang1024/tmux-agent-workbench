use std::io::{self, BufRead, BufReader, Read, Write};
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Request {
    pub protocol_version: u32,
    pub id: String,
    pub method: String,
    #[serde(default)]
    pub params: Value,
}

impl Request {
    pub fn new(method: impl Into<String>, params: Value) -> Self {
        Self {
            protocol_version: crate::IPC_PROTOCOL_VERSION,
            id: uuid::Uuid::new_v4().to_string(),
            method: method.into(),
            params,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Response {
    pub protocol_version: u32,
    pub id: String,
    pub ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<IpcErrorBody>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IpcErrorBody {
    pub code: String,
    pub message: String,
}

impl Response {
    pub fn success(id: String, result: Value) -> Self {
        Self {
            protocol_version: crate::IPC_PROTOCOL_VERSION,
            id,
            ok: true,
            result: Some(result),
            error: None,
        }
    }

    pub fn error(id: String, code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            protocol_version: crate::IPC_PROTOCOL_VERSION,
            id,
            ok: false,
            result: None,
            error: Some(IpcErrorBody {
                code: code.into(),
                message: message.into(),
            }),
        }
    }
}

#[derive(Debug, Error)]
pub enum ClientError {
    #[error("daemon unavailable at {path}: {source}")]
    Connect { path: String, source: io::Error },
    #[error("daemon I/O failed: {0}")]
    Io(#[from] io::Error),
    #[error("invalid daemon response: {0}")]
    Json(#[from] serde_json::Error),
    #[error("daemon protocol mismatch: expected {expected}, got {actual}")]
    Protocol { expected: u32, actual: u32 },
    #[error("daemon returned {code}: {message}")]
    Remote { code: String, message: String },
}

pub fn call(socket: &Path, request: &Request, timeout: Duration) -> Result<Value, ClientError> {
    let response = exchange(socket, request, timeout)?;
    if response.protocol_version != crate::IPC_PROTOCOL_VERSION {
        return Err(ClientError::Protocol {
            expected: crate::IPC_PROTOCOL_VERSION,
            actual: response.protocol_version,
        });
    }
    if !response.ok {
        let error = response.error.unwrap_or(IpcErrorBody {
            code: "unknown".into(),
            message: "daemon returned no error body".into(),
        });
        return Err(ClientError::Remote {
            code: error.code,
            message: error.message,
        });
    }
    Ok(response.result.unwrap_or(Value::Null))
}

pub fn exchange(
    socket: &Path,
    request: &Request,
    timeout: Duration,
) -> Result<Response, ClientError> {
    let mut stream = UnixStream::connect(socket).map_err(|source| ClientError::Connect {
        path: socket.display().to_string(),
        source,
    })?;
    let mut stream = crate::compat::unix_io(&mut stream, timeout)?;
    write_message(&mut stream, request)?;

    let mut line = String::new();
    BufReader::new(stream).read_line(&mut line)?;
    Ok(serde_json::from_str(&line)?)
}

pub fn read_request(stream: impl Read) -> Result<Request, io::Error> {
    let mut line = String::new();
    BufReader::new(stream).read_line(&mut line)?;
    serde_json::from_str(&line).map_err(io::Error::other)
}

pub fn write_response(mut stream: impl Write, response: &Response) -> Result<(), io::Error> {
    write_message(&mut stream, response)
}

pub(crate) fn prepare_connection(
    stream: &mut UnixStream,
) -> io::Result<crate::compat::TimedIo<&mut UnixStream>> {
    // A client that never sends a request should not pin a daemon thread.
    crate::compat::unix_io(stream, Duration::from_secs(2))
}

pub(crate) fn prepare_response_connection(
    stream: &mut UnixStream,
) -> io::Result<crate::compat::TimedIo<&mut UnixStream>> {
    // Large snapshot replies can take longer to drain. Preparing a separate
    // response phase also resets iSH's shared polling deadline after reading.
    crate::compat::unix_io(stream, Duration::from_secs(5))
}

// Serialize once: serde's small writes must not each become a socket syscall.
fn write_message(writer: &mut impl Write, message: &impl Serialize) -> io::Result<()> {
    let mut bytes = serde_json::to_vec(message).map_err(io::Error::other)?;
    bytes.push(b'\n');
    writer.write_all(&bytes)?;
    writer.flush()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::net::UnixListener;

    #[test]
    fn accepted_nonblocking_socket_delivers_a_complete_large_response() {
        use std::os::fd::AsRawFd;
        let root = tempfile::tempdir_in("/tmp").unwrap();
        let socket = root.path().join("ipc.sock");
        let listener = UnixListener::bind(&socket).unwrap();
        let client = std::thread::spawn(move || {
            call(
                &socket,
                &Request::new("snapshot.get", Value::Null),
                Duration::from_secs(5),
            )
            .unwrap()
        });
        let (mut socket_stream, _) = listener.accept().unwrap();
        // Exercise the inherited macOS mode on every CI platform.
        socket_stream.set_nonblocking(true).unwrap();
        let request = {
            let mut stream = prepare_connection(&mut socket_stream).unwrap();
            assert_eq!(
                unsafe { libc::fcntl(stream.as_raw_fd(), libc::F_GETFL) } & libc::O_NONBLOCK,
                if crate::compat::is_ish() {
                    libc::O_NONBLOCK
                } else {
                    0
                }
            );
            read_request(&mut stream).unwrap()
        };
        let mut stream = prepare_response_connection(&mut socket_stream).unwrap();
        let response = serde_json::json!({"snapshot": "x".repeat(128 * 1024)});
        write_response(
            &mut stream,
            &Response::success(request.id, response.clone()),
        )
        .unwrap();
        assert_eq!(client.join().unwrap(), response);
    }

    #[test]
    fn slow_reader_receives_a_complete_large_response() {
        use std::os::fd::AsRawFd;
        let (mut server, client) = UnixStream::pair().unwrap();
        let buffer_size: libc::c_int = 4096;
        let result = unsafe {
            libc::setsockopt(
                server.as_raw_fd(),
                libc::SOL_SOCKET,
                libc::SO_SNDBUF,
                (&buffer_size as *const libc::c_int).cast(),
                std::mem::size_of_val(&buffer_size) as libc::socklen_t,
            )
        };
        assert_eq!(result, 0, "could not limit the test socket buffer");
        let read_delay = if crate::compat::is_ish() {
            // Leave more time for the slower iSH emulator to drain the frame.
            Duration::from_millis(2_200)
        } else {
            Duration::from_millis(4_200)
        };
        let (ready_tx, ready_rx) = std::sync::mpsc::channel();
        let reader = std::thread::spawn(move || {
            ready_rx.recv().unwrap();
            std::thread::sleep(read_delay);
            let mut line = String::new();
            BufReader::new(client).read_line(&mut line).unwrap();
            serde_json::from_str::<Response>(&line).unwrap()
        });
        let mut stream = prepare_response_connection(&mut server).unwrap();
        ready_tx.send(()).unwrap();
        let started = std::time::Instant::now();
        write_response(
            &mut stream,
            &Response::success(
                "slow-reader".into(),
                serde_json::json!({"data": "x".repeat(512 * 1024)}),
            ),
        )
        .unwrap();
        assert!(started.elapsed() >= read_delay.saturating_sub(Duration::from_millis(200)));
        let response = reader.join().unwrap();
        assert_eq!(response.id, "slow-reader");
        assert_eq!(
            response.result.unwrap()["data"].as_str().unwrap().len(),
            512 * 1024
        );
    }

    #[test]
    fn inactive_requester_keeps_the_short_read_deadline() {
        let (mut server, client) = UnixStream::pair().unwrap();
        let mut stream = prepare_connection(&mut server).unwrap();
        let started = std::time::Instant::now();
        assert!(read_request(&mut stream).is_err());
        assert!(
            started.elapsed() < Duration::from_secs(4),
            "a silent requester held the connection for the response timeout"
        );
        drop(client);
    }

    #[test]
    fn large_message_is_batched_and_keeps_newline_framing() {
        #[derive(Default)]
        struct Writer {
            bytes: Vec<u8>,
            calls: usize,
        }
        impl Write for Writer {
            fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
                self.calls += 1;
                self.bytes.extend_from_slice(bytes);
                Ok(bytes.len())
            }
            fn flush(&mut self) -> io::Result<()> {
                Ok(())
            }
        }
        let message = Response::success(
            "test".into(),
            serde_json::json!({
                "agents": (0..100).map(|i| serde_json::json!({"id": i, "label": "line\nquote\""})).collect::<Vec<_>>()
            }),
        );
        let mut writer = Writer::default();
        write_message(&mut writer, &message).unwrap();
        assert_eq!(writer.calls, 1);
        assert_eq!(writer.bytes.iter().filter(|b| **b == b'\n').count(), 1);
        assert_eq!(
            serde_json::from_slice::<Response>(&writer.bytes).unwrap(),
            message
        );
    }

    #[test]
    fn protocol_messages_reject_unknown_fields() {
        let value = r#"{"protocol_version":1,"id":"x","method":"snapshot.get","extra":true}"#;
        assert!(serde_json::from_str::<Request>(value).is_err());
    }

    #[test]
    fn client_rejects_a_daemon_with_another_protocol_version() {
        let temp = tempfile::tempdir().unwrap();
        let socket = temp.path().join("daemon.sock");
        let listener = UnixListener::bind(&socket).unwrap();
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = String::new();
            BufReader::new(stream.try_clone().unwrap())
                .read_line(&mut request)
                .unwrap();
            stream
                .write_all(b"{\"protocol_version\":2,\"id\":\"x\",\"ok\":true,\"result\":null}\n")
                .unwrap();
        });
        let error = call(
            &socket,
            &Request {
                protocol_version: 1,
                id: "x".into(),
                method: "daemon.status".into(),
                params: Value::Null,
            },
            Duration::from_secs(1),
        )
        .unwrap_err();
        assert!(matches!(error, ClientError::Protocol { actual: 2, .. }));
        server.join().unwrap();
    }
}
