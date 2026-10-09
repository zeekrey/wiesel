# Rooted navigation

`src/navigation.rs` provides `Navigation<T, L>`, a UI-independent stack of owned
components and owner-defined pop lifecycle data. `Wiesel::navigation` is the only
route authority; `Screen` is a component discriminator, not a second page state.
Home/Launcher is always the permanent root, including before login/onboarding.
Settings (also the setup screen), Chat and Writing are children.

## Stack API

- `Navigation::new(root)` mounts the permanent root with no pop lifecycle.
- `push(component, on_pop) -> EntryId` mounts a child with a fresh identity.
  `T` can be a screen enum, an owned screen-state value, or a GPUI entity.
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
once, with its application/window context, after each explicit removal. This
avoids a closure holding/reentering a GPUI entity while it is being updated. The
stack itself does not run callbacks on drop. A caller must process **every**
returned entry, including entries from multi-pop. Root cannot be removed,
mutated through this API, or replaced. Entry identities are never reused within
one navigation instance.

## Wiesel integration API

All route-changing UI handlers use these owner helpers in `src/main.rs`:

- `current_screen()` derives the visible screen from `navigation.current()`.
- `push_screen(screen, startup, window, cx)` preserves retained domain state,
  snapshots the covered screen's focus, then focuses the new screen. Pushing
  Launcher instead unwinds to the permanent root; pushing current is a no-op.
- `push_settings(startup, window, cx)` is the Settings singleton entry point.
  Current Settings activation is a no-op. If Settings exists below current,
  intervening screens are popped via `pop_to_screen` to reveal it, not duplicated.
- `pop_screen(window, cx) -> bool` processes one lifecycle, restores the covered
  focus, and returns false at root. `pop_to_screen` and `pop_to_root` process the
  same lifecycle for each removed child.
- `ScreenLifecycle` records `restore_focus` and whether Settings was pushed for
  startup. `finish_pop` cancels an active browser login only when leaving Settings
  and exits shortcut recording. It does **not** cancel inference.

Header navigation controls prevent mouse-down's default focus transfer, so the
push snapshots the input that was actually focused, not the header/root handle.
Back is shown on Settings as well as Chat/Writing, and works while inference is
busy. Save Settings pops Settings to its underlying screen rather than resetting
the stack to Home. Header Settings remains usable while inference is busy.

### New screens and dirty editors

New screens should add one `Screen` component variant, rendering/focus behavior,
and use the helpers above; do not add authoritative route booleans or another
page field. Screen-local entities/state may be carried in the component payload;
app-owned retained domains may outlive mounting when explicitly intended.
Non-inference screens/local editing must not be gated by the inference busy flag.

**Preflight the whole removal before calling pop/pop-to-existing/pop-to-root if
any removed screen can discard unsaved state.** Built-in Chat/Writing/Settings
state is retained outside the mounted entries, so today's transitions are safe.
A new destructive editor must obtain its dirty-editor confirmation before
invoking these helpers (including Settings activation and hotkey root activation).
If denied or unsafe, leave the stack unchanged. Do not pop some entries, then ask
about later ones. The generic stack cannot make product-specific dirty decisions.
Settings must use `push_settings`, not an unconditional direct Settings push.

## Escape and activation

Key handling is deliberately layered:

1. Shortcut recording consumes Escape to stop recording, without navigation.
2. A focused child popover/modal consumes Escape before the parent's bubbling
   handler. The existing ModelPicker closes itself this way. New overlays must
   likewise stop propagation when dismissing.
3. Otherwise Escape pops **one** child and restores focus, including Settings.
4. At root Escape requests application hiding; it never underflows the stack.

Hide/minus hides without changing navigation or clearing drafts. Completed global
hotkey selection capture activates Home by unwinding idle non-Settings screens.
It retains Settings (onboarding/editing) and retains the current route while an
inference is busy. This preserves the prior intentional hotkey behavior, now via
stack removal/lifecycles rather than page replacement. Selection snapshots used
by an existing Writing request are not replaced by a later activation.

Startup pushes setup Settings over Home. Verification of a saved/successfully
persisted login may pop **only current startup Settings**, and only when login,
onboarding and registered-hotkey requirements are met. Explicitly opened Settings
never auto-pops, and startup Settings below another screen is never implicitly
removed. Authentication changes do not replace/reset the navigation stack.
Leaving setup does not bypass inference gates: requests still require verified
login, a valid credential and completed onboarding. Unsent Home drafts are not
consumed by a setup-gated submission.

## Retained state and asynchronous ownership

Navigation-only cover/pop/hide preserves Chat composer text/selection, messages,
failed drafts, scroll handle/reader position, Launcher draft, Settings fields,
and Writing source/title/result. Chat and Writing are retained app-owned domains,
not recreated on every entry push. A deliberate New Chat or nonempty Home
submission can replace chat history, but only when no physical inference is pending.

Every inference scope carries login generation/device plus a `CompletionScope`
(originating `EntryId` and monotonic request generation). `active_request` owns
one physical inference slot, even after its screen is popped. Navigation/Settings
remains available, but another inference and New Chat replacement remain blocked
until its terminal result. Reopening a retained domain can intentionally expose
its completed state; completion never pushes a route or resurrects a removed entry.
Same-session covered/popped successes update only that retained domain; failures
recover the failed draft without overwriting a newer draft. Replayed events from
an older request cannot modify/release a newer request's slot.

### Account-privacy exception

Actual session end/signout/revocation is not navigation-only. `end_session_state`
clears private Chat/Home drafts, history/failed drafts/previews, Writing source,
result/title and captured selection/snapshots immediately, while leaving routes
and Settings/instructions intact. It does not touch the OS clipboard, persisted
settings/actions or diagnostic files. Native credential cleanup remains in the
existing production `clear_session` path, separate from this in-memory boundary.

Session end invalidates capture identity but keeps draining the physical Copy
capture until clipboard restoration completes. A late capture cannot restore
ended-session selection or activate/change routes. It also retains the physical
inference slot: stale deltas and reply/error payloads are discarded, while the
**exact originating terminal event** can release that slot despite its stale login
scope. It cannot release a different/newer slot, recover old private drafts or
revive credentials/history/routes. A replacement login alone does not permit a
second physical request. These rules add no retry or provider cancellation, and
make no billing-outcome guarantee.

## Verification boundary

`src/navigation.rs` has ordinary Rust tests for root, identities, retained owned
state, lifecycle order and atomic unknown-target handling. `src/chat_tests.rs`
uses real GPUI fixture clicks/typing/Enter/Escape plus `ControlledBackend` releases
for nested navigation, focus/draft/selection/history/scroll restoration, overlay
precedence, singleton Settings, startup gating and covered/popped/stale requests.
Ephemeral session/capture tests avoid Keychain/native capture/model refresh and
exercise the production in-memory/event paths.

GPUI's TestPlatform has no native rendering and its `hide()` is unimplemented.
Ephemeral fixtures opt into a test-only hide recorder; real root Escape reaches
the production navigation handler and records hide **intent**, not native hiding.
Production/uninjected paths still call `cx.hide()`. No native launch, live inference,
user settings/credential read, clipboard operation or hotkey registration is
performed by these fixtures. See [testing.md](testing.md) for the separate native
smoke authorization and evidence boundary.
