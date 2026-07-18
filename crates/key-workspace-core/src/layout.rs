use crate::ViewId;
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SplitAxis {
    Horizontal,
    Vertical,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "snake_case", tag = "type")]
pub enum ContentPlacement {
    Empty,
    View {
        view_id: ViewId,
    },
    Split {
        axis: SplitAxis,
        /// Fraction allocated to `first`, clamped by consumers to 0.1..=0.9.
        ratio: f32,
        first: Box<ContentPlacement>,
        second: Box<ContentPlacement>,
    },
}

impl ContentPlacement {
    pub fn visit_views(&self, visit: &mut impl FnMut(ViewId)) {
        match self {
            Self::Empty => {}
            Self::View { view_id } => visit(*view_id),
            Self::Split { first, second, .. } => {
                first.visit_views(visit);
                second.visit_views(visit);
            }
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct DockPanel {
    /// Stable contribution identifier, for example `core.file_tree`.
    pub id: String,
    pub title: String,
    pub visible: bool,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct DockRegion {
    pub open: bool,
    pub extent: f32,
    pub panels: Vec<DockPanel>,
    pub active_panel: Option<String>,
}

impl DockRegion {
    pub fn closed(extent: f32) -> Self {
        Self {
            open: false,
            extent,
            panels: Vec::new(),
            active_panel: None,
        }
    }
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct WindowDocks {
    pub left: DockRegion,
    pub right: DockRegion,
    pub bottom: DockRegion,
}

impl Default for WindowDocks {
    fn default() -> Self {
        Self {
            left: DockRegion::closed(240.0),
            right: DockRegion::closed(320.0),
            bottom: DockRegion::closed(220.0),
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct SurfaceSlot {
    pub id: String,
    pub owner: Option<ViewId>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct WorkspaceLayout {
    pub docks: WindowDocks,
    pub content: ContentPlacement,
    /// Non-modal surfaces ordered back to front.
    pub overlays: Vec<SurfaceSlot>,
    /// At most one modal surface receives input.
    pub modal: Option<SurfaceSlot>,
}

impl WorkspaceLayout {
    pub fn single_view(view_id: ViewId) -> Self {
        Self {
            docks: WindowDocks::default(),
            content: ContentPlacement::View { view_id },
            overlays: Vec::new(),
            modal: None,
        }
    }

    pub fn contains_view(&self, needle: ViewId) -> bool {
        let mut found = false;
        self.content
            .visit_views(&mut |view| found |= view == needle);
        found
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn placement_tree_visits_every_leaf() {
        let layout = WorkspaceLayout {
            docks: WindowDocks::default(),
            content: ContentPlacement::Split {
                axis: SplitAxis::Horizontal,
                ratio: 0.4,
                first: Box::new(ContentPlacement::View {
                    view_id: ViewId::from_raw(1),
                }),
                second: Box::new(ContentPlacement::View {
                    view_id: ViewId::from_raw(2),
                }),
            },
            overlays: vec![],
            modal: None,
        };

        assert!(layout.contains_view(ViewId::from_raw(1)));
        assert!(layout.contains_view(ViewId::from_raw(2)));
        assert!(!layout.contains_view(ViewId::from_raw(3)));
    }
}
