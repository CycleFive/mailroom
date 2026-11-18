// Sign-in for an agent's MCP endpoint: a consent page that names the mailbox (taken from the
// request's `resource`, RFC 8707), then "Sign in with Discord" (scope identify) through that
// agent's own Discord application, then an allowlist check on the Discord user ID. The client
// ID lookup and the code exchange go through mailroom-core, which forwards them to the agent's
// broker, so this Worker holds no Discord secret and has no binding to any broker.

import {
  AuthorizationError,
  CimdFetchError,
  authorizationErrorRedirect,
  type ConsentDescription,
  type OAuthHelpers,
} from "@cloudflare/workers-oauth-provider";
import { publicUrl, type Env } from "./env";

export const SCOPE = "mailroom";
export const VERSION = "0.2.0";

export const DISCORD_AUTHORIZE = "https://discord.com/oauth2/authorize";

export interface AuthProps {
  id: string;
  username: string;
  agent: string;
}

/** ALLOWED_DISCORD_USERS: numeric Discord user IDs, comma- or space-separated. */
export function parseAllowlist(raw: string | undefined): { ids: Set<string>; bad: string[] } {
  const ids = new Set<string>();
  const bad: string[] = [];
  for (const entry of (raw ?? "").split(/[\s,]+/).filter(Boolean)) {
    if (/^\d{5,25}$/.test(entry)) ids.add(entry);
    else bad.push(entry);
  }
  return { ids, bad };
}

export function isAllowed(raw: string | undefined, user: { id: string | number }): boolean {
  return parseAllowlist(raw).ids.has(String(user.id));
}

/** Catch the usual configuration mix-ups before sending anyone to Discord, which answers them with an unhelpful page. */
export function configProblem(env: Env): string | null {
  const { ids, bad } = parseAllowlist(env.ALLOWED_DISCORD_USERS);
  if (bad.length) {
    return `ALLOWED_DISCORD_USERS takes numeric Discord user IDs only, but has: ${bad.join(", ")}. In Discord turn on Developer Mode (Settings, Advanced), then right-click your name and Copy User ID.`;
  }
  if (!ids.size) return "ALLOWED_DISCORD_USERS is empty, so nobody could sign in.";
  if (!env.PUBLIC_URL) return "PUBLIC_URL is not set (this Worker's public origin, no trailing slash).";
  return null;
}

export function escapeHtml(s: string): string {
  return s.replace(/[&<>"']/g, (c) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;" })[c]!);
}

function page(title: string, body: string, headers?: Headers, status = 200): Response {
  const h = headers ?? new Headers();
  h.set("Content-Type", "text/html; charset=utf-8");
  h.set("Cache-Control", "no-store");
  h.set("Content-Security-Policy", "default-src 'none'; style-src 'unsafe-inline'; frame-ancestors 'none'");
  return new Response(
    `<!doctype html><html><head><meta charset="utf-8"><meta name="viewport" content="width=device-width,initial-scale=1"><title>${escapeHtml(title)}</title>
<style>body{font:16px/1.5 system-ui,sans-serif;max-width:34rem;margin:3rem auto;padding:0 1rem;color:#1b1b1b;background:#fafafa}
@media(prefers-color-scheme:dark){body{color:#e8e8e8;background:#161616}}
code{font-size:.95em}.warn{border-left:3px solid #c80;padding-left:.8rem}button{font:inherit;padding:.5rem 1rem;margin-right:.5rem}
fieldset{border:0;padding:0;margin:1rem 0}label{display:block}</style></head>
<body>${body}</body></html>`,
    { status, headers: h },
  );
}

/** The resource (MCP address) of an agent. */
export function agentResource(env: Env, agent: string): string {
  return `${publicUrl(env)}/${agent}/mcp`;
}

/** The agent whose resource is exactly `resource`, else null. */
export function agentOfResource(env: Env, resource: string | undefined, agents: string[]): string | null {
  if (!resource) return null;
  return agents.find((a) => agentResource(env, a) === resource) ?? null;
}

export function consentPage(d: ConsentDescription, handle: string, agent: string, headers: Headers): Response {
  const who = d.clientDomain ? `${escapeHtml(d.clientName)} (verified domain ${escapeHtml(d.clientDomain)})` : escapeHtml(d.clientName);
  return page(
    "Connect Mailroom",
    `<h1>Connect Mailroom</h1>
<p><strong>${who}</strong> wants to read the mailbox of <strong>${escapeHtml(agent)}</strong>: the Discord and GitHub messages addressed to it.</p>
<p>Access will be sent to <code>${escapeHtml(d.redirectHost)}</code>${d.redirectIsLoopback ? " (an app on this computer)" : ""}.</p>
<p class="warn">Only continue if you started this yourself, from the app you are connecting, and you recognise that host.</p>
<form method="post" action="/authorize">
<input type="hidden" name="handle" value="${escapeHtml(handle)}">
<button name="decision" value="approve">Continue to Discord</button>
<button name="decision" value="deny">Deny</button>
</form>`,
    headers,
  );
}

/** Shown when the client named no mailbox: each one links back to /authorize with its resource added. */
export function mailboxListPage(request: Request, env: Env, agents: string[]): Response {
  const url = new URL(request.url);
  const links = agents
    .map((a) => {
      const q = new URLSearchParams(url.search);
      q.set("resource", agentResource(env, a));
      return `<li><a href="/authorize?${escapeHtml(q.toString())}">${escapeHtml(a)}</a></li>`;
    })
    .join("\n");
  return page(
    "Pick a mailbox",
    `<h1>Pick a mailbox</h1>
<p>The app you are connecting did not say which mailbox it wants. Pick the one it should read.</p>
<ul>
${links}
</ul>`,
  );
}

export function errorPage(message: string, status = 400): Response {
  return page("Sign-in failed", `<h1>Sign-in failed</h1><p>${escapeHtml(message)}</p>`, undefined, status);
}

function redirect(location: string, headers?: Headers): Response {
  const h = headers ?? new Headers();
  h.set("Location", location);
  return new Response(null, { status: 302, headers: h });
}

function renderAuthError(e: unknown): Response {
  if (e instanceof AuthorizationError) {
    console.log(JSON.stringify({ event: "authorize_error", code: e.code, description: e.description, redirect: Boolean(e.redirectTo) }));
    if (e.redirectTo) return redirect(e.redirectTo);
    return errorPage(e.description, 400);
  }
  if (e instanceof CimdFetchError) {
    // The client identified itself by a metadata URL that this Worker could not fetch.
    console.log(JSON.stringify({ event: "cimd_fetch_failed", url: e.metadataUrl, detail: e.detail }));
    return errorPage(`Could not fetch the client's metadata document (${e.metadataUrl}): ${e.detail}. Setting DISABLE_CIMD="true" makes clients register instead.`, 502);
  }
  console.log(JSON.stringify({ event: "authorize_exception", message: (e as Error)?.message }));
  return errorPage(`Unexpected error: ${(e as Error)?.message ?? String(e)}`, 500);
}

/** Shown to visitors instead of the core's error text, which may quote channel IDs or repo names. */
export const CORE_MISCONFIGURED = "The mailroom is misconfigured. The operator can find the reason in the mailroom-core logs.";

/** The `error` text of a `{"error": "..."}` answer, or empty. Goes to the log only, never to a page. */
async function upstreamError(res: Response): Promise<string> {
  const body = (await res.json().catch(() => null)) as { error?: unknown } | null;
  return typeof body?.error === "string" ? body.error : "";
}

/** The client ID of the agent's Discord application, via the core; null (after logging) on any failure. */
async function fetchSigninClient(env: Env, agent: string): Promise<string | null> {
  const res = await env.MAILROOM.fetch(`https://mailroom-core/signin/client?agent=${encodeURIComponent(agent)}`);
  if (res.status !== 200) {
    console.log(JSON.stringify({ event: "signin_client_failed", agent, status: res.status, message: await upstreamError(res) }));
    return null;
  }
  const body = (await res.json().catch(() => null)) as { client_id?: unknown } | null;
  if (typeof body?.client_id !== "string" || !body.client_id) {
    console.log(JSON.stringify({ event: "signin_client_failed", agent, status: res.status, message: "unreadable answer" }));
    return null;
  }
  return body.client_id;
}

/** GET and POST /authorize. `agents` is the list the OAuth server was built from. */
export async function handleAuthorize(request: Request, env: Env, oauth: OAuthHelpers, agents: string[]): Promise<Response> {
  try {
    if (request.method === "GET") {
      // Without a resource the library would refuse the request, so list the mailboxes first.
      if (!new URL(request.url).searchParams.has("resource")) return mailboxListPage(request, env, agents);
      const authReq = await oauth.parseAuthRequest(request);
      const agent = agentOfResource(env, authReq.resource, agents);
      if (!agent) return errorPage("That address is not a mailbox.", 400);
      const details = await oauth.describeConsent(authReq);
      const consent = await oauth.beginConsent(authReq);
      return consentPage(details, consent.handle, agent, consent.headers);
    }
    if (request.method !== "POST") return new Response("Method Not Allowed", { status: 405 });

    const form = await request.formData();
    const handle = String(form.get("handle") ?? "");
    if (form.get("decision") !== "approve") {
      const denied = await oauth.denyConsent(request, handle);
      return new Response(null, { status: 302, headers: denied.headers });
    }

    // One scope covers everything; offline_access lets the client refresh without a new sign-in.
    const approved = await oauth.approveConsent(request, handle, { scope: [SCOPE, "offline_access"] });
    // The mailbox comes from the stored request, never from the form.
    const agent = agentOfResource(env, approved.request.resource, agents);
    if (!agent) return errorPage("That address is not a mailbox.", 400);

    const problem = configProblem(env);
    if (problem) {
      console.log(JSON.stringify({ event: "config_error", message: problem }));
      return errorPage(problem, 500);
    }
    const clientId = await fetchSigninClient(env, agent);
    if (!clientId) return errorPage(CORE_MISCONFIGURED, 502);
    const up = await oauth.beginUpstream(approved.request, { data: { agent }, headers: approved.headers });
    const dc = new URL(DISCORD_AUTHORIZE);
    dc.searchParams.set("client_id", clientId);
    dc.searchParams.set("response_type", "code");
    dc.searchParams.set("redirect_uri", `${publicUrl(env)}/callback`);
    dc.searchParams.set("scope", "identify");
    dc.searchParams.set("state", up.state);
    return redirect(dc.toString(), up.headers);
  } catch (e) {
    return renderAuthError(e);
  }
}

/** GET /callback from Discord. */
export async function handleCallback(request: Request, env: Env, oauth: OAuthHelpers, agents: string[]): Promise<Response> {
  let resumed;
  try {
    resumed = await oauth.finishUpstream<{ agent: string }>(request);
  } catch (e) {
    return renderAuthError(e);
  }
  const { request: authReq, data, headers } = resumed;
  const params = new URL(request.url).searchParams;
  const code = params.get("code");
  if (params.get("error") || !code) {
    console.log(JSON.stringify({ event: "sign_in_cancelled", error: params.get("error"), description: params.get("error_description") }));
    return redirect(authorizationErrorRedirect(authReq, "access_denied", "Discord sign-in was cancelled"), headers);
  }

  const idRes = await env.MAILROOM.fetch("https://mailroom-core/signin", {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({ agent: data.agent, code, redirect_uri: `${publicUrl(env)}/callback` }),
  });
  if (idRes.status !== 200) {
    console.log(JSON.stringify({ event: "signin_exchange_failed", agent: data.agent, status: idRes.status, message: await upstreamError(idRes) }));
    return errorPage(CORE_MISCONFIGURED, 502);
  }
  const id = (await idRes.json().catch(() => null)) as { id?: unknown; username?: unknown } | null;
  if (typeof id?.id !== "string" || typeof id.username !== "string") {
    console.log(JSON.stringify({ event: "user_read_failed" }));
    return errorPage("Could not read your Discord account.", 502);
  }
  const user = { id: id.id, username: id.username };
  if (!isAllowed(env.ALLOWED_DISCORD_USERS, { id: user.id })) {
    console.log(JSON.stringify({ event: "sign_in_refused", username: user.username, id: user.id }));
    return errorPage(`Discord account ${user.username} (ID ${user.id}) is not in ALLOWED_DISCORD_USERS.`, 403);
  }

  // The mailbox was valid at consent time; make sure it still is.
  if (!agents.includes(data.agent)) {
    console.log(JSON.stringify({ event: "agent_gone", agent: data.agent }));
    return errorPage(`The mailbox "${data.agent}" is no longer configured. Start again from the app you are connecting.`, 400);
  }

  const { redirectTo } = await oauth.completeAuthorization({
    request: authReq,
    userId: user.id,
    metadata: { username: user.username, agent: data.agent },
    scope: authReq.scope,
    props: { id: user.id, username: user.username, agent: data.agent } satisfies AuthProps,
  });
  console.log(JSON.stringify({ event: "sign_in", username: user.username, id: user.id, agent: data.agent, client: authReq.clientId }));
  return redirect(redirectTo, headers);
}
