use std::io::{self, BufRead, BufReader, Write};
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
    stream.set_read_timeout(Some(timeout))?;
    stream.set_write_timeout(Some(timeout))?;
    serde_json::to_writer(&mut stream, request)?;
    stream.write_all(b"\n")?;
    stream.flush()?;

    let mut line = String::new();
    BufReader::new(stream).read_line(&mut line)?;
    Ok(serde_json::from_str(&line)?)
}

pub fn read_request(stream: &UnixStream) -> Result<Request, io::Error> {
    let mut line = String::new();
    BufReader::new(stream).read_line(&mut line)?;
    serde_json::from_str(&line).map_err(io::Error::other)
}

pub fn write_response(mut stream: &UnixStream, response: &Response) -> Result<(), io::Error> {
    serde_json::to_writer(&mut stream, response).map_err(io::Error::other)?;
    stream.write_all(b"\n")?;
    stream.flush()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::net::UnixListener;

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
