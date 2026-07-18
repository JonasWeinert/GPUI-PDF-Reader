//! Compatibility imports for the standalone reader's comment workflow.
//!
//! The reusable implementation lives in `key-editor-gpui`; keeping the old
//! action names here avoids coupling app menus and tests to the component's
//! internal migration history.

pub use key_editor_gpui::{
    MarkdownBackspace as CommentBackspace, MarkdownCancel as CommentCancel,
    MarkdownDelete as CommentDelete, MarkdownDown as CommentDown, MarkdownEditor,
    MarkdownEditorConfig, MarkdownEditorEvent, MarkdownEnd as CommentEnd,
    MarkdownHome as CommentHome, MarkdownLeft as CommentLeft, MarkdownNewline as CommentNewline,
    MarkdownRight as CommentRight, MarkdownSave as CommentSave,
    MarkdownSelectDown as CommentSelectDown, MarkdownSelectLeft as CommentSelectLeft,
    MarkdownSelectRight as CommentSelectRight, MarkdownSelectUp as CommentSelectUp,
    MarkdownToggleBold as CommentToggleBold,
    MarkdownToggleBulletedList as CommentToggleBulletedList,
    MarkdownToggleCode as CommentToggleCode, MarkdownToggleItalic as CommentToggleItalic,
    MarkdownToggleNumberedList as CommentToggleNumberedList, MarkdownUp as CommentUp,
    RichTextBuffer,
};

#[allow(unused_imports)]
pub use key_editor_gpui::MarkdownLimitExceeded as CommentMarkdownTooLong;
