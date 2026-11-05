//! A small YAML code editor on top of egui's `TextEdit`: line-number gutter, syntax
//! colours, indent guides, current-line highlight, and editor keys (auto-indent on Enter,
//! Tab / Shift+Tab indent by two spaces, Cmd+/ toggles comments, Cmd+S saves).

use std::sync::Arc;

use eframe::egui::{
    self, Color32, FontId, Galley, Key, Modifiers, Rect, Shape, Stroke, TextBuffer, TextFormat,
    text::{CCursor, CCursorRange, LayoutJob},
    widgets::text_edit::TextEditState,
};

use super::theme;

const INDENT: usize = 2;
const GUTTER_PAD: f32 = 14.0;

pub struct EditorOutput {
    /// The text changed this frame.
    pub changed: bool,
    /// Cmd+S was pressed.
    pub save: bool,
}

/// An editable YAML editor. `error_line` (1-based) is marked in the gutter.
/// `start` puts the cursor at the top and focuses the editor (when editing begins).
pub fn yaml_editor(ui: &mut egui::Ui, id: egui::Id, text: &mut String, error_line: Option<usize>, start: bool) -> EditorOutput {
    if start {
        let mut state = TextEditState::load(ui.ctx(), id).unwrap_or_default();
        state.cursor.set_char_range(Some(CCursorRange::one(CCursor::new(0))));
        state.store(ui.ctx(), id);
        ui.memory_mut(|m| m.request_focus(id));
    }
    let mut changed = false;
    let mut save = false;
    let focused = ui.memory(|m| m.has_focus(id));
    if focused {
        // Handle editor keys ourselves before TextEdit sees them.
        let (enter, tab, untab, comment, cmd_s) = ui.input_mut(|i| {
            (
                i.consume_key(Modifiers::NONE, Key::Enter),
                i.consume_key(Modifiers::NONE, Key::Tab),
                i.consume_key(Modifiers::SHIFT, Key::Tab),
                i.consume_key(Modifiers::COMMAND, Key::Slash),
                i.consume_key(Modifiers::COMMAND, Key::S),
            )
        });
        save = cmd_s;
        if enter || tab || untab || comment {
            let mut state = TextEditState::load(ui.ctx(), id).unwrap_or_default();
            let range = state.cursor.char_range().unwrap_or_else(|| CCursorRange::one(CCursor::new(text.chars().count())));
            let new_range = if enter {
                Some(newline_with_indent(text, range))
            } else if comment {
                Some(toggle_comment(text, range))
            } else {
                Some(indent_lines(text, range, untab))
            };
            if let Some(r) = new_range {
                state.cursor.set_char_range(Some(r));
                state.store(ui.ctx(), id);
                changed = true;
            }
        }
    }
    let out = show(ui, id, text, true, error_line);
    EditorOutput { changed: changed || out, save }
}

/// The same editor, read-only (selectable, copyable).
pub fn yaml_viewer(ui: &mut egui::Ui, id: egui::Id, text: &str) {
    let mut s = text;
    show(ui, id, &mut s, false, None);
}

fn show(ui: &mut egui::Ui, id: egui::Id, buffer: &mut dyn TextBuffer, editable: bool, error_line: Option<usize>) -> bool {
    let font = egui::TextStyle::Monospace.resolve(ui.style());
    let char_w = ui.fonts_mut(|f| f.glyph_width(&font, ' ')).max(1.0);
    let lines = buffer.as_str().split('\n').count().max(1);
    let digits = lines.to_string().len().max(2) as f32;
    let gutter = digits * char_w + GUTTER_PAD * 2.0;

    let mut layouter = |ui: &egui::Ui, buf: &dyn TextBuffer, _wrap_width: f32| -> Arc<Galley> {
        let mut job = highlight(buf.as_str(), &font);
        // No soft wrap: one row per line keeps numbers and guides aligned; long lines scroll sideways.
        job.wrap.max_width = f32::INFINITY;
        ui.fonts_mut(|f| f.layout_job(job))
    };

    // Painted behind the text once we know where the rows landed.
    let background = ui.painter().add(Shape::Noop);
    let out = egui::TextEdit::multiline(buffer)
        .id(id)
        .font(egui::TextStyle::Monospace)
        .code_editor()
        .frame(false)
        .interactive(true)
        .margin(egui::Margin { left: gutter as i8, right: 12, top: 6, bottom: 6 })
        .desired_width(f32::INFINITY)
        .min_size(ui.available_size())
        .layouter(&mut layouter)
        .show(ui);

    let text = out.galley.text();
    let rect = out.response.rect;
    let visible = ui.clip_rect();
    let origin = out.galley_pos.to_vec2();
    let row_left = rect.left() + gutter - 4.0;
    let row_right = rect.right().max(visible.right());

    // Which logical line holds the cursor.
    let cursor_line = out.cursor_range.filter(|_| editable || out.response.has_focus()).map(|r| {
        text.chars().take(r.primary.index).filter(|c| *c == '\n').count()
    });
    let indents = line_indents(text);

    let mut shapes = Vec::new();
    // Gutter ground and its hairline.
    let gutter_rect = Rect::from_min_max(rect.min, egui::pos2(rect.left() + gutter - 8.0, rect.bottom().max(visible.bottom())));
    shapes.push(Shape::rect_filled(gutter_rect, 0.0, theme::WELL));
    shapes.push(Shape::line_segment(
        [gutter_rect.right_top(), gutter_rect.right_bottom()],
        Stroke::new(1.0, theme::BORDER),
    ));

    let mut numbers = Vec::new();
    let mut line = 0usize;
    let mut first_row_of_line = true;
    for row in &out.galley.rows {
        let r = row.rect().translate(origin);
        if r.bottom() >= visible.top() && r.top() <= visible.bottom() {
            let band = Rect::from_x_y_ranges(row_left..=row_right, r.y_range());
            if error_line == Some(line + 1) {
                shapes.push(Shape::rect_filled(band, 0.0, theme::BAD.gamma_multiply(0.14)));
            } else if cursor_line == Some(line) {
                shapes.push(Shape::rect_filled(band, 0.0, theme::FIELD));
            }
            // Indent guides at every two columns inside the line's indentation.
            let indent = indents.get(line).copied().unwrap_or(0);
            let mut col = 0;
            while col < indent {
                let x = (out.galley_pos.x + col as f32 * char_w).round() + 0.5;
                shapes.push(Shape::line_segment([egui::pos2(x, r.top()), egui::pos2(x, r.bottom())], Stroke::new(1.0, theme::BORDER_STRONG)));
                col += INDENT;
            }
            if first_row_of_line {
                let color = if error_line == Some(line + 1) {
                    theme::BAD
                } else if cursor_line == Some(line) {
                    theme::TEXT
                } else {
                    theme::TEXT_FAINT
                };
                numbers.push((egui::pos2(rect.left() + gutter - GUTTER_PAD - 4.0, r.center().y), (line + 1).to_string(), color));
            }
        }
        first_row_of_line = row.ends_with_newline;
        if row.ends_with_newline {
            line += 1;
        }
    }
    ui.painter().set(background, Shape::Vec(shapes));
    let painter = ui.painter();
    for (pos, n, color) in numbers {
        painter.text(pos, egui::Align2::RIGHT_CENTER, n, font.clone(), color);
    }
    if out.response.hovered() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::Text);
    }
    out.response.changed()
}

/// Leading spaces per line; blank lines take the smaller of their neighbours so guides
/// run through gaps in a block instead of breaking.
fn line_indents(text: &str) -> Vec<usize> {
    let raw: Vec<Option<usize>> = text
        .split('\n')
        .map(|l| (!l.trim().is_empty()).then(|| l.len() - l.trim_start_matches(' ').len()))
        .collect();
    let mut out = vec![0; raw.len()];
    for i in 0..raw.len() {
        out[i] = match raw[i] {
            Some(n) => n,
            None => {
                let prev = raw[..i].iter().rev().flatten().next().copied().unwrap_or(0);
                let next = raw[i + 1..].iter().flatten().next().copied().unwrap_or(0);
                prev.min(next)
            }
        };
    }
    out
}

// ---------------------------------------------------------------------------
// Editing helpers (char indices, as egui's cursors use)
// ---------------------------------------------------------------------------

fn byte_at(text: &str, char_idx: usize) -> usize {
    text.char_indices().nth(char_idx).map(|(b, _)| b).unwrap_or(text.len())
}

fn line_start(text: &str, byte: usize) -> usize {
    text[..byte].rfind('\n').map(|i| i + 1).unwrap_or(0)
}

/// Enter: newline plus the current line's indentation, one level deeper after a `key:`
/// or inside a `- ` list item.
fn newline_with_indent(text: &mut String, range: CCursorRange) -> CCursorRange {
    let sel = range.as_sorted_char_range();
    let (start, end) = (byte_at(text, sel.start), byte_at(text, sel.end));
    let line = &text[line_start(text, start)..start];
    let indent = line.len() - line.trim_start_matches(' ').len();
    let body = line.trim();
    let code = body.split(" #").next().unwrap_or(body).trim_end();
    let extra = if body.starts_with('#') {
        0
    } else if code.ends_with(':') || code.ends_with('|') || code.ends_with('>') || code.ends_with("|-") || code.ends_with(">-") {
        INDENT
    } else if code.starts_with("- ") || code == "-" {
        INDENT
    } else {
        0
    };
    let insert = format!("\n{}", " ".repeat(indent + extra));
    text.replace_range(start..end, &insert);
    CCursorRange::one(CCursor::new(sel.start + insert.chars().count()))
}

/// Tab / Shift+Tab: with a selection over several lines, (un)indent them all; otherwise
/// insert spaces up to the next indent stop (or remove up to one level).
fn indent_lines(text: &mut String, range: CCursorRange, outdent: bool) -> CCursorRange {
    let sel = range.as_sorted_char_range();
    let (start, end) = (byte_at(text, sel.start), byte_at(text, sel.end));
    let multi = text[start..end].contains('\n');
    if !multi && !outdent {
        let col = start - line_start(text, start);
        let n = INDENT - col % INDENT;
        text.replace_range(start..end, &" ".repeat(n));
        return CCursorRange::one(CCursor::new(sel.start + n));
    }
    // Whole lines from the selection's first line to its last.
    let first = line_start(text, start);
    let last_end = text[end..].find('\n').map(|i| end + i).unwrap_or(text.len());
    let block = text[first..last_end].to_string();
    let mut removed_first = 0isize;
    let mut delta = 0isize;
    let new_block: Vec<String> = block
        .split('\n')
        .enumerate()
        .map(|(i, l)| {
            let change = if outdent {
                let n = (l.len() - l.trim_start_matches(' ').len()).min(INDENT);
                -(n as isize)
            } else if l.trim().is_empty() {
                0
            } else {
                INDENT as isize
            };
            if i == 0 {
                removed_first = change;
            }
            delta += change;
            if change >= 0 {
                format!("{}{l}", " ".repeat(change as usize))
            } else {
                l[(-change) as usize..].to_string()
            }
        })
        .collect();
    text.replace_range(first..last_end, &new_block.join("\n"));
    let a = (sel.start as isize + removed_first).max(0) as usize;
    let b = (sel.end as isize + delta).max(a as isize) as usize;
    if multi { CCursorRange::two(CCursor::new(a), CCursor::new(b)) } else { CCursorRange::one(CCursor::new(a)) }
}

/// Cmd+/: comment out the selected lines, or uncomment them if they all are.
fn toggle_comment(text: &mut String, range: CCursorRange) -> CCursorRange {
    let sel = range.as_sorted_char_range();
    let (start, end) = (byte_at(text, sel.start), byte_at(text, sel.end));
    let first = line_start(text, start);
    let last_end = text[end..].find('\n').map(|i| end + i).unwrap_or(text.len());
    let block = text[first..last_end].to_string();
    let lines: Vec<&str> = block.split('\n').collect();
    let all_commented = lines.iter().filter(|l| !l.trim().is_empty()).all(|l| l.trim_start().starts_with('#'));
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
                (format!("{}{}", &l[..pos], &l[pos + skip..]), -(skip as isize))
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
    if a == b { CCursorRange::one(CCursor::new(a)) } else { CCursorRange::two(CCursor::new(a), CCursor::new(b)) }
}

// ---------------------------------------------------------------------------
// Syntax colours (a line-based YAML tokenizer; good enough for Kubernetes objects)
// ---------------------------------------------------------------------------

fn highlight(text: &str, font: &FontId) -> LayoutJob {
    let mut job = LayoutJob::default();
    let mut push = |s: &str, color: Color32| {
        if !s.is_empty() {
            job.append(s, 0.0, TextFormat { font_id: font.clone(), color, ..Default::default() });
        }
    };
    let mut lines = text.split('\n').peekable();
    // Inside a `|` / `>` block scalar: lines deeper than this indent are plain text.
    let mut block_indent: Option<usize> = None;
    while let Some(line) = lines.next() {
        let indent = line.len() - line.trim_start_matches(' ').len();
        let (lead, rest) = line.split_at(indent);
        push(lead, theme::TEXT);
        if let Some(b) = block_indent {
            if rest.is_empty() || indent > b {
                push(rest, theme::SYNTAX_STRING);
                if lines.peek().is_some() {
                    push("\n", theme::TEXT);
                }
                continue;
            }
            block_indent = None;
        }
        let mut rest = rest;
        if rest.starts_with('#') {
            push(rest, theme::SYNTAX_COMMENT);
        } else if rest == "---" || rest == "..." {
            push(rest, theme::TEXT_FAINT);
        } else {
            while let Some(after) = rest.strip_prefix("- ").or_else(|| (rest == "-").then_some("")) {
                push(&rest[..rest.len() - after.len()], theme::TEXT_MUTED);
                rest = after;
            }
            match key_split(rest) {
                Some((key, value)) => {
                    push(key, theme::SYNTAX_KEY);
                    push(":", theme::TEXT_MUTED);
                    let value = &value[1..];
                    let trimmed = value.trim();
                    if matches!(trimmed, "|" | ">" | "|-" | ">-" | "|+" | ">+") {
                        block_indent = Some(indent);
                    }
                    push_value(&mut push, value);
                }
                None => push_value(&mut push, rest),
            }
        }
        if lines.peek().is_some() {
            push("\n", theme::TEXT);
        }
    }
    job
}

/// Splits `key: value` at the mapping colon (one followed by a space or the line end,
/// outside quotes). Returns (key, ":value...").
fn key_split(s: &str) -> Option<(&str, &str)> {
    let mut quote = None;
    for (i, c) in s.char_indices() {
        match (quote, c) {
            (None, '"' | '\'') if i == 0 => quote = Some(c),
            (Some(q), c) if c == q => quote = None,
            (None, ':') => {
                let next = s[i + 1..].chars().next();
                if next.is_none() || next == Some(' ') {
                    return Some((&s[..i], &s[i..]));
                }
            }
            (None, '#') if i > 0 && s[..i].ends_with(' ') => return None,
            _ => {}
        }
    }
    None
}

fn push_value(push: &mut impl FnMut(&str, Color32), value: &str) {
    // Trailing comment.
    let (code, comment) = match value.find(" #") {
        Some(i) if !value[..i].contains('"') && !value[..i].contains('\'') => (&value[..i], &value[i..]),
        _ => (value, ""),
    };
    let t = code.trim();
    let lead = &code[..code.len() - code.trim_start().len()];
    push(lead, theme::TEXT);
    let color = if t.starts_with('"') || t.starts_with('\'') {
        theme::SYNTAX_STRING
    } else if matches!(t, "true" | "false" | "null" | "~" | "True" | "False" | "yes" | "no")
        || t.parse::<f64>().is_ok()
    {
        theme::SYNTAX_LITERAL
    } else if matches!(t, "|" | ">" | "|-" | ">-" | "|+" | ">+" | "{}" | "[]") || t.starts_with('&') || t.starts_with('*') {
        theme::TEXT_MUTED
    } else {
        theme::TEXT
    };
    push(code.trim_start(), color);
    push(comment, theme::SYNTAX_COMMENT);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cur(i: usize) -> CCursorRange {
        CCursorRange::one(CCursor::new(i))
    }

    #[test]
    fn enter_keeps_and_deepens_indent() {
        let mut t = String::from("spec:\n  containers:");
        let n = t.chars().count();
        let r = newline_with_indent(&mut t, cur(n));
        assert_eq!(t, "spec:\n  containers:\n    ");
        assert_eq!(r.primary.index, t.chars().count());

        let mut t = String::from("  - name: app");
        let n = t.len();
        newline_with_indent(&mut t, cur(n));
        assert_eq!(t, "  - name: app\n    ");

        let mut t = String::from("    image: nginx");
        let n = t.len();
        newline_with_indent(&mut t, cur(n));
        assert_eq!(t, "    image: nginx\n    ");
    }

    #[test]
    fn tab_and_shift_tab() {
        let mut t = String::from("a:\nb");
        let r = indent_lines(&mut t, cur(3), false);
        assert_eq!(t, "a:\n  b");
        assert_eq!(r.primary.index, 5);

        let mut t = String::from("a:\n    b: 1\n    c: 2");
        indent_lines(&mut t, CCursorRange::two(CCursor::new(4), CCursor::new(18)), true);
        assert_eq!(t, "a:\n  b: 1\n  c: 2");
    }

    #[test]
    fn comment_toggle_round_trips() {
        let original = "a:\n  b: 1\n  c: 2";
        let mut t = String::from(original);
        let n = t.chars().count();
        let r = toggle_comment(&mut t, CCursorRange::two(CCursor::new(3), CCursor::new(n)));
        assert_eq!(t, "a:\n  # b: 1\n  # c: 2");
        toggle_comment(&mut t, r);
        assert_eq!(t, original);
    }

    #[test]
    fn guides_bridge_blank_lines() {
        assert_eq!(line_indents("a:\n  b:\n\n    c: 1\n  d: 2"), vec![0, 2, 2, 4, 2]);
    }

    #[test]
    fn key_split_ignores_urls_and_quotes() {
        assert_eq!(key_split("image: nginx:1.27").map(|k| k.0), Some("image"));
        assert_eq!(key_split("url: http://x").map(|k| k.0), Some("url"));
        assert_eq!(key_split("\"a: b\": 1").map(|k| k.0), Some("\"a: b\""));
        assert_eq!(key_split("plain value"), None);
    }
}
