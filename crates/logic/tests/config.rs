use mailroom_logic::config::*;

const GOOD: &str = r#"{
  "retain_days": 30,
  "agents": {
    "claude": {
      "signin": "DISCORD_CLAUDE",
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
    "manus": { "signin": "DISCORD_MANUS", "sources": [
      { "type": "discord", "broker": "DISCORD_MANUS", "channels": ["1566796119839875999"] }
    ] }
  }
}"#;

fn with_source(src: &str) -> String {
    format!(r#"{{"agents":{{"claude":{{"signin":"DISCORD_CLAUDE","sources":[{src}]}}}}}}"#)
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
    let e = err(r#"{"agents":{"Claude":{"signin":"DISCORD_CLAUDE","sources":[]}}}"#);
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

#[test]
fn parses_signin() {
    let c = Config::parse(GOOD).unwrap();
    assert_eq!(c.agents["claude"].signin, "DISCORD_CLAUDE");
    assert_eq!(c.agents["manus"].signin, "DISCORD_MANUS");
}

#[test]
fn rejects_missing_signin() {
    let e =
        err(r#"{"agents":{"claude":{"sources":[{"type":"discord","broker":"B","channels":["12345"]}]}}}"#);
    assert!(e.contains("signin"), "{e}");
}

#[test]
fn rejects_bad_signin() {
    for signin in ["", "discord-claude"] {
        let e = err(&format!(
            r#"{{"agents":{{"claude":{{"signin":"{signin}","sources":[{{"type":"discord","broker":"B","channels":["12345"]}}]}}}}}}"#
        ));
        assert!(e.contains("claude") && e.contains("signin"), "{e}");
    }
}

fn github(extra: &str) -> String {
    with_source(&format!(r#"{{"type":"github","broker":"G",{extra}}}"#))
}

fn github_source(c: &Config) -> &GithubSource {
    let SourceConfig::Github(g) = &c.agents["claude"].sources[0] else {
        panic!()
    };
    g
}

#[test]
fn labels_block_parses_and_names_its_labels() {
    let c = Config::parse(&github(
        r#""repos":["o/a"],"labels":{"org":"CycleFive","tag":"claude"}"#,
    ))
    .unwrap();
    let l = github_source(&c).labels.as_ref().unwrap();
    assert_eq!(l.assigned(), "assigned:claude");
    assert_eq!(l.review(), "review:claude");
}

#[test]
fn labels_adds_one_target_after_the_repos() {
    let c = Config::parse(&github(
        r#""repos":["o/a","o/b"],"labels":{"org":"CycleFive","tag":"claude"}"#,
    ))
    .unwrap();
    assert_eq!(
        c.agents["claude"].sources[0].target_keys(),
        vec!["github:G:o/a", "github:G:o/b", "github:G:labels:CycleFive"]
    );
    assert_eq!(
        github_source(&c).labels_key().as_deref(),
        Some("github:G:labels:CycleFive")
    );
}

#[test]
fn a_source_with_labels_may_have_no_repos() {
    let c = Config::parse(&github(r#""labels":{"org":"CycleFive","tag":"claude"}"#)).unwrap();
    assert_eq!(
        c.agents["claude"].sources[0].target_keys(),
        vec!["github:G:labels:CycleFive"]
    );
}

#[test]
fn a_github_source_needs_repos_or_labels() {
    assert!(err(&github(r#""repos":[]"#)).contains("repos: must not be empty unless labels is set"));
    assert!(err(&with_source(r#"{"type":"github","broker":"G"}"#))
        .contains("repos: must not be empty unless labels is set"));
}

#[test]
fn labels_org_and_tag_are_checked() {
    for org in ["", "has space"] {
        let e = err(&github(&format!(r#""labels":{{"org":"{org}","tag":"claude"}}"#)));
        assert!(e.contains("labels.org"), "{e}");
    }
    for tag in ["Upper", ""] {
        let e = err(&github(&format!(r#""labels":{{"org":"o","tag":"{tag}"}}"#)));
        assert!(e.contains("labels.tag"), "{e}");
    }
}

#[test]
fn labels_rejects_unknown_keys() {
    assert!(Config::parse(&github(r#""labels":{"org":"o","tag":"t","extra":1}"#)).is_err());
}

#[test]
fn a_source_without_labels_is_unchanged() {
    let c = Config::parse(&github(r#""repos":["o/a"]"#)).unwrap();
    assert_eq!(github_source(&c).labels_key(), None);
    assert_eq!(c.agents["claude"].sources[0].target_keys(), vec!["github:G:o/a"]);
}
