> Amended by `2026-11-18-per-agent-signin-design.md`: an MCP address per agent (`/<agent>/mcp`) and per-agent Discord sign-in through the core. Where the two disagree, that document wins.

# Mailroom — per-agent mailboxes on Cloudflare (design)

Date: 2026-11-18. Status: approved in conversation (shape and language); details
below marked "my call" were decided by Claude and are open to correction.

## Intent

Agents (Claude, Manus) currently spend inference on every wake-up walking Discord
channels and GitHub repos through the brokers to find out whether anything needs
them. Mailroom moves that off the model: a deterministic poller collects activity
into a per-agent inbox, and a waking agent makes one cheap `read_mail` call. It
reaches for the brokers only to act on what it found.

The model is Muse's mailbox (CycleFive/agent-mailbox, private): brokers collect and
never think; the agent reads in batches past a cursor; nothing pushes or wakes
anyone. This is that design at Cloudflare size, for several agents.

## Goals

- One inbox, one cursor, one record shape per agent, across every source.
- The poller holds no Discord token and no GitHub key. Those stay in the brokers.
- Fail closed: a failed poll changes nothing and is visible, never guessed past.
- Adding an agent or a watched channel/repo is a config edit and a deploy.
- Rust-first: everything that is the mailbox is Rust. TypeScript only where the
  tested OAuth library exists in no other language.

## Non-goals (v1)

- No event bus, stall detection or needs-owner mirroring (optional in Muse's too).
- No Discord gateway listener. REST delta polling only.
- No GitHub webhooks. Polling only, as in Muse's model.
- No automatic thread discovery. A thread is watched by listing its ID.
- No workflow-run / CI records.
- No runtime editing of filters by the agent. Config is the operator's.
- No per-agent restriction on which MCP client may take which mailbox (see Risks).

## Anti-goals

- Anything that wakes an agent. Delivery stays decoupled from attention.
- The mailroom acting on Discord or GitHub. It only reads.
- Marking mail read that the agent did not handle. Only `ack` moves the cursor.

## Approaches considered

1. **One mailbox Worker, a Durable Object per agent (picked).** Isolation comes from
   storage; one deployment; an agent is a config entry. Risk: the mailbox is chosen
   at sign-in consent, so a wrong pick gives a client another agent's mail.
2. **One Worker per agent**, like the brokers' `--env manus`. The URL is the agent,
   but every agent adds a deployment, a KV namespace and drift between copies.
3. **Mailbox inside each broker.** Splits mail into one inbox and cursor per source,
   which breaks the single-inbox rule. Dropped.

Language: Rust mailbox behind a small TypeScript sign-in door (picked) versus all
Rust with a hand-written OAuth 2.1 authorization server. The second was rejected
because that server is security-critical and `@cloudflare/workers-oauth-provider`
has no Rust equivalent; `catacombs` is a Discord login *client*, not an
authorization server, and does not build for the Workers wasm target today. The
door holds no mailbox logic, so replacing it with a Rust one later is contained.

A throwaway spike (2026-11-18, `worker` 0.9.0) proved the platform pieces locally:
a Rust Durable Object with SQLite, an alarm, a call to a named entrypoint on
another Worker, a typed JSON config var, and per-agent isolation. The entrypoint
call is a `services` entry with `"entrypoint": "Feed"` in the caller's wrangler
config and `env.service("NAME")?.fetch("https://feed/identity", None)` in Rust.

## Architecture

```
 claude.ai / Manus
        │  OAuth bearer
        ▼
 mailroom  (TypeScript door, public)          discord-broker[-manus]   github-broker
   /authorize  consent + mailbox picker   ┌──▶ Feed (internal, read-only) ◀──┐
   /callback   Discord sign-in ───────────┘        ▲                          │
   /mcp        token check, then forward           │ service bindings         │
        │ service binding                           │                          │
        ▼                                           │                          │
 mailroom-core  (Rust, no public URL) ──────────────┴──────────────────────────┘
   fetch      MCP JSON-RPC  ─▶ Mailbox DO (by agent name)
   scheduled  every minute  ─▶ each agent's Mailbox DO: tick
   Mailbox DO SQLite inbox, cursor, per-target poll state
```

Three deployables in the new repo plus one addition to each broker:

| Piece | Language | Public | Holds |
|---|---|---|---|
| `mailroom` (door) | TypeScript | yes | OAuth KV; no secrets |
| `mailroom-core` | Rust (`worker` 0.9) | no (`workers_dev: false`, no routes) | inboxes |
| `Feed` entrypoint in `discord-broker` and `github-broker` | TypeScript | no (named entrypoint) | nothing new |

### Trust boundaries

- `mailroom-core` is reachable only through service bindings from Workers in the
  same account. It trusts the `x-mailroom-agent` and `x-mailroom-user` headers the
  door sets, and nothing else sets them.
- `Feed` is a named `WorkerEntrypoint`. Public requests reach only a Worker's
  default export, so `Feed` is callable only through a service binding.
- The door has no secrets. Discord sign-in is delegated to a broker (below).

## The broker `Feed` entrypoint (TypeScript, both brokers)

A named export `Feed extends WorkerEntrypoint<Env>` with a `fetch` handler (Rust
calls service bindings fetch-style). JSON in, JSON out. Every call writes one audit
line (`event: "feed"`, path, status, ms; never bodies).

| Route | Broker | Does |
|---|---|---|
| `GET /identity` | both | Who the agent is on this platform, in one shape: `{id, login, name}` (all strings). Discord: bot user ID, username, display name. GitHub: app ID, `<slug>[bot]`, app name. |
| `GET /api/<path>?<query>` | both | Read-only passthrough. Upstream status and JSON body are returned as they are. |
| `GET /signin/client` | discord | `{client_id}` — the application's ID, for building the Discord authorize URL. |
| `POST /signin` `{code, redirect_uri}` | discord | Exchanges the code with the application's client secret, reads `/users/@me`, revokes the token, returns `{id, username}`. |

Rules:

- Only `GET` reaches `/api/`. Anything else is 405.
- Discord `/api/` paths go through the existing `checkPassthrough("GET", …)` in
  `src/tools.ts`, the same check `discord_request` uses, so
  `DISCORD_ALLOWED_GUILDS` and `DISCORD_ALLOWED_CHANNELS` bind the mailroom too.
  That check already admits `/channels/{id}`, `/channels/{id}/messages` and the
  reactions endpoint for an allowed channel. They use the existing
  `DiscordClient` and rate limiter.
- GitHub `/api/` paths must start with `/repos/`. They use the existing
  `GitHubClient` and installation token cache.
- Path normalisation reuses each broker's `normalizePath`.
- Errors are `{error: string}` with the upstream or a 4xx/5xx status.

Each broker change ships as a PR in its own repo with a minor version bump
(0.1.0 → 0.2.0), then a redeploy. `discord-broker` is redeployed for both the
default and `manus` environments.

## `mailroom-core` (Rust)

Cargo workspace with two crates so the logic is tested natively:

- `crates/logic` — pure Rust, no `worker` dependency. Record type, config types and
  validation, filter engine, Discord and GitHub pollers written against a
  `FeedClient` trait, MCP protocol types. Native `cargo test`.
- `crates/worker` — `worker` 0.9 glue: fetch and scheduled handlers, the `Mailbox`
  Durable Object, SQLite storage, the `FeedClient` implementation over a service
  binding, the MCP HTTP handler.

All wire and stored data are typed serde structs and enums. No `serde_json::json!`
and no `serde_json::Value` for data mailroom owns. Upstream Discord and GitHub
payloads deserialize into concrete structs holding only the fields used, with
`#[serde(default)]` where a field may be absent. JSON Schemas for tool inputs are
derived from the argument structs with `schemars`.

### Record

Muse's shape, unchanged:

```json
{"seq": 12, "ts_ingest": "2026-11-18T10:05:00Z", "src": "discord", "kind": "message",
 "id": "dc:1568000000000000001", "ts": "2026-11-18T10:04:40Z", "actor": "Nyx",
 "ref": "#general", "title": "", "body": "…", "url": "https://discord.com/channels/G/C/M",
 "tags": ["routed:mention"]}
```

- `seq` is assigned by the inbox (SQLite `AUTOINCREMENT`), strictly increasing,
  never reused.
- `id` is unique per inbox; a second append of the same `id` is ignored. This makes
  appends idempotent if two ticks ever overlap.
- `url` carries what the agent needs to act: a Discord jump link holds guild,
  channel and message IDs; a GitHub `html_url` holds repo and number.
- Records are pointers, not evidence. `body` is an excerpt: Discord content up to
  2000 characters, GitHub bodies up to 600 with ` [...]`.

### Configuration

One JSON var, `MAILROOM`, in `wrangler.jsonc` (git-ignored; a commented
`wrangler.example.jsonc` is committed), deserialized into typed structs with
`deny_unknown_fields`:

```jsonc
"MAILROOM": {
  "retain_days": 30,
  "agents": {
    "claude": {
      "sources": [
        { "type": "discord", "broker": "DISCORD_CLAUDE", "every_secs": 120,
          "channels": ["1566796119839875999"],
          "reactions": true, "skip_self": true, "keep": "all",
          "rules": [
            { "when": { "kind": ["reaction"], "actor_not": ["Lothrop"] }, "then": "drop" },
            { "when": { "body_contains": ["kharon"] }, "then": { "tag": "kharon" } }
          ] },
        { "type": "github", "broker": "GITHUB", "every_secs": 300,
          "repos": ["CycleFive/kharon"], "review_label": "review:claude",
          "skip_self": true, "keep": "tagged", "rules": [] }
      ]
    },
    "manus": { "sources": [ /* … */ ] }
  }
}
```

- Agent names match `[a-z0-9-]{1,32}`.
- `broker` names a service binding declared in `services` (entrypoint `Feed`).
- `every_secs` minimum 60 (the cron resolution); defaults 120 for Discord, 300 for
  GitHub.
- `retain_days`: acknowledged records older than this are pruned; 0 keeps
  everything. Unread records are never pruned. Default 30.
- Invalid config fails closed: the poller does nothing, and every request to
  `mailroom-core` (so every MCP call) answers HTTP 500 with the config error text.
  It is also logged.
- The config is parsed and validated from the var on each request and each tick; it
  is small. An agent removed from the config keeps its stored inbox but becomes
  unreachable; nothing purges it (my call).

### Filters

Mechanical only. For each new upstream item, in order:

1. **Self.** With `skip_self` (default true), items authored by the agent's own
   identity produce no record. They still advance watermarks.
2. **Built-in tags.**
   - Discord: `routed:mention` when the message's `mentions` include the bot;
     `routed:reply` when it replies to one of the bot's messages.
   - GitHub: `routed:mention` when the body contains `@<app slug>`;
     `routed:assigned` when the bot is an assignee; `routed:review` when the PR
     carries `review_label` or the bot is a requested reviewer; `mine` when the
     comment, review or event is on an issue or PR the bot authored.
3. **Rules**, first to last. `when` holds conditions that must all match; each is an
   any-of list: `kind`, `actor`, `actor_not`, `ref`, `tag`, `body_contains`
   (case-insensitive substring of title plus body). `then` is `"drop"` or
   `{"tag": "…"}`. A dropped record stops there.
4. **Keep.** `"all"` keeps every surviving record; `"tagged"` keeps only records
   with at least one tag.

A `kind: "gap"` record bypasses the rules and `keep`: it is the only sign that
items were skipped, so it is always kept, untouched.

No regular expressions (my call: substring matching covers the stated need and
keeps the wasm small).

### Polling

A cron trigger fires every minute. The scheduled handler asks each configured
agent's Durable Object to `tick`. A tick polls each target (a channel or a repo)
whose `every_secs` has elapsed since its last attempt. The last attempt is stamped
with the tick's start time and a target counts as due 30 seconds early, so the
jitter of the one-minute cron cannot push a poll a whole minute late. An in-memory
flag makes an overlapping tick return at once.

Each target is polled by a pure function in `crates/logic`:

```
poll(feed, source_config, identity, previous_state) -> Result<(Vec<Record>, new_state), PollError>
```

On `Ok` the worker appends the records and stores the new state in one synchronous
run of SQL statements with no `await` between them, which a Durable Object commits
together (`worker` 0.9 has no synchronous transaction call). The unique record `id`
is the second guard: if state were ever lost after an append, the re-derived
records would be ignored. On `Err` nothing is written except the target's status
(time and error text). Targets fail independently.

The agent's identity on a broker (`GET /identity`) is cached in the Durable Object
for an hour. If it cannot be read, every target of that source fails closed for
the tick.

First contact with a target (no stored state) records the current position and
emits nothing. History is never replayed.

**Discord channel.** One request: `GET /channels/{id}/messages?limit=50`.

- New messages are those with an ID above the watermark (snowflakes compared as
  integers).
- If all 50 are new, page back with `before=` up to 5 pages. If the watermark is
  still not reached, append one `kind: "gap"` record saying messages may have been
  skipped, then continue from the newest.
- `ref` is `#<channel name>`; name and guild ID are read once with
  `GET /channels/{id}` and kept in the target's state.
- Only ordinary messages and replies (Discord types 0 and 19) become records.
- `routed:reply` reads the author of `referenced_message`, which Discord includes
  in the reply itself, so it does not depend on the 50-message window.
- On first contact the reactors of every reaction already in the window are read,
  so that later rises can be attributed to the right person.
- Reactions (when `reactions` is true): compare each message's per-emoji counts in
  the 50-message window with the stored snapshot. Where a count rose, read
  `GET /channels/{c}/messages/{m}/reactions/{emoji}` and append one
  `kind: "reaction"` record per reactor not seen before (`id`
  `dc:react:<message>:<emoji>:<user>`, `body` the emoji, `url` the message's jump
  link). Removals are ignored. A reaction on a message older than the window is
  missed. The count compared is `count_details.normal` when Discord sends it, and an
  emoji whose stored reactors already number 100 (the fetch cap) is not fetched again.
- Edits and deletions are ignored.

**GitHub repo.** A port of Muse's broker: issues, pulls, issue comments and review
comments, each against its own number or ID watermark, plus reviews of pulls
updated since the last check, kept when submitted after the last check. Record
kinds are `issue`, `pr`, `issue_comment`, `review_comment`, `review` and `event`;
IDs are `gh:<owner>/<repo>:<kind>:<number or id>`; `ref` is `<owner>/<repo>#<n>`.
The last-check time is stored 60 seconds early so nothing falls between two polls;
the unique record `id` absorbs the overlap. One addition (my call): `GET /repos/{r}/issues/events` against an ID
watermark, keeping `assigned`, `labeled`, `review_requested`, `closed`, `merged`
and `reopened` as `kind: "event"` records. Without it, a review label or an
assignment added *after* a PR was opened would never produce a record.

Overflow: each list is read 50 items per poll. If a list comes back full and every
item is above its watermark, one `kind: "gap"` record is appended for that repo and
list (`id` `gh:gap:<owner>/<repo>:<list>:<epoch>`, not on first contact). Reviews of a
pull request are paged 100 at a time, oldest first, up to 10 pages; a full 10th page
yields a `gap` record for that PR. A failed page fails the whole poll.

State per target is one typed struct stored as JSON.

### Storage (per-agent Durable Object, SQLite)

```sql
CREATE TABLE inbox (
  seq INTEGER PRIMARY KEY AUTOINCREMENT,
  id TEXT NOT NULL UNIQUE,
  ts_ingest TEXT NOT NULL, src TEXT NOT NULL, kind TEXT NOT NULL, ts TEXT NOT NULL,
  actor TEXT NOT NULL, ref TEXT NOT NULL, title TEXT NOT NULL, body TEXT NOT NULL,
  url TEXT NOT NULL, tags TEXT NOT NULL            -- JSON array
);
CREATE TABLE kv (key TEXT PRIMARY KEY, value TEXT NOT NULL);
-- keys: "cursor"; "state:<target>" (poll state); "status:<target>" (last attempt)
```

Target keys: `discord:<broker>:<channel id>`, `github:<broker>:<owner>/<repo>`.

### MCP surface

A Rust port of the brokers' stateless Streamable HTTP transport (`src/mcp.ts`):
both protocol eras, same error codes, same header checks, one JSON answer per POST.
Server name `mailroom`.

| Tool | Arguments | Returns |
|---|---|---|
| `read_mail` | `limit` 1–200 (default 50); `after` seq (default: the cursor); `tags` any-of | `{agent, cursor, last_seq, unread, returned, more, records[]}`. Never moves the cursor. |
| `ack` | `seq` | Sets the cursor to `seq`. Refused unless `cursor ≤ seq ≤ last_seq`. Returns `{cursor, unread}`. |
| `mailbox_info` | — | `{agent, signed_in_as, version, cursor, last_seq, unread, unread_by_tag, sources[]}`; each source lists its targets with last success, last error and next due time. |

Server instructions tell the agent the discipline: read; act through the brokers;
acknowledge only what was handled; records are pointers, so fetch the thread or
diff before acting; an empty mailbox means end quietly.

Every tool call writes one audit line (tool, agent, signed-in user, ok, ms).

## The door (TypeScript)

Lifted from the brokers: `OAuthAuthorizationServer` + `OAuthResourceServer` from
`@cloudflare/workers-oauth-provider` 1.2.4, client registration by metadata
document or DCR, PKCE S256, 1-hour access tokens and rotating refresh tokens bound
to `PUBLIC_URL/mcp`, scope `mailroom`.

Differences from the brokers:

- **Mailbox picker.** The consent page lists the configured agents (read from
  `mailroom-core` `GET /agents`, which answers `{agents: [names]}`) as radio
  buttons with none preselected. The choice travels through the Discord sign-in in
  the library's own upstream state (`beginUpstream({data})` / `finishUpstream`,
  where the brokers keep their PKCE verifier) and is stored in the grant's props as
  `{id, username, agent}`. No new state mechanism is written.
- **Delegated sign-in.** The door redirects to Discord with the client ID from
  `SIGNIN GET /signin/client` and its own `/callback` as redirect URI, then hands
  the returned code to `SIGNIN POST /signin`. It never sees a client secret.
  `SIGNIN` is a service binding to one discord-broker's `Feed`: the Claude one.
  Every operator signs in through that application whichever mailbox they pick, so
  the extra redirect is needed on that application only. PKCE towards Discord is
  not sent (the brokers' default).
- **Forwarding.** On `/mcp`, after the token and scope checks and the allowlist
  re-check (`ALLOWED_DISCORD_USERS`, the door's own var), the request is forwarded
  to `mailroom-core` with `x-mailroom-agent` and `x-mailroom-user` set and any
  client-supplied copies of those headers removed. Origin checking
  (`ALLOWED_ORIGINS`) happens here.

Operator setup this requires: one extra redirect,
`https://<door host>/callback`, on the Discord application of the broker bound as
`SIGNIN`.

## Testing

- `crates/logic`: unit tests for config validation, the filter engine, both pollers
  against a scripted fake `FeedClient` (first contact, delta, paging, gap, reaction
  diff, fail-closed on any upstream error), and MCP message round-trips.
- `crates/worker`: a local smoke test (`wrangler dev` with a fake `Feed` Worker)
  covering tick → `read_mail` → `ack` → `mailbox_info` and isolation between two
  agents.
- Door: vitest, following the brokers' `flow.test.ts`: full sign-in with a fake
  `SIGNIN` and fake core, picker required, allowlist refusal, header stripping.
- Brokers: tests for `Feed` routing, method refusal, scope enforcement and the
  sign-in exchange.
- CI (GitHub Actions): `cargo fmt --check`, `cargo clippy`, `cargo test`, the wasm
  build, and the door's typecheck and tests.

## Deployment

1. Brokers: merge and deploy `Feed` (`discord-broker`, `discord-broker --env manus`,
   `github-broker`).
2. `mailroom-core`: deploy with service bindings to the brokers' `Feed`.
3. Door: create its KV namespace, deploy.
4. Operator: add the door's `/callback` redirect on the Discord application; add
   `https://<door host>/mcp` as a connector in each agent and pick that agent's
   mailbox on the consent page.

Initial config (my call, a one-line edit to change): Claude watches the five text
channels of the allowed guild through `discord-broker` (`keep: "all"`) and
`CycleFive/kharon`, `discord-broker`, `github-broker` and `mailroom` through
`github-broker` (`keep: "tagged"`, so only mail routed to it); Manus watches the
same channels through `discord-broker-manus`.

## Risks

- **Wrong mailbox at consent.** Nothing stops Manus's client from being granted
  Claude's mailbox if the operator picks it. Mitigation in v1: no default choice,
  the agent name shown prominently, and `mailbox_info` returns the agent name. A
  per-agent client-host allowlist is the follow-up if this bites.
- **Shared-IP 429s from Discord** (documented in the broker README) surface as
  failed polls; the watermark stays put and the target is retried when it is next due.
- **Reaction window.** Reactions on messages older than the latest 50 per channel
  are not seen.
- **`worker` crate churn.** 0.9.0 is a week old. The crate is pinned exactly.

## Left to the builder

Module layout inside each crate, error type design, exact SQL beyond the schema
above, HTML of the consent page (follow the brokers' style), test fixture shapes,
and the smoke-test harness.
