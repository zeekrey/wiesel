//! Shared in-process UI fixture: no settings, Keychain, clipboard, hotkeys, or network.
use super::*;
use completion_backend::{CompletionBackend, ControlledBackend};
use gpui::{Modifiers, TestAppContext, VisualTestContext};

pub(super) fn ephemeral_app(
    backend: ControlledBackend,
    window: &Window,
    cx: &mut Context<Wiesel>,
) -> Wiesel {
    let now = settings::utc_now_ms().unwrap();
    let credential = DeviceCredential::validated(
        zeroize::Zeroizing::new(format!("wd_{}", "t".repeat(43))),
        now + 3_600_000,
        uuid::Uuid::from_u128(1),
        now,
    )
    .unwrap();
    let settings = Settings {
        model: "test/chat-model".into(),
        onboarded: true,
        ..Default::default()
    };
    let (tx, rx) = mpsc::channel();
    let (diagnostics_tx, diagnostics_rx) = mpsc::channel();
    // Deliberately construct state rather than calling Wiesel::new, which loads
    // user settings/credentials, checks OS permission, and registers a hotkey.
    Wiesel {
        navigation: Navigation::new(Screen::Settings),
        surfaces: Surfaces::new(cx),
        hud: Some(window.window_handle()),
        hud_visible: true,
        hud_selected: 0,
        hud_hides_app: true,
        settings_startup: false,
        request_generation: 0,
        active_request: None,
        recorded_hides: Some(0),
        recorded_pastes: Some(Vec::new()),
        model_picker: cx.new(|cx| model_picker::ModelPicker::new(settings.model.clone(), cx)),
        tier_pickers: Wiesel::new_tier_pickers(&settings, cx),
        provider_tab: Wiesel::initial_provider_tab(&settings),
        ollama_status: None,
        ollama_checking: false,
        settings,
        actions: action_builder::ActionBuilder::fixture(),
        diagnostics: None,
        diagnostics_initializing: false,
        diagnostics_busy: false,
        controlled_diagnostic_requests: None,
        diagnostics_tx,
        diagnostics_rx,
        diagnostic_flush: None,
        diagnostic_flushed_at: Instant::now(),
        diagnostics_open_focus: cx.focus_handle().tab_stop(true),
        diagnostics_clear_focus: cx.focus_handle().tab_stop(true),
        manager: None,
        hotkey: None,
        session: DesktopSession {
            credential: Some(Arc::new(credential)),
            ..Default::default()
        },
        login_focus: cx.focus_handle().tab_stop(true),
        urls: mpsc::channel().1,
        status_loading: false,
        device_status: None,
        recovery: CredentialRecovery::Ready,
        composer: cx.new(|cx| {
            TextInput::new("Ask anything…", false, cx).with_accessibility_id("wiesel.chat.composer")
        }),
        focus: cx.focus_handle(),
        recording: false,
        authenticated: true,
        accessibility_granted: false,
        permission_checked_at: Instant::now(),
        busy: false,
        notifications: Notifications::default(),
        selection: selection::Capture::default(),
        pending_capture: None,
        direct_paste: None,
        paste_back: None,
        capture_scope: None,
        failed_chat: Default::default(),
        result: String::new(),
        writing_title: String::new(),
        messages: vec![],
        chat_action_model: None,
        tx,
        rx,
        chat_scroll: ScrollHandle::new(),
        chat_follow_bottom: true,
        streaming_chat: None,
        completion_backend: CompletionBackend::Controlled(backend),
    }
}

pub(super) fn setup(
    cx: &mut TestAppContext,
) -> (Entity<Wiesel>, ControlledBackend, &mut VisualTestContext) {
    // GPUI's default TestPlatform uses NoopTextSystem, not AppKit/a GPU. It
    // supports layout and input without a display, installed fonts, or OS AX.
    cx.update(|cx| {
        cx.set_reduce_motion(true);
        cx.bind_keys([KeyBinding::new(
            "cmd-a",
            input::SelectAll,
            Some("TextInput"),
        )]);
    });
    let backend = ControlledBackend::default();
    let (app, cx) = cx.add_window_view(|window, cx| {
        let app = ephemeral_app(backend.clone(), window, cx);
        window.focus(&app.focus, cx);
        app
    });
    cx.simulate_resize(hud_size(5));
    (app, backend, cx)
}

/// Settings and quick-action management are macOS menu commands, not on-screen keys.
pub(super) fn menu(cx: &mut VisualTestContext, action: impl gpui::Action) {
    cx.dispatch_action(action);
    cx.run_until_parked();
}

// Wiesel renders several windows: the launcher HUD (the fixture's own window) plus
// Settings, Chat and Writing. These helpers route input to the window that shows a
// control, or to the key window for typing, like a real user would.

/// Every open window, the launcher first.
pub(super) fn windows(cx: &VisualTestContext) -> Vec<VisualTestContext> {
    cx.cx
        .windows()
        .into_iter()
        .map(|window| VisualTestContext::from_window(window, &cx.cx))
        .collect()
}
/// The window currently rendering `selector`.
pub(super) fn on(cx: &mut VisualTestContext, selector: &'static str) -> Option<VisualTestContext> {
    cx.run_until_parked();
    windows(cx)
        .into_iter()
        .find_map(|mut window| window.debug_bounds(selector).map(|_| window))
}
/// Bounds of `selector` in whichever window renders it.
pub(super) fn bounds(
    cx: &mut VisualTestContext,
    selector: &'static str,
) -> Option<gpui::Bounds<Pixels>> {
    on(cx, selector).and_then(|mut window| window.debug_bounds(selector))
}
/// The key window, where keystrokes land; the launcher when none is active.
pub(super) fn key_window(cx: &mut VisualTestContext) -> VisualTestContext {
    cx.run_until_parked();
    let open = cx.cx.windows();
    let active = cx
        .update(|_, cx| cx.active_window())
        .filter(|window| open.contains(window));
    VisualTestContext::from_window(active.unwrap_or(open[0]), &cx.cx)
}
/// A surface's window; panics when it is not open.
pub(super) fn on_surface(
    app: &Entity<Wiesel>,
    cx: &mut VisualTestContext,
    surface: Surface,
) -> VisualTestContext {
    cx.run_until_parked();
    let window = cx
        .read_entity(app, |app, _| app.surface_window(surface))
        .unwrap_or_else(|| panic!("{surface:?} window is open"));
    VisualTestContext::from_window(window, &cx.cx)
}
pub(super) fn keys(cx: &mut VisualTestContext, keystrokes: &str) {
    let mut window = key_window(cx);
    window.simulate_keystrokes(keystrokes);
    cx.run_until_parked();
}
pub(super) fn input(cx: &mut VisualTestContext, text: &str) {
    let mut window = key_window(cx);
    window.simulate_input(text);
    cx.run_until_parked();
}
pub(super) fn click(cx: &mut VisualTestContext, selector: &'static str) {
    let mut window = on(cx, selector).unwrap_or_else(|| panic!("rendered {selector}"));
    let bounds = window.debug_bounds(selector).expect("rendered control");
    window.simulate_click(bounds.center(), Modifiers::none());
    cx.run_until_parked();
}
/// Run `f` with the key window, e.g. to check focus where keystrokes land.
pub(super) fn in_key_window<R>(
    cx: &mut VisualTestContext,
    f: impl FnOnce(&mut Window, &mut App) -> R,
) -> R {
    let mut window = key_window(cx);
    window.update(f)
}

pub(super) fn receive_results(app: &Entity<Wiesel>, cx: &mut VisualTestContext) {
    // Explicit releases enqueue real scoped ResultEvents. Drain only the same
    // event receiver used by poll, avoiding its OS permission/hotkey side effects
    // and any wall-clock timers, sleeping, or uncontrolled background threads.
    cx.update(|window, cx| {
        app.update(cx, |app, cx| app.receive_results(window, cx));
    });
    cx.run_until_parked();
}
