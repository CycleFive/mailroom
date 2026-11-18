use mailroom_logic::config::{Keep, Rule, Then, When};
use mailroom_logic::filter::{apply, matches};
use mailroom_logic::record::{Kind, NewRecord, Src};

fn rec(kind: Kind, actor: &str, body: &str, tags: &[&str]) -> NewRecord {
    NewRecord {
        src: Src::Discord,
        kind,
        id: "1".into(),
        ts: "2026-01-01T00:00:00Z".into(),
        actor: actor.into(),
        reference: "chan".into(),
        title: String::new(),
        body: body.into(),
        url: String::new(),
        tags: tags.iter().map(|t| t.to_string()).collect(),
    }
}

fn tag_rule(when: When, tag: &str) -> Rule {
    Rule {
        when,
        then: Then::Tag(tag.into()),
    }
}

#[test]
fn empty_when_matches_everything() {
    assert!(matches(&When::default(), &rec(Kind::Message, "x", "y", &[])));
}

#[test]
fn conditions_are_anded_and_lists_are_ored() {
    let rules = [Rule {
        when: When {
            kind: vec![Kind::Reaction],
            actor_not: vec!["Lothrop".into()],
            ..When::default()
        },
        then: Then::Drop,
    }];
    let input = vec![
        rec(Kind::Reaction, "Nyx", "", &[]),
        rec(Kind::Reaction, "lothrop", "", &[]),
        rec(Kind::Message, "Nyx", "", &[]),
    ];
    let out = apply(input, &rules, Keep::All);
    assert_eq!(out.len(), 2);
    assert_eq!(out[0].actor, "lothrop");
    assert_eq!(out[1].kind, Kind::Message);
}

#[test]
fn body_contains_is_case_insensitive_over_title_and_body() {
    let mut r = rec(Kind::Message, "a", "nothing", &[]);
    r.title = "KHARON is here".into();
    let when = When {
        body_contains: vec!["kharon".into()],
        ..When::default()
    };
    assert!(matches(&when, &r));
    let when = When {
        body_contains: vec!["absent".into(), "NOTHING".into()],
        ..When::default()
    };
    assert!(matches(&when, &r));
    let when = When {
        body_contains: vec!["absent".into()],
        ..When::default()
    };
    assert!(!matches(&when, &r));
}

#[test]
fn tag_rule_adds_once_and_later_rule_sees_it() {
    let rules = [
        tag_rule(When::default(), "a"),
        tag_rule(
            When {
                tag: vec!["a".into()],
                ..When::default()
            },
            "b",
        ),
        tag_rule(When::default(), "a"),
    ];
    let out = apply(vec![rec(Kind::Message, "x", "", &[])], &rules, Keep::All);
    assert_eq!(out[0].tags, vec!["a", "b"]);
}

#[test]
fn drop_stops_later_rules() {
    let rules = [
        Rule {
            when: When::default(),
            then: Then::Drop,
        },
        tag_rule(When::default(), "a"),
    ];
    let out = apply(vec![rec(Kind::Message, "x", "", &[])], &rules, Keep::All);
    assert!(out.is_empty());
}

#[test]
fn keep_tagged_removes_untagged_after_rules() {
    let rules = [tag_rule(
        When {
            actor: vec!["NYX".into()],
            ..When::default()
        },
        "hit",
    )];
    let input = vec![
        rec(Kind::Message, "other", "", &[]),
        rec(Kind::Message, "nyx", "", &[]),
        rec(Kind::Message, "pre", "", &["builtin"]),
    ];
    let out = apply(input, &rules, Keep::Tagged);
    assert_eq!(out.len(), 2);
    assert_eq!(out[0].actor, "nyx");
    assert_eq!(out[1].actor, "pre");
}

#[test]
fn order_is_preserved() {
    let input: Vec<_> = ["a", "b", "c"]
        .iter()
        .map(|a| rec(Kind::Message, a, "", &[]))
        .collect();
    let out = apply(input, &[], Keep::All);
    let actors: Vec<_> = out.iter().map(|r| r.actor.as_str()).collect();
    assert_eq!(actors, ["a", "b", "c"]);
}
