// mailroom door: the only public Worker of mailroom. OAuth 2.1 authorization server and
// protected resource for MCP clients; authenticated /mcp requests are forwarded to
// mailroom-core over a service binding, stamped with the agent and user from the grant.
//
//   /mcp                                   MCP endpoint (OAuth bearer required), forwarded to the core
//   /.well-known/oauth-protected-resource  RFC 9728 metadata (resource server)
//   /.well-known/oauth-authorization-server, /oauth/token, /oauth/register  (library)
//   /authorize, /callback                  consent page with mailbox picker + Sign in with Discord

import {
  OAuthAuthorizationServer,
  OAuthResourceServer,
  insufficientScope,
  type OAuthResourceContext,
} from "@cloudflare/workers-oauth-provider";
import { AuthProps, SCOPE, VERSION, handleAuthorize, handleCallback, isAllowed } from "./auth";
import { publicUrl, type Env } from "./env";

interface Roles {
  key: string;
  as: OAuthAuthorizationServer<Env>;
  rs: OAuthResourceServer<Env, AuthProps>;
}
let roles: Roles | undefined;

function getRoles(env: Env): Roles {
  const url = publicUrl(env);
  const key = `${url}|${env.DISABLE_CIMD ?? ""}`;
  if (roles && roles.key === key) return roles;
  const resource = `${url}/mcp`;

  const as = new OAuthAuthorizationServer<Env>({
    issuer: url,
    resources: [resource],
    scopesSupported: [SCOPE, "offline_access"],
    clientRegistrationEndpoint: "/oauth/register",
    // Claude prefers CIMD when advertised. DISABLE_CIMD="true" forces Dynamic Client Registration.
    clientIdMetadataDocumentEnabled: env.DISABLE_CIMD !== "true",
    accessTokenTTL: 3600,
  });

  const rs = new OAuthResourceServer<Env, AuthProps>({
    resourceMetadata: { resource, authorization_servers: [url], resource_name: "Mailroom" },
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
  });

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

async function route(request: Request, env: Env, ctx: ExecutionContext): Promise<Response> {
  const { pathname } = new URL(request.url);
  const { as, rs } = getRoles(env);

  if (pathname === "/authorize") return handleAuthorize(request, env, as.getOAuthApi(env));
  if (pathname === "/callback") return handleCallback(request, env, as.getOAuthApi(env));
  if (pathname === "/mcp" || pathname.startsWith("/mcp/") || pathname.startsWith("/.well-known/oauth-protected-resource")) {
    return rs.fetch(request, env, ctx);
  }
  if (pathname === "/" && request.method === "GET") {
    return new Response(`mailroom ${VERSION}: MCP endpoint at ${publicUrl(env)}/mcp\n`, {
      headers: { "Content-Type": "text/plain; charset=utf-8" },
    });
  }
  // Log every OAuth endpoint hit (path and status only) to make connector problems visible in `wrangler tail`.
  const res = await as.fetch(request, env, ctx);
  console.log(JSON.stringify({ event: "oauth_endpoint", method: request.method, path: pathname, status: res.status }));
  return res;
}
