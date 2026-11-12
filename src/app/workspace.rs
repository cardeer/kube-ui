//! One cluster tab: a kubeconfig + context + namespace with its own lists and drawer.

mod view;

use super::*;

pub(super) use view::ViewCtx;

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
    /// Compiled filter for the current page; refreshed by `update_matcher`.
    matcher: super::filter::Matcher,
    /// The current list's scroll offset.
    pub(super) list_scroll: f32,
    /// Sorted column per page; saved with the tab.
    pub(super) sorts: HashMap<Tab, Sort>,
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
            list_scroll: 0.0,
            sorts: HashMap::new(),
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
        let context = self
            .context
            .clone()
            .unwrap_or_else(|| file_label(&self.kubeconfig_path));
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
            sorts: self
                .sorts
                .iter()
                .map(|(t, s)| (t.key().to_string(), s.saved()))
                .collect(),
            filter_regex: self.filter_regex,
        }
    }

    // -----------------------------------------------------------------------
    // Environment switching
    // -----------------------------------------------------------------------

    pub(super) fn load_kubeconfig(
        &mut self,
        path: PathBuf,
        context: Option<String>,
        settings: &mut Settings,
    ) {
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
                    .or_else(|| {
                        settings
                            .last_context
                            .get(&key)
                            .filter(|c| info.contexts.contains(c))
                            .cloned()
                    })
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
        settings.last_context.insert(
            self.kubeconfig_path.to_string_lossy().to_string(),
            context.clone(),
        );
        settings.save();

        self.generation += 1;
        self.context = Some(context.clone());
        self.conn = Conn::Connecting;
        self.namespaces.clear();
        self.clear_lists();

        let (ws, generation, path, bus) = (
            self.id,
            self.generation,
            self.kubeconfig_path.clone(),
            self.bus.clone(),
        );
        self.rt.spawn(async move {
            let result = async {
                let (clients, default_ns) = kube_ops::connect(&path, &context).await?;
                // Listing namespaces may be forbidden; fall back to the context default.
                let namespaces = kube_ops::list_namespaces(&clients.main)
                    .await
                    .unwrap_or_default();
                Ok((clients, default_ns, namespaces))
            }
            .await
            .map_err(err_string);
            bus.send(Msg::Connected {
                ws,
                generation,
                result,
            });
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
            settings
                .last_namespace
                .insert(context.clone(), namespaces.join(","));
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
            self.list_scroll = 0.0;
            self.update_matcher();
            self.close_drawer();
            self.refresh();
        }
    }

    /// Sets the current page's name filter.
    pub(super) fn set_filter(&mut self, text: String) {
        if text.is_empty() {
            self.filters.remove(&self.tab);
        } else {
            self.filters.insert(self.tab, text);
        }
        self.update_matcher();
    }

    /// Recompiles the filter for the current page.
    pub(super) fn update_matcher(&mut self) {
        let pattern = self
            .filters
            .get(&self.tab)
            .map(String::as_str)
            .unwrap_or("");
        self.matcher.update(pattern, self.filter_regex);
    }

    fn matches(&self, name: &str, namespace: &str) -> bool {
        !self.matcher.is_active() || self.matcher.matches(name) || self.matcher.matches(namespace)
    }

    /// Header click: ascending, then descending, then back to the default order.
    pub(super) fn toggle_sort(&mut self, tab: Tab, col: String) {
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

    // -----------------------------------------------------------------------
    // Messages and actions for this tab
    // -----------------------------------------------------------------------

    pub(super) fn handle(&mut self, msg: Msg, settings: &Settings) {
        match msg {
            Msg::Connected {
                generation, result, ..
            } => {
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
                        let saved = self
                            .context
                            .as_ref()
                            .and_then(|c| settings.last_namespace.get(c))
                            .map(|s| {
                                s.split(',')
                                    .filter(|n| !n.is_empty())
                                    .map(str::to_string)
                                    .collect::<Vec<_>>()
                            });
                        let mut selection = self
                            .restore_ns
                            .take()
                            .or(saved)
                            .unwrap_or_else(|| vec![default_ns]);
                        selection.sort();
                        self.selected_ns = selection;
                        self.refresh();
                    }
                    Err(e) => self.conn = Conn::Failed(e),
                }
            }
            Msg::Listed {
                generation,
                kind,
                result,
                warning,
                ..
            } => {
                if generation != self.generation {
                    return;
                }
                fn apply<T>(
                    state: &mut ListState<T>,
                    result: Result<Vec<T>, String>,
                    warning: Option<String>,
                ) {
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
                    (_, Ok(ListData::Deployments(v))) => {
                        apply(&mut self.deployments, Ok(v), warning)
                    }
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
        self.list_scroll = 0.0;
        self.update_matcher();
        self.close_drawer();
        self.drawer_tab = DrawerTab::Overview;
        self.selected = Some(Selected {
            kind: Kind::Pod,
            namespace,
            name,
        });
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
            let result = details::read_secret(client, ns, name)
                .await
                .map_err(err_string);
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
            self.watch = Some(WatchTask {
                key: (self.generation, self.tab),
                handles,
            });
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
        let (ws, generation, namespaces, bus, live) = (
            self.id,
            self.generation,
            self.selected_ns.clone(),
            self.bus.clone(),
            self.live,
        );
        let send = move |result: Result<ListData, String>, warning: Option<String>| {
            bus.send(Msg::Listed {
                ws,
                generation,
                kind,
                result,
                warning,
            })
        };
        self.rt.spawn(async move {
            if live {
                match kind {
                    Kind::Pod => {
                        kube_ops::watch_rows(
                            client,
                            namespaces,
                            kube_ops::pod_row,
                            move |rows, errors| {
                                let (r, w) = snapshot(rows, errors);
                                send(r.map(ListData::Pods), w)
                            },
                        )
                        .await
                    }
                    Kind::Service => {
                        kube_ops::watch_rows(
                            client,
                            namespaces,
                            kube_ops::svc_row,
                            move |rows, errors| {
                                let (r, w) = snapshot(rows, errors);
                                send(r.map(ListData::Services), w)
                            },
                        )
                        .await
                    }
                    Kind::Deployment => {
                        kube_ops::watch_rows(
                            client,
                            namespaces,
                            kube_ops::deploy_row,
                            move |rows, errors| {
                                let (r, w) = snapshot(rows, errors);
                                send(r.map(ListData::Deployments), w)
                            },
                        )
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
                    let (r, w) =
                        list_in(&namespaces, |ns| kube_ops::list_pods(client.clone(), ns)).await;
                    (r.map(ListData::Pods), w)
                }
                Kind::Service => {
                    let (r, w) = list_in(&namespaces, |ns| {
                        kube_ops::list_services(client.clone(), ns)
                    })
                    .await;
                    (r.map(ListData::Services), w)
                }
                Kind::Deployment => {
                    let (r, w) = list_in(&namespaces, |ns| {
                        kube_ops::list_deployments(client.clone(), ns)
                    })
                    .await;
                    (r.map(ListData::Deployments), w)
                }
                k => {
                    let (r, w) = list_in(&namespaces, |ns| {
                        kube_ops::list_workloads(client.clone(), k, ns)
                    })
                    .await;
                    (r.map(ListData::Workloads), w)
                }
            };
            send(result, warning);
        })
    }

    /// The first row of a list page, for the dev screenshot.
    pub(super) fn first_row(&self, tab: Tab) -> Option<Selected> {
        let (kind, namespace, name) = match tab {
            Tab::Pods => self
                .pods
                .items
                .first()
                .map(|r| (Kind::Pod, &r.namespace, &r.name))?,
            Tab::Services => self
                .services
                .items
                .first()
                .map(|r| (Kind::Service, &r.namespace, &r.name))?,
            Tab::Deployments => self
                .deployments
                .items
                .first()
                .map(|r| (Kind::Deployment, &r.namespace, &r.name))?,
            Tab::Workload(k) => self
                .workload(k)
                .items
                .first()
                .map(|r| (k, &r.namespace, &r.name))?,
            _ => return None,
        };
        Some(Selected {
            kind,
            namespace: namespace.clone(),
            name: name.clone(),
        })
    }

    /// Pods in a failing state (CrashLoopBackOff, ImagePullBackOff, Failed, …).
    pub(super) fn failing_pods(&self) -> usize {
        self.pods
            .items
            .iter()
            .filter(|p| pod_health(&p.status) == Health::Bad)
            .count()
    }

    pub(super) fn workload(&self, kind: Kind) -> &ListState<WorkRow> {
        &self.workloads[&kind]
    }

    pub(super) fn is_loading(&self) -> bool {
        self.tab.kinds().into_iter().any(|k| self.kind_loading(k))
    }
    /// (Re)loads the drawer's details for the current selection.
    pub(super) fn fetch_details(&mut self) {
        let Some(sel) = self.selected.clone() else {
            return;
        };
        let Some(client) = self.client() else { return };
        let state = match &mut self.details {
            Some(d) if d.sel == sel => d,
            slot => slot.insert(DetailsState {
                sel: sel.clone(),
                data: None,
                loading: false,
            }),
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

    /// The detail drawer is showing something.
    pub(super) fn has_drawer(&self) -> bool {
        self.selected.is_some()
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
}

fn empty_workloads() -> HashMap<Kind, ListState<WorkRow>> {
    WORKLOAD_KINDS
        .into_iter()
        .map(|k| (k, ListState::default()))
        .collect()
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
    Tab::Overview
        .kinds()
        .iter()
        .position(|k| *k == kind)
        .unwrap_or(usize::MAX)
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
async fn list_in<T, F, Fut>(
    namespaces: &[String],
    list: F,
) -> (Result<Vec<T>, String>, Option<String>)
where
    F: Fn(Option<String>) -> Fut,
    Fut: std::future::Future<Output = anyhow::Result<Vec<T>>>,
{
    if namespaces.len() <= 1 {
        return (
            list(namespaces.first().cloned()).await.map_err(err_string),
            None,
        );
    }
    let results =
        futures::future::join_all(namespaces.iter().map(|ns| list(Some(ns.clone())))).await;
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
