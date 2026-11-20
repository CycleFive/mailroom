//! Polls one GitHub repository: new issues, pull requests, comments, reviews and events.

use serde::{Deserialize, Serialize};

use crate::config::GithubSource;
use crate::feed::{get_json, FeedClient, Identity, Poll, PollError};
use crate::record::{truncate, Kind, NewRecord, Src};
use crate::time::{epoch, iso};

const PER_PAGE: &str = "50";
/// A list response with this many items may have more behind it.
const PER_PAGE_N: usize = 50;
const REVIEWS_PER_PAGE: &str = "100";
const REVIEWS_PER_PAGE_N: usize = 100;
/// Reviews are paged (oldest first) at most this many times per pull request.
const REVIEWS_MAX_PAGES: usize = 10;
const BODY_MAX: usize = 600;
const BODY_SUFFIX: &str = " [...]";
/// `last_check_at` is stored this many seconds early so nothing falls between polls.
const OVERLAP_SECS: i64 = 60;
const EVENT_KINDS: [&str; 6] = [
    "assigned",
    "labeled",
    "review_requested",
    "closed",
    "merged",
    "reopened",
];

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GithubState {
    pub last_issue_number: u64,
    pub last_pr_number: u64,
    pub last_issue_comment_id: u64,
    pub last_review_comment_id: u64,
    pub last_event_id: u64,
    /// ISO time of the last successful poll, stored 60 s early.
    pub last_check_at: String,
}

#[derive(Deserialize, Default)]
pub(crate) struct User {
    #[serde(default)]
    pub(crate) login: String,
}

#[derive(Deserialize)]
pub(crate) struct Label {
    #[serde(default)]
    pub(crate) name: String,
}

/// An entry of `/issues` or `/pulls`; the fields not present on one are defaulted.
#[derive(Deserialize)]
struct Item {
    number: u64,
    #[serde(default)]
    title: String,
    #[serde(default)]
    body: Option<String>,
    #[serde(default)]
    html_url: String,
    #[serde(default)]
    created_at: String,
    #[serde(default)]
    updated_at: String,
    #[serde(default)]
    user: User,
    #[serde(default)]
    assignees: Vec<User>,
    #[serde(default)]
    labels: Vec<Label>,
    #[serde(default)]
    requested_reviewers: Vec<User>,
    #[serde(default)]
    pull_request: Option<Box<serde_json::value::RawValue>>,
}

#[derive(Deserialize)]
pub(crate) struct Comment {
    pub(crate) id: u64,
    #[serde(default)]
    pub(crate) body: Option<String>,
    #[serde(default)]
    pub(crate) html_url: String,
    #[serde(default)]
    pub(crate) issue_url: String,
    #[serde(default)]
    pub(crate) pull_request_url: String,
    #[serde(default)]
    pub(crate) created_at: String,
    #[serde(default)]
    pub(crate) user: User,
}

#[derive(Deserialize)]
pub(crate) struct Review {
    pub(crate) id: u64,
    #[serde(default)]
    pub(crate) state: String,
    #[serde(default)]
    pub(crate) body: Option<String>,
    #[serde(default)]
    pub(crate) html_url: String,
    #[serde(default)]
    pub(crate) submitted_at: Option<String>,
    #[serde(default)]
    pub(crate) user: User,
}

#[derive(Deserialize)]
struct EventIssue {
    number: u64,
    #[serde(default)]
    html_url: String,
    #[serde(default)]
    user: User,
}

#[derive(Deserialize)]
pub(crate) struct Event {
    pub(crate) id: u64,
    #[serde(default)]
    pub(crate) event: String,
    #[serde(default)]
    pub(crate) created_at: String,
    #[serde(default)]
    pub(crate) actor: Option<User>,
    #[serde(default)]
    pub(crate) assignee: Option<User>,
    #[serde(default)]
    pub(crate) label: Option<Label>,
    #[serde(default)]
    pub(crate) requested_reviewer: Option<User>,
    #[serde(default)]
    issue: Option<EventIssue>,
}

/// The number after the last `/` of an API URL.
fn trailing_number(url: &str) -> Option<u64> {
    url.rsplit('/').next()?.parse().ok()
}

pub(crate) fn excerpt(body: &Option<String>) -> String {
    truncate(body.as_deref().unwrap_or("").trim(), BODY_MAX, BODY_SUFFIX)
}

/// Whether `candidate` is strictly later than `reference`; unparsable means not newer.
pub(crate) fn newer(candidate: &str, reference: &str) -> bool {
    matches!((epoch(candidate), epoch(reference)), (Some(c), Some(r)) if c > r)
}

pub(crate) struct Ctx<'a> {
    pub(crate) repo: &'a str,
    pub(crate) bot: &'a str,
    skip_self: bool,
    mention: String,
    review_label: Option<&'a str>,
    pub(crate) mine_numbers: Vec<u64>,
}

impl<'a> Ctx<'a> {
    pub(crate) fn new(
        repo: &'a str,
        me: &'a Identity,
        opts: &'a GithubSource,
        mine_numbers: Vec<u64>,
    ) -> Self {
        let bot = me.login.as_str();
        let slug = bot.strip_suffix("[bot]").unwrap_or(bot);
        Ctx {
            repo,
            bot,
            skip_self: opts.skip_self,
            mention: format!("@{slug}").to_lowercase(),
            review_label: opts.review_label.as_deref(),
            mine_numbers,
        }
    }

    /// Whether nothing by `login` is to be mailed (the bot's own doing, with `skip_self`).
    pub(crate) fn skips(&self, login: &str) -> bool {
        self.skip_self && login == self.bot
    }

    pub(crate) fn mentions_me(&self, body: &Option<String>) -> bool {
        body.as_deref()
            .is_some_and(|b| b.to_lowercase().contains(&self.mention))
    }

    fn has_review_label(&self, name: &str) -> bool {
        self.review_label == Some(name)
    }

    pub(crate) fn record(&self, kind: Kind, id: String, ts: &str, actor: &str, number: u64) -> NewRecord {
        NewRecord {
            src: Src::Github,
            kind,
            id: format!("gh:{}:{id}", self.repo),
            ts: ts.to_string(),
            actor: actor.to_string(),
            reference: format!("{}#{number}", self.repo),
            title: String::new(),
            body: String::new(),
            url: String::new(),
            tags: Vec::new(),
        }
    }
}

/// The record for an issue comment on item `number`.
pub(crate) fn issue_comment_record(ctx: &Ctx, c: &Comment, number: u64) -> NewRecord {
    let mut r = ctx.record(
        Kind::IssueComment,
        format!("issue_comment:{}", c.id),
        &c.created_at,
        &c.user.login,
        number,
    );
    r.body = excerpt(&c.body);
    r.url = c.html_url.clone();
    if ctx.mine_numbers.contains(&number) {
        r.tags.push("mine".to_string());
    }
    if ctx.mentions_me(&c.body) {
        r.tags.push("routed:mention".to_string());
    }
    r
}

/// The record for a review comment on pull request `number`.
pub(crate) fn review_comment_record(ctx: &Ctx, c: &Comment, number: u64) -> NewRecord {
    let mut r = ctx.record(
        Kind::ReviewComment,
        format!("review_comment:{}", c.id),
        &c.created_at,
        &c.user.login,
        number,
    );
    r.body = excerpt(&c.body);
    r.url = c.html_url.clone();
    if ctx.mine_numbers.contains(&number) {
        r.tags.push("mine".to_string());
    }
    if ctx.mentions_me(&c.body) {
        r.tags.push("routed:mention".to_string());
    }
    r
}

/// The record for a review (submitted at `submitted`) of pull request `number` by `author`.
pub(crate) fn review_record(ctx: &Ctx, v: &Review, submitted: &str, number: u64, author: &str) -> NewRecord {
    let mut r = ctx.record(
        Kind::Review,
        format!("review:{}", v.id),
        submitted,
        &v.user.login,
        number,
    );
    r.title = v.state.clone();
    r.body = excerpt(&v.body);
    r.url = v.html_url.clone();
    if author == ctx.bot {
        r.tags.push("mine".to_string());
    }
    if ctx.mentions_me(&v.body) {
        r.tags.push("routed:mention".to_string());
    }
    r
}

impl Event {
    pub(crate) fn actor_login(&self) -> &str {
        self.actor.as_ref().map_or("", |a| a.login.as_str())
    }
}

/// The record for an event on item `number` (at `url`, opened by `author`); `None` for
/// event kinds that are not mailed.
pub(crate) fn event_record(ctx: &Ctx, e: &Event, number: u64, url: &str, author: &str) -> Option<NewRecord> {
    if !EVENT_KINDS.contains(&e.event.as_str()) {
        return None;
    }
    let actor = e.actor_login();
    let named = |u: &Option<User>| u.as_ref().map_or(String::new(), |u| u.login.clone());
    let title = match e.event.as_str() {
        "assigned" => format!("assigned {}", named(&e.assignee)),
        "labeled" => format!("labeled {}", e.label.as_ref().map_or("", |l| l.name.as_str())),
        "review_requested" => format!("review_requested {}", named(&e.requested_reviewer)),
        other => other.to_string(),
    };
    let mut r = ctx.record(
        Kind::Event,
        format!("event:{}", e.id),
        &e.created_at,
        actor,
        number,
    );
    r.title = title;
    r.url = url.to_string();
    let routed_assigned = e.event == "assigned" && e.assignee.as_ref().is_some_and(|a| a.login == ctx.bot);
    let routed_review = match e.event.as_str() {
        "labeled" => e.label.as_ref().is_some_and(|l| ctx.has_review_label(&l.name)),
        "review_requested" => e.requested_reviewer.as_ref().is_some_and(|u| u.login == ctx.bot),
        _ => false,
    };
    if routed_assigned {
        r.tags.push("routed:assigned".to_string());
    }
    if routed_review {
        r.tags.push("routed:review".to_string());
    }
    if author == ctx.bot {
        r.tags.push("mine".to_string());
    }
    Some(r)
}

/// A `gap` record: `list` of `repo` may have skipped items.
pub(crate) fn gap_record(repo: &str, list: &str, now_epoch: i64) -> NewRecord {
    NewRecord {
        src: Src::Github,
        kind: Kind::Gap,
        id: format!("gh:gap:{repo}:{list}:{now_epoch}"),
        ts: iso(now_epoch),
        actor: String::new(),
        reference: repo.to_string(),
        title: format!("{list} of {repo} may have skipped items"),
        body: format!(
            "The {list} list of {repo} grew past what one poll can read, so items may be missing. Look at the repo directly."
        ),
        url: format!("https://github.com/{repo}"),
        tags: Vec::new(),
    }
}

/// Whether a list is full and every item is newer than its watermark.
fn overflowed(len: usize, ids: impl Iterator<Item = u64>, watermark: u64) -> bool {
    len == PER_PAGE_N && ids.into_iter().all(|id| id > watermark)
}

/// Tags activity (comments, reviews, events) on claimed items `claimed`, after its other tags.
fn tag_claimed(records: &mut [NewRecord], claimed: &[u64]) {
    if claimed.is_empty() {
        return;
    }
    for r in records.iter_mut() {
        if !matches!(
            r.kind,
            Kind::IssueComment | Kind::ReviewComment | Kind::Review | Kind::Event
        ) {
            continue;
        }
        let number = r
            .reference
            .rsplit_once('#')
            .and_then(|(_, n)| n.parse::<u64>().ok());
        if number.is_some_and(|n| claimed.contains(&n)) && !r.tags.iter().any(|t| t == "claimed") {
            r.tags.push("claimed".to_string());
        }
    }
}

pub async fn poll_repo(
    feed: &impl FeedClient,
    me: &Identity,
    repo: &str,
    opts: &GithubSource,
    prev: Option<GithubState>,
    now_epoch: i64,
    claimed: &[u64],
) -> Result<Poll<GithubState>, PollError> {
    let base = format!("/repos/{repo}");
    let issues: Vec<Item> = get_json(
        feed,
        &format!("{base}/issues"),
        &[
            ("state", "all"),
            ("sort", "updated"),
            ("direction", "desc"),
            ("per_page", PER_PAGE),
        ],
    )
    .await?;
    let pulls: Vec<Item> = get_json(
        feed,
        &format!("{base}/pulls"),
        &[
            ("state", "all"),
            ("sort", "updated"),
            ("direction", "desc"),
            ("per_page", PER_PAGE),
        ],
    )
    .await?;
    let issue_comments: Vec<Comment> = get_json(
        feed,
        &format!("{base}/issues/comments"),
        &[("sort", "created"), ("direction", "desc"), ("per_page", PER_PAGE)],
    )
    .await?;
    let review_comments: Vec<Comment> = get_json(
        feed,
        &format!("{base}/pulls/comments"),
        &[("sort", "created"), ("direction", "desc"), ("per_page", PER_PAGE)],
    )
    .await?;
    let events: Vec<Event> =
        get_json(feed, &format!("{base}/issues/events"), &[("per_page", PER_PAGE)]).await?;

    // Overflow is judged on the raw list, which includes pull requests.
    let issues_len = issues.len();
    let issue_numbers: Vec<u64> = issues.iter().map(|i| i.number).collect();
    // Pull requests also appear in the issue list; they are handled from `pulls`.
    let issues: Vec<&Item> = issues.iter().filter(|i| i.pull_request.is_none()).collect();

    let bot = me.login.as_str();
    let mine: Vec<u64> = issues
        .iter()
        .copied()
        .chain(pulls.iter())
        .filter(|i| i.user.login == bot)
        .map(|i| i.number)
        .collect();
    let ctx = Ctx::new(repo, me, opts, mine);

    let max_of = |it: &mut dyn Iterator<Item = u64>| it.max().unwrap_or(0);
    let seen_issue = max_of(&mut issues.iter().map(|i| i.number));
    let seen_pr = max_of(&mut pulls.iter().map(|p| p.number));
    let seen_icomment = max_of(&mut issue_comments.iter().map(|c| c.id));
    let seen_rcomment = max_of(&mut review_comments.iter().map(|c| c.id));
    let seen_event = max_of(&mut events.iter().map(|e| e.id));
    let last_check_at = iso(now_epoch - OVERLAP_SECS);

    let Some(prev) = prev else {
        return Ok(Poll {
            records: Vec::new(),
            state: GithubState {
                last_issue_number: seen_issue,
                last_pr_number: seen_pr,
                last_issue_comment_id: seen_icomment,
                last_review_comment_id: seen_rcomment,
                last_event_id: seen_event,
                last_check_at,
            },
        });
    };

    let mut records = Vec::new();
    for (list, full_and_new) in [
        // The raw list mixes issues and pull requests, which share one number space but
        // have separate watermarks, so it is judged against the larger of the two.
        (
            "issues",
            overflowed(
                issues_len,
                issue_numbers.iter().copied(),
                prev.last_issue_number.max(prev.last_pr_number),
            ),
        ),
        (
            "pulls",
            overflowed(pulls.len(), pulls.iter().map(|p| p.number), prev.last_pr_number),
        ),
        (
            "issue_comments",
            overflowed(
                issue_comments.len(),
                issue_comments.iter().map(|c| c.id),
                prev.last_issue_comment_id,
            ),
        ),
        (
            "review_comments",
            overflowed(
                review_comments.len(),
                review_comments.iter().map(|c| c.id),
                prev.last_review_comment_id,
            ),
        ),
        (
            "events",
            overflowed(events.len(), events.iter().map(|e| e.id), prev.last_event_id),
        ),
    ] {
        if full_and_new {
            records.push(gap_record(repo, list, now_epoch));
        }
    }
    let skip = |login: &str| ctx.skips(login);

    for i in issues.iter().filter(|i| i.number > prev.last_issue_number) {
        if skip(&i.user.login) {
            continue;
        }
        let mut r = ctx.record(
            Kind::Issue,
            format!("issue:{}", i.number),
            &i.created_at,
            &i.user.login,
            i.number,
        );
        r.title = i.title.clone();
        r.body = excerpt(&i.body);
        r.url = i.html_url.clone();
        if i.assignees.iter().any(|a| a.login == bot) {
            r.tags.push("routed:assigned".to_string());
        }
        if ctx.mentions_me(&i.body) {
            r.tags.push("routed:mention".to_string());
        }
        records.push(r);
    }

    for p in pulls.iter().filter(|p| p.number > prev.last_pr_number) {
        if skip(&p.user.login) {
            continue;
        }
        let mut r = ctx.record(
            Kind::Pr,
            format!("pr:{}", p.number),
            &p.created_at,
            &p.user.login,
            p.number,
        );
        r.title = p.title.clone();
        r.body = excerpt(&p.body);
        r.url = p.html_url.clone();
        if p.labels.iter().any(|l| ctx.has_review_label(&l.name))
            || p.requested_reviewers.iter().any(|u| u.login == bot)
        {
            r.tags.push("routed:review".to_string());
        }
        if p.assignees.iter().any(|a| a.login == bot) {
            r.tags.push("routed:assigned".to_string());
        }
        if ctx.mentions_me(&p.body) {
            r.tags.push("routed:mention".to_string());
        }
        records.push(r);
    }

    for c in issue_comments
        .iter()
        .filter(|c| c.id > prev.last_issue_comment_id)
    {
        if skip(&c.user.login) {
            continue;
        }
        let number = trailing_number(&c.issue_url).unwrap_or(0);
        records.push(issue_comment_record(&ctx, c, number));
    }

    for c in review_comments
        .iter()
        .filter(|c| c.id > prev.last_review_comment_id)
    {
        if skip(&c.user.login) {
            continue;
        }
        let number = trailing_number(&c.pull_request_url).unwrap_or(0);
        records.push(review_comment_record(&ctx, c, number));
    }

    for p in pulls.iter().filter(|p| newer(&p.updated_at, &prev.last_check_at)) {
        let mut reviews: Vec<Review> = Vec::new();
        for page in 1..=REVIEWS_MAX_PAGES {
            let batch: Vec<Review> = get_json(
                feed,
                &format!("{base}/pulls/{}/reviews", p.number),
                &[("per_page", REVIEWS_PER_PAGE), ("page", &page.to_string())],
            )
            .await?;
            let short = batch.len() < REVIEWS_PER_PAGE_N;
            reviews.extend(batch);
            if short {
                break;
            }
            if page == REVIEWS_MAX_PAGES {
                records.push(gap_record(repo, &format!("reviews:{}", p.number), now_epoch));
            }
        }
        for v in &reviews {
            let Some(submitted) = v.submitted_at.as_deref() else {
                continue;
            };
            if !newer(submitted, &prev.last_check_at) || skip(&v.user.login) {
                continue;
            }
            records.push(review_record(&ctx, v, submitted, p.number, &p.user.login));
        }
    }

    for e in events.iter().filter(|e| e.id > prev.last_event_id) {
        let Some(issue) = &e.issue else { continue };
        if skip(e.actor_login()) {
            continue;
        }
        if let Some(r) = event_record(&ctx, e, issue.number, &issue.html_url, &issue.user.login) {
            records.push(r);
        }
    }

    // Stable: ties keep the table's order.
    records.sort_by_key(|r| epoch(&r.ts));
    tag_claimed(&mut records, claimed);

    Ok(Poll {
        records,
        state: GithubState {
            last_issue_number: prev.last_issue_number.max(seen_issue),
            last_pr_number: prev.last_pr_number.max(seen_pr),
            last_issue_comment_id: prev.last_issue_comment_id.max(seen_icomment),
            last_review_comment_id: prev.last_review_comment_id.max(seen_rcomment),
            last_event_id: prev.last_event_id.max(seen_event),
            last_check_at,
        },
    })
}
