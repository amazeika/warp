//! Parser for `git for-each-ref` over local and remote branches.
//!
//! `for-each-ref` with an explicit format is used rather than `git branch`
//! because the latter decorates its output for humans — a leading `*`, colour,
//! and truncation — none of which is stable to parse.
use serde::Serialize;

/// Field separator. Ref names cannot contain an ASCII unit separator, so this
/// cannot collide with the data.
const FIELD: char = '\x1f';

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum BranchScope {
    Local,
    Remote,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Branch {
    /// Short name: `main`, or `origin/main` for a remote-tracking branch.
    pub name: String,
    pub scope: BranchScope,
    pub upstream: Option<String>,
    pub short_commit: String,
    pub subject: String,
    /// True for the branch `HEAD` currently points at.
    pub is_head: bool,
}

pub fn branch_args() -> Vec<String> {
    vec![
        "for-each-ref".to_owned(),
        format!(
            "--format=%(refname:short){FIELD}%(upstream:short){FIELD}%(objectname:short){FIELD}%(HEAD){FIELD}%(contents:subject)"
        ),
        "refs/heads".to_owned(),
        "refs/remotes".to_owned(),
    ]
}

/// Parses `for-each-ref` output produced by [`branch_args`].
///
/// `refs/remotes/<remote>/HEAD` is dropped: it is a symbolic pointer at another
/// branch, not a branch a user can switch to.
pub fn parse(output: &str, remotes: &[String]) -> Vec<Branch> {
    output
        .lines()
        .filter_map(|line| parse_line(line, remotes))
        .filter(|branch| !branch.name.ends_with("/HEAD"))
        .collect()
}

fn parse_line(line: &str, remotes: &[String]) -> Option<Branch> {
    let mut fields = line.split(FIELD);
    let name = fields.next()?.trim();
    if name.is_empty() {
        return None;
    }
    let upstream = fields.next().unwrap_or_default().trim();
    let short_commit = fields.next().unwrap_or_default().trim();
    let head_marker = fields.next().unwrap_or_default().trim();
    let subject = fields.next().unwrap_or_default().trim();

    let scope = if remotes
        .iter()
        .any(|remote| name.starts_with(&format!("{remote}/")))
    {
        BranchScope::Remote
    } else {
        BranchScope::Local
    };

    Some(Branch {
        name: name.to_owned(),
        scope,
        upstream: (!upstream.is_empty()).then(|| upstream.to_owned()),
        short_commit: short_commit.to_owned(),
        subject: subject.to_owned(),
        is_head: head_marker == "*",
    })
}

/// Parses `git remote` output — one remote name per line.
pub fn parse_remotes(output: &str) -> Vec<String> {
    output
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(str::to_owned)
        .collect()
}

#[cfg(test)]
#[path = "branches_tests.rs"]
mod tests;
