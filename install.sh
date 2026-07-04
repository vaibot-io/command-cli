#!/usr/bin/env sh
# VAIBot universal installer.
#
# Installs the `vaibot` CLI and hands off to first-run setup. Works on macOS and
# Linux, POSIX sh, no bashisms. Safe to pipe:
#
#   curl -fsSL https://raw.githubusercontent.com/vaibot-io/command-cli/main/install.sh | sh
#
set -eu

say() { printf '==> %s\n' "$1"; }
die() { printf 'install.sh: %s\n' "$1" >&2; exit 1; }

# Put ~/.cargo/bin on PATH up front so we find cargo whether it was already
# installed or we install it below. (The previous script exported PATH *after*
# invoking cargo, which was a no-op.)
CARGO_HOME="${CARGO_HOME:-$HOME/.cargo}"
case ":$PATH:" in
  *":$CARGO_HOME/bin:"*) ;;
  *) PATH="$CARGO_HOME/bin:$PATH" ;;
esac
export PATH

# macOS: `cargo install` compiles from source and needs the Command Line Tools
# (clang + linker). Trigger the install and stop; the GUI installer runs async.
if [ "$(uname -s)" = "Darwin" ] && ! xcode-select -p >/dev/null 2>&1; then
  say "Installing the Xcode Command Line Tools (required to build Rust crates)..."
  xcode-select --install >/dev/null 2>&1 || true
  die "Command Line Tools are installing in the background — re-run this script once that window finishes."
fi

# Ensure a Rust toolchain.
if ! command -v cargo >/dev/null 2>&1; then
  command -v curl >/dev/null 2>&1 || die "curl is required to install Rust; install it and re-run."
  say "Installing Rust via rustup..."
  curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y
  # shellcheck disable=SC1091
  . "$CARGO_HOME/env"
fi
command -v cargo >/dev/null 2>&1 || die "cargo not found on PATH after install."

# Install the CLI.
say "Installing the vaibot CLI..."
cargo install vaibot

# Hand off to first-run setup: log in, install the guard, wire agents, set a
# policy floor.
say "Running vaibot init..."
vaibot init
