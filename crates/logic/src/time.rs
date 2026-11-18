//! ISO-8601 UTC helpers.

use chrono::{DateTime, Utc};

/// Formats epoch seconds as `2026-11-18T10:05:00Z`. Out-of-range input yields the Unix epoch.
pub fn iso(epoch_secs: i64) -> String {
    DateTime::<Utc>::from_timestamp(epoch_secs, 0)
        .unwrap_or_default()
        .format("%Y-%m-%dT%H:%M:%SZ")
        .to_string()
}

/// Parses an RFC 3339 timestamp (`Z` or numeric offset, optional fractions) to epoch seconds.
pub fn epoch(iso: &str) -> Option<i64> {
    DateTime::parse_from_rfc3339(iso).ok().map(|d| d.timestamp())
}
