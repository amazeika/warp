use super::*;

#[test]
fn a_stated_preference_decides_the_viewer_on_its_own() {
    // The preference is the only thing the protocol lets a plugin say about
    // presentation, so it has to hold against both the file's own type and the
    // user's default.
    assert!(wants_markdown(
        Some(PreferredView::Markdown),
        false, /* is_markdown */
        false, /* prefer_markdown_viewer */
    ));
    assert!(!wants_markdown(Some(PreferredView::Editor), true, true));
}

#[test]
fn with_nothing_stated_a_markdown_file_follows_the_users_own_setting() {
    assert!(wants_markdown(None, true, true));
    assert!(
        !wants_markdown(None, true, false),
        "a user who reads markdown as source keeps reading it as source"
    );
}

#[test]
fn a_file_that_is_not_markdown_never_reaches_the_markdown_viewer_by_default() {
    assert!(!wants_markdown(None, false, true));
}

#[test]
fn a_line_crosses_unshifted_and_carries_its_column() {
    assert_eq!(
        line_and_column(Some(12), Some(4)),
        Some(LineAndColumnArg {
            line_num: 12,
            column_num: Some(4),
        }),
        "both sides count from one, so neither is adjusted on the way through"
    );
    assert_eq!(
        line_and_column(Some(12), None),
        Some(LineAndColumnArg {
            line_num: 12,
            column_num: None,
        })
    );
}

#[test]
fn a_column_with_no_line_points_at_nothing_and_is_dropped() {
    assert_eq!(
        line_and_column(None, Some(4)),
        None,
        "landing on line 1 would put the user somewhere the plugin never named"
    );
    assert_eq!(line_and_column(None, None), None);
}
