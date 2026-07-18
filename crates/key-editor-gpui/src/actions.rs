//! GPUI actions handled by the reusable Markdown editor.

use gpui::actions;

actions!(
    markdown_editor,
    [
        MarkdownBackspace,
        MarkdownDelete,
        MarkdownLeft,
        MarkdownRight,
        MarkdownUp,
        MarkdownDown,
        MarkdownSelectLeft,
        MarkdownSelectRight,
        MarkdownSelectUp,
        MarkdownSelectDown,
        MarkdownSelectAll,
        MarkdownHome,
        MarkdownEnd,
        MarkdownCopy,
        MarkdownCut,
        MarkdownPaste,
        MarkdownToggleBold,
        MarkdownToggleItalic,
        MarkdownToggleCode,
        MarkdownToggleBulletedList,
        MarkdownToggleNumberedList,
        MarkdownNewline,
        MarkdownSave,
        MarkdownCancel,
    ]
);
