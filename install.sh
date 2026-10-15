#!/bin/sh
# Installs grammalocal for the current user. Does not need root.
#
#   grammalocald, grammalocal-popup, grammalocal -> ~/.local/bin
#   worker and vendored Grammalecte              -> ~/.local/share/grammalocal/
#   systemd user unit                            -> ~/.config/systemd/user/grammalocald.service
#   app icon and desktop entry                   -> ~/.local/share/icons, ~/.local/share/applications
#
# The source is picked automatically:
#   1. an extracted release tarball (bin/grammalocald next to this script)
#   2. a source checkout (Cargo.toml next to this script; needs Rust and GTK4 dev files)
#   3. piped from curl: downloads the release tarball and checks its sha256
#
# Any arguments are passed to `grammalocal setup`, e.g. `sh install.sh --yes`.
# Set GRAMMALOCAL_INSTALL_SKIP_SETUP=1 to install files only.
# Set GRAMMALOCAL_RELEASE_URL to override where mode 3 downloads from.
set -eu

RELEASE_URL="${GRAMMALOCAL_RELEASE_URL:-https://github.com/jere344/grammalocal/releases/latest/download}"
TARBALL="grammalocal-x86_64-linux.tar.gz"

BIN_DIR="$HOME/.local/bin"
SHARE_DIR="$HOME/.local/share/grammalocal"
UNIT_DIR="${XDG_CONFIG_HOME:-$HOME/.config}/systemd/user"
ICON_DIR="${XDG_DATA_HOME:-$HOME/.local/share}/icons/hicolor/scalable/apps"
APP_DIR="${XDG_DATA_HOME:-$HOME/.local/share}/applications"

say() { printf '%s\n' "$*"; }
die() { printf 'install.sh: %s\n' "$*" >&2; exit 1; }

need() {
    command -v "$1" >/dev/null 2>&1 || die "$1 is required but was not found"
}

# $0 is "sh" when piped from curl, so only trust it when it names a real file.
SELF_DIR=""
if [ -f "$0" ]; then
    SELF_DIR=$(cd "$(dirname "$0")" && pwd)
fi

[ "$(uname -s)" = "Linux" ] || die "grammalocal only supports Linux"

TMP=""
cleanup() {
    if [ -n "$TMP" ]; then
        rm -rf "$TMP"
    fi
}
trap cleanup EXIT INT TERM

# install_files BIN_DIR_SRC WORKER_SRC VENDOR_SRC UNIT_SRC ICON_SRC DESKTOP_SRC
install_files() {
    src_bin=$1
    src_worker=$2
    src_vendor=$3
    src_unit=$4
    src_icon=$5
    src_desktop=$6

    install -d "$BIN_DIR" "$SHARE_DIR" "$UNIT_DIR" "$ICON_DIR" "$APP_DIR"
    for b in grammalocald grammalocal-popup grammalocal; do
        install -m 755 "$src_bin/$b" "$BIN_DIR/$b"
    done
    install -m 644 "$src_worker" "$SHARE_DIR/grammalecte_worker.py"
    rm -rf "$SHARE_DIR/grammalecte-2.3.0"
    cp -R "$src_vendor" "$SHARE_DIR/grammalecte-2.3.0"
    # Precompile the bytecode now. Otherwise the worker compiles Grammalecte's
    # large generated rule modules on its first check, which peaks at about
    # 435 MB instead of 70 MB (Python keeps that memory afterwards).
    if command -v python3 >/dev/null 2>&1; then
        python3 -m compileall -q "$SHARE_DIR/grammalecte-2.3.0" >/dev/null 2>&1 \
            || say "warning: compileall failed; the first check will be slower"
    else
        say "warning: python3 not found; grammalocal needs Python 3.8 or newer"
    fi
    install -m 644 "$src_unit" "$UNIT_DIR/grammalocald.service"
    install -m 644 "$src_icon" "$ICON_DIR/grammalocal.svg"
    install -m 644 "$src_desktop" "$APP_DIR/grammalocal.desktop"
    if command -v update-desktop-database >/dev/null 2>&1; then
        update-desktop-database "$APP_DIR" >/dev/null 2>&1 || true
    fi
    say "Installed files to $BIN_DIR, $SHARE_DIR and $APP_DIR"

    if command -v systemctl >/dev/null 2>&1 \
        && systemctl --user is-active --quiet grammalocald 2>/dev/null; then
        systemctl --user daemon-reload || true
        systemctl --user restart grammalocald || say "warning: could not restart grammalocald"
        say "Restarted the running grammalocald service"
    fi
}

mode_tarball() {
    install_files "$SELF_DIR/bin" \
        "$SELF_DIR/share/grammalecte_worker.py" \
        "$SELF_DIR/vendor/grammalecte-2.3.0" \
        "$SELF_DIR/systemd/grammalocald.service" \
        "$SELF_DIR/share/grammalocal.svg" \
        "$SELF_DIR/share/grammalocal.desktop"
}

mode_source() {
    need cargo
    [ -d "$SELF_DIR/vendor/grammalecte-2.3.0" ] || die "vendor/grammalecte-2.3.0 is missing from the checkout"
    cargo build --release --manifest-path "$SELF_DIR/Cargo.toml" \
        -p grammalocald -p grammalocal-cli -p grammalocal-popup
    install_files "$SELF_DIR/target/release" \
        "$SELF_DIR/crates/grammalocald/worker/grammalecte_worker.py" \
        "$SELF_DIR/vendor/grammalecte-2.3.0" \
        "$SELF_DIR/packaging/grammalocald.service" \
        "$SELF_DIR/assets/logo.svg" \
        "$SELF_DIR/packaging/grammalocal.desktop"
}

mode_remote() {
    need tar
    need sha256sum
    case "$(uname -m)" in
        x86_64) ;;
        *) die "no prebuilt release for $(uname -m); use a source checkout" ;;
    esac
    if command -v curl >/dev/null 2>&1; then
        fetch() { curl -fsSL -o "$2" "$1"; }
    elif command -v wget >/dev/null 2>&1; then
        fetch() { wget -q -O "$2" "$1"; }
    else
        die "curl or wget is required"
    fi

    TMP=$(mktemp -d)
    say "Downloading $TARBALL"
    fetch "$RELEASE_URL/$TARBALL" "$TMP/$TARBALL" || die "download failed: $RELEASE_URL/$TARBALL"
    fetch "$RELEASE_URL/$TARBALL.sha256" "$TMP/$TARBALL.sha256" || die "download failed: checksum file"
    (cd "$TMP" && sha256sum -c "$TARBALL.sha256" >/dev/null) || die "sha256 mismatch; nothing was installed"
    say "Checksum ok"
    mkdir "$TMP/extract"
    tar -xzf "$TMP/$TARBALL" -C "$TMP/extract"
    install_files "$TMP/extract/bin" \
        "$TMP/extract/share/grammalecte_worker.py" \
        "$TMP/extract/vendor/grammalecte-2.3.0" \
        "$TMP/extract/systemd/grammalocald.service" \
        "$TMP/extract/share/grammalocal.svg" \
        "$TMP/extract/share/grammalocal.desktop"
}

if [ -n "$SELF_DIR" ] && [ -f "$SELF_DIR/bin/grammalocald" ]; then
    say "Installing from the extracted release in $SELF_DIR"
    mode_tarball
elif [ -n "$SELF_DIR" ] && [ -f "$SELF_DIR/Cargo.toml" ]; then
    say "Installing from the source checkout in $SELF_DIR"
    mode_source
else
    say "Installing the latest release"
    mode_remote
fi

if [ -n "${GRAMMALOCAL_INSTALL_SKIP_SETUP:-}" ]; then
    say "Skipping setup (GRAMMALOCAL_INSTALL_SKIP_SETUP is set)."
    say "Run $BIN_DIR/grammalocal setup when you are ready."
    exit 0
fi

# Clean up before exec, since exec replaces this process and skips the trap.
cleanup
TMP=""
# When piped from curl, stdin is the script itself: answer setup's questions from
# the terminal instead (or fall back to defaults when there is none).
if [ ! -t 0 ] && (: </dev/tty) 2>/dev/null; then
    exec "$BIN_DIR/grammalocal" setup "$@" </dev/tty
fi
exec "$BIN_DIR/grammalocal" setup "$@"
