#!/usr/bin/env bash
# Install a verified stable private release; always restart the upgraded engine.
# Needs authenticated gh, Python 3, makepkg and sudo. Never source release content.
set -euo pipefail
script_dir=$(cd -- "$(dirname -- "$0")" && pwd)
repo=aaronroquefonseca/MyWhisper
if (( $# > 1 )); then
  echo 'Usage: update.sh [stable version]' >&2
  exit 2
fi
if (( $# == 1 )); then
  version=${1#v}
else
  releases=$(gh release list --repo "$repo" --limit 100 --exclude-drafts --exclude-pre-releases --json tagName,isDraft,isPrerelease)
  version=$(python3 -c 'import json,re,sys
candidates=[]
for release in json.loads(sys.argv[1]):
    tag=release.get("tagName", "")
    if release.get("isDraft") is not False or release.get("isPrerelease") is not False: continue
    if re.fullmatch(r"v(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)", tag):
        candidates.append((tuple(map(int,tag[1:].split("."))),tag[1:]))
if not candidates: sys.exit("No stable release found")
print(max(candidates)[1])' "$releases")
fi
if [[ ! $version =~ ^(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)$ ]]; then
  echo 'The Arch installer accepts stable versions in major.minor.patch form.' >&2
  exit 2
fi
tarball="mywhisper-v${version}-x86_64-unknown-linux-gnu.tar.gz"
metadata=$(gh release view "v${version}" --repo "$repo" --json isDraft,isPrerelease,assets)
digest=$(python3 -c 'import json,re,sys
release=json.loads(sys.argv[1])
if release.get("isDraft") is not False or release.get("isPrerelease") is not False: sys.exit("Release is not stable")
assets=[a for a in release.get("assets",[]) if a.get("name")==sys.argv[2]]
if len(assets)!=1: sys.exit("Release asset is missing or ambiguous")
digest=assets[0].get("digest", "")
if not re.fullmatch(r"sha256:[0-9a-f]{64}",digest): sys.exit("Release has no authoritative SHA-256 digest")
print(digest[7:])' "$metadata" "$tarball")
work=$(mktemp -d /tmp/mywhisper-upgrade.XXXXXXXX)
trap 'rm -rf -- "$work"' EXIT
chmod 700 "$work"
gh release download "v${version}" --repo "$repo" --pattern "$tarball" --dir "$work"
actual=$(sha256sum -- "$work/$tarball")
if [[ ${actual%% *} != "$digest" ]]; then
  echo 'Release artifact integrity check failed; nothing installed.' >&2
  exit 1
fi
python3 "$script_dir/../validate-artifact.py" "$work/$tarball" "$version"
sed -e "s/^pkgver=.*/pkgver=${version}/" -e 's/^pkgrel=.*/pkgrel=1/' \
  -e "s/^sha256sums=.*/sha256sums=('${digest}')/" "$script_dir/PKGBUILD" > "$work/PKGBUILD"
(cd -- "$work" && makepkg -si --noconfirm)
systemctl --user daemon-reload
systemctl --user enable mywhisper.service
systemctl --user restart mywhisper.service
printf 'Installed and restarted MyWhisper %s\n' "$version"
