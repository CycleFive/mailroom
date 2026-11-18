//! Mechanical rule engine: decides which records are kept and which extra tags they get.

use crate::config::{Keep, Rule, Then, When};
use crate::record::{Kind, NewRecord};

/// Rules first to last, then `keep`. Built-in tags are already on the records.
/// A `gap` record is the only sign that items were skipped, so it bypasses
/// every rule and `keep`, and is always kept untouched.
pub fn apply(records: Vec<NewRecord>, rules: &[Rule], keep: Keep) -> Vec<NewRecord> {
    records
        .into_iter()
        .filter_map(|record| {
            if record.kind == Kind::Gap {
                return Some(record);
            }
            run_rules(record, rules).filter(|record| keep == Keep::All || !record.tags.is_empty())
        })
        .collect()
}

fn run_rules(mut record: NewRecord, rules: &[Rule]) -> Option<NewRecord> {
    for rule in rules {
        if !matches(&rule.when, &record) {
            continue;
        }
        match &rule.then {
            Then::Drop => return None,
            Then::Tag(tag) => {
                if !record.tags.contains(tag) {
                    record.tags.push(tag.clone());
                }
            }
        }
    }
    Some(record)
}

/// Empty lists are "no condition"; a non-empty list matches if any element does;
/// all non-empty conditions must match.
pub fn matches(when: &When, record: &NewRecord) -> bool {
    let actor = record.actor.to_lowercase();
    let actor_is = |a: &String| a.to_lowercase() == actor;
    (when.kind.is_empty() || when.kind.contains(&record.kind))
        && (when.actor.is_empty() || when.actor.iter().any(actor_is))
        && (when.actor_not.is_empty() || !when.actor_not.iter().any(actor_is))
        && (when.reference.is_empty() || when.reference.contains(&record.reference))
        && (when.tag.is_empty() || when.tag.iter().any(|t| record.tags.contains(t)))
        && (when.body_contains.is_empty() || {
            let haystack = format!("{}\n{}", record.title, record.body).to_lowercase();
            when.body_contains
                .iter()
                .any(|needle| haystack.contains(&needle.to_lowercase()))
        })
}
