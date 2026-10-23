//! In-process chat tests: no settings, Keychain, clipboard, hotkeys, or network.
use super::*;
use gpui::{KeyUpEvent, Keystroke, TestAppContext, VisualTestContext};
pub(super) use test_support::*;

/// Make a surface the key window, as clicking into it would.
fn activate(app: &Entity<Wiesel>, cx: &mut VisualTestContext, surface: Surface) {
    on_surface(app, cx, surface).update(|window, _| window.activate_window());
    cx.run_until_parked();
}

/// The global shortcut brings the launcher back.
fn show_launcher(app: &Entity<Wiesel>, cx: &mut VisualTestContext) {
    cx.update(|window, cx| app.update(cx, |app, cx| app.show_hud(window, cx)));
    cx.run_until_parked();
}

/// Whether `focus` holds focus inside `surface`'s window.
fn focused_in(
    app: &Entity<Wiesel>,
    cx: &mut VisualTestContext,
    surface: Surface,
    focus: impl FnOnce(&Wiesel, &App) -> FocusHandle,
) -> bool {
    on_surface(app, cx, surface).update(|window, cx| focus(app.read(cx), cx).is_focused(window))
}

fn open_chat(app: &Entity<Wiesel>, cx: &mut VisualTestContext) {
    click(cx, "chat");
    cx.read_entity(app, |app, cx| {
        assert!(app.is_open(Surface::Chat));
        assert!(app.composer.read(cx).content.is_empty());
    });
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
    input(cx, "Hello Wiesel");
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
    input(cx, "First question");
    keys(cx, "enter");
    backend.finish(0, Ok("First answer".into()));
    receive_results(&app, cx);

    input(cx, "Follow up");
    keys(cx, "enter");
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
    input(cx, "Pending question");
    keys(cx, "enter enter");
    input(cx, "New draft");
    click(cx, "send");
    keys(cx, "enter");
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
    input(cx, "Stream please");
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
    input(cx, "Failed draft");
    keys(cx, "enter");
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
    input(cx, "First question");
    keys(cx, "enter");
    backend.finish(0, Ok("First answer".into()));
    receive_results(&app, cx);
    input(cx, "Failed question");
    click(cx, "send");
    click(cx, "chat-composer");
    input(cx, "Newer draft");
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
    input(cx, "Question");
    keys(cx, "enter");
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
    input(cx, "Unsent draft");
    click(cx, "send");
    keys(cx, "enter");
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
    input(cx, "Question");
    keys(cx, "enter");
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
    input(cx, "Unsent draft");
    keys(cx, "enter");
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
    let focused = |cx: &mut VisualTestContext, focus: fn(&Wiesel, &App) -> FocusHandle| {
        assert!(focused_in(&app, cx, Surface::Settings, focus));
    };
    let tier =
        |index: usize| move |app: &Wiesel, cx: &App| app.tier_pickers[index].focus_handle(cx);
    // Account, then the three tier pickers, then diagnostics.
    for index in 0..3 {
        keys(cx, "tab");
        assert!(focused_in(&app, cx, Surface::Settings, tier(index)));
    }
    keys(cx, "tab");
    focused(cx, |app, _| app.diagnostics_open_focus.clone());
    keys(cx, "tab");
    focused(cx, |app, _| app.diagnostics_clear_focus.clone());
    keys(cx, "shift-tab");
    focused(cx, |app, _| app.diagnostics_open_focus.clone());
    for index in (0..3).rev() {
        keys(cx, "shift-tab");
        assert!(focused_in(&app, cx, Surface::Settings, tier(index)));
    }
    keys(cx, "shift-tab");
    focused(cx, |app, _| app.login_focus.clone());
}

#[gpui::test]
fn settings_tab_does_not_traverse_while_recording_a_shortcut(cx: &mut TestAppContext) {
    let (app, cx) = setup_settings(cx);
    on_surface(&app, cx, Surface::Settings).update(|window, cx| {
        app.update(cx, |app, cx| {
            // As the Record button does: the window root captures the shortcut.
            app.recording = true;
            window.focus(&app.surface_focus(Surface::Settings), cx);
            cx.notify();
        });
    });
    cx.run_until_parked();
    // These modifier-free shortcuts are rejected by recording without touching
    // hotkey registration/settings persistence. They must not become traversal.
    keys(cx, "tab shift-tab");
    assert!(focused_in(&app, cx, Surface::Settings, |app, _| {
        app.surface_focus(Surface::Settings)
    }));
    cx.read_entity(&app, |app, _| {
        assert!(app.recording);
        assert_eq!(app.settings.hotkey, Settings::default().hotkey);
    });
}

#[gpui::test]
fn settings_lit_record_key_cancels_recording(cx: &mut TestAppContext) {
    let (app, cx) = setup_settings(cx);
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
    keys(cx, "cmd-tab ctrl-tab alt-tab cmd-shift-tab");
    assert!(focused_in(&app, cx, Surface::Settings, |app, _| app
        .login_focus
        .clone()));
}

fn assert_diagnostic_key_press_activates_once(
    cx: &mut TestAppContext,
    control: DiagnosticControl,
    key: &str,
) {
    let (app, cx) = setup_settings(cx);
    let mut settings = on_surface(&app, cx, Surface::Settings);
    settings.update(|window, cx| {
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
    settings.simulate_event(KeyDownEvent {
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
    settings.simulate_event(KeyUpEvent { keystroke });
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
fn settings_window_leaves_chat_draft_selection_and_focus_intact(cx: &mut TestAppContext) {
    let (app, _, cx) = setup(cx);
    open_chat(&app, cx);
    input(cx, "Chat draft");
    keys(cx, "cmd-a");
    menu(cx, OpenSettings);
    menu(cx, OpenSettings);
    cx.read_entity(&app, |app, _| {
        assert_eq!(app.current_screen(), Screen::Settings);
        assert_eq!(
            app.navigation.depth(),
            1,
            "duplicate Settings must not mount"
        );
    });
    assert_eq!(windows(cx).len(), 3, "launcher, Chat and one Settings");
    keys(cx, "escape");
    cx.read_entity(&app, |app, cx| {
        assert!(!app.is_open(Surface::Settings));
        assert!(app.is_open(Surface::Chat));
        assert_eq!(app.composer.read(cx).content.as_ref(), "Chat draft");
    });
    assert!(focused_in(&app, cx, Surface::Chat, |app, cx| app
        .composer
        .focus_handle(cx)));
    // Selection survives the other window: typing replaces, rather than appends.
    activate(&app, cx, Surface::Chat);
    input(cx, "Replacement");
    cx.read_entity(&app, |app, cx| {
        assert_eq!(app.composer.read(cx).content.as_ref(), "Replacement")
    });
    keys(cx, "escape");
    cx.read_entity(&app, |app, _| assert!(!app.is_open(Surface::Chat)));
    // The launcher keeps its own focus throughout.
    cx.update(|window, cx| assert!(app.read(cx).focus.is_focused(window)));
}

#[gpui::test]
fn escape_closes_one_window_at_a_time_and_bare_launcher_escape_records_hide(
    cx: &mut TestAppContext,
) {
    let (app, _, cx) = setup(cx);
    open_chat(&app, cx);
    input(cx, "Retained draft");
    menu(cx, OpenSettings);
    keys(cx, "escape");
    cx.read_entity(&app, |app, _| {
        assert!(!app.is_open(Surface::Settings));
        assert!(app.is_open(Surface::Chat));
        assert_eq!(app.recorded_hides, Some(0));
    });
    activate(&app, cx, Surface::Chat);
    keys(cx, "escape");
    cx.read_entity(&app, |app, _| {
        assert!(!app.is_open(Surface::Chat));
        assert_eq!(app.recorded_hides, Some(0));
    });
    // Only the launcher is left: Esc hands focus back every time.
    keys(cx, "escape escape");
    cx.read_entity(&app, |app, cx| {
        assert!(Surface::ALL.iter().all(|surface| !app.is_open(*surface)));
        assert_eq!(
            app.recorded_hides,
            Some(2),
            "hide intent, not native hiding"
        );
        assert_eq!(app.composer.read(cx).content.as_ref(), "Retained draft");
    });
}

#[gpui::test]
fn settings_recording_escape_takes_precedence_over_closing(cx: &mut TestAppContext) {
    let (app, cx) = setup_settings(cx);
    click(cx, "record");
    cx.read_entity(&app, |app, _| assert!(app.recording));
    keys(cx, "escape");
    cx.read_entity(&app, |app, _| {
        assert!(!app.recording);
        assert!(app.is_open(Surface::Settings));
        assert_eq!(app.current_screen(), Screen::Settings);
    });
    keys(cx, "escape");
    cx.read_entity(&app, |app, _| assert!(!app.is_open(Surface::Settings)));
}

#[gpui::test]
fn chat_completion_behind_settings_preserves_messages_and_new_draft(cx: &mut TestAppContext) {
    let (app, backend, cx) = setup(cx);
    open_chat(&app, cx);
    input(cx, "Question");
    keys(cx, "enter");
    input(cx, "New draft");
    menu(cx, OpenSettings);
    backend.delta(0, "Preview");
    receive_results(&app, cx);
    cx.read_entity(&app, |app, _| {
        assert!(app.busy);
        assert!(app.is_open(Surface::Settings));
        assert_eq!(app.streaming_chat.as_ref().unwrap().content, "Preview");
    });
    backend.finish(0, Ok("Completed answer".into()));
    receive_results(&app, cx);
    cx.read_entity(&app, |app, cx| {
        assert!(!app.busy);
        assert!(app.is_open(Surface::Settings));
        assert!(app.is_open(Surface::Chat));
        assert_eq!(
            history(&app.messages),
            vec![("user", "Question"), ("assistant", "Completed answer")]
        );
        assert_eq!(app.composer.read(cx).content.as_ref(), "New draft");
    });
    keys(cx, "escape");
    cx.read_entity(&app, |app, _| assert!(!app.is_open(Surface::Settings)));
    assert!(focused_in(&app, cx, Surface::Chat, |app, cx| app
        .composer
        .focus_handle(cx)));
}

#[gpui::test]
fn closed_chat_request_remains_busy_and_completion_does_not_reopen_chat(cx: &mut TestAppContext) {
    let (app, backend, cx) = setup(cx);
    open_chat(&app, cx);
    input(cx, "Question");
    keys(cx, "enter");
    input(cx, "New draft");
    keys(cx, "escape");
    // A second inference cannot be started from the launcher while Chat is closed.
    show_launcher(&app, cx);
    keys(cx, "1");
    cx.read_entity(&app, |app, _| {
        assert!(app.busy);
        assert!(!app.is_open(Surface::Chat));
        assert!(!app.is_open(Surface::Writing));
    });
    assert_eq!(backend.request_count(), 1);
    click(cx, "chat");
    click(cx, "new-chat");
    keys(cx, "enter escape");
    assert_eq!(backend.request_count(), 1);
    backend.delta(0, "Late preview");
    backend.finish(0, Ok("Late answer".into()));
    receive_results(&app, cx);
    cx.read_entity(&app, |app, cx| {
        assert!(!app.is_open(Surface::Chat));
        assert!(!app.busy);
        assert_eq!(
            history(&app.messages),
            vec![("user", "Question"), ("assistant", "Late answer")]
        );
        assert_eq!(app.composer.read(cx).content.as_ref(), "New draft");
    });
    show_launcher(&app, cx);
    click(cx, "chat");
    cx.read_entity(&app, |app, _| assert_eq!(app.messages.len(), 2));
}

#[gpui::test]
fn chat_failure_behind_settings_retains_recoverable_draft(cx: &mut TestAppContext) {
    let (app, backend, cx) = setup(cx);
    open_chat(&app, cx);
    input(cx, "Failed question");
    keys(cx, "enter");
    input(cx, "New draft");
    menu(cx, OpenSettings);
    backend.finish(0, Err(anyhow::anyhow!("Controlled failure")));
    receive_results(&app, cx);
    cx.read_entity(&app, |app, cx| {
        assert!(app.is_open(Surface::Settings));
        assert!(!app.busy);
        assert!(app.messages.is_empty());
        assert_eq!(app.failed_chat, vec!["Failed question".to_owned()]);
        assert_eq!(app.composer.read(cx).content.as_ref(), "New draft");
    });
    keys(cx, "escape");
    cx.read_entity(&app, |app, _| {
        assert!(!app.is_open(Surface::Settings));
        assert!(app.is_open(Surface::Chat));
    });
    assert_eq!(backend.request_count(), 1, "no retry on failure");
}

#[gpui::test]
fn chat_failure_after_closing_restores_draft_without_reopening_chat(cx: &mut TestAppContext) {
    let (app, backend, cx) = setup(cx);
    open_chat(&app, cx);
    input(cx, "Recover me");
    keys(cx, "enter escape");
    backend.finish(0, Err(anyhow::anyhow!("Controlled failure")));
    receive_results(&app, cx);
    cx.read_entity(&app, |app, cx| {
        assert!(!app.is_open(Surface::Chat));
        assert!(!app.busy);
        assert!(app.messages.is_empty());
        assert_eq!(app.composer.read(cx).content.as_ref(), "Recover me");
    });
    show_launcher(&app, cx);
    click(cx, "chat");
    keys(cx, "enter");
    assert_eq!(backend.request_count(), 2, "only explicit Enter resubmits");
}

#[gpui::test]
fn replayed_old_request_does_not_mutate_newer_chat_request(cx: &mut TestAppContext) {
    let (app, backend, cx) = setup(cx);
    open_chat(&app, cx);
    input(cx, "First");
    keys(cx, "enter");
    let old_scope = cx.read_entity(&app, |app, _| EventScope {
        completion: app.active_request,
        ..app.session.scope()
    });
    backend.finish(0, Ok("First answer".into()));
    receive_results(&app, cx);
    click(cx, "new-chat");
    input(cx, "Second");
    keys(cx, "enter");
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
fn busy_new_chat_is_blocked_and_session_invalidation_rejects_completion_behind_settings(
    cx: &mut TestAppContext,
) {
    let (app, backend, cx) = setup(cx);
    open_chat(&app, cx);
    input(cx, "Question");
    keys(cx, "enter");
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
        assert!(app.is_open(Surface::Settings));
        assert!(app.is_open(Surface::Chat));
        assert!(
            !app.busy,
            "the exact stale terminal settles only its own physical request"
        );
        assert!(app.streaming_chat.is_none());
        assert_eq!(history(&app.messages), vec![("user", "Question")]);
    });
}

#[gpui::test]
fn unauthenticated_chat_tile_reveals_sign_in_and_setup_gating_cannot_send(cx: &mut TestAppContext) {
    let (app, backend, cx) = setup(cx);
    app.update(cx, |app, _| {
        app.authenticated = false;
        app.settings.onboarded = false;
    });
    // Tile 3 is Chat; setup gating reveals sign-in in Settings instead.
    keys(cx, "3");
    cx.read_entity(&app, |app, _| {
        assert!(app.is_open(Surface::Settings));
        assert!(!app.is_open(Surface::Chat));
        assert_eq!(app.current_screen(), Screen::Settings);
        assert_eq!(app.navigation.depth(), 1);
    });
    assert!(focused_in(&app, cx, Surface::Settings, |app, _| app
        .login_focus
        .clone()));
    // Esc closes Settings, then hides from the bare launcher.
    keys(cx, "escape");
    keys(cx, "escape");
    cx.read_entity(&app, |app, _| {
        assert!(!app.is_open(Surface::Settings));
        assert_eq!(app.recorded_hides, Some(1));
    });
    assert_eq!(backend.request_count(), 0);
    // Verification alone must not bypass incomplete setup.
    app.update(cx, |app, _| app.authenticated = true);
    show_launcher(&app, cx);
    click(cx, "chat");
    cx.read_entity(&app, |app, _| {
        assert!(app.is_open(Surface::Settings));
        assert!(!app.is_open(Surface::Chat));
    });
    assert_eq!(backend.request_count(), 0);
}

#[gpui::test]
fn startup_login_closes_only_untouched_startup_settings(cx: &mut TestAppContext) {
    let (app, _, cx) = setup(cx);
    // Invoke only the navigation half of login readiness (no model/network refresh).
    let login_ready = |cx: &mut VisualTestContext| {
        cx.update(|window, cx| {
            app.update(cx, |app, cx| {
                app.finish_startup_navigation(true, window, cx)
            });
        });
        cx.run_until_parked();
    };
    cx.update(|window, cx| {
        app.update(cx, |app, cx| {
            // Parse an in-memory hotkey without registering it.
            app.hotkey = Some(parse_hotkey(&app.settings.hotkey).unwrap());
            app.push_settings(true, window, cx);
        });
    });
    cx.run_until_parked();
    // The user moved on inside setup Settings: login leaves it open.
    menu(cx, AdjustQuickActions);
    login_ready(cx);
    cx.read_entity(&app, |app, _| {
        assert!(app.is_open(Surface::Settings));
        assert_eq!(app.current_screen(), Screen::ActionManager);
        assert_eq!(app.navigation.depth(), 2);
    });
    click(cx, "back");
    login_ready(cx);
    cx.read_entity(&app, |app, _| {
        assert!(!app.is_open(Surface::Settings));
        assert!(!app.settings_startup);
        assert!(app.hud_visible, "the launcher replaces setup Settings");
    });
    menu(cx, OpenSettings);
    login_ready(cx);
    cx.read_entity(&app, |app, _| {
        assert!(
            app.is_open(Surface::Settings),
            "explicit Settings stays open"
        );
        assert_eq!(app.current_screen(), Screen::Settings);
        assert_eq!(app.navigation.depth(), 1);
    });
}

#[gpui::test]
fn writing_completion_after_closing_retains_result_and_selection_without_reopening(
    cx: &mut TestAppContext,
) {
    let (app, backend, cx) = setup(cx);
    app.update(cx, |app, _| {
        app.selection.update(Ok("Selected source".into()))
    });
    click(cx, "grammar");
    assert!(!backend.request(0).2);
    menu(cx, OpenSettings);
    keys(cx, "escape");
    cx.read_entity(&app, |app, _| {
        assert!(!app.is_open(Surface::Settings));
        assert!(app.is_open(Surface::Writing));
        assert_eq!(app.selection.writing_source, "Selected source");
        assert!(app.busy);
    });
    activate(&app, cx, Surface::Writing);
    keys(cx, "escape");
    show_launcher(&app, cx);
    click(cx, "grammar");
    assert_eq!(
        backend.request_count(),
        1,
        "closing does not cancel or permit a second inference"
    );
    backend.finish(0, Ok("Edited source".into()));
    receive_results(&app, cx);
    cx.read_entity(&app, |app, _| {
        assert!(!app.is_open(Surface::Writing));
        assert!(!app.busy);
        assert_eq!(app.result, "Edited source");
        assert_eq!(app.selection.writing_source, "Selected source");
        assert_eq!(app.selection.text.as_deref(), Some("Selected source"));
    });
}

#[gpui::test]
fn hotkey_activation_shows_launcher_and_retains_every_window(cx: &mut TestAppContext) {
    let (app, backend, cx) = setup(cx);
    let hotkey = |cx: &mut VisualTestContext| {
        cx.update(|window, cx| app.update(cx, |app, cx| app.activate_from_hotkey(window, cx)));
        cx.run_until_parked();
    };
    open_chat(&app, cx);
    input(cx, "Draft");
    hotkey(cx);
    cx.read_entity(&app, |app, cx| {
        assert!(app.hud_visible);
        assert!(app.is_open(Surface::Chat));
        assert_eq!(app.composer.read(cx).content.as_ref(), "Draft");
    });
    activate(&app, cx, Surface::Chat);
    keys(cx, "enter");
    hotkey(cx);
    cx.read_entity(&app, |app, _| {
        assert!(app.busy);
        assert!(app.is_open(Surface::Chat));
    });
    menu(cx, AdjustQuickActions);
    backend.finish(0, Ok("Answer".into()));
    receive_results(&app, cx);
    hotkey(cx);
    cx.read_entity(&app, |app, _| {
        assert!(app.hud_visible);
        assert!(app.is_open(Surface::Chat));
        assert_eq!(app.messages.len(), 2);
        assert_eq!(app.current_screen(), Screen::ActionManager);
        assert_eq!(app.navigation.depth(), 2);
    });
}

#[gpui::test]
fn settings_window_leaves_chat_history_scroll_position(cx: &mut TestAppContext) {
    let (app, backend, cx) = setup(cx);
    open_chat(&app, cx);
    input(cx, "Long answer please");
    keys(cx, "enter");
    backend.finish(
        0,
        Ok("A long paragraph in the retained chat history.\n\n".repeat(50)),
    );
    receive_results(&app, cx);
    // Seed the reader's position after actual history layout. Opening Settings must
    // not replace the chat's scroll handle or resume following the bottom.
    app.update(cx, |app, cx| {
        app.chat_follow_bottom = false;
        app.chat_scroll.set_offset(point(px(0.), px(-100.)));
        cx.notify();
    });
    cx.run_until_parked();
    let offset = cx.read_entity(&app, |app, _| app.chat_scroll.offset());
    assert!(offset.y < px(0.));
    menu(cx, OpenSettings);
    keys(cx, "escape");
    cx.read_entity(&app, |app, _| {
        assert!(!app.is_open(Surface::Settings));
        assert!(app.is_open(Surface::Chat));
        assert_eq!(app.messages.len(), 2);
        assert_eq!(app.chat_scroll.offset(), offset);
        assert!(!app.chat_follow_bottom);
    });
}

#[gpui::test]
fn writing_completion_behind_settings_keeps_result_in_writing_window(cx: &mut TestAppContext) {
    let (app, backend, cx) = setup(cx);
    app.update(cx, |app, _| {
        app.selection.update(Ok("Selected source".into()))
    });
    click(cx, "grammar");
    menu(cx, OpenSettings);
    backend.finish(0, Ok("Edited source".into()));
    receive_results(&app, cx);
    cx.read_entity(&app, |app, _| {
        assert!(app.is_open(Surface::Settings));
        assert_eq!(app.result, "Edited source");
        assert!(!app.busy);
    });
    keys(cx, "escape");
    cx.read_entity(&app, |app, _| {
        assert!(!app.is_open(Surface::Settings));
        assert!(app.is_open(Surface::Writing));
        assert_eq!(app.result, "Edited source");
        assert_eq!(app.selection.writing_source, "Selected source");
    });
    assert!(
        on_surface(&app, cx, Surface::Writing)
            .debug_bounds("copy-result")
            .is_some()
    );
    assert_eq!(backend.request_count(), 1);
}

#[gpui::test]
fn settings_activation_reveals_existing_window_without_losing_chat_draft(cx: &mut TestAppContext) {
    let (app, cx) = setup_settings(cx);
    let (settings_window, settings_id) = cx.read_entity(&app, |app, _| {
        (
            app.surface_window(Surface::Settings),
            app.navigation.current().id,
        )
    });
    assert!(settings_window.is_some());
    show_launcher(&app, cx);
    open_chat(&app, cx);
    input(cx, "Retained Chat draft");
    menu(cx, OpenSettings);
    cx.read_entity(&app, |app, cx| {
        assert_eq!(app.surface_window(Surface::Settings), settings_window);
        assert_eq!(app.current_screen(), Screen::Settings);
        assert_eq!(app.navigation.current().id, settings_id);
        assert_eq!(app.navigation.depth(), 1);
        assert_eq!(
            app.composer.read(cx).content.as_ref(),
            "Retained Chat draft"
        );
    });
    assert_eq!(
        Some(in_key_window(cx, |window, _| window.window_handle())),
        settings_window,
        "Settings comes forward"
    );
    keys(cx, "escape");
    cx.read_entity(&app, |app, cx| {
        assert!(!app.is_open(Surface::Settings));
        assert!(app.is_open(Surface::Chat));
        assert_eq!(
            app.composer.read(cx).content.as_ref(),
            "Retained Chat draft"
        );
    });
}

#[gpui::test]
fn session_end_clears_private_state_and_stale_capture_without_releasing_physical_inference(
    cx: &mut TestAppContext,
) {
    let (app, backend, cx) = setup(cx);
    open_chat(&app, cx);
    input(cx, "Private question");
    keys(cx, "enter");
    input(cx, "Private composer draft");
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
            assert!(app.is_open(Surface::Settings) && app.is_open(Surface::Chat));
            assert_eq!(app.navigation.current().id, settings_id);
            assert_eq!(app.settings.model, "test/chat-model");
            // A late successful capture cannot restore text or show the launcher.
            app.finish_capture(Ok("Stale private source".into()), None, window, cx);
            app.poll_capture(window, cx);
            assert!(app.pending_capture.is_none());
            assert!(app.selection.text.is_none());
            assert!(app.selection.error.is_empty());
            assert!(!app.hud_visible);
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
    keys(cx, "escape");
    activate(&app, cx, Surface::Chat);
    input(cx, "Replacement prompt");
    keys(cx, "enter");
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
        assert!(app.is_open(Surface::Chat));
    });
    keys(cx, "enter");
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
fn stale_failure_settles_ended_sessions_exact_physical_slot_without_draft_recovery(
    cx: &mut TestAppContext,
) {
    let (app, backend, cx) = setup(cx);
    open_chat(&app, cx);
    input(cx, "Ended-session draft");
    keys(cx, "enter");
    keys(cx, "escape");
    app.update(cx, |app, cx| {
        app.end_session_state(cx);
    });
    cx.read_entity(&app, |app, _| assert!(app.busy));
    backend.finish(0, Err(anyhow::anyhow!("Ended-session failure")));
    receive_results(&app, cx);
    cx.read_entity(&app, |app, cx| {
        assert!(!app.busy);
        assert!(!app.is_open(Surface::Chat));
        assert!(!app.authenticated);
        assert!(app.messages.is_empty());
        assert!(app.failed_chat.is_empty());
        assert!(app.composer.read(cx).content.is_empty());
    });
    assert_eq!(backend.request_count(), 1);
}

fn use_sharp(app: &mut Wiesel, model: ModelChoice) {
    app.settings.models.sharp = Some(model);
}

#[gpui::test]
fn ollama_sharp_tier_serves_chat_without_sign_in(cx: &mut TestAppContext) {
    let (app, backend, cx) = setup(cx);
    app.update(cx, |app, _| {
        app.authenticated = false;
        app.session.credential = None;
        app.settings.onboarded = false;
        use_sharp(app, ModelChoice::ollama("qwen3.5:0.8b"));
    });
    open_chat(&app, cx);
    input(cx, "Hello local");
    click(cx, "send");
    assert_eq!(backend.request_count(), 1);
    assert!(backend.local(0));
    assert_eq!(backend.request(0).0, "qwen3.5:0.8b");
    backend.delta(0, "Hi");
    backend.finish(0, Ok("Hi there".into()));
    receive_results(&app, cx);
    cx.read_entity(&app, |app, _| {
        assert_eq!(
            history(&app.messages),
            vec![("user", "Hello local"), ("assistant", "Hi there")]
        );
        assert!(!app.busy);
    });
}

#[gpui::test]
fn each_model_routes_to_its_own_provider(cx: &mut TestAppContext) {
    let (app, backend, cx) = setup(cx);
    app.update(cx, |app, cx| {
        use_sharp(app, ModelChoice::ollama("llama3:8b"));
        app.model_picker.update(cx, |picker, cx| {
            picker.finish_load(Ok(vec!["test/pinned".into()]), cx)
        });
    });
    // Built-in actions follow the Sharp tier, here a local model.
    open_chat(&app, cx);
    input(cx, "Local");
    click(cx, "send");
    assert!(backend.local(0));
    assert_eq!(backend.request(0).0, "llama3:8b");
    backend.finish(0, Ok("Done".into()));
    receive_results(&app, cx);

    // A custom action pinned to a Wiesel model still uses the gateway.
    let action = quick_actions::CustomQuickAction::new(quick_actions::CustomActionFields {
        name: "Pinned".into(),
        icon: "wand".into(),
        model_override: Some("test/pinned".into()),
        instructions: "Answer briefly.".into(),
        paste_result: false,
    })
    .unwrap();
    cx.update(|window, cx| {
        app.update(cx, |app, cx| app.run_custom_action(action, window, cx));
    });
    assert!(!backend.local(1));
    assert_eq!(backend.request(1).0, "test/pinned");
    backend.finish(1, Ok("Done".into()));
    receive_results(&app, cx);

    // Clearing the tier returns the default to the Wiesel catalog model.
    app.update(cx, |app, cx| {
        app.assign_tier(Tier::Sharp, None, cx);
        app.new_chat(cx);
    });
    cx.read_entity(&app, |app, _| assert!(app.settings.models.sharp.is_none()));
    input(cx, "Cloud");
    click(cx, "send");
    assert!(!backend.local(2));
    assert_eq!(backend.request(2).0, "test/chat-model");
}

#[gpui::test]
fn wiesel_tier_requires_sign_in(cx: &mut TestAppContext) {
    let (app, backend, cx) = setup(cx);
    open_chat(&app, cx);
    app.update(cx, |app, _| {
        app.authenticated = false;
        use_sharp(app, ModelChoice::wiesel("test/chat-model"));
    });
    input(cx, "Hello");
    click(cx, "send");
    assert_eq!(backend.request_count(), 0);
    cx.read_entity(&app, |app, cx| {
        assert_eq!(app.composer.read(cx).content.as_ref(), "Hello");
    });
}

#[gpui::test]
fn ollama_failures_use_local_copy(cx: &mut TestAppContext) {
    let (app, backend, cx) = setup(cx);
    app.update(cx, |app, _| use_sharp(app, ModelChoice::ollama("m")));
    open_chat(&app, cx);
    input(cx, "Hello");
    click(cx, "send");
    backend.finish(0, Err(ollama::OllamaError("Ollama is not running.").into()));
    receive_results(&app, cx);
    cx.read_entity(&app, |app, cx| {
        assert!(app.messages.is_empty());
        assert_eq!(app.composer.read(cx).content.as_ref(), "Hello");
        // The failure triggers a re-probe so Settings reflects the new state.
        assert!(app.ollama_checking);
    });
}

#[gpui::test]
fn catalogs_fill_every_tier_picker_and_picks_persist(cx: &mut TestAppContext) {
    let (app, _, cx) = setup(cx);
    app.update(cx, |app, cx| {
        app.observe_tier_pickers(cx);
        app.refresh_ollama(cx);
        assert!(app.ollama_checking);
        let scope = app.session.scope();
        for result in [
            ResultPayload::Ollama(ollama::Status::Running {
                version: "0.40.6".into(),
                models: vec![ollama::LocalModel {
                    name: "qwen3.5:0.8b".into(),
                    thinking: true,
                }],
            }),
            ResultPayload::Models(Ok(vec!["test/sage".into()])),
        ] {
            app.tx.send(ResultEvent { scope, result }).unwrap();
        }
    });
    receive_results(&app, cx);
    cx.read_entity(&app, |app, cx| {
        assert!(!app.ollama_checking);
        for picker in &app.tier_pickers {
            assert_eq!(picker.read(cx).ollama.models, ["qwen3.5:0.8b"]);
            assert_eq!(picker.read(cx).wiesel.models, ["test/sage"]);
        }
    });
    // Choosing in Settings persists the tier. A tall window paints every section.
    menu(cx, OpenSettings);
    on_surface(&app, cx, Surface::Settings).simulate_resize(size(px(800.), px(2400.)));
    cx.run_until_parked();
    click(cx, "tier-sage-select");
    // Option 0 is "Not set", 1 the Ollama model, 2 the Wiesel model.
    click(cx, "tier-sage-option-2");
    click(cx, "tier-swift-select");
    click(cx, "tier-swift-option-1");
    cx.read_entity(&app, |app, _| {
        assert_eq!(
            app.settings.models.sage,
            Some(ModelChoice::wiesel("test/sage"))
        );
        assert_eq!(
            app.settings.models.swift,
            Some(ModelChoice::ollama("qwen3.5:0.8b"))
        );
        assert!(app.ollama_status.as_ref().unwrap().thinking("qwen3.5:0.8b"));
    });
}

#[gpui::test]
fn settings_switches_between_provider_tabs(cx: &mut TestAppContext) {
    let (app, cx) = setup_settings(cx);
    cx.read_entity(&app, |app, _| {
        assert_eq!(app.provider_tab, Provider::Wiesel)
    });
    click(cx, "provider-tab-ollama");
    cx.read_entity(&app, |app, _| {
        assert_eq!(app.provider_tab, Provider::Ollama)
    });
    // Sign-in entry points reveal the account tab again.
    cx.update(|window, cx| app.update(cx, |app, cx| app.start_login(window, cx)));
    cx.run_until_parked();
    cx.read_entity(&app, |app, _| {
        assert_eq!(app.provider_tab, Provider::Wiesel)
    });
}
