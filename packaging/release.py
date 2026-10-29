#!/usr/bin/env python3
"""Prepare a reviewable release candidate; publishing has a separate license gate."""
import argparse
import datetime
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
    sections = changelog_sections(changelog)
    notes = [body.strip() for heading, body in sections if heading == candidate]
    if len(notes) != 1 or not notes[0]:
        raise ValueError("release needs one nonempty matching changelog section")
    # Only the release being prepared must use the current format. Historical
    # headings remain readable so old notes still delimit the current section.
    _, start, end = next(heading for heading in changelog_headings(changelog) if heading[0] == candidate)
    heading = re.fullmatch(
        rf"## \[{re.escape(candidate)}\]\(https://github.com/[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+/releases/tag/v{re.escape(candidate)}\) - (\d{{4}}-\d{{2}}-\d{{2}})",
        changelog[start:end].rstrip("\n"))
    if heading is None:
        raise ValueError("changelog heading must be ## [VERSION](https://github.com/OWNER/REPO/releases/tag/vVERSION) - YYYY-MM-DD")
    try:
        datetime.date.fromisoformat(heading[1])
    except ValueError as error:
        raise ValueError("changelog heading needs a valid calendar date") from error
    return {"tag": tag, "version": candidate, "prerelease": prerelease,
            "notes": notes[0] + "\n"}


def changelog_headings(text):
    """Recognize version headings; prose, lists and nested notes stay free-form."""
    headings = []
    for match in re.finditer(r"^##[ \t]+([^\n]+)\n?", text, re.MULTILINE):
        heading = re.fullmatch(
            r"(?:\[([^]]+)\](?:\([^\n]*\))?|([^ \t]+))(?:[ \t]+-[ \t]+\d{4}-\d{2}-\d{2})?[ \t]*",
            match[1])
        if heading:
            candidate = heading[1] or heading[2]
            if VERSION.fullmatch(candidate):
                headings.append((candidate, match.start(), match.end()))
    return headings


def changelog_sections(text):
    headings = changelog_headings(text)
    return [(candidate, text[end:headings[index + 1][1] if index + 1 < len(headings) else len(text)])
            for index, (candidate, _, end) in enumerate(headings)]


def verify_notes_source(root, candidate):
    """Reject product changes added after the reviewed release-note snapshot."""
    path = root / ".github/release-source.json"
    if path.is_symlink() or not path.is_file() or path.stat().st_size > 16 * 1024:
        raise ValueError("prepare a release PR with source-tracked notes before tagging")
    record = json.loads(path.read_text())
    if not isinstance(record, dict) or record.get("schema") != 1 or record.get("version") != candidate:
        raise ValueError("release-note source does not match the package version")
    for key in ("source_sha", "previous_sha"):
        if not isinstance(record.get(key), str) or not re.fullmatch(r"[0-9a-f]{40}", record[key]):
            raise ValueError("invalid release-note source commit")
    previous = record.get("previous_tag", "")
    if not isinstance(previous, str) or not previous.startswith("v"):
        raise ValueError("invalid release-note baseline tag")
    version(previous[1:])

    def git(*args):
        return subprocess.check_output(["git", *args], cwd=root, text=True, timeout=30).strip()

    source = record["source_sha"]
    if git("rev-parse", "refs/tags/" + previous + "^{commit}") != record["previous_sha"]:
        raise ValueError("previous release tag moved after notes were prepared")
    git("merge-base", "--is-ancestor", record["previous_sha"], source)
    git("merge-base", "--is-ancestor", source, "HEAD")
    allowed = {"Cargo.toml", "Cargo.lock", "packaging/arch/PKGBUILD", "CHANGELOG.md", ".github/release-source.json"}
    changed = set(git("diff", "--name-only", source, "HEAD").splitlines())
    if changed - allowed:
        raise ValueError("product changes landed after release notes were prepared; refresh notes and their source snapshot")
    base_manifest = tomllib.loads(git("show", source + ":Cargo.toml"))
    current_manifest = tomllib.loads((root / "Cargo.toml").read_text())
    base_version = base_manifest["package"].pop("version")
    current_version = current_manifest["package"].pop("version")
    base_lock = tomllib.loads(git("show", source + ":Cargo.lock"))
    current_lock = tomllib.loads((root / "Cargo.lock").read_text())
    for lock, root_version in ((base_lock, base_version), (current_lock, current_version)):
        roots = [package for package in lock["package"] if package["name"] == "mywhisper"
                 and not package.get("source") and package["version"] == root_version]
        if len(roots) != 1:
            raise ValueError("expected one locked root package in release source")
        roots[0].pop("version")
    base_arch = git("show", source + ":packaging/arch/PKGBUILD")
    current_arch = (root / "packaging/arch/PKGBUILD").read_text().strip()
    normalize = lambda text: re.sub(r"^(pkgver|pkgrel)=.*$", r"\1=<release>", text, flags=re.MULTILINE)
    if base_manifest != current_manifest or base_lock != current_lock or normalize(base_arch) != normalize(current_arch):
        raise ValueError("package/dependency changes landed after release notes were prepared; refresh the source snapshot")


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
    # The archive digest is known only after building. Generate the final Arch
    # recipe alongside it, without editing the immutable checked source tree.
    recipe = (root / "packaging/arch/PKGBUILD").read_text()
    recipe, count = re.subn(r"^sha256sums=.*$", f"sha256sums=('{digest}')", recipe, flags=re.MULTILINE)
    if count != 1:
        raise ValueError("Arch recipe needs exactly one checksum field")
    recipe = re.sub(r"^# Published .* asset digest, verified through authenticated GitHub metadata\.$",
                    "# Checked release artifact digest; generated with this release.", recipe, flags=re.MULTILINE)
    recipe = recipe.replace(
        "# update.sh replaces this historical digest with the selected release's verified\n"
        "# digest in its temporary recipe. Direct makepkg needs that release's digest too.\n",
        "# Use with the archive attached to this release; its checksum is verified below.\n")
    (output / "PKGBUILD").write_text(recipe)
    recipe_digest = hashlib.sha256(recipe.encode()).hexdigest()
    metadata["arch_pkgbuild_sha256"] = recipe_digest
    (output / "SHA256SUMS").write_text(f"{digest}  {archive.name}\n{recipe_digest}  PKGBUILD\n")
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
    parser.add_argument("--verify-notes-source", action="store_true")
    parser.add_argument("--compare-version-to", help="CI base commit; new package versions require current source-tracked notes")
    parser.add_argument("--notices-output", type=Path)
    args = parser.parse_args()
    if args.notices_output:
        if args.tag or args.output or args.verify_tag or args.require_license or args.verify_notes_source or args.compare_version_to:
            parser.error("--notices-output is separate from release preparation")
        inventory = write_notices(args.root, args.notices_output)
        print(json.dumps({"dependency_count": len(inventory), "dependency_notices_complete": True}))
        return
    if not args.tag:
        parser.error("a release tag is required")
    metadata = preflight(args.root, args.tag)
    changed_version = False
    changed_notes_source = False
    if args.compare_version_to:
        if not re.fullmatch(r"[0-9a-f]{40}", args.compare_version_to):
            raise ValueError("invalid CI base commit")
        prior = subprocess.check_output(["git", "show", args.compare_version_to + ":Cargo.toml"],
                                        cwd=args.root, text=True, timeout=5)
        changed_version = tomllib.loads(prior)["package"]["version"] != metadata["version"]
        changed_notes_source = bool(subprocess.check_output(
            ["git", "diff", "--name-only", args.compare_version_to, "--", ".github/release-source.json"],
            cwd=args.root, timeout=5))
    if args.verify_tag or args.verify_notes_source or changed_version or changed_notes_source:
        verify_notes_source(args.root, metadata["version"])
    if args.verify_tag:
        target = subprocess.check_output(["git", "rev-parse", f"refs/tags/{args.tag}^{{commit}}"],
                                         cwd=args.root, text=True, timeout=5).strip()
        head = subprocess.check_output(["git", "rev-parse", "HEAD"], cwd=args.root,
                                       text=True, timeout=5).strip()
        if target != head:
            raise ValueError("checked-out source does not match the release tag")
        subprocess.run(["git", "merge-base", "--is-ancestor", head, "refs/remotes/origin/main"],
                       cwd=args.root, check=True, timeout=5)
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
