//! Validated mailroom configuration (the `MAILROOM` var).

use std::collections::BTreeMap;
use std::collections::BTreeSet;
use std::fmt;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::record::{Kind, Src};

const MIN_EVERY_SECS: u32 = 60;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Config {
    #[serde(default = "default_retain_days")]
    pub retain_days: u32,
    pub agents: BTreeMap<String, AgentConfig>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct AgentConfig {
    /// The service binding of the Discord broker that signs this agent's users in.
    pub signin: String,
    pub sources: Vec<SourceConfig>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum SourceConfig {
    Discord(DiscordSource),
    Github(GithubSource),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct DiscordSource {
    pub broker: String,
    #[serde(default = "default_discord_every")]
    pub every_secs: u32,
    pub channels: Vec<String>,
    #[serde(default = "default_true")]
    pub reactions: bool,
    #[serde(default = "default_true")]
    pub skip_self: bool,
    #[serde(default)]
    pub keep: Keep,
    #[serde(default)]
    pub rules: Vec<Rule>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct GithubSource {
    pub broker: String,
    #[serde(default = "default_github_every")]
    pub every_secs: u32,
    pub repos: Vec<String>,
    #[serde(default)]
    pub review_label: Option<String>,
    #[serde(default = "default_true")]
    pub skip_self: bool,
    #[serde(default)]
    pub keep: Keep,
    #[serde(default)]
    pub rules: Vec<Rule>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Keep {
    #[default]
    All,
    Tagged,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Rule {
    pub when: When,
    pub then: Then,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct When {
    pub kind: Vec<Kind>,
    pub actor: Vec<String>,
    pub actor_not: Vec<String>,
    #[serde(rename = "ref")]
    pub reference: Vec<String>,
    pub tag: Vec<String>,
    pub body_contains: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Then {
    Drop,
    Tag(String),
}

fn default_retain_days() -> u32 {
    30
}
fn default_discord_every() -> u32 {
    120
}
fn default_github_every() -> u32 {
    300
}
fn default_true() -> bool {
    true
}

/// A configuration problem; `Display` prints the message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfigError(pub String);

impl fmt::Display for ConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for ConfigError {}

impl Config {
    /// Deserializes, then validates.
    pub fn parse(json: &str) -> Result<Config, ConfigError> {
        let config: Config =
            serde_json::from_str(json).map_err(|e| ConfigError(format!("invalid config: {e}")))?;
        config.validate()?;
        Ok(config)
    }

    pub fn validate(&self) -> Result<(), ConfigError> {
        if self.agents.is_empty() {
            return Err(ConfigError("agents: must not be empty".into()));
        }
        for (name, agent) in &self.agents {
            let valid_name = (1..=32).contains(&name.len())
                && name
                    .bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-');
            if !valid_name {
                return Err(ConfigError(format!(
                    "agent {name:?}: name must be 1-32 chars of a-z, 0-9, '-'"
                )));
            }
            if !is_binding_name(&agent.signin) {
                return Err(ConfigError(format!(
                    "agent {name:?}: signin: {:?} must be non-empty and only A-Z, 0-9, '_'",
                    agent.signin
                )));
            }
            for (i, source) in agent.sources.iter().enumerate() {
                validate_source(name, i, source)?;
            }
        }
        Ok(())
    }
}

/// A service binding name: non-empty, only `A-Z`, `0-9`, `_`.
fn is_binding_name(name: &str) -> bool {
    !name.is_empty()
        && name
            .bytes()
            .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit() || b == b'_')
}

fn validate_source(agent: &str, index: usize, source: &SourceConfig) -> Result<(), ConfigError> {
    let at = format!("agent {agent:?} source {index}");
    let fail = |field: &str, msg: String| Err(ConfigError(format!("{at}: {field}: {msg}")));

    let broker = source.broker();
    if !is_binding_name(broker) {
        return fail(
            "broker",
            format!("{broker:?} must be non-empty and only A-Z, 0-9, '_'"),
        );
    }
    if source.every_secs() < MIN_EVERY_SECS {
        return fail(
            "every_secs",
            format!("{} is below the minimum of {MIN_EVERY_SECS}", source.every_secs()),
        );
    }

    let mut seen = BTreeSet::new();
    match source {
        SourceConfig::Discord(d) => {
            if d.channels.is_empty() {
                return fail("channels", "must not be empty".into());
            }
            for c in &d.channels {
                if !(5..=25).contains(&c.len()) || !c.bytes().all(|b| b.is_ascii_digit()) {
                    return fail("channels", format!("{c:?} must be 5-25 ASCII digits"));
                }
                if !seen.insert(c) {
                    return fail("channels", format!("duplicate {c:?}"));
                }
            }
        }
        SourceConfig::Github(g) => {
            if g.repos.is_empty() {
                return fail("repos", "must not be empty".into());
            }
            for r in &g.repos {
                let ok = matches!(r.split_once('/'), Some((o, n)) if !o.is_empty() && !n.is_empty() && !n.contains('/'));
                if !ok {
                    return fail("repos", format!("{r:?} must be owner/name"));
                }
                if !seen.insert(r) {
                    return fail("repos", format!("duplicate {r:?}"));
                }
            }
            if g.review_label.as_deref() == Some("") {
                return fail("review_label", "must not be empty".into());
            }
        }
    }
    for rule in source.rules() {
        if rule.then == Then::Tag(String::new()) {
            return fail("rules", "tag must not be empty".into());
        }
    }
    Ok(())
}

impl SourceConfig {
    pub fn broker(&self) -> &str {
        match self {
            SourceConfig::Discord(d) => &d.broker,
            SourceConfig::Github(g) => &g.broker,
        }
    }

    pub fn every_secs(&self) -> u32 {
        match self {
            SourceConfig::Discord(d) => d.every_secs,
            SourceConfig::Github(g) => g.every_secs,
        }
    }

    pub fn src(&self) -> Src {
        match self {
            SourceConfig::Discord(_) => Src::Discord,
            SourceConfig::Github(_) => Src::Github,
        }
    }

    pub fn keep(&self) -> Keep {
        match self {
            SourceConfig::Discord(d) => d.keep,
            SourceConfig::Github(g) => g.keep,
        }
    }

    pub fn rules(&self) -> &[Rule] {
        match self {
            SourceConfig::Discord(d) => &d.rules,
            SourceConfig::Github(g) => &g.rules,
        }
    }

    /// "discord:<broker>:<channel>" or "github:<broker>:<owner>/<repo>", in config order.
    pub fn target_keys(&self) -> Vec<String> {
        match self {
            SourceConfig::Discord(d) => d
                .channels
                .iter()
                .map(|c| format!("discord:{}:{c}", d.broker))
                .collect(),
            SourceConfig::Github(g) => g
                .repos
                .iter()
                .map(|r| format!("github:{}:{r}", g.broker))
                .collect(),
        }
    }
}
