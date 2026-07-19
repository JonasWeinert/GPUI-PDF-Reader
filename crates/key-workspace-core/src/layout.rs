use crate::{TabId, ViewId};
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

    #[must_use]
    pub fn view_ids(&self) -> Vec<ViewId> {
        let mut views = Vec::new();
        self.visit_views(&mut |view| views.push(view));
        views
    }

    #[must_use]
    pub fn leaf_count(&self) -> usize {
        match self {
            Self::Empty => 0,
            Self::View { .. } => 1,
            Self::Split { first, second, .. } => first.leaf_count() + second.leaf_count(),
        }
    }
}

/// Stable top-level tab identity and the views placed inside it.
///
/// The core placement tree remains future-proof, while current application
/// hosts deliberately validate tabs as either one view or one two-view split.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct WorkspaceTabLayout {
    pub id: TabId,
    pub active_view: ViewId,
    pub content: ContentPlacement,
}

impl WorkspaceTabLayout {
    #[must_use]
    pub fn single(id: TabId, view_id: ViewId) -> Self {
        Self {
            id,
            active_view: view_id,
            content: ContentPlacement::View { view_id },
        }
    }

    #[must_use]
    pub fn split_pair(
        id: TabId,
        axis: SplitAxis,
        ratio: f32,
        first: ViewId,
        second: ViewId,
        active_view: ViewId,
    ) -> Option<Self> {
        let layout = Self {
            id,
            active_view,
            content: ContentPlacement::Split {
                axis,
                ratio: ratio.clamp(0.1, 0.9),
                first: Box::new(ContentPlacement::View { view_id: first }),
                second: Box::new(ContentPlacement::View { view_id: second }),
            },
        };
        layout.is_supported().then_some(layout)
    }

    /// Current product contract: exactly one view or two distinct views, with
    /// the active view contained in the placement and a finite split ratio.
    #[must_use]
    pub fn is_supported(&self) -> bool {
        let views = self.content.view_ids();
        let unique = views
            .iter()
            .copied()
            .collect::<std::collections::BTreeSet<_>>();
        let shape_is_supported = match &self.content {
            ContentPlacement::View { .. } => true,
            ContentPlacement::Split {
                ratio,
                first,
                second,
                ..
            } => {
                ratio.is_finite()
                    && (0.1..=0.9).contains(ratio)
                    && matches!(first.as_ref(), ContentPlacement::View { .. })
                    && matches!(second.as_ref(), ContentPlacement::View { .. })
            }
            ContentPlacement::Empty => false,
        };
        shape_is_supported
            && matches!(views.len(), 1 | 2)
            && unique.len() == views.len()
            && unique.contains(&self.active_view)
    }

    #[must_use]
    pub fn visible_views(&self) -> Vec<ViewId> {
        self.content.view_ids()
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

    #[test]
    fn current_tab_layout_accepts_one_view_or_one_distinct_pair() {
        let tab = TabId::from_raw(9);
        let first = ViewId::from_raw(1);
        let second = ViewId::from_raw(2);
        assert!(WorkspaceTabLayout::single(tab, first).is_supported());

        let split =
            WorkspaceTabLayout::split_pair(tab, SplitAxis::Horizontal, 0.98, first, second, second)
                .expect("distinct split pair");
        assert_eq!(split.visible_views(), vec![first, second]);
        let ContentPlacement::Split { ratio, .. } = split.content else {
            panic!("expected split placement");
        };
        assert_eq!(ratio, 0.9);

        assert!(
            WorkspaceTabLayout::split_pair(tab, SplitAxis::Horizontal, 0.5, first, first, first,)
                .is_none()
        );
    }
}
