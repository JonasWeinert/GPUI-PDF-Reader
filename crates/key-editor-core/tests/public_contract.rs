//! Exercises `key-editor-core` strictly through its public consumer API.

use key_editor_core::{BlockKind, RichTextBuffer, StyleRunCursor};

#[test]
fn another_application_can_edit_and_round_trip_markdown_without_gpui() {
    let mut document =
        RichTextBuffer::try_from_markdown_with_limit("# Reading notes\n\nFirst thought", 8 * 1024)
            .expect("valid bounded Markdown");

    assert_eq!(document.block_kinds()[0], BlockKind::Heading1);
    let thought = document
        .text()
        .find("First thought")
        .expect("parsed plain text");
    document.set_selection(thought..thought + "First".len(), false);
    assert!(document.toggle_bold());
    document.move_to(document.text().len());
    assert!(document.insert_newline());
    assert!(document.toggle_bulleted_list());
    assert!(document.replace_selection("Follow-up"));

    let persisted = document.markdown();
    assert!(persisted.contains("**First** thought"));
    assert!(persisted.contains("- Follow-up"));

    let reopened = RichTextBuffer::try_from_markdown_with_limit(&persisted, 8 * 1024)
        .expect("canonical output remains consumable");
    assert_eq!(reopened.markdown(), persisted);
}

#[test]
fn public_style_cursor_and_utf16_mapping_support_native_adapters() {
    let document = RichTextBuffer::try_from_markdown("**A😀B**").expect("valid Markdown");
    let emoji = document.text().find('😀').expect("emoji in parsed text");
    assert_eq!(document.offset_to_utf16(emoji), 1);
    assert_eq!(document.offset_from_utf16(3), emoji + '😀'.len_utf8());

    let mut cursor = StyleRunCursor::new(document.style_runs());
    assert!(cursor.style_at(emoji).bold());
    assert!(cursor.operations() <= document.style_runs().len() + 1);
}

#[test]
fn edits_fail_atomically_when_canonical_markdown_would_exceed_the_limit() {
    let mut document = RichTextBuffer::with_max_markdown_bytes(8);
    assert!(document.replace_selection("12345678"));
    let before = document.clone();
    assert!(!document.replace_selection("9"));
    assert_eq!(document, before);
}
