# mailroom

Per-agent mailboxes on Cloudflare Workers. A deterministic poller collects Discord and
GitHub activity into one inbox per AI agent. The agent reads it over MCP with a cursor,
instead of spending inference walking channels and repos to find out whether anything
needs it.

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

The design follows a mailbox pattern first built by Muse, another agent in the same team,
for a single agent on one host. Mailroom is that pattern at Cloudflare size, for several
agents.

## Why

Without it, every wake-up costs model tokens: list the channels, read each one, list the
repos, read each one, decide that nothing was for you. Most wake-ups find nothing. With
mailroom the looking is done by code that costs nothing to run, and the agent makes one
`read_mail` call. It reaches for the brokers only to act on what it found.

## Rules

1. **The poller never thinks.** It copies new activity into the inbox and applies
   mechanical filters. It has no model, no judgement and no Discord token or GitHub key:
   those stay in the brokers.
2. **The agent reads in batches and acknowledges only what it handled.** `read_mail` never
   moves the cursor. `ack` does, and only to a sequence number the agent chose.
3. **Nothing pushes.** Mailroom never wakes an agent and never acts on Discord or GitHub.
   It only reads. Delivery stays decoupled from attention.

A failed poll changes nothing and is visible in `mailbox_info`. It is never guessed past.

## Pieces

| Piece | Language | Public | Holds |
| --- | --- | --- | --- |
| `mailroom` (the door, `door/`) | TypeScript | yes | the OAuth KV; no secrets |
| `mailroom-core` (`crates/worker/`) | Rust | no | the inboxes |
| `Feed` entrypoint in [discord-broker](https://github.com/CycleFive/discord-broker) and [github-broker](https://github.com/CycleFive/github-broker) | TypeScript | no | nothing new |

`crates/logic/` is the pure-Rust part (records, config, filters, pollers, MCP protocol).
It has no Workers dependency, so its tests run with plain `cargo test`.

The brokers must be at version 0.2.0 or later: that version adds the internal `Feed`
entrypoint mailroom reads through. `Feed` is read-only and has no public URL. It is
reachable only through a service binding from a Worker in the same Cloudflare account, and
the brokers' allowlists (`DISCORD_ALLOWED_GUILDS`, `DISCORD_ALLOWED_CHANNELS`) apply to it.

The door is TypeScript only because `@cloudflare/workers-oauth-provider`, the tested OAuth
2.1 authorization server, exists in no other language. It holds no mailbox logic.

## Records

Every item in every inbox has this shape:

```json
{"seq": 12, "ts_ingest": "2026-11-18T10:05:00Z", "src": "discord", "kind": "message",
 "id": "dc:123456789012345678", "ts": "2026-11-18T10:04:40Z", "actor": "Nyx",
 "ref": "#general", "title": "", "body": "...",
 "url": "https://discord.com/channels/111111111111111111/222222222222222222/123456789012345678",
 "tags": ["routed:mention"]}
```

| Field | Meaning |
| --- | --- |
| `seq` | Assigned by the inbox. Strictly increasing, never reused. |
| `ts_ingest` | When the poller stored it. |
| `src` | `discord` or `github`. |
| `kind` | See below. |
| `id` | Unique per inbox. A second append of the same `id` is ignored, so overlapping polls are harmless. |
| `ts`, `actor` | When it happened and who did it. |
| `ref` | `#<channel name>` for Discord, `<owner>/<repo>#<number>` for GitHub. |
| `title`, `body` | An excerpt: Discord content up to 2000 characters, GitHub bodies up to 600 with ` [...]`. |
| `url` | What the agent needs to act: a Discord jump link (guild, channel, message IDs) or a GitHub `html_url` (repo and number). |
| `tags` | Built-in and rule-added tags, below. |

Records are pointers, not evidence. Fetch the thread or the diff through the broker before
acting on one.

Kinds: Discord has `message`, `reaction` and `gap` (messages may have been skipped; see
below). GitHub has `issue`, `pr`, `issue_comment`, `review_comment`, `review` and `event`
(`assigned`, `labeled`, `review_requested`, `closed`, `merged`, `reopened`).

Tags the poller adds itself:

| Tag | Meaning |
| --- | --- |
| `routed:mention` | Discord: the message mentions the agent's bot. GitHub: the text contains `@<app slug>`. |
| `routed:reply` | Discord: the message replies to one of the bot's messages. |
| `routed:assigned` | GitHub: the agent is an assignee. |
| `routed:review` | GitHub: the PR carries `review_label` or the agent is a requested reviewer. |
| `mine` | GitHub: a comment, review or event on an issue or PR the agent authored. |

Tags starting `routed:` are addressed to the agent. `mine` is activity on its own work.
Untagged records are context. Rules can add more tags (see Configuration).

## Tools

| Tool | Arguments | Returns |
| --- | --- | --- |
| `read_mail` | `limit` 1-200 (default 50); `after` a seq (default: the cursor); `tags` keeps records carrying any of them | `{agent, cursor, last_seq, unread, returned, more, records[]}`, oldest first. Never moves the cursor. |
| `ack` | `seq` | `{cursor, unread}`. Moves the cursor to `seq`. Refused unless `cursor <= seq <= last_seq`. |
| `mailbox_info` | none | `{agent, signed_in_as, version, cursor, last_seq, unread, unread_by_tag, sources[]}`. Each source lists its targets with last success, last error and next due time. |

The server instructions tell the agent the discipline: read, act through the brokers,
acknowledge only what was handled, and end quietly on an empty mailbox. Every tool call
writes one audit line (tool, agent, signed-in user, ok, ms) to Workers Logs.

## Configuration

One JSON var, `MAILROOM`, in `crates/worker/wrangler.jsonc`. That file is git-ignored;
copy `crates/worker/wrangler.example.jsonc`. The var is parsed on every request and every
tick, and unknown fields are rejected.

**Top level**

| Field | Default | Rule |
| --- | --- | --- |
| `retain_days` | `30` | Acknowledged records older than this are pruned. `0` keeps everything. Unread records are never pruned. |
| `agents` | required | Map of agent name to `{ "sources": [...] }`. Must not be empty. Names are 1-32 characters of `a-z`, `0-9`, `-`. |

**Discord source** (`"type": "discord"`)

| Field | Default | Rule |
| --- | --- | --- |
| `broker` | required | Name of a service binding to a discord-broker `Feed`. Non-empty, only `A-Z`, `0-9`, `_`. |
| `channels` | required | Channel IDs. Not empty, no duplicates, each 5-25 ASCII digits. A thread is watched by listing its ID here. |
| `every_secs` | `120` | Minimum `60` (the cron resolution). |
| `reactions` | `true` | Record reactions added to recent messages. |
| `skip_self` | `true` | Items authored by the agent's own bot produce no record. |
| `keep` | `"all"` | `"all"` or `"tagged"`. |
| `rules` | `[]` | See below. |

**GitHub source** (`"type": "github"`)

| Field | Default | Rule |
| --- | --- | --- |
| `broker` | required | Name of a service binding to a github-broker `Feed`. Same rule as above. |
| `repos` | required | `owner/name`. Not empty, no duplicates. |
| `every_secs` | `300` | Minimum `60`. |
| `review_label` | none | A label that means "review this". When set it must not be empty. |
| `skip_self` | `true` | As above. |
| `keep` | `"all"` | `"all"` or `"tagged"`. |
| `rules` | `[]` | See below. |

**Rules.** Each rule is `{ "when": {...}, "then": ... }`. `then` is `"drop"` or
`{ "tag": "name" }` (the tag must not be empty). `when` holds conditions that must all
match; each is a list that matches if any element does, and an empty or missing list is no
condition.

| Condition | Matches |
| --- | --- |
| `kind` | the record's kind (`message`, `reaction`, `gap`, `issue`, `pr`, `issue_comment`, `review_comment`, `review`, `event`) |
| `actor` | the actor, case-insensitive |
| `actor_not` | none of these actors |
| `ref` | the exact `ref` |
| `tag` | the record already carries one of these tags |
| `body_contains` | a case-insensitive substring of title plus body |

There are no regular expressions.

**Filter order.** For each new upstream item:

1. `skip_self`: the agent's own items produce no record. They still advance the watermarks.
2. Built-in tags are added.
3. Rules run first to last. A tag rule adds its tag; a drop rule discards the record and
   stops there.
4. `keep`: `"all"` keeps every surviving record, `"tagged"` keeps only records with at
   least one tag.

**Invalid config fails closed.** The poller does nothing, and every request to
`mailroom-core` answers HTTP 500 with `mailroom config: <message>`. It is also logged.
An agent removed from the config keeps its stored inbox but becomes unreachable. Nothing
purges it.

**Example** (all IDs are fake):

```jsonc
"MAILROOM": {
  "retain_days": 30,
  "agents": {
    "claude": {
      "sources": [
        { "type": "discord", "broker": "DISCORD_CLAUDE", "every_secs": 120,
          "channels": ["123456789012345678", "123456789012345679"],
          "reactions": true, "skip_self": true, "keep": "all",
          "rules": [
            { "when": { "kind": ["reaction"], "actor_not": ["Alice"] }, "then": "drop" },
            { "when": { "body_contains": ["deploy"] }, "then": { "tag": "deploy" } }
          ] },
        { "type": "github", "broker": "GITHUB", "every_secs": 300,
          "repos": ["your-org/your-repo"], "review_label": "review:claude",
          "skip_self": true, "keep": "tagged", "rules": [] }
      ]
    },
    "manus": {
      "sources": [
        { "type": "discord", "broker": "DISCORD_MANUS",
          "channels": ["123456789012345678"] }
      ]
    }
  }
}
```

## What is polled, and what is missed

Polling is REST only: a cron trigger fires every minute, and each target (a channel or a
repo) is polled when its `every_secs` has elapsed. There is no gateway listener and no
webhook.

- **First contact emits nothing.** The first poll of a target records its current position
  and produces no records. History is never replayed.
- **Discord messages.** Each poll reads the latest 50 messages of the channel. If all 50
  are new it pages back, up to 5 pages. If the last position is still not reached it
  appends one `gap` record saying messages may have been skipped, then continues from the
  newest. Only ordinary messages and replies become records.
- **Reactions.** Read from the same 50-message window. When a count rises, the new
  reactors become one `reaction` record each. A reaction on a message older than the
  window is missed, and so are removed reactions.
- **Threads.** Not discovered. A thread is watched only when its ID is listed in
  `channels`.
- **Edits and deletions** are ignored, on both Discord and GitHub.
- **GitHub.** Issues, pulls, issue comments, review comments, reviews and issue events,
  each against its own watermark. The last-check time is stored 60 seconds early so
  nothing falls between two polls; the unique `id` absorbs the overlap. Workflow runs are
  not recorded.
- **Failures.** A failed target changes nothing except its status (time and error text),
  and the next tick retries from the same position. Targets fail independently. If a
  broker's identity cannot be read, every target of that source fails for the tick.

## Setup

You need:

- Rust (stable) with the wasm target: `rustup target add wasm32-unknown-unknown`
- `worker-build`: `cargo install worker-build --version 0.9.0 --locked`
- Node 20+
- A Cloudflare account
- discord-broker and github-broker, both at 0.2.0 or later, already set up

The Discord redirect and the mailbox picker below assume the broker bound as `SIGNIN` is
the discord-broker whose application you already sign in with.

### 1. Deploy the brokers' `Feed`

Merge and deploy discord-broker (and its `--env manus` copy, if you run one) and
github-broker at 0.2.0 or later. Nothing else changes for them.

### 2. Deploy `mailroom-core`

```sh
cd crates/worker
cp wrangler.example.jsonc wrangler.jsonc
```

Edit `wrangler.jsonc`: fill in `MAILROOM` (see Configuration), and make sure there is one
`services` entry per broker named in it, each with `"entrypoint": "Feed"`. Then:

```sh
npx wrangler login
npx wrangler deploy
```

The Worker is named `mailroom-core`. It has no public URL (`workers_dev` is false and
there are no routes); the door reaches it through a service binding. It also runs a cron
trigger every minute.

### 3. Deploy the door

```sh
cd door
npm install
cp wrangler.example.jsonc wrangler.jsonc
npx wrangler kv namespace create OAUTH_KV
```

`wrangler.jsonc` is git-ignored: it holds your own IDs and URL, no secrets. Paste the KV id
into it and fill in `vars`:

| Var | |
| --- | --- |
| `PUBLIC_URL` | The door's public origin, no trailing slash. |
| `ALLOWED_DISCORD_USERS` | Your numeric Discord user ID (Developer Mode, right-click yourself, Copy User ID), not a username. Comma- or space-separated. |
| `ALLOWED_ORIGINS` | Optional. Comma-separated Origins accepted when a request carries an Origin header. |
| `DISABLE_CIMD` | Optional. `"true"` makes clients register through Dynamic Client Registration instead of a Client ID Metadata Document. |

The `MAILROOM` service binding points at `mailroom-core`. The `SIGNIN` binding points at
`discord-broker` with entrypoint `Feed`. The door holds no secrets: the Discord client
secret stays in the broker.

```sh
npx wrangler deploy
```

It is deployed as `mailroom`, so the MCP URL is
`https://mailroom.<subdomain>.workers.dev/mcp`. Put that origin in `PUBLIC_URL` and
deploy again.

### 4. Add the Discord redirect

On the Discord application of the broker bound as `SIGNIN`, open OAuth2 and add the
Redirect `https://mailroom.<subdomain>.workers.dev/callback`. It must match exactly, with
no trailing slash. Every operator signs in through that application whichever mailbox they
pick, so this one redirect is enough.

### 5. Connect

- **claude.ai**: Settings, Connectors, Add custom connector. Give the URL
  `https://mailroom.<subdomain>.workers.dev/mcp` exactly, and leave Advanced settings
  blank.
- **Claude Code**: `claude mcp add --transport http mailroom https://mailroom.<subdomain>.workers.dev/mcp`
- **Manus**: add a custom MCP server with the same URL, and complete the sign-in.

You see the consent page (check it names the client you expect), then Discord's
"authorize" screen asking only for your username.

### 6. Pick the mailbox

The consent page lists the configured agents as radio buttons with none selected. Pick the
one this client is. The choice is stored in the grant and cannot be changed without
signing in again. Nothing stops a client being given another agent's mailbox if you pick
it, so the page shows the agent name prominently and `mailbox_info` returns it. Check
`agent` in `mailbox_info` after connecting.

## If something is wrong

| You see | Usually |
| --- | --- |
| `mailbox_info` shows a `last_error` on a target | That target's last poll failed; the position has not moved and the next tick retries. The text says whether the broker, the allowlist or Discord/GitHub refused. Discord sometimes answers Workers with a 429 because of other traffic on a shared IP; it usually clears. |
| HTTP 500 `mailroom config: ...` on every call, or on the consent page | The `MAILROOM` var is invalid; the message names the agent, source and field. Fix it and deploy `mailroom-core`. |
| "Pick a mailbox." | You approved without choosing one, or the choice is no longer in the config. Go back and pick. |
| Discord says "Invalid OAuth2 redirect_uri" | The Redirect on the `SIGNIN` broker's application is not exactly `<PUBLIC_URL>/callback`. |
| The door's sign-in page shows a broker error | The code exchange failed in the broker (usually the client secret or the redirect). Fix it there. |
| "not in ALLOWED_DISCORD_USERS" | The page names the account and ID; put that ID in the door's var and deploy. |
| `ack` is refused | `seq` must be between the cursor and `last_seq`. |
| A channel produces nothing | First contact emits nothing; check `last_ok` in `mailbox_info`, the broker's channel allowlist, and that the bot can read the channel. |

`npx wrangler tail` on either Worker shows one JSON line per poll (`poll`), tool call
(`tool_call`) and Feed call (`feed`).

## A prompt for the agent

```
On waking, call read_mail on the mailroom MCP server. If it returns no records, end
quietly. Otherwise go through the records oldest first. A record is a pointer: before
acting, fetch the thread or diff through the Discord or GitHub broker, and act there.
Records tagged routed:* are addressed to you; mine is activity on your own work; untagged
records are context. When you are done, call ack with the highest seq you actually
handled. Leave anything you did not handle unacknowledged.
```

## Development

```sh
cargo test --workspace                   # the logic crate: config, filters, pollers, MCP
cargo clippy --workspace --all-targets -- -D warnings
crates/worker/smoke/run.sh               # wrangler dev with a fake Feed: tick, read_mail, ack, mailbox_info
cd door && npm test && npm run typecheck # sign-in flow, picker, header stripping
```

The smoke test needs `worker-build`, `curl` and `jq`. The door's tests run the whole
sign-in against fake `SIGNIN` and `mailroom-core` service bindings; nothing touches a real
Discord or GitHub.

CI (`.github/workflows/ci.yml`) runs `cargo fmt --check`, clippy, `cargo test`, the wasm
build of `crates/worker`, and the door's typecheck and tests.

Layout: `crates/logic/src/` (`config.rs`, `record.rs`, `filter.rs`, `discord.rs`,
`github.rs`, `feed.rs`, `time.rs`, and the MCP protocol), `crates/worker/` (the
`Mailbox` Durable Object, SQLite storage, the service-binding `Feed` client, the fetch and
scheduled handlers), `door/src/` (`index.ts` routing and forwarding, `auth.ts` consent and
sign-in, `env.ts`).

Not covered: a Discord gateway listener, GitHub webhooks, workflow-run records, automatic
thread discovery, editing filters at runtime, and restricting which client may take which
mailbox.

## License

MIT, see [LICENSE](LICENSE).
