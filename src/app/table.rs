//! Resource tables: a sortable header and rows drawn only where they're visible, so lists
//! of thousands of pods stay quick. Wide tables scroll sideways instead of clipping columns.

use iced::{
    Element, Length, Size,
    widget::{Space, column, container, mouse_area, responsive, row, scrollable, text},
};

use super::{Message, ROW_HEIGHT, Sort, icons as icon, theme};

const HEADER_HEIGHT: f32 = 26.0;
const CELL_PAD: f32 = 8.0;

pub enum Width {
    Fixed(f32),
    /// Takes the room left over, but never less than this.
    Fill(f32),
}

pub struct Col {
    pub title: String,
    pub width: Width,
}

impl Col {
    pub fn fixed(title: impl Into<String>, width: f32) -> Self {
        Self {
            title: title.into(),
            width: Width::Fixed(width),
        }
    }

    pub fn fill(title: impl Into<String>, min: f32) -> Self {
        Self {
            title: title.into(),
            width: Width::Fill(min),
        }
    }
}

/// One row's cells, how it looks and what clicking it does.
pub struct Row<'a> {
    pub cells: Vec<Element<'a, Message>>,
    pub selected: bool,
    pub on_click: Message,
    pub on_double_click: Option<Message>,
}

pub struct Table<'a, F> {
    pub id: &'static str,
    pub cols: Vec<Col>,
    pub sort: Option<&'a Sort>,
    /// The page whose sort a header click changes.
    pub tab: super::Tab,
    pub len: usize,
    /// The body's scroll offset from the last scroll event.
    pub scroll: f32,
    pub hovered: Option<usize>,
    /// Builds row `i`, given each column's width.
    pub row: F,
}

impl<'a, F> Table<'a, F>
where
    F: Fn(usize, &[f32]) -> Row<'a> + 'a,
{
    pub fn view(self) -> Element<'a, Message> {
        responsive(move |size: Size| self.layout(size)).into()
    }

    fn layout(&self, size: Size) -> Element<'a, Message> {
        let fixed: f32 = self
            .cols
            .iter()
            .map(|c| match c.width {
                Width::Fixed(w) => w,
                Width::Fill(_) => 0.0,
            })
            .sum();
        let fill_min: f32 = self
            .cols
            .iter()
            .map(|c| match c.width {
                Width::Fill(m) => m,
                Width::Fixed(_) => 0.0,
            })
            .sum();
        let fills = self
            .cols
            .iter()
            .filter(|c| matches!(c.width, Width::Fill(_)))
            .count()
            .max(1) as f32;
        let total = size.width.max(fixed + fill_min).floor();
        let spare = (total - fixed - fill_min).max(0.0) / fills;
        let widths: Vec<f32> = self
            .cols
            .iter()
            .map(|c| match c.width {
                Width::Fixed(w) => w,
                Width::Fill(m) => m + spare,
            })
            .collect();
        let wide = total > size.width + 0.5;

        let header = row(self
            .cols
            .iter()
            .zip(&widths)
            .map(|(c, w)| sort_header(&c.title, self.sort, self.tab, *w)))
        .height(HEADER_HEIGHT)
        .width(total);

        // Leave room for the sideways scrollbar when there is one.
        let body_h = (size.height - HEADER_HEIGHT - if wide { 8.0 } else { 0.0 }).max(ROW_HEIGHT);
        let first = ((self.scroll / ROW_HEIGHT).floor().max(0.0) as usize).min(self.len);
        let visible = (body_h / ROW_HEIGHT).ceil() as usize + 2;
        let last = (first + visible).min(self.len);
        let mut rows = column![Space::new().height(first as f32 * ROW_HEIGHT)];
        for i in first..last {
            let r = (self.row)(i, &widths);
            let selected = r.selected;
            let hovered = self.hovered == Some(i);
            let stripe = i % 2 == 1;
            let cells = row(r.cells.into_iter().zip(&widths).map(|(cell, w)| {
                container(cell)
                    .width(*w)
                    .height(ROW_HEIGHT)
                    .padding([0.0, CELL_PAD])
                    .center_y(ROW_HEIGHT)
                    .clip(true)
                    .into()
            }));
            let ground = container(cells)
                .width(total)
                .height(ROW_HEIGHT)
                .style(move |_| {
                    let fill = if selected {
                        Some(theme::ACCENT_BG)
                    } else if hovered {
                        Some(theme::FIELD)
                    } else if stripe {
                        Some(theme::STRIPE)
                    } else {
                        None
                    };
                    container::Style {
                        background: fill.map(Into::into),
                        border: iced::Border {
                            radius: 4.0.into(),
                            ..Default::default()
                        },
                        ..Default::default()
                    }
                });
            let mut area = mouse_area(ground)
                .on_press(r.on_click)
                .on_enter(Message::RowHover(Some(i)))
                .on_exit(Message::RowLeave(i))
                .interaction(iced::mouse::Interaction::Pointer);
            if let Some(m) = r.on_double_click {
                area = area.on_double_click(m);
            }
            rows = rows.push(area);
        }
        rows = rows.push(Space::new().height((self.len - last) as f32 * ROW_HEIGHT));

        let body = scrollable(rows.width(total))
            .id(self.id)
            .direction(theme::thin())
            .on_scroll(|v| Message::ListScrolled(v.absolute_offset().y))
            .style(theme::scrollbar)
            .width(total)
            .height(body_h);
        let table = column![header, body].width(total);
        if wide {
            scrollable(table)
                .direction(scrollable::Direction::Horizontal(
                    scrollable::Scrollbar::new().width(6).scroller_width(6),
                ))
                .style(theme::scrollbar)
                .width(Length::Fill)
                .height(Length::Fill)
                .into()
        } else {
            container(table)
                .width(Length::Fill)
                .height(Length::Fill)
                .into()
        }
    }
}

/// A clickable column header: click to sort ascending, again for descending, again to clear.
fn sort_header<'a>(
    label: &str,
    sort: Option<&Sort>,
    tab: super::Tab,
    width: f32,
) -> Element<'a, Message> {
    let active = sort.filter(|s| s.col == label);
    let color = if active.is_some() {
        theme::TEXT
    } else {
        theme::TEXT_MUTED
    };
    let mut content = row![theme::label_small(label.to_string()).color(color)]
        .spacing(4)
        .align_y(iced::Center);
    if let Some(s) = active {
        content = content.push(
            text(if s.asc {
                icon::CARET_UP
            } else {
                icon::CARET_DOWN
            })
            .font(theme::ICONS)
            .size(11.0)
            .color(color),
        );
    }
    let hint = match active {
        Some(s) if s.asc => format!("Sorted by {label}, ascending. Click for descending."),
        Some(_) => format!("Sorted by {label}, descending. Click to clear."),
        None => format!("Sort by {label}"),
    };
    let cell: Element<'a, Message> = if label.is_empty() {
        Space::new().into()
    } else {
        theme::tip(
            mouse_area(content)
                .on_press(Message::Sort(tab, label.to_string()))
                .interaction(iced::mouse::Interaction::Pointer),
            hint,
        )
    };
    container(cell)
        .width(width)
        .height(HEADER_HEIGHT)
        .padding([0.0, CELL_PAD])
        .center_y(HEADER_HEIGHT)
        .clip(true)
        .into()
}

/// A resource name that fits its cell: Kubernetes names differ at the end (the ReplicaSet
/// hash and pod suffix), so shorten from the middle and keep the tail. Full name on hover.
pub fn name_cell<'a>(name: &str, width: f32) -> Element<'a, Message> {
    let fits = ((width - 2.0 * CELL_PAD - 4.0) / (theme::MONO_SIZE * 0.6))
        .floor()
        .max(8.0) as usize;
    let chars: Vec<char> = name.chars().collect();
    if chars.len() <= fits {
        return theme::mono(name.to_string()).into();
    }
    let tail = (fits / 3).clamp(5, 16);
    let head = fits.saturating_sub(tail + 1);
    let short: String = chars[..head]
        .iter()
        .chain(['…'].iter())
        .chain(chars[chars.len() - tail..].iter())
        .collect();
    theme::tip(theme::mono(short), name.to_string())
}

/// Text cut to its cell with an ellipsis (full text on hover).
pub fn truncated<'a>(
    s: &str,
    width: f32,
    char_w: f32,
    make: impl Fn(String) -> text::Text<'a>,
) -> Element<'a, Message> {
    let fits = ((width - 2.0 * CELL_PAD) / char_w).floor().max(4.0) as usize;
    if s.chars().count() <= fits {
        return make(s.to_string()).wrapping(text::Wrapping::None).into();
    }
    let short: String = s
        .chars()
        .take(fits.saturating_sub(1))
        .chain(['…'])
        .collect();
    theme::tip(make(short).wrapping(text::Wrapping::None), s.to_string())
}
