//! The YAML window: view, edit and apply one object's manifest, or create a new one.
//!
//! The document lives in a GPUI Kit code editor (`EditorState`, tree-sitter YAML). Kit
//! supplies line numbers, indent guides, folding, Tab indent, smart indent and its own
//! search panel (⌘F); this module adds the window, ⌘S apply, ⌘/ comments and the live YAML
//! check. `YamlView.content` mirrors the editor's text (read on every `Change` event).

use std::sync::Once;

use gpui_kit::component::input::{
    Editor, EditorState, GutterDecoration, GutterDecorationCollection, InputEvent,
};
use gpui_kit::component::{h_flex, v_flex};
use gpui_kit::{AnyElement, IntoElement as _, ParentElement as _, SharedString, Styled as _, Subscription, div, px};

use super::*;

pub(super) struct YamlView {
    pub title: String,
    /// The cluster tab and object it shows, so edits are saved to the right place.
    pub ws: u64,
    pub kind: Kind,
    pub namespace: String,
    pub name: String,
    pub text: Option<Result<String, String>>,
    /// Mirror of the editor's text: the YAML being viewed, or the edit buffer while editing.
    pub content: String,
    /// The Kit code editor holding the document (created in `prepare_yaml`).
    editor: Option<Entity<EditorState>>,
    _subscription: Option<Subscription>,
    /// Gutter marks (the YAML error line).
    marks: Option<GutterDecorationCollection>,
    /// `content` has been pushed into the editor.
    loaded: bool,
    /// Focus the editor on the next frame (after Edit, or when the window opens).
    focus_pending: bool,
    /// Editing (vs. viewing).
    pub editing: bool,
    /// The buffer differs from the fetched YAML (always true when creating).
    pub dirty: bool,
    /// Open straight into editing once the YAML arrives.
    pub edit_on_load: bool,
    pub saving: bool,
    pub save_error: Option<String>,
    /// Live YAML check of the edit buffer: (1-based line, message).
    pub parse_error: Option<(usize, String)>,
    /// A new object (Apply creates it) rather than an existing one.
    pub create: bool,
}

impl YamlView {
    #[allow(clippy::too_many_arguments)]
    fn new(
        title: String,
        ws: u64,
        kind: Kind,
        namespace: String,
        name: String,
        text: Option<Result<String, String>>,
        create: bool,
    ) -> Self {
        Self {
            title,
            ws,
            kind,
            namespace,
            name,
            content: match &text {
                Some(Ok(t)) => t.clone(),
                _ => String::new(),
            },
            text,
            editor: None,
            _subscription: None,
            marks: None,
            loaded: false,
            focus_pending: true,
            editing: create,
            dirty: create,
            edit_on_load: false,
            saving: false,
            save_error: None,
            parse_error: None,
            create,
        }
    }
}

impl App {
    /// Creates the YAML window's retained state (the Kit code editor) as needed, pushes
    /// freshly fetched text into it and moves focus when asked.
    pub(super) fn prepare_yaml(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        static SYNTAX: Once = Once::new();
        SYNTAX.call_once(|| code_editor::install_syntax(cx));
        let Some(y) = self.yaml.as_mut() else { return };
        if !matches!(y.text, Some(Ok(_))) {
            return;
        }
        if y.editor.is_none() {
            let state = cx.new(|cx| EditorState::new(window, cx).language("yaml"));
            y.marks = Some(state.update(cx, |s, cx| s.create_gutter_decorations_collection(Vec::new(), cx)));
            y._subscription = Some(cx.subscribe_in(
                &state,
                window,
                |app: &mut App, state, event: &InputEvent, _window, cx| {
                    if matches!(event, InputEvent::Change) {
                        let text = state.read(cx).value().to_string();
                        app.yaml_text_changed(text, cx);
                    }
                },
            ));
            y.editor = Some(state);
        }
        let Some(editor) = y.editor.clone() else { return };
        if !y.loaded {
            y.loaded = true;
            let text = SharedString::from(y.content.clone());
            editor.update(cx, |s, cx| s.set_value(text, window, cx));
            if let Some(m) = &y.marks {
                m.clear(cx);
            }
        }
        if y.focus_pending {
            y.focus_pending = false;
            editor.update(cx, |s, cx| s.focus(window, cx));
        }
    }

    /// The editor's text changed: refresh the mirror, dirty flag, YAML check and error mark.
    fn yaml_text_changed(&mut self, text: String, cx: &mut Context<Self>) {
        let Some(y) = self.yaml.as_mut().filter(|y| y.editing) else {
            return;
        };
        y.content = text;
        Self::yaml_changed(y);
        if let Some(marks) = &y.marks {
            let decorations = y
                .parse_error
                .as_ref()
                .map(|(line, _)| {
                    let range = code_editor::line_byte_range(&y.content, *line);
                    vec![GutterDecoration::new(range).with_color(theme::BAD.into())]
                })
                .unwrap_or_default();
            marks.set(decorations, cx);
        }
        cx.notify();
    }

    /// The YAML window's dialog panel (view.rs draws it over a backdrop).
    pub(super) fn view_yaml(&self, y: &YamlView) -> AnyElement {
        let title = if y.editing && y.dirty && !y.create {
            format!("{} (edited)", y.title)
        } else {
            y.title.clone()
        };
        let width = (self.window_size.width - 80.0).clamp(560.0, 1040.0);
        let height = (self.window_size.height - 80.0).clamp(360.0, 900.0);
        let mut col = v_flex()
            .size_full()
            .gap(px(6.0))
            .child(
                h_flex()
                    .items_center()
                    .child(theme::semibold(title).text_size(px(15.0)))
                    .child(theme::fill_space())
                    .child(theme::icon_button("yaml-close", icon::X, "Close", Some(Message::YamlClose))),
            );
        match &y.text {
            None => col = col.child(theme::faint("Loading…")),
            Some(Err(e)) => col = col.child(theme::colored(e.clone(), theme::BAD).whitespace_normal()),
            Some(Ok(original)) => {
                let mut bar = h_flex().items_center().gap(px(8.0));
                if y.editing {
                    let (glyph, label) = if y.create {
                        (icon::PLUS, "Create")
                    } else {
                        (icon::FLOPPY_DISK, "Apply")
                    };
                    let can_apply = y.dirty && !y.saving && y.parse_error.is_none();
                    bar = bar.child(theme::tip(
                        theme::primary_button(
                            "yaml-apply",
                            Some(glyph),
                            label,
                            can_apply.then_some(Message::YamlApply),
                        ),
                        "Replace the object with this YAML (⌘S)",
                    ));
                    bar = bar.child(theme::secondary_button(
                        "yaml-cancel",
                        None,
                        "Cancel",
                        (!y.saving).then_some(Message::YamlCancel),
                    ));
                    if y.saving {
                        bar = bar.child(theme::faint("Saving…"));
                    }
                } else {
                    bar = bar.child(theme::primary_button(
                        "yaml-edit",
                        Some(icon::PENCIL_SIMPLE),
                        "Edit",
                        Some(Message::YamlEdit),
                    ));
                    bar = bar.child(theme::secondary_button(
                        "yaml-copy",
                        Some(icon::COPY),
                        "Copy",
                        Some(Message::Copy(original.clone())),
                    ));
                }
                bar = bar.child(theme::fill_space());
                if y.create {
                    bar = bar.child(theme::faint(format!("Namespace defaults to {}", y.namespace)));
                } else {
                    bar = bar.child(theme::tip(
                        theme::secondary_button(
                            "yaml-reload",
                            Some(icon::ARROW_CLOCKWISE),
                            "Reload",
                            (!y.saving).then_some(Message::YamlReload),
                        ),
                        "Fetch the latest version (discards edits)",
                    ));
                }
                col = col.child(bar);
                if y.editing && original.contains("app.kubernetes.io/managed-by: Helm") {
                    col = col.child(
                        theme::colored(
                            "Managed by Helm: the next helm upgrade will overwrite changes made here.",
                            theme::WARN,
                        )
                        .whitespace_normal(),
                    );
                }
                if let Some(e) = &y.save_error {
                    col = col.child(theme::colored(e.clone(), theme::BAD).whitespace_normal());
                    if e.contains("modified") || e.contains("Conflict") {
                        col = col.child(
                            theme::faint(
                                "Someone changed it since you opened it. Copy your edits, Reload, and apply them again.",
                            )
                            .whitespace_normal(),
                        );
                    }
                }
                if let Some((line, msg)) = y.parse_error.as_ref().filter(|_| y.editing) {
                    col = col.child(
                        theme::colored(format!("Line {line}: {msg}"), theme::BAD).whitespace_normal(),
                    );
                }
                if let Some(editor) = &y.editor {
                    col = col.child(
                        theme::boxed(theme::CONTENT, theme::BORDER, 4.0)
                            .flex_1()
                            .min_h_0()
                            .w_full()
                            .overflow_hidden()
                            .child(
                                Editor::new(editor)
                                    .appearance(false)
                                    .readonly(!y.editing || y.saving)
                                    .size_full(),
                            ),
                    );
                }
            }
        }
        // The chrome's modal layer (view.rs) supplies the backdrop and centring.
        theme::dialog()
            .w(px(width))
            .h(px(height))
            .p(px(16.0))
            .child(col)
            .into_any_element()
    }

    /// Messages for the YAML window and its find bar.
    pub(super) fn yaml_message(&mut self, message: Message) -> Task {
        match message {
            Message::YamlEdit => {
                if let Some(y) = self.yaml.as_mut() {
                    y.editing = true;
                    y.focus_pending = true;
                }
            }
            Message::YamlApply => self.yaml_apply(),
            Message::YamlCancel => {
                if let Some(y) = self.yaml.as_mut() {
                    if y.create {
                        self.yaml = None;
                    } else {
                        y.editing = false;
                        y.dirty = false;
                        y.save_error = None;
                        y.parse_error = None;
                        if let Some(Ok(t)) = &y.text {
                            y.content = t.clone();
                            y.loaded = false;
                            y.focus_pending = true;
                        }
                    }
                }
            }
            Message::YamlReload => {
                if let Some(y) = &self.yaml {
                    let (kind, ns, name, editing) =
                        (y.kind, y.namespace.clone(), y.name.clone(), y.editing);
                    self.open_yaml(kind, ns, name);
                    if let Some(y) = self.yaml.as_mut() {
                        y.edit_on_load = editing;
                    }
                }
            }
            Message::YamlClose => self.yaml = None,
            Message::YamlComment => return Self::yaml_toggle_comment(),
            _ => {}
        }
        Task::none()
    }

    /// Results of fetching, applying or creating YAML (`Msg::Yaml*`).
    pub(super) fn yaml_result(&mut self, msg: Msg) -> Task {
        match msg {
            Msg::Yaml { title, result } => {
                if let Some(y) = self.yaml.as_mut().filter(|y| y.title == title) {
                    if let Ok(t) = &result {
                        y.content = t.clone();
                        y.loaded = false;
                        y.focus_pending = true;
                    }
                    if y.edit_on_load && result.is_ok() {
                        y.edit_on_load = false;
                        y.editing = true;
                    }
                    y.text = Some(result);
                }
            }
            Msg::YamlCreated { title, result } => {
                let mut created = None;
                if let Some(y) = self.yaml.as_mut().filter(|y| y.title == title) {
                    y.saving = false;
                    match result {
                        Ok(c) => created = Some((y.ws, c)),
                        Err(e) => y.save_error = Some(e),
                    }
                }
                if let Some((ws, (kind, namespace, name))) = created {
                    self.yaml = None;
                    self.toast(
                        format!("Created {} {name}", kind.label().to_lowercase()),
                        false,
                    );
                    if let Some(w) = self.workspaces.iter_mut().find(|w| w.id == ws) {
                        w.changed();
                    }
                    // Show it: its page, with the drawer open on it (it fills in as the watch sees it).
                    return self.apply(Action::OpenSelected(Selected {
                        kind,
                        namespace,
                        name,
                    }));
                }
            }
            Msg::YamlApplied { title, result } => {
                let mut saved = None;
                if let Some(y) = self.yaml.as_mut().filter(|y| y.title == title) {
                    y.saving = false;
                    match result {
                        Ok(()) => {
                            y.save_error = None;
                            saved = Some((y.ws, y.kind, y.namespace.clone(), y.name.clone()));
                        }
                        Err(e) => y.save_error = Some(e),
                    }
                }
                if let Some((ws, kind, ns, name)) = saved {
                    self.toast(format!("Saved {name}"), false);
                    if let Some(w) = self.workspaces.iter_mut().find(|w| w.id == ws) {
                        w.changed();
                    }
                    // Show what the server stored (defaults filled in, new resourceVersion).
                    self.open_yaml(kind, ns, name);
                }
            }
            _ => {}
        }
        Task::none()
    }

    /// The editor on a template for a new object, in the page's namespace.
    pub(super) fn open_create(&mut self, kind: Kind) -> Task {
        let Some(ws) = self.ws() else {
            return Task::none();
        };
        if ws.client().is_none() {
            return Task::none();
        }
        let ns = ws
            .selected_ns
            .first()
            .cloned()
            .unwrap_or_else(|| "default".into());
        let text = kube_ops::template(kind, &ns);
        self.yaml = Some(YamlView::new(
            format!("New {}", kind.label()),
            ws.id,
            kind,
            ns,
            String::new(),
            Some(Ok(text)),
            true,
        ));
        Task::none()
    }

    pub(super) fn open_yaml(&mut self, kind: Kind, ns: String, name: String) {
        let Some((ws, client)) = self.ws().and_then(|w| w.client().map(|c| (w.id, c))) else {
            return;
        };
        let title = format!("{} {ns}/{name}", kind.label());
        self.yaml = Some(YamlView::new(
            title.clone(),
            ws,
            kind,
            ns.clone(),
            name.clone(),
            None,
            false,
        ));
        let bus = self.bus.clone();
        self.rt.spawn(async move {
            let result = kube_ops::get_yaml(client, kind, ns, name)
                .await
                .map_err(err_string);
            bus.send(Msg::Yaml { title, result });
        });
    }

    /// After an edit: recompute dirty and the live YAML check.
    fn yaml_changed(y: &mut YamlView) {
        y.dirty = y.create || !matches!(&y.text, Some(Ok(t)) if *t == y.content);
        y.parse_error = yaml_error(&y.content);
    }

    /// ⌘/: comments out (or uncomments) the lines the selection touches, as one undo step.
    fn yaml_toggle_comment() -> Task {
        Task::window(|app, window, cx| {
            let Some(y) = app.yaml.as_ref().filter(|y| y.editing && !y.saving) else {
                return;
            };
            let Some(editor) = y.editor.clone() else { return };
            let text = editor.update(cx, |s, cx| {
                let text = s.value().to_string();
                let (block, new, selection) = code_editor::comment_edit(&text, s.selected_range());
                s.set_selected_range(block, cx);
                s.replace(new, window, cx);
                s.set_selected_range(selection, cx);
                s.value().to_string()
            });
            app.yaml_text_changed(text, cx);
        })
    }

    pub(super) fn yaml_apply(&mut self) {
        let Some(y) = self.yaml.as_mut() else { return };
        if !(y.editing && y.dirty && !y.saving && y.parse_error.is_none()) {
            return;
        }
        let yaml = y.content.clone();
        y.saving = true;
        y.save_error = None;
        let (title, ws, kind, ns, name) = (
            y.title.clone(),
            y.ws,
            y.kind,
            y.namespace.clone(),
            y.name.clone(),
        );
        let client = self
            .workspaces
            .iter()
            .find(|w| w.id == ws)
            .and_then(Workspace::client);
        let bus = self.bus.clone();
        match client {
            Some(client) if y.create => {
                self.rt.spawn(async move {
                    let result = kube_ops::create_yaml(client, ns, yaml)
                        .await
                        .map_err(err_string);
                    bus.send(Msg::YamlCreated { title, result });
                });
            }
            Some(client) => {
                self.rt.spawn(async move {
                    let result = kube_ops::apply_yaml(client, kind, ns, name, yaml)
                        .await
                        .map_err(err_string);
                    bus.send(Msg::YamlApplied { title, result });
                });
            }
            None => {
                y.saving = false;
                y.save_error = Some("That cluster tab is no longer connected.".into());
            }
        }
    }
}
