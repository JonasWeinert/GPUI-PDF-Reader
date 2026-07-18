use super::*;
use crate::model::{TextBounds, TextChar};
use std::time::Instant;

fn empty_worker_state(generation: u64) -> WorkerState {
    WorkerState {
        runtime: PdfRuntime::new(
            PdfiumEngine::new(PdfiumLibraryConfig::new(Vec::<PathBuf>::new())),
            CachePolicy::default(),
        ),
        generation: Some(generation),
        document_cancellation: CancellationSource::new(),
        automatic_text_cancellation: CancellationSource::new(),
        explicit_text_cancellation: CancellationSource::new(),
        text_cache: HashMap::new(),
        automatic_text_needs_quiet: false,
        page_count: 3,
        search: None,
        latest_search_revision: None,
        scientific: None,
        renders: RenderQueues::new(),
        previews: LatestWinsQueue::new(1),
    }
}

fn search_job(generation: u64, revision: u64) -> SearchJob {
    SearchJob {
        generation,
        revision,
        query: SearchQuery::new("page").unwrap(),
        next_page: 0,
        page_count: 3,
        total_results: 0,
        total_highlight_runs: 0,
        skipped_pages: 0,
        truncated: false,
        cancellation: CancellationSource::new(),
    }
}

#[test]
fn caller_side_replacement_cancels_in_flight_work_immediately() {
    let cancellations = WorkerCancellations::default();
    let first_render = cancellations.replace_render();
    let second_render = cancellations.replace_render();
    assert!(first_render.is_cancelled());
    assert!(!second_render.is_cancelled());

    let search = cancellations.replace_search();
    let document = cancellations.begin_document();
    assert!(second_render.is_cancelled());
    assert!(search.is_cancelled());
    assert!(!document.is_cancelled());
}

#[test]
fn appearance_mapping_preserves_semantic_rgb_values() {
    let mode = RenderAppearance::ForcedColors {
        background: RenderColor {
            red: 1,
            green: 2,
            blue: 3,
        },
        foreground: RenderColor {
            red: 4,
            green: 5,
            blue: 6,
        },
    }
    .color_mode();
    assert_eq!(
        mode,
        ColorMode::Forced {
            background: PixelColor {
                red: 1,
                green: 2,
                blue: 3,
                alpha: 255,
            },
            foreground: PixelColor {
                red: 4,
                green: 5,
                blue: 6,
                alpha: 255,
            },
        }
    );
}

#[test]
fn render_priority_keeps_visible_work_above_prefetch_work() {
    let tile = TileRequest {
        key: TileKey {
            page: 0,
            raster: RasterSize {
                width: 100,
                height: 100,
            },
            column: 0,
            row: 0,
        },
        core_rect: PixelRect {
            x: 0,
            y: 0,
            width: 100,
            height: 100,
        },
        render_rect: PixelRect {
            x: 0,
            y: 0,
            width: 100,
            height: 100,
        },
    };
    let visible = RenderRequest {
        generation: 1,
        appearance: RenderAppearance::Normal,
        tile,
        priority: 9,
        prefetch: false,
    };
    let prefetch = RenderRequest {
        prefetch: true,
        priority: 0,
        ..visible.clone()
    };
    assert!(render_priority(&visible).1 > render_priority(&prefetch).1);
}

#[test]
fn text_failure_is_cached_as_a_non_fatal_empty_layer() {
    let calls = std::cell::Cell::new(0);
    let mut cache = HashMap::new();
    let (text, warning) = cache_text_layer(&mut cache, 16, 2, || {
        calls.set(calls.get() + 1);
        Err("synthetic text failure".into())
    });
    assert!(text.unwrap().is_empty());
    assert!(warning.unwrap().contains("synthetic text failure"));

    let (text, warning) = cache_text_layer(&mut cache, 16, 2, || {
        calls.set(calls.get() + 1);
        Ok(Arc::new(TextLayer::empty()))
    });
    assert!(text.is_none());
    assert!(warning.is_none());
    assert_eq!(calls.get(), 1);
}

#[test]
fn text_cache_evicts_the_page_farthest_from_new_work() {
    let mut cache = HashMap::new();
    for page in 0..=16 {
        let (layer, warning) =
            cache_text_layer(&mut cache, 16, page, || Ok(Arc::new(TextLayer::empty())));
        assert!(layer.is_some());
        assert!(warning.is_none());
    }
    assert_eq!(cache.len(), 16);
    assert!(cache.contains_key(&16));
    assert!(!cache.contains_key(&0));
}

#[test]
fn latest_search_revision_replaces_and_rejects_stale_demand() {
    let mut state = empty_worker_state(7);
    assert!(advance_search_revision(&mut state, 7, 10));
    state.search = Some(search_job(7, 10));
    assert!(advance_search_revision(&mut state, 7, 11));
    assert!(state.search.is_none());
    state.search = Some(search_job(7, 11));
    assert!(!advance_search_revision(&mut state, 7, 10));
    assert_eq!(state.search.as_ref().unwrap().revision, 11);
    assert!(!advance_search_revision(&mut state, 6, 12));
}

#[test]
fn cancellation_barrier_preserves_the_replacement_revision() {
    let mut state = empty_worker_state(7);
    state.latest_search_revision = Some(10);
    state.search = Some(search_job(7, 10));
    cancel_searches_before(&mut state, 7, 11);
    assert!(state.search.is_none());
    assert_eq!(state.latest_search_revision, Some(10));

    assert!(advance_search_revision(&mut state, 7, 11));
    state.search = Some(search_job(7, 11));
    cancel_searches_before(&mut state, 7, 11);
    assert_eq!(state.search.as_ref().unwrap().revision, 11);
}

#[test]
fn search_highlight_storage_stops_before_the_global_run_cap() {
    let bounds = TextBounds {
        left: 0.1,
        top: 0.1,
        right: 0.2,
        bottom: 0.2,
    };
    let result = |start, run_count| crate::search::SearchMatch {
        id: crate::search::SearchMatchId {
            page: 0,
            start,
            end: start,
        },
        preview: String::new(),
        preview_match: 0..0,
        highlight_runs: vec![bounds; run_count],
    };
    let mut results = SearchPageResults {
        page: 0,
        matches: vec![result(0, 2), result(1, 2)],
        truncated: false,
    };
    assert_eq!(cap_search_highlight_runs(&mut results, 3), 2);
    assert_eq!(results.matches.len(), 1);
    assert!(results.truncated);
}

#[test]
fn no_match_page_emits_no_empty_result_event() {
    let query = SearchQuery::new("absent").unwrap();
    let text = [TextChar {
        value: 'x',
        bounds: None,
    }];
    let SearchPageOutcome::Complete(results) =
        search_page(2, &text, &query, MAX_SEARCH_RESULTS, || false)
    else {
        panic!("search unexpectedly cancelled");
    };
    let (events, received) = mpsc::sync_channel(1);
    assert!(send_search_page_results(&events, 7, 4, results));
    assert!(matches!(
        received.try_recv(),
        Err(mpsc::TryRecvError::Empty)
    ));
}

#[test]
fn worker_maps_runtime_open_render_text_preview_and_search_events() {
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join("tests/fixtures/interaction.pdf");
    assert!(fixture.is_file());

    let (worker, events) = PdfWorker::start();
    assert!(matches!(
        events.recv_timeout(Duration::from_secs(5)).unwrap(),
        WorkerEvent::Ready
    ));
    let generation = 41;
    assert!(worker.open(generation, fixture));
    let deadline = Instant::now() + Duration::from_secs(10);
    let pages = loop {
        let event = events
            .recv_timeout(deadline.saturating_duration_since(Instant::now()))
            .expect("worker should open the fixture");
        match event {
            WorkerEvent::Opened {
                generation: opened_generation,
                pages,
                toc,
                links,
                ..
            } => {
                assert_eq!(opened_generation, generation);
                assert_eq!(pages.len(), 3);
                assert_eq!(toc.len(), 4);
                assert_eq!(links.len(), 2);
                break pages;
            }
            WorkerEvent::Error { message, .. } => panic!("fixture open failed: {message}"),
            _ => {}
        }
    };

    let raster = RasterSize {
        width: pages[0].width.round() as u32,
        height: pages[0].height.round() as u32,
    };
    let rect = PixelRect {
        x: 0,
        y: 0,
        width: 256,
        height: 256,
    };
    let tile = TileRequest {
        key: TileKey {
            page: 0,
            raster,
            column: 0,
            row: 0,
        },
        core_rect: rect,
        render_rect: rect,
    };
    assert!(worker.render_viewport(generation, RenderAppearance::Normal, &[tile], 1, &[0]));

    let mut rendered = false;
    let mut extracted = false;
    let deadline = Instant::now() + Duration::from_secs(10);
    while !(rendered && extracted) {
        let event = events
            .recv_timeout(deadline.saturating_duration_since(Instant::now()))
            .expect("worker should render and extract text");
        match event {
            WorkerEvent::TileRendered {
                generation: event_generation,
                key,
                width,
                height,
                bgra,
                ..
            } if key == tile.key => {
                assert_eq!(event_generation, generation);
                assert_eq!((width, height), (256, 256));
                assert_eq!(bgra.len(), 256 * 256 * 4);
                rendered = true;
            }
            WorkerEvent::TextExtracted {
                generation: event_generation,
                page: 0,
                text,
            } => {
                assert_eq!(event_generation, generation);
                let content: String = text.iter().map(|character| character.value).collect();
                assert!(content.contains("GPUI PDF Reader"));
                extracted = true;
            }
            WorkerEvent::TileFailed { message, .. }
            | WorkerEvent::TextFailed { message, .. }
            | WorkerEvent::Error { message, .. } => {
                panic!("worker operation failed: {message}")
            }
            _ => {}
        }
    }

    assert!(worker.render_preview(
        generation,
        3,
        RenderAppearance::Normal,
        PreviewSpec {
            page: 0,
            raster,
            center_x: 0.5,
            center_y: 0.5,
        },
    ));
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        match events
            .recv_timeout(deadline.saturating_duration_since(Instant::now()))
            .expect("worker should render the preview")
        {
            WorkerEvent::PreviewRendered {
                generation: event_generation,
                revision: 3,
                width,
                height,
                bgra,
                ..
            } => {
                assert_eq!(event_generation, generation);
                assert_eq!((width, height), (360, 204));
                assert_eq!(bgra.len(), 360 * 204 * 4);
                break;
            }
            WorkerEvent::PreviewFailed { message, .. } | WorkerEvent::Error { message, .. } => {
                panic!("preview failed: {message}")
            }
            _ => {}
        }
    }

    assert!(worker.search(generation, 7, SearchQuery::new("page").unwrap(),));
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut page_results = 0;
    loop {
        match events
            .recv_timeout(deadline.saturating_duration_since(Instant::now()))
            .expect("worker should finish the search")
        {
            WorkerEvent::SearchPageResults {
                generation: event_generation,
                revision: 7,
                results,
            } => {
                assert_eq!(event_generation, generation);
                page_results += results.matches.len();
            }
            WorkerEvent::SearchFinished {
                generation: event_generation,
                revision: 7,
                searched_pages,
                total_results,
                ..
            } => {
                assert_eq!(event_generation, generation);
                assert_eq!(searched_pages, 3);
                assert_eq!(total_results, page_results);
                assert!(total_results > 0);
                break;
            }
            WorkerEvent::SearchFailed { message, .. } | WorkerEvent::Error { message, .. } => {
                panic!("search failed: {message}")
            }
            _ => {}
        }
    }
}
