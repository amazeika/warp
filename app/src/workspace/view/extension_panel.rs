//! Renders the panel snapshot an extension published, and sends back what the
//! user does with it.
//!
//! The snapshot ([`PanelViewState`]) is structure and labels only — there is no
//! markup and nothing executable in it — so everything on screen is drawn with
//! Warp's own components and an extension panel looks like a built-in one. That
//! is also what keeps a plugin from making a destructive row look ordinary.
//!
//! The view holds no copy of the snapshot. It reads the manager's record when
//! it rebuilds its rows, so there is one record of what a panel shows and no
//! way for the two to disagree.
use std::collections::{HashMap, HashSet};

use extension_protocol::{PanelItem, PanelItemAction, PanelStatus, PanelViewState};
use warp_core::ui::Icon;
use warp_core::ui::theme::Fill as ThemeFill;
use warp_core::ui::theme::color::internal_colors;
use warpui::color::ColorU;
use warpui::elements::{
    Align, ClippedScrollStateHandle, ClippedScrollable, ConstrainedBox, Container,
    CrossAxisAlignment, Element, Fill, Flex, Hoverable, MainAxisSize, MouseStateHandle,
    ParentElement, ScrollbarWidth, Shrinkable, Stack, Text,
};
use warpui::fonts::Weight;
use warpui::platform::Cursor;
use warpui::ui_components::components::{UiComponent, UiComponentStyles};
use warpui::{
    AppContext, Entity, SingletonEntity as _, TypedActionView, View, ViewContext, ViewHandle,
};

use crate::appearance::Appearance;
use crate::extensions::ExtensionManager;
use crate::menu::{Event as MenuEvent, Menu, MenuItem, MenuItemFields};
use crate::ui_components::menu_button::{MenuDirection, icon_button_with_context_menu};

/// Height reserved for one row, so a list of them does not jitter as labels
/// change length.
const ROW_MIN_HEIGHT: f32 = 26.;

/// How far each level of nesting is indented.
const INDENT: f32 = 12.;

const CHEVRON_SIZE: f32 = 12.;
const ICON_SIZE: f32 = 12.;
const ROW_FONT_SIZE: f32 = 12.;
const DESCRIPTION_FONT_SIZE: f32 = 11.;
const SECTION_FONT_SIZE: f32 = 11.;
const BADGE_FONT_SIZE: f32 = 10.;

/// The icons a panel item may ask for.
///
/// Warp owns the icon set: a plugin names an intent from this vocabulary and
/// Warp picks the glyph, so an extension cannot borrow the appearance of a
/// surface it is not. A name outside the vocabulary renders without an icon
/// rather than with a placeholder, because a wrong icon says something false
/// about the row while a missing one says nothing.
fn icon_for(name: &str) -> Option<Icon> {
    Some(match name {
        "added" => Icon::Plus,
        "modified" => Icon::Pencil,
        "deleted" => Icon::Minus,
        "renamed" => Icon::Rename,
        "copied" => Icon::Copy,
        "untracked" => Icon::Circle,
        "conflict" => Icon::AlertTriangle,
        "branch" | "branch-current" => Icon::GitBranch,
        "branch-remote" => Icon::Cloud,
        "commit" => Icon::GitCommit,
        "file" => Icon::File,
        "folder" => Icon::Folder,
        "check" => Icon::Check,
        "info" => Icon::Info,
        "warning" => Icon::Warning,
        "error" => Icon::AlertCircle,
        _ => return None,
    })
}

/// Identifies one row within a panel.
///
/// A section id plus the chain of item ids down to the row: item ids are only
/// unique within their section — the same path legitimately appears staged and
/// unstaged — and a child's id is only unique under its parent, so nothing
/// shorter identifies a row.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct RowKey {
    pub section_id: String,
    /// Empty for a section header.
    pub path: Vec<String>,
}

impl RowKey {
    fn section(section_id: &str) -> Self {
        Self {
            section_id: section_id.to_owned(),
            path: Vec::new(),
        }
    }

    fn child(&self, item_id: &str) -> Self {
        let mut path = self.path.clone();
        path.push(item_id.to_owned());
        Self {
            section_id: self.section_id.clone(),
            path,
        }
    }

    /// The item id the plugin knows this row by, which is what a
    /// `panel.action` carries back.
    fn item_id(&self) -> Option<&str> {
        self.path.last().map(String::as_str)
    }
}

/// One line of the rendered panel.
#[derive(Clone, Debug, PartialEq)]
pub struct PanelRow {
    pub key: RowKey,
    pub kind: RowKind,
}

#[derive(Clone, Debug, PartialEq)]
pub enum RowKind {
    Section {
        title: String,
        badge: Option<String>,
        collapsed: bool,
    },
    Item {
        label: String,
        description: Option<String>,
        icon: Option<String>,
        badge: Option<String>,
        actions: Vec<PanelItemAction>,
        depth: usize,
        /// `Some(expanded)` when the row has children, `None` when it is a leaf.
        expansion: Option<bool>,
    },
}

impl PanelRow {
    /// The action a click on the row runs.
    ///
    /// A row with children expands instead, so its first action stays available
    /// in the overflow menu rather than firing when the user only meant to look
    /// inside. Plugins order the actions with the primary one first, which is
    /// the contract the panel model already implies by rendering them in order.
    pub fn primary_action(&self) -> Option<&PanelItemAction> {
        match &self.kind {
            RowKind::Item {
                actions,
                expansion: None,
                ..
            } => actions.first(),
            _ => None,
        }
    }

    fn actions(&self) -> &[PanelItemAction] {
        match &self.kind {
            RowKind::Item { actions, .. } => actions,
            RowKind::Section { .. } => &[],
        }
    }
}

/// Flattens a snapshot into the rows to draw, honouring what the user has
/// collapsed.
///
/// `overrides` holds only the rows the user has clicked. Everything else keeps
/// the plugin's own `expanded` value, so a panel that wants a section open on
/// first sight gets it without overriding a choice the user already made.
pub fn build_rows(state: &PanelViewState, overrides: &HashMap<RowKey, bool>) -> Vec<PanelRow> {
    let mut rows = Vec::new();
    for section in &state.sections {
        let key = RowKey::section(&section.id);
        // An override records whether the row is open. Sections start open:
        // a panel's first section is usually the point of opening it, and the
        // model has no per-section `expanded` to say otherwise.
        let collapsed = !overrides.get(&key).copied().unwrap_or(true);
        rows.push(PanelRow {
            key: key.clone(),
            kind: RowKind::Section {
                title: section.title.clone(),
                badge: section.badge.clone(),
                collapsed,
            },
        });
        if !collapsed {
            push_items(&key, &section.items, 0, overrides, &mut rows);
        }
    }
    rows
}

fn push_items(
    parent: &RowKey,
    items: &[PanelItem],
    depth: usize,
    overrides: &HashMap<RowKey, bool>,
    rows: &mut Vec<PanelRow>,
) {
    for item in items {
        let key = parent.child(&item.id);
        let has_children = !item.children.is_empty();
        let expanded = overrides
            .get(&key)
            .copied()
            .unwrap_or(item.expanded && has_children);
        rows.push(PanelRow {
            key: key.clone(),
            kind: RowKind::Item {
                label: item.label.clone(),
                description: item.description.clone(),
                icon: item.icon.clone(),
                badge: item.badge.clone(),
                actions: item.actions.clone(),
                depth,
                expansion: has_children.then_some(expanded),
            },
        });
        if has_children && expanded {
            push_items(&key, &item.children, depth + 1, overrides, rows);
        }
    }
}

#[derive(Clone, Debug)]
pub enum ExtensionPanelAction {
    /// The user clicked a row, or picked one of its actions.
    Invoke {
        key: RowKey,
        action_id: String,
    },
    /// The user expanded or collapsed a section or a row with children.
    ToggleExpansion {
        key: RowKey,
    },
    ToggleOverflowMenu {
        key: RowKey,
    },
}

/// One contributed panel, drawn from whatever its extension last published.
pub struct ExtensionPanelView {
    extension_id: String,
    panel_id: String,
    rows: Vec<PanelRow>,
    /// Rows the user has explicitly expanded or collapsed. Absent means the
    /// plugin's own value still stands.
    expansion_overrides: HashMap<RowKey, bool>,
    row_mouse_states: HashMap<RowKey, MouseStateHandle>,
    overflow_button_states: HashMap<RowKey, MouseStateHandle>,
    overflow_menu: ViewHandle<Menu<ExtensionPanelAction>>,
    overflow_menu_open_for: Option<RowKey>,
    scroll_state: ClippedScrollStateHandle,
}

impl ExtensionPanelView {
    pub fn new(extension_id: String, panel_id: String, ctx: &mut ViewContext<Self>) -> Self {
        let overflow_menu = ctx.add_typed_action_view(|_| {
            Menu::new()
                .prevent_interaction_with_other_elements()
                .with_width(180.)
        });
        ctx.subscribe_to_view(&overflow_menu, |me, _, event, ctx| match event {
            MenuEvent::Close { .. } => {
                me.overflow_menu_open_for = None;
                ctx.notify();
            }
            MenuEvent::ItemSelected | MenuEvent::ItemHovered => {}
        });

        // The snapshot lives in the manager, so the panel is rebuilt when the
        // manager says it changed rather than on a timer or on every frame.
        if ctx.has_singleton_model::<ExtensionManager>() {
            let manager = ExtensionManager::handle(&*ctx);
            ctx.subscribe_to_model(&manager, |me, _, event, ctx| {
                if let crate::extensions::ExtensionManagerEvent::PanelStateChanged {
                    extension_id,
                    panel_id,
                } = event
                    && *extension_id == me.extension_id
                    && *panel_id == me.panel_id
                {
                    me.rebuild_rows(ctx);
                }
            });
        }

        let mut view = Self {
            extension_id,
            panel_id,
            rows: Vec::new(),
            expansion_overrides: HashMap::new(),
            row_mouse_states: HashMap::new(),
            overflow_button_states: HashMap::new(),
            overflow_menu,
            overflow_menu_open_for: None,
            scroll_state: ClippedScrollStateHandle::default(),
        };
        view.rebuild_rows(ctx);
        view
    }

    fn status(&self, app: &AppContext) -> Option<PanelStatus> {
        Some(self.state(app)?.status.clone())
    }

    fn state<'a>(&self, app: &'a AppContext) -> Option<&'a PanelViewState> {
        if !app.has_singleton_model::<ExtensionManager>() {
            return None;
        }
        ExtensionManager::as_ref(app).panel_state(&self.extension_id, &self.panel_id)
    }

    fn rebuild_rows(&mut self, ctx: &mut ViewContext<Self>) {
        self.rows = match self.state(ctx) {
            Some(state) => build_rows(state, &self.expansion_overrides),
            None => Vec::new(),
        };
        // Hover state is per row and has to outlive the render that reads it,
        // so each row keeps its handle for as long as it is drawn. A row that
        // is gone cannot be hovered, and dropping its handle is what keeps a
        // long-lived panel from accumulating one for every path it has shown.
        let keys: HashSet<RowKey> = self.rows.iter().map(|row| row.key.clone()).collect();
        self.row_mouse_states.retain(|key, _| keys.contains(key));
        self.overflow_button_states
            .retain(|key, _| keys.contains(key));
        for key in &keys {
            self.row_mouse_states.entry(key.clone()).or_default();
            self.overflow_button_states.entry(key.clone()).or_default();
        }
        if let Some(open_for) = &self.overflow_menu_open_for
            && !keys.contains(open_for)
        {
            self.overflow_menu_open_for = None;
        }
        ctx.notify();
    }

    fn toggle_expansion(&mut self, key: &RowKey, ctx: &mut ViewContext<Self>) {
        let Some(row) = self.rows.iter().find(|row| row.key == *key) else {
            return;
        };
        let currently_open = match &row.kind {
            RowKind::Section { collapsed, .. } => !collapsed,
            RowKind::Item { expansion, .. } => match expansion {
                Some(expanded) => *expanded,
                None => return,
            },
        };
        self.expansion_overrides
            .insert(key.clone(), !currently_open);
        self.rebuild_rows(ctx);
    }

    fn invoke(&mut self, key: &RowKey, action_id: &str, ctx: &mut ViewContext<Self>) {
        let Some(item_id) = key.item_id().map(str::to_owned) else {
            return;
        };
        if !ctx.has_singleton_model::<ExtensionManager>() {
            return;
        }
        let extension_id = self.extension_id.clone();
        let panel_id = self.panel_id.clone();
        let section_id = key.section_id.clone();
        let action_id = action_id.to_owned();
        let manager = ExtensionManager::handle(&*ctx);
        manager.update(ctx, |manager, ctx| {
            manager.invoke_panel_action(
                &extension_id,
                &panel_id,
                &section_id,
                &item_id,
                &action_id,
                ctx,
            );
        });
    }

    fn open_overflow_menu(&mut self, key: &RowKey, ctx: &mut ViewContext<Self>) {
        if self.overflow_menu_open_for.as_ref() == Some(key) {
            self.overflow_menu_open_for = None;
            ctx.notify();
            return;
        }
        let Some(row) = self.rows.iter().find(|row| row.key == *key) else {
            return;
        };
        let danger = Appearance::as_ref(ctx).theme().ansi_fg_red();
        let items: Vec<MenuItem<ExtensionPanelAction>> =
            row.actions()
                .iter()
                .map(|action| {
                    let mut fields = MenuItemFields::new(action.title.clone())
                        .with_on_select_action(ExtensionPanelAction::Invoke {
                            key: key.clone(),
                            action_id: action.id.clone(),
                        });
                    // The plugin says an action is destructive; Warp is what makes
                    // it look destructive, so the warning cannot be left out by an
                    // extension that would rather it were not there.
                    if action.destructive {
                        fields = fields.with_override_text_color(danger);
                    }
                    if let Some(icon) = action.icon.as_deref().and_then(icon_for) {
                        fields = fields.with_icon(icon);
                    }
                    fields.into_item()
                })
                .collect();
        if items.is_empty() {
            return;
        }
        self.overflow_menu.update(ctx, |menu, ctx| {
            menu.set_items(items, ctx);
        });
        self.overflow_menu_open_for = Some(key.clone());
        ctx.notify();
    }
}

impl Entity for ExtensionPanelView {
    type Event = ();
}

impl TypedActionView for ExtensionPanelView {
    type Action = ExtensionPanelAction;

    fn handle_action(&mut self, action: &Self::Action, ctx: &mut ViewContext<Self>) {
        match action {
            ExtensionPanelAction::Invoke { key, action_id } => {
                self.overflow_menu_open_for = None;
                self.invoke(key, action_id, ctx);
            }
            ExtensionPanelAction::ToggleExpansion { key } => self.toggle_expansion(key, ctx),
            ExtensionPanelAction::ToggleOverflowMenu { key } => self.open_overflow_menu(key, ctx),
        }
    }
}

impl View for ExtensionPanelView {
    fn ui_name() -> &'static str {
        "ExtensionPanelView"
    }

    fn render(&self, app: &AppContext) -> Box<dyn Element> {
        let appearance = Appearance::as_ref(app);
        // No snapshot yet is not an error: the extension is running and has not
        // said anything, which is exactly what loading looks like.
        let status = self.status(app).unwrap_or(PanelStatus::Loading);
        match status {
            PanelStatus::Loading => centered_message(appearance, "Loading\u{2026}", None),
            PanelStatus::Empty { message } => centered_message(appearance, &message, None),
            // Worded by the extension but presented by Warp, so a failure
            // cannot be dressed up as ordinary content.
            PanelStatus::Error { message } => {
                centered_message(appearance, "This panel could not load", Some(&message))
            }
            PanelStatus::Ready if self.rows.is_empty() => {
                centered_message(appearance, "Nothing to show", None)
            }
            PanelStatus::Ready => self.render_rows(appearance),
        }
    }
}

impl ExtensionPanelView {
    fn render_rows(&self, appearance: &Appearance) -> Box<dyn Element> {
        let theme = appearance.theme();
        let column = Flex::column()
            .with_main_axis_size(MainAxisSize::Min)
            .with_children(self.rows.iter().map(|row| self.render_row(row, appearance)));

        Container::new(
            ClippedScrollable::vertical(
                self.scroll_state.clone(),
                column.finish(),
                ScrollbarWidth::Auto,
                theme.nonactive_ui_detail().into(),
                theme.active_ui_detail().into(),
                Fill::None,
            )
            .with_overlayed_scrollbar()
            .finish(),
        )
        .with_padding_left(2.)
        .with_padding_right(2.)
        .finish()
    }

    fn render_row(&self, row: &PanelRow, appearance: &Appearance) -> Box<dyn Element> {
        match &row.kind {
            RowKind::Section {
                title,
                badge,
                collapsed,
            } => self.render_section(row, title, badge.as_deref(), *collapsed, appearance),
            RowKind::Item { .. } => self.render_item(row, appearance),
        }
    }

    fn render_section(
        &self,
        row: &PanelRow,
        title: &str,
        badge: Option<&str>,
        collapsed: bool,
        appearance: &Appearance,
    ) -> Box<dyn Element> {
        let theme = appearance.theme();
        let sub_text = internal_colors::text_sub(theme, theme.background().into_solid());
        let title = title.to_owned();
        let badge = badge.map(str::to_owned);
        let key = row.key.clone();
        let click_key = key.clone();

        Hoverable::new(self.mouse_state(&self.row_mouse_states, &key), move |_| {
            let mut line = Flex::row()
                .with_cross_axis_alignment(CrossAxisAlignment::Center)
                .with_spacing(4.)
                .with_child(chevron(collapsed, sub_text))
                .with_child(
                    appearance
                        .ui_builder()
                        .paragraph(title.to_uppercase())
                        .with_style(UiComponentStyles {
                            font_size: Some(SECTION_FONT_SIZE),
                            font_weight: Some(Weight::Semibold),
                            font_color: Some(sub_text),
                            ..Default::default()
                        })
                        .build()
                        .finish(),
                );
            if let Some(badge) = badge {
                line = line.with_child(badge_element(&badge, appearance));
            }
            Container::new(line.finish())
                .with_horizontal_padding(6.)
                .with_padding_top(8.)
                .with_padding_bottom(2.)
                .finish()
        })
        .with_cursor(Cursor::PointingHand)
        .on_click(move |ctx, _, _| {
            ctx.dispatch_typed_action(ExtensionPanelAction::ToggleExpansion {
                key: click_key.clone(),
            });
        })
        .finish()
    }

    fn render_item(&self, row: &PanelRow, appearance: &Appearance) -> Box<dyn Element> {
        let RowKind::Item {
            label,
            description,
            icon,
            badge,
            depth,
            expansion,
            ..
        } = &row.kind
        else {
            return Container::new(Flex::row().finish()).finish();
        };

        let theme = appearance.theme();
        let text_color = theme.foreground().into_solid();
        let sub_text = internal_colors::text_sub(theme, theme.background().into_solid());
        let label = label.clone();
        let description = description.clone();
        let icon = icon.as_deref().and_then(icon_for);
        let badge = badge.clone();
        let expansion = *expansion;
        let indent = INDENT * *depth as f32;

        let has_actions = !row.actions().is_empty();
        let is_menu_open = self.overflow_menu_open_for.as_ref() == Some(&row.key);
        let overflow_state = self.mouse_state(&self.overflow_button_states, &row.key);
        let overflow_menu = self.overflow_menu.clone();
        let menu_key = row.key.clone();

        let hoverable = Hoverable::new(
            self.mouse_state(&self.row_mouse_states, &row.key),
            move |state| {
                let mut line = Flex::row()
                    .with_cross_axis_alignment(CrossAxisAlignment::Center)
                    .with_spacing(4.);
                if let Some(expanded) = expansion {
                    line = line.with_child(chevron(!expanded, sub_text));
                }
                if let Some(icon) = icon {
                    line = line.with_child(
                        ConstrainedBox::new(
                            icon.to_warpui_icon(ThemeFill::Solid(sub_text)).finish(),
                        )
                        .with_width(ICON_SIZE)
                        .with_height(ICON_SIZE)
                        .finish(),
                    );
                }
                line = line.with_child(
                    Shrinkable::new(
                        1.0,
                        Text::new_inline(label.clone(), appearance.ui_font_family(), ROW_FONT_SIZE)
                            .with_color(text_color)
                            .soft_wrap(false)
                            .finish(),
                    )
                    .finish(),
                );
                if let Some(description) = &description {
                    line = line.with_child(
                        Shrinkable::new(
                            1.0,
                            Text::new_inline(
                                description.clone(),
                                appearance.ui_font_family(),
                                DESCRIPTION_FONT_SIZE,
                            )
                            .with_color(sub_text)
                            .soft_wrap(false)
                            .finish(),
                        )
                        .finish(),
                    );
                }
                if let Some(badge) = &badge {
                    line = line.with_child(badge_element(badge, appearance));
                }

                let row_content = ConstrainedBox::new(
                    Container::new(line.finish())
                        .with_padding_left(6. + indent)
                        .with_padding_right(6.)
                        .with_vertical_padding(3.)
                        .with_background(if state.is_hovered() {
                            Fill::from(internal_colors::fg_overlay_1(theme))
                        } else {
                            Fill::None
                        })
                        .finish(),
                )
                .with_min_height(ROW_MIN_HEIGHT)
                .finish();

                let mut stack = Stack::new().with_child(row_content);
                // The overflow button is only drawn while the row is under the
                // pointer or its menu is open, so a panel at rest is a list of
                // labels rather than a column of buttons.
                if has_actions && (state.is_hovered() || is_menu_open) {
                    let key = menu_key.clone();
                    stack.add_child(
                        Align::new(
                            Container::new(
                                icon_button_with_context_menu(
                                    Icon::DotsVertical,
                                    move |ctx, _, _| {
                                        ctx.dispatch_typed_action(
                                            ExtensionPanelAction::ToggleOverflowMenu {
                                                key: key.clone(),
                                            },
                                        );
                                    },
                                    overflow_state.clone(),
                                    &overflow_menu,
                                    is_menu_open,
                                    MenuDirection::Left,
                                    Some(Cursor::PointingHand),
                                    None,
                                    appearance,
                                )
                                .finish(),
                            )
                            .with_padding_right(4.)
                            .finish(),
                        )
                        .right()
                        .finish(),
                    );
                }
                stack.finish()
            },
        );

        let key = row.key.clone();
        let primary = row.primary_action().map(|action| action.id.clone());
        let is_expandable = expansion.is_some();
        if !is_expandable && primary.is_none() {
            return hoverable.finish();
        }
        hoverable
            .with_cursor(Cursor::PointingHand)
            .on_click(move |ctx, _, _| {
                if is_expandable {
                    ctx.dispatch_typed_action(ExtensionPanelAction::ToggleExpansion {
                        key: key.clone(),
                    });
                } else if let Some(action_id) = &primary {
                    ctx.dispatch_typed_action(ExtensionPanelAction::Invoke {
                        key: key.clone(),
                        action_id: action_id.clone(),
                    });
                }
            })
            .finish()
    }

    /// The handle a row's hover state lives in.
    ///
    /// Every drawn row has one — [`Self::rebuild_rows`] creates them alongside
    /// the rows — so the fallback only covers the frame between a snapshot
    /// arriving and the rebuild it triggers.
    fn mouse_state(
        &self,
        states: &HashMap<RowKey, MouseStateHandle>,
        key: &RowKey,
    ) -> MouseStateHandle {
        states.get(key).cloned().unwrap_or_default()
    }
}

fn chevron(collapsed: bool, color: ColorU) -> Box<dyn Element> {
    let icon = if collapsed {
        Icon::ChevronRight
    } else {
        Icon::ChevronDown
    };
    ConstrainedBox::new(icon.to_warpui_icon(ThemeFill::Solid(color)).finish())
        .with_width(CHEVRON_SIZE)
        .with_height(CHEVRON_SIZE)
        .finish()
}

/// A short count or status code the plugin attached to a row.
fn badge_element(badge: &str, appearance: &Appearance) -> Box<dyn Element> {
    let theme = appearance.theme();
    Text::new_inline(
        badge.to_owned(),
        appearance.ui_font_family(),
        BADGE_FONT_SIZE,
    )
    .with_color(internal_colors::text_sub(
        theme,
        theme.background().into_solid(),
    ))
    .soft_wrap(false)
    .finish()
}

/// The presentation Warp keeps for itself: loading, empty and error all look
/// the same whichever extension is behind them.
fn centered_message(
    appearance: &Appearance,
    title: &str,
    description: Option<&str>,
) -> Box<dyn Element> {
    let theme = appearance.theme();
    let mut content = Flex::column()
        .with_main_axis_size(MainAxisSize::Min)
        .with_cross_axis_alignment(CrossAxisAlignment::Center)
        .with_child(
            appearance
                .ui_builder()
                .paragraph(title.to_owned())
                .with_style(UiComponentStyles {
                    font_size: Some(13.),
                    font_color: Some(internal_colors::text_sub(
                        theme,
                        theme.background().into_solid(),
                    )),
                    ..Default::default()
                })
                .build()
                .finish(),
        );
    if let Some(description) = description {
        content = content.with_child(
            Container::new(
                appearance
                    .ui_builder()
                    .paragraph(description.to_owned())
                    .with_style(UiComponentStyles {
                        font_size: Some(12.),
                        font_color: Some(internal_colors::text_sub(
                            theme,
                            theme.background().into_solid(),
                        )),
                        ..Default::default()
                    })
                    .build()
                    .finish(),
            )
            .with_margin_top(6.)
            .finish(),
        );
    }
    Align::new(
        Container::new(content.finish())
            .with_uniform_padding(24.)
            .finish(),
    )
    .finish()
}

#[cfg(test)]
#[path = "extension_panel_tests.rs"]
mod tests;
