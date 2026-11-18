// Fake "Feed" entrypoint standing in for the Discord broker during the smoke test.
import { WorkerEntrypoint } from "cloudflare:workers";

// Call counts live at module level so they survive across requests.
let calls10000 = 0;
let calls20000 = 0;

const author = { id: "1", username: "someone" };
const message = (id, content, mentions = []) => ({
  id,
  type: 0,
  timestamp: "2026-11-18T10:00:00.000000+00:00",
  content,
  author,
  mentions,
});

const M1 = message("100000000000000001", "first");
const M2 = message("100000000000000002", "second");
const M3 = message("100000000000000003", "hey <@900>", [{ id: "900" }]);
const OTHER = message("200000000000000001", "elsewhere");

export class Feed extends WorkerEntrypoint {
  async fetch(request) {
    const { pathname } = new URL(request.url);
    switch (pathname) {
      case "/signin/client":
        return Response.json({ client_id: "424242" });
      case "/signin": {
        const body = await request.json().catch(() => ({}));
        if (body.code === "good" && body.redirect_uri === "https://door/callback") {
          return Response.json({ id: "77", username: "op" });
        }
        return Response.json({ error: "Discord did not return a token" }, { status: 502 });
      }
      case "/identity":
        return Response.json({ id: "900", login: "bot", name: "Bot" });
      case "/api/channels/10000":
        return Response.json({ name: "general", guild_id: "1" });
      case "/api/channels/20000":
        return Response.json({ name: "other", guild_id: "1" });
      case "/api/channels/30000":
        return Response.json({ name: "flaky", guild_id: "1" });
      case "/api/channels/10000/messages":
        calls10000 += 1;
        return Response.json(calls10000 === 1 ? [M1] : [M3, M2, M1]);
      case "/api/channels/20000/messages":
        calls20000 += 1;
        return Response.json([OTHER]);
      case "/api/channels/30000/messages":
        return new Response("<html><body>Too Many Requests</body></html>", {
          status: 429,
          headers: { "content-type": "text/html" },
        });
      default:
        return new Response("not found", { status: 404 });
    }
  }
}

export default {
  fetch() {
    return new Response("fake feed\n");
  },
};
