//! mailroom-core: per-agent inboxes in SQLite Durable Objects, filled by a cron poller.

mod object;
mod service_feed;
mod store;

use futures_util::future::join_all;
use mailroom_logic::config::Config;
use serde::{Deserialize, Serialize};
use worker::wasm_bindgen::JsValue;
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

/// The door's `POST /signin` body.
#[derive(Deserialize)]
struct SigninIn {
    agent: String,
    code: String,
    redirect_uri: String,
}

/// What the core forwards to the broker's `POST /signin`.
#[derive(Serialize)]
struct SigninOut<'a> {
    code: &'a str,
    redirect_uri: &'a str,
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
async fn fetch(mut req: Request, env: Env, _ctx: Context) -> Result<Response> {
    let config = match load_config(&env) {
        Ok(c) => c,
        Err(message) => return error_response(&message, 500),
    };
    match req.path().as_str() {
        "/agents" if req.method() == Method::Get => Response::from_json(&AgentsBody {
            agents: config.agents.keys().map(String::as_str).collect(),
        }),
        "/signin/client" if req.method() == Method::Get => {
            let agent = req
                .url()?
                .query_pairs()
                .find(|(key, _)| key == "agent")
                .map(|(_, value)| value.into_owned())
                .unwrap_or_default();
            match signin_broker(&env, &config, &agent) {
                Ok((binding, fetcher)) => relay(&binding, fetcher, "https://feed/signin/client", None).await,
                Err(response) => response,
            }
        }
        "/signin" if req.method() == Method::Post => {
            let Ok(text) = req.text().await else {
                return error_response("bad request", 400);
            };
            let Ok(body) = serde_json::from_str::<SigninIn>(&text) else {
                return error_response("bad request", 400);
            };
            let out = serde_json::to_string(&SigninOut {
                code: &body.code,
                redirect_uri: &body.redirect_uri,
            })
            .map_err(|e| Error::RustError(e.to_string()))?;
            match signin_broker(&env, &config, &body.agent) {
                Ok((binding, fetcher)) => relay(&binding, fetcher, "https://feed/signin", Some(out)).await,
                Err(response) => response,
            }
        }
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

/// The broker `Fetcher` that signs `agent` in, with its binding name; the error is the response to send.
fn signin_broker(
    env: &Env,
    config: &Config,
    agent: &str,
) -> std::result::Result<(String, Fetcher), Result<Response>> {
    let Some(agent_config) = config.agents.get(agent) else {
        return Err(error_response("unknown mailbox", 404));
    };
    let binding = agent_config.signin.clone();
    match env.service(&binding) {
        Ok(fetcher) => Ok((binding, fetcher)),
        Err(_) => Err(error_response(
            &format!("mailroom config: agent {agent:?}: signin binding {binding} is not bound"),
            500,
        )),
    }
}

/// Sends the sign-in call to the broker (a POST when `body` is given) and relays a 200 body verbatim.
/// Anything else becomes a 502 carrying the broker's own error text.
async fn relay(binding: &str, fetcher: Fetcher, url: &str, body: Option<String>) -> Result<Response> {
    let init = body.map(|json| {
        let headers = Headers::new();
        let _ = headers.set("content-type", "application/json");
        let mut init = RequestInit::new();
        init.with_method(Method::Post)
            .with_headers(headers)
            .with_body(Some(JsValue::from_str(&json)));
        init
    });
    let mut answer = match fetcher.fetch(url, init).await {
        Ok(answer) => answer,
        Err(e) => return bad_gateway(&format!("{binding} could not be reached: {e}")),
    };
    let status = answer.status_code();
    let text = match answer.text().await {
        Ok(text) => text,
        Err(e) => return bad_gateway(&format!("{binding} could not be read: {e}")),
    };
    if status != 200 {
        let message =
            service_feed::error_message(&text).unwrap_or_else(|| format!("{binding} answered {status}"));
        return bad_gateway(&message);
    }
    let mut response = Response::ok(text)?;
    response.headers_mut().set("content-type", "application/json")?;
    Ok(response)
}

fn bad_gateway(message: &str) -> Result<Response> {
    error_response(&mailroom_logic::record::truncate(message, 200, ""), 502)
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
