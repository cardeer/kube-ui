//! The Telepresence page: daemon status, connect/disconnect, and intercepts on the
//! workloads in the connected namespace. Telepresence is one connection per machine, so
//! this state lives on the app rather than on a cluster tab.

use iced::{
    Element, Length,
    widget::{Space, button, column, container, row, text_input},
};

use super::*;
use crate::telepresence::{self as tp, Snapshot, TpError};

/// How often the page re-reads `telepresence status` while it's open.
const POLL_EVERY: Duration = Duration::from_secs(5);
const INSTALL: &str = "brew install telepresenceio/telepresence/telepresence-oss";
const NS_BUTTON: &str = "tp-namespace";
const DOCS: &str = "https://telepresence.io/docs/install/client";

type El<'a> = Element<'a, Message>;

#[derive(Default)]
pub(super) struct TpState {
    /// `None` until the first check finishes.
    pub(super) snapshot: Option<Result<Snapshot, TpError>>,
    loading: bool,
    polled: Option<Instant>,
    /// A command in flight, e.g. "Connecting…".
    busy: Option<String>,
    /// The workload a command in flight is about, and what to show on its row
    /// ("Starting intercept…").
    row_busy: Option<(String, String)>,
    /// The last command's failure: what failed, the error, and a command line to offer for
    /// a terminal.
    error: Option<(String, String, Option<String>)>,
    pub(super) dialog: Option<InterceptDialog>,
    /// Asking for the administrator password a first connect needs.
    pub(super) password: Option<PasswordDialog>,
    /// The connect waiting on that password.
    pending: Option<PendingConnect>,
    /// Namespace picked for the next connect; `None` follows the tab's selection.
    namespace: Option<String>,
}

impl TpState {
    pub(super) fn connected(&self) -> Option<&Snapshot> {
        match &self.snapshot {
            Some(Ok(s)) if s.status.connected => Some(s),
            _ => None,
        }
    }

    fn root_running(&self) -> bool {
        matches!(&self.snapshot, Some(Ok(s)) if s.status.root_running)
    }
}

pub(super) struct InterceptDialog {
    workload: String,
    namespace: String,
    port: String,
}

pub(super) struct PasswordDialog {
    text: String,
    error: Option<String>,
}

#[derive(Clone)]
struct PendingConnect {
    args: Vec<String>,
    busy: String,
    done: String,
}

/// A background command: what to show while it runs, after it succeeds, and if it fails.
pub(super) struct TpJob {
    pub(super) done: String,
    pub(super) fail: String,
    /// A command line the user can run in a terminal instead, offered when it fails.
    pub(super) command: Option<String>,
    /// It was a connect, so a password prompt may follow.
    pub(super) connect: bool,
}

pub(super) const PASSWORD_INPUT: &str = "tp-password";

#[derive(Clone, Debug)]
pub enum TpMsg {
    Refresh,
    Connect,
    Disconnect,
    Intercept(String),
    Port(String),
    Start,
    Cancel,
    /// (workload, intercept name)
    Leave(String, String),
    /// Pick the namespace; reconnects there when already connected elsewhere.
    Namespace(String),
    Password(String),
    PasswordSubmit,
    PasswordCancel,
    DismissError,
    #[cfg_attr(not(target_os = "macos"), allow(dead_code))]
    RunInTerminal(String),
}

impl App {
    /// Re-reads the status unless a read is already running.
    pub(super) fn tp_refresh(&mut self) {
        if self.tp.loading {
            return;
        }
        self.tp.loading = true;
        self.tp.polled = Some(Instant::now());
        let bus = self.bus.clone();
        self.rt.spawn(async move {
            bus.send(Msg::TpSnapshot(tp::snapshot().await));
        });
    }

    /// Called every tick: keeps the page current while it's showing.
    pub(super) fn tp_poll(&mut self) {
        let showing = self.ws().is_some_and(|w| w.tab == Tab::Telepresence);
        let due = self.tp.polled.is_none_or(|t| t.elapsed() >= POLL_EVERY);
        if showing && due && self.tp.busy.is_none() {
            self.tp_refresh();
        }
    }

    pub(super) fn tp_snapshot(&mut self, result: Result<Snapshot, TpError>) {
        self.tp.loading = false;
        self.tp.polled = Some(Instant::now());
        self.tp.snapshot = Some(result);
    }

    pub(super) fn tp_done(&mut self, job: TpJob, result: Result<String, TpError>) -> Task<Message> {
        self.tp.busy = None;
        self.tp.row_busy = None;
        // A read started before the command finished may be stale; read again.
        self.tp.loading = false;
        self.tp_refresh();
        match result {
            Ok(_) => {
                self.tp.error = None;
                self.tp.pending = None;
                self.toast(job.done, false);
            }
            Err(e @ (TpError::NeedPassword | TpError::WrongPassword))
                if job.connect && self.tp.pending.is_some() =>
            {
                self.tp.password = Some(PasswordDialog {
                    text: String::new(),
                    error: matches!(e, TpError::WrongPassword)
                        .then(|| "Incorrect password. Try again.".to_string()),
                });
                return operation::focus(PASSWORD_INPUT);
            }
            Err(e) => {
                self.tp.pending = None;
                self.tp.error = Some((job.fail, e.text(), job.command));
            }
        }
        Task::none()
    }

    /// Runs a CLI command in the background, showing `busy` meanwhile (and on `row`'s line
    /// in the workload list).
    fn tp_run<F>(&mut self, busy: String, row: Option<String>, job: TpJob, run: F)
    where
        F: std::future::Future<Output = Result<String, TpError>> + Send + 'static,
    {
        self.tp.row_busy = row.map(|w| (w, busy.clone()));
        self.tp.busy = Some(busy);
        self.tp.error = None;
        let bus = self.bus.clone();
        self.rt.spawn(async move {
            let result = run.await;
            bus.send(Msg::TpDone { job, result });
        });
    }

    /// Connects (after quitting the current connection when `quit_first`). Without a
    /// password, one that needs it comes back as `NeedPassword` and opens the prompt.
    fn tp_connect(
        &mut self,
        pending: PendingConnect,
        quit_first: bool,
        password: Option<String>,
    ) {
        let root_running = self.tp.root_running();
        let line = tp::command_line(&pending.args);
        self.tp.pending = Some(pending.clone());
        let PendingConnect { args, busy, done } = pending;
        self.tp_run(
            busy,
            None,
            TpJob {
                done,
                fail: "Couldn't connect".into(),
                command: Some(line),
                connect: true,
            },
            async move {
                if quit_first {
                    tp::quit().await?;
                }
                tp::connect(args, password, root_running).await
            },
        );
    }

    /// Screenshot aid: shows the password prompt as if a connect needed it.
    pub(super) fn tp_dev_password(&mut self) {
        self.tp.password = Some(PasswordDialog {
            text: "hunter2".into(),
            error: Some("Incorrect password. Try again.".into()),
        });
    }

    pub(super) fn tp_update(&mut self, msg: TpMsg) -> Task<Message> {
        match msg {
            // A read already running answers this too.
            TpMsg::Refresh => self.tp_refresh(),
            TpMsg::Connect => {
                let Some(ws) = self.ws() else {
                    return Task::none();
                };
                let Some(context) = ws.context.clone() else {
                    self.toast("Pick a context first", true);
                    return Task::none();
                };
                let namespace = self.tp_namespace();
                let args = tp::connect_args(&ws.kubeconfig_path, &context, namespace.as_deref());
                self.tp_connect(
                    PendingConnect {
                        args,
                        busy: format!("Connecting to {context}…"),
                        done: format!("Telepresence connected to {context}"),
                    },
                    false,
                    None,
                );
            }
            TpMsg::Disconnect => self.tp_run(
                "Disconnecting…".into(),
                None,
                TpJob {
                    done: "Telepresence disconnected".into(),
                    fail: "Couldn't disconnect".into(),
                    command: None,
                    connect: false,
                },
                tp::quit(),
            ),
            TpMsg::Intercept(workload) => {
                let namespace = self
                    .tp
                    .connected()
                    .map(|s| s.status.namespace.clone())
                    .unwrap_or_default();
                self.tp.dialog = Some(InterceptDialog {
                    workload,
                    namespace,
                    port: "8080".into(),
                });
            }
            TpMsg::Port(p) => {
                if let Some(d) = self.tp.dialog.as_mut() {
                    d.port = p;
                }
            }
            TpMsg::Start => {
                let Some(d) = self.tp.dialog.as_ref() else {
                    return Task::none();
                };
                if !valid_port(&d.port) {
                    return Task::none();
                }
                let d = self.tp.dialog.take().unwrap();
                let port = d.port.trim().to_string();
                let w = d.workload;
                self.tp_run(
                    "Starting intercept…".into(),
                    Some(w.clone()),
                    TpJob {
                        done: format!("Intercepting {w}"),
                        fail: format!("Couldn't intercept {w}"),
                        command: None,
                        connect: false,
                    },
                    tp::intercept(w, port),
                );
            }
            TpMsg::Cancel => self.tp.dialog = None,
            TpMsg::Leave(workload, name) => self.tp_run(
                "Leaving…".into(),
                Some(workload),
                TpJob {
                    done: format!("Left intercept {name}"),
                    fail: format!("Couldn't leave {name}"),
                    command: None,
                    connect: false,
                },
                tp::leave(name),
            ),
            TpMsg::Namespace(ns) => {
                self.popup = None;
                let current = self.tp.connected().map(|s| s.status.namespace.clone());
                self.tp.namespace = Some(ns.clone());
                let Some(current) = current else {
                    return Task::none(); // Used by the next connect.
                };
                if current == ns {
                    return Task::none();
                }
                let Some(ws) = self.ws() else {
                    return Task::none();
                };
                let Some(context) = ws.context.clone() else {
                    return Task::none();
                };
                let args = tp::connect_args(&ws.kubeconfig_path, &context, Some(&ns));
                self.tp_connect(
                    PendingConnect {
                        args,
                        busy: format!("Switching to {ns}…"),
                        done: format!("Telepresence now in {ns}"),
                    },
                    true,
                    None,
                );
            }
            TpMsg::Password(text) => {
                if let Some(d) = self.tp.password.as_mut() {
                    d.text = text;
                }
            }
            TpMsg::PasswordSubmit => {
                let ready = self.tp.password.as_ref().is_some_and(|d| !d.text.is_empty());
                if !ready {
                    return Task::none();
                }
                let password = self.tp.password.take().map(|d| d.text);
                if let Some(pending) = self.tp.pending.take() {
                    self.tp_connect(pending, false, password);
                }
            }
            TpMsg::PasswordCancel => {
                self.tp.password = None;
                self.tp.pending = None;
            }
            TpMsg::DismissError => self.tp.error = None,
            TpMsg::RunInTerminal(line) => run_in_terminal(&line, &self.bus),
        }
        Task::none()
    }

    /// The namespace the next connect asks for: the one picked here, else the tab's single
    /// selected namespace, else none (the context's default).
    fn tp_namespace(&self) -> Option<String> {
        self.tp.namespace.clone().or_else(|| {
            match self.ws()?.selected_ns.as_slice() {
                [ns] => Some(ns.clone()),
                _ => None,
            }
        })
    }

    /// Namespaces offered by the picker: the cluster's, plus the connected one.
    pub(super) fn tp_namespaces(&self) -> Vec<String> {
        let mut list = self.ws().map(|w| w.namespaces.clone()).unwrap_or_default();
        if let Some(s) = self.tp.connected() {
            if !s.status.namespace.is_empty() && !list.contains(&s.status.namespace) {
                list.push(s.status.namespace.clone());
                list.sort();
            }
        }
        list
    }

    pub(super) fn popup_tp_namespace(&self, query: &str) -> El<'_> {
        let current = match self.tp.connected() {
            Some(s) => Some(s.status.namespace.clone()),
            None => self.tp_namespace(),
        };
        let q = query.to_lowercase();
        let mut col = column![Self::search_input(query), theme::hline()].spacing(4);
        let mut any = false;
        for ns in self.tp_namespaces() {
            if !ns.to_lowercase().contains(&q) {
                continue;
            }
            any = true;
            col = col.push(theme::option_entry(
                ns.clone(),
                current.as_ref() == Some(&ns),
                Message::Tp(TpMsg::Namespace(ns)),
            ));
        }
        if !any {
            col = col.push(container(theme::faint("No matches")).padding([4, 8]));
        }
        col.into()
    }

    /// Enter in the picker takes the first match.
    pub(super) fn tp_namespace_submit(&mut self, query: &str) -> Task<Message> {
        let q = query.to_lowercase();
        match self.tp_namespaces().into_iter().find(|n| n.to_lowercase().contains(&q)) {
            Some(ns) => self.tp_update(TpMsg::Namespace(ns)),
            None => Task::none(),
        }
    }

    fn tp_namespace_picker(&self, connected_ns: Option<&str>) -> El<'_> {
        let label = match connected_ns {
            Some(ns) => ns.to_string(),
            None => self.tp_namespace().unwrap_or_else(|| "Context default".into()),
        };
        container(theme::dropdown_button(
            label,
            220.0,
            self.popup_open(&PopupKind::TpNamespace),
            Message::OpenPopup(PopupKind::TpNamespace, widget::Id::new(NS_BUTTON)),
        ))
        .id(NS_BUTTON)
        .into()
    }

    // -----------------------------------------------------------------------
    // View
    // -----------------------------------------------------------------------

    pub(super) fn tp_badge(&self) -> theme::NavBadge {
        let s = self.tp.connected()?;
        let n: usize = s.workloads.iter().map(|w| w.intercepts.len()).sum();
        Some((
            if n > 0 {
                format!("{n} live")
            } else {
                "on".into()
            },
            Some(GREEN),
        ))
    }

    pub(super) fn view_telepresence(&self) -> El<'_> {
        let header = row![
            theme::title("Telepresence"),
            theme::fill_space(),
            theme::small_button(
                Some(icon::ARROW_CLOCKWISE),
                "Refresh",
                Some(Message::Tp(TpMsg::Refresh)),
            ),
        ]
        .align_y(iced::Center);
        let mut col = column![header, theme::vspace(12.0)];
        let snap = match &self.tp.snapshot {
            None => return col.push(theme::muted("Checking Telepresence…")).into(),
            Some(Err(TpError::NotInstalled)) => return col.push(not_installed()).into(),
            Some(Err(e)) => {
                return col
                    .push(theme::colored(e.text(), RED))
                    .push(theme::vspace(8.0))
                    .push(self.tp_error())
                    .into();
            }
            Some(Ok(s)) => s,
        };
        col = col.push(self.tp_status(snap)).push(self.tp_error());
        if snap.status.connected {
            col = col
                .push(self.tp_intercepts(snap))
                .push(theme::vspace(16.0))
                .push(self.tp_workloads(snap));
        }
        col.into()
    }

    fn tp_status<'a>(&'a self, snap: &'a Snapshot) -> El<'a> {
        if snap.status.connected {
            return self.tp_status_bar(snap);
        }
        let s = &snap.status;
        let (color, label) = if s.connected {
            (GREEN, s.status.as_str())
        } else {
            (theme::IDLE, s.status.as_str())
        };
        let ws_context = self.ws().and_then(|w| w.context.as_deref());
        let busy = self.tp.busy.is_some();
        let mut buttons = row![].spacing(8).align_y(iced::Center);
        if s.connected {
            buttons = buttons.push(theme::secondary_button(
                Some(icon::PLUGS),
                "Disconnect",
                (!busy).then_some(Message::Tp(TpMsg::Disconnect)),
            ));
        } else if let Some(ctx) = ws_context {
            buttons = buttons.push(theme::primary_button(
                Some(icon::PLUGS_CONNECTED),
                format!("Connect to {ctx}"),
                (!busy).then_some(Message::Tp(TpMsg::Connect)),
            ));
        }
        if let Some(b) = &self.tp.busy {
            buttons = buttons.push(theme::muted(b.as_str()));
        }
        let label_w = 150;
        let line = |k: &'a str, v: &'a str| -> El<'a> {
            row![
                theme::muted(k).width(label_w),
                if v.is_empty() {
                    theme::faint("-")
                } else {
                    theme::txt(v)
                }
            ]
            .align_y(iced::Center)
            .into()
        };
        let mut info = column![
            row![theme::dot(color), theme::semibold(label).color(color)]
                .spacing(8)
                .align_y(iced::Center),
            theme::vspace(4.0),
        ]
        .spacing(6);
        let ns_row = |picker: El<'a>| -> El<'a> {
            row![theme::muted("Namespace").width(label_w), picker]
                .align_y(iced::Center)
                .into()
        };
        if s.connected {
            info = info
                .push(line("Context", &s.context))
                .push(ns_row(self.tp_namespace_picker(Some(&s.namespace))))
                .push(line("Server", &s.server))
                .push(line("Traffic manager", &s.manager_version));
        } else if ws_context.is_some() {
            info = info.push(ns_row(self.tp_namespace_picker(None)));
        }
        info = info.push(line("Client", &s.version));
        if s.connected && ws_context.is_some_and(|c| c != s.context) {
            info = info.push(theme::colored(
                "Connected to a different context than this tab. Disconnect to switch.",
                YELLOW,
            ));
        }
        container(column![info, theme::vspace(12.0), buttons])
            .padding(14)
            .width(Length::Fill)
            .style(theme::card)
            .into()
    }

    /// Connected: everything on one line, so the workload list keeps the height.
    fn tp_status_bar<'a>(&'a self, snap: &'a Snapshot) -> El<'a> {
        let s = &snap.status;
        let busy = self.tp.busy.is_some();
        let pair = |k: &'a str, v: El<'a>| -> El<'a> {
            row![theme::muted(k), v].spacing(8).align_y(iced::Center).into()
        };
        let value = |v: &'a str| -> El<'a> {
            if v.is_empty() {
                theme::faint("-").into()
            } else {
                theme::txt(v).into()
            }
        };
        let mut bar = row![
            row![theme::dot(GREEN), theme::semibold(s.status.as_str()).color(GREEN)]
                .spacing(8)
                .align_y(iced::Center),
            theme::vline(18.0),
            pair("Context", theme::tip(value(&s.context), s.server.clone())),
            pair("Namespace", self.tp_namespace_picker(Some(&s.namespace))),
            pair("Manager", value(&s.manager_version)),
            pair("Client", value(&s.version)),
            theme::fill_space(),
        ]
        .spacing(16)
        .align_y(iced::Center);
        if let Some(b) = &self.tp.busy {
            bar = bar.push(theme::muted(b.as_str()));
        }
        bar = bar.push(theme::secondary_button(
            Some(icon::PLUGS),
            "Disconnect",
            (!busy).then_some(Message::Tp(TpMsg::Disconnect)),
        ));
        let mut col = column![bar].spacing(8);
        let ws_context = self.ws().and_then(|w| w.context.as_deref());
        if ws_context.is_some_and(|c| c != s.context) {
            col = col.push(theme::colored(
                "Connected to a different context than this tab. Disconnect to switch.",
                YELLOW,
            ));
        }
        container(col)
            .padding([10, 14])
            .width(Length::Fill)
            .style(theme::card)
            .into()
    }

    fn tp_error(&self) -> El<'_> {
        let Some((title, msg, command)) = &self.tp.error else {
            return Space::new().into();
        };
        let mut actions = row![].spacing(8).align_y(iced::Center);
        if let Some(line) = command {
            #[cfg(target_os = "macos")]
            {
                actions = actions.push(theme::small_button(
                    Some(icon::TERMINAL_WINDOW),
                    "Run in Terminal",
                    Some(Message::Tp(TpMsg::RunInTerminal(line.clone()))),
                ));
            }
            actions = actions.push(theme::small_button(
                Some(icon::COPY),
                "Copy command",
                Some(Message::Copy(line.clone())),
            ));
        }
        actions = actions.push(theme::small_button(
            None,
            "Dismiss",
            Some(Message::Tp(TpMsg::DismissError)),
        ));
        let body = column![
            theme::semibold(title.as_str()).color(RED),
            theme::mono(msg.as_str()).color(RED),
        ]
        .spacing(10);
        column![
            theme::vspace(12.0),
            container(body.push(actions))
                .padding(12)
                .width(Length::Fill)
                .style(theme::boxed(theme::alpha(RED, 0.08), theme::alpha(RED, 0.4), 6.0)),
        ]
        .into()
    }

    fn tp_workloads<'a>(&'a self, snap: &'a Snapshot) -> El<'a> {
        let ws = self.ws();
        // The list pages' filter, kept per page: matches the name or the kind.
        let rows: Vec<&tp::Workload> = snap
            .workloads
            .iter()
            .filter(|w| ws.is_none_or(|ws| ws.matches(&w.name, &w.kind)))
            .collect();
        let total = snap.workloads.len();
        let count = if rows.len() < total {
            format!("{} of {total}", rows.len())
        } else {
            total.to_string()
        };
        let mut heading = row![
            theme::label_small("Workloads"),
            theme::faint(format!("in {}", snap.status.namespace)),
            theme::faint(count),
            theme::hspace(6.0),
        ]
        .spacing(8)
        .align_y(iced::Center);
        if let Some(ws) = ws {
            let filter = ws.filters.get(&Tab::Telepresence).map(String::as_str).unwrap_or("");
            let hint = if ws.filter_regex {
                "Regex on name or kind"
            } else {
                "Filter by name or kind"
            };
            heading = heading
                .push(theme::search_field(filter, hint, 300.0, Message::Filter))
                .push(theme::toggle_button(
                    ".*",
                    ws.filter_regex,
                    "Regular expression (case-insensitive)",
                    Message::ToggleRegex,
                ));
            if !filter.is_empty() {
                heading = heading.push(theme::icon_button(
                    icon::X,
                    "Clear filter",
                    Some(Message::ClearFilter),
                ));
            }
            if let Some(err) = &ws.matcher.error {
                heading = heading.push(theme::tip(
                    row![
                        theme::icon(icon::WARNING).color(RED),
                        theme::colored("Invalid regex", RED)
                    ]
                    .spacing(5)
                    .align_y(iced::Center),
                    err.clone(),
                ));
            }
        }
        if let Some(e) = &snap.list_error {
            return column![heading, theme::vspace(6.0), theme::colored(e.as_str(), RED)].into();
        }
        if rows.is_empty() && total > 0 {
            return column![heading, theme::vspace(6.0), theme::muted("No workloads match the filter.")]
                .into();
        }
        if snap.workloads.is_empty() {
            return column![
                heading,
                theme::vspace(6.0),
                theme::muted("No workloads to intercept in this namespace.")
            ]
            .into();
        }
        let busy = self.tp.busy.is_some();
        let scroll = self.ws().map(|w| w.list_scroll).unwrap_or(0.0);
        let table = table::Table {
            id: "list",
            cols: vec![
                table::Col::fill("Name", 200.0),
                table::Col::fixed("Kind", 130.0),
                table::Col::fill("Intercept", 220.0),
                table::Col::fixed("Agent", 90.0),
                table::Col::fixed("", 130.0),
            ],
            sort: None,
            tab: Tab::Telepresence,
            sortable: false,
            widths: &self.settings.column_widths,
            len: rows.len(),
            scroll,
            hovered: self.hovered_row,
            row: move |i, w: &[f32]| {
                let wl: &tp::Workload = rows[i];
                let working = self
                    .tp
                    .row_busy
                    .as_ref()
                    .filter(|(w, _)| *w == wl.name)
                    .map(|(_, label)| label.as_str());
                let intercept: El<'_> = match (working, wl.intercepts.first()) {
                    (Some(label), _) => row![theme::dot(YELLOW), theme::colored(label, YELLOW)]
                        .spacing(6)
                        .align_y(iced::Center)
                        .into(),
                    (None, None) => theme::faint("-").into(),
                    (None, Some(ic)) => intercept_cell(ic),
                };
                let control = match wl.intercepts.first() {
                    Some(ic) => theme::small_button(
                        Some(icon::SIGN_OUT),
                        "Leave",
                        (!busy).then(|| {
                            Message::Tp(TpMsg::Leave(wl.name.clone(), ic.name.clone()))
                        }),
                    ),
                    None => theme::small_button(
                        Some(icon::ARROW_BEND_DOWN_RIGHT),
                        "Intercept",
                        (!busy).then(|| Message::Tp(TpMsg::Intercept(wl.name.clone()))),
                    ),
                };
                table::Row {
                    cells: vec![
                        table::truncated(&wl.name, w[0], 7.6, theme::txt),
                        table::truncated(&wl.kind, w[1], 7.6, theme::txt),
                        intercept,
                        if wl.agent {
                            theme::colored("Installed", GREEN).into()
                        } else {
                            theme::faint("-").into()
                        },
                        control,
                    ],
                    selected: false,
                    on_click: Message::Noop,
                    on_double_click: None,
                }
            },
        };
        column![heading, theme::vspace(8.0), table.view()]
            .height(Length::Fill)
            .into()
    }

    /// Every active intercept with its state, above the workload list.
    fn tp_intercepts<'a>(&'a self, snap: &'a Snapshot) -> El<'a> {
        let busy = self.tp.busy.is_some();
        let mut items: Vec<(&'a str, Option<&'a tp::Intercept>, &'a str)> = snap
            .workloads
            .iter()
            .flat_map(|w| w.intercepts.iter().map(move |i| (w.name.as_str(), Some(i), i.name.as_str())))
            .collect();
        // Intercepts the daemon knows about that the list didn't show (another namespace).
        for name in &snap.status.intercepts {
            if !items.iter().any(|(_, _, n)| n == name) {
                items.push((name.as_str(), None, name.as_str()));
            }
        }
        if items.is_empty() {
            return Space::new().into();
        }
        let mut list = column![].spacing(6);
        for (workload, ic, name) in items {
            let working = self
                .tp
                .row_busy
                .as_ref()
                .filter(|(w, _)| w == workload)
                .map(|(_, label)| label.as_str());
            let (ok, label) = match ic {
                Some(ic) if !intercept_ok(ic) => (false, disposition_label(&ic.disposition)),
                _ => (true, "Active".to_string()),
            };
            let color = if ok { GREEN } else { RED };
            let state: El<'a> = match working {
                Some(w) => theme::colored(w, YELLOW).into(),
                None => {
                    let t = theme::colored(label, color);
                    match ic.map(|i| i.message.as_str()).filter(|m| !m.is_empty()) {
                        Some(msg) => theme::tip(t, msg.to_string()),
                        None => t.into(),
                    }
                }
            };
            let mut line = row![
                theme::dot(if working.is_some() { YELLOW } else { color }),
                theme::semibold(workload),
                state,
            ]
            .spacing(10)
            .align_y(iced::Center);
            if let Some(ic) = ic {
                line = line.push(mapping(ic, color));
                if !ic.pod.is_empty() {
                    line = line.push(theme::tip(theme::faint(ic.pod.as_str()), "Pod"));
                }
            }
            line = line.push(theme::fill_space()).push(theme::small_button(
                Some(icon::SIGN_OUT),
                "Leave",
                (!busy).then(|| Message::Tp(TpMsg::Leave(workload.to_string(), name.to_string()))),
            ));
            list = list.push(line);
        }
        column![
            theme::vspace(12.0),
            theme::label_small("Active intercepts"),
            theme::vspace(6.0),
            container(list)
                .padding([10, 14])
                .width(Length::Fill)
                .style(theme::card),
        ]
        .into()
    }

    pub(super) fn view_password_dialog<'a>(&self, d: &'a PasswordDialog) -> El<'a> {
        let ready = !d.text.is_empty();
        let mut col = column![
            Self::dialog_title(
                "Administrator password".into(),
                Message::Tp(TpMsg::PasswordCancel)
            ),
            theme::vspace(4.0),
            theme::txt(
                "Telepresence needs administrator access to start its network daemon. \
                 This is asked once, until the daemon stops.",
            ),
            text_input("Password for this Mac", &d.text)
                .id(widget::Id::new(PASSWORD_INPUT))
                .secure(true)
                .on_input(|t| Message::Tp(TpMsg::Password(t)))
                .on_submit(Message::Tp(TpMsg::PasswordSubmit))
                .size(theme::BODY)
                .padding([7, 10])
                .style(theme::input),
        ]
        .spacing(10);
        if let Some(e) = &d.error {
            col = col.push(theme::colored(e.as_str(), RED));
        }
        col = col.push(theme::faint(
            "Passed straight to sudo, never saved.",
        ));
        col = col.push(theme::vspace(4.0)).push(
            row![
                theme::primary_button(
                    Some(icon::PLUGS_CONNECTED),
                    "Connect",
                    ready.then_some(Message::Tp(TpMsg::PasswordSubmit)),
                ),
                theme::secondary_button(
                    None,
                    "Cancel",
                    Some(Message::Tp(TpMsg::PasswordCancel))
                ),
            ]
            .spacing(8),
        );
        container(col)
            .padding(16)
            .width(440)
            .style(theme::dialog)
            .into()
    }

    pub(super) fn view_intercept_dialog<'a>(&self, d: &'a InterceptDialog) -> El<'a> {
        let label = |t: &'a str| theme::txt(t).width(100);
        let ok = valid_port(&d.port);
        container(
            column![
                Self::dialog_title(
                    format!("Intercept {}", d.workload),
                    Message::Tp(TpMsg::Cancel)
                ),
                theme::vspace(6.0),
                row![label("Namespace"), theme::txt(d.namespace.as_str())]
                    .align_y(iced::Center),
                row![
                    label("Port"),
                    text_input("8080", &d.port)
                        .on_input(|p| Message::Tp(TpMsg::Port(p)))
                        .on_submit(Message::Tp(TpMsg::Start))
                        .size(theme::BODY)
                        .padding([5, 8])
                        .width(140)
                        .style(theme::input),
                    theme::hspace(8.0),
                    theme::faint("local, or local:service-port"),
                ]
                .align_y(iced::Center),
                theme::muted(
                    "Traffic for the workload goes to this port on your machine until you leave.",
                ),
                theme::vspace(8.0),
                row![
                    theme::primary_button(
                        Some(icon::ARROW_BEND_DOWN_RIGHT),
                        "Start intercept",
                        ok.then_some(Message::Tp(TpMsg::Start)),
                    ),
                    theme::secondary_button(None, "Cancel", Some(Message::Tp(TpMsg::Cancel))),
                ]
                .spacing(8),
            ]
            .spacing(10),
        )
        .padding(16)
        .max_width(520)
        .style(theme::dialog)
        .into()
    }
}

fn intercept_ok(ic: &tp::Intercept) -> bool {
    ic.disposition.is_empty() || ic.disposition.eq_ignore_ascii_case("active")
}

/// "NO_AGENT" → "No agent".
fn disposition_label(d: &str) -> String {
    let lower = d.replace('_', " ").to_lowercase();
    let mut chars = lower.chars();
    chars
        .next()
        .map(|c| c.to_uppercase().chain(chars).collect())
        .unwrap_or_default()
}

/// "admin-api (80) → 127.0.0.1:6969".
fn mapping<'a>(ic: &tp::Intercept, color: Color) -> El<'a> {
    let port = if ic.port.is_empty() {
        String::new()
    } else {
        format!("{} ", ic.port)
    };
    let host = if ic.host.is_empty() { "localhost" } else { ic.host.as_str() };
    theme::mono(format!("{port}→ {host}:{}", ic.local_port))
        .color(color)
        .into()
}

/// The mapping with a dot, green when active, red (with the reason on hover) otherwise.
fn intercept_cell<'a>(ic: &'a tp::Intercept) -> El<'a> {
    let ok = intercept_ok(ic);
    let color = if ok { GREEN } else { RED };
    let r = row![theme::dot(color), mapping(ic, color)]
    .spacing(6)
    .align_y(iced::Center);
    if ok {
        r.into()
    } else {
        let why = if ic.message.is_empty() {
            disposition_label(&ic.disposition)
        } else {
            format!("{}: {}", disposition_label(&ic.disposition), ic.message)
        };
        theme::tip(r, why)
    }
}

/// `8080` or `8080:http` / `8080:80`.
fn valid_port(s: &str) -> bool {
    let s = s.trim();
    let (local, remote) = match s.split_once(':') {
        Some((l, r)) => (l, Some(r)),
        None => (s, None),
    };
    local.parse::<u16>().is_ok_and(|p| p > 0)
        && remote.is_none_or(|r| {
            !r.is_empty() && r.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
        })
}

fn not_installed<'a>() -> El<'a> {
    container(
        column![
            row![theme::dot(theme::IDLE), theme::semibold("Telepresence isn't installed")]
                .spacing(8)
                .align_y(iced::Center),
            theme::muted("Install the CLI, then refresh. With Homebrew:"),
            row![
                container(theme::mono(INSTALL))
                    .padding([6, 10])
                    .style(theme::well),
                theme::small_button(Some(icon::COPY), "Copy", Some(Message::Copy(INSTALL.into()))),
            ]
            .spacing(8)
            .align_y(iced::Center),
            button(theme::txt("Other ways to install").color(theme::ACCENT))
                .padding(0)
                .style(theme::link(theme::ACCENT))
                .on_press(Message::OpenUrl(DOCS.into())),
        ]
        .spacing(10),
    )
    .padding(14)
    .width(Length::Fill)
    .style(theme::card)
    .into()
}

/// Opens Terminal.app running `line`.
#[cfg(target_os = "macos")]
fn run_in_terminal(line: &str, bus: &Bus) {
    let quoted = line.replace('\\', "\\\\").replace('"', "\\\"");
    let result = std::process::Command::new("osascript")
        .args([
            "-e",
            &format!("tell application \"Terminal\" to do script \"{quoted}\""),
            "-e",
            "tell application \"Terminal\" to activate",
        ])
        .spawn();
    if let Err(e) = result {
        bus.send(Msg::Toast {
            text: format!("Couldn't open Terminal: {e}"),
            error: true,
        });
    }
}

#[cfg(not(target_os = "macos"))]
fn run_in_terminal(_line: &str, _bus: &Bus) {}

#[cfg(test)]
mod tests {
    use super::valid_port;

    #[test]
    fn ports() {
        assert!(valid_port("8080"));
        assert!(valid_port(" 8080:http "));
        assert!(valid_port("3000:80"));
        assert!(!valid_port("0"));
        assert!(!valid_port("abc"));
        assert!(!valid_port("8080:"));
        assert!(!valid_port("70000"));
        assert_eq!(super::disposition_label("NO_AGENT"), "No agent");
    }
}
