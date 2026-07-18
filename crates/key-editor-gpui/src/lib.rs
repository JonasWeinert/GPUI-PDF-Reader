//! Reusable WYSIWYG Markdown editing for GPUI.
//!
//! The editor keeps rich text as plain UTF-8 plus semantic inline and block
//! styles. Markdown is only a deterministic storage format; Markdown markers
//! are never exposed to the person editing the document. All document state
//! lives in [`key_editor_core`]; this crate only adapts it to GPUI rendering,
//! native text input, pointer interaction, commands, and themed controls.

#![forbid(unsafe_code)]

pub use key_editor_core::{
    BlockKind, DEFAULT_MAX_MARKDOWN_BYTES, InlineStyle, MarkdownLimitExceeded, RichTextBuffer,
};
mod actions;
mod config;
mod editor;
mod projection;

pub use config::{
    MARKDOWN_EDITOR_KEY_CONTEXT, MarkdownEditorCommand, MarkdownEditorConfig, MarkdownEditorStyle,
    MarkdownEditorStylePolicy,
};

pub use actions::*;
pub use editor::{MarkdownEditor, MarkdownEditorConfigError, MarkdownEditorEvent};
