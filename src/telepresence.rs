//! Telepresence, driven through its CLI (`telepresence status/list/connect/intercept/...`).
//!
//! JSON output differs between releases (snake_case or camelCase keys, a `{"stdout": ...}`
//! wrapper on some), so parsing reads loosely and ignores anything it doesn't know.

use std::{
    io::Write,
    os::unix::fs::{DirBuilderExt, OpenOptionsExt},
    path::{Path, PathBuf},
};

use serde_json::Value;
use tokio::process::Command;

const BIN: &str = "telepresence";

/// Why a command didn't run.
#[derive(Clone, Debug)]
pub enum TpError {
    /// The `telepresence` binary isn't on PATH.
    NotInstalled,
    /// Starting the root daemon needs the administrator password.
    NeedPassword,
    /// sudo rejected the password that was given.
    WrongPassword,
    Failed(String),
}

impl TpError {
    pub fn text(&self) -> String {
        match self {
            TpError::NotInstalled => "Telepresence isn't installed".into(),
            TpError::NeedPassword => "Administrator password required".into(),
            TpError::WrongPassword => "Incorrect password".into(),
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
    /// The root daemon is up, so connecting won't ask for a password.
    pub root_running: bool,
    /// Intercept names the daemon reports (any namespace).
    pub intercepts: Vec<String>,
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
    /// Extra detail from the traffic manager, usually about a failure.
    pub message: String,
    /// Where local traffic goes, when not localhost.
    pub host: String,
    /// The pod whose traffic is intercepted.
    pub pod: String,
}

/// The status and, when connected, the workloads in the connected namespace.
#[derive(Clone, Debug, Default)]
pub struct Snapshot {
    pub status: Status,
    pub workloads: Vec<Workload>,
    pub list_error: Option<String>,
}

async fn run(args: &[&str]) -> Result<String, TpError> {
    run_env(args, &[]).await
}

async fn run_env(args: &[&str], env: &[(&str, String)]) -> Result<String, TpError> {
    let out = Command::new(BIN)
        .args(args)
        .envs(env.iter().map(|(k, v)| (*k, v.as_str())))
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
    let lower = msg.to_lowercase();
    if lower.contains("incorrect password") {
        return Err(TpError::WrongPassword);
    }
    if [
        "a terminal is required",
        "no tty present",
        "a password is required",
        "no password was provided",
    ]
    .iter()
    .any(|m| lower.contains(m))
    {
        return Err(TpError::NeedPassword);
    }
    Err(TpError::Failed(if msg.is_empty() {
        format!("telepresence {} failed ({})", args.join(" "), out.status)
    } else {
        msg
    }))
}

pub async fn snapshot() -> Result<Snapshot, TpError> {
    let status = match run(&["status", "--output", "json"]).await {
        Ok(out) => parse_status(&out).map_err(TpError::Failed)?,
        // After a connect that couldn't start the root daemon, status fails on its missing
        // state file until the daemons are stopped; that's still "not connected".
        Err(TpError::Failed(e)) if e.contains("daemon.json") => Status {
            status: "Not connected".into(),
            ..Default::default()
        },
        Err(e) => return Err(e),
    };
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

/// Connects. The first connect starts the root daemon through `sudo`; with no terminal
/// to prompt on, sudo asks the `SUDO_ASKPASS` helper instead, which hands it `password`.
/// Without a password, a connect that would need one fails early with `NeedPassword`.
pub async fn connect(
    args: Vec<String>,
    password: Option<String>,
    root_running: bool,
) -> Result<String, TpError> {
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    let result = match password {
        None => {
            if !root_running && !sudo_without_password().await {
                return Err(TpError::NeedPassword);
            }
            run(&args).await
        }
        Some(password) => {
            // Telepresence doesn't pass sudo's messages on, so a wrong password would only
            // surface as a daemon launch failure. Check it first.
            if !password_ok(&password).await {
                return Err(TpError::WrongPassword);
            }
            let askpass = Askpass::new(&password).map_err(|e| {
                TpError::Failed(format!("Couldn't prepare the password helper: {e}"))
            })?;
            let mut env = vec![("SUDO_ASKPASS", askpass.script.display().to_string())];
            // sudo only turns to the helper when DISPLAY is set.
            if std::env::var_os("DISPLAY").is_none() {
                env.push(("DISPLAY", ":0".into()));
            }
            run_env(&args, &env).await
        }
    };
    if result.is_err() && !root_running {
        // A failed start leaves a half-started daemon behind that breaks `status`.
        let _ = run(&["quit", "-s"]).await;
    }
    result
}

/// sudo is set up to run without a password (NOPASSWD). Cached credentials don't count:
/// they're tied to this process, not to the sudo Telepresence runs.
async fn sudo_without_password() -> bool {
    Command::new("sudo")
        .args(["-n", "-k", "true"])
        .stdin(std::process::Stdio::null())
        .output()
        .await
        .is_ok_and(|o| o.status.success())
}

/// Asks sudo whether `password` is right, without running anything.
async fn password_ok(password: &str) -> bool {
    use tokio::io::AsyncWriteExt;
    let Ok(mut child) = Command::new("sudo")
        .args(["-S", "-k", "-v", "-p", ""])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
    else {
        return false;
    };
    if let Some(mut stdin) = child.stdin.take() {
        let _ = stdin.write_all(format!("{password}\n").as_bytes()).await;
    }
    child.wait().await.is_ok_and(|s| s.success())
}

/// A private folder holding the password and a script that prints it, for `SUDO_ASKPASS`.
/// Removed when dropped.
struct Askpass {
    dir: PathBuf,
    script: PathBuf,
}

impl Askpass {
    fn new(password: &str) -> std::io::Result<Self> {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let dir = std::env::temp_dir().join(format!(
            "kube-ui-askpass-{}-{nanos}",
            std::process::id()
        ));
        std::fs::DirBuilder::new().mode(0o700).create(&dir)?;
        let this = Askpass {
            script: dir.join("askpass"),
            dir,
        };
        let write = |path: &Path, mode: u32, data: &[u8]| -> std::io::Result<()> {
            std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(mode)
                .open(path)?
                .write_all(data)
        };
        write(&this.dir.join("pass"), 0o600, password.as_bytes())?;
        write(
            &this.script,
            0o700,
            b"#!/bin/sh\ncat \"$(dirname \"$0\")/pass\"\n",
        )?;
        Ok(this)
    }
}

impl Drop for Askpass {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
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
    let root = field(&v, &["root_daemon", "rootDaemon"]).unwrap_or(&Value::Null);
    let intercepts = match field(user, &["intercepts"]) {
        Some(Value::Array(a)) => a
            .iter()
            .map(|i| match i {
                Value::String(s) => s.clone(),
                _ => string(i, &["name"]),
            })
            .filter(|n| !n.is_empty())
            .collect(),
        _ => Vec::new(),
    };
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
        root_running: field(root, &["running"])
            .and_then(Value::as_bool)
            .unwrap_or(false),
        intercepts,
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
            // An array in most releases (under either name), a single object in some.
            let mut intercepts = Vec::new();
            for key in [["intercept_infos", "interceptInfos"], ["intercept_info", "interceptInfo"]] {
                match field(w, &key) {
                    Some(Value::Array(a)) => intercepts.extend(a.iter().map(parse_intercept)),
                    Some(i @ Value::Object(_)) => intercepts.push(parse_intercept(i)),
                    _ => {}
                }
            }
            intercepts.retain(|i| !i.name.is_empty());
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

/// The disposition as its name; newer releases send the enum's number.
fn disposition_name(v: Option<&Value>) -> String {
    const NAMES: [&str; 10] = [
        "UNSPECIFIED",
        "ACTIVE",
        "WAITING",
        "NO_CLIENT",
        "NO_AGENT",
        "NO_MECHANISM",
        "NO_PORTS",
        "AGENT_ERROR",
        "BAD_ARGS",
        "REMOVED",
    ];
    match v {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Number(n)) => n
            .as_u64()
            .and_then(|n| NAMES.get(n as usize))
            .map(|s| s.to_string())
            .unwrap_or_else(|| n.to_string()),
        _ => String::new(),
    }
}

fn parse_intercept(i: &Value) -> Intercept {
    let spec = field(i, &["spec"]).unwrap_or(i);
    let mut port = string(
        spec,
        &["port_identifier", "portIdentifier", "service_port_identifier", "servicePortIdentifier"],
    );
    let number = string(spec, &["service_port", "servicePort"]);
    if port.is_empty() {
        port = number;
    } else if !number.is_empty() && number != port {
        port = format!("{port} ({number})");
    }
    Intercept {
        name: string(spec, &["name"]),
        port,
        local_port: string(spec, &["target_port", "targetPort"]),
        disposition: disposition_name(field(i, &["disposition"])),
        pod: string(i, &["pod_name", "podName"]),
        message: string(i, &["message"]),
        host: string(spec, &["target_host", "targetHost"]),
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
            "namespace":"apps","intercepts":[{"name":"web","client":"me@host"}]},
            "root_daemon":{"running":true},"traffic_manager":{"version":"v2.20.0"}}"#,
        )
        .unwrap();
        assert!(s.connected);
        assert_eq!(s.context, "dev");
        assert_eq!(s.namespace, "apps");
        assert_eq!(s.manager_version, "v2.20.0");
        assert!(s.root_running);
        assert_eq!(s.intercepts, vec!["web".to_string()]);

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
                ..Default::default()
            }]
        );
        assert_eq!(kind_name("STATEFULSET"), "StatefulSet");
        assert_eq!(kind_name("CustomThing"), "CustomThing");
        assert!(parse_list("").unwrap().is_empty());
        assert!(parse_list("[]").unwrap().is_empty());
    }

    /// The shape 2.33 prints: `intercept_info` is an array and `disposition` a number.
    #[test]
    fn list_2_33() {
        let w = parse_list(
            r#"[{"name":"admin-api-app","namespace":"ncs-dev","workload_resource_type":"DEPLOYMENT",
              "intercept_info":[{"spec":{"name":"admin-api-app","target_host":"127.0.0.1",
              "port_identifier":"admin-api","service_port":80,"target_port":6969},
              "disposition":1,"pod_name":"admin-api-app-86d5f49b4b-nc6v4"}]},
              {"name":"other","namespace":"ncs-dev","workload_resource_type":"DEPLOYMENT"}]"#,
        )
        .unwrap();
        assert_eq!(w[0].kind, "Deployment");
        assert_eq!(
            w[0].intercepts,
            vec![Intercept {
                name: "admin-api-app".into(),
                port: "admin-api (80)".into(),
                local_port: "6969".into(),
                disposition: "ACTIVE".into(),
                host: "127.0.0.1".into(),
                pod: "admin-api-app-86d5f49b4b-nc6v4".into(),
                ..Default::default()
            }]
        );
        assert!(w[1].intercepts.is_empty());
    }

    #[test]
    fn version() {
        assert_eq!(
            client_version("OSS Client : v2.33.1\nRoot Daemon: not running\n"),
            "v2.33.1"
        );
    }

    #[test]
    fn askpass_prints_password() {
        let a = Askpass::new("p@ss 'word\"").unwrap();
        let out = std::process::Command::new(&a.script).output().unwrap();
        assert_eq!(out.stdout, b"p@ss 'word\"");
        let dir = a.dir.clone();
        drop(a);
        assert!(!dir.exists());
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
