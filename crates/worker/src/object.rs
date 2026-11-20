//! The per-agent Durable Object: inbox storage, the poller and the MCP tools.

use std::cell::Cell;

use mailroom_logic::claims::{self, ClaimsState};
use mailroom_logic::config::{AgentConfig, Config, SourceConfig};
use mailroom_logic::feed::{FeedClient, Identity, Poll};
use mailroom_logic::mcp::{
    self, checked_limit, AckArgs, AckResult, Mailbox as MailboxTools, MailboxInfo, McpReply, McpRequest,
    ReadMailArgs, ReadMailResult, SourceInfo, TargetInfo, ToolError,
};
use mailroom_logic::record::NewRecord;
use mailroom_logic::time::{self, iso};
use mailroom_logic::{discord, filter, github, VERSION};
use serde::{Deserialize, Serialize};
use worker::*;

use crate::service_feed::ServiceFeed;
use crate::store;
use crate::{agent_name, error_response, force_due, load_config};

const IDENTITY_TTL_SECS: i64 = 3600;
const USER_HEADER: &str = "x-mailroom-user";

#[durable_object]
pub struct Mailbox {
    state: State,
    env: Env,
    ticking: Cell<bool>,
}

impl DurableObject for Mailbox {
    fn new(state: State, env: Env) -> Self {
        Self {
            state,
            env,
            ticking: Cell::new(false),
        }
    }

    async fn fetch(&self, mut req: Request) -> Result<Response> {
        let config = match load_config(&self.env) {
            Ok(c) => c,
            Err(message) => return error_response(&message, 500),
        };
        let agent = agent_name(&req);
        let Some(agent_config) = config.agents.get(&agent) else {
            return error_response("unknown mailbox", 404);
        };
        let sql = self.state.storage().sql();
        store::init(&sql)?;

        match (req.method(), req.path().as_str()) {
            (Method::Post, "/tick") => {
                self.tick(&sql, &config, agent_config, &agent).await;
                Response::ok("ok")
            }
            (_, "/mcp") => {
                let user = req.headers().get(USER_HEADER)?.unwrap_or_default();
                let method = req.method().to_string();
                let protocol_version = req.headers().get("mcp-protocol-version")?;
                let mcp_method = req.headers().get("mcp-method")?;
                let body = req.text().await?;
                let view = View {
                    sql: &sql,
                    agent: &agent,
                    user: &user,
                    agent_config,
                };
                let reply = mcp::handle(
                    McpRequest {
                        http_method: &method,
                        protocol_version: protocol_version.as_deref(),
                        mcp_method: mcp_method.as_deref(),
                        body: &body,
                    },
                    &view,
                )
                .await;
                reply_response(reply)
            }
            _ => Response::error("not found", 404),
        }
    }
}

fn reply_response(reply: McpReply) -> Result<Response> {
    let headers = Headers::new();
    if let Some(allow) = reply.allow {
        headers.set("allow", allow)?;
    }
    let response = match reply.body {
        Some(body) => {
            let kind = if body.starts_with('{') {
                "application/json"
            } else {
                "text/plain"
            };
            headers.set("content-type", kind)?;
            Response::ok(body)?
        }
        None => Response::empty()?,
    };
    Ok(response.with_status(reply.status).with_headers(headers))
}

fn now_secs() -> i64 {
    (Date::now().as_millis() / 1000) as i64
}

fn elapsed_ms(started: u64) -> u64 {
    Date::now().as_millis().saturating_sub(started)
}

// ---------- stored shapes ----------

/// `status:<target key>`.
#[derive(Serialize, Deserialize, Default)]
pub struct TargetStatus {
    /// Epoch seconds of the last attempt; 0 when never attempted.
    pub last_attempt: i64,
    pub last_ok: Option<String>,
    pub last_error: Option<String>,
    pub last_error_at: Option<String>,
}

/// `identity:<broker>`.
#[derive(Serialize, Deserialize)]
struct CachedIdentity {
    identity: Identity,
    fetched_at: i64,
}

fn status_key(target: &str) -> String {
    format!("status:{target}")
}

fn state_key(target: &str) -> String {
    format!("state:{target}")
}

fn identity_key(broker: &str) -> String {
    format!("identity:{broker}")
}

// ---------- audit lines ----------

#[derive(Serialize)]
struct PollLog<'a> {
    event: &'static str,
    agent: &'a str,
    target: &'a str,
    ok: bool,
    records: usize,
    kept: usize,
    ms: u64,
    error: Option<&'a str>,
}

#[derive(Serialize)]
struct ToolLog<'a> {
    event: &'static str,
    tool: &'a str,
    agent: &'a str,
    user: &'a str,
    ok: bool,
    ms: u64,
}

fn log_line<T: Serialize>(line: &T) {
    if let Ok(text) = serde_json::to_string(line) {
        console_log!("{text}");
    }
}

// ---------- the poller ----------

/// Clears the in-flight flag when the tick ends, however it ends.
struct TickGuard<'a>(&'a Cell<bool>);

impl Drop for TickGuard<'_> {
    fn drop(&mut self) {
        self.0.set(false);
    }
}

/// What a successful poll did: records seen, records kept.
struct Committed {
    records: usize,
    kept: usize,
}

/// Filters, appends and records the new state and status. Deliberately synchronous, so
/// the three writes land together.
fn commit<S: Serialize>(
    sql: &SqlStorage,
    target: &str,
    source: &SourceConfig,
    poll: Poll<S>,
    now: i64,
) -> Result<Committed> {
    let records = poll.records.len();
    let kept: Vec<NewRecord> = filter::apply(poll.records, source.rules(), source.keep());
    let stamp = iso(now);
    store::append(sql, &kept, &stamp)?;
    store::put_json(sql, &state_key(target), &poll.state)?;
    store::put_json(
        sql,
        &status_key(target),
        &TargetStatus {
            last_attempt: now,
            last_ok: Some(stamp),
            last_error: None,
            last_error_at: None,
        },
    )?;
    Ok(Committed {
        records,
        kept: kept.len(),
    })
}

fn record_error(sql: &SqlStorage, target: &str, now: i64, message: &str) -> Result<()> {
    let previous: TargetStatus = store::get_json(sql, &status_key(target))?.unwrap_or_default();
    store::put_json(
        sql,
        &status_key(target),
        &TargetStatus {
            last_attempt: now,
            last_ok: previous.last_ok,
            last_error: Some(message.to_string()),
            last_error_at: Some(iso(now)),
        },
    )
}

fn is_due(sql: &SqlStorage, target: &str, every_secs: u32, now: i64, force: bool) -> Result<bool> {
    if force {
        return Ok(true);
    }
    let status: Option<TargetStatus> = store::get_json(sql, &status_key(target))?;
    Ok(match status {
        None => true,
        Some(s) => time::due(s.last_attempt, every_secs, now),
    })
}

impl Mailbox {
    async fn identity(
        &self,
        sql: &SqlStorage,
        broker: &str,
        feed: &ServiceFeed,
        now: i64,
    ) -> Result<Identity> {
        let cached: Option<CachedIdentity> = store::get_json(sql, &identity_key(broker))?;
        if let Some(c) = cached {
            if now - c.fetched_at < IDENTITY_TTL_SECS {
                return Ok(c.identity);
            }
        }
        let identity = feed
            .identity()
            .await
            .map_err(|e| Error::RustError(e.to_string()))?;
        store::put_json(
            sql,
            &identity_key(broker),
            &CachedIdentity {
                identity: identity.clone(),
                fetched_at: now,
            },
        )?;
        Ok(identity)
    }

    async fn tick(&self, sql: &SqlStorage, config: &Config, agent_config: &AgentConfig, agent: &str) {
        if self.ticking.replace(true) {
            return;
        }
        let _guard = TickGuard(&self.ticking);
        if let Err(e) = self.run_tick(sql, config, agent_config, agent).await {
            console_error!("tick {agent}: {e}");
        }
    }

    async fn run_tick(
        &self,
        sql: &SqlStorage,
        config: &Config,
        agent_config: &AgentConfig,
        agent: &str,
    ) -> Result<()> {
        let force = force_due(&self.env);
        // One timestamp for the whole tick: it is every target's `last_attempt`, so the next
        // tick's due check measures from the same point in the cron cycle.
        let now = now_secs();
        for source in &agent_config.sources {
            let keys = source.target_keys();
            let mut due = Vec::new();
            for key in &keys {
                if is_due(sql, key, source.every_secs(), now, force)? {
                    due.push(key.as_str());
                }
            }
            if due.is_empty() {
                continue;
            }

            let broker = source.broker();
            let feed = match self.env.service(broker) {
                Ok(fetcher) => ServiceFeed(fetcher),
                Err(e) => {
                    self.fail_all(sql, agent, &due, now, &format!("service binding {broker}: {e}"))?;
                    continue;
                }
            };
            let me = match self.identity(sql, broker, &feed, now).await {
                Ok(me) => me,
                Err(e) => {
                    self.fail_all(sql, agent, &due, now, &format!("identity: {e}"))?;
                    continue;
                }
            };

            // Targets are polled one after another so a slow broker is not hit in parallel.
            for (index, key) in keys.iter().enumerate() {
                if !due.contains(&key.as_str()) {
                    continue;
                }
                let started = Date::now().as_millis();
                let outcome = self.poll_target(sql, &feed, &me, source, index, key, now).await;
                let ms = elapsed_ms(started);
                match outcome {
                    Ok(done) => log_line(&PollLog {
                        event: "poll",
                        agent,
                        target: key,
                        ok: true,
                        records: done.records,
                        kept: done.kept,
                        ms,
                        error: None,
                    }),
                    Err(message) => {
                        record_error(sql, key, now, &message)?;
                        log_line(&PollLog {
                            event: "poll",
                            agent,
                            target: key,
                            ok: false,
                            records: 0,
                            kept: 0,
                            ms,
                            error: Some(&message),
                        });
                    }
                }
            }
        }
        if config.retain_days > 0 {
            let cutoff = now_secs() - i64::from(config.retain_days) * 86_400;
            store::prune(sql, &iso(cutoff))?;
        }
        Ok(())
    }

    fn fail_all(&self, sql: &SqlStorage, agent: &str, due: &[&str], now: i64, message: &str) -> Result<()> {
        for target in due {
            record_error(sql, target, now, message)?;
            log_line(&PollLog {
                event: "poll",
                agent,
                target,
                ok: false,
                records: 0,
                kept: 0,
                ms: 0,
                error: Some(message),
            });
        }
        Ok(())
    }

    /// Polls one target and commits the result; the error is the text to record.
    #[allow(clippy::too_many_arguments)]
    async fn poll_target(
        &self,
        sql: &SqlStorage,
        feed: &impl FeedClient,
        me: &Identity,
        source: &SourceConfig,
        index: usize,
        key: &str,
        now: i64,
    ) -> std::result::Result<Committed, String> {
        let prev = |e: Error| e.to_string();
        match source {
            SourceConfig::Discord(d) => {
                let previous = store::get_json(sql, &state_key(key)).map_err(prev)?;
                let poll = discord::poll_channel(feed, me, &d.channels[index], d, previous, &iso(now))
                    .await
                    .map_err(|e| e.to_string())?;
                commit(sql, key, source, poll, now).map_err(prev)
            }
            SourceConfig::Github(g) => {
                // Dispatch on the key before indexing: the labels key sits after the repos.
                if Some(key) == g.labels_key().as_deref() {
                    let previous = store::get_json(sql, &state_key(key)).map_err(prev)?;
                    let poll = claims::poll_claims(feed, me, g, previous, now)
                        .await
                        .map_err(|e| e.to_string())?;
                    return commit(sql, key, source, poll, now).map_err(prev);
                }
                let previous = store::get_json(sql, &state_key(key)).map_err(prev)?;
                let repo = &g.repos[index];
                // Absent or undecodable claims mean none: the labels target may not have run yet.
                let claimed = g
                    .labels_key()
                    .and_then(|k| store::get_json::<ClaimsState>(sql, &state_key(&k)).ok().flatten())
                    .map(|c| c.claimed_numbers(repo))
                    .unwrap_or_default();
                let poll = github::poll_repo(feed, me, repo, g, previous, now, &claimed)
                    .await
                    .map_err(|e| e.to_string())?;
                commit(sql, key, source, poll, now).map_err(prev)
            }
        }
    }
}

// ---------- the MCP tools ----------

struct View<'a> {
    sql: &'a SqlStorage,
    agent: &'a str,
    user: &'a str,
    agent_config: &'a AgentConfig,
}

fn failed(e: Error) -> ToolError {
    ToolError::Failed(e.to_string())
}

impl View<'_> {
    fn audit<T>(&self, tool: &str, started: u64, result: &std::result::Result<T, ToolError>) {
        log_line(&ToolLog {
            event: "tool_call",
            tool,
            agent: self.agent,
            user: self.user,
            ok: result.is_ok(),
            ms: elapsed_ms(started),
        });
    }
}

impl MailboxTools for View<'_> {
    async fn read_mail(&self, args: ReadMailArgs) -> std::result::Result<ReadMailResult, ToolError> {
        let started = Date::now().as_millis();
        let result = (|| {
            let limit = checked_limit(&args)?;
            let cursor = store::cursor(self.sql).map_err(failed)?;
            let after = args.after.unwrap_or(cursor);
            if after < 0 {
                return Err(ToolError::Input("after must be 0 or greater".into()));
            }
            let tags = args.tags.clone().unwrap_or_default();
            let (records, more) = store::read(self.sql, after, limit, &tags).map_err(failed)?;
            Ok(ReadMailResult {
                agent: self.agent.to_string(),
                cursor,
                last_seq: store::last_seq(self.sql).map_err(failed)?,
                unread: store::unread(self.sql).map_err(failed)?,
                returned: records.len(),
                more,
                records,
            })
        })();
        self.audit("read_mail", started, &result);
        result
    }

    async fn ack(&self, args: AckArgs) -> std::result::Result<AckResult, ToolError> {
        let started = Date::now().as_millis();
        let result = (|| {
            let cursor = store::cursor(self.sql).map_err(failed)?;
            let last_seq = store::last_seq(self.sql).map_err(failed)?;
            if args.seq < cursor || args.seq > last_seq {
                return Err(ToolError::Input(format!(
                    "seq must be between {cursor} and {last_seq}"
                )));
            }
            store::set_cursor(self.sql, args.seq).map_err(failed)?;
            Ok(AckResult {
                cursor: args.seq,
                unread: store::unread(self.sql).map_err(failed)?,
            })
        })();
        self.audit("ack", started, &result);
        result
    }

    async fn info(&self) -> std::result::Result<MailboxInfo, ToolError> {
        let started = Date::now().as_millis();
        let result = (|| {
            let mut sources = Vec::new();
            for source in &self.agent_config.sources {
                let mut targets = Vec::new();
                for key in source.target_keys() {
                    let status: TargetStatus = store::get_json(self.sql, &status_key(&key))
                        .map_err(failed)?
                        .unwrap_or_default();
                    let next_due = (status.last_attempt != 0)
                        .then(|| iso(status.last_attempt + i64::from(source.every_secs())));
                    targets.push(TargetInfo {
                        target: key,
                        last_ok: status.last_ok,
                        last_error: status.last_error,
                        last_error_at: status.last_error_at,
                        next_due,
                    });
                }
                sources.push(SourceInfo {
                    src: source.src(),
                    broker: source.broker().to_string(),
                    every_secs: source.every_secs(),
                    keep: source.keep(),
                    targets,
                });
            }
            Ok(MailboxInfo {
                agent: self.agent.to_string(),
                signed_in_as: self.user.to_string(),
                version: VERSION,
                cursor: store::cursor(self.sql).map_err(failed)?,
                last_seq: store::last_seq(self.sql).map_err(failed)?,
                unread: store::unread(self.sql).map_err(failed)?,
                unread_by_tag: store::unread_by_tag(self.sql).map_err(failed)?,
                sources,
            })
        })();
        self.audit("mailbox_info", started, &result);
        result
    }
}
