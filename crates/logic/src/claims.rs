//! Polls an organisation for work handed to the mailbox by label.

use std::collections::{BTreeMap, BTreeSet};

use serde::de::IgnoredAny;
use serde::{Deserialize, Serialize};

use crate::config::GithubSource;
use crate::feed::{get_json, FeedClient, FeedError, Identity, Poll, PollError};
use crate::github::{
    event_record, gap_record, issue_comment_record, newer, review_comment_record, review_record, Comment,
    Ctx, Event, Label, Review, User,
};
use crate::record::{Kind, NewRecord, Src};
use crate::time::{epoch, iso};

const PER_PAGE: &str = "100";
const PER_PAGE_N: usize = 100;
/// Events and reviews are paged (oldest first) at most this many times per item.
const MAX_PAGES: usize = 10;
/// New or changed items read per poll; the rest wait for the next poll.
const MAX_READ: usize = 20;
/// Vanished claims looked up per poll.
const MAX_LOOKUPS: usize = 20;

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
    /// Label name -> id of the latest `labeled` event handled for it.
    pub label_events: BTreeMap<String, u64>,
    /// True once any of its label events came from a trusted labeler.
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
                (r == repo).then(|| n.parse().ok()).flatten()
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

/// Every page (at most `MAX_PAGES`) of a list that is paged with `per_page` and `page`.
/// The flag says the last page was still full.
async fn paged<T: serde::de::DeserializeOwned>(
    feed: &impl FeedClient,
    path: &str,
) -> Result<(Vec<T>, bool), PollError> {
    let mut all = Vec::new();
    for page in 1..=MAX_PAGES {
        let batch: Vec<T> =
            get_json(feed, path, &[("per_page", PER_PAGE), ("page", &page.to_string())]).await?;
        let full = batch.len() >= PER_PAGE_N;
        all.extend(batch);
        if !full {
            return Ok((all, false));
        }
    }
    Ok((all, true))
}

fn claimed(mut r: NewRecord) -> NewRecord {
    r.tags.push("claimed".to_string());
    r
}

fn by_age<'a>(a: (&str, &'a str), b: (&str, &'a str)) -> std::cmp::Ordering {
    (epoch(a.0), a.1).cmp(&(epoch(b.0), b.1))
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

    // The items that count: open (the search says so), and labelled in a way that applies.
    struct Counted<'a> {
        key: String,
        repo: &'a str,
        item: &'a Found,
        labels: Vec<&'a str>,
    }
    let mut counted: Vec<Counted> = Vec::new();
    for item in &found.items {
        let Some((_, repo)) = item.repository_url.split_once("/repos/") else {
            continue;
        };
        let has = |name: &str| item.labels.iter().any(|l| l.name == name);
        let mut labels = Vec::new();
        if has(&assigned) {
            labels.push(assigned.as_str());
        }
        if item.pull_request.is_some() && has(&review) {
            labels.push(review.as_str());
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
        let (repo, item, n) = (c.repo, c.item, c.item.number);
        let (evs, _) = paged::<Event>(feed, &format!("/repos/{repo}/issues/{n}/events")).await?;
        let is_new = !claims.contains_key(&c.key);
        let mut claim = claims.get(&c.key).cloned().unwrap_or_else(|| Claim {
            is_pr: item.pull_request.is_some(),
            title: String::new(),
            url: String::new(),
            updated_at: String::new(),
            since: String::new(),
            label_events: BTreeMap::new(),
            trusted: false,
        });
        claim.title = item.title.clone();
        claim.url = item.html_url.clone();
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
        let mut mailed: Vec<&str> = Vec::new();
        let mut handled: Vec<&str> = Vec::new();
        for name in &c.labels {
            let Some(e) = evs
                .iter()
                .filter(|e| e.event == "labeled" && e.label.as_ref().is_some_and(|l| l.name == *name))
                .max_by_key(|e| e.id)
            else {
                continue;
            };
            if e.id <= claim.label_events.get(*name).copied().unwrap_or(0) {
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
            claim.trusted |= ok;
            claim.label_events.insert((*name).to_string(), e.id);
            handled.push(&e.created_at);
            if ctx.skips(actor) {
                continue;
            }
            let mut r = ctx.record(Kind::Event, format!("claim:{}", e.id), &e.created_at, actor, n);
            r.title = format!("labeled {name}");
            r.body = item.title.clone();
            r.url = item.html_url.clone();
            let routed = if *name == review {
                "routed:review"
            } else {
                "routed:assigned"
            };
            r.tags
                .push(if ok { routed } else { "untrusted-label" }.to_string());
            records.push(r);
            mailed.push(&e.created_at);
        }

        // What happened on a trusted claim, unless the per-repo poll carries it.
        let watched = opts.repos.iter().any(|r| r.eq_ignore_ascii_case(repo));
        if claim.trusted && !watched {
            // A new claim reads from its first mailed label event (or, when none was mailed,
            // its latest); a followed one from its marks.
            let first_read = if mailed.is_empty() {
                handled.iter().max_by_key(|t| epoch(t))
            } else {
                mailed.iter().min_by_key(|t| epoch(t))
            };
            let since = if is_new {
                first_read.map_or_else(|| item.updated_at.clone(), |t| (*t).to_string())
            } else {
                claim.since.clone()
            };
            for e in evs.iter().filter(|e| newer(&e.created_at, &since)) {
                let own_label = e.event == "labeled"
                    && e.label
                        .as_ref()
                        .is_some_and(|l| l.name == assigned || l.name == review);
                if own_label || ctx.skips(e.actor_login()) {
                    continue;
                }
                if let Some(r) = event_record(&ctx, e, n, &item.html_url, &item.user.login) {
                    records.push(claimed(r));
                }
            }
            let comments: Vec<Comment> = get_json(
                feed,
                &format!("/repos/{repo}/issues/{n}/comments"),
                &[("since", &since), ("per_page", PER_PAGE)],
            )
            .await?;
            for cm in comments
                .iter()
                .filter(|cm| newer(&cm.created_at, &since) && !ctx.skips(&cm.user.login))
            {
                records.push(claimed(issue_comment_record(&ctx, cm, n)));
            }
            if item.pull_request.is_some() {
                let rcs: Vec<Comment> = get_json(
                    feed,
                    &format!("/repos/{repo}/pulls/{n}/comments"),
                    &[("since", &since), ("per_page", PER_PAGE)],
                )
                .await?;
                for cm in rcs
                    .iter()
                    .filter(|cm| newer(&cm.created_at, &since) && !ctx.skips(&cm.user.login))
                {
                    records.push(claimed(review_comment_record(&ctx, cm, n)));
                }
                let (reviews, cut) =
                    paged::<Review>(feed, &format!("/repos/{repo}/pulls/{n}/reviews")).await?;
                if cut {
                    records.push(gap_record(repo, &format!("reviews:{n}"), now_epoch));
                }
                for v in &reviews {
                    let Some(submitted) = v.submitted_at.as_deref() else {
                        continue;
                    };
                    if newer(submitted, &since) && !ctx.skips(&v.user.login) {
                        records.push(claimed(review_record(&ctx, v, submitted, n, &item.user.login)));
                    }
                }
            }
        }

        claim.updated_at = item.updated_at.clone();
        claim.since = item.updated_at.clone();
        claims.insert(c.key.clone(), claim);
    }

    // Claims the search no longer returns: closed, unlabelled or gone, or the search lagged.
    if !found.incomplete_results {
        let mut vanished: Vec<(&String, &Claim)> = prev
            .claims
            .iter()
            .filter(|(k, _)| !counted_keys.contains(k.as_str()))
            .collect();
        vanished.sort_by(|a, b| by_age((&a.1.updated_at, a.0), (&b.1.updated_at, b.0)));
        vanished.truncate(MAX_LOOKUPS);
        for (key, claim) in vanished {
            let Some((repo, number)) = key.rsplit_once('#') else {
                continue;
            };
            let why = match get_json::<Looked>(feed, &format!("/repos/{repo}/issues/{number}"), &[]).await {
                Ok(l) => {
                    let has = |name: &str| l.labels.iter().any(|x| x.name == name);
                    let counts = has(&assigned) || (l.pull_request.is_some() && has(&review));
                    if l.state != "open" {
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
                let latest = claim.label_events.values().copied().max().unwrap_or(0);
                records.push(NewRecord {
                    src: Src::Github,
                    kind: Kind::Event,
                    id: format!("gh:{repo}:release:{number}:{latest}"),
                    ts: iso(now_epoch),
                    actor: String::new(),
                    reference: key.clone(),
                    title: format!("released: {why}"),
                    body: claim.title.clone(),
                    url: claim.url.clone(),
                    tags: vec!["released".to_string()],
                });
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
