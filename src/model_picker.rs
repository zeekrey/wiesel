use crate::{input::TextInput, notifications::Notification, theme};
use gpui::{prelude::*, *};

/// A bounded, searchable select; options always come from the live Gateway catalog.
pub struct ModelPicker {
    pub selected: String,
    pub models: Vec<String>,
    pub loading: bool,
    open: bool,
    highlighted: usize,
    search: Entity<TextInput>,
    focus: FocusHandle,
    scroll: ScrollHandle,
}
fn matches_query(model: &str, query: &str) -> bool {
    model.to_lowercase().contains(&query.trim().to_lowercase())
}
impl EventEmitter<Notification> for ModelPicker {}

impl ModelPicker {
    pub fn new(selected: String, cx: &mut Context<Self>) -> Self {
        let search = cx.new(|cx| TextInput::new("Search models or providers…", false, cx));
        cx.subscribe(&search, |_, _, notification: &Notification, cx| {
            cx.emit(notification.clone());
        })
        .detach();
        cx.observe(&search, |this, _, cx| {
            this.highlighted = 0;
            this.scroll.set_offset(point(px(0.), px(0.)));
            cx.notify();
        })
        .detach();
        Self {
            selected,
            models: vec![],
            loading: false,
            open: false,
            highlighted: 0,
            search,
            focus: cx.focus_handle(),
            scroll: ScrollHandle::new(),
        }
    }
    pub fn begin_load(&mut self, cx: &mut Context<Self>) {
        self.loading = true;
        cx.notify();
    }
    pub fn finish_load(&mut self, result: anyhow::Result<Vec<String>>, cx: &mut Context<Self>) {
        self.loading = false;
        // Failures are reported by Wiesel's notification surface; retain the last catalog.
        if let Ok(models) = result {
            self.models = models;
        }
        self.highlighted = 0;
        cx.notify();
    }
    fn filtered(&self, cx: &App) -> Vec<String> {
        let query = &self.search.read(cx).content;
        self.models
            .iter()
            .filter(|model| matches_query(model, query))
            .cloned()
            .collect()
    }
    fn select(&mut self, model: String, window: &mut Window, cx: &mut Context<Self>) {
        self.selected = model;
        self.open = false;
        window.focus(&self.focus, cx);
        cx.notify();
    }
    fn key_down(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        if !self.open {
            if matches!(event.keystroke.key.as_str(), "enter" | "space" | "down") {
                self.open = true;
                self.search.update(cx, |input, cx| input.set("", cx));
                window.focus(&self.search.focus_handle(cx), cx);
                cx.stop_propagation();
                cx.notify();
            }
            return;
        }
        let options = self.filtered(cx);
        match event.keystroke.key.as_str() {
            "escape" => {
                self.open = false;
                window.focus(&self.focus, cx);
            }
            "down" => {
                if !options.is_empty() {
                    self.highlighted = (self.highlighted + 1).min(options.len() - 1);
                }
                self.scroll.scroll_to_item(self.highlighted);
            }
            "up" => {
                self.highlighted = self.highlighted.saturating_sub(1);
                self.scroll.scroll_to_item(self.highlighted);
            }
            "enter" => {
                if let Some(model) = options.get(self.highlighted) {
                    self.select(model.clone(), window, cx);
                }
            }
            _ => return,
        }
        cx.stop_propagation();
        cx.notify();
    }
}
impl Render for ModelPicker {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let options = self.filtered(cx);
        let selected = self.selected.clone();
        div()
            .id("model-picker")
            .on_mouse_down_out(cx.listener(|this, _, _, cx| {
                if this.open {
                    this.open = false;
                    cx.notify();
                }
            }))
            .w_full()
            .flex_shrink_0()
            .flex()
            .flex_col()
            .gap_2()
            .track_focus(&self.focus)
            .capture_key_down(cx.listener(Self::key_down))
            .child(
                div()
                    .id("model-select")
                    .w_full()
                    .min_h(px(40.))
                    .flex_shrink_0()
                    .flex()
                    .items_center()
                    .justify_between()
                    .px_3()
                    .py_2()
                    .rounded_md()
                    .border_1()
                    .border_color(rgba(theme::INPUT_BORDER))
                    .bg(rgb(theme::CARD))
                    .cursor_pointer()
                    .hover(|s| s.bg(rgb(theme::ACCENT)))
                    .child(if selected.is_empty() {
                        "Select a model…".to_owned()
                    } else {
                        selected
                    })
                    .child(if self.open { "▴" } else { "▾" })
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.open = !this.open;
                        if this.open {
                            this.highlighted = 0;
                            this.search.update(cx, |input, cx| input.set("", cx));
                            window.focus(&this.search.focus_handle(cx), cx);
                        }
                        cx.notify();
                    })),
            )
            .when(self.open, |d| {
                d.child(
                    div()
                        .w_full()
                        .flex_shrink_0()
                        .flex()
                        .flex_col()
                        .gap_2()
                        .p_2()
                        .rounded_md()
                        .border_1()
                        .border_color(rgba(theme::INPUT_BORDER))
                        .bg(rgb(theme::CARD))
                        .child(self.search.clone())
                        .child(
                            div()
                                .id("model-options")
                                .h(px(220.))
                                .min_h(px(220.))
                                .flex_shrink_0()
                                .overflow_y_scroll()
                                .track_scroll(&self.scroll)
                                .when(options.is_empty() && !self.loading, |d| {
                                    d.child(div().p_3().text_size(px(13.)).child(
                                        "No models found. Try another search or Refresh models.",
                                    ))
                                })
                                .children(options.iter().enumerate().map(|(index, model)| {
                                    let model = model.clone();
                                    div()
                                        .id(("model-option", index))
                                        .min_h(px(34.))
                                        .flex_shrink_0()
                                        .px_3()
                                        .py_2()
                                        .text_size(px(13.))
                                        .cursor_pointer()
                                        .rounded_sm()
                                        .bg(rgb(if index == self.highlighted {
                                            theme::ACCENT
                                        } else {
                                            theme::CARD
                                        }))
                                        .hover(|s| s.bg(rgb(theme::ACCENT)))
                                        .child(format!(
                                            "{}{}",
                                            if model == self.selected { "✓ " } else { "" },
                                            model
                                        ))
                                        .on_click(cx.listener(move |this, _, window, cx| {
                                            this.select(model.clone(), window, cx)
                                        }))
                                })),
                        )
                        .child(
                            div()
                                .text_size(px(11.))
                                .text_color(rgb(theme::MUTED))
                                .child(format!(
                                    "{} of {} models · ↑↓ navigate · Enter selects · Esc closes",
                                    options.len(),
                                    self.models.len()
                                )),
                        ),
                )
            })
    }
}
#[cfg(test)]
mod tests {
    use super::matches_query;
    #[test]
    fn searches_provider_and_model_case_insensitively() {
        assert!(matches_query("openai/gpt-4o-mini", " OPENAI "));
        assert!(matches_query("anthropic/claude-sonnet", "SONNET"));
        assert!(matches_query("openai/gpt-4o-mini", ""));
        assert!(!matches_query("openai/gpt-4o-mini", "gemini"));
    }
}
