use gpui::{
    Animation, AnimationExt, AnyElement, App, ClickEvent, Context, IntoElement, Pixels, Point,
    Render, RenderOnce, ScrollHandle, SharedString, Window, WindowControlArea, div, ease_in_out,
    linear_color_stop, linear_gradient, prelude::*, px,
};
use gpui_component::{Icon, IconName};
use std::rc::Rc;
use std::time::Duration;

use crate::{
    DesignStyled, ElevationRole, HoverCardShell, IconRoleConfig, ThemeTokens, TypographyRole,
    resolved_design_system, semantic_icon,
};

#[must_use]
pub fn tab_search_popover_x(
    viewport_width: f32,
    leading_inset: f32,
    popover_width: f32,
    edge_margin: f32,
) -> f32 {
    // Align the surface with the start of the title-bar controls. This keeps
    // the chevron visually connected to the popover without letting a wide
    // surface drift over the document from the far edge of the window.
    let desired = leading_inset;
    desired.clamp(
        edge_margin,
        (viewport_width - popover_width - edge_margin).max(edge_margin),
    )
}

#[must_use]
pub fn tab_hover_card_x(
    tab_left: f32,
    tab_width: f32,
    viewport_width: f32,
    card_width: f32,
    edge_margin: f32,
) -> f32 {
    (tab_left + (tab_width - card_width) * 0.5).clamp(
        edge_margin,
        (viewport_width - card_width - edge_margin).max(edge_margin),
    )
}

#[must_use]
pub fn tab_hover_card_y(tab_bar_height: f32, tab_height: f32, gap: f32) -> f32 {
    (tab_bar_height - tab_height) * 0.5 + tab_height + gap
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
    on_close_segment: Option<TabSegmentAction>,
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
            on_close_segment: None,
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
    pub fn on_close_segment(mut self, action: TabSegmentAction) -> Self {
        self.on_close_segment = Some(action);
        self
    }

    #[must_use]
    pub fn on_drop(mut self, action: TabDropAction) -> Self {
        self.on_drop = Some(action);
        self
    }
}

impl RenderOnce for TabStrip {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let tokens = self.tokens;
        let system = resolved_design_system(cx);
        let viewport_width = f32::from(window.viewport_size().width);
        let width_class = system.responsive.classify(viewport_width);
        let chrome = *system.workspace.chrome.resolve(width_class);
        let utilities_in_tab_row = chrome.utilities_in_tab_row();
        let icons = system.appearance.icons;
        let fade_width = px(chrome.title_fade_width);
        let tab_count = self.tabs.len();
        let tab_units = self
            .tabs
            .iter()
            .map(|tab| {
                if tab.secondary.is_some() {
                    chrome.split_tab_width / chrome.tab_width
                } else {
                    1.0
                }
            })
            .sum::<f32>();
        let utility_width = if utilities_in_tab_row {
            chrome.utility_cluster_width
        } else {
            0.0
        };
        let available_tab_width = (viewport_width
            - chrome.tab_leading_inset
            - utility_width
            - chrome.trailing_reserved_width)
            .max(chrome.tab_width * chrome.tab_min_width_ratio);
        let tabs_overflow = tab_units * chrome.tab_width * chrome.tab_min_width_ratio
            + tokens.components.popover.edge_margin
            > available_tab_width;
        let preferred_tab_width = (tab_units * chrome.tab_width + 4.0).min(available_tab_width);
        let drop_at_end = self.on_drop.clone();
        let tabs = self.tabs.into_iter().enumerate().map(|(index, tab)| {
            let active = index == self.active;
            let show_separator = !active && index + 1 < tab_count && index + 1 != self.active;
            let activate = self.on_activate.clone();
            let activate_segment = self.on_activate_segment.clone();
            let close_segment = self.on_close_segment.clone();
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
                           activate_segment: Option<TabSegmentAction>,
                           close_segment: Option<TabSegmentAction>| {
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
                    .h(px(chrome.split_segment_height))
                    .px_2()
                    .flex_1()
                    .flex()
                    .items_center()
                    .gap_2()
                    .design_corners(tokens.components.corners.split_segment)
                    .border_1()
                    .border_color(tokens.surface.border.opacity(0.9 * emphasis))
                    .bg(tokens.surface.overlay.opacity(emphasis))
                    .design_elevation_with_strength(ElevationRole::Surface, emphasis, &tokens)
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
                        Icon::new(semantic_icon(icons.document))
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
                            .design_typography(
                                if selected {
                                    TypographyRole::Label
                                } else {
                                    TypographyRole::Body
                                },
                                &tokens,
                            )
                            .child(title),
                    )
                    .when_some(close_segment, |segment, close| {
                        segment.child(
                            div()
                                .id(("close-workspace-tab-segment", index * 2 + segment_index))
                                .size(px(20.0))
                                .flex_none()
                                .flex()
                                .items_center()
                                .justify_center()
                                .design_corners(tokens.components.corners.button)
                                .text_color(tokens.content.tertiary)
                                .hover(move |button| {
                                    button
                                        .bg(tokens.action.control_pressed)
                                        .text_color(tokens.content.primary)
                                })
                                .on_click(move |event, window, cx| {
                                    cx.stop_propagation();
                                    close(index, view, event, window, cx);
                                })
                                .child(Icon::new(semantic_icon(icons.close)).size(px(11.0))),
                        )
                    })
            };
            div()
                .id(("workspace-tab", index))
                .relative()
                .h(px(chrome.tab_height))
                .min_w(px(if is_compound {
                    chrome.split_tab_width * chrome.tab_min_width_ratio
                } else {
                    chrome.tab_width * chrome.tab_min_width_ratio
                }))
                .max_w(px(if is_compound {
                    chrome.split_tab_width
                } else {
                    chrome.tab_width
                }))
                .flex_1()
                .px(px(if is_compound {
                    chrome.split_horizontal_padding
                } else {
                    chrome.tab_horizontal_padding
                }))
                .flex()
                .items_center()
                .gap_2()
                .design_corners(tokens.components.corners.tab)
                .border_1()
                .border_color(if active {
                    tokens.materials.control.border
                } else {
                    gpui::transparent_black()
                })
                .bg(if active {
                    tokens.surface.background
                } else {
                    gpui::transparent_black()
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
                        Icon::new(semantic_icon(icons.document))
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
                            .design_typography(
                                if active {
                                    TypographyRole::Label
                                } else {
                                    TypographyRole::Body
                                },
                                &tokens,
                            )
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
                        close_segment.clone(),
                    ))
                    .child(
                        div()
                            .h(px(16.0))
                            .w(px(1.0))
                            .flex_none()
                            .design_corners(tokens.components.corners.context_pill)
                            .bg(tokens.surface.border.opacity(0.58)),
                    )
                    .child(segment(
                        1,
                        secondary.view,
                        secondary.title,
                        activate_segment,
                        close_segment,
                    ))
                })
                .when(!is_compound, |tab| {
                    tab.when_some(close, |tab, close| {
                        tab.child(
                            div()
                                .id(("close-workspace-tab", index))
                                .size(px(24.0))
                                .flex_none()
                                .flex()
                                .items_center()
                                .justify_center()
                                .design_corners(tokens.components.corners.button)
                                .hover(move |button| button.bg(tokens.action.control_pressed))
                                .on_click(move |event, window, cx| {
                                    cx.stop_propagation();
                                    close(index, event, window, cx);
                                })
                                .child(Icon::new(semantic_icon(icons.close)).size(px(13.0))),
                        )
                    })
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
                    tab.when_some(tokens.motion.tab_move_duration_ms, |tab, duration_ms| {
                        tab.child(
                            div()
                                .absolute()
                                .inset_0()
                                .design_corners(tokens.components.corners.tab)
                                .bg(tokens.action.accent_soft)
                                .with_animation(
                                    "recently-moved-tab",
                                    Animation::new(Duration::from_millis(duration_ms.into()))
                                        .with_easing(ease_in_out),
                                    |pulse, delta| pulse.opacity(1.0 - delta),
                                ),
                        )
                    })
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
        let inline_new_tab = new_tab.clone();
        div()
            .relative()
            .h(px(chrome.tab_bar_height))
            .w_full()
            .flex_none()
            .flex()
            .items_center()
            .overflow_hidden()
            .pl(px(if utilities_in_tab_row {
                chrome.utility_controls_leading_inset
            } else {
                chrome.tab_leading_inset
            }))
            .window_control_area(WindowControlArea::Drag)
            .bg(tokens.materials.chrome.background)
            .when(utilities_in_tab_row, |bar| {
                bar.child(workspace_utility_controls(
                    tokens, icons, sidebar, search, true,
                ))
            })
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
                    .child(
                        div()
                            .id("workspace-tabs-scroll")
                            .h_full()
                            .w_full()
                            .flex()
                            .items_center()
                            .overflow_x_scroll()
                            .track_scroll(&self.scroll)
                            .children(tabs)
                            .child(
                                div()
                                    .id("workspace-tab-drop-end")
                                    .h_full()
                                    .w(px(4.0))
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
                            )
                            .when(!tabs_overflow, |strip| {
                                strip.child(
                                    div()
                                        .h(px(chrome.tab_height))
                                        .flex_none()
                                        .flex()
                                        .items_center()
                                        .ml_1()
                                        .child(new_tab_button(
                                            tokens,
                                            "workspace-new-tab-inline",
                                            inline_new_tab,
                                            chrome.new_tab_button_size,
                                            semantic_icon(icons.new_tab),
                                        )),
                                )
                            }),
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
            .when(tabs_overflow, |bar| {
                bar.child(
                    div()
                        .h(px(chrome.tab_height))
                        .flex_none()
                        .flex()
                        .items_center()
                        .ml_1()
                        .child(new_tab_button(
                            tokens,
                            "workspace-new-tab-overflow",
                            new_tab,
                            chrome.new_tab_button_size,
                            semantic_icon(icons.new_tab),
                        )),
                )
            })
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
        let preview = self.tokens.components.popover;
        div()
            .pl(self.position.x - px(preview.drag_preview_width * 0.5))
            .pt(self.position.y - px(preview.drag_preview_height * 0.5))
            .child(
                div()
                    .w(px(preview.drag_preview_width))
                    .h(px(preview.drag_preview_height))
                    .px_3()
                    .flex()
                    .items_center()
                    .gap_2()
                    .overflow_hidden()
                    .design_corners(self.tokens.components.corners.card)
                    .border_1()
                    .border_color(self.tokens.surface.border)
                    .design_elevation(ElevationRole::Floating, &self.tokens)
                    .bg(self.tokens.materials.floating.background)
                    .text_color(self.tokens.content.primary)
                    .child(
                        Icon::new(semantic_icon(self.tokens.icons.document))
                            .size(px(self.tokens.components.common.icon_medium))
                            .text_color(self.tokens.action.accent),
                    )
                    .child(
                        div()
                            .min_w_0()
                            .overflow_hidden()
                            .text_ellipsis()
                            .whitespace_nowrap()
                            .design_typography(TypographyRole::Label, &self.tokens)
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
        let popover = self.tokens.components.popover;
        let title_distance = ((title_character_count
            .saturating_sub(popover.marquee_start_characters.into()))
            as f32
            * popover.average_character_width)
            .max(0.0);
        let marquee_speed = self.tokens.motion.marquee_points_per_second;
        let marquee_minimum = self.tokens.motion.marquee_min_duration_ms;
        let title: AnyElement = if title_distance > 0.0
            && let (Some(speed), Some(minimum_ms)) = (marquee_speed, marquee_minimum)
        {
            div()
                .absolute()
                .left_0()
                .whitespace_nowrap()
                .design_typography(TypographyRole::Heading, &self.tokens)
                .child(self.title)
                .with_animation(
                    "tab-hover-title-marquee",
                    Animation::new(Duration::from_secs_f32(
                        (title_distance / speed).max(minimum_ms as f32 / 1_000.0) * 2.0,
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
                .design_typography(TypographyRole::Heading, &self.tokens)
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
                            .h(px(self.tokens.typography.heading.size_rem
                                * 16.0
                                * self.tokens.typography.heading.line_height))
                            .overflow_hidden()
                            .child(title),
                    )
                    .when_some(self.subtitle, |content, subtitle| {
                        content.child(
                            div()
                                .overflow_hidden()
                                .text_ellipsis()
                                .whitespace_nowrap()
                                .design_typography(TypographyRole::Caption, &self.tokens)
                                .text_color(self.tokens.content.secondary)
                                .child(subtitle),
                        )
                    }),
            );

        HoverCardShell::new(
            self.tokens,
            px(self.tokens.components.popover.tab_hover_width),
        )
        .section(header)
        .when_some(self.body, |card, body| card.section(body))
        .when_some(self.footer, |card, footer| card.section(footer))
    }
}

/// Shared window-level utility group used by either chrome row according to
/// the typed responsive layout configuration.
pub fn workspace_utility_controls(
    tokens: ThemeTokens,
    icons: IconRoleConfig,
    sidebar: TabBarAction,
    search: TabBarAction,
    trailing_separator: bool,
) -> AnyElement {
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
            semantic_icon(icons.sidebar),
            sidebar,
        ))
        .child(titlebar_icon_button(
            tokens,
            "workspace-tab-search",
            semantic_icon(icons.tab_list),
            search,
        ))
        .when(trailing_separator, |group| {
            group.child(
                div()
                    .h(px(tokens.components.common.separator_length))
                    .w(px(tokens.components.common.separator_width))
                    .mx_2()
                    .bg(tokens.materials.chrome.border),
            )
        })
        .into_any_element()
}

fn titlebar_icon_button(
    tokens: ThemeTokens,
    id: &'static str,
    icon: IconName,
    action: TabBarAction,
) -> impl IntoElement {
    div()
        .id(id)
        .size(px(tokens.components.common.control_medium))
        .flex()
        .items_center()
        .justify_center()
        .design_corners(tokens.components.corners.button)
        .text_color(tokens.content.secondary)
        .cursor_pointer()
        .hover(move |button| button.bg(tokens.action.control_hover))
        .active(move |button| button.bg(tokens.action.control_pressed))
        .on_click(move |event, window, cx| action(event, window, cx))
        .child(Icon::new(icon).size(px(tokens.components.common.icon_large)))
}

fn new_tab_button(
    tokens: ThemeTokens,
    id: &'static str,
    action: TabBarAction,
    size: f32,
    icon: IconName,
) -> impl IntoElement {
    div()
        .id(id)
        .size(px(size))
        .flex()
        .items_center()
        .justify_center()
        .design_corners(tokens.components.corners.button)
        .text_color(tokens.content.secondary)
        .cursor_pointer()
        .hover(move |button| button.bg(tokens.action.control_hover))
        .active(move |button| button.bg(tokens.action.control_pressed))
        .on_click(move |event, window, cx| action(event, window, cx))
        .child(Icon::new(icon).size(px(tokens.components.common.icon_medium)))
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
            .w(px(self.tokens.components.popover.tab_search_width))
            .max_h(px(self.tokens.components.popover.tab_search_max_height))
            .overflow_hidden()
            .design_corners(self.tokens.components.corners.popover)
            .border_1()
            .border_color(self.tokens.materials.floating.border)
            .design_elevation(ElevationRole::Floating, &self.tokens)
            .bg(self.tokens.materials.floating.background)
            .text_color(self.tokens.content.primary)
            .flex()
            .flex_col()
            .child(
                div()
                    .h(px(self.tokens.components.popover.row_height))
                    .flex_none()
                    .px_3()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(
                        Icon::new(semantic_icon(self.tokens.icons.search))
                            .size(px(self.tokens.components.common.icon_medium))
                            .text_color(self.tokens.action.accent),
                    )
                    .child(
                        div()
                            .flex_1()
                            .design_typography(TypographyRole::Heading, &self.tokens)
                            .child("Open tabs"),
                    )
                    .child(
                        div()
                            .min_w(px(self.tokens.components.common.separator_length))
                            .h(px(self.tokens.components.common.row_compact - 6.0))
                            .px_2()
                            .design_corners(self.tokens.components.corners.context_pill)
                            .flex()
                            .items_center()
                            .justify_center()
                            .bg(self.tokens.surface.muted)
                            .design_typography(TypographyRole::Caption, &self.tokens)
                            .text_color(self.tokens.content.secondary)
                            .child(count),
                    ),
            )
            .child(
                div()
                    .h(px(self.tokens.components.common.separator_width))
                    .bg(self.tokens.materials.floating.border),
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
    use super::{TabPresentation, tab_hover_card_x, tab_hover_card_y, tab_search_popover_x};

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
        assert_eq!(tab_search_popover_x(1_200.0, 104.0, 380.0, 8.0), 104.0);
        assert_eq!(tab_search_popover_x(390.0, 104.0, 380.0, 8.0), 8.0);
    }

    #[test]
    fn hover_card_centers_under_tabs_and_clamps_to_edges() {
        assert_eq!(tab_hover_card_y(52.0, 34.0, 4.0), 47.0);
        assert_eq!(tab_hover_card_x(400.0, 200.0, 1_200.0, 282.0, 8.0), 359.0);
        assert_eq!(tab_hover_card_x(2.0, 120.0, 800.0, 282.0, 8.0), 8.0);
        assert_eq!(tab_hover_card_x(760.0, 120.0, 800.0, 282.0, 8.0), 510.0);
    }
}
