#!/usr/bin/env python3
"""Validate release archive layout and extraction bounds before package tooling."""
import re
import sys
import tarfile


def validate(path, version):
    if not re.fullmatch(r"(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)(?:-[0-9A-Za-z.-]+)?", version):
        raise ValueError("invalid release version")
    # Release archives are per-host-arch; accept exactly one supported triple.
    roots = {f"mywhisper-v{version}-{arch}-unknown-linux-gnu" for arch in ("x86_64", "aarch64")}
    required_names = (
        "mywhisper", "mywhisper.service", "com.mywhisper.App.desktop",
        "com.mywhisper.App.svg", "com.mywhisper.App-symbolic.svg",
    )
    seen = set()
    total = 0
    root = None
    with tarfile.open(path, "r:gz") as archive:
        for index, member in enumerate(archive):
            if index >= 128:
                raise ValueError("too many archive members")
            name = member.name.removeprefix("./").rstrip("/")
            if any(part in ("", ".", "..") for part in name.split("/")):
                raise ValueError("invalid archive path")
            match = next((c for c in roots if name == c or name.startswith(c + "/")), None)
            if match is None:
                raise ValueError("archive member escapes release directory")
            if root is None:
                root = match
            elif root != match:
                raise ValueError("archive mixes release architectures")
            wanted = {f"{root}/{entry}" for entry in required_names}
            if name in seen:
                raise ValueError("duplicate archive member")
            if not member.isfile() and not member.isdir():
                raise ValueError("release contains a link or special file")
            total += member.size
            if total > 256 * 1024 * 1024:
                raise ValueError("release expansion limit exceeded")
            if name in wanted and (not member.isfile() or member.size == 0):
                raise ValueError("required artifact is empty or not a file")
            if name == f"{root}/mywhisper" and not member.mode & 0o111:
                raise ValueError("binary is not executable")
            seen.add(name)
    if root is None:
        raise ValueError("release is missing required artifacts")
    if {f"{root}/{entry}" for entry in required_names} - seen:
        raise ValueError("release is missing required artifacts")


if __name__ == "__main__":
    try:
        validate(sys.argv[1], sys.argv[2])
    except (ValueError, OSError, tarfile.TarError, IndexError) as error:
        sys.exit(f"Invalid release artifact: {error}")
