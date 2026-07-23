use key_workspace_core::{ControlBarItem, ControlBarItemKind, ControlBarWidth};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ControlBarDisplayMode {
    Full,
    Compact,
    Icon,
}

impl ControlBarDisplayMode {
    const fn index(self) -> usize {
        match self {
            Self::Full => 0,
            Self::Compact => 1,
            Self::Icon => 2,
        }
    }

    const fn next(self) -> Self {
        match self {
            Self::Full => Self::Compact,
            Self::Compact | Self::Icon => Self::Icon,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct ControlBarLayoutItem {
    pub mode: ControlBarDisplayMode,
    pub width: f32,
}

/// Selects the richest presentation that fits. Expanded inputs retain their
/// richest width while other controls collapse first, then step down through
/// their own compact widths only when the bar would otherwise overflow.
#[must_use]
pub fn solve_control_bar_layout(
    items: &[ControlBarItem],
    available_width: f32,
    inter_item_gap: f32,
) -> Vec<ControlBarLayoutItem> {
    let visible = items.iter().filter(|item| item.state.visible).count();
    let gaps = visible.saturating_sub(1) as f32 * inter_item_gap.max(0.0);
    let budget = (available_width.max(0.0) - gaps).max(0.0);
    let mut modes = items
        .iter()
        .map(|item| {
            if item.state.visible {
                ControlBarDisplayMode::Full
            } else {
                ControlBarDisplayMode::Icon
            }
        })
        .collect::<Vec<_>>();
    let mut total = measured_width(items, &modes);
    let mut candidates = items
        .iter()
        .enumerate()
        .filter(|(_, item)| item.state.visible)
        .map(|(index, item)| (item.presentation.priority, index))
        .collect::<Vec<_>>();
    candidates.sort_by_key(|candidate| *candidate);

    while total > budget {
        let mut changed = false;
        for &(_, index) in &candidates {
            let item = &items[index];
            if item.kind == ControlBarItemKind::TextInput && item.state.expanded {
                continue;
            }
            let current = modes[index];
            let next = current.next();
            if next == current {
                continue;
            }
            total -= width(item, current) - width(item, next);
            modes[index] = next;
            changed = true;
            if total <= budget {
                break;
            }
        }
        if !changed {
            break;
        }
    }

    // An expanded field gets priority, not an unlimited width guarantee. At a
    // narrow window it must still contract after the rest of the chrome has
    // exhausted its compact presentations.
    while total > budget {
        let mut changed = false;
        for &(_, index) in &candidates {
            let item = &items[index];
            if item.kind != ControlBarItemKind::TextInput || !item.state.expanded {
                continue;
            }
            let current = modes[index];
            let next = current.next();
            if next == current {
                continue;
            }
            total -= width(item, current) - width(item, next);
            modes[index] = next;
            changed = true;
            if total <= budget {
                break;
            }
        }
        if !changed {
            break;
        }
    }

    let remaining = (budget - measured_width(items, &modes)).max(0.0);
    let max_items = items
        .iter()
        .filter(|item| item.state.visible && item.presentation.width == ControlBarWidth::Max)
        .count();
    let max_width = if max_items == 0 {
        0.0
    } else {
        remaining / max_items as f32
    };

    items
        .iter()
        .zip(modes)
        .map(|(item, mode)| ControlBarLayoutItem {
            mode,
            width: if item.state.visible {
                if item.presentation.width == ControlBarWidth::Max {
                    max_width
                } else {
                    width(item, mode)
                }
            } else {
                0.0
            },
        })
        .collect()
}

fn measured_width(items: &[ControlBarItem], modes: &[ControlBarDisplayMode]) -> f32 {
    items
        .iter()
        .zip(modes)
        .filter(|(item, _)| item.state.visible)
        .map(|(item, mode)| width(item, *mode))
        .sum()
}

fn width(item: &ControlBarItem, mode: ControlBarDisplayMode) -> f32 {
    if item.presentation.width == ControlBarWidth::Max {
        return 0.0;
    }
    let value = item.presentation.widths[mode.index()];
    if value.is_finite() {
        value.max(0.0)
    } else {
        0.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use key_workspace_core::{ControlBarItemState, ControlBarPresentation, ControlBarRegion};

    fn item(id: &str, priority: u16) -> ControlBarItem {
        ControlBarItem::new(
            id,
            ControlBarRegion::Trailing,
            ControlBarItemKind::Button,
            ControlBarPresentation::new(id, [100.0, 64.0, 32.0], priority),
        )
    }

    #[test]
    fn lower_priority_items_collapse_first() {
        let items = [item("low", 10), item("high", 90)];
        let layout = solve_control_bar_layout(&items, 168.0, 4.0);
        assert_eq!(layout[0].mode, ControlBarDisplayMode::Compact);
        assert_eq!(layout[1].mode, ControlBarDisplayMode::Full);
    }

    #[test]
    fn an_expanded_input_keeps_its_requested_width() {
        let mut input = ControlBarItem::new(
            "search",
            ControlBarRegion::Trailing,
            ControlBarItemKind::TextInput,
            ControlBarPresentation::new("Search", [280.0, 180.0, 32.0], 1),
        );
        input.state = ControlBarItemState {
            expanded: true,
            ..ControlBarItemState::default()
        };
        let items = [input, item("comments", 80), item("title", 2)];
        let layout = solve_control_bar_layout(&items, 360.0, 4.0);
        assert_eq!(layout[0].mode, ControlBarDisplayMode::Full);
        assert_eq!(layout[0].width, 280.0);
        assert_eq!(layout[1].mode, ControlBarDisplayMode::Icon);
    }

    #[test]
    fn an_expanded_input_compacts_only_after_other_controls() {
        let mut input = ControlBarItem::new(
            "search",
            ControlBarRegion::Trailing,
            ControlBarItemKind::TextInput,
            ControlBarPresentation::new("Search", [280.0, 180.0, 96.0], 1),
        );
        input.state = ControlBarItemState {
            expanded: true,
            ..ControlBarItemState::default()
        };
        let layout = solve_control_bar_layout(&[input, item("comments", 80)], 220.0, 4.0);
        assert_eq!(layout[0].mode, ControlBarDisplayMode::Compact);
        assert_eq!(layout[1].mode, ControlBarDisplayMode::Icon);
    }

    #[test]
    fn hidden_items_consume_no_width() {
        let mut hidden = item("hidden", 1);
        hidden.state.visible = false;
        let layout = solve_control_bar_layout(&[hidden], 0.0, 4.0);
        assert_eq!(layout[0].width, 0.0);
    }

    #[test]
    fn max_width_item_receives_the_space_left_by_intrinsic_controls() {
        let mut location = item("location", 1);
        location.presentation.width = ControlBarWidth::Max;
        let layout = solve_control_bar_layout(
            &[item("split", 90), location, item("search", 90)],
            300.0,
            4.0,
        );
        assert_eq!(layout[0].width, 100.0);
        assert_eq!(layout[2].width, 100.0);
        assert_eq!(layout[1].width, 92.0);
    }
}
