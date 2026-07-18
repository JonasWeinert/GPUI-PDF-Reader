use crate::OpenExtensionDetails;
use crate::annotations::{
    AnnotationId, AnnotationSet, DocumentIdentity, HighlightColor, MAX_TEXT_CHARACTER_INDEX,
    TextRange,
};
use crate::app_extensions::ReaderExtensions;
use crate::backend::{
    PdfWorker, PreviewSpec, RenderAppearance, RenderColor, TileRequest, WorkerEvent,
};
use crate::document_jump::DocumentJump;
#[cfg(feature = "installable-extensions")]
use crate::extension_packages::{
    InstalledPackageSummary, PackageActivation, PackageInstallPreview,
};
use crate::floating_panel::FloatingPanel;
use crate::link_preview::{
    LinkPreviewEvent, LinkPreviewFetcher, LinkPreviewSession, WebsitePreviewState,
};
use crate::link_resolution::{ResolvedInternalLink, link_source_text, resolve_internal_link};
use crate::markdown_editor::{
    MarkdownEditor, MarkdownEditorConfig, MarkdownEditorEvent, RichTextBuffer,
};
#[cfg(test)]
use crate::model::TextChar;
use crate::model::{
    DocumentLayout, PageAnchor, PageSize, PdfLink, PdfLinkTarget, PixelRect, RasterSize, Rect,
    TextBounds, TextLayer, TextPosition, TextSelection, TileKey, TocEntry,
    append_selected_page_text,
};
use crate::navigation_focus::{
    NavigationFocusEffect, NavigationFocusFrame, NavigationFocusMotion, NavigationFocusTone,
};
use crate::pdf_capability_bridge::PdfCapabilityBridge;
use crate::scholarly::{
    ScholarlyEvent, ScholarlyFetcher, ScholarlyMetadata, ScholarlyMetadataState, ScholarlySession,
    ScholarlySource,
};
#[cfg(debug_assertions)]
use crate::scientific::ScientificSignals;
use crate::scientific::{ScientificReference, grouped_citation_numbers};
use crate::search::{
    MAX_SEARCH_QUERY_BYTES, SearchMatch, SearchMatchId, SearchPageOutcome, SearchQuery, search_page,
};
use crate::text_field::{TextField, TextFieldEvent};
use crate::theme::{self, ReaderPalette, ThemePreference};
use crate::{
    ActualSize, AddComment, ClassicView, CopySelection, EditCopy, EditCut, EditPaste,
    EditSelectAll, Find, FirstPage, FitWidth, FluidView, InstallExtension, LastPage,
    ManageExtensions, NextSearchResult, OpenDocument, PageDown, PageUp, PreviousSearchResult, Quit,
    ScrollDown, ScrollLeft, ScrollRight, ScrollUp, SelectAll, ToggleComments, ZoomIn, ZoomOut,
};
use gpui::{
    Animation, AnimationExt, App, Bounds, ClickEvent, ClipboardItem, ContentMask, Context,
    CursorStyle, Entity, FocusHandle, Focusable, FontWeight, Hsla, IntoElement, ListAlignment,
    ListState, MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, PathPromptOptions,
    Pixels, Point, PromptButton, PromptLevel, Render, RenderImage, ScrollWheelEvent, SharedString,
    StyledText, Task, TextRun, Transformation, UniformListScrollHandle, Window, WindowControlArea,
    div, ease_in_out, font, img, list, percentage, point, prelude::*, px, quad, rems, size,
    uniform_list,
};
#[cfg(debug_assertions)]
use gpui::{Keystroke, Modifiers, ScrollDelta, TouchPhase};
use gpui_component::{
    Icon, IconName, Theme,
    text::{TextView, TextViewStyle},
};
use image::{Frame, RgbaImage};
use key_extension_api::{
    ContributionId, ContributionSlot, DataValue, EffectResult, ExtensionEffect, ExtensionError,
    ExtensionErrorCode, ExtensionId, SnapshotKind,
};
#[cfg(feature = "installable-extensions")]
use key_extension_api::{LifecycleState, Permission, SettingType};
use key_extension_gpui::{BoundedStateMap, DeclarativeView, InvokeExtensionCommand};
use key_extension_host::{ArbitratedEffect, ServiceDispatch};
#[cfg(feature = "installable-extensions")]
use key_extension_host::{OwnedCommand, PermissionDecision};
#[cfg(feature = "installable-extensions")]
use key_extension_package::PackageSourceKind;
use key_pdf_extension_api::{
    OverlayAppearance, OverlayBatch, OverlayEmphasis, OverlayShape, OverlayTone, PdfCapability,
    PdfExtensionError,
};
use key_pdf_gpui::{
    DEFAULT_MAX_CACHE_BYTES as MAX_CACHE_BYTES,
    DEFAULT_MAX_CACHED_TEXT_PAGES as MAX_CACHED_TEXT_PAGES,
    DEFAULT_MAX_CACHED_TILES as MAX_CACHED_TILES, DEFAULT_MAX_ZOOM as MAX_ZOOM,
    DEFAULT_MIN_ZOOM as MIN_ZOOM, DemandTier, InputDisposition, PdfCanvasMetrics, PdfCanvasPage,
    PdfCanvasPagePaintContext, PdfCanvasSnapshot, PdfCanvasStyle, PdfCanvasTile, PdfReaderConfig,
    PdfReaderLimits, ScrollBehavior as ViewportScrollBehavior, ScrollOffset, ViewportController,
    ViewportMetrics, ViewportPoint, command_wheel_zoom_factor, content_rect_to_bounds,
    desired_raster_size as viewport_raster_size, pdf_canvas,
    tile_core_rect as viewport_tile_core_rect, tile_logical_rect,
};
#[cfg(test)]
use key_pdf_gpui::{
    DEFAULT_MAX_RASTER_DIMENSION as MAX_RASTER_DIMENSION, DEFAULT_TILE_BLEED as TILE_BLEED,
    DEFAULT_TILE_SIZE as TILE_SIZE, TilePlanningInput,
    inflate_tile_rect as viewport_inflate_tile_rect,
    plan_visible_tiles as viewport_plan_visible_tiles,
};
use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use std::path::PathBuf;
use std::sync::{Arc, Mutex, mpsc};
use std::time::{Duration, Instant};

mod annotation_io;
mod comments;
mod extensions;
#[cfg(debug_assertions)]
mod qa;
mod references;
mod render;
mod search;
#[cfg(test)]
mod tests;
mod toc;
mod ui;

use annotation_io::{AnnotationIo, AnnotationIoEvent, AnnotationIoOperation};
use key_ui_gpui::UnitTransition;
#[cfg(debug_assertions)]
use qa::{QaExtensionPhase, QaFeaturePhase, QaFluidPhase};
use references::reference_panel_extent;
#[cfg(debug_assertions)]
use references::union_text_bounds;
#[cfg(test)]
use references::{
    adjacent_group_index, citation_expanded_height, compact_authors, compact_journal,
    compact_reference_panel_citation, compact_words, complete_grouped_reference_indices,
    escape_markdown_text, link_card_position, link_hover_candidate_needs_restart,
    link_preview_should_close, link_section_title, measured_preview_width, middle_truncate,
    pointer_link_card_position, reference_hero_height, reference_panel_width,
    reference_preview_width, scientific_reference_matches,
};
#[cfg(test)]
use search::{SearchListRow, next_search_match_id, search_document_jump};
use search::{SearchState, search_list_rows};
#[cfg(test)]
use toc::{
    ResolvedTocDestination, TOC_BREADCRUMB_MAX_LABEL_CHARACTERS, TOC_CARD_MIN_HEIGHT,
    TOC_STACK_MARGIN, active_toc_index, end_truncate, resolve_toc_destination,
    toc_breadcrumb_entries, toc_callout_height, toc_callout_width, toc_cascade_amount,
    toc_display_breadcrumbs, toc_stack_geometry, toc_title_match,
};
use toc::{advance_toc_hover_state, toc_hover_state_is_animating};
use ui::{ChromeButtonStyle, chrome_button, empty_state, error_banner, icon_label};

const TOOLBAR_HEIGHT: f32 = 52.0;
const ERROR_BAR_HEIGHT: f32 = 34.0;
const MAX_COPY_TEXT_BYTES: usize = 64 * 1024 * 1024;
const MAX_VISIBLE_SEARCH_HIGHLIGHT_RUNS: usize = 4_000;
const MAX_VISIBLE_ANNOTATION_QUADS: usize = 8_000;
const MAX_VISIBLE_SELECTION_QUADS: usize = 8_000;
const MAX_VISIBLE_EXTENSION_OVERLAY_REGIONS: usize = 8_192;
const ZOOM_RENDER_DEBOUNCE: Duration = Duration::from_millis(150);
const COMMENT_AUTOSAVE_DEBOUNCE: Duration = Duration::from_millis(500);
const SIDEBAR_WIDTH: f32 = 344.0;
const REFERENCE_PANEL_MIN_WIDTH: f32 = 372.0;
const REFERENCE_PANEL_MAX_WIDTH: f32 = 468.0;
const MIN_DOCUMENT_VIEWPORT_WIDTH: f32 = 300.0;
const FLUID_PANEL_HORIZONTAL_MARGIN: f32 = 12.0;
const FLUID_PANEL_VERTICAL_MARGIN: f32 = 18.0;
const FLUID_CONTEXT_PILL_WIDTH: f32 = 214.0;
const FLUID_CONTEXT_PILL_HEIGHT: f32 = 40.0;
const LINK_CARD_WIDTH: f32 = 340.0;
const LINK_CARD_MARGIN: f32 = 12.0;
const LINK_CARD_GAP: f32 = 8.0;
const LINK_HOVER_HANDOFF_DELAY: Duration = Duration::from_millis(180);
const LINK_HOVER_CLOSE_DELAY: Duration = Duration::from_millis(320);
const LINK_HOVER_STABILITY_RADIUS: f32 = 3.0;
const LINK_CARD_MOVE_DEBOUNCE: Duration = Duration::from_millis(45);
const DOI_COPY_FEEDBACK_DURATION: Duration = Duration::from_millis(1_100);

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

type Offset = ScrollOffset;

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

fn zoom_controls_enabled(document_open: bool, zoom: f32) -> (bool, bool) {
    (
        document_open && zoom > MIN_ZOOM + 0.001,
        document_open && zoom < MAX_ZOOM - 0.001,
    )
}

fn pdf_capability_extension_error(error: PdfExtensionError) -> ExtensionError {
    let (code, retryable) = match &error {
        PdfExtensionError::CapabilityUnavailable { .. } => {
            (ExtensionErrorCode::CapabilityUnavailable, false)
        }
        PdfExtensionError::PermissionDenied { .. } => (ExtensionErrorCode::PermissionDenied, false),
        PdfExtensionError::NoActiveDocument | PdfExtensionError::Busy => {
            (ExtensionErrorCode::TemporarilyUnavailable, true)
        }
        PdfExtensionError::StaleGeneration { .. } | PdfExtensionError::InvalidHandleKind { .. } => {
            (ExtensionErrorCode::StaleResource, false)
        }
        PdfExtensionError::ResourceNotFound { .. } | PdfExtensionError::PageOutOfBounds { .. } => {
            (ExtensionErrorCode::NotFound, false)
        }
        PdfExtensionError::InvalidInput { .. } | PdfExtensionError::Unsupported { .. } => {
            (ExtensionErrorCode::InvalidRequest, false)
        }
        PdfExtensionError::LimitExceeded { .. } => (ExtensionErrorCode::QuotaExceeded, false),
        PdfExtensionError::Cancelled => (ExtensionErrorCode::Cancelled, true),
        PdfExtensionError::Internal { .. } => (ExtensionErrorCode::Internal, false),
    };
    ExtensionError {
        code,
        message: error.to_string(),
        retryable,
    }
}

#[derive(Debug)]
struct DocumentState {
    path: PathBuf,
    pages: Vec<PageSize>,
    toc: Vec<TocEntry>,
    links: Vec<PdfLink>,
    scientific_references: Vec<ScientificReference>,
}

#[derive(Clone)]
struct CachedTile {
    core_rect: PixelRect,
    render_rect: PixelRect,
    byte_len: usize,
    image: Arc<RenderImage>,
}

#[derive(Clone)]
struct DestinationPreview {
    revision: u64,
    image: Arc<RenderImage>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum PreviewTarget {
    Link(usize),
    Reference(usize),
}

#[derive(Clone, Copy, Debug)]
struct PendingLinkHover {
    target: Option<PreviewTarget>,
    position: Point<Pixels>,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
enum ReferenceSummaryTab {
    #[default]
    Tldr,
    Abstract,
}

type RevealState = UnitTransition;

#[derive(Clone, Copy, Debug)]
struct PanState {
    pointer: Point<Pixels>,
    scroll: Offset,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum SidePanel {
    Comments,
    Search,
    Extensions,
}

#[cfg(feature = "installable-extensions")]
#[derive(Clone)]
enum ExtensionManagerPage {
    InstallReview {
        path: PathBuf,
        preview: Box<PackageInstallPreview>,
    },
    Details(ExtensionId),
}

struct ExtensionContributionPane {
    id: ContributionId,
    #[cfg_attr(not(feature = "installable-extensions"), allow(dead_code))]
    owner: ExtensionId,
    title: SharedString,
    view: Entity<DeclarativeView>,
}

#[derive(Default)]
struct ExtensionSnapshotDispatch {
    scheduled: bool,
}

impl ExtensionSnapshotDispatch {
    /// Returns true only for the first request in one coalescing window.
    fn request(&mut self) -> bool {
        if self.scheduled {
            false
        } else {
            self.scheduled = true;
            true
        }
    }

    fn begin_dispatch(&mut self) {
        self.scheduled = false;
    }
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

#[derive(Clone, Debug)]
enum DraftDiscardAction {
    Open(PathBuf),
    Quit,
    CloseWindow,
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
        let mut transition = UnitTransition::new(self.progress);
        transition.set_target(self.target);
        transition.advance(dt);
        self.progress = transition.value();
        self.target = transition.target();
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
        let mut transition = UnitTransition::new(self.progress);
        transition.set_target(self.target);
        transition.advance_with_response(dt, 24.0);
        self.progress = transition.value();
        self.target = transition.target();
    }
}

#[derive(Debug)]
struct PendingCopy {
    selection: TextSelection,
    next_page: usize,
    end_page: usize,
    text: String,
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
    extensions: ReaderExtensions,
    extension_contribution: Option<ExtensionContributionPane>,
    extension_ui_panel: RevealState,
    #[cfg(feature = "installable-extensions")]
    extension_packages: Vec<InstalledPackageSummary>,
    #[cfg(feature = "installable-extensions")]
    extension_commands: Vec<OwnedCommand>,
    #[cfg(feature = "installable-extensions")]
    extension_manager_page: Option<ExtensionManagerPage>,
    #[cfg(feature = "installable-extensions")]
    extension_manager_transition: RevealState,
    #[cfg(feature = "installable-extensions")]
    extension_setting_inputs: HashMap<String, Entity<TextField>>,
    extension_text_statistics: HashMap<usize, (usize, usize)>,
    extension_snapshot_dispatch: ExtensionSnapshotDispatch,
    extension_snapshot_task: Option<Task<()>>,
    worker: PdfWorker,
    annotation_io: AnnotationIo,
    link_preview_fetcher: LinkPreviewFetcher,
    link_preview_session: Option<LinkPreviewSession>,
    scholarly_fetcher: ScholarlyFetcher,
    scholarly_session: ScholarlySession,
    reference_details: Option<String>,
    reference_details_group: Vec<String>,
    reference_details_transition: RevealState,
    reference_details_direction: f32,
    reference_panel: RevealState,
    reference_citation_expansion: RevealState,
    reference_summary_transition: RevealState,
    reference_summary_tab: ReferenceSummaryTab,
    reference_summary_previous_tab: ReferenceSummaryTab,
    doi_copy_started: Option<Instant>,
    link_card_expansion: RevealState,
    #[cfg(debug_assertions)]
    scientific_analysis_complete: bool,
    scientific_document: bool,
    #[cfg(debug_assertions)]
    scientific_signals: ScientificSignals,
    generation: u64,
    document: Option<DocumentState>,
    viewport: ViewportController,
    extension_overlays: Option<Arc<OverlayBatch>>,
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
    comment_editor: Option<Entity<MarkdownEditor>>,
    comment_draft_dirty: bool,
    comment_discard_prompt_open: bool,
    comment_pane: CommentPaneState,
    comment_autosave_revision: u64,
    comment_autosave_task: Option<Task<()>>,
    editing_annotation: Option<AnnotationId>,
    pending_comment_range: Option<TextRange>,
    comment_order: Vec<AnnotationId>,
    search_list_state: ListState,
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
    hovered_link: Option<usize>,
    hovered_reference: Option<usize>,
    link_source_hovered: bool,
    link_card_hovered: bool,
    previewed_link: Option<usize>,
    previewed_reference: Option<usize>,
    destination_preview: Option<DestinationPreview>,
    destination_preview_revision: u64,
    link_hover_revision: u64,
    pending_link_hover: Option<PendingLinkHover>,
    link_card_pointer: Option<Offset>,
    link_card_pointer_target: Option<Offset>,
    link_card_reposition_revision: u64,
    pending_link_click: Option<usize>,
    pending_link_navigation: Option<usize>,
    toc_hovered: Option<usize>,
    toc_hover_position: f32,
    toc_hover_strength: f32,
    toc_hover_revision: u64,
    pending_toc_navigation: Option<usize>,
    navigation_focus: NavigationFocusEffect,
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
    qa_extension_phase: QaExtensionPhase,
    #[cfg(debug_assertions)]
    qa_extension_checks: usize,
    #[cfg(debug_assertions)]
    qa_extension_native_rejected: bool,
    #[cfg(debug_assertions)]
    qa_sidebar_anchor_reference: Option<PageAnchor>,
    #[cfg(debug_assertions)]
    qa_sidebar_transitions: usize,
    #[cfg(debug_assertions)]
    qa_max_sidebar_anchor_error: f32,
    #[cfg(debug_assertions)]
    qa_toc_text_matches: usize,
    #[cfg(debug_assertions)]
    qa_toc_callout_holds: usize,
    #[cfg(debug_assertions)]
    qa_search_focuses: usize,
    #[cfg(debug_assertions)]
    qa_link_navigations: usize,
    zoom_render_revision: u64,
    render_debounce_until: Option<Instant>,
    zoom_render_task: Option<Task<()>>,
}

impl PdfReader {
    pub fn new(
        initial_path: Option<PathBuf>,
        extensions: ReaderExtensions,
        window: &mut Window,
        cx: &mut App,
    ) -> Entity<Self> {
        let (worker, events) = PdfWorker::start();
        let (annotation_io, annotation_events) = AnnotationIo::start();
        let (link_preview_fetcher, link_preview_events) = LinkPreviewFetcher::new();
        let (scholarly_fetcher, scholarly_events) = ScholarlyFetcher::new();
        let extension_warning = extensions
            .startup_error()
            .map(|message| SharedString::from(message.to_owned()));
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
                extensions,
                extension_contribution: None,
                extension_ui_panel: RevealState::default(),
                #[cfg(feature = "installable-extensions")]
                extension_packages: Vec::new(),
                #[cfg(feature = "installable-extensions")]
                extension_commands: Vec::new(),
                #[cfg(feature = "installable-extensions")]
                extension_manager_page: None,
                #[cfg(feature = "installable-extensions")]
                extension_manager_transition: RevealState::default(),
                #[cfg(feature = "installable-extensions")]
                extension_setting_inputs: HashMap::new(),
                extension_text_statistics: HashMap::new(),
                extension_snapshot_dispatch: ExtensionSnapshotDispatch::default(),
                extension_snapshot_task: None,
                worker,
                annotation_io,
                link_preview_fetcher,
                link_preview_session: None,
                scholarly_fetcher,
                scholarly_session: ScholarlySession::default(),
                reference_details: None,
                reference_details_group: Vec::new(),
                reference_details_transition: RevealState::visible(),
                reference_details_direction: 1.0,
                reference_panel: RevealState::default(),
                reference_citation_expansion: RevealState::default(),
                reference_summary_transition: RevealState::visible(),
                reference_summary_tab: ReferenceSummaryTab::Tldr,
                reference_summary_previous_tab: ReferenceSummaryTab::Tldr,
                doi_copy_started: None,
                link_card_expansion: RevealState::default(),
                #[cfg(debug_assertions)]
                scientific_analysis_complete: false,
                scientific_document: false,
                #[cfg(debug_assertions)]
                scientific_signals: ScientificSignals::default(),
                generation: 0,
                document: None,
                viewport: ViewportController::new(PdfReaderConfig::default()),
                extension_overlays: None,
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
                search_list_state: ListState::new(0, ListAlignment::Top, px(160.0)),
                comment_list_scroll: UniformListScrollHandle::new(),
                status: ReaderStatus::Initializing,
                view_mode: ReaderView::Classic,
                theme_preference: ThemePreference::System,
                selected_theme: None,
                render_appearance: render_appearance_from_theme(Theme::global(cx), true),
                pdf_dark_mode_enabled: true,
                warning: extension_warning,
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
                hovered_link: None,
                hovered_reference: None,
                link_source_hovered: false,
                link_card_hovered: false,
                previewed_link: None,
                previewed_reference: None,
                destination_preview: None,
                destination_preview_revision: 0,
                link_hover_revision: 0,
                pending_link_hover: None,
                link_card_pointer: None,
                link_card_pointer_target: None,
                link_card_reposition_revision: 0,
                pending_link_click: None,
                pending_link_navigation: None,
                toc_hovered: None,
                toc_hover_position: 0.0,
                toc_hover_strength: 0.0,
                toc_hover_revision: 0,
                pending_toc_navigation: None,
                navigation_focus: NavigationFocusEffect::default(),
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
                qa_extension_phase: QaExtensionPhase::Seed,
                #[cfg(debug_assertions)]
                qa_extension_checks: 0,
                #[cfg(debug_assertions)]
                qa_extension_native_rejected: false,
                #[cfg(debug_assertions)]
                qa_sidebar_anchor_reference: None,
                #[cfg(debug_assertions)]
                qa_sidebar_transitions: 0,
                #[cfg(debug_assertions)]
                qa_max_sidebar_anchor_error: 0.0,
                #[cfg(debug_assertions)]
                qa_toc_text_matches: 0,
                #[cfg(debug_assertions)]
                qa_toc_callout_holds: 0,
                #[cfg(debug_assertions)]
                qa_search_focuses: 0,
                #[cfg(debug_assertions)]
                qa_link_navigations: 0,
                zoom_render_revision: 0,
                render_debounce_until: None,
                zoom_render_task: None,
            }
        });

        Self::listen_for_worker_events(&entity, events, window, cx);
        Self::listen_for_annotation_events(&entity, annotation_events, window, cx);
        Self::listen_for_link_preview_events(&entity, link_preview_events, window, cx);
        Self::listen_for_scholarly_events(&entity, scholarly_events, window, cx);
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

    fn listen_for_link_preview_events(
        entity: &Entity<Self>,
        events: mpsc::Receiver<LinkPreviewEvent>,
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
                    let _ = async_cx.update(|_, cx| {
                        weak.update(cx, |reader, cx| {
                            if event.generation() != reader.generation {
                                return;
                            }
                            let Some(session) = reader.link_preview_session.as_mut() else {
                                return;
                            };
                            if session.apply(event) == Some(reader.generation) {
                                cx.notify();
                            }
                        })
                        .ok();
                    });
                }
            })
            .detach();
    }

    fn listen_for_scholarly_events(
        entity: &Entity<Self>,
        events: mpsc::Receiver<ScholarlyEvent>,
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
                            if event.generation() == reader.generation
                                && reader.scholarly_session.apply(event) == Some(reader.generation)
                            {
                                if reader.current_reference_texts().iter().any(|reference| {
                                    matches!(
                                        reader.scholarly_session.state(reference),
                                        Some(ScholarlyMetadataState::Ready(_))
                                    )
                                }) {
                                    reader.link_card_expansion.set_target(1.0);
                                    reader.start_animation(window, cx);
                                }
                                cx.notify();
                            }
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
                links,
            } if generation == self.generation => {
                self.drop_all_images(window, cx);
                let capability_title = path
                    .file_name()
                    .and_then(|name| name.to_str())
                    .unwrap_or("PDF");
                if let Err(error) = self.pdf_capabilities().begin_live_document(
                    Some(capability_title),
                    &pages,
                    &toc,
                    &links,
                ) {
                    self.record_pdf_capability_error("document snapshot", error);
                }
                if let Err(error) = self
                    .extensions
                    .begin_document_storage(path.clone(), pages.len())
                {
                    self.warning =
                        Some(format!("Extension document storage is unavailable: {error}").into());
                }
                self.link_preview_session = match LinkPreviewSession::new() {
                    Ok(session) => Some(session),
                    Err(error) => {
                        self.warning = Some(error.into());
                        None
                    }
                };
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
                    pages: pages.clone(),
                    toc,
                    links,
                    scientific_references: Vec::new(),
                });
                if let Err(error) = self.viewport.set_document_pages(pages) {
                    self.close_pdf_capability_generation();
                    self.status = ReaderStatus::Error(error.to_string().into());
                    cx.notify();
                    return;
                }
                self.page_text.clear();
                self.pending.clear();
                self.render_viewport.clear();
                self.text_viewport.clear();
                self.render_debounce_until = None;
                self.zoom_render_task = None;
                self.text_pending.clear();
                self.copy_pending = None;
                self.selection = None;
                self.hovered_link = None;
                self.hovered_reference = None;
                self.link_source_hovered = false;
                self.link_card_hovered = false;
                self.previewed_link = None;
                self.previewed_reference = None;
                self.destination_preview = None;
                self.destination_preview_revision =
                    self.destination_preview_revision.wrapping_add(1);
                self.reference_details = None;
                self.reference_details_group.clear();
                self.reference_details_transition = RevealState::visible();
                self.reference_details_direction = 1.0;
                self.reference_panel = RevealState::default();
                self.reference_citation_expansion = RevealState::default();
                self.reference_summary_transition = RevealState::visible();
                self.reference_summary_tab = ReferenceSummaryTab::Tldr;
                self.reference_summary_previous_tab = ReferenceSummaryTab::Tldr;
                self.doi_copy_started = None;
                self.link_card_expansion = RevealState::default();
                self.pending_link_hover = None;
                self.link_card_pointer = None;
                self.link_card_pointer_target = None;
                self.link_card_reposition_revision =
                    self.link_card_reposition_revision.wrapping_add(1);
                self.pending_link_click = None;
                self.pending_link_navigation = None;
                self.status = ReaderStatus::Ready;
                self.viewport.fit_width();
                self.sync_viewport_snapshot();
                self.publish_pdf_selection();
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
            WorkerEvent::PreviewRendered {
                generation,
                revision,
                appearance,
                width,
                height,
                bgra,
            } if generation == self.generation
                && appearance == self.render_appearance
                && revision == self.destination_preview_revision =>
            {
                let expected = width
                    .checked_mul(height)
                    .and_then(|pixels| pixels.checked_mul(4))
                    .and_then(|bytes| usize::try_from(bytes).ok());
                if expected == Some(bgra.len())
                    && let Some(buffer) = RgbaImage::from_raw(width, height, bgra)
                {
                    let previous = self.destination_preview.replace(DestinationPreview {
                        revision,
                        image: Arc::new(RenderImage::new(vec![Frame::new(buffer)])),
                    });
                    if let Some(previous) = previous {
                        Self::retire_images(vec![previous.image], window, cx);
                    }
                }
            }
            WorkerEvent::PreviewFailed {
                generation,
                revision,
                message,
            } if generation == self.generation && revision == self.destination_preview_revision => {
                if let Some(previous) = self.destination_preview.take() {
                    Self::retire_images(vec![previous.image], window, cx);
                }
                self.warning = Some(format!("Could not render link preview: {message}").into());
            }
            WorkerEvent::TextExtracted {
                generation,
                page,
                text,
            } if generation == self.generation => {
                self.page_text.entry(page).or_insert(text);
                self.publish_pdf_text(page);
                self.publish_pdf_selection();
                self.text_pending.remove(&page);
                self.continue_pending_copy(cx);
                self.complete_pending_toc_navigation(page, window, cx);
                self.complete_pending_link_navigation(window, cx);
                if let Some(id) = self.previewed_link {
                    self.request_scholarly_for_link(id);
                }
                if let Some(index) = self.previewed_reference {
                    self.request_scholarly_for_reference(index);
                }
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
                self.publish_pdf_text(page);
                self.publish_pdf_selection();
                self.text_pending.remove(&page);
                self.warning = Some(message.into());
                self.continue_pending_copy(cx);
                self.complete_pending_toc_navigation(page, window, cx);
                self.complete_pending_link_navigation(window, cx);
                if let Some(id) = self.previewed_link {
                    self.request_scholarly_for_link(id);
                }
                if let Some(index) = self.previewed_reference {
                    self.request_scholarly_for_reference(index);
                }
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
                self.search_list_state
                    .reset(search_list_rows(&self.search.order).len());
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
            WorkerEvent::ScientificAnalysisComplete {
                generation,
                analysis,
            } if generation == self.generation => {
                #[cfg(debug_assertions)]
                {
                    self.scientific_analysis_complete = true;
                    self.scientific_signals = analysis.signals;
                }
                self.scientific_document = analysis.is_scientific;
                if let Some(document) = self.document.as_mut() {
                    document.scientific_references = analysis.references;
                    let mut next_id = document.links.len();
                    for mut link in analysis.synthetic_links {
                        link.id = next_id;
                        next_id += 1;
                        document.links.push(link);
                    }
                }
                let link_publication = self
                    .document
                    .as_ref()
                    .map(|document| self.pdf_capabilities().publish_live_links(&document.links));
                if let Some(Err(error)) = link_publication {
                    self.record_pdf_capability_error("link snapshot", error);
                }
                if let Some(id) = self.previewed_link {
                    self.request_scholarly_for_link(id);
                }
                if let Some(index) = self.previewed_reference {
                    self.request_scholarly_for_reference(index);
                }
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
        self.schedule_extension_snapshot_sync(window, cx);
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
        self.schedule_extension_snapshot_sync(window, cx);
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

    #[cfg(feature = "installable-extensions")]
    fn install_extension_dialog(
        &mut self,
        _: &InstallExtension,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let prompt = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: true,
            multiple: false,
            prompt: Some("Install .keyext or Development Extension".into()),
        });
        let weak = cx.weak_entity();
        window
            .spawn(cx, async move |cx| {
                let path = match prompt.await {
                    Ok(Ok(Some(paths))) => paths.into_iter().next(),
                    Ok(Ok(None)) => return,
                    Ok(Err(error)) => {
                        let message = format!("Could not select an extension: {error}");
                        let _ = cx.update(|_, cx| {
                            weak.update(cx, |reader, cx| {
                                reader.warning = Some(message.into());
                                cx.notify();
                            })
                            .ok();
                        });
                        return;
                    }
                    Err(error) => {
                        let message = format!("Extension selection was interrupted: {error}");
                        let _ = cx.update(|_, cx| {
                            weak.update(cx, |reader, cx| {
                                reader.warning = Some(message.into());
                                cx.notify();
                            })
                            .ok();
                        });
                        return;
                    }
                };
                let Some(path) = path else {
                    let _ = cx.update(|_, cx| {
                        weak.update(cx, |reader, cx| {
                            reader.warning = Some("The extension picker returned no path".into());
                            cx.notify();
                        })
                        .ok();
                    });
                    return;
                };

                // NSOpenPanel resolves its completion before AppKit has fully
                // dismissed the sheet. Cross one foreground turn before
                // moving the Extensions panel to the in-app review page.
                cx.background_executor()
                    .timer(Duration::from_millis(50))
                    .await;
                let path = resolve_extension_package_selection(path);
                let _ = cx.update(|window, cx| {
                    weak.update(cx, |reader, cx| {
                        reader.install_extension_path(path, window, cx)
                    })
                    .ok();
                });
            })
            .detach();
    }

    #[cfg(not(feature = "installable-extensions"))]
    fn install_extension_dialog(
        &mut self,
        _: &InstallExtension,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.warning =
            Some("This minimal build does not include the installable extension runtime".into());
        cx.notify();
    }

    #[cfg(feature = "installable-extensions")]
    fn install_extension_path(
        &mut self,
        path: PathBuf,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let preview = match self.extensions.preview_package(&path) {
            Ok(preview) => preview,
            Err(error) => {
                self.warning = Some(format!("Could not review extension: {error}").into());
                cx.notify();
                return;
            }
        };
        self.open_extension_manager_page(
            ExtensionManagerPage::InstallReview {
                path,
                preview: Box::new(preview),
            },
            window,
            cx,
        );
    }

    #[cfg(feature = "installable-extensions")]
    fn install_extension_after_review(
        &mut self,
        path: PathBuf,
        preview: PackageInstallPreview,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let verb = if preview.is_upgrade {
            "Updated"
        } else {
            "Installed"
        };
        let mut report = match self.extensions.install_reviewed_package(&path, &preview) {
            Ok(report) => report,
            Err(error) => {
                self.warning = Some(format!("Could not install extension: {error}").into());
                cx.notify();
                return;
            }
        };
        if matches!(report.activation, PackageActivation::AwaitingPermissions(_)) {
            report = match self.extensions.approve_package(&report.extension) {
                Ok(report) => report,
                Err(error) => {
                    self.warning =
                        Some(format!("Could not enable {}: {error}", preview.name).into());
                    self.refresh_extension_manager_state();
                    crate::rebuild_application_menus(&mut self.extensions, cx);
                    cx.notify();
                    return;
                }
            };
        }
        self.handle_package_report(report, verb, window, cx);
    }

    #[cfg(feature = "installable-extensions")]
    fn handle_package_report(
        &mut self,
        report: crate::extension_packages::PackageInstallReport,
        verb: &'static str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match report.activation {
            PackageActivation::Active => {
                self.warning = Some(format!("{verb} {} {}", report.name, report.version).into());
                self.refresh_extension_manager_state();
                crate::rebuild_application_menus(&mut self.extensions, cx);
                self.schedule_extension_snapshot_sync(window, cx);
            }
            PackageActivation::Activating => {
                self.warning = Some(
                    format!(
                        "Checking {} {} before completing the update…",
                        report.name, report.version
                    )
                    .into(),
                );
                self.refresh_extension_manager_state();
                self.schedule_extension_snapshot_sync(window, cx);
            }
            PackageActivation::Inactive(reason) => {
                self.warning = Some(
                    format!(
                        "{verb} {} {}, but it is inactive: {reason}",
                        report.name, report.version
                    )
                    .into(),
                );
                self.refresh_extension_manager_state();
                crate::rebuild_application_menus(&mut self.extensions, cx);
            }
            PackageActivation::AwaitingPermissions(permissions) => {
                self.warning = Some(
                    format!(
                        "{} needs {} required permission{} before it can be enabled",
                        report.name,
                        permissions.len(),
                        if permissions.len() == 1 { "" } else { "s" }
                    )
                    .into(),
                );
                self.refresh_extension_manager_state();
                self.open_extension_manager_page(
                    ExtensionManagerPage::Details(report.extension),
                    window,
                    cx,
                );
            }
        }
        cx.notify();
    }

    fn manage_extensions(
        &mut self,
        _: &ManageExtensions,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        #[cfg(feature = "installable-extensions")]
        {
            self.refresh_extension_manager_state();
            self.show_extension_manager_overview(window, cx);
        }
        self.show_sidebar(SidePanel::Extensions, window, cx);
    }

    #[cfg(feature = "installable-extensions")]
    fn open_extension_manager_page(
        &mut self,
        page: ExtensionManagerPage,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.extension_setting_inputs.clear();
        if let ExtensionManagerPage::Details(extension) = &page {
            self.prepare_extension_setting_inputs(extension.clone(), window, cx);
        }
        self.extension_manager_page = Some(page);
        self.extension_manager_transition = RevealState::default();
        self.extension_manager_transition.set_target(1.0);
        self.show_sidebar(SidePanel::Extensions, window, cx);
        self.start_animation(window, cx);
        cx.notify();
    }

    #[cfg(feature = "installable-extensions")]
    fn show_extension_manager_overview(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.extension_manager_page.is_none() {
            return;
        }
        self.extension_manager_transition.set_target(0.0);
        self.start_animation(window, cx);
        cx.notify();
    }

    #[cfg(feature = "installable-extensions")]
    fn open_extension_details(
        &mut self,
        extension: ExtensionId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.refresh_extension_manager_state();
        if self
            .extension_packages
            .iter()
            .any(|package| package.extension == extension)
        {
            self.open_extension_manager_page(ExtensionManagerPage::Details(extension), window, cx);
        }
    }

    fn open_extension_details_action(
        &mut self,
        action: &OpenExtensionDetails,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        #[cfg(feature = "installable-extensions")]
        self.open_extension_details(action.extension.clone(), window, cx);
        #[cfg(not(feature = "installable-extensions"))]
        {
            let _ = (action, window, cx);
        }
    }

    #[cfg(feature = "installable-extensions")]
    fn confirm_extension_install(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(ExtensionManagerPage::InstallReview { path, preview }) =
            self.extension_manager_page.take()
        else {
            return;
        };
        let extension = preview.extension.clone();
        self.install_extension_after_review(path, *preview, window, cx);
        self.refresh_extension_manager_state();
        if self
            .extension_packages
            .iter()
            .any(|package| package.extension == extension)
        {
            self.open_extension_manager_page(ExtensionManagerPage::Details(extension), window, cx);
        } else {
            self.show_extension_manager_overview(window, cx);
        }
    }

    #[cfg(feature = "installable-extensions")]
    fn prepare_extension_setting_inputs(
        &mut self,
        extension: ExtensionId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(package) = self
            .extension_packages
            .iter()
            .find(|package| package.extension == extension)
            .cloned()
        else {
            return;
        };
        for setting in package.settings_schema.fields.iter().filter(|setting| {
            !setting.sensitive
                && matches!(
                    setting.value_type,
                    SettingType::String { .. }
                        | SettingType::Integer { .. }
                        | SettingType::Number { .. }
                )
        }) {
            let maximum_bytes = match &setting.value_type {
                SettingType::String { maximum_bytes } => {
                    maximum_bytes.unwrap_or(4_096).min(64 * 1024) as usize
                }
                _ => 128,
            };
            let current = extension_setting_value(&package, &setting.key)
                .map(extension_setting_text)
                .unwrap_or_default();
            let placeholder = setting.label.clone();
            let field = cx.new(|cx| {
                let mut field = TextField::new(cx, placeholder, maximum_bytes);
                let _ = field.set_text(&current, cx);
                field
            });
            let field_for_event = field.clone();
            let extension_for_event = extension.clone();
            let key = setting.key.clone();
            cx.subscribe_in(
                &field,
                window,
                move |reader, _, event, window, cx| match event {
                    TextFieldEvent::Submit => {
                        let text = field_for_event.read(cx).text().to_owned();
                        reader.apply_extension_setting_text(
                            extension_for_event.clone(),
                            key.clone(),
                            text,
                            window,
                            cx,
                        );
                    }
                    TextFieldEvent::Rejected(rejection) => {
                        reader.warning = Some(rejection.to_string().into());
                        cx.notify();
                    }
                    TextFieldEvent::Changed(_) | TextFieldEvent::Cancel => {}
                },
            )
            .detach();
            self.extension_setting_inputs
                .insert(setting.key.clone(), field);
        }
    }

    #[cfg(feature = "installable-extensions")]
    fn apply_extension_setting_text(
        &mut self,
        extension: ExtensionId,
        key: String,
        text: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(setting) = self
            .extension_packages
            .iter()
            .find(|package| package.extension == extension)
            .and_then(|package| {
                package
                    .settings_schema
                    .fields
                    .iter()
                    .find(|setting| setting.key == key)
            })
        else {
            return;
        };
        let value = match &setting.value_type {
            SettingType::String { .. } => DataValue::String(text),
            SettingType::Integer { .. } => match text.parse::<i64>() {
                Ok(value) => DataValue::Integer(value),
                Err(_) => {
                    self.warning = Some(format!("{} must be a whole number", setting.label).into());
                    cx.notify();
                    return;
                }
            },
            SettingType::Number { .. } => match text.parse::<f64>() {
                Ok(value) if value.is_finite() => DataValue::Number(value),
                _ => {
                    self.warning = Some(format!("{} must be a number", setting.label).into());
                    cx.notify();
                    return;
                }
            },
            _ => return,
        };
        self.apply_extension_setting(extension, key, value, window, cx);
    }

    #[cfg(feature = "installable-extensions")]
    fn apply_extension_setting(
        &mut self,
        extension: ExtensionId,
        key: String,
        value: DataValue,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match self.extensions.set_package_setting(&extension, &key, value) {
            Ok(effects) => {
                self.execute_extension_effects(effects, window, cx);
                self.refresh_extension_manager_state();
                if self.extensions.has_pending_extension_work() {
                    self.schedule_extension_snapshot_sync(window, cx);
                }
                self.warning = Some("Extension setting saved".into());
            }
            Err(error) => {
                self.warning = Some(format!("Could not save extension setting: {error}").into());
            }
        }
        cx.notify();
    }

    #[cfg(feature = "installable-extensions")]
    fn enable_extension(
        &mut self,
        extension: ExtensionId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match self.extensions.enable_package(&extension) {
            Ok(report) => self.handle_package_report(report, "Enabled", window, cx),
            Err(error) => {
                self.warning = Some(format!("Could not enable extension: {error}").into());
                self.refresh_extension_manager_state();
                cx.notify();
            }
        }
    }

    #[cfg(feature = "installable-extensions")]
    fn disable_extension(
        &mut self,
        extension: ExtensionId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match self.extensions.disable_package(&extension) {
            Ok(()) => {
                if self
                    .extension_contribution
                    .as_ref()
                    .is_some_and(|pane| pane.owner == extension)
                {
                    self.extension_ui_panel.set_target(0.0);
                    self.start_animation(window, cx);
                }
                self.warning = Some("Extension disabled".into());
                self.refresh_extension_manager_state();
                crate::rebuild_application_menus(&mut self.extensions, cx);
            }
            Err(error) => {
                self.warning = Some(format!("Could not disable extension: {error}").into());
            }
        }
        self.refresh_active_extension_view(window, cx);
        cx.notify();
    }

    #[cfg(feature = "installable-extensions")]
    fn set_extension_permission(
        &mut self,
        extension: ExtensionId,
        permission: Permission,
        grant: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let decision = if grant {
            PermissionDecision::Granted
        } else {
            PermissionDecision::Denied
        };
        match self
            .extensions
            .set_package_permission(&extension, &permission, decision)
        {
            Ok(()) => {
                self.warning = Some(
                    if grant {
                        "Extension permission allowed"
                    } else {
                        "Extension permission revoked"
                    }
                    .into(),
                );
                self.refresh_extension_manager_state();
                crate::rebuild_application_menus(&mut self.extensions, cx);
                self.refresh_active_extension_view(window, cx);
                self.schedule_extension_snapshot_sync(window, cx);
            }
            Err(error) => {
                self.warning =
                    Some(format!("Could not update extension permission: {error}").into());
                self.refresh_extension_manager_state();
            }
        }
        cx.notify();
    }

    #[cfg(feature = "installable-extensions")]
    fn confirm_remove_extension(
        &mut self,
        extension: ExtensionId,
        name: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let answer = window.prompt(
            PromptLevel::Warning,
            "Remove extension?",
            Some(&format!("Remove {name} from this reader session?")),
            &[PromptButton::cancel("Cancel"), PromptButton::ok("Remove")],
            cx,
        );
        let weak = cx.weak_entity();
        window
            .spawn(cx, async move |cx| {
                if answer.await.ok() != Some(1) {
                    return;
                }
                let _ = cx.update(|window, cx| {
                    weak.update(cx, |reader, cx| {
                        match reader.extensions.remove_package(&extension) {
                            Ok(()) => {
                                if reader
                                    .extension_contribution
                                    .as_ref()
                                    .is_some_and(|pane| pane.owner == extension)
                                {
                                    reader.extension_ui_panel.set_target(0.0);
                                    reader.start_animation(window, cx);
                                }
                                reader.warning = Some(format!("Removed {name}").into());
                                reader.refresh_extension_manager_state();
                                if matches!(
                                    reader.extension_manager_page,
                                    Some(ExtensionManagerPage::Details(ref shown))
                                        if shown == &extension
                                ) {
                                    reader.show_extension_manager_overview(window, cx);
                                }
                                crate::rebuild_application_menus(&mut reader.extensions, cx);
                            }
                            Err(error) => {
                                reader.warning =
                                    Some(format!("Could not remove {name}: {error}").into());
                            }
                        }
                        reader.refresh_active_extension_view(window, cx);
                        cx.notify();
                    })
                    .ok();
                });
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
        self.extensions.invalidate_document_scope();
        self.generation = self.generation.wrapping_add(1);
        self.close_pdf_capability_generation();
        self.link_preview_fetcher.begin_document(self.generation);
        self.scholarly_fetcher.begin_document(self.generation);
        self.link_preview_session = None;
        self.scholarly_session = ScholarlySession::default();
        self.reference_details = None;
        self.reference_details_group.clear();
        self.reference_details_transition = RevealState::visible();
        self.reference_details_direction = 1.0;
        self.reference_panel = RevealState::default();
        self.reference_citation_expansion = RevealState::default();
        self.reference_summary_transition = RevealState::visible();
        self.reference_summary_tab = ReferenceSummaryTab::Tldr;
        self.reference_summary_previous_tab = ReferenceSummaryTab::Tldr;
        self.doi_copy_started = None;
        self.link_card_expansion = RevealState::default();
        self.link_card_pointer = None;
        self.link_card_pointer_target = None;
        self.link_card_reposition_revision = self.link_card_reposition_revision.wrapping_add(1);
        self.destination_preview = None;
        self.destination_preview_revision = self.destination_preview_revision.wrapping_add(1);
        #[cfg(debug_assertions)]
        {
            self.scientific_analysis_complete = false;
            self.scientific_signals = ScientificSignals::default();
        }
        self.scientific_document = false;
        self.drop_all_images(window, cx);
        self.document = None;
        self.viewport.clear_document();
        self.sync_viewport_snapshot();
        self.page_text.clear();
        self.extension_text_statistics.clear();
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
        self.hovered_link = None;
        self.hovered_reference = None;
        self.link_source_hovered = false;
        self.link_card_hovered = false;
        self.previewed_link = None;
        self.link_hover_revision = self.link_hover_revision.wrapping_add(1);
        self.pending_link_hover = None;
        self.pending_link_click = None;
        self.pending_link_navigation = None;
        self.toc_hovered = None;
        self.toc_hover_position = 0.0;
        self.toc_hover_strength = 0.0;
        self.toc_hover_revision = self.toc_hover_revision.wrapping_add(1);
        self.pending_toc_navigation = None;
        self.navigation_focus.cancel();
        self.animation_active = false;
        #[cfg(debug_assertions)]
        {
            self.qa_feature_phase = QaFeaturePhase::Seed;
            self.qa_fluid_phase = QaFluidPhase::Seed;
            self.qa_extension_phase = QaExtensionPhase::Seed;
            self.qa_extension_checks = 0;
            self.qa_extension_native_rejected = false;
            self.qa_sidebar_anchor_reference = None;
            self.qa_sidebar_transitions = 0;
            self.qa_max_sidebar_anchor_error = 0.0;
            self.qa_toc_text_matches = 0;
            self.qa_toc_callout_holds = 0;
            self.qa_search_focuses = 0;
            self.qa_link_navigations = 0;
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
        self.viewport.layout()
    }

    fn pdf_capabilities(&self) -> Arc<PdfCapabilityBridge> {
        self.extensions.pdf_capabilities()
    }

    fn close_pdf_capability_generation(&mut self) {
        self.extension_overlays = None;
        let _ = self.pdf_capabilities().close_document();
    }

    fn record_pdf_capability_error(&mut self, operation: &str, error: PdfExtensionError) {
        if !matches!(error, PdfExtensionError::NoActiveDocument) {
            self.warning = Some(format!("PDF extension {operation} unavailable: {error}").into());
        }
    }

    fn publish_pdf_text(&mut self, page: usize) {
        let Some(text) = self.page_text.get(&page).cloned() else {
            return;
        };
        self.extension_text_statistics
            .insert(page, text_layer_statistics(&text));
        if let Err(error) = self.pdf_capabilities().publish_live_text(page, &text) {
            self.record_pdf_capability_error("text snapshot", error);
        }
    }

    fn publish_pdf_selection(&mut self) {
        let bridge = self.pdf_capabilities();
        let selection = self.selection;
        let result = bridge.publish_live_selection(selection, |page| {
            self.page_text.get(&page).map(AsRef::as_ref)
        });
        if let Err(error) = result {
            self.record_pdf_capability_error("selection snapshot", error);
        }
    }

    fn publish_pdf_viewport(&mut self) {
        let Some(layout) = self.layout().cloned() else {
            return;
        };
        let visible_width = self.panel_safe_viewport_width();
        let result = self.pdf_capabilities().publish_live_viewport(
            self.zoom,
            &layout,
            self.scroll.x,
            self.scroll.y,
            visible_width,
            self.viewport_height,
        );
        if let Err(error) = result {
            self.record_pdf_capability_error("viewport snapshot", error);
        }
    }

    fn consume_pdf_extension_outputs(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let bridge = self.pdf_capabilities();
        match bridge.take_pending_navigation() {
            Ok(Some(pending)) => {
                let _accepted_revision = pending.receipt.viewport_revision;
                self.perform_document_jump(pending.jump, window, cx);
            }
            Ok(None) | Err(PdfExtensionError::NoActiveDocument) => {}
            Err(error) => self.record_pdf_capability_error("navigation", error),
        }
        match bridge.active_overlays() {
            Ok(overlays) => self.extension_overlays = overlays,
            Err(PdfExtensionError::NoActiveDocument) => self.extension_overlays = None,
            Err(error) => self.record_pdf_capability_error("overlays", error),
        }
    }

    /// Refreshes the paint-shell compatibility snapshot. The reusable
    /// controller is the sole mutable authority for these values; keeping the
    /// scalars here avoids coupling the rest of the product UI to its API in
    /// one all-or-nothing migration.
    fn sync_viewport_snapshot(&mut self) {
        let snapshot = self.viewport.snapshot();
        self.zoom = snapshot.zoom;
        self.fit_width = snapshot.fit_width;
        self.scroll = snapshot.scroll;
        self.scroll_target = snapshot.scroll_target;
        self.viewport_width = snapshot.metrics.width;
        self.viewport_height = snapshot.metrics.height;
        self.viewport.drain_events().for_each(drop);
        self.publish_pdf_viewport();
    }

    fn update_viewport(&mut self, window: &Window) {
        let size = window.viewport_size();
        let full_width = f32::from(size.width).max(1.0);
        let width = (full_width - self.sidebar_reserved_width(full_width)).max(1.0);
        let error_height = self.content_top() - TOOLBAR_HEIGHT;
        let height = (f32::from(size.height) - TOOLBAR_HEIGHT - error_height).max(1.0);
        let right_occlusion = if self.view_mode == ReaderView::Fluid {
            fluid_sidebar_extent(width, self.sidebar.progress)
                + reference_panel_extent(width, self.reference_panel.value())
        } else {
            0.0
        };
        self.viewport.set_viewport(ViewportMetrics {
            width,
            height,
            right_occlusion,
            scale_factor: window.scale_factor(),
        });
        self.sync_viewport_snapshot();
    }

    fn update_sidebar_viewport_preserving_anchor(&mut self, window: &Window) {
        let size = window.viewport_size();
        let full_width = f32::from(size.width).max(1.0);
        let next_width = (full_width - self.sidebar_reserved_width(full_width)).max(1.0);
        let right_occlusion = if self.view_mode == ReaderView::Fluid {
            fluid_sidebar_extent(next_width, self.sidebar.progress)
                + reference_panel_extent(next_width, self.reference_panel.value())
        } else {
            0.0
        };
        self.viewport.set_viewport(ViewportMetrics {
            width: next_width,
            height: self.viewport_height,
            right_occlusion,
            scale_factor: window.scale_factor(),
        });
        self.sync_viewport_snapshot();
        if let Some(anchor) = self.sidebar_anchor
            && let Some((x, y)) = self
                .layout()
                .and_then(|layout| layout.content_point_for_anchor(anchor))
        {
            self.viewport.set_scroll(Offset {
                x: x - self.panel_safe_viewport_width() * 0.5,
                y: y - self.viewport_height * 0.5,
            });
            self.sync_viewport_snapshot();
        }
    }

    fn sidebar_reserved_width(&self, full_width: f32) -> f32 {
        match self.view_mode {
            ReaderView::Classic => {
                self.sidebar.available_width(full_width)
                    + reference_panel_extent(full_width, self.reference_panel.value())
            }
            ReaderView::Fluid => 0.0,
        }
    }

    fn fluid_panel_occlusion(&self) -> f32 {
        if self.view_mode == ReaderView::Fluid {
            fluid_sidebar_extent(self.viewport_width, self.sidebar.progress)
                + reference_panel_extent(self.viewport_width, self.reference_panel.value())
        } else {
            0.0
        }
    }

    fn fluid_panel_width(&self) -> f32 {
        fluid_sidebar_width(self.viewport_width)
    }

    fn panel_safe_viewport_width(&self) -> f32 {
        (self.viewport_width - self.fluid_panel_occlusion()).max(1.0)
    }

    fn max_scroll_x(&self, _layout: &DocumentLayout) -> f32 {
        self.viewport.maximum_scroll().x
    }

    fn set_view_mode(&mut self, mode: ReaderView, window: &mut Window, cx: &mut Context<Self>) {
        if self.view_mode == mode {
            return;
        }
        self.sidebar_anchor = self.layout().and_then(|layout| {
            layout.anchor_at_content_point(
                self.scroll.x + self.panel_safe_viewport_width() * 0.5,
                self.scroll.y + self.viewport_height * 0.5,
            )
        });
        self.viewport.disable_fit_width();
        self.sync_viewport_snapshot();
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

    #[cfg(feature = "installable-extensions")]
    fn refresh_extension_manager_state(&mut self) {
        self.extension_packages = self.extensions.installed_packages();
        self.extension_commands = self.extensions.installed_commands();
    }

    fn refresh_active_extension_view(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(id) = self
            .extension_contribution
            .as_ref()
            .map(|pane| pane.id.clone())
        else {
            return;
        };
        let Some(owned) = self.extensions.contribution_view(&id) else {
            self.extension_ui_panel.set_target(0.0);
            self.start_animation(window, cx);
            return;
        };
        let Ok(state) = BoundedStateMap::new(owned.state, Default::default()) else {
            self.warning = Some("Extension view state exceeded host rendering limits".into());
            return;
        };
        if let Some(pane) = self.extension_contribution.as_ref() {
            pane.view.update(cx, |view, cx| {
                view.set_state(state, window, cx);
            });
        }
    }

    fn open_extension_contribution(
        &mut self,
        contribution: &ContributionId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> EffectResult {
        let Some(owned) = self.extensions.contribution_view(contribution) else {
            return Err(ExtensionError {
                code: ExtensionErrorCode::NotFound,
                message: "the contribution is no longer active".into(),
                retryable: true,
            });
        };
        if !matches!(
            owned.view.slot,
            ContributionSlot::SidePanel | ContributionSlot::SettingsPanel
        ) {
            return Err(ExtensionError {
                code: ExtensionErrorCode::CapabilityUnavailable,
                message: "this reader currently opens side-panel and settings contributions".into(),
                retryable: false,
            });
        }
        let owner = owned.owner.clone();
        #[cfg(feature = "installable-extensions")]
        let title = self
            .extension_packages
            .iter()
            .find(|summary| summary.extension == owner)
            .map(|summary| SharedString::from(summary.name.clone()))
            .unwrap_or_else(|| SharedString::from("Extension"));
        #[cfg(not(feature = "installable-extensions"))]
        let title = SharedString::from("Extension");
        let id = owned.view.id.clone();
        let view = cx.new(|cx| DeclarativeView::new(owned, window, cx));
        self.extension_contribution = Some(ExtensionContributionPane {
            id,
            owner,
            title,
            view,
        });
        if self.sidebar.target > 0.5 {
            self.toggle_sidebar(self.sidebar.panel, window, cx);
        }
        self.extension_ui_panel.set_target(1.0);
        self.start_animation(window, cx);
        Ok(DataValue::Null)
    }

    fn close_extension_contribution(
        &mut self,
        contribution: Option<&ContributionId>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> EffectResult {
        if contribution.is_some_and(|id| {
            self.extension_contribution
                .as_ref()
                .is_none_or(|pane| pane.id != *id)
        }) {
            return Ok(DataValue::Null);
        }
        self.extension_ui_panel.set_target(0.0);
        self.start_animation(window, cx);
        Ok(DataValue::Null)
    }

    fn extension_snapshot_values(&self) -> Vec<(SnapshotKind, DataValue)> {
        let page_count = self
            .document
            .as_ref()
            .map_or(0, |document| document.pages.len());
        let title = self
            .document
            .as_ref()
            .and_then(|document| document.path.file_name())
            .map(|name| bounded_snapshot_string(&name.to_string_lossy(), 256))
            .unwrap_or_default();
        let (word_count, character_count) = self.extension_text_statistics.values().copied().fold(
            (0usize, 0usize),
            |(words, characters), item| {
                (
                    words.saturating_add(item.0),
                    characters.saturating_add(item.1),
                )
            },
        );
        let statistics = data_record([
            ("page-count", DataValue::Integer(bounded_i64(page_count))),
            ("word-count", DataValue::Integer(bounded_i64(word_count))),
            (
                "character-count",
                DataValue::Integer(bounded_i64(character_count)),
            ),
            (
                "text-pages-known",
                DataValue::Integer(bounded_i64(self.extension_text_statistics.len())),
            ),
        ]);
        let document = data_record([
            ("open", DataValue::Boolean(self.document.is_some())),
            (
                "generation",
                DataValue::Integer(bounded_i64(self.generation)),
            ),
            ("title", DataValue::String(title)),
            ("statistics", statistics),
            ("scientific", DataValue::Boolean(self.scientific_document)),
        ]);

        let current_page = self
            .layout()
            .map(|layout| layout.current_page(self.scroll.y, self.viewport_height));
        let viewport = data_record([
            ("zoom", DataValue::Number(f64::from(self.zoom))),
            ("scroll-x", DataValue::Number(f64::from(self.scroll.x))),
            ("scroll-y", DataValue::Number(f64::from(self.scroll.y))),
            ("width", DataValue::Number(f64::from(self.viewport_width))),
            ("height", DataValue::Number(f64::from(self.viewport_height))),
            (
                "current-page",
                current_page.map_or(DataValue::Null, |page| {
                    DataValue::Integer(bounded_i64(page))
                }),
            ),
        ]);

        let selection = self.selection.map_or_else(
            || data_record([("active", DataValue::Boolean(false))]),
            |selection| {
                let (start, end) = selection.ordered();
                data_record([
                    ("active", DataValue::Boolean(true)),
                    ("start-page", DataValue::Integer(bounded_i64(start.page))),
                    (
                        "start-character",
                        DataValue::Integer(bounded_i64(start.index)),
                    ),
                    ("end-page", DataValue::Integer(bounded_i64(end.page))),
                    ("end-character", DataValue::Integer(bounded_i64(end.index))),
                ])
            },
        );
        vec![
            (SnapshotKind::Document, document),
            (SnapshotKind::Viewport, viewport),
            (SnapshotKind::Selection, selection),
        ]
    }

    /// Executes bounded host ticks from event/animation callbacks, never from
    /// `Render`. High-frequency navigation is coalesced by snapshot equality.
    fn flush_extension_snapshots(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let completions = self.extensions.poll_service_completions(32);
        for completion in completions {
            match self
                .extensions
                .complete_effect(&completion.effect, completion.result)
            {
                Ok(report) if !report.effects.is_empty() => {
                    self.execute_extension_effects(report.effects, window, cx);
                }
                Ok(_) => {}
                Err(error) => {
                    self.warning =
                        Some(format!("Extension task completion failed: {error}").into());
                }
            }
        }
        let report = match self
            .extensions
            .publish_snapshots(self.extension_snapshot_values())
        {
            Ok(report) => report,
            Err(error) => {
                self.warning = Some(format!("Extension snapshot failed: {error}").into());
                return;
            }
        };
        #[cfg(feature = "installable-extensions")]
        {
            let activation_updates = self.extensions.settle_package_activations();
            if let Some(update) = activation_updates.last() {
                self.warning = Some(update.message.clone().into());
                self.refresh_extension_manager_state();
                crate::rebuild_application_menus(&mut self.extensions, cx);
            }
        }
        self.refresh_active_extension_view(window, cx);
        if !report.effects.is_empty() {
            self.execute_extension_effects(report.effects, window, cx);
        }
        if report.deferred_events > 0
            || self.extensions.has_pending_service_work()
            || self.extensions.has_pending_extension_work()
        {
            self.schedule_extension_snapshot_sync(window, cx);
        }
    }

    fn schedule_extension_snapshot_sync(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.extension_snapshot_dispatch.request() {
            return;
        }
        let weak = cx.weak_entity();
        self.extension_snapshot_task = Some(window.spawn(cx, async move |cx| {
            cx.background_executor()
                .timer(Duration::from_millis(24))
                .await;
            let _ = cx.update(|window, cx| {
                weak.update(cx, |reader, cx| {
                    reader.extension_snapshot_task = None;
                    reader.extension_snapshot_dispatch.begin_dispatch();
                    reader.flush_extension_snapshots(window, cx);
                })
                .ok();
            });
        }));
    }

    fn apply_theme_selection(&mut self, name: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.selected_theme = theme::apply_selection(name, window, cx);
        self.theme_preference = if self.selected_theme.is_some() {
            ThemePreference::Named
        } else {
            ThemePreference::System
        };
        self.update_render_appearance(window, cx);
    }

    fn invoke_extension_command(
        &mut self,
        action: &InvokeExtensionCommand,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let effects = match self
            .extensions
            .invoke_command(&action.command, action.payload.clone())
        {
            Ok(effects) => effects,
            Err(error) => {
                let detail = self
                    .extensions
                    .latest_diagnostic_message()
                    .unwrap_or_else(|| error.to_string());
                self.warning = Some(format!("Extension command failed: {detail}").into());
                cx.notify();
                return;
            }
        };
        self.execute_extension_effects(effects, window, cx);
        if self.extensions.has_pending_service_work()
            || self.extensions.has_pending_extension_work()
        {
            self.schedule_extension_snapshot_sync(window, cx);
        }
        #[cfg(feature = "installable-extensions")]
        self.refresh_extension_manager_state();
        crate::rebuild_application_menus(&mut self.extensions, cx);
    }

    fn execute_extension_effects(
        &mut self,
        effects: Vec<ArbitratedEffect>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.refresh_active_extension_view(window, cx);
        const MAX_COMPLETION_CHAIN: usize = 64;
        let mut pending = VecDeque::from(effects);
        let mut completed = 0;
        while let Some(effect) = pending.pop_front() {
            if completed >= MAX_COMPLETION_CHAIN {
                self.warning = Some("Extension effect chain exceeded the reader limit".into());
                break;
            }
            completed += 1;
            let result = match self.extensions.dispatch_service_effect(&effect) {
                ServiceDispatch::Immediate(result) => result,
                ServiceDispatch::Deferred => {
                    self.schedule_extension_snapshot_sync(window, cx);
                    continue;
                }
                ServiceDispatch::Unsupported => self.execute_extension_effect(&effect, window, cx),
            };
            match self.extensions.complete_effect(&effect, result) {
                Ok(report) => {
                    pending.extend(report.effects);
                    self.refresh_active_extension_view(window, cx);
                }
                Err(error) => {
                    self.warning = Some(format!("Extension completion failed: {error}").into());
                    break;
                }
            }
        }
        self.consume_pdf_extension_outputs(window, cx);
        cx.notify();
    }

    fn execute_extension_effect(
        &mut self,
        effect: &ArbitratedEffect,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> EffectResult {
        match &effect.request.effect {
            ExtensionEffect::CapabilityCall {
                capability,
                operation,
                input: DataValue::String(name),
            } if capability.as_str() == "key:ui/theme" && operation == "select" => {
                self.apply_theme_selection(name, window, cx);
                Ok(DataValue::Null)
            }
            ExtensionEffect::CapabilityCall {
                capability,
                operation,
                input,
            } if PdfCapability::from_name(capability.as_str()).is_some() => {
                let capability = PdfCapability::from_name(capability.as_str())
                    .expect("guard resolved a PDF capability");
                self.pdf_capabilities()
                    .call(capability, operation, input.clone())
                    .map_err(pdf_capability_extension_error)
            }
            ExtensionEffect::CopyText { text } => {
                cx.write_to_clipboard(ClipboardItem::new_string(text.clone()));
                Ok(DataValue::Null)
            }
            ExtensionEffect::OpenBrowserUrl { url } => open::that_detached(url)
                .map(|_| DataValue::Null)
                .map_err(|error| ExtensionError {
                    code: ExtensionErrorCode::Internal,
                    message: error.to_string(),
                    retryable: true,
                }),
            ExtensionEffect::OpenContribution { contribution } => {
                self.open_extension_contribution(contribution, window, cx)
            }
            ExtensionEffect::CloseContribution { contribution } => {
                self.close_extension_contribution(Some(contribution), window, cx)
            }
            _ => Err(ExtensionError {
                code: ExtensionErrorCode::CapabilityUnavailable,
                message: "the reader does not implement this extension effect yet".into(),
                retryable: false,
            }),
        }
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
        let search_was_open = self.sidebar.panel == SidePanel::Search && self.sidebar.target > 0.5;
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
        // A panel transition is a viewport slide. Holding zoom fixed avoids a
        // render-scale churn on every animation frame when Fit was last used.
        self.viewport.disable_fit_width();
        self.viewport.set_scroll(self.scroll);
        self.sync_viewport_snapshot();
        self.reference_panel.set_target(0.0);
        self.sidebar.toggle(panel);
        if search_was_open && (self.sidebar.panel != SidePanel::Search || self.sidebar.target < 0.5)
        {
            self.reset_search(cx);
        }
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
                self.scroll.x + self.panel_safe_viewport_width() * 0.5,
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

    fn quit_application(&mut self, _: &Quit, window: &mut Window, cx: &mut Context<Self>) {
        if self.comment_draft_needs_confirmation() {
            self.confirm_discard_comment(DraftDiscardAction::Quit, window, cx);
        } else {
            self.close_pdf_capability_generation();
            cx.quit();
        }
    }

    pub fn should_close_window(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        if !self.comment_draft_needs_confirmation() {
            self.close_pdf_capability_generation();
            return true;
        }
        self.confirm_discard_comment(DraftDiscardAction::CloseWindow, window, cx);
        false
    }

    fn content_top(&self) -> f32 {
        TOOLBAR_HEIGHT
            + if matches!(self.status, ReaderStatus::Error(_)) {
                ERROR_BAR_HEIGHT
            } else {
                0.0
            }
    }

    fn request_visible_tiles(&mut self, _window: &Window) {
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
        let (Some(_), Some(_)) = (self.layout(), self.document.as_ref()) else {
            return;
        };
        // `update_viewport()` has already synchronized the window scale. The
        // shared controller therefore owns both visibility and tile priority.
        let plan = self.viewport.plan_tiles();
        let planned = &plan.tiles;
        let mut viewport: Vec<_> = planned
            .iter()
            .map(|tile| (tile.request.key, tile.tier))
            .collect();
        viewport.sort_by_key(|(key, tier)| (*tier, *key));
        let text_viewport = plan.text_pages(MAX_CACHED_TEXT_PAGES);

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
            let tile_requests: Vec<_> = demand
                .iter()
                .map(|tile| TileRequest {
                    key: tile.request.key,
                    core_rect: tile.request.core_rect,
                    render_rect: tile.request.render_rect,
                })
                .collect();
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
        let mut images = self
            .rendered
            .drain()
            .map(|(_, tile)| tile.image)
            .collect::<Vec<_>>();
        if let Some(preview) = self.destination_preview.take() {
            images.push(preview.image);
        }
        Self::retire_images(images, window, cx);
    }

    fn clear_destination_preview(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(preview) = self.destination_preview.take() {
            Self::retire_images(vec![preview.image], window, cx);
        }
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
        let protected_link_pages = self
            .previewed_link
            .or(self.pending_link_navigation)
            .and_then(|id| {
                self.document
                    .as_ref()?
                    .links
                    .iter()
                    .find(|link| link.id == id)
                    .and_then(|link| match link.target {
                        PdfLinkTarget::Internal { page, .. } => {
                            Some([link.page, page].into_iter().collect::<HashSet<_>>())
                        }
                        PdfLinkTarget::External { .. } => None,
                    })
            })
            .unwrap_or_default();
        let mut candidates: Vec<_> = self.page_text.keys().copied().collect();
        candidates.retain(|page| {
            !self.text_viewport.contains(page) && !protected_link_pages.contains(page)
        });
        candidates.sort_by_key(|page| std::cmp::Reverse(page.abs_diff(current_page)));
        for page in candidates {
            if self.page_text.len() <= MAX_CACHED_TEXT_PAGES {
                break;
            }
            self.page_text.remove(&page);
        }
    }

    fn scroll_by(
        &mut self,
        x: f32,
        y: f32,
        immediate: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.navigation_focus.cancel();
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
        if immediate && !ui_is_animating {
            self.animation_active = false;
        }
        let disposition = self.viewport.scroll_by(
            Offset::new(x, y),
            if immediate {
                ViewportScrollBehavior::Immediate
            } else {
                ViewportScrollBehavior::Smooth
            },
        );
        self.sync_viewport_snapshot();
        if disposition == InputDisposition::IgnoredInvalid {
            return;
        }
        self.schedule_extension_snapshot_sync(window, cx);
        if immediate {
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

    fn link_card_pointer_is_animating(&self) -> bool {
        matches!(
            (self.link_card_pointer, self.link_card_pointer_target),
            (Some(current), Some(target))
                if (current.x - target.x).abs() + (current.y - target.y).abs() > 0.35
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
        let reference_was_animating = self.reference_panel.is_animating();
        if sidebar_was_animating {
            self.sidebar.advance(dt);
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
        if reference_was_animating {
            self.reference_panel.advance(dt);
            if !self.reference_panel.is_animating() && self.reference_panel.target() == 0.0 {
                self.reference_details = None;
                self.reference_details_group.clear();
            }
        }
        if sidebar_was_animating || reference_was_animating {
            self.update_sidebar_viewport_preserving_anchor(window);
            if !self.sidebar.is_animating() && !self.reference_panel.is_animating() {
                #[cfg(debug_assertions)]
                self.record_qa_sidebar_transition();
                self.sidebar_anchor = None;
            }
        }
        if self.link_card_expansion.is_animating() {
            self.link_card_expansion.advance(dt);
        }
        if let (Some(current), Some(target)) = (
            self.link_card_pointer.as_mut(),
            self.link_card_pointer_target,
        ) {
            current.x += (target.x - current.x) * blend;
            current.y += (target.y - current.y) * blend;
            if (current.x - target.x).abs() + (current.y - target.y).abs() < 0.35 {
                *current = target;
            }
        }
        if self.reference_citation_expansion.is_animating() {
            self.reference_citation_expansion.advance(dt);
        }
        if self.reference_details_transition.is_animating() {
            self.reference_details_transition.advance(dt);
        }
        if self.reference_summary_transition.is_animating() {
            self.reference_summary_transition.advance(dt);
        }
        #[cfg(feature = "installable-extensions")]
        if self.extension_manager_transition.is_animating() {
            self.extension_manager_transition.advance(dt);
            if !self.extension_manager_transition.is_animating()
                && self.extension_manager_transition.target() <= 0.0
            {
                self.extension_manager_page = None;
                self.extension_setting_inputs.clear();
            }
        }
        if self.extension_ui_panel.is_animating() {
            self.extension_ui_panel.advance(dt);
            if !self.extension_ui_panel.is_animating() && self.extension_ui_panel.target() <= 0.0 {
                self.extension_contribution = None;
            }
        }
        if self
            .doi_copy_started
            .is_some_and(|started| now.duration_since(started) >= DOI_COPY_FEEDBACK_DURATION)
        {
            self.doi_copy_started = None;
        }
        advance_toc_hover_state(
            &mut self.toc_hover_position,
            &mut self.toc_hover_strength,
            self.toc_hovered,
            blend,
        );
        if self.sidebar_anchor.is_none() {
            self.viewport.advance_navigation(dt);
            self.sync_viewport_snapshot();
        }
        let navigation_settled = !self.viewport.is_scrolling()
            && !self.sidebar.is_animating()
            && !self.comment_pane.is_animating()
            && !self.reference_panel.is_animating()
            && !self.reference_details_transition.is_animating()
            && !self.reference_citation_expansion.is_animating()
            && !self.reference_summary_transition.is_animating()
            && !self.extension_ui_panel.is_animating()
            && {
                #[cfg(feature = "installable-extensions")]
                {
                    !self.extension_manager_transition.is_animating()
                }
                #[cfg(not(feature = "installable-extensions"))]
                {
                    true
                }
            }
            && self.doi_copy_started.is_none()
            && !self.link_card_expansion.is_animating()
            && !self.link_card_pointer_is_animating()
            && !self.toc_hover_is_animating();
        let focus_animating = self.navigation_focus.advance(now, navigation_settled);
        if navigation_settled && !focus_animating {
            self.animation_active = false;
        }
        self.schedule_extension_snapshot_sync(window, cx);
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
        self.navigation_focus.cancel();
        self.sidebar_anchor = None;
        let raw_x = f32::from(window_position.x);
        let raw_y = f32::from(window_position.y);
        if self.document.is_none() || !zoom.is_finite() || !raw_x.is_finite() || !raw_y.is_finite()
        {
            return;
        }
        let local_x = raw_x.clamp(0.0, self.viewport_width);
        let local_y = (raw_y - self.content_top()).clamp(0.0, self.viewport_height);
        let disposition = self
            .viewport
            .zoom_at(zoom, ViewportPoint::new(local_x, local_y));
        self.sync_viewport_snapshot();
        if disposition != InputDisposition::Applied {
            return;
        }
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
        let previous_zoom = self.zoom;
        let disposition = self.viewport.fit_width();
        self.sync_viewport_snapshot();
        if disposition == InputDisposition::Applied && (self.zoom - previous_zoom).abs() > 0.0001 {
            self.defer_render_after_zoom(window, cx);
        }
        cx.notify();
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
        self.navigation_focus.cancel();
        self.viewport.scroll_to(
            Offset::new(self.scroll_target.x, 0.0),
            ViewportScrollBehavior::Smooth,
        );
        self.sync_viewport_snapshot();
        self.start_animation(window, cx);
    }

    fn last_page(&mut self, _: &LastPage, window: &mut Window, cx: &mut Context<Self>) {
        self.navigation_focus.cancel();
        if self.layout().is_some() {
            let maximum = self.viewport.maximum_scroll();
            self.viewport.scroll_to(
                Offset::new(self.scroll_target.x, maximum.y),
                ViewportScrollBehavior::Smooth,
            );
            self.sync_viewport_snapshot();
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
        self.pending_link_click =
            (event.button == MouseButton::Left && !event.modifiers.shift && event.click_count == 1)
                .then(|| self.hit_test_link(event.position))
                .flatten();
        if self.pending_link_click.is_some() {
            self.pending_annotation_click = None;
            self.active_annotation = None;
        } else if !event.modifiers.shift && event.click_count == 1 {
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
                if self.pending_link_click.is_some() {
                    self.selection = None;
                    self.selecting = false;
                } else if let Some(position) = self.hit_test_text(event.position, false) {
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
                self.navigation_focus.cancel();
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
        self.publish_pdf_selection();
        cx.notify();
    }

    fn on_mouse_move(
        &mut self,
        event: &MouseMoveEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(pan) = self.pan {
            self.navigation_focus.cancel();
            let delta = event.position - pan.pointer;
            self.viewport.set_scroll(Offset {
                x: pan.scroll.x - f32::from(delta.x),
                y: pan.scroll.y - f32::from(delta.y),
            });
            self.sync_viewport_snapshot();
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
            self.publish_pdf_selection();
            cx.notify();
            return;
        }

        // Bibliography entries own their full detected text range, including
        // DOI annotations embedded inside it. This keeps the paper card
        // stable instead of switching to a generic website card mid-line.
        let hovered_reference = self.hit_test_scientific_reference(event.position);
        let hovered_link = hovered_reference
            .is_none()
            .then(|| self.hit_test_link(event.position))
            .flatten();
        let detected = hovered_reference
            .map(PreviewTarget::Reference)
            .or_else(|| hovered_link.map(PreviewTarget::Link));
        self.hovered_link = hovered_link;
        self.hovered_reference = hovered_reference;
        let current = self.current_preview_target();
        if current.is_none() {
            if let Some(target) = detected {
                self.set_link_card_pointer_immediate(event.position);
                self.show_preview_target(target, window, cx);
            }
        } else if detected == current {
            self.link_source_hovered = true;
            self.cancel_pending_link_hover();
            self.schedule_link_card_reposition(event.position, window, cx);
        } else {
            // Do not hand the card to a neighboring link merely because the
            // pointer crossed its tight PDF bounds. The new target must remain
            // stable for the full settle interval first.
            self.link_source_hovered = false;
            self.schedule_stable_link_hover(detected, event.position, window, cx);
        }
        if current != detected {
            cx.notify();
        }
    }

    fn on_mouse_up(&mut self, event: &MouseUpEvent, window: &mut Window, cx: &mut Context<Self>) {
        let activated_link = if event.button == MouseButton::Left
            && let Some(id) = self.pending_link_click.take()
            && self.hit_test_link(event.position) == Some(id)
        {
            self.activate_document_link(id, window, cx);
            true
        } else {
            false
        };
        if !activated_link
            && event.button == MouseButton::Left
            && let Some(id) = self.pending_annotation_click.take()
        {
            self.active_annotation = Some(id);
            self.selection = None;
        }
        self.selecting = false;
        self.pan = None;
        self.publish_pdf_selection();
        self.schedule_extension_snapshot_sync(window, cx);
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
        self.publish_pdf_selection();
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
    ) -> Vec<PdfCanvasTile> {
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
            result.extend(tiles.into_iter().map(|(_, tile)| {
                PdfCanvasTile::new(
                    tile_logical_rect(page_rect, raster, tile.core_rect),
                    tile_logical_rect(page_rect, raster, tile.render_rect),
                    tile.image.clone(),
                )
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
        chrome_button(palette, id, label, style, enabled, handler)
    }

    fn icon_label(icon: IconName, label: impl IntoElement) -> gpui::AnyElement {
        icon_label(icon, label)
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

    fn perform_document_jump(
        &mut self,
        jump: DocumentJump,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(resolved) = self.layout().and_then(|layout| {
            jump.resolve(
                layout,
                self.scroll.x,
                self.viewport_width,
                self.viewport_height,
                self.max_scroll_x(layout),
            )
        }) else {
            return;
        };
        self.sidebar_anchor = None;
        self.navigation_focus.cancel();
        if let Some(focus) = resolved.focus {
            self.navigation_focus.queue(focus);
        }
        self.viewport.scroll_to(
            Offset {
                x: resolved.x,
                y: resolved.y,
            },
            ViewportScrollBehavior::Smooth,
        );
        self.sync_viewport_snapshot();
        self.start_animation(window, cx);
        cx.notify();
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
}

struct PaintPageOverlay {
    text: Option<Arc<TextLayer>>,
    annotations: Vec<PaintAnnotation>,
    search: Option<Arc<[SearchMatch]>>,
    extension_overlays: Vec<PaintExtensionOverlay>,
}

struct PaintExtensionOverlay {
    regions: Vec<TextBounds>,
    appearance: OverlayAppearance,
}

#[derive(Clone, Copy)]
struct PaintAnnotation {
    id: AnnotationId,
    range: TextRange,
    color: Option<HighlightColor>,
    has_comment: bool,
}

struct PaintSnapshot {
    palette: ReaderPalette,
    canvas: PdfCanvasSnapshot<PaintPageOverlay>,
    selection: Option<TextSelection>,
    active_annotation: Option<AnnotationId>,
    active_search: Option<SearchMatchId>,
    navigation_focus: Option<NavigationFocusFrame>,
}

fn normalized_bounds_in_page(page: Rect, bounds: TextBounds) -> Rect {
    Rect {
        x: page.x + bounds.left * page.width,
        y: page.y + bounds.top * page.height,
        width: (bounds.right - bounds.left).max(0.0) * page.width,
        height: (bounds.bottom - bounds.top).max(0.0) * page.height,
    }
}

fn paint_extension_overlay(
    region: Rect,
    appearance: OverlayAppearance,
    canvas: Bounds<Pixels>,
    scroll: Offset,
    palette: ReaderPalette,
    window: &mut Window,
) {
    let color = match appearance.tone {
        OverlayTone::Accent => palette.accent,
        OverlayTone::SearchMatch => palette.warning,
        OverlayTone::Positive => palette.green,
        OverlayTone::Caution => palette.yellow,
        OverlayTone::Critical => palette.error,
        OverlayTone::Neutral => palette.text_secondary,
    };
    let (fill_alpha, stroke_alpha, stroke_width) = match appearance.emphasis {
        OverlayEmphasis::Subtle => (0.12, 0.42, 1.0),
        OverlayEmphasis::Regular => (0.2, 0.64, 1.5),
        OverlayEmphasis::Strong => (0.3, 0.86, 2.0),
    };
    match appearance.shape {
        OverlayShape::Highlight => window.paint_quad(quad(
            content_rect_to_bounds(canvas, region, scroll),
            px((region.height * 0.14).clamp(1.0, 4.0)),
            color.opacity(fill_alpha),
            px(0.0),
            gpui::transparent_black(),
            Default::default(),
        )),
        OverlayShape::Outline => window.paint_quad(quad(
            content_rect_to_bounds(canvas, region, scroll),
            px((region.height * 0.14).clamp(1.0, 4.0)),
            gpui::transparent_black(),
            px(stroke_width),
            color.opacity(stroke_alpha),
            Default::default(),
        )),
        OverlayShape::Underline => {
            let height = stroke_width.max(1.0);
            let underline = Rect {
                y: region.bottom() - height,
                height,
                ..region
            };
            window.paint_quad(quad(
                content_rect_to_bounds(canvas, underline, scroll),
                px(height * 0.5),
                color.opacity(stroke_alpha),
                px(0.0),
                gpui::transparent_black(),
                Default::default(),
            ));
        }
        OverlayShape::Marker => {
            let size = region.height.clamp(5.0, 11.0);
            let marker = Rect {
                x: region.x - size * 0.65,
                y: region.y + (region.height - size) * 0.5,
                width: size,
                height: size,
            };
            window.paint_quad(quad(
                content_rect_to_bounds(canvas, marker, scroll),
                px(size * 0.5),
                color.opacity(stroke_alpha),
                px(0.0),
                gpui::transparent_black(),
                Default::default(),
            ));
        }
    }
}

fn paint_navigation_focus(
    frame: &NavigationFocusFrame,
    page: Rect,
    page_bounds: Bounds<Pixels>,
    canvas: Bounds<Pixels>,
    scroll: Offset,
    palette: ReaderPalette,
    window: &mut Window,
) {
    if frame.sweep <= 0.0 || frame.intensity <= 0.0 {
        return;
    }
    let fallback = TextBounds {
        left: 0.1,
        top: (frame.target.y_fraction - 0.004).clamp(0.0, 1.0),
        right: 0.42,
        bottom: (frame.target.y_fraction + 0.004).clamp(0.0, 1.0),
    };
    let runs = if frame.target.text_runs.is_empty() {
        std::slice::from_ref(&fallback)
    } else {
        frame.target.text_runs.as_slice()
    };
    let focus_color = match frame.target.tone {
        NavigationFocusTone::Accent => palette.accent,
        NavigationFocusTone::SearchMatch => palette.warning,
    };

    window.with_content_mask(
        Some(ContentMask {
            bounds: page_bounds,
        }),
        |window| {
            for run in runs {
                let run = normalized_bounds_in_page(page, *run);
                let horizontal_padding = (run.height * 0.28).clamp(2.0, 5.0);
                let vertical_padding = (run.height * 0.18).clamp(1.5, 3.5);
                let expanded = Rect {
                    x: run.x - horizontal_padding,
                    y: run.y - vertical_padding,
                    width: run.width + horizontal_padding * 2.0,
                    height: run.height + vertical_padding * 2.0,
                };
                if frame.target.motion == NavigationFocusMotion::Pulse {
                    let scaled = Rect {
                        x: expanded.x + expanded.width * (1.0 - frame.scale) * 0.5,
                        y: expanded.y + expanded.height * (1.0 - frame.scale) * 0.5,
                        width: expanded.width * frame.scale,
                        height: expanded.height * frame.scale,
                    };
                    window.paint_quad(quad(
                        content_rect_to_bounds(canvas, scaled, scroll),
                        px((scaled.height * 0.22).clamp(2.0, 6.0)),
                        focus_color.opacity(0.11 * frame.intensity),
                        px(1.0),
                        focus_color.opacity(0.22 * frame.intensity),
                        Default::default(),
                    ));
                    continue;
                }
                let vertical = expanded.height > expanded.width * 1.5;
                let swept_width = if vertical {
                    expanded.width
                } else {
                    expanded.width * frame.sweep
                };
                let swept_height = if vertical {
                    expanded.height * frame.sweep
                } else {
                    expanded.height
                };
                if swept_width <= 0.5 || swept_height <= 0.5 {
                    continue;
                }
                let radius = px((expanded.height * 0.22).clamp(2.0, 6.0));
                let swept = Rect {
                    width: swept_width,
                    height: swept_height,
                    ..expanded
                };
                window.paint_quad(quad(
                    content_rect_to_bounds(canvas, swept, scroll),
                    radius,
                    focus_color.opacity(0.105 * frame.intensity),
                    px(0.0),
                    gpui::transparent_black(),
                    Default::default(),
                ));

                let head_extent = if vertical {
                    (expanded.height * 0.055).clamp(3.0, 14.0).min(swept_height)
                } else {
                    (expanded.width * 0.055).clamp(3.0, 14.0).min(swept_width)
                };
                let head = if vertical {
                    Rect {
                        y: expanded.y + swept_height - head_extent,
                        width: swept_width,
                        height: head_extent,
                        ..expanded
                    }
                } else {
                    Rect {
                        x: expanded.x + swept_width - head_extent,
                        width: head_extent,
                        height: swept_height,
                        ..expanded
                    }
                };
                window.paint_quad(quad(
                    content_rect_to_bounds(canvas, head, scroll),
                    radius,
                    focus_color.opacity(0.24 * frame.intensity),
                    px(0.0),
                    gpui::transparent_black(),
                    Default::default(),
                ));
            }
        },
    );
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

#[cfg(test)]
fn desired_raster_size(page_rect: Rect, scale_factor: f32) -> RasterSize {
    viewport_raster_size(page_rect, scale_factor, &PdfReaderLimits::default())
}

#[cfg(test)]
fn plan_visible_tiles(
    layout: &DocumentLayout,
    page_sizes: &[PageSize],
    scroll: Offset,
    viewport_width: f32,
    viewport_height: f32,
    scale_factor: f32,
) -> Vec<key_pdf_gpui::PlannedTile> {
    viewport_plan_visible_tiles(
        layout,
        page_sizes,
        TilePlanningInput::new(
            Rect {
                x: scroll.x,
                y: scroll.y,
                width: viewport_width,
                height: viewport_height,
            },
            scale_factor,
        ),
        &PdfReaderLimits::default(),
    )
    .tiles
}

fn tile_core_rect(key: TileKey) -> Option<PixelRect> {
    viewport_tile_core_rect(key, &PdfReaderLimits::default())
}

#[cfg(test)]
fn inflate_tile_rect(core: PixelRect, raster: RasterSize) -> PixelRect {
    viewport_inflate_tile_rect(core, raster, &PdfReaderLimits::default())
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

fn fluid_sidebar_width(full_width: f32) -> f32 {
    SIDEBAR_WIDTH.min((full_width - FLUID_PANEL_HORIZONTAL_MARGIN * 2.0).max(0.0))
}

fn fluid_sidebar_extent(full_width: f32, progress: f32) -> f32 {
    (fluid_sidebar_width(full_width) + FLUID_PANEL_HORIZONTAL_MARGIN * 2.0)
        * progress.clamp(0.0, 1.0)
}

fn data_record<const N: usize>(items: [(&str, DataValue); N]) -> DataValue {
    DataValue::Record(
        items
            .into_iter()
            .map(|(key, value)| (key.to_owned(), value))
            .collect(),
    )
}

#[cfg(feature = "installable-extensions")]
fn resolve_extension_package_selection(path: PathBuf) -> PathBuf {
    if path.is_dir()
        && !path.join("manifest.toml").is_file()
        && path.join("package/manifest.toml").is_file()
    {
        path.join("package")
    } else {
        path
    }
}

fn bounded_i64(value: impl TryInto<i64>) -> i64 {
    value.try_into().unwrap_or(i64::MAX)
}

fn bounded_snapshot_string(value: &str, maximum_characters: usize) -> String {
    value.chars().take(maximum_characters).collect()
}

fn text_layer_statistics(text: &TextLayer) -> (usize, usize) {
    let mut words = 0usize;
    let mut in_word = false;
    for character in text.iter() {
        let whitespace = character.value.is_whitespace();
        if !whitespace && !in_word {
            words = words.saturating_add(1);
        }
        in_word = !whitespace;
    }
    (words, text.len())
}

#[cfg(feature = "installable-extensions")]
fn extension_permission_label(permission: &Permission) -> &'static str {
    match permission {
        Permission::ReadDocumentMetadata => "Read document metadata",
        Permission::ReadDocumentText(_) => "Read document text",
        Permission::ReadSelection => "Read selected text",
        Permission::NavigateDocument => "Navigate the document",
        Permission::AddDocumentOverlays => "Draw document overlays",
        Permission::AddSidePanel => "Add a side panel",
        Permission::ReadAnnotations => "Read annotations",
        Permission::WriteAnnotations => "Write annotations",
        Permission::MutateDocument(_) => "Modify document content",
        Permission::ClipboardWrite => "Copy to the clipboard",
        Permission::OpenExternalUrl => "Open external links",
        Permission::Storage(_) => "Use extension storage",
        Permission::Network(_) => "Access declared websites",
    }
}

#[cfg(feature = "installable-extensions")]
fn extension_state_presentation(
    state: LifecycleState,
    palette: ReaderPalette,
) -> (&'static str, Hsla) {
    match state {
        LifecycleState::Active => ("Active", palette.green),
        LifecycleState::Suspended => ("Suspended", palette.warning),
        LifecycleState::Failed => ("Failed", palette.error),
        LifecycleState::Disabled => ("Disabled", palette.text_secondary),
        LifecycleState::Installed | LifecycleState::Validated => ("Ready", palette.accent),
        LifecycleState::Unloading => ("Stopping", palette.warning),
        LifecycleState::Removed => ("Removed", palette.text_tertiary),
    }
}

#[cfg(feature = "installable-extensions")]
fn extension_setting_value<'a>(
    package: &'a InstalledPackageSummary,
    key: &str,
) -> Option<&'a DataValue> {
    let mut value = package.settings.as_ref()?;
    for segment in key.split('.') {
        let DataValue::Record(record) = value else {
            return None;
        };
        value = record.get(segment)?;
    }
    Some(value)
}

#[cfg(feature = "installable-extensions")]
fn extension_setting_text(value: &DataValue) -> String {
    match value {
        DataValue::String(value) => value.clone(),
        DataValue::Integer(value) => value.to_string(),
        DataValue::Number(value) => value.to_string(),
        _ => String::new(),
    }
}
