pub mod code_review_view;
pub mod comment_list_view;
pub mod context;
pub mod diff_size_limits;
#[cfg_attr(not(feature = "local_fs"), allow(dead_code))]
pub mod diff_state;
pub mod editor_state;
pub(crate) mod find_model;
pub(crate) mod git_actions;
pub(crate) mod git_dialog;
pub mod git_repo_model;
mod git_repo_models;
pub mod github_repo_model;
mod hidden_lines;
pub mod telemetry_event;
#[cfg_attr(not(feature = "local_fs"), allow(unused_imports))]
pub use telemetry_event::CodeReviewTelemetryEvent;

pub(crate) mod code_review_header;
pub(crate) mod comment_rendering;
pub mod comments;
pub(crate) mod diff_menu;
pub(crate) mod diff_selector;
#[cfg_attr(not(feature = "local_fs"), allow(dead_code))]
pub(crate) mod file_invalidation_queue;

use code_review_view::CodeReviewAction;
use warp_core::SessionId;
use warpui::keymap::{EditableBinding, FixedBinding};
use warpui::{
    AppContext, Entity, EntityId, ModelContext, SingletonEntity, WeakViewHandle, WindowId, id,
};

use crate::BlocklistAIHistoryModel;
use crate::ai::agent::conversation::AIConversationId;
use crate::code::buffer_location::LocalOrRemotePath;
use crate::code_review::telemetry_event::CodeReviewPaneEntrypoint;
use crate::terminal::CLIAgent;
use crate::terminal::view::TerminalView;
use crate::util::bindings::CustomAction;

/// Where a request to open the review panel came from.
///
/// The panel needs two things that a terminal pane happens to hold: the
/// session the diff should load over, and the conversation to mark as having
/// had a review opened. Naming the origin rather than the pane is what lets
/// something outside the pane tree — the extension API — ask for a diff at
/// all, and it keeps the two questions separable: an origin can answer the
/// first without having an answer to the second.
#[derive(Clone)]
pub enum CodeReviewOrigin {
    /// The pane the user was in when they asked.
    Terminal(WeakViewHandle<TerminalView>),
    /// A caller with no pane of its own. The session to load the diff over is
    /// named outright rather than read off whichever view happens to be
    /// focused, so a diff of a remote repository cannot be loaded over a local
    /// session that shares its path.
    Detached { session_id: Option<SessionId> },
}

impl CodeReviewOrigin {
    /// The session the diff should be loaded over, if there is one.
    pub fn preferred_session(&self, ctx: &AppContext) -> Option<SessionId> {
        match self {
            Self::Terminal(view) => view
                .upgrade(ctx)
                .and_then(|view| view.as_ref(ctx).active_block_session_id()),
            Self::Detached { session_id } => *session_id,
        }
    }

    /// The conversation that should record that a review was opened.
    ///
    /// Only a terminal origin has one: the record exists to tell an agent
    /// conversation that its changes were looked at, and an origin with no
    /// conversation behind it has nothing to say about one.
    pub fn conversation_id(&self, ctx: &AppContext) -> Option<AIConversationId> {
        let Self::Terminal(view) = self else {
            return None;
        };
        let view = view.upgrade(ctx)?;
        BlocklistAIHistoryModel::as_ref(ctx).active_conversation_id(view.id())
    }
}

/// Arguments needed to open or toggle the code review panel.
/// Bundled into a struct so that events can atomically open the
/// review and perform follow-up work without relying on event ordering.
#[derive(Clone)]
pub struct CodeReviewPanelArg {
    pub repo_path: Option<LocalOrRemotePath>,
    pub origin: CodeReviewOrigin,
    pub entrypoint: CodeReviewPaneEntrypoint,
    pub focus_new_pane: bool,
    pub cli_agent: Option<CLIAgent>,
}

/// Scope for diff set context attachment
#[derive(Clone, Debug, PartialEq)]
pub enum DiffSetScope {
    All,
    /// A single repo-relative file path in the diff set.
    File(String),
}

/// The keystroke that submits in the code review panel. Meant to mirror the keystroke for
/// [`EditorViewEvent::CmdEnter`].
pub const CODE_REVIEW_SUBMIT_KEYSTROKE: &str = "cmdorctrl-enter";

/// Register keybindings for code review functionality.
pub fn init(app: &mut AppContext) {
    app.register_editable_bindings([
        EditableBinding::new(
            "code_review:save_all_unsaved_files",
            "Save all unsaved files in code review",
            CodeReviewAction::SaveAllUnsavedFiles,
        )
        .with_context_predicate(id!("CodeReviewView"))
        .with_key_binding("cmdorctrl-s"),
        EditableBinding::new(
            "code_review:show_find_bar",
            "Show find bar in code review",
            CodeReviewAction::ShowFindBar,
        )
        .with_context_predicate(id!("CodeReviewView"))
        .with_key_binding("cmdorctrl-f")
        .with_enabled(|| crate::features::FeatureFlag::CodeReviewFind.is_enabled()),
        EditableBinding::new(
            "code_review:toggle_file_navigation",
            "Toggle file navigation in code review",
            CodeReviewAction::ToggleFileSidebar,
        )
        .with_context_predicate(id!("CodeReviewView_NotEditing"))
        .with_key_binding("f")
        .with_enabled(|| crate::features::FeatureFlag::GitOperationsInCodeReview.is_enabled()),
    ]);

    app.register_fixed_bindings([
        FixedBinding::custom(
            CustomAction::Undo,
            CodeReviewAction::UndoRevert,
            "Undo",
            id!("CodeReviewView") & !id!("IMEOpen"),
        ),
        FixedBinding::new(
            CODE_REVIEW_SUBMIT_KEYSTROKE,
            CodeReviewAction::SubmitReviewComments,
            id!("CodeReviewView_NotEditing"),
        )
        .with_command_description("Send code review comments to agent"),
    ]);

    diff_menu::init(app);
    diff_selector::init(app);
    git_dialog::init(app);
}

/// Uses heuristics to determine if a file is auto-generated.
///
/// `file_path` is expected to be a repo-relative path (as a string),
/// matching the way file paths are stored on `FileDiff`.
fn is_file_autogenerated(file_path: &str, content: Option<&str>) -> bool {
    const AUTOGEN_HEADERS: [&str; 3] = [
        "Code generated by",
        "This file is automatically generated",
        "AUTO-GENERATED FILE",
    ];

    let file_name = file_path.rsplit('/').next().unwrap_or("");

    // Check for specific lock files and autogenerated files by exact name
    match file_name {
        // Package manager lock files
        "Cargo.lock" | "package-lock.json" | "yarn.lock" | "pnpm-lock.yaml" | "Gemfile.lock"
        | "composer.lock" | "Pipfile.lock" | "poetry.lock" | "go.sum" | "mix.lock" => return true,
        // Log files
        name if name.ends_with(".log") => return true,
        _ => {}
    }

    // Check for file path hints.
    if file_name.contains(".generated.")
        || file_name.contains(".gen.")
        || file_name.ends_with(".min.js")
        || file_name.ends_with(".min.css")
        || file_name.contains(".bundle.")
    {
        return true;
    }

    // Check for directory structure hints.
    if file_path.contains("__generated__/")
        || file_path.contains(".auto/")
        || file_path.contains("codegen/")
    {
        return true;
    }

    // Check the first line of the modified file for autogeneration headers.
    // We don't check any actual diffs because the user should probably inspect
    // auto-generated files if they are created for the first time.
    if let Some(content) = content
        && let Some(first_line) = content.lines().next()
        && AUTOGEN_HEADERS
            .iter()
            .any(|header| first_line.contains(header))
    {
        return true;
    }

    false
}

/// A [`SingletonEntity`] that the tracks events for the code review model throughought the app.
/// We need this because toasts are emitted in the Workspace, and want a click handler that triggers
/// behavior in a _specific_ review pane. We use this model get around restrictions that make it hard
/// to emit a CodeReviewView typed action from the toast because it's not in the view responder chain of the
/// Workspace.
pub struct GlobalCodeReviewModel;

impl GlobalCodeReviewModel {
    pub fn undo_revert_in_code_review_pane(
        &mut self,
        window_id: WindowId,
        view_id: EntityId,
        ctx: &mut ModelContext<Self>,
    ) {
        ctx.emit(GlobalCodeReviewEvent::DiffReverted { window_id, view_id });
    }
}

pub enum GlobalCodeReviewEvent {
    DiffReverted {
        window_id: WindowId,
        view_id: EntityId,
    },
}

impl SingletonEntity for GlobalCodeReviewModel {}

impl Entity for GlobalCodeReviewModel {
    type Event = GlobalCodeReviewEvent;
}
