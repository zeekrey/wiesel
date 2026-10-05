#!/usr/bin/env python3
"""Release trains, using only git and read-only gh API calls.

Migration boundary: the first-parent commit introducing this script is recorded in
train.json as migration_sha. PRs merged at/before it may lack Release Notes;
newer PRs must pass the same parser as check-pr. Direct commits are warned/skipped.
Publication retries use resolve/notes on the existing tag, never prepare again.
Resolve/notes run trusted main's script and inspect tag files with git show;
they never check out or execute the release snapshot.
"""
import argparse
import json
import os
from pathlib import Path
import plistlib
import re
import subprocess
import sys
import tomllib

VERSION_RE = r"(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)"
TAG_RE = re.compile(r"v" + VERSION_RE + r"(-pre)?")
BRANCH_RE = re.compile(r"v(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.x")
SHA_RE = re.compile(r"[0-9a-f]{40}")
VERSION_FILES = ("Cargo.toml", "Cargo.lock", "resources/Info.plist")
METADATA = ".release/train.json"


class ReleaseError(Exception):
    pass


def require(condition, message):
    if not condition:
        raise ReleaseError(message)


def run(*args):
    try:
        result = subprocess.run(args, text=True, capture_output=True, check=False)
    except OSError as exc:
        raise ReleaseError(f"Cannot run {args[0]}: {exc}") from exc
    require(result.returncode == 0,
            f"Command failed: {' '.join(args)}\n{result.stderr.strip() or result.stdout.strip()}")
    return result.stdout.strip()


def git(*args):
    return run("git", *args)


def ancestor(older, newer):
    result = subprocess.run(["git", "merge-base", "--is-ancestor", older, newer],
                            text=True, capture_output=True)
    require(result.returncode in (0, 1), f"Cannot check ancestry: {result.stderr.strip()}")
    return result.returncode == 0


def version_tuple(value):
    require(isinstance(value, str) and re.fullmatch(VERSION_RE, value),
            f"Invalid numeric version: {value!r}; expected X.Y.Z without leading zeros")
    return tuple(map(int, value.split(".")))


def tag_parts(tag):
    match = TAG_RE.fullmatch(tag)
    require(match, f"Invalid release tag: {tag!r}; expected vX.Y.Z[-pre]")
    return tuple(map(int, match.group(1, 2, 3))), "preview" if match.group(4) else "stable"


def branch_parts(branch):
    match = BRANCH_RE.fullmatch(branch or "")
    require(match, f"Invalid release branch: {branch!r}; expected vMAJOR.MINOR.x")
    return tuple(map(int, match.group(1, 2)))


def file_bytes(path, source=None):
    if source is None:
        return Path(path).read_bytes()
    result = subprocess.run(["git", "show", f"{source}:{path}"], capture_output=True)
    require(result.returncode == 0,
            f"Cannot read {path} at {source}: {result.stderr.decode(errors='replace').strip()}")
    return result.stdout


def version(source=None):
    package = tomllib.loads(file_bytes("Cargo.toml", source).decode())["package"]
    require(package.get("name") == "wiesel", "Cargo.toml package must be wiesel")
    lock = tomllib.loads(file_bytes("Cargo.lock", source).decode())
    entries = [p for p in lock.get("package", []) if p.get("name") == "wiesel"]
    require(len(entries) == 1, "Cargo.lock must contain exactly one wiesel package")
    plist = plistlib.loads(file_bytes("resources/Info.plist", source))
    values = [package["version"], entries[0]["version"], plist["CFBundleShortVersionString"]]
    for value in values:
        version_tuple(value)
    require(len(set(values)) == 1, f"Version mismatch (Cargo.toml, Cargo.lock, Info.plist): {values}")
    return values[0]


def set_version(value):
    version()  # Do not repair mismatched input silently.
    version_tuple(value)
    for filename in ("Cargo.toml", "Cargo.lock"):
        text = Path(filename).read_text()
        # Limit replacement to the package table, preserving all other formatting.
        header = "[package]" if filename == "Cargo.toml" else "[[package]]"
        blocks = re.split(r"(?m)(?=^\[)", text)
        count = 0
        for i, block in enumerate(blocks):
            if block.startswith(header + "\n") and re.search(r'^name\s*=\s*"wiesel"\s*$', block, re.M):
                blocks[i], replaced = re.subn(r'(?m)^(version\s*=\s*)"[^"]+"',
                                              lambda m: m[1] + f'"{value}"', block)
                count += replaced
        require(count == 1, f"Cannot uniquely update wiesel version in {filename}")
        Path(filename).write_text("".join(blocks))
    path = Path("resources/Info.plist")
    text, count = re.subn(r'(<key>CFBundleShortVersionString</key>\s*<string>)[^<]+(</string>)',
                         lambda m: m[1] + value + m[2], path.read_text())
    require(count == 1, "Cannot uniquely update Info.plist marketing version")
    path.write_text(text)
    require(version() == value, "Failed to update release version")


def parse_notes(body):
    require(isinstance(body, str), "PR body must contain a Release Notes: section")
    body = re.sub(r"<!--.*?(?:-->|$)", "", body, flags=re.S)
    lines = body.splitlines()
    starts = [i for i, line in enumerate(lines)
              if re.fullmatch(r"\s*(?:#{1,6}\s+)?Release Notes:?\s*", line, re.I)]
    require(len(starts) == 1, "PR body must contain exactly one Release Notes: section")
    bullets = []
    for line in lines[starts[0] + 1:]:
        if re.match(r"^\s*#{1,6}\s+", line):
            break
        if not line.strip():
            continue
        match = re.fullmatch(r"\s*-\s+(.+?)\s*", line)
        require(match, "Release Notes: must contain nonempty '-' bullets only")
        note = match[1]
        require(note.casefold() not in ("...", "…", "added/fixed/improved ...") and not re.search(r"(?i)(\bTODO\b|\bTBD\b|\bPLACEHOLDER\b|describe (?:the |your )?(?:change|user)|add (?:a |the |your |short |release |user-facing )*(?:note|summary|change)|write (?:release )?notes|replace (?:this|with)|(?:note|summary|change) here|<[^>]+>)", note),
                f"Replace the Release Notes template placeholder: {note}")
        bullets.append(note)
    require(bullets, "Release Notes: needs at least one '-' bullet or sole '- N/A'")
    if any(note.upper() == "N/A" for note in bullets):
        require(len(bullets) == 1, "Release Notes: '- N/A' must be the sole bullet")
        return []
    return bullets


def check_pr(path):
    event = json.loads(Path(path).read_text())
    require(isinstance(event.get("pull_request"), dict), "Event file does not contain pull_request")
    parse_notes(event["pull_request"].get("body"))


def clean():
    require(not git("status", "--porcelain"), "Refusing dirty working tree/index; commit or stash changes first")


def fetch():
    require(git("rev-parse", "--is-shallow-repository") == "false",
            "Full history is required; use checkout fetch-depth: 0")
    git("fetch", "--prune", "origin", "+refs/heads/*:refs/remotes/origin/*", "--tags")


def refs():
    tags = git("tag", "--list").splitlines()
    branches = git("for-each-ref", "--format=%(refname:strip=3)", "refs/remotes/origin").splitlines()
    for tag in tags:
        if re.match(r"^v[0-9]", tag):
            tag_parts(tag)
    for branch in branches:
        if re.match(r"^v[0-9]", branch):
            branch_parts(branch)
    return tags, branches


def sha(ref):
    return git("rev-parse", "--verify", ref + "^{commit}")


def gh_json(endpoint):
    data = json.loads(run("gh", "api", "--paginate", "--slurp", endpoint))
    require(isinstance(data, list) and all(isinstance(page, list) for page in data),
            f"Unexpected gh API response for {endpoint}")
    items = [item for page in data for item in page]
    require(all(isinstance(item, dict) for item in items), f"Unexpected gh API items for {endpoint}")
    return items


def published_releases():
    return [r for r in gh_json("repos/{owner}/{repo}/releases")
            if not r.get("draft") and r.get("published_at")]


def shares_history(left, right):
    result = subprocess.run(["git", "merge-base", left, right], text=True, capture_output=True)
    require(result.returncode in (0, 1), f"Cannot find common history: {result.stderr.strip()}")
    return result.returncode == 0


def baseline(source, releases, tags):
    candidates = []
    for release in releases:
        tag = release.get("tag_name", "")
        if not TAG_RE.fullmatch(tag) or release.get("prerelease") or tag.endswith("-pre"):
            continue
        require(tag in tags, f"Published stable tag {tag} is missing from origin history")
        commit = sha("refs/tags/" + tag)
        # Stable tags normally diverge from main by version/channel commits.
        # A..B excludes their shared history without requiring merge-back.
        if shares_history(commit, source):
            candidates.append((tag_parts(tag)[0], commit))
    return max(candidates)[1] if candidates else None


def migration_marker(source):
    # For an ordinary merge, the policy begins when main receives the tooling,
    # not at an earlier commit inside the tooling PR's development branch.
    commits = git("log", "--first-parent", "--reverse", "--diff-filter=A", "--format=%H", source,
                  "--", "scripts/release.py").splitlines()
    require(commits, "Cannot find release-notes migration marker (commit adding scripts/release.py)")
    return commits[0]


def read_metadata(source):
    data = json.loads(file_bytes(METADATA, source))
    require(isinstance(data, dict) and set(data) == {"channel", "base_sha", "migration_sha"},
            "Invalid .release/train.json fields")
    require(data["channel"] in ("preview", "stable"), "Invalid train channel")
    for key in ("base_sha", "migration_sha"):
        value = data[key]
        if key == "base_sha" and value is None:
            continue
        require(isinstance(value, str) and SHA_RE.fullmatch(value), f"Invalid train {key}")
        commit = sha(value)
        if key == "base_sha":
            require(shares_history(commit, source), "Train baseline has unrelated history")
        else:
            require(ancestor(commit, source), f"Train {key} is not reachable from release source")
    require(data["migration_sha"] == migration_marker(source),
            "Train migration marker differs from the first-parent commit introducing scripts/release.py")
    return data


def write_metadata(data):
    path = Path(METADATA)
    path.parent.mkdir(exist_ok=True)
    path.write_text(json.dumps(data, indent=2) + "\n")


def emit(tag, branch, channel, source, value):
    data = dict(source=source, tag=tag, branch=branch, channel=channel, version=value)
    output = os.environ.get("GITHUB_OUTPUT")
    if output:
        with open(output, "a") as handle:
            for key, value in data.items():
                handle.write(f"{key}={value}\n")
    print(json.dumps(data, sort_keys=True))
    return data


def prepare(action, branch=None):
    branch = branch or None  # workflow_dispatch supplies an empty optional branch.
    clean()
    fetch()
    head = sha("HEAD")
    require(head == sha("refs/remotes/origin/main"), "prepare must start at fresh origin/main, never reset main")
    require(git("branch", "--show-current") in ("", "main"), "prepare must start on main or detached origin/main")
    tags, branches = refs()
    require(action != "cut" or branch is None, "cut chooses the next train; do not supply --branch")
    if action == "cut":
        current = version_tuple(version())
        minors = [current[1]]
        minors += [branch_parts(b)[1] for b in branches if BRANCH_RE.fullmatch(b) and branch_parts(b)[0] == current[0]]
        minors += [tag_parts(t)[0][1] for t in tags if TAG_RE.fullmatch(t) and tag_parts(t)[0][0] == current[0]]
        value = f"{current[0]}.{max(minors) + 1}.0"
        branch = f"v{current[0]}.{max(minors) + 1}.x"
        metadata = dict(channel="preview", base_sha=baseline(head, published_releases(), tags),
                        migration_sha=migration_marker(head))
        git("switch", "--create", branch, "refs/remotes/origin/main")
    else:
        train = branch_parts(branch)
        require(branch in branches, f"Release branch {branch} does not exist on origin")
        git("switch", "--detach", "refs/remotes/origin/" + branch)
        head = sha("HEAD")
        current = version_tuple(version())
        require(current[:2] == train, "Release branch and package major/minor do not agree")
        metadata = read_metadata(head)
        channel = metadata["channel"]
        previous = f"v{version()}" + ("-pre" if channel == "preview" else "")
        require(previous in tags and ancestor(sha("refs/tags/" + previous), head),
                f"Branch must contain its current {channel} release tag {previous}")
        require(metadata == read_metadata(sha("refs/tags/" + previous)),
                "Train metadata must remain unchanged since its previous release")
        train_tags = [tag_parts(t)[0] for t in tags if TAG_RE.fullmatch(t) and tag_parts(t)[0][:2] == train]
        require(max(train_tags) == current, "Branch version is behind the latest tag for this train")
        if action == "patch" and channel == "preview":
            require(not any(tag_parts(t)[0][:2] == train and tag_parts(t)[1] == "stable"
                            for t in tags if TAG_RE.fullmatch(t)),
                    "Cannot create a preview patch for an already stable train")
        if action == "promote":
            require(channel == "preview", "Only a preview train can be promoted")
            previews = [r for r in published_releases()
                        if TAG_RE.fullmatch(r.get("tag_name", ""))
                        and tag_parts(r["tag_name"])[0][:2] == train
                        and tag_parts(r["tag_name"])[1] == "preview" and r.get("prerelease")]
            require(previews, "Promotion requires a published preview for this train")
            published = max(previews, key=lambda r: tag_parts(r["tag_name"])[0])
            latest = published["tag_name"]
            require(latest == previous and sha("refs/tags/" + latest) == head,
                    f"Promotion requires HEAD to equal newest published preview {latest} exactly; publish/patch first")
            require(published.get("target_commitish") == head,
                    "Published preview source differs from its current tag/branch; refusing promotion")
            metadata["channel"] = "stable"
            value = version()
        else:
            value = f"{current[0]}.{current[1]}.{current[2] + 1}"
    channel = metadata["channel"]
    tag = "v" + value + ("-pre" if channel == "preview" else "")
    require(tag not in tags, f"Tag {tag} already exists; rerun publication with resolve --tag {tag}, not prepare")
    set_version(value)
    write_metadata(metadata)
    allowed = (*VERSION_FILES, METADATA)
    git("add", "--", *allowed)
    staged = git("diff", "--cached", "--name-only").splitlines()
    require(staged and set(staged) <= set(allowed), "Release commit must change release metadata only")
    git("commit", "-m", f"chore(release): {tag}")
    source = sha("HEAD")
    git("tag", tag, source)
    # No force flags: a racing writer or remote divergence must reject the entire push.
    git("push", "--atomic", "origin", f"HEAD:refs/heads/{branch}", f"refs/tags/{tag}:refs/tags/{tag}")
    return emit(tag, branch, channel, source, value)


def validate_tag(tag):
    numbers, channel = tag_parts(tag)
    # --tags alone does not prune stale local tags or attest remote existence.
    git("fetch", "origin", "refs/tags/" + tag)
    source = sha("refs/tags/" + tag)
    require(source == sha("FETCH_HEAD"), "Local release tag differs from the origin tag")
    value = version(source)
    require(version_tuple(value) == numbers, "Tag and package version do not agree")
    metadata = read_metadata(source)
    require(metadata["channel"] == channel, "Tag and train channel do not agree")
    branch = f"v{numbers[0]}.{numbers[1]}.x"
    require(ancestor(source, sha("refs/remotes/origin/" + branch)),
            f"Tag {tag} is not in release branch {branch}")
    return source, branch, channel, value, metadata


def resolve(tag):
    clean()
    fetch()
    refs()
    source, branch, channel, value, _ = validate_tag(tag)
    return emit(tag, branch, channel, source, value)


def warn(message):
    print(f"warning: {message}", file=sys.stderr)


def pr_identities(pr):
    number = pr.get("number")
    require(isinstance(number, int) and number > 0, "Merged PR has invalid number")
    identities = {number}
    # Backport PRs can identify the original PR even when GitHub associates a
    # cherry-picked commit only with the backport. Keep this line in its summary.
    body = pr.get("body") or ""
    require(isinstance(body, str), "Merged PR has invalid body")
    identities.update(int(n) for n in re.findall(
        r"(?im)^\s*(?:cherry-pick|backport) of #(\d+)\b", body))
    return identities


def notes(tag, output):
    # Keep trusted main checked out; inspect only data from the tagged snapshot.
    fetch()
    refs()
    source, _, _, _, metadata = validate_tag(tag)
    base = metadata["base_sha"]
    revision = f"{base}..{source}" if base else source
    commits = git("rev-list", "--reverse", revision).splitlines()
    seen = set()
    if base:
        # Inspect only the prior stable branch's divergent commits. These are
        # typically backports and metadata; shared main history is already
        # excluded by base..source. Avoid announcing a shipped backport again
        # when its original PR appears on main in the next train.
        for commit in git("rev-list", f"{source}..{base}").splitlines():
            for pr in gh_json(f"repos/{{owner}}/{{repo}}/commits/{commit}/pulls"):
                if pr.get("merged_at"):
                    seen.update(pr_identities(pr))
    entries = []
    for commit in commits:
        pulls = gh_json(f"repos/{{owner}}/{{repo}}/commits/{commit}/pulls")
        included = False
        for pr in pulls:
            if not pr.get("merged_at"):
                continue
            merged = pr.get("merge_commit_sha")
            require(isinstance(merged, str) and SHA_RE.fullmatch(merged), "Merged PR has invalid merge_commit_sha")
            # Association with an included commit also includes cherry-picked/backported
            # PRs whose original merge is not ancestral to this release branch.
            included = True
            identities = pr_identities(pr)
            number = pr["number"]
            if identities & seen:
                seen.update(identities)
                continue
            seen.update(identities)
            url = pr.get("html_url")
            require(isinstance(url, str) and re.fullmatch(r"https://[^\s<>]+/pull/[0-9]+", url), "Merged PR has invalid URL")
            try:
                bullets = parse_notes(pr.get("body"))
            except ReleaseError as exc:
                if ancestor(merged, metadata["migration_sha"]):
                    warn(f"Skipping historical PR #{number}: {exc}")
                    continue
                raise ReleaseError(f"PR #{number} ({url}): {exc}") from exc
            entries.extend(f"- {bullet} ([#{number}]({url}))" for bullet in bullets)
        if not included:
            warn(f"Skipping direct/unassociated commit {commit}")
    repo_url = run("gh", "repo", "view", "--json", "url", "--jq", ".url")
    require(re.fullmatch(r"https://[^\s<>]+/[^/]+/[^/]+", repo_url), "Invalid repository URL from gh")
    # With no prior release, notes include all history; the comparison starts at
    # the repository's first root commit (GitHub has no empty-tree compare ref).
    compare = f"{repo_url}/compare/{base or commits[0]}...{tag}"
    text = "\n".join(entries) if entries else "No user-facing changes."
    Path(output).write_text(f"## Release Notes\n\n{text}\n\n[Full changes]({compare})\n")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    commands = parser.add_subparsers(dest="command", required=True)
    commands.add_parser("version")
    check = commands.add_parser("check-pr")
    check.add_argument("--event-file", required=True)
    prep = commands.add_parser("prepare")
    prep.add_argument("--action", choices=("cut", "patch", "promote"), required=True)
    prep.add_argument("--branch")
    resolution = commands.add_parser("resolve")
    resolution.add_argument("--tag", required=True)
    note = commands.add_parser("notes")
    note.add_argument("--tag", required=True)
    note.add_argument("--output", required=True)
    args = parser.parse_args()
    try:
        if args.command == "check-pr":
            check_pr(args.event_file)
        else:
            os.chdir(git("rev-parse", "--show-toplevel"))
            if args.command == "version":
                print(version())
            elif args.command == "prepare":
                prepare(args.action, args.branch)
            elif args.command == "resolve":
                resolve(args.tag)
            else:
                notes(args.tag, args.output)
    except (ReleaseError, OSError, ValueError, KeyError, TypeError) as exc:
        print(f"release: {exc}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
