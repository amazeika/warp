//! Opening a file a plugin pointed Warp at.
//!
//! The whole of the interesting work is binding the path to a host before
//! anything reads it. A plugin acting on an SSH session names paths on that
//! host, and every one of them may also exist on this machine: `/srv/app/.env`
//! is a real file in both places. The request's execution target is what
//! decides which, and it is not advisory — a target Warp cannot place is a
//! refusal, never a local file with the same name.
//!
//! Reaching the viewer is covered by running Warp. What is tested here is the
//! decision: which of Warp's viewers renders the file, and what a line and
//! column that name nothing turn into.
use extension_protocol::{ExecutionTarget, ExtensionError, FileOpenParams, PreferredView};
use warp_core::SessionId;
use warp_util::file_type::is_markdown_file;
use warp_util::local_or_remote_path::LocalOrRemotePath;
use warp_util::path::LineAndColumnArg;
use warpui::{AppContext, SingletonEntity as _};

use super::context::{active_workspace, resolve_path};
use super::execution::parse_session_id;
use crate::util::file::external_editor::EditorSettings;

/// What a `file.open` resolves to before there is a pane to show it in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct FileRequest {
    pub location: LocalOrRemotePath,
    /// The session a remote file is read over. `None` is the local machine.
    pub session_id: Option<SessionId>,
    pub line_col: Option<LineAndColumnArg>,
    /// Whether the file goes to the markdown viewer rather than the editor.
    pub markdown: bool,
}

pub(super) struct FileService;

impl FileService {
    pub(super) fn resolve(
        params: &FileOpenParams,
        ctx: &AppContext,
    ) -> Result<FileRequest, ExtensionError> {
        let location = resolve_path(&params.target, &params.path, ctx)?;
        let session_id = match &params.target {
            ExecutionTarget::Local => None,
            ExecutionTarget::Ssh { session_id } => Some(parse_session_id(session_id)?),
        };
        // Matched on the path's own text rather than on anything read from
        // disk, so a remote file is classified without a round trip to its
        // host — and classified the same way the file tree classifies it.
        let is_markdown = is_markdown_file(location.display_path());
        let prefer_markdown_viewer = *EditorSettings::as_ref(ctx).prefer_markdown_viewer;
        Ok(FileRequest {
            location,
            session_id,
            line_col: line_and_column(params.line, params.column),
            markdown: wants_markdown(params.preferred_view, is_markdown, prefer_markdown_viewer),
        })
    }
}

impl FileRequest {
    /// Opens the file in the workspace the user is looking at.
    pub(super) fn show(
        self,
        extension_id: &str,
        ctx: &mut AppContext,
    ) -> Result<(), ExtensionError> {
        let workspace = active_workspace(ctx)?;
        workspace.update(ctx, |workspace, ctx| {
            workspace.open_file_for_extension(
                extension_id.to_owned(),
                self.location,
                self.line_col,
                self.session_id,
                self.markdown,
                ctx,
            );
        });
        Ok(())
    }
}

/// Which of Warp's own viewers renders the file.
///
/// A stated preference wins outright — it is the only thing the protocol lets
/// a plugin say about presentation, so ignoring it would leave it meaningless.
/// With nothing stated the file is opened the way the user's own settings would
/// open it from the file tree, which is the behaviour they have already chosen
/// for markdown; a plugin should not be the reason a Markdown file suddenly
/// renders as source.
fn wants_markdown(
    preferred: Option<PreferredView>,
    is_markdown: bool,
    prefer_markdown_viewer: bool,
) -> bool {
    match preferred {
        Some(PreferredView::Markdown) => true,
        Some(PreferredView::Editor) => false,
        None => is_markdown && prefer_markdown_viewer,
    }
}

/// The position to jump to, if the request named one.
///
/// A column without a line is dropped rather than guessed at: there is no line
/// for it to be a column of, and landing on line 1 would put the user somewhere
/// the plugin never pointed. Line and column are 1-based on both sides, so
/// neither is shifted on the way through.
fn line_and_column(line: Option<u32>, column: Option<u32>) -> Option<LineAndColumnArg> {
    Some(LineAndColumnArg {
        line_num: line? as usize,
        column_num: column.map(|column| column as usize),
    })
}

#[cfg(test)]
#[path = "files_tests.rs"]
mod tests;
