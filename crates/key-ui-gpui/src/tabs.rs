use gpui::{
    AnyElement, App, ClickEvent, FontWeight, IntoElement, RenderOnce, ScrollHandle, SharedString,
    Window, WindowControlArea, div, linear_color_stop, linear_gradient, prelude::*, px,
};
use gpui_component::{Icon, IconName};
use std::rc::Rc;

use crate::ThemeTokens;

pub const TAB_BAR_HEIGHT: f32 = 52.0;

pub type TabIndexAction = Rc<dyn Fn(usize, &ClickEvent, &mut Window, &mut App)>;
pub type TabBarAction = Rc<dyn Fn(&ClickEvent, &mut Window, &mut App)>;

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
    on_hover: Rc<dyn Fn(Option<usize>, &mut Window, &mut App)>,
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
        on_hover: Rc<dyn Fn(Option<usize>, &mut Window, &mut App)>,
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
                .border_l_1()
                .border_r_1()
                .border_color(if active {
                    tokens.surface.border.opacity(0.72)
                } else {
                    tokens.surface.chrome
                })
                .bg(if active {
                    tokens.surface.background
                } else {
                    tokens.surface.chrome
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
            .pl(px(104.0))
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
                    )),
            )
            .child(
                div()
                    .relative()
                    .h_full()
                    .min_w_0()
                    .flex_1()
                    .overflow_hidden()
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
                                        linear_color_stop(tokens.surface.chrome, 0.0),
                                        linear_color_stop(tokens.surface.chrome.opacity(0.0), 1.0),
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
                                        linear_color_stop(tokens.surface.chrome, 0.0),
                                        linear_color_stop(tokens.surface.chrome.opacity(0.0), 1.0),
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
        }
    }
}

impl RenderOnce for TabHoverCard {
    fn render(self, _window: &mut Window, _cx: &mut App) -> impl IntoElement {
        div()
            .occlude()
            .w(px(250.0))
            .p_3()
            .rounded_lg()
            .border_1()
            .border_color(self.tokens.surface.border)
            .shadow_md()
            .bg(self.tokens.surface.overlay)
            .text_color(self.tokens.content.primary)
            .flex()
            .flex_col()
            .gap_1()
            .child(
                div()
                    .overflow_hidden()
                    .text_ellipsis()
                    .whitespace_nowrap()
                    .text_sm()
                    .font_weight(FontWeight::MEDIUM)
                    .child(self.title),
            )
            .child(
                div()
                    .overflow_hidden()
                    .text_ellipsis()
                    .whitespace_nowrap()
                    .text_xs()
                    .text_color(self.tokens.content.secondary)
                    .child(self.detail),
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
        div()
            .occlude()
            .w(px(360.0))
            .max_h(px(430.0))
            .p_2()
            .rounded_xl()
            .border_1()
            .border_color(self.tokens.surface.border)
            .shadow_lg()
            .bg(self.tokens.surface.overlay)
            .text_color(self.tokens.content.primary)
            .flex()
            .flex_col()
            .gap_2()
            .child(self.input)
            .child(
                div()
                    .id("tab-search-results")
                    .min_h_0()
                    .overflow_y_scroll()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .children(self.rows),
            )
    }
}
