//! In-process chat tests: no settings, Keychain, clipboard, hotkeys, or network.
use super::*;
use completion_backend::{CompletionBackend, ControlledBackend};
use gpui::{KeyUpEvent, Keystroke, Modifiers, TestAppContext, VisualTestContext};

fn ephemeral_app(backend: ControlledBackend, cx: &mut Context<Wiesel>) -> Wiesel {
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
        navigation: Navigation::new(Screen::Launcher),
        request_generation: 0,
        active_request: None,
        recorded_hides: Some(0),
        model_picker: cx.new(|cx| model_picker::ModelPicker::new(settings.model.clone(), cx)),
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
        let app = ephemeral_app(backend.clone(), cx);
        window.focus(&app.focus, cx);
        app
    });
    cx.simulate_resize(size(px(WINDOW_WIDTH), px(WINDOW_HEIGHT)));
    (app, backend, cx)
}

/// Settings and quick-action management are macOS menu commands, not on-screen keys.
pub(super) fn menu(cx: &mut VisualTestContext, action: impl gpui::Action) {
    cx.dispatch_action(action);
    cx.run_until_parked();
}

pub(super) fn click(cx: &mut VisualTestContext, selector: &'static str) {
    let bounds = cx.debug_bounds(selector).expect("rendered control");
    cx.simulate_click(bounds.center(), Modifiers::none());
}

fn open_chat(app: &Entity<Wiesel>, cx: &mut VisualTestContext) {
    click(cx, "chat");
    cx.read_entity(app, |app, cx| {
        assert!(app.current_screen() == Screen::Chat);
        assert!(app.composer.read(cx).content.is_empty());
    });
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

fn history(messages: &[Message]) -> Vec<(&str, &str)> {
    messages
        .iter()
        .map(|message| (message.role.as_str(), message.content.as_str()))
        .collect()
}

#[gpui::test]
fn chat_send_commits_first_reply_and_clears_composer(cx: &mut TestAppContext) {
    let (app, backend, cx) = setup(cx);
    open_chat(&app, cx);
    cx.simulate_input("Hello Wiesel");
    cx.read_entity(&app, |app, cx| {
        assert_eq!(app.composer.read(cx).content.as_ref(), "Hello Wiesel");
    });
    click(cx, "send");
    let (model, messages, chat) = backend.request(0);
    assert_eq!(model, "test/chat-model");
    assert!(chat);
    assert_eq!(
        history(&messages),
        vec![
            ("system", "You are a helpful, clear and concise assistant."),
            ("user", "Hello Wiesel"),
        ]
    );
    cx.read_entity(&app, |app, cx| {
        assert!(app.busy);
        assert_eq!(history(&app.messages), vec![("user", "Hello Wiesel")]);
        assert!(app.composer.read(cx).content.is_empty());
    });

    backend.finish(0, Ok("Hello from the fake backend".into()));
    receive_results(&app, cx);
    cx.read_entity(&app, |app, cx| {
        assert_eq!(
            history(&app.messages),
            vec![
                ("user", "Hello Wiesel"),
                ("assistant", "Hello from the fake backend"),
            ]
        );
        assert!(!app.busy);
        assert!(app.streaming_chat.is_none());
        assert!(app.composer.read(cx).content.is_empty());
    });
}

#[gpui::test]
fn chat_enter_sends_committed_multi_turn_history(cx: &mut TestAppContext) {
    let (app, backend, cx) = setup(cx);
    open_chat(&app, cx);
    cx.simulate_input("First question");
    cx.simulate_keystrokes("enter");
    backend.finish(0, Ok("First answer".into()));
    receive_results(&app, cx);

    cx.simulate_input("Follow up");
    cx.simulate_keystrokes("enter");
    assert_eq!(backend.request_count(), 2);
    assert_eq!(
        history(&backend.request(1).1),
        vec![
            ("system", "You are a helpful, clear and concise assistant."),
            ("user", "First question"),
            ("assistant", "First answer"),
            ("user", "Follow up"),
        ]
    );
    backend.finish(1, Ok("Second answer".into()));
    receive_results(&app, cx);
    cx.read_entity(&app, |app, _| {
        assert_eq!(
            history(&app.messages),
            vec![
                ("user", "First question"),
                ("assistant", "First answer"),
                ("user", "Follow up"),
                ("assistant", "Second answer"),
            ]
        );
        assert!(!app.busy);
    });
}

#[gpui::test]
fn chat_busy_prevents_duplicate_send_and_enter_without_losing_new_draft(cx: &mut TestAppContext) {
    let (app, backend, cx) = setup(cx);
    open_chat(&app, cx);
    cx.simulate_input("Pending question");
    cx.simulate_keystrokes("enter enter");
    cx.simulate_input("New draft");
    click(cx, "send");
    cx.simulate_keystrokes("enter");
    assert_eq!(backend.request_count(), 1);
    cx.read_entity(&app, |app, cx| {
        assert!(app.busy);
        assert_eq!(history(&app.messages), vec![("user", "Pending question")]);
        assert_eq!(app.composer.read(cx).content.as_ref(), "New draft");
    });
    backend.finish(0, Ok("Answer".into()));
    receive_results(&app, cx);
    cx.read_entity(&app, |app, cx| {
        assert!(!app.busy);
        assert_eq!(app.composer.read(cx).content.as_ref(), "New draft");
    });
}

#[gpui::test]
fn chat_streamed_preview_is_not_committed_until_completion(cx: &mut TestAppContext) {
    let (app, backend, cx) = setup(cx);
    open_chat(&app, cx);
    cx.simulate_input("Stream please");
    click(cx, "send");
    backend.delta(0, "Partial ");
    receive_results(&app, cx);
    backend.delta(0, "answer");
    receive_results(&app, cx);
    cx.read_entity(&app, |app, _| {
        assert!(app.busy);
        assert_eq!(history(&app.messages), vec![("user", "Stream please")]);
        assert_eq!(
            app.streaming_chat.as_ref().unwrap().content,
            "Partial answer"
        );
    });
    backend.finish(0, Ok("Partial answer completed".into()));
    receive_results(&app, cx);
    cx.read_entity(&app, |app, _| {
        assert!(!app.busy);
        assert!(app.streaming_chat.is_none());
        assert_eq!(
            history(&app.messages),
            vec![
                ("user", "Stream please"),
                ("assistant", "Partial answer completed"),
            ]
        );
    });
}

#[gpui::test]
fn chat_failed_stream_restores_draft_and_discards_preview(cx: &mut TestAppContext) {
    let (app, backend, cx) = setup(cx);
    open_chat(&app, cx);
    cx.simulate_input("Failed draft");
    cx.simulate_keystrokes("enter");
    backend.delta(0, "Incomplete reply");
    receive_results(&app, cx);
    backend.finish(0, Err(anyhow::anyhow!("Controlled failure")));
    receive_results(&app, cx);
    cx.read_entity(&app, |app, cx| {
        assert!(!app.busy);
        assert!(app.messages.is_empty());
        assert!(app.streaming_chat.is_none());
        assert!(app.failed_chat.is_empty());
        assert_eq!(app.composer.read(cx).content.as_ref(), "Failed draft");
    });
    assert_eq!(
        backend.request_count(),
        1,
        "failures never retry automatically"
    );
}

#[gpui::test]
fn chat_failed_send_preserves_newer_draft_and_committed_history(cx: &mut TestAppContext) {
    let (app, backend, cx) = setup(cx);
    open_chat(&app, cx);
    cx.simulate_input("First question");
    cx.simulate_keystrokes("enter");
    backend.finish(0, Ok("First answer".into()));
    receive_results(&app, cx);
    cx.simulate_input("Failed question");
    click(cx, "send");
    click(cx, "chat-composer");
    cx.simulate_input("Newer draft");
    backend.finish(1, Err(anyhow::anyhow!("Controlled failure")));
    receive_results(&app, cx);
    cx.read_entity(&app, |app, cx| {
        assert!(!app.busy);
        assert_eq!(app.composer.read(cx).content.as_ref(), "Newer draft");
        assert_eq!(app.failed_chat, vec!["Failed question".to_owned()]);
        assert_eq!(
            history(&app.messages),
            vec![("user", "First question"), ("assistant", "First answer")]
        );
    });
    assert_eq!(backend.request_count(), 2);
}

#[gpui::test]
fn chat_rejects_stale_generation_deltas_and_completion(cx: &mut TestAppContext) {
    let (app, backend, cx) = setup(cx);
    open_chat(&app, cx);
    cx.simulate_input("Question");
    cx.simulate_keystrokes("enter");
    app.update(cx, |app, _| app.session.cancel_login());
    backend.delta(0, "Stale preview");
    receive_results(&app, cx);
    cx.read_entity(&app, |app, _| {
        assert!(app.busy, "a stale delta cannot settle the physical request");
        assert!(app.streaming_chat.as_ref().unwrap().content.is_empty());
    });
    backend.finish(0, Ok("Stale completion".into()));
    receive_results(&app, cx);
    cx.read_entity(&app, |app, _| {
        assert!(
            !app.busy,
            "the exact stale terminal settles its physical request slot"
        );
        assert!(app.active_request.is_none());
        assert_eq!(history(&app.messages), vec![("user", "Question")]);
        assert!(app.streaming_chat.is_none());
    });
}

#[gpui::test]
fn chat_requires_verified_authentication_even_with_injected_backend(cx: &mut TestAppContext) {
    let (app, backend, cx) = setup(cx);
    open_chat(&app, cx);
    app.update(cx, |app, _| app.authenticated = false);
    cx.simulate_input("Unsent draft");
    click(cx, "send");
    cx.simulate_keystrokes("enter");
    assert_eq!(backend.request_count(), 0);
    cx.read_entity(&app, |app, cx| {
        assert!(!app.busy);
        assert!(app.messages.is_empty());
        assert!(app.streaming_chat.is_none());
        assert_eq!(app.composer.read(cx).content.as_ref(), "Unsent draft");
    });
}

#[gpui::test]
fn chat_rejects_deltas_and_completion_from_replaced_device(cx: &mut TestAppContext) {
    let (app, backend, cx) = setup(cx);
    open_chat(&app, cx);
    cx.simulate_input("Question");
    cx.simulate_keystrokes("enter");
    app.update(cx, |app, _| {
        let now = settings::utc_now_ms().unwrap();
        app.session.credential = Some(Arc::new(
            DeviceCredential::validated(
                zeroize::Zeroizing::new(format!("wd_{}", "t".repeat(43))),
                now + 3_600_000,
                uuid::Uuid::from_u128(2),
                now,
            )
            .unwrap(),
        ));
    });
    backend.delta(0, "Previous device preview");
    backend.finish(0, Ok("Previous device completion".into()));
    receive_results(&app, cx);
    cx.read_entity(&app, |app, _| {
        assert!(!app.busy);
        assert!(app.active_request.is_none());
        assert_eq!(history(&app.messages), vec![("user", "Question")]);
        assert!(app.streaming_chat.is_none());
    });
}

#[gpui::test]
fn chat_requires_device_credential_even_with_injected_backend(cx: &mut TestAppContext) {
    let (app, backend, cx) = setup(cx);
    open_chat(&app, cx);
    app.update(cx, |app, _| app.session.credential = None);
    cx.simulate_input("Unsent draft");
    cx.simulate_keystrokes("enter");
    assert_eq!(backend.request_count(), 0);
    cx.read_entity(&app, |app, cx| {
        assert!(!app.busy);
        assert!(app.messages.is_empty());
        assert!(app.streaming_chat.is_none());
        assert_eq!(app.composer.read(cx).content.as_ref(), "Unsent draft");
    });
}

#[gpui::test]
fn diagnostic_initialization_failure_does_not_block_chat_or_authentication(
    cx: &mut TestAppContext,
) {
    let (app, backend, cx) = setup(cx);
    cx.update(|_, cx| {
        app.update(cx, |app, cx| {
            app.diagnostics_initializing = true;
            app.diagnostics_tx
                .send(DiagnosticUpdate::Initialized(Err(
                    DiagnosticError::UnsafePath,
                )))
                .unwrap();
            app.poll_diagnostics(cx);
            assert!(!app.diagnostics_initializing);
            assert!(app.diagnostics.is_none());
            assert!(app.authenticated);
            assert!(!app.busy);
        })
    });
    open_chat(&app, cx);
    cx.update(|_, cx| {
        app.update(cx, |app, cx| {
            app.composer
                .update(cx, |input, cx| input.set("private draft", cx));
            app.send_chat(cx);
        })
    });
    assert_eq!(backend.request_count(), 1);
    backend.finish(0, Ok("private reply".into()));
    receive_results(&app, cx);
    cx.read_entity(&app, |app, _| assert_eq!(app.messages.len(), 2));
}

#[gpui::test]
fn diagnostic_controls_complete_independently_of_login_generation(cx: &mut TestAppContext) {
    let (app, _, cx) = setup(cx);
    cx.update(|_, cx| {
        app.update(cx, |app, cx| {
            app.diagnostics_busy = true;
            app.session.cancel_login();
            app.diagnostics_tx
                .send(DiagnosticUpdate::Controlled(
                    DiagnosticControl::ClearLogs,
                    Err(DiagnosticError::OtherSessionActive),
                ))
                .unwrap();
            app.poll_diagnostics(cx);
            assert!(!app.diagnostics_busy);
            app.diagnostics_busy = true;
            app.diagnostics_tx
                .send(DiagnosticUpdate::Controlled(
                    DiagnosticControl::ClearLogs,
                    Ok(()),
                ))
                .unwrap();
            app.poll_diagnostics(cx);
            assert!(!app.diagnostics_busy);
            assert!(app.authenticated);
        })
    });
}

fn setup_settings(cx: &mut TestAppContext) -> (Entity<Wiesel>, &mut VisualTestContext) {
    let (app, _, cx) = setup(cx);
    cx.update(|window, cx| {
        app.update(cx, |app, cx| {
            app.push_settings(false, window, cx);
            cx.notify();
        });
    });
    cx.run_until_parked();
    (app, cx)
}

#[gpui::test]
fn settings_tab_and_shift_tab_reach_both_diagnostic_controls(cx: &mut TestAppContext) {
    let (app, cx) = setup_settings(cx);
    cx.simulate_keystrokes("tab");
    cx.update(|window, cx| assert!(app.read(cx).diagnostics_open_focus.is_focused(window)));
    cx.simulate_keystrokes("tab");
    cx.update(|window, cx| assert!(app.read(cx).diagnostics_clear_focus.is_focused(window)));
    cx.simulate_keystrokes("shift-tab");
    cx.update(|window, cx| assert!(app.read(cx).diagnostics_open_focus.is_focused(window)));
    cx.simulate_keystrokes("shift-tab");
    cx.update(|window, cx| assert!(app.read(cx).login_focus.is_focused(window)));
}

#[gpui::test]
fn settings_tab_does_not_traverse_while_recording_a_shortcut(cx: &mut TestAppContext) {
    let (app, cx) = setup_settings(cx);
    cx.update(|window, cx| {
        app.update(cx, |app, cx| {
            app.recording = true;
            window.focus(&app.focus, cx);
            cx.notify();
        });
    });
    cx.run_until_parked();
    // These modifier-free shortcuts are rejected by recording without touching
    // hotkey registration/settings persistence. They must not become traversal.
    cx.simulate_keystrokes("tab shift-tab");
    cx.update(|window, cx| {
        let app = app.read(cx);
        assert!(app.recording);
        assert!(app.focus.is_focused(window));
        assert_eq!(app.settings.hotkey, Settings::default().hotkey);
    });
}

#[gpui::test]
fn settings_lit_record_key_cancels_recording(cx: &mut TestAppContext) {
    let (app, cx) = setup_settings(cx);
    cx.simulate_resize(size(px(800.), px(1600.)));
    click(cx, "record");
    cx.read_entity(&app, |app, _| assert!(app.recording));
    click(cx, "record");
    cx.read_entity(&app, |app, _| {
        assert!(!app.recording);
        assert_eq!(app.settings.hotkey, Settings::default().hotkey);
    });
}

#[gpui::test]
fn signing_in_completes_onboarding_without_a_save_step(cx: &mut TestAppContext) {
    let (app, _, cx) = setup(cx);
    app.update(cx, |app, _| {
        app.settings.onboarded = false;
        app.complete_onboarding();
        assert!(app.settings.onboarded);
    });
}

#[gpui::test]
fn settings_modified_tab_does_not_traverse(cx: &mut TestAppContext) {
    let (app, cx) = setup_settings(cx);
    cx.simulate_keystrokes("cmd-tab ctrl-tab alt-tab cmd-shift-tab");
    cx.update(|window, cx| assert!(app.read(cx).login_focus.is_focused(window)));
}

fn assert_diagnostic_key_press_activates_once(
    cx: &mut TestAppContext,
    control: DiagnosticControl,
    key: &str,
) {
    let (app, cx) = setup_settings(cx);
    cx.update(|window, cx| {
        app.update(cx, |app, cx| {
            app.controlled_diagnostic_requests = Some(Vec::new());
            window.focus(
                match control {
                    DiagnosticControl::OpenFolder => &app.diagnostics_open_focus,
                    DiagnosticControl::ClearLogs => &app.diagnostics_clear_focus,
                },
                cx,
            );
            cx.notify();
        });
    });
    cx.run_until_parked();
    let keystroke = Keystroke::parse(key).unwrap();
    cx.simulate_event(KeyDownEvent {
        keystroke: keystroke.clone(),
        is_held: false,
        prefer_character_input: false,
    });
    // Inject the fast-completion race while the key is still held. With the old
    // handlers this completes the key-down operation and permits a duplicate
    // key-up click. With keyboard on_click alone no action has started yet.
    cx.update(|_, cx| {
        app.update(cx, |app, cx| {
            app.diagnostics_tx
                .send(DiagnosticUpdate::Controlled(control, Ok(())))
                .unwrap();
            app.poll_diagnostics(cx);
            assert!(!app.diagnostics_busy);
        });
    });
    cx.run_until_parked();
    cx.simulate_event(KeyUpEvent { keystroke });
    cx.read_entity(&app, |app, _| {
        let requests = app.controlled_diagnostic_requests.as_ref().unwrap();
        assert_eq!(
            requests.len(),
            1,
            "one activation for the entire held key press"
        );
        assert!(matches!(
            (requests[0], control),
            (DiagnosticControl::OpenFolder, DiagnosticControl::OpenFolder)
                | (DiagnosticControl::ClearLogs, DiagnosticControl::ClearLogs)
        ));
        assert!(app.diagnostics_busy);
        assert!(app.diagnostics.is_none());
    });
}

#[gpui::test]
fn diagnostic_open_folder_held_enter_activates_once(cx: &mut TestAppContext) {
    assert_diagnostic_key_press_activates_once(cx, DiagnosticControl::OpenFolder, "enter");
}

#[gpui::test]
fn diagnostic_open_folder_held_space_activates_once(cx: &mut TestAppContext) {
    assert_diagnostic_key_press_activates_once(cx, DiagnosticControl::OpenFolder, "space");
}

#[gpui::test]
fn diagnostic_clear_logs_held_enter_activates_once(cx: &mut TestAppContext) {
    assert_diagnostic_key_press_activates_once(cx, DiagnosticControl::ClearLogs, "enter");
}

#[gpui::test]
fn diagnostic_clear_logs_held_space_activates_once(cx: &mut TestAppContext) {
    assert_diagnostic_key_press_activates_once(cx, DiagnosticControl::ClearLogs, "space");
}

#[gpui::test]
fn navigation_settings_back_restores_chat_draft_selection_and_focus(cx: &mut TestAppContext) {
    let (app, _, cx) = setup(cx);
    open_chat(&app, cx);
    cx.simulate_input("Chat draft");
    cx.simulate_keystrokes("cmd-a");
    let chat_id = cx.read_entity(&app, |app, _| app.navigation.current().id);
    menu(cx, OpenSettings);
    menu(cx, OpenSettings);
    cx.read_entity(&app, |app, _| {
        assert_eq!(app.current_screen(), Screen::Settings);
        assert_eq!(
            app.navigation.depth(),
            3,
            "duplicate Settings must not mount"
        );
    });
    click(cx, "back");
    cx.update(|window, cx| {
        let app = app.read(cx);
        assert_eq!(app.current_screen(), Screen::Chat);
        assert_eq!(app.navigation.current().id, chat_id);
        assert!(app.composer.focus_handle(cx).is_focused(window));
        assert_eq!(app.composer.read(cx).content.as_ref(), "Chat draft");
    });
    // Selection survives being covered: typing replaces, rather than appends.
    cx.simulate_input("Replacement");
    cx.read_entity(&app, |app, cx| {
        assert_eq!(app.composer.read(cx).content.as_ref(), "Replacement")
    });
    click(cx, "back");
    cx.update(|window, cx| {
        let app = app.read(cx);
        assert_eq!(app.current_screen(), Screen::Launcher);
        assert!(app.navigation.is_root());
        assert!(app.focus.is_focused(window));
    });
}

#[gpui::test]
fn navigation_escape_pops_one_entry_and_root_escape_records_hide_without_underflow(
    cx: &mut TestAppContext,
) {
    let (app, _, cx) = setup(cx);
    open_chat(&app, cx);
    cx.simulate_input("Retained draft");
    menu(cx, OpenSettings);
    cx.simulate_keystrokes("escape");
    cx.read_entity(&app, |app, _| {
        assert_eq!(app.current_screen(), Screen::Chat);
        assert_eq!(app.navigation.depth(), 2);
        assert_eq!(app.recorded_hides, Some(0));
    });
    cx.simulate_keystrokes("escape");
    cx.read_entity(&app, |app, _| {
        assert!(app.navigation.is_root());
        assert_eq!(app.recorded_hides, Some(0));
    });
    cx.simulate_keystrokes("escape escape");
    cx.read_entity(&app, |app, cx| {
        assert_eq!(app.navigation.depth(), 1);
        assert_eq!(app.current_screen(), Screen::Launcher);
        assert_eq!(
            app.recorded_hides,
            Some(2),
            "hide intent, not native hiding"
        );
        assert_eq!(app.composer.read(cx).content.as_ref(), "Retained draft");
    });
}

#[gpui::test]
fn navigation_recording_escape_takes_precedence(cx: &mut TestAppContext) {
    let (app, cx) = setup_settings(cx);
    // Make the real settings controls visible without involving a native window.
    cx.simulate_resize(size(px(800.), px(1600.)));
    click(cx, "record");
    cx.read_entity(&app, |app, _| assert!(app.recording));
    cx.simulate_keystrokes("escape");
    cx.read_entity(&app, |app, _| {
        assert!(!app.recording);
        assert_eq!(app.current_screen(), Screen::Settings);
        assert_eq!(app.navigation.depth(), 2);
    });
    cx.simulate_keystrokes("escape");
    cx.read_entity(&app, |app, _| assert!(app.navigation.is_root()));
}

#[gpui::test]
fn navigation_chat_completion_while_covered_preserves_route_messages_and_new_draft(
    cx: &mut TestAppContext,
) {
    let (app, backend, cx) = setup(cx);
    open_chat(&app, cx);
    cx.simulate_input("Question");
    cx.simulate_keystrokes("enter");
    cx.simulate_input("New draft");
    menu(cx, OpenSettings);
    backend.delta(0, "Preview");
    receive_results(&app, cx);
    cx.read_entity(&app, |app, _| {
        assert!(app.busy);
        assert_eq!(app.current_screen(), Screen::Settings);
        assert_eq!(app.streaming_chat.as_ref().unwrap().content, "Preview");
    });
    backend.finish(0, Ok("Completed answer".into()));
    receive_results(&app, cx);
    cx.read_entity(&app, |app, cx| {
        assert!(!app.busy);
        assert_eq!(app.current_screen(), Screen::Settings);
        assert_eq!(app.navigation.depth(), 3);
        assert_eq!(
            history(&app.messages),
            vec![("user", "Question"), ("assistant", "Completed answer")]
        );
        assert_eq!(app.composer.read(cx).content.as_ref(), "New draft");
    });
    click(cx, "back");
    cx.update(|window, cx| assert!(app.read(cx).composer.focus_handle(cx).is_focused(window)));
}

#[gpui::test]
fn navigation_popped_chat_request_remains_busy_and_completion_does_not_resurrect_chat(
    cx: &mut TestAppContext,
) {
    let (app, backend, cx) = setup(cx);
    open_chat(&app, cx);
    cx.simulate_input("Question");
    cx.simulate_keystrokes("enter");
    cx.simulate_input("New draft");
    cx.simulate_keystrokes("escape");
    // A second inference cannot be started from Home while the old view is popped.
    cx.simulate_keystrokes("1");
    cx.read_entity(&app, |app, _| {
        assert!(app.busy);
        assert!(app.navigation.is_root());
    });
    assert_eq!(backend.request_count(), 1);
    click(cx, "chat");
    click(cx, "new-chat");
    cx.simulate_keystrokes("enter escape");
    assert_eq!(backend.request_count(), 1);
    backend.delta(0, "Late preview");
    backend.finish(0, Ok("Late answer".into()));
    receive_results(&app, cx);
    cx.read_entity(&app, |app, cx| {
        assert!(app.navigation.is_root());
        assert!(!app.busy);
        assert_eq!(
            history(&app.messages),
            vec![("user", "Question"), ("assistant", "Late answer")]
        );
        assert_eq!(app.composer.read(cx).content.as_ref(), "New draft");
    });
    click(cx, "chat");
    cx.read_entity(&app, |app, _| assert_eq!(app.messages.len(), 2));
}

#[gpui::test]
fn navigation_chat_failure_while_covered_retains_recoverable_draft(cx: &mut TestAppContext) {
    let (app, backend, cx) = setup(cx);
    open_chat(&app, cx);
    cx.simulate_input("Failed question");
    cx.simulate_keystrokes("enter");
    cx.simulate_input("New draft");
    menu(cx, OpenSettings);
    backend.finish(0, Err(anyhow::anyhow!("Controlled failure")));
    receive_results(&app, cx);
    cx.read_entity(&app, |app, cx| {
        assert_eq!(app.current_screen(), Screen::Settings);
        assert!(!app.busy);
        assert!(app.messages.is_empty());
        assert_eq!(app.failed_chat, vec!["Failed question".to_owned()]);
        assert_eq!(app.composer.read(cx).content.as_ref(), "New draft");
    });
    click(cx, "back");
    assert_eq!(backend.request_count(), 1, "no retry on failure");
}

#[gpui::test]
fn navigation_chat_failure_after_pop_restores_draft_without_changing_route(
    cx: &mut TestAppContext,
) {
    let (app, backend, cx) = setup(cx);
    open_chat(&app, cx);
    cx.simulate_input("Recover me");
    cx.simulate_keystrokes("enter escape");
    backend.finish(0, Err(anyhow::anyhow!("Controlled failure")));
    receive_results(&app, cx);
    cx.read_entity(&app, |app, cx| {
        assert!(app.navigation.is_root());
        assert!(!app.busy);
        assert!(app.messages.is_empty());
        assert_eq!(app.composer.read(cx).content.as_ref(), "Recover me");
    });
    click(cx, "chat");
    cx.simulate_keystrokes("enter");
    assert_eq!(backend.request_count(), 2, "only explicit Enter resubmits");
}

#[gpui::test]
fn navigation_replayed_old_request_does_not_mutate_newer_chat_request(cx: &mut TestAppContext) {
    let (app, backend, cx) = setup(cx);
    open_chat(&app, cx);
    cx.simulate_input("First");
    cx.simulate_keystrokes("enter");
    let old_scope = cx.read_entity(&app, |app, _| EventScope {
        completion: app.active_request,
        ..app.session.scope()
    });
    backend.finish(0, Ok("First answer".into()));
    receive_results(&app, cx);
    click(cx, "new-chat");
    cx.simulate_input("Second");
    cx.simulate_keystrokes("enter");
    // Replay already-settled events through the normal receiver. Same login/device,
    // different request ownership; ControlledBackend itself disallows double finish.
    app.update(cx, |app, _| {
        app.tx
            .send(ResultEvent {
                scope: old_scope,
                result: ResultPayload::ChatDelta("Old preview".into()),
            })
            .unwrap();
        app.tx
            .send(ResultEvent {
                scope: old_scope,
                result: ResultPayload::Completion(Ok("Old completion".into()), true),
            })
            .unwrap();
    });
    receive_results(&app, cx);
    cx.read_entity(&app, |app, _| {
        assert!(app.busy);
        assert_eq!(history(&app.messages), vec![("user", "Second")]);
        assert!(app.streaming_chat.as_ref().unwrap().content.is_empty());
    });
    backend.finish(1, Ok("Second answer".into()));
    receive_results(&app, cx);
    cx.read_entity(&app, |app, _| {
        assert_eq!(
            history(&app.messages),
            vec![("user", "Second"), ("assistant", "Second answer")]
        )
    });
}

#[gpui::test]
fn navigation_busy_new_chat_is_blocked_and_session_invalidation_rejects_covered_completion(
    cx: &mut TestAppContext,
) {
    let (app, backend, cx) = setup(cx);
    open_chat(&app, cx);
    cx.simulate_input("Question");
    cx.simulate_keystrokes("enter");
    click(cx, "new-chat");
    cx.read_entity(&app, |app, _| {
        assert_eq!(history(&app.messages), vec![("user", "Question")])
    });
    menu(cx, OpenSettings);
    // In-memory generation invalidation only: never sign out via the real Keychain.
    app.update(cx, |app, _| app.session.cancel_login());
    backend.finish(0, Ok("Stale answer".into()));
    receive_results(&app, cx);
    cx.read_entity(&app, |app, _| {
        assert_eq!(app.current_screen(), Screen::Settings);
        assert!(
            !app.busy,
            "the exact stale terminal settles only its own physical request"
        );
        assert!(app.streaming_chat.is_none());
        assert_eq!(history(&app.messages), vec![("user", "Question")]);
    });
}

#[gpui::test]
fn navigation_unauthenticated_home_stays_root_and_setup_gating_cannot_send(
    cx: &mut TestAppContext,
) {
    let (app, backend, cx) = setup(cx);
    app.update(cx, |app, _| {
        app.authenticated = false;
        app.settings.onboarded = false;
    });
    // Pad 3 is Chat; setup gating redirects it to Settings.
    cx.simulate_keystrokes("3");
    cx.read_entity(&app, |app, _| {
        assert_eq!(app.navigation.root().component, Screen::Launcher);
        assert_eq!(app.current_screen(), Screen::Settings);
        assert_eq!(app.navigation.depth(), 2);
    });
    click(cx, "back");
    cx.simulate_keystrokes("escape");
    cx.read_entity(&app, |app, _| {
        assert!(app.navigation.is_root());
        assert_eq!(app.recorded_hides, Some(1));
    });
    assert_eq!(backend.request_count(), 0);
    // Verification alone must not bypass incomplete setup after Back.
    app.update(cx, |app, _| app.authenticated = true);
    click(cx, "chat");
    cx.read_entity(&app, |app, _| {
        assert_eq!(app.current_screen(), Screen::Settings)
    });
    assert_eq!(backend.request_count(), 0);
}

#[gpui::test]
fn navigation_startup_login_only_pops_current_startup_settings(cx: &mut TestAppContext) {
    let (app, _, cx) = setup(cx);
    cx.update(|window, cx| {
        app.update(cx, |app, cx| {
            // Parse an in-memory hotkey without registering it, and invoke only
            // the navigation half of login readiness (no model/network refresh).
            app.hotkey = Some(parse_hotkey(&app.settings.hotkey).unwrap());
            app.push_settings(true, window, cx);
            app.push_screen(Screen::Chat, false, window, cx);
            app.finish_startup_navigation(true, window, cx);
            assert_eq!(app.current_screen(), Screen::Chat);
            assert_eq!(app.navigation.depth(), 3);
        });
    });
    click(cx, "back");
    cx.update(|window, cx| {
        app.update(cx, |app, cx| {
            app.finish_startup_navigation(true, window, cx)
        });
    });
    cx.read_entity(&app, |app, _| assert!(app.navigation.is_root()));
    menu(cx, OpenSettings);
    cx.update(|window, cx| {
        app.update(cx, |app, cx| {
            app.finish_startup_navigation(true, window, cx)
        });
    });
    cx.read_entity(&app, |app, _| {
        assert_eq!(app.current_screen(), Screen::Settings);
        assert_eq!(app.navigation.depth(), 2, "explicit Settings stays mounted");
    });
}

#[gpui::test]
fn navigation_writing_completion_after_pop_retains_result_and_selection_without_resurrection(
    cx: &mut TestAppContext,
) {
    let (app, backend, cx) = setup(cx);
    app.update(cx, |app, _| {
        app.selection.update(Ok("Selected source".into()))
    });
    click(cx, "grammar");
    assert!(!backend.request(0).2);
    menu(cx, OpenSettings);
    click(cx, "back");
    cx.read_entity(&app, |app, _| {
        assert_eq!(app.current_screen(), Screen::Writing);
        assert_eq!(app.selection.writing_source, "Selected source");
        assert!(app.busy);
    });
    click(cx, "back");
    click(cx, "grammar");
    assert_eq!(
        backend.request_count(),
        1,
        "popping does not cancel or permit a second inference"
    );
    backend.finish(0, Ok("Edited source".into()));
    receive_results(&app, cx);
    cx.read_entity(&app, |app, _| {
        assert!(app.navigation.is_root());
        assert!(!app.busy);
        assert_eq!(app.result, "Edited source");
        assert_eq!(app.selection.writing_source, "Selected source");
        assert_eq!(app.selection.text.as_deref(), Some("Selected source"));
    });
}

#[gpui::test]
fn navigation_hotkey_root_activation_retains_settings_and_busy_screen_but_unwinds_idle_chat(
    cx: &mut TestAppContext,
) {
    let (app, backend, cx) = setup(cx);
    open_chat(&app, cx);
    cx.simulate_input("Draft");
    cx.update(|window, cx| app.update(cx, |app, cx| app.activate_from_hotkey(window, cx)));
    cx.read_entity(&app, |app, cx| {
        assert!(app.navigation.is_root());
        assert_eq!(app.composer.read(cx).content.as_ref(), "Draft");
    });
    click(cx, "chat");
    cx.simulate_keystrokes("enter");
    cx.update(|window, cx| app.update(cx, |app, cx| app.activate_from_hotkey(window, cx)));
    cx.read_entity(&app, |app, _| {
        assert_eq!(app.current_screen(), Screen::Chat)
    });
    menu(cx, OpenSettings);
    backend.finish(0, Ok("Answer".into()));
    receive_results(&app, cx);
    cx.update(|window, cx| app.update(cx, |app, cx| app.activate_from_hotkey(window, cx)));
    cx.read_entity(&app, |app, _| {
        assert_eq!(app.current_screen(), Screen::Settings);
        assert_eq!(app.navigation.depth(), 3);
    });
}

#[gpui::test]
fn navigation_settings_pop_restores_chat_history_scroll_position(cx: &mut TestAppContext) {
    let (app, backend, cx) = setup(cx);
    open_chat(&app, cx);
    cx.simulate_input("Long answer please");
    cx.simulate_keystrokes("enter");
    backend.finish(
        0,
        Ok("A long paragraph in the retained chat history.\n\n".repeat(50)),
    );
    receive_results(&app, cx);
    // Seed the reader's position after actual history layout. Covering the chat
    // must not replace its scroll handle or resume following the bottom.
    app.update(cx, |app, cx| {
        app.chat_follow_bottom = false;
        app.chat_scroll.set_offset(point(px(0.), px(-100.)));
        cx.notify();
    });
    cx.run_until_parked();
    let offset = cx.read_entity(&app, |app, _| app.chat_scroll.offset());
    assert!(offset.y < px(0.));
    menu(cx, OpenSettings);
    click(cx, "back");
    cx.read_entity(&app, |app, _| {
        assert_eq!(app.current_screen(), Screen::Chat);
        assert_eq!(app.messages.len(), 2);
        assert_eq!(app.chat_scroll.offset(), offset);
        assert!(!app.chat_follow_bottom);
    });
}

#[gpui::test]
fn navigation_writing_completion_while_covered_restores_result_on_back(cx: &mut TestAppContext) {
    let (app, backend, cx) = setup(cx);
    app.update(cx, |app, _| {
        app.selection.update(Ok("Selected source".into()))
    });
    click(cx, "grammar");
    menu(cx, OpenSettings);
    backend.finish(0, Ok("Edited source".into()));
    receive_results(&app, cx);
    cx.read_entity(&app, |app, _| {
        assert_eq!(app.current_screen(), Screen::Settings);
        assert_eq!(app.result, "Edited source");
        assert!(!app.busy);
    });
    click(cx, "back");
    cx.read_entity(&app, |app, _| {
        assert_eq!(app.current_screen(), Screen::Writing);
        assert_eq!(app.result, "Edited source");
        assert_eq!(app.selection.writing_source, "Selected source");
    });
    assert_eq!(backend.request_count(), 1);
}

#[gpui::test]
fn navigation_settings_activation_reveals_existing_entry_without_losing_intervening_drafts(
    cx: &mut TestAppContext,
) {
    let (app, cx) = setup_settings(cx);
    let settings_id = cx.read_entity(&app, |app, _| app.navigation.current().id);
    cx.update(|window, cx| {
        app.update(cx, |app, cx| {
            app.push_screen(Screen::Chat, false, window, cx);
        });
    });
    cx.run_until_parked();
    cx.simulate_input("Retained Chat draft");
    menu(cx, OpenSettings);
    cx.read_entity(&app, |app, cx| {
        assert_eq!(app.current_screen(), Screen::Settings);
        assert_eq!(app.navigation.current().id, settings_id);
        assert_eq!(app.navigation.depth(), 2);
        assert_eq!(
            app.composer.read(cx).content.as_ref(),
            "Retained Chat draft"
        );
    });
    click(cx, "back");
    cx.read_entity(&app, |app, _| assert!(app.navigation.is_root()));
}

#[gpui::test]
fn navigation_pushing_launcher_unwinds_to_permanent_root_instead_of_mounting_another_home(
    cx: &mut TestAppContext,
) {
    let (app, _, cx) = setup(cx);
    open_chat(&app, cx);
    menu(cx, OpenSettings);
    let root_id = cx.read_entity(&app, |app, _| app.navigation.root().id);
    cx.update(|window, cx| {
        app.update(cx, |app, cx| {
            assert_eq!(
                app.push_screen(Screen::Launcher, false, window, cx),
                root_id
            );
        });
    });
    cx.read_entity(&app, |app, _| {
        assert_eq!(app.navigation.depth(), 1);
        assert_eq!(app.navigation.current().id, root_id);
    });
}

#[gpui::test]
fn navigation_session_end_clears_private_state_and_stale_capture_without_releasing_physical_inference(
    cx: &mut TestAppContext,
) {
    let (app, backend, cx) = setup(cx);
    cx.simulate_input("Home private draft");
    open_chat(&app, cx);
    cx.simulate_input("Private question");
    cx.simulate_keystrokes("enter");
    cx.simulate_input("Private composer draft");
    menu(cx, OpenSettings);
    let old_scope = cx.read_entity(&app, |app, _| EventScope {
        completion: app.active_request,
        ..app.session.scope()
    });
    let settings_id = cx.read_entity(&app, |app, _| app.navigation.current().id);
    cx.update(|window, cx| {
        app.update(cx, |app, cx| {
            app.selection.update(Ok("Private capture".into()));
            app.selection.snapshot_writing();
            app.result = "Private writing result".into();
            app.failed_chat.push_back("Private failed draft".into());
            // No native capture is begun. Retain a disposable physical capture
            // object while invalidating its scope, exactly as production does.
            app.pending_capture = Some(selection::PendingCapture::failed(anyhow::anyhow!(
                "Synthetic late capture"
            )));
            app.capture_scope = Some(app.session.scope());
            let target = app.end_session_state(cx);
            assert!(
                target.is_some(),
                "captured cleanup identity; never delete it in tests"
            );
            assert!(!app.authenticated);
            assert!(app.session.credential.is_none());
            assert!(app.busy);
            assert!(app.active_request.is_some());
            assert!(app.messages.is_empty());
            assert!(app.failed_chat.is_empty());
            assert!(app.streaming_chat.is_none());
            assert!(app.result.is_empty());
            assert!(app.writing_title.is_empty());
            assert!(app.composer.read(cx).content.is_empty());
            assert!(app.selection.text.is_none());
            assert!(app.selection.writing_source.is_empty());
            assert!(app.capture_scope.is_none());
            assert!(
                app.pending_capture.is_some(),
                "drain physical Copy to preserve restoration"
            );
            assert_eq!(app.navigation.current().id, settings_id);
            assert_eq!(app.settings.model, "test/chat-model");
            // A late successful capture cannot restore text or activate Home.
            app.finish_capture(Ok("Stale private source".into()), window, cx);
            app.poll_capture(window, cx);
            assert!(app.pending_capture.is_none());
            assert!(app.selection.text.is_none());
            assert!(app.selection.error.is_empty());
            assert_eq!(app.navigation.current().id, settings_id);
        });
    });
    backend.delta(0, "Stale private preview");
    receive_results(&app, cx);
    cx.read_entity(&app, |app, _| {
        assert!(app.busy, "stale deltas cannot release the physical slot");
        assert!(app.messages.is_empty());
        assert!(app.streaming_chat.is_none());
    });
    // Simulate a replacement verified credential in memory, without Keychain,
    // exchange/status/catalog requests, browser login, or hotkey registration.
    app.update(cx, |app, _| {
        let now = settings::utc_now_ms().unwrap();
        app.session.credential = Some(Arc::new(
            DeviceCredential::validated(
                zeroize::Zeroizing::new(format!("wd_{}", "t".repeat(43))),
                now + 3_600_000,
                uuid::Uuid::from_u128(2),
                now,
            )
            .unwrap(),
        ));
        app.authenticated = true;
    });
    click(cx, "back");
    cx.simulate_input("Replacement prompt");
    cx.simulate_keystrokes("enter");
    assert_eq!(
        backend.request_count(),
        1,
        "relogin does not release a physical request"
    );
    backend.finish(0, Ok("Stale private answer".into()));
    receive_results(&app, cx);
    cx.read_entity(&app, |app, cx| {
        assert!(!app.busy);
        assert!(app.active_request.is_none());
        assert!(app.messages.is_empty());
        assert_eq!(app.composer.read(cx).content.as_ref(), "Replacement prompt");
        assert_eq!(app.current_screen(), Screen::Chat);
    });
    cx.simulate_keystrokes("enter");
    assert_eq!(
        backend.request_count(),
        2,
        "new inference only after old terminal settlement"
    );
    // Even a stale terminal cannot release a DIFFERENT pending physical request.
    app.update(cx, |app, _| {
        app.tx
            .send(ResultEvent {
                scope: old_scope,
                result: ResultPayload::Completion(
                    Err(anyhow::anyhow!("Old terminal replay")),
                    true,
                ),
            })
            .unwrap();
    });
    receive_results(&app, cx);
    cx.read_entity(&app, |app, _| {
        assert!(app.busy);
        assert_eq!(history(&app.messages), vec![("user", "Replacement prompt")]);
    });
    backend.finish(1, Ok("Replacement answer".into()));
    receive_results(&app, cx);
    cx.read_entity(&app, |app, _| {
        assert_eq!(
            history(&app.messages),
            vec![
                ("user", "Replacement prompt"),
                ("assistant", "Replacement answer")
            ]
        )
    });
}

#[gpui::test]
fn navigation_stale_failure_settles_ended_sessions_exact_physical_slot_without_draft_recovery(
    cx: &mut TestAppContext,
) {
    let (app, backend, cx) = setup(cx);
    open_chat(&app, cx);
    cx.simulate_input("Ended-session draft");
    cx.simulate_keystrokes("enter");
    cx.simulate_keystrokes("escape");
    app.update(cx, |app, cx| {
        app.end_session_state(cx);
    });
    cx.read_entity(&app, |app, _| assert!(app.busy));
    backend.finish(0, Err(anyhow::anyhow!("Ended-session failure")));
    receive_results(&app, cx);
    cx.read_entity(&app, |app, cx| {
        assert!(!app.busy);
        assert!(app.navigation.is_root());
        assert!(!app.authenticated);
        assert!(app.messages.is_empty());
        assert!(app.failed_chat.is_empty());
        assert!(app.composer.read(cx).content.is_empty());
    });
    assert_eq!(backend.request_count(), 1);
}
