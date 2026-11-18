# Per-agent address and sign-in Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Give every agent its own MCP address (`/<agent>/mcp`) whose sign-in goes through that agent's own Discord application, with the agent list living only in the core's config.

**Architecture:** Each agent in the core's config names a `signin` broker. The core gains two internal routes that forward sign-in to that broker's `Feed`. The door drops its own broker binding, serves one OAuth protected resource per agent, reads the agent from the `resource` parameter, and asks the core to do the sign-in.

**Tech Stack:** Rust (`mailroom-logic`, `mailroom-worker` on workers-rs `=0.9.0`), TypeScript door on `@cloudflare/workers-oauth-provider` 1.2.4, vitest, wrangler 4.

**Spec:** `docs/superpowers/specs/2026-11-18-per-agent-signin-design.md` (amends `docs/superpowers/specs/2026-11-18-mailroom-design.md`).

## Global Constraints

- Rust: all wire and stored data are typed serde structs/enums. **Never** `serde_json::json!` or `serde_json::Value` for data mailroom owns. Separate types per direction. Prefer `&str` in types built and immediately serialized. A broker's success body that the core only relays is passed through as text, not re-modelled.
- Config types keep `#[serde(deny_unknown_fields)]`.
- `worker` stays pinned `=0.9.0`. The logic crate has no `worker`, `wasm-bindgen` or `js-sys` dependency.
- Never log an OAuth code, a token, or a request body.
- The door holds no secrets and, after this plan, no broker binding.
- The repository is public: examples use placeholder IDs and names only.
- `wrangler.jsonc` is git-ignored; only `wrangler.example.jsonc` files are committed.
- Version is `0.2.0` everywhere it appears.
- Commit messages end with exactly this trailer and no other `Co-Authored-By`:
  `Co-Authored-By: Claude & Lothrop (cycle.five@proton.me)`

## Review Focus

1. **A token for one agent presented at another agent's address.** It must get 401 (audience), never mail. Test in Task 2.
2. **A client that sends no `resource`.** It gets the mailbox list, and after picking, the flow completes and the token works at the picked address, including a token request that also omits `resource`. Test in Task 2.
3. **`resource` naming an agent that is not configured**, or a URL on another host. The authorization is refused; no consent page for a mailbox that does not exist. Test in Task 2.
4. **An agent removed from config while a client holds a token.** Its address answers 404 within the cache window of 60 seconds; other agents are unaffected. Test in Task 2 (cache expiry driven by an injected clock or a reset hook).
5. **Sign-in for an agent whose `signin` broker fails or is not bound.** The door shows an error page and no grant is created; the core answers 502 or 500 as specified. Tests in Tasks 1 and 2.

---

### Task 1: Core — `signin` per agent and the sign-in routes

**Files:**
- Modify: `crates/logic/src/config.rs`, `crates/logic/tests/config.rs`
- Modify: `crates/worker/src/lib.rs` (routes), `crates/worker/src/service_feed.rs` only if a helper belongs there
- Modify: `crates/worker/wrangler.example.jsonc`, `crates/worker/smoke/feed.js`, `crates/worker/smoke/wrangler.core.jsonc`, `crates/worker/smoke/run.sh`
- Modify: `Cargo.toml` files of both crates (version `0.2.0`) and `Cargo.lock`

**Interfaces:**
- Consumes: broker `Feed` over a service binding: `GET https://feed/signin/client` → `200 {"client_id": string}`; `POST https://feed/signin` with `{"code","redirect_uri"}` → `200 {"id","username"}` or an error status with `{"error": string}`.
- Produces (the door relies on these, exactly):
  - `AgentConfig { signin: String, sources: Vec<SourceConfig> }` — `signin` required.
  - `GET /signin/client?agent=<name>` → `200` with the Feed's body relayed.
  - `POST /signin` with JSON `{"agent": string, "code": string, "redirect_uri": string}` → `200` with the Feed's body relayed.
  - Errors for both: unknown or missing agent → `404 {"error":"unknown mailbox"}`; body that is not that JSON shape → `400 {"error":"bad request"}`; Feed answers non-200 or cannot be reached → `502 {"error": <the Feed's error text, else "<binding> answered <status>", at most 200 chars>}`; the `signin` binding is not bound → `500 {"error":"mailroom config: agent \"<name>\": signin binding <BINDING> is not bound"}`.
  - `GET /agents` and `/mcp` are unchanged.

- [ ] **Step 1: Failing config tests** in `crates/logic/tests/config.rs`: a config whose agent has `"signin": "DISCORD_CLAUDE"` parses and `c.agents["claude"].signin == "DISCORD_CLAUDE"`; an agent without `signin` is rejected; `"signin": ""` and `"signin": "discord-claude"` are rejected with an error that names the agent and `signin`. Update every existing fixture in the logic tests so it carries `signin`.
- [ ] **Step 2: Run** `cargo test -p mailroom-logic --test config` — the new tests fail.
- [ ] **Step 3: Implement** the field and its validation in `config.rs`, reusing the rule already applied to a source's `broker`.
- [ ] **Step 4: Run** `cargo test --workspace` — all pass.
- [ ] **Step 5: Extend the smoke test first.** In `smoke/feed.js` add `GET /signin/client` → `{"client_id":"424242"}` and `POST /signin` → `{"id":"77","username":"op"}` when the JSON body is `{"code":"good","redirect_uri":"https://door/callback"}`, else `502 {"error":"Discord did not return a token"}`. Give every agent in `smoke/wrangler.core.jsonc` a `signin`; give one agent a `signin` naming a binding that does not exist. In `run.sh` assert: client for a good agent returns `424242`; sign-in with `good` returns `op`; sign-in with a bad code is `502` with that error text; unknown agent is `404`; a non-JSON body is `400`; the agent with the unbound binding is `500` and the body contains `is not bound`. Run it and keep the failing output.
- [ ] **Step 6: Implement the two routes** in `lib.rs` with typed request and outgoing bodies (`SigninIn` deserialized, `SigninOut<'a>` serialized). They live in the top-level `fetch`, not in the Durable Object.
- [ ] **Step 7: Run** `cargo fmt --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace`, `bash crates/worker/smoke/run.sh` (`smoke: ok`).
- [ ] **Step 8:** Add `signin` to every agent in `crates/worker/wrangler.example.jsonc` with a one-line comment; set both crates to `0.2.0`.
- [ ] **Step 9: Commit.**

### Task 2: Door — an address per agent, sign-in through the core

**Files:**
- Modify: `door/src/index.ts`, `door/src/auth.ts`, `door/src/env.ts`
- Modify: `door/test/flow.test.ts`, `door/test/auth.test.ts`, `door/test/fakes.ts`
- Modify: `door/wrangler.example.jsonc`, `door/package.json`, `door/package-lock.json` (version only)
- Modify: `README.md`, `docs/superpowers/specs/2026-11-18-mailroom-design.md` (a line at the top pointing at the amending spec)

**Interfaces:**
- Consumes from the core over `env.MAILROOM` (base `https://mailroom-core`): `GET /agents` → `{"agents": string[]}`; `GET /signin/client?agent=`; `POST /signin`; errors as listed in Task 1; `/mcp` with `x-mailroom-agent` and `x-mailroom-user` as today.
- Produces:
  - `Env` without `SIGNIN`.
  - Resource of an agent: `` `${PUBLIC_URL}/${agent}/mcp` ``.
  - Routes: `/<agent>/mcp` and its descendants, and `/.well-known/oauth-protected-resource/<agent>/mcp`, served by that agent's `OAuthResourceServer` (`resource_name` `` `Mailroom: ${agent}` ``); unknown agent → `404 {"error":"unknown mailbox"}`; `/mcp` → `404` plain text `The address of a mailbox is <PUBLIC_URL>/<agent>/mcp.`
  - One `OAuthAuthorizationServer` with `resources` = every agent's resource and no `defaultResource`, rebuilt when the agent list changes.
  - The agent list is cached in module memory for at most 60 seconds and refetched at once when a request names an agent that is not in the cached list.

- [ ] **Step 1: Update the fakes.** `fakes.ts`: the fake core answers `/agents`, `/signin/client?agent=`, `/signin` (recording `{path, method, body}` as the old fake sign-in did, with the same `user` and `failure` switches) and `/mcp`. Remove the fake `SIGNIN` binding.
- [ ] **Step 2: Failing tests** in `flow.test.ts` (keep the existing cases, re-pointed at `/<agent>/mcp`):
  - `POST /claude/mcp` without a token → `401`, and `WWW-Authenticate` names `…/.well-known/oauth-protected-resource/claude/mcp`; that metadata document has `resource` `…/claude/mcp`.
  - Authorize with `resource=…/manus/mcp`: the consent page names `manus`, contains no radio input, and after approval the redirect to Discord carries the client ID the fake core returned for `manus`; the core saw `/signin/client?agent=manus` and later `/signin` with `{"agent":"manus","code":…,"redirect_uri":"<BASE>/callback"}`.
  - The finished flow yields a token that works at `/manus/mcp` (the core sees `x-mailroom-agent: manus`) and gets `401` at `/claude/mcp`.
  - Authorize with no `resource`: a page listing every agent, each linking back to `/authorize` with the original query plus that agent's `resource`; following the `manus` link completes as above, and the token request without `resource` still yields a token that works at `/manus/mcp`.
  - Authorize with `resource=…/ghost/mcp` and with `resource=https://elsewhere.example/claude/mcp`: refused, no consent page.
  - A client-sent `x-mailroom-agent: claude` on `/manus/mcp` reaches the core as `manus`.
  - After the fake core drops `manus` from `/agents` and the cache window passes, `/manus/mcp` → `404`; `/claude/mcp` still works.
  - The core answering `500` or `502` on `/signin/client`, and `502` on `/signin`: an error page that does not contain the core's text, and no grant in KV.
  - `/mcp` → `404` with the explanatory line.
- [ ] **Step 3: Run** `npm test` in `door/` — the new tests fail.
- [ ] **Step 4: Implement** in `index.ts` and `auth.ts`:
  - `agentOfResource(env, resource, agents): string | null` — the agent whose resource equals `resource` exactly, else `null`.
  - GET `/authorize` with no `resource` query parameter renders the list page before the library parses the request. With `resource`, the library parses it; the agent is `agentOfResource` of the parsed request's `resource`; `null` → error page, status 400.
  - POST `/authorize` takes the agent from the stored authorization request's `resource`, never from a form field.
  - Sign-in calls go to the core as in Interfaces. The generic `CORE_MISCONFIGURED` page is used for every failure of the core; details go to the structured log.
  - The protected handler keeps its checks and adds: `props.agent` must equal the agent in the path, else `403`.
  - The consent page's warning reads: `Only continue if you started this yourself, from the app you are connecting, and you recognise that host.`
- [ ] **Step 5: Run** `npm run typecheck` and `npm test` — all pass.
- [ ] **Step 6: Docs and config.** `door/wrangler.example.jsonc`: remove `SIGNIN`. `README.md`: the address per agent, the `signin` key in the config reference and example, "Adding an agent" as four steps (spec section of that name), the redirect URL to register on each agent's Discord application, the changed troubleshooting rows, and removal of every statement that the operator picks a mailbox on the consent page (except the no-`resource` case). Put one line at the top of the original spec pointing at the amending spec. Set the door's version to `0.2.0` (`package.json`, lock file, `VERSION`).
- [ ] **Step 7: Commit.**

## After the tasks

Whole-branch review, PR, CI, merge. Deploy `mailroom-core` (with `signin` added to both live agents), then `mailroom` (without `SIGNIN`). Check `/<agent>/mcp` → 401 with per-agent metadata and that the Discord redirect for each agent carries that agent's client ID. Tag `v0.2.0`, release.
