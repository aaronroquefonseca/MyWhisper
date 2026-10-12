#!/usr/bin/env python3
"""Prepare a reviewable release candidate; publishing has a separate license gate."""
import argparse
import hashlib
import json
from pathlib import Path
import re
import shutil
import subprocess
import tarfile
import tomllib

VERSION = re.compile(r"(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)(?:-([0-9A-Za-z-]+(?:\.[0-9A-Za-z-]+)*))?")


def version(value):
    if len(value) > 128:
        raise ValueError("release version exceeds its bound")
    match = VERSION.fullmatch(value)
    if not match or (match[4] and any(part.isdigit() and len(part) > 1 and part[0] == "0"
                                    for part in match[4].split("."))):
        raise ValueError("release requires a canonical semantic version without build metadata")
    return bool(match[4])


def preflight(root, tag):
    if not tag.startswith("v"):
        raise ValueError("release tag must start with v")
    candidate = tag[1:]
    prerelease = version(candidate)
    manifest = tomllib.loads((root / "Cargo.toml").read_text())
    if candidate != manifest["package"]["version"]:
        raise ValueError("tag and Cargo package versions differ")
    pkgbuild = (root / "packaging/arch/PKGBUILD").read_text()
    package_versions = re.findall(r"^pkgver=([0-9A-Za-z._]+)$", pkgbuild, re.MULTILINE)
    # Arch pkgver cannot contain hyphens; prereleases remain explicitly excluded
    # from its stable installer, but their artifact identity is still checked.
    if package_versions != [candidate.replace("-", "")]:
        raise ValueError("Arch package version differs from the release")
    flake = (root / "packaging/nixos/flake.nix").read_text()
    if "version = manifest.package.version;" not in flake:
        raise ValueError("Nix package must derive its version from Cargo.toml")
    changelog = (root / "CHANGELOG.md").read_text()
    sections = re.findall(r"^## ([^\n]+)\n(.*?)(?=^## |\Z)", changelog, re.MULTILINE | re.DOTALL)
    notes = [body.strip() for heading, body in sections if heading == candidate]
    if len(notes) != 1 or not notes[0] or not any(line.startswith("- ") for line in notes[0].splitlines()):
        raise ValueError("release needs one nonempty matching changelog section")
    return {"tag": tag, "version": candidate, "prerelease": prerelease,
            "notes": notes[0] + "\n"}


def local_patch_provenance(package, directory):
    """Validate the complete reviewed local source, including upstream notices."""
    path = directory / "UPSTREAM.json"
    if path.is_symlink() or not path.is_file() or path.stat().st_size > 512 * 1024:
        raise ValueError(f"local dependency needs bounded patch provenance: {package['name']}")
    source = json.loads(path.read_text())
    vcs = json.loads((directory / ".cargo_vcs_info.json").read_text())
    if source["crate"] != package["name"] or source["version"] != package["version"] \
            or source["source_commit"] != vcs["git"]["sha1"] \
            or not re.fullmatch(r"[0-9a-f]{64}", source["crate_checksum"]):
        raise ValueError(f"local patch provenance differs from dependency: {package['name']}")
    expected = source["local_files"]
    upstream = source["upstream_files"]
    modified = set(source["modified_files"])
    if not expected or len(expected) > 8192 or not modified <= upstream.keys() \
            or not upstream.keys() <= expected.keys():
        raise ValueError("local patch file inventory is invalid")
    actual = set()
    for index, file in enumerate(directory.rglob("*")):
        if index >= 8192 or file.is_symlink():
            raise ValueError("local patch contains unreviewed links or too many files")
        if not file.is_file() or file == path:
            continue
        name = file.relative_to(directory).as_posix()
        if file.stat().st_size > 8 * 1024 * 1024:
            raise ValueError("local patch file exceeds its bound")
        digest = hashlib.sha256(file.read_bytes()).hexdigest()
        if expected.get(name) != digest:
            raise ValueError(f"local patch digest differs: {package['name']}/{name}")
        if name in upstream and name not in modified and upstream[name] != digest:
            raise ValueError(f"unlisted upstream modification: {package['name']}/{name}")
        actual.add(name)
    if actual != expected.keys():
        raise ValueError("local patch is missing reviewed files")
    return source


def dependency_notices(root):
    result = subprocess.run(["cargo", "metadata", "--locked", "--offline", "--format-version", "1",
                             "--filter-platform", "x86_64-unknown-linux-gnu"], cwd=root,
                            check=True, capture_output=True, timeout=30)
    if len(result.stdout) > 16 * 1024 * 1024:
        raise ValueError("dependency metadata exceeds its bound")
    graph = json.loads(result.stdout)
    nodes = {node["id"]: node for node in graph["resolve"]["nodes"]}
    reachable = set()
    pending = [graph["resolve"]["root"]]
    while pending:
        identifier = pending.pop()
        if identifier in reachable:
            continue
        reachable.add(identifier)
        pending.extend(dependency["pkg"] for dependency in nodes[identifier]["deps"])
    packages = [package for package in graph["packages"] if package["id"] in reachable]
    if len(packages) > 2048:
        raise ValueError("too many dependencies to review")
    notices = [(root / "packaging/notices/OpenWhispr.txt").read_text()]
    source_file = root / "packaging/notices/sources.json"
    sources = json.loads(source_file.read_text()) if source_file.is_file() else {}
    lock = tomllib.loads((root / "Cargo.lock").read_text())
    checksums = {(item["name"], item["version"]): item.get("checksum") for item in lock["package"]}
    inventory = []
    for package in sorted(packages, key=lambda item: (item["name"], item["version"])):
        if package["name"] == "mywhisper" and package["source"] is None:
            continue
        directory = Path(package["manifest_path"]).parent.resolve()
        patch_provenance = local_patch_provenance(package, directory) if package["source"] is None else None
        files = []
        for index, path in enumerate(directory.rglob("*")):
            if index >= 8192:
                raise ValueError(f"dependency file count exceeds its bound: {package['name']}")
            if path.is_symlink() or not path.is_file():
                continue
            if not path.name.lower().startswith(("licen", "copying", "notice", "copyright")):
                continue
            if path.stat().st_size > 512 * 1024:
                raise ValueError(f"dependency notice exceeds its bound: {package['name']}")
            try:
                content = path.read_text()
            except UnicodeDecodeError:
                continue
            if content.strip():
                relative = str(path.relative_to(directory))
                files.append(relative)
                notices.append(f"\n--- {package['name']} {package['version']}: {relative} ---\n{content}")
        provenance = None
        source = sources.get(f"{package['name']}@{package['version']}")
        if not files and source:
            vcs = json.loads((directory / ".cargo_vcs_info.json").read_text())
            if source["crate_checksum"] != checksums.get((package["name"], package["version"])) \
                    or source["source_commit"] != vcs["git"]["sha1"]:
                raise ValueError(f"notice provenance does not match locked crate: {package['name']}")
            for notice in source["files"]:
                if Path(notice["path"]).name != notice["path"]:
                    raise ValueError("upstream notice must have a plain filename")
                path = root / "packaging/notices" / notice["path"]
                if path.is_symlink() or path.stat().st_size > 512 * 1024:
                    raise ValueError("invalid upstream notice file")
                content = path.read_bytes()
                if hashlib.sha256(content).hexdigest() != notice["sha256"]:
                    raise ValueError(f"upstream notice digest differs: {package['name']}")
                files.append("upstream/" + notice["path"])
                notices.append(f"\n--- {package['name']} {package['version']}: upstream {notice['path']} ---\n"
                               + content.decode("utf-8"))
            provenance = source
        inventory.append({"name": package["name"], "version": package["version"],
                          "license": package["license"], "notice_files": files,
                          "upstream_notice_provenance": provenance,
                          "local_patch_provenance": patch_provenance})
    text = "\n".join(notices)
    if len(text.encode()) > 32 * 1024 * 1024:
        raise ValueError("combined dependency notices exceed their bound")
    return text, inventory


def require_distribution_license(root, inventory):
    license_file = root / "LICENSE"
    if license_file.is_symlink() or not license_file.is_file() or not license_file.read_text().strip():
        raise ValueError("distribution held: the owner has not established a project LICENSE")
    missing = [item["name"] for item in inventory if not item["license"] or not item["notice_files"]]
    if missing:
        raise ValueError("distribution held: dependency licensing/notices need review: " + ", ".join(missing))


def write_notices(root, directory):
    notices, inventory = dependency_notices(root)
    missing = [item["name"] for item in inventory if not item["license"] or not item["notice_files"]]
    if missing:
        raise ValueError("dependency notices incomplete: " + ", ".join(missing))
    directory.mkdir(mode=0o755, parents=True, exist_ok=False)
    (directory / "THIRD_PARTY_NOTICES.txt").write_text(notices)
    (directory / "THIRD_PARTY.json").write_text(json.dumps(inventory, indent=2) + "\n")
    return inventory


def stage(root, output, tag):
    metadata = preflight(root, tag)
    binary = root / "target/release/mywhisper"
    reported = subprocess.run([str(binary), "--version"], check=True, capture_output=True,
                              text=True, timeout=5).stdout.strip()
    if reported != f"mywhisper {metadata['version']}":
        raise ValueError("built binary version differs from the release")
    if (root / ".git").exists():
        metadata["source_sha"] = subprocess.check_output(
            ["git", "rev-parse", "HEAD"], cwd=root, text=True, timeout=5).strip()
        metadata["source_dirty"] = bool(subprocess.check_output(
            ["git", "status", "--porcelain", "--untracked-files=all", "--", ".",
             ":(exclude).release-candidate", ":(exclude)security-audit.json"],
            cwd=root, timeout=5))
    output.mkdir(mode=0o700, parents=True, exist_ok=False)
    directory = output / f"mywhisper-{tag}-x86_64-unknown-linux-gnu"
    directory.mkdir(mode=0o700)
    for source, target in [
        ("target/release/mywhisper", "mywhisper"),
        ("packaging/systemd/mywhisper.service", "mywhisper.service"),
        ("assets/applications/com.mywhisper.App.desktop", "com.mywhisper.App.desktop"),
        ("assets/icons/app-icon.svg", "com.mywhisper.App.svg"),
        ("assets/icons/symbolic.svg", "com.mywhisper.App-symbolic.svg"),
    ]:
        shutil.copyfile(root / source, directory / target)
        (directory / target).chmod(0o755 if target == "mywhisper" else 0o644)
    notices, inventory = dependency_notices(root)
    (directory / "THIRD_PARTY_NOTICES.txt").write_text(notices)
    (directory / "THIRD_PARTY.json").write_text(json.dumps(inventory, indent=2) + "\n")
    project_license = root / "LICENSE"
    if project_license.is_file() and not project_license.is_symlink():
        shutil.copyfile(project_license, directory / "LICENSE")
    metadata["project_license_established"] = (directory / "LICENSE").is_file()
    metadata["dependency_notices_complete"] = all(item["license"] and item["notice_files"] for item in inventory)
    archive = output / f"{directory.name}.tar.gz"
    with tarfile.open(archive, "w:gz") as tar:
        tar.add(directory, arcname=directory.name)
    with archive.open("rb") as file:
        digest = hashlib.file_digest(file, "sha256").hexdigest()
    metadata["asset"] = archive.name
    metadata["sha256"] = digest
    (output / "SHA256SUMS").write_text(f"{digest}  {archive.name}\n")
    (output / "notes.md").write_text(metadata.pop("notes"))
    (output / "release.json").write_text(json.dumps(metadata, indent=2) + "\n")
    return metadata


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("tag", nargs="?")
    parser.add_argument("--root", type=Path, default=Path(__file__).resolve().parent.parent)
    parser.add_argument("--output", type=Path)
    parser.add_argument("--require-license", action="store_true")
    parser.add_argument("--verify-tag", action="store_true")
    parser.add_argument("--notices-output", type=Path)
    args = parser.parse_args()
    if args.notices_output:
        if args.tag or args.output or args.verify_tag or args.require_license:
            parser.error("--notices-output is separate from release preparation")
        inventory = write_notices(args.root, args.notices_output)
        print(json.dumps({"dependency_count": len(inventory), "dependency_notices_complete": True}))
        return
    if not args.tag:
        parser.error("a release tag is required")
    metadata = preflight(args.root, args.tag)
    if args.verify_tag:
        target = subprocess.check_output(["git", "rev-parse", f"refs/tags/{args.tag}^{{commit}}"],
                                         cwd=args.root, text=True, timeout=5).strip()
        head = subprocess.check_output(["git", "rev-parse", "HEAD"], cwd=args.root,
                                       text=True, timeout=5).strip()
        if target != head:
            raise ValueError("checked-out source does not match the release tag")
    if args.require_license:
        require_distribution_license(args.root, [])
        _, inventory = dependency_notices(args.root)
        require_distribution_license(args.root, inventory)
    if args.output:
        metadata = stage(args.root, args.output, args.tag)
    print(json.dumps(metadata))


if __name__ == "__main__":
    try:
        main()
    except (ValueError, OSError, subprocess.SubprocessError) as error:
        raise SystemExit(f"Release gate failed: {error}") from error
