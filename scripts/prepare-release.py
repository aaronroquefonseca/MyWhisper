#!/usr/bin/env python3
"""Preview a PR-based changelog/version update, or explicitly open its release PR."""
import argparse
import importlib.util
import json
import os
from pathlib import Path
import re
import subprocess
import tempfile
import tomllib

ROOT = Path(__file__).resolve().parent.parent
spec = importlib.util.spec_from_file_location("release", ROOT / "packaging/release.py")
release = importlib.util.module_from_spec(spec)
spec.loader.exec_module(release)


def run(args, *, cwd=None, stdin=None, check=True):
    env = {key: value for key, value in os.environ.items() if not key.startswith("GIT_")}
    return subprocess.run(args, cwd=ROOT if cwd is None else cwd, env=env, input=stdin, text=True, capture_output=True,
                          timeout=120, check=check)


def version_key(value):
    release.version(value)
    numbers, _, suffix = value.partition("-")
    identifiers = tuple((0, int(part)) if part.isdigit() else (1, part)
                        for part in suffix.split(".")) if suffix else ()
    return tuple(map(int, numbers.split("."))) + (not bool(suffix), identifiers)


def remote_exists(reference):
    result = run(["git", "ls-remote", "--exit-code", "origin", reference], check=False)
    if result.returncode not in (0, 2):
        result.check_returncode()
    return result.returncode == 0


def replace_one(pattern, replacement, text, description):
    updated, count = re.subn(pattern, replacement, text, flags=re.MULTILINE)
    if count != 1:
        raise ValueError(f"expected one {description}, found {count}")
    return updated


def version_updates(root, candidate, notes):
    release.version(candidate)
    if not notes.strip() or len(notes.encode()) > 256 * 1024:
        raise ValueError("release notes must be nonempty and bounded")
    manifest = (root / "Cargo.toml").read_text()
    package = tomllib.loads(manifest)["package"]
    if package["name"] != "mywhisper" or version_key(candidate) <= version_key(package["version"]):
        raise ValueError("choose a newer MyWhisper package version")
    changelog = (root / "CHANGELOG.md").read_text()
    if any(value == candidate for value, _ in release.changelog_sections(changelog)):
        raise ValueError("this release already has a changelog section")
    section = re.search(r"(?ms)^\[package\][ \t]*\n(.*?)(?=^\[|\Z)", manifest)
    if section is None:
        raise ValueError("missing Cargo package section")
    body = replace_one(r'^(version[ \t]*=[ \t]*)"[^"\n]+"',
                       lambda m: m[1] + json.dumps(candidate), section[1], "Cargo package version")
    manifest = manifest[:section.start(1)] + body + manifest[section.end(1):]
    lock = (root / "Cargo.lock").read_text()
    lock = replace_one(r'(^name = "mywhisper"\nversion = ")[^"\n]+(")',
                       lambda m: m[1] + candidate + m[2], lock, "locked root package version")
    pkgbuild = replace_one(r"^pkgver=.*$", "pkgver=" + candidate.replace("-", ""),
                           (root / "packaging/arch/PKGBUILD").read_text(), "Arch package version")
    pkgbuild = replace_one(r"^pkgrel=.*$", "pkgrel=1", pkgbuild, "Arch package revision")
    # Generated GitHub Markdown and reviewed edits are retained verbatim. The
    # release parser recognizes only version headings, including dated/linked
    # forms; contributors need no special files or Markdown list syntax.
    heading = re.search(r"(?m)^# Changelog[ \t]*\n", changelog)
    if heading is None:
        raise ValueError("missing changelog document heading")
    changelog = changelog[:heading.end()] + f"\n## {candidate}\n\n{notes.strip()}\n\n" + changelog[heading.end():].lstrip("\n")
    return {"Cargo.toml": manifest, "Cargo.lock": lock,
            "packaging/arch/PKGBUILD": pkgbuild, "CHANGELOG.md": changelog}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("version", help="new semantic version, without the v prefix")
    parser.add_argument("--repo", help="owner/repo; defaults to gh's current repository")
    parser.add_argument("--previous-tag", help="last published stable release by default")
    parser.add_argument("--notes-file", type=Path, help="use previously reviewed Markdown instead of generated titles")
    action = parser.add_mutually_exclusive_group(required=True)
    action.add_argument("--output", type=Path, help="write a reviewable patch/notes; no push or PR")
    action.add_argument("--create-pr", action="store_true", help="commit, push a new release branch and open its PR")
    args = parser.parse_args()
    release.version(args.version)
    repo = args.repo or run(["gh", "repo", "view", "--json", "nameWithOwner", "--jq", ".nameWithOwner"]).stdout.strip()
    if not re.fullmatch(r"[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+", repo):
        raise ValueError("invalid repository name")
    source_sha = run(["git", "rev-parse", "HEAD"]).stdout.strip()
    branch = "release/v" + args.version + "-" + source_sha[:12]
    tag = "v" + args.version
    if remote_exists("refs/tags/" + tag):
        raise ValueError("release tag already exists; published versions are immutable")
    previous = args.previous_tag or json.loads(run([
        "gh", "api", f"repos/{repo}/releases/latest"]).stdout)["tag_name"]
    if not previous.startswith("v"):
        raise ValueError("previous release tag must start with v")
    release.version(previous[1:])
    previous_sha = run(["git", "rev-parse", "--verify", "refs/tags/" + previous + "^{commit}"]).stdout.strip()
    run(["git", "merge-base", "--is-ancestor", previous_sha, source_sha])
    if previous_sha == source_sha:
        raise ValueError("there are no commits after the previous release")
    if args.create_pr:
        if run(["git", "cat-file", "-e", source_sha + ":scripts/prepare-release.py"], check=False).returncode != 0:
            raise ValueError("merge the release tooling before creating its release PR; --output can preview the design")
        main_sha = run(["git", "ls-remote", "--exit-code", "origin", "refs/heads/main"]).stdout.split()[0]
        if source_sha != main_sha:
            raise ValueError("release PRs must be prepared from the current origin/main commit")
    if args.notes_file:
        notes = args.notes_file.read_text()
    else:
        request = {"tag_name": tag, "target_commitish": source_sha, "previous_tag_name": previous}
        if run(["git", "cat-file", "-e", source_sha + ":.github/release.yml"], check=False).returncode == 0:
            request["configuration_file_path"] = ".github/release.yml"
        generated = json.loads(run(["gh", "api", "--method", "POST", f"repos/{repo}/releases/generate-notes",
                                    "--input", "-"], stdin=json.dumps(request)).stdout)
        notes = generated["body"]
    with tempfile.TemporaryDirectory(prefix="mywhisper-release-preparation-") as directory:
        worktree = Path(directory) / "source"
        run(["git", "-c", "core.hooksPath=/dev/null", "worktree", "add", "--detach", str(worktree), source_sha])
        try:
            updates = version_updates(worktree, args.version, notes)
            updates[".github/release-source.json"] = json.dumps({
                "schema": 1, "version": args.version, "source_sha": source_sha,
                "previous_tag": previous, "previous_sha": previous_sha}, indent=2) + "\n"
            for name, content in updates.items():
                (worktree / name).parent.mkdir(parents=True, exist_ok=True)
                (worktree / name).write_text(content)
            release.preflight(worktree, tag)
            run(["git", "add", "--intent-to-add", "--", *updates], cwd=worktree)
            patch = run(["git", "diff", "--", *updates], cwd=worktree).stdout
            if args.output:
                args.output.mkdir(parents=True, exist_ok=False, mode=0o700)
                (args.output / "release.patch").write_text(patch)
                (args.output / "notes.md").write_text(notes)
                (args.output / "source.json").write_text(json.dumps(
                    {"version": args.version, "source_sha": source_sha, "previous_tag": previous,
                     "previous_sha": previous_sha}, indent=2) + "\n")
                print(f"Preview written to {args.output}; no commit, push, tag or release created.")
                return
            if remote_exists("refs/heads/" + branch):
                raise ValueError("release branch already exists; preserve its reviewed edits instead of overwriting it")
            run(["git", "-c", "core.hooksPath=/dev/null", "switch", "-c", branch], cwd=worktree)
            run(["git", "add", "--", *updates], cwd=worktree)
            run(["git", "-c", "core.hooksPath=/dev/null", "commit", "-m", "chore: prepare " + tag], cwd=worktree)
            run(["git", "push", "origin", "HEAD:refs/heads/" + branch], cwd=worktree)
            body = Path(directory) / "pr-body.md"
            body.write_text(f"Prepare {tag} from `{source_sha}` since `{previous}`.\n\n"
                "Review and expand the PR-linked changelog entries where a title omits important details. "
                "This PR updates the Cargo/lockfile and Arch version metadata; Nix derives its version from Cargo. "
                "It creates no release or tag and preserves the publication/license gates.\n\n"
                "Merge only after CI passes. Tag the tested release commit when ready to publish. "
                "Do not mix additional product changes into this PR.\n\n" + notes)
            labels = json.loads(run(["gh", "label", "list", "--repo", repo, "--limit", "1000", "--json", "name"]).stdout)
            if not any(label["name"] == "release" for label in labels):
                run(["gh", "label", "create", "release", "--repo", repo, "--color", "ededed",
                     "--description", "Version and release note preparation"])
            print(run(["gh", "pr", "create", "--repo", repo, "--base", "main", "--head", branch,
                       "--title", "Prepare release " + tag, "--label", "release", "--body-file", str(body)]).stdout.strip())
        finally:
            run(["git", "worktree", "remove", "--force", str(worktree)], check=False)


if __name__ == "__main__":
    try:
        main()
    except (ValueError, KeyError, OSError, subprocess.SubprocessError) as error:
        raise SystemExit(f"Release preparation failed: {error}") from error
