#[cfg(not(target_os = "macos"))]
compile_error!("Wiesel currently supports macOS only (native selection and Keychain backend).");

mod action_builder;
#[cfg(test)]
mod action_builder_tests;
mod auth;
#[cfg(test)]
mod chat_tests;
mod clipboard_capture;
mod completion_backend;
mod diagnostics;
mod gateway;
mod input;
mod model_picker;
mod navigation;
mod notifications;
mod quick_actions;
mod selection;
mod settings;
mod theme;

use auth::{AuthError, DesktopClient, DeviceCredential};
use diagnostics::{
    Category, Completion, DiagnosticError, Diagnostics, Event, EventKind, FailureCode, Stage,
};
use gateway::Message;
use global_hotkey::{GlobalHotKeyEvent, GlobalHotKeyManager, HotKeyState, hotkey::HotKey};
use gpui::{
    App, Bounds, ClipboardItem, Context, Div, Entity, FocusHandle, Focusable, FontWeight,
    KeyBinding, KeyDownEvent, MouseButton, Role, ScrollDelta, ScrollHandle, ScrollWheelEvent,
    Stateful, Window, WindowBounds, WindowOptions, actions, div, point, prelude::*, px, relative,
    rgb, rgba, size,
};
use input::TextInput;
use navigation::{EntryId, Navigation};
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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Screen {
    Settings,
    Launcher,
    Chat,
    Writing,
    ActionManager,
    ActionEditor,
}
struct ScreenLifecycle {
    restore_focus: Option<FocusHandle>,
    startup: bool,
}

// Completion ownership survives an entry's pop, but cannot be reused by a newer request.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct CompletionScope {
    entry: EntryId,
    generation: u64,
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
    completion: Option<CompletionScope>,
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
            completion: None,
        }
    }
    fn accepts(&self, scope: EventScope) -> bool {
        let current = self.scope();
        current.generation == scope.generation && current.device_id == scope.device_id
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
            device_id: Some(uuid::Uuid::from_u128(1)),
            completion: None,
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
#[derive(Clone, Copy)]
enum DiagnosticControl {
    OpenFolder,
    ClearLogs,
}
impl DiagnosticControl {
    fn stage(self) -> Stage {
        match self {
            Self::OpenFolder => Stage::OpenFolder,
            Self::ClearLogs => Stage::ClearLogs,
        }
    }
}
enum DiagnosticUpdate {
    Initialized(Result<Diagnostics, DiagnosticError>),
    Controlled(DiagnosticControl, Result<(), DiagnosticError>),
}
fn diagnostic_failure_message(error: DiagnosticError) -> &'static str {
    match error {
        DiagnosticError::OtherSessionActive => {
            "Close other Wiesel instances, then clear logs again."
        }
        DiagnosticError::UnsafePath => {
            "Unsafe logs path refused. Check ~/Library/Logs/Wiesel and relaunch."
        }
        DiagnosticError::InvalidHome => {
            "Logs folder unavailable. Check your home folder and relaunch."
        }
        DiagnosticError::QueueFull => "Diagnostics busy. Wait and try again.",
        DiagnosticError::StorageBudget => {
            "Logs storage full. Close other Wiesel instances and clear logs."
        }
        DiagnosticError::Io(std::io::ErrorKind::PermissionDenied) => {
            "Logs permission denied. Check folder permissions and relaunch."
        }
        _ => "Local diagnostics unavailable. Check disk space and folder permissions; relaunch.",
    }
}
fn finder_command(path: &std::path::Path) -> std::process::Command {
    let mut command = std::process::Command::new("/usr/bin/open");
    command
        .args(["-a", "Finder", "--"])
        .arg(path)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    command
}
fn run_diagnostic_control(
    diagnostics: &Diagnostics,
    control: DiagnosticControl,
) -> Result<(), DiagnosticError> {
    match control {
        DiagnosticControl::ClearLogs => diagnostics.clear()?.wait(),
        DiagnosticControl::OpenFolder => {
            let path = diagnostics.log_directory()?;
            let status = finder_command(&path)
                .status()
                .map_err(DiagnosticError::from)?;
            if status.success() {
                Ok(())
            } else {
                Err(DiagnosticError::Io(std::io::ErrorKind::Other))
            }
        }
    }
}

struct Wiesel {
    navigation: Navigation<Screen, ScreenLifecycle>,
    request_generation: u64,
    active_request: Option<CompletionScope>,
    #[cfg(test)]
    recorded_hides: Option<usize>,
    settings: Settings,
    actions: action_builder::ActionBuilder,
    diagnostics: Option<Diagnostics>,
    diagnostics_initializing: bool,
    diagnostics_busy: bool,
    #[cfg(test)]
    controlled_diagnostic_requests: Option<Vec<DiagnosticControl>>,
    diagnostics_tx: mpsc::Sender<DiagnosticUpdate>,
    diagnostics_rx: mpsc::Receiver<DiagnosticUpdate>,
    diagnostic_flush: Option<Completion>,
    diagnostic_flushed_at: Instant,
    diagnostics_open_focus: FocusHandle,
    diagnostics_clear_focus: FocusHandle,
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
    capture_scope: Option<EventScope>,
    failed_chat: std::collections::VecDeque<String>,
    result: String,
    writing_title: String,
    messages: Vec<Message>,
    tx: mpsc::Sender<ResultEvent>,
    rx: mpsc::Receiver<ResultEvent>,
    chat_scroll: ScrollHandle,
    chat_follow_bottom: bool,
    streaming_chat: Option<Message>,
    completion_backend: completion_backend::CompletionBackend,
}
impl Wiesel {
    fn current_screen(&self) -> Screen {
        self.navigation.current().component
    }

    fn screen_focus(&self, cx: &App) -> FocusHandle {
        match self.current_screen() {
            Screen::Launcher => self.launcher_input.focus_handle(cx),
            Screen::Chat => self.composer.focus_handle(cx),
            Screen::Settings => self.login_focus.clone(),
            Screen::Writing | Screen::ActionManager => self.focus.clone(),
            Screen::ActionEditor => self
                .actions
                .editor
                .as_ref()
                .map_or_else(|| self.focus.clone(), |editor| editor.name.focus_handle(cx)),
        }
    }

    /// Push without resetting retained screen state; capture the covered screen's focus.
    fn push_screen(
        &mut self,
        screen: Screen,
        startup: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> EntryId {
        debug_assert_eq!(self.navigation.root().component, Screen::Launcher);
        if screen == Screen::Launcher {
            self.pop_to_root(window, cx);
            return self.navigation.root().id;
        }
        if self.current_screen() == screen {
            return self.navigation.current().id;
        }
        let restore_focus = window.focused(cx).or_else(|| Some(self.screen_focus(cx)));
        let id = self.navigation.push(
            screen,
            ScreenLifecycle {
                restore_focus,
                startup,
            },
        );
        window.focus(&self.screen_focus(cx), cx);
        cx.notify();
        id
    }

    fn redirect_deleted_home_restore_focus(&mut self, deleted: &FocusHandle, cx: &App) {
        for lifecycle in self.navigation.lifecycles_mut() {
            if lifecycle.restore_focus.as_ref() == Some(deleted) {
                lifecycle.restore_focus = Some(self.launcher_input.focus_handle(cx));
            }
        }
    }

    fn push_settings(&mut self, startup: bool, window: &mut Window, cx: &mut Context<Self>) {
        if self.actions.dialog.is_some() {
            return;
        }
        let existing = self
            .navigation
            .entries()
            .find(|entry| entry.component == Screen::Settings)
            .map(|entry| entry.id);
        if let Some(id) = existing {
            if id != self.navigation.current().id {
                self.pop_to_screen(id, window, cx);
            }
            return;
        }
        self.push_screen(Screen::Settings, startup, window, cx);
    }

    // Built-in screen drafts are retained outside mounted entries. New destructive
    // editors must confirm all removals BEFORE invoking this atomic transition.
    fn pop_to_screen(&mut self, id: EntryId, window: &mut Window, cx: &mut Context<Self>) -> bool {
        if self.guard_action_removal(action_builder::Removal::To(id), window, cx) {
            return false;
        }
        let Some(entries) = self.navigation.pop_to(id) else {
            return false;
        };
        let mut restored = None;
        for entry in entries {
            restored = self.finish_pop(entry, cx);
        }
        window.focus(&restored.unwrap_or_else(|| self.screen_focus(cx)), cx);
        cx.notify();
        true
    }

    fn finish_pop(
        &mut self,
        entry: navigation::Entry<Screen, ScreenLifecycle>,
        cx: &mut Context<Self>,
    ) -> Option<FocusHandle> {
        if entry.component == Screen::ActionEditor {
            self.actions.editor = None;
        }
        if entry.component == Screen::Settings {
            self.recording = false;
            if self.session.attempt.is_some() || self.session.exchanging {
                self.cancel_login(cx);
            }
        }
        // Inference is not canceled: retained state and busy ownership outlive the entry.
        entry.on_pop.and_then(|lifecycle| lifecycle.restore_focus)
    }

    /// Returns false only at the permanent Launcher root.
    fn pop_screen(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        if self.navigation.is_root() {
            return false;
        }
        if self.guard_action_removal(action_builder::Removal::One, window, cx) {
            return true;
        }
        let Some(entry) = self.navigation.pop() else {
            return false;
        };
        let focus = self
            .finish_pop(entry, cx)
            .unwrap_or_else(|| self.screen_focus(cx));
        window.focus(&focus, cx);
        cx.notify();
        true
    }

    fn pop_to_root(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.guard_action_removal(action_builder::Removal::Root, window, cx) {
            return;
        }
        for entry in self.navigation.pop_to_root() {
            self.finish_pop(entry, cx);
        }
        window.focus(&self.launcher_input.focus_handle(cx), cx);
        cx.notify();
    }

    // A completed selection capture activates Home unless Settings or an inference is active.
    fn activate_from_hotkey(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.actions.dialog.is_some() {
            return;
        }
        if !self.busy && self.current_screen() != Screen::Settings {
            self.pop_to_root(window, cx);
        } else {
            window.focus(&self.screen_focus(cx), cx);
        }
    }

    fn hide_window(&mut self, cx: &mut Context<Self>) {
        #[cfg(test)]
        if let Some(hides) = &mut self.recorded_hides {
            *hides += 1;
            return;
        }
        cx.hide();
    }

    fn new(
        urls: mpsc::Receiver<zeroize::Zeroizing<String>>,
        diagnostics_tx: mpsc::Sender<DiagnosticUpdate>,
        diagnostics_rx: mpsc::Receiver<DiagnosticUpdate>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let mut notifications = Notifications::default();
        let mut settings_read_only = false;
        let settings = match settings::load() {
            Ok(s) => s,
            Err(e) => {
                settings_read_only = true;
                notifications.report(
                    Source::Settings,
                    "Settings could not be read. Saving is disabled; repair the settings file and relaunch.",
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
            navigation: Navigation::new(Screen::Launcher),
            request_generation: 0,
            active_request: None,
            #[cfg(test)]
            recorded_hides: None,
            settings,
            actions: action_builder::ActionBuilder::new(settings_read_only),
            diagnostics: None,
            diagnostics_initializing: true,
            diagnostics_busy: false,
            #[cfg(test)]
            controlled_diagnostic_requests: None,
            diagnostics_tx,
            diagnostics_rx,
            diagnostic_flush: None,
            diagnostic_flushed_at: Instant::now(),
            diagnostics_open_focus: cx.focus_handle().tab_stop(true),
            diagnostics_clear_focus: cx.focus_handle().tab_stop(true),
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
            composer: cx.new(|cx| {
                TextInput::new("Ask anything…", false, cx)
                    .with_accessibility_id("wiesel.chat.composer")
            }),
            launcher_input: cx.new(|cx| TextInput::launcher(cx).with_tab_stop()),
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
            capture_scope: None,
            failed_chat: std::collections::VecDeque::new(),
            result: String::new(),
            writing_title: String::new(),
            messages: vec![],
            tx,
            rx,
            chat_scroll: ScrollHandle::new(),
            chat_follow_bottom: true,
            streaming_chat: None,
            completion_backend: completion_backend::CompletionBackend::Gateway,
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
        }
        window.on_window_should_close(cx, |_, cx| {
            cx.hide();
            false
        });
        app.push_settings(true, window, cx);
        app.poll_diagnostics(cx);
        app.sync_permission_issue();
        app.refresh_status(true, cx);
        app
    }
    // Called only by the production entry point, never by in-process UI fixtures.
    fn register_diagnostic_shutdown(&mut self, cx: &mut Context<Self>) {
        cx.on_app_quit(|this, cx| {
            let diagnostics = this.diagnostics.take();
            cx.background_executor().spawn(async move {
                if let Some(diagnostics) = diagnostics {
                    let _ = diagnostics.record(
                        Event::new(Category::Application, EventKind::Succeeded),
                        None,
                    );
                    if let Err(error) = diagnostics.shutdown().and_then(Completion::wait) {
                        // Closed storage errors only; no arbitrary app/network errors.
                        eprintln!("Diagnostics shutdown incomplete: {error}");
                    }
                }
            })
        })
        .detach();
    }
    fn poll_diagnostics(&mut self, cx: &mut Context<Self>) {
        while let Ok(update) = self.diagnostics_rx.try_recv() {
            match update {
                DiagnosticUpdate::Initialized(result) => {
                    self.diagnostics_initializing = false;
                    match result {
                        Ok(diagnostics) => {
                            let _ = diagnostics.record(
                                Event::new(Category::Application, EventKind::Started),
                                None,
                            );
                            self.diagnostic_flush = diagnostics.flush().ok();
                            self.diagnostics = Some(diagnostics);
                            self.record_diagnostic_outcome(
                                Category::Accessibility,
                                self.accessibility_granted,
                                FailureCode::Permission,
                            );
                        }
                        Err(error) => self.notifications.issue(
                            Source::Diagnostics,
                            Severity::Warning,
                            diagnostic_failure_message(error),
                        ),
                    }
                }
                DiagnosticUpdate::Controlled(control, result) => {
                    self.diagnostics_busy = false;
                    self.notifications.clear(Source::Diagnostics);
                    match result {
                        Ok(()) => self.notifications.success(match control {
                            DiagnosticControl::OpenFolder => "Logs folder opened in Finder.",
                            DiagnosticControl::ClearLogs => "Logs cleared. New diagnostics continue locally.",
                        }),
                        Err(error) => self.notifications.issue(Source::Diagnostics, Severity::Warning,
                            if matches!(control, DiagnosticControl::OpenFolder) && matches!(error, DiagnosticError::Io(_)) {
                                "Could not open logs in Finder. Check folder permissions and try again."
                            } else { diagnostic_failure_message(error) }),
                    }
                }
            }
            cx.notify();
        }
        if let Some(result) = self
            .diagnostic_flush
            .as_ref()
            .and_then(Completion::try_wait)
        {
            self.diagnostic_flush = None;
            if let Err(error) = result {
                self.notifications.issue(
                    Source::Diagnostics,
                    Severity::Warning,
                    diagnostic_failure_message(error),
                );
                cx.notify();
            }
        }
        if self.diagnostic_flush.is_none()
            && self.diagnostic_flushed_at.elapsed() >= Duration::from_secs(30)
        {
            self.diagnostic_flushed_at = Instant::now();
            if let Some(diagnostics) = &self.diagnostics {
                match diagnostics.flush() {
                    Ok(completion) => self.diagnostic_flush = Some(completion),
                    Err(error) => {
                        self.notifications.issue(
                            Source::Diagnostics,
                            Severity::Warning,
                            diagnostic_failure_message(error),
                        );
                        cx.notify();
                    }
                }
            }
        }
    }
    fn diagnostic_control(&mut self, control: DiagnosticControl, cx: &mut Context<Self>) {
        if self.diagnostics_busy || self.diagnostics_initializing {
            return;
        }
        // Controlled UI tests release completions explicitly, without storage or Finder.
        #[cfg(test)]
        if let Some(requests) = &mut self.controlled_diagnostic_requests {
            requests.push(control);
            self.diagnostics_busy = true;
            cx.notify();
            return;
        }
        let Some(diagnostics) = self.diagnostics.clone() else {
            self.notifications
                .warning("Local diagnostics unavailable. Check folder permissions and relaunch.");
            cx.notify();
            return;
        };
        self.diagnostics_busy = true;
        self.notifications.working(
            Source::Diagnostics,
            match control {
                DiagnosticControl::OpenFolder => "Opening logs folder…",
                DiagnosticControl::ClearLogs => "Clearing local logs…",
            },
        );
        let tx = self.diagnostics_tx.clone();
        let spawned = std::thread::Builder::new()
            .name("wiesel-diagnostic-control".into())
            .spawn(move || {
                let started = Instant::now();
                let id = diagnostics.new_action_id();
                let _ = diagnostics.record(
                    Event::new(Category::Diagnostics, EventKind::Started)
                        .at_stage(control.stage(), 0),
                    Some(id),
                );
                let result = run_diagnostic_control(&diagnostics, control);
                let event = match result {
                    Ok(()) => Event::new(Category::Diagnostics, EventKind::Succeeded),
                    Err(DiagnosticError::OtherSessionActive) => {
                        Event::failure(Category::Diagnostics, FailureCode::Conflict)
                    }
                    Err(
                        DiagnosticError::UnsafePath
                        | DiagnosticError::Io(std::io::ErrorKind::PermissionDenied),
                    ) => Event::failure(Category::Diagnostics, FailureCode::Permission),
                    Err(_) => Event::failure(Category::Diagnostics, FailureCode::Storage),
                };
                let _ = diagnostics.record(
                    event.at_stage(
                        control.stage(),
                        started.elapsed().as_millis().min(u128::from(u64::MAX)) as u64,
                    ),
                    Some(id),
                );
                let _ = tx.send(DiagnosticUpdate::Controlled(control, result));
            });
        if let Err(error) = spawned {
            self.diagnostics_busy = false;
            self.notifications.issue(
                Source::Diagnostics,
                Severity::Warning,
                diagnostic_failure_message(error.into()),
            );
        }
        cx.notify();
    }
    fn record_diagnostic(&self, event: Event) {
        if let Some(diagnostics) = &self.diagnostics {
            let _ = diagnostics.record(event, Some(diagnostics.new_action_id()));
        }
    }
    fn record_diagnostic_outcome(&self, category: Category, succeeded: bool, code: FailureCode) {
        self.record_diagnostic(if succeeded {
            Event::new(category, EventKind::Succeeded)
        } else {
            Event::failure(category, code)
        });
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
        if self.actions.read_only {
            self.notifications
                .warning("Saving is disabled. Repair the settings file and relaunch.");
            cx.notify();
            return;
        }
        let previous = self.settings.hotkey.clone();
        if let Err(e) = self.register(&text) {
            self.record_diagnostic(Event::failure(Category::Hotkey, FailureCode::Conflict));
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
                    self.record_diagnostic(Event::new(Category::Settings, EventKind::Succeeded));
                    self.settings = next;
                    self.notifications.clear(Source::Settings);
                    self.notifications.success("Shortcut active and saved.");
                }
                Err(e) => {
                    self.record_diagnostic(Event::failure(
                        Category::Settings,
                        FailureCode::Storage,
                    ));
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
            self.record_diagnostic_outcome(
                Category::Accessibility,
                granted,
                FailureCode::Permission,
            );
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
                    self.capture_scope = Some(self.session.scope());
                    self.pending_capture = Some(selection::begin());
                    cx.notify();
                }
            }
        }
        self.poll_capture(window, cx);
        // Callback activation must not steal focus before Copy restoration finishes.
        if self.pending_capture.is_none() {
            self.receive_callbacks(window, cx);
        }
        // Resume following only once the reader has returned to the bottom.
        if !self.chat_follow_bottom
            && self.chat_scroll.max_offset().y + self.chat_scroll.offset().y <= px(1.)
        {
            self.chat_follow_bottom = true;
        }
        self.poll_diagnostics(cx);
        self.receive_results(window, cx);
        if self
            .notifications
            .update(Instant::now(), self.idle_notification())
        {
            cx.notify();
        }
    }
    // Drain physical capture even after its identity is invalidated, so Copy can
    // restore the clipboard. A late ended-session result never repopulates the UI.
    fn poll_capture(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(capture) = self.pending_capture.as_mut()
            && let Some(result) = capture.poll()
        {
            self.pending_capture = None;
            self.finish_capture(result, window, cx);
        }
    }

    fn finish_capture(
        &mut self,
        result: anyhow::Result<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self
            .capture_scope
            .take()
            .is_some_and(|scope| self.session.accepts(scope))
        {
            return;
        }
        self.record_diagnostic_outcome(
            Category::Selection,
            result.is_ok(),
            FailureCode::Unavailable,
        );
        self.notifications.clear(Source::Selection);
        match &result {
            Ok(text) => self.notifications.success(format!(
                "Captured {} characters. Ready for writing.",
                text.chars().count()
            )),
            Err(e) => {
                let (severity, message) = notifications::capture_issue(e);
                self.notifications
                    .issue(Source::Selection, severity, message);
            }
        }
        self.selection.update(result);
        self.activate_from_hotkey(window, cx);
        cx.activate(true);
        window.activate_window();
        cx.notify();
    }
    fn receive_results(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        while let Ok(event) = self.rx.try_recv() {
            let inference = matches!(
                event.result,
                ResultPayload::ChatDelta(_) | ResultPayload::Completion(_, _)
            );
            if inference
                && (event.scope.completion.is_none()
                    || event.scope.completion != self.active_request)
            {
                continue;
            }
            if !self.session.accepts(event.scope) {
                // Discard payloads from ended/replaced sessions. Only the exact
                // pending physical request's terminal event can release its slot.
                if matches!(event.result, ResultPayload::Completion(_, _)) {
                    self.busy = false;
                    self.active_request = None;
                    self.streaming_chat = None;
                    cx.notify();
                }
                continue;
            }
            self.apply_result(event.scope, event.result, window, cx);
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
                if let Ok(models) = &result {
                    self.update_action_catalog(models.clone(), cx);
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
                self.active_request = None;
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
        self.finish_startup_navigation(show_launcher, window, cx);
    }

    fn finish_startup_navigation(
        &mut self,
        show_launcher: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if show_launcher
            && self.authenticated
            && self.hotkey.is_some()
            && self.settings.onboarded
            && self.current_screen() == Screen::Settings
            && self
                .navigation
                .current()
                .on_pop
                .as_ref()
                .is_some_and(|lifecycle| lifecycle.startup)
        {
            self.pop_screen(window, cx);
        }
    }
    fn start_login(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.actions.dialog.is_some() {
            return;
        }
        self.push_settings(false, window, cx);
        // Revealing lower Settings may need dirty-editor confirmation. Never
        // start browser login or focus an unmounted control before that decision.
        if self.current_screen() != Screen::Settings || self.actions.dialog.is_some() {
            return;
        }
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
        self.record_diagnostic(Event::new(Category::Authentication, EventKind::Cancelled));
        self.session.cancel_login();
        self.notifications.clear(Source::Request);
        self.notifications
            .info("Login canceled. Old browser links cannot sign you in.");
        cx.notify();
    }
    fn receive_callbacks(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        while let Ok(raw) = self.urls.try_recv() {
            self.handle_callback(&raw, window, cx);
        }
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
            Err(AuthError::Inactive) => {
                self.record_diagnostic(Event::new(Category::Authentication, EventKind::Rejected));
                self.notifications.warning("No matching active login. Reopen Login / Sign up; old links cannot be restored.");
            }
            Err(_) => self
                .notifications
                .warning("Login link did not match. Continue in your browser or reopen login."),
        }
        if !self.authenticated {
            self.push_settings(false, window, cx);
            if self.current_screen() == Screen::Settings && self.actions.dialog.is_none() {
                window.focus(&self.login_focus, cx);
            }
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
    // Account-privacy boundary, separate from native credential cleanup. Navigation
    // stays intact and physical inference/capture must still settle independently.
    fn end_session_state(&mut self, cx: &mut Context<Self>) -> Option<settings::CleanupTarget> {
        let target = self.session.end_session();
        self.authenticated = false;
        self.status_loading = false;
        self.device_status = None;
        self.busy = self.active_request.is_some();
        self.streaming_chat = None;
        self.messages.clear();
        self.failed_chat.clear();
        self.result.clear();
        self.writing_title.clear();
        self.composer.update(cx, |input, cx| input.set("", cx));
        self.launcher_input
            .update(cx, |input, cx| input.set("", cx));
        self.selection = selection::Capture::default();
        self.capture_scope = None;
        self.update_action_catalog(vec![], cx);
        self.model_picker
            .update(cx, |picker, cx| picker.finish_load(Ok(vec![]), cx));
        self.notifications.clear(Source::Selection);
        self.notifications.clear(Source::Models);
        self.notifications.clear(Source::Request);
        target
    }
    fn clear_session(&mut self, message: &'static str, cx: &mut Context<Self>) {
        let request_pending = self.busy;
        let target = self.end_session_state(cx);
        // Local removal is independent of remote success. A failure is never labeled
        // persistent logout, and blocks a new login until removal is retried.
        let removed = self
            .recovery
            .remove(target, settings::delete_device_credential);
        self.record_diagnostic_outcome(Category::Keychain, removed.is_ok(), FailureCode::Storage);
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
        let action = self
            .diagnostics
            .as_ref()
            .map(|diagnostics| gateway::DiagnosticAction::new(diagnostics, Category::Models));
        std::thread::spawn(move || {
            let result = credential
                .ensure_valid()
                .map_err(|error| {
                    if let Some(action) = &action {
                        action.credential_rejected();
                    }
                    anyhow::Error::from(error)
                })
                .and_then(|()| gateway::models(credential.access_token(), action.as_ref()));
            let _ = tx.send(ResultEvent {
                scope,
                result: ResultPayload::Models(result),
            });
        });
    }
    fn finish_setup(&mut self, window: &mut Window, cx: &mut Context<Self>) {
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
        if self.actions.read_only {
            self.notifications
                .warning("Saving is disabled. Repair the settings file and relaunch.");
            cx.notify();
            return;
        }
        let previous = self.settings.hotkey.clone();
        if let Err(e) = self.register(&hotkey) {
            self.record_diagnostic(Event::failure(Category::Hotkey, FailureCode::Conflict));
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
                self.record_diagnostic(Event::new(Category::Settings, EventKind::Succeeded));
                self.settings = next;
                self.pop_screen(window, cx);
                self.notifications.clear(Source::Settings);
                self.notifications.success("Settings saved. Ready to go.");
            }
            Err(e) => {
                self.record_diagnostic(Event::failure(Category::Settings, FailureCode::Storage));
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
        self.request_with_model(messages, chat, self.settings.model.clone(), cx);
    }
    fn request_with_model(
        &mut self,
        messages: Vec<Message>,
        chat: bool,
        model: String,
        cx: &mut Context<Self>,
    ) {
        if self.busy {
            return;
        }
        if !self.authenticated || !self.settings.onboarded {
            self.notifications
                .warning("Complete login and setup in Settings before sending a request.");
            cx.notify();
            return;
        }
        let Some(credential) = self.valid_credential(cx) else {
            return;
        };
        self.request_generation += 1;
        let completion = CompletionScope {
            entry: self.navigation.current().id,
            generation: self.request_generation,
        };
        self.active_request = Some(completion);
        let scope = EventScope {
            completion: Some(completion),
            ..self.session.scope()
        };
        self.busy = true;
        self.notifications.clear(Source::Keychain);
        self.notifications.working(Source::Request, "Thinking…");
        let tx = self.tx.clone();
        if chat {
            self.streaming_chat = Some(Message::new("assistant", ""));
        }
        self.completion_backend.start(
            completion_backend::CompletionRequest {
                credential,
                model,
                messages,
                chat,
                scope,
                diagnostic_action: self.diagnostics.as_ref().map(|diagnostics| {
                    gateway::DiagnosticAction::new(diagnostics, Category::Request)
                }),
            },
            tx,
        );
        cx.notify();
    }
    fn run_quick_action(
        &mut self,
        action: QuickAction,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if action == QuickAction::Add {
            self.open_action_editor(None, window, cx);
            return;
        }
        if !self.authenticated || !self.settings.onboarded {
            self.push_settings(false, window, cx);
            self.notifications
                .warning("Complete login and setup in Settings first.");
            cx.notify();
            return;
        }
        match action {
            QuickAction::Chat => {
                self.push_screen(Screen::Chat, false, window, cx);
                window.focus(&self.composer.focus_handle(cx), cx);
                cx.notify();
            }
            QuickAction::Add => self.open_action_editor(None, window, cx),
            _ => self.writing(action, window, cx),
        }
    }
    fn writing(&mut self, action: QuickAction, window: &mut Window, cx: &mut Context<Self>) {
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
        self.push_screen(Screen::Writing, false, window, cx);
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
        if !self.authenticated || !self.settings.onboarded {
            self.run_quick_action(QuickAction::Chat, window, cx);
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
        if self.action_builder_key(event, window, cx) {
            return;
        }
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
        if self.current_screen() == Screen::Settings
            && key.key == "tab"
            && !key.modifiers.platform
            && !key.modifiers.control
            && !key.modifiers.alt
        {
            if key.modifiers.shift {
                window.focus_prev(cx);
            } else {
                window.focus_next(cx);
            }
            cx.stop_propagation();
            return;
        }
        if self.current_screen() == Screen::Settings
            && key.key == "enter"
            && !key.modifiers.shift
            && self.login_focus.is_focused(window)
        {
            self.start_login(window, cx);
            cx.stop_propagation();
        } else if key.key == "escape" {
            if !self.pop_screen(window, cx) {
                self.hide_window(cx);
            }
            cx.stop_propagation();
        } else if self.current_screen() == Screen::Launcher {
            if key.key == "enter"
                && !key.modifiers.shift
                && !key.modifiers.platform
                && !key.modifiers.control
                && !key.modifiers.alt
                && !event.is_held
            {
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
        } else if self.current_screen() == Screen::Chat
            && key.key == "enter"
            && !key.modifiers.shift
            && !key.modifiers.platform
            && !key.modifiers.control
            && !key.modifiers.alt
            && !event.is_held
        {
            self.send_chat(cx);
            cx.stop_propagation();
        }
    }
    fn button(id: &'static str, title: &str) -> Stateful<Div> {
        div()
            .id(id)
            .debug_selector(move || id.into())
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
            .debug_selector(move || id.into())
            // Preserve the covered screen's focus before the click pushes a route.
            .on_mouse_down(MouseButton::Left, |_, window, _| window.prevent_default())
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
    fn diagnostics_controls(&self, cx: &mut Context<Self>) -> Div {
        let unavailable = self.diagnostics.is_none() || self.diagnostics_busy;
        div().flex().flex_col().gap_2()
            .child(Self::label("Diagnostics"))
            .child(Self::label("Local JSONL logs · ~/Library/Logs/Wiesel. Includes model ID and raw server error type; review logs before sharing."))
            .child(Self::label("Inactive logs pruned after 7 days, up to 20 MiB; active sessions protected. Cleanup runs on logging/flush, not an idle timer."))
            .child(Self::label(if self.diagnostics_initializing { "Preparing local diagnostics…" }
                else if self.diagnostics.is_none() { "Diagnostics unavailable; other features still work. Check folder permissions and relaunch." }
                else if self.diagnostics_busy { "Working…" }
                else { "Clearing logs does not clear chat or settings. Close other Wiesel instances first." }))
            .child(div().flex().gap_2()
                .child(Self::button("open-logs-folder", "Open Logs Folder")
                    .debug_selector(|| "open-logs-folder".into())
                    .role(Role::Button).aria_label("Open Logs Folder in Finder")
                    .accessibility_id("wiesel.diagnostics.open-folder")
                    .aria_description(if unavailable { "Unavailable or busy; check Diagnostics status." } else { "Ready" })
                    .when(unavailable, |d| d.opacity(0.5))
                    .focus_visible(|s| s.border_color(rgb(theme::RING)))
                    .active(|s| s.bg(rgb(theme::ACCENT)))
                    .track_focus(&self.diagnostics_open_focus)
                    .on_click(cx.listener(|this, _, _, cx| this.diagnostic_control(DiagnosticControl::OpenFolder, cx))))
                .child(Self::button("clear-logs", "Clear Logs")
                    .debug_selector(|| "clear-logs".into())
                    .role(Role::Button).aria_label("Clear local diagnostic logs")
                    .accessibility_id("wiesel.diagnostics.clear")
                    .aria_description(if unavailable { "Unavailable or busy; check Diagnostics status." } else { "Ready" })
                    .when(unavailable, |d| d.opacity(0.5))
                    .focus_visible(|s| s.border_color(rgb(theme::RING)))
                    .active(|s| s.bg(rgb(theme::ACCENT)))
                    .track_focus(&self.diagnostics_clear_focus)
                    .on_click(cx.listener(|this, _, _, cx| this.diagnostic_control(DiagnosticControl::ClearLogs, cx)))))
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
            .child(Self::button("apply-hotkey", "Apply shortcut").when(self.actions.read_only, |d| d.opacity(0.5).aria_description("Saving disabled until the settings file is repaired and Wiesel relaunched")).on_click(cx.listener(|this, _, _, cx| this.apply_hotkey(cx))))
            .child(Self::label("Command (⌘) is called Super in the field above. Recorded shortcuts activate immediately; typed changes need Apply shortcut."))
            .child(self.login_controls(cx))
            .child(Self::label("3 · Model"))
            .child(div().w_full().flex_shrink_0().debug_selector(|| "settings-model-picker".into()).child(self.model_picker.clone()))
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
            .child(self.diagnostics_controls(cx))
            .child(Self::button("save", "Save & start").when(self.actions.read_only, |d| d.opacity(0.5).aria_description("Saving disabled until the settings file is repaired and Wiesel relaunched")).on_click(cx.listener(|this, _, window, cx| this.finish_setup(window, cx)))))
    }
    fn launcher(&self, cx: &mut Context<Self>) -> Stateful<Div> {
        div()
            .id("quick-actions-scroll")
            .debug_selector(|| "quick-actions-scroll".into())
            .track_scroll(&self.actions.home_scroll)
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .flex()
            .flex_col()
            .gap(px(12.))
            .p(px(20.))
            .children(self.home_actions().chunks(3).map(|actions| {
                div()
                    .flex()
                    .flex_shrink_0()
                    .gap(px(12.))
                    .w_full()
                    .children(actions.iter().cloned().map(|item| {
                        let (id, title, icon, shortcut, add) = item.presentation();
                        let debug_id = id.clone();
                        let keyboard_item = item.clone();
                        div()
                            .id(gpui::SharedString::from(id.clone()))
                            .debug_selector(move || debug_id.clone())
                            .on_mouse_down(MouseButton::Left, |_, window, _| {
                                window.prevent_default()
                            })
                            .role(Role::Button)
                            .track_focus(&self.action_control_focus(&id, cx))
                            .focus_visible(|s| s.border_color(rgb(theme::RING)))
                            .on_key_down(cx.listener(
                                move |this, event: &KeyDownEvent, window, cx| {
                                    if matches!(event.keystroke.key.as_str(), "enter" | "space")
                                        && !event.keystroke.modifiers.platform
                                        && !event.keystroke.modifiers.control
                                        && !event.keystroke.modifiers.alt
                                    {
                                        if !event.is_held {
                                            this.run_home_action(keyboard_item.clone(), window, cx);
                                        }
                                        cx.stop_propagation();
                                    }
                                },
                            ))
                            .accessibility_id(format!("wiesel.action.{id}"))
                            .aria_label(title.clone())
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
                            .when(add, |d| d.text_color(rgb(theme::MUTED)))
                            .hover(|s| s.bg(rgb(theme::ACCENT)).border_color(rgb(theme::RING)))
                            .child(theme::icon(icon, 24.).text_color(rgb(if add {
                                theme::MUTED
                            } else {
                                theme::FOREGROUND
                            })))
                            .child(
                                div()
                                    .text_size(px(13.))
                                    .font_weight(FontWeight::MEDIUM)
                                    .w_full()
                                    .px_2()
                                    .text_center()
                                    .text_ellipsis()
                                    .whitespace_nowrap()
                                    .overflow_hidden()
                                    .child(title),
                            )
                            .when(add || !shortcut.is_empty(), |d| {
                                d.child(
                                    div()
                                        .absolute()
                                        .top(px(10.))
                                        .right(px(10.))
                                        .child(theme::badge(if add { "+" } else { shortcut })),
                                )
                            })
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.run_home_action(item.clone(), window, cx);
                            }))
                    }))
                    .children(std::iter::repeat_with(|| div().flex_1()).take(3 - actions.len()))
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
                                // Only committed messages use user/assistant identifiers. A
                                // streaming preview must never satisfy a completed-reply check.
                                let message_kind = if index >= self.messages.len() {
                                    "streaming"
                                } else if user {
                                    "user"
                                } else {
                                    "assistant"
                                };
                                div()
                                    .id(("chat-message", index))
                                    .role(Role::Group)
                                    .accessibility_id(format!(
                                        "wiesel.message.{message_kind}.{index}"
                                    ))
                                    .aria_label(if user { "Your message" } else { "Wiesel reply" })
                                    .aria_value(text.clone())
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
                            .child(
                                div()
                                    .debug_selector(|| "chat-composer".into())
                                    .flex_1()
                                    .min_w_0()
                                    .child(self.composer.clone()),
                            )
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
                                    .debug_selector(|| "send".into())
                                    .role(Role::Button)
                                    .accessibility_id("wiesel.chat.send")
                                    .aria_label(if self.busy {
                                        "Sending message"
                                    } else {
                                        "Send message"
                                    })
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
        let content = match self.current_screen() {
            Screen::Settings => self.setup(cx).into_any_element(),
            Screen::Launcher => self.launcher(cx).into_any_element(),
            Screen::Chat => self.chat(cx).into_any_element(),
            Screen::Writing => self.writing_view(cx).into_any_element(),
            Screen::ActionManager => self.action_manager(cx).into_any_element(),
            Screen::ActionEditor => self.action_editor(cx).into_any_element(),
        };
        div()
            .id("wiesel-root")
            .role(Role::Group)
            .accessibility_id(match self.current_screen() {
                Screen::Settings => "wiesel.page.setup",
                Screen::Launcher => "wiesel.page.launcher",
                Screen::Chat => "wiesel.page.chat",
                Screen::Writing => "wiesel.page.writing",
                Screen::ActionManager => "wiesel.page.actions",
                Screen::ActionEditor => "wiesel.page.action-editor",
            })
            .aria_label("Wiesel")
            .aria_description(match (self.authenticated, self.busy) {
                (true, false) => "Signed in; request idle",
                (true, true) => "Signed in; request pending",
                (false, false) => "Signed out; request idle",
                (false, true) => "Signed out; request pending",
            })
            .relative()
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
                    .when(self.current_screen() == Screen::Launcher, |d| {
                        d.child(div().flex_1().min_w_0().child(self.launcher_input.clone()))
                    })
                    .when(self.current_screen() != Screen::Launcher, |d| {
                        d.when(self.navigation.depth() > 1, |d| {
                            d.child(
                                div()
                                    .id("back")
                                    .debug_selector(|| "back".into())
                                    .role(Role::Button)
                                    .aria_label("Back")
                                    .accessibility_id("wiesel.navigation.back")
                                    .when(
                                        matches!(
                                            self.current_screen(),
                                            Screen::ActionEditor | Screen::ActionManager
                                        ),
                                        |d| d.track_focus(&self.action_control_focus("back", cx)),
                                    )
                                    .on_key_down(cx.listener(
                                        |this, event: &KeyDownEvent, window, cx| {
                                            if matches!(
                                                event.keystroke.key.as_str(),
                                                "enter" | "space"
                                            ) && !event.keystroke.modifiers.platform
                                                && !event.keystroke.modifiers.control
                                                && !event.keystroke.modifiers.alt
                                            {
                                                if !event.is_held {
                                                    this.pop_screen(window, cx);
                                                }
                                                cx.stop_propagation();
                                            }
                                        },
                                    ))
                                    .on_mouse_down(MouseButton::Left, |_, window, _| {
                                        window.prevent_default()
                                    })
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
                                    .on_click(cx.listener(|this, event, window, cx| {
                                        this.back_clicked(event, window, cx);
                                    })),
                            )
                            .child(div().w(px(1.)).h(px(18.)).bg(rgba(theme::BORDER)))
                        })
                        .when(
                            !matches!(
                                self.current_screen(),
                                Screen::ActionManager | Screen::ActionEditor
                            ),
                            |d| {
                                d.child(theme::icon(
                                    match self.current_screen() {
                                        Screen::Writing => QUICK_ACTIONS
                                            .iter()
                                            .find(|action| action.title() == self.writing_title)
                                            .map_or("spell-check", |action| action.icon()),
                                        _ => "chat",
                                    },
                                    18.,
                                ))
                            },
                        )
                        .child(
                            div()
                                .flex_1()
                                .font_weight(FontWeight::MEDIUM)
                                .text_size(px(14.))
                                .on_mouse_down(MouseButton::Left, |_, window, _| {
                                    window.start_window_move();
                                })
                                .child(match self.current_screen() {
                                    Screen::Writing => self.writing_title.clone(),
                                    Screen::Settings => "Wiesel · Settings".to_owned(),
                                    Screen::ActionManager => "Manage actions".to_owned(),
                                    Screen::ActionEditor => if self
                                        .actions
                                        .editor
                                        .as_ref()
                                        .is_some_and(|editor| editor.existing)
                                    {
                                        "Edit action"
                                    } else {
                                        "New action"
                                    }
                                    .to_owned(),
                                    _ => "Chat".to_owned(),
                                }),
                        )
                    })
                    .when(self.current_screen() == Screen::Chat, |d| {
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
                    .when(self.current_screen() == Screen::Launcher, |d| {
                        d.child(self.builder_button(
                            "manage-actions",
                            "Manage actions",
                            action_builder::Command::Manage,
                            cx,
                        ))
                        .when(
                            !self.settings.custom_actions.is_empty(),
                            |d| {
                                d.child(self.builder_button(
                                    "home-new-action",
                                    "Add action",
                                    action_builder::Command::New,
                                    cx,
                                ))
                            },
                        )
                    })
                    .when(self.current_screen() == Screen::ActionManager, |d| {
                        d.child(self.builder_button(
                            "new-action",
                            "New action",
                            action_builder::Command::New,
                            cx,
                        ))
                    })
                    .child(
                        Self::icon_button("settings", "settings", "Settings").on_click(
                            cx.listener(|this, _, window, cx| {
                                this.push_settings(false, window, cx);
                            }),
                        ),
                    )
                    .child(
                        Self::icon_button("hide", "minus", "Hide")
                            .on_click(cx.listener(|this, _, _, cx| this.hide_window(cx))),
                    ),
            )
            .child(content)
            .child(
                div()
                    .h(px(48.))
                    .flex_shrink_0()
                    .debug_selector(|| "notifications-footer".into())
                    .child(self.notifications.render()),
            )
            .when(self.actions.dialog.is_some(), |d| {
                d.child(self.action_dialog(cx))
            })
    }
}
#[cfg(test)]
mod diagnostic_integration_tests {
    use super::*;
    use diagnostics::tests::{Temp, records};

    #[test]
    fn finder_command_passes_the_entire_folder_as_one_argument_without_a_shell() {
        let path = std::path::Path::new("/tmp/user home/Logs/$(private); Wiesel");
        let command = finder_command(path);
        assert_eq!(command.get_program(), "/usr/bin/open");
        assert_eq!(
            command.get_args().collect::<Vec<_>>(),
            vec![
                std::ffi::OsStr::new("-a"),
                std::ffi::OsStr::new("Finder"),
                std::ffi::OsStr::new("--"),
                path.as_os_str(),
            ]
        );
    }

    #[test]
    fn clear_control_waits_for_storage_completion_then_future_events_persist() {
        let temp = Temp::new();
        let diagnostics = temp.logger();
        diagnostics.record(Event::new(Category::Application, EventKind::Started), None);
        run_diagnostic_control(&diagnostics, DiagnosticControl::ClearLogs).unwrap();
        assert!(records(&temp.logs).is_empty());
        diagnostics.record(
            Event::new(Category::Diagnostics, EventKind::Succeeded).at_stage(Stage::ClearLogs, 1),
            None,
        );
        diagnostics.shutdown().unwrap().wait().unwrap();
        assert_eq!(records(&temp.logs)[0]["stage"], "clear_logs");
    }

    #[test]
    fn clear_control_surfaces_active_session_refusal_and_can_be_retried() {
        let temp = Temp::new();
        let diagnostics = temp.logger();
        let other = temp.logger();
        assert_eq!(
            run_diagnostic_control(&diagnostics, DiagnosticControl::ClearLogs),
            Err(DiagnosticError::OtherSessionActive)
        );
        assert_eq!(
            diagnostic_failure_message(DiagnosticError::OtherSessionActive),
            "Close other Wiesel instances, then clear logs again."
        );
        other.shutdown().unwrap().wait().unwrap();
        run_diagnostic_control(&diagnostics, DiagnosticControl::ClearLogs).unwrap();
        diagnostics.shutdown().unwrap().wait().unwrap();
    }
}

fn main() {
    let (diagnostics_tx, diagnostics_rx) = mpsc::channel();
    let init_tx = diagnostics_tx.clone();
    if let Err(error) = std::thread::Builder::new()
        .name("wiesel-diagnostics-init".into())
        .spawn(move || {
            let _ = init_tx.send(DiagnosticUpdate::Initialized(Diagnostics::init()));
        })
    {
        let _ = diagnostics_tx.send(DiagnosticUpdate::Initialized(Err(error.into())));
    }
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
            |window, cx| {
                cx.new(|cx| {
                    let mut app = Wiesel::new(urls, diagnostics_tx, diagnostics_rx, window, cx);
                    app.register_diagnostic_shutdown(cx);
                    app
                })
            },
        ) {
            Ok(handle) => handle,
            Err(_) => {
                // No notification surface exists yet; never print arbitrary error chains.
                eprintln!("Cannot open Wiesel window. Quit and relaunch Wiesel.");
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
