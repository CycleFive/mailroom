//! A stateless MCP server over Streamable HTTP, as a pure function from an
//! HTTP-level request to a reply. A faithful port of the transport used by the
//! sibling broker servers: no sessions, no SSE, one JSON answer per POST.
//!
//! Eras: 2026-07-28 has no handshake (`server/discover`, version in
//! `params._meta` and the `MCP-Protocol-Version` header, results carry
//! `resultType` and server info in `_meta`); the three legacy revisions use
//! `initialize`.

use std::collections::BTreeMap;

use schemars::{schema_for, JsonSchema, Schema};
use serde::de::IgnoredAny;
use serde::{Deserialize, Deserializer, Serialize};
use serde_json::value::RawValue;

use crate::config::Keep;
use crate::record::{Record, Src};
use crate::VERSION;

pub const MODERN_VERSION: &str = "2026-07-28";
pub const LEGACY_VERSIONS: [&str; 3] = ["2025-11-25", "2025-06-18", "2025-03-26"];
const SUPPORTED_VERSIONS: [&str; 4] = [
    MODERN_VERSION,
    LEGACY_VERSIONS[0],
    LEGACY_VERSIONS[1],
    LEGACY_VERSIONS[2],
];
const LIST_TTL_MS: u64 = 5 * 60 * 1000;
const DEFAULT_LIMIT: u32 = 50;

pub const INSTRUCTIONS: &str = "This is your mailbox. A poller collects Discord and GitHub activity here so you do not have to go looking. On waking, call read_mail. If it is empty, end quietly. Otherwise handle each record: records are pointers, so fetch the thread or diff through the Discord or GitHub broker before acting, and act there. Then call ack with the highest seq you actually handled; leave unhandled mail unacknowledged. Tags starting routed: are addressed to you; mine is activity on your own work; untagged records are context.";

const ERR_PARSE: i32 = -32700;
const ERR_INVALID_REQUEST: i32 = -32600;
const ERR_METHOD_NOT_FOUND: i32 = -32601;
const ERR_INVALID_PARAMS: i32 = -32602;
const ERR_HEADER_MISMATCH: i32 = -32020;
const ERR_UNSUPPORTED_VERSION: i32 = -32022;

// ---------- tool surface ----------

#[derive(Debug, Default, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ReadMailArgs {
    pub limit: Option<u32>,
    pub after: Option<i64>,
    pub tags: Option<Vec<String>>,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct AckArgs {
    pub seq: i64,
}

#[derive(Debug, Serialize, PartialEq)]
pub struct ReadMailResult {
    pub agent: String,
    pub cursor: i64,
    pub last_seq: i64,
    pub unread: i64,
    pub returned: usize,
    pub more: bool,
    pub records: Vec<Record>,
}

#[derive(Debug, Serialize, PartialEq)]
pub struct AckResult {
    pub cursor: i64,
    pub unread: i64,
}

#[derive(Debug, Serialize, PartialEq)]
pub struct MailboxInfo {
    pub agent: String,
    pub signed_in_as: String,
    pub version: &'static str,
    pub cursor: i64,
    pub last_seq: i64,
    pub unread: i64,
    pub unread_by_tag: BTreeMap<String, i64>,
    pub sources: Vec<SourceInfo>,
}

#[derive(Debug, Serialize, PartialEq)]
pub struct SourceInfo {
    #[serde(rename = "type")]
    pub src: Src,
    pub broker: String,
    pub every_secs: u32,
    pub keep: Keep,
    pub targets: Vec<TargetInfo>,
}

#[derive(Debug, Serialize, PartialEq)]
pub struct TargetInfo {
    pub target: String,
    pub last_ok: Option<String>,
    pub last_error: Option<String>,
    pub last_error_at: Option<String>,
    pub next_due: Option<String>,
}

/// Both variants become a tool result with `isError: true`.
#[derive(Debug, PartialEq)]
pub enum ToolError {
    Input(String),
    Failed(String),
}

#[allow(async_fn_in_trait)]
pub trait Mailbox {
    async fn read_mail(&self, args: ReadMailArgs) -> Result<ReadMailResult, ToolError>;
    async fn ack(&self, args: AckArgs) -> Result<AckResult, ToolError>;
    async fn info(&self) -> Result<MailboxInfo, ToolError>;
}

/// The effective `limit` for `read_mail`: 1-200, default 50.
pub fn checked_limit(args: &ReadMailArgs) -> Result<u32, ToolError> {
    match args.limit {
        None => Ok(DEFAULT_LIMIT),
        Some(n) if (1..=200).contains(&n) => Ok(n),
        Some(_) => Err(ToolError::Input("limit must be between 1 and 200".into())),
    }
}

// ---------- request / reply ----------

pub struct McpRequest<'a> {
    pub http_method: &'a str,
    pub protocol_version: Option<&'a str>,
    pub mcp_method: Option<&'a str>,
    pub body: &'a str,
}

#[derive(Debug, PartialEq)]
pub struct McpReply {
    pub status: u16,
    pub body: Option<String>,
    pub allow: Option<&'static str>,
}

// ---------- inbound ----------

/// A JSON-RPC id: string, number or an explicit null.
#[derive(Debug, Serialize, Deserialize)]
#[serde(untagged)]
enum Id {
    Null,
    Str(String),
    Num(serde_json::Number),
}

/// Present-but-null stays `Some(Id::Null)`; only a missing key is `None`.
fn present_id<'de, D: Deserializer<'de>>(d: D) -> Result<Option<Id>, D::Error> {
    Id::deserialize(d).map(Some)
}

/// A value of the wanted type, or `None` when the key holds anything else.
fn lenient<'de, D: Deserializer<'de>, T: Deserialize<'de>>(d: D) -> Result<Option<T>, D::Error> {
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum Lenient<T> {
        Is(T),
        Other(IgnoredAny),
    }
    Ok(match Option::<Lenient<T>>::deserialize(d)? {
        Some(Lenient::Is(v)) => Some(v),
        _ => None,
    })
}

#[derive(Deserialize)]
struct Envelope {
    #[serde(default, deserialize_with = "lenient")]
    jsonrpc: Option<String>,
    #[serde(default, deserialize_with = "present_id")]
    id: Option<Id>,
    #[serde(default, deserialize_with = "lenient")]
    method: Option<String>,
    params: Option<Box<RawValue>>,
}

#[derive(Deserialize, Default)]
struct Params {
    #[serde(default, rename = "_meta", deserialize_with = "lenient")]
    meta: Option<ParamsMeta>,
    #[serde(default, rename = "protocolVersion", deserialize_with = "lenient")]
    protocol_version: Option<String>,
    #[serde(default, deserialize_with = "lenient")]
    name: Option<String>,
    arguments: Option<Box<RawValue>>,
}

#[derive(Deserialize, Default)]
struct ParamsMeta {
    #[serde(
        default,
        rename = "io.modelcontextprotocol/protocolVersion",
        deserialize_with = "lenient"
    )]
    version: Option<String>,
}

// ---------- outbound ----------

#[derive(Serialize)]
struct Response<'a, R: Serialize> {
    jsonrpc: &'static str,
    id: &'a Id,
    #[serde(skip_serializing_if = "Option::is_none")]
    result: Option<R>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<RpcError<'a>>,
}

#[derive(Serialize)]
struct RpcError<'a> {
    code: i32,
    message: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    data: Option<UnsupportedData<'a>>,
}

#[derive(Serialize)]
struct UnsupportedData<'a> {
    supported: [&'static str; 4],
    requested: &'a str,
}

#[derive(Serialize)]
struct ServerInfo {
    name: &'static str,
    title: &'static str,
    version: &'static str,
}

const SERVER_INFO: ServerInfo = ServerInfo {
    name: "mailroom",
    title: "Mailroom",
    version: VERSION,
};

#[derive(Serialize)]
struct Capabilities {
    tools: ToolsCapability,
}

#[derive(Serialize)]
struct ToolsCapability {
    #[serde(rename = "listChanged")]
    list_changed: bool,
}

const CAPABILITIES: Capabilities = Capabilities {
    tools: ToolsCapability { list_changed: false },
};

/// Fields every 2026-07-28 result carries; absent in the legacy era.
#[derive(Serialize)]
struct ModernHead {
    #[serde(rename = "resultType")]
    result_type: &'static str,
    #[serde(rename = "_meta")]
    meta: ModernMeta,
}

#[derive(Serialize)]
struct ModernMeta {
    #[serde(rename = "io.modelcontextprotocol/serverInfo")]
    server_info: ServerInfo,
}

fn head(modern: bool) -> Option<ModernHead> {
    modern.then_some(ModernHead {
        result_type: "complete",
        meta: ModernMeta {
            server_info: SERVER_INFO,
        },
    })
}

#[derive(Serialize)]
struct CacheHints {
    #[serde(rename = "ttlMs")]
    ttl_ms: u64,
    #[serde(rename = "cacheScope")]
    cache_scope: &'static str,
}

const CACHE_HINTS: CacheHints = CacheHints {
    ttl_ms: LIST_TTL_MS,
    cache_scope: "private",
};

#[derive(Serialize)]
struct InitializeResult<'a> {
    #[serde(rename = "protocolVersion")]
    protocol_version: &'a str,
    capabilities: Capabilities,
    #[serde(rename = "serverInfo")]
    server_info: ServerInfo,
    instructions: &'static str,
}

#[derive(Serialize)]
struct DiscoverResult {
    #[serde(flatten)]
    head: Option<ModernHead>,
    #[serde(rename = "supportedVersions")]
    supported_versions: [&'static str; 4],
    capabilities: Capabilities,
    instructions: &'static str,
    #[serde(flatten)]
    cache: CacheHints,
}

#[derive(Serialize)]
struct EmptyResult {
    #[serde(flatten)]
    head: Option<ModernHead>,
}

#[derive(Serialize)]
struct ListResult {
    #[serde(flatten)]
    head: Option<ModernHead>,
    tools: Vec<ToolDescriptor>,
    #[serde(flatten)]
    cache: Option<CacheHints>,
}

#[derive(Serialize)]
struct ToolDescriptor {
    name: &'static str,
    title: &'static str,
    description: &'static str,
    #[serde(rename = "inputSchema")]
    input_schema: Schema,
    annotations: Annotations,
}

#[derive(Serialize)]
struct Annotations {
    #[serde(rename = "readOnlyHint")]
    read_only: bool,
    #[serde(rename = "idempotentHint", skip_serializing_if = "Option::is_none")]
    idempotent: Option<bool>,
    #[serde(rename = "openWorldHint")]
    open_world: bool,
}

#[derive(Serialize)]
struct CallResult {
    #[serde(flatten)]
    head: Option<ModernHead>,
    content: Vec<TextContent>,
    #[serde(rename = "isError")]
    is_error: bool,
}

#[derive(Serialize)]
struct TextContent {
    #[serde(rename = "type")]
    kind: &'static str,
    text: String,
}

fn tool_descriptors() -> Vec<ToolDescriptor> {
    vec![
        ToolDescriptor {
            name: "read_mail",
            title: "Read mail",
            description: "Records past your cursor, oldest first. Does not move the cursor. `limit` 1-200 (default 50). `after` reads from another seq instead. `tags` keeps records carrying any of them.",
            input_schema: schema_for!(ReadMailArgs),
            annotations: Annotations { read_only: true, idempotent: None, open_world: false },
        },
        ToolDescriptor {
            name: "ack",
            title: "Acknowledge mail",
            description: "Move your cursor to `seq`: everything up to it is handled. Only acknowledge mail you actually dealt with.",
            input_schema: schema_for!(AckArgs),
            annotations: Annotations { read_only: false, idempotent: Some(true), open_world: false },
        },
        ToolDescriptor {
            name: "mailbox_info",
            title: "Mailbox status",
            description: "Which mailbox this is, unread counts, and when each source was last polled or failed.",
            input_schema: schema_for!(NoArgs),
            annotations: Annotations { read_only: true, idempotent: None, open_world: false },
        },
    ]
}

/// `mailbox_info` takes no arguments.
#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct NoArgs {}

// ---------- reply construction ----------

fn reply(status: u16, body: String) -> McpReply {
    McpReply {
        status,
        body: Some(body),
        allow: None,
    }
}

fn encode<T: Serialize>(value: &T) -> String {
    serde_json::to_string(value).unwrap_or_else(|_| {
        r#"{"jsonrpc":"2.0","id":null,"error":{"code":-32603,"message":"Internal error"}}"#.to_string()
    })
}

fn rpc_error(id: &Id, code: i32, message: &str, status: u16) -> McpReply {
    rpc_error_data(id, code, message, None, status)
}

fn rpc_error_data(
    id: &Id,
    code: i32,
    message: &str,
    data: Option<UnsupportedData<'_>>,
    status: u16,
) -> McpReply {
    reply(
        status,
        encode(&Response::<()> {
            jsonrpc: "2.0",
            id,
            result: None,
            error: Some(RpcError { code, message, data }),
        }),
    )
}

fn ok<R: Serialize>(id: &Id, result: R) -> McpReply {
    reply(
        200,
        encode(&Response {
            jsonrpc: "2.0",
            id,
            result: Some(result),
            error: None,
        }),
    )
}

// ---------- the transport ----------

fn is_newer(v: &str) -> bool {
    // Revision ids are dates, so string order is chronological.
    v >= MODERN_VERSION
}

pub async fn handle(req: McpRequest<'_>, mailbox: &impl Mailbox) -> McpReply {
    if req.http_method != "POST" {
        return McpReply {
            status: 405,
            body: Some("Method Not Allowed".into()),
            allow: Some("POST"),
        };
    }

    if req.body.trim_start().starts_with('[') {
        // Valid JSON batches are refused; broken ones are parse errors.
        return match serde_json::from_str::<Box<RawValue>>(req.body) {
            Ok(_) => rpc_error(
                &Id::Null,
                ERR_INVALID_REQUEST,
                "Batch requests are not supported",
                400,
            ),
            Err(_) => rpc_error(&Id::Null, ERR_PARSE, "Parse error", 400),
        };
    }
    let msg: Envelope = match serde_json::from_str(req.body) {
        Ok(m) => m,
        Err(e) if e.is_data() => return rpc_error(&Id::Null, ERR_INVALID_REQUEST, "Invalid Request", 400),
        Err(_) => return rpc_error(&Id::Null, ERR_PARSE, "Parse error", 400),
    };
    if msg.jsonrpc.as_deref() != Some("2.0") {
        return rpc_error(&Id::Null, ERR_INVALID_REQUEST, "Invalid Request", 400);
    }

    // Responses from the client and notifications need no answer.
    let (Some(method), Some(id)) = (msg.method.as_deref(), msg.id.as_ref()) else {
        return McpReply {
            status: 202,
            body: None,
            allow: None,
        };
    };

    // Params that are not an object are treated as empty.
    let params: Params = msg
        .params
        .as_deref()
        .and_then(|raw| serde_json::from_str(raw.get()).ok())
        .unwrap_or_default();

    let header_version = req.protocol_version;
    let meta_version = params.meta.as_ref().and_then(|m| m.version.as_deref());
    let modern =
        method == "server/discover" || meta_version.is_some() || header_version.is_some_and(is_newer);

    if let (Some(h), Some(m)) = (
        header_version.filter(|v| !v.is_empty()),
        meta_version.filter(|v| !v.is_empty()),
    ) {
        if h != m {
            return rpc_error(
                id,
                ERR_HEADER_MISMATCH,
                "MCP-Protocol-Version header does not match _meta protocol version",
                400,
            );
        }
    }
    if let Some(h) = req.mcp_method.filter(|m| !m.is_empty()) {
        if h != method {
            return rpc_error(
                id,
                ERR_HEADER_MISMATCH,
                "Mcp-Method header does not match the request method",
                400,
            );
        }
    }
    let requested = meta_version.or(header_version).filter(|v| !v.is_empty());
    if let Some(requested) = requested {
        if !SUPPORTED_VERSIONS.contains(&requested) && method != "initialize" && method != "server/discover" {
            return rpc_error_data(
                id,
                ERR_UNSUPPORTED_VERSION,
                "Unsupported protocol version",
                Some(UnsupportedData {
                    supported: SUPPORTED_VERSIONS,
                    requested,
                }),
                400,
            );
        }
    }

    match method {
        "initialize" => {
            let asked = params.protocol_version.as_deref().unwrap_or("");
            let protocol_version = LEGACY_VERSIONS
                .iter()
                .find(|v| **v == asked)
                .unwrap_or(&LEGACY_VERSIONS[0]);
            ok(
                id,
                InitializeResult {
                    protocol_version,
                    capabilities: CAPABILITIES,
                    server_info: SERVER_INFO,
                    instructions: INSTRUCTIONS,
                },
            )
        }
        "server/discover" => ok(
            id,
            DiscoverResult {
                head: head(true),
                supported_versions: SUPPORTED_VERSIONS,
                capabilities: CAPABILITIES,
                instructions: INSTRUCTIONS,
                cache: CACHE_HINTS,
            },
        ),
        "ping" => ok(id, EmptyResult { head: head(modern) }),
        "tools/list" => ok(
            id,
            ListResult {
                head: head(modern),
                tools: tool_descriptors(),
                cache: modern.then_some(CACHE_HINTS),
            },
        ),
        "tools/call" => {
            let Some(name) = params.name.as_deref() else {
                return rpc_error(id, ERR_INVALID_PARAMS, "params.name is required", 200);
            };
            let args = params.arguments.as_deref().map_or("{}", |raw| raw.get());
            match call_tool(name, args, mailbox).await {
                Some((text, is_error)) => ok(
                    id,
                    CallResult {
                        head: head(modern),
                        content: vec![TextContent { kind: "text", text }],
                        is_error,
                    },
                ),
                None => rpc_error(id, ERR_INVALID_PARAMS, &format!("Unknown tool: {name}"), 200),
            }
        }
        _ => rpc_error(
            id,
            ERR_METHOD_NOT_FOUND,
            &format!("Method not found: {method}"),
            if modern { 404 } else { 200 },
        ),
    }
}

/// Runs a tool: `None` for an unknown name, else the result text and whether
/// it is an error.
async fn call_tool(name: &str, args: &str, mailbox: &impl Mailbox) -> Option<(String, bool)> {
    fn finish<T: Serialize>(out: Result<T, ToolError>) -> (String, bool) {
        match out {
            Ok(v) => match serde_json::to_string_pretty(&v) {
                Ok(text) => (text, false),
                Err(e) => (e.to_string(), true),
            },
            Err(ToolError::Input(m) | ToolError::Failed(m)) => (m, true),
        }
    }
    Some(match name {
        "read_mail" => match serde_json::from_str::<ReadMailArgs>(args) {
            Ok(a) => finish(mailbox.read_mail(a).await),
            Err(e) => (e.to_string(), true),
        },
        "ack" => match serde_json::from_str::<AckArgs>(args) {
            Ok(a) => finish(mailbox.ack(a).await),
            Err(e) => (e.to_string(), true),
        },
        "mailbox_info" => match serde_json::from_str::<NoArgs>(args) {
            Ok(_) => finish(mailbox.info().await),
            Err(e) => (e.to_string(), true),
        },
        _ => return None,
    })
}
