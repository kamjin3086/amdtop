#!/usr/bin/env bash
#
# Install this fork so that both `amdtop` and `sudo amdtop` work.
#
# Why the symlink: `sudo` replaces PATH with the sudoers `secure_path`
# (/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:...), which does not
# include ~/.cargo/bin. Without a copy or link inside that path,
# `sudo amdtop` fails with "command not found" even though `amdtop` works.
#
# Usage:
#   ./install.sh               install to ~/.cargo/bin + symlink /usr/local/bin
#   ./install.sh --system      install directly into /usr/local/bin
#   ./install.sh --no-symlink  install to ~/.cargo/bin only
#
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
BIN_NAME="amdtop"
SYSTEM_PATH="/usr/local/bin/${BIN_NAME}"
MODE="user"

usage() {
    cat <<'EOF'
Usage:
  ./install.sh               install to ~/.cargo/bin + symlink /usr/local/bin
  ./install.sh --system      install directly into /usr/local/bin
  ./install.sh --no-symlink  install to ~/.cargo/bin only
EOF
}

for arg in "$@"; do
    case "$arg" in
        --system) MODE="system" ;;
        --no-symlink) MODE="user-no-symlink" ;;
        -h|--help) usage; exit 0 ;;
        *) echo "unknown option: $arg" >&2; usage >&2; exit 2 ;;
    esac
done

if ! command -v cargo >/dev/null 2>&1; then
    echo "error: cargo is required (Rust 1.88 or newer)" >&2
    exit 1
fi

cd "$REPO_ROOT"

# Fresh machines need the network to fetch crates; machines with a warm cache
# can still install when crates.io is unreachable.
cargo_run() {
    if "$@"; then
        return 0
    fi
    echo "warning: cargo failed (crates.io unreachable?); retrying with --offline" >&2
    "$@" --offline
}

case "$MODE" in
    system)
        echo "==> building release binary"
        cargo_run cargo build --release
        echo "==> installing to ${SYSTEM_PATH}"
        sudo install -Dm755 "target/release/${BIN_NAME}" "$SYSTEM_PATH"
        echo
        echo "installed: ${SYSTEM_PATH}"
        echo "  amdtop        works"
        echo "  sudo amdtop   works"
        ;;
    user|user-no-symlink)
        echo "==> installing to ~/.cargo/bin (builds as your user)"
        cargo_run cargo install --path . --force
        user_bin="${HOME}/.cargo/bin/${BIN_NAME}"
        echo "installed: ${user_bin}"

        if [ "$MODE" = "user-no-symlink" ]; then
            echo
            echo "skipped the system symlink; run it as root with:"
            echo "  sudo ${user_bin}"
            exit 0
        fi

        echo "==> linking ${SYSTEM_PATH} -> ${user_bin}"
        if [ -e "$SYSTEM_PATH" ] && [ ! -L "$SYSTEM_PATH" ]; then
            echo "warning: ${SYSTEM_PATH} already exists and is not a symlink; leaving it alone" >&2
            echo "         run amdtop as root with: sudo ${user_bin}" >&2
            exit 0
        fi
        if command -v sudo >/dev/null 2>&1 && sudo ln -sf "$user_bin" "$SYSTEM_PATH"; then
            echo
            echo "installed and linked:"
            echo "  amdtop        works"
            echo "  sudo amdtop   works"
        else
            echo "warning: could not create ${SYSTEM_PATH}; \`sudo amdtop\` will not work" >&2
            echo "         create it yourself, or run amdtop as root with:" >&2
            echo "           sudo ln -sf \"${user_bin}\" ${SYSTEM_PATH}" >&2
            echo "           sudo ${user_bin}" >&2
        fi
        ;;
esac

echo
echo "==> version"
"$SYSTEM_PATH" --version 2>/dev/null || "${HOME}/.cargo/bin/${BIN_NAME}" --version
