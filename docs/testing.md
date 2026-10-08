# Testing Wiesel

We use two complementary layers: Rust/GPUI tests for fast regression
coverage, and a Swift Accessibility smoke test for the actual macOS bundle.

## 1. Fast logic and chat-state tests

Ordinary Rust unit tests cover isolated logic. `#[gpui::test]` scenarios in
`src/chat_tests.rs` simulate clicks, typing, and Enter through the real chat UI.
A test-only controlled backend records requests and explicitly releases streaming
deltas, replies, or failures through the application's normal scoped event path.
Assertions cover history, committed versus partial replies, busy/composer state,
duplicate sends, draft recovery, authentication, and stale-session/device events.
The fixture avoids user settings, credentials, clipboard, hotkeys, and network.

```sh
cargo test --offline --locked
cargo test --offline --locked --bin wiesel chat_tests
# Optional suite runner, if Cargo Nextest is installed:
cargo nextest run --offline --locked
```

GPUI test support is a dev dependency; cache dependencies with `cargo fetch`
before running offline on a fresh machine. These tests do not verify native
rendering, the installed bundle, or a live provider.

## 2. Native Accessibility smoke test

`tests/native_chat_smoke.rs::native_chat_completed_reply` invokes the existing
`scripts/ui-smoke.sh` Swift driver from the repository root. It launches/activates
Wiesel, opens Chat, enters a unique-marker prompt, presses Send, and asserts a
**committed** matching assistant reply, idle request, and empty composer.

Normal Cargo/Nextest runs ignore this scenario. The other wrapper tests use local
fake subprocesses and do not launch Wiesel. Validate them without a live request:

```sh
cargo test --offline --locked --test native_chat_smoke
bash scripts/ui-smoke.sh --self-test
```

For one explicitly authorized, potentially billable request:

```sh
WIESEL_UI_LIVE=1 cargo test --offline --locked --test native_chat_smoke \
  native_chat_completed_reply -- --ignored --exact --test-threads=1 --nocapture
# Optional Nextest equivalent:
WIESEL_UI_LIVE=1 cargo nextest run --offline --locked --test native_chat_smoke \
  --run-ignored only -E 'test(=native_chat_completed_reply)' \
  --test-threads 1 --retries 0
```

Selecting the ignored test without exact `WIESEL_UI_LIVE=1` fails before invoking
the driver. Complete [manual setup](../README.md#native-chat-ui-smoke-test-macos-opt-in-live-request):
built bundle, runner Accessibility permission, login/text model, empty chat/draft,
and no pending request. Optionally set `WIESEL_UI_APP` to a bundle path; relative
paths resolve from the repository root. Never run live commands concurrently.
The Nextest default-profile override serializes the live test and disables retries.

The wrapper suppresses subprocess output and reports sanitized failures. Reply
and total driver deadlines are 150 and 300 seconds. Timeout stops/reaps only the
direct driver process, not Wiesel or any compiler descendant. No automatic retry,
state reset, login bypass, or credential/message logging is allowed. An ambiguous
submission may have been charged; inspect Wiesel before deciding to rerun.
`--preflight` sends no chat message but can cause normal startup session/catalog requests.


## 3. Local diagnostic integration (offline)

```sh
cargo test --offline --locked --bin wiesel diagnostic
cargo test --offline --locked --bin wiesel gateway::tests
cargo test --offline --locked --bin wiesel chat_tests
cargo test --offline --locked --bin wiesel notifications::tests
cargo test --offline --locked --all-targets
cargo fmt --all -- --check
cargo clippy --offline --all-targets --all-features --locked -- -D warnings
```

The logger is registered in the actual application, not an external test harness.
Storage/control tests create private disposable directories beneath the canonical
temporary folder. Gateway tests use local mock HTTP servers and synthetic private
markers, verifying status/stage/elapsed/action correlation while keeping bodies,
URLs, tokens and replies absent from persisted JSONL. They preserve the conservative
unknown-outcome billing warning and no-retry policy. Finder tests inspect command
arguments without launching Finder. In-process chat fixtures never initialize the
real logger; injected initialization/control results test nonfatal behavior and
completion independence from login generation. No test reads credentials, clears
the user's logs or sends authenticated inference.

### Manual Settings smoke checks (disposable macOS test account)

These checks are **not** automated acceptance evidence. Use a disposable macOS
account with test-only logs/settings; do not clear an ordinary user's logs as a test.
No login, credential inspection or live inference is needed for the local controls.

1. Launch the bundle, open Settings before any login/request, and find Diagnostics
   by scrolling. Check privacy/retention text wraps within the existing layout and
   the bottom notification bar stays 48px high at the minimum supported width.
2. Click **Open Logs Folder**: Finder opens `~/Library/Logs/Wiesel`. Check Tab focus,
   Enter/Space activation, button labels and VoiceOver descriptions. Check that
   in-progress controls do not enqueue duplicate actions. No shell is involved.
3. Inspect JSONL locally: version/timestamp/session/action/category/kind and optional
   failure/stage/status/elapsed, validated request model_id, and raw error_type.
   Types up to 256 UTF-8 bytes are preserved exactly; larger/non-string types and
   malformed/oversized envelopes omit the type but retain HTTP status. Check that
   control characters are JSON-escaped, not additional log lines. Other response
   fields and request draft/reply/URL/token/other settings values should not appear.
   Error types are server-controlled: review them for sensitive text before sharing. IDs are diagnostic correlation, never billing proof.
4. Click **Clear Logs** and wait for the bottom-bar result. Old recognized files
   disappear, a fresh active file and the coordination lock remain, and new typed
   events (including clear completion) can be written. Chat/settings remain unchanged.
5. Launch a second instance in the test account and retry Clear Logs: it must refuse
   with a close-other-instances message, not partial deletion. Close the second
   instance and retry; then inspect fresh logging. Do not remove the lock manually.
6. In the disposable account only, use an unavailable/unsafe log path or denied
   folder permissions before launch: startup and unauthenticated UI remain usable,
   a safe diagnostic warning is shown, and controls do not claim success. Restore
   permissions/path and relaunch. Finder/clear failures must be visible, not hidden.
7. Quit normally and inspect completed local events. Forced termination/stalled disk
   can lose logs; retention runs on init/write/flush, not exactly at seven days while
   idle. Active files are protected and cannot force writes over the 20 MiB budget.

For gateway unknown-outcome evidence, prefer the offline local HTTP/partial-stream/
timeout tests above. A future explicitly authorized live reproduction can correlate
its local diagnostic action and stages, but must retain the may-have-been-charged
warning and must not be automatically retried. No live reproduction was performed
as part of this integration.
