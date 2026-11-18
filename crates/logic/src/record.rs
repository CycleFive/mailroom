//! Inbox record types.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Src {
    Discord,
    Github,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    Message,
    Reaction,
    Gap,
    Issue,
    Pr,
    IssueComment,
    ReviewComment,
    Review,
    Event,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct NewRecord {
    pub src: Src,
    pub kind: Kind,
    pub id: String,
    pub ts: String,
    pub actor: String,
    #[serde(rename = "ref")]
    pub reference: String,
    pub title: String,
    pub body: String,
    pub url: String,
    pub tags: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Record {
    pub seq: i64,
    pub ts_ingest: String,
    #[serde(flatten)]
    pub new: NewRecord,
}

/// At most `max` chars; when cut, `suffix` is appended after the cut.
pub fn truncate(text: &str, max: usize, suffix: &str) -> String {
    match text.char_indices().nth(max) {
        None => text.to_string(),
        Some((byte, _)) => format!("{}{}", &text[..byte], suffix),
    }
}
