use std::cell::RefCell;
use std::collections::BTreeMap;

use mailroom_logic::mcp::*;
use mailroom_logic::record::{Kind, NewRecord, Record, Src};
use mailroom_logic::VERSION;
use serde::Deserialize;

// ---------- fake mailbox ----------

#[derive(Default)]
struct FakeMailbox {
    calls: RefCell<Vec<String>>,
    fail: bool,
}

fn canned_record() -> Record {
    Record {
        seq: 4,
        ts_ingest: "2026-11-18T10:05:00Z".into(),
        new: NewRecord {
            src: Src::Github,
            kind: Kind::Pr,
            id: "gh:1".into(),
            ts: "t".into(),
            actor: "nyx".into(),
            reference: "a/b#1".into(),
            title: "T".into(),
            body: "B".into(),
            url: "u".into(),
            tags: vec!["mine".into()],
        },
    }
}

impl Mailbox for FakeMailbox {
    async fn read_mail(&self, args: ReadMailArgs) -> Result<ReadMailResult, ToolError> {
        self.calls.borrow_mut().push(format!("read_mail {args:?}"));
        if self.fail {
            return Err(ToolError::Failed("storage exploded".into()));
        }
        Ok(ReadMailResult {
            agent: "nyx".into(),
            cursor: 3,
            last_seq: 4,
            unread: 1,
            returned: 1,
            more: false,
            records: vec![canned_record()],
        })
    }
    async fn ack(&self, args: AckArgs) -> Result<AckResult, ToolError> {
        self.calls.borrow_mut().push(format!("ack {args:?}"));
        Ok(AckResult {
            cursor: args.seq,
            unread: 0,
        })
    }
    async fn info(&self) -> Result<MailboxInfo, ToolError> {
        self.calls.borrow_mut().push("info".into());
        Ok(MailboxInfo {
            agent: "nyx".into(),
            signed_in_as: "nyx@example".into(),
            version: VERSION,
            cursor: 3,
            last_seq: 4,
            unread: 1,
            unread_by_tag: BTreeMap::new(),
            sources: vec![],
        })
    }
}

// ---------- test-local reply shapes ----------

#[derive(Deserialize)]
struct Reply {
    jsonrpc: String,
    id: Option<TestId>,
    result: Option<Res>,
    error: Option<Err>,
}

#[derive(Deserialize, PartialEq, Debug)]
#[serde(untagged)]
enum TestId {
    Str(String),
    Num(i64),
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct Res {
    #[serde(rename = "resultType")]
    result_type: Option<String>,
    #[serde(rename = "protocolVersion")]
    protocol_version: Option<String>,
    #[serde(rename = "supportedVersions")]
    supported_versions: Vec<String>,
    #[serde(rename = "ttlMs")]
    ttl_ms: Option<u64>,
    #[serde(rename = "cacheScope")]
    cache_scope: Option<String>,
    #[serde(rename = "_meta")]
    meta: Option<Meta>,
    #[serde(rename = "serverInfo")]
    server_info: Option<ServerInfo>,
    instructions: Option<String>,
    tools: Vec<Tool>,
    content: Vec<Content>,
    #[serde(rename = "isError")]
    is_error: Option<bool>,
}

#[derive(Deserialize)]
struct Meta {
    #[serde(rename = "io.modelcontextprotocol/serverInfo")]
    server_info: ServerInfo,
}

#[derive(Deserialize)]
struct ServerInfo {
    name: String,
    title: String,
    version: String,
}

#[derive(Deserialize)]
struct Tool {
    name: String,
    title: String,
    annotations: Annotations,
    #[serde(rename = "inputSchema")]
    input_schema: Schema,
}

#[derive(Deserialize)]
struct Annotations {
    #[serde(rename = "readOnlyHint")]
    read_only: bool,
    #[serde(rename = "idempotentHint")]
    idempotent: Option<bool>,
    #[serde(rename = "openWorldHint")]
    open_world: bool,
}

#[derive(Deserialize)]
struct Schema {
    #[serde(rename = "type")]
    kind: String,
    #[serde(default)]
    required: Vec<String>,
}

#[derive(Deserialize)]
struct Content {
    #[serde(rename = "type")]
    kind: String,
    text: String,
}

#[derive(Deserialize)]
struct Err {
    code: i32,
    message: String,
    data: Option<ErrData>,
}

#[derive(Deserialize)]
struct ErrData {
    supported: Vec<String>,
    requested: String,
}

#[derive(Deserialize, PartialEq, Debug)]
struct ReadMailBack {
    agent: String,
    cursor: i64,
    last_seq: i64,
    unread: i64,
    returned: usize,
    more: bool,
    records: Vec<Record>,
}

// ---------- helpers ----------

const META: &str = "io.modelcontextprotocol/protocolVersion";

fn run(
    method: &str,
    version: Option<&str>,
    mcp_method: Option<&str>,
    body: &str,
    mb: &FakeMailbox,
) -> McpReply {
    pollster::block_on(handle(
        McpRequest {
            http_method: method,
            protocol_version: version,
            mcp_method,
            body,
        },
        mb,
    ))
}

fn post(body: &str) -> McpReply {
    run("POST", None, None, body, &FakeMailbox::default())
}

fn post_with(version: Option<&str>, mcp_method: Option<&str>, body: &str) -> McpReply {
    run("POST", version, mcp_method, body, &FakeMailbox::default())
}

fn parse(r: &McpReply) -> Reply {
    serde_json::from_str(r.body.as_deref().expect("a body")).expect("a reply body")
}

fn result(r: &McpReply) -> Res {
    parse(r).result.expect("a result")
}

fn error(r: &McpReply) -> Err {
    parse(r).error.expect("an error")
}

fn modern_body(id: u32, method: &str, extra: &str) -> String {
    format!(
        r#"{{"jsonrpc":"2.0","id":{id},"method":"{method}","params":{{"_meta":{{"{META}":"{MODERN_VERSION}"}}{extra}}}}}"#
    )
}

// ---------- transport ----------

#[test]
fn get_is_405_with_allow_post() {
    for m in ["GET", "DELETE"] {
        let r = run(m, None, None, "", &FakeMailbox::default());
        assert_eq!(r.status, 405);
        assert_eq!(r.allow, Some("POST"));
        assert_eq!(r.body.as_deref(), Some("Method Not Allowed"));
    }
}

#[test]
fn parse_error_is_400_minus_32700() {
    let r = post("{nope");
    assert_eq!(r.status, 400);
    let e = error(&r);
    assert_eq!(e.code, -32700);
    assert_eq!(e.message, "Parse error");
    assert!(r.body.unwrap().contains(r#""id":null"#));
}

#[test]
fn batch_is_rejected() {
    let r = post(r#"  [{"jsonrpc":"2.0","id":1,"method":"ping"}]"#);
    assert_eq!(r.status, 400);
    let e = error(&r);
    assert_eq!(e.code, -32600);
    assert_eq!(e.message, "Batch requests are not supported");
    // A malformed batch is still a parse error, as in the reference.
    assert_eq!(error(&post("[{nope")).code, -32700);
}

#[test]
fn invalid_request_is_400_minus_32600() {
    for body in [r#"{"id":1,"method":"ping"}"#, r#"{"jsonrpc":"1.0","id":1}"#, "5"] {
        let r = post(body);
        assert_eq!(r.status, 400, "{body}");
        assert_eq!(error(&r).code, -32600, "{body}");
    }
}

#[test]
fn notification_and_client_response_get_202_without_body() {
    let n = post(r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#);
    assert_eq!((n.status, n.body), (202, None));
    let c = post(r#"{"jsonrpc":"2.0","id":1,"result":{}}"#);
    assert_eq!((c.status, c.body), (202, None));
}

#[test]
fn initialize_echoes_supported_legacy_version_else_newest_legacy() {
    let ask = |v: &str| {
        let body = format!(
            r#"{{"jsonrpc":"2.0","id":1,"method":"initialize","params":{{"protocolVersion":"{v}","capabilities":{{}},"clientInfo":{{"name":"x","version":"1"}}}}}}"#
        );
        let r = post(&body);
        assert_eq!(r.status, 200);
        result(&r)
    };
    assert_eq!(ask("2025-06-18").protocol_version.as_deref(), Some("2025-06-18"));
    let other = ask("2024-11-05");
    assert_eq!(other.protocol_version.as_deref(), Some("2025-11-25"));
    assert_eq!(other.result_type, None);
    assert!(other.meta.is_none());
    let info = other.server_info.unwrap();
    assert_eq!(
        (info.name.as_str(), info.title.as_str(), info.version.as_str()),
        ("mailroom", "Mailroom", VERSION)
    );
    assert_eq!(other.instructions.as_deref(), Some(INSTRUCTIONS));
}

#[test]
fn server_discover_is_modern_with_result_type_and_server_info_meta() {
    let r = post(r#"{"jsonrpc":"2.0","id":1,"method":"server/discover"}"#);
    assert_eq!(r.status, 200);
    let res = result(&r);
    assert_eq!(res.result_type.as_deref(), Some("complete"));
    assert_eq!(
        res.supported_versions,
        vec!["2026-07-28", "2025-11-25", "2025-06-18", "2025-03-26"]
    );
    assert_eq!(res.ttl_ms, Some(300_000));
    assert_eq!(res.cache_scope.as_deref(), Some("private"));
    assert_eq!(res.instructions.as_deref(), Some(INSTRUCTIONS));
    assert_eq!(res.meta.unwrap().server_info.name, "mailroom");
}

#[test]
fn header_and_meta_version_mismatch_is_minus_32020() {
    let r = post_with(Some("2025-11-25"), None, &modern_body(1, "tools/list", ""));
    assert_eq!(r.status, 400);
    assert_eq!(error(&r).code, -32020);
}

#[test]
fn mcp_method_header_mismatch_is_minus_32020() {
    let r = post_with(
        None,
        Some("tools/call"),
        r#"{"jsonrpc":"2.0","id":1,"method":"ping"}"#,
    );
    assert_eq!(r.status, 400);
    assert_eq!(error(&r).code, -32020);
    let ok = post_with(None, Some("ping"), r#"{"jsonrpc":"2.0","id":1,"method":"ping"}"#);
    assert_eq!(ok.status, 200);
}

#[test]
fn unsupported_version_is_minus_32022_with_supported_list() {
    let body = format!(
        r#"{{"jsonrpc":"2.0","id":1,"method":"tools/list","params":{{"_meta":{{"{META}":"2099-01-01"}}}}}}"#
    );
    let r = post_with(Some("2099-01-01"), None, &body);
    assert_eq!(r.status, 400);
    let e = error(&r);
    assert_eq!(e.code, -32022);
    let data = e.data.unwrap();
    assert_eq!(
        data.supported,
        ["2026-07-28", "2025-11-25", "2025-06-18", "2025-03-26"]
    );
    assert_eq!(data.requested, "2099-01-01");
}

#[test]
fn unknown_method_is_404_when_modern_and_200_when_legacy() {
    let modern = post_with(Some(MODERN_VERSION), None, &modern_body(1, "nope", ""));
    assert_eq!(modern.status, 404);
    assert_eq!(error(&modern).code, -32601);
    let legacy = post(r#"{"jsonrpc":"2.0","id":1,"method":"nope"}"#);
    assert_eq!(legacy.status, 200);
    let e = error(&legacy);
    assert_eq!(e.code, -32601);
    assert_eq!(e.message, "Method not found: nope");
}

#[test]
fn string_and_null_ids_round_trip() {
    let s = post(r#"{"jsonrpc":"2.0","id":"abc","method":"ping"}"#);
    assert_eq!(parse(&s).id, Some(TestId::Str("abc".into())));
    assert_eq!(parse(&s).jsonrpc, "2.0");
    let n = post(r#"{"jsonrpc":"2.0","id":null,"method":"ping"}"#);
    assert_eq!(n.status, 200);
    assert!(n.body.unwrap().contains(r#""id":null"#));
    let num = post(r#"{"jsonrpc":"2.0","id":42,"method":"ping"}"#);
    assert_eq!(parse(&num).id, Some(TestId::Num(42)));
}

#[test]
fn ping_is_empty_result_modern_adds_meta() {
    let legacy = result(&post(r#"{"jsonrpc":"2.0","id":1,"method":"ping"}"#));
    assert!(legacy.result_type.is_none() && legacy.meta.is_none());
    let modern = result(&post_with(None, None, &modern_body(1, "ping", "")));
    assert_eq!(modern.result_type.as_deref(), Some("complete"));
}

// ---------- tools ----------

#[test]
fn tools_list_names_three_tools_with_object_schemas() {
    let r = post(r#"{"jsonrpc":"2.0","id":1,"method":"tools/list"}"#);
    let res = result(&r);
    assert!(res.result_type.is_none() && res.ttl_ms.is_none() && res.cache_scope.is_none());
    let names: Vec<&str> = res.tools.iter().map(|t| t.name.as_str()).collect();
    assert_eq!(names, ["read_mail", "ack", "mailbox_info"]);
    let titles: Vec<&str> = res.tools.iter().map(|t| t.title.as_str()).collect();
    assert_eq!(titles, ["Read mail", "Acknowledge mail", "Mailbox status"]);
    for t in &res.tools {
        assert_eq!(t.input_schema.kind, "object", "{}", t.name);
        assert!(!t.annotations.open_world);
    }
    assert_eq!(res.tools[1].input_schema.required, ["seq"]);
    let hints: Vec<(bool, Option<bool>)> = res
        .tools
        .iter()
        .map(|t| (t.annotations.read_only, t.annotations.idempotent))
        .collect();
    assert_eq!(hints, [(true, None), (false, Some(true)), (true, None)]);
}

#[test]
fn tools_list_modern_adds_ttl_and_cache_scope() {
    let r = post_with(
        Some(MODERN_VERSION),
        Some("tools/list"),
        &modern_body(7, "tools/list", ""),
    );
    let res = result(&r);
    assert_eq!(res.result_type.as_deref(), Some("complete"));
    assert_eq!(res.ttl_ms, Some(300_000));
    assert_eq!(res.cache_scope.as_deref(), Some("private"));
    assert_eq!(res.meta.unwrap().server_info.name, "mailroom");
    assert_eq!(res.tools.len(), 3);
}

#[test]
fn call_read_mail_passes_arguments_and_returns_text_json() {
    let mb = FakeMailbox::default();
    let body = r#"{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"read_mail","arguments":{"limit":5,"after":2,"tags":["mine"]}}}"#;
    let r = run("POST", None, None, body, &mb);
    assert_eq!(r.status, 200);
    let res = result(&r);
    assert_eq!(res.is_error, Some(false));
    assert_eq!(res.content.len(), 1);
    assert_eq!(res.content[0].kind, "text");
    let back: ReadMailBack = serde_json::from_str(&res.content[0].text).unwrap();
    assert_eq!(back.agent, "nyx");
    assert_eq!(back.records, vec![canned_record()]);
    assert_eq!(
        (back.cursor, back.last_seq, back.unread, back.returned, back.more),
        (3, 4, 1, 1, false)
    );
    assert_eq!(
        *mb.calls.borrow(),
        ["read_mail ReadMailArgs { limit: Some(5), after: Some(2), tags: Some([\"mine\"]) }"]
    );
}

#[test]
fn call_ack_and_info_reach_the_mailbox() {
    let mb = FakeMailbox::default();
    let ack =
        r#"{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"ack","arguments":{"seq":9}}}"#;
    let info = r#"{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"mailbox_info"}}"#;
    assert_eq!(result(&run("POST", None, None, ack, &mb)).is_error, Some(false));
    assert_eq!(result(&run("POST", None, None, info, &mb)).is_error, Some(false));
    assert_eq!(*mb.calls.borrow(), ["ack AckArgs { seq: 9 }", "info"]);
}

#[test]
fn call_with_unknown_argument_is_tool_error() {
    let mb = FakeMailbox::default();
    let body = r#"{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"read_mail","arguments":{"limt":5}}}"#;
    let r = run("POST", None, None, body, &mb);
    assert_eq!(r.status, 200);
    let res = result(&r);
    assert_eq!(res.is_error, Some(true));
    assert!(res.content[0].text.contains("limt"), "{}", res.content[0].text);
    assert!(mb.calls.borrow().is_empty());
    // A missing required argument is also a tool error.
    let ack = r#"{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"ack","arguments":{}}}"#;
    assert_eq!(result(&run("POST", None, None, ack, &mb)).is_error, Some(true));
}

#[test]
fn call_unknown_tool_is_invalid_params() {
    let r = post(r#"{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"nope","arguments":{}}}"#);
    assert_eq!(r.status, 200);
    let e = error(&r);
    assert_eq!(e.code, -32602);
    assert_eq!(e.message, "Unknown tool: nope");
    let missing = post(r#"{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{}}"#);
    let e = error(&missing);
    assert_eq!((e.code, e.message.as_str()), (-32602, "params.name is required"));
}

#[test]
fn tool_failure_is_is_error_result_not_rpc_error() {
    let mb = FakeMailbox {
        fail: true,
        ..Default::default()
    };
    let body =
        r#"{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"read_mail","arguments":{}}}"#;
    let r = run("POST", None, None, body, &mb);
    assert_eq!(r.status, 200);
    let reply = parse(&r);
    assert!(reply.error.is_none());
    let res = reply.result.unwrap();
    assert_eq!(res.is_error, Some(true));
    assert_eq!(res.content[0].text, "storage exploded");
}

#[test]
fn checked_limit_bounds() {
    let with = |limit| ReadMailArgs {
        limit,
        ..Default::default()
    };
    assert_eq!(checked_limit(&with(None)), Ok(50));
    assert_eq!(
        checked_limit(&with(Some(0))),
        Err(ToolError::Input("limit must be between 1 and 200".into()))
    );
    assert!(checked_limit(&with(Some(201))).is_err());
    assert_eq!(checked_limit(&with(Some(1))), Ok(1));
    assert_eq!(checked_limit(&with(Some(200))), Ok(200));
}
