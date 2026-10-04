#!/usr/bin/env python3
"""Mock lifecycle tests: no Keychain changes, real app signals, or GUI launches."""
import os
from pathlib import Path
import shutil
import subprocess
import tempfile
import unittest

SCRIPTS = Path(__file__).resolve().parent
FINGERPRINT = "A" * 40
MOCK = r'''#!/usr/bin/env bash
set -eu
name="${0##*/}"
printf '%s %s\n' "$name" "$*" >> "$TEST_ROOT/events"
case "$name" in
  security)
    if [[ "${SCENARIO:-}" != invalid ]]; then
      printf '  1) AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA "Wiesel Development"\n     1 valid identities found\n'
    fi ;;
  cargo) [[ "${SCENARIO:-}" != compile ]] ;;
  codesign)
    [[ "${SCENARIO:-}" != sign ]] || exit 1
    if [[ "${1:-}" == --verify && "${SCENARIO:-}" == verify ]]; then exit 1; fi ;;
  ps)
    [[ "${SCENARIO:-}" != inspect ]] || exit 1
    /bin/cat "$TEST_ROOT/processes" ;;
  sleep) : ;;
  open) [[ "${SCENARIO:-}" != launch ]] ;;
  mv)
    if [[ "${SCENARIO:-}" == install && "$1" == */.stage.*/Wiesel.app ]]; then exit 1; fi
    /bin/mv "$@" ;;
esac
'''


class LifecycleTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix="wiesel test ")
        self.root = Path(self.temp.name).resolve() / "repo with spaces"
        shutil.copytree(SCRIPTS, self.root / "scripts")
        (self.root / "resources").mkdir()
        (self.root / "resources/Info.plist").write_text("plist")
        for profile in ("debug", "release"):
            target = self.root / "target" / profile
            target.mkdir(parents=True)
            (target / "wiesel").write_text("new executable")
        self.app = self.root / "dist/Wiesel.app"
        (self.app / "Contents/MacOS").mkdir(parents=True)
        (self.app / "Contents/MacOS/Wiesel").write_text("old executable")
        self.bin = self.root / "mock-bin"
        self.bin.mkdir()
        for name in ("cargo", "security", "codesign", "ps", "sleep", "open", "mv"):
            path = self.bin / name
            path.write_text(MOCK)
            path.chmod(0o755)
        self.processes = self.root / "processes"
        self.processes.write_text(f"123 {self.app}/Contents/MacOS/Wiesel\n456 /Applications/Wiesel.app/Contents/MacOS/Wiesel\n")

    def tearDown(self):
        self.temp.cleanup()

    def run_script(self, scenario="", identity="Wiesel Development", mode="dev", args=()):
        env = dict(os.environ, PATH=f"{self.bin}:/usr/bin:/bin", TEST_ROOT=str(self.root), SCENARIO=scenario)
        env.pop("WIESEL_SIGNING_IDENTITY", None)
        if identity is not None:
            env["WIESEL_SIGNING_IDENTITY"] = identity
        # Mock the shell builtin too: never signal a real process.
        runner = r'''
kill() {
    printf 'kill %s\n' "$*" >> "$TEST_ROOT/events"
    if [[ "${SCENARIO:-}" != timeout ]]; then
        grep -v '^123 ' "$TEST_ROOT/processes" > "$TEST_ROOT/processes.next"
        /bin/mv "$TEST_ROOT/processes.next" "$TEST_ROOT/processes"
    fi
}
export -f kill
bash "$TEST_ROOT/scripts/$1.sh" "${@:2}"
'''
        result = subprocess.run(["bash", "-c", runner, "test", mode, *args], env=env, text=True, capture_output=True, timeout=10)
        events = (self.root / "events").read_text().splitlines() if (self.root / "events").exists() else []
        return result, events

    def old_preserved(self):
        self.assertEqual((self.app / "Contents/MacOS/Wiesel").read_text(), "old executable")
        self.assertFalse((self.root / "dist/.build-lock").exists())
        self.assertEqual(list((self.root / "dist").glob(".stage.*")), [])

    def test_success_order_and_exact_path(self):
        result, events = self.run_script()
        self.assertEqual(result.returncode, 0, result.stderr)
        build = next(i for i, e in enumerate(events) if e.startswith("cargo "))
        verify = next(i for i, e in enumerate(events) if e.startswith("codesign --verify"))
        stop = events.index("kill -TERM 123")
        install = next(i for i, e in enumerate(events) if e.startswith("mv ") and e.endswith(str(self.app)))
        launch = next(i for i, e in enumerate(events) if e.startswith("open "))
        self.assertLess(build, verify)
        self.assertLess(verify, stop)
        self.assertLess(stop, install)
        self.assertLess(install, launch)
        self.assertNotIn("kill -TERM 456", events)
        self.assertIn(FINGERPRINT, "\n".join(events))
        self.assertEqual((self.app / "Contents/MacOS/Wiesel").read_text(), "new executable")
        self.assertIn("456 /Applications/", self.processes.read_text())
        self.assertFalse((self.root / "dist/.build-lock").exists())

    def test_failures_before_stop(self):
        for scenario in ("compile", "sign", "verify", "invalid", "inspect"):
            with self.subTest(scenario=scenario):
                (self.root / "events").write_text("")
                result, events = self.run_script(scenario=scenario)
                self.assertNotEqual(result.returncode, 0)
                self.assertFalse(any(e.startswith(("kill ", "open ")) for e in events))
                self.old_preserved()

    def test_missing_and_adhoc_rejected(self):
        for identity in (None, "-"):
            result, events = self.run_script(identity=identity)
            self.assertNotEqual(result.returncode, 0)
            self.assertIn("requires WIESEL_SIGNING_IDENTITY", result.stderr)
            self.assertEqual(events, [])
            self.old_preserved()

    def test_stop_timeout(self):
        result, events = self.run_script(scenario="timeout")
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("did not exit", result.stderr)
        self.assertEqual([e for e in events if e.startswith("kill ")], ["kill -TERM 123"])
        self.assertFalse(any(e.startswith("open ") for e in events))
        self.old_preserved()

    def test_install_failure_rolls_back(self):
        result, events = self.run_script(scenario="install")
        self.assertNotEqual(result.returncode, 0)
        self.assertFalse(any(e.startswith("open ") for e in events))
        self.old_preserved()

    def test_lock_not_removed_by_competing_invocation(self):
        lock = self.root / "dist/.build-lock"
        lock.mkdir()
        result, events = self.run_script()
        self.assertNotEqual(result.returncode, 0)
        self.assertTrue(lock.exists())
        self.assertFalse(any(e.startswith("cargo ") for e in events))

    def test_build_refuses_running_app(self):
        result, events = self.run_script(mode="build-app", identity="-")
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("bundle is running", result.stderr)
        self.assertFalse(any(e.startswith(("kill ", "open ")) for e in events))
        self.old_preserved()

    def test_adhoc_standalone_build_and_release(self):
        self.processes.write_text("456 /Applications/Wiesel.app/Contents/MacOS/Wiesel\n")
        result, events = self.run_script(mode="build-app", identity=None, args=("--release",))
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("cargo build --release --locked", events)
        self.assertTrue(any(e.startswith("codesign --force --sign - ") for e in events))
        self.assertFalse(any(e.startswith(("kill ", "open ")) for e in events))

    def test_local_configuration_and_environment_override(self):
        (self.root / ".wiesel-dev.env").write_text("WIESEL_SIGNING_IDENTITY='Wiesel Development'\n")
        result, _ = self.run_script(identity=None)
        self.assertEqual(result.returncode, 0, result.stderr)
        result, _ = self.run_script(identity="-")
        self.assertNotEqual(result.returncode, 0)

    def test_launch_failure_reports_error_and_keeps_new_bundle(self):
        result, _ = self.run_script(scenario="launch")
        self.assertNotEqual(result.returncode, 0)
        self.assertEqual((self.app / "Contents/MacOS/Wiesel").read_text(), "new executable")
        self.assertFalse((self.root / "dist/.build-lock").exists())


if __name__ == "__main__":
    unittest.main(verbosity=2)
