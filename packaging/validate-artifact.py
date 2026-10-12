#!/usr/bin/env python3
"""Validate release archive layout and extraction bounds before package tooling."""
import re
import sys
import tarfile


def validate(path, version):
    if not re.fullmatch(r"(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)(?:-[0-9A-Za-z.-]+)?", version):
        raise ValueError("invalid release version")
    root = f"mywhisper-v{version}-x86_64-unknown-linux-gnu"
    required = {f"{root}/{name}" for name in (
        "mywhisper", "mywhisper.service", "com.mywhisper.App.desktop",
        "com.mywhisper.App.svg", "com.mywhisper.App-symbolic.svg",
    )}
    seen = set()
    total = 0
    with tarfile.open(path, "r:gz") as archive:
        for index, member in enumerate(archive):
            if index >= 128:
                raise ValueError("too many archive members")
            name = member.name.removeprefix("./").rstrip("/")
            if any(part in ("", ".", "..") for part in name.split("/")):
                raise ValueError("invalid archive path")
            if name != root and not name.startswith(root + "/"):
                raise ValueError("archive member escapes release directory")
            if name in seen:
                raise ValueError("duplicate archive member")
            if not member.isfile() and not member.isdir():
                raise ValueError("release contains a link or special file")
            total += member.size
            if total > 256 * 1024 * 1024:
                raise ValueError("release expansion limit exceeded")
            if name in required and (not member.isfile() or member.size == 0):
                raise ValueError("required artifact is empty or not a file")
            if name == f"{root}/mywhisper" and not member.mode & 0o111:
                raise ValueError("binary is not executable")
            seen.add(name)
    if required - seen:
        raise ValueError("release is missing required artifacts")


if __name__ == "__main__":
    try:
        validate(sys.argv[1], sys.argv[2])
    except (ValueError, OSError, tarfile.TarError, IndexError) as error:
        sys.exit(f"Invalid release artifact: {error}")
