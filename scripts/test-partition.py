#!/usr/bin/env python3
"""Regressions for scripts/partition-tests.py using synthetic manifests/logs.

No cargo invocation: collect_tree is monkeypatched for manifest-drift cases.
Real-tree coverage is proven by running the documented workflow on this scope.
"""

import importlib.util
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

SPEC = importlib.util.spec_from_file_location(
    "partition_tests", Path(__file__).with_name("partition-tests.py")
)
assert SPEC is not None and SPEC.loader is not None
PARTITION = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(PARTITION)

NAMES = [f"mod::tests::case_{index:02d}" for index in range(10)]
IGNORED = {"mod::tests::case_09"}
HARNESSES = [("bin", "mywhisper"), ("example", "ui_preview")]


def synthetic_manifest(directory):
    manifest = Path(directory) / "manifest.txt"
    lines = [
        f"test|lib|{name}|{'ignored' if name in IGNORED else 'run'}" for name in NAMES
    ]
    lines += [f"harness|{kind}|{name}" for kind, name in HARNESSES]
    manifest.write_text("# synthetic\n" + "\n".join(lines) + "\n")
    return manifest


def synthetic_logs(
    directory,
    drop=None,
    duplicate=None,
    extra=None,
    failed=None,
    ignored_extra=None,
    skip_harness=False,
):
    logs = []
    for batch in range(4):
        chosen = [
            name for position, name in enumerate(sorted(NAMES)) if position % 4 == batch
        ]
        lines = []
        if not skip_harness:
            lines.append(
                "     Running unittests src/bin/mywhisper.rs (target/debug/deps/mywhisper-abcdef)"
            )
            lines.append(
                "test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s"
            )
            lines.append(
                "     Running unittests examples/ui_preview.rs (target/debug/examples/ui_preview-123456)"
            )
            lines.append(
                "test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s"
            )
        for name in chosen:
            if name == drop:
                continue
            if name in IGNORED or name == ignored_extra:
                lines.append(f"test {name} ... ignored")
            else:
                lines.append(f"test {name} ... {'FAILED' if name == failed else 'ok'}")
        if duplicate is not None and duplicate in chosen:
            lines.append(f"test {duplicate} ... ok")
        if extra is not None and batch == 0:
            lines.append(f"test {extra} ... ok")
        log = Path(directory) / f"batch-{batch}.log"
        log.write_text("\n".join(lines) + "\n")
        logs.append(log)
    return logs


class PartitionContract(unittest.TestCase):
    def test_stride_covers_manifest_exactly_once(self):
        with tempfile.TemporaryDirectory() as directory:
            manifest = synthetic_manifest(directory)
            seen = []
            for batch in range(4):
                process = subprocess.run(
                    [
                        sys.executable,
                        str(Path(__file__).with_name("partition-tests.py")),
                        "filters",
                        "--manifest",
                        str(manifest),
                        "--batch",
                        f"{batch}/4",
                    ],
                    capture_output=True,
                    text=True,
                    timeout=30,
                )
                self.assertEqual(process.returncode, 0, process.stderr)
                seen.extend(process.stdout.split())
            self.assertEqual(sorted(seen), sorted(NAMES))
            self.assertEqual(len(seen), len(set(seen)))

    def test_rejects_bad_batch_and_empty_slice(self):
        with tempfile.TemporaryDirectory() as directory:
            manifest = synthetic_manifest(directory)
            script = str(Path(__file__).with_name("partition-tests.py"))
            for spec in ["4/4", "0/3", "x/4"]:
                process = subprocess.run(
                    [
                        sys.executable,
                        script,
                        "filters",
                        "--manifest",
                        str(manifest),
                        "--batch",
                        spec,
                    ],
                    capture_output=True,
                    text=True,
                    timeout=30,
                )
                self.assertNotEqual(process.returncode, 0, spec)
            tiny = Path(directory) / "tiny.txt"
            tiny.write_text("test|lib|only::one|run\n")
            process = subprocess.run(
                [
                    sys.executable,
                    script,
                    "filters",
                    "--manifest",
                    str(tiny),
                    "--batch",
                    "3/4",
                ],
                capture_output=True,
                text=True,
                timeout=30,
            )
            self.assertNotEqual(process.returncode, 0)
            self.assertIn("empty", process.stderr)

    def test_rejects_malformed_and_duplicate_manifest(self):
        with tempfile.TemporaryDirectory() as directory:
            bad = Path(directory) / "bad.txt"
            bad.write_text("test|lib|only-name\n")
            with self.assertRaises(SystemExit):
                PARTITION.read_manifest(bad)
            duplicate = Path(directory) / "dup.txt"
            duplicate.write_text("test|lib|a::b|run\ntest|lib|a::b|run\n")
            with self.assertRaises(SystemExit):
                PARTITION.read_manifest(duplicate)

    def test_verify_batches_accepts_faithful_logs(self):
        with tempfile.TemporaryDirectory() as directory:
            manifest = synthetic_manifest(directory)
            logs = synthetic_logs(directory)
            self.assertEqual(
                PARTITION.main(
                    [
                        "partition-tests.py",
                        "verify-batches",
                        "--manifest",
                        str(manifest),
                        "--logs",
                        *[str(log) for log in logs],
                    ]
                ),
                0,
            )

    def test_verify_batches_fails_closed(self):
        cases = [
            {"drop": "mod::tests::case_00"},
            {"duplicate": "mod::tests::case_01"},
            {"extra": "mod::tests::case_phantom"},
            {"failed": "mod::tests::case_02"},
            {"ignored_extra": "mod::tests::case_03"},
            {"skip_harness": True},
        ]
        for index, kwargs in enumerate(cases):
            with self.subTest(case=index), tempfile.TemporaryDirectory() as directory:
                manifest = synthetic_manifest(directory)
                logs = synthetic_logs(directory, **kwargs)
                with self.assertRaises(SystemExit):
                    PARTITION.main(
                        [
                            "partition-tests.py",
                            "verify-batches",
                            "--manifest",
                            str(manifest),
                            "--logs",
                            *[str(log) for log in logs],
                        ]
                    )

    def test_verify_manifest_detects_drift(self):
        from unittest import mock

        argv = ["partition-tests.py", "verify-manifest"]
        with tempfile.TemporaryDirectory() as directory:
            manifest = synthetic_manifest(directory)
            extra = (NAMES + ["mod::tests::case_new"], IGNORED, HARNESSES)
            with mock.patch.object(PARTITION, "collect_tree", return_value=extra):
                with self.assertRaises(SystemExit):
                    PARTITION.main([*argv, "--manifest", str(manifest)])
            changed = (NAMES, IGNORED | {"mod::tests::case_00"}, HARNESSES)
            with mock.patch.object(PARTITION, "collect_tree", return_value=changed):
                with self.assertRaises(SystemExit):
                    PARTITION.main([*argv, "--manifest", str(manifest)])
            current = (NAMES, IGNORED, HARNESSES)
            with mock.patch.object(PARTITION, "collect_tree", return_value=current):
                self.assertEqual(
                    PARTITION.main([*argv, "--manifest", str(manifest)]), 0
                )

    def test_parse_harness_artifacts_selects_only_test_harnesses(self):
        import json

        def artifact(kind, name, executable, test):
            return json.dumps(
                {
                    "reason": "compiler-artifact",
                    "target": {"kind": [kind], "name": name},
                    "executable": executable,
                    "profile": {"test": test},
                }
            )

        lines = [
            artifact("lib", "mywhisper", "/tmp/lib-hash", True),
            artifact("bin", "mywhisper", "/tmp/bin-hash", True),
            artifact("example", "ui_preview", "/tmp/ex-hash", True),
            artifact("lib", "mywhisper", None, test=False),  # rlib, no executable
            json.dumps({"reason": "build-script", "package": "x"}),
            artifact("bin", "pill", "/tmp/pill-hash", test=False),
            "not json at all",
            # ANSI-colored cargo banners live on stderr and never feed identity.
            "\x1b[1m\x1b[32m     Running\x1b[0m unittests src/lib.rs (hash)",
        ]
        self.assertEqual(
            PARTITION.parse_harness_artifacts("\n".join(lines)),
            [
                ("lib", "mywhisper", "/tmp/lib-hash"),
                ("bin", "mywhisper", "/tmp/bin-hash"),
                ("example", "ui_preview", "/tmp/ex-hash"),
            ],
        )
        with self.assertRaises(SystemExit):
            PARTITION.parse_harness_artifacts(
                "\n".join(
                    [
                        artifact("bin", "mywhisper", "/tmp/a", True),
                        artifact("bin", "mywhisper", "/tmp/b", True),
                    ]
                )
            )

    def test_verify_batches_accepts_colored_logs(self):
        with tempfile.TemporaryDirectory() as directory:
            manifest = synthetic_manifest(directory)
            log = Path(directory) / "batch-0.log"
            lines = [
                "     \x1b[1m\x1b[32mRunning\x1b[0m unittests src/bin/mywhisper.rs (target/debug/deps/mywhisper-abcdef)",
                "test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s",
                "     \x1b[1m\x1b[32mRunning\x1b[0m unittests examples/ui_preview.rs (target/debug/examples/ui_preview-123456)",
                "test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s",
            ]
            for name in NAMES:
                status = "ignored" if name in IGNORED else "ok"
                lines.append(f"test {name} ... {status}")
            log.write_text("\n".join(lines) + "\n")
            self.assertEqual(
                PARTITION.main(
                    [
                        "partition-tests.py",
                        "verify-batches",
                        "--manifest",
                        str(manifest),
                        "--logs",
                        str(log),
                    ]
                ),
                0,
            )

    def test_collect_tree_rejects_nonempty_nonlib_harness(self):
        from unittest import mock

        artifacts = [("lib", "mywhisper", "/tmp/lib"), ("bin", "tool", "/tmp/tool")]
        listings = {"/tmp/lib": (["a::one"], []), "/tmp/tool": (["b::two"], [])}

        def fake_list(executable, ignored_only):
            tests, ignored = listings[executable]
            return sorted(ignored) if ignored_only else sorted(tests)

        with mock.patch.object(
            PARTITION, "parse_harness_artifacts", return_value=artifacts
        ):
            with mock.patch.object(PARTITION, "list_executable", side_effect=fake_list):
                with self.assertRaises(SystemExit):
                    PARTITION.collect_tree()

    def test_verify_batches_reports_nested_harness_output(self):
        with tempfile.TemporaryDirectory() as directory:
            manifest = synthetic_manifest(directory)
            log = Path(directory) / "batch-0.log"
            lines = [
                "     Running unittests src/lib.rs (target/debug/deps/mywhisper-abcdef)",
                "running 10 tests",
            ]
            for name in NAMES:
                status = "ignored" if name in IGNORED else "ok"
                lines.append(f"test {name} ... {status}")
            # A test that re-executes the harness with inherited stdout leaves
            # a second suite start and a duplicate result line behind.
            lines.append("running 1 test")
            lines.append("test mod::tests::case_00 ... ok")
            lines.append(
                "test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.01s"
            )
            for kind, name in HARNESSES:
                lines.append(
                    f"     Running unittests src/{name}.rs (target/debug/deps/{name}-123456)"
                )
                lines.append(
                    "test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s"
                )
            log.write_text("\n".join(lines) + "\n")
            try:
                PARTITION.main(
                    [
                        "partition-tests.py",
                        "verify-batches",
                        "--manifest",
                        str(manifest),
                        "--logs",
                        str(log),
                    ]
                )
            except SystemExit as error:
                self.assertIn("nested harness output", str(error))
            else:
                raise AssertionError("nested harness output passed verification")


if __name__ == "__main__":
    unittest.main()
