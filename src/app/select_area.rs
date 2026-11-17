//! Turns presses and drags over a widget into text-selection events, leaving everything the
//! content handles itself (buttons, scrollbars) alone. Positions are relative to the
//! widget's top-left corner, and drags keep reporting outside it. Inside a scrollable, a
//! drag held past the visible edge asks for the view to scroll toward the cursor.

use std::time::{Duration, Instant};

use iced::advanced::widget::{Operation, Tree, tree};
use iced::advanced::{Clipboard, Layout, Shell, Widget, layout, overlay, renderer};
use iced::{Element, Event, Length, Point, Rectangle, Size, Vector, keyboard, mouse, window};

/// Two presses closer together than this (and a few pixels apart) count as one multi-click.
const MULTI_CLICK: Duration = Duration::from_millis(400);
/// A press has to travel this far to become a drag, so a click that wobbles by a pixel
/// doesn't select anything.
const DRAG_SLOP: f32 = 4.0;
/// Auto-scroll speed, in pixels per second per pixel the cursor is past the edge, and the
/// least speed once it is past at all.
const EDGE_GAIN: f32 = 14.0;
const EDGE_MIN: f32 = 120.0;
/// Overshoot past this is treated as this much, so speed tops out.
const EDGE_CAP: f32 = 160.0;

#[derive(Clone, Copy, Debug)]
pub enum SelectEvent {
    /// A press with its click count: 1, 2 (word) or 3 (line).
    Press(Point, u8),
    /// A shift-press: extend the selection to here.
    Extend(Point),
    Drag(Point),
    /// A drag is held past the visible edge: scroll the view by this much.
    Scroll(Vector),
}

pub struct SelectArea<'a, Message, Theme, Renderer> {
    content: Element<'a, Message, Theme, Renderer>,
    on_select: Box<dyn Fn(SelectEvent) -> Message + 'a>,
}

pub fn select_area<'a, Message, Theme, Renderer>(
    content: impl Into<Element<'a, Message, Theme, Renderer>>,
    on_select: impl Fn(SelectEvent) -> Message + 'a,
) -> SelectArea<'a, Message, Theme, Renderer> {
    SelectArea {
        content: content.into(),
        on_select: Box::new(on_select),
    }
}

#[derive(Default)]
struct State {
    /// Where the left button went down, while it is still down.
    pressed_at: Option<Point>,
    /// The cursor has travelled past `DRAG_SLOP` since then.
    dragging: bool,
    last_press: Option<(Instant, Point, u8)>,
    modifiers: keyboard::Modifiers,
    /// The last auto-scroll frame, while a drag is held past the edge.
    last_tick: Option<Instant>,
}

/// How far `p` is outside `viewport` on each axis (negative is up/left), zero inside.
fn overshoot(p: Point, viewport: &Rectangle) -> Vector {
    let axis = |v: f32, lo: f32, hi: f32| {
        if v < lo {
            v - lo
        } else if v > hi {
            v - hi
        } else {
            0.0
        }
    };
    Vector::new(
        axis(p.x, viewport.x, viewport.x + viewport.width),
        axis(p.y, viewport.y, viewport.y + viewport.height),
    )
}

fn edge_speed(over: f32) -> f32 {
    if over == 0.0 {
        0.0
    } else {
        over.signum() * (over.abs().min(EDGE_CAP) * EDGE_GAIN).max(EDGE_MIN)
    }
}

impl<Message, Theme, Renderer> Widget<Message, Theme, Renderer>
    for SelectArea<'_, Message, Theme, Renderer>
where
    Renderer: renderer::Renderer,
{
    fn tag(&self) -> tree::Tag {
        tree::Tag::of::<State>()
    }

    fn state(&self) -> tree::State {
        tree::State::new(State::default())
    }

    fn children(&self) -> Vec<Tree> {
        vec![Tree::new(&self.content)]
    }

    fn diff(&self, tree: &mut Tree) {
        tree.diff_children(std::slice::from_ref(&self.content));
    }

    fn size(&self) -> Size<Length> {
        self.content.as_widget().size()
    }

    fn layout(
        &mut self,
        tree: &mut Tree,
        renderer: &Renderer,
        limits: &layout::Limits,
    ) -> layout::Node {
        self.content
            .as_widget_mut()
            .layout(&mut tree.children[0], renderer, limits)
    }

    fn operate(
        &mut self,
        tree: &mut Tree,
        layout: Layout<'_>,
        renderer: &Renderer,
        operation: &mut dyn Operation,
    ) {
        self.content
            .as_widget_mut()
            .operate(&mut tree.children[0], layout, renderer, operation);
    }

    fn update(
        &mut self,
        tree: &mut Tree,
        event: &Event,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        renderer: &Renderer,
        clipboard: &mut dyn Clipboard,
        shell: &mut Shell<'_, Message>,
        viewport: &Rectangle,
    ) {
        self.content.as_widget_mut().update(
            &mut tree.children[0],
            event,
            layout,
            cursor,
            renderer,
            clipboard,
            shell,
            viewport,
        );
        let bounds = layout.bounds();
        let state = tree.state.downcast_mut::<State>();
        let rel = |p: Point| Point::new(p.x - bounds.x, p.y - bounds.y);
        match event {
            Event::Keyboard(keyboard::Event::ModifiersChanged(m)) => state.modifiers = *m,
            Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left)) => {
                if shell.is_event_captured() {
                    return;
                }
                let Some(p) = cursor.position_over(bounds) else {
                    return;
                };
                state.dragging = false;
                state.last_tick = None;
                if state.modifiers.shift() {
                    state.last_press = None;
                    state.pressed_at = Some(p);
                    // Already past the slop: moving on from here keeps extending.
                    state.dragging = true;
                    shell.publish((self.on_select)(SelectEvent::Extend(rel(p))));
                    shell.capture_event();
                    return;
                }
                let now = Instant::now();
                let count = match state.last_press {
                    Some((at, q, n)) if now - at < MULTI_CLICK && p.distance(q) < 4.0 => n % 3 + 1,
                    _ => 1,
                };
                state.last_press = Some((now, p, count));
                state.pressed_at = Some(p);
                shell.publish((self.on_select)(SelectEvent::Press(rel(p), count)));
                shell.capture_event();
            }
            Event::Mouse(mouse::Event::CursorMoved { .. }) => {
                let Some(from) = state.pressed_at else {
                    return;
                };
                // The cursor, not the event's position: inside a scrollable the cursor is
                // the one shifted into the content's own space, and the event's isn't. Off
                // the scrollable (or over its bars) it "levitates", which still counts here.
                let Some(position) = cursor.land().position() else {
                    return;
                };
                if !state.dragging {
                    if position.distance(from) < DRAG_SLOP {
                        return;
                    }
                    state.dragging = true;
                }
                shell.publish((self.on_select)(SelectEvent::Drag(rel(position))));
                if overshoot(position, viewport) != Vector::ZERO {
                    shell.request_redraw();
                }
            }
            Event::Window(window::Event::RedrawRequested(now)) => {
                if !state.dragging || state.pressed_at.is_none() {
                    state.last_tick = None;
                    return;
                }
                let Some(position) = cursor.land().position() else {
                    return;
                };
                let over = overshoot(position, viewport);
                if over == Vector::ZERO {
                    state.last_tick = None;
                    return;
                }
                // Frame-rate independent; the first frame past the edge only starts the clock.
                let dt = state
                    .last_tick
                    .map_or(0.0, |t| now.saturating_duration_since(t).as_secs_f32().min(0.1));
                state.last_tick = Some(*now);
                let step = Vector::new(edge_speed(over.x) * dt, edge_speed(over.y) * dt);
                if step != Vector::ZERO {
                    shell.publish((self.on_select)(SelectEvent::Scroll(step)));
                    // The cursor stays put on screen, so the content under it moves on.
                    shell.publish((self.on_select)(SelectEvent::Drag(rel(position + step))));
                }
                shell.request_redraw();
            }
            Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left)) => {
                state.pressed_at = None;
                state.dragging = false;
                state.last_tick = None;
            }
            _ => {}
        }
    }

    fn mouse_interaction(
        &self,
        tree: &Tree,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        viewport: &Rectangle,
        renderer: &Renderer,
    ) -> mouse::Interaction {
        let inner = self.content.as_widget().mouse_interaction(
            &tree.children[0],
            layout,
            cursor,
            viewport,
            renderer,
        );
        match inner {
            mouse::Interaction::None | mouse::Interaction::Idle
                if cursor.is_over(layout.bounds()) =>
            {
                mouse::Interaction::Text
            }
            other => other,
        }
    }

    fn draw(
        &self,
        tree: &Tree,
        renderer: &mut Renderer,
        theme: &Theme,
        style: &renderer::Style,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        viewport: &Rectangle,
    ) {
        self.content.as_widget().draw(
            &tree.children[0],
            renderer,
            theme,
            style,
            layout,
            cursor,
            viewport,
        );
    }

    fn overlay<'b>(
        &'b mut self,
        tree: &'b mut Tree,
        layout: Layout<'b>,
        renderer: &Renderer,
        viewport: &Rectangle,
        translation: Vector,
    ) -> Option<overlay::Element<'b, Message, Theme, Renderer>> {
        self.content.as_widget_mut().overlay(
            &mut tree.children[0],
            layout,
            renderer,
            viewport,
            translation,
        )
    }
}

impl<'a, Message, Theme, Renderer> From<SelectArea<'a, Message, Theme, Renderer>>
    for Element<'a, Message, Theme, Renderer>
where
    Message: 'a,
    Theme: 'a,
    Renderer: renderer::Renderer + 'a,
{
    fn from(w: SelectArea<'a, Message, Theme, Renderer>) -> Self {
        Element::new(w)
    }
}
