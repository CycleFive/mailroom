mod support;

use std::collections::BTreeMap;

use mailroom_logic::claims::{poll_claims, Claim, ClaimsState, LabelEvent};
use mailroom_logic::config::GithubSource;
use mailroom_logic::feed::{Identity, Poll, PollError};
use mailroom_logic::record::{Kind, NewRecord};
use mailroom_logic::time::epoch;
use pollster::block_on;
use support::FakeFeed;

const R: &str = "CycleFive/kharon";
const KEY7: &str = "CycleFive/kharon#7";
const NOW: &str = "2026-11-20T12:00:00Z";
/// When the label was put on.
const L: &str = "2026-11-20T09:00:00Z";
/// An earlier mark of a claim already followed.
const S: &str = "2026-11-20T08:00:00Z";
/// The item's last update, as the search shows it.
const U: &str = "2026-11-20T10:00:00Z";
const SEARCH: &str = "/search/issues?q=org:CycleFive is:open label:\"assigned:claude\",\"review:claude\"&sort=updated&order=desc&per_page=100";

fn me() -> Identity {
    Identity {
        id: "9".into(),
        login: "claudeduet[bot]".into(),
        name: "Claude Duet".into(),
    }
}

fn opts(repos: &[&str], skip_self: bool) -> GithubSource {
    let list = repos
        .iter()
        .map(|r| format!("\"{r}\""))
        .collect::<Vec<_>>()
        .join(",");
    let json = format!(
        r#"{{"broker":"B","repos":[{list}],"skip_self":{skip_self},"labels":{{"org":"CycleFive","tag":"claude"}}}}"#
    );
    serde_json::from_str(&json).unwrap()
}

fn run_with(
    f: &FakeFeed,
    o: &GithubSource,
    prev: Option<ClaimsState>,
) -> Result<Poll<ClaimsState>, PollError> {
    block_on(poll_claims(f, &me(), o, prev, epoch(NOW).unwrap()))
}

fn run(f: &FakeFeed, prev: Option<ClaimsState>) -> Poll<ClaimsState> {
    run_with(f, &opts(&[], true), prev).unwrap()
}

fn item_json(repo: &str, n: u64, updated: &str, labels: &[&str], pr: bool) -> String {
    let ls = labels
        .iter()
        .map(|l| format!(r#"{{"name":"{l}"}}"#))
        .collect::<Vec<_>>()
        .join(",");
    let pr = if pr { r#","pull_request":{"url":"x"}"# } else { "" };
    format!(
        r#"{{"number":{n},"title":"Item {n}","html_url":"https://gh/{repo}/{n}","updated_at":"{updated}","repository_url":"https://api.github.com/repos/{repo}","labels":[{ls}],"user":{{"login":"someone"}}{pr}}}"#
    )
}

fn search(f: &FakeFeed, total: u64, incomplete: bool, items: &[String]) {
    f.on(
        SEARCH,
        &format!(
            r#"{{"total_count":{total},"incomplete_results":{incomplete},"items":[{}]}}"#,
            items.join(",")
        ),
    );
}

fn search_items(f: &FakeFeed, items: &[String]) {
    search(f, items.len() as u64, false, items);
}

fn labeled(id: u64, label: &str, actor: &str, created: &str) -> String {
    format!(
        r#"{{"id":{id},"event":"labeled","created_at":"{created}","actor":{{"login":"{actor}"}},"label":{{"name":"{label}"}}}}"#
    )
}

fn plain_event(id: u64, kind: &str, actor: &str, created: &str) -> String {
    format!(r#"{{"id":{id},"event":"{kind}","created_at":"{created}","actor":{{"login":"{actor}"}}}}"#)
}

fn events(f: &FakeFeed, repo: &str, n: u64, evs: &[String]) {
    f.on(
        &format!("/repos/{repo}/issues/{n}/events?per_page=100&page=1"),
        &format!("[{}]", evs.join(",")),
    );
}

fn perm(f: &FakeFeed, login: &str, permission: &str, role: &str) {
    f.on(
        &format!("/repos/{R}/collaborators/{login}/permission"),
        &format!(r#"{{"permission":"{permission}","role_name":"{role}"}}"#),
    );
}

fn admin(f: &FakeFeed, login: &str) {
    perm(f, login, "admin", "admin");
}

fn comments_since(f: &FakeFeed, repo: &str, n: u64, since: &str, body: &str) {
    f.on(
        &format!("/repos/{repo}/issues/{n}/comments?since={since}&per_page=100&page=1"),
        body,
    );
}

fn comment(id: u64, user: &str, body: &str, created: &str) -> String {
    format!(
        r#"{{"id":{id},"body":"{body}","html_url":"https://gh/c/{id}","issue_url":"https://api.github.com/repos/{R}/issues/7","created_at":"{created}","user":{{"login":"{user}"}}}}"#
    )
}

fn claim(trusted: bool, since: &str, updated: &str, labels: &[(&str, u64)]) -> Claim {
    Claim {
        is_pr: false,
        title: "Item 7".into(),
        url: "https://gh/CycleFive/kharon/7".into(),
        updated_at: updated.into(),
        since: since.into(),
        label_events: labels
            .iter()
            .map(|(k, v)| (k.to_string(), LabelEvent { id: *v, trusted }))
            .collect(),
        trusted,
        reopened_event: 0,
    }
}

fn state_of(entries: Vec<(&str, Claim)>) -> ClaimsState {
    ClaimsState {
        claims: entries
            .into_iter()
            .map(|(k, c)| (k.to_string(), c))
            .collect::<BTreeMap<_, _>>(),
        ..ClaimsState::default()
    }
}

fn ids(rs: &[NewRecord]) -> Vec<&str> {
    rs.iter().map(|r| r.id.as_str()).collect()
}

fn tags(r: &NewRecord) -> Vec<&str> {
    r.tags.iter().map(String::as_str).collect()
}

/// A new issue 7 labelled `assigned:claude` by `actor`, with an empty activity reply.
fn new_issue_feed(actor: &str) -> FakeFeed {
    let f = FakeFeed::strict();
    search_items(&f, &[item_json(R, 7, U, &["assigned:claude"], false)]);
    events(&f, R, 7, &[labeled(500, "assigned:claude", actor, L)]);
    comments_since(&f, R, 7, L, "[]");
    f
}

fn the_claim_record(p: &Poll<ClaimsState>) -> &NewRecord {
    assert_eq!(p.records.len(), 1, "{:?}", ids(&p.records));
    &p.records[0]
}

#[test]
fn a_trusted_label_on_an_issue_is_mailed_once() {
    let f = new_issue_feed("lothrop");
    admin(&f, "lothrop");
    let p = run(&f, None);
    let r = the_claim_record(&p);
    assert_eq!(r.kind, Kind::Event);
    assert_eq!(r.id, "gh:CycleFive/kharon:claim:500");
    assert_eq!(r.actor, "lothrop");
    assert_eq!(r.reference, KEY7);
    assert_eq!(r.title, "labeled assigned:claude");
    assert_eq!(r.body, "Item 7");
    assert_eq!(r.url, "https://gh/CycleFive/kharon/7");
    assert_eq!(r.ts, L);
    assert_eq!(tags(r), ["routed:assigned"]);
    let c = &p.state.claims[KEY7];
    assert!(c.trusted && !c.is_pr);
    assert_eq!((c.since.as_str(), c.updated_at.as_str()), (U, U));
    assert_eq!(c.label_events["assigned:claude"].id, 500);

    let before = f.calls().len();
    let again = run(&f, Some(p.state.clone()));
    assert!(again.records.is_empty());
    assert_eq!(again.state, p.state);
    assert_eq!(f.calls().len(), before + 1, "{:?}", f.calls());
}

#[test]
fn a_review_label_on_a_pull_request_routes_review() {
    let f = FakeFeed::strict();
    search_items(&f, &[item_json(R, 4, U, &["review:claude"], true)]);
    events(&f, R, 4, &[labeled(510, "review:claude", "lothrop", L)]);
    admin(&f, "lothrop");
    comments_since(&f, R, 4, L, "[]");
    f.on(
        &format!("/repos/{R}/pulls/4/comments?since={L}&per_page=100&page=1"),
        "[]",
    );
    f.on(&format!("/repos/{R}/pulls/4/reviews?per_page=100&page=1"), "[]");
    let p = run(&f, None);
    let r = the_claim_record(&p);
    assert_eq!(r.id, "gh:CycleFive/kharon:claim:510");
    assert_eq!(r.title, "labeled review:claude");
    assert_eq!(tags(r), ["routed:review"]);
    assert!(p.state.claims["CycleFive/kharon#4"].is_pr);
}

#[test]
fn a_review_label_on_a_plain_issue_does_not_count() {
    let f = FakeFeed::strict();
    search_items(&f, &[item_json(R, 7, U, &["review:claude"], false)]);
    let p = run(&f, None);
    assert!(p.records.is_empty());
    assert!(p.state.claims.is_empty());
    assert_eq!(f.calls().len(), 1, "{:?}", f.calls());
}

fn trust_of(permission: &str, role: &str) -> bool {
    let f = new_issue_feed("lothrop");
    perm(&f, "lothrop", permission, role);
    let p = run(&f, None);
    let r = the_claim_record(&p);
    let trusted = tags(r) == ["routed:assigned"];
    assert_eq!(p.state.claims[KEY7].trusted, trusted);
    if !trusted {
        assert_eq!(tags(r), ["untrusted-label"]);
    }
    trusted
}

#[test]
fn triage_and_maintain_roles_are_trusted() {
    assert!(trust_of("read", "triage"));
    assert!(trust_of("write", "maintain"));
    assert!(trust_of("write", "write"));
    assert!(trust_of("admin", "admin"));
}

#[test]
fn a_reader_is_not_trusted() {
    assert!(!trust_of("read", "read"));
    assert!(!trust_of("none", "none"));
}

#[test]
fn a_bot_labeler_is_untrusted_without_a_lookup() {
    let f = new_issue_feed("cycle-five-muse[bot]");
    let p = run(&f, None);
    assert_eq!(tags(the_claim_record(&p)), ["untrusted-label"]);
    assert!(!p.state.claims[KEY7].trusted);
    assert!(
        !f.calls().iter().any(|c| c.contains("/collaborators/")),
        "{:?}",
        f.calls()
    );
}

#[test]
fn a_missing_labeler_is_untrusted() {
    let f = new_issue_feed("ghost");
    f.fail(&format!("/repos/{R}/collaborators/ghost/permission"), 404);
    let p = run(&f, None);
    assert_eq!(tags(the_claim_record(&p)), ["untrusted-label"]);
}

#[test]
fn a_failed_permission_lookup_fails_the_poll() {
    let f = new_issue_feed("lothrop");
    f.fail(&format!("/repos/{R}/collaborators/lothrop/permission"), 500);
    assert!(run_with(&f, &opts(&[], true), None).is_err());
}

#[test]
fn an_untrusted_claim_is_not_followed_and_not_mailed_again() {
    let f = FakeFeed::strict();
    let u2 = "2026-11-20T11:00:00Z";
    search_items(&f, &[item_json(R, 7, u2, &["assigned:claude"], false)]);
    events(&f, R, 7, &[labeled(500, "assigned:claude", "rando", L)]);
    let prev = state_of(vec![(KEY7, claim(false, U, U, &[("assigned:claude", 500)]))]);
    let p = run(&f, Some(prev));
    assert!(p.records.is_empty());
    let c = &p.state.claims[KEY7];
    assert!(!c.trusted);
    assert_eq!(c.updated_at, u2);
    assert!(
        !f.calls().iter().any(|c| c.contains("/comments")),
        "{:?}",
        f.calls()
    );
    assert!(f.calls().iter().any(|c| c.contains("/events")));
}

#[test]
fn a_trusted_relabel_of_an_untrusted_claim_is_mailed() {
    let f = FakeFeed::strict();
    let u2 = "2026-11-20T11:00:00Z";
    search_items(&f, &[item_json(R, 7, u2, &["assigned:claude"], false)]);
    events(
        &f,
        R,
        7,
        &[
            labeled(500, "assigned:claude", "rando", L),
            labeled(600, "assigned:claude", "lothrop", "2026-11-20T10:30:00Z"),
        ],
    );
    admin(&f, "lothrop");
    comments_since(&f, R, 7, "2026-11-20T10:30:00Z", "[]");
    let prev = state_of(vec![(KEY7, claim(false, U, U, &[("assigned:claude", 500)]))]);
    let p = run(&f, Some(prev));
    let r = the_claim_record(&p);
    assert_eq!(r.id, "gh:CycleFive/kharon:claim:600");
    assert_eq!(tags(r), ["routed:assigned"]);
    assert!(p.state.claims[KEY7].trusted);
}

/// A stored trusted claim of issue 7 (marks at `S`) that the search now shows updated at `U`.
fn followed_feed(evs: &[String]) -> (FakeFeed, ClaimsState) {
    let mut all = vec![labeled(500, "assigned:claude", "lothrop", "2026-11-20T07:00:00Z")];
    all.extend_from_slice(evs);
    followed_raw(&all)
}

/// Like `followed_feed`, with exactly these events.
fn followed_raw(evs: &[String]) -> (FakeFeed, ClaimsState) {
    let f = FakeFeed::strict();
    search_items(&f, &[item_json(R, 7, U, &["assigned:claude"], false)]);
    events(&f, R, 7, evs);
    let prev = state_of(vec![(
        KEY7,
        claim(true, S, "2026-11-20T08:30:00Z", &[("assigned:claude", 500)]),
    )]);
    (f, prev)
}

#[test]
fn activity_on_a_claim_in_an_unwatched_repo_is_mailed_as_claimed() {
    let (f, prev) = followed_feed(&[]);
    comments_since(
        &f,
        R,
        7,
        S,
        &format!(
            "[{},{}]",
            comment(31, "lothrop", "new one", "2026-11-20T09:30:00Z"),
            comment(30, "lothrop", "edited old one", "2026-11-20T07:00:00Z")
        ),
    );
    let p = run(&f, Some(prev));
    let r = the_claim_record(&p);
    assert_eq!(r.kind, Kind::IssueComment);
    assert_eq!(r.id, "gh:CycleFive/kharon:issue_comment:31");
    assert!(tags(r).contains(&"claimed"));
    let c = &p.state.claims[KEY7];
    assert_eq!((c.since.as_str(), c.updated_at.as_str()), (U, U));
}

#[test]
fn pull_request_claims_also_read_review_comments_and_reviews() {
    let f = FakeFeed::strict();
    search_items(&f, &[item_json(R, 4, U, &["assigned:claude"], true)]);
    events(
        &f,
        R,
        4,
        &[labeled(500, "assigned:claude", "lothrop", "2026-11-20T07:00:00Z")],
    );
    comments_since(&f, R, 4, S, "[]");
    f.on(
        &format!("/repos/{R}/pulls/4/comments?since={S}&per_page=100&page=1"),
        &format!(
            r#"[{{"id":60,"body":"nit","html_url":"https://gh/rc/60","pull_request_url":"https://api.github.com/repos/{R}/pulls/4","created_at":"2026-11-20T09:10:00Z","user":{{"login":"lothrop"}}}},
                {{"id":59,"body":"edited old","html_url":"https://gh/rc/59","pull_request_url":"https://api.github.com/repos/{R}/pulls/4","created_at":"2026-11-20T07:10:00Z","user":{{"login":"lothrop"}}}}]"#
        ),
    );
    f.on(
        &format!("/repos/{R}/pulls/4/reviews?per_page=100&page=1"),
        r#"[{"id":77,"state":"APPROVED","body":"ok","html_url":"https://gh/r/77","submitted_at":"2026-11-20T09:20:00Z","user":{"login":"lothrop"}},
            {"id":70,"state":"COMMENTED","body":"old","html_url":"https://gh/r/70","submitted_at":"2026-11-20T07:20:00Z","user":{"login":"lothrop"}}]"#,
    );
    let mut c = claim(true, S, "2026-11-20T08:30:00Z", &[("assigned:claude", 500)]);
    c.is_pr = true;
    let p = run(&f, Some(state_of(vec![("CycleFive/kharon#4", c)])));
    assert_eq!(
        ids(&p.records),
        [
            "gh:CycleFive/kharon:review_comment:60",
            "gh:CycleFive/kharon:review:77"
        ]
    );
    assert_eq!(p.records[1].kind, Kind::Review);
    assert!(p.records.iter().all(|r| tags(r).contains(&"claimed")));
}

#[test]
fn state_events_on_a_claim_are_mailed_but_its_own_label_event_is_not() {
    let (f, prev) = followed_raw(&[
        plain_event(499, "closed", "lothrop", "2026-11-20T07:30:00Z"),
        labeled(500, "assigned:claude", "lothrop", "2026-11-20T07:00:00Z"),
        plain_event(501, "reopened", "lothrop", "2026-11-20T09:30:00Z"),
        labeled(502, "assigned:claude", "lothrop", "2026-11-20T09:40:00Z"),
    ]);
    admin(&f, "lothrop");
    comments_since(&f, R, 7, S, "[]");
    let p = run(&f, Some(prev));
    assert_eq!(
        ids(&p.records),
        ["gh:CycleFive/kharon:event:501", "gh:CycleFive/kharon:claim:502"]
    );
    assert!(tags(&p.records[0]).contains(&"claimed"));
    assert_eq!(p.records[0].title, "reopened");
}

#[test]
fn a_mention_in_claimed_activity_keeps_routed_mention() {
    let (f, prev) = followed_feed(&[]);
    comments_since(
        &f,
        R,
        7,
        S,
        &format!(
            "[{}]",
            comment(32, "lothrop", "ping @claudeduet please", "2026-11-20T09:30:00Z")
        ),
    );
    let p = run(&f, Some(prev));
    let r = the_claim_record(&p);
    assert!(tags(r).contains(&"claimed") && tags(r).contains(&"routed:mention"));
}

#[test]
fn the_bots_own_comment_is_not_mailed() {
    let (f, prev) = followed_feed(&[]);
    comments_since(
        &f,
        R,
        7,
        S,
        &format!(
            "[{}]",
            comment(33, "claudeduet[bot]", "done", "2026-11-20T09:30:00Z")
        ),
    );
    assert!(run(&f, Some(prev)).records.is_empty());
}

#[test]
fn a_claim_in_a_watched_repo_is_not_fetched_here() {
    let (f, prev) = followed_feed(&[plain_event(501, "reopened", "lothrop", "2026-11-20T09:30:00Z")]);
    let p = run_with(&f, &opts(&[R], true), Some(prev)).unwrap();
    assert!(p.records.is_empty(), "{:?}", ids(&p.records));
    assert!(
        !f.calls()
            .iter()
            .any(|c| c.contains("/comments") || c.contains("/reviews")),
        "{:?}",
        f.calls()
    );
    assert!(p.state.claims.contains_key(KEY7));
}

#[test]
fn an_unchanged_claim_costs_no_requests() {
    let f = FakeFeed::strict();
    search_items(&f, &[item_json(R, 7, U, &["assigned:claude"], false)]);
    let prev = state_of(vec![(KEY7, claim(true, U, U, &[("assigned:claude", 500)]))]);
    let p = run(&f, Some(prev.clone()));
    assert_eq!(f.calls(), [SEARCH]);
    assert_eq!(p.state, prev);
}

#[test]
fn nothing_older_than_the_claim_is_mailed() {
    let f = new_issue_feed("lothrop");
    admin(&f, "lothrop");
    comments_since(
        &f,
        R,
        7,
        L,
        &format!(
            "[{}]",
            comment(20, "lothrop", "before the label", "2026-11-20T08:00:00Z")
        ),
    );
    let p = run(&f, None);
    assert_eq!(ids(&p.records), ["gh:CycleFive/kharon:claim:500"]);
    assert!(f.calls().contains(&format!(
        "/repos/{R}/issues/7/comments?since={L}&per_page=100&page=1"
    )));
}

fn item_reply(f: &FakeFeed, n: u64, state: &str, labels: &[&str]) {
    let ls = labels
        .iter()
        .map(|l| format!(r#"{{"name":"{l}"}}"#))
        .collect::<Vec<_>>()
        .join(",");
    f.on(
        &format!("/repos/{R}/issues/{n}"),
        &format!(
            r#"{{"state":"{state}","title":"Item {n}","html_url":"https://gh/x","repository_url":"https://api.github.com/repos/{R}","labels":[{ls}]}}"#
        ),
    );
}

fn stored(trusted: bool) -> ClaimsState {
    state_of(vec![(KEY7, claim(trusted, U, U, &[("assigned:claude", 500)]))])
}

fn released(f: &FakeFeed) -> Poll<ClaimsState> {
    search_items(f, &[]);
    run(f, Some(stored(true)))
}

#[test]
fn a_closed_item_is_released_once() {
    let f = FakeFeed::strict();
    item_reply(&f, 7, "closed", &["assigned:claude"]);
    let p = released(&f);
    let r = the_claim_record(&p);
    assert_eq!(r.kind, Kind::Event);
    assert_eq!(r.id, "gh:CycleFive/kharon:release:7:500");
    assert_eq!(r.title, "released: closed");
    assert_eq!(tags(r), ["released"]);
    assert_eq!(r.body, "Item 7");
    assert_eq!(r.reference, KEY7);
    assert!(p.state.claims.is_empty());
    let again = run(&f, Some(p.state));
    assert!(again.records.is_empty());
}

#[test]
fn a_removed_label_is_released() {
    let f = FakeFeed::strict();
    item_reply(&f, 7, "open", &["bug"]);
    let p = released(&f);
    assert_eq!(the_claim_record(&p).title, "released: label removed");
    assert!(p.state.claims.is_empty());
}

#[test]
fn a_vanished_item_is_released_as_gone() {
    let f = FakeFeed::strict();
    f.fail(&format!("/repos/{R}/issues/7"), 404);
    let p = released(&f);
    assert_eq!(the_claim_record(&p).title, "released: gone");
    let f = FakeFeed::strict();
    f.fail(&format!("/repos/{R}/issues/7"), 410);
    assert_eq!(the_claim_record(&released(&f)).title, "released: gone");
}

#[test]
fn a_failed_item_lookup_fails_the_poll() {
    let f = FakeFeed::strict();
    f.fail(&format!("/repos/{R}/issues/7"), 500);
    search_items(&f, &[]);
    assert!(run_with(&f, &opts(&[], true), Some(stored(true))).is_err());
}

#[test]
fn a_lagging_search_releases_nothing() {
    let f = FakeFeed::strict();
    item_reply(&f, 7, "open", &["assigned:claude"]);
    let p = released(&f);
    assert!(p.records.is_empty());
    assert_eq!(p.state, stored(true));
}

#[test]
fn an_incomplete_search_checks_no_vanished_claims() {
    let f = FakeFeed::strict();
    search(&f, 0, true, &[]);
    let p = run(&f, Some(stored(true)));
    assert!(p.records.is_empty());
    assert_eq!(p.state, stored(true));
    assert_eq!(f.calls(), [SEARCH]);
}

#[test]
fn an_untrusted_claim_is_dropped_without_a_note() {
    let f = FakeFeed::strict();
    item_reply(&f, 7, "closed", &["assigned:claude"]);
    search_items(&f, &[]);
    let p = run(&f, Some(stored(false)));
    assert!(p.records.is_empty());
    assert!(p.state.claims.is_empty());
}

#[test]
fn relabelling_after_a_release_is_a_new_claim() {
    let f = FakeFeed::strict();
    item_reply(&f, 7, "open", &["bug"]);
    let p = released(&f);
    assert!(p.state.claims.is_empty());
    search_items(&f, &[item_json(R, 7, U, &["assigned:claude"], false)]);
    events(
        &f,
        R,
        7,
        &[
            labeled(500, "assigned:claude", "lothrop", S),
            labeled(700, "assigned:claude", "lothrop", L),
        ],
    );
    admin(&f, "lothrop");
    comments_since(&f, R, 7, L, "[]");
    let again = run(&f, Some(p.state));
    assert_eq!(ids(&again.records), ["gh:CycleFive/kharon:claim:700"]);
}

#[test]
fn the_first_poll_mails_standing_assignments() {
    let f = FakeFeed::strict();
    search_items(
        &f,
        &[
            item_json(R, 7, U, &["assigned:claude"], false),
            item_json(R, 8, U, &["assigned:claude"], false),
        ],
    );
    admin(&f, "lothrop");
    for n in [7, 8] {
        events(&f, R, n, &[labeled(500 + n, "assigned:claude", "lothrop", L)]);
        comments_since(&f, R, n, L, "[]");
    }
    let p = run(&f, None);
    assert_eq!(
        ids(&p.records),
        ["gh:CycleFive/kharon:claim:507", "gh:CycleFive/kharon:claim:508"]
    );
}

#[test]
fn at_most_twenty_items_are_read_per_poll() {
    let f = FakeFeed::strict();
    admin(&f, "lothrop");
    let items: Vec<String> = (1..=25)
        .rev()
        .map(|n| {
            item_json(
                R,
                n,
                &format!("2026-11-20T10:00:{n:02}Z"),
                &["assigned:claude"],
                false,
            )
        })
        .collect();
    search_items(&f, &items);
    for n in 1..=25 {
        events(&f, R, n, &[labeled(1000 + n, "assigned:claude", "lothrop", L)]);
        comments_since(&f, R, n, L, "[]");
    }
    let p = run(&f, None);
    let reads = |calls: &[String]| calls.iter().filter(|c| c.contains("/events")).count();
    assert_eq!(reads(&f.calls()), 20);
    assert!(!f.calls().iter().any(|c| c.contains("/issues/21/")));
    assert_eq!(p.state.claims.len(), 20);
    assert_eq!(p.records.len(), 20);

    let before = f.calls().len();
    let next = run(&f, Some(p.state));
    assert_eq!(next.state.claims.len(), 25);
    assert_eq!(next.records.len(), 5);
    assert_eq!(reads(&f.calls()[before..]), 5);
}

#[test]
fn more_matches_than_a_page_adds_a_gap_record() {
    let f = FakeFeed::strict();
    let items: Vec<String> = (1..=100).map(|n| item_json(R, n, U, &["bug"], false)).collect();
    search(&f, 150, false, &items);
    let p = run(&f, None);
    let r = the_claim_record(&p);
    assert_eq!(r.kind, Kind::Gap);
    assert!(r.id.starts_with("gh:gap:labels:CycleFive:"), "{}", r.id);
    assert_eq!(r.title, "labelled items of CycleFive may have skipped items");
}

#[test]
fn a_failed_search_fails_the_poll() {
    let f = FakeFeed::strict();
    f.fail(SEARCH, 500);
    assert!(run_with(&f, &opts(&[], true), None).is_err());
}

#[test]
fn polling_without_a_labels_block_is_an_error() {
    let f = FakeFeed::strict();
    let o: GithubSource = serde_json::from_str(r#"{"broker":"B","repos":["a/b"]}"#).unwrap();
    assert!(run_with(&f, &o, None).is_err());
    assert!(f.calls().is_empty());
}

#[test]
fn the_search_request_is_exact() {
    let f = FakeFeed::strict();
    search_items(&f, &[]);
    run(&f, None);
    assert_eq!(f.calls()[0], SEARCH);
}

#[test]
fn claimed_numbers_lists_only_trusted_claims_of_that_repo() {
    let s = state_of(vec![
        (KEY7, claim(true, S, U, &[])),
        ("CycleFive/kharon#8", claim(false, S, U, &[])),
        ("CycleFive/kharon#9", claim(true, S, U, &[])),
        ("CycleFive/other#3", claim(true, S, U, &[])),
        ("CycleFive/kharon-x#4", claim(true, S, U, &[])),
    ]);
    assert_eq!(s.claimed_numbers(R), [7, 9]);
}

fn reopened(id: u64, created: &str) -> String {
    plain_event(id, "reopened", "lothrop", created)
}

#[test]
fn an_item_with_both_labels_gets_two_claim_records() {
    let f = FakeFeed::strict();
    search_items(
        &f,
        &[item_json(R, 4, U, &["assigned:claude", "review:claude"], true)],
    );
    events(
        &f,
        R,
        4,
        &[
            labeled(510, "review:claude", "lothrop", "2026-11-20T09:10:00Z"),
            labeled(500, "assigned:claude", "lothrop", L),
        ],
    );
    admin(&f, "lothrop");
    comments_since(&f, R, 4, L, "[]");
    f.on(
        &format!("/repos/{R}/pulls/4/comments?since={L}&per_page=100&page=1"),
        "[]",
    );
    f.on(&format!("/repos/{R}/pulls/4/reviews?per_page=100&page=1"), "[]");
    let p = run(&f, None);
    assert_eq!(
        ids(&p.records),
        ["gh:CycleFive/kharon:claim:500", "gh:CycleFive/kharon:claim:510"]
    );
    assert_eq!(tags(&p.records[0]), ["routed:assigned"]);
    assert_eq!(tags(&p.records[1]), ["routed:review"]);
    let c = &p.state.claims["CycleFive/kharon#4"];
    assert_eq!(c.label_events.len(), 2);
    assert_eq!((c.since.as_str(), c.updated_at.as_str()), (U, U));
}

#[test]
fn a_label_event_missing_from_the_events_read_stores_the_claim_and_mails_nothing() {
    // The search shows the label but the events do not (yet): nothing is stored, so the
    // item is read again on the next poll.
    let f = FakeFeed::strict();
    search_items(&f, &[item_json(R, 7, U, &["assigned:claude"], false)]);
    events(&f, R, 7, &[]);
    let p = run(&f, None);
    assert!(p.records.is_empty());
    assert!(p.state.claims.is_empty());
    let before = f.calls().len();
    let again = run(&f, Some(p.state.clone()));
    assert_eq!(f.calls().len(), before + 1, "{:?}", f.calls());
    assert_eq!(again.state, p.state);
}

#[test]
fn the_bots_own_label_is_stored_untrusted_and_not_mailed() {
    let f = new_issue_feed("claudeduet[bot]");
    let p = run(&f, None);
    assert!(p.records.is_empty());
    let c = &p.state.claims[KEY7];
    assert!(!c.trusted);
    assert_eq!(c.label_events["assigned:claude"].id, 500);
}

const REOPENED: &str = "2026-11-20T09:30:00Z";

#[test]
fn a_reopened_item_still_labelled_is_handed_over_again() {
    let f = FakeFeed::strict();
    search_items(&f, &[item_json(R, 7, U, &["assigned:claude"], false)]);
    events(
        &f,
        R,
        7,
        &[
            labeled(500, "assigned:claude", "lothrop", L),
            plain_event(800, "closed", "lothrop", "2026-11-20T09:10:00Z"),
            reopened(900, REOPENED),
        ],
    );
    admin(&f, "lothrop");
    comments_since(&f, R, 7, REOPENED, "[]");
    let p = run(&f, None);
    let r = the_claim_record(&p);
    assert_eq!(r.id, "gh:CycleFive/kharon:claim:500:900");
    assert_eq!(r.ts, REOPENED);
    assert_eq!(r.actor, "lothrop");
    assert_eq!(r.title, "labeled assigned:claude");
    assert_eq!(tags(r), ["routed:assigned"]);
    let c = &p.state.claims[KEY7];
    assert_eq!(c.reopened_event, 900);
    assert_eq!(c.label_events["assigned:claude"].id, 500);
    assert!(f.calls().contains(&format!(
        "/repos/{R}/issues/7/comments?since={REOPENED}&per_page=100&page=1"
    )));
}

#[test]
fn the_closing_note_after_a_reopen_has_its_own_id() {
    let f = FakeFeed::strict();
    item_reply(&f, 7, "closed", &["assigned:claude"]);
    search_items(&f, &[]);
    let mut c = claim(true, U, U, &[("assigned:claude", 500)]);
    c.reopened_event = 900;
    let p = run(&f, Some(state_of(vec![(KEY7, c)])));
    assert_eq!(the_claim_record(&p).id, "gh:CycleFive/kharon:release:7:500:900");
}

#[test]
fn a_reopen_before_the_label_event_changes_nothing() {
    let f = FakeFeed::strict();
    search_items(&f, &[item_json(R, 7, U, &["assigned:claude"], false)]);
    events(
        &f,
        R,
        7,
        &[
            reopened(300, "2026-11-20T08:00:00Z"),
            labeled(500, "assigned:claude", "lothrop", L),
        ],
    );
    admin(&f, "lothrop");
    comments_since(&f, R, 7, L, "[]");
    let p = run(&f, None);
    assert_eq!(the_claim_record(&p).id, "gh:CycleFive/kharon:claim:500");
    assert_eq!(p.state.claims[KEY7].reopened_event, 0);
}

#[test]
fn a_close_and_reopen_of_a_followed_claim_mails_the_event_and_updates_the_mark() {
    let (f, prev) = followed_feed(&[reopened(900, REOPENED)]);
    comments_since(&f, R, 7, S, "[]");
    let p = run(&f, Some(prev));
    assert_eq!(ids(&p.records), ["gh:CycleFive/kharon:event:900"]);
    assert_eq!(p.state.claims[KEY7].reopened_event, 900);
}

// ---- the final fix wave ----

fn events_page(f: &FakeFeed, repo: &str, n: u64, page: usize, evs: &[String]) {
    f.on(
        &format!("/repos/{repo}/issues/{n}/events?per_page=100&page={page}"),
        &format!("[{}]", evs.join(",")),
    );
}

fn comments_page(f: &FakeFeed, repo: &str, n: u64, since: &str, page: usize, cs: &[String]) {
    f.on(
        &format!("/repos/{repo}/issues/{n}/comments?since={since}&per_page=100&page={page}"),
        &format!("[{}]", cs.join(",")),
    );
}

fn pr_quiet(f: &FakeFeed, n: u64, since: &str) {
    f.on(
        &format!("/repos/{R}/pulls/{n}/comments?since={since}&per_page=100&page=1"),
        "[]",
    );
    f.on(&format!("/repos/{R}/pulls/{n}/reviews?per_page=100&page=1"), "[]");
}

fn unlabeled(id: u64, label: &str, actor: &str, created: &str) -> String {
    labeled(id, label, actor, created).replace("\"labeled\"", "\"unlabeled\"")
}

fn label_of<'a>(c: &'a Claim, name: &str) -> &'a LabelEvent {
    &c.label_events[name]
}

#[test]
fn the_newest_label_event_decides_trust() {
    let f = FakeFeed::strict();
    search_items(&f, &[item_json(R, 7, U, &["assigned:claude"], false)]);
    events(
        &f,
        R,
        7,
        &[
            labeled(500, "assigned:claude", "lothrop", S),
            labeled(520, "assigned:claude", "rando", L),
        ],
    );
    admin(&f, "lothrop");
    perm(&f, "rando", "read", "read");
    let p = run(&f, None);
    let r = the_claim_record(&p);
    assert_eq!(r.id, "gh:CycleFive/kharon:claim:520");
    assert_eq!(tags(r), ["untrusted-label"]);
    let c = &p.state.claims[KEY7];
    assert!(!c.trusted);
    assert_eq!(
        *label_of(c, "assigned:claude"),
        LabelEvent {
            id: 520,
            trusted: false
        }
    );
    assert!(
        !f.calls().iter().any(|c| c.contains("/comments")),
        "{:?}",
        f.calls()
    );
}

#[test]
fn a_label_removed_after_its_label_event_does_not_count() {
    let f = FakeFeed::strict();
    search_items(&f, &[item_json(R, 7, U, &["assigned:claude"], false)]);
    events(
        &f,
        R,
        7,
        &[
            labeled(500, "assigned:claude", "lothrop", L),
            unlabeled(510, "assigned:claude", "lothrop", "2026-11-20T09:30:00Z"),
        ],
    );
    let p = run(&f, None);
    assert!(p.records.is_empty(), "{:?}", ids(&p.records));
    assert!(p.state.claims.is_empty());
}

#[test]
fn a_label_put_back_after_its_removal_counts_again() {
    let f = FakeFeed::strict();
    search_items(&f, &[item_json(R, 7, U, &["assigned:claude"], false)]);
    events(
        &f,
        R,
        7,
        &[
            labeled(500, "assigned:claude", "lothrop", S),
            unlabeled(510, "assigned:claude", "lothrop", "2026-11-20T08:30:00Z"),
            labeled(520, "assigned:claude", "lothrop", L),
        ],
    );
    admin(&f, "lothrop");
    comments_since(&f, R, 7, L, "[]");
    let p = run(&f, None);
    assert_eq!(ids(&p.records), ["gh:CycleFive/kharon:claim:520"]);
}

#[test]
fn label_names_match_whatever_their_case() {
    let f = FakeFeed::strict();
    search_items(&f, &[item_json(R, 7, U, &["Assigned:Claude"], false)]);
    events(&f, R, 7, &[labeled(500, "Assigned:Claude", "lothrop", L)]);
    admin(&f, "lothrop");
    comments_since(&f, R, 7, L, "[]");
    let p = run(&f, None);
    let r = the_claim_record(&p);
    assert_eq!(r.title, "labeled Assigned:Claude");
    assert_eq!(tags(r), ["routed:assigned"]);
    assert!(p.state.claims[KEY7].trusted);
}

#[test]
fn a_vanished_claims_labels_are_matched_in_any_case() {
    let f = FakeFeed::strict();
    item_reply(&f, 7, "open", &["Assigned:Claude"]);
    let p = released(&f);
    assert!(p.records.is_empty());
    assert_eq!(p.state, stored(true));
}

fn pr_claim_with_two_labels() -> Claim {
    let mut c = claim(true, S, "2026-11-20T08:30:00Z", &[("assigned:claude", 500)]);
    c.is_pr = true;
    c.label_events.insert(
        "review:claude".into(),
        LabelEvent {
            id: 510,
            trusted: false,
        },
    );
    c
}

#[test]
fn a_claim_whose_trusted_label_goes_is_released_and_kept_untrusted() {
    let f = FakeFeed::strict();
    let u2 = "2026-11-20T11:00:00Z";
    search_items(&f, &[item_json(R, 4, u2, &["review:claude"], true)]);
    let evs = [
        labeled(500, "assigned:claude", "lothrop", S),
        labeled(510, "review:claude", "helper[bot]", "2026-11-20T08:10:00Z"),
        unlabeled(520, "assigned:claude", "lothrop", "2026-11-20T10:30:00Z"),
    ];
    events(&f, R, 4, &evs);
    let key4 = "CycleFive/kharon#4";
    let p = run(&f, Some(state_of(vec![(key4, pr_claim_with_two_labels())])));
    let r = the_claim_record(&p);
    assert_eq!(r.id, "gh:CycleFive/kharon:release:4:510");
    assert_eq!(r.title, "released: label removed");
    assert_eq!(tags(r), ["released"]);
    let c = &p.state.claims[key4];
    assert!(!c.trusted);
    assert_eq!(c.updated_at, u2);

    // A further changed poll mails no activity: the claim is not followed any more.
    let u3 = "2026-11-20T11:30:00Z";
    let g = FakeFeed::strict();
    search_items(&g, &[item_json(R, 4, u3, &["review:claude"], true)]);
    events(&g, R, 4, &evs);
    let again = run(&g, Some(p.state));
    assert!(again.records.is_empty(), "{:?}", ids(&again.records));
    assert!(
        !g.calls().iter().any(|c| c.contains("/comments")),
        "{:?}",
        g.calls()
    );
}

#[test]
fn an_untrusted_relabel_of_a_trusted_claim_releases_it() {
    let f = FakeFeed::strict();
    let u2 = "2026-11-20T11:00:00Z";
    search_items(&f, &[item_json(R, 7, u2, &["assigned:claude"], false)]);
    events(
        &f,
        R,
        7,
        &[
            labeled(500, "assigned:claude", "lothrop", S),
            unlabeled(510, "assigned:claude", "lothrop", L),
            labeled(520, "assigned:claude", "rando", "2026-11-20T10:30:00Z"),
        ],
    );
    perm(&f, "rando", "read", "read");
    let prev = state_of(vec![(KEY7, claim(true, S, U, &[("assigned:claude", 500)]))]);
    let p = run(&f, Some(prev));
    assert_eq!(
        ids(&p.records),
        [
            "gh:CycleFive/kharon:claim:520",
            "gh:CycleFive/kharon:release:7:500"
        ]
    );
    assert_eq!(tags(&p.records[0]), ["untrusted-label"]);
    assert_eq!(p.records[1].title, "released: label removed");
    let c = &p.state.claims[KEY7];
    assert!(!c.trusted);
    assert_eq!(
        *label_of(c, "assigned:claude"),
        LabelEvent {
            id: 520,
            trusted: false
        }
    );
}

#[test]
fn activity_before_the_trusted_label_is_not_mailed() {
    let f = FakeFeed::strict();
    let t = "2026-11-20T11:00:00Z";
    search_items(
        &f,
        &[item_json(
            R,
            4,
            "2026-11-20T12:30:00Z",
            &["assigned:claude", "review:claude"],
            true,
        )],
    );
    events(
        &f,
        R,
        4,
        &[
            labeled(500, "review:claude", "helper[bot]", "2026-11-20T08:00:00Z"),
            labeled(510, "assigned:claude", "lothrop", t),
        ],
    );
    admin(&f, "lothrop");
    comments_since(
        &f,
        R,
        4,
        t,
        &format!(
            "[{},{}]",
            comment(40, "lothrop", "before", "2026-11-20T09:00:00Z"),
            comment(41, "lothrop", "after", "2026-11-20T12:00:00Z")
        ),
    );
    pr_quiet(&f, 4, t);
    let p = run(&f, None);
    let comments: Vec<&str> = p
        .records
        .iter()
        .filter(|r| r.kind == Kind::IssueComment)
        .map(|r| r.id.as_str())
        .collect();
    assert_eq!(comments, ["gh:CycleFive/kharon:issue_comment:41"]);
    assert!(f.calls().contains(&format!(
        "/repos/{R}/issues/4/comments?since={t}&per_page=100&page=1"
    )));
    let c = &p.state.claims["CycleFive/kharon#4"];
    assert!(c.trusted);
}

#[test]
fn turning_trusted_moves_since_to_the_trusted_label() {
    let f = FakeFeed::strict();
    let t = "2026-11-20T10:30:00Z";
    let u2 = "2026-11-20T11:00:00Z";
    search_items(&f, &[item_json(R, 7, u2, &["assigned:claude"], false)]);
    events(
        &f,
        R,
        7,
        &[
            labeled(500, "assigned:claude", "rando", S),
            labeled(600, "assigned:claude", "lothrop", t),
        ],
    );
    admin(&f, "lothrop");
    comments_since(
        &f,
        R,
        7,
        t,
        &format!(
            "[{},{}]",
            comment(50, "lothrop", "between", "2026-11-20T10:15:00Z"),
            comment(51, "lothrop", "after", "2026-11-20T10:45:00Z")
        ),
    );
    let prev = state_of(vec![(KEY7, claim(false, S, U, &[("assigned:claude", 500)]))]);
    let p = run(&f, Some(prev));
    assert_eq!(
        ids(&p.records),
        [
            "gh:CycleFive/kharon:claim:600",
            "gh:CycleFive/kharon:issue_comment:51"
        ]
    );
    assert!(f.calls().contains(&format!(
        "/repos/{R}/issues/7/comments?since={t}&per_page=100&page=1"
    )));
}

#[test]
fn too_many_events_decide_nothing_and_are_reported_once() {
    let filler: Vec<String> = (1..=100)
        .map(|i| plain_event(i, "subscribed", "lothrop", S))
        .collect();
    let full = |f: &FakeFeed| {
        for page in 1..=10 {
            events_page(f, R, 7, page, &filler);
        }
    };
    let gap_id = format!("gh:gap:events:{R}#7:{U}");

    // A new item: not stored.
    let f = FakeFeed::strict();
    search_items(&f, &[item_json(R, 7, U, &["assigned:claude"], false)]);
    full(&f);
    let p = run(&f, None);
    let r = the_claim_record(&p);
    assert_eq!(r.kind, Kind::Gap);
    assert_eq!(r.id, gap_id);
    assert_eq!(r.title, format!("events of {R}#7 are too many to read"));
    assert_eq!(r.reference, format!("{R}#7"));
    assert!(p.state.claims.is_empty());

    // Reported once: the same item, unchanged, is not read again.
    let calls = f.calls().len();
    let again = run(&f, Some(p.state.clone()));
    assert!(again.records.is_empty(), "{:?}", ids(&again.records));
    assert_eq!(f.calls().len(), calls + 1, "{:?}", f.calls());

    // A stored item: left exactly as it was.
    let g = FakeFeed::strict();
    search_items(&g, &[item_json(R, 7, U, &["assigned:claude"], false)]);
    full(&g);
    let prev = state_of(vec![(
        KEY7,
        claim(true, S, "2026-11-20T08:30:00Z", &[("assigned:claude", 500)]),
    )]);
    let q = run(&g, Some(prev.clone()));
    assert_eq!(ids(&q.records), [gap_id.as_str()]);
    assert_eq!(q.state, prev);
}

#[test]
fn a_new_claim_in_a_watched_repo_mails_what_happened_since_the_label() {
    let f = new_issue_feed("lothrop");
    admin(&f, "lothrop");
    comments_since(
        &f,
        R,
        7,
        L,
        &format!("[{}]", comment(60, "lothrop", "after", "2026-11-20T09:30:00Z")),
    );
    let o = opts(&[R], true);
    let p = run_with(&f, &o, None).unwrap();
    assert_eq!(
        ids(&p.records),
        [
            "gh:CycleFive/kharon:claim:500",
            "gh:CycleFive/kharon:issue_comment:60"
        ]
    );
    assert!(tags(&p.records[1]).contains(&"claimed"));

    // Later polls leave a watched repository's activity to the per-repo poll.
    let u2 = "2026-11-20T11:00:00Z";
    let g = FakeFeed::strict();
    search_items(&g, &[item_json(R, 7, u2, &["assigned:claude"], false)]);
    events(&g, R, 7, &[labeled(500, "assigned:claude", "lothrop", L)]);
    let q = run_with(&g, &o, Some(p.state)).unwrap();
    assert!(q.records.is_empty());
    assert!(
        !g.calls().iter().any(|c| c.contains("/comments")),
        "{:?}",
        g.calls()
    );
}

#[test]
fn a_repo_listed_in_another_case_is_still_watched() {
    let (f, prev) = followed_feed(&[reopened(501, "2026-11-20T09:30:00Z")]);
    let p = run_with(&f, &opts(&["cyclefive/Kharon"], true), Some(prev)).unwrap();
    assert!(p.records.is_empty(), "{:?}", ids(&p.records));
    assert!(
        !f.calls().iter().any(|c| c.contains("/comments")),
        "{:?}",
        f.calls()
    );
}

#[test]
fn claimed_numbers_compares_repository_names_whatever_their_case() {
    let s = state_of(vec![(KEY7, claim(true, S, U, &[]))]);
    assert_eq!(s.claimed_numbers("cyclefive/Kharon"), [7]);
}

#[test]
fn activity_in_the_same_second_as_the_mark_is_mailed() {
    let (f, prev) = followed_feed(&[]);
    comments_since(
        &f,
        R,
        7,
        S,
        &format!("[{}]", comment(70, "lothrop", "on the mark", S)),
    );
    let p = run(&f, Some(prev));
    assert_eq!(ids(&p.records), ["gh:CycleFive/kharon:issue_comment:70"]);
}

#[test]
fn a_long_thread_is_read_page_by_page() {
    let (f, prev) = followed_feed(&[]);
    let at = "2026-11-20T09:30:00Z";
    let page = |from: u64, n: u64| -> Vec<String> {
        (from..from + n).map(|i| comment(i, "lothrop", "x", at)).collect()
    };
    comments_page(&f, R, 7, S, 1, &page(1000, 100));
    comments_page(&f, R, 7, S, 2, &page(2000, 50));
    let p = run(&f, Some(prev));
    assert_eq!(p.records.len(), 150);
    let reads: Vec<String> = f
        .calls()
        .into_iter()
        .filter(|c| c.contains("/comments"))
        .collect();
    assert_eq!(
        reads,
        [
            format!("/repos/{R}/issues/7/comments?since={S}&per_page=100&page=1"),
            format!("/repos/{R}/issues/7/comments?since={S}&per_page=100&page=2"),
        ]
    );
}

#[test]
fn ten_full_pages_of_comments_add_a_gap_record_and_still_advance() {
    let (f, prev) = followed_feed(&[]);
    let at = "2026-11-20T09:30:00Z";
    for page in 1..=10u64 {
        let cs: Vec<String> = (0..100)
            .map(|i| comment(page * 1000 + i, "lothrop", "x", at))
            .collect();
        comments_page(&f, R, 7, S, page as usize, &cs);
    }
    let p = run(&f, Some(prev));
    let gaps: Vec<&NewRecord> = p.records.iter().filter(|r| r.kind == Kind::Gap).collect();
    assert_eq!(gaps.len(), 1);
    assert_eq!(
        gaps[0].id,
        format!("gh:gap:comments:{R}#7:{}", epoch(NOW).unwrap())
    );
    assert_eq!(gaps[0].title, format!("comments of {R}#7 may have skipped items"));
    assert_eq!(p.records.len(), 1001);
    assert_eq!(p.state.claims[KEY7].since, U);
}

#[test]
fn a_poll_stops_reading_items_once_its_request_budget_is_spent() {
    let f = FakeFeed::strict();
    admin(&f, "lothrop");
    let items: Vec<String> = (1..=20)
        .rev()
        .map(|n| {
            item_json(
                R,
                n,
                &format!("2026-11-20T10:00:{n:02}Z"),
                &["assigned:claude"],
                false,
            )
        })
        .collect();
    search_items(&f, &items);
    for n in 1..=20u64 {
        // 250 events: three pages, the label event the newest.
        let filler = |from: u64, k: u64| -> Vec<String> {
            (from..from + k)
                .map(|i| plain_event(i, "subscribed", "lothrop", S))
                .collect()
        };
        events_page(&f, R, n, 1, &filler(1, 100));
        events_page(&f, R, n, 2, &filler(101, 100));
        let mut last = filler(201, 49);
        last.push(labeled(5000 + n, "assigned:claude", "lothrop", L));
        events_page(&f, R, n, 3, &last);
        comments_page(&f, R, n, L, 1, &[]);
    }
    let p = run(&f, None);
    let after_search = f.calls().len() - 1;
    // 60, plus the one item that was already being read when the count reached it.
    assert!((60..=65).contains(&after_search), "{after_search}");
    assert!(p.state.claims.len() < 20, "{}", p.state.claims.len());
    assert!(!p.state.claims.is_empty());

    let next = run(&f, Some(p.state));
    assert_eq!(next.state.claims.len(), 20);
}

#[test]
fn a_transferred_item_is_released_as_gone() {
    let f = FakeFeed::strict();
    f.on(
        &format!("/repos/{R}/issues/7"),
        r#"{"state":"open","title":"Item 7","html_url":"https://gh/x","repository_url":"https://api.github.com/repos/Elsewhere/kharon","labels":[{"name":"assigned:claude"}]}"#,
    );
    let p = released(&f);
    assert_eq!(the_claim_record(&p).title, "released: gone");
    assert!(p.state.claims.is_empty());
}

#[test]
fn a_renamed_repository_in_another_case_is_still_the_same_item() {
    let f = FakeFeed::strict();
    f.on(
        &format!("/repos/{R}/issues/7"),
        r#"{"state":"open","title":"Item 7","html_url":"https://gh/x","repository_url":"https://api.github.com/repos/cyclefive/KHARON","labels":[{"name":"assigned:claude"}]}"#,
    );
    let p = released(&f);
    assert!(p.records.is_empty());
}

fn two_label_pr_search(f: &FakeFeed, labels: &[&str]) -> &'static str {
    let u2 = "2026-11-20T11:00:00Z";
    search_items(f, &[item_json(R, 4, u2, labels, true)]);
    u2
}

const KEY4: &str = "CycleFive/kharon#4";

fn both_labels_on_events() -> [String; 2] {
    [
        labeled(500, "assigned:claude", "lothrop", S),
        labeled(510, "review:claude", "helper[bot]", "2026-11-20T08:10:00Z"),
    ]
}

#[test]
fn a_label_the_search_does_not_list_is_checked_on_the_item() {
    let f = FakeFeed::strict();
    two_label_pr_search(&f, &["review:claude"]);
    events(&f, R, 4, &both_labels_on_events());
    item_reply(&f, 4, "open", &["assigned:claude", "review:claude"]);
    comments_since(&f, R, 4, S, "[]");
    pr_quiet(&f, 4, S);
    let prev = state_of(vec![(KEY4, pr_claim_with_two_labels())]);
    let p = run(&f, Some(prev));
    assert!(p.records.is_empty(), "{:?}", ids(&p.records));
    assert!(p.state.claims[KEY4].trusted);
    assert_eq!(p.state.claims[KEY4].label_events.len(), 2);
    let lookups = f
        .calls()
        .iter()
        .filter(|c| c.as_str() == format!("/repos/{R}/issues/4"))
        .count();
    assert_eq!(lookups, 1, "{:?}", f.calls());
}

#[test]
fn a_label_really_removed_drops_its_entry() {
    let f = FakeFeed::strict();
    two_label_pr_search(&f, &["review:claude"]);
    let mut evs = both_labels_on_events().to_vec();
    evs.push(unlabeled(
        520,
        "assigned:claude",
        "lothrop",
        "2026-11-20T10:30:00Z",
    ));
    events(&f, R, 4, &evs);
    item_reply(&f, 4, "open", &["review:claude"]);
    let prev = state_of(vec![(KEY4, pr_claim_with_two_labels())]);
    let p = run(&f, Some(prev));
    let r = the_claim_record(&p);
    assert_eq!(r.title, "released: label removed");
    let c = &p.state.claims[KEY4];
    assert!(!c.trusted);
    assert_eq!(c.label_events.keys().collect::<Vec<_>>(), ["review:claude"]);
}

#[test]
fn closing_notes_never_reuse_an_id() {
    // 1. The trusted label goes: the note carries the latest id the claim held (510).
    let f = FakeFeed::strict();
    two_label_pr_search(&f, &["review:claude"]);
    let mut evs = both_labels_on_events().to_vec();
    evs.push(unlabeled(
        520,
        "assigned:claude",
        "lothrop",
        "2026-11-20T10:30:00Z",
    ));
    events(&f, R, 4, &evs);
    let prev = state_of(vec![(KEY4, pr_claim_with_two_labels())]);
    let p = run(&f, Some(prev));
    assert_eq!(the_claim_record(&p).id, "gh:CycleFive/kharon:release:4:510");

    // 2. A trusted person labels again (600): a new claim record.
    let g = FakeFeed::strict();
    search_items(
        &g,
        &[item_json(
            R,
            4,
            "2026-11-20T11:30:00Z",
            &["assigned:claude", "review:claude"],
            true,
        )],
    );
    evs.push(labeled(600, "assigned:claude", "lothrop", "2026-11-20T11:10:00Z"));
    events(&g, R, 4, &evs);
    admin(&g, "lothrop");
    comments_since(&g, R, 4, "2026-11-20T11:10:00Z", "[]");
    pr_quiet(&g, 4, "2026-11-20T11:10:00Z");
    let q = run(&g, Some(p.state));
    assert_eq!(ids(&q.records), ["gh:CycleFive/kharon:claim:600"]);

    // 3. The item closes: the note ends in 600, not 510.
    let h = FakeFeed::strict();
    item_reply(&h, 4, "closed", &["assigned:claude"]);
    search_items(&h, &[]);
    let r = run(&h, Some(q.state));
    assert_eq!(the_claim_record(&r).id, "gh:CycleFive/kharon:release:4:600");
}

#[test]
fn a_skipped_item_is_not_read_again_until_it_changes() {
    let f = FakeFeed::strict();
    search_items(&f, &[item_json(R, 7, U, &["assigned:claude"], false)]);
    events(
        &f,
        R,
        7,
        &[
            labeled(500, "assigned:claude", "lothrop", S),
            unlabeled(510, "assigned:claude", "lothrop", L),
        ],
    );
    let p = run(&f, None);
    assert!(p.state.claims.is_empty());
    assert_eq!(p.state.skipped[KEY7], U);

    let calls = f.calls().len();
    let again = run(&f, Some(p.state.clone()));
    assert_eq!(f.calls().len(), calls + 1, "{:?}", f.calls());
    assert_eq!(again.state, p.state);

    let u2 = "2026-11-20T11:00:00Z";
    search_items(&f, &[item_json(R, 7, u2, &["assigned:claude"], false)]);
    let calls = f.calls().len();
    let third = run(&f, Some(p.state));
    assert_eq!(f.calls().len(), calls + 2, "{:?}", f.calls());
    assert_eq!(third.state.skipped[KEY7], u2);
}

#[test]
fn a_skipped_item_that_becomes_a_claim_leaves_the_skipped_list() {
    let f = new_issue_feed("lothrop");
    admin(&f, "lothrop");
    let mut prev = ClaimsState::default();
    prev.skipped.insert(KEY7.into(), "2026-11-20T08:00:00Z".into());
    let p = run(&f, Some(prev));
    assert!(p.state.claims.contains_key(KEY7));
    assert!(p.state.skipped.is_empty());
}

#[test]
fn skipped_items_no_longer_listed_are_forgotten() {
    let mut prev = ClaimsState::default();
    prev.skipped.insert(KEY7.into(), U.into());
    prev.skipped.insert("CycleFive/kharon#9".into(), U.into());
    let f = FakeFeed::strict();
    search_items(&f, &[item_json(R, 7, U, &["assigned:claude"], false)]);
    let p = run(&f, Some(prev.clone()));
    assert_eq!(p.state.skipped.keys().collect::<Vec<_>>(), [KEY7]);
    // An incomplete search says nothing about what is missing.
    let g = FakeFeed::strict();
    search(&g, 0, true, &[]);
    assert_eq!(run(&g, Some(prev.clone())).state, prev);
}

#[test]
fn the_reopen_behind_a_claim_is_never_mailed_as_activity() {
    let (f, mut prev) = followed_feed(&[reopened(900, REOPENED)]);
    let c = prev.claims.get_mut(KEY7).unwrap();
    c.since = REOPENED.into();
    c.reopened_event = 900;
    comments_since(&f, R, 7, REOPENED, "[]");
    let p = run(&f, Some(prev));
    assert!(p.records.is_empty(), "{:?}", ids(&p.records));
}

#[test]
fn a_missing_repository_url_means_the_same_repository() {
    let f = FakeFeed::strict();
    f.on(
        &format!("/repos/{R}/issues/7"),
        r#"{"state":"open","title":"Item 7","html_url":"https://gh/x","labels":[{"name":"assigned:claude"}]}"#,
    );
    let p = released(&f);
    assert!(p.records.is_empty());
    assert_eq!(p.state, stored(true));
}
