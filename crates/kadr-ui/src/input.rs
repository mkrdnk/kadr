//! Single-line native GPUI input, adapted from GPUI's `examples/input.rs`.
//! Byte indices are internal; the platform/IME boundary always uses UTF-16.
use std::ops::Range;

use gpui::{
    App, Bounds, ClipboardItem, Context, CursorStyle, ElementId, ElementInputHandler, Entity,
    EntityInputHandler, FocusHandle, Focusable, GlobalElementId, KeyBinding, LayoutId, MouseButton,
    MouseDownEvent, MouseMoveEvent, MouseUpEvent, PaintQuad, Pixels, Point, ShapedLine,
    SharedString, Style, TextRun, UTF16Selection, UnderlineStyle, Window, actions, div, fill,
    point, prelude::*, px, relative, rgb, rgba, size,
};
use unicode_segmentation::UnicodeSegmentation;

actions!(
    kadr_input,
    [
        Backspace,
        Delete,
        Left,
        Right,
        SelectLeft,
        SelectRight,
        SelectAll,
        Home,
        End,
        Paste,
        Cut,
        Copy
    ]
);

pub(crate) fn init(cx: &mut App) {
    cx.bind_keys([
        KeyBinding::new("backspace", Backspace, Some("KadrInput")),
        KeyBinding::new("delete", Delete, Some("KadrInput")),
        KeyBinding::new("left", Left, Some("KadrInput")),
        KeyBinding::new("right", Right, Some("KadrInput")),
        KeyBinding::new("shift-left", SelectLeft, Some("KadrInput")),
        KeyBinding::new("shift-right", SelectRight, Some("KadrInput")),
        KeyBinding::new("home", Home, Some("KadrInput")),
        KeyBinding::new("end", End, Some("KadrInput")),
        KeyBinding::new("shift-home", SelectHome, Some("KadrInput")),
        KeyBinding::new("shift-end", SelectEnd, Some("KadrInput")),
    ]);
    #[cfg(target_os = "macos")]
    let modifier = "cmd";
    #[cfg(not(target_os = "macos"))]
    let modifier = "ctrl";
    cx.bind_keys([
        KeyBinding::new(&format!("{modifier}-a"), SelectAll, Some("KadrInput")),
        KeyBinding::new(&format!("{modifier}-c"), Copy, Some("KadrInput")),
        KeyBinding::new(&format!("{modifier}-v"), Paste, Some("KadrInput")),
        KeyBinding::new(&format!("{modifier}-x"), Cut, Some("KadrInput")),
    ]);
}

actions!(kadr_input, [SelectHome, SelectEnd]);

pub(crate) struct TextInput {
    focus: FocusHandle,
    content: SharedString,
    placeholder: SharedString,
    selected: Range<usize>,
    reversed: bool,
    marked: Option<Range<usize>>,
    layout: Option<ShapedLine>,
    bounds: Option<Bounds<Pixels>>,
    selecting: bool,
    scroll: Pixels,
}

impl TextInput {
    pub fn new(
        value: impl Into<SharedString>,
        placeholder: &'static str,
        cx: &mut Context<Self>,
    ) -> Self {
        let content = value.into();
        let end = content.len();
        Self {
            focus: cx.focus_handle(),
            content,
            placeholder: placeholder.into(),
            selected: end..end,
            reversed: false,
            marked: None,
            layout: None,
            bounds: None,
            selecting: false,
            scroll: px(0.),
        }
    }

    pub fn value(&self) -> &str {
        &self.content
    }

    pub fn set_value(&mut self, value: String, cx: &mut Context<Self>) {
        self.content = value.into();
        self.selected = self.content.len()..self.content.len();
        self.reversed = false;
        self.marked = None;
        self.scroll = px(0.);
        cx.notify();
    }

    fn cursor(&self) -> usize {
        if self.reversed {
            self.selected.start
        } else {
            self.selected.end
        }
    }

    fn move_to(&mut self, offset: usize, cx: &mut Context<Self>) {
        self.selected = offset..offset;
        self.reversed = false;
        cx.notify();
    }

    fn select_to(&mut self, offset: usize, cx: &mut Context<Self>) {
        let anchor = if self.reversed {
            self.selected.end
        } else {
            self.selected.start
        };
        self.selected = anchor.min(offset)..anchor.max(offset);
        self.reversed = offset < anchor;
        cx.notify();
    }

    fn previous(&self, offset: usize) -> usize {
        self.content
            .grapheme_indices(true)
            .rev()
            .find_map(|(i, _)| (i < offset).then_some(i))
            .unwrap_or(0)
    }

    fn next(&self, offset: usize) -> usize {
        self.content
            .grapheme_indices(true)
            .find_map(|(i, _)| (i > offset).then_some(i))
            .unwrap_or(self.content.len())
    }

    fn left(&mut self, _: &Left, _: &mut Window, cx: &mut Context<Self>) {
        let offset = if self.selected.is_empty() {
            self.previous(self.cursor())
        } else {
            self.selected.start
        };
        self.move_to(offset, cx);
    }
    fn right(&mut self, _: &Right, _: &mut Window, cx: &mut Context<Self>) {
        let offset = if self.selected.is_empty() {
            self.next(self.cursor())
        } else {
            self.selected.end
        };
        self.move_to(offset, cx);
    }
    fn select_left(&mut self, _: &SelectLeft, _: &mut Window, cx: &mut Context<Self>) {
        self.select_to(self.previous(self.cursor()), cx);
    }
    fn select_right(&mut self, _: &SelectRight, _: &mut Window, cx: &mut Context<Self>) {
        self.select_to(self.next(self.cursor()), cx);
    }
    fn select_home(&mut self, _: &SelectHome, _: &mut Window, cx: &mut Context<Self>) {
        self.select_to(0, cx);
    }
    fn select_end(&mut self, _: &SelectEnd, _: &mut Window, cx: &mut Context<Self>) {
        self.select_to(self.content.len(), cx);
    }
    fn select_all(&mut self, _: &SelectAll, _: &mut Window, cx: &mut Context<Self>) {
        self.move_to(0, cx);
        self.select_to(self.content.len(), cx);
    }
    fn home(&mut self, _: &Home, _: &mut Window, cx: &mut Context<Self>) {
        self.move_to(0, cx);
    }
    fn end(&mut self, _: &End, _: &mut Window, cx: &mut Context<Self>) {
        self.move_to(self.content.len(), cx);
    }
    fn backspace(&mut self, _: &Backspace, window: &mut Window, cx: &mut Context<Self>) {
        if self.selected.is_empty() {
            self.select_to(self.previous(self.cursor()), cx);
        }
        self.replace_text_in_range(None, "", window, cx);
    }
    fn delete(&mut self, _: &Delete, window: &mut Window, cx: &mut Context<Self>) {
        if self.selected.is_empty() {
            self.select_to(self.next(self.cursor()), cx);
        }
        self.replace_text_in_range(None, "", window, cx);
    }
    fn paste(&mut self, _: &Paste, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(text) = cx.read_from_clipboard().and_then(|item| item.text()) {
            self.replace_text_in_range(None, &text, window, cx);
        }
    }
    fn copy(&mut self, _: &Copy, _: &mut Window, cx: &mut Context<Self>) {
        if !self.selected.is_empty() {
            cx.write_to_clipboard(ClipboardItem::new_string(
                self.content[self.selected.clone()].into(),
            ));
        }
    }
    fn cut(&mut self, _: &Cut, window: &mut Window, cx: &mut Context<Self>) {
        self.copy(&Copy, window, cx);
        if !self.selected.is_empty() {
            self.replace_text_in_range(None, "", window, cx);
        }
    }
    fn mouse_index(&self, position: Point<Pixels>) -> usize {
        match (&self.bounds, &self.layout) {
            (Some(bounds), Some(line)) if !self.content.is_empty() => {
                line.closest_index_for_x(position.x - bounds.left() + self.scroll)
            }
            _ => 0,
        }
    }
    fn mouse_down(&mut self, event: &MouseDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        window.focus(&self.focus);
        self.selecting = true;
        let offset = self.mouse_index(event.position);
        if event.click_count >= 2 {
            self.move_to(0, cx);
            self.select_to(self.content.len(), cx);
        } else if event.modifiers.shift {
            self.select_to(offset, cx);
        } else {
            self.move_to(offset, cx);
        }
    }
    fn mouse_up(&mut self, _: &MouseUpEvent, _: &mut Window, _: &mut Context<Self>) {
        self.selecting = false;
    }
    fn mouse_move(&mut self, event: &MouseMoveEvent, _: &mut Window, cx: &mut Context<Self>) {
        if self.selecting {
            self.select_to(self.mouse_index(event.position), cx);
        }
    }
    fn range_from_utf16(&self, range: &Range<usize>) -> Range<usize> {
        utf16_to_utf8(&self.content, range.start)..utf16_to_utf8(&self.content, range.end)
    }
    fn to_utf16(&self, range: &Range<usize>) -> Range<usize> {
        self.content[..range.start].encode_utf16().count()
            ..self.content[..range.end].encode_utf16().count()
    }
}

/// Clamp to a complete scalar value, including malformed platform offsets that
/// fall in the middle of a surrogate pair. Never slice UTF-8 at an arbitrary byte.
fn utf16_to_utf8(text: &str, offset: usize) -> usize {
    let mut units = 0;
    for (byte, ch) in text.char_indices() {
        if units + ch.len_utf16() > offset {
            return byte;
        }
        units += ch.len_utf16();
    }
    text.len()
}

fn single_line(text: &str) -> String {
    text.chars()
        .map(|ch| if ch.is_control() { ' ' } else { ch })
        .collect()
}

impl EntityInputHandler for TextInput {
    fn text_for_range(
        &mut self,
        range: Range<usize>,
        actual: &mut Option<Range<usize>>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<String> {
        let range = self.range_from_utf16(&range);
        *actual = Some(self.to_utf16(&range));
        Some(self.content[range].into())
    }
    fn selected_text_range(
        &mut self,
        _: bool,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<UTF16Selection> {
        Some(UTF16Selection {
            range: self.to_utf16(&self.selected),
            reversed: self.reversed,
        })
    }
    fn marked_text_range(&self, _: &mut Window, _: &mut Context<Self>) -> Option<Range<usize>> {
        self.marked.as_ref().map(|range| self.to_utf16(range))
    }
    fn unmark_text(&mut self, _: &mut Window, cx: &mut Context<Self>) {
        self.marked = None;
        cx.notify();
    }
    fn replace_text_in_range(
        &mut self,
        range: Option<Range<usize>>,
        text: &str,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let range = range
            .as_ref()
            .map(|range| self.range_from_utf16(range))
            .or(self.marked.clone())
            .unwrap_or(self.selected.clone());
        let text = single_line(text);
        self.content = format!(
            "{}{}{}",
            &self.content[..range.start],
            text,
            &self.content[range.end..]
        )
        .into();
        self.selected = range.start + text.len()..range.start + text.len();
        self.reversed = false;
        self.marked = None;
        cx.notify();
    }
    fn replace_and_mark_text_in_range(
        &mut self,
        range: Option<Range<usize>>,
        text: &str,
        selection: Option<Range<usize>>,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let range = range
            .as_ref()
            .map(|range| self.range_from_utf16(range))
            .or(self.marked.clone())
            .unwrap_or(self.selected.clone());
        let text = single_line(text);
        let start = range.start;
        self.content = format!(
            "{}{}{}",
            &self.content[..start],
            text,
            &self.content[range.end..]
        )
        .into();
        self.marked = (!text.is_empty()).then_some(start..start + text.len());
        // IME selection is relative to the inserted text, not the whole field.
        self.selected = selection
            .map(|r| start + utf16_to_utf8(&text, r.start)..start + utf16_to_utf8(&text, r.end))
            .unwrap_or(start + text.len()..start + text.len());
        self.reversed = false;
        cx.notify();
    }
    fn bounds_for_range(
        &mut self,
        range: Range<usize>,
        bounds: Bounds<Pixels>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<Bounds<Pixels>> {
        let line = self.layout.as_ref()?;
        let range = self.range_from_utf16(&range);
        Some(Bounds::from_corners(
            point(
                bounds.left() + line.x_for_index(range.start) - self.scroll,
                bounds.top(),
            ),
            point(
                bounds.left() + line.x_for_index(range.end) - self.scroll,
                bounds.bottom(),
            ),
        ))
    }
    fn character_index_for_point(
        &mut self,
        point: Point<Pixels>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<usize> {
        let bounds = self.bounds?;
        let line = self.layout.as_ref()?;
        let index = line.closest_index_for_x(point.x - bounds.left() + self.scroll);
        Some(
            self.content[..index.min(self.content.len())]
                .encode_utf16()
                .count(),
        )
    }
}

struct TextElement {
    input: Entity<TextInput>,
}
struct Prepaint {
    line: ShapedLine,
    cursor: Option<PaintQuad>,
    selection: Option<PaintQuad>,
    scroll: Pixels,
}

impl IntoElement for TextElement {
    type Element = Self;
    fn into_element(self) -> Self {
        self
    }
}
impl Element for TextElement {
    type RequestLayoutState = ();
    type PrepaintState = Prepaint;
    fn id(&self) -> Option<ElementId> {
        None
    }
    fn source_location(&self) -> Option<&'static core::panic::Location<'static>> {
        None
    }
    fn request_layout(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&gpui::InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, ()) {
        let mut style = Style::default();
        style.size.width = relative(1.).into();
        style.size.height = window.line_height().into();
        (window.request_layout(style, [], cx), ())
    }
    fn prepaint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&gpui::InspectorElementId>,
        bounds: Bounds<Pixels>,
        _: &mut (),
        window: &mut Window,
        cx: &mut App,
    ) -> Prepaint {
        let input = self.input.read(cx);
        let style = window.text_style();
        let text = if input.content.is_empty() {
            input.placeholder.clone()
        } else {
            input.content.clone()
        };
        let run = TextRun {
            len: text.len(),
            font: style.font(),
            color: if input.content.is_empty() {
                rgb(0xa9b7c6).into()
            } else {
                style.color
            },
            background_color: None,
            underline: None,
            strikethrough: None,
        };
        let runs = if let Some(marked) = &input.marked {
            vec![
                TextRun {
                    len: marked.start,
                    ..run.clone()
                },
                TextRun {
                    len: marked.len(),
                    underline: Some(UnderlineStyle {
                        color: Some(run.color),
                        thickness: px(1.),
                        wavy: false,
                    }),
                    ..run.clone()
                },
                TextRun {
                    len: text.len() - marked.end,
                    ..run
                },
            ]
            .into_iter()
            .filter(|run| run.len > 0)
            .collect()
        } else {
            vec![run]
        };
        let line = window.text_system().shape_line(
            text,
            style.font_size.to_pixels(window.rem_size()),
            &runs,
            None,
        );
        let x = line.x_for_index(input.cursor());
        let width = bounds.size.width;
        let scroll = input.scroll.min(x).max(x - width + px(3.)).max(px(0.));
        let cursor = input.selected.is_empty().then(|| {
            fill(
                Bounds::new(
                    point(bounds.left() + x - scroll, bounds.top()),
                    size(px(2.), bounds.size.height),
                ),
                rgb(0x9adbc5),
            )
        });
        let selection = (!input.selected.is_empty()).then(|| {
            fill(
                Bounds::from_corners(
                    point(
                        bounds.left() + line.x_for_index(input.selected.start) - scroll,
                        bounds.top(),
                    ),
                    point(
                        bounds.left() + line.x_for_index(input.selected.end) - scroll,
                        bounds.bottom(),
                    ),
                ),
                rgba(0x9adbc540),
            )
        });
        Prepaint {
            line,
            cursor,
            selection,
            scroll,
        }
    }
    fn paint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&gpui::InspectorElementId>,
        bounds: Bounds<Pixels>,
        _: &mut (),
        prepaint: &mut Prepaint,
        window: &mut Window,
        cx: &mut App,
    ) {
        let focus = self.input.read(cx).focus.clone();
        window.handle_input(
            &focus,
            ElementInputHandler::new(bounds, self.input.clone()),
            cx,
        );
        if let Some(selection) = prepaint.selection.take() {
            window.paint_quad(selection);
        }
        let _ = prepaint.line.paint(
            point(bounds.left() - prepaint.scroll, bounds.top()),
            window.line_height(),
            window,
            cx,
        );
        if focus.is_focused(window)
            && let Some(cursor) = prepaint.cursor.take()
        {
            window.paint_quad(cursor);
        }
        self.input.update(cx, |input, _| {
            input.layout = Some(prepaint.line.clone());
            input.bounds = Some(bounds);
            input.scroll = prepaint.scroll;
        });
    }
}

impl Focusable for TextInput {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}
impl Render for TextInput {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .id("text-input")
            .key_context("KadrInput")
            .track_focus(&self.focus)
            .tab_index(0)
            .cursor(CursorStyle::IBeam)
            .w_full()
            .min_w_0()
            .bg(rgb(0x11161c))
            .border_1()
            .border_color(rgb(0x334150))
            .rounded_md()
            .focus(|style| style.border_color(rgb(0x9adbc5)))
            .px_3()
            .py_2()
            .text_base()
            .text_color(rgb(0xe8edf3))
            .line_height(px(24.))
            .overflow_hidden()
            .on_action(cx.listener(Self::backspace))
            .on_action(cx.listener(Self::delete))
            .on_action(cx.listener(Self::left))
            .on_action(cx.listener(Self::right))
            .on_action(cx.listener(Self::select_left))
            .on_action(cx.listener(Self::select_right))
            .on_action(cx.listener(Self::select_all))
            .on_action(cx.listener(Self::home))
            .on_action(cx.listener(Self::end))
            .on_action(cx.listener(Self::select_home))
            .on_action(cx.listener(Self::select_end))
            .on_action(cx.listener(Self::paste))
            .on_action(cx.listener(Self::cut))
            .on_action(cx.listener(Self::copy))
            .on_key_down(cx.listener(|this, event: &gpui::KeyDownEvent, _, cx| {
                // Enter commits a composition first; it must not submit the
                // surrounding search form while the IME still owns this text.
                if this.marked.is_some() && event.keystroke.key == "enter" {
                    cx.stop_propagation();
                }
            }))
            .on_mouse_down(MouseButton::Left, cx.listener(Self::mouse_down))
            .on_mouse_up(MouseButton::Left, cx.listener(Self::mouse_up))
            .on_mouse_up_out(MouseButton::Left, cx.listener(Self::mouse_up))
            .on_mouse_move(cx.listener(Self::mouse_move))
            .child(TextElement { input: cx.entity() })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn utf16_offsets_never_split_utf8_or_surrogates() {
        let text = "A🎬ё";
        assert_eq!(
            (0..=6).map(|n| utf16_to_utf8(text, n)).collect::<Vec<_>>(),
            vec![0, 1, 1, 5, 7, 7, 7]
        );
        assert_eq!(single_line("a\r\nb\tc"), "a  b c");
    }
}
