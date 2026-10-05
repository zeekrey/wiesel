# Wiesel

A small, Raycast-inspired native writing launcher built with Rust and GPUI. **This first version supports macOS only**; other targets are explicitly rejected until their native selection and persistent credential backends are implemented.

## Install a release

Download a DMG from this repository's GitHub **Releases** page: choose
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

For CI, CLI-controlled Knope releases, contributor instructions and one-time GitHub configuration,
see [CONTRIBUTING.md](CONTRIBUTING.md).

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

1. Record a global shortcut or enter one such as `Super+Shift+Space` (`Super` is Command on macOS). Recorded shortcuts activate and save immediately; after typing a shortcut, click **Apply shortcut**. The shortcut works independently of AI Gateway setup, including before onboarding is complete. The default shortcut is registered at launch. Avoid shortcuts already owned by the OS or another app. Registration failures are shown during setup.
2. Create an **AI Gateway API key** in the [Vercel dashboard](https://vercel.com/dashboard/ai-gateway/api-keys), paste it, and select **Verify & connect**. Wiesel verifies it using authenticated `GET /v1/credits`; the public models endpoint is not used as proof of authentication. A Vercel account and available Gateway credit are required for generation.
3. Open the **Model** dropdown and select a model from AI Gateway's live catalog. Search by provider or model name; use ↑/↓ and Enter or click an option. The catalog loads automatically without requiring an API key; **Refresh models** retries or updates it. It includes all available model IDs, so choose a text/chat model for the writing and chat features. The selection is saved with **Save & start**.
4. Optionally edit the two writing system prompts. Select **Save & start**.
5. Grant the built **Wiesel.app** access under **System Settings → Privacy & Security → Accessibility** (shown as **Gerätesteuerung und Datenzugriff** on some German macOS versions). Accessibility is required for simulated Copy and protected-field checks; Input Monitoring, Screen Recording, and Automation are not required. On newer macOS versions, allow Wiesel to paste from other apps if asked: Copy capture reads the clipboard to save and restore it. When running with Cargo instead, macOS may associate permission with the terminal or binary.

Every screen has the same **48px bottom status bar**: green for success/ready, yellow for missing input or permissions, red for failures, and muted for work in progress. This is the only notification surface; messages stay on one line. Accessibility is checked for the running process every second using macOS's `AXIsProcessTrusted()`. **Privacy settings** and **Check again** are available in Settings. If Wiesel still requests access after a rebuild despite the macOS toggle being enabled, remove the old entry, add the current `dist/Wiesel.app`, and relaunch. Permission does not guarantee that the target app supports Copy.

This uses Vercel's documented API-key authentication, not a browser OAuth login. Credentials are stored in macOS Keychain under `com.wiesel.ai-gateway`; they are never written into settings. **Disconnect** deletes the stored key. Keys are masked in the UI; copying/cutting them is disabled.

## Use

- Leave Wiesel running. Select text in another app, press your shortcut, then release the shortcut keys. Keep the source app active until Wiesel opens; Wiesel uses simulated ⌘C and restores your clipboard before opening.
- Home shows six quick-action tiles in a three-column, two-row grid, each with a Lucide icon.
- Choose **Fix spelling** (`1`) or **Rewrite** (`2`) to immediately send the captured selection with your custom system prompt to AI Gateway. Review the original and result, then **Copy result** and paste it back. Wiesel does not automatically replace text.
- Choose **Chat** (`3` or Enter) for a conversation. Enter sends. **New chat** clears the conversation; messages can be copied individually. Chat has a single-line composer in this first version.
- **Summarize** (`4`) and **Explain** (`5`) send the captured selection with built-in prompts to AI Gateway and show the result for review and copying.
- The bottom-right **Add quick action** tile has a plus icon and a **Soon** badge. Clicking it only shows a coming-soon message; custom action creation is not implemented yet.
- Escape or **Hide** hides Wiesel while keeping its global shortcut active. Command-Q quits it. The native titlebar controls are removed; drag the custom header text to move the fixed-size window.
- **Settings** lets you change your shortcut, model, prompts, or API key.

Text capture identifies the frontmost process using `NSWorkspace` before Wiesel takes focus. Accessibility is used only to check permission and reject focused fields identified as protected; selected text is captured exclusively using simulated **⌘C**:

1. Wait up to two seconds for Command/Control/Option/Shift to be released, without activating Wiesel.
2. Save **every clipboard item and advertised format** in memory, including rich text, images, and file representations. If any format cannot be read, or the clipboard exceeds the 64 MiB preservation limit, abort without sending Copy.
3. Send Copy keyboard events directly to the original process using a private event source. Wait up to 1.2 seconds for a fresh clipboard write and let it settle for 80 ms. The old clipboard text is never treated as a captured selection.
4. Read the copied plain text, restore the saved clipboard (including an originally empty clipboard), and only then open Wiesel. Non-text or empty Copy results also restore the clipboard and produce an error.

Capture aborts if the source process changes or a second clipboard version appears during the wait. In those cases the current clipboard is left untouched rather than overwritten with an older snapshot. Restoration also checks the clipboard version immediately before writing; if it detects a newer version, capture is discarded. **macOS has no atomic clipboard transaction or reliable writer identity**, so a concurrent writer can still race the checks or be mistaken for the initial Copy. Avoid copying elsewhere during capture. A crashed/killed app or a very late Copy response can also prevent restoration. Clipboard managers and Universal Clipboard may observe the temporary copied selection; restoring the clipboard does not erase their history.

Copy capture does not use AppleScript, Automation, Input Monitoring, or Screen Recording. Protected fields detected through Accessibility are never passed to Copy capture. Apps that disable Copy or do not put plain text on the clipboard still cannot be captured; paste text into Chat in those cases.

## Privacy and persistence

- Settings: `~/Library/Application Support/Wiesel/settings.json` (shortcut, model, prompts, onboarding completion; no credentials).
- API key: macOS Keychain.
- Conversation, selected text, generated results, and temporary clipboard snapshots: memory only in Wiesel; not saved to disk. External clipboard managers may retain the temporary Copy selection.
- Text is sent to Vercel AI Gateway and the selected model provider **only after a writing action or Send**. Account verification makes a metadata request but sends no selected text.
- Requests run off the UI thread, with connection and overall timeouts. Errors are shown in the app. Failed chat sends restore the original draft only if the composer is empty; otherwise your new draft is preserved and **Restore failed message** keeps the failed text recoverable.
- Capturing a new selection during a request updates the next writing action without changing the in-flight request's original text.

## Notifications (developer API)

`src/notifications.rs` owns notification state, message normalization, severity colors, timing, and the shared renderer. `Wiesel` renders it once, outside all page branches.

```rust
// In a Wiesel handler (follow with cx.notify()):
self.notifications.success("Copied.");
self.notifications.warning("Select text in another app first.");
self.notifications.working(Source::Request, "Thinking…");

// Persistent errors have an owner and a short, actionable message:
self.notifications.report(Source::Request, "Request failed. Try again.", &error);
self.notifications.clear(Source::Request); // recover only this source
```

- Success, informational feedback, and input warnings last five seconds; timers pause while the window is hidden.
- Scoped issues remain until that source recovers or retries. Navigation does not clear errors. A new failure replaces stale feedback immediately. Brief feedback takes precedence, then persistent errors, active work, remaining warnings, and idle readiness. Equal-severity issues prefer the latest; older unresolved issues reappear when newer ones recover.
- Async workers return `ResultEvent`; the UI thread publishes notifications. Do not mutate UI state from a worker.
- Child components implement `EventEmitter<Notification>`, call `cx.emit(Notification::new(Severity::Warning, "No text available to paste."))`, and their owner forwards the event to `Notifications::push`. Text inputs and model-search input use this path.
- Messages normalize whitespace and cap at 96 Unicode graphemes. The renderer also enforces no wrapping and width-aware ellipsis. Never put raw errors, remote response bodies, credentials, or selected text in notification copy.
- Dot/text changes use a restrained 180ms ease-out opacity transition, no looping pulses or layout animation. Repeated identical statuses do not restart it. Keyboard feedback is immediate, and macOS Reduce Motion disables motion.
- Detailed error chains remain in diagnostic logs; Gateway HTTP errors discard remote bodies and map authentication, credit, rate-limit, outage, timeout, and invalid-response failures to safe messages.
- Startup window creation failures occur before a bar exists and remain diagnostic-only; expected shutdown channel failures, empty sends, and protected-field Copy/Cut no-ops do not generate notifications.

### Status-bar acceptance checks

- Switch between Home, Chat, Writing, and Settings: the bottom bar must stay 48px high, without the old divider, quick-action count, or shortcut hints.
- Copy a chat message/result, save settings, disconnect/reconnect, and capture text: verify concise green feedback.
- Try missing inputs, shortcut conflicts, unavailable/offline models, revoked permissions, and clipboard races: verify yellow/red messages in the bottom bar only.
- Leave an issue unresolved, trigger an unrelated success, and wait five seconds: the issue must return. Hide during feedback and reopen: it should still be readable.
- Try a long Unicode message at the minimum window width: it must ellipsize, never wrap or change bar height.
- Enable macOS Reduce Motion and verify static updates; keyboard validations should also be immediate.

## Validation

```sh
cargo fmt --check
cargo check --locked
cargo test --locked
cargo clippy --locked -- -D warnings
# Build/restart script tests (Python 3, mocked OS commands):
python3 scripts/test-app-scripts.py
# Knope release metadata and pending change validation:
knope get-version
knope validate --dry-run
```

Unit tests cover response/model-catalog parsing, model search, settings serialization, hotkey validation, Unicode/IME offsets, selection snapshots across in-flight requests, failed-chat draft preservation, Copy release/timeout/focus handling, and clipboard preservation/races. Native clipboard tests use private pasteboards, not your general clipboard. Manual acceptance checks require your macOS permissions and real Gateway key:

- Complete onboarding, relaunch, and verify saved settings/key.
- Trigger the shortcut from another app; verify activation and fresh selected-text capture.
- In Brave, test an ordinary page selection (including text across paragraphs), a textarea, and no selection. Release the shortcut keys after pressing them.
- In Mail, test a received message's body separately from a compose field. Also test Unicode/emoji selections and verify text from another window or a previously selected field is not captured.
- Before capture, copy an unrelated rich-text snippet, an image, or a Finder file. After Wiesel opens, paste elsewhere and verify the previous clipboard is intact. Repeat with an empty clipboard and no selection; Wiesel must never use the old clipboard text as the selection.
- Hold the shortcut modifiers; Wiesel should wait, not send a modified Copy shortcut. Switch apps or change the clipboard during the wait; verify capture aborts without overwriting newer content.
- Test all four selected-text actions, copy/paste, and a multi-turn chat. Confirm the six-tile grid, shortcuts `1`–`5`, and the bottom-right Add placeholder (which must not send an AI request).
- Deny Accessibility, select no text, or use an unsupported app; verify no writing request occurs.
- Test a wrong/revoked key, unavailable model, offline network, and shortcut conflicts.
- Hide/close/reopen with the shortcut; Command-Q should stop it.

Known first-version limits: no tray menu/autostart, no response streaming or cancellation, no automatic text replacement, no persistent chat history, no native selection/credential integration for Linux or Windows, and app selections require Accessibility permission and standard Copy support.

## Sources

- [GPUI examples](https://github.com/zed-industries/zed/tree/main/crates/gpui/examples). `src/input.rs` adapts the Apache-2.0 GPUI `input.rs` example and adds masking, horizontal scrolling, and Unicode fixes.
- [Vercel AI Gateway](https://vercel.com/docs/ai-gateway)
- [OpenAI-compatible Chat Completions](https://vercel.com/docs/ai-gateway/sdks-and-apis/openai-chat-completions)
- [API keys](https://vercel.com/docs/ai-gateway/authentication-and-byok/api-keys)
- [REST API / credits](https://vercel.com/docs/ai-gateway/sdks-and-apis/rest-api)
