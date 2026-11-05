//! Log tabs. A tab streams one pod, or every pod behind a selector/controller merged into
//! one timeline (each line tagged with its pod), like `stern`.

use egui::text::{LayoutJob, TextFormat};

use super::*;

/// Distinct, readable-on-dark colors for telling pods apart.
const PALETTE: [Color32; 8] = [
    Color32::from_rgb(110, 165, 240),
    Color32::from_rgb(120, 200, 130),
    Color32::from_rgb(230, 170, 90),
    Color32::from_rgb(200, 130, 230),
    Color32::from_rgb(90, 200, 200),
    Color32::from_rgb(235, 120, 150),
    Color32::from_rgb(190, 200, 100),
    Color32::from_rgb(160, 150, 240),
];

#[derive(Clone, PartialEq)]
pub(super) enum LogTarget {
    Pod(String),
    /// Every pod matching a label selector, e.g. a deployment's or service's.
    Selector { label: String, selector: String },
    /// Every pod controlled by "Kind/name", e.g. a pod's ReplicaSet.
    Owner(String),
}

impl LogTarget {
    fn label(&self) -> String {
        match self {
            LogTarget::Pod(name) => name.clone(),
            LogTarget::Selector { label, .. } => label.clone(),
            LogTarget::Owner(owner) => owner.to_lowercase(),
        }
    }
}

struct Source {
    pod: String,
    /// Short tag shown before each line (the pod name's last segment).
    short: String,
    color: Color32,
    handle: Option<JoinHandle<()>>,
    streaming: bool,
    status: Option<String>,
    visible: bool,
    /// Lines from this pod currently in the buffer; sizes its "load older" request.
    held: usize,
}

struct ViewLine {
    src: usize,
    line: LogLine,
}

pub(super) struct LogView {
    /// Changes on every (re)start so messages from old streams are ignored.
    stream_id: u64,
    /// Identifies what this tab shows, so opening the same logs again focuses it.
    pub(super) key: String,
    context: String,
    clients: Clients,
    namespace: String,
    target: LogTarget,
    containers: Vec<String>,
    container: String,
    tail: i64,
    timestamps: bool,
    previous: bool,
    follow: bool,
    filter: String,
    sources: Vec<Source>,
    resolving: bool,
    error: Option<String>,
    lines: VecDeque<ViewLine>,
    older: Older,
    older_pending: usize,
    older_found: bool,
    tasks: Vec<JoinHandle<()>>,
    /// Scroll position and row pitch from the last frame, used to keep the view
    /// steady when older lines are prepended.
    scroll_offset: f32,
    row_pitch: f32,
    pending_offset: Option<f32>,
}

impl Drop for LogView {
    fn drop(&mut self) {
        self.stop();
    }
}

/// What the user asked for in the log toolbar this frame.
#[derive(Default)]
pub(super) struct LogUi {
    pub(super) restart: bool,
}

impl LogView {
    #[allow(clippy::too_many_arguments)]
    pub(super) fn new(
        key: String,
        context: String,
        clients: Clients,
        namespace: String,
        target: LogTarget,
        containers: Vec<String>,
        container: Option<String>,
    ) -> Self {
        Self {
            stream_id: 0,
            key,
            context,
            clients,
            namespace,
            container: container.or_else(|| containers.first().cloned()).unwrap_or_default(),
            containers,
            target,
            tail: 500,
            timestamps: false,
            previous: false,
            follow: true,
            filter: String::new(),
            sources: Vec::new(),
            resolving: false,
            error: None,
            lines: VecDeque::new(),
            older: Older::Idle,
            older_pending: 0,
            older_found: false,
            tasks: Vec::new(),
            scroll_offset: 0.0,
            row_pitch: 0.0,
            pending_offset: None,
        }
    }

    pub(super) fn stream_id(&self) -> u64 {
        self.stream_id
    }

    pub(super) fn is_group(&self) -> bool {
        !matches!(self.target, LogTarget::Pod(_))
    }

    pub(super) fn title(&self) -> String {
        let label = self.target.label();
        if self.is_group() && !self.sources.is_empty() {
            format!("{label} ({})", self.sources.len())
        } else {
            label
        }
    }

    pub(super) fn hover(&self) -> String {
        format!("{} · {}/{} · {}", self.context, self.namespace, self.target.label(), self.container)
    }

    pub(super) fn live(&self) -> bool {
        self.sources.iter().any(|s| s.streaming)
    }

    fn stop(&mut self) {
        for s in &mut self.sources {
            if let Some(h) = s.handle.take() {
                h.abort();
            }
        }
        for t in self.tasks.drain(..) {
            t.abort();
        }
    }

    /// (Re)starts streaming with the current options. Group tabs re-resolve their pods,
    /// so a restart picks up pods created by a rollout.
    pub(super) fn start(&mut self, stream_id: u64, rt: &Handle, bus: &Bus) {
        self.stop();
        self.stream_id = stream_id;
        self.lines.clear();
        self.older = Older::Idle;
        self.error = None;
        let hidden: HashSet<String> =
            self.sources.iter().filter(|s| !s.visible).map(|s| s.pod.clone()).collect();

        match self.target.clone() {
            LogTarget::Pod(name) => {
                self.set_sources(vec![name], &hidden);
                self.spawn_streams(rt, bus);
            }
            LogTarget::Selector { selector, .. } => self.resolve(Some(selector), None, rt, bus),
            LogTarget::Owner(owner) => self.resolve(None, Some(owner), rt, bus),
        }
    }

    fn resolve(&mut self, selector: Option<String>, owner: Option<String>, rt: &Handle, bus: &Bus) {
        self.resolving = true;
        self.sources.clear();
        let (id, client, ns, bus) =
            (self.stream_id, self.clients.main.clone(), self.namespace.clone(), bus.clone());
        self.tasks.push(rt.spawn(async move {
            let result = kube_ops::pods_for_logs(client, ns, selector, owner).await.map_err(err_string);
            bus.send(Msg::LogPods { id, result });
        }));
    }

    fn set_sources(&mut self, pods: Vec<String>, hidden: &HashSet<String>) {
        self.sources = pods
            .into_iter()
            .enumerate()
            .map(|(i, pod)| Source {
                short: pod.rsplit('-').next().unwrap_or(&pod).to_string(),
                color: PALETTE[i % PALETTE.len()],
                visible: !hidden.contains(&pod),
                pod,
                handle: None,
                streaming: false,
                status: None,
                held: 0,
            })
            .collect();
    }

    fn spawn_streams(&mut self, rt: &Handle, bus: &Bus) {
        for (src, source) in self.sources.iter_mut().enumerate() {
            let opts = LogOptions {
                container: self.container.clone(),
                tail: (self.tail > 0).then_some(self.tail),
                previous: self.previous,
            };
            let (id, client, ns, pod, bus) = (
                self.stream_id,
                self.clients.logs.clone(),
                self.namespace.clone(),
                source.pod.clone(),
                bus.clone(),
            );
            source.streaming = true;
            source.status = None;
            source.held = 0;
            source.handle = Some(rt.spawn(async move {
                let line_bus = bus.clone();
                let result = kube_ops::stream_logs(client, ns, pod, opts, |line| {
                    line_bus.send(Msg::LogLine { id, src, line })
                })
                .await;
                bus.send(Msg::LogEnded { id, src, error: result.err().map(err_string) });
            }));
        }
    }

    /// Handles a message for this tab. Returns an error worth showing as a toast.
    pub(super) fn handle(&mut self, msg: Msg, rt: &Handle, bus: &Bus) -> Option<String> {
        match msg {
            Msg::LogPods { result, .. } => {
                self.resolving = false;
                match result {
                    Ok(pods) if pods.is_empty() => self.error = Some("No pods match".into()),
                    Ok(pods) => {
                        // Containers come from the first pod; replicas share a spec.
                        let containers = pods[0].1.clone();
                        if !containers.contains(&self.container) {
                            self.container = containers.first().cloned().unwrap_or_default();
                        }
                        self.containers = containers;
                        let hidden = HashSet::new();
                        self.set_sources(pods.into_iter().map(|(name, _)| name).collect(), &hidden);
                        self.spawn_streams(rt, bus);
                    }
                    Err(e) => self.error = Some(e),
                }
            }
            Msg::LogLine { src, line, .. } => self.insert(ViewLine { src, line }),
            Msg::LogEnded { src, error, .. } => {
                if let Some(s) = self.sources.get_mut(src) {
                    s.streaming = false;
                    s.status = Some(error.unwrap_or_else(|| "Stream ended".into()));
                }
            }
            Msg::LogOlder { src, result, .. } => {
                self.older_pending = self.older_pending.saturating_sub(1);
                match result {
                    Ok(older) => {
                        let mut shown = 0;
                        for line in older {
                            if self.lines.len() >= MAX_LOG_LINES {
                                break;
                            }
                            let vl = ViewLine { src, line };
                            shown += self.visible(&vl) as usize;
                            self.insert(vl);
                            self.older_found = true;
                        }
                        // Everything fetched is older than what's on screen, so it lands above it.
                        let base = self.pending_offset.unwrap_or(self.scroll_offset);
                        self.pending_offset = Some(base + shown as f32 * self.row_pitch);
                    }
                    Err(e) => {
                        if self.older_pending == 0 {
                            self.older = Older::Idle;
                        }
                        return Some(format!("Could not load older logs: {e}"));
                    }
                }
                if self.older_pending == 0 {
                    self.older = if self.older_found { Older::Idle } else { Older::Done };
                }
            }
            _ => {}
        }
        None
    }

    /// Inserts in timestamp order. Lines usually arrive in order, so this is normally a push;
    /// with several pods, initial bursts interleave and older pages go near the front.
    fn insert(&mut self, vl: ViewLine) {
        if let Some(s) = self.sources.get_mut(vl.src) {
            s.held += 1;
        }
        let in_order = vl.line.ts.is_empty() || self.lines.back().is_none_or(|b| b.line.ts <= vl.line.ts);
        if in_order {
            self.lines.push_back(vl);
        } else {
            let at = self.lines.partition_point(|l| l.line.ts <= vl.line.ts);
            self.lines.insert(at, vl);
        }
        if self.lines.len() > MAX_LOG_LINES {
            if let Some(dropped) = self.lines.pop_front() {
                if let Some(s) = self.sources.get_mut(dropped.src) {
                    s.held = s.held.saturating_sub(1);
                }
            }
            // Dropped the oldest line, so there's history to page back into again.
            if self.older == Older::Done {
                self.older = Older::Idle;
            }
        }
    }

    /// Fetches, for every pod, the page of lines just before the oldest line we have.
    pub(super) fn load_older(&mut self, rt: &Handle, bus: &Bus) {
        if self.older != Older::Idle || self.sources.is_empty() {
            return;
        }
        // Without a full tail the streams already started at the first line.
        let Some(before) = self.lines.front().map(|l| l.line.ts.clone()).filter(|ts| !ts.is_empty() && self.tail > 0)
        else {
            self.older = Older::Done;
            return;
        };
        self.older = Older::Loading;
        self.older_pending = self.sources.len();
        self.older_found = false;
        for (src, source) in self.sources.iter().enumerate() {
            let opts = LogOptions {
                container: self.container.clone(),
                tail: Some((source.held + LOG_PAGE) as i64),
                previous: self.previous,
            };
            let (id, client, ns, pod, before, bus) = (
                self.stream_id,
                self.clients.logs.clone(),
                self.namespace.clone(),
                source.pod.clone(),
                before.clone(),
                bus.clone(),
            );
            self.tasks.push(rt.spawn(async move {
                let result = kube_ops::fetch_older_logs(client, ns, pod, opts, before)
                    .await
                    .map_err(err_string);
                bus.send(Msg::LogOlder { id, src, result });
            }));
        }
    }

    fn visible(&self, vl: &ViewLine) -> bool {
        self.sources.get(vl.src).is_none_or(|s| s.visible)
            && (self.filter.is_empty() || vl.line.text.to_lowercase().contains(&self.filter.to_lowercase()))
    }

    fn format_plain(&self, vl: &ViewLine) -> String {
        let mut out = String::new();
        if self.sources.len() > 1 {
            if let Some(s) = self.sources.get(vl.src) {
                out.push_str(&format!("[{}] ", s.pod));
            }
        }
        if self.timestamps && !vl.line.ts.is_empty() {
            out.push_str(&vl.line.ts);
            out.push(' ');
        }
        out.push_str(&vl.line.text);
        out
    }

    // -----------------------------------------------------------------------
    // Rendering
    // -----------------------------------------------------------------------

    /// Toolbar, pod chips and the scrolling lines. Returns what the caller must act on.
    pub(super) fn ui(&mut self, ui: &mut egui::Ui, load_older: &mut bool) -> LogUi {
        let mut out = LogUi::default();

        ui.horizontal_wrapped(|ui| {
            if self.containers.len() > 1 {
                egui::ComboBox::from_id_salt(("log-container", &self.key))
                    .icon(theme::caret)
                    .selected_text(&self.container)
                    .show_ui(ui, |ui| {
                        for c in self.containers.clone() {
                            if ui.selectable_value(&mut self.container, c.clone(), &c).changed() {
                                out.restart = true;
                            }
                        }
                    });
            } else {
                ui.weak(&self.container);
            }
            ui.separator();
            ui.label(RichText::new("Initial lines").color(theme::TEXT_MUTED));
            let tail_label = |n: i64| if n == 0 { "All".to_string() } else { n.to_string() };
            egui::ComboBox::from_id_salt(("log-tail", &self.key))
                .icon(theme::caret)
                .width(70.0)
                .selected_text(tail_label(self.tail))
                .show_ui(ui, |ui| {
                    for n in [100, 500, 1000, 5000, 0] {
                        if ui.selectable_value(&mut self.tail, n, tail_label(n)).changed() {
                            out.restart = true;
                        }
                    }
                })
                .response
                .on_hover_text("Lines per pod to start from. Scroll to the top to load older lines.");
            ui.add_space(4.0);
            theme::checkbox(ui, &mut self.timestamps, "Timestamps");
            out.restart |= theme::checkbox(ui, &mut self.previous, "Previous").changed();
            theme::checkbox(ui, &mut self.follow, "Auto-scroll");
            ui.add_space(4.0);
            theme::search_field(ui, &mut self.filter, "Filter lines", 200.0);
            let restart_hint = if self.is_group() { "Restart (also picks up new pods)" } else { "Restart stream" };
            if theme::icon_button(ui, icon::ARROW_CLOCKWISE, restart_hint).clicked() {
                out.restart = true;
            }
            if theme::icon_button(ui, icon::COPY, "Copy visible lines").clicked() {
                let text = self
                    .lines
                    .iter()
                    .filter(|l| self.visible(l))
                    .map(|l| self.format_plain(l))
                    .collect::<Vec<_>>()
                    .join("\n");
                ui.ctx().copy_text(text);
            }
            if theme::icon_button(ui, icon::BROOM, "Clear").clicked() {
                self.lines.clear();
                for s in &mut self.sources {
                    s.held = 0;
                }
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if self.live() {
                    ui.colored_label(GREEN, "live");
                    dot(ui, GREEN);
                } else if let Some(s) = self.sources.first().and_then(|s| s.status.as_ref()).filter(|_| self.sources.len() == 1) {
                    ui.weak(s);
                }
                ui.weak(format!("{} lines", self.lines.len()));
            });
        });

        // Pod chips: click to show/hide a pod's lines.
        if self.is_group() {
            ui.horizontal_wrapped(|ui| {
                ui.spacing_mut().item_spacing.x = 4.0;
                if self.resolving {
                    ui.spinner();
                    ui.weak("Finding pods…");
                }
                if let Some(e) = &self.error {
                    ui.colored_label(RED, e);
                }
                let all_visible = self.sources.iter().all(|s| s.visible);
                if self.sources.len() > 1 && !all_visible && ui.small_button("Show all").clicked() {
                    for s in &mut self.sources {
                        s.visible = true;
                    }
                }
                for s in &mut self.sources {
                    let color = if s.visible { s.color } else { ui.visuals().weak_text_color() };
                    let glyph = if s.streaming { icon::CIRCLE } else { icon::WARNING_CIRCLE };
                    let text = RichText::new(format!("{glyph} {}", s.short)).color(color).monospace();
                    let mut hover = s.pod.clone();
                    if let Some(st) = &s.status {
                        hover.push_str(&format!("\n{st}"));
                    }
                    hover.push_str("\nClick to show/hide");
                    if ui.add(egui::Button::selectable(s.visible, text)).on_hover_text(hover).clicked() {
                        s.visible = !s.visible;
                    }
                }
            });
        } else if let Some(e) = &self.error {
            ui.colored_label(RED, e);
        }
        ui.separator();

        let lines: Vec<&ViewLine> = self.lines.iter().filter(|l| self.visible(l)).collect();
        // Terminal rhythm: about 1.35 line height for the 12.5px mono.
        ui.spacing_mut().item_spacing.y = 2.0;
        let row_h = ui.text_style_height(&egui::TextStyle::Monospace);
        self.row_pitch = row_h + ui.spacing().item_spacing.y;
        let font = egui::TextStyle::Monospace.resolve(ui.style());
        let text_color = ui.visuals().text_color();
        let weak_color = ui.visuals().weak_text_color();
        let multi = self.sources.len() > 1;
        let tag_width = self.sources.iter().map(|s| s.short.len()).max().unwrap_or(0);

        let mut area = egui::ScrollArea::both().auto_shrink(false).stick_to_bottom(self.follow);
        if let Some(offset) = self.pending_offset.take() {
            area = area.vertical_scroll_offset(offset);
        }
        // Row 0 is the "older lines" banner; log lines follow.
        let older = &self.older;
        let output = area.show_rows(ui, row_h, lines.len() + 1, |ui, range| {
            ui.style_mut().interaction.selectable_labels = true;
            for i in range {
                if i == 0 {
                    ui.horizontal(|ui| match older {
                        Older::Loading => {
                            ui.weak(format!("{} Loading older lines…", icon::SPINNER));
                        }
                        Older::Done => {
                            ui.weak("Beginning of log");
                        }
                        Older::Idle => {
                            if ui.link(format!("{} Load older lines", icon::ARROW_LINE_UP)).clicked() {
                                *load_older = true;
                            }
                        }
                    });
                    continue;
                }
                let vl = lines[i - 1];
                let lower = vl.line.text.to_lowercase();
                let color = if lower.contains("error") || lower.contains("fatal") || lower.contains("panic") {
                    RED
                } else if lower.contains("warn") {
                    YELLOW
                } else {
                    text_color
                };
                let fmt = |color| TextFormat { font_id: font.clone(), color, ..Default::default() };
                let mut job = LayoutJob::default();
                if multi {
                    if let Some(s) = self.sources.get(vl.src) {
                        job.append(&format!("{:<tag_width$} ", s.short), 0.0, fmt(s.color));
                    }
                }
                if self.timestamps && !vl.line.ts.is_empty() {
                    job.append(&format!("{} ", vl.line.ts), 0.0, fmt(weak_color));
                }
                job.append(&vl.line.text, 0.0, fmt(color));
                ui.add(egui::Label::new(job).extend());
            }
        });
        self.scroll_offset = output.state.offset.y;

        // Auto-load when the user scrolls up into the banner, or when everything fits on screen.
        let near_top = output.state.offset.y <= self.row_pitch;
        let fits = output.content_size.y <= output.inner_rect.height();
        let scrolling_up =
            ui.rect_contains_pointer(output.inner_rect) && ui.input(|i| i.smooth_scroll_delta.y > 0.0);
        if near_top && !lines.is_empty() && (scrolling_up || fits) {
            *load_older = true;
        }
        if lines.is_empty() && self.live() {
            ui.weak("Waiting for log lines…");
        }
        out
    }
}
