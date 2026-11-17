mod app;
mod details;
mod kube_ops;
mod settings;
mod telepresence;

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
/// (aws, kubelogin, gke-gcloud-auth-plugin, …) and tools like `telepresence` wouldn't be
/// found. Borrow the shell's: interactive and login, so both `.zprofile` and `.zshrc` count
/// (Homebrew's PATH line often lives in `.zshrc`). Must run before any threads start.
fn inherit_shell_path() {
    if std::env::var_os("TERM").is_some() {
        return; // Started from a terminal: PATH is already right.
    }
    let mut path = shell_path().unwrap_or_else(|| std::env::var("PATH").unwrap_or_default());
    // Common install folders, in case the shell config didn't add them.
    for dir in ["/opt/homebrew/bin", "/opt/homebrew/sbin", "/usr/local/bin"] {
        if std::path::Path::new(dir).is_dir() && !path.split(':').any(|p| p == dir) {
            path.push(':');
            path.push_str(dir);
        }
    }
    // SAFETY: called first thing in main, before any other threads exist.
    unsafe { std::env::set_var("PATH", path) };
}

/// `$PATH` as the user's shell sets it, or `None` if the shell fails or takes too long.
fn shell_path() -> Option<String> {
    use std::{
        io::Read,
        process::{Command, Stdio},
        time::{Duration, Instant},
    };
    const MARK: &str = "__KUBE_UI_PATH__";
    let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/zsh".into());
    let mut child = Command::new(shell)
        .args(["-i", "-l", "-c", &format!("printf '\\n{MARK}%s{MARK}' \"$PATH\"")])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    // A shell config that waits on something mustn't hold up the launch.
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) if Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(20))
            }
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
        }
    }
    let mut stdout = String::new();
    child.stdout.take()?.read_to_string(&mut stdout).ok()?;
    let path = stdout.split(MARK).nth(1)?.trim();
    (!path.is_empty()).then(|| path.to_string())
}
