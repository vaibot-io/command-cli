# Changelog

All notable changes to the `vaibot` CLI (`command-cli`).

## [0.5.0] — 2026-07-05 — account key recovery

### Added
- `vaibot login` now **recovers a lost local API key**: when `credentials.json`
  has no api_key for the resolved env, it mints one via the session just
  established (`POST /v2/api-keys`) and persists it. No-op when a key already
  exists, so routine logins don't churn keys. Best-effort — narrates on failure,
  never fails an otherwise-successful login. No re-bootstrap dependency and no
  takeover surface (only a verified session grants a key).

_Note: `vaibot --version` remains pinned to `0.3.0` by design (TS-contract
compatibility); the crate/crates.io version is what advances._

## [0.4.1] — 2026-07-04 — guard-first install + universal installer

### Added
- **Universal `install.sh` at the repo root** — the one-command bootstrap for the whole
  stack. POSIX sh (macOS + Linux): puts `~/.cargo/bin` on PATH, triggers the macOS Xcode
  Command Line Tools when needed, rustup-bootstraps if `cargo` is missing, `cargo install
  vaibot`, then runs `vaibot init`. Since the CLI is the entry point for the whole stack,
  the installer lives here:
  `curl -fsSL https://raw.githubusercontent.com/vaibot-io/command-cli/main/install.sh | sh`.

### Changed
- **`vaibot init` / `vaibot guard install` now install the guard via the
  platform-aware ladder.** The CLI shells out to `vaibot-guard install`, which walks
  **systemd → launchd → self-spawn** (root-preferred; `--system` opts into the
  root/sudo tamper boundary), writes the unit, starts it, and **health-verifies**.
  Replaces the old hardcoded `systemctl --user enable` on a possibly-absent unit.
- The guard is installed + **health-gated _before_** the agent plugins are wired
  (`init` step 4 vs step 5).

### Fixed
- **`whoami` no longer counts an expired session as logged-in**, so `vaibot init`
  re-authenticates instead of forking onto a throwaway machine account (Phase-3 #3).

### Notes
- The CLI stays a thin orchestrator — the ladder's single source of truth lives in
  the guard's node modules (`guard-supervisor` detect + `guard-units` generate +
  `guard-install` walk), not duplicated in Rust.
