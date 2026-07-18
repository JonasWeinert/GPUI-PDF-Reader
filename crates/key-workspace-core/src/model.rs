use crate::{Generation, ItemId, ViewId, WindowId};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::path::PathBuf;

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ItemKind {
    Pdf,
    Markdown,
    Video,
    Settings,
    Custom(String),
}

#[derive(Clone, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Capability {
    Read,
    Edit,
    Search,
    Annotate,
    NavigateOutline,
    Playback,
    Export,
    Settings,
    Custom(String),
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case", tag = "kind", content = "value")]
pub enum ItemSource {
    File(PathBuf),
    Transient,
    Virtual(String),
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct WorkspaceItemDescriptor {
    pub id: ItemId,
    pub generation: Generation,
    pub kind: ItemKind,
    pub title: String,
    pub source: ItemSource,
    pub capabilities: BTreeSet<Capability>,
}

impl WorkspaceItemDescriptor {
    pub fn supports(&self, capability: &Capability) -> bool {
        self.capabilities.contains(capability)
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct WorkspaceViewDescriptor {
    pub id: ViewId,
    pub generation: Generation,
    /// None for host-owned views such as application settings.
    pub item_id: Option<ItemId>,
    pub kind: ItemKind,
    pub title: String,
    pub capabilities: BTreeSet<Capability>,
}

impl WorkspaceViewDescriptor {
    pub fn supports(&self, capability: &Capability) -> bool {
        self.capabilities.contains(capability)
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct WorkspaceWindowDescriptor {
    pub id: WindowId,
    pub title: String,
    pub active_view: Option<ViewId>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn host_owned_views_do_not_need_fake_items() {
        let view = WorkspaceViewDescriptor {
            id: ViewId::from_raw(1),
            generation: Generation::INITIAL,
            item_id: None,
            kind: ItemKind::Settings,
            title: "Settings".into(),
            capabilities: BTreeSet::from([Capability::Settings]),
        };

        assert_eq!(view.item_id, None);
        assert!(view.supports(&Capability::Settings));
    }
}
