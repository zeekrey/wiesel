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
