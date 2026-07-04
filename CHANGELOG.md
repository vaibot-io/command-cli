# Changelog

All notable changes to the `vaibot` CLI (`command-cli`).

## [Unreleased] — guard-first, platform-aware install

### Changed
- **`vaibot init` / `vaibot guard install` now install the guard via the
  platform-aware ladder.** The CLI shells out to `vaibot-guard install`, which walks
  **systemd → launchd → self-spawn** (root-preferred; `--system` opts into the
  root/sudo tamper boundary), writes the unit, starts it, and **health-verifies**.
  Replaces the old hardcoded `systemctl --user enable` on a possibly-absent unit.
- The guard is installed + **health-gated _before_** the agent plugins are wired
  (`init` step 4 vs step 5).

### Notes
- The CLI stays a thin orchestrator — the ladder's single source of truth lives in
  the guard's node modules (`guard-supervisor` detect + `guard-units` generate +
  `guard-install` walk), not duplicated in Rust.
- A one-command fresh-install bootstrap ships in the guard repo:
  `curl -fsSL https://raw.githubusercontent.com/vaibot-io/vaibot-guard/<branch>/install.sh | sh`.
