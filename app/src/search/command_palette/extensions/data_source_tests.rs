use warpui::App;

use super::*;
use crate::search::data_source::Query;

#[test]
fn a_build_without_extensions_contributes_nothing_rather_than_failing() {
    App::test((), |app| async move {
        let source = ExtensionCommandDataSource::new();
        app.read(|ctx| {
            let results = source
                .run_query(&Query::from("do the thing"), ctx)
                .expect("querying is never an error");
            assert!(
                results.is_empty(),
                "the palette runs in builds that never registered an extension manager, and \
                 asking for a singleton that does not exist panics"
            );
            assert!(
                ExtensionCommandDataSource::query_result("dev.warp.git", "do.thing", ctx).is_none()
            );
        });
    });
}
