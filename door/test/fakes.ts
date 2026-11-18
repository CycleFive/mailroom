// Test doubles: in-memory KV, and fake service bindings for discord-broker's Feed (SIGNIN)
// and mailroom-core (MAILROOM).

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

/** Imitates discord-broker's Feed: GET /signin/client and POST /signin. */
export class FakeSignin {
  user = { id: "800000000000000001", username: "lothrop" };
  /** When set, POST /signin answers this status with {"error": message}. */
  failure: { status: number; message: string } | null = null;
  seen: { path: string; method: string; body?: unknown }[] = [];

  fetcher = {
    fetch: async (input: RequestInfo | URL, init?: RequestInit) => {
      const req = new Request(input, init);
      const path = new URL(req.url).pathname;
      const body = req.method === "POST" ? await req.json().catch(() => null) : undefined;
      this.seen.push({ path, method: req.method, body });
      if (path === "/signin/client" && req.method === "GET") return jsonRes({ client_id: CLIENT_ID });
      if (path === "/signin" && req.method === "POST") {
        if (this.failure) return jsonRes({ error: this.failure.message }, this.failure.status);
        return jsonRes(this.user);
      }
      return jsonRes({ error: "not found" }, 404);
    },
  } as unknown as Fetcher;
}

export interface CoreSeen {
  url: string;
  method: string;
  headers: Headers;
  body: string;
}

/** Imitates mailroom-core: serves /agents and records everything else it is sent. */
export class FakeCore {
  agents = ["claude", "manus"];
  /** When set, /agents answers this status with {"error": message}. */
  agentsFailure: { status: number; message: string } | null = null;
  seen: CoreSeen[] = [];

  fetcher = {
    fetch: async (input: RequestInfo | URL, init?: RequestInit) => {
      const req = new Request(input, init);
      const path = new URL(req.url).pathname;
      if (path === "/agents") {
        if (this.agentsFailure) return jsonRes({ error: this.agentsFailure.message }, this.agentsFailure.status);
        return jsonRes({ agents: this.agents });
      }
      this.seen.push({ url: req.url, method: req.method, headers: req.headers, body: await req.text() });
      return jsonRes({ jsonrpc: "2.0", id: 1, result: { ok: true } });
    },
  } as unknown as Fetcher;
}
