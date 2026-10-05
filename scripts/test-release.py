#!/usr/bin/env python3
"""Stdlib unit and local bare-origin integration tests; never contact GitHub."""
import importlib.util
import json
import os
from pathlib import Path
import plistlib
import shutil
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import patch

SCRIPT = Path(__file__).with_name("release.py").resolve()
SPEC = importlib.util.spec_from_file_location("release", SCRIPT)
release = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(release)

MOCK_GH = '''#!/usr/bin/env python3
import json, os, sys
from pathlib import Path
args = sys.argv[1:]
data = json.loads(Path(os.environ["GH_FIXTURE"]).read_text())
if args[0] == "repo":
    print("https://github.com/example/wiesel")
elif args[-1].endswith("/releases"):
    print(json.dumps([data["releases"]]))
elif "/commits/" in args[-1]:
    commit = args[-1].split("/commits/")[1].split("/")[0]
    print(json.dumps([data.get("pulls", {}).get(commit, [])]))
else:
    sys.exit("Unexpected gh command: " + repr(args))
'''


class ParserTests(unittest.TestCase):
    def test_comments_headings_na_and_bullets(self):
        self.assertEqual(release.parse_notes("## Summary\nHi\n## Release Notes:\n<!-- - TODO -->\n- Fix launch\n- Keep [links](https://example.com)\n## Tests\n- TODO"),
                         ["Fix launch", "Keep [links](https://example.com)"])
        self.assertEqual(release.parse_notes("Release Notes:\n- N/A\n# Validation\nStuff"), [])
        self.assertEqual(release.parse_notes("## Release Notes\n- Lower latency"), ["Lower latency"])

    def test_invalid_sections(self):
        for body in (None, "", "- A note", "## Release Notes:\n", "## Release Notes:\n-",
                     "## Release Notes:\n- N/A\n- Another change", "## Release Notes:\n- N/A\n- N/A",
                     "## Release Notes:\nprose", "## Release Notes:\n- TODO",
                     "## Release Notes:\n- Describe the user-facing change here",
                     "## Release Notes:\n- Add a short user-facing note here",
                     "## Release Notes:\n- <your note>", "## Release Notes:\n- ...",
                     "Release Notes:\n<!-- Replace with notes -->\n- Added/Fixed/Improved ...",
                     "## Release Notes:\n- A\n## Release Notes:\n- B"):
            with self.subTest(body=body), self.assertRaises(release.ReleaseError):
                release.parse_notes(body)

    def test_body_is_data_not_a_command(self):
        text = '$(touch /tmp/wiesel-release-should-not-exist) `whoami` ${TOKEN}'
        self.assertEqual(release.parse_notes("## Release Notes:\n- " + text), [text])

    def test_numeric_and_ref_safety(self):
        for value in ("01.2.3", "1.2.3-pre", "v1.2.3", "1.2", "-1.2.3"):
            with self.subTest(value=value), self.assertRaises(release.ReleaseError):
                release.version_tuple(value)
        for value in ("main", "--upload-pack=bad", "v1.2.x/other", "v01.2.x", "v1.2.0"):
            with self.subTest(value=value), self.assertRaises(release.ReleaseError):
                release.branch_parts(value)
        for value in ("v1.2.3-rc1", "v1.2", "v01.2.3", "v1.2.3;echo bad"):
            with self.subTest(value=value), self.assertRaises(release.ReleaseError):
                release.tag_parts(value)

    def test_subprocess_failure_is_explained(self):
        with patch.object(release.subprocess, "run", return_value=subprocess.CompletedProcess(["git"], 1, "", "permission denied")):
            with self.assertRaisesRegex(release.ReleaseError, "(?s)git fetch.*permission denied"):
                release.git("fetch")


class GitTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix="release tests ")
        self.root = Path(self.temp.name)
        self.remote = self.root / "origin.git"
        self.repo = self.root / "checkout with spaces"
        self.env = dict(os.environ, GIT_CONFIG_NOSYSTEM="1", GIT_CONFIG_GLOBAL=os.devnull,
                        GIT_TERMINAL_PROMPT="0")
        self.env.pop("GITHUB_OUTPUT", None)
        self.call("git", "init", "--bare", str(self.remote), cwd=self.root)
        self.call("git", "init", "-b", "main", str(self.repo), cwd=self.root)
        self.git("config", "user.name", "Release Test")
        self.git("config", "user.email", "test@example.invalid")
        self.git("remote", "add", "origin", str(self.remote))
        (self.repo / "Cargo.toml").write_text('[package]\nname = "wiesel"\nversion = "0.1.0"\n[dependencies]\nother = "2"\n')
        (self.repo / "Cargo.lock").write_text('version = 4\n\n[[package]]\nname = "other"\nversion = "8.9.0"\n\n[[package]]\nname = "wiesel"\nversion = "0.1.0"\n')
        (self.repo / "resources").mkdir()
        (self.repo / "resources/Info.plist").write_bytes(plistlib.dumps({"CFBundleShortVersionString": "0.1.0", "CFBundleVersion": "1"}))
        self.commit("Historical initial commit")
        self.initial = self.git("rev-parse", "HEAD")
        (self.repo / "scripts").mkdir()
        shutil.copyfile(SCRIPT, self.repo / "scripts/release.py")
        self.commit("Adopt Release Notes policy")
        self.marker = self.git("rev-parse", "HEAD")
        self.git("push", "origin", "main")
        self.git("fetch", "origin")
        self.bin = self.root / "bin"
        self.bin.mkdir()
        (self.bin / "gh").write_text(MOCK_GH)
        (self.bin / "gh").chmod(0o755)
        self.fixture = self.root / "gh.json"
        self.data = {"releases": [], "pulls": {}}
        self.save_fixture()
        self.env.update(PATH=str(self.bin) + os.pathsep + os.environ["PATH"], GH_FIXTURE=str(self.fixture))

    def tearDown(self):
        self.temp.cleanup()

    def call(self, *args, cwd=None, check=True):
        result = subprocess.run(args, cwd=cwd or self.repo, env=self.env, capture_output=True, text=True, timeout=30)
        if check:
            self.assertEqual(result.returncode, 0, f"{args}\n{result.stderr}\n{result.stdout}")
        return result

    def git(self, *args):
        return self.call("git", *args).stdout.strip()

    def commit(self, message):
        self.git("add", ".")
        self.git("commit", "-m", message)
        return self.git("rev-parse", "HEAD")

    def save_fixture(self):
        self.fixture.write_text(json.dumps(self.data))

    def cli(self, *args, success=True):
        result = self.call(sys.executable, str(SCRIPT), *args, check=False)
        if success:
            self.assertEqual(result.returncode, 0, result.stderr)
        else:
            self.assertNotEqual(result.returncode, 0, result.stdout)
        return result

    def main(self):
        self.git("switch", "main")

    def cut(self):
        return json.loads(self.cli("prepare", "--action", "cut").stdout)

    def published(self, tag, prerelease=True):
        resolved = self.call("git", "rev-parse", "--verify", f"refs/tags/{tag}^{{commit}}", check=False)
        source = resolved.stdout.strip() if resolved.returncode == 0 else "f" * 40
        self.data["releases"].append(dict(tag_name=tag, draft=False, prerelease=prerelease,
                                          published_at="2026-01-01T00:00:00Z", target_commitish=source))
        self.save_fixture()

    def metadata(self):
        return json.loads((self.repo / release.METADATA).read_text())

    def pull(self, commit, number=1, body="## Release Notes:\n- Faster launch", merge=None):
        pr = dict(number=number, body=body, merged_at="2026-01-01T00:00:00Z",
                  merge_commit_sha=merge or commit, html_url=f"https://github.com/example/wiesel/pull/{number}")
        self.data["pulls"].setdefault(commit, []).append(pr)
        self.save_fixture()
        return pr

    def test_version_output_and_cross_file_mismatch(self):
        self.assertEqual(self.cli("version").stdout, "0.1.0\n")
        path = self.repo / "Cargo.lock"
        path.write_text(path.read_text().replace('version = "0.1.0"', 'version = "0.1.1"'))
        self.assertIn("Version mismatch", self.cli("version", success=False).stderr)

    def test_version_checks_plist_and_numeric_format(self):
        path = self.repo / "resources/Info.plist"
        path.write_bytes(plistlib.dumps({"CFBundleShortVersionString": "0.2.0"}))
        self.assertIn("Version mismatch", self.cli("version", success=False).stderr)
        path.write_bytes(plistlib.dumps({"CFBundleShortVersionString": "0.01.0"}))
        self.assertIn("Invalid numeric version", self.cli("version", success=False).stderr)

    def test_version_rejects_duplicate_lock_package(self):
        path = self.repo / "Cargo.lock"
        path.write_text(path.read_text() + '\n[[package]]\nname = "wiesel"\nversion = "0.1.0"\n')
        self.assertIn("exactly one", self.cli("version", success=False).stderr)

    def test_check_pr_cli_event_and_no_execution(self):
        event = self.root / "event.json"
        sentinel = self.root / "do-not-create"
        event.write_text(json.dumps({"pull_request": {"body": f"## Release Notes:\n- $(touch '{sentinel}')"}}))
        self.cli("check-pr", "--event-file", str(event))
        self.assertFalse(sentinel.exists())
        event.write_text(json.dumps({"pull_request": {"body": "## Summary\nOnly"}}))
        self.assertIn("Release Notes", self.cli("check-pr", "--event-file", str(event), success=False).stderr)

    def test_cut_outputs_versions_metadata_and_main_untouched(self):
        output = self.root / "outputs"
        self.env["GITHUB_OUTPUT"] = str(output)
        data = self.cut()
        self.assertEqual(data["tag"], "v0.2.0-pre")
        self.assertEqual(data["branch"], "v0.2.x")
        self.assertEqual(data["channel"], "preview")
        self.assertEqual(data["version"], "0.2.0")
        self.assertEqual(data["source"], self.git("rev-parse", "HEAD"))
        self.assertEqual(dict(line.split("=", 1) for line in output.read_text().splitlines()), data)
        self.assertEqual(self.cli("version").stdout.strip(), "0.2.0")
        self.assertIn('version = "8.9.0"', (self.repo / "Cargo.lock").read_text())
        self.assertEqual(self.metadata(), dict(channel="preview", base_sha=None, migration_sha=self.marker))
        self.assertEqual(self.git("rev-parse", "main"), self.marker)
        changed = self.git("diff-tree", "--no-commit-id", "--name-only", "-r", "HEAD").splitlines()
        self.assertEqual(set(changed), set((*release.VERSION_FILES, release.METADATA)))
        self.assertEqual(self.git("status", "--porcelain"), "")
        self.assertEqual(self.git("ls-remote", "origin", "refs/tags/" + data["tag"]).split()[0], data["source"])

    def test_workflow_shaped_empty_branch_argument(self):
        data = json.loads(self.cli("prepare", "--action", "cut", "--branch", "").stdout)
        self.assertEqual(data["tag"], "v0.2.0-pre")
        self.assertEqual(data["version"], "0.2.0")
        self.main()
        for action in ("patch", "promote"):
            self.assertIn("Invalid release branch", self.cli("prepare", "--action", action,
                                                             "--branch", "", success=False).stderr)
        self.assertIn("do not supply --branch", self.cli("prepare", "--action", "cut",
                                                        "--branch", "v0.3.x", success=False).stderr)

    def test_next_cut_considers_remote_trains_and_tags(self):
        self.git("branch", "v0.4.x")
        self.git("push", "origin", "v0.4.x")
        self.git("tag", "v0.5.8")
        self.git("push", "origin", "v0.5.8")
        self.assertEqual(self.cut()["tag"], "v0.6.0-pre")

    def test_cut_requires_migration_marker(self):
        self.git("switch", "--detach", self.initial)
        self.git("push", "origin", "HEAD:main", "--force")  # Fixture only, production never forces.
        self.assertIn("migration marker", self.cli("prepare", "--action", "cut", success=False).stderr)

    def test_migration_boundary_includes_normal_merge_of_tooling_pr(self):
        self.git("switch", "-c", "adoption")
        self.main()
        self.git("reset", "--hard", self.initial)  # Reconstruct a non-squashed adoption PR, fixture only.
        self.git("merge", "--no-ff", "adoption", "-m", "Merge release tooling PR")
        merged = self.git("rev-parse", "HEAD")
        self.git("push", "origin", "main")
        self.pull(self.marker, 14, "Legacy tooling PR without notes", merge=merged)
        self.pull(merged, 14, "Legacy tooling PR without notes", merge=merged)
        cut = self.cut()
        self.assertEqual(self.metadata()["migration_sha"], merged)
        self.main()
        output = self.root / "notes.md"
        result = self.cli("notes", "--tag", cut["tag"], "--output", str(output))
        self.assertIn("Skipping historical PR #14", result.stderr)
        self.assertIn("No user-facing changes.", output.read_text())

    def test_cut_baseline_nearest_reachable_published_stable(self):
        self.git("tag", "v0.0.1", self.initial)
        self.git("tag", "v0.1.0", self.marker)
        self.git("tag", "v0.1.1", self.marker)
        self.git("push", "origin", "--tags")
        self.published("v0.0.1", False)
        self.published("v0.1.0", False)
        self.data["releases"].append(dict(tag_name="v0.1.1", prerelease=False, draft=True, published_at=None))
        self.save_fixture()
        self.cut()
        self.assertEqual(self.metadata()["base_sha"], self.marker)

    def test_new_train_uses_divergent_stable_and_excludes_shipped_backports(self):
        (self.repo / "first-feature").write_text("first")
        feature = self.commit("First train feature")
        self.git("push", "origin", "main")
        self.pull(feature, 1, "Release Notes:\n- First train feature")
        cut = self.cut()
        self.main()
        (self.repo / "fix").write_text("fix")
        original = self.commit("Fix on main")
        self.git("push", "origin", "main")
        self.pull(original, 2, "Release Notes:\n- Fixed a shipped issue")
        self.git("switch", "--detach", "origin/" + cut["branch"])
        self.git("cherry-pick", original)
        backport = self.git("rev-parse", "HEAD")
        self.pull(backport, 3, "Cherry-pick of #2\n\nRelease Notes:\n- Fixed a shipped issue")
        self.git("push", "origin", "HEAD:" + cut["branch"])
        self.main()
        preview = json.loads(self.cli("prepare", "--action", "patch", "--branch", cut["branch"]).stdout)
        self.published(preview["tag"])
        self.main()
        stable = json.loads(self.cli("prepare", "--action", "promote", "--branch", cut["branch"]).stdout)
        self.published(stable["tag"], False)
        self.main()
        (self.repo / "next-feature").write_text("next")
        newer = self.commit("Next train feature")
        self.git("push", "origin", "main")
        self.pull(newer, 4, "Release Notes:\n- Next train feature")
        next_train = self.cut()
        self.assertEqual(next_train["tag"], "v0.3.0-pre")
        self.assertEqual(self.metadata()["base_sha"], stable["source"])
        self.main()
        output = self.root / "notes.md"
        self.cli("notes", "--tag", next_train["tag"], "--output", str(output))
        text = output.read_text()
        self.assertIn("Next train feature", text)
        self.assertNotIn("First train feature", text)
        self.assertNotIn("Fixed a shipped issue", text)
        self.assertIn(f"/compare/{stable['source']}...{next_train['tag']}", text)

    def test_normal_development_branches_are_not_release_refs(self):
        self.git("branch", "visual-polish")
        self.git("push", "origin", "visual-polish")
        self.git("tag", "vendor-snapshot")
        self.git("push", "origin", "vendor-snapshot")
        self.assertEqual(self.cut()["tag"], "v0.2.0-pre")

    def test_baseline_ignores_unreachable_stable(self):
        self.git("switch", "--orphan", "unrelated")
        (self.repo / "unrelated").write_text("unrelated")
        self.commit("Different root")
        self.git("tag", "v0.1.0")
        self.git("push", "origin", "unrelated", "v0.1.0")
        self.main()
        self.published("v0.1.0", False)
        self.cut()
        self.assertIsNone(self.metadata()["base_sha"])

    def test_preview_patch_preserves_baseline(self):
        cut = self.cut()
        self.main()
        result = json.loads(self.cli("prepare", "--action", "patch", "--branch", cut["branch"]).stdout)
        self.assertEqual(result["tag"], "v0.2.1-pre")
        self.assertEqual(self.metadata()["migration_sha"], self.marker)
        self.assertIsNone(self.metadata()["base_sha"])

    def test_exact_promotion_then_stable_patch(self):
        cut = self.cut()
        self.published(cut["tag"])
        # Newer preview on another train must not interfere.
        self.published("v9.9.9-pre")
        self.main()
        promoted = json.loads(self.cli("prepare", "--action", "promote", "--branch", cut["branch"]).stdout)
        self.assertEqual(promoted["tag"], "v0.2.0")
        self.assertEqual(promoted["channel"], "stable")
        self.assertEqual(self.git("diff", "--name-only", cut["source"], promoted["source"]), release.METADATA)
        self.assertEqual(self.metadata()["channel"], "stable")
        self.main()
        patched = json.loads(self.cli("prepare", "--action", "patch", "--branch", cut["branch"]).stdout)
        self.assertEqual(patched["tag"], "v0.2.1")
        self.assertEqual(patched["channel"], "stable")
        self.main()
        self.assertIn("Only a preview", self.cli("prepare", "--action", "promote", "--branch", cut["branch"], success=False).stderr)

    def test_promotion_rejects_unpublished_draft_or_changed_preview(self):
        cut = self.cut()
        self.main()
        self.assertIn("published preview", self.cli("prepare", "--action", "promote", "--branch", cut["branch"], success=False).stderr)
        self.data["releases"] = [dict(tag_name=cut["tag"], prerelease=True, draft=True, published_at=None)]
        self.save_fixture()
        self.main()
        self.assertIn("published preview", self.cli("prepare", "--action", "promote", "--branch", cut["branch"], success=False).stderr)
        self.published(cut["tag"])
        self.git("switch", "--detach", "origin/" + cut["branch"])
        (self.repo / "new-code").write_text("unreviewed after preview")
        self.commit("More code")
        self.git("push", "origin", "HEAD:" + cut["branch"])
        self.main()
        self.assertIn("exactly", self.cli("prepare", "--action", "promote", "--branch", cut["branch"], success=False).stderr)

    def test_promotion_rejects_tag_moved_from_published_source(self):
        cut = self.cut()
        self.published(cut["tag"])
        (self.repo / "untested").write_text("untested changes")
        moved = self.commit("Untested after preview")
        self.git("tag", "-f", cut["tag"], moved)
        self.git("push", "origin", "HEAD:" + cut["branch"])
        self.git("push", "--force", "origin", cut["tag"])  # Simulate unprotected tag, fixture only.
        self.main()
        self.assertIn("Published preview source differs", self.cli(
            "prepare", "--action", "promote", "--branch", cut["branch"], success=False).stderr)

    def test_promotion_rejects_new_unpublished_patch(self):
        cut = self.cut()
        self.published(cut["tag"])
        self.main()
        self.cli("prepare", "--action", "patch", "--branch", cut["branch"])
        self.main()
        self.assertIn("exactly", self.cli("prepare", "--action", "promote", "--branch", cut["branch"], success=False).stderr)

    def test_branch_and_worktree_safety(self):
        (self.repo / "dirty").write_text("untracked")
        self.assertIn("dirty", self.cli("prepare", "--action", "cut", success=False).stderr)
        (self.repo / "dirty").unlink()
        for branch in ("main", "v0.2.x;bad", "v0.2.x"):
            self.assertIn("branch", self.cli("prepare", "--action", "patch", "--branch", branch, success=False).stderr.lower())
        (self.repo / "ahead").write_text("ahead")
        self.commit("Ahead of origin main")
        self.assertIn("fresh origin/main", self.cli("prepare", "--action", "cut", success=False).stderr)

    def test_malformed_remote_refs_and_existing_stable_tag(self):
        self.git("tag", "v0.1.0-rc1")
        self.git("push", "origin", "v0.1.0-rc1")
        self.assertIn("Invalid release tag", self.cli("prepare", "--action", "cut", success=False).stderr)
        self.git("tag", "-d", "v0.1.0-rc1")
        self.git("push", "origin", ":refs/tags/v0.1.0-rc1")
        self.git("branch", "v0.2.bad")
        self.git("push", "origin", "v0.2.bad")
        self.assertIn("Invalid release branch", self.cli("prepare", "--action", "cut", success=False).stderr)
        self.git("push", "origin", ":refs/heads/v0.2.bad")
        cut = self.cut()
        self.published(cut["tag"])
        self.git("tag", "v0.2.0")
        self.git("push", "origin", "v0.2.0")
        self.main()
        self.assertIn("already exists", self.cli("prepare", "--action", "promote", "--branch", cut["branch"], success=False).stderr)

    def test_patch_rejects_preview_after_stable_tag_and_wrong_train_version(self):
        cut = self.cut()
        self.git("tag", "v0.2.0")
        self.git("push", "origin", "v0.2.0")
        self.main()
        self.assertIn("already stable train", self.cli("prepare", "--action", "patch", "--branch", cut["branch"], success=False).stderr)
        for filename in release.VERSION_FILES:
            path = self.repo / filename
            path.write_text(path.read_text().replace("0.2.0", "0.3.0"))
        self.commit("Incorrect version on train")
        self.git("push", "origin", "HEAD:" + cut["branch"])
        self.main()
        self.assertIn("major/minor", self.cli("prepare", "--action", "patch", "--branch", cut["branch"], success=False).stderr)

    def test_patch_rejects_invalid_metadata(self):
        cut = self.cut()
        path = self.repo / release.METADATA
        data = self.metadata()
        data["base_sha"] = "not-a-sha"
        path.write_text(json.dumps(data))
        self.commit("Invalid baseline")
        self.git("push", "origin", "HEAD:" + cut["branch"])
        self.main()
        self.assertIn("Invalid train base_sha", self.cli("prepare", "--action", "patch", "--branch", cut["branch"], success=False).stderr)

    def test_metadata_rejects_reachable_wrong_marker_and_changed_baseline(self):
        cut = self.cut()
        (self.repo / "new-change").write_text("change")
        newer = self.commit("New PR after migration")
        metadata = self.metadata()
        metadata["migration_sha"] = newer
        (self.repo / release.METADATA).write_text(json.dumps(metadata))
        self.commit("Move migration boundary")
        self.git("push", "origin", "HEAD:" + cut["branch"])
        self.main()
        self.assertIn("migration marker differs", self.cli(
            "prepare", "--action", "patch", "--branch", cut["branch"], success=False).stderr)
        metadata["migration_sha"] = self.marker
        metadata["base_sha"] = self.initial
        (self.repo / release.METADATA).write_text(json.dumps(metadata))
        self.commit("Change baseline")
        self.git("push", "origin", "HEAD:" + cut["branch"])
        self.main()
        self.assertIn("metadata must remain unchanged", self.cli(
            "prepare", "--action", "patch", "--branch", cut["branch"], success=False).stderr)

    def test_atomic_push_failure_leaves_remote_main_and_no_train(self):
        hook = self.remote / "hooks/pre-receive"
        hook.write_text('#!/bin/sh\nwhile read old new ref; do\ncase "$ref" in refs/tags/*) exit 1;; esac\ndone\n')
        hook.chmod(0o755)
        self.assertIn("Command failed: git push --atomic", self.cli("prepare", "--action", "cut", success=False).stderr)
        self.assertEqual(self.git("ls-remote", "origin", "refs/heads/main").split()[0], self.marker)
        self.assertEqual(self.git("ls-remote", "origin", "refs/heads/v0.2.x", "refs/tags/v0.2.0-pre"), "")

    def test_resolve_repeatable_from_trusted_main_and_branch_validation(self):
        cut = self.cut()
        self.main()
        self.assertFalse((self.repo / release.METADATA).exists())
        self.assertEqual(self.cli("version").stdout.strip(), "0.1.0")
        self.assertEqual(json.loads(self.cli("resolve", "--tag", cut["tag"]).stdout), cut)
        self.assertEqual(json.loads(self.cli("resolve", "--tag", cut["tag"]).stdout), cut)
        self.assertEqual(self.git("branch", "--show-current"), "main")
        self.assertEqual(self.git("rev-parse", "HEAD"), self.marker)
        self.git("push", "origin", ":refs/heads/" + cut["branch"])
        self.assertIn(cut["branch"], self.cli("resolve", "--tag", cut["tag"], success=False).stderr)

    def test_resolve_rejects_deleted_remote_or_local_only_tag(self):
        cut = self.cut()
        self.git("push", "origin", ":refs/tags/" + cut["tag"])
        self.main()
        self.assertIn("couldn't find remote ref", self.cli(
            "resolve", "--tag", cut["tag"], success=False).stderr)
        self.git("switch", "--detach", "origin/" + cut["branch"])
        for filename in release.VERSION_FILES:
            path = self.repo / filename
            path.write_text(path.read_text().replace("0.2.0", "0.2.1"))
        self.commit("Local unpublished patch")
        self.git("push", "origin", "HEAD:" + cut["branch"])
        self.git("tag", "v0.2.1-pre")
        self.main()
        self.assertIn("couldn't find remote ref", self.cli(
            "resolve", "--tag", "v0.2.1-pre", success=False).stderr)

    def test_resolve_rejects_version_and_channel_mismatch(self):
        cut = self.cut()
        self.git("tag", "v0.2.1-pre")
        self.git("tag", "v0.2.0")
        self.git("push", "origin", "v0.2.1-pre", "v0.2.0")
        self.main()
        self.assertIn("Tag and package version", self.cli("resolve", "--tag", "v0.2.1-pre", success=False).stderr)
        self.assertIn("Tag and train channel", self.cli("resolve", "--tag", "v0.2.0", success=False).stderr)

    def test_publication_reads_tag_data_without_executing_tag_script(self):
        cut = self.cut()
        sentinel = self.root / "untrusted-script-executed"
        (self.repo / "scripts/release.py").write_text(
            f"from pathlib import Path\nPath({str(sentinel)!r}).write_text('executed')\n")
        for filename in release.VERSION_FILES:
            path = self.repo / filename
            path.write_text(path.read_text().replace("0.2.0", "0.2.1"))
        source = self.commit("Untrusted release snapshot")
        tag = "v0.2.1-pre"
        self.git("tag", tag)
        self.git("push", "origin", "HEAD:" + cut["branch"], tag)
        self.main()
        data = json.loads(self.call(sys.executable, "scripts/release.py", "resolve", "--tag", tag).stdout)
        self.assertEqual(data["source"], source)
        self.assertEqual(data["version"], "0.2.1")
        output = self.root / "notes.md"
        self.call(sys.executable, "scripts/release.py", "notes", "--tag", tag, "--output", str(output))
        self.assertTrue(output.exists())
        self.assertFalse(sentinel.exists())
        self.assertEqual(self.git("branch", "--show-current"), "main")
        self.assertEqual(self.git("rev-parse", "HEAD"), self.marker)

    def test_notes_historical_policy_dedup_na_links_and_backports(self):
        (self.repo / "feature").write_text("feature")
        feature = self.commit("Merge feature PR")
        self.git("push", "origin", "main")
        self.pull(self.initial, 1, "Legacy PR without notes")
        self.pull(self.marker, 2, "Legacy adoption PR without notes")
        self.pull(feature, 3, "## Release Notes:\n- Faster launch\n- Improved search")
        cut = self.cut()
        self.pull(cut["source"], 3, "## Release Notes:\n- Faster launch\n- Improved search", merge=feature)
        self.pull(cut["source"], 4, "## Release Notes:\n- N/A")
        # Original merge of a cherry-picked PR lives on another branch.
        self.main()
        self.git("switch", "-c", "other")
        (self.repo / "backport-source").write_text("backport source")
        original = self.commit("Original PR on another branch")
        self.git("push", "origin", "other")
        self.main()
        self.pull(feature, 5, "## Release Notes:\n- Backported accessibility fix", merge=original)
        output = self.root / "notes.md"
        result = self.cli("notes", "--tag", cut["tag"], "--output", str(output))
        text = output.read_text()
        self.assertIn("Skipping historical PR #1", result.stderr)
        self.assertIn("Skipping historical PR #2", result.stderr)
        self.assertEqual(text.count("Faster launch"), 1)
        self.assertIn("Backported accessibility fix", text)
        self.assertIn("[#3](https://github.com/example/wiesel/pull/3)", text)
        self.assertNotIn("N/A", text)
        self.assertIn(f"/compare/{self.initial}...{cut['tag']}", text)

    def test_notes_fails_closed_for_new_missing_or_invalid_notes(self):
        (self.repo / "new-feature").write_text("new")
        commit = self.commit("New PR")
        self.git("push", "origin", "main")
        self.pull(commit, 9, "No release notes")
        cut = self.cut()
        self.main()
        output = self.root / "notes.md"
        result = self.cli("notes", "--tag", cut["tag"], "--output", str(output), success=False)
        self.assertIn("PR #9", result.stderr)
        self.assertFalse(output.exists())
        self.data["pulls"][commit][0]["body"] = "## Release Notes:\n- TODO"
        self.save_fixture()
        self.assertIn("placeholder", self.cli("notes", "--tag", cut["tag"], "--output", str(output), success=False).stderr)

    def test_patch_notes_remain_cumulative_and_include_branch_pr(self):
        self.git("tag", "v0.1.0", self.marker)
        self.git("push", "origin", "v0.1.0")
        self.published("v0.1.0", False)
        (self.repo / "minor-feature").write_text("minor")
        feature = self.commit("Minor feature")
        self.git("push", "origin", "main")
        self.pull(feature, 1, "## Release Notes:\n- Minor feature")
        cut = self.cut()
        (self.repo / "branch-fix").write_text("fix")
        fix = self.commit("Backport PR onto release branch")
        self.git("push", "origin", "HEAD:" + cut["branch"])
        self.pull(fix, 2, "## Release Notes:\n- Release branch fix")
        self.main()
        patched = json.loads(self.cli("prepare", "--action", "patch", "--branch", cut["branch"]).stdout)
        self.main()
        output = self.root / "notes.md"
        self.cli("notes", "--tag", patched["tag"], "--output", str(output))
        text = output.read_text()
        self.assertIn("Minor feature", text)
        self.assertIn("Release branch fix", text)
        self.assertIn(f"/compare/{self.marker}...{patched['tag']}", text)

    def test_notes_baseline_excludes_old_prs_and_fallback(self):
        self.git("tag", "v0.1.0", self.marker)
        self.git("push", "origin", "v0.1.0")
        self.published("v0.1.0", False)
        self.pull(self.initial, 1, "No notes")
        cut = self.cut()
        self.main()
        output = self.root / "notes.md"
        self.cli("notes", "--tag", cut["tag"], "--output", str(output))
        text = output.read_text()
        self.assertIn("No user-facing changes.", text)
        self.assertIn(f"/compare/{self.marker}...{cut['tag']}", text)
        self.assertNotIn("#1", text)


if __name__ == "__main__":
    unittest.main()
