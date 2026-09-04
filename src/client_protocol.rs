use std::io::{self, Read, Write};

use serde::{Deserialize, Serialize};

use crate::model::TmuxTarget;

pub const MAX_FRAME_BYTES: usize = 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum ClientMessage {
    Hello {
        version: u32,
        device_id: String,
        device_label: String,
        kind: String,
        capabilities: Vec<String>,
    },
    Welcome {
        version: u32,
        endpoint_id: String,
        heartbeat_seconds: u32,
        attachment_token: String,
    },
    Heartbeat {
        version: u32,
        activity_unix_ms: u64,
    },
    HeartbeatAck {
        version: u32,
        events: u32,
    },
    AttachmentBind {
        version: u32,
        token: String,
        tty: String,
    },
    EventDelivery {
        version: u32,
        event_id: String,
        category: String,
        title: String,
        body: String,
        target: TmuxTarget,
    },
    EventAccepted {
        version: u32,
        event_id: String,
    },
    FocusTarget {
        version: u32,
        event_id: String,
        target: TmuxTarget,
    },
    FocusRequest {
        version: u32,
        focused: bool,
        overlay_visible: bool,
    },
    FocusResult {
        version: u32,
        focused: Option<bool>,
        active_pane: Option<String>,
    },
    ClipboardRead {
        version: u32,
        request_id: String,
    },
    ClipboardWrite {
        version: u32,
        request_id: String,
        text: String,
    },
    ClipboardResult {
        version: u32,
        request_id: String,
        text: Option<String>,
        error: Option<String>,
    },
    Goodbye {
        version: u32,
    },
}

impl ClientMessage {
    pub fn validate(&self) -> Result<(), String> {
        let version = match self {
            Self::Hello { version, .. }
            | Self::Welcome { version, .. }
            | Self::Heartbeat { version, .. }
            | Self::HeartbeatAck { version, .. }
            | Self::AttachmentBind { version, .. }
            | Self::EventDelivery { version, .. }
            | Self::EventAccepted { version, .. }
            | Self::FocusTarget { version, .. }
            | Self::FocusRequest { version, .. }
            | Self::FocusResult { version, .. }
            | Self::ClipboardRead { version, .. }
            | Self::ClipboardWrite { version, .. }
            | Self::ClipboardResult { version, .. }
            | Self::Goodbye { version } => *version,
        };
        if version != crate::CLIENT_PROTOCOL_VERSION {
            return Err(format!("unsupported client protocol version {version}"));
        }
        if let Self::ClipboardWrite { text, .. } = self {
            validate_clipboard(text)?;
        }
        if let Self::ClipboardResult {
            text: Some(text), ..
        } = self
        {
            validate_clipboard(text)?;
        }
        Ok(())
    }
}

pub fn validate_clipboard(text: &str) -> Result<(), String> {
    if text.len() > MAX_FRAME_BYTES {
        return Err("clipboard text exceeds 1 MiB".into());
    }
    if text.contains('\0') {
        return Err("clipboard text contains NUL".into());
    }
    Ok(())
}

pub fn write_frame(mut writer: impl Write, message: &ClientMessage) -> io::Result<()> {
    message.validate().map_err(io::Error::other)?;
    let payload = serde_json::to_vec(message).map_err(io::Error::other)?;
    if payload.len() > MAX_FRAME_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "frame exceeds 1 MiB",
        ));
    }
    writer.write_all(&(payload.len() as u32).to_be_bytes())?;
    writer.write_all(&payload)?;
    writer.flush()
}

pub fn read_frame(mut reader: impl Read) -> io::Result<ClientMessage> {
    let mut length = [0_u8; 4];
    reader.read_exact(&mut length)?;
    let length = u32::from_be_bytes(length) as usize;
    if length == 0 || length > MAX_FRAME_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "invalid frame length",
        ));
    }
    let mut payload = vec![0_u8; length];
    reader.read_exact(&mut payload)?;
    let text = std::str::from_utf8(&payload)
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "frame is not UTF-8"))?;
    let message: ClientMessage = serde_json::from_str(text).map_err(io::Error::other)?;
    message.validate().map_err(io::Error::other)?;
    Ok(message)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn target() -> TmuxTarget {
        TmuxTarget {
            session_id: "$1".into(),
            session_name: "task".into(),
            window_id: "@2".into(),
            window_index: 1,
            window_name: "agent".into(),
            pane_id: "%3".into(),
            pane_index: 0,
        }
    }

    #[test]
    fn framed_json_round_trips() {
        let message = ClientMessage::Heartbeat {
            version: crate::CLIENT_PROTOCOL_VERSION,
            activity_unix_ms: 42,
        };
        let mut bytes = Vec::new();
        write_frame(&mut bytes, &message).unwrap();
        assert_eq!(read_frame(bytes.as_slice()).unwrap(), message);
    }

    #[test]
    fn rejects_oversized_invalid_and_nul_frames() {
        let bytes = ((MAX_FRAME_BYTES as u32) + 1).to_be_bytes();
        assert!(read_frame(bytes.as_slice()).is_err());
        assert!(validate_clipboard("bad\0text").is_err());
        assert!(validate_clipboard(&"x".repeat(MAX_FRAME_BYTES + 1)).is_err());
        let mut invalid = (2_u32).to_be_bytes().to_vec();
        invalid.extend_from_slice(&[0xff, 0xff]);
        assert!(read_frame(invalid.as_slice()).is_err());
    }

    #[test]
    fn focus_target_round_trips_with_exact_pane() {
        let message = ClientMessage::FocusTarget {
            version: crate::CLIENT_PROTOCOL_VERSION,
            event_id: "codex.42".into(),
            target: target(),
        };
        let mut bytes = Vec::new();
        write_frame(&mut bytes, &message).unwrap();
        assert_eq!(read_frame(bytes.as_slice()).unwrap(), message);
    }
}
