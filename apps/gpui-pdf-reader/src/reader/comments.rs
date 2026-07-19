use super::*;

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
    pub(super) fn new(loading: bool, persistence_blocked: bool) -> Self {
        let title = if loading {
            "Loading comments…"
        } else if persistence_blocked {
            "Comments unavailable"
        } else {
            "No comments yet"
        };
        let detail = if persistence_blocked {
            "Resolve the annotation sidecar problem before adding comments."
        } else {
            "Select text, then use either floating Note control."
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
    fn empty_copy_explains_the_floating_note_controls() {
        let empty = CommentEmptyPresentation::new(false, false);
        assert_eq!(empty.title, "No comments yet");
        assert!(empty.detail.contains("floating Note"));
        assert_eq!(
            CommentEmptyPresentation::new(false, true).title,
            "Comments unavailable"
        );
    }
}

pub(super) fn comment_draft_needs_confirmation(editor_open: bool, draft_dirty: bool) -> bool {
    editor_open && draft_dirty
}

pub(super) fn floating_pill_position(
    anchor: Rect,
    available_width: f32,
    viewport_height: f32,
    pill_width: f32,
    pill_height: f32,
) -> Offset {
    let margin = 12.0;
    let maximum_x = (available_width - pill_width - margin).max(margin);
    let x = (anchor.x + anchor.width * 0.5 - pill_width * 0.5).clamp(margin, maximum_x);
    let below = anchor.bottom() + 10.0;
    let y = if below + pill_height <= viewport_height - margin {
        below
    } else {
        (anchor.y - pill_height - 10.0).max(margin)
    };
    Offset { x, y }
}

impl PdfReader {
    pub(super) fn toggle_comments(
        &mut self,
        _: &ToggleComments,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.toggle_sidebar(SidePanel::Comments, window, cx);
    }

    pub(super) fn persist_annotations(&mut self) -> bool {
        if self.annotation_persistence_blocked {
            return false;
        }
        let (Some(document), Some(identity), Some(annotations)) = (
            self.document.as_ref(),
            self.annotation_identity.clone(),
            self.annotations.clone(),
        ) else {
            return false;
        };
        let revision = annotations.revision();
        if !self.annotation_io.save(
            self.generation,
            document.path.clone(),
            identity,
            self.annotation_saved_revision,
            annotations,
        ) {
            self.annotation_failed_revision = Some(revision);
            self.annotation_error = Some("The annotation sidecar worker is unavailable".into());
            false
        } else {
            self.annotation_enqueued_revision = self.annotation_enqueued_revision.max(revision);
            true
        }
    }

    pub(super) fn refresh_comment_order(&mut self) {
        self.comment_order = self
            .annotations
            .as_ref()
            .map(|annotations| {
                let mut comments = Vec::with_capacity(annotations.len());
                comments.extend(
                    annotations
                        .iter()
                        .filter(|annotation| annotation.comment_markdown().is_some())
                        .map(|annotation| annotation.id()),
                );
                comments
            })
            .unwrap_or_default();
    }

    pub(super) fn add_highlight(&mut self, color: HighlightColor, cx: &mut Context<Self>) {
        if self.annotations_loading {
            self.annotation_error = Some("Annotations are still loading".into());
            cx.notify();
            return;
        }
        if self.annotation_persistence_blocked {
            if self.annotation_error.is_none() {
                self.annotation_error =
                    Some("Annotations are disabled because the sidecar could not be loaded".into());
            }
            cx.notify();
            return;
        }
        let range = self.selection.map(TextRange::from_selection).or_else(|| {
            self.active_annotation.and_then(|id| {
                self.annotations
                    .as_ref()
                    .and_then(|annotations| annotations.get(id))
                    .map(|annotation| annotation.range())
            })
        });
        let Some(range) = range else {
            self.warning = Some("Select text before adding a highlight".into());
            cx.notify();
            return;
        };
        if range.end().index > MAX_TEXT_CHARACTER_INDEX {
            self.warning = Some(
                "Highlighting Select All is not supported yet; select a concrete text range".into(),
            );
            cx.notify();
            return;
        }
        let Some(annotations) = self.annotations.as_mut() else {
            return;
        };
        let existing = annotations
            .overlapping(range)
            .filter(|annotation| annotation.range() == range)
            .max_by_key(|annotation| (annotation.updated_revision(), annotation.id()))
            .map(|annotation| {
                (
                    annotation.id(),
                    annotation.comment_markdown().map(ToOwned::to_owned),
                )
            });
        let result = if let Some((id, comment)) = existing {
            annotations
                .update(id, range, Some(color), comment)
                .map(|changed| (id, changed))
        } else {
            annotations
                .add(range, Some(color), None)
                .map(|id| (id, true))
        };
        match result {
            Ok((id, changed)) => {
                if self.annotation_failed_revision.is_none() && !self.annotation_persistence_blocked
                {
                    self.annotation_error = None;
                }
                self.active_annotation = Some(id);
                self.selection = None;
                if changed
                    || self.annotations.as_ref().is_some_and(|annotations| {
                        annotations.revision() > self.annotation_saved_revision
                    })
                {
                    let _ = self.persist_annotations();
                }
                self.refresh_comment_order();
            }
            Err(error) => self.warning = Some(error.to_string().into()),
        }
        self.publish_pdf_selection();
        cx.notify();
    }

    pub(super) fn add_comment(
        &mut self,
        _: &AddComment,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.comment_editor.is_some() {
            self.warning =
                Some("Finish or cancel the current comment before starting another".into());
            self.show_sidebar(SidePanel::Comments, window, cx);
            cx.notify();
            return;
        }
        if self.annotations_loading {
            self.annotation_error = Some("Annotations are still loading".into());
            cx.notify();
            return;
        }
        if self.annotation_persistence_blocked {
            if self.annotation_error.is_none() {
                self.annotation_error =
                    Some("Comments are disabled because the sidecar could not be loaded".into());
            }
            cx.notify();
            return;
        }
        let Some(selection) = self.selection else {
            self.warning = Some("Select text before adding a comment".into());
            cx.notify();
            return;
        };
        let range = TextRange::from_selection(selection);
        if range.end().index > MAX_TEXT_CHARACTER_INDEX {
            self.warning = Some(
                "Commenting on Select All is not supported yet; select a concrete text range"
                    .into(),
            );
            cx.notify();
            return;
        }
        let exact = self.annotations.as_ref().and_then(|annotations| {
            annotations
                .overlapping(range)
                .filter(|annotation| annotation.range() == range)
                .max_by_key(|annotation| (annotation.updated_revision(), annotation.id()))
                .map(|annotation| {
                    (
                        annotation.id(),
                        annotation.comment_markdown().unwrap_or("").to_owned(),
                    )
                })
        });
        let (editing, markdown) = exact
            .map(|(id, markdown)| (Some(id), markdown))
            .unwrap_or((None, String::new()));
        self.open_comment_editor(range, editing, markdown, window, cx);
    }

    pub(super) fn comment_on_context(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.selection.is_some() {
            self.add_comment(&AddComment, window, cx);
            return;
        }
        let Some(id) = self.active_annotation else {
            self.warning = Some("Select text before adding a comment".into());
            cx.notify();
            return;
        };
        let Some((range, markdown)) = self
            .annotations
            .as_ref()
            .and_then(|annotations| annotations.get(id))
            .map(|annotation| {
                (
                    annotation.range(),
                    annotation.comment_markdown().unwrap_or("").to_owned(),
                )
            })
        else {
            return;
        };
        self.open_comment_editor(range, Some(id), markdown, window, cx);
    }

    pub(super) fn context_range(&self) -> Option<TextRange> {
        self.selection
            .filter(|selection| selection.anchor != selection.focus)
            .map(TextRange::from_selection)
            .or_else(|| {
                self.active_annotation.and_then(|id| {
                    self.annotations
                        .as_ref()
                        .and_then(|annotations| annotations.get(id))
                        .map(|annotation| annotation.range())
                })
            })
    }

    pub(super) fn context_has_comment(&self) -> bool {
        self.active_annotation.is_some_and(|id| {
            self.annotations
                .as_ref()
                .and_then(|annotations| annotations.get(id))
                .is_some_and(|annotation| annotation.comment_markdown().is_some())
        }) || self.selection.is_some_and(|selection| {
            if selection.anchor == selection.focus {
                return false;
            }
            let range = TextRange::from_selection(selection);
            self.annotations.as_ref().is_some_and(|annotations| {
                annotations.overlapping(range).any(|annotation| {
                    annotation.range() == range && annotation.comment_markdown().is_some()
                })
            })
        })
    }

    pub(super) fn context_anchor_in_viewport(&self) -> Option<Rect> {
        let range = self.context_range()?;
        let layout = self.layout()?;
        let content_viewport = Rect {
            x: self.scroll.x,
            y: self.scroll.y,
            width: self.viewport_width,
            height: self.viewport_height,
        };
        let visible_pages: Vec<_> = layout
            .visible_pages(self.scroll.y, self.viewport_height, 0.0)
            .collect();
        for page in visible_pages.into_iter().rev() {
            if page < range.start().page || page > range.end().page {
                continue;
            }
            let Some(chars) = self.page_text.get(&page) else {
                continue;
            };
            let Some(page_rect) = layout.page_rect(page) else {
                continue;
            };
            let Some(indices) = range.indices_on_page(page, chars.len()) else {
                continue;
            };
            let mut bottom_line: Option<Rect> = None;
            let mut visited = 0usize;
            chars.for_each_visible_in_range_while(
                page_rect,
                content_viewport,
                indices,
                |_, rect| {
                    visited += 1;
                    if visited > MAX_VISIBLE_SELECTION_QUADS {
                        return false;
                    }
                    bottom_line = Some(match bottom_line {
                        None => rect,
                        Some(current) => {
                            let tolerance = current.height.max(rect.height) * 0.5;
                            if rect.bottom() > current.bottom() + tolerance {
                                rect
                            } else if (rect.bottom() - current.bottom()).abs() <= tolerance {
                                let left = current.x.min(rect.x);
                                let top = current.y.min(rect.y);
                                Rect {
                                    x: left,
                                    y: top,
                                    width: current.right().max(rect.right()) - left,
                                    height: current.bottom().max(rect.bottom()) - top,
                                }
                            } else {
                                current
                            }
                        }
                    });
                    true
                },
            );
            if let Some(line) = bottom_line {
                return Some(Rect {
                    x: line.x - self.scroll.x,
                    y: line.y - self.scroll.y,
                    width: line.width,
                    height: line.height,
                });
            }
        }
        None
    }

    pub(super) fn open_comment_editor(
        &mut self,
        range: TextRange,
        editing: Option<AnnotationId>,
        markdown: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let buffer = match RichTextBuffer::try_from_markdown(&markdown) {
            Ok(buffer) => buffer,
            Err(error) => {
                self.annotation_error = Some(
                    format!("Unable to open comment: {error}. The stored comment was not changed.")
                        .into(),
                );
                self.show_sidebar(SidePanel::Comments, window, cx);
                cx.notify();
                return;
            }
        };
        if self.annotation_failed_revision.is_none() && !self.annotation_persistence_blocked {
            self.annotation_error = None;
        }
        let editor_limit = buffer.max_markdown_bytes();
        let editor = cx.new(move |cx| {
            MarkdownEditor::new(
                cx,
                buffer,
                MarkdownEditorConfig {
                    placeholder: "Write a comment…".into(),
                    max_markdown_bytes: editor_limit,
                    ..MarkdownEditorConfig::default()
                },
            )
            .expect("comment editor buffer and configured limits must match")
        });
        cx.subscribe_in(
            &editor,
            window,
            |reader, _, event, window, cx| match event {
                MarkdownEditorEvent::Changed => reader.comment_editor_changed(window, cx),
                MarkdownEditorEvent::Save(markdown) => {
                    reader.cancel_comment_autosave();
                    let _ = reader.write_comment(markdown.clone(), cx);
                }
                MarkdownEditorEvent::Cancel => reader.cancel_comment_editor(window, cx),
            },
        )
        .detach();
        self.pending_comment_range = Some(range);
        self.editing_annotation = editing;
        self.comment_editor = Some(editor.clone());
        self.comment_draft_dirty = false;
        self.comment_pane.show_editor(true);
        self.show_sidebar(SidePanel::Comments, window, cx);
        self.start_animation(window, cx);
        window.focus(&editor.focus_handle(cx));
        cx.notify();
    }

    pub(super) fn edit_comment(
        &mut self,
        id: AnnotationId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.comment_editor.is_some() {
            return;
        }
        let Some((range, markdown)) = self
            .annotations
            .as_ref()
            .and_then(|annotations| annotations.get(id))
            .and_then(|annotation| {
                annotation
                    .comment_markdown()
                    .map(|markdown| (annotation.range(), markdown.to_owned()))
            })
        else {
            return;
        };
        self.open_comment_editor(range, Some(id), markdown, window, cx);
    }

    pub(super) fn write_comment(&mut self, markdown: String, cx: &mut Context<Self>) -> bool {
        if markdown.trim().is_empty() {
            self.annotation_error = Some("A comment cannot be empty".into());
            cx.notify();
            return false;
        }
        let Some(range) = self.pending_comment_range else {
            return false;
        };
        if self.annotation_failed_revision.is_none() && !self.annotation_persistence_blocked {
            self.annotation_error = None;
        }
        let Some(annotations) = self.annotations.as_mut() else {
            return false;
        };
        let result = if let Some(id) = self.editing_annotation {
            let highlight = annotations
                .get(id)
                .and_then(|annotation| annotation.highlight());
            annotations
                .update(id, range, highlight, Some(markdown))
                .map(|changed| (id, changed))
        } else {
            annotations
                .add(range, None, Some(markdown))
                .map(|id| (id, true))
        };
        match result {
            Ok((id, changed)) => {
                self.active_annotation = Some(id);
                self.selection = None;
                if changed
                    || self.annotations.as_ref().is_some_and(|annotations| {
                        annotations.revision() > self.annotation_saved_revision
                    })
                {
                    let _ = self.persist_annotations();
                }
                self.refresh_comment_order();
                self.comment_draft_dirty = false;
                self.editing_annotation = Some(id);
                self.publish_pdf_selection();
                cx.notify();
                true
            }
            Err(error) => {
                self.annotation_error = Some(error.to_string().into());
                cx.notify();
                false
            }
        }
    }

    pub(super) fn comment_editor_changed(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.comment_draft_dirty = true;
        self.schedule_comment_autosave(window, cx);
        cx.notify();
    }

    pub(super) fn schedule_comment_autosave(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.comment_autosave_revision = self.comment_autosave_revision.wrapping_add(1);
        let revision = self.comment_autosave_revision;
        let weak = cx.weak_entity();
        self.comment_autosave_task = Some(window.spawn(cx, async move |cx| {
            cx.background_executor()
                .timer(COMMENT_AUTOSAVE_DEBOUNCE)
                .await;
            let _ = cx.update(|_, cx| {
                weak.update(cx, |reader, cx| {
                    if reader.comment_autosave_revision == revision {
                        reader.comment_autosave_task = None;
                        let _ = reader.flush_comment_autosave(cx);
                    }
                })
                .ok();
            });
        }));
    }

    pub(super) fn cancel_comment_autosave(&mut self) {
        self.comment_autosave_revision = self.comment_autosave_revision.wrapping_add(1);
        self.comment_autosave_task = None;
    }

    pub(super) fn flush_comment_autosave(&mut self, cx: &mut Context<Self>) -> bool {
        self.cancel_comment_autosave();
        if !self.comment_draft_dirty {
            return true;
        }
        let Some(editor) = self.comment_editor.as_ref() else {
            return true;
        };
        let markdown = editor.read(cx).markdown();
        if markdown.trim().is_empty() {
            return false;
        }
        self.write_comment(markdown, cx)
    }

    pub(super) fn return_to_comment_list(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let markdown_is_blank = self
            .comment_editor
            .as_ref()
            .is_none_or(|editor| editor.read(cx).is_blank());
        if self.comment_draft_dirty && !markdown_is_blank && !self.flush_comment_autosave(cx) {
            return;
        }
        self.cancel_comment_autosave();
        self.comment_draft_dirty = false;
        window.focus(&self.focus_handle);
        self.comment_pane.show_list(true);
        // If Back is activated before the editor's entrance animation has
        // painted its first frame, both the current and target progress are
        // already zero. There is then no animation tick to perform the
        // deferred unmount, so finish it synchronously.
        if !self.comment_pane.is_animating() {
            self.finish_comment_editor_close();
        }
        self.start_animation(window, cx);
        cx.notify();
    }

    pub(super) fn cancel_comment_editor(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.return_to_comment_list(window, cx);
    }

    pub(super) fn discard_comment_editor(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.cancel_comment_autosave();
        self.comment_pane.show_list(false);
        self.finish_comment_editor_close();
        window.focus(&self.focus_handle);
        cx.notify();
    }

    pub(super) fn finish_comment_editor_close(&mut self) {
        self.comment_editor = None;
        self.comment_draft_dirty = false;
        self.editing_annotation = None;
        self.pending_comment_range = None;
        self.comment_pane.close_editor_on_finish = false;
    }

    pub(super) fn comment_draft_needs_confirmation(&self) -> bool {
        comment_draft_needs_confirmation(self.comment_editor.is_some(), self.comment_draft_dirty)
    }

    pub(super) fn confirm_discard_comment(
        &mut self,
        action: DraftDiscardAction,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.comment_discard_prompt_open {
            return;
        }

        let (message, detail, discard_label) = match &action {
            DraftDiscardAction::Open(_) => (
                "Discard this comment draft?",
                "Opening another PDF will permanently discard the unsaved comment.",
                "Discard and Open",
            ),
            DraftDiscardAction::Quit => (
                "Quit with an unsaved comment?",
                "The comment draft will be permanently discarded.",
                "Discard and Quit",
            ),
            DraftDiscardAction::CloseWindow => (
                "Close with an unsaved comment?",
                "The comment draft will be permanently discarded.",
                "Discard and Close",
            ),
        };
        self.comment_discard_prompt_open = true;
        let answer = window.prompt(
            PromptLevel::Warning,
            message,
            Some(detail),
            &[
                PromptButton::cancel("Keep Editing"),
                PromptButton::ok(discard_label),
            ],
            cx,
        );
        let weak = cx.weak_entity();
        window
            .spawn(cx, async move |cx| {
                let discard = answer.await.ok() == Some(1);
                let _ = cx.update(|window, cx| {
                    weak.update(cx, |reader, cx| {
                        reader.comment_discard_prompt_open = false;
                        if discard {
                            reader.discard_comment_editor(window, cx);
                            match action {
                                DraftDiscardAction::Open(path) => {
                                    reader.open_path_after_comment_guard(path, window, cx)
                                }
                                DraftDiscardAction::Quit => {
                                    reader.close_pdf_capability_generation();
                                    cx.quit();
                                }
                                DraftDiscardAction::CloseWindow => {
                                    reader.close_pdf_capability_generation();
                                    window.remove_window();
                                }
                            }
                        } else {
                            reader.show_sidebar(SidePanel::Comments, window, cx);
                            if let Some(editor) = reader.comment_editor.as_ref() {
                                window.focus(&editor.focus_handle(cx));
                            }
                            cx.notify();
                        }
                    })
                    .ok();
                });
            })
            .detach();
    }

    pub(super) fn navigate_annotation(
        &mut self,
        id: AnnotationId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(range) = self
            .annotations
            .as_ref()
            .and_then(|annotations| annotations.get(id))
            .map(|annotation| annotation.range())
        else {
            return;
        };
        let start = range.start();
        let Some(page_rect) = self
            .layout()
            .and_then(|layout| layout.page_rect(start.page))
        else {
            return;
        };
        let target = self
            .page_text
            .get(&start.page)
            .and_then(|text| text.get(start.index))
            .and_then(|character| character.bounds)
            .map(|bounds| {
                (
                    page_rect.x + (bounds.left + bounds.right) * 0.5 * page_rect.width,
                    page_rect.y + (bounds.top + bounds.bottom) * 0.5 * page_rect.height,
                )
            })
            .unwrap_or((
                page_rect.x + page_rect.width * 0.5,
                page_rect.y + page_rect.height * 0.15,
            ));
        self.active_annotation = Some(id);
        self.sidebar_anchor = None;
        self.viewport.scroll_to(
            Offset {
                x: target.0 - self.viewport_width * 0.5,
                y: target.1 - self.viewport_height * 0.35,
            },
            ViewportScrollBehavior::Smooth,
        );
        self.sync_viewport_snapshot();
        self.start_animation(window, cx);
        cx.notify();
    }

    pub(super) fn open_comment_from_list(
        &mut self,
        id: AnnotationId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.navigate_annotation(id, window, cx);
        self.edit_comment(id, window, cx);
    }

    pub(super) fn render_comment_list(
        &mut self,
        palette: ReaderPalette,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        uniform_list(
            "comment-list",
            self.comment_order.len(),
            cx.processor(move |reader, range: std::ops::Range<usize>, _window, cx| {
                range
                    .filter_map(|index| {
                        let id = *reader.comment_order.get(index)?;
                        let (page, preview, color, active) =
                            reader.comment_row_presentation(id, palette)?;
                        let action = div()
                            .text_color(if active {
                                palette.accent
                            } else {
                                palette.text_secondary
                            })
                            .child(Self::icon_label(IconName::ArrowRight, "Open"));
                        Some(
                            div().h(px(96.0)).w_full().px_3().py_1().child(
                                div()
                                    .id(("comment", index))
                                    .size_full()
                                    .overflow_hidden()
                                    .flex()
                                    .rounded_md()
                                    .border_1()
                                    .border_color(if active {
                                        palette.accent_border
                                    } else {
                                        palette.separator
                                    })
                                    .bg(if active {
                                        palette.accent_soft
                                    } else {
                                        palette.surface
                                    })
                                    .cursor_pointer()
                                    .hover(move |row| {
                                        row.bg(if active {
                                            palette.accent_soft_hover
                                        } else {
                                            palette.surface_subtle
                                        })
                                    })
                                    .on_click(cx.listener(move |reader, _, window, cx| {
                                        reader.open_comment_from_list(id, window, cx);
                                    }))
                                    .child(div().w(px(4.0)).h_full().flex_none().bg(color))
                                    .child(
                                        div()
                                            .flex_1()
                                            .min_w(px(0.0))
                                            .px_3()
                                            .py_2()
                                            .flex()
                                            .flex_col()
                                            .gap_1()
                                            .child(
                                                div()
                                                    .flex()
                                                    .items_center()
                                                    .justify_between()
                                                    .text_xs()
                                                    .font_weight(FontWeight::MEDIUM)
                                                    .child(
                                                        div()
                                                            .text_color(if active {
                                                                palette.accent
                                                            } else {
                                                                palette.text_secondary
                                                            })
                                                            .child(format!("PAGE {}", page + 1)),
                                                    )
                                                    .child(action),
                                            )
                                            .child(
                                                div()
                                                    .h(px(42.0))
                                                    .overflow_hidden()
                                                    .text_sm()
                                                    .line_height(px(20.0))
                                                    .text_color(palette.text)
                                                    .child(preview),
                                            ),
                                    ),
                            ),
                        )
                    })
                    .collect::<Vec<_>>()
            }),
        )
        .track_scroll(self.comment_list_scroll.clone())
        .flex_1()
        .min_h(px(0.0))
        .w_full()
        .bg(palette.surface_subtle)
        .rounded_b_xl()
        .into_any_element()
    }

    pub(super) fn render_comments_panel(&mut self, cx: &mut Context<Self>) -> gpui::AnyElement {
        let palette = ReaderPalette::from_theme(Theme::global(cx));
        let list_header = div()
            .h(px(54.0))
            .flex_none()
            .px_4()
            .flex()
            .items_center()
            .justify_between()
            .border_b_1()
            .border_color(palette.separator)
            .child(
                div()
                    .text_lg()
                    .font_weight(FontWeight::SEMIBOLD)
                    .child("Comments"),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_1()
                    .child(Self::chrome_button(
                        palette,
                        "fluid-close-comments",
                        Icon::new(IconName::Close).size(px(16.0)),
                        ChromeButtonStyle::Ghost,
                        true,
                        cx.listener(|reader, _, window, cx| {
                            reader.toggle_sidebar(SidePanel::Comments, window, cx)
                        }),
                    )),
            );

        let list_body = if self.comment_order.is_empty() {
            let empty = CommentEmptyPresentation::new(
                self.annotations_loading,
                self.annotation_persistence_blocked,
            );
            empty_state(palette, IconName::BookOpen, empty.title, empty.detail)
        } else {
            self.render_comment_list(palette, cx)
        };

        let list_error = self
            .annotation_error
            .clone()
            .map(|message| error_banner(palette, message));
        let progress = self.comment_pane.progress;
        let list_pane = div()
            .absolute()
            .top_0()
            .bottom_0()
            .left(px(-SIDEBAR_WIDTH * progress))
            .w_full()
            .flex()
            .flex_col()
            .rounded_xl()
            .bg(palette.surface)
            .child(list_header)
            .children(list_error)
            .child(list_body);

        let editor_pane = self.comment_editor.clone().map(|editor| {
            let annotations_pending = self
                .annotations
                .as_ref()
                .is_some_and(|annotations| annotations.revision() > self.annotation_saved_revision);
            let presentation = CommentEditorPresentation::new(
                true,
                self.editing_annotation.is_some(),
                self.comment_draft_dirty
                    || self.comment_autosave_task.is_some()
                    || annotations_pending,
            );
            let title = presentation.title;
            let save_status = presentation.save_status;
            let editor_error = self
                .annotation_error
                .clone()
                .map(|message| error_banner(palette, message));
            div()
                .absolute()
                .top_0()
                .bottom_0()
                .left(px(SIDEBAR_WIDTH * (1.0 - progress)))
                .w_full()
                .flex()
                .flex_col()
                .rounded_xl()
                .bg(palette.surface)
                .child(
                    div()
                        .h(px(54.0))
                        .flex_none()
                        .px_3()
                        .flex()
                        .items_center()
                        .justify_between()
                        .border_b_1()
                        .border_color(palette.separator)
                        .child(Self::chrome_button(
                            palette,
                            "fluid-comment-back",
                            Self::icon_label(IconName::ChevronLeft, "Overview"),
                            ChromeButtonStyle::Ghost,
                            true,
                            cx.listener(|reader, _, window, cx| {
                                reader.return_to_comment_list(window, cx)
                            }),
                        ))
                        .child(
                            div()
                                .flex_1()
                                .min_w(px(0.0))
                                .px_2()
                                .flex()
                                .flex_col()
                                .items_center()
                                .child(
                                    div()
                                        .max_w(px(150.0))
                                        .overflow_hidden()
                                        .whitespace_nowrap()
                                        .text_ellipsis()
                                        .text_sm()
                                        .font_weight(FontWeight::SEMIBOLD)
                                        .child(title),
                                )
                                .child(
                                    div()
                                        .text_xs()
                                        .text_color(if save_status == "Saved" {
                                            palette.green
                                        } else {
                                            palette.text_secondary
                                        })
                                        .child(save_status),
                                ),
                        )
                        .child(Self::chrome_button(
                            palette,
                            "fluid-close-comment-editor",
                            Icon::new(IconName::Close).size(px(16.0)),
                            ChromeButtonStyle::Ghost,
                            true,
                            cx.listener(|reader, _, window, cx| {
                                reader.toggle_sidebar(SidePanel::Comments, window, cx)
                            }),
                        )),
                )
                .children(editor_error)
                .child(
                    div()
                        .flex_1()
                        .min_h(px(0.0))
                        .p_4()
                        .flex()
                        .flex_col()
                        .child(editor),
                )
        });

        div()
            .relative()
            .size_full()
            .overflow_hidden()
            .rounded_xl()
            .bg(palette.surface)
            .text_color(palette.text)
            .child(list_pane)
            .children(editor_pane)
            .into_any_element()
    }

    pub(super) fn comment_row_presentation(
        &self,
        id: AnnotationId,
        palette: ReaderPalette,
    ) -> Option<(usize, SharedString, Hsla, bool)> {
        let annotation = self.annotations.as_ref()?.get(id)?;
        let page = annotation.range().start().page;
        let markdown = annotation.comment_markdown().unwrap_or("");
        let preview = RichTextBuffer::try_from_markdown(markdown)
            .map(|buffer| compact_preview(buffer.text(), 96))
            .unwrap_or_else(|_| compact_preview(markdown, 96));
        let color = match annotation.highlight() {
            Some(HighlightColor::Yellow) => palette.yellow,
            Some(HighlightColor::Green) => palette.green,
            Some(HighlightColor::Blue) => palette.blue,
            Some(HighlightColor::Pink) => palette.pink,
            Some(HighlightColor::Purple) => palette.purple,
            None => palette.warning,
        };
        Some((
            page,
            preview.into(),
            color,
            self.active_annotation == Some(id),
        ))
    }
}
