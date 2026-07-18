//! Engine-independent PDF session coordination.
//!
//! The runtime defines the narrow contract between a PDF engine adapter and
//! product/UI code: document generations, cancellation, typed demands and
//! events, opaque resources, bounded latest-wins scheduling, and cache policy.
//! It contains no PDFium, GPUI, filesystem, or network integration.

#![forbid(unsafe_code)]

mod cache;
mod cancellation;
mod demand;
mod engine;
mod scheduler;
mod session;
mod supervisor;

pub use cache::{CachePolicy, CachePolicyError};
pub use cancellation::{CancellationSource, CancellationToken, Cancelled};
pub use demand::{
    ColorMode, DemandError, DemandIntent, DemandPriority, DemandStamp, PixelColor, PreviewDemand,
    RenderDemand, RenderDemandKey, TextDemand, TextDemandPurpose,
};
pub use engine::{
    DocumentDescriptor, DocumentEvent, DocumentResource, EngineCapabilities, EngineDocument,
    EngineFailure, EngineOutputError, PdfEngine, PdfRuntime, PixelFormat, PreviewEvent,
    PreviewResource, RasterImage, RasterImageError, RenderEvent, RenderResource, RenderedPreview,
    RenderedTile, RuntimeFailure, TextEvent, TextPage, TextResource,
};
pub use scheduler::{
    CompletionDisposition, DemandRevision, LatestWinsQueue, ScheduleOutcome, ScheduledDemand,
    SchedulerError,
};
pub use session::{
    AllocationError, DocumentGeneration, DocumentSession, DocumentSessionManager, RequestId,
    ResourceHandle, ResourceId, SessionError,
};
pub use supervisor::{
    DocumentClient, EngineSupervisor, SupervisorDocumentId, SupervisorEvent, SupervisorEvents,
    SupervisorPolicy, SupervisorPolicyError, SupervisorSendError, WorkClass,
    start_engine_supervisor,
};
