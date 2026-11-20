//! Polls an organisation for work handed to the mailbox by label.

use std::cell::Cell;
use std::collections::{BTreeMap, BTreeSet};

use serde::de::IgnoredAny;
use serde::{Deserialize, Serialize};

use crate::config::GithubSource;
use crate::feed::{get_json, FeedClient, FeedError, Identity, Poll, PollError};
use crate::github::{
    event_record, gap_record, issue_comment_record, review_comment_record, review_record, Comment, Ctx,
    Event, Label, Review, User,
};
use crate::record::{Kind, NewRecord, Src};
use crate::time::{epoch, iso};

const PER_PAGE: &str = "100";
const PER_PAGE_N: usize = 100;
/// Events, comments and reviews are paged (oldest first) at most this many times per item.
const MAX_PAGES: usize = 10;
/// New or changed items read per poll; the rest wait for the next poll.
const MAX_READ: usize = 20;
/// Vanished claims looked up per poll.
const MAX_LOOKUPS: usize = 20;
/// Requests after the search; once this many are made no further item is started.
const MAX_REQUESTS: usize = 60;

/// Items this mailbox is tracking, keyed "<owner>/<repo>#<number>".
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ClaimsState {
    pub claims: BTreeMap<String, Claim>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Claim {
    pub is_pr: bool,
    pub title: String,
    pub url: String,
    /// The item's `updated_at` when it was last read; a different value means "read it again".
    pub updated_at: String,
    /// Activity created after this instant has not been mailed yet.
    pub since: String,
    /// Label name -> the latest `labeled` event handled for it.
    pub label_events: BTreeMap<String, LabelEvent>,
    /// True when a label that counts has a trusted labeler.
    pub trusted: bool,
    /// Id of the latest `reopened` event after the claim's latest label event; 0 when none.
    #[serde(default)]
    pub reopened_event: u64,
}

/// The latest `labeled` event handled for one label, and whether its labeler was trusted.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LabelEvent {
    pub id: u64,
    pub trusted: bool,
}

impl ClaimsState {
    /// Numbers of the trusted claims in `repo` ("owner/name").
    pub fn claimed_numbers(&self, repo: &str) -> Vec<u64> {
        self.claims
            .iter()
            .filter(|(_, c)| c.trusted)
            .filter_map(|(key, _)| {
                let (r, n) = key.rsplit_once('#')?;
                r.eq_ignore_ascii_case(repo).then(|| n.parse().ok()).flatten()
            })
            .collect()
    }
}

#[derive(Deserialize)]
struct Search {
    #[serde(default)]
    total_count: u64,
    #[serde(default)]
    incomplete_results: bool,
    #[serde(default)]
    items: Vec<Found>,
}

/// An item in the search reply.
#[derive(Deserialize)]
struct Found {
    number: u64,
    #[serde(default)]
    title: String,
    #[serde(default)]
    html_url: String,
    #[serde(default)]
    updated_at: String,
    #[serde(default)]
    repository_url: String,
    #[serde(default)]
    labels: Vec<Label>,
    #[serde(default)]
    user: User,
    #[serde(default)]
    pull_request: Option<IgnoredAny>,
}

/// An item read on its own.
#[derive(Deserialize)]
struct Looked {
    #[serde(default)]
    state: String,
    /// A transferred or renamed item is answered under its new repository.
    #[serde(default)]
    repository_url: String,
    #[serde(default)]
    labels: Vec<Label>,
    #[serde(default)]
    pull_request: Option<IgnoredAny>,
}

#[derive(Deserialize)]
struct Permission {
    #[serde(default)]
    permission: String,
    #[serde(default)]
    role_name: String,
}

fn bad_status(e: &FeedError, codes: &[u16]) -> bool {
    matches!(e, FeedError::Status { status, .. } if codes.contains(status))
}

/// Whether `login` may hand work out in `repo`. Bots and missing users are not trusted.
async fn trusted_labeler(feed: &impl FeedClient, repo: &str, login: &str) -> Result<bool, PollError> {
    if login.is_empty() || !login.chars().all(|c| c.is_ascii_alphanumeric() || c == '-') {
        return Ok(false);
    }
    match get_json::<Permission>(
        feed,
        &format!("/repos/{repo}/collaborators/{login}/permission"),
        &[],
    )
    .await
    {
        Ok(p) => Ok(matches!(p.permission.as_str(), "admin" | "write")
            || matches!(p.role_name.as_str(), "admin" | "maintain" | "write" | "triage")),
        Err(e) if bad_status(&e, &[404]) => Ok(false),
        Err(e) => Err(e.into()),
    }
}

/// Every page (at most `MAX_PAGES`) of a list that is paged with `per_page` and `page`,
/// after the `lead` query parameters. The flag says the last page was still full.
async fn paged<T: serde::de::DeserializeOwned>(
    feed: &impl FeedClient,
    path: &str,
    lead: &[(&str, &str)],
) -> Result<(Vec<T>, bool), PollError> {
    let mut all = Vec::new();
    for page in 1..=MAX_PAGES {
        let page = page.to_string();
        let mut query = lead.to_vec();
        query.push(("per_page", PER_PAGE));
        query.push(("page", &page));
        let batch: Vec<T> = get_json(feed, path, &query).await?;
        let full = batch.len() >= PER_PAGE_N;
        all.extend(batch);
        if !full {
            return Ok((all, false));
        }
    }
    Ok((all, true))
}

/// Counts the requests made through it.
struct Counting<'a, F> {
    inner: &'a F,
    used: Cell<usize>,
}

impl<F: FeedClient> FeedClient for Counting<'_, F> {
    async fn get(&self, path: &str, query: &[(&str, &str)]) -> Result<String, FeedError> {
        self.used.set(self.used.get() + 1);
        self.inner.get(path, query).await
    }
}

fn claimed(mut r: NewRecord) -> NewRecord {
    r.tags.push("claimed".to_string());
    r
}

fn by_age<'a>(a: (&str, &'a str), b: (&str, &'a str)) -> std::cmp::Ordering {
    (epoch(a.0), a.1).cmp(&(epoch(b.0), b.1))
}

/// Whether `candidate` is at or after `reference`.
fn at_or_after(candidate: &str, reference: &str) -> bool {
    matches!((epoch(candidate), epoch(reference)), (Some(c), Some(r)) if c >= r)
}

/// A `gap` record for one list of one item.
fn item_gap(repo: &str, n: u64, list: &str, title: String, now_epoch: i64) -> NewRecord {
    NewRecord {
        src: Src::Github,
        kind: Kind::Gap,
        id: format!("gh:gap:{list}:{repo}#{n}:{now_epoch}"),
        ts: iso(now_epoch),
        actor: String::new(),
        reference: format!("{repo}#{n}"),
        title,
        body: format!(
            "The {list} of {repo}#{n} grew past what one poll can read. Look at the item directly."
        ),
        url: format!("https://github.com/{repo}/issues/{n}"),
        tags: Vec::new(),
    }
}

/// The closing note for `claim` (of `repo#number`); its id follows the claim's latest label
/// event (and reopen).
fn closing_note(repo: &str, number: &str, key: &str, claim: &Claim, why: &str, now_epoch: i64) -> NewRecord {
    let latest = claim.label_events.values().map(|l| l.id).max().unwrap_or(0);
    NewRecord {
        src: Src::Github,
        kind: Kind::Event,
        id: match claim.reopened_event {
            0 => format!("gh:{repo}:release:{number}:{latest}"),
            r => format!("gh:{repo}:release:{number}:{latest}:{r}"),
        },
        ts: iso(now_epoch),
        actor: String::new(),
        reference: key.to_string(),
        title: format!("released: {why}"),
        body: claim.title.clone(),
        url: claim.url.clone(),
        tags: vec!["released".to_string()],
    }
}

/// One poll of a source's labels target. `opts.labels` must be set (else a `PollError`).
pub async fn poll_claims(
    feed: &impl FeedClient,
    me: &Identity,
    opts: &GithubSource,
    prev: Option<ClaimsState>,
    now_epoch: i64,
) -> Result<Poll<ClaimsState>, PollError> {
    let routing = opts
        .labels
        .as_ref()
        .ok_or_else(|| PollError("the source has no labels block".to_string()))?;
    let (assigned, review) = (routing.assigned(), routing.review());
    let org = routing.org.as_str();
    let q = format!("org:{org} is:open label:\"{assigned}\",\"{review}\"");
    let found: Search = get_json(
        feed,
        "/search/issues",
        &[
            ("q", &q),
            ("sort", "updated"),
            ("order", "desc"),
            ("per_page", PER_PAGE),
        ],
    )
    .await?;
    // From here on every request counts against the poll's budget.
    let feed = Counting {
        inner: feed,
        used: Cell::new(0),
    };
    let feed = &feed;
    let over_budget = || feed.used.get() >= MAX_REQUESTS;

    let prev = prev.unwrap_or_default();
    let mut claims = prev.claims.clone();
    let mut records: Vec<NewRecord> = Vec::new();
    if found.total_count > found.items.len() as u64 {
        records.push(NewRecord {
            src: Src::Github,
            kind: Kind::Gap,
            id: format!("gh:gap:labels:{org}:{now_epoch}"),
            ts: iso(now_epoch),
            actor: String::new(),
            reference: org.to_string(),
            title: format!("labelled items of {org} may have skipped items"),
            body: format!(
                "More items of {org} carry the labels than one poll reads, so some may be missing. Search for them directly."
            ),
            url: format!("https://github.com/{org}"),
            tags: Vec::new(),
        });
    }

    // The items that may count: open (the search says so), and carrying a label that applies.
    // A label is its configured name and the name the item shows (any case).
    struct Counted<'a> {
        key: String,
        repo: &'a str,
        item: &'a Found,
        labels: Vec<(&'a str, &'a str)>,
    }
    let mut counted: Vec<Counted> = Vec::new();
    for item in &found.items {
        let Some((_, repo)) = item.repository_url.split_once("/repos/") else {
            continue;
        };
        let shown = |name: &str| {
            item.labels
                .iter()
                .find(|l| l.name.eq_ignore_ascii_case(name))
                .map(|l| l.name.as_str())
        };
        let mut labels = Vec::new();
        if let Some(on) = shown(&assigned) {
            labels.push((assigned.as_str(), on));
        }
        if item.pull_request.is_some() {
            if let Some(on) = shown(&review) {
                labels.push((review.as_str(), on));
            }
        }
        if !labels.is_empty() {
            counted.push(Counted {
                key: format!("{repo}#{}", item.number),
                repo,
                item,
                labels,
            });
        }
    }
    let counted_keys: BTreeSet<&str> = counted.iter().map(|c| c.key.as_str()).collect();

    let mut to_read: Vec<&Counted> = counted
        .iter()
        .filter(|c| {
            claims
                .get(&c.key)
                .is_none_or(|k| k.updated_at != c.item.updated_at)
        })
        .collect();
    to_read.sort_by(|a, b| by_age((&a.item.updated_at, &a.key), (&b.item.updated_at, &b.key)));
    to_read.truncate(MAX_READ);

    let mut trust: BTreeMap<(String, String), bool> = BTreeMap::new();
    for c in to_read {
        // What was not reached waits for the next poll, its stored state untouched.
        if over_budget() {
            break;
        }
        let (repo, item, n) = (c.repo, c.item, c.item.number);
        let (evs, cut) = paged::<Event>(feed, &format!("/repos/{repo}/issues/{n}/events"), &[]).await?;
        if cut {
            // The list is oldest first: the newest events are missing, so nothing is decided.
            records.push(item_gap(
                repo,
                n,
                "events",
                format!("events of {repo}#{n} are too many to read"),
                now_epoch,
            ));
            continue;
        }
        let prior = claims.get(&c.key).cloned();
        let (is_new, was_trusted) = (prior.is_none(), prior.as_ref().is_some_and(|p| p.trusted));
        let mut claim = prior.clone().unwrap_or_else(|| Claim {
            is_pr: item.pull_request.is_some(),
            title: String::new(),
            url: String::new(),
            updated_at: String::new(),
            since: String::new(),
            label_events: BTreeMap::new(),
            trusted: false,
            reopened_event: 0,
        });
        claim.title = item.title.clone();
        claim.url = item.html_url.clone();

        // A label counts when its latest `labeled` event is not followed by an `unlabeled` one.
        let named = |e: &Event, kind: &str, name: &str| {
            e.event == kind
                && e.label
                    .as_ref()
                    .is_some_and(|l| l.name.eq_ignore_ascii_case(name))
        };
        let mut live: Vec<(&str, &Event)> = Vec::new();
        for (cfg, _) in &c.labels {
            let Some(on) = evs
                .iter()
                .filter(|e| named(e, "labeled", cfg))
                .max_by_key(|e| e.id)
            else {
                continue;
            };
            if evs.iter().any(|e| named(e, "unlabeled", cfg) && e.id > on.id) {
                continue;
            }
            live.push((cfg, on));
        }
        if live.is_empty() && is_new {
            // Not a claim (yet): nothing is stored, so it is read again when it changes.
            continue;
        }

        let bot = me.login.as_str();
        let ctx = Ctx::new(
            repo,
            me,
            opts,
            if item.user.login == bot {
                vec![n]
            } else {
                Vec::new()
            },
        );

        // Label events not handled yet: one claim record per label.
        let mut trusted_at: Vec<&str> = Vec::new();
        let mut reopens_used: BTreeSet<u64> = BTreeSet::new();
        for (cfg, e) in &live {
            if e.id <= claim.label_events.get(*cfg).map_or(0, |l| l.id) {
                continue;
            }
            let actor = e.actor_login();
            let slot = (repo.to_string(), actor.to_string());
            let ok = match trust.get(&slot) {
                Some(ok) => *ok,
                None => {
                    let ok = trusted_labeler(feed, repo, actor).await?;
                    trust.insert(slot, ok);
                    ok
                }
            };
            claim.label_events.insert(
                (*cfg).to_string(),
                LabelEvent {
                    id: e.id,
                    trusted: ok,
                },
            );
            // An item reopened after the label went on is handed over again, under its own id.
            let reopen = evs
                .iter()
                .filter(|r| r.event == "reopened" && r.id > e.id)
                .max_by_key(|r| r.id);
            let (id, ts) = match reopen {
                Some(r) => {
                    reopens_used.insert(r.id);
                    (format!("claim:{}:{}", e.id, r.id), r.created_at.as_str())
                }
                None => (format!("claim:{}", e.id), e.created_at.as_str()),
            };
            if ok {
                trusted_at.push(ts);
            }
            if ctx.skips(actor) {
                continue;
            }
            let shown = e.label.as_ref().map_or("", |l| l.name.as_str());
            let mut r = ctx.record(Kind::Event, id, ts, actor, n);
            r.title = format!("labeled {shown}");
            r.body = item.title.clone();
            r.url = item.html_url.clone();
            let routed = if *cfg == review {
                "routed:review"
            } else {
                "routed:assigned"
            };
            r.tags
                .push(if ok { routed } else { "untrusted-label" }.to_string());
            records.push(r);
        }
        let latest_label = claim.label_events.values().map(|l| l.id).max().unwrap_or(0);
        claim.reopened_event = evs
            .iter()
            .filter(|r| r.event == "reopened" && r.id > latest_label)
            .map(|r| r.id)
            .max()
            .unwrap_or(0);

        // Trust is per label: a claim is trusted while a label that counts has a trusted labeler.
        claim.trusted = live
            .iter()
            .any(|(cfg, _)| claim.label_events.get(*cfg).is_some_and(|l| l.trusted));
        if was_trusted && !claim.trusted {
            if let Some(p) = &prior {
                records.push(closing_note(
                    repo,
                    &n.to_string(),
                    &c.key,
                    p,
                    "label removed",
                    now_epoch,
                ));
            }
        }

        // What happened on a trusted claim, unless the per-repo poll carries it. The poll in
        // which a claim becomes trusted reads it once either way.
        let turned = claim.trusted && !was_trusted;
        let watched = opts.repos.iter().any(|r| r.eq_ignore_ascii_case(repo));
        if claim.trusted && (turned || !watched) {
            let since = if turned {
                trusted_at
                    .iter()
                    .min_by_key(|t| epoch(t))
                    .map_or_else(|| item.updated_at.clone(), |t| (*t).to_string())
            } else {
                claim.since.clone()
            };
            for e in evs.iter().filter(|e| at_or_after(&e.created_at, &since)) {
                let own_label = named(e, "labeled", &assigned) || named(e, "labeled", &review);
                if own_label || reopens_used.contains(&e.id) || ctx.skips(e.actor_login()) {
                    continue;
                }
                if let Some(r) = event_record(&ctx, e, n, &item.html_url, &item.user.login) {
                    records.push(claimed(r));
                }
            }
            let (comments, cut) = paged::<Comment>(
                feed,
                &format!("/repos/{repo}/issues/{n}/comments"),
                &[("since", &since)],
            )
            .await?;
            if cut {
                records.push(item_gap(
                    repo,
                    n,
                    "comments",
                    format!("comments of {repo}#{n} may have skipped items"),
                    now_epoch,
                ));
            }
            for cm in comments
                .iter()
                .filter(|cm| at_or_after(&cm.created_at, &since) && !ctx.skips(&cm.user.login))
            {
                records.push(claimed(issue_comment_record(&ctx, cm, n)));
            }
            if item.pull_request.is_some() {
                let (rcs, cut) = paged::<Comment>(
                    feed,
                    &format!("/repos/{repo}/pulls/{n}/comments"),
                    &[("since", &since)],
                )
                .await?;
                if cut {
                    records.push(item_gap(
                        repo,
                        n,
                        "review-comments",
                        format!("review comments of {repo}#{n} may have skipped items"),
                        now_epoch,
                    ));
                }
                for cm in rcs
                    .iter()
                    .filter(|cm| at_or_after(&cm.created_at, &since) && !ctx.skips(&cm.user.login))
                {
                    records.push(claimed(review_comment_record(&ctx, cm, n)));
                }
                let (reviews, cut) =
                    paged::<Review>(feed, &format!("/repos/{repo}/pulls/{n}/reviews"), &[]).await?;
                if cut {
                    records.push(gap_record(repo, &format!("reviews:{n}"), now_epoch));
                }
                for v in &reviews {
                    let Some(submitted) = v.submitted_at.as_deref() else {
                        continue;
                    };
                    if at_or_after(submitted, &since) && !ctx.skips(&v.user.login) {
                        records.push(claimed(review_record(&ctx, v, submitted, n, &item.user.login)));
                    }
                }
            }
        }

        claim.updated_at = item.updated_at.clone();
        claim.since = item.updated_at.clone();
        claims.insert(c.key.clone(), claim);
    }

    // Claims the search no longer returns: closed, unlabelled, moved or gone, or the search lagged.
    if !found.incomplete_results {
        let mut vanished: Vec<(&String, &Claim)> = prev
            .claims
            .iter()
            .filter(|(k, _)| !counted_keys.contains(k.as_str()))
            .collect();
        vanished.sort_by(|a, b| by_age((&a.1.updated_at, a.0), (&b.1.updated_at, b.0)));
        vanished.truncate(MAX_LOOKUPS);
        for (key, claim) in vanished {
            if over_budget() {
                break;
            }
            let Some((repo, number)) = key.rsplit_once('#') else {
                continue;
            };
            let why = match get_json::<Looked>(feed, &format!("/repos/{repo}/issues/{number}"), &[]).await {
                Ok(l) => {
                    let has = |name: &str| l.labels.iter().any(|x| x.name.eq_ignore_ascii_case(name));
                    let counts = has(&assigned) || (l.pull_request.is_some() && has(&review));
                    // A transferred or renamed item answers under its new repository.
                    let here = l
                        .repository_url
                        .split_once("/repos/")
                        .is_some_and(|(_, r)| r.eq_ignore_ascii_case(repo));
                    if !here {
                        "gone"
                    } else if l.state != "open" {
                        "closed"
                    } else if !counts {
                        "label removed"
                    } else {
                        continue;
                    }
                }
                Err(e) if bad_status(&e, &[404, 410]) => "gone",
                Err(e) => return Err(e.into()),
            };
            claims.remove(key);
            if claim.trusted {
                records.push(closing_note(repo, number, key, claim, why, now_epoch));
            }
        }
    }

    // Stable: ties keep the order they were written in.
    records.sort_by_key(|r| epoch(&r.ts));
    Ok(Poll {
        records,
        state: ClaimsState { claims },
    })
}
