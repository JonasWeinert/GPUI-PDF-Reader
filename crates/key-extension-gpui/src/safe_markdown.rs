//! Side-effect-free rendering for untrusted extension Markdown.
//!
//! The general-purpose `gpui-component` Markdown renderer supports clickable
//! links, raw HTML, and remote images. Extension Markdown must not reach those
//! facilities because doing so would bypass the host's typed effect and
//! permission boundary. This module therefore parses a deliberately small,
//! text-only Markdown subset into a model that has no URL target, asset, or
//! callback variants. Links and images are flattened to inert display text.

use std::fmt::Write as _;

const MAX_INLINE_DEPTH: usize = 16;
const MAX_COMPLEX_SCANS_PER_BLOCK: usize = 128;

#[derive(Clone, Debug)]
struct InlineParseBudget {
    references: usize,
    delimiters: usize,
    autolinks: usize,
}

impl Default for InlineParseBudget {
    fn default() -> Self {
        Self {
            references: MAX_COMPLEX_SCANS_PER_BLOCK,
            delimiters: MAX_COMPLEX_SCANS_PER_BLOCK,
            autolinks: MAX_COMPLEX_SCANS_PER_BLOCK,
        }
    }
}

impl InlineParseBudget {
    fn take_reference(&mut self) -> bool {
        take_scan(&mut self.references)
    }

    fn take_delimiter(&mut self) -> bool {
        take_scan(&mut self.delimiters)
    }

    fn take_autolink(&mut self) -> bool {
        take_scan(&mut self.autolinks)
    }
}

fn take_scan(remaining: &mut usize) -> bool {
    if *remaining == 0 {
        return false;
    }
    *remaining -= 1;
    true
}

/// Parsed extension Markdown containing formatting and text only.
///
/// There is intentionally no link, image, HTML, callback, or action variant in
/// this model. A future interactive Markdown feature must introduce an
/// explicit extension command/effect in the public API instead of adding one
/// here.
#[derive(Clone, Debug, Eq, PartialEq)]
struct SafeMarkdownDocument {
    blocks: Vec<SafeBlock>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum SafeBlock {
    Paragraph(Vec<SafeInline>),
    Heading { level: u8, content: Vec<SafeInline> },
    BlockQuote(Vec<SafeInline>),
    UnorderedList(Vec<Vec<SafeInline>>),
    OrderedList(Vec<Vec<SafeInline>>),
    Code(String),
    ThematicBreak,
}

/// Inline formatting is also text-only by construction.
#[derive(Clone, Debug, Eq, PartialEq)]
enum SafeInline {
    Text(String),
    Emphasis(Vec<Self>),
    Strong(Vec<Self>),
    Strikethrough(Vec<Self>),
    Code(String),
    LineBreak,
}

impl SafeMarkdownDocument {
    fn parse(markdown: &str) -> Self {
        let normalized = markdown.replace("\r\n", "\n").replace('\r', "\n");
        let lines = normalized.split('\n').collect::<Vec<_>>();
        let mut blocks = Vec::new();
        let mut line_index = 0;

        while line_index < lines.len() {
            let line = lines[line_index];
            if line.trim().is_empty() {
                line_index += 1;
                continue;
            }

            if let Some((fence_character, fence_length)) = fence_start(line) {
                line_index += 1;
                let mut code_lines = Vec::new();
                while line_index < lines.len()
                    && !is_closing_fence(lines[line_index], fence_character, fence_length)
                {
                    code_lines.push(lines[line_index]);
                    line_index += 1;
                }
                if line_index < lines.len() {
                    line_index += 1;
                }
                blocks.push(SafeBlock::Code(code_lines.join("\n")));
                continue;
            }

            if let Some((level, content)) = heading(line) {
                blocks.push(SafeBlock::Heading {
                    level,
                    content: parse_inlines(content, 0),
                });
                line_index += 1;
                continue;
            }

            if is_thematic_break(line) {
                blocks.push(SafeBlock::ThematicBreak);
                line_index += 1;
                continue;
            }

            if unordered_item(line).is_some() {
                let mut items = Vec::new();
                while line_index < lines.len() {
                    let Some(item) = unordered_item(lines[line_index]) else {
                        break;
                    };
                    items.push(parse_inlines(item, 0));
                    line_index += 1;
                }
                blocks.push(SafeBlock::UnorderedList(items));
                continue;
            }

            if ordered_item(line).is_some() {
                let mut items = Vec::new();
                while line_index < lines.len() {
                    let Some(item) = ordered_item(lines[line_index]) else {
                        break;
                    };
                    items.push(parse_inlines(item, 0));
                    line_index += 1;
                }
                blocks.push(SafeBlock::OrderedList(items));
                continue;
            }

            if quote_content(line).is_some() {
                let mut quote = String::new();
                while line_index < lines.len() {
                    let Some(content) = quote_content(lines[line_index]) else {
                        break;
                    };
                    if !quote.is_empty() {
                        quote.push('\n');
                    }
                    quote.push_str(content);
                    line_index += 1;
                }
                blocks.push(SafeBlock::BlockQuote(parse_inlines(&quote, 0)));
                continue;
            }

            let mut paragraph = String::new();
            while line_index < lines.len()
                && !lines[line_index].trim().is_empty()
                && (paragraph.is_empty() || !starts_block(lines[line_index]))
            {
                if !paragraph.is_empty() {
                    paragraph.push('\n');
                }
                paragraph.push_str(lines[line_index]);
                line_index += 1;
            }
            blocks.push(SafeBlock::Paragraph(parse_inlines(&paragraph, 0)));
        }

        Self { blocks }
    }

    fn to_safe_html(&self) -> String {
        let mut html = String::new();
        for block in &self.blocks {
            match block {
                SafeBlock::Paragraph(content) => {
                    html.push_str("<p>");
                    write_inlines(content, &mut html);
                    html.push_str("</p>");
                }
                SafeBlock::Heading { level, content } => {
                    let _ = write!(html, "<h{level}>");
                    write_inlines(content, &mut html);
                    let _ = write!(html, "</h{level}>");
                }
                SafeBlock::BlockQuote(content) => {
                    html.push_str("<blockquote>");
                    write_inlines(content, &mut html);
                    html.push_str("</blockquote>");
                }
                SafeBlock::UnorderedList(items) => {
                    html.push_str("<ul>");
                    write_list_items(items, &mut html);
                    html.push_str("</ul>");
                }
                SafeBlock::OrderedList(items) => {
                    html.push_str("<ol>");
                    write_list_items(items, &mut html);
                    html.push_str("</ol>");
                }
                SafeBlock::Code(code) => {
                    html.push_str("<pre><code>");
                    write_escaped_text(code, &mut html);
                    html.push_str("</code></pre>");
                }
                SafeBlock::ThematicBreak => html.push_str("<hr>"),
            }
        }
        html
    }

    #[cfg(test)]
    fn plain_text(&self) -> String {
        let mut text = String::new();
        for (index, block) in self.blocks.iter().enumerate() {
            if index > 0 {
                text.push('\n');
            }
            match block {
                SafeBlock::Paragraph(content)
                | SafeBlock::BlockQuote(content)
                | SafeBlock::Heading { content, .. } => write_plain_inlines(content, &mut text),
                SafeBlock::UnorderedList(items) | SafeBlock::OrderedList(items) => {
                    for (item_index, item) in items.iter().enumerate() {
                        if item_index > 0 {
                            text.push('\n');
                        }
                        write_plain_inlines(item, &mut text);
                    }
                }
                SafeBlock::Code(code) => text.push_str(code),
                SafeBlock::ThematicBreak => {}
            }
        }
        text
    }
}

/// Convert untrusted extension Markdown into host-generated HTML containing
/// only a fixed, attribute-free formatting allowlist.
pub(super) fn safe_markdown_html(markdown: &str) -> String {
    SafeMarkdownDocument::parse(markdown).to_safe_html()
}

fn starts_block(line: &str) -> bool {
    fence_start(line).is_some()
        || heading(line).is_some()
        || is_thematic_break(line)
        || unordered_item(line).is_some()
        || ordered_item(line).is_some()
        || quote_content(line).is_some()
}

fn fence_start(line: &str) -> Option<(u8, usize)> {
    let trimmed = line.trim_start();
    let character = *trimmed.as_bytes().first()?;
    if character != b'`' && character != b'~' {
        return None;
    }
    let length = trimmed
        .as_bytes()
        .iter()
        .take_while(|candidate| **candidate == character)
        .count();
    (length >= 3).then_some((character, length))
}

fn is_closing_fence(line: &str, character: u8, minimum_length: usize) -> bool {
    let trimmed = line.trim();
    trimmed.len() >= minimum_length
        && trimmed
            .as_bytes()
            .iter()
            .all(|candidate| *candidate == character)
}

fn heading(line: &str) -> Option<(u8, &str)> {
    let trimmed = line.trim_start();
    let level = trimmed
        .as_bytes()
        .iter()
        .take_while(|character| **character == b'#')
        .count();
    if !(1..=6).contains(&level) {
        return None;
    }
    let content = trimmed.get(level..)?;
    content
        .strip_prefix([' ', '\t'])
        .map(|content| (level as u8, content.trim_end()))
}

fn is_thematic_break(line: &str) -> bool {
    let trimmed = line.trim();
    let mut marker = None;
    let mut count = 0;
    for character in trimmed.chars() {
        if character.is_whitespace() {
            continue;
        }
        if !matches!(character, '-' | '*' | '_') {
            return false;
        }
        if marker.is_some_and(|marker| marker != character) {
            return false;
        }
        marker = Some(character);
        count += 1;
    }
    count >= 3
}

fn unordered_item(line: &str) -> Option<&str> {
    let trimmed = line.trim_start();
    ["- ", "* ", "+ "]
        .iter()
        .find_map(|prefix| trimmed.strip_prefix(prefix))
}

fn ordered_item(line: &str) -> Option<&str> {
    let trimmed = line.trim_start();
    let digits = trimmed
        .as_bytes()
        .iter()
        .take_while(|character| character.is_ascii_digit())
        .count();
    if digits == 0 {
        return None;
    }
    trimmed.get(digits..)?.strip_prefix(". ")
}

fn quote_content(line: &str) -> Option<&str> {
    line.trim_start()
        .strip_prefix('>')
        .map(|content| content.strip_prefix(' ').unwrap_or(content))
}

fn parse_inlines(source: &str, depth: usize) -> Vec<SafeInline> {
    let mut budget = InlineParseBudget::default();
    parse_inlines_with_budget(source, depth, &mut budget)
}

fn parse_inlines_with_budget(
    source: &str,
    depth: usize,
    budget: &mut InlineParseBudget,
) -> Vec<SafeInline> {
    if depth >= MAX_INLINE_DEPTH {
        return vec![SafeInline::Text(source.to_owned())];
    }

    let bytes = source.as_bytes();
    let mut nodes = Vec::new();
    let mut text_start = 0;
    let mut index = 0;

    while index < bytes.len() {
        if bytes[index] == b'\\' {
            let next = index + 1;
            if next < bytes.len() && bytes[next].is_ascii_punctuation() {
                push_source_text(&mut nodes, &source[text_start..index]);
                push_source_text(&mut nodes, &source[next..next + 1]);
                index = next + 1;
                text_start = index;
                continue;
            }
        }

        if source[index..].starts_with("![") && budget.take_reference() {
            if let Some(reference) = bracketed_reference(source, index, true) {
                push_source_text(&mut nodes, &source[text_start..index]);
                push_inert_image(
                    &mut nodes,
                    reference.label,
                    reference.destination,
                    depth,
                    budget,
                );
                index = reference.end;
                text_start = index;
                continue;
            }
        } else if bytes[index] == b'['
            && budget.take_reference()
            && let Some(reference) = bracketed_reference(source, index, false)
        {
            push_source_text(&mut nodes, &source[text_start..index]);
            push_inert_link(
                &mut nodes,
                reference.label,
                reference.destination,
                depth,
                budget,
            );
            index = reference.end;
            text_start = index;
            continue;
        }

        if let Some((delimiter, style)) = inline_delimiter(source, index)
            && budget.take_delimiter()
            && let Some(end) = find_unescaped(source, index + delimiter.len(), delimiter)
        {
            push_source_text(&mut nodes, &source[text_start..index]);
            let content =
                parse_inlines_with_budget(&source[index + delimiter.len()..end], depth + 1, budget);
            nodes.push(match style {
                InlineStyle::Emphasis => SafeInline::Emphasis(content),
                InlineStyle::Strong => SafeInline::Strong(content),
                InlineStyle::Strikethrough => SafeInline::Strikethrough(content),
            });
            index = end + delimiter.len();
            text_start = index;
            continue;
        }

        if bytes[index] == b'`' && budget.take_delimiter() {
            let delimiter_length = bytes[index..]
                .iter()
                .take_while(|character| **character == b'`')
                .count();
            let delimiter = &source[index..index + delimiter_length];
            if let Some(end) = find_unescaped(source, index + delimiter_length, delimiter) {
                push_source_text(&mut nodes, &source[text_start..index]);
                nodes.push(SafeInline::Code(
                    source[index + delimiter_length..end].to_owned(),
                ));
                index = end + delimiter_length;
                text_start = index;
                continue;
            }
        }

        if bytes[index] == b'<'
            && budget.take_autolink()
            && let Some(end_offset) = source[index + 1..].find('>')
        {
            let end = index + 1 + end_offset;
            let candidate = &source[index + 1..end];
            if looks_like_autolink(candidate) {
                push_source_text(&mut nodes, &source[text_start..index]);
                push_source_text(&mut nodes, candidate);
                index = end + 1;
                text_start = index;
                continue;
            }
        }

        if bytes[index] == b'\n' {
            push_source_text(&mut nodes, &source[text_start..index]);
            nodes.push(SafeInline::LineBreak);
            index += 1;
            text_start = index;
            continue;
        }

        index += source[index..].chars().next().map_or(1, char::len_utf8);
    }

    push_source_text(&mut nodes, &source[text_start..]);
    nodes
}

#[derive(Clone, Copy)]
enum InlineStyle {
    Emphasis,
    Strong,
    Strikethrough,
}

fn inline_delimiter(source: &str, index: usize) -> Option<(&str, InlineStyle)> {
    let remainder = &source[index..];
    if remainder.starts_with("**") {
        Some(("**", InlineStyle::Strong))
    } else if remainder.starts_with("__") {
        Some(("__", InlineStyle::Strong))
    } else if remainder.starts_with("~~") {
        Some(("~~", InlineStyle::Strikethrough))
    } else if remainder.starts_with('*') {
        Some(("*", InlineStyle::Emphasis))
    } else if remainder.starts_with('_') {
        Some(("_", InlineStyle::Emphasis))
    } else {
        None
    }
}

fn find_unescaped(source: &str, start: usize, delimiter: &str) -> Option<usize> {
    let mut search_from = start;
    while let Some(offset) = source[search_from..].find(delimiter) {
        let found = search_from + offset;
        let preceding_backslashes = source.as_bytes()[..found]
            .iter()
            .rev()
            .take_while(|character| **character == b'\\')
            .count();
        if preceding_backslashes % 2 == 0 {
            return Some(found);
        }
        search_from = found + delimiter.len();
    }
    None
}

struct BracketedReference<'a> {
    label: &'a str,
    destination: &'a str,
    end: usize,
}

fn bracketed_reference(source: &str, start: usize, image: bool) -> Option<BracketedReference<'_>> {
    let label_start = start + usize::from(image) + 1;
    let label_end = find_balanced_closing(source, label_start, b'[', b']')?;
    if source.as_bytes().get(label_end + 1) != Some(&b'(') {
        return None;
    }
    let destination_start = label_end + 2;
    let destination_end = find_balanced_closing(source, destination_start, b'(', b')')?;
    Some(BracketedReference {
        label: &source[label_start..label_end],
        destination: source[destination_start..destination_end].trim(),
        end: destination_end + 1,
    })
}

fn find_balanced_closing(source: &str, start: usize, open: u8, close: u8) -> Option<usize> {
    let bytes = source.as_bytes();
    let mut depth = 0usize;
    let mut index = start;
    while index < bytes.len() {
        if bytes[index] == b'\\' {
            index += 1;
            if index < bytes.len() {
                index += source[index..].chars().next().map_or(1, char::len_utf8);
            }
            continue;
        }
        if bytes[index] == open {
            depth += 1;
        } else if bytes[index] == close {
            if depth == 0 {
                return Some(index);
            }
            depth -= 1;
        }
        index += source[index..].chars().next().map_or(1, char::len_utf8);
    }
    None
}

fn push_inert_link(
    nodes: &mut Vec<SafeInline>,
    label: &str,
    destination: &str,
    depth: usize,
    budget: &mut InlineParseBudget,
) {
    if label.is_empty() {
        push_source_text(nodes, destination);
        return;
    }
    nodes.extend(parse_inlines_with_budget(label, depth + 1, budget));
    if !destination.is_empty() && destination != label {
        push_source_text(nodes, " (");
        nodes.push(SafeInline::Code(destination.to_owned()));
        push_source_text(nodes, ")");
    }
}

fn push_inert_image(
    nodes: &mut Vec<SafeInline>,
    alternative_text: &str,
    destination: &str,
    depth: usize,
    budget: &mut InlineParseBudget,
) {
    push_source_text(nodes, "Image: ");
    if alternative_text.is_empty() {
        push_source_text(nodes, "unnamed");
    } else {
        nodes.extend(parse_inlines_with_budget(
            alternative_text,
            depth + 1,
            budget,
        ));
    }
    if !destination.is_empty() {
        push_source_text(nodes, " (");
        nodes.push(SafeInline::Code(destination.to_owned()));
        push_source_text(nodes, ")");
    }
}

fn looks_like_autolink(candidate: &str) -> bool {
    let lower = candidate.to_ascii_lowercase();
    lower.starts_with("http://")
        || lower.starts_with("https://")
        || lower.starts_with("mailto:")
        || lower.starts_with("file:")
        || lower.starts_with("data:")
        || (!candidate.contains(char::is_whitespace)
            && candidate.contains('@')
            && candidate.contains('.'))
}

fn push_source_text(nodes: &mut Vec<SafeInline>, text: &str) {
    if text.is_empty() {
        return;
    }
    if let Some(SafeInline::Text(existing)) = nodes.last_mut() {
        existing.push_str(text);
    } else {
        nodes.push(SafeInline::Text(text.to_owned()));
    }
}

fn write_list_items(items: &[Vec<SafeInline>], html: &mut String) {
    for item in items {
        html.push_str("<li>");
        write_inlines(item, html);
        html.push_str("</li>");
    }
}

fn write_inlines(inlines: &[SafeInline], html: &mut String) {
    for inline in inlines {
        match inline {
            SafeInline::Text(text) => write_escaped_text(text, html),
            SafeInline::Emphasis(content) => {
                html.push_str("<em>");
                write_inlines(content, html);
                html.push_str("</em>");
            }
            SafeInline::Strong(content) => {
                html.push_str("<strong>");
                write_inlines(content, html);
                html.push_str("</strong>");
            }
            SafeInline::Strikethrough(content) => {
                html.push_str("<del>");
                write_inlines(content, html);
                html.push_str("</del>");
            }
            SafeInline::Code(code) => {
                html.push_str("<code>");
                write_escaped_text(code, html);
                html.push_str("</code>");
            }
            SafeInline::LineBreak => html.push_str("<br>"),
        }
    }
}

fn write_escaped_text(text: &str, html: &mut String) {
    for character in text.chars() {
        match character {
            '&' => html.push_str("&amp;"),
            '<' => html.push_str("&lt;"),
            '>' => html.push_str("&gt;"),
            '"' => html.push_str("&quot;"),
            '\'' => html.push_str("&#39;"),
            _ => html.push(character),
        }
    }
}

#[cfg(test)]
fn write_plain_inlines(inlines: &[SafeInline], text: &mut String) {
    for inline in inlines {
        match inline {
            SafeInline::Text(content) | SafeInline::Code(content) => text.push_str(content),
            SafeInline::Emphasis(content)
            | SafeInline::Strong(content)
            | SafeInline::Strikethrough(content) => write_plain_inlines(content, text),
            SafeInline::LineBreak => text.push('\n'),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAFE_TAGS: &[&str] = &[
        "blockquote",
        "br",
        "code",
        "del",
        "em",
        "h1",
        "h2",
        "h3",
        "h4",
        "h5",
        "h6",
        "hr",
        "li",
        "ol",
        "p",
        "pre",
        "strong",
        "ul",
    ];

    #[test]
    fn preserves_basic_text_only_markdown_formatting() {
        let markdown = "# Title\n\nA **strong**, *quiet*, ~~old~~, and `coded` value.\n\n- one\n- two\n\n> quoted\n\n```html\n<b>literal</b>\n```";
        let html = safe_markdown_html(markdown);

        assert_eq!(
            html,
            "<h1>Title</h1><p>A <strong>strong</strong>, <em>quiet</em>, <del>old</del>, and <code>coded</code> value.</p><ul><li>one</li><li>two</li></ul><blockquote>quoted</blockquote><pre><code>&lt;b&gt;literal&lt;/b&gt;</code></pre>"
        );
        assert_only_safe_attribute_free_tags(&html);
    }

    #[test]
    fn raw_html_is_always_literal_and_cannot_create_active_nodes() {
        let markdown = r#"<script>fetch('https://tracker.invalid')</script>
<img src="file:///private/secret" onerror="alert(1)">
<a href="https://example.invalid">click me</a>
<iframe srcdoc="bad"></iframe>"#;
        let html = safe_markdown_html(markdown);

        assert!(html.contains("&lt;script&gt;"));
        assert!(html.contains("&lt;img src=&quot;file:///private/secret&quot;"));
        assert!(html.contains("&lt;a href=&quot;https://example.invalid&quot;&gt;"));
        assert!(html.contains("&lt;iframe srcdoc=&quot;bad&quot;&gt;"));
        assert!(!html.contains("<script"));
        assert!(!html.contains("<img"));
        assert!(!html.contains("<a "));
        assert!(!html.contains("<iframe"));
        assert_only_safe_attribute_free_tags(&html);
    }

    #[test]
    fn markdown_images_and_links_become_inert_selectable_text() {
        let markdown = r#"![remote](https://tracker.invalid/pixel.png)
![local](file:///private/secret.png)
![inline](data:image/svg+xml,<svg onload=alert(1)>)
[web](https://example.invalid)
[script](javascript:alert(1))
[file](file:///private/secret)
<https://autolink.invalid>
<user@example.invalid>"#;
        let document = SafeMarkdownDocument::parse(markdown);
        let plain_text = document.plain_text();
        let html = document.to_safe_html();

        assert!(plain_text.contains("Image: remote (https://tracker.invalid/pixel.png)"));
        assert!(plain_text.contains("Image: local (file:///private/secret.png)"));
        assert!(plain_text.contains("javascript:alert(1)"));
        assert!(plain_text.contains("https://autolink.invalid"));
        assert!(plain_text.contains("user@example.invalid"));

        // The render model contains formatting/text only and the generated HTML
        // has no element capable of loading or opening any of these targets.
        assert!(!html.contains("<img"));
        assert!(!html.contains("<a "));
        assert!(!html.contains("href="));
        assert!(!html.contains("src="));
        assert_only_safe_attribute_free_tags(&html);
    }

    #[test]
    fn hostile_link_labels_and_destinations_remain_escaped_text() {
        let markdown = r#"[<img src=x onerror=alert(1)>](https://example.invalid/" onclick="bad)
![</code><script>bad</script>](file:///tmp/a.png)"#;
        let html = safe_markdown_html(markdown);

        assert!(html.contains("&lt;img src=x onerror=alert(1)&gt;"));
        assert!(html.contains("&lt;script&gt;bad&lt;/script&gt;"));
        assert!(!html.contains("<img"));
        assert!(!html.contains("<script"));
        assert_only_safe_attribute_free_tags(&html);
    }

    #[test]
    fn unicode_escapes_and_unclosed_markup_never_break_safe_output() {
        let inputs = [
            r"[résumé](file:///tmp/\épreuve.pdf)",
            r"![图](https://example.invalid/\画像.png)",
            r"[unterminated](https://example.invalid",
            r"<img src='https://example.invalid/é.png'",
            r"**nested *formatting with ünicode* safely**",
        ];

        for markdown in inputs {
            let html = safe_markdown_html(markdown);
            assert_only_safe_attribute_free_tags(&html);
            assert!(!html.contains("<img"));
            assert!(!html.contains("<a "));
        }
    }

    #[test]
    fn pathological_unclosed_constructs_have_bounded_complex_scanning() {
        for markdown in ["[".repeat(16_384), "<".repeat(16_384)] {
            let document = SafeMarkdownDocument::parse(&markdown);
            assert_eq!(document.plain_text(), markdown);
            assert_only_safe_attribute_free_tags(&document.to_safe_html());
        }
    }

    fn assert_only_safe_attribute_free_tags(html: &str) {
        let mut remainder = html;
        while let Some(start) = remainder.find('<') {
            remainder = &remainder[start + 1..];
            let end = remainder.find('>').expect("generated tag must close");
            let raw_tag = &remainder[..end];
            let tag = raw_tag.strip_prefix('/').unwrap_or(raw_tag);
            assert!(
                SAFE_TAGS.contains(&tag),
                "unsafe or attributed generated tag: <{raw_tag}>"
            );
            remainder = &remainder[end + 1..];
        }
    }
}
