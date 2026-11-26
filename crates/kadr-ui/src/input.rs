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

use super::{ACCENT, BACKGROUND, BORDER, MUTED, TEXT};

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

const PASSWORD_MASK: &str = "•";

#[derive(Clone, Copy, PartialEq, Eq)]
enum InputMode {
    Plain,
    Password,
}

impl InputMode {
    fn display_text(self, content: &SharedString) -> SharedString {
        match self {
            Self::Plain => content.clone(),
            // One mask per scalar keeps every valid IME boundary representable,
            // including boundaries inside a combining sequence or ZWJ grapheme.
            Self::Password => PASSWORD_MASK.repeat(content.chars().count()).into(),
        }
    }

    fn layout_index(self, content: &str, byte: usize) -> usize {
        match self {
            Self::Plain => byte,
            Self::Password => {
                content
                    .char_indices()
                    .take_while(|(start, ch)| start + ch.len_utf8() <= byte)
                    .count()
                    * PASSWORD_MASK.len()
            }
        }
    }

    fn content_index(self, content: &str, byte: usize) -> usize {
        match self {
            Self::Plain => byte,
            Self::Password => content
                .char_indices()
                .nth(byte / PASSWORD_MASK.len())
                .map_or(content.len(), |(index, _)| index),
        }
    }

    fn platform_text(
        self,
        content: &str,
        range: Range<usize>,
        actual: &mut Option<Range<usize>>,
    ) -> Option<String> {
        match self {
            Self::Plain => {
                let range = utf16_to_utf8(content, range.start)..utf16_to_utf8(content, range.end);
                *actual = Some(
                    content[..range.start].encode_utf16().count()
                        ..content[..range.end].encode_utf16().count(),
                );
                Some(content[range].into())
            }
            // Do not disclose surrounding or composing text to the OS/IME.
            // Returning None also avoids inventing a different UTF-16 document.
            Self::Password => {
                *actual = None;
                None
            }
        }
    }

    fn clipboard_text(self, content: &str, range: Range<usize>) -> Option<&str> {
        match self {
            Self::Plain if !range.is_empty() => Some(&content[range]),
            _ => None,
        }
    }
}

pub(crate) struct TextInput {
    focus: FocusHandle,
    content: SharedString,
    mode: InputMode,
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
            mode: InputMode::Plain,
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

    /// An initially empty password field. Only `value()` exposes its real text;
    /// rendering, clipboard actions and OS surrounding-text queries never do.
    pub fn password(placeholder: &'static str, cx: &mut Context<Self>) -> Self {
        Self {
            mode: InputMode::Password,
            ..Self::new("", placeholder, cx)
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
        if let Some(text) = self
            .mode
            .clipboard_text(&self.content, self.selected.clone())
        {
            cx.write_to_clipboard(ClipboardItem::new_string(text.into()));
        }
    }
    fn cut(&mut self, _: &Cut, window: &mut Window, cx: &mut Context<Self>) {
        if self.mode == InputMode::Password {
            return;
        }
        self.copy(&Copy, window, cx);
        if !self.selected.is_empty() {
            self.replace_text_in_range(None, "", window, cx);
        }
    }
    fn mouse_index(&self, position: Point<Pixels>) -> usize {
        match (&self.bounds, &self.layout) {
            (Some(bounds), Some(line)) if !self.content.is_empty() => self.mode.content_index(
                &self.content,
                line.closest_index_for_x(position.x - bounds.left() + self.scroll),
            ),
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
        self.mode.platform_text(&self.content, range, actual)
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
        let range = self.mode.layout_index(&self.content, range.start)
            ..self.mode.layout_index(&self.content, range.end);
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
        let index = if self.mode == InputMode::Password && self.content.is_empty() {
            0
        } else {
            self.mode.content_index(
                &self.content,
                line.closest_index_for_x(point.x - bounds.left() + self.scroll),
            )
        };
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
            input.mode.display_text(&input.content)
        };
        let run = TextRun {
            len: text.len(),
            font: style.font(),
            color: if input.content.is_empty() {
                rgb(MUTED).into()
            } else {
                style.color
            },
            background_color: None,
            underline: None,
            strikethrough: None,
        };
        let runs = if let Some(marked) = &input.marked {
            let marked = input.mode.layout_index(&input.content, marked.start)
                ..input.mode.layout_index(&input.content, marked.end);
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
        let x = line.x_for_index(input.mode.layout_index(&input.content, input.cursor()));
        let width = bounds.size.width;
        let scroll = input.scroll.min(x).max(x - width + px(3.)).max(px(0.));
        let cursor = input.selected.is_empty().then(|| {
            fill(
                Bounds::new(
                    point(bounds.left() + x - scroll, bounds.top()),
                    size(px(2.), bounds.size.height),
                ),
                rgb(ACCENT),
            )
        });
        let selection = (!input.selected.is_empty()).then(|| {
            fill(
                Bounds::from_corners(
                    point(
                        bounds.left()
                            + line.x_for_index(
                                input
                                    .mode
                                    .layout_index(&input.content, input.selected.start),
                            )
                            - scroll,
                        bounds.top(),
                    ),
                    point(
                        bounds.left()
                            + line.x_for_index(
                                input.mode.layout_index(&input.content, input.selected.end),
                            )
                            - scroll,
                        bounds.bottom(),
                    ),
                ),
                rgba((ACCENT << 8) | 0x40),
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
            .bg(rgb(BACKGROUND))
            .border_1()
            .border_color(rgb(BORDER))
            .rounded(px(5.))
            .focus(|style| style.border_color(rgb(ACCENT)))
            .px(px(11.))
            .py(px(7.))
            .text_size(px(14.))
            .text_color(rgb(TEXT))
            .line_height(px(20.))
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

    #[test]
    fn password_layout_masks_every_scalar_including_composition() {
        let content: SharedString = "Aё🎬e\u{301}👩\u{200d}💻".into();
        let mode = InputMode::Password;
        let displayed = mode.display_text(&content);
        assert_eq!(displayed.as_ref(), "••••••••");

        // Selected and marked runs must slice only the masked layout. The
        // combining sequence and ZWJ sequence remain entirely confidential.
        for range in [1..7, 7..10, 10..content.len(), 0..content.len()] {
            let layout =
                mode.layout_index(&content, range.start)..mode.layout_index(&content, range.end);
            assert!(displayed[layout].chars().all(|ch| ch == '•'));
        }
        assert!(mode.display_text(&SharedString::default()).is_empty());
    }

    #[test]
    fn password_offsets_round_trip_utf8_layout_and_platform_utf16() {
        let content = "Aё🎬e\u{301}👩\u{200d}💻";
        let mode = InputMode::Password;
        let real_bytes = [0, 1, 3, 7, 8, 10, 14, 17, 21];
        let utf16_offsets = [0, 1, 2, 4, 5, 6, 8, 9, 11];
        for (scalar, (&byte, &utf16)) in real_bytes.iter().zip(&utf16_offsets).enumerate() {
            let layout = scalar * PASSWORD_MASK.len();
            assert_eq!(mode.layout_index(content, byte), layout);
            assert_eq!(mode.content_index(content, layout), byte);
            assert_eq!(utf16_to_utf8(content, utf16), byte);
            assert_eq!(
                content[..mode.content_index(content, layout)]
                    .encode_utf16()
                    .count(),
                utf16
            );
            // A malformed layout offset cannot produce a partial UTF-8 scalar.
            assert_eq!(mode.content_index(content, layout + 1), byte);
        }
        assert_eq!(mode.layout_index(content, usize::MAX), 24);
        assert_eq!(mode.content_index(content, usize::MAX), content.len());
        assert_eq!(mode.layout_index(content, 5), 6);
        assert_eq!(mode.content_index("", 3), 0);
        // The low half of the emoji's surrogate pair maps to its start.
        assert_eq!(mode.layout_index(content, utf16_to_utf8(content, 3)), 6);
    }

    #[test]
    fn password_never_exports_platform_or_clipboard_text() {
        let content = "sё🎬e\u{301}cret";
        for range in [0..0, 0..content.len(), 1..4, 2..3, usize::MAX..usize::MAX] {
            let mut actual = Some(1..2);
            assert_eq!(
                InputMode::Password.platform_text(content, range.clone(), &mut actual),
                None,
            );
            assert_eq!(actual, None);
            assert_eq!(InputMode::Password.clipboard_text(content, range), None);
        }
    }

    #[test]
    fn plain_mode_preserves_text_offsets_and_exports() {
        let content: SharedString = "A🎬ё".into();
        let mode = InputMode::Plain;
        assert_eq!(mode.display_text(&content), content);
        for byte in [0, 1, 5, 7] {
            assert_eq!(mode.layout_index(&content, byte), byte);
            assert_eq!(mode.content_index(&content, byte), byte);
        }
        let mut actual = None;
        assert_eq!(
            mode.platform_text(&content, 2..4, &mut actual).as_deref(),
            Some("🎬ё"),
        );
        assert_eq!(actual, Some(1..4));
        assert_eq!(mode.clipboard_text(&content, 1..5), Some("🎬"));
        assert_eq!(mode.clipboard_text(&content, 1..1), None);
    }
}
