// mailroom door: the only public Worker of mailroom. OAuth 2.1 authorization server and
// protected resource for MCP clients; authenticated requests are forwarded to mailroom-core
// over a service binding, stamped with the agent and user from the grant.
//
//   /<agent>/mcp                           MCP endpoint of one agent (OAuth bearer required)
//   /.well-known/oauth-protected-resource/<agent>/mcp   RFC 9728 metadata of that agent
//   /.well-known/oauth-authorization-server, /oauth/token, /oauth/register  (library)
//   /authorize, /callback                  consent page + Sign in with that agent's Discord application
//
// Every agent's address is its own OAuth protected resource. The agent list comes from the core.

import {
  OAuthAuthorizationServer,
  OAuthResourceServer,
  insufficientScope,
  type OAuthResourceContext,
} from "@cloudflare/workers-oauth-provider";
import { AuthProps, CORE_MISCONFIGURED, SCOPE, VERSION, agentResource, errorPage, handleAuthorize, handleCallback, isAllowed } from "./auth";
import { publicUrl, type Env } from "./env";

/** How long the agent list from the core is trusted. */
const AGENTS_TTL_MS = 60_000;
/** Agent names the core accepts; anything else cannot be an address. */
const AGENT_NAME = /^[a-z0-9-]{1,32}$/;
const AGENT_PATH = /^\/([a-z0-9-]{1,32})\/mcp(\/.*)?$/;
const AGENT_METADATA_PATH = /^\/\.well-known\/oauth-protected-resource\/([a-z0-9-]{1,32})\/mcp$/;

class CoreUnavailable extends Error {}

let agentCache: { at: number; agents: string[] } | undefined;
let roles: Roles | undefined;

/** Forget the cached agent list and the OAuth server built from it. Exists for tests. */
export function resetAgentCache(): void {
  agentCache = undefined;
  roles = undefined;
}

/** The agent names from the core, cached for AGENTS_TTL_MS; `fresh` skips the cache. */
async function getAgents(env: Env, fresh = false): Promise<string[]> {
  if (!fresh && agentCache && Date.now() - agentCache.at < AGENTS_TTL_MS) return agentCache.agents;
  const res = await env.MAILROOM.fetch("https://mailroom-core/agents");
  const body = res.status === 200 ? ((await res.json().catch(() => null)) as { agents?: unknown } | null) : null;
  if (!body || !Array.isArray(body.agents) || !body.agents.every((a) => typeof a === "string")) {
    console.log(JSON.stringify({ event: "agents_unavailable", status: res.status }));
    throw new CoreUnavailable();
  }
  const agents = (body.agents as string[]).filter((a) => AGENT_NAME.test(a));
  if (!agents.length) {
    console.log(JSON.stringify({ event: "agents_unavailable", status: res.status, message: "no usable agent names" }));
    throw new CoreUnavailable();
  }
  agentCache = { at: Date.now(), agents };
  return agents;
}

interface Roles {
  key: string;
  as: OAuthAuthorizationServer<Env>;
  rs: Map<string, OAuthResourceServer<Env, AuthProps>>;
}

/** The registry of resources is fixed at construction, so everything is rebuilt when the agent list changes. */
function getRoles(env: Env, agents: string[]): Roles {
  const url = publicUrl(env);
  const key = `${url}|${env.DISABLE_CIMD ?? ""}|${[...agents].sort().join(",")}`;
  if (roles && roles.key === key) return roles;

  const as = new OAuthAuthorizationServer<Env>({
    issuer: url,
    resources: agents.map((a) => agentResource(env, a)),
    scopesSupported: [SCOPE, "offline_access"],
    clientRegistrationEndpoint: "/oauth/register",
    // Claude prefers CIMD when advertised. DISABLE_CIMD="true" forces Dynamic Client Registration.
    clientIdMetadataDocumentEnabled: env.DISABLE_CIMD !== "true",
    accessTokenTTL: 3600,
  });

  const rs = new Map<string, OAuthResourceServer<Env, AuthProps>>();
  for (const agent of agents) {
    rs.set(
      agent,
      new OAuthResourceServer<Env, AuthProps>({
        resourceMetadata: { resource: agentResource(env, agent), authorization_servers: [url], resource_name: `Mailroom: ${agent}` },
        requiredScopes: [SCOPE],
        validateToken: (e) => (res, token) => as.validateToken<AuthProps>(res, token, e),
        handler: {
          async fetch(request: Request, env: Env, ctx: OAuthResourceContext<AuthProps>) {
            if (!ctx.auth.scope.includes(SCOPE)) return insufficientScope(ctx.auth, [SCOPE]);
            const props = ctx.props;
            // Re-check on every request, so removing someone from the allowlist takes effect at once.
            if (!props || !isAllowed(env.ALLOWED_DISCORD_USERS, props)) {
              return new Response("Forbidden", { status: 403 });
            }
            // The token is already bound to this address; the grant must agree.
            if (props.agent !== agent) return new Response("Forbidden", { status: 403 });
            const allowedOrigins = env.ALLOWED_ORIGINS?.split(",").map((s) => s.trim()).filter(Boolean);
            const origin = request.headers.get("origin");
            if (origin && allowedOrigins && allowedOrigins.length && !allowedOrigins.includes(origin)) {
              return new Response(JSON.stringify({ jsonrpc: "2.0", id: null, error: { code: -32600, message: "Origin not allowed" } }), {
                status: 403,
                headers: { "Content-Type": "application/json" },
              });
            }
            return forward(request, env, props);
          },
        },
      }),
    );
  }

  roles = { key, as, rs };
  return roles;
}

/**
 * Pass the request to mailroom-core. The x-mailroom-agent header is the whole boundary
 * between mailboxes, so it comes only from the grant, never from the client: every
 * x-mailroom-* header the client sent is dropped first. The bearer token and cookies stay here.
 */
async function forward(request: Request, env: Env, props: AuthProps): Promise<Response> {
  const headers = new Headers(request.headers);
  for (const name of [...headers.keys()]) {
    if (name.toLowerCase().startsWith("x-mailroom-")) headers.delete(name);
  }
  headers.delete("authorization");
  headers.delete("cookie");
  headers.set("x-mailroom-agent", props.agent);
  headers.set("x-mailroom-user", `${props.username} (${props.id})`);
  const hasBody = request.method !== "GET" && request.method !== "HEAD";
  return env.MAILROOM.fetch(new Request("https://mailroom-core/mcp", { method: request.method, headers, body: hasBody ? await request.arrayBuffer() : null }));
}

export default {
  async fetch(request: Request, env: Env, ctx: ExecutionContext): Promise<Response> {
    try {
      return await route(request, env, ctx);
    } catch (e) {
      console.log(JSON.stringify({ event: "unhandled", method: request.method, path: new URL(request.url).pathname, message: (e as Error)?.message }));
      return new Response(`Internal error: ${(e as Error)?.message ?? "unknown"}\n`, { status: 500, headers: { "Content-Type": "text/plain" } });
    }
  },
} satisfies ExportedHandler<Env>;

const unknownMailbox = () =>
  new Response(JSON.stringify({ error: "unknown mailbox" }), { status: 404, headers: { "Content-Type": "application/json" } });

async function route(request: Request, env: Env, ctx: ExecutionContext): Promise<Response> {
  const { pathname } = new URL(request.url);

  if (pathname === "/" && request.method === "GET") {
    return new Response(`mailroom ${VERSION}: the MCP address of a mailbox is ${publicUrl(env)}/<agent>/mcp\n`, {
      headers: { "Content-Type": "text/plain; charset=utf-8" },
    });
  }
  if (pathname === "/mcp" || pathname.startsWith("/mcp/")) {
    return new Response(`The address of a mailbox is ${publicUrl(env)}/<agent>/mcp.\n`, { status: 404, headers: { "Content-Type": "text/plain; charset=utf-8" } });
  }

  try {
    // The path shape decides the route; the agent list then decides whether the agent exists.
    const named = AGENT_PATH.exec(pathname) ?? AGENT_METADATA_PATH.exec(pathname);
    let agents = await getAgents(env);
    if (named && !agents.includes(named[1])) {
      agents = await getAgents(env, true);
      if (!agents.includes(named[1])) return unknownMailbox();
    }
    const { as, rs } = getRoles(env, agents);

    if (named) return rs.get(named[1])!.fetch(request, env, ctx);
    if (pathname === "/authorize") return handleAuthorize(request, env, as.getOAuthApi(env), agents);
    if (pathname === "/callback") return handleCallback(request, env, as.getOAuthApi(env), agents);
    if (pathname.startsWith("/.well-known/oauth-protected-resource")) return unknownMailbox();
    // Log every OAuth endpoint hit (path and status only) to make connector problems visible in `wrangler tail`.
    const res = await as.fetch(request, env, ctx);
    console.log(JSON.stringify({ event: "oauth_endpoint", method: request.method, path: pathname, status: res.status }));
    return res;
  } catch (e) {
    if (!(e instanceof CoreUnavailable)) throw e;
    if (pathname === "/authorize" || pathname === "/callback") return errorPage(CORE_MISCONFIGURED, 502);
    return new Response(`${CORE_MISCONFIGURED}\n`, { status: 502, headers: { "Content-Type": "text/plain; charset=utf-8" } });
  }
}
