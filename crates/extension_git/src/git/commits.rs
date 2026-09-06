//! Parser for `git log` with explicit record and field separators.
use serde::Serialize;

/// Field and record separators. Commit metadata is free text — a subject can
/// contain anything a user typed — so the separators are ASCII control
/// characters Git will not emit from `%s`, `%an` or `%ar`.
const FIELD: char = '\x1f';
const RECORD: char = '\x1e';

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Commit {
    pub hash: String,
    pub short_hash: String,
    pub author: String,
    /// Relative age as Git formats it, e.g. "2 hours ago".
    pub age: String,
    pub subject: String,
}

pub fn log_args(limit: u32) -> Vec<String> {
    vec![
        "log".to_owned(),
        format!("--max-count={limit}"),
        format!("--format=%H{FIELD}%h{FIELD}%an{FIELD}%ar{FIELD}%s{RECORD}"),
    ]
}

/// Parses `git log` output produced by [`log_args`].
pub fn parse(output: &str) -> Vec<Commit> {
    output
        .split(RECORD)
        .map(str::trim)
        .filter(|record| !record.is_empty())
        .filter_map(parse_record)
        .collect()
}

fn parse_record(record: &str) -> Option<Commit> {
    // The subject is taken as the whole remainder rather than another split,
    // because a commit subject is free text the user typed.
    let mut fields = record.splitn(5, FIELD);
    let hash = fields.next()?.trim();
    if hash.is_empty() {
        return None;
    }
    Some(Commit {
        hash: hash.to_owned(),
        short_hash: fields.next().unwrap_or_default().to_owned(),
        author: fields.next().unwrap_or_default().to_owned(),
        age: fields.next().unwrap_or_default().to_owned(),
        subject: fields.next().unwrap_or_default().trim().to_owned(),
    })
}

#[cfg(test)]
#[path = "commits_tests.rs"]
mod tests;
