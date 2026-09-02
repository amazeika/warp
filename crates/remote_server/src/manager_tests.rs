use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use futures::channel::oneshot;
use warp_core::SessionId;
use warp_util::standardized_path::StandardizedPath;
use warpui_core::{App, ModelHandle};

use super::{
    HostRequestError, MasterDisposition, PendingHostRequest, RemoteServerManager,
    RemoteServerManagerEvent, RemoteSessionState, RipgrepSearchParams,
};
use crate::HostId;
use crate::proto::{ClientMessage, RemoteAgentContextSnapshot, WriteFile, host_scoped_request};
use crate::protocol::RequestId;

#[test]
fn abort_host_request_removes_pending_request_and_resolves_caller() {
    App::test((), |mut app| async move {
        let manager = app.add_model(RemoteServerManager::new);
        let host_id = HostId::new("test-host".to_string());
        let request_id = RequestId::new();
        let (result_tx, result_rx) = oneshot::channel();
        let msg = ClientMessage::host_scoped(
            request_id.to_string(),
            host_scoped_request::Message::WriteFile(WriteFile {
                path: "/tmp/test".to_string(),
                content: String::new(),
            }),
        );

        manager.update(&mut app, |manager, _ctx| {
            manager.pending_host_requests.insert(
                request_id.clone(),
                PendingHostRequest {
                    host_id,
                    dispatched_session_id: SessionId::from(1),
                    msg,
                    result_tx,
                    timeout_abort: None,
                },
            );
            manager.abort_host_request(&request_id);
            assert!(!manager.pending_host_requests.contains_key(&request_id));
        });

        assert!(matches!(
            result_rx.await.expect("manager should resolve caller"),
            Err(HostRequestError::Aborted)
        ));
    });
}

#[test]
fn remote_agent_context_snapshot_is_a_host_scoped_manager_event() {
    let host_id = HostId::new("test-host".to_string());
    let event = RemoteServerManagerEvent::RemoteAgentContextSnapshot {
        host_id,
        snapshot: RemoteAgentContextSnapshot {
            revision: 1,
            home_dir: "/home/user".to_string(),
            skills: Vec::new(),
            global_rules: Vec::new(),
        },
    };
    assert!(event.session_id().is_none());
}

#[test]
fn remote_agent_context_snapshot_revisions_are_deduplicated_per_host() {
    App::test((), |mut app| async move {
        let manager = app.add_model(RemoteServerManager::new);
        let host_id = HostId::new("test-host".to_string());
        let other_host_id = HostId::new("other-host".to_string());

        manager.update(&mut app, |manager, ctx| {
            assert!(manager.accept_remote_agent_context_snapshot_revision(&host_id, 2));
            assert!(!manager.accept_remote_agent_context_snapshot_revision(&host_id, 2));
            assert!(!manager.accept_remote_agent_context_snapshot_revision(&host_id, 1));
            assert!(manager.accept_remote_agent_context_snapshot_revision(&host_id, 3));
            assert!(manager.accept_remote_agent_context_snapshot_revision(&other_host_id, 1));

            manager.handle_host_disconnected(&host_id, ctx);
            assert!(manager.accept_remote_agent_context_snapshot_revision(&host_id, 3));
        });
    });
}

#[test]
fn start_ripgrep_search_without_connected_host_resolves_immediately() {
    App::test((), |mut app| async move {
        let manager = app.add_model(RemoteServerManager::new);
        let host_id = HostId::new("missing-host".to_string());
        let pending = manager.update(&mut app, |manager, _ctx| {
            manager.start_ripgrep_search(
                &host_id,
                RipgrepSearchParams {
                    pattern: "needle".to_string(),
                    roots: vec![StandardizedPath::try_new("/repo").unwrap()],
                    ignore_case: false,
                    multiline: false,
                    max_matches: 100,
                },
            )
        });

        assert!(matches!(
            pending.result().await,
            Err(HostRequestError::AllSessionsDisconnected)
        ));
    });
}

/// Counts `SessionDeregistered` events for `session_id`.
fn track_deregistrations(
    app: &mut App,
    manager: &ModelHandle<RemoteServerManager>,
    session_id: SessionId,
) -> Arc<AtomicUsize> {
    let count = Arc::new(AtomicUsize::new(0));
    let count_for_closure = count.clone();
    app.update(|ctx| {
        ctx.subscribe_to_model(manager, move |_, event, _| {
            if matches!(
                event,
                RemoteServerManagerEvent::SessionDeregistered { session_id: id } if *id == session_id
            ) {
                count_for_closure.fetch_add(1, Ordering::Relaxed);
            }
        });
    });
    count
}

#[test]
fn release_session_client_stops_tracking_the_session() {
    App::test((), |mut app| async move {
        let manager = app.add_model(RemoteServerManager::new);
        let session_id = SessionId::from(1);

        manager.update(&mut app, |manager, ctx| {
            manager
                .sessions
                .insert(session_id, RemoteSessionState::Connecting);
            manager.release_session_client(session_id, ctx);

            assert!(
                !manager.sessions.contains_key(&session_id),
                "release should drop the session state, which is what SIGKILLs the proxy child"
            );
        });
    });
}

#[test]
fn release_session_client_clears_side_maps_left_behind_by_disconnect() {
    App::test((), |mut app| async move {
        let manager = app.add_model(RemoteServerManager::new);
        let session_id = SessionId::from(1);

        manager.update(&mut app, |manager, ctx| {
            // `mark_session_disconnected` removes the `sessions` entry on its own and leaves the
            // side maps behind, so "absent from `sessions`" is not "nothing left to clean".
            manager
                .session_labels
                .insert(session_id, "moira@devbox".to_string());
            manager.release_session_client(session_id, ctx);

            assert!(
                !manager.session_labels.contains_key(&session_id),
                "release should clean the side maps even with no `sessions` entry"
            );
        });
    });
}

#[test]
fn release_session_client_is_silent_for_an_untracked_session() {
    App::test((), |mut app| async move {
        let manager = app.add_model(RemoteServerManager::new);
        let session_id = SessionId::from(1);
        let deregistrations = track_deregistrations(&mut app, &manager, session_id);

        manager.update(&mut app, |manager, ctx| {
            manager.release_session_client(session_id, ctx);
        });

        assert_eq!(
            deregistrations.load(Ordering::Relaxed),
            0,
            "a session the manager never tracked must not be announced as deregistered"
        );
    });
}

#[test]
fn releasing_after_deregister_emits_no_second_deregistered() {
    App::test((), |mut app| async move {
        let manager = app.add_model(RemoteServerManager::new);
        let session_id = SessionId::from(1);
        let deregistrations = track_deregistrations(&mut app, &manager, session_id);

        manager.update(&mut app, |manager, ctx| {
            manager
                .sessions
                .insert(session_id, RemoteSessionState::Connecting);
            // The `ExitShell` hook arrives first, then the pane closes.
            manager.deregister_session(session_id, ctx);
            manager.release_session_client(session_id, ctx);
        });

        assert_eq!(
            deregistrations.load(Ordering::Relaxed),
            1,
            "the manager stops tracking a session once, so it announces that once"
        );
    });
}

/// The two teardown callers differ only in what they do with the master, and the force-exit
/// itself is a detached background task no test can observe — so the rule is pinned here.
#[test]
fn only_the_exit_shell_caller_force_exits_the_master() {
    assert!(
        MasterDisposition::ForceExitIfWarpOwned.force_exits(),
        "deregister_session runs after the user's shell exited, and its master is the interactive \
         ssh that hangs without an explicit -O exit"
    );
    assert!(
        !MasterDisposition::LeaveRunning.force_exits(),
        "releasing a pane's client must leave the master alone: a persistent one exists so a \
         split can still be attached to it, and a user-owned one was never ours to stop"
    );
}
