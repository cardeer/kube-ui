//! The native macOS menu bar. Items that need the app send a [`MenuCommand`] through the
//! message bus; window and app-level items (Hide, Minimize, Full Screen…) are handled by AppKit.
//!
//! Menu shortcuts reach AppKit before egui sees the key, so none of them may reuse a key the
//! app handles itself: ⌘F/⌘G/⌘⇧G (find), ⌘S and ⌘/ (YAML editor), and the text-editing keys.

use muda::{
    AboutMetadata, Menu, MenuEvent, MenuId, MenuItem, PredefinedMenuItem, Submenu,
    accelerator::{Accelerator, Code, Modifiers},
};

use super::{Bus, Msg, Tab, update};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MenuCommand {
    CheckForUpdates,
    WhatsNew,
    Settings,
    Quit,
    NewTab,
    OpenKubeconfig,
    Close,
    Reload,
    ToggleBottomPanel,
    Go(Tab),
    NextTab,
    PreviousTab,
    OpenGitHub,
    OpenUpdateLog,
}

/// Keeps the menu alive; AppKit only holds a weak reference to its items.
pub struct MenuBar {
    _menu: Menu,
}

impl MenuBar {
    /// Installs the menu bar, replacing winit's default one. Call once the app has launched.
    pub(super) fn install(bus: Bus) -> Option<Self> {
        let mut commands: Vec<(MenuId, MenuCommand)> = Vec::new();
        let mut item = |cmd: MenuCommand, text: &str, accel: Option<Accelerator>| {
            let id = MenuId::new(format!("{cmd:?}"));
            commands.push((id.clone(), cmd));
            MenuItem::with_id(id, text, true, accel)
        };
        let key = |mods: Modifiers, code| Some(Accelerator::new(Some(mods), code));
        let cmd = Modifiers::SUPER;
        let cmd_alt = Modifiers::SUPER | Modifiers::ALT;
        let cmd_shift = Modifiers::SUPER | Modifiers::SHIFT;

        let about = AboutMetadata {
            name: Some("Kube UI".into()),
            version: Some(update::CURRENT.into()),
            ..Default::default()
        };
        let app_menu = Submenu::with_items(
            "Kube UI",
            true,
            &[
                &PredefinedMenuItem::about(Some("About Kube UI"), Some(about)),
                &item(MenuCommand::CheckForUpdates, "Check for Updates…", None),
                &item(MenuCommand::WhatsNew, "What's New", None),
                &PredefinedMenuItem::separator(),
                &item(MenuCommand::Settings, "Settings…", key(cmd, Code::Comma)),
                &PredefinedMenuItem::separator(),
                &PredefinedMenuItem::services(None),
                &PredefinedMenuItem::separator(),
                &PredefinedMenuItem::hide(Some("Hide Kube UI")),
                &PredefinedMenuItem::hide_others(None),
                &PredefinedMenuItem::show_all(None),
                &PredefinedMenuItem::separator(),
                // Our own item rather than AppKit's, so eframe shuts down normally.
                &item(MenuCommand::Quit, "Quit Kube UI", key(cmd, Code::KeyQ)),
            ],
        )
        .ok()?;
        let file_menu = Submenu::with_items(
            "File",
            true,
            &[
                &item(MenuCommand::NewTab, "New Tab", key(cmd, Code::KeyT)),
                &item(
                    MenuCommand::OpenKubeconfig,
                    "Open Kubeconfig…",
                    key(cmd, Code::KeyO),
                ),
                &PredefinedMenuItem::separator(),
                &item(MenuCommand::Close, "Close", key(cmd, Code::KeyW)),
            ],
        )
        .ok()?;
        let view_menu = Submenu::with_items(
            "View",
            true,
            &[
                &item(MenuCommand::Reload, "Reload", key(cmd, Code::KeyR)),
                &item(
                    MenuCommand::ToggleBottomPanel,
                    "Show/Hide Logs & Terminal",
                    key(cmd, Code::KeyJ),
                ),
            ],
        )
        .ok()?;
        let go_menu = Submenu::with_items(
            "Go",
            true,
            &[
                &item(
                    MenuCommand::Go(Tab::Overview),
                    "Overview",
                    key(cmd_alt, Code::Digit1),
                ),
                &item(
                    MenuCommand::Go(Tab::Pods),
                    "Pods",
                    key(cmd_alt, Code::Digit2),
                ),
                &item(
                    MenuCommand::Go(Tab::Deployments),
                    "Deployments",
                    key(cmd_alt, Code::Digit3),
                ),
                &item(
                    MenuCommand::Go(Tab::Services),
                    "Services",
                    key(cmd_alt, Code::Digit4),
                ),
                &item(
                    MenuCommand::Go(Tab::Forwards),
                    "Port Forwards",
                    key(cmd_alt, Code::Digit5),
                ),
            ],
        )
        .ok()?;
        let window_menu = Submenu::with_items(
            "Window",
            true,
            &[
                &PredefinedMenuItem::minimize(None),
                &PredefinedMenuItem::maximize(Some("Zoom")),
                &PredefinedMenuItem::fullscreen(None),
                &PredefinedMenuItem::separator(),
                &item(
                    MenuCommand::NextTab,
                    "Show Next Tab",
                    key(cmd_shift, Code::BracketRight),
                ),
                &item(
                    MenuCommand::PreviousTab,
                    "Show Previous Tab",
                    key(cmd_shift, Code::BracketLeft),
                ),
                &PredefinedMenuItem::separator(),
                &PredefinedMenuItem::bring_all_to_front(None),
            ],
        )
        .ok()?;
        let help_menu = Submenu::with_items(
            "Help",
            true,
            &[
                &item(MenuCommand::OpenGitHub, "Kube UI on GitHub", None),
                &item(MenuCommand::OpenUpdateLog, "Show Update Log", None),
            ],
        )
        .ok()?;

        let menu = Menu::with_items(&[
            &app_menu,
            &file_menu,
            &view_menu,
            &go_menu,
            &window_menu,
            &help_menu,
        ])
        .ok()?;
        menu.init_for_nsapp();
        window_menu.set_as_windows_menu_for_nsapp();

        MenuEvent::set_event_handler(Some(move |event: MenuEvent| {
            if let Some((_, cmd)) = commands.iter().find(|(id, _)| id == event.id()) {
                bus.send(Msg::Menu(*cmd));
            }
        }));
        Some(Self { _menu: menu })
    }
}
