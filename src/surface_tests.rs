//! The launcher HUD and its secondary windows (Settings, Chat, Writing), driven through
//! real in-process clicks and keystrokes.
use super::*;
use gpui::TestAppContext;
use test_support::*;

const BUILTIN_TILES: [&str; 5] = ["grammar", "improve", "chat", "summarize", "explain"];

fn custom(name: &str) -> quick_actions::CustomQuickAction {
    quick_actions::CustomQuickAction::new(quick_actions::CustomActionFields {
        name: name.into(),
        icon: "wand".into(),
        model_override: None,
        instructions: "Say hi".into(),
        paste_result: false,
    })
    .unwrap()
}

#[gpui::test]
fn launcher_shows_configured_actions_in_one_row_without_an_add_tile(cx: &mut TestAppContext) {
    let (_app, _, cx) = setup(cx);
    let tiles: Vec<_> = BUILTIN_TILES
        .iter()
        .map(|id| bounds(cx, id).unwrap_or_else(|| panic!("tile {id}")))
        .collect();
    for pair in tiles.windows(2) {
        assert_eq!(pair[0].top(), pair[1].top(), "one row");
        assert_eq!(pair[0].size, pair[1].size, "equal tiles");
        assert!(pair[0].right() < pair[1].left(), "left to right");
    }
    assert!(bounds(cx, "add-action").is_none());
    assert!(bounds(cx, "status-bar").is_some());
    assert!(bounds(cx, "open-settings").is_some());
}

#[gpui::test]
fn launcher_size_setting_limits_tiles_and_sizes_the_window(cx: &mut TestAppContext) {
    let (app, _, cx) = setup(cx);
    cx.update(|_, cx| {
        app.update(cx, |app, cx| {
            for name in ["One", "Two", "Three", "Four"] {
                app.settings.custom_actions.push(custom(name));
            }
            cx.notify();
        })
    });
    for (count, shown) in [(3, 3), (5, 5), (8, 8)] {
        cx.update(|_, cx| {
            app.update(cx, |app, cx| {
                app.settings.quick_action_count = count;
                cx.notify();
            })
        });
        cx.run_until_parked();
        assert_eq!(
            cx.read_entity(&app, |app, _| app.home_actions().len()),
            shown
        );
        let rendered = (0..9)
            .filter(|slot| {
                cx.read_entity(&app, |app, _| {
                    app.home_actions()
                        .get(*slot)
                        .map(|action| action.presentation().0)
                })
                .is_some()
            })
            .count();
        assert_eq!(rendered, shown);
        assert!(hud_size(8).width > hud_size(5).width);
        assert!(hud_size(5).width > hud_size(3).width);
    }
    // Four custom actions follow the five built-ins; eight hides the last one.
    assert!(cx.read_entity(&app, |app, _| app.settings.actions().len()) == 9);
    assert!(bounds(cx, "explain").is_some());
}

#[gpui::test]
fn settings_launcher_size_control_persists_the_choice(cx: &mut TestAppContext) {
    let (app, _, cx) = setup(cx);
    let saved = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
    let record = saved.clone();
    cx.update(|_, cx| {
        app.update(cx, |app, _| {
            app.actions.persist = Some(Box::new(move |next| {
                record.borrow_mut().push(next.quick_action_count);
                Ok(())
            }));
        })
    });
    click(cx, "open-settings");
    assert!(cx.read_entity(&app, |app, _| app.is_open(Surface::Settings)));
    click(cx, "quick-action-count-3");
    assert_eq!(saved.borrow().as_slice(), [3]);
    assert_eq!(cx.read_entity(&app, |app, _| app.home_actions().len()), 3);
    assert!(bounds(cx, "summarize").is_none());
    click(cx, "quick-action-count-5");
    assert_eq!(saved.borrow().as_slice(), [3, 5]);
    assert!(bounds(cx, "summarize").is_some());
}

#[gpui::test]
fn number_keys_and_enter_run_tiles_and_chat_opens_its_own_window(cx: &mut TestAppContext) {
    let (app, backend, cx) = setup(cx);
    keys(cx, "3");
    cx.read_entity(&app, |app, _| {
        assert!(app.is_open(Surface::Chat));
        assert!(!app.hud_visible, "the launcher steps aside like cmd+tab");
        assert_eq!(app.hud_selected, 2);
    });
    input(cx, "Hello window");
    keys(cx, "enter");
    let (_, messages, chat) = backend.request(0);
    assert!(chat);
    assert_eq!(messages.last().unwrap().content, "Hello window");
    // The chat window, not the launcher, hosts the conversation.
    let mut chat_window = on_surface(&app, cx, Surface::Chat);
    assert!(chat_window.debug_bounds("send").is_some());
    assert!(bounds(cx, "grammar").is_some(), "launcher stays mounted");
}

#[gpui::test]
fn arrows_move_the_highlight_and_enter_runs_it(cx: &mut TestAppContext) {
    let (app, _, cx) = setup(cx);
    keys(cx, "right right");
    assert_eq!(cx.read_entity(&app, |app, _| app.hud_selected), 2);
    keys(cx, "left");
    assert_eq!(cx.read_entity(&app, |app, _| app.hud_selected), 1);
    keys(cx, "left left");
    assert_eq!(cx.read_entity(&app, |app, _| app.hud_selected), 4, "wraps");
    keys(cx, "right right right");
    assert_eq!(cx.read_entity(&app, |app, _| app.hud_selected), 2);
    keys(cx, "enter");
    assert!(cx.read_entity(&app, |app, _| app.is_open(Surface::Chat)));
}

#[gpui::test]
fn escape_closes_chat_and_reopening_keeps_history(cx: &mut TestAppContext) {
    let (app, backend, cx) = setup(cx);
    click(cx, "chat");
    input(cx, "Remember me");
    keys(cx, "enter");
    backend.finish(0, Ok("Remembered".into()));
    receive_results(&app, cx);
    keys(cx, "escape");
    cx.run_until_parked();
    assert!(!cx.read_entity(&app, |app, _| app.is_open(Surface::Chat)));
    cx.update(|window, cx| app.update(cx, |app, cx| app.show_hud(window, cx)));
    click(cx, "chat");
    cx.read_entity(&app, |app, _| {
        assert!(app.is_open(Surface::Chat));
        assert_eq!(app.messages.len(), 2);
    });
}

#[gpui::test]
fn launcher_escape_returns_to_the_previous_app_or_to_wiesels_open_windows(cx: &mut TestAppContext) {
    let (app, _, cx) = setup(cx);
    keys(cx, "escape");
    assert_eq!(cx.read_entity(&app, |app, _| app.recorded_hides), Some(1));
    click(cx, "open-settings");
    cx.update(|window, cx| app.update(cx, |app, cx| app.show_hud(window, cx)));
    cx.update(|window, _| window.activate_window());
    cx.run_until_parked();
    let mut hud = windows(cx).remove(0);
    hud.simulate_keystrokes("escape");
    cx.run_until_parked();
    cx.read_entity(&app, |app, _| {
        assert_eq!(app.recorded_hides, Some(1), "Settings stays reachable");
        assert!(!app.hud_visible);
        assert!(app.is_open(Surface::Settings));
    });
}

#[gpui::test]
fn settings_opens_in_its_own_window_and_escape_closes_it(cx: &mut TestAppContext) {
    let (app, _, cx) = setup(cx);
    menu(cx, OpenSettings);
    cx.read_entity(&app, |app, _| {
        assert!(app.is_open(Surface::Settings));
        assert_eq!(app.current_screen(), Screen::Settings);
    });
    let mut settings = on_surface(&app, cx, Surface::Settings);
    assert!(settings.debug_bounds("quick-action-count-8").is_some());
    assert!(bounds(cx, "grammar").is_some(), "the launcher is untouched");
    keys(cx, "escape");
    assert!(!cx.read_entity(&app, |app, _| app.is_open(Surface::Settings)));
}

#[gpui::test]
fn menu_adds_actions_in_the_settings_window_and_closing_confirms_a_dirty_draft(
    cx: &mut TestAppContext,
) {
    let (app, _, cx) = setup(cx);
    menu(cx, NewQuickAction);
    cx.read_entity(&app, |app, _| {
        assert!(app.is_open(Surface::Settings));
        assert_eq!(app.current_screen(), Screen::ActionEditor);
    });
    click(cx, "action-name");
    input(cx, "Draft");
    // ⌘W asks before discarding; Keep editing leaves the window open.
    on_surface(&app, cx, Surface::Settings).dispatch_action(CloseWindow);
    cx.run_until_parked();
    assert!(cx.read_entity(&app, |app, _| app.actions.dialog.is_some()));
    click(cx, "dialog-cancel");
    cx.read_entity(&app, |app, _| {
        assert!(app.is_open(Surface::Settings));
        assert_eq!(app.current_screen(), Screen::ActionEditor);
    });
    on_surface(&app, cx, Surface::Settings).dispatch_action(CloseWindow);
    cx.run_until_parked();
    click(cx, "dialog-confirm");
    cx.read_entity(&app, |app, _| {
        assert!(!app.is_open(Surface::Settings));
        assert!(app.actions.editor.is_none());
        assert!(app.navigation.is_root());
    });
}

#[gpui::test]
fn adjust_quick_actions_reveals_the_manager_with_back_to_settings(cx: &mut TestAppContext) {
    let (app, _, cx) = setup(cx);
    menu(cx, AdjustQuickActions);
    assert_eq!(
        cx.read_entity(&app, |app, _| app.current_screen()),
        Screen::ActionManager
    );
    click(cx, "back");
    assert_eq!(
        cx.read_entity(&app, |app, _| app.current_screen()),
        Screen::Settings
    );
    click(cx, "manage-actions");
    assert_eq!(
        cx.read_entity(&app, |app, _| app.current_screen()),
        Screen::ActionManager
    );
    // Settings… from the menu reveals the root instead of stacking another Settings.
    menu(cx, OpenSettings);
    cx.read_entity(&app, |app, _| {
        assert_eq!(app.current_screen(), Screen::Settings);
        assert_eq!(app.navigation.depth(), 1);
    });
}

#[gpui::test]
fn writing_results_open_in_their_own_window(cx: &mut TestAppContext) {
    let (app, backend, cx) = setup(cx);
    cx.update(|_, cx| {
        app.update(cx, |app, _| {
            app.selection.text = Some("teh text".into());
        })
    });
    keys(cx, "1");
    assert!(cx.read_entity(&app, |app, _| app.is_open(Surface::Writing)));
    backend.finish(0, Ok("the text".into()));
    receive_results(&app, cx);
    assert_eq!(
        cx.read_entity(&app, |app, _| app.result.clone()),
        "the text"
    );
    let mut writing = on_surface(&app, cx, Surface::Writing);
    assert!(writing.debug_bounds("copy-result").is_some());
}
