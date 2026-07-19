use key_pdf_core::{DocumentLayout, PageAnchor, PageSize, PixelRect, RasterSize, Rect, TileKey};
use key_pdf_gpui::{
    DEFAULT_MAX_RASTER_DIMENSION, DEFAULT_MAX_ZOOM, DEFAULT_MIN_ZOOM, DEFAULT_TILE_BLEED,
    DEFAULT_TILE_SIZE, DemandTier, InputDisposition, PdfReaderConfig, PdfReaderLimits,
    ScrollBehavior, ScrollOffset, TilePlanningInput, ViewportController, ViewportMetrics,
    ViewportPoint, command_wheel_zoom_factor, desired_raster_size, inflate_tile_rect,
    plan_visible_tiles, tile_core_rect, tile_logical_rect,
};
use key_pdf_runtime::{ColorMode, DemandIntent, DemandPriority, DocumentSessionManager};
use std::collections::HashSet;

fn letter_pages(count: usize) -> Vec<PageSize> {
    vec![
        PageSize {
            width: 612.0,
            height: 792.0,
        };
        count
    ]
}

fn controller(pages: usize, width: f32, height: f32) -> ViewportController {
    let mut controller =
        ViewportController::with_document(PdfReaderConfig::default(), letter_pages(pages)).unwrap();
    assert_eq!(
        controller.set_viewport(ViewportMetrics {
            width,
            height,
            right_occlusion: 0.0,
            scale_factor: 2.0,
        }),
        InputDisposition::Applied
    );
    controller.drain_events().for_each(drop);
    controller
}

fn anchor_at_center(controller: &ViewportController) -> PageAnchor {
    let metrics = controller.metrics();
    let scroll = controller.scroll();
    controller
        .layout()
        .unwrap()
        .anchor_at_content_point(
            scroll.x + metrics.safe_width() * 0.5,
            scroll.y + metrics.height * 0.5,
        )
        .unwrap()
}

#[test]
fn rapid_zoom_packets_stay_finite_and_inside_contract_limits() {
    let mut controller = controller(4, 1_000.0, 700.0);
    let point = ViewportPoint::new(500.0, 350.0);
    for index in 0..20_000 {
        let delta = if index % 2 == 0 { f32::MAX } else { -f32::MAX };
        assert_ne!(
            controller.zoom_from_wheel(delta, point),
            InputDisposition::IgnoredInvalid
        );
        assert!(controller.zoom().is_finite());
        assert!((DEFAULT_MIN_ZOOM..=DEFAULT_MAX_ZOOM).contains(&controller.zoom()));
        let scroll = controller.scroll();
        assert!(scroll.x.is_finite() && scroll.y.is_finite());
    }
    assert_eq!(
        controller.zoom_from_wheel(f32::NAN, point),
        InputDisposition::IgnoredInvalid
    );
}

#[test]
fn accelerated_wheel_factor_is_finite_and_clamped_per_packet() {
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
fn planner_requests_bounded_tiles_from_both_partially_visible_pages() {
    let pages = letter_pages(2);
    let layout = DocumentLayout::new(&pages, 1.0, 1_100.0);
    let first = layout.page_rect(0).unwrap();
    let second = layout.page_rect(1).unwrap();
    let scroll_y = first.bottom() - 80.0;
    let viewport_height = second.y - scroll_y + 90.0;
    let plan = plan_visible_tiles(
        &layout,
        &pages,
        TilePlanningInput::new(
            Rect {
                x: 0.0,
                y: scroll_y,
                width: 1_100.0,
                height: viewport_height,
            },
            2.0,
        ),
        &PdfReaderLimits::default(),
    );
    let visible_pages: HashSet<_> = plan
        .tiles
        .iter()
        .filter_map(|tile| (tile.tier == DemandTier::Visible).then_some(tile.request.key.page))
        .collect();
    assert_eq!(visible_pages, HashSet::from([0, 1]));
    assert!(plan.tiles.iter().all(|tile| {
        tile.request.render_rect.width <= DEFAULT_TILE_SIZE + DEFAULT_TILE_BLEED * 2
            && tile.request.render_rect.height <= DEFAULT_TILE_SIZE + DEFAULT_TILE_BLEED * 2
            && tile.request.core_rect.width > 0
            && tile.request.core_rect.height > 0
    }));
    let unique: HashSet<_> = plan.tiles.iter().map(|tile| tile.request.key).collect();
    assert_eq!(unique.len(), plan.tiles.len());
    assert_eq!(plan.visible_pages, 0..2);
}

#[test]
fn horizontal_panning_only_demands_nearby_columns() {
    let pages = letter_pages(1);
    let layout = DocumentLayout::new(&pages, 5.0, 900.0);
    let page = layout.page_rect(0).unwrap();
    let raster = desired_raster_size(page, 2.0, &PdfReaderLimits::default());
    let plan = plan_visible_tiles(
        &layout,
        &pages,
        TilePlanningInput::new(
            Rect {
                x: page.x + page.width * 0.65,
                y: page.y + 400.0,
                width: 700.0,
                height: 600.0,
            },
            2.0,
        ),
        &PdfReaderLimits::default(),
    );
    let columns: HashSet<_> = plan
        .tiles
        .iter()
        .map(|tile| tile.request.key.column)
        .collect();
    assert!(!columns.is_empty());
    assert!(columns.len() <= 4);
    assert!(
        columns
            .iter()
            .all(|column| *column < raster.width.div_ceil(DEFAULT_TILE_SIZE))
    );
    assert!(!columns.contains(&0));
}

#[test]
fn high_zoom_raster_is_sharp_without_allocating_a_full_page() {
    let limits = PdfReaderLimits::default();
    let raster = desired_raster_size(
        Rect {
            x: 0.0,
            y: 0.0,
            width: 4_080.0,
            height: 5_280.0,
        },
        2.0,
        &limits,
    );
    assert!(raster.width > 4_096 && raster.height > 4_096);
    assert!(raster.width <= DEFAULT_MAX_RASTER_DIMENSION);
    assert!(raster.height <= DEFAULT_MAX_RASTER_DIMENSION);
    let key = TileKey {
        page: 0,
        raster,
        column: 3,
        row: 4,
    };
    let core = tile_core_rect(key, &limits).unwrap();
    let rendered = inflate_tile_rect(core, raster, &limits);
    assert!(core.width <= DEFAULT_TILE_SIZE && core.height <= DEFAULT_TILE_SIZE);
    assert!(rendered.width <= DEFAULT_TILE_SIZE + DEFAULT_TILE_BLEED * 2);
    assert!(rendered.height <= DEFAULT_TILE_SIZE + DEFAULT_TILE_BLEED * 2);
    assert!(rendered.width as usize * rendered.height as usize * 4 < 5 * 1024 * 1024);
}

#[test]
fn raster_and_tile_inputs_are_finite_clipped_and_non_zero() {
    let limits = PdfReaderLimits::default();
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
            desired_raster_size(rect, f32::INFINITY, &limits),
            RasterSize {
                width: 1,
                height: 1
            }
        );
    }
    let raster = RasterSize {
        width: 2_050,
        height: 1_025,
    };
    assert_eq!(
        tile_core_rect(
            TileKey {
                page: 0,
                raster,
                column: 2,
                row: 1,
            },
            &limits,
        ),
        Some(PixelRect {
            x: 2_048,
            y: 1_024,
            width: 2,
            height: 1,
        })
    );
    assert!(
        tile_core_rect(
            TileKey {
                page: 0,
                raster,
                column: u32::MAX,
                row: 0,
            },
            &limits,
        )
        .is_none()
    );
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
fn viewport_resize_and_overlay_occlusion_preserve_the_center_page_anchor() {
    let mut controller = controller(6, 1_200.0, 760.0);
    controller.set_scroll(ScrollOffset::new(0.0, 1_900.0));
    let before = anchor_at_center(&controller);
    assert_eq!(
        controller.set_viewport(ViewportMetrics {
            width: 856.0,
            height: 760.0,
            right_occlusion: 0.0,
            scale_factor: 2.0,
        }),
        InputDisposition::Applied
    );
    let resized = anchor_at_center(&controller);
    assert_eq!(before.page, resized.page);
    assert!((before.x_fraction - resized.x_fraction).abs() < 0.0001);
    assert!((before.y_fraction - resized.y_fraction).abs() < 0.0001);

    let before_overlay = resized;
    controller.set_viewport(ViewportMetrics {
        width: 1_200.0,
        height: 760.0,
        right_occlusion: 344.0,
        scale_factor: 2.0,
    });
    let overlay = anchor_at_center(&controller);
    assert_eq!(before_overlay.page, overlay.page);
    assert!((before_overlay.x_fraction - overlay.x_fraction).abs() < 0.0001);
    assert!((before_overlay.y_fraction - overlay.y_fraction).abs() < 0.0001);
}

#[test]
fn fit_width_reacts_to_resize_and_preserves_a_center_anchor() {
    let mut controller = controller(5, 1_100.0, 720.0);
    controller.fit_width();
    controller.set_scroll(ScrollOffset::new(0.0, 1_600.0));
    let before = anchor_at_center(&controller);
    let old_zoom = controller.zoom();
    controller.set_viewport(ViewportMetrics {
        width: 800.0,
        height: 720.0,
        right_occlusion: 0.0,
        scale_factor: 2.0,
    });
    let after = anchor_at_center(&controller);
    assert!(controller.fit_width_enabled());
    assert!(controller.zoom() < old_zoom);
    assert_eq!(before.page, after.page);
    assert!((before.y_fraction - after.y_fraction).abs() < 0.0001);
}

#[test]
fn horizontal_scroll_and_smooth_navigation_are_bounded() {
    let mut controller = controller(1, 700.0, 600.0);
    controller.zoom_at(5.0, ViewportPoint::new(350.0, 300.0));
    assert_eq!(
        controller.scroll_by(
            ScrollOffset::new(f32::MAX, f32::MAX),
            ScrollBehavior::Smooth,
        ),
        InputDisposition::Applied
    );
    assert!(controller.scroll_target().x.is_finite());
    assert!(controller.scroll_target().y.is_finite());
    for _ in 0..300 {
        controller.advance_navigation(1.0 / 60.0);
    }
    assert!(!controller.is_scrolling());
    assert_eq!(controller.scroll(), controller.scroll_target());
    assert!(controller.scroll().x > 0.0);
}

#[test]
fn planner_honors_a_hard_consumer_tile_cap() {
    let pages = letter_pages(10);
    let layout = DocumentLayout::new(&pages, 5.0, 32_000.0);
    let limits = PdfReaderLimits {
        max_planned_tiles: 7,
        ..PdfReaderLimits::default()
    };
    let plan = plan_visible_tiles(
        &layout,
        &pages,
        TilePlanningInput::new(
            Rect {
                x: 0.0,
                y: 0.0,
                width: 32_000.0,
                height: 32_000.0,
            },
            2.0,
        ),
        &limits,
    );
    assert_eq!(plan.tiles.len(), 7);
    assert!(plan.tiles.windows(2).all(|pair| {
        (pair[0].tier, pair[0].distance, pair[0].request.key)
            <= (pair[1].tier, pair[1].distance, pair[1].request.key)
    }));
}

#[test]
fn replacement_document_with_equal_page_count_uses_its_own_geometry() {
    let mut controller = controller(1, 900.0, 700.0);
    let old = controller.layout().unwrap().page_rect(0).unwrap();
    controller
        .set_document_pages(vec![PageSize {
            width: 300.0,
            height: 1_200.0,
        }])
        .unwrap();
    let replacement = controller.layout().unwrap().page_rect(0).unwrap();
    assert_ne!(old.width, replacement.width);
    assert_ne!(old.height, replacement.height);
    assert!((replacement.width / replacement.height - 0.25).abs() < 0.0001);
}

#[test]
fn fit_width_emits_only_the_final_enabled_zoom_state() {
    let mut controller = controller(2, 1_000.0, 700.0);
    assert_eq!(controller.fit_width(), InputDisposition::Applied);
    let zoom_events = controller
        .drain_events()
        .filter_map(|event| match event {
            key_pdf_gpui::PdfReaderEvent::ZoomChanged { zoom, fit_width } => {
                Some((zoom, fit_width))
            }
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(zoom_events.len(), 1);
    assert!(zoom_events[0].0.is_finite());
    assert!(zoom_events[0].1);
}

#[test]
fn planned_geometry_binds_to_a_generation_checked_runtime_demand() {
    let controller = controller(1, 900.0, 700.0);
    let tile = controller.plan_tiles().tiles[0];
    let session = DocumentSessionManager::new().begin().unwrap();
    let demand = tile
        .request
        .render_demand(
            &session,
            ColorMode::Original,
            DemandPriority::VISIBLE,
            DemandIntent::Visible,
        )
        .unwrap();
    assert_eq!(demand.key(), tile.request.key);
    assert_eq!(demand.core_rect(), tile.request.core_rect);
    assert_eq!(demand.render_rect(), tile.request.render_rect);
    assert_eq!(demand.stamp().generation(), session.generation());
}

#[test]
fn cancelling_a_smooth_scroll_by_setting_the_current_position_is_observable() {
    let mut controller = controller(4, 900.0, 700.0);
    controller.set_scroll(ScrollOffset::new(0.0, 700.0));
    controller.drain_events().for_each(drop);
    controller.scroll_by(ScrollOffset::new(0.0, 500.0), ScrollBehavior::Smooth);
    let current = controller.scroll();
    assert_ne!(current, controller.scroll_target());
    assert_eq!(controller.set_scroll(current), InputDisposition::Applied);
    assert_eq!(controller.scroll(), controller.scroll_target());
    assert!(controller.drain_events().any(|event| matches!(
        event,
        key_pdf_gpui::PdfReaderEvent::ScrollChanged { offset, target }
            if offset == current && target == current
    )));
}

#[test]
fn hostile_planner_limits_are_normalized_before_use() {
    let normalized = PdfReaderLimits {
        tile_size: 0,
        max_raster_dimension: u32::MAX,
        max_planned_tiles: usize::MAX,
        max_viewport_dimension: f32::INFINITY,
        ..PdfReaderLimits::default()
    }
    .normalized();
    assert!(normalized.tile_size >= 256);
    assert_eq!(
        normalized.max_raster_dimension,
        DEFAULT_MAX_RASTER_DIMENSION
    );
    assert!(normalized.max_planned_tiles <= 4_096);
    assert!(normalized.max_viewport_dimension.is_finite());
}

#[test]
fn absolute_smooth_navigation_uses_the_same_bounded_target_state() {
    let mut controller = controller(4, 900.0, 700.0);
    assert_eq!(
        controller.scroll_to(
            ScrollOffset::new(f32::MAX, f32::MAX),
            ScrollBehavior::Smooth,
        ),
        InputDisposition::Applied
    );
    assert_eq!(controller.scroll_target(), controller.maximum_scroll());
    assert_ne!(controller.scroll(), controller.scroll_target());
}
