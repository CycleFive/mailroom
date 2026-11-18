// End-to-end through the real Worker entry point and the real OAuth library:
// client registration -> consent with mailbox pick -> Discord sign-in (delegated to a fake
// discord-broker) -> token -> MCP calls forwarded to a fake mailroom-core. KV is in memory.

import { createHash, randomBytes } from "node:crypto";
import { beforeEach, describe, expect, it, vi } from "vitest";
import worker from "../src/index";
import type { Env } from "../src/env";
import { CLIENT_ID, FakeCore, FakeSignin, MemoryKV } from "./fakes";

vi.spyOn(console, "log").mockImplementation(() => {});
vi.spyOn(console, "warn").mockImplementation(() => {});

const BASE = "https://door.test";
const REDIRECT = "https://claude.ai/api/mcp/auth_callback";
const LOTHROP = "800000000000000001";

let signin: FakeSignin;
let core: FakeCore;

beforeEach(() => {
  signin = new FakeSignin();
  core = new FakeCore();
});

function env(extra: Partial<Env> = {}): Env {
  return {
    OAUTH_KV: new MemoryKV() as unknown as KVNamespace,
    MAILROOM: core.fetcher,
    SIGNIN: signin.fetcher,
    PUBLIC_URL: BASE,
    ALLOWED_DISCORD_USERS: LOTHROP,
    ...extra,
  };
}

const ctx = { waitUntil: () => {}, passThroughOnException() {}, props: {} } as unknown as ExecutionContext;
const call = (e: Env, path: string, init?: RequestInit) => worker.fetch(new Request(BASE + path, init), e, ctx);

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
  handle: string;
  cookies: string;
  html: string;
}

async function startAuthorize(e: Env): Promise<Started> {
  const reg = await call(e, "/oauth/register", {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify({ client_name: "Claude", redirect_uris: [REDIRECT], token_endpoint_auth_method: "none" }),
  });
  expect(reg.status).toBe(201);
  const { client_id } = (await reg.json()) as { client_id: string };
  const verifier = randomBytes(32).toString("base64url");
  const q = new URLSearchParams({
    response_type: "code",
    client_id,
    redirect_uri: REDIRECT,
    scope: "mailroom offline_access",
    state: "st8",
    code_challenge: createHash("sha256").update(verifier).digest("base64url"),
    code_challenge_method: "S256",
    resource: `${BASE}/mcp`,
  });
  const consent = await call(e, `/authorize?${q}`);
  const html = await consent.text();
  if (consent.status !== 200) return { client_id, verifier, handle: "", cookies: "", html, status: consent.status } as any;
  return { client_id, verifier, handle: html.match(/name="handle" value="([^"]+)"/)![1], cookies: cookieHeader(consent), html };
}

const approve = (e: Env, s: Started, fields: Record<string, string>) =>
  call(e, "/authorize", {
    method: "POST",
    headers: { "content-type": "application/x-www-form-urlencoded", cookie: s.cookies },
    body: new URLSearchParams({ handle: s.handle, decision: "approve", ...fields }).toString(),
  });

async function signIn(e: Env, agent = "claude") {
  const s = await startAuthorize(e);
  const ap = await approve(e, s, { agent });
  expect(ap.status).toBe(302);
  const dc = new URL(ap.headers.get("location")!);
  expect(dc.origin + dc.pathname).toBe("https://discord.com/oauth2/authorize");
  expect(dc.searchParams.get("redirect_uri")).toBe(`${BASE}/callback`);
  expect(dc.searchParams.get("scope")).toBe("identify");
  expect(dc.searchParams.get("client_id")).toBe(CLIENT_ID);
  const cookies = cookieHeader(ap, s.cookies);
  const cb = await call(e, `/callback?code=dc_code&state=${encodeURIComponent(dc.searchParams.get("state")!)}`, { headers: { cookie: cookies } });
  return { cb, s };
}

async function token(e: Env, agent = "claude"): Promise<string> {
  const { cb, s } = await signIn(e, agent);
  expect(cb.status).toBe(302);
  const code = new URL(cb.headers.get("location")!).searchParams.get("code")!;
  const tok = await call(e, "/oauth/token", {
    method: "POST",
    headers: { "content-type": "application/x-www-form-urlencoded" },
    body: new URLSearchParams({ grant_type: "authorization_code", code, redirect_uri: REDIRECT, client_id: s.client_id, code_verifier: s.verifier, resource: `${BASE}/mcp` }).toString(),
  });
  expect(tok.status).toBe(200);
  return ((await tok.json()) as { access_token: string }).access_token;
}

const mcp = (e: Env, t: string, headers: Record<string, string> = {}) =>
  call(e, "/mcp", {
    method: "POST",
    headers: { authorization: `Bearer ${t}`, "content-type": "application/json", accept: "application/json, text/event-stream", ...headers },
    body: JSON.stringify({ jsonrpc: "2.0", id: 1, method: "tools/list" }),
  });

describe("mailbox picker", () => {
  it("lists the configured agents as unchecked required radios", async () => {
    const s = await startAuthorize(env());
    expect(s.html).toContain("Pick the mailbox this client should read. It cannot be changed without signing in again.");
    expect(s.html).toMatch(/<input type="radio" name="agent" value="claude" required>/);
    expect(s.html).toMatch(/<input type="radio" name="agent" value="manus" required>/);
    expect(s.html).not.toContain("checked");
  });

  it("requires a mailbox pick", async () => {
    const e = env();
    const res = await approve(e, await startAuthorize(e), {});
    expect(res.status).toBe(400);
    expect(await res.text()).toContain("Pick a mailbox.");
    expect(signin.seen).toEqual([]);
  });

  it("rejects an agent that is not configured", async () => {
    const e = env();
    const res = await approve(e, await startAuthorize(e), { agent: "root" });
    expect(res.status).toBe(400);
    expect(await res.text()).toContain("Pick a mailbox.");
  });

  it("shows a generic message when the core is misconfigured", async () => {
    core.agentsFailure = { status: 500, message: "mailroom config: x" };
    const s = (await startAuthorize(env())) as any;
    expect(s.status).toBe(502);
    expect(s.html).toContain("The mailroom is misconfigured. The operator can find the reason in the mailroom-core logs.");
  });

  it("does not show the core's error text on the page", async () => {
    core.agentsFailure = { status: 500, message: "mailroom config: secret-channel-123" };
    const s = (await startAuthorize(env())) as any;
    expect(s.html).not.toContain("mailroom config");
    expect(s.html).not.toContain("secret-channel-123");
  });
});

describe("sign-in and forwarding", () => {
  it("signs in end to end and stores the picked agent in the grant", async () => {
    const e = env();
    const t = await token(e, "claude");
    expect(signin.seen).toEqual([
      { path: "/signin/client", method: "GET", body: undefined },
      { path: "/signin", method: "POST", body: { code: "dc_code", redirect_uri: `${BASE}/callback` } },
    ]);
    const res = await mcp(e, t);
    expect(res.status).toBe(200);
    expect(await res.json()).toEqual({ jsonrpc: "2.0", id: 1, result: { ok: true } });
    expect(core.seen).toHaveLength(1);
    expect(core.seen[0].url).toBe("https://mailroom-core/mcp");
    expect(core.seen[0].method).toBe("POST");
    expect(core.seen[0].headers.get("x-mailroom-agent")).toBe("claude");
    expect(core.seen[0].headers.get("x-mailroom-user")).toBe(`lothrop (${LOTHROP})`);
    expect(core.seen[0].body).toContain("tools/list");
  });

  it("refuses a Discord user outside the allowlist", async () => {
    signin.user = { id: "800000000000000007", username: "stranger" };
    const { cb } = await signIn(env());
    expect(cb.status).toBe(403);
    expect(await cb.text()).toContain("800000000000000007");
  });

  it("shows the broker's error when the code exchange fails", async () => {
    signin.failure = { status: 502, message: "Discord did not return a token" };
    const { cb } = await signIn(env());
    expect(cb.status).toBe(502);
    expect(await cb.text()).toContain("Discord did not return a token");
  });

  it("refuses at the callback when the picked agent has since been removed", async () => {
    const e = env();
    const s = await startAuthorize(e);
    const ap = await approve(e, s, { agent: "manus" });
    const state = new URL(ap.headers.get("location")!).searchParams.get("state")!;
    core.agents = ["claude"];
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
    const t = await token(e, "claude");
    await mcp(e, t, { "x-mailroom-agent": "manus", "x-mailroom-evil": "1", "X-Mailroom-User": "evil (1)" });
    const h = core.seen[0].headers;
    expect(h.get("x-mailroom-agent")).toBe("claude");
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
    const res = await mcp(e, t, { origin: "https://evil.example" });
    expect(res.status).toBe(403);
    expect(await res.json()).toEqual({ jsonrpc: "2.0", id: null, error: { code: -32600, message: "Origin not allowed" } });
    expect(core.seen).toHaveLength(0);
    expect((await mcp(e, t, { origin: "https://claude.ai" })).status).toBe(200);
  });

  it("answers 401 with resource metadata when there is no token", async () => {
    const res = await call(env(), "/mcp", { method: "POST", body: "{}" });
    expect(res.status).toBe(401);
    expect(res.headers.get("www-authenticate")).toContain("resource_metadata");
    const bad = await call(env(), "/mcp", { method: "POST", headers: { authorization: "Bearer nope" }, body: "{}" });
    expect(bad.status).toBe(401);
  });

  it("serves resource metadata and a banner", async () => {
    const e = env();
    const meta = (await (await call(e, "/.well-known/oauth-protected-resource/mcp")).json()) as any;
    expect(meta.resource_name).toBe("Mailroom");
    expect(await (await call(e, "/")).text()).toBe(`mailroom 0.1.0: MCP endpoint at ${BASE}/mcp\n`);
  });
});
