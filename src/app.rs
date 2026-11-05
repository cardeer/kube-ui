mod code_editor;
mod filter;
mod logs;
mod shell;
mod theme;
mod workspace;

use std::{
    collections::{BTreeMap, HashMap, HashSet, VecDeque},
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
        mpsc::{self, Receiver, Sender},
    },
    time::{Duration, Instant},
};

use eframe::egui::{self, Align2, Color32, PopupCloseBehavior, RichText, Sense};
use egui_extras::{Column, TableBuilder};
use egui_phosphor::regular as icon;
use kube::Client;
use tokio::{
    runtime::{Handle, Runtime},
    task::JoinHandle,
};

use crate::{
    details::{self, Body, ContainerDetails, Details, EnvSource},
    kube_ops::{
        self, Clients, DeployRow, Health, Kind, KubeconfigInfo, LogLine, LogOptions, PfEvent, PfTarget, PodRow,
        SvcRow, WorkRow,
    },
    settings::{LogPanelMode, SavedForward, SavedSort, SavedWorkspace, Settings},
};
use logs::{LogTarget, LogView};
use shell::ShellView;
use workspace::Workspace;

/// How often the open drawer's details are re-fetched while live updates are on.
/// Lists themselves don't poll: they're kept current by a watch.
const DETAILS_EVERY: Duration = Duration::from_secs(5);
const MAX_LOG_LINES: usize = 100_000;
/// How many extra lines to request each time the user scrolls to the top of the logs.
const LOG_PAGE: usize = 1_000;
const TOAST_FOR: Duration = Duration::from_secs(5);
const ROW_HEIGHT: f32 = 30.0;
const NAV_WIDTH: f32 = 224.0;
const LOG_MIN_HEIGHT: f32 = 160.0;
/// Height of the log panel when collapsed to its tab strip.
const LOG_COLLAPSED_HEIGHT: f32 = 40.0;

const GREEN: Color32 = theme::OK;
const YELLOW: Color32 = theme::WARN;
const RED: Color32 = theme::BAD;

fn err_string(e: anyhow::Error) -> String {
    format!("{e:#}")
}

// ---------------------------------------------------------------------------
// Messages from background tasks to the UI thread
// ---------------------------------------------------------------------------

enum ListData {
    Pods(Vec<PodRow>),
    Services(Vec<SvcRow>),
    Deployments(Vec<DeployRow>),
    /// Any of `WORKLOAD_KINDS`; the kind is on the message.
    Workloads(Vec<WorkRow>),
}

/// Workload kinds that share the generic list, drawer and row type.
const WORKLOAD_KINDS: [Kind; 6] = [
    Kind::DaemonSet,
    Kind::StatefulSet,
    Kind::ReplicaSet,
    Kind::ReplicationController,
    Kind::Job,
    Kind::CronJob,
];

/// `ws` is the cluster tab a message belongs to; `id` is a log tab's stream id.
enum Msg {
    Connected {
        ws: u64,
        generation: u64,
        result: Result<(Clients, String, Vec<String>), String>,
    },
    Listed {
        ws: u64,
        generation: u64,
        kind: Kind,
        result: Result<ListData, String>,
        /// Some namespaces failed while others listed fine.
        warning: Option<String>,
    },
    Details { ws: u64, sel: Selected, result: Result<Details, String> },
    Secret { ws: u64, key: (String, String), result: Result<BTreeMap<String, String>, String> },
    /// An action finished; refresh that tab's list.
    Changed { ws: u64, text: String },
    LogPods { id: u64, result: Result<Vec<(String, Vec<String>)>, String> },
    LogLine { id: u64, src: usize, line: LogLine },
    LogEnded { id: u64, src: usize, error: Option<String> },
    LogOlder { id: u64, src: usize, result: Result<Vec<LogLine>, String> },
    Yaml { title: String, result: Result<String, String> },
    /// An edited YAML was saved (or refused).
    YamlApplied { title: String, result: Result<(), String> },
    /// A new object was created from the editor: (kind, namespace, name).
    YamlCreated { title: String, result: Result<(Kind, String, String), String> },
    PfListening { id: u64, port: u16 },
    PfFailed { id: u64, error: String },
    Toast { text: String, error: bool },
    ShellOutput { id: u64, data: Vec<u8> },
    ShellExited { id: u64, error: Option<String> },
}

impl Msg {
    fn workspace(&self) -> Option<u64> {
        match self {
            Msg::Connected { ws, .. }
            | Msg::Listed { ws, .. }
            | Msg::Details { ws, .. }
            | Msg::Secret { ws, .. } => Some(*ws),
            _ => None,
        }
    }

    fn shell(&self) -> Option<u64> {
        match self {
            Msg::ShellOutput { id, .. } | Msg::ShellExited { id, .. } => Some(*id),
            _ => None,
        }
    }

    fn log_stream(&self) -> Option<u64> {
        match self {
            Msg::LogPods { id, .. } | Msg::LogLine { id, .. } | Msg::LogEnded { id, .. } | Msg::LogOlder { id, .. } => {
                Some(*id)
            }
            _ => None,
        }
    }
}

/// Sender that also wakes the UI so messages are handled promptly.
#[derive(Clone)]
struct Bus {
    tx: Sender<Msg>,
    ctx: egui::Context,
}

impl Bus {
    fn send(&self, msg: Msg) {
        let _ = self.tx.send(msg);
        self.ctx.request_repaint();
    }
}

// ---------------------------------------------------------------------------
// UI state
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
enum Tab {
    Overview,
    Pods,
    Services,
    Deployments,
    /// One of `WORKLOAD_KINDS`.
    Workload(Kind),
    Forwards,
    Settings,
}

impl Tab {
    /// Pages that show a resource list (and the detail drawer).
    fn is_list(self) -> bool {
        matches!(self, Tab::Pods | Tab::Services | Tab::Deployments | Tab::Workload(_))
    }

    /// The resource lists this page needs loaded (and watched).
    fn kinds(self) -> Vec<Kind> {
        match self {
            Tab::Pods => vec![Kind::Pod],
            Tab::Services => vec![Kind::Service],
            Tab::Deployments => vec![Kind::Deployment],
            Tab::Workload(k) => vec![k],
            Tab::Overview => [Kind::Pod, Kind::Deployment].into_iter().chain(WORKLOAD_KINDS).collect(),
            Tab::Forwards | Tab::Settings => vec![],
        }
    }

    /// The list page for a kind.
    fn for_kind(kind: Kind) -> Tab {
        match kind {
            Kind::Pod => Tab::Pods,
            Kind::Service => Tab::Services,
            Kind::Deployment => Tab::Deployments,
            k => Tab::Workload(k),
        }
    }
}

enum Conn {
    Idle,
    Connecting,
    Ready(Clients),
    Failed(String),
}

impl Tab {
    /// Key used when saving per-page state such as filters.
    fn key(self) -> &'static str {
        match self {
            Tab::Overview => "overview",
            Tab::Pods => "pods",
            Tab::Services => "services",
            Tab::Deployments => "deployments",
            Tab::Workload(Kind::DaemonSet) => "daemonsets",
            Tab::Workload(Kind::StatefulSet) => "statefulsets",
            Tab::Workload(Kind::ReplicaSet) => "replicasets",
            Tab::Workload(Kind::ReplicationController) => "replicationcontrollers",
            Tab::Workload(Kind::Job) => "jobs",
            Tab::Workload(Kind::CronJob) => "cronjobs",
            Tab::Workload(_) => "workloads",
            Tab::Forwards => "forwards",
            Tab::Settings => "settings",
        }
    }

    fn from_key(key: &str) -> Option<Self> {
        [Tab::Overview, Tab::Pods, Tab::Services, Tab::Deployments, Tab::Forwards, Tab::Settings]
            .into_iter()
            .chain(WORKLOAD_KINDS.map(Tab::Workload))
            .find(|t| t.key() == key)
    }
}

impl Conn {
    fn status(&self) -> (Color32, &'static str) {
        match self {
            Conn::Ready(_) => (GREEN, "Connected"),
            Conn::Connecting => (YELLOW, "Connecting…"),
            Conn::Failed(_) => (RED, "Failed"),
            Conn::Idle => (theme::IDLE, "Idle"),
        }
    }
}

struct ListState<T> {
    items: Vec<T>,
    loading: bool,
    loaded: bool,
    error: Option<String>,
}

impl<T> Default for ListState<T> {
    fn default() -> Self {
        Self { items: Vec::new(), loading: false, loaded: false, error: None }
    }
}

/// The resource shown in the detail drawer.
#[derive(Clone, PartialEq)]
struct Selected {
    kind: Kind,
    namespace: String,
    name: String,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum DrawerTab {
    Overview,
    Containers,
    Volumes,
    Endpoints,
    Events,
}

struct DetailsState {
    sel: Selected,
    /// Kept while a refresh is in flight so the drawer doesn't flicker.
    data: Option<Result<Details, String>>,
    loading: bool,
}

#[derive(PartialEq)]
enum Older {
    Idle,
    Loading,
    /// Reached the start of the log (or the buffer limit).
    Done,
}

struct YamlView {
    title: String,
    /// The cluster tab and object it shows, so edits are saved to the right place.
    ws: u64,
    kind: Kind,
    namespace: String,
    name: String,
    text: Option<Result<String, String>>,
    /// The edit buffer while editing.
    edit: Option<String>,
    /// Open straight into editing once the YAML arrives.
    edit_on_load: bool,
    saving: bool,
    save_error: Option<String>,
    /// Live YAML check of the edit buffer: (1-based line, message).
    parse_error: Option<(usize, String)>,
    /// Editing just began: focus the editor with the cursor at the top.
    edit_started: bool,
    /// A new object (Apply creates it) rather than an existing one.
    create: bool,
}

#[derive(PartialEq)]
enum PfStatus {
    Starting,
    Listening,
    Failed(String),
    Stopped,
}

struct Forward {
    id: u64,
    /// What to forward; `spec.local_port` 0 means random, `bound_port` is what we got.
    spec: SavedForward,
    bound_port: Option<u16>,
    status: PfStatus,
    connections: Arc<AtomicUsize>,
    handle: Option<JoinHandle<()>>,
}

struct PfDialog {
    kubeconfig: PathBuf,
    context: String,
    namespace: String,
    target: PfTarget,
    ports: Vec<u16>,
    remote_port: u16,
    local_port: u16,
}

struct Toast {
    text: String,
    error: bool,
    at: Instant,
}

/// Things the user clicked while rendering; applied after rendering to keep borrows simple.
/// Resource actions apply to the active cluster tab.
enum Action {
    Select(Selected),
    GoToPod(String, String),
    RevealSecret(String, String),
    HideSecret(String, String),
    OpenLogs(PodRow, Option<String>),
    /// Open a shell in a pod's container (`None` = the default container).
    Shell(PodRow, Option<String>),
    OpenGroupLogs { namespace: String, target: LogTarget },
    Yaml(Kind, String, String),
    ForwardPod(PodRow, Option<u16>),
    ForwardService(SvcRow, Option<u16>),
    /// Ask before deleting any resource.
    AskDelete(Kind, String, String),
    /// Open the YAML window in edit mode.
    EditYaml(Kind, String, String),
    /// Open the editor on a template for a new object of this kind.
    Create(Kind),
    /// Run a cron job now.
    Trigger(String, String),
    Suspend(Kind, String, String, bool),
    /// Pause or resume a deployment's rollout.
    Pause(String, String, bool),
    Scale(Kind, String, String, i32),
    Restart(Kind, String, String),
    /// Switch page (from the Overview).
    Open(Tab),
    /// Switch page and select a resource on it.
    OpenSelected(Selected),
    StopForward(u64),
    RestartForward(u64),
    RemoveForward(u64),
}

pub struct App {
    rt: Runtime,
    bus: Bus,
    rx: Receiver<Msg>,
    settings: Settings,

    /// Cluster tabs. Empty means the welcome screen.
    workspaces: Vec<Workspace>,
    active: usize,

    logs: Vec<LogView>,
    /// Shell tabs; the dock shows log tabs first, then these.
    shells: Vec<ShellView>,
    /// Index into the dock's tabs: logs, then shells.
    active_log: usize,
    yaml: Option<YamlView>,
    forwards: Vec<Forward>,
    pf_dialog: Option<PfDialog>,
    /// (cluster tab, namespace, pod)
    confirm_delete: Option<(u64, Kind, String, String)>,
    toasts: Vec<Toast>,
    next_id: u64,
    /// When the window size last changed and hasn't been written to disk yet.
    window_dirty: Option<Instant>,
    /// The user is dragging the log panel's edge.
    log_resizing: bool,
    /// Development aid: `KUBE_UI_SCREENSHOT=<file.png>` captures the window once and quits.
    /// With `KUBE_UI_SCREENSHOT_TAB=<page key>` (e.g. `overview`, `jobs`) it shows that page
    /// instead of a pod with its logs; `KUBE_UI_SCREENSHOT_SELECT=1` also opens the first row,
    /// and `KUBE_UI_SCREENSHOT_EDIT=1` opens it in the YAML editor (nothing is saved).
    screenshot: Option<DevScreenshot>,
}

struct DevScreenshot {
    path: PathBuf,
    started: Instant,
    /// When the scene (a pod selected, its logs open) was set up.
    staged: Option<Instant>,
    requested: bool,
    tab: Option<Tab>,
    select: bool,
    /// Also open the first row in the YAML editor.
    edit: bool,
    /// `KUBE_UI_SCREENSHOT_SORT=<Column>[:desc]` sorts the page first.
    sort: Option<Sort>,
    /// `KUBE_UI_SCREENSHOT_SHELL=<command>` opens a shell on the first running pod and types it.
    shell: Option<String>,
    /// `KUBE_UI_SCREENSHOT_LOG_REGEX=<pattern>` sets the log tab's filter in regex mode.
    log_regex: Option<String>,
}

impl App {
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        let ctx = cc.egui_ctx.clone();
        ctx.set_theme(egui::Theme::Dark);
        theme::install(&ctx);

        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .expect("failed to start tokio runtime");
        let (tx, rx) = mpsc::channel();

        let mut app = Self {
            rt,
            bus: Bus { tx, ctx },
            rx,
            settings: Settings::load(),
            workspaces: Vec::new(),
            active: 0,
            logs: Vec::new(),
            shells: Vec::new(),
            active_log: 0,
            yaml: None,
            forwards: Vec::new(),
            pf_dialog: None,
            confirm_delete: None,
            toasts: Vec::new(),
            next_id: 1,
            window_dirty: None,
            log_resizing: false,
            screenshot: std::env::var_os("KUBE_UI_SCREENSHOT").map(|p| DevScreenshot {
                path: PathBuf::from(p),
                started: Instant::now(),
                staged: None,
                requested: false,
                tab: std::env::var("KUBE_UI_SCREENSHOT_TAB").ok().and_then(|k| Tab::from_key(&k)),
                select: std::env::var_os("KUBE_UI_SCREENSHOT_SELECT").is_some(),
                edit: std::env::var_os("KUBE_UI_SCREENSHOT_EDIT").is_some(),
                shell: std::env::var("KUBE_UI_SCREENSHOT_SHELL").ok(),
                log_regex: std::env::var("KUBE_UI_SCREENSHOT_LOG_REGEX").ok(),
                sort: std::env::var("KUBE_UI_SCREENSHOT_SORT").ok().map(|v| {
                    let (col, dir) = v.split_once(':').unwrap_or((&v, "asc"));
                    Sort { col: col.to_string(), asc: dir != "desc" }
                }),
            }),
        };

        // Restore cluster tabs (or migrate the single kubeconfig from older versions).
        let mut saved = app.settings.workspaces.clone();
        if saved.is_empty() {
            if let Some(path) = app.settings.active.take() {
                saved.push(SavedWorkspace {
                    kubeconfig: path,
                    context: None,
                    namespaces: Vec::new(),
                    all_namespaces: false,
                    namespace: None,
                    filters: BTreeMap::new(),
                    sorts: BTreeMap::new(),
                    filter_regex: false,
                });
            }
        }
        let active = app.settings.active_workspace;
        for s in saved {
            let selection = s.selection();
            app.add_workspace(s.kubeconfig, s.context, selection);
            if let Some(ws) = app.workspaces.last_mut() {
                ws.filter_regex = s.filter_regex;
                ws.filters = s
                    .filters
                    .iter()
                    .filter_map(|(k, v)| Some((Tab::from_key(k)?, v.clone())))
                    .collect();
                ws.sorts = s
                    .sorts
                    .iter()
                    .filter_map(|(k, v)| Some((Tab::from_key(k)?, Sort { col: v.column.clone(), asc: !v.descending })))
                    .collect();
            }
        }
        app.active = active.min(app.workspaces.len().saturating_sub(1));

        // Bring back saved forwards; resume the ones that were running when the app closed.
        for spec in app.settings.forwards.clone() {
            let resume = spec.active;
            let id = app.add_forward(spec);
            if resume {
                app.start_forward(id);
            }
        }
        app
    }

    fn next_id(&mut self) -> u64 {
        self.next_id += 1;
        self.next_id
    }

    fn toast(&mut self, text: impl Into<String>, error: bool) {
        self.toasts.push(Toast { text: text.into(), error, at: Instant::now() });
    }

    fn ws(&self) -> Option<&Workspace> {
        self.workspaces.get(self.active)
    }

    fn ws_mut(&mut self) -> Option<&mut Workspace> {
        self.workspaces.get_mut(self.active)
    }

    // -----------------------------------------------------------------------
    // Cluster tabs
    // -----------------------------------------------------------------------

    fn add_workspace(&mut self, path: PathBuf, context: Option<String>, namespaces: Option<Vec<String>>) {
        let id = self.next_id();
        let ws = Workspace::new(
            id,
            self.rt.handle().clone(),
            self.bus.clone(),
            path,
            context,
            namespaces,
            &mut self.settings,
        );
        self.workspaces.push(ws);
        self.active = self.workspaces.len() - 1;
    }

    fn activate_workspace(&mut self, index: usize) {
        if index >= self.workspaces.len() {
            return;
        }
        self.active = index;
        // A live tab restarts its watch on the next frame; otherwise catch up here.
        let ws = &mut self.workspaces[index];
        if !self.settings.live_updates && ws.last_refresh.is_none_or(|t| t.elapsed() >= DETAILS_EVERY) {
            ws.refresh();
        }
    }

    fn close_workspace(&mut self, index: usize) {
        if index >= self.workspaces.len() {
            return;
        }
        self.workspaces.remove(index);
        if self.active >= index && self.active > 0 {
            self.active -= 1;
        }
    }

    /// Remembers the window size. Writes are held back until resizing has paused for a
    /// second (and done on exit), so dragging the window edge doesn't hit the disk every frame.
    fn track_window(&mut self, ctx: &egui::Context) {
        let (size, maximized) = ctx.input(|i| {
            let v = i.viewport();
            (v.inner_rect.map(|r| [r.width(), r.height()]), v.maximized.unwrap_or(false))
        });
        if maximized != self.settings.window_maximized {
            self.settings.window_maximized = maximized;
            self.window_dirty = Some(Instant::now());
        }
        // While maximized, keep the normal size so un-maximizing next launch restores it.
        if let Some(size) = size.filter(|_| !maximized) {
            let changed = self
                .settings
                .window_size
                .is_none_or(|[w, h]| (w - size[0]).abs() > 1.0 || (h - size[1]).abs() > 1.0);
            if changed {
                self.settings.window_size = Some(size);
                self.window_dirty = Some(Instant::now());
            }
        }
        if let Some(at) = self.window_dirty {
            if at.elapsed() >= Duration::from_secs(1) {
                self.window_dirty = None;
                self.settings.save();
            } else {
                ctx.request_repaint_after(Duration::from_secs(1));
            }
        }
    }

    /// Stages a representative scene, captures the window to a PNG and closes the app.
    fn dev_screenshot(&mut self, ctx: &egui::Context) {
        let Some(shot) = self.screenshot.as_ref() else { return };
        ctx.request_repaint_after(Duration::from_millis(100));
        if shot.staged.is_none() && let Some(tab) = shot.tab {
            let (select, edit, sort) = (shot.select, shot.edit, shot.sort.clone());
            let timed_out = shot.started.elapsed() > Duration::from_secs(30);
            let Some(ws) = self.ws_mut() else { return };
            if let Some(sort) = sort {
                ws.sorts.insert(tab, sort);
            }
            // `KUBE_UI_SCREENSHOT_FILTER=<regex>` presets the page's filter in regex mode.
            if let Ok(re) = std::env::var("KUBE_UI_SCREENSHOT_FILTER") {
                ws.filters.insert(tab, re);
                ws.filter_regex = true;
            }
            if ws.tab != tab {
                ws.set_tab(tab);
                return;
            }
            let loaded = ws.client().is_some() && !ws.is_loading() && ws.last_refresh.is_some();
            if !loaded && !timed_out {
                return;
            }
            let running = ws.pods.items.iter().find(|p| p.status == "Running").cloned();
            let first = ws.first_row(tab);
            if select {
                if let Some(sel) = first.clone() {
                    ws.select(sel);
                }
            }
            if let Some(sel) = first.filter(|_| edit) {
                self.apply(Action::EditYaml(sel.kind, sel.namespace, sel.name));
            }
            if std::env::var_os("KUBE_UI_SCREENSHOT_CREATE").is_some() {
                if let Some(kind) = tab.kinds().first().copied() {
                    self.apply(Action::Create(kind));
                }
            }
            if let Some(pod) = running.filter(|_| self.screenshot.as_ref().is_some_and(|s| s.shell.is_some())) {
                self.apply(Action::Shell(pod, None));
            }
            if let Some(shot) = self.screenshot.as_mut() {
                shot.staged = Some(Instant::now());
            }
            return;
        }
        if shot.staged.is_none() {
            let first_pod = self.ws().and_then(|w| w.pods.items.first().cloned());
            let timed_out = shot.started.elapsed() > Duration::from_secs(20);
            if first_pod.is_none() && !timed_out {
                return;
            }
            if let Some(pod) = first_pod {
                self.apply(Action::Select(Selected {
                    kind: Kind::Pod,
                    namespace: pod.namespace.clone(),
                    name: pod.name.clone(),
                }));
                self.apply(Action::OpenLogs(pod, None));
                if let Some(re) = self.screenshot.as_ref().and_then(|s| s.log_regex.clone()) {
                    if let Some(log) = self.logs.last_mut() {
                        log.set_filter(re, true);
                    }
                }
            }
            if let Some(shot) = self.screenshot.as_mut() {
                shot.staged = Some(Instant::now());
            }
            return;
        }
        // Type the shell command once the session is up, then give it time to print.
        if let Some(cmd) = self.screenshot.as_ref().and_then(|s| s.shell.clone()) {
            match self.shells.last_mut() {
                Some(sh) if sh.status == shell::ShellStatus::Open => {
                    sh.type_text(&format!("{cmd}\r"));
                    if let Some(s) = self.screenshot.as_mut() {
                        s.shell = None;
                        s.staged = Some(Instant::now());
                    }
                }
                _ => return,
            }
        }
        let Some(shot) = self.screenshot.as_mut() else { return };
        if !shot.requested && shot.staged.is_some_and(|t| t.elapsed() > Duration::from_secs(3)) {
            shot.requested = true;
            ctx.send_viewport_cmd(egui::ViewportCommand::Screenshot(Default::default()));
        }
        let image = ctx.input(|i| {
            i.raw.events.iter().find_map(|e| match e {
                egui::Event::Screenshot { image, .. } => Some(image.clone()),
                _ => None,
            })
        });
        if let Some(image) = image {
            let [w, h] = image.size;
            if let Err(e) = image::save_buffer(&shot.path, image.as_raw(), w as u32, h as u32, image::ExtendedColorType::Rgba8) {
                eprintln!("screenshot failed: {e}");
            }
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }
    }

    /// Persists the open tabs whenever they change.
    fn save_workspaces(&mut self) {
        let saved: Vec<SavedWorkspace> = self.workspaces.iter().map(Workspace::saved).collect();
        if saved != self.settings.workspaces || self.active != self.settings.active_workspace {
            self.settings.workspaces = saved;
            self.settings.active_workspace = self.active;
            self.settings.save();
        }
    }

    // -----------------------------------------------------------------------
    // Background message handling
    // -----------------------------------------------------------------------

    fn handle_messages(&mut self) {
        while let Ok(msg) = self.rx.try_recv() {
            if let Some(id) = msg.workspace() {
                if let Some(ws) = self.workspaces.iter_mut().find(|w| w.id == id) {
                    ws.handle(msg, &self.settings);
                }
                continue;
            }
            if let Some(id) = msg.shell() {
                if let Some(s) = self.shells.iter_mut().find(|s| s.id == id) {
                    s.handle(msg);
                }
                continue;
            }
            if let Some(id) = msg.log_stream() {
                let handle = self.rt.handle().clone();
                let error = self
                    .logs
                    .iter_mut()
                    .find(|l| l.stream_id() == id)
                    .and_then(|l| l.handle(msg, &handle, &self.bus));
                if let Some(e) = error {
                    self.toast(e, true);
                }
                continue;
            }
            match msg {
                Msg::Changed { ws, text } => {
                    self.toast(text, false);
                    if let Some(w) = self.workspaces.iter_mut().find(|w| w.id == ws) {
                        w.changed();
                    }
                }
                Msg::Yaml { title, result } => {
                    if let Some(y) = self.yaml.as_mut().filter(|y| y.title == title) {
                        if y.edit_on_load {
                            y.edit_on_load = false;
                            y.edit = result.as_ref().ok().cloned();
                            y.edit_started = y.edit.is_some();
                        }
                        y.text = Some(result);
                    }
                }
                Msg::YamlCreated { title, result } => {
                    let mut created = None;
                    if let Some(y) = self.yaml.as_mut().filter(|y| y.title == title) {
                        y.saving = false;
                        match result {
                            Ok(c) => created = Some((y.ws, c)),
                            Err(e) => y.save_error = Some(e),
                        }
                    }
                    if let Some((ws, (kind, namespace, name))) = created {
                        self.yaml = None;
                        self.toast(format!("Created {} {name}", kind.label().to_lowercase()), false);
                        if let Some(w) = self.workspaces.iter_mut().find(|w| w.id == ws) {
                            w.changed();
                        }
                        // Show it: its page, with the drawer open on it (it fills in as the watch sees it).
                        self.apply(Action::OpenSelected(Selected { kind, namespace, name }));
                    }
                }
                Msg::YamlApplied { title, result } => {
                    let mut saved = None;
                    if let Some(y) = self.yaml.as_mut().filter(|y| y.title == title) {
                        y.saving = false;
                        match result {
                            Ok(()) => {
                                y.edit = None;
                                y.save_error = None;
                                saved = Some((y.ws, y.kind, y.namespace.clone(), y.name.clone()));
                            }
                            Err(e) => y.save_error = Some(e),
                        }
                    }
                    if let Some((ws, kind, ns, name)) = saved {
                        self.toast(format!("Saved {name}"), false);
                        if let Some(w) = self.workspaces.iter_mut().find(|w| w.id == ws) {
                            w.changed();
                        }
                        // Show what the server stored (defaults filled in, new resourceVersion).
                        self.open_yaml(kind, ns, name);
                    }
                }
                Msg::PfListening { id, port } => {
                    if let Some(f) = self.forwards.iter_mut().find(|f| f.id == id) {
                        f.bound_port = Some(port);
                        f.status = PfStatus::Listening;
                    }
                    self.save_forwards();
                }
                Msg::PfFailed { id, error } => {
                    if let Some(f) = self.forwards.iter_mut().find(|f| f.id == id) {
                        f.status = PfStatus::Failed(error.clone());
                        f.handle = None;
                    }
                    self.save_forwards();
                    self.toast(format!("Port forward failed: {error}"), true);
                }
                Msg::Toast { text, error } => self.toast(text, error),
                _ => {}
            }
        }
    }

    // -----------------------------------------------------------------------
    // Actions
    // -----------------------------------------------------------------------

    fn apply(&mut self, action: Action) {
        match action {
            Action::Select(sel) => {
                if let Some(ws) = self.ws_mut() {
                    ws.select(sel);
                }
            }
            Action::GoToPod(ns, name) => {
                if let Some(ws) = self.ws_mut() {
                    ws.go_to_pod(ns, name);
                }
            }
            Action::RevealSecret(ns, name) => {
                if let Some(ws) = self.ws_mut() {
                    ws.reveal_secret(ns, name);
                }
            }
            Action::HideSecret(ns, name) => {
                if let Some(ws) = self.ws_mut() {
                    ws.hide_secret(ns, name);
                }
            }
            Action::Shell(pod, container) => self.open_shell(pod, container),
            Action::OpenLogs(pod, container) => {
                let containers = pod.containers.iter().map(|c| c.name.clone()).collect();
                self.open_logs(pod.namespace, LogTarget::Pod(pod.name), containers, container);
            }
            Action::OpenGroupLogs { namespace, target } => self.open_logs(namespace, target, Vec::new(), None),
            Action::Yaml(kind, ns, name) => self.open_yaml(kind, ns, name),
            Action::ForwardPod(pod, port) => {
                let port = port.or(pod.ports.first().copied()).unwrap_or(8080);
                self.open_pf_dialog(pod.namespace, PfTarget::Pod(pod.name), pod.ports, port, port);
            }
            Action::ForwardService(svc, port) => {
                let ports: Vec<u16> = svc.ports.iter().map(|p| p.port).collect();
                let port = port.or(ports.first().copied()).unwrap_or(80);
                // Ports below 1024 need root locally.
                let local = if port >= 1024 { port } else { 8000 + port };
                self.open_pf_dialog(svc.namespace, PfTarget::Service(svc.name), ports, port, local);
            }
            Action::AskDelete(kind, ns, name) => {
                if let Some(id) = self.ws().map(|w| w.id) {
                    self.confirm_delete = Some((id, kind, ns, name));
                }
            }
            Action::Create(kind) => self.open_create(kind),
            Action::EditYaml(kind, ns, name) => {
                self.open_yaml(kind, ns, name);
                if let Some(y) = self.yaml.as_mut() {
                    y.edit_on_load = true;
                }
            }
            Action::Trigger(ns, name) => {
                let Some((ws, client)) = self.ws().and_then(|w| w.client().map(|c| (w.id, c))) else { return };
                let bus = self.bus.clone();
                self.rt.spawn(async move {
                    match kube_ops::trigger_cronjob(client, ns, name).await {
                        Ok(job) => bus.send(Msg::Changed { ws, text: format!("Started job {job}") }),
                        Err(e) => bus.send(Msg::Toast { text: err_string(e), error: true }),
                    }
                });
            }
            Action::Suspend(kind, ns, name, suspend) => self.run_change(
                None,
                format!("{} {name}", if suspend { "Suspended" } else { "Resumed" }),
                move |c| kube_ops::set_suspend(c, kind, ns, name, suspend),
            ),
            Action::Pause(ns, name, paused) => self.run_change(
                None,
                format!("{} rollout of {name}", if paused { "Paused" } else { "Resumed" }),
                move |c| kube_ops::set_paused(c, ns, name, paused),
            ),
            Action::Scale(kind, ns, name, replicas) => self.run_change(
                None,
                format!("Scaled {name} to {replicas}"),
                move |c| kube_ops::scale(c, kind, ns, name, replicas),
            ),
            Action::Restart(kind, ns, name) => self.run_change(
                None,
                format!("Restarting {name}"),
                move |c| kube_ops::restart(c, kind, ns, name),
            ),
            Action::Open(tab) => {
                if let Some(ws) = self.ws_mut() {
                    ws.set_tab(tab);
                }
            }
            Action::OpenSelected(sel) => {
                if let Some(ws) = self.ws_mut() {
                    ws.set_tab(Tab::for_kind(sel.kind));
                    ws.select(sel);
                }
            }
            Action::StopForward(id) => {
                if let Some(f) = self.forwards.iter_mut().find(|f| f.id == id) {
                    if let Some(h) = f.handle.take() {
                        h.abort();
                    }
                    f.status = PfStatus::Stopped;
                    f.connections.store(0, Ordering::Relaxed);
                }
                self.save_forwards();
            }
            Action::RestartForward(id) => self.start_forward(id),
            Action::RemoveForward(id) => {
                if let Some(i) = self.forwards.iter().position(|f| f.id == id) {
                    if let Some(h) = self.forwards.remove(i).handle {
                        h.abort();
                    }
                }
                self.save_forwards();
            }
        }
    }

    /// Runs a mutating API call against a cluster tab (the active one when `ws` is `None`),
    /// then toasts and refreshes that tab on success.
    fn run_change<F, Fut>(&self, ws: Option<u64>, ok_text: String, f: F)
    where
        F: FnOnce(Client) -> Fut + Send + 'static,
        Fut: std::future::Future<Output = anyhow::Result<()>> + Send,
    {
        let target = match ws {
            Some(id) => self.workspaces.iter().find(|w| w.id == id),
            None => self.ws(),
        };
        let Some((ws, client)) = target.and_then(|w| w.client().map(|c| (w.id, c))) else { return };
        let bus = self.bus.clone();
        self.rt.spawn(async move {
            match f(client).await {
                Ok(()) => bus.send(Msg::Changed { ws, text: ok_text }),
                Err(e) => bus.send(Msg::Toast { text: err_string(e), error: true }),
            }
        });
    }

    /// The editor on a template for a new object, in the page's namespace.
    fn open_create(&mut self, kind: Kind) {
        let Some(ws) = self.ws() else { return };
        if ws.client().is_none() {
            return;
        }
        let ns = ws.selected_ns.first().cloned().unwrap_or_else(|| "default".into());
        let text = kube_ops::template(kind, &ns);
        self.yaml = Some(YamlView {
            title: format!("New {}", kind.label()),
            ws: ws.id,
            kind,
            namespace: ns,
            name: String::new(),
            text: Some(Ok(text.clone())),
            edit: Some(text),
            edit_on_load: false,
            saving: false,
            save_error: None,
            parse_error: None,
            edit_started: true,
            create: true,
        });
    }

    fn open_yaml(&mut self, kind: Kind, ns: String, name: String) {
        let Some((ws, client)) = self.ws().and_then(|w| w.client().map(|c| (w.id, c))) else { return };
        let title = format!("{} {ns}/{name}", kind.label());
        self.yaml = Some(YamlView {
            title: title.clone(),
            ws,
            kind,
            namespace: ns.clone(),
            name: name.clone(),
            text: None,
            edit: None,
            edit_on_load: false,
            saving: false,
            save_error: None,
            parse_error: None,
            edit_started: false,
            create: false,
        });
        let bus = self.bus.clone();
        self.rt.spawn(async move {
            let result = kube_ops::get_yaml(client, kind, ns, name).await.map_err(err_string);
            bus.send(Msg::Yaml { title, result });
        });
    }

    /// Opens a log tab for the active cluster, or focuses it if the same logs are already open.
    fn open_logs(&mut self, namespace: String, target: LogTarget, containers: Vec<String>, container: Option<String>) {
        let Some(ws) = self.ws() else { return };
        let Some(clients) = ws.clients().cloned() else { return };
        let context = ws.context.clone().unwrap_or_default();
        let target_key = match &target {
            LogTarget::Pod(p) => format!("pod/{p}"),
            LogTarget::Selector { selector, .. } => format!("sel/{selector}"),
            LogTarget::Owner(o) => format!("owner/{o}"),
        };
        let key = format!(
            "{}|{context}|{namespace}|{target_key}|{}",
            ws.kubeconfig_path.display(),
            container.clone().unwrap_or_default()
        );
        self.show_logs();
        if let Some(i) = self.logs.iter().position(|l| l.key == key) {
            self.active_log = i;
            return;
        }
        let mut view = LogView::new(key, context, clients, namespace, target, containers, container);
        let id = self.next_id();
        view.start(id, self.rt.handle(), &self.bus);
        self.logs.push(view);
        self.active_log = self.logs.len() - 1;
    }

    /// Opens a shell tab into a pod's container. A container already open gets a second tab:
    /// several shells into one container are normal.
    fn open_shell(&mut self, pod: PodRow, container: Option<String>) {
        let Some(ws) = self.ws() else { return };
        let Some(client) = ws.client() else { return };
        let context = ws.context.clone().unwrap_or_default();
        // Default to the first regular container, as kubectl does.
        let container = container.or_else(|| pod.containers.iter().find(|c| !c.init).map(|c| c.name.clone()));
        let id = self.next_id();
        let mut view = ShellView::new(id, context, client, pod.namespace, pod.name, container);
        view.start(self.rt.handle(), &self.bus);
        self.shells.push(view);
        self.active_log = self.logs.len() + self.shells.len() - 1;
        self.show_logs();
    }

    fn open_pf_dialog(&mut self, namespace: String, target: PfTarget, ports: Vec<u16>, remote: u16, local: u16) {
        let Some(ws) = self.ws() else { return };
        let Some(context) = ws.context.clone() else { return };
        self.pf_dialog = Some(PfDialog {
            kubeconfig: ws.kubeconfig_path.clone(),
            context,
            namespace,
            target,
            ports,
            remote_port: remote,
            local_port: local,
        });
    }

    /// Adds a forward entry (stopped). Returns the existing entry's id if the same forward exists.
    fn add_forward(&mut self, spec: SavedForward) -> u64 {
        if let Some(f) = self.forwards.iter().find(|f| f.spec.same_target(&spec)) {
            return f.id;
        }
        let id = self.next_id();
        self.forwards.push(Forward {
            id,
            spec,
            bound_port: None,
            status: PfStatus::Stopped,
            connections: Arc::new(AtomicUsize::new(0)),
            handle: None,
        });
        id
    }

    fn start_forward(&mut self, id: u64) {
        let Some(f) = self.forwards.iter_mut().find(|f| f.id == id) else { return };
        if f.handle.is_some() {
            return;
        }
        // Reuse a connected cluster tab's client when one matches; otherwise connect
        // to the forward's own kubeconfig/context in the background.
        let client = self
            .workspaces
            .iter()
            .find(|w| w.kubeconfig_path == f.spec.kubeconfig && w.context.as_ref() == Some(&f.spec.context))
            .and_then(Workspace::client);
        // Reuse the port we actually got last time so bookmarks keep working.
        if let Some(port) = f.bound_port {
            f.spec.local_port = port;
        }
        f.status = PfStatus::Starting;
        f.connections.store(0, Ordering::Relaxed);

        let spec = f.spec.clone();
        let connections = f.connections.clone();
        let bus = self.bus.clone();
        f.handle = Some(self.rt.spawn(async move {
            let event_bus = bus.clone();
            let result = async {
                let client = match client {
                    Some(c) => c,
                    None => kube_ops::connect(&spec.kubeconfig, &spec.context).await?.0.main,
                };
                kube_ops::port_forward(
                    client,
                    spec.namespace,
                    spec.target,
                    spec.remote_port,
                    spec.local_port,
                    connections,
                    move |ev| match ev {
                        PfEvent::Listening(port) => event_bus.send(Msg::PfListening { id, port }),
                        PfEvent::ConnError(e) => event_bus.send(Msg::Toast {
                            text: format!("Forward connection error: {e}"),
                            error: true,
                        }),
                    },
                )
                .await
            }
            .await;
            if let Err(e) = result {
                bus.send(Msg::PfFailed { id, error: err_string(e) });
            }
        }));
        self.save_forwards();
    }

    /// Writes the forward list to settings, marking running ones so they resume on next launch.
    fn save_forwards(&mut self) {
        self.settings.forwards = self
            .forwards
            .iter()
            .map(|f| SavedForward {
                local_port: f.bound_port.unwrap_or(f.spec.local_port),
                active: f.handle.is_some(),
                ..f.spec.clone()
            })
            .collect();
        self.settings.save();
    }

    // -----------------------------------------------------------------------
    // Rendering: chrome
    // -----------------------------------------------------------------------

    fn ui_onboarding(&mut self, ctx: &egui::Context) {
        egui::CentralPanel::default().frame(theme::content_frame()).show(ctx, |ui| {
            ui.vertical_centered(|ui| {
                ui.add_space(ui.available_height() * 0.22);
                ui.label(RichText::new(icon::STEERING_WHEEL).size(56.0).color(theme::ACCENT));
                ui.add_space(6.0);
                ui.label(theme::title("Welcome to Kube UI").size(26.0));
                ui.add_space(4.0);
                ui.label("Pick a kubeconfig file to get started. You can open more clusters as tabs later.");
                ui.add_space(18.0);

                let choose = theme::primary_button(format!("{}  Choose kubeconfig file…", icon::FOLDER_OPEN))
                    .min_size(egui::vec2(260.0, 36.0));
                if ui.add(choose).clicked() {
                    if let Some(path) = pick_kubeconfig() {
                        self.add_workspace(path, None, None);
                    }
                }
                if let Some(default) = default_kubeconfig() {
                    ui.add_space(6.0);
                    let use_default = egui::Button::new(format!("Use {}", display_path(&default)))
                        .min_size(egui::vec2(260.0, 30.0));
                    if ui.add(use_default).clicked() {
                        self.add_workspace(default, None, None);
                    }
                }
                let recent: Vec<PathBuf> = self.settings.kubeconfigs.clone();
                if !recent.is_empty() {
                    ui.add_space(14.0);
                    ui.weak("Recent");
                    for p in recent {
                        if ui.link(display_path(&p)).clicked() {
                            self.add_workspace(p, None, None);
                        }
                    }
                }
            });
        });
    }

    /// "+" menu: open any context of any known kubeconfig in a new tab.
    fn new_tab_menu(&mut self, ui: &mut egui::Ui) -> Option<(PathBuf, Option<String>)> {
        let mut picked = None;
        ui.set_min_width(240.0);
        ui.weak("Open cluster in new tab");
        for path in self.settings.kubeconfigs.clone() {
            ui.menu_button(format!("{} {}", icon::FILE, display_path(&path)), |ui| {
                match kube_ops::read_kubeconfig(&path) {
                    Ok(info) => {
                        egui::ScrollArea::vertical().max_height(360.0).show(ui, |ui| {
                            for c in info.contexts {
                                if ui.button(&c).clicked() {
                                    picked = Some((path.clone(), Some(c)));
                                }
                            }
                        });
                    }
                    Err(e) => {
                        ui.colored_label(RED, err_string(e));
                    }
                }
            });
        }
        ui.separator();
        if ui.button(format!("{} Add kubeconfig file…", icon::FOLDER_OPEN)).clicked() {
            picked = pick_kubeconfig().map(|p| (p, None));
        }
        picked
    }

    fn ui_top_bar(&mut self, ctx: &egui::Context) {
        egui::TopBottomPanel::top("top").frame(theme::chrome_frame()).show(ctx, |ui| {
            ui.add_space(8.0);
            // No gap between the tab row and the picker row, so the active tab joins it.
            ui.spacing_mut().item_spacing.y = 0.0;

            // Cluster tabs
            let mut activate = None;
            let mut close = None;
            let mut new_tab = None;
            ui.horizontal_wrapped(|ui| {
                ui.spacing_mut().item_spacing.y = 0.0;
                ui.label(RichText::new(icon::STEERING_WHEEL).size(18.0).color(theme::TEXT_MUTED));
                ui.label(theme::semibold("Kube UI").size(14.5).color(theme::TEXT));
                ui.add_space(10.0);
                let closable = self.workspaces.len() > 1;
                for (i, ws) in self.workspaces.iter().enumerate() {
                    let (color, status) = ws.conn.status();
                    let selected = i == self.active;
                    // The active tab shares the picker row's ground and opens into it.
                    egui::Frame::new()
                        .fill(if selected { theme::CONTENT } else { Color32::TRANSPARENT })
                        .corner_radius(egui::CornerRadius { nw: 7, ne: 7, sw: 0, se: 0 })
                        .inner_margin(egui::Margin { left: 12, right: 6, top: 7, bottom: 8 })
                        .show(ui, |ui| {
                            ui.horizontal(|ui| {
                                ui.spacing_mut().item_spacing.x = 6.0;
                                dot(ui, color);
                                let title = if selected {
                                    theme::semibold(ws.title()).color(theme::TEXT)
                                } else {
                                    RichText::new(ws.title()).color(theme::TEXT_MUTED)
                                };
                                let resp = ui
                                    .add(egui::Label::new(title).sense(Sense::click()))
                                    .on_hover_text(format!("{status}\n{}", display_path(&ws.kubeconfig_path)));
                                if resp.clicked() {
                                    activate = Some(i);
                                }
                                if resp.middle_clicked() && closable {
                                    close = Some(i);
                                }
                                if closable && theme::icon_button(ui, icon::X, "Close tab").clicked() {
                                    close = Some(i);
                                }
                            });
                        });
                }
                theme::icon_menu(ui, icon::PLUS, "Open another cluster in a new tab", |ui| {
                    if let Some(t) = self.new_tab_menu(ui) {
                        new_tab = Some(t);
                        ui.close();
                    }
                });
            });
            if let Some(i) = activate {
                self.activate_workspace(i);
            }
            if let Some(i) = close {
                self.close_workspace(i);
            }
            if let Some((path, context)) = new_tab {
                self.add_workspace(path, context, None);
            }

            // Pickers for the active tab, on the content ground the active tab opens into.
            let Some(ws) = self.workspaces.get_mut(self.active) else { return };
            let settings = &mut self.settings;
            egui::Frame::new()
                .fill(theme::CONTENT)
                .outer_margin(egui::Margin { left: -14, right: -14, top: 0, bottom: 0 })
                .inner_margin(egui::Margin::symmetric(14, 8))
                .show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.label(RichText::new("Config").color(theme::TEXT_MUTED));
                let active = ws.kubeconfig_path.clone();
                let mut choose: Option<PathBuf> = None;
                let mut browse = false;
                egui::ComboBox::from_id_salt(("kubeconfig", ws.id))
                    .icon(theme::caret)
                    .width(170.0)
                    .selected_text(file_label(&active))
                    .show_ui(ui, |ui| {
                        for p in &settings.kubeconfigs {
                            if ui.selectable_label(*p == active, display_path(p)).clicked() {
                                choose = Some(p.clone());
                            }
                        }
                        ui.separator();
                        if ui.button(format!("{} Add kubeconfig…", icon::PLUS)).clicked() {
                            browse = true;
                        }
                    });
                if browse {
                    choose = pick_kubeconfig();
                }
                if let Some(p) = choose.filter(|p| *p != active) {
                    ws.load_kubeconfig(p, None, settings);
                }

                ui.label(RichText::new("Context").color(theme::TEXT_MUTED));
                let contexts: Vec<(Option<String>, String)> = ws
                    .kubeconfig
                    .iter()
                    .flat_map(|k| k.contexts.iter())
                    .map(|c| (Some(c.clone()), c.clone()))
                    .collect();
                let current_context = ws.context.clone();
                let picked = searchable_combo(
                    ui,
                    &format!("context-{}", ws.id),
                    220.0,
                    current_context.as_deref().unwrap_or_default(),
                    &mut ws.context_query,
                    &contexts,
                    &current_context,
                );
                if let Some(Some(c)) = picked.filter(|c| *c != current_context) {
                    ws.switch_context(c, settings);
                }

                ui.label(RichText::new("Namespace").color(theme::TEXT_MUTED));
                let picked = namespace_picker(
                    ui,
                    &format!("namespace-{}", ws.id),
                    220.0,
                    &mut ws.namespace_query,
                    &ws.namespaces,
                    &ws.selected_ns,
                );
                if let Some(selection) = picked {
                    ws.set_namespaces(selection, settings);
                }

                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let (color, text) = ws.conn.status();
                    ui.colored_label(color, text);
                    dot(ui, color);
                    let reload = egui::Button::new(format!("{} Refresh", icon::ARROW_CLOCKWISE));
                    if ui.add_enabled(ws.client().is_some(), reload).clicked() {
                        ws.refresh();
                    }
                    if ws.is_loading() {
                        ui.spinner();
                    }
                });
            });
                });
        });
    }

    fn ui_sidebar(&mut self, ctx: &egui::Context) {
        let active_forwards = self.forwards.iter().filter(|f| f.status == PfStatus::Listening).count();
        let collapsed = &mut self.settings.nav_collapsed;
        let mut toggled = false;
        let Some(ws) = self.workspaces.get_mut(self.active) else { return };
        egui::SidePanel::left("nav")
            .resizable(false)
            .exact_width(NAV_WIDTH)
            .frame(egui::Frame::new().fill(theme::CHROME).inner_margin(egui::Margin::symmetric(10, 12)))
            .show(ctx, |ui| {
                ui.spacing_mut().item_spacing.y = 2.0;
                let mut workloads = vec![(Tab::Overview, None), (Tab::Pods, count(&ws.pods)), (Tab::Deployments, count(&ws.deployments))];
                workloads.extend(WORKLOAD_KINDS.map(|k| (Tab::Workload(k), count(ws.workload(k)))));
                let groups = [
                    ("workloads", icon::CUBE, "Workloads", workloads),
                    ("network", icon::SHARE_NETWORK, "Network", vec![(Tab::Services, count(&ws.services))]),
                ];
                egui::ScrollArea::vertical().auto_shrink([false, true]).show(ui, |ui| {
                    for (key, glyph, label, children) in groups {
                        let open = !collapsed.iter().any(|c| c == key);
                        let has_selected = children.iter().any(|(t, _)| *t == ws.tab);
                        // A closed group still shows that the current page lives inside it.
                        if theme::nav_group(ui, glyph, label, open, !open && has_selected).clicked() {
                            if open {
                                collapsed.push(key.to_string());
                            } else {
                                collapsed.retain(|c| c != key);
                            }
                            toggled = true;
                        }
                        if open {
                            for (tab, n) in children {
                                if theme::nav_child(ui, tab_label(tab), n, ws.tab == tab).clicked() {
                                    ws.set_tab(tab);
                                }
                            }
                        }
                        ui.add_space(6.0);
                    }
                    // Resource pages, then app-wide pages, separated by a quiet gap.
                    ui.add_space(8.0);
                    let app_pages = [
                        (Tab::Forwards, icon::ARROWS_LEFT_RIGHT, "Port Forwards", Some(active_forwards)),
                        (Tab::Settings, icon::GEAR, "Settings", None),
                    ];
                    for (tab, glyph, label, n) in app_pages {
                        if theme::nav_item(ui, glyph, label, n, ws.tab == tab).clicked() {
                            ws.set_tab(tab);
                        }
                    }
                });
            });
        if toggled {
            self.settings.save();
        }
    }

    // -----------------------------------------------------------------------
    // Rendering: pages
    // -----------------------------------------------------------------------

    fn ui_forwards(&mut self, ui: &mut egui::Ui, actions: &mut Vec<Action>) {
        ui.label(theme::title("Port Forwards"));
        ui.add_space(4.0);
        if self.forwards.is_empty() {
            ui.label("No port forwards yet. Select a pod or service and choose Port forward.");
            return;
        }
        TableBuilder::new(ui)
            .striped(true)
            .resizable(true)
            .cell_layout(egui::Layout::left_to_right(egui::Align::Center))
            .column(Column::initial(160.0).clip(true))
            .column(Column::initial(130.0).clip(true))
            .column(Column::initial(260.0).clip(true))
            .column(Column::initial(150.0))
            .column(Column::initial(120.0).clip(true))
            .column(Column::initial(60.0))
            .column(Column::remainder().at_least(200.0))
            .header(24.0, |mut h| {
                for t in ["Context", "Namespace", "Target", "Local / Remote", "Status", "Conns", ""] {
                    h.col(|ui| { ui.label(theme::label_small(t)); });
                }
            })
            .body(|body| {
                body.rows(ROW_HEIGHT, self.forwards.len(), |mut row| {
                    let f = &self.forwards[row.index()];
                    row.col(|ui| {
                        ui.label(&f.spec.context)
                            .on_hover_text(display_path(&f.spec.kubeconfig));
                    });
                    row.col(|ui| { ui.label(&f.spec.namespace); });
                    row.col(|ui| { ui.label(f.spec.target.label()); });
                    row.col(|ui| {
                        let local = f.bound_port.unwrap_or(f.spec.local_port);
                        ui.monospace(format!("{local} {} {}", icon::ARROW_RIGHT, f.spec.remote_port));
                    });
                    row.col(|ui| {
                        let (color, text) = match &f.status {
                            PfStatus::Starting => (YELLOW, "Starting…"),
                            PfStatus::Listening => (GREEN, "Active"),
                            PfStatus::Stopped => (theme::IDLE, "Stopped"),
                            PfStatus::Failed(_) => (RED, "Failed"),
                        };
                        dot(ui, color);
                        let resp = ui.colored_label(color, text);
                        if let PfStatus::Failed(e) = &f.status {
                            resp.on_hover_text(e);
                        }
                    });
                    row.col(|ui| { ui.label(f.connections.load(Ordering::Relaxed).to_string()); });
                    row.col(|ui| {
                        let running = f.handle.is_some();
                        if running {
                            if ui.small_button(format!("{} Stop", icon::STOP_CIRCLE)).clicked() {
                                actions.push(Action::StopForward(f.id));
                            }
                        } else if ui.small_button(format!("{} Start", icon::PLAY)).clicked() {
                            actions.push(Action::RestartForward(f.id));
                        }
                        let address = f
                            .bound_port
                            .filter(|_| f.status == PfStatus::Listening)
                            .map(|port| format!("localhost:{port}"));
                        overflow_menu(ui, |ui| {
                            if ui
                                .add_enabled(address.is_some(), egui::Button::new(format!("{} Open in browser", icon::GLOBE)))
                                .clicked()
                            {
                                if let Some(a) = &address {
                                    ui.ctx().open_url(egui::OpenUrl::new_tab(format!("http://{a}")));
                                }
                            }
                            if ui
                                .add_enabled(address.is_some(), egui::Button::new(format!("{} Copy address", icon::COPY)))
                                .clicked()
                            {
                                if let Some(a) = &address {
                                    ui.ctx().copy_text(a.clone());
                                }
                            }
                            ui.separator();
                            let remove = RichText::new(format!("{} Remove", icon::TRASH)).color(RED);
                            if ui.button(remove).clicked() {
                                actions.push(Action::RemoveForward(f.id));
                            }
                        });
                    });
                });
            });
    }

    fn ui_settings(&mut self, ui: &mut egui::Ui) {
        ui.label(theme::title("Settings"));
        ui.add_space(12.0);
        ui.label(theme::label_small("Kubeconfig files"));
        ui.add_space(4.0);

        let mut open_new = None;
        let mut use_here = None;
        let mut remove = None;
        let current = self.ws().map(|w| w.kubeconfig_path.clone());
        egui::Grid::new("kubeconfigs").striped(true).num_columns(2).spacing([16.0, 8.0]).show(ui, |ui| {
            for p in &self.settings.kubeconfigs {
                let in_use = self.workspaces.iter().filter(|w| w.kubeconfig_path == *p).count();
                let text = RichText::new(display_path(p)).monospace();
                ui.label(if in_use > 0 { text.color(GREEN) } else { text });
                ui.horizontal(|ui| {
                    if in_use > 0 {
                        ui.weak(format!("{in_use} tab{}", if in_use == 1 { "" } else { "s" }));
                    }
                    if ui.small_button("Open in new tab").clicked() {
                        open_new = Some(p.clone());
                    }
                    if current.as_ref() != Some(p) && ui.small_button("Use in this tab").clicked() {
                        use_here = Some(p.clone());
                    }
                    if ui.small_button("Remove").clicked() {
                        remove = Some(p.clone());
                    }
                });
                ui.end_row();
            }
        });
        ui.add_space(6.0);
        if ui.button(format!("{} Add kubeconfig…", icon::PLUS)).clicked() {
            open_new = pick_kubeconfig();
        }
        if let Some(p) = open_new {
            self.add_workspace(p, None, None);
        }
        if let Some(p) = use_here {
            if let Some(ws) = self.workspaces.get_mut(self.active) {
                ws.load_kubeconfig(p, None, &mut self.settings);
            }
        }
        if let Some(p) = remove {
            // Forget the file; tabs already using it stay open until closed.
            self.settings.kubeconfigs.retain(|k| k != &p);
            self.settings.save();
        }

        ui.add_space(16.0);
        if theme::checkbox(ui, &mut self.settings.live_updates, "Live updates (watch lists for changes)").changed() {
            self.settings.save();
        }
        if let Some(p) = Settings::path() {
            ui.add_space(16.0);
            ui.weak(format!("Settings stored in {}", p.display()));
        }
    }

    // -----------------------------------------------------------------------
    // Rendering: logs, windows, toasts
    // -----------------------------------------------------------------------

    fn set_log_mode(&mut self, mode: LogPanelMode) {
        if self.settings.log_mode != mode {
            self.settings.log_mode = mode;
            self.settings.save();
        }
    }

    /// Un-collapses the log panel (opening or picking a log should show it).
    fn show_logs(&mut self) {
        if self.settings.log_mode == LogPanelMode::Collapsed {
            self.set_log_mode(LogPanelMode::Normal);
        }
    }

    fn ui_logs(&mut self, ctx: &egui::Context) {
        let total = self.logs.len() + self.shells.len();
        if total == 0 {
            return;
        }
        self.active_log = self.active_log.min(total - 1);
        let n_logs = self.logs.len();
        let mut reconnect = false;
        let mut select = None;
        let mut close = None;
        let mut close_all = false;
        let mut new_mode = None;
        let mut event = None;
        let mut load_older = false;

        let mode = self.settings.log_mode;
        // Leave room for the top bar and a bit of the page when expanded.
        let max_height = (ctx.available_rect().height() - 90.0).max(LOG_MIN_HEIGHT);
        // Each mode has its own panel id: the fixed-height modes must not overwrite the
        // size egui remembers for the resizable one, so "restore" returns to it.
        let panel = match mode {
            LogPanelMode::Normal => egui::TopBottomPanel::bottom("logs")
                .resizable(true)
                .default_height(self.settings.log_height.clamp(LOG_MIN_HEIGHT, max_height))
                .min_height(LOG_MIN_HEIGHT)
                .max_height(max_height),
            LogPanelMode::Expanded => {
                egui::TopBottomPanel::bottom("logs-expanded").resizable(false).exact_height(max_height)
            }
            LogPanelMode::Collapsed => {
                egui::TopBottomPanel::bottom("logs-collapsed").resizable(false).exact_height(LOG_COLLAPSED_HEIGHT)
            }
        };

        let panel_response = panel.frame(theme::well_frame()).show(ctx, |ui| {
                ui.add_space(4.0);
                ui.horizontal(|ui| {
                    egui::ScrollArea::horizontal()
                        .id_salt("log-tabs")
                        .max_width(ui.available_width() - 110.0)
                        .show(ui, |ui| {
                            ui.horizontal(|ui| {
                                let tabs = self
                                    .logs
                                    .iter()
                                    .map(|l| (if l.is_group() { icon::STACK } else { icon::SCROLL }, l.title(), l.hover(), l.live()))
                                    .chain(self.shells.iter().map(|s| (icon::TERMINAL_WINDOW, s.title(), s.hover(), s.live())));
                                for (i, (glyph, title, hover, live)) in tabs.enumerate() {
                                    let selected = i == self.active_log;
                                    let label = format!("{glyph}  {title}");
                                    let text = match (selected, live) {
                                        (true, _) => theme::semibold(label).color(theme::TEXT),
                                        (false, true) => RichText::new(label).color(theme::TEXT_MUTED),
                                        (false, false) => RichText::new(label).color(theme::TEXT_FAINT),
                                    };
                                    // A tab holding the title and its close button.
                                    let tab = egui::Frame::new()
                                        .fill(if selected { theme::FIELD } else { Color32::TRANSPARENT })
                                        .stroke(egui::Stroke::new(1.0, if selected { theme::BORDER_STRONG } else { theme::BORDER }))
                                        .corner_radius(theme::RADIUS)
                                        .inner_margin(egui::Margin { left: 10, right: 4, top: 3, bottom: 3 })
                                        .show(ui, |ui| {
                                            ui.spacing_mut().item_spacing.x = 4.0;
                                            ui.horizontal(|ui| {
                                                let title = ui
                                                    .add(egui::Label::new(text).sense(Sense::click()))
                                                    .on_hover_text(format!("{hover}\nMiddle-click to close"));
                                                let x = theme::icon_button(ui, icon::X, "Close");
                                                (title, x)
                                            })
                                            .inner
                                        });
                                    let (title, x) = tab.inner;
                                    if x.clicked() || title.middle_clicked() {
                                        close = Some(i);
                                    } else if title.clicked() {
                                        select = Some(i);
                                    }
                                }
                            });
                        });
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if theme::icon_button(ui, icon::X_CIRCLE, "Close all tabs").clicked() {
                            close_all = true;
                        }
                        let (glyph, hint, target) = match mode {
                            LogPanelMode::Expanded => (icon::CORNERS_IN, "Restore size", LogPanelMode::Normal),
                            _ => (icon::CORNERS_OUT, "Expand", LogPanelMode::Expanded),
                        };
                        if theme::icon_button(ui, glyph, hint).clicked() {
                            new_mode = Some(target);
                        }
                        let (glyph, hint, target) = match mode {
                            LogPanelMode::Collapsed => (icon::CARET_UP, "Restore size", LogPanelMode::Normal),
                            _ => (icon::CARET_DOWN, "Collapse to bottom", LogPanelMode::Collapsed),
                        };
                        if theme::icon_button(ui, glyph, hint).clicked() {
                            new_mode = Some(target);
                        }
                    });
                });
                if mode != LogPanelMode::Collapsed {
                    ui.separator();
                    if self.active_log < n_logs {
                        if let Some(log) = self.logs.get_mut(self.active_log) {
                            event = Some(log.ui(ui, &mut load_older));
                        }
                    } else if let Some(shell) = self.shells.get_mut(self.active_log - n_logs) {
                        reconnect = shell.ui(ui);
                    }
                }
            });

        // egui stores a resizable panel's size as whatever its contents occupied, so contents a
        // few pixels too tall make it creep taller every frame. Only a drag may change the size:
        // save it when the drag ends, and put any other change back.
        if mode == LogPanelMode::Normal {
            let rect = panel_response.response.rect;
            let height = rect.height();
            let changed = (height - self.settings.log_height).abs() > 1.0;
            if ctx.input(|i| i.pointer.any_down()) {
                self.log_resizing |= changed;
            } else if self.log_resizing {
                self.log_resizing = false;
                self.settings.log_height = height;
                self.settings.save();
            } else if changed {
                let kept = egui::Rect::from_min_max(
                    egui::pos2(rect.min.x, rect.max.y - self.settings.log_height),
                    rect.max,
                );
                ctx.data_mut(|d| d.insert_persisted(egui::Id::new("logs"), egui::containers::panel::PanelState { rect: kept }));
                ctx.request_repaint();
            }
        }
        if let Some(mode) = new_mode {
            self.set_log_mode(mode);
        }

        let handle = self.rt.handle().clone();
        if let Some(event) = event.filter(|_| self.active_log < n_logs) {
            if event.restart {
                let id = self.next_id();
                if let Some(log) = self.logs.get_mut(self.active_log) {
                    log.start(id, &handle, &self.bus);
                }
            } else if load_older {
                if let Some(log) = self.logs.get_mut(self.active_log) {
                    log.load_older(&handle, &self.bus);
                }
            }
        }
        if let Some(i) = select {
            self.active_log = i;
            self.show_logs();
        }
        if reconnect {
            if let Some(shell) = self.shells.get_mut(self.active_log - n_logs) {
                shell.start(&handle, &self.bus);
            }
        }
        if let Some(i) = close {
            if i < n_logs {
                self.logs.remove(i);
            } else {
                self.shells.remove(i - n_logs);
            }
            let total = self.logs.len() + self.shells.len();
            if self.active_log > i || self.active_log >= total {
                self.active_log = self.active_log.saturating_sub(1);
            }
        }
        if close_all {
            self.logs.clear();
            self.shells.clear();
        }
    }

    fn ui_windows(&mut self, ctx: &egui::Context) {
        // YAML viewer / editor
        let mut save = None;
        let mut reload = None;
        if let Some(y) = self.yaml.as_mut() {
            let mut open = true;
            let editing = y.edit.is_some();
            let dirty = y.create || matches!((&y.edit, &y.text), (Some(e), Some(Ok(t))) if e != t);
            let mut close_window = false;
            let title = if dirty && !y.create { format!("{} (edited)", y.title) } else { y.title.clone() };
            egui::Window::new(title)
                .id(egui::Id::new("yaml-window"))
                .open(&mut open)
                .default_size([760.0, 620.0])
                .resizable(true)
                .show(ctx, |ui| {
                    let Some(text) = &y.text else {
                        ui.spinner();
                        return;
                    };
                    let text = match text {
                        Err(e) => {
                            ui.colored_label(RED, e);
                            return;
                        }
                        Ok(t) => t.clone(),
                    };
                    ui.horizontal(|ui| {
                        if editing {
                            let apply = if y.create {
                                theme::primary_button(format!("{} Create", icon::PLUS))
                            } else {
                                theme::primary_button(format!("{} Apply", icon::FLOPPY_DISK))
                            };
                            let can_apply = dirty && !y.saving && y.parse_error.is_none();
                            if ui.add_enabled(can_apply, apply).on_hover_text("Replace the object with this YAML (⌘S)").clicked() {
                                save = Some(y.edit.clone().unwrap_or_default());
                            }
                            if ui.add_enabled(!y.saving, egui::Button::new("Cancel")).clicked() {
                                if y.create {
                                    close_window = true;
                                }
                                y.edit = None;
                                y.save_error = None;
                                y.parse_error = None;
                            }
                            if y.saving {
                                ui.spinner();
                            }
                        } else {
                            if ui.add(theme::primary_button(format!("{} Edit", icon::PENCIL_SIMPLE))).clicked() {
                                y.edit = Some(text.clone());
                                y.edit_started = true;
                            }
                            if ui.button(format!("{} Copy", icon::COPY)).clicked() {
                                ui.ctx().copy_text(text.clone());
                            }
                        }
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if y.create {
                                ui.weak(format!("Namespace defaults to {}", y.namespace));
                                return;
                            }
                            if ui.add_enabled(!y.saving, egui::Button::new(format!("{} Reload", icon::ARROW_CLOCKWISE)))
                                .on_hover_text("Fetch the latest version (discards edits)")
                                .clicked()
                            {
                                reload = Some((y.kind, y.namespace.clone(), y.name.clone(), editing));
                            }
                        });
                    });
                    if editing && text.contains("app.kubernetes.io/managed-by: Helm") {
                        ui.add_space(4.0);
                        ui.colored_label(YELLOW, format!("{} Managed by Helm: the next helm upgrade will overwrite changes made here.", icon::WARNING));
                    }
                    if let Some(e) = &y.save_error {
                        ui.add_space(4.0);
                        ui.colored_label(RED, e);
                        if e.contains("modified") || e.contains("Conflict") {
                            ui.weak("Someone changed it since you opened it. Copy your edits, Reload, and apply them again.");
                        }
                    }
                    if let Some((line, msg)) = editing.then_some(()).and(y.parse_error.as_ref()) {
                        ui.add_space(4.0);
                        ui.colored_label(RED, format!("Line {line}: {msg}"));
                    }
                    ui.add_space(4.0);
                    let editor_id = egui::Id::new("yaml-editor");
                    let mut cmd_save = false;
                    egui::Frame::new().fill(theme::CONTENT).stroke(egui::Stroke::new(1.0, theme::BORDER)).corner_radius(4).show(ui, |ui| {
                        egui::ScrollArea::both().auto_shrink(false).show(ui, |ui| match y.edit.as_mut() {
                            Some(buffer) => {
                                let error_line = y.parse_error.as_ref().map(|(l, _)| *l);
                                if y.edit_started {
                                    ui.scroll_to_cursor(Some(egui::Align::TOP));
                                }
                                let out = code_editor::yaml_editor(ui, editor_id, buffer, error_line, y.edit_started);
                                y.edit_started = false;
                                if out.changed {
                                    y.parse_error = yaml_error(buffer);
                                }
                                cmd_save = out.save;
                            }
                            None => code_editor::yaml_viewer(ui, editor_id, &text),
                        });
                    });
                    if cmd_save && dirty && !y.saving && y.parse_error.is_none() {
                        save = Some(y.edit.clone().unwrap_or_default());
                    }
                });
            if !open || close_window {
                self.yaml = None;
            }
        }
        if let Some(yaml) = save {
            if let Some(y) = self.yaml.as_mut() {
                y.saving = true;
                y.save_error = None;
                let (title, ws, kind, ns, name) = (y.title.clone(), y.ws, y.kind, y.namespace.clone(), y.name.clone());
                let client = self.workspaces.iter().find(|w| w.id == ws).and_then(Workspace::client);
                let bus = self.bus.clone();
                match client {
                    Some(client) if y.create => {
                        self.rt.spawn(async move {
                            let result = kube_ops::create_yaml(client, ns, yaml).await.map_err(err_string);
                            bus.send(Msg::YamlCreated { title, result });
                        });
                    }
                    Some(client) => {
                        self.rt.spawn(async move {
                            let result = kube_ops::apply_yaml(client, kind, ns, name, yaml).await.map_err(err_string);
                            bus.send(Msg::YamlApplied { title, result });
                        });
                    }
                    None => {
                        y.saving = false;
                        y.save_error = Some("That cluster tab is no longer connected.".into());
                    }
                }
            }
        }
        if let Some((kind, ns, name, editing)) = reload {
            self.open_yaml(kind, ns, name);
            if let Some(y) = self.yaml.as_mut() {
                y.edit_on_load = editing;
            }
        }

        // Port-forward dialog
        let mut start = None;
        if let Some(d) = self.pf_dialog.as_mut() {
            let mut open = true;
            let mut cancel = false;
            egui::Window::new("Start port forward")
                .open(&mut open)
                .collapsible(false)
                .resizable(false)
                .anchor(Align2::CENTER_CENTER, [0.0, 0.0])
                .show(ctx, |ui| {
                    egui::Grid::new("pf-grid").num_columns(2).spacing([12.0, 10.0]).show(ui, |ui| {
                        ui.label("Cluster");
                        ui.label(&d.context);
                        ui.end_row();

                        ui.label("Target");
                        ui.label(theme::semibold(format!("{}/{}", d.namespace, d.target.label())));
                        ui.end_row();

                        ui.label("Remote port");
                        ui.horizontal(|ui| {
                            for p in &d.ports {
                                ui.selectable_value(&mut d.remote_port, *p, p.to_string());
                            }
                            ui.add(egui::DragValue::new(&mut d.remote_port).range(1..=65535));
                        });
                        ui.end_row();

                        ui.label("Local port");
                        ui.horizontal(|ui| {
                            ui.add(egui::DragValue::new(&mut d.local_port).range(0..=65535));
                            ui.weak("0 = random");
                        });
                        ui.end_row();
                    });
                    ui.add_space(8.0);
                    ui.horizontal(|ui| {
                        if ui.add(theme::primary_button(format!("{} Start forward", icon::PLAY))).clicked() {
                            start = Some(SavedForward {
                                kubeconfig: d.kubeconfig.clone(),
                                context: d.context.clone(),
                                namespace: d.namespace.clone(),
                                target: d.target.clone(),
                                remote_port: d.remote_port,
                                local_port: d.local_port,
                                active: false,
                            });
                        }
                        if ui.button("Cancel").clicked() {
                            cancel = true;
                        }
                    });
                });
            if !open || cancel || start.is_some() {
                self.pf_dialog = None;
            }
        }
        if let Some(spec) = start {
            let id = self.add_forward(spec);
            self.start_forward(id);
            if let Some(ws) = self.ws_mut() {
                ws.set_tab(Tab::Forwards);
            }
        }

        // Delete confirmation
        if let Some((ws, kind, ns, name)) = self.confirm_delete.clone() {
            let mut decided = None;
            let label = kind.label();
            egui::Window::new(format!("Delete {label}?"))
                .collapsible(false)
                .resizable(false)
                .anchor(Align2::CENTER_CENTER, [0.0, 0.0])
                .show(ctx, |ui| {
                    ui.label(format!("Delete {label} {ns}/{name}?"));
                    let note = match kind {
                        Kind::Pod => "If it's managed by a controller, a replacement will be created.",
                        Kind::Service => "Traffic to it stops right away.",
                        Kind::CronJob => "Its jobs and their pods are deleted too.",
                        _ => "Its pods are deleted too. This can't be undone.",
                    };
                    ui.weak(note);
                    ui.add_space(8.0);
                    ui.horizontal(|ui| {
                        if ui.add(theme::danger_button(format!("{} Delete {label}", icon::TRASH))).clicked() {
                            decided = Some(true);
                        }
                        if ui.button("Cancel").clicked() {
                            decided = Some(false);
                        }
                    });
                });
            match decided {
                Some(true) => {
                    self.confirm_delete = None;
                    if let Some(w) = self.workspaces.iter_mut().find(|w| w.id == ws) {
                        w.close_drawer();
                    }
                    self.run_change(Some(ws), format!("Deleted {} {name}", label.to_lowercase()), move |c| {
                        kube_ops::delete(c, kind, ns, name)
                    });
                }
                Some(false) => self.confirm_delete = None,
                None => {}
            }
        }
    }

    fn ui_toasts(&mut self, ctx: &egui::Context) {
        self.toasts.retain(|t| t.at.elapsed() < TOAST_FOR);
        if self.toasts.is_empty() {
            return;
        }
        egui::Area::new(egui::Id::new("toasts"))
            .anchor(Align2::RIGHT_BOTTOM, [-16.0, -16.0])
            .order(egui::Order::Foreground)
            .show(ctx, |ui| {
                ui.set_max_width(420.0);
                for t in &self.toasts {
                    theme::card_frame().show(ui, |ui| {
                        let (color, glyph) = if t.error {
                            (RED, icon::WARNING_CIRCLE)
                        } else {
                            (GREEN, icon::CHECK_CIRCLE)
                        };
                        ui.horizontal_top(|ui| {
                            ui.label(RichText::new(glyph).size(16.0).color(color));
                            ui.add(egui::Label::new(RichText::new(&t.text).color(theme::TEXT)).wrap());
                        });
                    });
                    ui.add_space(6.0);
                }
            });
        ctx.request_repaint_after(Duration::from_millis(500));
    }
}

impl eframe::App for App {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.handle_messages();
        self.track_window(ctx);

        if self.workspaces.is_empty() {
            self.ui_onboarding(ctx);
            self.ui_toasts(ctx);
            self.save_workspaces();
            return;
        }
        self.active = self.active.min(self.workspaces.len() - 1);

        // Only the front tab watches its list; background tabs catch up when shown.
        let live = self.settings.live_updates;
        let active = self.active;
        for (i, ws) in self.workspaces.iter_mut().enumerate() {
            if i == active {
                ws.sync_watch(live);
            } else {
                ws.pause_watch();
            }
        }
        let tab = self.ws().map(|w| w.tab).unwrap_or(Tab::Pods);
        if let Some(ws) = self.ws_mut() {
            if live && ws.client().is_some() {
                ws.poll_details(DETAILS_EVERY);
                ctx.request_repaint_after(DETAILS_EVERY);
            }
        }
        if tab.is_list() || tab == Tab::Overview {
            // Ages are computed when drawn; keep the seconds ticking.
            ctx.request_repaint_after(Duration::from_secs(1));
        }
        if tab == Tab::Forwards && !self.forwards.is_empty() {
            // Keep connection counts fresh.
            ctx.request_repaint_after(Duration::from_secs(1));
        }

        let mut actions = Vec::new();
        self.ui_top_bar(ctx);
        self.ui_sidebar(ctx);
        self.ui_logs(ctx);
        if tab.is_list() {
            if let Some(ws) = self.workspaces.get_mut(self.active) {
                ws.ui_drawer(ctx, &mut actions);
            }
        }

        egui::CentralPanel::default().frame(theme::content_frame()).show(ctx, |ui| match tab {
            Tab::Forwards => self.ui_forwards(ui, &mut actions),
            Tab::Settings => self.ui_settings(ui),
            _ => {
                if let Some(ws) = self.workspaces.get_mut(self.active) {
                    match tab {
                        Tab::Overview => ws.ui_overview(ui, &mut actions),
                        Tab::Pods => ws.ui_pods(ui, &mut actions),
                        Tab::Services => ws.ui_services(ui, &mut actions),
                        Tab::Workload(kind) => ws.ui_workloads(ui, kind, &mut actions),
                        _ => ws.ui_deployments(ui, &mut actions),
                    }
                }
            }
        });
        for a in actions {
            self.apply(a);
        }

        self.ui_windows(ctx);
        self.ui_toasts(ctx);
        self.save_workspaces();
        self.dev_screenshot(ctx);
    }

    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        if self.window_dirty.take().is_some() {
            self.settings.save();
        }
    }
}

// ---------------------------------------------------------------------------
// Widgets
// ---------------------------------------------------------------------------

/// A combo box with a search field. Returns the picked value, if any.
fn searchable_combo(
    ui: &mut egui::Ui,
    id: &str,
    width: f32,
    selected_text: &str,
    query: &mut String,
    options: &[(Option<String>, String)],
    current: &Option<String>,
) -> Option<Option<String>> {
    let mut picked = None;
    let response = egui::ComboBox::from_id_salt(id)
        .icon(theme::caret)
        .width(width)
        .selected_text(selected_text)
        .close_behavior(PopupCloseBehavior::CloseOnClickOutside)
        .show_ui(ui, |ui| {
            ui.set_min_width(width.max(240.0));
            let edit = ui.add(
                egui::TextEdit::singleline(query)
                    .hint_text(format!("{} Search…", icon::MAGNIFYING_GLASS))
                    .desired_width(f32::INFINITY),
            );
            if !edit.has_focus() && ui.memory(|m| m.focused().is_none()) {
                edit.request_focus();
            }
            let q = query.to_lowercase();
            let matches: Vec<&(Option<String>, String)> =
                options.iter().filter(|(_, label)| label.to_lowercase().contains(&q)).collect();
            if edit.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                picked = matches.first().map(|(value, _)| value.clone());
            }
            ui.separator();
            egui::ScrollArea::vertical()
                .max_height(340.0)
                .auto_shrink([false, true])
                .show(ui, |ui| {
                    for (value, label) in &matches {
                        if ui.selectable_label(value == current, label).clicked() {
                            picked = Some(value.clone());
                        }
                    }
                    if matches.is_empty() {
                        ui.weak("No matches");
                    }
                });
            if picked.is_some() {
                ui.close();
            }
        });
    if response.inner.is_none() || picked.is_some() {
        query.clear();
    }
    picked
}

/// A "⋯" button that opens a menu of secondary actions. Any click inside closes it.
/// "ns", "a, b", "4 namespaces" or "All namespaces" (`short` gives "all" for the tab strip).
fn namespaces_label(selected: &[String], short: bool) -> String {
    match selected.len() {
        0 if short => "all".into(),
        0 => "All namespaces".into(),
        1 => selected[0].clone(),
        2 => selected.join(", "),
        n if short => format!("{n} ns"),
        n => format!("{n} namespaces"),
    }
}

/// Namespace picker with search. Clicking a name selects only that namespace; ticking a
/// checkbox adds it to (or removes it from) the selection and keeps the list open.
/// Returns the new selection (empty = all namespaces) when it changed.
fn namespace_picker(
    ui: &mut egui::Ui,
    id: &str,
    width: f32,
    query: &mut String,
    options: &[String],
    selected: &[String],
) -> Option<Vec<String>> {
    let mut picked: Option<Vec<String>> = None;
    let mut close = false;
    let response = egui::ComboBox::from_id_salt(id)
        .icon(theme::caret)
        .width(width)
        .selected_text(namespaces_label(selected, false))
        .close_behavior(PopupCloseBehavior::CloseOnClickOutside)
        .show_ui(ui, |ui| {
            ui.set_min_width(width.max(280.0));
            let edit = ui.add(
                egui::TextEdit::singleline(query)
                    .hint_text(format!("{} Search…", icon::MAGNIFYING_GLASS))
                    .desired_width(f32::INFINITY),
            );
            if !edit.has_focus() && ui.memory(|m| m.focused().is_none()) {
                edit.request_focus();
            }
            let q = query.to_lowercase();
            let matches: Vec<&String> = options.iter().filter(|n| n.to_lowercase().contains(&q)).collect();
            if edit.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                if let Some(first) = matches.first() {
                    picked = Some(vec![(*first).clone()]);
                    close = true;
                }
            }
            ui.horizontal(|ui| {
                ui.weak(RichText::new("Tick to add · click a name to pick only it").small());
                if selected.len() > 1 && ui.small_button("Clear").clicked() {
                    picked = Some(vec![selected[0].clone()]);
                }
            });
            ui.separator();
            egui::ScrollArea::vertical()
                .max_height(360.0)
                .auto_shrink([false, true])
                .show(ui, |ui| {
                    if q.is_empty() && ui.selectable_label(selected.is_empty(), "All namespaces").clicked() {
                        picked = Some(Vec::new());
                        close = true;
                    }
                    for ns in &matches {
                        ui.horizontal(|ui| {
                            let mut checked = selected.contains(ns);
                            if theme::checkbox(ui, &mut checked, "").on_hover_text("Add to selection").changed() {
                                let mut next: Vec<String> = selected.to_vec();
                                if checked {
                                    next.push((*ns).clone());
                                } else {
                                    next.retain(|n| n != *ns);
                                }
                                // Unticking the last one keeps it rather than jumping to "all".
                                if !next.is_empty() {
                                    picked = Some(next);
                                }
                            }
                            let only = selected.len() == 1 && selected[0] == **ns;
                            // Full-width so the whole row is clickable; the empty `right_text`
                            // inserts a flexible gap, which left-aligns the name.
                            let label = egui::Button::selectable(only, ns.as_str())
                                .right_text("")
                                .min_size(egui::vec2(ui.available_width(), 0.0));
                            if ui.add(label).clicked() {
                                picked = Some(vec![(*ns).clone()]);
                                close = true;
                            }
                        });
                    }
                    if matches.is_empty() {
                        ui.weak("No matches");
                    }
                });
            if close {
                ui.close();
            }
        });
    if response.inner.is_none() || close {
        query.clear();
    }
    picked
}

fn overflow_menu(ui: &mut egui::Ui, add_contents: impl FnOnce(&mut egui::Ui)) {
    // Framed like the secondary buttons it usually sits beside, so it reads as a control.
    let resp = ui.button(icon::DOTS_THREE).on_hover_text("More actions");
    egui::Popup::menu(&resp).show(|ui| {
        ui.set_min_width(180.0);
        add_contents(ui);
    });
}

fn sub_section(ui: &mut egui::Ui, title: &str) {
    ui.add_space(8.0);
    ui.label(theme::label_small(title));
}

fn code_block(ui: &mut egui::Ui, text: &str) {
    egui::Frame::new()
        .fill(ui.visuals().extreme_bg_color)
        .corner_radius(4.0)
        .inner_margin(egui::Margin::same(6))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.add(egui::Label::new(RichText::new(text).monospace().small()).wrap());
        });
}

/// Shows a value in monospace, wrapping, with long values cut short (full value on hover).
fn value_label(ui: &mut egui::Ui, value: &str) {
    const MAX: usize = 400;
    if value.is_empty() {
        ui.weak("(empty)");
    } else if value.chars().count() > MAX {
        let short: String = value.chars().take(MAX).collect();
        ui.add(egui::Label::new(RichText::new(format!("{short}…")).monospace()).wrap())
            .on_hover_text(value);
    } else {
        ui.add(egui::Label::new(RichText::new(value).monospace()).wrap());
    }
}

fn env_item(ui: &mut egui::Ui, name: &str, value: Option<&str>, source: Option<&str>) {
    ui.horizontal_wrapped(|ui| {
        ui.label(RichText::new(name).monospace().strong());
        if let Some(s) = source {
            ui.weak(RichText::new(s).small());
        }
    });
    match value {
        Some(v) => value_label(ui, v),
        None => {
            ui.weak("set at runtime");
        }
    }
}

fn resources_grid(ui: &mut egui::Ui, id: &str, requests: &[(String, String)], limits: &[(String, String)]) {
    let mut names: Vec<&String> = requests.iter().chain(limits.iter()).map(|(k, _)| k).collect();
    names.sort();
    names.dedup();
    let find = |list: &[(String, String)], name: &str| {
        list.iter().find(|(k, _)| k == name).map(|(_, v)| v.clone()).unwrap_or_else(|| "-".into())
    };
    egui::Grid::new(id).num_columns(3).spacing([24.0, 4.0]).show(ui, |ui| {
        ui.weak("");
        ui.weak("Request");
        ui.weak("Limit");
        ui.end_row();
        for name in names {
            ui.label(name.as_str());
            ui.monospace(find(requests, name));
            ui.monospace(find(limits, name));
            ui.end_row();
        }
    });
}

fn conditions(ui: &mut egui::Ui, list: &[details::Condition]) {
    if list.is_empty() {
        return;
    }
    section(ui, "Conditions");
    for c in list {
        ui.horizontal_wrapped(|ui| {
            dot(ui, if c.ok { GREEN } else { YELLOW });
            let resp = ui.label(&c.type_);
            if !c.message.is_empty() {
                resp.on_hover_text(&c.message);
            }
            if !c.reason.is_empty() {
                ui.weak(&c.reason);
            }
        });
    }
}

fn annotations(ui: &mut egui::Ui, id: &str, list: &[(String, String)]) {
    if list.is_empty() {
        return;
    }
    ui.add_space(10.0);
    egui::CollapsingHeader::new(theme::label_small(format!("Annotations ({})", list.len())))
        .id_salt(id)
        .show(ui, |ui| {
            for (k, v) in list {
                ui.label(RichText::new(k).small().weak());
                value_label(ui, v);
                ui.add_space(4.0);
            }
        });
}

/// Quotes a command-line word for display when it contains spaces or shell metacharacters.
fn shell_quote(word: &str) -> String {
    if !word.is_empty() && word.chars().all(|c| c.is_ascii_alphanumeric() || "-_./=:,@%+".contains(c)) {
        word.to_string()
    } else {
        format!("'{}'", word.replace('\'', "'\\''"))
    }
}

fn dot(ui: &mut egui::Ui, color: Color32) {
    let (rect, _) = ui.allocate_exact_size(egui::vec2(10.0, 10.0), Sense::hover());
    ui.painter().circle_filled(rect.center(), 4.0, color);
}

fn section(ui: &mut egui::Ui, title: &str) {
    ui.add_space(16.0);
    ui.label(theme::label_small(title));
    ui.add_space(4.0);
}

/// A resource name that fits its cell: Kubernetes names differ at the end (the
/// ReplicaSet hash and pod suffix), so shorten from the middle and keep the tail.
/// The full name shows on hover.
fn name_cell(ui: &mut egui::Ui, name: &str) {
    let font = egui::TextStyle::Monospace.resolve(ui.style());
    let char_width = ui.fonts_mut(|f| f.glyph_width(&font, 'm')).max(1.0);
    let fits = ((ui.available_width() - 4.0) / char_width).floor().max(8.0) as usize;
    let chars: Vec<char> = name.chars().collect();
    if chars.len() <= fits {
        ui.label(mono(name));
        return;
    }
    let tail = (fits / 3).clamp(5, 16);
    let head = fits.saturating_sub(tail + 1);
    let short: String = chars[..head].iter().chain(['…'].iter()).chain(chars[chars.len() - tail..].iter()).collect();
    ui.label(mono(short)).on_hover_text(name);
}

/// Names and data cells: JetBrains Mono, which also gives tabular figures.
fn mono(text: impl Into<String>) -> RichText {
    RichText::new(text).monospace().color(theme::TEXT)
}

fn mono_muted(text: impl Into<String>) -> RichText {
    RichText::new(text).monospace().color(theme::TEXT_MUTED)
}

/// Restarts on a fixed severity ramp: none, tinted amber, filled red at five or more.
fn restarts_badge(ui: &mut egui::Ui, restarts: i32) {
    match restarts {
        0 => {
            ui.label(mono("0").color(theme::TEXT_FAINT));
        }
        1..=4 => {
            theme::badge(ui, &restarts.to_string(), theme::WARN, false);
        }
        _ => {
            theme::badge(ui, &restarts.to_string(), theme::BAD, true);
        }
    }
}

fn details_grid(ui: &mut egui::Ui, id: &str, rows: &[(&str, &str)]) {
    egui::Grid::new(id).num_columns(2).spacing([16.0, 7.0]).min_col_width(110.0).show(ui, |ui| {
        for (key, value) in rows {
            ui.label(RichText::new(*key).color(theme::TEXT_MUTED));
            let value = if value.is_empty() { "-" } else { value };
            ui.add(egui::Label::new(mono(value)).truncate()).on_hover_text(value);
            ui.end_row();
        }
    });
}

fn tags(ui: &mut egui::Ui, items: impl Iterator<Item = String>) {
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing = egui::vec2(4.0, 4.0);
        for item in items {
            tag(ui, &item);
        }
    });
}

/// A pill-shaped label. Its size is measured up front so a wrapping layout moves the
/// whole tag to the next row instead of squeezing the text into the leftover space.
/// Only text longer than a full row wraps inside the tag. Click copies it.
fn tag(ui: &mut egui::Ui, text: &str) {
    let padding = egui::vec2(6.0, 2.0);
    let wrap_width = (ui.max_rect().width() - 2.0 * padding.x).max(40.0);
    let galley = egui::WidgetText::from(RichText::new(text).small().monospace()).into_galley(
        ui,
        Some(egui::TextWrapMode::Wrap),
        wrap_width,
        egui::TextStyle::Small,
    );
    let (rect, response) = ui.allocate_exact_size(galley.size() + 2.0 * padding, Sense::click());
    if ui.is_rect_visible(rect) {
        let visuals = ui.visuals();
        let fill = if response.hovered() { visuals.widgets.hovered.weak_bg_fill } else { visuals.faint_bg_color };
        ui.painter().rect(
            rect,
            4.0,
            fill,
            visuals.widgets.noninteractive.bg_stroke,
            egui::StrokeKind::Inside,
        );
        ui.painter().galley(rect.min + padding, galley, visuals.text_color());
    }
    if response.on_hover_text("Click to copy").clicked() {
        ui.ctx().copy_text(text.to_string());
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Where an edited YAML stops parsing, if it does.
fn yaml_error(text: &str) -> Option<(usize, String)> {
    let e = serde_yaml::from_str::<serde_yaml::Value>(text).err()?;
    let line = e.location().map(|l| l.line()).unwrap_or(1);
    // serde_yaml appends " at line N column M"; the gutter already shows the line.
    let msg = e.to_string();
    let msg = msg.split(" at line ").next().unwrap_or(&msg).to_string();
    Some((line, msg))
}

/// A table's sorted column (by header label) and direction.
#[derive(Clone, PartialEq, Debug)]
struct Sort {
    col: String,
    asc: bool,
}

impl Sort {
    fn saved(&self) -> SavedSort {
        SavedSort { column: self.col.clone(), descending: !self.asc }
    }
}

/// What a row sorts by in one column.
enum SortKey {
    Text(String),
    Num(i64),
}

impl SortKey {
    fn age(t: Option<&k8s_openapi::apimachinery::pkg::apis::meta::v1::Time>) -> Self {
        // Unknown ages sort as the oldest.
        SortKey::Num(kube_ops::age_secs(t).unwrap_or(i64::MAX))
    }
    fn text(s: &str) -> Self {
        SortKey::Text(s.to_string())
    }
}

fn cmp_keys(a: &SortKey, b: &SortKey) -> std::cmp::Ordering {
    match (a, b) {
        (SortKey::Num(x), SortKey::Num(y)) => x.cmp(y),
        (SortKey::Text(x), SortKey::Text(y)) => natural_cmp(x, y),
        (SortKey::Num(_), SortKey::Text(_)) => std::cmp::Ordering::Less,
        (SortKey::Text(_), SortKey::Num(_)) => std::cmp::Ordering::Greater,
    }
}

/// Case-insensitive, with digit runs compared as numbers: "pod-9" < "pod-10", "9/10" < "10/10".
fn natural_cmp(a: &str, b: &str) -> std::cmp::Ordering {
    use std::cmp::Ordering;
    let (mut a, mut b) = (a.chars().peekable(), b.chars().peekable());
    loop {
        match (a.peek().copied(), b.peek().copied()) {
            (None, None) => return Ordering::Equal,
            (None, Some(_)) => return Ordering::Less,
            (Some(_), None) => return Ordering::Greater,
            (Some(x), Some(y)) if x.is_ascii_digit() && y.is_ascii_digit() => {
                let take = |it: &mut std::iter::Peekable<std::str::Chars>| {
                    let mut s = String::new();
                    while let Some(c) = it.peek().copied().filter(char::is_ascii_digit) {
                        s.push(c);
                        it.next();
                    }
                    s
                };
                let (na, nb) = (take(&mut a), take(&mut b));
                let (ta, tb) = (na.trim_start_matches('0'), nb.trim_start_matches('0'));
                let o = ta.len().cmp(&tb.len()).then_with(|| ta.cmp(tb));
                if o != Ordering::Equal {
                    return o;
                }
            }
            (Some(x), Some(y)) => {
                let o = x.to_lowercase().cmp(y.to_lowercase());
                if o != Ordering::Equal {
                    return o;
                }
                a.next();
                b.next();
            }
        }
    }
}

/// Sorts rows in place (stable, so ties keep namespace/name order).
fn sort_rows<T>(rows: &mut [&T], sort: Option<&Sort>, key: impl Fn(&T, &str) -> SortKey) {
    let Some(s) = sort else { return };
    rows.sort_by(|a, b| {
        let o = cmp_keys(&key(a, &s.col), &key(b, &s.col));
        if s.asc { o } else { o.reverse() }
    });
}

/// A clickable column header: click to sort ascending, again for descending, again to clear.
fn sort_header(ui: &mut egui::Ui, label: &str, sort: Option<&Sort>, clicked: &mut Option<String>) {
    let active = sort.filter(|s| s.col == label);
    let text = match active {
        Some(s) => format!("{label} {}", if s.asc { icon::CARET_UP } else { icon::CARET_DOWN }),
        None => label.to_string(),
    };
    let color = if active.is_some() { theme::TEXT } else { theme::TEXT_MUTED };
    let resp = ui
        .add(egui::Label::new(theme::label_small(text).color(color)).sense(Sense::click()).selectable(false))
        .on_hover_cursor(egui::CursorIcon::PointingHand);
    let hint = match active {
        Some(s) if s.asc => format!("Sorted by {label}, ascending. Click for descending."),
        Some(_) => format!("Sorted by {label}, descending. Click to clear."),
        None => format!("Sort by {label}"),
    };
    if resp.on_hover_text(hint).clicked() {
        *clicked = Some(label.to_string());
    }
}

fn tab_label(tab: Tab) -> &'static str {
    match tab {
        Tab::Overview => "Overview",
        Tab::Pods => "Pods",
        Tab::Services => "Services",
        Tab::Deployments => "Deployments",
        Tab::Workload(k) => k.plural(),
        Tab::Forwards => "Port Forwards",
        Tab::Settings => "Settings",
    }
}

fn health_color(h: Health) -> Color32 {
    match h {
        Health::Ok => GREEN,
        Health::Progress => YELLOW,
        Health::Bad => RED,
        Health::Idle => theme::IDLE,
    }
}

fn count<T>(s: &ListState<T>) -> Option<usize> {
    s.loaded.then_some(s.items.len())
}

fn status_color(status: &str) -> Color32 {
    match status {
        "Running" | "Succeeded" | "Completed" => GREEN,
        "Pending" | "ContainerCreating" | "PodInitializing" | "Terminating" | "Waiting" => YELLOW,
        s if s.starts_with("Init:") => YELLOW,
        _ => RED,
    }
}

fn default_kubeconfig() -> Option<PathBuf> {
    if let Some(first) = std::env::var_os("KUBECONFIG")
        .and_then(|v| std::env::split_paths(&v).next())
        .filter(|p| p.is_file())
    {
        return Some(first);
    }
    dirs::home_dir().map(|h| h.join(".kube").join("config")).filter(|p| p.is_file())
}

fn pick_kubeconfig() -> Option<PathBuf> {
    let mut dialog = rfd::FileDialog::new().set_title("Select kubeconfig file");
    if let Some(dir) = dirs::home_dir().map(|h| h.join(".kube")).filter(|d| d.is_dir()) {
        dialog = dialog.set_directory(dir);
    }
    dialog.pick_file()
}

fn file_label(p: &Path) -> String {
    p.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default()
}

/// Shortens the home directory to `~` for display.
fn display_path(p: &Path) -> String {
    if let Some(home) = dirs::home_dir() {
        if let Ok(rest) = p.strip_prefix(&home) {
            return format!("~/{}", rest.display());
        }
    }
    p.display().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cmp::Ordering::*;

    #[test]
    fn natural_order() {
        assert_eq!(natural_cmp("pod-9", "pod-10"), Less);
        assert_eq!(natural_cmp("9/10", "10/10"), Less);
        assert_eq!(natural_cmp("App", "apple"), Less);
        assert_eq!(natural_cmp("a-01", "a-1"), Equal);
        assert_eq!(natural_cmp("10.43.70.9", "10.43.70.224"), Less);
        assert_eq!(natural_cmp("abc", "abc-1"), Less);
    }

    #[test]
    fn mixed_keys_put_numbers_first() {
        assert_eq!(cmp_keys(&SortKey::Num(5), &SortKey::text("x")), Less);
        assert_eq!(cmp_keys(&SortKey::Num(2), &SortKey::Num(10)), Less);
    }
}
