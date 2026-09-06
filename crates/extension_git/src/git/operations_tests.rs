use super::*;

fn paths(items: &[&str]) -> Vec<String> {
    items.iter().map(|item| (*item).to_owned()).collect()
}

#[test]
fn paths_are_separated_from_revisions_by_a_double_dash() {
    // Without `--`, a file named like a branch is ambiguous and Git may act on
    // the branch instead of the file.
    for argv in [
        stage(&paths(&["main"])),
        unstage(&paths(&["main"])),
        discard(&paths(&["main"])),
        delete_untracked(&paths(&["main"])),
    ] {
        let separator = argv
            .iter()
            .position(|arg| arg == "--")
            .expect("every path-taking command separates its paths");
        assert_eq!(
            argv[separator + 1..],
            ["main".to_owned()],
            "paths must follow the separator"
        );
    }
}

#[test]
fn staging_and_unstaging_are_symmetric_and_never_move_head() {
    assert_eq!(stage(&paths(&["a.rs"])), ["add", "--", "a.rs"]);
    assert_eq!(
        unstage(&paths(&["a.rs"])),
        ["restore", "--staged", "--", "a.rs"],
        "unstaging must use restore, not reset, so HEAD is untouched"
    );
    assert_eq!(stage_all(), ["add", "--all"]);
    assert_eq!(unstage_all(), ["reset", "--mixed", "HEAD"]);
}

#[test]
fn a_commit_passes_its_message_as_one_argument() {
    // A message with spaces, quotes and newlines must survive as a single argv
    // entry; there is no shell to re-split it.
    let message = "Fix \"the\" thing\n\nWith a body";
    let argv = commit(message);
    assert_eq!(argv, ["commit", "--message", message]);
    assert!(
        !argv.contains(&"--".to_owned()),
        "a commit takes no paths; passing one would commit that path, not the index"
    );
}

#[test]
fn a_force_update_never_maps_to_plain_force() {
    let argv = force_push_with_lease();
    assert_eq!(argv, ["push", "--force-with-lease"]);
    assert!(
        !argv.contains(&"--force".to_owned()),
        "--force discards a teammate's commits without warning; --force-with-lease refuses"
    );
}

#[test]
fn branch_deletion_distinguishes_safe_from_forced() {
    assert_eq!(delete_branch("old"), ["branch", "--delete", "old"]);
    assert_eq!(
        force_delete_branch("old"),
        ["branch", "--delete", "--force", "old"]
    );
    assert!(
        !delete_branch("old").contains(&"--force".to_owned()),
        "the safe delete must let Git refuse an unmerged branch"
    );
}

#[test]
fn unmerged_commits_are_counted_in_the_right_direction() {
    // `base..branch` is commits on the branch but not the base — what would be
    // lost. The reverse would count what the branch is missing.
    assert_eq!(
        unmerged_commit_count("feature", "main"),
        ["rev-list", "--count", "main..feature"]
    );
}

#[test]
fn deleting_untracked_files_is_always_scoped_to_named_paths() {
    let argv = delete_untracked(&paths(&["scratch.txt"]));
    assert_eq!(argv, ["clean", "--force", "--", "scratch.txt"]);
    for dangerous in ["-d", "-x", "-fdx", "--force -d"] {
        assert!(
            !argv.iter().any(|arg| arg == dangerous),
            "a bulk clean is not something a user can undo"
        );
    }
}

#[test]
fn a_pull_never_creates_a_merge_the_user_did_not_ask_for() {
    assert_eq!(pull(), ["pull", "--ff-only"]);
}

#[test]
fn a_fetch_prunes_deleted_remote_branches() {
    assert_eq!(fetch(), ["fetch", "--prune"]);
}

#[test]
fn setting_an_upstream_names_the_remote_and_branch_explicitly() {
    assert_eq!(
        push_setting_upstream("origin", "feature/x"),
        ["push", "--set-upstream", "origin", "feature/x"]
    );
}

#[test]
fn checking_out_a_remote_branch_creates_a_tracking_branch() {
    assert_eq!(
        switch_to_remote_branch("feature/x", "origin/feature/x"),
        [
            "switch",
            "--create",
            "feature/x",
            "--track",
            "origin/feature/x"
        ]
    );
}

#[test]
fn branch_creation_and_switching_use_switch_rather_than_checkout() {
    // `checkout` is overloaded — the same verb discards file changes — so a
    // typo in a branch name should not be able to destroy work.
    assert_eq!(
        create_branch("feature/x"),
        ["switch", "--create", "feature/x"]
    );
    assert_eq!(switch_branch("main"), ["switch", "main"]);
}

#[test]
fn rebase_continue_and_abort_are_available_separately_from_starting_one() {
    assert_eq!(rebase_onto("main"), ["rebase", "main"]);
    assert_eq!(rebase_continue(), ["rebase", "--continue"]);
    assert_eq!(rebase_abort(), ["rebase", "--abort"]);
    assert_eq!(merge_abort(), ["merge", "--abort"]);
}

#[test]
fn no_operation_ever_invokes_a_shell() {
    let every_argv = [
        stage(&paths(&["a"])),
        stage_all(),
        unstage(&paths(&["a"])),
        unstage_all(),
        discard(&paths(&["a"])),
        delete_untracked(&paths(&["a"])),
        commit("m"),
        fetch(),
        pull(),
        push(),
        push_setting_upstream("origin", "b"),
        force_push_with_lease(),
        create_branch("b"),
        switch_branch("b"),
        switch_to_remote_branch("b", "origin/b"),
        rename_branch("a", "b"),
        delete_branch("b"),
        force_delete_branch("b"),
        unmerged_commit_count("b", "main"),
        rebase_onto("main"),
        rebase_continue(),
        rebase_abort(),
        merge_abort(),
        remotes(),
    ];
    for argv in every_argv {
        for arg in &argv {
            assert!(
                !matches!(arg.as_str(), "sh" | "bash" | "-c" | "zsh"),
                "`{arg}` in {argv:?} would introduce a shell"
            );
        }
    }
}
