#!/usr/bin/env python3
"""Exercise the Bash tester with private Git/process/service-manager fixtures."""
import os
from pathlib import Path
import shlex
import signal
import subprocess
import tempfile
import time
import unittest

SCRIPT = Path(__file__).resolve().with_name("pr-test.sh")


class Tester(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.compiler = tempfile.TemporaryDirectory(prefix="mywhisper-bash-elf-")
        directory = Path(cls.compiler.name)
        source = directory / "fixture.c"
        source.write_text('''#include <stdio.h>
#include <stdlib.h>
#include <unistd.h>
int main(void) {
  const char *path = getenv("TEST_GUI_PID");
  if (path) { FILE *f = fopen(path, "w"); if (!f) return 2;
    fprintf(f, "%ld\\n", (long)getpid()); fclose(f); }
  if (getenv("TEST_GUI_EXIT")) { usleep(500000); return 0; }
  for (;;) pause();
}
''')
        cls.elf = directory / "fixture"
        subprocess.run(["cc", str(source), "-o", str(cls.elf)], check=True, timeout=15)

    @classmethod
    def tearDownClass(cls):
        cls.compiler.cleanup()

    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix="mywhisper-bash-test-")
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.home = self.root / "home"
        self.runtime = self.root / "runtime"
        self.repo = self.root / "repo"
        for path in (self.home, self.runtime, self.repo):
            path.mkdir(mode=0o700)
        self.env = {k: v for k, v in os.environ.items() if not k.startswith("GIT_")}
        self.env.update(HOME=str(self.home), XDG_RUNTIME_DIR=str(self.runtime),
                        XDG_CONFIG_HOME=str(self.home / "config"), XDG_DATA_HOME=str(self.home / "data"),
                        XDG_STATE_HOME=str(self.home / "state"), XDG_CACHE_HOME=str(self.root / "cache"),
                        CARGO_TARGET_DIR=str(self.root / "target"), TEST_REPO=str(self.repo),
                        TEST_ROOT=str(self.root), TEST_ELF=str(self.elf), TEST_GUI_PID=str(self.root / "gui.pid"),
                        TEST_GUI_EXIT="1", GIT_CONFIG_GLOBAL=str(self.home / ".gitconfig"),
                        GIT_CONFIG_NOSYSTEM="1", GIT_CONFIG_COUNT="0")
        self.git("init", "-q", "--initial-branch=main")
        self.git("config", "user.name", "Disposable")
        self.git("config", "user.email", "fixture@example.invalid")
        self.git("config", "commit.gpgsign", "false")
        (self.repo / "README.md").write_text("Synthetic source\n")
        self.git("add", ".")
        self.git("commit", "-qm", "Fixture source")
        self.control = self.runtime / "mywhisper-pr-test"
        self.override = self.runtime / "systemd/user/mywhisper.service.d/zz-mywhisper-pr-test.conf"

    def git(self, *args):
        return subprocess.run(["git", "-c", "core.hooksPath=/dev/null", *args], cwd=self.repo,
                              env=self.env, text=True, capture_output=True, check=True).stdout.strip()

    def prelude(self):
        return f'''source {shlex.quote(str(SCRIPT))}
umask 077
ROOT=$TEST_REPO
CONTROL=$XDG_RUNTIME_DIR/mywhisper-pr-test
OVERRIDE=$XDG_RUNTIME_DIR/systemd/user/mywhisper.service.d/$OVERRIDE_NAME
private_directory "$CONTROL"
private_directory "$TEST_ROOT/session"
manager() {{
  printf '%s\\n' "$*" >> "$TEST_ROOT/manager.calls"
  case $1 in
    show)
      case $3 in
        --property=LoadState,ActiveState) printf 'LoadState=loaded\\nActiveState=%s\\n' "${{TEST_ACTIVE:-active}}" ;;
        --property=DropInPaths) printf '%s%s\\n' "$OVERRIDE" "${{TEST_MASK:+ /other/zzzz-owner.conf}}" ;;
        --property=MainPID) printf '%s\\n' "${{TEST_SERVICE_PID:-0}}" ;;
      esac ;;
    start) [[ ${{TEST_START_FAIL:-0}} == 0 ]] ;;
  esac
}}
bus_owned() {{
  if [[ $1 == "$GUI_ID" && ( ${{TEST_EXISTING_GUI:-0}} == 1 || -s $TEST_GUI_PID ) ]]; then printf true
  elif [[ $1 == "$SERVICE_ID" && -n ${{TEST_SERVICE_PID:-}} ]]; then printf true
  else printf false; fi
}}
bus_call() {{ printf '(uint32 %s,)\\n' "${{TEST_OWNER_PID:-$(cat "$TEST_GUI_PID")}}"; }}
if [[ ${{TEST_REAL_SERVICE_WAIT:-0}} == 0 ]]; then wait_for_service() {{ return 0; }}; fi
cargo() {{
  printf '%s\\n' "$*" >> "$TEST_ROOT/cargo.calls"
  [[ ${{TEST_BUILD_FAIL:-0}} == 0 ]] || return 42
  [[ $* == 'build --locked --bin mywhisper' ]] || return 43
  mkdir -p "$CARGO_TARGET_DIR/debug"
  cp "$TEST_ELF" "$CARGO_TARGET_DIR/debug/mywhisper"
}}
'''

    def invoke(self, code, *, check=True, extra=None):
        env = self.env | (extra or {})
        result = subprocess.run(["bash", "-c", self.prelude() + code], env=env,
                                text=True, capture_output=True, timeout=10)
        if check:
            self.assertEqual(result.returncode, 0, result.stderr)
        return result

    def calls(self):
        path = self.root / "manager.calls"
        return path.read_text().splitlines() if path.exists() else []

    def test_fresh_storage_key_and_real_profile_environment(self):
        sentinel = self.home / "data/mywhisper/state.db"
        sentinel.parent.mkdir(parents=True)
        sentinel.write_text("Original personal data")
        self.invoke('''application_environment "$TEST_ROOT/session" 1
printf '%s\\n' "${APP_ENV[HOME]}" "${APP_ENV[XDG_DATA_HOME]}" > "$TEST_ROOT/isolated.paths"
private_directory "$TEST_ROOT/second"
application_environment "$TEST_ROOT/second" 1
application_environment "$TEST_ROOT/session" 0
printf '%s\\n' "${APP_ENV[HOME]}" "${APP_ENV[XDG_DATA_HOME]}" > "$TEST_ROOT/real.paths"
''')
        first = self.root / "session/test-db-key"
        second = self.root / "second/test-db-key"
        self.assertEqual(len(first.read_text().strip()), 64)
        self.assertNotEqual(first.read_text(), second.read_text())
        self.assertEqual(first.stat().st_mode & 0o777, 0o600)
        self.assertEqual(sentinel.read_text(), "Original personal data")
        self.assertIn(str(self.root / "session/data"), (self.root / "isolated.paths").read_text())
        self.assertIn(str(self.home / "data"), (self.root / "real.paths").read_text())
        self.assertIn(str(self.root / "session/home"), (self.root / "real.paths").read_text())

    def test_active_and_inactive_service_restore_previous_running_state(self):
        for active in ("active", "inactive"):
            with self.subTest(active=active):
                (self.root / "manager.calls").unlink(missing_ok=True)
                self.invoke('''application_environment "$TEST_ROOT/session" 0
replace_service "$CONTROL" "$OVERRIDE" "$TEST_ELF"
restore_service "$CONTROL" "$OVERRIDE"
''', extra={"TEST_ACTIVE": active})
                self.assertFalse(self.override.exists())
                self.assertFalse((self.control / "restore.state").exists())
                self.assertEqual(self.calls().count("start mywhisper.service"), 2 if active == "active" else 1)
                self.assertFalse(any("enable" in call or "disable" in call for call in self.calls()))

    def test_masking_override_rejects_test_start_and_recovers(self):
        self.invoke('''application_environment "$TEST_ROOT/session" 0
if replace_service "$CONTROL" "$OVERRIDE" "$TEST_ELF"; then exit 88; fi
restore_service "$CONTROL" "$OVERRIDE"
''', extra={"TEST_MASK": "1"})
        self.assertEqual(self.calls().count("start mywhisper.service"), 1)
        self.assertFalse(self.override.exists())

    def test_changed_override_is_preserved_without_stopping_service(self):
        result = self.invoke('''application_environment "$TEST_ROOT/session" 0
replace_service "$CONTROL" "$OVERRIDE" "$TEST_ELF"
printf 'Owner changed this\\n' > "$OVERRIDE"
: > "$TEST_ROOT/manager.calls"
if restore_service "$CONTROL" "$OVERRIDE"; then exit 88; fi
''')
        self.assertIn("preserving", result.stderr)
        self.assertEqual(self.calls(), [])
        self.assertEqual(self.override.read_text(), "Owner changed this\n")
        self.assertTrue((self.control / "restore.state").exists())

    def test_failed_start_and_recovery_keep_record_until_restored(self):
        self.invoke('''application_environment "$TEST_ROOT/session" 0
TEST_START_FAIL=1
if replace_service "$CONTROL" "$OVERRIDE" "$TEST_ELF"; then exit 88; fi
if restore_service "$CONTROL" "$OVERRIDE"; then exit 89; fi
[[ -f $CONTROL/restore.state && ! -e $OVERRIDE ]]
TEST_START_FAIL=0
restore_service "$CONTROL" "$OVERRIDE"
''')
        self.assertFalse((self.control / "restore.state").exists())

    def test_failed_build_does_not_touch_service(self):
        result = self.invoke('main --local --mode service', check=False, extra={"TEST_BUILD_FAIL": "1"})
        self.assertNotEqual(result.returncode, 0)
        self.assertEqual(self.calls(), [])
        self.assertFalse((self.control / "restore.state").exists())

    def test_existing_gui_and_unavailable_bus_reject_before_build(self):
        result = self.invoke('main --local', check=False, extra={"TEST_EXISTING_GUI": "1"})
        self.assertIn("close the installed", result.stderr)
        self.assertFalse((self.root / "cargo.calls").exists())
        result = self.invoke('bus_owned() { return 1; }; main --local', check=False)
        self.assertNotEqual(result.returncode, 0)
        self.assertFalse((self.root / "cargo.calls").exists())

    def test_systemd_quoting_and_newline_rejection(self):
        result = self.invoke('''APP_ENV[HOME]='/path/with % and $ and "quotes"'
override_text '/binary/with % and $ and "quotes"'
if override_text $'/binary\\nExecStart=/unowned'; then exit 88; fi
''')
        self.assertIn('ExecStart=:"/binary/with %% and $ and \\"quotes\\"" --service', result.stdout)
        self.assertIn('Environment="HOME=/path/with %% and $ and \\"quotes\\""', result.stdout)

    def test_restore_does_not_evaluate_record_as_shell(self):
        result = self.invoke('''printf 'version=1\\nwas_active=1\\noverride_sha256=$(touch %s)\\n' "$TEST_ROOT/unowned" > "$CONTROL/restore.state"
if restore_service "$CONTROL" "$OVERRIDE"; then exit 88; fi
''')
        self.assertIn("invalid recovery", result.stderr)
        self.assertFalse((self.root / "unowned").exists())
        self.assertEqual(self.calls(), [])

    def test_exact_pr_fetch_and_modified_cache_rejection(self):
        sha = self.git("rev-parse", "HEAD")
        bare = self.root / "origin.git"
        subprocess.run(["git", "init", "--bare", "-q", str(bare)], env=self.env, check=True)
        self.git("remote", "add", "origin", str(bare))
        self.git("push", "-q", "origin", "HEAD:refs/pull/13/head")
        result = self.invoke('main --pr 13')
        self.assertIn(sha[:12], result.stdout)
        self.assertEqual(self.git("rev-parse", "HEAD"), sha)
        self.assertEqual(self.git("status", "--porcelain"), "")
        worktree = next((self.root / "cache/mywhisper/pr-tests").glob("*/worktrees/*"))
        (worktree / "edited.txt").write_text("Unreviewed change")
        (self.root / "gui.pid").unlink()
        result = self.invoke('main --pr 13', check=False)
        self.assertIn("cached PR source was edited", result.stderr)

    def test_whole_gui_launch_is_fast_and_does_not_replace_service(self):
        result = self.invoke('main --local')
        self.assertIn("GUI registered from the built binary", result.stdout)
        self.assertEqual(self.calls(), [])
        self.assertFalse((self.control / "restore.state").exists())
        self.assertEqual((self.root / "cargo.calls").read_text(), "build --locked --bin mywhisper\n")

    def test_whole_service_launch_failure_runs_exit_recovery(self):
        result = self.invoke('main --local --mode service', check=False, extra={"TEST_START_FAIL": "1"})
        self.assertNotEqual(result.returncode, 0)
        self.assertFalse(self.override.exists())
        self.assertTrue((self.control / "restore.state").exists())
        self.assertIn("Recovery retained", result.stderr)
        self.invoke('main --restore')
        self.assertFalse((self.control / "restore.state").exists())

    def test_concurrent_session_and_parent_death_cleanup(self):
        env = self.env.copy()
        env.pop("TEST_GUI_EXIT")
        helper = subprocess.Popen(["bash", "-c", self.prelude() + 'main --local'], env=env,
                                  stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
        self.addCleanup(lambda: helper.poll() is not None or helper.kill())
        for _ in range(100):
            if (self.root / "gui.pid").exists():
                break
            time.sleep(0.03)
        else:
            helper.kill()
            out, err = helper.communicate(timeout=5)
            self.fail(out + err)
        pid = int((self.root / "gui.pid").read_text())
        duplicate = self.invoke('main --local', check=False)
        self.assertIn("another PR test session", duplicate.stderr)
        helper.kill()
        helper.communicate(timeout=5)
        for _ in range(50):
            stat = Path(f"/proc/{pid}/stat")
            if not stat.exists() or stat.read_text().split(")", 1)[1].split()[0] == "Z":
                break
            time.sleep(0.03)
        else:
            os.kill(pid, signal.SIGKILL)
            self.fail("GUI survived its foreground Bash helper")

    def test_wrong_gui_pid_is_not_reported_ready(self):
        result = self.invoke('main --local', check=False, extra={"TEST_OWNER_PID": "999"})
        self.assertIn("another GUI acquired", result.stderr)
        self.assertNotIn("GUI registered", result.stdout)

    def test_service_pid_must_be_the_built_executable(self):
        env = self.env.copy()
        env.pop("TEST_GUI_EXIT")
        process = subprocess.Popen([str(self.elf)], env=env)
        try:
            extra = {"TEST_REAL_SERVICE_WAIT": "1", "TEST_SERVICE_PID": str(process.pid),
                     "TEST_OWNER_PID": str(process.pid)}
            self.invoke('wait_for_service "$TEST_ELF"', extra=extra)
            result = self.invoke('wait_for_service /bin/true', check=False, extra=extra)
            self.assertIn("not running the built PR binary", result.stderr)
        finally:
            process.terminate()
            process.wait(timeout=5)

    def test_term_signal_restores_service_and_terminates_owned_gui(self):
        env = self.env.copy()
        env.pop("TEST_GUI_EXIT")
        helper = subprocess.Popen(["bash", "-c", self.prelude() + 'main --local --mode service'], env=env,
                                  stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
        try:
            for _ in range(100):
                if (self.root / "gui.pid").exists():
                    break
                time.sleep(0.03)
            else:
                self.fail("service-mode GUI did not start")
            helper.terminate()
            out, err = helper.communicate(timeout=8)
            self.assertEqual(helper.returncode, 143, out + err)
            self.assertFalse(self.override.exists())
            self.assertFalse((self.control / "restore.state").exists())
            self.assertEqual(self.calls().count("start mywhisper.service"), 2)
        finally:
            if helper.poll() is None:
                helper.kill()
                helper.communicate(timeout=5)


if __name__ == "__main__":
    unittest.main()
