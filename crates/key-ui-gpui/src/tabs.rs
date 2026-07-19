use gpui::{
    Animation, AnimationExt, AnyElement, App, BoxShadow, ClickEvent, Context, FontWeight,
    IntoElement, Pixels, Point, Render, RenderOnce, ScrollHandle, SharedString, Window,
    WindowControlArea, div, ease_in_out, linear_color_stop, linear_gradient, point, prelude::*, px,
};
use gpui_component::{Icon, IconName};
use std::rc::Rc;
use std::time::Duration;

use crate::{HoverCardShell, ThemeTokens};

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
pub type TabSegmentAction = Rc<dyn Fn(usize, u64, &ClickEvent, &mut Window, &mut App)>;
pub type TabBarAction = Rc<dyn Fn(&ClickEvent, &mut Window, &mut App)>;
pub type TabHoverAction = Rc<dyn Fn(Option<usize>, &mut Window, &mut App)>;
pub type TabDropAction = Rc<dyn Fn(&TabDragPayload, usize, &mut Window, &mut App)>;

#[derive(Clone, Debug, PartialEq)]
pub struct TabPresentation {
    pub view: u64,
    pub title: SharedString,
    pub detail: SharedString,
    pub drag: Option<TabDragPayload>,
    pub recently_moved: bool,
    pub secondary: Option<TabSegmentPresentation>,
    pub active_segment: usize,
    pub outgoing_segment: Option<usize>,
    pub segment_transition: f32,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TabSegmentPresentation {
    pub view: u64,
    pub title: SharedString,
    pub detail: SharedString,
}

impl TabPresentation {
    #[must_use]
    pub fn new(title: impl Into<SharedString>, detail: impl Into<SharedString>) -> Self {
        Self {
            view: 0,
            title: title.into(),
            detail: detail.into(),
            drag: None,
            recently_moved: false,
            secondary: None,
            active_segment: 0,
            outgoing_segment: None,
            segment_transition: 1.0,
        }
    }

    #[must_use]
    pub fn view(mut self, view: u64) -> Self {
        self.view = view;
        self
    }

    #[must_use]
    pub fn draggable(mut self, payload: TabDragPayload) -> Self {
        self.drag = Some(payload);
        self
    }

    #[must_use]
    pub fn recently_moved(mut self, recently_moved: bool) -> Self {
        self.recently_moved = recently_moved;
        self
    }

    #[must_use]
    pub fn split(
        mut self,
        view: u64,
        title: impl Into<SharedString>,
        detail: impl Into<SharedString>,
        active_segment: usize,
    ) -> Self {
        self.secondary = Some(TabSegmentPresentation {
            view,
            title: title.into(),
            detail: detail.into(),
        });
        self.active_segment = active_segment.min(1);
        self
    }

    /// Crossfades the selected treatment between a compound tab's children.
    #[must_use]
    pub fn segment_transition(mut self, outgoing: Option<usize>, progress: f32) -> Self {
        self.outgoing_segment = outgoing.map(|segment| segment.min(1));
        self.segment_transition = if progress.is_finite() {
            progress.clamp(0.0, 1.0)
        } else {
            1.0
        };
        self
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TabDragPayload {
    pub source_window: u64,
    pub tab: u64,
    pub view: u64,
    pub title: SharedString,
}

impl TabDragPayload {
    #[must_use]
    pub fn new(source_window: u64, tab: u64, view: u64, title: impl Into<SharedString>) -> Self {
        Self {
            source_window,
            tab,
            view,
            title: title.into(),
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
    on_activate_segment: Option<TabSegmentAction>,
    on_close: Option<TabIndexAction>,
    on_hover: TabHoverAction,
    on_sidebar: TabBarAction,
    on_search: TabBarAction,
    on_new: TabBarAction,
    on_drop: Option<TabDropAction>,
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
            on_activate_segment: None,
            on_close: None,
            on_hover,
            on_sidebar,
            on_search,
            on_new,
            on_drop: None,
        }
    }

    #[must_use]
    pub fn on_close(mut self, action: TabIndexAction) -> Self {
        self.on_close = Some(action);
        self
    }

    #[must_use]
    pub fn on_activate_segment(mut self, action: TabSegmentAction) -> Self {
        self.on_activate_segment = Some(action);
        self
    }

    #[must_use]
    pub fn on_drop(mut self, action: TabDropAction) -> Self {
        self.on_drop = Some(action);
        self
    }
}

impl RenderOnce for TabStrip {
    fn render(self, window: &mut Window, _cx: &mut App) -> impl IntoElement {
        let tokens = self.tokens;
        let fade_width = px(42.0);
        let tab_count = self.tabs.len();
        let tab_units = self
            .tabs
            .iter()
            .map(|tab| if tab.secondary.is_some() { 1.65 } else { 1.0 })
            .sum::<f32>();
        let available_tab_width =
            (f32::from(window.viewport_size().width) - TAB_BAR_LEADING_INSET - 96.0 - 52.0)
                .max(148.0);
        let tabs_overflow = tab_units * 148.0 + 8.0 > available_tab_width;
        let preferred_tab_width = (tab_units * 260.0 + 8.0).min(available_tab_width);
        let drop_at_end = self.on_drop.clone();
        let tabs = self.tabs.into_iter().enumerate().map(|(index, tab)| {
            let active = index == self.active;
            let show_separator = !active && index + 1 < tab_count && index + 1 != self.active;
            let activate = self.on_activate.clone();
            let activate_segment = self.on_activate_segment.clone();
            let hover = self.on_hover.clone();
            let close = self.on_close.clone();
            let drop = self.on_drop.clone();
            let drag = tab.drag.clone();
            let recently_moved = tab.recently_moved;
            let is_compound = tab.secondary.is_some();
            let primary_title = tab.title;
            let primary_view = tab.view;
            let secondary = tab.secondary;
            let active_segment = tab.active_segment;
            let outgoing_segment = tab.outgoing_segment;
            let segment_transition = tab.segment_transition;
            let segment = |segment_index: usize,
                           view: u64,
                           title: SharedString,
                           activate_segment: Option<TabSegmentAction>| {
                let selected = active && active_segment == segment_index;
                let emphasis = if !active {
                    0.0
                } else if selected {
                    segment_transition
                } else if outgoing_segment == Some(segment_index) {
                    1.0 - segment_transition
                } else {
                    0.0
                };
                div()
                    .id(("workspace-tab-segment", index * 2 + segment_index))
                    .relative()
                    .min_w_0()
                    .h(px(32.0))
                    .px_2()
                    .flex_1()
                    .flex()
                    .items_center()
                    .gap_2()
                    .rounded_lg()
                    .border_1()
                    .border_color(tokens.surface.border.opacity(0.9 * emphasis))
                    .bg(tokens.surface.overlay.opacity(emphasis))
                    .shadow(vec![
                        BoxShadow {
                            color: gpui::black().opacity(0.1 * emphasis),
                            offset: point(px(0.0), px(1.0)),
                            blur_radius: px(3.0),
                            spread_radius: px(-1.0),
                        },
                        BoxShadow {
                            color: gpui::black().opacity(0.12 * emphasis),
                            offset: point(px(0.0), px(3.0)),
                            blur_radius: px(9.0),
                            spread_radius: px(-2.0),
                        },
                    ])
                    .when(!selected, |segment| {
                        segment.hover(move |segment| segment.bg(tokens.action.control_hover))
                    })
                    .when_some(activate_segment, |segment, action| {
                        segment.on_click(move |event, window, cx| {
                            cx.stop_propagation();
                            action(index, view, event, window, cx);
                        })
                    })
                    .child(
                        Icon::new(IconName::File)
                            .size(px(14.0))
                            .text_color(if selected {
                                tokens.action.accent
                            } else {
                                tokens.content.tertiary
                            }),
                    )
                    .child(
                        div()
                            .min_w_0()
                            .overflow_hidden()
                            .text_ellipsis()
                            .whitespace_nowrap()
                            .text_sm()
                            .font_weight(if selected {
                                FontWeight::MEDIUM
                            } else {
                                FontWeight::NORMAL
                            })
                            .child(title),
                    )
            };
            div()
                .id(("workspace-tab", index))
                .relative()
                .h(px(42.0))
                .mt(px(10.0))
                .min_w(px(if is_compound { 240.0 } else { 148.0 }))
                .max_w(px(if is_compound { 380.0 } else { 260.0 }))
                .flex_1()
                .px(if is_compound { px(4.0) } else { px(12.0) })
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
                    tokens.surface.muted
                })
                .bg(if active {
                    tokens.surface.background
                } else {
                    tokens.surface.muted
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
                .when(!is_compound, |tab| {
                    tab.child(
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
                            .child(primary_title.clone()),
                    )
                })
                .when(is_compound, |tab| {
                    let secondary = secondary.expect("compound tab has a secondary segment");
                    tab.child(segment(
                        0,
                        primary_view,
                        primary_title,
                        activate_segment.clone(),
                    ))
                    .child(
                        div()
                            .h(px(16.0))
                            .w(px(1.0))
                            .flex_none()
                            .rounded_full()
                            .bg(tokens.surface.border.opacity(0.58)),
                    )
                    .child(segment(
                        1,
                        secondary.view,
                        secondary.title,
                        activate_segment,
                    ))
                })
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
                .when(show_separator, |tab| {
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
                .when(recently_moved, |tab| {
                    tab.child(
                        div()
                            .absolute()
                            .inset_0()
                            .rounded_t_xl()
                            .bg(tokens.action.accent_soft)
                            .with_animation(
                                "recently-moved-tab",
                                Animation::new(Duration::from_millis(260)).with_easing(ease_in_out),
                                |pulse, delta| pulse.opacity(1.0 - delta),
                            ),
                    )
                })
                .when_some(drag, |tab, drag| {
                    let preview_tokens = tokens;
                    tab.cursor_move()
                        .on_drag(drag, move |drag, position, _, cx| {
                            cx.new(|_| TabDragPreview {
                                tokens: preview_tokens,
                                title: drag.title.clone(),
                                position,
                            })
                        })
                })
                .when_some(drop, |tab, drop| {
                    tab.drag_over::<TabDragPayload>(move |style, _, _, _| {
                        style.border_l_2().border_color(tokens.action.accent)
                    })
                    .on_drop(move |drag: &TabDragPayload, window, cx| {
                        drop(drag, index, window, cx);
                    })
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
                    .when(tabs_overflow, |rail| rail.flex_1())
                    .when(!tabs_overflow, |rail| {
                        rail.w(px(preferred_tab_width)).flex_none()
                    })
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
                            .children(tabs)
                            .child(
                                div()
                                    .id("workspace-tab-drop-end")
                                    .h_full()
                                    .w(px(8.0))
                                    .flex_none()
                                    .when_some(drop_at_end, |target, drop| {
                                        target
                                            .drag_over::<TabDragPayload>(move |style, _, _, _| {
                                                style
                                                    .border_l_2()
                                                    .border_color(tokens.action.accent)
                                            })
                                            .on_drop(move |drag: &TabDragPayload, window, cx| {
                                                drop(drag, tab_count, window, cx);
                                            })
                                    }),
                            ),
                    )
                    .when(tabs_overflow, |strip| {
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
            .when(!tabs_overflow, |bar| bar.child(div().h_full().flex_1()))
    }
}

struct TabDragPreview {
    tokens: ThemeTokens,
    title: SharedString,
    position: Point<Pixels>,
}

impl Render for TabDragPreview {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div()
            .pl(self.position.x - px(110.0))
            .pt(self.position.y - px(20.0))
            .child(
                div()
                    .w(px(220.0))
                    .h(px(40.0))
                    .px_3()
                    .flex()
                    .items_center()
                    .gap_2()
                    .overflow_hidden()
                    .rounded_lg()
                    .border_1()
                    .border_color(self.tokens.surface.border)
                    .shadow_lg()
                    .bg(self.tokens.surface.background)
                    .text_color(self.tokens.content.primary)
                    .child(
                        Icon::new(IconName::File)
                            .size(px(14.0))
                            .text_color(self.tokens.action.accent),
                    )
                    .child(
                        div()
                            .min_w_0()
                            .overflow_hidden()
                            .text_ellipsis()
                            .whitespace_nowrap()
                            .text_sm()
                            .font_weight(FontWeight::MEDIUM)
                            .child(self.title.clone()),
                    ),
            )
    }
}

/// Compact detail surface displayed below an inactive hovered tab.
#[derive(IntoElement)]
pub struct TabHoverCard {
    tokens: ThemeTokens,
    title: SharedString,
    leading: Option<AnyElement>,
    subtitle: Option<SharedString>,
    body: Option<AnyElement>,
    footer: Option<AnyElement>,
}

impl TabHoverCard {
    #[must_use]
    pub fn new(tokens: ThemeTokens, title: impl Into<SharedString>) -> Self {
        Self {
            tokens,
            title: title.into(),
            leading: None,
            subtitle: None,
            body: None,
            footer: None,
        }
    }

    #[must_use]
    pub fn leading(mut self, leading: impl IntoElement) -> Self {
        self.leading = Some(leading.into_any_element());
        self
    }

    #[must_use]
    pub fn subtitle(mut self, subtitle: impl Into<SharedString>) -> Self {
        self.subtitle = Some(subtitle.into());
        self
    }

    #[must_use]
    pub fn body(mut self, body: impl IntoElement) -> Self {
        self.body = Some(body.into_any_element());
        self
    }

    #[must_use]
    pub fn footer(mut self, footer: impl IntoElement) -> Self {
        self.footer = Some(footer.into_any_element());
        self
    }
}

impl RenderOnce for TabHoverCard {
    fn render(self, _window: &mut Window, _cx: &mut App) -> impl IntoElement {
        let title_character_count = self.title.chars().count();
        let title_distance = ((title_character_count.saturating_sub(31)) as f32 * 7.2).max(0.0);
        let title: AnyElement = if title_distance > 0.0 {
            div()
                .absolute()
                .left_0()
                .whitespace_nowrap()
                .font_weight(FontWeight::SEMIBOLD)
                .child(self.title)
                .with_animation(
                    "tab-hover-title-marquee",
                    Animation::new(Duration::from_secs_f32(
                        (title_distance / 22.0).max(4.5) * 2.0,
                    ))
                    .repeat()
                    .with_easing(ease_in_out),
                    move |title, delta| {
                        let travel = if delta <= 0.5 {
                            delta * 2.0
                        } else {
                            (1.0 - delta) * 2.0
                        };
                        title.ml(px(-title_distance * travel))
                    },
                )
                .into_any_element()
        } else {
            div()
                .absolute()
                .left_0()
                .whitespace_nowrap()
                .font_weight(FontWeight::SEMIBOLD)
                .child(self.title)
                .into_any_element()
        };
        let header = div()
            .p_3()
            .flex()
            .items_center()
            .gap_3()
            .children(self.leading)
            .child(
                div()
                    .min_w_0()
                    .flex_1()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .child(
                        div()
                            .relative()
                            .h(px(18.0))
                            .overflow_hidden()
                            .text_sm()
                            .child(title),
                    )
                    .when_some(self.subtitle, |content, subtitle| {
                        content.child(
                            div()
                                .overflow_hidden()
                                .text_ellipsis()
                                .whitespace_nowrap()
                                .text_xs()
                                .text_color(self.tokens.content.secondary)
                                .child(subtitle),
                        )
                    }),
            );

        HoverCardShell::new(self.tokens, px(TAB_HOVER_CARD_WIDTH))
            .section(header)
            .when_some(self.body, |card, body| card.section(body))
            .when_some(self.footer, |card, footer| card.section(footer))
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
    use super::{TabPresentation, tab_hover_card_x, tab_search_popover_x};

    #[test]
    fn compound_tab_presentation_keeps_typed_child_identity_and_bounds_selection() {
        let tab = TabPresentation::new("First", "p 1/2")
            .view(41)
            .split(42, "Second", "p 2/2", 9)
            .segment_transition(Some(0), 0.35);
        assert_eq!(tab.view, 41);
        assert_eq!(tab.active_segment, 1);
        assert_eq!(tab.outgoing_segment, Some(0));
        assert!((tab.segment_transition - 0.35).abs() < f32::EPSILON);
        let second = tab.secondary.expect("split builder adds a second segment");
        assert_eq!(second.view, 42);
        assert_eq!(second.title.as_ref(), "Second");
    }

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
