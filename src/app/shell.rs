//! Shell tabs: an interactive terminal into a pod's container (`kubectl exec -it`), drawn
//! from a vt100 screen model and living in the bottom dock next to the log tabs.
//!
//! PORTING STUB (gpui-kit branch): the terminal canvas, key mapping and the view were
//! removed; see `git show main:src/app/shell.rs`. The terminal part re-adds them on GPUI.

use std::num::NonZeroU16;

use std::{cell::Cell, rc::Rc};

use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, BorderStyle, Bounds, DispatchPhase, FocusHandle, InteractiveElement as _,
    IntoElement as _, KeyBinding, Modifiers, MouseButton, MouseDownEvent, MouseMoveEvent,
    MouseUpEvent, ParentElement as _, ScrollDelta, SharedString,
    Styled as _, TextAlign, TextRun, UnderlineStyle, canvas, div, fill, font, outline, point, px,
    size,
};
use tokio::sync::mpsc::{UnboundedSender, unbounded_channel};

use super::*;
use crate::kube_ops::ShellInput;

const SCROLLBACK: usize = 5_000;
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
    focus: Option<FocusHandle>,
    /// Where the terminal was last laid out; read by the mouse handlers.
    geom: Rc<Cell<Geom>>,
    /// Cell a selection drag started from.
    drag: Rc<Cell<Option<(u16, u16)>>>,
}

/// The terminal surface's bounds and the measured width of one cell.
#[derive(Clone, Copy)]
struct Geom {
    bounds: Bounds<gpui_kit::Pixels>,
    cell_w: f32,
}

gpui_kit::actions!(shell_terminal, [TermTab, TermShiftTab, TermCopy, TermPaste, TermInterrupt]);

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
            focus: None,
            geom: Rc::new(Cell::new(Geom {
                bounds: Bounds::default(),
                cell_w: 0.0,
            })),
            drag: Rc::new(Cell::new(None)),
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

    pub(super) fn update(&mut self, msg: ShellMsg, rt: &Handle, bus: &Bus) -> Task {
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
                return Task::window(move |app, window, cx| {
                    let text = cx.read_from_clipboard().and_then(|item| item.text());
                    app.dispatch(Message::Shell(id, ShellMsg::Pasted(text)), window, cx);
                });
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
                    return Task::copy(text);
                }
            }
            ShellMsg::CopyScreen => {
                return Task::done(Message::Copy(self.parser.screen().contents()));
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

    /// Creates this tab's retained state (focus handle) as needed.
    pub(super) fn prepare(&mut self, window: &mut Window, cx: &mut Context<App>) {
        let _ = window;
        if self.focus.is_none() {
            self.focus = Some(cx.focus_handle());
            bind_keys(cx);
        }
    }

    /// Status strip and terminal.
    pub(super) fn view(&self) -> AnyElement {
        let id = self.id;
        let (color, status) = match &self.status {
            ShellStatus::Connecting => (YELLOW, "Connecting…".to_string()),
            ShellStatus::Open => (
                GREEN,
                format!("Connected · {}/{}", self.namespace, self.title()),
            ),
            ShellStatus::Exited(None) => (theme::IDLE, "Session ended".to_string()),
            ShellStatus::Exited(Some(e)) => (RED, format!("Session ended: {e}")),
        };
        let strip = div()
            .h(px(28.0))
            .w_full()
            .flex()
            .flex_none()
            .items_center()
            .gap(px(6.0))
            .child(theme::dot(color))
            .child(
                div()
                    .min_w_0()
                    .overflow_hidden()
                    .child(theme::muted(status)),
            )
            .child(theme::fill_space())
            .when(matches!(self.status, ShellStatus::Exited(_)), |el| {
                el.child(theme::primary_button(
                    format!("shell-reconnect-{id}"),
                    Some(icon::ARROW_CLOCKWISE),
                    "Reconnect",
                    Some(Message::Shell(id, ShellMsg::Reconnect)),
                ))
            })
            .child(theme::icon_button(
                format!("shell-copy-{id}"),
                icon::COPY,
                "Copy the visible screen",
                Some(Message::Shell(id, ShellMsg::CopyScreen)),
            ));

        let snapshot = self.snapshot();
        let app_cursor = self.parser.screen().application_cursor();
        let size = self.size;
        let (geom, drag) = (self.geom.clone(), self.drag.clone());
        let focus = self.focus.clone();

        let surface = canvas(
            {
                let geom = geom.clone();
                move |bounds, window, cx| {
                    let cell_w = cell_width(window);
                    geom.set(Geom { bounds, cell_w });
                    // Keep the pod's terminal size matched to the surface.
                    let cols = (((f32::from(bounds.size.width) - 2.0 * PAD_X) / cell_w).floor()
                        as u16)
                        .clamp(10, 1000);
                    let rows = (((f32::from(bounds.size.height) - 2.0 * PAD_Y) / LINE_H).floor()
                        as u16)
                        .clamp(2, 500);
                    if (cols, rows) != size {
                        window.defer(cx, move |window, cx| {
                            send(Message::Shell(id, ShellMsg::Resize(cols, rows)), window, cx)
                        });
                    }
                    cell_w
                }
            },
            {
                let focus = focus.clone();
                move |bounds, cell_w, window, cx| {
                    let focused = focus.as_ref().is_some_and(|f| f.is_focused(window));
                    paint_screen(&snapshot, bounds, cell_w, focused, window, cx);
                    // A drag keeps selecting while the pointer leaves the surface.
                    let (g, d) = (geom.clone(), drag.clone());
                    window.on_mouse_event(move |e: &MouseMoveEvent, phase, window, cx| {
                        if phase != DispatchPhase::Bubble {
                            return;
                        }
                        if let Some(anchor) = d.get() {
                            let head = cell_at(g.get(), size, e.position);
                            send(
                                Message::Shell(id, ShellMsg::Select(Some((anchor, head)))),
                                window,
                                cx,
                            );
                        }
                    });
                    let d = drag.clone();
                    window.on_mouse_event(move |_: &MouseUpEvent, phase, _, _| {
                        if phase == DispatchPhase::Bubble {
                            d.set(None);
                        }
                    });
                }
            },
        )
        .size_full();

        let mut area = div()
            .id(format!("shell-term-{id}"))
            .key_context("Terminal")
            .flex_1()
            .min_h_0()
            .w_full()
            .cursor_text()
            .child(surface)
            .on_action(move |_: &TermTab, window, cx| input(id, b"\t", window, cx))
            .on_action(move |_: &TermShiftTab, window, cx| input(id, b"\x1b[Z", window, cx))
            .on_action(move |_: &TermInterrupt, window, cx| input(id, &[3], window, cx))
            .on_action(move |_: &TermCopy, window, cx| {
                send(Message::Shell(id, ShellMsg::CopySelection), window, cx)
            })
            .on_action(move |_: &TermPaste, window, cx| {
                send(Message::Shell(id, ShellMsg::Paste), window, cx)
            })
            .on_key_down(move |e, window, cx| {
                let ks = &e.keystroke;
                // Cmd shortcuts stay with the app.
                if ks.modifiers.platform {
                    return;
                }
                let bytes = key_bytes(&ks.key, &ks.modifiers, app_cursor).or_else(|| {
                    ks.key_char
                        .as_ref()
                        .filter(|t| t.chars().any(|c| !c.is_control()))
                        .map(|t| t.as_bytes().to_vec())
                });
                if let Some(bytes) = bytes {
                    cx.stop_propagation();
                    input(id, &bytes, window, cx);
                }
            })
            .on_scroll(move |e, window, cx| {
                let lines = match e.delta {
                    ScrollDelta::Lines(d) => (d.y * 3.0).round(),
                    ScrollDelta::Pixels(d) => (f32::from(d.y) / LINE_H).round(),
                } as isize;
                if lines != 0 {
                    cx.stop_propagation();
                    send(Message::Shell(id, ShellMsg::Scroll(lines)), window, cx);
                }
            });
        if let Some(focus) = &self.focus {
            area = area.track_focus(focus);
        }
        let (geom, drag, focus) = (self.geom.clone(), self.drag.clone(), self.focus.clone());
        let area = area.on_mouse_down(MouseButton::Left, move |e: &MouseDownEvent, window, cx| {
            if let Some(focus) = &focus {
                window.focus(focus, cx);
            }
            drag.set(Some(cell_at(geom.get(), size, e.position)));
            send(Message::Shell(id, ShellMsg::Select(None)), window, cx);
        });

        div()
            .size_full()
            .flex()
            .flex_col()
            .gap(px(4.0))
            .child(strip)
            .child(area)
            .into_any_element()
    }

    /// The visible screen as paintable runs, selection folded into the colours.
    fn snapshot(&self) -> Snapshot {
        let screen = self.parser.screen();
        let (cols, rows) = self.size;
        let sel = self
            .selection
            .map(|(a, b)| if a <= b { (a, b) } else { (b, a) });
        let selected = |r: u16, c: u16| match sel {
            Some((s, e)) if s != e => (r, c) >= s && (r, c) <= e,
            _ => false,
        };
        let mut snap = Snapshot {
            lines: Vec::with_capacity(rows as usize),
            bgs: Vec::new(),
            cursor: None,
            scrolled: screen.scrollback(),
        };
        for r in 0..rows {
            let mut segs: Vec<Seg> = Vec::new();
            let mut cur: Option<Seg> = None;
            // Current background run: (first column, columns, colour).
            let mut bg_run: Option<(u16, u16, Color)> = None;
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
                let w = if wide { 2 } else { 1 };
                match &mut bg_run {
                    Some((_, n, color)) if *color == bg => *n += w,
                    _ => {
                        if let Some((start, n, color)) = bg_run.take()
                            && color != Color::TRANSPARENT
                        {
                            snap.bgs.push((r, start, n, color));
                        }
                        bg_run = Some((c, w, bg));
                    }
                }
                let content = if cell.has_contents() {
                    cell.contents()
                } else {
                    " "
                };
                let underline = cell.underline();
                if wide {
                    // Wide glyphs get their own segment so the columns after them stay aligned.
                    segs.extend(cur.take());
                    let mut seg = Seg::new(c);
                    seg.push(&content, fg, underline);
                    segs.push(seg);
                } else {
                    cur.get_or_insert_with(|| Seg::new(c))
                        .push(&content, fg, underline);
                }
                c += w;
            }
            segs.extend(cur.take());
            if let Some((start, n, color)) = bg_run.take()
                && color != Color::TRANSPARENT
            {
                snap.bgs.push((r, start, n, color));
            }
            segs.retain(|s| !s.text.trim().is_empty() || s.underlined);
            snap.lines.push(segs);
        }
        if !screen.hide_cursor() && screen.scrollback() == 0 && self.status != ShellStatus::Connecting
        {
            snap.cursor = Some(screen.cursor_position());
        }
        snap
    }
}

/// Registers the terminal's key bindings (once). They sit under the "Terminal" key
/// context so Tab, Cmd+C and friends reach the shell instead of the app's focus handling.
fn bind_keys(cx: &mut gpui_kit::App) {
    static BOUND: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
    if BOUND.swap(true, Ordering::SeqCst) {
        return;
    }
    let ctx = Some("Terminal");
    cx.bind_keys([
        KeyBinding::new("tab", TermTab, ctx),
        KeyBinding::new("shift-tab", TermShiftTab, ctx),
        KeyBinding::new("ctrl-c", TermInterrupt, ctx),
        #[cfg(target_os = "macos")]
        KeyBinding::new("cmd-c", TermCopy, ctx),
        #[cfg(target_os = "macos")]
        KeyBinding::new("cmd-v", TermPaste, ctx),
        #[cfg(not(target_os = "macos"))]
        KeyBinding::new("ctrl-shift-c", TermCopy, ctx),
        #[cfg(not(target_os = "macos"))]
        KeyBinding::new("ctrl-shift-v", TermPaste, ctx),
    ]);
}

fn input(id: u64, bytes: &[u8], window: &mut Window, cx: &mut gpui_kit::App) {
    send(
        Message::Shell(id, ShellMsg::Input(bytes.to_vec())),
        window,
        cx,
    );
}

/// The advance of one cell in the terminal font.
fn cell_width(window: &Window) -> f32 {
    let ts = window.text_system();
    let id = ts.resolve_font(&font(theme::MONO));
    ts.advance(id, px(theme::MONO_SIZE), 'm')
        .map(|s| f32::from(s.width))
        .ok()
        .filter(|w| *w > 1.0)
        .unwrap_or(theme::MONO_SIZE * 0.6)
}

/// The (row, col) under a window position.
fn cell_at(g: Geom, (cols, rows): (u16, u16), pos: gpui_kit::Point<gpui_kit::Pixels>) -> (u16, u16) {
    let cw = g.cell_w.max(1.0);
    let col = ((f32::from(pos.x - g.bounds.origin.x) - PAD_X) / cw)
        .floor()
        .clamp(0.0, cols as f32 - 1.0) as u16;
    let row = ((f32::from(pos.y - g.bounds.origin.y) - PAD_Y) / LINE_H)
        .floor()
        .clamp(0.0, rows as f32 - 1.0) as u16;
    (row, col)
}

/// A run of text starting at a column, in one font, with per-style runs.
struct Seg {
    col: u16,
    text: String,
    runs: Vec<TextRun>,
    /// The colour of the last run, to extend it when the style repeats.
    last: Option<(Color, bool)>,
    underlined: bool,
}

impl Seg {
    fn new(col: u16) -> Self {
        Self {
            col,
            text: String::new(),
            runs: Vec::new(),
            last: None,
            underlined: false,
        }
    }

    fn push(&mut self, s: &str, fg: Color, underline: bool) {
        self.text.push_str(s);
        self.underlined |= underline;
        if self.last == Some((fg, underline)) {
            if let Some(run) = self.runs.last_mut() {
                run.len += s.len();
                return;
            }
        }
        self.last = Some((fg, underline));
        self.runs.push(TextRun {
            len: s.len(),
            font: font(theme::MONO),
            color: fg.hsla(),
            background_color: None,
            underline: underline.then(|| UnderlineStyle {
                thickness: px(1.0),
                color: Some(fg.hsla()),
                wavy: false,
            }),
            strikethrough: None,
        });
    }
}

struct Snapshot {
    lines: Vec<Vec<Seg>>,
    /// (row, first column, columns, colour)
    bgs: Vec<(u16, u16, u16, Color)>,
    cursor: Option<(u16, u16)>,
    scrolled: usize,
}

fn paint_screen(
    snap: &Snapshot,
    bounds: Bounds<gpui_kit::Pixels>,
    cell_w: f32,
    focused: bool,
    window: &mut Window,
    cx: &mut gpui_kit::App,
) {
    window.paint_quad(fill(bounds, theme::WELL.hsla()));
    let (ox, oy) = (f32::from(bounds.origin.x), f32::from(bounds.origin.y));
    let cell = |r: u16, c: u16, cols: u16| {
        Bounds::new(
            point(
                px(ox + PAD_X + c as f32 * cell_w),
                px(oy + PAD_Y + r as f32 * LINE_H),
            ),
            size(px(cols as f32 * cell_w), px(LINE_H)),
        )
    };
    for &(r, c, n, color) in &snap.bgs {
        window.paint_quad(fill(cell(r, c, n), color.hsla()));
    }
    for (r, segs) in snap.lines.iter().enumerate() {
        for seg in segs {
            let origin = cell(r as u16, seg.col, 1).origin;
            let line = window.text_system().shape_line(
                SharedString::from(seg.text.clone()),
                px(theme::MONO_SIZE),
                &seg.runs,
                None,
            );
            let _ = line.paint(origin, px(LINE_H), TextAlign::Left, None, window, cx);
        }
    }
    // Cursor: a block when focused, an outline otherwise.
    if let Some((r, c)) = snap.cursor {
        let at = cell(r, c, 1);
        if focused {
            window.paint_quad(fill(at, theme::ACCENT.scale_alpha(0.75).hsla()));
        } else {
            window.paint_quad(outline(at, theme::TEXT_MUTED.hsla(), BorderStyle::Solid));
        }
    }
    if snap.scrolled > 0 {
        let text = format!("↑ {} lines back · type to return", snap.scrolled);
        let run = TextRun {
            len: text.len(),
            font: font(theme::INTER),
            color: theme::TEXT_FAINT.hsla(),
            background_color: None,
            underline: None,
            strikethrough: None,
        };
        let line = window.text_system().shape_line(
            SharedString::from(text),
            px(12.0),
            &[run],
            None,
        );
        let x = ox + f32::from(bounds.size.width) - 10.0 - f32::from(line.width);
        let _ = line.paint(point(px(x), px(oy + 8.0)), px(16.0), TextAlign::Left, None, window, cx);
    }
}

/// Bytes a key sends to the shell (xterm conventions), from GPUI's key name. Plain
/// characters come from the keystroke's `key_char` instead.
fn key_bytes(key: &str, m: &Modifiers, app_cursor: bool) -> Option<Vec<u8>> {
    let arrow = |c: u8| -> Vec<u8> {
        if m.alt {
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
    if m.control {
        let mut chars = key.chars();
        if let (Some(ch), None) = (chars.next(), chars.next()) {
            match ch {
                ch if ch.is_ascii_alphabetic() => {
                    return Some(vec![ch.to_ascii_lowercase() as u8 & 0x1f]);
                }
                '[' => return Some(vec![0x1b]),
                '\\' => return Some(vec![0x1c]),
                ']' => return Some(vec![0x1d]),
                '@' | '2' => return Some(vec![0]),
                _ => {}
            }
        } else if key == "space" {
            return Some(vec![0]);
        }
    }
    Some(match key {
        "enter" => b"\r".to_vec(),
        "backspace" if m.alt => b"\x1b\x7f".to_vec(),
        "backspace" => vec![0x7f],
        "tab" if m.shift => b"\x1b[Z".to_vec(),
        "tab" => b"\t".to_vec(),
        "escape" => vec![0x1b],
        "up" => arrow(b'A'),
        "down" => arrow(b'B'),
        "right" => arrow(b'C'),
        "left" => arrow(b'D'),
        "home" => b"\x1b[H".to_vec(),
        "end" => b"\x1b[F".to_vec(),
        "pageup" => b"\x1b[5~".to_vec(),
        "pagedown" => b"\x1b[6~".to_vec(),
        "delete" => b"\x1b[3~".to_vec(),
        "insert" => b"\x1b[2~".to_vec(),
        "f1" => b"\x1bOP".to_vec(),
        "f2" => b"\x1bOQ".to_vec(),
        "f3" => b"\x1bOR".to_vec(),
        "f4" => b"\x1bOS".to_vec(),
        "f5" => b"\x1b[15~".to_vec(),
        "f6" => b"\x1b[17~".to_vec(),
        "f7" => b"\x1b[18~".to_vec(),
        "f8" => b"\x1b[19~".to_vec(),
        "f9" => b"\x1b[20~".to_vec(),
        "f10" => b"\x1b[21~".to_vec(),
        "f11" => b"\x1b[23~".to_vec(),
        "f12" => b"\x1b[24~".to_vec(),
        "space" => b" ".to_vec(),
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
        let ctrl = Modifiers::control();
        let none = Modifiers::default();
        assert_eq!(key_bytes("c", &ctrl, false), Some(vec![3]));
        assert_eq!(key_bytes("d", &ctrl, false), Some(vec![4]));
        assert_eq!(key_bytes("enter", &none, false), Some(b"\r".to_vec()));
        assert_eq!(key_bytes("up", &none, false), Some(b"\x1b[A".to_vec()));
        assert_eq!(key_bytes("up", &none, true), Some(b"\x1bOA".to_vec()));
        // Plain letters come through the key's text, not here.
        assert_eq!(key_bytes("c", &none, false), None);
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
