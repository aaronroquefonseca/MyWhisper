#!/usr/bin/env python3
"""Release gate regressions use synthetic versions/files, never repository tags."""
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import subprocess
import tempfile
import unittest
from unittest.mock import patch

HERE = Path(__file__).resolve().parent
spec = importlib.util.spec_from_file_location("release", HERE / "release.py")
release = importlib.util.module_from_spec(spec)
spec.loader.exec_module(release)


class Release(unittest.TestCase):
    def setUp(self):
        self.directory = tempfile.TemporaryDirectory(prefix="mywhisper-release-regression-")
        self.addCleanup(self.directory.cleanup)
        self.root = Path(self.directory.name)
        self.write("Cargo.toml", '[package]\nname="mywhisper"\nversion="1.2.3"\n')
        self.write("packaging/arch/PKGBUILD", "pkgver=1.2.3\n")
        self.write("packaging/nixos/flake.nix", "version = manifest.package.version;\n")
        self.write("CHANGELOG.md", "## 1.2.3\n\n- Synthetic corrected behavior.\n\n## 1.2.2\n- Old.\n")

    def write(self, name, text):
        path = self.root / name
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(text)

    def test_rejects_tag_package_nix_and_arch_inconsistency(self):
        with self.assertRaisesRegex(ValueError, "Cargo"):
            release.preflight(self.root, "v9.0.0")
        self.write("packaging/arch/PKGBUILD", "pkgver=1.2.2\n")
        with self.assertRaisesRegex(ValueError, "Arch"):
            release.preflight(self.root, "v1.2.3")
        self.write("packaging/arch/PKGBUILD", "pkgver=1.2.3\n")
        self.write("packaging/nixos/flake.nix", 'version = "1.2.3";\n')
        with self.assertRaisesRegex(ValueError, "Nix"):
            release.preflight(self.root, "v1.2.3")

    def test_prerelease_is_explicit_and_malformed_versions_fail(self):
        self.assertFalse(release.preflight(self.root, "v1.2.3")["prerelease"])
        self.write("Cargo.toml", '[package]\nname="mywhisper"\nversion="1.2.3-rc.1"\n')
        self.write("packaging/arch/PKGBUILD", "pkgver=1.2.3rc.1\n")
        self.write("CHANGELOG.md", "## 1.2.3-rc.1\n- Candidate, not a stable installer offer.\n")
        self.assertTrue(release.preflight(self.root, "v1.2.3-rc.1")["prerelease"])
        for value in ["1.2", "01.2.3", "1.2.3-rc.01", "1.2.3+build", "1.2.3-", "1.2.3-界", "1" * 129]:
            with self.subTest(value=value), self.assertRaises(ValueError):
                release.version(value)

    def test_missing_empty_duplicate_or_wrong_notes_block_the_release(self):
        for text in ["## 1.2.2\n- Wrong.\n", "## 1.2.3\n", "## 1.2.3\n- First.\n## 1.2.3\n- Duplicate.\n"]:
            self.write("CHANGELOG.md", text)
            with self.assertRaisesRegex(ValueError, "changelog"):
                release.preflight(self.root, "v1.2.3")

    def test_distribution_requires_owner_license_and_dependency_notices(self):
        with self.assertRaisesRegex(ValueError, "owner"):
            release.require_distribution_license(self.root, [])
        # Synthetic fixture only; this creates no repository project LICENSE.
        self.write("LICENSE", "Disposable owner license fixture.\n")
        with self.assertRaisesRegex(ValueError, "dependency"):
            release.require_distribution_license(self.root, [{"name": "dependency", "license": "MIT", "notice_files": []}])
        release.require_distribution_license(self.root, [{"name": "dependency", "license": "MIT", "notice_files": ["LICENSE"]}])

    def test_candidate_digest_notes_notices_and_hold_are_reviewable(self):
        self.write("target/release/mywhisper", "#!/usr/bin/env python3\nprint('mywhisper 1.2.3')\n")
        (self.root / "target/release/mywhisper").chmod(0o700)
        for source in ["packaging/systemd/mywhisper.service",
                       "assets/applications/com.mywhisper.App.desktop", "assets/icons/app-icon.svg",
                       "assets/icons/symbolic.svg"]:
            self.write(source, "Synthetic artifact fixture.\n")
        output = self.root / "candidate"
        with patch.object(release, "dependency_notices", return_value=("Third party fixture notice.\n", [])):
            result = release.stage(self.root, output, "v1.2.3")
        self.assertFalse(result["project_license_established"])
        digest = hashlib.sha256((output / result["asset"]).read_bytes()).hexdigest()
        self.assertEqual(result["sha256"], digest)
        self.assertEqual((output / "SHA256SUMS").read_text(), f"{digest}  {result['asset']}\n")
        self.assertEqual(json.loads((output / "release.json").read_text()), result)
        self.assertIn("Synthetic corrected behavior", (output / "notes.md").read_text())
        self.assertTrue((output / result["asset"].removesuffix(".tar.gz") / "THIRD_PARTY_NOTICES.txt").is_file())
        with self.assertRaises(FileExistsError):
            release.stage(self.root, output, "v1.2.3")

    def test_container_git_trust_preserves_provenance_and_is_workspace_scoped(self):
        self.write("target/release/mywhisper", "#!/usr/bin/env python3\nprint('mywhisper 1.2.3')\n")
        (self.root / "target/release/mywhisper").chmod(0o700)
        for source in ["packaging/systemd/mywhisper.service",
                       "assets/applications/com.mywhisper.App.desktop", "assets/icons/app-icon.svg",
                       "assets/icons/symbolic.svg"]:
            self.write(source, "Synthetic artifact fixture.\n")
        home = tempfile.TemporaryDirectory(prefix="mywhisper-git-home-")
        other = tempfile.TemporaryDirectory(prefix="mywhisper-untrusted-repo-")
        self.addCleanup(home.cleanup)
        self.addCleanup(other.cleanup)
        env = {key: value for key, value in os.environ.items() if not key.startswith("GIT_")}
        env.update(HOME=home.name, GIT_CONFIG_GLOBAL=str(Path(home.name) / ".gitconfig"),
                   GIT_CONFIG_NOSYSTEM="1", GIT_CONFIG_COUNT="0")
        # Isolate Git from the very first command, including caller signing
        # policies and hooks; no desktop identity/agent is needed to commit.
        self.enterContext(patch.dict(os.environ, env, clear=True))
        subprocess.run(["git", "init", "-q", str(self.root)], check=True, timeout=5)
        subprocess.run(["git", "add", "."], cwd=self.root, check=True, timeout=5)
        subprocess.run(["git", "-c", "user.name=Disposable", "-c", "user.email=fixture@example.invalid",
                        "commit", "-qm", "Disposable source"], cwd=self.root, check=True, timeout=5)
        expected = subprocess.check_output(["git", "rev-parse", "HEAD"], cwd=self.root, text=True, timeout=5).strip()
        subprocess.run(["git", "init", "-q", other.name], check=True, timeout=5)
        # Git's own ownership test switch reproduces the container failure
        # without chown, elevated privileges or any real user's repository.
        output = self.root / "candidate"
        with patch.dict(os.environ, {"GIT_TEST_ASSUME_DIFFERENT_OWNER": "1"}), patch.object(
                release, "dependency_notices", return_value=("Third party fixture notice.\n", [])):
            with self.assertRaises(subprocess.CalledProcessError) as failure:
                release.stage(self.root, output, "v1.2.3")
            self.assertEqual(failure.exception.returncode, 128)
            self.assertFalse(output.exists())
            subprocess.run(["git", "config", "--global", "--add", "safe.directory", str(self.root)],
                           check=True, timeout=5)
            result = release.stage(self.root, output, "v1.2.3")
            self.assertEqual(result["source_sha"], expected)
            self.assertFalse(result["source_dirty"])
            self.assertFalse(result["project_license_established"])
            rejected = subprocess.run(["git", "-C", other.name, "rev-parse", "--git-dir"],
                                      capture_output=True, text=True, timeout=5)
            self.assertEqual(rejected.returncode, 128)
            self.assertIn("dubious ownership", rejected.stderr)

    def test_upstream_notice_matches_locked_crate_commit_and_content(self):
        from types import SimpleNamespace
        directory = self.root / "registry/dependency"
        self.write("registry/dependency/Cargo.toml", "# Synthetic published crate.\n")
        self.write("registry/dependency/.cargo_vcs_info.json", json.dumps({"git": {"sha1": "a" * 40}}))
        self.write("Cargo.lock", '[[package]]\nname="dependency"\nversion="1.0.0"\nchecksum="' + "b" * 64 + '"\n')
        self.write("packaging/notices/OpenWhispr.txt", "Synthetic existing notice.\n")
        content = "Synthetic upstream notice fixture.\n"
        self.write("packaging/notices/upstream.txt", content)
        source = {"crate_checksum": "b" * 64, "source_commit": "a" * 40,
                  "files": [{"path": "upstream.txt", "sha256": hashlib.sha256(content.encode()).hexdigest()}]}
        self.write("packaging/notices/sources.json", json.dumps({"dependency@1.0.0": source}))
        graph = {"resolve": {"root": "root", "nodes": [
            {"id": "root", "deps": [{"pkg": "dependency"}]}, {"id": "dependency", "deps": []}]},
            "packages": [
                {"id": "root", "name": "mywhisper", "version": "1.2.3", "source": None},
                {"id": "dependency", "name": "dependency", "version": "1.0.0",
                 "source": "registry", "license": "MIT", "manifest_path": str(directory / "Cargo.toml")}]}
        with patch.object(release.subprocess, "run", return_value=SimpleNamespace(stdout=json.dumps(graph).encode())):
            text, inventory = release.dependency_notices(self.root)
            self.assertIn(content, text)
            self.assertEqual(inventory[0]["upstream_notice_provenance"], source)
            source["crate_checksum"] = "c" * 64
            self.write("packaging/notices/sources.json", json.dumps({"dependency@1.0.0": source}))
            with self.assertRaisesRegex(ValueError, "provenance"):
                release.dependency_notices(self.root)
            source["crate_checksum"] = "b" * 64
            source["source_commit"] = "c" * 40
            self.write("packaging/notices/sources.json", json.dumps({"dependency@1.0.0": source}))
            with self.assertRaisesRegex(ValueError, "provenance"):
                release.dependency_notices(self.root)
            source["source_commit"] = "a" * 40
            self.write("packaging/notices/sources.json", json.dumps({"dependency@1.0.0": source}))
            self.write("packaging/notices/upstream.txt", "Corrupt fixture.\n")
            with self.assertRaisesRegex(ValueError, "digest"):
                release.dependency_notices(self.root)

    def test_stale_binary_is_rejected_before_creating_an_artifact(self):
        self.write("target/release/mywhisper", "#!/usr/bin/env python3\nprint('mywhisper 1.2.2')\n")
        (self.root / "target/release/mywhisper").chmod(0o700)
        output = self.root / "candidate"
        with self.assertRaisesRegex(ValueError, "binary version"):
            release.stage(self.root, output, "v1.2.3")
        self.assertFalse(output.exists())

    def test_local_patch_inventory_preserves_notices_and_rejects_unreviewed_source(self):
        from types import SimpleNamespace
        directory = self.root / "vendor/dependency"
        files = {"Cargo.toml": "# Disposable local crate.\n",
                 ".cargo_vcs_info.json": json.dumps({"git": {"sha1": "a" * 40}}),
                 "LICENSE-MIT": "Synthetic third-party notice.\n",
                 "src/lib.rs": "// Reviewed correction.\n"}
        for name, content in files.items():
            self.write("vendor/dependency/" + name, content)
        digests = {name: hashlib.sha256(content.encode()).hexdigest() for name, content in files.items()}
        source = {"crate": "dependency", "version": "1.0.0", "crate_checksum": "b" * 64,
                  "source_commit": "a" * 40, "local_files": digests,
                  "upstream_files": {**digests, "src/lib.rs": "c" * 64},
                  "modified_files": ["src/lib.rs"]}
        self.write("vendor/dependency/UPSTREAM.json", json.dumps(source))
        self.write("Cargo.lock", '[[package]]\nname="dependency"\nversion="1.0.0"\n')
        self.write("packaging/notices/OpenWhispr.txt", "Existing third-party notice.\n")
        graph = {"resolve": {"root": "root", "nodes": [
            {"id": "root", "deps": [{"pkg": "dependency"}]}, {"id": "dependency", "deps": []}]},
            "packages": [
                {"id": "root", "name": "mywhisper", "version": "1.2.3", "source": None},
                {"id": "dependency", "name": "dependency", "version": "1.0.0",
                 "source": None, "license": "MIT", "manifest_path": str(directory / "Cargo.toml")}]}
        with patch.object(release.subprocess, "run", return_value=SimpleNamespace(stdout=json.dumps(graph).encode())):
            text, inventory = release.dependency_notices(self.root)
            self.assertIn(files["LICENSE-MIT"], text)
            self.assertEqual(inventory[0]["local_patch_provenance"], source)
            self.write("vendor/dependency/src/lib.rs", "// Unreviewed source.\n")
            with self.assertRaisesRegex(ValueError, "digest"):
                release.dependency_notices(self.root)
            self.write("vendor/dependency/src/lib.rs", files["src/lib.rs"])
            source["modified_files"] = []
            self.write("vendor/dependency/UPSTREAM.json", json.dumps(source))
            with self.assertRaisesRegex(ValueError, "unlisted"):
                release.dependency_notices(self.root)
            source["modified_files"] = ["src/lib.rs"]
            self.write("vendor/dependency/UPSTREAM.json", json.dumps(source))
            (directory / "LICENSE-MIT").unlink()
            with self.assertRaisesRegex(ValueError, "missing"):
                release.dependency_notices(self.root)
            (directory / "LICENSE-MIT").symlink_to(directory / "src/lib.rs")
            with self.assertRaisesRegex(ValueError, "links"):
                release.dependency_notices(self.root)

    def test_source_package_notices_hold_on_missing_dependency_and_preserve_inventory(self):
        directory = self.root / "source-package/share/licenses/mywhisper"
        inventory = [{"name": "dependency", "version": "1.0.0", "license": "MIT",
                      "notice_files": []}]
        with patch.object(release, "dependency_notices", return_value=("Upstream fixture.\n", inventory)):
            with self.assertRaisesRegex(ValueError, "incomplete"):
                release.write_notices(self.root, directory)
            self.assertFalse(directory.exists())
            inventory[0]["notice_files"] = ["LICENSE"]
            self.assertEqual(release.write_notices(self.root, directory), inventory)
            self.assertEqual((directory / "THIRD_PARTY_NOTICES.txt").read_text(), "Upstream fixture.\n")
            self.assertEqual(json.loads((directory / "THIRD_PARTY.json").read_text()), inventory)
            self.assertFalse((directory / "LICENSE").exists())


if __name__ == "__main__":
    unittest.main()
