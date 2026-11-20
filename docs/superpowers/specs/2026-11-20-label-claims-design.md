# Work handed to an agent by label, across a whole organisation

Date: 2026-11-20. Status: approved in conversation (Lothrop: "yes that sounds right, you
have a go"), with the behaviour settled by Nyx's four answers, recorded below.

## Intent

A person hands work to an agent by putting a label on it: `assigned:<tag>` on an issue or
pull request, `review:<tag>` on a pull request, where `<tag>` belongs to one mailbox. The
agent should learn about it in its mailbox wherever in the organisation the item lives,
and then hear what happens on that item for as long as the label stays.

Today mailroom polls a fixed list of repositories (four of CycleFive's 26), tags
`routed:assigned` only when the bot is an assignee, and knows one optional review label
per source. Nyx's own poller already works by label; once this is live she will switch it
over to match, so the behaviour here is the shared one.

## Goals

1. One mail, "this is yours", when a trusted person first puts the label on an item,
   anywhere in the organisation. Keyed to that `labeled` event.
2. While the label stays, the item's activity (comments, reviews, review comments, state
   events) arrives as mail, also in a repository the mailbox does not otherwise watch.
3. One closing note when the label is removed or the item closes, so the agent can drop
   the claim. Putting the label back is a new assignment with a fresh mail.
4. Labels add to the existing assignee and requested-reviewer routing and reuse its tags.

## Non-goals

- Watching every repository of the organisation for all activity.
- Webhooks, or anything that pushes to an agent. Mailroom still never wakes anyone.
- Trusting comment text. The trust check is about who applied the label, nothing else.
- An allowlist of trusted bots. See Constraints.
- Replacing `review_label`, assignee or requested-reviewer routing.

## Anti-goals

- A "this is yours" that an untrusted labeler can cause. An agent acting on work that
  nobody with authority handed it is the failure this design exists to prevent.
- A false closing note (and then a second "this is yours") because the search index
  lagged or a poll failed.
- Re-mailing an item on every update with nothing new, as Nyx's current poller does in
  repositories it does not watch.
- The agent's own comments coming back as mail.
- Mail lost or duplicated for the repositories a mailbox already watches.

## Constraints

- GitHub's issue search (`GET /search/issues`) works with a GitHub App installation
  token, covers every repository the installation can read, takes `label:"a","b"` as "a
  or b", and is limited to 30 calls a minute per installation. Its index can lag and a
  response can say `incomplete_results: true`.
- Search does not say who applied a label. The item's events
  (`GET /repos/{repo}/issues/{n}/events`) do: a `labeled` event has `actor`, `label.name`,
  `id`, `created_at`.
- `GET /repos/{repo}/collaborators/{login}/permission` reports a person's access:
  `permission` is `admin`, `write`, `read` or `none`, and `role_name` adds `maintain` and
  `triage`. For a GitHub App's bot user it reports `none` (checked with
  `cycle-five-muse[bot]`). So a label applied by another agent is untrusted. That is kept
  on purpose: it stops one misled agent handing work to another.
- The GitHub broker's `Feed` reads `/repos/` paths only. It must also allow
  `/search/issues` (brokers 0.5.0, deployed before this).
- The inbox stores a record once per `id` (`INSERT OR IGNORE`): the first writer wins, and
  a later copy with more tags is dropped.
- Live GitHub sources use `keep: "tagged"`: an untagged record is dropped.
- Mailroom has no "wake". Agents act on tags that start `routed:`; other tags do not call
  for attention.

## Approaches

1. **Search, then follow each claimed item (chosen).** One search per poll; per claimed
   item, that item's own events, comments and reviews. Cost grows with claimed items.
2. **Watch all 26 repositories** with the existing per-repo poll. No search, but about
   130 calls per mailbox every five minutes, growing with every repository and mailbox.
3. **Webhooks from each GitHub App.** No polling, but mailroom has no public inbound
   path and each app would need a webhook secret.

## The design

### Configuration

A GitHub source gains an optional block:

```jsonc
{ "type": "github", "broker": "GITHUB", "repos": ["CycleFive/kharon"],
  "labels": { "org": "CycleFive", "tag": "claude" } }
```

`org` is the organisation searched; `tag` gives the two label names `assigned:<tag>` and
`review:<tag>`. With `labels` set, `repos` may be empty. The block adds one poll target to
the source, named `github:<broker>:labels:<org>`, on the source's `every_secs`.

### Which items count

An open item counts when it carries `assigned:<tag>` (issue or pull request), or carries
`review:<tag>` and is a pull request, and the item's own events agree: the label's newest
`labeled` event is not followed by an `unlabeled` event for the same name, and the label
is on the item. The search can be behind; the events are not. When the events say a label
is on and the search's list for that item does not show it, the item itself is read and
its labels are used. Label and repository names are compared without regard to
case, as GitHub does. A counted item the mailbox is tracking is a *claim*.

### Trust

The labeler of a `labeled` event is trusted when GitHub reports `permission` `admin` or
`write`, or `role_name` `admin`, `maintain`, `write` or `triage`, for them on that
repository. Anything else is untrusted: a missing user (404), and any login that is not
plain letters, digits and hyphens, which covers every app and bot, without a lookup. Any
other failure of the lookup fails the poll, which is retried; it never decides trust.

Trust is kept per label and follows that label's newest `labeled` event. A claim is
trusted exactly when one of the labels counting on it now was last applied by a trusted
labeler. A claim that loses that (its trusted label is removed while a bot's remains, or
is removed and put back by someone untrusted) is released with a closing note and kept as
an untrusted claim.

### One poll of the labels target

1. Search open items with either label, newest update first, 100 per page, one page. More
   than 100 matches produces a `gap` record.
2. For each item the search lists that is new, or whose `updated_at` differs from the
   stored one (at most 20 per poll, the longest-waiting first, and none once the poll has
   made 60 requests after the search; the rest wait for the next poll):
   - Read the item's events (pages of 100, at most ten). If all ten are full the newest
     events are missing, so nothing is decided: the item is skipped with a `gap` record.
   - For each counting label whose newest `labeled` event the claim has not handled:
     check the labeler's trust and write the *claim record* (below).
   - If the claim is trusted and its repository is not one of the source's `repos`,
     write the item's new activity since the claim's mark: events, comments, and for a
     pull request review comments and reviews (all paged, with a `gap` record when cut),
     in the same form and with the same record ids as the per-repo poll, each tagged
     `claimed`. A new claim's mark is the time of the trusted label event (or of the
     reopen, below), so what was said right after the hand-over is mailed and nothing
     before it; an untrusted label never moves the mark back.
   - If the repository is one of the source's `repos`, the same is done once, in the
     poll where the claim becomes trusted; after that the per-repo poll carries the
     activity and tags it `claimed` (it is given the claimed item numbers). Without that
     first read, a comment made in the same few minutes as the label would be dropped
     by `keep: "tagged"` before the repo poll knew of the claim.
3. For each stored claim that the search no longer returns, unless the search said
   `incomplete_results` (at most 20 per poll, within the same request budget): read the
   item itself. If it is closed, or no label counts on it, or it is gone or now lives in
   another repository, drop the claim and, if it was trusted, write the *closing note*.
   If it is still open and labelled, keep the claim: the search was behind.
4. The very first poll treats every labelled item as new, so standing assignments are
   mailed once at rollout.

An item that is read and turns out not to be a claim (no label counts, or its events could
not all be read) is remembered with its `updated_at` and not read again until that
changes, so such items cannot crowd out the others. A label entry is kept only while its
label counts, so a label that comes back needs a new `labeled` event, and a closing
note's id cannot repeat.

A source's `review_label` may not be one of its two label names: the per-repo poll routes
`review_label` without asking who applied it. Target keys are unique across an agent's
sources.

### Records

| Record | Kind | Id | Title | Tags |
| --- | --- | --- | --- | --- |
| claim, trusted | `event` | `gh:<repo>:claim:<event id>` | `labeled <label>` | `routed:assigned` for `assigned:<tag>`, `routed:review` for `review:<tag>` |
| claim, untrusted | `event` | same | same | `untrusted-label` |
| claim, after a reopen | `event` | `gh:<repo>:claim:<event id>:<reopened event id>` | same | as the label's claim record |
| activity on a trusted claim | as the per-repo poll | as the per-repo poll | as the per-repo poll | `claimed`, plus `routed:mention` and `mine` as today |
| closing note | `event` | `gh:<repo>:release:<n>:<latest claim event id>` | `released: closed`, `released: label removed` or `released: gone` | `released` |

The claim record and the closing note carry the item's title as body and its URL, the
labeler as actor (none on the note), and `<repo>#<n>` as reference. `skip_self` applies to
all of them as it does today: nothing whose actor is the mailbox's own bot is mailed.

An item that was released by closing and is then reopened with its label still on it is
handed over again: its claim record is keyed to the label event and the later `reopened`
event together and carries the reopen's time, and the closing note after it is keyed the
same way (`gh:<repo>:release:<n>:<event id>:<reopened event id>`), so neither is mistaken
for the earlier one by the inbox. Trust is still that of the labeler.

`claimed`, `released` and `untrusted-label` do not start `routed:`, so they inform without
calling for attention; `released` tells the agent to drop its claim.

### Broker

`Feed` of the GitHub broker allows `GET /api/search/issues` besides `/api/repos/...`.
Nothing else outside `/repos/` is opened. The installation token already limits a search
to what the app can read.

### Delivery

Brokers 0.5.0 first (all four copies), then mailroom 0.3.0 with `labels` added to each
mailbox's GitHub source in the mailbox manifest. An old mailroom is unaffected by the new
broker; a new mailroom against an old broker would only fail the labels target.

## My calls

- `assigned:<tag>` counts on issues and pull requests; `review:<tag>` only on pull requests.
- Items already labelled at rollout get their mail on the first poll.
- Bots are never trusted labelers; no allowlist.
- The claim record has its own id (`claim:`), not the per-repo poll's `event:` id, so in a
  watched repository the plain `labeled` event may also appear as context. A shared id
  would let whichever poll ran first decide whether the record carries the `routed:` tag.
- An untrusted claim is not followed and gets no closing note.
- A request failure on one item fails the whole labels poll; it is not skipped. `mailbox_info` shows the error.
- When one of two labels is removed and the other stays, nothing is mailed.
- At most 20 changed items are read per poll and one search page of 100.
- The door's version moves to 0.3.0 with the core, with no change in behaviour.

## Left to the builder

Internal names and module layout beyond what the plan fixes; wording of log lines.
