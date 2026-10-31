use serde::{Deserialize, Serialize};
use std::collections::HashMap;

pub const OPEN_URL_CAPABILITY: &str = "open-url";
const REQUEST_TTL_MS: u64 = 15_000;

pub fn validate_url(url: &str) -> Result<(), String> {
    if url.len() > 16 * 1024 || url.chars().any(|c| c.is_whitespace() || c.is_control()) {
        return Err("URL is too long or contains whitespace/control characters".into());
    }
    let (scheme, address) = url
        .split_once("://")
        .ok_or("only http:// and https:// URLs can be opened on a client")?;
    if !scheme.eq_ignore_ascii_case("http") && !scheme.eq_ignore_ascii_case("https") {
        return Err("only http:// and https:// URLs can be opened on a client".into());
    }
    if address.is_empty() || address.starts_with(['/', '?', '#']) {
        return Err("URL has no host".into());
    }
    Ok(())
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct OpenUrl {
    pub request_id: String,
    pub url: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct ActionStatus {
    pub completed: bool,
    pub error: Option<String>,
}

#[derive(Debug)]
struct Request {
    endpoint: String,
    attachment: String,
    action: OpenUrl,
    expires_ms: u64,
    sent: bool,
    result: Option<Option<String>>,
}

#[derive(Debug, Default)]
pub struct Actions {
    requests: HashMap<String, Request>,
}

impl Actions {
    pub fn enqueue(
        &mut self,
        endpoint: &str,
        attachment: &str,
        url: String,
        now: u64,
    ) -> Result<String, String> {
        validate_url(&url)?;
        self.requests.retain(|_, r| r.expires_ms >= now);
        if self
            .requests
            .values()
            .filter(|r| r.endpoint == endpoint && r.result.is_none())
            .count()
            >= 8
        {
            return Err("too many pending URL requests for this client".into());
        }
        let id = uuid::Uuid::new_v4().to_string();
        self.requests.insert(
            id.clone(),
            Request {
                endpoint: endpoint.into(),
                attachment: attachment.into(),
                action: OpenUrl {
                    request_id: id.clone(),
                    url,
                },
                expires_ms: now + REQUEST_TTL_MS,
                sent: false,
                result: None,
            },
        );
        Ok(id)
    }

    pub fn take(&mut self, endpoint: &str, attachment: Option<&str>, now: u64) -> Vec<OpenUrl> {
        self.requests
            .values_mut()
            .filter_map(|request| {
                if request.endpoint == endpoint
                    && attachment == Some(request.attachment.as_str())
                    && !request.sent
                    && request.expires_ms >= now
                {
                    request.sent = true;
                    Some(request.action.clone())
                } else {
                    None
                }
            })
            .collect()
    }

    pub fn complete(
        &mut self,
        endpoint: &str,
        id: &str,
        error: Option<String>,
        now: u64,
    ) -> Result<(), String> {
        let request = self.requests.get_mut(id).ok_or("unknown URL request")?;
        if request.endpoint != endpoint || !request.sent || request.expires_ms < now {
            return Err("URL request does not belong to this live client".into());
        }
        request.result = Some(error.map(|s| s.chars().take(512).collect()));
        Ok(())
    }

    pub fn status(&self, attachment: &str, id: &str, now: u64) -> Result<ActionStatus, String> {
        let request = self.requests.get(id).ok_or("unknown URL request")?;
        if request.attachment != attachment {
            return Err("URL request belongs to another terminal".into());
        }
        if request.expires_ms < now {
            return Err("client URL request timed out".into());
        }
        Ok(ActionStatus {
            completed: request.result.is_some(),
            error: request.result.clone().flatten(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn requests_are_delivered_once_to_the_exact_client_and_acknowledged() {
        let mut actions = Actions::default();
        let id = actions
            .enqueue(
                "phone",
                "/dev/pts/9",
                "https://example.com/?x=1&y=2#part".into(),
                1,
            )
            .unwrap();
        assert!(actions.take("mac", Some("/dev/pts/0"), 2).is_empty());
        assert!(actions.take("phone", None, 2).is_empty());
        assert!(!actions.status("/dev/pts/9", &id, 2).unwrap().completed);
        assert!(actions.status("/dev/pts/0", &id, 2).is_err());
        assert_eq!(actions.take("phone", Some("/dev/pts/9"), 2).len(), 1);
        assert!(actions.take("phone", Some("/dev/pts/9"), 3).is_empty());
        assert!(actions.complete("mac", &id, None, 3).is_err());
        actions
            .complete("phone", &id, Some("helper unavailable".into()), 3)
            .unwrap();
        assert_eq!(
            actions
                .status("/dev/pts/9", &id, 4)
                .unwrap()
                .error
                .as_deref(),
            Some("helper unavailable")
        );
    }
    #[test]
    fn expired_requests_and_unsafe_schemes_are_rejected() {
        let mut actions = Actions::default();
        let id = actions
            .enqueue("phone", "tty", "https://example.com/".into(), 1)
            .unwrap();
        assert!(actions.take("phone", Some("tty"), 20_000).is_empty());
        assert!(actions.status("tty", &id, 20_000).is_err());
        for url in [
            "file:///tmp/a",
            "javascript:alert(1)",
            "https:///missing",
            "https://x/\ncommand",
        ] {
            assert!(validate_url(url).is_err(), "{url}");
        }
    }
}
