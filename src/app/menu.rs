//! The native menu bar, built from GPUI actions. Each item is an action bound to its
//! shortcut; the window's root view turns it into a [`MenuCommand`] (see `App::render`).
//!
//! None of the shortcuts may reuse a key the app handles itself: ⌘F/⌘G/⌘⇧G (find), ⌘S and
//! ⌘/ (YAML editor), and the text-editing keys.

use gpui_kit::{self as gpui, KeyBinding, Menu, MenuItem};

use super::Tab;

gpui_kit::actions!(
    kube_ui,
    [
        CheckForUpdates,
        WhatsNew,
        OpenSettings,
        Quit,
        NewTab,
        OpenKubeconfig,
        Close,
        Reload,
        ToggleBottomPanel,
        GoOverview,
        GoPods,
        GoDeployments,
        GoServices,
        GoForwards,
        NextTab,
        PreviousTab,
        OpenGitHub,
        OpenUpdateLog,
    ]
);

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

/// Installs the menu bar and its shortcuts. Call once at startup.
pub fn install(cx: &mut gpui::App) {
    cx.bind_keys([
        KeyBinding::new("cmd-,", OpenSettings, None),
        KeyBinding::new("cmd-q", Quit, None),
        KeyBinding::new("cmd-t", NewTab, None),
        KeyBinding::new("cmd-o", OpenKubeconfig, None),
        KeyBinding::new("cmd-w", Close, None),
        KeyBinding::new("cmd-r", Reload, None),
        KeyBinding::new("cmd-j", ToggleBottomPanel, None),
        KeyBinding::new("cmd-alt-1", GoOverview, None),
        KeyBinding::new("cmd-alt-2", GoPods, None),
        KeyBinding::new("cmd-alt-3", GoDeployments, None),
        KeyBinding::new("cmd-alt-4", GoServices, None),
        KeyBinding::new("cmd-alt-5", GoForwards, None),
        KeyBinding::new("cmd-shift-]", NextTab, None),
        KeyBinding::new("cmd-shift-[", PreviousTab, None),
    ]);
    cx.set_menus(vec![
        Menu {
            name: "Kube UI".into(),
            items: vec![
                MenuItem::action("Check for Updates…", CheckForUpdates),
                MenuItem::action("What's New", WhatsNew),
                MenuItem::separator(),
                MenuItem::action("Settings…", OpenSettings),
                MenuItem::separator(),
                MenuItem::action("Quit Kube UI", Quit),
            ],
            disabled: false,
        },
        Menu {
            name: "File".into(),
            items: vec![
                MenuItem::action("New Tab", NewTab),
                MenuItem::action("Open Kubeconfig…", OpenKubeconfig),
                MenuItem::separator(),
                MenuItem::action("Close", Close),
            ],
            disabled: false,
        },
        Menu {
            name: "View".into(),
            items: vec![
                MenuItem::action("Reload", Reload),
                MenuItem::action("Show/Hide Logs & Terminal", ToggleBottomPanel),
            ],
            disabled: false,
        },
        Menu {
            name: "Go".into(),
            items: vec![
                MenuItem::action("Overview", GoOverview),
                MenuItem::action("Pods", GoPods),
                MenuItem::action("Deployments", GoDeployments),
                MenuItem::action("Services", GoServices),
                MenuItem::action("Port Forwards", GoForwards),
            ],
            disabled: false,
        },
        Menu {
            name: "Window".into(),
            items: vec![
                MenuItem::action("Show Next Tab", NextTab),
                MenuItem::action("Show Previous Tab", PreviousTab),
            ],
            disabled: false,
        },
        Menu {
            name: "Help".into(),
            items: vec![
                MenuItem::action("Kube UI on GitHub", OpenGitHub),
                MenuItem::action("Show Update Log", OpenUpdateLog),
            ],
            disabled: false,
        },
    ]);
}

/// Adds a handler for every menu action to the root element, each sending its command.
pub fn handle_actions<E: gpui::InteractiveElement>(el: E) -> E {
    use super::{Message, send};
    fn cmd<A: 'static>(c: MenuCommand) -> impl Fn(&A, &mut gpui::Window, &mut gpui::App) {
        move |_, window, cx| send(Message::Menu(c), window, cx)
    }
    el.on_action::<CheckForUpdates>(cmd(MenuCommand::CheckForUpdates))
        .on_action::<WhatsNew>(cmd(MenuCommand::WhatsNew))
        .on_action::<OpenSettings>(cmd(MenuCommand::Settings))
        .on_action::<Quit>(cmd(MenuCommand::Quit))
        .on_action::<NewTab>(cmd(MenuCommand::NewTab))
        .on_action::<OpenKubeconfig>(cmd(MenuCommand::OpenKubeconfig))
        .on_action::<Close>(cmd(MenuCommand::Close))
        .on_action::<Reload>(cmd(MenuCommand::Reload))
        .on_action::<ToggleBottomPanel>(cmd(MenuCommand::ToggleBottomPanel))
        .on_action::<GoOverview>(cmd(MenuCommand::Go(Tab::Overview)))
        .on_action::<GoPods>(cmd(MenuCommand::Go(Tab::Pods)))
        .on_action::<GoDeployments>(cmd(MenuCommand::Go(Tab::Deployments)))
        .on_action::<GoServices>(cmd(MenuCommand::Go(Tab::Services)))
        .on_action::<GoForwards>(cmd(MenuCommand::Go(Tab::Forwards)))
        .on_action::<NextTab>(cmd(MenuCommand::NextTab))
        .on_action::<PreviousTab>(cmd(MenuCommand::PreviousTab))
        .on_action::<OpenGitHub>(cmd(MenuCommand::OpenGitHub))
        .on_action::<OpenUpdateLog>(cmd(MenuCommand::OpenUpdateLog))
}
