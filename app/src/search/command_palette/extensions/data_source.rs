use fuzzy_match::{FuzzyMatchResult, match_indices_case_insensitive};
use warpui::{AppContext, Entity, SingletonEntity};

use super::ExtensionCommandSearchItem;
use crate::extensions::{ContributedCommand, ExtensionManager};
use crate::search::command_palette::mixer::CommandPaletteItemAction;
use crate::search::data_source::{Query, QueryResult};
use crate::search::mixer::{DataSourceRunErrorWrapper, SyncDataSource};

/// Commands contributed by the extensions that are running right now.
///
/// The source holds no state of its own: the contribution registry is the only
/// record of what exists, and reading it per query is what makes an entry
/// disappear the moment its extension stops rather than at the next palette
/// open.
pub struct ExtensionCommandDataSource;

impl ExtensionCommandDataSource {
    pub fn new() -> Self {
        Self
    }

    /// Rebuilds the result for one command, for the palette's recent section.
    ///
    /// Returns `None` when the extension is not running, which is the same
    /// answer the query gives: a remembered entry must not invoke a plugin that
    /// is no longer there to answer.
    pub fn query_result(
        extension_id: &str,
        command_id: &str,
        app: &AppContext,
    ) -> Option<QueryResult<CommandPaletteItemAction>> {
        let command = contributions(app)?.find(|command| {
            command.extension_id == extension_id && command.command_id == command_id
        })?;
        Some(QueryResult::from(ExtensionCommandSearchItem::new(
            command,
            FuzzyMatchResult::no_match(),
        )))
    }
}

impl Default for ExtensionCommandDataSource {
    fn default() -> Self {
        Self::new()
    }
}

impl Entity for ExtensionCommandDataSource {
    type Event = ();
}

impl SyncDataSource for ExtensionCommandDataSource {
    type Action = CommandPaletteItemAction;

    fn run_query(
        &self,
        query: &Query,
        app: &AppContext,
    ) -> Result<Vec<QueryResult<Self::Action>>, DataSourceRunErrorWrapper> {
        let Some(commands) = contributions(app) else {
            return Ok(Vec::new());
        };
        let query_text = query.text.as_str();

        Ok(commands
            .filter_map(|command| {
                let match_result = if query_text.is_empty() {
                    Some(FuzzyMatchResult::no_match())
                } else {
                    match_indices_case_insensitive(command.title.as_str(), query_text)
                }?;
                Some(QueryResult::from(ExtensionCommandSearchItem::new(
                    command,
                    match_result,
                )))
            })
            .collect())
    }
}

/// Every contributed command, or `None` when this build is not running
/// extensions at all — the manager is only registered when the feature is on,
/// and asking for a singleton that was never registered panics.
fn contributions(app: &AppContext) -> Option<impl Iterator<Item = ContributedCommand> + use<>> {
    if !app.has_singleton_model::<ExtensionManager>() {
        return None;
    }
    let commands: Vec<ContributedCommand> =
        ExtensionManager::as_ref(app).commands().cloned().collect();
    Some(commands.into_iter())
}

#[cfg(test)]
#[path = "data_source_tests.rs"]
mod tests;
