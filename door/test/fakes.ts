// Test doubles: in-memory KV, and a fake service binding for mailroom-core (MAILROOM).

export class MemoryKV {
  m = new Map<string, { v: string; exp?: number; metadata?: unknown }>();
  async get(k: string, type?: any) {
    const e = this.m.get(k);
    if (!e || (e.exp && e.exp < Date.now())) return null;
    const t = typeof type === "string" ? type : type?.type;
    return t === "json" ? JSON.parse(e.v) : e.v;
  }
  async put(k: string, v: string, o?: { expirationTtl?: number; expiration?: number; metadata?: unknown }) {
    const exp = o?.expirationTtl ? Date.now() + o.expirationTtl * 1000 : o?.expiration ? o.expiration * 1000 : undefined;
    this.m.set(k, { v, exp, metadata: o?.metadata });
  }
  async delete(k: string) {
    this.m.delete(k);
  }
  async list(o?: { prefix?: string; limit?: number; cursor?: string }) {
    const keys = [...this.m.keys()].filter((k) => !o?.prefix || k.startsWith(o.prefix)).sort();
    return { keys: keys.map((name) => ({ name, metadata: this.m.get(name)!.metadata })), list_complete: true, cursor: "" };
  }
}

export const jsonRes = (body: unknown, status = 200) =>
  new Response(JSON.stringify(body), { status, headers: { "content-type": "application/json" } });

export const CLIENT_ID = "123456789012345678";

export interface CoreSeen {
  url: string;
  method: string;
  headers: Headers;
  body: string;
}

/**
 * Imitates mailroom-core: /agents, /signin/client, /signin and /mcp.
 * Sign-in calls are recorded in `signin`, everything else (the /mcp forwards) in `seen`.
 */
export class FakeCore {
  agents = ["claude", "manus"];
  /** The Discord client ID per agent; unknown agents get 404 like the real core. */
  clientIds: Record<string, string> = { claude: CLIENT_ID, manus: "223456789012345678" };
  user = { id: "800000000000000001", username: "lothrop" };
  /** When set, /agents answers this status with {"error": message}. */
  agentsFailure: { status: number; message: string } | null = null;
  /** When set, GET /signin/client answers this status with {"error": message}. */
  clientFailure: { status: number; message: string } | null = null;
  /** When set, POST /signin answers this status with {"error": message}. */
  failure: { status: number; message: string } | null = null;
  agentsCalls = 0;
  signin: { path: string; method: string; query?: string; body?: unknown }[] = [];
  seen: CoreSeen[] = [];

  fetcher = {
    fetch: async (input: RequestInfo | URL, init?: RequestInit) => {
      const req = new Request(input, init);
      const url = new URL(req.url);
      const path = url.pathname;
      if (path === "/agents") {
        this.agentsCalls++;
        if (this.agentsFailure) return jsonRes({ error: this.agentsFailure.message }, this.agentsFailure.status);
        return jsonRes({ agents: this.agents });
      }
      if (path === "/signin/client" && req.method === "GET") {
        this.signin.push({ path, method: "GET", query: url.search });
        if (this.clientFailure) return jsonRes({ error: this.clientFailure.message }, this.clientFailure.status);
        const id = this.clientIds[url.searchParams.get("agent") ?? ""];
        return id ? jsonRes({ client_id: id }) : jsonRes({ error: "unknown mailbox" }, 404);
      }
      if (path === "/signin" && req.method === "POST") {
        this.signin.push({ path, method: "POST", body: await req.json().catch(() => null) });
        if (this.failure) return jsonRes({ error: this.failure.message }, this.failure.status);
        return jsonRes(this.user);
      }
      this.seen.push({ url: req.url, method: req.method, headers: req.headers, body: await req.text() });
      return jsonRes({ jsonrpc: "2.0", id: 1, result: { ok: true } });
    },
  } as unknown as Fetcher;
}
