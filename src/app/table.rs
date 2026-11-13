//! Resource tables: a sortable header and rows drawn only where they're visible, so lists
//! of thousands of pods stay quick. Wide tables scroll sideways instead of clipping columns.
//!
//! Views are plain functions, so the body is virtualised by hand: the scroll handles live in
//! the workspace (see `Retained`), and each frame renders the rows the last frame's scroll
//! offset and viewport make visible, between two spacers that stand in for the rest.

use std::collections::BTreeMap;

use gpui_kit::component::scroll::ScrollableElement as _;
use gpui_kit::component::{h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, ElementId, InteractiveElement as _, IntoElement as _, MouseButton,
    ParentElement as _, ScrollHandle, StatefulInteractiveElement as _, Styled as _, div, px,
};

use super::{Drag, Message, ROW_HEIGHT, Sort, icons as icon, on, send, theme};

const HEADER_HEIGHT: f32 = 30.0;
const CELL_PAD: f32 = 12.0;
/// The grab area at the right edge of each header cell.
const HANDLE_W: f32 = 7.0;
pub const MIN_COL_WIDTH: f32 = 40.0;

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

/// The scroll handles of one table: the body (vertical) and the whole table (sideways).
#[derive(Clone, Default)]
pub struct TableScroll {
    pub vertical: ScrollHandle,
    pub horizontal: ScrollHandle,
}

/// One row's cells, how it looks and what clicking it does.
pub struct Row {
    /// Identifies the row (derived from the resource, never the index).
    pub key: String,
    pub cells: Vec<AnyElement>,
    pub selected: bool,
    pub on_click: Message,
    pub on_double_click: Option<Message>,
}

pub struct Table<'a, F> {
    pub id: &'static str,
    pub cols: Vec<Col>,
    pub sort: Option<&'a Sort>,
    /// The page whose sort a header click changes; also keys its saved column widths.
    pub tab: super::Tab,
    /// Headers sort the rows when clicked.
    pub sortable: bool,
    /// Column widths the user dragged, keyed `"<page>/<column>"`.
    pub widths: &'a BTreeMap<String, f32>,
    pub len: usize,
    pub scroll: &'a TableScroll,
    /// Builds row `i`, given each column's width.
    pub row: F,
}

impl<F> Table<'_, F>
where
    F: Fn(usize, &[f32]) -> Row,
{
    pub fn view(self) -> AnyElement {
        // The viewport as of the last frame (zero on the very first one).
        let viewport = self.scroll.horizontal.bounds().size;
        let (view_w, view_h) = (f32::from(viewport.width), f32::from(self.scroll.vertical.bounds().size.height));

        // A column the user resized keeps that width.
        let cols: Vec<Width> = self
            .cols
            .iter()
            .map(|c| match self.widths.get(&width_key(self.tab, &c.title)) {
                Some(w) => Width::Fixed(*w),
                None => match c.width {
                    Width::Fixed(w) => Width::Fixed(w),
                    Width::Fill(m) => Width::Fill(m),
                },
            })
            .collect();
        let fixed: f32 = cols
            .iter()
            .map(|c| match c {
                Width::Fixed(w) => *w,
                Width::Fill(_) => 0.0,
            })
            .sum();
        let fill_min: f32 = cols
            .iter()
            .map(|c| match c {
                Width::Fill(m) => *m,
                Width::Fixed(_) => 0.0,
            })
            .sum();
        let fills = cols
            .iter()
            .filter(|c| matches!(c, Width::Fill(_)))
            .count()
            .max(1) as f32;
        let total = view_w.max(fixed + fill_min).floor();
        let spare = (total - fixed - fill_min).max(0.0) / fills;
        let widths: Vec<f32> = cols
            .iter()
            .map(|c| match c {
                Width::Fixed(w) => *w,
                Width::Fill(m) => m + spare,
            })
            .collect();
        let wide = total > view_w + 0.5 && view_w > 0.0;

        let header = h_flex()
            .w(px(total))
            .h(px(HEADER_HEIGHT))
            .flex_none()
            .children(
                self.cols
                    .iter()
                    .zip(&widths)
                    .map(|(c, w)| self.header_cell(&c.title, *w)),
            );

        let body_h = if view_h > 0.0 { view_h } else { 1200.0 };
        // A shorter list (filtered, another page) can leave the old offset past the end,
        // which would show nothing until the next scroll: pull it back.
        let max_offset = (self.len as f32 * ROW_HEIGHT - body_h).max(0.0);
        let mut offset = -f32::from(self.scroll.vertical.offset().y);
        if offset > max_offset + 0.5 {
            offset = max_offset;
            let mut o = self.scroll.vertical.offset();
            o.y = px(-max_offset);
            self.scroll.vertical.set_offset(o);
        }
        // The offset is last frame's, and a fast scroll moves further than a few rows
        // per frame: render a whole viewport of rows above and below so none show blank.
        let visible = (body_h / ROW_HEIGHT).ceil() as usize;
        let top = (offset / ROW_HEIGHT).floor().max(0.0) as usize;
        let first = top.saturating_sub(visible).min(self.len);
        let last = (top + 2 * visible + 2).min(self.len);

        let mut rows = v_flex()
            .w(px(total))
            .flex_none()
            .child(div().h(px(first as f32 * ROW_HEIGHT)).flex_none());
        for i in first..last {
            rows = rows.child(self.render_row(i, &widths, total));
        }
        rows = rows.child(div().h(px((self.len - last) as f32 * ROW_HEIGHT)).flex_none());

        let body = div()
            .id(ElementId::Name(format!("{}/body", self.id).into()))
            .size_full()
            .overflow_y_scroll()
            .track_scroll(&self.scroll.vertical)
            .child(rows);
        // The vertical scrollbar sits on the viewport edge, not the edge of a wide table.
        let body = div()
            .relative()
            .flex_1()
            .min_h_0()
            .w(px(total))
            .child(body);

        let table = v_flex().w(px(total)).h_full().child(header).child(body);
        let scroller = div()
            .id(ElementId::Name(format!("{}/scroll", self.id).into()))
            .size_full()
            .overflow_x_scroll()
            .track_scroll(&self.scroll.horizontal)
            .child(table);
        div()
            .relative()
            .flex_1()
            .min_h_0()
            .w_full()
            .child(scroller)
            .vertical_scrollbar(&self.scroll.vertical)
            .when(wide, |d| d.horizontal_scrollbar(&self.scroll.horizontal))
            .into_any_element()
    }

    fn render_row(&self, i: usize, widths: &[f32], total: f32) -> AnyElement {
        let r = (self.row)(i, widths);
        let (selected, stripe) = (r.selected, i % 2 == 1);
        let (click, double) = (r.on_click, r.on_double_click);
        let cells = r.cells.into_iter().zip(widths).map(|(cell, w)| {
            h_flex()
                .w(px(*w))
                .h(px(ROW_HEIGHT))
                .px(px(CELL_PAD))
                .flex_none()
                .overflow_hidden()
                .child(cell)
        });
        h_flex()
            .id(ElementId::Name(format!("{}/row/{}", self.id, r.key).into()))
            .w(px(total))
            .h(px(ROW_HEIGHT))
            .flex_none()
            .rounded(px(4.0))
            .cursor_pointer()
            .when(selected, |d| d.bg(theme::ACCENT_BG))
            .when(!selected, |d| {
                d.when(stripe, |d| d.bg(theme::STRIPE))
                    .hover(|s| s.bg(theme::FIELD))
            })
            .on_click(move |ev, window, cx| {
                // A double click already selected the row with its first click.
                match (&double, ev.click_count()) {
                    (Some(m), 2) => send(m.clone(), window, cx),
                    _ => send(click.clone(), window, cx),
                }
            })
            .children(cells)
            .into_any_element()
    }

    /// A column header: click to sort ascending, again for descending, again to clear.
    /// Its right edge drags to resize the column; double-click it to reset.
    fn header_cell(&self, label: &str, width: f32) -> AnyElement {
        let active = self.sort.filter(|s| s.col == label);
        let color = if active.is_some() {
            theme::TEXT
        } else {
            theme::TEXT_MUTED
        };
        let content = h_flex()
            .gap(px(4.0))
            .child(theme::label_small(label.to_string()).text_color(color))
            .when_some(active, |d, s| {
                d.child(
                    theme::icon(if s.asc {
                        icon::CARET_UP
                    } else {
                        icon::CARET_DOWN
                    })
                    .text_size(px(11.0))
                    .text_color(color),
                )
            });
        let id = format!("{}/hdr/{label}", self.id);
        let title = div()
            .id(ElementId::Name(id.clone().into()))
            .flex_1()
            .min_w_0()
            .h_full()
            .px(px(CELL_PAD))
            .flex()
            .items_center()
            .overflow_hidden()
            .child(content);
        let title = if label.is_empty() || !self.sortable {
            title.into_any_element()
        } else {
            let hint = match active {
                Some(s) if s.asc => format!("Sorted by {label}, ascending. Click for descending."),
                Some(_) => format!("Sorted by {label}, descending. Click to clear."),
                None => format!("Sort by {label}"),
            };
            theme::tip(
                title
                    .cursor_pointer()
                    .on_click(on(Message::Sort(self.tab, label.to_string()))),
                hint,
            )
            .into_any_element()
        };
        let key = width_key(self.tab, label);
        let reset = key.clone();
        let handle = theme::tip(
            div()
                .id(ElementId::Name(format!("{id}/grip").into()))
                .w(px(HANDLE_W))
                .h_full()
                .flex_none()
                .flex()
                .items_center()
                .justify_center()
                .cursor_col_resize()
                .child(theme::vline(14.0))
                .on_mouse_down(MouseButton::Left, move |_, window, cx| {
                    send(
                        Message::DragStart(Drag::Column {
                            key: key.clone(),
                            start: width,
                        }),
                        window,
                        cx,
                    )
                })
                .on_click(move |ev, window, cx| {
                    if ev.click_count() >= 2 {
                        send(Message::ResetColumn(reset.clone()), window, cx)
                    }
                }),
            "Drag to resize · double-click to reset",
        );
        h_flex()
            .w(px(width))
            .h(px(HEADER_HEIGHT))
            .flex_none()
            .child(title)
            .child(handle)
            .into_any_element()
    }
}

/// Where a column's dragged width is saved.
pub fn width_key(tab: super::Tab, col: &str) -> String {
    format!("{}/{col}", tab.key())
}

/// A resource name that fits its cell: Kubernetes names differ at the end (the ReplicaSet
/// hash and pod suffix), so shorten from the middle and keep the tail. Full name on hover.
pub fn name_cell(name: &str) -> AnyElement {
    theme::tip(
        theme::mono(name.to_string())
            .id(ElementId::Name(format!("name/{name}").into()))
            .flex_1()
            .min_w_0()
            .overflow_hidden()
            .text_ellipsis_middle(),
        name.to_string(),
    )
    .into_any_element()
}

/// Text cut to its cell with an ellipsis (full text on hover).
pub fn truncated(s: &str, make: impl Fn(String) -> gpui_kit::Div) -> AnyElement {
    let el = make(s.to_string())
        .flex_1()
        .min_w_0()
        .overflow_hidden()
        .text_ellipsis();
    if s.chars().count() > 10 {
        theme::tip(
            el.id(ElementId::Name(format!("cell/{s}").into())),
            s.to_string(),
        )
        .into_any_element()
    } else {
        el.into_any_element()
    }
}
