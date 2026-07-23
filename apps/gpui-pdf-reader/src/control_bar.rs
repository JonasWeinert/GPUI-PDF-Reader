//! Host-rendered projection of the command-active workspace view.

use crate::reader::control_bar::PdfControlBarSignature;
use crate::reader::{PdfReader, PdfReaderEvent};
use crate::text_field::{TextField, TextFieldEvent};
use gpui::{
    App, AppContext, Context, Entity, Focusable, FontWeight, IntoElement, Render, ScrollHandle,
    SharedString, StyledText, TextRun, Window, div, font, linear_color_stop, linear_gradient,
    point, prelude::*, px,
};
use gpui_component::{Icon, IconName, Theme};
use key_ui_gpui::{
    ChromeButtonStyle, ControlBarDisplayMode, DesignStyled as _, ElevationRole, IconRoleConfig,
    RadiusRole, ThemeTokens, TypographyRole, UnitTransition, WorkspaceContextBar, chrome_button,
    resolved_design_system, semantic_icon, solve_control_bar_layout,
};
use key_workspace_core::{
    ControlBarAuxiliary, ControlBarCard, ControlBarEvent, ControlBarInteraction, ControlBarItem,
    ControlBarItemKind, ControlBarRegion, ControlBarSnapshot, ControlIcon, WorkspaceViewDescriptor,
};
use std::time::Instant;

enum ControlBarProvider {
    Pdf(Entity<PdfReader>),
    Settings(WorkspaceViewDescriptor),
}

/// One lightweight chrome entity per workspace view. It observes the heavy
/// content entity but only repaints when its immutable projection changes.
pub(crate) struct ViewControlBar {
    provider: ControlBarProvider,
    snapshot: ControlBarSnapshot,
    pdf_signature: Option<PdfControlBarSignature>,
    search_field: Option<Entity<TextField>>,
    search_expanded: bool,
    search_reveal: UnitTransition,
    title_expanded: bool,
    title_reveal: UnitTransition,
    result_scroll: ScrollHandle,
    result_scroll_reveal: UnitTransition,
    result_scroll_start_x: f32,
    result_scroll_target_x: f32,
    animation_frame_queued: bool,
    last_animation_tick: Instant,
}

impl ViewControlBar {
    pub(crate) fn current_height(&self, cx: &App) -> f32 {
        let metrics = ThemeTokens::from_app(cx).components.control_bar;
        metrics.primary_height
            + (metrics.auxiliary_height * self.search_reveal.value())
                .max(metrics.title_auxiliary_height * self.title_reveal.value())
    }

    #[cfg(debug_assertions)]
    pub(crate) fn qa_open_search(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.set_search_expanded(true, window, cx);
    }

    #[cfg(debug_assertions)]
    pub(crate) fn qa_close_search(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.set_search_expanded(false, window, cx);
    }

    #[cfg(debug_assertions)]
    pub(crate) fn qa_set_search_query(&mut self, query: &str, cx: &mut Context<Self>) {
        if let Some(field) = &self.search_field {
            let _ = field.update(cx, |field, cx| field.set_text(query, cx));
        }
    }

    #[cfg(debug_assertions)]
    pub(crate) fn qa_state(
        &self,
        cx: &App,
    ) -> (key_workspace_core::ViewId, bool, usize, bool, f32) {
        let (results, complete) = self
            .snapshot
            .auxiliary
            .as_ref()
            .map_or((0, true), |auxiliary| {
                (auxiliary.cards.len(), !auxiliary.loading)
            });
        (
            self.snapshot.owner,
            self.search_expanded,
            results,
            complete,
            self.current_height(cx),
        )
    }

    pub(crate) fn new_pdf(
        reader: Entity<PdfReader>,
        window: &mut Window,
        cx: &mut App,
    ) -> Entity<Self> {
        let search_field = reader.read(cx).control_bar_search_field();
        let snapshot = reader
            .read(cx)
            .control_bar_snapshot(false, Theme::global(cx).is_dark());
        let pdf_signature = Some(
            reader
                .read(cx)
                .control_bar_signature(false, Theme::global(cx).is_dark()),
        );
        let reader_for_observer = reader.clone();
        let search_for_events = search_field.clone();
        cx.new(|cx| {
            cx.observe(
                &reader_for_observer,
                |bar: &mut ViewControlBar, reader, cx| {
                    let signature = reader
                        .read(cx)
                        .control_bar_signature(bar.search_expanded, Theme::global(cx).is_dark());
                    if bar.pdf_signature != Some(signature) {
                        bar.pdf_signature = Some(signature);
                        bar.snapshot = reader
                            .read(cx)
                            .control_bar_snapshot(bar.search_expanded, Theme::global(cx).is_dark());
                        cx.notify();
                    }
                },
            )
            .detach();
            cx.subscribe_in(
                &reader_for_observer,
                window,
                |bar: &mut ViewControlBar, _, event, window, cx| match event {
                    PdfReaderEvent::OpenSearch => bar.set_search_expanded(true, window, cx),
                },
            )
            .detach();
            cx.subscribe_in(
                &search_for_events,
                window,
                |bar: &mut ViewControlBar, _, event, window, cx| {
                    if matches!(event, TextFieldEvent::Cancel) {
                        bar.set_search_expanded(false, window, cx);
                    }
                },
            )
            .detach();
            Self {
                provider: ControlBarProvider::Pdf(reader),
                snapshot,
                pdf_signature,
                search_field: Some(search_field),
                search_expanded: false,
                search_reveal: UnitTransition::hidden(),
                title_expanded: false,
                title_reveal: UnitTransition::hidden(),
                result_scroll: ScrollHandle::new(),
                result_scroll_reveal: UnitTransition::hidden(),
                result_scroll_start_x: 0.0,
                result_scroll_target_x: 0.0,
                animation_frame_queued: false,
                last_animation_tick: Instant::now(),
            }
        })
    }

    pub(crate) fn new_settings(view: WorkspaceViewDescriptor, cx: &mut App) -> Entity<Self> {
        let snapshot = settings_snapshot(&view);
        cx.new(|_| Self {
            provider: ControlBarProvider::Settings(view),
            snapshot,
            pdf_signature: None,
            search_field: None,
            search_expanded: false,
            search_reveal: UnitTransition::hidden(),
            title_expanded: false,
            title_reveal: UnitTransition::hidden(),
            result_scroll: ScrollHandle::new(),
            result_scroll_reveal: UnitTransition::hidden(),
            result_scroll_start_x: 0.0,
            result_scroll_target_x: 0.0,
            animation_frame_queued: false,
            last_animation_tick: Instant::now(),
        })
    }

    fn refresh_snapshot(&mut self, cx: &App) {
        self.snapshot = match &self.provider {
            ControlBarProvider::Pdf(reader) => {
                self.pdf_signature = Some(
                    reader
                        .read(cx)
                        .control_bar_signature(self.search_expanded, Theme::global(cx).is_dark()),
                );
                reader
                    .read(cx)
                    .control_bar_snapshot(self.search_expanded, Theme::global(cx).is_dark())
            }
            ControlBarProvider::Settings(view) => settings_snapshot(view),
        };
    }

    fn set_search_expanded(&mut self, expanded: bool, window: &mut Window, cx: &mut Context<Self>) {
        if self.search_expanded == expanded {
            if expanded && let Some(field) = &self.search_field {
                window.focus(&field.read(cx).focus_handle(cx));
            }
            return;
        }
        self.search_expanded = expanded;
        if let Some(field) = &self.search_field {
            let _ = field.update(cx, |field, cx| field.set_borderless(expanded, cx));
        }
        if expanded {
            self.title_expanded = false;
            self.title_reveal.set_visible(false);
        }
        self.search_reveal.set_visible(expanded);
        self.last_animation_tick = Instant::now();
        if expanded {
            if let Some(field) = &self.search_field {
                window.focus(&field.read(cx).focus_handle(cx));
            }
        } else if let ControlBarProvider::Pdf(reader) = &self.provider {
            reader.update(cx, |reader, cx| reader.control_bar_close_search(cx));
        }
        self.refresh_snapshot(cx);
        self.queue_animation_frame(window, cx);
        cx.notify();
    }

    fn set_title_expanded(&mut self, expanded: bool, window: &mut Window, cx: &mut Context<Self>) {
        if self.title_expanded == expanded {
            return;
        }
        self.title_expanded = expanded;
        self.title_reveal.set_visible(expanded);
        if expanded && self.search_expanded {
            self.search_expanded = false;
            self.search_reveal.set_visible(false);
            if let Some(field) = &self.search_field {
                let _ = field.update(cx, |field, cx| field.set_borderless(false, cx));
            }
            if let ControlBarProvider::Pdf(reader) = &self.provider {
                reader.update(cx, |reader, cx| reader.control_bar_close_search(cx));
            }
        }
        self.last_animation_tick = Instant::now();
        self.refresh_snapshot(cx);
        self.queue_animation_frame(window, cx);
        cx.notify();
    }

    fn queue_animation_frame(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.animation_frame_queued {
            return;
        }
        self.animation_frame_queued = true;
        let weak = cx.weak_entity();
        window.on_next_frame(move |window, cx| {
            weak.update(cx, |bar, cx| bar.advance_animation(window, cx))
                .ok();
        });
    }

    fn advance_animation(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.animation_frame_queued = false;
        let now = Instant::now();
        let elapsed = now.duration_since(self.last_animation_tick).as_secs_f32();
        self.last_animation_tick = now;
        let motion = resolved_design_system(cx).motion;
        let animating = self
            .search_reveal
            .advance_with_optional_response(elapsed, motion.fast_response)
            || self
                .title_reveal
                .advance_with_optional_response(elapsed, motion.fast_response)
            || self
                .result_scroll_reveal
                .advance_with_optional_response(elapsed, motion.gentle_response);
        if self.result_scroll_reveal.is_animating() || self.result_scroll_reveal.value() > 0.0 {
            let current = self.result_scroll.offset();
            let x = self.result_scroll_start_x
                + (self.result_scroll_target_x - self.result_scroll_start_x)
                    * self.result_scroll_reveal.value();
            self.result_scroll.set_offset(point(px(x), current.y));
        }
        cx.notify();
        if animating {
            self.queue_animation_frame(window, cx);
        }
    }

    fn press(&mut self, control: &str, window: &mut Window, cx: &mut Context<Self>) {
        if control == crate::reader::control_bar::PDF_CONTROL_TITLE {
            self.set_title_expanded(!self.title_expanded, window, cx);
            return;
        }
        if control == crate::reader::control_bar::PDF_CONTROL_SEARCH {
            self.set_search_expanded(!self.search_expanded, window, cx);
            return;
        }
        if let ControlBarProvider::Pdf(reader) = &self.provider {
            let event = ControlBarEvent {
                owner: self.snapshot.owner,
                revision: self.snapshot.revision,
                control: control.to_owned().into(),
                interaction: ControlBarInteraction::Pressed,
            };
            reader.update(cx, |reader, cx| reader.control_bar_event(event, window, cx));
        }
    }

    fn activate_card(&mut self, control: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.center_search_card(control);
        if let ControlBarProvider::Pdf(reader) = &self.provider {
            let event = ControlBarEvent {
                owner: self.snapshot.owner,
                revision: self.snapshot.revision,
                control: control.to_owned().into(),
                interaction: ControlBarInteraction::ActivatedCard,
            };
            reader.update(cx, |reader, cx| reader.control_bar_event(event, window, cx));
        }
    }

    fn center_search_card(&mut self, control: &str) {
        let Some(index) = self.snapshot.auxiliary.as_ref().and_then(|auxiliary| {
            auxiliary
                .cards
                .iter()
                .position(|card| card.id.as_str() == control)
        }) else {
            return;
        };
        let (Some(card), bounds) = (
            self.result_scroll.bounds_for_item(index),
            self.result_scroll.bounds(),
        ) else {
            return;
        };
        let offset = self.result_scroll.offset();
        let desired = f32::from(offset.x) + f32::from(bounds.center().x - card.center().x);
        let maximum = f32::from(self.result_scroll.max_offset().width);
        let target = desired.clamp(-maximum, 0.0);
        if (target - f32::from(offset.x)).abs() < 1.0 {
            return;
        }
        self.result_scroll_start_x = f32::from(offset.x);
        self.result_scroll_target_x = target;
        self.result_scroll_reveal.snap_to(0.0);
        self.result_scroll_reveal.set_visible(true);
    }

    fn render_item(
        &self,
        item: &ControlBarItem,
        mode: ControlBarDisplayMode,
        width: f32,
        tokens: ThemeTokens,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let icons = tokens.icons;
        let id = item.id.as_str().to_owned();
        if item.kind == ControlBarItemKind::TextInput {
            let reveal = self.search_reveal.value();
            let field = self.search_field.clone();
            let weak = cx.weak_entity();
            return div()
                .id(SharedString::from(format!("control-input-{id}")))
                .relative()
                .h(px(tokens.components.control_bar.search_height))
                .w(px(width.max(tokens.components.common.control_small)))
                .min_w(px(tokens.components.common.control_small))
                .px_3()
                .flex_none()
                .flex()
                .items_center()
                .gap_2()
                .overflow_hidden()
                .design_radius(RadiusRole::Pill, &tokens)
                .border_1()
                .border_color(tokens.materials.control.border)
                .bg(tokens.materials.control.background)
                .child(
                    div()
                        .w(px(tokens.components.common.icon_large))
                        .flex_none()
                        .flex()
                        .items_center()
                        .justify_center()
                        .text_color(tokens.content.tertiary)
                        .child(
                            Icon::new(semantic_icon(icons.search))
                                .size(px(tokens.components.common.icon_medium)),
                        ),
                )
                .when_some(field, |input, field| {
                    input.child(div().min_w_0().flex_1().opacity(reveal).child(field))
                })
                .child(
                    div()
                        .id("control-search-close")
                        .absolute()
                        .top_0()
                        .bottom_0()
                        .right_0()
                        .w(px(tokens.components.control_bar.search_close_fade_width))
                        .flex_none()
                        .flex()
                        .items_center()
                        .justify_end()
                        .pr_1()
                        .bg(linear_gradient(
                            90.0,
                            linear_color_stop(tokens.surface.muted.opacity(0.0), 0.0),
                            linear_color_stop(tokens.surface.muted.opacity(0.72), 0.42),
                        ))
                        .child(
                            div()
                                .id("control-search-close")
                                .size(px(tokens.components.common.control_small - 1.0))
                                .flex()
                                .items_center()
                                .justify_center()
                                .design_radius(RadiusRole::Pill, &tokens)
                                .cursor_pointer()
                                .opacity(reveal)
                                .text_color(tokens.content.secondary)
                                .hover(move |button| button.bg(tokens.action.control_hover))
                                .on_click(move |_, window, cx| {
                                    weak.update(cx, |bar, cx| {
                                        bar.set_search_expanded(false, window, cx)
                                    })
                                    .ok();
                                })
                                .child(
                                    Icon::new(semantic_icon(icons.close))
                                        .size(px(tokens.components.common.icon_medium)),
                                ),
                        ),
                )
                .into_any_element();
        }

        let label = control_label(item, mode);
        if item.kind == ControlBarItemKind::Display {
            let icon = control_icon(item, mode, tokens);
            let label = label.map(|label| {
                div()
                    .min_w_0()
                    .flex_1()
                    .overflow_hidden()
                    .text_ellipsis()
                    .whitespace_nowrap()
                    .child(label)
                    .into_any_element()
            });
            let is_title = id == crate::reader::control_bar::PDF_CONTROL_TITLE;
            let weak = cx.weak_entity();
            return div()
                .id(SharedString::from(format!("control-display-{id}")))
                .h(px(tokens.components.common.control_small + 4.0))
                .w(px(width))
                .min_w_0()
                .flex_none()
                .px_2()
                .flex()
                .items_center()
                .justify_start()
                .gap_1()
                .overflow_hidden()
                .design_radius(RadiusRole::Medium, &tokens)
                .bg(if is_title && self.title_expanded {
                    tokens.action.accent_soft
                } else {
                    gpui::transparent_black()
                })
                .design_typography(TypographyRole::Label, &tokens)
                .text_color(if item.state.enabled {
                    tokens.content.secondary
                } else {
                    tokens.content.tertiary
                })
                .children(icon)
                .children(label)
                .when(is_title && item.state.enabled, |title| {
                    title
                        .cursor_pointer()
                        .hover(move |title| title.bg(tokens.action.control_hover))
                        .on_click(move |_, window, cx| {
                            weak.update(cx, |bar, cx| {
                                bar.set_title_expanded(!bar.title_expanded, window, cx)
                            })
                            .ok();
                        })
                })
                .into_any_element();
        }

        let weak = cx.weak_entity();
        let style = if item.state.selected {
            ChromeButtonStyle::SubtleSelected
        } else {
            ChromeButtonStyle::Ghost
        };
        div()
            .w(px(width))
            .flex_none()
            .overflow_hidden()
            .child(chrome_button(
                tokens,
                SharedString::from(format!("control-button-{id}")),
                div()
                    .flex()
                    .items_center()
                    .justify_center()
                    .gap_1()
                    .children(control_icon(item, mode, tokens))
                    .children(label),
                style,
                item.state.enabled,
                move |_, window, cx| {
                    weak.update(cx, |bar, cx| bar.press(&id, window, cx)).ok();
                },
            ))
            .into_any_element()
    }

    fn render_auxiliary(
        &self,
        auxiliary: &ControlBarAuxiliary,
        tokens: ThemeTokens,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let cards = auxiliary
            .cards
            .iter()
            .map(|card| self.render_card(card, tokens, cx))
            .collect::<Vec<_>>();
        let label: SharedString = auxiliary.label.clone().into();
        let empty = auxiliary.cards.is_empty().then(|| {
            div()
                .px_4()
                .design_typography(TypographyRole::Body, &tokens)
                .text_color(tokens.content.tertiary)
                .child(if auxiliary.loading {
                    "Results will appear as pages are searched"
                } else {
                    "No matching text in this document"
                })
                .into_any_element()
        });
        div()
            .size_full()
            .px_3()
            .pb_2()
            .flex()
            .items_center()
            .gap_3()
            .border_t_1()
            .border_color(tokens.materials.surface.border)
            .bg(tokens.materials.surface.background)
            .text_color(tokens.content.primary)
            .child(
                div()
                    .w(px(tokens.components.control_bar.result_label_width))
                    .flex_none()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .design_typography(TypographyRole::Caption, &tokens)
                    .text_color(tokens.content.secondary)
                    .child(label)
                    .when(auxiliary.loading, |status| {
                        status.child(
                            div()
                                .w(px(tokens.components.common.separator_length * 1.75))
                                .h(px(tokens.geometry.border_width * 2.0))
                                .design_radius(RadiusRole::Pill, &tokens)
                                .bg(tokens.action.accent.opacity(0.72)),
                        )
                    }),
            )
            .child(
                div()
                    .id("control-results-scroll")
                    .h_full()
                    .min_w_0()
                    .flex_1()
                    .overflow_x_scroll()
                    .track_scroll(&self.result_scroll)
                    .flex()
                    .items_center()
                    .gap_2()
                    .children(cards)
                    .children(empty),
            )
            .into_any_element()
    }

    fn render_title_auxiliary(
        &self,
        tokens: ThemeTokens,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let metadata = match &self.provider {
            ControlBarProvider::Pdf(reader) => reader.read(cx).control_bar_metadata(),
            ControlBarProvider::Settings(_) => None,
        };
        let weak = cx.weak_entity();
        div()
            .size_full()
            .px_4()
            .py_3()
            .flex()
            .flex_col()
            .gap_2()
            .border_t_1()
            .border_color(tokens.materials.surface.border)
            .bg(tokens.materials.surface.background)
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .child(
                        div()
                            .design_typography(TypographyRole::Heading, &tokens)
                            .child("PDF details"),
                    )
                    .child(
                        div()
                            .id("control-title-collapse")
                            .h(px(tokens.components.common.control_small))
                            .px_2()
                            .flex()
                            .items_center()
                            .gap_1()
                            .design_radius(RadiusRole::Medium, &tokens)
                            .cursor_pointer()
                            .design_typography(TypographyRole::Label, &tokens)
                            .text_color(tokens.content.secondary)
                            .hover(move |button| button.bg(tokens.action.control_hover))
                            .on_click(move |_, window, cx| {
                                weak.update(cx, |bar, cx| {
                                    bar.set_title_expanded(false, window, cx)
                                })
                                .ok();
                            })
                            .child(
                                Icon::new(semantic_icon(tokens.icons.collapse))
                                    .size(px(tokens.components.common.icon_medium)),
                            )
                            .child("Collapse"),
                    ),
            )
            .when_some(metadata, |panel, metadata| {
                panel
                    .child(
                        div()
                            .design_typography(TypographyRole::Heading, &tokens)
                            .child(metadata.title),
                    )
                    .child(
                        div()
                            .overflow_hidden()
                            .text_ellipsis()
                            .whitespace_nowrap()
                            .design_typography(TypographyRole::Body, &tokens)
                            .text_color(tokens.content.secondary)
                            .child(metadata.path),
                    )
                    .child(
                        div()
                            .flex()
                            .gap_4()
                            .design_typography(TypographyRole::Body, &tokens)
                            .text_color(tokens.content.secondary)
                            .child(format!(
                                "Page {} of {}",
                                metadata.current_page, metadata.page_count
                            ))
                            .child(format!("{}% zoom", metadata.zoom_percent)),
                    )
            })
            .into_any_element()
    }

    fn render_card(
        &self,
        card: &ControlBarCard,
        tokens: ThemeTokens,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let id = card.id.as_str().to_owned();
        let weak = cx.weak_entity();
        let preview = emphasized_text(card, tokens);
        div()
            .id(SharedString::from(format!("control-card-{id}")))
            .h(px(tokens.components.control_bar.result_card_height))
            .w(px(tokens.components.control_bar.result_card_width))
            .flex_none()
            .px_3()
            .py_2()
            .flex()
            .flex_col()
            .gap_1()
            .overflow_hidden()
            .design_radius(RadiusRole::Large, &tokens)
            .border_1()
            .border_color(if card.selected {
                tokens.action.accent_border
            } else {
                tokens.surface.border
            })
            .bg(if card.selected {
                tokens.action.accent_soft
            } else {
                tokens.surface.overlay
            })
            .design_elevation(ElevationRole::Surface, &tokens)
            .cursor_pointer()
            .hover(move |card| card.bg(tokens.action.accent_soft_hover))
            .on_click(move |_, window, cx| {
                weak.update(cx, |bar, cx| bar.activate_card(&id, window, cx))
                    .ok();
            })
            .when_some(card.eyebrow.clone(), |card, eyebrow| {
                card.child(
                    div()
                        .design_typography(TypographyRole::Heading, &tokens)
                        .text_color(tokens.action.accent)
                        .child(eyebrow),
                )
            })
            .child(
                div()
                    .h(px(tokens.components.control_bar.search_height))
                    .overflow_hidden()
                    .design_typography(TypographyRole::Body, &tokens)
                    .child(preview),
            )
            .into_any_element()
    }
}

impl Render for ViewControlBar {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // Theme changes are global rather than reader notifications.
        if let ControlBarProvider::Pdf(reader) = &self.provider {
            let signature = reader
                .read(cx)
                .control_bar_signature(self.search_expanded, Theme::global(cx).is_dark());
            if self.pdf_signature != Some(signature) {
                self.pdf_signature = Some(signature);
                self.snapshot = reader
                    .read(cx)
                    .control_bar_snapshot(self.search_expanded, Theme::global(cx).is_dark());
            }
        }

        let tokens = ThemeTokens::from_app(cx);
        let system = resolved_design_system(cx);
        let viewport_width = f32::from(window.viewport_size().width);
        let chrome = system
            .workspace
            .chrome
            .resolve(system.responsive.classify(viewport_width));
        let available = (viewport_width
            - tokens.components.control_bar.host_reserved_width
            - chrome.control_leading_inset)
            .max(tokens.components.popover.tab_hover_width * 0.57);
        // Use the exact same reveal value for the search pill and the layout
        // solver. That keeps the flexible title, all trailing controls, and
        // the auxiliary row moving on one clock instead of snapping to the
        // search field's final width.
        let mut items = self.snapshot.items.clone();
        let search_reveal = self.search_reveal.value();
        if self.search_expanded || search_reveal > 0.0 {
            if let Some(search) = items
                .iter_mut()
                .find(|item| item.id.as_str() == crate::reader::control_bar::PDF_CONTROL_SEARCH)
            {
                search.kind = ControlBarItemKind::TextInput;
                search.state.expanded = true;
                search.state.selected = true;
                for (width, expanded_width) in search
                    .presentation
                    .widths
                    .iter_mut()
                    .zip(crate::reader::control_bar::PDF_SEARCH_EXPANDED_WIDTHS)
                {
                    let compact = tokens.components.common.control_small;
                    *width = compact + (expanded_width - compact) * search_reveal;
                }
            }
        }
        let layout =
            solve_control_bar_layout(&items, available, tokens.components.control_bar.item_gap);
        let mut leading = Vec::new();
        let mut center = Vec::new();
        let mut trailing = Vec::new();
        for (item, layout) in items.iter().zip(layout) {
            if !item.state.visible || layout.width <= 0.0 {
                continue;
            }
            let rendered = self.render_item(item, layout.mode, layout.width, tokens, cx);
            match item.region {
                ControlBarRegion::Leading => leading.push(rendered),
                ControlBarRegion::Center => center.push(rendered),
                ControlBarRegion::Trailing => trailing.push(rendered),
            }
        }
        let title_reveal = self.title_reveal.value();
        let auxiliary_height = (tokens.components.control_bar.auxiliary_height * search_reveal)
            .max(tokens.components.control_bar.title_auxiliary_height * title_reveal);
        let auxiliary = if self.search_expanded {
            self.snapshot
                .auxiliary
                .as_ref()
                .map(|auxiliary| self.render_auxiliary(auxiliary, tokens, cx))
        } else if self.title_expanded {
            Some(self.render_title_auxiliary(tokens, cx))
        } else if search_reveal > 0.0 {
            self.snapshot
                .auxiliary
                .as_ref()
                .map(|auxiliary| self.render_auxiliary(auxiliary, tokens, cx))
        } else if title_reveal > 0.0 {
            Some(self.render_title_auxiliary(tokens, cx))
        } else {
            None
        };
        WorkspaceContextBar::new(tokens)
            .leading(
                div()
                    .flex()
                    .items_center()
                    .gap_1()
                    .child(div().w(px(36.0)).flex_none())
                    .children(leading),
            )
            .center(
                div()
                    .min_w_0()
                    .flex_1()
                    .flex()
                    .items_center()
                    .justify_start()
                    .children(center),
            )
            .trailing(div().flex().items_center().gap_1().children(trailing))
            .when_some(auxiliary, |bar, auxiliary| {
                bar.auxiliary(auxiliary_height, auxiliary)
            })
    }
}

fn settings_snapshot(view: &WorkspaceViewDescriptor) -> ControlBarSnapshot {
    use key_workspace_core::{ControlBarItem, ControlBarPresentation};
    let mut snapshot = ControlBarSnapshot::new(view.id, view.generation.get());
    snapshot.items.push(ControlBarItem::new(
        "settings.title",
        ControlBarRegion::Center,
        ControlBarItemKind::Display,
        ControlBarPresentation::new(view.title.clone(), [300.0, 150.0, 32.0], 90)
            .short_label("Settings")
            .icon(ControlIcon::Settings),
    ));
    snapshot
}

fn control_icon(
    item: &ControlBarItem,
    mode: ControlBarDisplayMode,
    tokens: ThemeTokens,
) -> Option<gpui::AnyElement> {
    let icon = item.presentation.icon?;
    let show = mode == ControlBarDisplayMode::Icon
        || item.kind == ControlBarItemKind::Button
        || item.kind == ControlBarItemKind::Display;
    show.then(|| {
        Icon::new(icon_name(icon, tokens.icons))
            .size(px(tokens.components.common.icon_medium))
            .into_any_element()
    })
}

fn control_label(item: &ControlBarItem, mode: ControlBarDisplayMode) -> Option<SharedString> {
    if item.presentation.icon_only {
        return None;
    }
    match mode {
        ControlBarDisplayMode::Full => Some(item.presentation.label.clone().into()),
        ControlBarDisplayMode::Compact => Some(
            item.presentation
                .short_label
                .as_ref()
                .unwrap_or(&item.presentation.label)
                .clone()
                .into(),
        ),
        ControlBarDisplayMode::Icon if item.presentation.icon.is_some() => None,
        ControlBarDisplayMode::Icon => Some(item.presentation.label.clone().into()),
    }
}

fn icon_name(icon: ControlIcon, icons: IconRoleConfig) -> IconName {
    match icon {
        ControlIcon::Add => semantic_icon(icons.new_tab),
        ControlIcon::Close => semantic_icon(icons.close),
        ControlIcon::Comments => semantic_icon(icons.comments),
        ControlIcon::Document => semantic_icon(icons.document),
        ControlIcon::FitWidth => semantic_icon(icons.fit_width),
        ControlIcon::Minus => semantic_icon(icons.zoom_out),
        ControlIcon::Moon => semantic_icon(icons.theme_dark),
        ControlIcon::Next => semantic_icon(icons.next),
        ControlIcon::Previous => semantic_icon(icons.previous),
        ControlIcon::Search => semantic_icon(icons.search),
        ControlIcon::Settings => semantic_icon(icons.settings),
        ControlIcon::Split => semantic_icon(icons.split),
        ControlIcon::Sun => semantic_icon(icons.theme_light),
    }
}

fn emphasized_text(card: &ControlBarCard, tokens: ThemeTokens) -> StyledText {
    let mut runs = Vec::new();
    if let Some(range) = card
        .emphasized
        .clone()
        .filter(|range| range.start <= range.end && range.end <= card.text.len())
    {
        for (span, weight) in [
            (0..range.start, FontWeight::NORMAL),
            (range.clone(), FontWeight::BOLD),
            (range.end..card.text.len(), FontWeight::NORMAL),
        ] {
            if span.is_empty() {
                continue;
            }
            let mut selected_font = font(".SystemUIFont");
            selected_font.weight = weight;
            runs.push(TextRun {
                len: span.len(),
                font: selected_font,
                color: tokens.content.primary,
                background_color: None,
                underline: None,
                strikethrough: None,
            });
        }
    }
    if runs.is_empty() {
        let mut selected_font = font(".SystemUIFont");
        selected_font.weight = FontWeight::NORMAL;
        runs.push(TextRun {
            len: card.text.len(),
            font: selected_font,
            color: tokens.content.primary,
            background_color: None,
            underline: None,
            strikethrough: None,
        });
    }
    StyledText::new(card.text.clone()).with_runs(runs)
}
