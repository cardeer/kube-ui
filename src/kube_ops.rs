//! Everything that talks to the Kubernetes API. No UI code in here.

use std::{
    collections::BTreeMap,
    fmt::Debug,
    path::Path,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
};

use anyhow::{Context as _, Result, anyhow};
use futures::{AsyncBufReadExt, StreamExt, TryStreamExt};
use k8s_openapi::{
    NamespaceResourceScope,
    api::{
        apps::v1::{DaemonSet, Deployment, ReplicaSet, StatefulSet},
        batch::v1::{CronJob, Job},
        core::v1::{ContainerState, Namespace, Pod, ReplicationController, Service},
    },
    apimachinery::pkg::{apis::meta::v1::Time, util::intstr::IntOrString},
    jiff::Timestamp,
};
use kube::{
    Client, Config, ResourceExt,
    api::{Api, DeleteParams, ListParams, LogParams, Patch, PatchParams, PostParams},
    client::ConfigExt,
    config::{KubeConfigOptions, Kubeconfig},
    runtime::{WatchStreamExt, watcher},
};
use serde::de::DeserializeOwned;
use hyper_util::{
    client::legacy::{Client as LegacyClient, connect::HttpConnector},
    rt::TokioExecutor,
};
use tower::{BoxError, ServiceBuilder};
use serde_json::json;
use tokio::net::TcpListener;

// ---------------------------------------------------------------------------
// Kubeconfig / connection
// ---------------------------------------------------------------------------

pub struct KubeconfigInfo {
    pub contexts: Vec<String>,
    pub current: Option<String>,
}

pub fn read_kubeconfig(path: &Path) -> Result<KubeconfigInfo> {
    let kc = Kubeconfig::read_from(path)
        .with_context(|| format!("failed to read kubeconfig {}", path.display()))?;
    let mut contexts: Vec<String> = kc.contexts.iter().map(|c| c.name.clone()).collect();
    contexts.sort();
    if contexts.is_empty() {
        return Err(anyhow!("kubeconfig {} has no contexts", path.display()));
    }
    Ok(KubeconfigInfo {
        contexts,
        current: kc.current_context,
    })
}

/// Clients for one context. `logs` may negotiate HTTP/2: some API proxies (e.g. Rancher)
/// never answer `follow=true` log requests over HTTP/1.1. Everything else, port-forward
/// included, needs HTTP/1.1 upgrades and goes through `main`.
#[derive(Clone)]
pub struct Clients {
    pub main: Client,
    pub logs: Client,
}

/// Builds clients for `context`. Returns them with the context's default namespace.
pub async fn connect(path: &Path, context: &str) -> Result<(Clients, String)> {
    let kc = Kubeconfig::read_from(path)?;
    let options = KubeConfigOptions {
        context: Some(context.to_string()),
        ..Default::default()
    };
    let config = Config::from_custom_kubeconfig(kc, &options)
        .await
        .with_context(|| format!("failed to load context {context}"))?;
    let default_ns = config.default_namespace.clone();
    let logs = http2_client(&config)?;
    let main = Client::try_from(config)?;
    // Cheap call that fails fast if the cluster is unreachable or credentials are bad.
    main.apiserver_version()
        .await
        .context("cannot reach API server")?;
    Ok((Clients { main, logs }, default_ns))
}

fn http2_client(config: &Config) -> Result<Client> {
    let mut http = HttpConnector::new();
    http.enforce_http(false);
    let https = hyper_rustls::HttpsConnectorBuilder::new()
        .with_tls_config(config.rustls_client_config()?)
        .https_or_http()
        .enable_http1()
        .enable_http2()
        .wrap_connector(http);
    let service = ServiceBuilder::new()
        .layer(config.base_uri_layer())
        .option_layer(config.auth_layer()?)
        .map_err(BoxError::from)
        .service(LegacyClient::builder(TokioExecutor::new()).build(https));
    Ok(Client::new(service, config.default_namespace.clone()))
}

pub async fn list_namespaces(client: &Client) -> Result<Vec<String>> {
    let api: Api<Namespace> = Api::all(client.clone());
    let mut names: Vec<String> = api
        .list(&ListParams::default())
        .await?
        .items
        .iter()
        .map(|n| n.name_any())
        .collect();
    names.sort();
    Ok(names)
}

fn scoped<K>(client: Client, ns: Option<&str>) -> Api<K>
where
    K: kube::Resource<Scope = NamespaceResourceScope>,
    <K as kube::Resource>::DynamicType: Default,
{
    match ns {
        Some(ns) => Api::namespaced(client, ns),
        None => Api::all(client),
    }
}

// ---------------------------------------------------------------------------
// Live lists
// ---------------------------------------------------------------------------

/// Pause after a change before emitting, so a burst of events (a rollout, a big
/// relist) becomes one snapshot instead of hundreds.
const WATCH_DEBOUNCE: std::time::Duration = std::time::Duration::from_millis(150);

/// What one namespace's watch currently knows.
struct WatchScope<R> {
    label: Option<String>,
    rows: BTreeMap<(String, String), R>,
    /// Filled while a (re)list is in progress, swapped in on `InitDone`.
    buffer: Option<BTreeMap<(String, String), R>>,
    synced: bool,
    error: Option<String>,
}

/// Lists and then watches `K` in each namespace (empty = all namespaces), the way
/// Lens and `kubectl get -w` do. Calls `emit(rows, errors)` with the full sorted
/// list whenever it changes; `errors` holds one "namespace: message" per failing
/// namespace. Reconnects with backoff and relists on its own; runs until dropped.
pub async fn watch_rows<K, R>(
    client: Client,
    namespaces: Vec<String>,
    to_row: fn(&K) -> R,
    mut emit: impl FnMut(Vec<R>, Vec<String>),
) where
    K: kube::Resource<Scope = NamespaceResourceScope> + Clone + DeserializeOwned + Debug + Send + 'static,
    <K as kube::Resource>::DynamicType: Default,
    R: Clone,
{
    let labels: Vec<Option<String>> = if namespaces.is_empty() {
        vec![None]
    } else {
        namespaces.into_iter().map(Some).collect()
    };
    let streams = labels.iter().enumerate().map(|(i, ns)| {
        let api: Api<K> = scoped(client.clone(), ns.as_deref());
        watcher(api, watcher::Config::default())
            .default_backoff()
            .map(move |event| (i, event))
            .boxed()
    });
    let mut events = futures::stream::select_all(streams);
    let mut scopes: Vec<WatchScope<R>> = labels
        .into_iter()
        .map(|label| WatchScope { label, rows: BTreeMap::new(), buffer: None, synced: false, error: None })
        .collect();
    let key = |k: &K| (k.namespace().unwrap_or_default(), k.name_any());
    let mut deadline: Option<tokio::time::Instant> = None;

    loop {
        let flush = async {
            match deadline {
                Some(d) => tokio::time::sleep_until(d).await,
                None => std::future::pending().await,
            }
        };
        let (i, event) = tokio::select! {
            next = events.next() => match next {
                Some(item) => item,
                None => return,
            },
            _ = flush => {
                deadline = None;
                // Hold off until every namespace has listed (or failed) so the
                // first snapshot isn't a partial one.
                if scopes.iter().all(|s| s.synced || s.error.is_some()) {
                    let rows = scopes.iter().flat_map(|s| s.rows.values().cloned()).collect();
                    let errors = scopes
                        .iter()
                        .filter_map(|s| {
                            let e = s.error.as_ref()?;
                            Some(match &s.label {
                                Some(ns) if scopes.len() > 1 => format!("{ns}: {e}"),
                                _ => e.clone(),
                            })
                        })
                        .collect();
                    emit(rows, errors);
                }
                continue;
            }
        };
        let scope = &mut scopes[i];
        let changed = match event {
            Ok(watcher::Event::Init) => {
                scope.buffer = Some(BTreeMap::new());
                false
            }
            Ok(watcher::Event::InitApply(k)) => {
                scope.buffer.get_or_insert_default().insert(key(&k), to_row(&k));
                false
            }
            Ok(watcher::Event::InitDone) => {
                scope.rows = scope.buffer.take().unwrap_or_default();
                scope.synced = true;
                scope.error = None;
                true
            }
            Ok(watcher::Event::Apply(k)) => {
                scope.rows.insert(key(&k), to_row(&k));
                scope.error = None;
                true
            }
            Ok(watcher::Event::Delete(k)) => {
                scope.rows.remove(&key(&k));
                true
            }
            Err(e) => {
                // The backoff retries forever; only report when the message changes.
                let e = describe_error(&anyhow::Error::from(e));
                let changed = scope.error.as_ref() != Some(&e);
                scope.error = Some(e);
                changed
            }
        };
        if changed && deadline.is_none() {
            deadline = Some(tokio::time::Instant::now() + WATCH_DEBOUNCE);
        }
    }
}

/// A readable message for an error. When the API server answered with a `Status` (403, 404,
/// …) this says what went wrong in plain words instead of dumping the nested debug output.
pub fn describe_error(e: &anyhow::Error) -> String {
    let status = e.chain().find_map(|c| {
        if let Some(kube::Error::Api(s)) = c.downcast_ref::<kube::Error>() {
            return Some(&**s);
        }
        if let Some(watcher::Error::WatchError(s)) = c.downcast_ref::<watcher::Error>() {
            return Some(&**s);
        }
        c.downcast_ref::<kube::core::Status>()
    });
    match status {
        Some(s) => describe_status(s),
        None => format!("{e:#}"),
    }
}

fn describe_status(s: &kube::core::Status) -> String {
    static FORBIDDEN: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
        regex::Regex::new(
            r#"User "([^"]+)" cannot (\w+) resource "([^"]+)"(?: in API group "([^"]*)")?(?: in the namespace "([^"]+)")?"#,
        )
        .unwrap()
    });
    let message = s.message.trim();
    match s.code {
        401 => "Not authenticated: the cluster rejected this kubeconfig's credentials. They may have expired.".into(),
        403 => match FORBIDDEN.captures(message) {
            Some(c) => {
                let user = &c[1];
                let verb = &c[2];
                let mut resource = c[3].to_string();
                if let Some(group) = c.get(4).map(|g| g.as_str()).filter(|g| !g.is_empty()) {
                    resource = format!("{resource}.{group}");
                }
                let place = match c.get(5) {
                    Some(ns) => format!("in namespace \"{}\"", ns.as_str()),
                    None => "across the cluster".into(),
                };
                format!("No permission to {verb} {resource} {place}. Signed in as {user}.")
            }
            None => format!("Permission denied: {message}"),
        },
        404 => format!("Not found: {message}"),
        409 => format!("Conflict: {message}"),
        422 => format!("Invalid: {message}"),
        _ if !message.is_empty() => message.to_string(),
        code => format!("Request failed ({code} {})", s.reason),
    }
}

pub fn age(t: Option<&Time>) -> String {
    let Some(t) = t else { return "-".into() };
    span(Timestamp::now().duration_since(t.0).as_secs())
}

/// Seconds since `t`, for sorting by age.
pub fn age_secs(t: Option<&Time>) -> Option<i64> {
    t.map(|t| Timestamp::now().duration_since(t.0).as_secs())
}

/// "42s", "5m", "3h12m", "2d4h".
pub fn span(secs: i64) -> String {
    match secs.max(0) {
        s if s < 60 => format!("{s}s"),
        s if s < 3600 => format!("{}m", s / 60),
        s if s < 86400 => format!("{}h{}m", s / 3600, (s % 3600) / 60),
        s => format!("{}d{}h", s / 86400, (s % 86400) / 3600),
    }
}

// ---------------------------------------------------------------------------
// Pods
// ---------------------------------------------------------------------------

#[derive(Clone)]
pub struct ContainerInfo {
    pub name: String,
    pub ready: bool,
    pub restarts: i32,
    pub state: String,
    pub init: bool,
}

#[derive(Clone)]
pub struct PodRow {
    pub name: String,
    pub namespace: String,
    pub ready: String,
    pub status: String,
    pub restarts: i32,
    pub created: Option<Time>,
    pub node: String,
    pub ip: String,
    /// "Kind/name" of the controlling owner, if any.
    pub owner: String,
    pub labels: Vec<(String, String)>,
    /// Regular containers first, then init containers.
    pub containers: Vec<ContainerInfo>,
    pub ports: Vec<u16>,
}

impl PodRow {
    pub fn age(&self) -> String {
        age(self.created.as_ref())
    }
}

fn container_state(s: Option<&ContainerState>) -> String {
    let Some(s) = s else { return "Waiting".into() };
    if s.running.is_some() {
        "Running".into()
    } else if let Some(w) = &s.waiting {
        w.reason.clone().unwrap_or_else(|| "Waiting".into())
    } else if let Some(t) = &s.terminated {
        t.reason.clone().unwrap_or_else(|| "Terminated".into())
    } else {
        "Unknown".into()
    }
}

pub fn pod_row(p: &Pod) -> PodRow {
    let status = p.status.as_ref();
    let spec = p.spec.as_ref();
    let statuses = status
        .and_then(|s| s.container_statuses.clone())
        .unwrap_or_default();
    let init_statuses = status
        .and_then(|s| s.init_container_statuses.clone())
        .unwrap_or_default();

    let total = spec.map(|s| s.containers.len()).unwrap_or(0);
    let ready = statuses.iter().filter(|c| c.ready).count();
    let restarts = statuses.iter().map(|c| c.restart_count).sum();

    // Mirror kubectl's STATUS column: a container's waiting/terminated reason wins over the phase.
    let mut state = status
        .and_then(|s| s.reason.clone().or_else(|| s.phase.clone()))
        .unwrap_or_else(|| "Unknown".into());
    for c in &statuses {
        let Some(s) = &c.state else { continue };
        if let Some(reason) = s.waiting.as_ref().and_then(|w| w.reason.clone()) {
            state = reason;
            break;
        }
        if let Some(reason) = s.terminated.as_ref().and_then(|t| t.reason.clone()) {
            if !c.ready && state == "Running" {
                state = reason;
                break;
            }
        }
    }
    if p.metadata.deletion_timestamp.is_some() {
        state = "Terminating".into();
    }

    let mut containers = Vec::new();
    if let Some(spec) = spec {
        let no_init = Vec::new();
        let groups = [
            (&spec.containers, &statuses, false),
            (spec.init_containers.as_ref().unwrap_or(&no_init), &init_statuses, true),
        ];
        for (specs, statuses, init) in groups {
            for c in specs {
                let st = statuses.iter().find(|s| s.name == c.name);
                containers.push(ContainerInfo {
                    name: c.name.clone(),
                    ready: st.is_some_and(|s| s.ready),
                    restarts: st.map(|s| s.restart_count).unwrap_or(0),
                    state: container_state(st.and_then(|s| s.state.as_ref())),
                    init,
                });
            }
        }
    }
    let ports = spec
        .map(|s| {
            s.containers
                .iter()
                .flat_map(|c| c.ports.iter().flatten())
                .map(|p| p.container_port as u16)
                .collect()
        })
        .unwrap_or_default();

    PodRow {
        name: p.name_any(),
        namespace: p.namespace().unwrap_or_default(),
        ready: format!("{ready}/{total}"),
        status: state,
        restarts,
        created: p.metadata.creation_timestamp.clone(),
        node: spec.and_then(|s| s.node_name.clone()).unwrap_or_default(),
        ip: status.and_then(|s| s.pod_ip.clone()).unwrap_or_default(),
        owner: p
            .metadata
            .owner_references
            .iter()
            .flatten()
            .find(|o| o.controller == Some(true))
            .map(|o| format!("{}/{}", o.kind, o.name))
            .unwrap_or_default(),
        labels: p.labels().iter().map(|(k, v)| (k.clone(), v.clone())).collect(),
        containers,
        ports,
    }
}

/// One-shot list of any namespaced kind, mapped to rows and sorted by (namespace, name).
pub async fn list_rows<K, R>(client: Client, ns: Option<String>, to_row: fn(&K) -> R) -> Result<Vec<R>>
where
    K: kube::Resource<Scope = NamespaceResourceScope> + Clone + DeserializeOwned + Debug,
    <K as kube::Resource>::DynamicType: Default,
{
    let api: Api<K> = scoped(client, ns.as_deref());
    let mut items = api.list(&ListParams::default()).await?.items;
    items.sort_by_key(|k| (k.namespace().unwrap_or_default(), k.name_any()));
    Ok(items.iter().map(to_row).collect())
}

pub async fn list_pods(client: Client, ns: Option<String>) -> Result<Vec<PodRow>> {
    let api: Api<Pod> = scoped(client, ns.as_deref());
    let mut rows: Vec<PodRow> = api
        .list(&ListParams::default())
        .await?
        .items
        .iter()
        .map(pod_row)
        .collect();
    rows.sort_by(|a, b| (&a.namespace, &a.name).cmp(&(&b.namespace, &b.name)));
    Ok(rows)
}


pub struct LogOptions {
    pub container: String,
    pub tail: Option<i64>,
    pub previous: bool,
}

/// One log line, split into the RFC 3339 timestamp the API server prepends and the text.
/// Timestamps are always requested so older pages can be merged without duplicates.
#[derive(Clone)]
pub struct LogLine {
    pub ts: String,
    pub text: String,
}

impl LogLine {
    fn parse(raw: &str) -> Self {
        match raw.split_once(' ') {
            Some((ts, text)) if ts.len() >= 20 && ts.as_bytes()[4] == b'-' => Self {
                ts: ts.to_string(),
                text: text.to_string(),
            },
            _ => Self {
                ts: String::new(),
                text: raw.to_string(),
            },
        }
    }
}

fn log_params(opts: &LogOptions, follow: bool) -> LogParams {
    LogParams {
        container: Some(opts.container.clone()),
        follow,
        tail_lines: opts.tail,
        timestamps: true,
        previous: opts.previous,
        ..Default::default()
    }
}

/// Streams log lines into `on_line` until the stream ends (or forever, when following).
pub async fn stream_logs(
    client: Client,
    ns: String,
    pod: String,
    opts: LogOptions,
    mut on_line: impl FnMut(LogLine),
) -> Result<()> {
    let api: Api<Pod> = Api::namespaced(client, &ns);
    let lp = log_params(&opts, !opts.previous);
    let lines = api.log_stream(&pod, &lp).await?.lines();
    futures::pin_mut!(lines);
    while let Some(line) = lines.try_next().await? {
        on_line(LogLine::parse(&line));
    }
    Ok(())
}

/// Pods to stream logs from, as (name, container names), sorted by name. Either all pods
/// matching a label `selector`, or those controlled by `owner` ("Kind/name").
pub async fn pods_for_logs(
    client: Client,
    ns: String,
    selector: Option<String>,
    owner: Option<String>,
) -> Result<Vec<(String, Vec<String>)>> {
    let api: Api<Pod> = Api::namespaced(client, &ns);
    let lp = match &selector {
        Some(s) => ListParams::default().labels(s),
        None => ListParams::default(),
    };
    let owned_by = |p: &Pod, owner: &str| {
        p.metadata
            .owner_references
            .iter()
            .flatten()
            .any(|r| format!("{}/{}", r.kind, r.name) == owner)
    };
    let mut pods: Vec<(String, Vec<String>)> = api
        .list(&lp)
        .await?
        .items
        .into_iter()
        .filter(|p| p.metadata.deletion_timestamp.is_none())
        .filter(|p| owner.as_deref().is_none_or(|o| owned_by(p, o)))
        .map(|p| {
            let containers = p
                .spec
                .as_ref()
                .map(|s| s.containers.iter().map(|c| c.name.clone()).collect())
                .unwrap_or_default();
            (p.name_any(), containers)
        })
        .collect();
    pods.sort();
    Ok(pods)
}

/// Fetches the last `opts.tail` lines once, returning only those strictly older than `before`.
/// The API has no "until" parameter, so paging backwards means asking for a larger tail.
pub async fn fetch_older_logs(
    client: Client,
    ns: String,
    pod: String,
    opts: LogOptions,
    before: String,
) -> Result<Vec<LogLine>> {
    let api: Api<Pod> = Api::namespaced(client, &ns);
    let text = api.logs(&pod, &log_params(&opts, false)).await?;
    Ok(text
        .lines()
        .map(LogLine::parse)
        .filter(|l| l.ts.as_str() < before.as_str())
        .collect())
}

// ---------------------------------------------------------------------------
// Services
// ---------------------------------------------------------------------------

#[derive(Clone)]
pub struct SvcPort {
    pub name: String,
    pub port: u16,
    pub target: String,
    pub protocol: String,
    pub node_port: Option<i32>,
}

#[derive(Clone)]
pub struct SvcRow {
    pub name: String,
    pub namespace: String,
    pub kind: String,
    pub cluster_ip: String,
    pub external: String,
    pub ports: Vec<SvcPort>,
    pub selector: String,
    pub created: Option<Time>,
}

impl SvcRow {
    pub fn age(&self) -> String {
        age(self.created.as_ref())
    }

    pub fn ports_label(&self) -> String {
        self.ports
            .iter()
            .map(|p| match p.node_port {
                Some(np) => format!("{}:{np}/{}", p.port, p.protocol),
                None => format!("{}/{}", p.port, p.protocol),
            })
            .collect::<Vec<_>>()
            .join(", ")
    }
}

pub fn svc_row(s: &Service) -> SvcRow {
    let spec = s.spec.as_ref();
    let mut external: Vec<String> = spec
        .and_then(|s| s.external_ips.clone())
        .unwrap_or_default();
    if let Some(ingress) = s
        .status
        .as_ref()
        .and_then(|st| st.load_balancer.as_ref())
        .and_then(|lb| lb.ingress.as_ref())
    {
        external.extend(
            ingress
                .iter()
                .filter_map(|i| i.ip.clone().or_else(|| i.hostname.clone())),
        );
    }
    if let Some(name) = spec.and_then(|s| s.external_name.clone()) {
        external.push(name);
    }
    SvcRow {
        name: s.name_any(),
        namespace: s.namespace().unwrap_or_default(),
        kind: spec
            .and_then(|s| s.type_.clone())
            .unwrap_or_else(|| "ClusterIP".into()),
        cluster_ip: spec.and_then(|s| s.cluster_ip.clone()).unwrap_or_default(),
        external: if external.is_empty() {
            "-".into()
        } else {
            external.join(", ")
        },
        ports: spec
            .and_then(|s| s.ports.as_ref())
            .map(|ports| {
                ports
                    .iter()
                    .map(|p| SvcPort {
                        name: p.name.clone().unwrap_or_default(),
                        port: p.port as u16,
                        target: match &p.target_port {
                            Some(IntOrString::Int(i)) => i.to_string(),
                            Some(IntOrString::String(s)) => s.clone(),
                            None => p.port.to_string(),
                        },
                        protocol: p.protocol.clone().unwrap_or_else(|| "TCP".into()),
                        node_port: p.node_port,
                    })
                    .collect()
            })
            .unwrap_or_default(),
        selector: spec
            .and_then(|s| s.selector.as_ref())
            .map(|sel| {
                sel.iter()
                    .map(|(k, v)| format!("{k}={v}"))
                    .collect::<Vec<_>>()
                    .join(",")
            })
            .unwrap_or_default(),
        created: s.metadata.creation_timestamp.clone(),
    }
}

pub async fn list_services(client: Client, ns: Option<String>) -> Result<Vec<SvcRow>> {
    let api: Api<Service> = scoped(client, ns.as_deref());
    let mut rows: Vec<SvcRow> = api
        .list(&ListParams::default())
        .await?
        .items
        .iter()
        .map(svc_row)
        .collect();
    rows.sort_by(|a, b| (&a.namespace, &a.name).cmp(&(&b.namespace, &b.name)));
    Ok(rows)
}

// ---------------------------------------------------------------------------
// Deployments
// ---------------------------------------------------------------------------

#[derive(Clone)]
pub struct DeployRow {
    pub name: String,
    pub namespace: String,
    pub replicas: i32,
    pub ready: i32,
    pub updated: i32,
    pub available: i32,
    pub created: Option<Time>,
    pub strategy: String,
    pub selector: String,
    pub images: Vec<String>,
}

impl DeployRow {
    pub fn age(&self) -> String {
        age(self.created.as_ref())
    }
}

pub fn deploy_row(d: &Deployment) -> DeployRow {
    let st = d.status.as_ref();
    DeployRow {
        name: d.name_any(),
        namespace: d.namespace().unwrap_or_default(),
        replicas: d.spec.replicas.unwrap_or(1),
        ready: st.and_then(|s| s.ready_replicas).unwrap_or(0),
        updated: st.and_then(|s| s.updated_replicas).unwrap_or(0),
        available: st.and_then(|s| s.available_replicas).unwrap_or(0),
        created: d.metadata.creation_timestamp.clone(),
        strategy: d
            .spec
            .strategy
            .as_ref()
            .and_then(|s| s.type_.clone())
            .unwrap_or_else(|| "RollingUpdate".into()),
        selector: d
            .spec
            .selector
            .match_labels
            .iter()
            .flatten()
            .map(|(k, v)| format!("{k}={v}"))
            .collect::<Vec<_>>()
            .join(","),
        images: d
            .spec
            .template
            .spec
            .iter()
            .flat_map(|s| s.containers.iter())
            .filter_map(|c| c.image.clone())
            .collect(),
    }
}

pub async fn list_deployments(client: Client, ns: Option<String>) -> Result<Vec<DeployRow>> {
    let api: Api<Deployment> = scoped(client, ns.as_deref());
    let mut rows: Vec<DeployRow> = api
        .list(&ListParams::default())
        .await?
        .items
        .iter()
        .map(deploy_row)
        .collect();
    rows.sort_by(|a, b| (&a.namespace, &a.name).cmp(&(&b.namespace, &b.name)));
    Ok(rows)
}

async fn merge_patch<K>(client: Client, ns: &str, name: &str, patch: serde_json::Value) -> Result<()>
where
    K: kube::Resource<Scope = NamespaceResourceScope> + Clone + DeserializeOwned + Debug,
    <K as kube::Resource>::DynamicType: Default,
{
    Api::<K>::namespaced(client, ns)
        .patch(name, &PatchParams::default(), &Patch::Merge(&patch))
        .await?;
    Ok(())
}

pub async fn scale(client: Client, kind: Kind, ns: String, name: String, replicas: i32) -> Result<()> {
    let patch = json!({ "spec": { "replicas": replicas } });
    match kind {
        Kind::Deployment => merge_patch::<Deployment>(client, &ns, &name, patch).await,
        Kind::StatefulSet => merge_patch::<StatefulSet>(client, &ns, &name, patch).await,
        Kind::ReplicaSet => merge_patch::<ReplicaSet>(client, &ns, &name, patch).await,
        Kind::ReplicationController => merge_patch::<ReplicationController>(client, &ns, &name, patch).await,
        _ => Err(anyhow!("{} can't be scaled", kind.label())),
    }
}

/// Same as `kubectl rollout restart`: bump an annotation on the pod template.
pub async fn restart(client: Client, kind: Kind, ns: String, name: String) -> Result<()> {
    let patch = json!({ "spec": { "template": { "metadata": { "annotations": {
        "kubectl.kubernetes.io/restartedAt": Timestamp::now().to_string()
    }}}}});
    match kind {
        Kind::Deployment => merge_patch::<Deployment>(client, &ns, &name, patch).await,
        Kind::DaemonSet => merge_patch::<DaemonSet>(client, &ns, &name, patch).await,
        Kind::StatefulSet => merge_patch::<StatefulSet>(client, &ns, &name, patch).await,
        _ => Err(anyhow!("{} can't be restarted", kind.label())),
    }
}

// ---------------------------------------------------------------------------
// Shell (exec with a TTY)
// ---------------------------------------------------------------------------

/// Keystrokes and window changes from the terminal widget.
pub enum ShellInput {
    Data(Vec<u8>),
    /// (cols, rows)
    Resize(u16, u16),
}

/// Starts an interactive shell in a container, like `kubectl exec -it -- sh`, preferring
/// bash when the image has it. Streams output to `on_output` until the shell exits;
/// returns the exit problem, if any (`Ok(None)` is a clean exit).
pub async fn run_shell(
    client: Client,
    ns: String,
    pod: String,
    container: Option<String>,
    size: (u16, u16),
    mut input: tokio::sync::mpsc::UnboundedReceiver<ShellInput>,
    mut on_output: impl FnMut(Vec<u8>),
) -> Result<Option<String>> {
    use futures::SinkExt;
    use kube::api::{AttachParams, TerminalSize};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let api: Api<Pod> = Api::namespaced(client, &ns);
    let mut params = AttachParams::interactive_tty();
    if let Some(c) = container {
        params = params.container(c);
    }
    // TERM isn't set for exec sessions; without it bash and most tools fall back to dumb output.
    let script = "export TERM=xterm-256color; if command -v bash >/dev/null 2>&1; then exec bash; else exec sh; fi";
    let mut proc = api.exec(&pod, ["/bin/sh", "-c", script], &params).await?;
    let mut stdin = proc.stdin().ok_or_else(|| anyhow!("no stdin"))?;
    let mut stdout = proc.stdout().ok_or_else(|| anyhow!("no stdout"))?;
    let mut resize = proc.terminal_size();
    let status = proc.take_status();
    if let Some(r) = resize.as_mut() {
        let _ = r.send(TerminalSize { width: size.0, height: size.1 }).await;
    }

    let mut buf = vec![0u8; 16 * 1024];
    loop {
        tokio::select! {
            n = stdout.read(&mut buf) => match n {
                Ok(0) | Err(_) => break,
                Ok(n) => on_output(buf[..n].to_vec()),
            },
            msg = input.recv() => match msg {
                Some(ShellInput::Data(bytes)) => {
                    if stdin.write_all(&bytes).await.is_err() {
                        break;
                    }
                    let _ = stdin.flush().await;
                }
                Some(ShellInput::Resize(cols, rows)) => {
                    if let Some(r) = resize.as_mut() {
                        let _ = r.send(TerminalSize { width: cols, height: rows }).await;
                    }
                }
                // The tab was closed.
                None => return Ok(None),
            },
        }
    }
    let status = match status {
        Some(s) => s.await,
        None => None,
    };
    Ok(status.filter(|s| s.status.as_deref() == Some("Failure")).map(|s| {
        s.message.unwrap_or_else(|| "the shell exited with an error".into())
    }))
}

// ---------------------------------------------------------------------------
// Changes that work across kinds
// ---------------------------------------------------------------------------

async fn delete_one<K>(client: Client, ns: &str, name: &str) -> Result<()>
where
    K: kube::Resource<Scope = NamespaceResourceScope> + Clone + DeserializeOwned + Debug,
    <K as kube::Resource>::DynamicType: Default,
{
    // Background propagation, like kubectl: the API's own default for jobs orphans their pods.
    Api::<K>::namespaced(client, ns).delete(name, &DeleteParams::background()).await?;
    Ok(())
}

pub async fn delete(client: Client, kind: Kind, ns: String, name: String) -> Result<()> {
    match kind {
        Kind::Pod => delete_one::<Pod>(client, &ns, &name).await,
        Kind::Service => delete_one::<Service>(client, &ns, &name).await,
        Kind::Deployment => delete_one::<Deployment>(client, &ns, &name).await,
        Kind::DaemonSet => delete_one::<DaemonSet>(client, &ns, &name).await,
        Kind::StatefulSet => delete_one::<StatefulSet>(client, &ns, &name).await,
        Kind::ReplicaSet => delete_one::<ReplicaSet>(client, &ns, &name).await,
        Kind::ReplicationController => delete_one::<ReplicationController>(client, &ns, &name).await,
        Kind::Job => delete_one::<Job>(client, &ns, &name).await,
        Kind::CronJob => delete_one::<CronJob>(client, &ns, &name).await,
    }
}

/// Suspends or resumes a cron job (no new runs) or a job (its pods are stopped).
pub async fn set_suspend(client: Client, kind: Kind, ns: String, name: String, suspend: bool) -> Result<()> {
    let patch = json!({ "spec": { "suspend": suspend } });
    match kind {
        Kind::CronJob => merge_patch::<CronJob>(client, &ns, &name, patch).await,
        Kind::Job => merge_patch::<Job>(client, &ns, &name, patch).await,
        _ => Err(anyhow!("{} can't be suspended", kind.label())),
    }
}

/// Pauses or resumes a deployment's rollout.
pub async fn set_paused(client: Client, ns: String, name: String, paused: bool) -> Result<()> {
    merge_patch::<Deployment>(client, &ns, &name, json!({ "spec": { "paused": paused } })).await
}

/// Runs a cron job now, like `kubectl create job --from=cronjob/<name>`. Returns the job's name.
pub async fn trigger_cronjob(client: Client, ns: String, name: String) -> Result<String> {
    let cron = Api::<CronJob>::namespaced(client.clone(), &ns).get(&name).await?;
    let template = cron.spec.job_template;
    // Job names are DNS labels (63 chars); keep the suffix and trim the cron job's name.
    let suffix = format!("-manual-{}", Timestamp::now().as_second() % 1_000_000);
    let base: String = name.chars().take(63 - suffix.len()).collect();
    let job_name = format!("{}{suffix}", base.trim_end_matches('-'));
    let tmeta = template.metadata.unwrap_or_default();
    let mut annotations = tmeta.annotations.unwrap_or_default();
    annotations.insert("cronjob.kubernetes.io/instantiate".into(), "manual".into());
    let job = Job {
        metadata: k8s_openapi::apimachinery::pkg::apis::meta::v1::ObjectMeta {
            name: Some(job_name.clone()),
            namespace: Some(ns.clone()),
            labels: tmeta.labels,
            annotations: Some(annotations),
            // Owned by the cron job, so it shows up (and is cleaned up) with its runs.
            owner_references: Some(vec![k8s_openapi::apimachinery::pkg::apis::meta::v1::OwnerReference {
                api_version: "batch/v1".into(),
                kind: "CronJob".into(),
                name: name.clone(),
                uid: cron.metadata.uid.clone().unwrap_or_default(),
                controller: Some(true),
                block_owner_deletion: Some(true),
            }]),
            ..Default::default()
        },
        spec: template.spec,
        status: None,
    };
    Api::<Job>::namespaced(client, &ns).create(&PostParams::default(), &job).await?;
    Ok(job_name)
}

async fn create_one<K>(client: Client, ns: &str, value: serde_json::Value) -> Result<String>
where
    K: kube::Resource<Scope = NamespaceResourceScope> + Clone + DeserializeOwned + Debug + serde::Serialize,
    <K as kube::Resource>::DynamicType: Default,
{
    let obj: K = serde_json::from_value(value).context("the YAML doesn't match its kind")?;
    let created = Api::<K>::namespaced(client, ns).create(&PostParams::default(), &obj).await?;
    Ok(created.name_any())
}

/// Creates an object from YAML, like `kubectl create -f`. The kind comes from the YAML;
/// the namespace too, falling back to `default_ns`. Returns (kind, namespace, name).
pub async fn create_yaml(client: Client, default_ns: String, yaml: String) -> Result<(Kind, String, String)> {
    let mut value: serde_json::Value = serde_yaml::from_str(&yaml).context("invalid YAML")?;
    let kind_name = value["kind"].as_str().unwrap_or_default().to_string();
    let kind = Kind::from_label(&kind_name).ok_or_else(|| {
        let known = Kind::ALL.map(Kind::label).join(", ");
        anyhow!("kind {kind_name:?} isn't supported here (use one of: {known})")
    })?;
    let ns = value["metadata"]["namespace"].as_str().map(str::to_string).unwrap_or(default_ns);
    if !value["metadata"].is_object() {
        return Err(anyhow!("metadata is missing"));
    }
    value["metadata"]["namespace"] = serde_json::Value::String(ns.clone());
    let name = match kind {
        Kind::Pod => create_one::<Pod>(client, &ns, value).await?,
        Kind::Service => create_one::<Service>(client, &ns, value).await?,
        Kind::Deployment => create_one::<Deployment>(client, &ns, value).await?,
        Kind::DaemonSet => create_one::<DaemonSet>(client, &ns, value).await?,
        Kind::StatefulSet => create_one::<StatefulSet>(client, &ns, value).await?,
        Kind::ReplicaSet => create_one::<ReplicaSet>(client, &ns, value).await?,
        Kind::ReplicationController => create_one::<ReplicationController>(client, &ns, value).await?,
        Kind::Job => create_one::<Job>(client, &ns, value).await?,
        Kind::CronJob => create_one::<CronJob>(client, &ns, value).await?,
    };
    Ok((kind, ns, name))
}

/// A starting point for a new object of `kind` in `ns`, like Lens's create templates.
pub fn template(kind: Kind, ns: &str) -> String {
    let pod_spec = |indent: &str| {
        [
            "containers:",
            "  - name: main",
            "    image: nginx:alpine",
            "    ports:",
            "      - containerPort: 80",
            "    resources:",
            "      requests:",
            "        cpu: 50m",
            "        memory: 64Mi",
            "      limits:",
            "        memory: 128Mi",
        ]
        .iter()
        .map(|l| format!("{indent}{l}"))
        .collect::<Vec<_>>()
        .join("\n")
    };
    match kind {
        Kind::Pod => format!(
            "apiVersion: v1\nkind: Pod\nmetadata:\n  name: my-pod\n  namespace: {ns}\n  labels:\n    app: my-pod\nspec:\n{}\n  restartPolicy: Always\n",
            pod_spec("  ")
        ),
        Kind::Deployment => format!(
            "apiVersion: apps/v1\nkind: Deployment\nmetadata:\n  name: my-app\n  namespace: {ns}\n  labels:\n    app: my-app\nspec:\n  replicas: 1\n  selector:\n    matchLabels:\n      app: my-app\n  template:\n    metadata:\n      labels:\n        app: my-app\n    spec:\n{}\n",
            pod_spec("      ")
        ),
        Kind::StatefulSet => format!(
            "apiVersion: apps/v1\nkind: StatefulSet\nmetadata:\n  name: my-db\n  namespace: {ns}\nspec:\n  serviceName: my-db\n  replicas: 1\n  selector:\n    matchLabels:\n      app: my-db\n  template:\n    metadata:\n      labels:\n        app: my-db\n    spec:\n{}\n",
            pod_spec("      ")
        ),
        Kind::DaemonSet => format!(
            "apiVersion: apps/v1\nkind: DaemonSet\nmetadata:\n  name: my-agent\n  namespace: {ns}\nspec:\n  selector:\n    matchLabels:\n      app: my-agent\n  template:\n    metadata:\n      labels:\n        app: my-agent\n    spec:\n{}\n",
            pod_spec("      ")
        ),
        Kind::ReplicaSet => format!(
            "apiVersion: apps/v1\nkind: ReplicaSet\nmetadata:\n  name: my-rs\n  namespace: {ns}\nspec:\n  replicas: 1\n  selector:\n    matchLabels:\n      app: my-rs\n  template:\n    metadata:\n      labels:\n        app: my-rs\n    spec:\n{}\n",
            pod_spec("      ")
        ),
        Kind::ReplicationController => format!(
            "apiVersion: v1\nkind: ReplicationController\nmetadata:\n  name: my-rc\n  namespace: {ns}\nspec:\n  replicas: 1\n  selector:\n    app: my-rc\n  template:\n    metadata:\n      labels:\n        app: my-rc\n    spec:\n{}\n",
            pod_spec("      ")
        ),
        Kind::Service => format!(
            "apiVersion: v1\nkind: Service\nmetadata:\n  name: my-service\n  namespace: {ns}\nspec:\n  type: ClusterIP\n  selector:\n    app: my-app\n  ports:\n    - name: http\n      port: 80\n      targetPort: 80\n"
        ),
        Kind::Job => format!(
            "apiVersion: batch/v1\nkind: Job\nmetadata:\n  name: my-job\n  namespace: {ns}\nspec:\n  backoffLimit: 2\n  ttlSecondsAfterFinished: 3600\n  template:\n    spec:\n      restartPolicy: Never\n      containers:\n        - name: main\n          image: busybox:1.36\n          command: [\"sh\", \"-c\", \"echo hello && sleep 5\"]\n"
        ),
        Kind::CronJob => format!(
            "apiVersion: batch/v1\nkind: CronJob\nmetadata:\n  name: my-cronjob\n  namespace: {ns}\nspec:\n  schedule: \"*/30 * * * *\"\n  concurrencyPolicy: Forbid\n  jobTemplate:\n    spec:\n      backoffLimit: 1\n      template:\n        spec:\n          restartPolicy: Never\n          containers:\n            - name: main\n              image: busybox:1.36\n              command: [\"sh\", \"-c\", \"date\"]\n"
        ),
    }
}

async fn replace_one<K>(client: Client, ns: &str, name: &str, value: serde_json::Value) -> Result<()>
where
    K: kube::Resource<Scope = NamespaceResourceScope> + Clone + DeserializeOwned + Debug + serde::Serialize,
    <K as kube::Resource>::DynamicType: Default,
{
    let obj: K = serde_json::from_value(value).context("the YAML doesn't match this kind")?;
    Api::<K>::namespaced(client, ns).replace(name, &PostParams::default(), &obj).await?;
    Ok(())
}

/// Saves an edited object (a full replace). The `resourceVersion` in the YAML makes the
/// API refuse the write if someone else changed the object since it was loaded.
pub async fn apply_yaml(client: Client, kind: Kind, ns: String, name: String, yaml: String) -> Result<()> {
    let value: serde_json::Value = serde_yaml::from_str(&yaml).context("invalid YAML")?;
    let meta = &value["metadata"];
    if meta["name"].as_str() != Some(name.as_str()) {
        return Err(anyhow!("metadata.name must stay {name:?}; create a copy with kubectl instead"));
    }
    if meta["namespace"].as_str().is_some_and(|n| n != ns) {
        return Err(anyhow!("metadata.namespace must stay {ns:?}"));
    }
    if value["kind"].as_str().is_some_and(|k| k != kind.label()) {
        return Err(anyhow!("kind must stay {}", kind.label()));
    }
    match kind {
        Kind::Pod => replace_one::<Pod>(client, &ns, &name, value).await,
        Kind::Service => replace_one::<Service>(client, &ns, &name, value).await,
        Kind::Deployment => replace_one::<Deployment>(client, &ns, &name, value).await,
        Kind::DaemonSet => replace_one::<DaemonSet>(client, &ns, &name, value).await,
        Kind::StatefulSet => replace_one::<StatefulSet>(client, &ns, &name, value).await,
        Kind::ReplicaSet => replace_one::<ReplicaSet>(client, &ns, &name, value).await,
        Kind::ReplicationController => replace_one::<ReplicationController>(client, &ns, &name, value).await,
        Kind::Job => replace_one::<Job>(client, &ns, &name, value).await,
        Kind::CronJob => replace_one::<CronJob>(client, &ns, &name, value).await,
    }
}

// ---------------------------------------------------------------------------
// YAML
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Kind {
    Pod,
    Service,
    Deployment,
    DaemonSet,
    StatefulSet,
    ReplicaSet,
    ReplicationController,
    Job,
    CronJob,
}

impl Kind {
    /// The API kind name, as used in events and YAML.
    pub fn label(self) -> &'static str {
        match self {
            Kind::Pod => "Pod",
            Kind::Service => "Service",
            Kind::Deployment => "Deployment",
            Kind::DaemonSet => "DaemonSet",
            Kind::StatefulSet => "StatefulSet",
            Kind::ReplicaSet => "ReplicaSet",
            Kind::ReplicationController => "ReplicationController",
            Kind::Job => "Job",
            Kind::CronJob => "CronJob",
        }
    }

    /// Page title.
    pub fn plural(self) -> &'static str {
        match self {
            Kind::Pod => "Pods",
            Kind::Service => "Services",
            Kind::Deployment => "Deployments",
            Kind::DaemonSet => "Daemon Sets",
            Kind::StatefulSet => "Stateful Sets",
            Kind::ReplicaSet => "Replica Sets",
            Kind::ReplicationController => "Replication Controllers",
            Kind::Job => "Jobs",
            Kind::CronJob => "Cron Jobs",
        }
    }

    pub const ALL: [Kind; 9] = [
        Kind::Pod,
        Kind::Service,
        Kind::Deployment,
        Kind::DaemonSet,
        Kind::StatefulSet,
        Kind::ReplicaSet,
        Kind::ReplicationController,
        Kind::Job,
        Kind::CronJob,
    ];

    pub fn from_label(label: &str) -> Option<Kind> {
        Kind::ALL.into_iter().find(|k| k.label() == label)
    }

    pub fn scalable(self) -> bool {
        matches!(self, Kind::Deployment | Kind::StatefulSet | Kind::ReplicaSet | Kind::ReplicationController)
    }

    pub fn restartable(self) -> bool {
        matches!(self, Kind::Deployment | Kind::DaemonSet | Kind::StatefulSet)
    }
}

async fn get_value<K>(client: Client, ns: &str, name: &str) -> Result<serde_json::Value>
where
    K: kube::Resource<Scope = NamespaceResourceScope> + Clone + DeserializeOwned + Debug + serde::Serialize,
    <K as kube::Resource>::DynamicType: Default,
{
    Ok(serde_json::to_value(Api::<K>::namespaced(client, ns).get(name).await?)?)
}

/// The object as JSON, for any supported kind.
pub async fn get_object(client: Client, kind: Kind, ns: &str, name: &str) -> Result<serde_json::Value> {
    match kind {
        Kind::Pod => get_value::<Pod>(client, ns, name).await,
        Kind::Service => get_value::<Service>(client, ns, name).await,
        Kind::Deployment => get_value::<Deployment>(client, ns, name).await,
        Kind::DaemonSet => get_value::<DaemonSet>(client, ns, name).await,
        Kind::StatefulSet => get_value::<StatefulSet>(client, ns, name).await,
        Kind::ReplicaSet => get_value::<ReplicaSet>(client, ns, name).await,
        Kind::ReplicationController => get_value::<ReplicationController>(client, ns, name).await,
        Kind::Job => get_value::<Job>(client, ns, name).await,
        Kind::CronJob => get_value::<CronJob>(client, ns, name).await,
    }
}

pub async fn get_yaml(client: Client, kind: Kind, ns: String, name: String) -> Result<String> {
    let mut value = get_object(client, kind, &ns, &name).await?;
    if let Some(meta) = value.get_mut("metadata").and_then(|m| m.as_object_mut()) {
        meta.remove("managedFields");
    }
    Ok(serde_yaml::to_string(&value)?)
}

// ---------------------------------------------------------------------------
// Port forwarding
// ---------------------------------------------------------------------------

#[derive(Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum PfTarget {
    Pod(String),
    Service(String),
}

impl PfTarget {
    pub fn label(&self) -> String {
        match self {
            PfTarget::Pod(n) => format!("pod/{n}"),
            PfTarget::Service(n) => format!("svc/{n}"),
        }
    }
}

pub enum PfEvent {
    Listening(u16),
    ConnError(String),
}

/// Resolves a forward target to a concrete pod name and container port.
/// For services this picks a ready pod behind the selector and maps `targetPort`.
async fn resolve_target(client: &Client, ns: &str, target: &PfTarget, port: u16) -> Result<(String, u16)> {
    let name = match target {
        PfTarget::Pod(name) => return Ok((name.clone(), port)),
        PfTarget::Service(name) => name,
    };
    let svc = Api::<Service>::namespaced(client.clone(), ns).get(name).await?;
    let spec = svc.spec.context("service has no spec")?;
    let selector = spec
        .selector
        .filter(|s| !s.is_empty())
        .ok_or_else(|| anyhow!("service {name} has no selector"))?;
    let svc_port = spec
        .ports
        .unwrap_or_default()
        .into_iter()
        .find(|p| p.port as u16 == port)
        .ok_or_else(|| anyhow!("service {name} has no port {port}"))?;

    let labels = selector
        .iter()
        .map(|(k, v)| format!("{k}={v}"))
        .collect::<Vec<_>>()
        .join(",");
    let pods = Api::<Pod>::namespaced(client.clone(), ns)
        .list(&ListParams::default().labels(&labels))
        .await?;
    let is_ready = |p: &Pod| {
        p.metadata.deletion_timestamp.is_none()
            && p.status
                .as_ref()
                .and_then(|s| s.conditions.as_ref())
                .is_some_and(|c| c.iter().any(|c| c.type_ == "Ready" && c.status == "True"))
    };
    let pod = pods
        .items
        .into_iter()
        .find(is_ready)
        .ok_or_else(|| anyhow!("no ready pods behind service {name}"))?;

    let target_port = match svc_port.target_port {
        None => port,
        Some(IntOrString::Int(p)) => p as u16,
        Some(IntOrString::String(port_name)) => pod
            .spec
            .as_ref()
            .into_iter()
            .flat_map(|s| s.containers.iter())
            .flat_map(|c| c.ports.iter().flatten())
            .find(|p| p.name.as_deref() == Some(port_name.as_str()))
            .map(|p| p.container_port as u16)
            .or_else(|| port_name.parse().ok())
            .ok_or_else(|| anyhow!("pod {} has no port named {port_name}", pod.name_any()))?,
    };
    Ok((pod.name_any(), target_port))
}

/// Listens on 127.0.0.1:`local_port` and tunnels each connection to the target.
/// Runs until the task is aborted or the listener fails.
pub async fn port_forward(
    client: Client,
    ns: String,
    target: PfTarget,
    remote_port: u16,
    local_port: u16,
    connections: Arc<AtomicUsize>,
    on_event: impl Fn(PfEvent) + Send + Sync + 'static,
) -> Result<()> {
    // Fail early if the target doesn't resolve, rather than on the first connection.
    resolve_target(&client, &ns, &target, remote_port).await?;
    let listener = TcpListener::bind(("127.0.0.1", local_port))
        .await
        .with_context(|| format!("cannot bind 127.0.0.1:{local_port}"))?;
    on_event(PfEvent::Listening(listener.local_addr()?.port()));
    let on_event = Arc::new(on_event);

    loop {
        let (mut socket, _) = listener.accept().await?;
        let (client, ns, target) = (client.clone(), ns.clone(), target.clone());
        let (connections, on_event) = (connections.clone(), on_event.clone());
        tokio::spawn(async move {
            connections.fetch_add(1, Ordering::Relaxed);
            let result: Result<()> = async {
                let (pod, port) = resolve_target(&client, &ns, &target, remote_port).await?;
                let api: Api<Pod> = Api::namespaced(client, &ns);
                let mut forwarder = api.portforward(&pod, &[port]).await?;
                let mut upstream = forwarder
                    .take_stream(port)
                    .context("port-forward stream unavailable")?;
                let _ = tokio::io::copy_bidirectional(&mut socket, &mut upstream).await;
                drop(upstream);
                // Surfaces errors the kubelet reports, e.g. nothing listening on the port.
                forwarder.join().await.with_context(|| format!("pod/{pod}:{port}"))
            }
            .await;
            connections.fetch_sub(1, Ordering::Relaxed);
            if let Err(e) = result {
                on_event(PfEvent::ConnError(format!("{e:#}")));
            }
        });
    }
}

// ---------------------------------------------------------------------------
// Other workloads (daemon sets, stateful sets, replica sets, replication
// controllers, jobs, cron jobs) share one row shape: a few kind-specific
// columns plus what the drawer needs.
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Health {
    Ok,
    /// Rolling out, scaling, running.
    Progress,
    Bad,
    /// Scaled to zero, suspended.
    Idle,
}

/// A table cell. Times are kept raw so they keep counting between watch events.
#[derive(Clone)]
pub enum Cell {
    Text(String),
    /// Time since then ("5m"), or "-".
    Ago(Option<Time>),
    /// From start to end, or to now while still running.
    Span(Option<Time>, Option<Time>),
}

impl Cell {
    /// The time a cell stands for, in seconds (for sorting); `None` for text.
    pub fn seconds(&self) -> Option<i64> {
        match self {
            Cell::Text(_) => None,
            Cell::Ago(t) => Some(age_secs(t.as_ref()).unwrap_or(i64::MAX)),
            Cell::Span(None, _) => Some(i64::MAX),
            Cell::Span(Some(start), end) => {
                let end = end.as_ref().map(|t| t.0).unwrap_or_else(Timestamp::now);
                Some(end.duration_since(start.0).as_secs())
            }
        }
    }

    pub fn text(&self) -> String {
        match self {
            Cell::Text(s) => s.clone(),
            Cell::Ago(t) => age(t.as_ref()),
            Cell::Span(None, _) => "-".into(),
            Cell::Span(Some(start), end) => {
                let end = end.as_ref().map(|t| t.0).unwrap_or_else(Timestamp::now);
                span(end.duration_since(start.0).as_secs())
            }
        }
    }
}

#[derive(Clone)]
pub struct WorkRow {
    pub kind: Kind,
    pub name: String,
    pub namespace: String,
    pub created: Option<Time>,
    pub health: Health,
    /// One-line state for the drawer header, e.g. "Ready 2/3" or "Complete".
    pub summary: String,
    /// Kind-specific columns, matching `columns(kind)`.
    pub cells: Vec<Cell>,
    /// Desired replicas, for kinds that scale.
    pub replicas: Option<i32>,
    /// Pod label selector ("k=v,k2=v2"), used for "Logs (all pods)".
    pub selector: String,
    pub images: Vec<String>,
    /// Extra drawer rows.
    pub info: Vec<(&'static str, String)>,
    /// "Kind/name" of the controlling owner, if any.
    pub owner: String,
    /// For jobs and cron jobs: whether `spec.suspend` is set.
    pub suspended: Option<bool>,
    /// For jobs: finished (complete or failed), so suspending no longer applies.
    pub finished: bool,
}

impl WorkRow {
    pub fn age(&self) -> String {
        age(self.created.as_ref())
    }
}

/// Column titles and widths for each workload kind's table (after Name, before Age).
pub fn columns(kind: Kind) -> &'static [(&'static str, f32)] {
    match kind {
        Kind::DaemonSet => &[("Desired", 68.0), ("Current", 68.0), ("Ready", 60.0), ("Up-to-date", 84.0), ("Available", 76.0)],
        Kind::StatefulSet => &[("Ready", 72.0), ("Up-to-date", 84.0)],
        Kind::ReplicaSet | Kind::ReplicationController => &[("Desired", 68.0), ("Current", 68.0), ("Ready", 60.0)],
        Kind::Job => &[("Status", 96.0), ("Completions", 96.0), ("Duration", 76.0)],
        Kind::CronJob => &[("Schedule", 130.0), ("Suspend", 70.0), ("Active", 56.0), ("Last run", 76.0)],
        _ => &[],
    }
}

fn n(v: i32) -> Cell {
    Cell::Text(v.to_string())
}

fn match_labels(sel: Option<&k8s_openapi::apimachinery::pkg::apis::meta::v1::LabelSelector>) -> String {
    sel.and_then(|s| s.match_labels.as_ref()).map(join_labels).unwrap_or_default()
}

fn join_labels(m: &std::collections::BTreeMap<String, String>) -> String {
    m.iter().map(|(k, v)| format!("{k}={v}")).collect::<Vec<_>>().join(",")
}

fn template_images(t: Option<&k8s_openapi::api::core::v1::PodTemplateSpec>) -> Vec<String> {
    t.and_then(|t| t.spec.as_ref())
        .map(|s| s.containers.iter().filter_map(|c| c.image.clone()).collect())
        .unwrap_or_default()
}

fn owner_of(meta: &k8s_openapi::apimachinery::pkg::apis::meta::v1::ObjectMeta) -> String {
    meta.owner_references
        .iter()
        .flatten()
        .find(|o| o.controller == Some(true))
        .map(|o| format!("{}/{}", o.kind, o.name))
        .unwrap_or_default()
}

/// Ready-vs-desired health shared by the replica-style kinds.
fn replica_health(ready: i32, desired: i32) -> Health {
    if desired == 0 {
        Health::Idle
    } else if ready >= desired {
        Health::Ok
    } else {
        Health::Progress
    }
}

fn base<K: kube::Resource>(kind: Kind, k: &K) -> WorkRow {
    let meta = k.meta();
    WorkRow {
        kind,
        name: meta.name.clone().unwrap_or_default(),
        namespace: meta.namespace.clone().unwrap_or_default(),
        created: meta.creation_timestamp.clone(),
        health: Health::Ok,
        summary: String::new(),
        cells: Vec::new(),
        replicas: None,
        selector: String::new(),
        images: Vec::new(),
        info: Vec::new(),
        owner: owner_of(meta),
        suspended: None,
        finished: false,
    }
}

pub fn daemon_set_row(d: &DaemonSet) -> WorkRow {
    let st = d.status.as_ref();
    let desired = st.map(|s| s.desired_number_scheduled).unwrap_or(0);
    let current = st.map(|s| s.current_number_scheduled).unwrap_or(0);
    let ready = st.map(|s| s.number_ready).unwrap_or(0);
    let updated = st.and_then(|s| s.updated_number_scheduled).unwrap_or(0);
    let available = st.and_then(|s| s.number_available).unwrap_or(0);
    let mut row = base(Kind::DaemonSet, d);
    row.health = replica_health(ready, desired);
    row.summary = format!("Ready {ready}/{desired}");
    row.cells = vec![n(desired), n(current), n(ready), n(updated), n(available)];
    row.selector = match_labels(Some(&d.spec.selector));
    row.images = template_images(Some(&d.spec.template));
    let node_selector = d
        .spec
        .template
        .spec
        .as_ref()
        .and_then(|s| s.node_selector.as_ref())
        .map(join_labels)
        .unwrap_or_default();
    row.info = vec![
        ("Strategy", d.spec.update_strategy.as_ref().and_then(|s| s.type_.clone()).unwrap_or_else(|| "RollingUpdate".into())),
        ("Node selector", if node_selector.is_empty() { "-".into() } else { node_selector }),
        ("Misscheduled", st.map(|s| s.number_misscheduled).unwrap_or(0).to_string()),
    ];
    row
}

pub fn stateful_set_row(s: &StatefulSet) -> WorkRow {
    let st = s.status.as_ref();
    let desired = s.spec.replicas.unwrap_or(1);
    let ready = st.and_then(|s| s.ready_replicas).unwrap_or(0);
    let updated = st.and_then(|s| s.updated_replicas).unwrap_or(0);
    let mut row = base(Kind::StatefulSet, s);
    row.health = replica_health(ready, desired);
    row.summary = format!("Ready {ready}/{desired}");
    row.cells = vec![Cell::Text(format!("{ready}/{desired}")), n(updated)];
    row.replicas = Some(desired);
    row.selector = match_labels(Some(&s.spec.selector));
    row.images = template_images(Some(&s.spec.template));
    row.info = vec![
        ("Service", s.spec.service_name.clone().unwrap_or_else(|| "-".into())),
        ("Strategy", s.spec.update_strategy.as_ref().and_then(|s| s.type_.clone()).unwrap_or_else(|| "RollingUpdate".into())),
        ("Pod management", s.spec.pod_management_policy.clone().unwrap_or_else(|| "OrderedReady".into())),
        ("Volume claims", s.spec.volume_claim_templates.as_ref().map(Vec::len).unwrap_or(0).to_string()),
    ];
    row
}

pub fn replica_set_row(r: &ReplicaSet) -> WorkRow {
    let st = r.status.as_ref();
    let desired = r.spec.replicas.unwrap_or(1);
    let current = st.map(|s| s.replicas).unwrap_or(0);
    let ready = st.and_then(|s| s.ready_replicas).unwrap_or(0);
    let mut row = base(Kind::ReplicaSet, r);
    row.health = replica_health(ready, desired);
    row.summary = format!("Ready {ready}/{desired}");
    row.cells = vec![n(desired), n(current), n(ready)];
    row.replicas = Some(desired);
    row.selector = match_labels(Some(&r.spec.selector));
    row.images = template_images(r.spec.template.as_ref());
    row
}

pub fn replication_controller_row(r: &ReplicationController) -> WorkRow {
    let spec = r.spec.as_ref();
    let st = r.status.as_ref();
    let desired = spec.and_then(|s| s.replicas).unwrap_or(1);
    let current = st.map(|s| s.replicas).unwrap_or(0);
    let ready = st.and_then(|s| s.ready_replicas).unwrap_or(0);
    let mut row = base(Kind::ReplicationController, r);
    row.health = replica_health(ready, desired);
    row.summary = format!("Ready {ready}/{desired}");
    row.cells = vec![n(desired), n(current), n(ready)];
    row.replicas = Some(desired);
    row.selector = spec.and_then(|s| s.selector.as_ref()).map(join_labels).unwrap_or_default();
    row.images = template_images(spec.and_then(|s| s.template.as_ref()));
    row
}

pub fn job_row(j: &Job) -> WorkRow {
    let spec = j.spec.as_ref();
    let st = j.status.as_ref();
    let has = |t: &str| {
        st.and_then(|s| s.conditions.as_ref())
            .into_iter()
            .flatten()
            .any(|c| c.type_ == t && c.status == "True")
    };
    let succeeded = st.and_then(|s| s.succeeded).unwrap_or(0);
    let failed = st.and_then(|s| s.failed).unwrap_or(0);
    let active = st.and_then(|s| s.active).unwrap_or(0);
    let completions = spec.and_then(|s| s.completions);
    let (status, health) = if has("Failed") {
        ("Failed", Health::Bad)
    } else if has("Complete") {
        ("Complete", Health::Ok)
    } else if spec.and_then(|s| s.suspend).unwrap_or(false) {
        ("Suspended", Health::Idle)
    } else if active > 0 {
        ("Running", Health::Progress)
    } else {
        ("Pending", Health::Progress)
    };
    let mut row = base(Kind::Job, j);
    row.health = health;
    row.suspended = Some(spec.and_then(|s| s.suspend).unwrap_or(false));
    row.finished = matches!(status, "Failed" | "Complete");
    row.summary = status.to_string();
    row.cells = vec![
        Cell::Text(status.into()),
        Cell::Text(match completions {
            Some(c) => format!("{succeeded}/{c}"),
            None => succeeded.to_string(),
        }),
        // Failed jobs have no completion time; they ended when the Failed condition was set.
        Cell::Span(
            st.and_then(|s| s.start_time.clone()),
            st.and_then(|s| s.completion_time.clone()).or_else(|| {
                st.and_then(|s| s.conditions.as_ref())
                    .into_iter()
                    .flatten()
                    .find(|c| c.type_ == "Failed" && c.status == "True")
                    .and_then(|c| c.last_transition_time.clone())
            }),
        ),
    ];
    row.selector = match_labels(spec.and_then(|s| s.selector.as_ref()));
    row.images = template_images(spec.map(|s| &s.template));
    row.info = vec![
        ("Active", active.to_string()),
        ("Succeeded", succeeded.to_string()),
        ("Failed", failed.to_string()),
        ("Parallelism", spec.and_then(|s| s.parallelism).unwrap_or(1).to_string()),
        ("Backoff limit", spec.and_then(|s| s.backoff_limit).unwrap_or(6).to_string()),
    ];
    row
}

pub fn cron_job_row(c: &CronJob) -> WorkRow {
    let st = c.status.as_ref();
    let suspended = c.spec.suspend.unwrap_or(false);
    let active = st.and_then(|s| s.active.as_ref()).map(Vec::len).unwrap_or(0);
    let mut row = base(Kind::CronJob, c);
    row.suspended = Some(suspended);
    row.health = if suspended {
        Health::Idle
    } else if active > 0 {
        Health::Progress
    } else {
        Health::Ok
    };
    row.summary = if suspended { "Suspended".into() } else { format!("Scheduled · {}", c.spec.schedule) };
    row.cells = vec![
        Cell::Text(c.spec.schedule.clone()),
        Cell::Text(if suspended { "Yes" } else { "No" }.into()),
        Cell::Text(active.to_string()),
        Cell::Ago(st.and_then(|s| s.last_schedule_time.clone())),
    ];
    let job_spec = c.spec.job_template.spec.as_ref();
    row.images = template_images(job_spec.map(|s| &s.template));
    row.info = vec![
        ("Time zone", c.spec.time_zone.clone().unwrap_or_else(|| "Controller local".into())),
        ("Concurrency", c.spec.concurrency_policy.clone().unwrap_or_else(|| "Allow".into())),
        ("Last success", age(st.and_then(|s| s.last_successful_time.as_ref()))),
        ("History", format!(
            "{} ok / {} failed",
            c.spec.successful_jobs_history_limit.unwrap_or(3),
            c.spec.failed_jobs_history_limit.unwrap_or(1)
        )),
    ];
    row
}

/// Lists one workload kind (one-shot).
pub async fn list_workloads(client: Client, kind: Kind, ns: Option<String>) -> Result<Vec<WorkRow>> {
    match kind {
        Kind::DaemonSet => list_rows(client, ns, daemon_set_row).await,
        Kind::StatefulSet => list_rows(client, ns, stateful_set_row).await,
        Kind::ReplicaSet => list_rows(client, ns, replica_set_row).await,
        Kind::ReplicationController => list_rows(client, ns, replication_controller_row).await,
        Kind::Job => list_rows(client, ns, job_row).await,
        Kind::CronJob => list_rows(client, ns, cron_job_row).await,
        _ => Err(anyhow!("{} is not a generic workload", kind.label())),
    }
}

/// Watches one workload kind; see `watch_rows`.
pub async fn watch_workloads(
    client: Client,
    kind: Kind,
    namespaces: Vec<String>,
    emit: impl FnMut(Vec<WorkRow>, Vec<String>),
) {
    match kind {
        Kind::DaemonSet => watch_rows(client, namespaces, daemon_set_row, emit).await,
        Kind::StatefulSet => watch_rows(client, namespaces, stateful_set_row, emit).await,
        Kind::ReplicaSet => watch_rows(client, namespaces, replica_set_row, emit).await,
        Kind::ReplicationController => watch_rows(client, namespaces, replication_controller_row, emit).await,
        Kind::Job => watch_rows(client, namespaces, job_row, emit).await,
        Kind::CronJob => watch_rows(client, namespaces, cron_job_row, emit).await,
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every create template is valid YAML that deserializes into its kind.
    #[test]
    fn templates_parse() {
        fn check<K: DeserializeOwned>(kind: Kind) {
            let v: serde_json::Value = serde_yaml::from_str(&template(kind, "demo")).expect("yaml");
            assert_eq!(v["kind"], kind.label());
            assert_eq!(v["metadata"]["namespace"], "demo");
            serde_json::from_value::<K>(v).unwrap_or_else(|e| panic!("{}: {e}", kind.label()));
        }
        check::<Pod>(Kind::Pod);
        check::<Service>(Kind::Service);
        check::<Deployment>(Kind::Deployment);
        check::<DaemonSet>(Kind::DaemonSet);
        check::<StatefulSet>(Kind::StatefulSet);
        check::<ReplicaSet>(Kind::ReplicaSet);
        check::<ReplicationController>(Kind::ReplicationController);
        check::<Job>(Kind::Job);
        check::<CronJob>(Kind::CronJob);
        assert_eq!(Kind::from_label("CronJob"), Some(Kind::CronJob));
        assert_eq!(Kind::from_label("Ingress"), None);
    }
}

#[cfg(test)]
mod describe_tests {
    use super::*;

    fn status(code: u16, message: &str) -> anyhow::Error {
        let s = kube::core::Status { code, message: message.into(), reason: "Forbidden".into(), ..Default::default() };
        anyhow::Error::from(kube::Error::Api(Box::new(s)))
    }

    #[test]
    fn forbidden_in_namespace() {
        let e = status(
            403,
            r#"pods is forbidden: User "system:serviceaccount:kube-system:dev" cannot list resource "pods" in API group "" in the namespace "cattle-fleet-system""#,
        );
        assert_eq!(
            describe_error(&e),
            r#"No permission to list pods in namespace "cattle-fleet-system". Signed in as system:serviceaccount:kube-system:dev."#
        );
    }

    #[test]
    fn forbidden_cluster_scope_with_group() {
        let e = status(
            403,
            r#"deployments.apps is forbidden: User "alice" cannot list resource "deployments" in API group "apps" at the cluster scope"#,
        );
        assert_eq!(describe_error(&e), "No permission to list deployments.apps across the cluster. Signed in as alice.");
    }
}
