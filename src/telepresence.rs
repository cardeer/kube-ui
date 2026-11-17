//! Telepresence, driven through its CLI (`telepresence status/list/connect/intercept/...`).
//!
//! JSON output differs between releases (snake_case or camelCase keys, a `{"stdout": ...}`
//! wrapper on some), so parsing reads loosely and ignores anything it doesn't know.

use std::path::Path;

use serde_json::Value;
use tokio::process::Command;

const BIN: &str = "telepresence";

/// Why a command didn't run.
#[derive(Clone, Debug)]
pub enum TpError {
    /// The `telepresence` binary isn't on PATH.
    NotInstalled,
    Failed(String),
}

impl TpError {
    pub fn text(&self) -> String {
        match self {
            TpError::NotInstalled => "Telepresence isn't installed".into(),
            TpError::Failed(e) => e.clone(),
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct Status {
    pub version: String,
    pub connected: bool,
    /// What the user daemon reports, e.g. "Connected" or "Not connected".
    pub status: String,
    pub context: String,
    pub namespace: String,
    pub server: String,
    pub manager_version: String,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Workload {
    pub name: String,
    pub namespace: String,
    pub kind: String,
    /// The traffic agent is installed.
    pub agent: bool,
    pub intercepts: Vec<Intercept>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Intercept {
    pub name: String,
    /// Service port name or number being intercepted.
    pub port: String,
    /// Local port the traffic goes to.
    pub local_port: String,
    /// e.g. "ACTIVE", or an error disposition.
    pub disposition: String,
}

/// The status and, when connected, the workloads in the connected namespace.
#[derive(Clone, Debug, Default)]
pub struct Snapshot {
    pub status: Status,
    pub workloads: Vec<Workload>,
    pub list_error: Option<String>,
}

async fn run(args: &[&str]) -> Result<String, TpError> {
    let out = Command::new(BIN)
        .args(args)
        .stdin(std::process::Stdio::null())
        .output()
        .await
        .map_err(|e| match e.kind() {
            std::io::ErrorKind::NotFound => TpError::NotInstalled,
            _ => TpError::Failed(e.to_string()),
        })?;
    let stdout = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if out.status.success() {
        return Ok(stdout);
    }
    let stderr = String::from_utf8_lossy(&out.stderr).trim().to_string();
    let msg = [stderr, stdout]
        .into_iter()
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join("\n");
    Err(TpError::Failed(if msg.is_empty() {
        format!("telepresence {} failed ({})", args.join(" "), out.status)
    } else {
        msg
    }))
}

pub async fn snapshot() -> Result<Snapshot, TpError> {
    let status = parse_status(&run(&["status", "--output", "json"]).await?)
        .map_err(TpError::Failed)?;
    let mut snap = Snapshot {
        status,
        ..Default::default()
    };
    // Status only reports the version once the daemon runs; ask the client otherwise.
    if snap.status.version.is_empty() {
        if let Ok(out) = run(&["version"]).await {
            snap.status.version = client_version(&out);
        }
    }
    if snap.status.connected {
        match run(&["list", "--output", "json"]).await {
            Ok(out) => match parse_list(&out) {
                Ok(w) => snap.workloads = w,
                Err(e) => snap.list_error = Some(e),
            },
            Err(e) => snap.list_error = Some(e.text()),
        }
    }
    Ok(snap)
}

/// The `connect` command line, also shown to the user to run in a terminal (the first
/// connect on macOS asks for a sudo password, which needs one).
pub fn connect_args(kubeconfig: &Path, context: &str, namespace: Option<&str>) -> Vec<String> {
    let mut args = vec![
        "connect".to_string(),
        "--kubeconfig".into(),
        kubeconfig.display().to_string(),
        "--context".into(),
        context.into(),
    ];
    if let Some(ns) = namespace {
        args.push("--namespace".into());
        args.push(ns.into());
    }
    args
}

/// A shell-quoted command line for the terminal.
pub fn command_line(args: &[String]) -> String {
    std::iter::once(BIN.to_string())
        .chain(args.iter().map(|a| shell_quote(a)))
        .collect::<Vec<_>>()
        .join(" ")
}

fn shell_quote(s: &str) -> String {
    if !s.is_empty()
        && s
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "-_./:=@,+".contains(c))
    {
        s.to_string()
    } else {
        format!("'{}'", s.replace('\'', r"'\''"))
    }
}

pub async fn connect(args: Vec<String>) -> Result<String, TpError> {
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    run(&args).await
}

pub async fn quit() -> Result<String, TpError> {
    run(&["quit"]).await
}

/// `port` is `<local>` or `<local>:<service port name or number>`.
pub async fn intercept(workload: String, port: String) -> Result<String, TpError> {
    run(&["intercept", &workload, "--port", &port, "--mount=false"]).await
}

pub async fn leave(name: String) -> Result<String, TpError> {
    run(&["leave", &name]).await
}

// ---------------------------------------------------------------------------
// Parsing
// ---------------------------------------------------------------------------

/// Some releases wrap structured output as `{"cmd": ..., "stdout": <value>}`.
fn unwrap(v: Value) -> Value {
    match v {
        Value::Object(mut o) if o.contains_key("stdout") && o.contains_key("cmd") => {
            match o.remove("stdout").unwrap_or(Value::Null) {
                Value::String(s) => serde_json::from_str(&s).unwrap_or(Value::String(s)),
                other => other,
            }
        }
        other => other,
    }
}

/// The first of `keys` present on `v`.
fn field<'a>(v: &'a Value, keys: &[&str]) -> Option<&'a Value> {
    keys.iter().find_map(|k| v.get(*k).filter(|x| !x.is_null()))
}

fn string(v: &Value, keys: &[&str]) -> String {
    match field(v, keys) {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Number(n)) => n.to_string(),
        Some(Value::Bool(b)) => b.to_string(),
        _ => String::new(),
    }
}

/// "OSS Client : v2.33.1" (plain `telepresence version` output) → "v2.33.1".
fn client_version(out: &str) -> String {
    out.lines()
        .find(|l| l.contains("Client"))
        .and_then(|l| l.split_once(':'))
        .map(|(_, v)| v.trim().to_string())
        .unwrap_or_default()
}

pub fn parse_status(out: &str) -> Result<Status, String> {
    let v: Value = serde_json::from_str(out)
        .map_err(|e| format!("Unexpected output from telepresence status: {e}"))?;
    let v = unwrap(v);
    let user = field(&v, &["user_daemon", "userDaemon"]).unwrap_or(&Value::Null);
    let manager = field(&v, &["traffic_manager", "trafficManager"]).unwrap_or(&Value::Null);
    let status = string(user, &["status"]);
    let running = field(user, &["running"])
        .and_then(Value::as_bool)
        .unwrap_or(false);
    Ok(Status {
        version: string(user, &["version"]),
        connected: running && status.eq_ignore_ascii_case("connected"),
        status: if status.is_empty() {
            "Not connected".into()
        } else {
            status
        },
        context: string(user, &["kubernetes_context", "kubernetesContext", "context"]),
        namespace: string(user, &["namespace", "connected_namespace", "connectedNamespace"]),
        server: string(user, &["kubernetes_server", "kubernetesServer"]),
        manager_version: string(manager, &["version"]),
    })
}

pub fn parse_list(out: &str) -> Result<Vec<Workload>, String> {
    if out.trim().is_empty() {
        return Ok(Vec::new());
    }
    let v: Value = serde_json::from_str(out)
        .map_err(|e| format!("Unexpected output from telepresence list: {e}"))?;
    let v = unwrap(v);
    let items = match &v {
        Value::Array(a) => a.as_slice(),
        Value::Object(_) => match field(&v, &["workloads", "items"]) {
            Some(Value::Array(a)) => a.as_slice(),
            _ => std::slice::from_ref(&v),
        },
        _ => &[],
    };
    let mut workloads: Vec<Workload> = items
        .iter()
        .filter(|w| w.is_object())
        .map(|w| {
            let mut intercepts = Vec::new();
            match field(w, &["intercept_infos", "interceptInfos"]) {
                Some(Value::Array(a)) => intercepts.extend(a.iter().map(parse_intercept)),
                _ => {}
            }
            if let Some(i) = field(w, &["intercept_info", "interceptInfo"]) {
                intercepts.push(parse_intercept(i));
            }
            let agent = !string(w, &["agent_version", "agentVersion"]).is_empty()
                || field(w, &["agent_info", "agentInfo", "agent_infos"]).is_some();
            Workload {
                name: string(w, &["name"]),
                namespace: string(w, &["namespace"]),
                kind: kind_name(&string(
                    w,
                    &["workload_resource_type", "workloadResourceType", "kind"],
                )),
                agent,
                intercepts,
            }
        })
        .filter(|w| !w.name.is_empty())
        .collect();
    workloads.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(workloads)
}

/// Newer releases print kinds in capitals ("STATEFULSET"); show the Kubernetes spelling.
fn kind_name(kind: &str) -> String {
    match kind.to_ascii_lowercase().as_str() {
        "deployment" => "Deployment",
        "statefulset" => "StatefulSet",
        "replicaset" => "ReplicaSet",
        "daemonset" => "DaemonSet",
        "rollout" => "Rollout",
        "pod" => "Pod",
        _ => return kind.to_string(),
    }
    .into()
}

fn parse_intercept(i: &Value) -> Intercept {
    let spec = field(i, &["spec"]).unwrap_or(i);
    Intercept {
        name: string(spec, &["name"]),
        port: string(
            spec,
            &["port_identifier", "portIdentifier", "service_port_identifier", "servicePortIdentifier"],
        ),
        local_port: string(spec, &["target_port", "targetPort"]),
        disposition: string(i, &["disposition"]),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status() {
        let s = parse_status(
            r#"{"user_daemon":{"running":true,"name":"OSS User Daemon","version":"v2.20.0",
            "status":"Connected","kubernetes_server":"https://1.2.3.4","kubernetes_context":"dev",
            "namespace":"apps"},"traffic_manager":{"version":"v2.20.0"}}"#,
        )
        .unwrap();
        assert!(s.connected);
        assert_eq!(s.context, "dev");
        assert_eq!(s.namespace, "apps");
        assert_eq!(s.manager_version, "v2.20.0");

        let s = parse_status(r#"{"user_daemon":{"running":false,"status":"Not connected"}}"#)
            .unwrap();
        assert!(!s.connected);
        assert_eq!(s.status, "Not connected");
    }

    #[test]
    fn list() {
        let w = parse_list(
            r#"{"cmd":"list","stdout":[
              {"name":"web","namespace":"apps","workload_resource_type":"Deployment",
               "agent_version":"2.20.0","intercept_infos":[{"spec":{"name":"web",
               "port_identifier":"http","target_port":8080},"disposition":"ACTIVE"}]},
              {"name":"api","namespace":"apps","workloadResourceType":"Deployment"}]}"#,
        )
        .unwrap();
        assert_eq!(w.len(), 2);
        assert_eq!(w[0].name, "api");
        assert!(w[0].intercepts.is_empty() && !w[0].agent);
        assert_eq!(
            w[1].intercepts,
            vec![Intercept {
                name: "web".into(),
                port: "http".into(),
                local_port: "8080".into(),
                disposition: "ACTIVE".into(),
            }]
        );
        assert_eq!(kind_name("STATEFULSET"), "StatefulSet");
        assert_eq!(kind_name("CustomThing"), "CustomThing");
        assert!(parse_list("").unwrap().is_empty());
        assert!(parse_list("[]").unwrap().is_empty());
    }

    #[test]
    fn version() {
        assert_eq!(
            client_version("OSS Client : v2.33.1\nRoot Daemon: not running\n"),
            "v2.33.1"
        );
    }

    #[test]
    fn quoting() {
        let args = connect_args(Path::new("/Users/me/my config"), "dev", Some("apps"));
        assert_eq!(
            command_line(&args),
            "telepresence connect --kubeconfig '/Users/me/my config' --context dev --namespace apps"
        );
    }
}
