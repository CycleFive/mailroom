# Per-agent address and sign-in

Amends `2026-11-18-mailroom-design.md`. Where the two disagree, this document wins.

## Why

Mailroom serves a list of agents. There are two today and there will be many more.
Version 0.1.0 had one MCP address with a mailbox picker, and every sign-in went through
one Discord application (the first agent's). Two things were wrong with that:

- An agent's connector signed in through another agent's Discord application.
- With many agents, every connection depended on a person picking the right mailbox.

## Goals

- Adding an agent is one edit to the core's config and one redeploy of the core. The door
  is not edited or redeployed.
- Each agent has its own MCP address. A connector cannot end up on another agent's mailbox.
- Each agent's sign-in goes through that agent's own Discord application.

## Non-goals

- Per-agent lists of who may sign in. `ALLOWED_DISCORD_USERS` stays one list on the door.
- Keeping the 0.1.0 address `/mcp` working. Nothing was connected to it.

## Approaches considered

1. **A door Worker per agent.** Rejected: each new agent would need a new Worker, a new
   token store and a new config block.
2. **One address with a picker, then the picked agent's sign-in.** Works, but every
   connection depends on the right pick.
3. **One door, an address per agent, sign-in routed by the core.** Chosen. The address
   names the mailbox, so there is nothing to pick and nothing to get wrong.

## Design

### Config

Each agent names the broker that does its sign-in:

```jsonc
"agents": {
  "manus": {
    "signin": "DISCORD_MANUS",        // a Discord broker binding on mailroom-core
    "sources": [ … ]
  }
}
```

`signin` is required. It follows the same rule as a source's `broker` (non-empty, only
`A-Z`, `0-9`, `_`) and must be a service binding on mailroom-core whose `Feed` serves
`/signin/client` and `/signin`. It need not be a broker the agent also polls.

### Addresses

- MCP endpoint of an agent: `PUBLIC_URL/<agent>/mcp`.
- Its metadata: `PUBLIC_URL/.well-known/oauth-protected-resource/<agent>/mcp`.
- Each agent's address is its own OAuth protected resource. A token is bound to the
  resource it was issued for, so a token for one agent is refused at another agent's
  address. The grant also records the agent, and the door checks that it matches the path.
- `/mcp` answers 404 with a line saying the address is `/<agent>/mcp`.
- An unknown agent answers 404 `{"error":"unknown mailbox"}`.

The door learns the agent list from the core (`GET /agents`) and keeps it in memory for
at most 60 seconds, so a redeploy of the core is picked up without touching the door.

### Authorization

- A client names the address it wants with the `resource` parameter (RFC 8707), as MCP
  clients do. The door reads the agent from it. The consent page states the mailbox and
  offers no choice.
- A client that sends no `resource` gets a page listing the mailboxes. Picking one
  re-enters `/authorize` with that agent's `resource` added. This is the only place a
  picker remains.
- After approval the browser goes to Discord with the client ID of **that agent's**
  Discord application. The redirect URI is `PUBLIC_URL/callback` for every agent, so each
  agent's Discord application lists that one URL.

### Sign-in goes through the core

The door has no binding to any broker. It asks the core, which routes by agent:

- `GET /signin/client?agent=<name>` → `200 {"client_id":"…"}`.
- `POST /signin` with `{"agent","code","redirect_uri"}` → `200 {"id","username"}`.
- Unknown agent → `404 {"error":"unknown mailbox"}`. A malformed body → `400`.
  A broker that refuses or fails → `502 {"error":"…"}`. A `signin` binding that is not
  bound on the core → `500 {"error":"mailroom config: …"}`.

The core forwards to the `Feed` of the agent's `signin` broker (`/signin/client`,
`/signin`). It never logs the code.

### Adding an agent

1. Deploy its brokers.
2. On mailroom-core: add the service bindings and the agent's entry (with `signin`), deploy.
3. On the agent's Discord application: add the redirect `PUBLIC_URL/callback`.
4. Give the agent the address `PUBLIC_URL/<agent>/mcp`.

## Left to the builder

Page wording, the in-memory cache's exact form, test structure.
