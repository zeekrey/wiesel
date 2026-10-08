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
        page: Page::Launcher,
        hotkey_input: cx.new(|cx| TextInput::new("Shortcut", false, cx)),
        model_picker: cx.new(|cx| model_picker::ModelPicker::new(settings.model.clone(), cx)),
        grammar_input: cx.new(|cx| TextInput::new("Grammar", false, cx)),
        improve_input: cx.new(|cx| TextInput::new("Improve", false, cx)),
        settings,
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
        launcher_input: cx.new(TextInput::launcher),
        focus: cx.focus_handle(),
        recording: false,
        authenticated: true,
        accessibility_granted: false,
        permission_checked_at: Instant::now(),
        busy: false,
        notifications: Notifications::default(),
        selection: selection::Capture::default(),
        pending_capture: None,
        failed_chat: Default::default(),
        result: String::new(),
        writing_title: String::new(),
        messages: vec![],
        tx,
        rx,
        chat_scroll: ScrollHandle::new(),
        chat_follow_bottom: true,
        streaming_chat: None,
        completion_backend: CompletionBackend::Controlled(backend),
    }
}

fn setup(cx: &mut TestAppContext) -> (Entity<Wiesel>, ControlledBackend, &mut VisualTestContext) {
    // GPUI's default TestPlatform uses NoopTextSystem, not AppKit/a GPU. It
    // supports layout and input without a display, installed fonts, or OS AX.
    cx.update(|cx| cx.set_reduce_motion(true));
    let backend = ControlledBackend::default();
    let (app, cx) = cx.add_window_view(|window, cx| {
        let app = ephemeral_app(backend.clone(), cx);
        window.focus(&app.launcher_input.focus_handle(cx), cx);
        app
    });
    cx.simulate_resize(size(px(800.), px(450.)));
    (app, backend, cx)
}

fn click(cx: &mut VisualTestContext, selector: &'static str) {
    let bounds = cx.debug_bounds(selector).expect("rendered control");
    cx.simulate_click(bounds.center(), Modifiers::none());
}

fn open_chat(app: &Entity<Wiesel>, cx: &mut VisualTestContext) {
    click(cx, "chat");
    cx.read_entity(app, |app, cx| {
        assert!(app.page == Page::Chat);
        assert!(app.composer.read(cx).content.is_empty());
    });
}

fn receive_results(app: &Entity<Wiesel>, cx: &mut VisualTestContext) {
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
    backend.finish(0, Ok("Stale completion".into()));
    receive_results(&app, cx);
    cx.read_entity(&app, |app, _| {
        assert!(app.busy, "stale completion must not mutate request state");
        assert_eq!(history(&app.messages), vec![("user", "Question")]);
        assert!(app.streaming_chat.as_ref().unwrap().content.is_empty());
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
        assert!(app.busy);
        assert_eq!(history(&app.messages), vec![("user", "Question")]);
        assert!(app.streaming_chat.as_ref().unwrap().content.is_empty());
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
            app.page = Page::Setup;
            window.focus(&app.login_focus, cx);
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
        assert!(app.hotkey_input.read(cx).content.is_empty());
    });
}

#[gpui::test]
fn settings_modified_tab_does_not_traverse(cx: &mut TestAppContext) {
    let (app, cx) = setup_settings(cx);
    cx.simulate_keystrokes("cmd-tab ctrl-tab alt-tab cmd-shift-tab");
    cx.update(|window, cx| assert!(app.read(cx).login_focus.is_focused(window)));
}

#[gpui::test]
fn settings_tab_traversal_preserves_text_input_contents(cx: &mut TestAppContext) {
    let (app, cx) = setup_settings(cx);
    cx.update(|window, cx| {
        window.focus(&app.read(cx).grammar_input.focus_handle(cx), cx);
    });
    cx.run_until_parked();
    cx.simulate_input("private draft");
    cx.simulate_keystrokes("tab");
    cx.read_entity(&app, |app, cx| {
        assert_eq!(app.grammar_input.read(cx).content.as_ref(), "private draft")
    });
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
