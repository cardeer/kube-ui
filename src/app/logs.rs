//! Log tabs. A tab streams one pod, or every pod behind a selector/controller merged into
//! one timeline (each line tagged with its pod), like `stern`.
//!
//! PORTING STUB (gpui-kit branch): rendering and scrolling were removed; see `git show
//! main:src/app/logs.rs`. The logs part re-adds them on GPUI (virtualised lines).

use std::ops::Range;

use gpui_kit::{
    AnyElement, HighlightStyle, InteractiveElement as _, IntoElement as _,
    ParentElement as _, ScrollHandle, SharedString, StatefulInteractiveElement as _,
    Styled as _, StyledText, Subscription, div,
    component::{
        Sizable as _, h_flex, v_flex,
        input::{Input, InputEvent, InputState},
        scroll::ScrollableElement as _,
    },
    prelude::FluentBuilder as _,
    px,
};

use super::filter::Matcher;

use super::*;

/// Distinct, readable-on-dark colors for telling pods apart.
const PALETTE: [Color; 8] = [
    Color::from_rgb8(110, 165, 240),
    Color::from_rgb8(120, 200, 130),
    Color::from_rgb8(230, 170, 90),
    Color::from_rgb8(200, 130, 230),
    Color::from_rgb8(90, 200, 200),
    Color::from_rgb8(235, 120, 150),
    Color::from_rgb8(190, 200, 100),
    Color::from_rgb8(160, 150, 240),
];

/// Row pitch of the log lines: about 1.35 line height for the 12.5px mono.
const LINE: f32 = 17.0;

#[derive(Clone, PartialEq, Debug)]
pub enum LogTarget {
    Pod(String),
    /// Every pod matching a label selector, e.g. a deployment's or service's.
    Selector {
        label: String,
        selector: String,
    },
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

/// What the user did in a log tab.
#[derive(Clone, Debug)]
pub enum LogMsg {
    SetContainer(String),
    SetTail(i64),
    ToggleTimestamps,
    ToggleFollow,
    SetPrevious(bool),
    Filter(String),
    ToggleRegex,
    Restart,
    Copy,
    Clear,
    ToggleSource(usize),
    ShowAll,
    LoadOlder,
    /// The lines scrolled: (offset, viewport height, content height).
    Scrolled(f32, f32, f32),
}

struct Source {
    pod: String,
    /// Short tag shown before each line (the pod name's last segment).
    short: String,
    color: Color,
    handle: Option<JoinHandle<()>>,
    streaming: bool,
    status: Option<String>,
    visible: bool,
    /// Lines from this pod currently in the buffer; sizes its "load older" request.
    held: usize,
}

struct ViewLine {
    src: usize,
    /// `line.text` has ANSI escapes removed; their colours live in `spans`.
    line: LogLine,
    spans: Vec<Span>,
    /// The colour of plain text on this line (red for errors, amber for warnings).
    tint: Color,
}

/// A run of `line.text` (byte range) with the colour an ANSI SGR code gave it.
#[derive(Debug, PartialEq)]
struct Span {
    range: std::ops::Range<usize>,
    color: Option<Color>,
}

impl ViewLine {
    fn new(src: usize, mut line: LogLine) -> Self {
        let (text, spans) = parse_ansi(&line.text);
        line.text = text;
        let lower = line.text.to_lowercase();
        let tint = if lower.contains("error") || lower.contains("fatal") || lower.contains("panic")
        {
            RED
        } else if lower.contains("warn") {
            YELLOW
        } else {
            theme::TEXT
        };
        Self {
            src,
            line,
            spans,
            tint,
        }
    }
}

/// Strips ANSI escape sequences (and stray control bytes) from `raw`, returning the plain
/// text plus coloured spans for SGR foreground colours. Returns no spans for plain lines.
fn parse_ansi(raw: &str) -> (String, Vec<Span>) {
    if !raw.bytes().any(|b| b < 0x20 && b != b'\t') {
        return (raw.to_owned(), Vec::new());
    }
    let mut out = String::with_capacity(raw.len());
    let mut spans: Vec<Span> = Vec::new();
    let mut color: Option<vt100::Color> = None;
    let mut bold = false;
    let mut start = 0;
    let mut chars = raw.chars().peekable();
    let close = |spans: &mut Vec<Span>,
                 out: &str,
                 start: usize,
                 color: Option<vt100::Color>,
                 bold: bool| {
        if out.len() > start {
            let c = color
                .map(|c| super::shell::term_color(c, true, bold))
                .or(bold.then_some(theme::TEXT));
            spans.push(Span {
                range: start..out.len(),
                color: c,
            });
        }
    };
    while let Some(ch) = chars.next() {
        if ch == '\u{1b}' {
            match chars.peek() {
                Some('[') => {
                    chars.next();
                    let mut params = String::new();
                    let mut fin = None;
                    for c in chars.by_ref() {
                        if ('\u{40}'..='\u{7e}').contains(&c) {
                            fin = Some(c);
                            break;
                        }
                        params.push(c);
                    }
                    if fin != Some('m') {
                        continue;
                    }
                    close(&mut spans, &out, start, color, bold);
                    start = out.len();
                    let nums: Vec<u16> = params
                        .split([';', ':'])
                        .map(|p| p.parse().unwrap_or(0))
                        .collect();
                    let mut i = 0;
                    while i < nums.len() {
                        match nums[i] {
                            0 => (color, bold) = (None, false),
                            1 => bold = true,
                            22 => bold = false,
                            n @ 30..=37 => color = Some(vt100::Color::Idx((n - 30) as u8)),
                            n @ 90..=97 => color = Some(vt100::Color::Idx((n - 90 + 8) as u8)),
                            39 => color = None,
                            38 | 48 => {
                                let fg = nums[i] == 38;
                                let c = match nums.get(i + 1) {
                                    Some(5) => {
                                        let c =
                                            nums.get(i + 2).map(|&n| vt100::Color::Idx(n as u8));
                                        i += 2;
                                        c
                                    }
                                    Some(2) => {
                                        let g = |k| nums.get(i + k).copied().unwrap_or(0) as u8;
                                        let c = Some(vt100::Color::Rgb(g(2), g(3), g(4)));
                                        i += 4;
                                        c
                                    }
                                    _ => None,
                                };
                                if fg {
                                    color = c;
                                }
                            }
                            _ => {}
                        }
                        i += 1;
                    }
                }
                Some(_) => {
                    chars.next();
                }
                None => {}
            }
        } else if ch == '\t' || !ch.is_control() {
            out.push(ch);
        }
    }
    close(&mut spans, &out, start, color, bold);
    if spans.iter().all(|s| s.color.is_none()) {
        spans.clear();
    }
    (out, spans)
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
    pub(super) containers: Vec<String>,
    pub(super) container: String,
    pub(super) tail: i64,
    timestamps: bool,
    /// Show the run before the last restart (`kubectl logs --previous`).
    previous: bool,
    /// How each container's previous run ended, from the pod list when the tab opened.
    last_exit: HashMap<String, String>,
    follow: bool,
    filter: String,
    /// Treat `filter` as a regular expression.
    regex: bool,
    /// Compiled form of `filter`/`regex`, rebuilt when either changes.
    matcher: Matcher,
    sources: Vec<Source>,
    resolving: bool,
    error: Option<String>,
    lines: VecDeque<ViewLine>,
    /// Indices of the lines that pass the filter and pod chips; `None` when all do.
    shown: Option<Vec<usize>>,
    older: Older,
    older_pending: usize,
    older_found: bool,
    tasks: Vec<JoinHandle<()>>,
    /// The line view's scroll state from its last report (see `prepare`).
    offset: f32,
    view_h: f32,
    content_h: f32,
    /// Scroll state of the line view; owned here so follow mode can drive it.
    scroll: ScrollHandle,
    /// The filter field and its subscription, created in `prepare`.
    filter_input: Option<Entity<InputState>>,
    subs: Vec<Subscription>,
    /// Whether the field's placeholder currently says "regex".
    hint_regex: bool,
    /// Scrolled to the end: new lines keep it there while following.
    at_bottom: bool,
    /// Lines arrived since the last update.
    new_lines: bool,
    /// Older lines were prepended: scroll by this much to keep the view steady.
    pending_offset: Option<f32>,
}

impl Drop for LogView {
    fn drop(&mut self) {
        self.stop();
    }
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
        previous: bool,
        last_exit: HashMap<String, String>,
    ) -> Self {
        Self {
            stream_id: 0,
            key,
            context,
            clients,
            namespace,
            container: container
                .or_else(|| containers.first().cloned())
                .unwrap_or_default(),
            containers,
            target,
            tail: 500,
            timestamps: false,
            previous,
            last_exit,
            follow: true,
            filter: String::new(),
            regex: false,
            matcher: Matcher::default(),
            sources: Vec::new(),
            resolving: false,
            error: None,
            lines: VecDeque::new(),
            shown: None,
            older: Older::Idle,
            older_pending: 0,
            older_found: false,
            tasks: Vec::new(),
            offset: 0.0,
            view_h: 0.0,
            content_h: 0.0,
            scroll: ScrollHandle::new(),
            filter_input: None,
            subs: Vec::new(),
            hint_regex: false,
            at_bottom: true,
            new_lines: false,
            pending_offset: None,
        }
    }

    /// Dev screenshot hook: preset the line filter.
    pub(super) fn set_filter(&mut self, filter: String, regex: bool) {
        self.filter = filter;
        self.regex = regex;
        self.matcher.update(&self.filter, self.regex);
        self.refilter();
    }

    pub(super) fn stream_id(&self) -> u64 {
        self.stream_id
    }

    pub(super) fn is_group(&self) -> bool {
        !matches!(self.target, LogTarget::Pod(_))
    }

    pub(super) fn previous(&self) -> bool {
        self.previous
    }

    pub(super) fn title(&self) -> String {
        let label = self.target.label();
        let label = if self.is_group() && !self.sources.is_empty() {
            format!("{label} ({})", self.sources.len())
        } else {
            label
        };
        if self.previous {
            format!("{label} · previous")
        } else {
            label
        }
    }

    pub(super) fn hover(&self) -> String {
        format!(
            "{} · {}/{} · {}",
            self.context,
            self.namespace,
            self.target.label(),
            self.container
        )
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
        self.shown = None;
        self.older = Older::Idle;
        self.error = None;
        self.at_bottom = true;
        let hidden: HashSet<String> = self
            .sources
            .iter()
            .filter(|s| !s.visible)
            .map(|s| s.pod.clone())
            .collect();

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
        let (id, client, ns, bus) = (
            self.stream_id,
            self.clients.main.clone(),
            self.namespace.clone(),
            bus.clone(),
        );
        self.tasks.push(rt.spawn(async move {
            let result = kube_ops::pods_for_logs(client, ns, selector, owner)
                .await
                .map_err(err_string);
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
                bus.send(Msg::LogEnded {
                    id,
                    src,
                    error: result.err().map(err_string),
                });
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
            Msg::LogLine { src, line, .. } => {
                self.insert(ViewLine::new(src, line));
                self.new_lines = true;
            }
            Msg::LogEnded { src, error, .. } => {
                if let Some(s) = self.sources.get_mut(src) {
                    s.streaming = false;
                    s.status = Some(match error {
                        // `--previous` on a container that never restarted.
                        Some(e) if e.contains("previous terminated container") => {
                            "No previous run: this container hasn't restarted".into()
                        }
                        Some(e) => e,
                        None => "Stream ended".into(),
                    });
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
                            let vl = ViewLine::new(src, line);
                            shown += self.visible(&vl) as usize;
                            self.insert(vl);
                            self.older_found = true;
                        }
                        // Everything fetched is older than what's on screen, so it lands above it.
                        let base = self.pending_offset.unwrap_or(self.offset);
                        self.pending_offset = Some(base + shown as f32 * LINE);
                    }
                    Err(e) => {
                        if self.older_pending == 0 {
                            self.older = Older::Idle;
                        }
                        return Some(format!("Could not load older logs: {e}"));
                    }
                }
                if self.older_pending == 0 {
                    self.older = if self.older_found {
                        Older::Idle
                    } else {
                        Older::Done
                    };
                }
            }
            _ => {}
        }
        None
    }

    /// After a batch of messages: refilter, keep a followed log at the bottom, and keep
    /// the view steady when older lines were prepended.
    pub(super) fn after_update(&mut self) -> Task {
        self.refilter();
        let new_lines = std::mem::take(&mut self.new_lines);
        if let Some(y) = self.pending_offset.take() {
            self.offset = y;
            let at = self.scroll.offset();
            self.scroll.set_offset(gpui::point(at.x, px(-y)));
            return Task::none();
        }
        if self.follow && self.at_bottom && new_lines {
            return self.snap_to_bottom();
        }
        Task::none()
    }

    /// Inserts in timestamp order. Lines usually arrive in order, so this is normally a push;
    /// with several pods, initial bursts interleave and older pages go near the front.
    fn insert(&mut self, vl: ViewLine) {
        if let Some(s) = self.sources.get_mut(vl.src) {
            s.held += 1;
        }
        let in_order =
            vl.line.ts.is_empty() || self.lines.back().is_none_or(|b| b.line.ts <= vl.line.ts);
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
    fn load_older(&mut self, rt: &Handle, bus: &Bus) {
        if self.older != Older::Idle || self.sources.is_empty() {
            return;
        }
        // Without a full tail the streams already started at the first line.
        let Some(before) = self
            .lines
            .front()
            .map(|l| l.line.ts.clone())
            .filter(|ts| !ts.is_empty() && self.tail > 0)
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
        self.sources.get(vl.src).is_none_or(|s| s.visible) && self.matcher.matches(&vl.line.text)
    }

    /// Recomputes which lines pass the filter and the pod chips.
    fn refilter(&mut self) {
        let all_sources = self.sources.iter().all(|s| s.visible);
        self.shown = if all_sources && !self.matcher.is_active() {
            None
        } else {
            Some(
                self.lines
                    .iter()
                    .enumerate()
                    .filter(|(_, l)| self.visible(l))
                    .map(|(i, _)| i)
                    .collect(),
            )
        };
    }

    /// Scrolls to the last line, leaving the sideways position where the user put it.
    fn snap_to_bottom(&mut self) -> Task {
        // Set the estimate now so the next frame already draws the end, then let the
        // scroll area settle exactly on the real end once it has laid the content out.
        self.offset = self.end_offset();
        let at = self.scroll.offset();
        self.scroll.set_offset(gpui::point(at.x, px(-self.offset)));
        self.scroll.scroll_to_bottom();
        Task::none()
    }

    /// The scroll offset of the last line (the banner row counts too).
    fn end_offset(&self) -> f32 {
        let view_h = match f32::from(self.scroll.bounds().size.height) {
            h if h > 0.0 => h,
            _ => 300.0,
        };
        ((self.shown_len() + 1) as f32 * LINE - view_h).max(0.0)
    }

    fn shown_len(&self) -> usize {
        self.shown.as_ref().map_or(self.lines.len(), Vec::len)
    }

    fn shown_line(&self, i: usize) -> Option<&ViewLine> {
        match &self.shown {
            Some(idx) => idx.get(i).and_then(|j| self.lines.get(*j)),
            None => self.lines.get(i),
        }
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

    pub(super) fn update(
        &mut self,
        msg: LogMsg,
        next_id: u64,
        rt: &Handle,
        bus: &Bus,
    ) -> Task {
        match msg {
            LogMsg::SetContainer(c) => {
                if c != self.container {
                    self.container = c;
                    self.start(next_id, rt, bus);
                }
            }
            LogMsg::SetTail(n) => {
                if n != self.tail {
                    self.tail = n;
                    self.start(next_id, rt, bus);
                }
            }
            LogMsg::ToggleTimestamps => self.timestamps = !self.timestamps,
            LogMsg::ToggleFollow => {
                self.follow = !self.follow;
                if self.follow {
                    self.at_bottom = true;
                    return self.snap_to_bottom();
                }
            }
            LogMsg::SetPrevious(previous) => {
                self.previous = previous;
                self.start(next_id, rt, bus);
            }
            LogMsg::Filter(f) => {
                self.filter = f;
                self.matcher.update(&self.filter, self.regex);
                self.refilter();
            }
            LogMsg::ToggleRegex => {
                self.regex = !self.regex;
                self.matcher.update(&self.filter, self.regex);
                self.refilter();
            }
            LogMsg::Restart => self.start(next_id, rt, bus),
            LogMsg::Copy => {
                let text = (0..self.shown_len())
                    .filter_map(|i| self.shown_line(i))
                    .map(|l| self.format_plain(l))
                    .collect::<Vec<_>>()
                    .join("\n");
                return Task::done(Message::Copy(text));
            }
            LogMsg::Clear => {
                self.lines.clear();
                self.shown = None;
                for s in &mut self.sources {
                    s.held = 0;
                }
            }
            LogMsg::ToggleSource(i) => {
                if let Some(s) = self.sources.get_mut(i) {
                    s.visible = !s.visible;
                }
                self.refilter();
            }
            LogMsg::ShowAll => {
                for s in &mut self.sources {
                    s.visible = true;
                }
                self.refilter();
            }
            LogMsg::LoadOlder => self.load_older(rt, bus),
            LogMsg::Scrolled(offset, view_h, content_h) => {
                let up = offset < self.offset;
                self.offset = offset;
                self.view_h = view_h;
                self.content_h = content_h;
                self.at_bottom = offset + view_h >= content_h - LINE * 1.5;
                // Scrolling up into the banner loads the page before.
                if up && offset <= LINE && self.shown_len() > 0 {
                    self.load_older(rt, bus);
                }
            }
        }
        Task::none()
    }

    // -----------------------------------------------------------------------
    // Rendering
    // -----------------------------------------------------------------------

    /// Creates this tab's retained state (filter field) as needed, keeps the field in step
    /// with the filter, and reports the lines' scroll state so follow mode, "at the bottom"
    /// and "scrolled to the top" keep working.
    pub(super) fn prepare(&mut self, window: &mut Window, cx: &mut Context<App>) {
        if self.filter_input.is_none() {
            let input = cx.new(|cx| {
                InputState::new(window, cx)
                    .placeholder("Filter lines")
                    .default_value(self.filter.clone())
            });
            let key = self.key.clone();
            self.subs.push(cx.subscribe_in(
                &input,
                window,
                move |app: &mut App, state, event: &InputEvent, window, cx| {
                    if !matches!(event, InputEvent::Change) {
                        return;
                    }
                    let value = state.read(cx).value().to_string();
                    if let Some(i) = app.logs.iter().position(|l| l.key == key) {
                        app.dispatch(Message::Log(i, LogMsg::Filter(value)), window, cx);
                    }
                },
            ));
            self.hint_regex = false;
            self.filter_input = Some(input);
        }
        if let Some(input) = self.filter_input.clone() {
            if self.hint_regex != self.regex {
                self.hint_regex = self.regex;
                let hint = if self.regex {
                    "Filter (regex)"
                } else {
                    "Filter lines"
                };
                input.update(cx, |state, cx| state.set_placeholder(hint, window, cx));
            }
            // Only when set from outside (the dev hook): typing keeps them equal.
            if input.read(cx).value().as_ref() != self.filter {
                let value = self.filter.clone();
                input.update(cx, |state, cx| state.set_value(value, window, cx));
            }
        }

        let view_h = f32::from(self.scroll.bounds().size.height);
        if view_h > 0.0 {
            let offset = -f32::from(self.scroll.offset().y);
            let content_h = f32::from(self.scroll.max_offset().y) + view_h;
            if (offset, view_h, content_h) != (self.offset, self.view_h, self.content_h) {
                let key = self.key.clone();
                cx.defer_in(window, move |app, window, cx| {
                    if let Some(i) = app.logs.iter().position(|l| l.key == key) {
                        let msg = LogMsg::Scrolled(offset, view_h, content_h);
                        app.dispatch(Message::Log(i, msg), window, cx);
                    }
                });
            }
        }
    }

    fn eid(&self, what: &str) -> SharedString {
        format!("log-{what}-{}", self.key).into()
    }

    /// Toolbar, pod chips and the scrolling lines. `index` is the tab's place in the dock.
    pub(super) fn view(&self, index: usize, popup: Option<&PopupKind>) -> AnyElement {
        let msg = move |m: LogMsg| Message::Log(index, m);
        let mut bar = h_flex().flex_wrap().items_center().gap(px(8.0)).gap_y(px(6.0));
        if self.containers.len() > 1 {
            bar = bar.child(crate::app::view::trigger(
                PopupKind::LogContainer(index),
                theme::dropdown_button(
                    self.eid("container"),
                    self.container.clone(),
                    150.0,
                    popup == Some(&PopupKind::LogContainer(index)),
                    Message::OpenPopup(PopupKind::LogContainer(index)),
                ),
            ));
        } else {
            bar = bar.child(theme::muted(self.container.clone()));
        }
        bar = bar
            .child(theme::vline(18.0))
            .child(theme::muted("Initial lines"))
            .child(crate::app::view::trigger(
                PopupKind::LogTail(index),
                theme::tip(
                    theme::dropdown_button(
                        self.eid("tail"),
                        tail_label(self.tail),
                        76.0,
                        popup == Some(&PopupKind::LogTail(index)),
                        Message::OpenPopup(PopupKind::LogTail(index)),
                    ),
                    "Lines per pod to start from. Scroll to the top to load older lines.",
                ),
            ))
            .child(theme::checkbox(
                self.eid("timestamps"),
                self.timestamps,
                "Timestamps",
                msg(LogMsg::ToggleTimestamps),
            ))
            .child(theme::checkbox(
                self.eid("follow"),
                self.follow,
                "Auto-scroll",
                msg(LogMsg::ToggleFollow),
            ))
            .child(theme::segmented(
                self.eid("previous"),
                &["Current", "Previous"],
                self.previous as usize,
                "Current run, or the run before the last restart (--previous)",
                move |i| msg(LogMsg::SetPrevious(i == 1)),
            ));
        if self.previous {
            if let Some(exit) = self
                .last_exit
                .get(&self.container)
                .filter(|e| !e.is_empty())
            {
                bar = bar.child(theme::colored(format!("Last run: {exit}"), YELLOW));
            }
        }
        if let Some(input) = &self.filter_input {
            bar = bar.child(
                div().w(px(200.0)).child(
                    Input::new(input)
                        .small()
                        .prefix(theme::icon(icon::MAGNIFYING_GLASS).text_size(px(13.0))),
                ),
            );
        }
        bar = bar.child(theme::toggle_button(
            self.eid("regex"),
            ".*",
            self.regex,
            "Regular expression (case-insensitive)",
            msg(LogMsg::ToggleRegex),
        ));
        if let Some(err) = &self.matcher.error {
            bar = bar.child(theme::tip(
                div()
                    .id(self.eid("regex-error"))
                    .text_color(RED)
                    .child(theme::icon_label(icon::WARNING, "Invalid regex")),
                err.clone(),
            ));
        }
        let restart_hint = if self.is_group() {
            "Restart (also picks up new pods)"
        } else {
            "Restart stream"
        };
        bar = bar
            .child(theme::icon_button(
                self.eid("restart"),
                icon::ARROW_CLOCKWISE,
                restart_hint,
                Some(msg(LogMsg::Restart)),
            ))
            .child(theme::icon_button(
                self.eid("copy"),
                icon::COPY,
                "Copy visible lines",
                Some(msg(LogMsg::Copy)),
            ))
            .child(theme::icon_button(
                self.eid("clear"),
                icon::BROOM,
                "Clear",
                Some(msg(LogMsg::Clear)),
            ));
        // Kept short so it never runs over the controls; a stream's message gets its own line below.
        let mut status = h_flex()
            .flex_none()
            .items_center()
            .gap(px(8.0))
            .child(theme::faint(format!("{} lines", self.lines.len())));
        if self.live() {
            status = status.child(
                h_flex()
                    .items_center()
                    .gap(px(5.0))
                    .child(theme::dot(GREEN))
                    .child(theme::colored("live", GREEN)),
            );
        } else if !self.sources.is_empty() {
            status = status.child(theme::faint("stopped"));
        }

        let mut col = v_flex().size_full().gap(px(6.0)).child(
            h_flex()
                .w_full()
                .items_start()
                .gap(px(8.0))
                .child(bar.flex_1().min_w_0())
                .child(status),
        );

        // Pod chips: click to show/hide a pod's lines.
        if self.is_group() {
            let mut chips = h_flex().flex_wrap().items_center().gap(px(4.0));
            if self.resolving {
                chips = chips.child(theme::faint("Finding pods…"));
            }
            if let Some(e) = &self.error {
                chips = chips.child(theme::colored(e.clone(), RED));
            }
            let all_visible = self.sources.iter().all(|s| s.visible);
            if self.sources.len() > 1 && !all_visible {
                chips = chips.child(theme::small_button(
                    self.eid("show-all"),
                    None,
                    "Show all",
                    Some(msg(LogMsg::ShowAll)),
                ));
            }
            for (i, s) in self.sources.iter().enumerate() {
                let color = if s.visible {
                    s.color
                } else {
                    theme::TEXT_FAINT
                };
                let glyph = if s.streaming {
                    icon::CIRCLE
                } else {
                    icon::WARNING_CIRCLE
                };
                let mut hover = s.pod.clone();
                if let Some(st) = &s.status {
                    hover.push_str(&format!("\n{st}"));
                }
                hover.push_str("\nClick to show/hide");
                let visible = s.visible;
                let chip = h_flex()
                    .id(self.eid(&format!("chip-{}", s.pod)))
                    .items_center()
                    .gap(px(4.0))
                    .px(px(6.0))
                    .py(px(2.0))
                    .rounded(px(theme::RADIUS_SMALL))
                    .cursor_pointer()
                    .when(visible, |d| d.bg(theme::alpha(color, 0.12)))
                    .hover(|d| d.bg(theme::FIELD_HOVER))
                    .child(
                        theme::icon(glyph)
                            .text_size(px(11.0))
                            .line_height(px(11.0))
                            .text_color(color),
                    )
                    .child(
                        div()
                            .font_family(theme::MONO)
                            .text_size(px(theme::MONO_SIZE))
                            .text_color(color)
                            .child(s.short.clone()),
                    )
                    .on_click(on(msg(LogMsg::ToggleSource(i))));
                chips = chips.child(theme::tip(chip, hover));
            }
            col = col.child(chips);
        } else if let Some(e) = &self.error {
            col = col.child(theme::colored(e.clone(), RED));
        } else if let Some(st) = self
            .sources
            .first()
            .filter(|s| !s.streaming)
            .and_then(|s| s.status.as_ref())
        {
            let color = if st == "Stream ended" || st.starts_with("No previous run") {
                theme::TEXT_MUTED
            } else {
                YELLOW
            };
            col = col.child(theme::colored(st.clone(), color));
        }
        col = col.child(theme::hline()).child(self.lines_view(index));
        col.into_any_element()
    }

    /// The lines, built only where they're visible. Row 0 is the "older lines" banner.
    fn lines_view(&self, index: usize) -> AnyElement {
        let total = self.shown_len();
        if total == 0 && self.live() {
            return div()
                .flex_1()
                .child(theme::faint("Waiting for log lines…"))
                .into_any_element();
        }
        let rows_total = total + 1;
        let view_h = match f32::from(self.scroll.bounds().size.height) {
            h if h > 0.0 => h,
            _ => 800.0,
        };
        let offset = -f32::from(self.scroll.offset().y);
        // A margin either side covers an offset that is a little out of date.
        let top = ((offset / LINE).floor().max(0.0) as usize).min(rows_total);
        let first = top.saturating_sub(40);
        let last = (top + (view_h / LINE).ceil() as usize + 40).min(rows_total);
        let multi = self.sources.len() > 1;
        let tag_width = self
            .sources
            .iter()
            .map(|s| s.short.len())
            .max()
            .unwrap_or(0);

        // Rows are drawn without wrapping, so the content is as wide as the longest line
        // (estimated from the mono advance) to give the sideways scroll its extent.
        let longest = self.lines.iter().map(|l| l.line.text.len()).max().unwrap_or(0)
            + if multi { tag_width + 1 } else { 0 }
            + if self.timestamps { 31 } else { 0 };
        let width = (longest.min(20_000) as f32 * theme::MONO_SIZE * 0.6 + 24.0).ceil();

        let mut rows = v_flex()
            .flex_none()
            .w_full()
            .min_w(px(width))
            .child(div().h(px(first as f32 * LINE)).flex_none());
        for i in first..last {
            if i == 0 {
                let banner: AnyElement = match self.older {
                    Older::Loading => theme::faint("Loading older lines…")
                        .text_size(px(12.5))
                        .into_any_element(),
                    Older::Done => theme::faint("Beginning of log")
                        .text_size(px(12.5))
                        .into_any_element(),
                    Older::Idle => div()
                        .id(self.eid("older"))
                        .text_color(theme::ACCENT)
                        .cursor_pointer()
                        .hover(|d| d.text_color(theme::ACCENT_HOVER))
                        .child(theme::icon_label(icon::ARROW_LINE_UP, "Load older lines"))
                        .on_click(on(Message::Log(index, LogMsg::LoadOlder)))
                        .into_any_element(),
                };
                rows = rows.child(
                    div()
                        .h(px(LINE))
                        .flex_none()
                        .flex()
                        .items_center()
                        .px(px(6.0))
                        .child(banner),
                );
                continue;
            }
            let Some(vl) = self.shown_line(i - 1) else {
                continue;
            };
            rows = rows.child(self.line_row(vl, multi, tag_width));
        }
        rows = rows.child(div().h(px((rows_total - last) as f32 * LINE)).flex_none());

        div()
            .id(self.eid("lines"))
            .flex_1()
            .min_h_0()
            .w_full()
            .relative()
            .track_scroll(&self.scroll)
            .overflow_scroll()
            .vertical_scrollbar(&self.scroll)
            .horizontal_scrollbar(&self.scroll)
            .child(rows)
            .into_any_element()
    }

    /// One log line: optional pod tag and timestamp, then the text in its ANSI colours.
    fn line_row(&self, vl: &ViewLine, multi: bool, tag_width: usize) -> AnyElement {
        let mut text = String::new();
        let mut marks: Vec<(Range<usize>, HighlightStyle)> = Vec::new();
        let mut push = |text: &mut String, s: &str, color: Color| {
            let start = text.len();
            text.push_str(s);
            marks.push((
                start..text.len(),
                HighlightStyle {
                    color: Some(color.into()),
                    ..Default::default()
                },
            ));
        };
        if multi {
            if let Some(s) = self.sources.get(vl.src) {
                push(&mut text, &format!("{:<tag_width$} ", s.short), s.color);
            }
        }
        if self.timestamps && !vl.line.ts.is_empty() {
            push(&mut text, &format!("{} ", vl.line.ts), theme::TEXT_MUTED);
        }
        if vl.spans.is_empty() {
            text.push_str(&vl.line.text);
        } else {
            for s in &vl.spans {
                push(
                    &mut text,
                    &vl.line.text[s.range.clone()],
                    s.color.unwrap_or(vl.tint),
                );
            }
        }
        div()
            .h(px(LINE))
            .flex_none()
            .px(px(6.0))
            .font_family(theme::MONO)
            .text_size(px(theme::MONO_SIZE))
            .line_height(px(LINE))
            .text_color(vl.tint)
            .whitespace_nowrap()
            .child(StyledText::new(text).with_highlights(marks))
            .into_any_element()
    }
}

pub(super) fn tail_label(n: i64) -> String {
    if n == 0 {
        "All".to_string()
    } else {
        n.to_string()
    }
}
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_and_colours_ansi() {
        let (text, spans) = parse_ansi("--> GET / \u{1b}[32m200\u{1b}[0m 5ms");
        assert_eq!(text, "--> GET / 200 5ms");
        assert_eq!(spans.len(), 3);
        assert_eq!(&text[spans[1].range.clone()], "200");
        assert_eq!(spans[1].color, Some(theme::OK));
        assert_eq!(spans[2].color, None);
    }

    #[test]
    fn plain_lines_have_no_spans() {
        let (text, spans) = parse_ansi("hello\tworld");
        assert_eq!(text, "hello\tworld");
        assert!(spans.is_empty());
        let (text, spans) = parse_ansi("\u{1b}[2Kcleared\u{1b}[1A");
        assert_eq!(text, "cleared");
        assert!(spans.is_empty());
    }
}
