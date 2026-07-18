use serde::{Deserialize, Serialize};

use crate::{
    BooleanSource, CommandId, ContributionId, DataValue, LocalId, MenuSlotId, PackagePath,
    StateBinding,
};

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct ContributionSet {
    #[serde(default)]
    pub commands: Vec<CommandDefinition>,
    #[serde(default)]
    pub menus: Vec<MenuContribution>,
    #[serde(default)]
    pub views: Vec<UiContribution>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct CommandDefinition {
    pub id: CommandId,
    pub title: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub category: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct UiContribution {
    pub id: ContributionId,
    pub slot: ContributionSlot,
    #[serde(default)]
    pub order: ContributionOrder,
    pub root: UiNode,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ContributionSlot {
    CommandPalette,
    TopToolbar,
    PdfFloatingToolbar,
    SelectionContextPill,
    DocumentOverlay,
    HoverCard,
    SidePanel,
    ContextMenu,
    StatusArea,
    SettingsPanel,
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct ContributionOrder {
    #[serde(default)]
    pub priority: i16,
    #[serde(default)]
    pub before: Vec<ContributionId>,
    #[serde(default)]
    pub after: Vec<ContributionId>,
}

/// A contribution to a host-owned named menubar slot. Nested `Submenu` items
/// model folders; the host controls their native rendering and maximum depth.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct MenuContribution {
    pub id: ContributionId,
    pub slot: MenuSlotId,
    #[serde(default)]
    pub order: ContributionOrder,
    pub items: Vec<MenuItem>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct MenuItem {
    pub id: LocalId,
    #[serde(default)]
    pub order: MenuItemOrder,
    #[serde(default)]
    pub visible: BooleanSource,
    #[serde(flatten)]
    pub kind: MenuItemKind,
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct MenuItemOrder {
    #[serde(default)]
    pub priority: i16,
    #[serde(default)]
    pub before: Vec<LocalId>,
    #[serde(default)]
    pub after: Vec<LocalId>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum MenuItemKind {
    Command {
        label: String,
        command: CommandId,
        icon: Option<IconRef>,
        #[serde(default)]
        enabled: BooleanSource,
        checked: Option<BooleanSource>,
    },
    Submenu {
        label: String,
        icon: Option<IconRef>,
        children: Vec<MenuItem>,
    },
    Separator,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "value", rename_all = "snake_case")]
pub enum IconRef {
    Host(String),
    Asset(PackagePath),
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct UiNode {
    pub id: LocalId,
    #[serde(default)]
    pub visible: BooleanSource,
    #[serde(flatten)]
    pub kind: UiNodeKind,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum UiNodeKind {
    Column {
        children: Vec<UiNode>,
    },
    Row {
        children: Vec<UiNode>,
    },
    Stack {
        children: Vec<UiNode>,
    },
    Text {
        text: String,
        selectable: bool,
    },
    StyledText {
        spans: Vec<TextSpan>,
        selectable: bool,
    },
    Markdown {
        markdown: String,
        selectable: bool,
    },
    Button {
        label: String,
        command: CommandId,
        payload: Option<DataValue>,
    },
    IconButton {
        icon: IconRef,
        label: String,
        command: CommandId,
    },
    Toggle {
        label: String,
        value: StateBinding,
        command: CommandId,
    },
    Select {
        label: String,
        value: StateBinding,
        options: Vec<SelectOption>,
        command: CommandId,
    },
    TextField {
        label: String,
        value: StateBinding,
        command: CommandId,
        maximum_bytes: u32,
    },
    List {
        children: Vec<UiNode>,
    },
    Tabs {
        tabs: Vec<UiTab>,
        selected: StateBinding,
        command: CommandId,
    },
    Badge {
        label: String,
        tone: UiTone,
    },
    Divider,
    Image {
        asset: PackagePath,
        alternative_text: String,
    },
    Progress {
        label: String,
        basis_points: u16,
    },
    Spacer,
}

impl UiNodeKind {
    #[must_use]
    pub fn children(&self) -> &[UiNode] {
        match self {
            Self::Column { children }
            | Self::Row { children }
            | Self::Stack { children }
            | Self::List { children } => children,
            _ => &[],
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct TextSpan {
    pub text: String,
    #[serde(default)]
    pub emphasis: TextEmphasis,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TextEmphasis {
    #[default]
    Normal,
    Muted,
    Strong,
    Code,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SelectOption {
    pub label: String,
    pub value: DataValue,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct UiTab {
    pub id: LocalId,
    pub label: String,
    pub content: UiNode,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UiTone {
    Neutral,
    Accent,
    Positive,
    Caution,
    Critical,
}
