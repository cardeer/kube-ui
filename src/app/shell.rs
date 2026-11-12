//! Shell tabs: an interactive terminal into a pod's container (`kubectl exec -it`), drawn
//! from a vt100 screen model and living in the bottom dock next to the log tabs.

use std::num::NonZeroU16;

use egui::text::{LayoutJob, TextFormat};
use tokio::sync::mpsc::{UnboundedSender, unbounded_channel};

use super::*;
use crate::kube_ops::ShellInput;

const SCROLLBACK: usize = 5_000;

#[derive(Clone, PartialEq)]
pub(super) enum ShellStatus {
    Connecting,
    Open,
    /// The shell ended; `Some` holds the reason when it wasn't a clean exit.
    Exited(Option<String>),
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
    /// Mouse selection in screen cells: (row, col) anchor and head.
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
        self.status != ShellStatus::Exited(None) && !matches!(self.status, ShellStatus::Exited(_))
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

    /// Draws the terminal and handles typing. Returns true when the user asks to reconnect.
    pub(super) fn ui(&mut self, ui: &mut egui::Ui) -> bool {
        let mut reconnect = false;
        let font = egui::TextStyle::Monospace.resolve(ui.style());
        let (char_w, line_h) = ui.fonts_mut(|f| (f.glyph_width(&font, 'm'), f.row_height(&font)));
        let char_w = char_w.max(1.0);
        let line_h = line_h.ceil().max(1.0);

        // Status strip.
        ui.horizontal(|ui| {
            let (color, text) = match &self.status {
                ShellStatus::Connecting => (YELLOW, "Connecting…".to_string()),
                ShellStatus::Open => (
                    GREEN,
                    format!("Connected · {}/{}", self.namespace, self.title()),
                ),
                ShellStatus::Exited(None) => (theme::IDLE, "Session ended".to_string()),
                ShellStatus::Exited(Some(e)) => (RED, format!("Session ended: {e}")),
            };
            dot(ui, color);
            ui.add(egui::Label::new(RichText::new(text).color(theme::TEXT_MUTED)).truncate());
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if matches!(self.status, ShellStatus::Exited(_)) {
                    if ui
                        .add(theme::primary_button(format!(
                            "{} Reconnect",
                            icon::ARROW_CLOCKWISE
                        )))
                        .clicked()
                    {
                        reconnect = true;
                    }
                }
                if theme::icon_button(ui, icon::COPY, "Copy the visible screen").clicked() {
                    ui.ctx().copy_text(self.parser.screen().contents());
                }
            });
        });
        ui.add_space(4.0);

        // Terminal area.
        let avail = ui.available_size();
        let (rect, resp) = ui.allocate_exact_size(avail, Sense::click_and_drag());
        let inner = rect.shrink2(egui::vec2(8.0, 6.0));
        let cols = ((inner.width() / char_w).floor() as u16).clamp(10, 1000);
        let rows = ((inner.height() / line_h).floor() as u16).clamp(2, 500);
        if (cols, rows) != self.size {
            self.size = (cols, rows);
            self.parser.set_size(nz(rows), nz(cols));
            if let Some(tx) = &self.tx {
                let _ = tx.send(ShellInput::Resize(cols, rows));
            }
        }

        let id = resp.id;
        if resp.clicked() || resp.drag_started() {
            ui.memory_mut(|m| m.request_focus(id));
        }
        let focused = ui.memory(|m| m.has_focus(id));
        if focused {
            // Keep Tab, arrows and Escape for the shell instead of egui's focus navigation.
            ui.memory_mut(|m| {
                m.set_focus_lock_filter(
                    id,
                    egui::EventFilter {
                        tab: true,
                        horizontal_arrows: true,
                        vertical_arrows: true,
                        escape: true,
                    },
                )
            });
        }

        // Mouse: selection by drag (screen cells), wheel scrolls back through history.
        let cell_at = |pos: egui::Pos2| -> (u16, u16) {
            let col = ((pos.x - inner.left()) / char_w)
                .floor()
                .clamp(0.0, cols as f32 - 1.0) as u16;
            let row = ((pos.y - inner.top()) / line_h)
                .floor()
                .clamp(0.0, rows as f32 - 1.0) as u16;
            (row, col)
        };
        if resp.drag_started() {
            if let Some(p) = resp.interact_pointer_pos() {
                let c = cell_at(p);
                self.selection = Some((c, c));
            }
        } else if resp.dragged() {
            if let (Some(p), Some(sel)) = (resp.interact_pointer_pos(), self.selection.as_mut()) {
                sel.1 = cell_at(p);
            }
        } else if resp.clicked() {
            self.selection = None;
        }
        if resp.hovered() {
            ui.ctx().set_cursor_icon(egui::CursorIcon::Text);
            let scroll = ui.input(|i| i.smooth_scroll_delta.y);
            if scroll.abs() > 0.0 && !self.parser.screen().alternate_screen() {
                let lines = (scroll / line_h).round() as isize;
                let now = self.parser.screen().scrollback() as isize;
                self.parser
                    .screen_mut()
                    .set_scrollback((now + lines).max(0) as usize);
            }
        }

        if focused && matches!(self.status, ShellStatus::Open | ShellStatus::Connecting) {
            self.handle_keys(ui);
        } else if focused
            && matches!(self.status, ShellStatus::Exited(_))
            && ui.input(|i| i.key_pressed(egui::Key::Enter))
        {
            reconnect = true;
        }

        self.paint(ui, rect, inner, &font, char_w, line_h, focused);
        reconnect
    }

    fn handle_keys(&mut self, ui: &mut egui::Ui) {
        let events = ui.input(|i| i.events.clone());
        let app_cursor = self.parser.screen().application_cursor();
        let bracketed = self.parser.screen().bracketed_paste();
        let mut typed = false;
        for e in events {
            match e {
                egui::Event::Text(t) => {
                    self.send(t.into_bytes());
                    typed = true;
                }
                egui::Event::Paste(t) => {
                    let t = t.replace("\r\n", "\r").replace('\n', "\r");
                    if bracketed {
                        self.send(format!("\x1b[200~{t}\x1b[201~"));
                    } else {
                        self.send(t);
                    }
                    typed = true;
                }
                egui::Event::Copy => {
                    if let Some(text) = self.selected_text() {
                        ui.ctx().copy_text(text);
                    }
                }
                egui::Event::Key {
                    key,
                    pressed: true,
                    modifiers,
                    ..
                } => {
                    if let Some(bytes) = key_bytes(key, modifiers, app_cursor) {
                        self.send(bytes);
                        typed = true;
                    }
                }
                _ => {}
            }
        }
        if typed {
            // Typing jumps back to the live screen and drops any selection.
            self.parser.screen_mut().set_scrollback(0);
            self.selection = None;
        }
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

    #[allow(clippy::too_many_arguments)]
    fn paint(
        &self,
        ui: &egui::Ui,
        rect: egui::Rect,
        inner: egui::Rect,
        font: &egui::FontId,
        char_w: f32,
        line_h: f32,
        focused: bool,
    ) {
        let painter = ui.painter_at(rect);
        painter.rect_filled(rect, 0.0, theme::WELL);
        let screen = self.parser.screen();
        let (rows, cols) = (self.size.1, self.size.0);
        let sel = self
            .selection
            .map(|(a, b)| if a <= b { (a, b) } else { (b, a) });
        let selected = |r: u16, c: u16| match sel {
            Some((s, e)) if s != e => (r, c) >= s && (r, c) <= e,
            _ => false,
        };

        for r in 0..rows {
            let y = inner.top() + r as f32 * line_h;
            let mut job = LayoutJob::default();
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
                    if bg == Color32::TRANSPARENT {
                        bg = theme::TEXT;
                    }
                    if fg == Color32::TRANSPARENT {
                        fg = theme::WELL;
                    }
                }
                if selected(r, c) {
                    bg = theme::ACCENT_BG;
                    fg = theme::ACCENT_TEXT;
                }
                if cell.dim() {
                    fg = fg.gamma_multiply(0.6);
                }
                let width = if cell.is_wide() { 2.0 } else { 1.0 };
                if bg != Color32::TRANSPARENT {
                    let x = inner.left() + c as f32 * char_w;
                    painter.rect_filled(
                        egui::Rect::from_min_size(
                            egui::pos2(x, y),
                            egui::vec2(char_w * width, line_h),
                        ),
                        0.0,
                        bg,
                    );
                }
                let text = if cell.has_contents() {
                    cell.contents()
                } else {
                    " "
                };
                job.append(
                    text,
                    0.0,
                    TextFormat {
                        font_id: font.clone(),
                        color: fg,
                        italics: cell.italic(),
                        underline: if cell.underline() {
                            egui::Stroke::new(1.0, fg)
                        } else {
                            egui::Stroke::NONE
                        },
                        ..Default::default()
                    },
                );
                c += if cell.is_wide() { 2 } else { 1 };
            }
            job.wrap.max_width = f32::INFINITY;
            let galley = ui.fonts_mut(|f| f.layout_job(job));
            painter.galley(egui::pos2(inner.left(), y), galley, theme::TEXT);
        }

        // Cursor: a block when focused, an outline otherwise; hidden while scrolled back.
        if !screen.hide_cursor()
            && screen.scrollback() == 0
            && self.status != ShellStatus::Connecting
        {
            let (r, c) = screen.cursor_position();
            let cr = egui::Rect::from_min_size(
                egui::pos2(
                    inner.left() + c as f32 * char_w,
                    inner.top() + r as f32 * line_h,
                ),
                egui::vec2(char_w, line_h),
            );
            if focused {
                painter.rect_filled(cr, 1.0, theme::ACCENT.gamma_multiply(0.75));
            } else {
                painter.rect_stroke(
                    cr,
                    1.0,
                    egui::Stroke::new(1.0, theme::TEXT_MUTED),
                    egui::StrokeKind::Inside,
                );
            }
        }
        if screen.scrollback() > 0 {
            painter.text(
                rect.right_top() + egui::vec2(-10.0, 8.0),
                Align2::RIGHT_TOP,
                format!("↑ {} lines back · type to return", screen.scrollback()),
                egui::FontId::proportional(12.0),
                theme::TEXT_FAINT,
            );
        }
    }
}

/// Bytes a key sends to the shell (xterm conventions). Plain characters arrive as text events.
fn key_bytes(key: egui::Key, m: egui::Modifiers, app_cursor: bool) -> Option<Vec<u8>> {
    use egui::Key::*;
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
    if m.ctrl && !m.command_only() {
        let name = key.name();
        if name.len() == 1 {
            let ch = name.as_bytes()[0].to_ascii_lowercase();
            if ch.is_ascii_lowercase() {
                return Some(vec![ch & 0x1f]);
            }
        }
        match key {
            OpenBracket => return Some(vec![0x1b]),
            Backslash => return Some(vec![0x1c]),
            CloseBracket => return Some(vec![0x1d]),
            _ => {}
        }
    }
    Some(match key {
        Enter => b"\r".to_vec(),
        Backspace if m.alt => b"\x1b\x7f".to_vec(),
        Backspace => vec![0x7f],
        Tab if m.shift => b"\x1b[Z".to_vec(),
        Tab => b"\t".to_vec(),
        Escape => vec![0x1b],
        ArrowUp => arrow(b'A'),
        ArrowDown => arrow(b'B'),
        ArrowRight => arrow(b'C'),
        ArrowLeft => arrow(b'D'),
        Home => b"\x1b[H".to_vec(),
        End => b"\x1b[F".to_vec(),
        PageUp => b"\x1b[5~".to_vec(),
        PageDown => b"\x1b[6~".to_vec(),
        Delete => b"\x1b[3~".to_vec(),
        Insert => b"\x1b[2~".to_vec(),
        F1 => b"\x1bOP".to_vec(),
        F2 => b"\x1bOQ".to_vec(),
        F3 => b"\x1bOR".to_vec(),
        F4 => b"\x1bOS".to_vec(),
        _ => return None,
    })
}

/// xterm colours mapped onto the app palette. Default background is transparent (the well).
pub(super) fn term_color(c: vt100::Color, fg: bool, bold: bool) -> Color32 {
    const ANSI: [Color32; 16] = [
        Color32::from_rgb(0x2e, 0x34, 0x41), // black
        theme::BAD,
        theme::OK,
        theme::WARN,
        theme::ACCENT,
        Color32::from_rgb(0xc0, 0x8b, 0xf0), // magenta
        Color32::from_rgb(0x5f, 0xc8, 0xd4), // cyan
        theme::TEXT_MUTED,                   // white
        theme::IDLE,                         // bright black
        Color32::from_rgb(0xff, 0x7b, 0x7f),
        Color32::from_rgb(0x6e, 0xe6, 0xaa),
        Color32::from_rgb(0xff, 0xc5, 0x5c),
        theme::ACCENT_HOVER,
        Color32::from_rgb(0xd6, 0xb0, 0xff),
        Color32::from_rgb(0x8a, 0xe3, 0xec),
        theme::TEXT,
    ];
    match c {
        vt100::Color::Default => {
            if fg {
                theme::TEXT
            } else {
                Color32::TRANSPARENT
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
            Color32::from_rgb(level(i / 36), level((i / 6) % 6), level(i % 6))
        }
        vt100::Color::Idx(i) => {
            let v = 8 + (i - 232) * 10;
            Color32::from_rgb(v, v, v)
        }
        vt100::Color::Rgb(r, g, b) => Color32::from_rgb(r, g, b),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn control_keys() {
        let ctrl = egui::Modifiers {
            ctrl: true,
            ..Default::default()
        };
        assert_eq!(key_bytes(egui::Key::C, ctrl, false), Some(vec![3]));
        assert_eq!(key_bytes(egui::Key::D, ctrl, false), Some(vec![4]));
        assert_eq!(
            key_bytes(egui::Key::Enter, egui::Modifiers::NONE, false),
            Some(b"\r".to_vec())
        );
        assert_eq!(
            key_bytes(egui::Key::ArrowUp, egui::Modifiers::NONE, false),
            Some(b"\x1b[A".to_vec())
        );
        assert_eq!(
            key_bytes(egui::Key::ArrowUp, egui::Modifiers::NONE, true),
            Some(b"\x1bOA".to_vec())
        );
        // Plain letters come through text events, not key events.
        assert_eq!(key_bytes(egui::Key::A, egui::Modifiers::NONE, false), None);
    }

    #[test]
    fn colours() {
        assert_eq!(
            term_color(vt100::Color::Default, false, false),
            Color32::TRANSPARENT
        );
        assert_eq!(
            term_color(vt100::Color::Idx(1), true, true),
            term_color(vt100::Color::Idx(9), true, false)
        );
        assert_eq!(
            term_color(vt100::Color::Rgb(1, 2, 3), true, false),
            Color32::from_rgb(1, 2, 3)
        );
    }
}
