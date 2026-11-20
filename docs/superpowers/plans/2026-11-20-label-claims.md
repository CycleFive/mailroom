# Label Claims Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** A GitHub source with a `labels` block mails an agent once when a trusted person labels an item `assigned:<tag>` or `review:<tag>` anywhere in an organisation, follows that item while the label stays, and writes one closing note when it goes.

**Architecture:** A new poll target per source, `github:<broker>:labels:<org>`, handled by a new pure module `crates/logic/src/claims.rs` (`poll_claims`), which searches the organisation, keeps a `ClaimsState` of tracked items, and builds activity records with the same builders the per-repo poll uses. The per-repo poll learns which item numbers are claimed and tags their activity. The worker crate only dispatches.

**Tech Stack:** Rust (workspace: `crates/logic` pure logic with `pollster` tests and `FakeFeed`; `crates/worker` on `workers-rs`), serde, the existing `FeedClient` trait.

**Spec:** `docs/superpowers/specs/2026-11-20-label-claims-design.md`

## Global Constraints

- Typed serde only: every request and stored shape is a struct or enum deriving `Serialize`/`Deserialize`; no `serde_json::json!`, no `serde_json::Value`.
- Label names are exactly `assigned:<tag>` and `review:<tag>`. An item counts when it is open and carries `assigned:<tag>`, or carries `review:<tag>` and is a pull request.
- Trusted labeler: `permission` is `admin` or `write`, or `role_name` is `admin`, `maintain`, `write` or `triage`. A login containing any character outside `A-Z a-z 0-9 -` (every `[bot]` login) is untrusted without a lookup. A 404 from the lookup is untrusted. Any other lookup failure fails the poll.
- Tags, exactly: `routed:assigned`, `routed:review`, `untrusted-label`, `claimed`, `released`. Record ids, exactly: claim `gh:<repo>:claim:<event id>`; closing note `gh:<repo>:release:<number>:<latest claim event id>`; activity uses the per-repo poll's ids unchanged.
- Titles, exactly: `labeled <label>`; `released: closed`, `released: label removed`, `released: gone`.
- Requests, exactly (path, then query in this order):
  - `/search/issues` — `q` = `org:<org> is:open label:"assigned:<tag>","review:<tag>"`, `sort=updated`, `order=desc`, `per_page=100`
  - `/repos/<repo>/issues/<n>/events` — `per_page=100`, `page=<1..>`; next page while a page has 100 entries, at most 10 pages
  - `/repos/<repo>/collaborators/<login>/permission` — no query
  - `/repos/<repo>/issues/<n>/comments` — `since=<claim.since>`, `per_page=100`
  - `/repos/<repo>/pulls/<n>/comments` — `since=<claim.since>`, `per_page=100`
  - `/repos/<repo>/pulls/<n>/reviews` — `per_page=100`, `page=<1..>`, at most 10 pages
  - `/repos/<repo>/issues/<n>` — no query
- At most 20 new-or-changed items are read per poll (smallest `updated_at` first) and at most 20 vanished claims are checked per poll; the rest wait for the next poll.
- Nothing whose actor is the mailbox's own bot is mailed when `skip_self` is true.
- `poll_repo`'s existing behaviour and every existing test stay as they are, apart from the new `claimed` argument.
- `cargo fmt --check`, `cargo clippy --workspace --all-targets -- -D warnings` and `cargo test --workspace` pass at every commit.
- Every commit message ends with exactly this trailer and no other `Co-Authored-By` line: `Co-Authored-By: Claude & Lothrop (cycle.five@proton.me)`
- Never read, create or commit `crates/worker/wrangler.jsonc`, `door/wrangler.jsonc`, `.dev.vars` or `*.pem`; never run `wrangler deploy`, `wrangler tail` or anything against the account.
- Write each test first and see it fail (against a stub returning `todo!()` for a new function); the report quotes the failing run.

## Review Focus

1. A label applied by a bot or by someone with only read access: the item gets `untrusted-label`, never a `routed:` tag, and is never followed. (Task 2)
2. The search lags or answers `incomplete_results: true`, or a poll fails half-way: no closing note, no second "this is yours", state unchanged on failure. (Task 2)
3. The label is removed and put back: a closing note, then a fresh claim record keyed to the new `labeled` event. (Task 2)
4. An item with a long history: activity older than the claim is never mailed, and an edited old comment is not re-mailed. (Task 2)
5. A claimed item in a repository the source also watches: its activity is mailed once, tagged `claimed`, by the per-repo poll; the labels target does not fetch it. (Tasks 2 and 3)

---

### Task 1: The `labels` block in the configuration

**Files:**
- Modify: `crates/logic/src/config.rs`, `crates/logic/tests/config.rs`
- Modify: `crates/worker/wrangler.example.jsonc` (add the block to the `claude` GitHub source, with a comment)

**Interfaces:**
- Produces:

```rust
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct LabelRouting { pub org: String, pub tag: String }

impl LabelRouting {
    pub fn assigned(&self) -> String;   // "assigned:<tag>"
    pub fn review(&self) -> String;     // "review:<tag>"
}

// GithubSource gains:  #[serde(default)] pub labels: Option<LabelRouting>
// and `repos` becomes  #[serde(default)] pub repos: Vec<String>

impl GithubSource {
    pub fn labels_key(&self) -> Option<String>;   // "github:<broker>:labels:<org>" when `labels` is set
}
```

- `SourceConfig::target_keys()` returns the repo keys in order and then, when `labels` is set, the labels key last.
- Validation: `org` matches `^[A-Za-z0-9-]{1,39}$` (`labels.org: ...`); `tag` matches `^[a-z0-9-]{1,32}$` (`labels.tag: ...`); `repos` may be empty only when `labels` is set (message `repos: must not be empty unless labels is set`).

- [ ] **Step 1: Write the failing tests** in `crates/logic/tests/config.rs`

```rust
#[test] fn labels_block_parses_and_names_its_labels()      // org "CycleFive", tag "claude": assigned() == "assigned:claude", review() == "review:claude"
#[test] fn labels_adds_one_target_after_the_repos()        // repos ["o/a","o/b"], broker "G": target_keys == ["github:G:o/a","github:G:o/b","github:G:labels:CycleFive"]
#[test] fn a_source_with_labels_may_have_no_repos()        // parses; target_keys == ["github:G:labels:CycleFive"]
#[test] fn a_github_source_needs_repos_or_labels()         // error contains "repos: must not be empty unless labels is set"
#[test] fn labels_org_and_tag_are_checked()                // "" and "has space" org; "Upper" and "" tag: each an error naming labels.org / labels.tag
#[test] fn labels_rejects_unknown_keys()                   // {"org":"o","tag":"t","extra":1} fails to parse
#[test] fn a_source_without_labels_is_unchanged()          // labels_key() == None; target_keys as before
```

- [ ] **Step 2: Run and see them fail** — `cargo test -p mailroom-logic --test config`. Expected: compile errors for the missing items.
- [ ] **Step 3: Implement** in `config.rs`; add the block to the example config.
- [ ] **Step 4: Run** `cargo fmt --check && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace`. Expected: PASS.
- [ ] **Step 5: Commit** — `git commit -m "feat(config): a labels block on a GitHub source, as its own poll target"`

---

### Task 2: `poll_claims`

**Files:**
- Create: `crates/logic/src/claims.rs`, `crates/logic/tests/claims.rs`
- Modify: `crates/logic/src/lib.rs` (`pub mod claims;`), `crates/logic/src/github.rs` (make the record builders shared, see below)

**Interfaces:**
- Consumes: `GithubSource`, `LabelRouting` (Task 1); `FeedClient`, `get_json`, `Identity`, `Poll`, `PollError`, `FeedError` from `feed.rs`; `NewRecord`, `Kind`, `Src` from `record.rs`; `iso`, `epoch` from `time.rs`.
- Produces:

```rust
/// Items this mailbox is tracking, keyed "<owner>/<repo>#<number>".
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ClaimsState { pub claims: BTreeMap<String, Claim> }

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Claim {
    pub is_pr: bool,
    pub title: String,
    pub url: String,
    /// The item's `updated_at` when it was last read; a different value means "read it again".
    pub updated_at: String,
    /// Activity created after this instant has not been mailed yet.
    pub since: String,
    /// Label name -> id of the latest `labeled` event handled for it.
    pub label_events: BTreeMap<String, u64>,
    /// True once any of its label events came from a trusted labeler.
    pub trusted: bool,
}

impl ClaimsState {
    /// Numbers of the trusted claims in `repo` ("owner/name").
    pub fn claimed_numbers(&self, repo: &str) -> Vec<u64>;
}

/// One poll of a source's labels target. `opts.labels` must be set (else a `PollError`).
pub async fn poll_claims(
    feed: &impl FeedClient,
    me: &Identity,
    opts: &GithubSource,
    prev: Option<ClaimsState>,
    now_epoch: i64,
) -> Result<Poll<ClaimsState>, PollError>;
```

- In `github.rs`, the code that turns an issue comment, a review comment, a review and an issue event into a `NewRecord` (kind, id, timestamp, actor, reference, title, body excerpt, url, and the `routed:mention` / `mine` tags) moves into `pub(crate)` functions that both `poll_repo` and `poll_claims` call. No record-building logic is written twice. `poll_repo`'s output is unchanged and its tests are not edited.

Behaviour (the spec's "One poll of the labels target" is the authority; the points below fix what it leaves open):

- A new claim's `since` is the `created_at` of the earliest claim event mailed for it in that poll (or of the latest `labeled` event when none was mailed because of `skip_self`). After a claim is read, `updated_at` becomes the search item's `updated_at` and `since` becomes that same value.
- Activity is mailed when its creation time (`created_at`; `submitted_at` for a review) is strictly later than the claim's `since` before this poll. Events that are `labeled` events for one of the two labels are not mailed as activity (the claim record covers them).
- Per label currently on a counted item: take the latest `labeled` event for that label; if its id is greater than the stored one, check trust and write the claim record; store the id.
- An untrusted claim is stored (so it is not mailed again) and re-read only when its `updated_at` changes.
- Vanished claim, item read: `state` not `open` → `released: closed`; open but not counting → `released: label removed`; lookup status 404 or 410 → `released: gone`; still counting → keep the claim untouched. Only a trusted claim gets the note; either way the claim is dropped (except in the keep case).
- More matches than the page holds (`total_count` greater than the number of items returned) adds a `gap` record with id `gh:gap:labels:<org>:<now_epoch>` and title `labelled items of <org> may have skipped items`.
- Records are sorted by timestamp, stably, as in `poll_repo`. Any feed error other than the ones named above returns `Err`, so nothing is stored.

- [ ] **Step 1: Write the failing tests** in `crates/logic/tests/claims.rs`

Use `support::FakeFeed`; identity `claudeduet[bot]`; org `CycleFive`, tag `claude`; a helper that registers the search reply and helpers for an item's events, permission, comments and item replies.

```rust
#[test] fn a_trusted_label_on_an_issue_is_mailed_once()
// search: issue kharon#7 with assigned:claude; events: labeled by "lothrop" (id 500); permission admin.
// one record: kind Event, id "gh:CycleFive/kharon:claim:500", actor "lothrop", ref "CycleFive/kharon#7",
// title "labeled assigned:claude", body = the item's title, url = its html_url, tags ["routed:assigned"].
// polling again with the returned state and the same replies: no records, and no request other than the search.
#[test] fn a_review_label_on_a_pull_request_routes_review()          // tags ["routed:review"]
#[test] fn a_review_label_on_a_plain_issue_does_not_count()          // no record, no claim, no events request
#[test] fn triage_and_maintain_roles_are_trusted()                   // permission "read" + role_name "triage"; permission "write" + role_name "maintain"
#[test] fn a_reader_is_not_trusted()                                 // permission "read", role_name "read": tags ["untrusted-label"]
#[test] fn a_bot_labeler_is_untrusted_without_a_lookup()             // actor "cycle-five-muse[bot]": tags ["untrusted-label"]; no /collaborators/ request was made
#[test] fn a_missing_labeler_is_untrusted()                          // permission lookup 404
#[test] fn a_failed_permission_lookup_fails_the_poll()               // permission lookup 500: Err
#[test] fn an_untrusted_claim_is_not_followed_and_not_mailed_again() // second poll with a changed updated_at and a new comment: events re-read, no comment request, no record
#[test] fn a_trusted_relabel_of_an_untrusted_claim_is_mailed()       // new labeled event id 600 by an admin: a record with id ...:claim:600 and routed:assigned
#[test] fn activity_on_a_claim_in_an_unwatched_repo_is_mailed_as_claimed()
// state holds a trusted claim with since S; the search shows a newer updated_at; comments reply has one comment created after S
// and one created before S (edited since): exactly one IssueComment record, id "gh:<repo>:issue_comment:<id>", tags contain "claimed";
// the comments request carried since=S; the new state has since == updated_at == the search item's updated_at.
#[test] fn pull_request_claims_also_read_review_comments_and_reviews()  // a review submitted after S: kind Review, tag "claimed"
#[test] fn state_events_on_a_claim_are_mailed_but_its_own_label_event_is_not()  // a "closed"... use "reopened" after S: mailed; the labeled event for assigned:claude after S: not mailed as activity
#[test] fn a_mention_in_claimed_activity_keeps_routed_mention()       // tags contain both "claimed" and "routed:mention"
#[test] fn the_bots_own_comment_is_not_mailed()                       // skip_self true: no record
#[test] fn a_claim_in_a_watched_repo_is_not_fetched_here()            // opts.repos contains the repo: after the events read, no comments/reviews request and no activity record
#[test] fn an_unchanged_claim_costs_no_requests()                     // same updated_at: calls == [the search]
#[test] fn nothing_older_than_the_claim_is_mailed()                   // new claim, item has a comment created before the labeled event: no comment record; since == the labeled event's created_at
#[test] fn a_closed_item_is_released_once()
// state holds a trusted claim; search returns nothing; item reply state "closed":
// one record id "gh:<repo>:release:7:500", title "released: closed", tags ["released"], body = claim title; the claim is gone from the state.
#[test] fn a_removed_label_is_released()                              // item open, labels without ours: "released: label removed"
#[test] fn a_vanished_item_is_released_as_gone()                      // item lookup 404: "released: gone"
#[test] fn a_lagging_search_releases_nothing()                        // search returns nothing but the item is open and still labelled: no record, claim kept unchanged
#[test] fn an_incomplete_search_checks_no_vanished_claims()           // incomplete_results true: no /issues/7 request, claim kept
#[test] fn an_untrusted_claim_is_dropped_without_a_note()
#[test] fn relabelling_after_a_release_is_a_new_claim()               // after the release poll, a search hit with labeled event id 700: record ...:claim:700
#[test] fn the_first_poll_mails_standing_assignments()                // prev None with two labelled items: two claim records
#[test] fn at_most_twenty_items_are_read_per_poll()                   // 25 new items: 20 events requests, for the 20 smallest updated_at; 20 claims stored; the other 5 are read on the next poll
#[test] fn more_matches_than_a_page_adds_a_gap_record()               // total_count 150 with 100 items: one Kind::Gap record, id starts "gh:gap:labels:CycleFive:"
#[test] fn a_failed_search_fails_the_poll()                           // Err; no state
#[test] fn the_search_request_is_exact()                              // calls[0] == "/search/issues?q=org:CycleFive is:open label:\"assigned:claude\",\"review:claude\"&sort=updated&order=desc&per_page=100"
#[test] fn claimed_numbers_lists_only_trusted_claims_of_that_repo()
```

- [ ] **Step 2: Run and see them fail** — add `claims.rs` with the types and `poll_claims` returning `todo!()`; `cargo test -p mailroom-logic --test claims`. Expected: FAIL (panics at `todo!()`), quoted in the report.
- [ ] **Step 3: Share the record builders** in `github.rs`; run `cargo test -p mailroom-logic --test github`. Expected: PASS with that test file unedited.
- [ ] **Step 4: Implement `poll_claims`**.
- [ ] **Step 5: Run** `cargo fmt --check && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace`. Expected: PASS.
- [ ] **Step 6: Commit** — `git commit -m "feat: poll_claims: work handed over by label, followed until released"`

---

### Task 3: Wire it in, tag claimed activity in watched repos, 0.3.0

**Files:**
- Modify: `crates/logic/src/github.rs`, `crates/logic/tests/github.rs`
- Modify: `crates/worker/src/object.rs`
- Modify: `crates/logic/Cargo.toml`, `crates/worker/Cargo.toml`, `Cargo.lock`, `door/package.json`, `door/package-lock.json`, `door/src/auth.ts` (and any door test that states the version)
- Modify: `README.md`

**Interfaces:**
- Consumes: `ClaimsState`, `poll_claims` (Task 2); `GithubSource::labels_key` (Task 1).
- Changes: `poll_repo(feed, me, repo, opts, prev, now_epoch, claimed: &[u64])`. A record of kind `IssueComment`, `ReviewComment`, `Review` or `Event` whose item number is in `claimed` gets the tag `claimed` (once, after its other tags). Nothing else changes; existing tests pass `&[]`.
- `object.rs`: the target whose key equals the source's `labels_key()` is polled with `poll_claims` and its state stored like any other target's. A repo target of a source with `labels` reads the stored `ClaimsState` of that source's labels target (absent means no claims) and passes `claimed_numbers(repo)` to `poll_repo`. `mailbox_info` lists the labels target like any other (it already walks `target_keys()`).

- [ ] **Step 1: Write the failing tests** in `crates/logic/tests/github.rs`

```rust
#[test] fn activity_on_a_claimed_item_is_tagged_claimed()      // claimed [4]: a new comment, review and event on #4 carry "claimed"; the same on #5 do not
#[test] fn claimed_does_not_displace_other_tags()              // a mention on #4: tags == ["routed:mention", "claimed"]
#[test] fn new_issues_and_pull_requests_are_not_tagged_claimed()  // Kind::Issue / Kind::Pr records for #4 carry no "claimed"
```

- [ ] **Step 2: Run and see them fail** — `cargo test -p mailroom-logic --test github`.
- [ ] **Step 3: Implement** the `claimed` argument and the dispatch in `object.rs`.
- [ ] **Step 4: Version 0.3.0** in both `Cargo.toml` files (then `cargo build` to refresh `Cargo.lock`), `door/package.json`, `door/package-lock.json` (`npm install --package-lock-only` in `door/`), `VERSION` in `door/src/auth.ts`, and any test literal.
- [ ] **Step 5: README** — in Configuration, the `labels` row and a short section "Work handed over by label": the two labels, who is trusted (and that an app's bot never is), the claim record, following while the label stays (also outside `repos`), the closing note, the three new tags in the tags table with which ones call for attention, the limits (20 items a poll, one search page), that the broker's `Feed` must allow `/search/issues` (CycleFive/brokers 0.5.0 or later), and the example config. Match the README's existing tone.
- [ ] **Step 6: Run** `cargo fmt --check && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace`, and in `door/`: `npm ci && npm run typecheck && npm test`. Expected: PASS.
- [ ] **Step 7: Commit** — `git commit -m "feat: label claims wired into the mailbox; claimed activity in watched repos (0.3.0)"`
