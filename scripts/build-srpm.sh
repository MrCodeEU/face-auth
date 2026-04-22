#!/usr/bin/env bash
# Build a self-contained SRPM for COPR.
# Bundles: source tree + cargo vendor/ + ORT 1.24.2 native lib
# No network needed during RPM build.
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$REPO_ROOT"

VERSION="${1:-$(git describe --tags --abbrev=0 | sed 's/^v//')}"
PKGNAME="face-auth-${VERSION}"
TARBALL="${PKGNAME}.tar.gz"
WORK_DIR="$(mktemp -d)"
trap 'rm -rf "$WORK_DIR"' EXIT

echo "==> Building SRPM for face-auth ${VERSION}"

# --- 1. Vendor Rust dependencies ---
echo "==> Vendoring cargo dependencies..."
cargo vendor --locked "${WORK_DIR}/vendor" > /dev/null

# --- 2. Download ORT 1.24.2 native lib ---
ORT_VERSION="1.24.2"
ORT_URL="https://cdn.pyke.io/0/pyke:ort-rs/ms@${ORT_VERSION}/x86_64-unknown-linux-gnu.tar.lzma2"
ORT_DIR="${WORK_DIR}/ort"
mkdir -p "$ORT_DIR"

echo "==> Downloading ORT ${ORT_VERSION}..."
curl -fsSL -o "${WORK_DIR}/ort.tar.lzma2" "$ORT_URL"

# Extract .so from the ORT tarball (lzma2 = xz format)
tar -xJf "${WORK_DIR}/ort.tar.lzma2" -C "$ORT_DIR" --wildcards '*.so*' --strip-components=1 2>/dev/null || \
    xz -d < "${WORK_DIR}/ort.tar.lzma2" | tar -x -C "$ORT_DIR" --wildcards '*.so*' --strip-components=1
echo "==> ORT libs: $(ls "$ORT_DIR")"

# --- 3. Build source tarball ---
echo "==> Creating source tarball..."
SRC_DIR="${WORK_DIR}/${PKGNAME}"
git archive --format=tar --prefix="${PKGNAME}/" HEAD | tar -x -C "$WORK_DIR"

# Copy vendored deps and ORT into the source tree
cp -r "${WORK_DIR}/vendor" "${SRC_DIR}/vendor"
cp -r "$ORT_DIR"           "${SRC_DIR}/ort"

tar czf "$TARBALL" -C "$WORK_DIR" "$PKGNAME"
echo "==> Tarball: ${TARBALL} ($(du -sh "$TARBALL" | cut -f1))"

# --- 4. Build SRPM ---
echo "==> Building SRPM..."
mkdir -p ~/rpmbuild/{SOURCES,SPECS}
cp "$TARBALL" ~/rpmbuild/SOURCES/
cp packaging/face-auth.spec ~/rpmbuild/SPECS/

# Stamp version into spec
sed -i "s/^Version:.*/Version:        ${VERSION}/" ~/rpmbuild/SPECS/face-auth.spec

rpmbuild -bs ~/rpmbuild/SPECS/face-auth.spec \
    --define "_sourcedir ${PWD}" \
    --define "_srcrpmdir ${PWD}"

echo "==> Done: face-auth-${VERSION}-*.src.rpm"
