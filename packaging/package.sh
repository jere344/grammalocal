#!/bin/sh
# Builds the release tarball that install.sh unpacks (mode 1) and that the
# curl-piped installer downloads (mode 3).
#
#   dist/grammalocal-x86_64-linux.tar.gz
#   dist/grammalocal-x86_64-linux.tar.gz.sha256
#
# Usage: sh packaging/package.sh
set -eu

ROOT=$(cd "$(dirname "$0")/.." && pwd)
NAME="grammalocal-x86_64-linux"
DIST="$ROOT/dist"
STAGE=$(mktemp -d)
trap 'rm -rf "$STAGE"' EXIT INT TERM

[ "$(uname -m)" = "x86_64" ] || { echo "package.sh: must run on x86_64" >&2; exit 1; }
command -v cargo >/dev/null 2>&1 || { echo "package.sh: cargo is required" >&2; exit 1; }
command -v sha256sum >/dev/null 2>&1 || { echo "package.sh: sha256sum is required" >&2; exit 1; }

cargo build --release --locked --manifest-path "$ROOT/Cargo.toml" \
    -p grammalocald -p grammalocal-cli -p grammalocal-popup

PKG="$STAGE/$NAME"
mkdir -p "$PKG/bin" "$PKG/share" "$PKG/systemd"
for b in grammalocald grammalocal-popup grammalocal; do
    install -m 755 "$ROOT/target/release/$b" "$PKG/bin/$b"
done
install -m 644 "$ROOT/crates/grammalocald/worker/grammalecte_worker.py" "$PKG/share/grammalecte_worker.py"
install -m 644 "$ROOT/packaging/grammalocald.service" "$PKG/systemd/grammalocald.service"
install -m 644 "$ROOT/assets/logo.svg" "$PKG/share/grammalocal.svg"
install -m 644 "$ROOT/packaging/grammalocal.desktop" "$PKG/share/grammalocal.desktop"
install -m 755 "$ROOT/install.sh" "$PKG/install.sh"
cp -R "$ROOT/vendor" "$PKG/vendor"
# Keep only the vendored Grammalecte package and its licences.
find "$PKG/vendor" -name '__pycache__' -type d -prune -exec rm -rf {} +
[ -f "$ROOT/LICENSE" ] && install -m 644 "$ROOT/LICENSE" "$PKG/LICENSE"

mkdir -p "$DIST"
TARBALL="$DIST/$NAME.tar.gz"
# --sort/--owner make the archive reproducible across machines.
# No wrapper directory: install.sh expects bin/, share/, ... at the top level.
tar --sort=name --owner=0 --group=0 --numeric-owner -C "$PKG" -czf "$TARBALL" .
(cd "$DIST" && sha256sum "$NAME.tar.gz" > "$NAME.tar.gz.sha256")

echo "wrote $TARBALL ($(du -h "$TARBALL" | cut -f1))"
echo "wrote $TARBALL.sha256"
