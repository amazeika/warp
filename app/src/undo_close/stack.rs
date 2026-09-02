use std::collections::HashMap;

use uuid::Uuid;
use warp_errors::report_error;
use warpui::r#async::SpawnedFutureHandle;
use warpui::{
    AppContext, ClosedWindowData, Entity, EntityId, ModelContext, ModelHandle, SingletonEntity,
    ViewHandle, WeakViewHandle, WindowId,
};

use super::UndoCloseSettings;
use super::settings::UndoCloseSettingsChangedEvent;
use crate::ai::active_agent_views_model::ActiveAgentViewsModel;
use crate::ai::blocklist::BlocklistAIHistoryModel;
use crate::pane_group::{PaneGroup, PaneId};
use crate::remote_server::manager::RemoteServerManager;
use crate::send_telemetry_from_app_ctx;
use crate::server::telemetry::{TelemetryEvent, UndoCloseItemType};
use crate::tab::TabData;
use crate::terminal::model::session::SessionId;
use crate::workspace::Workspace;

/// A unique identifier for an item in the undo close stack.
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
struct ItemId(Uuid);

impl ItemId {
    /// Constructs a new ItemId.
    fn new() -> Self {
        Self(Uuid::new_v4())
    }
}

/// Data for an item in the undo close stack.
struct UndoData {
    closed_item: ClosedItem,
    expiry_data: ExpiryData,
}

/// Data needed to handle expiration for items in the undo close stack.
struct ExpiryData {
    id: ItemId,
    task_handle: SpawnedFutureHandle,
}

impl std::ops::Drop for ExpiryData {
    fn drop(&mut self) {
        // Make sure we abort the expiry task when we drop the expiry data.
        self.task_handle.abort();
    }
}

/// Data needed to restore a closed pane.
pub(super) struct PaneData {
    /// The pane ID - content is retrieved from the pane group during restoration
    pane_id: PaneId,
    /// Reference to the pane group that contained this pane
    pane_group: WeakViewHandle<PaneGroup>,
}

/// An item in the undo close stack which can be re-opened.
pub enum ClosedItem {
    Window {
        data: Box<ClosedWindowData>,
        /// The SSH wrapper sessions this window's panes started, captured by
        /// `Workspace::on_window_closed` while the window was still live. Released only if the
        /// window turns out to be unrestorable; dropped untouched on a successful undo, whose
        /// restored panes stay armed to release themselves.
        ssh_sessions: Vec<SessionId>,
    },
    Tab {
        workspace: WeakViewHandle<Workspace>,
        tab_index: usize,
        data: TabData,
    },
    Pane {
        data: PaneData,
    },
}

impl ClosedItem {
    fn discard(self, ctx: &mut ModelContext<UndoCloseStack>) {
        let history_model = BlocklistAIHistoryModel::handle(ctx);

        match self {
            ClosedItem::Window { data, ssh_sessions } => {
                // This window is gone for good, and its panes were never reachable from here to
                // release themselves: `clean_up_pane_group` below early-returns on a closed
                // window, and the window discard has always been a no-op for that reason.
                release_ssh_sessions(ssh_sessions, ctx);

                let ClosedWindowData { window_id, .. } = *data;
                ActiveAgentViewsModel::handle(ctx).update(ctx, |model, ctx| {
                    model.remove_focused_state_for_window(window_id, ctx);
                });
                if let Some(workspace) = window_workspace(window_id, ctx) {
                    workspace.update(ctx, |workspace, ctx| {
                        for pane_group in workspace.tab_views() {
                            // Mark conversations from all terminal panes in each tab
                            Self::mark_conversations_historical_for_pane_group(
                                pane_group,
                                &history_model,
                                ctx,
                            );
                            Self::clean_up_pane_group(pane_group, ctx);
                        }
                    });
                }
            }
            ClosedItem::Tab { data, .. } => {
                // Mark conversations from all terminal panes in the tab
                Self::mark_conversations_historical_for_pane_group(
                    &data.pane_group,
                    &history_model,
                    ctx,
                );
                Self::clean_up_pane_group(&data.pane_group, ctx);
            }
            ClosedItem::Pane { data } => {
                ctx.emit(UndoCloseStackEvent::DiscardPane(data.pane_id));
            }
        }
    }

    /// Marks conversations as historical for all terminal panes in a pane group so they remain searchable.
    /// Historical conversations consist of non-live conversations that were read from disk on startup,
    /// and conversations (recorded here) that were live this session but have now been cleared.
    fn mark_conversations_historical_for_pane_group(
        pane_group: &ViewHandle<PaneGroup>,
        history_model: &ModelHandle<BlocklistAIHistoryModel>,
        ctx: &mut AppContext,
    ) {
        // Check if the window and view still exist before attempting to read
        let window_id = pane_group.window_id(ctx);
        let view_id = pane_group.id();

        if ctx.view_with_id::<PaneGroup>(window_id, view_id).is_some() {
            let terminal_view_ids: Vec<EntityId> = pane_group.read(ctx, |pg, ctx| {
                pg.terminal_pane_ids()
                    .filter_map(|pane_id| {
                        pg.terminal_view_from_pane_id(pane_id, ctx)
                            .map(|terminal_view| terminal_view.id())
                    })
                    .collect()
            });

            for terminal_view_id in terminal_view_ids {
                history_model.update(ctx, |history_model, _| {
                    history_model
                        .mark_conversations_historical_for_terminal_surface(terminal_view_id);
                });
            }
        }
    }

    fn clean_up_pane_group(pane_group: &ViewHandle<PaneGroup>, ctx: &mut AppContext) {
        let window_id = pane_group.window_id(ctx);

        if !ctx.is_window_open(window_id) {
            return;
        }

        pane_group.update(ctx, |pane_group, ctx| {
            pane_group.clean_up_panes(ctx);
        });
    }
}

pub enum UndoCloseStackEvent {
    DiscardPane(PaneId),
}

/// A stack of closed items which can be re-opened in LIFO order.
pub struct UndoCloseStack {
    stack: Vec<UndoData>,
    /// Sessions captured by `Workspace::on_window_closed` while each window was still live,
    /// waiting for the `handle_window_closed` that names the same window.
    staged_window_ssh_sessions: HashMap<WindowId, Vec<SessionId>>,
}

/// Releases Warp's own multiplexed client for each session, so no `remote-server-proxy` child
/// outlives the panes that started them.
///
/// The `ControlMaster` itself is left alone deliberately — a persistent one exists so a split can
/// still attach to it, and a user-owned one was never Warp's to stop; both are reaped by the idle
/// timeout once this was the last client. Releasing a session the manager no longer tracks is a
/// silent no-op, which is what makes a second release harmless.
fn release_ssh_sessions(ssh_sessions: Vec<SessionId>, ctx: &mut AppContext) {
    if ssh_sessions.is_empty() {
        return;
    }
    RemoteServerManager::handle(ctx).update(ctx, |manager, ctx| {
        for session_id in ssh_sessions {
            manager.release_session_client(session_id, ctx);
        }
    });
}

impl UndoCloseStack {
    /// Constructs a new undo close stack.
    pub fn new(ctx: &mut ModelContext<Self>) -> Self {
        ctx.subscribe_to_model(&UndoCloseSettings::handle(ctx), |me, _, event, ctx| {
            me.handle_settings_event(event, ctx);
        });

        Self {
            stack: Default::default(),
            staged_window_ssh_sessions: Default::default(),
        }
    }

    /// Returns whether or not the stack is empty.
    #[cfg_attr(not(target_os = "macos"), allow(dead_code))]
    pub fn is_empty(&self) -> bool {
        self.stack.is_empty()
    }

    /// Returns true only if the pane group is present in the undo close stack as part of a closed tab.
    pub fn is_pane_group_tab_in_stack(&self, pane_group_id: EntityId) -> bool {
        self.stack
            .iter()
            .any(|undo_data| matches!(&undo_data.closed_item, ClosedItem::Tab { data, .. } if data.pane_group.id() == pane_group_id))
    }

    /// Discards a pane group from the undo close stack early.
    pub fn discard_pane_group_parent(
        &mut self,
        pane_group_id: EntityId,
        ctx: &mut ModelContext<Self>,
    ) {
        if let Some(pos) = self
            .stack
            .iter()
            .position(|undo_data| match &undo_data.closed_item {
                ClosedItem::Tab { data, .. } => data.pane_group.id() == pane_group_id,
                ClosedItem::Pane { data } => data.pane_group.id() == pane_group_id,
                _ => false,
            })
        {
            let removed_item = self.stack.remove(pos);
            removed_item.expiry_data.task_handle.abort();
            removed_item.closed_item.discard(ctx);
        }
    }

    /// Handles a window being closed, adding the necessary data to the undo
    /// stack.
    pub fn handle_window_closed(&mut self, data: ClosedWindowData, ctx: &mut ModelContext<Self>) {
        let ssh_sessions = self
            .staged_window_ssh_sessions
            .remove(&data.window_id)
            .unwrap_or_default();
        self.push_item(
            ClosedItem::Window {
                data: Box::new(data),
                ssh_sessions,
            },
            ctx,
        );
    }

    /// The pane groups of tabs closed from `workspace_id` that are still on this stack.
    ///
    /// A closing window needs these because `close_tab` removes a tab from `Workspace::tabs`
    /// before handing it here, so `tab_views()` no longer lists it while its panes — detached
    /// `HiddenForClose` — still hold their sessions. Once the window is gone those panes are
    /// unreachable from anywhere: the tab entry's own discard runs `clean_up_pane_group`, which
    /// early-returns on a closed window.
    pub fn stacked_tab_pane_groups(&self, workspace_id: EntityId) -> Vec<ViewHandle<PaneGroup>> {
        self.stack
            .iter()
            .filter_map(|item| match &item.closed_item {
                ClosedItem::Tab {
                    workspace, data, ..
                } if workspace.id() == workspace_id => Some(data.pane_group.clone()),
                _ => None,
            })
            .collect()
    }

    /// Releases the sessions staged for `window_id` and forgets them, for a caller that reused the
    /// window-close path without closing a window.
    ///
    /// `Workspace::on_log_out` is the only such caller. Its panes are destroyed immediately
    /// afterwards and never see a `Closed` detach, so releasing here is both correct and the only
    /// thing that ever will; leaving the entry staged would orphan it under a window that is still
    /// open, where a later real close would merge it into that window's own sessions.
    pub fn release_staged_window_ssh_sessions(
        &mut self,
        window_id: WindowId,
        ctx: &mut ModelContext<Self>,
    ) {
        let ssh_sessions = self
            .staged_window_ssh_sessions
            .remove(&window_id)
            .unwrap_or_default();
        release_ssh_sessions(ssh_sessions, ctx);
    }

    /// The sessions staged for `window_id`, for tests that need to observe a capture on its own,
    /// without a close or a discard following it.
    #[cfg(test)]
    pub(crate) fn staged_window_ssh_sessions_for_test(
        &self,
        window_id: WindowId,
    ) -> Vec<SessionId> {
        self.staged_window_ssh_sessions
            .get(&window_id)
            .cloned()
            .unwrap_or_default()
    }

    /// Records the SSH wrapper sessions of a window that is closing, for the
    /// `handle_window_closed` that names the same window.
    ///
    /// Extends rather than replaces, so several stagers for one window merge. An entry is
    /// orphaned when no close follows — application termination, where `on_window_will_close`
    /// returns early, and `Workspace::on_log_out`, which reuses this path without closing a
    /// window. Both are bounded: an orphan is one `Vec<SessionId>` held until the window really
    /// closes or the process exits.
    pub fn stage_window_ssh_sessions(&mut self, window_id: WindowId, ssh_sessions: Vec<SessionId>) {
        if ssh_sessions.is_empty() {
            return;
        }
        self.staged_window_ssh_sessions
            .entry(window_id)
            .or_default()
            .extend(ssh_sessions);
    }

    /// Handles a tab being closed, adding the necessary data to the undo
    /// stack.
    pub fn handle_tab_closed(
        &mut self,
        workspace: WeakViewHandle<Workspace>,
        tab_index: usize,
        data: TabData,
        ctx: &mut ModelContext<Self>,
    ) {
        self.push_item(
            ClosedItem::Tab {
                workspace,
                tab_index,
                data,
            },
            ctx,
        );
    }

    /// Handles a pane being closed, adding the necessary data to the undo stack.
    pub fn handle_pane_closed_by_id(
        &mut self,
        pane_group: WeakViewHandle<PaneGroup>,
        pane_id: PaneId,
        ctx: &mut ModelContext<Self>,
    ) {
        let pane_data = PaneData {
            pane_id,
            pane_group,
        };

        self.push_item(ClosedItem::Pane { data: pane_data }, ctx);
    }

    /// Undoes the last close action in the stack, if possible.
    pub fn undo_close(&mut self, ctx: &mut AppContext) {
        let Some(UndoData { closed_item, .. }) = self.stack.pop() else {
            return;
        };

        match closed_item {
            ClosedItem::Window { data, ssh_sessions } => {
                send_telemetry_from_app_ctx!(
                    TelemetryEvent::UndoClose {
                        item_type: UndoCloseItemType::Window,
                    },
                    ctx
                );

                let window_id = data.window_id;
                ctx.reopen_closed_window(*data);

                // A restore counts only if it produced both a platform window and a live
                // workspace, because neither implies the other and either shape alone leaves
                // these panes unreachable. `reopen_closed_window` returns before touching any
                // state when the cached bounds are absent, leaving no workspace; and it
                // reinstates the cached window and views before it inspects whether the platform
                // window actually opened, so a platform failure leaves a resolvable workspace
                // behind a window that does not exist. Anything short of both means the window is
                // gone for good with nothing left to release these itself.
                let has_platform_window = ctx.windows().platform_window(window_id).is_some();
                match window_workspace(window_id, ctx) {
                    Some(workspace) if has_platform_window => {
                        workspace.update(ctx, |workspace, ctx| {
                            workspace.handle_reopen(ctx);
                        });
                    }
                    _ => release_ssh_sessions(ssh_sessions, ctx),
                }

                // Make sure we update our session restoration state now that the
                // window has been reopened.
                ctx.dispatch_global_action("workspace:save_app", &());
            }
            ClosedItem::Tab {
                workspace,
                tab_index,
                data,
            } => {
                if let Some(workspace) = workspace.upgrade(ctx) {
                    send_telemetry_from_app_ctx!(
                        TelemetryEvent::UndoClose {
                            item_type: UndoCloseItemType::Tab,
                        },
                        ctx
                    );
                    workspace.update(ctx, |workspace, ctx| {
                        workspace.restore_closed_tab(tab_index, data, ctx);
                    });
                    ctx.windows()
                        .show_window_and_focus_app(workspace.window_id(ctx));
                }
                // Make sure we update our session restoration state now that the
                // tab has been reopened.
                ctx.dispatch_global_action("workspace:save_app", &());
            }
            ClosedItem::Pane { data } => {
                if let Some(pane_group) = data.pane_group.upgrade(ctx) {
                    let pane_id = data.pane_id;
                    let window_id = pane_group.window_id(ctx);
                    let pane_group_id = pane_group.id();
                    let restored = pane_group.update(ctx, |pane_group, ctx| {
                        pane_group.restore_closed_pane(pane_id, ctx)
                    });

                    if restored {
                        send_telemetry_from_app_ctx!(
                            TelemetryEvent::UndoClose {
                                item_type: UndoCloseItemType::Pane,
                            },
                            ctx
                        );

                        // Focus the window first
                        ctx.windows().show_window_and_focus_app(window_id);

                        // Now properly focus the restored pane by activating its tab and focusing the pane
                        if let Some(workspace) = window_workspace(window_id, ctx) {
                            workspace.update(ctx, |workspace, ctx| {
                                let locator = crate::workspace::PaneViewLocator {
                                    pane_group_id,
                                    pane_id,
                                };
                                workspace.focus_pane(locator, ctx);
                            });
                        }

                        ctx.dispatch_global_action("workspace:save_app", &());
                    }
                }
            }
        }
    }

    /// Handles a change to the undo close settings.
    fn handle_settings_event(
        &mut self,
        event: &UndoCloseSettingsChangedEvent,
        ctx: &mut ModelContext<Self>,
    ) {
        match event {
            UndoCloseSettingsChangedEvent::UndoCloseEnabled { .. } => {
                let settings = UndoCloseSettings::as_ref(ctx);
                if !*settings.enabled {
                    for undo_data in self.stack.drain(..) {
                        undo_data.closed_item.discard(ctx);
                    }
                }
            }
            UndoCloseSettingsChangedEvent::UndoCloseGracePeriod { .. } => {}
        }
    }

    /// Pushes a new item onto the stack.
    fn push_item(&mut self, closed_item: ClosedItem, ctx: &mut ModelContext<Self>) {
        let settings = UndoCloseSettings::as_ref(ctx);
        if !*settings.enabled {
            closed_item.discard(ctx);
            return;
        }

        let id = ItemId::new();
        let grace_period = *settings.grace_period;
        let task_handle = ctx.spawn_abortable(
            warpui::r#async::Timer::after(grace_period),
            move |me, _, ctx| {
                let initial_len = me.stack.len();
                if let Some(pos) = me.stack.iter().position(|item| item.expiry_data.id == id) {
                    let removed_item = me.stack.remove(pos);
                    removed_item.closed_item.discard(ctx);
                }
                // Log errors if the expired item was not found or multiple items were found
                if me.stack.len() == initial_len {
                    report_error!("Undo close expiry task did not find item in stack!");
                } else if me.stack.len() < initial_len - 1 {
                    report_error!("Undo close expiry task found multiple matching items in stack!");
                } else {
                    log::debug!("Removed expired item from undo stack");
                }
            },
            |_, _| {},
        );

        self.stack.push(UndoData {
            closed_item,
            expiry_data: ExpiryData { id, task_handle },
        })
    }
}

/// Find the root [`Workspace`] view for a window.
fn window_workspace(window_id: WindowId, ctx: &mut AppContext) -> Option<ViewHandle<Workspace>> {
    ctx.views_of_type::<Workspace>(window_id)
        .and_then(|views| views.first().cloned())
}

impl Entity for UndoCloseStack {
    type Event = UndoCloseStackEvent;
}

impl SingletonEntity for UndoCloseStack {}

#[cfg(test)]
#[path = "stack_tests.rs"]
mod tests;
