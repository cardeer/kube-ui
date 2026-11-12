mod app;
mod details;
mod kube_ops;
mod settings;

use eframe::egui;

const MIN_WINDOW_SIZE: [f32; 2] = [1120.0, 600.0];

fn main() -> eframe::Result {
    inherit_shell_path();
    // Reopen at the size the window had last time.
    let saved = settings::Settings::load();
    let size = saved
        .window_size
        .map(|[w, h]| [w.max(MIN_WINDOW_SIZE[0]), h.max(MIN_WINDOW_SIZE[1])])
        .unwrap_or([1360.0, 860.0]);
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("Kube UI")
            .with_inner_size(size)
            .with_maximized(saved.window_maximized)
            .with_min_inner_size(MIN_WINDOW_SIZE)
            // eframe otherwise replaces the Dock icon with the egui logo while running.
            .with_icon(
                eframe::icon_data::from_png_bytes(include_bytes!("../assets/icon.png"))
                    .expect("bundled icon is a valid PNG"),
            ),
        ..Default::default()
    };
    eframe::run_native(
        "Kube UI",
        options,
        Box::new(|cc| Ok(Box::new(app::App::new(cc)))),
    )
}

/// Apps launched from Finder get a minimal PATH, so kubeconfig exec plugins
/// (aws, kubelogin, gke-gcloud-auth-plugin, …) wouldn't be found. Borrow the login shell's.
/// Must run before any threads start.
fn inherit_shell_path() {
    if std::env::var_os("TERM").is_some() {
        return; // Started from a terminal: PATH is already right.
    }
    let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/zsh".into());
    let Ok(out) = std::process::Command::new(shell)
        .args(["-l", "-c", "printf '\\n__KUBE_UI_PATH__%s' \"$PATH\""])
        .output()
    else {
        return;
    };
    let stdout = String::from_utf8_lossy(&out.stdout);
    if let Some(path) = stdout
        .rsplit("__KUBE_UI_PATH__")
        .next()
        .filter(|p| !p.is_empty() && stdout.contains("__KUBE_UI_PATH__"))
    {
        // SAFETY: called first thing in main, before any other threads exist.
        unsafe { std::env::set_var("PATH", path.trim()) };
    }
}
