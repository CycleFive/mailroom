// Sign-in for the MCP endpoint: a consent page where the operator picks which agent's
// mailbox the client gets, then "Sign in with Discord" (scope identify), then an allowlist
// check on the Discord user ID. The code exchange is delegated to the discord-broker Worker
// over a service binding, so this Worker holds no Discord secret.

import {
  AuthorizationError,
  CimdFetchError,
  authorizationErrorRedirect,
  type ConsentDescription,
  type OAuthHelpers,
} from "@cloudflare/workers-oauth-provider";
import { publicUrl, type Env } from "./env";

export const SCOPE = "mailroom";
export const VERSION = "0.1.0";

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

export function consentPage(d: ConsentDescription, handle: string, agents: string[], headers: Headers): Response {
  const who = d.clientDomain ? `${escapeHtml(d.clientName)} (verified domain ${escapeHtml(d.clientDomain)})` : escapeHtml(d.clientName);
  const options = agents
    .map((a) => `<label><input type="radio" name="agent" value="${escapeHtml(a)}" required> ${escapeHtml(a)}</label>`)
    .join("\n");
  return page(
    "Connect Mailroom",
    `<h1>Connect Mailroom</h1>
<p><strong>${who}</strong> wants to read an agent's mailbox: the Discord and GitHub messages addressed to it.</p>
<p>Access will be sent to <code>${escapeHtml(d.redirectHost)}</code>${d.redirectIsLoopback ? " (an app on this computer)" : ""}.</p>
<p class="warn">Only continue if you started this yourself, from Claude, and that host is <code>claude.ai</code> (or <code>localhost</code> for Claude Code).</p>
<form method="post" action="/authorize">
<input type="hidden" name="handle" value="${escapeHtml(handle)}">
<fieldset><legend>Pick the mailbox this client should read. It cannot be changed without signing in again.</legend>
${options}
</fieldset>
<button name="decision" value="approve">Continue to Discord</button>
<button name="decision" value="deny">Deny</button>
</form>`,
    headers,
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

/** The `error` text of a `{"error": "..."}` answer, or a generic line naming the status. */
async function upstreamError(res: Response, who: string): Promise<string> {
  const body = (await res.json().catch(() => null)) as { error?: unknown } | null;
  return typeof body?.error === "string" && body.error ? body.error : `${who} answered ${res.status}.`;
}

type Agents = { ok: true; agents: string[] } | { ok: false; message: string };

/** The configured agent names, from mailroom-core. */
async function fetchAgents(env: Env): Promise<Agents> {
  const res = await env.MAILROOM.fetch("https://mailroom-core/agents");
  if (res.status !== 200) return { ok: false, message: await upstreamError(res, "mailroom-core") };
  const body = (await res.json().catch(() => null)) as { agents?: unknown } | null;
  if (!Array.isArray(body?.agents) || !body.agents.every((a) => typeof a === "string")) {
    return { ok: false, message: "mailroom-core returned an unreadable agent list." };
  }
  return { ok: true, agents: body.agents as string[] };
}

/** GET and POST /authorize. */
export async function handleAuthorize(request: Request, env: Env, oauth: OAuthHelpers): Promise<Response> {
  try {
    if (request.method === "GET") {
      const agents = await fetchAgents(env);
      if (!agents.ok) {
        console.log(JSON.stringify({ event: "agents_unavailable", message: agents.message }));
        return errorPage(agents.message, 502);
      }
      const authReq = await oauth.parseAuthRequest(request);
      const details = await oauth.describeConsent(authReq);
      const consent = await oauth.beginConsent(authReq);
      return consentPage(details, consent.handle, agents.agents, consent.headers);
    }
    if (request.method !== "POST") return new Response("Method Not Allowed", { status: 405 });

    const form = await request.formData();
    const handle = String(form.get("handle") ?? "");
    if (form.get("decision") !== "approve") {
      const denied = await oauth.denyConsent(request, handle);
      return new Response(null, { status: 302, headers: denied.headers });
    }

    const agents = await fetchAgents(env);
    if (!agents.ok) {
      console.log(JSON.stringify({ event: "agents_unavailable", message: agents.message }));
      return errorPage(agents.message, 502);
    }
    const agent = String(form.get("agent") ?? "");
    if (!agents.agents.includes(agent)) return errorPage("Pick a mailbox.", 400);

    // One scope covers everything; offline_access lets the client refresh without a new sign-in.
    const approved = await oauth.approveConsent(request, handle, { scope: [SCOPE, "offline_access"] });

    const problem = configProblem(env);
    if (problem) {
      console.log(JSON.stringify({ event: "config_error", message: problem }));
      return errorPage(problem, 500);
    }
    const clientRes = await env.SIGNIN.fetch("https://signin/signin/client");
    const client = (await clientRes.json().catch(() => null)) as { client_id?: unknown } | null;
    if (clientRes.status !== 200 || typeof client?.client_id !== "string" || !client.client_id) {
      const message = await upstreamError(clientRes, "discord-broker");
      console.log(JSON.stringify({ event: "signin_client_failed", status: clientRes.status, message }));
      return errorPage(message, 502);
    }
    const up = await oauth.beginUpstream(approved.request, { data: { agent }, headers: approved.headers });
    const dc = new URL(DISCORD_AUTHORIZE);
    dc.searchParams.set("client_id", client.client_id);
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
export async function handleCallback(request: Request, env: Env, oauth: OAuthHelpers): Promise<Response> {
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

  const idRes = await env.SIGNIN.fetch("https://signin/signin", {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({ code, redirect_uri: `${publicUrl(env)}/callback` }),
  });
  if (idRes.status !== 200) {
    const message = await upstreamError(idRes, "discord-broker");
    console.log(JSON.stringify({ event: "signin_exchange_failed", status: idRes.status, message }));
    return errorPage(message, idRes.status >= 400 ? idRes.status : 502);
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
  const agents = await fetchAgents(env);
  if (!agents.ok) return errorPage(agents.message, 502);
  if (!agents.agents.includes(data.agent)) {
    console.log(JSON.stringify({ event: "agent_gone", agent: data.agent }));
    return errorPage(`The mailbox "${data.agent}" is no longer configured. Start again and pick another.`, 400);
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
