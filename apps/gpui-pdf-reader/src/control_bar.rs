//! Host-rendered projection of the command-active workspace view.

use crate::reader::control_bar::PdfControlBarSignature;
use crate::reader::{PdfReader, PdfReaderEvent};
use crate::text_field::{TextField, TextFieldEvent};
use gpui::{
    App, AppContext, Context, Entity, Focusable, FontWeight, IntoElement, Render, ScrollHandle,
    SharedString, StyledText, TextRun, Window, div, font, prelude::*, px,
};
use gpui_component::{Icon, IconName, Theme};
use key_ui_gpui::{
    ChromeButtonStyle, ControlBarDisplayMode, ThemeTokens, UnitTransition, WorkspaceContextBar,
    chrome_button, solve_control_bar_layout,
};
use key_workspace_core::{
    ControlBarAuxiliary, ControlBarCard, ControlBarEvent, ControlBarInteraction, ControlBarItem,
    ControlBarItemKind, ControlBarRegion, ControlBarSnapshot, ControlIcon, WorkspaceViewDescriptor,
};
use std::time::Instant;

const CONTROL_GAP: f32 = 4.0;
const HOST_RESERVED_WIDTH: f32 = 72.0;
const AUXILIARY_HEIGHT: f32 = 94.0;

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
    result_scroll: ScrollHandle,
    animation_frame_queued: bool,
    last_animation_tick: Instant,
}

impl ViewControlBar {
    pub(crate) fn current_height(&self) -> f32 {
        key_ui_gpui::CONTEXT_BAR_HEIGHT + AUXILIARY_HEIGHT * self.search_reveal.value()
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
    pub(crate) fn qa_state(&self) -> (key_workspace_core::ViewId, bool, usize, bool, f32) {
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
            self.current_height(),
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
                result_scroll: ScrollHandle::new(),
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
            result_scroll: ScrollHandle::new(),
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
        let animating = self.search_reveal.advance_with_response(elapsed, 25.0);
        cx.notify();
        if animating {
            self.queue_animation_frame(window, cx);
        }
    }

    fn press(&mut self, control: &str, window: &mut Window, cx: &mut Context<Self>) {
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

    fn render_item(
        &self,
        item: &ControlBarItem,
        mode: ControlBarDisplayMode,
        width: f32,
        tokens: ThemeTokens,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let id = item.id.as_str().to_owned();
        if item.kind == ControlBarItemKind::TextInput {
            let reveal = self.search_reveal.value();
            let animated_width = 32.0 + (width.max(32.0) - 32.0) * reveal;
            let field = self.search_field.clone();
            let weak = cx.weak_entity();
            return div()
                .id(SharedString::from(format!("control-input-{id}")))
                .h(px(36.0))
                .w(px(animated_width))
                .min_w(px(32.0))
                .flex_none()
                .flex()
                .items_center()
                .overflow_hidden()
                .child(
                    div()
                        .w(px(28.0 * (1.0 - reveal)))
                        .flex_none()
                        .overflow_hidden()
                        .flex()
                        .items_center()
                        .justify_center()
                        .opacity(1.0 - reveal)
                        .text_color(tokens.content.tertiary)
                        .child(Icon::new(IconName::Search).size(px(15.0))),
                )
                .when_some(field, |input, field| {
                    input.child(div().min_w_0().flex_1().opacity(reveal).child(field))
                })
                .child(
                    div()
                        .id("control-search-close")
                        .ml_1()
                        .size(px(27.0))
                        .flex_none()
                        .flex()
                        .items_center()
                        .justify_center()
                        .rounded_md()
                        .cursor_pointer()
                        .opacity(reveal)
                        .text_color(tokens.content.secondary)
                        .hover(move |button| button.bg(tokens.action.control_hover))
                        .on_click(move |_, window, cx| {
                            weak.update(cx, |bar, cx| bar.set_search_expanded(false, window, cx))
                                .ok();
                        })
                        .child(Icon::new(IconName::Close).size(px(14.0))),
                )
                .into_any_element();
        }

        let label = control_label(item, mode);
        if item.kind == ControlBarItemKind::Display {
            return div()
                .id(SharedString::from(format!("control-display-{id}")))
                .h(px(32.0))
                .w(px(width))
                .min_w_0()
                .flex_none()
                .px_2()
                .flex()
                .items_center()
                .justify_center()
                .gap_1()
                .overflow_hidden()
                .rounded_md()
                .bg(tokens.surface.muted.opacity(0.68))
                .text_ellipsis()
                .whitespace_nowrap()
                .text_sm()
                .font_weight(FontWeight::MEDIUM)
                .text_color(if item.state.enabled {
                    tokens.content.secondary
                } else {
                    tokens.content.tertiary
                })
                .children(control_icon(item, mode))
                .children(label)
                .into_any_element();
        }

        let weak = cx.weak_entity();
        let style = if item.state.selected {
            ChromeButtonStyle::Selected
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
                    .children(control_icon(item, mode))
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
                .text_sm()
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
            .border_color(tokens.surface.border.opacity(0.58))
            .bg(tokens.surface.background)
            .child(
                div()
                    .w(px(116.0))
                    .flex_none()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .text_xs()
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(tokens.content.secondary)
                    .child(label)
                    .when(auxiliary.loading, |status| {
                        status.child(
                            div()
                                .w(px(42.0))
                                .h(px(2.0))
                                .rounded_full()
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
            .h(px(66.0))
            .w(px(270.0))
            .flex_none()
            .px_3()
            .py_2()
            .flex()
            .flex_col()
            .gap_1()
            .overflow_hidden()
            .rounded_lg()
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
            .shadow_sm()
            .cursor_pointer()
            .hover(move |card| card.bg(tokens.action.accent_soft_hover))
            .on_click(move |_, window, cx| {
                weak.update(cx, |bar, cx| bar.activate_card(&id, window, cx))
                    .ok();
            })
            .when_some(card.eyebrow.clone(), |card, eyebrow| {
                card.child(
                    div()
                        .text_xs()
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(tokens.action.accent)
                        .child(eyebrow),
                )
            })
            .child(
                div()
                    .h(px(34.0))
                    .overflow_hidden()
                    .text_sm()
                    .line_height(px(17.0))
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

        let tokens = ThemeTokens::from_theme(Theme::global(cx));
        let available = (f32::from(window.viewport_size().width) - HOST_RESERVED_WIDTH).max(160.0);
        let layout = solve_control_bar_layout(&self.snapshot.items, available, CONTROL_GAP);
        let mut leading = Vec::new();
        let mut center = Vec::new();
        let mut trailing = Vec::new();
        for (item, layout) in self.snapshot.items.iter().zip(layout) {
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
        let reveal = self.search_reveal.value();
        let auxiliary = self
            .snapshot
            .auxiliary
            .as_ref()
            .map(|auxiliary| self.render_auxiliary(auxiliary, tokens, cx));
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
                    .justify_center()
                    .children(center),
            )
            .trailing(div().flex().items_center().gap_1().children(trailing))
            .when_some(auxiliary, |bar, auxiliary| {
                bar.auxiliary(AUXILIARY_HEIGHT * reveal, auxiliary)
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

fn control_icon(item: &ControlBarItem, mode: ControlBarDisplayMode) -> Option<gpui::AnyElement> {
    let icon = item.presentation.icon?;
    let show = mode == ControlBarDisplayMode::Icon
        || item.kind == ControlBarItemKind::Button
        || item.kind == ControlBarItemKind::Display;
    show.then(|| Icon::new(icon_name(icon)).size(px(15.0)).into_any_element())
}

fn control_label(item: &ControlBarItem, mode: ControlBarDisplayMode) -> Option<SharedString> {
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

fn icon_name(icon: ControlIcon) -> IconName {
    match icon {
        ControlIcon::Add => IconName::Plus,
        ControlIcon::Close => IconName::Close,
        ControlIcon::Comments => IconName::PanelRight,
        ControlIcon::Document => IconName::File,
        ControlIcon::FitWidth => IconName::Maximize,
        ControlIcon::Minus => IconName::Minus,
        ControlIcon::Moon => IconName::Moon,
        ControlIcon::Next => IconName::ArrowDown,
        ControlIcon::Previous => IconName::ArrowUp,
        ControlIcon::Search => IconName::Search,
        ControlIcon::Settings => IconName::Settings,
        ControlIcon::Split => IconName::Frame,
        ControlIcon::Sun => IconName::Sun,
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
