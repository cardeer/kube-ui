//! Rendering for a cluster tab: the resource lists, the Overview and the detail drawer.

use iced::{
    Element, Length,
    widget::{Space, button, column, container, mouse_area, responsive, row, scrollable, text},
};

use super::*;
use crate::app::table::{self, Col, Row, Table};

/// What the tab's views need from the app around them.
pub(in crate::app) struct ViewCtx<'a> {
    pub hovered: Option<usize>,
    pub popup: Option<&'a PopupKind>,
    pub env_filter: &'a str,
    pub toggled: &'a HashSet<String>,
    pub drawer_width: f32,
    /// Table column widths the user dragged.
    pub widths: &'a BTreeMap<String, f32>,
}

impl ViewCtx<'_> {
    /// Whether a collapsible section is open (its default, unless the user flipped it).
    fn open(&self, key: &str, default: bool) -> bool {
        default != self.toggled.contains(key)
    }
}

type El<'a> = Element<'a, Message>;

fn act(a: Action) -> Message {
    Message::Action(a)
}

impl Workspace {
    // -----------------------------------------------------------------------
    // Lists
    // -----------------------------------------------------------------------

    /// The current page (Overview or a list).
    pub(in crate::app) fn view_page<'a>(&'a self, ctx: &ViewCtx<'a>) -> El<'a> {
        match self.tab {
            Tab::Overview => self.view_overview(ctx),
            Tab::Pods => self.view_pods(ctx),
            Tab::Services => self.view_services(ctx),
            Tab::Deployments => self.view_deployments(ctx),
            Tab::Workload(kind) => self.view_workloads(kind, ctx),
            _ => Space::new().into(),
        }
    }

    fn filter_bar<'a>(&'a self, title: &'a str, shown: usize, total: Option<usize>) -> El<'a> {
        let mut bar = row![theme::title(title)].spacing(10).align_y(iced::Center);
        if let Some(n) = total {
            let label = if shown < n {
                format!("{shown} of {n}")
            } else {
                n.to_string()
            };
            bar = bar.push(text(label).size(15.0).color(theme::TEXT_FAINT));
        }
        bar = bar.push(theme::hspace(6.0));
        let hint = if self.filter_regex {
            "Regex on name or namespace"
        } else {
            "Filter by name or namespace"
        };
        let filter = self
            .filters
            .get(&self.tab)
            .map(String::as_str)
            .unwrap_or("");
        bar = bar.push(theme::search_field(filter, hint, 300.0, Message::Filter));
        bar = bar.push(theme::toggle_button(
            ".*",
            self.filter_regex,
            "Regular expression (case-insensitive)",
            Message::ToggleRegex,
        ));
        if !filter.is_empty() {
            bar = bar.push(theme::icon_button(
                icon::X,
                "Clear filter",
                Some(Message::ClearFilter),
            ));
        }
        if let Some(err) = &self.matcher.error {
            bar = bar.push(theme::tip(
                row![
                    theme::icon(icon::WARNING).color(theme::BAD),
                    theme::colored("Invalid regex", theme::BAD)
                ]
                .spacing(5)
                .align_y(iced::Center),
                err.clone(),
            ));
        }
        if let Some(kind) = self
            .tab
            .kinds()
            .first()
            .copied()
            .filter(|_| self.tab.is_list())
        {
            bar = bar.push(theme::fill_space());
            bar = bar.push(theme::tip(
                theme::primary_button(
                    Some(icon::PLUS),
                    "Create",
                    matches!(self.conn, Conn::Ready(_)).then(|| act(Action::Create(kind))),
                ),
                format!("New {} from a template", kind.label()),
            ));
        }
        column![bar, theme::vspace(10.0)].into()
    }

    /// Connection problems and progress; `None` once connected.
    fn conn_preamble<'a>(&self) -> Option<El<'a>> {
        match &self.conn {
            Conn::Failed(e) => Some(theme::colored(format!("Could not connect: {e}"), RED).into()),
            Conn::Connecting => Some(theme::muted("Connecting to cluster…").into()),
            Conn::Idle => Some(theme::muted("Select a context to begin.").into()),
            Conn::Ready(_) => None,
        }
    }

    /// The page under the filter bar: connection and list problems, empty and loading
    /// states, or the table.
    fn list_body<'a, T>(&self, state: &'a ListState<T>, table: impl FnOnce() -> El<'a>) -> El<'a> {
        if let Some(p) = self.conn_preamble() {
            return p;
        }
        let mut col = column![];
        if let Some(e) = &state.error {
            col = col.push(error_callout(e));
        }
        if !state.loaded {
            col = col.push(theme::muted("Loading…"));
        } else if state.items.is_empty() {
            if state.error.is_none() {
                col = col.push(theme::muted("Nothing here."));
            }
        } else {
            col = col.push(table());
        }
        col.height(Length::Fill).into()
    }

    fn is_selected(&self, kind: Kind, namespace: &str, name: &str) -> bool {
        self.selected
            .as_ref()
            .is_some_and(|s| s.kind == kind && s.namespace == namespace && s.name == name)
    }

    fn select_msg(kind: Kind, namespace: &str, name: &str) -> Message {
        act(Action::Select(Selected {
            kind,
            namespace: namespace.to_string(),
            name: name.to_string(),
        }))
    }

    fn view_pods<'a>(&'a self, ctx: &ViewCtx<'a>) -> El<'a> {
        let mut rows: Vec<&PodRow> = self
            .pods
            .items
            .iter()
            .filter(|p| self.matches(&p.name, &p.namespace))
            .collect();
        let sort = self.sorts.get(&Tab::Pods);
        sort_rows(&mut rows, sort, |p, col| match col {
            "Status" => SortKey::text(&p.status),
            "Restarts" => SortKey::Num(p.restarts as i64),
            "Ready" => SortKey::text(&p.ready),
            "Age" => SortKey::age(p.created.as_ref()),
            "Namespace" => SortKey::text(&p.namespace),
            "Node" => SortKey::text(&p.node),
            _ => SortKey::text(&p.name),
        });
        let bar = self.filter_bar("Pods", rows.len(), count(&self.pods));
        let show_ns = self.selected_ns.len() != 1;
        // With the drawer open, Node lives in the drawer; give its width to the name.
        let show_node = self.selected.is_none();
        let body = self.list_body(&self.pods, || {
            // State first (status, restarts, ready) so it stays visible next to the drawer.
            let mut cols = vec![
                Col::fill("Name", 180.0),
                Col::fixed("Status", 132.0),
                Col::fixed("Restarts", 80.0),
                Col::fixed("Ready", 64.0),
                Col::fixed("Age", 72.0),
            ];
            if show_ns {
                cols.push(Col::fixed("Namespace", 110.0));
            }
            if show_node {
                cols.push(Col::fixed("Node", 190.0));
            }
            Table {
                id: "list",
                cols,
                sort,
                tab: Tab::Pods,
                sortable: true,
                widths: ctx.widths,
                len: rows.len(),
                scroll: self.list_scroll,
                hovered: ctx.hovered,
                row: move |i, w: &[f32]| {
                    let p: &PodRow = rows[i];
                    let color = status_color(&p.status);
                    // Problems read from across the room: tint anything not healthy.
                    let tint = if color == GREEN { theme::TEXT } else { color };
                    let mut cells: Vec<El<'a>> = vec![
                        table::name_cell(&p.name, w[0]),
                        row![
                            theme::dot(color),
                            table::truncated(&p.status, w[1] - 14.0, 7.6, move |s| {
                                theme::colored(s, tint)
                            })
                        ]
                        .spacing(6)
                        .align_y(iced::Center)
                        .into(),
                        restarts_badge(p.restarts),
                        theme::mono(p.ready.as_str()).into(),
                        theme::mono_muted(p.age()).into(),
                    ];
                    if show_ns {
                        cells.push(table::truncated(
                            &p.namespace.as_str(),
                            w[cells.len()],
                            7.5,
                            theme::mono_muted,
                        ));
                    }
                    if show_node {
                        cells.push(table::truncated(
                            &p.node.as_str(),
                            w[cells.len()],
                            7.5,
                            theme::mono_muted,
                        ));
                    }
                    Row {
                        cells,
                        selected: self.is_selected(Kind::Pod, &p.namespace, &p.name),
                        on_click: Self::select_msg(Kind::Pod, &p.namespace, &p.name),
                        on_double_click: Some(act(Action::OpenLogs(p.clone(), None, false))),
                    }
                },
            }
            .view()
        });
        column![bar, body].into()
    }

    fn view_services<'a>(&'a self, ctx: &ViewCtx<'a>) -> El<'a> {
        let mut rows: Vec<&SvcRow> = self
            .services
            .items
            .iter()
            .filter(|s| self.matches(&s.name, &s.namespace))
            .collect();
        let sort = self.sorts.get(&Tab::Services);
        sort_rows(&mut rows, sort, |s, col| match col {
            "Type" => SortKey::text(&s.kind),
            "Ports" => SortKey::text(&s.ports_label()),
            "Cluster IP" => SortKey::text(&s.cluster_ip),
            "Age" => SortKey::age(s.created.as_ref()),
            "Namespace" => SortKey::text(&s.namespace),
            "External" => SortKey::text(&s.external),
            _ => SortKey::text(&s.name),
        });
        let bar = self.filter_bar("Services", rows.len(), count(&self.services));
        let show_ns = self.selected_ns.len() != 1;
        let show_external = self.selected.is_none();
        let body = self.list_body(&self.services, || {
            let mut cols = vec![
                Col::fill("Name", 180.0),
                Col::fixed("Type", 96.0),
                Col::fixed("Ports", 170.0),
                Col::fixed("Cluster IP", 120.0),
                Col::fixed("Age", 72.0),
            ];
            if show_ns {
                cols.push(Col::fixed("Namespace", 110.0));
            }
            if show_external {
                cols.push(Col::fixed("External", 170.0));
            }
            Table {
                id: "list",
                cols,
                sort,
                tab: Tab::Services,
                sortable: true,
                widths: ctx.widths,
                len: rows.len(),
                scroll: self.list_scroll,
                hovered: ctx.hovered,
                row: move |i, w: &[f32]| {
                    let s: &SvcRow = rows[i];
                    let mut cells: Vec<El<'a>> = vec![
                        table::name_cell(&s.name, w[0]),
                        table::truncated(&s.kind, w[1], 7.6, theme::muted),
                        table::truncated(&s.ports_label(), w[2], 7.5, theme::mono),
                        theme::mono_muted(s.cluster_ip.as_str()).into(),
                        theme::mono_muted(s.age()).into(),
                    ];
                    if show_ns {
                        cells.push(table::truncated(
                            &s.namespace.as_str(),
                            w[cells.len()],
                            7.5,
                            theme::mono_muted,
                        ));
                    }
                    if show_external {
                        cells.push(table::truncated(
                            &s.external.as_str(),
                            w[cells.len()],
                            7.5,
                            theme::mono_muted,
                        ));
                    }
                    Row {
                        cells,
                        selected: self.is_selected(Kind::Service, &s.namespace, &s.name),
                        on_click: Self::select_msg(Kind::Service, &s.namespace, &s.name),
                        on_double_click: None,
                    }
                },
            }
            .view()
        });
        column![bar, body].into()
    }

    fn view_deployments<'a>(&'a self, ctx: &ViewCtx<'a>) -> El<'a> {
        let mut rows: Vec<&DeployRow> = self
            .deployments
            .items
            .iter()
            .filter(|d| self.matches(&d.name, &d.namespace))
            .collect();
        let sort = self.sorts.get(&Tab::Deployments);
        sort_rows(&mut rows, sort, |d, col| match col {
            // Least ready first when ascending: a fraction, scaled to keep integer keys.
            "Ready" => SortKey::Num(if d.replicas == 0 {
                1_000_000
            } else {
                d.ready as i64 * 1_000_000 / d.replicas as i64
            }),
            "Up-to-date" => SortKey::Num(d.updated as i64),
            "Available" => SortKey::Num(d.available as i64),
            "Age" => SortKey::age(d.created.as_ref()),
            "Namespace" => SortKey::text(&d.namespace),
            _ => SortKey::text(&d.name),
        });
        let bar = self.filter_bar("Deployments", rows.len(), count(&self.deployments));
        let show_ns = self.selected_ns.len() != 1;
        let body = self.list_body(&self.deployments, || {
            let mut cols = vec![
                Col::fill("Name", 180.0),
                Col::fixed("Ready", 80.0),
                Col::fixed("Up-to-date", 102.0),
                Col::fixed("Available", 92.0),
                Col::fixed("Age", 72.0),
            ];
            if show_ns {
                cols.push(Col::fixed("Namespace", 110.0));
            }
            Table {
                id: "list",
                cols,
                sort,
                tab: Tab::Deployments,
                sortable: true,
                widths: ctx.widths,
                len: rows.len(),
                scroll: self.list_scroll,
                hovered: ctx.hovered,
                row: move |i, w: &[f32]| {
                    let d: &DeployRow = rows[i];
                    let c = if d.ready >= d.replicas {
                        theme::TEXT
                    } else {
                        YELLOW
                    };
                    let mut cells: Vec<El<'a>> = vec![
                        table::name_cell(&d.name, w[0]),
                        theme::mono(format!("{}/{}", d.ready, d.replicas))
                            .color(c)
                            .into(),
                        theme::mono_muted(d.updated.to_string()).into(),
                        theme::mono_muted(d.available.to_string()).into(),
                        theme::mono_muted(d.age()).into(),
                    ];
                    if show_ns {
                        cells.push(table::truncated(
                            &d.namespace.as_str(),
                            w[cells.len()],
                            7.5,
                            theme::mono_muted,
                        ));
                    }
                    Row {
                        cells,
                        selected: self.is_selected(Kind::Deployment, &d.namespace, &d.name),
                        on_click: Self::select_msg(Kind::Deployment, &d.namespace, &d.name),
                        on_double_click: None,
                    }
                },
            }
            .view()
        });
        column![bar, body].into()
    }

    /// Daemon sets, stateful sets, replica sets, replication controllers, jobs, cron jobs.
    fn view_workloads<'a>(&'a self, kind: Kind, ctx: &ViewCtx<'a>) -> El<'a> {
        let state = self.workload(kind);
        let mut rows: Vec<&WorkRow> = state
            .items
            .iter()
            .filter(|w| self.matches(&w.name, &w.namespace))
            .collect();
        let columns = kube_ops::columns(kind);
        let sort = self.sorts.get(&Tab::Workload(kind));
        sort_rows(&mut rows, sort, |w, col| match col {
            "Age" => SortKey::age(w.created.as_ref()),
            "Namespace" => SortKey::text(&w.namespace),
            "Name" => SortKey::text(&w.name),
            col => match columns
                .iter()
                .position(|(t, _)| *t == col)
                .and_then(|i| w.cells.get(i))
            {
                Some(cell) => match cell.seconds() {
                    Some(secs) => SortKey::Num(secs),
                    None => SortKey::text(&cell.text()),
                },
                None => SortKey::text(&w.name),
            },
        });
        let bar = self.filter_bar(kind.plural(), rows.len(), count(state));
        let show_ns = self.selected_ns.len() != 1;
        // The column that carries the state gets the state colour when unhealthy.
        let state_col = columns
            .iter()
            .position(|(t, _)| matches!(*t, "Ready" | "Status"));
        let body = self.list_body(state, || {
            let mut cols = vec![Col::fill("Name", 180.0)];
            for (title, width) in columns {
                // Room for the header's sort arrow.
                cols.push(Col::fixed(*title, *width + 14.0));
            }
            cols.push(Col::fixed("Age", 72.0));
            if show_ns {
                cols.push(Col::fixed("Namespace", 110.0));
            }
            Table {
                id: "list",
                cols,
                sort,
                tab: Tab::Workload(kind),
                sortable: true,
                widths: ctx.widths,
                len: rows.len(),
                scroll: self.list_scroll,
                hovered: ctx.hovered,
                row: move |i, w: &[f32]| {
                    let r: &WorkRow = rows[i];
                    let mut cells: Vec<El<'a>> = vec![table::name_cell(&r.name, w[0])];
                    for (j, cell) in r.cells.iter().enumerate() {
                        let value = cell.text();
                        let width = w[j + 1];
                        cells.push(if Some(j) == state_col {
                            let color = health_color(r.health);
                            let c = if r.health == Health::Ok {
                                theme::TEXT
                            } else {
                                color
                            };
                            let label = table::truncated(&value, width, 7.5, move |s| {
                                theme::mono(s).color(c)
                            });
                            if columns[j].0 == "Status" {
                                row![theme::dot(color), label]
                                    .spacing(6)
                                    .align_y(iced::Center)
                                    .into()
                            } else {
                                label
                            }
                        } else if j == 0 {
                            table::truncated(&value, width, 7.5, theme::mono)
                        } else {
                            table::truncated(&value, width, 7.5, theme::mono_muted)
                        });
                    }
                    cells.push(table::truncated(
                        &r.age(),
                        w[cells.len()],
                        7.5,
                        theme::mono_muted,
                    ));
                    if show_ns {
                        cells.push(table::truncated(
                            &r.namespace.as_str(),
                            w[cells.len()],
                            7.5,
                            theme::mono_muted,
                        ));
                    }
                    Row {
                        cells,
                        selected: self.is_selected(kind, &r.namespace, &r.name),
                        on_click: Self::select_msg(kind, &r.namespace, &r.name),
                        on_double_click: None,
                    }
                },
            }
            .view()
        });
        column![bar, body].into()
    }

    // -----------------------------------------------------------------------
    // Overview
    // -----------------------------------------------------------------------

    /// Health of every row in one list: [ok, progress, bad, idle]; `None` until it has loaded.
    fn health_counts(&self, kind: Kind) -> Option<[usize; 4]> {
        let mut counts = [0usize; 4];
        let mut add = |h: Health| {
            counts[match h {
                Health::Ok => 0,
                Health::Progress => 1,
                Health::Bad => 2,
                Health::Idle => 3,
            }] += 1
        };
        match kind {
            Kind::Pod => {
                if !self.pods.loaded {
                    return None;
                }
                self.pods
                    .items
                    .iter()
                    .for_each(|p| add(pod_health(&p.status)));
            }
            Kind::Deployment => {
                if !self.deployments.loaded {
                    return None;
                }
                self.deployments
                    .items
                    .iter()
                    .for_each(|d| add(deploy_health(d)));
            }
            k => {
                let state = self.workload(k);
                if !state.loaded {
                    return None;
                }
                state.items.iter().for_each(|w| add(w.health));
            }
        }
        Some(counts)
    }

    /// Workload counts and health at a glance, plus everything that isn't healthy.
    fn view_overview<'a>(&'a self, ctx: &ViewCtx<'a>) -> El<'a> {
        let header = row![
            theme::title("Overview"),
            text(namespaces_label(&self.selected_ns, false))
                .size(15.0)
                .color(theme::TEXT_FAINT)
        ]
        .spacing(10)
        .align_y(iced::Center);
        if let Some(p) = self.conn_preamble() {
            return column![header, theme::vspace(12.0), p].into();
        }

        // Cards share the width: four across when there's room, fewer on narrow windows.
        let kinds = Tab::Overview.kinds();
        let cards = responsive(move |size| {
            const MIN_CARD_W: f32 = 232.0;
            let gap = 12.0;
            let per_row = (((size.width + gap) / (MIN_CARD_W + gap)).floor() as usize).clamp(1, 4);
            let mut grid = column![].spacing(gap);
            for chunk in kinds.chunks(per_row) {
                let mut r = row![].spacing(gap);
                for kind in chunk {
                    r = r.push(self.overview_card(*kind));
                }
                for _ in chunk.len()..per_row {
                    r = r.push(Space::new().width(Length::Fill));
                }
                grid = grid.push(r);
            }
            grid.into()
        })
        .height(Length::Shrink);

        // Everything that needs a look, worst first.
        let mut problems: Vec<(Kind, &str, &str, String, Health)> = Vec::new();
        for p in &self.pods.items {
            let h = pod_health(&p.status);
            if matches!(h, Health::Bad | Health::Progress) {
                problems.push((Kind::Pod, &p.namespace, &p.name, p.status.clone(), h));
            }
        }
        for d in &self.deployments.items {
            if deploy_health(d) == Health::Progress {
                problems.push((
                    Kind::Deployment,
                    &d.namespace,
                    &d.name,
                    format!("Ready {}/{}", d.ready, d.replicas),
                    Health::Progress,
                ));
            }
        }
        for kind in WORKLOAD_KINDS {
            for w in &self.workload(kind).items {
                // A running job is fine; a replica-style workload short of ready isn't.
                let flagged = w.health == Health::Bad
                    || (w.health == Health::Progress && !matches!(kind, Kind::Job | Kind::CronJob));
                if flagged {
                    problems.push((kind, &w.namespace, &w.name, w.summary.clone(), w.health));
                }
            }
        }
        problems.sort_by_key(|p| (p.4 != Health::Bad, kind_order(p.0)));
        let sort = self.sorts.get(&Tab::Overview);
        if let Some(s) = sort {
            let key = |p: &(Kind, &str, &str, String, Health)| match s.col.as_str() {
                "Kind" => SortKey::Num(kind_order(p.0) as i64),
                "Status" => SortKey::text(&p.3),
                "Namespace" => SortKey::text(p.1),
                _ => SortKey::text(p.2),
            };
            problems.sort_by(|a, b| {
                let o = cmp_keys(&key(a), &key(b));
                if s.asc { o } else { o.reverse() }
            });
        }

        let mut attention = row![theme::semibold("Needs attention").size(15.0)]
            .spacing(8)
            .align_y(iced::Center);
        if !problems.is_empty() {
            attention = attention.push(
                text(problems.len().to_string())
                    .size(15.0)
                    .color(theme::TEXT_FAINT),
            );
        }
        let all_loaded = Tab::Overview
            .kinds()
            .iter()
            .all(|k| self.health_counts(*k).is_some());
        let list: El<'a> = if problems.is_empty() {
            theme::muted(if all_loaded {
                "Everything is healthy."
            } else {
                "Loading…"
            })
            .into()
        } else {
            let show_ns = self.selected_ns.len() != 1;
            let mut cols = vec![
                Col::fill("Name", 180.0),
                Col::fixed("Kind", 150.0),
                Col::fixed("Status", 190.0),
            ];
            if show_ns {
                cols.push(Col::fixed("Namespace", 130.0));
            }
            Table {
                id: "list",
                cols,
                sort,
                tab: Tab::Overview,
                sortable: true,
                widths: ctx.widths,
                len: problems.len(),
                scroll: self.list_scroll,
                hovered: ctx.hovered,
                row: move |i, w: &[f32]| {
                    let (kind, ns, name, status, health): &(Kind, &str, &str, String, Health) =
                        &problems[i];
                    let color = health_color(*health);
                    let mut cells: Vec<El<'a>> = vec![
                        table::name_cell(name, w[0]),
                        table::truncated(kind.label(), w[1], 7.6, theme::muted),
                        row![
                            theme::dot(color),
                            table::truncated(status, w[2] - 14.0, 7.6, move |s| {
                                theme::colored(s, color)
                            })
                        ]
                        .spacing(6)
                        .align_y(iced::Center)
                        .into(),
                    ];
                    if show_ns {
                        cells.push(table::truncated(
                            &*ns,
                            w[cells.len()],
                            7.5,
                            theme::mono_muted,
                        ));
                    }
                    Row {
                        cells,
                        selected: false,
                        on_click: act(Action::OpenSelected(Selected {
                            kind: *kind,
                            namespace: ns.to_string(),
                            name: name.to_string(),
                        })),
                        on_double_click: None,
                    }
                },
            }
            .view()
        };
        column![
            header,
            theme::vspace(12.0),
            cards,
            theme::vspace(22.0),
            attention,
            theme::vspace(8.0),
            list
        ]
        .height(Length::Fill)
        .into()
    }

    fn overview_card<'a>(&'a self, kind: Kind) -> El<'a> {
        let counts = self.health_counts(kind);
        let total = counts.map(|c| c.iter().sum::<usize>());
        let number = total.map(|t| t.to_string()).unwrap_or_else(|| "–".into());

        // Stacked health bar.
        let colors = [GREEN, YELLOW, RED, theme::IDLE];
        let mut bar = row![].height(5).width(Length::Fill);
        match (counts, total.filter(|t| *t > 0)) {
            (Some(c), Some(_)) => {
                for (n, color) in c.iter().zip(colors) {
                    if *n == 0 {
                        continue;
                    }
                    bar = bar.push(
                        container(Space::new().height(5))
                            .width(Length::FillPortion(*n as u16))
                            .style(theme::fill(color)),
                    );
                }
            }
            _ => {
                bar = bar.push(
                    container(Space::new().height(5))
                        .width(Length::Fill)
                        .style(theme::fill(theme::FIELD_ACTIVE)),
                );
            }
        }
        let bar = container(bar)
            .width(Length::Fill)
            .clip(true)
            .style(|_| container::Style {
                border: iced::Border {
                    radius: 2.0.into(),
                    ..Default::default()
                },
                ..Default::default()
            });

        // Legend: only the states that are present.
        let legend = match counts {
            None => "Loading…".to_string(),
            Some(c) if c.iter().sum::<usize>() == 0 => "None".to_string(),
            Some(c) => health_words(kind)
                .iter()
                .zip(c)
                .filter(|(_, n)| *n > 0)
                .map(|(w, n)| format!("{n} {w}"))
                .collect::<Vec<_>>()
                .join(" · "),
        };
        let color = match counts {
            Some(c) if c[2] > 0 => RED,
            Some(c) if c[1] > 0 => YELLOW,
            _ => theme::TEXT_FAINT,
        };
        let content = column![
            row![
                text(kind.plural()).size(13.0).color(theme::TEXT_MUTED),
                theme::fill_space(),
                text(kind_glyph(kind))
                    .font(theme::ICONS)
                    .size(15.0)
                    .color(theme::TEXT_FAINT),
            ],
            text(number)
                .size(24.0)
                .font(theme::SEMIBOLD)
                .color(theme::TEXT),
            theme::vspace(6.0),
            bar,
            theme::vspace(6.0),
            text(legend)
                .size(12.0)
                .color(color)
                .wrapping(text::Wrapping::None),
        ]
        .spacing(2);
        button(container(content).height(68).clip(true))
            .width(Length::Fill)
            .padding([12, 14])
            .style(|_, status| {
                let fill = match status {
                    button::Status::Hovered | button::Status::Pressed => theme::FIELD,
                    _ => theme::RAISED,
                };
                button::Style {
                    background: Some(fill.into()),
                    text_color: theme::TEXT,
                    border: iced::Border {
                        color: theme::BORDER,
                        width: 1.0,
                        radius: 8.0.into(),
                    },
                    ..Default::default()
                }
            })
            .on_press(act(Action::Open(Tab::for_kind(kind))))
            .into()
    }

    // -----------------------------------------------------------------------
    // Detail drawer
    // -----------------------------------------------------------------------

    fn drawer_tabs(&self, sel: &Selected) -> Vec<(DrawerTab, String)> {
        let details = self.loaded_details();
        let n = |label: &str, count: Option<usize>| match count {
            Some(c) => format!("{label} {c}"),
            None => label.to_string(),
        };
        let events = n(
            "Events",
            details.and_then(|d| d.events.as_ref().ok()).map(Vec::len),
        );
        let mut tabs = vec![(DrawerTab::Overview, "Overview".to_string())];
        match (sel.kind, details.map(|d| &d.body)) {
            (Kind::Pod, body) => {
                let pod = match body {
                    Some(Body::Pod(p)) => Some(p),
                    _ => None,
                };
                tabs.push((
                    DrawerTab::Containers,
                    n("Containers", pod.map(|p| p.containers.len())),
                ));
                tabs.push((
                    DrawerTab::Volumes,
                    n("Volumes", pod.map(|p| p.volumes.len())),
                ));
            }
            (Kind::Service, body) => {
                let count = match body {
                    Some(Body::Service(s)) => Some(s.endpoints.len()),
                    _ => None,
                };
                tabs.push((DrawerTab::Endpoints, n("Endpoints", count)));
            }
            _ => {}
        }
        tabs.push((DrawerTab::Events, events));
        tabs
    }

    /// The drawer beside the list, or `None` when nothing is selected.
    pub(in crate::app) fn view_drawer<'a>(&'a self, ctx: &ViewCtx<'a>) -> Option<El<'a>> {
        let sel = self.selected.as_ref()?;
        let loading = self.details.as_ref().is_some_and(|d| d.loading);
        let header = row![
            text(sel.name.as_str())
                .size(16.5)
                .font(theme::SEMIBOLD)
                .color(theme::TEXT)
                .width(Length::Fill),
            if loading {
                Element::from(theme::icon(icon::SPINNER).color(theme::TEXT_FAINT))
            } else {
                Space::new().into()
            },
            theme::icon_button(
                icon::COPY,
                "Copy name",
                Some(Message::Copy(sel.name.clone()))
            ),
            theme::icon_button(icon::X, "Close (Esc)", Some(Message::CloseDrawer)),
        ]
        .spacing(4)
        .align_y(iced::Alignment::Start);
        let subtitle = row![
            theme::icon(kind_glyph(sel.kind)),
            theme::muted(format!("{} · {}", sel.kind.label(), sel.namespace))
        ]
        .spacing(8)
        .align_y(iced::Center);
        let tabs = self.drawer_tabs(sel);
        let current = tabs
            .iter()
            .position(|(t, _)| *t == self.drawer_tab)
            .unwrap_or(0);
        let tab_ids: Vec<DrawerTab> = tabs.iter().map(|(t, _)| *t).collect();
        let strip = theme::underline_tabs(
            tabs.into_iter().map(|(_, l)| l).collect(),
            current,
            move |i| Message::DrawerTab(tab_ids[i]),
        );
        let body: El<'a> = match (sel.kind, self.drawer_tab) {
            (_, DrawerTab::Events) => self.drawer_events(),
            (Kind::Pod, DrawerTab::Containers) => self.drawer_pod_containers(sel, ctx),
            (Kind::Pod, DrawerTab::Volumes) => self.drawer_pod_volumes(),
            (Kind::Service, DrawerTab::Endpoints) => self.drawer_service_endpoints(),
            (Kind::Pod, _) => self.drawer_pod(sel, ctx),
            (Kind::Service, _) => self.drawer_service(sel, ctx),
            (Kind::Deployment, _) => self.drawer_deployment(sel, ctx),
            _ => self.drawer_workload(sel, ctx),
        };
        let content = column![
            header,
            theme::vspace(2.0),
            subtitle,
            theme::vspace(14.0),
            strip,
            scrollable(
                container(column![body, theme::vspace(12.0)]).padding(iced::Padding {
                    right: 10.0,
                    ..Default::default()
                })
            )
            .direction(theme::thin())
            .style(theme::scrollbar)
            .height(Length::Fill),
        ];
        // The left edge drags to resize.
        let handle = mouse_area(
            container(Space::new().width(1).height(Length::Fill))
                .padding([0, 2])
                .style(theme::fill(theme::BORDER)),
        )
        .on_press(Message::DragStart(Drag::Drawer))
        .interaction(iced::mouse::Interaction::ResizingHorizontally);
        Some(
            row![
                handle,
                container(content)
                    .padding([14, 18])
                    .width(Length::Fill)
                    .height(Length::Fill)
                    .style(theme::raised)
            ]
            .width(ctx.drawer_width)
            .height(Length::Fill)
            .into(),
        )
    }

    /// Like `loaded_details`, but shows a message while they aren't available.
    fn details_or_status<'a>(&'a self) -> Result<&'a Details, El<'a>> {
        match self.details.as_ref().and_then(|d| d.data.as_ref()) {
            Some(Ok(d)) => Ok(d),
            Some(Err(e)) => Err(theme::colored(e.clone(), RED).into()),
            None => Err(theme::faint("Loading details…").into()),
        }
    }

    /// The "⋯" button that opens the drawer's menu.
    fn drawer_menu_button<'a>(&self, ctx: &ViewCtx<'a>) -> El<'a> {
        let open = ctx.popup == Some(&PopupKind::Drawer);
        theme::tip(
            container(
                button(theme::icon(icon::DOTS_THREE).color(theme::TEXT).size(15.0))
                    .padding([5, 9])
                    .style(move |t, s| {
                        if open {
                            theme::toggled(true)(t, s)
                        } else {
                            theme::secondary(t, s)
                        }
                    })
                    .on_press(Message::OpenPopup(
                        PopupKind::Drawer,
                        iced::widget::Id::new(DRAWER_MENU),
                    )),
            )
            .id(DRAWER_MENU),
            "More actions",
        )
    }

    /// The drawer's "⋯" menu: (icon, label, colour, message), `None` for a separator.
    pub(in crate::app) fn drawer_menu(
        &self,
    ) -> Vec<Option<(&'static str, String, Color, Message)>> {
        let Some(sel) = &self.selected else {
            return Vec::new();
        };
        let (kind, ns, name) = (sel.kind, sel.namespace.clone(), sel.name.clone());
        let mut items = Vec::new();
        match kind {
            Kind::Deployment => {
                items.push(Some((
                    icon::ARROW_COUNTER_CLOCKWISE,
                    "Rollout restart".to_string(),
                    theme::TEXT,
                    act(Action::Restart(kind, ns.clone(), name.clone())),
                )));
                // Paused state comes with the details; offer the toggle once it's known.
                if let Some(Body::Deployment(det)) = self.loaded_details().map(|d| &d.body) {
                    let (glyph, label) = if det.paused {
                        (icon::PLAY, "Resume rollout")
                    } else {
                        (icon::PAUSE, "Pause rollout")
                    };
                    items.push(Some((
                        glyph,
                        label.to_string(),
                        theme::TEXT,
                        act(Action::Pause(ns.clone(), name.clone(), !det.paused)),
                    )));
                }
                items.push(None);
            }
            k if k.restartable() => {
                items.push(Some((
                    icon::ARROW_COUNTER_CLOCKWISE,
                    "Rollout restart".to_string(),
                    theme::TEXT,
                    act(Action::Restart(kind, ns.clone(), name.clone())),
                )));
                items.push(None);
            }
            _ => {}
        }
        // Edit YAML, View YAML and Delete: the end of every drawer's menu.
        items.push(Some((
            icon::PENCIL_SIMPLE,
            "Edit YAML".to_string(),
            theme::TEXT,
            act(Action::EditYaml(kind, ns.clone(), name.clone())),
        )));
        items.push(Some((
            icon::FILE_CODE,
            "View YAML".to_string(),
            theme::TEXT,
            act(Action::Yaml(kind, ns.clone(), name.clone())),
        )));
        items.push(None);
        items.push(Some((
            icon::TRASH,
            format!("Delete {}", kind.label().to_lowercase()),
            RED,
            act(Action::AskDelete(kind, ns, name)),
        )));
        items
    }

    /// The selected pod's regular containers, for the Shell menu.
    pub(in crate::app) fn shell_menu(&self) -> Vec<(String, Message)> {
        let Some(sel) = &self.selected else {
            return Vec::new();
        };
        let Some(p) = self
            .pods
            .items
            .iter()
            .find(|p| p.namespace == sel.namespace && p.name == sel.name)
        else {
            return Vec::new();
        };
        p.containers
            .iter()
            .filter(|c| !c.init)
            .map(|c| {
                (
                    c.name.clone(),
                    act(Action::Shell(p.clone(), Some(c.name.clone()))),
                )
            })
            .collect()
    }

    fn drawer_pod<'a>(&'a self, sel: &Selected, ctx: &ViewCtx<'a>) -> El<'a> {
        let Some(p) = self
            .pods
            .items
            .iter()
            .find(|p| p.namespace == sel.namespace && p.name == sel.name)
        else {
            return theme::faint(if self.pods.loaded {
                "This pod no longer exists."
            } else {
                "Loading…"
            })
            .into();
        };
        let details = match self.loaded_details().map(|d| &d.body) {
            Some(Body::Pod(d)) => Some(d),
            _ => None,
        };

        let color = status_color(&p.status);
        let summary = row![
            theme::dot(color),
            theme::colored(p.status.as_str(), color),
            theme::vline(14.0),
            theme::txt(format!("Ready {}", p.ready)),
            theme::vline(14.0),
            theme::txt(format!("Restarts {}", p.restarts)),
            theme::vline(14.0),
            theme::txt(format!("Age {}", p.age())),
        ]
        .spacing(8)
        .align_y(iced::Center);

        // A pod that restarted offers the previous run's logs; when it's failing that's
        // where the reason is, so it takes the one primary button.
        let restarted = p.restarts > 0;
        let failing = pod_health(&p.status) == Health::Bad;
        let previous_hint = match p.containers.iter().find(|c| !c.last_exit.is_empty()) {
            Some(c) => format!(
                "Logs from the run before the last restart\nLast run: {}",
                c.last_exit
            ),
            None => "Logs from the run before the last restart".to_string(),
        };
        let logs = act(Action::OpenLogs(p.clone(), None, false));
        let previous = act(Action::OpenLogs(p.clone(), None, true));
        let mut actions = row![].spacing(6).align_y(iced::Center);
        if restarted && failing {
            actions = actions.push(theme::tip(
                theme::primary_button(
                    Some(icon::CLOCK_COUNTER_CLOCKWISE),
                    "Previous logs",
                    Some(previous),
                ),
                previous_hint,
            ));
            actions = actions.push(theme::secondary_button(
                Some(icon::SCROLL),
                "Logs",
                Some(logs),
            ));
        } else {
            actions = actions.push(theme::primary_button(
                Some(icon::SCROLL),
                "Logs",
                Some(logs),
            ));
            if restarted {
                actions = actions.push(theme::tip(
                    theme::secondary_button(
                        Some(icon::CLOCK_COUNTER_CLOCKWISE),
                        "Previous logs",
                        Some(previous),
                    ),
                    previous_hint,
                ));
            }
        }
        if !p.owner.is_empty() {
            actions = actions.push(theme::tip(
                theme::secondary_button(
                    Some(icon::STACK),
                    "All replicas",
                    Some(act(Action::OpenGroupLogs {
                        namespace: p.namespace.clone(),
                        target: LogTarget::Owner(p.owner.clone()),
                    })),
                ),
                format!("Logs from every pod of {}", p.owner),
            ));
        }
        let running = p.status == "Running";
        let regular = p.containers.iter().filter(|c| !c.init).count();
        if regular > 1 && running {
            actions = actions.push(
                container(theme::secondary_button(
                    Some(icon::TERMINAL_WINDOW),
                    "Shell",
                    Some(Message::OpenPopup(
                        PopupKind::ShellContainers,
                        iced::widget::Id::new(SHELL_MENU),
                    )),
                ))
                .id(SHELL_MENU),
            );
        } else {
            actions = actions.push(theme::tip(
                theme::secondary_button(
                    Some(icon::TERMINAL_WINDOW),
                    "Shell",
                    running.then(|| act(Action::Shell(p.clone(), None))),
                ),
                if running {
                    "Open a shell in the container"
                } else {
                    "The pod isn't running"
                },
            ));
        }
        actions = actions.push(theme::secondary_button(
            Some(icon::ARROWS_LEFT_RIGHT),
            "Port forward",
            Some(act(Action::ForwardPod(p.clone(), None))),
        ));
        actions = actions.push(self.drawer_menu_button(ctx));

        let mut col = column![
            summary,
            theme::vspace(8.0),
            actions.wrap().vertical_spacing(6)
        ];

        col = col.push(section("Details"));
        let mut rows = vec![
            ("Node", p.node.clone()),
            ("Pod IP", p.ip.clone()),
            ("Controlled by", p.owner.clone()),
        ];
        if let Some(d) = details {
            rows.extend([
                ("Host IP", d.host_ip.clone()),
                ("QoS class", d.qos.clone()),
                ("Service account", d.service_account.clone()),
                ("Restart policy", d.restart_policy.clone()),
                ("Priority class", d.priority_class.clone()),
                ("Started", d.started.clone()),
            ]);
        }
        col = col.push(details_grid(rows));

        col = col.push(section("Containers"));
        for c in &p.containers {
            let color = if c.ready || c.state == "Completed" {
                GREEN
            } else {
                status_color(&c.state)
            };
            let mut line = row![theme::dot(color), theme::semibold(c.name.as_str())]
                .spacing(6)
                .align_y(iced::Center);
            if c.init {
                line = line.push(theme::faint("init"));
            }
            line = line.push(theme::txt(c.state.as_str()));
            line = line.push(theme::faint(format!("· {} restarts", c.restarts)));
            col = col.push(container(line).padding([3, 0]));
        }

        if let Some(d) = details {
            col = col.push(conditions(&d.conditions));
        }

        if !p.ports.is_empty() {
            col = col.push(section("Ports"));
            for port in &p.ports {
                col = col.push(
                    row![
                        theme::mono(port.to_string()),
                        theme::small_button(
                            Some(icon::ARROWS_LEFT_RIGHT),
                            "Forward",
                            Some(act(Action::ForwardPod(p.clone(), Some(*port))))
                        )
                    ]
                    .spacing(8)
                    .align_y(iced::Center),
                );
            }
        }

        if !p.labels.is_empty() {
            col = col.push(section("Labels"));
            col = col.push(tags(p.labels.iter().map(|(k, v)| format!("{k}={v}"))));
        }
        if let Some(d) = details {
            if !d.node_selector.is_empty() {
                col = col.push(section("Node selector"));
                col = col.push(tags(d.node_selector.iter().cloned()));
            }
            col = col.push(annotations(&d.annotations, ctx));
        }
        col.spacing(2).into()
    }

    fn drawer_pod_containers<'a>(&'a self, sel: &Selected, ctx: &ViewCtx<'a>) -> El<'a> {
        let details = match self.details_or_status() {
            Ok(d) => d,
            Err(e) => return e,
        };
        let Body::Pod(pod) = &details.body else {
            return Space::new().into();
        };
        let row_info = self
            .pods
            .items
            .iter()
            .find(|p| p.namespace == sel.namespace && p.name == sel.name);

        let mut col = column![].spacing(6);
        for c in &pod.containers {
            let info = row_info.and_then(|p| p.containers.iter().find(|i| i.name == c.name));
            let color = match info {
                Some(i) if i.ready || i.state == "Completed" => GREEN,
                Some(i) => status_color(&i.state),
                None => theme::IDLE,
            };
            let mut head = row![theme::dot(color), theme::semibold(c.name.as_str())]
                .spacing(6)
                .align_y(iced::Center);
            if c.init {
                head = head.push(theme::faint("init"));
            }
            head = head.push(theme::fill_space());
            if let Some(p) = row_info {
                if info.is_some_and(|i| i.restarts > 0) {
                    head = head.push(theme::tip(
                        theme::small_button(
                            Some(icon::CLOCK_COUNTER_CLOCKWISE),
                            "Previous",
                            Some(act(Action::OpenLogs(p.clone(), Some(c.name.clone()), true))),
                        ),
                        "Logs from the run before the last restart",
                    ));
                }
                head = head.push(theme::small_button(
                    Some(icon::SCROLL),
                    "Logs",
                    Some(act(Action::OpenLogs(
                        p.clone(),
                        Some(c.name.clone()),
                        false,
                    ))),
                ));
                let can_shell = !c.init && p.status == "Running";
                head = head.push(theme::small_button(
                    Some(icon::TERMINAL_WINDOW),
                    "Shell",
                    can_shell.then(|| act(Action::Shell(p.clone(), Some(c.name.clone())))),
                ));
            }
            let mut card = column![head].spacing(4);
            if let Some(i) = info {
                card = card.push(
                    row![
                        theme::txt(i.state.as_str()),
                        theme::faint(format!("· {} restarts", i.restarts))
                    ]
                    .spacing(6),
                );
            }
            if !c.last_termination.is_empty() {
                card = card.push(theme::colored(
                    format!("Last terminated: {}", c.last_termination),
                    YELLOW,
                ));
            }
            card = card.push(theme::mono_muted(c.image.as_str()));

            let mut rows = vec![("Pull policy", c.pull_policy.clone())];
            if !c.working_dir.is_empty() {
                rows.push(("Working dir", c.working_dir.clone()));
            }
            card = card.push(details_grid(rows));

            if !c.command.is_empty() || !c.args.is_empty() {
                card = card.push(sub_section("Command"));
                let line = c
                    .command
                    .iter()
                    .chain(c.args.iter())
                    .map(|s| shell_quote(s))
                    .collect::<Vec<_>>()
                    .join(" ");
                card = card.push(code_block(line));
            }
            if !c.ports.is_empty() {
                card = card.push(sub_section("Ports"));
                card = card.push(tags(c.ports.iter().cloned()));
            }
            if !c.requests.is_empty() || !c.limits.is_empty() {
                card = card.push(sub_section("Resources"));
                card = card.push(resources_grid(&c.requests, &c.limits));
            }

            let env_count = c.env.len() + c.env_from_secrets.len();
            card = card.push(collapsible(
                ctx,
                format!("env/{}", c.name),
                true,
                format!("Environment ({env_count})"),
                || self.env_list(&sel.namespace, c, ctx),
            ));
            if !c.mounts.is_empty() {
                card = card.push(collapsible(
                    ctx,
                    format!("mounts/{}", c.name),
                    false,
                    format!("Mounts ({})", c.mounts.len()),
                    || {
                        column(c.mounts.iter().map(|(path, source)| {
                            row![
                                theme::mono(path.as_str()),
                                theme::faint(format!("← {source}"))
                            ]
                            .spacing(6)
                            .wrap()
                            .into()
                        }))
                        .spacing(3)
                        .into()
                    },
                ));
            }
            if !c.probes.is_empty() {
                card = card.push(collapsible(
                    ctx,
                    format!("probes/{}", c.name),
                    false,
                    format!("Probes ({})", c.probes.len()),
                    || {
                        column(c.probes.iter().map(|(kind, desc)| {
                            column![
                                theme::semibold(kind.as_str()),
                                theme::mono_muted(desc.as_str()).size(11.5)
                            ]
                            .spacing(2)
                            .into()
                        }))
                        .spacing(6)
                        .into()
                    },
                ));
            }
            col = col.push(container(card).padding([10, 12]).width(Length::Fill).style(
                theme::boxed(
                    theme::alpha(theme::FIELD, 0.55),
                    theme::BORDER,
                    theme::RADIUS,
                ),
            ));
        }
        col.into()
    }

    fn env_list<'a>(&'a self, ns: &str, c: &'a ContainerDetails, ctx: &ViewCtx<'a>) -> El<'a> {
        if c.env.is_empty() && c.env_from_secrets.is_empty() {
            return theme::faint("No environment variables").into();
        }
        let secret = |name: &str| self.secrets.get(&(ns.to_string(), name.to_string()));
        // One search shared by every container's list.
        let mut col = column![theme::search_field(
            ctx.env_filter,
            "Search env",
            220.0,
            Message::EnvFilter
        )]
        .spacing(4);
        let needle = ctx.env_filter.trim().to_lowercase();
        let mut shown = 0usize;
        // Everything with a known value, for "Copy as .env".
        let mut dotenv = Vec::new();

        for name in &c.env_from_secrets {
            col = col.push(
                row![
                    theme::faint(format!("All keys from Secret {name}")),
                    self.secret_toggle(ns, name)
                ]
                .spacing(6)
                .align_y(iced::Center),
            );
            if let Some(Ok(values)) = secret(name) {
                let source = format!("Secret {name}");
                for (k, v) in values {
                    if !env_matches(&needle, k, Some(v), Some(&source)) {
                        continue;
                    }
                    shown += 1;
                    col = col.push(env_item(k, Some(v.clone()), Some(source.clone()), None));
                    dotenv.push(format!("{k}={v}"));
                }
            }
        }

        for e in &c.env {
            let (value, source, secret_name) = match &e.source {
                EnvSource::Literal => (e.value.clone(), None, None),
                EnvSource::ConfigMap { name, key } => (
                    Some(
                        e.value
                            .clone()
                            .unwrap_or_else(|| "(ConfigMap not readable)".into()),
                    ),
                    Some(format!("ConfigMap {name} · {key}")),
                    None,
                ),
                EnvSource::Secret { name, key } => {
                    let value = match secret(name) {
                        Some(Ok(values)) => Some(
                            values
                                .get(key)
                                .cloned()
                                .unwrap_or_else(|| "(missing key)".into()),
                        ),
                        _ => None,
                    };
                    (
                        value,
                        Some(format!("Secret {name} · {key}")),
                        Some(name.as_str()),
                    )
                }
                EnvSource::Field(path) => (None, Some(format!("field {path}")), None),
                EnvSource::Resource(r) => (None, Some(format!("resource {r}")), None),
            };
            if !env_matches(&needle, &e.name, value.as_deref(), source.as_deref()) {
                continue;
            }
            shown += 1;
            if let Some(v) = &value {
                dotenv.push(format!("{}={v}", e.name));
            }
            let toggle = secret_name.map(|name| self.secret_toggle(ns, name));
            col = col.push(env_item(
                &e.name,
                value,
                source,
                toggle.map(|t| (t, secret_name.is_some())),
            ));
        }

        if !needle.is_empty() && shown == 0 {
            let hidden = c
                .env_from_secrets
                .iter()
                .any(|n| !matches!(secret(n), Some(Ok(_))));
            col = col.push(theme::faint(if hidden {
                "No variables match (reveal secrets to search their keys)"
            } else {
                "No variables match"
            }));
        }
        let copy_label = if needle.is_empty() {
            "Copy as .env"
        } else {
            "Copy matches as .env"
        };
        col = col.push(theme::vspace(2.0));
        col = col.push(theme::small_button(
            Some(icon::COPY),
            copy_label,
            Some(Message::Copy(dotenv.join("\n"))),
        ));
        col.into()
    }

    fn secret_toggle<'a>(&self, ns: &str, name: &str) -> El<'a> {
        let key = (ns.to_string(), name.to_string());
        if self.secrets_loading.contains(&key) {
            return theme::faint("Reading…").into();
        }
        match self.secrets.get(&key) {
            Some(Ok(_)) => theme::small_button(
                Some(icon::EYE_SLASH),
                "Hide",
                Some(act(Action::HideSecret(key.0, key.1))),
            ),
            Some(Err(e)) => theme::tip(
                row![
                    theme::icon(icon::WARNING_CIRCLE).color(RED),
                    text("not readable").size(theme::SMALL).color(RED)
                ]
                .spacing(4)
                .align_y(iced::Center),
                e.clone(),
            ),
            None => theme::tip(
                theme::small_button(
                    Some(icon::EYE),
                    "Reveal",
                    Some(act(Action::RevealSecret(key.0, key.1))),
                ),
                "Read this secret",
            ),
        }
    }

    fn drawer_pod_volumes<'a>(&'a self) -> El<'a> {
        let details = match self.details_or_status() {
            Ok(d) => d,
            Err(e) => return e,
        };
        let Body::Pod(pod) = &details.body else {
            return Space::new().into();
        };
        if pod.volumes.is_empty() {
            return theme::faint("No volumes").into();
        }
        let mut col = column![].spacing(8);
        for (name, desc) in &pod.volumes {
            let mounted_by: Vec<String> = pod
                .containers
                .iter()
                .flat_map(|c| {
                    c.mounts
                        .iter()
                        .filter(|(_, src)| {
                            src == name
                                || src.starts_with(&format!("{name}/"))
                                || src.starts_with(&format!("{name} "))
                        })
                        .map(move |(path, _)| format!("{}:{path}", c.name))
                })
                .collect();
            let mut entry = column![
                row![theme::semibold(name.as_str()), theme::faint(desc.as_str())]
                    .spacing(8)
                    .wrap()
            ]
            .spacing(2);
            for m in mounted_by {
                entry = entry.push(theme::mono_muted(m).size(11.5));
            }
            col = col.push(entry);
        }
        col.into()
    }

    fn drawer_events<'a>(&'a self) -> El<'a> {
        let details = match self.details_or_status() {
            Ok(d) => d,
            Err(e) => return e,
        };
        let events = match &details.events {
            Ok(e) => e,
            Err(e) => return theme::faint(format!("Events unavailable: {e}")).into(),
        };
        if events.is_empty() {
            return theme::faint("No recent events. Kubernetes keeps events for about an hour.")
                .into();
        }
        let mut col = column![].spacing(8);
        for e in events {
            let (glyph, color) = if e.warning {
                (icon::WARNING, YELLOW)
            } else {
                (icon::INFO, theme::TEXT_MUTED)
            };
            let count = if e.count > 1 {
                format!(" ×{}", e.count)
            } else {
                String::new()
            };
            col = col.push(
                column![
                    row![
                        theme::icon(glyph).color(color),
                        theme::semibold(e.reason.as_str()),
                        theme::fill_space(),
                        theme::faint(format!("{} ago{count}", e.age)),
                    ]
                    .spacing(6)
                    .align_y(iced::Center),
                    theme::txt(e.message.as_str()),
                    theme::vspace(2.0),
                    theme::hline(),
                ]
                .spacing(4),
            );
        }
        col.into()
    }

    fn drawer_service<'a>(&'a self, sel: &Selected, ctx: &ViewCtx<'a>) -> El<'a> {
        let Some(s) = self
            .services
            .items
            .iter()
            .find(|s| s.namespace == sel.namespace && s.name == sel.name)
        else {
            return theme::faint("This service no longer exists.").into();
        };
        let details = match self.loaded_details().map(|d| &d.body) {
            Some(Body::Service(d)) => Some(d),
            _ => None,
        };
        let can_forward = !s.selector.is_empty();
        let actions = row![
            theme::tip(
                theme::secondary_button(
                    Some(icon::ARROWS_LEFT_RIGHT),
                    "Port forward",
                    (can_forward && !s.ports.is_empty())
                        .then(|| act(Action::ForwardService(s.clone(), None))),
                ),
                if can_forward && !s.ports.is_empty() {
                    ""
                } else {
                    "Service has no selector or ports"
                },
            ),
            theme::tip(
                theme::secondary_button(
                    Some(icon::STACK),
                    "Logs (all pods)",
                    can_forward.then(|| act(Action::OpenGroupLogs {
                        namespace: s.namespace.clone(),
                        target: LogTarget::Selector {
                            label: format!("svc/{}", s.name),
                            selector: s.selector.clone(),
                        },
                    })),
                ),
                if can_forward {
                    ""
                } else {
                    "Service has no selector"
                },
            ),
            self.drawer_menu_button(ctx),
        ]
        .spacing(6)
        .align_y(iced::Center);

        let mut col = column![actions.wrap().vertical_spacing(6)];
        col = col.push(section("Details"));
        let mut rows = vec![
            ("Type", s.kind.clone()),
            ("Cluster IP", s.cluster_ip.clone()),
            ("External", s.external.clone()),
            ("Age", s.age()),
        ];
        if let Some(d) = details {
            rows.extend([
                ("Session affinity", d.session_affinity.clone()),
                ("External traffic", d.external_traffic_policy.clone()),
                ("Internal traffic", d.internal_traffic_policy.clone()),
                ("IP families", d.ip_families.clone()),
            ]);
        }
        col = col.push(details_grid(rows));

        if let Some(d) = details {
            let ready = d.endpoints.iter().filter(|e| e.ready).count();
            col = col.push(theme::vspace(4.0));
            col = col.push(
                row![
                    theme::dot(if ready > 0 { GREEN } else { RED }),
                    theme::txt(format!("{ready} of {} endpoints ready", d.endpoints.len()))
                ]
                .spacing(6)
                .align_y(iced::Center),
            );
        }

        col = col.push(section("Ports"));
        if s.ports.is_empty() {
            col = col.push(theme::faint("No ports"));
        }
        for port in &s.ports {
            let name = if port.name.is_empty() {
                String::new()
            } else {
                format!("{} ", port.name)
            };
            let mut line = row![theme::mono(format!(
                "{name}{} → {}/{}",
                port.port, port.target, port.protocol
            ))]
            .spacing(8)
            .align_y(iced::Center);
            if let Some(np) = port.node_port {
                line = line.push(theme::faint(format!("node {np}")));
            }
            line = line.push(theme::small_button(
                Some(icon::ARROWS_LEFT_RIGHT),
                "Forward",
                can_forward.then(|| act(Action::ForwardService(s.clone(), Some(port.port)))),
            ));
            col = col.push(line);
        }

        col = col.push(section("Selector"));
        if s.selector.is_empty() {
            col = col.push(theme::faint("None"));
        } else {
            col = col.push(tags(s.selector.split(',').map(str::to_string)));
        }
        if let Some(d) = details {
            if !d.labels.is_empty() {
                col = col.push(section("Labels"));
                col = col.push(tags(d.labels.iter().map(|(k, v)| format!("{k}={v}"))));
            }
            col = col.push(annotations(&d.annotations, ctx));
        }
        col.spacing(2).into()
    }

    fn drawer_service_endpoints<'a>(&'a self) -> El<'a> {
        let details = match self.details_or_status() {
            Ok(d) => d,
            Err(e) => return e,
        };
        let Body::Service(svc) = &details.body else {
            return Space::new().into();
        };
        let mut col = column![].spacing(4);
        if !svc.endpoint_ports.is_empty() {
            col = col.push(sub_section("Ports"));
            col = col.push(tags(svc.endpoint_ports.iter().cloned()));
            col = col.push(theme::vspace(6.0));
        }
        if svc.endpoints.is_empty() {
            return col
                .push(theme::faint(
                    "No endpoints. Check that the selector matches ready pods.",
                ))
                .into();
        }
        let ns = self
            .selected
            .as_ref()
            .map(|s| s.namespace.clone())
            .unwrap_or_default();
        for e in &svc.endpoints {
            let mut line = row![
                theme::dot(if e.ready { GREEN } else { RED }),
                theme::mono(e.ip.as_str())
            ]
            .spacing(8)
            .align_y(iced::Center);
            line = line.push(match e.target.strip_prefix("pod/") {
                Some(pod) => theme::tip(
                    button(text(e.target.as_str()).size(theme::BODY))
                        .padding(0)
                        .style(theme::link(theme::ACCENT_HOVER))
                        .on_press(act(Action::GoToPod(ns.clone(), pod.to_string()))),
                    "Open pod",
                ),
                None => theme::txt(e.target.as_str()).into(),
            });
            if !e.ready {
                line = line.push(theme::colored("not ready", RED));
            }
            col = col.push(line);
            if !e.node.is_empty() {
                col = col.push(container(theme::small(format!("on {}", e.node))).padding(
                    iced::Padding {
                        left: 16.0,
                        ..Default::default()
                    },
                ));
            }
        }
        col.into()
    }

    fn replicas_control<'a>(kind: Kind, ns: &str, name: &str, replicas: i32) -> El<'a> {
        let scale = |n: i32| act(Action::Scale(kind, ns.to_string(), name.to_string(), n));
        row![
            theme::txt("Replicas"),
            theme::tip(
                theme::secondary_button(
                    Some(icon::MINUS),
                    "",
                    (replicas > 0).then(|| scale(replicas - 1))
                ),
                "Scale down"
            ),
            theme::semibold(replicas.to_string()),
            theme::tip(
                theme::secondary_button(Some(icon::PLUS), "", Some(scale(replicas + 1))),
                "Scale up"
            ),
        ]
        .spacing(6)
        .align_y(iced::Center)
        .into()
    }

    fn drawer_deployment<'a>(&'a self, sel: &Selected, ctx: &ViewCtx<'a>) -> El<'a> {
        let Some(d) = self
            .deployments
            .items
            .iter()
            .find(|d| d.namespace == sel.namespace && d.name == sel.name)
        else {
            return theme::faint("This deployment no longer exists.").into();
        };
        let details = match self.loaded_details().map(|d| &d.body) {
            Some(Body::Deployment(d)) => Some(d),
            _ => None,
        };
        let color = if d.ready >= d.replicas { GREEN } else { YELLOW };
        let summary = row![
            theme::dot(color),
            theme::colored(format!("Ready {}/{}", d.ready, d.replicas), color),
            theme::vline(14.0),
            theme::txt(format!("Up-to-date {}", d.updated)),
            theme::vline(14.0),
            theme::txt(format!("Available {}", d.available)),
            theme::vline(14.0),
            theme::txt(format!("Age {}", d.age())),
        ]
        .spacing(8)
        .align_y(iced::Center);
        let mut col = column![summary, theme::vspace(8.0)].spacing(2);
        if !d.selector.is_empty() {
            col = col.push(theme::secondary_button(
                Some(icon::STACK),
                "Logs (all pods)",
                Some(act(Action::OpenGroupLogs {
                    namespace: d.namespace.clone(),
                    target: LogTarget::Selector {
                        label: format!("deploy/{}", d.name),
                        selector: d.selector.clone(),
                    },
                })),
            ));
            col = col.push(theme::vspace(4.0));
        }
        col = col.push(
            row![
                Self::replicas_control(Kind::Deployment, &d.namespace, &d.name, d.replicas),
                theme::vline(18.0),
                self.drawer_menu_button(ctx),
            ]
            .spacing(8)
            .align_y(iced::Center),
        );

        col = col.push(section("Details"));
        let mut rows = vec![("Strategy", d.strategy.clone())];
        if let Some(det) = details {
            rows.extend([
                ("Paused", if det.paused { "Yes" } else { "No" }.to_string()),
                ("Min ready", det.min_ready_seconds.to_string()),
                ("Revision history", det.revision_history.to_string()),
            ]);
        }
        col = col.push(details_grid(rows));
        if let Some(det) = details {
            col = col.push(conditions(&det.conditions));
        }
        col = col.push(section("Images"));
        for image in &d.images {
            col = col.push(theme::mono(image.as_str()));
        }
        if !d.selector.is_empty() {
            col = col.push(section("Selector"));
            col = col.push(tags(d.selector.split(',').map(str::to_string)));
        }
        if let Some(det) = details {
            if !det.labels.is_empty() {
                col = col.push(section("Labels"));
                col = col.push(tags(det.labels.iter().map(|(k, v)| format!("{k}={v}"))));
            }
            col = col.push(annotations(&det.annotations, ctx));
        }
        col.into()
    }

    fn drawer_workload<'a>(&'a self, sel: &Selected, ctx: &ViewCtx<'a>) -> El<'a> {
        let Some(w) = self
            .workload(sel.kind)
            .items
            .iter()
            .find(|w| w.namespace == sel.namespace && w.name == sel.name)
        else {
            return theme::faint(format!("This {} no longer exists.", sel.kind.label())).into();
        };
        let details = match self.loaded_details().map(|d| &d.body) {
            Some(Body::Workload(d)) => Some(d),
            _ => None,
        };
        let color = health_color(w.health);
        let summary = row![
            theme::dot(color),
            theme::colored(w.summary.as_str(), color),
            theme::vline(14.0),
            theme::txt(format!("Age {}", w.age())),
        ]
        .spacing(8)
        .align_y(iced::Center);

        // Separators only between groups that are actually shown.
        let mut actions = row![].spacing(6).align_y(iced::Center);
        let mut first = true;
        if w.kind == Kind::CronJob {
            first = false;
            actions = actions.push(theme::tip(
                theme::primary_button(
                    Some(icon::PLAY),
                    "Run now",
                    Some(act(Action::Trigger(w.namespace.clone(), w.name.clone()))),
                ),
                "Start a job from this cron job's template",
            ));
        }
        if let Some(suspended) = w.suspended.filter(|_| !w.finished) {
            first = false;
            let (glyph, label) = if suspended {
                (icon::PLAY_CIRCLE, "Resume")
            } else {
                (icon::PAUSE, "Suspend")
            };
            let hint = match (w.kind, suspended) {
                (Kind::CronJob, false) => "Stop scheduling new runs",
                (Kind::CronJob, true) => "Start scheduling runs again",
                (_, false) => "Stop the job's pods until resumed",
                (_, true) => "Let the job run again",
            };
            actions = actions.push(theme::tip(
                theme::secondary_button(
                    Some(glyph),
                    label,
                    Some(act(Action::Suspend(
                        w.kind,
                        w.namespace.clone(),
                        w.name.clone(),
                        !suspended,
                    ))),
                ),
                hint,
            ));
        }
        if !w.selector.is_empty() && w.kind != Kind::CronJob {
            if !first {
                actions = actions.push(theme::vline(18.0));
            }
            first = false;
        }
        if !w.selector.is_empty() {
            actions = actions.push(theme::secondary_button(
                Some(icon::STACK),
                "Logs (all pods)",
                Some(act(Action::OpenGroupLogs {
                    namespace: w.namespace.clone(),
                    target: LogTarget::Selector {
                        label: format!("{}/{}", w.kind.label().to_lowercase(), w.name),
                        selector: w.selector.clone(),
                    },
                })),
            ));
        }
        if let Some(replicas) = w.replicas.filter(|_| w.kind.scalable()) {
            if !first {
                actions = actions.push(theme::vline(18.0));
            }
            first = false;
            actions = actions.push(Self::replicas_control(
                w.kind,
                &w.namespace,
                &w.name,
                replicas,
            ));
        }
        if !first {
            actions = actions.push(theme::vline(18.0));
        }
        actions = actions.push(self.drawer_menu_button(ctx));

        let mut col = column![
            summary,
            theme::vspace(8.0),
            actions.wrap().vertical_spacing(6)
        ]
        .spacing(2);
        col = col.push(section("Details"));
        let columns = kube_ops::columns(w.kind);
        let mut rows: Vec<(&str, String)> = columns
            .iter()
            .map(|(t, _)| *t)
            .zip(w.cells.iter().map(|c| c.text()))
            .collect();
        rows.extend(w.info.iter().map(|(k, v)| (*k, v.clone())));
        if !w.owner.is_empty() {
            rows.push(("Controlled by", w.owner.clone()));
        }
        col = col.push(details_grid(rows));
        if let Some(det) = details {
            col = col.push(conditions(&det.conditions));
        }
        if !w.images.is_empty() {
            col = col.push(section("Images"));
            for image in &w.images {
                col = col.push(theme::mono(image.as_str()));
            }
        }
        if !w.selector.is_empty() {
            col = col.push(section("Selector"));
            col = col.push(tags(w.selector.split(',').map(str::to_string)));
        }
        if let Some(det) = details {
            if !det.labels.is_empty() {
                col = col.push(section("Labels"));
                col = col.push(tags(det.labels.iter().map(|(k, v)| format!("{k}={v}"))));
            }
            col = col.push(annotations(&det.annotations, ctx));
        }
        col.into()
    }
}

pub(in crate::app) const DRAWER_MENU: &str = "drawer-menu";
pub(in crate::app) const SHELL_MENU: &str = "shell-menu";

// ---------------------------------------------------------------------------
// Drawer pieces
// ---------------------------------------------------------------------------

fn section<'a>(title: &'a str) -> El<'a> {
    column![
        theme::vspace(16.0),
        theme::label_small(title),
        theme::vspace(4.0)
    ]
    .into()
}

fn sub_section<'a>(title: &'a str) -> El<'a> {
    column![theme::vspace(6.0), theme::label_small(title)].into()
}

fn details_grid<'a>(rows: Vec<(&'a str, String)>) -> El<'a> {
    column(rows.into_iter().map(|(key, value)| {
        let value = if value.is_empty() {
            "-".to_string()
        } else {
            value
        };
        row![
            theme::muted(key).width(120),
            theme::tip(
                theme::mono(value.clone()).wrapping(text::Wrapping::WordOrGlyph),
                if value.chars().count() > 48 {
                    value
                } else {
                    String::new()
                },
            )
        ]
        .spacing(16)
        .into()
    }))
    .spacing(7)
    .into()
}

/// Pill-shaped labels that wrap onto new rows. Click copies one.
fn tags<'a>(items: impl Iterator<Item = String>) -> El<'a> {
    row(items.map(|item| {
        theme::tip(
            button(
                text(item.clone())
                    .font(theme::MONO)
                    .size(11.5)
                    .color(theme::TEXT),
            )
            .padding([2, 6])
            .style(|_, status| {
                let fill = match status {
                    button::Status::Hovered | button::Status::Pressed => theme::FIELD_HOVER,
                    _ => theme::STRIPE,
                };
                button::Style {
                    background: Some(fill.into()),
                    text_color: theme::TEXT,
                    border: iced::Border {
                        color: theme::BORDER,
                        width: 1.0,
                        radius: 4.0.into(),
                    },
                    ..Default::default()
                }
            })
            .on_press(Message::Copy(item)),
            "Click to copy",
        )
    }))
    .spacing(4)
    .wrap()
    .vertical_spacing(4)
    .into()
}

/// A header that folds its content away. `key` remembers the state for this drawer.
fn collapsible<'a>(
    ctx: &ViewCtx<'a>,
    key: String,
    default_open: bool,
    title: String,
    content: impl FnOnce() -> El<'a>,
) -> El<'a> {
    let open = ctx.open(&key, default_open);
    let header = button(
        row![
            text(if open {
                icon::CARET_DOWN
            } else {
                icon::CARET_RIGHT
            })
            .font(theme::ICONS)
            .size(11.0)
            .color(theme::TEXT_FAINT),
            theme::semibold(title),
        ]
        .spacing(6)
        .align_y(iced::Center),
    )
    .padding([4, 0])
    .style(theme::link(theme::TEXT))
    .on_press(Message::ToggleSection(key));
    let mut col = column![header].spacing(4);
    if open {
        col = col.push(container(content()).padding(iced::Padding {
            left: 17.0,
            ..Default::default()
        }));
    }
    col.into()
}

fn annotations<'a>(list: &'a [(String, String)], ctx: &ViewCtx<'a>) -> El<'a> {
    if list.is_empty() {
        return Space::new().into();
    }
    column![
        theme::vspace(10.0),
        collapsible(
            ctx,
            "annotations".into(),
            false,
            format!("Annotations ({})", list.len()),
            || {
                column(list.iter().map(|(k, v)| {
                    column![theme::small(k.as_str()), value_label(v, theme::TEXT)]
                        .spacing(2)
                        .into()
                }))
                .spacing(6)
                .into()
            },
        )
    ]
    .into()
}

fn conditions<'a>(list: &'a [details::Condition]) -> El<'a> {
    if list.is_empty() {
        return Space::new().into();
    }
    let mut col = column![section("Conditions")].spacing(3);
    for c in list {
        let mut line = row![
            theme::dot(if c.ok { GREEN } else { YELLOW }),
            theme::tip(theme::txt(c.type_.as_str()), c.message.clone()),
        ]
        .spacing(6)
        .align_y(iced::Center);
        if !c.reason.is_empty() {
            line = line.push(theme::faint(c.reason.as_str()));
        }
        col = col.push(line);
    }
    col.into()
}

/// A value in monospace, wrapping, with long values cut short (full value on hover).
fn value_label<'a>(value: &str, color: Color) -> El<'a> {
    const MAX: usize = 400;
    if value.is_empty() {
        theme::faint("(empty)").into()
    } else if value.chars().count() > MAX {
        let short: String = value.chars().take(MAX).collect();
        theme::tip(
            theme::mono(format!("{short}…"))
                .color(color)
                .wrapping(text::Wrapping::WordOrGlyph),
            value.to_string(),
        )
    } else {
        theme::mono(value.to_string())
            .color(color)
            .wrapping(text::Wrapping::WordOrGlyph)
            .into()
    }
}

/// One environment variable: name, where it comes from, its value.
fn env_item<'a>(
    name: &str,
    value: Option<String>,
    source: Option<String>,
    secret: Option<(El<'a>, bool)>,
) -> El<'a> {
    let mut head = row![theme::mono(name.to_string()).font(iced::Font {
        weight: iced::font::Weight::Bold,
        ..theme::MONO
    })]
    .spacing(6)
    .align_y(iced::Center);
    if let Some(s) = source {
        head = head.push(theme::small(s));
    }
    let is_secret = secret.as_ref().is_some_and(|(_, s)| *s);
    if let Some((toggle, _)) = secret {
        head = head.push(toggle);
    }
    let value: El<'a> = match value {
        Some(v) => value_label(&v, theme::SYNTAX_STRING),
        None if is_secret => theme::faint("••••••••").into(),
        None => theme::faint("set at runtime").into(),
    };
    column![head.wrap(), value].spacing(2).into()
}

fn resources_grid<'a>(requests: &[(String, String)], limits: &[(String, String)]) -> El<'a> {
    let mut names: Vec<&String> = requests
        .iter()
        .chain(limits.iter())
        .map(|(k, _)| k)
        .collect();
    names.sort();
    names.dedup();
    let find = |list: &[(String, String)], name: &str| {
        list.iter()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.clone())
            .unwrap_or_else(|| "-".into())
    };
    let mut col = column![row![
        Space::new().width(90),
        theme::faint("Request").width(90),
        theme::faint("Limit").width(90)
    ]]
    .spacing(4);
    for name in names {
        col = col.push(row![
            theme::txt(name.clone()).width(90),
            theme::mono(find(requests, name)).width(90),
            theme::mono(find(limits, name)).width(90),
        ]);
    }
    col.into()
}

fn code_block<'a>(line: String) -> El<'a> {
    container(
        theme::mono(line)
            .size(11.5)
            .wrapping(text::Wrapping::WordOrGlyph),
    )
    .padding(6)
    .width(Length::Fill)
    .style(theme::boxed(theme::CHROME, theme::CHROME, 4.0))
    .into()
}

/// Restarts on a fixed severity ramp: none, tinted amber, filled red at five or more.
fn restarts_badge<'a>(restarts: i32) -> El<'a> {
    match restarts {
        0 => theme::mono("0").color(theme::TEXT_FAINT).into(),
        1..=4 => theme::badge(restarts.to_string(), theme::WARN, false),
        _ => theme::badge(restarts.to_string(), theme::BAD, true),
    }
}

/// A list error as a tinted box, one row per message (one per failed namespace).
fn error_callout<'a>(error: &str) -> El<'a> {
    let lines = error.lines().filter(|l| !l.trim().is_empty()).map(|line| {
        row![
            theme::icon(icon::WARNING_CIRCLE).color(theme::BAD),
            theme::txt(line.to_string()).wrapping(text::Wrapping::WordOrGlyph)
        ]
        .spacing(8)
        .into()
    });
    column![
        container(column(lines).spacing(4))
            .padding([10, 12])
            .width(Length::Fill)
            .style(theme::boxed(
                theme::alpha(theme::BAD, 0.10),
                theme::alpha(theme::BAD, 0.45),
                theme::RADIUS
            )),
        theme::vspace(8.0)
    ]
    .into()
}
