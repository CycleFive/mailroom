#!/usr/bin/env bash
# Smoke test: builds mailroom-core, runs it next to a fake Feed under `wrangler dev`, and exercises it over HTTP.
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
repo="$(cd "$here/../../.." && pwd)"
port=8797
base="http://127.0.0.1:$port"
log="$(mktemp)"
state="$(mktemp -d)" # fresh Durable Object storage on every run

if [ -x "$repo/door/node_modules/.bin/wrangler" ]; then
  wrangler=("$repo/door/node_modules/.bin/wrangler")
else
  wrangler=(npx --yes wrangler@4.157.0)
fi

fail() { echo "smoke: FAIL: $*" >&2; echo "--- wrangler log ---" >&2; tail -n 40 "$log" >&2; exit 1; }

cd "$here"
"${wrangler[@]}" dev -c wrangler.core.jsonc -c wrangler.feed.jsonc \
  --test-scheduled --port "$port" --local --persist-to "$state" >"$log" 2>&1 &
pid=$!

cleanup() {
  # Kill the whole process tree rooted at the wrangler we started (never pkill by pattern).
  kill_tree() {
    local p child
    for child in $(pgrep -P "$1" 2>/dev/null || true); do kill_tree "$child"; done
    kill "$1" 2>/dev/null || true
  }
  kill_tree "$pid"
  wait "$pid" 2>/dev/null || true
  rm -rf "$log" "$state"
}
trap cleanup EXIT

ready=""
deadline=$((SECONDS + 150))
while [ "$SECONDS" -lt "$deadline" ]; do
  if curl -sS --max-time 5 -o /dev/null "$base/agents" 2>/dev/null; then ready=yes; break; fi
  kill -0 "$pid" 2>/dev/null || fail "wrangler dev exited early"
  sleep 1
done
[ -n "$ready" ] || fail "wrangler dev did not start listening on port $port"

# mcp AGENT BODY -> prints the response body; HTTP status goes to $status.
status=""
mcp() {
  local out
  out="$(curl -sS --max-time 30 -w '\n%{http_code}' -X POST "$base/mcp" \
    -H 'content-type: application/json' -H 'accept: application/json, text/event-stream' \
    -H "x-mailroom-agent: $1" -H 'x-mailroom-user: smoke' -d "$2")"
  status="${out##*$'\n'}"
  printf '%s' "${out%$'\n'*}"
}
call() { # AGENT TOOL ARGS-JSON -> the tool result object
  mcp "$1" "{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"tools/call\",\"params\":{\"name\":\"$2\",\"arguments\":$3}}"
}
payload() { jq -c '.result.content[0].text | fromjson'; }
tick() { curl -fsS --max-time 60 "$base/__scheduled?cron=*+*+*+*+*" >/dev/null || fail "scheduled trigger failed"; }

# 1. agents
agents="$(curl -sS --max-time 20 "$base/agents" | jq -c '.agents' 2>&1)" || agents="(request failed)"
[ "$agents" = '["alpha","beta"]' ] || fail "GET /agents gave $agents"

# 2. first contact records nothing
tick
n="$(call alpha read_mail '{}' | payload | jq '.records | length')"
[ "$n" = 0 ] || fail "alpha has $n records after first contact, want 0"

# 3. second poll
tick
read_a="$(call alpha read_mail '{}' | payload)"
[ "$(jq '.records | length' <<<"$read_a")" = 2 ] || fail "alpha records: $read_a"
[ "$(jq -c '[.records[].seq]' <<<"$read_a")" = '[1,2]' ] || fail "alpha seqs: $read_a"
[ "$(jq -c '.records[1].tags' <<<"$read_a")" = '["routed:mention"]' ] || fail "alpha tags: $read_a"
n="$(call beta read_mail '{}' | payload | jq '.records | length')"
[ "$n" = 0 ] || fail "beta has $n records, want 0"

# 4. mailbox_info
info="$(call alpha mailbox_info '{}' | payload)"
bad="$(jq -c '.sources[0].targets[] | select(.target=="discord:FEED:30000")' <<<"$info")"
[ "$(jq '.last_error != null and .last_ok == null' <<<"$bad")" = true ] || fail "target 30000 status: $bad"
good="$(jq -c '.sources[0].targets[] | select(.target=="discord:FEED:10000")' <<<"$info")"
[ "$(jq '.last_ok != null' <<<"$good")" = true ] || fail "target 10000 status: $good"

# 5. ack
r="$(call alpha ack '{"seq":2}' | payload)"
[ "$r" = '{"cursor":2,"unread":0}' ] || fail "ack gave $r"
[ "$(call alpha ack '{"seq":9}' | jq '.result.isError')" = true ] || fail "ack 9 should be an error"
n="$(call alpha read_mail '{}' | payload | jq '.records | length')"
[ "$n" = 0 ] || fail "read after ack gave $n records"
n="$(call alpha read_mail '{"after":0}' | payload | jq '.records | length')"
[ "$n" = 2 ] || fail "read after=0 gave $n records"

# 6. unknown mailbox
mcp nobody '{"jsonrpc":"2.0","id":1,"method":"tools/list"}' >/dev/null
[ "$status" = 404 ] || fail "unknown agent gave status $status"

# 7. tools/list
n="$(mcp alpha '{"jsonrpc":"2.0","id":1,"method":"tools/list"}' | jq '.result.tools | length')"
[ "$n" = 3 ] || fail "tools/list gave $n tools"

echo "smoke: ok"
