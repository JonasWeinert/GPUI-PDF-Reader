use crate::annotations::{
    AnnotationId, AnnotationSet, DocumentIdentity, HighlightColor, MAX_TEXT_CHARACTER_INDEX,
    TextRange, load_sidecar, save_sidecar,
};
use crate::backend::{PdfWorker, RenderAppearance, RenderColor, TileRequest, WorkerEvent};
use crate::comment_editor::{CommentEditor, CommentEditorEvent, RichTextBuffer};
#[cfg(test)]
use crate::model::TextChar;
use crate::model::{
    DocumentLayout, PageAnchor, PageSize, PixelRect, RasterSize, Rect, TextBounds, TextLayer,
    TextPosition, TextSelection, TileKey, TocEntry, append_selected_page_text,
};
use crate::search::{
    MAX_SEARCH_QUERY_BYTES, SearchMatch, SearchMatchId, SearchPageOutcome, SearchQuery, search_page,
};
use crate::text_field::{TextField, TextFieldEvent};
use crate::theme::{self, ReaderPalette, ThemePreference};
use crate::{
    ActualSize, AddComment, ClassicView, CopySelection, EditCopy, EditCut, EditPaste,
    EditSelectAll, Find, FirstPage, FitWidth, FluidView, LastPage, NextSearchResult, OpenDocument,
    PageDown, PageUp, PreviousSearchResult, Quit, ScrollDown, ScrollLeft, ScrollRight, ScrollUp,
    SelectAll, SelectTheme, ToggleComments, ZoomIn, ZoomOut,
};
use gpui::{
    App, Bounds, ClickEvent, ClipboardItem, ContentMask, Context, Corners, CursorStyle, Entity,
    FocusHandle, Focusable, FontWeight, IntoElement, MouseButton, MouseDownEvent, MouseMoveEvent,
    MouseUpEvent, PathPromptOptions, Pixels, Point, PromptButton, PromptLevel, Render, RenderImage,
    ScrollStrategy, ScrollWheelEvent, SharedString, Task, UniformListScrollHandle, Window,
    WindowControlArea, canvas, div, point, prelude::*, px, quad, size, uniform_list,
};
#[cfg(debug_assertions)]
use gpui::{Keystroke, Modifiers, ScrollDelta, TouchPhase};
use gpui_component::{Icon, IconName, Theme};
use image::{Frame, RgbaImage};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs::File;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, mpsc};
use std::thread;
use std::time::{Duration, Instant};

const TOOLBAR_HEIGHT: f32 = 52.0;
const ERROR_BAR_HEIGHT: f32 = 34.0;
const MIN_ZOOM: f32 = 0.2;
const MAX_ZOOM: f32 = 5.0;
const RENDER_QUANTUM: u32 = 64;
const TILE_SIZE: u32 = 1_024;
// PDFium may cull rotated glyphs close to a clipped bitmap edge. A 32-pixel
// gutter eliminated the visible/culling differences across real 1024px
// boundaries in 0°, 90°, and CropBox fixtures (only <=1-channel rounding
// differences remain); only the core is displayed.
const TILE_BLEED: u32 = 32;
const MAX_RASTER_DIMENSION: u32 = 65_536;
const MAX_CACHED_TILES: usize = 48;
const MAX_CACHE_BYTES: usize = 128 * 1024 * 1024;
const MAX_CACHED_TEXT_PAGES: usize = 16;
const MAX_COPY_TEXT_BYTES: usize = 64 * 1024 * 1024;
const MAX_VISIBLE_SEARCH_HIGHLIGHT_RUNS: usize = 4_000;
const MAX_VISIBLE_ANNOTATION_QUADS: usize = 8_000;
const MAX_VISIBLE_SELECTION_QUADS: usize = 8_000;
const ZOOM_RENDER_DEBOUNCE: Duration = Duration::from_millis(150);
const SEARCH_DEBOUNCE: Duration = Duration::from_millis(180);
const COMMENT_AUTOSAVE_DEBOUNCE: Duration = Duration::from_millis(500);
const SIDEBAR_WIDTH: f32 = 344.0;
const MIN_DOCUMENT_VIEWPORT_WIDTH: f32 = 300.0;
const FLUID_PANEL_HORIZONTAL_MARGIN: f32 = 12.0;
const FLUID_PANEL_VERTICAL_MARGIN: f32 = 18.0;
const FLUID_CONTEXT_PILL_WIDTH: f32 = 214.0;
const FLUID_CONTEXT_PILL_HEIGHT: f32 = 40.0;
const TOC_RAIL_WIDTH: f32 = 54.0;
const TOC_MARKER_LEFT: f32 = 8.0;
const TOC_STACK_MARGIN: f32 = 22.0;
const TOC_STACK_SPACING: f32 = 12.0;
const TOC_CASCADE_RADIUS: f32 = 5.0;
const TOC_CARD_HEIGHT: f32 = 82.0;
const TOC_DESTINATION_CONTEXT: f32 = 42.0;
const MAX_TOC_HEADING_MATCHES: usize = 16;

fn render_appearance_from_theme(theme: &Theme, pdf_dark_mode_enabled: bool) -> RenderAppearance {
    if !theme.is_dark() || !pdf_dark_mode_enabled {
        return RenderAppearance::Normal;
    }

    let to_render_color = |color| {
        let color = gpui::Rgba::from(color);
        let channel = |value: f32| (value.clamp(0.0, 1.0) * 255.0).round() as u8;
        RenderColor {
            red: channel(color.r),
            green: channel(color.g),
            blue: channel(color.b),
        }
    };
    RenderAppearance::ForcedColors {
        background: to_render_color(theme::pdf_paper_color(theme, true)),
        foreground: to_render_color(theme.foreground),
    }
}

#[cfg(target_os = "macos")]
const TITLEBAR_CONTROL_INSET: f32 = 76.0;
#[cfg(not(target_os = "macos"))]
const TITLEBAR_CONTROL_INSET: f32 = 0.0;

#[derive(Clone, Copy, Debug, Default)]
struct Offset {
    x: f32,
    y: f32,
}

#[derive(Clone, Copy, Debug)]
struct PaintBudget {
    remaining: usize,
}

impl PaintBudget {
    fn new(limit: usize) -> Self {
        Self { remaining: limit }
    }

    fn take(&mut self) -> bool {
        let Some(remaining) = self.remaining.checked_sub(1) else {
            return false;
        };
        self.remaining = remaining;
        true
    }

    fn exhausted(self) -> bool {
        self.remaining == 0
    }
}

fn is_inactive<T: Copy + PartialEq>(candidate: T, active: Option<T>) -> bool {
    Some(candidate) != active
}

fn annotation_actions_enabled(
    has_context: bool,
    annotations_loading: bool,
    persistence_blocked: bool,
    comment_editor_open: bool,
) -> bool {
    has_context && !annotations_loading && !persistence_blocked && !comment_editor_open
}

fn zoom_controls_enabled(document_open: bool, zoom: f32) -> (bool, bool) {
    (
        document_open && zoom > MIN_ZOOM + 0.001,
        document_open && zoom < MAX_ZOOM - 0.001,
    )
}

fn comment_draft_needs_confirmation(editor_open: bool, draft_dirty: bool) -> bool {
    editor_open && draft_dirty
}

fn comments_toolbar_label(
    editor_open: bool,
    compact_toolbar: bool,
    very_compact_toolbar: bool,
) -> &'static str {
    if editor_open {
        if compact_toolbar {
            "Notes •"
        } else {
            "Comments · Editing"
        }
    } else if very_compact_toolbar {
        "Notes"
    } else {
        "Comments"
    }
}

fn floating_pill_position(
    anchor: Rect,
    available_width: f32,
    viewport_height: f32,
    pill_width: f32,
    pill_height: f32,
) -> Offset {
    let margin = 12.0;
    let maximum_x = (available_width - pill_width - margin).max(margin);
    let x = (anchor.x + anchor.width * 0.5 - pill_width * 0.5).clamp(margin, maximum_x);
    let below = anchor.bottom() + 10.0;
    let y = if below + pill_height <= viewport_height - margin {
        below
    } else {
        (anchor.y - pill_height - 10.0).max(margin)
    };
    Offset { x, y }
}

fn toc_scroll_target(
    layout: &DocumentLayout,
    page: usize,
    destination_y: Option<f32>,
    viewport_height: f32,
) -> Option<f32> {
    let page = layout.page_rect(page)?;
    let maximum = (layout.content_height - viewport_height).max(0.0);
    let destination = destination_y
        .filter(|value| value.is_finite())
        .map_or(page.y, |value| {
            page.y + page.height * value.clamp(0.0, 1.0) - TOC_DESTINATION_CONTEXT
        });
    Some(destination.clamp(0.0, maximum))
}

fn toc_title_match_y(title: &str, text: &TextLayer) -> Option<f32> {
    let query = SearchQuery::new(title).ok()?;
    let SearchPageOutcome::Complete(results) =
        search_page(0, text.as_slice(), &query, MAX_TOC_HEADING_MATCHES, || {
            false
        })
    else {
        return None;
    };
    let mut best: Option<(f32, f32)> = None;
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
        if best.is_none_or(|(best_height, best_top)| {
            height > best_height + 0.0001
                || ((height - best_height).abs() <= 0.0001 && top < best_top)
        }) {
            best = Some((height, top));
        }
    }
    best.map(|(_, top)| top.clamp(0.0, 1.0))
}

fn toc_stack_geometry(viewport_height: f32, count: usize) -> Option<(f32, f32)> {
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

fn toc_cascade_amount(index: usize, hover_position: f32, hover_strength: f32) -> f32 {
    if !hover_position.is_finite() || !hover_strength.is_finite() {
        return 0.0;
    }
    let distance = (index as f32 - hover_position).abs();
    (1.0 - distance / TOC_CASCADE_RADIUS).clamp(0.0, 1.0) * hover_strength.clamp(0.0, 1.0)
}

fn toc_hover_state_is_animating(position: f32, strength: f32, target: Option<usize>) -> bool {
    let target_strength = if target.is_some() { 1.0 } else { 0.0 };
    let strength_is_animating = (target_strength - strength).abs() > 0.002;
    let position_is_animating = target.is_some_and(|index| (index as f32 - position).abs() > 0.002);
    strength_is_animating || position_is_animating
}

fn advance_toc_hover_state(
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

fn active_toc_index(entries: &[TocEntry], current_page: usize) -> Option<usize> {
    entries
        .iter()
        .enumerate()
        .rev()
        .find_map(|(index, entry)| (entry.page <= current_page).then_some(index))
        .or((!entries.is_empty()).then_some(0))
}

fn toc_breadcrumb(entries: &[TocEntry], index: usize) -> Option<String> {
    let current = entries.get(index)?;
    let mut path = vec![current.title.as_str()];
    let mut expected_depth = current.depth;
    for entry in entries[..index].iter().rev() {
        let Some(parent_depth) = expected_depth.checked_sub(1) else {
            break;
        };
        if entry.depth == parent_depth {
            path.push(entry.title.as_str());
            expected_depth = parent_depth;
        }
    }
    path.reverse();
    Some(format!("Document  ›  {}", path.join("  ›  ")))
}

#[derive(Debug)]
struct DocumentState {
    path: PathBuf,
    pages: Vec<PageSize>,
    toc: Vec<TocEntry>,
}

#[derive(Clone)]
struct CachedTile {
    core_rect: PixelRect,
    render_rect: PixelRect,
    byte_len: usize,
    image: Arc<RenderImage>,
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
enum DemandTier {
    Visible,
    Prefetch,
}

#[derive(Clone, Copy, Debug)]
struct PlannedTile {
    request: TileRequest,
    tier: DemandTier,
    distance: u64,
}

#[derive(Clone, Copy, Debug)]
struct PanState {
    pointer: Point<Pixels>,
    scroll: Offset,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum SidePanel {
    Comments,
    Search,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
enum ReaderView {
    #[default]
    Classic,
    Fluid,
}

struct FluidPillState {
    available_width: f32,
    document_open: bool,
    zoom_out_enabled: bool,
    zoom_in_enabled: bool,
    zoom_label: SharedString,
    search_selected: bool,
    comments_selected: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ChromeButtonStyle {
    Neutral,
    Ghost,
    Floating,
    Selected,
    Primary,
}

#[derive(Clone, Debug)]
enum DraftDiscardAction {
    Open(PathBuf),
    Quit,
    CloseWindow,
}

#[cfg(debug_assertions)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum QaFeaturePhase {
    Seed,
    WaitCommentEditor,
    WaitCommentEdited,
    WaitCommentSaved,
    WaitCommentBack,
    WaitCommentList,
    WaitCommentsOpen,
    WaitCommentsClosed,
    WaitSearchOpen,
    WaitSearch,
    WaitNavigation,
    WaitSearchReturn,
    WaitSearchClosed,
    WaitSearchReopened,
    WaitFinalNavigation,
    Complete,
}

#[cfg(debug_assertions)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum QaFluidPhase {
    Seed,
    WaitEditor,
    WaitAutosave,
    WaitList,
    WaitReopenedEditor,
    WaitFinalList,
    WaitSearchOpen,
    WaitSearchResults,
    Complete,
}

#[derive(Clone, Copy, Debug)]
struct SidebarState {
    panel: SidePanel,
    progress: f32,
    target: f32,
}

impl Default for SidebarState {
    fn default() -> Self {
        Self {
            panel: SidePanel::Comments,
            progress: 0.0,
            target: 0.0,
        }
    }
}

impl SidebarState {
    fn toggle(&mut self, panel: SidePanel) {
        if self.panel == panel && self.target > 0.5 {
            self.target = 0.0;
        } else {
            self.panel = panel;
            self.target = 1.0;
        }
    }

    fn is_animating(self) -> bool {
        (self.progress - self.target).abs() > 0.001
    }

    fn advance(&mut self, dt: f32) {
        let blend = 1.0 - (-22.0 * dt.clamp(1.0 / 240.0, 0.05)).exp();
        self.progress += (self.target - self.progress) * blend;
        if (self.target - self.progress).abs() < 0.001 {
            self.progress = self.target;
        }
        self.progress = self.progress.clamp(0.0, 1.0);
    }

    fn available_width(self, full_width: f32) -> f32 {
        let maximum_sidebar = (full_width - MIN_DOCUMENT_VIEWPORT_WIDTH).max(0.0);
        SIDEBAR_WIDTH.min(maximum_sidebar) * self.progress
    }
}

#[derive(Clone, Copy, Debug, Default)]
struct CommentPaneState {
    progress: f32,
    target: f32,
    close_editor_on_finish: bool,
}

impl CommentPaneState {
    fn show_editor(&mut self, animated: bool) {
        if !animated {
            self.progress = 1.0;
        }
        self.target = 1.0;
        self.close_editor_on_finish = false;
    }

    fn show_list(&mut self, animated: bool) {
        if !animated {
            self.progress = 0.0;
        }
        self.target = 0.0;
        self.close_editor_on_finish = true;
    }

    fn is_animating(self) -> bool {
        (self.progress - self.target).abs() > 0.001
    }

    fn advance(&mut self, dt: f32) {
        let blend = 1.0 - (-24.0 * dt.clamp(1.0 / 240.0, 0.05)).exp();
        self.progress += (self.target - self.progress) * blend;
        if (self.target - self.progress).abs() < 0.001 {
            self.progress = self.target;
        }
        self.progress = self.progress.clamp(0.0, 1.0);
    }
}

#[derive(Debug)]
struct PendingCopy {
    selection: TextSelection,
    next_page: usize,
    end_page: usize,
    text: String,
}

#[derive(Debug, Default)]
struct SearchState {
    query: String,
    input_error: Option<SharedString>,
    revision: u64,
    pages: BTreeMap<usize, Arc<[SearchMatch]>>,
    order: Vec<SearchMatchId>,
    active: Option<SearchMatchId>,
    searched_pages: usize,
    total_highlight_runs: usize,
    complete: bool,
    truncated: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum AnnotationIoOperation {
    Load,
    Save,
}

enum AnnotationIoCommand {
    Load {
        generation: u64,
        path: PathBuf,
        page_count: usize,
    },
    Save {
        generation: u64,
        path: PathBuf,
        identity: DocumentIdentity,
        expected_disk_revision: u64,
        annotations: AnnotationSet,
    },
}

enum AnnotationIoEvent {
    Loaded {
        generation: u64,
        identity: DocumentIdentity,
        annotations: AnnotationSet,
    },
    Saved {
        generation: u64,
        revision: u64,
    },
    Failed {
        generation: u64,
        operation: AnnotationIoOperation,
        revision: Option<u64>,
        message: String,
    },
}

struct AnnotationIo {
    commands: Option<mpsc::Sender<AnnotationIoCommand>>,
    thread: Option<thread::JoinHandle<()>>,
}

impl AnnotationIo {
    fn start() -> (Self, mpsc::Receiver<AnnotationIoEvent>) {
        let (command_tx, command_rx) = mpsc::channel();
        let (event_tx, event_rx) = mpsc::channel();
        let thread = thread::Builder::new()
            .name("annotation-sidecar".into())
            .spawn(move || {
                let mut deferred = None;
                let mut observed_disk_revisions = HashMap::<(u64, PathBuf), u64>::new();
                loop {
                    let command = match deferred.take() {
                        Some(command) => command,
                        None => match command_rx.recv() {
                            Ok(command) => command,
                            Err(_) => break,
                        },
                    };
                    match command {
                        AnnotationIoCommand::Load {
                            generation,
                            path,
                            page_count,
                        } => match DocumentIdentity::from_pdf(&path, page_count).and_then(
                            |identity| {
                                load_sidecar(&path, &identity)
                                    .map(|annotations| (identity, annotations))
                            },
                        ) {
                            Ok((identity, annotations)) => {
                                // A load is an ordering boundary for the sole
                                // current document; older generations can no
                                // longer save and must not accumulate paths.
                                observed_disk_revisions.clear();
                                observed_disk_revisions.insert(
                                    (generation, path.clone()),
                                    annotations.revision(),
                                );
                                let _ = event_tx.send(AnnotationIoEvent::Loaded {
                                    generation,
                                    identity,
                                    annotations,
                                });
                            }
                            Err(error) => {
                                observed_disk_revisions.clear();
                                let _ = event_tx.send(AnnotationIoEvent::Failed {
                                    generation,
                                    operation: AnnotationIoOperation::Load,
                                    revision: None,
                                    message: error.to_string(),
                                });
                            }
                        },
                        AnnotationIoCommand::Save {
                            generation,
                            path,
                            mut identity,
                            expected_disk_revision,
                            mut annotations,
                        } => {
                            // Saving a sidecar snapshot is much slower than a
                            // color click. Collapse a queued same-document
                            // burst to its newest revision while preserving a
                            // load or another document's ordering boundary. The
                            // first command's expected disk revision remains the
                            // base for the whole coalesced burst.
                            while let Ok(command) = command_rx.try_recv() {
                                match command {
                                    AnnotationIoCommand::Save {
                                        generation: next_generation,
                                        path: next_path,
                                        identity: next_identity,
                                        expected_disk_revision: _,
                                        annotations: next_annotations,
                                    } if next_generation == generation && next_path == path => {
                                        identity = next_identity;
                                        annotations = next_annotations;
                                    }
                                    command => {
                                        deferred = Some(command);
                                        break;
                                    }
                                }
                            }
                            let revision = annotations.revision();
                            let revision_key = (generation, path.clone());
                            let expected_disk_revision = observed_disk_revisions
                                .get(&revision_key)
                                .copied()
                                .unwrap_or(expected_disk_revision);
                            let save_result: Result<(), String> = (|| {
                                // Serialize the compare-and-replace section
                                // across app processes. The lock is advisory,
                                // crash-safe, and attached to the PDF itself,
                                // so no stale lock file is left beside it.
                                let pdf_lock = File::open(&path)
                                    .map_err(|error| format!("could not lock PDF: {error}"))?;
                                pdf_lock.lock().map_err(|error| {
                                    format!("could not lock PDF for annotation save: {error}")
                                })?;
                                let current_identity =
                                    DocumentIdentity::from_pdf(&path, identity.page_count())
                                        .map_err(|error| error.to_string())?;
                                if current_identity != identity {
                                    return Err(
                                        crate::annotations::AnnotationError::DocumentIdentityMismatch {
                                            expected: identity.clone(),
                                            found: current_identity,
                                        }
                                        .to_string(),
                                    );
                                }

                                let disk_revision = load_sidecar(&path, &identity)
                                    .map_err(|error| error.to_string())?
                                    .revision();
                                if disk_revision != expected_disk_revision {
                                    return Err(format!(
                                        "annotation sidecar changed on disk (expected revision {expected_disk_revision}, found revision {disk_revision}); reload the PDF before saving"
                                    ));
                                }

                                save_sidecar(&path, &identity, &annotations)
                                    .map(|_| ())
                                    .map_err(|error| error.to_string())
                            })();
                            match save_result {
                                Ok(_) => {
                                    observed_disk_revisions.insert(revision_key, revision);
                                    let _ = event_tx.send(AnnotationIoEvent::Saved {
                                        generation,
                                        revision,
                                    });
                                }
                                Err(error) => {
                                    let _ = event_tx.send(AnnotationIoEvent::Failed {
                                        generation,
                                        operation: AnnotationIoOperation::Save,
                                        revision: Some(revision),
                                        message: error.to_string(),
                                    });
                                }
                            }
                        }
                    }
                }
            })
            .expect("failed to start the annotation sidecar thread");
        (
            Self {
                commands: Some(command_tx),
                thread: Some(thread),
            },
            event_rx,
        )
    }

    fn load(&self, generation: u64, path: PathBuf, page_count: usize) -> bool {
        self.commands.as_ref().is_some_and(|commands| {
            commands
                .send(AnnotationIoCommand::Load {
                    generation,
                    path,
                    page_count,
                })
                .is_ok()
        })
    }

    fn save(
        &self,
        generation: u64,
        path: PathBuf,
        identity: DocumentIdentity,
        expected_disk_revision: u64,
        annotations: AnnotationSet,
    ) -> bool {
        self.commands.as_ref().is_some_and(|commands| {
            commands
                .send(AnnotationIoCommand::Save {
                    generation,
                    path,
                    identity,
                    expected_disk_revision,
                    annotations,
                })
                .is_ok()
        })
    }
}

impl Drop for AnnotationIo {
    fn drop(&mut self) {
        self.commands.take();
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

fn next_search_match_id(
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

fn unsaved_annotation_revision(current: Option<u64>, saved: u64) -> Option<u64> {
    current.filter(|revision| *revision > saved)
}

#[derive(Debug, Eq, PartialEq)]
enum PendingOpenTransition {
    None,
    Waiting,
    Open(PathBuf),
    Cancelled(PathBuf),
}

fn transition_pending_open(
    pending: &mut Option<PathBuf>,
    current_revision: Option<u64>,
    saved_revision: u64,
    save_failed: bool,
) -> PendingOpenTransition {
    if pending.is_none() {
        return PendingOpenTransition::None;
    }
    if save_failed {
        return PendingOpenTransition::Cancelled(
            pending.take().expect("pending path checked above"),
        );
    }
    if unsaved_annotation_revision(current_revision, saved_revision).is_some() {
        PendingOpenTransition::Waiting
    } else {
        PendingOpenTransition::Open(pending.take().expect("pending path checked above"))
    }
}

#[derive(Clone, Debug)]
enum ReaderStatus {
    Initializing,
    Empty,
    Loading(SharedString),
    Ready,
    Error(SharedString),
}

pub struct PdfReader {
    worker: PdfWorker,
    annotation_io: AnnotationIo,
    generation: u64,
    document: Option<DocumentState>,
    layout: Option<Arc<DocumentLayout>>,
    rendered: HashMap<TileKey, CachedTile>,
    page_text: HashMap<usize, Arc<TextLayer>>,
    pending: HashSet<TileKey>,
    render_viewport: Vec<(TileKey, DemandTier)>,
    text_viewport: Vec<usize>,
    text_pending: HashSet<usize>,
    copy_pending: Option<PendingCopy>,
    search: SearchState,
    search_debounce_task: Option<Task<()>>,
    annotations: Option<AnnotationSet>,
    annotation_identity: Option<DocumentIdentity>,
    annotations_loading: bool,
    annotation_persistence_blocked: bool,
    annotation_error: Option<SharedString>,
    annotation_enqueued_revision: u64,
    annotation_failed_revision: Option<u64>,
    annotation_saved_revision: u64,
    pending_open: Option<PathBuf>,
    active_annotation: Option<AnnotationId>,
    search_field: Entity<TextField>,
    comment_editor: Option<Entity<CommentEditor>>,
    comment_draft_dirty: bool,
    comment_discard_prompt_open: bool,
    comment_pane: CommentPaneState,
    comment_autosave_revision: u64,
    comment_autosave_task: Option<Task<()>>,
    editing_annotation: Option<AnnotationId>,
    pending_comment_range: Option<TextRange>,
    comment_order: Vec<AnnotationId>,
    search_list_scroll: UniformListScrollHandle,
    comment_list_scroll: UniformListScrollHandle,
    status: ReaderStatus,
    view_mode: ReaderView,
    theme_preference: ThemePreference,
    selected_theme: Option<SharedString>,
    render_appearance: RenderAppearance,
    pdf_dark_mode_enabled: bool,
    warning: Option<SharedString>,
    focus_handle: FocusHandle,
    zoom: f32,
    fit_width: bool,
    scroll: Offset,
    scroll_target: Offset,
    viewport_width: f32,
    viewport_height: f32,
    selection: Option<TextSelection>,
    selecting: bool,
    pending_annotation_click: Option<AnnotationId>,
    toc_hovered: Option<usize>,
    toc_hover_position: f32,
    toc_hover_strength: f32,
    pending_toc_navigation: Option<usize>,
    pan: Option<PanState>,
    animation_active: bool,
    animation_frame_queued: bool,
    last_animation_tick: Instant,
    sidebar: SidebarState,
    sidebar_anchor: Option<PageAnchor>,
    #[cfg(debug_assertions)]
    qa_feature_phase: QaFeaturePhase,
    #[cfg(debug_assertions)]
    qa_fluid_phase: QaFluidPhase,
    #[cfg(debug_assertions)]
    qa_sidebar_anchor_reference: Option<PageAnchor>,
    #[cfg(debug_assertions)]
    qa_sidebar_transitions: usize,
    #[cfg(debug_assertions)]
    qa_max_sidebar_anchor_error: f32,
    #[cfg(debug_assertions)]
    qa_toc_text_matches: usize,
    zoom_render_revision: u64,
    render_debounce_until: Option<Instant>,
    zoom_render_task: Option<Task<()>>,
}

impl PdfReader {
    pub fn new(initial_path: Option<PathBuf>, window: &mut Window, cx: &mut App) -> Entity<Self> {
        let (worker, events) = PdfWorker::start();
        let (annotation_io, annotation_events) = AnnotationIo::start();
        let search_field =
            cx.new(|cx| TextField::new(cx, "Search document", MAX_SEARCH_QUERY_BYTES));
        let search_field_for_reader = search_field.clone();
        let entity: Entity<Self> = cx.new(|cx: &mut Context<Self>| {
            cx.subscribe_in(
                &search_field_for_reader,
                window,
                |reader, _, event, window, cx| match event {
                    TextFieldEvent::Changed(query) => reader.set_search_query(query.clone(), cx),
                    TextFieldEvent::Rejected(rejection) => {
                        reader.search.input_error = Some(rejection.to_string().into());
                        cx.notify();
                    }
                    TextFieldEvent::Submit => reader.navigate_search(true, window, cx),
                    TextFieldEvent::Cancel => reader.toggle_sidebar(SidePanel::Search, window, cx),
                },
            )
            .detach();
            Self {
                worker,
                annotation_io,
                generation: 0,
                document: None,
                layout: None,
                rendered: HashMap::new(),
                page_text: HashMap::new(),
                pending: HashSet::new(),
                render_viewport: Vec::new(),
                text_viewport: Vec::new(),
                text_pending: HashSet::new(),
                copy_pending: None,
                search: SearchState::default(),
                search_debounce_task: None,
                annotations: None,
                annotation_identity: None,
                annotations_loading: false,
                annotation_persistence_blocked: false,
                annotation_error: None,
                annotation_enqueued_revision: 0,
                annotation_failed_revision: None,
                annotation_saved_revision: 0,
                pending_open: None,
                active_annotation: None,
                search_field,
                comment_editor: None,
                comment_draft_dirty: false,
                comment_discard_prompt_open: false,
                comment_pane: CommentPaneState::default(),
                comment_autosave_revision: 0,
                comment_autosave_task: None,
                editing_annotation: None,
                pending_comment_range: None,
                comment_order: Vec::new(),
                search_list_scroll: UniformListScrollHandle::new(),
                comment_list_scroll: UniformListScrollHandle::new(),
                status: ReaderStatus::Initializing,
                view_mode: ReaderView::Classic,
                theme_preference: ThemePreference::System,
                selected_theme: None,
                render_appearance: render_appearance_from_theme(Theme::global(cx), true),
                pdf_dark_mode_enabled: true,
                warning: None,
                focus_handle: cx.focus_handle(),
                zoom: 1.0,
                fit_width: false,
                scroll: Offset::default(),
                scroll_target: Offset::default(),
                viewport_width: 1.0,
                viewport_height: 1.0,
                selection: None,
                selecting: false,
                pending_annotation_click: None,
                toc_hovered: None,
                toc_hover_position: 0.0,
                toc_hover_strength: 0.0,
                pending_toc_navigation: None,
                pan: None,
                animation_active: false,
                animation_frame_queued: false,
                last_animation_tick: Instant::now(),
                sidebar: SidebarState::default(),
                sidebar_anchor: None,
                #[cfg(debug_assertions)]
                qa_feature_phase: QaFeaturePhase::Seed,
                #[cfg(debug_assertions)]
                qa_fluid_phase: QaFluidPhase::Seed,
                #[cfg(debug_assertions)]
                qa_sidebar_anchor_reference: None,
                #[cfg(debug_assertions)]
                qa_sidebar_transitions: 0,
                #[cfg(debug_assertions)]
                qa_max_sidebar_anchor_error: 0.0,
                #[cfg(debug_assertions)]
                qa_toc_text_matches: 0,
                zoom_render_revision: 0,
                render_debounce_until: None,
                zoom_render_task: None,
            }
        });

        Self::listen_for_worker_events(&entity, events, window, cx);
        Self::listen_for_annotation_events(&entity, annotation_events, window, cx);
        Self::listen_for_native_pinch(&entity, window, cx);
        entity.update(cx, |_, cx| {
            cx.observe_window_appearance(window, |reader, window, cx| {
                if reader.theme_preference == ThemePreference::System {
                    Theme::sync_system_appearance(Some(window), cx);
                    reader.update_render_appearance(window, cx);
                }
            })
            .detach();
        });
        let weak = entity.downgrade();
        window.on_window_should_close(cx, move |window, cx| {
            weak.update(cx, |reader, cx| reader.should_close_window(window, cx))
                .unwrap_or(true)
        });

        if let Some(path) = initial_path {
            entity.update(cx, |reader, cx| reader.open_path(path, window, cx));
        }
        entity
    }

    pub fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }

    #[cfg(debug_assertions)]
    pub fn qa_report(&self) -> String {
        let cached_bytes = self
            .rendered
            .values()
            .map(|tile| tile.byte_len)
            .sum::<usize>();
        let visible_exact: Vec<_> = self
            .render_viewport
            .iter()
            .filter_map(|(key, tier)| (*tier == DemandTier::Visible).then_some(*key))
            .collect();
        let exact_cached = visible_exact
            .iter()
            .filter(|key| self.rendered.contains_key(key))
            .count();
        let visible_pages = visible_exact
            .iter()
            .map(|key| key.page)
            .collect::<HashSet<_>>()
            .len();
        let max_tile_bytes = self
            .rendered
            .values()
            .map(|tile| tile.byte_len)
            .max()
            .unwrap_or(0);
        let mut highlight_colors = HashSet::new();
        let mut highlight_count = 0;
        let mut comment_count = 0;
        if let Some(annotations) = self.annotations.as_ref() {
            for annotation in annotations.iter() {
                if let Some(color) = annotation.highlight() {
                    highlight_count += 1;
                    highlight_colors.insert(color);
                }
                comment_count += usize::from(annotation.comment_markdown().is_some());
            }
        }
        let active_search = self
            .search
            .active
            .and_then(|active| self.search.order.iter().position(|id| *id == active))
            .map_or(0, |index| index + 1);
        let theme_name = self
            .selected_theme
            .as_ref()
            .map(|name| name.as_ref())
            .unwrap_or_else(|| self.theme_preference.name());
        format!(
            "GPUI_PDF_READER_QA view={:?} theme={} pdf_render={} pdf_dark_enabled={} toc={} toc_hover={} toc_hover_strength={:.3} toc_text_matches={} zoom={:.3} cached_tiles={} cached_bytes={} max_tile_bytes={} cached_text_pages={} text_desired={} pending={} desired={} visible_exact={}/{} visible_pages={} debouncing={} scroll=({:.2},{:.2}) sidebar={:.3}/{:.0} comment_pane={:.3}/{:.0} comment_editor={} comment_dirty={} autosave_pending={} sidebar_transitions={} sidebar_anchor_error={:.6} annotations={} highlights={} highlight_colors={} comments={} annotation_revision={}/{}/{} annotation_loading={} annotation_blocked={} search_results={} search_pages={} search_highlight_runs={} active_search={} search_complete={} status={:?}",
            self.view_mode,
            theme_name,
            if matches!(
                self.render_appearance,
                RenderAppearance::ForcedColors { .. }
            ) {
                "forced"
            } else {
                "normal"
            },
            u8::from(self.pdf_dark_mode_enabled),
            self.document
                .as_ref()
                .map_or(0, |document| document.toc.len()),
            self.toc_hovered.map_or(0, |index| index + 1),
            self.toc_hover_strength,
            self.qa_toc_text_matches,
            self.zoom,
            self.rendered.len(),
            cached_bytes,
            max_tile_bytes,
            self.page_text.len(),
            self.text_viewport.len(),
            self.pending.len(),
            self.render_viewport.len(),
            exact_cached,
            visible_exact.len(),
            visible_pages,
            u8::from(self.render_debounce_until.is_some()),
            self.scroll.x,
            self.scroll.y,
            self.sidebar.progress,
            self.sidebar.target,
            self.comment_pane.progress,
            self.comment_pane.target,
            u8::from(self.comment_editor.is_some()),
            u8::from(self.comment_draft_dirty),
            u8::from(self.comment_autosave_task.is_some()),
            self.qa_sidebar_transitions,
            self.qa_max_sidebar_anchor_error,
            self.annotations.as_ref().map_or(0, AnnotationSet::len),
            highlight_count,
            highlight_colors.len(),
            comment_count,
            self.annotations.as_ref().map_or(0, AnnotationSet::revision),
            self.annotation_enqueued_revision,
            self.annotation_saved_revision,
            u8::from(self.annotations_loading),
            u8::from(self.annotation_persistence_blocked),
            self.search.order.len(),
            self.search.searched_pages,
            self.search.total_highlight_runs,
            active_search,
            u8::from(self.search.complete),
            self.status,
        )
    }

    #[cfg(debug_assertions)]
    pub fn qa_use_fluid_view(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.set_view_mode(ReaderView::Fluid, window, cx);
    }

    #[cfg(debug_assertions)]
    pub fn qa_set_pdf_dark_mode(
        &mut self,
        enabled: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.pdf_dark_mode_enabled = enabled;
        self.update_render_appearance(window, cx);
    }

    #[cfg(debug_assertions)]
    pub fn qa_set_toc_hovered(
        &mut self,
        index: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Result<(), String> {
        let count = self
            .document
            .as_ref()
            .map_or(0, |document| document.toc.len());
        if index >= count {
            return Err(format!(
                "TOC hover index {index} is outside {count} entries"
            ));
        }
        self.set_toc_hovered(index, true, window, cx);
        Ok(())
    }

    #[cfg(debug_assertions)]
    pub fn qa_navigate_toc(
        &mut self,
        index: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Result<(), String> {
        let count = self
            .document
            .as_ref()
            .map_or(0, |document| document.toc.len());
        if index >= count {
            return Err(format!(
                "TOC navigation index {index} is outside {count} entries"
            ));
        }
        self.navigate_toc(index, window, cx);
        Ok(())
    }

    #[cfg(debug_assertions)]
    pub fn qa_select_theme(
        &mut self,
        name: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        if name != "system"
            && !theme::bundled_themes()
                .iter()
                .any(|theme| theme.name == name)
        {
            return false;
        }
        self.select_theme(
            &SelectTheme {
                name: if name == "system" {
                    SharedString::default()
                } else {
                    name.to_owned().into()
                },
            },
            window,
            cx,
        );
        true
    }

    #[cfg(debug_assertions)]
    pub fn qa_viewport_is_settled(&self) -> bool {
        if !matches!(self.status, ReaderStatus::Ready)
            || self.render_debounce_until.is_some()
            || !self.pending.is_empty()
            || self.annotations_loading
            || (!self.annotation_persistence_blocked
                && self.annotations.as_ref().is_some_and(|annotations| {
                    annotations.revision() > self.annotation_saved_revision
                }))
            || (!self.search.query.is_empty()
                && (!self.search.complete || self.search_debounce_task.is_some()))
            || self.sidebar.is_animating()
            || self.comment_pane.is_animating()
            || self.toc_hover_is_animating()
            || self.pending_toc_navigation.is_some()
            || self.comment_autosave_task.is_some()
        {
            return false;
        }
        let mut visible = self
            .render_viewport
            .iter()
            .filter_map(|(key, tier)| (*tier == DemandTier::Visible).then_some(key));
        let Some(first) = visible.next() else {
            return false;
        };
        self.rendered.contains_key(first) && visible.all(|key| self.rendered.contains_key(key))
    }

    #[cfg(debug_assertions)]
    pub fn qa_command_wheel(
        &mut self,
        delta_y: f32,
        position: Point<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.on_scroll_wheel(
            &ScrollWheelEvent {
                position,
                delta: ScrollDelta::Pixels(Point::new(px(0.0), px(delta_y))),
                modifiers: Modifiers {
                    platform: true,
                    ..Default::default()
                },
                touch_phase: TouchPhase::Moved,
            },
            window,
            cx,
        );
    }

    #[cfg(debug_assertions)]
    fn qa_text_range(&self, page: usize, needle: &str) -> Option<TextRange> {
        let characters = self.page_text.get(&page)?.as_slice();
        let needle: Vec<_> = needle.chars().collect();
        if needle.is_empty() || needle.len() > characters.len() {
            return None;
        }
        let start = characters.windows(needle.len()).position(|window| {
            window
                .iter()
                .map(|character| character.value)
                .eq(needle.iter().copied())
        })?;
        Some(TextRange::new(
            TextPosition { page, index: start },
            TextPosition {
                page,
                index: start + needle.len() - 1,
            },
        ))
    }

    #[cfg(debug_assertions)]
    fn qa_defer_keystrokes(
        keys: &[&str],
        window: &Window,
        cx: &mut Context<Self>,
    ) -> Result<(), String> {
        let keystrokes = keys
            .iter()
            .map(|key| {
                Keystroke::parse(key)
                    .map(|keystroke| ((*key).to_owned(), keystroke))
                    .map_err(|error| format!("invalid QA key {key:?}: {error}"))
            })
            .collect::<Result<Vec<_>, _>>()?;
        window.defer(cx, move |window, cx| {
            for (name, keystroke) in keystrokes {
                if !window.dispatch_keystroke(keystroke, cx) {
                    eprintln!("GPUI_PDF_READER_QA_ERROR key {name:?} was not handled");
                    cx.quit();
                    return;
                }
            }
        });
        Ok(())
    }

    #[cfg(debug_assertions)]
    fn qa_seed_annotations_and_comment(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Result<(), String> {
        let annotations = self
            .annotations
            .as_ref()
            .ok_or_else(|| "annotations have not loaded".to_owned())?;
        if annotations.iter().next().is_some() {
            return Err("feature scenario requires a PDF with no existing sidecar".to_owned());
        }
        if self.annotation_persistence_blocked {
            return Err("annotation persistence is blocked".to_owned());
        }

        let needles = ["GPUI", "PDF", "Reader", "integration", "fixture"];
        let ranges = needles
            .iter()
            .map(|needle| {
                self.qa_text_range(0, needle)
                    .ok_or_else(|| format!("fixture text {needle:?} was not extracted"))
            })
            .collect::<Result<Vec<_>, _>>()?;
        for (range, color) in ranges.into_iter().zip(HighlightColor::ALL) {
            self.selection = Some(range.as_selection());
            self.add_highlight(color, cx);
        }

        let comment_range = self
            .qa_text_range(0, "Select this sentence")
            .ok_or_else(|| "fixture comment text was not extracted".to_owned())?;
        self.selection = Some(comment_range.as_selection());
        self.open_comment_editor(comment_range, None, String::new(), window, cx);

        let annotations = self
            .annotations
            .as_ref()
            .ok_or_else(|| "annotations disappeared while seeding".to_owned())?;
        if annotations.len() != 5 || annotations.revision() != 5 {
            return Err(format!(
                "feature seeding produced {} annotations at revision {}, expected 5/5 before the comment save",
                annotations.len(),
                annotations.revision()
            ));
        }
        Ok(())
    }

    #[cfg(debug_assertions)]
    fn qa_validate_feature_scenario(&self) -> Result<(), String> {
        let annotations = self
            .annotations
            .as_ref()
            .ok_or_else(|| "annotations disappeared".to_owned())?;
        let colors: HashSet<_> = annotations
            .iter()
            .filter_map(|annotation| annotation.highlight())
            .collect();
        let highlights = annotations
            .iter()
            .filter(|annotation| annotation.highlight().is_some())
            .count();
        let comments = annotations
            .iter()
            .filter(|annotation| annotation.comment_markdown().is_some())
            .count();
        if annotations.len() != 6
            || highlights != 5
            || colors.len() != HighlightColor::ALL.len()
            || comments != 1
            || self.annotation_saved_revision != annotations.revision()
        {
            return Err(format!(
                "unexpected persisted feature state: annotations={}, highlights={highlights}, colors={}, comments={comments}, revision={}/{},",
                annotations.len(),
                colors.len(),
                annotations.revision(),
                self.annotation_saved_revision
            ));
        }
        if self.search.order.len() < 3
            || !self.search.complete
            || self.search.total_highlight_runs == 0
            || self
                .search
                .active
                .and_then(|active| self.search.order.iter().position(|id| *id == active))
                != Some(1)
        {
            return Err(format!(
                "unexpected search state: results={}, runs={}, active={:?}, complete={}",
                self.search.order.len(),
                self.search.total_highlight_runs,
                self.search.active,
                self.search.complete
            ));
        }
        if self.sidebar.panel != SidePanel::Search
            || self.sidebar.progress != 1.0
            || self.sidebar.target != 1.0
            || self.qa_sidebar_transitions < 4
            || self.qa_max_sidebar_anchor_error > 0.002
        {
            return Err(format!(
                "unexpected sidebar state: panel={:?}, progress={}, target={}, transitions={}, anchor_error={}",
                self.sidebar.panel,
                self.sidebar.progress,
                self.sidebar.target,
                self.qa_sidebar_transitions,
                self.qa_max_sidebar_anchor_error
            ));
        }
        Ok(())
    }

    /// Advances one deterministic native feature scenario without bypassing
    /// production annotation persistence, sidebar animation, or PDFium search.
    #[cfg(debug_assertions)]
    pub fn qa_drive_feature_scenario(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Result<bool, String> {
        match self.qa_feature_phase {
            QaFeaturePhase::Seed => {
                if !self.qa_viewport_is_settled()
                    || self.annotations_loading
                    || !self.page_text.contains_key(&0)
                {
                    return Ok(false);
                }
                self.qa_seed_annotations_and_comment(window, cx)?;
                self.qa_feature_phase = QaFeaturePhase::WaitCommentEditor;
            }
            QaFeaturePhase::WaitCommentEditor => {
                let Some(editor) = self.comment_editor.clone() else {
                    return Err("comment editor disappeared before its native frame".to_owned());
                };
                if editor.read(cx).qa_has_painted() {
                    // Repeating Add Comment used to silently replace the open
                    // editor and lose its draft. Exercise the production
                    // command path and require identity/range preservation.
                    let pending_range = self.pending_comment_range;
                    self.add_comment(&AddComment, window, cx);
                    if self.comment_editor.as_ref() != Some(&editor)
                        || self.pending_comment_range != pending_range
                    {
                        return Err("repeated Add Comment replaced the open draft".to_owned());
                    }
                    self.warning = None;
                    // Exercise the same native key/input path a person uses:
                    // multiword text (including Space), selection, and
                    // formatting. The shared debounce must persist it without
                    // an explicit save command.
                    Self::qa_defer_keystrokes(
                        &[
                            "i", "m", "p", "o", "r", "t", "a", "n", "t", "space", "c", "o", "p",
                            "y", "space", "c", "h", "e", "c", "k", "cmd-a", "cmd-b",
                        ],
                        window,
                        cx,
                    )?;
                    self.qa_feature_phase = QaFeaturePhase::WaitCommentEdited;
                }
            }
            QaFeaturePhase::WaitCommentEdited => {
                let Some(editor) = self.comment_editor.as_ref() else {
                    return Err("comment editor disappeared after native editing".to_owned());
                };
                if !self.comment_draft_dirty {
                    return Err("native comment edits did not mark the draft dirty".to_owned());
                }
                if editor.read(cx).markdown() != "**important copy check**" {
                    return Ok(false);
                }
                self.qa_feature_phase = QaFeaturePhase::WaitCommentSaved;
            }
            QaFeaturePhase::WaitCommentSaved => {
                let annotations = self
                    .annotations
                    .as_ref()
                    .ok_or_else(|| "annotations disappeared after comment save".to_owned())?;
                if annotations.len() != 6 || annotations.revision() != 6 {
                    return Ok(false);
                }
                if self.comment_draft_dirty {
                    return Ok(false);
                }
                if self.comment_editor.is_none() {
                    return Err("Classic autosave closed the comment editor".to_owned());
                }
                let comment = annotations
                    .iter()
                    .find_map(|annotation| annotation.comment_markdown().map(ToOwned::to_owned))
                    .ok_or_else(|| "native comment save produced no Markdown".to_owned())?;
                if comment != "**important copy check**" {
                    return Err(format!(
                        "native comment input/formatting produced unexpected Markdown: {comment:?}"
                    ));
                }
                self.qa_feature_phase = QaFeaturePhase::WaitCommentBack;
            }
            QaFeaturePhase::WaitCommentBack => {
                let Some(editor) = self.comment_editor.clone() else {
                    return Err("comment editor disappeared before Back QA".to_owned());
                };
                if editor.read(cx).qa_has_painted() {
                    Self::qa_defer_keystrokes(&["escape"], window, cx)?;
                    self.qa_feature_phase = QaFeaturePhase::WaitCommentList;
                }
            }
            QaFeaturePhase::WaitCommentList => {
                if self.comment_editor.is_some() {
                    return Ok(false);
                }
                let annotations = self
                    .annotations
                    .as_ref()
                    .ok_or_else(|| "annotations disappeared after comment cancel".to_owned())?;
                let comment = annotations
                    .iter()
                    .find_map(|annotation| annotation.comment_markdown());
                if annotations.revision() != 6 || comment != Some("**important copy check**") {
                    return Err("Back changed the persisted comment".to_owned());
                }
                let annotation = annotations
                    .iter()
                    .find(|annotation| annotation.comment_markdown().is_some())
                    .ok_or_else(|| "Classic comment vanished before parity checks".to_owned())?;
                let id = annotation.id();
                let start = annotation.range().start();
                let page = self
                    .layout()
                    .and_then(|layout| layout.page_rect(start.page))
                    .ok_or_else(|| "Classic hit-test page is not laid out".to_owned())?;
                let bounds = self
                    .page_text
                    .get(&start.page)
                    .and_then(|text| text.get(start.index))
                    .and_then(|character| character.bounds)
                    .ok_or_else(|| "Classic hit-test character has no bounds".to_owned())?;
                let pointer = point(
                    px(page.x + (bounds.left + bounds.right) * page.width * 0.5 - self.scroll.x),
                    px(self.content_top()
                        + page.y
                        + (bounds.top + bounds.bottom) * page.height * 0.5
                        - self.scroll.y),
                );
                self.active_annotation = None;
                self.on_mouse_down(
                    &MouseDownEvent {
                        button: MouseButton::Left,
                        position: pointer,
                        click_count: 1,
                        ..Default::default()
                    },
                    window,
                    cx,
                );
                self.on_mouse_up(
                    &MouseUpEvent {
                        button: MouseButton::Left,
                        position: pointer,
                        ..Default::default()
                    },
                    window,
                    cx,
                );
                if self.active_annotation != Some(id)
                    || self.selection.is_some()
                    || !self.context_has_comment()
                {
                    return Err(
                        "clicking a Classic highlight did not expose its toolbar context".into(),
                    );
                }
                self.comment_on_context(window, cx);
                if self.comment_editor.is_none() || self.editing_annotation != Some(id) {
                    return Err("Classic toolbar Edit Comment did not open the annotation".into());
                }
                self.return_to_comment_list(window, cx);
                self.open_comment_from_list(id, window, cx);
                if self.comment_editor.is_none() || self.editing_annotation != Some(id) {
                    return Err("Classic comment-list row did not open the editor".into());
                }
                self.return_to_comment_list(window, cx);
                self.qa_feature_phase = QaFeaturePhase::WaitCommentsOpen;
            }
            QaFeaturePhase::WaitCommentsOpen => {
                if self.qa_viewport_is_settled()
                    && self.sidebar.panel == SidePanel::Comments
                    && self.sidebar.progress == 1.0
                {
                    self.toggle_sidebar(SidePanel::Comments, window, cx);
                    // A precise trackpad packet during the slide must move the
                    // document without cancelling the sidebar's frame chain.
                    self.scroll_by(0.0, 12.0, true, window, cx);
                    // Middle-button panning used to cancel the same frame
                    // chain through a separate input branch. Press/release is
                    // enough to prove the slide remains scheduled.
                    let pointer = point(px(100.0), px(self.content_top() + 100.0));
                    self.on_mouse_down(
                        &MouseDownEvent {
                            button: MouseButton::Middle,
                            position: pointer,
                            ..Default::default()
                        },
                        window,
                        cx,
                    );
                    self.on_mouse_up(
                        &MouseUpEvent {
                            button: MouseButton::Middle,
                            position: pointer,
                            ..Default::default()
                        },
                        window,
                        cx,
                    );
                    self.qa_feature_phase = QaFeaturePhase::WaitCommentsClosed;
                }
            }
            QaFeaturePhase::WaitCommentsClosed => {
                if self.qa_viewport_is_settled() && self.sidebar.progress == 0.0 {
                    if !self.focus_handle.is_focused(window) {
                        return Err(
                            "closing Comments left a hidden comment input focused".to_owned()
                        );
                    }
                    self.show_sidebar(SidePanel::Search, window, cx);
                    self.qa_feature_phase = QaFeaturePhase::WaitSearchOpen;
                }
            }
            QaFeaturePhase::WaitSearchOpen => {
                if self.qa_viewport_is_settled()
                    && self.sidebar.panel == SidePanel::Search
                    && self.sidebar.progress == 1.0
                {
                    window.focus(&self.search_field.focus_handle(cx));
                    Self::qa_defer_keystrokes(&["p", "a", "g", "e"], window, cx)?;
                    self.qa_feature_phase = QaFeaturePhase::WaitSearch;
                }
            }
            QaFeaturePhase::WaitSearch => {
                if self.qa_viewport_is_settled() {
                    if self.search_field.read(cx).text() != "page" {
                        return Err("native search typing did not update the field".to_owned());
                    }
                    if self.search.order.len() < 2 {
                        return Err(format!(
                            "fixture search returned only {} result(s)",
                            self.search.order.len()
                        ));
                    }
                    Self::qa_defer_keystrokes(&["enter"], window, cx)?;
                    self.qa_feature_phase = QaFeaturePhase::WaitNavigation;
                }
            }
            QaFeaturePhase::WaitNavigation => {
                if self.qa_viewport_is_settled() {
                    let active_position = self
                        .search
                        .active
                        .and_then(|active| self.search.order.iter().position(|id| *id == active));
                    if active_position != Some(1) {
                        return Ok(false);
                    }
                    self.navigate_search(false, window, cx);
                    self.qa_feature_phase = QaFeaturePhase::WaitSearchReturn;
                }
            }
            QaFeaturePhase::WaitSearchReturn => {
                if self.qa_viewport_is_settled() {
                    // Closing a sidebar widens the viewport. Center the
                    // document first so preserving its center is geometrically
                    // possible instead of being dominated by a scroll-edge
                    // clamp near the search hit.
                    if let Some(layout) = self.layout() {
                        self.scroll.x = (layout.content_width - self.viewport_width).max(0.0) * 0.5;
                        self.scroll_target.x = self.scroll.x;
                    }
                    self.toggle_sidebar(SidePanel::Search, window, cx);
                    self.qa_feature_phase = QaFeaturePhase::WaitSearchClosed;
                }
            }
            QaFeaturePhase::WaitSearchClosed => {
                if self.qa_viewport_is_settled() && self.sidebar.progress == 0.0 {
                    if !self.focus_handle.is_focused(window) {
                        return Err("closing Search left its hidden text field focused".to_owned());
                    }
                    self.show_sidebar(SidePanel::Search, window, cx);
                    self.qa_feature_phase = QaFeaturePhase::WaitSearchReopened;
                }
            }
            QaFeaturePhase::WaitSearchReopened => {
                if self.qa_viewport_is_settled()
                    && self.sidebar.panel == SidePanel::Search
                    && self.sidebar.progress == 1.0
                {
                    self.navigate_search(true, window, cx);
                    self.qa_feature_phase = QaFeaturePhase::WaitFinalNavigation;
                }
            }
            QaFeaturePhase::WaitFinalNavigation => {
                if self.qa_viewport_is_settled() {
                    self.qa_validate_feature_scenario()?;
                    self.qa_feature_phase = QaFeaturePhase::Complete;
                    return Ok(true);
                }
            }
            QaFeaturePhase::Complete => return Ok(true),
        }
        Ok(false)
    }

    /// Exercises Fluid-only interaction semantics through native editor input,
    /// production autosave/persistence, annotation hit testing, both comment
    /// pane slide directions, and the overlay search-panel geometry.
    #[cfg(debug_assertions)]
    pub fn qa_drive_fluid_scenario(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Result<bool, String> {
        match self.qa_fluid_phase {
            QaFluidPhase::Seed => {
                if !self.qa_viewport_is_settled()
                    || self.annotations_loading
                    || !self.page_text.contains_key(&0)
                {
                    return Ok(false);
                }
                if self
                    .annotations
                    .as_ref()
                    .is_none_or(|annotations| annotations.iter().next().is_some())
                {
                    return Err("fluid scenario requires a PDF with no existing sidecar".into());
                }
                self.set_view_mode(ReaderView::Fluid, window, cx);
                let range = self
                    .qa_text_range(0, "Select this sentence")
                    .ok_or_else(|| "fixture Fluid text was not extracted".to_owned())?;
                self.selection = Some(range.as_selection());
                if std::env::var_os("GPUI_PDF_READER_QA_FLUID_SELECTION_VISUAL").is_some() {
                    self.qa_fluid_phase = QaFluidPhase::Complete;
                    cx.notify();
                    return Ok(true);
                }
                self.add_highlight(HighlightColor::Yellow, cx);
                let id = self
                    .active_annotation
                    .ok_or_else(|| "Fluid highlight did not become active".to_owned())?;
                if self.annotation_at_text_position(range.start()) != Some(id) {
                    return Err("highlight hit lookup did not resolve the active annotation".into());
                }
                self.comment_on_context(window, cx);
                self.qa_fluid_phase = QaFluidPhase::WaitEditor;
            }
            QaFluidPhase::WaitEditor => {
                let Some(editor) = self.comment_editor.clone() else {
                    return Err("Fluid comment editor disappeared before painting".into());
                };
                if editor.read(cx).qa_has_painted()
                    && self.comment_pane.progress == 1.0
                    && self.sidebar.progress == 1.0
                {
                    Self::qa_defer_keystrokes(
                        &["f", "l", "u", "i", "d", "space", "n", "o", "t", "e"],
                        window,
                        cx,
                    )?;
                    self.qa_fluid_phase = QaFluidPhase::WaitAutosave;
                }
            }
            QaFluidPhase::WaitAutosave => {
                if !self.qa_viewport_is_settled() {
                    return Ok(false);
                }
                let (id, range, comment, highlight) = self
                    .annotations
                    .as_ref()
                    .and_then(|annotations| {
                        annotations.iter().find_map(|annotation| {
                            annotation.comment_markdown().map(|comment| {
                                (
                                    annotation.id(),
                                    annotation.range(),
                                    comment.to_owned(),
                                    annotation.highlight(),
                                )
                            })
                        })
                    })
                    .ok_or_else(|| "Fluid autosave produced no persisted comment".to_owned())?;
                if comment != "fluid note" || highlight != Some(HighlightColor::Yellow) {
                    return Err(format!(
                        "Fluid autosave produced unexpected annotation: comment={comment:?}, highlight={highlight:?}"
                    ));
                }
                if self.comment_editor.is_none()
                    || self.comment_draft_dirty
                    || self.editing_annotation != Some(id)
                {
                    return Err(
                        "Fluid autosave closed the editor or left the saved draft dirty".into(),
                    );
                }
                if self.context_range() != Some(range) || !self.context_has_comment() {
                    return Err("Fluid context did not switch from Add note to Edit note".into());
                }
                self.return_to_comment_list(window, cx);
                self.qa_fluid_phase = QaFluidPhase::WaitList;
            }
            QaFluidPhase::WaitList => {
                if !self.qa_viewport_is_settled()
                    || self.comment_editor.is_some()
                    || self.comment_pane.progress != 0.0
                {
                    return Ok(false);
                }
                let (id, start) = self
                    .annotations
                    .as_ref()
                    .and_then(|annotations| {
                        annotations.iter().find_map(|annotation| {
                            annotation
                                .comment_markdown()
                                .is_some()
                                .then_some((annotation.id(), annotation.range().start()))
                        })
                    })
                    .ok_or_else(|| "Fluid comment vanished after Back".to_owned())?;
                let page = self
                    .layout()
                    .and_then(|layout| layout.page_rect(start.page))
                    .ok_or_else(|| "Fluid hit-test page is not laid out".to_owned())?;
                let bounds = self
                    .page_text
                    .get(&start.page)
                    .and_then(|text| text.get(start.index))
                    .and_then(|character| character.bounds)
                    .ok_or_else(|| "Fluid hit-test character has no bounds".to_owned())?;
                let pointer = point(
                    px(page.x + (bounds.left + bounds.right) * page.width * 0.5 - self.scroll.x),
                    px(self.content_top()
                        + page.y
                        + (bounds.top + bounds.bottom) * page.height * 0.5
                        - self.scroll.y),
                );
                self.active_annotation = None;
                self.on_mouse_down(
                    &MouseDownEvent {
                        button: MouseButton::Left,
                        position: pointer,
                        click_count: 1,
                        ..Default::default()
                    },
                    window,
                    cx,
                );
                self.on_mouse_up(
                    &MouseUpEvent {
                        button: MouseButton::Left,
                        position: pointer,
                        ..Default::default()
                    },
                    window,
                    cx,
                );
                if self.active_annotation != Some(id) || self.selection.is_some() {
                    return Err("clicking a Fluid highlight did not activate it cleanly".into());
                }
                if std::env::var_os("GPUI_PDF_READER_QA_FLUID_CONTEXT_VISUAL").is_some() {
                    self.qa_fluid_phase = QaFluidPhase::Complete;
                    return Ok(true);
                }
                self.open_comment_from_list(id, window, cx);
                self.qa_fluid_phase = QaFluidPhase::WaitReopenedEditor;
            }
            QaFluidPhase::WaitReopenedEditor => {
                let Some(editor) = self.comment_editor.clone() else {
                    return Err("comment-list navigation did not open the Fluid editor".into());
                };
                if self.comment_pane.progress == 1.0 && editor.read(cx).qa_has_painted() {
                    if editor.read(cx).markdown() != "fluid note" {
                        return Err("reopened Fluid editor did not preserve Markdown".into());
                    }
                    if std::env::var_os("GPUI_PDF_READER_QA_FLUID_EDITOR_VISUAL").is_some() {
                        self.qa_fluid_phase = QaFluidPhase::Complete;
                        return Ok(true);
                    }
                    self.return_to_comment_list(window, cx);
                    self.qa_fluid_phase = QaFluidPhase::WaitFinalList;
                }
            }
            QaFluidPhase::WaitFinalList => {
                if self.qa_viewport_is_settled()
                    && self.comment_editor.is_none()
                    && self.comment_pane.progress == 0.0
                {
                    self.show_sidebar(SidePanel::Search, window, cx);
                    self.qa_fluid_phase = QaFluidPhase::WaitSearchOpen;
                }
            }
            QaFluidPhase::WaitSearchOpen => {
                if self.qa_viewport_is_settled()
                    && self.sidebar.panel == SidePanel::Search
                    && self.sidebar.progress == 1.0
                {
                    window.focus(&self.search_field.focus_handle(cx));
                    Self::qa_defer_keystrokes(&["p", "a", "g", "e"], window, cx)?;
                    self.qa_fluid_phase = QaFluidPhase::WaitSearchResults;
                }
            }
            QaFluidPhase::WaitSearchResults => {
                if !self.qa_viewport_is_settled() {
                    return Ok(false);
                }
                let annotations = self
                    .annotations
                    .as_ref()
                    .ok_or_else(|| "Fluid annotations disappeared".to_owned())?;
                let annotation = annotations
                    .iter()
                    .next()
                    .ok_or_else(|| "Fluid scenario annotation disappeared".to_owned())?;
                if self.view_mode != ReaderView::Fluid
                    || annotations.len() != 1
                    || annotation.highlight() != Some(HighlightColor::Yellow)
                    || annotation.comment_markdown() != Some("fluid note")
                    || self.active_annotation != Some(annotation.id())
                    || !self.context_has_comment()
                    || self.context_anchor_in_viewport().is_none()
                    || self.search.order.len() < 2
                    || !self.search.complete
                {
                    return Err(format!(
                        "unexpected final Fluid state: view={:?}, annotations={}, results={}, complete={}",
                        self.view_mode,
                        annotations.len(),
                        self.search.order.len(),
                        self.search.complete
                    ));
                }
                let layout = self
                    .layout()
                    .ok_or_else(|| "Fluid layout disappeared".to_owned())?;
                let base_max = (layout.content_width - self.viewport_width).max(0.0);
                let expected_max = base_max + self.fluid_panel_occlusion();
                if (self.max_scroll_x(layout) - expected_max).abs() > 0.01
                    || self.fluid_panel_occlusion() <= SIDEBAR_WIDTH
                {
                    return Err("Fluid panel occlusion was not added to horizontal reach".into());
                }
                self.qa_fluid_phase = QaFluidPhase::Complete;
                return Ok(true);
            }
            QaFluidPhase::Complete => return Ok(true),
        }
        Ok(false)
    }

    fn listen_for_worker_events(
        entity: &Entity<Self>,
        events: mpsc::Receiver<WorkerEvent>,
        window: &mut Window,
        cx: &mut App,
    ) {
        let events = Arc::new(Mutex::new(events));
        let weak = entity.downgrade();
        window
            .spawn(cx, async move |async_cx| {
                loop {
                    let events = events.clone();
                    let receive = async_cx
                        .background_executor()
                        .spawn(async move { events.lock().unwrap().recv() });
                    let Ok(event) = receive.await else {
                        break;
                    };
                    let _ = async_cx.update(|window, cx| {
                        weak.update(cx, |reader, cx| {
                            reader.handle_worker_event(event, window, cx)
                        })
                        .ok();
                    });
                }
            })
            .detach();
    }

    fn listen_for_annotation_events(
        entity: &Entity<Self>,
        events: mpsc::Receiver<AnnotationIoEvent>,
        window: &mut Window,
        cx: &mut App,
    ) {
        let events = Arc::new(Mutex::new(events));
        let weak = entity.downgrade();
        window
            .spawn(cx, async move |async_cx| {
                loop {
                    let events = events.clone();
                    let receive = async_cx
                        .background_executor()
                        .spawn(async move { events.lock().unwrap().recv() });
                    let Ok(event) = receive.await else {
                        break;
                    };
                    let _ = async_cx.update(|window, cx| {
                        weak.update(cx, |reader, cx| {
                            reader.handle_annotation_event(event, window, cx)
                        })
                        .ok();
                    });
                }
            })
            .detach();
    }

    fn listen_for_native_pinch(entity: &Entity<Self>, window: &mut Window, cx: &mut App) {
        let receiver = Arc::new(Mutex::new(crate::native_gestures::install_pinch_monitor()));
        let weak = entity.downgrade();
        window
            .spawn(cx, async move |async_cx| {
                loop {
                    let receiver = receiver.clone();
                    let receive = async_cx
                        .background_executor()
                        .spawn(async move { receiver.lock().unwrap().recv() });
                    let Ok(pinch) = receive.await else {
                        break;
                    };
                    let _ = async_cx.update(|window, cx| {
                        let window_height = f32::from(window.viewport_size().height);
                        weak.update(cx, |reader, cx| {
                            let position = point(px(pinch.x), px(window_height - pinch.cocoa_y));
                            reader.zoom_at(reader.zoom * (1.0 + pinch.delta), position, window, cx);
                        })
                        .ok();
                    });
                }
            })
            .detach();
    }

    fn handle_worker_event(
        &mut self,
        event: WorkerEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match event {
            WorkerEvent::Ready => {
                if self.document.is_none() && !matches!(self.status, ReaderStatus::Loading(_)) {
                    self.status = ReaderStatus::Empty;
                }
            }
            WorkerEvent::Opened {
                generation,
                path,
                pages,
                toc,
            } if generation == self.generation => {
                self.drop_all_images(window, cx);
                let page_count = pages.len();
                self.annotations_loading = true;
                if !self
                    .annotation_io
                    .load(generation, path.clone(), page_count)
                {
                    self.annotations_loading = false;
                    self.annotation_persistence_blocked = true;
                    self.annotations = Some(AnnotationSet::new(page_count));
                    self.warning = Some("The annotation sidecar worker is unavailable".into());
                }
                self.document = Some(DocumentState {
                    path: path.clone(),
                    pages,
                    toc,
                });
                self.page_text.clear();
                self.pending.clear();
                self.render_viewport.clear();
                self.text_viewport.clear();
                self.render_debounce_until = None;
                self.zoom_render_task = None;
                self.text_pending.clear();
                self.copy_pending = None;
                self.selection = None;
                self.scroll = Offset::default();
                self.scroll_target = Offset::default();
                self.status = ReaderStatus::Ready;
                self.fit_width = true;
                self.apply_fit_width();
                self.rebuild_layout();
                let title = path
                    .file_name()
                    .and_then(|name| name.to_str())
                    .unwrap_or("PDF");
                window.set_window_title(&format!("{title} — GPUI PDF Reader"));
                self.request_visible_tiles(window);
            }
            WorkerEvent::TileRendered {
                generation,
                appearance,
                key,
                core_rect,
                render_rect,
                width,
                height,
                bgra,
            } if generation == self.generation && appearance == self.render_appearance => {
                let page = key.page;
                self.pending.remove(&key);

                let expected_len = width
                    .checked_mul(height)
                    .and_then(|pixels| pixels.checked_mul(4))
                    .and_then(|bytes| usize::try_from(bytes).ok());
                if expected_len != Some(bgra.len())
                    || width != render_rect.width
                    || height != render_rect.height
                {
                    self.status = ReaderStatus::Error(
                        format!("PDFium returned an invalid tile for page {}", page + 1).into(),
                    );
                    cx.notify();
                    return;
                }

                // GPUI documents RenderImage's frame data as BGRA. The image
                // crate buffer is only the owned four-byte carrier.
                if let Some(buffer) = RgbaImage::from_raw(width, height, bgra) {
                    let image = Arc::new(RenderImage::new(vec![Frame::new(buffer)]));
                    if let Some(previous) = self.rendered.insert(
                        key,
                        CachedTile {
                            core_rect,
                            render_rect,
                            byte_len: expected_len.unwrap(),
                            image,
                        },
                    ) {
                        Self::retire_images(vec![previous.image], window, cx);
                    }
                }
                self.evict_distant_tiles(window, cx);
                self.request_visible_tiles(window);
            }
            WorkerEvent::TileFailed {
                generation,
                appearance,
                key,
                message,
            } if generation == self.generation && appearance == self.render_appearance => {
                self.pending.remove(&key);
                self.warning = Some(message.into());
            }
            WorkerEvent::TextExtracted {
                generation,
                page,
                text,
            } if generation == self.generation => {
                self.page_text.entry(page).or_insert(text);
                self.text_pending.remove(&page);
                self.continue_pending_copy(cx);
                self.complete_pending_toc_navigation(page, window, cx);
                self.evict_distant_text();
            }
            WorkerEvent::TextFailed {
                generation,
                page,
                message,
            } if generation == self.generation => {
                // Complete selection/copy bookkeeping with an empty layer;
                // text failure on one page must not stall rendering or an
                // operation spanning the rest of the document.
                self.page_text
                    .entry(page)
                    .or_insert_with(|| Arc::new(TextLayer::empty()));
                self.text_pending.remove(&page);
                self.warning = Some(message.into());
                self.continue_pending_copy(cx);
                self.complete_pending_toc_navigation(page, window, cx);
                self.evict_distant_text();
            }
            WorkerEvent::SearchPageResults {
                generation,
                revision,
                results,
            } if generation == self.generation && revision == self.search.revision => {
                self.search.searched_pages = self.search.searched_pages.max(results.page + 1);
                if let Some(previous) = self.search.pages.remove(&results.page) {
                    let previous_ids: HashSet<_> =
                        previous.iter().map(|result| result.id).collect();
                    self.search.order.retain(|id| !previous_ids.contains(id));
                }
                self.search
                    .order
                    .extend(results.matches.iter().map(|result| result.id));
                self.search.order.sort_unstable();
                self.search
                    .pages
                    .insert(results.page, Arc::from(results.matches));
                self.search.truncated |= results.truncated;
            }
            WorkerEvent::SearchFinished {
                generation,
                revision,
                searched_pages,
                total_results,
                total_highlight_runs,
                skipped_pages,
                truncated,
            } if generation == self.generation && revision == self.search.revision => {
                self.search.searched_pages = searched_pages;
                self.search.total_highlight_runs = total_highlight_runs;
                self.search.complete = true;
                self.search.truncated = truncated;
                debug_assert_eq!(total_results, self.search.order.len());
                if self.search.active.is_none()
                    && let Some(first) = self.search.order.first().copied()
                {
                    self.activate_search_match(first, window, cx);
                }
                if skipped_pages > 0 {
                    self.warning = Some(
                        format!(
                            "Search skipped {skipped_pages} page{} whose text could not be read",
                            if skipped_pages == 1 { "" } else { "s" }
                        )
                        .into(),
                    );
                }
            }
            WorkerEvent::SearchWarning {
                generation,
                revision,
                page,
                message,
            } if generation == self.generation && revision == self.search.revision => {
                self.warning = Some(format!("Search page {}: {message}", page + 1).into());
            }
            WorkerEvent::SearchFailed {
                generation,
                revision,
                message,
            } if generation == self.generation && revision == self.search.revision => {
                self.search.complete = true;
                self.warning = Some(message.into());
            }
            WorkerEvent::Error {
                generation,
                message,
            } if generation.is_none() || generation == Some(self.generation) => {
                self.pending.clear();
                self.render_viewport.clear();
                self.text_viewport.clear();
                self.render_debounce_until = None;
                self.zoom_render_task = None;
                self.text_pending.clear();
                self.copy_pending = None;
                self.status = ReaderStatus::Error(message.into());
            }
            _ => {}
        }
        cx.notify();
    }

    fn handle_annotation_event(
        &mut self,
        event: AnnotationIoEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let mut saved_current_annotations = false;
        match event {
            AnnotationIoEvent::Loaded {
                generation,
                identity,
                annotations,
            } if generation == self.generation => {
                self.annotation_saved_revision = annotations.revision();
                self.annotation_enqueued_revision = annotations.revision();
                self.annotation_identity = Some(identity);
                self.annotations = Some(annotations);
                self.refresh_comment_order();
                self.annotations_loading = false;
                self.annotation_persistence_blocked = false;
                self.annotation_error = None;
                self.annotation_failed_revision = None;
            }
            AnnotationIoEvent::Saved {
                generation,
                revision,
            } if generation == self.generation => {
                self.annotation_saved_revision = self.annotation_saved_revision.max(revision);
                if self
                    .annotation_failed_revision
                    .is_some_and(|failed| revision >= failed)
                {
                    self.annotation_failed_revision = None;
                    self.annotation_error = None;
                }
                saved_current_annotations = true;
            }
            AnnotationIoEvent::Failed {
                generation,
                operation,
                revision,
                message,
            } if generation == self.generation => {
                if operation == AnnotationIoOperation::Load {
                    self.annotations_loading = false;
                    self.annotation_persistence_blocked = true;
                    let page_count = self
                        .document
                        .as_ref()
                        .map(|document| document.pages.len())
                        .unwrap_or(0);
                    self.annotations = Some(AnnotationSet::new(page_count));
                    self.comment_order.clear();
                }
                if let Some(revision) = revision {
                    self.annotation_failed_revision = Some(
                        self.annotation_failed_revision
                            .map_or(revision, |current| current.max(revision)),
                    );
                }
                let cancelled_open = operation == AnnotationIoOperation::Save
                    && matches!(
                        transition_pending_open(
                            &mut self.pending_open,
                            self.annotations.as_ref().map(AnnotationSet::revision),
                            self.annotation_saved_revision,
                            true,
                        ),
                        PendingOpenTransition::Cancelled(_)
                    );
                if cancelled_open {
                    self.annotation_error = Some(
                        format!(
                            "Could not open another PDF because annotations could not be saved: {message}"
                        )
                        .into(),
                    );
                } else {
                    self.annotation_error = Some(message.into());
                }
            }
            _ => {}
        }
        if saved_current_annotations
            && let PendingOpenTransition::Open(path) = transition_pending_open(
                &mut self.pending_open,
                self.annotations.as_ref().map(AnnotationSet::revision),
                self.annotation_saved_revision,
                false,
            )
        {
            self.begin_open_path(path, window, cx);
            return;
        }
        cx.notify();
    }

    fn open_dialog(&mut self, _: &OpenDocument, window: &mut Window, cx: &mut Context<Self>) {
        let prompt = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: Some("Open PDF".into()),
        });
        let weak = cx.weak_entity();
        window
            .spawn(cx, async move |cx| {
                if let Ok(Ok(Some(paths))) = prompt.await
                    && let Some(path) = paths.into_iter().next()
                {
                    let _ = cx.update(|window, cx| {
                        weak.update(cx, |reader, cx| reader.open_path(path, window, cx))
                            .ok();
                    });
                }
            })
            .detach();
    }

    fn open_path(&mut self, path: PathBuf, window: &mut Window, cx: &mut Context<Self>) {
        if self.comment_draft_needs_confirmation() {
            self.confirm_discard_comment(DraftDiscardAction::Open(path), window, cx);
            return;
        }
        self.open_path_after_comment_guard(path, window, cx);
    }

    fn open_path_after_comment_guard(
        &mut self,
        path: PathBuf,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let dirty_revision = unsaved_annotation_revision(
            self.annotations.as_ref().map(AnnotationSet::revision),
            self.annotation_saved_revision,
        );
        if let Some(revision) = dirty_revision {
            if self.annotation_persistence_blocked {
                self.annotation_error = Some(
                    "Could not open another PDF because unsaved annotations are blocked".into(),
                );
                cx.notify();
                return;
            }
            self.pending_open = Some(path);
            if (self.annotation_enqueued_revision < revision
                || self.annotation_failed_revision.is_some())
                && !self.persist_annotations()
            {
                self.pending_open = None;
                if self.annotation_error.is_none() {
                    self.annotation_error = Some(
                        "Could not open another PDF because annotations could not be queued for saving"
                            .into(),
                    );
                }
            }
            cx.notify();
            return;
        }
        self.begin_open_path(path, window, cx);
    }

    fn begin_open_path(&mut self, path: PathBuf, window: &mut Window, cx: &mut Context<Self>) {
        self.pending_open = None;
        self.generation = self.generation.wrapping_add(1);
        self.drop_all_images(window, cx);
        self.document = None;
        self.layout = None;
        self.page_text.clear();
        self.pending.clear();
        self.render_viewport.clear();
        self.text_viewport.clear();
        self.zoom_render_revision = self.zoom_render_revision.wrapping_add(1);
        self.render_debounce_until = None;
        self.zoom_render_task = None;
        self.text_pending.clear();
        self.copy_pending = None;
        self.search = SearchState::default();
        self.search_debounce_task = None;
        if !self.search_field.read(cx).text().is_empty()
            && let Err(rejection) = self
                .search_field
                .update(cx, |field, cx| field.set_text("", cx))
        {
            self.search.input_error = Some(rejection.to_string().into());
        }
        self.annotations = None;
        self.annotation_identity = None;
        self.annotations_loading = false;
        self.annotation_persistence_blocked = false;
        self.annotation_error = None;
        self.annotation_enqueued_revision = 0;
        self.annotation_failed_revision = None;
        self.annotation_saved_revision = 0;
        self.active_annotation = None;
        self.comment_order.clear();
        self.comment_editor = None;
        self.comment_draft_dirty = false;
        self.comment_discard_prompt_open = false;
        self.comment_pane = CommentPaneState::default();
        self.comment_autosave_revision = self.comment_autosave_revision.wrapping_add(1);
        self.comment_autosave_task = None;
        self.editing_annotation = None;
        self.pending_comment_range = None;
        self.warning = None;
        self.selection = None;
        self.pending_annotation_click = None;
        self.toc_hovered = None;
        self.toc_hover_position = 0.0;
        self.toc_hover_strength = 0.0;
        self.pending_toc_navigation = None;
        self.scroll = Offset::default();
        self.scroll_target = Offset::default();
        self.animation_active = false;
        #[cfg(debug_assertions)]
        {
            self.qa_feature_phase = QaFeaturePhase::Seed;
            self.qa_fluid_phase = QaFluidPhase::Seed;
            self.qa_sidebar_anchor_reference = None;
            self.qa_sidebar_transitions = 0;
            self.qa_max_sidebar_anchor_error = 0.0;
            self.qa_toc_text_matches = 0;
        }
        let name = path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("document")
            .to_owned();
        self.status = ReaderStatus::Loading(format!("Opening {name}…").into());
        if !self.worker.open(self.generation, path) {
            self.status = ReaderStatus::Error("The PDF renderer is unavailable".into());
        }
        cx.notify();
    }

    fn layout(&self) -> Option<&DocumentLayout> {
        self.layout.as_deref()
    }

    fn rebuild_layout(&mut self) {
        let Some(document) = self.document.as_ref() else {
            self.layout = None;
            return;
        };
        let layout = if let Some(layout) = self.layout.as_deref() {
            // `open_path()` clears the layout before replacing the document,
            // so an existing layout always owns this document's geometry.
            debug_assert_eq!(layout.page_count(), document.pages.len());
            layout.rescaled(self.zoom, self.viewport_width)
        } else {
            DocumentLayout::new(&document.pages, self.zoom, self.viewport_width)
        };
        self.layout = Some(Arc::new(layout));
    }

    fn update_viewport(&mut self, window: &Window) {
        let size = window.viewport_size();
        let previous_width = self.viewport_width;
        let previous_zoom = self.zoom;
        let full_width = f32::from(size.width).max(1.0);
        self.viewport_width = (full_width - self.sidebar_reserved_width(full_width)).max(1.0);
        let error_height = self.content_top() - TOOLBAR_HEIGHT;
        self.viewport_height = (f32::from(size.height) - TOOLBAR_HEIGHT - error_height).max(1.0);
        if self.fit_width {
            self.apply_fit_width();
        }
        if (self.viewport_width - previous_width).abs() > 0.01
            || (self.zoom - previous_zoom).abs() > 0.0001
        {
            self.rebuild_layout();
        }
        self.clamp_scroll();
    }

    fn update_sidebar_viewport_preserving_anchor(&mut self, window: &Window) {
        let size = window.viewport_size();
        let full_width = f32::from(size.width).max(1.0);
        let next_width = (full_width - self.sidebar_reserved_width(full_width)).max(1.0);
        if (next_width - self.viewport_width).abs() <= 0.01 {
            return;
        }
        self.viewport_width = next_width;
        self.rebuild_layout();
        if let Some(anchor) = self.sidebar_anchor
            && let Some((x, y)) = self
                .layout()
                .and_then(|layout| layout.content_point_for_anchor(anchor))
        {
            self.scroll = Offset {
                x: x - self.viewport_width * 0.5,
                y: y - self.viewport_height * 0.5,
            };
            self.scroll_target = self.scroll;
        }
        self.clamp_scroll();
    }

    fn sidebar_reserved_width(&self, full_width: f32) -> f32 {
        match self.view_mode {
            ReaderView::Classic => self.sidebar.available_width(full_width),
            ReaderView::Fluid => 0.0,
        }
    }

    fn fluid_panel_occlusion(&self) -> f32 {
        if self.view_mode == ReaderView::Fluid {
            (self.fluid_panel_width() + FLUID_PANEL_HORIZONTAL_MARGIN * 2.0) * self.sidebar.progress
        } else {
            0.0
        }
    }

    fn fluid_panel_width(&self) -> f32 {
        SIDEBAR_WIDTH.min((self.viewport_width - FLUID_PANEL_HORIZONTAL_MARGIN * 2.0).max(0.0))
    }

    fn max_scroll_x(&self, layout: &DocumentLayout) -> f32 {
        (layout.content_width - self.viewport_width + self.fluid_panel_occlusion()).max(0.0)
    }

    fn set_view_mode(&mut self, mode: ReaderView, window: &mut Window, cx: &mut Context<Self>) {
        if self.view_mode == mode {
            return;
        }
        self.sidebar_anchor = self.layout().and_then(|layout| {
            layout.anchor_at_content_point(
                self.scroll.x + self.viewport_width * 0.5,
                self.scroll.y + self.viewport_height * 0.5,
            )
        });
        self.fit_width = false;
        self.cancel_comment_autosave();
        self.view_mode = mode;
        let editor_open = self.comment_editor.is_some();
        self.comment_pane = CommentPaneState {
            progress: if editor_open { 1.0 } else { 0.0 },
            target: if editor_open { 1.0 } else { 0.0 },
            close_editor_on_finish: false,
        };
        self.update_sidebar_viewport_preserving_anchor(window);
        self.sidebar_anchor = None;
        self.clamp_scroll();
        self.request_visible_tiles(window);
        if self.comment_draft_dirty {
            self.schedule_comment_autosave(window, cx);
        }
        cx.notify();
    }

    fn use_classic_view(&mut self, _: &ClassicView, window: &mut Window, cx: &mut Context<Self>) {
        self.set_view_mode(ReaderView::Classic, window, cx);
    }

    fn use_fluid_view(&mut self, _: &FluidView, window: &mut Window, cx: &mut Context<Self>) {
        self.set_view_mode(ReaderView::Fluid, window, cx);
    }

    fn select_theme(&mut self, action: &SelectTheme, window: &mut Window, cx: &mut Context<Self>) {
        self.selected_theme = theme::apply_selection(action.name.as_ref(), window, cx);
        self.theme_preference = if self.selected_theme.is_some() {
            ThemePreference::Named
        } else {
            ThemePreference::System
        };
        self.update_render_appearance(window, cx);
    }

    fn update_render_appearance(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let appearance =
            render_appearance_from_theme(Theme::global(cx), self.pdf_dark_mode_enabled);
        if appearance != self.render_appearance {
            self.render_appearance = appearance;
            self.drop_all_images(window, cx);
            self.pending.clear();
            self.render_viewport.clear();
            self.request_visible_tiles(window);
        }
        cx.notify();
    }

    fn toggle_pdf_dark_mode(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.pdf_dark_mode_enabled = !self.pdf_dark_mode_enabled;
        self.update_render_appearance(window, cx);
    }

    fn toggle_sidebar(&mut self, panel: SidePanel, window: &mut Window, cx: &mut Context<Self>) {
        let center_anchor = self.layout().and_then(|layout| {
            layout.anchor_at_content_point(
                self.scroll.x + self.viewport_width * 0.5,
                self.scroll.y + self.viewport_height * 0.5,
            )
        });
        self.sidebar_anchor = center_anchor;
        #[cfg(debug_assertions)]
        {
            self.qa_sidebar_anchor_reference = center_anchor;
        }
        // A panel transition is a viewport slide. Holding zoom fixed avoids a
        // render-scale churn on every animation frame when Fit was last used.
        self.fit_width = false;
        self.scroll_target = self.scroll;
        self.sidebar.toggle(panel);
        if self.sidebar.target < 0.5 {
            window.focus(&self.focus_handle);
        } else if panel == SidePanel::Comments {
            if let Some(editor) = self.comment_editor.as_ref() {
                window.focus(&editor.focus_handle(cx));
            } else {
                window.focus(&self.focus_handle);
            }
        } else {
            window.focus(&self.focus_handle);
        }
        self.start_animation(window, cx);
    }

    #[cfg(debug_assertions)]
    fn record_qa_sidebar_transition(&mut self) {
        let Some(expected) = self.qa_sidebar_anchor_reference.take() else {
            return;
        };
        let actual = self.layout().and_then(|layout| {
            layout.anchor_at_content_point(
                self.scroll.x + self.viewport_width * 0.5,
                self.scroll.y + self.viewport_height * 0.5,
            )
        });
        let error = actual.map_or(1.0, |actual| {
            if actual.page != expected.page {
                1.0
            } else {
                (actual.x_fraction - expected.x_fraction)
                    .abs()
                    .max((actual.y_fraction - expected.y_fraction).abs())
            }
        });
        self.qa_sidebar_transitions += 1;
        self.qa_max_sidebar_anchor_error = self.qa_max_sidebar_anchor_error.max(error);
    }

    fn show_sidebar(&mut self, panel: SidePanel, window: &mut Window, cx: &mut Context<Self>) {
        if self.sidebar.panel != panel || self.sidebar.target < 0.5 {
            self.toggle_sidebar(panel, window, cx);
        }
    }

    fn find_document(&mut self, _: &Find, window: &mut Window, cx: &mut Context<Self>) {
        self.show_sidebar(SidePanel::Search, window, cx);
        window.focus(&self.search_field.focus_handle(cx));
    }

    fn toggle_comments(&mut self, _: &ToggleComments, window: &mut Window, cx: &mut Context<Self>) {
        self.toggle_sidebar(SidePanel::Comments, window, cx);
    }

    fn next_search_result(
        &mut self,
        _: &NextSearchResult,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.navigate_search(true, window, cx);
    }

    fn previous_search_result(
        &mut self,
        _: &PreviousSearchResult,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.navigate_search(false, window, cx);
    }

    fn set_search_query(&mut self, query: String, cx: &mut Context<Self>) {
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
        self.search.query = query.clone();
        self.search.pages.clear();
        self.search.order.clear();
        self.search.active = None;
        self.search.searched_pages = 0;
        self.search.total_highlight_runs = 0;
        self.search.complete = false;
        self.search.truncated = false;
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

    fn activate_search_match(
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
        let Some(page_rect) = self.layout().and_then(|layout| layout.page_rect(id.page)) else {
            return;
        };
        let (x, y) = result
            .highlight_runs
            .first()
            .map(|run| {
                (
                    page_rect.x + (run.left + run.right) * 0.5 * page_rect.width,
                    page_rect.y + (run.top + run.bottom) * 0.5 * page_rect.height,
                )
            })
            .unwrap_or((
                page_rect.x + page_rect.width * 0.5,
                page_rect.y + page_rect.height * 0.15,
            ));
        self.search.active = Some(id);
        if let Some(index) = self.search.order.iter().position(|result| *result == id) {
            self.search_list_scroll
                .scroll_to_item(index, ScrollStrategy::Center);
        }
        self.sidebar_anchor = None;
        self.scroll_target = Offset {
            x: x - self.viewport_width * 0.5,
            y: y - self.viewport_height * 0.35,
        };
        self.clamp_scroll();
        self.start_animation(window, cx);
        cx.notify();
    }

    fn navigate_search(&mut self, forward: bool, window: &mut Window, cx: &mut Context<Self>) {
        let Some(id) = next_search_match_id(&self.search.order, self.search.active, forward) else {
            return;
        };
        self.activate_search_match(id, window, cx);
    }

    fn persist_annotations(&mut self) -> bool {
        if self.annotation_persistence_blocked {
            return false;
        }
        let (Some(document), Some(identity), Some(annotations)) = (
            self.document.as_ref(),
            self.annotation_identity.clone(),
            self.annotations.clone(),
        ) else {
            return false;
        };
        let revision = annotations.revision();
        if !self.annotation_io.save(
            self.generation,
            document.path.clone(),
            identity,
            self.annotation_saved_revision,
            annotations,
        ) {
            self.annotation_failed_revision = Some(revision);
            self.annotation_error = Some("The annotation sidecar worker is unavailable".into());
            false
        } else {
            self.annotation_enqueued_revision = self.annotation_enqueued_revision.max(revision);
            true
        }
    }

    fn refresh_comment_order(&mut self) {
        self.comment_order = self
            .annotations
            .as_ref()
            .map(|annotations| {
                let mut comments = Vec::with_capacity(annotations.len());
                comments.extend(
                    annotations
                        .iter()
                        .filter(|annotation| annotation.comment_markdown().is_some())
                        .map(|annotation| annotation.id()),
                );
                comments
            })
            .unwrap_or_default();
    }

    fn add_highlight(&mut self, color: HighlightColor, cx: &mut Context<Self>) {
        if self.annotations_loading {
            self.annotation_error = Some("Annotations are still loading".into());
            cx.notify();
            return;
        }
        if self.annotation_persistence_blocked {
            if self.annotation_error.is_none() {
                self.annotation_error =
                    Some("Annotations are disabled because the sidecar could not be loaded".into());
            }
            cx.notify();
            return;
        }
        let range = self.selection.map(TextRange::from_selection).or_else(|| {
            self.active_annotation.and_then(|id| {
                self.annotations
                    .as_ref()
                    .and_then(|annotations| annotations.get(id))
                    .map(|annotation| annotation.range())
            })
        });
        let Some(range) = range else {
            self.warning = Some("Select text before adding a highlight".into());
            cx.notify();
            return;
        };
        if range.end().index > MAX_TEXT_CHARACTER_INDEX {
            self.warning = Some(
                "Highlighting Select All is not supported yet; select a concrete text range".into(),
            );
            cx.notify();
            return;
        }
        let Some(annotations) = self.annotations.as_mut() else {
            return;
        };
        let existing = annotations
            .overlapping(range)
            .filter(|annotation| annotation.range() == range)
            .max_by_key(|annotation| (annotation.updated_revision(), annotation.id()))
            .map(|annotation| {
                (
                    annotation.id(),
                    annotation.comment_markdown().map(ToOwned::to_owned),
                )
            });
        let result = if let Some((id, comment)) = existing {
            annotations
                .update(id, range, Some(color), comment)
                .map(|changed| (id, changed))
        } else {
            annotations
                .add(range, Some(color), None)
                .map(|id| (id, true))
        };
        match result {
            Ok((id, changed)) => {
                if self.annotation_failed_revision.is_none() && !self.annotation_persistence_blocked
                {
                    self.annotation_error = None;
                }
                self.active_annotation = Some(id);
                self.selection = None;
                if changed
                    || self.annotations.as_ref().is_some_and(|annotations| {
                        annotations.revision() > self.annotation_saved_revision
                    })
                {
                    let _ = self.persist_annotations();
                }
                self.refresh_comment_order();
            }
            Err(error) => self.warning = Some(error.to_string().into()),
        }
        cx.notify();
    }

    fn add_comment(&mut self, _: &AddComment, window: &mut Window, cx: &mut Context<Self>) {
        if self.comment_editor.is_some() {
            self.warning =
                Some("Finish or cancel the current comment before starting another".into());
            self.show_sidebar(SidePanel::Comments, window, cx);
            cx.notify();
            return;
        }
        if self.annotations_loading {
            self.annotation_error = Some("Annotations are still loading".into());
            cx.notify();
            return;
        }
        if self.annotation_persistence_blocked {
            if self.annotation_error.is_none() {
                self.annotation_error =
                    Some("Comments are disabled because the sidecar could not be loaded".into());
            }
            cx.notify();
            return;
        }
        let Some(selection) = self.selection else {
            self.warning = Some("Select text before adding a comment".into());
            cx.notify();
            return;
        };
        let range = TextRange::from_selection(selection);
        if range.end().index > MAX_TEXT_CHARACTER_INDEX {
            self.warning = Some(
                "Commenting on Select All is not supported yet; select a concrete text range"
                    .into(),
            );
            cx.notify();
            return;
        }
        let exact = self.annotations.as_ref().and_then(|annotations| {
            annotations
                .overlapping(range)
                .filter(|annotation| annotation.range() == range)
                .max_by_key(|annotation| (annotation.updated_revision(), annotation.id()))
                .map(|annotation| {
                    (
                        annotation.id(),
                        annotation.comment_markdown().unwrap_or("").to_owned(),
                    )
                })
        });
        let (editing, markdown) = exact
            .map(|(id, markdown)| (Some(id), markdown))
            .unwrap_or((None, String::new()));
        self.open_comment_editor(range, editing, markdown, window, cx);
    }

    fn comment_on_context(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.selection.is_some() {
            self.add_comment(&AddComment, window, cx);
            return;
        }
        let Some(id) = self.active_annotation else {
            self.warning = Some("Select text before adding a comment".into());
            cx.notify();
            return;
        };
        let Some((range, markdown)) = self
            .annotations
            .as_ref()
            .and_then(|annotations| annotations.get(id))
            .map(|annotation| {
                (
                    annotation.range(),
                    annotation.comment_markdown().unwrap_or("").to_owned(),
                )
            })
        else {
            return;
        };
        self.open_comment_editor(range, Some(id), markdown, window, cx);
    }

    fn context_range(&self) -> Option<TextRange> {
        self.selection
            .filter(|selection| selection.anchor != selection.focus)
            .map(TextRange::from_selection)
            .or_else(|| {
                self.active_annotation.and_then(|id| {
                    self.annotations
                        .as_ref()
                        .and_then(|annotations| annotations.get(id))
                        .map(|annotation| annotation.range())
                })
            })
    }

    fn context_has_comment(&self) -> bool {
        self.active_annotation.is_some_and(|id| {
            self.annotations
                .as_ref()
                .and_then(|annotations| annotations.get(id))
                .is_some_and(|annotation| annotation.comment_markdown().is_some())
        }) || self.selection.is_some_and(|selection| {
            if selection.anchor == selection.focus {
                return false;
            }
            let range = TextRange::from_selection(selection);
            self.annotations.as_ref().is_some_and(|annotations| {
                annotations.overlapping(range).any(|annotation| {
                    annotation.range() == range && annotation.comment_markdown().is_some()
                })
            })
        })
    }

    fn context_anchor_in_viewport(&self) -> Option<Rect> {
        let range = self.context_range()?;
        let layout = self.layout()?;
        let content_viewport = Rect {
            x: self.scroll.x,
            y: self.scroll.y,
            width: self.viewport_width,
            height: self.viewport_height,
        };
        let visible_pages: Vec<_> = layout
            .visible_pages(self.scroll.y, self.viewport_height, 0.0)
            .collect();
        for page in visible_pages.into_iter().rev() {
            if page < range.start().page || page > range.end().page {
                continue;
            }
            let Some(chars) = self.page_text.get(&page) else {
                continue;
            };
            let Some(page_rect) = layout.page_rect(page) else {
                continue;
            };
            let Some(indices) = range.indices_on_page(page, chars.len()) else {
                continue;
            };
            let mut bottom_line: Option<Rect> = None;
            let mut visited = 0usize;
            chars.for_each_visible_in_range_while(
                page_rect,
                content_viewport,
                indices,
                |_, rect| {
                    visited += 1;
                    if visited > MAX_VISIBLE_SELECTION_QUADS {
                        return false;
                    }
                    bottom_line = Some(match bottom_line {
                        None => rect,
                        Some(current) => {
                            let tolerance = current.height.max(rect.height) * 0.5;
                            if rect.bottom() > current.bottom() + tolerance {
                                rect
                            } else if (rect.bottom() - current.bottom()).abs() <= tolerance {
                                let left = current.x.min(rect.x);
                                let top = current.y.min(rect.y);
                                Rect {
                                    x: left,
                                    y: top,
                                    width: current.right().max(rect.right()) - left,
                                    height: current.bottom().max(rect.bottom()) - top,
                                }
                            } else {
                                current
                            }
                        }
                    });
                    true
                },
            );
            if let Some(line) = bottom_line {
                return Some(Rect {
                    x: line.x - self.scroll.x,
                    y: line.y - self.scroll.y,
                    width: line.width,
                    height: line.height,
                });
            }
        }
        None
    }

    fn open_comment_editor(
        &mut self,
        range: TextRange,
        editing: Option<AnnotationId>,
        markdown: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let buffer = match RichTextBuffer::try_from_markdown(&markdown) {
            Ok(buffer) => buffer,
            Err(error) => {
                self.annotation_error = Some(
                    format!("Unable to open comment: {error}. The stored comment was not changed.")
                        .into(),
                );
                self.show_sidebar(SidePanel::Comments, window, cx);
                cx.notify();
                return;
            }
        };
        if self.annotation_failed_revision.is_none() && !self.annotation_persistence_blocked {
            self.annotation_error = None;
        }
        let editor = cx.new(|cx| CommentEditor::new(cx, buffer));
        cx.subscribe_in(
            &editor,
            window,
            |reader, _, event, window, cx| match event {
                CommentEditorEvent::Changed => reader.comment_editor_changed(window, cx),
                CommentEditorEvent::Save(markdown) => {
                    reader.cancel_comment_autosave();
                    let _ = reader.write_comment(markdown.clone(), cx);
                }
                CommentEditorEvent::Cancel => reader.cancel_comment_editor(window, cx),
            },
        )
        .detach();
        self.pending_comment_range = Some(range);
        self.editing_annotation = editing;
        self.comment_editor = Some(editor.clone());
        self.comment_draft_dirty = false;
        self.comment_pane
            .show_editor(self.view_mode == ReaderView::Fluid);
        self.show_sidebar(SidePanel::Comments, window, cx);
        if self.view_mode == ReaderView::Fluid {
            self.start_animation(window, cx);
        }
        window.focus(&editor.focus_handle(cx));
        cx.notify();
    }

    fn edit_comment(&mut self, id: AnnotationId, window: &mut Window, cx: &mut Context<Self>) {
        if self.comment_editor.is_some() {
            return;
        }
        let Some((range, markdown)) = self
            .annotations
            .as_ref()
            .and_then(|annotations| annotations.get(id))
            .and_then(|annotation| {
                annotation
                    .comment_markdown()
                    .map(|markdown| (annotation.range(), markdown.to_owned()))
            })
        else {
            return;
        };
        self.open_comment_editor(range, Some(id), markdown, window, cx);
    }

    fn write_comment(&mut self, markdown: String, cx: &mut Context<Self>) -> bool {
        if markdown.trim().is_empty() {
            self.annotation_error = Some("A comment cannot be empty".into());
            cx.notify();
            return false;
        }
        let Some(range) = self.pending_comment_range else {
            return false;
        };
        if self.annotation_failed_revision.is_none() && !self.annotation_persistence_blocked {
            self.annotation_error = None;
        }
        let Some(annotations) = self.annotations.as_mut() else {
            return false;
        };
        let result = if let Some(id) = self.editing_annotation {
            let highlight = annotations
                .get(id)
                .and_then(|annotation| annotation.highlight());
            annotations
                .update(id, range, highlight, Some(markdown))
                .map(|changed| (id, changed))
        } else {
            annotations
                .add(range, None, Some(markdown))
                .map(|id| (id, true))
        };
        match result {
            Ok((id, changed)) => {
                self.active_annotation = Some(id);
                self.selection = None;
                if changed
                    || self.annotations.as_ref().is_some_and(|annotations| {
                        annotations.revision() > self.annotation_saved_revision
                    })
                {
                    let _ = self.persist_annotations();
                }
                self.refresh_comment_order();
                self.comment_draft_dirty = false;
                self.editing_annotation = Some(id);
                cx.notify();
                true
            }
            Err(error) => {
                self.annotation_error = Some(error.to_string().into());
                cx.notify();
                false
            }
        }
    }

    fn comment_editor_changed(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.comment_draft_dirty = true;
        self.schedule_comment_autosave(window, cx);
        cx.notify();
    }

    fn schedule_comment_autosave(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.comment_autosave_revision = self.comment_autosave_revision.wrapping_add(1);
        let revision = self.comment_autosave_revision;
        let weak = cx.weak_entity();
        self.comment_autosave_task = Some(window.spawn(cx, async move |cx| {
            cx.background_executor()
                .timer(COMMENT_AUTOSAVE_DEBOUNCE)
                .await;
            let _ = cx.update(|_, cx| {
                weak.update(cx, |reader, cx| {
                    if reader.comment_autosave_revision == revision {
                        reader.comment_autosave_task = None;
                        let _ = reader.flush_comment_autosave(cx);
                    }
                })
                .ok();
            });
        }));
    }

    fn cancel_comment_autosave(&mut self) {
        self.comment_autosave_revision = self.comment_autosave_revision.wrapping_add(1);
        self.comment_autosave_task = None;
    }

    fn flush_comment_autosave(&mut self, cx: &mut Context<Self>) -> bool {
        self.cancel_comment_autosave();
        if !self.comment_draft_dirty {
            return true;
        }
        let Some(editor) = self.comment_editor.as_ref() else {
            return true;
        };
        let markdown = editor.read(cx).markdown();
        if markdown.trim().is_empty() {
            return false;
        }
        self.write_comment(markdown, cx)
    }

    fn return_to_comment_list(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let markdown_is_blank = self
            .comment_editor
            .as_ref()
            .is_none_or(|editor| editor.read(cx).is_blank());
        if self.comment_draft_dirty && !markdown_is_blank && !self.flush_comment_autosave(cx) {
            return;
        }
        self.cancel_comment_autosave();
        self.comment_draft_dirty = false;
        window.focus(&self.focus_handle);
        if self.view_mode == ReaderView::Fluid {
            self.comment_pane.show_list(true);
            self.start_animation(window, cx);
        } else {
            self.comment_pane.show_list(false);
            self.finish_comment_editor_close();
        }
        cx.notify();
    }

    fn cancel_comment_editor(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.return_to_comment_list(window, cx);
    }

    fn discard_comment_editor(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.cancel_comment_autosave();
        self.comment_pane.show_list(false);
        self.finish_comment_editor_close();
        window.focus(&self.focus_handle);
        cx.notify();
    }

    fn finish_comment_editor_close(&mut self) {
        self.comment_editor = None;
        self.comment_draft_dirty = false;
        self.editing_annotation = None;
        self.pending_comment_range = None;
        self.comment_pane.close_editor_on_finish = false;
    }

    fn comment_draft_needs_confirmation(&self) -> bool {
        comment_draft_needs_confirmation(self.comment_editor.is_some(), self.comment_draft_dirty)
    }

    fn confirm_discard_comment(
        &mut self,
        action: DraftDiscardAction,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.comment_discard_prompt_open {
            return;
        }

        let (message, detail, discard_label) = match &action {
            DraftDiscardAction::Open(_) => (
                "Discard this comment draft?",
                "Opening another PDF will permanently discard the unsaved comment.",
                "Discard and Open",
            ),
            DraftDiscardAction::Quit => (
                "Quit with an unsaved comment?",
                "The comment draft will be permanently discarded.",
                "Discard and Quit",
            ),
            DraftDiscardAction::CloseWindow => (
                "Close with an unsaved comment?",
                "The comment draft will be permanently discarded.",
                "Discard and Close",
            ),
        };
        self.comment_discard_prompt_open = true;
        let answer = window.prompt(
            PromptLevel::Warning,
            message,
            Some(detail),
            &[
                PromptButton::cancel("Keep Editing"),
                PromptButton::ok(discard_label),
            ],
            cx,
        );
        let weak = cx.weak_entity();
        window
            .spawn(cx, async move |cx| {
                let discard = answer.await.ok() == Some(1);
                let _ = cx.update(|window, cx| {
                    weak.update(cx, |reader, cx| {
                        reader.comment_discard_prompt_open = false;
                        if discard {
                            reader.discard_comment_editor(window, cx);
                            match action {
                                DraftDiscardAction::Open(path) => {
                                    reader.open_path_after_comment_guard(path, window, cx)
                                }
                                DraftDiscardAction::Quit => cx.quit(),
                                DraftDiscardAction::CloseWindow => window.remove_window(),
                            }
                        } else {
                            reader.show_sidebar(SidePanel::Comments, window, cx);
                            if let Some(editor) = reader.comment_editor.as_ref() {
                                window.focus(&editor.focus_handle(cx));
                            }
                            cx.notify();
                        }
                    })
                    .ok();
                });
            })
            .detach();
    }

    fn quit_application(&mut self, _: &Quit, window: &mut Window, cx: &mut Context<Self>) {
        if self.comment_draft_needs_confirmation() {
            self.confirm_discard_comment(DraftDiscardAction::Quit, window, cx);
        } else {
            cx.quit();
        }
    }

    pub fn should_close_window(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        if !self.comment_draft_needs_confirmation() {
            return true;
        }
        self.confirm_discard_comment(DraftDiscardAction::CloseWindow, window, cx);
        false
    }

    fn navigate_annotation(
        &mut self,
        id: AnnotationId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(range) = self
            .annotations
            .as_ref()
            .and_then(|annotations| annotations.get(id))
            .map(|annotation| annotation.range())
        else {
            return;
        };
        let start = range.start();
        let Some(page_rect) = self
            .layout()
            .and_then(|layout| layout.page_rect(start.page))
        else {
            return;
        };
        let target = self
            .page_text
            .get(&start.page)
            .and_then(|text| text.get(start.index))
            .and_then(|character| character.bounds)
            .map(|bounds| {
                (
                    page_rect.x + (bounds.left + bounds.right) * 0.5 * page_rect.width,
                    page_rect.y + (bounds.top + bounds.bottom) * 0.5 * page_rect.height,
                )
            })
            .unwrap_or((
                page_rect.x + page_rect.width * 0.5,
                page_rect.y + page_rect.height * 0.15,
            ));
        self.active_annotation = Some(id);
        self.sidebar_anchor = None;
        self.scroll_target = Offset {
            x: target.0 - self.viewport_width * 0.5,
            y: target.1 - self.viewport_height * 0.35,
        };
        self.clamp_scroll();
        self.start_animation(window, cx);
        cx.notify();
    }

    fn open_comment_from_list(
        &mut self,
        id: AnnotationId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.navigate_annotation(id, window, cx);
        self.edit_comment(id, window, cx);
    }

    fn content_top(&self) -> f32 {
        TOOLBAR_HEIGHT
            + if matches!(self.status, ReaderStatus::Error(_)) {
                ERROR_BAR_HEIGHT
            } else {
                0.0
            }
    }

    fn apply_fit_width(&mut self) {
        let Some(document) = self.document.as_ref() else {
            return;
        };
        let widest = document
            .pages
            .iter()
            .map(|page| page.width)
            .fold(1.0_f32, f32::max);
        let available = (self.viewport_width - crate::model::PAGE_MARGIN * 2.0).max(100.0);
        self.zoom = (available / (widest * crate::model::PDF_POINTS_TO_LOGICAL_PIXELS))
            .clamp(MIN_ZOOM, MAX_ZOOM);
    }

    fn request_visible_tiles(&mut self, window: &Window) {
        // While a replacement document is opening, `self.document` still describes
        // the previous PDF. Never pair that stale layout with the new generation.
        if !matches!(self.status, ReaderStatus::Ready) {
            return;
        }
        if self
            .render_debounce_until
            .is_some_and(|deadline| Instant::now() < deadline)
        {
            return;
        }
        self.render_debounce_until = None;
        let (Some(layout), Some(document)) = (self.layout(), self.document.as_ref()) else {
            return;
        };
        let mut planned = plan_visible_tiles(
            layout,
            &document.pages,
            self.scroll,
            self.viewport_width,
            self.viewport_height,
            window.scale_factor(),
        );
        planned.sort_by_key(|tile| (tile.tier, tile.distance, tile.request.key));
        let mut viewport: Vec<_> = planned
            .iter()
            .map(|tile| (tile.request.key, tile.tier))
            .collect();
        viewport.sort_by_key(|(key, tier)| (*tier, *key));
        let mut seen_text_pages = HashSet::new();
        let text_viewport: Vec<_> = planned
            .iter()
            .filter(|tile| tile.tier == DemandTier::Visible)
            .filter_map(|tile| {
                seen_text_pages
                    .insert(tile.request.key.page)
                    .then_some(tile.request.key.page)
            })
            .take(MAX_CACHED_TEXT_PAGES)
            .collect();

        // Completion does not change these full desired signatures. That
        // matters: re-sending shrinking lists after every result can race the
        // worker and reinsert work it has just finished.
        if viewport != self.render_viewport || text_viewport != self.text_viewport {
            let demand: Vec<_> = planned
                .iter()
                .filter_map(|tile| {
                    (!self.rendered.contains_key(&tile.request.key)).then_some(*tile)
                })
                .collect();
            let visible_tile_count = demand
                .iter()
                .take_while(|tile| tile.tier == DemandTier::Visible)
                .count();
            let tile_requests: Vec<_> = demand.iter().map(|tile| tile.request).collect();
            let text_pages: Vec<_> = text_viewport
                .iter()
                .copied()
                .filter(|page| !self.page_text.contains_key(page))
                .collect();
            if self.worker.render_viewport(
                self.generation,
                self.render_appearance,
                &tile_requests,
                visible_tile_count,
                &text_pages,
            ) {
                self.pending.clear();
                self.pending
                    .extend(tile_requests.iter().map(|tile| tile.key));
                self.render_viewport = viewport;
                self.text_viewport = text_viewport;
                self.warning = None;
            } else {
                self.pending.clear();
                self.status = ReaderStatus::Error("The PDF renderer is unavailable".into());
            }
        }
    }

    fn retire_images(images: Vec<Arc<RenderImage>>, window: &mut Window, cx: &mut Context<Self>) {
        if images.is_empty() {
            return;
        }
        // GPUI's Blade renderer submits Metal command buffers with unretained
        // texture references. Removing an atlas image immediately can destroy a
        // texture that the preceding frame is still sampling. Cross two frame
        // callbacks first: the intervening draw waits for the preceding GPU
        // submission, and the following callback can then retire the texture.
        let weak = cx.weak_entity();
        window.on_next_frame(move |window, cx| {
            window.on_next_frame(move |window, _| {
                for image in images {
                    let _ = window.drop_image(image);
                }
            });
            weak.update(cx, |_, cx| cx.notify()).ok();
        });
        cx.notify();
    }

    fn drop_all_images(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let images = self.rendered.drain().map(|(_, tile)| tile.image).collect();
        Self::retire_images(images, window, cx);
    }

    fn evict_distant_tiles(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let mut desired_by_page = HashMap::<usize, (RasterSize, Vec<TileKey>)>::new();
        for (key, tier) in &self.render_viewport {
            let entry = desired_by_page
                .entry(key.page)
                .or_insert((key.raster, Vec::new()));
            if *tier == DemandTier::Visible {
                entry.1.push(*key);
            }
        }
        let exact_pages: HashSet<_> = desired_by_page
            .iter()
            .filter_map(|(page, (_, visible))| {
                (!visible.is_empty() && visible.iter().all(|key| self.rendered.contains_key(key)))
                    .then_some(*page)
            })
            .collect();
        let obsolete: Vec<_> = self
            .rendered
            .keys()
            .copied()
            .filter(|key| {
                exact_pages.contains(&key.page)
                    && desired_by_page
                        .get(&key.page)
                        .is_some_and(|(raster, _)| *raster != key.raster)
            })
            .collect();
        let mut retired = Vec::new();
        for key in obsolete {
            if let Some(tile) = self.rendered.remove(&key) {
                retired.push(tile.image);
            }
        }

        let mut cached_bytes = self
            .rendered
            .values()
            .map(|tile| tile.byte_len)
            .sum::<usize>();
        if self.rendered.len() <= MAX_CACHED_TILES && cached_bytes <= MAX_CACHE_BYTES {
            Self::retire_images(retired, window, cx);
            return;
        }
        let protected: HashSet<_> = self
            .render_viewport
            .iter()
            .filter_map(|(key, tier)| (*tier == DemandTier::Visible).then_some(*key))
            .collect();
        let mut candidates: Vec<_> = self
            .rendered
            .keys()
            .copied()
            .filter(|key| !protected.contains(key))
            .collect();
        candidates.sort_by_key(|key| {
            let desired = self
                .render_viewport
                .iter()
                .find_map(|(desired, _)| (desired.page == key.page).then_some(desired.raster));
            let stale_scale = desired.is_some_and(|raster| raster != key.raster);
            (
                !stale_scale,
                std::cmp::Reverse(tile_distance_from_viewport(
                    *key,
                    self.layout(),
                    self.scroll,
                    self.viewport_width,
                    self.viewport_height,
                )),
            )
        });
        for key in candidates {
            if self.rendered.len() <= MAX_CACHED_TILES && cached_bytes <= MAX_CACHE_BYTES {
                break;
            }
            if let Some(cache) = self.rendered.remove(&key) {
                cached_bytes = cached_bytes.saturating_sub(cache.byte_len);
                retired.push(cache.image);
            }
        }
        Self::retire_images(retired, window, cx);
    }

    fn evict_distant_text(&mut self) {
        if self.page_text.len() <= MAX_CACHED_TEXT_PAGES {
            return;
        }
        let current_page = self
            .layout()
            .map(|layout| layout.current_page(self.scroll.y, self.viewport_height))
            .unwrap_or(0);
        let mut candidates: Vec<_> = self.page_text.keys().copied().collect();
        candidates.retain(|page| !self.text_viewport.contains(page));
        candidates.sort_by_key(|page| std::cmp::Reverse(page.abs_diff(current_page)));
        for page in candidates {
            if self.page_text.len() <= MAX_CACHED_TEXT_PAGES {
                break;
            }
            self.page_text.remove(&page);
        }
    }

    fn clamp_scroll(&mut self) {
        let Some(layout) = self.layout() else {
            self.scroll = Offset::default();
            self.scroll_target = Offset::default();
            return;
        };
        let max_x = self.max_scroll_x(layout);
        let max_y = (layout.content_height - self.viewport_height).max(0.0);
        self.scroll.x = self.scroll.x.clamp(0.0, max_x);
        self.scroll.y = self.scroll.y.clamp(0.0, max_y);
        self.scroll_target.x = self.scroll_target.x.clamp(0.0, max_x);
        self.scroll_target.y = self.scroll_target.y.clamp(0.0, max_y);
    }

    fn scroll_by(
        &mut self,
        x: f32,
        y: f32,
        immediate: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let ui_is_animating = self.sidebar.is_animating()
            || self.comment_pane.is_animating()
            || self.toc_hover_is_animating();
        self.sidebar_anchor = None;
        #[cfg(debug_assertions)]
        {
            // Once the user moves the document, the original transition
            // anchor is intentionally no longer the expected outcome.
            self.qa_sidebar_anchor_reference = None;
        }
        if !x.is_finite() || !y.is_finite() {
            return;
        }
        if immediate {
            if !ui_is_animating {
                self.animation_active = false;
            }
            self.scroll_target = self.scroll;
        }
        self.scroll_target.x += x;
        self.scroll_target.y += y;
        self.clamp_scroll();
        if immediate {
            self.scroll = self.scroll_target;
            self.request_visible_tiles(window);
            if ui_is_animating {
                self.start_animation(window, cx);
            }
            cx.notify();
        } else {
            self.start_animation(window, cx);
        }
    }

    fn start_animation(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.animation_active {
            return;
        }
        self.animation_active = true;
        self.last_animation_tick = Instant::now();
        self.queue_animation_frame(window, cx);
    }

    fn toc_hover_is_animating(&self) -> bool {
        toc_hover_state_is_animating(
            self.toc_hover_position,
            self.toc_hover_strength,
            self.toc_hovered,
        )
    }

    fn queue_animation_frame(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.animation_frame_queued {
            return;
        }
        self.animation_frame_queued = true;
        let weak = cx.weak_entity();
        window.on_next_frame(move |window, cx| {
            weak.update(cx, |reader, cx| reader.animation_tick(window, cx))
                .ok();
        });
        // `request_animation_frame()` requires an active GPUI paint context and
        // panics from input handlers. Notifying the entity schedules the frame;
        // the callback above then chains the animation tick after it renders.
        cx.notify();
    }

    fn animation_tick(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.animation_frame_queued = false;
        if !self.animation_active {
            return;
        }
        let now = Instant::now();
        let dt = now
            .duration_since(self.last_animation_tick)
            .as_secs_f32()
            .clamp(1.0 / 240.0, 0.05);
        self.last_animation_tick = now;
        let blend = 1.0 - (-18.0 * dt).exp();
        let sidebar_was_animating = self.sidebar.is_animating();
        if sidebar_was_animating {
            self.sidebar.advance(dt);
            self.update_sidebar_viewport_preserving_anchor(window);
            if !self.sidebar.is_animating() {
                #[cfg(debug_assertions)]
                self.record_qa_sidebar_transition();
                self.sidebar_anchor = None;
            }
        }
        if self.comment_pane.is_animating() {
            self.comment_pane.advance(dt);
            if !self.comment_pane.is_animating()
                && self.comment_pane.target == 0.0
                && self.comment_pane.close_editor_on_finish
            {
                self.finish_comment_editor_close();
            }
        }
        advance_toc_hover_state(
            &mut self.toc_hover_position,
            &mut self.toc_hover_strength,
            self.toc_hovered,
            blend,
        );
        if self.sidebar_anchor.is_none() {
            self.scroll.x += (self.scroll_target.x - self.scroll.x) * blend;
            self.scroll.y += (self.scroll_target.y - self.scroll.y) * blend;
        }
        let distance = (self.scroll_target.x - self.scroll.x).abs()
            + (self.scroll_target.y - self.scroll.y).abs();
        if distance < 0.35
            && !self.sidebar.is_animating()
            && !self.comment_pane.is_animating()
            && !self.toc_hover_is_animating()
        {
            self.scroll = self.scroll_target;
            self.animation_active = false;
        }
        self.request_visible_tiles(window);
        cx.notify();
        if self.animation_active {
            self.queue_animation_frame(window, cx);
        }
    }

    fn on_scroll_wheel(
        &mut self,
        event: &ScrollWheelEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let delta = event.delta.pixel_delta(px(44.0));
        let dx = f32::from(delta.x);
        let dy = f32::from(delta.y);
        if !dx.is_finite() || !dy.is_finite() {
            cx.stop_propagation();
            return;
        }
        if event.modifiers.platform || event.modifiers.control {
            // A single accelerated wheel packet must not overflow the zoom
            // calculation or jump through the entire range in one event.
            let factor = command_wheel_zoom_factor(dy).expect("finite delta checked above");
            self.zoom_at(self.zoom * factor, event.position, window, cx);
        } else {
            let (x, y) = if event.modifiers.shift && dx.abs() < f32::EPSILON {
                (-dy, 0.0)
            } else {
                (-dx, -dy)
            };
            self.scroll_by(x, y, event.delta.precise(), window, cx);
        }
        cx.stop_propagation();
    }

    fn zoom_at(
        &mut self,
        zoom: f32,
        window_position: Point<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.sidebar_anchor = None;
        let raw_x = f32::from(window_position.x);
        let raw_y = f32::from(window_position.y);
        if self.document.is_none() || !zoom.is_finite() || !raw_x.is_finite() || !raw_y.is_finite()
        {
            return;
        }
        let new_zoom = zoom.clamp(MIN_ZOOM, MAX_ZOOM);
        if (new_zoom - self.zoom).abs() < 0.0001 {
            return;
        }
        let local_x = raw_x.clamp(0.0, self.viewport_width);
        let local_y = (raw_y - self.content_top()).clamp(0.0, self.viewport_height);
        let anchor = self.layout().and_then(|layout| {
            layout.anchor_at_content_point(self.scroll.x + local_x, self.scroll.y + local_y)
        });

        self.zoom = new_zoom;
        self.fit_width = false;
        self.rebuild_layout();
        if let Some(anchor) = anchor
            && let Some((x, y)) = self
                .layout()
                .and_then(|layout| layout.content_point_for_anchor(anchor))
        {
            self.scroll = Offset {
                x: x - local_x,
                y: y - local_y,
            };
            self.scroll_target = self.scroll;
        }
        self.clamp_scroll();
        self.defer_render_after_zoom(window, cx);
        cx.notify();
    }

    fn defer_render_after_zoom(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.render_debounce_until.is_none() {
            // Debouncing new work is not enough: without this replacement the
            // worker continues the old multi-tile queue throughout the zoom.
            // Cancel it once at the beginning of a burst; at most the one tile
            // already inside PDFium can still complete.
            self.render_viewport.clear();
            self.text_viewport.clear();
            self.pending.clear();
            if !self
                .worker
                .render_viewport(self.generation, self.render_appearance, &[], 0, &[])
            {
                self.status = ReaderStatus::Error("The PDF renderer is unavailable".into());
                return;
            }
        }
        self.zoom_render_revision = self.zoom_render_revision.wrapping_add(1);
        let revision = self.zoom_render_revision;
        self.render_debounce_until = Some(Instant::now() + ZOOM_RENDER_DEBOUNCE);
        let weak = cx.weak_entity();
        self.zoom_render_task = Some(window.spawn(cx, async move |cx| {
            cx.background_executor().timer(ZOOM_RENDER_DEBOUNCE).await;
            let _ = cx.update(|window, cx| {
                weak.update(cx, |reader, cx| {
                    if reader.zoom_render_revision == revision {
                        reader.render_debounce_until = None;
                        reader.request_visible_tiles(window);
                        cx.notify();
                    }
                })
                .ok();
            });
        }));
    }

    fn zoom_in(&mut self, _: &ZoomIn, window: &mut Window, cx: &mut Context<Self>) {
        self.zoom_at(self.zoom * 1.15, self.viewport_center(), window, cx);
    }

    fn zoom_out(&mut self, _: &ZoomOut, window: &mut Window, cx: &mut Context<Self>) {
        self.zoom_at(self.zoom / 1.15, self.viewport_center(), window, cx);
    }

    fn actual_size(&mut self, _: &ActualSize, window: &mut Window, cx: &mut Context<Self>) {
        self.zoom_at(1.0, self.viewport_center(), window, cx);
    }

    fn fit_width(&mut self, _: &FitWidth, window: &mut Window, cx: &mut Context<Self>) {
        let before = self.zoom;
        self.fit_width = true;
        self.apply_fit_width();
        let target = self.zoom;
        self.zoom = before;
        self.zoom_at(target, self.viewport_center(), window, cx);
        self.fit_width = true;
    }

    fn viewport_center(&self) -> Point<Pixels> {
        point(
            px(self.viewport_width * 0.5),
            px(self.content_top() + self.viewport_height * 0.5),
        )
    }

    fn scroll_up(&mut self, _: &ScrollUp, window: &mut Window, cx: &mut Context<Self>) {
        self.scroll_by(0.0, -64.0, false, window, cx);
    }

    fn scroll_down(&mut self, _: &ScrollDown, window: &mut Window, cx: &mut Context<Self>) {
        self.scroll_by(0.0, 64.0, false, window, cx);
    }

    fn scroll_left(&mut self, _: &ScrollLeft, window: &mut Window, cx: &mut Context<Self>) {
        self.scroll_by(-64.0, 0.0, false, window, cx);
    }

    fn scroll_right(&mut self, _: &ScrollRight, window: &mut Window, cx: &mut Context<Self>) {
        self.scroll_by(64.0, 0.0, false, window, cx);
    }

    fn page_up(&mut self, _: &PageUp, window: &mut Window, cx: &mut Context<Self>) {
        self.scroll_by(0.0, -self.viewport_height * 0.9, false, window, cx);
    }

    fn page_down(&mut self, _: &PageDown, window: &mut Window, cx: &mut Context<Self>) {
        self.scroll_by(0.0, self.viewport_height * 0.9, false, window, cx);
    }

    fn first_page(&mut self, _: &FirstPage, window: &mut Window, cx: &mut Context<Self>) {
        self.scroll_target.y = 0.0;
        self.start_animation(window, cx);
    }

    fn last_page(&mut self, _: &LastPage, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(layout) = self.layout() {
            self.scroll_target.y = (layout.content_height - self.viewport_height).max(0.0);
            self.start_animation(window, cx);
        }
    }

    fn on_mouse_down(
        &mut self,
        event: &MouseDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        window.focus(&self.focus_handle);
        if !event.modifiers.shift && event.click_count == 1 {
            self.pending_annotation_click = self
                .hit_test_text(event.position, false)
                .and_then(|position| self.annotation_at_text_position(position));
            if self.pending_annotation_click.is_none() {
                self.active_annotation = None;
            }
        } else {
            self.pending_annotation_click = None;
        }
        match event.button {
            MouseButton::Left => {
                if let Some(position) = self.hit_test_text(event.position, false) {
                    if event.click_count >= 2 {
                        self.select_word(position);
                    } else if event.modifiers.shift {
                        if let Some(selection) = self.selection.as_mut() {
                            selection.focus = position;
                        } else {
                            self.selection = Some(TextSelection {
                                anchor: position,
                                focus: position,
                            });
                        }
                    } else {
                        self.selection = Some(TextSelection {
                            anchor: position,
                            focus: position,
                        });
                    }
                    self.selecting = true;
                } else if !event.modifiers.shift {
                    self.selection = None;
                }
            }
            MouseButton::Middle => {
                let ui_is_animating = self.sidebar.is_animating()
                    || self.comment_pane.is_animating()
                    || self.toc_hover_is_animating();
                self.sidebar_anchor = None;
                #[cfg(debug_assertions)]
                {
                    self.qa_sidebar_anchor_reference = None;
                }
                self.pan = Some(PanState {
                    pointer: event.position,
                    scroll: self.scroll,
                });
                if ui_is_animating {
                    self.start_animation(window, cx);
                } else {
                    self.animation_active = false;
                }
            }
            _ => {}
        }
        cx.notify();
    }

    fn on_mouse_move(
        &mut self,
        event: &MouseMoveEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(pan) = self.pan {
            let delta = event.position - pan.pointer;
            self.scroll = Offset {
                x: pan.scroll.x - f32::from(delta.x),
                y: pan.scroll.y - f32::from(delta.y),
            };
            self.scroll_target = self.scroll;
            self.clamp_scroll();
            self.request_visible_tiles(window);
            cx.notify();
            return;
        }

        if self.selecting
            && event.pressed_button == Some(MouseButton::Left)
            && let Some(position) = self.hit_test_text(event.position, true)
        {
            if let Some(selection) = self.selection.as_mut() {
                if selection.focus != position {
                    self.pending_annotation_click = None;
                }
                selection.focus = position;
            }
            let local_y = f32::from(event.position.y) - self.content_top();
            if local_y < 28.0 {
                self.scroll_by(0.0, -28.0, true, window, cx);
            } else if local_y > self.viewport_height - 28.0 {
                self.scroll_by(0.0, 28.0, true, window, cx);
            }
            cx.notify();
        }
    }

    fn on_mouse_up(&mut self, event: &MouseUpEvent, _window: &mut Window, cx: &mut Context<Self>) {
        if event.button == MouseButton::Left
            && let Some(id) = self.pending_annotation_click.take()
        {
            self.active_annotation = Some(id);
            self.selection = None;
        }
        self.selecting = false;
        self.pan = None;
        cx.notify();
    }

    fn hit_test_text(&self, position: Point<Pixels>, nearest: bool) -> Option<TextPosition> {
        let layout = self.layout()?;
        let x = self.scroll.x + f32::from(position.x);
        let y = self.scroll.y + f32::from(position.y) - self.content_top();
        let page = layout.page_at_content_point(x, y).or_else(|| {
            nearest.then(|| {
                layout
                    .visible_pages(self.scroll.y, self.viewport_height, self.viewport_height)
                    .min_by_key(|page| {
                        let rect = layout.page_rect(*page).unwrap();
                        if y < rect.y {
                            (rect.y - y) as i32
                        } else if y > rect.bottom() {
                            (y - rect.bottom()) as i32
                        } else {
                            0
                        }
                    })
            })?
        })?;
        let chars = self.page_text.get(&page)?;
        let page_rect = layout.page_rect(page)?;
        chars
            .hit_test(page_rect, x, y, nearest)
            .map(|index| TextPosition { page, index })
    }

    fn annotation_at_text_position(&self, position: TextPosition) -> Option<AnnotationId> {
        self.annotations.as_ref().and_then(|annotations| {
            annotations
                .at(position)
                .filter(|annotation| {
                    annotation.highlight().is_some() || annotation.comment_markdown().is_some()
                })
                .max_by_key(|annotation| (annotation.updated_revision(), annotation.id()))
                .map(|annotation| annotation.id())
        })
    }

    fn select_word(&mut self, position: TextPosition) {
        let Some(chars) = self.page_text.get(&position.page) else {
            return;
        };
        let Some(clicked) = chars.get(position.index) else {
            return;
        };
        let is_word = |value: char| value.is_alphanumeric() || value == '_' || value == '-';
        let category = is_word(clicked.value);
        let mut start = position.index;
        let mut end = position.index;
        while start > 0 && is_word(chars[start - 1].value) == category {
            start -= 1;
        }
        while end + 1 < chars.len() && is_word(chars[end + 1].value) == category {
            end += 1;
        }
        self.selection = Some(TextSelection {
            anchor: TextPosition {
                page: position.page,
                index: start,
            },
            focus: TextPosition {
                page: position.page,
                index: end,
            },
        });
    }

    fn copy_selection(&mut self, _: &CopySelection, _window: &mut Window, cx: &mut Context<Self>) {
        let Some(selection) = self.selection else {
            return;
        };
        let (start, end) = selection.ordered();
        let Some(last_document_page) = self
            .document
            .as_ref()
            .and_then(|document| document.pages.len().checked_sub(1))
        else {
            return;
        };
        self.warning = None;
        // The worker treats explicit extraction as latest-wins. Mirror that
        // replacement locally so an abandoned page cannot remain marked
        // pending and suppress a future request for the same page.
        self.text_pending.clear();
        let _ = self.worker.cancel_explicit_text(self.generation);
        self.copy_pending = Some(PendingCopy {
            selection,
            next_page: start.page.min(last_document_page),
            end_page: end.page.min(last_document_page),
            text: String::new(),
        });
        self.continue_pending_copy(cx);
        cx.notify();
    }

    fn edit_copy(&mut self, _: &EditCopy, window: &mut Window, cx: &mut Context<Self>) {
        self.copy_selection(&CopySelection, window, cx);
        cx.stop_propagation();
    }

    fn edit_cut(&mut self, _: &EditCut, _: &mut Window, cx: &mut Context<Self>) {
        cx.stop_propagation();
    }

    fn edit_paste(&mut self, _: &EditPaste, _: &mut Window, cx: &mut Context<Self>) {
        cx.stop_propagation();
    }

    fn select_all(&mut self, _: &SelectAll, _window: &mut Window, cx: &mut Context<Self>) {
        let Some(page_count) = self
            .document
            .as_ref()
            .map(|document| document.pages.len())
            .filter(|count| *count > 0)
        else {
            return;
        };
        // Character indices on the end page are clamped against its actual
        // layer when painted or copied, so Select All is O(1) even for a very
        // long document and never queues every page at once.
        self.selection = Some(TextSelection {
            anchor: TextPosition { page: 0, index: 0 },
            focus: TextPosition {
                page: page_count - 1,
                index: usize::MAX,
            },
        });
        cx.notify();
    }

    fn edit_select_all(&mut self, _: &EditSelectAll, window: &mut Window, cx: &mut Context<Self>) {
        self.select_all(&SelectAll, window, cx);
        cx.stop_propagation();
    }

    fn continue_pending_copy(&mut self, cx: &mut Context<Self>) {
        loop {
            let Some(mut pending) = self.copy_pending.take() else {
                return;
            };
            if pending.next_page > pending.end_page {
                if !pending.text.is_empty() {
                    cx.write_to_clipboard(ClipboardItem::new_string(pending.text));
                }
                return;
            }

            let page = pending.next_page;
            let Some(text) = self.page_text.get(&page).cloned() else {
                self.copy_pending = Some(pending);
                if self.text_pending.insert(page)
                    && !self.worker.extract_text(self.generation, page)
                {
                    self.status = ReaderStatus::Error("The PDF renderer is unavailable".into());
                    self.text_pending.remove(&page);
                    self.copy_pending = None;
                }
                return;
            };

            append_selected_page_text(&mut pending.text, pending.selection, page, text.as_slice());
            if pending.text.len() > MAX_COPY_TEXT_BYTES {
                self.warning = Some(
                    "Copy stopped because the selected text exceeds the 64 MiB safety limit".into(),
                );
                return;
            }
            pending.next_page += 1;
            self.copy_pending = Some(pending);
        }
    }

    fn paint_tiles_for_page(
        &self,
        page: usize,
        page_rect: Rect,
        desired: RasterSize,
    ) -> Vec<PaintTile> {
        let visible_exact: Vec<_> = self
            .render_viewport
            .iter()
            .filter_map(|(key, tier)| {
                (key.page == page && *tier == DemandTier::Visible).then_some(*key)
            })
            .collect();
        let exact_complete = !visible_exact.is_empty()
            && visible_exact
                .iter()
                .all(|key| self.rendered.contains_key(key));

        let mut rasters: Vec<_> = self
            .rendered
            .keys()
            .filter_map(|key| (key.page == page && key.raster != desired).then_some(key.raster))
            .collect();
        rasters.sort();
        rasters.dedup();
        let viewport = Rect {
            x: self.scroll.x,
            y: self.scroll.y,
            width: self.viewport_width,
            height: self.viewport_height,
        };
        let fallback = (!exact_complete)
            .then(|| {
                rasters.into_iter().max_by(|a, b| {
                    let coverage = |raster: RasterSize| {
                        self.rendered
                            .iter()
                            .filter(|(key, _)| key.page == page && key.raster == raster)
                            .filter_map(|(_, tile)| {
                                let tile = tile_logical_rect(page_rect, raster, tile.core_rect);
                                intersect_rect(tile, viewport)
                            })
                            .map(|visible| f64::from(visible.width) * f64::from(visible.height))
                            .sum::<f64>()
                    };
                    coverage(*a).total_cmp(&coverage(*b)).then_with(|| {
                        let distance = |raster: RasterSize| {
                            u64::from(raster.width.abs_diff(desired.width))
                                + u64::from(raster.height.abs_diff(desired.height))
                        };
                        distance(*b).cmp(&distance(*a))
                    })
                })
            })
            .flatten();

        let mut layers = Vec::with_capacity(2);
        if let Some(fallback) = fallback {
            layers.push(fallback);
        }
        layers.push(desired);

        let mut result = Vec::new();
        for raster in layers {
            let mut tiles: Vec<_> = self
                .rendered
                .iter()
                .filter(|(key, _)| key.page == page && key.raster == raster)
                .collect();
            tiles.sort_by_key(|(key, _)| **key);
            result.extend(tiles.into_iter().map(|(_, tile)| PaintTile {
                core_rect: tile_logical_rect(page_rect, raster, tile.core_rect),
                render_rect: tile_logical_rect(page_rect, raster, tile.render_rect),
                image: tile.image.clone(),
            }));
        }
        result
    }

    fn status_text(&self) -> SharedString {
        match &self.status {
            ReaderStatus::Initializing => "Starting PDFium…".into(),
            ReaderStatus::Empty => "Open a PDF to begin".into(),
            ReaderStatus::Loading(message) => message.clone(),
            ReaderStatus::Error(message) => message.clone(),
            ReaderStatus::Ready if self.annotations_loading => "Loading annotations…".into(),
            ReaderStatus::Ready if self.annotation_error.is_some() => {
                self.annotation_error.clone().unwrap()
            }
            ReaderStatus::Ready
                if self.annotations.as_ref().is_some_and(|annotations| {
                    annotations.revision() > self.annotation_saved_revision
                }) =>
            {
                "Saving annotations…".into()
            }
            ReaderStatus::Ready if self.copy_pending.is_some() => "Reading text for copy…".into(),
            ReaderStatus::Ready if self.warning.is_some() => self.warning.clone().unwrap(),
            ReaderStatus::Ready if !self.pending.is_empty() => format!(
                "Rendering {} tile{}…",
                self.pending.len(),
                if self.pending.len() == 1 { "" } else { "s" }
            )
            .into(),
            ReaderStatus::Ready => "Ready".into(),
        }
    }

    fn chrome_button(
        palette: ReaderPalette,
        id: &'static str,
        label: impl IntoElement,
        style: ChromeButtonStyle,
        enabled: bool,
        handler: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
    ) -> impl IntoElement {
        let (background, border, text, hover, pressed) = match style {
            ChromeButtonStyle::Neutral => (
                palette.control,
                palette.separator,
                palette.text,
                palette.control_hover,
                palette.control_pressed,
            ),
            ChromeButtonStyle::Ghost => (
                palette.chrome,
                palette.chrome,
                palette.text_secondary,
                palette.control_hover,
                palette.control_pressed,
            ),
            ChromeButtonStyle::Floating => (
                palette.surface,
                palette.surface,
                palette.text_secondary,
                palette.control_hover,
                palette.control_pressed,
            ),
            ChromeButtonStyle::Selected => (
                palette.accent_soft,
                palette.accent_border,
                palette.accent,
                palette.accent_soft_hover,
                palette.control_pressed,
            ),
            ChromeButtonStyle::Primary => (
                palette.accent,
                palette.accent,
                palette.accent_foreground,
                palette.accent_hover,
                palette.accent_active,
            ),
        };
        div()
            .id(id)
            .h(px(32.0))
            .min_w(px(32.0))
            .px_3()
            .flex()
            .items_center()
            .justify_center()
            .overflow_hidden()
            .rounded_md()
            .border_1()
            .border_color(border)
            .bg(background)
            .text_color(text)
            .text_sm()
            .font_weight(FontWeight::MEDIUM)
            .when(enabled, |button| {
                button
                    .cursor_pointer()
                    .hover(move |button| button.bg(hover))
                    .active(move |button| button.bg(pressed))
                    .on_click(handler)
            })
            .when(!enabled, |button| button.opacity(0.42))
            .child(label)
    }

    fn icon_label(icon: IconName, label: impl IntoElement) -> gpui::AnyElement {
        div()
            .flex()
            .items_center()
            .gap_1()
            .child(Icon::new(icon))
            .child(label)
            .into_any_element()
    }

    fn segment_button(
        palette: ReaderPalette,
        id: &'static str,
        label: impl IntoElement,
        enabled: bool,
        handler: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
    ) -> impl IntoElement {
        div()
            .id(id)
            .h(px(30.0))
            .min_w(px(30.0))
            .px_2()
            .flex()
            .items_center()
            .justify_center()
            .overflow_hidden()
            .rounded_md()
            .text_color(palette.text)
            .text_sm()
            .when(enabled, |button| {
                button
                    .cursor_pointer()
                    .hover(|button| button.bg(palette.control_hover))
                    .active(|button| button.bg(palette.control_pressed))
                    .on_click(handler)
            })
            .when(!enabled, |button| button.opacity(0.38))
            .child(label)
    }

    fn highlight_button(
        palette: ReaderPalette,
        id: &'static str,
        color: HighlightColor,
        enabled: bool,
        handler: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
    ) -> impl IntoElement {
        let dot = match color {
            HighlightColor::Yellow => palette.yellow,
            HighlightColor::Green => palette.green,
            HighlightColor::Blue => palette.blue,
            HighlightColor::Pink => palette.pink,
            HighlightColor::Purple => palette.purple,
        };
        div()
            .id(id)
            .size(px(28.0))
            .flex_none()
            .flex()
            .items_center()
            .justify_center()
            .overflow_hidden()
            .rounded_md()
            .when(enabled, |button| {
                button
                    .cursor_pointer()
                    .hover(|button| button.bg(palette.control_hover))
                    .active(|button| button.bg(palette.control_pressed))
                    .on_click(handler)
            })
            .when(!enabled, |button| button.opacity(0.34))
            .child(
                div()
                    .size(px(14.0))
                    .rounded_full()
                    .border_1()
                    .border_color(palette.text.opacity(0.14))
                    .bg(dot),
            )
    }

    fn set_toc_hovered(
        &mut self,
        index: usize,
        hovered: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if hovered {
            if self.toc_hovered.is_none() && self.toc_hover_strength <= 0.002 {
                self.toc_hover_position = index as f32;
            }
            self.toc_hovered = Some(index);
        } else if self.toc_hovered == Some(index) {
            self.toc_hovered = None;
        }
        self.start_animation(window, cx);
    }

    fn navigate_toc(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
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
        self.active_annotation = None;
        self.pending_toc_navigation = None;

        if let Some(destination_y) = destination_y {
            self.scroll_to_toc_destination(page, Some(destination_y), window, cx);
            return;
        }
        if let Some(text) = self.page_text.get(&page) {
            let matched_y = toc_title_match_y(&title, text);
            #[cfg(debug_assertions)]
            if matched_y.is_some() {
                self.qa_toc_text_matches += 1;
            }
            self.scroll_to_toc_destination(page, matched_y, window, cx);
            return;
        }

        self.pending_toc_navigation = Some(index);
        if self.text_pending.insert(page) && !self.worker.extract_text(self.generation, page) {
            self.text_pending.remove(&page);
            self.pending_toc_navigation = None;
            self.scroll_to_toc_destination(page, None, window, cx);
        }
        cx.notify();
    }

    fn complete_pending_toc_navigation(
        &mut self,
        page: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(index) = self.pending_toc_navigation else {
            return;
        };
        let Some((entry_page, title)) = self
            .document
            .as_ref()
            .and_then(|document| document.toc.get(index))
            .map(|entry| (entry.page, entry.title.clone()))
        else {
            self.pending_toc_navigation = None;
            return;
        };
        if entry_page != page {
            return;
        }
        self.pending_toc_navigation = None;
        let matched_y = self
            .page_text
            .get(&page)
            .and_then(|text| toc_title_match_y(&title, text));
        #[cfg(debug_assertions)]
        if matched_y.is_some() {
            self.qa_toc_text_matches += 1;
        }
        self.scroll_to_toc_destination(page, matched_y, window, cx);
    }

    fn scroll_to_toc_destination(
        &mut self,
        page: usize,
        destination_y: Option<f32>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(target) = self.layout().and_then(|layout| {
            toc_scroll_target(layout, page, destination_y, self.viewport_height)
        }) else {
            return;
        };
        self.scroll_target.y = target;
        self.clamp_scroll();
        self.start_animation(window, cx);
        cx.notify();
    }

    fn render_toc_navigation(
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
            Some((
                entry.title.clone(),
                entry.page,
                toc_breadcrumb(&document.toc, index),
                marker_y,
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
        let card_width = (self.viewport_width - TOC_RAIL_WIDTH - 28.0).clamp(190.0, 310.0);
        let detail_card = detail.map(|(title, page, breadcrumb, marker_y)| {
            let card_y = (marker_y - TOC_CARD_HEIGHT * 0.5).clamp(
                10.0,
                (self.viewport_height - TOC_CARD_HEIGHT - 10.0).max(10.0),
            );
            div()
                .id("toc-hover-detail")
                .block_mouse_except_scroll()
                .absolute()
                .top(px(card_y))
                .left(px(TOC_RAIL_WIDTH + 6.0))
                .w(px(card_width))
                .min_h(px(TOC_CARD_HEIGHT))
                .px_4()
                .py_3()
                .overflow_hidden()
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
                .when_some(breadcrumb, |card, breadcrumb| {
                    card.child(
                        div()
                            .mt_1()
                            .whitespace_nowrap()
                            .text_ellipsis()
                            .text_xs()
                            .text_color(palette.text_secondary)
                            .child(breadcrumb),
                    )
                })
                .child(
                    div()
                        .mt_1()
                        .text_xs()
                        .text_color(palette.text_tertiary)
                        .child(format!("Page {}", page + 1)),
                )
        });

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

    fn render_fluid_main_pill(
        &mut self,
        state: FluidPillState,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let palette = ReaderPalette::from_theme(Theme::global(cx));
        let compact = state.available_width < 700.0;
        let has_context = self.context_range().is_some() && self.comment_editor.is_none();
        // When a side panel leaves a narrow document strip, the local pill
        // keeps context actions available and the main pill keeps its global
        // zoom/search/comments controls unclipped.
        let show_context_in_main = has_context && state.available_width >= 520.0;
        let context_actions_enabled =
            has_context && !self.annotations_loading && !self.annotation_persistence_blocked;
        let comment_label = if compact {
            Icon::new(if self.context_has_comment() {
                IconName::BookOpen
            } else {
                IconName::Plus
            })
            .into_any_element()
        } else if self.context_has_comment() {
            "Edit note".into_any_element()
        } else {
            "Add note".into_any_element()
        };

        div()
            .id("fluid-main-pill")
            .block_mouse_except_scroll()
            .h(px(44.0))
            .max_w(px((state.available_width - 24.0).max(280.0)))
            .px_1()
            .flex()
            .items_center()
            .gap_1()
            .overflow_hidden()
            .rounded_full()
            .border_1()
            .border_color(palette.text.opacity(0.13))
            .bg(palette.surface)
            .shadow_sm()
            .text_color(palette.text)
            .child(Self::segment_button(
                palette,
                "fluid-zoom-out",
                Icon::new(IconName::Minus),
                state.zoom_out_enabled,
                cx.listener(|reader, _, window, cx| reader.zoom_out(&ZoomOut, window, cx)),
            ))
            .child(
                div()
                    .h(px(30.0))
                    .min_w(px(if compact { 46.0 } else { 54.0 }))
                    .px_2()
                    .flex()
                    .items_center()
                    .justify_center()
                    .text_sm()
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(if state.document_open {
                        palette.text
                    } else {
                        palette.text_tertiary
                    })
                    .child(state.zoom_label),
            )
            .child(Self::segment_button(
                palette,
                "fluid-zoom-in",
                Icon::new(IconName::Plus),
                state.zoom_in_enabled,
                cx.listener(|reader, _, window, cx| reader.zoom_in(&ZoomIn, window, cx)),
            ))
            .when(!compact, |pill| {
                pill.child(Self::segment_button(
                    palette,
                    "fluid-fit-width",
                    "Fit",
                    state.document_open,
                    cx.listener(|reader, _, window, cx| reader.fit_width(&FitWidth, window, cx)),
                ))
            })
            .when(show_context_in_main, |pill| {
                pill.child(div().h(px(24.0)).w(px(1.0)).bg(palette.separator))
                    .children(
                        [
                            "fluid-main-highlight-yellow",
                            "fluid-main-highlight-green",
                            "fluid-main-highlight-blue",
                            "fluid-main-highlight-pink",
                            "fluid-main-highlight-purple",
                        ]
                        .into_iter()
                        .zip(HighlightColor::ALL)
                        .map(|(id, color)| {
                            Self::highlight_button(
                                palette,
                                id,
                                color,
                                context_actions_enabled,
                                cx.listener(move |reader, _, _, cx| {
                                    reader.add_highlight(color, cx)
                                }),
                            )
                        }),
                    )
                    .child(Self::segment_button(
                        palette,
                        "fluid-main-context-comment",
                        comment_label,
                        context_actions_enabled,
                        cx.listener(|reader, _, window, cx| reader.comment_on_context(window, cx)),
                    ))
            })
            .child(div().h(px(24.0)).w(px(1.0)).bg(palette.separator))
            .child(Self::chrome_button(
                palette,
                "fluid-toggle-search",
                if compact {
                    Icon::new(IconName::Search).into_any_element()
                } else {
                    "Search".into_any_element()
                },
                if state.search_selected {
                    ChromeButtonStyle::Selected
                } else {
                    ChromeButtonStyle::Floating
                },
                state.document_open,
                cx.listener(|reader, _, window, cx| reader.find_document(&Find, window, cx)),
            ))
            .child(Self::chrome_button(
                palette,
                "fluid-toggle-comments",
                if compact {
                    Icon::new(IconName::PanelRight).into_any_element()
                } else {
                    "Comments".into_any_element()
                },
                if state.comments_selected {
                    ChromeButtonStyle::Selected
                } else {
                    ChromeButtonStyle::Floating
                },
                state.document_open,
                cx.listener(|reader, _, window, cx| {
                    reader.toggle_comments(&ToggleComments, window, cx)
                }),
            ))
            .when(Theme::global(cx).is_dark(), |pill| {
                pill.child(Self::segment_button(
                    palette,
                    "fluid-toggle-pdf-dark-mode",
                    Icon::new(if self.pdf_dark_mode_enabled {
                        IconName::Moon
                    } else {
                        IconName::Sun
                    }),
                    true,
                    cx.listener(|reader, _, window, cx| reader.toggle_pdf_dark_mode(window, cx)),
                ))
            })
            .into_any_element()
    }

    fn render_fluid_context_pill(
        &mut self,
        position: Offset,
        enabled: bool,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let palette = ReaderPalette::from_theme(Theme::global(cx));
        let comment_label = if self.context_has_comment() {
            Icon::new(IconName::BookOpen)
        } else {
            Icon::new(IconName::Plus)
        };
        div()
            .id("fluid-context-pill")
            .block_mouse_except_scroll()
            .absolute()
            .left(px(position.x))
            .top(px(position.y))
            .h(px(FLUID_CONTEXT_PILL_HEIGHT))
            .w(px(FLUID_CONTEXT_PILL_WIDTH))
            .px_1()
            .flex()
            .items_center()
            .justify_center()
            .gap_1()
            .rounded_full()
            .border_1()
            .border_color(palette.text.opacity(0.15))
            .bg(palette.surface)
            .shadow_sm()
            .children(
                [
                    "fluid-context-highlight-yellow",
                    "fluid-context-highlight-green",
                    "fluid-context-highlight-blue",
                    "fluid-context-highlight-pink",
                    "fluid-context-highlight-purple",
                ]
                .into_iter()
                .zip(HighlightColor::ALL)
                .map(|(id, color)| {
                    Self::highlight_button(
                        palette,
                        id,
                        color,
                        enabled,
                        cx.listener(move |reader, _, _, cx| reader.add_highlight(color, cx)),
                    )
                }),
            )
            .child(div().h(px(22.0)).w(px(1.0)).bg(palette.separator))
            .child(Self::segment_button(
                palette,
                "fluid-context-comment",
                comment_label,
                enabled,
                cx.listener(|reader, _, window, cx| reader.comment_on_context(window, cx)),
            ))
            .into_any_element()
    }

    fn render_search_panel(&mut self, cx: &mut Context<Self>) -> gpui::AnyElement {
        let palette = ReaderPalette::from_theme(Theme::global(cx));
        let fluid = self.view_mode == ReaderView::Fluid;
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
                        .rounded_full()
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
            uniform_list(
                "search-results",
                result_count,
                cx.processor(move |reader, range: std::ops::Range<usize>, _window, cx| {
                    range
                        .filter_map(|index| {
                            let id = *reader.search.order.get(index)?;
                            let result = reader
                                .search
                                .pages
                                .get(&id.page)?
                                .iter()
                                .find(|result| result.id == id)?;
                            let preview: SharedString = result.preview.clone().into();
                            let active = reader.search.active == Some(id);
                            Some(
                                div().h(px(88.0)).w_full().px_3().py_1().child(
                                    div()
                                        .id(("search-result", index))
                                        .size_full()
                                        .overflow_hidden()
                                        .flex()
                                        .rounded_md()
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
                                                .flex()
                                                .flex_col()
                                                .gap_1()
                                                .child(
                                                    div()
                                                        .flex()
                                                        .items_center()
                                                        .justify_between()
                                                        .text_xs()
                                                        .font_weight(FontWeight::MEDIUM)
                                                        .text_color(if active {
                                                            palette.accent
                                                        } else {
                                                            palette.text_secondary
                                                        })
                                                        .child(format!("PAGE {}", id.page + 1))
                                                        .child(format!("{}", index + 1)),
                                                )
                                                .child(
                                                    div()
                                                        .h(px(40.0))
                                                        .overflow_hidden()
                                                        .text_sm()
                                                        .line_height(px(19.0))
                                                        .text_color(palette.text)
                                                        .child(preview),
                                                ),
                                        ),
                                ),
                            )
                        })
                        .collect::<Vec<_>>()
                }),
            )
            .track_scroll(self.search_list_scroll.clone())
            .flex_1()
            .min_h(px(0.0))
            .w_full()
            .bg(palette.surface_subtle)
            .when(fluid, |list| list.rounded_b_xl())
            .into_any_element()
        };

        div()
            .size_full()
            .flex()
            .flex_col()
            .when(fluid, |panel| panel.rounded_xl())
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
                        "Close",
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

    fn render_fluid_comments_panel(&mut self, cx: &mut Context<Self>) -> gpui::AnyElement {
        let palette = ReaderPalette::from_theme(Theme::global(cx));
        let editor_open = self.comment_editor.is_some();
        let can_add_comment = annotation_actions_enabled(
            self.selection.is_some(),
            self.annotations_loading,
            self.annotation_persistence_blocked,
            editor_open,
        );
        let list_header = div()
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
                    .child("Comments"),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_1()
                    .child(Self::chrome_button(
                        palette,
                        "fluid-add-comment",
                        "+ New",
                        ChromeButtonStyle::Neutral,
                        can_add_comment,
                        cx.listener(|reader, _, window, cx| {
                            reader.add_comment(&AddComment, window, cx)
                        }),
                    ))
                    .child(Self::chrome_button(
                        palette,
                        "fluid-close-comments",
                        "Close",
                        ChromeButtonStyle::Ghost,
                        true,
                        cx.listener(|reader, _, window, cx| {
                            reader.toggle_sidebar(SidePanel::Comments, window, cx)
                        }),
                    )),
            );

        let list_body = if self.comment_order.is_empty() {
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
                        .rounded_full()
                        .bg(palette.accent_soft)
                        .text_color(palette.accent)
                        .text_lg()
                        .child(Icon::new(IconName::BookOpen)),
                )
                .child(
                    div()
                        .text_sm()
                        .font_weight(FontWeight::MEDIUM)
                        .text_color(palette.text)
                        .child(if self.annotations_loading {
                            "Loading comments…"
                        } else if self.annotation_persistence_blocked {
                            "Comments unavailable"
                        } else {
                            "No comments yet"
                        }),
                )
                .child(
                    div()
                        .max_w(px(248.0))
                        .text_xs()
                        .line_height(px(18.0))
                        .text_color(palette.text_secondary)
                        .child(if self.annotation_persistence_blocked {
                            "Resolve the annotation sidecar problem before adding comments."
                        } else {
                            "Select text, then use either floating Note control."
                        }),
                )
                .into_any_element()
        } else {
            uniform_list(
                "fluid-comment-list",
                self.comment_order.len(),
                cx.processor(move |reader, range: std::ops::Range<usize>, _window, cx| {
                    range
                        .filter_map(|index| {
                            let id = *reader.comment_order.get(index)?;
                            let annotation = reader.annotations.as_ref()?.get(id)?;
                            let page = annotation.range().start().page;
                            let markdown = annotation.comment_markdown().unwrap_or("");
                            let preview = RichTextBuffer::try_from_markdown(markdown)
                                .map(|buffer| compact_preview(buffer.text(), 96))
                                .unwrap_or_else(|_| compact_preview(markdown, 96));
                            let preview: SharedString = preview.into();
                            let color = match annotation.highlight() {
                                Some(HighlightColor::Yellow) => palette.yellow,
                                Some(HighlightColor::Green) => palette.green,
                                Some(HighlightColor::Blue) => palette.blue,
                                Some(HighlightColor::Pink) => palette.pink,
                                Some(HighlightColor::Purple) => palette.purple,
                                None => palette.warning,
                            };
                            let active = reader.active_annotation == Some(id);
                            Some(
                                div().h(px(96.0)).w_full().px_3().py_1().child(
                                    div()
                                        .id(("fluid-comment", index))
                                        .size_full()
                                        .overflow_hidden()
                                        .flex()
                                        .rounded_md()
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
                                            reader.open_comment_from_list(id, window, cx);
                                        }))
                                        .child(div().w(px(4.0)).h_full().flex_none().bg(color))
                                        .child(
                                            div()
                                                .flex_1()
                                                .min_w(px(0.0))
                                                .px_3()
                                                .py_2()
                                                .flex()
                                                .flex_col()
                                                .gap_1()
                                                .child(
                                                    div()
                                                        .flex()
                                                        .items_center()
                                                        .justify_between()
                                                        .text_xs()
                                                        .font_weight(FontWeight::MEDIUM)
                                                        .text_color(if active {
                                                            palette.accent
                                                        } else {
                                                            palette.text_secondary
                                                        })
                                                        .child(format!("PAGE {}", page + 1))
                                                        .child(Self::icon_label(
                                                            IconName::ArrowRight,
                                                            "Open",
                                                        )),
                                                )
                                                .child(
                                                    div()
                                                        .h(px(42.0))
                                                        .overflow_hidden()
                                                        .text_sm()
                                                        .line_height(px(20.0))
                                                        .text_color(palette.text)
                                                        .child(preview),
                                                ),
                                        ),
                                ),
                            )
                        })
                        .collect::<Vec<_>>()
                }),
            )
            .track_scroll(self.comment_list_scroll.clone())
            .flex_1()
            .min_h(px(0.0))
            .w_full()
            .bg(palette.surface_subtle)
            .rounded_b_xl()
            .into_any_element()
        };

        let list_error = self.annotation_error.clone().map(|message| {
            div()
                .mx_4()
                .mt_3()
                .p_3()
                .rounded_md()
                .bg(palette.error_soft)
                .text_xs()
                .line_height(px(18.0))
                .text_color(palette.error)
                .child(message)
        });
        let progress = self.comment_pane.progress;
        let list_pane = div()
            .absolute()
            .top_0()
            .bottom_0()
            .left(px(-SIDEBAR_WIDTH * progress))
            .w_full()
            .flex()
            .flex_col()
            .rounded_xl()
            .bg(palette.surface)
            .child(list_header)
            .children(list_error)
            .child(list_body);

        let editor_pane = self.comment_editor.clone().map(|editor| {
            let title = if self.editing_annotation.is_some() {
                "Edit Comment"
            } else {
                "New Comment"
            };
            let annotations_pending = self
                .annotations
                .as_ref()
                .is_some_and(|annotations| annotations.revision() > self.annotation_saved_revision);
            let save_status = if self.comment_draft_dirty
                || self.comment_autosave_task.is_some()
                || annotations_pending
            {
                "Saving…"
            } else if self.editing_annotation.is_some() {
                "Saved"
            } else {
                "Auto-save"
            };
            let editor_error = self.annotation_error.clone().map(|message| {
                div()
                    .mx_4()
                    .mt_3()
                    .p_3()
                    .rounded_md()
                    .bg(palette.error_soft)
                    .text_xs()
                    .line_height(px(18.0))
                    .text_color(palette.error)
                    .child(message)
            });
            div()
                .absolute()
                .top_0()
                .bottom_0()
                .left(px(SIDEBAR_WIDTH * (1.0 - progress)))
                .w_full()
                .flex()
                .flex_col()
                .rounded_xl()
                .bg(palette.surface)
                .child(
                    div()
                        .h(px(54.0))
                        .flex_none()
                        .px_3()
                        .flex()
                        .items_center()
                        .justify_between()
                        .border_b_1()
                        .border_color(palette.separator)
                        .child(Self::chrome_button(
                            palette,
                            "fluid-comment-back",
                            Self::icon_label(IconName::ChevronLeft, "Back"),
                            ChromeButtonStyle::Ghost,
                            true,
                            cx.listener(|reader, _, window, cx| {
                                reader.return_to_comment_list(window, cx)
                            }),
                        ))
                        .child(
                            div()
                                .flex_1()
                                .min_w(px(0.0))
                                .px_2()
                                .flex()
                                .flex_col()
                                .items_center()
                                .child(
                                    div()
                                        .max_w(px(150.0))
                                        .overflow_hidden()
                                        .whitespace_nowrap()
                                        .text_ellipsis()
                                        .text_sm()
                                        .font_weight(FontWeight::SEMIBOLD)
                                        .child(title),
                                )
                                .child(
                                    div()
                                        .text_xs()
                                        .text_color(if save_status == "Saved" {
                                            palette.green
                                        } else {
                                            palette.text_secondary
                                        })
                                        .child(save_status),
                                ),
                        )
                        .child(Self::chrome_button(
                            palette,
                            "fluid-close-comment-editor",
                            "Close",
                            ChromeButtonStyle::Ghost,
                            true,
                            cx.listener(|reader, _, window, cx| {
                                reader.toggle_sidebar(SidePanel::Comments, window, cx)
                            }),
                        )),
                )
                .children(editor_error)
                .child(
                    div()
                        .flex_1()
                        .min_h(px(0.0))
                        .p_4()
                        .flex()
                        .flex_col()
                        .child(editor),
                )
        });

        div()
            .relative()
            .size_full()
            .overflow_hidden()
            .rounded_xl()
            .bg(palette.surface)
            .text_color(palette.text)
            .child(list_pane)
            .children(editor_pane)
            .into_any_element()
    }

    fn render_comments_panel(&mut self, cx: &mut Context<Self>) -> gpui::AnyElement {
        match self.view_mode {
            ReaderView::Classic => self.render_classic_comments_panel(cx),
            ReaderView::Fluid => self.render_fluid_comments_panel(cx),
        }
    }

    fn render_classic_comments_panel(&mut self, cx: &mut Context<Self>) -> gpui::AnyElement {
        let palette = ReaderPalette::from_theme(Theme::global(cx));
        let error = self.annotation_error.clone();
        let editor_open = self.comment_editor.is_some();
        let can_add_comment = annotation_actions_enabled(
            self.context_range().is_some(),
            self.annotations_loading,
            self.annotation_persistence_blocked,
            editor_open,
        );
        let context_has_comment = self.context_has_comment();
        let annotations_pending = self
            .annotations
            .as_ref()
            .is_some_and(|annotations| annotations.revision() > self.annotation_saved_revision);
        let save_status = if self.comment_draft_dirty
            || self.comment_autosave_task.is_some()
            || annotations_pending
        {
            "Saving…"
        } else if editor_open && self.editing_annotation.is_some() {
            "Saved"
        } else {
            "Auto-save"
        };
        let title = if editor_open {
            if self.editing_annotation.is_some() {
                "Edit Comment"
            } else {
                "New Comment"
            }
        } else {
            "Comments"
        };
        let header = div()
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
                    .flex()
                    .items_center()
                    .gap_2()
                    .when(editor_open, |heading| {
                        heading.child(Self::chrome_button(
                            palette,
                            "classic-comment-back",
                            Self::icon_label(IconName::ChevronLeft, "Back"),
                            ChromeButtonStyle::Ghost,
                            true,
                            cx.listener(|reader, _, window, cx| {
                                reader.return_to_comment_list(window, cx)
                            }),
                        ))
                    })
                    .child(
                        div()
                            .text_lg()
                            .font_weight(FontWeight::SEMIBOLD)
                            .child(title),
                    )
                    .when(editor_open, |heading| {
                        heading.child(
                            div()
                                .px_2()
                                .py_1()
                                .rounded_full()
                                .bg(if save_status == "Saved" {
                                    palette.green.opacity(0.12)
                                } else {
                                    palette.accent_soft
                                })
                                .text_xs()
                                .font_weight(FontWeight::MEDIUM)
                                .text_color(if save_status == "Saved" {
                                    palette.green
                                } else {
                                    palette.accent
                                })
                                .child(save_status),
                        )
                    }),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_1()
                    .when(!editor_open, |actions| {
                        actions.child(Self::chrome_button(
                            palette,
                            "add-comment",
                            if context_has_comment { "Edit" } else { "+ New" },
                            ChromeButtonStyle::Neutral,
                            can_add_comment,
                            cx.listener(|reader, _, window, cx| {
                                reader.comment_on_context(window, cx)
                            }),
                        ))
                    })
                    .child(Self::chrome_button(
                        palette,
                        "close-comments",
                        "Close",
                        ChromeButtonStyle::Ghost,
                        true,
                        cx.listener(|reader, _, window, cx| {
                            reader.toggle_sidebar(SidePanel::Comments, window, cx)
                        }),
                    )),
            );

        let body = if let Some(editor) = self.comment_editor.clone() {
            div()
                .flex_1()
                .min_h(px(0.0))
                .p_4()
                .flex()
                .flex_col()
                .child(editor)
                .into_any_element()
        } else if self.comment_order.is_empty() {
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
                        .rounded_full()
                        .bg(palette.accent_soft)
                        .text_color(palette.accent)
                        .text_lg()
                        .child(Icon::new(IconName::BookOpen)),
                )
                .child(
                    div()
                        .text_sm()
                        .font_weight(FontWeight::MEDIUM)
                        .text_color(palette.text)
                        .child(if self.annotations_loading {
                            "Loading comments…"
                        } else if self.annotation_persistence_blocked {
                            "Comments unavailable"
                        } else {
                            "No comments yet"
                        }),
                )
                .child(
                    div()
                        .max_w(px(248.0))
                        .text_xs()
                        .line_height(px(18.0))
                        .text_color(palette.text_secondary)
                        .child(if self.annotation_persistence_blocked {
                            "Resolve the annotation sidecar problem before adding comments."
                        } else {
                            "Select text in the document, then choose New Comment."
                        }),
                )
                .into_any_element()
        } else {
            uniform_list(
                "comment-list",
                self.comment_order.len(),
                cx.processor(move |reader, range: std::ops::Range<usize>, _window, cx| {
                    range
                        .filter_map(|index| {
                            let id = *reader.comment_order.get(index)?;
                            let annotation = reader.annotations.as_ref()?.get(id)?;
                            let page = annotation.range().start().page;
                            let markdown = annotation.comment_markdown().unwrap_or("");
                            let preview = RichTextBuffer::try_from_markdown(markdown)
                                .map(|buffer| compact_preview(buffer.text(), 96))
                                .unwrap_or_else(|_| compact_preview(markdown, 96));
                            let preview: SharedString = preview.into();
                            let color = match annotation.highlight() {
                                Some(HighlightColor::Yellow) => palette.yellow,
                                Some(HighlightColor::Green) => palette.green,
                                Some(HighlightColor::Blue) => palette.blue,
                                Some(HighlightColor::Pink) => palette.pink,
                                Some(HighlightColor::Purple) => palette.purple,
                                None => palette.warning,
                            };
                            let active = reader.active_annotation == Some(id);
                            Some(
                                div().h(px(104.0)).w_full().px_3().py_1().child(
                                    div()
                                        .id(("comment", index))
                                        .size_full()
                                        .overflow_hidden()
                                        .flex()
                                        .rounded_md()
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
                                            reader.open_comment_from_list(id, window, cx);
                                        }))
                                        .child(div().w(px(4.0)).h_full().flex_none().bg(color))
                                        .child(
                                            div()
                                                .flex_1()
                                                .min_w(px(0.0))
                                                .px_3()
                                                .py_2()
                                                .flex()
                                                .flex_col()
                                                .gap_1()
                                                .child(
                                                    div()
                                                        .flex()
                                                        .items_center()
                                                        .justify_between()
                                                        .text_xs()
                                                        .font_weight(FontWeight::MEDIUM)
                                                        .child(
                                                            div()
                                                                .text_color(if active {
                                                                    palette.accent
                                                                } else {
                                                                    palette.text_secondary
                                                                })
                                                                .child(format!(
                                                                    "PAGE {}",
                                                                    page + 1
                                                                )),
                                                        )
                                                        .child(
                                                            div()
                                                                .id(("edit-comment", index))
                                                                .px_2()
                                                                .py_1()
                                                                .overflow_hidden()
                                                                .rounded_md()
                                                                .text_color(palette.accent)
                                                                .hover(|button| {
                                                                    button.bg(palette.accent_soft)
                                                                })
                                                                .on_click(cx.listener(
                                                                    move |reader, _, window, cx| {
                                                                        reader.edit_comment(
                                                                            id, window, cx,
                                                                        )
                                                                    },
                                                                ))
                                                                .child("Edit"),
                                                        ),
                                                )
                                                .child(
                                                    div()
                                                        .h(px(44.0))
                                                        .overflow_hidden()
                                                        .text_sm()
                                                        .line_height(px(20.0))
                                                        .text_color(palette.text)
                                                        .child(preview),
                                                ),
                                        ),
                                ),
                            )
                        })
                        .collect::<Vec<_>>()
                }),
            )
            .track_scroll(self.comment_list_scroll.clone())
            .flex_1()
            .min_h(px(0.0))
            .w_full()
            .bg(palette.surface_subtle)
            .into_any_element()
        };

        div()
            .size_full()
            .flex()
            .flex_col()
            .bg(palette.surface)
            .text_color(palette.text)
            .child(header)
            .children(error.map(|message| {
                div()
                    .mx_4()
                    .mt_3()
                    .p_3()
                    .rounded_md()
                    .bg(palette.error_soft)
                    .text_xs()
                    .line_height(px(18.0))
                    .text_color(palette.error)
                    .child(message)
            }))
            .child(body)
            .into_any_element()
    }

    fn paint_document(snapshot: PaintSnapshot, bounds: Bounds<Pixels>, window: &mut Window) {
        let palette = snapshot.palette;
        let content_viewport = Rect {
            x: snapshot.scroll.x,
            y: snapshot.scroll.y,
            width: snapshot.viewport_width,
            height: snapshot.viewport_height,
        };
        let mut pages = snapshot.pages;
        pages.sort_by_key(|page| {
            let has_active_annotation = page
                .annotations
                .iter()
                .any(|annotation| Some(annotation.id) == snapshot.active_annotation);
            let has_active_search = snapshot
                .active_search
                .is_some_and(|active| active.page == page.index);
            !(has_active_annotation || has_active_search)
        });
        let mut annotation_budget = PaintBudget::new(MAX_VISIBLE_ANNOTATION_QUADS);
        let mut search_budget = PaintBudget::new(MAX_VISIBLE_SEARCH_HIGHLIGHT_RUNS);
        let mut selection_budget = PaintBudget::new(MAX_VISIBLE_SELECTION_QUADS);
        for page in pages {
            let rect = page.rect;
            let page_bounds = Bounds::new(
                point(
                    bounds.left() + px(rect.x - snapshot.scroll.x),
                    bounds.top() + px(rect.y - snapshot.scroll.y),
                ),
                size(px(rect.width), px(rect.height)),
            );
            let shadow_bounds = Bounds::new(
                page_bounds.origin + point(px(0.0), px(4.0)),
                page_bounds.size + size(px(0.0), px(8.0)),
            );
            window.paint_quad(quad(
                shadow_bounds,
                px(5.0),
                palette.overlay.opacity(0.32),
                px(0.0),
                gpui::transparent_black(),
                Default::default(),
            ));
            window.paint_quad(quad(
                page_bounds,
                px(2.0),
                palette.paper,
                px(1.0),
                palette.paper_border,
                Default::default(),
            ));

            for tile in page.tiles {
                let render_bounds =
                    content_rect_to_bounds(bounds, tile.render_rect, snapshot.scroll);
                let core_bounds = content_rect_to_bounds(bounds, tile.core_rect, snapshot.scroll);
                window.with_content_mask(
                    Some(ContentMask {
                        bounds: core_bounds,
                    }),
                    |window| {
                        let _ = window.paint_image(
                            render_bounds,
                            Corners::default(),
                            tile.image,
                            0,
                            false,
                        );
                    },
                );
            }

            if let Some(chars) = page.text.as_ref() {
                window.with_content_mask(
                    Some(ContentMask {
                        bounds: page_bounds,
                    }),
                    |window| {
                        for annotation in &page.annotations {
                            if annotation_budget.exhausted() {
                                break;
                            }
                            let Some(range) =
                                annotation.range.indices_on_page(page.index, chars.len())
                            else {
                                continue;
                            };
                            let active = Some(annotation.id) == snapshot.active_annotation;
                            let color = match annotation.color {
                                Some(HighlightColor::Yellow) if active => {
                                    palette.yellow.opacity(0.53)
                                }
                                Some(HighlightColor::Yellow) => palette.yellow.opacity(0.38),
                                Some(HighlightColor::Green) if active => {
                                    palette.green.opacity(0.53)
                                }
                                Some(HighlightColor::Green) => palette.green.opacity(0.38),
                                Some(HighlightColor::Blue) if active => palette.blue.opacity(0.53),
                                Some(HighlightColor::Blue) => palette.blue.opacity(0.38),
                                Some(HighlightColor::Pink) if active => palette.pink.opacity(0.53),
                                Some(HighlightColor::Pink) => palette.pink.opacity(0.38),
                                Some(HighlightColor::Purple) if active => {
                                    palette.purple.opacity(0.53)
                                }
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
                                    if !annotation_budget.take() {
                                        return false;
                                    }
                                    window.paint_quad(quad(
                                        content_rect_to_bounds(bounds, highlight, snapshot.scroll),
                                        px(1.0),
                                        color,
                                        px(0.0),
                                        gpui::transparent_black(),
                                        Default::default(),
                                    ));
                                    !annotation_budget.exhausted()
                                },
                            );
                            if !exhausted {
                                break;
                            }
                        }
                    },
                );
            }

            if let Some(matches) = page.search.as_ref() {
                window.with_content_mask(
                    Some(ContentMask {
                        bounds: page_bounds,
                    }),
                    |window| {
                        let active = snapshot.active_search;
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
                                if search_budget.exhausted() {
                                    return;
                                }
                                let highlight = normalized_bounds_in_page(rect, *run);
                                if !highlight.intersects(content_viewport) {
                                    continue;
                                }
                                let painted = search_budget.take();
                                debug_assert!(painted, "exhaustion checked above");
                                window.paint_quad(quad(
                                    content_rect_to_bounds(bounds, highlight, snapshot.scroll),
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

            if let (Some(selection), Some(chars)) = (snapshot.selection, page.text)
                && !selection_budget.exhausted()
                && let Some(range) = selection.indices_on_page(page.index, chars.len())
            {
                // Dense text pages are indexed once on the worker. Paint only
                // selected glyphs that intersect this frame's viewport, and
                // clip malformed PDF geometry to the physical page.
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
                                if !selection_budget.take() {
                                    return false;
                                }
                                let highlight_bounds =
                                    content_rect_to_bounds(bounds, highlight, snapshot.scroll);
                                window.paint_quad(quad(
                                    highlight_bounds,
                                    px(1.0),
                                    palette.selection,
                                    px(0.0),
                                    gpui::transparent_black(),
                                    Default::default(),
                                ));
                                !selection_budget.exhausted()
                            },
                        );
                    },
                );
            }
        }

        let max_y = (snapshot.layout.content_height - snapshot.viewport_height).max(0.0);
        if max_y > 0.0 {
            let thumb_height = (snapshot.viewport_height * snapshot.viewport_height
                / snapshot.layout.content_height)
                .max(38.0)
                .min(snapshot.viewport_height);
            let travel = snapshot.viewport_height - thumb_height - 8.0;
            let y = 4.0 + travel * (snapshot.scroll.y / max_y);
            let thumb = Bounds::new(
                point(bounds.right() - px(8.0), bounds.top() + px(y)),
                size(px(5.0), px(thumb_height)),
            );
            window.paint_quad(quad(
                thumb,
                px(2.5),
                palette.text_secondary.opacity(0.56),
                px(0.0),
                gpui::transparent_black(),
                Default::default(),
            ));
        }

        let max_x = snapshot.max_scroll_x;
        if max_x > 0.0 {
            let effective_content_width = snapshot.viewport_width + max_x;
            let thumb_width = (snapshot.viewport_width * snapshot.viewport_width
                / effective_content_width)
                .max(38.0)
                .min(snapshot.viewport_width);
            let travel = snapshot.viewport_width - thumb_width - 12.0;
            let x = 4.0 + travel * (snapshot.scroll.x / max_x);
            let thumb = Bounds::new(
                point(bounds.left() + px(x), bounds.bottom() - px(8.0)),
                size(px(thumb_width), px(5.0)),
            );
            window.paint_quad(quad(
                thumb,
                px(2.5),
                palette.text_secondary.opacity(0.56),
                px(0.0),
                gpui::transparent_black(),
                Default::default(),
            ));
        }
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

        let content = if let Some(layout) = self.layout() {
            let visible = layout.visible_pages(
                self.scroll.y,
                self.viewport_height,
                self.viewport_height * 0.2,
            );
            let pages = visible
                .filter_map(|index| {
                    let rect = layout.page_rect(index)?;
                    let desired = desired_raster_size(rect, window.scale_factor());
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
                    Some(PaintPage {
                        index,
                        rect,
                        tiles: self.paint_tiles_for_page(index, rect, desired),
                        text: self.page_text.get(&index).cloned(),
                        annotations: paint_annotations,
                        search: self.search.pages.get(&index).cloned(),
                    })
                })
                .collect();
            let snapshot = PaintSnapshot {
                palette,
                layout: self.layout.as_ref().unwrap().clone(),
                pages,
                scroll: self.scroll,
                selection: self.selection,
                active_annotation: self.active_annotation,
                active_search: self.search.active,
                viewport_width: self.viewport_width,
                viewport_height: self.viewport_height,
                max_scroll_x: self.max_scroll_x(layout),
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
                } else {
                    CursorStyle::IBeam
                })
                .on_scroll_wheel(cx.listener(Self::on_scroll_wheel))
                .on_mouse_down(MouseButton::Left, cx.listener(Self::on_mouse_down))
                .on_mouse_down(MouseButton::Middle, cx.listener(Self::on_mouse_down))
                .on_mouse_move(cx.listener(Self::on_mouse_move))
                .on_mouse_up(MouseButton::Left, cx.listener(Self::on_mouse_up))
                .on_mouse_up(MouseButton::Middle, cx.listener(Self::on_mouse_up))
                .on_mouse_up_out(MouseButton::Left, cx.listener(Self::on_mouse_up))
                .on_mouse_up_out(MouseButton::Middle, cx.listener(Self::on_mouse_up))
                .child(
                    canvas(
                        |_, _, _| (),
                        move |bounds, _, window, _| Self::paint_document(snapshot, bounds, window),
                    )
                    .size_full(),
                )
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
                    .flex_1()
                    .min_h(px(0.0))
                    .w_full()
                    .flex()
                    .child(content)
                    .child(sidebar)
                    .into_any_element()
            }
            ReaderView::Fluid => {
                let panel_width = self.fluid_panel_width();
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
                    .w(px(panel_reveal))
                    .overflow_hidden()
                    .child(
                        div()
                            .id("reader-sidebar")
                            .occlude()
                            .absolute()
                            .top(px(FLUID_PANEL_VERTICAL_MARGIN))
                            .bottom(px(FLUID_PANEL_VERTICAL_MARGIN))
                            .right(px(FLUID_PANEL_HORIZONTAL_MARGIN))
                            .w(px(panel_width))
                            .overflow_hidden()
                            .rounded_xl()
                            .border_1()
                            .border_color(palette.text.opacity(0.13))
                            .bg(palette.surface)
                            .shadow_sm()
                            .child(sidebar_content),
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

        div()
            .key_context("PdfReader")
            .track_focus(&self.focus_handle)
            .size_full()
            .flex()
            .flex_col()
            .bg(palette.canvas)
            .on_action(cx.listener(Self::open_dialog))
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
            .on_action(cx.listener(Self::select_theme))
            .on_action(cx.listener(Self::quit_application))
            .child(toolbar)
            .children(error_bar)
            .child(workspace)
    }
}

#[derive(Clone)]
struct PaintPage {
    index: usize,
    rect: Rect,
    tiles: Vec<PaintTile>,
    text: Option<Arc<TextLayer>>,
    annotations: Vec<PaintAnnotation>,
    search: Option<Arc<[SearchMatch]>>,
}

#[derive(Clone, Copy)]
struct PaintAnnotation {
    id: AnnotationId,
    range: TextRange,
    color: Option<HighlightColor>,
    has_comment: bool,
}

#[derive(Clone)]
struct PaintTile {
    core_rect: Rect,
    render_rect: Rect,
    image: Arc<RenderImage>,
}

#[derive(Clone)]
struct PaintSnapshot {
    palette: ReaderPalette,
    layout: Arc<DocumentLayout>,
    pages: Vec<PaintPage>,
    scroll: Offset,
    selection: Option<TextSelection>,
    active_annotation: Option<AnnotationId>,
    active_search: Option<SearchMatchId>,
    viewport_width: f32,
    viewport_height: f32,
    max_scroll_x: f32,
}

fn normalized_bounds_in_page(page: Rect, bounds: TextBounds) -> Rect {
    Rect {
        x: page.x + bounds.left * page.width,
        y: page.y + bounds.top * page.height,
        width: (bounds.right - bounds.left).max(0.0) * page.width,
        height: (bounds.bottom - bounds.top).max(0.0) * page.height,
    }
}

fn compact_preview(text: &str, max_characters: usize) -> String {
    let mut result = String::new();
    let mut previous_space = true;
    let mut character_count = 0;
    for value in text.chars() {
        if value.is_whitespace() {
            if !previous_space {
                result.push(' ');
                previous_space = true;
                character_count += 1;
            }
        } else {
            if character_count >= max_characters {
                break;
            }
            result.push(value);
            previous_space = false;
            character_count += 1;
        }
    }
    while result.ends_with(' ') {
        result.pop();
    }
    if text.chars().filter(|value| !value.is_whitespace()).count()
        > result
            .chars()
            .filter(|value| !value.is_whitespace())
            .count()
    {
        result.push('…');
    }
    result
}

fn desired_raster_size(page_rect: Rect, scale_factor: f32) -> RasterSize {
    if !page_rect.width.is_finite()
        || !page_rect.height.is_finite()
        || page_rect.width <= 0.0
        || page_rect.height <= 0.0
    {
        return RasterSize {
            width: 1,
            height: 1,
        };
    }
    let scale_factor = if scale_factor.is_finite() && scale_factor > 0.0 {
        scale_factor
    } else {
        1.0
    };
    let aspect = f64::from(page_rect.height) / f64::from(page_rect.width);
    let width_cap =
        ((f64::from(MAX_RASTER_DIMENSION) / aspect).floor() as u32).clamp(1, MAX_RASTER_DIMENSION);
    let raw_width = (f64::from(page_rect.width) * f64::from(scale_factor)).ceil();
    let raw_width = if raw_width.is_finite() {
        raw_width.clamp(1.0, f64::from(width_cap)) as u32
    } else {
        width_cap
    };
    let width = raw_width
        .div_ceil(RENDER_QUANTUM)
        .saturating_mul(RENDER_QUANTUM)
        .min(width_cap)
        .max(1);
    let height = (aspect * f64::from(width))
        .round()
        .clamp(1.0, f64::from(MAX_RASTER_DIMENSION)) as u32;
    RasterSize { width, height }
}

fn plan_visible_tiles(
    layout: &DocumentLayout,
    page_sizes: &[PageSize],
    scroll: Offset,
    viewport_width: f32,
    viewport_height: f32,
    scale_factor: f32,
) -> Vec<PlannedTile> {
    if viewport_width <= 0.0
        || viewport_height <= 0.0
        || !viewport_width.is_finite()
        || !viewport_height.is_finite()
    {
        return Vec::new();
    }
    let scale_factor = if scale_factor.is_finite() && scale_factor > 0.0 {
        scale_factor
    } else {
        1.0
    };
    let viewport = Rect {
        x: scroll.x,
        y: scroll.y,
        width: viewport_width,
        height: viewport_height,
    };
    let vertical_overscan = TILE_SIZE as f32 / scale_factor;
    let viewport_center = (
        f64::from(viewport.x + viewport.width * 0.5),
        f64::from(viewport.y + viewport.height * 0.5),
    );
    let mut result = Vec::new();

    for page in layout.visible_pages(scroll.y, viewport_height, vertical_overscan) {
        let (Some(page_rect), Some(_)) = (layout.page_rect(page), page_sizes.get(page)) else {
            continue;
        };
        let raster = desired_raster_size(page_rect, scale_factor);
        let tile_logical_width = page_rect.width * TILE_SIZE as f32 / raster.width as f32;
        let tile_logical_height = page_rect.height * TILE_SIZE as f32 / raster.height as f32;
        let expanded = Rect {
            x: viewport.x - tile_logical_width,
            y: viewport.y - tile_logical_height,
            width: viewport.width + tile_logical_width * 2.0,
            height: viewport.height + tile_logical_height * 2.0,
        };
        let Some(intersection) = intersect_rect(page_rect, expanded) else {
            continue;
        };

        let pixel_x = |content_x: f32, round_up: bool| {
            let value = ((content_x - page_rect.x) / page_rect.width * raster.width as f32)
                .clamp(0.0, raster.width as f32);
            (if round_up {
                value.ceil()
            } else {
                value.floor()
            }) as u32
        };
        let pixel_y = |content_y: f32, round_up: bool| {
            let value = ((content_y - page_rect.y) / page_rect.height * raster.height as f32)
                .clamp(0.0, raster.height as f32);
            (if round_up {
                value.ceil()
            } else {
                value.floor()
            }) as u32
        };
        let left = pixel_x(intersection.x, false);
        let right = pixel_x(intersection.right(), true);
        let top = pixel_y(intersection.y, false);
        let bottom = pixel_y(intersection.bottom(), true);
        if left >= right || top >= bottom {
            continue;
        }
        let first_column = left / TILE_SIZE;
        let last_column = (right - 1) / TILE_SIZE;
        let first_row = top / TILE_SIZE;
        let last_row = (bottom - 1) / TILE_SIZE;

        for row in first_row..=last_row {
            for column in first_column..=last_column {
                let key = TileKey {
                    page,
                    raster,
                    column,
                    row,
                };
                let Some(core_rect) = tile_core_rect(key) else {
                    continue;
                };
                let logical = tile_logical_rect(page_rect, raster, core_rect);
                let tier = if intersect_rect(logical, viewport).is_some() {
                    DemandTier::Visible
                } else {
                    DemandTier::Prefetch
                };
                let center_x = f64::from(logical.x + logical.width * 0.5);
                let center_y = f64::from(logical.y + logical.height * 0.5);
                let distance = ((center_x - viewport_center.0).powi(2)
                    + (center_y - viewport_center.1).powi(2))
                .round()
                .clamp(0.0, u64::MAX as f64) as u64;
                result.push(PlannedTile {
                    request: TileRequest {
                        key,
                        core_rect,
                        render_rect: inflate_tile_rect(core_rect, raster),
                    },
                    tier,
                    distance,
                });
            }
        }
    }
    result
}

fn tile_core_rect(key: TileKey) -> Option<PixelRect> {
    let x = key.column.checked_mul(TILE_SIZE)?;
    let y = key.row.checked_mul(TILE_SIZE)?;
    if x >= key.raster.width || y >= key.raster.height {
        return None;
    }
    Some(PixelRect {
        x,
        y,
        width: TILE_SIZE.min(key.raster.width - x),
        height: TILE_SIZE.min(key.raster.height - y),
    })
}

fn inflate_tile_rect(core: PixelRect, raster: RasterSize) -> PixelRect {
    let x = core.x.saturating_sub(TILE_BLEED);
    let y = core.y.saturating_sub(TILE_BLEED);
    let right = core
        .x
        .saturating_add(core.width)
        .saturating_add(TILE_BLEED)
        .min(raster.width);
    let bottom = core
        .y
        .saturating_add(core.height)
        .saturating_add(TILE_BLEED)
        .min(raster.height);
    PixelRect {
        x,
        y,
        width: right - x,
        height: bottom - y,
    }
}

fn tile_logical_rect(page: Rect, raster: RasterSize, pixels: PixelRect) -> Rect {
    let x0 =
        f64::from(page.x) + f64::from(page.width) * f64::from(pixels.x) / f64::from(raster.width);
    let x1 = f64::from(page.x)
        + f64::from(page.width) * f64::from(pixels.x.saturating_add(pixels.width))
            / f64::from(raster.width);
    let y0 =
        f64::from(page.y) + f64::from(page.height) * f64::from(pixels.y) / f64::from(raster.height);
    let y1 = f64::from(page.y)
        + f64::from(page.height) * f64::from(pixels.y.saturating_add(pixels.height))
            / f64::from(raster.height);
    Rect {
        x: x0 as f32,
        y: y0 as f32,
        width: (x1 - x0) as f32,
        height: (y1 - y0) as f32,
    }
}

fn intersect_rect(a: Rect, b: Rect) -> Option<Rect> {
    let left = a.x.max(b.x);
    let top = a.y.max(b.y);
    let right = a.right().min(b.right());
    let bottom = a.bottom().min(b.bottom());
    (left < right && top < bottom).then_some(Rect {
        x: left,
        y: top,
        width: right - left,
        height: bottom - top,
    })
}

fn command_wheel_zoom_factor(delta_y: f32) -> Option<f32> {
    delta_y
        .is_finite()
        .then(|| (delta_y / 420.0).clamp(-1.5, 1.5).exp())
}

fn tile_distance_from_viewport(
    key: TileKey,
    layout: Option<&DocumentLayout>,
    scroll: Offset,
    viewport_width: f32,
    viewport_height: f32,
) -> u64 {
    let Some(logical) = layout
        .and_then(|layout| layout.page_rect(key.page))
        .zip(tile_core_rect(key))
        .map(|(page, core)| tile_logical_rect(page, key.raster, core))
    else {
        return u64::MAX;
    };
    let dx = f64::from(logical.x + logical.width * 0.5 - scroll.x - viewport_width * 0.5);
    let dy = f64::from(logical.y + logical.height * 0.5 - scroll.y - viewport_height * 0.5);
    (dx.powi(2) + dy.powi(2))
        .round()
        .clamp(0.0, u64::MAX as f64) as u64
}

fn content_rect_to_bounds(canvas: Bounds<Pixels>, rect: Rect, scroll: Offset) -> Bounds<Pixels> {
    Bounds::new(
        point(
            canvas.left() + px(rect.x - scroll.x),
            canvas.top() + px(rect.y - scroll.y),
        ),
        size(px(rect.width), px(rect.height)),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui_component::{ThemeColor, ThemeMode};

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
            toc_breadcrumb(&entries, 1).as_deref(),
            Some("Document  ›  Part one  ›  Details")
        );
        assert_eq!(
            toc_breadcrumb(&entries, 2).as_deref(),
            Some("Document  ›  Part two")
        );

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
        assert_eq!(
            toc_scroll_target(&layout, 2, None, 600.0),
            Some(layout.page_rect(2).unwrap().y)
        );
        let page = layout.page_rect(1).unwrap();
        assert_eq!(
            toc_scroll_target(&layout, 1, Some(0.5), 600.0),
            Some(page.y + page.height * 0.5 - TOC_DESTINATION_CONTEXT)
        );
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
        assert!(
            (toc_title_match_y("methods", &text).expect("heading should match") - 0.62).abs()
                < 0.001
        );
        assert_eq!(toc_title_match_y("missing heading", &text), None);
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
        assert_eq!(load_sidecar(&pdf, &identity).unwrap(), revision_two);
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
        assert_eq!(load_sidecar(&pdf, &identity).unwrap(), first_latest);
    }

    #[test]
    fn pdf_file_lock_serializes_the_sidecar_compare_and_replace_section() {
        let directory = TestDirectory::new("sidecar-file-lock");
        let pdf = directory.path().join("document.pdf");
        std::fs::write(&pdf, b"stable pdf bytes").unwrap();
        let first = File::open(&pdf).unwrap();
        let second = File::open(&pdf).unwrap();

        first.lock().unwrap();
        assert!(matches!(
            second.try_lock(),
            Err(std::fs::TryLockError::WouldBlock)
        ));
        first.unlock().unwrap();
        second.lock().unwrap();
        second.unlock().unwrap();
    }
}
