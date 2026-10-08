//! One notification surface, with scoped issues and short-lived feedback.
//!
//! Use `success`/`warning` for action feedback, `working` for pending work, and
//! `report`/`issue` for failures that persist until their source recovers.
use crate::theme;
use gpui::{prelude::*, *};
use std::time::{Duration, Instant};
use unicode_segmentation::UnicodeSegmentation;

pub const STATUS_BAR_HEIGHT: f32 = 48.;
const MAX_MESSAGE_GRAPHEMES: usize = 96;
const FEEDBACK_DURATION: Duration = Duration::from_secs(5);

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Severity {
    Info,
    Success,
    Warning,
    Error,
}
impl Severity {
    fn color(self) -> u32 {
        match self {
            Self::Info => theme::MUTED,
            Self::Success => 0x22c55e,
            Self::Warning => 0xeab308,
            Self::Error => 0xef4444,
        }
    }
    fn label(self) -> &'static str {
        match self {
            Self::Info => "Status",
            Self::Success => "Success",
            Self::Warning => "Warning",
            Self::Error => "Error",
        }
    }
}

/// A source owns its issue; unrelated successes cannot erase it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Source {
    Diagnostics,
    Settings,
    Keychain,
    Hotkey,
    Models,
    Request,
    Selection,
    Accessibility,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Notification {
    pub severity: Severity,
    pub message: String,
}
impl Notification {
    pub fn new(severity: Severity, message: impl AsRef<str>) -> Self {
        Self {
            severity,
            message: concise(message.as_ref()),
        }
    }
}

fn concise(message: &str) -> String {
    let normalized = message.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut graphemes = normalized.graphemes(true);
    let prefix = graphemes
        .by_ref()
        .take(MAX_MESSAGE_GRAPHEMES - 1)
        .collect::<String>();
    if graphemes.next().is_some() {
        format!("{prefix}…")
    } else if prefix.is_empty() {
        "Ready".into()
    } else {
        prefix
    }
}

#[derive(Default)]
pub struct Notifications {
    issues: Vec<(Source, Notification)>,
    activity: Vec<(Source, Notification)>,
    feedback: Option<(Notification, Instant)>,
    visible: Option<Notification>,
    revision: usize,
    animate: bool,
    pub suppress_motion: bool,
    paused_at: Option<Instant>,
}
impl Notifications {
    /// Show brief green feedback, then return to any unresolved issue or idle state.
    pub fn success(&mut self, message: impl AsRef<str>) {
        self.feedback(Severity::Success, message);
    }
    /// Missing input is action feedback, not an application failure.
    pub fn warning(&mut self, message: impl AsRef<str>) {
        self.feedback(Severity::Warning, message);
    }
    pub fn info(&mut self, message: impl AsRef<str>) {
        self.feedback(Severity::Info, message);
    }
    fn feedback(&mut self, severity: Severity, message: impl AsRef<str>) {
        self.push(Notification::new(severity, message));
    }
    /// Components can emit a Notification event and let their owner forward it here.
    pub fn push(&mut self, notification: Notification) {
        let now = Instant::now();
        if self.paused_at.is_some() {
            self.paused_at = Some(now);
        }
        self.feedback = Some((
            Notification::new(notification.severity, notification.message),
            now + FEEDBACK_DURATION,
        ));
    }
    pub fn working(&mut self, source: Source, message: impl AsRef<str>) {
        self.clear(source);
        self.feedback = None;
        self.activity
            .push((source, Notification::new(Severity::Info, message)));
    }
    pub fn issue(&mut self, source: Source, severity: Severity, message: impl AsRef<str>) {
        self.clear(source);
        // A newly reported failure must not sit behind stale success feedback.
        self.feedback = None;
        self.issues
            .push((source, Notification::new(severity, message)));
    }
    /// Publish safe UI copy without formatting the error or its source chain.
    /// Typed local diagnostics are recorded separately by the operation owner.
    pub fn report(&mut self, source: Source, message: &str, _error: &anyhow::Error) {
        self.issue(source, Severity::Error, message);
    }
    pub fn clear(&mut self, source: Source) {
        self.issues.retain(|(s, _)| *s != source);
        self.activity.retain(|(s, _)| *s != source);
    }
    /// Keep brief feedback readable when the app is hidden.
    pub fn set_visible(&mut self, now: Instant, visible: bool) {
        if !visible && self.paused_at.is_none() {
            self.paused_at = Some(now);
        } else if visible
            && let Some(paused_at) = self.paused_at.take()
            && let Some((_, deadline)) = &mut self.feedback
        {
            *deadline += now.saturating_duration_since(paused_at);
        }
    }
    /// Reconcile once per app poll/render. Only actual visible changes restart motion.
    pub fn update(&mut self, now: Instant, idle: Notification) -> bool {
        let now = self.paused_at.unwrap_or(now);
        if self
            .feedback
            .as_ref()
            .is_some_and(|(_, deadline)| now >= *deadline)
        {
            self.feedback = None;
        }
        let next = self
            .feedback
            .as_ref()
            .map(|(n, _)| n)
            .or_else(|| {
                self.issues
                    .iter()
                    .rev()
                    .find(|(_, n)| n.severity == Severity::Error)
                    .map(|(_, n)| n)
            })
            .or_else(|| self.activity.last().map(|(_, n)| n))
            .or_else(|| {
                self.issues
                    .iter()
                    .max_by_key(|(_, n)| n.severity)
                    .map(|(_, n)| n)
            })
            .cloned()
            .unwrap_or(idle);
        if self.visible.as_ref() == Some(&next) {
            return false;
        }
        self.visible = Some(next);
        self.revision = self.revision.wrapping_add(1);
        self.animate = !self.suppress_motion;
        true
    }
    pub fn render(&self) -> Div {
        let notification = self
            .visible
            .clone()
            .unwrap_or_else(|| Notification::new(Severity::Success, "Ready"));
        // A restrained opacity transition avoids layout motion and looping pulses.
        // GPUI honors App::reduce_motion for this native animation wrapper.
        let animate = self.animate;
        let duration = Duration::from_millis(180);
        div()
            .h(px(STATUS_BAR_HEIGHT))
            .min_h(px(STATUS_BAR_HEIGHT))
            .max_h(px(STATUS_BAR_HEIGHT))
            .w_full()
            .flex_shrink_0()
            .flex()
            .items_center()
            .gap(px(8.))
            .px(px(20.))
            .border_t_1()
            .border_color(rgba(theme::BORDER))
            .child(
                div()
                    .size(px(6.))
                    .flex_shrink_0()
                    .rounded_full()
                    .bg(rgb(notification.severity.color()))
                    .map(|dot| {
                        if animate {
                            dot.with_animation(
                                ("status-dot", self.revision),
                                Animation::new(duration).with_easing(ease_out_quint()),
                                |dot, progress| dot.opacity(0.55 + 0.45 * progress),
                            )
                            .into_any_element()
                        } else {
                            dot.into_any_element()
                        }
                    }),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .text_size(px(12.))
                    .line_height(px(16.))
                    .text_color(rgb(theme::MUTED))
                    .text_ellipsis()
                    .overflow_hidden()
                    .whitespace_nowrap()
                    .child(format!(
                        "{}{}",
                        if matches!(notification.severity, Severity::Warning | Severity::Error) {
                            format!("{}: ", notification.severity.label())
                        } else {
                            String::new()
                        },
                        notification.message
                    ))
                    .map(|text| {
                        if animate {
                            text.with_animation(
                                ("status-text", self.revision),
                                Animation::new(duration).with_easing(ease_out_quint()),
                                |text, progress| text.opacity(0.65 + 0.35 * progress),
                            )
                            .into_any_element()
                        } else {
                            text.into_any_element()
                        }
                    }),
            )
    }
}

/// Translate capture safety failures without displaying a multi-line error chain.
pub fn capture_issue(error: &anyhow::Error) -> (Severity, &'static str) {
    let detail = format!("{error:#}").to_lowercase();
    if detail.contains("newer clipboard") && detail.contains("preserved") {
        return (
            Severity::Warning,
            "Clipboard changed. Capture cancelled; newer content preserved.",
        );
    }
    if detail.contains("restoration failed")
        || detail.contains("could not restore")
        || detail.contains("rejected clipboard restoration")
    {
        (
            Severity::Error,
            "Clipboard restoration failed. Check your clipboard before pasting.",
        )
    } else if detail.contains("accessibility") || detail.contains("denied simulated copy") {
        (
            Severity::Warning,
            "Enable Accessibility for Wiesel in System Settings.",
        )
    } else if detail.contains("protected") {
        (Severity::Warning, "Protected fields cannot be captured.")
    } else if detail.contains("clipboard")
        && (detail.contains("save")
            || detail.contains("read")
            || detail.contains("preservation")
            || detail.contains("snapshot"))
    {
        (
            Severity::Warning,
            "Could not safely preserve your clipboard. Allow clipboard access and retry.",
        )
    } else if detail.contains("too large") {
        (
            Severity::Warning,
            "Selection is too large. Select a shorter passage.",
        )
    } else if detail.contains("release") {
        (
            Severity::Warning,
            "Release the shortcut keys, then try again.",
        )
    } else if detail.contains("changed") || detail.contains("frontmost") {
        (
            Severity::Warning,
            "Capture cancelled. Keep the source app active and retry.",
        )
    } else if detail.contains("no plain text")
        || detail.contains("empty")
        || detail.contains("stable text")
    {
        (
            Severity::Warning,
            "No text captured. Select text in another app and retry.",
        )
    } else {
        (
            Severity::Error,
            "Could not capture text safely. Select text and retry.",
        )
    }
}

#[cfg(test)]
mod tests {
    use super::{
        FEEDBACK_DURATION, MAX_MESSAGE_GRAPHEMES, Notification, Notifications, Severity, Source,
        capture_issue,
    };
    use std::time::Instant;
    use unicode_segmentation::UnicodeSegmentation;
    fn idle() -> Notification {
        Notification::new(Severity::Success, "Ready")
    }
    #[test]
    fn messages_are_single_line() {
        assert_eq!(
            Notification::new(Severity::Error, "a\n\r\t b").message,
            "a b"
        );
    }
    #[test]
    fn long_unicode_messages_are_bounded_without_splitting_graphemes() {
        let n = Notification::new(Severity::Warning, "👩🏽‍💻".repeat(200));
        assert_eq!(n.message.graphemes(true).count(), MAX_MESSAGE_GRAPHEMES);
        assert!(n.message.ends_with('…'));
    }
    #[test]
    fn success_expires_back_to_unresolved_issue() {
        let mut state = Notifications::default();
        state.issue(Source::Models, Severity::Error, "Refresh models.");
        state.success("Copied.");
        state.update(Instant::now(), idle());
        assert_eq!(state.visible.as_ref().unwrap().severity, Severity::Success);
        state.update(Instant::now() + FEEDBACK_DURATION, idle());
        assert_eq!(state.visible.as_ref().unwrap().message, "Refresh models.");
    }
    #[test]
    fn recovery_only_clears_its_own_source() {
        let mut state = Notifications::default();
        state.issue(Source::Models, Severity::Warning, "Refresh models.");
        state.issue(Source::Keychain, Severity::Error, "Unlock Keychain.");
        state.clear(Source::Keychain);
        state.update(Instant::now(), idle());
        assert_eq!(state.visible.as_ref().unwrap().message, "Refresh models.");
    }
    #[test]
    fn diagnostic_recovery_does_not_clear_a_settings_failure() {
        let mut state = Notifications::default();
        state.issue(
            Source::Settings,
            Severity::Error,
            "Could not save settings.",
        );
        state.issue(
            Source::Diagnostics,
            Severity::Warning,
            "Close other Wiesel instances.",
        );
        state.clear(Source::Diagnostics);
        state.update(Instant::now(), idle());
        assert_eq!(
            state.visible.as_ref().unwrap().message,
            "Could not save settings."
        );
    }
    #[test]
    fn unchanged_status_does_not_restart_animation() {
        let mut state = Notifications::default();
        state.update(Instant::now(), idle());
        assert!(!state.update(Instant::now(), idle()));
    }
    #[test]
    fn keyboard_feedback_does_not_animate() {
        let mut state = Notifications {
            suppress_motion: true,
            ..Default::default()
        };
        state.warning("Include Command.");
        state.update(Instant::now(), idle());
        assert!(!state.animate);
    }
    #[test]
    fn hidden_app_pauses_feedback_expiry() {
        let mut state = Notifications::default();
        state.success("Copied.");
        let now = Instant::now();
        state.set_visible(now, false);
        state.update(now + FEEDBACK_DURATION * 2, idle());
        assert_eq!(state.visible.as_ref().unwrap().message, "Copied.");
        state.set_visible(now + FEEDBACK_DURATION * 2, true);
        state.update(now + FEEDBACK_DURATION * 2, idle());
        assert_eq!(state.visible.as_ref().unwrap().message, "Copied.");
        state.update(now + FEEDBACK_DURATION * 3, idle());
        assert_eq!(state.visible.as_ref().unwrap().message, "Ready");
    }
    #[test]
    fn new_error_replaces_old_success_immediately() {
        let mut state = Notifications::default();
        state.success("Copied.");
        state.report(Source::Request, "Request failed.", &anyhow::anyhow!("test"));
        state.update(Instant::now(), idle());
        assert_eq!(state.visible.as_ref().unwrap().severity, Severity::Error);
    }
    #[test]
    fn background_work_cannot_hide_an_unresolved_error() {
        let mut state = Notifications::default();
        state.issue(Source::Keychain, Severity::Error, "Unlock Keychain.");
        state.working(Source::Models, "Loading models.");
        state.update(Instant::now(), idle());
        assert_eq!(state.visible.as_ref().unwrap().message, "Unlock Keychain.");
    }
    #[test]
    fn retry_replaces_its_own_error_with_progress() {
        let mut state = Notifications::default();
        state.issue(Source::Request, Severity::Error, "Retry.");
        state.working(Source::Request, "Thinking…");
        state.update(Instant::now(), idle());
        assert_eq!(state.visible.as_ref().unwrap().message, "Thinking…");
    }
    #[test]
    fn latest_warning_wins_when_severity_matches() {
        let mut state = Notifications::default();
        state.issue(
            Source::Accessibility,
            Severity::Warning,
            "Enable Accessibility.",
        );
        state.issue(Source::Selection, Severity::Warning, "Select text.");
        state.update(Instant::now(), idle());
        assert_eq!(state.visible.as_ref().unwrap().message, "Select text.");
    }
    #[test]
    fn clipboard_race_with_preserved_content_is_only_a_warning() {
        let error = anyhow::anyhow!("The clipboard changed again. Newer clipboard content was preserved instead of overwritten.")
            .context("Could not restore the previous clipboard; captured text was discarded");
        assert_eq!(capture_issue(&error).0, Severity::Warning);
    }
    #[test]
    fn status_bar_has_fixed_dimensions() {
        use gpui::{Styled, px};
        let mut bar = Notifications::default().render();
        let height = Some(px(super::STATUS_BAR_HEIGHT).into());
        assert_eq!(bar.style().size.height, height);
        assert_eq!(bar.style().min_size.height, height);
        assert_eq!(bar.style().max_size.height, height);
        assert_eq!(bar.style().flex_shrink, Some(0.));
    }
    #[test]
    fn clipboard_restoration_failure_is_an_error() {
        assert_eq!(
            capture_issue(&anyhow::anyhow!(
                "Copy timed out and clipboard restoration failed"
            ))
            .0,
            Severity::Error
        );
    }
}
