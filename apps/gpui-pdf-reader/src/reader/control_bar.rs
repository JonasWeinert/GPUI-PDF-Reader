use super::*;
use key_workspace_core::{
    ControlBarAuxiliary, ControlBarCard, ControlBarEvent, ControlBarInteraction, ControlBarItem,
    ControlBarItemKind, ControlBarItemState, ControlBarPresentation, ControlBarRegion,
    ControlBarSnapshot, ControlIcon,
};

pub(crate) const PDF_CONTROL_ZOOM_OUT: &str = "pdf.zoom-out";
pub(crate) const PDF_CONTROL_ZOOM_IN: &str = "pdf.zoom-in";
pub(crate) const PDF_CONTROL_FIT_WIDTH: &str = "pdf.fit-width";
pub(crate) const PDF_CONTROL_TITLE: &str = "pdf.title";
pub(crate) const PDF_CONTROL_SEARCH: &str = "pdf.search";
pub(crate) const PDF_CONTROL_COMMENTS: &str = "pdf.comments";
pub(crate) const PDF_CONTROL_DARK_MODE: &str = "pdf.dark-mode";
pub(crate) const PDF_SEARCH_EXPANDED_WIDTHS: [f32; 3] = [420.0, 300.0, 180.0];
const MAX_CONTROL_BAR_SEARCH_CARDS: usize = 256;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct PdfControlBarSignature {
    generation: u64,
    document_open: bool,
    zoom: u32,
    fit_width: bool,
    comments_selected: bool,
    search_expanded: bool,
    search_revision: u64,
    search_results: usize,
    search_pages: usize,
    search_active: Option<SearchMatchId>,
    search_complete: bool,
    dark_theme: bool,
    pdf_dark_mode: bool,
}

#[derive(Clone, Debug)]
pub(crate) struct PdfControlBarMetadata {
    pub title: String,
    pub path: String,
    pub current_page: usize,
    pub page_count: usize,
    pub zoom_percent: u32,
}

impl PdfReader {
    pub(crate) fn control_bar_metadata(&self) -> Option<PdfControlBarMetadata> {
        let document = self.document.as_ref()?;
        let page_count = document.pages.len();
        let current_page = self
            .layout()
            .map(|layout| layout.current_page(self.scroll.y, self.viewport_height) + 1)
            .unwrap_or(1)
            .min(page_count.max(1));
        Some(PdfControlBarMetadata {
            title: document.title.clone().unwrap_or_else(|| {
                document.path.file_name().map_or_else(
                    || "PDF document".to_owned(),
                    |name| name.to_string_lossy().into_owned(),
                )
            }),
            path: document.path.display().to_string(),
            current_page,
            page_count,
            zoom_percent: (self.zoom * 100.0).round() as u32,
        })
    }

    pub(crate) fn control_bar_signature(
        &self,
        search_expanded: bool,
        dark_theme: bool,
    ) -> PdfControlBarSignature {
        PdfControlBarSignature {
            generation: self.generation,
            document_open: self.document.is_some(),
            zoom: self.zoom.to_bits(),
            fit_width: self.fit_width,
            comments_selected: self.sidebar.panel == SidePanel::Comments
                && self.sidebar.target > 0.5,
            search_expanded,
            search_revision: self.search.revision,
            search_results: self.search.order.len(),
            search_pages: self.search.searched_pages,
            search_active: self.search.active,
            search_complete: self.search.complete,
            dark_theme,
            pdf_dark_mode: self.pdf_dark_mode_enabled,
        }
    }

    pub(crate) fn control_bar_snapshot(
        &self,
        search_expanded: bool,
        dark_theme: bool,
    ) -> ControlBarSnapshot {
        let document_open = self.document.is_some();
        let (zoom_out_enabled, zoom_in_enabled) = zoom_controls_enabled(document_open, self.zoom);
        let title = self.document.as_ref().map_or_else(
            || "GPUI PDF Reader".to_owned(),
            |document| {
                document.title.clone().unwrap_or_else(|| {
                    document.path.file_name().map_or_else(
                        || "PDF document".to_owned(),
                        |name| name.to_string_lossy().into_owned(),
                    )
                })
            },
        );
        let mut snapshot = ControlBarSnapshot::new(
            self.workspace_view_id,
            self.zoom_render_revision
                .wrapping_add(self.search.revision.rotate_left(7))
                .wrapping_add((self.search.order.len() as u64).rotate_left(17))
                .wrapping_add(u64::from(search_expanded).rotate_left(31)),
        );

        let mut title_item = ControlBarItem::new(
            PDF_CONTROL_TITLE,
            ControlBarRegion::Leading,
            ControlBarItemKind::Display,
            ControlBarPresentation::new(title, [0.0, 0.0, 0.0], 100)
                .short_label("PDF")
                .icon(ControlIcon::Document)
                .tooltip("Show document details")
                .max_width(),
        );
        title_item.state.enabled = document_open;
        snapshot.items.push(title_item);

        let mut zoom_out = ControlBarItem::new(
            PDF_CONTROL_ZOOM_OUT,
            ControlBarRegion::Leading,
            ControlBarItemKind::Button,
            ControlBarPresentation::new("Zoom out", [32.0, 32.0, 32.0], 70)
                .short_label("Out")
                .icon(ControlIcon::Minus)
                .tooltip("Zoom out")
                .icon_only(),
        );
        zoom_out.state.enabled = zoom_out_enabled;
        snapshot.items.push(zoom_out);

        let mut zoom_in = ControlBarItem::new(
            PDF_CONTROL_ZOOM_IN,
            ControlBarRegion::Leading,
            ControlBarItemKind::Button,
            ControlBarPresentation::new("Zoom in", [32.0, 32.0, 32.0], 70)
                .short_label("In")
                .icon(ControlIcon::Add)
                .tooltip("Zoom in")
                .icon_only(),
        );
        zoom_in.state.enabled = zoom_in_enabled;
        snapshot.items.push(zoom_in);

        let mut search = ControlBarItem::new(
            PDF_CONTROL_SEARCH,
            ControlBarRegion::Trailing,
            if search_expanded {
                ControlBarItemKind::TextInput
            } else {
                ControlBarItemKind::Button
            },
            ControlBarPresentation::new(
                "Search",
                if search_expanded {
                    PDF_SEARCH_EXPANDED_WIDTHS
                } else {
                    [32.0, 32.0, 32.0]
                },
                100,
            )
            .short_label("Find")
            .icon(ControlIcon::Search)
            .tooltip("Search this document")
            .icon_only(),
        );
        search.state = ControlBarItemState {
            enabled: document_open,
            selected: search_expanded,
            expanded: search_expanded,
            value: Some(self.search.query.clone()),
            ..ControlBarItemState::default()
        };
        snapshot.items.push(search);

        let mut comments = ControlBarItem::new(
            PDF_CONTROL_COMMENTS,
            ControlBarRegion::Trailing,
            ControlBarItemKind::Button,
            ControlBarPresentation::new("Comments", [32.0, 32.0, 32.0], 60)
                .short_label("Notes")
                .icon(ControlIcon::Comments)
                .tooltip("Show comments")
                .icon_only(),
        );
        comments.state.enabled = document_open;
        comments.state.selected =
            self.sidebar.panel == SidePanel::Comments && self.sidebar.target > 0.5;
        snapshot.items.push(comments);

        if dark_theme {
            let mut dark_mode = ControlBarItem::new(
                PDF_CONTROL_DARK_MODE,
                ControlBarRegion::Trailing,
                ControlBarItemKind::Button,
                ControlBarPresentation::new("Invert PDF colors", [32.0, 32.0, 32.0], 60)
                    .icon(if self.pdf_dark_mode_enabled {
                        ControlIcon::Moon
                    } else {
                        ControlIcon::Sun
                    })
                    .tooltip(if self.pdf_dark_mode_enabled {
                        "Show the original PDF colors"
                    } else {
                        "Invert PDF colors for this dark theme"
                    })
                    .icon_only(),
            );
            dark_mode.state.enabled = document_open;
            dark_mode.state.selected = self.pdf_dark_mode_enabled;
            snapshot.items.push(dark_mode);
        }

        if search_expanded {
            let active_ordinal = self
                .search
                .active
                .and_then(|active| self.search.order.iter().position(|id| *id == active));
            let card_range = control_bar_card_range(self.search.order.len(), active_ordinal);
            let cards = self
                .search
                .order
                .iter()
                .skip(card_range.start)
                .take(card_range.len())
                .filter_map(|id| {
                    self.search.pages.get(&id.page).and_then(|matches| {
                        matches
                            .iter()
                            .find(|result| result.id == *id)
                            .map(|result| ControlBarCard {
                                id: format!("pdf.search.{}.{}.{}", id.page, id.start, id.end)
                                    .into(),
                                eyebrow: Some(format!("p {}", id.page + 1)),
                                text: result.preview.to_string(),
                                emphasized: Some(result.preview_match.clone()),
                                selected: self.search.active == Some(*id),
                            })
                    })
                })
                .collect::<Vec<_>>();
            let label = if self.search.query.trim().is_empty() {
                "Type to search".to_owned()
            } else if self.search.complete {
                let total = self.search.order.len();
                if total > cards.len() {
                    format!(
                        "{total} results · showing {}–{}",
                        card_range.start + 1,
                        card_range.end
                    )
                } else {
                    format!("{total} result{}", if total == 1 { "" } else { "s" })
                }
            } else {
                let pages = self
                    .document
                    .as_ref()
                    .map_or(0, |document| document.pages.len());
                format!("Searching {} / {} pages", self.search.searched_pages, pages)
            };
            snapshot.auxiliary = Some(ControlBarAuxiliary {
                label,
                loading: !self.search.complete,
                cards,
            });
        }

        snapshot
    }

    pub(crate) fn control_bar_search_field(&self) -> Entity<TextField> {
        self.search_field.clone()
    }

    fn control_bar_activate_search_card(
        &mut self,
        control: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(value) = control.strip_prefix("pdf.search.") else {
            return;
        };
        let mut parts = value.split('.');
        let (Some(page), Some(start), Some(end), None) =
            (parts.next(), parts.next(), parts.next(), parts.next())
        else {
            return;
        };
        let (Ok(page), Ok(start), Ok(end)) = (
            page.parse::<usize>(),
            start.parse::<usize>(),
            end.parse::<usize>(),
        ) else {
            return;
        };
        self.activate_search_match(SearchMatchId { page, start, end }, window, cx);
    }

    fn control_bar_press(&mut self, control: &str, window: &mut Window, cx: &mut Context<Self>) {
        match control {
            PDF_CONTROL_ZOOM_OUT => self.zoom_out(&ZoomOut, window, cx),
            PDF_CONTROL_ZOOM_IN => self.zoom_in(&ZoomIn, window, cx),
            PDF_CONTROL_FIT_WIDTH => self.fit_width(&FitWidth, window, cx),
            PDF_CONTROL_COMMENTS => self.toggle_comments(&ToggleComments, window, cx),
            PDF_CONTROL_DARK_MODE => self.toggle_pdf_dark_mode(window, cx),
            _ => {}
        }
    }

    pub(crate) fn control_bar_close_search(&mut self, cx: &mut Context<Self>) {
        self.reset_search(cx);
    }

    pub(crate) fn control_bar_event(
        &mut self,
        event: ControlBarEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if event.owner != self.workspace_view_id {
            return;
        }
        match event.interaction {
            ControlBarInteraction::Pressed => {
                self.control_bar_press(event.control.as_str(), window, cx)
            }
            ControlBarInteraction::ActivatedCard => {
                self.control_bar_activate_search_card(event.control.as_str(), window, cx)
            }
            ControlBarInteraction::ValueChanged(_)
            | ControlBarInteraction::Submitted
            | ControlBarInteraction::Cancelled => {}
        }
    }
}

fn control_bar_card_range(total: usize, active: Option<usize>) -> std::ops::Range<usize> {
    if total <= MAX_CONTROL_BAR_SEARCH_CARDS {
        return 0..total;
    }
    let half = MAX_CONTROL_BAR_SEARCH_CARDS / 2;
    let start = active
        .unwrap_or(0)
        .saturating_sub(half)
        .min(total - MAX_CONTROL_BAR_SEARCH_CARDS);
    start..start + MAX_CONTROL_BAR_SEARCH_CARDS
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn horizontal_result_cards_are_bounded_and_keep_the_active_result_visible() {
        assert_eq!(control_bar_card_range(5, None), 0..5);
        let middle = control_bar_card_range(20_000, Some(10_000));
        assert_eq!(middle.len(), MAX_CONTROL_BAR_SEARCH_CARDS);
        assert!(middle.contains(&10_000));
        let tail = control_bar_card_range(20_000, Some(19_999));
        assert_eq!(tail.end, 20_000);
        assert!(tail.contains(&19_999));
    }
}
