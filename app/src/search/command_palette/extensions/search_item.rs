use fuzzy_match::FuzzyMatchResult;
use ordered_float::OrderedFloat;
use warp_core::ui::theme::Fill;
use warpui::elements::{
    Align, ConstrainedBox, Container, Flex, Highlight, ParentElement, Shrinkable, Text,
};
use warpui::fonts::{Properties, Weight};
use warpui::{AppContext, Element, SingletonEntity};

use crate::appearance::Appearance;
use crate::extensions::ContributedCommand;
use crate::search::action::search_item::styles;
use crate::search::command_palette::mixer::CommandPaletteItemAction;
use crate::search::command_palette::render_util;
use crate::search::item::SearchItem;
use crate::search::result_renderer::ItemHighlightState;
use crate::ui_components::icons::Icon;

/// One palette row for a command an extension declared.
#[derive(Debug)]
pub struct ExtensionCommandSearchItem {
    command: ContributedCommand,
    match_result: FuzzyMatchResult,
}

impl ExtensionCommandSearchItem {
    pub fn new(command: ContributedCommand, match_result: FuzzyMatchResult) -> Self {
        Self {
            command,
            match_result,
        }
    }

    /// The extension's name, shown next to the title so a palette entry a user
    /// does not recognise names what put it there. Two extensions may
    /// legitimately contribute commands with the same title.
    fn render_source(
        &self,
        item_highlight_state: ItemHighlightState,
        appearance: &Appearance,
    ) -> Box<dyn Element> {
        Container::new(
            Text::new_inline(
                self.command.extension_name.clone(),
                appearance.ui_font_family(),
                appearance.monospace_font_size(),
            )
            .with_color(item_highlight_state.sub_text_fill(appearance).into_solid())
            .finish(),
        )
        .with_margin_left(8.)
        .finish()
    }

    fn render_title(
        &self,
        item_highlight_state: ItemHighlightState,
        appearance: &Appearance,
    ) -> Box<dyn Element> {
        Text::new_inline(
            self.command.title.clone(),
            appearance.ui_font_family(),
            appearance.monospace_font_size(),
        )
        .with_color(item_highlight_state.main_text_fill(appearance).into_solid())
        .with_single_highlight(
            Highlight::new()
                .with_properties(Properties::default().weight(Weight::Bold))
                .with_foreground_color(
                    item_highlight_state.main_text_fill(appearance).into_solid(),
                ),
            self.match_result.matched_indices.clone(),
        )
        .finish()
    }
}

impl SearchItem for ExtensionCommandSearchItem {
    type Action = CommandPaletteItemAction;

    fn render_icon(
        &self,
        highlight_state: ItemHighlightState,
        appearance: &Appearance,
    ) -> Box<dyn Element> {
        let icon_color: Fill = appearance.theme().terminal_colors().normal.magenta.into();
        render_util::render_search_item_icon(
            appearance,
            Icon::Tool,
            icon_color.into_solid(),
            highlight_state,
        )
    }

    fn render_item(
        &self,
        highlight_state: ItemHighlightState,
        app: &AppContext,
    ) -> Box<dyn Element> {
        let appearance = Appearance::as_ref(app);
        let row = Flex::row()
            .with_child(
                Shrinkable::new(
                    1.,
                    Align::new(self.render_title(highlight_state, appearance))
                        .left()
                        .finish(),
                )
                .finish(),
            )
            .with_child(
                Align::new(self.render_source(highlight_state, appearance))
                    .left()
                    .finish(),
            )
            .finish();

        ConstrainedBox::new(row)
            .with_height(styles::SEARCH_ITEM_HEIGHT)
            .finish()
    }

    fn score(&self) -> OrderedFloat<f64> {
        OrderedFloat(self.match_result.score as f64)
    }

    fn accept_result(&self) -> CommandPaletteItemAction {
        CommandPaletteItemAction::InvokeExtensionCommand {
            extension_id: self.command.extension_id.clone(),
            command_id: self.command.command_id.clone(),
        }
    }

    fn execute_result(&self) -> CommandPaletteItemAction {
        self.accept_result()
    }

    fn accessibility_label(&self) -> String {
        format!(
            "{}, from the {} extension.",
            self.command.title, self.command.extension_name
        )
    }
}
