//! Full detail views for the drawer, fetched on demand when a resource is selected.

use std::collections::{BTreeMap, BTreeSet};

use anyhow::Result;
use k8s_openapi::{
    api::{
        apps::v1::Deployment,
        core::v1::{ConfigMap, Container, Endpoints, Event, Pod, Probe, Secret, Service},
        discovery::v1::EndpointSlice,
    },
    apimachinery::pkg::{apis::meta::v1::Time, util::intstr::IntOrString},
};
use kube::{
    Client, ResourceExt,
    api::{Api, ListParams},
};

use crate::kube_ops::{Kind, age};

pub struct Condition {
    pub type_: String,
    pub ok: bool,
    pub reason: String,
    pub message: String,
}

pub struct EventRow {
    pub warning: bool,
    pub reason: String,
    pub message: String,
    pub age: String,
    pub count: i32,
}

/// Where an environment variable's value comes from.
pub enum EnvSource {
    Literal,
    /// Resolved from a ConfigMap (value filled in when readable).
    ConfigMap {
        name: String,
        key: String,
    },
    /// Secret values are never fetched until the user asks to reveal them.
    Secret {
        name: String,
        key: String,
    },
    Field(String),
    Resource(String),
}

pub struct EnvRow {
    pub name: String,
    /// `None` when the value could not be resolved (or is a secret).
    pub value: Option<String>,
    pub source: EnvSource,
}

pub struct ContainerDetails {
    pub name: String,
    pub init: bool,
    pub image: String,
    pub pull_policy: String,
    pub command: Vec<String>,
    pub args: Vec<String>,
    pub working_dir: String,
    pub ports: Vec<String>,
    pub env: Vec<EnvRow>,
    /// `envFrom` secrets: all keys of these secrets are injected.
    pub env_from_secrets: Vec<String>,
    pub requests: Vec<(String, String)>,
    pub limits: Vec<(String, String)>,
    pub mounts: Vec<(String, String)>,
    pub probes: Vec<(String, String)>,
    pub last_termination: String,
}

pub struct PodDetails {
    pub qos: String,
    pub service_account: String,
    pub restart_policy: String,
    pub host_ip: String,
    pub started: String,
    pub priority_class: String,
    pub node_selector: Vec<String>,
    pub conditions: Vec<Condition>,
    pub containers: Vec<ContainerDetails>,
    pub volumes: Vec<(String, String)>,
    pub annotations: Vec<(String, String)>,
}

pub struct EndpointRow {
    pub ip: String,
    pub target: String,
    pub node: String,
    pub ready: bool,
}

pub struct ServiceDetails {
    pub session_affinity: String,
    pub external_traffic_policy: String,
    pub internal_traffic_policy: String,
    pub ip_families: String,
    pub endpoint_ports: Vec<String>,
    pub endpoints: Vec<EndpointRow>,
    pub labels: Vec<(String, String)>,
    pub annotations: Vec<(String, String)>,
}

pub struct DeploymentDetails {
    pub paused: bool,
    pub min_ready_seconds: i32,
    pub revision_history: i32,
    pub conditions: Vec<Condition>,
    pub labels: Vec<(String, String)>,
    pub annotations: Vec<(String, String)>,
}

/// Daemon sets, stateful sets, jobs, ...: what every workload has in common.
pub struct WorkloadDetails {
    pub conditions: Vec<Condition>,
    pub labels: Vec<(String, String)>,
    pub annotations: Vec<(String, String)>,
}

pub enum Body {
    Pod(PodDetails),
    Service(ServiceDetails),
    Deployment(DeploymentDetails),
    Workload(WorkloadDetails),
}

pub struct Details {
    pub body: Body,
    /// Events may be forbidden by RBAC; that shouldn't hide the rest of the details.
    pub events: Result<Vec<EventRow>, String>,
}

pub async fn fetch(client: Client, kind: Kind, ns: String, name: String) -> Result<Details> {
    let body = match kind {
        Kind::Pod => Body::Pod(pod_details(&client, &ns, &name).await?),
        Kind::Service => Body::Service(service_details(&client, &ns, &name).await?),
        Kind::Deployment => Body::Deployment(deployment_details(&client, &ns, &name).await?),
        _ => Body::Workload(workload_details(&client, kind, &ns, &name).await?),
    };
    let events = events(&client, &ns, kind.label(), &name)
        .await
        .map_err(|e| format!("{e:#}"));
    Ok(Details { body, events })
}

/// Reads a secret's decoded values, for the "reveal" button.
pub async fn read_secret(
    client: Client,
    ns: String,
    name: String,
) -> Result<BTreeMap<String, String>> {
    let secret = Api::<Secret>::namespaced(client, &ns).get(&name).await?;
    Ok(secret
        .data
        .unwrap_or_default()
        .into_iter()
        .map(|(k, v)| (k, String::from_utf8_lossy(&v.0).into_owned()))
        .collect())
}

fn pairs(map: Option<&BTreeMap<String, String>>) -> Vec<(String, String)> {
    map.map(|m| m.iter().map(|(k, v)| (k.clone(), v.clone())).collect())
        .unwrap_or_default()
}

fn conditions_ok(status: &str) -> bool {
    status == "True"
}

async fn events(client: &Client, ns: &str, kind: &str, name: &str) -> Result<Vec<EventRow>> {
    let api: Api<Event> = Api::namespaced(client.clone(), ns);
    let selector = format!("involvedObject.kind={kind},involvedObject.name={name}");
    let mut items = api
        .list(&ListParams::default().fields(&selector))
        .await?
        .items;
    let last_seen = |e: &Event| {
        e.last_timestamp
            .as_ref()
            .map(|t| t.0)
            .or_else(|| e.event_time.as_ref().map(|t| t.0))
            .or_else(|| e.metadata.creation_timestamp.as_ref().map(|t| t.0))
    };
    items.sort_by_key(|e| std::cmp::Reverse(last_seen(e)));
    Ok(items
        .iter()
        .map(|e| EventRow {
            warning: e.type_.as_deref() == Some("Warning"),
            reason: e.reason.clone().unwrap_or_default(),
            message: e.message.clone().unwrap_or_default(),
            age: age(last_seen(e).map(Time).as_ref()),
            count: e.count.unwrap_or(1),
        })
        .collect())
}

// ---------------------------------------------------------------------------
// Pods
// ---------------------------------------------------------------------------

async fn pod_details(client: &Client, ns: &str, name: &str) -> Result<PodDetails> {
    let pod = Api::<Pod>::namespaced(client.clone(), ns).get(name).await?;
    let spec = pod.spec.clone().unwrap_or_default();
    let status = pod.status.clone().unwrap_or_default();

    // Resolve ConfigMap-backed env vars. Unreadable ConfigMaps just leave values unresolved.
    let mut config_map_names = BTreeSet::new();
    for c in spec
        .containers
        .iter()
        .chain(spec.init_containers.iter().flatten())
    {
        for e in c.env.iter().flatten() {
            if let Some(r) = e
                .value_from
                .as_ref()
                .and_then(|v| v.config_map_key_ref.as_ref())
            {
                config_map_names.insert(r.name.clone());
            }
        }
        for f in c.env_from.iter().flatten() {
            if let Some(r) = &f.config_map_ref {
                config_map_names.insert(r.name.clone());
            }
        }
    }
    let cm_api: Api<ConfigMap> = Api::namespaced(client.clone(), ns);
    let mut config_maps: BTreeMap<String, BTreeMap<String, String>> = BTreeMap::new();
    for cm in config_map_names {
        if let Ok(obj) = cm_api.get(&cm).await {
            config_maps.insert(cm, obj.data.unwrap_or_default());
        }
    }

    let statuses: Vec<_> = status
        .container_statuses
        .iter()
        .flatten()
        .chain(status.init_container_statuses.iter().flatten())
        .collect();
    let containers = spec
        .containers
        .iter()
        .map(|c| (c, false))
        .chain(spec.init_containers.iter().flatten().map(|c| (c, true)))
        .map(|(c, init)| {
            let status = statuses.iter().find(|s| s.name == c.name);
            container_details(c, init, status.copied(), &config_maps)
        })
        .collect();

    let volumes = spec
        .volumes
        .iter()
        .flatten()
        .map(|v| {
            let desc = if let Some(cm) = &v.config_map {
                format!("ConfigMap {}", cm.name)
            } else if let Some(s) = &v.secret {
                format!("Secret {}", s.secret_name.clone().unwrap_or_default())
            } else if let Some(pvc) = &v.persistent_volume_claim {
                format!("PVC {}", pvc.claim_name)
            } else if let Some(e) = &v.empty_dir {
                match &e.medium {
                    Some(m) if !m.is_empty() => format!("EmptyDir ({m})"),
                    _ => "EmptyDir".into(),
                }
            } else if let Some(h) = &v.host_path {
                format!("HostPath {}", h.path)
            } else if v.projected.is_some() {
                "Projected".into()
            } else if v.downward_api.is_some() {
                "Downward API".into()
            } else if v.csi.is_some() {
                "CSI".into()
            } else {
                "Other".into()
            };
            (v.name.clone(), desc)
        })
        .collect();

    Ok(PodDetails {
        qos: status.qos_class.clone().unwrap_or_default(),
        service_account: spec.service_account_name.clone().unwrap_or_default(),
        restart_policy: spec.restart_policy.clone().unwrap_or_default(),
        host_ip: status.host_ip.clone().unwrap_or_default(),
        started: status
            .start_time
            .as_ref()
            .map(|t| format!("{} ago", age(Some(t))))
            .unwrap_or_default(),
        priority_class: spec.priority_class_name.clone().unwrap_or_default(),
        node_selector: spec
            .node_selector
            .iter()
            .flatten()
            .map(|(k, v)| format!("{k}={v}"))
            .collect(),
        conditions: status
            .conditions
            .iter()
            .flatten()
            .map(|c| Condition {
                type_: c.type_.clone(),
                ok: conditions_ok(&c.status),
                reason: c.reason.clone().unwrap_or_default(),
                message: c.message.clone().unwrap_or_default(),
            })
            .collect(),
        containers,
        volumes,
        annotations: pairs(pod.metadata.annotations.as_ref()),
    })
}

fn container_details(
    c: &Container,
    init: bool,
    status: Option<&k8s_openapi::api::core::v1::ContainerStatus>,
    config_maps: &BTreeMap<String, BTreeMap<String, String>>,
) -> ContainerDetails {
    let mut env = Vec::new();
    let mut env_from_secrets = Vec::new();
    // envFrom comes first; explicit `env` entries override it, as in Kubernetes.
    for f in c.env_from.iter().flatten() {
        let prefix = f.prefix.clone().unwrap_or_default();
        if let Some(r) = &f.config_map_ref {
            match config_maps.get(&r.name) {
                Some(data) => env.extend(data.iter().map(|(k, v)| EnvRow {
                    name: format!("{prefix}{k}"),
                    value: Some(v.clone()),
                    source: EnvSource::ConfigMap {
                        name: r.name.clone(),
                        key: k.clone(),
                    },
                })),
                None => env.push(EnvRow {
                    name: format!("{prefix}*"),
                    value: None,
                    source: EnvSource::ConfigMap {
                        name: r.name.clone(),
                        key: "*".into(),
                    },
                }),
            }
        }
        if let Some(r) = &f.secret_ref {
            env_from_secrets.push(r.name.clone());
        }
    }
    for e in c.env.iter().flatten() {
        let (value, source) = match &e.value_from {
            None => (
                Some(e.value.clone().unwrap_or_default()),
                EnvSource::Literal,
            ),
            Some(from) => {
                if let Some(r) = &from.config_map_key_ref {
                    let value = config_maps
                        .get(&r.name)
                        .and_then(|d| d.get(&r.key))
                        .cloned();
                    (
                        value,
                        EnvSource::ConfigMap {
                            name: r.name.clone(),
                            key: r.key.clone(),
                        },
                    )
                } else if let Some(r) = &from.secret_key_ref {
                    (
                        None,
                        EnvSource::Secret {
                            name: r.name.clone(),
                            key: r.key.clone(),
                        },
                    )
                } else if let Some(r) = &from.field_ref {
                    (None, EnvSource::Field(r.field_path.clone()))
                } else if let Some(r) = &from.resource_field_ref {
                    (None, EnvSource::Resource(r.resource.clone()))
                } else {
                    (None, EnvSource::Literal)
                }
            }
        };
        env.retain(|row| row.name != e.name);
        env.push(EnvRow {
            name: e.name.clone(),
            value,
            source,
        });
    }

    let quantities =
        |m: Option<&BTreeMap<String, k8s_openapi::apimachinery::pkg::api::resource::Quantity>>| {
            m.map(|m| m.iter().map(|(k, v)| (k.clone(), v.0.clone())).collect())
                .unwrap_or_default()
        };
    let resources = c.resources.as_ref();

    let mut probes = Vec::new();
    for (label, probe) in [
        ("Liveness", &c.liveness_probe),
        ("Readiness", &c.readiness_probe),
        ("Startup", &c.startup_probe),
    ] {
        if let Some(p) = probe {
            probes.push((label.to_string(), describe_probe(p)));
        }
    }

    let last_termination = status
        .and_then(|s| s.last_state.as_ref())
        .and_then(|s| s.terminated.as_ref())
        .map(|t| {
            format!(
                "{} (exit {}) {}",
                t.reason.clone().unwrap_or_else(|| "Terminated".into()),
                t.exit_code,
                t.finished_at
                    .as_ref()
                    .map(|f| format!("{} ago", age(Some(f))))
                    .unwrap_or_default()
            )
        })
        .unwrap_or_default();

    ContainerDetails {
        name: c.name.clone(),
        init,
        image: c.image.clone().unwrap_or_default(),
        pull_policy: c.image_pull_policy.clone().unwrap_or_default(),
        command: c.command.clone().unwrap_or_default(),
        args: c.args.clone().unwrap_or_default(),
        working_dir: c.working_dir.clone().unwrap_or_default(),
        ports: c
            .ports
            .iter()
            .flatten()
            .map(|p| {
                let proto = p.protocol.clone().unwrap_or_else(|| "TCP".into());
                match &p.name {
                    Some(n) => format!("{n} {}/{proto}", p.container_port),
                    None => format!("{}/{proto}", p.container_port),
                }
            })
            .collect(),
        env,
        env_from_secrets,
        requests: quantities(resources.and_then(|r| r.requests.as_ref())),
        limits: quantities(resources.and_then(|r| r.limits.as_ref())),
        mounts: c
            .volume_mounts
            .iter()
            .flatten()
            .map(|m| {
                let mut source = m.name.clone();
                if let Some(sub) = m.sub_path.as_ref().filter(|s| !s.is_empty()) {
                    source = format!("{source}/{sub}");
                }
                if m.read_only == Some(true) {
                    source.push_str(" (ro)");
                }
                (m.mount_path.clone(), source)
            })
            .collect(),
        probes,
        last_termination,
    }
}

fn describe_probe(p: &Probe) -> String {
    let port = |p: &IntOrString| match p {
        IntOrString::Int(i) => i.to_string(),
        IntOrString::String(s) => s.clone(),
    };
    let action = if let Some(h) = &p.http_get {
        format!(
            "HTTP {} :{}{}",
            h.scheme.clone().unwrap_or_else(|| "HTTP".into()),
            port(&h.port),
            h.path.clone().unwrap_or_default()
        )
    } else if let Some(t) = &p.tcp_socket {
        format!("TCP :{}", port(&t.port))
    } else if let Some(e) = &p.exec {
        format!("exec {}", e.command.clone().unwrap_or_default().join(" "))
    } else if let Some(g) = &p.grpc {
        format!("gRPC :{}", g.port)
    } else {
        "unknown".into()
    };
    format!(
        "{action} · delay {}s · every {}s · timeout {}s · fail after {}",
        p.initial_delay_seconds.unwrap_or(0),
        p.period_seconds.unwrap_or(10),
        p.timeout_seconds.unwrap_or(1),
        p.failure_threshold.unwrap_or(3)
    )
}

// ---------------------------------------------------------------------------
// Services
// ---------------------------------------------------------------------------

async fn service_details(client: &Client, ns: &str, name: &str) -> Result<ServiceDetails> {
    let svc = Api::<Service>::namespaced(client.clone(), ns)
        .get(name)
        .await?;
    let spec = svc.spec.clone().unwrap_or_default();

    let mut endpoint_ports = BTreeSet::new();
    let mut endpoints = Vec::new();
    let slices = Api::<EndpointSlice>::namespaced(client.clone(), ns)
        .list(&ListParams::default().labels(&format!("kubernetes.io/service-name={name}")))
        .await;
    match slices {
        Ok(slices) => {
            for slice in &slices.items {
                for p in slice.ports.iter().flatten() {
                    endpoint_ports.insert(port_label(
                        p.name.as_deref(),
                        p.port,
                        p.protocol.as_deref(),
                    ));
                }
                for e in slice.endpoints.iter().flatten() {
                    let target = e.target_ref.as_ref().map(target_label).unwrap_or_default();
                    for ip in &e.addresses {
                        endpoints.push(EndpointRow {
                            ip: ip.clone(),
                            target: target.clone(),
                            node: e.node_name.clone().unwrap_or_default(),
                            ready: e.conditions.as_ref().and_then(|c| c.ready).unwrap_or(true),
                        });
                    }
                }
            }
        }
        // Some RBAC setups grant the legacy Endpoints API but not EndpointSlices.
        Err(_) => {
            #[allow(deprecated)]
            let legacy = Api::<Endpoints>::namespaced(client.clone(), ns)
                .get_opt(name)
                .await?;
            for subset in legacy.iter().flat_map(|e| e.subsets.iter().flatten()) {
                for p in subset.ports.iter().flatten() {
                    endpoint_ports.insert(port_label(
                        p.name.as_deref(),
                        Some(p.port),
                        p.protocol.as_deref(),
                    ));
                }
                let groups = [
                    (&subset.addresses, true),
                    (&subset.not_ready_addresses, false),
                ];
                for (addresses, ready) in groups {
                    for a in addresses.iter().flatten() {
                        endpoints.push(EndpointRow {
                            ip: a.ip.clone(),
                            target: a.target_ref.as_ref().map(target_label).unwrap_or_default(),
                            node: a.node_name.clone().unwrap_or_default(),
                            ready,
                        });
                    }
                }
            }
        }
    }
    endpoints.sort_by(|a, b| (!a.ready, &a.target).cmp(&(!b.ready, &b.target)));

    Ok(ServiceDetails {
        session_affinity: spec
            .session_affinity
            .clone()
            .unwrap_or_else(|| "None".into()),
        external_traffic_policy: spec.external_traffic_policy.clone().unwrap_or_default(),
        internal_traffic_policy: spec.internal_traffic_policy.clone().unwrap_or_default(),
        ip_families: spec.ip_families.clone().unwrap_or_default().join(", "),
        endpoint_ports: endpoint_ports.into_iter().collect(),
        endpoints,
        labels: pairs(svc.metadata.labels.as_ref()),
        annotations: pairs(svc.metadata.annotations.as_ref()),
    })
}

fn port_label(name: Option<&str>, port: Option<i32>, protocol: Option<&str>) -> String {
    let port = port.map(|p| p.to_string()).unwrap_or_default();
    let proto = protocol.unwrap_or("TCP");
    match name {
        Some(n) if !n.is_empty() => format!("{n} {port}/{proto}"),
        _ => format!("{port}/{proto}"),
    }
}

fn target_label(r: &k8s_openapi::api::core::v1::ObjectReference) -> String {
    format!(
        "{}/{}",
        r.kind.clone().unwrap_or_default().to_lowercase(),
        r.name.clone().unwrap_or_default()
    )
}

// ---------------------------------------------------------------------------
// Deployments
// ---------------------------------------------------------------------------

async fn deployment_details(client: &Client, ns: &str, name: &str) -> Result<DeploymentDetails> {
    let d = Api::<Deployment>::namespaced(client.clone(), ns)
        .get(name)
        .await?;
    Ok(DeploymentDetails {
        paused: d.spec.paused.unwrap_or(false),
        min_ready_seconds: d.spec.min_ready_seconds.unwrap_or(0),
        revision_history: d.spec.revision_history_limit.unwrap_or(10),
        conditions: d
            .status
            .as_ref()
            .and_then(|s| s.conditions.as_ref())
            .into_iter()
            .flatten()
            .map(|c| Condition {
                type_: c.type_.clone().unwrap_or_default(),
                ok: conditions_ok(c.status.as_deref().unwrap_or_default()),
                reason: c.reason.clone().unwrap_or_default(),
                message: c.message.clone().unwrap_or_default(),
            })
            .collect(),
        labels: pairs(Some(d.labels())),
        annotations: pairs(Some(d.annotations()))
            .into_iter()
            // The last-applied blob is huge and duplicates the YAML view.
            .filter(|(k, _)| k != "kubectl.kubernetes.io/last-applied-configuration")
            .collect(),
    })
}

// ---------------------------------------------------------------------------
// Other workloads
// ---------------------------------------------------------------------------

/// Read from the object's JSON so one function covers every workload kind.
async fn workload_details(
    client: &Client,
    kind: Kind,
    ns: &str,
    name: &str,
) -> Result<WorkloadDetails> {
    let v = crate::kube_ops::get_object(client.clone(), kind, ns, name).await?;
    let str_map = |key: &str| -> Vec<(String, String)> {
        v["metadata"][key]
            .as_object()
            .map(|m| {
                m.iter()
                    .map(|(k, v)| (k.clone(), v.as_str().unwrap_or_default().to_string()))
                    .collect()
            })
            .unwrap_or_default()
    };
    let text = |c: &serde_json::Value, key: &str| c[key].as_str().unwrap_or_default().to_string();
    Ok(WorkloadDetails {
        conditions: v["status"]["conditions"]
            .as_array()
            .into_iter()
            .flatten()
            .map(|c| Condition {
                type_: text(c, "type"),
                ok: conditions_ok(&text(c, "status")),
                reason: text(c, "reason"),
                message: text(c, "message"),
            })
            .collect(),
        labels: str_map("labels"),
        annotations: str_map("annotations")
            .into_iter()
            .filter(|(k, _)| k != "kubectl.kubernetes.io/last-applied-configuration")
            .collect(),
    })
}
