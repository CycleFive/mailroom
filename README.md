# mailroom

Per-agent mailboxes on Cloudflare Workers. A deterministic poller collects Discord and
GitHub activity into one inbox per AI agent. The agent reads it over MCP with a cursor,
instead of spending inference walking channels and repos to find out whether anything
needs it.

```
 claude.ai / Manus
        │  OAuth bearer
        ▼
 mailroom  (TypeScript door, public)
   /authorize     consent for the agent named in the request
   /callback      Discord sign-in, through the core
   /<agent>/mcp   token check, then forward
        │ service binding (the door's only one)
        ▼
 mailroom-core  (Rust, no public URL) ──▶ Feed (internal) of discord-broker[-manus]
   fetch      /signin/client, /signin ─▶   and github-broker, by service bindings
              the agent's own broker
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
| `Feed` entrypoint in [discord-broker](https://github.com/CycleFive/brokers/tree/master/brokers/discord) and [github-broker](https://github.com/CycleFive/brokers/tree/master/brokers/github) | TypeScript | no | nothing new |

`crates/logic/` is the pure-Rust part (records, config, filters, pollers, MCP protocol).
It has no Workers dependency, so its tests run with plain `cargo test`.

The brokers must be at version 0.2.0 or later: that version adds the internal `Feed`
entrypoint mailroom reads through. `Feed` is read-only and has no public URL. It is
reachable only through a service binding from a Worker in the same Cloudflare account, and
the brokers' allowlists (`DISCORD_ALLOWED_GUILDS`, `DISCORD_ALLOWED_CHANNELS`) apply to it.

Each agent has its own address, `<PUBLIC_URL>/<agent>/mcp`, and signs in through its own
Discord application: the door asks the core, and the core forwards to the broker named by
the agent's `signin` key. The door has no binding to any broker.

The door is TypeScript only because `@cloudflare/workers-oauth-provider`, the tested OAuth
2.1 authorization server, exists in no other language. It holds no mailbox logic.

## Records

Every item in every inbox has this shape:

```json
{"seq": 12, "ts_ingest": "2026-11-18T10:05:00Z", "src": "discord", "kind": "message",
 "id": "dc:123456789012345678", "ts": "2026-11-18T10:04:40.123000+00:00", "actor": "Nyx",
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
| `title`, `body` | An excerpt: Discord content up to 2000 characters (attachment and embed summary lines are appended after the cut), GitHub bodies up to 600 with ` [...]`. |
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
| `routed:assigned` | GitHub: the agent is an assignee, or a trusted person put `assigned:<tag>` on the item (see Work handed over by label). |
| `routed:review` | GitHub: the PR carries `review_label` or the agent is a requested reviewer, or a trusted person put `review:<tag>` on the PR (see Work handed over by label). |
| `mine` | GitHub: a comment, review or event on an issue or PR the agent authored. |
| `claimed` | GitHub: activity on an item handed to the agent by label (see Work handed over by label). |
| `released` | GitHub: the closing note of such an item. Drop the claim. |
| `untrusted-label` | GitHub: the label was applied by someone GitHub does not report as having triage, write, maintain or admin access to that repository, or by any app or bot. Not followed. |

Tags starting `routed:` are addressed to the agent. `mine` is activity on its own work.
`claimed`, `released` and `untrusted-label` do not start with `routed:`, so they do not
call for attention by themselves: they inform. `released` tells the agent to drop a claim.
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
| `agents` | required | Map of agent name to `{ "signin": "...", "sources": [...] }`. Must not be empty. Names are 1-32 characters of `a-z`, `0-9`, `-`. |

**Agent**

| Field | Default | Rule |
| --- | --- | --- |
| `signin` | required | Name of a service binding to a discord-broker `Feed`, the broker whose Discord application this agent signs in through. Non-empty, only `A-Z`, `0-9`, `_`. It need not be a broker the agent also polls. |
| `sources` | required | The sources below. |

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
| `repos` | required | `owner/name`. No duplicates. Not empty unless `labels` is set. |
| `every_secs` | `300` | Minimum `60`. |
| `review_label` | none | A label that means "review this". When set it must not be empty, and with `labels` it must not be `assigned:<tag>` or `review:<tag>` (in any case): those labels already route, with a trust check. |
| `labels` | none | `{ "org": "your-org", "tag": "claude" }`. Also finds work handed over by label across the whole organisation; see Work handed over by label. `tag` is 1-32 characters of `a-z`, `0-9`, `-`. |
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
      "signin": "DISCORD_CLAUDE",
      "sources": [
        { "type": "discord", "broker": "DISCORD_CLAUDE", "every_secs": 120,
          "channels": ["123456789012345678", "123456789012345679"],
          "reactions": true, "skip_self": true, "keep": "all",
          "rules": [
            { "when": { "kind": ["reaction"], "actor_not": ["Alice"] }, "then": "drop" },
            { "when": { "body_contains": ["deploy"] }, "then": { "tag": "deploy" } }
          ] },
        { "type": "github", "broker": "GITHUB", "every_secs": 300,
          "repos": ["your-org/your-repo"], "review_label": "needs-review",
          "skip_self": true, "keep": "tagged", "rules": [] }
      ]
    },
    "manus": {
      "signin": "DISCORD_MANUS",
      "sources": [
        { "type": "discord", "broker": "DISCORD_MANUS",
          "channels": ["123456789012345678"] }
      ]
    }
  }
}
```

## Work handed over by label

A person hands work to an agent by labelling an item. With `labels` set on a GitHub source
(for example `"labels": { "org": "your-org", "tag": "claude" }`), mailroom searches the
whole organisation, not only `repos`, for two labels:

- `assigned:<tag>` on an open issue or pull request (`assigned:claude`);
- `review:<tag>` on an open pull request (`review:claude`).

`<tag>` belongs to one mailbox. The source gets one more poll target,
`github:<broker>:labels:<org>`, on the source's `every_secs`; `mailbox_info` lists it like
any other. Put `labels` on the same source that lists the organisation's repositories in
`repos`: only that source's repo polls learn which items are claimed, and so only they
tag the claimed activity.

Label names match in any case (`Assigned:Claude` is `assigned:claude`); the claim record
shows the name as GitHub has it.

**Who is trusted.** The label counts only when the person who applied it can write to the
repository: GitHub reports their `permission` as `admin` or `write`, or their role as
`admin`, `maintain`, `write` or `triage`. Anyone else, and anyone GitHub cannot find, is
untrusted. A GitHub App's bot is never a trusted labeler (GitHub reports no access for
it, and a login containing `[bot]` is untrusted without a lookup), so one agent cannot
hand work to another. There is no allowlist. A failed permission lookup fails the poll
and is retried; it never decides trust.

Trust is decided per label, by the item's events and never by the search alone. For each
label that the search shows, the newest `labeled` event decides, and it counts only if no
later `unlabeled` event removed it, even when the search still lists the item. An item is
trusted while at least one label that counts was applied by a trusted person. If a
trusted claim stops being trusted (its trusted label was removed while an untrusted one
stays, or it was removed and put back by an untrusted person), the agent gets a closing
note, `released: label removed`, and the claim is kept as untrusted: not followed, no
more activity.

**What the agent receives.**

- A *claim record* when a trusted person first puts the label on an item: kind `event`,
  title `labeled <label>`, the item's title as body, the labeler as actor, and
  `routed:assigned` (for `assigned:<tag>`) or `routed:review` (for `review:<tag>`). This is
  the mail that calls for attention. If the labeler is untrusted the record carries only
  `untrusted-label`, and the item is not followed.
- While the label stays, the item's activity (comments, review comments, reviews and
  `assigned`, `labeled`, `review_requested`, `closed`, `merged`, `reopened` events), tagged
  `claimed` after any other tags (`routed:mention`, `mine`). This includes repositories
  that are not in `repos`. In a repository that is in `repos`, the poll that makes the
  claim (or makes it trusted) mails what happened since the trusted label, once; from then
  on the per-repo poll carries the activity and adds `claimed` the same way. Nothing
  older than the trusted label is mailed; activity at or after it is, and the inbox drops
  repeats.
- A *closing note* when the item closes, loses its label, or is gone: title
  `released: closed`, `released: label removed` or `released: gone`, tag `released`. Drop
  the claim. Putting the label back is a new assignment with a fresh claim record. While a
  trusted label still stands, removing the other label mails nothing; when the trusted one
  is removed and only an untrusted one remains, the closing note is written.
- Reopening: an item released by closing and then reopened with its label still on it is
  handed over again. Its claim record is keyed to the label event and the reopen together
  and carries the reopen's time; the closing note after it is keyed the same way.

Nothing whose actor is the mailbox's own bot is mailed when `skip_self` is true. Items
already labelled when `labels` is first turned on are mailed once, on the first poll.

**Limits.** At most 20 new or changed items are read per poll (the longest-waiting first;
the rest wait for the next poll), and at most 20 vanished claims are checked per poll.
Once a poll has made 60 requests after the search it starts no further item or lookup;
what was not reached waits for the next poll with its state untouched. Events, comments,
review comments and reviews are read in pages of 100, at most 10 pages. If an item's
events fill all ten pages the newest are missing, so nothing is decided for that item in
that poll and a `gap` record says so; a tenth full page of comments adds a `gap` record
too. An item that was moved to another repository is released as `gone`. The
search reads one page of 100; more matches produce a `gap` record. The broker's `Feed`
must allow `/search/issues` (brokers 0.5.0 or later); against an older broker only the
labels target fails.

Example (a mailbox watching one repository and the whole organisation):

```jsonc
{ "type": "github", "broker": "GITHUB", "repos": ["your-org/your-repo"],
  "keep": "tagged", "labels": { "org": "your-org", "tag": "claude" } }
```

## What is polled, and what is missed

Polling is REST only: a cron trigger fires every minute, and each target (a channel or a
repo) is polled when it is next due, about `every_secs` after its last attempt. There is no gateway listener and no
webhook.

- **First contact emits nothing.** The first poll of a target records its current position
  and produces no records. History is never replayed. The labels target is the exception:
  items already labelled are mailed on its first poll.
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
- **GitHub.** Issues, pulls, issue comments, review comments, reviews and issue events.
  Reviews are found by their submission time since the last check. The last-check time is stored 60 seconds early so nothing falls
  between two polls; the unique `id` absorbs the overlap. Workflow runs are not recorded.
- **Limits.** Each GitHub list is read 50 items per poll. If a list comes back full and
  every item on it is new, one `gap` record names the list and repo (items may have been
  skipped; look at the repo directly). Reviews are read 100 per page, up to 10 pages per
  pull request, with a `gap` record if that is not enough. Discord reactor lists are
  capped at 100 per message, and reactions from burst reactors are not recorded.
  A `gap` record is never dropped by `rules` or `keep`.
- **Failures.** A failed target changes nothing except its status (time and error text),
  and the target is retried when it is next due (after its own `every_secs`), from the
  same position. Targets fail independently. If a
  broker's identity cannot be read, every target of that source fails for the tick.

## Setup

You need:

- Rust (stable) with the wasm target: `rustup target add wasm32-unknown-unknown`
- `worker-build`: `cargo install worker-build --version 0.9.0 --locked`
- Node 22.12 or newer
- A Cloudflare account
- discord-broker and github-broker, both at 0.2.0 or later, already set up

Each agent signs in through the Discord application of the broker named by its `signin`
key, so that application needs the door's redirect (step 4).

### 1. Deploy the brokers' `Feed`

Merge and deploy discord-broker (and its `--env manus` copy, if you run one) and
github-broker at 0.2.0 or later. Nothing else changes for them.

### 2. Deploy `mailroom-core`

```sh
cd crates/worker
cp wrangler.example.jsonc wrangler.jsonc
```

Edit `wrangler.jsonc`: fill in `MAILROOM` (see Configuration), and make sure there is one
`services` entry per broker named in it (including each agent's `signin` broker), each
with `"entrypoint": "Feed"`. Then:

```sh
npx wrangler@4 login
npx wrangler@4 deploy
```

This crate has no `package.json`, so `npx wrangler@4` fetches Wrangler on the fly; the
`@4` pins the major version.

The Worker is named `mailroom-core`. It has no public URL (`workers_dev` and `preview_urls` are
both false, and there are no routes); the door reaches it through a service binding. It also runs a cron
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

The `MAILROOM` service binding points at `mailroom-core`. It is the door's only binding:
the Discord client ID and the code exchange go through the core to each agent's broker.
The door holds no secrets: the Discord client secrets stay in the brokers.

```sh
npx wrangler deploy
```

It is deployed as `mailroom`, so the MCP URL of an agent is
`https://mailroom.<subdomain>.workers.dev/<agent>/mcp`. Put that origin in `PUBLIC_URL`
and deploy again. The door learns the agent list from the core and keeps it for at most 60
seconds, so adding an agent never needs a door deploy.

**Going back to 0.1.0.** Use `npx wrangler@4 rollback` (in `door/` and in `crates/worker`):
a Worker version carries its own settings. Redeploying 0.1.0 code against a config that
has `signin` fails, because 0.1.0 rejects the unknown key.

### 4. Add the Discord redirect

On the Discord application of each agent's `signin` broker, open OAuth2 and add the
Redirect `https://mailroom.<subdomain>.workers.dev/callback`. It must match exactly, with
no trailing slash. Every agent uses this same URL, so each agent's application lists it
once.

### 5. Connect

Give each agent its own address, `https://mailroom.<subdomain>.workers.dev/<agent>/mcp`,
for example `https://mailroom.<subdomain>.workers.dev/claude/mcp`. Use the address exactly
as written: no trailing slash, and no explicit `:443`.

- **claude.ai**: Settings, Connectors, Add custom connector. Give the agent's URL
  exactly, and leave Advanced settings blank.
- **Claude Code**: `claude mcp add --transport http mailroom https://mailroom.<subdomain>.workers.dev/<agent>/mcp`
- **Manus**: add a custom MCP server with the agent's URL, and complete the sign-in.

You see the consent page, which names the mailbox and the client (check both), then
Discord's "authorize" screen asking only for your username. The Discord application is
the agent's own. The address names the mailbox, so there is nothing to pick, and a token
for one agent is refused at another agent's address. Check `agent` in `mailbox_info`
after connecting.

A client that does not say which address it wants (it sends no `resource` parameter)
gets a page listing the mailboxes instead. Picking one continues as above.

### Adding an agent

1. Deploy its brokers.
2. On `mailroom-core`: add the service bindings and the agent's entry (with `signin`),
   then deploy.
3. On the agent's Discord application: add the redirect `<PUBLIC_URL>/callback`.
4. Give the agent the address `<PUBLIC_URL>/<agent>/mcp`.

The door is not edited or redeployed.

If you run the brokers from [CycleFive/brokers](https://github.com/CycleFive/brokers), its
mailbox tool does steps 1 and 2 from one manifest: `npm run mailbox -- deploy <agent> --mailroom`
there deploys the agent's brokers, then writes `crates/worker/wrangler.jsonc` here (the
`services` and the `MAILROOM` var, on top of `wrangler.example.jsonc`) and deploys
`mailroom-core`. A `wrangler.jsonc` that starts with a "Generated by" comment is written by
that tool: change the manifest, not the file. Steps 3 and 4 are still done by hand; the
command prints the redirect and the address.

## If something is wrong

| You see | Usually |
| --- | --- |
| `mailbox_info` shows a `last_error` on a target | That target's last poll failed; the position has not moved and the target is retried when it is next due (after its own `every_secs`). The text says whether the broker, the allowlist or Discord/GitHub refused. Discord sometimes answers Workers with a 429 because of other traffic on a shared IP; it usually clears. |
| HTTP 500 `mailroom config: ...` on every call to the core, or "The mailroom is misconfigured" (the door shows it as 502) | The `MAILROOM` var is invalid. While the core is unavailable or misconfigured, every door address except `/` answers 502 (the OAuth endpoints, `/authorize`, `/callback` and every `/<agent>/mcp`), not only the consent page. The page is generic on purpose; the door logs the core's reason as `agents_unavailable` (`status` and `message`), so read it with `wrangler tail` in `door/`. Fix the config and deploy `mailroom-core`. |
| `404 {"error":"unknown mailbox"}` at `/<agent>/mcp` | The agent is not in the core's config. Check the name, or deploy `mailroom-core` with the agent added (the door picks it up within 60 seconds, sooner when a request names it). |
| `404` saying "The address of a mailbox is ..." | The client used `/mcp`. Use `<PUBLIC_URL>/<agent>/mcp`. |
| `401` at `/<agent>/mcp` with a token that worked elsewhere | A token is bound to the address it was issued for. Connect again at this agent's address. |
| The client reports `invalid_target` when you connect | The `resource` the client sent names no agent, so the door refuses it and sends you back to the client, which shows its own text. Usual causes: a wrong agent name, a trailing slash, an explicit `:443`, or another host. Give the client the exact `<PUBLIC_URL>/<agent>/mcp`. |
| `The mailbox "<name>" is no longer configured.` | The agent was removed from config between your approval and the Discord sign-in. Start again. |
| Discord says "Invalid OAuth2 redirect_uri" | The Redirect on that agent's `signin` broker's application is not exactly `<PUBLIC_URL>/callback`. |
| Discord shows the wrong application at sign-in | The agent's `signin` key names another agent's broker. Fix it in the core's config and deploy `mailroom-core`. |
| The sign-in page says "The mailroom is misconfigured" after approving | The core could not reach the agent's `signin` broker, or the broker refused the code (usually the client secret or the redirect). The page is generic on purpose; the door logs `signin_client_failed` or `signin_exchange_failed` with the status and the core's text. |
| "not in ALLOWED_DISCORD_USERS" | The page names the account and ID; put that ID in the door's var and deploy. |
| `ack` is refused | `seq` must be between the cursor and `last_seq`. |
| A channel produces nothing | First contact emits nothing; check `last_ok` in `mailbox_info`, the broker's channel allowlist, and that the bot can read the channel. |

`npx wrangler@4 tail mailroom-core` (from `crates/worker`) shows one JSON line per poll
(`poll`) and tool call (`tool_call`). `npx wrangler tail` in `door/` shows the door's
sign-in and OAuth events (`oauth_endpoint`, `authorize_error`, `sign_in`, `sign_in_refused`,
`sign_in_cancelled`, `signin_client_failed`, `signin_exchange_failed`, `agent_gone`, `agents_unavailable`, `config_error`, and a few
failure events). The brokers log their own `feed` lines; read those with `wrangler tail`
in each broker.

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
cd door && npm test && npm run typecheck # sign-in flow, per-agent addresses, header stripping
```

The smoke test needs `worker-build`, `curl` and `jq`. The door's tests run the whole
sign-in against a fake `mailroom-core` service binding; nothing touches a real
Discord or GitHub.

CI (`.github/workflows/ci.yml`) runs `cargo fmt --check`, clippy, `cargo test`, the wasm
build of `crates/worker`, and the door's typecheck and tests.

Layout: `crates/logic/src/` (`config.rs`, `record.rs`, `filter.rs`, `discord.rs`,
`github.rs`, `feed.rs`, `time.rs`, and the MCP protocol), `crates/worker/` (the
`Mailbox` Durable Object, SQLite storage, the service-binding `Feed` client, the fetch and
scheduled handlers), `door/src/` (`index.ts` routing and forwarding, `auth.ts` consent and
sign-in, `env.ts`).

Not covered: a Discord gateway listener, GitHub webhooks, workflow-run records, automatic
thread discovery, editing filters at runtime, and per-agent lists of who may sign in
(`ALLOWED_DISCORD_USERS` is one list on the door).

## License

MIT, see [LICENSE](LICENSE).
