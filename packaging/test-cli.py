#!/usr/bin/env python3
"""Real informational CLI checks with no keyring, devices or personal data."""
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import tomllib
import unittest

ROOT = Path(__file__).resolve().parent.parent
BINARY = Path(sys.argv.pop(1)).resolve() if len(sys.argv) > 1 else ROOT / 'target/release/mywhisper'
VERSION = tomllib.loads((ROOT / 'Cargo.toml').read_text())['package']['version']


class InformationalCli(unittest.TestCase):
    def check(self, flag, expected):
        with tempfile.TemporaryDirectory(prefix='mywhisper-cli-audit.') as directory:
            root = Path(directory)
            root.chmod(0o700)
            (root / '.disposable').touch(mode=0o600)
            env = os.environ.copy()
            for key in ('DBUS_SESSION_BUS_ADDRESS', 'MYWHISPER_DB_KEY', 'DISPLAY', 'WAYLAND_DISPLAY'):
                env.pop(key, None)
            for key, name in [('HOME', 'home'), ('XDG_CONFIG_HOME', 'config'),
                              ('XDG_DATA_HOME', 'data'), ('XDG_STATE_HOME', 'state'),
                              ('XDG_CACHE_HOME', 'cache'), ('XDG_RUNTIME_DIR', 'runtime')]:
                path = root / name
                path.mkdir(mode=0o700)
                env[key] = str(path)
            result = subprocess.run([str(BINARY), flag], env=env, capture_output=True,
                                    text=True, timeout=2)
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertIn(expected, result.stdout)
            self.assertEqual(result.stderr, '')
            self.assertFalse(any(root.rglob('mywhisper')),
                             'informational CLI created application data')

    def test_version_without_keyring_or_data(self):
        for flag in ('--version', '-V'):
            with self.subTest(flag=flag):
                self.check(flag, f'mywhisper {VERSION}\n')

    def test_help_without_keyring_or_data(self):
        for flag in ('--help', '-h'):
            with self.subTest(flag=flag):
                self.check(flag, 'Usage: mywhisper')


if __name__ == '__main__':
    unittest.main()
