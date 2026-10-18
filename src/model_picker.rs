use crate::{input::TextInput, notifications::Notification, theme};
use gpui::{prelude::*, *};

/// A bounded, searchable select; options always come from the live Gateway catalog.
pub struct ModelPicker {
    pub selected: String,
    pub models: Vec<String>,
    pub loading: bool,
    open: bool,
    include_default: bool,
    mouse_dismissed: bool,
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
            include_default: false,
            mouse_dismissed: false,
            highlighted: 0,
            search,
            focus: cx.focus_handle(),
            scroll: ScrollHandle::new(),
        }
    }
    pub fn with_default(mut self) -> Self {
        self.include_default = true;
        self.focus = self.focus.tab_stop(true);
        self
    }
    pub fn take_mouse_dismissal(&mut self) -> bool {
        std::mem::take(&mut self.mouse_dismissed)
    }
    pub fn close(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let was_open = self.open;
        self.open = false;
        if was_open {
            window.focus(&self.focus, cx);
            cx.notify();
        }
        was_open
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
        std::iter::once("")
            .filter(|_| self.include_default)
            .chain(self.models.iter().map(String::as_str))
            .filter(|model| {
                matches_query(
                    if model.is_empty() {
                        "Default model"
                    } else {
                        model
                    },
                    query,
                )
            })
            .map(str::to_owned)
            .collect()
    }
    fn select(&mut self, model: String, window: &mut Window, cx: &mut Context<Self>) {
        self.selected = model;
        self.open = false;
        window.focus(&self.focus, cx);
        cx.notify();
    }
    fn key_down(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        if event.keystroke.modifiers.platform
            || event.keystroke.modifiers.control
            || event.keystroke.modifiers.alt
        {
            return;
        }
        if event.is_held && matches!(event.keystroke.key.as_str(), "enter" | "space") {
            cx.stop_propagation();
            return;
        }
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
            .on_mouse_down_out(cx.listener(|this, _, window, cx| {
                this.mouse_dismissed = this.close(window, cx);
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
                    .debug_selector(|| "model-select".into())
                    .role(Role::Button)
                    .aria_label("Choose model")
                    .aria_expanded(self.open)
                    .accessibility_id("wiesel.model.select")
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
                    .border_color(rgba(theme::p().input_border))
                    .bg(rgb(theme::p().card))
                    .cursor_pointer()
                    .hover(|s| s.bg(rgb(theme::p().accent)))
                    .child(if selected.is_empty() {
                        if self.include_default {
                            "Default model · follows app setting"
                        } else {
                            "Select a model…"
                        }
                        .to_owned()
                    } else {
                        selected
                    })
                    .child(if self.open { "▴" } else { "▾" })
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.open = !this.open;
                        this.mouse_dismissed = false;
                        if !this.open {
                            window.focus(&this.focus, cx);
                        }
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
                        .border_color(rgba(theme::p().input_border))
                        .bg(rgb(theme::p().card))
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
                                        .debug_selector(move || format!("model-option-{index}"))
                                        .role(Role::Button)
                                        .aria_selected(model == self.selected)
                                        .aria_label(if model.is_empty() {
                                            "Default model".to_owned()
                                        } else {
                                            model.clone()
                                        })
                                        .min_h(px(34.))
                                        .flex_shrink_0()
                                        .px_3()
                                        .py_2()
                                        .text_size(px(13.))
                                        .cursor_pointer()
                                        .rounded_sm()
                                        .bg(rgb(if index == self.highlighted {
                                            theme::p().accent
                                        } else {
                                            theme::p().card
                                        }))
                                        .hover(|s| s.bg(rgb(theme::p().accent)))
                                        .child(format!(
                                            "{}{}",
                                            if model == self.selected { "✓ " } else { "" },
                                            if model.is_empty() {
                                                "Default model · follows app setting"
                                            } else {
                                                &model
                                            }
                                        ))
                                        .on_click(cx.listener(move |this, _, window, cx| {
                                            this.select(model.clone(), window, cx)
                                        }))
                                })),
                        )
                        .child(
                            div()
                                .text_size(px(11.))
                                .text_color(rgb(theme::p().muted))
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
impl Focusable for ModelPicker {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
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
