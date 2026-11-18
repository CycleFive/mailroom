export interface Env {
  /** KV namespace the OAuth library keeps clients, grants and tokens in. */
  OAUTH_KV: KVNamespace;
  /** Service binding to mailroom-core, the mailbox logic. It has no public URL. */
  MAILROOM: Fetcher;
  /** Service binding to discord-broker, entrypoint Feed: Sign in with Discord code exchange. */
  SIGNIN: Fetcher;

  /** This Worker's public origin, e.g. https://mailroom.example.workers.dev (no trailing slash). */
  PUBLIC_URL: string;
  /** Discord user IDs allowed to sign in, comma- or space-separated. Numeric IDs only. */
  ALLOWED_DISCORD_USERS: string;

  /** Optional: comma-separated Origins the MCP endpoint accepts when an Origin header is present. */
  ALLOWED_ORIGINS?: string;
  /** Optional: "true" turns off Client ID Metadata Documents so clients use Dynamic Client Registration. */
  DISABLE_CIMD?: string;
}

export function publicUrl(env: Env): string {
  if (!env.PUBLIC_URL) throw new Error("PUBLIC_URL is not set");
  return env.PUBLIC_URL.replace(/\/+$/, "");
}
