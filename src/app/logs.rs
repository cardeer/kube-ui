//! Log tabs. A tab streams one pod, or every pod behind a selector/controller merged into
//! one timeline (each line tagged with its pod), like `stern`.

use iced::{
    Element, Length, Padding, Task,
    widget::{
        self, Space, button, column, container, operation, rich_text, row, scrollable, span, text,
    },
};

use super::filter::Matcher;
use super::select_area::{SelectEvent, select_area};

use super::*;

/// Selected text's background: the accent, see-through enough to keep text readable.
const SELECTION: Color = Color::from_rgba8(0x4c, 0x8d, 0xff, 0.38);

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
/// JetBrains Mono's advance is 0.6em.
const CHAR_W: f32 = theme::MONO_SIZE * 0.6;
/// Room kept clear for a scrollbar, so it never sits on the text.
const BAR_ROOM: f32 = 12.0;

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
    CopySelection,
    SelectAll,
    /// A press or drag over the lines, in content pixels, with the click count.
    Point(SelectEvent),
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
    scroll_id: widget::Id,
    /// The line view's scroll state from its last scroll event.
    offset: f32,
    view_h: f32,
    /// Scrolled to the end: new lines keep it there while following.
    at_bottom: bool,
    /// Lines arrived since the last update.
    new_lines: bool,
    /// Older lines were prepended: scroll by this much to keep the view steady.
    pending_offset: Option<f32>,
    /// Selected text: (row, column) anchor and head. Row 0 is the banner.
    selection: Option<((usize, usize), (usize, usize))>,
    /// Where the current drag began, so a drag can run in either direction.
    anchor: Option<(usize, usize)>,
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
            scroll_id: widget::Id::unique(),
            offset: 0.0,
            view_h: 0.0,
            at_bottom: true,
            new_lines: false,
            pending_offset: None,
            selection: None,
            anchor: None,
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
    pub(super) fn after_update(&mut self) -> Task<Message> {
        self.refilter();
        let new_lines = std::mem::take(&mut self.new_lines);
        if let Some(y) = self.pending_offset.take() {
            self.offset = y;
            return operation::scroll_to(
                self.scroll_id.clone(),
                operation::AbsoluteOffset {
                    x: None,
                    y: Some(y),
                },
            );
        }
        if self.follow && self.at_bottom && new_lines {
            // Scrolling by operation reports no scroll event, so track the offset here.
            self.offset = self.end_offset();
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
            // The oldest line goes, so the selection moves up a row with the text.
            if let Some((a, b)) = self.selection {
                self.selection = (a.0 > 1 && b.0 > 1).then(|| (((a.0 - 1), a.1), ((b.0 - 1), b.1)));
            }
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
        // Older lines go in above, so the rows the selection names would move.
        self.selection = None;
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
    fn snap_to_bottom(&self) -> Task<Message> {
        operation::snap_to(
            self.scroll_id.clone(),
            operation::RelativeOffset {
                x: None,
                y: Some(1.0),
            },
        )
    }

    /// The scroll offset of the last line (the banner row counts too).
    fn end_offset(&self) -> f32 {
        let view_h = if self.view_h > 0.0 {
            self.view_h
        } else {
            300.0
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

    fn multi(&self) -> bool {
        self.sources.len() > 1
    }

    fn tag_width(&self) -> usize {
        self.sources
            .iter()
            .map(|s| s.short.len())
            .max()
            .unwrap_or(0)
    }

    /// The coloured runs of content row `i` (row 0 is the banner), exactly as drawn.
    fn row_runs(&self, i: usize) -> Vec<(String, Color)> {
        let Some(vl) = (i > 0).then(|| self.shown_line(i - 1)).flatten() else {
            return Vec::new();
        };
        let mut runs = Vec::new();
        if self.multi()
            && let Some(s) = self.sources.get(vl.src)
        {
            let w = self.tag_width();
            runs.push((format!("{:<w$} ", s.short), s.color));
        }
        if self.timestamps && !vl.line.ts.is_empty() {
            runs.push((format!("{} ", vl.line.ts), theme::TEXT_MUTED));
        }
        if vl.spans.is_empty() {
            runs.push((vl.line.text.clone(), vl.tint));
        } else {
            for sp in &vl.spans {
                runs.push((
                    vl.line.text[sp.range.clone()].to_string(),
                    sp.color.unwrap_or(vl.tint),
                ));
            }
        }
        runs
    }

    /// Row `i` as plain text, as drawn.
    fn row_text(&self, i: usize) -> String {
        self.row_runs(i).iter().map(|(t, _)| t.as_str()).collect()
    }

    /// The content row and character column at a point inside the scrolled lines.
    fn cell_at(&self, p: iced::Point) -> (usize, usize) {
        let row = (p.y / LINE).floor().max(0.0) as usize;
        let row = row.min(self.shown_len());
        let col = (p.x / CHAR_W).round().max(0.0) as usize;
        (row, col)
    }

    /// The run of word characters around a column, as (start, end) character indices.
    fn word_at(&self, row: usize, col: usize) -> (usize, usize) {
        let chars: Vec<char> = self.row_text(row).chars().collect();
        if col >= chars.len() {
            return (chars.len(), chars.len());
        }
        let is_word = |c: char| c.is_alphanumeric() || "-_./:@".contains(c);
        if !is_word(chars[col]) {
            return (col, col + 1);
        }
        let mut a = col;
        let mut b = col;
        while a > 0 && is_word(chars[a - 1]) {
            a -= 1;
        }
        while b + 1 < chars.len() && is_word(chars[b + 1]) {
            b += 1;
        }
        (a, b + 1)
    }

    /// The selected rows and their character ranges, in order.
    fn selection_rows(&self) -> Option<Vec<(usize, usize, usize)>> {
        let (a, b) = self.selection?;
        let (start, end) = if a <= b { (a, b) } else { (b, a) };
        if start == end {
            return None;
        }
        Some(
            (start.0..=end.0)
                .map(|row| {
                    let len = self.row_text(row).chars().count();
                    let from = if row == start.0 { start.1.min(len) } else { 0 };
                    let to = if row == end.0 { end.1.min(len) } else { len };
                    (row, from, to)
                })
                .collect(),
        )
    }

    pub(super) fn selected_text(&self) -> Option<String> {
        let rows = self.selection_rows()?;
        let text = rows
            .iter()
            .map(|&(row, from, to)| {
                self.row_text(row)
                    .chars()
                    .skip(from)
                    .take(to.saturating_sub(from))
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n");
        (!text.trim().is_empty()).then_some(text)
    }

    pub(super) fn update(
        &mut self,
        msg: LogMsg,
        next_id: u64,
        rt: &Handle,
        bus: &Bus,
    ) -> Task<Message> {
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
                    self.offset = self.end_offset();
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
                if let Some(text) = self.selected_text() {
                    return Task::done(Message::Copy(text));
                }
                let text = (0..self.shown_len())
                    .filter_map(|i| self.shown_line(i))
                    .map(|l| self.format_plain(l))
                    .collect::<Vec<_>>()
                    .join("\n");
                return Task::done(Message::Copy(text));
            }
            LogMsg::CopySelection => {
                if let Some(text) = self.selected_text() {
                    return Task::done(Message::Copy(text));
                }
            }
            LogMsg::SelectAll => {
                let last = self.shown_len();
                let len = self.row_text(last).chars().count();
                self.selection = (last > 0).then_some(((1, 0), (last, len)));
            }
            LogMsg::Point(SelectEvent::Press(p, count)) => {
                let (row, col) = self.cell_at(p);
                match count {
                    2 => {
                        self.anchor = None;
                        let (a, b) = self.word_at(row, col);
                        self.selection = Some(((row, a), (row, b)));
                    }
                    3 => {
                        self.anchor = None;
                        let len = self.row_text(row).chars().count();
                        self.selection = Some(((row, 0), (row, len)));
                    }
                    _ => {
                        self.anchor = Some((row, col));
                        self.selection = None;
                    }
                }
            }
            LogMsg::Point(SelectEvent::Drag(p)) => {
                if let Some(anchor) = self.anchor {
                    self.selection = Some((anchor, self.cell_at(p)));
                }
            }
            LogMsg::Clear => {
                self.selection = None;
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

    /// Toolbar, pod chips and the scrolling lines. `index` is the tab's place in the dock.
    pub(super) fn view(&self, index: usize, popup: Option<&PopupKind>) -> Element<'_, Message> {
        let msg = move |m: LogMsg| Message::Log(index, m);
        let mut bar = row![].spacing(8).align_y(iced::Center);
        if self.containers.len() > 1 {
            let id = format!("log-container-{index}");
            bar = bar.push(
                container(theme::dropdown_button(
                    self.container.clone(),
                    150.0,
                    popup == Some(&PopupKind::LogContainer(index)),
                    Message::OpenPopup(
                        PopupKind::LogContainer(index),
                        widget::Id::from(id.clone()),
                    ),
                ))
                .id(id),
            );
        } else {
            bar = bar.push(theme::muted(self.container.clone()));
        }
        bar = bar.push(theme::vline(18.0));
        bar = bar.push(theme::muted("Initial lines"));
        let tail_id = format!("log-tail-{index}");
        bar = bar.push(theme::tip(
            container(theme::dropdown_button(
                tail_label(self.tail),
                76.0,
                popup == Some(&PopupKind::LogTail(index)),
                Message::OpenPopup(PopupKind::LogTail(index), widget::Id::from(tail_id.clone())),
            ))
            .id(tail_id),
            "Lines per pod to start from. Scroll to the top to load older lines.",
        ));
        bar = bar.push(theme::checkbox(
            self.timestamps,
            "Timestamps",
            msg(LogMsg::ToggleTimestamps),
        ));
        bar = bar.push(theme::checkbox(
            self.follow,
            "Auto-scroll",
            msg(LogMsg::ToggleFollow),
        ));
        bar = bar.push(theme::segmented(
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
                bar = bar.push(theme::colored(format!("Last run: {exit}"), YELLOW));
            }
        }
        let hint = if self.regex {
            "Filter (regex)"
        } else {
            "Filter lines"
        };
        bar = bar.push(theme::search_field(&self.filter, hint, 200.0, move |f| {
            msg(LogMsg::Filter(f))
        }));
        bar = bar.push(theme::toggle_button(
            ".*",
            self.regex,
            "Regular expression (case-insensitive)",
            msg(LogMsg::ToggleRegex),
        ));
        if let Some(err) = &self.matcher.error {
            bar = bar.push(theme::tip(
                theme::icon_label(icon::WARNING, "Invalid regex").map(|m: Message| m),
                err.clone(),
            ));
        }
        let restart_hint = if self.is_group() {
            "Restart (also picks up new pods)"
        } else {
            "Restart stream"
        };
        bar = bar.push(theme::icon_button(
            icon::ARROW_CLOCKWISE,
            restart_hint,
            Some(msg(LogMsg::Restart)),
        ));
        bar = bar.push(theme::icon_button(
            icon::COPY,
            "Copy the selection, or every visible line",
            Some(msg(LogMsg::Copy)),
        ));
        bar = bar.push(theme::icon_button(
            icon::BROOM,
            "Clear",
            Some(msg(LogMsg::Clear)),
        ));
        // Kept short so it never runs over the controls; a stream's message gets its own line below.
        let mut status = row![theme::faint(format!("{} lines", self.lines.len()))]
            .spacing(8)
            .align_y(iced::Center);
        if self.live() {
            status = status.push(
                row![theme::dot(GREEN), theme::colored("live", GREEN)]
                    .spacing(5)
                    .align_y(iced::Center),
            );
        } else if !self.sources.is_empty() {
            status = status.push(theme::faint("stopped"));
        }

        let mut col = column![
            row![
                container(bar.wrap().vertical_spacing(6)).width(Length::Fill),
                status
            ]
            .spacing(8)
            .align_y(iced::Alignment::Start)
        ]
        .spacing(6);

        // Pod chips: click to show/hide a pod's lines.
        if self.is_group() {
            let mut chips = row![].spacing(4).align_y(iced::Center);
            if self.resolving {
                chips = chips.push(theme::faint("Finding pods…"));
            }
            if let Some(e) = &self.error {
                chips = chips.push(theme::colored(e.clone(), RED));
            }
            let all_visible = self.sources.iter().all(|s| s.visible);
            if self.sources.len() > 1 && !all_visible {
                chips = chips.push(theme::small_button(
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
                chips = chips.push(theme::tip(
                    button(
                        row![
                            text(glyph).font(theme::ICONS).size(11.0).color(color),
                            text(s.short.clone())
                                .font(theme::MONO)
                                .size(theme::MONO_SIZE)
                                .color(color),
                        ]
                        .spacing(4)
                        .align_y(iced::Center),
                    )
                    .padding([2, 6])
                    .style(theme::menu_item(visible, color))
                    .on_press(msg(LogMsg::ToggleSource(i))),
                    hover,
                ));
            }
            col = col.push(chips.wrap());
        } else if let Some(e) = &self.error {
            col = col.push(theme::colored(e.clone(), RED));
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
            col = col.push(theme::colored(st.clone(), color));
        }
        col = col.push(theme::hline());
        col = col.push(self.lines_view(index));
        col.height(Length::Fill).into()
    }

    /// The lines, drawn only where they're visible. Row 0 is the "older lines" banner.
    fn lines_view(&self, index: usize) -> Element<'_, Message> {
        let total = self.shown_len();
        if total == 0 && self.live() {
            return container(theme::faint("Waiting for log lines…"))
                .height(Length::Fill)
                .into();
        }
        let rows_total = total + 1;
        let view_h = if self.view_h > 0.0 {
            self.view_h
        } else {
            800.0
        };
        // A margin either side covers an offset that is a little out of date.
        let top = ((self.offset / LINE).floor().max(0.0) as usize).min(rows_total);
        let first = top.saturating_sub(40);
        let last = (top + (view_h / LINE).ceil() as usize + 40).min(rows_total);
        // Selected ranges by content row, so each row can tint its own characters.
        let sel: HashMap<usize, (usize, usize)> = self
            .selection_rows()
            .unwrap_or_default()
            .into_iter()
            .map(|(row, a, b)| (row, (a, b)))
            .collect();

        let mut col = column![Space::new().height(first as f32 * LINE)];
        for i in first..last {
            if i == 0 {
                let banner: Element<'_, Message> = match self.older {
                    Older::Loading => theme::faint("Loading older lines…").size(12.5).into(),
                    Older::Done => theme::faint("Beginning of log").size(12.5).into(),
                    Older::Idle => {
                        button(theme::icon_label(icon::ARROW_LINE_UP, "Load older lines"))
                            .padding(0)
                            .style(theme::link(theme::ACCENT_HOVER))
                            .on_press(Message::Log(index, LogMsg::LoadOlder))
                            .into()
                    }
                };
                col = col.push(container(banner).height(LINE).center_y(LINE));
                continue;
            }
            if self.shown_line(i - 1).is_none() {
                continue;
            }
            // Runs are split again where the selection starts and ends, so only the
            // selected characters get a highlight.
            let selected = sel.get(&i).copied();
            let mut spans: Vec<text::Span<'_, ()>> = Vec::new();
            let mut at = 0usize;
            for (run, color) in self.row_runs(i) {
                let chars: Vec<char> = run.chars().collect();
                let mut cut = vec![0usize, chars.len()];
                if let Some((a, b)) = selected {
                    for edge in [a, b] {
                        if edge > at && edge - at < chars.len() {
                            cut.push(edge - at);
                        }
                    }
                }
                cut.sort_unstable();
                cut.dedup();
                for w in cut.windows(2) {
                    let (from, to) = (w[0], w[1]);
                    let piece: String = chars[from..to].iter().collect();
                    let on = selected.is_some_and(|(a, b)| at + from >= a && at + to <= b);
                    let mut sp = span(piece).color(color);
                    if on {
                        sp = sp.background(SELECTION);
                    }
                    spans.push(sp);
                }
                at += chars.len();
            }
            col = col.push(
                rich_text(spans)
                    .font(theme::MONO)
                    .size(theme::MONO_SIZE)
                    .line_height(text::LineHeight::Absolute(LINE.into()))
                    .wrapping(text::Wrapping::None)
                    .height(LINE),
            );
        }
        col = col.push(Space::new().height((rows_total - last) as f32 * LINE));
        let lines = select_area(col.width(Length::Shrink), move |e| {
            Message::Log(index, LogMsg::Point(e))
        });
        // Room around the lines for the scrollbars, which float over the content.
        let lines = container(lines).padding(Padding {
            top: 2.0,
            right: BAR_ROOM,
            bottom: BAR_ROOM,
            left: 2.0,
        });
        scrollable(lines)
            .id(self.scroll_id.clone())
            .direction(theme::thin_both())
            .style(theme::scrollbar)
            .on_scroll(move |v| {
                Message::Log(
                    index,
                    LogMsg::Scrolled(
                        v.absolute_offset().y,
                        v.bounds().height,
                        v.content_bounds().height,
                    ),
                )
            })
            .width(Length::Fill)
            .height(Length::Fill)
            .into()
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

    /// A view with `texts` as its lines and no pod tags or timestamps.
    fn view_with(texts: &[&str]) -> LogView {
        let rt = tokio::runtime::Runtime::new().unwrap();
        let _g = rt.enter();
        let cfg = kube::Config::new("http://127.0.0.1:1".parse().unwrap());
        let client = Client::try_from(cfg).unwrap();
        let clients = kube_ops::Clients {
            main: client.clone(),
            logs: client,
        };
        let mut v = LogView::new(
            "k".into(),
            "ctx".into(),
            clients,
            "ns".into(),
            LogTarget::Pod("pod".into()),
            vec!["app".into()],
            None,
            false,
            HashMap::new(),
        );
        for t in texts {
            v.lines.push_back(ViewLine::new(
                0,
                LogLine {
                    ts: String::new(),
                    text: (*t).to_string(),
                },
            ));
        }
        v
    }

    #[test]
    fn drag_selects_across_rows() {
        let mut v = view_with(&["hello world", "second line"]);
        // Press on row 1 at column 6, drag to row 2 column 6.
        let p =
            |row: usize, col: usize| iced::Point::new(col as f32 * CHAR_W, row as f32 * LINE + 2.0);
        v.selection = None;
        v.anchor = Some(v.cell_at(p(1, 6)));
        v.selection = Some((v.anchor.unwrap(), v.cell_at(p(2, 6))));
        assert_eq!(v.selected_text().as_deref(), Some("world\nsecond"));
        // Backwards drags select the same text.
        v.selection = Some((v.cell_at(p(2, 6)), v.cell_at(p(1, 6))));
        assert_eq!(v.selected_text().as_deref(), Some("world\nsecond"));
    }

    #[test]
    fn double_click_picks_a_word() {
        let v = view_with(&["get /api/v1 200 ok"]);
        assert_eq!(v.word_at(1, 6), (4, 11));
        assert_eq!(v.row_text(1), "get /api/v1 200 ok");
        // Row 0 is the banner, with no text of its own.
        assert_eq!(v.row_text(0), "");
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
