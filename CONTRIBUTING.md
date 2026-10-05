# Contributing and releases

## Development

Wiesel currently builds only on macOS. Install Rust through rustup, Python 3 for
the existing lifecycle tests, [Knope](https://knope.tech/installation/), and the
[GitHub CLI](https://cli.github.com/) if you maintain releases.
CI pins Knope to 0.23.0 and Rust through `rust-toolchain.toml`. There is no Node.js
or npm dependency.

```sh
cargo fmt --all -- --check
cargo check --locked --all-targets
cargo test --locked --all-targets
cargo clippy --locked --all-targets -- -D warnings
python3 scripts/test-app-scripts.py
knope get-version
knope validate --dry-run
```

Knope's validation previews a release without modifying files, pushing commits,
or publishing. It checks pending change files and agreement between Cargo's
manifest/lock version and the bundle's marketing version.

## Documenting changes

Use conventional commits for simple changes:

- `fix: ...` — patch release.
- `feat: ...` — feature release.
- `feat!: ...` or a `BREAKING CHANGE:` footer — breaking change.
- `ci: ...`, `docs: ...`, or `chore: ...` — no release by itself.

Knope applies semantic-version rules, including special treatment of pre-1.0
versions. Preserve conventional commit messages when merging, or configure
squash merges to retain conventional PR titles.

For detailed user-facing notes, run `knope document-change`, or commit a Markdown
file in `.changeset/`:

```markdown
---
default: patch
---

# Fix a user-visible issue

Explain the impact on users.
```

The single package is named `default` in Knope change files. Use `patch`,
`minor`, or `major`; this is Knope's native format, not npm Changesets. Do not put
non-change Markdown files in that directory. There is no bot enforcing PR
change documentation or creating change files for you.

## Preparing a release

This uses Knope's [basic CLI workflow](https://knope.tech/tutorials/releasing-basic-projects/),
split into preparation and publication so version changes can be reviewed under
normal branch protection. Neither ordinary merges nor tag pushes publish a
release automatically. This setup currently supports stable `X.Y.Z` versions,
not prerelease labels.

Start from current main, fetch release tags, and create a release branch:

```sh
git switch main
git pull --ff-only
git fetch origin --tags
git switch -c release/next
knope prepare-release --dry-run
knope prepare-release
```

`prepare-release` updates and stages `Cargo.toml`, the Wiesel entry in
`Cargo.lock`, the plist's marketing version, and `CHANGELOG.md`. It consumes the
pending change files. It does **not** commit, push, tag, or contact GitHub.
Review the staged diff, then commit and open an ordinary PR:

```sh
git diff --cached
git commit -m "chore: prepare release"
git push -u origin HEAD
gh pr create --base main --title "chore: prepare release" --body "Prepare the next Wiesel release."
```

Merge that PR once its required CI checks and review pass. A directly committed
version update is also possible if your repository policy permits it; the CLI
never bypasses main protection for you.

## Publishing a release

After merging the prepared version, update your clean local main:

```sh
git switch main
git pull --ff-only
knope release --dry-run
knope release
```

Unlike the tutorial's all-in-one default workflow, `release` deliberately does
not bump versions again. Its dry run previews tagging/dispatching the **current
prepared version**; `prepare-release --dry-run` previews the next version and
notes. Dry runs do not execute the Git/authentication guards.

The real release command:

1. Requires a clean local `main` matching `origin/main`, current-version release
   notes, and the prepared release's changelog update at the main tip. This
   prevents silently including later merges that aren't in the prepared notes.
2. Uses Knope's native Git-only `Release` step to create `vX.Y.Z` locally.
3. Pushes only that tag; no version commits are pushed to main.
4. Explicitly dispatches `.github/workflows/release.yml` on main through `gh`.

If main advances after the prepared release merges, the CLI refuses to include
those later changes silently. To intentionally release the earlier prepared
commit, tag that exact merge SHA yourself, push the tag, and dispatch it:

```sh
git tag v0.1.1 <prepared-merge-sha>
git push origin refs/tags/v0.1.1:refs/tags/v0.1.1
gh workflow run release.yml --ref main -f tag=v0.1.1
```

Use the prepared version and SHA, not the example values. Actions still verifies
main ancestry, version agreement, and release notes for that source.

The command enqueues Actions; it does not wait for builds or publish a GitHub
release itself. Watch progress with `gh run list --workflow release.yml`, then
`gh run watch <run-id>`.

Actions resolves the tag's exact commit, requires it to belong to main, verifies
its version and changelog, and runs the same Intel/Apple Silicon checks as PR CI.
It builds both native apps, creates a draft release with the prepared notes,
uploads both DMGs, both ZIPs, and both SHA-256 files, then publishes. Tests or
build failures occur before any GitHub release is created. Interrupted uploads
may leave a draft that can be recovered.

All builds use the resolved SHA, not moving main. A moved tag fails validation.
Published assets are not replaced on reruns; recovering an older draft does not
mark it Latest over a newer published stable version. Publication is serialized
and active publications are never canceled.

The one additional shell script, `scripts/package-release.sh`, handles native
macOS packaging. Knope handles versions/changelogs directly. The bundle build
number is the release commit's reachable Git commit count, separate from the
marketing version. License notices are included before signing.

## One-time configuration

This checkout currently uses local branch `master` and has no remote configured.
These files target `main`; this setup does not rename branches or push anything.

1. Configure the GitHub remote as `origin`, make `main` the default branch, and
   commit/push the CI and Knope configuration. The dispatch workflow must exist
   on the default branch before it can be run.
2. Install Knope (0.23.0 recommended) and GitHub CLI locally. Run `gh auth login`
   for the account that can push release tags and dispatch workflows. If you
   supply a fine-grained token instead, it needs access to this repository with
   **Actions: write** and the appropriate **Contents** permissions. Ensure your
   Git credentials can push tags too. No `[github]` owner/repo placeholders in
   `knope.toml` need filling: `gh` uses the repository's Git remote.
3. Enable GitHub Actions. Allow the publication job's requested
   `contents: write` permission; GitHub supplies its job token automatically.
   No stored CI PAT, bot, custom GitHub App, or additional release secret is
   required.
4. Configure the `release` Actions environment, restricted to main. Optional
   required reviewers add final approval before the draft is created/published.
5. Require the **CI required** check from CI, PR review, and resolved conversations.
   Disallow force pushes/deletion of main, and protect `v*` tags against updates
   or deletion while allowing release maintainers to create them.
6. If Knope Bot was already installed, remove/disable its installation for this
   repository and remove its **Require changes to be documented** required check.
   Removing bot configuration from Git does not uninstall a GitHub App.

There is no Dependabot configuration or replacement dependency-update bot.
Manual acceptance testing of the first downloaded release on both architectures
is still required.

## Recovery and distribution caveats

If tagging succeeds but dispatch fails, do not prepare/bump another version.
Authenticate/fix permissions and dispatch the existing tag explicitly:

```sh
gh workflow run release.yml --ref main -f tag=v0.1.1
```

Replace `v0.1.1` with the prepared tag. The same command (or GitHub's **Run
workflow** button on main) recovers failed builds/uploads and old drafts. Rerun
the original run if only a failed job needs retrying. Never move an existing
release tag to fix a source bug; prepare a new release instead. GitHub may
replace pending jobs during bursts, so an unprocessed tag/draft can be explicitly
redispatched.

Builds are **ad-hoc signed, not notarized**. Follow the README's Gatekeeper
instructions only if you trust the download. Developer ID signing/notarization
would require separate Apple credentials; no AI Gateway key is needed in CI.
`MACOSX_DEPLOYMENT_TARGET=12.0` matches the plist, but minimum-OS compatibility,
installation, Accessibility, Gatekeeper, and UI behavior still need manual
acceptance testing.
