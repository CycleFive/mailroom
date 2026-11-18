use mailroom_logic::record::*;

#[test]
fn record_serializes_in_muse_shape() {
    let r = Record {
        seq: 12,
        ts_ingest: "2026-11-18T10:05:00Z".into(),
        new: NewRecord {
            src: Src::Discord,
            kind: Kind::Message,
            id: "dc:1".into(),
            ts: "t".into(),
            actor: "Nyx".into(),
            reference: "#general".into(),
            title: "".into(),
            body: "hi".into(),
            url: "u".into(),
            tags: vec!["routed:mention".into()],
        },
    };
    assert_eq!(
        serde_json::to_string(&r).unwrap(),
        r##"{"seq":12,"ts_ingest":"2026-11-18T10:05:00Z","src":"discord","kind":"message","id":"dc:1","ts":"t","actor":"Nyx","ref":"#general","title":"","body":"hi","url":"u","tags":["routed:mention"]}"##
    );
}

#[test]
fn kinds_are_snake_case() {
    assert_eq!(
        serde_json::to_string(&Kind::IssueComment).unwrap(),
        r#""issue_comment""#
    );
    assert_eq!(serde_json::to_string(&Kind::Pr).unwrap(), r#""pr""#);
}

#[test]
fn truncate_counts_chars_not_bytes() {
    let s = "🙂".repeat(2001);
    let t = truncate(&s, 2000, "");
    assert_eq!(t.chars().count(), 2000);
    assert_eq!(truncate("abcdef", 3, " [...]"), "abc [...]");
    assert_eq!(truncate("abc", 3, " [...]"), "abc");
}
