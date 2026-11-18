//! The mailbox's SQLite storage (one Durable Object per agent).

use std::collections::BTreeMap;

use mailroom_logic::record::{Kind, NewRecord, Record, Src};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use worker::{Error, Result, SqlStorage, SqlStorageValue};

const CURSOR_KEY: &str = "cursor";

/// Creates the tables and index; safe to call on every request.
pub fn init(sql: &SqlStorage) -> Result<()> {
    sql.exec(
        "CREATE TABLE IF NOT EXISTS inbox (
            seq INTEGER PRIMARY KEY AUTOINCREMENT,
            id TEXT NOT NULL UNIQUE,
            ts_ingest TEXT NOT NULL, src TEXT NOT NULL, kind TEXT NOT NULL, ts TEXT NOT NULL,
            actor TEXT NOT NULL, ref TEXT NOT NULL, title TEXT NOT NULL, body TEXT NOT NULL,
            url TEXT NOT NULL, tags TEXT NOT NULL
        )",
        None,
    )?;
    sql.exec(
        "CREATE TABLE IF NOT EXISTS kv (key TEXT PRIMARY KEY, value TEXT NOT NULL)",
        None,
    )?;
    sql.exec("CREATE INDEX IF NOT EXISTS inbox_ts ON inbox(ts_ingest)", None)?;
    Ok(())
}

#[derive(Deserialize)]
struct Count {
    n: i64,
}

#[derive(Deserialize)]
struct SeqRow {
    seq: i64,
}

#[derive(Deserialize)]
struct ValueRow {
    value: String,
}

#[derive(Deserialize)]
struct TagCount {
    tag: String,
    n: i64,
}

/// A row of `inbox`, with the enum columns stored as the same strings serde uses on the wire.
#[derive(Deserialize)]
struct InboxRow {
    seq: i64,
    id: String,
    ts_ingest: String,
    src: Src,
    kind: Kind,
    ts: String,
    actor: String,
    #[serde(rename = "ref")]
    reference: String,
    title: String,
    body: String,
    url: String,
    tags: String,
}

impl InboxRow {
    fn into_record(self) -> Result<Record> {
        let tags: Vec<String> = serde_json::from_str(&self.tags)
            .map_err(|e| Error::RustError(format!("inbox row {}: tags: {e}", self.seq)))?;
        Ok(Record {
            seq: self.seq,
            ts_ingest: self.ts_ingest,
            new: NewRecord {
                src: self.src,
                kind: self.kind,
                id: self.id,
                ts: self.ts,
                actor: self.actor,
                reference: self.reference,
                title: self.title,
                body: self.body,
                url: self.url,
                tags,
            },
        })
    }
}

fn to_json<T: Serialize>(value: &T) -> Result<String> {
    serde_json::to_string(value).map_err(|e| Error::RustError(format!("serialize: {e}")))
}

/// The string serde uses for a unit-variant enum, e.g. `Kind::IssueComment` -> `issue_comment`.
fn enum_text<T: Serialize>(value: &T) -> Result<String> {
    let quoted = to_json(value)?;
    Ok(quoted.trim_matches('"').to_string())
}

/// Appends records (ignoring ids already present); returns the number of rows written.
pub fn append(sql: &SqlStorage, records: &[NewRecord], ts_ingest: &str) -> Result<usize> {
    let mut written = 0;
    for r in records {
        let bindings: Vec<SqlStorageValue> = vec![
            r.id.as_str().into(),
            ts_ingest.into(),
            enum_text(&r.src)?.into(),
            enum_text(&r.kind)?.into(),
            r.ts.as_str().into(),
            r.actor.as_str().into(),
            r.reference.as_str().into(),
            r.title.as_str().into(),
            r.body.as_str().into(),
            r.url.as_str().into(),
            to_json(&r.tags)?.into(),
        ];
        sql.exec(
            "INSERT OR IGNORE INTO inbox (id, ts_ingest, src, kind, ts, actor, ref, title, body, url, tags)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
            bindings,
        )?;
        let changed: Count = sql.exec("SELECT changes() AS n", None)?.one()?;
        written += changed.n as usize;
    }
    Ok(written)
}

/// Records with `seq > after` (any of `tags`, when given), oldest first; `bool` is "more exist".
pub fn read(sql: &SqlStorage, after: i64, limit: u32, tags: &[String]) -> Result<(Vec<Record>, bool)> {
    let mut query = String::from(
        "SELECT seq, id, ts_ingest, src, kind, ts, actor, ref, title, body, url, tags FROM inbox WHERE seq > ?",
    );
    let mut bindings: Vec<SqlStorageValue> = vec![after.into()];
    if !tags.is_empty() {
        let marks = vec!["?"; tags.len()].join(", ");
        query.push_str(&format!(
            " AND EXISTS (SELECT 1 FROM json_each(inbox.tags) WHERE value IN ({marks}))"
        ));
        bindings.extend(tags.iter().map(|t| SqlStorageValue::from(t.as_str())));
    }
    query.push_str(" ORDER BY seq LIMIT ?");
    bindings.push(i64::from(limit + 1).into());

    let rows: Vec<InboxRow> = sql.exec(&query, bindings)?.to_array()?;
    let more = rows.len() > limit as usize;
    let records = rows
        .into_iter()
        .take(limit as usize)
        .map(InboxRow::into_record)
        .collect::<Result<Vec<_>>>()?;
    Ok((records, more))
}

/// The acknowledged sequence number; 0 when unset.
pub fn cursor(sql: &SqlStorage) -> Result<i64> {
    Ok(get_json::<i64>(sql, CURSOR_KEY)?.unwrap_or(0))
}

pub fn set_cursor(sql: &SqlStorage, seq: i64) -> Result<()> {
    put_json(sql, CURSOR_KEY, &seq)
}

/// The highest sequence number ever assigned; unaffected by pruning, 0 when empty.
pub fn last_seq(sql: &SqlStorage) -> Result<i64> {
    let rows: Vec<SeqRow> = sql
        .exec("SELECT seq FROM sqlite_sequence WHERE name = 'inbox'", None)?
        .to_array()?;
    Ok(rows.first().map_or(0, |r| r.seq))
}

/// Records after the cursor.
pub fn unread(sql: &SqlStorage) -> Result<i64> {
    let after = cursor(sql)?;
    let row: Count = sql
        .exec(
            "SELECT COUNT(*) AS n FROM inbox WHERE seq > ?",
            vec![after.into()],
        )?
        .one()?;
    Ok(row.n)
}

/// Unread records per tag (a record with several tags counts under each).
pub fn unread_by_tag(sql: &SqlStorage) -> Result<BTreeMap<String, i64>> {
    let after = cursor(sql)?;
    let rows: Vec<TagCount> = sql
        .exec(
            "SELECT j.value AS tag, COUNT(*) AS n FROM inbox, json_each(inbox.tags) AS j
             WHERE inbox.seq > ? GROUP BY j.value",
            vec![after.into()],
        )?
        .to_array()?;
    Ok(rows.into_iter().map(|r| (r.tag, r.n)).collect())
}

pub fn get_json<T: DeserializeOwned>(sql: &SqlStorage, key: &str) -> Result<Option<T>> {
    let rows: Vec<ValueRow> = sql
        .exec("SELECT value FROM kv WHERE key = ?", vec![key.into()])?
        .to_array()?;
    match rows.first() {
        None => Ok(None),
        Some(row) => serde_json::from_str(&row.value)
            .map(Some)
            .map_err(|e| Error::RustError(format!("kv {key}: {e}"))),
    }
}

pub fn put_json<T: Serialize>(sql: &SqlStorage, key: &str, value: &T) -> Result<()> {
    sql.exec(
        "INSERT INTO kv (key, value) VALUES (?, ?)
         ON CONFLICT(key) DO UPDATE SET value = excluded.value",
        vec![key.into(), to_json(value)?.into()],
    )?;
    Ok(())
}

/// Deletes acknowledged records ingested before `cutoff_iso`.
pub fn prune(sql: &SqlStorage, cutoff_iso: &str) -> Result<()> {
    let after = cursor(sql)?;
    sql.exec(
        "DELETE FROM inbox WHERE seq <= ? AND ts_ingest < ?",
        vec![after.into(), cutoff_iso.into()],
    )?;
    Ok(())
}
