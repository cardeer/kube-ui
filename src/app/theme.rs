//! Visual system: colour tokens, fonts, and the app's own small controls, on GPUI.
//! A dark developer tool in the Lens manner: three neutral layers, hairline borders, one
//! blue accent for selection/focus/primary action, state colours for state only.
//!
//! Controls take a [`Message`] and dispatch it through [`on`] when clicked, so views stay
//! plain `&self -> AnyElement` functions (no `Context` threading). Every clickable control
//! needs an `id` that is unique among its siblings (derive it from the data, not an index).

use std::borrow::Cow;

use gpui_kit::component::{Theme as KitTheme, ThemeMode, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use super::icons as icon;
use super::{Message, on};

// ---------------------------------------------------------------------------
// Colour
// ---------------------------------------------------------------------------

/// An sRGB colour with straight alpha (0–1). Converts into GPUI's `Hsla` wherever a
/// colour is taken (`.bg(c)`, `.text_color(c)`, …).
#[derive(Clone, Copy, PartialEq, Debug, Default)]
pub struct Color {
    pub r: f32,
    pub g: f32,
    pub b: f32,
    pub a: f32,
}

impl Color {
    pub const WHITE: Color = Color::from_rgb(1.0, 1.0, 1.0);
    pub const BLACK: Color = Color::from_rgb(0.0, 0.0, 0.0);
    pub const TRANSPARENT: Color = Color::from_rgba(0.0, 0.0, 0.0, 0.0);

    pub const fn from_rgb(r: f32, g: f32, b: f32) -> Self {
        Self { r, g, b, a: 1.0 }
    }
    pub const fn from_rgba(r: f32, g: f32, b: f32, a: f32) -> Self {
        Self { r, g, b, a }
    }
    pub const fn from_rgb8(r: u8, g: u8, b: u8) -> Self {
        Self::from_rgb(r as f32 / 255.0, g as f32 / 255.0, b as f32 / 255.0)
    }
    pub const fn from_rgba8(r: u8, g: u8, b: u8, a: f32) -> Self {
        Self::from_rgba(r as f32 / 255.0, g as f32 / 255.0, b as f32 / 255.0, a)
    }
    /// This colour at `factor` of its opacity.
    pub fn scale_alpha(self, factor: f32) -> Self {
        Self {
            a: self.a * factor,
            ..self
        }
    }
    pub fn hsla(self) -> Hsla {
        self.into()
    }
}

impl From<Color> for Rgba {
    fn from(c: Color) -> Self {
        Rgba {
            r: c.r,
            g: c.g,
            b: c.b,
            a: c.a,
        }
    }
}

impl From<Color> for Hsla {
    fn from(c: Color) -> Self {
        Rgba::from(c).into()
    }
}

impl From<Color> for Fill {
    fn from(c: Color) -> Self {
        Hsla::from(c).into()
    }
}

// ---------------------------------------------------------------------------
// Tokens
// ---------------------------------------------------------------------------

/// Window chrome: top bar, nav rail.
pub const CHROME: Color = Color::from_rgb8(0x10, 0x12, 0x16);
/// Page background.
pub const CONTENT: Color = Color::from_rgb8(0x15, 0x17, 0x1c);
/// Cards, menus, dialogs, the drawer.
pub const RAISED: Color = Color::from_rgb8(0x1b, 0x1e, 0x25);
/// Sunken areas: log and code wells.
pub const WELL: Color = Color::from_rgb8(0x0e, 0x10, 0x14);
/// Resting control fill.
pub const FIELD: Color = Color::from_rgb8(0x1f, 0x23, 0x2b);
pub const FIELD_HOVER: Color = Color::from_rgb8(0x27, 0x2c, 0x36);
pub const FIELD_ACTIVE: Color = Color::from_rgb8(0x2e, 0x34, 0x41);
/// Alternate table rows.
pub const STRIPE: Color = Color::from_rgb8(0x19, 0x1c, 0x22);
/// Hairlines.
pub const BORDER: Color = Color::from_rgb8(0x26, 0x2a, 0x33);
pub const BORDER_STRONG: Color = Color::from_rgb8(0x34, 0x3a, 0x47);
/// Ink.
pub const TEXT: Color = Color::from_rgb8(0xe6, 0xe8, 0xee);
pub const TEXT_MUTED: Color = Color::from_rgb8(0x9a, 0xa1, 0xb1);
/// Faint Ink: hints, placeholders, secondary counts.
pub const TEXT_FAINT: Color = Color::from_rgb8(0x7c, 0x84, 0x96);
/// The one accent: selection, focus, primary action.
pub const ACCENT: Color = Color::from_rgb8(0x4c, 0x8d, 0xff);
pub const ACCENT_HOVER: Color = Color::from_rgb8(0x66, 0x9f, 0xff);
/// Selection Navy: the fill behind selected items.
pub const ACCENT_BG: Color = Color::from_rgb8(0x1d, 0x2a, 0x45);
/// Text on Selection Navy.
pub const ACCENT_TEXT: Color = Color::from_rgb8(0xcf, 0xdf, 0xff);
/// State colours: for state only.
pub const OK: Color = Color::from_rgb8(0x3e, 0xcf, 0x8e);
pub const WARN: Color = Color::from_rgb8(0xf5, 0xa5, 0x24);
pub const BAD: Color = Color::from_rgb8(0xf2, 0x55, 0x5a);
pub const IDLE: Color = Color::from_rgb8(0x6b, 0x72, 0x82);
pub const DANGER_FILL: Color = Color::from_rgb8(0xc9, 0x3a, 0x40);

/// YAML syntax colours (editor and code blocks).
pub const SYNTAX_KEY: Color = Color::from_rgb8(0x8f, 0xb8, 0xff);
pub const SYNTAX_STRING: Color = Color::from_rgb8(0xa8, 0xd4, 0x9b);
pub const SYNTAX_LITERAL: Color = Color::from_rgb8(0xe8, 0xb8, 0x6c);
pub const SYNTAX_COMMENT: Color = TEXT_FAINT;

pub const RADIUS: f32 = 6.0;
pub const RADIUS_SMALL: f32 = 4.0;

// ---------------------------------------------------------------------------
// Fonts
// ---------------------------------------------------------------------------

/// Font family names, as registered from `assets/fonts`.
pub const INTER: &str = "Inter";
pub const MONO: &str = "JetBrains Mono";
pub const ICONS: &str = "Phosphor";

pub const BODY: f32 = 13.5;
pub const SMALL: f32 = 11.5;
pub const MONO_SIZE: f32 = 12.5;

/// Font files to load at startup.
pub fn fonts() -> [&'static [u8]; 4] {
    [
        include_bytes!("../../assets/fonts/Inter-Regular.ttf"),
        include_bytes!("../../assets/fonts/Inter-SemiBold.ttf"),
        include_bytes!("../../assets/fonts/JetBrainsMono-Regular.ttf"),
        include_bytes!("../../assets/fonts/Phosphor.ttf"),
    ]
}

/// Registers the fonts and points GPUI Kit's theme at the app's palette, so Kit
/// components (inputs, the code editor, scrollbars, tooltips) match the hand-drawn ones.
/// Call once, after `gpui_kit::init`.
pub fn install(cx: &mut gpui::App) {
    let fonts = fonts().into_iter().map(Cow::Borrowed).collect();
    if let Err(e) = cx.text_system().add_fonts(fonts) {
        eprintln!("failed to load fonts: {e}");
    }
    KitTheme::change(ThemeMode::Dark, None, cx);
    KitTheme::update(cx, |t| {
        t.font_family = INTER.into();
        t.font_size = px(BODY);
        t.mono_font_family = MONO.into();
        t.mono_font_size = px(MONO_SIZE);
        t.radius = px(RADIUS_SMALL);
        t.radius_lg = px(RADIUS);
        t.shadow = true;
        let c = &mut t.colors;
        c.background = CONTENT.into();
        c.foreground = TEXT.into();
        c.border = BORDER.into();
        c.input = BORDER.into();
        c.ring = ACCENT.into();
        c.caret = TEXT.into();
        c.selection = ACCENT.scale_alpha(0.4).into();
        c.muted = FIELD.into();
        c.muted_foreground = TEXT_MUTED.into();
        c.accent = FIELD_HOVER.into();
        c.accent_foreground = TEXT.into();
        c.primary = ACCENT.into();
        c.primary_hover = ACCENT_HOVER.into();
        c.primary_active = ACCENT_HOVER.into();
        c.primary_foreground = Color::WHITE.into();
        c.secondary = FIELD.into();
        c.secondary_hover = FIELD_HOVER.into();
        c.secondary_active = FIELD_ACTIVE.into();
        c.secondary_foreground = TEXT.into();
        c.danger = DANGER_FILL.into();
        c.danger_hover = BAD.into();
        c.danger_active = BAD.into();
        c.danger_foreground = Color::WHITE.into();
        c.success = OK.into();
        c.warning = WARN.into();
        c.popover = RAISED.into();
        c.popover_foreground = TEXT.into();
        c.list = CONTENT.into();
        c.list_hover = FIELD.into();
        c.list_active = ACCENT_BG.into();
        c.list_active_border = ACCENT.into();
        c.list_even = STRIPE.into();
        c.list_head = CONTENT.into();
        c.table = CONTENT.into();
        c.table_hover = FIELD.into();
        c.table_active = ACCENT_BG.into();
        c.table_active_border = ACCENT.into();
        c.table_even = STRIPE.into();
        c.table_head = CONTENT.into();
        c.table_head_foreground = TEXT_MUTED.into();
        c.table_row_border = BORDER.into();
        c.scrollbar = Color::TRANSPARENT.into();
        c.scrollbar_thumb = BORDER_STRONG.into();
        c.scrollbar_thumb_hover = TEXT_FAINT.into();
        c.tab_bar = CHROME.into();
        c.tab = CHROME.into();
        c.tab_active = CONTENT.into();
        c.tab_foreground = TEXT_MUTED.into();
        c.tab_active_foreground = TEXT.into();
        c.title_bar = CHROME.into();
        c.title_bar_border = BORDER.into();
        c.overlay = Color::from_rgba(0.0, 0.0, 0.0, 0.45).into();
        c.drag_border = ACCENT.into();
        c.link = ACCENT.into();
        c.link_hover = ACCENT_HOVER.into();
        c.link_active = ACCENT_HOVER.into();
    });
}

/// `color` at `factor` opacity.
pub fn alpha(color: Color, factor: f32) -> Color {
    color.scale_alpha(factor)
}

/// A soft drop shadow, `y` pixels down.
pub fn shadow(y: f32, blur: f32, a: f32) -> Vec<BoxShadow> {
    vec![BoxShadow {
        color: Color::from_rgba(0.0, 0.0, 0.0, a).into(),
        offset: point(px(0.0), px(y)),
        blur_radius: px(blur),
        spread_radius: px(0.0),
        inset: false,
    }]
}

// ---------------------------------------------------------------------------
// Surfaces
// ---------------------------------------------------------------------------

/// Floating card (toasts, menus): raised fill, hairline border, soft shadow.
pub fn card() -> Div {
    div()
        .bg(RAISED)
        .text_color(TEXT)
        .border_1()
        .border_color(BORDER_STRONG)
        .rounded(px(8.0))
        .shadow(shadow(8.0, 22.0, 0.55))
}

/// Dialog panel.
pub fn dialog() -> Div {
    div()
        .bg(RAISED)
        .text_color(TEXT)
        .border_1()
        .border_color(BORDER_STRONG)
        .rounded(px(10.0))
        .shadow(shadow(10.0, 28.0, 0.5))
}

/// A bordered box (container cards in the drawer, code blocks).
pub fn boxed(fill: Color, stroke: Color, radius: f32) -> Div {
    div()
        .bg(fill)
        .text_color(TEXT)
        .border_1()
        .border_color(stroke)
        .rounded(px(radius))
}

/// The dimmed layer behind a dialog; clicks on it go to `on_click` (and nothing below).
pub fn backdrop(id: impl Into<ElementId>, on_click: Option<Message>) -> Stateful<Div> {
    let el = div()
        .id(id)
        .absolute()
        .inset_0()
        .bg(Color::from_rgba(0.0, 0.0, 0.0, 0.45))
        .occlude();
    match on_click {
        Some(m) => el.on_click(on(m)),
        None => el,
    }
}

// ---------------------------------------------------------------------------
// Text
// ---------------------------------------------------------------------------

/// Text at body size in `color`. Single line (no wrapping); add `.whitespace_normal()`
/// to wrap.
pub fn colored(s: impl Into<SharedString>, color: Color) -> Div {
    div()
        .text_size(px(BODY))
        .text_color(color)
        .whitespace_nowrap()
        .child(s.into())
}

pub fn txt(s: impl Into<SharedString>) -> Div {
    colored(s, TEXT)
}

pub fn muted(s: impl Into<SharedString>) -> Div {
    colored(s, TEXT_MUTED)
}

pub fn faint(s: impl Into<SharedString>) -> Div {
    colored(s, TEXT_FAINT)
}

pub fn small(s: impl Into<SharedString>) -> Div {
    colored(s, TEXT_MUTED).text_size(px(SMALL))
}

pub fn semibold(s: impl Into<SharedString>) -> Div {
    txt(s).font_weight(FontWeight::SEMIBOLD)
}

pub fn mono(s: impl Into<SharedString>) -> Div {
    txt(s).font_family(MONO).text_size(px(MONO_SIZE))
}

pub fn mono_muted(s: impl Into<SharedString>) -> Div {
    mono(s).text_color(TEXT_MUTED)
}

pub fn title(s: impl Into<SharedString>) -> Div {
    txt(s).text_size(px(19.0)).font_weight(FontWeight::SEMIBOLD)
}

/// Small semibold muted label for table headers and panel sections.
pub fn label_small(s: impl Into<SharedString>) -> Div {
    colored(s, TEXT_MUTED)
        .text_size(px(12.0))
        .font_weight(FontWeight::SEMIBOLD)
}

/// A Phosphor glyph (see `icons`), 14px, muted. Override with `.text_color` / `.text_size`.
pub fn icon(glyph: &'static str) -> Div {
    div()
        .font_family(ICONS)
        .text_size(px(14.0))
        .line_height(px(14.0))
        .text_color(TEXT_MUTED)
        .flex_none()
        .child(glyph)
}

/// An icon and a label, as a button's content (inherits the text colour).
pub fn icon_label(glyph: &'static str, label: impl Into<SharedString>) -> Div {
    h_flex()
        .gap(px(6.0))
        .items_center()
        .child(
            div()
                .font_family(ICONS)
                .text_size(px(14.0))
                .line_height(px(14.0))
                .child(glyph),
        )
        .child(div().text_size(px(BODY)).whitespace_nowrap().child(label.into()))
}

// ---------------------------------------------------------------------------
// Layout bits
// ---------------------------------------------------------------------------

pub fn space(size: f32) -> Div {
    div().size(px(size)).flex_none()
}

pub fn vspace(size: f32) -> Div {
    div().h(px(size)).flex_none()
}

pub fn hspace(size: f32) -> Div {
    div().w(px(size)).flex_none()
}

/// Takes the remaining space in a row or column.
pub fn fill_space() -> Div {
    div().flex_1()
}

pub fn hline() -> Div {
    div().h(px(1.0)).w_full().flex_none().bg(BORDER)
}

pub fn vline(height: f32) -> Div {
    div().w(px(1.0)).h(px(height)).flex_none().bg(BORDER_STRONG)
}

/// A filled status dot.
pub fn dot(color: Color) -> Div {
    div().size(px(8.0)).flex_none().rounded(px(4.0)).bg(color)
}

/// Adds a hover tooltip (after a short delay) to an element with an id.
pub fn tip<E: StatefulInteractiveElement>(el: E, hint: impl Into<SharedString>) -> E {
    let hint: SharedString = hint.into();
    if hint.is_empty() {
        return el;
    }
    el.tooltip(move |window, cx| gpui_kit::component::tooltip::Tooltip::new(hint.clone()).build(window, cx))
}

// ---------------------------------------------------------------------------
// Controls
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, PartialEq)]
enum Look {
    Primary,
    Danger,
    Secondary,
    Ghost,
}

/// A button's ground: fill, ink and stroke per state. `None` press disables it.
fn base_button(id: ElementId, look: Look, on_press: Option<Message>) -> Stateful<Div> {
    let enabled = on_press.is_some();
    let (bg, fg, stroke, hover_bg, hover_stroke, active_bg, active_stroke) = match look {
        Look::Primary => (
            ACCENT,
            Color::WHITE,
            ACCENT_HOVER,
            ACCENT_HOVER,
            ACCENT_HOVER,
            ACCENT_HOVER,
            ACCENT_HOVER,
        ),
        Look::Danger => (DANGER_FILL, Color::WHITE, BAD, BAD, BAD, BAD, BAD),
        Look::Secondary => (
            FIELD,
            TEXT,
            BORDER,
            FIELD_HOVER,
            BORDER_STRONG,
            FIELD_ACTIVE,
            ACCENT,
        ),
        Look::Ghost => (
            Color::TRANSPARENT,
            TEXT_MUTED,
            Color::TRANSPARENT,
            FIELD_HOVER,
            Color::TRANSPARENT,
            FIELD_HOVER,
            Color::TRANSPARENT,
        ),
    };
    let el = div()
        .id(id)
        .flex()
        .flex_none()
        .items_center()
        .justify_center()
        .rounded(px(RADIUS_SMALL))
        .border_1()
        .whitespace_nowrap();
    if !enabled {
        let (bg, fg, stroke) = match look {
            Look::Primary => (alpha(ACCENT, 0.35), alpha(Color::WHITE, 0.55), alpha(ACCENT, 0.35)),
            Look::Secondary => (alpha(FIELD, 0.6), TEXT_FAINT, BORDER),
            Look::Ghost => (Color::TRANSPARENT, alpha(TEXT_MUTED, 0.5), Color::TRANSPARENT),
            Look::Danger => (alpha(DANGER_FILL, 0.5), alpha(Color::WHITE, 0.6), alpha(BAD, 0.5)),
        };
        return el.bg(bg).text_color(fg).border_color(stroke);
    }
    let fg_hover = if look == Look::Ghost { TEXT } else { fg };
    el.bg(bg)
        .text_color(fg)
        .border_color(stroke)
        .cursor_pointer()
        .hover(move |s| s.bg(hover_bg).border_color(hover_stroke).text_color(fg_hover))
        .active(move |s| s.bg(active_bg).border_color(active_stroke))
        .when_some(on_press, |el, m| el.on_click(on(m)))
}

fn labelled(glyph: Option<&'static str>, label: SharedString, semibold: bool, size: f32) -> Div {
    let text = div()
        .text_size(px(size))
        .whitespace_nowrap()
        .when(semibold, |d| d.font_weight(FontWeight::SEMIBOLD))
        .child(label);
    match glyph {
        Some(g) => h_flex().gap(px(6.0)).items_center().child(
            div()
                .font_family(ICONS)
                .text_size(px(size + 0.5))
                .line_height(px(size + 0.5))
                .child(g),
        ),
        None => h_flex().items_center(),
    }
    .child(text)
}

/// The one filled, accent-coloured action in a group. `None` disables it.
pub fn primary_button(
    id: impl Into<ElementId>,
    glyph: Option<&'static str>,
    label: impl Into<SharedString>,
    on_press: Option<Message>,
) -> Stateful<Div> {
    base_button(id.into(), Look::Primary, on_press)
        .px(px(10.0))
        .py(px(5.0))
        .child(labelled(glyph, label.into(), true, BODY))
}

pub fn danger_button(
    id: impl Into<ElementId>,
    glyph: Option<&'static str>,
    label: impl Into<SharedString>,
    on_press: Message,
) -> Stateful<Div> {
    base_button(id.into(), Look::Danger, Some(on_press))
        .px(px(10.0))
        .py(px(5.0))
        .child(labelled(glyph, label.into(), true, BODY))
}

/// Resting control: field fill, hairline border.
pub fn secondary_button(
    id: impl Into<ElementId>,
    glyph: Option<&'static str>,
    label: impl Into<SharedString>,
    on_press: Option<Message>,
) -> Stateful<Div> {
    base_button(id.into(), Look::Secondary, on_press)
        .px(px(10.0))
        .py(px(5.0))
        .child(labelled(glyph, label.into(), false, BODY))
}

pub fn small_button(
    id: impl Into<ElementId>,
    glyph: Option<&'static str>,
    label: impl Into<SharedString>,
    on_press: Option<Message>,
) -> Stateful<Div> {
    base_button(id.into(), Look::Secondary, on_press)
        .px(px(7.0))
        .py(px(2.0))
        .child(labelled(glyph, label.into(), false, 12.5))
}

/// Frameless; a fill only on hover. Wrap any content.
pub fn ghost_button(id: impl Into<ElementId>, on_press: Option<Message>) -> Stateful<Div> {
    base_button(id.into(), Look::Ghost, on_press)
}

/// Frameless 24×24 icon button that only shows a fill on hover.
pub fn icon_button(
    id: impl Into<ElementId>,
    glyph: &'static str,
    hint: &str,
    on_press: Option<Message>,
) -> Stateful<Div> {
    let el = base_button(id.into(), Look::Ghost, on_press)
        .size(px(24.0))
        .child(
            div()
                .font_family(ICONS)
                .text_size(px(14.0))
                .line_height(px(14.0))
                .child(glyph),
        );
    tip(el, hint.to_string())
}

/// Lit (Selection Navy, accent hairline) while `on`, otherwise a ghost.
fn toggled(id: ElementId, on: bool, on_press: Message) -> Stateful<Div> {
    if on {
        div()
            .id(id)
            .flex()
            .flex_none()
            .items_center()
            .justify_center()
            .rounded(px(RADIUS_SMALL))
            .border_1()
            .bg(ACCENT_BG)
            .text_color(ACCENT_TEXT)
            .border_color(ACCENT)
            .cursor_pointer()
            .on_click(super::on(on_press))
    } else {
        base_button(id, Look::Ghost, Some(on_press))
    }
}

/// [`icon_button`] that stays lit while `on`.
pub fn icon_toggle(
    id: impl Into<ElementId>,
    glyph: &'static str,
    on: bool,
    hint: &str,
    on_press: Message,
) -> Stateful<Div> {
    let el = toggled(id.into(), on, on_press).size(px(24.0)).child(
        div()
            .font_family(ICONS)
            .text_size(px(14.0))
            .line_height(px(14.0))
            .child(glyph),
    );
    tip(el, hint.to_string())
}

/// Small on/off text toggle (e.g. `.*` for regex), sized to sit beside an icon button.
pub fn toggle_button(
    id: impl Into<ElementId>,
    label: &'static str,
    on: bool,
    hint: &str,
    on_press: Message,
) -> Stateful<Div> {
    let el = toggled(id.into(), on, on_press)
        .h(px(24.0))
        .px(px(6.0))
        .child(div().font_family(MONO).text_size(px(13.0)).child(label));
    tip(el, hint.to_string())
}

/// A small segmented control: one bordered strip, the chosen segment filled.
pub fn segmented(
    id: impl Into<ElementId>,
    labels: &[&'static str],
    selected: usize,
    hint: &str,
    on_select: impl Fn(usize) -> Message,
) -> Stateful<Div> {
    let id: ElementId = id.into();
    let segments = labels.iter().enumerate().map(|(i, label)| {
        let active = i == selected;
        let seg = div()
            .id(ElementId::NamedInteger("segment".into(), i as u64))
            .h(px(20.0))
            .px(px(9.0))
            .flex()
            .items_center()
            .rounded(px(RADIUS_SMALL - 1.0))
            .text_size(px(12.5))
            .whitespace_nowrap()
            .child(*label);
        if active {
            seg.bg(FIELD_ACTIVE)
                .text_color(TEXT)
                .font_weight(FontWeight::SEMIBOLD)
        } else {
            seg.text_color(TEXT_MUTED)
                .cursor_pointer()
                .hover(|s| s.bg(FIELD_HOVER).text_color(TEXT))
                .on_click(on(on_select(i)))
        }
    });
    let strip = h_flex()
        .id(id)
        .p(px(2.0))
        .bg(CHROME)
        .border_1()
        .border_color(BORDER)
        .rounded(px(RADIUS_SMALL))
        .children(segments);
    tip(strip, hint.to_string())
}

/// Checkbox drawn in the app's vocabulary: a 15px square, accent-filled with a check when on.
pub fn checkbox(
    id: impl Into<ElementId>,
    value: bool,
    label: impl Into<SharedString>,
    on_toggle: Message,
) -> Stateful<Div> {
    let label: SharedString = label.into();
    let mark = div()
        .size(px(15.0))
        .flex_none()
        .flex()
        .items_center()
        .justify_center()
        .rounded(px(3.0))
        .border_1()
        .bg(if value { ACCENT } else { FIELD })
        .border_color(if value { ACCENT } else { BORDER_STRONG })
        .when(value, |d| {
            d.child(
                div()
                    .font_family(ICONS)
                    .text_size(px(11.0))
                    .line_height(px(11.0))
                    .text_color(Color::WHITE)
                    .child(icon::CHECK),
            )
        });
    h_flex()
        .id(id.into())
        .h(px(22.0))
        .gap(px(7.0))
        .items_center()
        .cursor_pointer()
        .child(mark)
        .when(!label.is_empty(), |d| {
            d.child(
                div()
                    .text_size(px(BODY))
                    .text_color(if value { TEXT } else { TEXT_MUTED })
                    .whitespace_nowrap()
                    .child(label),
            )
        })
        .on_click(on(on_toggle))
}

/// Small tinted pill, e.g. a kind label or a restart count.
pub fn badge(label: impl Into<SharedString>, color: Color, filled: bool) -> Div {
    div()
        .flex_none()
        .px(px(6.0))
        .py(px(1.0))
        .rounded(px(RADIUS_SMALL))
        .bg(if filled { color } else { alpha(color, 0.16) })
        .text_size(px(SMALL))
        .font_weight(FontWeight::SEMIBOLD)
        .text_color(if filled { Color::WHITE } else { color })
        .whitespace_nowrap()
        .child(label.into())
}

/// A right-aligned nav count: text, and a colour for when the item isn't selected
/// (`None` = Faint Ink; a state colour only when the number reports state).
pub type NavBadge = Option<(String, Option<Color>)>;

fn nav_badge(badge: NavBadge, selected: bool) -> Option<Div> {
    badge.map(|(label, color)| {
        let color = match (selected, color) {
            (_, Some(c)) => c,
            (true, None) => ACCENT_TEXT,
            (false, None) => TEXT_FAINT,
        };
        div()
            .text_size(px(12.0))
            .text_color(color)
            .whitespace_nowrap()
            .child(label)
    })
}

fn nav_row(id: ElementId, selected: bool, on_press: Message) -> Stateful<Div> {
    let el = h_flex()
        .id(id)
        .w_full()
        .items_center()
        .rounded(px(RADIUS))
        .text_size(px(BODY))
        .whitespace_nowrap()
        .cursor_pointer()
        .on_click(on(on_press));
    if selected {
        el.bg(ACCENT_BG).text_color(ACCENT_TEXT)
    } else {
        el.text_color(TEXT_MUTED)
            .hover(|s| s.bg(FIELD).text_color(TEXT))
    }
}

/// Left navigation entry: icon, label, optional count; accent-tinted when selected.
pub fn nav_item(
    id: impl Into<ElementId>,
    glyph: &'static str,
    label: impl Into<SharedString>,
    badge: NavBadge,
    selected: bool,
    on_press: Message,
) -> Stateful<Div> {
    nav_row(id.into(), selected, on_press)
        .h(px(32.0))
        .px(px(10.0))
        .gap(px(10.0))
        .child(
            div()
                .font_family(ICONS)
                .text_size(px(16.0))
                .line_height(px(16.0))
                .child(glyph),
        )
        .child(
            div()
                .when(selected, |d| d.font_weight(FontWeight::SEMIBOLD))
                .child(label.into()),
        )
        .child(fill_space())
        .children(nav_badge(badge, selected))
}

/// Collapsible sidebar group header: caret, glyph, label. `marked` highlights the
/// header when it's closed over the current page.
pub fn nav_group(
    id: impl Into<ElementId>,
    glyph: &'static str,
    label: impl Into<SharedString>,
    open: bool,
    marked: bool,
    on_press: Message,
) -> Stateful<Div> {
    nav_row(id.into(), marked, on_press)
        .h(px(32.0))
        .px(px(6.0))
        .gap(px(8.0))
        .child(
            div()
                .font_family(ICONS)
                .text_size(px(12.0))
                .line_height(px(12.0))
                .text_color(TEXT_FAINT)
                .child(if open {
                    icon::CARET_DOWN
                } else {
                    icon::CARET_RIGHT
                }),
        )
        .child(
            div()
                .font_family(ICONS)
                .text_size(px(16.0))
                .line_height(px(16.0))
                .child(glyph),
        )
        .child(
            div()
                .font_weight(FontWeight::SEMIBOLD)
                .child(label.into()),
        )
}

/// A page inside a sidebar group: text only, hung off a hairline guide under the group glyph.
pub fn nav_child(
    id: impl Into<ElementId>,
    label: impl Into<SharedString>,
    badge: NavBadge,
    selected: bool,
    on_press: Message,
) -> Div {
    let guide = div()
        .relative()
        .w(px(4.0))
        .h_full()
        .flex_none()
        .child(
            div()
                .absolute()
                .left(px(1.5))
                .top_0()
                .bottom_0()
                .w(px(1.0))
                .bg(BORDER_STRONG),
        )
        .when(selected, |d| {
            d.child(
                div()
                    .absolute()
                    .left(px(1.0))
                    .top(px(7.0))
                    .bottom(px(7.0))
                    .w(px(2.0))
                    .bg(ACCENT),
            )
        });
    h_flex()
        .h(px(32.0))
        .w_full()
        .child(hspace(28.0))
        .child(guide)
        .child(hspace(4.0))
        .child(
            div().flex_1().py(px(1.0)).child(
                nav_row(id.into(), selected, on_press)
                    .h(px(30.0))
                    .px(px(10.0))
                    .child(
                        div()
                            .when(selected, |d| d.font_weight(FontWeight::SEMIBOLD))
                            .child(label.into()),
                    )
                    .child(fill_space())
                    .children(nav_badge(badge, selected)),
            ),
        )
}

/// Underlined tab strip (drawer sections).
pub fn underline_tabs(
    id: impl Into<ElementId>,
    labels: Vec<String>,
    selected: usize,
    on_select: impl Fn(usize) -> Message,
) -> Div {
    let id: ElementId = id.into();
    let tabs = labels.into_iter().enumerate().map(|(i, label)| {
        let active = i == selected;
        v_flex()
            .id(ElementId::NamedInteger("tab".into(), i as u64))
            .gap(px(6.0))
            .cursor_pointer()
            .child(
                div()
                    .text_size(px(BODY))
                    .whitespace_nowrap()
                    .text_color(if active { TEXT } else { TEXT_MUTED })
                    .when(active, |d| d.font_weight(FontWeight::SEMIBOLD))
                    .when(!active, |d| d.hover(|s| s.text_color(ACCENT_HOVER)))
                    .child(label),
            )
            .child(
                div()
                    .h(px(2.0))
                    .w_full()
                    .when(active, |d| d.bg(ACCENT)),
            )
            .on_click(on(on_select(i)))
    });
    v_flex()
        .child(h_flex().id(id).gap(px(18.0)).children(tabs))
        .child(hline())
        .child(vspace(10.0))
}

/// The ground of a dropdown trigger: field fill, caret on the right.
pub fn dropdown_button(
    id: impl Into<ElementId>,
    label: impl Into<SharedString>,
    width: f32,
    open: bool,
    on_press: Message,
) -> Stateful<Div> {
    let el = base_button(id.into(), Look::Secondary, Some(on_press))
        .w(px(width))
        .px(px(10.0))
        .py(px(5.0))
        .justify_start()
        .gap(px(6.0))
        .child(
            div()
                .flex_1()
                .overflow_hidden()
                .text_size(px(BODY))
                .text_ellipsis()
                .child(label.into()),
        )
        .child(
            div()
                .font_family(ICONS)
                .text_size(px(13.0))
                .line_height(px(13.0))
                .text_color(TEXT_MUTED)
                .child(if open {
                    icon::CARET_UP
                } else {
                    icon::CARET_DOWN
                }),
        );
    if open {
        el.bg(FIELD_HOVER).border_color(ACCENT)
    } else {
        el
    }
}

/// A row in a menu or a dropdown list; `selected` keeps it tinted.
fn menu_row(id: ElementId, selected: bool, color: Color, on_press: Option<Message>) -> Stateful<Div> {
    let el = h_flex()
        .id(id)
        .w_full()
        .px(px(8.0))
        .py(px(5.0))
        .items_center()
        .rounded(px(RADIUS_SMALL))
        .text_size(px(BODY))
        .whitespace_nowrap();
    let Some(m) = on_press else {
        return el.text_color(TEXT_FAINT);
    };
    let el = if selected {
        el.bg(ACCENT_BG).text_color(ACCENT_TEXT)
    } else {
        el.text_color(color)
    };
    el.cursor_pointer()
        .hover(move |s| {
            s.bg(FIELD_HOVER)
                .text_color(if selected { ACCENT_TEXT } else { color })
        })
        .on_click(on(m))
}

/// A menu entry: optional icon, label, full width.
pub fn menu_entry(
    id: impl Into<ElementId>,
    glyph: Option<&'static str>,
    label: impl Into<SharedString>,
    color: Color,
    on_press: Option<Message>,
) -> Stateful<Div> {
    menu_row(id.into(), false, color, on_press).child(labelled(glyph, label.into(), false, BODY))
}

/// A selectable list row (dropdown options).
pub fn option_entry(
    id: impl Into<ElementId>,
    label: impl Into<SharedString>,
    selected: bool,
    on_press: Message,
) -> Stateful<Div> {
    menu_row(id.into(), selected, TEXT, Some(on_press)).child(label.into())
}
