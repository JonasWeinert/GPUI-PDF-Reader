//! Host configuration and semantic styling for the GPUI editor.

use gpui::{App, Hsla, SharedString};
use gpui_component::{IconName, Theme};
use key_editor_core::{DEFAULT_MAX_MARKDOWN_BYTES, MarkdownLimitExceeded, RichTextBuffer};
use std::{fmt, rc::Rc};

/// Key context used by the editor and its default key bindings.
pub const MARKDOWN_EDITOR_KEY_CONTEXT: &str = "MarkdownEditor";

const DEFAULT_LIMIT_MESSAGE: &str = "Markdown exceeds the configured storage limit.";

/// Semantic colors used by the editor. Keeping the component dependent on a
/// semantic palette instead of application colors lets hosts theme it without
/// teaching the editor about their visual system.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MarkdownEditorStyle {
    pub surface: Hsla,
    pub surface_subtle: Hsla,
    pub control_hover: Hsla,
    pub control_pressed: Hsla,
    pub separator: Hsla,
    pub text: Hsla,
    pub text_secondary: Hsla,
    pub text_tertiary: Hsla,
    pub accent: Hsla,
    pub accent_soft: Hsla,
    pub error: Hsla,
    pub selection: Hsla,
}

impl MarkdownEditorStyle {
    /// Resolves the editor palette from gpui-component's active theme.
    pub fn from_theme(theme: &Theme) -> Self {
        Self {
            surface: theme.background,
            surface_subtle: theme.muted,
            control_hover: theme.secondary_hover,
            control_pressed: theme.secondary_active,
            separator: theme.border,
            text: theme.foreground,
            text_secondary: theme.secondary_foreground,
            text_tertiary: theme.muted_foreground,
            accent: theme.primary,
            accent_soft: theme.accent,
            error: theme.danger,
            selection: theme.selection,
        }
    }
}

/// Determines how semantic editor colors are resolved for each render.
#[derive(Clone)]
pub enum MarkdownEditorStylePolicy {
    /// Follow gpui-component's active theme, including live theme changes.
    ComponentTheme,
    /// Use a host-supplied static semantic palette.
    Fixed(MarkdownEditorStyle),
    /// Resolve a semantic palette from arbitrary host state.
    Custom(Rc<dyn Fn(&App) -> MarkdownEditorStyle>),
}

impl MarkdownEditorStylePolicy {
    pub(crate) fn resolve(&self, cx: &App) -> MarkdownEditorStyle {
        match self {
            Self::ComponentTheme => MarkdownEditorStyle::from_theme(Theme::global(cx)),
            Self::Fixed(style) => *style,
            Self::Custom(resolve) => resolve(cx),
        }
    }
}

impl fmt::Debug for MarkdownEditorStylePolicy {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ComponentTheme => formatter.write_str("ComponentTheme"),
            Self::Fixed(style) => formatter.debug_tuple("Fixed").field(style).finish(),
            Self::Custom(_) => formatter.write_str("Custom(<resolver>)"),
        }
    }
}

/// Formatting operation available in the toolbar and/or slash menu.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum MarkdownEditorCommand {
    Paragraph,
    Heading1,
    Heading2,
    Heading3,
    BulletedList,
    NumberedList,
    Quote,
    Bold,
    Italic,
    InlineCode,
}

impl MarkdownEditorCommand {
    pub const ALL: [Self; 10] = [
        Self::Paragraph,
        Self::Heading1,
        Self::Heading2,
        Self::Heading3,
        Self::BulletedList,
        Self::NumberedList,
        Self::Quote,
        Self::Bold,
        Self::Italic,
        Self::InlineCode,
    ];

    pub const DEFAULT_TOOLBAR: [Self; 5] = [
        Self::Bold,
        Self::Italic,
        Self::InlineCode,
        Self::BulletedList,
        Self::NumberedList,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Self::Paragraph => "Text",
            Self::Heading1 => "Heading 1",
            Self::Heading2 => "Heading 2",
            Self::Heading3 => "Heading 3",
            Self::BulletedList => "Bulleted list",
            Self::NumberedList => "Numbered list",
            Self::Quote => "Quote",
            Self::Bold => "Bold",
            Self::Italic => "Italic",
            Self::InlineCode => "Inline code",
        }
    }

    pub fn detail(self) -> &'static str {
        match self {
            Self::Paragraph => "Plain paragraph",
            Self::Heading1 => "Large section heading",
            Self::Heading2 => "Medium section heading",
            Self::Heading3 => "Small section heading",
            Self::BulletedList => "Create an unordered list",
            Self::NumberedList => "Create an ordered list",
            Self::Quote => "Emphasize a quotation",
            Self::Bold => "Strong emphasis",
            Self::Italic => "Light emphasis",
            Self::InlineCode => "Monospaced inline text",
        }
    }

    pub(crate) fn icon(self) -> IconName {
        match self {
            Self::Paragraph | Self::Heading1 | Self::Heading2 | Self::Heading3 => {
                IconName::ALargeSmall
            }
            Self::BulletedList | Self::NumberedList => IconName::Menu,
            Self::Quote => IconName::Asterisk,
            Self::Bold | Self::Italic => IconName::CaseSensitive,
            Self::InlineCode => IconName::SquareTerminal,
        }
    }

    pub(crate) fn toolbar_label(self) -> &'static str {
        match self {
            Self::Paragraph => "¶",
            Self::Heading1 => "H1",
            Self::Heading2 => "H2",
            Self::Heading3 => "H3",
            Self::BulletedList => "•",
            Self::NumberedList => "1.",
            Self::Quote => "›",
            Self::Bold => "B",
            Self::Italic => "I",
            Self::InlineCode => "<>",
        }
    }

    pub(crate) fn element_id(self) -> &'static str {
        match self {
            Self::Paragraph => "markdown-paragraph",
            Self::Heading1 => "markdown-heading-1",
            Self::Heading2 => "markdown-heading-2",
            Self::Heading3 => "markdown-heading-3",
            Self::BulletedList => "markdown-bullets",
            Self::NumberedList => "markdown-numbers",
            Self::Quote => "markdown-quote",
            Self::Bold => "markdown-bold",
            Self::Italic => "markdown-italic",
            Self::InlineCode => "markdown-code",
        }
    }

    pub(crate) fn matches(self, query: &str) -> bool {
        let query = query.to_ascii_lowercase();
        query.is_empty()
            || self.label().to_ascii_lowercase().contains(&query)
            || (query.len() >= 2 && self.detail().to_ascii_lowercase().contains(&query))
    }
}

/// Host-controlled behavior and appearance for one editor instance.
#[derive(Clone, Debug)]
pub struct MarkdownEditorConfig {
    pub placeholder: SharedString,
    pub max_markdown_bytes: usize,
    pub slash_commands: Vec<MarkdownEditorCommand>,
    pub format_commands: Vec<MarkdownEditorCommand>,
    pub style_policy: MarkdownEditorStylePolicy,
    pub limit_message: SharedString,
}

impl MarkdownEditorConfig {
    /// Parses persisted Markdown under this instance's byte-limit policy.
    pub fn parse_markdown(&self, markdown: &str) -> Result<RichTextBuffer, MarkdownLimitExceeded> {
        RichTextBuffer::try_from_markdown_with_limit(markdown, self.max_markdown_bytes)
    }

    pub(crate) fn normalize(&mut self) {
        deduplicate_commands(&mut self.slash_commands);
        deduplicate_commands(&mut self.format_commands);
    }
}

impl Default for MarkdownEditorConfig {
    fn default() -> Self {
        Self {
            placeholder: "Write Markdown…".into(),
            max_markdown_bytes: DEFAULT_MAX_MARKDOWN_BYTES,
            slash_commands: MarkdownEditorCommand::ALL.to_vec(),
            format_commands: MarkdownEditorCommand::DEFAULT_TOOLBAR.to_vec(),
            style_policy: MarkdownEditorStylePolicy::ComponentTheme,
            limit_message: DEFAULT_LIMIT_MESSAGE.into(),
        }
    }
}

fn deduplicate_commands(commands: &mut Vec<MarkdownEditorCommand>) {
    let mut unique = Vec::with_capacity(commands.len());
    commands.retain(|command| {
        if unique.contains(command) {
            false
        } else {
            unique.push(*command);
            true
        }
    });
}
