//! Searchable model select grouped by provider: local Ollama models first, then the
//! Wiesel gateway catalog. Action editors additionally offer the tiers themselves.
use crate::{
    input::TextInput,
    model_picker::matches_query,
    notifications::Notification,
    settings::{ActionModel, ModelChoice, Provider, Tier},
    theme,
};
use gpui::{prelude::*, *};

/// Emitted only for user choices, never for catalog refreshes.
pub struct Picked(pub Option<ActionModel>);

/// Catalog of one provider. `note` explains why the provider is unavailable.
#[derive(Clone, Default)]
pub struct Catalog {
    pub models: Vec<String>,
    pub loading: bool,
    pub note: Option<&'static str>,
}

pub struct TierPicker {
    pub selected: Option<ActionModel>,
    pub ollama: Catalog,
    pub wiesel: Catalog,
    id: SharedString,
    /// Offer Swift/Sharp/Sage instead of "Not set".
    tiers: bool,
    open: bool,
    mouse_dismissed: bool,
    highlighted: usize,
    search: Entity<TextInput>,
    focus: FocusHandle,
    scroll: ScrollHandle,
}
impl EventEmitter<Notification> for TierPicker {}
impl EventEmitter<Picked> for TierPicker {}

/// One rendered line of the open list.
enum Line {
    Header(&'static str, Option<&'static str>),
    Option(Option<ActionModel>),
}

fn provider_label(provider: Provider) -> &'static str {
    match provider {
        Provider::Ollama => "Ollama",
        Provider::Wiesel => "Wiesel",
    }
}

impl TierPicker {
    pub fn new(
        id: impl Into<SharedString>,
        selected: Option<ActionModel>,
        cx: &mut Context<Self>,
    ) -> Self {
        let search = cx.new(|cx| TextInput::new("Search models…", false, cx));
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
            ollama: Catalog::default(),
            wiesel: Catalog::default(),
            id: id.into(),
            tiers: false,
            open: false,
            mouse_dismissed: false,
            highlighted: 0,
            search,
            focus: cx.focus_handle().tab_stop(true),
            scroll: ScrollHandle::new(),
        }
    }
    pub fn with_tiers(mut self) -> Self {
        self.tiers = true;
        self
    }
    /// Whether the last outside mousedown closed the list, consuming that gesture.
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
    pub fn catalog_mut(&mut self, provider: Provider) -> &mut Catalog {
        match provider {
            Provider::Ollama => &mut self.ollama,
            Provider::Wiesel => &mut self.wiesel,
        }
    }
    /// The selection is set but its provider no longer lists it.
    pub fn missing(&self) -> bool {
        self.selected
            .as_ref()
            .and_then(ActionModel::choice)
            .is_some_and(|choice| {
                let catalog = match choice.provider {
                    Provider::Ollama => &self.ollama,
                    Provider::Wiesel => &self.wiesel,
                };
                !catalog.loading && catalog.note.is_none() && !catalog.models.contains(&choice.id)
            })
    }
    fn lines(&self, cx: &App) -> Vec<Line> {
        let query = &self.search.read(cx).content;
        let mut lines = Vec::new();
        if self.tiers {
            let matches: Vec<_> = Tier::ALL
                .into_iter()
                .filter(|tier| matches_query(tier.name(), query) || matches_query("Tiers", query))
                .collect();
            lines.push(Line::Header(
                "Tiers",
                matches.is_empty().then_some("No matches."),
            ));
            lines.extend(
                matches
                    .into_iter()
                    .map(|tier| Line::Option(Some(ActionModel::Tier(tier)))),
            );
        } else if matches_query("Not set", query) {
            lines.push(Line::Option(None));
        }
        for (provider, catalog) in [
            (Provider::Ollama, &self.ollama),
            (Provider::Wiesel, &self.wiesel),
        ] {
            let matches: Vec<_> = catalog
                .models
                .iter()
                .filter(|model| {
                    matches_query(model, query) || matches_query(provider_label(provider), query)
                })
                .collect();
            let note = if catalog.loading {
                Some("Loading…")
            } else if catalog.models.is_empty() {
                Some(catalog.note.unwrap_or("No models available."))
            } else if matches.is_empty() {
                Some("No matches.")
            } else {
                None
            };
            lines.push(Line::Header(provider_label(provider), note));
            lines.extend(matches.into_iter().map(|model| {
                Line::Option(Some(ActionModel::Model(ModelChoice {
                    provider,
                    id: model.clone(),
                })))
            }));
        }
        lines
    }
    fn options(&self, cx: &App) -> Vec<Option<ActionModel>> {
        self.lines(cx)
            .into_iter()
            .filter_map(|line| match line {
                Line::Option(choice) => Some(choice),
                Line::Header(..) => None,
            })
            .collect()
    }
    /// Child index of the nth option, counting section headers.
    fn line_index(&self, option: usize, cx: &App) -> usize {
        self.lines(cx)
            .iter()
            .enumerate()
            .filter(|(_, line)| matches!(line, Line::Option(_)))
            .nth(option)
            .map_or(0, |(index, _)| index)
    }
    fn open_list(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.open = true;
        self.mouse_dismissed = false;
        self.highlighted = 0;
        self.search.update(cx, |input, cx| input.set("", cx));
        window.focus(&self.search.focus_handle(cx), cx);
        cx.notify();
    }
    fn select(&mut self, choice: Option<ActionModel>, window: &mut Window, cx: &mut Context<Self>) {
        self.open = false;
        window.focus(&self.focus, cx);
        if choice != self.selected {
            self.selected = choice.clone();
            cx.emit(Picked(choice));
        }
        cx.notify();
    }
    fn key_down(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let modifiers = &event.keystroke.modifiers;
        if modifiers.platform || modifiers.control || modifiers.alt {
            return;
        }
        if event.is_held && matches!(event.keystroke.key.as_str(), "enter" | "space") {
            cx.stop_propagation();
            return;
        }
        if !self.open {
            if matches!(event.keystroke.key.as_str(), "enter" | "space" | "down") {
                self.open_list(window, cx);
                cx.stop_propagation();
            }
            return;
        }
        let options = self.options(cx);
        match event.keystroke.key.as_str() {
            "escape" => {
                self.open = false;
                window.focus(&self.focus, cx);
            }
            "down" => {
                if !options.is_empty() {
                    self.highlighted = (self.highlighted + 1).min(options.len() - 1);
                }
                self.scroll
                    .scroll_to_item(self.line_index(self.highlighted, cx));
            }
            "up" => {
                self.highlighted = self.highlighted.saturating_sub(1);
                self.scroll
                    .scroll_to_item(self.line_index(self.highlighted, cx));
            }
            "enter" => {
                if let Some(choice) = options.get(self.highlighted) {
                    self.select(choice.clone(), window, cx);
                }
            }
            _ => return,
        }
        cx.stop_propagation();
        cx.notify();
    }
}

impl Render for TierPicker {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let palette = theme::p();
        let lines = self.lines(cx);
        let total = self.ollama.models.len() + self.wiesel.models.len();
        let id = self.id.clone();
        let mut option_index = 0;
        let rows: Vec<AnyElement> = lines
            .into_iter()
            .map(|line| match line {
                Line::Header(title, note) => div()
                    .flex_shrink_0()
                    .px_3()
                    .pt(px(10.))
                    .pb(px(4.))
                    .flex()
                    .items_center()
                    .justify_between()
                    .child(theme::silkscreen(title, palette.muted))
                    .when_some(note, |d, note| {
                        d.child(
                            div()
                                .text_size(px(11.5))
                                .text_color(rgb(palette.muted))
                                .child(note),
                        )
                    })
                    .into_any_element(),
                Line::Option(choice) => {
                    let index = option_index;
                    option_index += 1;
                    let checked = choice == self.selected;
                    let label = choice
                        .as_ref()
                        .map_or("Not set", ActionModel::label)
                        .to_owned();
                    let hint = match &choice {
                        Some(ActionModel::Tier(tier)) => Some(tier.hint()),
                        _ => None,
                    };
                    let selector = format!("{id}-option-{index}");
                    div()
                        .id(SharedString::from(selector.clone()))
                        .debug_selector(move || selector.clone())
                        .role(Role::Button)
                        .aria_selected(checked)
                        .aria_label(label.clone())
                        .min_h(px(32.))
                        .flex_shrink_0()
                        .px_3()
                        .py(px(7.))
                        .text_size(px(13.))
                        .cursor_pointer()
                        .rounded_sm()
                        .bg(rgb(if index == self.highlighted {
                            palette.accent
                        } else {
                            palette.card
                        }))
                        .hover(|s| s.bg(rgb(theme::p().accent)))
                        .when(choice.is_none(), |d| d.text_color(rgb(palette.muted)))
                        .flex()
                        .items_center()
                        .justify_between()
                        .gap(px(8.))
                        .child(format!("{}{label}", if checked { "✓ " } else { "" }))
                        .when_some(hint, |d, hint| {
                            d.child(
                                div()
                                    .min_w_0()
                                    .truncate()
                                    .text_size(px(11.5))
                                    .text_color(rgb(palette.muted))
                                    .child(hint),
                            )
                        })
                        .on_click(cx.listener(move |this, _, window, cx| {
                            this.select(choice.clone(), window, cx)
                        }))
                        .into_any_element()
                }
            })
            .collect();
        let selector = format!("{id}-select");
        div()
            .id(id.clone())
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
                    .id(SharedString::from(selector.clone()))
                    .debug_selector(move || selector.clone())
                    .role(Role::Button)
                    .aria_label("Choose model")
                    .aria_expanded(self.open)
                    .accessibility_id(SharedString::from(format!("wiesel.{id}.select")))
                    .w_full()
                    .h(px(32.))
                    .flex_shrink_0()
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap(px(8.))
                    .px(px(10.))
                    .rounded(px(4.))
                    .border_1()
                    .border_color(rgba(palette.input_border))
                    .bg(rgb(palette.field))
                    .text_size(px(13.))
                    .cursor_pointer()
                    .hover(|s| s.bg(rgb(theme::p().accent)))
                    .child(match &self.selected {
                        Some(model) => div()
                            .min_w_0()
                            .flex()
                            .items_center()
                            .gap(px(8.))
                            .child(
                                div()
                                    .flex_shrink_0()
                                    .px(px(5.))
                                    .py(px(1.))
                                    .rounded(px(3.))
                                    .border_1()
                                    .border_color(rgba(palette.border))
                                    .child(theme::silkscreen(
                                        match model {
                                            ActionModel::Tier(_) => "Tier",
                                            ActionModel::Model(choice) => {
                                                provider_label(choice.provider)
                                            }
                                        },
                                        palette.muted,
                                    )),
                            )
                            .child(div().min_w_0().truncate().child(model.label().to_owned())),
                        None => div().text_color(rgb(palette.muted)).child("Not set"),
                    })
                    .child(
                        div()
                            .flex_shrink_0()
                            .text_color(rgb(palette.muted))
                            .child(if self.open { "▴" } else { "▾" }),
                    )
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.mouse_dismissed = false;
                        if this.open {
                            this.close(window, cx);
                        } else {
                            this.open_list(window, cx);
                        }
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
                        .border_color(rgba(palette.input_border))
                        .bg(rgb(palette.card))
                        .child(self.search.clone())
                        .child(
                            div()
                                .id(SharedString::from(format!("{id}-options")))
                                .h(px(220.))
                                .flex_shrink_0()
                                .overflow_y_scroll()
                                .track_scroll(&self.scroll)
                                .children(rows),
                        )
                        .child(
                            div()
                                .text_size(px(11.))
                                .text_color(rgb(palette.muted))
                                .child(format!(
                                    "{total} models · ↑↓ navigate · Enter selects · Esc closes"
                                )),
                        ),
                )
            })
    }
}

impl Focusable for TierPicker {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::{ActionModel, Catalog, ModelChoice, TierPicker};
    use gpui::{AppContext as _, TestAppContext};

    #[gpui::test]
    fn only_reachable_providers_flag_a_missing_selection(cx: &mut TestAppContext) {
        let gone = ActionModel::Model(ModelChoice::ollama("gone"));
        let picker = cx.new(|cx| TierPicker::new("t", Some(gone), cx));
        picker.update(cx, |picker, _| {
            // Ollama down: the provider is the problem, not the model.
            picker.ollama.note = Some("Ollama isn't running.");
            assert!(!picker.missing());
            picker.ollama = Catalog {
                models: vec!["llama3".into()],
                ..Default::default()
            };
            assert!(picker.missing());
            picker.selected = Some(ActionModel::Model(ModelChoice::ollama("llama3")));
            assert!(!picker.missing());
        });
    }
}
