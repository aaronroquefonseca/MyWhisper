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
- CI regenerates the listing from the compiled tree and byte-compares the
  test/harness sets: any missing/extra test, target, or ignored-set change
  fails the gate until the manifest is explicitly refreshed in review.
- Batch i of N owns every Nth sorted lib test (stride). The verifier proves
  the batch logs execute every manifest test exactly once, with no
  failures, no unrecognized tests, and the identical ignored set.
- Empty harnesses (bins/examples with zero tests) always run whole via
  --all-targets; the verifier requires their zero-test summaries.
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

import re
import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
BATCHES = 4

LIST_LINE = re.compile(r"^(\S+): test$")
RUNNING_LINE = re.compile(r"^\s*Running unittests (\S+) \(")
COUNT_LINE = re.compile(r"^\s*(\d+) tests?, \d+ benchmarks?$")
RESULT_LINE = re.compile(r"^test (\S+) \.\.\. (ok|ignored|FAILED)", re.MULTILINE)


def cargo_list(target_args):
    process = subprocess.run(
        ["cargo", "test", "--locked", *target_args, "--", "--list"],
        cwd=ROOT,
        capture_output=True,
        text=True,
        timeout=300,
    )
    if process.returncode != 0:
        raise SystemExit(
            f"cargo --list failed for {target_args}:\n{process.stderr[-2000:]}"
        )
    # Running/test-count banners go to stderr; test names to stdout.
    return process.stdout + process.stderr


def cargo_list_ignored(target_args):
    process = subprocess.run(
        ["cargo", "test", "--locked", *target_args, "--", "--list", "--ignored"],
        cwd=ROOT,
        capture_output=True,
        text=True,
        timeout=300,
    )
    if process.returncode != 0:
        raise SystemExit(
            f"cargo --list --ignored failed for {target_args}:\n{process.stderr[-2000:]}"
        )
    return process.stdout


def parse_tests(output):
    names = []
    for line in output.splitlines():
        match = LIST_LINE.match(line)
        if match is not None:
            names.append(match.group(1))
    return sorted(names)


def collect_tree():
    """Return (lib_tests, lib_ignored, harnesses) from the compiled tree."""
    lib_tests = parse_tests(cargo_list(["--lib"]))
    lib_ignored = set(parse_tests(cargo_list_ignored(["--lib"])))
    harnesses = []
    for args, kind in [(["--bins"], "bin"), (["--examples"], "example")]:
        output = cargo_list(args)
        current = None
        total = 0
        for line in output.splitlines():
            running = RUNNING_LINE.match(line)
            if running:
                if current is not None:
                    harnesses.append((current[0], current[1], total))
                name = Path(running.group(1)).stem
                current = (kind, name)
                total = 0
                continue
            count = COUNT_LINE.match(line)
            if count and current is not None:
                total = int(count.group(1))
        if current is not None:
            harnesses.append((current[0], current[1], total))
    return lib_tests, lib_ignored, harnesses


def manifest_entries(lib_tests, lib_ignored, harnesses):
    lines = []
    for name in lib_tests:
        status = "ignored" if name in lib_ignored else "run"
        lines.append(f"test|lib|{name}|{status}")
    for kind, name, total in harnesses:
        if total != 0:
            raise SystemExit(
                f"non-empty {kind} harness {name} has {total} tests: partitioning assumes empty harnesses"
            )
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
        fresh_pairs = sorted((kind, name) for kind, name, _ in fresh_harnesses)
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
            text = Path(log).read_text(errors="replace")
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
                    stem = Path(running.group(1)).stem
                    current = next((h for h in harnesses if h[1] == stem), None)
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
