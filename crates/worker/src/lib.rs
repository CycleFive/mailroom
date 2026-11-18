//! mailroom-core: per-agent inboxes in SQLite Durable Objects, filled by a cron poller.

mod object;
mod service_feed;
mod store;

use futures_util::future::join_all;
use mailroom_logic::config::Config;
use serde::Serialize;
use worker::*;

pub(crate) const AGENT_HEADER: &str = "x-mailroom-agent";
const OBJECT_BINDING: &str = "MAILBOX";

#[derive(Serialize)]
struct ErrorBody<'a> {
    error: &'a str,
}

#[derive(Serialize)]
struct AgentsBody<'a> {
    agents: Vec<&'a str>,
}

pub(crate) fn error_response(message: &str, status: u16) -> Result<Response> {
    Ok(Response::from_json(&ErrorBody { error: message })?.with_status(status))
}

/// The validated `MAILROOM` var; the error is the message for the 500 body.
pub(crate) fn load_config(env: &Env) -> std::result::Result<Config, String> {
    let config: Config = env
        .object_var("MAILROOM")
        .map_err(|e| format!("mailroom config: {e}"))?;
    config.validate().map_err(|e| format!("mailroom config: {e}"))?;
    Ok(config)
}

pub(crate) fn agent_name(req: &Request) -> String {
    req.headers().get(AGENT_HEADER).ok().flatten().unwrap_or_default()
}

/// Test-only switch: every target is due on every tick.
pub(crate) fn force_due(env: &Env) -> bool {
    env.var("MAILROOM_FORCE_DUE")
        .is_ok_and(|v| v.to_string() == "true")
}

#[event(fetch)]
async fn fetch(req: Request, env: Env, _ctx: Context) -> Result<Response> {
    let config = match load_config(&env) {
        Ok(c) => c,
        Err(message) => return error_response(&message, 500),
    };
    match req.path().as_str() {
        "/agents" if req.method() == Method::Get => Response::from_json(&AgentsBody {
            agents: config.agents.keys().map(String::as_str).collect(),
        }),
        "/mcp" => {
            let agent = agent_name(&req);
            if !config.agents.contains_key(&agent) {
                return error_response("unknown mailbox", 404);
            }
            env.durable_object(OBJECT_BINDING)?
                .get_by_name(&agent)?
                .fetch_with_request(req)
                .await
        }
        _ => Response::error("not found", 404),
    }
}

async fn tick_agent(env: &Env, agent: &str) -> Result<()> {
    let headers = Headers::new();
    headers.set(AGENT_HEADER, agent)?;
    let mut init = RequestInit::new();
    init.with_method(Method::Post).with_headers(headers);
    let request = Request::new_with_init("https://mailbox/tick", &init)?;
    env.durable_object(OBJECT_BINDING)?
        .get_by_name(agent)?
        .fetch_with_request(request)
        .await?;
    Ok(())
}

#[event(scheduled)]
async fn scheduled(_event: ScheduledEvent, env: Env, _ctx: ScheduleContext) {
    let config = match load_config(&env) {
        Ok(c) => c,
        Err(message) => {
            console_error!("{message}");
            return;
        }
    };
    let ticks = config.agents.keys().map(|agent| {
        let env = &env;
        async move {
            if let Err(e) = tick_agent(env, agent).await {
                console_error!("tick {agent}: {e}");
            }
        }
    });
    join_all(ticks).await;
}
