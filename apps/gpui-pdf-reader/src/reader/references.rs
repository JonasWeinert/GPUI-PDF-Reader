use super::*;

pub(super) fn link_section_title(
    entries: &[TocEntry],
    page: usize,
    y_fraction: Option<f32>,
) -> Option<String> {
    let target_y = y_fraction.unwrap_or(0.0).clamp(0.0, 1.0);
    entries
        .iter()
        .filter(|entry| entry.page == page)
        .filter(|entry| entry.destination_y.unwrap_or(0.0) <= target_y + 0.025)
        .max_by(|left, right| {
            left.destination_y
                .unwrap_or(0.0)
                .total_cmp(&right.destination_y.unwrap_or(0.0))
                .then(left.depth.cmp(&right.depth))
        })
        .or_else(|| entries.iter().find(|entry| entry.page == page))
        .map(|entry| entry.title.clone())
}

pub(super) fn link_card_position(
    anchor: Rect,
    viewport_width: f32,
    viewport_height: f32,
    card_width: f32,
    estimated_height: f32,
) -> Offset {
    let maximum_x = (viewport_width - card_width - LINK_CARD_MARGIN).max(LINK_CARD_MARGIN);
    let x = (anchor.x + anchor.width * 0.5 - card_width * 0.5).clamp(LINK_CARD_MARGIN, maximum_x);
    let below = anchor.bottom() + LINK_CARD_GAP;
    let y = if below + estimated_height <= viewport_height - LINK_CARD_MARGIN {
        below
    } else {
        anchor.y - estimated_height - LINK_CARD_GAP
    }
    .clamp(
        LINK_CARD_MARGIN,
        (viewport_height - estimated_height - LINK_CARD_MARGIN).max(LINK_CARD_MARGIN),
    );
    Offset { x, y }
}

pub(super) fn pointer_link_card_position(
    pointer: Offset,
    viewport_width: f32,
    viewport_height: f32,
    card_width: f32,
    estimated_height: f32,
) -> Offset {
    let maximum_x = (viewport_width - card_width - LINK_CARD_MARGIN).max(LINK_CARD_MARGIN);
    let x = (pointer.x - card_width * 0.5).clamp(LINK_CARD_MARGIN, maximum_x);
    let below = pointer.y + LINK_CARD_GAP;
    let y = if below + estimated_height <= viewport_height - LINK_CARD_MARGIN {
        below
    } else {
        pointer.y - estimated_height - LINK_CARD_GAP
    }
    .clamp(
        LINK_CARD_MARGIN,
        (viewport_height - estimated_height - LINK_CARD_MARGIN).max(LINK_CARD_MARGIN),
    );
    Offset { x, y }
}

pub(super) fn link_preview_should_close(source_hovered: bool, card_hovered: bool) -> bool {
    !source_hovered && !card_hovered
}

impl PdfReader {
    pub(super) fn hit_test_link(&self, position: Point<Pixels>) -> Option<usize> {
        let layout = self.layout()?;
        let document = self.document.as_ref()?;
        let x = self.scroll.x + f32::from(position.x);
        let y = self.scroll.y + f32::from(position.y) - self.content_top();
        let page = layout.page_at_content_point(x, y)?;
        let page_rect = layout.page_rect(page)?;
        document
            .links
            .iter()
            .rev()
            .filter(|link| link.page == page)
            .find_map(|link| {
                let bounds = normalized_bounds_in_page(page_rect, link.bounds);
                bounds.contains(x, y).then_some(link.id)
            })
    }

    pub(super) fn hit_test_scientific_reference(&self, position: Point<Pixels>) -> Option<usize> {
        if !self.scientific_document {
            return None;
        }
        let layout = self.layout()?;
        let document = self.document.as_ref()?;
        let x = self.scroll.x + f32::from(position.x);
        let y = self.scroll.y + f32::from(position.y) - self.content_top();
        let page = layout.page_at_content_point(x, y)?;
        let page_rect = layout.page_rect(page)?;
        document
            .scientific_references
            .iter()
            .enumerate()
            .filter(|(_, reference)| reference.page == page)
            .find_map(|(index, reference)| {
                reference
                    .text_runs
                    .iter()
                    .any(|run| normalized_bounds_in_page(page_rect, *run).contains(x, y))
                    .then_some(index)
            })
    }

    pub(super) fn resolved_internal_link(&self, id: usize) -> Option<ResolvedInternalLink> {
        let document = self.document.as_ref()?;
        let link = document.links.iter().find(|link| link.id == id)?;
        let PdfLinkTarget::Internal {
            page,
            x_fraction,
            y_fraction,
        } = link.target
        else {
            return None;
        };
        let source_text = self.page_text.get(&link.page)?;
        let target_text = self.page_text.get(&page)?;
        Some(resolve_internal_link(
            source_text,
            link.bounds,
            target_text,
            page,
            x_fraction,
            y_fraction,
        ))
    }

    pub(super) fn request_internal_link_text(&mut self, id: usize) -> bool {
        let Some((source_page, target_page)) = self
            .document
            .as_ref()
            .and_then(|document| document.links.iter().find(|link| link.id == id))
            .and_then(|link| match link.target {
                PdfLinkTarget::Internal { page, .. } => Some((link.page, page)),
                PdfLinkTarget::External { .. } => None,
            })
        else {
            return false;
        };
        let pages = [source_page, target_page]
            .into_iter()
            .filter(|page| !self.page_text.contains_key(page))
            .collect::<HashSet<_>>();
        if pages.is_empty() {
            return true;
        }
        self.text_pending.extend(pages.iter().copied());
        let mut pages = pages.into_iter().collect::<Vec<_>>();
        pages.sort_unstable();
        if self
            .worker
            .ensure_text_pages(self.generation, pages.clone())
        {
            true
        } else {
            for page in pages {
                self.text_pending.remove(&page);
            }
            false
        }
    }

    pub(super) fn request_scholarly_for_link(&mut self, id: usize) -> bool {
        if !self.scientific_document {
            return false;
        }
        let references = self.scientific_reference_indices_for_link(id);
        if references.is_empty() {
            self.request_destination_preview(id);
            return false;
        }
        let texts = self
            .document
            .as_ref()
            .map(|document| {
                references
                    .into_iter()
                    .filter_map(|index| document.scientific_references.get(index))
                    .map(|reference| reference.text.clone())
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        texts.into_iter().fold(false, |requested, reference| {
            self.scholarly_session
                .request(&self.scholarly_fetcher, self.generation, &reference)
                || requested
        })
    }

    pub(super) fn request_scholarly_for_reference(&mut self, index: usize) -> bool {
        let Some(reference) = self
            .document
            .as_ref()
            .and_then(|document| document.scientific_references.get(index))
            .cloned()
        else {
            return false;
        };
        self.scholarly_session
            .request(&self.scholarly_fetcher, self.generation, &reference.text)
    }

    #[cfg(debug_assertions)]
    pub(super) fn current_reference_text(&self) -> Option<String> {
        self.current_reference_texts().into_iter().next()
    }

    pub(super) fn current_reference_texts(&self) -> Vec<String> {
        let Some(document) = self.document.as_ref() else {
            return Vec::new();
        };
        if let Some(index) = self.previewed_reference {
            return document
                .scientific_references
                .get(index)
                .map(|reference| vec![reference.text.clone()])
                .unwrap_or_default();
        }
        self.previewed_link
            .map(|id| {
                self.scientific_reference_indices_for_link(id)
                    .into_iter()
                    .filter_map(|index| document.scientific_references.get(index))
                    .map(|reference| reference.text.clone())
                    .collect()
            })
            .unwrap_or_default()
    }

    pub(super) fn jump_to_scientific_reference(
        &mut self,
        index: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(reference) = self
            .document
            .as_ref()
            .and_then(|document| document.scientific_references.get(index))
            .cloned()
        else {
            return;
        };
        self.perform_document_jump(
            DocumentJump::new(reference.page)
                .position(reference.x_fraction, reference.y_fraction)
                .center_horizontal(reference.x_fraction.is_some())
                .focus(
                    reference.text_runs,
                    NavigationFocusTone::Accent,
                    NavigationFocusMotion::Sweep,
                ),
            window,
            cx,
        );
    }

    pub(super) fn open_reference_details(
        &mut self,
        reference: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.open_reference_details_group(reference.clone(), vec![reference], window, cx);
    }

    pub(super) fn open_reference_details_group(
        &mut self,
        reference: String,
        mut group: Vec<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let center_anchor = self.layout().and_then(|layout| {
            layout.anchor_at_content_point(
                self.scroll.x + self.panel_safe_viewport_width() * 0.5,
                self.scroll.y + self.viewport_height * 0.5,
            )
        });
        self.sidebar_anchor = center_anchor;
        #[cfg(debug_assertions)]
        {
            self.qa_sidebar_anchor_reference = center_anchor;
        }
        self.viewport.disable_fit_width();
        self.viewport.set_scroll(self.scroll);
        self.sync_viewport_snapshot();
        if !group.contains(&reference) {
            group.insert(0, reference.clone());
        }
        group.dedup();
        self.reference_details_group = group;
        self.reference_details = Some(reference);
        self.reset_reference_detail_content_state();
        self.reference_details_transition = RevealState::visible();
        self.reference_details_direction = 1.0;
        self.sidebar.target = 0.0;
        self.reference_panel.set_target(1.0);
        self.dismiss_link_preview(window, cx);
        self.start_animation(window, cx);
        cx.notify();
    }

    pub(super) fn reset_reference_detail_content_state(&mut self) {
        let preferred_summary = self
            .reference_details
            .as_deref()
            .and_then(|reference| self.scholarly_session.state(reference))
            .and_then(|state| match state {
                ScholarlyMetadataState::Ready(metadata) if metadata.tldr_text.is_some() => {
                    Some(ReferenceSummaryTab::Tldr)
                }
                ScholarlyMetadataState::Ready(metadata) if metadata.abstract_text.is_some() => {
                    Some(ReferenceSummaryTab::Abstract)
                }
                _ => None,
            })
            .unwrap_or(ReferenceSummaryTab::Tldr);
        self.reference_citation_expansion = RevealState::default();
        self.reference_summary_tab = preferred_summary;
        self.reference_summary_previous_tab = preferred_summary;
        self.reference_summary_transition = RevealState::visible();
        self.doi_copy_started = None;
    }

    pub(super) fn navigate_reference_details(
        &mut self,
        forward: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let navigable = self.navigable_reference_details_group();
        if navigable.len() < 2 {
            return;
        }
        let Some(current) = self.reference_details.as_ref() else {
            return;
        };
        let current_index = navigable
            .iter()
            .position(|reference| reference == current)
            .unwrap_or(0);
        let next_index = adjacent_group_index(current_index, navigable.len(), forward);
        self.reference_details = navigable.get(next_index).cloned();
        self.reference_details_direction = if forward { 1.0 } else { -1.0 };
        self.reference_details_transition = RevealState::hidden();
        self.reference_details_transition.set_target(1.0);
        self.reset_reference_detail_content_state();
        self.start_animation(window, cx);
        cx.notify();
    }

    pub(super) fn navigable_reference_details_group(&self) -> Vec<String> {
        self.reference_details_group
            .iter()
            .filter(|reference| {
                matches!(
                    self.scholarly_session.state(reference),
                    Some(ScholarlyMetadataState::Ready(_))
                )
            })
            .cloned()
            .collect()
    }

    pub(super) fn close_reference_details(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let center_anchor = self.layout().and_then(|layout| {
            layout.anchor_at_content_point(
                self.scroll.x + self.panel_safe_viewport_width() * 0.5,
                self.scroll.y + self.viewport_height * 0.5,
            )
        });
        self.sidebar_anchor = center_anchor;
        #[cfg(debug_assertions)]
        {
            self.qa_sidebar_anchor_reference = center_anchor;
        }
        self.viewport.disable_fit_width();
        self.viewport.set_scroll(self.scroll);
        self.sync_viewport_snapshot();
        self.reference_panel.set_target(0.0);
        self.start_animation(window, cx);
        cx.notify();
    }

    pub(super) fn scientific_reference_for_link(&self, id: usize) -> Option<&ScientificReference> {
        let document = self.document.as_ref()?;
        let link = document.links.iter().find(|link| link.id == id)?;
        let PdfLinkTarget::Internal {
            page,
            x_fraction: _,
            y_fraction,
        } = link.target
        else {
            return None;
        };
        let resolved = self.resolved_internal_link(id);
        document.scientific_references.iter().find(|reference| {
            scientific_reference_matches(reference, page, resolved.as_ref(), y_fraction)
        })
    }

    pub(super) fn scientific_reference_indices_for_link(&self, id: usize) -> Vec<usize> {
        let Some(primary_number) = self
            .scientific_reference_for_link(id)
            .map(|reference| reference.number)
        else {
            return Vec::new();
        };
        let Some(document) = self.document.as_ref() else {
            return Vec::new();
        };
        let Some(primary_index) = document
            .scientific_references
            .iter()
            .position(|reference| reference.number == primary_number)
        else {
            return Vec::new();
        };
        let Some(link) = document.links.iter().find(|link| link.id == id) else {
            return vec![primary_index];
        };
        let Some(source_text) = self.page_text.get(&link.page) else {
            return vec![primary_index];
        };
        let source = link_source_text(source_text, link.bounds);
        complete_grouped_reference_indices(&document.scientific_references, &source, primary_number)
            .unwrap_or_else(|| vec![primary_index])
    }

    pub(super) fn request_destination_preview(&mut self, id: usize) -> bool {
        if !self.scientific_document {
            return false;
        }
        let Some((page, page_size, center_x, center_y)) =
            self.document.as_ref().and_then(|document| {
                let link = document.links.iter().find(|link| link.id == id)?;
                let PdfLinkTarget::Internal {
                    page,
                    x_fraction,
                    y_fraction,
                } = link.target
                else {
                    return None;
                };
                let resolved = self.resolved_internal_link(id);
                Some((
                    page,
                    *document.pages.get(page)?,
                    resolved
                        .as_ref()
                        .and_then(|value| value.x_fraction)
                        .or(x_fraction)
                        .unwrap_or(0.5),
                    resolved
                        .as_ref()
                        .and_then(|value| value.y_fraction)
                        .or(y_fraction)
                        .unwrap_or(0.5),
                ))
            })
        else {
            return false;
        };
        let longest = page_size.width.max(page_size.height).max(f32::MIN_POSITIVE);
        let raster = RasterSize {
            width: ((page_size.width / longest) * 1_200.0)
                .round()
                .clamp(1.0, 1_200.0) as u32,
            height: ((page_size.height / longest) * 1_200.0)
                .round()
                .clamp(1.0, 1_200.0) as u32,
        };
        self.destination_preview_revision = self.destination_preview_revision.wrapping_add(1);
        self.worker.render_preview(
            self.generation,
            self.destination_preview_revision,
            self.render_appearance,
            PreviewSpec {
                page,
                raster,
                center_x,
                center_y,
            },
        )
    }

    pub(super) fn perform_internal_link_jump(
        &mut self,
        page: usize,
        rough_x: Option<f32>,
        rough_y: Option<f32>,
        resolved: Option<ResolvedInternalLink>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let x_fraction = resolved
            .as_ref()
            .and_then(|resolved| resolved.x_fraction)
            .or(rough_x);
        let y_fraction = resolved
            .as_ref()
            .and_then(|resolved| resolved.y_fraction)
            .or(rough_y);
        let focus_runs = resolved.map_or_else(Vec::new, |resolved| resolved.text_runs);
        let jump = DocumentJump::new(page)
            .position(x_fraction, y_fraction)
            .center_horizontal(x_fraction.is_some())
            .focus(
                focus_runs,
                NavigationFocusTone::Accent,
                NavigationFocusMotion::Sweep,
            );
        self.perform_document_jump(jump, window, cx);
    }

    pub(super) fn complete_pending_link_navigation(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(id) = self.pending_link_navigation else {
            return;
        };
        let Some(resolved) = self.resolved_internal_link(id) else {
            return;
        };
        let Some((page, rough_x, rough_y)) = self
            .document
            .as_ref()
            .and_then(|document| document.links.iter().find(|link| link.id == id))
            .and_then(|link| match link.target {
                PdfLinkTarget::Internal {
                    page,
                    x_fraction,
                    y_fraction,
                } => Some((page, x_fraction, y_fraction)),
                PdfLinkTarget::External { .. } => None,
            })
        else {
            self.pending_link_navigation = None;
            return;
        };
        self.pending_link_navigation = None;
        self.perform_internal_link_jump(page, rough_x, rough_y, Some(resolved), window, cx);
    }

    pub(super) fn activate_document_link(
        &mut self,
        id: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(target) = self
            .document
            .as_ref()
            .and_then(|document| document.links.iter().find(|link| link.id == id))
            .map(|link| link.target.clone())
        else {
            return;
        };
        match target {
            PdfLinkTarget::Internal {
                page,
                x_fraction,
                y_fraction,
            } => {
                if let Some(resolved) = self.resolved_internal_link(id) {
                    self.pending_link_navigation = None;
                    self.perform_internal_link_jump(
                        page,
                        x_fraction,
                        y_fraction,
                        Some(resolved),
                        window,
                        cx,
                    );
                } else if self.request_internal_link_text(id) {
                    self.pending_link_navigation = Some(id);
                    cx.notify();
                } else {
                    self.pending_link_navigation = None;
                    self.perform_internal_link_jump(page, x_fraction, y_fraction, None, window, cx);
                }
            }
            PdfLinkTarget::External { url } => {
                if let Err(error) = open::that_detached(&url) {
                    self.warning = Some(format!("Could not open link: {error}").into());
                    cx.notify();
                }
            }
        }
    }

    pub(super) fn show_link_preview(
        &mut self,
        id: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(target) = self
            .document
            .as_ref()
            .and_then(|document| document.links.iter().find(|link| link.id == id))
            .map(|link| link.target.clone())
        else {
            return;
        };
        self.link_hover_revision = self.link_hover_revision.wrapping_add(1);
        self.pending_link_hover = None;
        self.link_source_hovered = true;
        if self.previewed_link != Some(id) || self.previewed_reference.is_some() {
            self.link_card_expansion = RevealState::default();
            self.clear_destination_preview(window, cx);
        }
        self.previewed_link = Some(id);
        self.previewed_reference = None;
        match target {
            PdfLinkTarget::Internal { .. } => {
                self.request_internal_link_text(id);
                self.request_scholarly_for_link(id);
                if self.current_reference_texts().iter().any(|reference| {
                    matches!(
                        self.scholarly_session.state(reference),
                        Some(ScholarlyMetadataState::Ready(_))
                    )
                }) {
                    self.link_card_expansion.set_target(1.0);
                    self.start_animation(window, cx);
                }
            }
            PdfLinkTarget::External { url } => {
                if let Some(session) = self.link_preview_session.as_mut() {
                    session.request_website(&self.link_preview_fetcher, self.generation, &url);
                }
            }
        }
        cx.notify();
    }

    pub(super) fn toggle_reference_citation(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.reference_citation_expansion.toggle();
        self.start_animation(window, cx);
        cx.notify();
    }

    pub(super) fn select_reference_summary(
        &mut self,
        tab: ReferenceSummaryTab,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if tab == self.reference_summary_tab {
            return;
        }
        self.reference_summary_previous_tab = self.reference_summary_tab;
        self.reference_summary_tab = tab;
        self.reference_summary_transition = RevealState::hidden();
        self.reference_summary_transition.set_target(1.0);
        self.start_animation(window, cx);
        cx.notify();
    }

    pub(super) fn copy_reference_doi(
        &mut self,
        doi: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        cx.write_to_clipboard(ClipboardItem::new_string(format!("https://doi.org/{doi}")));
        self.doi_copy_started = Some(Instant::now());
        self.start_animation(window, cx);
        cx.notify();
    }

    pub(super) fn show_reference_preview(
        &mut self,
        index: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self
            .document
            .as_ref()
            .and_then(|document| document.scientific_references.get(index))
            .is_none()
        {
            return;
        }
        self.link_hover_revision = self.link_hover_revision.wrapping_add(1);
        self.pending_link_hover = None;
        self.link_source_hovered = true;
        if self.previewed_reference != Some(index) || self.previewed_link.is_some() {
            self.link_card_expansion = RevealState::default();
            self.clear_destination_preview(window, cx);
        }
        self.previewed_link = None;
        self.previewed_reference = Some(index);
        self.request_scholarly_for_reference(index);
        if self.current_reference_texts().iter().any(|reference| {
            matches!(
                self.scholarly_session.state(reference),
                Some(ScholarlyMetadataState::Ready(_))
            )
        }) {
            self.link_card_expansion.set_target(1.0);
            self.start_animation(window, cx);
        }
        cx.notify();
    }

    pub(super) fn set_link_card_hovered(
        &mut self,
        hovered: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.link_card_hovered = hovered;
        if hovered {
            self.cancel_pending_link_hover();
            self.link_card_reposition_revision = self.link_card_reposition_revision.wrapping_add(1);
            cx.notify();
        } else {
            self.schedule_link_preview_clear(window, cx);
        }
    }

    pub(super) fn schedule_link_preview_clear(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.schedule_stable_link_hover(None, point(px(-1.0), px(-1.0)), window, cx);
    }

    pub(super) fn current_preview_target(&self) -> Option<PreviewTarget> {
        self.previewed_link
            .map(PreviewTarget::Link)
            .or_else(|| self.previewed_reference.map(PreviewTarget::Reference))
    }

    pub(super) fn show_preview_target(
        &mut self,
        target: PreviewTarget,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match target {
            PreviewTarget::Link(id) => self.show_link_preview(id, window, cx),
            PreviewTarget::Reference(index) => self.show_reference_preview(index, window, cx),
        }
    }

    pub(super) fn cancel_pending_link_hover(&mut self) {
        self.link_hover_revision = self.link_hover_revision.wrapping_add(1);
        self.pending_link_hover = None;
    }

    pub(super) fn dismiss_link_preview(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.cancel_pending_link_hover();
        self.link_card_reposition_revision = self.link_card_reposition_revision.wrapping_add(1);
        self.hovered_link = None;
        self.hovered_reference = None;
        self.link_source_hovered = false;
        self.link_card_hovered = false;
        self.previewed_link = None;
        self.previewed_reference = None;
        self.link_card_pointer = None;
        self.link_card_pointer_target = None;
        self.clear_destination_preview(window, cx);
    }

    pub(super) fn link_pointer_in_viewport(&self, position: Point<Pixels>) -> Offset {
        Offset {
            x: f32::from(position.x),
            y: f32::from(position.y) - self.content_top(),
        }
    }

    pub(super) fn set_link_card_pointer_immediate(&mut self, position: Point<Pixels>) {
        self.link_card_reposition_revision = self.link_card_reposition_revision.wrapping_add(1);
        let pointer = self.link_pointer_in_viewport(position);
        self.link_card_pointer = Some(pointer);
        self.link_card_pointer_target = Some(pointer);
    }

    pub(super) fn schedule_link_card_reposition(
        &mut self,
        position: Point<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let target = self.link_pointer_in_viewport(position);
        let already_at_target = self.link_card_pointer_target.is_some_and(|current| {
            (current.x - target.x).abs() < 0.5 && (current.y - target.y).abs() < 0.5
        });
        // Every movement invalidates an older pending retarget, including a
        // movement back to the position where the card already sits.
        self.link_card_reposition_revision = self.link_card_reposition_revision.wrapping_add(1);
        if already_at_target {
            return;
        }
        let revision = self.link_card_reposition_revision;
        let weak = cx.weak_entity();
        window
            .spawn(cx, async move |cx| {
                cx.background_executor()
                    .timer(LINK_CARD_MOVE_DEBOUNCE)
                    .await;
                let _ = cx.update(|window, cx| {
                    weak.update(cx, |reader, cx| {
                        if reader.link_card_reposition_revision == revision
                            && reader.link_source_hovered
                            && reader.current_preview_target().is_some()
                        {
                            reader.link_card_pointer_target = Some(target);
                            reader.link_card_pointer.get_or_insert(target);
                            reader.start_animation(window, cx);
                            cx.notify();
                        }
                    })
                    .ok();
                });
            })
            .detach();
    }

    pub(super) fn schedule_stable_link_hover(
        &mut self,
        target: Option<PreviewTarget>,
        position: Point<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self
            .pending_link_hover
            .is_some_and(|pending| !link_hover_candidate_needs_restart(pending, target, position))
        {
            return;
        }
        self.link_hover_revision = self.link_hover_revision.wrapping_add(1);
        let revision = self.link_hover_revision;
        self.pending_link_hover = Some(PendingLinkHover { target, position });
        let settle_delay = if target.is_some() {
            LINK_HOVER_HANDOFF_DELAY
        } else {
            LINK_HOVER_CLOSE_DELAY
        };
        let weak = cx.weak_entity();
        window
            .spawn(cx, async move |cx| {
                cx.background_executor().timer(settle_delay).await;
                let _ = cx.update(|window, cx| {
                    weak.update(cx, |reader, cx| {
                        if reader.link_hover_revision == revision && !reader.link_card_hovered {
                            let settled = reader.pending_link_hover.take();
                            if let Some(PendingLinkHover {
                                target: Some(target),
                                position,
                            }) = settled
                            {
                                reader.set_link_card_pointer_immediate(position);
                                reader.show_preview_target(target, window, cx);
                            } else if link_preview_should_close(
                                reader.link_source_hovered,
                                reader.link_card_hovered,
                            ) {
                                reader.dismiss_link_preview(window, cx);
                            }
                            cx.notify();
                        }
                    })
                    .ok();
                });
            })
            .detach();
    }

    pub(super) fn on_document_hover(
        &mut self,
        hovered: &bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !hovered {
            self.hovered_link = None;
            self.hovered_reference = None;
            self.link_source_hovered = false;
            self.schedule_link_preview_clear(window, cx);
        }
    }

    pub(super) fn render_link_preview_card(
        &mut self,
        palette: ReaderPalette,
        cx: &mut Context<Self>,
    ) -> Option<gpui::AnyElement> {
        let target = self
            .previewed_link
            .map(PreviewTarget::Link)
            .or_else(|| self.previewed_reference.map(PreviewTarget::Reference))?;
        let document = self.document.as_ref()?;
        let layout = self.layout()?;
        let (mut anchor, content, estimated_height, desired_width, card_accent) = match target {
            PreviewTarget::Reference(index) => {
                let reference = document.scientific_references.get(index)?.clone();
                let card_accent = reference_identity_color(&reference.text, palette);
                let page_rect = layout.page_rect(reference.page)?;
                let bounds = reference
                    .text_runs
                    .iter()
                    .copied()
                    .reduce(union_text_bounds)?;
                let anchor = normalized_bounds_in_page(page_rect, bounds);
                let state = self.scholarly_session.state(&reference.text).cloned();
                let desired_width =
                    reference_preview_width(state.as_ref(), self.link_card_expansion.value());
                let content = self.render_reference_source_card(
                    target,
                    reference.text,
                    state,
                    card_accent,
                    palette,
                    cx,
                );
                (
                    anchor,
                    content,
                    122.0 + self.link_card_expansion.value() * 104.0,
                    desired_width,
                    card_accent,
                )
            }
            PreviewTarget::Link(id) => {
                let link = document.links.iter().find(|link| link.id == id)?.clone();
                let page_rect = layout.page_rect(link.page)?;
                let anchor = normalized_bounds_in_page(page_rect, link.bounds);
                match &link.target {
                    PdfLinkTarget::External { url } => {
                        let parsed = url::Url::parse(url).ok();
                        let host = parsed
                            .as_ref()
                            .and_then(url::Url::host_str)
                            .unwrap_or("External link")
                            .to_owned();
                        let state = self
                            .link_preview_session
                            .as_ref()
                            .and_then(|session| session.website(url))
                            .cloned();
                        let (title, site_name, image_path, status) = match state {
                            Some(WebsitePreviewState::Ready(preview)) => (
                                preview.title.unwrap_or_else(|| host.clone()),
                                preview.site_name,
                                preview.image_path,
                                None,
                            ),
                            Some(WebsitePreviewState::Failed(_)) => (
                                host.clone(),
                                None,
                                None,
                                Some("Preview unavailable".to_owned()),
                            ),
                            Some(WebsitePreviewState::Loading) => (
                                host.clone(),
                                None,
                                None,
                                Some("Loading website preview…".to_owned()),
                            ),
                            None => (
                                host.clone(),
                                None,
                                None,
                                Some("Website preview is unavailable".to_owned()),
                            ),
                        };
                        let open_id = id;
                        let desired_width = if image_path.is_some() {
                            328.0
                        } else {
                            measured_preview_width(
                                &[title.as_str(), site_name.as_deref().unwrap_or(&host)],
                                228.0,
                                360.0,
                            )
                        };
                        let content = div()
                            .min_w_0()
                            .when_some(image_path, |content, image_path| {
                                content.child(
                                    div()
                                        .mb_3()
                                        .h(px(116.0))
                                        .w_full()
                                        .overflow_hidden()
                                        .rounded_lg()
                                        .bg(palette.canvas)
                                        .child(
                                            img(image_path)
                                                .size_full()
                                                .object_fit(gpui::ObjectFit::Cover),
                                        ),
                                )
                            })
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .gap_2()
                                    .child(
                                        Icon::new(IconName::Globe)
                                            .size(px(15.0))
                                            .text_color(palette.text_tertiary),
                                    )
                                    .child(
                                        div()
                                            .min_w_0()
                                            .whitespace_nowrap()
                                            .text_ellipsis()
                                            .text_sm()
                                            .font_weight(FontWeight::SEMIBOLD)
                                            .text_color(palette.text)
                                            .child(title),
                                    ),
                            )
                            .child(
                                div()
                                    .mt_1()
                                    .min_w_0()
                                    .whitespace_nowrap()
                                    .text_ellipsis()
                                    .text_xs()
                                    .text_color(palette.text_tertiary)
                                    .child(site_name.unwrap_or(host)),
                            )
                            .when_some(status, |content, status| {
                                content.child(
                                    div()
                                        .mt_2()
                                        .text_xs()
                                        .text_color(palette.text_secondary)
                                        .child(status),
                                )
                            })
                            .child(
                                div()
                                    .id(("link-preview-open", id))
                                    .mt_3()
                                    .h(px(32.0))
                                    .px_3()
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .gap_1()
                                    .rounded_md()
                                    .bg(palette.blue)
                                    .text_xs()
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .text_color(palette.accent_foreground)
                                    .cursor_pointer()
                                    .hover(|button| button.bg(palette.blue.opacity(0.86)))
                                    .active(|button| button.bg(palette.blue.opacity(0.72)))
                                    .on_click(cx.listener(move |reader, _, window, cx| {
                                        reader.activate_document_link(open_id, window, cx);
                                        reader.dismiss_link_preview(window, cx);
                                        cx.stop_propagation();
                                    }))
                                    .child(Icon::new(IconName::ExternalLink).size(px(14.0)))
                                    .child("Open in browser"),
                            )
                            .into_any_element();
                        (
                            anchor,
                            content,
                            if self
                                .link_preview_session
                                .as_ref()
                                .and_then(|session| session.website(url))
                                .is_some_and(|state| {
                                    matches!(state, WebsitePreviewState::Ready(preview) if preview.image_path.is_some())
                                })
                            {
                                274.0
                            } else {
                                156.0
                            },
                            desired_width,
                            palette.blue,
                        )
                    }
                    PdfLinkTarget::Internal { page, .. } => {
                        let reference_indices = self.scientific_reference_indices_for_link(id);
                        if reference_indices.len() > 1 {
                            let references = reference_indices
                                .into_iter()
                                .filter_map(|index| {
                                    let reference =
                                        document.scientific_references.get(index)?.clone();
                                    let state =
                                        self.scholarly_session.state(&reference.text).cloned();
                                    Some((index, reference, state))
                                })
                                .collect::<Vec<_>>();
                            let card_accent = references
                                .first()
                                .map(|(_, reference, _)| {
                                    reference_identity_color(&reference.text, palette)
                                })
                                .unwrap_or(palette.purple);
                            let estimated_height = (58.0 + references.len() as f32 * 122.0)
                                .min((self.viewport_height - LINK_CARD_MARGIN * 2.0).max(160.0));
                            (
                                anchor,
                                self.render_grouped_reference_source_card(
                                    references,
                                    card_accent,
                                    palette,
                                    cx,
                                ),
                                estimated_height,
                                LINK_CARD_WIDTH,
                                card_accent,
                            )
                        } else if let Some(reference) =
                            self.scientific_reference_for_link(id).cloned()
                        {
                            let card_accent = reference_identity_color(&reference.text, palette);
                            let state = self.scholarly_session.state(&reference.text).cloned();
                            let desired_width = reference_preview_width(
                                state.as_ref(),
                                self.link_card_expansion.value(),
                            );
                            (
                                anchor,
                                self.render_reference_source_card(
                                    target,
                                    reference.text,
                                    state,
                                    card_accent,
                                    palette,
                                    cx,
                                ),
                                122.0 + self.link_card_expansion.value() * 104.0,
                                desired_width,
                                card_accent,
                            )
                        } else {
                            let title = link_section_title(
                                &document.toc,
                                *page,
                                match link.target {
                                    PdfLinkTarget::Internal { y_fraction, .. } => y_fraction,
                                    PdfLinkTarget::External { .. } => None,
                                },
                            )
                            .unwrap_or_else(|| "Document section".to_owned());
                            let preview = self
                                .resolved_internal_link(id)
                                .map(|resolved| resolved.preview)
                                .filter(|preview| !preview.is_empty());
                            let image = self
                                .destination_preview
                                .as_ref()
                                .filter(|preview| {
                                    preview.revision == self.destination_preview_revision
                                })
                                .map(|preview| preview.image.clone());
                            let image_pending = image.is_none();
                            let desired_width = if image.is_some() {
                                328.0
                            } else {
                                measured_preview_width(
                                    &[
                                        title.as_str(),
                                        preview.as_deref().unwrap_or("Document section"),
                                    ],
                                    240.0,
                                    344.0,
                                )
                            };
                            let content = div()
                                .min_w_0()
                                .child(
                                    div()
                                        .text_sm()
                                        .font_weight(FontWeight::SEMIBOLD)
                                        .text_color(palette.text)
                                        .child(compact_words(&title, 12)),
                                )
                                .child(
                                    div()
                                        .mt_1()
                                        .text_xs()
                                        .text_color(palette.text_tertiary)
                                        .child(format!("Page {}", page + 1)),
                                )
                                .child(
                                    div()
                                        .mt_3()
                                        .h(px(112.0))
                                        .w_full()
                                        .overflow_hidden()
                                        .rounded_lg()
                                        .border_1()
                                        .border_color(palette.separator)
                                        .bg(palette.canvas)
                                        .when_some(image, |thumbnail, image| {
                                            thumbnail.child(
                                                img(image)
                                                    .size_full()
                                                    .object_fit(gpui::ObjectFit::Cover),
                                            )
                                        })
                                        .when(image_pending, |thumbnail| {
                                            thumbnail
                                                .flex()
                                                .items_center()
                                                .justify_center()
                                                .text_xs()
                                                .text_color(palette.text_tertiary)
                                                .child("Rendering preview…")
                                        }),
                                )
                                .when_some(preview, |content, preview| {
                                    content.child(
                                        div()
                                            .mt_2()
                                            .text_xs()
                                            .text_color(palette.text_secondary)
                                            .child(compact_words(&preview, 22)),
                                    )
                                })
                                .child(self.render_preview_jump(target, palette.purple, cx))
                                .into_any_element();
                            (anchor, content, 256.0, desired_width, palette.purple)
                        }
                    }
                }
            }
        };
        anchor.x -= self.scroll.x;
        anchor.y -= self.scroll.y;
        let card_width = desired_width.clamp(
            220.0,
            LINK_CARD_WIDTH.min((self.viewport_width - LINK_CARD_MARGIN * 2.0).max(220.0)),
        );
        let position = self.link_card_pointer.map_or_else(
            || {
                link_card_position(
                    anchor,
                    self.viewport_width,
                    self.viewport_height,
                    card_width,
                    estimated_height,
                )
            },
            |pointer| {
                pointer_link_card_position(
                    pointer,
                    self.viewport_width,
                    self.viewport_height,
                    card_width,
                    estimated_height,
                )
            },
        );
        Some(
            div()
                .id("link-preview-card")
                .block_mouse_except_scroll()
                .on_hover(cx.listener(|reader, hovered, window, cx| {
                    reader.set_link_card_hovered(*hovered, window, cx)
                }))
                .absolute()
                .top(px(position.y))
                .left(px(position.x))
                .w(px(card_width))
                .max_h(px(
                    (self.viewport_height - LINK_CARD_MARGIN * 2.0).max(120.0)
                ))
                .overflow_hidden()
                .rounded_xl()
                .border_1()
                .border_color(card_accent.opacity(0.30))
                .bg(palette.surface)
                .shadow_sm()
                .child(
                    div()
                        .id("link-preview-card-scroll")
                        .max_h(px(
                            (self.viewport_height - LINK_CARD_MARGIN * 2.0).max(120.0)
                        ))
                        .overflow_y_scroll()
                        .child(div().h(px(3.0)).w_full().flex_none().bg(card_accent))
                        .child(div().min_w_0().px_4().pt_3().pb_4().child(content)),
                )
                .into_any_element(),
        )
    }

    pub(super) fn render_grouped_reference_source_card(
        &self,
        references: Vec<(usize, ScientificReference, Option<ScholarlyMetadataState>)>,
        identity_color: gpui::Hsla,
        palette: ReaderPalette,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let group = references
            .iter()
            .map(|(_, reference, _)| reference.text.clone())
            .collect::<Vec<_>>();
        let count = references.len();
        let rows = references.into_iter().enumerate().map(
            |(position, (reference_index, reference, state))| {
                let details_group = group.clone();
                let details_reference = reference.text.clone();
                let body = match state {
                    Some(ScholarlyMetadataState::Ready(metadata)) => div()
                        .min_w_0()
                        .child(
                            div()
                                .text_sm()
                                .font_weight(FontWeight::SEMIBOLD)
                                .text_color(palette.text)
                                .child(compact_words(&metadata.title, 14)),
                        )
                        .child(
                            div()
                                .mt_1()
                                .min_w_0()
                                .whitespace_nowrap()
                                .text_ellipsis()
                                .text_xs()
                                .text_color(palette.text_secondary)
                                .child(compact_citation_line(&metadata)),
                        )
                        .child(
                            div()
                                .mt_2()
                                .flex()
                                .items_center()
                                .gap_4()
                                .child(self.render_grouped_reference_jump(
                                    reference_index,
                                    palette,
                                    cx,
                                ))
                                .child(
                                    div()
                                        .id(("grouped-reference-details", reference_index))
                                        .flex()
                                        .items_center()
                                        .gap_1()
                                        .text_xs()
                                        .font_weight(FontWeight::SEMIBOLD)
                                        .text_color(identity_color)
                                        .cursor_pointer()
                                        .hover(|button| button.opacity(0.68))
                                        .active(|button| button.opacity(0.48))
                                        .on_click(cx.listener(move |reader, _, window, cx| {
                                            reader.open_reference_details_group(
                                                details_reference.clone(),
                                                details_group.clone(),
                                                window,
                                                cx,
                                            );
                                            cx.stop_propagation();
                                        }))
                                        .child("View details")
                                        .child(Icon::new(IconName::ArrowRight).size(px(12.0))),
                                ),
                        )
                        .into_any_element(),
                    Some(ScholarlyMetadataState::Failed(_)) => div()
                        .min_w_0()
                        .child(
                            div()
                                .text_xs()
                                .line_height(px(18.0))
                                .text_color(palette.text_secondary)
                                .child(compact_words(&reference.text, 18)),
                        )
                        .child(
                            div()
                                .mt_2()
                                .flex()
                                .items_center()
                                .gap_2()
                                .text_xs()
                                .text_color(palette.text_tertiary)
                                .child(Icon::new(IconName::CircleX).size(px(13.0)))
                                .child("No source found"),
                        )
                        .child(div().mt_2().child(self.render_grouped_reference_jump(
                            reference_index,
                            palette,
                            cx,
                        )))
                        .into_any_element(),
                    Some(ScholarlyMetadataState::Loading) | None => div()
                        .min_w_0()
                        .child(
                            div()
                                .text_xs()
                                .line_height(px(18.0))
                                .text_color(palette.text_secondary)
                                .child(compact_words(&reference.text, 18)),
                        )
                        .child(
                            div()
                                .mt_2()
                                .flex()
                                .items_center()
                                .gap_2()
                                .text_xs()
                                .text_color(palette.text_tertiary)
                                .child(loading_source_icon(identity_color))
                                .child("Checking source"),
                        )
                        .child(div().mt_2().child(self.render_grouped_reference_jump(
                            reference_index,
                            palette,
                            cx,
                        )))
                        .into_any_element(),
                };
                div()
                    .when(position != 0, |row| {
                        row.mt_3()
                            .pt_3()
                            .border_t_1()
                            .border_color(palette.separator)
                    })
                    .child(
                        div()
                            .mb_1()
                            .text_xs()
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(identity_color)
                            .child(format!("REFERENCE {}", reference.number)),
                    )
                    .child(body)
            },
        );
        div()
            .min_w_0()
            .child(
                div()
                    .mb_3()
                    .flex()
                    .items_center()
                    .justify_between()
                    .child(
                        div()
                            .text_sm()
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(palette.text)
                            .child("Grouped citation"),
                    )
                    .child(
                        div()
                            .px_2()
                            .py_1()
                            .rounded_full()
                            .bg(identity_color.opacity(0.12))
                            .text_xs()
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(identity_color)
                            .child(format!("{count} sources")),
                    ),
            )
            .children(rows)
            .into_any_element()
    }

    pub(super) fn render_grouped_reference_jump(
        &self,
        reference_index: usize,
        palette: ReaderPalette,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        div()
            .id(("grouped-reference-jump", reference_index))
            .flex()
            .items_center()
            .gap_1()
            .text_xs()
            .font_weight(FontWeight::MEDIUM)
            .text_color(palette.text_secondary)
            .cursor_pointer()
            .hover(|button| button.opacity(0.68))
            .active(|button| button.opacity(0.48))
            .on_click(cx.listener(move |reader, _, window, cx| {
                reader.jump_to_scientific_reference(reference_index, window, cx);
                reader.dismiss_link_preview(window, cx);
                cx.stop_propagation();
            }))
            .child("Jump")
            .child(Icon::new(IconName::ArrowRight).size(px(12.0)))
            .into_any_element()
    }

    pub(super) fn render_reference_source_card(
        &self,
        target: PreviewTarget,
        reference: String,
        state: Option<ScholarlyMetadataState>,
        identity_color: gpui::Hsla,
        palette: ReaderPalette,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let status = match state.clone() {
            Some(ScholarlyMetadataState::Ready(metadata)) => {
                let details_reference = reference.clone();
                let citation = compact_citation_line(&metadata);
                let progress = self.link_card_expansion.value();
                let details = div()
                    .min_w_0()
                    .child(
                        div()
                            .text_sm()
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(palette.text)
                            .child(compact_words(&metadata.title, 11)),
                    )
                    .child(
                        div()
                            .mt_1()
                            .min_w_0()
                            .whitespace_nowrap()
                            .text_ellipsis()
                            .text_xs()
                            .text_color(palette.text_secondary)
                            .child(citation),
                    )
                    .child(
                        div()
                            .id("view-reference-details")
                            .mt_3()
                            .h(px(32.0))
                            .px_3()
                            .flex()
                            .items_center()
                            .justify_center()
                            .gap_1()
                            .rounded_md()
                            .bg(identity_color.opacity(0.12))
                            .text_xs()
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(identity_color)
                            .cursor_pointer()
                            .hover(move |button| button.bg(identity_color.opacity(0.19)))
                            .active(|button| button.opacity(0.76))
                            .on_click(cx.listener(move |reader, _, window, cx| {
                                reader.open_reference_details(
                                    details_reference.clone(),
                                    window,
                                    cx,
                                );
                                cx.stop_propagation();
                            }))
                            .child("View Reference details")
                            .child(Icon::new(IconName::ArrowRight).size(px(14.0))),
                    );
                div()
                    .h(px(20.0 + progress * 104.0))
                    .min_w_0()
                    .overflow_hidden()
                    .when(progress < 0.65, |status| {
                        status
                            .flex()
                            .items_center()
                            .gap_2()
                            .text_sm()
                            .text_color(palette.text_secondary)
                            .child(loading_source_icon(identity_color))
                            .child("Checking source")
                    })
                    .when(progress >= 0.65, |status| {
                        status.child(details.opacity(((progress - 0.65) / 0.35).clamp(0.0, 1.0)))
                    })
                    .into_any_element()
            }
            Some(ScholarlyMetadataState::Failed(_)) => div()
                .flex()
                .items_center()
                .gap_2()
                .text_sm()
                .text_color(palette.text_secondary)
                .child(Icon::new(IconName::CircleX).size(px(15.0)))
                .child("No source found")
                .into_any_element(),
            Some(ScholarlyMetadataState::Loading) | None => div()
                .flex()
                .items_center()
                .gap_2()
                .text_sm()
                .text_color(palette.text_secondary)
                .child(loading_source_icon(identity_color))
                .child("Checking source")
                .into_any_element(),
        };
        div()
            .min_w_0()
            .child(status)
            .child(
                div()
                    .mt_3()
                    .pt_3()
                    .border_t_1()
                    .border_color(palette.text.opacity(0.10))
                    .child(self.render_preview_jump(target, identity_color, cx)),
            )
            .into_any_element()
    }

    pub(super) fn render_preview_jump(
        &self,
        target: PreviewTarget,
        accent: gpui::Hsla,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        div()
            .id("link-preview-jump")
            .h(px(30.0))
            .px_2()
            .flex()
            .items_center()
            .justify_between()
            .gap_2()
            .rounded_md()
            .text_xs()
            .font_weight(FontWeight::MEDIUM)
            .text_color(accent)
            .cursor_pointer()
            .hover(move |button| button.bg(accent.opacity(0.10)))
            .active(move |button| button.bg(accent.opacity(0.17)))
            .on_click(cx.listener(move |reader, _, window, cx| {
                match target {
                    PreviewTarget::Link(id) => reader.activate_document_link(id, window, cx),
                    PreviewTarget::Reference(index) => {
                        reader.jump_to_scientific_reference(index, window, cx)
                    }
                }
                reader.dismiss_link_preview(window, cx);
                cx.stop_propagation();
            }))
            .child("Jump to Document section")
            .child(Icon::new(IconName::ArrowRight).size(px(14.0)))
            .into_any_element()
    }

    pub(super) fn render_reference_details_panel(
        &mut self,
        palette: ReaderPalette,
        full_width: f32,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<gpui::AnyElement> {
        let reference = self.reference_details.clone()?;
        let Some(ScholarlyMetadataState::Ready(metadata)) =
            self.scholarly_session.state(&reference).cloned()
        else {
            return None;
        };
        let panel_width = reference_panel_width(full_width);
        if panel_width <= 0.0 {
            return None;
        }
        let progress = self.reference_panel.value();
        let right = 12.0 - (panel_width + 24.0) * (1.0 - progress);
        let navigable_details = self.navigable_reference_details_group();
        let detail_position = navigable_details
            .iter()
            .position(|candidate| candidate == &reference)
            .unwrap_or(0);
        let detail_count = navigable_details.len().max(1);
        let page_transition = self.reference_details_transition.value();
        let detail_offset =
            self.reference_details_direction * (1.0 - page_transition) * (panel_width - 2.0);
        let detail_opacity = (0.72 + page_transition * 0.28).clamp(0.0, 1.0);
        let authors = if metadata.authors.is_empty() {
            "Authors unavailable".to_owned()
        } else {
            metadata.authors.join(", ")
        };
        let journal = metadata
            .journal
            .clone()
            .unwrap_or_else(|| "Journal unavailable".to_owned());
        let source_line = format!(
            "{} · {}",
            metadata.source.label(),
            metadata
                .certainty
                .map(|certainty| certainty.label())
                .unwrap_or("DOI match")
        );
        let summary_height = (self.viewport_height * 0.30).clamp(150.0, 300.0);
        let title_size = if metadata.title.chars().count() > 120 {
            17.0
        } else {
            19.0
        };
        let doi_url = metadata
            .doi
            .as_ref()
            .map(|doi| format!("https://doi.org/{doi}"));
        let open_access = metadata.open_access;
        let identity_color = reference_identity_color(&metadata.title, palette);
        let access_icon = match open_access {
            Some(true) => IconName::CircleCheck,
            Some(false) => IconName::EyeOff,
            None => IconName::Info,
        };
        let access_label = match open_access {
            Some(true) => "Open access",
            Some(false) => "Access may be restricted",
            None => "Access unknown",
        };
        let access_color = if open_access == Some(true) {
            palette.green
        } else {
            palette.text_secondary
        };
        let hero_height = reference_hero_height(&metadata.title);
        let mut access_actions = Vec::new();
        if let Some(url) = metadata.journal_url.clone() {
            access_actions.push(self.render_reference_action(
                "open-reference-journal",
                IconName::BookOpen,
                "Journal",
                url,
                false,
                identity_color,
                palette,
                cx,
            ));
        }
        if let Some(url) = metadata.full_text_url.clone() {
            access_actions.push(self.render_reference_action(
                "open-reference-pdf",
                IconName::File,
                "PDF",
                url,
                true,
                identity_color,
                palette,
                cx,
            ));
        }
        if let Some(url) = doi_url {
            access_actions.push(self.render_reference_action(
                "open-reference-doi",
                IconName::ExternalLink,
                "Publisher",
                url,
                false,
                identity_color,
                palette,
                cx,
            ));
        }
        if let Some(url) = metadata.landing_url.clone() {
            access_actions.push(self.render_reference_action(
                "open-reference-metadata",
                IconName::Globe,
                "Metadata",
                url,
                false,
                identity_color,
                palette,
                cx,
            ));
        }
        let hero_title = self.selectable_reference_text(
            "reference-hero-title-text",
            &metadata.title,
            window,
            cx,
        );
        let source_text =
            self.selectable_reference_text("reference-source-text", &source_line, window, cx);
        let citation = self.render_reference_citation(
            &metadata,
            &authors,
            &journal,
            identity_color,
            palette,
            window,
            cx,
        );
        let summary = self.render_reference_summary(
            &metadata,
            panel_width,
            summary_height,
            identity_color,
            palette,
            window,
            cx,
        );
        let header_doi = metadata.doi.as_deref().map(|doi| {
            self.render_copyable_doi(
                "reference-hero-doi",
                "reference-hero-doi-text",
                doi,
                &middle_truncate(doi, 25),
                identity_color,
                palette,
                window,
                cx,
            )
        });
        let navigation = (detail_count > 1).then(|| {
            div()
                .flex()
                .items_center()
                .gap_1()
                .child(
                    div()
                        .id("previous-reference-detail")
                        .size(px(28.0))
                        .flex()
                        .items_center()
                        .justify_center()
                        .rounded_md()
                        .cursor_pointer()
                        .text_color(palette.text_secondary)
                        .hover(|button| button.opacity(0.68))
                        .active(|button| button.opacity(0.48))
                        .on_click(cx.listener(|reader, _, window, cx| {
                            reader.navigate_reference_details(false, window, cx);
                            cx.stop_propagation();
                        }))
                        .child(Icon::new(IconName::ChevronLeft).size(px(15.0))),
                )
                .child(
                    div()
                        .min_w(px(42.0))
                        .text_center()
                        .text_xs()
                        .font_weight(FontWeight::MEDIUM)
                        .text_color(palette.text_tertiary)
                        .child(format!("{} of {}", detail_position + 1, detail_count)),
                )
                .child(
                    div()
                        .id("next-reference-detail")
                        .size(px(28.0))
                        .flex()
                        .items_center()
                        .justify_center()
                        .rounded_md()
                        .cursor_pointer()
                        .text_color(palette.text_secondary)
                        .hover(|button| button.opacity(0.68))
                        .active(|button| button.opacity(0.48))
                        .on_click(cx.listener(|reader, _, window, cx| {
                            reader.navigate_reference_details(true, window, cx);
                            cx.stop_propagation();
                        }))
                        .child(Icon::new(IconName::ChevronRight).size(px(15.0))),
                )
        });
        let content = div()
            .size_full()
            .min_w_0()
            .min_h_0()
            .flex()
            .flex_col()
            .child(
                div()
                    .h(px(58.0))
                    .flex_none()
                    .px_5()
                    .flex()
                    .items_center()
                    .justify_between()
                    .border_b_1()
                    .border_color(palette.separator)
                    .child(
                        div()
                            .text_sm()
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(palette.text)
                            .child("Reference details"),
                    )
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .children(navigation)
                            .child(
                                div()
                                    .id("close-reference-details")
                                    .size(px(30.0))
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .rounded_md()
                                    .cursor_pointer()
                                    .text_color(palette.text_secondary)
                                    .hover(|button| button.bg(palette.control_hover))
                                    .active(|button| button.bg(palette.control_pressed))
                                    .on_click(cx.listener(|reader, _, window, cx| {
                                        reader.close_reference_details(window, cx);
                                        cx.stop_propagation();
                                    }))
                                    .child(Icon::new(IconName::Close).size(px(16.0))),
                            ),
                    ),
            )
            .child(
                div()
                    .h(px(hero_height))
                    .flex_none()
                    .relative()
                    .left(px(detail_offset))
                    .opacity(detail_opacity)
                    .overflow_hidden()
                    .bg(identity_color.opacity(0.14))
                    .child(
                        div()
                            .absolute()
                            .left_0()
                            .top_0()
                            .bottom_0()
                            .w(px(5.0))
                            .bg(identity_color),
                    )
                    .child(
                        div()
                            .absolute()
                            .right(px(-54.0))
                            .top(px(-64.0))
                            .size(px(180.0))
                            .rounded_full()
                            .border_1()
                            .border_color(identity_color.opacity(0.28)),
                    )
                    .child(
                        div()
                            .absolute()
                            .right(px(22.0))
                            .bottom(px(-70.0))
                            .size(px(128.0))
                            .rounded_full()
                            .border_1()
                            .border_color(identity_color.opacity(0.20)),
                    )
                    .child(
                        div()
                            .absolute()
                            .right(px(28.0))
                            .top(px(28.0))
                            .text_color(identity_color.opacity(0.52))
                            .child(Icon::new(IconName::BookOpen).size(px(38.0))),
                    )
                    .child(
                        div()
                            .absolute()
                            .left(px(22.0))
                            .right(px(22.0))
                            .top(px(20.0))
                            .bottom(px(16.0))
                            .min_w_0()
                            .child(
                                div()
                                    .pr(px(70.0))
                                    .text_xs()
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .text_color(identity_color)
                                    .child(source_text),
                            )
                            .child(
                                div()
                                    .mt_2()
                                    .pr(px(70.0))
                                    .text_size(px(title_size))
                                    .line_height(px(title_size + 6.0))
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .text_color(palette.text)
                                    .child(hero_title),
                            )
                            .child(
                                div()
                                    .mt_2()
                                    .min_w_0()
                                    .flex()
                                    .items_center()
                                    .gap_2()
                                    .text_xs()
                                    .child(
                                        div()
                                            .flex_none()
                                            .flex()
                                            .items_center()
                                            .gap_1()
                                            .font_weight(FontWeight::MEDIUM)
                                            .text_color(access_color)
                                            .child(Icon::new(access_icon).size(px(14.0)))
                                            .child(access_label),
                                    )
                                    .when(metadata.doi.is_some(), |status| {
                                        status.child(
                                            div()
                                                .size(px(3.0))
                                                .flex_none()
                                                .rounded_full()
                                                .bg(palette.text_tertiary.opacity(0.56)),
                                        )
                                    })
                                    .children(header_doi),
                            ),
                    ),
            )
            .child(
                div()
                    .id(("reference-details-scroll", detail_position))
                    .relative()
                    .left(px(detail_offset))
                    .opacity(detail_opacity)
                    .flex_1()
                    .min_h_0()
                    .min_w_0()
                    .overflow_y_scroll()
                    .px_5()
                    .pt_3()
                    .pb_6()
                    .child(citation)
                    .when(!access_actions.is_empty(), |body| {
                        body.child(
                            div()
                                .mt_6()
                                .mb_1()
                                .text_xs()
                                .font_weight(FontWeight::SEMIBOLD)
                                .text_color(identity_color)
                                .child("ACCESS & LINKS"),
                        )
                        .child(
                            div()
                                .w_full()
                                .min_w_0()
                                .flex()
                                .gap_1()
                                .overflow_hidden()
                                .children(access_actions),
                        )
                    })
                    .children(summary),
            );
        Some(
            div()
                .id("reference-details-panel")
                .block_mouse_except_scroll()
                .absolute()
                .top(px(TOOLBAR_HEIGHT + FLUID_PANEL_VERTICAL_MARGIN))
                .bottom(px(FLUID_PANEL_VERTICAL_MARGIN))
                .right(px(right))
                .w(px(panel_width))
                .opacity(progress.max(0.01))
                .child(FloatingPanel::new(palette, content))
                .into_any_element(),
        )
    }

    pub(super) fn selectable_reference_text(
        &self,
        id: &'static str,
        text: impl AsRef<str>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        TextView::markdown(id, escape_markdown_text(text.as_ref()), window, cx)
            .style(TextViewStyle::default().paragraph_gap(rems(0.0)))
            .selectable(true)
            .cursor(CursorStyle::IBeam)
            .into_any_element()
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) fn render_copyable_doi(
        &self,
        container_id: &'static str,
        text_id: &'static str,
        doi: &str,
        display: &str,
        identity_color: gpui::Hsla,
        palette: ReaderPalette,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let doi_text = self.selectable_reference_text(text_id, display, window, cx);
        let copy_doi = doi.to_owned();
        let copy_progress = self.doi_copy_started.map_or(0.0, |started| {
            (Instant::now().duration_since(started).as_secs_f32()
                / DOI_COPY_FEEDBACK_DURATION.as_secs_f32())
            .clamp(0.0, 1.0)
        });
        let copy_scale = 1.0 + (copy_progress * std::f32::consts::PI).sin() * 0.12;
        let copy_id = if container_id == "reference-hero-doi" {
            "copy-reference-hero-doi"
        } else {
            "copy-reference-citation-doi"
        };
        div()
            .id(container_id)
            .min_w_0()
            .flex()
            .items_center()
            .gap_1()
            .text_color(palette.text_secondary)
            .child(Icon::new(IconName::ExternalLink).size(px(13.0)))
            .child(
                div()
                    .min_w_0()
                    .flex_1()
                    .overflow_hidden()
                    .whitespace_nowrap()
                    .text_ellipsis()
                    .child(doi_text),
            )
            .child(
                div()
                    .id(copy_id)
                    .size(px(26.0))
                    .flex_none()
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded_md()
                    .cursor_pointer()
                    .text_color(identity_color)
                    .hover(|button| button.opacity(0.72))
                    .active(|button| button.opacity(0.52))
                    .on_click(cx.listener(move |reader, _, window, cx| {
                        reader.copy_reference_doi(copy_doi.clone(), window, cx);
                        cx.stop_propagation();
                    }))
                    .child(
                        Icon::new(if copy_progress > 0.0 {
                            IconName::Check
                        } else {
                            IconName::Copy
                        })
                        .size(px(14.0))
                        .transform(Transformation::scale(size(copy_scale, copy_scale))),
                    ),
            )
            .into_any_element()
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) fn render_reference_summary_content(
        &self,
        text_id: &'static str,
        text: &str,
        tab: ReferenceSummaryTab,
        metadata: &ScholarlyMetadata,
        identity_color: gpui::Hsla,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let text = self.selectable_reference_text(text_id, text, window, cx);
        let semantic_scholar_url = (tab == ReferenceSummaryTab::Tldr
            && metadata.source == ScholarlySource::SemanticScholar)
            .then(|| metadata.landing_url.clone())
            .flatten();
        let source_id = if text_id == "reference-summary-current-text" {
            "reference-summary-current-source"
        } else {
            "reference-summary-previous-source"
        };
        div()
            .min_w_0()
            .child(text)
            .when_some(semantic_scholar_url, |content, url| {
                content.child(
                    div()
                        .id(source_id)
                        .mt_3()
                        .flex()
                        .items_center()
                        .gap_1()
                        .text_xs()
                        .font_weight(FontWeight::MEDIUM)
                        .text_color(identity_color)
                        .cursor_pointer()
                        .hover(|link| link.opacity(0.72))
                        .active(|link| link.opacity(0.52))
                        .on_click(cx.listener(move |reader, _, _, cx| {
                            if let Err(error) = open::that_detached(&url) {
                                reader.warning = Some(
                                    format!("Could not open Semantic Scholar: {error}").into(),
                                );
                            }
                            cx.stop_propagation();
                            cx.notify();
                        }))
                        .child("Provided by Semantic Scholar")
                        .child(Icon::new(IconName::ExternalLink).size(px(12.0))),
                )
            })
            .into_any_element()
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) fn render_reference_citation(
        &self,
        metadata: &ScholarlyMetadata,
        authors: &str,
        journal: &str,
        identity_color: gpui::Hsla,
        palette: ReaderPalette,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let progress = self.reference_citation_expansion.value();
        let expanded_height = citation_expanded_height(authors, journal, metadata.doi.is_some());
        let compact = compact_reference_panel_citation(metadata);
        let compact_text =
            self.selectable_reference_text("reference-citation-compact-text", &compact, window, cx);
        let authors_text =
            self.selectable_reference_text("reference-citation-authors-text", authors, window, cx);
        let journal_text =
            self.selectable_reference_text("reference-citation-journal-text", journal, window, cx);
        let year = metadata
            .year
            .map(|year| year.to_string())
            .unwrap_or_else(|| "Year unavailable".to_owned());
        let year_text =
            self.selectable_reference_text("reference-citation-year-text", &year, window, cx);
        let expanded_doi = metadata.doi.as_deref().map(|doi| {
            self.render_copyable_doi(
                "reference-citation-doi",
                "reference-citation-doi-text",
                doi,
                doi,
                identity_color,
                palette,
                window,
                cx,
            )
        });
        div()
            .child(
                div()
                    .id("reference-citation-toggle")
                    .min_h(px(46.0))
                    .px_2()
                    .flex()
                    .items_center()
                    .gap_2()
                    .rounded_md()
                    .cursor_pointer()
                    .hover(|row| row.bg(palette.control_hover))
                    .active(|row| row.bg(palette.control_pressed))
                    .on_click(cx.listener(|reader, _, window, cx| {
                        reader.toggle_reference_citation(window, cx);
                        cx.stop_propagation();
                    }))
                    .child(
                        Icon::new(IconName::CircleUser)
                            .size(px(14.0))
                            .text_color(identity_color),
                    )
                    .child(
                        div()
                            .min_w_0()
                            .flex_1()
                            .overflow_hidden()
                            .whitespace_nowrap()
                            .text_ellipsis()
                            .text_xs()
                            .text_color(palette.text_secondary)
                            .child(compact_text),
                    )
                    .child(
                        div()
                            .size(px(24.0))
                            .flex_none()
                            .flex()
                            .items_center()
                            .justify_center()
                            .rounded_full()
                            .bg(identity_color.opacity(0.10))
                            .child(
                                Icon::new(if progress > 0.5 {
                                    IconName::ChevronUp
                                } else {
                                    IconName::ChevronDown
                                })
                                .size(px(14.0))
                                .text_color(identity_color),
                            ),
                    ),
            )
            .child(
                div()
                    .h(px(expanded_height * progress))
                    .opacity(progress)
                    .overflow_hidden()
                    .child(
                        div()
                            .px_2()
                            .pt_2()
                            .pb_3()
                            .child(
                                div()
                                    .flex()
                                    .items_start()
                                    .gap_3()
                                    .child(
                                        Icon::new(IconName::CircleUser)
                                            .size(px(14.0))
                                            .text_color(palette.text_tertiary),
                                    )
                                    .child(
                                        div()
                                            .min_w_0()
                                            .flex_1()
                                            .text_sm()
                                            .line_height(px(20.0))
                                            .text_color(palette.text)
                                            .child(authors_text),
                                    ),
                            )
                            .child(
                                div()
                                    .mt_3()
                                    .flex()
                                    .items_start()
                                    .gap_3()
                                    .child(
                                        Icon::new(IconName::BookOpen)
                                            .size(px(14.0))
                                            .text_color(palette.text_tertiary),
                                    )
                                    .child(
                                        div()
                                            .min_w_0()
                                            .flex_1()
                                            .text_sm()
                                            .line_height(px(20.0))
                                            .text_color(palette.text)
                                            .child(journal_text),
                                    ),
                            )
                            .child(
                                div()
                                    .mt_3()
                                    .flex()
                                    .items_start()
                                    .gap_3()
                                    .child(
                                        Icon::new(IconName::Calendar)
                                            .size(px(14.0))
                                            .text_color(palette.text_tertiary),
                                    )
                                    .child(
                                        div()
                                            .min_w_0()
                                            .flex_1()
                                            .text_sm()
                                            .line_height(px(20.0))
                                            .text_color(palette.text)
                                            .child(year_text),
                                    ),
                            )
                            .when_some(expanded_doi, |details, doi| {
                                details.child(
                                    div()
                                        .mt_3()
                                        .min_w_0()
                                        .pl(px(26.0))
                                        .text_sm()
                                        .line_height(px(20.0))
                                        .child(doi),
                                )
                            }),
                    ),
            )
            .into_any_element()
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) fn render_reference_summary(
        &self,
        metadata: &ScholarlyMetadata,
        panel_width: f32,
        summary_height: f32,
        identity_color: gpui::Hsla,
        palette: ReaderPalette,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<gpui::AnyElement> {
        let has_tldr = metadata.tldr_text.is_some();
        let has_abstract = metadata.abstract_text.is_some();
        if !has_tldr && !has_abstract {
            return None;
        }
        let current = if summary_text(metadata, self.reference_summary_tab).is_some() {
            self.reference_summary_tab
        } else if has_tldr {
            ReferenceSummaryTab::Tldr
        } else {
            ReferenceSummaryTab::Abstract
        };
        let current_text = summary_text(metadata, current).unwrap_or_default();
        let current_view = self.render_reference_summary_content(
            "reference-summary-current-text",
            current_text,
            current,
            metadata,
            identity_color,
            window,
            cx,
        );
        let transition = self.reference_summary_transition.value();
        let switching = self.reference_summary_transition.is_animating()
            && self.reference_summary_previous_tab != current
            && summary_text(metadata, self.reference_summary_previous_tab).is_some();
        let previous_view = switching.then(|| {
            self.render_reference_summary_content(
                "reference-summary-previous-text",
                summary_text(metadata, self.reference_summary_previous_tab).unwrap_or_default(),
                self.reference_summary_previous_tab,
                metadata,
                identity_color,
                window,
                cx,
            )
        });
        let direction = if self.reference_summary_previous_tab == ReferenceSummaryTab::Tldr
            && current == ReferenceSummaryTab::Abstract
        {
            1.0
        } else {
            -1.0
        };
        let slide_width = (panel_width - 40.0).max(200.0);
        let summary_pane = |id: &'static str, text: gpui::AnyElement, offset: f32, opacity: f32| {
            div()
                .id(id)
                .absolute()
                .top_0()
                .bottom_0()
                .left(px(offset))
                .w_full()
                .opacity(opacity)
                .overflow_y_scroll()
                .pl_4()
                .pr_2()
                .py_2()
                .border_l_2()
                .border_color(identity_color.opacity(0.52))
                .text_sm()
                .line_height(px(21.0))
                .text_color(palette.text_secondary)
                .child(text)
        };
        let current_offset = if switching {
            direction * (1.0 - transition) * slide_width
        } else {
            0.0
        };
        let previous_offset = -direction * transition * slide_width;
        Some(
            div()
                .mt_5()
                .child(
                    div()
                        .h(px(34.0))
                        .flex()
                        .items_center()
                        .gap_1()
                        .when(has_tldr && has_abstract, |tabs| {
                            tabs.child(reference_summary_tab_button(
                                "reference-summary-tldr-tab",
                                "TL;DR",
                                ReferenceSummaryTab::Tldr,
                                current,
                                identity_color,
                                palette,
                                cx,
                            ))
                            .child(reference_summary_tab_button(
                                "reference-summary-abstract-tab",
                                "Abstract",
                                ReferenceSummaryTab::Abstract,
                                current,
                                identity_color,
                                palette,
                                cx,
                            ))
                        })
                        .when(!(has_tldr && has_abstract), |label| {
                            label.child(
                                div()
                                    .text_xs()
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .text_color(identity_color)
                                    .child(if has_tldr { "TL;DR" } else { "ABSTRACT" }),
                            )
                        }),
                )
                .child(
                    div()
                        .relative()
                        .h(px(summary_height))
                        .min_w_0()
                        .overflow_hidden()
                        .when_some(previous_view, |panes, previous_view| {
                            panes.child(summary_pane(
                                "reference-summary-previous-pane",
                                previous_view,
                                previous_offset,
                                (1.0 - transition * 0.28).clamp(0.0, 1.0),
                            ))
                        })
                        .child(summary_pane(
                            "reference-summary-current-pane",
                            current_view,
                            current_offset,
                            if switching {
                                (0.72 + transition * 0.28).clamp(0.0, 1.0)
                            } else {
                                1.0
                            },
                        )),
                )
                .into_any_element(),
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) fn render_reference_action(
        &self,
        id: &'static str,
        icon: IconName,
        label: &'static str,
        url: String,
        primary: bool,
        identity_color: gpui::Hsla,
        palette: ReaderPalette,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        div()
            .id(id)
            .h(px(48.0))
            .min_w_0()
            .flex_1()
            .px_2()
            .flex()
            .items_center()
            .justify_center()
            .gap_1()
            .cursor_pointer()
            .hover(|row| row.opacity(0.72))
            .active(|row| row.opacity(0.52))
            .on_click(cx.listener(move |reader, _, _, cx| {
                if let Err(error) = open::that_detached(&url) {
                    reader.warning = Some(format!("Could not open reference: {error}").into());
                }
                cx.stop_propagation();
                cx.notify();
            }))
            .child(
                div()
                    .size(px(26.0))
                    .flex_none()
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded_md()
                    .bg(if primary {
                        identity_color
                    } else {
                        identity_color.opacity(0.12)
                    })
                    .text_color(if primary {
                        palette.accent_foreground
                    } else {
                        identity_color
                    })
                    .child(Icon::new(icon).size(px(13.0))),
            )
            .child(
                div()
                    .min_w_0()
                    .whitespace_nowrap()
                    .text_ellipsis()
                    .text_xs()
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(if primary {
                        identity_color
                    } else {
                        palette.text
                    })
                    .child(label),
            )
            .into_any_element()
    }
}

pub(super) fn text_bounds_overlap(left: TextBounds, right: TextBounds) -> bool {
    left.left < right.right
        && left.right > right.left
        && left.top < right.bottom
        && left.bottom > right.top
}

pub(super) fn reference_panel_width(full_width: f32) -> f32 {
    let maximum =
        (full_width - MIN_DOCUMENT_VIEWPORT_WIDTH - FLUID_PANEL_HORIZONTAL_MARGIN * 2.0).max(0.0);
    (full_width * 0.36)
        .clamp(REFERENCE_PANEL_MIN_WIDTH, REFERENCE_PANEL_MAX_WIDTH)
        .min(maximum)
}

pub(super) fn reference_panel_extent(full_width: f32, progress: f32) -> f32 {
    let width = reference_panel_width(full_width);
    if width <= 0.0 {
        0.0
    } else {
        (width + FLUID_PANEL_HORIZONTAL_MARGIN * 2.0) * progress.clamp(0.0, 1.0)
    }
}

pub(super) fn scientific_reference_matches(
    reference: &ScientificReference,
    target_page: usize,
    resolved: Option<&ResolvedInternalLink>,
    rough_y: Option<f32>,
) -> bool {
    if reference.page != target_page {
        return false;
    }
    if resolved.is_some_and(|resolved| {
        resolved.text_runs.iter().any(|resolved_run| {
            reference
                .text_runs
                .iter()
                .any(|reference_run| text_bounds_overlap(*resolved_run, *reference_run))
        })
    }) {
        return true;
    }
    let target_y = resolved
        .and_then(|resolved| resolved.y_fraction)
        .or(rough_y);
    let top = reference
        .text_runs
        .iter()
        .map(|run| run.top)
        .min_by(f32::total_cmp);
    let bottom = reference
        .text_runs
        .iter()
        .map(|run| run.bottom)
        .max_by(f32::total_cmp);
    matches!((target_y, top, bottom), (Some(y), Some(top), Some(bottom)) if y >= top - 0.012 && y <= bottom + 0.012)
}

pub(super) fn union_text_bounds(left: TextBounds, right: TextBounds) -> TextBounds {
    TextBounds {
        left: left.left.min(right.left),
        top: left.top.min(right.top),
        right: left.right.max(right.right),
        bottom: left.bottom.max(right.bottom),
    }
}

pub(super) fn complete_grouped_reference_indices(
    references: &[ScientificReference],
    source: &str,
    primary_number: u32,
) -> Option<Vec<usize>> {
    let numbers = grouped_citation_numbers(source)?;
    let indices = numbers
        .iter()
        .map(|number| {
            references
                .iter()
                .position(|reference| reference.number == *number)
        })
        .collect::<Option<Vec<_>>>()?;
    indices
        .iter()
        .any(|index| references[*index].number == primary_number)
        .then_some(indices)
}

pub(super) fn adjacent_group_index(current: usize, count: usize, forward: bool) -> usize {
    if count <= 1 {
        return 0;
    }
    let current = current.min(count - 1);
    if forward {
        (current + 1) % count
    } else {
        (current + count - 1) % count
    }
}

pub(super) fn measured_preview_width(lines: &[&str], minimum: f32, maximum: f32) -> f32 {
    let longest = lines
        .iter()
        .map(|line| line.chars().count().min(64))
        .max()
        .unwrap_or(0);
    (longest as f32 * 6.6 + 54.0).clamp(minimum, maximum)
}

pub(super) fn escape_markdown_text(text: &str) -> String {
    let mut escaped = String::with_capacity(text.len());
    for character in text.chars() {
        if matches!(
            character,
            '\\' | '`' | '*' | '_' | '{' | '}' | '[' | ']' | '<' | '>' | '#' | '+' | '-' | '!'
        ) {
            escaped.push('\\');
        }
        escaped.push(character);
    }
    escaped
}

pub(super) fn summary_text(metadata: &ScholarlyMetadata, tab: ReferenceSummaryTab) -> Option<&str> {
    match tab {
        ReferenceSummaryTab::Tldr => metadata.tldr_text.as_deref(),
        ReferenceSummaryTab::Abstract => metadata.abstract_text.as_deref(),
    }
}

#[allow(clippy::too_many_arguments)]
pub(super) fn reference_summary_tab_button(
    id: &'static str,
    label: &'static str,
    tab: ReferenceSummaryTab,
    selected: ReferenceSummaryTab,
    identity_color: gpui::Hsla,
    palette: ReaderPalette,
    cx: &mut Context<PdfReader>,
) -> gpui::AnyElement {
    let active = tab == selected;
    div()
        .id(id)
        .h(px(28.0))
        .px_3()
        .flex()
        .items_center()
        .rounded_full()
        .cursor_pointer()
        .bg(if active {
            identity_color.opacity(0.14)
        } else {
            palette.surface
        })
        .text_xs()
        .font_weight(if active {
            FontWeight::SEMIBOLD
        } else {
            FontWeight::NORMAL
        })
        .text_color(if active {
            identity_color
        } else {
            palette.text_secondary
        })
        .hover(move |button| button.bg(identity_color.opacity(0.11)))
        .on_click(cx.listener(move |reader, _, window, cx| {
            reader.select_reference_summary(tab, window, cx);
            cx.stop_propagation();
        }))
        .child(label)
        .into_any_element()
}

pub(super) fn citation_expanded_height(authors: &str, journal: &str, has_doi: bool) -> f32 {
    let author_lines = authors.chars().count().div_ceil(44).clamp(1, 5);
    let journal_lines = journal.chars().count().div_ceil(44).clamp(1, 3);
    72.0 + (author_lines + journal_lines) as f32 * 20.0 + if has_doi { 40.0 } else { 0.0 }
}

pub(super) fn link_hover_candidate_needs_restart(
    pending: PendingLinkHover,
    target: Option<PreviewTarget>,
    position: Point<Pixels>,
) -> bool {
    if pending.target != target {
        return true;
    }
    let dx = f32::from(position.x - pending.position.x);
    let dy = f32::from(position.y - pending.position.y);
    dx.mul_add(dx, dy * dy) > LINK_HOVER_STABILITY_RADIUS * LINK_HOVER_STABILITY_RADIUS
}

pub(super) fn reference_identity_color(title: &str, palette: ReaderPalette) -> gpui::Hsla {
    let signature = title.bytes().fold(0_u32, |hash, byte| {
        hash.wrapping_mul(16777619).wrapping_add(u32::from(byte))
    });
    match signature % 4 {
        0 => palette.blue,
        1 => palette.pink,
        2 => palette.purple,
        _ => palette.green,
    }
}

pub(super) fn reference_hero_height(title: &str) -> f32 {
    let wrapped_lines = title.chars().count().div_ceil(28).clamp(1, 5);
    116.0 + (wrapped_lines.saturating_sub(1) as f32 * 24.0)
}

pub(super) fn reference_preview_width(
    state: Option<&ScholarlyMetadataState>,
    expansion_progress: f32,
) -> f32 {
    let compact_width = 232.0;
    let Some(ScholarlyMetadataState::Ready(metadata)) = state else {
        return compact_width;
    };
    let title = compact_words(&metadata.title, 11);
    let citation = compact_citation_line(metadata);
    let expanded_width =
        measured_preview_width(&[title.as_str(), citation.as_str()], 280.0, LINK_CARD_WIDTH);
    compact_width + (expanded_width - compact_width) * expansion_progress.clamp(0.0, 1.0)
}

pub(super) fn compact_words(text: &str, maximum_words: usize) -> String {
    let words = text.split_whitespace().collect::<Vec<_>>();
    if words.len() <= maximum_words {
        return words.join(" ");
    }
    format!("{}…", words[..maximum_words].join(" "))
}

pub(super) fn author_last_name(author: &str) -> &str {
    author
        .split_whitespace()
        .last()
        .unwrap_or(author)
        .trim_matches(|character: char| matches!(character, ',' | ';'))
}

pub(super) fn compact_authors(authors: &[String]) -> String {
    if authors.is_empty() {
        return "Unknown author".to_owned();
    }
    if authors.len() > 3 {
        return format!("{} et al.", author_last_name(&authors[0]));
    }
    let joined = authors
        .iter()
        .map(|author| author_last_name(author))
        .collect::<Vec<_>>()
        .join(", ");
    if joined.chars().count() > 38 {
        format!("{} et al.", author_last_name(&authors[0]))
    } else {
        joined
    }
}

pub(super) fn compact_journal(journal: &str) -> String {
    let mut result = journal.chars().take(28).collect::<String>();
    if journal.chars().count() > 28 {
        result.push('…');
    }
    result
}

pub(super) fn middle_truncate(text: &str, maximum_characters: usize) -> String {
    let characters = text.chars().collect::<Vec<_>>();
    if characters.len() <= maximum_characters || maximum_characters < 3 {
        return text.to_owned();
    }
    let left = (maximum_characters - 1).div_ceil(2);
    let right = maximum_characters - 1 - left;
    format!(
        "{}…{}",
        characters[..left].iter().collect::<String>(),
        characters[characters.len() - right..]
            .iter()
            .collect::<String>()
    )
}

pub(super) fn compact_reference_panel_citation(metadata: &ScholarlyMetadata) -> String {
    let mut parts = vec![compact_authors(&metadata.authors)];
    if let Some(journal) = metadata
        .journal_short
        .as_deref()
        .or(metadata.journal.as_deref())
    {
        parts.push(middle_truncate(journal, 22));
    }
    if let Some(year) = metadata.year {
        parts.push(year.to_string());
    }
    parts.join(" · ")
}

pub(super) fn compact_citation_line(metadata: &ScholarlyMetadata) -> String {
    let mut parts = vec![compact_authors(&metadata.authors)];
    if let Some(journal) = metadata.journal.as_deref() {
        parts.push(compact_journal(journal));
    }
    if let Some(year) = metadata.year {
        parts.push(year.to_string());
    }
    parts.join(" · ")
}

pub(super) fn loading_source_icon(color: gpui::Hsla) -> gpui::AnyElement {
    Icon::new(IconName::LoaderCircle)
        .size(px(15.0))
        .text_color(color)
        .with_animation(
            "reference-source-spinner",
            Animation::new(Duration::from_millis(800))
                .repeat()
                .with_easing(ease_in_out),
            |icon, delta| icon.transform(Transformation::rotate(percentage(delta))),
        )
        .into_any_element()
}
