#[cfg(not(target_os = "macos"))]
compile_error!("Wiesel currently supports macOS only (native selection and Keychain backend).");

mod clipboard_capture;
mod gateway;
mod input;
mod model_picker;
mod notifications;
mod quick_actions;
mod selection;
mod settings;
mod theme;

use anyhow::Context as _;
use gateway::Message;
use global_hotkey::{GlobalHotKeyEvent, GlobalHotKeyManager, HotKeyState, hotkey::HotKey};
use gpui::{KeyBinding, prelude::*, *};
use input::TextInput;
use notifications::{Notification, Notifications, Severity, Source};
use quick_actions::{QUICK_ACTIONS, QuickAction};
use settings::Settings;
use std::{
    str::FromStr,
    sync::mpsc,
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

#[derive(Clone, Copy, PartialEq)]
enum Page {
    Setup,
    Launcher,
    Chat,
    Writing,
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

enum ResultEvent {
    Auth(anyhow::Result<()>, String),
    Models(anyhow::Result<Vec<String>>),
    ChatDelta(String),
    Completion(anyhow::Result<String>, bool),
}
struct Wiesel {
    page: Page,
    settings: Settings,
    manager: Option<GlobalHotKeyManager>,
    hotkey: Option<HotKey>,
    hotkey_input: Entity<TextInput>,
    key_input: Entity<TextInput>,
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
    failed_chat: std::collections::VecDeque<String>,
    result: String,
    writing_title: String,
    messages: Vec<Message>,
    tx: mpsc::Sender<ResultEvent>,
    rx: mpsc::Receiver<ResultEvent>,
    chat_scroll: ScrollHandle,
    chat_follow_bottom: bool,
    streaming_chat: Option<Message>,
}
impl Wiesel {
    fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let mut notifications = Notifications::default();
        let settings = match settings::load() {
            Ok(s) => s,
            Err(e) => {
                notifications.report(
                    Source::Settings,
                    "Could not load settings. Using defaults.",
                    &e,
                );
                Settings::default()
            }
        };
        let authenticated = match settings::key() {
            Ok(key) => key.is_some(),
            Err(e) => {
                notifications.report(
                    Source::Keychain,
                    "Could not read API key. Unlock Keychain and reconnect.",
                    &e,
                );
                false
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
            page: if settings.onboarded && authenticated {
                Page::Launcher
            } else {
                Page::Setup
            },
            settings,
            manager,
            hotkey: None,
            hotkey_input,
            model_picker,
            grammar_input,
            improve_input,
            key_input: cx.new(|cx| TextInput::new("Paste your AI Gateway API key", true, cx)),
            composer: cx.new(|cx| TextInput::new("Ask anything…", false, cx)),
            launcher_input: cx.new(TextInput::launcher),
            focus: cx.focus_handle(),
            recording: false,
            authenticated,
            accessibility_granted: selection::accessibility_granted(),
            permission_checked_at: Instant::now(),
            busy: false,
            notifications,
            selection: selection::Capture {
                error: "Select text in another app and press your hotkey.".into(),
                ..Default::default()
            },
            pending_capture: None,
            failed_chat: std::collections::VecDeque::new(),
            result: String::new(),
            writing_title: String::new(),
            messages: vec![],
            tx,
            rx,
            chat_scroll: ScrollHandle::new(),
            chat_follow_bottom: true,
            streaming_chat: None,
        };
        for input in [
            &app.hotkey_input,
            &app.key_input,
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
        // Opening the app must work even before AI Gateway onboarding is complete.
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
            app.page = Page::Setup;
        }
        window.on_window_should_close(cx, |_, cx| {
            cx.hide();
            false
        });
        if app.page == Page::Launcher {
            window.focus(&app.launcher_input.focus_handle(cx), cx);
        } else {
            window.focus(&app.focus, cx);
        }
        app.sync_permission_issue();
        app.refresh_models(cx);
        app
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
        let previous = self.settings.hotkey.clone();
        if let Err(e) = self.register(&text) {
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
                    self.settings = next;
                    self.notifications.clear(Source::Settings);
                    self.notifications.success("Shortcut active and saved.");
                }
                Err(e) => {
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
            Notification::new(Severity::Warning, "Connect AI Gateway in Settings.")
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
                    self.pending_capture = Some(selection::begin());
                    cx.notify();
                }
            }
        }
        if let Some(capture) = self.pending_capture.as_mut()
            && let Some(result) = capture.poll()
        {
            self.pending_capture = None;
            self.notifications.clear(Source::Selection);
            match &result {
                Ok(text) => self.notifications.success(format!(
                    "Captured {} characters. Ready for writing.",
                    text.chars().count()
                )),
                Err(e) => {
                    let (severity, message) = notifications::capture_issue(e);
                    eprintln!("Wiesel selection: {e:#}");
                    self.notifications
                        .issue(Source::Selection, severity, message);
                }
            }
            self.selection.update(result);
            if !self.busy && self.page != Page::Setup {
                self.page = Page::Launcher;
            }
            cx.activate(true);
            window.activate_window();
            if self.page == Page::Launcher {
                window.focus(&self.launcher_input.focus_handle(cx), cx);
            } else {
                window.focus(&self.focus, cx);
            }
            cx.notify();
        }
        // Resume following only once the reader has returned to the bottom.
        if !self.chat_follow_bottom
            && self.chat_scroll.max_offset().y + self.chat_scroll.offset().y <= px(1.)
        {
            self.chat_follow_bottom = true;
        }
        while let Ok(event) = self.rx.try_recv() {
            if matches!(&event, ResultEvent::Auth(..) | ResultEvent::Completion(..)) {
                self.busy = false;
            }
            match event {
                ResultEvent::Models(result) => {
                    self.notifications.clear(Source::Models);
                    match &result {
                        Ok(_) => {
                            if self.page == Page::Setup
                                && !self.busy
                                && self.pending_capture.is_none()
                            {
                                self.notifications.success("Model catalog refreshed.");
                            }
                        }
                        Err(e) => self.notifications.report(
                            Source::Models,
                            "Could not load models. Refresh models in Settings.",
                            e,
                        ),
                    }
                    self.model_picker
                        .update(cx, |picker, cx| picker.finish_load(result, cx));
                }
                ResultEvent::Auth(result, key) => {
                    self.notifications.clear(Source::Request);
                    match result {
                        Ok(()) => match settings::save_key(&key) {
                            Ok(()) => {
                                self.authenticated = true;
                                self.key_input.update(cx, |input, cx| input.set("", cx));
                                self.notifications.clear(Source::Keychain);
                                self.notifications
                                    .success("Connected. API key saved securely in Keychain.");
                            }
                            Err(e) => self.notifications.report(
                                Source::Keychain,
                                "Key verified but not saved. Unlock Keychain and retry.",
                                &e,
                            ),
                        },
                        Err(e) => self.notifications.report(
                            Source::Request,
                            gateway::failure_message(&e),
                            &e,
                        ),
                    }
                }
                ResultEvent::ChatDelta(delta) => {
                    if let Some(message) = &mut self.streaming_chat {
                        message.content.push_str(&delta);
                        self.follow_chat_bottom();
                    }
                }
                ResultEvent::Completion(result, chat) => {
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
                            self.notifications.report(
                                Source::Request,
                                gateway::failure_message(&e),
                                &e,
                            );
                            if chat && let Some(message) = self.messages.pop() {
                                let (draft, failed) = recover_failed_draft(
                                    &self.composer.read(cx).content,
                                    message.content,
                                );
                                if let Some(failed) = failed {
                                    // Preserve the newer draft's caret, selection, and active IME state.
                                    self.failed_chat.push_back(failed);
                                } else {
                                    self.composer.update(cx, |i, cx| i.set(draft, cx));
                                }
                            }
                        }
                    }
                }
            }
            cx.notify();
        }
        if self
            .notifications
            .update(Instant::now(), self.idle_notification())
        {
            cx.notify();
        }
    }
    fn refresh_models(&mut self, cx: &mut Context<Self>) {
        if self.model_picker.read(cx).loading {
            return;
        }
        self.model_picker
            .update(cx, |picker, cx| picker.begin_load(cx));
        self.notifications
            .working(Source::Models, "Loading model catalog…");
        let tx = self.tx.clone();
        std::thread::spawn(move || {
            let _ = tx.send(ResultEvent::Models(gateway::models()));
        });
    }
    fn authenticate(&mut self, cx: &mut Context<Self>) {
        if self.busy {
            return;
        }
        let key = self.key_input.read(cx).content.trim().to_owned();
        if key.is_empty() {
            self.notifications.warning("Paste an AI Gateway key first.");
            cx.notify();
            return;
        }
        self.busy = true;
        self.notifications
            .working(Source::Request, "Verifying your API key…");
        let tx = self.tx.clone();
        std::thread::spawn(move || {
            let result = gateway::authenticate(&key);
            let _ = tx.send(ResultEvent::Auth(result, key));
        });
        cx.notify();
    }
    fn finish_setup(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.busy {
            return;
        }
        let picker = self.model_picker.read(cx);
        let model = picker.selected.clone();
        let grammar = self.grammar_input.read(cx).content.trim().to_owned();
        let improve = self.improve_input.read(cx).content.trim().to_owned();
        let hotkey = self.hotkey_input.read(cx).content.trim().to_owned();
        let missing = if !self.authenticated {
            Some("Connect AI Gateway first.")
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
        let previous = self.settings.hotkey.clone();
        if let Err(e) = self.register(&hotkey) {
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
                self.settings = next;
                self.page = Page::Launcher;
                self.notifications.clear(Source::Settings);
                self.notifications.success("Settings saved. Ready to go.");
                window.focus(&self.launcher_input.focus_handle(cx), cx);
            }
            Err(e) => {
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
        let key = match settings::key() {
            Ok(Some(key)) => key,
            Ok(None) => {
                self.authenticated = false;
                self.notifications.issue(
                    Source::Keychain,
                    Severity::Warning,
                    "No saved API key. Connect in Settings.",
                );
                cx.notify();
                return;
            }
            Err(e) => {
                self.notifications.report(
                    Source::Keychain,
                    "Could not read API key. Unlock Keychain and reconnect.",
                    &e,
                );
                cx.notify();
                return;
            }
        };
        self.busy = true;
        self.notifications.clear(Source::Keychain);
        self.notifications.working(Source::Request, "Thinking…");
        let model = self.settings.model.clone();
        let tx = self.tx.clone();
        if chat {
            self.streaming_chat = Some(Message::new("assistant", ""));
        }
        std::thread::spawn(move || {
            let result = if chat {
                gateway::complete_stream(&key, &model, &messages, |delta| {
                    tx.send(ResultEvent::ChatDelta(delta.to_owned()))
                        .context("Chat window closed")
                })
            } else {
                gateway::complete(&key, &model, &messages)
            };
            let _ = tx.send(ResultEvent::Completion(result, chat));
        });
        cx.notify();
    }
    fn run_quick_action(
        &mut self,
        action: QuickAction,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.busy {
            return;
        }
        match action {
            QuickAction::Chat => {
                self.page = Page::Chat;
                window.focus(&self.composer.focus_handle(cx), cx);
                cx.notify();
            }
            QuickAction::Add => {
                self.notifications
                    .info("Custom quick actions are coming soon.");
                cx.notify();
            }
            _ => self.writing(action, cx),
        }
    }
    fn writing(&mut self, action: QuickAction, cx: &mut Context<Self>) {
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
        self.page = Page::Writing;
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
        // Resolve credentials before mutating conversation; request reports failure if Keychain fails.
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
        if key.key == "escape" {
            cx.hide();
            cx.stop_propagation();
        } else if self.page == Page::Launcher {
            if key.key == "enter" && !key.modifiers.shift {
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
        } else if self.page == Page::Chat && key.key == "enter" && !key.modifiers.shift {
            self.send_chat(cx);
            cx.stop_propagation();
        }
    }
    fn button(id: &'static str, title: &str) -> Stateful<Div> {
        div()
            .id(id)
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
    fn setup(&self, cx: &mut Context<Self>) -> Stateful<Div> {
        // Keep scrollable content at its natural height instead of shrinking every field to fit.
        div().id("settings-scroll").flex_1().min_h_0().overflow_y_scroll()
            .child(div().w_full().flex().flex_col().gap_3().p_5()
            .child(div().text_size(px(24.)).font_weight(FontWeight::BOLD).child(if self.settings.onboarded { "Make it yours" } else { "Welcome to Wiesel" }))
            .child(Self::label("Your writing assistant, one shortcut away."))
            .child(Self::label("1 · Global hotkey"))
            .child(div().flex().gap_2().child(div().flex_1().child(self.hotkey_input.clone()))
                .child(Self::button("record", if self.recording { "Press shortcut…" } else { "Record shortcut" }).on_click(cx.listener(|this, _, window, cx| { this.recording = true; this.notifications.info("Press your shortcut. Escape cancels."); window.focus(&this.focus, cx); cx.notify(); }))))
            .child(Self::button("apply-hotkey", "Apply shortcut").on_click(cx.listener(|this, _, _, cx| this.apply_hotkey(cx))))
            .child(Self::label("Command (⌘) is called Super in the field above. Recorded shortcuts activate immediately; typed changes need Apply shortcut."))
            .child(Self::label("2 · Connect Vercel AI Gateway"))
            .child(Self::label("API key"))
            .child(div().w_full().h(px(40.)).min_h(px(40.)).flex_shrink_0().rounded_md().border_1().border_color(rgba(theme::INPUT_BORDER)).overflow_hidden().child(self.key_input.clone()))
            .child(Self::button("connect", if self.busy { "Verifying…" } else { "Verify & connect" }).on_click(cx.listener(|this, _, _, cx| this.authenticate(cx))))
            .child(Self::label(if self.authenticated { "✓ Key stored in macOS Keychain. Paste a new key only to replace it." } else { "Your key is verified with Vercel and stored in macOS Keychain, never in settings." }))
            .child(Self::label("3 · Model"))
            .child(self.model_picker.clone())
            .child(Self::button("refresh-models", "Refresh models").on_click(cx.listener(|this, _, _, cx| this.refresh_models(cx))))
            .child(Self::label("The live catalog includes all Gateway models. For writing and chat, choose a text/chat model."))
            .child(Self::label("Custom prompt · Fix grammar"))
            .child(self.grammar_input.clone())
            .child(Self::label("Custom prompt · Improve writing"))
            .child(self.improve_input.clone())
            .child(Self::label("Writing actions send selected text to Vercel and the chosen model provider. Nothing is sent until you choose an action."))
            .child(Self::label("Required for selected text: System Settings → Privacy & Security → Accessibility (Gerätesteuerung und Datenzugriff). Enable Wiesel; its live status appears in the bottom bar."))
            .child(self.permission_controls(cx))
            .child(Self::label("Wiesel captures selected text using ⌘C and restores your previous clipboard before opening. Release the shortcut keys and keep the source app active. Clipboard managers may retain the temporary selection."))
            .child(Self::label("Input Monitoring, Screen Recording, and Automation are not required. On newer macOS versions, allow Wiesel to paste from other apps if asked so it can save and restore the clipboard."))
            .child(Self::label("If macOS shows Wiesel enabled but the bottom bar still requests access after a rebuild, remove the old entry, add this Wiesel.app again, and relaunch it."))
            .child(div().flex().gap_2()
                .child(Self::button("save", "Save & start").on_click(cx.listener(|this, _, window, cx| this.finish_setup(window, cx))))
                .child(Self::button("disconnect", "Disconnect").on_click(cx.listener(|this, _, _, cx| {
                    if this.busy { return; }
                    match settings::delete_key() {
                        Ok(()) => {
                            this.authenticated = false;
                            this.notifications.clear(Source::Keychain);
                            this.notifications.clear(Source::Request);
                            this.notifications.success("Disconnected. API key removed from Keychain.");
                        },
                        Err(e) => this.notifications.report(Source::Keychain, "Could not remove API key. Unlock Keychain and retry.", &e),
                    }
                    cx.notify();
                })))))
    }
    fn launcher(&self, cx: &mut Context<Self>) -> Stateful<Div> {
        div()
            .id("quick-actions-scroll")
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .flex()
            .flex_col()
            .gap(px(12.))
            .p(px(20.))
            .children(QUICK_ACTIONS.chunks(3).map(|actions| {
                div().flex().flex_shrink_0().gap(px(12.)).w_full().children(
                    actions.iter().copied().map(|action| {
                        div()
                            .id(action.id())
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
                            .when(action == QuickAction::Add, |d| {
                                d.text_color(rgb(theme::MUTED))
                            })
                            .hover(|s| s.bg(rgb(theme::ACCENT)).border_color(rgb(theme::RING)))
                            .child(theme::icon(action.icon(), 24.).text_color(rgb(
                                if action == QuickAction::Add {
                                    theme::MUTED
                                } else {
                                    theme::FOREGROUND
                                },
                            )))
                            .child(
                                div()
                                    .text_size(px(13.))
                                    .font_weight(FontWeight::MEDIUM)
                                    .child(action.title()),
                            )
                            .child(div().absolute().top(px(10.)).right(px(10.)).child(
                                theme::badge(if action == QuickAction::Add {
                                    "Soon"
                                } else {
                                    action.shortcut()
                                }),
                            ))
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.run_quick_action(action, window, cx);
                            }))
                    }),
                )
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
                                div()
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
                            .child(div().flex_1().min_w_0().child(self.composer.clone()))
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
        let content = match self.page {
            Page::Setup => self.setup(cx).into_any_element(),
            Page::Launcher => self.launcher(cx).into_any_element(),
            Page::Chat => self.chat(cx).into_any_element(),
            Page::Writing => self.writing_view(cx).into_any_element(),
        };
        div()
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
                    .when(self.page == Page::Launcher, |d| {
                        d.child(div().flex_1().min_w_0().child(self.launcher_input.clone()))
                    })
                    .when(self.page != Page::Launcher, |d| {
                        d.when(self.page != Page::Setup, |d| {
                            d.child(
                                div()
                                    .id("back")
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
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        if !this.busy {
                                            this.page = Page::Launcher;
                                            window.focus(&this.launcher_input.focus_handle(cx), cx);
                                            cx.notify();
                                        }
                                    })),
                            )
                            .child(div().w(px(1.)).h(px(18.)).bg(rgba(theme::BORDER)))
                        })
                        .child(theme::icon(
                            match self.page {
                                Page::Writing => QUICK_ACTIONS
                                    .iter()
                                    .find(|action| action.title() == self.writing_title)
                                    .map_or("spell-check", |action| action.icon()),
                                _ => "chat",
                            },
                            18.,
                        ))
                        .child(
                            div()
                                .flex_1()
                                .font_weight(FontWeight::MEDIUM)
                                .text_size(px(14.))
                                .on_mouse_down(MouseButton::Left, |_, window, _| {
                                    window.start_window_move();
                                })
                                .child(match self.page {
                                    Page::Writing => self.writing_title.clone(),
                                    Page::Setup => "Wiesel · Settings".to_owned(),
                                    _ => "Chat".to_owned(),
                                }),
                        )
                    })
                    .when(self.page == Page::Chat, |d| {
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
                    .child(
                        Self::icon_button("settings", "settings", "Settings").on_click(
                            cx.listener(|this, _, _, cx| {
                                if !this.busy {
                                    this.page = Page::Setup;
                                    cx.notify();
                                }
                            }),
                        ),
                    )
                    .child(
                        Self::icon_button("hide", "minus", "Hide").on_click(|_, _, cx| cx.hide()),
                    ),
            )
            .child(content)
            .child(self.notifications.render())
    }
}
fn main() {
    gpui_platform::application()
        .with_assets(theme::Assets)
        .run(|cx: &mut App| {
            cx.set_reduce_motion(
                objc2_app_kit::NSWorkspace::sharedWorkspace()
                    .accessibilityDisplayShouldReduceMotion(),
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
                |window, cx| cx.new(|cx| Wiesel::new(window, cx)),
            ) {
                Ok(handle) => handle,
                Err(error) => {
                    // No notification surface exists yet; fail gracefully with a diagnostic.
                    eprintln!("Cannot open Wiesel window: {error:#}");
                    cx.quit();
                    return;
                }
            };
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
