use warp_util::host_id::HostId;
use warp_util::remote_path::RemotePath;
use warp_util::standardized_path::StandardizedPath;

use super::*;

fn remote(path: &str) -> LocalOrRemotePath {
    LocalOrRemotePath::Remote(RemotePath::new(
        HostId::new("host-1".to_owned()),
        StandardizedPath::try_new(path).expect("a remote path"),
    ))
}

fn local(path: &str) -> LocalOrRemotePath {
    LocalOrRemotePath::Local(path.into())
}

fn local_context() -> ActiveContext {
    ActiveContext {
        workspace_id: "1".to_owned(),
        active_pane_id: Some("7".to_owned()),
        cwd: Some(local("/home/dev/repo/crates")),
        repository_root: Some(local("/home/dev/repo")),
        session_id: Some(SessionId::from(42)),
        execution_target: ExecutionTarget::Local,
    }
}

fn remote_context() -> ActiveContext {
    ActiveContext {
        workspace_id: "1".to_owned(),
        active_pane_id: Some("7".to_owned()),
        cwd: Some(remote("/srv/repo")),
        repository_root: Some(remote("/srv/repo")),
        session_id: Some(SessionId::from(9)),
        execution_target: ExecutionTarget::Ssh {
            session_id: "9".to_owned(),
        },
    }
}

#[test]
fn a_pane_that_has_not_said_where_it_is_has_no_context_to_report() {
    let context = ActiveContext {
        cwd: None,
        ..local_context()
    };
    assert!(
        context.to_wire().is_none(),
        "there is no honest working directory to invent for a pane that has not reported one"
    );
    assert!(
        local_context().to_wire().is_some(),
        "a pane that has reported one is reportable"
    );
}

#[test]
fn a_remote_context_keeps_its_session_and_target() {
    let wire = remote_context().to_wire().expect("a reportable context");

    assert_eq!(wire.cwd, "/srv/repo");
    assert_eq!(wire.repository_root.as_deref(), Some("/srv/repo"));
    assert_eq!(wire.session_id.as_deref(), Some("9"));
    assert_eq!(
        wire.execution_target,
        ExecutionTarget::Ssh {
            session_id: "9".to_owned()
        },
        "a remote pane must not be described with a target that would run here"
    );
}

#[test]
fn an_origin_names_the_session_and_repository_the_action_started_in() {
    let origin = remote_context().origin();

    assert_eq!(origin.workspace_id, "1");
    assert_eq!(origin.session_id.as_deref(), Some("9"));
    assert_eq!(origin.repository_root.as_deref(), Some("/srv/repo"));
    assert_eq!(
        origin.execution_target,
        ExecutionTarget::Ssh {
            session_id: "9".to_owned()
        }
    );
}

fn kinds(events: &[ContextEvent]) -> Vec<EventKind> {
    events.iter().map(|event| event.kind).collect()
}

#[test]
fn the_first_context_reports_every_part_of_itself() {
    let context = local_context();

    assert_eq!(
        kinds(&WorkspaceContextService::changes(None, Some(&context))),
        vec![
            EventKind::WorkspaceChanged,
            EventKind::CwdChanged,
            EventKind::RepositoryChanged,
            EventKind::SessionChanged,
        ],
        "a plugin that has just started has been told nothing yet, so everything is new"
    );
}

#[test]
fn an_unchanged_context_says_nothing() {
    let context = local_context();

    assert!(
        WorkspaceContextService::changes(Some(&context), Some(&context)).is_empty(),
        "a refresh that changed nothing must not wake every plugin"
    );
}

#[test]
fn moving_within_one_repository_reports_only_the_directory() {
    let before = local_context();
    let after = ActiveContext {
        cwd: Some(local("/home/dev/repo/app")),
        ..before.clone()
    };

    let events = WorkspaceContextService::changes(Some(&before), Some(&after));
    assert_eq!(kinds(&events), vec![EventKind::CwdChanged]);
    assert!(
        matches!(
            &events[0].params,
            ContextEventParams::Context(params)
                if params.cwd.as_deref() == Some("/home/dev/repo/app")
                    && params.repository_root.as_deref() == Some("/home/dev/repo")
        ),
        "the event carries the whole context, so a plugin does not have to hold the unchanged half"
    );
}

#[test]
fn switching_windows_onto_the_same_repository_does_not_claim_it_changed() {
    let before = local_context();
    let after = ActiveContext {
        workspace_id: "2".to_owned(),
        active_pane_id: Some("11".to_owned()),
        ..before.clone()
    };

    assert_eq!(
        kinds(&WorkspaceContextService::changes(
            Some(&before),
            Some(&after)
        )),
        vec![EventKind::WorkspaceChanged],
        "the repository and directory are the same ones; only the window moved"
    );
}

#[test]
fn moving_onto_a_remote_session_reports_the_new_target() {
    let events = WorkspaceContextService::changes(Some(&local_context()), Some(&remote_context()));

    assert!(
        kinds(&events).contains(&EventKind::SessionChanged),
        "the target a later request would run against has changed, which is the whole point of the event"
    );
    let session = events
        .iter()
        .find(|event| event.kind == EventKind::SessionChanged)
        .expect("a session event");
    assert!(matches!(
        &session.params,
        ContextEventParams::Session(params)
            if params.execution_target
                == ExecutionTarget::Ssh { session_id: "9".to_owned() }
    ));
}

#[test]
fn a_new_session_in_the_same_directory_is_still_a_session_change() {
    let before = local_context();
    let after = ActiveContext {
        session_id: Some(SessionId::from(43)),
        ..before.clone()
    };

    assert_eq!(
        kinds(&WorkspaceContextService::changes(
            Some(&before),
            Some(&after)
        )),
        vec![EventKind::SessionChanged],
        "a restarted shell is a different session, and a plugin holding the old id must hear so"
    );
}

#[test]
fn losing_the_workspace_reports_nothing() {
    assert!(
        WorkspaceContextService::changes(Some(&local_context()), None).is_empty(),
        "a closing window is not a statement about a workspace; the next request is what fails"
    );
}

#[test]
fn every_context_event_encodes() {
    for event in WorkspaceContextService::changes(None, Some(&remote_context())) {
        let envelope = event.encode().expect("the event encodes");
        assert_eq!(envelope.event, event.kind);
    }
}

#[test]
fn a_local_path_is_bound_to_this_machine() {
    warpui::App::test((), |mut app| async move {
        app.update(|ctx| {
            assert_eq!(
                resolve_path(&ExecutionTarget::Local, "/home/dev/repo/src/main.rs", ctx)
                    .expect("a local path"),
                local("/home/dev/repo/src/main.rs")
            );
        });
    });
}

#[test]
fn a_relative_path_is_never_completed_from_somewhere_warp_picked() {
    warpui::App::test((), |mut app| async move {
        app.update(|ctx| {
            let error = resolve_path(&ExecutionTarget::Local, "src/main.rs", ctx)
                .expect_err("not an absolute path");
            assert_eq!(error.code, ErrorCode::InvalidRequest);
        });
    });
}

#[test]
fn a_remote_path_is_never_resolved_to_the_same_named_local_file() {
    warpui::App::test((), |mut app| async move {
        app.update(|ctx| {
            // The session is one this build has never heard of, which is the
            // case that matters: the highest-consequence failure this API has
            // is acting on `/srv/repo` here when the plugin meant `/srv/repo`
            // over there.
            let resolved = resolve_path(
                &ExecutionTarget::Ssh {
                    session_id: "12".to_owned(),
                },
                "/srv/repo/src/main.rs",
                ctx,
            );
            match resolved {
                Ok(path) => panic!("a remote path must not resolve to {path:?}"),
                Err(error) => assert_eq!(error.code, ErrorCode::TargetStale),
            }
        });
    });
}

#[test]
fn a_session_id_that_is_not_one_is_a_bad_request_rather_than_a_stale_target() {
    warpui::App::test((), |mut app| async move {
        app.update(|ctx| {
            let error = resolve_path(
                &ExecutionTarget::Ssh {
                    session_id: "over-there".to_owned(),
                },
                "/srv/repo",
                ctx,
            )
            .expect_err("not a session id");
            assert_eq!(error.code, ErrorCode::InvalidRequest);
        });
    });
}
