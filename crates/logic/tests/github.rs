mod support;

use mailroom_logic::config::GithubSource;
use mailroom_logic::feed::{Identity, Poll, PollError};
use mailroom_logic::github::{poll_repo, GithubState};
use mailroom_logic::record::{Kind, NewRecord};
use mailroom_logic::time::epoch;
use pollster::block_on;
use support::FakeFeed;

const R: &str = "CycleFive/kharon";
const LAST_CHECK: &str = "2026-11-18T10:00:00Z";
const NOW: &str = "2026-11-18T12:00:00Z";

const ISSUES: &str = "/repos/CycleFive/kharon/issues?state=all&sort=updated&direction=desc&per_page=50";
const PULLS: &str = "/repos/CycleFive/kharon/pulls?state=all&sort=updated&direction=desc&per_page=50";
const ICOMMENTS: &str = "/repos/CycleFive/kharon/issues/comments?sort=created&direction=desc&per_page=50";
const RCOMMENTS: &str = "/repos/CycleFive/kharon/pulls/comments?sort=created&direction=desc&per_page=50";
const EVENTS: &str = "/repos/CycleFive/kharon/issues/events?per_page=50";

fn me() -> Identity {
    Identity {
        id: "9".into(),
        login: "claudeduet[bot]".into(),
        name: "Claude Duet".into(),
    }
}

fn opts(review_label: Option<&str>, skip_self: bool) -> GithubSource {
    let label = review_label
        .map(|l| format!(r#","review_label":"{l}""#))
        .unwrap_or_default();
    let json = format!(r#"{{"broker":"B","repos":["{R}"],"skip_self":{skip_self}{label}}}"#);
    serde_json::from_str(&json).unwrap()
}

fn state() -> GithubState {
    GithubState {
        last_issue_number: 5,
        last_pr_number: 5,
        last_issue_comment_id: 100,
        last_review_comment_id: 100,
        last_event_id: 100,
        last_check_at: LAST_CHECK.into(),
    }
}

/// A feed with all five always-made requests registered as empty lists.
fn feed() -> FakeFeed {
    let f = FakeFeed::new();
    for p in [ISSUES, PULLS, ICOMMENTS, RCOMMENTS, EVENTS] {
        f.on(p, "[]");
    }
    f
}

fn try_poll(
    f: &FakeFeed,
    o: &GithubSource,
    prev: Option<GithubState>,
) -> Result<Poll<GithubState>, PollError> {
    block_on(poll_repo(f, &me(), R, o, prev, epoch(NOW).unwrap()))
}

fn poll(f: &FakeFeed, prev: Option<GithubState>) -> Poll<GithubState> {
    try_poll(f, &opts(Some("review:claude"), true), prev).unwrap()
}

fn issue(n: u64, login: &str, extra: &str) -> String {
    format!(
        r#"{{"number":{n},"title":"Issue {n}","body":"body {n}","html_url":"https://gh/i/{n}","created_at":"2026-11-18T11:00:00Z","updated_at":"2026-11-18T11:00:00Z","user":{{"login":"{login}"}},"state":"open"{extra}}}"#
    )
}

fn pull(n: u64, login: &str, updated: &str, extra: &str) -> String {
    format!(
        r#"{{"number":{n},"title":"PR {n}","body":"pr body {n}","html_url":"https://gh/p/{n}","created_at":"2026-11-18T11:00:00Z","updated_at":"{updated}","user":{{"login":"{login}"}},"head":{{"ref":"x"}}{extra}}}"#
    )
}

fn icomment(id: u64, login: &str, body: &str, issue_no: u64, ts: &str) -> String {
    format!(
        r#"{{"id":{id},"body":"{body}","html_url":"https://gh/c/{id}","issue_url":"https://api.github.com/repos/CycleFive/kharon/issues/{issue_no}","created_at":"{ts}","user":{{"login":"{login}"}}}}"#
    )
}

fn rcomment(id: u64, login: &str, body: &str, pr_no: u64) -> String {
    format!(
        r#"{{"id":{id},"body":"{body}","html_url":"https://gh/rc/{id}","pull_request_url":"https://api.github.com/repos/CycleFive/kharon/pulls/{pr_no}","created_at":"2026-11-18T11:00:00Z","user":{{"login":"{login}"}}}}"#
    )
}

fn review(id: u64, login: &str, state: &str, submitted: Option<&str>) -> String {
    let sub = submitted
        .map(|s| format!(r#""submitted_at":"{s}""#))
        .unwrap_or_else(|| r#""submitted_at":null"#.to_string());
    format!(
        r#"{{"id":{id},"state":"{state}","body":"looks good","html_url":"https://gh/r/{id}",{sub},"user":{{"login":"{login}"}}}}"#
    )
}

fn event(id: u64, kind: &str, actor: &str, issue_author: &str, extra: &str) -> String {
    format!(
        r#"{{"id":{id},"event":"{kind}","created_at":"2026-11-18T11:00:00Z","actor":{{"login":"{actor}"}},"issue":{{"number":3,"html_url":"https://gh/i/3","user":{{"login":"{issue_author}"}}}}{extra}}}"#
    )
}

fn list(items: &[String]) -> String {
    format!("[{}]", items.join(","))
}

fn tags(r: &NewRecord) -> Vec<&str> {
    r.tags.iter().map(String::as_str).collect()
}

#[test]
fn first_contact_sets_watermarks_and_emits_nothing() {
    let f = feed();
    f.on(ISSUES, &list(&[issue(7, "a", ""), issue(3, "a", "")]));
    f.on(PULLS, &list(&[pull(9, "a", "2026-11-18T11:30:00Z", "")]));
    f.on(
        ICOMMENTS,
        &list(&[icomment(55, "a", "hi", 7, "2026-11-18T11:00:00Z")]),
    );
    f.on(RCOMMENTS, &list(&[rcomment(66, "a", "hi", 9)]));
    f.on(EVENTS, &list(&[event(77, "closed", "a", "a", "")]));
    let p = poll(&f, None);
    assert!(p.records.is_empty());
    assert_eq!(
        p.state,
        GithubState {
            last_issue_number: 7,
            last_pr_number: 9,
            last_issue_comment_id: 55,
            last_review_comment_id: 66,
            last_event_id: 77,
            last_check_at: "2026-11-18T11:59:00Z".into(),
        }
    );
    assert!(f.calls().iter().all(|c| !c.contains("/reviews")));
    assert_eq!(f.calls().len(), 5);
}

#[test]
fn new_issue_becomes_record() {
    let f = feed();
    f.on(ISSUES, &list(&[issue(7, "alice", ""), issue(5, "bob", "")]));
    let p = poll(&f, Some(state()));
    assert_eq!(p.records.len(), 1);
    let r = &p.records[0];
    assert_eq!(r.kind, Kind::Issue);
    assert_eq!(r.id, "gh:CycleFive/kharon:issue:7");
    assert_eq!(r.reference, "CycleFive/kharon#7");
    assert_eq!(r.actor, "alice");
    assert_eq!(r.title, "Issue 7");
    assert_eq!(r.body, "body 7");
    assert_eq!(r.url, "https://gh/i/7");
    assert_eq!(r.ts, "2026-11-18T11:00:00Z");
    assert!(r.tags.is_empty());
    assert_eq!(p.state.last_issue_number, 7);
}

#[test]
fn pull_requests_in_issue_list_are_skipped() {
    let f = feed();
    f.on(
        ISSUES,
        &list(&[issue(8, "alice", r#","pull_request":{"url":"x"}"#)]),
    );
    let p = poll(&f, Some(state()));
    assert!(p.records.is_empty());
}

#[test]
fn issue_assigned_to_bot_is_tagged() {
    let f = feed();
    f.on(
        ISSUES,
        &list(&[issue(7, "alice", r#","assignees":[{"login":"claudeduet[bot]"}]"#)]),
    );
    let p = poll(&f, Some(state()));
    assert_eq!(tags(&p.records[0]), ["routed:assigned"]);
}

#[test]
fn pr_with_review_label_or_requested_reviewer_is_routed_review() {
    let f = feed();
    f.on(
        PULLS,
        &list(&[
            pull(
                7,
                "alice",
                "2026-11-18T09:00:00Z",
                r#","labels":[{"name":"review:claude"}]"#,
            ),
            pull(
                6,
                "alice",
                "2026-11-18T09:00:00Z",
                r#","requested_reviewers":[{"login":"claudeduet[bot]"}]"#,
            ),
        ]),
    );
    let p = poll(&f, Some(state()));
    assert_eq!(p.records.len(), 2);
    for r in &p.records {
        assert_eq!(r.kind, Kind::Pr);
        assert_eq!(tags(r), ["routed:review"]);
    }
    assert_eq!(p.records[0].id, "gh:CycleFive/kharon:pr:7");
    assert_eq!(p.state.last_pr_number, 7);
}

#[test]
fn mention_is_case_insensitive_and_uses_slug() {
    let f = feed();
    f.on(
        ISSUES,
        &list(&[issue(7, "alice", "").replace("body 7", "@ClaudeDuet please")]),
    );
    let p = poll(&f, Some(state()));
    assert_eq!(tags(&p.records[0]), ["routed:mention"]);
}

#[test]
fn comment_on_bots_pr_is_mine() {
    let f = feed();
    f.on(
        PULLS,
        &list(&[pull(4, "claudeduet[bot]", "2026-11-18T09:00:00Z", "")]),
    );
    f.on(
        ICOMMENTS,
        &list(&[
            icomment(101, "alice", "on mine", 4, "2026-11-18T11:00:00Z"),
            icomment(102, "alice", "on other", 3, "2026-11-18T11:00:00Z"),
        ]),
    );
    f.on(RCOMMENTS, &list(&[rcomment(103, "alice", "inline", 4)]));
    let p = poll(&f, Some(state()));
    assert_eq!(p.records.len(), 3);
    let by = |id: &str| p.records.iter().find(|r| r.id.ends_with(id)).unwrap();
    assert_eq!(tags(by("issue_comment:101")), ["mine"]);
    assert!(by("issue_comment:102").tags.is_empty());
    assert_eq!(by("review_comment:103").kind, Kind::ReviewComment);
    assert_eq!(tags(by("review_comment:103")), ["mine"]);
    assert_eq!(by("issue_comment:101").title, "");
    assert_eq!(by("issue_comment:101").reference, "CycleFive/kharon#4");
}

fn with_reviews(f: &FakeFeed, reviews: &[String]) {
    f.on(
        PULLS,
        &list(&[pull(4, "claudeduet[bot]", "2026-11-18T11:00:00Z", "")]),
    );
    f.on(
        "/repos/CycleFive/kharon/pulls/4/reviews?per_page=50",
        &list(reviews),
    );
}

#[test]
fn review_is_kept_only_when_submitted_after_last_check() {
    let f = feed();
    with_reviews(
        &f,
        &[
            review(1, "alice", "COMMENTED", Some("2026-11-18T09:00:00Z")),
            review(2, "bob", "APPROVED", Some("2026-11-18T10:30:00Z")),
        ],
    );
    let p = poll(&f, Some(state()));
    let reviews: Vec<_> = p.records.iter().filter(|r| r.kind == Kind::Review).collect();
    assert_eq!(reviews.len(), 1);
    let r = reviews[0];
    assert_eq!(r.title, "APPROVED");
    assert_eq!(r.id, "gh:CycleFive/kharon:review:2");
    assert_eq!(r.ts, "2026-11-18T10:30:00Z");
    assert_eq!(r.reference, "CycleFive/kharon#4");
    assert_eq!(tags(r), ["mine"]);
}

#[test]
fn pending_review_without_submitted_at_is_skipped() {
    let f = feed();
    with_reviews(&f, &[review(1, "alice", "PENDING", None)]);
    let p = poll(&f, Some(state()));
    assert!(p.records.iter().all(|r| r.kind != Kind::Review));
}

#[test]
fn reviews_fetched_only_for_pulls_updated_since_last_check() {
    let f = feed();
    f.on(
        PULLS,
        &list(&[
            pull(4, "a", "2026-11-18T11:00:00Z", ""),
            pull(3, "a", "2026-11-18T09:00:00Z", ""),
        ]),
    );
    f.on("/repos/CycleFive/kharon/pulls/4/reviews?per_page=50", "[]");
    poll(&f, Some(state()));
    let reviews: Vec<_> = f.calls().into_iter().filter(|c| c.contains("/reviews")).collect();
    assert_eq!(reviews, ["/repos/CycleFive/kharon/pulls/4/reviews?per_page=50"]);
}

#[test]
fn labeled_event_with_review_label_is_routed_review() {
    let f = feed();
    f.on(
        EVENTS,
        &list(&[event(
            101,
            "labeled",
            "alice",
            "alice",
            r#","label":{"name":"review:claude"}"#,
        )]),
    );
    let p = poll(&f, Some(state()));
    let r = &p.records[0];
    assert_eq!(r.kind, Kind::Event);
    assert_eq!(r.title, "labeled review:claude");
    assert_eq!(r.id, "gh:CycleFive/kharon:event:101");
    assert_eq!(r.actor, "alice");
    assert_eq!(r.body, "");
    assert_eq!(r.url, "https://gh/i/3");
    assert_eq!(r.reference, "CycleFive/kharon#3");
    assert_eq!(tags(r), ["routed:review"]);
}

#[test]
fn assigned_event_for_someone_else_has_no_routed_tag() {
    let f = feed();
    f.on(
        EVENTS,
        &list(&[
            event(
                101,
                "assigned",
                "alice",
                "alice",
                r#","assignee":{"login":"bob"}"#,
            ),
            event(
                102,
                "assigned",
                "alice",
                "alice",
                r#","assignee":{"login":"claudeduet[bot]"}"#,
            ),
            event(
                103,
                "review_requested",
                "alice",
                "claudeduet[bot]",
                r#","requested_reviewer":{"login":"claudeduet[bot]"}"#,
            ),
        ]),
    );
    let p = poll(&f, Some(state()));
    let by = |id: &str| p.records.iter().find(|r| r.id.ends_with(id)).unwrap();
    assert_eq!(by("event:101").title, "assigned bob");
    assert!(by("event:101").tags.is_empty());
    assert_eq!(tags(by("event:102")), ["routed:assigned"]);
    assert_eq!(by("event:103").title, "review_requested claudeduet[bot]");
    assert_eq!(tags(by("event:103")), ["routed:review", "mine"]);
}

#[test]
fn unlisted_event_types_are_ignored() {
    let f = feed();
    f.on(EVENTS, &list(&[event(150, "subscribed", "alice", "alice", "")]));
    let p = poll(&f, Some(state()));
    assert!(p.records.is_empty());
    assert_eq!(p.state.last_event_id, 150);
}

#[test]
fn own_activity_is_skipped_but_advances_watermarks() {
    let f = feed();
    let bot = "claudeduet[bot]";
    f.on(ISSUES, &list(&[issue(7, bot, "")]));
    f.on(PULLS, &list(&[pull(8, bot, "2026-11-18T09:00:00Z", "")]));
    f.on(
        ICOMMENTS,
        &list(&[icomment(120, bot, "x", 7, "2026-11-18T11:00:00Z")]),
    );
    f.on(RCOMMENTS, &list(&[rcomment(121, bot, "x", 8)]));
    f.on(EVENTS, &list(&[event(122, "closed", bot, bot, "")]));
    let p = poll(&f, Some(state()));
    assert!(p.records.is_empty());
    assert_eq!(
        (
            p.state.last_issue_number,
            p.state.last_pr_number,
            p.state.last_issue_comment_id,
            p.state.last_review_comment_id,
            p.state.last_event_id
        ),
        (7, 8, 120, 121, 122)
    );
    // With skip_self off the same activity is emitted.
    let p = try_poll(&f, &opts(None, false), Some(state())).unwrap();
    assert_eq!(p.records.len(), 5);
}

#[test]
fn body_excerpt_is_600_chars_with_marker() {
    let f = feed();
    let long = "é".repeat(700);
    f.on(
        ISSUES,
        &list(&[issue(7, "alice", "").replace("body 7", &format!("  {long}  "))]),
    );
    let p = poll(&f, Some(state()));
    assert_eq!(p.records[0].body, format!("{} [...]", "é".repeat(600)));
}

#[test]
fn null_body_is_empty_string() {
    let f = feed();
    f.on(
        ISSUES,
        &list(&[issue(7, "alice", "").replace(r#""body 7""#, "null")]),
    );
    let p = poll(&f, Some(state()));
    assert_eq!(p.records[0].body, "");
}

#[test]
fn last_check_at_is_stored_sixty_seconds_early() {
    let f = feed();
    let p = poll(&f, Some(state()));
    assert_eq!(p.state.last_check_at, "2026-11-18T11:59:00Z");
    assert_eq!(p.state.last_issue_number, 5);
}

#[test]
fn any_failed_request_returns_err() {
    let f = feed();
    f.fail(RCOMMENTS, 502);
    assert!(try_poll(&f, &opts(None, true), Some(state())).is_err());
    assert!(try_poll(&f, &opts(None, true), None).is_err());
}

#[test]
fn records_are_sorted_by_ts() {
    let f = feed();
    f.on(ISSUES, &list(&[issue(7, "alice", "")])); // 11:00
    f.on(
        ICOMMENTS,
        &list(&[
            icomment(101, "alice", "late", 7, "2026-11-18T11:40:00Z"),
            icomment(102, "alice", "early", 7, "2026-11-18T10:20:00Z"),
        ]),
    );
    let p = poll(&f, Some(state()));
    let ids: Vec<_> = p.records.iter().map(|r| r.id.as_str()).collect();
    assert_eq!(
        ids,
        [
            "gh:CycleFive/kharon:issue_comment:102",
            "gh:CycleFive/kharon:issue:7",
            "gh:CycleFive/kharon:issue_comment:101",
        ]
    );
}
