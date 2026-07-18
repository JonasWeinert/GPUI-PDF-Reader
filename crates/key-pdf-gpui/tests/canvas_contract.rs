use key_pdf_core::Rect;
use key_pdf_gpui::{
    PdfCanvasLimits, PdfCanvasMetrics, PdfCanvasPageGeometry, ScrollOffset, plan_pdf_canvas_frame,
};

#[test]
fn an_external_host_can_plan_a_bounded_canvas_without_an_engine() {
    let pages = [
        PdfCanvasPageGeometry {
            page_index: 0,
            rect: Rect {
                x: 40.0,
                y: 24.0,
                width: 600.0,
                height: 800.0,
            },
            tile_count: 4,
        },
        PdfCanvasPageGeometry {
            page_index: 1,
            rect: Rect {
                x: 40.0,
                y: 848.0,
                width: 600.0,
                height: 800.0,
            },
            tile_count: 4,
        },
    ];
    let plan = plan_pdf_canvas_frame(
        &pages,
        PdfCanvasMetrics::new(ScrollOffset::new(0.0, 760.0), 700.0, 800.0, 1_672.0, 0.0),
        PdfCanvasLimits::default(),
    );

    assert_eq!(plan.pages.len(), 2);
    assert_eq!(plan.pages[0].viewport_rect.y, -736.0);
    assert_eq!(plan.pages[1].viewport_rect.y, 88.0);
    assert_eq!(plan.admitted_tiles, 8);
    assert!(plan.scrollbars.vertical.is_some());
    assert!(plan.scrollbars.horizontal.is_none());
}

#[cfg(target_os = "macos")]
#[test]
fn an_external_gpui_harness_can_embed_the_rendered_component() {
    use gpui::prelude::Styled as _;
    use key_pdf_gpui::{PdfCanvasPage, PdfCanvasSnapshot, PdfCanvasStyle, pdf_canvas};

    let page = PdfCanvasPage::new(
        7,
        Rect {
            x: 24.0,
            y: 24.0,
            width: 500.0,
            height: 700.0,
        },
        Vec::new(),
        "host overlay payload",
    );
    let snapshot = PdfCanvasSnapshot::new(
        vec![page],
        PdfCanvasMetrics::new(ScrollOffset::default(), 600.0, 800.0, 748.0, 0.0),
        PdfCanvasStyle::default(),
    );

    // No reader main-window type, PDFium handle, cache, file picker, or app
    // singleton is required to construct the complete rendered component.
    let _element = pdf_canvas(snapshot, |page, _window| {
        assert_eq!(page.page_index, 7);
        assert_eq!(page.overlay, &"host overlay payload");
    })
    .size_full();
}
