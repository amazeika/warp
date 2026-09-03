//! Settings-search coverage for the Warpify page's SSH widgets.
//!
//! A widget is the smallest unit `PageType::update_filter` can keep or drop, so its `search_terms`
//! are the whole of what makes its rows reachable. These pin the queries a user actually types.

use super::super::settings_page::{SettingsWidget, search_terms_match};
use super::{SSHWidget, SshCloneOnSplitWidget};

#[test]
fn clone_on_split_is_reachable_by_what_it_does() {
    let widget = SshCloneOnSplitWidget::default();

    for query in [
        "split",
        // The word the rendered row actually uses. Search never reads the label, so a term that
        // does not contain this leaves the row unreachable by its own name.
        "splitting",
        "splitting a pane",
        "ssh split",
        "split pane",
        "reuse connection",
        "controlmaster",
        "remote",
    ] {
        assert!(
            search_terms_match(widget.search_terms(), query),
            "settings search for {query:?} must reach the clone-on-split setting"
        );
    }
}

/// Every word of the query has to appear, so an unrelated query must not drag the widget in.
#[test]
fn clone_on_split_is_not_reachable_by_an_unrelated_query() {
    let widget = SshCloneOnSplitWidget::default();

    for query in ["theme", "keybindings", "ssh keyboard"] {
        assert!(
            !search_terms_match(widget.search_terms(), query),
            "settings search for {query:?} must not reach the clone-on-split setting"
        );
    }
}

/// The two SSH widgets are separately filterable, and a query aimed at splitting must not also
/// pull in the unrelated ControlMaster row — that is the whole reason this setting got its own
/// widget rather than a row inside `SSHWidget`.
#[test]
fn splitting_queries_do_not_match_the_other_ssh_widget() {
    let ssh_widget = SSHWidget::default();

    assert!(!search_terms_match(ssh_widget.search_terms(), "split"));
}
