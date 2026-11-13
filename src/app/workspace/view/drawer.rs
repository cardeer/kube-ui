//! The detail drawer beside a list: tabs, per-kind details, containers with their env,
//! volumes, events and endpoints.

use gpui_kit::component::{h_flex, scroll::ScrollableElement as _, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, Div, InteractiveElement as _, IntoElement as _, MouseButton, ParentElement as _,
    StatefulInteractiveElement as _, Styled as _, div, px, relative,
};

use super::*;

impl Workspace {
    fn drawer_tabs(&self, sel: &Selected) -> Vec<(DrawerTab, String)> {
        let details = self.loaded_details();
        let n = |label: &str, count: Option<usize>| match count {
            Some(c) => format!("{label} {c}"),
            None => label.to_string(),
        };
        let events = n(
            "Events",
            details.and_then(|d| d.events.as_ref().ok()).map(Vec::len),
        );
        let mut tabs = vec![(DrawerTab::Overview, "Overview".to_string())];
        match (sel.kind, details.map(|d| &d.body)) {
            (Kind::Pod, body) => {
                let pod = match body {
                    Some(Body::Pod(p)) => Some(p),
                    _ => None,
                };
                tabs.push((
                    DrawerTab::Containers,
                    n("Containers", pod.map(|p| p.containers.len())),
                ));
                tabs.push((
                    DrawerTab::Volumes,
                    n("Volumes", pod.map(|p| p.volumes.len())),
                ));
            }
            (Kind::Service, body) => {
                let count = match body {
                    Some(Body::Service(s)) => Some(s.endpoints.len()),
                    _ => None,
                };
                tabs.push((DrawerTab::Endpoints, n("Endpoints", count)));
            }
            _ => {}
        }
        tabs.push((DrawerTab::Events, events));
        tabs
    }

    /// The drawer beside the list, or `None` when nothing is selected.
    pub(in crate::app) fn view_drawer(&self, ctx: &ViewCtx<'_>) -> Option<El> {
        let sel = self.selected.as_ref()?;
        let loading = self.details.as_ref().is_some_and(|d| d.loading);
        let header = h_flex()
            .w_full()
            .gap(px(4.0))
            .items_start()
            .child(
                theme::semibold(sel.name.clone())
                    .text_size(px(16.5))
                    .whitespace_normal()
                    .flex_1()
                    .min_w_0(),
            )
            .when(loading, |d| {
                d.child(theme::icon(icon::SPINNER).text_color(theme::TEXT_FAINT))
            })
            .child(theme::icon_button(
                "drawer-copy",
                icon::COPY,
                "Copy name",
                Some(Message::Copy(sel.name.clone())),
            ))
            .child(theme::icon_button(
                "drawer-close",
                icon::X,
                "Close (Esc)",
                Some(Message::CloseDrawer),
            ));
        let subtitle = h_flex()
            .gap(px(8.0))
            .child(theme::icon(kind_glyph(sel.kind)))
            .child(theme::muted(format!("{} · {}", sel.kind.label(), sel.namespace)));
        let tabs = self.drawer_tabs(sel);
        let current = tabs
            .iter()
            .position(|(t, _)| *t == self.drawer_tab)
            .unwrap_or(0);
        let tab_ids: Vec<DrawerTab> = tabs.iter().map(|(t, _)| *t).collect();
        let strip = theme::underline_tabs(
            "drawer-tabs",
            tabs.into_iter().map(|(_, l)| l).collect(),
            current,
            move |i| Message::DrawerTab(tab_ids[i]),
        );
        let body: El = match (sel.kind, self.drawer_tab) {
            (_, DrawerTab::Events) => self.drawer_events(),
            (Kind::Pod, DrawerTab::Containers) => self.drawer_pod_containers(sel, ctx),
            (Kind::Pod, DrawerTab::Volumes) => self.drawer_pod_volumes(),
            (Kind::Service, DrawerTab::Endpoints) => self.drawer_service_endpoints(),
            (Kind::Pod, _) => self.drawer_pod(sel, ctx),
            (Kind::Service, _) => self.drawer_service(sel, ctx),
            (Kind::Deployment, _) => self.drawer_deployment(sel, ctx),
            _ => self.drawer_workload(sel, ctx),
        };
        let scroller = div().flex_1().min_h_0().w_full().child(
            v_flex()
                .w_full()
                .pr(px(10.0))
                .items_stretch()
                .child(body)
                .child(theme::vspace(12.0))
                .overflow_y_scrollbar(),
        );
        let content = v_flex()
            .flex_1()
            .min_w_0()
            .h_full()
            .items_stretch()
            .px(px(18.0))
            .py(px(14.0))
            .bg(theme::RAISED)
            .child(header)
            .child(theme::vspace(2.0))
            .child(subtitle)
            .child(theme::vspace(14.0))
            .child(strip)
            .child(scroller);
        // The left edge drags to resize.
        let handle = div()
            .id("drawer-grip")
            .w(px(5.0))
            .h_full()
            .flex_none()
            .flex()
            .justify_center()
            .bg(theme::RAISED)
            .cursor_col_resize()
            .child(div().w(px(1.0)).h_full().bg(theme::BORDER))
            .on_mouse_down(MouseButton::Left, on(Message::DragStart(Drag::Drawer)));
        Some(
            h_flex()
                .w(px(ctx.drawer_width))
                .h_full()
                .flex_none()
                .items_stretch()
                .child(handle)
                .child(content)
                .into_any_element(),
        )
    }

    /// Like `loaded_details`, but shows a message while they aren't available.
    fn details_or_status(&self) -> Result<&Details, El> {
        match self.details.as_ref().and_then(|d| d.data.as_ref()) {
            Some(Ok(d)) => Ok(d),
            Some(Err(e)) => Err(theme::colored(e.clone(), RED)
                .whitespace_normal()
                .into_any_element()),
            None => Err(theme::faint("Loading details…").into_any_element()),
        }
    }

    /// The "⋯" button that opens the drawer's menu.
    fn drawer_menu_button(&self, ctx: &ViewCtx<'_>) -> El {
        let open = ctx.popup == Some(&PopupKind::Drawer);
        let button = theme::secondary_button(
            "drawer-menu",
            Some(icon::DOTS_THREE),
            "",
            Some(Message::OpenPopup(PopupKind::Drawer)),
        )
        .px(px(9.0))
        .pr(px(3.0))
        .when(open, |b| b.bg(theme::FIELD_ACTIVE).border_color(theme::ACCENT));
        crate::app::view::trigger(PopupKind::Drawer, theme::tip(button, "More actions"))
            .into_any_element()
    }

    /// The drawer's "⋯" menu: (icon, label, colour, message), `None` for a separator.
    pub(in crate::app) fn drawer_menu(
        &self,
    ) -> Vec<Option<(&'static str, String, Color, Message)>> {
        let Some(sel) = &self.selected else {
            return Vec::new();
        };
        let (kind, ns, name) = (sel.kind, sel.namespace.clone(), sel.name.clone());
        let mut items = Vec::new();
        match kind {
            Kind::Deployment => {
                items.push(Some((
                    icon::ARROW_COUNTER_CLOCKWISE,
                    "Rollout restart".to_string(),
                    theme::TEXT,
                    act(Action::Restart(kind, ns.clone(), name.clone())),
                )));
                // Paused state comes with the details; offer the toggle once it's known.
                if let Some(Body::Deployment(det)) = self.loaded_details().map(|d| &d.body) {
                    let (glyph, label) = if det.paused {
                        (icon::PLAY, "Resume rollout")
                    } else {
                        (icon::PAUSE, "Pause rollout")
                    };
                    items.push(Some((
                        glyph,
                        label.to_string(),
                        theme::TEXT,
                        act(Action::Pause(ns.clone(), name.clone(), !det.paused)),
                    )));
                }
                items.push(None);
            }
            k if k.restartable() => {
                items.push(Some((
                    icon::ARROW_COUNTER_CLOCKWISE,
                    "Rollout restart".to_string(),
                    theme::TEXT,
                    act(Action::Restart(kind, ns.clone(), name.clone())),
                )));
                items.push(None);
            }
            _ => {}
        }
        // Edit YAML, View YAML and Delete: the end of every drawer's menu.
        items.push(Some((
            icon::PENCIL_SIMPLE,
            "Edit YAML".to_string(),
            theme::TEXT,
            act(Action::EditYaml(kind, ns.clone(), name.clone())),
        )));
        items.push(Some((
            icon::FILE_CODE,
            "View YAML".to_string(),
            theme::TEXT,
            act(Action::Yaml(kind, ns.clone(), name.clone())),
        )));
        items.push(None);
        items.push(Some((
            icon::TRASH,
            format!("Delete {}", kind.label().to_lowercase()),
            RED,
            act(Action::AskDelete(kind, ns, name)),
        )));
        items
    }

    /// The selected pod's regular containers, for the Shell menu.
    pub(in crate::app) fn shell_menu(&self) -> Vec<(String, Message)> {
        let Some(sel) = &self.selected else {
            return Vec::new();
        };
        let Some(p) = self
            .pods
            .items
            .iter()
            .find(|p| p.namespace == sel.namespace && p.name == sel.name)
        else {
            return Vec::new();
        };
        p.containers
            .iter()
            .filter(|c| !c.init)
            .map(|c| {
                (
                    c.name.clone(),
                    act(Action::Shell(p.clone(), Some(c.name.clone()))),
                )
            })
            .collect()
    }

    fn drawer_pod(&self, sel: &Selected, ctx: &ViewCtx<'_>) -> El {
        let Some(p) = self
            .pods
            .items
            .iter()
            .find(|p| p.namespace == sel.namespace && p.name == sel.name)
        else {
            return theme::faint(if self.pods.loaded {
                "This pod no longer exists."
            } else {
                "Loading…"
            })
            .into_any_element();
        };
        let details = match self.loaded_details().map(|d| &d.body) {
            Some(Body::Pod(d)) => Some(d),
            _ => None,
        };

        let color = status_color(&p.status);
        let summary = h_flex()
            .gap(px(8.0))
            .child(theme::dot(color))
            .child(theme::colored(p.status.clone(), color))
            .child(theme::vline(14.0))
            .child(theme::txt(format!("Ready {}", p.ready)))
            .child(theme::vline(14.0))
            .child(theme::txt(format!("Restarts {}", p.restarts)))
            .child(theme::vline(14.0))
            .child(theme::txt(format!("Age {}", p.age())));

        // A pod that restarted offers the previous run's logs; when it's failing that's
        // where the reason is, so it takes the one primary button.
        let restarted = p.restarts > 0;
        let failing = pod_health(&p.status) == Health::Bad;
        let previous_hint = match p.containers.iter().find(|c| !c.last_exit.is_empty()) {
            Some(c) => format!(
                "Logs from the run before the last restart\nLast run: {}",
                c.last_exit
            ),
            None => "Logs from the run before the last restart".to_string(),
        };
        let logs = act(Action::OpenLogs(p.clone(), None, false));
        let previous = act(Action::OpenLogs(p.clone(), None, true));
        let mut actions = wrap_row();
        if restarted && failing {
            actions = actions
                .child(theme::tip(
                    theme::primary_button(
                        "previous-logs",
                        Some(icon::CLOCK_COUNTER_CLOCKWISE),
                        "Previous logs",
                        Some(previous),
                    ),
                    previous_hint,
                ))
                .child(theme::secondary_button(
                    "logs",
                    Some(icon::SCROLL),
                    "Logs",
                    Some(logs),
                ));
        } else {
            actions = actions.child(theme::primary_button(
                "logs",
                Some(icon::SCROLL),
                "Logs",
                Some(logs),
            ));
            if restarted {
                actions = actions.child(theme::tip(
                    theme::secondary_button(
                        "previous-logs",
                        Some(icon::CLOCK_COUNTER_CLOCKWISE),
                        "Previous logs",
                        Some(previous),
                    ),
                    previous_hint,
                ));
            }
        }
        if !p.owner.is_empty() {
            actions = actions.child(theme::tip(
                theme::secondary_button(
                    "all-replicas",
                    Some(icon::STACK),
                    "All replicas",
                    Some(act(Action::OpenGroupLogs {
                        namespace: p.namespace.clone(),
                        target: LogTarget::Owner(p.owner.clone()),
                    })),
                ),
                format!("Logs from every pod of {}", p.owner),
            ));
        }
        let running = p.status == "Running";
        let regular = p.containers.iter().filter(|c| !c.init).count();
        if regular > 1 && running {
            actions = actions.child(crate::app::view::trigger(
                PopupKind::ShellContainers,
                theme::secondary_button(
                    "shell-menu",
                    Some(icon::TERMINAL_WINDOW),
                    "Shell",
                    Some(Message::OpenPopup(PopupKind::ShellContainers)),
                ),
            ));
        } else {
            actions = actions.child(theme::tip(
                theme::secondary_button(
                    "shell",
                    Some(icon::TERMINAL_WINDOW),
                    "Shell",
                    running.then(|| act(Action::Shell(p.clone(), None))),
                ),
                if running {
                    "Open a shell in the container"
                } else {
                    "The pod isn't running"
                },
            ));
        }
        actions = actions
            .child(theme::secondary_button(
                "port-forward",
                Some(icon::ARROWS_LEFT_RIGHT),
                "Port forward",
                Some(act(Action::ForwardPod(p.clone(), None))),
            ))
            .child(self.drawer_menu_button(ctx));

        let mut col = col()
            .child(summary)
            .child(theme::vspace(8.0))
            .child(actions)
            .child(section("Details"));
        let mut rows = vec![
            ("Node", p.node.clone()),
            ("Pod IP", p.ip.clone()),
            ("Controlled by", p.owner.clone()),
        ];
        if let Some(d) = details {
            rows.extend([
                ("Host IP", d.host_ip.clone()),
                ("QoS class", d.qos.clone()),
                ("Service account", d.service_account.clone()),
                ("Restart policy", d.restart_policy.clone()),
                ("Priority class", d.priority_class.clone()),
                ("Started", d.started.clone()),
            ]);
        }
        col = col.child(details_grid(rows)).child(section("Containers"));
        for c in &p.containers {
            let color = if c.ready || c.state == "Completed" {
                GREEN
            } else {
                status_color(&c.state)
            };
            let line = h_flex()
                .gap(px(6.0))
                .py(px(6.0))
                .child(theme::dot(color))
                .child(theme::semibold(c.name.clone()))
                .when(c.init, |l| l.child(theme::faint("init")))
                .child(theme::txt(c.state.clone()))
                .child(theme::faint(format!("· {} restarts", c.restarts)));
            col = col.child(line);
        }

        if let Some(d) = details {
            col = col.child(conditions(&d.conditions));
        }

        if !p.ports.is_empty() {
            col = col.child(section("Ports"));
            for port in &p.ports {
                col = col.child(
                    h_flex()
                        .gap(px(8.0))
                        .child(theme::mono(port.to_string()))
                        .child(theme::small_button(
                            eid(format!("fwd/{port}")),
                            Some(icon::ARROWS_LEFT_RIGHT),
                            "Forward",
                            Some(act(Action::ForwardPod(p.clone(), Some(*port)))),
                        )),
                );
            }
        }

        if !p.labels.is_empty() {
            col = col
                .child(section("Labels"))
                .child(tags(p.labels.iter().map(|(k, v)| format!("{k}={v}"))));
        }
        if let Some(d) = details {
            if !d.node_selector.is_empty() {
                col = col
                    .child(section("Node selector"))
                    .child(tags(d.node_selector.iter().cloned()));
            }
            col = col.child(annotations(&d.annotations, ctx));
        }
        col.into_any_element()
    }

    fn drawer_pod_containers(&self, sel: &Selected, ctx: &ViewCtx<'_>) -> El {
        let details = match self.details_or_status() {
            Ok(d) => d,
            Err(e) => return e,
        };
        let Body::Pod(pod) = &details.body else {
            return div().into_any_element();
        };
        let row_info = self
            .pods
            .items
            .iter()
            .find(|p| p.namespace == sel.namespace && p.name == sel.name);

        let mut out = col().gap(px(10.0));
        for c in &pod.containers {
            let info = row_info.and_then(|p| p.containers.iter().find(|i| i.name == c.name));
            let color = match info {
                Some(i) if i.ready || i.state == "Completed" => GREEN,
                Some(i) => status_color(&i.state),
                None => theme::IDLE,
            };
            let mut head = h_flex()
                .w_full()
                .gap(px(8.0))
                .child(theme::dot(color))
                .child(theme::semibold(c.name.clone()).text_size(px(15.0)));
            if c.init {
                head = head.child(theme::badge("init", theme::IDLE, false));
            }
            if let Some(i) = info {
                head = head.child(theme::badge(i.state.clone(), color, false));
                if i.restarts > 0 {
                    head = head.child(theme::tip(
                        restarts_badge(i.restarts)
                            .into_any_element()
                            .into_id_div(eid(format!("rb/{}", c.name))),
                        format!("{} restarts", i.restarts),
                    ));
                }
            }
            head = head.child(theme::fill_space());
            if let Some(p) = row_info {
                if info.is_some_and(|i| i.restarts > 0) {
                    head = head.child(theme::tip(
                        theme::small_button(
                            eid(format!("prev/{}", c.name)),
                            Some(icon::CLOCK_COUNTER_CLOCKWISE),
                            "Previous",
                            Some(act(Action::OpenLogs(p.clone(), Some(c.name.clone()), true))),
                        ),
                        "Logs from the run before the last restart",
                    ));
                }
                head = head.child(theme::small_button(
                    eid(format!("logs/{}", c.name)),
                    Some(icon::SCROLL),
                    "Logs",
                    Some(act(Action::OpenLogs(
                        p.clone(),
                        Some(c.name.clone()),
                        false,
                    ))),
                ));
                let can_shell = !c.init && p.status == "Running";
                head = head.child(theme::small_button(
                    eid(format!("shell/{}", c.name)),
                    Some(icon::TERMINAL_WINDOW),
                    "Shell",
                    can_shell.then(|| act(Action::Shell(p.clone(), Some(c.name.clone())))),
                ));
            }
            let mut card = col().gap(px(10.0)).child(head);
            if !c.last_termination.is_empty() {
                card = card.child(
                    h_flex()
                        .gap(px(6.0))
                        .child(theme::icon(icon::WARNING_CIRCLE).text_color(YELLOW))
                        .child(
                            theme::colored(
                                format!("Last terminated: {}", c.last_termination),
                                YELLOW,
                            )
                            .text_size(px(theme::SMALL)),
                        ),
                );
            }

            // Image, pull policy, working dir and ports in one aligned key/value grid.
            let mut props = col()
                .gap(px(14.0))
                .child(prop_row("Image", image_label(&c.image)))
                .child(prop_row("Pull policy", theme::mono(c.pull_policy.clone()).into_any_element()));
            if !c.working_dir.is_empty() {
                props = props.child(prop_row(
                    "Working dir",
                    theme::mono(c.working_dir.clone())
                        .whitespace_normal()
                        .into_any_element(),
                ));
            }
            if !c.ports.is_empty() {
                props = props.child(prop_row("Ports", tags(c.ports.iter().cloned())));
            }
            card = card.child(theme::hline()).child(props);

            if !c.command.is_empty() || !c.args.is_empty() {
                let line = c
                    .command
                    .iter()
                    .chain(c.args.iter())
                    .map(|s| shell_quote(s))
                    .collect::<Vec<_>>()
                    .join(" ");
                card = card.child(sub_section("Command")).child(code_block(line));
            }
            if !c.requests.is_empty() || !c.limits.is_empty() {
                card = card
                    .child(sub_section("Resources"))
                    .child(resources_grid(&c.requests, &c.limits));
            }

            let env_count = c.env.len() + c.env_from_secrets.len();
            card = card.child(theme::hline()).child(collapsible(
                ctx,
                format!("env/{}", c.name),
                true,
                format!("Environment ({env_count})"),
                || self.env_list(&sel.namespace, c, ctx),
            ));
            if !c.mounts.is_empty() {
                card = card.child(collapsible(
                    ctx,
                    format!("mounts/{}", c.name),
                    false,
                    format!("Mounts ({})", c.mounts.len()),
                    || {
                        col()
                            .gap(px(3.0))
                            .children(c.mounts.iter().map(|(path, source)| {
                                h_flex()
                                    .flex_wrap()
                                    .gap(px(6.0))
                                    .child(theme::mono(path.clone()))
                                    .child(theme::faint(format!("← {source}")))
                            }))
                            .into_any_element()
                    },
                ));
            }
            if !c.probes.is_empty() {
                card = card.child(collapsible(
                    ctx,
                    format!("probes/{}", c.name),
                    false,
                    format!("Probes ({})", c.probes.len()),
                    || {
                        col()
                            .gap(px(6.0))
                            .children(c.probes.iter().map(|(kind, desc)| {
                                col()
                                    .gap(px(2.0))
                                    .child(theme::semibold(kind.clone()))
                                    .child(
                                        theme::mono_muted(desc.clone())
                                            .text_size(px(11.5))
                                            .whitespace_normal(),
                                    )
                            }))
                            .into_any_element()
                    },
                ));
            }
            out = out.child(
                theme::boxed(
                    theme::alpha(theme::FIELD, 0.55),
                    theme::BORDER,
                    theme::RADIUS,
                )
                .w_full()
                .px(px(14.0))
                .py(px(12.0))
                .child(card),
            );
        }
        out.into_any_element()
    }

    fn env_list(&self, ns: &str, c: &ContainerDetails, ctx: &ViewCtx<'_>) -> El {
        if c.env.is_empty() && c.env_from_secrets.is_empty() {
            return theme::faint("No environment variables").into_any_element();
        }
        let secret = |name: &str| self.secrets.get(&(ns.to_string(), name.to_string()));
        // One search shared by every container's list.
        let mut out = col().gap(px(4.0)).when_some(self.ui.env.get(&c.name), |d, input| {
            d.child(
                div().w(px(220.0)).child(
                    gpui_kit::component::input::Input::new(input)
                        .small()
                        .prefix(theme::icon(icon::MAGNIFYING_GLASS).text_size(px(13.0))),
                ),
            )
        });
        let needle = ctx.env_filter.trim().to_lowercase();
        let mut shown = 0usize;
        // The variables as a striped two-column table.
        let mut table = col();
        // Everything with a known value, for "Copy as .env".
        let mut dotenv = Vec::new();

        for name in &c.env_from_secrets {
            table = table.child(
                h_flex()
                    .w_full()
                    .gap(px(6.0))
                    .px(px(8.0))
                    .py(px(5.0))
                    .bg(theme::RAISED)
                    .child(theme::small(format!("All keys from Secret {name}")))
                    .child(theme::fill_space())
                    .child(self.secret_toggle(ns, name)),
            );
            if let Some(Ok(values)) = secret(name) {
                let source = format!("Secret {name}");
                for (k, v) in values {
                    if !env_matches(&needle, k, Some(v), Some(&source)) {
                        continue;
                    }
                    shown += 1;
                    table = table.child(env_item(
                        &format!("{}/{name}/{k}", c.name),
                        k,
                        Some(v.clone()),
                        Some(source.clone()),
                        None,
                        shown,
                    ));
                    dotenv.push(format!("{k}={v}"));
                }
            }
        }

        for e in &c.env {
            let (value, source, secret_name) = match &e.source {
                EnvSource::Literal => (e.value.clone(), None, None),
                EnvSource::ConfigMap { name, key } => (
                    Some(
                        e.value
                            .clone()
                            .unwrap_or_else(|| "(ConfigMap not readable)".into()),
                    ),
                    Some(format!("ConfigMap {name} · {key}")),
                    None,
                ),
                EnvSource::Secret { name, key } => {
                    let value = match secret(name) {
                        Some(Ok(values)) => Some(
                            values
                                .get(key)
                                .cloned()
                                .unwrap_or_else(|| "(missing key)".into()),
                        ),
                        _ => None,
                    };
                    (
                        value,
                        Some(format!("Secret {name} · {key}")),
                        Some(name.as_str()),
                    )
                }
                EnvSource::Field(path) => (None, Some(format!("field {path}")), None),
                EnvSource::Resource(r) => (None, Some(format!("resource {r}")), None),
            };
            if !env_matches(&needle, &e.name, value.as_deref(), source.as_deref()) {
                continue;
            }
            shown += 1;
            if let Some(v) = &value {
                dotenv.push(format!("{}={v}", e.name));
            }
            let toggle = secret_name.map(|name| self.secret_toggle(ns, name));
            table = table.child(env_item(
                &format!("{}/{}", c.name, e.name),
                &e.name,
                value,
                source,
                toggle.map(|t| (t, secret_name.is_some())),
                shown,
            ));
        }
        out = out.child(
            theme::boxed(theme::CONTENT, theme::BORDER, theme::RADIUS_SMALL)
                .w_full()
                .overflow_hidden()
                .child(table),
        );

        if !needle.is_empty() && shown == 0 {
            let hidden = c
                .env_from_secrets
                .iter()
                .any(|n| !matches!(secret(n), Some(Ok(_))));
            out = out.child(theme::faint(if hidden {
                "No variables match (reveal secrets to search their keys)"
            } else {
                "No variables match"
            }));
        }
        let copy_label = if needle.is_empty() {
            "Copy as .env"
        } else {
            "Copy matches as .env"
        };
        out.child(theme::vspace(2.0))
            .child(
                h_flex().child(theme::small_button(
                    eid(format!("copy-env/{}", c.name)),
                    Some(icon::COPY),
                    copy_label,
                    Some(Message::Copy(dotenv.join("\n"))),
                )),
            )
            .into_any_element()
    }

    fn secret_toggle(&self, ns: &str, name: &str) -> El {
        let key = (ns.to_string(), name.to_string());
        let id = eid(format!("secret/{ns}/{name}"));
        if self.secrets_loading.contains(&key) {
            return theme::faint("Reading…").into_any_element();
        }
        match self.secrets.get(&key) {
            Some(Ok(_)) => theme::small_button(
                id,
                Some(icon::EYE_SLASH),
                "Hide",
                Some(act(Action::HideSecret(key.0, key.1))),
            )
            .into_any_element(),
            Some(Err(e)) => theme::tip(
                h_flex()
                    .id(id)
                    .gap(px(4.0))
                    .child(theme::icon(icon::WARNING_CIRCLE).text_color(RED))
                    .child(theme::colored("not readable", RED).text_size(px(theme::SMALL))),
                e.clone(),
            )
            .into_any_element(),
            None => theme::tip(
                theme::small_button(
                    id,
                    Some(icon::EYE),
                    "Reveal",
                    Some(act(Action::RevealSecret(key.0, key.1))),
                ),
                "Read this secret",
            )
            .into_any_element(),
        }
    }

    fn drawer_pod_volumes(&self) -> El {
        let details = match self.details_or_status() {
            Ok(d) => d,
            Err(e) => return e,
        };
        let Body::Pod(pod) = &details.body else {
            return div().into_any_element();
        };
        if pod.volumes.is_empty() {
            return theme::faint("No volumes").into_any_element();
        }
        let mut out = col().gap(px(8.0));
        for (name, desc) in &pod.volumes {
            let mounted_by: Vec<String> = pod
                .containers
                .iter()
                .flat_map(|c| {
                    c.mounts
                        .iter()
                        .filter(|(_, src)| {
                            src == name
                                || src.starts_with(&format!("{name}/"))
                                || src.starts_with(&format!("{name} "))
                        })
                        .map(move |(path, _)| format!("{}:{path}", c.name))
                })
                .collect();
            let mut entry = col().gap(px(2.0)).child(
                h_flex()
                    .flex_wrap()
                    .gap(px(8.0))
                    .child(theme::semibold(name.clone()))
                    .child(theme::faint(desc.clone())),
            );
            for m in mounted_by {
                entry = entry.child(theme::mono_muted(m).text_size(px(11.5)));
            }
            out = out.child(entry);
        }
        out.into_any_element()
    }

    fn drawer_events(&self) -> El {
        let details = match self.details_or_status() {
            Ok(d) => d,
            Err(e) => return e,
        };
        let events = match &details.events {
            Ok(e) => e,
            Err(e) => {
                return theme::faint(format!("Events unavailable: {e}"))
                    .whitespace_normal()
                    .into_any_element();
            }
        };
        if events.is_empty() {
            return theme::faint("No recent events. Kubernetes keeps events for about an hour.")
                .whitespace_normal()
                .into_any_element();
        }
        let mut out = col().gap(px(8.0));
        for e in events {
            let (glyph, color) = if e.warning {
                (icon::WARNING, YELLOW)
            } else {
                (icon::INFO, theme::TEXT_MUTED)
            };
            let count = if e.count > 1 {
                format!(" ×{}", e.count)
            } else {
                String::new()
            };
            out = out.child(
                col()
                    .gap(px(4.0))
                    .child(
                        h_flex()
                            .w_full()
                            .gap(px(6.0))
                            .child(theme::icon(glyph).text_color(color))
                            .child(theme::semibold(e.reason.clone()))
                            .child(theme::fill_space())
                            .child(theme::faint(format!("{} ago{count}", e.age))),
                    )
                    .child(theme::txt(e.message.clone()).whitespace_normal())
                    .child(theme::vspace(2.0))
                    .child(theme::hline()),
            );
        }
        out.into_any_element()
    }

    fn drawer_service(&self, sel: &Selected, ctx: &ViewCtx<'_>) -> El {
        let Some(s) = self
            .services
            .items
            .iter()
            .find(|s| s.namespace == sel.namespace && s.name == sel.name)
        else {
            return theme::faint("This service no longer exists.").into_any_element();
        };
        let details = match self.loaded_details().map(|d| &d.body) {
            Some(Body::Service(d)) => Some(d),
            _ => None,
        };
        let can_forward = !s.selector.is_empty();
        let actions = wrap_row()
            .child(theme::tip(
                theme::secondary_button(
                    "port-forward",
                    Some(icon::ARROWS_LEFT_RIGHT),
                    "Port forward",
                    (can_forward && !s.ports.is_empty())
                        .then(|| act(Action::ForwardService(s.clone(), None))),
                ),
                if can_forward && !s.ports.is_empty() {
                    ""
                } else {
                    "Service has no selector or ports"
                },
            ))
            .child(theme::tip(
                theme::secondary_button(
                    "logs-all",
                    Some(icon::STACK),
                    "Logs (all pods)",
                    can_forward.then(|| {
                        act(Action::OpenGroupLogs {
                            namespace: s.namespace.clone(),
                            target: LogTarget::Selector {
                                label: format!("svc/{}", s.name),
                                selector: s.selector.clone(),
                            },
                        })
                    }),
                ),
                if can_forward {
                    ""
                } else {
                    "Service has no selector"
                },
            ))
            .child(self.drawer_menu_button(ctx));

        let mut out = col().child(actions).child(section("Details"));
        let mut rows = vec![
            ("Type", s.kind.clone()),
            ("Cluster IP", s.cluster_ip.clone()),
            ("External", s.external.clone()),
            ("Age", s.age()),
        ];
        if let Some(d) = details {
            rows.extend([
                ("Session affinity", d.session_affinity.clone()),
                ("External traffic", d.external_traffic_policy.clone()),
                ("Internal traffic", d.internal_traffic_policy.clone()),
                ("IP families", d.ip_families.clone()),
            ]);
        }
        out = out.child(details_grid(rows));

        if let Some(d) = details {
            let ready = d.endpoints.iter().filter(|e| e.ready).count();
            out = out.child(theme::vspace(4.0)).child(
                h_flex()
                    .gap(px(6.0))
                    .child(theme::dot(if ready > 0 { GREEN } else { RED }))
                    .child(theme::txt(format!(
                        "{ready} of {} endpoints ready",
                        d.endpoints.len()
                    ))),
            );
        }

        out = out.child(section("Ports"));
        if s.ports.is_empty() {
            out = out.child(theme::faint("No ports"));
        }
        for port in &s.ports {
            let name = if port.name.is_empty() {
                String::new()
            } else {
                format!("{} ", port.name)
            };
            let line = h_flex()
                .gap(px(8.0))
                .child(theme::mono(format!(
                    "{name}{} → {}/{}",
                    port.port, port.target, port.protocol
                )))
                .when_some(port.node_port, |l, np| {
                    l.child(theme::faint(format!("node {np}")))
                })
                .child(theme::small_button(
                    eid(format!("fwd/{}/{}", port.port, port.protocol)),
                    Some(icon::ARROWS_LEFT_RIGHT),
                    "Forward",
                    can_forward.then(|| act(Action::ForwardService(s.clone(), Some(port.port)))),
                ));
            out = out.child(line);
        }

        out = out.child(section("Selector"));
        if s.selector.is_empty() {
            out = out.child(theme::faint("None"));
        } else {
            out = out.child(tags(s.selector.split(',').map(str::to_string)));
        }
        if let Some(d) = details {
            if !d.labels.is_empty() {
                out = out
                    .child(section("Labels"))
                    .child(tags(d.labels.iter().map(|(k, v)| format!("{k}={v}"))));
            }
            out = out.child(annotations(&d.annotations, ctx));
        }
        out.into_any_element()
    }

    fn drawer_service_endpoints(&self) -> El {
        let details = match self.details_or_status() {
            Ok(d) => d,
            Err(e) => return e,
        };
        let Body::Service(svc) = &details.body else {
            return div().into_any_element();
        };
        let mut out = col().gap(px(4.0));
        if !svc.endpoint_ports.is_empty() {
            out = out
                .child(sub_section("Ports"))
                .child(tags(svc.endpoint_ports.iter().cloned()))
                .child(theme::vspace(6.0));
        }
        if svc.endpoints.is_empty() {
            return out
                .child(
                    theme::faint("No endpoints. Check that the selector matches ready pods.")
                        .whitespace_normal(),
                )
                .into_any_element();
        }
        let ns = self
            .selected
            .as_ref()
            .map(|s| s.namespace.clone())
            .unwrap_or_default();
        for e in &svc.endpoints {
            let target: El = match e.target.strip_prefix("pod/") {
                Some(pod) => theme::tip(
                    div()
                        .id(eid(format!("ep/{}/{}", e.ip, e.target)))
                        .text_size(px(theme::BODY))
                        .text_color(theme::ACCENT_HOVER)
                        .cursor_pointer()
                        .hover(|s| s.text_color(theme::ACCENT))
                        .on_click(on(act(Action::GoToPod(ns.clone(), pod.to_string()))))
                        .child(e.target.clone()),
                    "Open pod",
                )
                .into_any_element(),
                None => theme::txt(e.target.clone()).into_any_element(),
            };
            out = out.child(
                h_flex()
                    .gap(px(8.0))
                    .child(theme::dot(if e.ready { GREEN } else { RED }))
                    .child(theme::mono(e.ip.clone()))
                    .child(target)
                    .when(!e.ready, |l| l.child(theme::colored("not ready", RED))),
            );
            if !e.node.is_empty() {
                out = out.child(div().pl(px(16.0)).child(theme::small(format!("on {}", e.node))));
            }
        }
        out.into_any_element()
    }

    fn replicas_control(kind: Kind, ns: &str, name: &str, replicas: i32) -> El {
        let scale = |n: i32| act(Action::Scale(kind, ns.to_string(), name.to_string(), n));
        h_flex()
            .gap(px(6.0))
            .child(theme::txt("Replicas"))
            .child(theme::tip(
                icon_only("scale-down", icon::MINUS, (replicas > 0).then(|| scale(replicas - 1))),
                "Scale down",
            ))
            .child(theme::semibold(replicas.to_string()))
            .child(theme::tip(
                icon_only("scale-up", icon::PLUS, Some(scale(replicas + 1))),
                "Scale up",
            ))
            .into_any_element()
    }

    fn drawer_deployment(&self, sel: &Selected, ctx: &ViewCtx<'_>) -> El {
        let Some(d) = self
            .deployments
            .items
            .iter()
            .find(|d| d.namespace == sel.namespace && d.name == sel.name)
        else {
            return theme::faint("This deployment no longer exists.").into_any_element();
        };
        let details = match self.loaded_details().map(|d| &d.body) {
            Some(Body::Deployment(d)) => Some(d),
            _ => None,
        };
        let color = if d.ready >= d.replicas { GREEN } else { YELLOW };
        let summary = h_flex()
            .gap(px(8.0))
            .child(theme::dot(color))
            .child(theme::colored(format!("Ready {}/{}", d.ready, d.replicas), color))
            .child(theme::vline(14.0))
            .child(theme::txt(format!("Up-to-date {}", d.updated)))
            .child(theme::vline(14.0))
            .child(theme::txt(format!("Available {}", d.available)))
            .child(theme::vline(14.0))
            .child(theme::txt(format!("Age {}", d.age())));
        let mut out = col().child(summary).child(theme::vspace(8.0));
        if !d.selector.is_empty() {
            out = out
                .child(h_flex().child(theme::secondary_button(
                    "logs-all",
                    Some(icon::STACK),
                    "Logs (all pods)",
                    Some(act(Action::OpenGroupLogs {
                        namespace: d.namespace.clone(),
                        target: LogTarget::Selector {
                            label: format!("deploy/{}", d.name),
                            selector: d.selector.clone(),
                        },
                    })),
                )))
                .child(theme::vspace(4.0));
        }
        out = out.child(
            h_flex()
                .gap(px(8.0))
                .child(Self::replicas_control(
                    Kind::Deployment,
                    &d.namespace,
                    &d.name,
                    d.replicas,
                ))
                .child(theme::vline(18.0))
                .child(self.drawer_menu_button(ctx)),
        );

        out = out.child(section("Details"));
        let mut rows = vec![("Strategy", d.strategy.clone())];
        if let Some(det) = details {
            rows.extend([
                ("Paused", if det.paused { "Yes" } else { "No" }.to_string()),
                ("Min ready", det.min_ready_seconds.to_string()),
                ("Revision history", det.revision_history.to_string()),
            ]);
        }
        out = out.child(details_grid(rows));
        if let Some(det) = details {
            out = out.child(conditions(&det.conditions));
        }
        out = out.child(section("Images"));
        for image in &d.images {
            out = out.child(theme::mono(image.clone()).whitespace_normal());
        }
        if !d.selector.is_empty() {
            out = out
                .child(section("Selector"))
                .child(tags(d.selector.split(',').map(str::to_string)));
        }
        if let Some(det) = details {
            if !det.labels.is_empty() {
                out = out
                    .child(section("Labels"))
                    .child(tags(det.labels.iter().map(|(k, v)| format!("{k}={v}"))));
            }
            out = out.child(annotations(&det.annotations, ctx));
        }
        out.into_any_element()
    }

    fn drawer_workload(&self, sel: &Selected, ctx: &ViewCtx<'_>) -> El {
        let Some(w) = self
            .workload(sel.kind)
            .items
            .iter()
            .find(|w| w.namespace == sel.namespace && w.name == sel.name)
        else {
            return theme::faint(format!("This {} no longer exists.", sel.kind.label()))
                .into_any_element();
        };
        let details = match self.loaded_details().map(|d| &d.body) {
            Some(Body::Workload(d)) => Some(d),
            _ => None,
        };
        let color = health_color(w.health);
        let summary = h_flex()
            .gap(px(8.0))
            .child(theme::dot(color))
            .child(theme::colored(w.summary.clone(), color))
            .child(theme::vline(14.0))
            .child(theme::txt(format!("Age {}", w.age())));

        // Separators only between groups that are actually shown.
        let mut actions = wrap_row();
        let mut first = true;
        if w.kind == Kind::CronJob {
            first = false;
            actions = actions.child(theme::tip(
                theme::primary_button(
                    "run-now",
                    Some(icon::PLAY),
                    "Run now",
                    Some(act(Action::Trigger(w.namespace.clone(), w.name.clone()))),
                ),
                "Start a job from this cron job's template",
            ));
        }
        if let Some(suspended) = w.suspended.filter(|_| !w.finished) {
            first = false;
            let (glyph, label) = if suspended {
                (icon::PLAY_CIRCLE, "Resume")
            } else {
                (icon::PAUSE, "Suspend")
            };
            let hint = match (w.kind, suspended) {
                (Kind::CronJob, false) => "Stop scheduling new runs",
                (Kind::CronJob, true) => "Start scheduling runs again",
                (_, false) => "Stop the job's pods until resumed",
                (_, true) => "Let the job run again",
            };
            actions = actions.child(theme::tip(
                theme::secondary_button(
                    "suspend",
                    Some(glyph),
                    label,
                    Some(act(Action::Suspend(
                        w.kind,
                        w.namespace.clone(),
                        w.name.clone(),
                        !suspended,
                    ))),
                ),
                hint,
            ));
        }
        if !w.selector.is_empty() && w.kind != Kind::CronJob {
            if !first {
                actions = actions.child(theme::vline(18.0));
            }
            first = false;
        }
        if !w.selector.is_empty() {
            actions = actions.child(theme::secondary_button(
                "logs-all",
                Some(icon::STACK),
                "Logs (all pods)",
                Some(act(Action::OpenGroupLogs {
                    namespace: w.namespace.clone(),
                    target: LogTarget::Selector {
                        label: format!("{}/{}", w.kind.label().to_lowercase(), w.name),
                        selector: w.selector.clone(),
                    },
                })),
            ));
        }
        if let Some(replicas) = w.replicas.filter(|_| w.kind.scalable()) {
            if !first {
                actions = actions.child(theme::vline(18.0));
            }
            first = false;
            actions = actions.child(Self::replicas_control(
                w.kind,
                &w.namespace,
                &w.name,
                replicas,
            ));
        }
        if !first {
            actions = actions.child(theme::vline(18.0));
        }
        actions = actions.child(self.drawer_menu_button(ctx));

        let mut out = col()
            .child(summary)
            .child(theme::vspace(8.0))
            .child(actions)
            .child(section("Details"));
        let columns = kube_ops::columns(w.kind);
        let mut rows: Vec<(&str, String)> = columns
            .iter()
            .map(|(t, _)| *t)
            .zip(w.cells.iter().map(|c| c.text()))
            .collect();
        rows.extend(w.info.iter().map(|(k, v)| (*k, v.clone())));
        if !w.owner.is_empty() {
            rows.push(("Controlled by", w.owner.clone()));
        }
        out = out.child(details_grid(rows));
        if let Some(det) = details {
            out = out.child(conditions(&det.conditions));
        }
        if !w.images.is_empty() {
            out = out.child(section("Images"));
            for image in &w.images {
                out = out.child(theme::mono(image.clone()).whitespace_normal());
            }
        }
        if !w.selector.is_empty() {
            out = out
                .child(section("Selector"))
                .child(tags(w.selector.split(',').map(str::to_string)));
        }
        if let Some(det) = details {
            if !det.labels.is_empty() {
                out = out
                    .child(section("Labels"))
                    .child(tags(det.labels.iter().map(|(k, v)| format!("{k}={v}"))));
            }
            out = out.child(annotations(&det.annotations, ctx));
        }
        out.into_any_element()
    }
}

// ---------------------------------------------------------------------------
// Drawer pieces
// ---------------------------------------------------------------------------

/// A full-width column whose children stretch.
fn col() -> Div {
    v_flex().w_full().items_stretch().gap(px(2.0))
}

/// A row of controls that wraps onto new lines.
fn wrap_row() -> Div {
    h_flex().w_full().flex_wrap().gap(px(6.0))
}

/// A secondary button holding only a glyph.
fn icon_only(
    id: &'static str,
    glyph: &'static str,
    on_press: Option<Message>,
) -> gpui_kit::Stateful<Div> {
    theme::secondary_button(id, Some(glyph), "", on_press)
        .px(px(9.0))
        .pr(px(3.0))
}

/// Turns an element into an id'd div so it can carry a tooltip.
trait IntoIdDiv {
    fn into_id_div(self, id: ElementId) -> gpui_kit::Stateful<Div>;
}

impl IntoIdDiv for AnyElement {
    fn into_id_div(self, id: ElementId) -> gpui_kit::Stateful<Div> {
        div().id(id).child(self)
    }
}

fn section(title: &str) -> El {
    v_flex()
        .child(theme::vspace(28.0))
        .child(theme::label_small(title.to_string()))
        .child(theme::vspace(10.0))
        .into_any_element()
}

fn sub_section(title: &str) -> El {
    v_flex()
        .child(theme::vspace(14.0))
        .child(theme::label_small(title.to_string()))
        .child(theme::vspace(4.0))
        .into_any_element()
}

fn details_grid(rows: Vec<(&str, String)>) -> El {
    col()
        .gap(px(14.0))
        .children(rows.into_iter().map(|(key, value)| {
            let value = if value.is_empty() {
                "-".to_string()
            } else {
                value
            };
            let long = value.chars().count() > 48;
            let text = theme::mono(value.clone())
                .id(eid(format!("val/{key}")))
                .whitespace_normal()
                .flex_1()
                .min_w_0();
            h_flex()
                .w_full()
                .gap(px(20.0))
                .items_start()
                .child(theme::muted(key.to_string()).w(px(136.0)).flex_none())
                .child(theme::tip(text, if long { value } else { String::new() }))
        }))
        .into_any_element()
}

/// Pill-shaped labels that wrap onto new rows. Click copies one.
fn tags(items: impl Iterator<Item = String>) -> El {
    h_flex()
        .w_full()
        .flex_wrap()
        .gap(px(8.0))
        .children(items.map(|item| {
            theme::tip(
                div()
                    .id(eid(format!("tag/{item}")))
                    .px(px(10.0))
                    .py(px(4.0))
                    .bg(theme::STRIPE)
                    .border_1()
                    .border_color(theme::BORDER)
                    .rounded(px(4.0))
                    .cursor_pointer()
                    .hover(|s| s.bg(theme::FIELD_HOVER))
                    .text_color(theme::TEXT)
                    .font_family(theme::MONO)
                    .text_size(px(11.5))
                    .child(item.clone())
                    .on_click(on(Message::Copy(item))),
                "Click to copy",
            )
        }))
        .into_any_element()
}

/// A header that folds its content away. `key` remembers the state for this drawer.
fn collapsible(
    ctx: &ViewCtx<'_>,
    key: String,
    default_open: bool,
    title: String,
    content: impl FnOnce() -> El,
) -> El {
    let open = ctx.open(&key, default_open);
    let header = h_flex()
        .id(eid(format!("section/{key}")))
        .gap(px(6.0))
        .py(px(4.0))
        .cursor_pointer()
        .child(
            theme::icon(if open {
                icon::CARET_DOWN
            } else {
                icon::CARET_RIGHT
            })
            .text_size(px(11.0))
            .text_color(theme::TEXT_FAINT),
        )
        .child(theme::semibold(title))
        .on_click(on(Message::ToggleSection(key)));
    col()
        .gap(px(4.0))
        .child(header)
        .when(open, |c| c.child(div().pl(px(17.0)).child(content())))
        .into_any_element()
}

fn annotations(list: &[(String, String)], ctx: &ViewCtx<'_>) -> El {
    if list.is_empty() {
        return div().into_any_element();
    }
    col()
        .child(theme::vspace(10.0))
        .child(collapsible(
            ctx,
            "annotations".into(),
            false,
            format!("Annotations ({})", list.len()),
            || {
                col()
                    .gap(px(6.0))
                    .children(list.iter().map(|(k, v)| {
                        col()
                            .gap(px(2.0))
                            .child(theme::small(k.clone()).whitespace_normal())
                            .child(value_label(v, theme::TEXT))
                    }))
                    .into_any_element()
            },
        ))
        .into_any_element()
}

fn conditions(list: &[details::Condition]) -> El {
    if list.is_empty() {
        return div().into_any_element();
    }
    let mut out = col().gap(px(9.0)).child(section("Conditions"));
    for c in list {
        out = out.child(
            h_flex()
                .gap(px(6.0))
                .child(theme::dot(if c.ok { GREEN } else { YELLOW }))
                .child(theme::tip(
                    theme::txt(c.type_.clone()).id(eid(format!("cond/{}", c.type_))),
                    c.message.clone(),
                ))
                .when(!c.reason.is_empty(), |l| {
                    l.child(theme::faint(c.reason.clone()))
                }),
        );
    }
    out.into_any_element()
}

/// A value in monospace, wrapping, with long values cut short (full value on hover).
fn value_label(value: &str, color: Color) -> El {
    const MAX: usize = 400;
    if value.is_empty() {
        theme::faint("(empty)").into_any_element()
    } else if value.chars().count() > MAX {
        let short: String = value.chars().take(MAX).collect();
        theme::tip(
            theme::mono(format!("{short}…"))
                .id(eid(format!("long/{}", &value[..value.len().min(24)])))
                .text_color(color)
                .whitespace_normal(),
            value.to_string(),
        )
        .into_any_element()
    } else {
        theme::mono(value.to_string())
            .text_color(color)
            .whitespace_normal()
            .into_any_element()
    }
}

/// One environment variable as a table row: name and its source on the left, the value on
/// the right. `index` alternates the row stripe.
fn env_item(
    _key: &str,
    name: &str,
    value: Option<String>,
    source: Option<String>,
    secret: Option<(El, bool)>,
    index: usize,
) -> El {
    let is_secret = secret.as_ref().is_some_and(|(_, s)| *s);
    let key = col()
        .gap(px(2.0))
        .w(relative(0.4))
        .child(
            theme::mono(name.to_string())
                .font_weight(gpui_kit::FontWeight::BOLD)
                .whitespace_normal(),
        )
        .when_some(source, |c, s| {
            c.child(theme::small(s).text_color(theme::TEXT_FAINT).whitespace_normal())
        })
        .when_some(secret, |c, (toggle, _)| c.child(h_flex().child(toggle)));
    let value: El = match value {
        Some(v) => value_label(&v, theme::SYNTAX_STRING),
        None if is_secret => theme::faint("••••••••").into_any_element(),
        None => theme::faint("set at runtime").into_any_element(),
    };
    let stripe = if index % 2 == 0 {
        theme::STRIPE
    } else {
        theme::CONTENT
    };
    h_flex()
        .w_full()
        .gap(px(12.0))
        .items_start()
        .px(px(8.0))
        .py(px(5.0))
        .bg(stripe)
        .child(key)
        .child(div().flex_1().min_w_0().child(value))
        .into_any_element()
}

/// A label column and its value, for the container card's property grid.
fn prop_row(label: &str, value: El) -> El {
    h_flex()
        .w_full()
        .gap(px(12.0))
        .items_start()
        .child(theme::muted(label.to_string()).w(px(96.0)).flex_none())
        .child(div().flex_1().min_w_0().child(value))
        .into_any_element()
}

/// An image reference with its registry/repository path muted and `name:tag` emphasised.
/// Click copies the whole reference.
fn image_label(image: &str) -> El {
    let (path, name) = match image.rfind('/') {
        Some(i) => (&image[..=i], &image[i + 1..]),
        None => ("", image),
    };
    let el = col()
        .id(eid(format!("image/{image}")))
        .gap(px(1.0))
        .cursor_pointer()
        .when(!path.is_empty(), |c| {
            c.child(
                theme::mono_muted(path.to_string())
                    .text_size(px(theme::SMALL))
                    .whitespace_normal(),
            )
        })
        .child(theme::mono(name.to_string()).whitespace_normal())
        .on_click(on(Message::Copy(image.to_string())));
    theme::tip(el, "Click to copy").into_any_element()
}

fn resources_grid(requests: &[(String, String)], limits: &[(String, String)]) -> El {
    let mut names: Vec<&String> = requests
        .iter()
        .chain(limits.iter())
        .map(|(k, _)| k)
        .collect();
    names.sort();
    names.dedup();
    let find = |list: &[(String, String)], name: &str| {
        list.iter()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.clone())
            .unwrap_or_else(|| "-".into())
    };
    let cell = |e: El| div().flex_1().min_w_0().child(e);
    let mut out = col()
        .gap(px(0.0))
        .child(
            h_flex()
                .w_full()
                .gap(px(8.0))
                .px(px(8.0))
                .py(px(5.0))
                .bg(theme::RAISED)
                .child(cell(div().into_any_element()))
                .child(cell(theme::label_small("Request").into_any_element()))
                .child(cell(theme::label_small("Limit").into_any_element())),
        );
    for (i, name) in names.into_iter().enumerate() {
        let stripe = if i % 2 == 0 {
            theme::CONTENT
        } else {
            theme::STRIPE
        };
        out = out.child(
            h_flex()
                .w_full()
                .gap(px(8.0))
                .px(px(8.0))
                .py(px(5.0))
                .bg(stripe)
                .child(cell(theme::txt(name.clone()).into_any_element()))
                .child(cell(theme::mono(find(requests, name)).into_any_element()))
                .child(cell(theme::mono(find(limits, name)).into_any_element())),
        );
    }
    theme::boxed(theme::CONTENT, theme::BORDER, theme::RADIUS_SMALL)
        .w_full()
        .overflow_hidden()
        .child(out)
        .into_any_element()
}

fn code_block(line: String) -> El {
    theme::boxed(theme::CHROME, theme::CHROME, 4.0)
        .w_full()
        .p(px(6.0))
        .child(
            theme::mono(line)
                .text_size(px(11.5))
                .whitespace_normal(),
        )
        .into_any_element()
}
