#!/usr/bin/env bash
# Bump packaging/aur for a tagged CLI release (no makepkg required — CI-friendly).
# Usage: ./packaging/aur/bump.sh cli/v0.1.1-beta
#        ./packaging/aur/bump.sh v0.1.1-beta
#        ./packaging/aur/bump.sh 0.1.1-beta
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
PKGBUILD="$ROOT/packaging/aur/PKGBUILD"
SRCINFO="$ROOT/packaging/aur/.SRCINFO"
REPO_URL='https://github.com/horizzon3507/usagenometer'

RAW="${1:?usage: bump.sh <cli/vX.Y.Z-channel | vX.Y.Z-channel | X.Y.Z-channel>}"

# Accept cli/v…, v…, or bare version
case "$RAW" in
  cli/v*) VER="${RAW#cli/v}" ;;
  cli/*)  VER="${RAW#cli/}" ; VER="${VER#v}" ;;
  v*)     VER="${RAW#v}" ;;
  *)      VER="$RAW" ;;
esac
SURFACE=cli
PKGVER="${VER//-/_}"
TARBALL_URL="${REPO_URL}/archive/refs/tags/${SURFACE}/v${VER}.tar.gz"

echo "==> waiting for $TARBALL_URL"
for _ in $(seq 1 12); do
  if curl -fsI "$TARBALL_URL" >/dev/null 2>&1; then
    break
  fi
  sleep 5
done

echo "==> hashing tarball"
SHA="$(curl -fsSL "$TARBALL_URL" | sha256sum | awk '{print $1}')"
echo "    sha256=$SHA"

echo "==> updating PKGBUILD → ${SURFACE}/v${VER} (pkgver=${PKGVER})"
sed -i "s/^_surface=.*/_surface=${SURFACE}/" "$PKGBUILD"
sed -i "s/^_pkgver=.*/_pkgver=${VER}/" "$PKGBUILD"
# pkgver line is derived via ${...}; keep the expression
if ! grep -q '^pkgver=\${_pkgver' "$PKGBUILD"; then
  sed -i "s/^pkgver=.*/pkgver=\${_pkgver\/\/-\/_}/" "$PKGBUILD"
fi
sed -i "s/^pkgrel=.*/pkgrel=1/" "$PKGBUILD"
sed -i "s/^sha256sums=.*/sha256sums=('${SHA}')/" "$PKGBUILD"

echo "==> writing .SRCINFO"
cat > "$SRCINFO" <<EOF
pkgbase = usagenometer
	pkgdesc = AI usage meters in the terminal (Codex, Cursor, Antigravity, Claude, Grok)
	pkgver = ${PKGVER}
	pkgrel = 1
	url = ${REPO_URL}
	arch = x86_64
	arch = aarch64
	license = Apache-2.0
	makedepends = cargo
	depends = gcc-libs
	depends = glibc
	optdepends = libnotify: desktop notifications for usg --notify / --alert
	options = !lto
	source = usagenometer-${PKGVER}.tar.gz::${REPO_URL}/archive/refs/tags/${SURFACE}/v${VER}.tar.gz
	sha256sums = ${SHA}

pkgname = usagenometer
EOF

echo "==> done (packaging/aur ready for AUR push)"
