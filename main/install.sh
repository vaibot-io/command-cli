#!/usr/bin/env sh
  set -eu

  if ! command -v cargo >/dev/null 2>&1; then
    echo "Installing Rust via rustup..."
    curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y
    export PATH="$HOME/.cargo/bin:$PATH"
  fi

  cargo install vaibot
  vaibot status

  export PATH="$HOME/.cargo/bin:$PATH"
