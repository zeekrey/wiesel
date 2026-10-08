#[cfg(not(target_os = "macos"))]
compile_error!("Wiesel currently supports macOS only (native selection and Keychain backend).");

mod auth;
mod clipboard_capture;
mod gateway;
mod input;
mod model_picker;
mod notifications;
mod quick_actions;
mod selection;
mod settings;
mod theme;

use anyhow::Context as _;
use auth::{AuthError, DesktopClient, DeviceCredential};
use gateway::Message;
use global_hotkey::{GlobalHotKeyEvent, GlobalHotKeyManager, HotKeyState, hotkey::HotKey};
use gpui::{KeyBinding, prelude::*, *};
use input::TextInput;
use notifications::{Notification, Notifications, Severity, Source};
use quick_actions::{QUICK_ACTIONS, QuickAction};
use settings::Settings;
use std::{
    str::FromStr,
    sync::{Arc, mpsc},
    time::{Duration, Instant},
};

fn parse_hotkey(text: &str) -> anyhow::Result<HotKey> {
    let hotkey = HotKey::from_str(text).map_err(|e| anyhow::anyhow!("Invalid hotkey: {e}"))?;
    anyhow::ensure!(
        hotkey.mods.intersects(
            global_hotkey::hotkey::Modifiers::SUPER
                | global_hotkey::hotkey::Modifiers::CONTROL
                | global_hotkey::hotkey::Modifiers::ALT
        ),
        "Use Command, Control, or Option in your hotkey."
    );
    Ok(hotkey)
}

#[cfg(test)]
mod hotkey_tests {
    use super::{Settings, parse_hotkey};
    #[test]
    fn default_and_recorded_shortcuts_parse() {
        assert!(parse_hotkey(&Settings::default().hotkey).is_ok());
        assert!(parse_hotkey("Super+Shift+KeyW").is_ok());
        assert!(parse_hotkey("Control+Alt+Digit1").is_ok());
    }
    #[test]
    fn unsafe_or_invalid_shortcuts_are_rejected() {
        assert!(parse_hotkey("KeyW").is_err());
        assert!(parse_hotkey("Shift+Space").is_err());
        assert!(parse_hotkey("Super+NotAKey").is_err());
    }
}

fn recover_failed_draft(current: &str, failed: String) -> (String, Option<String>) {
    if current.is_empty() {
        (failed, None)
    } else {
        (current.to_owned(), Some(failed))
    }
}
#[cfg(test)]
mod draft_tests {
    use super::recover_failed_draft;
    #[test]
    fn failure_restores_an_empty_composer() {
        assert_eq!(recover_failed_draft("", "A".into()), ("A".into(), None));
    }
    #[test]
    fn failure_preserves_new_draft_and_retains_failed_message() {
        assert_eq!(
            recover_failed_draft("B", "A".into()),
            ("B".into(), Some("A".into()))
        );
    }
}

#[derive(Clone, Copy, PartialEq)]
enum Page {
    Setup,
    Launcher,
    Chat,
    Writing,
}
struct HeaderTooltip(&'static str);
impl Render for HeaderTooltip {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div()
            .px_2()
            .py_1()
            .rounded(px(6.))
            .bg(rgb(theme::CARD))
            .border_1()
            .border_color(rgba(theme::BORDER))
            .text_color(rgb(theme::FOREGROUND))
            .text_size(px(11.))
            .child(self.0)
    }
}

actions!(wiesel, [Login]);

// Every async event is bound to both a login generation and the issuing device.
// Secrets stay in the credential/attempt; neither is copied into the event scope.
#[derive(Clone, Copy, PartialEq, Eq)]
struct EventScope {
    generation: u64,
    device_id: Option<uuid::Uuid>,
}
#[derive(Default)]
struct DesktopSession {
    generation: u64,
    credential: Option<Arc<DeviceCredential>>,
    attempt: Option<auth::Attempt>,
    exchanging: bool,
}
impl DesktopSession {
    fn scope(&self) -> EventScope {
        EventScope {
            generation: self.generation,
            device_id: self.credential.as_ref().map(|c| c.device_id()),
        }
    }
    fn accepts(&self, scope: EventScope) -> bool {
        self.scope() == scope
    }
    fn cancel_login(&mut self) {
        self.generation += 1;
        if let Some(mut attempt) = self.attempt.take() {
            attempt.cancel();
        }
        self.exchanging = false;
    }
    fn end_session(&mut self) -> Option<settings::CleanupTarget> {
        let target = self
            .credential
            .take()
            .map(|credential| settings::CleanupTarget::device(credential.device_id()));
        self.cancel_login();
        target
    }
    fn consume_callback(&mut self, raw: &str) -> Result<auth::ExchangeRequest, AuthError> {
        let request = auth::consume_callback(&mut self.attempt, raw)?;
        self.exchanging = true;
        Ok(request)
    }
    fn persist_exchange(
        &mut self,
        scope: EventScope,
        credential: DeviceCredential,
        save: impl FnOnce(&DeviceCredential) -> Result<(), settings::CredentialError>,
    ) -> Result<bool, settings::CredentialError> {
        if !self.accepts(scope) || !self.exchanging {
            return Ok(false);
        }
        self.exchanging = false;
        save(&credential)?;
        self.credential = Some(Arc::new(credential));
        Ok(true)
    }
    fn login_url(&mut self) -> Result<zeroize::Zeroizing<String>, AuthError> {
        // Replacement invalidates old completions even if secure randomness fails.
        self.generation += 1;
        self.exchanging = false;
        let result = if let Some(attempt) = &mut self.attempt {
            attempt.replace()
        } else {
            auth::Attempt::new().map(|attempt| self.attempt = Some(attempt))
        };
        let result = result.and_then(|()| {
            self.attempt
                .as_mut()
                .ok_or(AuthError::Inactive)?
                .login_url()
        });
        if result.is_err() {
            self.cancel_login();
        }
        result.map(zeroize::Zeroizing::new)
    }
}
// Recovery never discovers a deletion target at retry time. A load that could
// not observe the pointer can only retry restoration, not remove a later login.
#[derive(Default)]
enum CredentialRecovery {
    #[default]
    Ready,
    Restore,
    Remove(settings::CleanupTarget),
}
impl CredentialRecovery {
    fn from_load_failure(failure: settings::CredentialLoadError) -> Self {
        match failure.cleanup {
            Some(target) => Self::Remove(target),
            None => Self::Restore,
        }
    }
    fn blocks_login(&self) -> bool {
        !matches!(self, Self::Ready)
    }
    fn restore_only(&self) -> bool {
        matches!(self, Self::Restore)
    }
    fn remove(
        &mut self,
        target: Option<settings::CleanupTarget>,
        delete: impl FnOnce(&settings::CleanupTarget) -> Result<(), settings::CredentialError>,
    ) -> Result<(), settings::CredentialError> {
        if let Some(target) = target {
            *self = Self::Remove(target);
        }
        match self {
            Self::Ready => (),
            Self::Restore => return Err(settings::CredentialError::Keychain),
            Self::Remove(target) => delete(target)?,
        }
        *self = Self::Ready;
        Ok(())
    }
}
#[cfg(test)]
mod desktop_session_tests {
    use super::{CredentialRecovery, DesktopSession, EventScope};
    use crate::{
        auth::AuthError,
        settings::{self, CredentialError, DeviceCredential},
    };
    use std::{cell::Cell, sync::Arc};

    fn credential(device: u128) -> DeviceCredential {
        let now = settings::utc_now_ms().unwrap();
        DeviceCredential::validated(
            zeroize::Zeroizing::new(format!("wd_{}", "t".repeat(43))),
            now + 60_000,
            uuid::Uuid::from_u128(device),
            now,
        )
        .unwrap()
    }
    fn callback(url: &str) -> String {
        let url = reqwest::Url::parse(url).unwrap();
        let state = url
            .query_pairs()
            .find(|(name, _)| name == "state")
            .unwrap()
            .1
            .into_owned();
        format!(
            "wiesel://auth/callback?code={}&state={state}",
            "c".repeat(43)
        )
    }
    #[test]
    fn cold_launch_callback_does_not_create_an_exchange() {
        let mut session = DesktopSession::default();
        let raw = format!(
            "wiesel://auth/callback?code={}&state={}",
            "c".repeat(43),
            "s".repeat(43)
        );
        assert!(matches!(
            session.consume_callback(&raw),
            Err(AuthError::Inactive)
        ));
        assert!(!session.exchanging);
    }
    #[test]
    fn matching_callback_consumes_the_only_attempt_before_exchange() {
        let mut session = DesktopSession::default();
        let raw = callback(&session.login_url().unwrap());
        let _request = session.consume_callback(&raw).unwrap();
        assert!(session.attempt.is_none());
        assert!(session.exchanging);
        assert!(matches!(
            session.consume_callback(&raw),
            Err(AuthError::Inactive)
        ));
    }
    #[test]
    fn canceled_exchange_never_calls_persistence() {
        let mut session = DesktopSession::default();
        let raw = callback(&session.login_url().unwrap());
        let _request = session.consume_callback(&raw).unwrap();
        let scope = session.scope();
        session.cancel_login();
        let called = Cell::new(false);
        let saved = session
            .persist_exchange(scope, credential(1), |_| {
                called.set(true);
                Ok(())
            })
            .unwrap();
        assert!(!saved);
        assert!(!called.get());
        assert!(session.credential.is_none());
    }
    #[test]
    fn replaced_exchange_never_calls_persistence_or_changes_new_attempt() {
        let mut session = DesktopSession::default();
        let raw = callback(&session.login_url().unwrap());
        let _request = session.consume_callback(&raw).unwrap();
        let scope = session.scope();
        let new_raw = callback(&session.login_url().unwrap());
        let saved = session
            .persist_exchange(scope, credential(1), |_| panic!("stale persistence"))
            .unwrap();
        assert!(!saved);
        assert!(session.consume_callback(&new_raw).is_ok());
    }
    #[test]
    fn matching_exchange_is_persisted_once_and_binds_events_to_device() {
        let mut session = DesktopSession::default();
        let raw = callback(&session.login_url().unwrap());
        let _request = session.consume_callback(&raw).unwrap();
        let scope = session.scope();
        let writes = Cell::new(0);
        assert!(
            session
                .persist_exchange(scope, credential(1), |_| {
                    writes.set(writes.get() + 1);
                    Ok(())
                })
                .unwrap()
        );
        assert!(
            !session
                .persist_exchange(scope, credential(1), |_| panic!("replay persistence"))
                .unwrap()
        );
        assert_eq!(writes.get(), 1);
        assert_eq!(session.scope().device_id, Some(uuid::Uuid::from_u128(1)));
    }
    #[test]
    fn persistence_failure_does_not_sign_in_or_leave_attempt_secrets() {
        let mut session = DesktopSession::default();
        let raw = callback(&session.login_url().unwrap());
        let _request = session.consume_callback(&raw).unwrap();
        let result = session.persist_exchange(session.scope(), credential(1), |_| {
            Err(CredentialError::Keychain)
        });
        assert_eq!(result, Err(CredentialError::Keychain));
        assert!(session.credential.is_none());
        assert!(session.attempt.is_none());
        assert!(!session.exchanging);
    }
    #[test]
    fn old_status_models_unauthorized_and_inference_events_fail_one_shared_gate() {
        let mut session = DesktopSession {
            credential: Some(Arc::new(credential(1))),
            ..Default::default()
        };
        let old_scope = session.scope();
        session.cancel_login();
        session.credential = Some(Arc::new(credential(2)));
        assert!(!session.accepts(old_scope));
        assert!(session.accepts(session.scope()));
    }
    #[test]
    fn wrong_device_is_rejected_even_with_the_current_generation() {
        let session = DesktopSession {
            credential: Some(Arc::new(credential(2))),
            ..Default::default()
        };
        assert!(!session.accepts(EventScope {
            generation: session.generation,
            device_id: Some(uuid::Uuid::from_u128(1))
        }));
    }
    #[test]
    fn cancel_reopen_invalidates_the_old_browser_callback() {
        let mut session = DesktopSession::default();
        let old_raw = callback(&session.login_url().unwrap());
        session.cancel_login();
        let _new_url = session.login_url().unwrap();
        assert!(matches!(
            session.consume_callback(&old_raw),
            Err(AuthError::StateMismatch)
        ));
        assert!(session.attempt.is_some());
    }
    #[test]
    fn native_delete_failure_blocks_login_and_successful_retry_clears_recovery() {
        let mut session = DesktopSession {
            credential: Some(Arc::new(credential(1))),
            ..Default::default()
        };
        let old_scope = session.scope();
        let target = session.end_session();
        let mut recovery = CredentialRecovery::Ready;
        let failed = recovery.remove(target, |_| {
            settings::checked_native_delete(Err(security_framework::base::Error::from_code(-25292)))
        });
        assert_eq!(failed, Err(CredentialError::Keychain));
        assert!(recovery.blocks_login());
        assert!(!recovery.restore_only());
        assert!(session.credential.is_none());
        assert!(!session.accepts(old_scope));
        let called = Cell::new(false);
        recovery
            .remove(None, |_| {
                called.set(true);
                settings::checked_native_delete(Ok(()))
            })
            .unwrap();
        assert!(called.get());
        assert!(!recovery.blocks_login());
    }

    #[test]
    fn unobserved_startup_failure_can_only_retry_restore_not_delete_a_later_device() {
        let mut recovery = CredentialRecovery::from_load_failure(settings::CredentialLoadError {
            error: CredentialError::Keychain,
            cleanup: None,
        });
        assert!(recovery.restore_only());
        assert_eq!(
            recovery.remove(None, |_| panic!("unknown-identity cleanup must not delete")),
            Err(CredentialError::Keychain)
        );
        assert!(recovery.blocks_login());
    }

    #[test]
    fn failed_known_startup_cleanup_retains_target_for_retry_without_restoring() {
        let mut recovery = CredentialRecovery::from_load_failure(settings::CredentialLoadError {
            error: CredentialError::Invalid,
            cleanup: Some(settings::CleanupTarget::device(uuid::Uuid::from_u128(1))),
        });
        assert!(!recovery.restore_only());
        assert_eq!(
            recovery.remove(None, |_| Err(CredentialError::Keychain)),
            Err(CredentialError::Keychain)
        );
        assert!(recovery.blocks_login());
        recovery.remove(None, |_| Ok(())).unwrap();
        assert!(!recovery.blocks_login());
    }
}

struct ResultEvent {
    scope: EventScope,
    result: ResultPayload,
}
enum ResultPayload {
    Exchange(Result<DeviceCredential, AuthError>),
    Status(Result<auth::DeviceStatus, AuthError>, bool),
    Models(anyhow::Result<Vec<String>>),
    ChatDelta(String),
    Completion(anyhow::Result<String>, bool),
    SignOut(Result<(), AuthError>),
}
struct Wiesel {
    page: Page,
    settings: Settings,
    manager: Option<GlobalHotKeyManager>,
    hotkey: Option<HotKey>,
    hotkey_input: Entity<TextInput>,
    session: DesktopSession,
    login_focus: FocusHandle,
    urls: mpsc::Receiver<zeroize::Zeroizing<String>>,
    status_loading: bool,
    device_status: Option<auth::DeviceStatus>,
    recovery: CredentialRecovery,
    model_picker: Entity<model_picker::ModelPicker>,
    grammar_input: Entity<TextInput>,
    improve_input: Entity<TextInput>,
    composer: Entity<TextInput>,
    launcher_input: Entity<TextInput>,
    focus: FocusHandle,
    recording: bool,
    authenticated: bool,
    accessibility_granted: bool,
    permission_checked_at: Instant,
    busy: bool,
    notifications: Notifications,
    selection: selection::Capture,
    pending_capture: Option<selection::PendingCapture>,
    failed_chat: std::collections::VecDeque<String>,
    result: String,
    writing_title: String,
    messages: Vec<Message>,
    tx: mpsc::Sender<ResultEvent>,
    rx: mpsc::Receiver<ResultEvent>,
    chat_scroll: ScrollHandle,
    chat_follow_bottom: bool,
    streaming_chat: Option<Message>,
}
impl Wiesel {
    fn new(
        urls: mpsc::Receiver<zeroize::Zeroizing<String>>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let mut notifications = Notifications::default();
        let settings = match settings::load() {
            Ok(s) => s,
            Err(e) => {
                notifications.report(
                    Source::Settings,
                    "Could not load settings. Using defaults.",
                    &e,
                );
                Settings::default()
            }
        };
        let (credential, recovery) = match settings::load_device_credential() {
            Ok(credential) => (credential.map(Arc::new), CredentialRecovery::Ready),
            Err(failure) => {
                notifications.issue(
                    Source::Keychain,
                    Severity::Error,
                    "Could not restore login. Unlock Keychain and retry the saved-login recovery control.",
                );
                (None, CredentialRecovery::from_load_failure(failure))
            }
        };
        let manager = match GlobalHotKeyManager::new() {
            Ok(manager) => Some(manager),
            Err(e) => {
                notifications.report(
                    Source::Hotkey,
                    "Global shortcuts unavailable. Relaunch Wiesel.",
                    &e.into(),
                );
                None
            }
        };
        let hotkey_input = cx.new(|cx| TextInput::new("Super+Shift+Space", false, cx));
        hotkey_input.update(cx, |i, cx| i.set(settings.hotkey.clone(), cx));
        let model_picker = cx.new(|cx| model_picker::ModelPicker::new(settings.model.clone(), cx));
        let grammar_input = cx.new(|cx| TextInput::new("Grammar system prompt", false, cx));
        grammar_input.update(cx, |i, cx| i.set(settings.grammar_prompt.clone(), cx));
        let improve_input = cx.new(|cx| TextInput::new("Improve writing system prompt", false, cx));
        improve_input.update(cx, |i, cx| i.set(settings.improve_prompt.clone(), cx));
        let (tx, rx) = mpsc::channel();
        let mut app = Self {
            page: Page::Setup,
            settings,
            manager,
            hotkey: None,
            hotkey_input,
            model_picker,
            grammar_input,
            improve_input,
            session: DesktopSession {
                credential,
                ..Default::default()
            },
            login_focus: cx.focus_handle().tab_stop(true),
            urls,
            status_loading: false,
            device_status: None,
            recovery,
            composer: cx.new(|cx| TextInput::new("Ask anything…", false, cx)),
            launcher_input: cx.new(TextInput::launcher),
            focus: cx.focus_handle(),
            recording: false,
            authenticated: false,
            accessibility_granted: selection::accessibility_granted(),
            permission_checked_at: Instant::now(),
            busy: false,
            notifications,
            selection: selection::Capture {
                error: "Select text in another app and press your hotkey.".into(),
                ..Default::default()
            },
            pending_capture: None,
            failed_chat: std::collections::VecDeque::new(),
            result: String::new(),
            writing_title: String::new(),
            messages: vec![],
            tx,
            rx,
            chat_scroll: ScrollHandle::new(),
            chat_follow_bottom: true,
            streaming_chat: None,
        };
        for input in [
            &app.hotkey_input,
            &app.grammar_input,
            &app.improve_input,
            &app.composer,
            &app.launcher_input,
        ] {
            cx.subscribe(input, |this, _, notification: &Notification, cx| {
                this.input_notification(notification, cx);
            })
            .detach();
        }
        cx.subscribe(
            &app.model_picker,
            |this, _, notification: &Notification, cx| {
                this.input_notification(notification, cx);
            },
        )
        .detach();
        // The shortcut also works before browser login and onboarding are complete.
        if let Err(e) = app.register(&app.settings.hotkey.clone()) {
            app.notifications.report(
                Source::Hotkey,
                if app.manager.is_none() {
                    "Global shortcuts unavailable. Relaunch Wiesel."
                } else {
                    "Shortcut unavailable. Choose another in Settings."
                },
                &e,
            );
            app.page = Page::Setup;
        }
        window.on_window_should_close(cx, |_, cx| {
            cx.hide();
            false
        });
        window.focus(&app.login_focus, cx);
        app.sync_permission_issue();
        app.refresh_status(true, cx);
        app
    }
    fn input_notification(&mut self, notification: &Notification, cx: &mut Context<Self>) {
        self.notifications.suppress_motion = true;
        self.notifications.push(notification.clone());
        self.notifications
            .update(Instant::now(), self.idle_notification());
        self.notifications.suppress_motion = false;
        cx.notify();
    }
    fn register(&mut self, text: &str) -> anyhow::Result<()> {
        let next = parse_hotkey(text)?;
        if self.hotkey == Some(next) {
            return Ok(());
        }
        let manager = self
            .manager
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("Global hotkeys are unavailable"))?;
        manager
            .register(next)
            .map_err(|e| anyhow::anyhow!("Cannot register shortcut (it may be in use): {e}"))?;
        if let Some(old) = self.hotkey
            && let Err(e) = manager.unregister(old)
        {
            if let Err(rollback) = manager.unregister(next) {
                return Err(anyhow::anyhow!(
                    "Could not replace shortcut: {e}; rollback failed: {rollback}"
                ));
            }
            return Err(e.into());
        }
        self.hotkey = Some(next);
        Ok(())
    }
    fn apply_hotkey(&mut self, cx: &mut Context<Self>) {
        let text = self.hotkey_input.read(cx).content.trim().to_owned();
        if parse_hotkey(&text).is_err() {
            self.notifications
                .warning("Choose a valid shortcut with Command, Control, or Option.");
            cx.notify();
            return;
        }
        let previous = self.settings.hotkey.clone();
        if let Err(e) = self.register(&text) {
            self.notifications.report(
                Source::Hotkey,
                "Could not activate shortcut. Choose another and retry.",
                &e,
            );
        } else {
            self.notifications.clear(Source::Hotkey);
            let mut next = self.settings.clone();
            next.hotkey = text;
            match settings::save(&next) {
                Ok(()) => {
                    self.settings = next;
                    self.notifications.clear(Source::Settings);
                    self.notifications.success("Shortcut active and saved.");
                }
                Err(e) => {
                    self.notifications.report(
                        Source::Settings,
                        "Could not save shortcut. Try again.",
                        &e,
                    );
                    self.rollback_hotkey(&previous);
                }
            }
        }
        cx.notify();
    }
    fn rollback_hotkey(&mut self, previous: &str) {
        if let Err(e) = self.register(previous) {
            self.notifications.report(
                Source::Hotkey,
                "Could not restore shortcut. Reapply it in Settings.",
                &e,
            );
        }
    }
    fn sync_permission_issue(&mut self) {
        if self.accessibility_granted {
            self.notifications.clear(Source::Accessibility);
        } else {
            self.notifications.issue(
                Source::Accessibility,
                Severity::Warning,
                "Enable Accessibility in System Settings to capture selected text.",
            );
        }
    }
    fn idle_notification(&self) -> Notification {
        if !self.authenticated {
            Notification::new(Severity::Warning, "Log in to Wiesel in Settings (⌘⇧L).")
        } else if let Some(text) = &self.selection.text {
            Notification::new(
                Severity::Success,
                format!("Ready · {} characters selected", text.chars().count()),
            )
        } else {
            Notification::new(Severity::Success, "Ready")
        }
    }
    fn refresh_permissions(&mut self, cx: &mut Context<Self>) {
        self.permission_checked_at = Instant::now();
        let granted = selection::accessibility_granted();
        if self.accessibility_granted != granted {
            self.accessibility_granted = granted;
            // Never leave a previously captured selection ready after permission is revoked.
            if !granted {
                self.selection.update(Err(anyhow::anyhow!("Accessibility access was revoked. Enable Wiesel in System Settings → Privacy & Security → Accessibility.")));
            }
            self.sync_permission_issue();
            if granted {
                if self
                    .selection
                    .error
                    .to_lowercase()
                    .contains("accessibility")
                {
                    self.notifications.clear(Source::Selection);
                }
                self.notifications.success("Accessibility enabled.");
            }
            cx.notify();
        }
    }
    fn poll(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.notifications
            .set_visible(Instant::now(), window.is_visible());
        if self.permission_checked_at.elapsed() >= Duration::from_secs(1) {
            cx.set_reduce_motion(
                objc2_app_kit::NSWorkspace::sharedWorkspace()
                    .accessibilityDisplayShouldReduceMotion(),
            );
            self.refresh_permissions(cx);
        }
        if self
            .session
            .attempt
            .as_mut()
            .is_some_and(|attempt| attempt.expired())
        {
            self.cancel_login(cx);
            self.notifications
                .warning("Browser login expired. Start a new login.");
        }
        if self
            .session
            .credential
            .as_ref()
            .is_some_and(|c| c.ensure_valid().is_err())
        {
            self.clear_session("Login expired. Log in again; sessions do not refresh.", cx);
        }
        while let Ok(event) = GlobalHotKeyEvent::receiver().try_recv() {
            if event.state == HotKeyState::Pressed
                && self.hotkey.is_some_and(|h| h.id() == event.id)
            {
                if self.recording {
                    // Carbon consumes a registered shortcut before GPUI's key-down handler sees it.
                    self.hotkey_input
                        .update(cx, |input, cx| input.set(self.settings.hotkey.clone(), cx));
                    self.recording = false;
                    self.apply_hotkey(cx);
                    continue;
                }
                if self.pending_capture.is_none() {
                    // Keep the source active until Copy and clipboard restoration finish.
                    // Ignore repeated shortcut presses while capture is in progress.
                    self.selection.update(Err(anyhow::anyhow!(
                        "Capturing selection… release the shortcut keys."
                    )));
                    self.notifications.working(
                        Source::Selection,
                        "Capturing selection… release the shortcut keys.",
                    );
                    self.pending_capture = Some(selection::begin());
                    cx.notify();
                }
            }
        }
        if let Some(capture) = self.pending_capture.as_mut()
            && let Some(result) = capture.poll()
        {
            self.pending_capture = None;
            self.notifications.clear(Source::Selection);
            match &result {
                Ok(text) => self.notifications.success(format!(
                    "Captured {} characters. Ready for writing.",
                    text.chars().count()
                )),
                Err(e) => {
                    let (severity, message) = notifications::capture_issue(e);
                    eprintln!("Wiesel selection: {e:#}");
                    self.notifications
                        .issue(Source::Selection, severity, message);
                }
            }
            self.selection.update(result);
            if !self.busy && self.page != Page::Setup {
                self.page = Page::Launcher;
            }
            cx.activate(true);
            window.activate_window();
            if self.page == Page::Launcher {
                window.focus(&self.launcher_input.focus_handle(cx), cx);
            } else if self.page == Page::Setup && !self.authenticated {
                window.focus(&self.login_focus, cx);
            } else {
                window.focus(&self.focus, cx);
            }
            cx.notify();
        }
        // Callback activation must not steal focus before Copy restoration finishes.
        if self.pending_capture.is_none() {
            while let Ok(raw) = self.urls.try_recv() {
                self.handle_callback(&raw, window, cx);
            }
        }
        // Resume following only once the reader has returned to the bottom.
        if !self.chat_follow_bottom
            && self.chat_scroll.max_offset().y + self.chat_scroll.offset().y <= px(1.)
        {
            self.chat_follow_bottom = true;
        }
        while let Ok(event) = self.rx.try_recv() {
            if !self.session.accepts(event.scope) {
                // In particular, never persist a canceled/replaced exchange result.
                continue;
            }
            self.apply_result(event.scope, event.result, window, cx);
            cx.notify();
        }
        if self
            .notifications
            .update(Instant::now(), self.idle_notification())
        {
            cx.notify();
        }
    }
    fn apply_result(
        &mut self,
        scope: EventScope,
        result: ResultPayload,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match result {
            ResultPayload::Exchange(result) => {
                self.notifications.clear(Source::Request);
                match result {
                    Ok(credential) => {
                        // UI-thread persistence is serialized with cancel/signout. No worker
                        // writes Keychain before its generation has been accepted here.
                        let target = settings::CleanupTarget::device(credential.device_id());
                        match self.session.persist_exchange(
                            scope,
                            credential,
                            settings::save_device_credential,
                        ) {
                            Ok(false) => (),
                            Ok(true) => {
                                self.authenticated = true;
                                self.recovery = CredentialRecovery::Ready;
                                self.notifications.clear(Source::Keychain);
                                self.notifications
                                    .success("Logged in. Device login saved in Keychain.");
                                self.login_ready(true, window, cx);
                                self.refresh_status(false, cx);
                            }
                            Err(_) => {
                                self.recovery = CredentialRecovery::Remove(target);
                                self.clear_session(
                                    "Login could not be saved. Unlock Keychain and restart login.",
                                    cx,
                                );
                            }
                        }
                    }
                    Err(_) => {
                        self.session.cancel_login();
                        self.notifications.issue(
                            Source::Request,
                            Severity::Error,
                            "Login exchange failed. Start a new login; do not retry the old link.",
                        );
                    }
                }
            }
            ResultPayload::Status(result, startup) => {
                self.status_loading = false;
                self.notifications.clear(Source::Request);
                match result {
                    Ok(status) => {
                        self.device_status = Some(status);
                        self.authenticated = true;
                        self.notifications.success("Device login verified.");
                        self.login_ready(startup, window, cx);
                    }
                    Err(AuthError::HttpStatus(401)) => {
                        self.clear_session("Device login rejected. Log in again.", cx);
                    }
                    Err(AuthError::Credential(_)) => {
                        self.clear_session("Device login is no longer valid. Log in again.", cx);
                    }
                    Err(_) => self.notifications.issue(Source::Request, Severity::Error,
                        "Could not check device login. Check your connection and Check login again."),
                }
            }
            ResultPayload::Models(result) => {
                if result.as_ref().is_err_and(gateway::is_unauthorized) {
                    self.clear_session("Device login rejected. Log in again.", cx);
                    return;
                }
                self.notifications.clear(Source::Models);
                if let Err(e) = &result {
                    self.notifications.issue(
                        Source::Models,
                        Severity::Error,
                        gateway::failure_message(e),
                    );
                }
                self.model_picker
                    .update(cx, |picker, cx| picker.finish_load(result, cx));
            }
            ResultPayload::ChatDelta(delta) => {
                if let Some(message) = &mut self.streaming_chat {
                    message.content.push_str(&delta);
                    self.follow_chat_bottom();
                }
            }
            ResultPayload::Completion(result, chat) => {
                self.busy = false;
                if chat {
                    self.streaming_chat = None;
                }
                match result {
                    Ok(text) => {
                        if chat {
                            self.messages.push(Message::new("assistant", text));
                            self.follow_chat_bottom();
                        } else {
                            self.result = text;
                        }
                        self.notifications.clear(Source::Request);
                        self.notifications.success(if chat {
                            "Response ready."
                        } else {
                            "Writing ready. Copy the result to use it."
                        });
                    }
                    Err(e) => {
                        if gateway::is_unauthorized(&e) {
                            self.clear_session("Device login rejected. Log in again.", cx);
                            return;
                        }
                        self.notifications.issue(
                            Source::Request,
                            Severity::Error,
                            gateway::failure_message(&e),
                        );
                        if chat && let Some(message) = self.messages.pop() {
                            let (draft, failed) = recover_failed_draft(
                                &self.composer.read(cx).content,
                                message.content,
                            );
                            if let Some(failed) = failed {
                                self.failed_chat.push_back(failed);
                            } else {
                                self.composer.update(cx, |i, cx| i.set(draft, cx));
                            }
                        }
                    }
                }
            }
            ResultPayload::SignOut(result) => {
                if result.is_err() {
                    self.notifications.issue(
                        Source::Request,
                        Severity::Warning,
                        "Remote sign-out unconfirmed. Revoke this device on the website if needed.",
                    );
                } else if !self.recovery.blocks_login() {
                    self.notifications
                        .success("Signed out this device. Its saved credential was removed.");
                }
            }
        }
    }
    fn login_ready(&mut self, show_launcher: bool, window: &mut Window, cx: &mut Context<Self>) {
        self.refresh_models(cx);
        if show_launcher
            && self.hotkey.is_some()
            && self.settings.onboarded
            && self.page == Page::Setup
        {
            self.page = Page::Launcher;
            window.focus(&self.launcher_input.focus_handle(cx), cx);
        }
    }
    fn start_login(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.page = Page::Setup;
        window.focus(&self.login_focus, cx);
        if self.session.credential.is_some() {
            self.notifications
                .info("Already have a device login. Check login or Sign out to switch accounts.");
        } else if self.recovery.blocks_login() {
            self.notifications.warning(if self.recovery.restore_only() {
                "Unlock Keychain and retry restoring login before starting a new login."
            } else {
                "Remove this saved login first. Unlock Keychain and retry removal."
            });
        } else {
            self.notifications.clear(Source::Request);
            match self.session.login_url() {
                Ok(url) => {
                    cx.open_url(&url);
                    self.notifications.working(
                        Source::Request,
                        "Waiting for browser login… Cancel or reopen anytime.",
                    );
                }
                Err(_) => self.notifications.issue(
                    Source::Request,
                    Severity::Error,
                    "Could not start secure login. Try Login / Sign up again.",
                ),
            }
        }
        cx.notify();
    }
    fn cancel_login(&mut self, cx: &mut Context<Self>) {
        self.session.cancel_login();
        self.notifications.clear(Source::Request);
        self.notifications
            .info("Login canceled. Old browser links cannot sign you in.");
        cx.notify();
    }
    fn handle_callback(&mut self, raw: &str, window: &mut Window, cx: &mut Context<Self>) {
        cx.activate(true);
        window.activate_window();
        match self.session.consume_callback(raw) {
            Ok(request) => {
                self.notifications
                    .working(Source::Request, "Completing secure login…");
                let scope = self.session.scope();
                let tx = self.tx.clone();
                std::thread::spawn(move || {
                    let result = DesktopClient::new().and_then(|client| client.exchange(request));
                    let _ = tx.send(ResultEvent {
                        scope,
                        result: ResultPayload::Exchange(result),
                    });
                });
            }
            Err(AuthError::Expired) => {
                self.cancel_login(cx);
                self.notifications
                    .warning("Login expired. Start a new login.");
            }
            Err(AuthError::Inactive) => self.notifications.warning(
                "No matching active login. Reopen Login / Sign up; old links cannot be restored.",
            ),
            Err(_) => self
                .notifications
                .warning("Login link did not match. Continue in your browser or reopen login."),
        }
        if !self.authenticated {
            self.page = Page::Setup;
            window.focus(&self.login_focus, cx);
        }
        cx.notify();
    }
    fn valid_credential(&mut self, cx: &mut Context<Self>) -> Option<Arc<DeviceCredential>> {
        let credential = self.session.credential.as_ref()?.clone();
        if credential.ensure_valid().is_err() {
            self.clear_session("Login expired. Log in again; sessions do not refresh.", cx);
            return None;
        }
        Some(credential)
    }
    fn clear_session(&mut self, message: &'static str, cx: &mut Context<Self>) {
        let request_pending = self.busy;
        let target = self.session.end_session();
        self.authenticated = false;
        self.status_loading = false;
        self.device_status = None;
        self.busy = false;
        self.streaming_chat = None;
        self.messages.clear();
        self.failed_chat.clear();
        self.result.clear();
        self.page = Page::Setup;
        self.model_picker
            .update(cx, |picker, cx| picker.finish_load(Ok(vec![]), cx));
        self.notifications.clear(Source::Models);
        self.notifications.clear(Source::Request);
        // Local removal is independent of remote success. A failure is never labeled
        // persistent logout, and blocks a new login until removal is retried.
        let removed = self
            .recovery
            .remove(target, settings::delete_device_credential);
        if removed.is_err() {
            self.notifications.issue(
                Source::Keychain,
                Severity::Error,
                "Session ended, but saved-login recovery is incomplete. Unlock Keychain and retry.",
            );
        } else {
            self.notifications.clear(Source::Keychain);
            self.notifications.warning(message);
        }
        if request_pending {
            self.notifications.issue(
                Source::Request,
                Severity::Warning,
                "Pending request outcome unknown; it may be charged. Do not retry blindly.",
            );
        }
        cx.notify();
    }
    fn sign_out(&mut self, cx: &mut Context<Self>) {
        let credential = self.session.credential.clone();
        self.clear_session(
            "Local session ended. Captured saved-login cleanup completed.",
            cx,
        );
        if let Some(credential) = credential {
            let tx = self.tx.clone();
            let scope = self.session.scope();
            std::thread::spawn(move || {
                let result = DesktopClient::new().and_then(|client| client.sign_out(&credential));
                let _ = tx.send(ResultEvent {
                    scope,
                    result: ResultPayload::SignOut(result),
                });
            });
        }
    }
    fn retry_restore(&mut self, cx: &mut Context<Self>) {
        // There was no safely observed identity at the failed load. Retry a
        // locked load instead of inventing a destructive cleanup target.
        match settings::load_device_credential() {
            Ok(credential) => {
                self.session.cancel_login();
                self.session.credential = credential.map(Arc::new);
                self.recovery = CredentialRecovery::Ready;
                self.notifications.clear(Source::Keychain);
                if self.session.credential.is_some() {
                    self.refresh_status(false, cx);
                } else {
                    self.notifications
                        .success("Saved login checked. Start a fresh login.");
                }
            }
            Err(failure) => {
                self.recovery = CredentialRecovery::from_load_failure(failure);
                self.notifications.issue(
                    Source::Keychain,
                    Severity::Error,
                    "Saved-login recovery failed. Unlock Keychain and retry the recovery control.",
                );
            }
        }
        cx.notify();
    }
    fn refresh_status(&mut self, startup: bool, cx: &mut Context<Self>) {
        if self.status_loading {
            return;
        }
        let Some(credential) = self.valid_credential(cx) else {
            return;
        };
        self.status_loading = true;
        self.notifications
            .working(Source::Request, "Checking saved device login…");
        let tx = self.tx.clone();
        let scope = self.session.scope();
        std::thread::spawn(move || {
            let result = DesktopClient::new().and_then(|client| client.device_status(&credential));
            let _ = tx.send(ResultEvent {
                scope,
                result: ResultPayload::Status(result, startup),
            });
        });
        cx.notify();
    }
    fn refresh_models(&mut self, cx: &mut Context<Self>) {
        if !self.authenticated {
            self.notifications
                .warning("Log in and check your device login before loading models.");
            cx.notify();
            return;
        }
        if self.model_picker.read(cx).loading {
            return;
        }
        let Some(credential) = self.valid_credential(cx) else {
            return;
        };
        self.model_picker
            .update(cx, |picker, cx| picker.begin_load(cx));
        self.notifications
            .working(Source::Models, "Loading model catalog…");
        let tx = self.tx.clone();
        let scope = self.session.scope();
        std::thread::spawn(move || {
            let result = credential
                .ensure_valid()
                .map_err(anyhow::Error::from)
                .and_then(|()| gateway::models(credential.access_token()));
            let _ = tx.send(ResultEvent {
                scope,
                result: ResultPayload::Models(result),
            });
        });
    }
    fn finish_setup(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.busy {
            return;
        }
        let picker = self.model_picker.read(cx);
        let model = picker.selected.clone();
        let grammar = self.grammar_input.read(cx).content.trim().to_owned();
        let improve = self.improve_input.read(cx).content.trim().to_owned();
        let hotkey = self.hotkey_input.read(cx).content.trim().to_owned();
        let missing = if !self.authenticated {
            Some("Log in to Wiesel first.")
        } else if picker.loading {
            Some("Wait for the model catalog to finish loading.")
        } else if picker.models.is_empty() {
            Some("Refresh the model catalog before saving.")
        } else if !picker.models.contains(&model) {
            Some("Select an available model.")
        } else if grammar.is_empty() || improve.is_empty() {
            Some("Writing prompts cannot be empty.")
        } else if parse_hotkey(&hotkey).is_err() {
            Some("Choose a valid shortcut with Command, Control, or Option.")
        } else {
            None
        };
        if let Some(message) = missing {
            self.notifications.warning(message);
            cx.notify();
            return;
        }
        let previous = self.settings.hotkey.clone();
        if let Err(e) = self.register(&hotkey) {
            self.notifications.report(
                Source::Hotkey,
                "Could not activate shortcut. Choose another and retry.",
                &e,
            );
            cx.notify();
            return;
        }
        self.notifications.clear(Source::Hotkey);
        let mut next = self.settings.clone();
        next.hotkey = hotkey;
        next.model = model;
        next.grammar_prompt = grammar;
        next.improve_prompt = improve;
        next.onboarded = true;
        match settings::save(&next) {
            Ok(()) => {
                self.settings = next;
                self.page = Page::Launcher;
                self.notifications.clear(Source::Settings);
                self.notifications.success("Settings saved. Ready to go.");
                window.focus(&self.launcher_input.focus_handle(cx), cx);
            }
            Err(e) => {
                self.notifications.report(
                    Source::Settings,
                    "Could not save settings. Try again.",
                    &e,
                );
                self.rollback_hotkey(&previous);
            }
        }
        cx.notify();
    }
    fn request(&mut self, messages: Vec<Message>, chat: bool, cx: &mut Context<Self>) {
        if !self.authenticated {
            self.notifications
                .warning("Log in to Wiesel and check your device login first.");
            cx.notify();
            return;
        }
        let Some(credential) = self.valid_credential(cx) else {
            return;
        };
        let scope = self.session.scope();
        self.busy = true;
        self.notifications.clear(Source::Keychain);
        self.notifications.working(Source::Request, "Thinking…");
        let model = self.settings.model.clone();
        let tx = self.tx.clone();
        if chat {
            self.streaming_chat = Some(Message::new("assistant", ""));
        }
        std::thread::spawn(move || {
            let result = credential
                .ensure_valid()
                .map_err(anyhow::Error::from)
                .and_then(|()| {
                    if chat {
                        gateway::complete_stream(
                            credential.access_token(),
                            &model,
                            &messages,
                            |delta| {
                                tx.send(ResultEvent {
                                    scope,
                                    result: ResultPayload::ChatDelta(delta.to_owned()),
                                })
                                .context("Chat window closed")
                            },
                        )
                    } else {
                        gateway::complete(credential.access_token(), &model, &messages)
                    }
                });
            let _ = tx.send(ResultEvent {
                scope,
                result: ResultPayload::Completion(result, chat),
            });
        });
        cx.notify();
    }
    fn run_quick_action(
        &mut self,
        action: QuickAction,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.busy {
            return;
        }
        match action {
            QuickAction::Chat => {
                self.page = Page::Chat;
                window.focus(&self.composer.focus_handle(cx), cx);
                cx.notify();
            }
            QuickAction::Add => {
                self.notifications
                    .info("Custom quick actions are coming soon.");
                cx.notify();
            }
            _ => self.writing(action, cx),
        }
    }
    fn writing(&mut self, action: QuickAction, cx: &mut Context<Self>) {
        if self.busy {
            return;
        }
        let Some(prompt) = action.prompt(&self.settings) else {
            return;
        };
        let Some(text) = self.selection.text.clone() else {
            self.notifications
                .warning("Select text in another app and press your shortcut.");
            cx.notify();
            return;
        };
        if text.len() > 100_000 {
            self.notifications
                .warning("Selection is too large (100 KB max). Select less text.");
            cx.notify();
            return;
        }
        let prompt = prompt.to_owned();
        self.selection.snapshot_writing();
        self.page = Page::Writing;
        self.result.clear();
        self.writing_title = action.title().into();
        self.request(
            vec![Message::new("system", prompt), Message::new("user", text)],
            false,
            cx,
        );
    }
    fn submit_launcher(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.busy || self.launcher_input.read(cx).is_composing() {
            return;
        }
        let text = self.launcher_input.read(cx).content.trim().to_owned();
        if text.is_empty() {
            self.run_quick_action(QuickAction::Chat, window, cx);
            return;
        }
        self.messages.clear();
        self.failed_chat.clear();
        self.chat_follow_bottom = true;
        self.composer.update(cx, |input, cx| input.set(text, cx));
        self.launcher_input
            .update(cx, |input, cx| input.set("", cx));
        self.run_quick_action(QuickAction::Chat, window, cx);
        self.send_chat(cx);
    }

    fn follow_chat_bottom(&self) {
        if self.chat_follow_bottom {
            self.chat_scroll.scroll_to_bottom();
        }
    }

    fn send_chat(&mut self, cx: &mut Context<Self>) {
        if self.busy {
            return;
        }
        let text = self.composer.read(cx).content.trim().to_owned();
        if text.is_empty() {
            return;
        }
        if self.messages.iter().map(|m| m.content.len()).sum::<usize>() + text.len() > 200_000 {
            self.notifications
                .warning("Conversation is too large. Start a new chat.");
            cx.notify();
            return;
        }
        // request validates the device session before the draft is cleared.
        self.messages.push(Message::new("user", text));
        let mut messages = vec![Message::new(
            "system",
            "You are a helpful, clear and concise assistant.",
        )];
        messages.extend(self.messages.clone());
        self.request(messages, true, cx);
        if self.busy {
            self.follow_chat_bottom();
            self.composer.update(cx, |i, cx| i.set("", cx));
        } else {
            self.messages.pop();
        }
    }
    fn key_down(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        self.notifications.suppress_motion = true;
        self.key_down_impl(event, window, cx);
        self.notifications
            .update(Instant::now(), self.idle_notification());
        self.notifications.suppress_motion = false;
    }
    fn key_down_impl(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let key = &event.keystroke;
        if self.recording {
            cx.stop_propagation();
            if key.key == "escape" {
                self.recording = false;
                self.notifications.info("Shortcut recording cancelled.");
                cx.notify();
                return;
            }
            let code = match key.key.as_str() {
                "space" => "Space".into(),
                "enter" => "Enter".into(),
                "tab" => "Tab".into(),
                s if s.len() == 1 && s.as_bytes()[0].is_ascii_alphabetic() => {
                    format!("Key{}", s.to_uppercase())
                }
                s if s.len() == 1 && s.as_bytes()[0].is_ascii_digit() => format!("Digit{s}"),
                _ => {
                    self.notifications
                        .warning("Choose a letter, number, Space, Enter, or Tab with modifiers.");
                    cx.notify();
                    return;
                }
            };
            let mut parts = vec![];
            if key.modifiers.platform {
                parts.push("Super".to_owned());
            }
            if key.modifiers.control {
                parts.push("Control".to_owned());
            }
            if key.modifiers.alt {
                parts.push("Alt".to_owned());
            }
            if key.modifiers.shift {
                parts.push("Shift".to_owned());
            }
            if !key.modifiers.platform && !key.modifiers.control && !key.modifiers.alt {
                self.notifications
                    .warning("Include Command, Control, or Option.");
                cx.notify();
                return;
            }
            parts.push(code);
            self.hotkey_input
                .update(cx, |i, cx| i.set(parts.join("+"), cx));
            self.recording = false;
            self.apply_hotkey(cx);
            return;
        }
        if self.page == Page::Setup
            && key.key == "enter"
            && !key.modifiers.shift
            && self.login_focus.is_focused(window)
        {
            self.start_login(window, cx);
            cx.stop_propagation();
        } else if key.key == "escape" {
            if self.session.attempt.is_some() || self.session.exchanging {
                self.cancel_login(cx);
            }
            cx.hide();
            cx.stop_propagation();
        } else if self.page == Page::Launcher {
            if key.key == "enter" && !key.modifiers.shift {
                self.submit_launcher(window, cx);
                cx.stop_propagation();
            } else if !self.launcher_input.focus_handle(cx).is_focused(window)
                && let Some(action) = QUICK_ACTIONS
                    .iter()
                    .copied()
                    .find(|action| action.shortcut() == key.key)
            {
                // Letter shortcuts must not trigger actions while typing a prompt.
                self.run_quick_action(action, window, cx);
                cx.stop_propagation();
            }
        } else if self.page == Page::Chat && key.key == "enter" && !key.modifiers.shift {
            self.send_chat(cx);
            cx.stop_propagation();
        }
    }
    fn button(id: &'static str, title: &str) -> Stateful<Div> {
        div()
            .id(id)
            .h(px(28.))
            .flex_shrink_0()
            .flex()
            .items_center()
            .justify_center()
            .gap(px(6.))
            .px(px(10.))
            .rounded(px(6.))
            .border_1()
            .border_color(rgba(theme::BORDER))
            .text_color(rgb(theme::MUTED))
            .text_size(px(11.))
            .cursor_pointer()
            .hover(|s| s.bg(rgb(theme::ACCENT)).text_color(rgb(theme::FOREGROUND)))
            .child(title.to_owned())
    }
    fn icon_button(id: &'static str, icon: &'static str, label: &'static str) -> Stateful<Div> {
        div()
            .id(id)
            .size(px(28.))
            .flex_shrink_0()
            .flex()
            .items_center()
            .justify_center()
            .rounded(px(6.))
            .cursor_pointer()
            .hover(|s| s.bg(rgb(theme::ACCENT)))
            .tooltip(move |_, cx| cx.new(|_| HeaderTooltip(label)).into())
            .child(theme::icon(icon, 16.).text_color(rgb(theme::MUTED)))
    }

    fn label(text: &str) -> Div {
        div()
            .text_size(px(12.))
            .text_color(rgb(theme::MUTED))
            .child(text.to_owned())
    }
    fn permission_controls(&self, cx: &mut Context<Self>) -> Div {
        div().flex().gap_2()
            .child(Self::button("privacy-settings", "Privacy settings ↗").on_click(|_, _, cx| cx.open_url("x-apple.systempreferences:com.apple.preference.security?Privacy_Accessibility")))
            .child(Self::button("check-permissions", "Check again").on_click(cx.listener(|this, _, _, cx| {
                this.refresh_permissions(cx);
                this.sync_permission_issue();
                if this.accessibility_granted {
                    this.notifications.success("Accessibility enabled.");
                } else {
                    this.notifications.warning("Enable Wiesel in System Settings → Accessibility.");
                }
                cx.notify();
            })))
    }
    fn login_controls(&self, cx: &mut Context<Self>) -> Div {
        let pending = self.session.attempt.is_some() || self.session.exchanging;
        let saved = self.session.credential.is_some();
        div().flex().flex_col().gap_2()
            .child(Self::label("2 · Wiesel account"))
            .child(Self::button("login", if pending { "Reopen Login / Sign up ↗" } else { "Login / Sign up ↗" })
                .track_focus(&self.login_focus)
                .on_click(cx.listener(|this, _, window, cx| this.start_login(window, cx))))
            .child(Self::label("Opens your system browser at wiesel.run. Enter when login is focused · ⌘⇧L from any screen."))
            .when(pending, |d| d
                .child(Self::label(if self.session.exchanging { "Completing login…" } else { "Waiting for browser login (up to 10 minutes)…" }))
                .child(Self::button("cancel-login", "Cancel login").on_click(cx.listener(|this, _, _, cx| this.cancel_login(cx)))))
            .child(Self::label("New account? Finish signup and email verification on the website, then Reopen Login / Sign up here for a fresh attempt."))
            .when(saved, |d| d.child(Self::button("check-login", if self.status_loading { "Checking login…" } else { "Check login" })
                .on_click(cx.listener(|this, _, _, cx| this.refresh_status(false, cx)))))
            .when(self.authenticated, |d| d.child(Self::label("✓ Logged in · device token stored only in macOS Keychain. Expiry requires login again; no refresh.")))
            .when_some(self.device_status.as_ref(), |d, status| d.child(Self::label(&format!(
                "Plan: {} · virtual credits: {} (informational; service decides request allowance)",
                match status.plan { auth::Plan::Free => "Free", auth::Plan::Starter => "Starter", auth::Plan::Unlimited => "Unlimited" },
                status.virtual_credits))))
            .when_some(self.session.credential.as_ref(), |d, credential| d.child(Self::label(&format!(
                "Device login expires in about {} hours; log in again after expiry.",
                settings::utc_now_ms().ok().map(|now| (credential.expires_at().saturating_sub(now).max(0) as u64).div_ceil(3_600_000)).unwrap_or(0)))))
            .when(self.recovery.blocks_login(), |d| d.child(Self::label(if self.recovery.restore_only() {
                "Saved login could not be read safely. Unlock Keychain, then Retry restoring login. No later saved login will be deleted."
            } else {
                "Saved-login cleanup failed. Unlock Keychain, then Retry saved login removal. This device may still be signed in after relaunch. Cleanup targets only the observed device or corrupt metadata."
            })))
            .when(saved || self.recovery.blocks_login(), |d| d.child(Self::button("sign-out", if self.recovery.restore_only() { "Retry restoring login" } else if self.recovery.blocks_login() { "Retry saved login removal" } else { "Sign out" })
                .on_click(cx.listener(|this, _, _, cx| {
                    if this.recovery.restore_only() { this.retry_restore(cx); } else { this.sign_out(cx); }
                }))))
    }
    fn setup(&self, cx: &mut Context<Self>) -> Stateful<Div> {
        // Keep scrollable content at its natural height instead of shrinking every field to fit.
        div().id("settings-scroll").flex_1().min_h_0().overflow_y_scroll()
            .child(div().w_full().flex().flex_col().gap_3().p_5()
            .child(div().text_size(px(24.)).font_weight(FontWeight::BOLD).child(if self.settings.onboarded { "Make it yours" } else { "Welcome to Wiesel" }))
            .child(Self::label("Your writing assistant, one shortcut away."))
            .child(Self::label("1 · Global hotkey"))
            .child(div().flex().gap_2().child(div().flex_1().child(self.hotkey_input.clone()))
                .child(Self::button("record", if self.recording { "Press shortcut…" } else { "Record shortcut" }).on_click(cx.listener(|this, _, window, cx| { this.recording = true; this.notifications.info("Press your shortcut. Escape cancels."); window.focus(&this.focus, cx); cx.notify(); }))))
            .child(Self::button("apply-hotkey", "Apply shortcut").on_click(cx.listener(|this, _, _, cx| this.apply_hotkey(cx))))
            .child(Self::label("Command (⌘) is called Super in the field above. Recorded shortcuts activate immediately; typed changes need Apply shortcut."))
            .child(self.login_controls(cx))
            .child(Self::label("3 · Model"))
            .child(self.model_picker.clone())
            .child(Self::button("refresh-models", "Refresh models").on_click(cx.listener(|this, _, _, cx| this.refresh_models(cx))))
            .child(Self::label("The authenticated Wiesel catalog lists model IDs. Choose a text/chat model; only text/chat requests are supported."))
            .child(Self::label("Custom prompt · Fix grammar"))
            .child(self.grammar_input.clone())
            .child(Self::label("Custom prompt · Improve writing"))
            .child(self.improve_input.clone())
            .child(Self::label("Writing actions send selected text to wiesel.run and its model provider. Nothing is sent until you choose an action."))
            .child(Self::label("Required for selected text: System Settings → Privacy & Security → Accessibility (Gerätesteuerung und Datenzugriff). Enable Wiesel; its live status appears in the bottom bar."))
            .child(self.permission_controls(cx))
            .child(Self::label("Wiesel captures selected text using ⌘C and restores your previous clipboard before opening. Release the shortcut keys and keep the source app active. Clipboard managers may retain the temporary selection."))
            .child(Self::label("Input Monitoring, Screen Recording, and Automation are not required. On newer macOS versions, allow Wiesel to paste from other apps if asked so it can save and restore the clipboard."))
            .child(Self::label("If macOS shows Wiesel enabled but the bottom bar still requests access after a rebuild, remove the old entry, add this Wiesel.app again, and relaunch it."))
            .child(Self::button("save", "Save & start").on_click(cx.listener(|this, _, window, cx| this.finish_setup(window, cx)))))
    }
    fn launcher(&self, cx: &mut Context<Self>) -> Stateful<Div> {
        div()
            .id("quick-actions-scroll")
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .flex()
            .flex_col()
            .gap(px(12.))
            .p(px(20.))
            .children(QUICK_ACTIONS.chunks(3).map(|actions| {
                div().flex().flex_shrink_0().gap(px(12.)).w_full().children(
                    actions.iter().copied().map(|action| {
                        div()
                            .id(action.id())
                            .relative()
                            .flex_1()
                            .h(px(106.))
                            .flex_shrink_0()
                            .flex()
                            .flex_col()
                            .items_center()
                            .justify_center()
                            .gap(px(12.))
                            .rounded(px(10.))
                            .border_1()
                            .border_color(rgba(theme::BORDER))
                            .bg(rgb(theme::CARD))
                            .cursor_pointer()
                            .when(action == QuickAction::Add, |d| {
                                d.text_color(rgb(theme::MUTED))
                            })
                            .hover(|s| s.bg(rgb(theme::ACCENT)).border_color(rgb(theme::RING)))
                            .child(theme::icon(action.icon(), 24.).text_color(rgb(
                                if action == QuickAction::Add {
                                    theme::MUTED
                                } else {
                                    theme::FOREGROUND
                                },
                            )))
                            .child(
                                div()
                                    .text_size(px(13.))
                                    .font_weight(FontWeight::MEDIUM)
                                    .child(action.title()),
                            )
                            .child(div().absolute().top(px(10.)).right(px(10.)).child(
                                theme::badge(if action == QuickAction::Add {
                                    "Soon"
                                } else {
                                    action.shortcut()
                                }),
                            ))
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.run_quick_action(action, window, cx);
                            }))
                    }),
                )
            }))
    }
    fn chat(&self, cx: &mut Context<Self>) -> Div {
        div()
            .flex_1()
            .min_h_0()
            .flex()
            .flex_col()
            .child(
                div()
                    .id("chat-history")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .track_scroll(&self.chat_scroll)
                    .on_scroll_wheel(cx.listener(|this, event: &ScrollWheelEvent, _, _| {
                        // Stop following immediately on an upward gesture, even mid-stream.
                        let upward = match event.delta {
                            ScrollDelta::Pixels(delta) => delta.y > px(0.),
                            ScrollDelta::Lines(delta) => delta.y > 0.,
                        };
                        if upward && this.chat_scroll.max_offset().y > px(0.) {
                            this.chat_follow_bottom = false;
                        }
                    }))
                    .flex()
                    .flex_col()
                    .gap(px(18.))
                    .px(px(24.))
                    .py(px(16.))
                    .when(self.messages.is_empty(), |d| {
                        d.child(
                            div()
                                .flex_1()
                                .flex()
                                .flex_col()
                                .items_center()
                                .justify_center()
                                .gap_2()
                                .child(theme::icon("chat", 24.).text_color(rgb(theme::MUTED)))
                                .child(Self::label("Start a conversation"))
                                .child(Self::label("Enter sends · history stays in memory only")),
                        )
                    })
                    .children(
                        self.messages
                            .iter()
                            .chain(self.streaming_chat.iter())
                            .filter(|message| !message.content.is_empty())
                            .enumerate()
                            .map(|(index, message)| {
                                let user = message.role == "user";
                                let text = message.content.clone();
                                div()
                                    .w_full()
                                    .flex_shrink_0()
                                    .flex()
                                    .when(user, |d| d.justify_end())
                                    .child(
                                        div()
                                            .flex()
                                            .flex_col()
                                            .gap(px(if user { 6. } else { 8. }))
                                            .when(user, |d| {
                                                d.w(relative(0.83))
                                                    .px(px(16.))
                                                    .py(px(12.))
                                                    .rounded(px(10.))
                                                    .border_1()
                                                    .border_color(rgba(theme::BORDER))
                                                    .bg(rgb(theme::ACCENT))
                                            })
                                            .when(!user, |d| d.w_full())
                                            .child(
                                                div()
                                                    .text_size(px(if user { 13. } else { 14. }))
                                                    .line_height(px(if user { 19. } else { 21. }))
                                                    .child(text.clone()),
                                            )
                                            .when(!user && index < self.messages.len(), |d| {
                                                d.child(
                                                    div()
                                                        .id(("copy-message", index))
                                                        .flex()
                                                        .items_center()
                                                        .gap(px(6.))
                                                        .text_size(px(11.))
                                                        .text_color(rgb(theme::MUTED))
                                                        .cursor_pointer()
                                                        .hover(|s| {
                                                            s.text_color(rgb(theme::FOREGROUND))
                                                        })
                                                        .child(
                                                            theme::icon("copy", 13.)
                                                                .text_color(rgb(theme::MUTED)),
                                                        )
                                                        .child("Copy")
                                                        .on_click(cx.listener(
                                                            move |this, _, _, cx| {
                                                                cx.write_to_clipboard(
                                                                    ClipboardItem::new_string(
                                                                        text.clone(),
                                                                    ),
                                                                );
                                                                this.notifications
                                                                    .success("Copied.");
                                                                cx.notify();
                                                            },
                                                        )),
                                                )
                                            }),
                                    )
                            }),
                    ),
            )
            .when(!self.failed_chat.is_empty(), |d| {
                d.child(div().px_5().py_2().child(
                    Self::button("restore-failed", "Restore failed message").on_click(cx.listener(
                        |this, _, _, cx| {
                            if this.busy {
                                return;
                            }
                            if this.composer.read(cx).content.is_empty() {
                                if let Some(text) = this.failed_chat.pop_front() {
                                    this.composer.update(cx, |i, cx| i.set(text, cx));
                                    this.notifications.success("Failed message restored.");
                                }
                            } else {
                                this.notifications.warning(
                                    "Send or clear your draft before restoring the failed message.",
                                );
                            }
                            cx.notify();
                        },
                    )),
                ))
            })
            .child(
                div()
                    .flex_shrink_0()
                    .h(px(72.))
                    .px(px(20.))
                    .py(px(12.))
                    .border_t_1()
                    .border_color(rgba(theme::BORDER))
                    .child(
                        div()
                            .h(px(46.))
                            .w_full()
                            .flex()
                            .items_center()
                            .gap(px(12.))
                            .pl(px(10.))
                            .pr(px(10.))
                            .rounded(px(8.))
                            .border_1()
                            .border_color(rgba(theme::INPUT_BORDER))
                            .bg(rgb(theme::CARD))
                            .child(div().flex_1().min_w_0().child(self.composer.clone()))
                            .child(
                                div()
                                    .font_family(theme::MONO)
                                    .text_size(px(10.))
                                    .text_color(rgb(theme::MUTED))
                                    .child("↵"),
                            )
                            .child(
                                div()
                                    .id("send")
                                    .size(px(28.))
                                    .flex_shrink_0()
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .rounded(px(6.))
                                    .bg(rgb(if self.busy {
                                        theme::ACCENT
                                    } else {
                                        theme::PRIMARY
                                    }))
                                    .text_color(rgb(if self.busy {
                                        theme::MUTED
                                    } else {
                                        theme::CARD
                                    }))
                                    .cursor_pointer()
                                    .hover(|s| s.bg(rgb(theme::MUTED)))
                                    .child(if self.busy {
                                        div().text_size(px(12.)).child("…").into_any_element()
                                    } else {
                                        theme::icon("arrow-up", 16.)
                                            .text_color(rgb(theme::CARD))
                                            .into_any_element()
                                    })
                                    .on_click(cx.listener(|this, _, _, cx| this.send_chat(cx))),
                            ),
                    ),
            )
    }
    fn writing_view(&self, cx: &mut Context<Self>) -> Div {
        div()
            .flex_1()
            .min_h_0()
            .flex()
            .flex_col()
            .child(
                div()
                    .id("writing-scroll")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .flex()
                    .flex_col()
                    .gap(px(18.))
                    .px(px(24.))
                    .py(px(16.))
                    .child(
                        div().w_full().flex().justify_end().child(
                            div()
                                .w(relative(0.83))
                                .px(px(16.))
                                .py(px(12.))
                                .rounded(px(10.))
                                .border_1()
                                .border_color(rgba(theme::BORDER))
                                .bg(rgb(theme::ACCENT))
                                .flex()
                                .flex_col()
                                .gap(px(6.))
                                .child(
                                    div()
                                        .text_size(px(10.))
                                        .font_weight(FontWeight::MEDIUM)
                                        .text_color(rgb(theme::MUTED))
                                        .child("YOU · SELECTED TEXT"),
                                )
                                .child(
                                    div()
                                        .text_size(px(13.))
                                        .line_height(px(19.))
                                        .child(self.selection.writing_source.clone()),
                                ),
                        ),
                    )
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap(px(8.))
                            .child(
                                div()
                                    .text_size(px(11.))
                                    .font_weight(FontWeight::MEDIUM)
                                    .text_color(rgb(theme::MUTED))
                                    .child("Wiesel"),
                            )
                            .child(div().text_size(px(14.)).line_height(px(21.)).child(
                                if self.busy {
                                    String::new()
                                } else {
                                    self.result.clone()
                                },
                            ))
                            .when(!self.result.is_empty(), |d| {
                                d.child(
                                    Self::button("copy-result", "Copy result")
                                        .child(
                                            theme::icon("copy", 13.).text_color(rgb(theme::MUTED)),
                                        )
                                        .on_click(cx.listener(|this, _, _, cx| {
                                            cx.write_to_clipboard(ClipboardItem::new_string(
                                                this.result.clone(),
                                            ));
                                            this.notifications
                                                .success("Copied. Paste it back into your app.");
                                            cx.notify();
                                        })),
                                )
                            }),
                    ),
            )
            .child(
                div()
                    .flex_shrink_0()
                    .border_t_1()
                    .border_color(rgba(theme::BORDER))
                    .px_5()
                    .py_3()
                    .child(Self::label(
                        "Your original text is never replaced automatically.",
                    )),
            )
    }
}
impl Render for Wiesel {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.notifications
            .update(Instant::now(), self.idle_notification());
        let content = match self.page {
            Page::Setup => self.setup(cx).into_any_element(),
            Page::Launcher => self.launcher(cx).into_any_element(),
            Page::Chat => self.chat(cx).into_any_element(),
            Page::Writing => self.writing_view(cx).into_any_element(),
        };
        div()
            .size_full()
            .rounded(px(14.))
            .border_1()
            .border_color(rgba(theme::BORDER))
            .overflow_hidden()
            .bg(rgb(theme::BACKGROUND))
            .text_color(rgb(theme::FOREGROUND))
            .font_family(theme::SANS)
            .text_size(px(13.))
            .flex()
            .flex_col()
            .track_focus(&self.focus)
            .on_key_down(cx.listener(Self::key_down))
            .child(
                div()
                    .h(px(56.))
                    .flex_shrink_0()
                    .flex()
                    .items_center()
                    .gap(px(14.))
                    .px(px(20.))
                    .border_b_1()
                    .border_color(rgba(theme::BORDER))
                    .when(self.page == Page::Launcher, |d| {
                        d.child(div().flex_1().min_w_0().child(self.launcher_input.clone()))
                    })
                    .when(self.page != Page::Launcher, |d| {
                        d.when(self.page != Page::Setup, |d| {
                            d.child(
                                div()
                                    .id("back")
                                    .size(px(28.))
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .rounded(px(6.))
                                    .cursor_pointer()
                                    .text_color(rgb(theme::MUTED))
                                    .hover(|s| {
                                        s.bg(rgb(theme::ACCENT)).text_color(rgb(theme::FOREGROUND))
                                    })
                                    .child(
                                        theme::icon("arrow-left", 18.)
                                            .text_color(rgb(theme::MUTED)),
                                    )
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        if !this.busy {
                                            if this.session.attempt.is_some()
                                                || this.session.exchanging
                                            {
                                                this.cancel_login(cx);
                                            }
                                            this.page = Page::Launcher;
                                            window.focus(&this.launcher_input.focus_handle(cx), cx);
                                            cx.notify();
                                        }
                                    })),
                            )
                            .child(div().w(px(1.)).h(px(18.)).bg(rgba(theme::BORDER)))
                        })
                        .child(theme::icon(
                            match self.page {
                                Page::Writing => QUICK_ACTIONS
                                    .iter()
                                    .find(|action| action.title() == self.writing_title)
                                    .map_or("spell-check", |action| action.icon()),
                                _ => "chat",
                            },
                            18.,
                        ))
                        .child(
                            div()
                                .flex_1()
                                .font_weight(FontWeight::MEDIUM)
                                .text_size(px(14.))
                                .on_mouse_down(MouseButton::Left, |_, window, _| {
                                    window.start_window_move();
                                })
                                .child(match self.page {
                                    Page::Writing => self.writing_title.clone(),
                                    Page::Setup => "Wiesel · Settings".to_owned(),
                                    _ => "Chat".to_owned(),
                                }),
                        )
                    })
                    .when(self.page == Page::Chat, |d| {
                        d.child(Self::icon_button("new-chat", "plus", "New chat").on_click(
                            cx.listener(|this, _, _, cx| {
                                if !this.busy {
                                    this.messages.clear();
                                    this.failed_chat.clear();
                                    this.chat_follow_bottom = true;
                                    this.chat_scroll.set_offset(point(px(0.), px(0.)));
                                    this.notifications.clear(Source::Request);
                                    this.notifications.success("New chat ready.");
                                    cx.notify();
                                }
                            }),
                        ))
                    })
                    .child(
                        Self::icon_button("settings", "settings", "Settings").on_click(
                            cx.listener(|this, _, _, cx| {
                                if !this.busy {
                                    this.page = Page::Setup;
                                    cx.notify();
                                }
                            }),
                        ),
                    )
                    .child(
                        Self::icon_button("hide", "minus", "Hide").on_click(|_, _, cx| cx.hide()),
                    ),
            )
            .child(content)
            .child(self.notifications.render())
    }
}
fn main() {
    let application = gpui_platform::application().with_assets(theme::Assets);
    let (url_tx, urls) = mpsc::sync_channel(16);
    application.on_open_urls(move |raw_urls| {
        for raw in raw_urls {
            // Oversized input is represented as invalid, never truncated into a valid URL.
            let raw = zeroize::Zeroizing::new(raw);
            let raw = if raw.len() <= 1024 {
                raw
            } else {
                zeroize::Zeroizing::new(String::new())
            };
            let _ = url_tx.try_send(raw);
        }
    });
    application.run(move |cx: &mut App| {
        cx.set_reduce_motion(
            objc2_app_kit::NSWorkspace::sharedWorkspace().accessibilityDisplayShouldReduceMotion(),
        );
        theme::load_fonts(cx);
        cx.on_action(|_: &input::Quit, cx| cx.quit());
        cx.bind_keys([KeyBinding::new("cmd-q", input::Quit, None)]);
        cx.bind_keys([
            KeyBinding::new("backspace", input::Backspace, Some("TextInput")),
            KeyBinding::new("delete", input::Delete, Some("TextInput")),
            KeyBinding::new("left", input::Left, Some("TextInput")),
            KeyBinding::new("right", input::Right, Some("TextInput")),
            KeyBinding::new("shift-left", input::SelectLeft, Some("TextInput")),
            KeyBinding::new("shift-right", input::SelectRight, Some("TextInput")),
            KeyBinding::new("cmd-a", input::SelectAll, Some("TextInput")),
            KeyBinding::new("cmd-v", input::Paste, Some("TextInput")),
            KeyBinding::new("cmd-c", input::Copy, Some("TextInput")),
            KeyBinding::new("cmd-x", input::Cut, Some("TextInput")),
            KeyBinding::new("home", input::Home, Some("TextInput")),
            KeyBinding::new("end", input::End, Some("TextInput")),
        ]);
        let bounds = Bounds::centered(None, size(px(800.), px(450.)), cx);
        let handle = match cx.open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                titlebar: None,
                app_owns_titlebar_drag: true,
                is_resizable: false,
                is_minimizable: false,
                window_min_size: Some(size(px(580.), px(450.))),
                ..Default::default()
            },
            |window, cx| cx.new(|cx| Wiesel::new(urls, window, cx)),
        ) {
            Ok(handle) => handle,
            Err(error) => {
                // No notification surface exists yet; fail gracefully with a diagnostic.
                eprintln!("Cannot open Wiesel window: {error:#}");
                cx.quit();
                return;
            }
        };
        cx.bind_keys([KeyBinding::new("cmd-shift-l", Login, None)]);
        cx.on_action(move |_: &Login, cx| {
            let _ = handle.update(cx, |app, window, cx| {
                app.notifications.suppress_motion = true;
                app.start_login(window, cx);
                app.notifications
                    .update(Instant::now(), app.idle_notification());
                app.notifications.suppress_motion = false;
            });
        });
        cx.spawn(async move |cx| {
            loop {
                cx.background_executor()
                    .timer(Duration::from_millis(40))
                    .await;
                if handle
                    .update(cx, |app, window, cx| app.poll(window, cx))
                    .is_err()
                {
                    break;
                }
            }
        })
        .detach();
        cx.activate(true);
    });
}
