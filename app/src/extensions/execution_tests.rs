use super::*;

#[test]
fn a_local_target_needs_nothing_resolved() {
    warpui::App::test((), |mut app| async move {
        app.update(|ctx| {
            assert!(
                matches!(
                    ExecutionService::resolve(&ExecutionTarget::Local, ctx),
                    Ok(Runner::Local)
                ),
                "the local machine is always reachable, so nothing can make this fail"
            );
        });
    });
}

#[test]
fn a_session_id_that_is_not_one_is_a_bad_request() {
    let error = parse_session_id("session-3").expect_err("not a session id");

    assert_eq!(error.code, ErrorCode::InvalidRequest);
}

#[test]
fn a_session_warp_never_had_is_a_stale_target() {
    let error = refusal(SessionId::from(4), false);

    assert_eq!(
        error.code,
        ErrorCode::TargetStale,
        "the target was never Warp's to run on, which is different from one that ended"
    );
    assert!(error.message.contains('4'), "the message names the session");
}

#[test]
fn a_session_that_ended_fails_closed_rather_than_running_here() {
    let error = refusal(SessionId::from(4), true);

    assert_eq!(error.code, ErrorCode::SessionDisconnected);
}

#[test]
fn an_unreachable_remote_target_is_never_resolved_to_the_local_machine() {
    warpui::App::test((), |mut app| async move {
        app.update(|ctx| {
            let resolved = ExecutionService::resolve(
                &ExecutionTarget::Ssh {
                    session_id: "12".to_owned(),
                },
                ctx,
            );
            match resolved {
                Ok(_) => panic!("a remote command must not fall back to running here"),
                Err(error) => assert_eq!(error.code, ErrorCode::TargetStale),
            }
        });
    });
}

#[test]
fn argv_survives_quoting_as_one_word_each() {
    assert_eq!(
        shell_command(
            "git",
            &["commit".to_owned(), "-m".to_owned(), "a message".to_owned()]
        ),
        "'git' 'commit' '-m' 'a message'",
        "a message with a space is one argument, not two"
    );
}

#[test]
fn a_quoted_word_cannot_close_its_own_quoting() {
    // The whole risk the asymmetry in §7 carries: an argument that ends the
    // quoting would let the rest of it be read as shell syntax.
    assert_eq!(
        shell_command("git", &["-m".to_owned(), "it's fine".to_owned()]),
        r#"'git' '-m' 'it'\''s fine'"#
    );
    assert_eq!(
        shell_command("git", &["-m".to_owned(), "'; rm -rf /; '".to_owned()]),
        r#"'git' '-m' ''\''; rm -rf /; '\'''"#
    );
}

#[test]
fn shell_expansions_stay_literal_text() {
    assert_eq!(
        shell_command(
            "echo",
            &[
                "$(whoami)".to_owned(),
                "`id`".to_owned(),
                "$HOME".to_owned()
            ]
        ),
        "'echo' '$(whoami)' '`id`' '$HOME'",
        "single quotes are the one POSIX context where none of these expand"
    );
}

#[test]
fn an_empty_argument_stays_an_argument() {
    assert_eq!(
        shell_command("grep", &[String::new(), "file".to_owned()]),
        "'grep' '' 'file'",
        "dropping an empty argument would shift every one after it"
    );
}

#[test]
fn output_within_the_limit_is_returned_whole() {
    let (text, truncated) = truncate(b"ok\n".to_vec());

    assert_eq!(text, "ok\n");
    assert!(!truncated);
}

#[test]
fn output_over_the_limit_is_cut_and_says_so() {
    let (text, truncated) = truncate(vec![b'x'; MAX_OUTPUT_BYTES + 1]);

    assert_eq!(text.len(), MAX_OUTPUT_BYTES);
    assert!(
        truncated,
        "a plugin acting on a cut answer has to be able to tell it was cut"
    );
}

#[test]
fn a_character_split_by_the_cut_does_not_take_the_rest_with_it() {
    let mut output = vec![b'x'; MAX_OUTPUT_BYTES - 1];
    output.extend("é".as_bytes());
    let (text, truncated) = truncate(output);

    assert!(truncated);
    assert_eq!(
        text.chars().filter(|c| *c == 'x').count(),
        MAX_OUTPUT_BYTES - 1,
        "the half character is replaced, not treated as a reason to drop what came before"
    );
}
