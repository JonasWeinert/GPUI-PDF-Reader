use key_extension_api::{CommandId, LocalId};
use serde::{Deserialize, Serialize};

use crate::{
    DocumentHandle, InputErrorKind, NormalizedRect, OverlaySetHandle, PageIndex, PdfExtensionError,
    PdfExtensionResult, SnapshotRevision,
};

/// Host-rendered overlay primitive.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OverlayShape {
    /// Translucent fill behind or over document content.
    #[default]
    Highlight,
    /// Theme-derived outline around each region.
    Outline,
    /// Theme-derived underline at the bottom of each region.
    Underline,
    /// Compact marker anchored to each region.
    Marker,
}

/// Theme-derived overlay tone; extensions never supply raw display colors.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OverlayTone {
    /// Host theme accent.
    #[default]
    Accent,
    /// Current search-result tone.
    SearchMatch,
    /// Positive or completed state.
    Positive,
    /// Caution state.
    Caution,
    /// Critical state.
    Critical,
    /// Neutral foreground tone.
    Neutral,
}

/// Host-owned emphasis preset for overlay strokes and fills.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OverlayEmphasis {
    /// Quiet secondary treatment.
    Subtle,
    /// Standard treatment.
    #[default]
    Regular,
    /// Stronger accessible treatment.
    Strong,
}

/// Optional bounded one-shot overlay motion.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OverlayMotion {
    /// No animation.
    #[default]
    None,
    /// Host-controlled brief pulse.
    PulseOnce,
    /// Host-controlled brief sweep.
    SweepOnce,
}

/// Semantic appearance for a trusted host-rendered overlay.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct OverlayAppearance {
    /// Primitive used to paint each region.
    pub shape: OverlayShape,
    /// Theme-derived color role.
    pub tone: OverlayTone,
    /// Theme-derived visual weight.
    pub emphasis: OverlayEmphasis,
    /// Optional bounded one-shot motion.
    pub motion: OverlayMotion,
}

/// One extension-owned overlay item.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct DocumentOverlay {
    /// Stable extension-local identity used for atomic replacement and events.
    pub id: LocalId,
    /// Zero-based page index.
    pub page: PageIndex,
    /// One or more normalized page regions.
    pub regions: Vec<NormalizedRect>,
    /// Host-rendered appearance.
    pub appearance: OverlayAppearance,
    /// Optional accessible and hover-detail label.
    pub label: Option<String>,
    /// Optional declared extension command invoked when the overlay is clicked.
    pub command: Option<CommandId>,
}

/// Atomic replacement of an extension's overlays for one document.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct OverlayBatch {
    /// Document that owns the overlay coordinate space.
    pub document: DocumentHandle,
    /// Extension-controlled monotonic revision used to reject stale updates.
    pub revision: SnapshotRevision,
    /// Complete replacement set; an empty vector clears all owned overlays.
    pub overlays: Vec<DocumentOverlay>,
}

impl OverlayBatch {
    /// Checks the extension-controlled revision before replacing retained data.
    ///
    /// # Errors
    ///
    /// Returns `InvalidInput` with `StaleRevision` when this batch does not
    /// advance beyond the previously accepted revision.
    pub fn validate_revision_after(
        &self,
        previous: Option<SnapshotRevision>,
    ) -> PdfExtensionResult<()> {
        if previous.is_some_and(|previous| self.revision <= previous) {
            Err(PdfExtensionError::InvalidInput {
                field: "overlays.revision".into(),
                reason: InputErrorKind::StaleRevision,
            })
        } else {
            Ok(())
        }
    }
}

/// Confirmation that an overlay replacement was accepted.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct OverlayReceipt {
    /// Host-issued identity for this accepted set.
    pub handle: OverlaySetHandle,
    /// Accepted extension revision.
    pub revision: SnapshotRevision,
    /// Number of accepted overlay items.
    pub overlay_count: u32,
}
