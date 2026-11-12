//! Rendering of the window: top bar, sidebar, pages, the bottom dock, dialogs, popups
//! and toasts. Cluster-tab content (lists, Overview, drawer) lives in `workspace::view`.

use iced::{
    Element, Length, Padding,
    widget::{
        Space, button, column, container, mouse_area, opaque, pin, row, scrollable, stack, text,
        text_editor, text_input,
    },
};

use super::*;
use crate::app::code_editor;
use crate::app::workspace::ViewCtx;

type El<'a> = Element<'a, Message>;

pub const POPUP_SEARCH: &str = "popup-search";
pub const YAML_EDITOR: &str = "yaml-editor";
pub const NEW_TAB_BUTTON: &str = "new-tab";
const KUBECONFIG_BUTTON: &str = "kubeconfig-picker";
const CONTEXT_BUTTON: &str = "context-picker";
const NAMESPACE_BUTTON: &str = "namespace-picker";

impl App {
    pub fn view(&self) -> El<'_> {
        let base: El<'_> = if self.workspaces.is_empty() {
            self.view_onboarding()
        } else {
            column![
                self.view_top_bar(),
                row![self.view_sidebar(), self.view_main()].height(Length::Fill)
            ]
            .into()
        };
        let mut layers = vec![base];
        if let Some(m) = self.view_modal() {
            layers.push(m);
        }
        if let Some(p) = self.view_popup() {
            layers.push(p);
        }
        if !self.toasts.is_empty() {
            layers.push(self.view_toasts());
        }
        container(stack(layers).width(Length::Fill).height(Length::Fill))
            .style(theme::content)
            .into()
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

    fn view_onboarding(&self) -> El<'_> {
        let mut col = column![
            text(icon::STEERING_WHEEL)
                .font(theme::ICONS)
                .size(56.0)
                .color(theme::ACCENT),
            theme::vspace(6.0),
            text("Welcome to Kube UI")
                .size(26.0)
                .font(theme::SEMIBOLD)
                .color(theme::TEXT),
            theme::vspace(4.0),
            theme::txt(
                "Pick a kubeconfig file to get started. You can open more clusters as tabs later."
            ),
            theme::vspace(18.0),
            container(theme::primary_button(
                Some(icon::FOLDER_OPEN),
                "Choose kubeconfig file…",
                Some(Message::BrowseNewTab),
            ))
            .width(260)
            .center_x(260),
        ]
        .align_x(iced::Center);
        if let Some(default) = default_kubeconfig() {
            col = col.push(theme::vspace(6.0));
            col = col.push(theme::secondary_button(
                None,
                format!("Use {}", display_path(&default)),
                Some(Message::AddWorkspace(default, None)),
            ));
        }
        if !self.settings.kubeconfigs.is_empty() {
            col = col.push(theme::vspace(14.0));
            col = col.push(theme::faint("Recent"));
            for p in &self.settings.kubeconfigs {
                col = col.push(
                    button(text(display_path(p)).size(theme::BODY))
                        .padding(2)
                        .style(theme::link(theme::ACCENT_HOVER))
                        .on_press(Message::AddWorkspace(p.clone(), None)),
                );
            }
        }
        container(column![
            Space::new().height(Length::FillPortion(22)),
            col,
            Space::new().height(Length::FillPortion(78))
        ])
        .center_x(Length::Fill)
        .height(Length::Fill)
        .style(theme::content)
        .into()
    }

    fn view_top_bar(&self) -> El<'_> {
        // Cluster tabs.
        let closable = self.workspaces.len() > 1;
        let mut tabs = row![
            text(icon::STEERING_WHEEL)
                .font(theme::ICONS)
                .size(18.0)
                .color(theme::TEXT_MUTED),
            text("Kube UI")
                .size(14.5)
                .font(theme::SEMIBOLD)
                .color(theme::TEXT),
            theme::hspace(10.0),
        ]
        .spacing(8)
        .align_y(iced::Center);
        for (i, ws) in self.workspaces.iter().enumerate() {
            let (color, status) = ws.conn.status();
            let selected = i == self.active;
            let title = text(ws.title())
                .size(theme::BODY)
                .font(if selected {
                    theme::SEMIBOLD
                } else {
                    theme::INTER
                })
                .color(if selected {
                    theme::TEXT
                } else {
                    theme::TEXT_MUTED
                });
            let mut content = row![
                theme::dot(color),
                theme::tip(
                    mouse_area(title)
                        .on_press(Message::ActivateWorkspace(i))
                        .on_middle_press(if closable {
                            Message::CloseWorkspace(i)
                        } else {
                            Message::Noop
                        })
                        .interaction(iced::mouse::Interaction::Pointer),
                    format!("{status}\n{}", display_path(&ws.kubeconfig_path)),
                ),
            ]
            .spacing(6)
            .align_y(iced::Center);
            if closable {
                content = content.push(theme::icon_button(
                    icon::X,
                    "Close tab",
                    Some(Message::CloseWorkspace(i)),
                ));
            } else {
                content = content.push(theme::hspace(6.0));
            }
            // The active tab shares the picker row's ground and opens into it.
            tabs = tabs.push(
                mouse_area(
                    container(content)
                        .padding(Padding {
                            left: 12.0,
                            right: 6.0,
                            top: 7.0,
                            bottom: 8.0,
                        })
                        .style(move |_| container::Style {
                            background: selected.then_some(theme::CONTENT.into()),
                            border: iced::Border {
                                radius: iced::border::Radius {
                                    top_left: 7.0,
                                    top_right: 7.0,
                                    bottom_left: 0.0,
                                    bottom_right: 0.0,
                                },
                                ..Default::default()
                            },
                            ..Default::default()
                        }),
                )
                .on_press(Message::ActivateWorkspace(i)),
            );
        }
        tabs = tabs.push(
            container(theme::icon_button(
                icon::PLUS,
                "Open another cluster in a new tab (⌘T)",
                Some(Message::OpenPopup(
                    PopupKind::NewTab,
                    widget::Id::new(NEW_TAB_BUTTON),
                )),
            ))
            .id(NEW_TAB_BUTTON),
        );
        tabs = tabs.push(theme::fill_space());
        tabs = tabs.push(self.view_update_control());
        tabs = tabs.push(theme::hspace(4.0));

        let mut col = column![container(tabs).padding(Padding {
            left: 14.0,
            right: 14.0,
            top: 8.0,
            bottom: 0.0,
        })];
        // Pickers for the active tab, on the content ground the active tab opens into.
        if let Some(ws) = self.ws() {
            let (color, status) = ws.conn.status();
            let mut pickers = row![
                theme::muted("Config"),
                container(theme::dropdown_button(
                    file_label(&ws.kubeconfig_path),
                    170.0,
                    self.popup_open(&PopupKind::Kubeconfig),
                    Message::OpenPopup(PopupKind::Kubeconfig, widget::Id::new(KUBECONFIG_BUTTON)),
                ))
                .id(KUBECONFIG_BUTTON),
                theme::muted("Context"),
                container(theme::dropdown_button(
                    ws.context.clone().unwrap_or_default(),
                    220.0,
                    self.popup_open(&PopupKind::Context),
                    Message::OpenPopup(PopupKind::Context, widget::Id::new(CONTEXT_BUTTON)),
                ))
                .id(CONTEXT_BUTTON),
                theme::muted("Namespace"),
                container(theme::dropdown_button(
                    namespaces_label(&ws.selected_ns, false),
                    220.0,
                    self.popup_open(&PopupKind::Namespace),
                    Message::OpenPopup(PopupKind::Namespace, widget::Id::new(NAMESPACE_BUTTON)),
                ))
                .id(NAMESPACE_BUTTON),
                theme::fill_space(),
            ]
            .spacing(8)
            .align_y(iced::Center);
            if ws.is_loading() {
                pickers = pickers.push(theme::faint("Loading…"));
            }
            pickers = pickers.push(theme::secondary_button(
                Some(icon::ARROW_CLOCKWISE),
                "Refresh",
                ws.client().is_some().then_some(Message::Refresh),
            ));
            pickers = pickers.push(
                row![theme::dot(color), theme::colored(status, color)]
                    .spacing(6)
                    .align_y(iced::Center),
            );
            col = col.push(
                container(pickers)
                    .padding([8, 14])
                    .width(Length::Fill)
                    .style(theme::content),
            );
        }
        container(col)
            .width(Length::Fill)
            .style(theme::chrome)
            .into()
    }

    /// Right end of the tab row: check for updates, or offer the one found.
    fn view_update_control(&self) -> El<'_> {
        let current = update::CURRENT;
        match &self.update {
            // The launch check runs silently; only a check the user asked for shows progress.
            update::UpdateState::Checking { manual: true } => {
                theme::faint("Checking for updates…").into()
            }
            update::UpdateState::Installing(tag) => {
                theme::faint(format!("Installing {tag}… Kube UI will reopen")).into()
            }
            update::UpdateState::Installed(tag) => theme::tip(
                theme::primary_button(
                    Some(icon::ARROW_CLOCKWISE),
                    "Restart to finish",
                    Some(Message::Relaunch),
                ),
                format!("{tag} is installed. Quits and reopens Kube UI."),
            ),
            update::UpdateState::Available(tag) => theme::tip(
                theme::primary_button(
                    Some(icon::ARROW_CIRCLE_UP),
                    format!("Update to {tag}"),
                    Some(Message::StartUpdate(tag.clone())),
                ),
                format!("You have {current}. Downloads {tag}, installs it and reopens Kube UI."),
            ),
            // On macOS, checking and "What's new" live in the menu bar instead.
            #[cfg(target_os = "macos")]
            _ => Space::new().into(),
            #[cfg(not(target_os = "macos"))]
            state => {
                let hint = match state {
                    update::UpdateState::UpToDate => {
                        format!("Kube UI {current} is up to date. Check again")
                    }
                    update::UpdateState::Failed(e) => format!("Last check failed: {e}"),
                    _ => format!("Kube UI {current}. Check GitHub for a newer release"),
                };
                row![
                    theme::tip(
                        button(theme::icon_label(
                            icon::ARROW_CLOCKWISE,
                            "Check for updates"
                        ))
                        .padding([3, 6])
                        .style(theme::ghost)
                        .on_press(Message::CheckUpdates),
                        hint,
                    ),
                    theme::tip(
                        button(text(format!("v{current}")).size(theme::SMALL))
                            .padding([3, 4])
                            .style(theme::link(theme::TEXT_FAINT))
                            .on_press(Message::ShowChangelog(true)),
                        "What's new",
                    ),
                ]
                .align_y(iced::Center)
                .into()
            }
        }
    }

    fn view_sidebar(&self) -> El<'_> {
        let Some(ws) = self.ws() else {
            return Space::new().into();
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
        // No spacing: child rows carry their own 2px gap so the guide line stays unbroken.
        let mut list = column![];
        for (key, glyph, label, children) in groups {
            let open = !self.settings.nav_collapsed.iter().any(|c| c == key);
            let has_selected = children.iter().any(|(t, _)| *t == ws.tab);
            // A closed group still shows that the current page lives inside it.
            list = list.push(theme::nav_group(
                glyph,
                label,
                open,
                !open && has_selected,
                Message::ToggleNavGroup(key),
            ));
            list = list.push(theme::vspace(1.0));
            if open {
                for (tab, badge) in children {
                    list = list.push(theme::nav_child(
                        tab_label(tab),
                        badge,
                        ws.tab == tab,
                        Message::SetTab(tab),
                    ));
                }
            }
            list = list.push(theme::vspace(6.0));
        }
        // Settings is pinned to the bottom of the rail; the groups scroll above it.
        container(column![
            scrollable(list)
                .direction(theme::thin())
                .style(theme::scrollbar)
                .height(Length::Fill),
            theme::vspace(6.0),
            theme::nav_item(
                icon::GEAR,
                "Settings",
                None,
                ws.tab == Tab::Settings,
                Message::SetTab(Tab::Settings)
            ),
        ])
        .width(NAV_WIDTH)
        .height(Length::Fill)
        .padding([12, 10])
        .style(theme::chrome)
        .into()
    }

    fn view_main(&self) -> El<'_> {
        let Some(ws) = self.ws() else {
            return Space::new().into();
        };
        let ctx = self.ctx();
        let page: El<'_> = match ws.tab {
            Tab::Forwards => self.view_forwards(),
            Tab::Settings => self.view_settings(),
            _ => ws.view_page(&ctx),
        };
        let page = container(page)
            .padding(Padding {
                left: 20.0,
                right: 20.0,
                top: 16.0,
                bottom: 8.0,
            })
            .width(Length::Fill)
            .height(Length::Fill)
            .style(theme::content);
        let mut upper = row![page].height(Length::Fill);
        if ws.tab.is_list() {
            if let Some(drawer) = ws.view_drawer(&ctx) {
                upper = upper.push(drawer);
            }
        }
        let mut col = column![upper];
        if let Some(dock) = self.view_dock() {
            col = col.push(dock);
        }
        col.width(Length::Fill).height(Length::Fill).into()
    }

    // -----------------------------------------------------------------------
    // Pages
    // -----------------------------------------------------------------------

    fn view_forwards(&self) -> El<'_> {
        let title = theme::title("Port Forwards");
        if self.forwards.is_empty() {
            return column![
                title,
                theme::vspace(4.0),
                theme::txt(
                    "No port forwards yet. Select a pod or service and choose Port forward."
                )
            ]
            .into();
        }
        let scroll = self.ws().map(|w| w.list_scroll).unwrap_or(0.0);
        let table = table::Table {
            id: "list",
            cols: vec![
                table::Col::fixed("Context", 200.0),
                table::Col::fixed("Namespace", 130.0),
                table::Col::fill("Target", 220.0),
                table::Col::fixed("Local / Remote", 150.0),
                table::Col::fixed("Status", 120.0),
                table::Col::fixed("Conns", 70.0),
                table::Col::fixed("", 170.0),
            ],
            sort: None,
            tab: Tab::Forwards,
            sortable: false,
            widths: &self.settings.column_widths,
            len: self.forwards.len(),
            scroll,
            hovered: self.hovered_row,
            row: move |i, w: &[f32]| {
                let f: &Forward = &self.forwards[i];
                let local = f.bound_port.unwrap_or(f.spec.local_port);
                let (color, label) = match &f.status {
                    PfStatus::Starting => (YELLOW, "Starting…"),
                    PfStatus::Listening => (GREEN, "Active"),
                    PfStatus::Stopped => (theme::IDLE, "Stopped"),
                    PfStatus::Failed(_) => (RED, "Failed"),
                };
                let status: El<'_> = {
                    let r = row![theme::dot(color), theme::colored(label, color)]
                        .spacing(6)
                        .align_y(iced::Center);
                    match &f.status {
                        PfStatus::Failed(e) => theme::tip(r, e.clone()),
                        _ => r.into(),
                    }
                };
                let menu_id = format!("pf-menu-{}", f.id);
                let controls: El<'_> = row![
                    if f.handle.is_some() {
                        theme::small_button(
                            Some(icon::STOP_CIRCLE),
                            "Stop",
                            Some(Message::Action(Action::StopForward(f.id))),
                        )
                    } else {
                        theme::small_button(
                            Some(icon::PLAY),
                            "Start",
                            Some(Message::Action(Action::RestartForward(f.id))),
                        )
                    },
                    container(theme::small_button(
                        Some(icon::DOTS_THREE),
                        "",
                        Some(Message::OpenPopup(
                            PopupKind::Forward(f.id),
                            widget::Id::from(menu_id.clone()),
                        )),
                    ))
                    .id(menu_id),
                ]
                .spacing(6)
                .align_y(iced::Center)
                .into();
                let context = table::truncated(&f.spec.context, w[0], 7.6, theme::txt);
                table::Row {
                    cells: vec![
                        theme::tip(context, display_path(&f.spec.kubeconfig)),
                        table::truncated(&f.spec.namespace, w[1], 7.6, theme::txt),
                        table::truncated(&f.spec.target.label(), w[2], 7.6, theme::txt),
                        table::truncated(
                            &format!("{local} → {}", f.spec.remote_port),
                            w[3],
                            7.5,
                            theme::mono,
                        ),
                        status,
                        theme::txt(f.connections.load(Ordering::Relaxed).to_string()).into(),
                        controls,
                    ],
                    selected: false,
                    on_click: Message::Noop,
                    on_double_click: None,
                }
            },
        };
        column![title, theme::vspace(8.0), table.view()].into()
    }

    fn view_settings(&self) -> El<'_> {
        let current = self.ws().map(|w| w.kubeconfig_path.clone());
        let mut files = column![].spacing(8);
        for p in &self.settings.kubeconfigs {
            let in_use = self
                .workspaces
                .iter()
                .filter(|w| w.kubeconfig_path == *p)
                .count();
            let mut line = row![
                theme::mono(display_path(p))
                    .color(if in_use > 0 { GREEN } else { theme::TEXT })
                    .width(Length::FillPortion(3)),
            ]
            .spacing(8)
            .align_y(iced::Center);
            let mut buttons = row![].spacing(6).align_y(iced::Center);
            if in_use > 0 {
                buttons = buttons.push(theme::faint(format!(
                    "{in_use} tab{}",
                    if in_use == 1 { "" } else { "s" }
                )));
            }
            buttons = buttons.push(theme::small_button(
                None,
                "Open in new tab",
                Some(Message::AddWorkspace(p.clone(), None)),
            ));
            if current.as_ref() != Some(p) {
                buttons = buttons.push(theme::small_button(
                    None,
                    "Use in this tab",
                    Some(Message::UseKubeconfig(p.clone())),
                ));
            }
            buttons = buttons.push(theme::small_button(
                None,
                "Remove",
                Some(Message::RemoveKubeconfig(p.clone())),
            ));
            line = line.push(container(buttons).width(Length::FillPortion(2)));
            files = files.push(line);
        }
        let mut col = column![
            theme::title("Settings"),
            theme::vspace(12.0),
            theme::label_small("Kubeconfig files"),
            theme::vspace(4.0),
            files,
            theme::vspace(6.0),
            theme::secondary_button(
                Some(icon::PLUS),
                "Add kubeconfig…",
                Some(Message::BrowseNewTab)
            ),
            theme::vspace(16.0),
            theme::checkbox(
                self.settings.live_updates,
                "Live updates (watch lists for changes)",
                Message::ToggleLive
            ),
            theme::vspace(16.0),
            row![
                theme::faint(format!("Kube UI v{}", update::CURRENT)),
                theme::small_button(
                    Some(icon::SPARKLE),
                    "What's new",
                    Some(Message::ShowChangelog(true))
                ),
            ]
            .spacing(8)
            .align_y(iced::Center),
        ];
        if let Some(p) = Settings::path() {
            col = col.push(theme::vspace(8.0));
            col = col.push(theme::faint(format!("Settings stored in {}", p.display())));
        }
        scrollable(col)
            .direction(theme::thin())
            .style(theme::scrollbar)
            .into()
    }

    // -----------------------------------------------------------------------
    // Bottom dock: logs and shells
    // -----------------------------------------------------------------------

    fn view_dock(&self) -> Option<El<'_>> {
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
                (glyph, l.title(), l.hover(), l.live())
            })
            .chain(
                self.shells
                    .iter()
                    .map(|s| (icon::TERMINAL_WINDOW, s.title(), s.hover(), s.live())),
            );
        let mut strip = row![].spacing(6).align_y(iced::Center);
        for (i, (glyph, title, hover, live)) in tabs.enumerate() {
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
            let label = row![
                text(glyph).font(theme::ICONS).size(13.0).color(color),
                text(title)
                    .size(theme::BODY)
                    .font(if selected {
                        theme::SEMIBOLD
                    } else {
                        theme::INTER
                    })
                    .color(color),
            ]
            .spacing(6)
            .align_y(iced::Center);
            let tab = container(
                row![
                    theme::tip(
                        mouse_area(label)
                            .on_press(Message::DockSelect(i))
                            .on_middle_press(Message::DockClose(i))
                            .interaction(iced::mouse::Interaction::Pointer),
                        format!("{hover}\nMiddle-click to close"),
                    ),
                    theme::icon_button(icon::X, "Close", Some(Message::DockClose(i))),
                ]
                .spacing(4)
                .align_y(iced::Center),
            )
            .padding(Padding {
                left: 10.0,
                right: 4.0,
                top: 3.0,
                bottom: 3.0,
            })
            .style(move |_| container::Style {
                background: selected.then_some(theme::FIELD.into()),
                border: iced::Border {
                    color: if selected {
                        theme::BORDER_STRONG
                    } else {
                        theme::BORDER
                    },
                    width: 1.0,
                    radius: theme::RADIUS.into(),
                },
                ..Default::default()
            });
            strip = strip.push(tab);
        }
        let mut controls = row![].spacing(4).align_y(iced::Center);
        let (glyph, hint, target) = match mode {
            LogPanelMode::Collapsed => (icon::CARET_UP, "Restore size", LogPanelMode::Normal),
            _ => (
                icon::CARET_DOWN,
                "Collapse to bottom",
                LogPanelMode::Collapsed,
            ),
        };
        controls = controls.push(theme::icon_button(
            glyph,
            hint,
            Some(Message::SetLogMode(target)),
        ));
        if can_split {
            let hint = if split {
                "Show one tab at a time"
            } else {
                "Split: logs beside the terminal"
            };
            controls = controls.push(theme::icon_toggle(
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
        controls = controls.push(theme::icon_button(
            glyph,
            hint,
            Some(Message::SetLogMode(target)),
        ));
        controls = controls.push(theme::icon_button(
            icon::X_CIRCLE,
            "Close all tabs",
            Some(Message::DockCloseAll),
        ));
        let header = row![
            scrollable(strip)
                .direction(scrollable::Direction::Horizontal(
                    scrollable::Scrollbar::new().width(3).scroller_width(3)
                ))
                .style(theme::scrollbar)
                .width(Length::Fill),
            controls
        ]
        .spacing(8)
        .align_y(iced::Center);

        let (height, body): (f32, Option<El<'_>>) = match mode {
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
        let mut col = column![header].spacing(6);
        if let Some(body) = body {
            col = col.push(theme::hline());
            col = col.push(body);
        }
        let panel = container(col)
            .padding(if mode == LogPanelMode::Collapsed {
                [6, 14]
            } else {
                [8, 14]
            })
            .width(Length::Fill)
            .height(Length::Fill)
            .style(theme::well);
        // The top edge drags to resize (in the normal mode).
        let edge: El<'_> = if mode == LogPanelMode::Normal {
            mouse_area(
                container(Space::new().height(1).width(Length::Fill))
                    .padding([2, 0])
                    .style(theme::fill(theme::BORDER)),
            )
            .on_press(Message::DragStart(Drag::Dock))
            .interaction(iced::mouse::Interaction::ResizingVertically)
            .into()
        } else {
            container(Space::new().height(1).width(Length::Fill))
                .style(theme::fill(theme::BORDER))
                .into()
        };
        Some(column![edge, panel].height(height).into())
    }

    fn dock_body(&self, shown_log: Option<usize>, shown_shell: Option<usize>) -> El<'_> {
        let popup = self.popup.as_ref().map(|p| &p.kind);
        let log = shown_log.and_then(|i| self.logs.get(i).map(|l| l.view(i, popup)));
        let shell = shown_shell
            .and_then(|i| self.shells.get(i))
            .map(ShellView::view);
        match (log, shell) {
            // Logs get the wider side; a hairline and a gutter separate the two.
            (Some(l), Some(s)) => row![
                container(l).width(Length::FillPortion(58)),
                container(Space::new().width(1).height(Length::Fill))
                    .style(theme::fill(theme::BORDER)),
                container(s).width(Length::FillPortion(42)),
            ]
            .spacing(10)
            .height(Length::Fill)
            .into(),
            (Some(l), None) => l,
            (None, Some(s)) => s,
            (None, None) => Space::new().into(),
        }
    }

    // -----------------------------------------------------------------------
    // Dialogs
    // -----------------------------------------------------------------------

    fn view_modal(&self) -> Option<El<'_>> {
        let (card, close): (El<'_>, Option<Message>) =
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
        let backdrop = mouse_area(
            container(Space::new())
                .width(Length::Fill)
                .height(Length::Fill)
                .style(theme::backdrop),
        )
        .on_press(close.unwrap_or(Message::Noop));
        Some(opaque(stack![
            backdrop,
            container(opaque(card)).center(Length::Fill)
        ]))
    }

    fn dialog_title<'a>(title: String, close: Message) -> El<'a> {
        row![
            text(title)
                .size(15.0)
                .font(theme::SEMIBOLD)
                .color(theme::TEXT),
            theme::fill_space(),
            theme::icon_button(icon::X, "Close", Some(close)),
        ]
        .align_y(iced::Center)
        .into()
    }

    fn view_delete<'a>(&self, kind: Kind, ns: &str, name: &str) -> El<'a> {
        let label = kind.label();
        let note = match kind {
            Kind::Pod => "If it's managed by a controller, a replacement will be created.",
            Kind::Service => "Traffic to it stops right away.",
            Kind::CronJob => "Its jobs and their pods are deleted too.",
            _ => "Its pods are deleted too. This can't be undone.",
        };
        container(
            column![
                Self::dialog_title(format!("Delete {label}?"), Message::ConfirmDelete(false)),
                theme::vspace(4.0),
                theme::txt(format!("Delete {label} {ns}/{name}?")),
                theme::faint(note),
                theme::vspace(8.0),
                row![
                    theme::danger_button(
                        Some(icon::TRASH),
                        format!("Delete {label}"),
                        Message::ConfirmDelete(true)
                    ),
                    theme::secondary_button(None, "Cancel", Some(Message::ConfirmDelete(false))),
                ]
                .spacing(8),
            ]
            .spacing(4),
        )
        .padding(16)
        .max_width(480)
        .style(theme::dialog)
        .into()
    }

    fn view_pf_dialog<'a>(&self, d: &'a PfDialog) -> El<'a> {
        let label = |t: &'a str| theme::txt(t).width(100);
        let mut ports = row![].spacing(4).align_y(iced::Center);
        for p in &d.ports {
            ports = ports.push(
                button(text(p.to_string()).size(theme::BODY))
                    .padding([4, 8])
                    .style(theme::menu_item(d.remote_port == *p, theme::TEXT))
                    .on_press(Message::PfRemote(p.to_string())),
            );
        }
        let field = |value: &'a str, on: fn(String) -> Message| {
            text_input("", value)
                .on_input(on)
                .on_submit(Message::PfStart)
                .size(theme::BODY)
                .padding([5, 8])
                .width(90)
                .style(theme::input)
        };
        ports = ports.push(field(&d.remote_text, Message::PfRemote));
        container(
            column![
                Self::dialog_title("Start port forward".into(), Message::PfCancel),
                theme::vspace(6.0),
                row![label("Cluster"), theme::txt(d.context.as_str())].align_y(iced::Center),
                row![
                    label("Target"),
                    theme::semibold(format!("{}/{}", d.namespace, d.target.label()))
                ]
                .align_y(iced::Center),
                row![label("Remote port"), ports].align_y(iced::Center),
                row![
                    label("Local port"),
                    field(&d.local_text, Message::PfLocal),
                    theme::hspace(8.0),
                    theme::faint("0 = random")
                ]
                .align_y(iced::Center),
                theme::vspace(8.0),
                row![
                    theme::primary_button(
                        Some(icon::PLAY),
                        "Start forward",
                        Some(Message::PfStart)
                    ),
                    theme::secondary_button(None, "Cancel", Some(Message::PfCancel)),
                ]
                .spacing(8),
            ]
            .spacing(10),
        )
        .padding(16)
        .max_width(520)
        .style(theme::dialog)
        .into()
    }

    fn view_yaml<'a>(&'a self, y: &'a YamlView) -> El<'a> {
        let title = if y.editing && y.dirty && !y.create {
            format!("{} (edited)", y.title)
        } else {
            y.title.clone()
        };
        let width = (self.window_size.width - 80.0).clamp(560.0, 1040.0);
        let height = (self.window_size.height - 80.0).clamp(360.0, 900.0);
        let mut col = column![Self::dialog_title(title, Message::YamlClose)].spacing(6);
        match &y.text {
            None => col = col.push(theme::faint("Loading…")),
            Some(Err(e)) => col = col.push(theme::colored(e.clone(), RED)),
            Some(Ok(original)) => {
                let mut bar = row![].spacing(8).align_y(iced::Center);
                if y.editing {
                    let (glyph, label) = if y.create {
                        (icon::PLUS, "Create")
                    } else {
                        (icon::FLOPPY_DISK, "Apply")
                    };
                    let can_apply = y.dirty && !y.saving && y.parse_error.is_none();
                    bar = bar.push(theme::tip(
                        theme::primary_button(
                            Some(glyph),
                            label,
                            can_apply.then_some(Message::YamlApply),
                        ),
                        "Replace the object with this YAML (⌘S)",
                    ));
                    bar = bar.push(theme::secondary_button(
                        None,
                        "Cancel",
                        (!y.saving).then_some(Message::YamlCancel),
                    ));
                    if y.saving {
                        bar = bar.push(theme::faint("Saving…"));
                    }
                } else {
                    bar = bar.push(theme::primary_button(
                        Some(icon::PENCIL_SIMPLE),
                        "Edit",
                        Some(Message::YamlEdit),
                    ));
                    bar = bar.push(theme::secondary_button(
                        Some(icon::COPY),
                        "Copy",
                        Some(Message::Copy(original.clone())),
                    ));
                }
                bar = bar.push(theme::fill_space());
                if y.create {
                    bar = bar.push(theme::faint(format!(
                        "Namespace defaults to {}",
                        y.namespace
                    )));
                } else {
                    bar = bar.push(theme::tip(
                        theme::secondary_button(
                            Some(icon::ARROW_CLOCKWISE),
                            "Reload",
                            (!y.saving).then_some(Message::YamlReload),
                        ),
                        "Fetch the latest version (discards edits)",
                    ));
                }
                col = col.push(bar);
                if self.find.open {
                    col = col.push(self.find.bar());
                }
                if y.editing && original.contains("app.kubernetes.io/managed-by: Helm") {
                    col = col.push(theme::colored(
                        "Managed by Helm: the next helm upgrade will overwrite changes made here.",
                        YELLOW,
                    ));
                }
                if let Some(e) = &y.save_error {
                    col = col.push(theme::colored(e.clone(), RED));
                    if e.contains("modified") || e.contains("Conflict") {
                        col = col.push(theme::faint(
                            "Someone changed it since you opened it. Copy your edits, Reload, and apply them again.",
                        ));
                    }
                }
                if let Some((line, msg)) = y.parse_error.as_ref().filter(|_| y.editing) {
                    col = col.push(code_editor::error_line(*line, msg));
                }
                col = col.push(self.yaml_editor(y));
            }
        }
        container(col)
            .padding(16)
            .width(width)
            .height(height)
            .style(theme::dialog)
            .into()
    }

    fn yaml_editor<'a>(&'a self, y: &'a YamlView) -> El<'a> {
        let lines = y.content.line_count();
        let longest = y
            .content
            .lines()
            .map(|l| l.text.chars().count())
            .max()
            .unwrap_or(0);
        let width =
            (longest as f32 + 4.0) * code_editor::CHAR_WIDTH + 2.0 * code_editor::EDITOR_PAD;
        let cursor_line = Some(y.content.cursor().position.line);
        let error_line = y
            .parse_error
            .as_ref()
            .map(|(l, _)| *l)
            .filter(|_| y.editing);
        let editor = text_editor(&y.content)
            .id(YAML_EDITOR)
            .on_action(Message::YamlAction)
            .highlight_with::<code_editor::Highlighter>(
                self.find.highlight(),
                code_editor::to_format,
            )
            .key_binding(code_editor::key_binding)
            .font(theme::MONO)
            .size(theme::MONO_SIZE)
            .line_height(text::LineHeight::Absolute(code_editor::LINE_HEIGHT.into()))
            .padding(code_editor::EDITOR_PAD)
            .wrapping(text::Wrapping::None)
            // Unbounded height inside the scrollable: the editor then leaves wheel events to
            // the scrollable (with a fixed height it takes them and nothing scrolls).
            .height(Length::Shrink)
            .width(width.max(400.0))
            .style(theme::editor);
        container(
            scrollable(row![
                code_editor::gutter(lines, cursor_line, error_line),
                container(Space::new().width(1).height(Length::Fill))
                    .style(theme::fill(theme::BORDER)),
                editor
            ])
            .id(code_editor::YAML_SCROLL)
            .direction(theme::thin_both())
            .style(theme::scrollbar)
            .width(Length::Fill)
            .height(Length::Fill),
        )
        .style(theme::boxed(theme::CONTENT, theme::BORDER, 4.0))
        .height(Length::Fill)
        .into()
    }

    // -----------------------------------------------------------------------
    // Popups
    // -----------------------------------------------------------------------

    fn view_popup(&self) -> Option<El<'_>> {
        let p = self.popup.as_ref()?;
        let (content, min_w): (El<'_>, f32) = match &p.kind {
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
        let a = p.anchor;
        let width = a.width.max(min_w);
        let x = a.x.min(self.window_size.width - width - 8.0).max(8.0);
        let below = a.y + a.height + 4.0;
        let max_h = (self.window_size.height - below - 12.0).max(160.0);
        let menu = container(
            scrollable(content)
                .direction(theme::thin())
                .style(theme::scrollbar),
        )
        .padding(6)
        .width(width)
        .max_height(max_h.min(460.0))
        .style(theme::card);
        let backdrop = mouse_area(Space::new().width(Length::Fill).height(Length::Fill))
            .on_press(Message::ClosePopup);
        Some(stack![backdrop, pin(opaque(menu)).x(x).y(below)].into())
    }

    fn search_input<'a>(query: &str) -> El<'a> {
        theme::search_field(query, "Search…", 0.0, Message::PopupQuery)
            .id(POPUP_SEARCH)
            .on_submit(Message::PopupSubmit)
            .width(Length::Fill)
            .into()
    }

    fn popup_new_tab(&self, expanded: Option<&PathBuf>) -> El<'_> {
        let mut col =
            column![container(theme::faint("Open cluster in new tab")).padding([2, 8])].spacing(2);
        for path in &self.settings.kubeconfigs {
            let open = expanded == Some(path);
            col = col.push(theme::menu_entry(
                Some(if open { icon::CARET_DOWN } else { icon::FILE }),
                display_path(path),
                theme::TEXT,
                Some(Message::NewTabExpand(path.clone())),
            ));
            if open {
                let list: El<'_> = match kube_ops::read_kubeconfig(path) {
                    Ok(info) => column(info.contexts.into_iter().map(|c| {
                        theme::menu_entry(
                            None,
                            c.clone(),
                            theme::TEXT,
                            Some(Message::AddWorkspace(path.clone(), Some(c))),
                        )
                    }))
                    .into(),
                    Err(e) => theme::colored(err_string(e), RED).into(),
                };
                col = col.push(container(list).padding(Padding {
                    left: 18.0,
                    ..Default::default()
                }));
            }
        }
        col = col.push(theme::hline());
        col = col.push(theme::menu_entry(
            Some(icon::FOLDER_OPEN),
            "Add kubeconfig file…",
            theme::TEXT,
            Some(Message::BrowseNewTab),
        ));
        col.into()
    }

    fn popup_kubeconfig(&self) -> El<'_> {
        let active = self.ws().map(|w| w.kubeconfig_path.clone());
        let mut col = column![].spacing(2);
        for p in &self.settings.kubeconfigs {
            col = col.push(theme::option_entry(
                display_path(p),
                active.as_ref() == Some(p),
                Message::UseKubeconfig(p.clone()),
            ));
        }
        col = col.push(theme::hline());
        col = col.push(theme::menu_entry(
            Some(icon::PLUS),
            "Add kubeconfig…",
            theme::TEXT,
            Some(Message::BrowseKubeconfig),
        ));
        col.into()
    }

    fn popup_context(&self, query: &str) -> El<'_> {
        let Some(ws) = self.ws() else {
            return Space::new().into();
        };
        let q = query.to_lowercase();
        let mut col = column![Self::search_input(query), theme::hline()].spacing(4);
        let mut any = false;
        for c in ws.kubeconfig.iter().flat_map(|k| k.contexts.iter()) {
            if !c.to_lowercase().contains(&q) {
                continue;
            }
            any = true;
            col = col.push(theme::option_entry(
                c.clone(),
                ws.context.as_ref() == Some(c),
                Message::SwitchContext(c.clone()),
            ));
        }
        if !any {
            col = col.push(container(theme::faint("No matches")).padding([4, 8]));
        }
        col.into()
    }

    /// Clicking a name selects only that namespace; ticking a checkbox adds it to (or
    /// removes it from) the selection and keeps the list open.
    fn popup_namespace(&self, query: &str) -> El<'_> {
        let Some(ws) = self.ws() else {
            return Space::new().into();
        };
        let selected = &ws.selected_ns;
        let q = query.to_lowercase();
        let mut hint = row![
            text("Tick to add · click a name to pick only it")
                .size(theme::SMALL)
                .color(theme::TEXT_FAINT)
        ]
        .spacing(6)
        .align_y(iced::Center);
        if selected.len() > 1 {
            hint = hint.push(theme::fill_space());
            hint = hint.push(theme::small_button(
                None,
                "Clear",
                Some(Message::SetNamespaces(vec![selected[0].clone()])),
            ));
        }
        let mut col = column![
            Self::search_input(query),
            container(hint).padding([0, 4]),
            theme::hline()
        ]
        .spacing(4);
        if q.is_empty() {
            col = col.push(theme::option_entry(
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
            col = col.push(
                row![
                    container(theme::checkbox(checked, "", toggle)).padding([0, 6]),
                    theme::option_entry(
                        ns.clone(),
                        only,
                        Message::PickNamespaces(vec![ns.clone()])
                    ),
                ]
                .align_y(iced::Center),
            );
        }
        if !any {
            col = col.push(container(theme::faint("No matches")).padding([4, 8]));
        }
        col.into()
    }

    fn popup_drawer(&self) -> El<'_> {
        let Some(ws) = self.ws() else {
            return Space::new().into();
        };
        column(ws.drawer_menu().into_iter().map(|item| match item {
            Some((glyph, label, color, msg)) => {
                theme::menu_entry(Some(glyph), label, color, Some(msg))
            }
            None => container(theme::hline()).padding([3, 0]).into(),
        }))
        .spacing(2)
        .into()
    }

    fn popup_shell(&self) -> El<'_> {
        let Some(ws) = self.ws() else {
            return Space::new().into();
        };
        column(
            ws.shell_menu()
                .into_iter()
                .map(|(label, msg)| theme::menu_entry(None, label, theme::TEXT, Some(msg))),
        )
        .spacing(2)
        .into()
    }

    fn popup_forward(&self, id: u64) -> El<'_> {
        let Some(f) = self.forwards.iter().find(|f| f.id == id) else {
            return Space::new().into();
        };
        let address = f
            .bound_port
            .filter(|_| f.status == PfStatus::Listening)
            .map(|port| format!("localhost:{port}"));
        column![
            theme::menu_entry(
                Some(icon::GLOBE),
                "Open in browser",
                theme::TEXT,
                address
                    .as_ref()
                    .map(|a| Message::OpenUrl(format!("http://{a}"))),
            ),
            theme::menu_entry(
                Some(icon::COPY),
                "Copy address",
                theme::TEXT,
                address.map(Message::Copy),
            ),
            container(theme::hline()).padding([3, 0]),
            theme::menu_entry(
                Some(icon::TRASH),
                "Remove",
                RED,
                Some(Message::Action(Action::RemoveForward(id))),
            ),
        ]
        .spacing(2)
        .into()
    }

    fn popup_log_tail(&self, i: usize) -> El<'_> {
        let current = self.logs.get(i).map(|l| l.tail);
        column([100, 500, 1000, 5000, 0].into_iter().map(|n| {
            theme::option_entry(
                logs::tail_label(n),
                current == Some(n),
                Message::Log(i, logs::LogMsg::SetTail(n)),
            )
        }))
        .spacing(2)
        .into()
    }

    fn popup_log_container(&self, i: usize) -> El<'_> {
        let Some(log) = self.logs.get(i) else {
            return Space::new().into();
        };
        column(log.containers.iter().map(|c| {
            theme::option_entry(
                c.clone(),
                *c == log.container,
                Message::Log(i, logs::LogMsg::SetContainer(c.clone())),
            )
        }))
        .spacing(2)
        .into()
    }

    // -----------------------------------------------------------------------
    // Toasts
    // -----------------------------------------------------------------------

    fn view_toasts(&self) -> El<'_> {
        let cards = self.toasts.iter().map(|t| {
            let (color, glyph) = if t.error {
                (RED, icon::WARNING_CIRCLE)
            } else {
                (GREEN, icon::CHECK_CIRCLE)
            };
            container(
                row![
                    text(glyph).font(theme::ICONS).size(16.0).color(color),
                    theme::txt(t.text.as_str()).width(Length::Fill),
                ]
                .spacing(8),
            )
            .padding([10, 14])
            .width(Length::Fill)
            .style(theme::card)
            .into()
        });
        container(column(cards).spacing(6).width(420))
            .align_right(Length::Fill)
            .align_bottom(Length::Fill)
            .padding(16)
            .into()
    }
}
