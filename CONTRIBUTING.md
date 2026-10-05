# Contributing and releases

## Development

Wiesel currently builds only on macOS. Install Rust through rustup and Python
3.11+ for script tests and release metadata validation. CI uses Python 3.13 and
the Rust toolchain in `rust-toolchain.toml`. Release maintainers also need the
[GitHub CLI](https://cli.github.com/). There is no Knope, Changesets, Node.js, or
npm dependency.

```sh
cargo fmt --all -- --check
cargo check --locked --all-targets
cargo test --locked --all-targets
cargo clippy --locked --all-targets -- -D warnings
python3 scripts/test-app-scripts.py
python3 scripts/test-release.py
python3 scripts/release.py version
```

`version` verifies that the package version in `Cargo.toml`, the Wiesel entry in
`Cargo.lock`, and `CFBundleShortVersionString` in `resources/Info.plist` agree.

## Pull requests and release notes

Open feature/fix PRs against `main`. Include testing results; for native/UI
changes, include manual macOS checks and screenshots where useful. **Do not bump
versions in feature PRs.** Commit prefixes do not determine release versions.

Every PR must contain a `Release Notes:` section with user-facing bullets:

```markdown
Release Notes:

- Added a keyboard shortcut for opening settings.
- Fixed a failed request clearing the current draft.
```

Describe what a user can see or feel. Mention changed settings or shortcuts.
For internal-only changes, use exactly:

```markdown
Release Notes:

- N/A
```

The **Release notes required** Action validates the PR description on opening,
updates, and edits. Missing/empty sections, placeholders, and mixing `N/A` with
real notes fail. Marking this check required in branch protection prevents
merging until it passes. Reviewers still verify the notes' accuracy. The check
reads the event JSON as data and executes the trusted base-branch validator,
not scripts supplied by a fork.

## Release model

There are **preview and stable channels, no nightly**:

```text
feature PRs → main → v0.2.x preview → v0.2.x stable
                └→ v0.3.x preview → v0.3.x stable
```

- `main`: continuous development and CI; no automatic publishing or packaging.
- `vMAJOR.MINOR.x`: a snapshot of `main`, maintained through backport PRs.
- Preview tags: `v0.2.0-pre`, `v0.2.1-pre`, etc.; GitHub prereleases.
- Stable tags: `v0.2.1`, etc.; the tested preview promoted without newer `main`
  changes. Promotion changes only release-channel metadata, not code/version.

The **Release train** Action is manually dispatched on `main`. It chooses and
updates versions, commits the version/channel metadata on the train branch,
and atomically pushes that branch and its tag. It never pushes a version bump
to `main`. Cutting a new train automatically selects the next minor version
above the current development version and existing trains/tags. Patch releases
increment the branch's patch version. Human maintainers choose **when** to cut,
patch, or promote; developers do not manage versions in everyday PRs.

The publisher is explicitly called after preparation: pushes made with
`GITHUB_TOKEN` do not trigger another Actions workflow. Ordinary PR merges and
manual tag pushes do not publish installers.

### Cut a preview

After desired PRs have merged to `main`:

```sh
gh workflow run release-train.yml --ref main -f action=cut
```

This creates the next `vMAJOR.MINOR.x` branch and its initial `vX.Y.0-pre` tag.
Watch the run with `gh run list --workflow release-train.yml` and
`gh run watch <run-id>`.

### Ship preview fixes

Prefer fixing on `main` first, then cherry-pick the fix onto a separate branch
and open a PR against the train. Retain a valid release-notes section on the
backport PR. For example:

```sh
git fetch origin
git switch -c backport/fix origin/v0.2.x
git cherry-pick <fix-commit-sha>
git push -u origin HEAD
gh pr create --base v0.2.x
```

Start the backport PR summary with `Cherry-pick of #<original-pr-number>` (or
`Backport of #<original-pr-number>`), followed by its required release notes.
This preserves the original PR identity so later trains do not announce a fix
that already shipped on stable a second time.

After merging the backport and passing CI:

```sh
gh workflow run release-train.yml --ref main -f action=patch -f branch=v0.2.x
```

A preview branch publishes the next preview patch, e.g. `v0.2.1-pre`.

### Promote a tested preview

Manually test the downloaded preview installers on both architectures. Verify
installation, launch, minimum-OS behavior, Accessibility permission, shortcuts,
and representative UI/request flows. Then:

```sh
gh workflow run release-train.yml --ref main -f action=promote -f branch=v0.2.x
```

Promotion requires the branch tip to be **exactly the newest published preview
for that train**. If additional changes merged, publish/test another preview
first. Draft or failed previews cannot be promoted. The stable tag has the same
numeric version as that preview. Once stable, this branch cannot be promoted
again; a new preview train comes from `main`.

### Stable hotfixes

Backport a fix through a PR against the stable train, then dispatch `patch` on
that branch. This increments its patch version and publishes a stable hotfix.
Unlike promotion, this path does not require a preview first: maintainers must
perform appropriate acceptance testing and review before approving publication.
Normal features should wait for the next preview train.

## Packaging, notes, and publication

The **Release** workflow resolves an immutable tag SHA and checks its version,
channel, metadata, and membership in its train branch. It re-runs CI on that
source, packages Intel and Apple Silicon apps, verifies architecture/DMGs and
SHA-256 checksums, and creates a GitHub **draft** with six assets (DMG, ZIP, and
checksum file for each architecture).

Notes are assembled from merged PRs associated with included commits, with PR
links and `N/A` entries omitted. Notes are cumulative from the prior stable
baseline captured when cutting the train, so stable promotion includes the
whole train rather than only its last preview patch. The baseline can diverge
from `main`: no merge-back is needed. Shared commits and already shipped
backport PR identities are excluded from the next train's notes. The first train can cover
all history when no prior stable baseline exists. `.release/train.json` records
the baseline, channel, and release-note migration boundary.

Historical PRs through the first-parent commit introducing this tooling may
have no release-notes section and are skipped with warnings. This includes the
tooling-adoption PR whether it is squash-merged or merged normally. New PRs
must have valid notes; invalid notes fail generation. Direct commits are not a substitute for documented PRs;
review the generated draft for omissions. Maintain PR descriptions through
publication, since notes are collected from GitHub at release time.

**Review/edit the draft notes and approve the `release` environment deployment.**
Configure required environment reviewers to make this a real approval gate;
without them, GitHub publishes immediately after the draft job. Draft-note
edits are preserved on retries. Tests/builds must pass before a draft is created.

All artifacts come from the resolved release SHA, not moving `main`. The live
tag is checked again before uploading and after approval. Existing published
assets/notes are not overwritten. Previews are never marked Latest, and
recovering an older stable release cannot mark it Latest over a newer version.
Train management and publication are serialized without canceling active runs.

`scripts/package-release.sh` handles macOS packaging. The app marketing version
is numeric even for previews; the release tag carries `-pre`. The bundle build
number is the release commit's reachable commit count. License notices are
included before signing.

Generated release notes live on GitHub Releases. `CHANGELOG.md` is an index,
not another source requiring manual versioned entries.

## One-time GitHub configuration

These changes only configure local files; they do not configure GitHub settings
or publish anything.

1. Merge this tooling into the default `main` branch before dispatching it.
   Enable Actions and allow the train/draft/publication jobs' requested
   **Contents: write** permission. No stored PAT, bot, or GitHub App is required
   by CI. Maintainers need permission to dispatch workflows; local `gh auth
   login` is sufficient with normal repository access.
2. Protect `main` and `v*.x` train branches: require PR review, resolved
   conversations, **CI required**, and **Release notes required**. Disable
   force pushes/deletion. The train Action must be allowed to create train
   branches and push its **metadata-only version/channel commits**. If rules
   require every commit to come through a PR, configure a narrowly scoped
   automation bypass for these train updates. Do not bypass PR policy for
   ordinary code changes. Some repository rules may require a dedicated GitHub
   App instead of `GITHUB_TOKEN`; the workflow fails rather than bypassing them.
3. Protect `v*` tags from updates/deletion, allowing the release automation to
   create them. Keep release tags immutable.
4. Create the **release** Actions environment, restrict deployment to `main`
   (the publishing workflow runs on main even though artifacts come from a
   train tag), and add required reviewers. Review the draft before approval.
   Environment protection availability depends on GitHub plan/repo visibility.
5. Remove old Knope/Changesets bot installations and required checks if they
   were configured. Deleting configuration files does not uninstall a GitHub
   App. Require the new release-note check after the validator exists on main.

Merge-queue CI is supported; the release-notes check relies on validation of
its constituent PRs. Train pushes from the default token do not independently
start CI, but the explicit publisher re-runs source checks before packaging.

## Recovery and distribution caveats

If preparation pushed a tag but builds/uploads/publication failed, **do not
bump again**. Dispatch the publisher using the existing tag:

```sh
gh workflow run release.yml --ref main -f tag=v0.2.1-pre
# Stable recovery:
gh workflow run release.yml --ref main -f tag=v0.2.1
```

Use the actual tag from the original train run's summary. You can also retry
failed publication jobs. Re-running train preparation means a new release
operation, not recovery. Never move an existing tag to fix a source bug.
GitHub may replace pending concurrency runs during bursts; explicitly dispatch
an existing unprocessed tag if needed.

Builds remain **ad-hoc signed, not notarized**. Follow README Gatekeeper
instructions only if you trust the download. Developer ID signing/notarization
is separate work requiring Apple credentials; no AI Gateway key is needed in
CI. `MACOSX_DEPLOYMENT_TARGET=12.0` matches the plist, but distribution and UI
acceptance testing are still manual.
