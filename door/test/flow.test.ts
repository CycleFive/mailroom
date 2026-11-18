// End-to-end through the real Worker entry point and the real OAuth library:
// client registration -> consent for the mailbox named by `resource` -> Discord sign-in
// (through a fake mailroom-core) -> token -> MCP calls at /<agent>/mcp forwarded to the fake
// core. KV is in memory.

import { createHash, randomBytes } from "node:crypto";
import { beforeEach, describe, expect, it, vi } from "vitest";
import worker, { resetAgentCache } from "../src/index";
import type { Env } from "../src/env";
import { CLIENT_ID, FakeCore, MemoryKV } from "./fakes";

vi.spyOn(console, "log").mockImplementation(() => {});
vi.spyOn(console, "warn").mockImplementation(() => {});

const BASE = "https://door.test";
const REDIRECT = "https://claude.ai/api/mcp/auth_callback";
const LOTHROP = "800000000000000001";
const MANUS_CLIENT_ID = "223456789012345678";

let core: FakeCore;

beforeEach(() => {
  core = new FakeCore();
  resetAgentCache();
  vi.useRealTimers();
});

function env(extra: Partial<Env> = {}): Env {
  return {
    OAUTH_KV: new MemoryKV() as unknown as KVNamespace,
    MAILROOM: core.fetcher,
    PUBLIC_URL: BASE,
    ALLOWED_DISCORD_USERS: LOTHROP,
    ...extra,
  };
}

const ctx = { waitUntil: () => {}, passThroughOnException() {}, props: {} } as unknown as ExecutionContext;
const call = (e: Env, path: string, init?: RequestInit) => worker.fetch(new Request(BASE + path, init), e, ctx);
const res = (agent: string) => `${BASE}/${agent}/mcp`;

function cookieHeader(res: Response, prior = ""): string {
  const jar = new Map(prior.split("; ").filter(Boolean).map((c) => c.split("=", 2) as [string, string]));
  for (const sc of res.headers.getSetCookie()) {
    const [nv] = sc.split(";");
    const i = nv.indexOf("=");
    jar.set(nv.slice(0, i), nv.slice(i + 1));
  }
  return [...jar].map(([k, v]) => `${k}=${v}`).join("; ");
}

interface Started {
  client_id: string;
  verifier: string;
  query: URLSearchParams;
  status: number;
  handle: string;
  cookies: string;
  html: string;
}

async function register(e: Env): Promise<string> {
  const r = await call(e, "/oauth/register", {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify({ client_name: "Claude", redirect_uris: [REDIRECT], token_endpoint_auth_method: "none" }),
  });
  expect(r.status).toBe(201);
  return ((await r.json()) as { client_id: string }).client_id;
}

function authQuery(client_id: string, verifier: string, resource?: string): URLSearchParams {
  const q = new URLSearchParams({
    response_type: "code",
    client_id,
    redirect_uri: REDIRECT,
    scope: "mailroom offline_access",
    state: "st8",
    code_challenge: createHash("sha256").update(verifier).digest("base64url"),
    code_challenge_method: "S256",
  });
  if (resource) q.set("resource", resource);
  return q;
}

/** GET /authorize with the given query; when it answers a consent page, reads the handle and cookies. */
async function authorizeWith(e: Env, client_id: string, verifier: string, query: URLSearchParams): Promise<Started> {
  const consent = await call(e, `/authorize?${query}`);
  const html = await consent.text();
  const handle = html.match(/name="handle" value="([^"]+)"/)?.[1] ?? "";
  return { client_id, verifier, query, status: consent.status, handle, cookies: cookieHeader(consent), html };
}

async function startAuthorize(e: Env, agent = "claude"): Promise<Started> {
  const client_id = await register(e);
  const verifier = randomBytes(32).toString("base64url");
  return authorizeWith(e, client_id, verifier, authQuery(client_id, verifier, res(agent)));
}

const approve = (e: Env, s: Started, fields: Record<string, string> = {}) =>
  call(e, "/authorize", {
    method: "POST",
    headers: { "content-type": "application/x-www-form-urlencoded", cookie: s.cookies },
    body: new URLSearchParams({ handle: s.handle, decision: "approve", ...fields }).toString(),
  });

/** Approve a started authorization and run the Discord callback. */
async function finish(e: Env, s: Started, expectedClientId: string) {
  const ap = await approve(e, s);
  expect(ap.status).toBe(302);
  const dc = new URL(ap.headers.get("location")!);
  expect(dc.origin + dc.pathname).toBe("https://discord.com/oauth2/authorize");
  expect(dc.searchParams.get("redirect_uri")).toBe(`${BASE}/callback`);
  expect(dc.searchParams.get("scope")).toBe("identify");
  expect(dc.searchParams.get("client_id")).toBe(expectedClientId);
  const cookies = cookieHeader(ap, s.cookies);
  return call(e, `/callback?code=dc_code&state=${encodeURIComponent(dc.searchParams.get("state")!)}`, { headers: { cookie: cookies } });
}

async function signIn(e: Env, agent = "claude") {
  const s = await startAuthorize(e, agent);
  expect(s.status).toBe(200);
  const cb = await finish(e, s, core.clientIds[agent]);
  return { cb, s };
}

async function exchange(e: Env, s: Started, cb: Response, resource?: string): Promise<Response> {
  expect(cb.status).toBe(302);
  const code = new URL(cb.headers.get("location")!).searchParams.get("code")!;
  const fields: Record<string, string> = { grant_type: "authorization_code", code, redirect_uri: REDIRECT, client_id: s.client_id, code_verifier: s.verifier };
  if (resource) fields.resource = resource;
  return call(e, "/oauth/token", {
    method: "POST",
    headers: { "content-type": "application/x-www-form-urlencoded" },
    body: new URLSearchParams(fields).toString(),
  });
}

async function token(e: Env, agent = "claude"): Promise<string> {
  const { cb, s } = await signIn(e, agent);
  const tok = await exchange(e, s, cb, res(agent));
  expect(tok.status).toBe(200);
  return ((await tok.json()) as { access_token: string }).access_token;
}

const mcp = (e: Env, t: string, headers: Record<string, string> = {}, agent = "claude") =>
  call(e, `/${agent}/mcp`, {
    method: "POST",
    headers: { authorization: `Bearer ${t}`, "content-type": "application/json", accept: "application/json, text/event-stream", ...headers },
    body: JSON.stringify({ jsonrpc: "2.0", id: 1, method: "tools/list" }),
  });

describe("address per agent", () => {
  it("answers 401 naming the agent's own metadata when there is no token", async () => {
    const e = env();
    const r = await call(e, "/claude/mcp", { method: "POST", body: "{}" });
    expect(r.status).toBe(401);
    expect(r.headers.get("www-authenticate")).toContain(`${BASE}/.well-known/oauth-protected-resource/claude/mcp`);
    const bad = await call(e, "/claude/mcp", { method: "POST", headers: { authorization: "Bearer nope" }, body: "{}" });
    expect(bad.status).toBe(401);
  });

  it("serves each agent's resource metadata", async () => {
    const e = env();
    const meta = (await (await call(e, "/.well-known/oauth-protected-resource/claude/mcp")).json()) as any;
    expect(meta.resource).toBe(res("claude"));
    expect(meta.resource_name).toBe("Mailroom: claude");
    const other = (await (await call(e, "/.well-known/oauth-protected-resource/manus/mcp")).json()) as any;
    expect(other.resource).toBe(res("manus"));
  });

  it("answers 404 for an unknown mailbox", async () => {
    const e = env();
    const r = await call(e, "/ghost/mcp", { method: "POST", body: "{}" });
    expect(r.status).toBe(404);
    expect(await r.json()).toEqual({ error: "unknown mailbox" });
    expect((await call(e, "/.well-known/oauth-protected-resource/ghost/mcp")).status).toBe(404);
  });

  it("answers 404 at /mcp and says where the address is", async () => {
    const r = await call(env(), "/mcp", { method: "POST", body: "{}" });
    expect(r.status).toBe(404);
    expect(await r.text()).toBe(`The address of a mailbox is ${BASE}/<agent>/mcp.\n`);
  });

  it("serves a banner", async () => {
    expect(await (await call(env(), "/")).text()).toBe(`mailroom 0.2.0: the MCP address of a mailbox is ${BASE}/<agent>/mcp\n`);
  });

  it("keeps the authorization server metadata", async () => {
    const meta = (await (await call(env(), "/.well-known/oauth-authorization-server")).json()) as any;
    expect(meta.issuer).toBe(BASE);
    expect(meta.protected_resources).toEqual([res("claude"), res("manus")]);
  });

  it("notices a dropped agent once the cache window has passed, and keeps the others", async () => {
    vi.useFakeTimers({ toFake: ["Date"] });
    const e = env();
    const tClaude = await token(e, "claude");
    const tManus = await token(e, "manus");
    expect((await mcp(e, tManus, {}, "manus")).status).toBe(200);
    core.agents = ["claude"];
    vi.setSystemTime(Date.now() + 61_000);
    const gone = await mcp(e, tManus, {}, "manus");
    expect(gone.status).toBe(404);
    expect(await gone.json()).toEqual({ error: "unknown mailbox" });
    expect((await mcp(e, tClaude, {}, "claude")).status).toBe(200);
  });

  it("refetches at once for an agent missing from the cache, and caches otherwise", async () => {
    const e = env();
    await call(e, "/claude/mcp", { method: "POST", body: "{}" });
    await call(e, "/claude/mcp", { method: "POST", body: "{}" });
    expect(core.agentsCalls).toBe(1);
    core.agents = ["claude", "manus", "newcomer"];
    const r = await call(e, "/newcomer/mcp", { method: "POST", body: "{}" });
    expect(r.status).toBe(401);
    expect(core.agentsCalls).toBe(2);
  });
});

describe("authorization names the mailbox", () => {
  it("shows the mailbox named by resource, with no choice", async () => {
    const s = await startAuthorize(env(), "manus");
    expect(s.status).toBe(200);
    expect(s.html).toContain("<strong>manus</strong>");
    expect(s.html).not.toContain('type="radio"');
    expect(s.html).toContain("Only continue if you started this yourself, from the app you are connecting, and you recognise that host.");
  });

  it("sends the browser to Discord with that agent's client ID and asks the core for it", async () => {
    const e = env();
    const s = await startAuthorize(e, "manus");
    const cb = await finish(e, s, MANUS_CLIENT_ID);
    expect(cb.status).toBe(302);
    expect(core.signin).toEqual([
      { path: "/signin/client", method: "GET", query: "?agent=manus" },
      { path: "/signin", method: "POST", body: { agent: "manus", code: "dc_code", redirect_uri: `${BASE}/callback` } },
    ]);
  });

  it("takes the agent from the stored request, not from a form field", async () => {
    const e = env();
    const s = await startAuthorize(e, "manus");
    const ap = await approve(e, s, { agent: "claude" });
    expect(new URL(ap.headers.get("location")!).searchParams.get("client_id")).toBe(MANUS_CLIENT_ID);
  });

  it("refuses a resource that names no mailbox, without a consent page", async () => {
    for (const resource of [res("ghost"), "https://elsewhere.example/claude/mcp"]) {
      const e = env();
      const client_id = await register(e);
      const verifier = randomBytes(32).toString("base64url");
      const r = await call(e, `/authorize?${authQuery(client_id, verifier, resource)}`);
      expect(r.status).toBeGreaterThanOrEqual(300);
      expect(await r.text()).not.toContain('name="handle"');
    }
  });

  it("lists the mailboxes when the client sent no resource, and completes through the link", async () => {
    const e = env();
    const client_id = await register(e);
    const verifier = randomBytes(32).toString("base64url");
    const q = authQuery(client_id, verifier);
    const list = await call(e, `/authorize?${q}`);
    expect(list.status).toBe(200);
    const html = await list.text();
    expect(html).not.toContain('name="handle"');
    const links = [...html.matchAll(/href="(\/authorize\?[^"]+)"/g)].map((m) => new URL(m[1].replaceAll("&amp;", "&"), BASE));
    expect(links).toHaveLength(2);
    const manus = links.find((u) => u.searchParams.get("resource") === res("manus"))!;
    expect(links.map((u) => u.searchParams.get("resource")).sort()).toEqual([res("claude"), res("manus")]);
    for (const [k, v] of q) expect(manus.searchParams.get(k)).toBe(v);

    const s = await authorizeWith(e, client_id, verifier, manus.searchParams);
    expect(s.status).toBe(200);
    expect(s.html).toContain("<strong>manus</strong>");
    const cb = await finish(e, s, MANUS_CLIENT_ID);
    // The token request carries no resource either; the library takes it from the grant.
    const tok = await exchange(e, s, cb);
    expect(tok.status).toBe(200);
    const t = ((await tok.json()) as { access_token: string }).access_token;
    expect((await mcp(e, t, {}, "manus")).status).toBe(200);
    expect(core.seen[0].headers.get("x-mailroom-agent")).toBe("manus");
    expect((await mcp(e, t, {}, "claude")).status).toBe(401);
  });

  it("escapes agent names on the list page", async () => {
    core.agents = ["claude"];
    const list = await call(env(), "/authorize?response_type=code&state=%22%3E%3Cb%3E");
    const html = await list.text();
    expect(html).not.toContain("<b>");
  });
});

describe("agents added or named oddly", () => {
  it("reaches the consent page of an agent added within the cache window", async () => {
    core.agents = ["claude"];
    const e = env();
    await call(e, "/claude/mcp", { method: "POST", body: "{}" });
    core.agents = ["claude", "manus"];
    const s = await startAuthorize(e, "manus");
    expect(s.status).toBe(200);
    expect(s.html).toContain("<strong>manus</strong>");
  });

  it("serves an agent named mcp at /mcp/mcp, while /mcp still explains", async () => {
    core.agents = ["claude", "mcp"];
    const e = env();
    const r = await call(e, "/mcp/mcp", { method: "POST", body: "{}" });
    expect(r.status).toBe(401);
    expect(r.headers.get("www-authenticate")).toContain(`${BASE}/.well-known/oauth-protected-resource/mcp/mcp`);
    const plain = await call(e, "/mcp", { method: "POST", body: "{}" });
    expect(plain.status).toBe(404);
    expect(await plain.text()).toContain("The address of a mailbox is");
  });
});

describe("failures of the core", () => {
  it("shows a generic message when the agent list is unavailable, never the core's text", async () => {
    core.agentsFailure = { status: 500, message: "mailroom config: secret-channel-123" };
    const r = await call(env(), "/authorize?response_type=code");
    expect(r.status).toBe(502);
    const html = await r.text();
    expect(html).toContain("The mailroom is misconfigured. The operator can find the reason in the mailroom door&#39;s logs.");
    expect(html).not.toContain("secret-channel-123");
    const m = await call(env(), "/claude/mcp", { method: "POST", body: "{}" });
    expect(m.status).toBe(502);
    expect(await m.text()).not.toContain("secret-channel-123");
  });

  it("logs the core's reason when the agent list is unavailable, but never shows it", async () => {
    const log = vi.mocked(console.log);
    log.mockClear();
    core.agentsFailure = { status: 500, message: "mailroom config: agents: must not be empty" };
    const r = await call(env(), "/claude/mcp", { method: "POST", body: "{}" });
    expect(r.status).toBe(502);
    expect(await r.text()).not.toContain("must not be empty");
    const lines = log.mock.calls.map((c) => String(c[0])).filter((l) => l.startsWith("{")).map((l) => JSON.parse(l));
    const line = lines.find((l) => l.event === "agents_unavailable");
    expect(line.status).toBe(500);
    expect(line.message).toContain("must not be empty");
  });

  for (const status of [500, 502]) {
    it(`shows a generic page when /signin/client answers ${status}, and stores no grant`, async () => {
      const e = env();
      core.clientFailure = { status, message: "mailroom config: secret-binding-9" };
      const s = await startAuthorize(e);
      const ap = await approve(e, s);
      expect(ap.status).toBe(502);
      const html = await ap.text();
      expect(html).toContain("The mailroom is misconfigured.");
      expect(html).not.toContain("secret-binding-9");
      expect([...(e.OAUTH_KV as unknown as MemoryKV).m.keys()].filter((k) => k.startsWith("grant:"))).toEqual([]);
    });
  }

  it("shows a generic page when /signin answers 502, and stores no grant", async () => {
    const e = env();
    core.failure = { status: 502, message: "Discord did not return a token" };
    const { cb } = await signIn(e);
    expect(cb.status).toBe(502);
    const html = await cb.text();
    expect(html).toContain("The mailroom is misconfigured.");
    expect(html).not.toContain("Discord did not return a token");
    expect([...(e.OAUTH_KV as unknown as MemoryKV).m.keys()].filter((k) => k.startsWith("grant:"))).toEqual([]);
  });
});

describe("sign-in and forwarding", () => {
  it("signs in end to end and stores the agent in the grant", async () => {
    const e = env();
    const t = await token(e, "claude");
    const r = await mcp(e, t);
    expect(r.status).toBe(200);
    expect(await r.json()).toEqual({ jsonrpc: "2.0", id: 1, result: { ok: true } });
    expect(core.seen).toHaveLength(1);
    expect(core.seen[0].url).toBe("https://mailroom-core/mcp");
    expect(core.seen[0].method).toBe("POST");
    expect(core.seen[0].headers.get("x-mailroom-agent")).toBe("claude");
    expect(core.seen[0].headers.get("x-mailroom-user")).toBe(`lothrop (${LOTHROP})`);
    expect(core.seen[0].body).toContain("tools/list");
  });

  it("a token for one agent works there and gets 401 at another", async () => {
    const e = env();
    const t = await token(e, "manus");
    expect((await mcp(e, t, {}, "manus")).status).toBe(200);
    expect(core.seen[0].headers.get("x-mailroom-agent")).toBe("manus");
    expect((await mcp(e, t, {}, "claude")).status).toBe(401);
  });

  it("refuses a Discord user outside the allowlist", async () => {
    core.user = { id: "800000000000000007", username: "stranger" };
    const { cb } = await signIn(env());
    expect(cb.status).toBe(403);
    expect(await cb.text()).toContain("800000000000000007");
  });

  it("refuses at the callback when the agent has since been removed", async () => {
    vi.useFakeTimers({ toFake: ["Date"] });
    const e = env();
    const s = await startAuthorize(e, "manus");
    const ap = await approve(e, s);
    const state = new URL(ap.headers.get("location")!).searchParams.get("state")!;
    core.agents = ["claude"];
    vi.setSystemTime(Date.now() + 61_000);
    const cb = await call(e, `/callback?code=c&state=${encodeURIComponent(state)}`, { headers: { cookie: cookieHeader(ap, s.cookies) } });
    expect(cb.status).toBe(400);
    expect(await cb.text()).toContain("no longer configured");
  });

  it("re-checks the allowlist on every MCP request", async () => {
    const e = env();
    const t = await token(e);
    e.ALLOWED_DISCORD_USERS = "800000000000000009";
    expect((await mcp(e, t)).status).toBe(403);
    expect(core.seen).toHaveLength(0);
  });

  it("replaces client-supplied x-mailroom headers", async () => {
    const e = env();
    const t = await token(e, "manus");
    await mcp(e, t, { "x-mailroom-agent": "claude", "x-mailroom-evil": "1", "X-Mailroom-User": "evil (1)" }, "manus");
    const h = core.seen[0].headers;
    expect(h.get("x-mailroom-agent")).toBe("manus");
    expect(h.get("x-mailroom-user")).toBe(`lothrop (${LOTHROP})`);
    expect(h.get("x-mailroom-evil")).toBeNull();
  });

  it("does not forward the bearer token or cookies to the core", async () => {
    const e = env();
    const t = await token(e);
    await mcp(e, t, { cookie: "session=secret" });
    const h = core.seen[0].headers;
    expect(h.get("authorization")).toBeNull();
    expect(h.get("cookie")).toBeNull();
  });

  it("rejects a disallowed Origin with 403", async () => {
    const e = env({ ALLOWED_ORIGINS: "https://claude.ai" });
    const t = await token(e);
    const r = await mcp(e, t, { origin: "https://evil.example" });
    expect(r.status).toBe(403);
    expect(await r.json()).toEqual({ jsonrpc: "2.0", id: null, error: { code: -32600, message: "Origin not allowed" } });
    expect(core.seen).toHaveLength(0);
    expect((await mcp(e, t, { origin: "https://claude.ai" })).status).toBe(200);
  });
});
