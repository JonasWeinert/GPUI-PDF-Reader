use super::*;

const SEARCH_DEBOUNCE: Duration = Duration::from_millis(180);

#[derive(Debug, Default)]
pub(super) struct SearchState {
    pub(super) query: String,
    pub(super) input_error: Option<SharedString>,
    pub(super) revision: u64,
    pub(super) pages: BTreeMap<usize, Arc<[SearchMatch]>>,
    pub(super) order: Vec<SearchMatchId>,
    pub(super) active: Option<SearchMatchId>,
    pub(super) searched_pages: usize,
    pub(super) total_highlight_runs: usize,
    pub(super) complete: bool,
    pub(super) truncated: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum SearchListRow {
    Page(usize),
    Match { id: SearchMatchId, ordinal: usize },
}

pub(super) fn search_list_rows(order: &[SearchMatchId]) -> Vec<SearchListRow> {
    let mut rows = Vec::with_capacity(order.len().saturating_add(8));
    let mut previous_page = None;
    for (ordinal, id) in order.iter().copied().enumerate() {
        if previous_page != Some(id.page) {
            rows.push(SearchListRow::Page(id.page));
            previous_page = Some(id.page);
        }
        rows.push(SearchListRow::Match { id, ordinal });
    }
    rows
}

fn search_preview_text(result: &SearchMatch, palette: ReaderPalette) -> StyledText {
    let matched = result.preview_match.clone();
    let mut runs = Vec::with_capacity(3);
    for (range, weight) in [
        (0..matched.start, FontWeight::NORMAL),
        (matched.clone(), FontWeight::BOLD),
        (matched.end..result.preview.len(), FontWeight::NORMAL),
    ] {
        if !range.is_empty() {
            let mut selected_font = font(".SystemUIFont");
            selected_font.weight = weight;
            runs.push(TextRun {
                len: range.len(),
                font: selected_font,
                color: palette.text,
                background_color: None,
                underline: None,
                strikethrough: None,
            });
        }
    }
    StyledText::new(result.preview.clone()).with_runs(runs)
}

pub(super) fn next_search_match_id(
    results: &[SearchMatchId],
    active: Option<SearchMatchId>,
    forward: bool,
) -> Option<SearchMatchId> {
    let len = results.len();
    if len == 0 {
        return None;
    }
    let current = active.and_then(|id| results.iter().position(|result| *result == id));
    let index = match (current, forward) {
        (Some(index), true) => (index + 1) % len,
        (Some(index), false) => (index + len - 1) % len,
        (None, true) => 0,
        (None, false) => len - 1,
    };
    Some(results[index])
}

pub(super) fn search_document_jump(result: &SearchMatch) -> DocumentJump {
    let (x_fraction, y_fraction) = result.highlight_runs.first().map_or((0.5, 0.15), |run| {
        ((run.left + run.right) * 0.5, (run.top + run.bottom) * 0.5)
    });
    DocumentJump::new(result.id.page)
        .position(Some(x_fraction), Some(y_fraction))
        .viewport_anchor_y(0.35)
        .center_horizontal(true)
        .focus(
            result.highlight_runs.clone(),
            NavigationFocusTone::SearchMatch,
            NavigationFocusMotion::Pulse,
        )
}

impl PdfReader {
    pub(super) fn find_document(&mut self, _: &Find, _: &mut Window, cx: &mut Context<Self>) {
        cx.emit(PdfReaderEvent::OpenSearch);
    }

    pub(super) fn next_search_result(
        &mut self,
        _: &NextSearchResult,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.navigate_search(true, window, cx);
    }

    pub(super) fn previous_search_result(
        &mut self,
        _: &PreviousSearchResult,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.navigate_search(false, window, cx);
    }

    pub(super) fn set_search_query(&mut self, query: String, cx: &mut Context<Self>) {
        let cleared_input_error = self.search.input_error.take().is_some();
        if self.search.query == query {
            if cleared_input_error {
                if let Err(error) = SearchQuery::new(&query)
                    && !matches!(error, crate::search::SearchQueryError::Empty)
                {
                    self.search.input_error = Some(error.to_string().into());
                }
                cx.notify();
            }
            return;
        }
        self.search.revision = self.search.revision.wrapping_add(1);
        self.navigation_focus.cancel();
        self.search.query = query.clone();
        self.search.pages.clear();
        self.search.order.clear();
        self.search.active = None;
        self.search.searched_pages = 0;
        self.search.total_highlight_runs = 0;
        self.search.complete = false;
        self.search.truncated = false;
        self.search_list_state.reset(0);
        self.search_debounce_task = None;
        let _ = self
            .worker
            .cancel_search(self.generation, self.search.revision);

        let search_query = match SearchQuery::new(&query) {
            Ok(query) => query,
            Err(crate::search::SearchQueryError::Empty) => {
                self.search.complete = true;
                cx.notify();
                return;
            }
            Err(error) => {
                self.search.complete = true;
                self.search.input_error = Some(error.to_string().into());
                cx.notify();
                return;
            }
        };
        let generation = self.generation;
        let revision = self.search.revision;
        self.search_debounce_task = Some(cx.spawn(async move |weak, cx| {
            cx.background_executor().timer(SEARCH_DEBOUNCE).await;
            weak.update(cx, |reader, cx| {
                if reader.generation == generation && reader.search.revision == revision {
                    reader.search_debounce_task = None;
                    if !reader.worker.search(generation, revision, search_query) {
                        reader.search.complete = true;
                        reader.warning = Some("The PDF search worker is unavailable".into());
                    }
                    cx.notify();
                }
            })
            .ok();
        }));
        cx.notify();
    }

    pub(super) fn activate_search_match(
        &mut self,
        id: SearchMatchId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(result) = self
            .search
            .pages
            .get(&id.page)
            .and_then(|matches| matches.iter().find(|result| result.id == id))
        else {
            return;
        };
        let jump = search_document_jump(result);
        self.search.active = Some(id);
        if let Some(index) = search_list_rows(&self.search.order).iter().position(
            |row| matches!(row, SearchListRow::Match { id: candidate, .. } if *candidate == id),
        ) {
            self.search_list_state.scroll_to_reveal_item(index);
        }
        self.perform_document_jump(jump, window, cx);
        #[cfg(debug_assertions)]
        {
            self.qa_search_focuses += 1;
        }
    }

    pub(super) fn navigate_search(
        &mut self,
        forward: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(id) = next_search_match_id(&self.search.order, self.search.active, forward) else {
            return;
        };
        self.activate_search_match(id, window, cx);
    }

    pub(super) fn reset_search(&mut self, cx: &mut Context<Self>) {
        let revision = self.search.revision.wrapping_add(1);
        let _ = self.worker.cancel_search(self.generation, revision);
        self.search = SearchState {
            revision,
            complete: true,
            ..SearchState::default()
        };
        self.search_debounce_task = None;
        self.navigation_focus.cancel();
        self.search_list_state.reset(0);
        if !self.search_field.read(cx).text().is_empty()
            && let Err(rejection) = self
                .search_field
                .update(cx, |field, cx| field.set_text("", cx))
        {
            self.search.input_error = Some(rejection.to_string().into());
        }
        cx.notify();
    }

    pub(super) fn render_search_panel(&mut self, cx: &mut Context<Self>) -> gpui::AnyElement {
        let palette = ReaderPalette::from_app(cx);
        let result_count = self.search.order.len();
        let input_error = self.search.input_error.clone();
        let page_count = self
            .document
            .as_ref()
            .map_or(0, |document| document.pages.len());
        let status: SharedString = if let Some(error) = input_error.clone() {
            error
        } else if self.search.query.trim().is_empty() {
            "Type to search the document".into()
        } else if self.search.complete {
            format!(
                "{} result{}{}",
                result_count,
                if result_count == 1 { "" } else { "s" },
                if self.search.truncated {
                    " (limit reached)"
                } else {
                    ""
                }
            )
            .into()
        } else {
            format!(
                "Searching… {} / {} pages",
                self.search.searched_pages, page_count
            )
            .into()
        };
        let active_index = self.search.active.and_then(|active| {
            self.search
                .order
                .iter()
                .position(|candidate| *candidate == active)
        });
        let navigation_enabled = result_count > 0;
        let (empty_title, empty_detail): (SharedString, SharedString) = if input_error.is_some() {
            (
                "Search needs attention".into(),
                "Fix the query above to continue searching.".into(),
            )
        } else if self.search.complete && !self.search.query.trim().is_empty() {
            (
                "No matches".into(),
                "Try a different word or a shorter phrase.".into(),
            )
        } else {
            (
                "Find text in this document".into(),
                "Matches will be highlighted as you type.".into(),
            )
        };
        let results = if result_count == 0 {
            div()
                .flex_1()
                .min_h(px(0.0))
                .flex()
                .flex_col()
                .items_center()
                .justify_center()
                .gap_2()
                .px_6()
                .text_center()
                .child(
                    div()
                        .size(px(38.0))
                        .flex()
                        .items_center()
                        .justify_center()
                        .design_radius(RadiusRole::Pill, &palette.ui)
                        .bg(palette.accent_soft)
                        .text_color(palette.accent)
                        .text_lg()
                        .child(Icon::new(IconName::Search)),
                )
                .child(
                    div()
                        .text_sm()
                        .font_weight(FontWeight::MEDIUM)
                        .text_color(palette.text)
                        .child(empty_title),
                )
                .child(
                    div()
                        .max_w(px(240.0))
                        .text_xs()
                        .line_height(px(18.0))
                        .text_color(palette.text_secondary)
                        .child(empty_detail),
                )
                .into_any_element()
        } else {
            let rows: Arc<[SearchListRow]> = search_list_rows(&self.search.order).into();
            list(
                self.search_list_state.clone(),
                cx.processor(move |reader, index, _window, cx| {
                    let Some(row) = rows.get(index).copied() else {
                        return div().into_any_element();
                    };
                    match row {
                        SearchListRow::Page(page) => div()
                            .h(px(34.0))
                            .w_full()
                            .px_4()
                            .pt_3()
                            .text_xs()
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(palette.text_secondary)
                            .child(format!("PAGE {}", page + 1))
                            .into_any_element(),
                        SearchListRow::Match { id, ordinal } => {
                            let Some(result) =
                                reader.search.pages.get(&id.page).and_then(|matches| {
                                    matches.iter().find(|result| result.id == id)
                                })
                            else {
                                return div().into_any_element();
                            };
                            let preview = search_preview_text(result, palette);
                            let active = reader.search.active == Some(id);
                            div()
                                .h(px(68.0))
                                .w_full()
                                .px_3()
                                .py_1()
                                .child(
                                    div()
                                        .id(("search-result", ordinal))
                                        .size_full()
                                        .overflow_hidden()
                                        .flex()
                                        .design_radius(RadiusRole::Medium, &palette.ui)
                                        .border_1()
                                        .border_color(if active {
                                            palette.accent_border
                                        } else {
                                            palette.separator
                                        })
                                        .bg(if active {
                                            palette.accent_soft
                                        } else {
                                            palette.surface
                                        })
                                        .cursor_pointer()
                                        .hover(move |row| {
                                            row.bg(if active {
                                                palette.accent_soft_hover
                                            } else {
                                                palette.surface_subtle
                                            })
                                        })
                                        .on_click(cx.listener(move |reader, _, window, cx| {
                                            reader.activate_search_match(id, window, cx)
                                        }))
                                        .when(active, |row| {
                                            row.child(
                                                div()
                                                    .w(px(3.0))
                                                    .h_full()
                                                    .flex_none()
                                                    .bg(palette.accent),
                                            )
                                        })
                                        .child(
                                            div()
                                                .flex_1()
                                                .min_w(px(0.0))
                                                .px_3()
                                                .py_2()
                                                .h(px(58.0))
                                                .overflow_hidden()
                                                .text_sm()
                                                .line_height(px(19.0))
                                                .child(preview),
                                        ),
                                )
                                .into_any_element()
                        }
                    }
                }),
            )
            .flex_1()
            .min_h(px(0.0))
            .w_full()
            .bg(palette.surface_subtle)
            .rounded_b_xl()
            .into_any_element()
        };

        div()
            .size_full()
            .flex()
            .flex_col()
            .design_radius(RadiusRole::Large, &palette.ui)
            .bg(palette.surface)
            .text_color(palette.text)
            .child(
                div()
                    .h(px(54.0))
                    .flex_none()
                    .px_4()
                    .flex()
                    .items_center()
                    .justify_between()
                    .border_b_1()
                    .border_color(palette.separator)
                    .child(
                        div()
                            .text_lg()
                            .font_weight(FontWeight::SEMIBOLD)
                            .child("Find in Document"),
                    )
                    .child(Self::chrome_button(
                        palette,
                        "close-search",
                        Icon::new(IconName::Close).size(px(16.0)),
                        ChromeButtonStyle::Ghost,
                        true,
                        cx.listener(|reader, _, window, cx| {
                            reader.toggle_sidebar(SidePanel::Search, window, cx)
                        }),
                    )),
            )
            .child(
                div()
                    .flex_none()
                    .p_4()
                    .pb_3()
                    .flex()
                    .flex_col()
                    .gap_2()
                    .border_b_1()
                    .border_color(palette.separator)
                    .child(self.search_field.clone())
                    .child(
                        div()
                            .h(px(32.0))
                            .flex()
                            .items_center()
                            .justify_between()
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(if input_error.is_some() {
                                        palette.error
                                    } else {
                                        palette.text_secondary
                                    })
                                    .child(if let Some(index) = active_index {
                                        format!("{} of {}", index + 1, result_count).into()
                                    } else {
                                        status
                                    }),
                            )
                            .child(
                                div()
                                    .flex()
                                    .gap_1()
                                    .child(Self::chrome_button(
                                        palette,
                                        "previous-search-result",
                                        Icon::new(IconName::ArrowUp),
                                        ChromeButtonStyle::Ghost,
                                        navigation_enabled,
                                        cx.listener(|reader, _, window, cx| {
                                            reader.navigate_search(false, window, cx)
                                        }),
                                    ))
                                    .child(Self::chrome_button(
                                        palette,
                                        "next-search-result",
                                        Icon::new(IconName::ArrowDown),
                                        ChromeButtonStyle::Ghost,
                                        navigation_enabled,
                                        cx.listener(|reader, _, window, cx| {
                                            reader.navigate_search(true, window, cx)
                                        }),
                                    )),
                            ),
                    ),
            )
            .child(results)
            .into_any_element()
    }
}
