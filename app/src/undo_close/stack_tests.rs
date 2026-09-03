//! Releasing a closed window's SSH sessions.
//!
//! A window's panes are unreachable the moment it closes: `ClosedItem::Window::discard` resolves
//! its workspace through `views_of_type`, which returns `None` once the window is out of
//! `AppContext::windows`, and `clean_up_pane_group` early-returns on a closed window besides. So
//! the sessions are captured while the window is still live and released here, and these pin the
//! four outcomes that capture can lead to.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use remote_server::manager::RemoteServerManager;
use settings::Setting;
use warpui::App;
use warpui::geometry::rect::RectF;
use warpui::geometry::vector::vec2f;

use super::*;
use crate::terminal::view::TerminalView;
use crate::undo_close::UndoCloseSettings;
use crate::workspace::view::tests::{initialize_app, mock_workspace};

/// What the window's restore attempt should be allowed to do.
#[derive(Clone, Copy, PartialEq)]
enum Restore {
    /// Leave the entry on the stack; never undo.
    Never,
    /// Undo the close with cached bounds in place, so the window really comes back.
    Succeeds,
    /// Undo the close with no cached bounds, so `reopen_closed_window` returns at its
    /// missing-bounds guard and the window is gone for good.
    Fails,
    /// Never undo, and wait out a deliberately short grace period so the entry is discarded by
    /// its expiry timer rather than by `push_item`.
    Expires,
}

/// Closes a window holding one warpified SSH session and reports whether the manager still tracks
/// that session afterwards.
///
/// `undo_close_enabled` picks which discard path runs: with it off, `push_item` discards
/// immediately; with it on, the entry survives until `restore` says what happens to it.
fn still_tracked_after_window_close(undo_close_enabled: bool, restore: Restore) -> bool {
    let tracked = Arc::new(AtomicBool::new(true));
    let tracked_for_closure = tracked.clone();
    App::test((), |mut app| {
        let tracked = tracked_for_closure;
        async move {
            initialize_app(&mut app);
            let manager = RemoteServerManager::handle(&app);
            let session_id = warp_core::SessionId::from(1);

            UndoCloseSettings::handle(&app).update(&mut app, |settings, ctx| {
                settings
                    .enabled
                    .set_value(undo_close_enabled, ctx)
                    .expect("can set undo-close via settings");
                if restore == Restore::Expires {
                    settings
                        .grace_period
                        .set_value(Duration::from_millis(50), ctx)
                        .expect("can set the undo-close grace period via settings");
                }
            });

            manager.update(&mut app, |manager, _ctx| {
                manager.seed_connecting_session_for_test(session_id);
            });

            let workspace = mock_workspace(&mut app);
            let window_id = workspace.update(&mut app, |_workspace, ctx| ctx.window_id());

            workspace.update(&mut app, |workspace, ctx| {
                let pane_group = workspace.active_tab_pane_group().clone();
                pane_group.update(ctx, |pane_group, ctx| {
                    let terminal_view = pane_group
                        .terminal_views(ctx)
                        .into_iter()
                        .next()
                        .expect("the workspace opens with a terminal pane");
                    terminal_view.update(ctx, |view: &mut TerminalView, _ctx| {
                        view.record_ssh_wrapper_session_for_test(session_id);
                    });
                });
            });

            // A window created through `add_window` carries `WindowBounds::Default`, whose
            // `bounds()` is `None`, and no resize callback fires under the test platform. Seeding
            // the cache is therefore what separates a restore that works from one that trips
            // `reopen_closed_window`'s missing-bounds guard.
            if restore == Restore::Succeeds {
                app.update(|ctx| {
                    ctx.update_window_bounds(
                        window_id,
                        RectF::new(vec2f(0., 0.), vec2f(800., 600.)),
                    )
                });
            }

            // Stand in for the platform close: `close_window_async` is a no-op under test, so
            // nothing else drives `handle_window_closed` or the `on_window_will_close` callback
            // that forwards its data to the stack.
            let closed = app
                .update(|ctx| ctx.close_window_for_test(window_id))
                .expect("closing a live window yields its data");
            UndoCloseStack::handle(&app).update(&mut app, |stack, ctx| {
                stack.handle_window_closed(closed, ctx);
            });

            match restore {
                Restore::Never => {}
                Restore::Expires => {
                    // Ten times the grace period, so the wait is not a race with it.
                    warpui::r#async::Timer::after(Duration::from_millis(500)).await;
                }
                Restore::Succeeds | Restore::Fails => {
                    UndoCloseStack::handle(&app).update(&mut app, |stack, ctx| {
                        stack.undo_close(ctx);
                    });
                }
            }
            app.update(|_| ());

            manager.read(&app, |manager, _ctx| {
                tracked.store(manager.tracks_session(session_id), Ordering::Relaxed);
            });
        }
    });
    tracked.load(Ordering::Relaxed)
}

#[test]
fn a_window_closed_with_undo_disabled_releases_its_sessions() {
    assert!(
        !still_tracked_after_window_close(false, Restore::Never),
        "`push_item` discards immediately when undo-close is off, and that discard is the only \
         thing that will ever reach this window's panes"
    );
}

#[test]
fn a_window_closed_onto_the_undo_stack_keeps_its_sessions() {
    assert!(
        still_tracked_after_window_close(true, Restore::Never),
        "a window still on the undo stack can come back to the same connections, so releasing \
         while it waits would break the restore it is waiting for"
    );
}

#[test]
fn undoing_a_window_close_releases_nothing() {
    assert!(
        still_tracked_after_window_close(true, Restore::Succeeds),
        "the restored panes keep their connections and stay usable"
    );
}

#[test]
fn a_window_whose_restore_fails_releases_its_sessions() {
    assert!(
        !still_tracked_after_window_close(true, Restore::Fails),
        "`undo_close` has already popped the entry and consumed the `ClosedWindowData`, so a \
         restore that does not produce a live window leaves nothing else able to release these"
    );
}

/// The other discard path. With undo-close on, an entry nobody undoes is discarded by the expiry
/// timer rather than by `push_item`, and the release has to happen on that path too.
#[test]
fn a_window_whose_undo_entry_expires_releases_its_sessions() {
    assert!(
        !still_tracked_after_window_close(true, Restore::Expires),
        "once the grace period passes the window can no longer be restored, so the entry's \
         discard is the last thing that will ever reach its panes"
    );
}

/// Logging out reuses `Workspace::on_window_closed` without closing a window, so nothing will ever
/// deliver the `ClosedWindowData` that drains what it staged. The panes are destroyed immediately
/// afterwards without a `Closed` detach, so logout has to release them itself — and it must not
/// leave the entry behind, where a later real close of the same window would merge it in.
#[test]
fn logging_out_releases_its_sessions_instead_of_staging_them() {
    let tracked = Arc::new(AtomicBool::new(true));
    let staged_left = Arc::new(AtomicBool::new(true));
    let tracked_for_closure = tracked.clone();
    let staged_for_closure = staged_left.clone();
    App::test((), |mut app| {
        let (tracked, staged_left) = (tracked_for_closure, staged_for_closure);
        async move {
            initialize_app(&mut app);
            let manager = RemoteServerManager::handle(&app);
            let session_id = warp_core::SessionId::from(1);

            manager.update(&mut app, |manager, _ctx| {
                manager.seed_connecting_session_for_test(session_id);
            });

            let workspace = mock_workspace(&mut app);
            let window_id = workspace.update(&mut app, |_workspace, ctx| ctx.window_id());

            workspace.update(&mut app, |workspace, ctx| {
                let pane_group = workspace.active_tab_pane_group().clone();
                pane_group.update(ctx, |pane_group, ctx| {
                    let terminal_view = pane_group
                        .terminal_views(ctx)
                        .into_iter()
                        .next()
                        .expect("the workspace opens with a terminal pane");
                    terminal_view.update(ctx, |view: &mut TerminalView, _ctx| {
                        view.record_ssh_wrapper_session_for_test(session_id);
                    });
                });
                workspace.on_log_out(ctx);
            });
            app.update(|_| ());

            manager.read(&app, |manager, _ctx| {
                tracked.store(manager.tracks_session(session_id), Ordering::Relaxed);
            });
            let remaining = app.read(|ctx| {
                UndoCloseStack::as_ref(ctx).staged_window_ssh_sessions_for_test(window_id)
            });
            staged_left.store(!remaining.is_empty(), Ordering::Relaxed);
        }
    });

    assert!(
        !tracked.load(Ordering::Relaxed),
        "the logged-out panes are dropped without a Closed detach, so if logout does not release \
         them nothing ever will"
    );
    assert!(
        !staged_left.load(Ordering::Relaxed),
        "leaving the entry staged would orphan it under a still-open window, where a later real \
         close would merge it into that window's own sessions"
    );
}

/// The pane released its own session first, as a `DetachType::Closed` detach does, and the window
/// then closes still holding the id in its captured snapshot — the snapshot being a copy is what
/// makes that overlap normal rather than exceptional.
#[test]
fn releasing_a_session_the_manager_already_forgot_is_harmless() {
    App::test((), |mut app| async move {
        initialize_app(&mut app);
        let manager = RemoteServerManager::handle(&app);
        let session_id = warp_core::SessionId::from(1);

        manager.update(&mut app, |manager, _ctx| {
            manager.seed_connecting_session_for_test(session_id);
        });
        manager.update(&mut app, |manager, ctx| {
            manager.release_session_client(session_id, ctx);
        });
        assert!(
            !manager.read(&app, |manager, _ctx| manager.tracks_session(session_id)),
            "the first release stops the manager tracking the session"
        );

        manager.update(&mut app, |manager, ctx| {
            manager.release_session_client(session_id, ctx);
        });

        assert!(
            !manager.read(&app, |manager, _ctx| manager.tracks_session(session_id)),
            "a second release is a silent no-op, not an error and not a resurrection"
        );
    });
}
