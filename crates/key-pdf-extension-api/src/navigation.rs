use key_pdf_core::{
    DocumentJump, NavigationFocusMotion as CoreFocusMotion, NavigationFocusTone as CoreFocusTone,
    TextBounds,
};
use serde::{Deserialize, Serialize};

use crate::{DocumentHandle, NormalizedRect, PageIndex, SnapshotRevision, TextSelectionRange};

/// Bounded text hint used to refine a page-only destination at navigation time.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct TextTargetHint {
    /// Text to locate after Unicode whitespace normalization.
    pub text: String,
    /// Zero-based match occurrence to prefer when a page repeats the text.
    pub occurrence: u16,
}

/// A semantic destination within the current document.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct DocumentDestination {
    /// Zero-based destination page.
    pub page: PageIndex,
    /// Optional normalized horizontal destination supplied by the PDF.
    pub x_fraction: Option<f32>,
    /// Optional normalized vertical destination supplied by the PDF.
    pub y_fraction: Option<f32>,
    /// Optional stable text range when the target is known exactly.
    pub text_range: Option<TextSelectionRange>,
    /// Optional text used by the host to refine a page-only or rough target.
    pub text_hint: Option<TextTargetHint>,
}

impl DocumentDestination {
    /// Creates a page-only destination.
    #[must_use]
    pub const fn page(page: PageIndex) -> Self {
        Self {
            page,
            x_fraction: None,
            y_fraction: None,
            text_range: None,
            text_hint: None,
        }
    }

    /// Replaces rough coordinates with a host-resolved exact position.
    #[must_use]
    pub fn resolved(mut self, x_fraction: Option<f32>, y_fraction: Option<f32>) -> Self {
        self.x_fraction = x_fraction;
        self.y_fraction = y_fraction;
        self
    }
}

/// Placement policy applied after target refinement.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct NavigationPlacement {
    /// Target's vertical viewport anchor from top (`0`) to bottom (`1`).
    pub viewport_anchor_y: f32,
    /// Whether an explicit horizontal target should be centered.
    pub center_horizontal: bool,
}

impl Default for NavigationPlacement {
    fn default() -> Self {
        Self {
            viewport_anchor_y: 0.5,
            center_horizontal: true,
        }
    }
}

/// Semantic color of a transient navigation focus cue.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NavigationFocusTone {
    /// Theme accent used for general document navigation.
    #[default]
    Accent,
    /// Search-result highlight tone.
    SearchMatch,
}

/// Motion used for a transient navigation focus cue.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NavigationFocusMotion {
    /// Brief directional sweep over the target.
    #[default]
    Sweep,
    /// Brief scale-and-opacity pulse.
    Pulse,
}

/// Optional focus cue played after scrolling has settled.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct NavigationFocusRequest {
    /// Theme-derived focus color.
    pub tone: NavigationFocusTone,
    /// Bounded host-owned motion preset.
    pub motion: NavigationFocusMotion,
    /// Target text regions; an empty list focuses the resolved page position.
    pub regions: Vec<NormalizedRect>,
}

/// Semantic request to navigate the current PDF viewport.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct NavigationRequest {
    /// Document to navigate.
    pub document: DocumentHandle,
    /// Rough or exact semantic target.
    pub destination: DocumentDestination,
    /// Viewport placement policy.
    pub placement: NavigationPlacement,
    /// Optional transient cue that begins only after movement settles.
    pub focus: Option<NavigationFocusRequest>,
}

impl NavigationRequest {
    /// Builds the shared `key-pdf-core` jump after a provider has applied any
    /// requested text refinement to `destination`.
    #[must_use]
    pub fn to_document_jump(&self) -> DocumentJump {
        let mut jump = DocumentJump::new(self.destination.page.into())
            .position(self.destination.x_fraction, self.destination.y_fraction)
            .viewport_anchor_y(self.placement.viewport_anchor_y)
            .center_horizontal(self.placement.center_horizontal);
        if let Some(focus) = &self.focus {
            let bounds = focus
                .regions
                .iter()
                .copied()
                .map(TextBounds::from)
                .collect();
            jump = jump.focus(bounds, focus.tone.into(), focus.motion.into());
        }
        jump
    }
}

impl From<NavigationFocusTone> for CoreFocusTone {
    fn from(tone: NavigationFocusTone) -> Self {
        match tone {
            NavigationFocusTone::Accent => Self::Accent,
            NavigationFocusTone::SearchMatch => Self::SearchMatch,
        }
    }
}

impl From<NavigationFocusMotion> for CoreFocusMotion {
    fn from(motion: NavigationFocusMotion) -> Self {
        match motion {
            NavigationFocusMotion::Sweep => Self::Sweep,
            NavigationFocusMotion::Pulse => Self::Pulse,
        }
    }
}

/// Confirmation that the host accepted a navigation request.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct NavigationReceipt {
    /// Document that accepted the request.
    pub document: DocumentHandle,
    /// Monotonic viewport revision resulting from the request.
    pub viewport_revision: SnapshotRevision,
    /// Refined destination actually used by the host.
    pub destination: DocumentDestination,
    /// Whether a post-scroll focus cue was scheduled.
    pub focus_scheduled: bool,
}
