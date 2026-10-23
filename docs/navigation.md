# Windows and navigation

Wiesel has one state owner, the `Wiesel` entity in `src/main.rs`, and several
windows that render from it:

- **Launcher HUD**: `Wiesel`'s own window. A borderless, blurred panel modeled on the
  macOS cmd+tab switcher: one row of quick-action tiles above a slim status strip
  with the Settings gear. It shows the first `Settings::quick_action_count` actions
  (3, 5 or 8, chosen in Settings). There is no "add" tile; new actions come from
  **File → New Quick Action…** or Settings.
- **Surfaces** (`enum Surface`): Settings, Chat and Writing (a quick action's result).
  Each is a singleton window whose root view (`SurfaceView`) only forwards rendering
  to `Wiesel::render_surface`. Closing one drops the window, never its state: chat
  history, drafts, failed messages and the last result survive and reappear when the
  window opens again.

## Opening and closing windows

- `open_surface(surface, focus, cx)` opens or activates a surface and focuses
  `focus` (or the window's root focus) inside it. GPUI renders a window as soon as it
  opens, which would re-enter `Wiesel` mid-update, so the window is created in a
  deferred effect. Until then `Surfaces::opening` marks it as open, so `is_open`
  is true at once. Closing a surface that is still opening cancels the open.
- `focus_in(surface, focus, window, cx)` focuses directly when `window` already is
  that surface's window, otherwise it opens/activates the surface with a pending
  focus.
- `route_to_settings(window, cx, operation)` re-runs a Settings operation inside the
  Settings window when it was triggered from another window (menu, ⌘⇧L, a login
  callback). Focus capture, focus restoration and confirmation dialogs then act on the
  Settings window.
- `close_surface` / `forget_surface` drop a window. Closing Settings unmounts its
  stack (see below), stops shortcut recording and cancels a pending browser login.
  `surface_should_close` backs the close button, ⌘W and Escape at a root. Settings
  first asks to discard a dirty action draft (`Removal::Close`).
- Opening any surface dismisses the HUD (`dismiss_hud` orders the HUD window out
  without hiding the app). The global shortcut calls `show_hud`, which re-centers the
  HUD, highlights the first tile and focuses it. Other windows keep their state.
  Clicking the Dock icon with nothing on screen also shows the HUD.

## The Settings stack

`src/navigation.rs` provides `Navigation<T, L>`, a UI-independent stack of owned
components and owner-defined pop lifecycle data. `Wiesel::navigation` is the
**Settings window's** stack: `Screen::Settings` is its permanent root, with
`Screen::ActionManager` (Quick actions) and `Screen::ActionEditor` as children.
`current_screen()` is the stack's top (Settings while the window is closed).

### Stack API

- `Navigation::new(root)` mounts the permanent root with no pop lifecycle.
- `push(component, on_pop) -> EntryId` mounts a child with a fresh identity.
- `current()` / `root()` return immutable `Entry<T, L>` references. Entries expose
  `id`, `component` and `on_pop: Option<L>`; the root's lifecycle is `None`.
- `entries()` iterates mounted entries from root to current. `depth()` includes
  root; `is_root()` reports whether there are no children.
- `pop() -> Option<Entry<T, L>>` removes exactly one child. At root it returns
  `None` without changing root, identity or depth.
- `pop_to(id) -> Option<Vec<Entry<T, L>>>` reveals an existing mounted entry.
  An unknown identity returns `None` without changing the stack. Targeting current
  returns an empty vector; targeting root removes all children.
- `pop_to_root() -> Vec<Entry<T, L>>` removes all children. Both multi-pop APIs
  return removed entries in top-to-bottom order.

`L` is the Rust equivalent of `onPop`: the owner consumes the returned lifecycle
once, with its application/window context, after each explicit removal. A caller
must process **every** returned entry, including entries from multi-pop. Entry
identities are never reused within one navigation instance.

### Wiesel integration

- `push_screen(screen, window, cx)` opens the Settings window if needed, snapshots
  the covered screen's focus (only when called from the Settings window) and focuses
  the new screen. Pushing the current screen is a no-op.
- `push_settings(startup, window, cx)` is the Settings entry point. It reveals the
  root by popping intervening screens via `pop_to_screen` instead of stacking a
  duplicate. `startup` marks Settings opened for setup at launch
  (`settings_startup`).
- `open_action_manager` reveals or pushes Quick actions; it is a singleton.
- `pop_screen(window, cx) -> bool` processes one lifecycle, restores the covered
  focus and returns false at the Settings root.
- `finish_pop` clears the action editor when its entry is removed; leaving Settings
  stops recording and cancels a pending browser login. It does **not** cancel
  inference.

Back in the Settings title bar prevents mouse-down's default focus transfer, so the
push snapshots the input that was actually focused. Settings, Quick actions and New
Quick Action are menu commands handled on every window's root and routed into the
Settings window; they remain usable while inference is busy and first close any
action-editor popup.

### Dirty editors

**Preflight the whole removal before calling pop/pop-to-existing/closing if any
removed screen can discard unsaved state.** `guard_action_removal` asks before a
dirty action draft is removed by Back/Escape (`Removal::One`), by revealing a lower
screen (`Removal::To`) or by closing the window (`Removal::Close`). If denied, the
stack and window stay unchanged.

## Keys

Launcher HUD:

1. `1`–`9` run the tile printed with that number.
2. ←/→ and Tab/⇧Tab move the highlight (wrapping); Enter or Space runs it. Hovering a
   tile highlights it too.
3. Escape dismisses the HUD. With no other window open it hides Wiesel instead, so
   focus returns to the previous app.

Settings window, in order of precedence:

1. Shortcut recording consumes Escape to stop recording.
2. A focused popover or confirmation consumes Escape before the window does.
3. Otherwise Escape pops **one** screen and restores focus; at the Settings root it
   closes the window.

Chat and Writing: Escape closes the window. ⌘W closes any secondary window (with the
same dirty-draft confirmation for Settings) and dismisses the HUD.

## Startup

Startup opens setup Settings over the HUD. Verification of a saved or freshly
persisted login closes Settings again **only** when it is still the startup window,
on its root, and login, onboarding and the registered shortcut are in place. An
explicitly opened Settings window never closes itself. Leaving setup does not bypass
inference gates: requests still require verified login (or a local model), a valid
credential and completed onboarding.

## Retained state and asynchronous ownership

Closing or covering windows preserves Chat composer text/selection, messages,
failed drafts, scroll position, Settings fields and Writing source/title/result.
A deliberate New Chat can replace chat history, but only when no physical inference
is pending.

Every inference scope carries login generation/device plus a `CompletionScope`
(an `EntryId` and monotonic request generation). `active_request` owns one physical
inference slot, even after its window closes. Navigation and Settings remain
available, but another inference and New Chat replacement remain blocked until its
terminal result. Completion never opens a window by itself, except that a failed
paste-back opens Writing so the result is not lost. Replayed events from an older
request cannot modify or release a newer request's slot.

### Account-privacy exception

Actual session end/signout/revocation is not navigation-only. `end_session_state`
clears private Chat drafts, history/failed drafts/previews, Writing source,
result/title and captured selection/snapshots immediately, while leaving windows
and Settings/instructions intact. It does not touch the OS clipboard, persisted
settings/actions or diagnostic files. Native credential cleanup remains in the
existing production `clear_session` path, separate from this in-memory boundary.

Session end invalidates capture identity but keeps draining the physical Copy
capture until clipboard restoration completes. A late capture cannot restore
ended-session selection or show the HUD. It also retains the physical inference
slot: stale deltas and reply/error payloads are discarded, while the **exact
originating terminal event** can release that slot despite its stale login scope.

## Verification boundary

`src/navigation.rs` has ordinary Rust tests for the stack. `src/surface_tests.rs`
covers the HUD row, launcher size, keys, and opening/closing the secondary windows.
`src/chat_tests.rs` and `src/action_builder_tests.rs` drive Chat, Settings and the
action editor through real GPUI clicks and keystrokes. `src/test_support.rs` routes
input to the window that renders a control (or to the key window for typing).

GPUI's TestPlatform has no native rendering, and its windows have no AppKit handle,
so ordering the HUD out and centering it are no-ops in tests (the `hud_visible` flag
records intent). Root Escape records hide **intent** through a test-only recorder.
No native launch, live inference, user settings/credential read, clipboard
operation or hotkey registration is performed by these fixtures. See
[testing.md](testing.md) for the separate native smoke authorization boundary.
