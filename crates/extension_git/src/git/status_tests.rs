use super::*;

/// Captured verbatim from `git status --porcelain=v2 --branch -z`, with NUL
/// separators written as `\0`. Hand-written status fixtures drift from what Git
/// actually emits, which is exactly the bug class this parser exists to avoid.
const MIXED: &str = concat!(
    "# branch.oid d3ad6c38ecf3b62eafc5fcfce68b94b61db5c352\0",
    "# branch.head main\0",
    "1 AM N... 000000 100644 100644 0000000000000000000000000000000000000000 49f33a8c6e8bb31f5d7c68f9c298cac55ec7cd85 both.txt\0",
    "1 D. N... 100644 000000 000000 f2ad6c76f0115a6ba5b00456a849810e7ec0af20 0000000000000000000000000000000000000000 gone.txt\0",
    "1 .M N... 100644 100644 100644 78981922613b2afb6025042ff6bd878ac1994e85 78981922613b2afb6025042ff6bd878ac1994e85 keep.txt\0",
    "2 R. N... 100644 100644 100644 61780798228d17af2d34fce4cfbdf35556832472 61780798228d17af2d34fce4cfbdf35556832472 R100 new name.txt\0",
    "old name.txt\0",
    "1 A. N... 000000 100644 100644 0000000000000000000000000000000000000000 19d9cc8584ac2c7dcf57d2680375e80f099dc481 staged.txt\0",
    "? untracked.txt\0",
);

fn paths(changes: &[FileChange]) -> Vec<&str> {
    changes.iter().map(|change| change.path.as_str()).collect()
}

#[test]
fn a_mixed_working_tree_is_sorted_into_its_sections() {
    let status = parse(MIXED);

    assert_eq!(status.branch.head.as_deref(), Some("main"));
    assert_eq!(status.branch.upstream, None);
    assert_eq!((status.branch.ahead, status.branch.behind), (0, 0));
    assert!(!status.branch.is_initial);

    assert_eq!(
        paths(&status.staged),
        ["both.txt", "gone.txt", "new name.txt", "staged.txt"]
    );
    assert_eq!(paths(&status.unstaged), ["both.txt", "keep.txt"]);
    assert_eq!(paths(&status.untracked), ["untracked.txt"]);
    assert!(status.conflicts.is_empty());
    assert_eq!(status.total_changes(), 7);
}

#[test]
fn a_path_staged_and_then_modified_again_appears_in_both_sections() {
    let status = parse(MIXED);

    let staged = status
        .staged
        .iter()
        .find(|change| change.path == "both.txt")
        .expect("both.txt is staged");
    let unstaged = status
        .unstaged
        .iter()
        .find(|change| change.path == "both.txt")
        .expect("both.txt is also modified in the worktree");

    assert_eq!(staged.kind, ChangeKind::Added);
    assert_eq!(unstaged.kind, ChangeKind::Modified);
}

#[test]
fn a_rename_keeps_its_original_path_and_consumes_the_extra_record() {
    let status = parse(MIXED);

    let renamed = status
        .staged
        .iter()
        .find(|change| change.path == "new name.txt")
        .expect("the rename is staged");
    assert_eq!(renamed.kind, ChangeKind::Renamed);
    assert_eq!(renamed.original_path.as_deref(), Some("old name.txt"));

    assert!(
        !paths(&status.staged).contains(&"old name.txt"),
        "the original-path record must be consumed, not read as another change"
    );
    assert!(
        paths(&status.staged).contains(&"staged.txt"),
        "records after a rename must still be parsed"
    );
}

#[test]
fn paths_containing_spaces_survive_intact() {
    let status = parse(MIXED);
    assert!(paths(&status.staged).contains(&"new name.txt"));
}

#[test]
fn a_deletion_is_reported_as_deleted_rather_than_missing() {
    let status = parse(MIXED);
    let deleted = status
        .staged
        .iter()
        .find(|change| change.path == "gone.txt")
        .expect("the deletion is staged");
    assert_eq!(deleted.kind, ChangeKind::Deleted);
    assert_eq!(deleted.kind.code(), "D");
}

#[test]
fn an_unmerged_path_is_a_conflict_and_not_a_staged_change() {
    let output = concat!(
        "# branch.oid 2d834329a935a696ef4292b1c51aa72abdce82da\0",
        "# branch.head main\0",
        "u UU N... 100644 100644 100644 100644 df967b96a579e45a18b8251732d16804b2e56a55 ba2906d0666cf726c7eaadd2cd3db615dedfdf3a e45c9c2666d44e0327c1f9c239a74c508336053e f.txt\0",
    );
    let status = parse(output);

    assert_eq!(paths(&status.conflicts), ["f.txt"]);
    assert!(status.staged.is_empty());
    assert!(status.unstaged.is_empty());
    assert!(!status.is_clean());
}

#[test]
fn an_empty_repository_reports_its_initial_state() {
    let status = parse("# branch.oid (initial)\0# branch.head main\0");

    assert!(status.branch.is_initial);
    assert_eq!(status.branch.head.as_deref(), Some("main"));
    assert!(status.is_clean());
}

#[test]
fn a_detached_head_has_no_branch_name() {
    let status =
        parse("# branch.oid 2d834329a935a696ef4292b1c51aa72abdce82da\0# branch.head (detached)\0");
    assert_eq!(status.branch.head, None);
    assert!(!status.branch.is_initial);
}

#[test]
fn divergence_from_upstream_is_read_from_the_branch_headers() {
    let status = parse(concat!(
        "# branch.oid abc123\0",
        "# branch.head feature\0",
        "# branch.upstream origin/feature\0",
        "# branch.ab +3 -12\0",
    ));

    assert_eq!(status.branch.upstream.as_deref(), Some("origin/feature"));
    assert_eq!(status.branch.ahead, 3);
    assert_eq!(status.branch.behind, 12);
}

#[test]
fn a_clean_repository_has_nothing_to_show() {
    let status = parse("# branch.oid abc123\0# branch.head main\0# branch.ab +0 -0\0");
    assert!(status.is_clean());
    assert_eq!(status.total_changes(), 0);
}

#[test]
fn empty_and_unrecognised_output_does_not_panic() {
    assert!(parse("").is_clean());
    assert!(parse("\0\0").is_clean());
    // A record type this parser predates must degrade the panel, not blank it.
    assert_eq!(
        parse("# branch.head main\0z something new\0? real.txt\0")
            .untracked
            .len(),
        1
    );
}

#[test]
fn truncated_records_are_skipped_rather_than_misparsed() {
    for truncated in [
        "1 AM\0",
        "1 AM N... 000000\0",
        "2 R. N... 100644\0",
        "u UU\0",
    ] {
        let status = parse(truncated);
        assert!(
            status.is_clean(),
            "`{truncated}` must not produce a bogus change"
        );
    }
}

#[test]
fn the_status_arguments_pin_the_format_this_parser_expects() {
    let args = status_args();
    assert!(args.contains(&"--porcelain=v2".to_owned()));
    assert!(
        args.contains(&"-z".to_owned()),
        "NUL termination is what makes paths unambiguous"
    );
    assert!(args.contains(&"--branch".to_owned()));
}
