use gpui::{
    AnyElement, App, ClickEvent, FontWeight, IntoElement, RenderOnce, ScrollHandle, SharedString,
    Window, WindowControlArea, div, linear_color_stop, linear_gradient, prelude::*, px,
};
use gpui_component::{Icon, IconName};
use std::rc::Rc;

use crate::ThemeTokens;

pub const TAB_BAR_HEIGHT: f32 = 52.0;
pub const TAB_HOVER_CARD_WIDTH: f32 = 282.0;
pub const TAB_SEARCH_POPOVER_WIDTH: f32 = 380.0;
const TAB_BAR_LEADING_INSET: f32 = 104.0;

#[must_use]
pub fn tab_search_popover_x(viewport_width: f32) -> f32 {
    // Align the surface with the start of the title-bar controls. This keeps
    // the chevron visually connected to the popover without letting a wide
    // surface drift over the document from the far edge of the window.
    let desired = TAB_BAR_LEADING_INSET;
    desired.clamp(
        8.0,
        (viewport_width - TAB_SEARCH_POPOVER_WIDTH - 8.0).max(8.0),
    )
}

#[must_use]
pub fn tab_hover_card_x(
    tab_left: f32,
    tab_width: f32,
    viewport_width: f32,
    card_width: f32,
) -> f32 {
    (tab_left + (tab_width - card_width) * 0.5)
        .clamp(8.0, (viewport_width - card_width - 8.0).max(8.0))
}

pub type TabIndexAction = Rc<dyn Fn(usize, &ClickEvent, &mut Window, &mut App)>;
pub type TabBarAction = Rc<dyn Fn(&ClickEvent, &mut Window, &mut App)>;
pub type TabHoverAction = Rc<dyn Fn(Option<usize>, &mut Window, &mut App)>;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TabPresentation {
    pub title: SharedString,
    pub detail: SharedString,
}

impl TabPresentation {
    #[must_use]
    pub fn new(title: impl Into<SharedString>, detail: impl Into<SharedString>) -> Self {
        Self {
            title: title.into(),
            detail: detail.into(),
        }
    }
}

/// Application-neutral Chrome-style title-bar tab strip.
///
/// The owning workspace supplies identities and behavior. This component owns
/// clipping, horizontal scrolling, edge fades, compact hover detail, and the
/// stable sidebar/search/new-tab affordances shared by Key applications.
#[derive(IntoElement)]
pub struct TabStrip {
    tokens: ThemeTokens,
    tabs: Vec<TabPresentation>,
    active: usize,
    scroll: ScrollHandle,
    on_activate: TabIndexAction,
    on_close: Option<TabIndexAction>,
    on_hover: TabHoverAction,
    on_sidebar: TabBarAction,
    on_search: TabBarAction,
    on_new: TabBarAction,
}

impl TabStrip {
    #[allow(clippy::too_many_arguments)]
    #[must_use]
    pub fn new(
        tokens: ThemeTokens,
        tabs: Vec<TabPresentation>,
        active: usize,
        scroll: ScrollHandle,
        on_activate: TabIndexAction,
        on_hover: TabHoverAction,
        on_sidebar: TabBarAction,
        on_search: TabBarAction,
        on_new: TabBarAction,
    ) -> Self {
        Self {
            tokens,
            tabs,
            active,
            scroll,
            on_activate,
            on_close: None,
            on_hover,
            on_sidebar,
            on_search,
            on_new,
        }
    }

    #[must_use]
    pub fn on_close(mut self, action: TabIndexAction) -> Self {
        self.on_close = Some(action);
        self
    }
}

impl RenderOnce for TabStrip {
    fn render(self, _window: &mut Window, _cx: &mut App) -> impl IntoElement {
        let tokens = self.tokens;
        let fade_width = px(42.0);
        let tab_count = self.tabs.len();
        let tabs = self.tabs.into_iter().enumerate().map(|(index, tab)| {
            let active = index == self.active;
            let activate = self.on_activate.clone();
            let hover = self.on_hover.clone();
            let close = self.on_close.clone();
            div()
                .id(("workspace-tab", index))
                .relative()
                .h(px(42.0))
                .mt(px(10.0))
                .min_w(px(148.0))
                .max_w(px(260.0))
                .flex_1()
                .px_3()
                .flex()
                .items_center()
                .gap_2()
                .rounded_t_xl()
                .border_t_1()
                .border_l_1()
                .border_r_1()
                .border_color(if active {
                    tokens.surface.border.opacity(0.72)
                } else {
                    tokens.surface.border.opacity(0.42)
                })
                .bg(if active {
                    tokens.surface.background
                } else {
                    tokens.surface.overlay
                })
                .text_color(if active {
                    tokens.content.primary
                } else {
                    tokens.content.secondary
                })
                .cursor_pointer()
                .when(!active, |tab| {
                    tab.hover(move |tab| tab.bg(tokens.action.control_hover))
                })
                .on_hover(move |is_hovered, window, cx| {
                    hover(is_hovered.then_some(index), window, cx)
                })
                .on_click(move |event, window, cx| activate(index, event, window, cx))
                .child(
                    Icon::new(IconName::File)
                        .size(px(15.0))
                        .text_color(if active {
                            tokens.action.accent
                        } else {
                            tokens.content.tertiary
                        }),
                )
                .child(
                    div()
                        .min_w_0()
                        .flex_1()
                        .overflow_hidden()
                        .text_ellipsis()
                        .whitespace_nowrap()
                        .text_sm()
                        .font_weight(if active {
                            FontWeight::MEDIUM
                        } else {
                            FontWeight::NORMAL
                        })
                        .child(tab.title),
                )
                .when_some(close, |tab, close| {
                    tab.child(
                        div()
                            .id(("close-workspace-tab", index))
                            .size(px(24.0))
                            .flex_none()
                            .flex()
                            .items_center()
                            .justify_center()
                            .rounded_md()
                            .hover(move |button| button.bg(tokens.action.control_pressed))
                            .on_click(move |event, window, cx| {
                                cx.stop_propagation();
                                close(index, event, window, cx);
                            })
                            .child(Icon::new(IconName::Close).size(px(13.0))),
                    )
                })
                .when(!active, |tab| {
                    tab.child(
                        div()
                            .absolute()
                            .top(px(13.0))
                            .bottom(px(9.0))
                            .right_0()
                            .w(px(1.0))
                            .bg(tokens.surface.border.opacity(0.5)),
                    )
                })
        });

        let sidebar = self.on_sidebar;
        let search = self.on_search;
        let new_tab = self.on_new;
        div()
            .relative()
            .h(px(TAB_BAR_HEIGHT))
            .w_full()
            .flex_none()
            .flex()
            .items_center()
            .overflow_hidden()
            .pl(px(TAB_BAR_LEADING_INSET))
            .window_control_area(WindowControlArea::Drag)
            .bg(tokens.surface.chrome)
            .border_b_1()
            .border_color(tokens.surface.border.opacity(0.65))
            .child(
                div()
                    .h_full()
                    .flex_none()
                    .flex()
                    .items_center()
                    .gap_1()
                    .px_1()
                    .child(titlebar_icon_button(
                        tokens,
                        "workspace-sidebar-placeholder",
                        IconName::PanelLeft,
                        sidebar,
                    ))
                    .child(titlebar_icon_button(
                        tokens,
                        "workspace-tab-search",
                        IconName::ChevronDown,
                        search,
                    ))
                    .child(
                        div()
                            .h(px(24.0))
                            .w(px(1.0))
                            .mx_2()
                            .bg(tokens.surface.border.opacity(0.72)),
                    ),
            )
            .child(
                div()
                    .relative()
                    .h_full()
                    .min_w_0()
                    .flex_1()
                    .overflow_hidden()
                    .bg(tokens.surface.muted)
                    .child(
                        div()
                            .id("workspace-tabs-scroll")
                            .h_full()
                            .w_full()
                            .flex()
                            .overflow_x_scroll()
                            .track_scroll(&self.scroll)
                            .children(tabs),
                    )
                    .when(tab_count > 1, |strip| {
                        strip
                            .child(
                                div()
                                    .absolute()
                                    .top_0()
                                    .bottom_0()
                                    .left_0()
                                    .w(fade_width)
                                    .bg(linear_gradient(
                                        90.0,
                                        linear_color_stop(tokens.surface.muted, 0.0),
                                        linear_color_stop(tokens.surface.muted.opacity(0.0), 1.0),
                                    )),
                            )
                            .child(
                                div()
                                    .absolute()
                                    .top_0()
                                    .bottom_0()
                                    .right_0()
                                    .w(fade_width)
                                    .bg(linear_gradient(
                                        270.0,
                                        linear_color_stop(tokens.surface.muted, 0.0),
                                        linear_color_stop(tokens.surface.muted.opacity(0.0), 1.0),
                                    )),
                            )
                    }),
            )
            .child(
                div()
                    .h_full()
                    .flex_none()
                    .flex()
                    .items_center()
                    .px_2()
                    .child(titlebar_icon_button(
                        tokens,
                        "workspace-new-tab",
                        IconName::Plus,
                        new_tab,
                    )),
            )
    }
}

/// Compact detail surface displayed below an inactive hovered tab.
#[derive(IntoElement)]
pub struct TabHoverCard {
    tokens: ThemeTokens,
    title: SharedString,
    detail: SharedString,
    status: Option<SharedString>,
}

impl TabHoverCard {
    #[must_use]
    pub fn new(
        tokens: ThemeTokens,
        title: impl Into<SharedString>,
        detail: impl Into<SharedString>,
    ) -> Self {
        Self {
            tokens,
            title: title.into(),
            detail: detail.into(),
            status: None,
        }
    }

    #[must_use]
    pub fn status(mut self, status: impl Into<SharedString>) -> Self {
        self.status = Some(status.into());
        self
    }
}

impl RenderOnce for TabHoverCard {
    fn render(self, _window: &mut Window, _cx: &mut App) -> impl IntoElement {
        div()
            .occlude()
            .w(px(TAB_HOVER_CARD_WIDTH))
            .overflow_hidden()
            .rounded_xl()
            .border_1()
            .border_color(self.tokens.content.primary.opacity(0.12))
            .shadow_lg()
            .bg(self.tokens.surface.background)
            .text_color(self.tokens.content.primary)
            .flex()
            .flex_col()
            .child(
                div()
                    .p_3()
                    .flex()
                    .items_center()
                    .gap_3()
                    .child(
                        div()
                            .size(px(32.0))
                            .flex_none()
                            .flex()
                            .items_center()
                            .justify_center()
                            .rounded_lg()
                            .bg(self.tokens.action.accent_soft)
                            .text_color(self.tokens.action.accent)
                            .child(Icon::new(IconName::File).size(px(15.0))),
                    )
                    .child(
                        div()
                            .min_w_0()
                            .flex_1()
                            .flex()
                            .flex_col()
                            .gap_1()
                            .child(
                                div()
                                    .overflow_hidden()
                                    .text_ellipsis()
                                    .whitespace_nowrap()
                                    .text_sm()
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .child(self.title),
                            )
                            .when_some(self.status, |content, status| {
                                content.child(
                                    div()
                                        .overflow_hidden()
                                        .text_ellipsis()
                                        .whitespace_nowrap()
                                        .text_xs()
                                        .text_color(self.tokens.content.secondary)
                                        .child(status),
                                )
                            }),
                    ),
            )
            .child(
                div()
                    .h(px(1.0))
                    .mx_3()
                    .bg(self.tokens.surface.border.opacity(0.7)),
            )
            .child(
                div()
                    .px_3()
                    .py_2()
                    .flex()
                    .items_center()
                    .gap_2()
                    .text_xs()
                    .text_color(self.tokens.content.tertiary)
                    .child(Icon::new(IconName::Folder).size(px(13.0)))
                    .child(
                        div()
                            .min_w_0()
                            .flex_1()
                            .overflow_hidden()
                            .text_ellipsis()
                            .whitespace_nowrap()
                            .child(self.detail),
                    ),
            )
    }
}

fn titlebar_icon_button(
    tokens: ThemeTokens,
    id: &'static str,
    icon: IconName,
    action: TabBarAction,
) -> impl IntoElement {
    div()
        .id(id)
        .size(px(34.0))
        .flex()
        .items_center()
        .justify_center()
        .rounded_lg()
        .text_color(tokens.content.secondary)
        .cursor_pointer()
        .hover(move |button| button.bg(tokens.action.control_hover))
        .active(move |button| button.bg(tokens.action.control_pressed))
        .on_click(move |event, window, cx| action(event, window, cx))
        .child(Icon::new(icon).size(px(16.0)))
}

/// Reusable popover shell for searchable tab lists. The owner supplies its
/// typed input and rows, keeping this component independent of editor state.
#[derive(IntoElement)]
pub struct TabSearchPopover {
    tokens: ThemeTokens,
    input: AnyElement,
    rows: Vec<AnyElement>,
}

impl TabSearchPopover {
    #[must_use]
    pub fn new(
        tokens: ThemeTokens,
        input: impl IntoElement,
        rows: impl IntoIterator<Item = AnyElement>,
    ) -> Self {
        Self {
            tokens,
            input: input.into_any_element(),
            rows: rows.into_iter().collect(),
        }
    }
}

impl RenderOnce for TabSearchPopover {
    fn render(self, _window: &mut Window, _cx: &mut App) -> impl IntoElement {
        let row_count = self.rows.len();
        let count: SharedString = format!("{row_count}").into();
        div()
            .occlude()
            .w(px(TAB_SEARCH_POPOVER_WIDTH))
            .max_h(px(460.0))
            .overflow_hidden()
            .rounded_xl()
            .border_1()
            .border_color(self.tokens.content.primary.opacity(0.12))
            .shadow_lg()
            .bg(self.tokens.surface.background)
            .text_color(self.tokens.content.primary)
            .flex()
            .flex_col()
            .child(
                div()
                    .h(px(48.0))
                    .flex_none()
                    .px_3()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(
                        Icon::new(IconName::Search)
                            .size(px(15.0))
                            .text_color(self.tokens.action.accent),
                    )
                    .child(
                        div()
                            .flex_1()
                            .text_sm()
                            .font_weight(FontWeight::SEMIBOLD)
                            .child("Open tabs"),
                    )
                    .child(
                        div()
                            .min_w(px(24.0))
                            .h(px(22.0))
                            .px_2()
                            .rounded_full()
                            .flex()
                            .items_center()
                            .justify_center()
                            .bg(self.tokens.surface.muted)
                            .text_xs()
                            .text_color(self.tokens.content.secondary)
                            .child(count),
                    ),
            )
            .child(
                div()
                    .h(px(1.0))
                    .bg(self.tokens.surface.border.opacity(0.72)),
            )
            .child(div().px_3().pt_3().pb_2().child(self.input))
            .child(
                div()
                    .id("tab-search-results")
                    .min_h_0()
                    .px_2()
                    .pb_2()
                    .overflow_y_scroll()
                    .flex()
                    .flex_col()
                    .children(self.rows),
            )
    }
}

#[cfg(test)]
mod tests {
    use super::{tab_hover_card_x, tab_search_popover_x};

    #[test]
    fn search_popover_stays_near_its_control_and_inside_narrow_windows() {
        assert_eq!(tab_search_popover_x(1_200.0), 104.0);
        assert_eq!(tab_search_popover_x(390.0), 8.0);
    }

    #[test]
    fn hover_card_centers_under_tabs_and_clamps_to_edges() {
        assert_eq!(tab_hover_card_x(400.0, 200.0, 1_200.0, 282.0), 359.0);
        assert_eq!(tab_hover_card_x(2.0, 120.0, 800.0, 282.0), 8.0);
        assert_eq!(tab_hover_card_x(760.0, 120.0, 800.0, 282.0), 510.0);
    }
}
