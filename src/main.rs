mod app;
mod details;
mod kube_ops;
mod settings;

use gpui_kit::{AppContext as _, Bounds, WindowBounds, WindowOptions, px, size};

fn main() {
    inherit_shell_path();
    // Reopen at the size the window had last time.
    let saved = settings::Settings::load();
    let min = app::MIN_WINDOW_SIZE;
    let default = app::DEFAULT_WINDOW_SIZE;
    let [w, h] = saved
        .window_size
        .map(|[w, h]| [w.max(min.width), h.max(min.height)])
        .unwrap_or([default.width, default.height]);

    gpui_kit::application()
        .with_assets(gpui_kit::assets::Assets)
        .run(move |cx| {
            gpui_kit::init(cx);
            app::install(cx);
            let options = WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(Bounds::centered(
                    None,
                    size(px(w), px(h)),
                    cx,
                ))),
                window_min_size: Some(size(px(min.width), px(min.height))),
                app_id: Some("kube-ui".into()),
                ..Default::default()
            };
            gpui_kit::open_window(options, cx, |window, cx| {
                window.set_window_title("Kube UI");
                cx.new(|cx| app::App::new(window, cx))
            })
            .expect("failed to open the window");
            cx.activate(true);
        });
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
