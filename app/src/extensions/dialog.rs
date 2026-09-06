//! The modal question Warp asks on an extension's behalf.
//!
//! Two things need one: the permission prompt, which decides whether a plugin
//! runs at all, and `dialog.confirm`, which a plugin uses before an action it
//! cannot undo. Both are the same shape — a title, an explanation, and two
//! answers — so both go through here, and Warp owns the presentation in each
//! case. A plugin never draws its own confirmation.
#[cfg(not(test))]
use std::rc::Rc;

use warpui::AppContext;
#[cfg(not(test))]
use warpui::modals::{AlertDialogWithCallbacks, ModalButton};

#[cfg(not(test))]
use crate::workspace::Workspace;

/// One question with exactly two answers.
pub(super) struct Question {
    pub title: String,
    /// The smaller explanatory text: what is being asked for, in full.
    pub body: String,
    pub confirm_label: String,
    pub cancel_label: String,
}

/// Puts `question` in front of the user and calls `answer` with their choice.
///
/// Returns false when there is nowhere to ask, in which case `answer` is never
/// called and the caller decides what an unaskable question means.
///
/// A window is required on every platform, including the one whose alert could
/// technically be raised without one. Extensions are discovered while Warp is
/// still starting, and a modal that interrupts before the workspace has drawn
/// would make startup wait on a question about something the user has not seen
/// yet — which is exactly what extensions are not allowed to cost.
#[cfg(not(test))]
pub(super) fn show(
    question: &Question,
    answer: impl Fn(bool, &mut AppContext) + 'static,
    ctx: &mut AppContext,
) -> bool {
    // The two buttons share one answer callback, and exactly one of them runs.
    let answer = Rc::new(answer);
    let confirm = {
        let answer = Rc::clone(&answer);
        ModalButton::for_app(question.confirm_label.clone(), move |ctx| answer(true, ctx))
    };
    let cancel = {
        let answer = Rc::clone(&answer);
        ModalButton::for_app(question.cancel_label.clone(), move |ctx| answer(false, ctx))
    };
    // Cancel is last because that is the button escape triggers, and first is
    // the one enter triggers. Refusing has to be the easier of the two.
    let dialog = AlertDialogWithCallbacks::for_app(
        question.title.clone(),
        question.body.clone(),
        vec![confirm, cancel],
        // The "don't show again" affordance the modal offers has no meaning
        // here: declining already stops Warp asking about this extension until
        // the user comes back to it.
        |_| {},
    );

    let Some(window_id) = ctx.windows().active_window() else {
        return false;
    };

    if cfg!(all(not(target_family = "wasm"), target_os = "macos")) {
        ctx.show_native_platform_modal(dialog);
        return true;
    }

    // Everywhere else the platform has no modal of its own, so the question is
    // drawn by the workspace itself.
    let Some(workspace) = ctx
        .views_of_type::<Workspace>(window_id)
        .and_then(|workspaces| workspaces.first().cloned())
    else {
        return false;
    };
    workspace.update(ctx, |workspace, ctx| {
        workspace.show_native_modal(dialog, ctx);
    });
    true
}

/// Under test there is no window and no platform modal loop, so a question is
/// reported as shown and left on screen forever. That is what lets a test play
/// the click through the manager's own answer path, which is the behaviour
/// worth pinning; the presentation above is covered by running Warp.
#[cfg(test)]
pub(super) fn show(
    question: &Question,
    answer: impl Fn(bool, &mut AppContext) + 'static,
    ctx: &mut AppContext,
) -> bool {
    let _ = (question, answer, ctx);
    true
}
