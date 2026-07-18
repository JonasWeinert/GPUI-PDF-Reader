use super::{
    PreviewRequest, QueuedRender, RenderTier, WorkerState, accept_available_commands,
    accept_deferred_commands, cache_text_layer, collect_available_commands,
    command_supersedes_text,
};
use crate::backend::protocol::{WorkerCommand, WorkerEvent};
use crate::model::{TextLayer, TileKey};
use key_pdf_runtime::{
    CancellationToken, CompletionDisposition, DemandIntent, DemandPriority, PixelFormat,
    PreviewEvent, RenderEvent, ScheduledDemand, TextDemandPurpose, TextEvent,
};
use key_pdfium::PdfiumLibraryConfig;
use std::collections::{BTreeSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::{Arc, mpsc};

pub(super) fn pdfium_library_config() -> PdfiumLibraryConfig {
    let mut candidates = Vec::new();
    if let Some(configured) = std::env::var_os("PDFIUM_DYNAMIC_LIB_PATH") {
        candidates.push(PathBuf::from(configured));
    }
    if let Ok(executable) = std::env::current_exe()
        && let Some(directory) = executable.parent()
    {
        candidates.push(directory.to_path_buf());
        candidates.push(directory.join("../Resources"));
    }
    candidates.push(
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../..")
            .join("vendor/pdfium/lib"),
    );
    PdfiumLibraryConfig::new(candidates).with_system_fallback(true)
}

pub(super) fn process_render_work(
    tier: RenderTier,
    scheduled: ScheduledDemand<TileKey, QueuedRender>,
    commands: &mpsc::Receiver<WorkerCommand>,
    events: &mpsc::SyncSender<WorkerEvent>,
    state: &mut WorkerState,
    explicit_text: &mut BTreeSet<usize>,
    automatic_text: &mut VecDeque<usize>,
) -> bool {
    let operation_cancellation = state
        .document_cancellation
        .token()
        .combined(&scheduled.value().cancellation.token())
        .combined(&scheduled.cancellation());
    let runtime_event = state
        .runtime
        .render_with_cancellation(scheduled.value().demand.clone(), &operation_cancellation);

    // Apply replacement viewport/open commands before deciding whether an
    // in-flight completion is still current.
    if !accept_available_commands(commands, events, state, explicit_text, automatic_text) {
        return false;
    }
    if state.renders.finish(tier, &scheduled) != CompletionDisposition::Publish {
        return true;
    }

    let request = &scheduled.value().request;
    if state.generation != Some(request.generation) {
        return true;
    }
    match runtime_event {
        RenderEvent::Ready { tile, .. } => {
            if tile.image.format() != PixelFormat::Bgra8Premultiplied {
                return events
                    .send(WorkerEvent::TileFailed {
                        generation: request.generation,
                        appearance: request.appearance,
                        key: request.tile.key,
                        message: "PDF engine returned an unsupported pixel format".into(),
                    })
                    .is_ok();
            }
            events
                .send(WorkerEvent::TileRendered {
                    generation: request.generation,
                    appearance: request.appearance,
                    key: request.tile.key,
                    core_rect: request.tile.core_rect,
                    render_rect: request.tile.render_rect,
                    width: tile.image.width(),
                    height: tile.image.height(),
                    bgra: tile.image.pixels().to_vec(),
                })
                .is_ok()
        }
        RenderEvent::Failed { error, .. } => events
            .send(WorkerEvent::TileFailed {
                generation: request.generation,
                appearance: request.appearance,
                key: request.tile.key,
                message: format!(
                    "Could not render page {}: {error}",
                    request.tile.key.page + 1
                ),
            })
            .is_ok(),
        RenderEvent::Cancelled { .. } | RenderEvent::Discarded { .. } => true,
    }
}

pub(super) fn process_preview_work(
    scheduled: ScheduledDemand<(), PreviewRequest>,
    commands: &mpsc::Receiver<WorkerCommand>,
    events: &mpsc::SyncSender<WorkerEvent>,
    state: &mut WorkerState,
    explicit_text: &mut BTreeSet<usize>,
    automatic_text: &mut VecDeque<usize>,
) -> bool {
    let operation_cancellation = state
        .document_cancellation
        .token()
        .combined(&scheduled.value().cancellation.token())
        .combined(&scheduled.cancellation());
    let runtime_event = state.runtime.render_preview_with_cancellation(
        scheduled.value().demand.clone(),
        &operation_cancellation,
    );
    if !accept_available_commands(commands, events, state, explicit_text, automatic_text) {
        return false;
    }
    if state.previews.finish(&scheduled) != CompletionDisposition::Publish {
        return true;
    }

    let request = scheduled.value();
    if state.generation != Some(request.generation) {
        return true;
    }
    match runtime_event {
        PreviewEvent::Ready { preview, .. } => {
            if preview.image.format() != PixelFormat::Bgra8Premultiplied {
                return events
                    .send(WorkerEvent::PreviewFailed {
                        generation: request.generation,
                        revision: request.revision,
                        message: "PDF engine returned an unsupported pixel format".into(),
                    })
                    .is_ok();
            }
            events
                .send(WorkerEvent::PreviewRendered {
                    generation: request.generation,
                    revision: request.revision,
                    appearance: request.appearance,
                    width: preview.image.width(),
                    height: preview.image.height(),
                    bgra: preview.image.pixels().to_vec(),
                })
                .is_ok()
        }
        PreviewEvent::Failed { error, .. } => events
            .send(WorkerEvent::PreviewFailed {
                generation: request.generation,
                revision: request.revision,
                message: error.to_string(),
            })
            .is_ok(),
        PreviewEvent::Cancelled { .. } | PreviewEvent::Discarded { .. } => true,
    }
}

#[allow(clippy::too_many_arguments)]
pub(super) fn process_text_work(
    page: usize,
    explicit: bool,
    commands: &mpsc::Receiver<WorkerCommand>,
    events: &mpsc::SyncSender<WorkerEvent>,
    state: &mut WorkerState,
    explicit_text: &mut BTreeSet<usize>,
    automatic_text: &mut VecDeque<usize>,
) -> bool {
    let Some(generation) = state.generation else {
        return true;
    };
    if let Some(text) = state.text_cache.get(&page).cloned() {
        return events
            .send(WorkerEvent::TextExtracted {
                generation,
                page,
                text,
            })
            .is_ok();
    }

    let purpose = if explicit {
        TextDemandPurpose::Copy
    } else {
        TextDemandPurpose::VisibleLayer
    };
    let operation_cancellation = if explicit {
        state.explicit_text_cancellation.token()
    } else {
        state.automatic_text_cancellation.token()
    };
    let extracted = extract_runtime_text(state, page, purpose, &operation_cancellation);

    let mut deferred = Vec::new();
    if !collect_available_commands(commands, &mut deferred) {
        return false;
    }
    let viewport_changed = deferred.iter().any(|command| {
        matches!(
            command,
            WorkerCommand::Open { .. } | WorkerCommand::RenderViewport { .. }
        )
    });
    let text_superseded = deferred
        .iter()
        .any(|command| command_supersedes_text(command, page, explicit));
    if !accept_deferred_commands(deferred, events, state, explicit_text, automatic_text) {
        return false;
    }
    if state.generation != Some(generation) {
        return true;
    }

    let (text, warning) = cache_text_layer(
        &mut state.text_cache,
        state.runtime.cache_policy().text_pages(),
        page,
        || extracted,
    );
    if text_superseded || (viewport_changed && !explicit && !automatic_text.contains(&page)) {
        return true;
    }
    match (text, warning) {
        (Some(text), None) => events
            .send(WorkerEvent::TextExtracted {
                generation,
                page,
                text,
            })
            .is_ok(),
        (Some(_), Some(message)) => events
            .send(WorkerEvent::TextFailed {
                generation,
                page,
                message,
            })
            .is_ok(),
        (None, None) => true,
        (None, Some(_)) => unreachable!("a text warning always carries a cached empty layer"),
    }
}

pub(super) fn extract_runtime_text(
    state: &mut WorkerState,
    page: usize,
    purpose: TextDemandPurpose,
    operation_cancellation: &CancellationToken,
) -> Result<Arc<TextLayer>, String> {
    let session = state
        .runtime
        .session()
        .ok_or_else(|| "no document is open".to_owned())?;
    let (priority, intent) = match purpose {
        TextDemandPurpose::Copy | TextDemandPurpose::LinkResolution => {
            (DemandPriority::INTERACTIVE, DemandIntent::Explicit)
        }
        TextDemandPurpose::VisibleLayer => (DemandPriority::VISIBLE, DemandIntent::Visible),
        TextDemandPurpose::Search | TextDemandPurpose::DocumentAnalysis => {
            (DemandPriority::BACKGROUND, DemandIntent::Background)
        }
    };
    let demand = session
        .text_demand(page, purpose, priority, intent)
        .map_err(|error| error.to_string())?;
    let cancellation = state
        .document_cancellation
        .token()
        .combined(operation_cancellation);
    match state
        .runtime
        .extract_text_with_cancellation(demand, &cancellation)
    {
        TextEvent::Ready { text, .. } => Ok(text.layer),
        TextEvent::Failed { error, .. } => Err(error.to_string()),
        TextEvent::Cancelled { .. } => Err("text extraction was cancelled".into()),
        TextEvent::Discarded { .. } => Err("text extraction belongs to a stale document".into()),
    }
}
