use super::*;

const TOC_RAIL_WIDTH: f32 = 54.0;
const TOC_MARKER_LEFT: f32 = 8.0;
pub(super) const TOC_STACK_MARGIN: f32 = 22.0;
const TOC_STACK_SPACING: f32 = 12.0;
const TOC_CASCADE_RADIUS: f32 = 5.0;
pub(super) const TOC_CARD_MIN_HEIGHT: f32 = 82.0;
const TOC_BREADCRUMB_CHARACTER_BUDGET: usize = 46;
const TOC_BREADCRUMB_MIN_LABEL_CHARACTERS: usize = 8;
pub(super) const TOC_BREADCRUMB_MAX_LABEL_CHARACTERS: usize = 20;
const TOC_HOVER_LEAVE_DELAY: Duration = Duration::from_millis(120);
const MAX_TOC_HEADING_MATCHES: usize = 16;

#[derive(Clone, Debug, PartialEq)]
pub(super) struct TocTitleMatch {
    pub(super) y: f32,
    pub(super) text_runs: Vec<TextBounds>,
}

#[derive(Clone, Debug, PartialEq)]
pub(super) struct ResolvedTocDestination {
    pub(super) y: Option<f32>,
    pub(super) text_runs: Vec<TextBounds>,
    pub(super) matched_title: bool,
}

pub(super) fn toc_title_match(title: &str, text: &TextLayer) -> Option<TocTitleMatch> {
    let query = SearchQuery::new(title).ok()?;
    let SearchPageOutcome::Complete(results) =
        search_page(0, text.as_slice(), &query, MAX_TOC_HEADING_MATCHES, || {
            false
        })
    else {
        return None;
    };
    let mut best: Option<(f32, f32, Vec<TextBounds>)> = None;
    for result in results.matches {
        let Some(top) = result
            .highlight_runs
            .iter()
            .map(|bounds| bounds.top)
            .min_by(f32::total_cmp)
        else {
            continue;
        };
        let Some(height) = result
            .highlight_runs
            .iter()
            .map(|bounds| (bounds.bottom - bounds.top).max(0.0))
            .max_by(f32::total_cmp)
        else {
            continue;
        };
        if best.as_ref().is_none_or(|(best_height, best_top, _)| {
            height > *best_height + 0.0001
                || ((height - *best_height).abs() <= 0.0001 && top < *best_top)
        }) {
            best = Some((height, top, result.highlight_runs));
        }
    }
    best.map(|(_, top, text_runs)| TocTitleMatch {
        y: top.clamp(0.0, 1.0),
        text_runs,
    })
}

pub(super) fn resolve_toc_destination(
    title: &str,
    text: &TextLayer,
    explicit_destination_y: Option<f32>,
) -> ResolvedTocDestination {
    if let Some(matched) = toc_title_match(title, text) {
        ResolvedTocDestination {
            y: Some(matched.y),
            text_runs: matched.text_runs,
            matched_title: true,
        }
    } else {
        ResolvedTocDestination {
            y: explicit_destination_y,
            text_runs: Vec::new(),
            matched_title: false,
        }
    }
}

pub(super) fn toc_stack_geometry(viewport_height: f32, count: usize) -> Option<(f32, f32)> {
    if count == 0 || !viewport_height.is_finite() || viewport_height <= 0.0 {
        return None;
    }
    if count == 1 {
        return Some((viewport_height * 0.5, 0.0));
    }
    let available = (viewport_height - TOC_STACK_MARGIN * 2.0).max(1.0);
    let spacing = (available / (count - 1) as f32).min(TOC_STACK_SPACING);
    let height = spacing * (count - 1) as f32;
    Some(((viewport_height - height) * 0.5, spacing))
}

pub(super) fn toc_cascade_amount(index: usize, hover_position: f32, hover_strength: f32) -> f32 {
    if !hover_position.is_finite() || !hover_strength.is_finite() {
        return 0.0;
    }
    let distance = (index as f32 - hover_position).abs();
    (1.0 - distance / TOC_CASCADE_RADIUS).clamp(0.0, 1.0) * hover_strength.clamp(0.0, 1.0)
}

pub(super) fn toc_hover_state_is_animating(
    position: f32,
    strength: f32,
    target: Option<usize>,
) -> bool {
    let target_strength = if target.is_some() { 1.0 } else { 0.0 };
    let strength_is_animating = (target_strength - strength).abs() > 0.002;
    let position_is_animating = target.is_some_and(|index| (index as f32 - position).abs() > 0.002);
    strength_is_animating || position_is_animating
}

pub(super) fn advance_toc_hover_state(
    position: &mut f32,
    strength: &mut f32,
    target: Option<usize>,
    blend: f32,
) {
    if let Some(index) = target {
        *position += (index as f32 - *position) * blend;
    }
    let target_strength = if target.is_some() { 1.0 } else { 0.0 };
    *strength += (target_strength - *strength) * blend;
    if !toc_hover_state_is_animating(*position, *strength, target) {
        *strength = target_strength;
        if let Some(index) = target {
            *position = index as f32;
        }
    }
}

pub(super) fn active_toc_index(entries: &[TocEntry], current_page: usize) -> Option<usize> {
    entries
        .iter()
        .enumerate()
        .rev()
        .find_map(|(index, entry)| (entry.page <= current_page).then_some(index))
        .or((!entries.is_empty()).then_some(0))
}

pub(super) fn toc_breadcrumb_entries(
    entries: &[TocEntry],
    index: usize,
) -> Option<Vec<(usize, String)>> {
    let current = entries.get(index)?;
    let mut path = vec![(index, current.title.clone())];
    let mut expected_depth = current.depth;
    for (ancestor_index, entry) in entries[..index].iter().enumerate().rev() {
        let Some(parent_depth) = expected_depth.checked_sub(1) else {
            break;
        };
        if entry.depth == parent_depth {
            path.push((ancestor_index, entry.title.clone()));
            expected_depth = parent_depth;
        }
    }
    path.reverse();
    Some(path)
}

pub(super) fn end_truncate(text: &str, maximum_characters: usize) -> String {
    let character_count = text.chars().count();
    if character_count <= maximum_characters || maximum_characters < 2 {
        return text.to_owned();
    }
    let mut result = text
        .chars()
        .take(maximum_characters - 1)
        .collect::<String>();
    result.push('…');
    result
}

pub(super) fn toc_display_breadcrumbs(breadcrumbs: &[(usize, String)]) -> Vec<(usize, String)> {
    if breadcrumbs.is_empty() {
        return Vec::new();
    }
    let separators = breadcrumbs.len().saturating_sub(1) * 3;
    let label_budget = TOC_BREADCRUMB_CHARACTER_BUDGET
        .saturating_sub(separators)
        .checked_div(breadcrumbs.len())
        .unwrap_or(TOC_BREADCRUMB_MIN_LABEL_CHARACTERS)
        .clamp(
            TOC_BREADCRUMB_MIN_LABEL_CHARACTERS,
            TOC_BREADCRUMB_MAX_LABEL_CHARACTERS,
        );
    breadcrumbs
        .iter()
        .map(|(index, title)| (*index, end_truncate(title, label_budget)))
        .collect()
}

pub(super) fn toc_callout_height(breadcrumbs: &[(usize, String)], width: f32) -> f32 {
    let available_width = (width - 32.0).max(64.0);
    let characters_per_line = (available_width / 6.5).floor().max(8.0);
    let character_count = breadcrumbs
        .iter()
        .map(|(_, title)| title.chars().count())
        .sum::<usize>()
        .saturating_add(breadcrumbs.len().saturating_sub(1) * 3);
    let lines = ((character_count as f32 / characters_per_line).ceil() as usize).max(1);
    (64.0 + lines as f32 * 18.0).max(TOC_CARD_MIN_HEIGHT)
}

pub(super) fn toc_callout_width(breadcrumbs: &[(usize, String)], maximum_width: f32) -> f32 {
    let character_count = breadcrumbs
        .iter()
        .map(|(_, title)| title.chars().count())
        .sum::<usize>()
        .saturating_add(breadcrumbs.len().saturating_sub(1) * 3);
    (character_count as f32 * 6.5 + 42.0).clamp(190.0, maximum_width.max(190.0))
}

impl PdfReader {
    pub(super) fn set_toc_hovered(
        &mut self,
        index: usize,
        hovered: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if hovered {
            self.toc_hover_revision = self.toc_hover_revision.wrapping_add(1);
            if self.toc_hovered.is_none() && self.toc_hover_strength <= 0.002 {
                self.toc_hover_position = index as f32;
            }
            self.toc_hovered = Some(index);
            self.start_animation(window, cx);
        } else if self.toc_hovered == Some(index) {
            self.schedule_toc_hover_clear(window, cx);
        }
    }

    pub(super) fn set_toc_callout_hovered(
        &mut self,
        hovered: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if hovered {
            self.toc_hover_revision = self.toc_hover_revision.wrapping_add(1);
        } else {
            self.schedule_toc_hover_clear(window, cx);
        }
    }

    fn schedule_toc_hover_clear(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.toc_hover_revision = self.toc_hover_revision.wrapping_add(1);
        let revision = self.toc_hover_revision;
        let weak = cx.weak_entity();
        window
            .spawn(cx, async move |cx| {
                cx.background_executor().timer(TOC_HOVER_LEAVE_DELAY).await;
                let _ = cx.update(|window, cx| {
                    weak.update(cx, |reader, cx| {
                        if reader.toc_hover_revision == revision {
                            reader.toc_hovered = None;
                            reader.start_animation(window, cx);
                        }
                    })
                    .ok();
                });
            })
            .detach();
    }

    pub(super) fn navigate_toc(
        &mut self,
        index: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some((page, title, destination_y)) = self
            .document
            .as_ref()
            .and_then(|document| document.toc.get(index))
            .map(|entry| (entry.page, entry.title.clone(), entry.destination_y))
        else {
            return;
        };
        self.sidebar_anchor = None;
        self.selection = None;
        self.publish_pdf_selection();
        self.active_annotation = None;
        self.pending_toc_navigation = None;

        if self.page_text.contains_key(&page) {
            if self.navigate_toc_through_extension(&title, page, window, cx) {
                #[cfg(debug_assertions)]
                if self
                    .page_text
                    .get(&page)
                    .and_then(|text| toc_title_match(&title, text))
                    .is_some()
                {
                    self.qa_toc_text_matches += 1;
                }
                return;
            }
            let bridge = self.pdf_capabilities();
            if let Ok(receipt) = bridge.activate_live_outline(&title, page) {
                if receipt.destination.text_range.is_some() {
                    #[cfg(debug_assertions)]
                    {
                        self.qa_toc_text_matches += 1;
                    }
                    self.consume_pdf_extension_outputs(window, cx);
                    return;
                }
                let _ = bridge.take_pending_navigation();
            }
            let text = self
                .page_text
                .get(&page)
                .expect("page presence checked before capability navigation");
            let resolved = resolve_toc_destination(&title, text, destination_y);
            #[cfg(debug_assertions)]
            if resolved.matched_title {
                self.qa_toc_text_matches += 1;
            }
            self.scroll_to_toc_destination(page, resolved.y, resolved.text_runs, window, cx);
            return;
        }

        self.pending_toc_navigation = Some(index);
        if self.text_pending.insert(page) && !self.worker.extract_text(self.generation, page) {
            self.text_pending.remove(&page);
            self.pending_toc_navigation = None;
            self.scroll_to_toc_destination(page, destination_y, Vec::new(), window, cx);
        }
        cx.notify();
    }

    pub(super) fn complete_pending_toc_navigation(
        &mut self,
        page: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(index) = self.pending_toc_navigation else {
            return;
        };
        let Some((entry_page, title, explicit_destination_y)) = self
            .document
            .as_ref()
            .and_then(|document| document.toc.get(index))
            .map(|entry| (entry.page, entry.title.clone(), entry.destination_y))
        else {
            self.pending_toc_navigation = None;
            return;
        };
        if entry_page != page {
            return;
        }
        self.pending_toc_navigation = None;
        if self.navigate_toc_through_extension(&title, page, window, cx) {
            #[cfg(debug_assertions)]
            if self
                .page_text
                .get(&page)
                .and_then(|text| toc_title_match(&title, text))
                .is_some()
            {
                self.qa_toc_text_matches += 1;
            }
            return;
        }
        let bridge = self.pdf_capabilities();
        if let Ok(receipt) = bridge.activate_live_outline(&title, page) {
            if receipt.destination.text_range.is_some() {
                #[cfg(debug_assertions)]
                {
                    self.qa_toc_text_matches += 1;
                }
                self.consume_pdf_extension_outputs(window, cx);
                return;
            }
            let _ = bridge.take_pending_navigation();
        }
        let resolved = self.page_text.get(&page).map_or(
            ResolvedTocDestination {
                y: explicit_destination_y,
                text_runs: Vec::new(),
                matched_title: false,
            },
            |text| resolve_toc_destination(&title, text, explicit_destination_y),
        );
        #[cfg(debug_assertions)]
        if resolved.matched_title {
            self.qa_toc_text_matches += 1;
        }
        self.scroll_to_toc_destination(page, resolved.y, resolved.text_runs, window, cx);
    }

    fn navigate_toc_through_extension(
        &mut self,
        title: &str,
        page: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let Ok(effects) = self
            .extensions
            .invoke_toc_navigation(title.to_owned(), page)
        else {
            return false;
        };
        self.execute_extension_effects(effects, window, cx);
        true
    }

    fn scroll_to_toc_destination(
        &mut self,
        page: usize,
        destination_y: Option<f32>,
        focus_runs: Vec<TextBounds>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let jump = DocumentJump::new(page).position(None, destination_y).focus(
            focus_runs,
            NavigationFocusTone::Accent,
            NavigationFocusMotion::Sweep,
        );
        self.perform_document_jump(jump, window, cx);
    }

    pub(super) fn render_toc_navigation(
        &mut self,
        palette: ReaderPalette,
        cx: &mut Context<Self>,
    ) -> Option<gpui::AnyElement> {
        let document = self.document.as_ref()?;
        let layout = self.layout()?;
        if document.toc.is_empty() {
            return None;
        }

        let (stack_top, marker_spacing) =
            toc_stack_geometry(self.viewport_height, document.toc.len())?;
        let marker_hit_height = if marker_spacing <= f32::EPSILON {
            TOC_STACK_SPACING
        } else {
            marker_spacing.clamp(6.0, TOC_STACK_SPACING)
        };
        let current_page = layout.current_page(self.scroll.y, self.viewport_height);
        let active = active_toc_index(&document.toc, current_page);
        let hovered = self.toc_hovered.filter(|index| *index < document.toc.len());
        let maximum_card_width = (self.viewport_width - TOC_RAIL_WIDTH - 28.0).clamp(190.0, 360.0);
        let marker_data: Vec<_> = document
            .toc
            .iter()
            .enumerate()
            .map(|(index, entry)| {
                let selected = active == Some(index);
                let cascade =
                    toc_cascade_amount(index, self.toc_hover_position, self.toc_hover_strength);
                let baseline = (14.0 - f32::from(entry.depth.min(4))).max(10.0);
                let width = baseline + (52.0 - baseline) * cascade;
                (
                    index,
                    stack_top + index as f32 * marker_spacing,
                    selected,
                    hovered == Some(index),
                    width,
                )
            })
            .collect();
        let detail = hovered.and_then(|index| {
            let entry = document.toc.get(index)?;
            let marker_y = stack_top + self.toc_hover_position * marker_spacing;
            let breadcrumbs =
                toc_display_breadcrumbs(&toc_breadcrumb_entries(&document.toc, index)?);
            let card_width = toc_callout_width(&breadcrumbs, maximum_card_width);
            let card_height = toc_callout_height(&breadcrumbs, card_width)
                .min((self.viewport_height - 20.0).max(TOC_CARD_MIN_HEIGHT));
            Some((
                entry.title.clone(),
                entry.page,
                breadcrumbs,
                marker_y,
                card_width,
                card_height,
            ))
        });

        let markers = marker_data
            .into_iter()
            .map(|(index, y, selected, is_hovered, width)| {
                div()
                    .id(("toc-marker", index))
                    .absolute()
                    .top(px(y - marker_hit_height * 0.5))
                    .left_0()
                    .h(px(marker_hit_height))
                    .w(px(TOC_RAIL_WIDTH))
                    .flex()
                    .items_center()
                    .pl(px(TOC_MARKER_LEFT))
                    .cursor_pointer()
                    .on_hover(cx.listener(move |reader, hovered, window, cx| {
                        reader.set_toc_hovered(index, *hovered, window, cx)
                    }))
                    .on_click(cx.listener(move |reader, _, window, cx| {
                        reader.navigate_toc(index, window, cx)
                    }))
                    .child(
                        div()
                            .h(px(if is_hovered { 3.0 } else { 2.0 }))
                            .w(px(width))
                            .rounded_full()
                            .bg(if is_hovered {
                                palette.text
                            } else if hovered.is_none() && selected {
                                palette.text.opacity(0.88)
                            } else {
                                palette.text_tertiary.opacity(0.48)
                            }),
                    )
            });
        let detail_card =
            detail.map(
                |(title, page, breadcrumbs, marker_y, card_width, card_height)| {
                    let card_y = (marker_y - card_height * 0.5)
                        .clamp(10.0, (self.viewport_height - card_height - 10.0).max(10.0));
                    let breadcrumb_elements = breadcrumbs.into_iter().enumerate().map(
                        |(position, (entry_index, title))| {
                            div()
                                .flex()
                                .flex_shrink()
                                .items_center()
                                .gap_1()
                                .when(position != 0, |group| {
                                    group.child(
                                        div()
                                            .flex_none()
                                            .text_xs()
                                            .text_color(palette.text_tertiary)
                                            .child("›"),
                                    )
                                })
                                .child(
                                    div()
                                        .id(("toc-breadcrumb", entry_index))
                                        .max_w(px(card_width - 42.0))
                                        .whitespace_normal()
                                        .rounded_sm()
                                        .px_1()
                                        .text_xs()
                                        .text_color(palette.text_secondary)
                                        .cursor_pointer()
                                        .hover(|item| {
                                            item.bg(palette.control_hover).text_color(palette.text)
                                        })
                                        .on_click(cx.listener(move |reader, _, window, cx| {
                                            reader.navigate_toc(entry_index, window, cx);
                                            cx.stop_propagation();
                                        }))
                                        .child(title),
                                )
                        },
                    );
                    div()
                        .id("toc-hover-detail")
                        .block_mouse_except_scroll()
                        .on_hover(cx.listener(|reader, hovered, window, cx| {
                            reader.set_toc_callout_hovered(*hovered, window, cx)
                        }))
                        .absolute()
                        .top(px(card_y))
                        .left(px(TOC_RAIL_WIDTH + 6.0))
                        .w(px(card_width))
                        .h(px(card_height))
                        .px_4()
                        .py_3()
                        .overflow_y_scroll()
                        .rounded_xl()
                        .border_1()
                        .border_color(palette.text.opacity(0.13))
                        .bg(palette.surface)
                        .shadow_sm()
                        .text_color(palette.text)
                        .child(
                            div()
                                .whitespace_nowrap()
                                .text_ellipsis()
                                .text_sm()
                                .font_weight(FontWeight::SEMIBOLD)
                                .child(title),
                        )
                        .child(
                            div()
                                .mt_1()
                                .w_full()
                                .flex()
                                .flex_wrap()
                                .items_center()
                                .gap_1()
                                .children(breadcrumb_elements),
                        )
                        .child(
                            div()
                                .mt_1()
                                .text_xs()
                                .text_color(palette.text_tertiary)
                                .child(format!("Page {}", page + 1)),
                        )
                },
            );

        Some(
            div()
                .id("toc-navigation-rail")
                .block_mouse_except_scroll()
                .absolute()
                .top_0()
                .bottom_0()
                .left_0()
                .w(px(TOC_RAIL_WIDTH))
                .children(markers)
                .children(detail_card)
                .into_any_element(),
        )
    }
}
