use mailroom_logic::config::*;

const GOOD: &str = r#"{
  "retain_days": 30,
  "agents": {
    "claude": {
      "sources": [
        { "type": "discord", "broker": "DISCORD_CLAUDE", "every_secs": 120,
          "channels": ["1566796119839875999"],
          "reactions": true, "skip_self": true, "keep": "all",
          "rules": [
            { "when": { "kind": ["reaction"], "actor_not": ["Lothrop"] }, "then": "drop" },
            { "when": { "body_contains": ["kharon"] }, "then": { "tag": "kharon" } }
          ] },
        { "type": "github", "broker": "GITHUB", "every_secs": 300,
          "repos": ["CycleFive/kharon"], "review_label": "review:claude",
          "skip_self": true, "keep": "tagged", "rules": [] }
      ]
    },
    "manus": { "sources": [
      { "type": "discord", "broker": "DISCORD_MANUS", "channels": ["1566796119839875999"] }
    ] }
  }
}"#;

fn with_source(src: &str) -> String {
    format!(r#"{{"agents":{{"claude":{{"sources":[{src}]}}}}}}"#)
}

fn err(json: &str) -> String {
    Config::parse(json).unwrap_err().to_string()
}

#[test]
fn parses_spec_example_with_defaults() {
    let c = Config::parse(GOOD).unwrap();
    assert_eq!(c.retain_days, 30);
    let SourceConfig::Discord(d) = &c.agents["claude"].sources[0] else {
        panic!()
    };
    assert_eq!(
        (d.every_secs, d.reactions, d.skip_self, d.keep),
        (120, true, true, Keep::All)
    );
    assert_eq!(d.rules[0].then, Then::Drop);
    assert_eq!(d.rules[1].then, Then::Tag("kharon".into()));
    assert_eq!(
        c.agents["claude"].sources[0].target_keys(),
        vec!["discord:DISCORD_CLAUDE:1566796119839875999"]
    );
    assert_eq!(
        c.agents["claude"].sources[1].target_keys(),
        vec!["github:GITHUB:CycleFive/kharon"]
    );
}

#[test]
fn minimal_discord_source_gets_defaults() {
    let c = Config::parse(&with_source(
        r#"{"type":"discord","broker":"B","channels":["12345"]}"#,
    ))
    .unwrap();
    assert_eq!(c.retain_days, 30);
    let s = &c.agents["claude"].sources[0];
    let SourceConfig::Discord(d) = s else { panic!() };
    assert_eq!(
        (d.every_secs, d.reactions, d.skip_self, d.keep),
        (120, true, true, Keep::All)
    );
    assert!(d.rules.is_empty());
    assert_eq!(
        (s.broker(), s.every_secs(), s.src(), s.keep()),
        ("B", 120, mailroom_logic::record::Src::Discord, Keep::All)
    );
    assert!(s.rules().is_empty());
}

#[test]
fn minimal_github_source_gets_defaults() {
    let c = Config::parse(&with_source(r#"{"type":"github","broker":"G","repos":["a/b"]}"#)).unwrap();
    assert_eq!(c.agents["claude"].sources[0].every_secs(), 300);
}

#[test]
fn rejects_unknown_field() {
    let e = err(&with_source(
        r#"{"type":"discord","broker":"B","chanels":["12345"]}"#,
    ));
    assert!(e.contains("chanels"), "{e}");
}

#[test]
fn rejects_bad_agent_name() {
    let e = err(r#"{"agents":{"Claude":{"sources":[]}}}"#);
    assert!(e.contains("Claude"), "{e}");
}

#[test]
fn rejects_short_interval() {
    let e = err(&with_source(
        r#"{"type":"discord","broker":"B","every_secs":30,"channels":["12345"]}"#,
    ));
    assert!(e.contains("every_secs") && e.contains("claude"), "{e}");
}

#[test]
fn rejects_non_numeric_channel() {
    let e = err(&with_source(
        r#"{"type":"discord","broker":"B","channels":["general"]}"#,
    ));
    assert!(e.contains("general"), "{e}");
}

#[test]
fn rejects_bad_repo() {
    for repo in ["kharon", "a/b/c", "/b", "a/"] {
        let e = err(&with_source(&format!(
            r#"{{"type":"github","broker":"G","repos":["{repo}"]}}"#
        )));
        assert!(e.contains(repo), "{e}");
    }
}

#[test]
fn rejects_duplicate_target() {
    let e = err(&with_source(
        r#"{"type":"discord","broker":"B","channels":["12345","12345"]}"#,
    ));
    assert!(e.contains("12345"), "{e}");
    assert!(Config::parse(&with_source(
        r#"{"type":"github","broker":"G","repos":["a/b","a/b"]}"#
    ))
    .is_err());
}

#[test]
fn rejects_empty_agents() {
    assert!(Config::parse(r#"{"agents":{}}"#).is_err());
}

#[test]
fn rejects_bad_broker_and_empty_tag_and_label() {
    assert!(Config::parse(&with_source(
        r#"{"type":"discord","broker":"bad-name","channels":["12345"]}"#
    ))
    .is_err());
    assert!(Config::parse(&with_source(
        r#"{"type":"discord","broker":"","channels":["12345"]}"#
    ))
    .is_err());
    assert!(Config::parse(&with_source(
        r#"{"type":"discord","broker":"B","channels":["12345"],"rules":[{"when":{},"then":{"tag":""}}]}"#
    ))
    .is_err());
    assert!(Config::parse(&with_source(
        r#"{"type":"github","broker":"G","repos":["a/b"],"review_label":""}"#
    ))
    .is_err());
}

#[test]
fn then_serializes_as_spec() {
    assert_eq!(serde_json::to_string(&Then::Drop).unwrap(), r#""drop""#);
    assert_eq!(
        serde_json::to_string(&Then::Tag("x".into())).unwrap(),
        r#"{"tag":"x"}"#
    );
}
