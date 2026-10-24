//! Real in-process UI input with isolated settings persistence and ControlledBackend.
use super::*;
use gpui::{Modifiers, TestAppContext, VisualTestContext};
use quick_actions::{CustomActionFields, CustomQuickAction};
use std::{cell::RefCell, rc::Rc};
use test_support::{bounds, input, key_window, keys, menu, on, on_surface, receive_results, setup};

fn click(cx: &mut VisualTestContext, selector: &str) {
    // GPUI debug selectors require static lifetimes; these tiny test-only IDs
    // live for the test process, not application state.
    let selector = Box::leak(selector.to_owned().into_boxed_str());
    // Built-ins precede custom rows now; scroll manager controls into view before clicking.
    if selector.starts_with("edit-") || selector.starts_with("delete-") {
        for _ in 0..3 {
            let Some(control) = bounds(cx, selector) else {
                break;
            };
            let Some(viewport) = bounds(cx, "action-manager-scroll") else {
                break;
            };
            let delta = if control.bottom() > viewport.bottom() {
                viewport.bottom() - control.bottom() - px(8.)
            } else if control.top() < viewport.top() {
                viewport.top() - control.top() + px(8.)
            } else {
                break;
            };
            on(cx, "action-manager-scroll")
                .unwrap()
                .simulate_event(ScrollWheelEvent {
                    position: viewport.center(),
                    delta: ScrollDelta::Pixels(point(px(0.), delta)),
                    modifiers: Modifiers::none(),
                    touch_phase: gpui::TouchPhase::Moved,
                });
            cx.run_until_parked();
        }
    }
    let mut window = on(cx, selector).unwrap_or_else(|| panic!("rendered {selector}"));
    let bounds = window.debug_bounds(selector).expect("rendered control");
    window.simulate_click(bounds.center(), Modifiers::none());
    cx.run_until_parked();
}
fn fill(cx: &mut VisualTestContext, name: &str, instructions: &str) {
    click(cx, "action-name");
    keys(cx, "cmd-a");
    input(cx, name);
    click(cx, "action-instructions");
    keys(cx, "cmd-a");
    input(cx, instructions);
}
fn create(cx: &mut VisualTestContext, name: &str, instructions: &str) {
    menu(cx, NewQuickAction);
    fill(cx, name, instructions);
    click(cx, "action-save");
}
/// Summon the launcher like the global shortcut does; other windows keep their state.
fn show_hud(app: &Entity<Wiesel>, cx: &mut VisualTestContext) {
    cx.update(|window, cx| app.update(cx, |app, cx| app.show_hud(window, cx)));
    cx.run_until_parked();
}
/// Summon the launcher and press one of its tiles.
fn run_tile(app: &Entity<Wiesel>, cx: &mut VisualTestContext, tile: &str) {
    show_hud(app, cx);
    click(cx, tile);
}
/// Summon the launcher and type on it (the fixture window is the launcher).
fn hud_keys(app: &Entity<Wiesel>, cx: &mut VisualTestContext, keystrokes: &str) {
    show_hud(app, cx);
    cx.simulate_keystrokes(keystrokes);
    cx.run_until_parked();
}
fn is_open(app: &Entity<Wiesel>, cx: &mut VisualTestContext, surface: Surface) -> bool {
    cx.read_entity(app, |app, _| app.is_open(surface))
}
fn action(name: &str, model: Option<&str>) -> CustomQuickAction {
    CustomQuickAction::new(CustomActionFields {
        name: name.into(),
        icon: "wand".into(),
        model_override: model.map(Into::into),
        instructions: "Exact custom instructions\n with whitespace \n{selection}".into(),
        paste_result: false,
    })
    .unwrap()
}
/// Options list the three tiers first, then Ollama (empty here), then Wiesel models.
fn load_models(app: &mut Wiesel, models: Vec<String>, cx: &mut Context<Wiesel>) {
    app.update_catalogs(Provider::Wiesel, cx, |catalog| {
        catalog.models = models.clone();
        catalog.note = None;
    });
    app.model_picker
        .update(cx, |picker, cx| picker.finish_load(Ok(models), cx));
}
fn seed(app: &Entity<Wiesel>, cx: &mut VisualTestContext, action: CustomQuickAction) {
    cx.update(|_, cx| {
        app.update(cx, |app, cx| {
            app.settings.custom_actions.push(action);
            load_models(
                app,
                vec!["test/chat-model".into(), "test/override".into()],
                cx,
            );
            app.selection.text = Some("Selected source text".into());
            cx.notify();
        })
    });
    cx.run_until_parked();
}

#[gpui::test]
fn builtin_manager_edit_dispatch_delete_and_shortcut_follow_saved_customization(
    cx: &mut TestAppContext,
) {
    let (app, backend, cx) = setup(cx);
    seed(&app, cx, action("User action", None));
    let id = QuickAction::FixSpelling.persistent_id();
    menu(cx, AdjustQuickActions);
    click(cx, &format!("edit-{id}"));
    cx.read_entity(&app, |app, cx| {
        let editor = app.actions.editor.as_ref().unwrap();
        assert!(editor.instructions.read(cx).content.contains("{selection}"));
    });
    fill(
        cx,
        "Fix grammar",
        "Fix grammar and spelling for the following text:\n\n{selection}",
    );
    click(cx, "action-icon");
    click(cx, "icon-lightbulb");
    click(cx, "model-select");
    input(cx, "override");
    keys(cx, "enter");
    click(cx, "action-save");
    cx.read_entity(&app, |app, _| {
        let configured = &app.settings.actions()[0];
        assert_eq!(configured.id, id);
        assert_eq!(configured.name, "Fix grammar");
        assert_eq!(configured.icon, "lightbulb");
        assert_eq!(configured.model_override, Some("test/override".into()));
    });
    click(cx, "back");
    run_tile(&app, cx, "grammar");
    assert!(is_open(&app, cx, Surface::Writing));
    let (model, messages, chat) = backend.request(0);
    assert_eq!(model, "test/override");
    assert!(!chat);
    assert_eq!(
        messages[1].content,
        "Fix grammar and spelling for the following text:\n\nSelected source text"
    );
    assert!(!messages[0].content.contains("Selected source text"));
    backend.finish(0, Ok("Corrected text".into()));
    receive_results(&app, cx);
    keys(cx, "escape");
    assert!(!is_open(&app, cx, Surface::Writing));
    menu(cx, AdjustQuickActions);
    click(cx, &format!("delete-{id}"));
    click(cx, "dialog-confirm");
    click(cx, "back");
    assert!(bounds(cx, "grammar").is_none());
    hud_keys(&app, cx, "cmd-1");
    assert_eq!(backend.request_count(), 1);
    cx.read_entity(&app, |app, _| {
        assert_eq!(app.current_screen(), Screen::Settings);
        assert!(!app.is_open(Surface::Writing));
        assert_eq!(app.settings.actions().len(), 5);
        assert_eq!(app.settings.custom_actions.len(), 1);
    });
}

#[gpui::test]
fn builder_say_hi_opens_chat_without_selection_or_clipboard(cx: &mut TestAppContext) {
    let (app, backend, cx) = setup(cx);
    seed(&app, cx, action("Existing", None));
    cx.update(|_, cx| {
        cx.write_to_clipboard(ClipboardItem::new_string(String::new()));
        app.update(cx, |app, _| app.selection.text = None);
    });
    create(cx, "Say hi", "say hi");
    let id = cx.read_entity(&app, |app, _| {
        app.settings.custom_actions.last().unwrap().id
    });
    run_tile(&app, cx, &format!("custom-{id}"));
    let (_, messages, chat) = backend.request(0);
    assert!(chat);
    assert_eq!(messages.len(), 2);
    assert_eq!(messages[1].role, "user");
    assert_eq!(messages[1].content, "say hi");
    cx.read_entity(&app, |app, _| {
        assert!(app.is_open(Surface::Chat));
        assert_eq!(app.messages[0].content, "say hi");
    });
    backend.finish(0, Ok("Hi!".into()));
    receive_results(&app, cx);
    cx.read_entity(&app, |app, _| {
        assert_eq!(app.messages.last().unwrap().content, "Hi!");
    });
}

#[gpui::test]
fn literal_action_ignores_unreferenced_selection_and_clipboard(cx: &mut TestAppContext) {
    let (app, backend, cx) = setup(cx);
    let mut custom = action("Literal", None);
    custom.instructions = "say hi {unknown}".into();
    let id = custom.id;
    seed(&app, cx, custom);
    cx.update(|_, cx| {
        cx.write_to_clipboard(ClipboardItem::new_string("Private clipboard".into()));
        app.update(cx, |app, _| app.selection.text = Some("x".repeat(100_001)));
    });
    click(cx, &format!("custom-{id}"));
    assert_eq!(backend.request(0).1[1].content, "say hi {unknown}");
    cx.update(|_, cx| {
        assert_eq!(
            cx.read_from_clipboard().unwrap().text().as_deref(),
            Some("Private clipboard")
        );
    });
}

#[gpui::test]
fn action_chat_followups_keep_model_and_expanded_sources_until_new_chat(cx: &mut TestAppContext) {
    let (app, backend, cx) = setup(cx);
    let mut custom = action("Explain", Some("test/override"));
    custom.instructions = "Explain {clipboard}".into();
    let id = custom.id;
    seed(&app, cx, custom);
    cx.update(|_, cx| cx.write_to_clipboard(ClipboardItem::new_string("Original source".into())));
    click(cx, &format!("custom-{id}"));
    backend.finish(0, Ok("Explanation".into()));
    receive_results(&app, cx);
    cx.update(|_, cx| cx.write_to_clipboard(ClipboardItem::new_string(String::new())));
    input(cx, "More detail");
    keys(cx, "enter");
    let (model, messages, chat) = backend.request(1);
    assert!(chat);
    assert_eq!(model, "test/override");
    assert_eq!(messages[1].content, "Explain Original source");
    assert_eq!(messages[2].content, "Explanation");
    assert_eq!(messages[3].content, "More detail");
    backend.finish(1, Ok("Details".into()));
    receive_results(&app, cx);
    click(cx, "new-chat");
    input(cx, "Hello");
    keys(cx, "enter");
    assert_eq!(backend.request(2).0, "test/chat-model");
    assert_eq!(backend.request(2).1.len(), 2);
}

#[gpui::test]
fn edited_builtin_without_placeholders_does_not_require_selection(cx: &mut TestAppContext) {
    let (app, backend, cx) = setup(cx);
    let id = QuickAction::FixSpelling.persistent_id();
    menu(cx, AdjustQuickActions);
    click(cx, &format!("edit-{id}"));
    fill(cx, "Greeting", "say hi");
    click(cx, "action-save");
    click(cx, "back");
    run_tile(&app, cx, "grammar");
    assert_eq!(backend.request(0).1[1].content, "say hi");
    cx.read_entity(&app, |app, _| {
        assert!(app.is_open(Surface::Writing));
        assert!(app.selection.writing_source.is_empty());
    });
}

#[gpui::test]
fn clipboard_only_action_runs_without_selection_and_preserves_literal_source_tokens(
    cx: &mut TestAppContext,
) {
    let (app, backend, cx) = setup(cx);
    let mut custom = action("Clipboard action", None);
    custom.instructions = "Explain:\n{clipboard}".into();
    let id = custom.id;
    seed(&app, cx, custom);
    cx.update(|_, cx| {
        cx.write_to_clipboard(ClipboardItem::new_string("Copied {selection}".into()));
        app.update(cx, |app, cx| {
            app.selection.text = None;
            cx.notify();
        });
    });
    click(cx, &format!("custom-{id}"));
    let (_, messages, _) = backend.request(0);
    assert_eq!(messages[1].content, "Explain:\nCopied {selection}");
    cx.read_entity(&app, |app, _| {
        assert!(app.selection.writing_source.is_empty())
    });
}

#[gpui::test]
fn selection_and_clipboard_variables_are_combined_and_missing_sources_send_nothing(
    cx: &mut TestAppContext,
) {
    let (app, backend, cx) = setup(cx);
    let mut custom = action("Compare", None);
    custom.instructions = "Compare {selection} with {clipboard}.".into();
    let id = custom.id;
    seed(&app, cx, custom);
    cx.update(|_, cx| cx.write_to_clipboard(ClipboardItem::new_string(String::new())));
    click(cx, &format!("custom-{id}"));
    assert_eq!(backend.request_count(), 0);
    cx.update(|_, cx| {
        cx.write_to_clipboard(ClipboardItem::new_string("Copied text".into()));
        app.update(cx, |app, cx| {
            app.selection.text = None;
            cx.notify();
        });
    });
    click(cx, &format!("custom-{id}"));
    assert_eq!(backend.request_count(), 0);
    cx.update(|_, cx| {
        app.update(cx, |app, cx| {
            app.selection.text = Some("Selected text".into());
            cx.notify();
        })
    });
    click(cx, &format!("custom-{id}"));
    assert_eq!(
        backend.request(0).1[1].content,
        "Compare Selected text with Copied text."
    );
}

#[gpui::test]
fn edited_builtin_chat_keeps_conversation_mode_and_honors_instructions(cx: &mut TestAppContext) {
    let (app, backend, cx) = setup(cx);
    let id = QuickAction::Chat.persistent_id();
    menu(cx, AdjustQuickActions);
    click(cx, &format!("edit-{id}"));
    fill(cx, "My chat", "Answer in short sentences.");
    click(cx, "action-save");
    click(cx, "back");
    run_tile(&app, cx, "chat");
    input(cx, "Hello");
    keys(cx, "enter");
    let (_, messages, chat) = backend.request(0);
    assert!(chat);
    assert_eq!(messages[0].content, "Answer in short sentences.");
    assert_eq!(messages[1].content, "Hello");
}

#[gpui::test]
fn create_icon_model_multiline_and_edit_preserve_uuid(cx: &mut TestAppContext) {
    let (app, backend, cx) = setup(cx);
    cx.update(|_, cx| {
        app.update(cx, |app, cx| {
            load_models(
                app,
                vec!["test/chat-model".into(), "test/override".into()],
                cx,
            )
        })
    });
    menu(cx, NewQuickAction);
    fill(cx, "  My action  ", "First line");
    keys(cx, "enter");
    input(cx, "Second line");
    click(cx, "action-icon");
    click(cx, "icon-lightbulb");
    click(cx, "model-select");
    input(cx, "override");
    keys(cx, "enter");
    keys(cx, "cmd-enter");
    let id = cx.read_entity(&app, |app, _| {
        assert_eq!(app.current_screen(), Screen::Settings);
        assert_eq!(app.settings.custom_actions.len(), 1);
        let action = &app.settings.custom_actions[0];
        assert_eq!(action.name, "My action");
        assert_eq!(action.icon, "lightbulb");
        assert_eq!(action.instructions, "First line\nSecond line");
        assert_eq!(action.model_override, Some("test/override".into()));
        action.id
    });
    assert_eq!(backend.request_count(), 0);
    menu(cx, AdjustQuickActions);
    click(cx, &format!("edit-{id}"));
    fill(cx, "Renamed", "Updated instructions");
    click(cx, "model-select");
    // Sharp is the default, so it clears the override.
    click(cx, "model-option-1");
    click(cx, "action-save");
    cx.read_entity(&app, |app, _| {
        assert_eq!(app.current_screen(), Screen::ActionManager);
        assert_eq!(app.settings.custom_actions[0].id, id);
        assert_eq!(app.settings.custom_actions[0].name, "Renamed");
        assert!(app.settings.custom_actions[0].model_override.is_none());
    });
    // Esc returns to Settings with the focus it had when Quick actions covered it.
    keys(cx, "escape");
    assert_eq!(
        cx.read_entity(&app, |app, _| app.current_screen()),
        Screen::Settings
    );
    on_surface(&app, cx, Surface::Settings)
        .update(|window, cx| assert!(app.read(cx).screen_focus(cx).is_focused(window)));
}

#[gpui::test]
fn validation_and_failed_create_keep_editor_and_retry_identity(cx: &mut TestAppContext) {
    let (app, backend, cx) = setup(cx);
    let attempts = Rc::new(RefCell::new(vec![]));
    let saved = attempts.clone();
    cx.update(|_, cx| {
        app.update(cx, |app, _| {
            app.actions.persist = Some(Box::new(move |next| {
                saved.borrow_mut().push(next.custom_actions[0].id);
                if saved.borrow().len() == 1 {
                    Err(settings::SettingsError::Write)
                } else {
                    Ok(())
                }
            }));
        })
    });
    menu(cx, NewQuickAction);
    click(cx, "action-save");
    assert!(bounds(cx, "action-error").is_some());
    fill(cx, "Name", "");
    click(cx, "action-save");
    assert!(attempts.borrow().is_empty());
    fill(cx, "Name", "Private instructions");
    click(cx, "action-save");
    cx.read_entity(&app, |app, cx| {
        assert_eq!(app.current_screen(), Screen::ActionEditor);
        assert!(app.settings.custom_actions.is_empty());
        assert_eq!(
            app.actions
                .editor
                .as_ref()
                .unwrap()
                .name
                .read(cx)
                .content
                .as_ref(),
            "Name"
        );
    });
    assert!(bounds(cx, "action-error").is_some());
    click(cx, "action-save");
    assert_eq!(attempts.borrow().len(), 2);
    assert_eq!(attempts.borrow()[0], attempts.borrow()[1]);
    cx.read_entity(&app, |app, _| {
        assert_eq!(app.settings.custom_actions.len(), 1);
        assert_eq!(app.current_screen(), Screen::Settings);
    });
    assert_eq!(backend.request_count(), 0);
}

#[gpui::test]
fn delete_cancel_confirm_and_failed_update_delete_are_transactional(cx: &mut TestAppContext) {
    let (app, _, cx) = setup(cx);
    let original = action("Original", None);
    let id = original.id;
    seed(&app, cx, original.clone());
    menu(cx, AdjustQuickActions);
    click(cx, &format!("delete-{id}"));
    click(cx, "dialog-cancel");
    cx.read_entity(&app, |app, _| {
        assert_eq!(app.settings.custom_actions, vec![original.clone()])
    });
    cx.update(|_, cx| {
        app.update(cx, |app, _| {
            app.actions.persist = Some(Box::new(|_| Err(settings::SettingsError::Write)))
        })
    });
    click(cx, &format!("edit-{id}"));
    fill(cx, "Changed", "Changed instructions");
    click(cx, "action-save");
    cx.read_entity(&app, |app, _| {
        assert_eq!(app.current_screen(), Screen::ActionEditor);
        assert_eq!(app.settings.custom_actions, vec![original.clone()]);
    });
    click(cx, "action-cancel");
    click(cx, "dialog-confirm");
    click(cx, &format!("delete-{id}"));
    click(cx, "dialog-confirm");
    cx.read_entity(&app, |app, _| {
        assert_eq!(app.settings.custom_actions, vec![original.clone()])
    });
    assert!(bounds(cx, "action-error").is_some());
    cx.update(|_, cx| {
        app.update(cx, |app, _| {
            app.actions.persist = Some(Box::new(|_| Ok(())))
        })
    });
    click(cx, &format!("delete-{id}"));
    keys(cx, "tab enter");
    cx.read_entity(&app, |app, _| {
        assert!(app.settings.custom_actions.is_empty());
        assert_eq!(app.current_screen(), Screen::ActionManager);
    });
}

#[gpui::test]
fn dirty_escape_back_cancel_and_popup_precedence_preserve_draft(cx: &mut TestAppContext) {
    let (app, _, cx) = setup(cx);
    menu(cx, NewQuickAction);
    fill(cx, "Dirty name", "Draft");
    click(cx, "action-icon");
    keys(cx, "escape");
    cx.read_entity(&app, |app, _| {
        assert_eq!(app.current_screen(), Screen::ActionEditor);
        assert!(app.actions.dialog.is_none());
    });
    click(cx, "model-select");
    keys(cx, "escape");
    cx.read_entity(&app, |app, _| assert!(app.actions.dialog.is_none()));
    keys(cx, "escape");
    keys(cx, "escape");
    cx.read_entity(&app, |app, cx| {
        assert_eq!(app.current_screen(), Screen::ActionEditor);
        assert_eq!(
            app.actions
                .editor
                .as_ref()
                .unwrap()
                .name
                .read(cx)
                .content
                .as_ref(),
            "Dirty name"
        );
    });
    click(cx, "back");
    click(cx, "dialog-cancel");
    click(cx, "action-cancel");
    click(cx, "dialog-confirm");
    cx.read_entity(&app, |app, _| {
        assert_eq!(app.current_screen(), Screen::Settings);
        assert!(app.is_open(Surface::Settings));
        assert!(app.settings.custom_actions.is_empty());
        assert!(app.actions.editor.is_none());
    });
}

#[gpui::test]
fn manager_new_cancel_and_settings_menu_keep_nested_editor_until_confirmed(
    cx: &mut TestAppContext,
) {
    let (app, _, cx) = setup(cx);
    menu(cx, AdjustQuickActions);
    click(cx, "new-action");
    click(cx, "action-cancel");
    cx.read_entity(&app, |app, _| {
        assert_eq!(app.current_screen(), Screen::ActionManager)
    });
    click(cx, "new-action");
    fill(cx, "Nested", "Draft");
    // Settings is the window's root, so revealing it asks before dropping the draft.
    menu(cx, OpenSettings);
    cx.read_entity(&app, |app, _| {
        assert_eq!(app.navigation.depth(), 3);
        assert!(app.actions.dialog.is_some());
    });
    click(cx, "dialog-cancel");
    cx.read_entity(&app, |app, cx| {
        assert_eq!(app.current_screen(), Screen::ActionEditor);
        assert_eq!(
            app.actions
                .editor
                .as_ref()
                .unwrap()
                .name
                .read(cx)
                .content
                .as_ref(),
            "Nested"
        );
    });
    click(cx, "action-save");
    cx.read_entity(&app, |app, _| {
        assert_eq!(app.current_screen(), Screen::ActionManager);
        assert_eq!(app.settings.custom_actions[0].name, "Nested");
    });
    click(cx, "back");
    cx.read_entity(&app, |app, _| {
        assert_eq!(app.current_screen(), Screen::Settings)
    });
}

#[gpui::test]
fn custom_dispatch_opens_chat_with_expanded_instructions_model_and_stale_guards(
    cx: &mut TestAppContext,
) {
    let (app, backend, cx) = setup(cx);
    let custom = action("Custom", Some("test/override"));
    let id = custom.id;
    seed(&app, cx, custom.clone());
    click(cx, &format!("custom-{id}"));
    let (model, messages, chat) = backend.request(0);
    assert_eq!(model, "test/override");
    assert!(chat);
    assert_eq!(messages[0].role, "system");
    assert!(!messages[0].content.contains("Selected source text"));
    assert_eq!(messages[1].role, "user");
    assert_eq!(
        messages[1].content,
        "Exact custom instructions\n with whitespace \nSelected source text"
    );
    cx.read_entity(&app, |app, _| {
        assert_eq!(app.settings.model, "test/chat-model");
        assert!(app.is_open(Surface::Chat));
        assert!(app.selection.writing_source.is_empty());
    });
    menu(cx, OpenSettings);
    backend.finish(0, Ok("Finished custom result".into()));
    receive_results(&app, cx);
    cx.read_entity(&app, |app, _| {
        assert_eq!(app.current_screen(), Screen::Settings);
        assert!(app.is_open(Surface::Settings) && app.is_open(Surface::Chat));
        assert_eq!(
            app.messages.last().unwrap().content,
            "Finished custom result"
        );
    });
    keys(cx, "escape");
    assert!(!is_open(&app, cx, Surface::Settings));
    run_tile(&app, cx, &format!("custom-{id}"));
    assert_eq!(backend.request_count(), 2);
    keys(cx, "escape");
    assert!(!is_open(&app, cx, Surface::Chat));
    cx.update(|_, cx| {
        app.update(cx, |app, cx| {
            app.session.cancel_login();
            app.end_session_state(cx);
            cx.notify();
        })
    });
    backend.finish(1, Ok("Stale private result".into()));
    receive_results(&app, cx);
    cx.read_entity(&app, |app, _| {
        assert!(!app.is_open(Surface::Chat) && !app.is_open(Surface::Writing));
        assert!(app.messages.is_empty());
        assert!(app.result.is_empty());
        assert!(app.selection.writing_source.is_empty());
        assert!(!app.busy);
    });
}

#[gpui::test]
fn default_follows_changed_setting_and_unavailable_override_never_falls_back(
    cx: &mut TestAppContext,
) {
    let (app, backend, cx) = setup(cx);
    let custom = action("Follow default", None);
    let id = custom.id;
    seed(&app, cx, custom);
    cx.update(|_, cx| {
        app.update(cx, |app, cx| {
            app.settings.model = "test/override".into();
            cx.notify();
        })
    });
    click(cx, &format!("custom-{id}"));
    assert_eq!(backend.request(0).0, "test/override");
    backend.finish(0, Ok("Result".into()));
    receive_results(&app, cx);
    keys(cx, "escape");
    cx.update(|_, cx| {
        app.update(cx, |app, cx| {
            app.settings.custom_actions[0].model_override = Some("test/removed".into());
            cx.notify();
        })
    });
    run_tile(&app, cx, &format!("custom-{id}"));
    assert_eq!(backend.request_count(), 1);
    cx.read_entity(&app, |app, _| {
        assert!(!app.is_open(Surface::Chat));
        assert!(app.hud_visible);
        assert!(!app.busy);
        assert_eq!(app.settings.model, "test/override");
    });
}

#[gpui::test]
fn read_only_settings_block_create_delete_without_fake_success(cx: &mut TestAppContext) {
    let (app, _, cx) = setup(cx);
    let custom = action("Saved", None);
    let id = custom.id;
    seed(&app, cx, custom);
    cx.update(|_, cx| {
        app.update(cx, |app, cx| {
            app.actions.read_only = true;
            cx.notify();
        })
    });
    menu(cx, AdjustQuickActions);
    click(cx, &format!("delete-{id}"));
    cx.read_entity(&app, |app, _| assert!(app.actions.dialog.is_none()));
    cx.read_entity(&app, |app, _| {
        assert_eq!(app.settings.custom_actions.len(), 1)
    });
    click(cx, "new-action");
    fill(cx, "New", "Draft");
    click(cx, "action-save");
    cx.read_entity(&app, |app, _| {
        assert_eq!(app.current_screen(), Screen::ActionEditor);
        assert_eq!(app.settings.custom_actions.len(), 1);
    });
}

#[gpui::test]
fn keyboard_tab_enter_and_cmd_enter_do_not_hijack_typing(cx: &mut TestAppContext) {
    let (app, backend, cx) = setup(cx);
    menu(cx, NewQuickAction);
    input(cx, "fircse");
    keys(cx, "tab");
    keys(cx, "enter escape tab");
    input(cx, "Instructions via Tab");
    keys(cx, "enter");
    input(cx, "Line two");
    keys(cx, "cmd-enter");
    cx.read_entity(&app, |app, _| {
        assert_eq!(app.current_screen(), Screen::Settings);
        assert_eq!(app.settings.custom_actions[0].name, "fircse");
        assert_eq!(
            app.settings.custom_actions[0].instructions,
            "Instructions via Tab\nLine two"
        );
    });
    assert_eq!(backend.request_count(), 0);
}

struct DisposableSettings(std::path::PathBuf);
impl DisposableSettings {
    fn new() -> Self {
        use std::os::unix::fs::DirBuilderExt;
        let path = std::env::temp_dir().canonicalize().unwrap().join(format!(
            "wiesel-builder-{}",
            uuid::Uuid::from_u128(
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            )
        ));
        std::fs::DirBuilder::new()
            .mode(0o700)
            .create(&path)
            .unwrap();
        Self(path)
    }
    fn file(&self) -> std::path::PathBuf {
        self.0.join("settings.json")
    }
}
impl Drop for DisposableSettings {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[gpui::test]
fn rendered_crud_round_trips_disposable_settings_without_user_files(cx: &mut TestAppContext) {
    let (app, _, cx) = setup(cx);
    let temp = DisposableSettings::new();
    let path = temp.file();
    let sink = path.clone();
    cx.update(|_, cx| {
        app.update(cx, |app, _| {
            app.actions.persist = Some(Box::new(move |next| settings::save_to(&sink, next)))
        })
    });
    create(cx, "Persisted", "Original instructions");
    let saved = settings::load_from(&path).unwrap();
    let id = saved.custom_actions[0].id;
    menu(cx, AdjustQuickActions);
    click(cx, &format!("edit-{id}"));
    fill(cx, "Updated", "  Kept exactly\nnext line  ");
    click(cx, "action-save");
    let saved = settings::load_from(&path).unwrap();
    assert_eq!(saved.custom_actions[0].id, id);
    assert_eq!(
        saved.custom_actions[0].instructions,
        "  Kept exactly\nnext line  "
    );
    click(cx, &format!("delete-{id}"));
    click(cx, "dialog-confirm");
    assert!(
        settings::load_from(&path)
            .unwrap()
            .custom_actions
            .is_empty()
    );
    cx.read_entity(&app, |app, _| {
        assert!(app.settings.custom_actions.is_empty())
    });
}

#[gpui::test]
fn invalid_disposable_settings_recovery_retains_file_and_failed_editor(cx: &mut TestAppContext) {
    let (app, _, cx) = setup(cx);
    let temp = DisposableSettings::new();
    let path = temp.file();
    let sink = path.clone();
    let invalid = b"{invalid private settings data";
    std::fs::write(&path, invalid).unwrap();
    cx.update(|_, cx| {
        app.update(cx, |app, _| {
            app.actions.persist = Some(Box::new(move |next| settings::save_to(&sink, next)))
        })
    });
    menu(cx, NewQuickAction);
    fill(cx, "Must not overwrite", "Private draft");
    click(cx, "action-save");
    assert_eq!(std::fs::read(&path).unwrap(), invalid);
    cx.read_entity(&app, |app, _| {
        assert!(app.actions.read_only);
        assert!(app.settings.custom_actions.is_empty());
        assert_eq!(app.current_screen(), Screen::ActionEditor);
    });
    assert!(bounds(cx, "action-error").is_some());
}

#[gpui::test]
fn hotkey_keeps_dirty_editor_and_settings_reveal_is_atomic_until_confirmed(
    cx: &mut TestAppContext,
) {
    let (app, _, cx) = setup(cx);
    menu(cx, AdjustQuickActions);
    click(cx, "new-action");
    fill(cx, "Retain me", "Draft");
    // The shortcut only brings up the launcher; the Settings window keeps its stack.
    cx.update(|window, cx| app.update(cx, |app, cx| app.activate_from_hotkey(window, cx)));
    cx.run_until_parked();
    cx.read_entity(&app, |app, cx| {
        assert!(app.hud_visible);
        assert!(app.is_open(Surface::Settings));
        assert_eq!(app.navigation.depth(), 3);
        assert!(app.actions.dialog.is_none());
        assert_eq!(
            app.actions
                .editor
                .as_ref()
                .unwrap()
                .name
                .read(cx)
                .content
                .as_ref(),
            "Retain me"
        );
    });
    // Revealing the Settings root would drop the draft: every entry is guarded
    // before the first pop.
    menu(cx, OpenSettings);
    cx.read_entity(&app, |app, _| {
        assert_eq!(app.current_screen(), Screen::ActionEditor);
        assert_eq!(app.navigation.depth(), 3);
        assert!(app.actions.dialog.is_some());
    });
    click(cx, "dialog-cancel");
    cx.read_entity(&app, |app, _| {
        assert_eq!(app.navigation.depth(), 3);
        assert!(app.actions.editor.is_some());
    });
    let root = cx.read_entity(&app, |app, _| app.navigation.root().id);
    on_surface(&app, cx, Surface::Settings).update(|window, cx| {
        app.update(cx, |app, cx| app.pop_to_screen(root, window, cx));
    });
    cx.run_until_parked();
    click(cx, "dialog-confirm");
    cx.read_entity(&app, |app, _| {
        assert_eq!(app.current_screen(), Screen::Settings);
        assert_eq!(app.navigation.depth(), 1);
        assert!(app.actions.editor.is_none());
    });
    // Settings directly below a newly mounted dirty editor.
    on_surface(&app, cx, Surface::Settings)
        .update(|window, cx| app.update(cx, |app, cx| app.open_action_editor(None, window, cx)));
    cx.run_until_parked();
    fill(cx, "Nested", "Draft");
    menu(cx, OpenSettings);
    cx.read_entity(&app, |app, _| {
        assert_eq!(app.current_screen(), Screen::ActionEditor);
        assert_eq!(app.navigation.depth(), 2);
        assert!(app.actions.dialog.is_some());
    });
    click(cx, "dialog-cancel");
    menu(cx, OpenSettings);
    click(cx, "dialog-confirm");
    cx.read_entity(&app, |app, _| {
        assert_eq!(app.current_screen(), Screen::Settings);
        assert_eq!(app.navigation.depth(), 1);
        assert!(app.actions.editor.is_none());
    });
}

#[gpui::test]
fn busy_inference_allows_builder_editing_without_duplicate_or_route_resurrection(
    cx: &mut TestAppContext,
) {
    let (app, backend, cx) = setup(cx);
    let custom = action("Original", None);
    let id = custom.id;
    seed(&app, cx, custom);
    click(cx, &format!("custom-{id}"));
    // Closing Chat does not cancel the request; pressing the tile again sends nothing.
    keys(cx, "escape");
    run_tile(&app, cx, &format!("custom-{id}"));
    assert_eq!(backend.request_count(), 1);
    menu(cx, AdjustQuickActions);
    click(cx, &format!("edit-{id}"));
    fill(cx, "Edited during request", "Updated for next run");
    click(cx, "action-save");
    backend.finish(0, Ok("Older completed result".into()));
    receive_results(&app, cx);
    cx.read_entity(&app, |app, _| {
        assert_eq!(app.current_screen(), Screen::ActionManager);
        assert!(!app.is_open(Surface::Chat), "a result does not reopen Chat");
        assert_eq!(app.settings.custom_actions[0].name, "Edited during request");
        assert_eq!(
            app.messages.last().unwrap().content,
            "Older completed result"
        );
        assert!(app.selection.writing_source.is_empty());
        assert!(!app.busy);
    });
}

#[gpui::test]
fn inference_auth_selection_and_size_gates_send_nothing(cx: &mut TestAppContext) {
    let (app, backend, cx) = setup(cx);
    let custom = action("Guarded", None);
    let id = custom.id;
    seed(&app, cx, custom);
    cx.update(|_, cx| {
        app.update(cx, |app, cx| {
            app.selection.text = None;
            cx.notify();
        })
    });
    click(cx, &format!("custom-{id}"));
    cx.update(|_, cx| {
        app.update(cx, |app, cx| {
            app.selection.text = Some("x".repeat(100_001));
            cx.notify();
        })
    });
    click(cx, &format!("custom-{id}"));
    cx.update(|_, cx| {
        app.update(cx, |app, cx| {
            app.selection.text = Some("Selected".into());
            app.authenticated = false;
            cx.notify();
        })
    });
    click(cx, &format!("custom-{id}"));
    cx.read_entity(&app, |app, _| {
        assert!(app.is_open(Surface::Settings));
        assert_eq!(app.current_screen(), Screen::Settings)
    });
    keys(cx, "escape");
    assert!(!is_open(&app, cx, Surface::Settings));
    cx.update(|_, cx| {
        app.update(cx, |app, cx| {
            app.authenticated = true;
            app.session.credential = None;
            cx.notify();
        })
    });
    run_tile(&app, cx, &format!("custom-{id}"));
    assert_eq!(backend.request_count(), 0);
    cx.read_entity(&app, |app, _| assert!(!app.busy));
}

#[gpui::test]
fn number_keys_run_only_the_tiles_on_the_launcher(cx: &mut TestAppContext) {
    let (app, backend, cx) = setup(cx);
    // Five built-ins fill five of eight slots; 6–9 print on no tile and do nothing.
    keys(cx, "6 9");
    cx.read_entity(&app, |app, _| {
        assert!(app.hud_visible);
        assert!(!app.is_open(Surface::Settings) && !app.is_open(Surface::Chat));
        assert!(app.actions.editor.is_none(), "there is no add tile");
    });
    let custom = action("Sixth", None);
    seed(&app, cx, custom);
    keys(cx, "6");
    cx.read_entity(&app, |app, _| {
        assert!(app.is_open(Surface::Chat));
        assert_eq!(app.hud_selected, 5);
    });
    assert_eq!(
        backend.request(0).1[1].content,
        "Exact custom instructions\n with whitespace \nSelected source text"
    );
}

#[gpui::test]
fn menu_commands_reveal_singleton_settings_and_quick_actions(cx: &mut TestAppContext) {
    let (app, _, cx) = setup(cx);
    menu(cx, AdjustQuickActions);
    menu(cx, OpenSettings);
    menu(cx, AdjustQuickActions);
    cx.read_entity(&app, |app, _| {
        assert_eq!(app.current_screen(), Screen::ActionManager);
        assert_eq!(
            app.navigation.depth(),
            2,
            "quick actions are revealed, not stacked"
        );
    });
    menu(cx, NewQuickAction);
    cx.read_entity(&app, |app, _| {
        assert_eq!(app.current_screen(), Screen::ActionEditor)
    });
    assert!(
        on_surface(&app, cx, Surface::Settings)
            .debug_bounds("toolbar")
            .is_some()
    );
    click(cx, "back");
    click(cx, "back");
    menu(cx, NewChat);
    cx.read_entity(&app, |app, _| {
        assert!(app.is_open(Surface::Chat));
        assert!(app.is_open(Surface::Settings));
    });
}

#[gpui::test]
fn custom_actions_follow_the_builtins_on_the_launcher_up_to_its_size(cx: &mut TestAppContext) {
    let (app, _, cx) = setup(cx);
    let customs: Vec<_> = [
        "First custom",
        "Second custom",
        "Third custom",
        "Fourth custom",
    ]
    .into_iter()
    .map(|name| action(name, None))
    .collect();
    for custom in &customs {
        seed(&app, cx, custom.clone());
    }
    let tile = |action: &CustomQuickAction| -> &'static str {
        Box::leak(format!("custom-{}", action.id).into_boxed_str())
    };
    let row: Vec<_> = [
        "explain",
        tile(&customs[0]),
        tile(&customs[1]),
        tile(&customs[2]),
    ]
    .into_iter()
    .map(|id| bounds(cx, id).unwrap_or_else(|| panic!("tile {id}")))
    .collect();
    for pair in row.windows(2) {
        assert_eq!(pair[0].top(), pair[1].top(), "one row");
        assert!(pair[0].right() < pair[1].left(), "in settings order");
    }
    // Eight tiles by default: the ninth action is only reachable from Quick actions.
    assert_eq!(cx.read_entity(&app, |app, _| app.home_actions().len()), 8);
    assert!(bounds(cx, tile(&customs[3])).is_none());
}

#[gpui::test]
fn many_actions_keep_one_launcher_row_and_menu_create_reachable(cx: &mut TestAppContext) {
    let (app, _, cx) = setup(cx);
    cx.update(|_, cx| {
        app.update(cx, |app, cx| {
            app.settings.custom_actions = (0..16)
                .map(|n| action(&format!("Action {n}"), None))
                .collect();
            cx.notify();
        })
    });
    cx.run_until_parked();
    let status = bounds(cx, "status-bar").unwrap();
    assert_eq!(status.size.height, px(HUD_STATUS_HEIGHT));
    let first = bounds(cx, "grammar").unwrap();
    let third = bounds(cx, "chat").unwrap();
    assert_eq!(first.size, third.size);
    assert_eq!(first.top(), third.top());
    assert!(first.left() < third.left());
    assert!(third.bottom() <= status.top());
    menu(cx, NewQuickAction);
    fill(cx, "Accessible overflow add", "Instructions");
    click(cx, "action-save");
    cx.read_entity(&app, |app, _| {
        assert_eq!(app.settings.custom_actions.len(), 17);
        assert_eq!(app.home_actions().len(), 8);
    });
    menu(cx, AdjustQuickActions);
    click(cx, "new-action");
    click(cx, "action-cancel");
    cx.read_entity(&app, |app, _| {
        assert_eq!(app.current_screen(), Screen::ActionManager)
    });
}

#[gpui::test]
fn repeated_save_shortcut_saves_once_and_sends_nothing(cx: &mut TestAppContext) {
    let (app, backend, cx) = setup(cx);
    menu(cx, NewQuickAction);
    fill(cx, "Once", "Instructions");
    keys(cx, "cmd-enter cmd-enter cmd-enter");
    cx.read_entity(&app, |app, _| {
        assert_eq!(app.current_screen(), Screen::Settings);
        assert_eq!(app.settings.custom_actions.len(), 1);
    });
    assert_eq!(backend.request_count(), 0);
}

#[gpui::test]
fn login_shortcut_cannot_bypass_dirty_editor_or_confirmation_focus(cx: &mut TestAppContext) {
    let (app, backend, cx) = setup(cx);
    menu(cx, OpenSettings);
    on_surface(&app, cx, Surface::Settings)
        .update(|window, cx| app.update(cx, |app, cx| app.open_action_editor(None, window, cx)));
    cx.run_until_parked();
    fill(cx, "Private draft", "Instructions");
    // From another window, login is routed into the Settings window.
    cx.update(|window, cx| app.update(cx, |app, cx| app.start_login(window, cx)));
    cx.run_until_parked();
    cx.read_entity(&app, |app, _| {
        assert_eq!(app.current_screen(), Screen::ActionEditor);
        assert!(app.actions.dialog.is_some());
        assert!(app.session.attempt.is_none());
    });
    on_surface(&app, cx, Surface::Settings).update(|window, cx| {
        let focused = window.focused(cx);
        app.update(cx, |app, cx| app.start_login(window, cx));
        assert_eq!(focused, window.focused(cx));
    });
    cx.run_until_parked();
    click(cx, "dialog-cancel");
    cx.read_entity(&app, |app, cx| {
        assert!(app.session.attempt.is_none());
        assert_eq!(
            app.actions
                .editor
                .as_ref()
                .unwrap()
                .name
                .read(cx)
                .content
                .as_ref(),
            "Private draft"
        )
    });
    assert_eq!(backend.request_count(), 0);
}

/// Action builder controls live in the Settings window.
fn control_focused(app: &Entity<Wiesel>, cx: &mut VisualTestContext, id: &str) -> bool {
    on_surface(app, cx, Surface::Settings)
        .update(|window, cx| app.read(cx).action_control_focus(id, cx).is_focused(window))
}
fn tab_to(app: &Entity<Wiesel>, cx: &mut VisualTestContext, id: &str) {
    for _ in 0..100 {
        if control_focused(app, cx, id) {
            return;
        }
        keys(cx, "tab");
    }
    panic!("Tab did not reach {id}");
}
fn wheel(cx: &mut VisualTestContext, selector: &'static str, delta: f32) {
    let mut window = on(cx, selector).unwrap_or_else(|| panic!("rendered {selector}"));
    let bounds = window.debug_bounds(selector).unwrap();
    window.simulate_event(ScrollWheelEvent {
        position: bounds.center(),
        delta: ScrollDelta::Pixels(point(px(0.), px(delta))),
        modifiers: Modifiers::none(),
        touch_phase: gpui::TouchPhase::Moved,
    });
    cx.run_until_parked();
}
fn key_event(cx: &mut VisualTestContext, key: &str, is_held: bool) {
    key_window(cx).simulate_event(KeyDownEvent {
        keystroke: gpui::Keystroke::parse(key).unwrap(),
        is_held,
        prefer_character_input: false,
    });
}
fn held_key(cx: &mut VisualTestContext, key: &str) {
    key_event(cx, key, true);
}
fn mouse_press(cx: &mut VisualTestContext, selector: &'static str) -> VisualTestContext {
    let mut window = on(cx, selector).unwrap_or_else(|| panic!("rendered {selector}"));
    let center = window.debug_bounds(selector).unwrap().center();
    window.simulate_mouse_down(center, MouseButton::Left, Modifiers::none());
    window
}
fn mouse_release(window: &mut VisualTestContext, selector: &'static str) {
    let center = window.debug_bounds(selector).unwrap().center();
    window.simulate_mouse_up(center, MouseButton::Left, Modifiers::none());
    window.run_until_parked();
}

#[gpui::test]
fn manager_new_and_edit_focus_reactivate_with_enter_after_pop(cx: &mut TestAppContext) {
    let (app, _, cx) = setup(cx);
    let custom = action("Saved", None);
    let id = custom.id;
    seed(&app, cx, custom);
    menu(cx, AdjustQuickActions);
    cx.read_entity(&app, |app, _| {
        assert_eq!(app.current_screen(), Screen::ActionManager)
    });
    tab_to(&app, cx, "new-action");
    keys(cx, "enter escape");
    assert!(control_focused(&app, cx, "new-action"));
    keys(cx, "enter escape");
    tab_to(&app, cx, &format!("edit-{id}"));
    keys(cx, "enter escape");
    assert!(control_focused(&app, cx, &format!("edit-{id}")));
    keys(cx, "enter");
    cx.read_entity(&app, |app, _| {
        assert_eq!(app.current_screen(), Screen::ActionEditor)
    });
    keys(cx, "escape escape");
    cx.read_entity(&app, |app, _| {
        assert_eq!(app.current_screen(), Screen::Settings);
        assert!(app.is_open(Surface::Settings));
    });
}

#[gpui::test]
fn delete_cancel_failure_and_success_restore_only_mounted_controls(cx: &mut TestAppContext) {
    let (app, _, cx) = setup(cx);
    let custom = action("Delete target", None);
    let id = custom.id;
    seed(&app, cx, custom);
    menu(cx, AdjustQuickActions);
    tab_to(&app, cx, &format!("delete-{id}"));
    keys(cx, "enter escape");
    assert!(control_focused(&app, cx, &format!("delete-{id}")));
    keys(cx, "enter");
    cx.update(|_, cx| {
        app.update(cx, |app, _| {
            app.actions.persist = Some(Box::new(|_| Err(settings::SettingsError::Write)))
        })
    });
    keys(cx, "tab enter");
    assert!(control_focused(&app, cx, &format!("delete-{id}")));
    cx.update(|_, cx| {
        app.update(cx, |app, _| {
            app.actions.persist = Some(Box::new(|_| Ok(())))
        })
    });
    keys(cx, "enter tab enter");
    assert!(control_focused(&app, cx, "new-action"));
    keys(cx, "enter");
    cx.read_entity(&app, |app, _| {
        assert_eq!(app.current_screen(), Screen::ActionEditor);
        assert!(app.settings.custom_actions.is_empty());
    });
    keys(cx, "escape escape");
    cx.read_entity(&app, |app, _| {
        assert_eq!(app.current_screen(), Screen::Settings)
    });
}

#[gpui::test]
fn clean_editor_back_gesture_only_dismisses_model_or_icon_popup(cx: &mut TestAppContext) {
    let (app, _, cx) = setup(cx);
    menu(cx, NewQuickAction);
    click(cx, "model-select");
    let mut settings = mouse_press(cx, "back");
    mouse_release(&mut settings, "back");
    cx.read_entity(&app, |app, _| {
        assert_eq!(app.current_screen(), Screen::ActionEditor);
        assert!(app.actions.dialog.is_none());
    });
    keys(cx, "enter");
    assert!(bounds(cx, "model-option-0").is_some());
    keys(cx, "escape");
    click(cx, "action-icon");
    click(cx, "back");
    assert!(control_focused(&app, cx, "action-icon"));
    keys(cx, "enter");
    assert!(bounds(cx, "icon-wand").is_some());
    keys(cx, "escape");
    click(cx, "back");
    cx.read_entity(&app, |app, _| {
        assert_eq!(app.current_screen(), Screen::Settings)
    });
}

#[gpui::test]
fn dirty_editor_back_gesture_dismisses_popup_before_discard_confirmation(cx: &mut TestAppContext) {
    let (app, _, cx) = setup(cx);
    menu(cx, NewQuickAction);
    fill(cx, "Dirty", "Draft");
    click(cx, "model-select");
    click(cx, "back");
    cx.read_entity(&app, |app, _| {
        assert_eq!(app.current_screen(), Screen::ActionEditor);
        assert!(app.actions.dialog.is_none());
    });
    click(cx, "action-icon");
    tab_to(&app, cx, "icon-chat");
    keys(cx, "escape");
    assert!(control_focused(&app, cx, "action-icon"));
    keys(cx, "enter");
    tab_to(&app, cx, "icon-chat");
    keys(cx, "enter");
    assert!(control_focused(&app, cx, "action-icon"));
    click(cx, "action-icon");
    click(cx, "back");
    cx.read_entity(&app, |app, _| assert!(app.actions.dialog.is_none()));
    click(cx, "back");
    cx.read_entity(&app, |app, _| assert!(app.actions.dialog.is_some()));
    keys(cx, "escape");
    cx.read_entity(&app, |app, cx| {
        assert_eq!(
            app.actions
                .editor
                .as_ref()
                .unwrap()
                .name
                .read(cx)
                .content
                .as_ref(),
            "Dirty"
        )
    });
}

#[gpui::test]
fn busy_capture_activation_and_rejected_callback_preserve_confirmation_focus(
    cx: &mut TestAppContext,
) {
    let (app, backend, cx) = setup(cx);
    click(cx, "chat");
    input(cx, "Pending");
    keys(cx, "enter");
    // Closing Chat leaves its request running.
    keys(cx, "escape");
    menu(cx, NewQuickAction);
    fill(cx, "Unsaved", "Draft");
    keys(cx, "escape");
    let focus =
        on_surface(&app, cx, Surface::Settings).update(|window, cx| window.focused(cx).unwrap());
    // A finished capture brings up the launcher in its own window.
    cx.update(|window, cx| {
        app.update(cx, |app, cx| {
            app.capture_scope = Some(app.session.scope());
            app.finish_capture(Ok("New capture".into()), None, window, cx);
        })
    });
    cx.run_until_parked();
    assert!(cx.read_entity(&app, |app, _| app.hud_visible));
    on_surface(&app, cx, Surface::Settings).update(|window, _| assert!(focus.is_focused(window)));
    input(cx, "Must not reach draft");
    let (tx, rx) = mpsc::channel();
    tx.send(zeroize::Zeroizing::new(
        "wiesel://auth/callback?invalid".into(),
    ))
    .unwrap();
    cx.update(|window, cx| {
        app.update(cx, |app, cx| {
            app.authenticated = false;
            app.urls = rx;
            app.receive_callbacks(window, cx);
        })
    });
    cx.run_until_parked();
    on_surface(&app, cx, Surface::Settings).update(|window, _| assert!(focus.is_focused(window)));
    cx.read_entity(&app, |app, cx| {
        assert_eq!(app.current_screen(), Screen::ActionEditor);
        assert!(app.actions.dialog.is_some());
        assert_eq!(
            app.actions
                .editor
                .as_ref()
                .unwrap()
                .name
                .read(cx)
                .content
                .as_ref(),
            "Unsaved"
        );
        assert_eq!(
            app.actions
                .editor
                .as_ref()
                .unwrap()
                .instructions
                .read(cx)
                .content
                .as_ref(),
            "Draft"
        );
    });
    assert_eq!(backend.request_count(), 1);
    backend.finish(0, Ok("Reply".into()));
    receive_results(&app, cx);
    on_surface(&app, cx, Surface::Settings).simulate_keystrokes("escape");
    cx.run_until_parked();
    cx.read_entity(&app, |app, _| {
        assert_eq!(app.current_screen(), Screen::ActionEditor);
        assert!(app.actions.dialog.is_none());
    });
}

#[gpui::test]
fn injected_paste_action_preserves_multiline_prompt_without_clipboard_reads(
    cx: &mut TestAppContext,
) {
    let (app, backend, cx) = setup(cx);
    cx.update(|_, cx| cx.bind_keys([KeyBinding::new("cmd-v", input::Paste, Some("TextInput"))]));
    menu(cx, NewQuickAction);
    cx.update(|_, cx| {
        let editor = app.read(cx).actions.editor.as_ref().unwrap();
        let name = editor.name.clone();
        let instructions = editor.instructions.clone();
        name.update(cx, |input, _| {
            input.inject_paste_reader(|| Some("Pasted\nname".into()))
        });
        instructions.update(cx, |input, _| {
            input.inject_paste_reader(|| Some("First rule\r\nSecond rule\nThird".into()))
        });
    });
    keys(cx, "cmd-v");
    click(cx, "action-instructions");
    keys(cx, "cmd-v cmd-enter");
    let id = cx.read_entity(&app, |app, _| {
        let action = &app.settings.custom_actions[0];
        assert_eq!(action.name, "Pasted name");
        assert_eq!(action.instructions, "First rule\nSecond rule\nThird");
        action.id
    });
    cx.update(|_, cx| {
        app.update(cx, |app, cx| {
            app.selection.text = Some("Source".into());
            load_models(app, vec!["test/chat-model".into()], cx);
            cx.notify();
        })
    });
    cx.run_until_parked();
    run_tile(&app, cx, &format!("custom-{id}"));
    assert_eq!(
        backend.request(0).1[1].content,
        "First rule\nSecond rule\nThird"
    );
}

#[gpui::test]
fn held_model_activation_does_not_reopen_or_reset_selected_override(cx: &mut TestAppContext) {
    let (app, _, cx) = setup(cx);
    let original = action("Override", Some("test/override"));
    let id = original.id;
    seed(&app, cx, original);
    menu(cx, AdjustQuickActions);
    click(cx, &format!("edit-{id}"));
    click(cx, "model-select");
    keys(cx, "down down down");
    held_key(cx, "down");
    key_event(cx, "enter", false);
    held_key(cx, "enter");
    held_key(cx, "enter");
    held_key(cx, "space");
    key_window(cx).simulate_event(gpui::KeyUpEvent {
        keystroke: gpui::Keystroke::parse("enter").unwrap(),
    });
    assert!(bounds(cx, "model-option-0").is_none());
    click(cx, "action-save");
    cx.read_entity(&app, |app, _| {
        assert_eq!(
            app.settings.custom_actions[0].model_override,
            Some("test/override".into())
        )
    });
}

#[gpui::test]
fn scoped_catalog_result_refreshes_open_editor_behind_the_launcher_without_request(
    cx: &mut TestAppContext,
) {
    let (app, backend, cx) = setup(cx);
    menu(cx, NewQuickAction);
    fill(cx, "Retained", "Instructions");
    // The shortcut brings up the launcher; the editor stays open in its own window.
    cx.update(|window, cx| app.update(cx, |app, cx| app.activate_from_hotkey(window, cx)));
    cx.run_until_parked();
    cx.update(|_, cx| {
        app.update(cx, |app, _| {
            app.tx
                .send(ResultEvent {
                    scope: app.session.scope(),
                    result: ResultPayload::Models(Ok(vec!["test/new-catalog".into()])),
                })
                .unwrap()
        })
    });
    receive_results(&app, cx);
    cx.read_entity(&app, |app, cx| {
        assert!(app.hud_visible);
        assert_eq!(app.current_screen(), Screen::ActionEditor);
        assert_eq!(
            app.actions
                .editor
                .as_ref()
                .unwrap()
                .name
                .read(cx)
                .content
                .as_ref(),
            "Retained"
        );
        assert_eq!(
            app.actions
                .editor
                .as_ref()
                .unwrap()
                .model
                .read(cx)
                .wiesel
                .models,
            vec!["test/new-catalog"]
        );
    });
    on_surface(&app, cx, Surface::Settings).update(|window, _| window.activate_window());
    click(cx, "model-select");
    input(cx, "new-catalog");
    keys(cx, "enter");
    click(cx, "action-save");
    cx.read_entity(&app, |app, _| {
        assert_eq!(
            app.settings.custom_actions[0].model_override,
            Some("test/new-catalog".into())
        )
    });
    assert_eq!(backend.request_count(), 0);
}

#[gpui::test]
fn manager_scroll_restore_and_tab_reveals_trailing_controls(cx: &mut TestAppContext) {
    let (app, _, cx) = setup(cx);
    cx.update(|_, cx| {
        app.update(cx, |app, cx| {
            app.settings.custom_actions = (0..20)
                .map(|n| action(&format!("Action {n}"), None))
                .collect();
            cx.notify();
        })
    });
    cx.run_until_parked();
    menu(cx, AdjustQuickActions);
    wheel(cx, "action-manager-scroll", -180.);
    let manager_offset = cx.read_entity(&app, |app, _| app.actions.manager_scroll.offset());
    assert!(manager_offset.y < px(0.));
    click(cx, "new-action");
    keys(cx, "escape");
    assert_eq!(
        cx.read_entity(&app, |app, _| app.actions.manager_scroll.offset()),
        manager_offset
    );
    let last = cx.read_entity(&app, |app, _| {
        app.settings.custom_actions.last().unwrap().id
    });
    tab_to(&app, cx, &format!("delete-{last}"));
    let delete = bounds(cx, Box::leak(format!("delete-{last}").into_boxed_str())).unwrap();
    let viewport = bounds(cx, "action-manager-scroll").unwrap();
    assert!(delete.top() >= viewport.top() && delete.bottom() <= viewport.bottom());
    let lower_offset = cx.read_entity(&app, |app, _| app.actions.manager_scroll.offset());
    assert_ne!(lower_offset, manager_offset);
    keys(cx, "shift-tab enter escape");
    assert!(control_focused(&app, cx, &format!("edit-{last}")));
    assert_eq!(
        cx.read_entity(&app, |app, _| app.actions.manager_scroll.offset()),
        lower_offset
    );
    keys(cx, "enter");
    cx.read_entity(&app, |app, _| {
        assert_eq!(app.current_screen(), Screen::ActionEditor)
    });
    click(cx, "action-save");
    assert!(control_focused(&app, cx, &format!("edit-{last}")));
    assert_eq!(
        cx.read_entity(&app, |app, _| app.actions.manager_scroll.offset()),
        lower_offset
    );
}

#[gpui::test]
fn editor_scroll_survives_a_menu_command_and_tab_reveals_instructions(cx: &mut TestAppContext) {
    let (app, _, cx) = setup(cx);
    menu(cx, NewQuickAction);
    // The window fits the whole editor; shorten it so the editor overflows
    // by more than the wheel distance even after the model popup closes.
    on_surface(&app, cx, Surface::Settings).simulate_resize(size(px(600.), px(370.)));
    click(cx, "model-select");
    wheel(cx, "action-editor-scroll", -110.);
    let offset = cx.read_entity(&app, |app, _| {
        app.actions.editor.as_ref().unwrap().scroll.offset()
    });
    assert!(offset.y < px(0.));
    // New Quick Action keeps the open editor. Like every menu command it first
    // closes the open popup, restoring focus to the retained model trigger.
    menu(cx, NewQuickAction);
    assert_eq!(
        cx.read_entity(&app, |app, _| app
            .actions
            .editor
            .as_ref()
            .unwrap()
            .scroll
            .offset()),
        offset
    );
    assert!(bounds(cx, "model-option-0").is_none());
    // Tab out of the retained model trigger to the instructions field.
    keys(cx, "tab");
    on_surface(&app, cx, Surface::Settings).update(|window, cx| {
        assert!(
            app.read(cx)
                .actions
                .editor
                .as_ref()
                .unwrap()
                .instructions
                .focus_handle(cx)
                .is_focused(window)
        )
    });
    cx.run_until_parked();
    let field = bounds(cx, "action-instructions").unwrap();
    let viewport = bounds(cx, "action-editor-scroll").unwrap();
    assert!(field.top() >= viewport.top() && field.bottom() <= viewport.bottom());
    input(cx, "Visible after traversal");
}

/// ⌘W on the Settings window; a dirty editor asks before the window closes.
fn close_settings(app: &Entity<Wiesel>, cx: &mut VisualTestContext) {
    on_surface(app, cx, Surface::Settings).dispatch_action(CloseWindow);
    cx.run_until_parked();
}

#[gpui::test]
fn close_discard_confirmation_closes_popup_before_owning_and_restoring_focus(
    cx: &mut TestAppContext,
) {
    let (app, _, cx) = setup(cx);
    menu(cx, NewQuickAction);
    fill(cx, "Dirty", "Draft");
    click(cx, "model-select");
    close_settings(&app, cx);
    let dialog_focus =
        on_surface(&app, cx, Surface::Settings).update(|window, cx| window.focused(cx).unwrap());
    let mut settings = mouse_press(cx, "dialog-cancel");
    settings.update(|window, _| assert!(dialog_focus.is_focused(window)));
    mouse_release(&mut settings, "dialog-cancel");
    keys(cx, "enter");
    assert!(bounds(cx, "model-option-0").is_some());
    cx.read_entity(&app, |app, _| {
        assert!(app.is_open(Surface::Settings));
        assert_eq!(app.current_screen(), Screen::ActionEditor)
    });
}

#[gpui::test]
fn close_discard_closes_both_keyboard_opened_popups_before_cancel_owns_focus(
    cx: &mut TestAppContext,
) {
    let (app, backend, cx) = setup(cx);
    menu(cx, NewQuickAction);
    fill(cx, "Dirty", "Draft");
    click(cx, "action-name");
    // No outside mouse gesture between opening Model and Icon: both really
    // remain open, exercising the modal invariant rather than single dismissal.
    keys(cx, "tab enter");
    assert!(bounds(cx, "model-option-0").is_some());
    tab_to(&app, cx, "action-icon");
    keys(cx, "enter");
    assert!(bounds(cx, "icon-wand").is_some());
    assert!(bounds(cx, "model-option-0").is_some());
    close_settings(&app, cx);
    assert!(bounds(cx, "icon-wand").is_none());
    assert!(bounds(cx, "model-option-0").is_none());
    cx.read_entity(&app, |app, _| assert!(app.actions.dialog.is_some()));
    let mut settings = on_surface(&app, cx, Surface::Settings);
    let cancel_focus = settings.update(|window, cx| window.focused(cx).unwrap());
    let backdrop = point(px(10.), px(10.));
    settings.simulate_mouse_down(backdrop, MouseButton::Left, Modifiers::none());
    settings.update(|window, cx| {
        assert_eq!(window.focused(cx).as_ref(), Some(&cancel_focus));
        assert!(cancel_focus.is_focused(window));
    });
    settings.simulate_mouse_up(backdrop, MouseButton::Left, Modifiers::none());
    keys(cx, "enter");
    // Enter must activate safe Cancel, not reopen Model or discard the editor.
    assert!(bounds(cx, "model-option-0").is_none());
    assert!(bounds(cx, "icon-wand").is_none());
    cx.read_entity(&app, |app, cx| {
        assert!(app.actions.dialog.is_none());
        assert!(app.is_open(Surface::Settings));
        assert_eq!(app.current_screen(), Screen::ActionEditor);
        assert_eq!(app.navigation.depth(), 2);
        assert!(app.settings.custom_actions.is_empty());
        let editor = app.actions.editor.as_ref().unwrap();
        assert_eq!(editor.name.read(cx).content.as_ref(), "Dirty");
        assert_eq!(editor.instructions.read(cx).content.as_ref(), "Draft");
    });
    assert_eq!(backend.request_count(), 0);
}

#[gpui::test]
fn ordinary_escape_dismisses_one_of_two_popups_before_dirty_back_confirmation(
    cx: &mut TestAppContext,
) {
    let (app, _, cx) = setup(cx);
    menu(cx, NewQuickAction);
    fill(cx, "Dirty", "Draft");
    click(cx, "action-name");
    keys(cx, "tab enter");
    tab_to(&app, cx, "action-icon");
    keys(cx, "enter");
    assert!(bounds(cx, "icon-wand").is_some());
    assert!(bounds(cx, "model-option-0").is_some());
    keys(cx, "escape");
    assert!(bounds(cx, "icon-wand").is_none());
    assert!(bounds(cx, "model-option-0").is_some());
    assert!(control_focused(&app, cx, "action-icon"));
    cx.read_entity(&app, |app, _| {
        assert_eq!(app.current_screen(), Screen::ActionEditor);
        assert!(app.actions.dialog.is_none());
    });
    keys(cx, "escape");
    assert!(bounds(cx, "model-option-0").is_none());
    cx.read_entity(&app, |app, _| assert!(app.actions.dialog.is_none()));
    click(cx, "back");
    cx.read_entity(&app, |app, _| assert!(app.actions.dialog.is_some()));
    keys(cx, "escape");
    cx.read_entity(&app, |app, cx| {
        assert_eq!(app.current_screen(), Screen::ActionEditor);
        assert_eq!(
            app.actions
                .editor
                .as_ref()
                .unwrap()
                .name
                .read(cx)
                .content
                .as_ref(),
            "Dirty"
        );
    });
}

#[gpui::test]
fn deleting_a_launcher_action_removes_its_tile_without_touching_launcher_focus(
    cx: &mut TestAppContext,
) {
    let (app, backend, cx) = setup(cx);
    let original = action("Delete from launcher", None);
    let id = original.id;
    let tile: &'static str = Box::leak(format!("custom-{id}").into_boxed_str());
    seed(&app, cx, original);
    assert!(bounds(cx, tile).is_some());
    menu(cx, AdjustQuickActions);
    click(cx, &format!("delete-{id}"));
    click(cx, "dialog-confirm");
    // Focus moves to a mounted control of the Settings window; the launcher keeps its own.
    assert!(control_focused(&app, cx, "new-action"));
    assert!(bounds(cx, tile).is_none());
    click(cx, "back");
    cx.update(|window, cx| assert!(app.read(cx).focus.is_focused(window)));
    cx.read_entity(&app, |app, _| {
        assert_eq!(app.current_screen(), Screen::Settings);
        assert!(app.settings.custom_actions.is_empty());
    });
    keys(cx, "escape");
    assert!(!is_open(&app, cx, Surface::Settings));
    hud_keys(&app, cx, "escape");
    cx.read_entity(&app, |app, _| assert_eq!(app.recorded_hides, Some(1)));
    assert_eq!(backend.request_count(), 0);
}

#[gpui::test]
fn canceled_or_failed_delete_keeps_the_launcher_tile_runnable(cx: &mut TestAppContext) {
    let (app, backend, cx) = setup(cx);
    let original = action("Surviving launcher tile", None);
    let id = original.id;
    let tile: &'static str = Box::leak(format!("custom-{id}").into_boxed_str());
    seed(&app, cx, original);
    menu(cx, AdjustQuickActions);
    click(cx, &format!("delete-{id}"));
    click(cx, "dialog-cancel");
    assert!(control_focused(&app, cx, &format!("delete-{id}")));
    cx.update(|_, cx| {
        app.update(cx, |app, _| {
            app.actions.persist = Some(Box::new(|_| Err(settings::SettingsError::Write)));
        })
    });
    click(cx, &format!("delete-{id}"));
    click(cx, "dialog-confirm");
    assert!(control_focused(&app, cx, &format!("delete-{id}")));
    assert!(bounds(cx, "action-error").is_some());
    click(cx, "back");
    assert!(bounds(cx, tile).is_some());
    hud_keys(&app, cx, "6");
    assert_eq!(backend.request_count(), 1);
    cx.read_entity(&app, |app, _| {
        assert!(app.is_open(Surface::Chat));
        assert_eq!(app.settings.custom_actions[0].id, id);
    });
    backend.finish(0, Ok("Controlled result".into()));
    receive_results(&app, cx);
    assert_eq!(
        cx.read_entity(&app, |app, _| app.messages.last().unwrap().content.clone()),
        "Controlled result"
    );
}

#[gpui::test]
fn editor_offers_tiers_then_models_from_both_providers(cx: &mut TestAppContext) {
    let (app, backend, cx) = setup(cx);
    cx.update(|_, cx| {
        app.update(cx, |app, cx| {
            load_models(app, vec!["test/chat-model".into()], cx);
            app.update_catalogs(Provider::Ollama, cx, |catalog| {
                catalog.models = vec!["qwen3.5:0.8b".into()];
            });
            app.settings
                .models
                .set(Tier::Swift, Some(ModelChoice::ollama("qwen3.5:0.8b")));
            app.selection.text = Some("Source".into());
        })
    });
    cx.run_until_parked();
    menu(cx, NewQuickAction);
    fill(cx, "Quick", "Answer briefly.");
    click(cx, "model-select");
    click(cx, "model-option-0");
    click(cx, "action-save");
    let id = cx.read_entity(&app, |app, _| {
        let action = &app.settings.custom_actions[0];
        assert_eq!(action.model_override, Some(ActionModel::Tier(Tier::Swift)));
        action.id
    });
    // The tier resolves at dispatch time to the model assigned in Settings.
    run_tile(&app, cx, &format!("custom-{id}"));
    assert!(backend.local(0));
    assert_eq!(backend.request(0).0, "qwen3.5:0.8b");
    backend.finish(0, Ok("Done".into()));
    receive_results(&app, cx);

    keys(cx, "escape");
    menu(cx, AdjustQuickActions);
    click(cx, &format!("edit-{id}"));
    click(cx, "model-select");
    input(cx, "qwen");
    keys(cx, "enter");
    click(cx, "action-save");
    cx.read_entity(&app, |app, _| {
        assert_eq!(
            app.settings.custom_actions[0].model_override,
            Some(ActionModel::Model(ModelChoice::ollama("qwen3.5:0.8b")))
        );
    });
}

fn paste_action(name: &str) -> CustomQuickAction {
    let mut action = action(name, None);
    action.paste_result = true;
    action
}
fn set_source(app: &Entity<Wiesel>, cx: &mut VisualTestContext, source: Option<i32>) {
    cx.update(|_, cx| app.update(cx, |app, _| app.selection.source = source));
}
fn pastes(app: &Entity<Wiesel>, cx: &mut VisualTestContext) -> Vec<(i32, String)> {
    cx.read_entity(app, |app, _| app.recorded_pastes.clone().unwrap())
}

#[gpui::test]
fn editor_paste_toggle_saves_and_reverting_it_is_not_dirty(cx: &mut TestAppContext) {
    let (app, _, cx) = setup(cx);
    let id = QuickAction::FixSpelling.persistent_id();
    menu(cx, AdjustQuickActions);
    click(cx, &format!("edit-{id}"));
    click(cx, "action-paste");
    cx.read_entity(&app, |app, cx| {
        assert!(app.actions.editor.as_ref().unwrap().dirty(cx));
    });
    click(cx, "action-paste");
    cx.read_entity(&app, |app, cx| {
        assert!(!app.actions.editor.as_ref().unwrap().dirty(cx));
    });
    click(cx, "action-paste");
    click(cx, "action-save");
    cx.read_entity(&app, |app, _| {
        assert!(app.settings.actions()[0].paste_result);
        assert!(!app.settings.actions()[1].paste_result);
    });
    // The built-in Chat is always a conversation and offers no toggle.
    click(cx, &format!("edit-{}", QuickAction::Chat.persistent_id()));
    assert!(bounds(cx, "action-paste").is_none());
}

#[gpui::test]
fn paste_action_spins_on_its_launcher_tile_then_pastes_into_the_source_app(
    cx: &mut TestAppContext,
) {
    let (app, backend, cx) = setup(cx);
    let custom = paste_action("Shout");
    let id = custom.id;
    seed(&app, cx, custom);
    set_source(&app, cx, Some(42));
    click(cx, &format!("custom-{id}"));
    let (_, messages, chat) = backend.request(0);
    assert!(!chat);
    assert!(messages[1].content.contains("Selected source text"));
    cx.run_until_parked();
    assert!(bounds(cx, "home-spinner-5").is_some());
    cx.read_entity(&app, |app, _| {
        assert!(app.hud_visible, "the launcher stays up until pasting");
        assert!(!app.is_open(Surface::Chat) && !app.is_open(Surface::Writing));
        assert_eq!(app.recorded_hides, Some(0));
    });
    // A second press while working sends nothing.
    click(cx, &format!("custom-{id}"));
    assert_eq!(backend.request_count(), 1);
    backend.finish(0, Ok("SELECTED SOURCE TEXT".into()));
    receive_results(&app, cx);
    cx.run_until_parked();
    assert_eq!(pastes(&app, cx), [(42, "SELECTED SOURCE TEXT".into())]);
    assert!(bounds(cx, "home-spinner-5").is_none());
    cx.read_entity(&app, |app, _| {
        assert!(!app.is_open(Surface::Chat) && !app.is_open(Surface::Writing));
        assert_eq!(app.recorded_hides, Some(1));
        assert!(app.messages.is_empty());
        // One paste per capture: the replaced selection cannot be pasted over again.
        assert!(app.selection.source.is_none());
        assert!(app.direct_paste.is_none());
    });
    click(cx, &format!("custom-{id}"));
    assert_eq!(backend.request_count(), 1);
}

#[gpui::test]
fn builtin_paste_action_skips_the_writing_window(cx: &mut TestAppContext) {
    let (app, backend, cx) = setup(cx);
    cx.update(|_, cx| {
        app.update(cx, |app, _| {
            let mut actions = app.settings.actions();
            actions[0].paste_result = true;
            app.settings.builtin_actions = Some(actions);
            app.selection.text = Some("teh text".into());
            app.selection.source = Some(7);
        })
    });
    click(cx, "grammar");
    backend.finish(0, Ok("the text".into()));
    receive_results(&app, cx);
    assert_eq!(pastes(&app, cx), [(7, "the text".into())]);
    assert!(!is_open(&app, cx, Surface::Writing));
}

#[gpui::test]
fn paste_action_without_a_source_app_or_with_a_failed_request_pastes_nothing(
    cx: &mut TestAppContext,
) {
    let (app, backend, cx) = setup(cx);
    let custom = paste_action("Shout");
    let id = custom.id;
    seed(&app, cx, custom);
    click(cx, &format!("custom-{id}"));
    assert_eq!(backend.request_count(), 0);
    set_source(&app, cx, Some(42));
    click(cx, &format!("custom-{id}"));
    backend.finish(0, Err(anyhow::anyhow!("offline")));
    receive_results(&app, cx);
    // Empty output must not erase the selection either.
    click(cx, &format!("custom-{id}"));
    backend.finish(1, Ok("  \n".into()));
    receive_results(&app, cx);
    assert!(pastes(&app, cx).is_empty());
    cx.read_entity(&app, |app, _| {
        assert_eq!(app.recorded_hides, Some(0));
        assert!(app.direct_paste.is_none());
        assert!(!app.busy);
        assert_eq!(app.selection.source, Some(42));
    });
}

#[gpui::test]
fn icon_picker_searches_every_lucide_icon_by_name_and_tag(cx: &mut TestAppContext) {
    let (app, backend, cx) = setup(cx);
    menu(cx, NewQuickAction);
    fill(cx, "Launch", "Go {selection}");
    click(cx, "action-icon");
    // Opening focuses the search field, so typing filters immediately.
    input(cx, "rocket");
    assert!(bounds(cx, "icon-rocket").is_some());
    assert!(bounds(cx, "icon-wand").is_none());
    keys(cx, "enter");
    assert!(bounds(cx, "icon-search").is_none());
    assert!(control_focused(&app, cx, "action-icon"));
    cx.read_entity(&app, |app, cx| {
        assert_eq!(
            app.actions.editor.as_ref().unwrap().fields(cx).icon,
            "rocket"
        );
    });
    // Tags match too: "heart rate" is a tag of `activity`, not part of its name.
    click(cx, "action-icon");
    input(cx, "heart rate monitor");
    click(cx, "icon-activity");
    // Reopening clears the search; arrow keys move through the grid by rows.
    click(cx, "action-icon");
    assert!(bounds(cx, "icon-wand").is_some());
    input(cx, "arrow");
    keys(cx, "down down up enter");
    let expected = action_builder::icon_options("arrow")[action_builder::ICON_COLUMNS];
    cx.read_entity(&app, |app, cx| {
        assert_eq!(
            app.actions.editor.as_ref().unwrap().fields(cx).icon,
            expected
        );
    });
    click(cx, "action-icon");
    input(cx, "heart rate monitor");
    click(cx, "icon-activity");
    click(cx, "action-icon");
    input(cx, "zzzz");
    assert!(bounds(cx, "icon-rocket").is_none());
    keys(cx, "enter");
    assert!(bounds(cx, "icon-search").is_some());
    keys(cx, "escape");
    click(cx, "action-save");
    cx.read_entity(&app, |app, _| {
        assert_eq!(app.settings.custom_actions[0].icon, "activity");
    });
    assert_eq!(backend.request_count(), 0);
}

#[gpui::test]
fn selection_actions_are_disabled_until_text_is_captured(cx: &mut TestAppContext) {
    let (app, backend, cx) = setup(cx);
    let custom = action("Needs text", None);
    let id = custom.id;
    seed(&app, cx, custom);
    cx.update(|_, cx| {
        app.update(cx, |app, cx| {
            app.selection.text = Some("  ".into());
            cx.notify();
        })
    });
    cx.read_entity(&app, |app, _| {
        let actions = app.home_actions();
        let tile = actions.iter().find(|a| a.action_id() == id).unwrap();
        assert!(tile.needs_selection());
        assert!(!app.home_action_enabled(tile));
        // Chat never reads the selection and stays available.
        let chat = actions
            .iter()
            .find(|a| a.action_id() == QuickAction::Chat.persistent_id())
            .unwrap();
        assert!(app.home_action_enabled(chat));
    });
    // Neither the pointer nor the printed number runs a disabled tile.
    run_tile(&app, cx, &format!("custom-{id}"));
    hud_keys(&app, cx, "6");
    assert_eq!(backend.request_count(), 0);
    cx.update(|_, cx| {
        app.update(cx, |app, cx| {
            app.selection.text = Some("Selected".into());
            cx.notify();
        })
    });
    run_tile(&app, cx, &format!("custom-{id}"));
    assert_eq!(backend.request_count(), 1);
}
