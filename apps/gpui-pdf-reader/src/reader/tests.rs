use super::comments::{
    annotation_actions_enabled, comment_draft_needs_confirmation, comments_toolbar_label,
    floating_pill_position,
};
use super::*;
use crate::annotations::{AnnotationStore, DocumentKey, JsonSidecarStore};
use gpui_component::{ThemeColor, ThemeMode};

#[test]
fn extension_statistics_count_known_text_without_copying_it_into_snapshots() {
    let text = TextLayer::new(
        "one  two\nthree"
            .chars()
            .map(|value| TextChar {
                value,
                bounds: None,
            })
            .collect(),
    );
    assert_eq!(text_layer_statistics(&text), (3, 14));
    assert_eq!(bounded_snapshot_string("abcdefgh", 4), "abcd");
}

#[test]
fn raw_input_snapshot_requests_coalesce_until_async_dispatch_begins() {
    let mut dispatch = ExtensionSnapshotDispatch::default();
    assert!(dispatch.request());
    assert!(!dispatch.request());
    assert!(!dispatch.request());
    dispatch.begin_dispatch();
    assert!(dispatch.request());
}

#[cfg(feature = "installable-extensions")]
#[test]
fn extension_picker_accepts_both_project_and_package_directories() {
    let directory = TestDirectory::new("extension-picker");
    let extension = directory.path().join("example-extension");
    let package = extension.join("package");
    std::fs::create_dir_all(&package).unwrap();
    std::fs::write(package.join("manifest.toml"), "schema_version = 1\n").unwrap();

    assert_eq!(
        resolve_extension_package_selection(extension),
        package,
        "selecting the visible extension root should resolve its package directory"
    );
    assert_eq!(
        resolve_extension_package_selection(package.clone()),
        package,
        "selecting the package directory itself must remain unchanged"
    );
    let archive = directory.path().join("example.keyext");
    std::fs::write(&archive, b"archive placeholder").unwrap();
    assert_eq!(
        resolve_extension_package_selection(archive.clone()),
        archive
    );
}

#[test]
fn scientific_lookup_is_limited_to_exact_reference_ranges() {
    let reference = ScientificReference {
        number: 7,
        page: 9,
        x_fraction: Some(0.2),
        y_fraction: Some(0.72),
        text: "7. A real paper reference".to_owned(),
        text_runs: vec![TextBounds {
            left: 0.1,
            top: 0.70,
            right: 0.9,
            bottom: 0.78,
        }],
    };
    let figure = ResolvedInternalLink {
        x_fraction: Some(0.5),
        y_fraction: Some(0.22),
        text_runs: vec![TextBounds {
            left: 0.1,
            top: 0.20,
            right: 0.8,
            bottom: 0.25,
        }],
        preview: "Figure 2".to_owned(),
        matched_source: true,
    };
    let citation = ResolvedInternalLink {
        x_fraction: Some(0.5),
        y_fraction: Some(0.72),
        text_runs: reference.text_runs.clone(),
        preview: reference.text.clone(),
        matched_source: true,
    };
    assert!(!scientific_reference_matches(
        &reference,
        9,
        Some(&figure),
        Some(0.72)
    ));
    assert!(scientific_reference_matches(
        &reference,
        9,
        Some(&citation),
        None
    ));
    assert!(!scientific_reference_matches(
        &reference,
        8,
        Some(&citation),
        Some(0.72)
    ));
}

#[test]
fn grouped_reference_resolution_requires_every_bibliography_entry() {
    let reference = |number| ScientificReference {
        number,
        page: 9,
        x_fraction: Some(0.2),
        y_fraction: Some(0.1 + number as f32 * 0.01),
        text: format!("{number}. Reference {number}"),
        text_runs: Vec::new(),
    };
    let complete = vec![reference(20), reference(21), reference(22)];
    assert_eq!(
        complete_grouped_reference_indices(&complete, "[20-22]", 20),
        Some(vec![0, 1, 2])
    );
    assert_eq!(
        complete_grouped_reference_indices(&complete, "[20-22]", 19),
        None
    );
    let incomplete = vec![reference(20), reference(22)];
    assert_eq!(
        complete_grouped_reference_indices(&incomplete, "[20-22]", 20),
        None
    );
    assert_eq!(adjacent_group_index(0, 3, true), 1);
    assert_eq!(adjacent_group_index(2, 3, true), 0);
    assert_eq!(adjacent_group_index(0, 3, false), 2);
    assert_eq!(adjacent_group_index(7, 1, false), 0);
}

#[test]
fn compact_reference_labels_are_bounded_and_use_last_names() {
    let authors = vec![
        "Ada Lovelace".to_owned(),
        "Grace Hopper".to_owned(),
        "Alan Turing".to_owned(),
    ];
    assert_eq!(compact_authors(&authors), "Lovelace, Hopper, Turing");
    let many = [authors, vec!["Katherine Johnson".to_owned()]].concat();
    assert_eq!(compact_authors(&many), "Lovelace et al.");
    assert_eq!(compact_words("one two three four", 3), "one two three…");
    assert!(compact_journal("A very long journal title that needs shortening").ends_with('…'));
}

#[test]
fn reference_previews_measure_content_and_expand_their_shell_smoothly() {
    let short = measured_preview_width(&["Short"], 220.0, 340.0);
    let long = measured_preview_width(
        &["A substantially longer title that should earn a wider preview shell"],
        220.0,
        340.0,
    );
    assert_eq!(short, 220.0);
    assert!(long > short);
    assert_eq!(long, 340.0);

    let ready = ScholarlyMetadataState::Ready(Box::new(ScholarlyMetadata {
        source: crate::scholarly::ScholarlySource::OpenAlex,
        title: "A substantially longer scientific title for adaptive sizing".to_owned(),
        abstract_text: None,
        tldr_text: None,
        authors: vec!["Ada Author".to_owned(), "Ben Writer".to_owned()],
        year: Some(2025),
        journal: Some("Journal of Responsive Interfaces".to_owned()),
        journal_short: Some("J Resp Interfaces".to_owned()),
        journal_url: None,
        doi: Some("10.1000/adaptive".to_owned()),
        open_access: Some(true),
        full_text_url: None,
        landing_url: None,
        certainty: None,
    }));
    let collapsed = reference_preview_width(Some(&ready), 0.0);
    let halfway = reference_preview_width(Some(&ready), 0.5);
    let expanded = reference_preview_width(Some(&ready), 1.0);
    assert_eq!(collapsed, 232.0);
    assert!(halfway > collapsed && halfway < expanded);
    assert!(expanded <= LINK_CARD_WIDTH);
    assert_eq!(reference_preview_width(None, 1.0), 232.0);
    assert_eq!(reference_hero_height("Short title"), 116.0);
    assert_eq!(
        reference_hero_height(
            "A journal with a deliberately long descriptive name that wraps cleanly"
        ),
        164.0
    );
    let ScholarlyMetadataState::Ready(metadata) = &ready else {
        unreachable!();
    };
    assert_eq!(
        compact_reference_panel_citation(metadata),
        "Author, Writer · J Resp Interfaces · 2025"
    );
}

#[test]
fn dense_link_hover_requires_a_stable_neighbor_before_handoff() {
    assert!(LINK_CARD_MOVE_DEBOUNCE < LINK_HOVER_HANDOFF_DELAY);
    assert!(LINK_HOVER_HANDOFF_DELAY < LINK_HOVER_CLOSE_DELAY);
    assert!(LINK_HOVER_CLOSE_DELAY >= Duration::from_millis(300));
    let origin = point(px(100.0), px(200.0));
    let pending = PendingLinkHover {
        target: Some(PreviewTarget::Link(1)),
        position: origin,
    };
    assert!(!link_hover_candidate_needs_restart(
        pending,
        Some(PreviewTarget::Link(1)),
        point(px(102.0), px(201.0)),
    ));
    assert!(link_hover_candidate_needs_restart(
        pending,
        Some(PreviewTarget::Link(2)),
        origin,
    ));
    assert!(link_hover_candidate_needs_restart(
        pending,
        Some(PreviewTarget::Link(1)),
        point(px(104.0), px(200.0)),
    ));
}

#[test]
fn reference_detail_helpers_bound_expansion_and_preserve_literal_text() {
    assert_eq!(escape_markdown_text("DOI_10*[x]"), "DOI\\_10\\*\\[x\\]");
    assert_eq!(
        middle_truncate("10.1234/a-very-long-doi", 15),
        "10.1234…ong-doi"
    );
    assert!(citation_expanded_height("One Author", "A Journal", false) >= 112.0);
    assert!(citation_expanded_height(&"Author ".repeat(80), &"Journal ".repeat(20), true) <= 272.0);
}

#[test]
fn reusable_reveal_state_reverses_and_settles() {
    let mut reveal = RevealState::hidden();
    reveal.set_target(1.0);
    for _ in 0..60 {
        reveal.advance(1.0 / 60.0);
    }
    assert_eq!(reveal.value(), 1.0);
    reveal.set_target(0.0);
    for _ in 0..60 {
        reveal.advance(1.0 / 60.0);
    }
    assert_eq!(reveal.value(), 0.0);
}

#[test]
fn reference_panel_geometry_is_responsive_and_preserves_document_space() {
    assert_eq!(reference_panel_width(250.0), 0.0);
    assert_eq!(reference_panel_extent(250.0, 1.0), 0.0);
    assert_eq!(reference_panel_width(500.0), 176.0);
    assert!((reference_panel_width(1_100.0) - 396.0).abs() < 0.001);
    assert_eq!(reference_panel_width(2_000.0), REFERENCE_PANEL_MAX_WIDTH);

    let full_extent = reference_panel_extent(1_100.0, 1.0);
    assert!((full_extent - 420.0).abs() < 0.001);
    assert_eq!(reference_panel_extent(1_100.0, -2.0), 0.0);
    assert_eq!(reference_panel_extent(1_100.0, 2.0), full_extent);
    assert!(1_100.0 - full_extent >= MIN_DOCUMENT_VIEWPORT_WIDTH);

    assert_eq!(fluid_sidebar_extent(1_100.0, 0.0), 0.0);
    assert_eq!(fluid_sidebar_extent(1_100.0, 1.0), 368.0);
    assert_eq!(fluid_sidebar_extent(1_100.0, -1.0), 0.0);
    assert_eq!(fluid_sidebar_extent(1_100.0, 2.0), 368.0);
    assert!(reference_panel_extent(1_100.0, 1.0) > 0.0);
}

#[test]
fn toc_helpers_center_the_stack_cascade_hover_and_preserve_navigation() {
    let entries = vec![
        TocEntry {
            title: "Part one".to_owned(),
            page: 0,
            depth: 0,
            destination_y: Some(0.25),
        },
        TocEntry {
            title: "Details".to_owned(),
            page: 1,
            depth: 1,
            destination_y: None,
        },
        TocEntry {
            title: "Part two".to_owned(),
            page: 2,
            depth: 0,
            destination_y: None,
        },
    ];
    let layout = DocumentLayout::new(
        &[
            PageSize {
                width: 612.0,
                height: 792.0,
            },
            PageSize {
                width: 612.0,
                height: 792.0,
            },
            PageSize {
                width: 612.0,
                height: 792.0,
            },
        ],
        1.0,
        900.0,
    );

    assert_eq!(active_toc_index(&entries, 0), Some(0));
    assert_eq!(active_toc_index(&entries, 1), Some(1));
    assert_eq!(active_toc_index(&entries, 2), Some(2));
    assert_eq!(
        toc_breadcrumb_entries(&entries, 1),
        Some(vec![(0, "Part one".to_owned()), (1, "Details".to_owned())])
    );
    assert_eq!(
        toc_breadcrumb_entries(&entries, 2),
        Some(vec![(2, "Part two".to_owned())])
    );
    let short_breadcrumbs = toc_breadcrumb_entries(&entries, 1).unwrap();
    assert!(toc_callout_width(&short_breadcrumbs, 360.0) < 280.0);
    assert_eq!(
        toc_callout_height(
            &short_breadcrumbs,
            toc_callout_width(&short_breadcrumbs, 360.0)
        ),
        TOC_CARD_MIN_HEIGHT
    );
    let long_breadcrumbs = vec![
        (
            0,
            "A very long parent section title that must wrap".to_owned(),
        ),
        (
            1,
            "An equally long child section title that must remain fully visible".to_owned(),
        ),
    ];
    let compact_breadcrumbs = toc_display_breadcrumbs(&long_breadcrumbs);
    assert_eq!(
        compact_breadcrumbs
            .iter()
            .map(|(index, _)| *index)
            .collect::<Vec<_>>(),
        vec![0, 1]
    );
    assert!(compact_breadcrumbs.iter().all(|(_, title)| {
        title.chars().count() <= TOC_BREADCRUMB_MAX_LABEL_CHARACTERS && title.ends_with('…')
    }));
    assert!(toc_callout_width(&compact_breadcrumbs, 360.0) < 360.0);
    assert_eq!(
        toc_callout_height(
            &compact_breadcrumbs,
            toc_callout_width(&compact_breadcrumbs, 360.0)
        ),
        TOC_CARD_MIN_HEIGHT
    );
    assert_eq!(end_truncate("Résumé détaillé", 8), "Résumé …");

    assert_eq!(toc_stack_geometry(600.0, 3), Some((288.0, 12.0)));
    assert_eq!(toc_stack_geometry(600.0, 1), Some((300.0, 0.0)));
    let (dense_top, dense_spacing) = toc_stack_geometry(100.0, 100).unwrap();
    assert!((dense_top - TOC_STACK_MARGIN).abs() < 0.001);
    assert!(dense_spacing < 1.0);

    assert_eq!(toc_cascade_amount(4, 4.0, 1.0), 1.0);
    assert!((toc_cascade_amount(3, 4.0, 1.0) - 0.8).abs() < 0.001);
    assert!((toc_cascade_amount(2, 4.0, 0.5) - 0.3).abs() < 0.001);
    assert_eq!(toc_cascade_amount(9, 4.0, 1.0), 0.0);
    assert!((toc_cascade_amount(4, 4.5, 1.0) - 0.9).abs() < 0.001);
    assert!((toc_cascade_amount(5, 4.5, 1.0) - 0.9).abs() < 0.001);

    let mut hover_position = 3.0;
    let mut hover_strength = 1.0;
    advance_toc_hover_state(&mut hover_position, &mut hover_strength, Some(7), 0.5);
    assert_eq!(hover_position, 5.0);
    assert_eq!(hover_strength, 1.0);
    assert!(toc_hover_state_is_animating(
        hover_position,
        hover_strength,
        Some(7)
    ));
    advance_toc_hover_state(&mut hover_position, &mut hover_strength, None, 0.5);
    assert_eq!(hover_position, 5.0);
    assert_eq!(hover_strength, 0.5);
    assert!(toc_hover_state_is_animating(
        hover_position,
        hover_strength,
        None
    ));
    let page_only = DocumentJump::new(2)
        .resolve(&layout, 0.0, 800.0, 600.0, 0.0)
        .unwrap();
    assert_eq!(page_only.y, layout.page_rect(2).unwrap().y);
    let page = layout.page_rect(1).unwrap();
    let positioned = DocumentJump::new(1)
        .position(None, Some(0.5))
        .resolve(&layout, 0.0, 800.0, 600.0, 0.0)
        .unwrap();
    assert_eq!(positioned.y, page.y + page.height * 0.5 - 300.0);
}

#[test]
fn link_preview_helpers_choose_destination_context_and_keep_cards_on_screen() {
    let entries = vec![
        TocEntry {
            title: "Introduction".to_owned(),
            page: 1,
            depth: 0,
            destination_y: Some(0.1),
        },
        TocEntry {
            title: "Detailed results".to_owned(),
            page: 1,
            depth: 1,
            destination_y: Some(0.62),
        },
    ];
    assert_eq!(
        link_section_title(&entries, 1, Some(0.7)).as_deref(),
        Some("Detailed results")
    );
    assert_eq!(
        link_section_title(&entries, 1, Some(0.2)).as_deref(),
        Some("Introduction")
    );

    let position = link_card_position(
        Rect {
            x: 740.0,
            y: 560.0,
            width: 30.0,
            height: 16.0,
        },
        800.0,
        600.0,
        340.0,
        190.0,
    );
    assert!(position.x >= LINK_CARD_MARGIN);
    assert!(position.x + 340.0 <= 800.0 - LINK_CARD_MARGIN + 0.001);
    assert!(position.y >= LINK_CARD_MARGIN);
    assert!(position.y + 190.0 <= 600.0 - LINK_CARD_MARGIN + 0.001);
    let pointer_position =
        pointer_link_card_position(Offset { x: 400.0, y: 240.0 }, 800.0, 600.0, 340.0, 190.0);
    assert!((pointer_position.x + 170.0 - 400.0).abs() < 0.001);
    assert_eq!(pointer_position.y, 240.0 + LINK_CARD_GAP);
    let edge_position =
        pointer_link_card_position(Offset { x: 790.0, y: 590.0 }, 800.0, 600.0, 340.0, 190.0);
    assert!(edge_position.x + 340.0 <= 800.0 - LINK_CARD_MARGIN + 0.001);
    assert!(edge_position.y < 590.0);
    assert!(!link_preview_should_close(true, false));
    assert!(!link_preview_should_close(false, true));
    assert!(!link_preview_should_close(true, true));
    assert!(link_preview_should_close(false, false));
}

#[test]
fn toc_title_matching_prefers_the_largest_exact_page_match() {
    let source = "Methods body Methods";
    let characters = source
        .chars()
        .enumerate()
        .map(|(index, value)| {
            let second_heading = index >= 13;
            let top = if second_heading { 0.62 } else { 0.12 };
            let height = if second_heading { 0.06 } else { 0.02 };
            TextChar {
                value,
                bounds: (!value.is_whitespace()).then_some(TextBounds {
                    left: index as f32 * 0.02,
                    top,
                    right: index as f32 * 0.02 + 0.015,
                    bottom: top + height,
                }),
            }
        })
        .collect();
    let text = TextLayer::new(characters);
    let matched = toc_title_match("methods", &text).expect("heading should match");
    assert!((matched.y - 0.62).abs() < 0.001);
    assert!(!matched.text_runs.is_empty());

    let resolved = resolve_toc_destination("methods", &text, Some(0.12));
    assert_eq!(resolved.y, Some(0.62));
    assert!(resolved.matched_title);
    assert!(!resolved.text_runs.is_empty());

    assert_eq!(
        resolve_toc_destination("missing heading", &text, Some(0.12)),
        ResolvedTocDestination {
            y: Some(0.12),
            text_runs: Vec::new(),
            matched_title: false,
        }
    );
    assert_eq!(toc_title_match("missing heading", &text), None);
}

#[test]
fn pdf_render_appearance_follows_theme_mode_and_colors() {
    let light = Theme::from(ThemeColor::light().as_ref());
    assert_eq!(
        render_appearance_from_theme(&light, true),
        RenderAppearance::Normal
    );

    let mut dark = Theme::from(ThemeColor::dark().as_ref());
    dark.mode = ThemeMode::Dark;
    let expected_background = gpui::Rgba::from(theme::pdf_paper_color(&dark, true));
    let expected_foreground = gpui::Rgba::from(dark.foreground);
    let channel = |value: f32| (value.clamp(0.0, 1.0) * 255.0).round() as u8;
    assert_eq!(
        render_appearance_from_theme(&dark, true),
        RenderAppearance::ForcedColors {
            background: RenderColor {
                red: channel(expected_background.r),
                green: channel(expected_background.g),
                blue: channel(expected_background.b),
            },
            foreground: RenderColor {
                red: channel(expected_foreground.r),
                green: channel(expected_foreground.g),
                blue: channel(expected_foreground.b),
            },
        }
    );
    assert_eq!(
        render_appearance_from_theme(&dark, false),
        RenderAppearance::Normal
    );
}

struct TestDirectory(PathBuf);

impl TestDirectory {
    fn new(label: &str) -> Self {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "gpui-pdf-reader-reader-{label}-{}-{nonce}",
            std::process::id()
        ));
        std::fs::create_dir_all(&path).unwrap();
        Self(path)
    }

    fn path(&self) -> &std::path::Path {
        &self.0
    }
}

impl Drop for TestDirectory {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn letter_pages(count: usize) -> Vec<PageSize> {
    vec![
        PageSize {
            width: 612.0,
            height: 792.0,
        };
        count
    ]
}

#[test]
fn annotation_controls_enable_for_any_writable_text_context_without_an_open_editor() {
    assert!(annotation_actions_enabled(true, false, false, false));
    assert!(!annotation_actions_enabled(false, false, false, false));
    assert!(!annotation_actions_enabled(true, true, false, false));
    assert!(!annotation_actions_enabled(true, false, true, false));
    assert!(!annotation_actions_enabled(true, false, false, true));
}

#[test]
fn zoom_controls_disable_without_a_document_and_at_their_exact_limits() {
    assert_eq!(zoom_controls_enabled(false, 1.0), (false, false));
    assert_eq!(zoom_controls_enabled(true, MIN_ZOOM), (false, true));
    assert_eq!(zoom_controls_enabled(true, 1.0), (true, true));
    assert_eq!(zoom_controls_enabled(true, MAX_ZOOM), (true, false));
}

#[test]
fn only_a_modified_open_comment_requires_discard_confirmation() {
    assert!(!comment_draft_needs_confirmation(false, false));
    assert!(!comment_draft_needs_confirmation(false, true));
    assert!(!comment_draft_needs_confirmation(true, false));
    assert!(comment_draft_needs_confirmation(true, true));
}

#[test]
fn hidden_comment_editor_remains_visible_in_the_responsive_toolbar_label() {
    assert_eq!(comments_toolbar_label(false, false, false), "Comments");
    assert_eq!(comments_toolbar_label(false, true, true), "Notes");
    assert_eq!(
        comments_toolbar_label(true, false, false),
        "Comments · Editing"
    );
    assert_eq!(comments_toolbar_label(true, true, false), "Notes •");
    assert_eq!(comments_toolbar_label(true, true, true), "Notes •");
}

#[test]
fn fluid_context_pill_stays_on_screen_and_prefers_below_the_selection() {
    let below = floating_pill_position(
        Rect {
            x: 160.0,
            y: 120.0,
            width: 80.0,
            height: 18.0,
        },
        500.0,
        600.0,
        FLUID_CONTEXT_PILL_WIDTH,
        FLUID_CONTEXT_PILL_HEIGHT,
    );
    assert_eq!(below.y, 148.0);
    assert!((below.x - 93.0).abs() < f32::EPSILON);

    let clamped_left = floating_pill_position(
        Rect {
            x: -200.0,
            y: 20.0,
            width: 10.0,
            height: 10.0,
        },
        500.0,
        600.0,
        FLUID_CONTEXT_PILL_WIDTH,
        FLUID_CONTEXT_PILL_HEIGHT,
    );
    assert_eq!(clamped_left.x, 12.0);

    let above = floating_pill_position(
        Rect {
            x: 450.0,
            y: 570.0,
            width: 30.0,
            height: 18.0,
        },
        500.0,
        600.0,
        FLUID_CONTEXT_PILL_WIDTH,
        FLUID_CONTEXT_PILL_HEIGHT,
    );
    assert_eq!(above.x, 274.0);
    assert_eq!(above.y, 520.0);
}

#[test]
fn comment_pane_slides_both_directions_and_only_closes_after_back_finishes() {
    let mut pane = CommentPaneState::default();
    pane.show_editor(true);
    assert_eq!(pane.target, 1.0);
    assert!(!pane.close_editor_on_finish);
    for _ in 0..240 {
        pane.advance(1.0 / 60.0);
    }
    assert_eq!(pane.progress, 1.0);
    assert!(!pane.is_animating());

    pane.show_list(true);
    assert_eq!(pane.target, 0.0);
    assert!(pane.close_editor_on_finish);
    pane.advance(1.0 / 60.0);
    assert!(pane.progress > 0.0);
    assert!(pane.is_animating());
    for _ in 0..240 {
        pane.advance(1.0 / 60.0);
    }
    assert_eq!(pane.progress, 0.0);
    assert!(!pane.is_animating());
    assert!(pane.close_editor_on_finish);
}

#[test]
fn high_zoom_raster_is_sharp_without_allocating_a_full_page() {
    let raster = desired_raster_size(
        Rect {
            x: 0.0,
            y: 0.0,
            width: 4_080.0,
            height: 5_280.0,
        },
        2.0,
    );
    assert!(raster.width > 4_096);
    assert!(raster.height > 4_096);
    assert!(raster.width <= MAX_RASTER_DIMENSION);
    assert!(raster.height <= MAX_RASTER_DIMENSION);

    let key = TileKey {
        page: 0,
        raster,
        column: 3,
        row: 4,
    };
    let core = tile_core_rect(key).unwrap();
    let rendered = inflate_tile_rect(core, raster);
    assert!(core.width <= TILE_SIZE && core.height <= TILE_SIZE);
    assert!(rendered.width <= TILE_SIZE + TILE_BLEED * 2);
    assert!(rendered.height <= TILE_SIZE + TILE_BLEED * 2);
    assert!(rendered.width as usize * rendered.height as usize * 4 < 5 * 1024 * 1024);
}

#[test]
fn tile_grid_clips_partial_edges_without_zero_sized_tiles() {
    let raster = RasterSize {
        width: 2_050,
        height: 1_025,
    };
    let first = tile_core_rect(TileKey {
        page: 0,
        raster,
        column: 0,
        row: 0,
    })
    .unwrap();
    let last = tile_core_rect(TileKey {
        page: 0,
        raster,
        column: 2,
        row: 1,
    })
    .unwrap();
    assert_eq!(first.width, 1_024);
    assert_eq!(
        last,
        PixelRect {
            x: 2_048,
            y: 1_024,
            width: 2,
            height: 1
        }
    );
    assert!(
        tile_core_rect(TileKey {
            page: 0,
            raster,
            column: 3,
            row: 0,
        })
        .is_none()
    );
    assert!(
        tile_core_rect(TileKey {
            page: 0,
            raster,
            column: u32::MAX,
            row: 0,
        })
        .is_none()
    );
}

#[test]
fn planner_requests_bounded_tiles_from_both_partially_visible_pages() {
    let pages = letter_pages(2);
    let layout = DocumentLayout::new(&pages, 1.0, 1_100.0);
    let first = layout.page_rect(0).unwrap();
    let second = layout.page_rect(1).unwrap();
    let scroll = Offset {
        x: 0.0,
        y: first.bottom() - 80.0,
    };
    let viewport_height = second.y - scroll.y + 90.0;
    let planned = plan_visible_tiles(&layout, &pages, scroll, 1_100.0, viewport_height, 2.0);
    let visible_pages: HashSet<_> = planned
        .iter()
        .filter_map(|tile| (tile.tier == DemandTier::Visible).then_some(tile.request.key.page))
        .collect();
    assert_eq!(visible_pages, HashSet::from([0, 1]));
    assert!(planned.iter().all(|tile| {
        tile.request.render_rect.width <= TILE_SIZE + TILE_BLEED * 2
            && tile.request.render_rect.height <= TILE_SIZE + TILE_BLEED * 2
            && tile.request.core_rect.width > 0
            && tile.request.core_rect.height > 0
    }));
    let unique: HashSet<_> = planned.iter().map(|tile| tile.request.key).collect();
    assert_eq!(unique.len(), planned.len());
}

#[test]
fn horizontal_panning_only_demands_nearby_columns() {
    let pages = letter_pages(1);
    let layout = DocumentLayout::new(&pages, 5.0, 900.0);
    let page = layout.page_rect(0).unwrap();
    let raster = desired_raster_size(page, 2.0);
    let scroll = Offset {
        x: page.x + page.width * 0.65,
        y: page.y + 400.0,
    };
    let planned = plan_visible_tiles(&layout, &pages, scroll, 700.0, 600.0, 2.0);
    let columns: HashSet<_> = planned.iter().map(|tile| tile.request.key.column).collect();
    assert!(!columns.is_empty());
    assert!(columns.len() <= 4);
    assert!(
        columns
            .iter()
            .all(|column| *column < raster.width.div_ceil(TILE_SIZE))
    );
    assert!(!columns.contains(&0));
}

#[test]
fn adjacent_tile_destinations_share_the_same_global_edge() {
    let page = Rect {
        x: 12.25,
        y: 30.5,
        width: 777.3,
        height: 1_005.8,
    };
    let raster = RasterSize {
        width: 1_663,
        height: 2_151,
    };
    let left = tile_logical_rect(
        page,
        raster,
        PixelRect {
            x: 0,
            y: 0,
            width: 1_024,
            height: 1_024,
        },
    );
    let right = tile_logical_rect(
        page,
        raster,
        PixelRect {
            x: 1_024,
            y: 0,
            width: raster.width - 1_024,
            height: 1_024,
        },
    );
    assert!((left.right() - right.x).abs() < 0.0001);
    assert!((right.right() - page.right()).abs() < 0.0001);
}

#[test]
fn invalid_raster_inputs_are_finite_and_bounded() {
    for rect in [
        Rect {
            x: 0.0,
            y: 0.0,
            width: f32::NAN,
            height: 1.0,
        },
        Rect {
            x: 0.0,
            y: 0.0,
            width: 0.0,
            height: f32::INFINITY,
        },
    ] {
        assert_eq!(
            desired_raster_size(rect, f32::INFINITY),
            RasterSize {
                width: 1,
                height: 1
            }
        );
    }
}

#[test]
fn accelerated_command_wheel_zoom_is_finite_and_clamped_per_packet() {
    assert_eq!(command_wheel_zoom_factor(0.0), Some(1.0));
    assert_eq!(command_wheel_zoom_factor(f32::NAN), None);
    assert_eq!(command_wheel_zoom_factor(f32::INFINITY), None);

    let maximum = command_wheel_zoom_factor(f32::MAX).unwrap();
    let minimum = command_wheel_zoom_factor(-f32::MAX).unwrap();
    assert!((maximum - 1.5_f32.exp()).abs() < f32::EPSILON);
    assert!((minimum - (-1.5_f32).exp()).abs() < f32::EPSILON);
    assert!(minimum > 0.0 && maximum.is_finite());
}

#[test]
fn sidebar_animation_opens_closes_reverses_and_clamps_width() {
    let mut sidebar = SidebarState::default();
    assert_eq!(sidebar.available_width(1_200.0), 0.0);
    sidebar.toggle(SidePanel::Comments);
    assert_eq!(sidebar.target, 1.0);

    let mut previous = sidebar.progress;
    for _ in 0..240 {
        sidebar.advance(1.0 / 60.0);
        assert!(sidebar.progress >= previous);
        assert!((0.0..=1.0).contains(&sidebar.progress));
        previous = sidebar.progress;
    }
    assert_eq!(sidebar.progress, 1.0);
    assert_eq!(sidebar.available_width(1_200.0), SIDEBAR_WIDTH);
    assert_eq!(sidebar.available_width(500.0), 200.0);
    assert_eq!(sidebar.available_width(250.0), 0.0);

    sidebar.toggle(SidePanel::Comments);
    sidebar.advance(1.0 / 60.0);
    let closing_progress = sidebar.progress;
    assert!(closing_progress < 1.0);
    sidebar.toggle(SidePanel::Comments);
    assert_eq!(sidebar.target, 1.0);
    sidebar.advance(1.0 / 60.0);
    assert!(sidebar.progress > closing_progress);

    sidebar.toggle(SidePanel::Comments);
    for _ in 0..240 {
        sidebar.advance(1.0 / 60.0);
    }
    assert_eq!(sidebar.progress, 0.0);
    assert!(!sidebar.is_animating());
}

#[test]
fn paint_budget_is_hard_and_active_items_sort_first() {
    let mut budget = PaintBudget::new(3);
    assert!(!budget.exhausted());
    assert!(budget.take());
    assert!(budget.take());
    assert!(budget.take());
    assert!(budget.exhausted());
    assert!(!budget.take());
    assert!(!budget.take());

    let mut ids = [AnnotationId(3), AnnotationId(1), AnnotationId(2)];
    ids.sort_by_key(|id| is_inactive(*id, Some(AnnotationId(2))));
    assert_eq!(ids[0], AnnotationId(2));
    assert!(ids[1..].contains(&AnnotationId(1)));
    assert!(ids[1..].contains(&AnnotationId(3)));
}

#[test]
fn switching_sidebar_panels_keeps_the_sidebar_open() {
    let mut sidebar = SidebarState::default();
    sidebar.toggle(SidePanel::Comments);
    for _ in 0..240 {
        sidebar.advance(1.0 / 60.0);
    }
    sidebar.toggle(SidePanel::Search);
    assert_eq!(sidebar.panel, SidePanel::Search);
    assert_eq!(sidebar.target, 1.0);
    assert_eq!(sidebar.progress, 1.0);
}

#[test]
fn search_list_groups_matches_under_one_heading_per_page() {
    let rows = search_list_rows(&[
        SearchMatchId {
            page: 1,
            start: 2,
            end: 3,
        },
        SearchMatchId {
            page: 1,
            start: 8,
            end: 9,
        },
        SearchMatchId {
            page: 4,
            start: 1,
            end: 2,
        },
    ]);
    assert_eq!(rows.len(), 5);
    assert_eq!(rows[0], SearchListRow::Page(1));
    assert!(matches!(rows[1], SearchListRow::Match { ordinal: 0, .. }));
    assert!(matches!(rows[2], SearchListRow::Match { ordinal: 1, .. }));
    assert_eq!(rows[3], SearchListRow::Page(4));
    assert!(matches!(rows[4], SearchListRow::Match { ordinal: 2, .. }));
}

#[test]
fn search_jump_uses_result_geometry_and_a_bounded_fallback() {
    let result = SearchMatch {
        id: SearchMatchId {
            page: 3,
            start: 7,
            end: 12,
        },
        preview: "result".to_owned(),
        preview_match: 0..6,
        highlight_runs: vec![
            TextBounds {
                left: 0.2,
                top: 0.4,
                right: 0.5,
                bottom: 0.46,
            },
            TextBounds {
                left: 0.2,
                top: 0.52,
                right: 0.42,
                bottom: 0.58,
            },
        ],
    };

    let layout = DocumentLayout::new(
        &[PageSize {
            width: 600.0,
            height: 800.0,
        }; 6],
        1.0,
        900.0,
    );
    let target = search_document_jump(&result)
        .resolve(&layout, 0.0, 500.0, 400.0, 500.0)
        .unwrap()
        .focus
        .unwrap();
    assert_eq!(target.page, 3);
    assert!((target.y_fraction - 0.43).abs() < 0.001);
    assert_eq!(target.text_runs, result.highlight_runs);
    assert_eq!(target.tone, NavigationFocusTone::SearchMatch);
    assert_eq!(target.motion, NavigationFocusMotion::Pulse);

    let fallback = search_document_jump(&SearchMatch {
        id: SearchMatchId {
            page: 5,
            start: 1,
            end: 2,
        },
        preview: "unlocated".to_owned(),
        preview_match: 0..9,
        highlight_runs: Vec::new(),
    })
    .resolve(&layout, 0.0, 500.0, 400.0, 500.0)
    .unwrap()
    .focus
    .unwrap();
    assert_eq!(fallback.page, 5);
    assert_eq!(fallback.y_fraction, 0.15);
    assert!(fallback.text_runs.is_empty());
    assert_eq!(fallback.tone, NavigationFocusTone::SearchMatch);
    assert_eq!(fallback.motion, NavigationFocusMotion::Pulse);
}

#[test]
fn search_navigation_handles_initial_stale_and_wrapped_results() {
    let first = SearchMatchId {
        page: 0,
        start: 2,
        end: 5,
    };
    let second = SearchMatchId {
        page: 2,
        start: 7,
        end: 10,
    };
    let stale = SearchMatchId {
        page: 9,
        start: 0,
        end: 0,
    };
    let results = [first, second];

    assert_eq!(next_search_match_id(&[], None, true), None);
    assert_eq!(next_search_match_id(&results, None, true), Some(first));
    assert_eq!(next_search_match_id(&results, None, false), Some(second));
    assert_eq!(
        next_search_match_id(&results, Some(first), true),
        Some(second)
    );
    assert_eq!(
        next_search_match_id(&results, Some(second), true),
        Some(first)
    );
    assert_eq!(
        next_search_match_id(&results, Some(first), false),
        Some(second)
    );
    assert_eq!(
        next_search_match_id(&results, Some(stale), true),
        Some(first)
    );
    assert_eq!(
        next_search_match_id(&results, Some(stale), false),
        Some(second)
    );
}

#[test]
fn pending_document_open_waits_opens_or_cancels_without_losing_its_path() {
    let first = PathBuf::from("next.pdf");
    let mut pending = Some(first.clone());

    assert_eq!(
        transition_pending_open(&mut pending, Some(6), 5, false),
        PendingOpenTransition::Waiting
    );
    assert_eq!(pending, Some(first.clone()));
    assert_eq!(
        transition_pending_open(&mut pending, Some(6), 6, false),
        PendingOpenTransition::Open(first)
    );
    assert_eq!(pending, None);

    let replacement = PathBuf::from("replacement.pdf");
    pending = Some(replacement.clone());
    assert_eq!(
        transition_pending_open(&mut pending, Some(1), 0, true),
        PendingOpenTransition::Cancelled(replacement)
    );
    assert_eq!(pending, None);
    assert_eq!(
        transition_pending_open(&mut pending, None, 0, false),
        PendingOpenTransition::None
    );
}

#[test]
fn comment_previews_collapse_whitespace_and_truncate_by_unicode_character() {
    assert_eq!(compact_preview("  Café\n\t日本語  ", 32), "Café 日本語");
    assert_eq!(compact_preview("😀😀😀😀", 3), "😀😀😀…");
    assert_eq!(compact_preview("one   two three", 7), "one two…");
    assert_eq!(compact_preview("", 3), "");
}

#[test]
fn annotation_io_revalidates_pdf_identity_immediately_before_save() {
    let directory = TestDirectory::new("identity-recheck");
    let pdf = directory.path().join("document.pdf");
    std::fs::write(&pdf, b"original pdf bytes").unwrap();
    let identity = DocumentIdentity::from_pdf(&pdf, 1).unwrap();
    let mut annotations = AnnotationSet::new(1);
    annotations
        .add(
            TextRange::new(
                TextPosition { page: 0, index: 0 },
                TextPosition { page: 0, index: 4 },
            ),
            Some(HighlightColor::Yellow),
            None,
        )
        .unwrap();

    std::fs::write(&pdf, b"changed pdf bytes that invalidate identity").unwrap();
    let (io, events) = AnnotationIo::start();
    assert!(io.save(7, pdf.clone(), identity, 0, annotations));
    let event = events.recv_timeout(Duration::from_secs(2)).unwrap();
    assert!(matches!(
        event,
        AnnotationIoEvent::Failed {
            generation: 7,
            operation: AnnotationIoOperation::Save,
            revision: Some(1),
            ..
        }
    ));
    assert!(!crate::annotations::sidecar_path(&pdf).unwrap().exists());
}

#[test]
fn annotation_io_queued_revisions_leave_the_latest_snapshot_on_disk() {
    let directory = TestDirectory::new("latest-revision");
    let pdf = directory.path().join("document.pdf");
    std::fs::write(&pdf, b"stable pdf bytes").unwrap();
    let identity = DocumentIdentity::from_pdf(&pdf, 1).unwrap();
    let range = TextRange::new(
        TextPosition { page: 0, index: 1 },
        TextPosition { page: 0, index: 3 },
    );
    let mut revision_one = AnnotationSet::new(1);
    revision_one
        .add(range, Some(HighlightColor::Green), None)
        .unwrap();
    let mut revision_two = revision_one.clone();
    revision_two
        .add(
            TextRange::new(
                TextPosition { page: 0, index: 8 },
                TextPosition { page: 0, index: 12 },
            ),
            None,
            Some("**persisted** comment".into()),
        )
        .unwrap();

    let (io, events) = AnnotationIo::start();
    assert!(io.save(3, pdf.clone(), identity.clone(), 0, revision_one));
    assert!(io.save(3, pdf.clone(), identity.clone(), 0, revision_two.clone()));
    let mut revisions = Vec::new();
    loop {
        match events.recv_timeout(Duration::from_secs(2)).unwrap() {
            AnnotationIoEvent::Saved { revision, .. } => {
                revisions.push(revision);
                if revision == 2 {
                    break;
                }
            }
            AnnotationIoEvent::Failed { message, .. } => panic!("save failed: {message}"),
            AnnotationIoEvent::Loaded { .. } => panic!("unexpected load event"),
        }
    }
    assert!(matches!(revisions.as_slice(), [2] | [1, 2]));
    let document = DocumentKey::new(pdf.clone(), identity.clone());
    assert_eq!(JsonSidecarStore.load(&document).unwrap(), revision_two);
}

#[test]
fn annotation_io_rejects_a_stale_second_writer_without_overwriting_disk() {
    let directory = TestDirectory::new("concurrent-writers");
    let pdf = directory.path().join("document.pdf");
    std::fs::write(&pdf, b"stable pdf bytes").unwrap();
    let identity = DocumentIdentity::from_pdf(&pdf, 1).unwrap();

    let (first_writer, first_events) = AnnotationIo::start();
    let (second_writer, second_events) = AnnotationIo::start();
    assert!(first_writer.load(11, pdf.clone(), 1));
    assert!(second_writer.load(22, pdf.clone(), 1));
    for events in [&first_events, &second_events] {
        match events.recv_timeout(Duration::from_secs(2)).unwrap() {
            AnnotationIoEvent::Loaded { annotations, .. } => {
                assert_eq!(annotations.revision(), 0)
            }
            AnnotationIoEvent::Saved { .. } => panic!("unexpected save event during load"),
            AnnotationIoEvent::Failed { message, .. } => {
                panic!("initial sidecar load failed: {message}")
            }
        }
    }

    let first_range = TextRange::new(
        TextPosition { page: 0, index: 1 },
        TextPosition { page: 0, index: 3 },
    );
    let mut first_revision = AnnotationSet::new(1);
    first_revision
        .add(first_range, Some(HighlightColor::Green), None)
        .unwrap();
    assert!(first_writer.save(11, pdf.clone(), identity.clone(), 0, first_revision.clone(),));
    assert!(matches!(
        first_events.recv_timeout(Duration::from_secs(2)).unwrap(),
        AnnotationIoEvent::Saved {
            generation: 11,
            revision: 1
        }
    ));

    let mut first_latest = first_revision;
    first_latest
        .add(
            TextRange::new(
                TextPosition { page: 0, index: 8 },
                TextPosition { page: 0, index: 12 },
            ),
            None,
            Some("first writer's comment".into()),
        )
        .unwrap();
    // The deliberately stale fallback proves that a successful local save
    // advanced the worker's observed on-disk revision from 0 to 1.
    assert!(first_writer.save(11, pdf.clone(), identity.clone(), 0, first_latest.clone(),));
    assert!(matches!(
        first_events.recv_timeout(Duration::from_secs(2)).unwrap(),
        AnnotationIoEvent::Saved {
            generation: 11,
            revision: 2
        }
    ));

    let mut stale_second_writer = AnnotationSet::new(1);
    stale_second_writer
        .add(first_range, Some(HighlightColor::Purple), None)
        .unwrap();
    assert!(second_writer.save(22, pdf.clone(), identity.clone(), 0, stale_second_writer,));
    match second_events.recv_timeout(Duration::from_secs(2)).unwrap() {
        AnnotationIoEvent::Failed {
            generation: 22,
            operation: AnnotationIoOperation::Save,
            revision: Some(1),
            message,
        } => {
            assert!(message.contains("expected revision 0"));
            assert!(message.contains("found revision 2"));
        }
        AnnotationIoEvent::Saved { .. } => {
            panic!("a stale second writer overwrote the sidecar")
        }
        AnnotationIoEvent::Loaded { .. } => panic!("unexpected load event during save"),
        AnnotationIoEvent::Failed { message, .. } => {
            panic!("unexpected conflict shape: {message}")
        }
    }
    let document = DocumentKey::new(pdf, identity);
    assert_eq!(JsonSidecarStore.load(&document).unwrap(), first_latest);
}
