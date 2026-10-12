#!/usr/bin/env python3
"""Disposable installation-contract regressions; never invokes host installers."""
import hashlib
import importlib.util
import io
import json
import os
from pathlib import Path
import subprocess
import tarfile
import tempfile
import unittest

HERE = Path(__file__).resolve().parent
spec = importlib.util.spec_from_file_location("artifact", HERE / "validate-artifact.py")
artifact = importlib.util.module_from_spec(spec)
spec.loader.exec_module(artifact)


def archive(path, extra=None):
    root = "mywhisper-v1.10.0-x86_64-unknown-linux-gnu"
    with tarfile.open(path, "w:gz") as output:
        for name in ["mywhisper", "mywhisper.service", "com.mywhisper.App.desktop", "com.mywhisper.App.svg", "com.mywhisper.App-symbolic.svg"]:
            member = tarfile.TarInfo(f"{root}/{name}")
            member.size = 4
            member.mode = 0o755 if name == "mywhisper" else 0o644
            output.addfile(member, io.BytesIO(b"test"))
        if extra:
            member = tarfile.TarInfo(extra)
            member.type = tarfile.SYMTYPE
            member.linkname = "/etc/passwd"
            output.addfile(member)


class Upgrade(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix="mywhisper-package-check-")
        self.root = Path(self.temp.name)
        self.bin = self.root / "bin"
        self.bin.mkdir()
        self.asset = self.root / "asset.tar.gz"
        archive(self.asset)
        self.original = (HERE / "arch/PKGBUILD").read_bytes()
        self.log = self.root / "commands.jsonl"
        script = '''#!/usr/bin/env python3
import hashlib,json,os,pathlib,shutil,sys
root=pathlib.Path(os.environ["PACKAGE_TEST_ROOT"])
name=pathlib.Path(sys.argv[0]).name
with (root/"commands.jsonl").open("a") as log: log.write(json.dumps([name]+sys.argv[1:])+"\\n")
if name=="gh":
    if sys.argv[2]=="list":
        print(json.dumps([
            {"tagName":"v8.0.0-rc.1","isDraft":False,"isPrerelease":True},
            {"tagName":"v1.2.0","isDraft":False,"isPrerelease":False},
            {"tagName":"v1.10.0","isDraft":False,"isPrerelease":False}]))
    elif sys.argv[2]=="view":
        digest=hashlib.sha256((root/"asset.tar.gz").read_bytes()).hexdigest()
        print(json.dumps({"isDraft":os.environ.get("PACKAGE_TEST_FAIL")=="draft","isPrerelease":False,"assets":[{"name":"mywhisper-v1.10.0-x86_64-unknown-linux-gnu.tar.gz","digest":"sha256:"+digest}]}))
    elif sys.argv[2]=="download":
        directory=pathlib.Path(sys.argv[sys.argv.index("--dir")+1])
        target=directory/"mywhisper-v1.10.0-x86_64-unknown-linux-gnu.tar.gz"
        shutil.copyfile(root/"asset.tar.gz",target)
        if os.environ.get("PACKAGE_TEST_FAIL")=="digest": target.write_bytes(b"corrupt")
elif name=="makepkg":
    text=pathlib.Path("PKGBUILD").read_text()
    digest=hashlib.sha256((root/"asset.tar.gz").read_bytes()).hexdigest()
    assert "pkgver=1.10.0" in text and digest in text and "SKIP" not in text
    if os.environ.get("PACKAGE_TEST_FAIL")=="build": sys.exit(1)
'''
        for name in ["gh", "makepkg", "systemctl"]:
            path = self.bin / name
            path.write_text(script)
            path.chmod(0o755)
        self.env = dict(os.environ, PATH=f"{self.bin}:{os.environ['PATH']}", PACKAGE_TEST_ROOT=str(self.root))

    def tearDown(self):
        self.assertEqual((HERE / "arch/PKGBUILD").read_bytes(), self.original)
        self.temp.cleanup()

    def run_upgrade(self, *args, failure=None):
        env = dict(self.env)
        if failure:
            env["PACKAGE_TEST_FAIL"] = failure
        result = subprocess.run(["bash", str(HERE / "arch/update.sh"), *args], env=env, capture_output=True, timeout=15)
        commands = [json.loads(line) for line in self.log.read_text().splitlines()] if self.log.exists() else []
        return result, commands

    def test_stable_numeric_selection_verified_package_and_upgrade_restart(self):
        result, commands = self.run_upgrade()
        self.assertEqual(result.returncode, 0, result.stderr.decode())
        self.assertIn(["systemctl", "--user", "restart", "mywhisper.service"], commands)
        self.assertNotIn(["systemctl", "--user", "enable", "--now", "mywhisper.service"], commands)
        view = next(command for command in commands if command[:3] == ["gh", "release", "view"])
        self.assertEqual(view[3], "v1.10.0")

    def test_invalid_versions_never_reach_network_or_install(self):
        for version in ["1.2.3-rc.1", "1.2.3;exit 0", "../1.2.3", "vv1.2.3", "01.2.3"]:
            result, commands = self.run_upgrade(version)
            self.assertNotEqual(result.returncode, 0)
            self.assertEqual(commands, [])

    def test_draft_tampering_and_failed_build_never_restart_service(self):
        for failure in ["draft", "digest", "build"]:
            self.log.unlink(missing_ok=True)
            result, commands = self.run_upgrade("1.10.0", failure=failure)
            self.assertNotEqual(result.returncode, 0)
            self.assertFalse(any(command[0] == "systemctl" for command in commands))
            if failure != "build":
                self.assertFalse(any(command[0] == "makepkg" for command in commands))

    def test_authenticated_digest_does_not_allow_archive_escape_or_links(self):
        archive(self.asset, "mywhisper-v1.10.0-x86_64-unknown-linux-gnu/escape")
        result, commands = self.run_upgrade("1.10.0")
        self.assertNotEqual(result.returncode, 0)
        self.assertFalse(any(command[0] in ("makepkg", "systemctl") for command in commands))

    def test_archive_validator_requires_complete_executable_artifact(self):
        artifact.validate(self.asset, "1.10.0")
        for member_name in ["../escape", "/escape", "foreign/file"]:
            archive(self.asset, member_name)
            with self.assertRaises(ValueError):
                artifact.validate(self.asset, "1.10.0")


if __name__ == "__main__":
    unittest.main()
