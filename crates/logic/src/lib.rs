//! Pure-Rust logic for mailroom: records, time helpers and configuration.

pub mod claims;
pub mod config;
pub mod discord;
pub mod feed;
pub mod filter;
pub mod github;
pub mod mcp;
pub mod record;
pub mod time;

pub const VERSION: &str = env!("CARGO_PKG_VERSION");
