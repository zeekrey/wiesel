// Adapted from GPUI input.rs (Apache-2.0); retains native IME and Unicode editing.
use std::ops::Range;

use crate::{
    notifications::{Notification, Severity},
    theme,
};

use gpui::{
    App, Bounds, ClipboardItem, Context, CursorStyle, ElementId, ElementInputHandler, Entity,
    EntityInputHandler, FocusHandle, Focusable, GlobalElementId, LayoutId, MouseButton,
    MouseDownEvent, MouseMoveEvent, MouseUpEvent, PaintQuad, Pixels, Point, ShapedLine,
    SharedString, Style, TextRun, UTF16Selection, UnderlineStyle, Window, actions, div, fill,
    point, prelude::*, px, relative, rgb, rgba, size,
};
use unicode_segmentation::*;

fn byte_offset_for_utf16(text: &str, offset: usize) -> usize {
    let mut units = 0;
    for (index, character) in text.char_indices() {
        if units + character.len_utf16() > offset {
            return index;
        }
        units += character.len_utf16();
    }
    text.len()
}

actions!(
    text_input,
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
        ShowCharacterPalette,
        Paste,
        Cut,
        Copy,
        Quit,
    ]
);

fn normalize_pasted_text(text: &str, multiline: bool) -> String {
    if multiline {
        text.replace("\r\n", "\n")
    } else {
        text.replace("\n", " ")
    }
}

pub struct TextInput {
    focus_handle: FocusHandle,
    pub content: SharedString,
    secret: bool,
    multiline: bool,
    placeholder: SharedString,
    accessibility_id: Option<SharedString>,
    selected_range: Range<usize>,
    selection_reversed: bool,
    marked_range: Option<Range<usize>>,
    last_layout: Option<ShapedLine>,
    last_lines: Vec<(usize, ShapedLine)>,
    scroll_y: Pixels,
    last_bounds: Option<Bounds<Pixels>>,
    scroll_x: Pixels,
    is_selecting: bool,
    #[cfg(test)]
    paste_reader: Option<Box<dyn FnMut() -> Option<String>>>,
}

impl gpui::EventEmitter<Notification> for TextInput {}

impl TextInput {
    pub fn new(placeholder: &str, secret: bool, cx: &mut Context<Self>) -> Self {
        Self {
            focus_handle: cx.focus_handle(),
            content: "".into(),
            secret,
            multiline: false,
            placeholder: placeholder.to_owned().into(),
            accessibility_id: None,
            selected_range: 0..0,
            selection_reversed: false,
            marked_range: None,
            last_layout: None,
            last_lines: Vec::new(),
            scroll_y: px(0.),
            last_bounds: None,
            scroll_x: px(0.),
            is_selecting: false,
            #[cfg(test)]
            paste_reader: None,
        }
    }
    #[cfg(test)]
    pub fn inject_paste_reader(&mut self, reader: impl FnMut() -> Option<String> + 'static) {
        self.paste_reader = Some(Box::new(reader));
    }
    pub fn with_tab_stop(mut self) -> Self {
        self.focus_handle = self.focus_handle.tab_stop(true);
        self
    }
    pub fn multiline(placeholder: &str, cx: &mut Context<Self>) -> Self {
        Self {
            multiline: true,
            ..Self::new(placeholder, false, cx)
        }
    }
    pub fn with_accessibility_id(mut self, id: &'static str) -> Self {
        self.accessibility_id = Some(id.into());
        self
    }

    pub fn is_composing(&self) -> bool {
        self.marked_range.is_some()
    }

    pub fn set(&mut self, value: impl Into<SharedString>, cx: &mut Context<Self>) {
        self.reset();
        self.content = value.into();
        self.selected_range = self.content.len()..self.content.len();
        cx.notify();
    }
    fn left(&mut self, _: &Left, _: &mut Window, cx: &mut Context<Self>) {
        if self.selected_range.is_empty() {
            self.move_to(self.previous_boundary(self.cursor_offset()), cx);
        } else {
            self.move_to(self.selected_range.start, cx)
        }
    }

    fn right(&mut self, _: &Right, _: &mut Window, cx: &mut Context<Self>) {
        if self.selected_range.is_empty() {
            self.move_to(self.next_boundary(self.selected_range.end), cx);
        } else {
            self.move_to(self.selected_range.end, cx)
        }
    }

    fn select_left(&mut self, _: &SelectLeft, _: &mut Window, cx: &mut Context<Self>) {
        self.select_to(self.previous_boundary(self.cursor_offset()), cx);
    }

    fn select_right(&mut self, _: &SelectRight, _: &mut Window, cx: &mut Context<Self>) {
        self.select_to(self.next_boundary(self.cursor_offset()), cx);
    }

    fn select_all(&mut self, _: &SelectAll, _: &mut Window, cx: &mut Context<Self>) {
        self.move_to(0, cx);
        self.select_to(self.content.len(), cx)
    }

    fn home(&mut self, _: &Home, _: &mut Window, cx: &mut Context<Self>) {
        self.move_to(0, cx);
    }

    fn end(&mut self, _: &End, _: &mut Window, cx: &mut Context<Self>) {
        self.move_to(self.content.len(), cx);
    }

    fn backspace(&mut self, _: &Backspace, window: &mut Window, cx: &mut Context<Self>) {
        if self.selected_range.is_empty() {
            let prev = self.previous_boundary(self.cursor_offset());
            if self.cursor_offset() == prev {
                window.play_system_bell();
                return;
            }
            self.select_to(prev, cx)
        }
        self.replace_text_in_range(None, "", window, cx)
    }

    fn delete(&mut self, _: &Delete, window: &mut Window, cx: &mut Context<Self>) {
        if self.selected_range.is_empty() {
            let next = self.next_boundary(self.cursor_offset());
            if self.cursor_offset() == next {
                window.play_system_bell();
                return;
            }
            self.select_to(next, cx)
        }
        self.replace_text_in_range(None, "", window, cx)
    }

    fn on_mouse_down(
        &mut self,
        event: &MouseDownEvent,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.is_selecting = true;

        if event.modifiers.shift {
            self.select_to(self.index_for_mouse_position(event.position), cx);
        } else {
            self.move_to(self.index_for_mouse_position(event.position), cx)
        }
    }

    fn on_mouse_up(&mut self, _: &MouseUpEvent, _window: &mut Window, _: &mut Context<Self>) {
        self.is_selecting = false;
    }

    fn on_mouse_move(&mut self, event: &MouseMoveEvent, _: &mut Window, cx: &mut Context<Self>) {
        if self.is_selecting {
            self.select_to(self.index_for_mouse_position(event.position), cx);
        }
    }

    fn show_character_palette(
        &mut self,
        _: &ShowCharacterPalette,
        window: &mut Window,
        _: &mut Context<Self>,
    ) {
        window.show_character_palette();
    }

    fn read_paste_text(&mut self, cx: &App) -> Option<String> {
        #[cfg(test)]
        if let Some(reader) = &mut self.paste_reader {
            return reader();
        }
        cx.read_from_clipboard().and_then(|item| item.text())
    }
    fn paste(&mut self, _: &Paste, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(text) = self.read_paste_text(cx).filter(|text| !text.is_empty()) {
            self.replace_text_in_range(
                None,
                &normalize_pasted_text(&text, self.multiline),
                window,
                cx,
            );
        } else {
            cx.emit(Notification::new(
                Severity::Warning,
                "No text available to paste.",
            ));
        }
    }

    fn copy(&mut self, _: &Copy, _: &mut Window, cx: &mut Context<Self>) {
        if !self.secret && !self.selected_range.is_empty() {
            cx.write_to_clipboard(ClipboardItem::new_string(
                self.content[self.selected_range.clone()].to_string(),
            ));
        }
    }
    fn cut(&mut self, _: &Cut, window: &mut Window, cx: &mut Context<Self>) {
        if !self.secret && !self.selected_range.is_empty() {
            cx.write_to_clipboard(ClipboardItem::new_string(
                self.content[self.selected_range.clone()].to_string(),
            ));
            self.replace_text_in_range(None, "", window, cx)
        }
    }

    fn move_to(&mut self, offset: usize, cx: &mut Context<Self>) {
        self.selected_range = offset..offset;
        self.selection_reversed = false;
        cx.notify()
    }

    fn cursor_offset(&self) -> usize {
        if self.selection_reversed {
            self.selected_range.start
        } else {
            self.selected_range.end
        }
    }

    fn index_for_mouse_position(&self, position: Point<Pixels>) -> usize {
        if self.content.is_empty() {
            return 0;
        }
        if self.multiline {
            let Some(bounds) = self.last_bounds else {
                return 0;
            };
            let row = ((position.y - bounds.top() + self.scroll_y) / px(28.)).max(0.) as usize;
            if let Some((start, line)) = self
                .last_lines
                .get(row.min(self.last_lines.len().saturating_sub(1)))
            {
                return self.content_boundary(
                    start + line.closest_index_for_x(position.x - bounds.left() + self.scroll_x),
                );
            }
            return 0;
        }

        let (Some(bounds), Some(line)) = (self.last_bounds.as_ref(), self.last_layout.as_ref())
        else {
            return 0;
        };
        if position.y < bounds.top() {
            return 0;
        }
        if position.y > bounds.bottom() {
            return self.content.len();
        }
        self.content_boundary(line.closest_index_for_x(position.x - bounds.left() + self.scroll_x))
    }

    fn content_boundary(&self, index: usize) -> usize {
        let mut index = index.min(self.content.len());
        while !self.content.is_char_boundary(index) {
            index -= 1;
        }
        index
    }

    fn select_to(&mut self, offset: usize, cx: &mut Context<Self>) {
        if self.selection_reversed {
            self.selected_range.start = offset
        } else {
            self.selected_range.end = offset
        };
        if self.selected_range.end < self.selected_range.start {
            self.selection_reversed = !self.selection_reversed;
            self.selected_range = self.selected_range.end..self.selected_range.start;
        }
        cx.notify()
    }

    fn offset_from_utf16(&self, offset: usize) -> usize {
        byte_offset_for_utf16(&self.content, offset)
    }

    fn offset_to_utf16(&self, offset: usize) -> usize {
        let mut utf16_offset = 0;
        let mut utf8_count = 0;

        for ch in self.content.chars() {
            if utf8_count >= offset {
                break;
            }
            utf8_count += ch.len_utf8();
            utf16_offset += ch.len_utf16();
        }

        utf16_offset
    }

    fn range_to_utf16(&self, range: &Range<usize>) -> Range<usize> {
        self.offset_to_utf16(range.start)..self.offset_to_utf16(range.end)
    }

    fn range_from_utf16(&self, range_utf16: &Range<usize>) -> Range<usize> {
        self.offset_from_utf16(range_utf16.start)..self.offset_from_utf16(range_utf16.end)
    }

    fn previous_boundary(&self, offset: usize) -> usize {
        self.content
            .grapheme_indices(true)
            .rev()
            .find_map(|(idx, _)| (idx < offset).then_some(idx))
            .unwrap_or(0)
    }

    fn next_boundary(&self, offset: usize) -> usize {
        self.content
            .grapheme_indices(true)
            .find_map(|(idx, _)| (idx > offset).then_some(idx))
            .unwrap_or(self.content.len())
    }

    pub fn reset(&mut self) {
        self.content = "".into();
        self.selected_range = 0..0;
        self.selection_reversed = false;
        self.marked_range = None;
        self.last_layout = None;
        self.last_lines.clear();
        self.scroll_y = px(0.);
        self.last_bounds = None;
        self.scroll_x = px(0.);
        self.is_selecting = false;
    }
}

impl EntityInputHandler for TextInput {
    fn text_for_range(
        &mut self,
        range_utf16: Range<usize>,
        actual_range: &mut Option<Range<usize>>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<String> {
        if self.secret {
            return None;
        }
        let range = self.range_from_utf16(&range_utf16);
        actual_range.replace(self.range_to_utf16(&range));
        Some(self.content[range].to_string())
    }

    fn selected_text_range(
        &mut self,
        _ignore_disabled_input: bool,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<UTF16Selection> {
        Some(UTF16Selection {
            range: self.range_to_utf16(&self.selected_range),
            reversed: self.selection_reversed,
        })
    }

    fn marked_text_range(
        &self,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<Range<usize>> {
        self.marked_range
            .as_ref()
            .map(|range| self.range_to_utf16(range))
    }

    fn unmark_text(&mut self, _window: &mut Window, _cx: &mut Context<Self>) {
        self.marked_range = None;
    }

    fn replace_text_in_range(
        &mut self,
        range_utf16: Option<Range<usize>>,
        new_text: &str,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let range = range_utf16
            .as_ref()
            .map(|range_utf16| self.range_from_utf16(range_utf16))
            .or(self.marked_range.clone())
            .unwrap_or(self.selected_range.clone());

        self.content =
            (self.content[0..range.start].to_owned() + new_text + &self.content[range.end..])
                .into();
        self.selected_range = range.start + new_text.len()..range.start + new_text.len();
        self.marked_range.take();
        cx.notify();
    }

    fn replace_and_mark_text_in_range(
        &mut self,
        range_utf16: Option<Range<usize>>,
        new_text: &str,
        new_selected_range_utf16: Option<Range<usize>>,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let range = range_utf16
            .as_ref()
            .map(|range_utf16| self.range_from_utf16(range_utf16))
            .or(self.marked_range.clone())
            .unwrap_or(self.selected_range.clone());

        self.content =
            (self.content[0..range.start].to_owned() + new_text + &self.content[range.end..])
                .into();
        if !new_text.is_empty() {
            self.marked_range = Some(range.start..range.start + new_text.len());
        } else {
            self.marked_range = None;
        }
        self.selected_range = new_selected_range_utf16
            .as_ref()
            .map(|selection| {
                byte_offset_for_utf16(new_text, selection.start)
                    ..byte_offset_for_utf16(new_text, selection.end)
            })
            .map(|new_range| new_range.start + range.start..new_range.end + range.start)
            .unwrap_or_else(|| range.start + new_text.len()..range.start + new_text.len());

        cx.notify();
    }

    fn bounds_for_range(
        &mut self,
        range_utf16: Range<usize>,
        bounds: Bounds<Pixels>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<Bounds<Pixels>> {
        let range = self.range_from_utf16(&range_utf16);
        if self.multiline {
            let (row, (start, line)) = self
                .last_lines
                .iter()
                .enumerate()
                .rev()
                .find(|(_, (start, _))| *start <= range.start)?;
            let y = bounds.top() + px(row as f32 * 28.) - self.scroll_y;
            return Some(Bounds::new(
                point(
                    bounds.left() + line.x_for_index(range.start - start) - self.scroll_x,
                    y,
                ),
                size(px(2.), px(28.)),
            ));
        }
        let last_layout = self.last_layout.as_ref()?;
        Some(Bounds::from_corners(
            point(
                bounds.left() + last_layout.x_for_index(range.start) - self.scroll_x,
                bounds.top(),
            ),
            point(
                bounds.left() + last_layout.x_for_index(range.end) - self.scroll_x,
                bounds.bottom(),
            ),
        ))
    }

    fn character_index_for_point(
        &mut self,
        point: gpui::Point<Pixels>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<usize> {
        if self.multiline {
            return Some(self.offset_to_utf16(self.index_for_mouse_position(point)));
        }
        let line_point = self.last_bounds?.localize(&point)?;
        let last_layout = self.last_layout.as_ref()?;

        // A password field's painted text is masked, not equal to its content.
        let utf8_index =
            self.content_boundary(last_layout.index_for_x(line_point.x + self.scroll_x)?);
        Some(self.offset_to_utf16(utf8_index))
    }
}

struct TextElement {
    input: Entity<TextInput>,
}

struct PrepaintState {
    scroll_x: Pixels,
    lines: Vec<(usize, ShapedLine)>,
    scroll_y: Pixels,
    cursor: Option<PaintQuad>,
    selections: Vec<PaintQuad>,
}

impl IntoElement for TextElement {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

impl Element for TextElement {
    type RequestLayoutState = ();
    type PrepaintState = PrepaintState;

    fn id(&self) -> Option<ElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static core::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&gpui::InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, Self::RequestLayoutState) {
        let mut style = Style::default();
        style.size.width = relative(1.).into();
        style.size.height = if self.input.read(cx).multiline {
            px(112.)
        } else {
            window.line_height()
        }
        .into();
        (window.request_layout(style, [], cx), ())
    }

    fn prepaint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&gpui::InspectorElementId>,
        bounds: Bounds<Pixels>,
        _request_layout: &mut Self::RequestLayoutState,
        window: &mut Window,
        cx: &mut App,
    ) -> Self::PrepaintState {
        let input = self.input.read(cx);
        let content = input.content.clone();
        let selected_range = input.selected_range.clone();
        let cursor = input.cursor_offset();
        let style = window.text_style();

        let (display_text, text_color) = if content.is_empty() {
            (input.placeholder.clone(), rgb(theme::p().muted).into())
        } else {
            (
                if input.secret {
                    "*".repeat(content.len()).into()
                } else {
                    content
                },
                style.color,
            )
        };

        // Loaded values and native input can contain newlines even in single-line
        // fields. Flatten only the display, preserving byte offsets and stored prompts.
        let display_text = if !input.multiline && display_text.contains('\n') {
            display_text.replace('\n', " ").into()
        } else {
            display_text
        };
        let font_size = style.font_size.to_pixels(window.rem_size());
        let mut start = 0;
        let parts: Vec<&str> = if input.multiline {
            display_text.split('\n').collect()
        } else {
            vec![&display_text]
        };
        let lines: Vec<_> = parts
            .into_iter()
            .map(|text| {
                let run = TextRun {
                    len: text.len(),
                    font: style.font(),
                    color: text_color,
                    background_color: None,
                    underline: None,
                    strikethrough: None,
                };
                let mut runs = vec![run.clone()];
                if let Some(marked) = input.marked_range.as_ref() {
                    let from = marked.start.saturating_sub(start).min(text.len());
                    let to = marked.end.saturating_sub(start).min(text.len());
                    if from < to {
                        runs = vec![
                            TextRun {
                                len: from,
                                ..run.clone()
                            },
                            TextRun {
                                len: to - from,
                                underline: Some(UnderlineStyle {
                                    color: Some(run.color),
                                    thickness: px(1.),
                                    wavy: false,
                                }),
                                ..run.clone()
                            },
                            TextRun {
                                len: text.len() - to,
                                ..run
                            },
                        ]
                        .into_iter()
                        .filter(|run| run.len > 0)
                        .collect();
                    }
                }
                let line =
                    window
                        .text_system()
                        .shape_line(text.to_owned().into(), font_size, &runs, None);
                let offset = start;
                start += text.len() + 1;
                (offset, line)
            })
            .collect();
        let row = lines
            .iter()
            .rposition(|(start, _)| *start <= cursor)
            .unwrap_or(0);
        let cursor_x = lines.get(row).map_or(px(0.), |(start, line)| {
            line.x_for_index(cursor.saturating_sub(*start))
        });
        let scroll_x = if cursor_x < input.scroll_x {
            cursor_x
        } else if cursor_x > input.scroll_x + bounds.size.width - px(8.) {
            (cursor_x - bounds.size.width + px(8.)).max(px(0.))
        } else {
            input.scroll_x
        };
        let line_height = window.line_height();
        let cursor_y = line_height * row as f32;
        let scroll_y = if cursor_y < input.scroll_y {
            cursor_y
        } else if cursor_y + line_height > input.scroll_y + bounds.size.height {
            (cursor_y + line_height - bounds.size.height).max(px(0.))
        } else {
            input.scroll_y
        };
        let cursor = selected_range.is_empty().then(|| {
            fill(
                Bounds::new(
                    point(
                        bounds.left() + cursor_x - scroll_x,
                        bounds.top() + cursor_y - scroll_y,
                    ),
                    size(px(2.), line_height),
                ),
                rgb(theme::p().ring),
            )
        });
        let selections = lines
            .iter()
            .enumerate()
            .filter_map(|(row, (start, line))| {
                let end = lines
                    .get(row + 1)
                    .map_or(input.content.len(), |(start, _)| start.saturating_sub(1));
                if selected_range.start > end
                    || selected_range.end <= *start
                    || selected_range.is_empty()
                {
                    return None;
                }
                let from = selected_range.start.saturating_sub(*start);
                let to = selected_range.end.min(end).saturating_sub(*start);
                Some(fill(
                    Bounds::new(
                        point(
                            bounds.left() + line.x_for_index(from) - scroll_x,
                            bounds.top() + line_height * row as f32 - scroll_y,
                        ),
                        size(
                            (line.x_for_index(to) - line.x_for_index(from)).max(px(2.)),
                            line_height,
                        ),
                    ),
                    rgba((theme::p().ring << 8) | 0x40),
                ))
            })
            .collect();
        PrepaintState {
            scroll_x,
            scroll_y,
            lines,
            cursor,
            selections,
        }
    }

    fn paint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&gpui::InspectorElementId>,
        bounds: Bounds<Pixels>,
        _request_layout: &mut Self::RequestLayoutState,
        prepaint: &mut Self::PrepaintState,
        window: &mut Window,
        cx: &mut App,
    ) {
        let focus_handle = self.input.read(cx).focus_handle.clone();
        window.handle_input(
            &focus_handle,
            ElementInputHandler::new(bounds, self.input.clone()),
            cx,
        );
        for selection in prepaint.selections.drain(..) {
            window.paint_quad(selection);
        }
        let lines = std::mem::take(&mut prepaint.lines);
        for (row, (_, line)) in lines.iter().enumerate() {
            let _ = line.paint(
                point(
                    bounds.left() - prepaint.scroll_x,
                    bounds.top() + window.line_height() * row as f32 - prepaint.scroll_y,
                ),
                window.line_height(),
                gpui::TextAlign::Left,
                None,
                window,
                cx,
            );
        }
        if focus_handle.is_focused(window)
            && let Some(cursor) = prepaint.cursor.take()
        {
            window.paint_quad(cursor);
        }
        self.input.update(cx, |input, _| {
            input.last_layout = lines.first().map(|(_, line)| line.clone());
            input.last_lines = lines;
            input.last_bounds = Some(bounds);
            input.scroll_x = prepaint.scroll_x;
            input.scroll_y = prepaint.scroll_y;
        });
    }
}

impl Render for TextInput {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .id("text-input")
            .when_some(self.accessibility_id.clone(), |d, id| {
                d.role(if self.secret {
                    gpui::Role::PasswordInput
                } else {
                    gpui::Role::TextInput
                })
                .accessibility_id(id)
                .aria_label(self.placeholder.clone())
                .when(self.multiline, |d| d.aria_description("Multiline instructions. Enter inserts a new line; Command-Enter saves the action."))
                .aria_value(if self.secret {
                    SharedString::default()
                } else {
                    self.content.clone()
                })
                .on_a11y_action(gpui::AccessibleAction::SetValue, {
                    let input = cx.entity().downgrade();
                    move |data, _, cx| {
                        if let Some(gpui::accesskit::ActionData::Value(value)) = data {
                            let _ = input.update(cx, |this, cx| {
                                this.set(value.to_string(), cx);
                            });
                        }
                    }
                })
            })
            .flex()
            .w_full()
            .min_w_0()
            .h(px(if self.multiline { 120. } else { 38. }))
            .min_h(px(if self.multiline { 120. } else { 38. }))
            .flex_shrink_0()
            .key_context("TextInput")
            .track_focus(&self.focus_handle(cx))
            .on_key_down(cx.listener(|this, event: &gpui::KeyDownEvent, window, cx| {
                if this.multiline
                    && event.keystroke.key == "enter"
                    && !event.keystroke.modifiers.platform
                    && !event.keystroke.modifiers.control
                    && !event.keystroke.modifiers.alt
                {
                    if !this.is_composing() {
                        this.replace_text_in_range(None, "\n", window, cx);
                    }
                    cx.stop_propagation();
                }
                if this.multiline
                    && matches!(event.keystroke.key.as_str(), "up" | "down")
                    && !event.keystroke.modifiers.platform
                    && !event.keystroke.modifiers.control
                    && !event.keystroke.modifiers.alt
                {
                    let cursor = this.cursor_offset();
                    let row = this
                        .last_lines
                        .iter()
                        .rposition(|(start, _)| *start <= cursor)
                        .unwrap_or(0);
                    let next = if event.keystroke.key == "up" {
                        row.saturating_sub(1)
                    } else {
                        (row + 1).min(this.last_lines.len().saturating_sub(1))
                    };
                    if let (Some((start, line)), Some((next_start, next_line))) =
                        (this.last_lines.get(row), this.last_lines.get(next))
                    {
                        let index = this.content_boundary(
                            next_start
                                + next_line.closest_index_for_x(line.x_for_index(cursor - start)),
                        );
                        if event.keystroke.modifiers.shift {
                            this.select_to(index, cx);
                        } else {
                            this.move_to(index, cx);
                        }
                    }
                    cx.stop_propagation();
                }
            }))
            .cursor(CursorStyle::IBeam)
            .on_action(cx.listener(Self::backspace))
            .on_action(cx.listener(Self::delete))
            .on_action(cx.listener(Self::left))
            .on_action(cx.listener(Self::right))
            .on_action(cx.listener(Self::select_left))
            .on_action(cx.listener(Self::select_right))
            .on_action(cx.listener(Self::select_all))
            .on_action(cx.listener(Self::home))
            .on_action(cx.listener(Self::end))
            .on_action(cx.listener(Self::show_character_palette))
            .on_action(cx.listener(Self::paste))
            .on_action(cx.listener(Self::cut))
            .on_action(cx.listener(Self::copy))
            .on_mouse_down(MouseButton::Left, cx.listener(Self::on_mouse_down))
            .on_mouse_up(MouseButton::Left, cx.listener(Self::on_mouse_up))
            .on_mouse_up_out(MouseButton::Left, cx.listener(Self::on_mouse_up))
            .on_mouse_move(cx.listener(Self::on_mouse_move))
            .overflow_hidden()
            .map(|d| {
                let p = theme::p();
                d.rounded(px(4.))
                    .border_1()
                    .border_color(rgba(p.input_border))
                    .bg(rgb(p.field))
                    .shadow(vec![
                        theme::shadow(0., 1., 0., 0., p.edge_highlight),
                        theme::shadow(0., 1., 2., 0., 0x0000001f).inset(),
                    ])
                    .focus(|s| s.border_color(rgb(p.ring)))
                    .text_color(rgb(p.foreground))
                    .text_size(px(13.))
            })
            .line_height(px(28.))
            .child(
                div()
                    .h(px(if self.multiline { 120. } else { 38. }))
                    .w_full()
                    .py(px(4.))
                    .px(px(8.))
                    .child(TextElement { input: cx.entity() }),
            )
    }
}

impl Focusable for TextInput {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::{TextInput, byte_offset_for_utf16, normalize_pasted_text};
    use gpui::{AppContext, TestAppContext};

    #[gpui::test]
    fn single_line_renders_saved_newlines_without_changing_content(cx: &mut TestAppContext) {
        let value = "First é\nSecond 😀\n";
        let (input, cx) = cx.add_window_view(|_, cx| {
            let mut input = TextInput::new("Prompt", false, cx);
            input.set(value, cx);
            input
        });
        cx.read_entity(&input, |input, _| {
            assert_eq!(input.content.as_ref(), value);
            assert_eq!(input.selected_range, value.len()..value.len());
            assert_eq!(input.last_lines.len(), 1);
            assert_eq!(input.last_lines[0].1.text.as_ref(), "First é Second 😀 ");
        });
    }

    #[gpui::test]
    fn single_line_renders_newlines_in_placeholder(cx: &mut TestAppContext) {
        let (input, cx) = cx.add_window_view(|_, cx| TextInput::new("First\nSecond", false, cx));
        cx.read_entity(&input, |input, _| {
            assert!(input.content.is_empty());
            assert_eq!(input.last_lines[0].1.text.as_ref(), "First Second");
        });
    }

    #[gpui::test]
    fn multiline_render_preserves_rows_and_utf8_offsets(cx: &mut TestAppContext) {
        let value = "First é\nSecond 😀\n";
        let (input, cx) = cx.add_window_view(|_, cx| {
            let mut input = TextInput::multiline("Prompt", cx);
            input.set(value, cx);
            input
        });
        cx.read_entity(&input, |input, _| {
            assert_eq!(input.content.as_ref(), value);
            assert_eq!(
                input
                    .last_lines
                    .iter()
                    .map(|(offset, line)| (*offset, line.text.as_ref()))
                    .collect::<Vec<_>>(),
                vec![
                    (0, "First é"),
                    ("First é\n".len(), "Second 😀"),
                    (value.len(), "")
                ]
            );
        });
    }

    #[test]
    fn multiline_paste_preserves_lines_and_normalizes_crlf() {
        assert_eq!(
            normalize_pasted_text("First\r\nSecond\nThird", true),
            "First\nSecond\nThird"
        );
    }
    #[test]
    fn single_line_paste_retains_existing_newline_normalization() {
        assert_eq!(
            normalize_pasted_text("First\r\nSecond\nThird", false),
            "First\r Second Third"
        );
    }
    #[test]
    fn utf16_offsets_do_not_split_surrogates_or_utf8() {
        let text = "a😀é";
        assert_eq!(byte_offset_for_utf16(text, 0), 0);
        assert_eq!(byte_offset_for_utf16(text, 1), 1);
        assert_eq!(byte_offset_for_utf16(text, 2), 1);
        assert_eq!(byte_offset_for_utf16(text, 3), 5);
        assert_eq!(byte_offset_for_utf16(text, 4), 7);
        assert_eq!(byte_offset_for_utf16(text, 99), 7);
    }
    #[test]
    fn ime_selection_is_relative_to_inserted_text() {
        let inserted = "日本😀";
        assert_eq!(byte_offset_for_utf16(inserted, 2), 6);
        assert_eq!(byte_offset_for_utf16(inserted, 4), 10);
    }
}
