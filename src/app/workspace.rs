//! One cluster tab: a kubeconfig + context + namespace with its own lists and drawer.

use super::*;

/// The running watch for one list. Dropping it stops the watch.
struct WatchTask {
    /// (generation, tab) it was started for; anything else means it's stale.
    key: (u64, Tab),
    /// One per list the page shows (the Overview watches every workload kind).
    handles: Vec<tokio::task::AbortHandle>,
}

impl Drop for WatchTask {
    fn drop(&mut self) {
        for h in &self.handles {
            h.abort();
        }
    }
}

pub(super) struct Workspace {
    pub(super) id: u64,
    rt: Handle,
    bus: Bus,

    pub(super) kubeconfig_path: PathBuf,
    pub(super) kubeconfig: Option<KubeconfigInfo>,
    pub(super) context: Option<String>,
    pub(super) conn: Conn,
    /// Every namespace in the cluster (or the context default, if listing is forbidden).
    pub(super) namespaces: Vec<String>,
    /// Selected namespaces, sorted. Empty means all namespaces.
    pub(super) selected_ns: Vec<String>,
    /// Selection to restore once connected (from a saved tab); takes precedence over settings.
    restore_ns: Option<Vec<String>>,
    /// Bumped whenever context or namespace changes so stale responses are dropped.
    generation: u64,

    pub(super) tab: Tab,
    /// Name filter per page; kept when switching pages and saved with the tab.
    pub(super) filters: HashMap<Tab, String>,
    /// Table filters are regular expressions (one switch for every page).
    pub(super) filter_regex: bool,
    /// Compiled filter for the current page; refreshed by `ui_filter_bar`.
    matcher: super::filter::Matcher,
    /// Sorted column per page; saved with the tab.
    pub(super) sorts: HashMap<Tab, Sort>,
    pub(super) context_query: String,
    pub(super) namespace_query: String,
    pub(super) pods: ListState<PodRow>,
    pub(super) services: ListState<SvcRow>,
    pub(super) deployments: ListState<DeployRow>,
    /// One list per `WORKLOAD_KINDS` entry.
    workloads: HashMap<Kind, ListState<WorkRow>>,
    pub(super) last_refresh: Option<Instant>,
    /// Keep the visible list live with a watch (vs. loading it once per refresh).
    live: bool,
    watch: Option<WatchTask>,
    last_details: Option<Instant>,
    selected: Option<Selected>,
    pub(super) drawer_tab: DrawerTab,
    details: Option<DetailsState>,
    /// Secrets the user chose to reveal, keyed by (namespace, name). Cleared when the drawer changes.
    secrets: HashMap<(String, String), Result<BTreeMap<String, String>, String>>,
    secrets_loading: HashSet<(String, String)>,
}

impl Workspace {
    /// Opens a tab for `path`. `context`/`namespace` come from a saved tab; otherwise the
    /// last-used (or kubeconfig's current) context is picked.
    pub(super) fn new(
        id: u64,
        rt: Handle,
        bus: Bus,
        path: PathBuf,
        context: Option<String>,
        namespaces: Option<Vec<String>>,
        settings: &mut Settings,
    ) -> Self {
        let mut ws = Self {
            id,
            rt,
            bus,
            kubeconfig_path: path.clone(),
            kubeconfig: None,
            context: None,
            conn: Conn::Idle,
            namespaces: Vec::new(),
            selected_ns: Vec::new(),
            restore_ns: namespaces,
            generation: 0,
            tab: Tab::Pods,
            filters: HashMap::new(),
            filter_regex: false,
            matcher: Default::default(),
            sorts: HashMap::new(),
            context_query: String::new(),
            namespace_query: String::new(),
            pods: ListState::default(),
            services: ListState::default(),
            deployments: ListState::default(),
            workloads: empty_workloads(),
            last_refresh: None,
            live: false,
            watch: None,
            last_details: None,
            selected: None,
            drawer_tab: DrawerTab::Overview,
            details: None,
            secrets: HashMap::new(),
            secrets_loading: HashSet::new(),
        };
        ws.load_kubeconfig(path, context, settings);
        ws
    }

    /// Short label for the cluster tab strip.
    pub(super) fn title(&self) -> String {
        let context = self.context.clone().unwrap_or_else(|| file_label(&self.kubeconfig_path));
        match &self.conn {
            Conn::Ready(_) => format!("{context} · {}", namespaces_label(&self.selected_ns, true)),
            _ => context,
        }
    }

    pub(super) fn saved(&self) -> SavedWorkspace {
        let selection = match &self.conn {
            Conn::Ready(_) => Some(self.selected_ns.clone()),
            // Not connected yet: keep what we were asked to restore.
            _ => self.restore_ns.clone(),
        };
        SavedWorkspace {
            kubeconfig: self.kubeconfig_path.clone(),
            context: self.context.clone(),
            namespaces: selection.clone().unwrap_or_default(),
            all_namespaces: selection.is_some_and(|s| s.is_empty()),
            namespace: None,
            filters: self
                .filters
                .iter()
                .filter(|(_, v)| !v.is_empty())
                .map(|(t, v)| (t.key().to_string(), v.clone()))
                .collect(),
            sorts: self.sorts.iter().map(|(t, s)| (t.key().to_string(), s.saved())).collect(),
            filter_regex: self.filter_regex,
        }
    }

    // -----------------------------------------------------------------------
    // Environment switching
    // -----------------------------------------------------------------------

    pub(super) fn load_kubeconfig(&mut self, path: PathBuf, context: Option<String>, settings: &mut Settings) {
        self.kubeconfig_path = path.clone();
        match kube_ops::read_kubeconfig(&path) {
            Ok(info) => {
                if !settings.kubeconfigs.contains(&path) {
                    settings.kubeconfigs.push(path.clone());
                    settings.save();
                }
                let key = path.to_string_lossy().to_string();
                let context = context
                    .filter(|c| info.contexts.contains(c))
                    .or_else(|| settings.last_context.get(&key).filter(|c| info.contexts.contains(c)).cloned())
                    .or_else(|| info.current.clone().filter(|c| info.contexts.contains(c)))
                    .or_else(|| info.contexts.first().cloned());
                self.kubeconfig = Some(info);
                self.context = None;
                if let Some(context) = context {
                    self.switch_context(context, settings);
                }
            }
            Err(e) => {
                self.kubeconfig = None;
                self.context = None;
                self.clear_lists();
                self.conn = Conn::Failed(err_string(e));
            }
        }
    }

    pub(super) fn switch_context(&mut self, context: String, settings: &mut Settings) {
        settings
            .last_context
            .insert(self.kubeconfig_path.to_string_lossy().to_string(), context.clone());
        settings.save();

        self.generation += 1;
        self.context = Some(context.clone());
        self.conn = Conn::Connecting;
        self.namespaces.clear();
        self.clear_lists();

        let (ws, generation, path, bus) = (self.id, self.generation, self.kubeconfig_path.clone(), self.bus.clone());
        self.rt.spawn(async move {
            let result = async {
                let (clients, default_ns) = kube_ops::connect(&path, &context).await?;
                // Listing namespaces may be forbidden; fall back to the context default.
                let namespaces = kube_ops::list_namespaces(&clients.main).await.unwrap_or_default();
                Ok((clients, default_ns, namespaces))
            }
            .await
            .map_err(err_string);
            bus.send(Msg::Connected { ws, generation, result });
        });
    }

    /// Selects namespaces (empty = all) and remembers the choice for this context.
    pub(super) fn set_namespaces(&mut self, mut namespaces: Vec<String>, settings: &mut Settings) {
        namespaces.sort();
        namespaces.dedup();
        if self.selected_ns == namespaces {
            return;
        }
        if let Some(context) = &self.context {
            settings.last_namespace.insert(context.clone(), namespaces.join(","));
            settings.save();
        }
        self.generation += 1;
        self.selected_ns = namespaces;
        self.clear_lists();
        self.refresh();
    }

    pub(super) fn set_tab(&mut self, tab: Tab) {
        if self.tab != tab {
            self.tab = tab;
            self.close_drawer();
            self.refresh();
        }
    }

    // -----------------------------------------------------------------------
    // Messages and actions for this tab
    // -----------------------------------------------------------------------

    pub(super) fn handle(&mut self, msg: Msg, settings: &Settings) {
        match msg {
            Msg::Connected { generation, result, .. } => {
                if generation != self.generation {
                    return;
                }
                match result {
                    Ok((clients, default_ns, mut namespaces)) => {
                        if namespaces.is_empty() {
                            namespaces.push(default_ns.clone());
                        }
                        self.namespaces = namespaces;
                        self.conn = Conn::Ready(clients);
                        let saved = self.context.as_ref().and_then(|c| settings.last_namespace.get(c)).map(|s| {
                            s.split(',').filter(|n| !n.is_empty()).map(str::to_string).collect::<Vec<_>>()
                        });
                        let mut selection = self.restore_ns.take().or(saved).unwrap_or_else(|| vec![default_ns]);
                        selection.sort();
                        self.selected_ns = selection;
                        self.refresh();
                    }
                    Err(e) => self.conn = Conn::Failed(e),
                }
            }
            Msg::Listed { generation, kind, result, warning, .. } => {
                if generation != self.generation {
                    return;
                }
                fn apply<T>(state: &mut ListState<T>, result: Result<Vec<T>, String>, warning: Option<String>) {
                    state.loading = false;
                    state.loaded = true;
                    match result {
                        Ok(items) => {
                            state.items = items;
                            state.error = warning;
                        }
                        Err(e) => state.error = Some(e),
                    }
                }
                match (kind, result) {
                    (_, Ok(ListData::Pods(v))) => apply(&mut self.pods, Ok(v), warning),
                    (_, Ok(ListData::Services(v))) => apply(&mut self.services, Ok(v), warning),
                    (_, Ok(ListData::Deployments(v))) => apply(&mut self.deployments, Ok(v), warning),
                    (Kind::Pod, Err(e)) => apply(&mut self.pods, Err(e), warning),
                    (Kind::Service, Err(e)) => apply(&mut self.services, Err(e), warning),
                    (Kind::Deployment, Err(e)) => apply(&mut self.deployments, Err(e), warning),
                    (k, result) => {
                        if let Some(state) = self.workloads.get_mut(&k) {
                            let result = result.map(|data| match data {
                                ListData::Workloads(v) => v,
                                _ => Vec::new(),
                            });
                            apply(state, result, warning);
                        }
                    }
                }
            }
            Msg::Details { sel, result, .. } => {
                if let Some(d) = self.details.as_mut().filter(|d| d.sel == sel) {
                    d.loading = false;
                    // Keep showing the last good data if a background refresh fails.
                    if result.is_ok() || !matches!(d.data, Some(Ok(_))) {
                        d.data = Some(result);
                    }
                }
            }
            Msg::Secret { key, result, .. } => {
                if self.secrets_loading.remove(&key) {
                    self.secrets.insert(key, result);
                }
            }
            _ => {}
        }
    }

    pub(super) fn select(&mut self, sel: Selected) {
        if self.selected.as_ref() != Some(&sel) {
            self.close_drawer();
            self.drawer_tab = DrawerTab::Overview;
            self.selected = Some(sel);
            self.fetch_details();
        }
    }

    pub(super) fn go_to_pod(&mut self, namespace: String, name: String) {
        self.tab = Tab::Pods;
        // Keep the user's filter unless it would hide the pod we're jumping to.
        if !self.matches(&name, &namespace) {
            self.filters.remove(&Tab::Pods);
        }
        self.close_drawer();
        self.drawer_tab = DrawerTab::Overview;
        self.selected = Some(Selected { kind: Kind::Pod, namespace, name });
        self.refresh();
    }

    pub(super) fn reveal_secret(&mut self, ns: String, name: String) {
        let Some(client) = self.client() else { return };
        let key = (ns.clone(), name.clone());
        if !self.secrets_loading.insert(key.clone()) {
            return;
        }
        let (ws, bus) = (self.id, self.bus.clone());
        self.rt.spawn(async move {
            let result = details::read_secret(client, ns, name).await.map_err(err_string);
            bus.send(Msg::Secret { ws, key, result });
        });
    }

    pub(super) fn hide_secret(&mut self, ns: String, name: String) {
        self.secrets.remove(&(ns, name));
    }

    pub(super) fn clients(&self) -> Option<&Clients> {
        match &self.conn {
            Conn::Ready(c) => Some(c),
            _ => None,
        }
    }
    pub(super) fn client(&self) -> Option<Client> {
        self.clients().map(|c| c.main.clone())
    }
    pub(super) fn clear_lists(&mut self) {
        self.pods = ListState::default();
        self.services = ListState::default();
        self.deployments = ListState::default();
        self.workloads = empty_workloads();
        self.last_refresh = None;
        self.watch = None;
        self.close_drawer();
    }

    /// Called every frame for the front tab: starts, restarts or stops the watch so
    /// it always matches the current context, namespaces and page.
    pub(super) fn sync_watch(&mut self, live: bool) {
        self.live = live;
        let want = live && self.client().is_some() && !self.tab.kinds().is_empty();
        let current = self.watch.as_ref().map(|w| w.key);
        if want && current != Some((self.generation, self.tab)) {
            self.refresh();
        } else if !want {
            self.watch = None;
        }
    }

    /// Background tabs don't watch; `sync_watch` resumes (with a fresh list) when the tab comes back.
    pub(super) fn pause_watch(&mut self) {
        self.watch = None;
    }

    /// An action (delete, scale, ...) finished. A live list picks the change up by itself.
    pub(super) fn changed(&mut self) {
        if self.watch.is_some() {
            self.fetch_details();
        } else {
            self.refresh();
        }
    }

    /// Keeps the open drawer's details fresh; the watch only covers the list rows.
    pub(super) fn poll_details(&mut self, every: Duration) {
        if self.details.is_some() && self.last_details.is_none_or(|t| t.elapsed() >= every) {
            self.fetch_details();
        }
    }

    /// Reloads the visible lists: restarts the watches (which relist) when live, otherwise lists once.
    pub(super) fn refresh(&mut self) {
        let Some(client) = self.client() else { return };
        self.fetch_details();
        let kinds = self.tab.kinds();
        if kinds.is_empty() {
            return;
        }
        self.last_refresh = Some(Instant::now());
        self.watch = None;
        let mut handles = Vec::new();
        for kind in kinds {
            self.set_loading(kind);
            handles.push(self.spawn_list(client.clone(), kind).abort_handle());
        }
        if self.live {
            self.watch = Some(WatchTask { key: (self.generation, self.tab), handles });
        }
    }

    fn set_loading(&mut self, kind: Kind) {
        match kind {
            Kind::Pod => self.pods.loading = true,
            Kind::Service => self.services.loading = true,
            Kind::Deployment => self.deployments.loading = true,
            k => {
                if let Some(s) = self.workloads.get_mut(&k) {
                    s.loading = true;
                }
            }
        }
    }

    fn kind_loading(&self, kind: Kind) -> bool {
        match kind {
            Kind::Pod => self.pods.loading,
            Kind::Service => self.services.loading,
            Kind::Deployment => self.deployments.loading,
            k => self.workloads.get(&k).is_some_and(|s| s.loading),
        }
    }

    /// Starts loading one list: a watch that runs until aborted when live, else a single list.
    fn spawn_list(&self, client: Client, kind: Kind) -> tokio::task::JoinHandle<()> {
        let (ws, generation, namespaces, bus, live) =
            (self.id, self.generation, self.selected_ns.clone(), self.bus.clone(), self.live);
        let send = move |result: Result<ListData, String>, warning: Option<String>| {
            bus.send(Msg::Listed { ws, generation, kind, result, warning })
        };
        self.rt.spawn(async move {
            if live {
                match kind {
                    Kind::Pod => {
                        kube_ops::watch_rows(client, namespaces, kube_ops::pod_row, move |rows, errors| {
                            let (r, w) = snapshot(rows, errors);
                            send(r.map(ListData::Pods), w)
                        })
                        .await
                    }
                    Kind::Service => {
                        kube_ops::watch_rows(client, namespaces, kube_ops::svc_row, move |rows, errors| {
                            let (r, w) = snapshot(rows, errors);
                            send(r.map(ListData::Services), w)
                        })
                        .await
                    }
                    Kind::Deployment => {
                        kube_ops::watch_rows(client, namespaces, kube_ops::deploy_row, move |rows, errors| {
                            let (r, w) = snapshot(rows, errors);
                            send(r.map(ListData::Deployments), w)
                        })
                        .await
                    }
                    k => {
                        kube_ops::watch_workloads(client, k, namespaces, move |rows, errors| {
                            let (r, w) = snapshot(rows, errors);
                            send(r.map(ListData::Workloads), w)
                        })
                        .await
                    }
                }
                return;
            }
            let (result, warning) = match kind {
                Kind::Pod => {
                    let (r, w) = list_in(&namespaces, |ns| kube_ops::list_pods(client.clone(), ns)).await;
                    (r.map(ListData::Pods), w)
                }
                Kind::Service => {
                    let (r, w) = list_in(&namespaces, |ns| kube_ops::list_services(client.clone(), ns)).await;
                    (r.map(ListData::Services), w)
                }
                Kind::Deployment => {
                    let (r, w) = list_in(&namespaces, |ns| kube_ops::list_deployments(client.clone(), ns)).await;
                    (r.map(ListData::Deployments), w)
                }
                k => {
                    let (r, w) = list_in(&namespaces, |ns| kube_ops::list_workloads(client.clone(), k, ns)).await;
                    (r.map(ListData::Workloads), w)
                }
            };
            send(result, warning);
        })
    }

    /// The first row of a list page, for the dev screenshot.
    pub(super) fn first_row(&self, tab: Tab) -> Option<Selected> {
        let (kind, namespace, name) = match tab {
            Tab::Pods => self.pods.items.first().map(|r| (Kind::Pod, &r.namespace, &r.name))?,
            Tab::Services => self.services.items.first().map(|r| (Kind::Service, &r.namespace, &r.name))?,
            Tab::Deployments => self.deployments.items.first().map(|r| (Kind::Deployment, &r.namespace, &r.name))?,
            Tab::Workload(k) => self.workload(k).items.first().map(|r| (k, &r.namespace, &r.name))?,
            _ => return None,
        };
        Some(Selected { kind, namespace: namespace.clone(), name: name.clone() })
    }

    /// Pods in a failing state (CrashLoopBackOff, ImagePullBackOff, Failed, …).
    pub(super) fn failing_pods(&self) -> usize {
        self.pods.items.iter().filter(|p| pod_health(&p.status) == Health::Bad).count()
    }

    pub(super) fn workload(&self, kind: Kind) -> &ListState<WorkRow> {
        &self.workloads[&kind]
    }

    pub(super) fn is_loading(&self) -> bool {
        self.tab.kinds().into_iter().any(|k| self.kind_loading(k))
    }
    /// (Re)loads the drawer's details for the current selection.
    pub(super) fn fetch_details(&mut self) {
        let Some(sel) = self.selected.clone() else { return };
        let Some(client) = self.client() else { return };
        let state = match &mut self.details {
            Some(d) if d.sel == sel => d,
            slot => slot.insert(DetailsState { sel: sel.clone(), data: None, loading: false }),
        };
        if state.loading {
            return;
        }
        state.loading = true;
        self.last_details = Some(Instant::now());
        let (ws, bus) = (self.id, self.bus.clone());
        self.rt.spawn(async move {
            let result = details::fetch(client, sel.kind, sel.namespace.clone(), sel.name.clone())
                .await
                .map_err(err_string);
            bus.send(Msg::Details { ws, sel, result });
        });
    }

    // -----------------------------------------------------------------------
    // Rendering: lists
    // -----------------------------------------------------------------------

    fn ui_filter_bar(&mut self, ui: &mut egui::Ui, title: &str, create_kind: &mut Option<Kind>) {
        let pattern = self.filters.get(&self.tab).map(String::as_str).unwrap_or("");
        self.matcher.update(pattern, self.filter_regex);
        // "512", or "8 of 512" while a filter narrows the list.
        let (shown, total) = match self.tab {
            Tab::Pods => (self.pods.items.iter().filter(|p| self.matches(&p.name, &p.namespace)).count(), count(&self.pods)),
            Tab::Services => (self.services.items.iter().filter(|s| self.matches(&s.name, &s.namespace)).count(), count(&self.services)),
            Tab::Deployments => (self.deployments.items.iter().filter(|d| self.matches(&d.name, &d.namespace)).count(), count(&self.deployments)),
            Tab::Workload(k) => {
                let state = self.workload(k);
                (state.items.iter().filter(|w| self.matches(&w.name, &w.namespace)).count(), count(state))
            }
            _ => (0, None),
        };
        ui.horizontal(|ui| {
            ui.label(theme::title(title));
            if let Some(n) = total {
                let text = if shown < n { format!("{shown} of {n}") } else { n.to_string() };
                ui.label(RichText::new(text).size(15.0).color(theme::TEXT_FAINT));
            }
            ui.add_space(16.0);
            let hint = if self.filter_regex { "Regex on name or namespace" } else { "Filter by name or namespace" };
            let filter = self.filters.entry(self.tab).or_default();
            theme::search_field(ui, filter, hint, 300.0);
            let has_text = !filter.is_empty();
            if theme::toggle_button(ui, ".*", self.filter_regex, "Regular expression (case-insensitive)").clicked() {
                self.filter_regex = !self.filter_regex;
            }
            if has_text && theme::icon_button(ui, icon::X, "Clear filter").clicked() {
                self.filters.remove(&self.tab);
            }
            if let Some(err) = &self.matcher.error {
                ui.label(RichText::new(format!("{} Invalid regex", icon::WARNING)).color(theme::BAD))
                    .on_hover_text(err);
            }
            if let Some(kind) = self.tab.kinds().first().copied().filter(|_| self.tab.is_list()) {
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let create = theme::primary_button(format!("{} Create", icon::PLUS));
                    if ui
                        .add_enabled(matches!(self.conn, Conn::Ready(_)), create)
                        .on_hover_text(format!("New {} from a template", kind.label()))
                        .clicked()
                    {
                        *create_kind = Some(kind);
                    }
                });
            }
        });
        ui.add_space(10.0);
    }

    fn matches(&self, name: &str, namespace: &str) -> bool {
        !self.matcher.is_active() || self.matcher.matches(name) || self.matcher.matches(namespace)
    }

    fn is_selected(&self, kind: Kind, namespace: &str, name: &str) -> bool {
        self.selected
            .as_ref()
            .is_some_and(|s| s.kind == kind && s.namespace == namespace && s.name == name)
    }

    /// Shared header for list views: connection problems, list errors, empty states.
    /// Returns false when there's nothing to show.
    fn list_preamble<T>(&self, ui: &mut egui::Ui, state: &ListState<T>) -> bool {
        if !self.conn_preamble(ui) {
            return false;
        }
        if let Some(e) = &state.error {
            error_callout(ui, e);
        }
        if !state.loaded {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label("Loading…");
            });
            return false;
        }
        if state.items.is_empty() {
            if state.error.is_some() {
                return false;
            }
            ui.label("Nothing here.");
            return false;
        }
        true
    }

    /// Connection problems and progress. Returns false until connected.
    fn conn_preamble(&self, ui: &mut egui::Ui) -> bool {
        match &self.conn {
            Conn::Failed(e) => {
                ui.colored_label(RED, format!("Could not connect: {e}"));
                return false;
            }
            Conn::Connecting => {
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label("Connecting to cluster…");
                });
                return false;
            }
            Conn::Idle => {
                ui.label("Select a context to begin.");
                return false;
            }
            Conn::Ready(_) => {}
        }
        true
    }

    fn table<'a>(ui: &'a mut egui::Ui) -> TableBuilder<'a> {
        // Rows are exactly ROW_HEIGHT apart; egui_extras would add item spacing on top.
        ui.spacing_mut().item_spacing.y = 0.0;
        TableBuilder::new(ui)
            .striped(true)
            .resizable(false)
            .sense(Sense::click())
            .cell_layout(egui::Layout::left_to_right(egui::Align::Center))
    }

    pub(super) fn ui_pods(&mut self, ui: &mut egui::Ui, actions: &mut Vec<Action>) {
        let mut create_kind = None;
        self.ui_filter_bar(ui, "Pods", &mut create_kind);
        if let Some(kind) = create_kind {
            actions.push(Action::Create(kind));
        }
        if !self.list_preamble(ui, &self.pods) {
            return;
        }
        let show_ns = self.selected_ns.len() != 1;
        // With the drawer open, Node lives in the drawer; give its width to the name.
        let show_node = self.selected.is_none();
        let mut rows: Vec<&PodRow> = self
            .pods
            .items
            .iter()
            .filter(|p| self.matches(&p.name, &p.namespace))
            .collect();
        let sort = self.sorts.get(&Tab::Pods).cloned();
        let sort = sort.as_ref();
        let mut clicked = None;
        sort_rows(&mut rows, sort, |p, col| match col {
            "Status" => SortKey::text(&p.status),
            "Restarts" => SortKey::Num(p.restarts as i64),
            "Ready" => SortKey::text(&p.ready),
            "Age" => SortKey::age(p.created.as_ref()),
            "Namespace" => SortKey::text(&p.namespace),
            "Node" => SortKey::text(&p.node),
            _ => SortKey::text(&p.name),
        });

        // State first (status, restarts, ready) so it stays visible next to the drawer.
        // Wide tables scroll sideways instead of clipping columns off the edge.
        egui::ScrollArea::horizontal().auto_shrink(false).show(ui, |ui| {
            let mut table = Self::table(ui)
                .column(Column::remainder().at_least(180.0).clip(true))
                .column(Column::exact(132.0).clip(true))
                // Room for the header's sort arrow.
                .column(Column::exact(80.0))
                .column(Column::exact(64.0))
                .column(Column::exact(72.0));
            if show_ns {
                table = table.column(Column::exact(110.0).clip(true));
            }
            if show_node {
                table = table.column(Column::exact(190.0).clip(true));
            }
            table
                .header(26.0, |mut h| {
                    for t in ["Name", "Status", "Restarts", "Ready", "Age"] {
                        h.col(|ui| sort_header(ui, t, sort, &mut clicked));
                    }
                    if show_ns {
                        h.col(|ui| sort_header(ui, "Namespace", sort, &mut clicked));
                    }
                    if show_node {
                        h.col(|ui| sort_header(ui, "Node", sort, &mut clicked));
                    }
                })
                .body(|body| {
                    body.rows(ROW_HEIGHT, rows.len(), |mut row| {
                        let p = rows[row.index()];
                        row.set_selected(self.is_selected(Kind::Pod, &p.namespace, &p.name));
                        row.col(|ui| name_cell(ui, &p.name));
                        row.col(|ui| {
                            let color = status_color(&p.status);
                            dot(ui, color);
                            // Problems read from across the room: tint anything not healthy.
                            let text = RichText::new(&p.status);
                            let text = if color == GREEN { text.color(theme::TEXT) } else { text.color(color) };
                            // Long reasons (Init:CrashLoopBackOff) end in an ellipsis; hover shows them in full.
                            ui.add(egui::Label::new(text).truncate()).on_hover_text(&p.status);
                        });
                        row.col(|ui| restarts_badge(ui, p.restarts));
                        row.col(|ui| { ui.label(mono(&p.ready)); });
                        row.col(|ui| { ui.label(mono_muted(p.age())); });
                        if show_ns {
                            row.col(|ui| { ui.label(mono_muted(&p.namespace)); });
                        }
                        if show_node {
                            row.col(|ui| { ui.label(mono_muted(&p.node)); });
                        }

                        let resp = row.response();
                        if resp.double_clicked() {
                            actions.push(Action::OpenLogs(p.clone(), None, false));
                        }
                        if resp.clicked() {
                            actions.push(Action::Select(Selected {
                                kind: Kind::Pod,
                                namespace: p.namespace.clone(),
                                name: p.name.clone(),
                            }));
                        }
                    });
                });
        });
        if let Some(col) = clicked {
            self.toggle_sort(Tab::Pods, col);
        }
    }

    pub(super) fn ui_services(&mut self, ui: &mut egui::Ui, actions: &mut Vec<Action>) {
        let mut create_kind = None;
        self.ui_filter_bar(ui, "Services", &mut create_kind);
        if let Some(kind) = create_kind {
            actions.push(Action::Create(kind));
        }
        if !self.list_preamble(ui, &self.services) {
            return;
        }
        let show_ns = self.selected_ns.len() != 1;
        let show_external = self.selected.is_none();
        let mut rows: Vec<&SvcRow> = self
            .services
            .items
            .iter()
            .filter(|s| self.matches(&s.name, &s.namespace))
            .collect();
        let sort = self.sorts.get(&Tab::Services).cloned();
        let sort = sort.as_ref();
        let mut clicked = None;
        sort_rows(&mut rows, sort, |s, col| match col {
            "Type" => SortKey::text(&s.kind),
            "Ports" => SortKey::text(&s.ports_label()),
            "Cluster IP" => SortKey::text(&s.cluster_ip),
            "Age" => SortKey::age(s.created.as_ref()),
            "Namespace" => SortKey::text(&s.namespace),
            "External" => SortKey::text(&s.external),
            _ => SortKey::text(&s.name),
        });

        // Wide tables scroll sideways instead of clipping columns off the edge.
        egui::ScrollArea::horizontal().auto_shrink(false).show(ui, |ui| {
            let mut table = Self::table(ui)
                .column(Column::remainder().at_least(180.0).clip(true))
                .column(Column::exact(96.0))
                .column(Column::exact(170.0).clip(true))
                .column(Column::exact(120.0).clip(true))
                .column(Column::exact(72.0));
            if show_ns {
                table = table.column(Column::exact(110.0).clip(true));
            }
            if show_external {
                table = table.column(Column::exact(170.0).clip(true));
            }
            table
                .header(26.0, |mut h| {
                    for t in ["Name", "Type", "Ports", "Cluster IP", "Age"] {
                        h.col(|ui| sort_header(ui, t, sort, &mut clicked));
                    }
                    if show_ns {
                        h.col(|ui| sort_header(ui, "Namespace", sort, &mut clicked));
                    }
                    if show_external {
                        h.col(|ui| sort_header(ui, "External", sort, &mut clicked));
                    }
                })
                .body(|body| {
                    body.rows(ROW_HEIGHT, rows.len(), |mut row| {
                        let s = rows[row.index()];
                        row.set_selected(self.is_selected(Kind::Service, &s.namespace, &s.name));
                        row.col(|ui| name_cell(ui, &s.name));
                        row.col(|ui| { ui.label(RichText::new(&s.kind).color(theme::TEXT_MUTED)); });
                        row.col(|ui| { ui.label(mono(s.ports_label())); });
                        row.col(|ui| { ui.label(mono_muted(&s.cluster_ip)); });
                        row.col(|ui| { ui.label(mono_muted(s.age())); });
                        if show_ns {
                            row.col(|ui| { ui.label(mono_muted(&s.namespace)); });
                        }
                        if show_external {
                            row.col(|ui| { ui.label(mono_muted(&s.external)); });
                        }
                        if row.response().clicked() {
                            actions.push(Action::Select(Selected {
                                kind: Kind::Service,
                                namespace: s.namespace.clone(),
                                name: s.name.clone(),
                            }));
                        }
                    });
                });
        });
        if let Some(col) = clicked {
            self.toggle_sort(Tab::Services, col);
        }
    }

    pub(super) fn ui_deployments(&mut self, ui: &mut egui::Ui, actions: &mut Vec<Action>) {
        let mut create_kind = None;
        self.ui_filter_bar(ui, "Deployments", &mut create_kind);
        if let Some(kind) = create_kind {
            actions.push(Action::Create(kind));
        }
        if !self.list_preamble(ui, &self.deployments) {
            return;
        }
        let show_ns = self.selected_ns.len() != 1;
        let mut rows: Vec<&DeployRow> = self
            .deployments
            .items
            .iter()
            .filter(|d| self.matches(&d.name, &d.namespace))
            .collect();
        let sort = self.sorts.get(&Tab::Deployments).cloned();
        let sort = sort.as_ref();
        let mut clicked = None;
        sort_rows(&mut rows, sort, |d, col| match col {
            // Least ready first when ascending: a fraction, scaled to keep integer keys.
            "Ready" => SortKey::Num(if d.replicas == 0 { 1_000_000 } else { d.ready as i64 * 1_000_000 / d.replicas as i64 }),
            "Up-to-date" => SortKey::Num(d.updated as i64),
            "Available" => SortKey::Num(d.available as i64),
            "Age" => SortKey::age(d.created.as_ref()),
            "Namespace" => SortKey::text(&d.namespace),
            _ => SortKey::text(&d.name),
        });

        // Wide tables scroll sideways instead of clipping columns off the edge.
        egui::ScrollArea::horizontal().auto_shrink(false).show(ui, |ui| {
            let mut table = Self::table(ui)
                .column(Column::remainder().at_least(180.0).clip(true))
                .column(Column::exact(80.0))
                .column(Column::exact(102.0))
                .column(Column::exact(92.0))
                .column(Column::exact(72.0));
            if show_ns {
                table = table.column(Column::exact(110.0).clip(true));
            }
            table
                .header(26.0, |mut h| {
                    for t in ["Name", "Ready", "Up-to-date", "Available", "Age"] {
                        h.col(|ui| sort_header(ui, t, sort, &mut clicked));
                    }
                    if show_ns {
                        h.col(|ui| sort_header(ui, "Namespace", sort, &mut clicked));
                    }
                })
                .body(|body| {
                    body.rows(ROW_HEIGHT, rows.len(), |mut row| {
                        let d = rows[row.index()];
                        row.set_selected(self.is_selected(Kind::Deployment, &d.namespace, &d.name));
                        row.col(|ui| name_cell(ui, &d.name));
                        row.col(|ui| {
                            let c = if d.ready >= d.replicas { theme::TEXT } else { YELLOW };
                            ui.label(mono(format!("{}/{}", d.ready, d.replicas)).color(c));
                        });
                        row.col(|ui| { ui.label(mono_muted(d.updated.to_string())); });
                        row.col(|ui| { ui.label(mono_muted(d.available.to_string())); });
                        row.col(|ui| { ui.label(mono_muted(d.age())); });
                        if show_ns {
                            row.col(|ui| { ui.label(mono_muted(&d.namespace)); });
                        }
                        if row.response().clicked() {
                            actions.push(Action::Select(Selected {
                                kind: Kind::Deployment,
                                namespace: d.namespace.clone(),
                                name: d.name.clone(),
                            }));
                        }
                    });
                });
        });
        if let Some(col) = clicked {
            self.toggle_sort(Tab::Deployments, col);
        }
    }

    /// Daemon sets, stateful sets, replica sets, replication controllers, jobs, cron jobs.
    pub(super) fn ui_workloads(&mut self, ui: &mut egui::Ui, kind: Kind, actions: &mut Vec<Action>) {
        let mut create_kind = None;
        self.ui_filter_bar(ui, kind.plural(), &mut create_kind);
        if let Some(kind) = create_kind {
            actions.push(Action::Create(kind));
        }
        let state = self.workload(kind);
        if !self.list_preamble(ui, state) {
            return;
        }
        let show_ns = self.selected_ns.len() != 1;
        let mut rows: Vec<&WorkRow> = state.items.iter().filter(|w| self.matches(&w.name, &w.namespace)).collect();
        let columns = kube_ops::columns(kind);
        let sort = self.sorts.get(&Tab::Workload(kind)).cloned();
        let sort = sort.as_ref();
        let mut clicked = None;
        sort_rows(&mut rows, sort, |w, col| match col {
            "Age" => SortKey::age(w.created.as_ref()),
            "Namespace" => SortKey::text(&w.namespace),
            "Name" => SortKey::text(&w.name),
            col => match columns.iter().position(|(t, _)| *t == col).and_then(|i| w.cells.get(i)) {
                Some(cell) => match cell.seconds() {
                    Some(secs) => SortKey::Num(secs),
                    None => SortKey::text(&cell.text()),
                },
                None => SortKey::text(&w.name),
            },
        });
        // The column that carries the state gets the state colour when unhealthy.
        let state_col = columns.iter().position(|(t, _)| matches!(*t, "Ready" | "Status"));

        // Wide tables scroll sideways instead of clipping columns off the edge.
        egui::ScrollArea::horizontal().auto_shrink(false).show(ui, |ui| {
            let mut table = Self::table(ui).column(Column::remainder().at_least(180.0).clip(true));
            for (_, width) in columns {
                // Room for the header's sort arrow.
                table = table.column(Column::exact(*width + 14.0).clip(true));
            }
            table = table.column(Column::exact(72.0));
            if show_ns {
                table = table.column(Column::exact(110.0).clip(true));
            }
            table
                .header(26.0, |mut h| {
                    h.col(|ui| sort_header(ui, "Name", sort, &mut clicked));
                    for (title, _) in columns {
                        h.col(|ui| sort_header(ui, *title, sort, &mut clicked));
                    }
                    h.col(|ui| sort_header(ui, "Age", sort, &mut clicked));
                    if show_ns {
                        h.col(|ui| sort_header(ui, "Namespace", sort, &mut clicked));
                    }
                })
                .body(|body| {
                    body.rows(ROW_HEIGHT, rows.len(), |mut row| {
                        let w = rows[row.index()];
                        row.set_selected(self.is_selected(kind, &w.namespace, &w.name));
                        row.col(|ui| name_cell(ui, &w.name));
                        for (i, cell) in w.cells.iter().enumerate() {
                            row.col(|ui| {
                                let text = cell.text();
                                if Some(i) == state_col {
                                    let color = health_color(w.health);
                                    if columns[i].0 == "Status" {
                                        dot(ui, color);
                                    }
                                    let c = if w.health == Health::Ok { theme::TEXT } else { color };
                                    ui.label(mono(text).color(c));
                                } else if i == 0 {
                                    ui.label(mono(text));
                                } else {
                                    ui.label(mono_muted(text));
                                }
                            });
                        }
                        row.col(|ui| { ui.label(mono_muted(w.age())); });
                        if show_ns {
                            row.col(|ui| { ui.label(mono_muted(&w.namespace)); });
                        }
                        if row.response().clicked() {
                            actions.push(Action::Select(Selected {
                                kind,
                                namespace: w.namespace.clone(),
                                name: w.name.clone(),
                            }));
                        }
                    });
                });
        });
        if let Some(col) = clicked {
            self.toggle_sort(Tab::Workload(kind), col);
        }
    }

    /// Header click: ascending, then descending, then back to the default order.
    fn toggle_sort(&mut self, tab: Tab, col: String) {
        match self.sorts.get(&tab) {
            Some(s) if s.col == col && s.asc => {
                self.sorts.insert(tab, Sort { col, asc: false });
            }
            Some(s) if s.col == col => {
                self.sorts.remove(&tab);
            }
            _ => {
                self.sorts.insert(tab, Sort { col, asc: true });
            }
        }
    }

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
                self.pods.items.iter().for_each(|p| add(pod_health(&p.status)));
            }
            Kind::Deployment => {
                if !self.deployments.loaded {
                    return None;
                }
                self.deployments.items.iter().for_each(|d| add(deploy_health(d)));
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
    pub(super) fn ui_overview(&mut self, ui: &mut egui::Ui, actions: &mut Vec<Action>) {
        ui.horizontal(|ui| {
            ui.label(theme::title("Overview"));
            ui.label(RichText::new(namespaces_label(&self.selected_ns, false)).size(15.0).color(theme::TEXT_FAINT));
        });
        ui.add_space(12.0);
        if !self.conn_preamble(ui) {
            return;
        }

        // Cards share the width: four across when there's room, fewer on narrow windows.
        const MIN_CARD_W: f32 = 232.0;
        const CARD_H: f32 = 92.0;
        let gap = 12.0;
        let avail = ui.available_width();
        let per_row = (((avail + gap) / (MIN_CARD_W + gap)).floor() as usize).clamp(1, 4);
        let card_w = (avail - gap * (per_row - 1) as f32) / per_row as f32;
        let kinds = Tab::Overview.kinds();
        egui::Grid::new("overview-cards").spacing([gap, gap]).show(ui, |ui| {
            for (i, kind) in kinds.iter().copied().enumerate() {
                self.overview_card(ui, kind, egui::vec2(card_w, CARD_H), actions);
                if (i + 1) % per_row == 0 {
                    ui.end_row();
                }
            }
        });

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
                problems.push((Kind::Deployment, &d.namespace, &d.name, format!("Ready {}/{}", d.ready, d.replicas), Health::Progress));
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
        let sort = self.sorts.get(&Tab::Overview).cloned();
        let sort = sort.as_ref();
        let mut clicked = None;
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

        ui.add_space(22.0);
        ui.horizontal(|ui| {
            ui.label(theme::semibold("Needs attention").size(15.0).color(theme::TEXT));
            if !problems.is_empty() {
                ui.label(RichText::new(problems.len().to_string()).size(15.0).color(theme::TEXT_FAINT));
            }
        });
        ui.add_space(8.0);
        let all_loaded = kinds.iter().all(|k| self.health_counts(*k).is_some());
        if problems.is_empty() {
            ui.label(RichText::new(if all_loaded { "Everything is healthy." } else { "Loading…" }).color(theme::TEXT_MUTED));
            return;
        }
        let show_ns = self.selected_ns.len() != 1;
        // Wide tables scroll sideways instead of clipping columns off the edge.
        egui::ScrollArea::horizontal().auto_shrink(false).show(ui, |ui| {
            let mut table = Self::table(ui)
                .column(Column::remainder().at_least(180.0).clip(true))
                .column(Column::exact(150.0).clip(true))
                .column(Column::exact(190.0).clip(true));
            if show_ns {
                table = table.column(Column::exact(130.0).clip(true));
            }
            table
                .header(26.0, |mut h| {
                    for t in ["Name", "Kind", "Status"] {
                        h.col(|ui| sort_header(ui, t, sort, &mut clicked));
                    }
                    if show_ns {
                        h.col(|ui| sort_header(ui, "Namespace", sort, &mut clicked));
                    }
                })
                .body(|body| {
                    body.rows(ROW_HEIGHT, problems.len(), |mut row| {
                        let (kind, ns, name, status, health) = &problems[row.index()];
                        row.col(|ui| name_cell(ui, name));
                        row.col(|ui| { ui.label(RichText::new(kind.label()).color(theme::TEXT_MUTED)); });
                        row.col(|ui| {
                            let color = health_color(*health);
                            dot(ui, color);
                            ui.add(egui::Label::new(RichText::new(status).color(color)).truncate()).on_hover_text(status);
                        });
                        if show_ns {
                            row.col(|ui| { ui.label(mono_muted(*ns)); });
                        }
                        if row.response().clicked() {
                            actions.push(Action::OpenSelected(Selected {
                                kind: *kind,
                                namespace: ns.to_string(),
                                name: name.to_string(),
                            }));
                        }
                    });
                });
        });
        if let Some(col) = clicked {
            self.toggle_sort(Tab::Overview, col);
        }
    }

    fn overview_card(&self, ui: &mut egui::Ui, kind: Kind, size: egui::Vec2, actions: &mut Vec<Action>) {
        let (rect, resp) = ui.allocate_exact_size(size, Sense::click());
        let resp = resp.on_hover_cursor(egui::CursorIcon::PointingHand);
        if !ui.is_rect_visible(rect) {
            return;
        }
        let p = ui.painter();
        let fill = if resp.hovered() { theme::FIELD } else { theme::RAISED };
        p.rect(rect, egui::CornerRadius::same(8), fill, egui::Stroke::new(1.0, theme::BORDER), egui::StrokeKind::Inside);
        let inner = rect.shrink2(egui::vec2(14.0, 12.0));
        p.text(inner.left_top(), Align2::LEFT_TOP, kind.plural(), egui::FontId::proportional(13.0), theme::TEXT_MUTED);
        p.text(inner.right_top(), Align2::RIGHT_TOP, kind_glyph(kind), egui::FontId::proportional(15.0), theme::TEXT_FAINT);

        let counts = self.health_counts(kind);
        let total = counts.map(|c| c.iter().sum::<usize>());
        let number = total.map(|t| t.to_string()).unwrap_or_else(|| "–".into());
        p.text(
            inner.left_top() + egui::vec2(0.0, 20.0),
            Align2::LEFT_TOP,
            number,
            egui::FontId::new(24.0, theme::semibold_family()),
            theme::TEXT,
        );

        // Stacked health bar.
        let bar = egui::Rect::from_min_size(egui::pos2(inner.left(), inner.bottom() - 22.0), egui::vec2(inner.width(), 5.0));
        p.rect_filled(bar, egui::CornerRadius::same(2), theme::FIELD_ACTIVE);
        let colors = [GREEN, YELLOW, RED, theme::IDLE];
        if let (Some(c), Some(t)) = (counts, total.filter(|t| *t > 0)) {
            let mut x = bar.left();
            for (n, color) in c.iter().zip(colors) {
                if *n == 0 {
                    continue;
                }
                let w = bar.width() * *n as f32 / t as f32;
                let seg = egui::Rect::from_min_size(egui::pos2(x, bar.top()), egui::vec2(w.max(2.0), bar.height()));
                p.rect_filled(seg.intersect(bar), egui::CornerRadius::ZERO, color);
                x += w;
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
        let worst = counts.map(|c| if c[2] > 0 { RED } else if c[1] > 0 { YELLOW } else { theme::TEXT_FAINT });
        let color = worst.unwrap_or(theme::TEXT_FAINT);
        // One line, ending in an ellipsis on narrow cards.
        let mut job = egui::text::LayoutJob::single_section(
            legend,
            egui::TextFormat { font_id: egui::FontId::proportional(12.0), color, ..Default::default() },
        );
        job.wrap = egui::text::TextWrapping::truncate_at_width(inner.width());
        let galley = p.layout_job(job);
        p.galley(egui::pos2(inner.left(), inner.bottom() - galley.size().y), galley, color);
        if resp.clicked() {
            actions.push(Action::Open(Tab::for_kind(kind)));
        }
    }

    // -----------------------------------------------------------------------
    // Rendering: detail drawer
    // -----------------------------------------------------------------------

    fn drawer_tabs(&self, sel: &Selected) -> Vec<(DrawerTab, String)> {
        let details = self.loaded_details();
        let n = |label: &str, count: Option<usize>| match count {
            Some(c) => format!("{label} {c}"),
            None => label.to_string(),
        };
        let events = n("Events", details.and_then(|d| d.events.as_ref().ok()).map(Vec::len));
        let mut tabs = vec![(DrawerTab::Overview, "Overview".to_string())];
        match (sel.kind, details.map(|d| &d.body)) {
            (Kind::Pod, body) => {
                let pod = match body {
                    Some(Body::Pod(p)) => Some(p),
                    _ => None,
                };
                tabs.push((DrawerTab::Containers, n("Containers", pod.map(|p| p.containers.len()))));
                tabs.push((DrawerTab::Volumes, n("Volumes", pod.map(|p| p.volumes.len()))));
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

    pub(super) fn ui_drawer(&mut self, ctx: &egui::Context, actions: &mut Vec<Action>) {
        let Some(sel) = self.selected.clone() else { return };
        let mut close = false;
        let mut new_tab = None;
        egui::SidePanel::right("drawer")
            .resizable(true)
            .default_width(460.0)
            .min_width(340.0)
            .frame(theme::raised_frame())
            .show(ctx, |ui| {
                let (glyph, kind) = (kind_glyph(sel.kind), sel.kind.label());
                ui.horizontal_top(|ui| {
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Min), |ui| {
                        if theme::icon_button(ui, icon::X, "Close (Esc)").clicked() {
                            close = true;
                        }
                        if theme::icon_button(ui, icon::COPY, "Copy name").clicked() {
                            ui.ctx().copy_text(sel.name.clone());
                        }
                        if self.details.as_ref().is_some_and(|d| d.loading) {
                            ui.spinner();
                        }
                        // The name takes the rest of the row and wraps beside the buttons.
                        ui.with_layout(egui::Layout::left_to_right(egui::Align::Min), |ui| {
                            ui.add(egui::Label::new(theme::semibold(&sel.name).size(16.5).color(theme::TEXT)).wrap());
                        });
                    });
                });
                ui.add_space(2.0);
                ui.label(RichText::new(format!("{glyph}  {kind} · {}", sel.namespace)).color(theme::TEXT_MUTED));
                ui.add_space(14.0);
                let tabs = self.drawer_tabs(&sel);
                let labels: Vec<String> = tabs.iter().map(|(_, l)| l.clone()).collect();
                let current = tabs.iter().position(|(t, _)| *t == self.drawer_tab).unwrap_or(0);
                if let Some(i) = theme::underline_tabs(ui, &labels, current) {
                    new_tab = Some(tabs[i].0);
                }

                egui::ScrollArea::vertical().auto_shrink(false).show(ui, |ui| {
                    // Let people select and copy values in the drawer.
                    ui.style_mut().interaction.selectable_labels = true;
                    match (sel.kind, self.drawer_tab) {
                        (_, DrawerTab::Events) => self.drawer_events(ui),
                        (Kind::Pod, DrawerTab::Containers) => self.drawer_pod_containers(ui, &sel, actions),
                        (Kind::Pod, DrawerTab::Volumes) => self.drawer_pod_volumes(ui),
                        (Kind::Service, DrawerTab::Endpoints) => self.drawer_service_endpoints(ui, actions),
                        (Kind::Pod, _) => self.drawer_pod(ui, &sel, actions),
                        (Kind::Service, _) => self.drawer_service(ui, &sel, actions),
                        (Kind::Deployment, _) => self.drawer_deployment(ui, &sel, actions),
                        _ => self.drawer_workload(ui, &sel, actions),
                    }
                    ui.add_space(12.0);
                });
            });
        if let Some(tab) = new_tab {
            self.drawer_tab = tab;
        }
        if close || ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
            self.close_drawer();
        }
    }

    pub(super) fn close_drawer(&mut self) {
        self.selected = None;
        self.details = None;
        self.secrets.clear();
    }

    /// Details for the current selection, if they've loaded.
    fn loaded_details(&self) -> Option<&Details> {
        match self.details.as_ref().and_then(|d| d.data.as_ref()) {
            Some(Ok(d)) => Some(d),
            _ => None,
        }
    }

    /// Like `loaded_details`, but shows a spinner or the error while they aren't available.
    fn details_or_status(&self, ui: &mut egui::Ui) -> Option<&Details> {
        match self.details.as_ref().and_then(|d| d.data.as_ref()) {
            Some(Ok(d)) => Some(d),
            Some(Err(e)) => {
                ui.colored_label(RED, e);
                None
            }
            None => {
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.weak("Loading details…");
                });
                None
            }
        }
    }

    fn drawer_pod(&self, ui: &mut egui::Ui, sel: &Selected, actions: &mut Vec<Action>) {
        let Some(p) = self.pods.items.iter().find(|p| p.namespace == sel.namespace && p.name == sel.name)
        else {
            if self.pods.loaded {
                ui.weak("This pod no longer exists.");
            } else {
                ui.spinner();
            }
            return;
        };
        let details = match self.loaded_details().map(|d| &d.body) {
            Some(Body::Pod(d)) => Some(d),
            _ => None,
        };

        ui.horizontal_wrapped(|ui| {
            dot(ui, status_color(&p.status));
            ui.colored_label(status_color(&p.status), &p.status);
            ui.separator();
            ui.label(format!("Ready {}", p.ready));
            ui.separator();
            ui.label(format!("Restarts {}", p.restarts));
            ui.separator();
            ui.label(format!("Age {}", p.age()));
        });
        ui.add_space(8.0);
        ui.horizontal_wrapped(|ui| {
            // A pod that restarted offers the previous run's logs; when it's failing that's
            // where the reason is, so it takes the one primary button.
            let restarted = p.restarts > 0;
            let failing = pod_health(&p.status) == Health::Bad;
            let logs_label = format!("{} Logs", icon::SCROLL);
            let previous_label = format!("{} Previous logs", icon::CLOCK_COUNTER_CLOCKWISE);
            let previous_hint = match p.containers.iter().find(|c| !c.last_exit.is_empty()) {
                Some(c) => format!("Logs from the run before the last restart\nLast run: {}", c.last_exit),
                None => "Logs from the run before the last restart".to_string(),
            };
            if restarted && failing {
                if ui.add(theme::primary_button(previous_label)).on_hover_text(&previous_hint).clicked() {
                    actions.push(Action::OpenLogs(p.clone(), None, true));
                }
                if ui.button(logs_label).clicked() {
                    actions.push(Action::OpenLogs(p.clone(), None, false));
                }
            } else {
                if ui.add(theme::primary_button(logs_label)).clicked() {
                    actions.push(Action::OpenLogs(p.clone(), None, false));
                }
                if restarted && ui.button(previous_label).on_hover_text(&previous_hint).clicked() {
                    actions.push(Action::OpenLogs(p.clone(), None, true));
                }
            }
            if !p.owner.is_empty()
                && ui
                    .button(format!("{} All replicas", icon::STACK))
                    .on_hover_text(format!("Logs from every pod of {}", p.owner))
                    .clicked()
            {
                actions.push(Action::OpenGroupLogs {
                    namespace: p.namespace.clone(),
                    target: LogTarget::Owner(p.owner.clone()),
                });
            }
            let running = p.status == "Running";
            let shell = egui::Button::new(format!("{} Shell", icon::TERMINAL_WINDOW));
            let regular: Vec<&String> = p.containers.iter().filter(|c| !c.init).map(|c| &c.name).collect();
            if regular.len() > 1 && running {
                ui.menu_button(format!("{} Shell", icon::TERMINAL_WINDOW), |ui| {
                    for c in &regular {
                        if ui.button(c.as_str()).clicked() {
                            actions.push(Action::Shell(p.clone(), Some((*c).clone())));
                            ui.close();
                        }
                    }
                });
            } else if ui
                .add_enabled(running, shell)
                .on_hover_text("Open a shell in the container")
                .on_disabled_hover_text("The pod isn't running")
                .clicked()
            {
                actions.push(Action::Shell(p.clone(), None));
            }
            if ui.button(format!("{} Port forward", icon::ARROWS_LEFT_RIGHT)).clicked() {
                actions.push(Action::ForwardPod(p.clone(), None));
            }
            overflow_menu(ui, |ui| resource_menu(ui, Kind::Pod, &p.namespace, &p.name, actions));
        });

        section(ui, "Details");
        let mut rows = vec![
            ("Node", p.node.as_str()),
            ("Pod IP", p.ip.as_str()),
            ("Controlled by", p.owner.as_str()),
        ];
        if let Some(d) = details {
            rows.extend([
                ("Host IP", d.host_ip.as_str()),
                ("QoS class", d.qos.as_str()),
                ("Service account", d.service_account.as_str()),
                ("Restart policy", d.restart_policy.as_str()),
                ("Priority class", d.priority_class.as_str()),
                ("Started", d.started.as_str()),
            ]);
        }
        details_grid(ui, "pod-details", &rows);

        section(ui, "Containers");
        for c in &p.containers {
            ui.horizontal(|ui| {
                let color = if c.ready || c.state == "Completed" { GREEN } else { status_color(&c.state) };
                dot(ui, color);
                ui.label(theme::semibold(&c.name));
                if c.init {
                    ui.weak("init");
                }
                ui.label(&c.state);
                ui.weak(format!("· {} restarts", c.restarts));
            });
        }

        if let Some(d) = details {
            conditions(ui, &d.conditions);
        }

        if !p.ports.is_empty() {
            section(ui, "Ports");
            for port in &p.ports {
                ui.horizontal(|ui| {
                    ui.monospace(port.to_string());
                    if ui.small_button(format!("{} Forward", icon::ARROWS_LEFT_RIGHT)).clicked() {
                        actions.push(Action::ForwardPod(p.clone(), Some(*port)));
                    }
                });
            }
        }

        if !p.labels.is_empty() {
            section(ui, "Labels");
            tags(ui, p.labels.iter().map(|(k, v)| format!("{k}={v}")));
        }
        if let Some(d) = details {
            if !d.node_selector.is_empty() {
                section(ui, "Node selector");
                tags(ui, d.node_selector.iter().cloned());
            }
            annotations(ui, "pod-annotations", &d.annotations);
        }
    }

    fn drawer_pod_containers(&self, ui: &mut egui::Ui, sel: &Selected, actions: &mut Vec<Action>) {
        let Some(details) = self.details_or_status(ui) else { return };
        let Body::Pod(pod) = &details.body else { return };
        let row = self.pods.items.iter().find(|p| p.namespace == sel.namespace && p.name == sel.name);

        for c in &pod.containers {
            let info = row.and_then(|p| p.containers.iter().find(|i| i.name == c.name));
            egui::Frame::new()
                .fill(theme::FIELD.gamma_multiply(0.55))
                .stroke(egui::Stroke::new(1.0, theme::BORDER))
                .corner_radius(theme::RADIUS)
                .inner_margin(egui::Margin::symmetric(12, 10))
                .show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.horizontal(|ui| {
                    let color = match info {
                        Some(i) if i.ready || i.state == "Completed" => GREEN,
                        Some(i) => status_color(&i.state),
                        None => theme::IDLE,
                    };
                    dot(ui, color);
                    ui.label(theme::semibold(&c.name));
                    if c.init {
                        ui.weak("init");
                    }
                    if let Some(p) = row {
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if ui.small_button(format!("{} Logs", icon::SCROLL)).clicked() {
                                actions.push(Action::OpenLogs(p.clone(), Some(c.name.clone()), false));
                            }
                            if info.is_some_and(|i| i.restarts > 0)
                                && ui
                                    .small_button(format!("{} Previous", icon::CLOCK_COUNTER_CLOCKWISE))
                                    .on_hover_text("Logs from the run before the last restart")
                                    .clicked()
                            {
                                actions.push(Action::OpenLogs(p.clone(), Some(c.name.clone()), true));
                            }
                            let can_shell = !c.init && p.status == "Running";
                            if ui.add_enabled(can_shell, egui::Button::new(format!("{} Shell", icon::TERMINAL_WINDOW)).small()).clicked() {
                                actions.push(Action::Shell(p.clone(), Some(c.name.clone())));
                            }
                        });
                    }
                });
                if let Some(i) = info {
                    ui.horizontal_wrapped(|ui| {
                        ui.label(&i.state);
                        ui.weak(format!("· {} restarts", i.restarts));
                    });
                }
                if !c.last_termination.is_empty() {
                    ui.add(egui::Label::new(
                        RichText::new(format!("Last terminated: {}", c.last_termination)).color(YELLOW),
                    ).wrap());
                }
                ui.add(egui::Label::new(RichText::new(&c.image).monospace().weak()).wrap());

                let mut rows = vec![("Pull policy", c.pull_policy.as_str())];
                if !c.working_dir.is_empty() {
                    rows.push(("Working dir", c.working_dir.as_str()));
                }
                details_grid(ui, &format!("c-{}", c.name), &rows);

                if !c.command.is_empty() || !c.args.is_empty() {
                    sub_section(ui, "Command");
                    let line = c.command.iter().chain(c.args.iter()).map(|s| shell_quote(s)).collect::<Vec<_>>().join(" ");
                    code_block(ui, &line);
                }
                if !c.ports.is_empty() {
                    sub_section(ui, "Ports");
                    tags(ui, c.ports.iter().cloned());
                }
                if !c.requests.is_empty() || !c.limits.is_empty() {
                    sub_section(ui, "Resources");
                    resources_grid(ui, &format!("res-{}", c.name), &c.requests, &c.limits);
                }

                let env_count = c.env.len() + c.env_from_secrets.len();
                egui::CollapsingHeader::new(RichText::new(format!("Environment ({env_count})")).strong())
                    .id_salt(("env", &c.name))
                    .default_open(true)
                    .show(ui, |ui| self.env_list(ui, &sel.namespace, c, actions));
                if !c.mounts.is_empty() {
                    egui::CollapsingHeader::new(RichText::new(format!("Mounts ({})", c.mounts.len())).strong())
                        .id_salt(("mounts", &c.name))
                        .show(ui, |ui| {
                            for (path, source) in &c.mounts {
                                ui.horizontal_wrapped(|ui| {
                                    ui.monospace(path);
                                    ui.weak(format!("{} {source}", icon::ARROW_LEFT));
                                });
                            }
                        });
                }
                if !c.probes.is_empty() {
                    egui::CollapsingHeader::new(RichText::new(format!("Probes ({})", c.probes.len())).strong())
                        .id_salt(("probes", &c.name))
                        .show(ui, |ui| {
                            for (kind, desc) in &c.probes {
                                ui.label(theme::semibold(kind));
                                ui.add(egui::Label::new(RichText::new(desc).monospace().small()).wrap());
                                ui.add_space(4.0);
                            }
                        });
                }
            });
            ui.add_space(6.0);
        }
    }

    fn env_list(&self, ui: &mut egui::Ui, ns: &str, c: &ContainerDetails, actions: &mut Vec<Action>) {
        if c.env.is_empty() && c.env_from_secrets.is_empty() {
            ui.weak("No environment variables");
            return;
        }
        let secret = |name: &str| self.secrets.get(&(ns.to_string(), name.to_string()));

        // One search shared by every container's list (kept in egui memory for this drawer).
        let filter_key = egui::Id::new("env-filter");
        let mut filter: String = ui.data(|d| d.get_temp(filter_key)).unwrap_or_default();
        let resp = ui.push_id(("env-filter", &c.name), |ui| theme::search_field(ui, &mut filter, "Search env", 220.0)).inner;
        if resp.changed() {
            ui.data_mut(|d| d.insert_temp(filter_key, filter.clone()));
        }
        // Esc clears the search; consumed so it doesn't also close the drawer.
        if resp.has_focus() && !filter.is_empty() && ui.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Escape)) {
            filter.clear();
            ui.data_mut(|d| d.insert_temp(filter_key, String::new()));
        }
        ui.add_space(4.0);
        let needle = filter.trim().to_lowercase();
        let mut shown = 0usize;

        // Everything with a known value, for "Copy as .env".
        let mut dotenv = Vec::new();

        for name in &c.env_from_secrets {
            ui.horizontal_wrapped(|ui| {
                ui.weak(format!("All keys from Secret {name}"));
                self.secret_toggle(ui, ns, name, actions);
            });
            if let Some(Ok(values)) = secret(name) {
                let source = format!("Secret {name}");
                for (k, v) in values {
                    if !env_matches(&needle, k, Some(v), Some(&source)) {
                        continue;
                    }
                    shown += 1;
                    env_item(ui, k, Some(v), Some(&source));
                    dotenv.push(format!("{k}={v}"));
                }
            }
            ui.add_space(4.0);
        }

        for e in &c.env {
            let (value, source, secret_name) = match &e.source {
                EnvSource::Literal => (e.value.clone(), None, None),
                EnvSource::ConfigMap { name, key } => (
                    Some(e.value.clone().unwrap_or_else(|| "(ConfigMap not readable)".into())),
                    Some(format!("ConfigMap {name} · {key}")),
                    None,
                ),
                EnvSource::Secret { name, key } => {
                    let value = match secret(name) {
                        Some(Ok(values)) => Some(values.get(key).cloned().unwrap_or_else(|| "(missing key)".into())),
                        _ => None,
                    };
                    (value, Some(format!("Secret {name} · {key}")), Some(name.as_str()))
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
            ui.horizontal_wrapped(|ui| {
                ui.label(RichText::new(&e.name).monospace().strong());
                if let Some(s) = &source {
                    ui.weak(RichText::new(s).small());
                }
                if let Some(name) = secret_name {
                    self.secret_toggle(ui, ns, name, actions);
                }
            });
            match (&value, secret_name) {
                (Some(v), _) => value_label_in(ui, v, theme::SYNTAX_STRING),
                (None, Some(_)) => {
                    ui.weak("••••••••");
                }
                (None, None) => {
                    ui.weak("set at runtime");
                }
            }
            ui.add_space(4.0);
        }

        if !needle.is_empty() && shown == 0 {
            let hidden = c.env_from_secrets.iter().any(|n| !matches!(secret(n), Some(Ok(_))));
            ui.weak(if hidden { "No variables match (reveal secrets to search their keys)" } else { "No variables match" });
            ui.add_space(4.0);
        }
        let copy_label = if needle.is_empty() { "Copy as .env" } else { "Copy matches as .env" };
        if ui.small_button(format!("{} {copy_label}", icon::COPY)).clicked() {
            ui.ctx().copy_text(dotenv.join("\n"));
        }
    }

    fn secret_toggle(&self, ui: &mut egui::Ui, ns: &str, name: &str, actions: &mut Vec<Action>) {
        let key = (ns.to_string(), name.to_string());
        if self.secrets_loading.contains(&key) {
            ui.spinner();
            return;
        }
        match self.secrets.get(&key) {
            Some(Ok(_)) => {
                if ui.small_button(format!("{} Hide", icon::EYE_SLASH)).clicked() {
                    actions.push(Action::HideSecret(key.0, key.1));
                }
            }
            Some(Err(e)) => {
                ui.colored_label(RED, RichText::new(format!("{} not readable", icon::WARNING_CIRCLE)).small())
                    .on_hover_text(e);
            }
            None => {
                if ui.small_button(format!("{} Reveal", icon::EYE)).on_hover_text("Read this secret").clicked() {
                    actions.push(Action::RevealSecret(key.0, key.1));
                }
            }
        }
    }

    fn drawer_pod_volumes(&self, ui: &mut egui::Ui) {
        let Some(details) = self.details_or_status(ui) else { return };
        let Body::Pod(pod) = &details.body else { return };
        if pod.volumes.is_empty() {
            ui.weak("No volumes");
            return;
        }
        for (name, desc) in &pod.volumes {
            let mounted_by: Vec<String> = pod
                .containers
                .iter()
                .flat_map(|c| {
                    c.mounts
                        .iter()
                        .filter(|(_, src)| src == name || src.starts_with(&format!("{name}/")) || src.starts_with(&format!("{name} ")))
                        .map(move |(path, _)| format!("{}:{path}", c.name))
                })
                .collect();
            ui.horizontal_wrapped(|ui| {
                ui.label(theme::semibold(name));
                ui.weak(desc);
            });
            for m in mounted_by {
                ui.label(RichText::new(m).monospace().small());
            }
            ui.add_space(6.0);
        }
    }

    fn drawer_events(&self, ui: &mut egui::Ui) {
        let Some(details) = self.details_or_status(ui) else { return };
        let events = match &details.events {
            Ok(e) => e,
            Err(e) => {
                ui.weak(format!("Events unavailable: {e}"));
                return;
            }
        };
        if events.is_empty() {
            ui.weak("No recent events. Kubernetes keeps events for about an hour.");
            return;
        }
        for e in events {
            ui.horizontal(|ui| {
                let (glyph, color) = if e.warning {
                    (icon::WARNING, YELLOW)
                } else {
                    (icon::INFO, ui.visuals().weak_text_color())
                };
                ui.colored_label(color, glyph);
                ui.label(theme::semibold(&e.reason));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let count = if e.count > 1 { format!(" ×{}", e.count) } else { String::new() };
                    ui.weak(format!("{} ago{count}", e.age));
                });
            });
            ui.add(egui::Label::new(&e.message).wrap());
            ui.separator();
        }
    }

    fn drawer_service(&self, ui: &mut egui::Ui, sel: &Selected, actions: &mut Vec<Action>) {
        let Some(s) = self.services.items.iter().find(|s| s.namespace == sel.namespace && s.name == sel.name)
        else {
            ui.weak("This service no longer exists.");
            return;
        };
        let details = match self.loaded_details().map(|d| &d.body) {
            Some(Body::Service(d)) => Some(d),
            _ => None,
        };
        let can_forward = !s.selector.is_empty();

        ui.horizontal_wrapped(|ui| {
            let forward = egui::Button::new(format!("{} Port forward", icon::ARROWS_LEFT_RIGHT));
            if ui
                .add_enabled(can_forward && !s.ports.is_empty(), forward)
                .on_disabled_hover_text("Service has no selector or ports")
                .clicked()
            {
                actions.push(Action::ForwardService(s.clone(), None));
            }
            let logs = egui::Button::new(format!("{} Logs (all pods)", icon::STACK));
            if ui.add_enabled(can_forward, logs).on_disabled_hover_text("Service has no selector").clicked() {
                actions.push(Action::OpenGroupLogs {
                    namespace: s.namespace.clone(),
                    target: LogTarget::Selector { label: format!("svc/{}", s.name), selector: s.selector.clone() },
                });
            }
            overflow_menu(ui, |ui| resource_menu(ui, Kind::Service, &s.namespace, &s.name, actions));
        });

        section(ui, "Details");
        let age = s.age();
        let mut rows = vec![
            ("Type", s.kind.as_str()),
            ("Cluster IP", s.cluster_ip.as_str()),
            ("External", s.external.as_str()),
            ("Age", age.as_str()),
        ];
        if let Some(d) = details {
            rows.extend([
                ("Session affinity", d.session_affinity.as_str()),
                ("External traffic", d.external_traffic_policy.as_str()),
                ("Internal traffic", d.internal_traffic_policy.as_str()),
                ("IP families", d.ip_families.as_str()),
            ]);
        }
        details_grid(ui, "svc-details", &rows);

        if let Some(d) = details {
            let ready = d.endpoints.iter().filter(|e| e.ready).count();
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                dot(ui, if ready > 0 { GREEN } else { RED });
                ui.label(format!("{ready} of {} endpoints ready", d.endpoints.len()));
            });
        }

        section(ui, "Ports");
        if s.ports.is_empty() {
            ui.weak("No ports");
        }
        for port in &s.ports {
            ui.horizontal_wrapped(|ui| {
                let name = if port.name.is_empty() { String::new() } else { format!("{} ", port.name) };
                ui.monospace(format!(
                    "{name}{} {} {}/{}",
                    port.port,
                    icon::ARROW_RIGHT,
                    port.target,
                    port.protocol
                ));
                if let Some(np) = port.node_port {
                    ui.weak(format!("node {np}"));
                }
                let forward = egui::Button::new(format!("{} Forward", icon::ARROWS_LEFT_RIGHT)).small();
                if ui.add_enabled(can_forward, forward).clicked() {
                    actions.push(Action::ForwardService(s.clone(), Some(port.port)));
                }
            });
        }

        section(ui, "Selector");
        if s.selector.is_empty() {
            ui.weak("None");
        } else {
            tags(ui, s.selector.split(',').map(str::to_string));
        }
        if let Some(d) = details {
            if !d.labels.is_empty() {
                section(ui, "Labels");
                tags(ui, d.labels.iter().map(|(k, v)| format!("{k}={v}")));
            }
            annotations(ui, "svc-annotations", &d.annotations);
        }
    }

    fn drawer_service_endpoints(&self, ui: &mut egui::Ui, actions: &mut Vec<Action>) {
        let Some(details) = self.details_or_status(ui) else { return };
        let Body::Service(svc) = &details.body else { return };
        if !svc.endpoint_ports.is_empty() {
            sub_section(ui, "Ports");
            tags(ui, svc.endpoint_ports.iter().cloned());
            ui.add_space(6.0);
        }
        if svc.endpoints.is_empty() {
            ui.weak("No endpoints. Check that the selector matches ready pods.");
            return;
        }
        let ns = self.selected.as_ref().map(|s| s.namespace.clone()).unwrap_or_default();
        for e in &svc.endpoints {
            ui.horizontal_wrapped(|ui| {
                dot(ui, if e.ready { GREEN } else { RED });
                ui.monospace(&e.ip);
                match e.target.strip_prefix("pod/") {
                    Some(pod) => {
                        if ui.link(&e.target).on_hover_text("Open pod").clicked() {
                            actions.push(Action::GoToPod(ns.clone(), pod.to_string()));
                        }
                    }
                    None => {
                        ui.label(&e.target);
                    }
                }
                if !e.ready {
                    ui.colored_label(RED, "not ready");
                }
            });
            if !e.node.is_empty() {
                ui.weak(RichText::new(format!("    on {}", e.node)).small());
            }
        }
    }

    fn drawer_deployment(&self, ui: &mut egui::Ui, sel: &Selected, actions: &mut Vec<Action>) {
        let Some(d) = self.deployments.items.iter().find(|d| d.namespace == sel.namespace && d.name == sel.name)
        else {
            ui.weak("This deployment no longer exists.");
            return;
        };
        let details = match self.loaded_details().map(|d| &d.body) {
            Some(Body::Deployment(d)) => Some(d),
            _ => None,
        };

        ui.horizontal_wrapped(|ui| {
            let color = if d.ready >= d.replicas { GREEN } else { YELLOW };
            dot(ui, color);
            ui.colored_label(color, format!("Ready {}/{}", d.ready, d.replicas));
            ui.separator();
            ui.label(format!("Up-to-date {}", d.updated));
            ui.separator();
            ui.label(format!("Available {}", d.available));
            ui.separator();
            ui.label(format!("Age {}", d.age()));
        });
        ui.add_space(8.0);
        if !d.selector.is_empty() && ui.button(format!("{} Logs (all pods)", icon::STACK)).clicked() {
            actions.push(Action::OpenGroupLogs {
                namespace: d.namespace.clone(),
                target: LogTarget::Selector { label: format!("deploy/{}", d.name), selector: d.selector.clone() },
            });
        }
        ui.horizontal(|ui| {
            ui.label("Replicas");
            if ui.add_enabled(d.replicas > 0, egui::Button::new(icon::MINUS)).on_hover_text("Scale down").clicked() {
                actions.push(Action::Scale(Kind::Deployment, d.namespace.clone(), d.name.clone(), d.replicas - 1));
            }
            ui.label(theme::semibold(d.replicas.to_string()));
            if ui.button(icon::PLUS).on_hover_text("Scale up").clicked() {
                actions.push(Action::Scale(Kind::Deployment, d.namespace.clone(), d.name.clone(), d.replicas + 1));
            }
            ui.separator();
            overflow_menu(ui, |ui| {
                if ui.button(format!("{} Rollout restart", icon::ARROW_COUNTER_CLOCKWISE)).clicked() {
                    actions.push(Action::Restart(Kind::Deployment, d.namespace.clone(), d.name.clone()));
                }
                // Paused state comes with the details; offer the toggle once it's known.
                if let Some(paused) = details.map(|det| det.paused) {
                    let (glyph, text) = if paused { (icon::PLAY, "Resume rollout") } else { (icon::PAUSE, "Pause rollout") };
                    if ui.button(format!("{glyph} {text}")).clicked() {
                        actions.push(Action::Pause(d.namespace.clone(), d.name.clone(), !paused));
                    }
                }
                ui.separator();
                resource_menu(ui, Kind::Deployment, &d.namespace, &d.name, actions);
            });
        });

        section(ui, "Details");
        let min_ready = details.map(|d| d.min_ready_seconds.to_string()).unwrap_or_default();
        let history = details.map(|d| d.revision_history.to_string()).unwrap_or_default();
        let mut rows = vec![("Strategy", d.strategy.as_str())];
        if let Some(det) = details {
            rows.extend([
                ("Paused", if det.paused { "Yes" } else { "No" }),
                ("Min ready", min_ready.as_str()),
                ("Revision history", history.as_str()),
            ]);
        }
        details_grid(ui, "deploy-details", &rows);

        if let Some(det) = details {
            conditions(ui, &det.conditions);
        }

        section(ui, "Images");
        for image in &d.images {
            ui.add(egui::Label::new(RichText::new(image).monospace()).wrap());
        }

        if !d.selector.is_empty() {
            section(ui, "Selector");
            tags(ui, d.selector.split(',').map(str::to_string));
        }
        if let Some(det) = details {
            if !det.labels.is_empty() {
                section(ui, "Labels");
                tags(ui, det.labels.iter().map(|(k, v)| format!("{k}={v}")));
            }
            annotations(ui, "deploy-annotations", &det.annotations);
        }
    }
    fn drawer_workload(&self, ui: &mut egui::Ui, sel: &Selected, actions: &mut Vec<Action>) {
        let Some(w) = self.workload(sel.kind).items.iter().find(|w| w.namespace == sel.namespace && w.name == sel.name)
        else {
            ui.weak(format!("This {} no longer exists.", sel.kind.label()));
            return;
        };
        let details = match self.loaded_details().map(|d| &d.body) {
            Some(Body::Workload(d)) => Some(d),
            _ => None,
        };

        ui.horizontal_wrapped(|ui| {
            let color = health_color(w.health);
            dot(ui, color);
            ui.colored_label(color, &w.summary);
            ui.separator();
            ui.label(format!("Age {}", w.age()));
        });
        ui.add_space(8.0);
        ui.horizontal(|ui| {
            // Separators only between groups that are actually shown.
            let mut first = true;
            if w.kind == Kind::CronJob {
                first = false;
                if ui.add(theme::primary_button(format!("{} Run now", icon::PLAY))).on_hover_text("Start a job from this cron job's template").clicked() {
                    actions.push(Action::Trigger(w.namespace.clone(), w.name.clone()));
                }
            }
            if let Some(suspended) = w.suspended.filter(|_| !w.finished) {
                first = false;
                let (glyph, text) = if suspended { (icon::PLAY_CIRCLE, "Resume") } else { (icon::PAUSE, "Suspend") };
                let hint = match (w.kind, suspended) {
                    (Kind::CronJob, false) => "Stop scheduling new runs",
                    (Kind::CronJob, true) => "Start scheduling runs again",
                    (_, false) => "Stop the job's pods until resumed",
                    (_, true) => "Let the job run again",
                };
                if ui.button(format!("{glyph} {text}")).on_hover_text(hint).clicked() {
                    actions.push(Action::Suspend(w.kind, w.namespace.clone(), w.name.clone(), !suspended));
                }
            }
            if !w.selector.is_empty() && w.kind != Kind::CronJob {
                if !first {
                    ui.separator();
                }
                first = false;
            }
            if !w.selector.is_empty() && ui.button(format!("{} Logs (all pods)", icon::STACK)).clicked() {
                actions.push(Action::OpenGroupLogs {
                    namespace: w.namespace.clone(),
                    target: LogTarget::Selector {
                        label: format!("{}/{}", w.kind.label().to_lowercase(), w.name),
                        selector: w.selector.clone(),
                    },
                });
            }
            if let Some(replicas) = w.replicas.filter(|_| w.kind.scalable()) {
                if !first {
                    ui.separator();
                }
                first = false;
                ui.label("Replicas");
                if ui.add_enabled(replicas > 0, egui::Button::new(icon::MINUS)).on_hover_text("Scale down").clicked() {
                    actions.push(Action::Scale(w.kind, w.namespace.clone(), w.name.clone(), replicas - 1));
                }
                ui.label(theme::semibold(replicas.to_string()));
                if ui.button(icon::PLUS).on_hover_text("Scale up").clicked() {
                    actions.push(Action::Scale(w.kind, w.namespace.clone(), w.name.clone(), replicas + 1));
                }
            }
            if !first {
                ui.separator();
            }
            overflow_menu(ui, |ui| {
                if w.kind.restartable() {
                    if ui.button(format!("{} Rollout restart", icon::ARROW_COUNTER_CLOCKWISE)).clicked() {
                        actions.push(Action::Restart(w.kind, w.namespace.clone(), w.name.clone()));
                    }
                    ui.separator();
                }
                resource_menu(ui, w.kind, &w.namespace, &w.name, actions);
            });
        });

        section(ui, "Details");
        let columns = kube_ops::columns(w.kind);
        let cells: Vec<(&str, String)> = columns.iter().map(|(t, _)| *t).zip(w.cells.iter().map(|c| c.text())).collect();
        let mut rows: Vec<(&str, &str)> = cells.iter().map(|(k, v)| (*k, v.as_str())).collect();
        rows.extend(w.info.iter().map(|(k, v)| (*k, v.as_str())));
        if !w.owner.is_empty() {
            rows.push(("Controlled by", w.owner.as_str()));
        }
        details_grid(ui, "workload-details", &rows);

        if let Some(det) = details {
            conditions(ui, &det.conditions);
        }
        if !w.images.is_empty() {
            section(ui, "Images");
            for image in &w.images {
                ui.add(egui::Label::new(RichText::new(image).monospace()).wrap());
            }
        }
        if !w.selector.is_empty() {
            section(ui, "Selector");
            tags(ui, w.selector.split(',').map(str::to_string));
        }
        if let Some(det) = details {
            if !det.labels.is_empty() {
                section(ui, "Labels");
                tags(ui, det.labels.iter().map(|(k, v)| format!("{k}={v}")));
            }
            annotations(ui, "workload-annotations", &det.annotations);
        }
    }
}

/// Edit YAML, View YAML and Delete: the end of every drawer's ⋯ menu.
fn resource_menu(ui: &mut egui::Ui, kind: Kind, ns: &str, name: &str, actions: &mut Vec<Action>) {
    if ui.button(format!("{} Edit YAML", icon::PENCIL_SIMPLE)).clicked() {
        actions.push(Action::EditYaml(kind, ns.to_string(), name.to_string()));
    }
    if ui.button(format!("{} View YAML", icon::FILE_CODE)).clicked() {
        actions.push(Action::Yaml(kind, ns.to_string(), name.to_string()));
    }
    ui.separator();
    let label = format!("{} Delete {}", icon::TRASH, kind.label().to_lowercase());
    if ui.button(RichText::new(label).color(RED)).clicked() {
        actions.push(Action::AskDelete(kind, ns.to_string(), name.to_string()));
    }
}

fn empty_workloads() -> HashMap<Kind, ListState<WorkRow>> {
    WORKLOAD_KINDS.into_iter().map(|k| (k, ListState::default())).collect()
}

fn kind_glyph(kind: Kind) -> &'static str {
    match kind {
        Kind::Pod => icon::CUBE,
        Kind::Service => icon::SHARE_NETWORK,
        Kind::Deployment => icon::ROCKET_LAUNCH,
        Kind::DaemonSet => icon::HARD_DRIVES,
        Kind::StatefulSet => icon::DATABASE,
        Kind::ReplicaSet | Kind::ReplicationController => icon::COPY_SIMPLE,
        Kind::Job => icon::PLAY,
        Kind::CronJob => icon::CLOCK,
    }
}

/// Sort order on the Overview, matching the sidebar.
fn kind_order(kind: Kind) -> usize {
    Tab::Overview.kinds().iter().position(|k| *k == kind).unwrap_or(usize::MAX)
}

fn pod_health(status: &str) -> Health {
    match status {
        "Succeeded" | "Completed" => Health::Idle,
        s => match status_color(s) {
            c if c == GREEN => Health::Ok,
            c if c == YELLOW => Health::Progress,
            _ => Health::Bad,
        },
    }
}

fn deploy_health(d: &DeployRow) -> Health {
    if d.replicas == 0 {
        Health::Idle
    } else if d.ready >= d.replicas {
        Health::Ok
    } else {
        Health::Progress
    }
}

/// Legend words for [ok, progress, bad, idle] on an Overview card.
fn health_words(kind: Kind) -> [&'static str; 4] {
    match kind {
        Kind::Pod => ["running", "pending", "failing", "completed"],
        Kind::Job => ["complete", "running", "failed", "suspended"],
        Kind::CronJob => ["scheduled", "active", "failed", "suspended"],
        _ => ["ready", "updating", "failed", "scaled to 0"],
    }
}

/// Turns a watch snapshot into a list result: errors become a warning unless nothing listed.
fn snapshot<T>(rows: Vec<T>, errors: Vec<String>) -> (Result<Vec<T>, String>, Option<String>) {
    let warning = (!errors.is_empty()).then(|| errors.join("\n"));
    if rows.is_empty() {
        if let Some(w) = warning {
            return (Err(w), None);
        }
    }
    (Ok(rows), warning)
}

/// Lists across the selected namespaces. "All" or a single namespace is one call; several
/// namespaces are fetched in parallel, one call each, since a cluster-wide list is often
/// forbidden. Returns the merged items plus a warning naming namespaces that failed.
async fn list_in<T, F, Fut>(namespaces: &[String], list: F) -> (Result<Vec<T>, String>, Option<String>)
where
    F: Fn(Option<String>) -> Fut,
    Fut: std::future::Future<Output = anyhow::Result<Vec<T>>>,
{
    if namespaces.len() <= 1 {
        return (list(namespaces.first().cloned()).await.map_err(err_string), None);
    }
    let results = futures::future::join_all(namespaces.iter().map(|ns| list(Some(ns.clone())))).await;
    let mut items = Vec::new();
    let mut errors = Vec::new();
    // `namespaces` is sorted and each list is sorted by name, so the merge stays ordered.
    for (ns, result) in namespaces.iter().zip(results) {
        match result {
            Ok(v) => items.extend(v),
            Err(e) => errors.push(format!("{ns}: {}", err_string(e))),
        }
    }
    let warning = (!errors.is_empty()).then(|| errors.join("\n"));
    if items.is_empty() {
        if let Some(w) = warning {
            return (Err(w), None);
        }
    }
    (Ok(items), warning)
}

/// A list error as a tinted box, one row per message (one per failed namespace).
fn error_callout(ui: &mut egui::Ui, error: &str) {
    egui::Frame::new()
        .fill(theme::BAD.gamma_multiply(0.10))
        .stroke(egui::Stroke::new(1.0, theme::BAD.gamma_multiply(0.45)))
        .corner_radius(theme::RADIUS)
        .inner_margin(egui::Margin::symmetric(12, 10))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            for line in error.lines().filter(|l| !l.trim().is_empty()) {
                ui.horizontal_wrapped(|ui| {
                    ui.label(RichText::new(icon::WARNING_CIRCLE).color(theme::BAD));
                    ui.label(RichText::new(line).color(theme::TEXT));
                });
            }
        });
    ui.add_space(8.0);
}
