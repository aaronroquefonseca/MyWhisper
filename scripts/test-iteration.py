#!/usr/bin/env python3
"""Test release preparation and desktop-session failures in private fixtures."""
import importlib.util
import json
import os
from pathlib import Path
import signal
import subprocess
import sys
import tempfile
import time
import tomllib
import unittest
from unittest.mock import patch

HERE = Path(__file__).resolve().parent


def load(name, file):
    spec = importlib.util.spec_from_file_location(name, file)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


prepare = load("prepare", HERE / "prepare-release.py")


class Fixture(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory(prefix="mywhisper-iteration-test-")
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)
        self.home = self.root / "home"
        self.home.mkdir(mode=0o700)
        self.runtime = self.root / "runtime"
        self.runtime.mkdir(mode=0o700)
        self.env = {k: v for k, v in os.environ.items() if not k.startswith("GIT_")}
        self.env.update(HOME=str(self.home), XDG_RUNTIME_DIR=str(self.runtime),
                        XDG_CACHE_HOME=str(self.root / "cache"), XDG_CONFIG_HOME=str(self.home / ".config"),
                        XDG_DATA_HOME=str(self.home / ".local/share"), XDG_STATE_HOME=str(self.home / ".local/state"))
        self.enterContext(patch.dict(os.environ, self.env, clear=True))

    def write(self, root, name, text):
        path = root / name
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(text)
        return path


class ReleasePreparation(Fixture):
    def setUp(self):
        super().setUp()
        self.repo = self.root / "repo"
        self.repo.mkdir()
        self.enterContext(patch.object(prepare, "ROOT", self.repo))
        self.write(self.repo, "Cargo.toml", '[package]\nname = "mywhisper"\nversion = "0.9.3"\n\n'
                   '[dependencies]\nexample = { version = "1.0.0" }\n')
        self.write(self.repo, "Cargo.lock", 'version = 4\n\n[[package]]\nname = "mywhisper"\nversion = "0.9.3"\n\n'
                   '[[package]]\nname = "example"\nversion = "1.0.0"\n')
        self.write(self.repo, "packaging/arch/PKGBUILD", "pkgver=0.9.3\npkgrel=2\nsha256sums=('" + "0" * 64 + "')\n")
        self.write(self.repo, "packaging/nixos/flake.nix", "version = manifest.package.version;\n")
        self.write(self.repo, "scripts/prepare-release.py", "# Synthetic installed release tooling.\n")
        self.write(self.repo, "CHANGELOG.md", "# Changelog\n\n## 0.9.3\n\n- Released behavior.\n")
        self.git("init", "-q", "--initial-branch=main")
        self.git("config", "--local", "user.name", "Disposable")
        self.git("config", "--local", "user.email", "fixture@example.invalid")
        self.git("config", "--local", "commit.gpgsign", "false")
        self.git("add", ".")
        self.git("commit", "-qm", "Released fixture")
        self.git("tag", "v0.9.3")
        self.write(self.repo, "README.md", "A new related improvement.\n")
        self.git("add", ".")
        self.git("commit", "-qm", "Improve fixture")
        self.bare = self.root / "origin.git"
        subprocess.run(["git", "init", "--bare", "-q", str(self.bare)], check=True)
        self.git("remote", "add", "origin", str(self.bare))
        self.git("push", "-q", "origin", "main", "--tags")
        self.sha = self.git("rev-parse", "HEAD")
        self.notes = "## What's Changed\n* Improve privacy and startup by @fixture in https://example.invalid/pull/5\n\n"
        self.actual_run = prepare.run
        self.gh_calls = []

    def git(self, *args):
        return subprocess.run(["git", "-c", "core.hooksPath=/dev/null", *args], cwd=self.repo,
                              text=True, capture_output=True, check=True).stdout.strip()

    def commands(self, args, **kwargs):
        if args[0] != "gh":
            return self.actual_run(args, **kwargs)
        self.gh_calls.append((args, kwargs))
        if "generate-notes" in " ".join(args):
            body = json.loads(kwargs["stdin"])
            self.assertEqual(body["target_commitish"], self.sha)
            self.assertEqual(body["previous_tag_name"], "v0.9.3")
            return subprocess.CompletedProcess(args, 0, json.dumps({"body": self.notes}), "")
        if args[1:3] == ["label", "list"]:
            return subprocess.CompletedProcess(args, 0, "[]", "")
        if args[1:3] == ["label", "create"]:
            return subprocess.CompletedProcess(args, 0, "", "")
        if args[1:3] == ["pr", "create"]:
            text = Path(args[args.index("--body-file") + 1]).read_text()
            self.assertIn(self.notes, text)
            return subprocess.CompletedProcess(args, 0, "https://example.invalid/pull/9\n", "")
        raise AssertionError(f"unexpected external fixture request: {args}")

    def invoke(self, *args):
        with patch.object(prepare, "run", side_effect=self.commands), patch.object(sys, "argv", ["prepare-release.py", *args]):
            prepare.main()

    def test_preview_preserves_checkout_and_remote_and_maps_prs(self):
        output = self.root / "preview"
        self.invoke("0.9.4", "--repo", "fixture/project", "--previous-tag", "v0.9.3", "--output", str(output))
        self.assertIn("/pull/5", (output / "notes.md").read_text())
        self.assertIn('version = "0.9.4"', (output / "release.patch").read_text())
        self.assertIn(".github/release-source.json", (output / "release.patch").read_text())
        self.assertEqual(json.loads((output / "source.json").read_text())["source_sha"], self.sha)
        self.assertEqual(self.git("rev-parse", "HEAD"), self.sha)
        self.assertEqual(self.git("status", "--porcelain"), "")
        self.assertEqual(self.git("ls-remote", "origin", "refs/heads/release/*"), "")
        self.assertFalse(any(args[1:3] == ["pr", "create"] for args, _ in self.gh_calls))

    def test_explicit_create_pushes_only_private_fixture_branch_without_tagging(self):
        self.invoke("0.9.4", "--repo", "fixture/project", "--previous-tag", "v0.9.3", "--create-pr")
        sha = self.git("ls-remote", "origin", "refs/heads/release/v0.9.4-" + self.sha[:12]).split()[0]
        self.assertNotEqual(sha, self.sha)
        self.assertEqual(self.git("rev-parse", "HEAD"), self.sha)
        self.assertEqual(self.git("ls-remote", "origin", "refs/tags/v0.9.4"), "")
        self.assertIn("/pull/5", self.git("show", sha + ":CHANGELOG.md"))
        manifest = tomllib.loads(self.git("show", sha + ":Cargo.toml"))
        self.assertEqual(manifest["package"]["version"], "0.9.4")
        self.assertEqual(manifest["dependencies"]["example"]["version"], "1.0.0")
        self.assertTrue(any(args[1:3] == ["pr", "create"] for args, _ in self.gh_calls))

    def test_existing_tag_and_branch_are_never_overwritten(self):
        self.git("tag", "v0.9.4")
        self.git("push", "-q", "origin", "v0.9.4")
        with self.assertRaisesRegex(ValueError, "tag already exists"):
            self.invoke("0.9.4", "--repo", "fixture/project", "--previous-tag", "v0.9.3", "--create-pr")
        name = "refs/heads/release/v0.9.5-" + self.sha[:12]
        self.git("push", "-q", "origin", "HEAD:" + name)
        with self.assertRaisesRegex(ValueError, "branch already exists"):
            self.invoke("0.9.5", "--repo", "fixture/project", "--previous-tag", "v0.9.3", "--create-pr")
        self.assertEqual(self.git("ls-remote", "origin", name).split()[0], self.sha)

    def test_version_validation_and_order_include_prereleases(self):
        self.assertLess(prepare.version_key("1.0.0-rc.2"), prepare.version_key("1.0.0-rc.10"))
        self.assertLess(prepare.version_key("1.0.0-rc.10"), prepare.version_key("1.0.0"))
        for value in ["0.9.3", "0.9.2", "0.9.3-rc.1", "v0.9.4", "0.9.4;touch /tmp/unowned"]:
            with self.subTest(value=value), self.assertRaises(ValueError):
                prepare.version_updates(self.repo, value, self.notes)
        updates = prepare.version_updates(self.repo, "0.9.4-rc.1", self.notes)
        self.assertIn("pkgver=0.9.4rc.1", updates["packaging/arch/PKGBUILD"])
        self.assertIn("pkgrel=1", updates["packaging/arch/PKGBUILD"])

    def test_source_snapshot_blocks_changes_merged_after_preparation(self):
        self.invoke("0.9.4", "--repo", "fixture/project", "--previous-tag", "v0.9.3", "--create-pr")
        self.git("merge", "--ff-only", "release/v0.9.4-" + self.sha[:12])
        prepare.release.verify_notes_source(self.repo, "0.9.4")
        manifest = (self.repo / "Cargo.toml").read_text().replace(
            'example = { version = "1.0.0" }', 'example = { version = "1.0.0", features = ["new"] }')
        self.write(self.repo, "Cargo.toml", manifest)
        self.git("add", "Cargo.toml")
        self.git("commit", "-qm", "An intervening dependency change")
        with self.assertRaisesRegex(ValueError, "dependency changes"):
            prepare.release.verify_notes_source(self.repo, "0.9.4")
        self.write(self.repo, "README.md", "Another merged product change.\n")
        self.git("add", "README.md")
        self.git("commit", "-qm", "An intervening product change")
        with self.assertRaisesRegex(ValueError, "product changes"):
            prepare.release.verify_notes_source(self.repo, "0.9.4")

    def test_tag_cli_requires_prepared_notes_and_a_commit_merged_to_main(self):
        self.invoke("0.9.4", "--repo", "fixture/project", "--previous-tag", "v0.9.3", "--create-pr")
        branch = "release/v0.9.4-" + self.sha[:12]
        self.git("switch", branch)
        self.git("tag", "v0.9.4")
        command = [sys.executable, str(HERE.parent / "packaging/release.py"), "v0.9.4",
                   "--root", str(self.repo), "--verify-tag"]
        result = subprocess.run(command, capture_output=True, text=True)
        self.assertNotEqual(result.returncode, 0, "an unmerged release branch was accepted")
        self.git("switch", "main")
        self.git("merge", "--ff-only", branch)
        self.git("push", "-q", "origin", "main")
        result = subprocess.run(command, capture_output=True, text=True)
        self.assertEqual(result.returncode, 0, result.stderr)
        result = subprocess.run(command[:-1] + ["--compare-version-to", self.sha], capture_output=True, text=True)
        self.assertEqual(result.returncode, 0, result.stderr)

    def test_remote_failure_is_not_treated_as_absence(self):
        with patch.object(prepare, "run", return_value=subprocess.CompletedProcess([], 128, "", "offline")):
            with self.assertRaises(subprocess.CalledProcessError):
                prepare.remote_exists("refs/tags/v0.9.4")

    def test_refresh_after_preparation_merge_preserves_version_and_historical_notes(self):
        self.invoke("0.9.4", "--repo", "fixture/project", "--previous-tag", "v0.9.3", "--create-pr")
        self.git("merge", "--ff-only", "release/v0.9.4-" + self.sha[:12])
        old = (self.repo / "CHANGELOG.md").read_text()
        old = old.replace("## 0.9.4", "## [0.9.4](https://example.invalid/tag) - 2026-10-12")
        self.write(self.repo, "CHANGELOG.md", old)
        self.write(self.repo, "README.md", "Another product improvement after preparation.\n")
        self.git("add", ".")
        self.git("commit", "-qm", "Improve after preparation")
        self.git("push", "-q", "origin", "main")
        self.sha = self.git("rev-parse", "HEAD")
        self.notes = "## What's Changed\n* Include both reviewed improvements ([#5](https://example.invalid/pull/5)).\n"
        self.invoke("0.9.4", "--refresh", "--repo", "fixture/project", "--previous-tag", "v0.9.3", "--create-pr")
        self.git("merge", "--ff-only", "release/v0.9.4-" + self.sha[:12])
        prepare.release.verify_notes_source(self.repo, "0.9.4")
        changelog = (self.repo / "CHANGELOG.md").read_text()
        self.assertEqual([v for v, _ in prepare.release.changelog_sections(changelog)].count("0.9.4"), 1)
        self.assertIn("## [0.9.4](https://example.invalid/tag) - 2026-10-12", changelog)
        self.assertIn("Include both reviewed improvements", changelog)
        self.assertIn("Released behavior", changelog)
        self.assertEqual(tomllib.loads((self.repo / "Cargo.toml").read_text())["package"]["version"], "0.9.4")
        command = [sys.executable, str(HERE.parent / "packaging/release.py"), "v0.9.4",
                   "--root", str(self.repo), "--compare-version-to", self.sha]
        result = subprocess.run(command, capture_output=True, text=True)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.write(self.repo, "README.md", "Product work omitted from this refreshed snapshot.\n")
        self.git("add", ".")
        self.git("commit", "-qm", "Intervening product change")
        result = subprocess.run(command, capture_output=True, text=True)
        self.assertNotEqual(result.returncode, 0, "stale same-version refresh passed CI")
        self.assertIn("product changes", result.stderr)

    def test_refresh_rejects_wrong_version_and_published_tags(self):
        with self.assertRaisesRegex(ValueError, "current unpublished"):
            prepare.version_updates(self.repo, "0.9.4", self.notes, refresh=True)
        with self.assertRaisesRegex(ValueError, "tag already exists"):
            self.invoke("0.9.3", "--refresh", "--repo", "fixture/project", "--previous-tag", "v0.9.3", "--create-pr")



if __name__ == "__main__":
    unittest.main()
