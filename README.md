# Wiesel 🦫

A small, Raycast-inspired native writing launcher built with Rust and GPUI. **This first version supports macOS only**; other targets are explicitly rejected until their native selection and persistent credential backends are implemented.

## Install a release

Download a DMG from this repository's GitHub **Releases** page. Choose the latest
**stable** release for normal use, or a release marked **Pre-release** to test the
next preview. Preview tags end in `-pre`; stable tags do not. Choose
`macos-arm64` for Apple Silicon or `macos-x86_64` for Intel. Open it and drag
**Wiesel.app** to **Applications**. ZIP downloads and SHA-256 checksum files are
also provided.

**These builds are ad-hoc signed, not Apple-notarized.** macOS may block a
downloaded app. Only if you trust the release, try launching it, then use
**System Settings → Privacy & Security → Open Anyway** and confirm. Do not
disable Gatekeeper globally. See Apple's
[guidance for opening apps safely](https://support.apple.com/en-us/102445).
Grant Accessibility access during [first launch](#first-launch).

Verify downloaded assets before installing:

```sh
# Download the matching .sha256 file, DMG and ZIP into the same directory:
shasum -a 256 -c Wiesel-<version>-macos-<arch>.sha256
```

Wiesel uses manually triggered **preview → stable release trains**. `main` runs
CI without publishing; release branches produce installers. For contribution
instructions, release commands, and one-time GitHub configuration, see
[CONTRIBUTING.md](CONTRIBUTING.md).

## Run

```sh
cargo run --locked
```

For a named app bundle (recommended for Accessibility permissions):

```sh
bash scripts/build-app.sh
open dist/Wiesel.app
# Optional optimized build:
bash scripts/build-app.sh --release
```

GPUI is pinned to a specific Zed revision. Runtime Metal shader compilation is enabled, so the standalone `metal` build tool/full Xcode installation is not required. Rust and Apple's Command Line Tools are still required. Without signing configuration, `build-app.sh` uses a local ad-hoc signature; rebuilding can require re-granting Accessibility permission. Configure persistent development signing below to avoid that. Distribution signing/notarization is a separate concern.

## Development builds

### One-time signing setup

macOS checks the app's code-signing identity, not just its bundle ID. Use the **same certificate** for successive builds, keep `com.wiesel.launcher` unchanged, and launch the same `dist/Wiesel.app` path. This should preserve Accessibility authorization across normal rebuilds, but verify it on your macOS version. Replacing/renewing the certificate or moving the app may require authorization again.

Choose one certificate option:

- **Apple Development:** If you have an Apple Developer team, create an Apple Development certificate using Xcode's **Settings → Accounts → your team → Manage Certificates → + → Apple Development**. It must appear in your Keychain with its private key.
- **Local development only, no developer account:** Open **Keychain Access → Certificate Assistant → Create a Certificate** (in Keychain Access's application menu). Name it **Wiesel Development**, select **Self Signed Root** as Identity Type and **Code Signing** as Certificate Type. Enable **Let me override defaults** if you want a longer validity period, and complete the assistant, storing the identity in your login keychain. Keep this certificate and its private key rather than recreating them for each build. In its **Get Info → Trust** section, set **Code Signing** to **Always Trust** if needed for it to be recognized as valid; leave unrelated trust purposes at their defaults. macOS may ask for your password to save the trust change. Do not use this certificate for distribution.

Check that your identity is usable:

```sh
security find-identity -v -p codesigning
```

It must list a valid identity; a certificate without a private key is insufficient. Configure its **exact name** or, preferably, its listed SHA-1 fingerprint (avoids duplicate-name ambiguity):

```sh
cp .wiesel-dev.env.example .wiesel-dev.env
# Edit .wiesel-dev.env to use your certificate's name or fingerprint.
```

`.wiesel-dev.env` is ignored by Git and sourced as trusted local shell configuration. Alternatively, set `WIESEL_SIGNING_IDENTITY` in your shell; a nonempty environment value takes precedence over the file:

```sh
export WIESEL_SIGNING_IDENTITY='Wiesel Development'
```

On the first certificate-signed launch, remove the old ad-hoc Wiesel Accessibility entry if necessary, add this repo's `dist/Wiesel.app`, and relaunch. Subsequent builds use the same certificate and macOS-generated designated requirement. You can inspect the requirement with `codesign -d -r- dist/Wiesel.app`; it should remain stable across rebuilds even though the executable's code hash changes. No script creates certificates, changes Keychain trust, or resets/grants permissions.

### Build and restart with one command

```sh
bash scripts/dev.sh
# Optional optimized build:
bash scripts/dev.sh --release
```

The command validates the signing identity, compiles, then stages/signs/verifies a new bundle **before stopping the current app**. It sends SIGTERM only to processes whose executable path matches this repo's bundle, waits up to five seconds, replaces the bundle at the same path, and launches it. Other installed copies of Wiesel are not stopped. No Automation permission or AppleScript is required.

- Compilation/signing failures leave the old app running and its bundle unchanged.
- If Wiesel does not exit, the command aborts without force-killing or replacing it. Quit manually with Command-Q and retry.
- An installation failure attempts to restore the old bundle, but a previously stopped app must be relaunched manually. A launch failure leaves the newly installed bundle available to launch with `open dist/Wiesel.app`.
- Builds are serialized with `dist/.build-lock`. If a process is killed without cleanup, confirm no build is active before removing that empty directory with `rmdir dist/.build-lock`.
- **Restart loses in-memory chat/drafts and interrupts requests. SIGTERM is not an application-level graceful quit and can interrupt clipboard restoration. Do not restart during selection capture.**

`build-app.sh` shares the staging/signing logic but does not stop or launch an app. It refuses to replace this bundle while it is running. It uses your configured identity when present, or explicitly warns and uses ad-hoc signing when none is configured. `dev.sh` never falls back to ad-hoc signing. This is manual one-command restart, not a file watcher or Rust hot reload.

## First launch

1. Record a global shortcut or enter one such as `Super+Shift+Space` (`Super` is Command on macOS). Recorded shortcuts activate and save immediately; after typing a shortcut, click **Apply shortcut**. The shortcut works independently of account login, including before onboarding is complete. The default shortcut is registered at launch. Avoid shortcuts already owned by the OS or another app. Registration failures are shown during setup.
2. Choose **Login / Sign up ↗** to open your external system browser at [wiesel.run](https://wiesel.run). On the login-focused first-run surface, Enter opens login; **Command-Shift-L** opens account settings/login from any screen, including while a request is pending. There is no token-paste field. For a new account, complete website signup and email verification, then **Reopen Login / Sign up ↗** in Wiesel to start a fresh desktop attempt. Keep Wiesel running while finishing browser login.
3. After login, the authenticated Wiesel model catalog loads automatically. Open **Model**, search by provider or model name, and use ↑/↓ and Enter or click an option. Choose a text/chat model; the catalog may contain other model IDs, but this app sends text/chat requests only. **Refresh models** updates the catalog. The selection is saved with **Save & start**.
4. Optionally edit the two writing system prompts. Select **Save & start**.
5. Grant the built **Wiesel.app** access under **System Settings → Privacy & Security → Accessibility** (shown as **Gerätesteuerung und Datenzugriff** on some German macOS versions). Accessibility is required for simulated Copy and protected-field checks; Input Monitoring, Screen Recording, and Automation are not required. On newer macOS versions, allow Wiesel to paste from other apps if asked: Copy capture reads the clipboard to save and restore it. When running with Cargo instead, macOS may associate permission with the terminal or binary.

Every screen has the same **48px bottom status bar**: green for success/ready, yellow for missing input or permissions, red for failures, and muted for work in progress. This is the only notification surface; messages stay on one line. Accessibility is checked for the running process every second using macOS's `AXIsProcessTrusted()`. **Privacy settings** and **Check again** are available in Settings. If Wiesel still requests access after a rebuild despite the macOS toggle being enabled, remove the old entry, add the current `dist/Wiesel.app`, and relaunch. Permission does not guarantee that the target app supports Copy.

### Browser login and device sessions

Wiesel opens `https://wiesel.run/desktop/login` using PKCE S256: fresh independent random state and verifier stay in app memory; only the challenge and state go to the browser. The native `wiesel://auth/callback` route is registered in the app bundle and received through GPUI's macOS URL event handler for running and cold launches. A matching callback is consumed once before exchange. **Cancel login**, Escape while waiting, or reopening login invalidates the old attempt and any pending exchange completion. Attempts expire after ten minutes. If Wiesel quit/restarted, an old callback cannot restore the lost attempt: reopen login for a fresh one. Browser opening/OS routing still needs the signed-bundle manual checks below; `cargo run` alone is not a scheme-registration test.

A successful exchange stores the backend-issued device token, expiry and device ID only in macOS Keychain, service `com.wiesel.desktop.auth`, account `device:<UUID>`; `active-device` stores just the UUID. No browser cookies or provider API keys are used by desktop requests. Existing `com.wiesel.ai-gateway` provider-key entries are deleted without reading or converting them. Saved credentials are checked for expiry on launch, before requests and during polling. Startup also validates the saved device asynchronously. **Check login** retries a failed status check. Expired/rejected (HTTP 401) credentials require a fresh browser login; **there is no refresh token or automatic refresh**.

**Sign out** ends the local session and removes its captured device's Keychain entry independently of the remote sign-out request. Pending results cannot revive the session. Sign-out, expiry, rejected-session cleanup and failed-cleanup retries retain the original device identity: they never delete whichever newer device another app instance has saved. The shared active pointer is removed only if it still identifies that device. All credential load/save/delete operations (including legacy cleanup and rollback) are serialized across participating Wiesel processes by `~/Library/Application Support/Wiesel/desktop-auth.lock`, a nonsecret, empty file. Do not unlink or replace this file while an instance is running; closing the OS lock handle releases it automatically, including after a process exits.

Native removal uses Security.framework's status-checked `SecItemDelete` for both device and legacy entries, restricted to the same User-domain keychain and exact generic-password service/account used by keyring. Only native success or item-not-found is treated as harmless. If deletion fails, the app reports incomplete saved-login recovery, blocks new login, retains its cleanup target and offers **Retry saved login removal**. Do not assume persistent logout until removal succeeds. A startup failure that never safely observed a pointer instead offers **Retry restoring login**, not deletion of a later credential. A captured corrupt pointer can only remove that exact metadata snapshot; it never guesses a device record. Corrupt-pointer recovery may leave an unreachable secure record, rather than risking deletion of another device. Remote sign-out failure is reported separately; verify/revoke the device on the website if needed. Sign-out does not abort an already-sent generation request or promise a refund. Plan and virtual credits are informational projections, not a promise that a model request will be admitted.

## Use

- Leave Wiesel running. Select text in another app, press your shortcut, then release the shortcut keys. Keep the source app active until Wiesel opens; Wiesel uses simulated ⌘C and restores your clipboard before opening.
- Home shows six quick-action tiles in a three-column, two-row grid, each with a Lucide icon.
- Choose **Fix spelling** (`1`) or **Rewrite** (`2`) to immediately send the captured selection with your custom system prompt to wiesel.run and its model provider. Review the original and result, then **Copy result** and paste it back. Wiesel does not automatically replace text.
- Choose **Chat** (`3` or Enter) for a conversation. Enter sends. **New chat** clears the conversation; messages can be copied individually. Chat has a single-line composer in this first version.
- **Summarize** (`4`) and **Explain** (`5`) send the captured selection with built-in prompts to wiesel.run and its model provider and show the result for review and copying.
- The bottom-right **Add quick action** tile has a plus icon and a **Soon** badge. Clicking it only shows a coming-soon message; custom action creation is not implemented yet.
- Escape or **Hide** hides Wiesel while keeping its global shortcut active. Command-Q quits it. The native titlebar controls are removed; drag the custom header text to move the fixed-size window.
- **Settings** lets you change your shortcut, model, prompts, or account login.

Text capture identifies the frontmost process using `NSWorkspace` before Wiesel takes focus. Accessibility is used only to check permission and reject focused fields identified as protected; selected text is captured exclusively using simulated **⌘C**:

1. Wait up to two seconds for Command/Control/Option/Shift to be released, without activating Wiesel.
2. Save **every clipboard item and advertised format** in memory, including rich text, images, and file representations. If any format cannot be read, or the clipboard exceeds the 64 MiB preservation limit, abort without sending Copy.
3. Send Copy keyboard events directly to the original process using a private event source. Wait up to 1.2 seconds for a fresh clipboard write and let it settle for 80 ms. The old clipboard text is never treated as a captured selection.
4. Read the copied plain text, restore the saved clipboard (including an originally empty clipboard), and only then open Wiesel. Non-text or empty Copy results also restore the clipboard and produce an error.

Capture aborts if the source process changes or a second clipboard version appears during the wait. In those cases the current clipboard is left untouched rather than overwritten with an older snapshot. Restoration also checks the clipboard version immediately before writing; if it detects a newer version, capture is discarded. **macOS has no atomic clipboard transaction or reliable writer identity**, so a concurrent writer can still race the checks or be mistaken for the initial Copy. Avoid copying elsewhere during capture. A crashed/killed app or a very late Copy response can also prevent restoration. Clipboard managers and Universal Clipboard may observe the temporary copied selection; restoring the clipboard does not erase their history.

Copy capture does not use AppleScript, Automation, Input Monitoring, or Screen Recording. Protected fields detected through Accessibility are never passed to Copy capture. Apps that disable Copy or do not put plain text on the clipboard still cannot be captured; paste text into Chat in those cases.

## Privacy and persistence

- Settings: `~/Library/Application Support/Wiesel/settings.json` (shortcut, model, prompts, onboarding completion; no credentials).
- Device bearer token, expiry and device ID: device-scoped macOS Keychain entries only (never settings, clipboard or logs). `desktop-auth.lock` alongside settings contains no credentials or state; it coordinates Keychain operations across app processes.
- Conversation, selected text, generated results, and temporary clipboard snapshots: memory only in Wiesel; not saved to disk. External clipboard managers may retain the temporary Copy selection.
- Text is sent to the fixed HTTPS Wiesel backend at `https://wiesel.run/v1` and its model provider **only after a writing action or Send**. Browser login, device status and authenticated model-catalog requests send no selected text. Wiesel receives only the text/chat fields needed for the action; provider routing and allowance enforcement are backend responsibilities.
- HTTP requests run off the UI thread, with connection/overall timeouts, no automatic retries or redirect following, and no desktop cookie jar. Async results are bound to their login generation and device identity; stale status/catalog/401/stream/exchange results are discarded after cancellation or sign-out. Keychain operations are serialized on the UI thread so stale exchanges cannot persist credentials. Failed chat sends restore the original draft only if the composer is empty; otherwise your new draft is preserved and **Restore failed message** keeps the failed text recoverable (except when the session is cleared).
- Chat streams text as it arrives; incomplete answers are not committed to conversation history. Network/stream ambiguity may mean the action was charged: do not retry blindly. HTTP 402 means insufficient allowance, 409 means already submitted/do not retry, and 429 means throttling. No local credit projection authorizes admission, and there is no cancellation/refund promise.
- Capturing a new selection during a request updates the next writing action without changing the in-flight request's original text.

## Notifications (developer API)

`src/notifications.rs` owns notification state, message normalization, severity colors, timing, and the shared renderer. `Wiesel` renders it once, outside all page branches.

```rust
// In a Wiesel handler (follow with cx.notify()):
self.notifications.success("Copied.");
self.notifications.warning("Select text in another app first.");
self.notifications.working(Source::Request, "Thinking…");

// Persistent errors have an owner and a short, actionable message:
self.notifications.issue(Source::Request, Severity::Error, gateway::failure_message(&error));
self.notifications.clear(Source::Request); // recover only this source
```

- Success, informational feedback, and input warnings last five seconds; timers pause while the window is hidden.
- Scoped issues remain until that source recovers or retries. Navigation does not clear errors. A new failure replaces stale feedback immediately. Brief feedback takes precedence, then persistent errors, active work, remaining warnings, and idle readiness. Equal-severity issues prefer the latest; older unresolved issues reappear when newer ones recover.
- Async workers return `ResultEvent`; the UI thread publishes notifications. Do not mutate UI state from a worker.
- Child components implement `EventEmitter<Notification>`, call `cx.emit(Notification::new(Severity::Warning, "No text available to paste."))`, and their owner forwards the event to `Notifications::push`. Text inputs and model-search input use this path.
- Messages normalize whitespace and cap at 96 Unicode graphemes. The renderer also enforces no wrapping and width-aware ellipsis. Never put raw errors, remote response bodies, credentials, or selected text in notification copy.
- Dot/text changes use a restrained 180ms ease-out opacity transition, no looping pulses or layout animation. Repeated identical statuses do not restart it. Keyboard feedback is immediate, and macOS Reduce Motion disables motion.
- Authentication and inference failures are published as client-safe messages without raw error logging. The HTTP components discard sensitive transport/source chains and remote bodies; never log callback/login URLs, state, verifier, tokens or credential payloads. Other local subsystems may emit diagnostic logs.
- Startup window creation failures occur before a bar exists and remain diagnostic-only; expected shutdown channel failures, empty sends, and protected-field Copy/Cut no-ops do not generate notifications.

### Status-bar acceptance checks

- Switch between Home, Chat, Writing, and Settings: the bottom bar must stay 48px high, without the old divider, quick-action count, or shortcut hints.
- Copy a chat message/result, save settings, sign out/log in, and capture text: verify concise green feedback.
- Try missing inputs, shortcut conflicts, unavailable/offline models, revoked permissions, and clipboard races: verify yellow/red messages in the bottom bar only.
- Leave an issue unresolved, trigger an unrelated success, and wait five seconds: the issue must return. Hide during feedback and reopen: it should still be readable.
- Try a long Unicode message at the minimum window width: it must ellipsize, never wrap or change bar height.
- Enable macOS Reduce Motion and verify static updates; keyboard validations should also be immediate.

## Validation

```sh
cargo fmt --check
cargo check --offline --locked
cargo test --offline --locked
# Focused offline native-wrapper tests (the live scenario stays ignored):
cargo test --offline --locked --test native_chat_smoke
cargo clippy --offline --all-targets --all-features --locked -- -D warnings
# Build/restart script tests (Python 3, mocked OS commands):
python3 scripts/test-app-scripts.py
# Release metadata and train/PR-note tests (Python 3.11+):
python3 scripts/release.py version
python3 scripts/test-release.py
```

Unit tests cover PKCE/callback grammar, single-use exchanges, fixed-origin authenticated wire contracts, sanitized failures, SSE bounds/fragmentation, mock Keychain lifecycle/native-delete status rejection, cross-process file-lock contention and pointer-mutation races, stale-device cleanup/retry identity, startup recovery, generation/device guards and stale-exchange persistence suppression, plus model search, settings, hotkeys, Unicode/IME, selection and draft preservation. Native clipboard tests use private pasteboards, not your general clipboard. Build/restart scripts use mocked OS commands. **These automated checks are local source/mocked validation, not live backend, website, browser, production Keychain, signed-bundle routing or deployment verification. No deployment or live request is implied.**

### Native chat UI smoke test (macOS, opt-in live request)

The two-layer strategy and complete Cargo/optional Nextest commands are in
[docs/testing.md](docs/testing.md). Normal `cargo test` runs the native wrapper
tests offline and ignores `native_chat_completed_reply`; it never launches the bundle
or invokes the live driver.

`scripts/ui-smoke.sh` compiles a small Swift Accessibility runner at
`target/ui-smoke/wiesel-ui-smoke`. It launches or activates the **specified bundle**,
presses the Chat tile, enters a prompt through the native text-field Accessibility
API, presses Send, and asserts a **committed** assistant reply. No private app IPC,
HTTP test client, clipboard access, coordinate clicks, or screenshot/OCR is used.
This MVP exercises the Send button, not the Enter key or global selection hotkey.

Build the current source into the bundle first. Quit Wiesel manually before using
`build-app.sh`; it refuses to replace a running bundle. Use your usual persistent
signing identity when configured, or `dev.sh` for your normal signed restart.

```sh
bash scripts/build-app.sh
# Compile/check the runner without launching Wiesel or sending requests:
bash scripts/ui-smoke.sh --self-test
# One-time authorization (does not grant permission automatically):
bash scripts/ui-smoke.sh --request-permission
# Launch and inspect selectors without sending a chat message:
bash scripts/ui-smoke.sh --preflight
# Submit ONE potentially billable live chat request and assert its reply:
bash scripts/ui-smoke.sh --live
# Optional custom prompt and exact expected answer:
bash scripts/ui-smoke.sh --live --prompt 'Reply with only: WIESEL_SMOKE_OK' \
  --expect WIESEL_SMOKE_OK --timeout 150
# Optional explicit bundle path:
bash scripts/ui-smoke.sh --preflight --app /absolute/path/to/Wiesel.app
```

**One-time manual prerequisites:** allow the terminal/runner under System Settings
→ Privacy & Security → Accessibility (separate from Wiesel's selected-text
permission). The helper prints its executable path; if needed, use the `+` button
and Command-Shift-G to add that path. macOS can attribute the helper to its hosting
terminal. Rebuilding the helper can require reauthorizing it. Complete browser
login and model setup in Wiesel using an authorized test account. Permission and
login are never bypassed or configured by the runner.

Start from the launcher or an **empty Chat with an empty composer**, with no request
pending. The runner refuses existing history/drafts instead of deleting them;
choose **New chat** and clear any draft manually before another run. Other running
copies of Wiesel are rejected. Leave the app alone during testing. Wiesel is left
open afterward; settings, Keychain entries, and the general clipboard are not
modified by the runner. Normal app startup can perform session/catalog requests
even in preflight mode.

The default prompt requests a fresh unique marker on each run. Success requires
that the submitted user message is displayed, a completed assistant message
matches the marker (ignoring surrounding whitespace), the request is idle, and
the composer is empty. Streaming previews cannot pass. This is a live-model smoke
test, not deterministic CI: model noncompliance, network issues, and allowance
failures can fail it. The reply deadline defaults to 150 seconds (`--timeout`,
1–600). There is **no automatic retry**, including after timeouts with unknown
billing outcomes. Inspect the app's status bar before deciding to rerun.

Logs contain phase/selector diagnostics, never prompt/reply contents or tokens.
No screenshots are captured. Exit codes: `0` success, `1` reply/assertion failure,
`2` permission/setup/usage failure. `--self-test` validates the runner's parser and
reply assertions only; it does **not** prove native UI or backend success.

#### Cargo / optional Nextest live entry point

After completing the manual prerequisites above, the ignored Rust integration
test runs exactly the same live scenario with the default unique-marker prompt:

```sh
WIESEL_UI_LIVE=1 cargo test --offline --locked --test native_chat_smoke \
  native_chat_completed_reply -- --ignored --exact --test-threads=1 --nocapture
# Optional bundle override (otherwise this checkout's dist/Wiesel.app):
WIESEL_UI_LIVE=1 WIESEL_UI_APP='/absolute/path/to/Wiesel.app' \
  cargo test --offline --locked --test native_chat_smoke \
  native_chat_completed_reply -- --ignored --exact --test-threads=1 --nocapture
# Optional, only if Cargo Nextest is already installed:
WIESEL_UI_LIVE=1 cargo nextest run --offline --locked --test native_chat_smoke \
  --run-ignored only -E 'test(=native_chat_completed_reply)' \
  --test-threads 1 --retries 0
```

`WIESEL_UI_LIVE=1` is a second explicit gate for the **Rust entry point**:
selecting the ignored test without it fails before any driver invocation. A
normal run reports the live scenario as ignored/skipped; once explicitly
selected, missing prerequisites or a nonzero driver exit are failures, not
skips. `WIESEL_UI_APP` is passed as a single bundle-path argument, resolving
relative paths from the repository root. Do not run live commands concurrently.
The Nextest default-profile override serializes this test and disables retries;
neither runner should be wrapped in a retry loop.

Cargo/Nextest receive sanitized exit-category diagnostics, never subprocess
stdout/stderr or message contents. The reply deadline is fixed at 150 seconds;
the wrapper's total driver deadline is 300 seconds (including compilation and
startup). On a process timeout it stops/reaps only its driver, never Wiesel,
and does not reset state or retry. A request may still be pending or charged.
Investigate setup with `--self-test` or an explicitly chosen manual `--preflight`;
inspect the app's status bar before considering another billable attempt.
Offline fake-driver tests and runner self-tests are not live-request evidence.

### Manual launch checklist (required before release)

Use a signed app bundle, test accounts and explicit permission for live requests. Do not record credential-bearing URLs or payloads in logs/screenshots.

- Verify the bundle declares URL name `run.wiesel.auth`, scheme `wiesel`. With the app running (also hidden), finish browser login and verify activation plus a catalog load. Quit, deliver an old callback for a cold launch, and verify it cannot exchange or sign in from that link; restart login instead. Verify the OS routes to the intended installed copy if multiple Wiesel bundles exist.
- Test **Login / Sign up**, Enter on focused login, and Command-Shift-L while text/model search is focused and while inference is pending. Cancel, reopen, timeout, replay a consumed callback, and deliver a wrong-state/malformed link. None may exchange against another attempt, write Keychain or revive a canceled session. Deliver an old exchange completion after cancel/reopen/sign-out and verify it is ignored.
- Test the real website with the external system browser: existing login, new signup, email verification followed by a fresh desktop attempt, web session expiry, and browser back/reload. Verify login-page/CSP navigation and scheme-launch permission/confirmation allow the callback without exposing the verifier or bearer.
- Unlock/lock/deny Keychain and test restore/save/delete failures, device replacement, local sign-out with remote network failure, and retrying local removal. Verify settings contain no tokens; old provider keys are removed, not reused. Test device revocation/401 separately on status, catalog and inference, expiry while idle and before a request, and relaunch after successful cleanup. Older device operations must not clear a new login. With two authorized app instances sharing the same User-domain Keychain and app-support directory, save B while A retains its old session; then sign out/expire/reject A and retry failed A cleanup. B's record and pointer must remain. Test native deletion rejection after a successful read (not merely lookup failure), verify fresh login remains blocked until a status-checked retry succeeds, and verify unreadable/corrupt startup recovery never deletes a later instance's login.
- Verify plan/virtual credits are informational. Test allowance rejection (402), duplicate submission (409), throttling (429), timeout/offline/server errors, redirects, a stream ending without DONE and a disconnect after partial text. Verify no automatic retry and no admission, cancellation or refund promise; ambiguous actions may have been charged.

Retain the existing local interaction checks:

- Complete onboarding, relaunch, and verify saved settings and a valid device login.
- Trigger the shortcut from another app; verify activation and fresh selected-text capture.
- In Brave, test an ordinary page selection (including text across paragraphs), a textarea, and no selection. Release the shortcut keys after pressing them.
- In Mail, test a received message's body separately from a compose field. Also test Unicode/emoji selections and verify text from another window or a previously selected field is not captured.
- Before capture, copy an unrelated rich-text snippet, an image, or a Finder file. After Wiesel opens, paste elsewhere and verify the previous clipboard is intact. Repeat with an empty clipboard and no selection; Wiesel must never use the old clipboard text as the selection.
- Hold the shortcut modifiers; Wiesel should wait, not send a modified Copy shortcut. Switch apps or change the clipboard during the wait; verify capture aborts without overwriting newer content.
- Test all four selected-text actions, copy/paste, and a multi-turn chat. Confirm the six-tile grid, shortcuts `1`–`5`, and the bottom-right Add placeholder (which must not send an AI request).
- Deny Accessibility, select no text, or use an unsupported app; verify no writing request occurs.
- Test a rejected/revoked device login, unavailable model, offline network, and shortcut conflicts.
- Hide/close/reopen with the shortcut; Command-Q should stop it.

Known first-version limits: no tray menu/autostart, no request cancellation, no automatic text replacement, no persistent chat history, no native selection/credential integration for Linux or Windows, and app selections require Accessibility permission and standard Copy support.

## Sources

- [GPUI examples](https://github.com/zed-industries/zed/tree/main/crates/gpui/examples). `src/input.rs` adapts the Apache-2.0 GPUI `input.rs` example and adds masking, horizontal scrolling, and Unicode fixes.
- [Wiesel browser login and backend](https://wiesel.run)
- [RFC 7636: Proof Key for Code Exchange](https://www.rfc-editor.org/rfc/rfc7636)
