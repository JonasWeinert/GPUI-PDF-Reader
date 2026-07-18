use super::comments::{annotation_actions_enabled, comments_toolbar_label, floating_pill_position};
use super::*;

struct PaintPageOverlayState {
    palette: ReaderPalette,
    selection: Option<TextSelection>,
    active_annotation: Option<AnnotationId>,
    active_search: Option<SearchMatchId>,
    navigation_focus: Option<NavigationFocusFrame>,
    annotation_budget: PaintBudget,
    search_budget: PaintBudget,
    selection_budget: PaintBudget,
    extension_overlay_budget: PaintBudget,
}

impl PaintPageOverlayState {
    fn new(snapshot: &PaintSnapshot) -> Self {
        Self {
            palette: snapshot.palette,
            selection: snapshot.selection,
            active_annotation: snapshot.active_annotation,
            active_search: snapshot.active_search,
            navigation_focus: snapshot.navigation_focus.clone(),
            annotation_budget: PaintBudget::new(MAX_VISIBLE_ANNOTATION_QUADS),
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
                        if self.annotation_budget.exhausted() {
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
                        let exhausted = chars.for_each_visible_in_range_while(
                            rect,
                            content_viewport,
                            range,
                            |_, highlight| {
                                if !self.annotation_budget.take() {
                                    return false;
                                }
                                window.paint_quad(quad(
                                    content_rect_to_bounds(bounds, highlight, scroll),
                                    px(1.0),
                                    color,
                                    px(0.0),
                                    gpui::transparent_black(),
                                    Default::default(),
                                ));
                                !self.annotation_budget.exhausted()
                            },
                        );
                        if !exhausted {
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
            // Dense text pages are indexed once on the worker. Paint only
            // selected glyphs that intersect this frame's viewport, and clip
            // malformed PDF geometry to the physical page.
            window.with_content_mask(
                Some(ContentMask {
                    bounds: page_bounds,
                }),
                |window| {
                    chars.for_each_visible_in_range_while(
                        rect,
                        content_viewport,
                        range,
                        |_, highlight| {
                            if !self.selection_budget.take() {
                                return false;
                            }
                            window.paint_quad(quad(
                                content_rect_to_bounds(bounds, highlight, scroll),
                                px(1.0),
                                palette.selection,
                                px(0.0),
                                gpui::transparent_black(),
                                Default::default(),
                            ));
                            !self.selection_budget.exhausted()
                        },
                    );
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
    fn document_canvas(mut snapshot: PaintSnapshot) -> impl IntoElement {
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
        let mut overlay_state = PaintPageOverlayState::new(&snapshot);
        pdf_canvas(snapshot.canvas, move |page, window| {
            overlay_state.paint_page(page, window);
        })
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
        let active_theme = Theme::global(cx);
        let mut palette = ReaderPalette::from_theme(active_theme);
        let forced_dark = matches!(
            self.render_appearance,
            RenderAppearance::ForcedColors { .. }
        );
        palette.paper = theme::pdf_paper_color(active_theme, forced_dark);
        palette.paper_border = theme::pdf_paper_border(active_theme, forced_dark);
        self.update_viewport(window);
        self.request_visible_tiles(window);
        let full_width = f32::from(window.viewport_size().width).max(1.0);
        let compact_toolbar = full_width < 920.0;
        let very_compact_toolbar = full_width < 800.0;

        let page_count = self
            .document
            .as_ref()
            .map(|document| document.pages.len())
            .unwrap_or(0);
        let current_page = self
            .layout()
            .map(|layout| layout.current_page(self.scroll.y, self.viewport_height) + 1)
            .unwrap_or(0);
        let filename: SharedString = self
            .document
            .as_ref()
            .and_then(|document| document.path.file_name())
            .map(|name| name.to_string_lossy().into_owned().into())
            .unwrap_or_else(|| "GPUI PDF Reader".into());
        let zoom_label: SharedString = format!("{}%", (self.zoom * 100.0).round() as u32).into();
        let status_text = self.status_text();
        let document_open = page_count > 0;
        let (zoom_out_enabled, zoom_in_enabled) = zoom_controls_enabled(document_open, self.zoom);
        let editor_open = self.comment_editor.is_some();
        let comments_label =
            comments_toolbar_label(editor_open, compact_toolbar, very_compact_toolbar);
        let has_annotation_context = self.context_range().is_some();
        let show_annotation_tools = has_annotation_context && !editor_open;
        let annotation_tools_enabled = annotation_actions_enabled(
            has_annotation_context,
            self.annotations_loading,
            self.annotation_persistence_blocked,
            editor_open,
        );
        let search_selected = self.sidebar.panel == SidePanel::Search && self.sidebar.target > 0.5;
        let comments_selected =
            self.sidebar.panel == SidePanel::Comments && self.sidebar.target > 0.5;
        let show_status = !matches!(status_text.as_ref(), "Ready" | "Open a PDF to begin");

        let classic_toolbar = div()
            .h(px(TOOLBAR_HEIGHT))
            .flex_none()
            .w_full()
            .flex()
            .items_center()
            .gap_2()
            .px_3()
            .bg(palette.chrome)
            .border_b_1()
            .border_color(palette.separator)
            .text_color(palette.text)
            .child(
                div()
                    .h_full()
                    .w(px(TITLEBAR_CONTROL_INSET))
                    .flex_none()
                    .window_control_area(WindowControlArea::Drag),
            )
            .child(Self::chrome_button(
                palette,
                "open-document",
                if very_compact_toolbar {
                    "Open"
                } else {
                    "Open…"
                },
                ChromeButtonStyle::Ghost,
                true,
                cx.listener(|reader, _, window, cx| reader.open_dialog(&OpenDocument, window, cx)),
            ))
            .child(
                div()
                    .h(px(32.0))
                    .flex()
                    .items_center()
                    .overflow_hidden()
                    .rounded_md()
                    .border_1()
                    .border_color(palette.separator)
                    .bg(palette.control)
                    .child(Self::segment_button(
                        palette,
                        "zoom-out",
                        Icon::new(IconName::Minus),
                        zoom_out_enabled,
                        cx.listener(|reader, _, window, cx| reader.zoom_out(&ZoomOut, window, cx)),
                    ))
                    .child(
                        div()
                            .h_full()
                            .min_w(px(54.0))
                            .px_2()
                            .flex()
                            .items_center()
                            .justify_center()
                            .border_l_1()
                            .border_r_1()
                            .border_color(palette.separator)
                            .text_sm()
                            .text_color(if document_open {
                                palette.text
                            } else {
                                palette.text_tertiary
                            })
                            .child(zoom_label.clone()),
                    )
                    .child(Self::segment_button(
                        palette,
                        "zoom-in",
                        Icon::new(IconName::Plus),
                        zoom_in_enabled,
                        cx.listener(|reader, _, window, cx| reader.zoom_in(&ZoomIn, window, cx)),
                    ))
                    .when(!very_compact_toolbar, |controls| {
                        controls
                            .child(div().h_full().w(px(1.0)).bg(palette.separator))
                            .child(Self::segment_button(
                                palette,
                                "fit-width",
                                if compact_toolbar { "Fit" } else { "Fit Width" },
                                document_open,
                                cx.listener(|reader, _, window, cx| {
                                    reader.fit_width(&FitWidth, window, cx)
                                }),
                            ))
                    }),
            )
            .child(
                div()
                    .flex_1()
                    .min_w(px(0.0))
                    .h_full()
                    .px_2()
                    .flex()
                    .items_center()
                    .justify_center()
                    .gap_2()
                    .window_control_area(WindowControlArea::Drag)
                    .when(!compact_toolbar, |title| {
                        title.child(
                            div()
                                .min_w(px(0.0))
                                .max_w(px(260.0))
                                .overflow_hidden()
                                .whitespace_nowrap()
                                .text_ellipsis()
                                .text_sm()
                                .font_weight(FontWeight::MEDIUM)
                                .child(filename.clone()),
                        )
                    })
                    .when(document_open && !very_compact_toolbar, |title| {
                        title.child(
                            div()
                                .px_2()
                                .py_1()
                                .rounded_full()
                                .bg(palette.control_hover)
                                .text_xs()
                                .text_color(palette.text_secondary)
                                .child(format!("{current_page} / {page_count}")),
                        )
                    })
                    .when(show_status, |title| {
                        title.child(
                            div()
                                .max_w(px(180.0))
                                .overflow_hidden()
                                .whitespace_nowrap()
                                .text_ellipsis()
                                .text_xs()
                                .text_color(palette.text_secondary)
                                .child(status_text.clone()),
                        )
                    }),
            )
            .when(show_annotation_tools, |toolbar| {
                toolbar.child(
                    div()
                        .h(px(34.0))
                        .flex()
                        .items_center()
                        .gap_1()
                        .px_1()
                        .rounded_md()
                        .border_1()
                        .border_color(palette.separator)
                        .bg(palette.control)
                        .when(!compact_toolbar, |controls| {
                            controls.child(
                                div()
                                    .pl_2()
                                    .pr_1()
                                    .text_xs()
                                    .font_weight(FontWeight::MEDIUM)
                                    .text_color(palette.text_secondary)
                                    .child("Highlight"),
                            )
                        })
                        .children(
                            [
                                "highlight-yellow",
                                "highlight-green",
                                "highlight-blue",
                                "highlight-pink",
                                "highlight-purple",
                            ]
                            .into_iter()
                            .zip(HighlightColor::ALL)
                            .map(|(id, color)| {
                                Self::highlight_button(
                                    palette,
                                    id,
                                    color,
                                    annotation_tools_enabled,
                                    cx.listener(move |reader, _, _, cx| {
                                        reader.add_highlight(color, cx)
                                    }),
                                )
                            }),
                        )
                        .child(div().h(px(22.0)).w(px(1.0)).bg(palette.separator))
                        .child(Self::segment_button(
                            palette,
                            "add-selection-comment",
                            if self.context_has_comment() {
                                if very_compact_toolbar {
                                    "Edit"
                                } else {
                                    "Edit Comment"
                                }
                            } else if very_compact_toolbar {
                                "Note"
                            } else {
                                "Comment"
                            },
                            annotation_tools_enabled,
                            cx.listener(|reader, _, window, cx| {
                                reader.comment_on_context(window, cx)
                            }),
                        )),
                )
            })
            .when(
                !(very_compact_toolbar && show_annotation_tools),
                |toolbar| {
                    toolbar
                        .child(Self::chrome_button(
                            palette,
                            "toggle-search",
                            if compact_toolbar { "Find" } else { "Search" },
                            if search_selected {
                                ChromeButtonStyle::Selected
                            } else {
                                ChromeButtonStyle::Ghost
                            },
                            document_open,
                            cx.listener(|reader, _, window, cx| {
                                reader.find_document(&Find, window, cx)
                            }),
                        ))
                        .child(Self::chrome_button(
                            palette,
                            "toggle-comments",
                            comments_label,
                            if comments_selected {
                                ChromeButtonStyle::Selected
                            } else {
                                ChromeButtonStyle::Ghost
                            },
                            document_open,
                            cx.listener(|reader, _, window, cx| {
                                reader.toggle_comments(&ToggleComments, window, cx)
                            }),
                        ))
                },
            );

        let fluid_toolbar = div()
            .h(px(TOOLBAR_HEIGHT))
            .flex_none()
            .w_full()
            .flex()
            .items_center()
            .gap_2()
            .px_3()
            .bg(palette.chrome)
            .border_b_1()
            .border_color(palette.separator)
            .text_color(palette.text)
            .child(
                div()
                    .h_full()
                    .w(px(TITLEBAR_CONTROL_INSET))
                    .flex_none()
                    .window_control_area(WindowControlArea::Drag),
            )
            .child(Self::chrome_button(
                palette,
                "fluid-open-document",
                "Open…",
                ChromeButtonStyle::Ghost,
                true,
                cx.listener(|reader, _, window, cx| reader.open_dialog(&OpenDocument, window, cx)),
            ))
            .child(
                div()
                    .flex_1()
                    .min_w(px(0.0))
                    .h_full()
                    .flex()
                    .items_center()
                    .justify_center()
                    .gap_2()
                    .window_control_area(WindowControlArea::Drag)
                    .child(
                        div()
                            .max_w(px(320.0))
                            .overflow_hidden()
                            .whitespace_nowrap()
                            .text_ellipsis()
                            .text_sm()
                            .font_weight(FontWeight::MEDIUM)
                            .child(filename.clone()),
                    )
                    .when(document_open, |title| {
                        title.child(
                            div()
                                .px_2()
                                .py_1()
                                .rounded_full()
                                .bg(palette.control_hover)
                                .text_xs()
                                .text_color(palette.text_secondary)
                                .child(format!("{current_page} / {page_count}")),
                        )
                    })
                    .when(show_status && !compact_toolbar, |title| {
                        title.child(
                            div()
                                .max_w(px(180.0))
                                .overflow_hidden()
                                .whitespace_nowrap()
                                .text_ellipsis()
                                .text_xs()
                                .text_color(palette.text_secondary)
                                .child(status_text.clone()),
                        )
                    }),
            )
            .child(
                div()
                    .px_2()
                    .py_1()
                    .rounded_full()
                    .bg(palette.accent_soft)
                    .text_xs()
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(palette.accent)
                    .child("Fluid"),
            );

        let toolbar = match self.view_mode {
            ReaderView::Classic => classic_toolbar.into_any_element(),
            ReaderView::Fluid => fluid_toolbar.into_any_element(),
        };
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
                .flex_none()
                .h_full()
                .w(px(self.viewport_width))
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
                .child(Self::document_canvas(snapshot))
                .children(toc_navigation)
                .into_any_element()
        } else {
            div()
                .id("empty-state")
                .flex_none()
                .h_full()
                .w(px(self.viewport_width))
                .flex()
                .flex_col()
                .items_center()
                .justify_center()
                .gap_3()
                .bg(palette.canvas_empty)
                .text_color(palette.text)
                .child(
                    div()
                        .mb_2()
                        .size(px(58.0))
                        .flex()
                        .items_center()
                        .justify_center()
                        .rounded_lg()
                        .border_1()
                        .border_color(palette.text.opacity(0.15))
                        .bg(palette.surface.opacity(0.07))
                        .shadow_sm()
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
            SidePanel::Contribution => self.render_extension_contribution_panel(cx),
        };
        let workspace = match self.view_mode {
            ReaderView::Classic => {
                let sidebar_width = self.sidebar.available_width(full_width);
                let sidebar_inner_width =
                    SIDEBAR_WIDTH.min((full_width - MIN_DOCUMENT_VIEWPORT_WIDTH).max(0.0));
                let sidebar = div()
                    .id("reader-sidebar")
                    .h_full()
                    .w(px(sidebar_width))
                    .flex_none()
                    .overflow_hidden()
                    .bg(palette.surface)
                    .border_l_1()
                    .border_color(palette.separator)
                    .child(
                        div()
                            .h_full()
                            .w(px(sidebar_inner_width))
                            .child(sidebar_content),
                    );
                div()
                    .relative()
                    .flex_1()
                    .min_h(px(0.0))
                    .w_full()
                    .flex()
                    .overflow_hidden()
                    .child(content)
                    .child(sidebar)
                    .children(link_preview)
                    .into_any_element()
            }
            ReaderView::Fluid => {
                let panel_width = self.fluid_panel_width();
                let sidebar_reveal =
                    fluid_sidebar_extent(self.viewport_width, self.sidebar.progress);
                let panel_reveal = self.fluid_panel_occlusion();
                let available_width = (self.viewport_width - panel_reveal).max(1.0);
                let main_pill = self.render_fluid_main_pill(
                    FluidPillState {
                        available_width,
                        document_open,
                        zoom_out_enabled,
                        zoom_in_enabled,
                        zoom_label,
                        search_selected,
                        comments_selected,
                    },
                    cx,
                );
                let has_stable_selection = self
                    .selection
                    .is_some_and(|selection| selection.anchor != selection.focus);
                let show_context_pill = !self.selecting
                    && self.comment_editor.is_none()
                    && (has_stable_selection || self.active_annotation.is_some());
                let context_enabled = show_context_pill
                    && !self.annotations_loading
                    && !self.annotation_persistence_blocked;
                let context_pill = show_context_pill
                    .then(|| self.context_anchor_in_viewport())
                    .flatten()
                    .map(|anchor| {
                        let position = floating_pill_position(
                            anchor,
                            available_width,
                            self.viewport_height,
                            FLUID_CONTEXT_PILL_WIDTH,
                            FLUID_CONTEXT_PILL_HEIGHT,
                        );
                        self.render_fluid_context_pill(position, context_enabled, cx)
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
                            .top(px(FLUID_PANEL_VERTICAL_MARGIN))
                            .bottom(px(FLUID_PANEL_VERTICAL_MARGIN))
                            .right(px(FLUID_PANEL_HORIZONTAL_MARGIN))
                            .w(px(panel_width))
                            .child(FloatingPanel::new(palette, sidebar_content)),
                    );
                div()
                    .relative()
                    .flex_1()
                    .min_h(px(0.0))
                    .w_full()
                    .overflow_hidden()
                    .child(content)
                    .child(
                        div()
                            .absolute()
                            .top(px(14.0))
                            .left_0()
                            .w(px(available_width))
                            .flex()
                            .justify_center()
                            .child(main_pill),
                    )
                    .children(context_pill)
                    .child(sidebar)
                    .children(link_preview)
                    .into_any_element()
            }
        };

        let error_bar = if let ReaderStatus::Error(message) = &self.status {
            Some(
                div()
                    .h(px(ERROR_BAR_HEIGHT))
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
        div()
            .key_context("PdfReader")
            .track_focus(&self.focus_handle)
            .size_full()
            .relative()
            .flex()
            .flex_col()
            .bg(palette.canvas)
            .on_action(cx.listener(Self::open_dialog))
            .on_action(cx.listener(Self::install_extension_dialog))
            .on_action(cx.listener(Self::manage_extensions))
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
            .on_action(cx.listener(Self::use_classic_view))
            .on_action(cx.listener(Self::use_fluid_view))
            .on_action(cx.listener(Self::invoke_extension_command))
            .on_action(cx.listener(Self::quit_application))
            .child(toolbar)
            .children(error_bar)
            .child(workspace)
            .children(reference_details_panel)
    }
}
