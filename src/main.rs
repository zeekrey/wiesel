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
mod ollama;
mod quick_actions;
mod selection;
mod settings;
mod settings_view;
mod theme;
mod tier_picker;

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
use quick_actions::QuickAction;
use settings::{ActionModel, ModelChoice, Provider, Settings, Tier};
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

/// The user's side of a conversation: a dark slate bubble in both appearances.
fn user_bubble(bubble: Div) -> Div {
    let palette = theme::p();
    bubble
        .bg(theme::vertical(palette.bubble_hi, palette.bubble, 1.))
        .shadow(vec![
            theme::shadow(0., 0., 0., 1., (palette.bubble_edge << 8) | 0xff).inset(),
        ])
        .text_color(rgb(palette.bubble_text))
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
            .bg(rgb(theme::p().card))
            .border_1()
            .border_color(rgba(theme::p().border))
            .text_color(rgb(theme::p().foreground))
            .text_size(px(11.))
            .child(self.0)
    }
}

actions!(
    wiesel,
    [
        Login,
        OpenSettings,
        AdjustQuickActions,
        NewQuickAction,
        NewChat,
        HideWiesel
    ]
);

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
    Ollama(ollama::Status),
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
    session: DesktopSession,
    login_focus: FocusHandle,
    urls: mpsc::Receiver<zeroize::Zeroizing<String>>,
    status_loading: bool,
    device_status: Option<auth::DeviceStatus>,
    recovery: CredentialRecovery,
    model_picker: Entity<model_picker::ModelPicker>,
    /// One picker per `Tier::ALL` entry.
    tier_pickers: Vec<Entity<tier_picker::TierPicker>>,
    /// Provider shown in the Settings switch.
    provider_tab: Provider,
    ollama_status: Option<ollama::Status>,
    ollama_checking: bool,
    composer: Entity<TextInput>,
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
    // User-created actions start chats with a fixed model and expanded first turn.
    chat_action_model: Option<ModelChoice>,
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
            Screen::Launcher => self.focus.clone(),
            Screen::Chat => self.composer.focus_handle(cx),
            Screen::Settings if self.provider_tab == Provider::Wiesel => self.login_focus.clone(),
            Screen::Settings => self.focus.clone(),
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

    fn redirect_deleted_home_restore_focus(&mut self, deleted: &FocusHandle) {
        for lifecycle in self.navigation.lifecycles_mut() {
            if lifecycle.restore_focus.as_ref() == Some(deleted) {
                lifecycle.restore_focus = Some(self.focus.clone());
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

    /// Quick-action management is a singleton like Settings: reveal it instead of stacking.
    fn open_action_manager(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.actions.dialog.is_some() {
            return;
        }
        let existing = self
            .navigation
            .entries()
            .find(|entry| entry.component == Screen::ActionManager)
            .map(|entry| entry.id);
        match existing {
            Some(id) if id != self.navigation.current().id => {
                self.pop_to_screen(id, window, cx);
            }
            Some(_) => (),
            None => {
                self.push_screen(Screen::ActionManager, false, window, cx);
            }
        }
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
        window.focus(&self.focus, cx);
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
        let model_picker = cx.new(|cx| model_picker::ModelPicker::new(settings.model.clone(), cx));
        let tier_pickers = Self::new_tier_pickers(&settings, cx);
        let provider_tab = Self::initial_provider_tab(&settings);
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
            model_picker,
            tier_pickers,
            provider_tab,
            ollama_status: None,
            ollama_checking: false,
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
            chat_action_model: None,
            tx,
            rx,
            chat_scroll: ScrollHandle::new(),
            chat_follow_bottom: true,
            streaming_chat: None,
            completion_backend: completion_backend::CompletionBackend::Gateway,
        };
        cx.subscribe(&app.composer, |this, _, notification: &Notification, cx| {
            this.input_notification(notification, cx);
        })
        .detach();
        cx.subscribe(
            &app.model_picker,
            |this, _, notification: &Notification, cx| {
                this.input_notification(notification, cx);
            },
        )
        .detach();
        app.observe_tier_pickers(cx);
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
        // Re-render with the matching palette when macOS switches Light/Dark.
        cx.observe_window_appearance(window, |_, _, cx| cx.notify())
            .detach();
        window.on_window_should_close(cx, |_, cx| {
            cx.hide();
            false
        });
        app.push_settings(true, window, cx);
        app.poll_diagnostics(cx);
        app.sync_permission_issue();
        app.refresh_status(true, cx);
        app.refresh_ollama(cx);
        // A local model needs no sign-in, so startup can go straight to the launcher.
        if app.settings.default_model().local() {
            app.finish_startup_navigation(true, window, cx);
        }
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
    /// Recorded shortcuts register and save immediately; there is no separate Apply step.
    fn apply_hotkey(&mut self, text: String, cx: &mut Context<Self>) {
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
        if !self.can_request() {
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
                    self.recording = false;
                    self.apply_hotkey(self.settings.hotkey.clone(), cx);
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
            if let ResultPayload::Ollama(status) = event.result {
                self.finish_ollama(status, cx);
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
                    Err(_) => self.notifications.issue(
                        Source::Request,
                        Severity::Error,
                        "Could not verify sign-in. Check your connection and retry.",
                    ),
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
                let loaded = result.as_ref().ok().cloned();
                self.update_catalogs(Provider::Wiesel, cx, move |catalog| {
                    catalog.loading = false;
                    if let Some(models) = &loaded {
                        catalog.models = models.clone();
                        catalog.note = None;
                    }
                });
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
                        let local = ollama::failure_message(&e);
                        self.notifications.issue(
                            Source::Request,
                            Severity::Error,
                            local.unwrap_or_else(|| gateway::failure_message(&e)),
                        );
                        // A local failure may mean Ollama stopped or lost the model.
                        if local.is_some() {
                            self.refresh_ollama(cx);
                        }
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
            ResultPayload::Ollama(status) => self.finish_ollama(status, cx),
        }
    }
    fn login_ready(&mut self, show_launcher: bool, window: &mut Window, cx: &mut Context<Self>) {
        self.complete_onboarding();
        self.refresh_models(cx);
        self.finish_startup_navigation(show_launcher, window, cx);
    }

    /// Signing in is the only setup step: the shortcut works before login and
    /// every other preference has a working default.
    fn complete_onboarding(&mut self) {
        if self.settings.onboarded {
            return;
        }
        // Unlock actions for this session even if the flag cannot be persisted.
        self.settings.onboarded = true;
        if self.actions.read_only {
            return;
        }
        #[cfg(test)]
        if let Some(persist) = &mut self.actions.persist {
            let _ = persist(&self.settings);
            return;
        }
        if let Err(e) = settings::save(&self.settings) {
            self.record_diagnostic(Event::failure(Category::Settings, FailureCode::Storage));
            self.notifications
                .report(Source::Settings, "Could not save settings. Try again.", &e);
        }
    }

    fn finish_startup_navigation(
        &mut self,
        show_launcher: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if std::env::var_os("WIESEL_SHOT").is_some() && self.current_screen() == Screen::Settings {
            if std::env::var_os("WIESEL_SHOT_CHAT").is_some() {
                self.pop_screen(window, cx);
                self.push_screen(Screen::Chat, false, window, cx);
                self.messages.push(gateway::Message {
                    role: "user".into(),
                    content: "Can you make this paragraph sound friendlier for a customer email?"
                        .into(),
                });
                self.messages.push(gateway::Message { role: "assistant".into(), content: "Of course! Here's a warmer version: “Thanks so much for reaching out — we've looked into your order and it's on its way. If anything else comes up, we're happy to help.”".into() });
            } else {
                self.pop_screen(window, cx);
            }
            return;
        }
        if show_launcher
            && self.can_request()
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
        self.show_account_tab(cx);
        self.push_settings(false, window, cx);
        // Revealing lower Settings may need dirty-editor confirmation. Never
        // start browser login or focus an unmounted control before that decision.
        if self.current_screen() != Screen::Settings || self.actions.dialog.is_some() {
            return;
        }
        window.focus(&self.login_focus, cx);
        if self.session.credential.is_some() {
            self.notifications
                .info("Already signed in. Sign out to switch accounts.");
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
                    "Could not start secure login. Try Sign in again.",
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
                self.notifications.warning(
                    "No matching active login. Reopen sign-in; old links cannot be restored.",
                );
            }
            Err(_) => self
                .notifications
                .warning("Login link did not match. Continue in your browser or reopen login."),
        }
        if !self.authenticated {
            self.show_account_tab(cx);
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
        self.chat_action_model = None;
        self.failed_chat.clear();
        self.result.clear();
        self.writing_title.clear();
        self.composer.update(cx, |input, cx| input.set("", cx));
        self.selection = selection::Capture::default();
        self.capture_scope = None;
        self.model_picker
            .update(cx, |picker, cx| picker.finish_load(Ok(vec![]), cx));
        self.update_catalogs(Provider::Wiesel, cx, |catalog| {
            *catalog = Self::signed_out_catalog();
        });
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
        self.update_catalogs(Provider::Wiesel, cx, |catalog| catalog.loading = true);
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
    /// A local default model replaces sign-in; otherwise a verified login is required.
    fn can_request(&self) -> bool {
        self.settings.default_model().local() || self.signed_in()
    }
    fn signed_in(&self) -> bool {
        self.authenticated && self.settings.onboarded
    }
    fn initial_provider_tab(settings: &Settings) -> Provider {
        if settings.default_model().local() {
            Provider::Ollama
        } else {
            Provider::Wiesel
        }
    }
    /// Reveal the account controls before focusing or starting sign-in.
    fn show_account_tab(&mut self, cx: &mut Context<Self>) {
        if self.provider_tab != Provider::Wiesel {
            self.provider_tab = Provider::Wiesel;
            cx.notify();
        }
    }
    fn signed_out_catalog() -> tier_picker::Catalog {
        tier_picker::Catalog {
            note: Some("Sign in to use Wiesel models."),
            ..Default::default()
        }
    }
    fn new_tier_pickers(
        settings: &Settings,
        cx: &mut Context<Self>,
    ) -> Vec<Entity<tier_picker::TierPicker>> {
        Tier::ALL
            .into_iter()
            .map(|tier| {
                let selected = settings.models.get(tier).cloned().map(ActionModel::Model);
                cx.new(|cx| {
                    let mut picker = tier_picker::TierPicker::new(
                        format!("tier-{}", tier.name().to_lowercase()),
                        selected,
                        cx,
                    );
                    picker.wiesel = Self::signed_out_catalog();
                    picker
                })
            })
            .collect()
    }
    fn observe_tier_pickers(&mut self, cx: &mut Context<Self>) {
        for (tier, picker) in Tier::ALL.into_iter().zip(self.tier_pickers.clone()) {
            cx.subscribe(&picker, |this, _, notification: &Notification, cx| {
                this.input_notification(notification, cx);
            })
            .detach();
            cx.subscribe(&picker, move |this, _, picked: &tier_picker::Picked, cx| {
                let choice = picked.0.as_ref().and_then(ActionModel::choice).cloned();
                this.assign_tier(tier, choice, cx);
            })
            .detach();
        }
    }
    fn tier_picker(&self, tier: Tier) -> &Entity<tier_picker::TierPicker> {
        let index = Tier::ALL
            .iter()
            .position(|t| *t == tier)
            .unwrap_or_default();
        &self.tier_pickers[index]
    }
    fn update_catalogs(
        &self,
        provider: Provider,
        cx: &mut Context<Self>,
        update: impl Fn(&mut tier_picker::Catalog),
    ) {
        let editor = self.actions.editor.as_ref().map(|editor| &editor.model);
        for picker in self.tier_pickers.iter().chain(editor) {
            picker.update(cx, |picker, cx| {
                update(picker.catalog_mut(provider));
                cx.notify();
            });
        }
    }
    fn refresh_ollama(&mut self, cx: &mut Context<Self>) {
        if self.ollama_checking {
            return;
        }
        self.ollama_checking = true;
        self.update_catalogs(Provider::Ollama, cx, |catalog| catalog.loading = true);
        // In-process fixtures never touch the network; they release status explicitly.
        #[cfg(not(test))]
        {
            let tx = self.tx.clone();
            let scope = self.session.scope();
            std::thread::spawn(move || {
                let _ = tx.send(ResultEvent {
                    scope,
                    result: ResultPayload::Ollama(ollama::status()),
                });
            });
        }
        cx.notify();
    }
    fn finish_ollama(&mut self, status: ollama::Status, cx: &mut Context<Self>) {
        self.ollama_checking = false;
        let names: Vec<String> = status
            .models()
            .iter()
            .map(|model| model.name.clone())
            .collect();
        let note = match &status {
            ollama::Status::NotInstalled => Some("Ollama isn't installed."),
            ollama::Status::NotRunning => Some("Ollama isn't running."),
            ollama::Status::Running { .. } => None,
        };
        self.update_catalogs(Provider::Ollama, cx, |catalog| {
            catalog.loading = false;
            catalog.models = names.clone();
            catalog.note = note;
        });
        if self.settings.models.uses_ollama() && !matches!(status, ollama::Status::Running { .. }) {
            self.notifications
                .warning("Ollama is not running. Start it, or pick other models in Settings.");
        }
        self.ollama_status = Some(status);
        cx.notify();
    }
    fn assign_tier(&mut self, tier: Tier, choice: Option<ModelChoice>, cx: &mut Context<Self>) {
        let mut next = self.settings.clone();
        next.models.set(tier, choice);
        let saved = if self.actions.read_only {
            Err(anyhow::anyhow!("Settings are read-only"))
        } else {
            self.persist_settings(&next)
        };
        match saved {
            Ok(()) => {
                self.record_diagnostic(Event::new(Category::Settings, EventKind::Succeeded));
                self.notifications.clear(Source::Settings);
                self.notifications.success(match next.models.get(tier) {
                    Some(choice) if choice.local() => {
                        format!("{} uses {} on this Mac.", tier.name(), choice.id)
                    }
                    Some(choice) => format!("{} uses {} via Wiesel.", tier.name(), choice.id),
                    None => format!("{} has no model.", tier.name()),
                });
                self.settings = next;
                if self.settings.default_model().local() {
                    self.complete_onboarding();
                }
            }
            Err(e) => {
                let previous = self
                    .settings
                    .models
                    .get(tier)
                    .cloned()
                    .map(ActionModel::Model);
                self.tier_picker(tier)
                    .update(cx, |picker, _| picker.selected = previous);
                if self.actions.read_only {
                    self.notifications
                        .warning("Saving is disabled. Repair the settings file and relaunch.");
                } else {
                    self.record_diagnostic(Event::failure(
                        Category::Settings,
                        FailureCode::Storage,
                    ));
                    self.notifications.report(
                        Source::Settings,
                        "Could not save the model. Try again.",
                        &e,
                    );
                }
            }
        }
        cx.notify();
    }
    fn persist_settings(&mut self, next: &Settings) -> anyhow::Result<()> {
        #[cfg(test)]
        if let Some(persist) = &mut self.actions.persist {
            return persist(next).map_err(Into::into);
        }
        settings::save(next)
    }
    fn request_with_model(
        &mut self,
        messages: Vec<Message>,
        chat: bool,
        model: ModelChoice,
        cx: &mut Context<Self>,
    ) {
        if self.busy {
            return;
        }
        if !model.local() && !self.signed_in() {
            self.notifications
                .warning("Sign in to Wiesel before sending a request.");
            cx.notify();
            return;
        }
        // Each model goes to its own provider; Ollama needs no credential.
        let route = match model.provider {
            Provider::Ollama => completion_backend::Route::Ollama {
                thinking: self
                    .ollama_status
                    .as_ref()
                    .is_some_and(|status| status.thinking(&model.id)),
            },
            Provider::Wiesel => {
                let Some(credential) = self.valid_credential(cx) else {
                    return;
                };
                completion_backend::Route::Gateway(credential)
            }
        };
        let model = model.id;
        let local = matches!(route, completion_backend::Route::Ollama { .. });
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
                route,
                model,
                messages,
                chat,
                scope,
                diagnostic_action: self.diagnostics.as_ref().filter(|_| !local).map(
                    |diagnostics| gateway::DiagnosticAction::new(diagnostics, Category::Request),
                ),
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
        if !self.can_request() {
            self.show_account_tab(cx);
            self.push_settings(false, window, cx);
            self.notifications.warning("Sign in to Wiesel first.");
            cx.notify();
            return;
        }
        if !self
            .settings
            .actions()
            .iter()
            .any(|configured| configured.id == action.persistent_id())
        {
            return;
        }
        match action {
            QuickAction::Chat => self.open_chat(window, cx),
            QuickAction::Add => self.open_action_editor(None, window, cx),
            _ => self.writing(action, window, cx),
        }
    }
    fn open_chat(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.push_screen(Screen::Chat, false, window, cx);
        window.focus(&self.composer.focus_handle(cx), cx);
        cx.notify();
    }
    fn writing(&mut self, action: QuickAction, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(configured) = self
            .settings
            .actions()
            .into_iter()
            .find(|configured| configured.id == action.persistent_id())
        {
            self.run_custom_action(configured, window, cx);
        }
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
        let configured = if self.chat_action_model.is_none() {
            self.settings
                .actions()
                .into_iter()
                .find(|action| action.id == QuickAction::Chat.persistent_id())
        } else {
            None
        };
        let instructions = configured.as_ref().map_or(
            "You are a helpful, clear and concise assistant.",
            |action| action.instructions.as_str(),
        );
        let Some(mut messages) = self.action_messages(instructions, true, cx) else {
            return;
        };
        let model = self.chat_action_model.clone().unwrap_or_else(|| {
            configured.as_ref().map_or_else(
                || self.settings.default_model(),
                |action| action.effective_model(&self.settings),
            )
        });
        if !model.local()
            && (self.chat_action_model.is_some()
                || configured
                    .as_ref()
                    .is_some_and(quick_actions::CustomQuickAction::pins_model))
        {
            let picker = self.model_picker.read(cx);
            if picker.loading || !picker.models.contains(&model.id) {
                self.notifications.warning("This action's model is unavailable. Edit the action to pick another; no request was sent.");
                cx.notify();
                return;
            }
        }
        // request validates the device session before the draft is cleared.
        self.messages.push(Message::new("user", text));
        messages.extend(self.messages.clone());
        self.request_with_model(messages, true, model, cx);
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
            self.recording = false;
            self.apply_hotkey(parts.join("+"), cx);
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
            && self.session.credential.is_none()
            && !self.recovery.blocks_login()
        {
            self.start_login(window, cx);
            cx.stop_propagation();
        } else if key.key == "escape" {
            if !self.pop_screen(window, cx) {
                self.hide_window(cx);
            }
            cx.stop_propagation();
        } else if self.current_screen() == Screen::Launcher
            && !key.modifiers.platform
            && !key.modifiers.control
            && !key.modifiers.alt
            && !event.is_held
            && let Some(slot) = home_slot_shortcut(&key.key)
        {
            // Number keys press the pad printed with the same number.
            if let Some(Some(action)) = self.home_action_slots().get(slot).cloned() {
                self.run_home_action(action, window, cx);
            }
            cx.stop_propagation();
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
        Self::key_button(id, title, theme::Key::Grey)
    }
    /// A labelled hardware key; the caption is printed on the cap in bold caps.
    fn key_button(id: &'static str, title: &str, kind: theme::Key) -> Stateful<Div> {
        theme::key(
            div()
                .id(id)
                .debug_selector(move || id.into())
                .aria_label(title.to_owned()),
            kind,
        )
        .h(px(28.))
        .px(px(12.))
        .text_size(px(10.))
        .child(title.to_uppercase())
    }

    fn label(text: &str) -> Div {
        div()
            .text_size(px(12.))
            .text_color(rgb(theme::p().muted))
            .child(text.to_owned())
    }
    fn launcher(&self, window: &Window, cx: &mut Context<Self>) -> Div {
        // Fit three pad rows (plus their printed labels) below the header, as in the design.
        let viewport = window.viewport_size();
        let gap = px(HOME_COLUMN_GAP);
        let rows_chrome = HEADER_HEIGHT
            + HOME_PADDING_TOP
            + HOME_PADDING_BOTTOM
            + 3. * (HOME_LABEL_GAP + HOME_LABEL_HEIGHT)
            + 2. * HOME_ROW_GAP;
        let pad = ((viewport.height - px(rows_chrome)) / 3.)
            .min((viewport.width - px(2. * HOME_PADDING_X) - gap * 2.) / 3.)
            .max(px(64.));
        let grid_width = pad * 3. + gap * 2.;
        div()
            .flex_1()
            .min_h_0()
            .flex()
            .flex_col()
            .items_center()
            .child(
                div()
                    .id("quick-actions-scroll")
                    .debug_selector(|| "quick-actions-scroll".into())
                    .track_scroll(&self.actions.home_scroll)
                    .flex_1()
                    .min_h_0()
                    .w_full()
                    .overflow_y_scroll()
                    .flex()
                    .flex_col()
                    .items_center()
                    .gap(px(HOME_ROW_GAP))
                    .pt(px(HOME_PADDING_TOP))
                    .pb(px(HOME_PADDING_BOTTOM))
                    .children(self.home_action_slots().chunks(3).enumerate().map(
                        |(row, actions)| {
                            div()
                                .w(grid_width)
                                .flex()
                                .flex_shrink_0()
                                .gap(gap)
                                .children(actions.iter().cloned().enumerate().map(
                                    |(column, item)| self.home_pad(row * 3 + column, item, pad, cx),
                                ))
                        },
                    )),
            )
    }
    fn home_pad(
        &self,
        slot: usize,
        item: Option<action_builder::HomeAction>,
        size: gpui::Pixels,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let palette = theme::p();
        // Every pad is printed with its position; 1–9 double as keyboard shortcuts.
        let number = if slot < 9 {
            (slot + 1).to_string()
        } else {
            String::new()
        };
        let label = |title: Option<String>| {
            div()
                .h(px(HOME_LABEL_HEIGHT))
                .w(size)
                .flex()
                .items_center()
                .gap(px(8.))
                .px(px(1.))
                .font_family(theme::PRINT)
                .text_size(px(9.5))
                .line_height(px(HOME_LABEL_HEIGHT))
                .font_weight(FontWeight::BOLD)
                .whitespace_nowrap()
                .overflow_hidden()
                .child(
                    div()
                        .flex_shrink_0()
                        .font_weight(FontWeight::NORMAL)
                        .text_color(rgb(palette.pad_number))
                        .child(number.clone()),
                )
                .child(match title {
                    Some(title) => div()
                        .min_w_0()
                        .text_ellipsis()
                        .overflow_hidden()
                        .text_color(rgb(palette.pad_title))
                        .child(theme::spaced(&title)),
                    None => div()
                        .text_color(rgb(palette.pad_number))
                        .font_weight(FontWeight::NORMAL)
                        .child("—"),
                })
        };
        let column = div()
            .flex()
            .flex_col()
            .flex_shrink_0()
            .gap(px(HOME_LABEL_GAP));
        let Some(item) = item else {
            return column
                .child(
                    theme::pad(
                        div()
                            .id(("home-empty", slot))
                            .debug_selector(move || format!("home-empty-{slot}")),
                        false,
                    )
                    .size(size),
                )
                .child(label(None))
                .into_any_element();
        };
        let (id, title, icon, _, add) = item.presentation();
        let debug_id = id.clone();
        let keyboard_item = item.clone();
        let pad = div()
            .id(gpui::SharedString::from(id.clone()))
            .debug_selector(move || debug_id.clone())
            .on_mouse_down(MouseButton::Left, |_, window, _| window.prevent_default())
            .role(Role::Button)
            .track_focus(&self.action_control_focus(&id, cx))
            .on_key_down(cx.listener(move |this, event: &KeyDownEvent, window, cx| {
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
            }))
            .accessibility_id(format!("wiesel.action.{id}"))
            .aria_label(title.clone())
            .on_click(cx.listener(move |this, _, window, cx| {
                this.run_home_action(item.clone(), window, cx);
            }));
        let add_hover = palette.empty_pad_hi;
        column
            .child(
                theme::pad(pad, !add)
                    .size(size)
                    .flex()
                    .items_center()
                    .justify_center()
                    // The unlit "+" slot still invites a press.
                    .when(add, |d| {
                        d.cursor_pointer()
                            .hover(move |s| s.bg(rgb(add_hover)).border_color(rgb(theme::PAD_GLOW)))
                    })
                    .child(if add {
                        theme::icon(icon, 30.).text_color(rgb(palette.add_icon))
                    } else {
                        theme::icon(icon, 26.).text_color(rgb(theme::PAD_INK))
                    }),
            )
            .child(label(Some(title)))
            .into_any_element()
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
                                .child(theme::icon("chat", 24.).text_color(rgb(theme::p().muted)))
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
                                                    .rounded(px(6.))
                                                    .map(user_bubble)
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
                                                        .text_color(rgb(theme::p().muted))
                                                        .cursor_pointer()
                                                        .hover(|s| {
                                                            s.text_color(rgb(theme::p().foreground))
                                                        })
                                                        .child(
                                                            theme::icon("copy", 13.)
                                                                .text_color(rgb(theme::p().muted)),
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
                    .flex()
                    .items_center()
                    .gap(px(12.))
                    .px(px(16.))
                    .py(px(12.))
                    .border_t_1()
                    .border_color(rgba(theme::p().border))
                    .child(
                        div()
                            .debug_selector(|| "chat-composer".into())
                            .flex_1()
                            .min_w_0()
                            .child(self.composer.clone()),
                    )
                    .child(
                        theme::key(
                            div()
                                .id("send")
                                .debug_selector(|| "send".into())
                                .role(Role::Button)
                                .accessibility_id("wiesel.chat.send")
                                .aria_label(if self.busy {
                                    "Sending message"
                                } else {
                                    "Send message"
                                }),
                            theme::Key::Blue,
                        )
                        .w(px(76.))
                        .h(px(38.))
                        .text_size(px(10.))
                        .when(self.busy, |d| d.opacity(0.6))
                        .child(if self.busy { "…" } else { "SEND" })
                        .on_click(cx.listener(|this, _, _, cx| this.send_chat(cx))),
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
                                .rounded(px(6.))
                                .map(user_bubble)
                                .flex()
                                .flex_col()
                                .gap(px(6.))
                                .child(
                                    div()
                                        .text_size(px(10.))
                                        .font_weight(FontWeight::MEDIUM)
                                        .text_color(rgb(theme::p().bubble_meta))
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
                                theme::silkscreen("Wiesel", theme::LOGO_RED)
                                    .font_weight(FontWeight::BOLD),
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
                                            theme::icon("copy", 13.)
                                                .text_color(rgb(theme::p().muted)),
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
                    .border_color(rgba(theme::p().border))
                    .px_5()
                    .py_3()
                    .child(Self::label(
                        "Your original text is never replaced automatically.",
                    )),
            )
    }
}
pub(crate) const HEADER_HEIGHT: f32 = 56.;
const TOOLBAR_HEIGHT: f32 = 44.;
const HOME_PADDING_X: f32 = 26.;
const HOME_PADDING_TOP: f32 = 27.;
const HOME_PADDING_BOTTOM: f32 = 22.;
const HOME_COLUMN_GAP: f32 = 16.;
const HOME_ROW_GAP: f32 = 7.;
const HOME_LABEL_GAP: f32 = 5.;
const HOME_LABEL_HEIGHT: f32 = 12.;

/// Maps the printed pad numbers 1–9 to Home slots.
fn home_slot_shortcut(key: &str) -> Option<usize> {
    match key.as_bytes() {
        [digit @ b'1'..=b'9'] => Some(usize::from(digit - b'1')),
        _ => None,
    }
}

impl Wiesel {
    fn screen_title(&self) -> String {
        match self.current_screen() {
            Screen::Launcher => "Home".into(),
            Screen::Chat => "Chat".into(),
            Screen::Writing => self.writing_title.clone(),
            Screen::Settings => "Settings".into(),
            Screen::ActionManager => "Quick actions".into(),
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
            .into(),
        }
    }
    fn new_chat(&mut self, cx: &mut Context<Self>) {
        if !self.busy {
            self.messages.clear();
            self.chat_action_model = None;
            self.failed_chat.clear();
            self.chat_follow_bottom = true;
            self.chat_scroll.set_offset(point(px(0.), px(0.)));
            self.notifications.clear(Source::Request);
            self.notifications.success("New chat ready.");
            cx.notify();
        }
    }
    /// Wordmark and status LED; the whole strip doubles as the window's drag handle.
    fn header(&self) -> Div {
        let palette = theme::p();
        div()
            .debug_selector(|| "header".into())
            .h(px(HEADER_HEIGHT))
            .flex_shrink_0()
            .flex()
            .items_center()
            .justify_between()
            .gap(px(16.))
            .pl(px(18.))
            .pr(px(20.))
            .bg(theme::vertical(palette.header, palette.header_hi, 1.))
            .border_b_1()
            .border_color(rgb(palette.header_edge))
            .on_mouse_down(MouseButton::Left, |_, window, _| window.start_window_move())
            .child(
                div()
                    .id("wordmark")
                    .role(Role::Heading)
                    .aria_label("Wiesel")
                    .child(theme::wordmark(25.)),
            )
            .child(
                div()
                    .debug_selector(|| "status".into())
                    .id("status")
                    .role(Role::Status)
                    .accessibility_id("wiesel.status")
                    .min_w_0()
                    .max_w(px(280.))
                    .child(self.notifications.render(self.busy)),
            )
    }
    /// Child screens get a Back key, their title and one screen-specific command.
    fn toolbar(&self, cx: &mut Context<Self>) -> Div {
        let palette = theme::p();
        let back = theme::key(
            div()
                .id("back")
                .debug_selector(|| "back".into())
                .role(Role::Button)
                .aria_label("Back")
                .accessibility_id("wiesel.navigation.back")
                // Preserve the covered screen's focus before the click pops a route.
                .on_mouse_down(MouseButton::Left, |_, window, _| window.prevent_default())
                .when(
                    matches!(
                        self.current_screen(),
                        Screen::ActionEditor | Screen::ActionManager
                    ),
                    |d| d.track_focus(&self.action_control_focus("back", cx)),
                )
                .on_click(
                    cx.listener(|this, event, window, cx| this.back_clicked(event, window, cx)),
                )
                .on_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| {
                    if matches!(event.keystroke.key.as_str(), "enter" | "space")
                        && !event.keystroke.modifiers.platform
                        && !event.keystroke.modifiers.control
                        && !event.keystroke.modifiers.alt
                    {
                        if !event.is_held {
                            this.pop_screen(window, cx);
                        }
                        cx.stop_propagation();
                    }
                })),
            theme::Key::Grey,
        )
        .tooltip(|_, cx| cx.new(|_| HeaderTooltip("Back (Esc)")).into())
        .h(px(24.))
        .px(px(8.))
        .child(theme::icon("arrow-left", 12.).text_color(rgb(theme::Key::Grey.label())))
        .child("BACK");
        let command = match self.current_screen() {
            Screen::Chat => Some(
                Self::button("new-chat", "New chat")
                    .h(px(24.))
                    .px(px(10.))
                    .on_mouse_down(MouseButton::Left, |_, window, _| window.prevent_default())
                    .on_click(cx.listener(|this, _, _, cx| this.new_chat(cx))),
            ),
            Screen::ActionManager => Some(
                theme::key(
                    self.builder_button_base(
                        "new-action",
                        "New action",
                        action_builder::Command::New,
                        cx,
                    ),
                    theme::Key::Grey,
                )
                .h(px(24.))
                .px(px(10.))
                .child(theme::icon("plus", 11.).text_color(rgb(theme::Key::Grey.label())))
                .child("NEW ACTION"),
            ),
            _ => None,
        };
        div()
            .debug_selector(|| "toolbar".into())
            .h(px(TOOLBAR_HEIGHT))
            .flex_shrink_0()
            .flex()
            .items_center()
            .gap(px(12.))
            .child(back)
            .child(
                div().flex_1().min_w_0().overflow_hidden().child(
                    theme::silkscreen(self.screen_title(), palette.pad_title).text_ellipsis(),
                ),
            )
            .children(command)
    }
    fn dispatch_menu(&mut self, command: MenuCommand, window: &mut Window, cx: &mut Context<Self>) {
        self.notifications.suppress_motion = true;
        // Menu commands have no outside mouse-down, so dismiss editor popups explicitly
        // before another screen covers them.
        if self.current_screen() == Screen::ActionEditor && self.actions.dialog.is_none() {
            while self.close_editor_popup(window, cx) {}
        }
        match command {
            MenuCommand::Settings => self.push_settings(false, window, cx),
            MenuCommand::QuickActions => self.open_action_manager(window, cx),
            MenuCommand::NewAction => {
                self.builder_command(action_builder::Command::New, window, cx)
            }
            MenuCommand::NewChat => {
                if self.current_screen() != Screen::Chat {
                    self.run_quick_action(QuickAction::Chat, window, cx);
                }
                if self.current_screen() == Screen::Chat {
                    self.new_chat(cx);
                }
            }
        }
        self.notifications
            .update(Instant::now(), self.idle_notification());
        self.notifications.suppress_motion = false;
        cx.notify();
    }
}
#[derive(Clone, Copy)]
enum MenuCommand {
    Settings,
    QuickActions,
    NewAction,
    NewChat,
}
impl Render for Wiesel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        theme::set_appearance(window.appearance());
        let palette = theme::p();
        self.notifications
            .update(Instant::now(), self.idle_notification());
        let launcher = self.current_screen() == Screen::Launcher;
        let content = match self.current_screen() {
            Screen::Settings => self.setup(cx).into_any_element(),
            Screen::Launcher => self.launcher(window, cx).into_any_element(),
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
            .overflow_hidden()
            .bg(theme::vertical(palette.chassis_hi, palette.chassis, 1.))
            .text_color(rgb(palette.foreground))
            .font_family(theme::SANS)
            .text_size(px(13.))
            .flex()
            .flex_col()
            .track_focus(&self.focus)
            .on_key_down(cx.listener(Self::key_down))
            .on_action(cx.listener(|this, _: &OpenSettings, window, cx| {
                this.dispatch_menu(MenuCommand::Settings, window, cx)
            }))
            .on_action(cx.listener(|this, _: &AdjustQuickActions, window, cx| {
                this.dispatch_menu(MenuCommand::QuickActions, window, cx)
            }))
            .on_action(cx.listener(|this, _: &NewQuickAction, window, cx| {
                this.dispatch_menu(MenuCommand::NewAction, window, cx)
            }))
            .on_action(cx.listener(|this, _: &NewChat, window, cx| {
                this.dispatch_menu(MenuCommand::NewChat, window, cx)
            }))
            .child(self.header())
            .child(
                // Pads sit straight on the chassis; everything else on a sheet of paper.
                div()
                    .debug_selector(|| "deck".into())
                    .flex_1()
                    .min_h_0()
                    .flex()
                    .flex_col()
                    .map(|d| {
                        if launcher {
                            return d.child(content);
                        }
                        d.child(self.toolbar(cx)).px(px(16.)).pb(px(16.)).child(
                            theme::paper()
                                .flex_1()
                                .min_h_0()
                                .overflow_hidden()
                                .flex()
                                .flex_col()
                                .child(content),
                        )
                    }),
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

const WINDOW_WIDTH: f32 = 486.;
const WINDOW_HEIGHT: f32 = 573.;

/// The macOS menu bar: Settings and quick-action management live here, not on screen.
fn app_menus() -> Vec<gpui::Menu> {
    use gpui::{Menu, MenuItem};
    vec![
        Menu::new("Wiesel").items([
            MenuItem::action("Settings…", OpenSettings),
            MenuItem::action("Adjust Quick Actions…", AdjustQuickActions),
            MenuItem::separator(),
            MenuItem::action("Log In / Sign Up…", Login),
            MenuItem::separator(),
            MenuItem::action("Hide Wiesel", HideWiesel),
            MenuItem::action("Quit Wiesel", input::Quit),
        ]),
        Menu::new("File").items([
            MenuItem::action("New Chat", NewChat),
            MenuItem::action("New Quick Action…", NewQuickAction),
        ]),
        Menu::new("Edit").items([
            MenuItem::action("Cut", input::Cut),
            MenuItem::action("Copy", input::Copy),
            MenuItem::action("Paste", input::Paste),
            MenuItem::separator(),
            MenuItem::action("Select All", input::SelectAll),
        ]),
    ]
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
        cx.bind_keys([
            KeyBinding::new("cmd-,", OpenSettings, None),
            KeyBinding::new("cmd-n", NewChat, None),
            KeyBinding::new("cmd-h", HideWiesel, None),
        ]);
        cx.on_action(|_: &HideWiesel, cx| cx.hide());
        cx.set_menus(app_menus());
        let window_size = size(px(WINDOW_WIDTH), px(WINDOW_HEIGHT));
        let bounds = Bounds::centered(None, window_size, cx);
        let handle = match cx.open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                titlebar: None,
                app_owns_titlebar_drag: true,
                is_resizable: false,
                is_minimizable: false,
                window_min_size: Some(window_size),
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
        // Menu commands normally reach the window's root listeners; these fallbacks cover a
        // moment with no focused element, and bring a hidden window back first.
        fn menu_fallback<A: gpui::Action>(
            handle: gpui::WindowHandle<Wiesel>,
            command: MenuCommand,
            cx: &mut App,
        ) {
            cx.on_action(move |_: &A, cx| {
                cx.activate(true);
                let _ = handle.update(cx, |app, window, cx| {
                    window.activate_window();
                    app.dispatch_menu(command, window, cx);
                });
            });
        }
        menu_fallback::<OpenSettings>(handle, MenuCommand::Settings, cx);
        menu_fallback::<AdjustQuickActions>(handle, MenuCommand::QuickActions, cx);
        menu_fallback::<NewQuickAction>(handle, MenuCommand::NewAction, cx);
        menu_fallback::<NewChat>(handle, MenuCommand::NewChat, cx);
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
