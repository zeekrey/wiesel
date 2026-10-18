//! Settings screen: grouped cards of "label ↔ control" rows. Copy is deliberately
//! minimal; transient detail belongs in the status bar, not on the page.
use super::*;
use gpui::SharedString;

const ROW_GAP: f32 = 8.;
const CONTROL_HEIGHT: f32 = 28.;

/// `Super+Shift+Space` → `⌘ ⇧ Space`, with modifiers in macOS menu order.
pub(super) fn display_hotkey(text: &str) -> String {
    const ORDER: [(&str, &str); 4] = [
        ("Control", "⌃"),
        ("Alt", "⌥"),
        ("Shift", "⇧"),
        ("Super", "⌘"),
    ];
    let parts: Vec<&str> = text.split('+').map(str::trim).collect();
    let mut out: Vec<String> = ORDER
        .iter()
        .filter(|(name, _)| parts.iter().any(|part| part.eq_ignore_ascii_case(name)))
        .map(|(_, symbol)| (*symbol).to_owned())
        .collect();
    out.extend(
        parts
            .iter()
            .filter(|part| {
                !ORDER
                    .iter()
                    .any(|(name, _)| part.eq_ignore_ascii_case(name))
            })
            .map(|part| match *part {
                "Enter" => "↩".to_owned(),
                "Tab" => "⇥".to_owned(),
                key => key
                    .strip_prefix("Key")
                    .or_else(|| key.strip_prefix("Digit"))
                    .unwrap_or(key)
                    .to_owned(),
            }),
    );
    out.join(" ")
}

/// Session lifetime in the coarsest readable unit.
fn duration(hours: u64) -> String {
    match hours {
        0..=1 => "1 hour".into(),
        2..=47 => format!("{hours} hours"),
        _ => format!("{} days", hours / 24),
    }
}

/// `12345` → `12,345`.
fn grouped(value: i64) -> String {
    let digits = value.unsigned_abs().to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3 + 1);
    if value < 0 {
        out.push('-');
    }
    for (index, ch) in digits.chars().enumerate() {
        if index > 0 && (digits.len() - index).is_multiple_of(3) {
            out.push(',');
        }
        out.push(ch);
    }
    out
}

fn section(title: &'static str, rows: Vec<Stateful<Div>>) -> Div {
    let palette = theme::p();
    let count = rows.len();
    div()
        .flex()
        .flex_col()
        .gap(px(8.))
        .child(
            div()
                .pl(px(2.))
                .child(theme::silkscreen(title, palette.muted)),
        )
        .child(
            div()
                .id(title)
                .role(Role::Group)
                .aria_label(title)
                .flex()
                .flex_col()
                .rounded(px(6.))
                .bg(rgb(palette.card))
                .border_1()
                .border_color(rgba(palette.border))
                .shadow(vec![theme::shadow(0., 1., 0., 0., palette.edge_highlight)])
                .children(rows.into_iter().enumerate().map(move |(index, row)| {
                    // Inset hairlines between rows, none after the last.
                    row.when(index + 1 < count, |row| {
                        row.border_b_1().border_color(rgba(palette.border))
                    })
                })),
        )
}

/// One setting: title (and optional one-line hint) on the left, control on the right.
fn row(
    title: &'static str,
    hint: Option<SharedString>,
    control: impl IntoElement,
) -> Stateful<Div> {
    let palette = theme::p();
    div()
        .id(SharedString::from(format!("settings-row-{title}")))
        .mx(px(14.))
        .min_h(px(48.))
        .py(px(10.))
        .flex()
        .items_center()
        .justify_between()
        .gap(px(16.))
        .child(
            div()
                .flex_1()
                .min_w_0()
                .flex()
                .flex_col()
                .gap(px(2.))
                .child(div().text_size(px(13.)).child(title))
                .when_some(hint, |d, hint| {
                    d.child(
                        div()
                            .text_size(px(11.5))
                            .text_color(rgb(palette.muted))
                            .child(hint),
                    )
                }),
        )
        .child(
            div()
                .flex_shrink_0()
                .flex()
                .items_center()
                .gap(px(ROW_GAP))
                .child(control),
        )
}

/// Read-only value on the right of a row.
fn value(text: impl Into<SharedString>) -> Div {
    div()
        .text_size(px(13.))
        .text_color(rgb(theme::p().muted))
        .child(text.into())
}

/// LED plus a short word, for states that need no action.
fn indicator(color: u32, text: &'static str) -> Div {
    div()
        .flex()
        .items_center()
        .gap(px(8.))
        .child(theme::led(color))
        .child(value(text))
}

impl Wiesel {
    pub(super) fn setup(&self, cx: &mut Context<Self>) -> Stateful<Div> {
        // Keep content at its natural height instead of shrinking rows to fit.
        div()
            .id("settings-scroll")
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .child(
                div()
                    .w_full()
                    .flex()
                    .flex_col()
                    .gap(px(20.))
                    .p(px(20.))
                    .when(self.actions.read_only, |d| {
                        d.child(indicator(
                            theme::LED_AMBER,
                            "Settings file unreadable. Changes are not saved until it is repaired.",
                        ))
                    })
                    .child(section("Account", self.account_rows(cx)))
                    .child(section("General", vec![self.shortcut_row(cx)]))
                    .child(section("Permissions", vec![self.accessibility_row()]))
                    .child(section("Diagnostics", vec![self.diagnostics_row(cx)])),
            )
    }

    fn start_recording(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.recording = true;
        self.notifications
            .info("Press your new shortcut. Esc cancels.");
        window.focus(&self.focus, cx);
        cx.notify();
    }

    fn cancel_recording(&mut self, cx: &mut Context<Self>) {
        self.recording = false;
        self.notifications.info("Shortcut recording cancelled.");
        cx.notify();
    }

    fn account_rows(&self, cx: &mut Context<Self>) -> Vec<Stateful<Div>> {
        let title = "Wiesel account";
        if self.recovery.blocks_login() {
            let restore = self.recovery.restore_only();
            return vec![row(
                title,
                Some("Saved sign-in is locked. Unlock Keychain, then retry.".into()),
                Self::button("sign-out", "Retry")
                    .track_focus(&self.login_focus)
                    .on_click(cx.listener(move |this, _, _, cx| {
                        if restore {
                            this.retry_restore(cx);
                        } else {
                            this.sign_out(cx);
                        }
                    })),
            )];
        }
        let sign_out = || {
            Self::button("sign-out", "Sign out")
                .on_click(cx.listener(|this, _, _, cx| this.sign_out(cx)))
        };
        if self.authenticated {
            let hours = self.session.credential.as_ref().and_then(|credential| {
                settings::utc_now_ms().ok().map(|now| {
                    (credential.expires_at().saturating_sub(now).max(0) as u64).div_ceil(3_600_000)
                })
            });
            let (plan, credits) = self.device_status.as_ref().map_or_else(
                || ("—".to_owned(), "—".to_owned()),
                |status| {
                    let plan = match status.plan {
                        auth::Plan::Free => "Free",
                        auth::Plan::Starter => "Starter",
                        auth::Plan::Unlimited => "Unlimited",
                    };
                    (plan.to_owned(), grouped(status.virtual_credits))
                },
            );
            return vec![
                row(
                    title,
                    hours.map(|hours| {
                        format!("Signed in · session expires in {}", duration(hours)).into()
                    }),
                    sign_out(),
                )
                // Focusable but inert: opening Settings must never land on Sign out.
                .track_focus(&self.login_focus),
                row("Plan", None, value(plan)),
                row("Credits", None, value(credits)),
            ];
        }
        if self.session.credential.is_some() {
            return vec![row(
                title,
                Some(
                    if self.status_loading {
                        "Checking sign-in…"
                    } else {
                        "Could not verify sign-in."
                    }
                    .into(),
                ),
                div().flex().gap(px(ROW_GAP)).child(sign_out()).child(
                    Self::button("check-login", "Retry")
                        .track_focus(&self.login_focus)
                        .when(self.status_loading, |d| d.opacity(0.5))
                        .on_click(cx.listener(|this, _, _, cx| this.refresh_status(false, cx))),
                ),
            )];
        }
        let pending = self.session.attempt.is_some() || self.session.exchanging;
        let login = Self::key_button(
            "login",
            if pending { "Reopen ↗" } else { "Sign in ↗" },
            theme::Key::Blue,
        )
        .aria_label(if pending {
            "Reopen sign-in in browser"
        } else {
            "Sign in or create an account in browser"
        })
        .track_focus(&self.login_focus)
        .on_click(cx.listener(|this, _, window, cx| this.start_login(window, cx)));
        if pending {
            return vec![row(
                title,
                Some(
                    if self.session.exchanging {
                        "Completing sign-in…"
                    } else {
                        "Continue in your browser."
                    }
                    .into(),
                ),
                div()
                    .flex()
                    .gap(px(ROW_GAP))
                    .child(
                        Self::button("cancel-login", "Cancel")
                            .on_click(cx.listener(|this, _, _, cx| this.cancel_login(cx))),
                    )
                    .child(login),
            )];
        }
        vec![row(
            title,
            Some("Sign in or create an account.".into()),
            login,
        )]
    }

    fn shortcut_row(&self, cx: &mut Context<Self>) -> Stateful<Div> {
        let palette = theme::p();
        let recording = self.recording;
        let default = Settings::default().hotkey;
        let field = div()
            .id("hotkey-field")
            .debug_selector(|| "hotkey-field".into())
            .role(Role::Button)
            .aria_label(format!(
                "Global shortcut {}",
                display_hotkey(&self.settings.hotkey)
            ))
            .h(px(CONTROL_HEIGHT))
            .min_w(px(112.))
            .px(px(12.))
            .flex()
            .items_center()
            .justify_center()
            .rounded(px(4.))
            .bg(rgb(palette.field))
            .border_1()
            .map(|d| {
                if recording {
                    d.border_color(rgb(palette.ring))
                        .text_color(rgb(palette.muted))
                        .child("Press keys…")
                } else {
                    d.border_color(rgba(palette.input_border))
                        .child(display_hotkey(&self.settings.hotkey))
                }
            })
            .text_size(px(12.))
            .cursor_pointer()
            .on_click(cx.listener(move |this, _, window, cx| {
                if !this.recording {
                    this.start_recording(window, cx);
                }
            }));
        // While recording the key stays lit; pressing it again cancels.
        let record = Self::key_button(
            "record",
            if recording { "Recording" } else { "Record" },
            if recording {
                theme::Key::Blue
            } else {
                theme::Key::Grey
            },
        )
        .aria_label(if recording {
            "Cancel shortcut recording"
        } else {
            "Record a new shortcut"
        })
        .on_click(cx.listener(|this, _, window, cx| {
            if this.recording {
                this.cancel_recording(cx);
            } else {
                this.start_recording(window, cx);
            }
        }));
        let reset = (!recording && self.settings.hotkey != default).then(|| {
            Self::button("reset-hotkey", "")
                .aria_label("Reset shortcut to default")
                .tooltip(|_, cx| cx.new(|_| HeaderTooltip("Reset to default")).into())
                .w(px(CONTROL_HEIGHT))
                .px(px(0.))
                .child(theme::icon("rotate-ccw", 12.).text_color(rgb(theme::Key::Grey.label())))
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.apply_hotkey(Settings::default().hotkey, cx)
                }))
        });
        row(
            "Shortcut",
            Some("Opens Wiesel from any app.".into()),
            div()
                .flex()
                .items_center()
                .gap(px(ROW_GAP))
                .child(field)
                .child(record)
                .children(reset),
        )
    }

    fn accessibility_row(&self) -> Stateful<Div> {
        row(
            "Accessibility",
            Some("Needed to read your selected text.".into()),
            if self.accessibility_granted {
                indicator(theme::LED_GREEN, "Granted").into_any_element()
            } else {
                // Status refreshes every second, so no "check again" control is needed.
                Self::button("privacy-settings", "Open Settings ↗")
                    .aria_label("Open Accessibility privacy settings")
                    .on_click(|_, _, cx| {
                        cx.open_url("x-apple.systempreferences:com.apple.preference.security?Privacy_Accessibility")
                    })
                    .into_any_element()
            },
        )
    }

    fn diagnostics_row(&self, cx: &mut Context<Self>) -> Stateful<Div> {
        let unavailable = self.diagnostics.is_none() || self.diagnostics_busy;
        let control = |id: &'static str,
                       title: &str,
                       label: &str,
                       ax: &'static str,
                       focus: &FocusHandle,
                       control: DiagnosticControl| {
            Self::button(id, title)
                .role(Role::Button)
                .aria_label(label.to_owned())
                .accessibility_id(ax)
                .aria_description(if unavailable {
                    "Unavailable or busy"
                } else {
                    "Ready"
                })
                .when(unavailable, |d| d.opacity(0.5))
                .track_focus(focus)
                .on_click(cx.listener(move |this, _, _, cx| this.diagnostic_control(control, cx)))
        };
        row(
            "Logs",
            Some("Stored locally on this Mac.".into()),
            div()
                .flex()
                .gap(px(ROW_GAP))
                .child(control(
                    "open-logs-folder",
                    "Open",
                    "Open Logs Folder in Finder",
                    "wiesel.diagnostics.open-folder",
                    &self.diagnostics_open_focus,
                    DiagnosticControl::OpenFolder,
                ))
                .child(control(
                    "clear-logs",
                    "Clear",
                    "Clear local diagnostic logs",
                    "wiesel.diagnostics.clear",
                    &self.diagnostics_clear_focus,
                    DiagnosticControl::ClearLogs,
                )),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::{display_hotkey, duration, grouped};

    #[test]
    fn session_lifetime_uses_days_beyond_two_days() {
        assert_eq!(duration(0), "1 hour");
        assert_eq!(duration(5), "5 hours");
        assert_eq!(duration(168), "7 days");
    }

    #[test]
    fn shortcuts_display_as_mac_symbols_in_menu_order() {
        assert_eq!(display_hotkey("Super+Shift+Space"), "⇧ ⌘ Space");
        assert_eq!(display_hotkey("Shift+Alt+Control+KeyK"), "⌃ ⌥ ⇧ K");
        assert_eq!(display_hotkey("Super+Digit1"), "⌘ 1");
        assert_eq!(display_hotkey("Control+Enter"), "⌃ ↩");
    }

    #[test]
    fn credits_are_grouped_by_thousands() {
        assert_eq!(grouped(0), "0");
        assert_eq!(grouped(999), "999");
        assert_eq!(grouped(1_234), "1,234");
        assert_eq!(grouped(-1_234_567), "-1,234,567");
    }
}
