//! Rendering for a cluster tab: the resource lists, the Overview and the detail drawer
//! (`view/drawer.rs`).
//!
//! Views are plain `&self -> AnyElement` functions. What GPUI has to keep between frames
//! (the filter and env search fields, the table scroll handles) lives in [`Retained`] and
//! is created lazily in [`Workspace::prepare`].

mod drawer;

use std::collections::HashMap;

use gpui_kit::component::{
    Sizable as _, h_flex,
    input::{Input, InputEvent, InputState},
    v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, ElementId, InteractiveElement as _, IntoElement as _, ParentElement as _,
    ScrollHandle, Size as GSize, StatefulInteractiveElement as _, Styled as _, Subscription, div,
    px, relative,
};

use super::*;
use crate::app::table::{self, Col, Row, Table, TableScroll};

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

/// GPUI state a cluster tab keeps between frames.
#[derive(Default)]
pub(in crate::app) struct Retained {
    /// The filter field above a list; shows the current page's filter.
    filter: Option<Entity<InputState>>,
    /// Whether the field's placeholder describes the regex mode.
    filter_regex: Option<bool>,
    /// One env search field per container (they all show the same, shared text).
    env: HashMap<String, Entity<InputState>>,
    subs: Vec<Subscription>,
    /// Scroll handles of the current page's table.
    list: TableScroll,
    /// Reports the width the Overview's cards have to share.
    cards: ScrollHandle,
    /// The page the scroll handles were last used for.
    page: Option<Tab>,
    /// Sizes seen last frame; a change means the next layout needs another frame.
    sizes: [GSize<gpui_kit::Pixels>; 3],
}

type El = AnyElement;

fn act(a: Action) -> Message {
    Message::Action(a)
}

/// An element id from text.
fn eid(s: impl Into<String>) -> ElementId {
    ElementId::Name(s.into().into())
}

impl Workspace {
    /// Creates this tab's retained state (filter field, env search, list scroll) as needed
    /// and keeps it in step with the model.
    pub(in crate::app) fn prepare(&mut self, window: &mut Window, cx: &mut Context<App>) {
        if self.ui.filter.is_none() {
            let state = cx.new(|cx| InputState::new(window, cx).placeholder(filter_hint(false)));
            let sub = cx.subscribe_in(
                &state,
                window,
                |app: &mut App, state, ev: &InputEvent, window, cx| {
                    if matches!(ev, InputEvent::Change) {
                        let text = state.read(cx).value().to_string();
                        app.dispatch(Message::Filter(text), window, cx);
                    }
                },
            );
            self.ui.subs.push(sub);
            self.ui.filter = Some(state);
        }
        // Filters are per page: show this page's.
        if let Some(state) = self.ui.filter.clone() {
            let want = self.filters.get(&self.tab).cloned().unwrap_or_default();
            if state.read(cx).value().as_ref() != want {
                state.update(cx, |s, cx| s.set_value(want, window, cx));
            }
            if self.ui.filter_regex != Some(self.filter_regex) {
                self.ui.filter_regex = Some(self.filter_regex);
                state.update(cx, |s, cx| {
                    s.set_placeholder(filter_hint(self.filter_regex), window, cx)
                });
            }
        }

        // One env search per container of the open pod.
        let names: Vec<String> = match self.loaded_details().map(|d| &d.body) {
            Some(Body::Pod(p)) => p.containers.iter().map(|c| c.name.clone()).collect(),
            _ => Vec::new(),
        };
        for name in names {
            if self.ui.env.contains_key(&name) {
                continue;
            }
            let state = cx.new(|cx| InputState::new(window, cx).placeholder("Search env"));
            let sub = cx.subscribe_in(
                &state,
                window,
                |app: &mut App, state, ev: &InputEvent, window, cx| {
                    if matches!(ev, InputEvent::Change) {
                        let text = state.read(cx).value().to_string();
                        app.dispatch(Message::EnvFilter(text), window, cx);
                    }
                },
            );
            self.ui.subs.push(sub);
            self.ui.env.insert(name, state);
        }

        // A new page starts at the top.
        if self.ui.page != Some(self.tab) {
            self.ui.page = Some(self.tab);
            let top = gpui_kit::point(px(0.0), px(0.0));
            self.ui.list.vertical.set_offset(top);
            self.ui.list.horizontal.set_offset(top);
        }
        // Tables size themselves from last frame's bounds; ask for another frame when
        // those changed (window resize, a drawer opening, the first layout).
        let sizes = [
            self.ui.list.horizontal.bounds().size,
            self.ui.list.vertical.bounds().size,
            self.ui.cards.bounds().size,
        ];
        if sizes != self.ui.sizes {
            self.ui.sizes = sizes;
            cx.notify();
        }
    }

    /// Makes the env search fields show `text` (the app's shared env filter).
    pub(in crate::app) fn sync_env(
        &mut self,
        text: &str,
        window: &mut Window,
        cx: &mut Context<App>,
    ) {
        for state in self.ui.env.values() {
            if state.read(cx).value().as_ref() != text {
                state.update(cx, |s, cx| s.set_value(text.to_string(), window, cx));
            }
        }
    }

    // -----------------------------------------------------------------------
    // Lists
    // -----------------------------------------------------------------------

    /// The current page (Overview or a list).
    pub(in crate::app) fn view_page(&self, ctx: &ViewCtx<'_>) -> AnyElement {
        match self.tab {
            Tab::Overview => self.view_overview(ctx),
            Tab::Pods => self.view_pods(ctx),
            Tab::Services => self.view_services(ctx),
            Tab::Deployments => self.view_deployments(ctx),
            Tab::Workload(kind) => self.view_workloads(kind, ctx),
            _ => div().into_any_element(),
        }
    }

    fn filter_bar(&self, title: &str, shown: usize, total: Option<usize>) -> El {
        let filter = self
            .filters
            .get(&self.tab)
            .map(String::as_str)
            .unwrap_or("");
        let kind = self
            .tab
            .kinds()
            .first()
            .copied()
            .filter(|_| self.tab.is_list());
        let bar = h_flex()
            .w_full()
            .gap(px(10.0))
            .child(theme::title(title.to_string()))
            .when_some(total, |bar, n| {
                let label = if shown < n {
                    format!("{shown} of {n}")
                } else {
                    n.to_string()
                };
                bar.child(theme::colored(label, theme::TEXT_FAINT).text_size(px(15.0)))
            })
            .child(theme::hspace(6.0))
            .when_some(self.ui.filter.as_ref(), |bar, input| {
                bar.child(
                    div().w(px(300.0)).child(
                        Input::new(input)
                            .small()
                            .prefix(theme::icon(icon::MAGNIFYING_GLASS).text_size(px(13.0))),
                    ),
                )
            })
            .child(theme::toggle_button(
                "filter-regex",
                ".*",
                self.filter_regex,
                "Regular expression (case-insensitive)",
                Message::ToggleRegex,
            ))
            .when(!filter.is_empty(), |bar| {
                bar.child(theme::icon_button(
                    "filter-clear",
                    icon::X,
                    "Clear filter",
                    Some(Message::ClearFilter),
                ))
            })
            .when_some(self.matcher.error.clone(), |bar, err| {
                bar.child(theme::tip(
                    h_flex()
                        .id("filter-error")
                        .gap(px(5.0))
                        .child(theme::icon(icon::WARNING).text_color(theme::BAD))
                        .child(theme::colored("Invalid regex", theme::BAD)),
                    err,
                ))
            })
            .when_some(kind, |bar, kind| {
                bar.child(theme::fill_space()).child(theme::tip(
                    theme::primary_button(
                        "create",
                        Some(icon::PLUS),
                        "Create",
                        matches!(self.conn, Conn::Ready(_)).then(|| act(Action::Create(kind))),
                    ),
                    format!("New {} from a template", kind.label()),
                ))
            });
        v_flex()
            .w_full()
            .flex_none()
            .child(bar)
            .child(theme::vspace(10.0))
            .into_any_element()
    }

    /// Connection problems and progress; `None` once connected.
    fn conn_preamble(&self) -> Option<El> {
        match &self.conn {
            Conn::Failed(e) => {
                Some(theme::colored(format!("Could not connect: {e}"), RED).into_any_element())
            }
            Conn::Connecting => Some(theme::muted("Connecting to cluster…").into_any_element()),
            Conn::Idle => Some(theme::muted("Select a context to begin.").into_any_element()),
            Conn::Ready(_) => None,
        }
    }

    /// The page under the filter bar: connection and list problems, empty and loading
    /// states, or the table.
    fn list_body<T>(&self, state: &ListState<T>, table: impl FnOnce() -> El) -> El {
        if let Some(p) = self.conn_preamble() {
            return p;
        }
        let mut col = v_flex().w_full().flex_1().min_h_0().items_stretch();
        if let Some(e) = &state.error {
            col = col.child(error_callout(e));
        }
        if !state.loaded {
            col = col.child(theme::muted("Loading…"));
        } else if state.items.is_empty() {
            if state.error.is_none() {
                col = col.child(theme::muted("Nothing here."));
            }
        } else {
            col = col.child(table());
        }
        col.into_any_element()
    }

    fn page(bar: El, body: El) -> El {
        v_flex()
            .size_full()
            .items_stretch()
            .child(bar)
            .child(body)
            .into_any_element()
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

    fn view_pods(&self, ctx: &ViewCtx<'_>) -> El {
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
                scroll: &self.ui.list,
                row: |i, _w: &[f32]| {
                    let p: &PodRow = rows[i];
                    let color = status_color(&p.status);
                    // Problems read from across the room: tint anything not healthy.
                    let tint = if color == GREEN { theme::TEXT } else { color };
                    let mut cells: Vec<El> = vec![
                        table::name_cell(&p.name),
                        h_flex()
                            .gap(px(6.0))
                            .min_w_0()
                            .child(theme::dot(color))
                            .child(table::truncated(&p.status, move |s| {
                                theme::colored(s, tint)
                            }))
                            .into_any_element(),
                        restarts_badge(p.restarts),
                        theme::mono(p.ready.clone()).into_any_element(),
                        theme::mono_muted(p.age()).into_any_element(),
                    ];
                    if show_ns {
                        cells.push(table::truncated(&p.namespace, theme::mono_muted));
                    }
                    if show_node {
                        cells.push(table::truncated(&p.node, theme::mono_muted));
                    }
                    Row {
                        key: format!("{}/{}", p.namespace, p.name),
                        cells,
                        selected: self.is_selected(Kind::Pod, &p.namespace, &p.name),
                        on_click: Self::select_msg(Kind::Pod, &p.namespace, &p.name),
                        on_double_click: Some(act(Action::OpenLogs(p.clone(), None, false))),
                    }
                },
            }
            .view()
        });
        Self::page(bar, body)
    }

    fn view_services(&self, ctx: &ViewCtx<'_>) -> El {
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
                scroll: &self.ui.list,
                row: |i, _w: &[f32]| {
                    let s: &SvcRow = rows[i];
                    let mut cells: Vec<El> = vec![
                        table::name_cell(&s.name),
                        table::truncated(&s.kind, theme::muted),
                        table::truncated(&s.ports_label(), theme::mono),
                        table::truncated(&s.cluster_ip, theme::mono_muted),
                        theme::mono_muted(s.age()).into_any_element(),
                    ];
                    if show_ns {
                        cells.push(table::truncated(&s.namespace, theme::mono_muted));
                    }
                    if show_external {
                        cells.push(table::truncated(&s.external, theme::mono_muted));
                    }
                    Row {
                        key: format!("{}/{}", s.namespace, s.name),
                        cells,
                        selected: self.is_selected(Kind::Service, &s.namespace, &s.name),
                        on_click: Self::select_msg(Kind::Service, &s.namespace, &s.name),
                        on_double_click: None,
                    }
                },
            }
            .view()
        });
        Self::page(bar, body)
    }

    fn view_deployments(&self, ctx: &ViewCtx<'_>) -> El {
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
                scroll: &self.ui.list,
                row: |i, _w: &[f32]| {
                    let d: &DeployRow = rows[i];
                    let c = if d.ready >= d.replicas {
                        theme::TEXT
                    } else {
                        YELLOW
                    };
                    let mut cells: Vec<El> = vec![
                        table::name_cell(&d.name),
                        theme::mono(format!("{}/{}", d.ready, d.replicas))
                            .text_color(c)
                            .into_any_element(),
                        theme::mono_muted(d.updated.to_string()).into_any_element(),
                        theme::mono_muted(d.available.to_string()).into_any_element(),
                        theme::mono_muted(d.age()).into_any_element(),
                    ];
                    if show_ns {
                        cells.push(table::truncated(&d.namespace, theme::mono_muted));
                    }
                    Row {
                        key: format!("{}/{}", d.namespace, d.name),
                        cells,
                        selected: self.is_selected(Kind::Deployment, &d.namespace, &d.name),
                        on_click: Self::select_msg(Kind::Deployment, &d.namespace, &d.name),
                        on_double_click: None,
                    }
                },
            }
            .view()
        });
        Self::page(bar, body)
    }

    /// Daemon sets, stateful sets, replica sets, replication controllers, jobs, cron jobs.
    fn view_workloads(&self, kind: Kind, ctx: &ViewCtx<'_>) -> El {
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
                scroll: &self.ui.list,
                row: |i, _w: &[f32]| {
                    let r: &WorkRow = rows[i];
                    let mut cells: Vec<El> = vec![table::name_cell(&r.name)];
                    for (j, cell) in r.cells.iter().enumerate() {
                        let value = cell.text();
                        cells.push(if Some(j) == state_col {
                            let color = health_color(r.health);
                            let c = if r.health == Health::Ok {
                                theme::TEXT
                            } else {
                                color
                            };
                            let label = table::truncated(&value, move |s| {
                                theme::mono(s).text_color(c)
                            });
                            if columns[j].0 == "Status" {
                                h_flex()
                                    .gap(px(6.0))
                                    .min_w_0()
                                    .child(theme::dot(color))
                                    .child(label)
                                    .into_any_element()
                            } else {
                                label
                            }
                        } else if j == 0 {
                            table::truncated(&value, theme::mono)
                        } else {
                            table::truncated(&value, theme::mono_muted)
                        });
                    }
                    cells.push(table::truncated(&r.age(), theme::mono_muted));
                    if show_ns {
                        cells.push(table::truncated(&r.namespace, theme::mono_muted));
                    }
                    Row {
                        key: format!("{}/{}", r.namespace, r.name),
                        cells,
                        selected: self.is_selected(kind, &r.namespace, &r.name),
                        on_click: Self::select_msg(kind, &r.namespace, &r.name),
                        on_double_click: None,
                    }
                },
            }
            .view()
        });
        Self::page(bar, body)
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
    fn view_overview(&self, ctx: &ViewCtx<'_>) -> El {
        let header = h_flex()
            .gap(px(10.0))
            .child(theme::title("Overview"))
            .child(
                theme::colored(namespaces_label(&self.selected_ns, false), theme::TEXT_FAINT)
                    .text_size(px(15.0)),
            );
        if let Some(p) = self.conn_preamble() {
            return v_flex()
                .size_full()
                .items_start()
                .child(header)
                .child(theme::vspace(12.0))
                .child(p)
                .into_any_element();
        }

        // Cards share the width: four across when there's room, fewer on narrow windows.
        const MIN_CARD_W: f32 = 232.0;
        const GAP: f32 = 12.0;
        let width = f32::from(self.ui.cards.bounds().size.width);
        let per_row = if width > 0.0 {
            (((width + GAP) / (MIN_CARD_W + GAP)).floor() as usize).clamp(1, 4)
        } else {
            4
        };
        let kinds = Tab::Overview.kinds();
        let mut grid = v_flex().w_full().gap(px(GAP)).items_stretch();
        for chunk in kinds.chunks(per_row) {
            let mut r = h_flex().w_full().gap(px(GAP)).items_stretch();
            for kind in chunk {
                r = r.child(self.overview_card(*kind));
            }
            for _ in chunk.len()..per_row {
                r = r.child(div().flex_1());
            }
            grid = grid.child(r);
        }
        let cards = div()
            .id("overview-cards")
            .w_full()
            .flex_none()
            .overflow_hidden()
            .track_scroll(&self.ui.cards)
            .child(grid);

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

        let attention = h_flex()
            .gap(px(8.0))
            .child(theme::semibold("Needs attention").text_size(px(15.0)))
            .when(!problems.is_empty(), |d| {
                d.child(
                    theme::colored(problems.len().to_string(), theme::TEXT_FAINT)
                        .text_size(px(15.0)),
                )
            });
        let all_loaded = Tab::Overview
            .kinds()
            .iter()
            .all(|k| self.health_counts(*k).is_some());
        let list: El = if problems.is_empty() {
            theme::muted(if all_loaded {
                "Everything is healthy."
            } else {
                "Loading…"
            })
            .into_any_element()
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
                scroll: &self.ui.list,
                row: |i, _w: &[f32]| {
                    let (kind, ns, name, status, health): &(Kind, &str, &str, String, Health) =
                        &problems[i];
                    let color = health_color(*health);
                    let mut cells: Vec<El> = vec![
                        table::name_cell(name),
                        table::truncated(kind.label(), theme::muted),
                        h_flex()
                            .gap(px(6.0))
                            .min_w_0()
                            .child(theme::dot(color))
                            .child(table::truncated(status, move |s| theme::colored(s, color)))
                            .into_any_element(),
                    ];
                    if show_ns {
                        cells.push(table::truncated(ns, theme::mono_muted));
                    }
                    Row {
                        key: format!("{}/{}/{}", kind.label(), ns, name),
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
        v_flex()
            .size_full()
            .items_stretch()
            .child(header)
            .child(theme::vspace(12.0))
            .child(cards)
            .child(theme::vspace(22.0))
            .child(attention)
            .child(theme::vspace(8.0))
            .child(list)
            .into_any_element()
    }

    fn overview_card(&self, kind: Kind) -> El {
        let counts = self.health_counts(kind);
        let total = counts.map(|c| c.iter().sum::<usize>());
        let number = total.map(|t| t.to_string()).unwrap_or_else(|| "–".into());

        // Stacked health bar.
        let colors = [GREEN, YELLOW, RED, theme::IDLE];
        let mut bar = h_flex()
            .w_full()
            .h(px(5.0))
            .flex_none()
            .rounded(px(2.0))
            .overflow_hidden();
        match (counts, total.filter(|t| *t > 0)) {
            (Some(c), Some(total)) => {
                for (n, color) in c.iter().zip(colors) {
                    if *n == 0 {
                        continue;
                    }
                    bar = bar.child(
                        div()
                            .h_full()
                            .w(relative(*n as f32 / total as f32))
                            .bg(color),
                    );
                }
            }
            _ => {
                bar = bar.child(div().h_full().w_full().bg(theme::FIELD_ACTIVE));
            }
        }

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
        let content = v_flex()
            .w_full()
            .h(px(68.0))
            .gap(px(2.0))
            .items_stretch()
            .overflow_hidden()
            .child(
                h_flex()
                    .w_full()
                    .child(theme::muted(kind.plural()).text_size(px(13.0)))
                    .child(theme::fill_space())
                    .child(
                        theme::icon(kind_glyph(kind))
                            .text_size(px(15.0))
                            .text_color(theme::TEXT_FAINT),
                    ),
            )
            .child(theme::semibold(number).text_size(px(24.0)))
            .child(theme::vspace(6.0))
            .child(bar)
            .child(theme::vspace(6.0))
            .child(
                theme::colored(legend, color)
                    .text_size(px(12.0))
                    .overflow_hidden(),
            );
        div()
            .id(eid(format!("card/{}", kind.plural())))
            .flex_1()
            .min_w_0()
            .px(px(14.0))
            .py(px(12.0))
            .bg(theme::RAISED)
            .border_1()
            .border_color(theme::BORDER)
            .rounded(px(8.0))
            .cursor_pointer()
            .hover(|s| s.bg(theme::FIELD))
            .on_click(on(act(Action::Open(Tab::for_kind(kind)))))
            .child(content)
            .into_any_element()
    }
}

fn filter_hint(regex: bool) -> &'static str {
    if regex {
        "Regex on name or namespace"
    } else {
        "Filter by name or namespace"
    }
}

/// Restarts on a fixed severity ramp: none, tinted amber, filled red at five or more.
fn restarts_badge(restarts: i32) -> El {
    match restarts {
        0 => theme::mono("0")
            .text_color(theme::TEXT_FAINT)
            .into_any_element(),
        1..=4 => theme::badge(restarts.to_string(), theme::WARN, false).into_any_element(),
        _ => theme::badge(restarts.to_string(), theme::BAD, true).into_any_element(),
    }
}

/// A list error as a tinted box, one row per message (one per failed namespace).
fn error_callout(error: &str) -> El {
    let lines = error
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(|line| {
            h_flex()
                .gap(px(8.0))
                .items_start()
                .child(theme::icon(icon::WARNING_CIRCLE).text_color(theme::BAD))
                .child(
                    theme::txt(line.to_string())
                        .whitespace_normal()
                        .min_w_0()
                        .flex_1(),
                )
        })
        .collect::<Vec<_>>();
    v_flex()
        .w_full()
        .flex_none()
        .child(
            theme::boxed(
                theme::alpha(theme::BAD, 0.10),
                theme::alpha(theme::BAD, 0.45),
                theme::RADIUS,
            )
            .w_full()
            .px(px(12.0))
            .py(px(10.0))
            .child(v_flex().gap(px(4.0)).items_stretch().children(lines)),
        )
        .child(theme::vspace(8.0))
        .into_any_element()
}

#[allow(dead_code)]
pub(in crate::app) const DRAWER_MENU: &str = "drawer-menu";
#[allow(dead_code)]
pub(in crate::app) const SHELL_MENU: &str = "shell-menu";
