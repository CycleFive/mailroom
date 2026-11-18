//! `FeedClient` over a service binding to a broker's `Feed` entrypoint.

use mailroom_logic::feed::{FeedClient, FeedError, Identity};
use mailroom_logic::record::truncate;
use serde::Deserialize;
use worker::Fetcher;

pub struct ServiceFeed(pub Fetcher);

#[derive(Deserialize)]
struct ErrorBody {
    error: Option<String>,
}

/// Percent-encodes every byte outside the unreserved set.
fn encode(text: &str) -> String {
    let mut out = String::new();
    for b in text.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~') {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

impl ServiceFeed {
    async fn read(&self, url: &str) -> Result<String, FeedError> {
        let mut response = self
            .0
            .fetch(url, None)
            .await
            .map_err(|e| FeedError::Transport(e.to_string()))?;
        let status = response.status_code();
        let body = response
            .text()
            .await
            .map_err(|e| FeedError::Transport(e.to_string()))?;
        if status >= 400 {
            let message = serde_json::from_str::<ErrorBody>(&body)
                .ok()
                .and_then(|b| b.error)
                .unwrap_or_else(|| truncate(&body, 200, ""));
            return Err(FeedError::Status { status, message });
        }
        Ok(body)
    }

    pub async fn identity(&self) -> Result<Identity, FeedError> {
        let body = self.read("https://feed/identity").await?;
        serde_json::from_str(&body).map_err(|e| FeedError::Decode(format!("/identity: {e}")))
    }
}

impl FeedClient for ServiceFeed {
    async fn get(&self, path: &str, query: &[(&str, &str)]) -> Result<String, FeedError> {
        let mut url = format!("https://feed/api{path}");
        for (i, (key, value)) in query.iter().enumerate() {
            url.push(if i == 0 { '?' } else { '&' });
            url.push_str(&format!("{}={}", encode(key), encode(value)));
        }
        self.read(&url).await
    }
}
