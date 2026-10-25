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
mod lucide;
mod model_picker;
mod native;
mod navigation;
mod notifications;
mod ollama;
mod quick_actions;
mod selection;
mod settings;
mod settings_view;
#[cfg(test)]
mod surface_tests;
#[cfg(test)]
mod test_support;
mod theme;
mod tier_picker;

use auth::{AuthError, DesktopClient, DeviceCredential};
use diagnostics::{
    Category, Completion, DiagnosticError, Diagnostics, Event, EventKind, FailureCode, Stage,
};
use gateway::Message;
use global_hotkey::{GlobalHotKeyEvent, GlobalHotKeyManager, HotKeyState, hotkey::HotKey};
use gpui::{
    Animation, AnimationExt, AnyElement, AnyWindowHandle, App, Bounds, ClipboardItem, Context, Div,
    Entity, FocusHandle, Focusable, FontWeight, KeyBinding, KeyDownEvent, MouseButton, Pixels,
    Role, ScrollDelta, ScrollHandle, ScrollWheelEvent, Size, Stateful, TitlebarOptions,
    Transformation, Window, WindowBackgroundAppearance, WindowBounds, WindowOptions, actions, div,
    percentage, point, prelude::*, px, relative, rgb, rgba, size,
};
use input::TextInput;
use navigation::{EntryId, Navigation};
use notifications::{Notification, Notifications, Severity, Source};
use quick_actions::QuickAction;
use settings::{ActionModel, Appearance, ModelChoice, Provider, Settings, Tier};
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

/// Screens of the Settings window's navigation stack; Settings is its permanent root.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Screen {
    Settings,
    ActionManager,
    ActionEditor,
}

/// Windows besides the launcher HUD. Each is a singleton rendered from `Wiesel`'s state,
/// so closing one keeps its drafts and history for the next time it opens.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Surface {
    Settings,
    Chat,
    Writing,
}
impl Surface {
    const ALL: [Surface; 3] = [Surface::Settings, Surface::Chat, Surface::Writing];
    fn index(self) -> usize {
        self as usize
    }
    fn title(self) -> &'static str {
        match self {
            Surface::Settings => "Settings",
            Surface::Chat => "Chat",
            Surface::Writing => "Result",
        }
    }
    fn size(self) -> Size<Pixels> {
        match self {
            Surface::Settings => size(px(600.), px(680.)),
            Surface::Chat => size(px(560.), px(640.)),
            Surface::Writing => size(px(520.), px(560.)),
        }
    }
    fn options(self, cx: &App) -> WindowOptions {
        let window_size = self.size();
        WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(Bounds::centered(
                None,
                window_size,
                cx,
            ))),
            titlebar: Some(TitlebarOptions {
                title: Some(self.title().into()),
                appears_transparent: true,
                traffic_light_position: Some(point(px(14.), px(15.))),
            }),
            window_min_size: Some(size(px(440.), px(420.))),
            ..Default::default()
        }
    }
}

/// Window handles and root focus for every `Surface`.
pub(crate) struct Surfaces {
    windows: [Option<AnyWindowHandle>; 3],
    opening: [bool; 3],
    /// Focus to apply once a surface window exists or is activated.
    pending_focus: [Option<FocusHandle>; 3],
    focus: [FocusHandle; 3],
}
impl Surfaces {
    fn new(cx: &mut App) -> Self {
        Self {
            windows: [None; 3],
            opening: [false; 3],
            pending_focus: Default::default(),
            focus: [cx.focus_handle(), cx.focus_handle(), cx.focus_handle()],
        }
    }
}

/// A secondary window's root view: renders one surface from the shared app state.
struct SurfaceView {
    app: Entity<Wiesel>,
    surface: Surface,
}
impl SurfaceView {
    fn new(
        app: Entity<Wiesel>,
        surface: Surface,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        cx.observe(&app, |_, _, cx| cx.notify()).detach();
        cx.observe_window_appearance(window, |_, _, cx| cx.notify())
            .detach();
        let weak = app.downgrade();
        window.on_window_should_close(cx, move |window, cx| {
            weak.update(cx, |app, cx| app.surface_should_close(surface, window, cx))
                .unwrap_or(true)
        });
        Self { app, surface }
    }
}
impl Render for SurfaceView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let surface = self.surface;
        self.app
            .update(cx, |app, cx| app.render_surface(surface, window, cx))
    }
}
struct ScreenLifecycle {
    restore_focus: Option<FocusHandle>,
}

// Completion ownership survives an entry's pop, but cannot be reused by a newer request.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct CompletionScope {
    entry: EntryId,
    generation: u64,
}

/// A request whose result replaces the selection in `source` instead of opening a view.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct DirectPaste {
    action: uuid::Uuid,
    source: i32,
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
        HideWiesel,
        CloseWindow
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
    /// The Settings window's stack: Settings → Quick actions → action editor.
    navigation: Navigation<Screen, ScreenLifecycle>,
    surfaces: Surfaces,
    /// The launcher HUD window (this view's own window).
    hud: Option<AnyWindowHandle>,
    hud_visible: bool,
    /// Highlighted launcher tile; arrows, Tab and hover move it, Enter runs it.
    hud_selected: usize,
    /// The launcher was summoned over another app, so Esc hands focus back to it.
    hud_hides_app: bool,
    /// Settings was opened for setup at launch and may close itself after sign-in.
    settings_startup: bool,
    request_generation: u64,
    active_request: Option<CompletionScope>,
    #[cfg(test)]
    recorded_hides: Option<usize>,
    #[cfg(test)]
    recorded_pastes: Option<Vec<(i32, String)>>,
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
    direct_paste: Option<DirectPaste>,
    paste_back: Option<clipboard_capture::PasteBack>,
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
    /// The visible screen of the Settings window's stack.
    fn current_screen(&self) -> Screen {
        self.navigation.current().component
    }

    fn screen_focus(&self, cx: &App) -> FocusHandle {
        match self.current_screen() {
            Screen::Settings if self.provider_tab == Provider::Wiesel => self.login_focus.clone(),
            Screen::Settings | Screen::ActionManager => self.surface_focus(Surface::Settings),
            Screen::ActionEditor => self.actions.editor.as_ref().map_or_else(
                || self.surface_focus(Surface::Settings),
                |editor| editor.name.focus_handle(cx),
            ),
        }
    }

    fn surface_focus(&self, surface: Surface) -> FocusHandle {
        self.surfaces.focus[surface.index()].clone()
    }
    fn surface_window(&self, surface: Surface) -> Option<AnyWindowHandle> {
        self.surfaces.windows[surface.index()]
    }
    /// Open, or opening: its state is mounted even before the native window appears.
    fn is_open(&self, surface: Surface) -> bool {
        self.surfaces.windows[surface.index()].is_some() || self.surfaces.opening[surface.index()]
    }
    fn window_is(&self, surface: Surface, window: &Window) -> bool {
        self.surface_window(surface) == Some(window.window_handle())
    }

    /// Bring a surface window forward (opening it if needed) and focus `focus` inside it.
    /// Windows open after the current update, because opening one renders it immediately.
    fn open_surface(
        &mut self,
        surface: Surface,
        focus: Option<FocusHandle>,
        cx: &mut Context<Self>,
    ) {
        let index = surface.index();
        self.surfaces.pending_focus[index] =
            Some(focus.unwrap_or_else(|| self.surface_focus(surface)));
        self.dismiss_hud(cx);
        cx.activate(true);
        cx.notify();
        if self.surfaces.opening[index] {
            return;
        }
        let app = cx.entity();
        if let Some(handle) = self.surfaces.windows[index] {
            cx.defer(move |cx| {
                let _ = handle.update(cx, |_, window, cx| {
                    window.activate_window();
                    app.update(cx, |app, cx| app.apply_pending_focus(surface, window, cx));
                });
            });
            return;
        }
        self.surfaces.opening[index] = true;
        cx.defer(move |cx| {
            // Closed again before the window existed.
            if !app.read(cx).surfaces.opening[index] {
                return;
            }
            let opened = cx.open_window(surface.options(cx), |window, cx| {
                let view = cx.new(|cx| SurfaceView::new(app.clone(), surface, window, cx));
                app.update(cx, |app, cx| app.apply_pending_focus(surface, window, cx));
                view
            });
            let opened = opened.ok().map(AnyWindowHandle::from);
            app.update(cx, |app, cx| {
                app.surfaces.opening[index] = false;
                app.surfaces.windows[index] = opened;
                cx.notify();
            });
            if let Some(handle) = opened {
                let _ = handle.update(cx, |_, window, _| window.activate_window());
            }
        });
    }
    fn apply_pending_focus(
        &mut self,
        surface: Surface,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(focus) = self.surfaces.pending_focus[surface.index()].take() {
            window.focus(&focus, cx);
        }
    }
    /// Focus inside a surface; from another window this activates the surface first.
    fn focus_in(
        &mut self,
        surface: Surface,
        focus: FocusHandle,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.window_is(surface, window) {
            window.focus(&focus, cx);
        } else {
            self.open_surface(surface, Some(focus), cx);
        }
    }
    /// Run a Settings-window operation inside that window, so focus capture/restore and
    /// confirmation dialogs act on the right window. Returns true when it was deferred.
    fn route_to_settings(
        &mut self,
        window: &Window,
        cx: &mut Context<Self>,
        operation: impl FnOnce(&mut Self, &mut Window, &mut Context<Self>) + 'static,
    ) -> bool {
        let Some(handle) = self.surface_window(Surface::Settings) else {
            return false;
        };
        if handle == window.window_handle() {
            return false;
        }
        let app = cx.entity();
        cx.defer(move |cx| {
            let _ = handle.update(cx, |_, window, cx| {
                window.activate_window();
                app.update(cx, |app, cx| operation(app, window, cx));
            });
        });
        true
    }
    /// Forget a surface's window and leave its screens; the caller closes the native window.
    fn forget_surface(
        &mut self,
        surface: Surface,
        cx: &mut Context<Self>,
    ) -> Option<AnyWindowHandle> {
        let index = surface.index();
        self.surfaces.pending_focus[index] = None;
        self.surfaces.opening[index] = false;
        if surface == Surface::Settings {
            for entry in self.navigation.pop_to_root() {
                self.finish_pop(entry, cx);
            }
            self.leave_settings(cx);
            self.settings_startup = false;
        }
        cx.notify();
        self.surfaces.windows[index].take()
    }
    fn close_surface(&mut self, surface: Surface, cx: &mut Context<Self>) {
        if let Some(handle) = self.forget_surface(surface, cx) {
            cx.defer(move |cx| {
                let _ = handle.update(cx, |_, window, _| window.remove_window());
            });
        }
    }
    /// The close button, ⌘W or Esc at a root: Settings first confirms a dirty action draft.
    fn surface_should_close(
        &mut self,
        surface: Surface,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        if surface == Surface::Settings
            && self.guard_action_removal(action_builder::Removal::Close, window, cx)
        {
            return false;
        }
        self.forget_surface(surface, cx);
        true
    }
    fn request_close(&mut self, surface: Surface, window: &mut Window, cx: &mut Context<Self>) {
        if self.surface_should_close(surface, window, cx) {
            window.remove_window();
        }
    }

    /// Show the launcher centered on screen with the first tile highlighted.
    fn show_hud(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let summoned = match cx.active_window() {
            // Another app was frontmost (also after Wiesel hid itself).
            None => {
                self.hud_hides_app = true;
                true
            }
            Some(active) if Some(active) == self.hud => !self.hud_visible,
            Some(_) => {
                self.hud_hides_app = false;
                true
            }
        };
        // Actions may have changed while the launcher was away.
        self.sync_hud_size(window);
        if summoned {
            self.hud_selected = 0;
            native::center(window);
        }
        self.hud_visible = true;
        cx.activate(true);
        window.activate_window();
        window.focus(&self.focus, cx);
        cx.notify();
    }
    /// Take the launcher off screen without hiding Wiesel's other windows.
    fn dismiss_hud(&mut self, cx: &mut Context<Self>) {
        if !self.hud_visible {
            return;
        }
        self.hud_visible = false;
        // GPUI orders windows front from a foreground task; queue behind it.
        if let Some(hud) = self.hud {
            cx.spawn(async move |this, cx| {
                let still_hidden = this.read_with(cx, |this, _| !this.hud_visible);
                if still_hidden.unwrap_or(false) {
                    let _ = hud.update(cx, |_, window, _| native::order_out(window));
                }
            })
            .detach();
        }
        cx.notify();
    }

    /// Push onto the Settings window's stack, opening the window and capturing the covered
    /// screen's focus.
    fn push_screen(
        &mut self,
        screen: Screen,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> EntryId {
        debug_assert_eq!(self.navigation.root().component, Screen::Settings);
        if self.current_screen() == screen && self.is_open(Surface::Settings) {
            return self.navigation.current().id;
        }
        let restore_focus = self
            .window_is(Surface::Settings, window)
            .then(|| window.focused(cx))
            .flatten()
            .or_else(|| Some(self.screen_focus(cx)));
        let id = if self.current_screen() == screen {
            self.navigation.current().id
        } else {
            self.navigation
                .push(screen, ScreenLifecycle { restore_focus })
        };
        let focus = self.screen_focus(cx);
        self.focus_in(Surface::Settings, focus, window, cx);
        cx.notify();
        id
    }

    /// Open Settings at its root. An open window reveals Settings instead of stacking it.
    fn push_settings(&mut self, startup: bool, window: &mut Window, cx: &mut Context<Self>) {
        if self.actions.dialog.is_some() {
            return;
        }
        if !self.is_open(Surface::Settings) {
            self.settings_startup = startup;
        }
        let root = self.navigation.root().id;
        if root != self.navigation.current().id && !self.pop_to_screen(root, window, cx) {
            return;
        }
        let focus = self.screen_focus(cx);
        self.focus_in(Surface::Settings, focus, window, cx);
        cx.notify();
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
            Some(_) => {
                let focus = self.screen_focus(cx);
                self.focus_in(Surface::Settings, focus, window, cx);
            }
            None => {
                self.push_screen(Screen::ActionManager, window, cx);
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
        let focus = restored.unwrap_or_else(|| self.screen_focus(cx));
        self.focus_in(Surface::Settings, focus, window, cx);
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
            self.leave_settings(cx);
        }
        // Inference is not canceled: retained state and busy ownership outlive the entry.
        entry.on_pop.and_then(|lifecycle| lifecycle.restore_focus)
    }
    /// Leaving Settings stops recording and cancels a pending browser login.
    fn leave_settings(&mut self, cx: &mut Context<Self>) {
        self.recording = false;
        if self.session.attempt.is_some() || self.session.exchanging {
            self.cancel_login(cx);
        }
    }

    /// Pops one Settings-window screen; returns false at its Settings root.
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
        self.focus_in(Surface::Settings, focus, window, cx);
        cx.notify();
        true
    }

    // A completed selection capture shows the launcher; other windows keep their state.
    fn activate_from_hotkey(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.show_hud(window, cx);
    }

    /// Esc on the launcher: put it away. Summoned over another app (or with no other
    /// window open), hand focus back to that app; otherwise return to Wiesel's windows.
    fn escape_hud(&mut self, cx: &mut Context<Self>) {
        if !self.hud_hides_app
            && Surface::ALL
                .into_iter()
                .any(|surface| self.is_open(surface))
        {
            self.dismiss_hud(cx);
        } else {
            self.hide_window(cx);
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
            navigation: Navigation::new(Screen::Settings),
            surfaces: Surfaces::new(cx),
            hud: Some(window.window_handle()),
            hud_visible: true,
            hud_selected: 0,
            hud_hides_app: true,
            settings_startup: false,
            request_generation: 0,
            active_request: None,
            #[cfg(test)]
            recorded_hides: None,
            #[cfg(test)]
            recorded_pastes: None,
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
            direct_paste: None,
            paste_back: None,
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
        app.sync_hud_size(window);
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
        self.sync_hud_size(window);
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
                // Pasting back owns the clipboard until it has been restored.
                if self.pending_capture.is_none() && self.paste_back.is_none() {
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
        self.poll_paste_back(cx);
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
            let source = capture.source();
            self.pending_capture = None;
            self.finish_capture(result, source, window, cx);
        }
    }

    fn finish_capture(
        &mut self,
        result: anyhow::Result<String>,
        source: Option<i32>,
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
            Ok(_) => self.notifications.success("Text captured"),
            Err(e) => {
                let (severity, message) = notifications::capture_issue(e);
                self.notifications
                    .issue(Source::Selection, severity, message);
            }
        }
        let captured = result.is_ok();
        self.selection.update(result);
        if captured {
            self.selection.source = source;
        }
        self.activate_from_hotkey(window, cx);
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
                    self.direct_paste = None;
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
                let result = match (self.direct_paste.take().filter(|_| !chat), result) {
                    (Some(paste), Ok(text)) => {
                        self.start_paste_back(paste, text, cx);
                        return;
                    }
                    (_, result) => result,
                };
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
        if std::env::var_os("WIESEL_SHOT").is_some() && self.settings_startup {
            self.close_surface(Surface::Settings, cx);
            if std::env::var_os("WIESEL_SHOT_CHAT").is_some() {
                self.messages.push(gateway::Message {
                    role: "user".into(),
                    content: "Can you make this paragraph sound friendlier for a customer email?"
                        .into(),
                });
                self.messages.push(gateway::Message { role: "assistant".into(), content: "Of course! Here's a warmer version: “Thanks so much for reaching out — we've looked into your order and it's on its way. If anything else comes up, we're happy to help.”".into() });
                self.open_chat(window, cx);
            } else {
                self.show_hud_from(window, cx);
            }
            return;
        }
        // Only setup Settings that is still on its root closes itself; an explicitly
        // opened Settings window never does.
        if show_launcher
            && self.can_request()
            && self.hotkey.is_some()
            && self.settings.onboarded
            && self.settings_startup
            && self.is_open(Surface::Settings)
            && self.navigation.is_root()
        {
            self.close_surface(Surface::Settings, cx);
            self.show_hud_from(window, cx);
        }
    }
    /// Show the launcher from any window's context.
    fn show_hud_from(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.hud == Some(window.window_handle()) {
            self.show_hud(window, cx);
        } else if let Some(hud) = self.hud {
            let app = cx.entity();
            cx.defer(move |cx| {
                let _ = hud.update(cx, |_, window, cx| {
                    app.update(cx, |app, cx| app.show_hud(window, cx));
                });
            });
        }
    }
    fn start_login(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.actions.dialog.is_some()
            || self.route_to_settings(window, cx, |this, window, cx| this.start_login(window, cx))
        {
            return;
        }
        self.show_account_tab(cx);
        self.push_settings(false, window, cx);
        // Revealing lower Settings may need dirty-editor confirmation. Never
        // start browser login or focus an unmounted control before that decision.
        if self.current_screen() != Screen::Settings || self.actions.dialog.is_some() {
            return;
        }
        let focus = self.login_focus.clone();
        self.focus_in(Surface::Settings, focus, window, cx);
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
            self.reveal_login(window, cx);
        }
        cx.notify();
    }
    /// Open Settings on its root with the account control focused.
    fn reveal_login(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.route_to_settings(window, cx, |this, window, cx| this.reveal_login(window, cx)) {
            return;
        }
        self.push_settings(false, window, cx);
        if self.current_screen() == Screen::Settings && self.actions.dialog.is_none() {
            let focus = self.login_focus.clone();
            self.focus_in(Surface::Settings, focus, window, cx);
        }
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
    fn apply_appearance(&mut self, appearance: Appearance, cx: &mut Context<Self>) {
        if self.settings.appearance == appearance {
            return;
        }
        if self.actions.read_only {
            self.notifications
                .warning("Saving is disabled. Repair the settings file and relaunch.");
            cx.notify();
            return;
        }
        let mut next = self.settings.clone();
        next.appearance = appearance;
        match self.persist_settings(&next) {
            Ok(()) => {
                self.record_diagnostic(Event::new(Category::Settings, EventKind::Succeeded));
                self.notifications.clear(Source::Settings);
                self.settings = next;
            }
            Err(e) => {
                self.record_diagnostic(Event::failure(Category::Settings, FailureCode::Storage));
                self.notifications.report(
                    Source::Settings,
                    "Could not save the theme. Try again.",
                    &e,
                );
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
        if !self.can_request() {
            self.show_account_tab(cx);
            self.reveal_login(window, cx);
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
            _ => self.writing(action, window, cx),
        }
    }
    /// Hide Wiesel and hand the result to the source app; the clipboard is restored after.
    fn start_paste_back(&mut self, paste: DirectPaste, text: String, cx: &mut Context<Self>) {
        // Retained for the fallback view if the source app cannot be reached.
        self.result = text.clone();
        self.notifications.clear(Source::Request);
        if text.trim().is_empty() {
            self.notifications
                .warning("The model returned no text. Nothing was pasted.");
            return;
        }
        // Paste once per capture: afterwards the selection has been replaced.
        self.selection.source = None;
        self.hide_window(cx);
        #[cfg(test)]
        if let Some(pastes) = &mut self.recorded_pastes {
            pastes.push((paste.source, text));
            self.notifications.success("Result pasted.");
            return;
        }
        self.notifications
            .working(Source::Request, "Pasting result…");
        self.paste_back = Some(clipboard_capture::PasteBack::new(paste.source, text));
    }
    fn poll_paste_back(&mut self, cx: &mut Context<Self>) {
        let Some(result) = self
            .paste_back
            .as_mut()
            .and_then(clipboard_capture::PasteBack::poll)
        else {
            return;
        };
        self.paste_back = None;
        self.notifications.clear(Source::Request);
        match result {
            Ok(()) => self.notifications.success("Result pasted."),
            Err(clipboard_capture::PasteError::Restore(error)) => {
                eprintln!("Wiesel paste cleanup: {error:#}");
                self.notifications.issue(
                    Source::Request,
                    Severity::Error,
                    "Result pasted, but the previous clipboard could not be restored.",
                );
            }
            Err(clipboard_capture::PasteError::NotPasted(error)) => {
                // Never lose a result: show it with a Copy button instead.
                // Paste errors are fixed copy and never contain clipboard or result text.
                self.notifications.issue(
                    Source::Request,
                    Severity::Warning,
                    format!(
                        "{}. Copy the result instead.",
                        error.to_string().trim_end_matches('.')
                    ),
                );
                self.open_surface(Surface::Writing, None, cx);
            }
        }
        cx.notify();
    }
    fn open_chat(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let composer = self.composer.focus_handle(cx);
        self.focus_in(Surface::Chat, composer, window, cx);
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
        self.hud_key(event, window, cx);
        self.notifications
            .update(Instant::now(), self.idle_notification());
        self.notifications.suppress_motion = false;
    }
    fn surface_key_down(
        &mut self,
        surface: Surface,
        event: &KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.notifications.suppress_motion = true;
        match surface {
            Surface::Settings => self.settings_key(event, window, cx),
            Surface::Chat | Surface::Writing => self.content_key(surface, event, window, cx),
        }
        self.notifications
            .update(Instant::now(), self.idle_notification());
        self.notifications.suppress_motion = false;
    }
    /// Launcher keys, as in cmd+tab: arrows/Tab move the highlight, Enter runs it, the
    /// printed numbers run their tile directly and Esc puts the launcher away.
    fn hud_key(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let key = &event.keystroke;
        let plain = !key.modifiers.platform && !key.modifiers.control && !key.modifiers.alt;
        let actions = self.home_actions();
        match key.key.as_str() {
            "escape" => self.escape_hud(cx),
            "left" | "right" | "tab" if plain && !actions.is_empty() => {
                let back = key.key == "left" || (key.key == "tab" && key.modifiers.shift);
                let count = actions.len();
                self.hud_selected = if back {
                    (self.hud_selected + count - 1) % count
                } else {
                    (self.hud_selected + 1) % count
                };
                cx.notify();
            }
            "enter" | "space" if plain => {
                if !event.is_held
                    && let Some(action) = actions.get(self.hud_selected).cloned()
                {
                    self.run_home_action(action, window, cx);
                }
            }
            digit if plain && !event.is_held => {
                // Number keys press the tile printed with the same number.
                let Some(slot) = home_slot_shortcut(digit) else {
                    return;
                };
                if let Some(action) = actions.get(slot).cloned() {
                    self.hud_selected = slot;
                    self.run_home_action(action, window, cx);
                }
            }
            _ => return,
        }
        cx.stop_propagation();
    }
    fn content_key(
        &mut self,
        surface: Surface,
        event: &KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let key = &event.keystroke;
        if key.key == "escape" {
            self.request_close(surface, window, cx);
            cx.stop_propagation();
        } else if surface == Surface::Chat
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
    fn settings_key(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
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
                self.request_close(Surface::Settings, window, cx);
            }
            cx.stop_propagation();
        }
    }
    fn button(id: &'static str, title: &str) -> Stateful<Div> {
        Self::key_button(id, title, theme::Key::Grey)
    }
    /// A labelled shadcn/ui button.
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
        .child(title.to_owned())
    }

    fn label(text: &str) -> Div {
        div()
            .text_size(px(12.))
            .text_color(rgb(theme::p().muted))
            .child(text.to_owned())
    }
    /// The launcher's row of icon tiles, like the cmd+tab switcher.
    fn launcher(&self, cx: &mut Context<Self>) -> Div {
        let actions = self.home_actions();
        let selected = self.hud_selected.min(actions.len().saturating_sub(1));
        div()
            .debug_selector(|| "quick-actions".into())
            .flex_shrink_0()
            .flex()
            .justify_center()
            .gap(px(HUD_TILE_GAP))
            .pt(px(HUD_PADDING))
            .pb(px(HUD_PADDING - 2.))
            .px(px(HUD_PADDING))
            .when(actions.is_empty(), |d| {
                d.child(
                    div()
                        .h(px(HUD_SLOT_HEIGHT))
                        .flex()
                        .flex_col()
                        .items_center()
                        .justify_center()
                        .gap(px(4.))
                        .child(
                            div()
                                .text_size(px(13.))
                                .font_weight(FontWeight::MEDIUM)
                                .child("No quick actions"),
                        )
                        .child(Self::label("Add one with File → New Quick Action…")),
                )
            })
            .children(
                actions
                    .into_iter()
                    .enumerate()
                    .map(|(slot, item)| self.home_pad(slot, item, slot == selected, cx)),
            )
    }
    fn home_pad(
        &self,
        slot: usize,
        item: action_builder::HomeAction,
        selected: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let palette = theme::p();
        let (id, title, icon, _) = item.presentation();
        // A direct-paste action spins on its own tile while the model works.
        let pasting = self.busy && self.direct_paste.is_some();
        let loading = pasting
            && self
                .direct_paste
                .is_some_and(|paste| item.action_id() == paste.action);
        let enabled = self.home_action_enabled(&item);
        let debug_id = id.clone();
        let ink = palette.tile_ink;
        let glyph = if loading {
            // A faint full ring with a short arc circling it, at the glyph's optical size.
            // Constant motion stays linear; a brisk turn makes the wait feel shorter.
            div()
                .debug_selector(move || format!("home-spinner-{slot}"))
                .relative()
                .size(px(26.))
                .flex()
                .items_center()
                .justify_center()
                .child(
                    theme::icon("spinner-track", 22.)
                        .absolute()
                        .text_color(rgba((ink << 8) | 0x29)),
                )
                .child(
                    theme::icon("spinner-arc", 22.)
                        .absolute()
                        .text_color(rgb(ink))
                        .with_animation(
                            "home-spinner",
                            Animation::new(Duration::from_millis(700)).repeat(),
                            |icon, delta| {
                                icon.with_transformation(Transformation::rotate(percentage(delta)))
                            },
                        ),
                )
                .into_any_element()
        } else {
            // The glyph rests a shade quieter and lights up with its tile.
            theme::icon(icon, 26.)
                .text_color(rgb(if selected { ink } else { palette.tile_ink_rest }))
                .into_any_element()
        };
        // Every tile is printed with its position; 1–9 double as keyboard shortcuts.
        let number = (slot < 9).then(|| (slot + 1).to_string());
        let square = theme::tile(div().id(("home-tile", slot)), selected)
            .size(px(HUD_TILE))
            .child(
                theme::tile_well()
                    .flex()
                    .flex_col()
                    .items_center()
                    .justify_center()
                    .gap(px(4.))
                    .pt(px(3.))
                    .child(glyph)
                    .child(
                        div()
                            .h(px(12.))
                            .font_family(theme::MONO)
                            .text_size(px(10.))
                            .line_height(px(12.))
                            .text_color(rgb(palette.tile_number))
                            .children(number),
                    ),
            );
        div()
            .id(gpui::SharedString::from(id.clone()))
            .debug_selector(move || debug_id.clone())
            .role(Role::Button)
            .accessibility_id(format!("wiesel.action.{id}"))
            .aria_label(title.clone())
            .aria_selected(selected)
            .when(loading, |d| {
                d.aria_description("Working; the result will be pasted back")
            })
            .when(!enabled, |d| {
                d.aria_description("Select text in another app to use this action")
            })
            .w(px(HUD_SLOT_WIDTH))
            .h(px(HUD_SLOT_HEIGHT))
            .flex_shrink_0()
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
            .gap(px(8.))
            // Hover and keyboard selection light the icon tile and label, not the slot.
            // Hover never selects a disabled tile; the keyboard may, and sees it muted.
            .when(enabled, |d| d.cursor_pointer().active(|s| s.opacity(0.8)))
            .when((pasting && !loading) || !enabled, |d| d.opacity(0.4))
            .on_hover(cx.listener(move |this, hovered: &bool, _, cx| {
                if *hovered && enabled && this.hud_selected != slot {
                    this.hud_selected = slot;
                    cx.notify();
                }
            }))
            .on_click(cx.listener(move |this, _, window, cx| {
                if !enabled {
                    return;
                }
                this.hud_selected = slot;
                this.run_home_action(item.clone(), window, cx);
            }))
            .child(square)
            .child(
                div()
                    .max_w(px(HUD_SLOT_WIDTH - 8.))
                    .text_size(px(12.))
                    .line_height(px(16.))
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(rgb(if selected {
                        palette.tile_label_selected
                    } else {
                        palette.tile_label
                    }))
                    .whitespace_nowrap()
                    .text_ellipsis()
                    .overflow_hidden()
                    .child(title),
            )
            .into_any_element()
    }
    /// Status message on the left; key hints and the Settings gear on the right.
    fn status_bar(&self, cx: &mut Context<Self>) -> Div {
        let palette = theme::p();
        let wide = self.home_actions().len() >= 5;
        let hint = |text: &'static str| {
            div()
                .text_size(px(11.))
                .text_color(rgb(palette.status_detail))
                .child(text)
        };
        let count = self.home_actions().len().min(9);
        div()
            .debug_selector(|| "status-bar".into())
            .h(px(HUD_STATUS_HEIGHT))
            .flex_shrink_0()
            .flex()
            .items_center()
            .justify_between()
            .gap(px(12.))
            .pl(px(18.))
            .pr(px(8.))
            .border_t_1()
            .border_color(rgba(palette.status_edge))
            .bg(rgba(palette.status_fill))
            .child(
                div()
                    .id("status")
                    .debug_selector(|| "status".into())
                    .role(Role::Status)
                    .accessibility_id("wiesel.status")
                    .aria_label(
                        self.notifications
                            .description()
                            .unwrap_or_else(|| "Ready".into()),
                    )
                    .flex_1()
                    .min_w_0()
                    .child(self.notifications.render()),
            )
            .child(
                div()
                    .flex_shrink_0()
                    .flex()
                    .items_center()
                    .gap(px(12.))
                    .when(wide && count > 0, |d| {
                        d.child(
                            div()
                                .flex()
                                .items_center()
                                .gap(px(5.))
                                .child(theme::kbd(if count == 1 {
                                    "1".to_owned()
                                } else {
                                    format!("1–{count}")
                                }))
                                .child(hint("run").pr(px(6.)))
                                .child(theme::kbd("esc"))
                                .child(hint("close")),
                        )
                    })
                    .child(
                        div()
                            .id("open-settings")
                            .debug_selector(|| "open-settings".into())
                            .role(Role::Button)
                            .aria_label("Open Settings")
                            .accessibility_id("wiesel.settings")
                            .tooltip(|_, cx| cx.new(|_| HeaderTooltip("Settings (⌘,)")).into())
                            .size(px(22.))
                            .flex()
                            .items_center()
                            .justify_center()
                            .rounded(px(6.))
                            .cursor_pointer()
                            .hover(|s| s.bg(rgba(theme::p().tile_selected)))
                            .child(
                                theme::icon("settings", 15.).text_color(rgb(palette.status_label)),
                            )
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.dispatch_menu(MenuCommand::Settings, window, cx)
                            })),
                    ),
            )
    }
    /// The launcher window's size for the configured number of tiles.
    fn hud_size(&self) -> Size<Pixels> {
        hud_size(self.home_actions().len())
    }
    fn sync_hud_size(&self, window: &mut Window) {
        let wanted = self.hud_size();
        let current = window.bounds().size;
        if (current.width - wanted.width).abs() > px(0.5)
            || (current.height - wanted.height).abs() > px(0.5)
        {
            window.resize(wanted);
            // AppKit resizes from the bottom-left corner; keep the launcher centered.
            native::center(window);
        }
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
                                                    .rounded(px(14.))
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
                        .h(px(32.))
                        .px(px(14.))
                        .when(self.busy, |d| d.opacity(0.6))
                        .child(
                            theme::icon("arrow-up", 14.).text_color(rgb(theme::Key::Blue.label())),
                        )
                        .child(if self.busy { "Sending…" } else { "Send" })
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
                                .rounded(px(14.))
                                .map(user_bubble)
                                .flex()
                                .flex_col()
                                .gap(px(6.))
                                .child(
                                    div()
                                        .text_size(px(10.))
                                        .font_weight(FontWeight::MEDIUM)
                                        .text_color(rgb(theme::p().bubble_meta))
                                        .child("Selected text"),
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
                            .child(theme::silkscreen("Wiesel", theme::p().muted))
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
/// Height of a secondary window's title strip (traffic lights, title, status).
pub(crate) const TITLEBAR_HEIGHT: f32 = 48.;
const HUD_PADDING: f32 = 14.;
const HUD_TILE: f32 = 76.;
const HUD_TILE_GAP: f32 = 6.;
const HUD_SLOT_WIDTH: f32 = 104.;
const HUD_SLOT_HEIGHT: f32 = 120.;
const HUD_STATUS_HEIGHT: f32 = 32.;
const HUD_MIN_WIDTH: f32 = 360.;

/// The launcher fits its tiles exactly: padding, slots and the status strip.
fn hud_size(tiles: usize) -> Size<Pixels> {
    let tiles = tiles as f32;
    let width = 2. * HUD_PADDING + tiles * HUD_SLOT_WIDTH + (tiles - 1.).max(0.) * HUD_TILE_GAP;
    size(
        px(width.max(HUD_MIN_WIDTH)),
        px(2. * HUD_PADDING - 2. + HUD_SLOT_HEIGHT + HUD_STATUS_HEIGHT),
    )
}

/// Maps the printed pad numbers 1–9 to Home slots.
fn home_slot_shortcut(key: &str) -> Option<usize> {
    match key.as_bytes() {
        [digit @ b'1'..=b'9'] => Some(usize::from(digit - b'1')),
        _ => None,
    }
}

impl Wiesel {
    fn surface_title(&self, surface: Surface) -> String {
        match surface {
            Surface::Chat => "Chat".into(),
            Surface::Writing if self.writing_title.is_empty() => "Result".into(),
            Surface::Writing => self.writing_title.clone(),
            Surface::Settings => self.screen_title(),
        }
    }
    fn screen_title(&self) -> String {
        match self.current_screen() {
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
    /// A secondary window's title strip: room for the traffic lights, Back on nested
    /// Settings screens, the title, one screen command and the status line. The empty
    /// title area doubles as the window's drag handle.
    fn titlebar(&self, surface: Surface, cx: &mut Context<Self>) -> Div {
        let palette = theme::p();
        let nested = surface == Surface::Settings && !self.navigation.is_root();
        let back = nested.then(|| {
            theme::key(
                div()
                    .id("back")
                    .debug_selector(|| "back".into())
                    .role(Role::Button)
                    .aria_label("Back")
                    .accessibility_id("wiesel.navigation.back")
                    // Preserve the covered screen's focus before the click pops a route.
                    .on_mouse_down(MouseButton::Left, |_, window, _| window.prevent_default())
                    .track_focus(&self.action_control_focus("back", cx))
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
            .size(px(26.))
            .child(theme::icon("arrow-left", 14.).text_color(rgb(theme::Key::Grey.label())))
        });
        let command = match (surface, self.current_screen()) {
            (Surface::Chat, _) => Some(
                Self::button("new-chat", "New chat")
                    .h(px(26.))
                    .px(px(10.))
                    .on_mouse_down(MouseButton::Left, |_, window, _| window.prevent_default())
                    .on_click(cx.listener(|this, _, _, cx| this.new_chat(cx))),
            ),
            (Surface::Settings, Screen::ActionManager) => Some(
                theme::key(
                    self.builder_button_base(
                        "new-action",
                        "New action",
                        action_builder::Command::New,
                        cx,
                    ),
                    theme::Key::Grey,
                )
                .h(px(26.))
                .px(px(10.))
                .child(theme::icon("plus", 13.).text_color(rgb(theme::Key::Grey.label())))
                .child("New action"),
            ),
            _ => None,
        };
        div()
            .debug_selector(|| "toolbar".into())
            .h(px(TITLEBAR_HEIGHT))
            .flex_shrink_0()
            .flex()
            .items_center()
            .gap(px(10.))
            .pl(px(84.))
            .pr(px(12.))
            .border_b_1()
            .border_color(rgba(palette.border))
            .children(back)
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .h_full()
                    .flex()
                    .items_center()
                    .gap(px(12.))
                    .on_mouse_down(MouseButton::Left, |_, window, _| window.start_window_move())
                    .child(
                        div()
                            .flex_shrink_0()
                            .text_size(px(13.))
                            .font_weight(FontWeight::SEMIBOLD)
                            .whitespace_nowrap()
                            .child(self.surface_title(surface)),
                    )
                    .child(
                        div()
                            .id("status")
                            .debug_selector(|| "status".into())
                            .role(Role::Status)
                            .accessibility_id("wiesel.status")
                            .aria_label(
                                self.notifications
                                    .description()
                                    .unwrap_or_else(|| "Ready".into()),
                            )
                            .flex_1()
                            .min_w_0()
                            .flex()
                            .justify_end()
                            .child(self.notifications.render()),
                    ),
            )
            .children(command)
    }
    fn dispatch_menu(&mut self, command: MenuCommand, window: &mut Window, cx: &mut Context<Self>) {
        // Settings-window commands run inside that window when it is open, so dirty-editor
        // confirmations and focus restoration stay there.
        if matches!(
            command,
            MenuCommand::Settings | MenuCommand::QuickActions | MenuCommand::NewAction
        ) && self.route_to_settings(window, cx, move |this, window, cx| {
            this.dispatch_menu(command, window, cx)
        }) {
            return;
        }
        self.notifications.suppress_motion = true;
        // Menu commands have no outside mouse-down, so dismiss editor popups explicitly
        // before another screen covers them.
        if self.current_screen() == Screen::ActionEditor
            && self.actions.dialog.is_none()
            && self.window_is(Surface::Settings, window)
        {
            while self.close_editor_popup(window, cx) {}
        }
        match command {
            MenuCommand::Settings => self.push_settings(false, window, cx),
            MenuCommand::QuickActions => self.open_action_manager(window, cx),
            MenuCommand::NewAction => {
                self.builder_command(action_builder::Command::New, window, cx)
            }
            MenuCommand::NewChat => {
                if !self.is_open(Surface::Chat) {
                    self.run_quick_action(QuickAction::Chat, window, cx);
                }
                if self.is_open(Surface::Chat) {
                    self.new_chat(cx);
                    self.open_chat(window, cx);
                }
            }
        }
        self.notifications
            .update(Instant::now(), self.idle_notification());
        self.notifications.suppress_motion = false;
        cx.notify();
    }
    /// Menu and window commands every window's root handles.
    fn window_actions<E: InteractiveElement>(&self, element: E, cx: &mut Context<Self>) -> E {
        element
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
    }
    fn render_surface(
        &mut self,
        surface: Surface,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        theme::set_appearance(self.settings.appearance, window.appearance());
        let palette = theme::p();
        self.notifications
            .update(Instant::now(), self.idle_notification());
        let content = match surface {
            Surface::Settings => match self.current_screen() {
                Screen::Settings => self.setup(cx).into_any_element(),
                Screen::ActionManager => self.action_manager(cx).into_any_element(),
                Screen::ActionEditor => self.action_editor(cx).into_any_element(),
            },
            Surface::Chat => self.chat(cx).into_any_element(),
            Surface::Writing => self.writing_view(cx).into_any_element(),
        };
        let page = match (surface, self.current_screen()) {
            (Surface::Settings, Screen::Settings) => "wiesel.page.setup",
            (Surface::Settings, Screen::ActionManager) => "wiesel.page.actions",
            (Surface::Settings, Screen::ActionEditor) => "wiesel.page.action-editor",
            (Surface::Chat, _) => "wiesel.page.chat",
            (Surface::Writing, _) => "wiesel.page.writing",
        };
        let root = div()
            .id(("wiesel-surface", surface.index()))
            .role(Role::Group)
            .accessibility_id(page)
            .aria_label(self.surface_title(surface))
            .aria_description(self.request_description())
            .relative()
            .size_full()
            .bg(rgb(palette.background))
            .text_color(rgb(palette.foreground))
            .font_family(theme::SANS)
            .text_size(px(13.))
            .flex()
            .flex_col()
            .track_focus(&self.surface_focus(surface))
            .on_key_down(cx.listener(move |this, event, window, cx| {
                this.surface_key_down(surface, event, window, cx)
            }))
            .on_action(cx.listener(move |this, _: &CloseWindow, window, cx| {
                this.request_close(surface, window, cx)
            }))
            .child(self.titlebar(surface, cx))
            .child(
                div()
                    .debug_selector(|| "deck".into())
                    .flex_1()
                    .min_h_0()
                    .flex()
                    .flex_col()
                    .child(content),
            )
            .when(
                surface == Surface::Settings && self.actions.dialog.is_some(),
                |d| d.child(self.action_dialog(cx)),
            );
        self.window_actions(root, cx).into_any_element()
    }
}
impl Wiesel {
    /// Every window's root announces sign-in and request state (read by the UI smoke test).
    fn request_description(&self) -> &'static str {
        match (self.authenticated, self.busy) {
            (true, false) => "Signed in; request idle",
            (true, true) => "Signed in; request pending",
            (false, false) => "Signed out; request idle",
            (false, true) => "Signed out; request pending",
        }
    }
}
#[derive(Clone, Copy)]
enum MenuCommand {
    Settings,
    QuickActions,
    NewAction,
    NewChat,
}
/// The launcher HUD: glass over the window's native blur, tiles above a status strip.
impl Render for Wiesel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        theme::set_appearance(self.settings.appearance, window.appearance());
        let palette = theme::p();
        self.notifications
            .update(Instant::now(), self.idle_notification());
        let root = div()
            .id("wiesel-root")
            .role(Role::Group)
            .accessibility_id("wiesel.page.launcher")
            .aria_label("Wiesel")
            .aria_description(self.request_description())
            .relative()
            .size_full()
            .overflow_hidden()
            .bg(rgba(palette.hud_tint))
            .text_color(rgb(palette.foreground))
            .font_family(theme::SANS)
            .text_size(px(13.))
            .flex()
            .flex_col()
            .justify_between()
            .track_focus(&self.focus)
            .on_key_down(cx.listener(Self::key_down))
            .on_action(cx.listener(|this, _: &CloseWindow, _, cx| this.escape_hud(cx)))
            .child(self.launcher(cx))
            .child(self.status_bar(cx));
        self.window_actions(root, cx)
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
            MenuItem::separator(),
            MenuItem::action("Close Window", CloseWindow),
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
    // Clicking the Dock icon with no window on screen brings the launcher back.
    let reopen_hud = std::rc::Rc::new(std::cell::Cell::new(None::<gpui::WindowHandle<Wiesel>>));
    let reopen = reopen_hud.clone();
    application.on_reopen(move |cx| {
        if let Some(handle) = reopen.get() {
            let _ = handle.update(cx, |app, window, cx| app.show_hud(window, cx));
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
            KeyBinding::new("cmd-w", CloseWindow, None),
        ]);
        cx.on_action(|_: &HideWiesel, cx| cx.hide());
        cx.set_menus(app_menus());
        // The launcher is a borderless HUD over the desktop's native blur, like cmd+tab.
        let window_size = hud_size(quick_actions::QUICK_ACTIONS.len());
        let bounds = Bounds::centered(None, window_size, cx);
        let handle = match cx.open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                titlebar: None,
                app_owns_titlebar_drag: true,
                is_resizable: false,
                is_minimizable: false,
                window_background: WindowBackgroundAppearance::Blurred,
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
        reopen_hud.set(Some(handle));
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
