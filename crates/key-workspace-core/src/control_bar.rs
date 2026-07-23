use crate::ViewId;
use serde::{Deserialize, Serialize};
use std::ops::Range;

/// Stable, host-routable identity for a control supplied by a view or extension.
#[derive(Clone, Debug, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(transparent)]
pub struct ControlId(String);

impl ControlId {
    #[must_use]
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl From<&str> for ControlId {
    fn from(value: &str) -> Self {
        Self::new(value)
    }
}

impl From<String> for ControlId {
    fn from(value: String) -> Self {
        Self::new(value)
    }
}

/// Semantic icons understood by every Key host. The model deliberately avoids
/// exposing a particular UI toolkit's icon type to views or extensions.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ControlIcon {
    Add,
    Close,
    Comments,
    Document,
    FitWidth,
    Minus,
    Moon,
    Next,
    Previous,
    Search,
    Settings,
    Split,
    Sun,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ControlBarRegion {
    Leading,
    Center,
    Trailing,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ControlBarItemKind {
    Display,
    Button,
    TextInput,
}

/// How an item consumes horizontal space in a control-bar host.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ControlBarWidth {
    /// Keep the presentation's measured width.
    #[default]
    Intrinsic,
    /// Consume the space left after intrinsic items have been laid out.
    Max,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct ControlBarPresentation {
    pub label: String,
    pub short_label: Option<String>,
    pub icon: Option<ControlIcon>,
    pub tooltip: Option<String>,
    /// Estimated widths for full, compact, and icon-only presentation.
    pub widths: [f32; 3],
    /// Lower-priority items collapse before higher-priority items.
    pub priority: u16,
    /// Lets one view-owned control act as the control bar's flexible location.
    pub width: ControlBarWidth,
    /// Keeps a button icon-only at every responsive width.
    #[serde(default)]
    pub icon_only: bool,
}

impl ControlBarPresentation {
    #[must_use]
    pub fn new(label: impl Into<String>, widths: [f32; 3], priority: u16) -> Self {
        Self {
            label: label.into(),
            short_label: None,
            icon: None,
            tooltip: None,
            widths,
            priority,
            width: ControlBarWidth::Intrinsic,
            icon_only: false,
        }
    }

    #[must_use]
    pub fn short_label(mut self, label: impl Into<String>) -> Self {
        self.short_label = Some(label.into());
        self
    }

    #[must_use]
    pub fn icon(mut self, icon: ControlIcon) -> Self {
        self.icon = Some(icon);
        self
    }

    #[must_use]
    pub fn tooltip(mut self, tooltip: impl Into<String>) -> Self {
        self.tooltip = Some(tooltip.into());
        self
    }

    /// Requests all remaining horizontal space in the control-bar host.
    #[must_use]
    pub fn max_width(mut self) -> Self {
        self.width = ControlBarWidth::Max;
        self
    }

    #[must_use]
    pub fn icon_only(mut self) -> Self {
        self.icon_only = true;
        self
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ControlBarItemState {
    pub visible: bool,
    pub enabled: bool,
    pub selected: bool,
    pub expanded: bool,
    pub loading: bool,
    pub value: Option<String>,
}

impl Default for ControlBarItemState {
    fn default() -> Self {
        Self {
            visible: true,
            enabled: true,
            selected: false,
            expanded: false,
            loading: false,
            value: None,
        }
    }
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct ControlBarItem {
    pub id: ControlId,
    pub region: ControlBarRegion,
    pub kind: ControlBarItemKind,
    pub presentation: ControlBarPresentation,
    pub state: ControlBarItemState,
}

impl ControlBarItem {
    #[must_use]
    pub fn new(
        id: impl Into<ControlId>,
        region: ControlBarRegion,
        kind: ControlBarItemKind,
        presentation: ControlBarPresentation,
    ) -> Self {
        Self {
            id: id.into(),
            region,
            kind,
            presentation,
            state: ControlBarItemState::default(),
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ControlBarCard {
    pub id: ControlId,
    pub eyebrow: Option<String>,
    pub text: String,
    pub emphasized: Option<Range<usize>>,
    pub selected: bool,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ControlBarAuxiliary {
    pub label: String,
    pub loading: bool,
    pub cards: Vec<ControlBarCard>,
}

/// Immutable projection of the command-active view into window chrome.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct ControlBarSnapshot {
    pub owner: ViewId,
    pub revision: u64,
    pub items: Vec<ControlBarItem>,
    pub auxiliary: Option<ControlBarAuxiliary>,
}

impl ControlBarSnapshot {
    #[must_use]
    pub fn new(owner: ViewId, revision: u64) -> Self {
        Self {
            owner,
            revision,
            items: Vec::new(),
            auxiliary: None,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ControlBarEvent {
    pub owner: ViewId,
    pub revision: u64,
    pub control: ControlId,
    pub interaction: ControlBarInteraction,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ControlBarInteraction {
    Pressed,
    ValueChanged(String),
    Submitted,
    Cancelled,
    ActivatedCard,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snapshot_round_trips_without_ui_toolkit_types() {
        let mut snapshot = ControlBarSnapshot::new(ViewId::from_raw(9), 3);
        snapshot.items.push(ControlBarItem::new(
            "pdf.search",
            ControlBarRegion::Trailing,
            ControlBarItemKind::Button,
            ControlBarPresentation::new("Search", [94.0, 72.0, 32.0], 80)
                .short_label("Find")
                .icon(ControlIcon::Search),
        ));
        let encoded = serde_json::to_string(&snapshot).expect("snapshot serializes");
        let decoded: ControlBarSnapshot =
            serde_json::from_str(&encoded).expect("snapshot deserializes");
        assert_eq!(decoded, snapshot);
        assert!(!encoded.contains("gpui"));
    }
}
