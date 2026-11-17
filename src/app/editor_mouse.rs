//! Mouse selection the text editor leaves out: shift-click extends the selection, and a
//! drag after a double or triple click keeps selecting by word or line. iced's editor
//! only drags after a single click, though its engine extends a word or line selection
//! itself once it is sent drags. Wraps the editor element directly, so positions match
//! the editor's own (relative to its text area, inside the padding).

use iced::advanced::widget::{Operation, Tree, tree};
use iced::advanced::{Clipboard, Layout, Shell, Widget, layout, overlay, renderer};
use iced::advanced::mouse::{Click, click};
use iced::{Element, Event, Length, Point, Rectangle, Size, Vector, keyboard, mouse};

#[derive(Clone, Copy, Debug)]
pub enum EditorMouse {
    /// A shift-press: extend the selection to here.
    Extend(Point),
    /// The pointer moved while held after a shift, double or triple click.
    Drag(Point),
}

pub struct EditorMouseArea<'a, Message, Theme, Renderer> {
    content: Element<'a, Message, Theme, Renderer>,
    padding: Vector,
    on_event: Box<dyn Fn(EditorMouse) -> Message + 'a>,
}

/// `padding` is the editor's (left, top) text padding.
pub fn editor_mouse<'a, Message, Theme, Renderer>(
    content: impl Into<Element<'a, Message, Theme, Renderer>>,
    padding: Vector,
    on_event: impl Fn(EditorMouse) -> Message + 'a,
) -> EditorMouseArea<'a, Message, Theme, Renderer> {
    EditorMouseArea {
        content: content.into(),
        padding,
        on_event: Box::new(on_event),
    }
}

#[derive(Default)]
struct State {
    modifiers: keyboard::Modifiers,
    /// Mirrors the editor's own click counting, so both agree on double and triple clicks.
    last_click: Option<Click>,
    /// The button is held after a press the editor won't drag from.
    dragging: bool,
}

impl<Message, Theme, Renderer> Widget<Message, Theme, Renderer>
    for EditorMouseArea<'_, Message, Theme, Renderer>
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
        let bounds = layout.bounds();
        let to_text = |p: Point| Point::new(p.x - bounds.x, p.y - bounds.y) - self.padding;
        let state = tree.state.downcast_mut::<State>();
        match event {
            Event::Keyboard(keyboard::Event::ModifiersChanged(m)) => state.modifiers = *m,
            // Shift-press: the editor would move the caret and drop the selection, so it
            // never sees this press.
            Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left))
                if state.modifiers.shift() && !shell.is_event_captured() =>
            {
                if let Some(p) = cursor.position_over(bounds) {
                    state.last_click = None;
                    state.dragging = true;
                    shell.publish((self.on_event)(EditorMouse::Extend(to_text(p))));
                    shell.capture_event();
                    return;
                }
            }
            _ => {}
        }

        let captured_before = shell.is_event_captured();
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
        let state = tree.state.downcast_mut::<State>();
        match event {
            Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left)) if !captured_before => {
                let Some(p) = cursor.position_over(bounds) else {
                    return;
                };
                let click = Click::new(to_text(p), mouse::Button::Left, state.last_click);
                state.last_click = Some(click);
                // The editor drags single clicks itself.
                state.dragging = click.kind() != click::Kind::Single;
            }
            Event::Mouse(mouse::Event::CursorMoved { .. }) if state.dragging => {
                // Inside a scrollable the cursor is the one in content space; off it (or
                // over its bars) it "levitates", which still counts.
                if let Some(p) = cursor.land().position() {
                    shell.publish((self.on_event)(EditorMouse::Drag(to_text(p))));
                }
            }
            Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left)) => {
                state.dragging = false;
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

impl<'a, Message, Theme, Renderer> From<EditorMouseArea<'a, Message, Theme, Renderer>>
    for Element<'a, Message, Theme, Renderer>
where
    Message: 'a,
    Theme: 'a,
    Renderer: renderer::Renderer + 'a,
{
    fn from(w: EditorMouseArea<'a, Message, Theme, Renderer>) -> Self {
        Element::new(w)
    }
}
