//! Real in-process UI input with isolated settings persistence and ControlledBackend.
use super::*;
use chat_tests::{receive_results, setup};
use gpui::{Modifiers, TestAppContext, VisualTestContext};
use quick_actions::{CustomActionFields, CustomQuickAction};
use std::{cell::RefCell, rc::Rc};

fn click(cx: &mut VisualTestContext, selector: &str) {
    // GPUI debug selectors require static lifetimes; these tiny test-only IDs
    // live for the test process, not application state.
    let selector = Box::leak(selector.to_owned().into_boxed_str());
    let bounds = cx
        .debug_bounds(selector)
        .unwrap_or_else(|| panic!("rendered {selector}"));
    cx.simulate_click(bounds.center(), Modifiers::none());
}
fn fill(cx: &mut VisualTestContext, name: &str, instructions: &str) {
    click(cx, "action-name");
    cx.simulate_keystrokes("cmd-a");
    cx.simulate_input(name);
    click(cx, "action-instructions");
    cx.simulate_keystrokes("cmd-a");
    cx.simulate_input(instructions);
}
fn create(cx: &mut VisualTestContext, name: &str, instructions: &str) {
    click(cx, "add-action");
    fill(cx, name, instructions);
    click(cx, "action-save");
}
fn action(name: &str, model: Option<&str>) -> CustomQuickAction {
    CustomQuickAction::new(CustomActionFields {
        name: name.into(),
        icon: "wand".into(),
        model_override: model.map(str::to_owned),
        instructions: "Exact custom instructions\n with whitespace ".into(),
    })
    .unwrap()
}
fn seed(app: &Entity<Wiesel>, cx: &mut VisualTestContext, action: CustomQuickAction) {
    cx.update(|_, cx| {
        app.update(cx, |app, cx| {
            app.settings.custom_actions.push(action);
            app.model_picker.update(cx, |picker, cx| {
                picker.finish_load(
                    Ok(vec!["test/chat-model".into(), "test/override".into()]),
                    cx,
                )
            });
            app.selection.text = Some("Selected source text".into());
            cx.notify();
        })
    });
    cx.run_until_parked();
}

#[gpui::test]
fn create_icon_model_multiline_and_edit_preserve_uuid_and_home_draft(cx: &mut TestAppContext) {
    let (app, backend, cx) = setup(cx);
    cx.simulate_input("retained Home draft");
    cx.update(|_, cx| {
        app.update(cx, |app, cx| {
            app.model_picker.update(cx, |picker, cx| {
                picker.finish_load(
                    Ok(vec!["test/chat-model".into(), "test/override".into()]),
                    cx,
                )
            })
        })
    });
    click(cx, "add-action");
    fill(cx, "  My action  ", "First line");
    cx.simulate_keystrokes("enter");
    cx.simulate_input("Second line");
    click(cx, "action-icon");
    click(cx, "icon-lightbulb");
    click(cx, "model-select");
    click(cx, "model-option-2");
    cx.simulate_keystrokes("cmd-enter");
    let id = cx.read_entity(&app, |app, cx| {
        assert_eq!(app.current_screen(), Screen::Launcher);
        assert_eq!(app.settings.custom_actions.len(), 1);
        let action = &app.settings.custom_actions[0];
        assert_eq!(action.name, "My action");
        assert_eq!(action.icon, "lightbulb");
        assert_eq!(action.instructions, "First line\nSecond line");
        assert_eq!(action.model_override.as_deref(), Some("test/override"));
        assert_eq!(
            app.launcher_input.read(cx).content.as_ref(),
            "retained Home draft"
        );
        action.id
    });
    assert_eq!(backend.request_count(), 0);
    click(cx, "manage-actions");
    click(cx, &format!("edit-{id}"));
    fill(cx, "Renamed", "Updated instructions");
    click(cx, "model-select");
    click(cx, "model-option-0");
    click(cx, "action-save");
    cx.read_entity(&app, |app, _| {
        assert_eq!(app.current_screen(), Screen::ActionManager);
        assert_eq!(app.settings.custom_actions[0].id, id);
        assert_eq!(app.settings.custom_actions[0].name, "Renamed");
        assert!(app.settings.custom_actions[0].model_override.is_none());
    });
    cx.simulate_keystrokes("escape");
    cx.update(|window, cx| {
        assert!(
            app.read(cx)
                .launcher_input
                .focus_handle(cx)
                .is_focused(window)
        )
    });
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
    click(cx, "add-action");
    click(cx, "action-save");
    assert!(cx.debug_bounds("action-error").is_some());
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
    assert!(cx.debug_bounds("action-error").is_some());
    click(cx, "action-save");
    assert_eq!(attempts.borrow().len(), 2);
    assert_eq!(attempts.borrow()[0], attempts.borrow()[1]);
    cx.read_entity(&app, |app, _| {
        assert_eq!(app.settings.custom_actions.len(), 1);
        assert_eq!(app.current_screen(), Screen::Launcher);
    });
    assert_eq!(backend.request_count(), 0);
}

#[gpui::test]
fn delete_cancel_confirm_and_failed_update_delete_are_transactional(cx: &mut TestAppContext) {
    let (app, _, cx) = setup(cx);
    let original = action("Original", None);
    let id = original.id;
    seed(&app, cx, original.clone());
    click(cx, "manage-actions");
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
    assert!(cx.debug_bounds("action-error").is_some());
    cx.update(|_, cx| {
        app.update(cx, |app, _| {
            app.actions.persist = Some(Box::new(|_| Ok(())))
        })
    });
    click(cx, &format!("delete-{id}"));
    cx.simulate_keystrokes("tab enter");
    cx.read_entity(&app, |app, _| {
        assert!(app.settings.custom_actions.is_empty());
        assert_eq!(app.current_screen(), Screen::ActionManager);
    });
}

#[gpui::test]
fn dirty_escape_back_cancel_and_popup_precedence_preserve_draft(cx: &mut TestAppContext) {
    let (app, _, cx) = setup(cx);
    click(cx, "add-action");
    fill(cx, "Dirty name", "Draft");
    click(cx, "action-icon");
    cx.simulate_keystrokes("escape");
    cx.read_entity(&app, |app, _| {
        assert_eq!(app.current_screen(), Screen::ActionEditor);
        assert!(app.actions.dialog.is_none());
    });
    click(cx, "model-select");
    cx.simulate_keystrokes("escape");
    cx.read_entity(&app, |app, _| assert!(app.actions.dialog.is_none()));
    cx.simulate_keystrokes("escape");
    cx.simulate_keystrokes("escape");
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
        assert_eq!(app.current_screen(), Screen::Launcher);
        assert!(app.settings.custom_actions.is_empty());
        assert!(app.actions.editor.is_none());
    });
}

#[gpui::test]
fn manager_new_cancel_and_settings_cover_restore_nested_editor(cx: &mut TestAppContext) {
    let (app, _, cx) = setup(cx);
    click(cx, "manage-actions");
    click(cx, "new-action");
    click(cx, "action-cancel");
    cx.read_entity(&app, |app, _| {
        assert_eq!(app.current_screen(), Screen::ActionManager)
    });
    click(cx, "new-action");
    fill(cx, "Nested", "Draft");
    click(cx, "settings");
    cx.read_entity(&app, |app, _| assert_eq!(app.navigation.depth(), 4));
    click(cx, "back");
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
        assert_eq!(app.current_screen(), Screen::ActionManager)
    });
    click(cx, "back");
    cx.read_entity(&app, |app, _| {
        assert_eq!(app.current_screen(), Screen::Launcher)
    });
}

#[gpui::test]
fn custom_dispatch_uses_exact_model_instructions_selection_and_stale_guards(
    cx: &mut TestAppContext,
) {
    let (app, backend, cx) = setup(cx);
    let custom = action("Custom", Some("test/override"));
    let id = custom.id;
    seed(&app, cx, custom.clone());
    click(cx, &format!("custom-{id}"));
    let (model, messages, chat) = backend.request(0);
    assert_eq!(model, "test/override");
    assert!(!chat);
    assert_eq!(messages[0].role, "system");
    assert_eq!(messages[0].content, custom.instructions);
    assert_eq!(messages[1].role, "user");
    assert_eq!(messages[1].content, "Selected source text");
    cx.read_entity(&app, |app, _| {
        assert_eq!(app.settings.model, "test/chat-model");
        assert_eq!(app.selection.writing_source, "Selected source text");
    });
    click(cx, "settings");
    backend.finish(0, Ok("Finished custom result".into()));
    receive_results(&app, cx);
    cx.read_entity(&app, |app, _| {
        assert_eq!(app.current_screen(), Screen::Settings);
        assert_eq!(app.result, "Finished custom result");
    });
    click(cx, "back");
    click(cx, "back");
    click(cx, &format!("custom-{id}"));
    click(cx, "back");
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
        assert_eq!(app.current_screen(), Screen::Launcher);
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
    click(cx, "back");
    cx.update(|_, cx| {
        app.update(cx, |app, cx| {
            app.settings.custom_actions[0].model_override = Some("test/removed".into());
            cx.notify();
        })
    });
    click(cx, &format!("custom-{id}"));
    assert_eq!(backend.request_count(), 1);
    cx.read_entity(&app, |app, _| {
        assert_eq!(app.current_screen(), Screen::Launcher);
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
    click(cx, "manage-actions");
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
    click(cx, "add-action");
    cx.simulate_input("fircse");
    cx.simulate_keystrokes("tab");
    cx.simulate_keystrokes("enter escape tab");
    cx.simulate_input("Instructions via Tab");
    cx.simulate_keystrokes("enter");
    cx.simulate_input("Line two");
    cx.simulate_keystrokes("cmd-enter");
    cx.read_entity(&app, |app, _| {
        assert_eq!(app.current_screen(), Screen::Launcher);
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
    click(cx, "manage-actions");
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
    click(cx, "add-action");
    fill(cx, "Must not overwrite", "Private draft");
    click(cx, "action-save");
    assert_eq!(std::fs::read(&path).unwrap(), invalid);
    cx.read_entity(&app, |app, _| {
        assert!(app.actions.read_only);
        assert!(app.settings.custom_actions.is_empty());
        assert_eq!(app.current_screen(), Screen::ActionEditor);
    });
    assert!(cx.debug_bounds("action-error").is_some());
}

#[gpui::test]
fn dirty_hotkey_root_and_existing_settings_multi_pop_are_atomic_until_confirmed(
    cx: &mut TestAppContext,
) {
    let (app, _, cx) = setup(cx);
    click(cx, "manage-actions");
    click(cx, "new-action");
    fill(cx, "Retain me", "Draft");
    cx.update(|window, cx| app.update(cx, |app, cx| app.activate_from_hotkey(window, cx)));
    cx.run_until_parked();
    cx.read_entity(&app, |app, _| {
        assert_eq!(app.navigation.depth(), 3);
        assert!(app.actions.dialog.is_some());
    });
    click(cx, "dialog-cancel");
    // Exercise the same singleton-Settings preflight seam when Settings is below
    // an editor, without login/catalog/OS effects.
    cx.update(|window, cx| {
        app.update(cx, |app, cx| {
            app.push_settings(false, window, cx);
            app.push_screen(Screen::Chat, false, window, cx);
        })
    });
    cx.run_until_parked();
    click(cx, "settings");
    cx.read_entity(&app, |app, _| {
        assert_eq!(app.current_screen(), Screen::Settings)
    });
    click(cx, "back");
    cx.read_entity(&app, |app, _| {
        assert_eq!(app.current_screen(), Screen::ActionEditor)
    });
    cx.update(|window, cx| app.update(cx, |app, cx| app.pop_to_root(window, cx)));
    cx.run_until_parked();
    click(cx, "dialog-confirm");
    cx.read_entity(&app, |app, _| {
        assert_eq!(app.current_screen(), Screen::Launcher);
        assert_eq!(app.navigation.depth(), 1);
        assert!(app.actions.editor.is_none());
    });
    // Settings below a newly mounted dirty editor: revealing it must guard all
    // entries before the first pop.
    click(cx, "settings");
    cx.update(|window, cx| app.update(cx, |app, cx| app.open_action_editor(None, window, cx)));
    cx.run_until_parked();
    fill(cx, "Nested", "Draft");
    click(cx, "settings");
    cx.read_entity(&app, |app, _| {
        assert_eq!(app.current_screen(), Screen::ActionEditor);
        assert_eq!(app.navigation.depth(), 3);
        assert!(app.actions.dialog.is_some());
    });
    click(cx, "dialog-cancel");
    click(cx, "settings");
    click(cx, "dialog-confirm");
    cx.read_entity(&app, |app, _| {
        assert_eq!(app.current_screen(), Screen::Settings);
        assert_eq!(app.navigation.depth(), 2);
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
    click(cx, "back");
    click(cx, &format!("custom-{id}"));
    assert_eq!(backend.request_count(), 1);
    click(cx, "manage-actions");
    click(cx, &format!("edit-{id}"));
    fill(cx, "Edited during request", "Updated for next run");
    click(cx, "action-save");
    backend.finish(0, Ok("Older completed result".into()));
    receive_results(&app, cx);
    cx.read_entity(&app, |app, _| {
        assert_eq!(app.current_screen(), Screen::ActionManager);
        assert_eq!(app.settings.custom_actions[0].name, "Edited during request");
        assert_eq!(app.result, "Older completed result");
        assert_eq!(app.selection.writing_source, "Selected source text");
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
        assert_eq!(app.current_screen(), Screen::Settings)
    });
    click(cx, "back");
    cx.update(|_, cx| {
        app.update(cx, |app, cx| {
            app.authenticated = true;
            app.session.credential = None;
            cx.notify();
        })
    });
    click(cx, &format!("custom-{id}"));
    assert_eq!(backend.request_count(), 0);
    cx.read_entity(&app, |app, _| assert!(!app.busy));
}

#[gpui::test]
fn home_overflow_preserves_three_column_cards_fixed_footer_and_reachable_create(
    cx: &mut TestAppContext,
) {
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
    let footer = cx.debug_bounds("notifications-footer").unwrap();
    assert_eq!(footer.size.height, px(48.));
    assert_eq!(footer.bottom(), px(449.));
    let first = cx.debug_bounds("grammar").unwrap();
    let second = cx.debug_bounds("improve").unwrap();
    let third = cx.debug_bounds("chat").unwrap();
    assert!(((first.size.width - second.size.width) / px(1.)).abs() <= 1.);
    assert!(((second.size.width - third.size.width) / px(1.)).abs() <= 1.);
    assert!(first.left() < second.left() && second.left() < third.left());
    click(cx, "home-new-action");
    fill(cx, "Accessible overflow add", "Instructions");
    click(cx, "action-save");
    cx.read_entity(&app, |app, _| {
        assert_eq!(app.settings.custom_actions.len(), 17)
    });
    click(cx, "manage-actions");
    assert_eq!(
        cx.debug_bounds("notifications-footer").unwrap().size.height,
        px(48.)
    );
    click(cx, "new-action");
    click(cx, "action-cancel");
    cx.read_entity(&app, |app, _| {
        assert_eq!(app.current_screen(), Screen::ActionManager)
    });
}

#[gpui::test]
fn repeated_save_shortcut_cannot_send_retained_home_draft(cx: &mut TestAppContext) {
    let (app, backend, cx) = setup(cx);
    cx.simulate_input("Private Home draft");
    click(cx, "add-action");
    fill(cx, "Once", "Instructions");
    cx.simulate_keystrokes("cmd-enter cmd-enter cmd-enter");
    cx.read_entity(&app, |app, cx| {
        assert_eq!(app.current_screen(), Screen::Launcher);
        assert_eq!(app.settings.custom_actions.len(), 1);
        assert_eq!(
            app.launcher_input.read(cx).content.as_ref(),
            "Private Home draft"
        );
    });
    assert_eq!(backend.request_count(), 0);
}

#[gpui::test]
fn login_shortcut_cannot_bypass_dirty_editor_or_confirmation_focus(cx: &mut TestAppContext) {
    let (app, backend, cx) = setup(cx);
    click(cx, "settings");
    cx.update(|window, cx| app.update(cx, |app, cx| app.open_action_editor(None, window, cx)));
    cx.run_until_parked();
    fill(cx, "Private draft", "Instructions");
    cx.update(|window, cx| app.update(cx, |app, cx| app.start_login(window, cx)));
    cx.run_until_parked();
    cx.read_entity(&app, |app, _| {
        assert_eq!(app.current_screen(), Screen::ActionEditor);
        assert!(app.actions.dialog.is_some());
        assert!(app.session.attempt.is_none());
    });
    cx.update(|window, cx| {
        let focused = window.focused(cx);
        app.update(cx, |app, cx| app.start_login(window, cx));
        assert_eq!(focused, window.focused(cx));
    });
    click(cx, "dialog-cancel");
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
            "Private draft"
        )
    });
    assert_eq!(backend.request_count(), 0);
}

fn control_focused(app: &Entity<Wiesel>, cx: &mut VisualTestContext, id: &str) -> bool {
    cx.update(|window, cx| app.read(cx).action_control_focus(id, cx).is_focused(window))
}
fn tab_to(app: &Entity<Wiesel>, cx: &mut VisualTestContext, id: &str) {
    for _ in 0..100 {
        if control_focused(app, cx, id) {
            return;
        }
        cx.simulate_keystrokes("tab");
    }
    panic!("Tab did not reach {id}");
}
fn wheel(cx: &mut VisualTestContext, selector: &'static str, delta: f32) {
    let bounds = cx.debug_bounds(selector).unwrap();
    cx.simulate_event(ScrollWheelEvent {
        position: bounds.center(),
        delta: ScrollDelta::Pixels(point(px(0.), px(delta))),
        modifiers: Modifiers::none(),
        touch_phase: gpui::TouchPhase::Moved,
    });
    cx.run_until_parked();
}
fn held_key(cx: &mut VisualTestContext, key: &str) {
    cx.simulate_event(KeyDownEvent {
        keystroke: gpui::Keystroke::parse(key).unwrap(),
        is_held: true,
        prefer_character_input: false,
    });
}

#[gpui::test]
fn retained_home_manager_new_and_edit_focus_reactivate_with_enter_after_pop(
    cx: &mut TestAppContext,
) {
    let (app, _, cx) = setup(cx);
    let custom = action("Saved", None);
    let id = custom.id;
    seed(&app, cx, custom);
    tab_to(&app, cx, "manage-actions");
    cx.simulate_keystrokes("enter escape");
    assert!(control_focused(&app, cx, "manage-actions"));
    cx.simulate_keystrokes("enter");
    cx.read_entity(&app, |app, _| {
        assert_eq!(app.current_screen(), Screen::ActionManager)
    });
    tab_to(&app, cx, "new-action");
    cx.simulate_keystrokes("enter escape");
    assert!(control_focused(&app, cx, "new-action"));
    cx.simulate_keystrokes("enter escape");
    tab_to(&app, cx, &format!("edit-{id}"));
    cx.simulate_keystrokes("enter escape");
    assert!(control_focused(&app, cx, &format!("edit-{id}")));
    cx.simulate_keystrokes("enter");
    cx.read_entity(&app, |app, _| {
        assert_eq!(app.current_screen(), Screen::ActionEditor)
    });
    cx.simulate_keystrokes("escape escape");
    tab_to(&app, cx, "chat");
    cx.simulate_keystrokes("enter escape");
    assert!(control_focused(&app, cx, "chat"));
    cx.simulate_keystrokes("enter");
    cx.read_entity(&app, |app, _| {
        assert_eq!(app.current_screen(), Screen::Chat)
    });
}

#[gpui::test]
fn delete_cancel_failure_and_success_restore_only_mounted_controls(cx: &mut TestAppContext) {
    let (app, _, cx) = setup(cx);
    let custom = action("Delete target", None);
    let id = custom.id;
    seed(&app, cx, custom);
    click(cx, "manage-actions");
    tab_to(&app, cx, &format!("delete-{id}"));
    cx.simulate_keystrokes("enter escape");
    assert!(control_focused(&app, cx, &format!("delete-{id}")));
    cx.simulate_keystrokes("enter");
    cx.update(|_, cx| {
        app.update(cx, |app, _| {
            app.actions.persist = Some(Box::new(|_| Err(settings::SettingsError::Write)))
        })
    });
    cx.simulate_keystrokes("tab enter");
    assert!(control_focused(&app, cx, &format!("delete-{id}")));
    cx.update(|_, cx| {
        app.update(cx, |app, _| {
            app.actions.persist = Some(Box::new(|_| Ok(())))
        })
    });
    cx.simulate_keystrokes("enter tab enter");
    assert!(control_focused(&app, cx, "new-action"));
    cx.simulate_keystrokes("enter");
    cx.read_entity(&app, |app, _| {
        assert_eq!(app.current_screen(), Screen::ActionEditor);
        assert!(app.settings.custom_actions.is_empty());
    });
    cx.simulate_keystrokes("escape escape");
    cx.read_entity(&app, |app, _| {
        assert_eq!(app.current_screen(), Screen::Launcher)
    });
}

#[gpui::test]
fn clean_editor_back_gesture_only_dismisses_model_or_icon_popup(cx: &mut TestAppContext) {
    let (app, _, cx) = setup(cx);
    click(cx, "add-action");
    click(cx, "model-select");
    let back = cx.debug_bounds("back").unwrap();
    cx.simulate_mouse_down(back.center(), MouseButton::Left, Modifiers::none());
    cx.simulate_mouse_up(back.center(), MouseButton::Left, Modifiers::none());
    cx.read_entity(&app, |app, _| {
        assert_eq!(app.current_screen(), Screen::ActionEditor);
        assert!(app.actions.dialog.is_none());
    });
    cx.simulate_keystrokes("enter");
    assert!(cx.debug_bounds("model-option-0").is_some());
    cx.simulate_keystrokes("escape");
    click(cx, "action-icon");
    click(cx, "back");
    assert!(control_focused(&app, cx, "action-icon"));
    cx.simulate_keystrokes("enter");
    assert!(cx.debug_bounds("icon-wand").is_some());
    cx.simulate_keystrokes("escape");
    click(cx, "back");
    cx.read_entity(&app, |app, _| {
        assert_eq!(app.current_screen(), Screen::Launcher)
    });
}

#[gpui::test]
fn dirty_editor_back_gesture_dismisses_popup_before_discard_confirmation(cx: &mut TestAppContext) {
    let (app, _, cx) = setup(cx);
    click(cx, "add-action");
    fill(cx, "Dirty", "Draft");
    click(cx, "model-select");
    click(cx, "back");
    cx.read_entity(&app, |app, _| {
        assert_eq!(app.current_screen(), Screen::ActionEditor);
        assert!(app.actions.dialog.is_none());
    });
    click(cx, "action-icon");
    tab_to(&app, cx, "icon-chat");
    cx.simulate_keystrokes("escape");
    assert!(control_focused(&app, cx, "action-icon"));
    cx.simulate_keystrokes("enter");
    tab_to(&app, cx, "icon-chat");
    cx.simulate_keystrokes("enter");
    assert!(control_focused(&app, cx, "action-icon"));
    click(cx, "action-icon");
    click(cx, "back");
    cx.read_entity(&app, |app, _| assert!(app.actions.dialog.is_none()));
    click(cx, "back");
    cx.read_entity(&app, |app, _| assert!(app.actions.dialog.is_some()));
    cx.simulate_keystrokes("escape");
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
    cx.simulate_input("Pending");
    cx.simulate_keystrokes("enter");
    click(cx, "back");
    click(cx, "add-action");
    fill(cx, "Unsaved", "Draft");
    cx.simulate_keystrokes("escape");
    let focus = cx.update(|window, cx| window.focused(cx).unwrap());
    cx.update(|window, cx| {
        app.update(cx, |app, cx| {
            app.capture_scope = Some(app.session.scope());
            app.finish_capture(Ok("New capture".into()), window, cx);
        })
    });
    cx.run_until_parked();
    cx.update(|window, _| assert!(focus.is_focused(window)));
    cx.simulate_input("Must not reach draft");
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
    cx.update(|window, _| assert!(focus.is_focused(window)));
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
    cx.simulate_keystrokes("escape");
    cx.read_entity(&app, |app, _| {
        assert_eq!(app.current_screen(), Screen::ActionEditor)
    });
}

#[gpui::test]
fn injected_paste_action_preserves_multiline_prompt_without_clipboard_reads(
    cx: &mut TestAppContext,
) {
    let (app, backend, cx) = setup(cx);
    cx.update(|_, cx| cx.bind_keys([KeyBinding::new("cmd-v", input::Paste, Some("TextInput"))]));
    click(cx, "add-action");
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
    cx.simulate_keystrokes("cmd-v");
    click(cx, "action-instructions");
    cx.simulate_keystrokes("cmd-v cmd-enter");
    let id = cx.read_entity(&app, |app, _| {
        let action = &app.settings.custom_actions[0];
        assert_eq!(action.name, "Pasted name");
        assert_eq!(action.instructions, "First rule\nSecond rule\nThird");
        action.id
    });
    cx.update(|_, cx| {
        app.update(cx, |app, cx| {
            app.selection.text = Some("Source".into());
            app.model_picker.update(cx, |picker, cx| {
                picker.finish_load(Ok(vec!["test/chat-model".into()]), cx)
            });
            cx.notify();
        })
    });
    cx.run_until_parked();
    click(cx, &format!("custom-{id}"));
    assert_eq!(
        backend.request(0).1[0].content,
        "First rule\nSecond rule\nThird"
    );
}

#[gpui::test]
fn held_model_activation_does_not_reopen_or_reset_selected_override(cx: &mut TestAppContext) {
    let (app, _, cx) = setup(cx);
    let original = action("Override", Some("test/override"));
    let id = original.id;
    seed(&app, cx, original);
    click(cx, "manage-actions");
    click(cx, &format!("edit-{id}"));
    click(cx, "model-select");
    cx.simulate_keystrokes("down");
    held_key(cx, "down");
    cx.simulate_event(KeyDownEvent {
        keystroke: gpui::Keystroke::parse("enter").unwrap(),
        is_held: false,
        prefer_character_input: false,
    });
    held_key(cx, "enter");
    held_key(cx, "enter");
    held_key(cx, "space");
    cx.simulate_event(gpui::KeyUpEvent {
        keystroke: gpui::Keystroke::parse("enter").unwrap(),
    });
    assert!(cx.debug_bounds("model-option-0").is_none());
    click(cx, "action-save");
    cx.read_entity(&app, |app, _| {
        assert_eq!(
            app.settings.custom_actions[0].model_override.as_deref(),
            Some("test/override")
        )
    });
}

#[gpui::test]
fn scoped_catalog_result_refreshes_retained_editor_without_request(cx: &mut TestAppContext) {
    let (app, backend, cx) = setup(cx);
    click(cx, "add-action");
    fill(cx, "Retained", "Instructions");
    click(cx, "settings");
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
        assert_eq!(app.current_screen(), Screen::Settings);
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
            app.actions.editor.as_ref().unwrap().model.read(cx).models,
            vec!["test/new-catalog"]
        );
    });
    click(cx, "back");
    click(cx, "model-select");
    click(cx, "model-option-1");
    click(cx, "action-save");
    cx.read_entity(&app, |app, _| {
        assert_eq!(
            app.settings.custom_actions[0].model_override.as_deref(),
            Some("test/new-catalog")
        )
    });
    assert_eq!(backend.request_count(), 0);
}

#[gpui::test]
fn home_and_manager_scroll_restore_and_tab_reveals_trailing_controls(cx: &mut TestAppContext) {
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
    wheel(cx, "quick-actions-scroll", -160.);
    let home_offset = cx.read_entity(&app, |app, _| app.actions.home_scroll.offset());
    assert!(home_offset.y < px(0.));
    click(cx, "manage-actions");
    wheel(cx, "action-manager-scroll", -180.);
    let manager_offset = cx.read_entity(&app, |app, _| app.actions.manager_scroll.offset());
    assert!(manager_offset.y < px(0.));
    click(cx, "new-action");
    cx.simulate_keystrokes("escape");
    assert_eq!(
        cx.read_entity(&app, |app, _| app.actions.manager_scroll.offset()),
        manager_offset
    );
    cx.simulate_keystrokes("escape");
    assert_eq!(
        cx.read_entity(&app, |app, _| app.actions.home_scroll.offset()),
        home_offset
    );
    let last = cx.read_entity(&app, |app, _| {
        app.settings.custom_actions.last().unwrap().id
    });
    tab_to(&app, cx, &format!("custom-{last}"));
    let card = cx
        .debug_bounds(Box::leak(format!("custom-{last}").into_boxed_str()))
        .unwrap();
    let viewport = cx.debug_bounds("quick-actions-scroll").unwrap();
    assert!(card.top() >= viewport.top() && card.bottom() <= viewport.bottom());
    let lower_home = cx.read_entity(&app, |app, _| app.actions.home_scroll.offset());
    cx.simulate_keystrokes("tab enter escape");
    assert!(control_focused(&app, cx, "add-action"));
    assert_eq!(
        cx.read_entity(&app, |app, _| app.actions.home_scroll.offset()),
        lower_home
    );
    cx.simulate_keystrokes("enter escape");
    click(cx, "manage-actions");
    tab_to(&app, cx, &format!("delete-{last}"));
    let delete = cx
        .debug_bounds(Box::leak(format!("delete-{last}").into_boxed_str()))
        .unwrap();
    let viewport = cx.debug_bounds("action-manager-scroll").unwrap();
    assert!(delete.top() >= viewport.top() && delete.bottom() <= viewport.bottom());
    let lower_offset = cx.read_entity(&app, |app, _| app.actions.manager_scroll.offset());
    cx.simulate_keystrokes("shift-tab enter escape");
    assert!(control_focused(&app, cx, &format!("edit-{last}")));
    assert_eq!(
        cx.read_entity(&app, |app, _| app.actions.manager_scroll.offset()),
        lower_offset
    );
    cx.simulate_keystrokes("enter");
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
fn editor_outer_scroll_survives_settings_cover_and_tab_reveals_instructions(
    cx: &mut TestAppContext,
) {
    let (app, _, cx) = setup(cx);
    click(cx, "add-action");
    click(cx, "model-select");
    wheel(cx, "action-editor-scroll", -110.);
    let offset = cx.read_entity(&app, |app, _| {
        app.actions.editor.as_ref().unwrap().scroll.offset()
    });
    assert!(offset.y < px(0.));
    click(cx, "settings");
    click(cx, "back");
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
    cx.simulate_keystrokes("escape");
    // Tab out of the retained model trigger to the instructions field.
    cx.simulate_keystrokes("tab");
    cx.update(|window, cx| {
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
    let field = cx.debug_bounds("action-instructions").unwrap();
    let viewport = cx.debug_bounds("action-editor-scroll").unwrap();
    assert!(field.top() >= viewport.top() && field.bottom() <= viewport.bottom());
    cx.simulate_input("Visible after traversal");
}

#[gpui::test]
fn root_discard_confirmation_closes_popup_before_owning_and_restoring_focus(
    cx: &mut TestAppContext,
) {
    let (app, _, cx) = setup(cx);
    click(cx, "add-action");
    fill(cx, "Dirty", "Draft");
    click(cx, "model-select");
    cx.update(|window, cx| app.update(cx, |app, cx| app.activate_from_hotkey(window, cx)));
    cx.run_until_parked();
    let dialog_focus = cx.update(|window, cx| window.focused(cx).unwrap());
    let cancel = cx.debug_bounds("dialog-cancel").unwrap();
    cx.simulate_mouse_down(cancel.center(), MouseButton::Left, Modifiers::none());
    cx.update(|window, _| assert!(dialog_focus.is_focused(window)));
    cx.simulate_mouse_up(cancel.center(), MouseButton::Left, Modifiers::none());
    cx.simulate_keystrokes("enter");
    assert!(cx.debug_bounds("model-option-0").is_some());
    cx.read_entity(&app, |app, _| {
        assert_eq!(app.current_screen(), Screen::ActionEditor)
    });
}

#[gpui::test]
fn root_discard_closes_both_keyboard_opened_popups_before_cancel_owns_focus(
    cx: &mut TestAppContext,
) {
    let (app, backend, cx) = setup(cx);
    click(cx, "add-action");
    fill(cx, "Dirty", "Draft");
    click(cx, "action-name");
    // No outside mouse gesture between opening Model and Icon: both really
    // remain open, exercising the modal invariant rather than single dismissal.
    cx.simulate_keystrokes("tab enter");
    assert!(cx.debug_bounds("model-option-0").is_some());
    tab_to(&app, cx, "action-icon");
    cx.simulate_keystrokes("enter");
    assert!(cx.debug_bounds("icon-wand").is_some());
    assert!(cx.debug_bounds("model-option-0").is_some());
    cx.update(|window, cx| app.update(cx, |app, cx| app.activate_from_hotkey(window, cx)));
    cx.run_until_parked();
    assert!(cx.debug_bounds("icon-wand").is_none());
    assert!(cx.debug_bounds("model-option-0").is_none());
    cx.read_entity(&app, |app, _| assert!(app.actions.dialog.is_some()));
    let cancel_focus = cx.update(|window, cx| window.focused(cx).unwrap());
    let backdrop = point(px(10.), px(10.));
    cx.simulate_mouse_down(backdrop, MouseButton::Left, Modifiers::none());
    cx.update(|window, cx| {
        assert_eq!(window.focused(cx).as_ref(), Some(&cancel_focus));
        assert!(cancel_focus.is_focused(window));
    });
    cx.simulate_mouse_up(backdrop, MouseButton::Left, Modifiers::none());
    cx.simulate_keystrokes("enter");
    // Enter must activate safe Cancel, not reopen Model or discard the editor.
    assert!(cx.debug_bounds("model-option-0").is_none());
    assert!(cx.debug_bounds("icon-wand").is_none());
    cx.read_entity(&app, |app, cx| {
        assert!(app.actions.dialog.is_none());
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
    click(cx, "add-action");
    fill(cx, "Dirty", "Draft");
    click(cx, "action-name");
    cx.simulate_keystrokes("tab enter");
    tab_to(&app, cx, "action-icon");
    cx.simulate_keystrokes("enter");
    assert!(cx.debug_bounds("icon-wand").is_some());
    assert!(cx.debug_bounds("model-option-0").is_some());
    cx.simulate_keystrokes("escape");
    assert!(cx.debug_bounds("icon-wand").is_none());
    assert!(cx.debug_bounds("model-option-0").is_some());
    assert!(control_focused(&app, cx, "action-icon"));
    cx.read_entity(&app, |app, _| {
        assert_eq!(app.current_screen(), Screen::ActionEditor);
        assert!(app.actions.dialog.is_none());
    });
    cx.simulate_keystrokes("escape");
    assert!(cx.debug_bounds("model-option-0").is_none());
    cx.read_entity(&app, |app, _| assert!(app.actions.dialog.is_none()));
    click(cx, "back");
    cx.read_entity(&app, |app, _| assert!(app.actions.dialog.is_some()));
    cx.simulate_keystrokes("escape");
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
fn successful_delete_redirects_saved_home_card_restore_to_mounted_launcher(
    cx: &mut TestAppContext,
) {
    let (app, backend, cx) = setup(cx);
    cx.simulate_input("Retained Home draft");
    let original = action("Delete from Home", None);
    let id = original.id;
    seed(&app, cx, original);
    tab_to(&app, cx, &format!("custom-{id}"));
    assert!(control_focused(&app, cx, &format!("custom-{id}")));
    // Mouse Manage deliberately preserves the card focus in its pop lifecycle.
    click(cx, "manage-actions");
    click(cx, &format!("delete-{id}"));
    click(cx, "dialog-confirm");
    click(cx, "back");
    cx.update(|window, cx| {
        assert!(
            app.read(cx)
                .launcher_input
                .focus_handle(cx)
                .is_focused(window)
        );
    });
    cx.read_entity(&app, |app, cx| {
        assert_eq!(app.current_screen(), Screen::Launcher);
        assert!(app.settings.custom_actions.is_empty());
        assert_eq!(
            app.launcher_input.read(cx).content.as_ref(),
            "Retained Home draft"
        );
    });
    tab_to(&app, cx, "manage-actions");
    cx.simulate_keystrokes("enter escape");
    assert!(control_focused(&app, cx, "manage-actions"));
    cx.simulate_keystrokes("escape");
    cx.read_entity(&app, |app, _| assert_eq!(app.recorded_hides, Some(1)));
    assert_eq!(backend.request_count(), 0);
}

#[gpui::test]
fn canceled_or_failed_delete_preserves_saved_home_card_restore_and_activation(
    cx: &mut TestAppContext,
) {
    let (app, backend, cx) = setup(cx);
    let original = action("Surviving Home card", None);
    let id = original.id;
    seed(&app, cx, original);
    tab_to(&app, cx, &format!("custom-{id}"));
    click(cx, "manage-actions");
    click(cx, &format!("delete-{id}"));
    click(cx, "dialog-cancel");
    click(cx, "back");
    assert!(control_focused(&app, cx, &format!("custom-{id}")));
    click(cx, "manage-actions");
    cx.update(|_, cx| {
        app.update(cx, |app, _| {
            app.actions.persist = Some(Box::new(|_| Err(settings::SettingsError::Write)));
        })
    });
    click(cx, &format!("delete-{id}"));
    click(cx, "dialog-confirm");
    click(cx, "back");
    assert!(control_focused(&app, cx, &format!("custom-{id}")));
    cx.simulate_keystrokes("enter");
    assert_eq!(backend.request_count(), 1);
    cx.read_entity(&app, |app, _| {
        assert_eq!(app.current_screen(), Screen::Writing);
        assert_eq!(app.settings.custom_actions[0].id, id);
    });
    backend.finish(0, Ok("Controlled result".into()));
    receive_results(&app, cx);
    cx.simulate_keystrokes("escape");
    assert!(control_focused(&app, cx, &format!("custom-{id}")));
}
