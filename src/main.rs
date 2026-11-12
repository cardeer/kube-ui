mod app;
mod details;
mod kube_ops;
mod settings;

fn main() -> iced::Result {
    inherit_shell_path();
    // Reopen at the size the window had last time.
    let saved = settings::Settings::load();
    let min = app::MIN_WINDOW_SIZE;
    let size = saved
        .window_size
        .map(|[w, h]| iced::Size::new(w.max(min.width), h.max(min.height)))
        .unwrap_or(iced::Size::new(1360.0, 860.0));
    let mut app = iced::application(app::App::new, app::App::update, app::App::view)
        .title(app::App::title)
        .theme(app::App::theme)
        .subscription(app::App::subscription)
        .window(iced::window::Settings {
            size,
            min_size: Some(min),
            icon: iced::window::icon::from_file_data(include_bytes!("../assets/icon.png"), None)
                .ok(),
            ..Default::default()
        })
        .exit_on_close_request(false)
        .antialiasing(true)
        .default_font(app::theme_font());
    for font in app::fonts() {
        app = app.font(font);
    }
    app.run()
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
