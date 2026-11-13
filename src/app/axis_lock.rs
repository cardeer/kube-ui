//! Locks wheel and trackpad scrolling to one axis at a time, as macOS text views do: a
//! gesture scrolls along the axis it starts on, so a slightly diagonal swipe doesn't drift
//! sideways (or down) while reading. A clearly perpendicular movement takes over at once,
//! so changing direction (even during momentum scrolling) has no delay.

use std::time::{Duration, Instant};

use iced::advanced::widget::{Operation, Tree, tree};
use iced::advanced::{Clipboard, Layout, Shell, Widget, layout, overlay, renderer};
use iced::mouse::{self, ScrollDelta};
use iced::{Element, Event, Length, Rectangle, Size, Vector};

/// A pause this long between scroll events ends the gesture; the next one picks its axis anew.
const GESTURE_GAP: Duration = Duration::from_millis(120);
/// Mid-gesture, the other axis takes over when it moves this many times more than the
/// locked one (and by more than a jitter's worth).
const SWITCH_RATIO: f32 = 2.0;

pub struct AxisLock<'a, Message, Theme, Renderer> {
    content: Element<'a, Message, Theme, Renderer>,
}

/// Wraps a scrollable (or anything holding one) so each scroll gesture moves one axis.
pub fn axis_lock<'a, Message, Theme, Renderer>(
    content: impl Into<Element<'a, Message, Theme, Renderer>>,
) -> AxisLock<'a, Message, Theme, Renderer> {
    AxisLock {
        content: content.into(),
    }
}

#[derive(Clone, Copy, PartialEq)]
enum Axis {
    Horizontal,
    Vertical,
}

#[derive(Default)]
struct State {
    /// The current gesture's axis, and when it last scrolled.
    locked: Option<(Axis, Instant)>,
}

impl State {
    /// The delta with the other axis zeroed.
    fn lock(&mut self, delta: ScrollDelta) -> ScrollDelta {
        let (x, y) = match delta {
            ScrollDelta::Lines { x, y } | ScrollDelta::Pixels { x, y } => (x, y),
        };
        if x == 0.0 && y == 0.0 {
            return delta;
        }
        let jitter = match delta {
            ScrollDelta::Lines { .. } => 0.3,
            ScrollDelta::Pixels { .. } => 3.0,
        };
        let now = Instant::now();
        let dominant = if x.abs() > y.abs() {
            Axis::Horizontal
        } else {
            Axis::Vertical
        };
        let axis = match self.locked {
            Some((axis, at)) if now - at < GESTURE_GAP => {
                let (along, across) = match axis {
                    Axis::Horizontal => (x.abs(), y.abs()),
                    Axis::Vertical => (y.abs(), x.abs()),
                };
                if across > jitter && across > along * SWITCH_RATIO {
                    dominant
                } else {
                    axis
                }
            }
            _ => dominant,
        };
        self.locked = Some((axis, now));
        let (x, y) = match axis {
            Axis::Horizontal => (x, 0.0),
            Axis::Vertical => (0.0, y),
        };
        match delta {
            ScrollDelta::Lines { .. } => ScrollDelta::Lines { x, y },
            ScrollDelta::Pixels { .. } => ScrollDelta::Pixels { x, y },
        }
    }
}

impl<Message, Theme, Renderer> Widget<Message, Theme, Renderer>
    for AxisLock<'_, Message, Theme, Renderer>
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
        let locked;
        let event = match event {
            Event::Mouse(mouse::Event::WheelScrolled { delta })
                if cursor.is_over(layout.bounds()) =>
            {
                let delta = tree.state.downcast_mut::<State>().lock(*delta);
                locked = Event::Mouse(mouse::Event::WheelScrolled { delta });
                &locked
            }
            _ => event,
        };
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
    }

    fn mouse_interaction(
        &self,
        tree: &Tree,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        viewport: &Rectangle,
        renderer: &Renderer,
    ) -> mouse::Interaction {
        self.content.as_widget().mouse_interaction(
            &tree.children[0],
            layout,
            cursor,
            viewport,
            renderer,
        )
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

impl<'a, Message, Theme, Renderer> From<AxisLock<'a, Message, Theme, Renderer>>
    for Element<'a, Message, Theme, Renderer>
where
    Message: 'a,
    Theme: 'a,
    Renderer: renderer::Renderer + 'a,
{
    fn from(w: AxisLock<'a, Message, Theme, Renderer>) -> Self {
        Element::new(w)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_gesture_keeps_its_first_axis() {
        let mut s = State::default();
        let px = |x, y| ScrollDelta::Pixels { x, y };
        assert_eq!(s.lock(px(1.0, 8.0)), px(0.0, 8.0));
        // Drift sideways stays vertical.
        assert_eq!(s.lock(px(5.0, 4.0)), px(0.0, 4.0));
        assert_eq!(s.lock(px(2.0, 0.5)), px(0.0, 0.5));
        // A clearly sideways move takes over right away.
        assert_eq!(s.lock(px(9.0, 2.0)), px(9.0, 0.0));
        assert_eq!(s.lock(px(4.0, 5.0)), px(4.0, 0.0));
        // After a pause a new gesture picks its own axis.
        s.locked = s.locked.map(|(a, at)| (a, at - GESTURE_GAP));
        assert_eq!(s.lock(px(4.0, 5.0)), px(0.0, 5.0));
    }
}
