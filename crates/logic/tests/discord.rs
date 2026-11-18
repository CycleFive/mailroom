mod support;

use mailroom_logic::config::DiscordSource;
use mailroom_logic::discord::{poll_channel, DiscordState};
use mailroom_logic::feed::{Identity, Poll};
use mailroom_logic::record::Kind;
use pollster::block_on;
use std::collections::BTreeMap;
use support::FakeFeed;

const C: &str = "555";
const G: &str = "777";
const NOW: &str = "2026-11-18T12:00:00Z";
const MSGS: &str = "/channels/555/messages?limit=50";

fn me() -> Identity {
    Identity {
        id: "42".into(),
        login: "bot".into(),
        name: "Bot".into(),
    }
}

fn opts(reactions: bool, skip_self: bool) -> DiscordSource {
    let json =
        format!(r#"{{"broker":"B","channels":["{C}"],"reactions":{reactions},"skip_self":{skip_self}}}"#);
    serde_json::from_str(&json).unwrap()
}

fn state(watermark: u64) -> DiscordState {
    DiscordState {
        channel_name: "general".into(),
        guild_id: G.into(),
        watermark,
        reactions: BTreeMap::new(),
    }
}

fn msg(id: &str, author: &str, content: &str) -> String {
    format!(
        r#"{{"id":"{id}","type":0,"timestamp":"2026-11-18T11:00:00+00:00","content":"{content}","author":{{"id":"{author}","username":"user{author}","global_name":null}},"mentions":[],"attachments":[],"embeds":[]}}"#
    )
}

fn page(msgs: &[String]) -> String {
    format!("[{}]", msgs.join(","))
}

fn poll(feed: &FakeFeed, o: &DiscordSource, prev: Option<DiscordState>) -> Poll<DiscordState> {
    block_on(poll_channel(feed, &me(), C, o, prev, NOW)).unwrap()
}

fn feed_with(messages_json: &str) -> FakeFeed {
    let f = FakeFeed::new();
    f.on(MSGS, messages_json);
    f
}

#[test]
fn first_contact_records_position_and_emits_nothing() {
    let f = feed_with(&page(&[msg("30", "1", "b"), msg("20", "1", "a")]));
    f.on("/channels/555", r#"{"name":"general","guild_id":"777"}"#);
    let p = poll(&f, &opts(true, true), None);
    assert!(p.records.is_empty());
    assert_eq!(p.state.watermark, 30);
    assert_eq!(p.state.channel_name, "general");
    assert_eq!(p.state.guild_id, "777");
}

#[test]
fn first_contact_on_empty_channel_sets_watermark_zero() {
    let f = feed_with("[]");
    f.on("/channels/555", r#"{"name":"general"}"#);
    let p = poll(&f, &opts(true, true), None);
    assert_eq!(p.state.watermark, 0);
    assert_eq!(p.state.guild_id, "@me");
}

#[test]
fn delta_emits_only_messages_above_watermark_oldest_first() {
    let f = feed_with(&page(&[
        msg("30", "1", "c"),
        msg("20", "1", "b"),
        msg("10", "1", "a"),
    ]));
    let p = poll(&f, &opts(true, true), Some(state(10)));
    let ids: Vec<_> = p.records.iter().map(|r| r.id.as_str()).collect();
    assert_eq!(ids, ["dc:20", "dc:30"]);
    assert_eq!(p.state.watermark, 30);
    assert_eq!(p.records[0].reference, "#general");
    assert_eq!(p.records[0].actor, "user1");
    assert_eq!(p.records[0].kind, Kind::Message);
}

#[test]
fn snowflakes_compare_numerically() {
    let f = feed_with(&page(&[msg("10000", "1", "x"), msg("999", "1", "y")]));
    let p = poll(&f, &opts(true, true), Some(state(1000)));
    assert_eq!(p.records.len(), 1);
    assert_eq!(p.records[0].id, "dc:10000");
}

#[test]
fn own_messages_are_skipped_but_advance_watermark() {
    let f = feed_with(&page(&[msg("30", "42", "mine"), msg("20", "1", "theirs")]));
    let p = poll(&f, &opts(true, true), Some(state(10)));
    assert_eq!(p.records.len(), 1);
    assert_eq!(p.state.watermark, 30);
}

#[test]
fn skip_self_false_keeps_own_messages() {
    let f = feed_with(&page(&[msg("30", "42", "mine")]));
    let p = poll(&f, &opts(true, false), Some(state(10)));
    assert_eq!(p.records.len(), 1);
}

#[test]
fn mention_and_reply_tags() {
    let m = r#"[{"id":"30","type":19,"timestamp":"t","content":"yo","author":{"id":"1","username":"u","global_name":"Una"},"mentions":[{"id":"42"}],"referenced_message":{"author":{"id":"42"}}}]"#;
    let f = feed_with(m);
    let p = poll(&f, &opts(true, true), Some(state(10)));
    assert_eq!(p.records[0].tags, ["routed:mention", "routed:reply"]);
    assert_eq!(p.records[0].actor, "Una");
}

#[test]
fn empty_content_still_yields_record_and_mention_tag() {
    let m = r#"[{"id":"30","type":0,"timestamp":"t","content":"","author":{"id":"1","username":"u"},"mentions":[{"id":"42"}]}]"#;
    let f = feed_with(m);
    let p = poll(&f, &opts(true, true), Some(state(10)));
    assert_eq!(p.records[0].body, "");
    assert_eq!(p.records[0].tags, ["routed:mention"]);
}

#[test]
fn body_truncates_at_2000_chars_on_char_boundary() {
    let long = "\u{1F600}".repeat(2001);
    let f = feed_with(&page(&[msg("30", "1", &long)]));
    let p = poll(&f, &opts(true, true), Some(state(10)));
    assert_eq!(p.records[0].body.chars().count(), 2000);
}

#[test]
fn attachments_and_embed_titles_are_noted() {
    let m = r#"[{"id":"30","type":0,"timestamp":"t","content":"hi","author":{"id":"1","username":"u"},"attachments":[{},{}],"embeds":[{"title":"Title"},{}]}]"#;
    let f = feed_with(m);
    let p = poll(&f, &opts(true, true), Some(state(10)));
    assert_eq!(p.records[0].body, "hi\n[2 attachment(s)]\n[embed] Title");
}

#[test]
fn system_messages_are_ignored() {
    let m = r#"[{"id":"30","type":7,"timestamp":"t","content":"","author":{"id":"1","username":"u"}}]"#;
    let f = feed_with(m);
    let p = poll(&f, &opts(true, true), Some(state(10)));
    assert!(p.records.is_empty());
    assert_eq!(p.state.watermark, 30);
}

fn run_of(from: u64, count: u64) -> Vec<String> {
    (0..count)
        .map(|i| msg(&(from + count - 1 - i).to_string(), "1", "m"))
        .collect()
}

#[test]
fn full_page_of_new_messages_pages_back() {
    // page 1: ids 1050..=1101 minus -> 50 messages 1051..=1100; page 2: 3 messages
    let f = feed_with(&page(&run_of(1051, 50)));
    f.on(
        "/channels/555/messages?limit=50&before=1051",
        &page(&run_of(1048, 3)),
    );
    let p = poll(&f, &opts(true, true), Some(state(1000)));
    assert_eq!(p.records.len(), 53);
    assert_eq!(p.records[0].id, "dc:1048");
    assert!(f
        .calls()
        .contains(&"/channels/555/messages?limit=50&before=1051".to_string()));
}

#[test]
fn five_full_pages_emits_gap_record() {
    let f = feed_with(&page(&run_of(1201, 50)));
    for (before, from) in [(1201, 1151), (1151, 1101), (1101, 1051), (1051, 1001)] {
        f.on(
            &format!("/channels/555/messages?limit=50&before={before}"),
            &page(&run_of(from, 50)),
        );
    }
    let p = poll(&f, &opts(true, true), Some(state(1000)));
    let gaps: Vec<_> = p.records.iter().filter(|r| r.kind == Kind::Gap).collect();
    assert_eq!(gaps.len(), 1);
    assert_eq!(gaps[0].id, "dc:gap:555:1250");
    assert_eq!(gaps[0].title, "messages may have been skipped");
    assert_eq!(gaps[0].url, "https://discord.com/channels/777/555/1001");
    assert_eq!(p.records.iter().filter(|r| r.kind == Kind::Message).count(), 250);
    assert_eq!(p.state.watermark, 1250);
}

fn with_reaction(emoji: &str, count: u32) -> String {
    format!(
        r#"[{{"id":"30","type":0,"timestamp":"t","content":"x","author":{{"id":"1","username":"u"}},"reactions":[{{"count":{count},"emoji":{emoji}}}]}}]"#
    )
}

const CHECK: &str = r#"{"id":null,"name":"✅"}"#;
const CHECK_PATH: &str = "/channels/555/messages/30/reactions/%E2%9C%85?limit=100";

fn state_with_reactors(key: &str, ids: &[&str]) -> DiscordState {
    let mut s = state(30);
    s.reactions.insert(
        "30".into(),
        BTreeMap::from([(key.to_string(), ids.iter().map(|i| i.to_string()).collect())]),
    );
    s
}

#[test]
fn new_reaction_yields_one_record_per_new_reactor() {
    let f = feed_with(&with_reaction(CHECK, 2));
    f.on(
        CHECK_PATH,
        r#"[{"id":"1","username":"a"},{"id":"2","username":"b","global_name":"Bee"}]"#,
    );
    let p = poll(&f, &opts(true, true), Some(state_with_reactors("✅", &["1"])));
    assert_eq!(p.records.len(), 1);
    let r = &p.records[0];
    assert_eq!(r.kind, Kind::Reaction);
    assert_eq!(r.id, "dc:react:30:✅:2");
    assert_eq!(r.body, "✅");
    assert_eq!(r.actor, "Bee");
    assert_eq!(r.ts, NOW);
    assert_eq!(r.url, "https://discord.com/channels/777/555/30");
    assert_eq!(p.state.reactions["30"]["✅"], ["1", "2"]);
}

#[test]
fn first_contact_snapshots_existing_reactors() {
    let f = feed_with(&with_reaction(CHECK, 1));
    f.on("/channels/555", r#"{"name":"general","guild_id":"777"}"#);
    f.on(CHECK_PATH, r#"[{"id":"1","username":"a"}]"#);
    let p = poll(&f, &opts(true, true), None);
    assert!(p.records.is_empty());
    assert_eq!(p.state.reactions["30"]["✅"], ["1"]);

    let f2 = feed_with(&with_reaction(CHECK, 2));
    f2.on(
        CHECK_PATH,
        r#"[{"id":"1","username":"a"},{"id":"3","username":"c"}]"#,
    );
    let p2 = poll(&f2, &opts(true, true), Some(p.state));
    assert_eq!(p2.records.len(), 1);
    assert_eq!(p2.records[0].id, "dc:react:30:✅:3");
}

#[test]
fn custom_emoji_key_and_body() {
    let f = feed_with(&with_reaction(r#"{"id":"123","name":"party"}"#, 1));
    f.on(
        "/channels/555/messages/30/reactions/party%3A123?limit=100",
        r#"[{"id":"1","username":"a"}]"#,
    );
    let p = poll(&f, &opts(true, true), Some(state(30)));
    assert_eq!(p.records[0].id, "dc:react:30:party:123:1");
    assert_eq!(p.records[0].body, ":party:");
}

#[test]
fn reactions_false_makes_no_reaction_calls() {
    let f = feed_with(&with_reaction(CHECK, 3));
    let p = poll(&f, &opts(false, true), Some(state(10)));
    assert_eq!(p.records.len(), 1);
    assert!(f.calls().iter().all(|c| !c.contains("/reactions/")));
}

#[test]
fn own_reaction_is_skipped() {
    let f = feed_with(&with_reaction(CHECK, 2));
    f.on(
        CHECK_PATH,
        r#"[{"id":"42","username":"bot"},{"id":"2","username":"b"}]"#,
    );
    let p = poll(&f, &opts(true, true), Some(state(30)));
    assert_eq!(p.records.len(), 1);
    assert_eq!(p.records[0].id, "dc:react:30:✅:2");
}

#[test]
fn upstream_error_returns_err() {
    let f = FakeFeed::new();
    f.fail(MSGS, 429);
    assert!(block_on(poll_channel(&f, &me(), C, &opts(true, true), Some(state(1)), NOW)).is_err());
}

#[test]
fn non_json_body_returns_err() {
    let f = feed_with("<html>");
    let r = block_on(poll_channel(&f, &me(), C, &opts(true, true), Some(state(1)), NOW));
    assert!(r.is_err());
}

#[test]
fn jump_url_shape() {
    let f = feed_with(&page(&[msg("30", "1", "x")]));
    let p = poll(&f, &opts(true, true), Some(state(10)));
    assert_eq!(p.records[0].url, "https://discord.com/channels/777/555/30");
}

#[test]
fn burst_reactors_do_not_trigger_a_reactor_fetch() {
    let f = feed_with(&with_reaction_json(
        r#"{"count":3,"count_details":{"normal":2,"burst":1},"emoji":{"id":null,"name":"✅"}}"#,
    ));
    let p = poll(
        &f,
        &opts(true, true),
        Some(state_with_reactors("✅", &["1", "2"])),
    );
    assert!(p.records.is_empty());
    assert!(f.calls().iter().all(|c| !c.contains("/reactions/")));
    assert_eq!(p.state.reactions["30"]["✅"], ["1", "2"]);
}

#[test]
fn reactors_at_the_fetch_cap_are_not_refetched() {
    let f = feed_with(&with_reaction(CHECK, 150));
    let ids: Vec<String> = (1..=100).map(|i| i.to_string()).collect();
    let refs: Vec<&str> = ids.iter().map(String::as_str).collect();
    let p = poll(&f, &opts(true, true), Some(state_with_reactors("✅", &refs)));
    assert!(p.records.is_empty());
    assert!(f.calls().iter().all(|c| !c.contains("/reactions/")));
    assert_eq!(p.state.reactions["30"]["✅"].len(), 100);
}

fn with_reaction_json(reaction: &str) -> String {
    format!(
        r#"[{{"id":"30","type":0,"timestamp":"t","content":"x","author":{{"id":"1","username":"u"}},"reactions":[{reaction}]}}]"#
    )
}
