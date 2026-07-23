use super::comments::floating_pill_position;
use super::*;

struct PaintPageOverlayState {
    palette: ReaderPalette,
    selection: Option<TextSelection>,
    active_annotation: Option<AnnotationId>,
    active_search: Option<SearchMatchId>,
    navigation_focus: Option<NavigationFocusFrame>,
    annotation_budget: PaintBudget,
    annotation_geometry_budget: PaintBudget,
    search_budget: PaintBudget,
    selection_budget: PaintBudget,
    extension_overlay_budget: PaintBudget,
}

impl PaintPageOverlayState {
    fn new(
        palette: ReaderPalette,
        selection: Option<TextSelection>,
        active_annotation: Option<AnnotationId>,
        active_search: Option<SearchMatchId>,
        navigation_focus: Option<NavigationFocusFrame>,
    ) -> Self {
        Self {
            palette,
            selection,
            active_annotation,
            active_search,
            navigation_focus,
            annotation_budget: PaintBudget::new(MAX_VISIBLE_ANNOTATION_QUADS),
            annotation_geometry_budget: PaintBudget::new(MAX_VISIBLE_ANNOTATION_QUADS),
            search_budget: PaintBudget::new(MAX_VISIBLE_SEARCH_HIGHLIGHT_RUNS),
            selection_budget: PaintBudget::new(MAX_VISIBLE_SELECTION_QUADS),
            extension_overlay_budget: PaintBudget::new(MAX_VISIBLE_EXTENSION_OVERLAY_REGIONS),
        }
    }

    fn paint_page(
        &mut self,
        page: PdfCanvasPagePaintContext<'_, PaintPageOverlay>,
        window: &mut Window,
    ) {
        let palette = self.palette;
        let rect = page.page_rect;
        let page_bounds = page.page_bounds;
        let bounds = page.canvas_bounds;
        let content_viewport = page.content_viewport;
        let scroll = page.scroll;
        let overlay = page.overlay;

        if !self.extension_overlay_budget.exhausted() {
            window.with_content_mask(
                Some(ContentMask {
                    bounds: page_bounds,
                }),
                |window| {
                    for extension_overlay in &overlay.extension_overlays {
                        for region in &extension_overlay.regions {
                            let region = normalized_bounds_in_page(rect, *region);
                            if !region.intersects(content_viewport) {
                                continue;
                            }
                            if !self.extension_overlay_budget.take() {
                                return;
                            }
                            paint_extension_overlay(
                                region,
                                extension_overlay.appearance,
                                bounds,
                                scroll,
                                palette,
                                window,
                            );
                        }
                    }
                },
            );
        }

        if let Some(chars) = overlay.text.as_ref() {
            window.with_content_mask(
                Some(ContentMask {
                    bounds: page_bounds,
                }),
                |window| {
                    for annotation in &overlay.annotations {
                        if self.annotation_budget.exhausted()
                            || self.annotation_geometry_budget.exhausted()
                        {
                            break;
                        }
                        let Some(range) = annotation
                            .range
                            .indices_on_page(page.page_index, chars.len())
                        else {
                            continue;
                        };
                        let active = Some(annotation.id) == self.active_annotation;
                        let color = match annotation.color {
                            Some(HighlightColor::Yellow) if active => palette.yellow.opacity(0.53),
                            Some(HighlightColor::Yellow) => palette.yellow.opacity(0.38),
                            Some(HighlightColor::Green) if active => palette.green.opacity(0.53),
                            Some(HighlightColor::Green) => palette.green.opacity(0.38),
                            Some(HighlightColor::Blue) if active => palette.blue.opacity(0.53),
                            Some(HighlightColor::Blue) => palette.blue.opacity(0.38),
                            Some(HighlightColor::Pink) if active => palette.pink.opacity(0.53),
                            Some(HighlightColor::Pink) => palette.pink.opacity(0.38),
                            Some(HighlightColor::Purple) if active => palette.purple.opacity(0.53),
                            Some(HighlightColor::Purple) => palette.purple.opacity(0.38),
                            None if active => palette.warning.opacity(0.47),
                            None if annotation.has_comment => palette.warning.opacity(0.24),
                            None => continue,
                        };
                        // Highlights use the same line-aware geometry as the
                        // live selection: source-order glyphs on a visual line
                        // are unioned, bridging glyph-less whitespace without
                        // crossing a line break. Keep geometry work bounded
                        // separately from the number of resulting quads.
                        let (highlight_runs, inspected_glyphs) = chars
                            .visible_selection_runs_with_glyph_count(
                                rect,
                                content_viewport,
                                range,
                                self.annotation_geometry_budget.remaining(),
                            );
                        self.annotation_geometry_budget.take_up_to(inspected_glyphs);
                        for highlight in highlight_runs {
                            if !self.annotation_budget.take() {
                                break;
                            }
                            window.paint_quad(quad(
                                content_rect_to_bounds(bounds, highlight, scroll),
                                px(1.0),
                                color,
                                px(0.0),
                                gpui::transparent_black(),
                                Default::default(),
                            ));
                        }
                        if self.annotation_budget.exhausted()
                            || self.annotation_geometry_budget.exhausted()
                        {
                            break;
                        }
                    }
                },
            );
        }

        if let Some(matches) = overlay.search.as_ref() {
            window.with_content_mask(
                Some(ContentMask {
                    bounds: page_bounds,
                }),
                |window| {
                    let active = self.active_search;
                    for result in matches
                        .iter()
                        .filter(|result| !is_inactive(result.id, active))
                        .chain(
                            matches
                                .iter()
                                .filter(|result| is_inactive(result.id, active)),
                        )
                    {
                        let is_active = !is_inactive(result.id, active);
                        for run in &result.highlight_runs {
                            if self.search_budget.exhausted() {
                                return;
                            }
                            let highlight = normalized_bounds_in_page(rect, *run);
                            if !highlight.intersects(content_viewport) {
                                continue;
                            }
                            let painted = self.search_budget.take();
                            debug_assert!(painted, "exhaustion checked above");
                            window.paint_quad(quad(
                                content_rect_to_bounds(bounds, highlight, scroll),
                                px(1.0),
                                if is_active {
                                    palette.warning.opacity(0.53)
                                } else {
                                    palette.yellow.opacity(0.33)
                                },
                                px(0.0),
                                gpui::transparent_black(),
                                Default::default(),
                            ));
                        }
                    }
                },
            );
        }

        if let (Some(selection), Some(chars)) = (self.selection, overlay.text.as_ref())
            && !self.selection_budget.exhausted()
            && let Some(range) = selection.indices_on_page(page.page_index, chars.len())
        {
            // Query only visible selected glyphs, then coalesce each visual
            // line into one rectangle. This bridges spaces without PDFium
            // bounds and avoids the fragmented word-by-word selection look.
            let selection_runs = chars.visible_selection_runs(
                rect,
                content_viewport,
                range,
                MAX_VISIBLE_SELECTION_QUADS,
            );
            window.with_content_mask(
                Some(ContentMask {
                    bounds: page_bounds,
                }),
                |window| {
                    for highlight in selection_runs {
                        if !self.selection_budget.take() {
                            break;
                        }
                        window.paint_quad(quad(
                            content_rect_to_bounds(bounds, highlight, scroll),
                            px(1.0),
                            palette.selection,
                            px(0.0),
                            gpui::transparent_black(),
                            Default::default(),
                        ));
                    }
                },
            );
        }

        if let Some(frame) = self
            .navigation_focus
            .as_ref()
            .filter(|frame| frame.target.page == page.page_index)
        {
            paint_navigation_focus(frame, rect, page_bounds, bounds, scroll, palette, window);
        }
    }
}

impl PdfReader {
    fn record_pane_bounds(
        bounds: Bounds<Pixels>,
        measured_bounds: &Cell<Option<Bounds<Pixels>>>,
        reader: gpui::WeakEntity<PdfReader>,
        window: &mut Window,
    ) {
        if measured_bounds.replace(Some(bounds)) == Some(bounds) {
            return;
        }
        window.on_next_frame(move |window, cx| {
            reader
                .update(cx, |reader, cx| {
                    reader.update_viewport_for_pane(bounds.size, window);
                    reader.request_visible_tiles(window);
                    cx.notify();
                })
                .ok();
        });
    }

    fn empty_pane_bounds_probe(
        measured_bounds: Rc<Cell<Option<Bounds<Pixels>>>>,
        reader: gpui::WeakEntity<PdfReader>,
    ) -> impl IntoElement {
        canvas(
            move |bounds, window, _| {
                Self::record_pane_bounds(bounds, &measured_bounds, reader, window);
            },
            |_, _, _, _| {},
        )
        .absolute()
        .inset_0()
    }

    fn document_canvas(
        mut snapshot: PaintSnapshot,
        canvas_bounds: Rc<Cell<Option<Bounds<Pixels>>>>,
        reader: gpui::WeakEntity<PdfReader>,
    ) -> impl IntoElement {
        snapshot.canvas.pages.sort_by_key(|page| {
            let has_active_annotation = page
                .overlay
                .annotations
                .iter()
                .any(|annotation| Some(annotation.id) == snapshot.active_annotation);
            let has_active_search = snapshot
                .active_search
                .is_some_and(|active| active.page == page.page_index);
            !(has_active_annotation || has_active_search)
        });
        let PaintSnapshot {
            palette,
            canvas,
            selection,
            active_annotation,
            active_search,
            navigation_focus,
        } = snapshot;
        let mut overlay_state = PaintPageOverlayState::new(
            palette,
            selection,
            active_annotation,
            active_search,
            navigation_focus,
        );
        let measured_bounds = canvas_bounds.clone();
        pdf_canvas_measured(
            canvas,
            move |bounds, window, _| {
                Self::record_pane_bounds(bounds, &measured_bounds, reader, window);
            },
            move |page, window| overlay_state.paint_page(page, window),
        )
        .size_full()
    }
}

impl Focusable for PdfReader {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for PdfReader {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.theme_tokens = ThemeTokens::from_app(cx);
        self.synchronize_render_appearance(window, cx);
        let active_theme = Theme::global(cx);
        let mut palette = ReaderPalette::from_app(cx);
        let forced_dark = matches!(
            self.render_appearance,
            RenderAppearance::ForcedColors { .. }
        );
        palette.paper = theme::pdf_paper_color(active_theme, forced_dark);
        palette.paper_border = theme::pdf_paper_border(active_theme, forced_dark);
        self.update_viewport(window);
        self.request_visible_tiles(window);
        let full_width = self.viewport_width.max(1.0);

        let toc_navigation = self.render_toc_navigation(palette, cx);
        let link_preview = self.render_link_preview_card(palette, cx);

        let content = if let Some(layout) = self.layout() {
            let visible = layout.visible_pages(
                self.scroll.y,
                self.viewport_height,
                self.viewport_height * 0.2,
            );
            let pages = visible
                .filter_map(|index| {
                    let rect = layout.page_rect(index)?;
                    let desired = viewport_raster_size(
                        rect,
                        window.scale_factor(),
                        &self.viewport.config().limits,
                    );
                    let mut paint_annotations: Vec<_> = self
                        .annotations
                        .as_ref()
                        .map(|annotations| {
                            annotations
                                .on_page(index)
                                .map(|annotation| PaintAnnotation {
                                    id: annotation.id(),
                                    range: annotation.range(),
                                    color: annotation.highlight(),
                                    has_comment: annotation.comment_markdown().is_some(),
                                })
                                .collect()
                        })
                        .unwrap_or_default();
                    paint_annotations.sort_by_key(|annotation| {
                        is_inactive(annotation.id, self.active_annotation)
                    });
                    Some(PdfCanvasPage::new(
                        index,
                        rect,
                        self.paint_tiles_for_page(index, rect, desired),
                        PaintPageOverlay {
                            text: self.page_text.get(&index).cloned(),
                            annotations: paint_annotations,
                            search: self.search.pages.get(&index).cloned(),
                            extension_overlays: self
                                .extension_overlays
                                .as_ref()
                                .map(|batch| {
                                    batch
                                        .overlays
                                        .iter()
                                        .filter(|overlay| usize::from(overlay.page) == index)
                                        .map(|overlay| PaintExtensionOverlay {
                                            regions: overlay
                                                .regions
                                                .iter()
                                                .copied()
                                                .map(TextBounds::from)
                                                .collect(),
                                            appearance: overlay.appearance,
                                        })
                                        .collect()
                                })
                                .unwrap_or_default(),
                        },
                    ))
                })
                .collect();
            let snapshot = PaintSnapshot {
                palette,
                canvas: PdfCanvasSnapshot::new(
                    pages,
                    PdfCanvasMetrics::new(
                        self.scroll,
                        self.viewport_width,
                        self.viewport_height,
                        layout.content_height,
                        self.max_scroll_x(layout),
                    ),
                    PdfCanvasStyle::new(
                        palette.canvas,
                        palette.paper,
                        palette.paper_border,
                        palette.overlay.opacity(0.32),
                        palette.text_secondary.opacity(0.56),
                    ),
                ),
                selection: self.selection,
                active_annotation: self.active_annotation,
                active_search: self.search.active,
                navigation_focus: self.navigation_focus.frame(Instant::now()),
            };
            div()
                .id("document-viewport")
                .relative()
                .min_w_0()
                .flex_1()
                .h_full()
                .w_full()
                .overflow_hidden()
                .bg(palette.canvas)
                .cursor(if self.pan.is_some() {
                    CursorStyle::ClosedHand
                } else if self.hovered_link.is_some() || self.hovered_reference.is_some() {
                    CursorStyle::PointingHand
                } else {
                    CursorStyle::IBeam
                })
                .on_scroll_wheel(cx.listener(Self::on_scroll_wheel))
                .on_mouse_down(MouseButton::Left, cx.listener(Self::on_mouse_down))
                .on_mouse_down(MouseButton::Middle, cx.listener(Self::on_mouse_down))
                .on_mouse_move(cx.listener(Self::on_mouse_move))
                .on_hover(cx.listener(Self::on_document_hover))
                .on_mouse_up(MouseButton::Left, cx.listener(Self::on_mouse_up))
                .on_mouse_up(MouseButton::Middle, cx.listener(Self::on_mouse_up))
                .on_mouse_up_out(MouseButton::Left, cx.listener(Self::on_mouse_up))
                .on_mouse_up_out(MouseButton::Middle, cx.listener(Self::on_mouse_up))
                .child(Self::document_canvas(
                    snapshot,
                    self.canvas_bounds.clone(),
                    cx.weak_entity(),
                ))
                .children(toc_navigation)
                .into_any_element()
        } else {
            div()
                .id("empty-state")
                .relative()
                .min_w_0()
                .flex_1()
                .h_full()
                .w_full()
                .flex()
                .flex_col()
                .items_center()
                .justify_center()
                .gap_3()
                .bg(palette.canvas_empty)
                .text_color(palette.text)
                .child(Self::empty_pane_bounds_probe(
                    self.canvas_bounds.clone(),
                    cx.weak_entity(),
                ))
                .child(
                    div()
                        .mb_2()
                        .size(px(58.0))
                        .flex()
                        .items_center()
                        .justify_center()
                        .design_radius(RadiusRole::Large, &palette.ui)
                        .border_1()
                        .border_color(palette.text.opacity(0.15))
                        .bg(palette.surface.opacity(0.07))
                        .design_elevation(ElevationRole::Surface, &palette.ui)
                        .text_sm()
                        .font_weight(FontWeight::SEMIBOLD)
                        .child("PDF"),
                )
                .child(
                    div()
                        .text_2xl()
                        .font_weight(FontWeight::SEMIBOLD)
                        .child("Open a document"),
                )
                .child(
                    div()
                        .max_w(px(430.0))
                        .text_center()
                        .text_sm()
                        .line_height(px(21.0))
                        .text_color(palette.text_secondary)
                        .child("Read, search, highlight, and comment with a fast native PDF workspace."),
                )
                .child(Self::chrome_button(
                    palette,
                    "empty-open-document",
                    "Choose PDF…",
                    ChromeButtonStyle::Primary,
                    true,
                    cx.listener(|reader, _, window, cx| {
                        reader.open_dialog(&OpenDocument, window, cx)
                    }),
                ))
                .child(
                    div()
                        .mt_2()
                        .text_xs()
                        .text_color(palette.text_tertiary)
                        .child("⌘O to open  ·  Pinch or ⌘-scroll to zoom"),
                )
                .into_any_element()
        };

        let sidebar_content = match self.sidebar.panel {
            SidePanel::Comments => self.render_comments_panel(cx),
            SidePanel::Search => self.render_search_panel(cx),
            SidePanel::Extensions => self.render_extensions_panel(cx),
        };
        let panel_width = self.fluid_panel_width();
        let sidebar_reveal = fluid_sidebar_extent_with_ui(
            self.viewport_width,
            self.sidebar.progress,
            self.theme_tokens.reader,
        );
        let panel_reveal = self.fluid_panel_occlusion();
        let available_width = (self.viewport_width - panel_reveal).max(1.0);
        let has_stable_selection = self
            .selection
            .is_some_and(|selection| selection.anchor != selection.focus);
        let show_context_pill = !self.selecting
            && self.comment_editor.is_none()
            && (has_stable_selection || self.active_annotation.is_some());
        let context_enabled =
            show_context_pill && !self.annotations_loading && !self.annotation_persistence_blocked;
        let context_pill = show_context_pill
            .then(|| self.context_anchor_in_viewport())
            .flatten()
            .map(|anchor| {
                let ui = self.theme_tokens.reader;
                let context_pill_width = ui
                    .context_pill_width
                    .min((available_width - ui.panel_horizontal_margin * 2.0).max(1.0));
                let position = floating_pill_position(
                    anchor,
                    available_width,
                    self.viewport_height,
                    context_pill_width,
                    ui.context_pill_height,
                );
                self.render_fluid_context_pill(position, context_pill_width, context_enabled, cx)
            });
        let sidebar = div()
            .id("fluid-sidebar-reveal")
            .absolute()
            .top_0()
            .bottom_0()
            .right_0()
            .w(px(sidebar_reveal))
            .overflow_hidden()
            .child(
                div()
                    .id("reader-sidebar")
                    .absolute()
                    .top(px(self.theme_tokens.reader.panel_vertical_margin))
                    .bottom(px(self.theme_tokens.reader.panel_vertical_margin))
                    .right(px(self.theme_tokens.reader.panel_horizontal_margin))
                    .w(px(panel_width))
                    .child(FloatingPanel::new(palette, sidebar_content)),
            );
        let workspace = div()
            .relative()
            .flex_1()
            .min_h(px(0.0))
            .w_full()
            .overflow_hidden()
            .child(content)
            .children(context_pill)
            .child(sidebar)
            .children(link_preview)
            .into_any_element();

        let error_bar = if let ReaderStatus::Error(message) = &self.status {
            Some(
                div()
                    .h(px(self.theme_tokens.reader.error_bar_height))
                    .flex_none()
                    .w_full()
                    .flex()
                    .items_center()
                    .px_3()
                    .bg(palette.error_soft)
                    .text_color(palette.error)
                    .text_sm()
                    .child(message.clone()),
            )
        } else {
            None
        };

        let reference_details_panel =
            self.render_reference_details_panel(palette, full_width, window, cx);
        let extension_ui_panel = self.render_extension_ui_floating_panel(palette, full_width, cx);
        div()
            .key_context("PdfReader")
            .track_focus(&self.focus_handle)
            .size_full()
            .relative()
            .flex()
            .flex_col()
            .bg(palette.canvas)
            .on_action(cx.listener(Self::install_extension_dialog))
            .on_action(cx.listener(Self::manage_extensions))
            .on_action(cx.listener(Self::open_extension_details_action))
            .on_action(cx.listener(Self::zoom_in))
            .on_action(cx.listener(Self::zoom_out))
            .on_action(cx.listener(Self::actual_size))
            .on_action(cx.listener(Self::fit_width))
            .on_action(cx.listener(Self::copy_selection))
            .on_action(cx.listener(Self::select_all))
            .on_action(cx.listener(Self::edit_copy))
            .on_action(cx.listener(Self::edit_cut))
            .on_action(cx.listener(Self::edit_paste))
            .on_action(cx.listener(Self::edit_select_all))
            .on_action(cx.listener(Self::scroll_up))
            .on_action(cx.listener(Self::scroll_down))
            .on_action(cx.listener(Self::scroll_left))
            .on_action(cx.listener(Self::scroll_right))
            .on_action(cx.listener(Self::page_up))
            .on_action(cx.listener(Self::page_down))
            .on_action(cx.listener(Self::first_page))
            .on_action(cx.listener(Self::last_page))
            .on_action(cx.listener(Self::find_document))
            .on_action(cx.listener(Self::toggle_comments))
            .on_action(cx.listener(Self::add_comment))
            .on_action(cx.listener(Self::next_search_result))
            .on_action(cx.listener(Self::previous_search_result))
            .on_action(cx.listener(Self::invoke_extension_command))
            .on_action(cx.listener(Self::quit_application))
            .children(error_bar)
            .child(workspace)
            .children(extension_ui_panel)
            .children(reference_details_panel)
    }
}
