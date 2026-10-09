#!/usr/bin/env bash
# Update MyWhisper from the private GitHub Release and install it.
# Usage: ./update.sh [version]   (default: latest release)
# Needs: gh (authenticated), makepkg, sudo.
set -euo pipefail
cd "$(dirname "$0")"

VERSION="${1:-$(gh release list --repo aaronroquefonseca/MyWhisper --limit 1 --json tagName -q '.[0].tagName')}"
VERSION="${VERSION#v}"
TARBALL="mywhisper-v${VERSION}-x86_64-unknown-linux-gnu.tar.gz"

echo "==> downloading $TARBALL"
gh release download "v${VERSION}" --repo aaronroquefonseca/MyWhisper --pattern "$TARBALL" --clobber

echo "==> bumping PKGBUILD to $VERSION"
sed -i "s/^pkgver=.*/pkgver=${VERSION}/; s/^pkgrel=.*/pkgrel=1/" PKGBUILD

echo "==> building + installing"
makepkg -si --noconfirm

echo "==> enabling user service"
systemctl --user daemon-reload
systemctl --user enable --now mywhisper.service
echo "installed $VERSION"
