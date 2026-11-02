#!/usr/bin/env bash
# Build TernaryGrid for Linux (x86_64 or aarch64, the machine's) into dist/:
#
#   TernaryGrid-<version>-linux-<arch>.tar.gz          bin/ternarygrid, its desktop entry and icons (any distribution)
#   TernaryGrid-<version>-linux-<arch>.AppImage        one file that runs on most distributions
#   TernaryGrid-<version>-linux-<deb arch>.deb         Debian, Ubuntu, Linux Mint, Pop!_OS, elementary …
#   TernaryGrid-<version>-linux-<arch>.rpm             Fedora, RHEL / Rocky / Alma, openSUSE …
#   TernaryGrid-<version>-linux-<arch>.pkg.tar.zst     Arch Linux, Manjaro, EndeavourOS …
#
# Usage:
#   scripts/bundle-linux.sh             # builds the release binary first
#   scripts/bundle-linux.sh --no-build  # packages target/release/ternarygrid
#
# The packages are made with nfpm and the AppImage with appimagetool, both
# downloaded into target/ at pinned versions when missing. The binary links
# only to glibc, at the version of the system it is built on: CI builds in
# AlmaLinux 9 (glibc 2.34), so it runs there and on everything newer. The
# window system and OpenGL libraries are loaded when it starts, so the
# packages depend on them.
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"

NFPM_VERSION=2.47.0
APPIMAGETOOL_VERSION=1.9.1

BUILD=1
while [[ $# -gt 0 ]]; do
  case "$1" in
    --no-build) BUILD=0; shift ;;
    *) echo "unknown argument: $1" >&2; exit 2 ;;
  esac
done

VERSION="$(grep -m1 '^version' Cargo.toml | sed -E 's/version = "(.*)"/\1/')"
ARCH="$(uname -m)"
case "$ARCH" in
  x86_64) NFPM_ARCH=amd64; DEB_ARCH=amd64 ;;
  aarch64) NFPM_ARCH=arm64; DEB_ARCH=arm64 ;;
  *) echo "unsupported architecture: $ARCH" >&2; exit 1 ;;
esac
DIST="$ROOT/dist"
TOOLS="$ROOT/target/packaging-tools"
STAGE="$ROOT/target/linux-stage"
NAME="TernaryGrid-$VERSION-linux"
mkdir -p "$DIST" "$TOOLS"

if [[ $BUILD -eq 1 ]]; then
  echo "==> Building TernaryGrid $VERSION (release, $ARCH)"
  cargo build --release --locked -p ternarygrid-app
fi
BIN="$ROOT/target/release/ternarygrid"
[[ -x "$BIN" ]] || { echo "missing $BIN" >&2; exit 1; }
SAID="$("$BIN" --version)"
[[ "$SAID" == "ternarygrid $VERSION" ]] || { echo "$BIN --version said '$SAID', expected $VERSION" >&2; exit 1; }
GLIBC="$(objdump -T "$BIN" | grep -o 'GLIBC_[0-9.]*' | sort -uV | tail -1)"
echo "==> $SAID, $(du -h "$BIN" | cut -f1), needs $GLIBC"

# The files every format installs, under /usr.
rm -rf "$STAGE"
install -Dm755 "$BIN" "$STAGE/usr/bin/ternarygrid"
install -Dm644 packaging/linux/dev.ternarygrid.TernaryGrid.desktop "$STAGE/usr/share/applications/dev.ternarygrid.TernaryGrid.desktop"
install -Dm644 crates/ternarygrid-app/assets/icon-256.png "$STAGE/usr/share/icons/hicolor/256x256/apps/dev.ternarygrid.TernaryGrid.png"
install -Dm644 crates/ternarygrid-app/assets/icon.svg "$STAGE/usr/share/icons/hicolor/scalable/apps/dev.ternarygrid.TernaryGrid.svg"

echo "==> $NAME-$ARCH.tar.gz"
rm -rf "$STAGE/tar" && mkdir -p "$STAGE/tar/$NAME-$ARCH"
cp -R "$STAGE/usr/." "$STAGE/tar/$NAME-$ARCH/"
tar -C "$STAGE/tar" -czf "$DIST/$NAME-$ARCH.tar.gz" "$NAME-$ARCH"

echo "==> $NAME-$ARCH.AppImage"
APPIMAGETOOL="$TOOLS/appimagetool-$APPIMAGETOOL_VERSION-$ARCH.AppImage"
if [[ ! -x "$APPIMAGETOOL" ]]; then
  curl -fsSL -o "$APPIMAGETOOL" "https://github.com/AppImage/appimagetool/releases/download/$APPIMAGETOOL_VERSION/appimagetool-$ARCH.AppImage"
  chmod +x "$APPIMAGETOOL"
fi
APPDIR="$STAGE/TernaryGrid.AppDir"
mkdir -p "$APPDIR"
cp -R "$STAGE/usr" "$APPDIR/usr"
cp packaging/linux/dev.ternarygrid.TernaryGrid.desktop "$APPDIR/"
cp crates/ternarygrid-app/assets/icon-256.png "$APPDIR/dev.ternarygrid.TernaryGrid.png"
ln -s usr/bin/ternarygrid "$APPDIR/AppRun"
# No FUSE needed (CI runners and containers have none).
APPIMAGE_EXTRACT_AND_RUN=1 ARCH="$ARCH" "$APPIMAGETOOL" --no-appstream "$APPDIR" "$DIST/$NAME-$ARCH.AppImage" >/dev/null

NFPM="$TOOLS/nfpm-$NFPM_VERSION"
if [[ ! -x "$NFPM" ]]; then
  case "$ARCH" in x86_64) NFPM_TAR="nfpm_${NFPM_VERSION}_Linux_x86_64.tar.gz" ;; aarch64) NFPM_TAR="nfpm_${NFPM_VERSION}_Linux_arm64.tar.gz" ;; esac
  BASE="https://github.com/goreleaser/nfpm/releases/download/v$NFPM_VERSION"
  (cd "$TOOLS" && curl -fsSLO "$BASE/$NFPM_TAR" && curl -fsSL -o "nfpm-$NFPM_VERSION-checksums.txt" "$BASE/checksums.txt" \
    && grep " $NFPM_TAR\$" "nfpm-$NFPM_VERSION-checksums.txt" | sha256sum --check --quiet \
    && tar -xzf "$NFPM_TAR" nfpm && mv nfpm "nfpm-$NFPM_VERSION" && rm "$NFPM_TAR")
fi
cat >"$STAGE/nfpm.yaml" <<EOF
name: ternarygrid
arch: $NFPM_ARCH
platform: linux
version: $VERSION
release: 1
section: database
priority: optional
maintainer: Tommy Lumos Empire
vendor: TernaryGrid
homepage: https://github.com/ilhamnadhif/ternarygrid
license: MIT OR Apache-2.0
description: |-
  A lightweight SQL database client.
  PostgreSQL, CockroachDB, MySQL, MariaDB, SQLite, SQL Server and ClickHouse
  in one small native program: SQL editor with autocomplete, data grid,
  export / import, ER diagrams, compare and more.
contents:
  - src: $STAGE/usr/bin/ternarygrid
    dst: /usr/bin/ternarygrid
    file_info: { mode: 0755 }
  - src: $STAGE/usr/share/applications/dev.ternarygrid.TernaryGrid.desktop
    dst: /usr/share/applications/dev.ternarygrid.TernaryGrid.desktop
  - src: $STAGE/usr/share/icons/hicolor/256x256/apps/dev.ternarygrid.TernaryGrid.png
    dst: /usr/share/icons/hicolor/256x256/apps/dev.ternarygrid.TernaryGrid.png
  - src: $STAGE/usr/share/icons/hicolor/scalable/apps/dev.ternarygrid.TernaryGrid.svg
    dst: /usr/share/icons/hicolor/scalable/apps/dev.ternarygrid.TernaryGrid.svg
overrides:
  deb:
    depends:
      - libc6 (>= 2.34)
      - libgcc-s1
      - libegl1
      - libgl1
      - libxkbcommon0
      - libxkbcommon-x11-0
      - libx11-6
      - libxcursor1
      - libxrandr2
      - libxi6
      - libwayland-client0
      - libwayland-cursor0
      - libwayland-egl1
    recommends:
      - xdg-desktop-portal
      - gnome-keyring | kwalletmanager
  rpm:
    depends:
      - glibc >= 2.34
      - libEGL.so.1()(64bit)
      - libGL.so.1()(64bit)
      - libxkbcommon.so.0()(64bit)
      - libxkbcommon-x11.so.0()(64bit)
      - libX11.so.6()(64bit)
      - libXcursor.so.1()(64bit)
      - libXrandr.so.2()(64bit)
      - libXi.so.6()(64bit)
      - libwayland-client.so.0()(64bit)
      - libwayland-cursor.so.0()(64bit)
      - libwayland-egl.so.1()(64bit)
    recommends:
      - xdg-desktop-portal
  archlinux:
    depends:
      - glibc
      - gcc-libs
      - libglvnd
      - libxkbcommon
      - libxkbcommon-x11
      - libx11
      - libxcursor
      - libxrandr
      - libxi
      - wayland
EOF
for p in deb rpm archlinux; do
  case "$p" in
    deb) OUT="$NAME-$DEB_ARCH.deb" ;;
    rpm) OUT="$NAME-$ARCH.rpm" ;;
    archlinux) OUT="$NAME-$ARCH.pkg.tar.zst" ;;
  esac
  echo "==> $OUT"
  "$NFPM" package --config "$STAGE/nfpm.yaml" --packager "$p" --target "$DIST/$OUT" >/dev/null
done

echo "==> Done"
ls -lh "$DIST"/"$NAME"-* | awk '{print "    " $NF "  " $5}'
