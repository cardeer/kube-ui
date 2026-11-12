//! The YAML editor's pieces on top of iced's `text_editor`: a line-based YAML highlighter
//! (which also tints find matches), editor keys (auto-indent on Enter, Tab / Shift+Tab
//! indent by two spaces, Cmd+/ toggles comments, Cmd+S saves), a line-number gutter,
//! indent guides, and the find bar (Cmd+F).

use std::ops::Range;

use iced::{
    Color, Element, Length, Task,
    keyboard::{Key, key::Named},
    widget::{
        canvas, column, container, operation, rich_text, row, span, text, text_editor, text_input,
    },
};

use super::{Message, icons as icon, theme};

const INDENT: usize = 2;
/// Row pitch of the editor and its gutter, so line numbers line up.
pub const LINE_HEIGHT: f32 = 17.0;
/// JetBrains Mono's advance is 0.6em.
pub const CHAR_WIDTH: f32 = theme::MONO_SIZE * 0.6;
pub const EDITOR_PAD: f32 = 6.0;

// ---------------------------------------------------------------------------
// Positions: the editor speaks (line, byte column); the helpers speak char indices.
// ---------------------------------------------------------------------------

/// Char index of an editor position in `text`.
fn char_index(text: &str, p: text_editor::Position) -> usize {
    let mut chars = 0;
    for (i, line) in text.split('\n').enumerate() {
        if i == p.line {
            let col = p.column.min(line.len());
            let col = (0..=col)
                .rev()
                .find(|c| line.is_char_boundary(*c))
                .unwrap_or(0);
            return chars + line[..col].chars().count();
        }
        chars += line.chars().count() + 1;
    }
    text.chars().count()
}

/// The editor position of a char index in `text`.
fn position(text: &str, char_idx: usize) -> text_editor::Position {
    let mut remaining = char_idx;
    let mut last = 0;
    for (i, line) in text.split('\n').enumerate() {
        let n = line.chars().count();
        if remaining <= n {
            let column = line
                .char_indices()
                .nth(remaining)
                .map(|(b, _)| b)
                .unwrap_or(line.len());
            return text_editor::Position { line: i, column };
        }
        remaining -= n + 1;
        last = i;
    }
    text_editor::Position {
        line: last,
        column: text.split('\n').next_back().map(str::len).unwrap_or(0),
    }
}

/// The cursor's selection as a sorted char range.
pub fn char_range(text: &str, cursor: text_editor::Cursor) -> Range<usize> {
    let a = char_index(text, cursor.position);
    let b = cursor.selection.map(|s| char_index(text, s)).unwrap_or(a);
    a.min(b)..a.max(b)
}

/// A cursor selecting `range` (caret at its end).
pub fn cursor_for(text: &str, range: Range<usize>) -> text_editor::Cursor {
    let end = position(text, range.end);
    text_editor::Cursor {
        position: end,
        selection: (range.start != range.end).then(|| position(text, range.start)),
    }
}

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

/// What Enter inserts after `before` (the line up to the cursor): a newline plus its
/// indentation, one level deeper after a `key:` or inside a `- ` list item.
pub fn newline_insert(before: &str) -> String {
    let indent = before.len() - before.trim_start_matches(' ').len();
    let body = before.trim();
    let code = body.split(" #").next().unwrap_or(body).trim_end();
    let extra = if body.starts_with('#') {
        0
    } else if code.ends_with(':')
        || code.ends_with('|')
        || code.ends_with('>')
        || code.ends_with("|-")
        || code.ends_with(">-")
        || code.starts_with("- ")
        || code == "-"
    {
        INDENT
    } else {
        0
    };
    format!("\n{}", " ".repeat(indent + extra))
}

/// Tab / Shift+Tab: with a selection over several lines, (un)indent them all; otherwise
/// insert spaces up to the next indent stop (or remove up to one level).
pub fn indent_lines(text: &mut String, sel: Range<usize>, outdent: bool) -> Range<usize> {
    let (start, end) = (byte_at(text, sel.start), byte_at(text, sel.end));
    let multi = text[start..end].contains('\n');
    if !multi && !outdent {
        let col = text[line_start(text, start)..start].chars().count();
        let n = INDENT - col % INDENT;
        text.replace_range(start..end, &" ".repeat(n));
        return sel.start + n..sel.start + n;
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
    if multi { a..b } else { a..a }
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

/// The editor's own keys; everything else gets iced's defaults.
pub fn key_binding(press: text_editor::KeyPress) -> Option<text_editor::Binding<Message>> {
    use text_editor::Binding;
    if !matches!(press.status, text_editor::Status::Focused { .. }) {
        return None;
    }
    let m = press.modifiers;
    let latin = press.key.to_latin(press.physical_key);
    match (press.key.as_ref(), latin) {
        (Key::Named(Named::Enter), _) if m.is_empty() => {
            return Some(Binding::Custom(Message::YamlEnter));
        }
        (Key::Named(Named::Tab), _) if !m.command() && !m.alt() => {
            return Some(Binding::Custom(Message::YamlIndent(m.shift())));
        }
        (_, Some('/')) if m.command() => return Some(Binding::Custom(Message::YamlComment)),
        (_, Some('s')) if m.command() => return Some(Binding::Custom(Message::YamlApply)),
        (_, Some('f')) if m.command() => {
            return Some(Binding::Custom(Message::Find(FindMsg::Open)));
        }
        (_, Some('g')) if m.command() => {
            return Some(Binding::Custom(Message::Find(FindMsg::Step(m.shift()))));
        }
        (Key::Named(Named::Escape), _) => {
            return Some(Binding::Custom(Message::Find(FindMsg::Close)));
        }
        _ => {}
    }
    Binding::from_key_press(press)
}

// ---------------------------------------------------------------------------
// Find bar
// ---------------------------------------------------------------------------

/// Matches beyond this are not collected (keeps huge documents responsive).
const MAX_MATCHES: usize = 10_000;
pub const FIND_FIELD: &str = "yaml-find-field";
pub const YAML_SCROLL: &str = "yaml-scroll";

#[derive(Clone, Debug)]
pub enum FindMsg {
    Open,
    Close,
    Query(String),
    ToggleCase,
    ToggleRegex,
    /// Next match, or the previous one when `true`.
    Step(bool),
    /// The document changed: recount.
    Refresh,
}

/// What the highlighter should tint.
#[derive(Clone, PartialEq, Default)]
pub struct Pattern {
    query: String,
    regex: bool,
    case_sensitive: bool,
}

impl Pattern {
    fn compile(&self) -> Result<Option<regex::Regex>, String> {
        if self.query.is_empty() {
            return Ok(None);
        }
        let pattern = if self.regex {
            self.query.clone()
        } else {
            regex::escape(&self.query)
        };
        regex::RegexBuilder::new(&pattern)
            .case_insensitive(!self.case_sensitive)
            .size_limit(1 << 20)
            .build()
            .map(Some)
            .map_err(|e| e.to_string())
    }
}

/// Find-in-document state: the query, its matches (char ranges) and which one is current.
/// One instance serves the YAML window's viewer and editor.
#[derive(Default)]
pub struct Find {
    pub open: bool,
    pattern: Pattern,
    matches: Vec<Range<usize>>,
    current: usize,
    error: Option<String>,
}

impl Find {
    /// The highlighter's settings: matches are tinted while the bar is open.
    pub fn highlight(&self) -> Option<Pattern> {
        (self.open && self.error.is_none() && !self.pattern.query.is_empty())
            .then(|| self.pattern.clone())
    }

    /// Open the bar with a query (development screenshots).
    pub fn preset(&mut self, query: String) {
        self.open = true;
        self.pattern.query = query;
    }

    pub fn update(&mut self, msg: FindMsg, content: &mut text_editor::Content) -> Task<Message> {
        let text = content.text();
        match msg {
            FindMsg::Open => {
                self.open = true;
                // A single-line selection becomes the query.
                if let Some(sel) = content
                    .selection()
                    .filter(|s| !s.is_empty() && !s.contains('\n'))
                {
                    self.pattern.query = if self.pattern.regex {
                        regex::escape(&sel)
                    } else {
                        sel
                    };
                }
                let anchor = char_range(&text, content.cursor()).start;
                self.recompute(&text, Some(anchor));
                return Task::batch([
                    operation::focus(FIND_FIELD),
                    operation::select_all(FIND_FIELD),
                ]);
            }
            FindMsg::Close => {
                self.open = false;
                return operation::focus(super::view::YAML_EDITOR);
            }
            FindMsg::Query(q) => {
                self.pattern.query = q;
                let anchor = char_range(&text, content.cursor()).start;
                self.recompute(&text, Some(anchor));
                return self.reveal(&text, content);
            }
            FindMsg::ToggleCase => {
                self.pattern.case_sensitive = !self.pattern.case_sensitive;
                self.recompute(&text, None);
            }
            FindMsg::ToggleRegex => {
                self.pattern.regex = !self.pattern.regex;
                self.recompute(&text, None);
            }
            FindMsg::Step(back) => {
                if !self.open {
                    return Task::none();
                }
                self.recompute(&text, None);
                let n = self.matches.len();
                if n == 0 {
                    return Task::none();
                }
                self.current = if back {
                    (self.current + n - 1) % n
                } else {
                    (self.current + 1) % n
                };
                return self.reveal(&text, content);
            }
            FindMsg::Refresh => self.recompute(&text, None),
        }
        Task::none()
    }

    /// Recompute matches; `anchor` restarts from the first match at or after it.
    fn recompute(&mut self, text: &str, anchor: Option<usize>) {
        let (matches, error) = find_matches(text, &self.pattern);
        self.matches = matches;
        self.error = error;
        match anchor {
            Some(a) => {
                self.current = self.matches.iter().position(|m| m.start >= a).unwrap_or(0);
            }
            None if self.current >= self.matches.len() => {
                self.current = self.matches.len().saturating_sub(1);
            }
            None => {}
        }
    }

    /// Select the current match and scroll it into view.
    fn reveal(&self, text: &str, content: &mut text_editor::Content) -> Task<Message> {
        let Some(m) = self.matches.get(self.current) else {
            return Task::none();
        };
        let cursor = cursor_for(text, m.clone());
        content.move_to(cursor);
        let y = (cursor.position.line as f32 * LINE_HEIGHT - 160.0).max(0.0);
        let x = (cursor.position.column as f32 * CHAR_WIDTH - 300.0).max(0.0);
        operation::scroll_to(
            YAML_SCROLL,
            operation::AbsoluteOffset {
                x: Some(x),
                y: Some(y),
            },
        )
    }

    /// The bar: query field, case / regex toggles, match count, previous / next, close.
    pub fn bar(&self) -> Element<'_, Message> {
        let hint = if self.pattern.regex {
            "Find (regex)"
        } else {
            "Find"
        };
        let field = text_input(hint, &self.pattern.query)
            .id(FIND_FIELD)
            .on_input(|q| Message::Find(FindMsg::Query(q)))
            .on_submit(Message::Find(FindMsg::Step(false)))
            .icon(text_input::Icon {
                font: theme::ICONS,
                code_point: icon::MAGNIFYING_GLASS.chars().next().unwrap_or(' '),
                size: Some(13.0.into()),
                spacing: 6.0,
                side: text_input::Side::Left,
            })
            .size(theme::BODY)
            .padding([5, 8])
            .width(240)
            .style(theme::input);
        let status: Element<'_, Message> = if self.error.is_some() {
            theme::tip(
                theme::colored(format!("{} Invalid regex", ""), theme::BAD),
                self.error.clone().unwrap_or_default(),
            )
        } else if self.pattern.query.is_empty() {
            theme::space(0.0)
        } else if self.matches.is_empty() {
            theme::colored("No results", theme::BAD).into()
        } else {
            let more = if self.matches.len() >= MAX_MATCHES {
                "+"
            } else {
                ""
            };
            theme::muted(format!(
                "{} of {}{more}",
                self.current + 1,
                self.matches.len()
            ))
            .into()
        };
        row![
            field,
            theme::toggle_button(
                "Aa",
                self.pattern.case_sensitive,
                "Match case",
                Message::Find(FindMsg::ToggleCase)
            ),
            theme::toggle_button(
                ".*",
                self.pattern.regex,
                "Regular expression",
                Message::Find(FindMsg::ToggleRegex)
            ),
            status,
            theme::icon_button(
                icon::ARROW_UP,
                "Previous match (⇧⌘G)",
                Some(Message::Find(FindMsg::Step(true)))
            ),
            theme::icon_button(
                icon::ARROW_DOWN,
                "Next match (↩)",
                Some(Message::Find(FindMsg::Step(false)))
            ),
            theme::icon_button(icon::X, "Close (Esc)", Some(Message::Find(FindMsg::Close))),
        ]
        .spacing(4)
        .align_y(iced::Center)
        .into()
    }
}

/// Char ranges of the pattern in `text`; an invalid regex finds nothing and reports why.
fn find_matches(text: &str, pattern: &Pattern) -> (Vec<Range<usize>>, Option<String>) {
    let re = match pattern.compile() {
        Ok(Some(re)) => re,
        Ok(None) => return (Vec::new(), None),
        Err(e) => return (Vec::new(), Some(e)),
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

// ---------------------------------------------------------------------------
// Gutter
// ---------------------------------------------------------------------------

/// Line numbers beside the editor: the cursor's line in Ink, an error line in red.
pub fn gutter<'a>(
    lines: usize,
    cursor_line: Option<usize>,
    error_line: Option<usize>,
) -> Element<'a, Message> {
    let digits = lines.max(1).to_string().len().max(2);
    let spans: Vec<text::Span<'_, ()>> = (0..lines.max(1))
        .map(|i| {
            let color = if error_line == Some(i + 1) {
                theme::BAD
            } else if cursor_line == Some(i) {
                theme::TEXT
            } else {
                theme::TEXT_FAINT
            };
            span(format!("{:>digits$}\n", i + 1)).color(color)
        })
        .collect();
    container(
        rich_text(spans)
            .font(theme::MONO)
            .size(theme::MONO_SIZE)
            .line_height(text::LineHeight::Absolute(LINE_HEIGHT.into()))
            .wrapping(text::Wrapping::None),
    )
    .padding([EDITOR_PAD, 14.0])
    .height(Length::Shrink)
    .style(theme::well)
    .into()
}

/// A full-width red band behind an error message.
pub fn error_line<'a>(line: usize, msg: &'a str) -> Element<'a, Message> {
    column![theme::colored(format!("Line {line}: {msg}"), theme::BAD)].into()
}

// ---------------------------------------------------------------------------
// Indent guides
// ---------------------------------------------------------------------------

/// Leading-space depth of each line. Blank lines take the shallower of their nearest
/// non-blank neighbours, so guides run through gaps inside a block but not past its end.
pub fn indent_depths<S: AsRef<str>>(lines: impl IntoIterator<Item = S>) -> Vec<usize> {
    let raw: Vec<Option<usize>> = lines
        .into_iter()
        .map(|l| {
            let l = l.as_ref();
            let depth = l.len() - l.trim_start_matches(' ').len();
            (!l.trim().is_empty()).then_some(depth)
        })
        .collect();
    let mut next = vec![0; raw.len()];
    let mut below = 0;
    for (i, d) in raw.iter().enumerate().rev() {
        next[i] = below;
        if let Some(d) = d {
            below = *d;
        }
    }
    let mut above = 0;
    raw.iter()
        .enumerate()
        .map(|(i, d)| match d {
            Some(d) => {
                above = *d;
                *d
            }
            None => above.min(next[i]),
        })
        .collect()
}

/// Vertical lines at each indent level, drawn on a canvas behind the (transparent) editor.
/// The innermost guide around the cursor is drawn brighter.
pub struct IndentGuides {
    pub depths: Vec<usize>,
    pub cursor_line: Option<usize>,
}

impl<M> canvas::Program<M> for IndentGuides {
    type State = ();

    fn draw(
        &self,
        _state: &(),
        renderer: &iced::Renderer,
        _theme: &iced::Theme,
        bounds: iced::Rectangle,
        _cursor: iced::mouse::Cursor,
    ) -> Vec<canvas::Geometry> {
        let mut frame = canvas::Frame::new(renderer, bounds.size());
        let levels = self
            .depths
            .iter()
            .max()
            .copied()
            .unwrap_or(0)
            .div_ceil(INDENT);
        let active = self.cursor_line.and_then(|line| {
            let depth = *self.depths.get(line)?;
            (depth > 0).then(|| ((depth - 1) / INDENT, line))
        });
        let n = self.depths.len();
        for level in 0..levels {
            let col = level * INDENT;
            let x = (EDITOR_PAD + col as f32 * CHAR_WIDTH).round() + 0.5;
            let mut start = None;
            for i in 0..=n {
                let inside = i < n && self.depths[i] > col;
                match (inside, start) {
                    (true, None) => start = Some(i),
                    (false, Some(s)) => {
                        let lit =
                            matches!(active, Some((l, c)) if l == level && (s..i).contains(&c));
                        let color = if lit {
                            theme::alpha(theme::TEXT_FAINT, 0.7)
                        } else {
                            theme::BORDER_STRONG
                        };
                        let top = EDITOR_PAD + s as f32 * LINE_HEIGHT;
                        let bottom = EDITOR_PAD + i as f32 * LINE_HEIGHT;
                        frame.stroke(
                            &canvas::Path::line([x, top].into(), [x, bottom].into()),
                            canvas::Stroke::default().with_color(color).with_width(1.0),
                        );
                        start = None;
                    }
                    _ => {}
                }
            }
        }
        vec![frame.into_geometry()]
    }
}

// ---------------------------------------------------------------------------
// Syntax colours (a line-based YAML tokenizer; good enough for Kubernetes objects)
// ---------------------------------------------------------------------------

pub struct Highlighter {
    /// Inside a `|` / `>` block scalar at the start of each line: its key's indent.
    states: Vec<Option<usize>>,
    current: usize,
    find: Option<regex::Regex>,
}

impl iced::advanced::text::Highlighter for Highlighter {
    type Settings = Option<Pattern>;
    type Highlight = Color;
    type Iterator<'a> = std::vec::IntoIter<(Range<usize>, Color)>;

    fn new(settings: &Self::Settings) -> Self {
        Self {
            states: vec![None],
            current: 0,
            find: settings.as_ref().and_then(|p| p.compile().ok().flatten()),
        }
    }

    fn update(&mut self, settings: &Self::Settings) {
        self.find = settings.as_ref().and_then(|p| p.compile().ok().flatten());
        self.states.truncate(1);
        self.current = 0;
    }

    fn change_line(&mut self, line: usize) {
        self.current = line.min(self.states.len() - 1);
        self.states.truncate(self.current + 1);
    }

    fn highlight_line(&mut self, line: &str) -> Self::Iterator<'_> {
        let block = self.states.get(self.current).copied().flatten();
        let (mut spans, next) = highlight_line(line, block);
        if let Some(re) = &self.find {
            for m in re.find_iter(line).filter(|m| !m.is_empty()) {
                spans.push((m.range(), theme::WARN));
            }
        }
        self.current += 1;
        if self.states.len() <= self.current {
            self.states.push(next);
        } else {
            self.states[self.current] = next;
        }
        spans.into_iter()
    }

    fn current_line(&self) -> usize {
        self.current
    }
}

pub fn to_format(
    color: &Color,
    _theme: &iced::Theme,
) -> iced::advanced::text::highlighter::Format<iced::Font> {
    iced::advanced::text::highlighter::Format {
        color: Some(*color),
        font: None,
    }
}

/// Coloured byte ranges of one line, and the block-scalar state for the next one.
fn highlight_line(line: &str, block: Option<usize>) -> (Vec<(Range<usize>, Color)>, Option<usize>) {
    let mut out = Vec::new();
    let mut push = |start: usize, end: usize, color: Color| {
        if end > start {
            out.push((start..end, color));
        }
    };
    let indent = line.len() - line.trim_start_matches(' ').len();
    if let Some(b) = block {
        if line[indent..].is_empty() || indent > b {
            push(indent, line.len(), theme::SYNTAX_STRING);
            return (out, Some(b));
        }
    }
    let mut next = None;
    let mut pos = indent;
    let mut rest = &line[indent..];
    if rest.starts_with('#') {
        push(pos, line.len(), theme::SYNTAX_COMMENT);
    } else if rest == "---" || rest == "..." {
        push(pos, line.len(), theme::TEXT_FAINT);
    } else {
        while let Some(after) = rest
            .strip_prefix("- ")
            .or_else(|| (rest == "-").then_some(""))
        {
            let n = rest.len() - after.len();
            push(pos, pos + n, theme::TEXT_MUTED);
            pos += n;
            rest = after;
        }
        match key_split(rest) {
            Some((key, value)) => {
                push(pos, pos + key.len(), theme::SYNTAX_KEY);
                pos += key.len();
                push(pos, pos + 1, theme::TEXT_MUTED);
                pos += 1;
                let value = &value[1..];
                if matches!(value.trim(), "|" | ">" | "|-" | ">-" | "|+" | ">+") {
                    next = Some(indent);
                }
                push_value(&mut push, pos, value);
            }
            None => push_value(&mut push, pos, rest),
        }
    }
    (out, next)
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

fn push_value(push: &mut impl FnMut(usize, usize, Color), at: usize, value: &str) {
    // Trailing comment.
    let (code, comment) = match value.find(" #") {
        Some(i) if !value[..i].contains('"') && !value[..i].contains('\'') => {
            (&value[..i], &value[i..])
        }
        _ => (value, ""),
    };
    let t = code.trim();
    let lead = code.len() - code.trim_start().len();
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
    push(at + lead, at + code.len(), color);
    push(at + code.len(), at + value.len(), theme::SYNTAX_COMMENT);
    let _ = comment;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn indent_depths_bridge_blank_lines() {
        let d = indent_depths("a:\n  b:\n    c: 1\n\n    d: 2\n\ne: 3\n".split('\n'));
        assert_eq!(d, vec![0, 2, 4, 4, 4, 0, 0, 0]);
    }

    #[test]
    fn enter_keeps_and_deepens_indent() {
        assert_eq!(newline_insert("  containers:"), "\n    ");
        assert_eq!(newline_insert("  - name: app"), "\n    ");
        assert_eq!(newline_insert("    image: nginx"), "\n    ");
        assert_eq!(newline_insert("  # note:"), "\n  ");
    }

    #[test]
    fn tab_and_shift_tab() {
        let mut t = String::from("a:\nb");
        let r = indent_lines(&mut t, 3..3, false);
        assert_eq!(t, "a:\n  b");
        assert_eq!(r, 5..5);

        let mut t = String::from("a:\n    b: 1\n    c: 2");
        indent_lines(&mut t, 4..18, true);
        assert_eq!(t, "a:\n  b: 1\n  c: 2");
    }

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
    fn find_text_and_regex() {
        let t = "é: Nginx\nimage: nginx:1.27";
        let p = |q: &str, regex, case_sensitive| Pattern {
            query: q.into(),
            regex,
            case_sensitive,
        };
        assert_eq!(
            find_matches(t, &p("nginx", false, false)).0,
            vec![3..8, 16..21]
        );
        assert_eq!(find_matches(t, &p("nginx", false, true)).0, vec![16..21]);
        assert_eq!(find_matches(t, &p("1.2", false, false)).0, vec![22..25]);
        assert_eq!(
            find_matches(t, &p(r"\d+", true, false)).0,
            vec![22..23, 24..26]
        );
        assert!(find_matches(t, &p("(", true, false)).1.is_some());
        assert!(find_matches(t, &p("", false, false)).0.is_empty());
    }

    #[test]
    fn positions_round_trip() {
        let t = "é: a\nbc\n\nd";
        for i in 0..=t.chars().count() {
            assert_eq!(char_index(t, position(t, i)), i);
        }
        let c = cursor_for(t, 2..6);
        assert_eq!(char_range(t, c), 2..6);
    }

    #[test]
    fn highlights_keys_and_block_scalars() {
        let (spans, next) = highlight_line("  script: |", None);
        assert_eq!(spans[0], (2..8, theme::SYNTAX_KEY));
        assert_eq!(next, Some(2));
        let (spans, next) = highlight_line("    echo hi", Some(2));
        assert_eq!(spans, vec![(4..11, theme::SYNTAX_STRING)]);
        assert_eq!(next, Some(2));
        let (_, next) = highlight_line("  other: 1", Some(2));
        assert_eq!(next, None);
    }

    #[test]
    fn key_split_ignores_urls_and_quotes() {
        assert_eq!(key_split("image: nginx:1.27").map(|k| k.0), Some("image"));
        assert_eq!(key_split("url: http://x").map(|k| k.0), Some("url"));
        assert_eq!(key_split("\"a: b\": 1").map(|k| k.0), Some("\"a: b\""));
        assert_eq!(key_split("plain value"), None);
    }
}
