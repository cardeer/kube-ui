//! The YAML editor's extras on top of GPUI Kit's code editor. Kit already provides line
//! numbers, indent guides, folding, tree-sitter YAML highlighting, Tab / Shift+Tab indent,
//! smart indent on Enter and a search panel (⌘F); what it lacks lives here: ⌘/ toggles
//! comments, and the colours that make Kit's highlight theme match the app's palette.

use std::{ops::Range, sync::Arc};

use gpui_kit::component::Theme as KitTheme;

use super::theme::{self, Color};

// ---------------------------------------------------------------------------
// Editing helpers (char indices)
// ---------------------------------------------------------------------------

fn byte_at(text: &str, char_idx: usize) -> usize {
    text.char_indices()
        .nth(char_idx)
        .map(|(b, _)| b)
        .unwrap_or(text.len())
}

fn line_start(text: &str, byte: usize) -> usize {
    text[..byte].rfind('\n').map(|i| i + 1).unwrap_or(0)
}

/// Cmd+/: comment out the selected lines, or uncomment them if they all are.
pub fn toggle_comment(text: &mut String, sel: Range<usize>) -> Range<usize> {
    let (start, end) = (byte_at(text, sel.start), byte_at(text, sel.end));
    let first = line_start(text, start);
    let last_end = text[end..]
        .find('\n')
        .map(|i| end + i)
        .unwrap_or(text.len());
    let block = text[first..last_end].to_string();
    let lines: Vec<&str> = block.split('\n').collect();
    let all_commented = lines
        .iter()
        .filter(|l| !l.trim().is_empty())
        .all(|l| l.trim_start().starts_with('#'));
    // Comment marks go at the block's shallowest indentation so they line up.
    let min_indent = lines
        .iter()
        .filter(|l| !l.trim().is_empty())
        .map(|l| l.len() - l.trim_start_matches(' ').len())
        .min()
        .unwrap_or(0);
    let mut delta = 0isize;
    let mut first_delta = 0isize;
    let new: Vec<String> = lines
        .iter()
        .enumerate()
        .map(|(i, l)| {
            let (out, d) = if l.trim().is_empty() {
                (l.to_string(), 0)
            } else if all_commented {
                let pos = l.find('#').unwrap_or(0);
                let skip = if l[pos + 1..].starts_with(' ') { 2 } else { 1 };
                (
                    format!("{}{}", &l[..pos], &l[pos + skip..]),
                    -(skip as isize),
                )
            } else {
                (format!("{}# {}", &l[..min_indent], &l[min_indent..]), 2)
            };
            if i == 0 {
                first_delta = d;
            }
            delta += d;
            out
        })
        .collect();
    text.replace_range(first..last_end, &new.join("\n"));
    let a = (sel.start as isize + first_delta).max(0) as usize;
    let b = (sel.end as isize + delta).max(a as isize) as usize;
    a..b
}


/// Byte range of the 1-based `line` in `text` (without its newline); empty at the end if
/// the line doesn't exist.
pub fn line_byte_range(text: &str, line: usize) -> Range<usize> {
    let mut start = 0;
    for (i, l) in text.split('\n').enumerate() {
        if i + 1 == line {
            return start..start + l.len();
        }
        start += l.len() + 1;
    }
    text.len()..text.len()
}

/// ⌘/ on a Kit document: given the selection as byte offsets, the byte range of the whole
/// lines it touches, their replacement text, and the new selection (byte offsets in the
/// whole document). The caller replaces the block, then restores the selection.
pub fn comment_edit(text: &str, sel: Range<usize>) -> (Range<usize>, String, Range<usize>) {
    let first = line_start(text, sel.start.min(text.len()));
    let last = text[sel.end.min(text.len())..]
        .find('\n')
        .map(|i| sel.end + i)
        .unwrap_or(text.len());
    let mut block = text[first..last].to_string();
    // toggle_comment works on char indices within the block.
    let to_chars = |b: usize| text[first..b.clamp(first, last)].chars().count();
    let chars = to_chars(sel.start)..to_chars(sel.end);
    let new_chars = toggle_comment(&mut block, chars);
    let to_bytes = |c: usize| first + byte_at(&block, c);
    (first..last, block.clone(), to_bytes(new_chars.start)..to_bytes(new_chars.end))
}

// ---------------------------------------------------------------------------
// Syntax colours
// ---------------------------------------------------------------------------

fn hex(c: Color) -> String {
    let b = |v: f32| (v * 255.0).round() as u8;
    format!("#{:02x}{:02x}{:02x}{:02x}", b(c.r), b(c.g), b(c.b), b(c.a))
}

/// Points Kit's highlight theme (tree-sitter YAML captures) and editor colours at the
/// app's palette. Idempotent; call before the first editor renders.
pub fn install_syntax(cx: &mut gpui_kit::App) {
    let style = |c: Color| serde_json::json!({ "color": hex(c) });
    let syntax = serde_json::json!({
        "property": style(theme::SYNTAX_KEY),
        "string": style(theme::SYNTAX_STRING),
        "string.special": style(theme::SYNTAX_STRING),
        "number": style(theme::SYNTAX_LITERAL),
        "boolean": style(theme::SYNTAX_LITERAL),
        "constant": style(theme::SYNTAX_LITERAL),
        "constant.builtin": style(theme::SYNTAX_LITERAL),
        "comment": style(theme::SYNTAX_COMMENT),
        "comment.doc": style(theme::SYNTAX_COMMENT),
        "punctuation": style(theme::TEXT_MUTED),
        "punctuation.delimiter": style(theme::TEXT_MUTED),
        "punctuation.bracket": style(theme::TEXT_MUTED),
        "punctuation.special": style(theme::TEXT_MUTED),
        "punctuation.list_marker": style(theme::TEXT_MUTED),
        "label": style(theme::TEXT_MUTED),
        "type": style(theme::TEXT_MUTED),
        "attribute": style(theme::TEXT_MUTED),
        "variable": style(theme::TEXT),
        "primary": style(theme::TEXT),
    });
    let Ok(syntax) = serde_json::from_value(syntax) else {
        return;
    };
    KitTheme::update(cx, |t| {
        let mut ht = (*t.highlight_theme).clone();
        ht.style.syntax = syntax;
        ht.style.editor_foreground = Some(theme::TEXT.into());
        ht.style.editor_line_number = Some(theme::TEXT_FAINT.into());
        ht.style.editor_active_line_number = Some(theme::TEXT_MUTED.into());
        ht.style.editor_active_line = None;
        ht.style.editor_gutter_background = None;
        t.highlight_theme = Arc::new(ht);
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn comment_toggle_round_trips() {
        let original = "a:\n  b: 1\n  c: 2";
        let mut t = String::from(original);
        let n = t.chars().count();
        let r = toggle_comment(&mut t, 3..n);
        assert_eq!(t, "a:\n  # b: 1\n  # c: 2");
        toggle_comment(&mut t, r);
        assert_eq!(t, original);
    }

    #[test]
    fn comment_edit_covers_whole_lines_in_bytes() {
        // "é" is two bytes: byte offsets differ from char indices.
        let text = "é: 1\nb: 2\nc: 3";
        let sel = text.find("b:").unwrap()..text.find("b:").unwrap() + 1;
        let (block, new, sel2) = comment_edit(text, sel);
        assert_eq!(&text[block.clone()], "b: 2");
        assert_eq!(new, "# b: 2");
        assert_eq!(sel2.start, block.start + 2);
    }

    #[test]
    fn line_ranges() {
        let t = "a: 1\nbb: 2\n";
        assert_eq!(line_byte_range(t, 2), 5..10);
        assert_eq!(line_byte_range(t, 9), 11..11);
    }

    #[test]
    fn hex_has_alpha() {
        assert_eq!(hex(Color::from_rgba8(0xff, 0x80, 0, 1.0)), "#ff8000ff");
    }
}
