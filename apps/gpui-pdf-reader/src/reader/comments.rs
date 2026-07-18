/// View-independent copy and state derived for both comment panel layouts.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct CommentEditorPresentation {
    pub(super) title: &'static str,
    pub(super) save_status: &'static str,
}

impl CommentEditorPresentation {
    pub(super) fn new(editor_open: bool, editing: bool, saving: bool) -> Self {
        let title = if editor_open {
            if editing {
                "Edit Comment"
            } else {
                "New Comment"
            }
        } else {
            "Comments"
        };
        let save_status = if saving {
            "Saving…"
        } else if editor_open && editing {
            "Saved"
        } else {
            "Auto-save"
        };
        Self { title, save_status }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct CommentEmptyPresentation {
    pub(super) title: &'static str,
    pub(super) detail: &'static str,
}

impl CommentEmptyPresentation {
    pub(super) fn new(loading: bool, persistence_blocked: bool, fluid: bool) -> Self {
        let title = if loading {
            "Loading comments…"
        } else if persistence_blocked {
            "Comments unavailable"
        } else {
            "No comments yet"
        };
        let detail = if persistence_blocked {
            "Resolve the annotation sidecar problem before adding comments."
        } else if fluid {
            "Select text, then use either floating Note control."
        } else {
            "Select text in the document, then choose New Comment."
        };
        Self { title, detail }
    }
}

#[cfg(test)]
mod tests {
    use super::{CommentEditorPresentation, CommentEmptyPresentation};

    #[test]
    fn editor_copy_is_shared_without_losing_state() {
        assert_eq!(
            CommentEditorPresentation::new(false, false, false).title,
            "Comments"
        );
        assert_eq!(
            CommentEditorPresentation::new(true, true, false).save_status,
            "Saved"
        );
        assert_eq!(
            CommentEditorPresentation::new(true, true, true).save_status,
            "Saving…"
        );
    }

    #[test]
    fn empty_copy_keeps_layout_specific_guidance() {
        let classic = CommentEmptyPresentation::new(false, false, false);
        let fluid = CommentEmptyPresentation::new(false, false, true);
        assert_eq!(classic.title, fluid.title);
        assert_ne!(classic.detail, fluid.detail);
        assert_eq!(
            CommentEmptyPresentation::new(false, true, true).title,
            "Comments unavailable"
        );
    }
}
