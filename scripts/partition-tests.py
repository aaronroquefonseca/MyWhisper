#!/usr/bin/env python3
"""Deterministic hosted partitioning for the Rust all-target regression suite.

The whole suite (`cargo test --locked --all-targets`) stays REQUIRED and is
executed locally in normal disposable state to detect cross-case
interference. Hosted CI additionally executes the same suite as explicit
sequential batches, each inside its own 120s disposable invocation with
unchanged libtest concurrency, production deadlines and assertions.

Coverage contract (fail closed):
- scripts/test-manifest.txt is CHECKED IN and reviewable. It pins every
  compiled test (target, path, run/ignored) and every empty harness.
- CI regenerates the listing from the compiled tree and compares the
  test/harness sets: any missing/extra test, target, or ignored-set change
  fails the gate until the manifest is explicitly refreshed in review.
  Target identity comes from structured cargo compiler-artifact JSON (never
  human banners, which cargo colorizes under CARGO_TERM_COLOR=always);
  test/ignored names come from canonical libtest --list of each harness
  executable (uncolored when piped).
- Batch i of N owns every Nth sorted lib test (stride). The verifier proves
  the batch logs execute every manifest test exactly once, with no
  failures, no unrecognized tests, and the identical ignored set.
  Log parsing strips ANSI color codes first.
- Empty harnesses (bins/examples with zero tests) always run whole via
  --all-targets; the verifier requires their zero-test summaries.
  A non-empty non-lib harness is an explicit error, not silent coverage.
- libtest filters use --exact full paths: no substring over-matching, so a
  test can never leak into two batches.

Subcommands:
  generate --manifest PATH          refresh the checked-in manifest
  verify-manifest --manifest PATH   fail on any drift vs compiled tree
  filters --manifest PATH --batch i/N
                                    print batch i's exact test paths
  verify-batches --manifest PATH --logs LOG [LOG ...]
                                    prove coverage from batch output logs
"""

import json
import re
import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
BATCHES = 4

LIST_LINE = re.compile(r"^(\S+): test$")
RUNNING_LINE = re.compile(r"Running unittests (\S+) \(")
ANSI_LINE = re.compile(r"\x1b\[[0-9;]*m")
RESULT_LINE = re.compile(r"^test (\S+) \.\.\. (ok|ignored|FAILED)", re.MULTILINE)


def strip_ansi(text):
    return ANSI_LINE.sub("", text)


def cargo_no_run_json():
    """Compile all targets and return stdout artifact JSON (banners on stderr)."""
    process = subprocess.run(
        [
            "cargo",
            "test",
            "--locked",
            "--all-targets",
            "--no-run",
            "--message-format=json",
        ],
        cwd=ROOT,
        capture_output=True,
        text=True,
        timeout=600,
    )
    if process.returncode != 0:
        raise SystemExit(f"cargo --no-run failed:\n{process.stderr[-2000:]}")
    return process.stdout


def parse_harness_artifacts(json_text):
    """Return [(kind, name, executable)] for test harnesses from artifact JSON.

    Skips rlibs (no executable), non-test profiles and non-JSON lines.
    Duplicate (kind, name) entries fail closed.
    """
    harnesses = []
    for line in json_text.splitlines():
        try:
            message = json.loads(line)
        except ValueError:
            continue
        if (
            not isinstance(message, dict)
            or message.get("reason") != "compiler-artifact"
        ):
            continue
        target = message.get("target", {})
        executable = message.get("executable")
        profile = message.get("profile", {})
        if not executable or not profile.get("test"):
            continue
        kinds = target.get("kind", [])
        if "lib" in kinds:
            kind = "lib"
        elif "bin" in kinds:
            kind = "bin"
        elif "example" in kinds:
            kind = "example"
        else:
            continue
        entry = (kind, target.get("name"), executable)
        if any(
            known[0] == kind and known[1] == target.get("name") for known in harnesses
        ):
            raise SystemExit(
                f"duplicate test harness artifact: {kind}/{target.get('name')}"
            )
        harnesses.append(entry)
    return harnesses


def list_executable(executable, ignored_only):
    """Return sorted test names from one harness executable via libtest --list."""
    args = [executable, "--list"] + (["--ignored"] if ignored_only else [])
    process = subprocess.run(
        args, cwd=ROOT, capture_output=True, text=True, timeout=120
    )
    if process.returncode != 0:
        raise SystemExit(
            f"harness --list failed for {executable}:\n{process.stderr[-2000:]}"
        )
    return parse_tests(process.stdout)


def parse_tests(output):
    names = []
    for line in output.splitlines():
        match = LIST_LINE.match(line)
        if match is not None:
            names.append(match.group(1))
    return sorted(names)


def collect_tree():
    """Return (lib_tests, lib_ignored, harnesses) from the compiled tree."""
    lib_tests = []
    lib_ignored = set()
    harnesses = []
    for kind, name, executable in parse_harness_artifacts(cargo_no_run_json()):
        tests = list_executable(executable, False)
        ignored = set(list_executable(executable, True))
        if kind == "lib":
            lib_tests.extend(tests)
            lib_ignored.update(ignored)
        else:
            if tests or ignored:
                raise SystemExit(
                    f"non-empty {kind} harness {name} holds {len(tests)} tests: "
                    "partitioning assumes empty non-lib harnesses; extend the mechanism explicitly"
                )
            harnesses.append((kind, name))
    return sorted(lib_tests), lib_ignored, harnesses


def manifest_entries(lib_tests, lib_ignored, harnesses):
    lines = []
    for name in lib_tests:
        status = "ignored" if name in lib_ignored else "run"
        lines.append(f"test|lib|{name}|{status}")
    for kind, name in sorted(harnesses):
        lines.append(f"harness|{kind}|{name}")
    return lines


def read_manifest(path):
    tests, harnesses = {}, []
    for line in Path(path).read_text().splitlines():
        if not line or line.startswith("#"):
            continue
        fields = line.split("|")
        if fields[0] == "test":
            if (
                len(fields) != 4
                or fields[1] != "lib"
                or fields[3] not in ("run", "ignored")
            ):
                raise SystemExit(f"malformed manifest line: {line}")
            if fields[2] in tests:
                raise SystemExit(f"duplicate manifest test: {fields[2]}")
            tests[fields[2]] = fields[3]
        elif fields[0] == "harness":
            if len(fields) != 3 or fields[1] not in ("bin", "example"):
                raise SystemExit(f"malformed manifest line: {line}")
            harnesses.append((fields[1], fields[2]))
        else:
            raise SystemExit(f"malformed manifest line: {line}")
    return tests, harnesses


def write_manifest(path, lines):
    header = (
        "# Checked-in all-target test manifest. Refresh ONLY in review with:\n"
        "#   python3 scripts/partition-tests.py generate --manifest scripts/test-manifest.txt\n"
        "# then re-run every gate. Batches: 4-way stride over sorted lib tests.\n"
    )
    Path(path).write_text(header + "".join(line + "\n" for line in lines))


def parse_batch(value):
    try:
        index, total = (int(part) for part in value.split("/"))
    except ValueError:
        raise SystemExit("--batch must look like 0/4")
    if total != BATCHES or not 0 <= index < total:
        raise SystemExit(f"--batch must be 0..{BATCHES - 1}/{BATCHES}")
    return index, total


def main(argv):
    if len(argv) < 2:
        raise SystemExit(
            "usage: partition-tests.py {generate|verify-manifest|filters|verify-batches} ..."
        )
    command, rest = argv[1], argv[2:]

    def option(name):
        for index, argument in enumerate(rest):
            if argument == name and index + 1 < len(rest):
                return rest[index + 1]
        raise SystemExit(f"missing {name} value")

    if command == "generate":
        manifest = option("--manifest")
        lib_tests, lib_ignored, harnesses = collect_tree()
        lines = manifest_entries(lib_tests, lib_ignored, harnesses)
        write_manifest(manifest, lines)
        runs = sum(1 for line in lines if line.endswith("|run"))
        ignored = sum(1 for line in lines if line.endswith("|ignored"))
        print(
            f"manifest: {len(lib_tests)} lib tests ({runs} run, {ignored} ignored), {len(harnesses)} empty harnesses"
        )
        return 0

    if command == "verify-manifest":
        manifest = option("--manifest")
        tests, harnesses = read_manifest(manifest)
        lib_tests, lib_ignored, fresh_harnesses = collect_tree()
        fresh = {}
        for name in lib_tests:
            fresh[name] = "ignored" if name in lib_ignored else "run"
        if fresh != tests:
            missing = sorted(set(fresh) - set(tests))
            extra = sorted(set(tests) - set(fresh))
            changed = sorted(
                name for name in set(fresh) & set(tests) if fresh[name] != tests[name]
            )
            raise SystemExit(
                "manifest drift vs compiled tree:\n"
                f"  missing: {missing}\n  extra: {extra}\n  status-changed: {changed}\n"
                "refresh explicitly in review with the generate subcommand"
            )
        fresh_pairs = sorted(fresh_harnesses)
        if fresh_pairs != sorted(harnesses):
            raise SystemExit(
                f"harness drift: compiled {fresh_pairs} vs manifest {sorted(harnesses)}"
            )
        print(f"manifest current: {len(tests)} tests, {len(harnesses)} empty harnesses")
        return 0

    if command == "filters":
        manifest = option("--manifest")
        index, total = parse_batch(option("--batch"))
        tests, _ = read_manifest(manifest)
        ordered = sorted(tests)
        chosen = [
            name for position, name in enumerate(ordered) if position % total == index
        ]
        if not chosen:
            raise SystemExit(
                f"batch {index}/{total} is empty: refusing to run an unfiltered suite"
            )
        print(" ".join(chosen))
        return 0

    if command == "verify-batches":
        manifest = option("--manifest")
        # Collect every --logs value (flag may repeat or take several paths).
        logs = []
        take = False
        for argument in rest:
            if argument == "--logs":
                take = True
                continue
            if argument.startswith("--"):
                take = False
                continue
            if take:
                logs.append(argument)
        if not logs:
            raise SystemExit("missing --logs LOG [LOG ...]")
        tests, harnesses = read_manifest(manifest)
        seen_ok, seen_ignored, failures, unknown = {}, set(), [], []
        harness_runs = {harness: 0 for harness in harnesses}
        for log in logs:
            # Batch logs may carry cargo ANSI color codes (CARGO_TERM_COLOR);
            # strip them before parsing so colored and plain logs verify alike.
            text = strip_ansi(Path(log).read_text(errors="replace"))
            if re.search(r"test result: [^\n]*[1-9]\d* failed", text):
                failures.append(f"{log}: nonzero failed count")
            for match in RESULT_LINE.finditer(text):
                name, status = match.groups()
                if status == "FAILED":
                    failures.append(f"{log}: {name} FAILED")
                elif status == "ok":
                    if name in seen_ok:
                        failures.append(
                            f"{log}: {name} executed twice (also in {seen_ok[name]})"
                        )
                    elif name not in tests:
                        unknown.append(f"{log}: unrecognized test {name}")
                    else:
                        seen_ok[name] = log
                else:
                    seen_ignored.add(name)
                    if name not in tests:
                        unknown.append(f"{log}: unrecognized ignored test {name}")
            current = None
            for line in text.splitlines():
                running = RUNNING_LINE.search(line)
                if running:
                    # Executable stems carry a hash suffix (name-deadbeef);
                    # match the manifest harness by target-name prefix.
                    stem = Path(running.group(1)).stem
                    current = next(
                        (
                            h
                            for h in harnesses
                            if stem == h[1] or stem.startswith(h[1] + "-")
                        ),
                        None,
                    )
                    continue
                if current is not None and line.strip().startswith(
                    "test result: ok. 0 passed"
                ):
                    harness_runs[current] += 1
                    current = None
        missing = sorted(
            name for name in tests if tests[name] == "run" and name not in seen_ok
        )
        if missing:
            failures.append(f"never executed: {missing}")
        want_ignored = {name for name, status in tests.items() if status == "ignored"}
        if seen_ignored != want_ignored:
            failures.append(
                f"ignored-set change: ran {sorted(seen_ignored)} vs manifest {sorted(want_ignored)}"
            )
        for harness, count in harness_runs.items():
            if count < 1:
                failures.append(f"empty harness never ran: {harness}")
        failures.extend(f"coverage: {entry}" for entry in unknown)
        if failures:
            raise SystemExit("batch coverage FAILED:\n" + "\n".join(failures))
        print(
            f"batches cover all {len(tests)} tests exactly once ({len(want_ignored)} ignored), {len(harnesses)} harnesses ran"
        )
        return 0

    raise SystemExit(f"unknown subcommand: {command}")


if __name__ == "__main__":
    sys.exit(main(sys.argv))
