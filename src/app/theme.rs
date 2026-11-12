//! Visual system: colour tokens, fonts, egui widget styling and a few custom-drawn
//! controls. A dark developer tool in the Lens manner: three neutral layers, hairline
//! borders, one blue accent for selection/focus/primary action, state colours for state only.

use std::sync::Arc;

use eframe::egui::{
    self, Color32, CornerRadius, FontData, FontDefinitions, FontFamily, FontId, Margin, Response,
    RichText, Sense, Shadow, Stroke, TextStyle, Vec2,
};
use egui_phosphor::regular as icon;

// ---------------------------------------------------------------------------
// Tokens
// ---------------------------------------------------------------------------

/// Window chrome: top bar, nav rail.
pub const CHROME: Color32 = Color32::from_rgb(0x10, 0x12, 0x16);
/// Main content ground.
pub const CONTENT: Color32 = Color32::from_rgb(0x15, 0x17, 0x1c);
/// Drawer, dialogs, menus, toasts.
pub const RAISED: Color32 = Color32::from_rgb(0x1b, 0x1e, 0x25);
/// Log well, slightly below the content.
pub const WELL: Color32 = Color32::from_rgb(0x0e, 0x10, 0x14);
/// Text fields and resting controls.
pub const FIELD: Color32 = Color32::from_rgb(0x1f, 0x23, 0x2b);
pub const FIELD_HOVER: Color32 = Color32::from_rgb(0x27, 0x2c, 0x36);
pub const FIELD_ACTIVE: Color32 = Color32::from_rgb(0x2e, 0x34, 0x41);
/// Alternate table rows.
pub const STRIPE: Color32 = Color32::from_rgb(0x19, 0x1c, 0x22);

pub const BORDER: Color32 = Color32::from_rgb(0x26, 0x2a, 0x33);
pub const BORDER_STRONG: Color32 = Color32::from_rgb(0x34, 0x3a, 0x47);

pub const TEXT: Color32 = Color32::from_rgb(0xe6, 0xe8, 0xee);
pub const TEXT_MUTED: Color32 = Color32::from_rgb(0x9a, 0xa1, 0xb1);
/// Hints and disabled text; still at least 4.5:1 on the content ground.
pub const TEXT_FAINT: Color32 = Color32::from_rgb(0x7c, 0x84, 0x96);

pub const ACCENT: Color32 = Color32::from_rgb(0x4c, 0x8d, 0xff);
pub const ACCENT_HOVER: Color32 = Color32::from_rgb(0x66, 0x9f, 0xff);
/// Selected-row / selected-item fill.
pub const ACCENT_BG: Color32 = Color32::from_rgb(0x1d, 0x2a, 0x45);
/// Text on an accent-tinted fill.
pub const ACCENT_TEXT: Color32 = Color32::from_rgb(0xcf, 0xdf, 0xff);

pub const OK: Color32 = Color32::from_rgb(0x3e, 0xcf, 0x8e);
pub const WARN: Color32 = Color32::from_rgb(0xf5, 0xa5, 0x24);
pub const BAD: Color32 = Color32::from_rgb(0xf2, 0x55, 0x5a);
pub const IDLE: Color32 = Color32::from_rgb(0x6b, 0x72, 0x82);

/// YAML editor syntax: keys in a soft Signal Blue, strings sage, literals (numbers,
/// booleans, null) amber, comments Faint Ink. Plain values stay Ink.
pub const SYNTAX_KEY: Color32 = Color32::from_rgb(0x8f, 0xb8, 0xff);
pub const SYNTAX_STRING: Color32 = Color32::from_rgb(0xa8, 0xd4, 0x9b);
pub const SYNTAX_LITERAL: Color32 = Color32::from_rgb(0xe8, 0xb8, 0x6c);
pub const SYNTAX_COMMENT: Color32 = TEXT_FAINT;

pub const RADIUS: u8 = 6;
pub const RADIUS_SMALL: u8 = 4;

/// Font family for titles, headers and emphasis (egui has no font weights).
pub fn semibold_family() -> FontFamily {
    FontFamily::Name("semibold".into())
}

// ---------------------------------------------------------------------------
// Install
// ---------------------------------------------------------------------------

pub fn install(ctx: &egui::Context) {
    install_fonts(ctx);
    ctx.style_mut(|style| {
        style.text_styles = [
            (
                TextStyle::Small,
                FontId::new(11.5, FontFamily::Proportional),
            ),
            (TextStyle::Body, FontId::new(13.5, FontFamily::Proportional)),
            (
                TextStyle::Button,
                FontId::new(13.5, FontFamily::Proportional),
            ),
            (TextStyle::Heading, FontId::new(18.0, semibold_family())),
            (
                TextStyle::Monospace,
                FontId::new(12.5, FontFamily::Monospace),
            ),
        ]
        .into();

        let s = &mut style.spacing;
        s.item_spacing = Vec2::new(8.0, 6.0);
        s.button_padding = Vec2::new(10.0, 5.0);
        s.interact_size = Vec2::new(36.0, 26.0);
        s.menu_margin = Margin::same(6);
        s.window_margin = Margin::same(16);
        s.icon_width = 15.0;
        s.icon_width_inner = 9.0;
        s.combo_height = 320.0;
        s.scroll = egui::style::ScrollStyle::floating();
        s.scroll.bar_width = 8.0;
        s.scroll.floating_width = 3.0;

        // Labels must not swallow clicks meant for table rows.
        style.interaction.selectable_labels = false;

        style.visuals = visuals();
    });
}

fn install_fonts(ctx: &egui::Context) {
    let mut fonts = FontDefinitions::default();
    let mut add = |name: &str, bytes: &'static [u8]| {
        fonts
            .font_data
            .insert(name.into(), Arc::new(FontData::from_static(bytes)));
    };
    add(
        "inter",
        include_bytes!("../../assets/fonts/Inter-Regular.ttf"),
    );
    add(
        "inter-semibold",
        include_bytes!("../../assets/fonts/Inter-SemiBold.ttf"),
    );
    add(
        "jetbrains-mono",
        include_bytes!("../../assets/fonts/JetBrainsMono-Regular.ttf"),
    );
    egui_phosphor::add_to_fonts(&mut fonts, egui_phosphor::Variant::Regular);

    // egui's bundled fonts stay as fallbacks for glyphs ours lack.
    let defaults = |family: FontFamily| fonts.families.get(&family).cloned().unwrap_or_default();
    let proportional_fallback: Vec<String> = defaults(FontFamily::Proportional)
        .into_iter()
        .filter(|f| f != "phosphor")
        .collect();
    let mono_fallback = defaults(FontFamily::Monospace);

    // The embedded text fonts were subset to drop their private-use glyphs (Inter ships
    // hundreds), so Phosphor's icons in that range are never shadowed. See assets/fonts.
    let chain = |first: &str, rest: &[String]| {
        let mut v = vec![first.to_string(), "phosphor".to_string()];
        v.extend(rest.iter().cloned());
        v
    };
    fonts.families.insert(
        FontFamily::Proportional,
        chain("inter", &proportional_fallback),
    );
    fonts.families.insert(
        semibold_family(),
        chain("inter-semibold", &proportional_fallback),
    );
    fonts.families.insert(
        FontFamily::Monospace,
        chain("jetbrains-mono", &mono_fallback),
    );
    ctx.set_fonts(fonts);
}

fn visuals() -> egui::Visuals {
    let mut v = egui::Visuals::dark();
    // Controls take the small radius: egui also uses it for checkboxes, where 6px on a
    // 15px box reads as a radio button.
    let r = CornerRadius::same(RADIUS_SMALL);
    let hairline = |c| Stroke::new(1.0, c);

    v.panel_fill = CONTENT;
    v.window_fill = RAISED;
    v.window_stroke = hairline(BORDER_STRONG);
    v.window_corner_radius = CornerRadius::same(10);
    v.window_shadow = Shadow {
        offset: [0, 10],
        blur: 28,
        spread: 0,
        color: Color32::from_black_alpha(130),
    };
    v.popup_shadow = Shadow {
        offset: [0, 6],
        blur: 18,
        spread: 0,
        color: Color32::from_black_alpha(120),
    };
    v.menu_corner_radius = CornerRadius::same(8);
    v.window_highlight_topmost = false;

    v.faint_bg_color = STRIPE;
    v.extreme_bg_color = CHROME;
    v.text_edit_bg_color = Some(CHROME);
    v.code_bg_color = WELL;
    v.hyperlink_color = ACCENT_HOVER;
    v.warn_fg_color = WARN;
    v.error_fg_color = BAD;
    v.weak_text_color = Some(TEXT_MUTED);
    v.override_text_color = None;

    v.selection.bg_fill = ACCENT_BG;
    v.selection.stroke = hairline(ACCENT_TEXT);
    v.text_cursor.stroke = Stroke::new(1.5, ACCENT_HOVER);

    v.collapsing_header_frame = false;
    v.indent_has_left_vline = false;
    v.striped = true;
    v.slider_trailing_fill = true;
    v.handle_shape = egui::style::HandleShape::Rect { aspect_ratio: 0.5 };
    v.interact_cursor = Some(egui::CursorIcon::PointingHand);

    let w = &mut v.widgets;
    w.noninteractive.bg_fill = RAISED;
    w.noninteractive.weak_bg_fill = RAISED;
    w.noninteractive.bg_stroke = hairline(BORDER);
    w.noninteractive.fg_stroke = hairline(TEXT);
    w.noninteractive.corner_radius = r;

    w.inactive.bg_fill = FIELD;
    w.inactive.weak_bg_fill = FIELD;
    w.inactive.bg_stroke = hairline(BORDER);
    w.inactive.fg_stroke = hairline(TEXT);
    w.inactive.corner_radius = r;
    w.inactive.expansion = 0.0;

    w.hovered.bg_fill = FIELD_HOVER;
    w.hovered.weak_bg_fill = FIELD_HOVER;
    w.hovered.bg_stroke = hairline(BORDER_STRONG);
    w.hovered.fg_stroke = hairline(TEXT);
    w.hovered.corner_radius = r;
    w.hovered.expansion = 0.0;

    w.active.bg_fill = FIELD_ACTIVE;
    w.active.weak_bg_fill = FIELD_ACTIVE;
    w.active.bg_stroke = hairline(ACCENT);
    w.active.fg_stroke = hairline(TEXT);
    w.active.corner_radius = r;
    w.active.expansion = 0.0;

    w.open.bg_fill = FIELD_HOVER;
    w.open.weak_bg_fill = FIELD_HOVER;
    w.open.bg_stroke = hairline(ACCENT);
    w.open.fg_stroke = hairline(TEXT);
    w.open.corner_radius = r;
    v
}

// ---------------------------------------------------------------------------
// Frames
// ---------------------------------------------------------------------------

pub fn chrome_frame() -> egui::Frame {
    egui::Frame::new()
        .fill(CHROME)
        .inner_margin(Margin::symmetric(14, 0))
}

pub fn content_frame() -> egui::Frame {
    egui::Frame::new().fill(CONTENT).inner_margin(Margin {
        left: 20,
        right: 20,
        top: 16,
        bottom: 8,
    })
}

pub fn raised_frame() -> egui::Frame {
    egui::Frame::new()
        .fill(RAISED)
        .inner_margin(Margin::symmetric(18, 14))
}

pub fn well_frame() -> egui::Frame {
    egui::Frame::new()
        .fill(WELL)
        .inner_margin(Margin::symmetric(14, 8))
}

/// Floating card (toasts): raised fill, hairline border, soft offset shadow.
pub fn card_frame() -> egui::Frame {
    egui::Frame::new()
        .fill(RAISED)
        .stroke(Stroke::new(1.0, BORDER_STRONG))
        .corner_radius(CornerRadius::same(8))
        .inner_margin(Margin::symmetric(14, 10))
        .shadow(Shadow {
            offset: [0, 8],
            blur: 22,
            spread: 0,
            color: Color32::from_black_alpha(140),
        })
}

// ---------------------------------------------------------------------------
// Text helpers
// ---------------------------------------------------------------------------

pub fn semibold(text: impl Into<String>) -> RichText {
    RichText::new(text).family(semibold_family())
}

pub fn title(text: impl Into<String>) -> RichText {
    RichText::new(text)
        .family(semibold_family())
        .size(19.0)
        .color(TEXT)
}

/// Small semibold muted label for table headers and panel sections.
pub fn label_small(text: impl Into<String>) -> RichText {
    RichText::new(text)
        .family(semibold_family())
        .size(12.0)
        .color(TEXT_MUTED)
}

// ---------------------------------------------------------------------------
// Controls
// ---------------------------------------------------------------------------

/// The one filled, accent-coloured action in a group.
pub fn primary_button(text: impl Into<String>) -> egui::Button<'static> {
    egui::Button::new(
        RichText::new(text)
            .family(semibold_family())
            .color(Color32::WHITE),
    )
    .fill(ACCENT)
    .stroke(Stroke::new(1.0, ACCENT_HOVER))
}

pub fn danger_button(text: impl Into<String>) -> egui::Button<'static> {
    egui::Button::new(
        RichText::new(text)
            .family(semibold_family())
            .color(Color32::WHITE),
    )
    .fill(Color32::from_rgb(0xc9, 0x3a, 0x40))
    .stroke(Stroke::new(1.0, BAD))
}

/// Frameless icon button that only shows a fill on hover.
pub fn icon_button(ui: &mut egui::Ui, glyph: &str, hint: &str) -> Response {
    let size = Vec2::splat(24.0);
    let (rect, resp) = ui.allocate_exact_size(size, Sense::click());
    if ui.is_rect_visible(rect) {
        let p = ui.painter();
        if resp.hovered() {
            p.rect_filled(rect, CornerRadius::same(RADIUS_SMALL), FIELD_HOVER);
        }
        let color = if resp.hovered() { TEXT } else { TEXT_MUTED };
        p.text(
            rect.center(),
            egui::Align2::CENTER_CENTER,
            glyph,
            FontId::proportional(14.0),
            color,
        );
    }
    resp.on_hover_text(hint)
}

/// [`icon_button`] that stays lit (Selection Navy, accent hairline) while `on`.
pub fn icon_toggle(ui: &mut egui::Ui, glyph: &str, on: bool, hint: &str) -> Response {
    let (rect, resp) = ui.allocate_exact_size(Vec2::splat(24.0), Sense::click());
    if ui.is_rect_visible(rect) {
        let p = ui.painter();
        let radius = CornerRadius::same(RADIUS_SMALL);
        if on {
            p.rect_filled(rect, radius, ACCENT_BG);
            p.rect_stroke(
                rect,
                radius,
                Stroke::new(1.0, ACCENT),
                egui::StrokeKind::Inside,
            );
        } else if resp.hovered() {
            p.rect_filled(rect, radius, FIELD_HOVER);
        }
        let color = if on {
            ACCENT_TEXT
        } else if resp.hovered() {
            TEXT
        } else {
            TEXT_MUTED
        };
        p.text(
            rect.center(),
            egui::Align2::CENTER_CENTER,
            glyph,
            FontId::proportional(14.0),
            color,
        );
    }
    resp.on_hover_text(hint)
}

/// Small on/off text toggle (e.g. `.*` for regex), sized to sit beside an icon button.
pub fn toggle_button(ui: &mut egui::Ui, text: &str, on: bool, hint: &str) -> Response {
    let font = FontId::monospace(13.0);
    let galley = ui
        .painter()
        .layout_no_wrap(text.to_string(), font.clone(), TEXT);
    let size = Vec2::new((galley.size().x + 12.0).max(24.0), 24.0);
    let (rect, resp) = ui.allocate_exact_size(size, Sense::click());
    if ui.is_rect_visible(rect) {
        let p = ui.painter();
        let radius = CornerRadius::same(RADIUS_SMALL);
        if on {
            p.rect_filled(rect, radius, ACCENT_BG);
            p.rect_stroke(
                rect,
                radius,
                Stroke::new(1.0, ACCENT),
                egui::StrokeKind::Inside,
            );
        } else if resp.hovered() {
            p.rect_filled(rect, radius, FIELD_HOVER);
        }
        let color = if on {
            ACCENT_TEXT
        } else if resp.hovered() {
            TEXT
        } else {
            TEXT_MUTED
        };
        p.text(
            rect.center(),
            egui::Align2::CENTER_CENTER,
            text,
            font,
            color,
        );
    }
    resp.on_hover_text(hint)
}

/// A small segmented control: one bordered strip, the chosen segment filled.
/// Returns the index clicked when it differs from `selected`.
pub fn segmented(ui: &mut egui::Ui, labels: &[&str], selected: usize, hint: &str) -> Option<usize> {
    let font = FontId::proportional(12.5);
    let bold = FontId::new(12.5, semibold_family());
    let widths: Vec<f32> = labels
        .iter()
        .map(|l| {
            ui.painter()
                .layout_no_wrap(l.to_string(), bold.clone(), TEXT)
                .size()
                .x
                + 18.0
        })
        .collect();
    let size = Vec2::new(widths.iter().sum::<f32>() + 4.0, 24.0);
    let (rect, resp) = ui.allocate_exact_size(size, Sense::hover());
    let mut clicked = None;
    let radius = CornerRadius::same(RADIUS_SMALL);
    ui.painter().rect_filled(rect, radius, CHROME);
    ui.painter().rect_stroke(
        rect,
        radius,
        Stroke::new(1.0, BORDER),
        egui::StrokeKind::Inside,
    );
    let mut x = rect.left() + 2.0;
    for (i, (label, w)) in labels.iter().zip(&widths).enumerate() {
        let seg =
            egui::Rect::from_min_size(egui::pos2(x, rect.top() + 2.0), Vec2::new(*w, size.y - 4.0));
        x += w;
        let r = ui.interact(seg, resp.id.with(i), Sense::click());
        let on = i == selected;
        let p = ui.painter();
        if on {
            p.rect_filled(
                seg,
                CornerRadius::same(RADIUS_SMALL.saturating_sub(1)),
                FIELD_ACTIVE,
            );
        } else if r.hovered() {
            p.rect_filled(
                seg,
                CornerRadius::same(RADIUS_SMALL.saturating_sub(1)),
                FIELD_HOVER,
            );
        }
        let (f, color) = if on {
            (bold.clone(), TEXT)
        } else {
            (font.clone(), if r.hovered() { TEXT } else { TEXT_MUTED })
        };
        p.text(seg.center(), egui::Align2::CENTER_CENTER, *label, f, color);
        if r.on_hover_text(hint).clicked() && !on {
            clicked = Some(i);
        }
    }
    clicked
}

/// Left navigation entry: icon, label, optional count; accent-tinted when selected.
pub fn nav_item(
    ui: &mut egui::Ui,
    glyph: &str,
    label: &str,
    count: Option<usize>,
    selected: bool,
) -> Response {
    nav_item_badge(ui, glyph, label, plain_count(count), selected)
}

/// A right-aligned nav count: text, and a colour for when the item isn't selected
/// (`None` = Faint Ink; a state colour only when the number reports state).
pub type NavBadge = Option<(String, Option<Color32>)>;

fn plain_count(count: Option<usize>) -> NavBadge {
    count.filter(|n| *n > 0).map(|n| (n.to_string(), None))
}

fn paint_nav_badge(p: &egui::Painter, rect: egui::Rect, badge: &NavBadge, selected: bool) {
    if let Some((text, color)) = badge {
        let color = match (selected, color) {
            (_, Some(c)) => *c,
            (true, None) => ACCENT_TEXT,
            (false, None) => TEXT_FAINT,
        };
        p.text(
            egui::pos2(rect.right() - 10.0, rect.center().y),
            egui::Align2::RIGHT_CENTER,
            text,
            FontId::proportional(12.0),
            color,
        );
    }
}

/// [`nav_item`] with a custom count (see [`NavBadge`]).
pub fn nav_item_badge(
    ui: &mut egui::Ui,
    glyph: &str,
    label: &str,
    badge: NavBadge,
    selected: bool,
) -> Response {
    let height = 32.0;
    let (rect, resp) =
        ui.allocate_exact_size(Vec2::new(ui.available_width(), height), Sense::click());
    if ui.is_rect_visible(rect) {
        let p = ui.painter();
        let fill = if selected {
            ACCENT_BG
        } else if resp.hovered() {
            FIELD
        } else {
            Color32::TRANSPARENT
        };
        p.rect_filled(rect, CornerRadius::same(RADIUS), fill);
        let (icon_color, text_color) = if selected {
            (ACCENT_HOVER, ACCENT_TEXT)
        } else if resp.hovered() {
            (TEXT, TEXT)
        } else {
            (TEXT_MUTED, TEXT_MUTED)
        };
        let y = rect.center().y;
        p.text(
            egui::pos2(rect.left() + 12.0, y),
            egui::Align2::LEFT_CENTER,
            glyph,
            FontId::proportional(16.0),
            icon_color,
        );
        let font = if selected {
            FontId::new(13.5, semibold_family())
        } else {
            FontId::proportional(13.5)
        };
        p.text(
            egui::pos2(rect.left() + 38.0, y),
            egui::Align2::LEFT_CENTER,
            label,
            font,
            text_color,
        );
        paint_nav_badge(p, rect, &badge, selected);
    }
    resp
}

/// Collapsible sidebar group header: caret, glyph, label. `marked` highlights the
/// header when it's closed over the current page.
pub fn nav_group(
    ui: &mut egui::Ui,
    glyph: &str,
    label: &str,
    open: bool,
    marked: bool,
) -> Response {
    let height = 32.0;
    let (rect, resp) =
        ui.allocate_exact_size(Vec2::new(ui.available_width(), height), Sense::click());
    if ui.is_rect_visible(rect) {
        let p = ui.painter();
        let fill = if marked {
            ACCENT_BG
        } else if resp.hovered() {
            FIELD
        } else {
            Color32::TRANSPARENT
        };
        p.rect_filled(rect, CornerRadius::same(RADIUS), fill);
        let color = if marked {
            ACCENT_TEXT
        } else if resp.hovered() {
            TEXT
        } else {
            TEXT_MUTED
        };
        let y = rect.center().y;
        let caret = if open {
            egui_phosphor::regular::CARET_DOWN
        } else {
            egui_phosphor::regular::CARET_RIGHT
        };
        p.text(
            egui::pos2(rect.left() + 6.0, y),
            egui::Align2::LEFT_CENTER,
            caret,
            FontId::proportional(12.0),
            TEXT_FAINT,
        );
        p.text(
            egui::pos2(rect.left() + 22.0, y),
            egui::Align2::LEFT_CENTER,
            glyph,
            FontId::proportional(16.0),
            color,
        );
        p.text(
            egui::pos2(rect.left() + 46.0, y),
            egui::Align2::LEFT_CENTER,
            label,
            FontId::new(13.5, semibold_family()),
            color,
        );
    }
    resp.on_hover_cursor(egui::CursorIcon::PointingHand)
}

/// A page inside a sidebar group: text only, hung off a hairline guide under the group glyph.
pub fn nav_child_badge(
    ui: &mut egui::Ui,
    label: &str,
    badge: NavBadge,
    selected: bool,
) -> Response {
    let height = 30.0;
    let (rect, resp) =
        ui.allocate_exact_size(Vec2::new(ui.available_width(), height), Sense::click());
    if ui.is_rect_visible(rect) {
        let p = ui.painter();
        // Guide spans the 2px gap above so consecutive children read as one line.
        let guide_x = rect.left() + 30.0;
        p.line_segment(
            [
                egui::pos2(guide_x, rect.top() - 2.0),
                egui::pos2(guide_x, rect.bottom()),
            ],
            egui::Stroke::new(1.0, BORDER_STRONG),
        );
        let item = egui::Rect::from_min_max(egui::pos2(rect.left() + 36.0, rect.top()), rect.max);
        let fill = if selected {
            ACCENT_BG
        } else if resp.hovered() {
            FIELD
        } else {
            Color32::TRANSPARENT
        };
        p.rect_filled(item, CornerRadius::same(RADIUS), fill);
        if selected {
            // The guide lights up beside the current page.
            p.line_segment(
                [
                    egui::pos2(guide_x, rect.top() + 6.0),
                    egui::pos2(guide_x, rect.bottom() - 6.0),
                ],
                egui::Stroke::new(2.0, ACCENT),
            );
        }
        let text_color = if selected {
            ACCENT_TEXT
        } else if resp.hovered() {
            TEXT
        } else {
            TEXT_MUTED
        };
        let font = if selected {
            FontId::new(13.5, semibold_family())
        } else {
            FontId::proportional(13.5)
        };
        let y = rect.center().y;
        p.text(
            egui::pos2(item.left() + 10.0, y),
            egui::Align2::LEFT_CENTER,
            label,
            font,
            text_color,
        );
        paint_nav_badge(p, rect, &badge, selected);
    }
    resp
}

/// Underlined tab strip (drawer sections). Returns the clicked index.
pub fn underline_tabs(ui: &mut egui::Ui, labels: &[String], selected: usize) -> Option<usize> {
    let mut clicked = None;
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 18.0;
        for (i, label) in labels.iter().enumerate() {
            let active = i == selected;
            let text = if active {
                RichText::new(label).family(semibold_family()).color(TEXT)
            } else {
                RichText::new(label).color(TEXT_MUTED)
            };
            let resp = ui.add(egui::Label::new(text).sense(Sense::click()));
            let resp = if resp.hovered() && !active {
                resp.on_hover_cursor(egui::CursorIcon::PointingHand)
            } else {
                resp
            };
            if active {
                let r = resp.rect;
                ui.painter().line_segment(
                    [
                        egui::pos2(r.left(), r.bottom() + 7.0),
                        egui::pos2(r.right(), r.bottom() + 7.0),
                    ],
                    Stroke::new(2.0, ACCENT),
                );
            }
            if resp.clicked() {
                clicked = Some(i);
            }
        }
    });
    ui.add_space(6.0);
    let rect = ui.max_rect();
    let y = ui.cursor().top();
    ui.painter().line_segment(
        [egui::pos2(rect.left(), y), egui::pos2(rect.right(), y)],
        Stroke::new(1.0, BORDER),
    );
    ui.add_space(10.0);
    clicked
}

/// Small tinted pill, e.g. a kind label or a restart count.
pub fn badge(ui: &mut egui::Ui, text: &str, color: Color32, filled: bool) -> Response {
    let galley = ui.painter().layout_no_wrap(
        text.to_string(),
        FontId::new(11.5, semibold_family()),
        if filled { Color32::WHITE } else { color },
    );
    let size = galley.size() + Vec2::new(12.0, 4.0);
    let (rect, resp) = ui.allocate_exact_size(size, Sense::hover());
    if ui.is_rect_visible(rect) {
        let fill = if filled {
            color
        } else {
            color.gamma_multiply(0.16)
        };
        ui.painter()
            .rect_filled(rect, CornerRadius::same(RADIUS_SMALL), fill);
        ui.painter()
            .galley(rect.center() - galley.size() / 2.0, galley, color);
    }
    resp
}

/// Search field with a leading magnifier, as used for list filters.
pub fn search_field(ui: &mut egui::Ui, text: &mut String, hint: &str, width: f32) -> Response {
    ui.add(
        egui::TextEdit::singleline(text)
            .hint_text(
                RichText::new(format!("{}  {hint}", icon::MAGNIFYING_GLASS)).color(TEXT_FAINT),
            )
            .desired_width(width)
            .margin(Margin::symmetric(8, 5)),
    )
}

/// Dropdown caret for every combo box (replaces egui's filled triangle).
pub fn caret(ui: &egui::Ui, rect: egui::Rect, _visuals: &egui::style::WidgetVisuals, open: bool) {
    let glyph = if open {
        icon::CARET_UP
    } else {
        icon::CARET_DOWN
    };
    ui.painter().text(
        rect.center(),
        egui::Align2::CENTER_CENTER,
        glyph,
        FontId::proportional(13.0),
        TEXT_MUTED,
    );
}

/// Checkbox drawn in the app's vocabulary: a 15px square, accent-filled with a check when on.
pub fn checkbox(ui: &mut egui::Ui, value: &mut bool, label: &str) -> Response {
    let font = FontId::proportional(13.5);
    let text_color = if *value { TEXT } else { TEXT_MUTED };
    let galley = ui
        .painter()
        .layout_no_wrap(label.to_string(), font, text_color);
    let box_size = 15.0;
    let gap = if label.is_empty() { 0.0 } else { 7.0 };
    let size = Vec2::new(
        box_size + gap + galley.size().x,
        box_size.max(galley.size().y).max(22.0),
    );
    let (rect, mut resp) = ui.allocate_exact_size(size, Sense::click());
    if resp.clicked() {
        *value = !*value;
        resp.mark_changed();
    }
    if ui.is_rect_visible(rect) {
        let p = ui.painter();
        let b = egui::Rect::from_min_size(
            egui::pos2(rect.left(), rect.center().y - box_size / 2.0),
            Vec2::splat(box_size),
        );
        if *value {
            p.rect_filled(
                b,
                CornerRadius::same(3),
                if resp.hovered() { ACCENT_HOVER } else { ACCENT },
            );
            p.text(
                b.center(),
                egui::Align2::CENTER_CENTER,
                icon::CHECK,
                FontId::proportional(11.5),
                Color32::WHITE,
            );
        } else {
            p.rect_filled(
                b,
                CornerRadius::same(3),
                if resp.hovered() { FIELD_HOVER } else { FIELD },
            );
            p.rect_stroke(
                b,
                CornerRadius::same(3),
                Stroke::new(
                    1.0,
                    if resp.hovered() {
                        BORDER_STRONG
                    } else {
                        BORDER_STRONG.gamma_multiply(0.85)
                    },
                ),
                egui::StrokeKind::Inside,
            );
        }
        p.galley(
            egui::pos2(b.right() + gap, rect.center().y - galley.size().y / 2.0),
            galley,
            text_color,
        );
    }
    resp
}

/// Frameless icon button that opens a menu.
pub fn icon_menu(
    ui: &mut egui::Ui,
    glyph: &str,
    hint: &str,
    add_contents: impl FnOnce(&mut egui::Ui),
) -> Response {
    let resp = icon_button(ui, glyph, hint);
    egui::Popup::menu(&resp).show(|ui| {
        ui.set_min_width(180.0);
        add_contents(ui);
    });
    resp
}
