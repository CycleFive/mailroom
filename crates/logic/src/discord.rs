//! Polls one Discord channel: new messages and new reactions since the previous state.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::config::DiscordSource;
use crate::feed::{get_json, FeedClient, Identity, Poll, PollError};
use crate::record::{truncate, Kind, NewRecord, Src};

const PAGE: usize = 50;
const MAX_PAGES: usize = 5;
const PAGE_LIMIT: &str = "50";
const BODY_MAX: usize = 2000;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DiscordState {
    pub channel_name: String,
    pub guild_id: String,
    pub watermark: u64,
    /// message id -> emoji key -> reactor user ids
    pub reactions: BTreeMap<String, BTreeMap<String, Vec<String>>>,
}

#[derive(Deserialize)]
struct Channel {
    #[serde(default)]
    name: String,
    #[serde(default)]
    guild_id: Option<String>,
}

#[derive(Deserialize)]
struct Message {
    id: String,
    #[serde(default, rename = "type")]
    kind: u32,
    #[serde(default)]
    timestamp: String,
    #[serde(default)]
    content: String,
    #[serde(default)]
    author: User,
    #[serde(default)]
    mentions: Vec<UserId>,
    #[serde(default)]
    referenced_message: Option<Referenced>,
    #[serde(default)]
    attachments: Vec<Attachment>,
    #[serde(default)]
    embeds: Vec<Embed>,
    #[serde(default)]
    reactions: Vec<ReactionSummary>,
}

#[derive(Deserialize, Default)]
struct User {
    #[serde(default)]
    id: String,
    #[serde(default)]
    username: String,
    #[serde(default)]
    global_name: Option<String>,
}

impl User {
    fn display(&self) -> String {
        match self.global_name.as_deref() {
            Some(g) if !g.is_empty() => g.to_string(),
            _ => self.username.clone(),
        }
    }
}

#[derive(Deserialize, Default)]
struct UserId {
    #[serde(default)]
    id: String,
}

#[derive(Deserialize)]
struct Referenced {
    #[serde(default)]
    author: UserId,
}

#[derive(Deserialize)]
struct Attachment {}

#[derive(Deserialize)]
struct Embed {
    #[serde(default)]
    title: Option<String>,
}

#[derive(Deserialize)]
struct ReactionSummary {
    #[serde(default)]
    count: usize,
    emoji: Emoji,
}

#[derive(Deserialize)]
struct Emoji {
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    name: Option<String>,
}

impl Emoji {
    /// The unicode character, or `name:id` for a custom emoji.
    fn key(&self) -> String {
        let name = self.name.as_deref().unwrap_or("");
        match &self.id {
            Some(id) => format!("{name}:{id}"),
            None => name.to_string(),
        }
    }

    /// The unicode character, or `:name:` for a custom emoji.
    fn body(&self) -> String {
        let name = self.name.as_deref().unwrap_or("");
        match &self.id {
            Some(_) => format!(":{name}:"),
            None => name.to_string(),
        }
    }
}

/// An emoji on a page-1 message, as last seen.
struct Seen {
    key: String,
    body: String,
    count: usize,
}

fn parse_id(id: &str) -> Result<u64, PollError> {
    id.parse()
        .map_err(|_| PollError(format!("unparsable Discord id {id:?}")))
}

/// Percent-encodes every byte outside the unreserved set.
fn encode(key: &str) -> String {
    let mut out = String::new();
    for b in key.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~') {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

async fn fetch_reactors(
    feed: &impl FeedClient,
    channel_id: &str,
    message_id: &str,
    key: &str,
) -> Result<Vec<User>, PollError> {
    let path = format!(
        "/channels/{channel_id}/messages/{message_id}/reactions/{}",
        encode(key)
    );
    Ok(get_json(feed, &path, &[("limit", "100")]).await?)
}

fn jump_url(guild_id: &str, channel_id: &str, message_id: &str) -> String {
    format!("https://discord.com/channels/{guild_id}/{channel_id}/{message_id}")
}

pub async fn poll_channel(
    feed: &impl FeedClient,
    me: &Identity,
    channel_id: &str,
    opts: &DiscordSource,
    prev: Option<DiscordState>,
    now: &str,
) -> Result<Poll<DiscordState>, PollError> {
    let messages_path = format!("/channels/{channel_id}/messages");

    let Some(prev) = prev else {
        let channel: Channel = get_json(feed, &format!("/channels/{channel_id}"), &[]).await?;
        let page: Vec<Message> = get_json(feed, &messages_path, &[("limit", PAGE_LIMIT)]).await?;
        let mut watermark = 0;
        for m in &page {
            watermark = watermark.max(parse_id(&m.id)?);
        }
        let mut reactions = BTreeMap::new();
        if opts.reactions {
            for m in &page {
                for r in &m.reactions {
                    let key = r.emoji.key();
                    let users = fetch_reactors(feed, channel_id, &m.id, &key).await?;
                    let ids = users.into_iter().map(|u| u.id).collect();
                    reactions
                        .entry(m.id.clone())
                        .or_insert_with(BTreeMap::new)
                        .insert(key, ids);
                }
            }
        }
        return Ok(Poll {
            records: Vec::new(),
            state: DiscordState {
                channel_name: channel.name,
                guild_id: channel.guild_id.unwrap_or_else(|| "@me".to_string()),
                watermark,
                reactions,
            },
        });
    };

    // Page 1, then older pages while every message so far has been new.
    let first: Vec<Message> = get_json(feed, &messages_path, &[("limit", PAGE_LIMIT)]).await?;
    let mut ids = Vec::with_capacity(first.len());
    for m in &first {
        ids.push(parse_id(&m.id)?);
    }
    let mut new: Vec<(u64, Message)> = Vec::new();
    let mut seen_max = ids.iter().copied().max().unwrap_or(0);
    let mut full_and_new = first.len() == PAGE && ids.iter().all(|&i| i > prev.watermark);
    let mut lowest = ids.iter().copied().min();
    let mut pages = 1;
    // Page 1's messages (new and old) are the ones whose reactions are tracked.
    let page_one_info: Vec<(String, Vec<Seen>)> = first
        .iter()
        .map(|m| {
            (
                m.id.clone(),
                m.reactions
                    .iter()
                    .map(|r| Seen {
                        key: r.emoji.key(),
                        body: r.emoji.body(),
                        count: r.count,
                    })
                    .collect(),
            )
        })
        .collect();
    for (id, m) in ids.iter().copied().zip(first) {
        if id > prev.watermark {
            new.push((id, m));
        }
    }

    let mut gap = false;
    while full_and_new {
        if pages == MAX_PAGES {
            gap = true;
            break;
        }
        let before = lowest.unwrap_or(0).to_string();
        let page: Vec<Message> = get_json(
            feed,
            &messages_path,
            &[("limit", PAGE_LIMIT), ("before", &before)],
        )
        .await?;
        pages += 1;
        let mut page_ids = Vec::with_capacity(page.len());
        for m in &page {
            page_ids.push(parse_id(&m.id)?);
        }
        seen_max = seen_max.max(page_ids.iter().copied().max().unwrap_or(0));
        full_and_new = page.len() == PAGE && page_ids.iter().all(|&i| i > prev.watermark);
        if let Some(l) = page_ids.iter().copied().min() {
            lowest = Some(lowest.map_or(l, |c| c.min(l)));
        }
        for (id, m) in page_ids.into_iter().zip(page) {
            if id > prev.watermark {
                new.push((id, m));
            }
        }
    }
    new.sort_by_key(|(id, _)| *id);

    let guild_id = &prev.guild_id;
    let reference = format!("#{}", prev.channel_name);
    let mut records = Vec::new();

    if gap {
        let newest = new.last().map(|(id, _)| *id).unwrap_or(0);
        let oldest = lowest.unwrap_or(0);
        records.push(NewRecord {
            src: Src::Discord,
            kind: Kind::Gap,
            id: format!("dc:gap:{channel_id}:{newest}"),
            ts: now.to_string(),
            actor: String::new(),
            reference: reference.clone(),
            title: "messages may have been skipped".to_string(),
            body: String::new(),
            url: jump_url(guild_id, channel_id, &oldest.to_string()),
            tags: Vec::new(),
        });
    }

    for (id, m) in &new {
        if m.kind != 0 && m.kind != 19 {
            continue;
        }
        if opts.skip_self && m.author.id == me.id {
            continue;
        }
        let mut body = truncate(&m.content, BODY_MAX, "");
        if !m.attachments.is_empty() {
            body.push_str(&format!("\n[{} attachment(s)]", m.attachments.len()));
        }
        for title in m.embeds.iter().filter_map(|e| e.title.as_deref()) {
            body.push_str(&format!("\n[embed] {title}"));
        }
        let mut tags = Vec::new();
        if m.mentions.iter().any(|u| u.id == me.id) {
            tags.push("routed:mention".to_string());
        }
        if m.referenced_message
            .as_ref()
            .is_some_and(|r| r.author.id == me.id)
        {
            tags.push("routed:reply".to_string());
        }
        records.push(NewRecord {
            src: Src::Discord,
            kind: Kind::Message,
            id: format!("dc:{id}"),
            ts: m.timestamp.clone(),
            actor: m.author.display(),
            reference: reference.clone(),
            title: String::new(),
            body,
            url: jump_url(guild_id, channel_id, &m.id),
            tags,
        });
    }

    let mut reactions: BTreeMap<String, BTreeMap<String, Vec<String>>> = BTreeMap::new();
    if opts.reactions {
        for (message_id, emojis) in &page_one_info {
            for Seen { key, body, count } in emojis {
                let stored = prev
                    .reactions
                    .get(message_id)
                    .and_then(|m| m.get(key))
                    .cloned()
                    .unwrap_or_default();
                let list = if *count > stored.len() {
                    let users = fetch_reactors(feed, channel_id, message_id, key).await?;
                    for u in &users {
                        if stored.contains(&u.id) || (opts.skip_self && u.id == me.id) {
                            continue;
                        }
                        records.push(NewRecord {
                            src: Src::Discord,
                            kind: Kind::Reaction,
                            id: format!("dc:react:{message_id}:{key}:{}", u.id),
                            ts: now.to_string(),
                            actor: u.display(),
                            reference: reference.clone(),
                            title: String::new(),
                            body: body.clone(),
                            url: jump_url(guild_id, channel_id, message_id),
                            tags: Vec::new(),
                        });
                    }
                    users.into_iter().map(|u| u.id).collect()
                } else {
                    stored
                };
                reactions
                    .entry(message_id.clone())
                    .or_default()
                    .insert(key.clone(), list);
            }
        }
    }

    Ok(Poll {
        records,
        state: DiscordState {
            channel_name: prev.channel_name.clone(),
            guild_id: prev.guild_id.clone(),
            watermark: prev.watermark.max(seen_max),
            reactions,
        },
    })
}
