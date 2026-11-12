//! Shell tabs: an interactive terminal into a pod's container (`kubectl exec -it`), drawn
//! from a vt100 screen model on a canvas and living in the bottom dock next to the log tabs.

use std::num::NonZeroU16;

use iced::{
    Element, Length, Point, Rectangle, Renderer, Size, Task, Theme,
    keyboard::{self, Key, key::Named},
    mouse,
    widget::{Action as CanvasAction, canvas, column, container, row},
};
use tokio::sync::mpsc::{UnboundedSender, unbounded_channel};

use super::*;
use crate::kube_ops::ShellInput;

const SCROLLBACK: usize = 5_000;
/// JetBrains Mono's advance is 0.6em.
const CHAR_W: f32 = theme::MONO_SIZE * 0.6;
const LINE_H: f32 = 16.0;
const PAD_X: f32 = 8.0;
const PAD_Y: f32 = 6.0;

#[derive(Clone, PartialEq)]
pub(super) enum ShellStatus {
    Connecting,
    Open,
    /// The shell ended; `Some` holds the reason when it wasn't a clean exit.
    Exited(Option<String>),
}

/// What the user did in a shell tab.
#[derive(Clone, Debug)]
pub enum ShellMsg {
    Input(Vec<u8>),
    Resize(u16, u16),
    /// Mouse selection in screen cells: (row, col) anchor and head.
    Select(Option<((u16, u16), (u16, u16))>),
    /// Scroll back through history by this many lines (negative: toward the live screen).
    Scroll(isize),
    Paste,
    Pasted(Option<String>),
    CopySelection,
    CopyScreen,
    Reconnect,
}

pub(super) struct ShellView {
    pub(super) id: u64,
    pub(super) context: String,
    pub(super) namespace: String,
    pub(super) pod: String,
    pub(super) container: Option<String>,
    client: Client,
    parser: vt100::Parser,
    tx: Option<UnboundedSender<ShellInput>>,
    task: Option<JoinHandle<()>>,
    pub(super) status: ShellStatus,
    /// (cols, rows) last sent to the pod.
    size: (u16, u16),
    selection: Option<((u16, u16), (u16, u16))>,
}

impl Drop for ShellView {
    fn drop(&mut self) {
        if let Some(t) = self.task.take() {
            t.abort();
        }
    }
}

fn nz(v: u16) -> NonZeroU16 {
    NonZeroU16::new(v.max(1)).unwrap_or(NonZeroU16::MIN)
}

impl ShellView {
    pub(super) fn new(
        id: u64,
        context: String,
        client: Client,
        namespace: String,
        pod: String,
        container: Option<String>,
    ) -> Self {
        Self {
            id,
            context,
            namespace,
            pod,
            container,
            client,
            parser: vt100::Parser::new(nz(24), nz(80), SCROLLBACK),
            tx: None,
            task: None,
            status: ShellStatus::Connecting,
            size: (80, 24),
            selection: None,
        }
    }

    pub(super) fn title(&self) -> String {
        match &self.container {
            Some(c) => format!("{} · {c}", self.pod),
            None => self.pod.clone(),
        }
    }

    pub(super) fn hover(&self) -> String {
        format!(
            "Shell in {}/{} ({})",
            self.namespace,
            self.title(),
            self.context
        )
    }

    pub(super) fn live(&self) -> bool {
        !matches!(self.status, ShellStatus::Exited(_))
    }

    /// (Re)connects. Output and the exit arrive on the bus under `self.id`.
    pub(super) fn start(&mut self, rt: &Handle, bus: &Bus) {
        if let Some(t) = self.task.take() {
            t.abort();
        }
        let (tx, rx) = unbounded_channel();
        self.tx = Some(tx);
        self.status = ShellStatus::Connecting;
        let (id, client, ns, pod, container, size) = (
            self.id,
            self.client.clone(),
            self.namespace.clone(),
            self.pod.clone(),
            self.container.clone(),
            self.size,
        );
        let bus = bus.clone();
        self.task = Some(rt.spawn(async move {
            let out_bus = bus.clone();
            let result = kube_ops::run_shell(client, ns, pod, container, size, rx, move |data| {
                out_bus.send(Msg::ShellOutput { id, data })
            })
            .await;
            let error = match result {
                Ok(e) => e,
                Err(e) => Some(err_string(e)),
            };
            bus.send(Msg::ShellExited { id, error });
        }));
    }

    pub(super) fn handle(&mut self, msg: Msg) {
        match msg {
            Msg::ShellOutput { data, .. } => {
                if self.status == ShellStatus::Connecting {
                    self.status = ShellStatus::Open;
                }
                self.parser.process(&data);
            }
            Msg::ShellExited { error, .. } => {
                self.status = ShellStatus::Exited(error);
                self.tx = None;
            }
            _ => {}
        }
    }

    /// Types text into the shell (dev screenshot).
    pub(super) fn type_text(&mut self, text: &str) {
        self.send(text.as_bytes().to_vec());
    }

    fn send(&mut self, bytes: impl Into<Vec<u8>>) {
        if let Some(tx) = &self.tx {
            let _ = tx.send(ShellInput::Data(bytes.into()));
        }
    }

    pub(super) fn update(&mut self, msg: ShellMsg, rt: &Handle, bus: &Bus) -> Task<Message> {
        match msg {
            ShellMsg::Input(bytes) => {
                if matches!(self.status, ShellStatus::Exited(_)) {
                    // Enter on an ended session reconnects.
                    if bytes == b"\r" {
                        self.start(rt, bus);
                    }
                    return Task::none();
                }
                self.send(bytes);
                // Typing jumps back to the live screen and drops any selection.
                self.parser.screen_mut().set_scrollback(0);
                self.selection = None;
            }
            ShellMsg::Resize(cols, rows) => {
                if (cols, rows) != self.size {
                    self.size = (cols, rows);
                    self.parser.set_size(nz(rows), nz(cols));
                    if let Some(tx) = &self.tx {
                        let _ = tx.send(ShellInput::Resize(cols, rows));
                    }
                }
            }
            ShellMsg::Select(sel) => self.selection = sel,
            ShellMsg::Scroll(lines) => {
                if !self.parser.screen().alternate_screen() {
                    let now = self.parser.screen().scrollback() as isize;
                    self.parser
                        .screen_mut()
                        .set_scrollback((now + lines).max(0) as usize);
                }
            }
            ShellMsg::Paste => {
                let id = self.id;
                return iced::clipboard::read()
                    .map(move |t| Message::Shell(id, ShellMsg::Pasted(t)));
            }
            ShellMsg::Pasted(Some(t)) => {
                let t = t.replace("\r\n", "\r").replace('\n', "\r");
                let data = if self.parser.screen().bracketed_paste() {
                    format!("\x1b[200~{t}\x1b[201~")
                } else {
                    t
                };
                return self.update(ShellMsg::Input(data.into_bytes()), rt, bus);
            }
            ShellMsg::Pasted(None) => {}
            ShellMsg::CopySelection => {
                if let Some(text) = self.selected_text() {
                    return iced::clipboard::write(text);
                }
            }
            ShellMsg::CopyScreen => {
                return iced::clipboard::write(self.parser.screen().contents());
            }
            ShellMsg::Reconnect => self.start(rt, bus),
        }
        Task::none()
    }

    fn selected_text(&self) -> Option<String> {
        let (a, b) = self.selection?;
        let (start, end) = if a <= b { (a, b) } else { (b, a) };
        if start == end {
            return None;
        }
        Some(
            self.parser
                .screen()
                .contents_between(start.0, start.1, end.0, end.1 + 1),
        )
    }

    /// Status strip and terminal.
    pub(super) fn view(&self) -> Element<'_, Message> {
        let (color, status) = match &self.status {
            ShellStatus::Connecting => (YELLOW, "Connecting…".to_string()),
            ShellStatus::Open => (
                GREEN,
                format!("Connected · {}/{}", self.namespace, self.title()),
            ),
            ShellStatus::Exited(None) => (theme::IDLE, "Session ended".to_string()),
            ShellStatus::Exited(Some(e)) => (RED, format!("Session ended: {e}")),
        };
        let mut strip = row![
            theme::dot(color),
            theme::muted(status).wrapping(iced::widget::text::Wrapping::None),
            theme::fill_space(),
        ]
        .spacing(6)
        .align_y(iced::Center);
        if matches!(self.status, ShellStatus::Exited(_)) {
            strip = strip.push(theme::primary_button(
                Some(icon::ARROW_CLOCKWISE),
                "Reconnect",
                Some(Message::Shell(self.id, ShellMsg::Reconnect)),
            ));
        }
        strip = strip.push(theme::icon_button(
            icon::COPY,
            "Copy the visible screen",
            Some(Message::Shell(self.id, ShellMsg::CopyScreen)),
        ));
        column![
            container(strip).height(28).center_y(28),
            canvas(Terminal { view: self })
                .width(Length::Fill)
                .height(Length::Fill),
        ]
        .spacing(4)
        .height(Length::Fill)
        .into()
    }
}

/// Canvas program that draws the screen and turns input into shell bytes.
struct Terminal<'a> {
    view: &'a ShellView,
}

#[derive(Default)]
struct TermState {
    focused: bool,
    /// Dragging out a selection from this cell.
    anchor: Option<(u16, u16)>,
}

impl Terminal<'_> {
    fn grid(bounds: Rectangle) -> (u16, u16) {
        let cols = (((bounds.width - 2.0 * PAD_X) / CHAR_W).floor() as u16).clamp(10, 1000);
        let rows = (((bounds.height - 2.0 * PAD_Y) / LINE_H).floor() as u16).clamp(2, 500);
        (cols, rows)
    }

    fn cell_at(&self, bounds: Rectangle, p: Point) -> (u16, u16) {
        let (cols, rows) = self.view.size;
        let col = ((p.x - bounds.x - PAD_X) / CHAR_W)
            .floor()
            .clamp(0.0, cols as f32 - 1.0) as u16;
        let row = ((p.y - bounds.y - PAD_Y) / LINE_H)
            .floor()
            .clamp(0.0, rows as f32 - 1.0) as u16;
        (row, col)
    }

    fn msg(&self, m: ShellMsg) -> CanvasAction<Message> {
        CanvasAction::publish(Message::Shell(self.view.id, m))
    }
}

impl canvas::Program<Message> for Terminal<'_> {
    type State = TermState;

    fn update(
        &self,
        state: &mut TermState,
        event: &iced::Event,
        bounds: Rectangle,
        cursor: mouse::Cursor,
    ) -> Option<CanvasAction<Message>> {
        // Keep the pod's terminal size matched to the canvas.
        let grid = Self::grid(bounds);
        if grid != self.view.size {
            return Some(self.msg(ShellMsg::Resize(grid.0, grid.1)));
        }
        match event {
            iced::Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left)) => {
                match cursor.position_in(bounds).map(|_| cursor.position()) {
                    Some(Some(p)) => {
                        state.focused = true;
                        let c = self.cell_at(bounds, p);
                        state.anchor = Some(c);
                        Some(self.msg(ShellMsg::Select(None)).and_capture())
                    }
                    _ => {
                        state.focused = false;
                        None
                    }
                }
            }
            iced::Event::Mouse(mouse::Event::CursorMoved { position }) => {
                let anchor = state.anchor?;
                let head = self.cell_at(bounds, *position);
                Some(self.msg(ShellMsg::Select(Some((anchor, head)))))
            }
            iced::Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left)) => {
                state.anchor = None;
                None
            }
            iced::Event::Mouse(mouse::Event::WheelScrolled { delta }) => {
                cursor.position_in(bounds)?;
                let lines = match delta {
                    mouse::ScrollDelta::Lines { y, .. } => (y * 3.0).round(),
                    mouse::ScrollDelta::Pixels { y, .. } => (y / LINE_H).round(),
                } as isize;
                (lines != 0).then(|| self.msg(ShellMsg::Scroll(lines)).and_capture())
            }
            iced::Event::Keyboard(keyboard::Event::KeyPressed {
                key,
                physical_key,
                modifiers,
                text,
                ..
            }) if state.focused => {
                let latin = key.to_latin(*physical_key);
                if modifiers.command() && !modifiers.control() {
                    return match latin {
                        Some('c') => Some(self.msg(ShellMsg::CopySelection).and_capture()),
                        Some('v') => Some(self.msg(ShellMsg::Paste).and_capture()),
                        // Other Cmd shortcuts stay with the app.
                        _ => None,
                    };
                }
                let app_cursor = self.view.parser.screen().application_cursor();
                let bytes = key_bytes(key, latin, *modifiers, app_cursor).or_else(|| {
                    text.as_ref()
                        .filter(|t| t.chars().any(|c| !c.is_control()))
                        .map(|t| t.as_bytes().to_vec())
                })?;
                Some(self.msg(ShellMsg::Input(bytes)).and_capture())
            }
            _ => None,
        }
    }

    fn draw(
        &self,
        state: &TermState,
        renderer: &Renderer,
        _theme: &Theme,
        bounds: Rectangle,
        _cursor: mouse::Cursor,
    ) -> Vec<canvas::Geometry> {
        let mut frame = canvas::Frame::new(renderer, bounds.size());
        frame.fill_rectangle(Point::ORIGIN, bounds.size(), theme::WELL);
        let screen = self.view.parser.screen();
        let (cols, rows) = self.view.size;
        let sel = self
            .view
            .selection
            .map(|(a, b)| if a <= b { (a, b) } else { (b, a) });
        let selected = |r: u16, c: u16| match sel {
            Some((s, e)) if s != e => (r, c) >= s && (r, c) <= e,
            _ => false,
        };
        let text_at = |frame: &mut canvas::Frame, s: &str, x: f32, y: f32, color: Color| {
            if s.trim().is_empty() {
                return;
            }
            frame.fill_text(canvas::Text {
                content: s.to_string(),
                position: Point::new(x, y),
                color,
                size: theme::MONO_SIZE.into(),
                line_height: iced::widget::text::LineHeight::Absolute(LINE_H.into()),
                font: theme::MONO,
                ..Default::default()
            });
        };

        for r in 0..rows {
            let y = PAD_Y + r as f32 * LINE_H;
            // Runs of cells with the same colour share one text draw.
            let mut run = String::new();
            let mut run_x = PAD_X;
            let mut run_color = theme::TEXT;
            let mut c = 0;
            while c < cols {
                let Some(cell) = screen.cell(r, c) else { break };
                if cell.is_wide_continuation() {
                    c += 1;
                    continue;
                }
                let (mut fg, mut bg) = (
                    term_color(cell.fgcolor(), true, cell.bold()),
                    term_color(cell.bgcolor(), false, false),
                );
                if cell.inverse() {
                    std::mem::swap(&mut fg, &mut bg);
                    // Default colours have no swatch to swap: use the well and ink explicitly.
                    if bg == Color::TRANSPARENT {
                        bg = theme::TEXT;
                    }
                    if fg == Color::TRANSPARENT {
                        fg = theme::WELL;
                    }
                }
                if selected(r, c) {
                    bg = theme::ACCENT_BG;
                    fg = theme::ACCENT_TEXT;
                }
                if cell.dim() {
                    fg = fg.scale_alpha(0.6);
                }
                let wide = cell.is_wide();
                let x = PAD_X + c as f32 * CHAR_W;
                let w = if wide { 2.0 } else { 1.0 };
                if bg != Color::TRANSPARENT {
                    frame.fill_rectangle(Point::new(x, y), Size::new(CHAR_W * w, LINE_H), bg);
                }
                if cell.underline() {
                    frame.fill_rectangle(
                        Point::new(x, y + LINE_H - 2.0),
                        Size::new(CHAR_W * w, 1.0),
                        fg,
                    );
                }
                let content = if cell.has_contents() {
                    cell.contents()
                } else {
                    " "
                };
                if fg != run_color || wide {
                    text_at(&mut frame, &run, run_x, y, run_color);
                    run.clear();
                    run_x = x;
                    run_color = fg;
                }
                run.push_str(content);
                if wide {
                    // Wide glyphs are drawn alone so the columns after them stay aligned.
                    text_at(&mut frame, &run, run_x, y, run_color);
                    run.clear();
                    run_x = x + CHAR_W * 2.0;
                }
                c += if wide { 2 } else { 1 };
            }
            text_at(&mut frame, &run, run_x, y, run_color);
        }

        // Cursor: a block when focused, an outline otherwise; hidden while scrolled back.
        if !screen.hide_cursor()
            && screen.scrollback() == 0
            && self.view.status != ShellStatus::Connecting
        {
            let (r, c) = screen.cursor_position();
            let at = Point::new(PAD_X + c as f32 * CHAR_W, PAD_Y + r as f32 * LINE_H);
            let size = Size::new(CHAR_W, LINE_H);
            if state.focused {
                frame.fill_rectangle(at, size, theme::ACCENT.scale_alpha(0.75));
            } else {
                frame.stroke_rectangle(
                    at,
                    size,
                    canvas::Stroke::default()
                        .with_width(1.0)
                        .with_color(theme::TEXT_MUTED),
                );
            }
        }
        if screen.scrollback() > 0 {
            frame.fill_text(canvas::Text {
                content: format!("↑ {} lines back · type to return", screen.scrollback()),
                position: Point::new(bounds.width - 10.0, 8.0),
                color: theme::TEXT_FAINT,
                size: 12.0.into(),
                font: theme::INTER,
                align_x: iced::widget::text::Alignment::Right,
                ..Default::default()
            });
        }
        vec![frame.into_geometry()]
    }

    fn mouse_interaction(
        &self,
        _state: &TermState,
        bounds: Rectangle,
        cursor: mouse::Cursor,
    ) -> mouse::Interaction {
        if cursor.is_over(bounds) {
            mouse::Interaction::Text
        } else {
            mouse::Interaction::default()
        }
    }
}

/// Bytes a key sends to the shell (xterm conventions). Plain characters come from the
/// key's text instead.
fn key_bytes(
    key: &Key,
    latin: Option<char>,
    m: keyboard::Modifiers,
    app_cursor: bool,
) -> Option<Vec<u8>> {
    let arrow = |c: u8| -> Vec<u8> {
        if m.alt() {
            // Alt+Left/Right: word movement in readline.
            match c {
                b'D' => b"\x1bb".to_vec(),
                b'C' => b"\x1bf".to_vec(),
                _ => vec![0x1b, b'[', c],
            }
        } else if app_cursor {
            vec![0x1b, b'O', c]
        } else {
            vec![0x1b, b'[', c]
        }
    };
    // Ctrl+letter → control code (Ctrl+C = 0x03 ...). On macOS, Cmd stays with the app.
    if m.control() {
        match latin {
            Some(ch) if ch.is_ascii_alphabetic() => {
                return Some(vec![ch.to_ascii_lowercase() as u8 & 0x1f]);
            }
            Some('[') => return Some(vec![0x1b]),
            Some('\\') => return Some(vec![0x1c]),
            Some(']') => return Some(vec![0x1d]),
            _ => {}
        }
    }
    let Key::Named(named) = key else {
        return None;
    };
    Some(match named {
        Named::Enter => b"\r".to_vec(),
        Named::Backspace if m.alt() => b"\x1b\x7f".to_vec(),
        Named::Backspace => vec![0x7f],
        Named::Tab if m.shift() => b"\x1b[Z".to_vec(),
        Named::Tab => b"\t".to_vec(),
        Named::Escape => vec![0x1b],
        Named::ArrowUp => arrow(b'A'),
        Named::ArrowDown => arrow(b'B'),
        Named::ArrowRight => arrow(b'C'),
        Named::ArrowLeft => arrow(b'D'),
        Named::Home => b"\x1b[H".to_vec(),
        Named::End => b"\x1b[F".to_vec(),
        Named::PageUp => b"\x1b[5~".to_vec(),
        Named::PageDown => b"\x1b[6~".to_vec(),
        Named::Delete => b"\x1b[3~".to_vec(),
        Named::Insert => b"\x1b[2~".to_vec(),
        Named::F1 => b"\x1bOP".to_vec(),
        Named::F2 => b"\x1bOQ".to_vec(),
        Named::F3 => b"\x1bOR".to_vec(),
        Named::F4 => b"\x1bOS".to_vec(),
        Named::Space => b" ".to_vec(),
        _ => return None,
    })
}

/// xterm colours mapped onto the app palette. Default background is transparent (the well).
pub(super) fn term_color(c: vt100::Color, fg: bool, bold: bool) -> Color {
    const ANSI: [Color; 16] = [
        Color::from_rgb8(0x2e, 0x34, 0x41), // black
        theme::BAD,
        theme::OK,
        theme::WARN,
        theme::ACCENT,
        Color::from_rgb8(0xc0, 0x8b, 0xf0), // magenta
        Color::from_rgb8(0x5f, 0xc8, 0xd4), // cyan
        theme::TEXT_MUTED,                  // white
        theme::IDLE,                        // bright black
        Color::from_rgb8(0xff, 0x7b, 0x7f),
        Color::from_rgb8(0x6e, 0xe6, 0xaa),
        Color::from_rgb8(0xff, 0xc5, 0x5c),
        theme::ACCENT_HOVER,
        Color::from_rgb8(0xd6, 0xb0, 0xff),
        Color::from_rgb8(0x8a, 0xe3, 0xec),
        theme::TEXT,
    ];
    match c {
        vt100::Color::Default => {
            if fg {
                theme::TEXT
            } else {
                Color::TRANSPARENT
            }
        }
        vt100::Color::Idx(i) if i < 16 => {
            // Bold brightens the eight base colours, as most terminals do.
            let i = if fg && bold && i < 8 { i + 8 } else { i };
            ANSI[i as usize]
        }
        vt100::Color::Idx(i) if i < 232 => {
            let i = i - 16;
            let level = |v: u8| if v == 0 { 0 } else { 55 + v * 40 };
            Color::from_rgb8(level(i / 36), level((i / 6) % 6), level(i % 6))
        }
        vt100::Color::Idx(i) => {
            let v = 8 + (i - 232) * 10;
            Color::from_rgb8(v, v, v)
        }
        vt100::Color::Rgb(r, g, b) => Color::from_rgb8(r, g, b),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn control_keys() {
        let ctrl = keyboard::Modifiers::CTRL;
        let none = keyboard::Modifiers::empty();
        let c = Key::Character("c".into());
        assert_eq!(key_bytes(&c, Some('c'), ctrl, false), Some(vec![3]));
        assert_eq!(key_bytes(&c, Some('d'), ctrl, false), Some(vec![4]));
        let enter = Key::Named(Named::Enter);
        assert_eq!(key_bytes(&enter, None, none, false), Some(b"\r".to_vec()));
        let up = Key::Named(Named::ArrowUp);
        assert_eq!(key_bytes(&up, None, none, false), Some(b"\x1b[A".to_vec()));
        assert_eq!(key_bytes(&up, None, none, true), Some(b"\x1bOA".to_vec()));
        // Plain letters come through the key's text, not here.
        assert_eq!(key_bytes(&c, Some('c'), none, false), None);
    }

    #[test]
    fn colours() {
        assert_eq!(
            term_color(vt100::Color::Default, false, false),
            Color::TRANSPARENT
        );
        assert_eq!(
            term_color(vt100::Color::Idx(1), true, true),
            term_color(vt100::Color::Idx(9), true, false)
        );
        assert_eq!(
            term_color(vt100::Color::Rgb(1, 2, 3), true, false),
            Color::from_rgb8(1, 2, 3)
        );
    }
}
