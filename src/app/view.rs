//! Rendering of the window: top bar, sidebar, pages, the bottom dock, dialogs, popups
//! and toasts. Cluster-tab content (lists, Overview, drawer) lives in `workspace::view`;
//! the YAML window in `yaml.rs`; log and shell tabs in `logs.rs` / `shell.rs`.

use std::collections::HashMap;

use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::{h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, ClickEvent, Div, ElementId, FontWeight, InteractiveElement as _,
    IntoElement as _, MouseButton, ParentElement as _, SharedString, Stateful,
    StatefulInteractiveElement as _, Styled as _, Subscription, canvas, div, px, relative,
};

use super::*;
use crate::app::workspace::ViewCtx;

// ---------------------------------------------------------------------------
// Retained state
// ---------------------------------------------------------------------------

/// The port-forward dialog's two text fields. Created when the dialog opens, dropped when
/// it closes.
pub(super) struct PfInputs {
    remote: Entity<InputState>,
    local: Entity<InputState>,
    _subs: Vec<Subscription>,
}

/// Retained GPUI state owned by the window chrome.
#[derive(Default)]
pub(super) struct ChromeState {
    /// The search field of the searchable pickers (context, namespace).
    popup_search: Option<Entity<InputState>>,
    pf: Option<PfInputs>,
    subs: Vec<Subscription>,
}

/// Where each popup trigger was last painted, so a popup can open right below it. Written
/// while painting (no re-render needed), read when the popup is laid out.
#[derive(Default)]
struct TriggerBounds(HashMap<String, Rectangle>);

impl gpui::Global for TriggerBounds {}

fn trigger_key(kind: &PopupKind) -> String {
    match kind {
        // The submenu is part of the "+" menu.
        PopupKind::NewTabContexts(_) => format!("{:?}", PopupKind::NewTab),
        k => format!("{k:?}"),
    }
}

/// An invisible layer that records its parent's bounds as the anchor of `kind`'s popup.
/// Put it inside a `relative()` trigger element (see [`trigger`]).
pub fn track_bounds(kind: &PopupKind) -> AnyElement {
    let key = trigger_key(kind);
    canvas(
        move |bounds, _window, cx| {
            cx.default_global::<TriggerBounds>()
                .0
                .insert(key, bounds.into());
        },
        |_, _, _, _| {},
    )
    .absolute()
    .size_full()
    .into_any_element()
}

/// Wraps a popup trigger so its popup opens right below it.
pub fn trigger(kind: PopupKind, el: impl gpui::IntoElement) -> Div {
    div()
        .relative()
        .flex_none()
        .child(el)
        .child(track_bounds(&kind))
}

fn nid(name: &'static str, n: u64) -> ElementId {
    ElementId::NamedInteger(name.into(), n)
}

fn eid(s: impl Into<SharedString>) -> ElementId {
    ElementId::Name(s.into())
}

/// A text-ish link: accent coloured, lighter on hover.
fn link(id: impl Into<ElementId>, color: Color, label: impl Into<SharedString>, msg: Message) -> Stateful<Div> {
    div()
        .id(id)
        .px(px(2.0))
        .py(px(2.0))
        .text_size(px(theme::BODY))
        .text_color(color)
        .cursor_pointer()
        .hover(|s| s.text_color(theme::ACCENT_HOVER))
        .on_click(on(msg))
        .child(label.into())
}

fn search_prefix() -> Div {
    theme::icon(icon::MAGNIFYING_GLASS)
        .text_size(px(13.0))
        .text_color(theme::TEXT_FAINT)
}

impl App {
    /// Creates the chrome's retained state (popup search field, dialog inputs) as needed.
    pub(super) fn prepare_chrome(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        // The popup's search field lives as long as the app.
        if self.chrome.popup_search.is_none() {
            let input = cx.new(|cx| InputState::new(window, cx).placeholder("Search…"));
            let sub = cx.subscribe_in(
                &input,
                window,
                |app: &mut App, state, ev: &InputEvent, window, cx| match ev {
                    InputEvent::Change => {
                        let text = state.read(cx).value().to_string();
                        app.dispatch(Message::PopupQuery(text), window, cx);
                    }
                    InputEvent::PressEnter { .. } => {
                        app.dispatch(Message::PopupSubmit, window, cx);
                    }
                    _ => {}
                },
            );
            self.chrome.subs.push(sub);
            self.chrome.popup_search = Some(input);
        }
        if let Some(input) = self.chrome.popup_search.clone() {
            let searchable = matches!(
                self.popup.as_ref().map(|p| &p.kind),
                Some(PopupKind::Context | PopupKind::Namespace)
            );
            if searchable {
                // A freshly opened picker starts empty.
                let query = self.popup.as_ref().map(|p| p.query.clone()).unwrap_or_default();
                if input.read(cx).value().as_ref() != query.as_str() {
                    input.update(cx, |s, cx| s.set_value(SharedString::from(query), window, cx));
                }
            } else if self.popup.is_none() {
                // Hand the keyboard back to the window when the picker goes away.
                use gpui::Focusable as _;
                if input.focus_handle(cx).is_focused(window) {
                    window.focus(&self.focus, cx);
                }
            }
        }

        // The port-forward dialog's fields.
        if let Some(d) = &self.pf_dialog {
            let (remote_text, local_text) = (d.remote_text.clone(), d.local_text.clone());
            match &self.chrome.pf {
                None => {
                    let remote = cx.new(|cx| {
                        InputState::new(window, cx).default_value(SharedString::from(remote_text))
                    });
                    let local = cx.new(|cx| {
                        InputState::new(window, cx).default_value(SharedString::from(local_text))
                    });
                    let mut subs = Vec::new();
                    for (input, is_remote) in [(&remote, true), (&local, false)] {
                        subs.push(cx.subscribe_in(
                            input,
                            window,
                            move |app: &mut App, state, ev: &InputEvent, window, cx| match ev {
                                InputEvent::Change => {
                                    let text = state.read(cx).value().to_string();
                                    let msg = if is_remote {
                                        Message::PfRemote(text)
                                    } else {
                                        Message::PfLocal(text)
                                    };
                                    app.dispatch(msg, window, cx);
                                }
                                InputEvent::PressEnter { .. } => {
                                    app.dispatch(Message::PfStart, window, cx);
                                }
                                _ => {}
                            },
                        ));
                    }
                    self.chrome.pf = Some(PfInputs {
                        remote,
                        local,
                        _subs: subs,
                    });
                }
                Some(pf) => {
                    // A port button sets the remote field's text from outside.
                    let remote = pf.remote.clone();
                    if remote.read(cx).value().as_ref() != remote_text.as_str() {
                        remote.update(cx, |s, cx| {
                            s.set_value(SharedString::from(remote_text), window, cx)
                        });
                    }
                }
            }
        } else if let Some(pf) = self.chrome.pf.take() {
            use gpui::Focusable as _;
            if pf.remote.focus_handle(cx).is_focused(window)
                || pf.local.focus_handle(cx).is_focused(window)
            {
                window.focus(&self.focus, cx);
            }
        }
    }

    /// The whole window. Called from `App::render`; `window`/`cx` are there for creating
    /// retained state (Kit inputs) lazily.
    pub fn view(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let _ = window;
        let base = if self.workspaces.is_empty() {
            self.view_onboarding()
        } else {
            v_flex()
                .size_full()
                .child(self.view_top_bar())
                .child(
                    h_flex()
                        .flex_1()
                        .min_h_0()
                        .child(self.view_sidebar())
                        .child(self.view_main()),
                )
                .into_any_element()
        };
        let mut root = div().size_full().relative().bg(theme::CONTENT).child(base);
        if let Some(m) = self.view_modal() {
            root = root.child(m);
        }
        if let Some(p) = self.view_popup(cx) {
            root = root.child(p);
        }
        if !self.toasts.is_empty() {
            root = root.child(self.view_toasts());
        }
        root.into_any_element()
    }

    fn ctx(&self) -> ViewCtx<'_> {
        ViewCtx {
            hovered: self.hovered_row,
            popup: self.popup.as_ref().map(|p| &p.kind),
            env_filter: &self.env_filter,
            toggled: &self.toggled_sections,
            drawer_width: self.drawer_width,
            widths: &self.settings.column_widths,
        }
    }

    fn popup_open(&self, kind: &PopupKind) -> bool {
        self.popup.as_ref().is_some_and(|p| &p.kind == kind)
    }

    // -----------------------------------------------------------------------
    // Chrome
    // -----------------------------------------------------------------------

    fn view_onboarding(&self) -> AnyElement {
        let mut col = v_flex()
            .items_center()
            .child(
                theme::icon(icon::STEERING_WHEEL)
                    .text_size(px(56.0))
                    .line_height(px(56.0))
                    .text_color(theme::ACCENT),
            )
            .child(theme::vspace(10.0))
            .child(
                div()
                    .text_size(px(26.0))
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(theme::TEXT)
                    .child("Welcome to Kube UI"),
            )
            .child(theme::vspace(4.0))
            .child(theme::txt(
                "Pick a kubeconfig file to get started. You can open more clusters as tabs later.",
            ))
            .child(theme::vspace(18.0))
            .child(
                div().w(px(260.0)).flex().justify_center().child(
                    theme::primary_button(
                        "welcome-choose",
                        Some(icon::FOLDER_OPEN),
                        "Choose kubeconfig file…",
                        Some(Message::BrowseNewTab),
                    ),
                ),
            );
        if let Some(default) = default_kubeconfig() {
            col = col.child(theme::vspace(6.0)).child(theme::secondary_button(
                "welcome-default",
                None,
                format!("Use {}", display_path(&default)),
                Some(Message::AddWorkspace(default, None)),
            ));
        }
        if !self.settings.kubeconfigs.is_empty() {
            col = col
                .child(theme::vspace(14.0))
                .child(theme::faint("Recent"));
            for p in &self.settings.kubeconfigs {
                col = col.child(link(
                    format!("welcome-recent-{}", p.display()),
                    theme::ACCENT_HOVER,
                    display_path(p),
                    Message::AddWorkspace(p.clone(), None),
                ));
            }
        }
        v_flex()
            .size_full()
            .items_center()
            .bg(theme::CONTENT)
            .child(div().h(relative(0.22)).flex_none())
            .child(col)
            .into_any_element()
    }

    fn view_top_bar(&self) -> AnyElement {
        // Cluster tabs.
        let closable = self.workspaces.len() > 1;
        let mut tabs = h_flex()
            .items_center()
            .gap(px(8.0))
            .child(
                theme::icon(icon::STEERING_WHEEL)
                    .text_size(px(18.0))
                    .line_height(px(18.0)),
            )
            .child(
                div()
                    .text_size(px(14.5))
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(theme::TEXT)
                    .whitespace_nowrap()
                    .child("Kube UI"),
            )
            .child(theme::hspace(10.0));
        for (i, ws) in self.workspaces.iter().enumerate() {
            let (color, status) = ws.conn.status();
            let selected = i == self.active;
            let title = div()
                .id(nid("tab-title", ws.id))
                .flex()
                .items_center()
                .gap(px(6.0))
                .pl(px(12.0))
                .py(px(1.0))
                .cursor_pointer()
                .on_click(on(Message::ActivateWorkspace(i)))
                .on_mouse_down(
                    MouseButton::Middle,
                    on(if closable {
                        Message::CloseWorkspace(i)
                    } else {
                        Message::Noop
                    }),
                )
                .child(theme::dot(color))
                .child(
                    div()
                        .text_size(px(theme::BODY))
                        .whitespace_nowrap()
                        .text_color(if selected {
                            theme::TEXT
                        } else {
                            theme::TEXT_MUTED
                        })
                        .when(selected, |d| d.font_weight(FontWeight::SEMIBOLD))
                        .child(ws.title()),
                );
            let title = theme::tip(
                title,
                format!("{status}\n{}", display_path(&ws.kubeconfig_path)),
            );
            // The active tab shares the picker row's ground and opens into it.
            let tab = h_flex()
                .items_center()
                .gap(px(6.0))
                .pr(px(6.0))
                .pt(px(7.0))
                .pb(px(8.0))
                .rounded_t(px(7.0))
                .when(selected, |d| d.bg(theme::CONTENT))
                .child(title)
                .child(if closable {
                    theme::icon_button(
                        nid("tab-close", ws.id),
                        icon::X,
                        "Close tab",
                        Some(Message::CloseWorkspace(i)),
                    )
                    .into_any_element()
                } else {
                    theme::hspace(6.0).into_any_element()
                });
            tabs = tabs.child(tab);
        }
        tabs = tabs
            .child(trigger(
                PopupKind::NewTab,
                theme::icon_button(
                    "new-tab",
                    icon::PLUS,
                    "Open another cluster in a new tab (⌘T)",
                    Some(Message::OpenPopup(PopupKind::NewTab)),
                ),
            ))
            .child(theme::fill_space())
            .child(self.view_update_control())
            .child(theme::hspace(4.0));

        let mut col = v_flex().w_full().child(
            div()
                .w_full()
                .pl(px(14.0))
                .pr(px(14.0))
                .pt(px(8.0))
                .child(tabs),
        );
        // Pickers for the active tab, on the content ground the active tab opens into.
        if let Some(ws) = self.ws() {
            let (color, status) = ws.conn.status();
            let picker = |label: &'static str,
                          kind: PopupKind,
                          text: String,
                          width: f32,
                          id: &'static str| {
                let open = self.popup_open(&kind);
                [
                    theme::muted(label).into_any_element(),
                    trigger(
                        kind.clone(),
                        theme::dropdown_button(id, text, width, open, Message::OpenPopup(kind)),
                    )
                    .into_any_element(),
                ]
            };
            let mut pickers = h_flex()
                .w_full()
                .items_center()
                .gap(px(8.0))
                .px(px(14.0))
                .py(px(8.0))
                .bg(theme::CONTENT)
                .children(picker(
                    "Config",
                    PopupKind::Kubeconfig,
                    file_label(&ws.kubeconfig_path),
                    170.0,
                    "kubeconfig-picker",
                ))
                .children(picker(
                    "Context",
                    PopupKind::Context,
                    ws.context.clone().unwrap_or_default(),
                    220.0,
                    "context-picker",
                ))
                .children(picker(
                    "Namespace",
                    PopupKind::Namespace,
                    namespaces_label(&ws.selected_ns, false),
                    220.0,
                    "namespace-picker",
                ))
                .child(theme::fill_space());
            if ws.is_loading() {
                pickers = pickers.child(theme::faint("Loading…"));
            }
            pickers = pickers
                .child(theme::secondary_button(
                    "refresh",
                    Some(icon::ARROW_CLOCKWISE),
                    "Refresh",
                    ws.client().is_some().then_some(Message::Refresh),
                ))
                .child(
                    h_flex()
                        .items_center()
                        .gap(px(6.0))
                        .child(theme::dot(color))
                        .child(theme::colored(status, color)),
                );
            col = col.child(pickers);
        }
        div()
            .w_full()
            .flex_none()
            .bg(theme::CHROME)
            .child(col)
            .into_any_element()
    }

    /// Right end of the tab row: check for updates, or offer the one found.
    fn view_update_control(&self) -> AnyElement {
        let current = update::CURRENT;
        match &self.update {
            // The launch check runs silently; only a check the user asked for shows progress.
            update::UpdateState::Checking { manual: true } => {
                theme::faint("Checking for updates…").into_any_element()
            }
            update::UpdateState::Installing(tag) => {
                theme::faint(format!("Installing {tag}… Kube UI will reopen")).into_any_element()
            }
            update::UpdateState::Installed(tag) => theme::tip(
                theme::primary_button(
                    "update-restart",
                    Some(icon::ARROW_CLOCKWISE),
                    "Restart to finish",
                    Some(Message::Relaunch),
                ),
                format!("{tag} is installed. Quits and reopens Kube UI."),
            )
            .into_any_element(),
            update::UpdateState::Available(tag) => theme::tip(
                theme::primary_button(
                    "update-available",
                    Some(icon::ARROW_CIRCLE_UP),
                    format!("Update to {tag}"),
                    Some(Message::StartUpdate(tag.clone())),
                ),
                format!("You have {current}. Downloads {tag}, installs it and reopens Kube UI."),
            )
            .into_any_element(),
            // On macOS, checking and "What's new" live in the menu bar instead.
            #[cfg(target_os = "macos")]
            _ => div().into_any_element(),
            #[cfg(not(target_os = "macos"))]
            state => {
                let hint = match state {
                    update::UpdateState::UpToDate => {
                        format!("Kube UI {current} is up to date. Check again")
                    }
                    update::UpdateState::Failed(e) => format!("Last check failed: {e}"),
                    _ => format!("Kube UI {current}. Check GitHub for a newer release"),
                };
                h_flex()
                    .items_center()
                    .child(theme::tip(
                        theme::ghost_button("check-updates", Some(Message::CheckUpdates))
                            .px(px(6.0))
                            .py(px(3.0))
                            .child(theme::icon_label(icon::ARROW_CLOCKWISE, "Check for updates")),
                        hint,
                    ))
                    .child(theme::tip(
                        link(
                            "whats-new-version",
                            theme::TEXT_FAINT,
                            format!("v{current}"),
                            Message::ShowChangelog(true),
                        )
                        .text_size(px(theme::SMALL)),
                        "What's new",
                    ))
                    .into_any_element()
            }
        }
    }

    fn view_sidebar(&self) -> AnyElement {
        let Some(ws) = self.ws() else {
            return div().into_any_element();
        };
        let active_forwards = self
            .forwards
            .iter()
            .filter(|f| f.status == PfStatus::Listening)
            .count();
        let plain = |n: Option<usize>| n.filter(|n| *n > 0).map(|n| (n.to_string(), None));
        // Pods report failures rather than a total, so a problem shows from any page.
        let failing = ws.failing_pods();
        let pods_badge = if failing > 0 {
            Some((format!("{failing} failing"), Some(RED)))
        } else {
            plain(count(&ws.pods))
        };
        let mut workloads = vec![
            (Tab::Overview, None),
            (Tab::Pods, pods_badge),
            (Tab::Deployments, plain(count(&ws.deployments))),
        ];
        workloads.extend(WORKLOAD_KINDS.map(|k| (Tab::Workload(k), plain(count(ws.workload(k))))));
        let forwards_badge =
            (active_forwards > 0).then(|| (format!("{active_forwards} live"), Some(GREEN)));
        let groups = [
            ("workloads", icon::CUBE, "Workloads", workloads),
            (
                "network",
                icon::SHARE_NETWORK,
                "Network",
                vec![
                    (Tab::Services, plain(count(&ws.services))),
                    (Tab::Forwards, forwards_badge),
                ],
            ),
        ];
        // No spacing: child rows carry their own gap so the guide line stays unbroken.
        let mut list = v_flex().w_full();
        for (key, glyph, label, children) in groups {
            let open = !self.settings.nav_collapsed.iter().any(|c| c == key);
            let has_selected = children.iter().any(|(t, _)| *t == ws.tab);
            // A closed group still shows that the current page lives inside it.
            list = list
                .child(theme::nav_group(
                    format!("nav-group-{key}"),
                    glyph,
                    label,
                    open,
                    !open && has_selected,
                    Message::ToggleNavGroup(key),
                ))
                .child(theme::vspace(1.0));
            if open {
                for (tab, badge) in children {
                    list = list.child(theme::nav_child(
                        format!("nav-{}", tab.key()),
                        tab_label(tab),
                        badge,
                        ws.tab == tab,
                        Message::SetTab(tab),
                    ));
                }
            }
            list = list.child(theme::vspace(6.0));
        }
        // Settings is pinned to the bottom of the rail; the groups scroll above it.
        v_flex()
            .w(px(NAV_WIDTH))
            .h_full()
            .flex_none()
            .px(px(10.0))
            .py(px(12.0))
            .bg(theme::CHROME)
            .child(
                div()
                    .id("nav-scroll")
                    .flex_1()
                    .min_h_0()
                    .w_full()
                    .overflow_y_scroll()
                    .child(list),
            )
            .child(theme::vspace(6.0))
            .child(theme::nav_item(
                "nav-settings",
                icon::GEAR,
                "Settings",
                None,
                ws.tab == Tab::Settings,
                Message::SetTab(Tab::Settings),
            ))
            .into_any_element()
    }

    fn view_main(&self) -> AnyElement {
        let Some(ws) = self.ws() else {
            return div().into_any_element();
        };
        let ctx = self.ctx();
        let page = match ws.tab {
            Tab::Forwards => self.view_forwards(),
            Tab::Settings => self.view_settings(),
            _ => ws.view_page(&ctx),
        };
        let page = div()
            .flex_1()
            .min_w_0()
            .h_full()
            .overflow_hidden()
            .bg(theme::CONTENT)
            .pl(px(20.0))
            .pr(px(20.0))
            .pt(px(16.0))
            .pb(px(8.0))
            .child(div().size_full().child(page));
        let mut upper = h_flex().flex_1().min_h_0().w_full().child(page);
        if ws.tab.is_list() {
            if let Some(drawer) = ws.view_drawer(&ctx) {
                upper = upper.child(drawer);
            }
        }
        v_flex()
            .flex_1()
            .min_w_0()
            .h_full()
            .child(upper)
            .children(self.view_dock())
            .into_any_element()
    }

    // -----------------------------------------------------------------------
    // Pages
    // -----------------------------------------------------------------------

    fn view_forwards(&self) -> AnyElement {
        let title = theme::title("Port Forwards");
        if self.forwards.is_empty() {
            return v_flex()
                .child(title)
                .child(theme::vspace(4.0))
                .child(theme::txt(
                    "No port forwards yet. Select a pod or service and choose Port forward.",
                ))
                .into_any_element();
        }
        // (title, default width, fills the rest)
        let cols: [(&str, f32, bool); 7] = [
            ("Context", 200.0, false),
            ("Namespace", 130.0, false),
            ("Target", 220.0, true),
            ("Local / Remote", 150.0, false),
            ("Status", 120.0, false),
            ("Conns", 70.0, false),
            ("", 170.0, false),
        ];
        let width = |i: usize| {
            let (name, w, _) = cols[i];
            self.settings
                .column_widths
                .get(&table::width_key(Tab::Forwards, name))
                .copied()
                .unwrap_or(w)
        };
        let cell = |i: usize, child: AnyElement| {
            let (_, _, fill) = cols[i];
            let c = div()
                .h(px(ROW_HEIGHT))
                .px(px(12.0))
                .flex()
                .items_center()
                .overflow_hidden()
                .child(child);
            if fill {
                c.flex_1().min_w(px(width(i)))
            } else {
                c.w(px(width(i))).flex_none()
            }
        };
        let clip = |d: Div| d.min_w_0().overflow_hidden().text_ellipsis().whitespace_nowrap();

        let header = h_flex().h(px(30.0)).w_full().flex_none().children(
            cols.iter().enumerate().map(|(i, (name, _, fill))| {
                let key = table::width_key(Tab::Forwards, name);
                let start = width(i);
                let c = div()
                    .relative()
                    .h_full()
                    .px(px(12.0))
                    .flex()
                    .items_center()
                    .overflow_hidden()
                    .child(theme::label_small(*name));
                let c = if *fill {
                    c.flex_1().min_w(px(start))
                } else {
                    c.w(px(start)).flex_none()
                };
                let handle = div()
                    .id(eid(format!("pf-col-{key}")))
                    .absolute()
                    .right_0()
                    .top_0()
                    .h_full()
                    .w(px(7.0))
                    .cursor_col_resize()
                    .on_mouse_down(
                        MouseButton::Left,
                        on(Message::DragStart(Drag::Column {
                            key: key.clone(),
                            start,
                        })),
                    )
                    .on_click({
                        let key = key.clone();
                        move |e: &ClickEvent, window, cx| {
                            if e.click_count() >= 2 {
                                send(Message::ResetColumn(key.clone()), window, cx);
                            }
                        }
                    });
                c.when(!name.is_empty(), |c| c.child(handle))
            }),
        );

        let rows = self.forwards.iter().enumerate().map(|(n, f)| {
            let local = f.bound_port.unwrap_or(f.spec.local_port);
            let (color, label) = match &f.status {
                PfStatus::Starting => (YELLOW, "Starting…"),
                PfStatus::Listening => (GREEN, "Active"),
                PfStatus::Stopped => (theme::IDLE, "Stopped"),
                PfStatus::Failed(_) => (RED, "Failed"),
            };
            let status = h_flex()
                .id(nid("pf-status", f.id))
                .items_center()
                .gap(px(6.0))
                .child(theme::dot(color))
                .child(theme::colored(label, color));
            let status = match &f.status {
                PfStatus::Failed(e) => theme::tip(status, e.clone()).into_any_element(),
                _ => status.into_any_element(),
            };
            let toggle = if f.handle.is_some() {
                theme::small_button(
                    nid("pf-stop", f.id),
                    Some(icon::STOP_CIRCLE),
                    "Stop",
                    Some(Message::Action(Action::StopForward(f.id))),
                )
            } else {
                theme::small_button(
                    nid("pf-start", f.id),
                    Some(icon::PLAY),
                    "Start",
                    Some(Message::Action(Action::RestartForward(f.id))),
                )
            };
            let controls = h_flex()
                .items_center()
                .gap(px(6.0))
                .child(toggle)
                .child(trigger(
                    PopupKind::Forward(f.id),
                    theme::small_button(
                        nid("pf-menu", f.id),
                        Some(icon::DOTS_THREE),
                        "",
                        Some(Message::OpenPopup(PopupKind::Forward(f.id))),
                    ),
                ));
            let context = theme::tip(
                clip(theme::txt(f.spec.context.clone())).id(nid("pf-ctx", f.id)),
                display_path(&f.spec.kubeconfig),
            );
            h_flex()
                .id(nid("pf-row", f.id))
                .w_full()
                .h(px(ROW_HEIGHT))
                .flex_none()
                .when(n % 2 == 1, |d| d.bg(theme::STRIPE))
                .hover(|s| s.bg(theme::FIELD))
                .child(cell(0, context.into_any_element()))
                .child(cell(
                    1,
                    clip(theme::txt(f.spec.namespace.clone())).into_any_element(),
                ))
                .child(cell(
                    2,
                    clip(theme::txt(f.spec.target.label())).into_any_element(),
                ))
                .child(cell(
                    3,
                    clip(theme::mono(format!("{local} → {}", f.spec.remote_port)))
                        .into_any_element(),
                ))
                .child(cell(4, status))
                .child(cell(
                    5,
                    theme::txt(f.connections.load(Ordering::Relaxed).to_string())
                        .into_any_element(),
                ))
                .child(cell(6, controls.into_any_element()))
        });
        v_flex()
            .size_full()
            .child(title)
            .child(theme::vspace(8.0))
            .child(header)
            .child(theme::hline())
            .child(
                v_flex()
                    .id("pf-rows")
                    .flex_1()
                    .min_h_0()
                    .w_full()
                    .overflow_y_scroll()
                    .children(rows),
            )
            .into_any_element()
    }

    fn view_settings(&self) -> AnyElement {
        let current = self.ws().map(|w| w.kubeconfig_path.clone());
        let mut files = v_flex().gap(px(8.0));
        for p in &self.settings.kubeconfigs {
            let key = p.display().to_string();
            let in_use = self
                .workspaces
                .iter()
                .filter(|w| w.kubeconfig_path == *p)
                .count();
            let mut buttons = h_flex().items_center().gap(px(6.0));
            if in_use > 0 {
                buttons = buttons.child(theme::faint(format!(
                    "{in_use} tab{}",
                    if in_use == 1 { "" } else { "s" }
                )));
            }
            buttons = buttons.child(theme::small_button(
                format!("cfg-new-{key}"),
                None,
                "Open in new tab",
                Some(Message::AddWorkspace(p.clone(), None)),
            ));
            if current.as_ref() != Some(p) {
                buttons = buttons.child(theme::small_button(
                    format!("cfg-use-{key}"),
                    None,
                    "Use in this tab",
                    Some(Message::UseKubeconfig(p.clone())),
                ));
            }
            buttons = buttons.child(theme::small_button(
                format!("cfg-remove-{key}"),
                None,
                "Remove",
                Some(Message::RemoveKubeconfig(p.clone())),
            ));
            files = files.child(
                h_flex()
                    .items_center()
                    .gap(px(8.0))
                    .child(
                        theme::mono(display_path(p))
                            .text_color(if in_use > 0 { GREEN } else { theme::TEXT })
                            .flex_1()
                            .min_w_0()
                            .overflow_hidden()
                            .text_ellipsis(),
                    )
                    .child(buttons),
            );
        }
        let mut col = v_flex()
            .child(theme::title("Settings"))
            .child(theme::vspace(12.0))
            .child(theme::label_small("Kubeconfig files"))
            .child(theme::vspace(4.0))
            .child(files)
            .child(theme::vspace(6.0))
            .child(div().flex().child(theme::secondary_button(
                "cfg-add",
                Some(icon::PLUS),
                "Add kubeconfig…",
                Some(Message::BrowseNewTab),
            )))
            .child(theme::vspace(16.0))
            .child(theme::checkbox(
                "cfg-live",
                self.settings.live_updates,
                "Live updates (watch lists for changes)",
                Message::ToggleLive,
            ))
            .child(theme::vspace(16.0))
            .child(
                h_flex()
                    .items_center()
                    .gap(px(8.0))
                    .child(theme::faint(format!("Kube UI v{}", update::CURRENT)))
                    .child(theme::small_button(
                        "cfg-whats-new",
                        Some(icon::SPARKLE),
                        "What's new",
                        Some(Message::ShowChangelog(true)),
                    )),
            );
        if let Some(p) = Settings::path() {
            col = col
                .child(theme::vspace(8.0))
                .child(theme::faint(format!("Settings stored in {}", p.display())));
        }
        div()
            .id("settings-scroll")
            .size_full()
            .overflow_y_scroll()
            .child(col)
            .into_any_element()
    }

    // -----------------------------------------------------------------------
    // Bottom dock: logs and shells
    // -----------------------------------------------------------------------

    fn view_dock(&self) -> Option<AnyElement> {
        let total = self.logs.len() + self.shells.len();
        if total == 0 {
            return None;
        }
        let n_logs = self.logs.len();
        let can_split = n_logs > 0 && !self.shells.is_empty();
        let split = self.settings.log_split && can_split;
        let (shown_log, shown_shell) = self.dock_shown();
        let active = self.active_log.min(total - 1);
        let mode = self.settings.log_mode;

        // (element id, glyph, title, hover, live)
        let tabs = self
            .logs
            .iter()
            .map(|l| {
                let glyph = if l.previous() {
                    icon::CLOCK_COUNTER_CLOCKWISE
                } else if l.is_group() {
                    icon::STACK
                } else {
                    icon::SCROLL
                };
                (l.stream_id(), "dock-log", glyph, l.title(), l.hover(), l.live())
            })
            .chain(self.shells.iter().map(|s| {
                (
                    s.id,
                    "dock-shell",
                    icon::TERMINAL_WINDOW,
                    s.title(),
                    s.hover(),
                    s.live(),
                )
            }));
        let mut strip = h_flex().items_center().gap(px(6.0));
        for (i, (id, kind, glyph, title, hover, live)) in tabs.enumerate() {
            let selected = if split {
                Some(i) == shown_log || Some(i) == shown_shell.map(|s| s + n_logs)
            } else {
                i == active
            };
            let color = match (selected, live) {
                (true, _) => theme::TEXT,
                (false, true) => theme::TEXT_MUTED,
                (false, false) => theme::TEXT_FAINT,
            };
            let label = div()
                .id(nid(kind, id))
                .flex()
                .items_center()
                .gap(px(6.0))
                .cursor_pointer()
                .on_click(on(Message::DockSelect(i)))
                .on_mouse_down(MouseButton::Middle, on(Message::DockClose(i)))
                .child(
                    theme::icon(glyph)
                        .text_size(px(13.0))
                        .line_height(px(13.0))
                        .text_color(color),
                )
                .child(
                    div()
                        .text_size(px(theme::BODY))
                        .whitespace_nowrap()
                        .text_color(color)
                        .when(selected, |d| d.font_weight(FontWeight::SEMIBOLD))
                        .child(title),
                );
            let tab = h_flex()
                .flex_none()
                .items_center()
                .gap(px(4.0))
                .pl(px(10.0))
                .pr(px(4.0))
                .py(px(3.0))
                .rounded(px(theme::RADIUS))
                .border_1()
                .border_color(if selected {
                    theme::BORDER_STRONG
                } else {
                    theme::BORDER
                })
                .when(selected, |d| d.bg(theme::FIELD))
                .child(theme::tip(label, format!("{hover}\nMiddle-click to close")))
                .child(theme::icon_button(
                    nid(if kind == "dock-log" { "dock-log-x" } else { "dock-shell-x" }, id),
                    icon::X,
                    "Close",
                    Some(Message::DockClose(i)),
                ));
            strip = strip.child(tab);
        }

        let (glyph, hint, target) = match mode {
            LogPanelMode::Collapsed => (icon::CARET_UP, "Restore size", LogPanelMode::Normal),
            _ => (
                icon::CARET_DOWN,
                "Collapse to bottom",
                LogPanelMode::Collapsed,
            ),
        };
        let mut controls = h_flex().flex_none().items_center().gap(px(4.0)).child(
            theme::icon_button("dock-collapse", glyph, hint, Some(Message::SetLogMode(target))),
        );
        if can_split {
            let hint = if split {
                "Show one tab at a time"
            } else {
                "Split: logs beside the terminal"
            };
            controls = controls.child(theme::icon_toggle(
                "dock-split",
                icon::SQUARE_SPLIT_HORIZONTAL,
                split,
                hint,
                Message::ToggleSplit,
            ));
        }
        let (glyph, hint, target) = match mode {
            LogPanelMode::Expanded => (icon::CORNERS_IN, "Restore size", LogPanelMode::Normal),
            _ => (icon::CORNERS_OUT, "Expand", LogPanelMode::Expanded),
        };
        controls = controls
            .child(theme::icon_button(
                "dock-expand",
                glyph,
                hint,
                Some(Message::SetLogMode(target)),
            ))
            .child(theme::icon_button(
                "dock-close-all",
                icon::X_CIRCLE,
                "Close all tabs",
                Some(Message::DockCloseAll),
            ));
        let header = h_flex()
            .w_full()
            .flex_none()
            .items_center()
            .gap(px(8.0))
            .child(
                div()
                    .id("dock-strip")
                    .flex_1()
                    .min_w_0()
                    .overflow_x_scroll()
                    .child(strip),
            )
            .child(controls);

        let (height, body): (f32, Option<AnyElement>) = match mode {
            LogPanelMode::Collapsed => (LOG_COLLAPSED_HEIGHT, None),
            LogPanelMode::Expanded => (
                self.dock_max_height(),
                Some(self.dock_body(shown_log, shown_shell)),
            ),
            LogPanelMode::Normal => (
                self.settings
                    .log_height
                    .clamp(LOG_MIN_HEIGHT, self.dock_max_height()),
                Some(self.dock_body(shown_log, shown_shell)),
            ),
        };
        let collapsed = mode == LogPanelMode::Collapsed;
        let mut panel = v_flex()
            .w_full()
            .flex_1()
            .min_h_0()
            .bg(theme::WELL)
            .px(px(14.0))
            .py(px(if collapsed { 6.0 } else { 8.0 }))
            .gap(px(6.0))
            .child(header);
        if let Some(body) = body {
            panel = panel.child(theme::hline()).child(
                div()
                    .flex_1()
                    .min_h_0()
                    .w_full()
                    .overflow_hidden()
                    .child(body),
            );
        }
        // The top edge drags to resize (in the normal mode).
        let edge = if mode == LogPanelMode::Normal {
            div()
                .id("dock-edge")
                .w_full()
                .flex_none()
                .py(px(2.0))
                .bg(theme::BORDER)
                .cursor_row_resize()
                .on_mouse_down(MouseButton::Left, on(Message::DragStart(Drag::Dock)))
                .child(div().h(px(1.0)).w_full())
                .into_any_element()
        } else {
            div()
                .w_full()
                .flex_none()
                .h(px(1.0))
                .bg(theme::BORDER)
                .into_any_element()
        };
        Some(
            v_flex()
                .w_full()
                .h(px(height))
                .flex_none()
                .child(edge)
                .child(panel)
                .into_any_element(),
        )
    }

    fn dock_body(&self, shown_log: Option<usize>, shown_shell: Option<usize>) -> AnyElement {
        let popup = self.popup.as_ref().map(|p| &p.kind);
        let log = shown_log.and_then(|i| self.logs.get(i).map(|l| l.view(i, popup)));
        let shell = shown_shell
            .and_then(|i| self.shells.get(i))
            .map(ShellView::view);
        match (log, shell) {
            // Logs get the wider side; a hairline and a gutter separate the two.
            (Some(l), Some(s)) => h_flex()
                .size_full()
                .gap(px(10.0))
                .child(div().w(relative(0.58)).h_full().min_w_0().overflow_hidden().child(l))
                .child(div().w(px(1.0)).h_full().flex_none().bg(theme::BORDER))
                .child(div().flex_1().h_full().min_w_0().overflow_hidden().child(s))
                .into_any_element(),
            (Some(l), None) => div().size_full().child(l).into_any_element(),
            (None, Some(s)) => div().size_full().child(s).into_any_element(),
            (None, None) => div().into_any_element(),
        }
    }

    // -----------------------------------------------------------------------
    // Dialogs
    // -----------------------------------------------------------------------

    fn view_modal(&self) -> Option<AnyElement> {
        let (card, close): (AnyElement, Option<Message>) =
            if let Some((_, kind, ns, name)) = &self.confirm_delete {
                (
                    self.view_delete(*kind, ns, name),
                    Some(Message::ConfirmDelete(false)),
                )
            } else if let Some(d) = &self.pf_dialog {
                (self.view_pf_dialog(d), Some(Message::PfCancel))
            } else if let Some(y) = &self.yaml {
                (self.view_yaml(y), None)
            } else if self.changelog_open {
                (
                    changelog::view(self.window_size),
                    Some(Message::ShowChangelog(false)),
                )
            } else {
                return None;
            };
        Some(
            div()
                .absolute()
                .inset_0()
                .child(theme::backdrop("modal-backdrop", close))
                .child(
                    div()
                        .absolute()
                        .inset_0()
                        .flex()
                        .items_center()
                        .justify_center()
                        .child(div().occlude().child(card)),
                )
                .into_any_element(),
        )
    }

    pub(super) fn dialog_title(title: impl Into<SharedString>, close: Message) -> Div {
        h_flex()
            .w_full()
            .items_center()
            .child(
                div()
                    .text_size(px(15.0))
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(theme::TEXT)
                    .child(title.into()),
            )
            .child(theme::fill_space())
            .child(theme::icon_button(
                "dialog-close",
                icon::X,
                "Close",
                Some(close),
            ))
    }

    fn view_delete(&self, kind: Kind, ns: &str, name: &str) -> AnyElement {
        let label = kind.label();
        let note = match kind {
            Kind::Pod => "If it's managed by a controller, a replacement will be created.",
            Kind::Service => "Traffic to it stops right away.",
            Kind::CronJob => "Its jobs and their pods are deleted too.",
            _ => "Its pods are deleted too. This can't be undone.",
        };
        theme::dialog()
            .p(px(16.0))
            .w(px(480.0))
            .max_w(px(480.0))
            .child(
                v_flex()
                    .gap(px(4.0))
                    .child(Self::dialog_title(
                        format!("Delete {label}?"),
                        Message::ConfirmDelete(false),
                    ))
                    .child(theme::vspace(4.0))
                    .child(theme::txt(format!("Delete {label} {ns}/{name}?")))
                    .child(theme::faint(note))
                    .child(theme::vspace(8.0))
                    .child(
                        h_flex()
                            .gap(px(8.0))
                            .child(theme::danger_button(
                                "delete-confirm",
                                Some(icon::TRASH),
                                format!("Delete {label}"),
                                Message::ConfirmDelete(true),
                            ))
                            .child(theme::secondary_button(
                                "delete-cancel",
                                None,
                                "Cancel",
                                Some(Message::ConfirmDelete(false)),
                            )),
                    ),
            )
            .into_any_element()
    }

    fn view_pf_dialog(&self, d: &PfDialog) -> AnyElement {
        let label = |t: &str| div().w(px(100.0)).flex_none().child(theme::txt(t.to_string()));
        let mut ports = h_flex().items_center().gap(px(4.0));
        for p in &d.ports {
            let selected = d.remote_port == *p;
            ports = ports.child(
                theme::ghost_button(
                    format!("pf-port-{p}"),
                    Some(Message::PfRemote(p.to_string())),
                )
                .px(px(8.0))
                .py(px(4.0))
                .when(selected, |b| b.bg(theme::ACCENT_BG).text_color(theme::ACCENT_TEXT))
                .child(div().text_size(px(theme::BODY)).child(p.to_string())),
            );
        }
        let (remote_input, local_input) = match &self.chrome.pf {
            Some(pf) => (
                Input::new(&pf.remote).w(px(90.0)).into_any_element(),
                Input::new(&pf.local).w(px(90.0)).into_any_element(),
            ),
            None => (div().into_any_element(), div().into_any_element()),
        };
        ports = ports.child(remote_input);
        theme::dialog()
            .p(px(16.0))
            .w(px(520.0))
            .max_w(px(520.0))
            .child(
                v_flex()
                    .gap(px(10.0))
                    .child(Self::dialog_title("Start port forward", Message::PfCancel))
                    .child(theme::vspace(6.0))
                    .child(
                        h_flex()
                            .items_center()
                            .child(label("Cluster"))
                            .child(theme::txt(d.context.clone())),
                    )
                    .child(
                        h_flex()
                            .items_center()
                            .child(label("Target"))
                            .child(theme::semibold(format!(
                                "{}/{}",
                                d.namespace,
                                d.target.label()
                            ))),
                    )
                    .child(
                        h_flex()
                            .items_center()
                            .child(label("Remote port"))
                            .child(ports),
                    )
                    .child(
                        h_flex()
                            .items_center()
                            .child(label("Local port"))
                            .child(local_input)
                            .child(theme::hspace(8.0))
                            .child(theme::faint("0 = random")),
                    )
                    .child(theme::vspace(8.0))
                    .child(
                        h_flex()
                            .gap(px(8.0))
                            .child(theme::primary_button(
                                "pf-start",
                                Some(icon::PLAY),
                                "Start forward",
                                Some(Message::PfStart),
                            ))
                            .child(theme::secondary_button(
                                "pf-cancel",
                                None,
                                "Cancel",
                                Some(Message::PfCancel),
                            )),
                    ),
            )
            .into_any_element()
    }

    // -----------------------------------------------------------------------
    // Popups
    // -----------------------------------------------------------------------

    fn view_popup(&self, cx: &gpui::App) -> Option<AnyElement> {
        let p = self.popup.as_ref()?;
        let (content, min_w): (AnyElement, f32) = match &p.kind {
            PopupKind::NewTab => (self.popup_new_tab(None), 260.0),
            PopupKind::NewTabContexts(path) => (self.popup_new_tab(Some(path)), 260.0),
            PopupKind::Kubeconfig => (self.popup_kubeconfig(), 240.0),
            PopupKind::Context => (self.popup_context(&p.query), 240.0),
            PopupKind::Namespace => (self.popup_namespace(&p.query), 330.0),
            PopupKind::Drawer => (self.popup_drawer(), 200.0),
            PopupKind::ShellContainers => (self.popup_shell(), 180.0),
            PopupKind::Forward(id) => (self.popup_forward(*id), 200.0),
            PopupKind::LogTail(i) => (self.popup_log_tail(*i), 100.0),
            PopupKind::LogContainer(i) => (self.popup_log_container(*i), 160.0),
        };
        // Below the trigger when its bounds are known, else where the pointer was.
        let a = cx
            .try_global::<TriggerBounds>()
            .and_then(|t| t.0.get(&trigger_key(&p.kind)).copied())
            .unwrap_or(p.anchor);
        let width = a.width.max(min_w);
        let below = a.y + a.height + 4.0;
        let max_h = (self.window_size.height - below - 12.0).max(160.0).min(460.0);
        let menu = theme::card()
            .id("popup-menu")
            .w(px(width))
            .max_h(px(max_h))
            .p(px(6.0))
            .overflow_y_scroll()
            .occlude()
            .child(content);
        // Placed in window coordinates (the space the trigger was measured in), so no
        // parent's offset can shift it; it stays on screen near the edges.
        let target = gpui::Bounds::new(
            gpui::point(px(a.x), px(a.y)),
            gpui::size(px(a.width), px(a.height)),
        );
        let menu = gpui::deferred(
            gpui::anchored()
                .attach(target, gpui::Anchor::BottomLeft)
                .offset(gpui::point(px(0.0), px(4.0)))
                .snap_to_window_with_margin(px(8.0))
                .child(menu),
        )
        .with_priority(1);
        Some(
            div()
                .absolute()
                .inset_0()
                .child(
                    div()
                        .id("popup-backdrop")
                        .absolute()
                        .inset_0()
                        .occlude()
                        .on_mouse_down(MouseButton::Left, on(Message::ClosePopup)),
                )
                .child(menu)
                .into_any_element(),
        )
    }

    fn search_input(&self) -> AnyElement {
        match &self.chrome.popup_search {
            Some(input) => Input::new(input)
                .prefix(search_prefix())
                .w_full()
                .into_any_element(),
            None => div().into_any_element(),
        }
    }

    fn popup_new_tab(&self, expanded: Option<&PathBuf>) -> AnyElement {
        let mut col = v_flex().gap(px(2.0)).child(
            div()
                .px(px(8.0))
                .py(px(2.0))
                .child(theme::faint("Open cluster in new tab")),
        );
        for path in &self.settings.kubeconfigs {
            let open = expanded == Some(path);
            let key = path.display().to_string();
            col = col.child(theme::menu_entry(
                format!("nt-{key}"),
                Some(if open { icon::CARET_DOWN } else { icon::FILE }),
                display_path(path),
                theme::TEXT,
                Some(Message::NewTabExpand(path.clone())),
            ));
            if open {
                let list = match kube_ops::read_kubeconfig(path) {
                    Ok(info) => v_flex()
                        .children(info.contexts.into_iter().map(|c| {
                            theme::menu_entry(
                                format!("nt-{key}-{c}"),
                                None,
                                c.clone(),
                                theme::TEXT,
                                Some(Message::AddWorkspace(path.clone(), Some(c))),
                            )
                        }))
                        .into_any_element(),
                    Err(e) => theme::colored(err_string(e), RED).into_any_element(),
                };
                col = col.child(div().pl(px(18.0)).child(list));
            }
        }
        col.child(theme::hline())
            .child(theme::menu_entry(
                "nt-add",
                Some(icon::FOLDER_OPEN),
                "Add kubeconfig file…",
                theme::TEXT,
                Some(Message::BrowseNewTab),
            ))
            .into_any_element()
    }

    fn popup_kubeconfig(&self) -> AnyElement {
        let active = self.ws().map(|w| w.kubeconfig_path.clone());
        let mut col = v_flex().gap(px(2.0));
        for p in &self.settings.kubeconfigs {
            col = col.child(theme::option_entry(
                format!("kc-{}", p.display()),
                display_path(p),
                active.as_ref() == Some(p),
                Message::UseKubeconfig(p.clone()),
            ));
        }
        col.child(theme::hline())
            .child(theme::menu_entry(
                "kc-add",
                Some(icon::PLUS),
                "Add kubeconfig…",
                theme::TEXT,
                Some(Message::BrowseKubeconfig),
            ))
            .into_any_element()
    }

    fn no_matches() -> Div {
        div()
            .px(px(8.0))
            .py(px(4.0))
            .child(theme::faint("No matches"))
    }

    fn popup_context(&self, query: &str) -> AnyElement {
        let Some(ws) = self.ws() else {
            return div().into_any_element();
        };
        let q = query.to_lowercase();
        let mut col = v_flex()
            .gap(px(4.0))
            .child(self.search_input())
            .child(theme::hline());
        let mut any = false;
        for c in ws.kubeconfig.iter().flat_map(|k| k.contexts.iter()) {
            if !c.to_lowercase().contains(&q) {
                continue;
            }
            any = true;
            col = col.child(theme::option_entry(
                format!("ctx-{c}"),
                c.clone(),
                ws.context.as_ref() == Some(c),
                Message::SwitchContext(c.clone()),
            ));
        }
        if !any {
            col = col.child(Self::no_matches());
        }
        col.into_any_element()
    }

    /// Clicking a name selects only that namespace; ticking a checkbox adds it to (or
    /// removes it from) the selection and keeps the list open.
    fn popup_namespace(&self, query: &str) -> AnyElement {
        let Some(ws) = self.ws() else {
            return div().into_any_element();
        };
        let selected = &ws.selected_ns;
        let q = query.to_lowercase();
        let mut hint = h_flex().items_center().gap(px(6.0)).px(px(4.0)).child(
            div()
                .text_size(px(theme::SMALL))
                .text_color(theme::TEXT_FAINT)
                .whitespace_nowrap()
                .child("Tick to add · click a name to pick only it"),
        );
        if selected.len() > 1 {
            hint = hint.child(theme::fill_space()).child(theme::small_button(
                "ns-clear",
                None,
                "Clear",
                Some(Message::SetNamespaces(vec![selected[0].clone()])),
            ));
        }
        let mut col = v_flex()
            .gap(px(4.0))
            .child(self.search_input())
            .child(hint)
            .child(theme::hline());
        if q.is_empty() {
            col = col.child(theme::option_entry(
                "ns-all",
                "All namespaces",
                selected.is_empty(),
                Message::PickNamespaces(Vec::new()),
            ));
        }
        let mut any = false;
        for ns in &ws.namespaces {
            if !ns.to_lowercase().contains(&q) {
                continue;
            }
            any = true;
            let checked = selected.contains(ns);
            let mut next: Vec<String> = selected.clone();
            if checked {
                next.retain(|n| n != ns);
            } else {
                next.push(ns.clone());
            }
            // Unticking the last one keeps it rather than jumping to "all".
            let toggle = if next.is_empty() {
                Message::Noop
            } else {
                Message::SetNamespaces(next)
            };
            let only = selected.len() == 1 && selected[0] == *ns;
            col = col.child(
                h_flex()
                    .items_center()
                    .child(
                        div()
                            .px(px(6.0))
                            .flex_none()
                            .child(theme::checkbox(format!("ns-check-{ns}"), checked, "", toggle)),
                    )
                    .child(div().flex_1().min_w_0().child(theme::option_entry(
                        format!("ns-{ns}"),
                        ns.clone(),
                        only,
                        Message::PickNamespaces(vec![ns.clone()]),
                    ))),
            );
        }
        if !any {
            col = col.child(Self::no_matches());
        }
        col.into_any_element()
    }

    fn popup_drawer(&self) -> AnyElement {
        let Some(ws) = self.ws() else {
            return div().into_any_element();
        };
        v_flex()
            .gap(px(2.0))
            .children(ws.drawer_menu().into_iter().enumerate().map(|(n, item)| match item {
                Some((glyph, label, color, msg)) => {
                    theme::menu_entry(format!("dm-{label}"), Some(glyph), label, color, Some(msg))
                        .into_any_element()
                }
                None => div()
                    .id(nid("dm-sep", n as u64))
                    .py(px(3.0))
                    .child(theme::hline())
                    .into_any_element(),
            }))
            .into_any_element()
    }

    fn popup_shell(&self) -> AnyElement {
        let Some(ws) = self.ws() else {
            return div().into_any_element();
        };
        v_flex()
            .gap(px(2.0))
            .children(ws.shell_menu().into_iter().map(|(label, msg)| {
                theme::menu_entry(format!("sm-{label}"), None, label, theme::TEXT, Some(msg))
            }))
            .into_any_element()
    }

    fn popup_forward(&self, id: u64) -> AnyElement {
        let Some(f) = self.forwards.iter().find(|f| f.id == id) else {
            return div().into_any_element();
        };
        let address = f
            .bound_port
            .filter(|_| f.status == PfStatus::Listening)
            .map(|port| format!("localhost:{port}"));
        v_flex()
            .gap(px(2.0))
            .child(theme::menu_entry(
                "fm-open",
                Some(icon::GLOBE),
                "Open in browser",
                theme::TEXT,
                address
                    .as_ref()
                    .map(|a| Message::OpenUrl(format!("http://{a}"))),
            ))
            .child(theme::menu_entry(
                "fm-copy",
                Some(icon::COPY),
                "Copy address",
                theme::TEXT,
                address.map(Message::Copy),
            ))
            .child(div().py(px(3.0)).child(theme::hline()))
            .child(theme::menu_entry(
                "fm-remove",
                Some(icon::TRASH),
                "Remove",
                RED,
                Some(Message::Action(Action::RemoveForward(id))),
            ))
            .into_any_element()
    }

    fn popup_log_tail(&self, i: usize) -> AnyElement {
        let current = self.logs.get(i).map(|l| l.tail);
        v_flex()
            .gap(px(2.0))
            .children([100, 500, 1000, 5000, 0].into_iter().map(|n| {
                theme::option_entry(
                    format!("tail-{n}"),
                    logs::tail_label(n),
                    current == Some(n),
                    Message::Log(i, logs::LogMsg::SetTail(n)),
                )
            }))
            .into_any_element()
    }

    fn popup_log_container(&self, i: usize) -> AnyElement {
        let Some(log) = self.logs.get(i) else {
            return div().into_any_element();
        };
        v_flex()
            .gap(px(2.0))
            .children(log.containers.iter().map(|c| {
                theme::option_entry(
                    format!("lc-{c}"),
                    c.clone(),
                    *c == log.container,
                    Message::Log(i, logs::LogMsg::SetContainer(c.clone())),
                )
            }))
            .into_any_element()
    }

    // -----------------------------------------------------------------------
    // Toasts
    // -----------------------------------------------------------------------

    fn view_toasts(&self) -> AnyElement {
        let cards = self.toasts.iter().map(|t| {
            let (color, glyph) = if t.error {
                (RED, icon::WARNING_CIRCLE)
            } else {
                (GREEN, icon::CHECK_CIRCLE)
            };
            theme::card()
                .w_full()
                .px(px(14.0))
                .py(px(10.0))
                .child(
                    h_flex()
                        .items_start()
                        .gap(px(8.0))
                        .child(
                            theme::icon(glyph)
                                .text_size(px(16.0))
                                .line_height(px(18.0))
                                .text_color(color),
                        )
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .text_size(px(theme::BODY))
                                .text_color(theme::TEXT)
                                .child(t.text.clone()),
                        ),
                )
        });
        div()
            .absolute()
            .inset_0()
            .flex()
            .flex_col()
            .items_end()
            .justify_end()
            .p(px(16.0))
            .child(v_flex().w(px(420.0)).gap(px(6.0)).children(cards))
            .into_any_element()
    }
}

/// Focuses the open popup's search field (if it has one).
pub fn focus_popup_search() -> Task {
    Task::window(|app, window, cx| {
        let searchable = matches!(
            app.popup.as_ref().map(|p| &p.kind),
            Some(PopupKind::Context | PopupKind::Namespace)
        );
        if let Some(input) = app.chrome.popup_search.clone().filter(|_| searchable) {
            input.update(cx, |s, cx| s.focus(window, cx));
        }
    })
}
