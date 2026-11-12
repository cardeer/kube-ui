//! Visual system: colour tokens, fonts, widget styles and the app's own small controls.
//! A dark developer tool in the Lens manner: three neutral layers, hairline borders, one
//! blue accent for selection/focus/primary action, state colours for state only.

use iced::{
    Background, Border, Color, Element, Font, Length, Padding, Shadow, Theme, Vector,
    font::Weight,
    widget::{
        Space, button, column, container, mouse_area, row, rule, scrollable, stack, text,
        text_editor, text_input, tooltip,
    },
};

use super::icons as icon;

// ---------------------------------------------------------------------------
// Tokens
// ---------------------------------------------------------------------------

/// Window chrome: top bar, nav rail.
pub const CHROME: Color = Color::from_rgb8(0x10, 0x12, 0x16);
/// Main content ground.
pub const CONTENT: Color = Color::from_rgb8(0x15, 0x17, 0x1c);
/// Drawer, dialogs, menus, toasts.
pub const RAISED: Color = Color::from_rgb8(0x1b, 0x1e, 0x25);
/// Log well, slightly below the content.
pub const WELL: Color = Color::from_rgb8(0x0e, 0x10, 0x14);
/// Text fields and resting controls.
pub const FIELD: Color = Color::from_rgb8(0x1f, 0x23, 0x2b);
pub const FIELD_HOVER: Color = Color::from_rgb8(0x27, 0x2c, 0x36);
pub const FIELD_ACTIVE: Color = Color::from_rgb8(0x2e, 0x34, 0x41);
/// Alternate table rows.
pub const STRIPE: Color = Color::from_rgb8(0x19, 0x1c, 0x22);

pub const BORDER: Color = Color::from_rgb8(0x26, 0x2a, 0x33);
pub const BORDER_STRONG: Color = Color::from_rgb8(0x34, 0x3a, 0x47);

pub const TEXT: Color = Color::from_rgb8(0xe6, 0xe8, 0xee);
pub const TEXT_MUTED: Color = Color::from_rgb8(0x9a, 0xa1, 0xb1);
/// Hints and disabled text; still at least 4.5:1 on the content ground.
pub const TEXT_FAINT: Color = Color::from_rgb8(0x7c, 0x84, 0x96);

pub const ACCENT: Color = Color::from_rgb8(0x4c, 0x8d, 0xff);
pub const ACCENT_HOVER: Color = Color::from_rgb8(0x66, 0x9f, 0xff);
/// Selected-row / selected-item fill.
pub const ACCENT_BG: Color = Color::from_rgb8(0x1d, 0x2a, 0x45);
/// Text on an accent-tinted fill.
pub const ACCENT_TEXT: Color = Color::from_rgb8(0xcf, 0xdf, 0xff);

pub const OK: Color = Color::from_rgb8(0x3e, 0xcf, 0x8e);
pub const WARN: Color = Color::from_rgb8(0xf5, 0xa5, 0x24);
pub const BAD: Color = Color::from_rgb8(0xf2, 0x55, 0x5a);
pub const IDLE: Color = Color::from_rgb8(0x6b, 0x72, 0x82);
const DANGER_FILL: Color = Color::from_rgb8(0xc9, 0x3a, 0x40);

/// YAML editor syntax: keys in a soft Signal Blue, strings sage, literals (numbers,
/// booleans, null) amber, comments Faint Ink. Plain values stay Ink.
pub const SYNTAX_KEY: Color = Color::from_rgb8(0x8f, 0xb8, 0xff);
pub const SYNTAX_STRING: Color = Color::from_rgb8(0xa8, 0xd4, 0x9b);
pub const SYNTAX_LITERAL: Color = Color::from_rgb8(0xe8, 0xb8, 0x6c);
pub const SYNTAX_COMMENT: Color = TEXT_FAINT;

pub const RADIUS: f32 = 6.0;
pub const RADIUS_SMALL: f32 = 4.0;

// ---------------------------------------------------------------------------
// Fonts
// ---------------------------------------------------------------------------

pub const INTER: Font = Font::with_name("Inter");
pub const SEMIBOLD: Font = Font {
    weight: Weight::Semibold,
    ..Font::with_name("Inter")
};
pub const MONO: Font = Font::with_name("JetBrains Mono");
pub const ICONS: Font = Font::with_name("Phosphor");

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

pub fn theme() -> Theme {
    Theme::custom(
        "Kube UI",
        iced::theme::Palette {
            background: CONTENT,
            text: TEXT,
            primary: ACCENT,
            success: OK,
            warning: WARN,
            danger: BAD,
        },
    )
}

/// `color` at `factor` opacity.
pub fn alpha(color: Color, factor: f32) -> Color {
    color.scale_alpha(factor)
}

fn border(color: Color, radius: f32) -> Border {
    Border {
        color,
        width: 1.0,
        radius: radius.into(),
    }
}

fn shadow(y: f32, blur: f32, a: f32) -> Shadow {
    Shadow {
        color: Color::from_rgba(0.0, 0.0, 0.0, a),
        offset: Vector::new(0.0, y),
        blur_radius: blur,
    }
}

// ---------------------------------------------------------------------------
// Container styles
// ---------------------------------------------------------------------------

pub fn fill(color: Color) -> impl Fn(&Theme) -> container::Style {
    move |_| container::Style {
        background: Some(color.into()),
        text_color: Some(TEXT),
        ..Default::default()
    }
}

pub fn chrome(t: &Theme) -> container::Style {
    fill(CHROME)(t)
}

pub fn content(t: &Theme) -> container::Style {
    fill(CONTENT)(t)
}

pub fn raised(t: &Theme) -> container::Style {
    fill(RAISED)(t)
}

pub fn well(t: &Theme) -> container::Style {
    fill(WELL)(t)
}

/// Floating card (toasts, menus, dialogs): raised fill, hairline border, soft shadow.
pub fn card(_: &Theme) -> container::Style {
    container::Style {
        background: Some(RAISED.into()),
        text_color: Some(TEXT),
        border: border(BORDER_STRONG, 8.0),
        shadow: shadow(8.0, 22.0, 0.55),
        ..Default::default()
    }
}

pub fn dialog(_: &Theme) -> container::Style {
    container::Style {
        background: Some(RAISED.into()),
        text_color: Some(TEXT),
        border: border(BORDER_STRONG, 10.0),
        shadow: shadow(10.0, 28.0, 0.5),
        ..Default::default()
    }
}

/// A bordered box (container cards in the drawer, code blocks).
pub fn boxed(fill: Color, stroke: Color, radius: f32) -> impl Fn(&Theme) -> container::Style {
    move |_| container::Style {
        background: Some(fill.into()),
        text_color: Some(TEXT),
        border: border(stroke, radius),
        ..Default::default()
    }
}

pub fn tooltip_style(_: &Theme) -> container::Style {
    container::Style {
        background: Some(RAISED.into()),
        text_color: Some(TEXT),
        border: border(BORDER_STRONG, RADIUS_SMALL),
        shadow: shadow(4.0, 12.0, 0.45),
        ..Default::default()
    }
}

pub fn backdrop(_: &Theme) -> container::Style {
    container::Style {
        background: Some(Color::from_rgba(0.0, 0.0, 0.0, 0.45).into()),
        ..Default::default()
    }
}

// ---------------------------------------------------------------------------
// Button styles
// ---------------------------------------------------------------------------

fn button_base(bg: Option<Color>, fg: Color, stroke: Option<Color>, radius: f32) -> button::Style {
    button::Style {
        background: bg.map(Background::from),
        text_color: fg,
        border: match stroke {
            Some(c) => border(c, radius),
            None => Border {
                radius: radius.into(),
                ..Default::default()
            },
        },
        ..Default::default()
    }
}

/// The one filled, accent-coloured action in a group.
pub fn primary(_: &Theme, status: button::Status) -> button::Style {
    match status {
        button::Status::Hovered | button::Status::Pressed => button_base(
            Some(ACCENT_HOVER),
            Color::WHITE,
            Some(ACCENT_HOVER),
            RADIUS_SMALL,
        ),
        button::Status::Disabled => button_base(
            Some(alpha(ACCENT, 0.35)),
            alpha(Color::WHITE, 0.55),
            Some(alpha(ACCENT, 0.35)),
            RADIUS_SMALL,
        ),
        button::Status::Active => {
            button_base(Some(ACCENT), Color::WHITE, Some(ACCENT_HOVER), RADIUS_SMALL)
        }
    }
}

pub fn danger(_: &Theme, status: button::Status) -> button::Style {
    match status {
        button::Status::Hovered | button::Status::Pressed => {
            button_base(Some(BAD), Color::WHITE, Some(BAD), RADIUS_SMALL)
        }
        _ => button_base(Some(DANGER_FILL), Color::WHITE, Some(BAD), RADIUS_SMALL),
    }
}

/// Resting control: field fill, hairline border.
pub fn secondary(_: &Theme, status: button::Status) -> button::Style {
    match status {
        button::Status::Hovered => {
            button_base(Some(FIELD_HOVER), TEXT, Some(BORDER_STRONG), RADIUS_SMALL)
        }
        button::Status::Pressed => {
            button_base(Some(FIELD_ACTIVE), TEXT, Some(ACCENT), RADIUS_SMALL)
        }
        button::Status::Disabled => button_base(
            Some(alpha(FIELD, 0.6)),
            TEXT_FAINT,
            Some(BORDER),
            RADIUS_SMALL,
        ),
        button::Status::Active => button_base(Some(FIELD), TEXT, Some(BORDER), RADIUS_SMALL),
    }
}

/// Frameless; a fill only on hover.
pub fn ghost(_: &Theme, status: button::Status) -> button::Style {
    match status {
        button::Status::Hovered | button::Status::Pressed => {
            button_base(Some(FIELD_HOVER), TEXT, None, RADIUS_SMALL)
        }
        button::Status::Disabled => button_base(None, alpha(TEXT_MUTED, 0.5), None, RADIUS_SMALL),
        button::Status::Active => button_base(None, TEXT_MUTED, None, RADIUS_SMALL),
    }
}

/// [`ghost`] that stays lit (Selection Navy, accent hairline) while `on`.
pub fn toggled(on: bool) -> impl Fn(&Theme, button::Status) -> button::Style {
    move |t, status| {
        if on {
            button_base(Some(ACCENT_BG), ACCENT_TEXT, Some(ACCENT), RADIUS_SMALL)
        } else {
            ghost(t, status)
        }
    }
}

/// A row in a menu or a dropdown list; `selected` keeps it tinted.
pub fn menu_item(selected: bool, color: Color) -> impl Fn(&Theme, button::Status) -> button::Style {
    move |_, status| match status {
        button::Status::Hovered | button::Status::Pressed => button_base(
            Some(FIELD_HOVER),
            if selected { ACCENT_TEXT } else { color },
            None,
            RADIUS_SMALL,
        ),
        button::Status::Disabled => button_base(None, TEXT_FAINT, None, RADIUS_SMALL),
        button::Status::Active if selected => {
            button_base(Some(ACCENT_BG), ACCENT_TEXT, None, RADIUS_SMALL)
        }
        button::Status::Active => button_base(None, color, None, RADIUS_SMALL),
    }
}

/// Plain clickable text (links, tab titles).
pub fn link(color: Color) -> impl Fn(&Theme, button::Status) -> button::Style {
    move |_, status| {
        let c = match status {
            button::Status::Hovered | button::Status::Pressed => ACCENT_HOVER,
            _ => color,
        };
        button_base(None, c, None, 0.0)
    }
}

// ---------------------------------------------------------------------------
// Inputs, scrollbars, editor
// ---------------------------------------------------------------------------

pub fn input(_: &Theme, status: text_input::Status) -> text_input::Style {
    let stroke = match status {
        text_input::Status::Focused { .. } => ACCENT,
        text_input::Status::Hovered => BORDER_STRONG,
        _ => BORDER,
    };
    text_input::Style {
        background: CHROME.into(),
        border: border(stroke, RADIUS_SMALL),
        icon: TEXT_FAINT,
        placeholder: TEXT_FAINT,
        value: TEXT,
        selection: alpha(ACCENT, 0.4),
    }
}

pub fn scrollbar(_: &Theme, status: scrollable::Status) -> scrollable::Style {
    let (hovered_y, hovered_x) = match status {
        scrollable::Status::Hovered {
            is_vertical_scrollbar_hovered: y,
            is_horizontal_scrollbar_hovered: x,
            ..
        } => (y, x),
        scrollable::Status::Dragged {
            is_vertical_scrollbar_dragged: y,
            is_horizontal_scrollbar_dragged: x,
            ..
        } => (y, x),
        scrollable::Status::Active { .. } => (false, false),
    };
    let rail = |hot: bool| scrollable::Rail {
        background: None,
        border: Border::default(),
        scroller: scrollable::Scroller {
            background: (if hot { TEXT_FAINT } else { BORDER_STRONG }).into(),
            border: Border {
                radius: 3.0.into(),
                ..Default::default()
            },
        },
    };
    scrollable::Style {
        container: container::Style::default(),
        vertical_rail: rail(hovered_y),
        horizontal_rail: rail(hovered_x),
        gap: None,
        auto_scroll: scrollable::AutoScroll {
            background: RAISED.into(),
            border: border(BORDER_STRONG, 12.0),
            shadow: Shadow::default(),
            icon: TEXT,
        },
    }
}

/// A thin vertical scrollbar, as used throughout.
pub fn thin() -> scrollable::Direction {
    scrollable::Direction::Vertical(scrollable::Scrollbar::new().width(6).scroller_width(6))
}

pub fn thin_both() -> scrollable::Direction {
    let bar = scrollable::Scrollbar::new().width(6).scroller_width(6);
    scrollable::Direction::Both {
        vertical: bar,
        horizontal: bar,
    }
}

pub fn editor(_: &Theme, status: text_editor::Status) -> text_editor::Style {
    let _ = status;
    text_editor::Style {
        background: Color::TRANSPARENT.into(),
        border: Border::default(),
        placeholder: TEXT_FAINT,
        value: TEXT,
        selection: alpha(ACCENT, 0.4),
    }
}

// ---------------------------------------------------------------------------
// Text helpers
// ---------------------------------------------------------------------------

pub fn txt<'a>(s: impl text::IntoFragment<'a>) -> text::Text<'a> {
    text(s).size(BODY).color(TEXT)
}

pub fn muted<'a>(s: impl text::IntoFragment<'a>) -> text::Text<'a> {
    text(s).size(BODY).color(TEXT_MUTED)
}

pub fn faint<'a>(s: impl text::IntoFragment<'a>) -> text::Text<'a> {
    text(s).size(BODY).color(TEXT_FAINT)
}

pub fn small<'a>(s: impl text::IntoFragment<'a>) -> text::Text<'a> {
    text(s).size(SMALL).color(TEXT_MUTED)
}

pub fn semibold<'a>(s: impl text::IntoFragment<'a>) -> text::Text<'a> {
    text(s).size(BODY).font(SEMIBOLD).color(TEXT)
}

pub fn mono<'a>(s: impl text::IntoFragment<'a>) -> text::Text<'a> {
    text(s).size(MONO_SIZE).font(MONO).color(TEXT)
}

pub fn mono_muted<'a>(s: impl text::IntoFragment<'a>) -> text::Text<'a> {
    mono(s).color(TEXT_MUTED)
}

pub fn colored<'a>(s: impl text::IntoFragment<'a>, color: Color) -> text::Text<'a> {
    text(s).size(BODY).color(color)
}

pub fn title<'a>(s: impl text::IntoFragment<'a>) -> text::Text<'a> {
    text(s).size(19.0).font(SEMIBOLD).color(TEXT)
}

/// Small semibold muted label for table headers and panel sections.
pub fn label_small<'a>(s: impl text::IntoFragment<'a>) -> text::Text<'a> {
    text(s).size(12.0).font(SEMIBOLD).color(TEXT_MUTED)
}

pub fn icon<'a>(glyph: &'a str) -> text::Text<'a> {
    text(glyph)
        .font(ICONS)
        .size(14.0)
        .color(TEXT_MUTED)
        .line_height(1.0)
}

/// An icon and a label, as a button's content.
pub fn icon_label<'a, M: 'a>(glyph: &'a str, label: impl text::IntoFragment<'a>) -> Element<'a, M> {
    row![
        text(glyph).font(ICONS).size(14.0).line_height(1.0),
        text(label).size(BODY)
    ]
    .spacing(6)
    .align_y(iced::Center)
    .into()
}

pub fn space<'a, M: 'a>(px: f32) -> Element<'a, M> {
    Space::new().width(px).height(px).into()
}

pub fn vspace<'a, M: 'a>(px: f32) -> Element<'a, M> {
    Space::new().height(px).into()
}

pub fn hspace<'a, M: 'a>(px: f32) -> Element<'a, M> {
    Space::new().width(px).into()
}

pub fn fill_space<'a, M: 'a>() -> Element<'a, M> {
    Space::new().width(Length::Fill).into()
}

pub fn hline<'a, M: 'a>() -> Element<'a, M> {
    rule::horizontal(1)
        .style(|_| rule::Style {
            color: BORDER,
            radius: 0.0.into(),
            fill_mode: rule::FillMode::Full,
            snap: true,
        })
        .into()
}

pub fn vline<'a, M: 'a>(height: f32) -> Element<'a, M> {
    container(rule::vertical(1).style(|_| rule::Style {
        color: BORDER_STRONG,
        radius: 0.0.into(),
        fill_mode: rule::FillMode::Full,
        snap: true,
    }))
    .height(height)
    .into()
}

/// A filled status dot.
pub fn dot<'a, M: 'a>(color: Color) -> Element<'a, M> {
    container(Space::new().width(8).height(8))
        .style(move |_| container::Style {
            background: Some(color.into()),
            border: Border {
                radius: 4.0.into(),
                ..Default::default()
            },
            ..Default::default()
        })
        .into()
}

/// Wraps `content` in a hover tooltip.
pub fn tip<'a, M: 'a>(
    content: impl Into<Element<'a, M>>,
    hint: impl Into<String>,
) -> Element<'a, M> {
    let hint: String = hint.into();
    if hint.is_empty() {
        return content.into();
    }
    tooltip(
        content,
        container(text(hint).size(12.5).color(TEXT))
            .padding([5, 8])
            .max_width(420)
            .style(tooltip_style),
        tooltip::Position::Bottom,
    )
    .gap(4)
    .delay(std::time::Duration::from_millis(400))
    .into()
}

// ---------------------------------------------------------------------------
// Controls
// ---------------------------------------------------------------------------

const BUTTON_PAD: Padding = Padding {
    top: 5.0,
    bottom: 5.0,
    left: 10.0,
    right: 10.0,
};

const SMALL_PAD: Padding = Padding {
    top: 2.0,
    bottom: 2.0,
    left: 7.0,
    right: 7.0,
};

fn labelled<'a, M: 'a>(
    glyph: Option<&'a str>,
    label: String,
    font: Font,
    size: f32,
) -> Element<'a, M> {
    match glyph {
        Some(g) => row![
            text(g).font(ICONS).size(size + 0.5).line_height(1.0),
            text(label).size(size).font(font)
        ]
        .spacing(6)
        .align_y(iced::Center)
        .into(),
        None => text(label).size(size).font(font).into(),
    }
}

/// The one filled, accent-coloured action in a group. `None` disables it.
pub fn primary_button<'a, M: Clone + 'a>(
    glyph: Option<&'a str>,
    label: impl Into<String>,
    on_press: Option<M>,
) -> Element<'a, M> {
    button(labelled(glyph, label.into(), SEMIBOLD, BODY))
        .padding(BUTTON_PAD)
        .style(primary)
        .on_press_maybe(on_press)
        .into()
}

pub fn danger_button<'a, M: Clone + 'a>(
    glyph: Option<&'a str>,
    label: impl Into<String>,
    on_press: M,
) -> Element<'a, M> {
    button(labelled(glyph, label.into(), SEMIBOLD, BODY))
        .padding(BUTTON_PAD)
        .style(danger)
        .on_press(on_press)
        .into()
}

pub fn secondary_button<'a, M: Clone + 'a>(
    glyph: Option<&'a str>,
    label: impl Into<String>,
    on_press: Option<M>,
) -> Element<'a, M> {
    button(labelled(glyph, label.into(), INTER, BODY))
        .padding(BUTTON_PAD)
        .style(secondary)
        .on_press_maybe(on_press)
        .into()
}

pub fn small_button<'a, M: Clone + 'a>(
    glyph: Option<&'a str>,
    label: impl Into<String>,
    on_press: Option<M>,
) -> Element<'a, M> {
    button(labelled(glyph, label.into(), INTER, 12.5))
        .padding(SMALL_PAD)
        .style(secondary)
        .on_press_maybe(on_press)
        .into()
}

/// Frameless icon button that only shows a fill on hover.
pub fn icon_button<'a, M: Clone + 'a>(
    glyph: &'a str,
    hint: &str,
    on_press: Option<M>,
) -> Element<'a, M> {
    tip(
        button(container(text(glyph).font(ICONS).size(14.0).line_height(1.0)).center(Length::Fill))
            .width(24)
            .height(24)
            .padding(0)
            .style(ghost)
            .on_press_maybe(on_press),
        hint,
    )
}

/// [`icon_button`] that stays lit while `on`.
pub fn icon_toggle<'a, M: Clone + 'a>(
    glyph: &'a str,
    on: bool,
    hint: &str,
    on_press: M,
) -> Element<'a, M> {
    tip(
        button(container(text(glyph).font(ICONS).size(14.0).line_height(1.0)).center(Length::Fill))
            .width(24)
            .height(24)
            .padding(0)
            .style(toggled(on))
            .on_press(on_press),
        hint,
    )
}

/// Small on/off text toggle (e.g. `.*` for regex), sized to sit beside an icon button.
pub fn toggle_button<'a, M: Clone + 'a>(
    label: &'a str,
    on: bool,
    hint: &str,
    on_press: M,
) -> Element<'a, M> {
    tip(
        button(
            container(text(label).font(MONO).size(13.0).line_height(1.0)).center_y(Length::Fill),
        )
        .height(24)
        .padding([0, 6])
        .style(toggled(on))
        .on_press(on_press),
        hint,
    )
}

/// A small segmented control: one bordered strip, the chosen segment filled.
pub fn segmented<'a, M: Clone + 'a>(
    labels: &[&'a str],
    selected: usize,
    hint: &str,
    on_select: impl Fn(usize) -> M,
) -> Element<'a, M> {
    let segments = labels.iter().enumerate().map(|(i, label)| {
        let on = i == selected;
        button(
            container(
                text(*label)
                    .size(12.5)
                    .font(if on { SEMIBOLD } else { INTER })
                    .line_height(1.0),
            )
            .center_y(Length::Fill),
        )
        .height(20)
        .padding([0, 9])
        .style(move |_, status| {
            let fill = match (on, status) {
                (true, _) => Some(FIELD_ACTIVE),
                (false, button::Status::Hovered | button::Status::Pressed) => Some(FIELD_HOVER),
                _ => None,
            };
            let fg = if on || matches!(status, button::Status::Hovered) {
                TEXT
            } else {
                TEXT_MUTED
            };
            button_base(fill, fg, None, RADIUS_SMALL - 1.0)
        })
        .on_press_maybe((!on).then(|| on_select(i)))
        .into()
    });
    tip(
        container(row(segments))
            .padding(2)
            .style(boxed(CHROME, BORDER, RADIUS_SMALL)),
        hint,
    )
}

/// Search field with a leading magnifier, as used for list filters.
pub fn search_field<'a, M: Clone + 'a>(
    value: &str,
    hint: &str,
    width: f32,
    on_input: impl Fn(String) -> M + 'a,
) -> text_input::TextInput<'a, M> {
    text_input(hint, value)
        .on_input(on_input)
        .icon(text_input::Icon {
            font: ICONS,
            code_point: icon::MAGNIFYING_GLASS.chars().next().unwrap_or(' '),
            size: Some(13.0.into()),
            spacing: 6.0,
            side: text_input::Side::Left,
        })
        .size(BODY)
        .padding([5, 8])
        .width(width)
        .style(input)
}

/// Checkbox drawn in the app's vocabulary: a 15px square, accent-filled with a check when on.
pub fn checkbox<'a, M: Clone + 'a>(value: bool, label: &'a str, on_toggle: M) -> Element<'a, M> {
    let mark = container(if value {
        Element::from(
            text(icon::CHECK)
                .font(ICONS)
                .size(11.0)
                .color(Color::WHITE)
                .line_height(1.0),
        )
    } else {
        Space::new().into()
    })
    .center(15)
    .style(move |_| container::Style {
        background: Some((if value { ACCENT } else { FIELD }).into()),
        border: Border {
            color: if value { ACCENT } else { BORDER_STRONG },
            width: 1.0,
            radius: 3.0.into(),
        },
        ..Default::default()
    });
    let content: Element<'a, M> = if label.is_empty() {
        mark.into()
    } else {
        row![
            mark,
            text(label)
                .size(BODY)
                .color(if value { TEXT } else { TEXT_MUTED })
        ]
        .spacing(7)
        .align_y(iced::Center)
        .into()
    };
    mouse_area(container(content).center_y(22))
        .on_press(on_toggle)
        .interaction(iced::mouse::Interaction::Pointer)
        .into()
}

/// Small tinted pill, e.g. a kind label or a restart count.
pub fn badge<'a, M: 'a>(label: impl Into<String>, color: Color, filled: bool) -> Element<'a, M> {
    container(
        text(label.into())
            .size(SMALL)
            .font(SEMIBOLD)
            .color(if filled { Color::WHITE } else { color }),
    )
    .padding([1, 6])
    .style(move |_| container::Style {
        background: Some((if filled { color } else { alpha(color, 0.16) }).into()),
        border: Border {
            radius: RADIUS_SMALL.into(),
            ..Default::default()
        },
        ..Default::default()
    })
    .into()
}

/// A right-aligned nav count: text, and a colour for when the item isn't selected
/// (`None` = Faint Ink; a state colour only when the number reports state).
pub type NavBadge = Option<(String, Option<Color>)>;

fn nav_badge<'a, M: 'a>(badge: NavBadge, selected: bool) -> Element<'a, M> {
    match badge {
        Some((label, color)) => {
            let color = match (selected, color) {
                (_, Some(c)) => c,
                (true, None) => ACCENT_TEXT,
                (false, None) => TEXT_FAINT,
            };
            text(label).size(12.0).color(color).into()
        }
        None => Space::new().into(),
    }
}

fn nav_style(selected: bool) -> impl Fn(&Theme, button::Status) -> button::Style {
    move |_, status| {
        let (fill, fg) = if selected {
            (Some(ACCENT_BG), ACCENT_TEXT)
        } else {
            match status {
                button::Status::Hovered | button::Status::Pressed => (Some(FIELD), TEXT),
                _ => (None, TEXT_MUTED),
            }
        };
        button_base(fill, fg, None, RADIUS)
    }
}

/// Left navigation entry: icon, label, optional count; accent-tinted when selected.
pub fn nav_item<'a, M: Clone + 'a>(
    glyph: &'a str,
    label: &'a str,
    badge: NavBadge,
    selected: bool,
    on_press: M,
) -> Element<'a, M> {
    button(
        row![
            text(glyph).font(ICONS).size(16.0).line_height(1.0),
            text(label)
                .size(BODY)
                .font(if selected { SEMIBOLD } else { INTER }),
            fill_space(),
            nav_badge(badge, selected),
        ]
        .spacing(10)
        .height(Length::Fill)
        .align_y(iced::Center),
    )
    .width(Length::Fill)
    .height(32)
    .padding([0, 10])
    .style(nav_style(selected))
    .on_press(on_press)
    .into()
}

/// Collapsible sidebar group header: caret, glyph, label. `marked` highlights the
/// header when it's closed over the current page.
pub fn nav_group<'a, M: Clone + 'a>(
    glyph: &'a str,
    label: &'a str,
    open: bool,
    marked: bool,
    on_press: M,
) -> Element<'a, M> {
    button(
        row![
            text(if open {
                icon::CARET_DOWN
            } else {
                icon::CARET_RIGHT
            })
            .font(ICONS)
            .size(12.0)
            .color(TEXT_FAINT)
            .line_height(1.0),
            text(glyph).font(ICONS).size(16.0).line_height(1.0),
            text(label).size(BODY).font(SEMIBOLD),
        ]
        .spacing(8)
        .height(Length::Fill)
        .align_y(iced::Center),
    )
    .width(Length::Fill)
    .height(32)
    .padding([0, 6])
    .style(nav_style(marked))
    .on_press(on_press)
    .into()
}

/// A page inside a sidebar group: text only, hung off a hairline guide under the group glyph.
pub fn nav_child<'a, M: Clone + 'a>(
    label: &'a str,
    badge: NavBadge,
    selected: bool,
    on_press: M,
) -> Element<'a, M> {
    // One continuous hairline through every child row (each row includes the 2px gap
    // between items); the current page lights it up with an accent stroke on the same axis.
    let line = |width: f32, color: Color| {
        container(Space::new().width(width).height(Length::Fill)).style(fill(color))
    };
    let mut guide = stack![container(line(1.0, BORDER_STRONG)).center_x(Length::Fill)];
    if selected {
        guide = guide.push(
            container(line(2.0, ACCENT))
                .padding([7, 0])
                .center_x(Length::Fill),
        );
    }
    row![
        hspace(28.0),
        container(guide).width(4).height(Length::Fill),
        hspace(4.0),
        container(
            button(
                row![
                    text(label)
                        .size(BODY)
                        .font(if selected { SEMIBOLD } else { INTER }),
                    fill_space(),
                    nav_badge(badge, selected),
                ]
                .height(Length::Fill)
                .align_y(iced::Center),
            )
            .width(Length::Fill)
            .height(30)
            .padding([0, 10])
            .style(nav_style(selected))
            .on_press(on_press),
        )
        .padding([1, 0]),
    ]
    .height(32)
    .into()
}

/// Underlined tab strip (drawer sections).
pub fn underline_tabs<'a, M: Clone + 'a>(
    labels: Vec<String>,
    selected: usize,
    on_select: impl Fn(usize) -> M,
) -> Element<'a, M> {
    let tabs = labels.into_iter().enumerate().map(|(i, label)| {
        let active = i == selected;
        let underline = container(Space::new().height(2).width(Length::Fill)).style(move |_| {
            container::Style {
                background: active.then_some(ACCENT.into()),
                ..Default::default()
            }
        });
        column![
            button(
                text(label)
                    .size(BODY)
                    .font(if active { SEMIBOLD } else { INTER })
                    .color(if active { TEXT } else { TEXT_MUTED })
            )
            .padding([0, 0])
            .style(link(if active { TEXT } else { TEXT_MUTED }))
            .on_press(on_select(i)),
            vspace(6.0),
            underline,
        ]
        .width(Length::Shrink)
        .into()
    });
    column![row(tabs).spacing(18), hline(), vspace(10.0)].into()
}

/// The ground of a dropdown trigger: field fill, caret on the right.
pub fn dropdown_button<'a, M: Clone + 'a>(
    label: impl Into<String>,
    width: f32,
    open: bool,
    on_press: M,
) -> Element<'a, M> {
    button(
        row![
            text(label.into())
                .size(BODY)
                .wrapping(text::Wrapping::None)
                .width(Length::Fill),
            text(if open {
                icon::CARET_UP
            } else {
                icon::CARET_DOWN
            })
            .font(ICONS)
            .size(13.0)
            .color(TEXT_MUTED)
            .line_height(1.0),
        ]
        .spacing(6)
        .align_y(iced::Center),
    )
    .width(width)
    .padding(BUTTON_PAD)
    .style(move |t, status| {
        if open {
            button_base(Some(FIELD_HOVER), TEXT, Some(ACCENT), RADIUS_SMALL)
        } else {
            secondary(t, status)
        }
    })
    .on_press(on_press)
    .into()
}

/// A menu entry: optional icon, label, full width.
pub fn menu_entry<'a, M: Clone + 'a>(
    glyph: Option<&'a str>,
    label: impl Into<String>,
    color: Color,
    on_press: Option<M>,
) -> Element<'a, M> {
    button(labelled(glyph, label.into(), INTER, BODY))
        .width(Length::Fill)
        .padding([5, 8])
        .style(menu_item(false, color))
        .on_press_maybe(on_press)
        .into()
}

/// A selectable list row (dropdown options).
pub fn option_entry<'a, M: Clone + 'a>(
    label: impl Into<String>,
    selected: bool,
    on_press: M,
) -> Element<'a, M> {
    button(text(label.into()).size(BODY).wrapping(text::Wrapping::None))
        .width(Length::Fill)
        .padding([5, 8])
        .style(menu_item(selected, TEXT))
        .on_press(on_press)
        .into()
}
