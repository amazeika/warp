use extension_protocol::ErrorCode;
use warp_util::host_id::HostId;
use warp_util::remote_path::RemotePath;
use warp_util::standardized_path::StandardizedPath;

use super::*;

fn request(session_id: Option<SessionId>, file: Option<&str>) -> DiffRequest {
    DiffRequest {
        repo_path: match session_id {
            None => LocalOrRemotePath::Local("/home/dev/repo".into()),
            Some(_) => LocalOrRemotePath::Remote(RemotePath::new(
                HostId::new("host-1".to_owned()),
                StandardizedPath::try_new("/srv/repo").expect("a remote path"),
            )),
        },
        session_id,
        mode: DiffMode::Head,
        file: file.map(str::to_owned),
    }
}

#[test]
fn every_comparison_lands_on_the_only_base_this_build_has() {
    // The two narrower ones are not representable by today's diff model, and
    // the panel labels what it is showing, so widening to the superset is
    // visible to the user rather than silently wrong.
    for comparison in [
        DiffComparison::HeadToWorktree,
        DiffComparison::HeadToIndex,
        DiffComparison::IndexToWorktree,
    ] {
        assert_eq!(
            diff_mode(comparison),
            DiffMode::Head,
            "no comparison may resolve to a branch base nobody asked for"
        );
    }
}

#[test]
fn a_remote_diff_carries_the_session_it_named() {
    let session_id = session_of(&ExecutionTarget::Ssh {
        session_id: "9".to_owned(),
    })
    .expect("a session");

    assert_eq!(session_id, Some(SessionId::from(9)));
}

#[test]
fn a_local_diff_names_no_session_rather_than_a_wrong_one() {
    assert_eq!(
        session_of(&ExecutionTarget::Local).expect("no session"),
        None,
        "a repository on this machine is read without a session, not over whichever one is focused"
    );
}

#[test]
fn a_session_id_that_is_not_one_is_a_bad_request() {
    let error = session_of(&ExecutionTarget::Ssh {
        session_id: "the last one".to_owned(),
    })
    .expect_err("not a session id");

    assert_eq!(error.code, ErrorCode::InvalidRequest);
}

#[test]
fn a_diff_opened_without_a_pane_holds_no_pane() {
    let arg = request(Some(SessionId::from(9)), None).panel_arg();

    assert!(
        matches!(
            arg.origin,
            CodeReviewOrigin::Detached {
                session_id: Some(session_id)
            } if session_id == SessionId::from(9)
        ),
        "the session was named by the request, so a focus change between asking \
         and opening cannot move the diff to another host"
    );
}

#[test]
fn a_local_diff_opens_detached_with_nothing_to_load_over() {
    let arg = request(None, None).panel_arg();

    assert!(matches!(
        arg.origin,
        CodeReviewOrigin::Detached { session_id: None }
    ));
    assert_eq!(
        arg.repo_path,
        Some(LocalOrRemotePath::Local("/home/dev/repo".into()))
    );
}

#[test]
fn an_ordinary_repo_relative_path_survives_unchanged() {
    // Round-tripping matters: the same string keys the review's own file list
    // and comes back out of Git's output, so anything done to it here would
    // stop the two matching.
    for path in ["src/main.rs", "a.txt", "deep/nested/dir/file.rs", "src/"] {
        assert_eq!(
            repo_relative(path).expect("a path inside the repository"),
            path
        );
    }
}

#[test]
fn a_path_that_climbs_out_of_the_repository_is_refused() {
    for path in ["../secrets", "src/../../etc/passwd", ".."] {
        let error = repo_relative(path).expect_err("a path outside the repository");
        assert_eq!(error.code, ErrorCode::InvalidRequest, "{path}");
    }
}

#[test]
fn an_absolute_path_is_not_a_path_inside_a_repository() {
    // Absolute is refused rather than reinterpreted: the repository root came
    // from the request's target, and a path that ignores it names a file the
    // target never authorised.
    for path in ["/etc/passwd", r"\\server\share\file", r"C:\Windows\notepad"] {
        let error = repo_relative(path).expect_err("an absolute path");
        assert_eq!(error.code, ErrorCode::InvalidRequest, "{path}");
    }
}

#[test]
fn a_path_that_names_nothing_is_refused() {
    assert_eq!(
        repo_relative("").expect_err("an empty path").code,
        ErrorCode::InvalidRequest
    );
    assert_eq!(
        repo_relative("src//main.rs")
            .expect_err("an empty component")
            .code,
        ErrorCode::InvalidRequest
    );
}
