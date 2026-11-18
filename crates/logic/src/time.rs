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

/// How early a target may be polled. The cron fires about every 60 s with jitter; without
/// slack a 120 s target would be polled every 180 s.
pub const DUE_SLACK_SECS: i64 = 30;

/// Whether a target last attempted at `last_attempt` (epoch seconds, 0 = never) is due at `now`.
pub fn due(last_attempt: i64, every_secs: u32, now: i64) -> bool {
    last_attempt == 0 || now - last_attempt + DUE_SLACK_SECS >= i64::from(every_secs)
}
