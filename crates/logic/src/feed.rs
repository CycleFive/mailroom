//! The upstream-read abstraction shared by the pollers.

use std::fmt;

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

use crate::record::NewRecord;

/// Who the bot is on the upstream service.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Identity {
    pub id: String,
    pub login: String,
    pub name: String,
}

#[derive(Debug, Clone, PartialEq)]
pub enum FeedError {
    Status { status: u16, message: String },
    Transport(String),
    Decode(String),
}

impl fmt::Display for FeedError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            FeedError::Status { status, message } => write!(f, "upstream status {status}: {message}"),
            FeedError::Transport(m) => write!(f, "transport error: {m}"),
            FeedError::Decode(m) => write!(f, "decode error: {m}"),
        }
    }
}

impl std::error::Error for FeedError {}

#[allow(async_fn_in_trait)]
pub trait FeedClient {
    /// GET an upstream API path (e.g. "/channels/1/messages"); returns the response body text.
    async fn get(&self, path: &str, query: &[(&str, &str)]) -> Result<String, FeedError>;
}

/// GETs `path` and deserializes the body; a body that is not valid JSON for `T` is a
/// `FeedError::Decode` naming the path.
pub async fn get_json<T: DeserializeOwned>(
    feed: &impl FeedClient,
    path: &str,
    query: &[(&str, &str)],
) -> Result<T, FeedError> {
    let body = feed.get(path, query).await?;
    serde_json::from_str(&body).map_err(|e| FeedError::Decode(format!("{path}: {e}")))
}

/// The result of one poll: records to append and the state to persist (only on `Ok`).
pub struct Poll<S> {
    pub records: Vec<NewRecord>,
    pub state: S,
}

#[derive(Debug)]
pub struct PollError(pub String);

impl fmt::Display for PollError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for PollError {}

impl From<FeedError> for PollError {
    fn from(e: FeedError) -> Self {
        PollError(e.to_string())
    }
}
