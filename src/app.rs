mod changelog;
mod code_editor;
mod filter;
mod icons;
mod logs;
mod menu;
mod shell;
mod table;
mod theme;
mod update;
mod view;
mod workspace;
mod yaml;

use std::{
    collections::{BTreeMap, HashMap, HashSet, VecDeque},
    path::{Path, PathBuf},
    sync::{
        Arc, LazyLock,
        atomic::{AtomicUsize, Ordering},
        mpsc::{self, Receiver, Sender},
    },
    time::{Duration, Instant},
};

use gpui_kit::{
    self as gpui, AppContext as _, Context, Entity, Global, InteractiveElement as _,
    IntoElement, Keystroke, ParentElement as _, Render, Styled as _, WeakEntity, Window,
};
use kube::Client;
use tokio::{
    runtime::{Handle, Runtime},
    sync::Notify,
    task::JoinHandle,
};

use crate::{
    details::{self, Body, ContainerDetails, Details, EnvSource},
    kube_ops::{
        self, Clients, DeployRow, Health, Kind, KubeconfigInfo, LogLine, LogOptions, PfEvent,
        PfTarget, PodRow, SvcRow, WorkRow,
    },
    settings::{LogPanelMode, SavedForward, SavedSort, SavedWorkspace, Settings},
};
use icons as icon;
use logs::{LogTarget, LogView};
use shell::ShellView;
use theme::Color;
use workspace::Workspace;
use yaml::YamlView;

/// How often the open drawer's details are re-fetched while live updates are on.
/// Lists themselves don't poll: they're kept current by a watch.
const DETAILS_EVERY: Duration = Duration::from_secs(5);
const MAX_LOG_LINES: usize = 100_000;
/// How many extra lines to request each time the user scrolls to the top of the logs.
const LOG_PAGE: usize = 1_000;
const TOAST_FOR: Duration = Duration::from_secs(5);
const ROW_HEIGHT: f32 = 36.0;
const NAV_WIDTH: f32 = 224.0;
const LOG_MIN_HEIGHT: f32 = 160.0;
/// Height of the log panel when collapsed to its tab strip.
const LOG_COLLAPSED_HEIGHT: f32 = 44.0;
const DRAWER_MIN_WIDTH: f32 = 340.0;
pub const MIN_WINDOW_SIZE: Size = Size::new(1120.0, 600.0);
pub const DEFAULT_WINDOW_SIZE: Size = Size::new(1360.0, 860.0);

// ---------------------------------------------------------------------------
// Geometry (logical pixels, as plain f32)
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, PartialEq, Debug, Default)]
pub struct Point {
    pub x: f32,
    pub y: f32,
}

impl Point {
    pub const ORIGIN: Point = Point { x: 0.0, y: 0.0 };
    pub const fn new(x: f32, y: f32) -> Self {
        Self { x, y }
    }
}

impl From<gpui::Point<gpui::Pixels>> for Point {
    fn from(p: gpui::Point<gpui::Pixels>) -> Self {
        Self::new(f32::from(p.x), f32::from(p.y))
    }
}

#[derive(Clone, Copy, PartialEq, Debug, Default)]
pub struct Size {
    pub width: f32,
    pub height: f32,
}

impl Size {
    pub const fn new(width: f32, height: f32) -> Self {
        Self { width, height }
    }
}

impl From<gpui::Size<gpui::Pixels>> for Size {
    fn from(s: gpui::Size<gpui::Pixels>) -> Self {
        Self::new(f32::from(s.width), f32::from(s.height))
    }
}

#[derive(Clone, Copy, PartialEq, Debug, Default)]
pub struct Rectangle {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
}

impl Rectangle {
    pub fn new(origin: Point, size: Size) -> Self {
        Self {
            x: origin.x,
            y: origin.y,
            width: size.width,
            height: size.height,
        }
    }
}

impl From<gpui::Bounds<gpui::Pixels>> for Rectangle {
    fn from(b: gpui::Bounds<gpui::Pixels>) -> Self {
        Self::new(b.origin.into(), b.size.into())
    }
}

// ---------------------------------------------------------------------------
// Follow-up work (what iced called a Task)
// ---------------------------------------------------------------------------

/// Follow-up work an update asks for: more messages, or something that needs the window
/// (focus, clipboard, quitting). Returned from handlers and run by [`App::dispatch`].
#[must_use]
#[derive(Default)]
pub struct Task {
    effects: Vec<Effect>,
}

enum Effect {
    Msg(Message),
    Copy(String),
    Quit,
    Window(Box<dyn FnOnce(&mut App, &mut Window, &mut Context<App>)>),
}

impl Task {
    pub fn none() -> Self {
        Self::default()
    }
    /// Handle `msg` next.
    pub fn done(msg: Message) -> Self {
        Self {
            effects: vec![Effect::Msg(msg)],
        }
    }
    pub fn batch(tasks: impl IntoIterator<Item = Task>) -> Self {
        Self {
            effects: tasks.into_iter().flat_map(|t| t.effects).collect(),
        }
    }
    /// Put `text` on the clipboard.
    pub fn copy(text: String) -> Self {
        Self {
            effects: vec![Effect::Copy(text)],
        }
    }
    pub fn quit() -> Self {
        Self {
            effects: vec![Effect::Quit],
        }
    }
    /// Run `f` with the window, e.g. to move focus: `Task::window(|app, window, cx| …)`.
    pub fn window(f: impl FnOnce(&mut App, &mut Window, &mut Context<App>) + 'static) -> Self {
        Self {
            effects: vec![Effect::Window(Box::new(f))],
        }
    }
}

// ---------------------------------------------------------------------------
// Sending messages from views
// ---------------------------------------------------------------------------

/// The app's entity, so event handlers anywhere can reach it (see [`on`]).
struct AppHandle(WeakEntity<App>);

impl Global for AppHandle {}

/// Handles `msg` in the app (from an event handler, a subscription or an async task
/// that has the window).
pub fn send(msg: Message, window: &mut Window, cx: &mut gpui::App) {
    let Some(app) = cx.try_global::<AppHandle>().and_then(|h| h.0.upgrade()) else {
        return;
    };
    app.update(cx, |app, cx| app.dispatch(msg, window, cx));
}

/// An event handler that sends `msg`: `.on_click(on(Message::Refresh))`.
pub fn on<E: ?Sized>(msg: Message) -> impl Fn(&E, &mut Window, &mut gpui::App) + 'static {
    move |_, window, cx| send(msg.clone(), window, cx)
}

const GREEN: Color = theme::OK;
const YELLOW: Color = theme::WARN;
const RED: Color = theme::BAD;

fn err_string(e: anyhow::Error) -> String {
    kube_ops::describe_error(&e)
}

// ---------------------------------------------------------------------------
// Messages from background tasks
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

/// Results from background tasks. `ws` is the cluster tab a message belongs to; `id` is a
/// log tab's stream id. They travel over [`Bus`] and are drained on [`Message::Wake`].
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
    Details {
        ws: u64,
        sel: Selected,
        result: Result<Details, String>,
    },
    Secret {
        ws: u64,
        key: (String, String),
        result: Result<BTreeMap<String, String>, String>,
    },
    /// An action finished; refresh that tab's list.
    Changed {
        ws: u64,
        text: String,
    },
    LogPods {
        id: u64,
        result: Result<Vec<(String, Vec<String>)>, String>,
    },
    LogLine {
        id: u64,
        src: usize,
        line: LogLine,
    },
    LogEnded {
        id: u64,
        src: usize,
        error: Option<String>,
    },
    LogOlder {
        id: u64,
        src: usize,
        result: Result<Vec<LogLine>, String>,
    },
    Yaml {
        title: String,
        result: Result<String, String>,
    },
    /// An edited YAML was saved (or refused).
    YamlApplied {
        title: String,
        result: Result<(), String>,
    },
    /// A new object was created from the editor: (kind, namespace, name).
    YamlCreated {
        title: String,
        result: Result<(Kind, String, String), String>,
    },
    PfListening {
        id: u64,
        port: u16,
    },
    PfFailed {
        id: u64,
        error: String,
    },
    Toast {
        text: String,
        error: bool,
    },
    /// The latest release tag from GitHub; `manual` when the user asked (reports "up to date").
    UpdateChecked {
        result: Result<String, String>,
        manual: bool,
    },
    /// The installer finished without quitting us.
    UpdateInstalled(Result<(), String>),
    ShellOutput {
        id: u64,
        data: Vec<u8>,
    },
    ShellExited {
        id: u64,
        error: Option<String>,
    },
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
            Msg::LogPods { id, .. }
            | Msg::LogLine { id, .. }
            | Msg::LogEnded { id, .. }
            | Msg::LogOlder { id, .. } => Some(*id),
            _ => None,
        }
    }
}

/// Wakes the UI when the bus has messages. Notifications coalesce, so a burst of log
/// lines is drained in one update.
static WAKE: LazyLock<Notify> = LazyLock::new(Notify::new);

/// Sender for background results; also wakes the UI so they're handled promptly.
#[derive(Clone)]
struct Bus {
    tx: Sender<Msg>,
}

impl Bus {
    fn send(&self, msg: Msg) {
        let _ = self.tx.send(msg);
        WAKE.notify_one();
    }
}


// ---------------------------------------------------------------------------
// UI state
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Tab {
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
        matches!(
            self,
            Tab::Pods | Tab::Services | Tab::Deployments | Tab::Workload(_)
        )
    }

    /// The resource lists this page needs loaded (and watched).
    fn kinds(self) -> Vec<Kind> {
        match self {
            Tab::Pods => vec![Kind::Pod],
            Tab::Services => vec![Kind::Service],
            Tab::Deployments => vec![Kind::Deployment],
            Tab::Workload(k) => vec![k],
            Tab::Overview => [Kind::Pod, Kind::Deployment]
                .into_iter()
                .chain(WORKLOAD_KINDS)
                .collect(),
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
        [
            Tab::Overview,
            Tab::Pods,
            Tab::Services,
            Tab::Deployments,
            Tab::Forwards,
            Tab::Settings,
        ]
        .into_iter()
        .chain(WORKLOAD_KINDS.map(Tab::Workload))
        .find(|t| t.key() == key)
    }
}

enum Conn {
    Idle,
    Connecting,
    Ready(Clients),
    Failed(String),
}

impl Conn {
    fn status(&self) -> (Color, &'static str) {
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
        Self {
            items: Vec::new(),
            loading: false,
            loaded: false,
            error: None,
        }
    }
}

/// The resource shown in the detail drawer.
#[derive(Clone, PartialEq, Debug)]
pub struct Selected {
    kind: Kind,
    namespace: String,
    name: String,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum DrawerTab {
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

#[derive(PartialEq, Clone, Copy)]
enum Older {
    Idle,
    Loading,
    /// Reached the start of the log (or the buffer limit).
    Done,
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
    /// What's typed in the two port fields (may be mid-edit).
    remote_text: String,
    local_text: String,
}

struct Toast {
    text: String,
    error: bool,
    at: Instant,
}

/// Something the user asked for on a resource. Applies to the active cluster tab.
#[derive(Clone)]
pub enum Action {
    Select(Selected),
    GoToPod(String, String),
    RevealSecret(String, String),
    HideSecret(String, String),
    /// Open a pod's logs: container (`None` = the default one), and whether to show the
    /// previous (crashed) run instead of the current one.
    OpenLogs(PodRow, Option<String>, bool),
    /// Open a shell in a pod's container (`None` = the default container).
    Shell(PodRow, Option<String>),
    OpenGroupLogs {
        namespace: String,
        target: LogTarget,
    },
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

/// The dropdowns and menus that float over the window.
#[derive(Clone, PartialEq, Debug)]
pub enum PopupKind {
    /// "+": open any context of any known kubeconfig in a new tab.
    NewTab,
    /// The "+" menu's submenu for one kubeconfig.
    NewTabContexts(PathBuf),
    Kubeconfig,
    Context,
    Namespace,
    /// The drawer's "⋯" menu (its items depend on the selected resource).
    Drawer,
    /// The drawer's Shell menu, for pods with several containers.
    ShellContainers,
    /// A port forward row's "⋯" menu.
    Forward(u64),
    /// A log tab's "Initial lines" picker.
    LogTail(usize),
    /// A log tab's container picker.
    LogContainer(usize),
}

struct Popup {
    kind: PopupKind,
    /// Where the trigger is; the popup opens below it.
    anchor: Rectangle,
    /// Search text, for the searchable pickers.
    query: String,
}

/// An edge being dragged to resize a panel or a table column.
#[derive(Clone, PartialEq, Debug)]
pub enum Drag {
    Drawer,
    Dock,
    /// A table column (its saved-width key) and its width when the drag began.
    Column {
        key: String,
        start: f32,
    },
}

/// Messages from the UI.
#[derive(Clone)]
pub enum Message {
    /// Background results are waiting on the bus.
    Wake,
    Tick,
    WindowResized(Size),
    CloseRequested,
    CursorMoved(Point),
    MouseReleased,
    /// A key press nobody else handled.
    Key(Keystroke),
    /// An item in the menu bar (or its shortcut) was chosen.
    Menu(menu::MenuCommand),

    // Cluster tabs and pickers
    ActivateWorkspace(usize),
    CloseWorkspace(usize),
    AddWorkspace(PathBuf, Option<String>),
    BrowseNewTab,
    UseKubeconfig(PathBuf),
    BrowseKubeconfig,
    SwitchContext(String),
    SetNamespaces(Vec<String>),
    /// Set the namespaces and close the picker.
    PickNamespaces(Vec<String>),
    Refresh,
    RemoveKubeconfig(PathBuf),

    // Popups
    /// Open (or toggle) a popup, anchored at the pointer until a view reports the
    /// trigger's bounds with `PopupAnchored`.
    OpenPopup(PopupKind),
    PopupAnchored(PopupKind, Rectangle),
    ClosePopup,
    /// Unfold one kubeconfig's contexts in the "+" menu.
    NewTabExpand(PathBuf),
    PopupQuery(String),
    PopupSubmit,

    // Navigation and lists
    SetTab(Tab),
    ToggleNavGroup(&'static str),
    Filter(String),
    ToggleRegex,
    ClearFilter,
    Sort(Tab, String),
    ListScrolled(f32),
    RowHover(Option<usize>),
    RowLeave(usize),
    Action(Action),

    // Drawer
    DrawerTab(DrawerTab),
    CloseDrawer,
    EnvFilter(String),
    ToggleSection(String),
    DragStart(Drag),
    /// Back to a column's default width.
    ResetColumn(String),

    // Generic
    Copy(String),
    OpenUrl(String),
    Noop,

    // Updates and "What's new"
    #[cfg_attr(target_os = "macos", allow(dead_code))]
    CheckUpdates,
    StartUpdate(String),
    Relaunch,
    ShowChangelog(bool),
    ToggleLive,

    // Bottom dock: logs and shells
    DockSelect(usize),
    DockClose(usize),
    DockCloseAll,
    SetLogMode(LogPanelMode),
    ToggleSplit,
    Log(usize, logs::LogMsg),
    Shell(u64, shell::ShellMsg),

    // YAML window
    /// The editor's text changed.
    YamlEdit,
    YamlApply,
    YamlCancel,
    YamlReload,
    YamlClose,
    YamlComment,

    // Dialogs
    PfRemote(String),
    PfLocal(String),
    PfStart,
    PfCancel,
    ConfirmDelete(bool),
}

pub struct App {
    /// The root view's focus, so keys and menu actions reach it when nothing else has focus.
    focus: gpui::FocusHandle,
    rt: Runtime,
    bus: Bus,
    rx: Receiver<Msg>,
    settings: Settings,
    window_size: Size,
    cursor: Point,

    /// Cluster tabs. Empty means the welcome screen.
    workspaces: Vec<Workspace>,
    active: usize,

    logs: Vec<LogView>,
    /// Shell tabs; the dock shows log tabs first, then these.
    shells: Vec<ShellView>,
    /// Index into the dock's tabs: logs, then shells.
    active_log: usize,
    /// In split view: the log tab (left) and shell tab (right) last picked.
    split_log: usize,
    split_shell: usize,
    yaml: Option<YamlView>,
    forwards: Vec<Forward>,
    pf_dialog: Option<PfDialog>,
    /// (cluster tab, kind, namespace, name)
    confirm_delete: Option<(u64, Kind, String, String)>,
    toasts: Vec<Toast>,
    update: update::UpdateState,
    /// Outcome of a running installer (normally it quits and reopens the app before reporting).
    changelog_open: bool,
    popup: Option<Popup>,
    drag: Option<Drag>,
    /// Where the pointer was when the drag began.
    drag_origin: Point,
    drawer_width: f32,
    /// The pointer is over this row of the current list.
    hovered_row: Option<usize>,
    /// The env search in the drawer (shared by every container).
    env_filter: String,
    /// Collapsible drawer sections whose state differs from their default.
    toggled_sections: HashSet<String>,
    next_id: u64,
    /// When the window size last changed and hasn't been written to disk yet.
    window_dirty: Option<Instant>,
    /// Development aid: `KUBE_UI_SCREENSHOT=<file.png>` captures the window once and quits.
    /// With `KUBE_UI_SCREENSHOT_TAB=<page key>` (e.g. `overview`, `jobs`) it shows that page
    /// instead of a pod with its logs; `KUBE_UI_SCREENSHOT_SELECT=1` also opens the first row,
    /// and `KUBE_UI_SCREENSHOT_EDIT=1` opens it in the YAML editor (nothing is saved).
    /// `KUBE_UI_SCREENSHOT_CHANGELOG=1` opens the "What's new" window.
    screenshot: Option<DevScreenshot>,
    /// Chrome's retained inputs (popup search, port-forward dialog fields); see `view.rs`.
    chrome: view::ChromeState,
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
    /// Builds the app for its window and starts the background loops. Call from
    /// `cx.new` inside the window's builder.
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .expect("failed to start tokio runtime");
        let (tx, rx) = mpsc::channel();
        let settings = Settings::load();
        let window_size = settings
            .window_size
            .map(|[w, h]| Size::new(w, h))
            .unwrap_or(Size::new(1360.0, 860.0));

        let focus = cx.focus_handle();
        window.focus(&focus, cx);
        let mut app = Self {
            focus,
            rt,
            bus: Bus { tx },
            rx,
            settings,
            window_size,
            cursor: Point::ORIGIN,
            workspaces: Vec::new(),
            active: 0,
            logs: Vec::new(),
            shells: Vec::new(),
            active_log: 0,
            split_log: 0,
            split_shell: 0,
            yaml: None,
            forwards: Vec::new(),
            pf_dialog: None,
            confirm_delete: None,
            toasts: Vec::new(),
            update: update::UpdateState::Idle,
            changelog_open: false,
            popup: None,
            drag: None,
            drag_origin: Point::ORIGIN,
            drawer_width: 460.0,
            hovered_row: None,
            env_filter: String::new(),
            toggled_sections: HashSet::new(),
            next_id: 1,
            window_dirty: None,
            chrome: Default::default(),
            screenshot: std::env::var_os("KUBE_UI_SCREENSHOT").map(|p| DevScreenshot {
                path: PathBuf::from(p),
                started: Instant::now(),
                staged: None,
                requested: false,
                tab: std::env::var("KUBE_UI_SCREENSHOT_TAB")
                    .ok()
                    .and_then(|k| Tab::from_key(&k)),
                select: std::env::var_os("KUBE_UI_SCREENSHOT_SELECT").is_some(),
                edit: std::env::var_os("KUBE_UI_SCREENSHOT_EDIT").is_some(),
                shell: std::env::var("KUBE_UI_SCREENSHOT_SHELL").ok(),
                log_regex: std::env::var("KUBE_UI_SCREENSHOT_LOG_REGEX").ok(),
                sort: std::env::var("KUBE_UI_SCREENSHOT_SORT").ok().map(|v| {
                    let (col, dir) = v.split_once(':').unwrap_or((&v, "asc"));
                    Sort {
                        col: col.to_string(),
                        asc: dir != "desc",
                    }
                }),
            }),
        };

        // Show what changed after an update (not on a fresh install).
        if app.screenshot.is_some() {
            app.changelog_open = std::env::var_os("KUBE_UI_SCREENSHOT_CHANGELOG").is_some();
        } else if app.settings.last_seen_version.as_deref() != Some(update::CURRENT) {
            app.changelog_open = app.settings.last_seen_version.is_some();
            app.settings.last_seen_version = Some(update::CURRENT.to_string());
            app.settings.save();
        }

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
                    .filter_map(|(k, v)| {
                        Some((
                            Tab::from_key(k)?,
                            Sort {
                                col: v.column.clone(),
                                asc: !v.descending,
                            },
                        ))
                    })
                    .collect();
                ws.update_matcher();
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
        // `KUBE_UI_SCREENSHOT_UPDATE=<tag>` shows that release as available instead of asking GitHub.
        match std::env::var("KUBE_UI_SCREENSHOT_UPDATE") {
            Ok(tag) => app.update = update::UpdateState::Available(tag),
            Err(_) if app.screenshot.is_none() => app.check_for_updates(false),
            Err(_) => {}
        }
        app.sync();
        app.start_loops(window, cx);
        app
    }

    /// The background loops that replace iced's subscriptions: draining the bus when a
    /// task wakes it, the once-a-second tick, and saving on close.
    fn start_loops(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let handle = AppHandle(cx.entity().downgrade());
        cx.set_global(handle);
        // Results from background tasks.
        cx.spawn_in(window, async move |this, cx| {
            loop {
                WAKE.notified().await;
                let alive = this.update_in(cx, |app, window, cx| {
                    app.dispatch(Message::Wake, window, cx)
                });
                if alive.is_err() {
                    break;
                }
            }
        })
        .detach();
        // The tick.
        let every = if self.screenshot.is_some() {
            Duration::from_millis(100)
        } else {
            Duration::from_secs(1)
        };
        cx.spawn_in(window, async move |this, cx| {
            loop {
                cx.background_executor().timer(every).await;
                let alive = this.update_in(cx, |app, window, cx| {
                    app.dispatch(Message::Tick, window, cx)
                });
                if alive.is_err() {
                    break;
                }
            }
        })
        .detach();
        // Closing the window quits (after saving), as the only window.
        let handle = cx.entity().downgrade();
        window.on_window_should_close(cx, move |window, cx| {
            if let Some(app) = handle.upgrade() {
                app.update(cx, |app, cx| app.dispatch(Message::CloseRequested, window, cx));
            }
            true
        });
    }

    // -----------------------------------------------------------------------
    // Update
    // -----------------------------------------------------------------------

    /// Handles a message and whatever follow-up work it asks for, then redraws.
    pub fn dispatch(&mut self, message: Message, window: &mut Window, cx: &mut Context<Self>) {
        // Pointer motion arrives constantly; only a drag in progress needs a redraw.
        let quiet = matches!(message, Message::CursorMoved(_)) && self.drag.is_none();
        let mut pending = VecDeque::from([Effect::Msg(message)]);
        while let Some(effect) = pending.pop_front() {
            match effect {
                Effect::Msg(msg) => {
                    let task = self.handle(msg);
                    self.sync();
                    pending.extend(task.effects);
                }
                Effect::Copy(text) => cx.write_to_clipboard(gpui::ClipboardItem::new_string(text)),
                Effect::Quit => cx.quit(),
                Effect::Window(f) => f(self, window, cx),
            }
        }
        if !quiet {
            cx.notify();
        }
    }

    /// Keeps background state in line with what's on screen after every message: only the
    /// front tab watches its list, and open tabs are saved when they change.
    fn sync(&mut self) {
        if self.workspaces.is_empty() {
            self.save_workspaces();
            return;
        }
        self.active = self.active.min(self.workspaces.len() - 1);
        let live = self.settings.live_updates;
        let active = self.active;
        for (i, ws) in self.workspaces.iter_mut().enumerate() {
            if i == active {
                ws.sync_watch(live);
            } else {
                ws.pause_watch();
            }
        }
        self.save_workspaces();
    }

    fn handle(&mut self, message: Message) -> Task {
        match message {
            Message::Wake => return self.handle_messages(),
            Message::Tick => return self.tick(),
            Message::WindowResized(size) => {
                self.window_size = size;
                self.track_window();
            }
            Message::CloseRequested => return self.quit(),
            Message::CursorMoved(p) => {
                self.cursor = p;
                if let Some(drag) = self.drag.clone() {
                    self.drag_to(drag, p);
                }
            }
            Message::MouseReleased => {
                if matches!(self.drag.take(), Some(Drag::Dock | Drag::Column { .. })) {
                    self.settings.save();
                }
            }
            Message::Key(keystroke) => return self.key(&keystroke),
            Message::Menu(cmd) => return self.menu_command(cmd),

            Message::ActivateWorkspace(i) => self.activate_workspace(i),
            Message::CloseWorkspace(i) => self.close_workspace(i),
            Message::AddWorkspace(path, context) => {
                self.popup = None;
                self.add_workspace(path, context, None);
            }
            Message::BrowseNewTab => {
                self.popup = None;
                if let Some(path) = pick_kubeconfig() {
                    self.add_workspace(path, None, None);
                }
            }
            Message::UseKubeconfig(path) => {
                self.popup = None;
                if let Some(ws) = self.workspaces.get_mut(self.active) {
                    if ws.kubeconfig_path != path {
                        ws.load_kubeconfig(path, None, &mut self.settings);
                    }
                }
            }
            Message::BrowseKubeconfig => {
                self.popup = None;
                if let Some(path) = pick_kubeconfig() {
                    return self.handle(Message::UseKubeconfig(path));
                }
            }
            Message::SwitchContext(c) => {
                self.popup = None;
                if let Some(ws) = self.workspaces.get_mut(self.active) {
                    if ws.context.as_ref() != Some(&c) {
                        ws.switch_context(c, &mut self.settings);
                    }
                }
            }
            Message::SetNamespaces(selection) => {
                if let Some(ws) = self.workspaces.get_mut(self.active) {
                    ws.set_namespaces(selection, &mut self.settings);
                }
            }
            Message::PickNamespaces(selection) => {
                self.popup = None;
                return self.handle(Message::SetNamespaces(selection));
            }
            Message::Refresh => {
                if let Some(ws) = self.ws_mut() {
                    ws.refresh();
                }
            }
            Message::RemoveKubeconfig(p) => {
                // Forget the file; tabs already using it stay open until closed.
                self.settings.kubeconfigs.retain(|k| k != &p);
                self.settings.save();
            }

            Message::OpenPopup(kind) => {
                if self.popup.as_ref().is_some_and(|p| p.kind == kind) {
                    self.popup = None;
                    return Task::none();
                }
                // A submenu stays inside its parent's popup; anything else replaces it.
                let anchor = Rectangle::new(self.cursor, Size::new(1.0, 1.0));
                self.popup = Some(Popup {
                    kind,
                    anchor,
                    query: String::new(),
                });
                return view::focus_popup_search();
            }
            Message::PopupAnchored(kind, bounds) => {
                if let Some(p) = self.popup.as_mut().filter(|p| p.kind == kind) {
                    p.anchor = bounds;
                }
            }
            Message::ClosePopup => self.popup = None,
            Message::NewTabExpand(path) => {
                if let Some(p) = self.popup.as_mut() {
                    p.kind = if p.kind == PopupKind::NewTabContexts(path.clone()) {
                        PopupKind::NewTab
                    } else {
                        PopupKind::NewTabContexts(path)
                    };
                }
            }
            Message::PopupQuery(q) => {
                if let Some(p) = self.popup.as_mut() {
                    p.query = q;
                }
            }
            Message::PopupSubmit => return self.popup_submit(),

            Message::SetTab(tab) => {
                self.popup = None;
                if let Some(ws) = self.ws_mut() {
                    ws.set_tab(tab);
                }
                self.hovered_row = None;
            }
            Message::ToggleNavGroup(key) => {
                let collapsed = &mut self.settings.nav_collapsed;
                if collapsed.iter().any(|c| c == key) {
                    collapsed.retain(|c| c != key);
                } else {
                    collapsed.push(key.to_string());
                }
                self.settings.save();
            }
            Message::Filter(text) => {
                if let Some(ws) = self.ws_mut() {
                    ws.set_filter(text);
                }
            }
            Message::ToggleRegex => {
                if let Some(ws) = self.ws_mut() {
                    ws.filter_regex = !ws.filter_regex;
                    ws.update_matcher();
                }
            }
            Message::ClearFilter => {
                if let Some(ws) = self.ws_mut() {
                    ws.set_filter(String::new());
                }
            }
            Message::Sort(tab, col) => {
                if let Some(ws) = self.ws_mut() {
                    ws.toggle_sort(tab, col);
                }
            }
            Message::ListScrolled(y) => {
                if let Some(ws) = self.ws_mut() {
                    ws.list_scroll = y;
                }
            }
            Message::RowHover(row) => self.hovered_row = row,
            Message::RowLeave(row) => {
                if self.hovered_row == Some(row) {
                    self.hovered_row = None;
                }
            }
            Message::Action(action) => {
                self.popup = None;
                return self.apply(action);
            }

            Message::DrawerTab(tab) => {
                if let Some(ws) = self.ws_mut() {
                    ws.drawer_tab = tab;
                }
            }
            Message::CloseDrawer => {
                if let Some(ws) = self.ws_mut() {
                    ws.close_drawer();
                }
            }
            Message::EnvFilter(f) => self.env_filter = f,
            Message::ToggleSection(key) => {
                if !self.toggled_sections.remove(&key) {
                    self.toggled_sections.insert(key);
                }
            }
            Message::DragStart(drag) => {
                self.drag = Some(drag);
                self.drag_origin = self.cursor;
            }
            Message::ResetColumn(key) => {
                if self.settings.column_widths.remove(&key).is_some() {
                    self.settings.save();
                }
            }

            Message::Copy(text) => {
                self.toast(copied_message(&text), false);
                return Task::copy(text);
            }
            Message::OpenUrl(url) => {
                self.popup = None;
                open_url(&url);
            }
            Message::Noop => {}

            Message::CheckUpdates => self.check_for_updates(true),
            Message::StartUpdate(tag) => self.start_update(tag),
            Message::Relaunch => {
                if update::relaunch() {
                    return self.quit();
                }
                self.toast("Quit and reopen Kube UI to use the new version.", false);
            }
            Message::ShowChangelog(open) => self.changelog_open = open,
            Message::ToggleLive => {
                self.settings.live_updates = !self.settings.live_updates;
                self.settings.save();
            }

            Message::DockSelect(i) => {
                self.active_log = i;
                self.show_logs();
            }
            Message::DockClose(i) => self.close_dock_tab(i),
            Message::DockCloseAll => {
                self.logs.clear();
                self.shells.clear();
            }
            Message::SetLogMode(mode) => self.set_log_mode(mode),
            Message::ToggleSplit => {
                self.settings.log_split = !self.settings.log_split;
                self.settings.save();
            }
            Message::Log(i, msg) => return self.log_message(i, msg),
            Message::Shell(id, msg) => return self.shell_message(id, msg),

            Message::YamlEdit
            | Message::YamlApply
            | Message::YamlCancel
            | Message::YamlReload
            | Message::YamlClose
            | Message::YamlComment => return self.yaml_message(message),

            Message::PfRemote(text) => {
                if let Some(d) = self.pf_dialog.as_mut() {
                    if let Ok(p) = text.parse::<u16>() {
                        d.remote_port = p.max(1);
                    }
                    d.remote_text = text;
                }
            }
            Message::PfLocal(text) => {
                if let Some(d) = self.pf_dialog.as_mut() {
                    if let Ok(p) = text.parse::<u16>() {
                        d.local_port = p;
                    } else if text.is_empty() {
                        d.local_port = 0;
                    }
                    d.local_text = text;
                }
            }
            Message::PfStart => {
                if let Some(d) = self.pf_dialog.take() {
                    let id = self.add_forward(SavedForward {
                        kubeconfig: d.kubeconfig,
                        context: d.context,
                        namespace: d.namespace,
                        target: d.target,
                        remote_port: d.remote_port,
                        local_port: d.local_port,
                        active: false,
                    });
                    self.start_forward(id);
                    if let Some(ws) = self.ws_mut() {
                        ws.set_tab(Tab::Forwards);
                    }
                }
            }
            Message::PfCancel => self.pf_dialog = None,
            Message::ConfirmDelete(yes) => {
                if let Some((ws, kind, ns, name)) = self.confirm_delete.take() {
                    if yes {
                        if let Some(w) = self.workspaces.iter_mut().find(|w| w.id == ws) {
                            w.close_drawer();
                        }
                        self.run_change(
                            Some(ws),
                            format!("Deleted {} {name}", kind.label().to_lowercase()),
                            move |c| kube_ops::delete(c, kind, ns, name),
                        );
                    }
                }
            }
        }
        Task::none()
    }

    fn quit(&mut self) -> Task {
        if self.window_dirty.take().is_some() {
            self.settings.save();
        }
        Task::quit()
    }

    /// Once a second (every 100ms while taking a dev screenshot): expire toasts, refresh
    /// the open drawer, flush the window size.
    fn tick(&mut self) -> Task {
        self.toasts.retain(|t| t.at.elapsed() < TOAST_FOR);
        if self.settings.live_updates {
            if let Some(ws) = self.ws_mut().filter(|ws| ws.client().is_some()) {
                ws.poll_details(DETAILS_EVERY);
            }
        }
        if self
            .window_dirty
            .is_some_and(|at| at.elapsed() >= Duration::from_secs(1))
        {
            self.window_dirty = None;
            self.settings.save();
        }
        self.dev_screenshot()
    }

    /// Remembers the window size. Writes wait until resizing has paused for a second (and
    /// happen on exit), so dragging the window edge doesn't hit the disk on every step.
    /// (Not the maximized state: macOS reports any window that fills the screen as maximized.)
    fn track_window(&mut self) {
        if self.screenshot.is_some() {
            return;
        }
        let size = self.window_size;
        let changed = self
            .settings
            .window_size
            .is_none_or(|[w, h]| (w - size.width).abs() > 1.0 || (h - size.height).abs() > 1.0);
        if changed {
            self.settings.window_size = Some([size.width, size.height]);
            self.window_dirty = Some(Instant::now());
        }
    }

    fn drag_to(&mut self, drag: Drag, p: Point) {
        match drag {
            Drag::Drawer => {
                let max = (self.window_size.width - NAV_WIDTH - 360.0).max(DRAWER_MIN_WIDTH);
                self.drawer_width = (self.window_size.width - p.x).clamp(DRAWER_MIN_WIDTH, max);
            }
            Drag::Column { key, start } => {
                let w = (start + p.x - self.drag_origin.x).clamp(table::MIN_COL_WIDTH, 1600.0);
                self.settings.column_widths.insert(key, w.round());
            }
            Drag::Dock => {
                let max = self.dock_max_height();
                self.settings.log_height =
                    (self.window_size.height - p.y).clamp(LOG_MIN_HEIGHT, max);
            }
        }
    }

    /// Leave room for the top bar and a bit of the page when the dock is expanded.
    fn dock_max_height(&self) -> f32 {
        (self.window_size.height - 180.0).max(LOG_MIN_HEIGHT)
    }

    fn key(&mut self, keystroke: &Keystroke) -> Task {
        if self.screenshot.is_some() {
            return Task::none();
        }
        let m = &keystroke.modifiers;
        if keystroke.key == "escape" {
            if self.popup.take().is_some() {
                return Task::none();
            }
            if self.pf_dialog.take().is_some() || self.confirm_delete.take().is_some() {
                return Task::none();
            }
            if self.yaml.is_some() || self.changelog_open {
                return Task::none();
            }
            if let Some(ws) = self.ws_mut() {
                ws.close_drawer();
            }
            return Task::none();
        }
        let command = if cfg!(target_os = "macos") {
            m.platform
        } else {
            m.control
        };
        if !command {
            return Task::none();
        }
        let mut chars = keystroke.key.chars();
        let (Some(c), None) = (chars.next(), chars.next()) else {
            return Task::none();
        };
        match c {
            // ⌘1…⌘8 pick a cluster tab and ⌘9 the last one, as in browsers.
            '1'..='9' if !m.alt => {
                let i = c as usize - '1' as usize;
                if i == 8 {
                    self.activate_workspace(self.workspaces.len().saturating_sub(1));
                } else {
                    self.activate_workspace(i);
                }
            }
            's' if self.yaml.as_ref().is_some_and(|y| y.editing) => self.yaml_apply(),
            '/' if self.yaml.as_ref().is_some_and(|y| y.editing) => {
                return self.yaml_message(Message::YamlComment);
            }
            _ => {}
        }
        Task::none()
    }

    // -----------------------------------------------------------------------
    // Menu bar
    // -----------------------------------------------------------------------

    fn menu_command(&mut self, cmd: menu::MenuCommand) -> Task {
        use menu::MenuCommand;
        match cmd {
            MenuCommand::CheckForUpdates => match &self.update {
                update::UpdateState::Available(tag) => self.toast(
                    format!("Kube UI {tag} is available. Use Update in the top bar."),
                    false,
                ),
                update::UpdateState::Installed(tag) => self.toast(
                    format!("{tag} is installed. Restart Kube UI to finish."),
                    false,
                ),
                _ => self.check_for_updates(true),
            },
            MenuCommand::WhatsNew => self.changelog_open = true,
            MenuCommand::Settings => match self.ws_mut() {
                Some(ws) => ws.set_tab(Tab::Settings),
                None => self.toast("Add a cluster to open Settings.", false),
            },
            MenuCommand::Quit => return self.quit(),
            MenuCommand::NewTab if !self.workspaces.is_empty() => {
                return self.handle(Message::OpenPopup(PopupKind::NewTab));
            }
            MenuCommand::NewTab | MenuCommand::OpenKubeconfig => {
                if let Some(path) = pick_kubeconfig() {
                    self.add_workspace(path, None, None);
                }
            }
            MenuCommand::Close => self.close_frontmost(),
            MenuCommand::Reload => {
                if let Some(ws) = self.ws_mut() {
                    ws.refresh();
                }
            }
            MenuCommand::ToggleBottomPanel => {
                if self.logs.is_empty() && self.shells.is_empty() {
                    self.toast("No logs or terminals open.", false);
                } else if self.settings.log_mode == LogPanelMode::Collapsed {
                    self.set_log_mode(LogPanelMode::Normal);
                } else {
                    self.set_log_mode(LogPanelMode::Collapsed);
                }
            }
            MenuCommand::Go(tab) => {
                if let Some(ws) = self.ws_mut() {
                    ws.set_tab(tab);
                }
            }
            MenuCommand::NextTab => self.cycle_workspace(1),
            MenuCommand::PreviousTab => self.cycle_workspace(-1),
            MenuCommand::OpenGitHub => open_url(&format!("https://github.com/{}", update::REPO)),
            MenuCommand::OpenUpdateLog => {
                let path = update::log_path();
                if !path.exists() {
                    self.toast("No update log yet.", false);
                } else if std::process::Command::new("open")
                    .arg(&path)
                    .spawn()
                    .is_err()
                {
                    self.toast(format!("Update log: {}", display_path(&path)), false);
                }
            }
        }
        Task::none()
    }

    /// ⌘W: closes the innermost thing showing: the YAML window, "What's new", the detail
    /// drawer, then the cluster tab (never the last one).
    fn close_frontmost(&mut self) {
        if let Some(y) = &self.yaml {
            if y.editing && y.dirty {
                self.toast(
                    "This YAML has unapplied changes. Apply them or close the window with ×.",
                    false,
                );
            } else {
                self.yaml = None;
            }
        } else if self.changelog_open {
            self.changelog_open = false;
        } else if let Some(ws) = self.ws_mut().filter(|ws| ws.has_drawer()) {
            ws.close_drawer();
        } else if self.workspaces.len() > 1 {
            self.close_workspace(self.active);
        }
    }

    /// Moves to the next (`1`) or previous (`-1`) cluster tab, wrapping around.
    fn cycle_workspace(&mut self, step: isize) {
        let n = self.workspaces.len() as isize;
        if n > 1 {
            self.activate_workspace((self.active as isize + step).rem_euclid(n) as usize);
        }
    }

    // -----------------------------------------------------------------------
    // Updates
    // -----------------------------------------------------------------------

    fn check_for_updates(&mut self, manual: bool) {
        if matches!(
            self.update,
            update::UpdateState::Checking { .. }
                | update::UpdateState::Installing(_)
                | update::UpdateState::Installed(_)
        ) {
            return;
        }
        self.update = update::UpdateState::Checking { manual };
        let bus = self.bus.clone();
        self.rt.spawn(async move {
            let result = update::latest_tag().await;
            bus.send(Msg::UpdateChecked { result, manual });
        });
    }

    fn update_checked(&mut self, result: Result<String, String>, manual: bool) {
        self.update = match result {
            Ok(tag) if update::is_newer(&tag, update::CURRENT) => {
                update::UpdateState::Available(tag)
            }
            Ok(_) => {
                if manual {
                    self.toast(format!("Kube UI {} is up to date.", update::CURRENT), false);
                }
                update::UpdateState::UpToDate
            }
            Err(e) => {
                if manual {
                    self.toast(format!("Couldn't check for updates: {e}"), true);
                    update::UpdateState::Failed(e)
                } else {
                    // A failed background check stays quiet; the button still offers a retry.
                    update::UpdateState::Idle
                }
            }
        };
    }

    fn start_update(&mut self, tag: String) {
        match update::install(&tag) {
            Ok(rx) => {
                self.update = update::UpdateState::Installing(tag);
                let bus = self.bus.clone();
                std::thread::spawn(move || {
                    if let Ok(result) = rx.recv() {
                        bus.send(Msg::UpdateInstalled(result));
                    }
                });
            }
            Err(e) => {
                self.toast(e.clone(), true);
                self.update = update::UpdateState::Failed(e);
            }
        }
    }

    fn update_installed(&mut self, result: Result<(), String>) {
        let tag = match &self.update {
            update::UpdateState::Installing(t) => t.clone(),
            _ => String::new(),
        };
        match result {
            // Installed, but we weren't quit: offer the restart ourselves.
            Ok(()) => {
                self.toast(
                    format!("Kube UI {tag} is installed. Restart to use it."),
                    false,
                );
                self.update = update::UpdateState::Installed(tag);
            }
            Err(e) => {
                self.toast(
                    format!(
                        "Update failed: {e} (log: {})",
                        display_path(&update::log_path())
                    ),
                    true,
                );
                self.update = update::UpdateState::Failed(e);
            }
        }
    }

    fn next_id(&mut self) -> u64 {
        self.next_id += 1;
        self.next_id
    }

    fn toast(&mut self, text: impl Into<String>, error: bool) {
        self.toasts.push(Toast {
            text: text.into(),
            error,
            at: Instant::now(),
        });
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

    fn add_workspace(
        &mut self,
        path: PathBuf,
        context: Option<String>,
        namespaces: Option<Vec<String>>,
    ) {
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
        self.popup = None;
        // A live tab restarts its watch in `sync`; otherwise catch up here.
        let ws = &mut self.workspaces[index];
        if !self.settings.live_updates
            && ws.last_refresh.is_none_or(|t| t.elapsed() >= DETAILS_EVERY)
        {
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
    // Popups
    // -----------------------------------------------------------------------

    /// Enter in a searchable picker takes the first match.
    fn popup_submit(&mut self) -> Task {
        let Some(p) = &self.popup else {
            return Task::none();
        };
        let q = p.query.to_lowercase();
        let Some(ws) = self.ws() else {
            return Task::none();
        };
        match p.kind {
            PopupKind::Context => {
                let first = ws
                    .kubeconfig
                    .iter()
                    .flat_map(|k| k.contexts.iter())
                    .find(|c| c.to_lowercase().contains(&q))
                    .cloned();
                if let Some(c) = first {
                    return self.handle(Message::SwitchContext(c));
                }
            }
            PopupKind::Namespace => {
                let first = ws
                    .namespaces
                    .iter()
                    .find(|n| n.to_lowercase().contains(&q))
                    .cloned();
                if let Some(ns) = first {
                    self.popup = None;
                    return self.handle(Message::SetNamespaces(vec![ns]));
                }
            }
            _ => {}
        }
        Task::none()
    }

    // -----------------------------------------------------------------------
    // Background message handling
    // -----------------------------------------------------------------------

    fn handle_messages(&mut self) -> Task {
        let mut tasks = Vec::new();
        let mut touched_logs = HashSet::new();
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
                let found = self.logs.iter_mut().position(|l| l.stream_id() == id);
                if let Some(i) = found {
                    touched_logs.insert(i);
                    if let Some(e) = self.logs[i].handle(msg, &handle, &self.bus) {
                        self.toast(e, true);
                    }
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
                Msg::Yaml { .. } | Msg::YamlCreated { .. } | Msg::YamlApplied { .. } => {
                    tasks.push(self.yaml_result(msg));
                }
                Msg::Toast { text, error } => self.toast(text, error),
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
                Msg::UpdateChecked { result, manual } => self.update_checked(result, manual),
                Msg::UpdateInstalled(result) => self.update_installed(result),
                _ => {}
            }
        }
        // Keep followed logs pinned to the bottom, and older pages from moving the view.
        for i in touched_logs {
            if let Some(log) = self.logs.get_mut(i) {
                tasks.push(log.after_update());
            }
        }
        Task::batch(tasks)
    }

    // -----------------------------------------------------------------------
    // Actions
    // -----------------------------------------------------------------------

    fn apply(&mut self, action: Action) -> Task {
        match action {
            Action::Select(sel) => {
                if let Some(ws) = self.ws_mut() {
                    ws.select(sel);
                }
                self.env_filter.clear();
                self.toggled_sections.clear();
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
            Action::OpenLogs(pod, container, previous) => {
                let containers = pod.containers.iter().map(|c| c.name.clone()).collect();
                let last_exit = pod
                    .containers
                    .iter()
                    .map(|c| (c.name.clone(), c.last_exit.clone()))
                    .collect();
                self.open_logs(
                    pod.namespace,
                    LogTarget::Pod(pod.name),
                    containers,
                    container,
                    previous,
                    last_exit,
                );
            }
            Action::OpenGroupLogs { namespace, target } => {
                self.open_logs(namespace, target, Vec::new(), None, false, HashMap::new())
            }
            Action::Yaml(kind, ns, name) => self.open_yaml(kind, ns, name),
            Action::ForwardPod(pod, port) => {
                let port = port.or(pod.ports.first().copied()).unwrap_or(8080);
                self.open_pf_dialog(
                    pod.namespace,
                    PfTarget::Pod(pod.name),
                    pod.ports,
                    port,
                    port,
                );
            }
            Action::ForwardService(svc, port) => {
                let ports: Vec<u16> = svc.ports.iter().map(|p| p.port).collect();
                let port = port.or(ports.first().copied()).unwrap_or(80);
                // Ports below 1024 need root locally.
                let local = if port >= 1024 { port } else { 8000 + port };
                self.open_pf_dialog(
                    svc.namespace,
                    PfTarget::Service(svc.name),
                    ports,
                    port,
                    local,
                );
            }
            Action::AskDelete(kind, ns, name) => {
                if let Some(id) = self.ws().map(|w| w.id) {
                    self.confirm_delete = Some((id, kind, ns, name));
                }
            }
            Action::Create(kind) => return self.open_create(kind),
            Action::EditYaml(kind, ns, name) => {
                self.open_yaml(kind, ns, name);
                if let Some(y) = self.yaml.as_mut() {
                    y.edit_on_load = true;
                }
            }
            Action::Trigger(ns, name) => {
                let Some((ws, client)) = self.ws().and_then(|w| w.client().map(|c| (w.id, c)))
                else {
                    return Task::none();
                };
                let bus = self.bus.clone();
                self.rt.spawn(async move {
                    match kube_ops::trigger_cronjob(client, ns, name).await {
                        Ok(job) => bus.send(Msg::Changed {
                            ws,
                            text: format!("Started job {job}"),
                        }),
                        Err(e) => bus.send(Msg::Toast {
                            text: err_string(e),
                            error: true,
                        }),
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
                format!(
                    "{} rollout of {name}",
                    if paused { "Paused" } else { "Resumed" }
                ),
                move |c| kube_ops::set_paused(c, ns, name, paused),
            ),
            Action::Scale(kind, ns, name, replicas) => {
                self.run_change(None, format!("Scaled {name} to {replicas}"), move |c| {
                    kube_ops::scale(c, kind, ns, name, replicas)
                })
            }
            Action::Restart(kind, ns, name) => {
                self.run_change(None, format!("Restarting {name}"), move |c| {
                    kube_ops::restart(c, kind, ns, name)
                })
            }
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
        Task::none()
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
        let Some((ws, client)) = target.and_then(|w| w.client().map(|c| (w.id, c))) else {
            return;
        };
        let bus = self.bus.clone();
        self.rt.spawn(async move {
            match f(client).await {
                Ok(()) => bus.send(Msg::Changed { ws, text: ok_text }),
                Err(e) => bus.send(Msg::Toast {
                    text: err_string(e),
                    error: true,
                }),
            }
        });
    }

    // -----------------------------------------------------------------------
    // Logs and shells
    // -----------------------------------------------------------------------

    /// Opens a log tab for the active cluster, or focuses it if the same logs are already open.
    /// `previous` opens the run before the last restart (`kubectl logs --previous`) in its own tab;
    /// `last_exit` says how each container's previous run ended.
    fn open_logs(
        &mut self,
        namespace: String,
        target: LogTarget,
        containers: Vec<String>,
        container: Option<String>,
        previous: bool,
        last_exit: HashMap<String, String>,
    ) {
        let Some(ws) = self.ws() else { return };
        let Some(clients) = ws.clients().cloned() else {
            return;
        };
        let context = ws.context.clone().unwrap_or_default();
        let target_key = match &target {
            LogTarget::Pod(p) => format!("pod/{p}"),
            LogTarget::Selector { selector, .. } => format!("sel/{selector}"),
            LogTarget::Owner(o) => format!("owner/{o}"),
        };
        let key = format!(
            "{}|{context}|{namespace}|{target_key}|{}{}",
            ws.kubeconfig_path.display(),
            container.clone().unwrap_or_default(),
            if previous { "|previous" } else { "" }
        );
        self.show_logs();
        if let Some(i) = self.logs.iter().position(|l| l.key == key) {
            self.active_log = i;
            return;
        }
        let mut view = LogView::new(
            key, context, clients, namespace, target, containers, container, previous, last_exit,
        );
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
        let container = container.or_else(|| {
            pod.containers
                .iter()
                .find(|c| !c.init)
                .map(|c| c.name.clone())
        });
        let id = self.next_id();
        let mut view = ShellView::new(id, context, client, pod.namespace, pod.name, container);
        view.start(self.rt.handle(), &self.bus);
        self.shells.push(view);
        self.active_log = self.logs.len() + self.shells.len() - 1;
        self.show_logs();
    }

    fn log_message(&mut self, i: usize, msg: logs::LogMsg) -> Task {
        if matches!(
            msg,
            logs::LogMsg::Restart | logs::LogMsg::SetTail(_) | logs::LogMsg::SetContainer(_)
        ) {
            self.popup = None;
        }
        let id = self.next_id();
        let handle = self.rt.handle().clone();
        match self.logs.get_mut(i) {
            Some(log) => log.update(msg, id, &handle, &self.bus),
            None => Task::none(),
        }
    }

    fn shell_message(&mut self, id: u64, msg: shell::ShellMsg) -> Task {
        let handle = self.rt.handle().clone();
        match self.shells.iter_mut().find(|s| s.id == id) {
            Some(s) => s.update(msg, &handle, &self.bus),
            None => Task::none(),
        }
    }

    fn close_dock_tab(&mut self, i: usize) {
        let n_logs = self.logs.len();
        if i < n_logs {
            self.logs.remove(i);
        } else if i - n_logs < self.shells.len() {
            self.shells.remove(i - n_logs);
        }
        let total = self.logs.len() + self.shells.len();
        if self.active_log > i || self.active_log >= total {
            self.active_log = self.active_log.saturating_sub(1);
        }
    }

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

    /// What the dock body shows: (log tab, shell tab). Split view keeps the last log tab
    /// and the last shell tab on screen together.
    fn dock_shown(&self) -> (Option<usize>, Option<usize>) {
        let (n_logs, n_shells) = (self.logs.len(), self.shells.len());
        let total = n_logs + n_shells;
        if total == 0 {
            return (None, None);
        }
        let active = self.active_log.min(total - 1);
        let (mut split_log, mut split_shell) = (self.split_log, self.split_shell);
        if active < n_logs {
            split_log = active;
        } else {
            split_shell = active - n_logs;
        }
        let split_log = split_log.min(n_logs.saturating_sub(1));
        let split_shell = split_shell.min(n_shells.saturating_sub(1));
        if self.settings.log_split && n_logs > 0 && n_shells > 0 {
            (Some(split_log), Some(split_shell))
        } else if active < n_logs {
            (Some(active), None)
        } else {
            (None, Some(active - n_logs))
        }
    }

    // -----------------------------------------------------------------------
    // Port forwards
    // -----------------------------------------------------------------------

    fn open_pf_dialog(
        &mut self,
        namespace: String,
        target: PfTarget,
        ports: Vec<u16>,
        remote: u16,
        local: u16,
    ) {
        let Some(ws) = self.ws() else { return };
        let Some(context) = ws.context.clone() else {
            return;
        };
        self.pf_dialog = Some(PfDialog {
            kubeconfig: ws.kubeconfig_path.clone(),
            context,
            namespace,
            target,
            ports,
            remote_port: remote,
            local_port: local,
            remote_text: remote.to_string(),
            local_text: local.to_string(),
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
        let Some(f) = self.forwards.iter_mut().find(|f| f.id == id) else {
            return;
        };
        if f.handle.is_some() {
            return;
        }
        // Reuse a connected cluster tab's client when one matches; otherwise connect
        // to the forward's own kubeconfig/context in the background.
        let client = self
            .workspaces
            .iter()
            .find(|w| {
                w.kubeconfig_path == f.spec.kubeconfig
                    && w.context.as_ref() == Some(&f.spec.context)
            })
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
                    None => {
                        kube_ops::connect(&spec.kubeconfig, &spec.context)
                            .await?
                            .0
                            .main
                    }
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
                bus.send(Msg::PfFailed {
                    id,
                    error: err_string(e),
                });
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
    // Development screenshot
    // -----------------------------------------------------------------------

    /// Stages a representative scene, then captures the window to a PNG and quits.
    fn dev_screenshot(&mut self) -> Task {
        let Some(shot) = self.screenshot.as_ref() else {
            return Task::none();
        };
        let mut tasks = Vec::new();
        if shot.staged.is_none()
            && let Some(tab) = shot.tab
        {
            let (select, edit, sort) = (shot.select, shot.edit, shot.sort.clone());
            let timed_out = shot.started.elapsed() > Duration::from_secs(30);
            let Some(ws) = self.ws_mut() else {
                return Task::none();
            };
            if let Some(sort) = sort {
                ws.sorts.insert(tab, sort);
            }
            // `KUBE_UI_SCREENSHOT_FILTER=<regex>` presets the page's filter in regex mode.
            if let Ok(re) = std::env::var("KUBE_UI_SCREENSHOT_FILTER") {
                ws.filters.insert(tab, re);
                ws.filter_regex = true;
                ws.update_matcher();
            }
            if ws.tab != tab {
                ws.set_tab(tab);
                return Task::none();
            }
            let loaded = ws.client().is_some() && !ws.is_loading() && ws.last_refresh.is_some();
            if !loaded && !timed_out {
                return Task::none();
            }
            let running = ws
                .pods
                .items
                .iter()
                .find(|p| p.status == "Running")
                .cloned();
            let first = ws.first_row(tab);
            if select {
                if let Some(sel) = first.clone() {
                    ws.select(sel);
                }
            }
            if let Some(sel) = first.filter(|_| edit) {
                tasks.push(self.apply(Action::EditYaml(sel.kind, sel.namespace, sel.name)));
            }
            // `KUBE_UI_SCREENSHOT_ENV=<text>` presets the drawer's env search.
            if let Ok(q) = std::env::var("KUBE_UI_SCREENSHOT_ENV") {
                self.env_filter = q;
                if let Some(ws) = self.ws_mut() {
                    ws.drawer_tab = DrawerTab::Containers;
                }
            }
            if std::env::var_os("KUBE_UI_SCREENSHOT_CREATE").is_some() {
                if let Some(kind) = tab.kinds().first().copied() {
                    tasks.push(self.apply(Action::Create(kind)));
                }
            }
            if let Some(pod) =
                running.filter(|_| self.screenshot.as_ref().is_some_and(|s| s.shell.is_some()))
            {
                tasks.push(self.apply(Action::Shell(pod, None)));
            }
            if let Some(shot) = self.screenshot.as_mut() {
                shot.staged = Some(Instant::now());
            }
            return Task::batch(tasks);
        }
        if shot.staged.is_none() {
            let first_pod = self.ws().and_then(|w| w.pods.items.first().cloned());
            let timed_out = shot.started.elapsed() > Duration::from_secs(20);
            if first_pod.is_none() && !timed_out {
                return Task::none();
            }
            if let Some(pod) = first_pod {
                tasks.push(self.apply(Action::Select(Selected {
                    kind: Kind::Pod,
                    namespace: pod.namespace.clone(),
                    name: pod.name.clone(),
                })));
                // `KUBE_UI_SCREENSHOT_PREVIOUS=1` opens the previous run's logs instead.
                let previous = std::env::var_os("KUBE_UI_SCREENSHOT_PREVIOUS").is_some();
                tasks.push(self.apply(Action::OpenLogs(pod, None, previous)));
                if let Some(re) = self.screenshot.as_ref().and_then(|s| s.log_regex.clone()) {
                    if let Some(log) = self.logs.last_mut() {
                        log.set_filter(re, true);
                    }
                }
            }
            // `KUBE_UI_SCREENSHOT_LOGS_COLLAPSED=1` shows the log panel folded to its tab strip.
            if std::env::var_os("KUBE_UI_SCREENSHOT_LOGS_COLLAPSED").is_some() {
                self.settings.log_mode = LogPanelMode::Collapsed;
            }
            if let Some(shot) = self.screenshot.as_mut() {
                shot.staged = Some(Instant::now());
            }
            return Task::batch(tasks);
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
                _ => return Task::none(),
            }
        }
        let Some(shot) = self.screenshot.as_mut() else {
            return Task::none();
        };
        if !shot.requested
            && shot
                .staged
                .is_some_and(|t| t.elapsed() > Duration::from_secs(3))
        {
            shot.requested = true;
            return Task::window(|app, window, _| {
                let bounds = Rectangle::from(window.bounds());
                app.save_screenshot(bounds);
            });
        }
        Task::none()
    }

    /// Captures the window's area of the screen (macOS `screencapture`) and quits.
    fn save_screenshot(&mut self, bounds: Rectangle) {
        let Some(dev) = self.screenshot.as_ref() else {
            return;
        };
        let region = format!(
            "{},{},{},{}",
            bounds.x, bounds.y, bounds.width, bounds.height
        );
        let status = std::process::Command::new("screencapture")
            .args(["-x", "-o", "-R", &region])
            .arg(&dev.path)
            .status();
        if !status.as_ref().is_ok_and(|s| s.success()) {
            eprintln!("screenshot failed: {status:?}");
        }
        std::process::exit(0);
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
        SavedSort {
            column: self.col.clone(),
            descending: !self.asc,
        }
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

fn health_color(h: Health) -> Color {
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

fn status_color(status: &str) -> Color {
    match status {
        "Running" | "Succeeded" | "Completed" => GREEN,
        "Pending" | "ContainerCreating" | "PodInitializing" | "Terminating" | "Waiting" => YELLOW,
        s if s.starts_with("Init:") => YELLOW,
        _ => RED,
    }
}

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

/// Quotes a command-line word for display when it contains spaces or shell metacharacters.
fn shell_quote(word: &str) -> String {
    if !word.is_empty()
        && word
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "-_./=:,@%+".contains(c))
    {
        word.to_string()
    } else {
        format!("'{}'", word.replace('\'', "'\\''"))
    }
}

/// Case-insensitive env filter on the name, value and where it comes from.
fn env_matches(filter: &str, name: &str, value: Option<&str>, source: Option<&str>) -> bool {
    filter.is_empty()
        || [Some(name), value, source]
            .into_iter()
            .flatten()
            .any(|s| s.to_lowercase().contains(filter))
}

fn default_kubeconfig() -> Option<PathBuf> {
    if let Some(first) = std::env::var_os("KUBECONFIG")
        .and_then(|v| std::env::split_paths(&v).next())
        .filter(|p| p.is_file())
    {
        return Some(first);
    }
    dirs::home_dir()
        .map(|h| h.join(".kube").join("config"))
        .filter(|p| p.is_file())
}

fn pick_kubeconfig() -> Option<PathBuf> {
    let mut dialog = rfd::FileDialog::new().set_title("Select kubeconfig file");
    if let Some(dir) = dirs::home_dir()
        .map(|h| h.join(".kube"))
        .filter(|d| d.is_dir())
    {
        dialog = dialog.set_directory(dir);
    }
    dialog.pick_file()
}

fn file_label(p: &Path) -> String {
    p.file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_default()
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

fn open_url(url: &str) {
    #[cfg(target_os = "macos")]
    let result = std::process::Command::new("open").arg(url).spawn();
    #[cfg(target_os = "windows")]
    let result = std::process::Command::new("cmd")
        .args(["/C", "start", "", url])
        .spawn();
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    let result = std::process::Command::new("xdg-open").arg(url).spawn();
    if let Err(e) = result {
        eprintln!("could not open {url}: {e}");
    }
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

/// One-time setup before the window opens: fonts and theme, the menu bar.
pub fn install(cx: &mut gpui::App) {
    theme::install(cx);
    menu::install(cx);
}

impl App {
    /// Before each frame: lets every part create the retained GPUI state it renders
    /// (Kit inputs, scroll handles, the editor) and subscribe to it. Views themselves are
    /// plain `&self` functions, so anything needing `window`/`cx` happens here.
    fn prepare(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.prepare_chrome(window, cx);
        for ws in &mut self.workspaces {
            ws.prepare(window, cx);
            ws.sync_env(&self.env_filter, window, cx);
        }
        for log in &mut self.logs {
            log.prepare(window, cx);
        }
        for shell in &mut self.shells {
            shell.prepare(window, cx);
        }
        self.prepare_yaml(window, cx);
    }
}

impl Render for App {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let size = Size::from(window.viewport_size());
        if size != self.window_size {
            self.window_size = size;
            self.track_window();
        }
        self.prepare(window, cx);
        let root = gpui::div()
            .id("root")
            .size_full()
            .relative()
            .font_family(theme::INTER)
            .text_size(gpui::px(theme::BODY))
            .text_color(theme::TEXT)
            .bg(theme::CONTENT)
            .track_focus(&self.focus)
            .on_key_down(|e: &gpui::KeyDownEvent, window, cx| {
                send(Message::Key(e.keystroke.clone()), window, cx)
            })
            .on_mouse_move(|e: &gpui::MouseMoveEvent, window, cx| {
                send(Message::CursorMoved(e.position.into()), window, cx)
            })
            .on_mouse_up(gpui::MouseButton::Left, |_, window, cx| {
                send(Message::MouseReleased, window, cx)
            })
            .child(self.view(window, cx));
        menu::handle_actions(root)
    }
}

/// The toast shown after copying: short single-line values are quoted, the rest summarised.
fn copied_message(text: &str) -> String {
    let lines = text.lines().count();
    if lines <= 1 && text.chars().count() <= 60 && !text.is_empty() {
        format!("Copied “{}”", text.trim())
    } else if lines > 1 {
        format!("Copied {lines} lines")
    } else {
        "Copied to clipboard".to_string()
    }
}
