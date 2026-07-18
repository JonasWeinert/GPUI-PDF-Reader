use gpui_component::IconName;

/// Resolve a versioned host icon name to the trusted gpui-component asset.
/// Unknown names are not interpreted as paths and never load extension data.
#[must_use]
pub fn resolve_host_icon(name: &str) -> Option<IconName> {
    let normalized = name.trim().to_ascii_lowercase().replace('_', "-");
    Some(match normalized.as_str() {
        "a-large-small" => IconName::ALargeSmall,
        "arrow-down" => IconName::ArrowDown,
        "arrow-left" => IconName::ArrowLeft,
        "arrow-right" => IconName::ArrowRight,
        "arrow-up" => IconName::ArrowUp,
        "asterisk" => IconName::Asterisk,
        "bell" => IconName::Bell,
        "book-open" => IconName::BookOpen,
        "bot" => IconName::Bot,
        "building-2" => IconName::Building2,
        "calendar" => IconName::Calendar,
        "case-sensitive" => IconName::CaseSensitive,
        "chart-pie" => IconName::ChartPie,
        "check" => IconName::Check,
        "chevron-down" => IconName::ChevronDown,
        "chevron-left" => IconName::ChevronLeft,
        "chevron-right" => IconName::ChevronRight,
        "chevrons-up-down" => IconName::ChevronsUpDown,
        "chevron-up" => IconName::ChevronUp,
        "circle-check" => IconName::CircleCheck,
        "circle-user" => IconName::CircleUser,
        "circle-x" => IconName::CircleX,
        "close" | "x" => IconName::Close,
        "copy" => IconName::Copy,
        "dash" => IconName::Dash,
        "delete" | "trash" => IconName::Delete,
        "ellipsis" => IconName::Ellipsis,
        "ellipsis-vertical" => IconName::EllipsisVertical,
        "external-link" => IconName::ExternalLink,
        "eye" => IconName::Eye,
        "eye-off" => IconName::EyeOff,
        "file" => IconName::File,
        "folder" => IconName::Folder,
        "folder-closed" => IconName::FolderClosed,
        "folder-open" => IconName::FolderOpen,
        "frame" => IconName::Frame,
        "gallery-vertical-end" => IconName::GalleryVerticalEnd,
        "github" => IconName::GitHub,
        "globe" => IconName::Globe,
        "heart" => IconName::Heart,
        "heart-off" => IconName::HeartOff,
        "inbox" => IconName::Inbox,
        "info" => IconName::Info,
        "inspector" => IconName::Inspector,
        "layout-dashboard" => IconName::LayoutDashboard,
        "loader" => IconName::Loader,
        "loader-circle" => IconName::LoaderCircle,
        "map" => IconName::Map,
        "maximize" => IconName::Maximize,
        "menu" => IconName::Menu,
        "minimize" => IconName::Minimize,
        "minus" => IconName::Minus,
        "moon" => IconName::Moon,
        "palette" => IconName::Palette,
        "panel-bottom" => IconName::PanelBottom,
        "panel-bottom-open" => IconName::PanelBottomOpen,
        "panel-left" => IconName::PanelLeft,
        "panel-left-close" => IconName::PanelLeftClose,
        "panel-left-open" => IconName::PanelLeftOpen,
        "panel-right" => IconName::PanelRight,
        "panel-right-close" => IconName::PanelRightClose,
        "panel-right-open" => IconName::PanelRightOpen,
        "plus" => IconName::Plus,
        "redo" => IconName::Redo,
        "redo-2" => IconName::Redo2,
        "replace" => IconName::Replace,
        "resize-corner" => IconName::ResizeCorner,
        "search" => IconName::Search,
        "settings" => IconName::Settings,
        "settings-2" => IconName::Settings2,
        "sort-ascending" => IconName::SortAscending,
        "sort-descending" => IconName::SortDescending,
        "square-terminal" => IconName::SquareTerminal,
        "star" => IconName::Star,
        "star-off" => IconName::StarOff,
        "sun" => IconName::Sun,
        "thumbs-down" => IconName::ThumbsDown,
        "thumbs-up" => IconName::ThumbsUp,
        "triangle-alert" => IconName::TriangleAlert,
        "undo" => IconName::Undo,
        "undo-2" => IconName::Undo2,
        "user" => IconName::User,
        "window-close" => IconName::WindowClose,
        "window-maximize" => IconName::WindowMaximize,
        "window-minimize" => IconName::WindowMinimize,
        "window-restore" => IconName::WindowRestore,
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use gpui_component::IconNamed as _;

    use super::*;

    #[test]
    fn maps_names_and_safe_aliases_to_bundled_assets() {
        assert_eq!(
            resolve_host_icon("search").unwrap().path().as_ref(),
            "icons/search.svg"
        );
        assert_eq!(
            resolve_host_icon("Arrow_Right").unwrap().path().as_ref(),
            "icons/arrow-right.svg"
        );
        assert_eq!(
            resolve_host_icon("x").unwrap().path().as_ref(),
            "icons/close.svg"
        );
    }

    #[test]
    fn never_interprets_unknown_names_as_paths() {
        assert!(resolve_host_icon("../../secret.svg").is_none());
        assert!(resolve_host_icon("extension-owned-icon").is_none());
    }
}
