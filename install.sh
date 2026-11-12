#!/usr/bin/env bash
set -euo pipefail

# Install from a checkout:
#   ./install.sh
#
# Install from the internet:
#   curl -fsSL https://raw.githubusercontent.com/TheShovel/smol-coder/main/install.sh | bash

REPO="TheShovel/smol-coder"
BRANCH="main"
BIN_DIR="${SMOL_BIN_DIR:-$HOME/.local/bin}"
RC_FILE="${SMOL_RC_FILE:-}"

say() { printf '%s\n' "$*"; }
die() { printf 'smol install: %s\n' "$*" >&2; exit 1; }
need() { command -v "$1" >/dev/null 2>&1 || die "missing required command: $1"; }

ROOT=""
SOURCE="${BASH_SOURCE[0]:-}"
if [ -n "$SOURCE" ] && [ -f "$SOURCE" ]; then
    CANDIDATE="$(cd "$(dirname "$SOURCE")" && pwd)"
    if [ -f "$CANDIDATE/Cargo.toml" ]; then
        ROOT="$CANDIDATE"
    fi
fi

if [ -z "$ROOT" ]; then
    need curl
    need tar
    TMP_DIR="$(mktemp -d)"
    trap 'rm -rf "$TMP_DIR"' EXIT
    say "Downloading smol"
    curl --proto '=https' --tlsv1.2 -fsSL "https://github.com/$REPO/archive/refs/heads/$BRANCH.tar.gz" |
        tar -xz -C "$TMP_DIR" --strip-components=1
    ROOT="$TMP_DIR"
fi

if [ ! -f "$ROOT/Cargo.toml" ]; then
    die "could not find smol source"
fi

if ! command -v cargo >/dev/null 2>&1; then
    need curl
    say "Installing Rust"
    curl --proto '=https' --tlsv1.2 -fsSL https://sh.rustup.rs | sh -s -- -y --profile minimal
    # shellcheck source=/dev/null
    source "$HOME/.cargo/env"
fi
command -v cargo >/dev/null 2>&1 || die "Rust installation failed"

say "Building smol"
(cd "$ROOT" && cargo build --release)

say "Removing previous smol installations"
rm -f "$BIN_DIR/smol" "$BIN_DIR/smol-coder"
rm -rf "$HOME/.config/smol" "$HOME/.local/share/smol" "$HOME/.local/share/smolcoder" "$HOME/.local/share/smol-coder"

mkdir -p "$BIN_DIR"
install -m 755 "$ROOT/target/release/smol" "$BIN_DIR/smol"

if ! printf '%s' ":$PATH:" | grep -Fq ":$BIN_DIR:"; then
    if [ -z "$RC_FILE" ]; then
        case "$(basename "${SHELL:-bash}")" in
            zsh) RC_FILE="$HOME/.zshrc" ;;
            *) RC_FILE="$HOME/.bashrc" ;;
        esac
    fi
    touch "$RC_FILE"
    if ! grep -Fq "# smol path" "$RC_FILE"; then
        {
            printf '\n# smol path\n'
            printf 'export PATH="%s:$PATH"\n' "$BIN_DIR"
        } >>"$RC_FILE"
    fi
    say "Added $BIN_DIR to PATH in $RC_FILE"
    say "Open a new terminal or run: source \"$RC_FILE\""
else
    say "$BIN_DIR is already on PATH"
fi

say ""
say "Installed $BIN_DIR/smol"
say "Run smol in any folder to open it"
