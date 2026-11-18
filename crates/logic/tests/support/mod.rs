//! A fake `FeedClient` that knows nothing about any upstream service.
#![allow(dead_code)]

use std::cell::RefCell;
use std::collections::BTreeMap;

use mailroom_logic::feed::{FeedClient, FeedError};

enum Reply {
    Body(String),
    Status(u16),
}

#[derive(Default)]
pub struct FakeFeed {
    replies: RefCell<BTreeMap<String, Reply>>,
    calls: RefCell<Vec<String>>,
}

impl FakeFeed {
    pub fn new() -> Self {
        Self::default()
    }

    /// Registers a body for `path?k=v&k=v` (or just `path`).
    pub fn on(&self, path_and_query: &str, body: &str) {
        self.replies
            .borrow_mut()
            .insert(path_and_query.to_string(), Reply::Body(body.to_string()));
    }

    pub fn fail(&self, path_and_query: &str, status: u16) {
        self.replies
            .borrow_mut()
            .insert(path_and_query.to_string(), Reply::Status(status));
    }

    pub fn calls(&self) -> Vec<String> {
        self.calls.borrow().clone()
    }
}

impl FeedClient for FakeFeed {
    async fn get(&self, path: &str, query: &[(&str, &str)]) -> Result<String, FeedError> {
        let mut key = path.to_string();
        for (i, (k, v)) in query.iter().enumerate() {
            key.push(if i == 0 { '?' } else { '&' });
            key.push_str(&format!("{k}={v}"));
        }
        self.calls.borrow_mut().push(key.clone());
        match self.replies.borrow().get(&key) {
            Some(Reply::Body(b)) => Ok(b.clone()),
            Some(Reply::Status(status)) => Err(FeedError::Status {
                status: *status,
                message: format!("fake failure for {key}"),
            }),
            None => Err(FeedError::Status {
                status: 404,
                message: format!("no fake registered for {key}"),
            }),
        }
    }
}
