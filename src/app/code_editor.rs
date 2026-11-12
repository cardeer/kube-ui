//! A small YAML code editor on top of egui's `TextEdit`: line-number gutter, syntax
//! colours, indent guides, current-line highlight, and editor keys (auto-indent on Enter,
//! Tab / Shift+Tab indent by two spaces, Cmd+/ toggles comments, Cmd+S saves), plus a
//! find bar (Cmd+F) that highlights matches in the viewer and the editor.

use std::{
    hash::{Hash, Hasher},
    ops::Range,
    sync::Arc,
};

use eframe::egui::{
    self, Color32, FontId, Galley, Key, Modifiers, Rect, Shape, Stroke, TextBuffer, TextFormat,
    text::{CCursor, CCursorRange, LayoutJob},
    widgets::text_edit::TextEditState,
};

use egui_phosphor::regular as icon;

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
pub fn yaml_editor(
    ui: &mut egui::Ui,
    id: egui::Id,
    text: &mut String,
    error_line: Option<usize>,
    start: bool,
    find: &mut Find,
) -> EditorOutput {
    if start {
        let mut state = TextEditState::load(ui.ctx(), id).unwrap_or_default();
        state
            .cursor
            .set_char_range(Some(CCursorRange::one(CCursor::new(0))));
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
            let range = state
                .cursor
                .char_range()
                .unwrap_or_else(|| CCursorRange::one(CCursor::new(text.chars().count())));
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
    let out = show(ui, id, text, true, error_line, find);
    EditorOutput {
        changed: changed || out,
        save,
    }
}

/// The same editor, read-only (selectable, copyable).
pub fn yaml_viewer(ui: &mut egui::Ui, id: egui::Id, text: &str, find: &mut Find) {
    let mut s = text;
    show(ui, id, &mut s, false, None, find);
}

fn show(
    ui: &mut egui::Ui,
    id: egui::Id,
    buffer: &mut dyn TextBuffer,
    editable: bool,
    error_line: Option<usize>,
    find: &mut Find,
) -> bool {
    find.update(buffer.as_str());
    let reveal = std::mem::take(&mut find.reveal)
        .then(|| find.current_match())
        .flatten();
    if let Some(m) = &reveal {
        // Select the current match so closing the bar leaves the cursor on it.
        let mut state = TextEditState::load(ui.ctx(), id).unwrap_or_default();
        state.cursor.set_char_range(Some(CCursorRange::two(
            CCursor::new(m.start),
            CCursor::new(m.end),
        )));
        state.store(ui.ctx(), id);
    }

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
        .margin(egui::Margin {
            left: gutter as i8,
            right: 12,
            top: 6,
            bottom: 6,
        })
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
    let cursor_line = out
        .cursor_range
        .filter(|_| editable || out.response.has_focus())
        .map(|r| {
            text.chars()
                .take(r.primary.index)
                .filter(|c| *c == '\n')
                .count()
        });
    let indents = line_indents(text);

    let mut shapes = Vec::new();
    // Gutter ground and its hairline.
    let gutter_rect = Rect::from_min_max(
        rect.min,
        egui::pos2(
            rect.left() + gutter - 8.0,
            rect.bottom().max(visible.bottom()),
        ),
    );
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
                shapes.push(Shape::rect_filled(
                    band,
                    0.0,
                    theme::BAD.gamma_multiply(0.14),
                ));
            } else if cursor_line == Some(line) {
                shapes.push(Shape::rect_filled(band, 0.0, theme::FIELD));
            }
            // Indent guides at every two columns inside the line's indentation.
            let indent = indents.get(line).copied().unwrap_or(0);
            let mut col = 0;
            while col < indent {
                let x = (out.galley_pos.x + col as f32 * char_w).round() + 0.5;
                shapes.push(Shape::line_segment(
                    [egui::pos2(x, r.top()), egui::pos2(x, r.bottom())],
                    Stroke::new(1.0, theme::BORDER_STRONG),
                ));
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
                numbers.push((
                    egui::pos2(rect.left() + gutter - GUTTER_PAD - 4.0, r.center().y),
                    (line + 1).to_string(),
                    color,
                ));
            }
        }
        first_row_of_line = row.ends_with_newline;
        if row.ends_with_newline {
            line += 1;
        }
    }
    // Find matches: a soft tint on each visible one, a stronger one on the current.
    if find.open && !find.matches.is_empty() {
        let current = find.current_match();
        for m in &find.matches {
            let r = match_rect(&out.galley, text, m).translate(origin);
            if r.bottom() < visible.top() || r.top() > visible.bottom() {
                continue;
            }
            if current.as_ref() == Some(m) {
                shapes.push(Shape::rect_filled(r, 2.0, theme::WARN.gamma_multiply(0.45)));
                shapes.push(Shape::rect_stroke(
                    r,
                    2.0,
                    Stroke::new(1.0, theme::WARN),
                    egui::StrokeKind::Outside,
                ));
            } else {
                shapes.push(Shape::rect_filled(r, 2.0, theme::WARN.gamma_multiply(0.18)));
            }
        }
    }
    if let Some(m) = &reveal {
        let r = match_rect(&out.galley, text, m).translate(origin);
        ui.scroll_to_rect(r.expand2(egui::vec2(40.0, 0.0)), Some(egui::Align::Center));
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

/// Where a match sits in the galley; one spanning lines is drawn to the end of its first line.
fn match_rect(galley: &Galley, text: &str, m: &Range<usize>) -> Rect {
    let first_line = text
        .chars()
        .skip(m.start)
        .take(m.end - m.start)
        .take_while(|c| *c != '\n')
        .count();
    let a = galley.pos_from_cursor(CCursor::new(m.start));
    let b = galley.pos_from_cursor(CCursor::new(m.start + first_line));
    let width = (b.left() - a.left()).max(3.0);
    Rect::from_min_size(a.min, egui::vec2(width, a.height()))
}

// ---------------------------------------------------------------------------
// Find bar
// ---------------------------------------------------------------------------

/// Matches beyond this are not collected (keeps huge documents responsive).
const MAX_MATCHES: usize = 10_000;

/// Find-in-document state: the query, its matches (char ranges, as egui's cursors use)
/// and which one is current. One instance serves the YAML window's viewer and editor.
#[derive(Default)]
pub struct Find {
    pub open: bool,
    query: String,
    regex: bool,
    case_sensitive: bool,
    matches: Vec<Range<usize>>,
    current: usize,
    error: Option<String>,
    /// (query, regex, case, text hash) the matches were computed for.
    key: Option<(String, bool, bool, u64)>,
    /// Cursor position when the bar opened; the first match after it becomes current.
    anchor: usize,
    /// Select and scroll to the current match on the next frame.
    reveal: bool,
    focus_field: bool,
    /// The query field had focus at the end of the last frame.
    pub field_focused: bool,
}

impl Find {
    /// Cmd+F: open (or refocus) the bar. A single-line selection in the document becomes the query.
    pub fn open(&mut self, ctx: &egui::Context, doc_id: egui::Id, text: &str) {
        self.open = true;
        self.focus_field = true;
        if let Some(range) = TextEditState::load(ctx, doc_id).and_then(|s| s.cursor.char_range()) {
            let sel = range.as_sorted_char_range();
            self.anchor = sel.start;
            let selected: String = text
                .chars()
                .skip(sel.start)
                .take(sel.end - sel.start)
                .collect();
            if !selected.is_empty() && !selected.contains('\n') {
                self.query = if self.regex {
                    regex::escape(&selected)
                } else {
                    selected
                };
            }
        }
        self.key = None;
    }

    /// Open the bar with a query (development screenshots).
    pub fn preset(&mut self, query: String) {
        self.open = true;
        self.query = query;
        self.key = None;
    }

    pub fn close(&mut self, ctx: &egui::Context, doc_id: egui::Id) {
        self.open = false;
        self.field_focused = false;
        ctx.memory_mut(|m| m.request_focus(doc_id));
    }

    fn current_match(&self) -> Option<Range<usize>> {
        self.matches.get(self.current).cloned()
    }

    /// Step to the next (or previous) match, wrapping around.
    pub fn step(&mut self, back: bool) {
        let n = self.matches.len();
        if n == 0 {
            return;
        }
        self.current = if back {
            (self.current + n - 1) % n
        } else {
            (self.current + 1) % n
        };
        self.reveal = true;
    }

    /// Recompute matches when the query, options or text changed.
    fn update(&mut self, text: &str) {
        let mut h = std::collections::hash_map::DefaultHasher::new();
        text.hash(&mut h);
        let key = (
            self.query.clone(),
            self.regex,
            self.case_sensitive,
            h.finish(),
        );
        if self.key.as_ref() == Some(&key) {
            return;
        }
        let query_changed = self
            .key
            .as_ref()
            .is_none_or(|k| (&k.0, k.1, k.2) != (&key.0, key.1, key.2));
        self.key = Some(key);
        let (matches, error) = find_matches(text, &self.query, self.regex, self.case_sensitive);
        self.matches = matches;
        self.error = error;
        if query_changed {
            // Start from the first match at or after where the cursor was.
            self.current = self
                .matches
                .iter()
                .position(|m| m.start >= self.anchor)
                .unwrap_or(0);
            self.reveal = !self.matches.is_empty();
        } else if self.current >= self.matches.len() {
            self.current = self.matches.len().saturating_sub(1);
        }
    }

    /// The bar: query field, case / regex toggles, match count, previous / next, close.
    pub fn bar(&mut self, ui: &mut egui::Ui, doc_id: egui::Id, text: &str) {
        let field_id = egui::Id::new("yaml-find-field");
        // Enter / Shift+Enter step through matches; take them before the field surrenders focus.
        let (mut next, mut prev, mut esc) = (false, false, false);
        if ui.memory(|m| m.has_focus(field_id)) {
            ui.input_mut(|i| {
                prev = i.consume_key(Modifiers::SHIFT, Key::Enter);
                next = i.consume_key(Modifiers::NONE, Key::Enter);
                esc = i.consume_key(Modifiers::NONE, Key::Escape);
            });
        }
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 4.0;
            let hint = if self.regex { "Find (regex)" } else { "Find" };
            let resp = ui.add(
                egui::TextEdit::singleline(&mut self.query)
                    .id(field_id)
                    .hint_text(
                        egui::RichText::new(format!("{}  {hint}", icon::MAGNIFYING_GLASS))
                            .color(theme::TEXT_FAINT),
                    )
                    .desired_width(240.0)
                    .margin(egui::Margin::symmetric(8, 5)),
            );
            if std::mem::take(&mut self.focus_field) {
                resp.request_focus();
                if let Some(mut state) = TextEditState::load(ui.ctx(), field_id) {
                    let n = self.query.chars().count();
                    state
                        .cursor
                        .set_char_range(Some(CCursorRange::two(CCursor::new(0), CCursor::new(n))));
                    state.store(ui.ctx(), field_id);
                }
            }
            if theme::toggle_button(ui, "Aa", self.case_sensitive, "Match case").clicked() {
                self.case_sensitive = !self.case_sensitive;
            }
            if theme::toggle_button(ui, ".*", self.regex, "Regular expression").clicked() {
                self.regex = !self.regex;
            }
            self.update(text);
            if let Some(err) = &self.error {
                ui.label(
                    egui::RichText::new(format!("{} Invalid regex", icon::WARNING))
                        .color(theme::BAD),
                )
                .on_hover_text(err);
            } else if !self.query.is_empty() {
                let label = if self.matches.is_empty() {
                    egui::RichText::new("No results").color(theme::BAD)
                } else {
                    let more = if self.matches.len() >= MAX_MATCHES {
                        "+"
                    } else {
                        ""
                    };
                    egui::RichText::new(format!(
                        "{} of {}{more}",
                        self.current + 1,
                        self.matches.len()
                    ))
                    .color(theme::TEXT_MUTED)
                };
                ui.label(label);
            }
            if theme::icon_button(ui, icon::ARROW_UP, "Previous match (⇧↩)").clicked() {
                prev = true;
            }
            if theme::icon_button(ui, icon::ARROW_DOWN, "Next match (↩)").clicked() {
                next = true;
            }
            if theme::icon_button(ui, icon::X, "Close (Esc)").clicked() {
                esc = true;
            }
            if next || prev {
                resp.request_focus();
            }
            self.field_focused = resp.has_focus() || next || prev;
        });
        if esc {
            self.close(ui.ctx(), doc_id);
        } else if next || prev {
            self.step(prev);
        }
    }
}

/// Char ranges of `query` in `text`; an invalid regex finds nothing and reports why.
fn find_matches(
    text: &str,
    query: &str,
    regex: bool,
    case_sensitive: bool,
) -> (Vec<Range<usize>>, Option<String>) {
    if query.is_empty() {
        return (Vec::new(), None);
    }
    let pattern = if regex {
        query.to_owned()
    } else {
        regex::escape(query)
    };
    let re = match regex::RegexBuilder::new(&pattern)
        .case_insensitive(!case_sensitive)
        .size_limit(1 << 20)
        .build()
    {
        Ok(re) => re,
        Err(e) => return (Vec::new(), Some(e.to_string())),
    };
    // Byte offsets to char indices, walking the text once.
    let mut out = Vec::new();
    let mut chars = text.char_indices().enumerate().peekable();
    let mut to_char = |byte: usize| -> usize {
        while let Some(&(ci, (b, _))) = chars.peek() {
            if b >= byte {
                return ci;
            }
            chars.next();
        }
        text.chars().count()
    };
    for m in re
        .find_iter(text)
        .filter(|m| !m.is_empty())
        .take(MAX_MATCHES)
    {
        let start = to_char(m.start());
        let end = to_char(m.end());
        out.push(start..end);
    }
    (out, None)
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
    text.char_indices()
        .nth(char_idx)
        .map(|(b, _)| b)
        .unwrap_or(text.len())
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
    } else if code.ends_with(':')
        || code.ends_with('|')
        || code.ends_with('>')
        || code.ends_with("|-")
        || code.ends_with(">-")
    {
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
    let last_end = text[end..]
        .find('\n')
        .map(|i| end + i)
        .unwrap_or(text.len());
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
    if multi {
        CCursorRange::two(CCursor::new(a), CCursor::new(b))
    } else {
        CCursorRange::one(CCursor::new(a))
    }
}

/// Cmd+/: comment out the selected lines, or uncomment them if they all are.
fn toggle_comment(text: &mut String, range: CCursorRange) -> CCursorRange {
    let sel = range.as_sorted_char_range();
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
    if a == b {
        CCursorRange::one(CCursor::new(a))
    } else {
        CCursorRange::two(CCursor::new(a), CCursor::new(b))
    }
}

// ---------------------------------------------------------------------------
// Syntax colours (a line-based YAML tokenizer; good enough for Kubernetes objects)
// ---------------------------------------------------------------------------

fn highlight(text: &str, font: &FontId) -> LayoutJob {
    let mut job = LayoutJob::default();
    let mut push = |s: &str, color: Color32| {
        if !s.is_empty() {
            job.append(
                s,
                0.0,
                TextFormat {
                    font_id: font.clone(),
                    color,
                    ..Default::default()
                },
            );
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
            while let Some(after) = rest
                .strip_prefix("- ")
                .or_else(|| (rest == "-").then_some(""))
            {
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
        Some(i) if !value[..i].contains('"') && !value[..i].contains('\'') => {
            (&value[..i], &value[i..])
        }
        _ => (value, ""),
    };
    let t = code.trim();
    let lead = &code[..code.len() - code.trim_start().len()];
    push(lead, theme::TEXT);
    let color = if t.starts_with('"') || t.starts_with('\'') {
        theme::SYNTAX_STRING
    } else if matches!(
        t,
        "true" | "false" | "null" | "~" | "True" | "False" | "yes" | "no"
    ) || t.parse::<f64>().is_ok()
    {
        theme::SYNTAX_LITERAL
    } else if matches!(t, "|" | ">" | "|-" | ">-" | "|+" | ">+" | "{}" | "[]")
        || t.starts_with('&')
        || t.starts_with('*')
    {
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
        indent_lines(
            &mut t,
            CCursorRange::two(CCursor::new(4), CCursor::new(18)),
            true,
        );
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
    fn find_text_and_regex() {
        let t = "é: Nginx\nimage: nginx:1.27";
        assert_eq!(find_matches(t, "nginx", false, false).0, vec![3..8, 16..21]);
        assert_eq!(find_matches(t, "nginx", false, true).0, vec![16..21]);
        assert_eq!(find_matches(t, "1.2", false, false).0, vec![22..25]);
        assert_eq!(find_matches(t, r"\d+", true, false).0, vec![22..23, 24..26]);
        assert!(find_matches(t, "(", true, false).1.is_some());
        assert!(find_matches(t, "", false, false).0.is_empty());
    }

    #[test]
    fn guides_bridge_blank_lines() {
        assert_eq!(
            line_indents("a:\n  b:\n\n    c: 1\n  d: 2"),
            vec![0, 2, 2, 4, 2]
        );
    }

    #[test]
    fn key_split_ignores_urls_and_quotes() {
        assert_eq!(key_split("image: nginx:1.27").map(|k| k.0), Some("image"));
        assert_eq!(key_split("url: http://x").map(|k| k.0), Some("url"));
        assert_eq!(key_split("\"a: b\": 1").map(|k| k.0), Some("\"a: b\""));
        assert_eq!(key_split("plain value"), None);
    }
}
