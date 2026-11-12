//! Checking GitHub for a newer release and installing it with `scripts/install.sh`.
//!
//! The latest tag comes from where `github.com/<repo>/releases/latest` redirects (no API
//! rate limit). Installing runs the published install script detached, with its output in a
//! log file: the script quits this app, replaces the bundle and reopens it.

use std::{
    fs::File,
    path::PathBuf,
    process::{Command, Stdio},
};

pub const REPO: &str = "cardeer/kube-ui";
pub const CURRENT: &str = env!("CARGO_PKG_VERSION");

#[derive(Default)]
pub enum UpdateState {
    #[default]
    Idle,
    Checking {
        manual: bool,
    },
    UpToDate,
    /// A newer release, by tag (e.g. `v0.2.0`).
    Available(String),
    Installing(String),
    /// The installer finished but this process is still running (it couldn't quit us):
    /// the new version starts on the next launch.
    Installed(String),
    // Only shown in the top bar's hover text, which macOS replaces with the menu bar.
    Failed(#[cfg_attr(target_os = "macos", allow(dead_code))] String),
}

/// The latest release's tag, e.g. `v0.2.0`.
pub async fn latest_tag() -> Result<String, String> {
    let url = format!("https://github.com/{REPO}/releases/latest");
    let out = tokio::process::Command::new("/usr/bin/curl")
        .args([
            "-fsSIL",
            "--max-time",
            "15",
            "-o",
            "/dev/null",
            "-w",
            "%{url_effective}",
            &url,
        ])
        .output()
        .await
        .map_err(|e| format!("Could not run curl: {e}"))?;
    if !out.status.success() {
        return Err("Could not reach GitHub.".into());
    }
    let effective = String::from_utf8_lossy(&out.stdout);
    effective
        .rsplit_once("/tag/")
        .map(|(_, tag)| tag.trim().to_string())
        .filter(|t| !t.is_empty())
        .ok_or_else(|| "No release found.".into())
}

/// True when `tag` (`v1.2.3` or `1.2.3`) is a higher version than `current`.
pub fn is_newer(tag: &str, current: &str) -> bool {
    fn parts(v: &str) -> Vec<u64> {
        v.trim_start_matches('v')
            .split(['.', '-', '+'])
            .take(3)
            .map(|p| p.parse().unwrap_or(0))
            .collect()
    }
    parts(tag) > parts(current)
}

/// Where the installer's output goes.
pub fn log_path() -> PathBuf {
    dirs::home_dir()
        .unwrap_or_default()
        .join("Library/Logs/kube-ui-update.log")
}

/// The folder holding the running `Kube UI.app`, so an update replaces this copy.
/// `None` when not running from a bundle (e.g. `cargo run`): the installer's default applies.
fn install_dir() -> Option<PathBuf> {
    app_bundle()?.parent().map(PathBuf::from)
}

/// The running `Kube UI.app`, when running from a bundle.
fn app_bundle() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    // .../Kube UI.app/Contents/MacOS/kube-ui
    let app = exe.parent()?.parent()?.parent()?;
    (app.extension()? == "app").then(|| app.to_path_buf())
}

/// Reopens the app shortly after this process exits (the caller then closes the window).
/// False when not running from a bundle.
pub fn relaunch() -> bool {
    let Some(app) = app_bundle() else {
        return false;
    };
    Command::new("/bin/sh")
        .arg("-c")
        .arg(r#"while kill -0 "$1" 2>/dev/null; do sleep 0.2; done; open "$0""#)
        .arg(&app)
        .arg(std::process::id().to_string())
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .is_ok()
}

/// Starts the installer for `tag`, detached. The receiver gets its outcome; normally the
/// installer quits and reopens the app first, so a result only arrives when it couldn't.
pub fn install(tag: &str) -> Result<std::sync::mpsc::Receiver<Result<(), String>>, String> {
    let log = log_path();
    if let Some(dir) = log.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let out = File::create(&log).map_err(|e| format!("Could not write {}: {e}", log.display()))?;
    let err = out.try_clone().map_err(|e| e.to_string())?;

    let script = format!("https://raw.githubusercontent.com/{REPO}/main/scripts/install.sh");
    let mut args = vec!["--version".to_string(), tag.to_string()];
    if let Some(dir) = install_dir() {
        args.push("--dir".into());
        args.push(dir.display().to_string());
    }
    // Arguments go through bash's positional parameters, never into the command string.
    let mut child = Command::new("/bin/bash")
        .arg("-c")
        .arg(r#"set -o pipefail; curl -fsSL "$0" | bash -s -- "$@""#)
        .arg(&script)
        .args(&args)
        .stdin(Stdio::null())
        .stdout(out)
        .stderr(err)
        .spawn()
        .map_err(|e| format!("Could not start the installer: {e}"))?;

    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let ok = child.wait().map(|s| s.success()).unwrap_or(false);
        let result = if ok {
            Ok(())
        } else {
            let tail = std::fs::read_to_string(log_path()).unwrap_or_default();
            let last = tail
                .lines()
                .rev()
                .find(|l| !l.trim().is_empty())
                .unwrap_or("")
                .to_string();
            Err(if last.is_empty() {
                "The installer failed.".into()
            } else {
                strip_ansi(&last)
            })
        };
        let _ = tx.send(result);
    });
    Ok(rx)
}

fn strip_ansi(s: &str) -> String {
    let mut out = String::new();
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c == '\u{1b}' {
            for c in chars.by_ref() {
                if c.is_ascii_alphabetic() {
                    break;
                }
            }
        } else {
            out.push(c);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versions_compare_numerically() {
        assert!(is_newer("v0.2.0", "0.1.0"));
        assert!(is_newer("v0.10.0", "0.9.3"));
        assert!(is_newer("1.0.0", "0.99.99"));
        assert!(!is_newer("v0.1.0", "0.1.0"));
        assert!(!is_newer("v0.0.9", "0.1.0"));
    }

    #[test]
    fn strips_colours() {
        assert_eq!(strip_ansi("\u{1b}[31mError:\u{1b}[0m nope"), "Error: nope");
    }
}
