use super::*;

fn remotes() -> Vec<String> {
    vec!["origin".to_owned(), "fork".to_owned()]
}

/// One record per line, fields separated by the unit separator, exactly as
/// `for-each-ref` emits for the format in [`branch_args`].
fn line(name: &str, upstream: &str, commit: &str, head: &str, subject: &str) -> String {
    format!("{name}\x1f{upstream}\x1f{commit}\x1f{head}\x1f{subject}")
}

#[test]
fn local_and_remote_branches_are_distinguished_by_remote_name() {
    let output = [
        line("main", "origin/main", "abc1234", "", "Initial commit"),
        line("feature/x", "", "def5678", "*", "Work in progress"),
        line("origin/main", "", "abc1234", "", "Initial commit"),
        line("fork/experiment", "", "999aaaa", "", "Try something"),
    ]
    .join("\n");

    let parsed = parse(&output, &remotes());
    assert_eq!(parsed.len(), 4);
    assert_eq!(parsed[0].scope, BranchScope::Local);
    assert_eq!(parsed[1].scope, BranchScope::Local);
    assert_eq!(parsed[2].scope, BranchScope::Remote);
    assert_eq!(parsed[3].scope, BranchScope::Remote);
}

#[test]
fn a_local_branch_named_like_a_remote_prefix_is_still_local() {
    // `origin-backup` starts with "origin" but is not "origin/", so the check
    // must be on the separator, not a bare prefix.
    let output = line("origin-backup", "", "abc1234", "", "Backup");
    let parsed = parse(&output, &remotes());
    assert_eq!(parsed[0].scope, BranchScope::Local);
}

#[test]
fn the_checked_out_branch_is_marked() {
    let output = [
        line("main", "origin/main", "abc1234", "", "Initial commit"),
        line("feature/x", "", "def5678", "*", "Work in progress"),
    ]
    .join("\n");

    let parsed = parse(&output, &remotes());
    assert!(!parsed[0].is_head);
    assert!(parsed[1].is_head);
}

#[test]
fn an_absent_upstream_is_none_rather_than_an_empty_string() {
    let output = [
        line("main", "origin/main", "abc1234", "", "Initial"),
        line("local-only", "", "def5678", "", "Local"),
    ]
    .join("\n");

    let parsed = parse(&output, &remotes());
    assert_eq!(parsed[0].upstream.as_deref(), Some("origin/main"));
    assert_eq!(parsed[1].upstream, None);
}

#[test]
fn the_remote_head_pointer_is_not_offered_as_a_branch() {
    let output = [
        line("origin/HEAD", "", "abc1234", "", ""),
        line("origin/main", "", "abc1234", "", "Initial"),
    ]
    .join("\n");

    let parsed = parse(&output, &remotes());
    assert_eq!(parsed.len(), 1);
    assert_eq!(parsed[0].name, "origin/main");
}

#[test]
fn a_subject_containing_spaces_and_slashes_survives() {
    let output = line(
        "feature/a-b",
        "origin/feature/a-b",
        "abc1234",
        "*",
        "Fix a/b handling in the parser",
    );
    let parsed = parse(&output, &remotes());
    assert_eq!(parsed[0].subject, "Fix a/b handling in the parser");
    assert_eq!(parsed[0].name, "feature/a-b");
}

#[test]
fn blank_and_malformed_lines_are_skipped() {
    let output = format!("\n\n{}\n\n", line("main", "", "abc", "", "x"));
    assert_eq!(parse(&output, &remotes()).len(), 1);
    assert!(parse("", &remotes()).is_empty());
}

#[test]
fn remotes_are_read_one_per_line() {
    assert_eq!(
        parse_remotes("origin\nfork\n"),
        ["origin".to_owned(), "fork".to_owned()]
    );
    assert!(parse_remotes("\n\n").is_empty());
}

#[test]
fn the_format_argument_requests_every_field_the_parser_reads() {
    let args = branch_args();
    let format = args
        .iter()
        .find(|arg| arg.starts_with("--format="))
        .expect("a format is pinned");
    for field in [
        "%(refname:short)",
        "%(upstream:short)",
        "%(objectname:short)",
        "%(HEAD)",
        "%(contents:subject)",
    ] {
        assert!(format.contains(field), "{format} is missing {field}");
    }
}
