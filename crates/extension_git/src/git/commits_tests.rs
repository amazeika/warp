use super::*;

fn record(hash: &str, short: &str, author: &str, age: &str, subject: &str) -> String {
    format!("{hash}\x1f{short}\x1f{author}\x1f{age}\x1f{subject}\x1e")
}

#[test]
fn commits_are_parsed_in_order() {
    let output = [
        record(
            "abc123def",
            "abc123d",
            "Ada",
            "2 hours ago",
            "Add the parser",
        ),
        record(
            "999888777",
            "9998887",
            "Grace",
            "3 days ago",
            "Fix the codec",
        ),
    ]
    .concat();

    let parsed = parse(&output);
    assert_eq!(parsed.len(), 2);
    assert_eq!(parsed[0].hash, "abc123def");
    assert_eq!(parsed[0].short_hash, "abc123d");
    assert_eq!(parsed[0].author, "Ada");
    assert_eq!(parsed[0].age, "2 hours ago");
    assert_eq!(parsed[0].subject, "Add the parser");
    assert_eq!(parsed[1].subject, "Fix the codec");
}

#[test]
fn a_subject_containing_newlines_stays_with_its_commit() {
    // Git's %s is the first line, but a pasted subject can still carry one, and
    // splitting on newlines would attribute the tail to the next commit.
    let output = [
        record("abc", "abc", "Ada", "now", "First line\nsecond line"),
        record("def", "def", "Grace", "now", "Next commit"),
    ]
    .concat();

    let parsed = parse(&output);
    assert_eq!(parsed.len(), 2);
    assert_eq!(parsed[0].subject, "First line\nsecond line");
    assert_eq!(parsed[1].subject, "Next commit");
}

#[test]
fn an_empty_subject_is_allowed() {
    let parsed = parse(&record("abc", "abc", "Ada", "now", ""));
    assert_eq!(parsed.len(), 1);
    assert_eq!(parsed[0].subject, "");
}

#[test]
fn empty_output_yields_no_commits() {
    assert!(parse("").is_empty());
    assert!(parse("\x1e\x1e").is_empty());
}

#[test]
fn the_log_arguments_bound_the_history_that_is_fetched() {
    let args = log_args(25);
    assert!(args.contains(&"--max-count=25".to_owned()));
    assert!(
        args.iter().any(|arg| arg.starts_with("--format=")),
        "history must use an explicit format, never Git's default"
    );
}
