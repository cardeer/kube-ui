use std::{
    collections::{BTreeMap, HashMap},
    fs,
    path::PathBuf,
};

use serde::{Deserialize, Serialize};

use crate::kube_ops::PfTarget;

/// A port forward the user created; kept across restarts.
#[derive(Serialize, Deserialize, Clone)]
pub struct SavedForward {
    pub kubeconfig: PathBuf,
    pub context: String,
    pub namespace: String,
    pub target: PfTarget,
    pub remote_port: u16,
    /// 0 means "pick a random port".
    pub local_port: u16,
    /// Was running when last saved; restarted on launch.
    #[serde(default)]
    pub active: bool,
}

impl SavedForward {
    pub fn same_target(&self, other: &Self) -> bool {
        self.kubeconfig == other.kubeconfig
            && self.context == other.context
            && self.namespace == other.namespace
            && self.target == other.target
            && self.remote_port == other.remote_port
            && (self.local_port == other.local_port || other.local_port == 0)
    }
}

/// An open cluster tab; restored on launch.
#[derive(Serialize, Deserialize, Clone, PartialEq)]
pub struct SavedWorkspace {
    pub kubeconfig: PathBuf,
    #[serde(default)]
    pub context: Option<String>,
    /// Selected namespaces; empty with `all_namespaces` means all.
    #[serde(default)]
    pub namespaces: Vec<String>,
    #[serde(default)]
    pub all_namespaces: bool,
    /// Single namespace saved by older versions; read once, then replaced by `namespaces`.
    #[serde(default, skip_serializing)]
    pub namespace: Option<String>,
    /// Name filter per page ("pods", "services", "deployments").
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub filters: BTreeMap<String, String>,
    /// Sorted column per page.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub sorts: BTreeMap<String, SavedSort>,
    /// Table filters are regular expressions.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub filter_regex: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SavedSort {
    pub column: String,
    #[serde(default)]
    pub descending: bool,
}

impl SavedWorkspace {
    /// The selection to restore: `Some(vec![])` is all namespaces, `None` means use defaults.
    pub fn selection(&self) -> Option<Vec<String>> {
        if self.all_namespaces {
            Some(Vec::new())
        } else if !self.namespaces.is_empty() {
            Some(self.namespaces.clone())
        } else {
            self.namespace.clone().map(|ns| vec![ns])
        }
    }
}

/// How the bottom log panel is shown.
#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Default)]
pub enum LogPanelMode {
    /// Resizable, at the height the user dragged it to.
    #[default]
    Normal,
    /// Fills most of the window.
    Expanded,
    /// Only the log tab strip, docked at the bottom.
    Collapsed,
}

/// Persisted app settings, stored as JSON in the platform config dir.
#[derive(Serialize, Deserialize)]
pub struct Settings {
    /// Every kubeconfig file the user has added.
    #[serde(default)]
    pub kubeconfigs: Vec<PathBuf>,
    /// Kubeconfig in use before cluster tabs existed; migrated into `workspaces` on load.
    #[serde(default, skip_serializing)]
    pub active: Option<PathBuf>,
    #[serde(default)]
    pub workspaces: Vec<SavedWorkspace>,
    #[serde(default)]
    pub active_workspace: usize,
    /// Last selected context, keyed by kubeconfig path.
    #[serde(default)]
    pub last_context: HashMap<String, String>,
    /// Last selected namespaces (comma-separated), keyed by context name. Empty string means "all namespaces".
    #[serde(default)]
    pub last_namespace: HashMap<String, String>,
    /// Keep lists current with a watch. Older settings files call this `auto_refresh`.
    #[serde(default = "default_true", alias = "auto_refresh")]
    pub live_updates: bool,
    #[serde(default)]
    pub forwards: Vec<SavedForward>,
    /// Sidebar groups the user folded away ("workloads", "network").
    #[serde(default)]
    pub nav_collapsed: Vec<String>,
    /// Log panel height in normal mode.
    #[serde(default = "default_log_height")]
    pub log_height: f32,
    #[serde(default)]
    pub log_mode: LogPanelMode,
    /// Show the current log tab and the current terminal tab side by side.
    #[serde(default)]
    pub log_split: bool,
    /// Window inner size from the last run (the size to restore to when it was maximized).
    #[serde(default)]
    pub window_size: Option<[f32; 2]>,
    #[serde(default)]
    pub window_maximized: bool,
    /// Version that last ran; "What's new" opens once when it differs from the running one.
    #[serde(default)]
    pub last_seen_version: Option<String>,
}

fn default_log_height() -> f32 {
    380.0
}

fn default_true() -> bool {
    true
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            kubeconfigs: Vec::new(),
            active: None,
            workspaces: Vec::new(),
            active_workspace: 0,
            last_context: HashMap::new(),
            last_namespace: HashMap::new(),
            live_updates: true,
            forwards: Vec::new(),
            nav_collapsed: Vec::new(),
            log_height: default_log_height(),
            log_mode: LogPanelMode::Normal,
            log_split: false,
            window_size: None,
            window_maximized: false,
            last_seen_version: None,
        }
    }
}

impl Settings {
    pub fn path() -> Option<PathBuf> {
        dirs::config_dir().map(|d| d.join("kube-ui").join("settings.json"))
    }

    pub fn load() -> Self {
        Self::path()
            .and_then(|p| fs::read_to_string(p).ok())
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default()
    }

    pub fn save(&self) {
        let Some(path) = Self::path() else { return };
        if let Some(dir) = path.parent() {
            let _ = fs::create_dir_all(dir);
        }
        if let Ok(json) = serde_json::to_string_pretty(self) {
            if let Err(e) = fs::write(&path, json) {
                eprintln!("failed to save settings to {}: {e}", path.display());
            }
        }
    }
}
