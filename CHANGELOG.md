# Changelog

All notable changes to the `vaibot` CLI (`command-cli`).

## [0.6.1] — unreleased — Platinum `vaibot init` (reliability + clarity)

### Changed
- **`vaibot init` reworked for reliability + clarity.** Every step is now
  independent and **best-effort** — a component that fails warns and the flow
  continues (previously a guard-setup error would `?`-abort the *entire* init,
  leaving a half-set-up machine; that hard-fail is the likely reason people were
  reverting to 0.4.1). The flow is interactive with a **y/n before each item**
  (default **Yes**; `--yes` accepts all), asks about **email upfront**, and runs in
  a saner order: **account → email → guard → MCP server → plugins**. Plugins are
  offered per detected agent with **Codex and Cursor last** (they're the most
  interactive to install). Ends with a one-line summary of what installed / skipped
  / failed. Init-driven installs are counted by the anonymous install telemetry too.

## [0.6.0] — unreleased — Cursor plugin support + install telemetry

### Added
- **`cursor` is now a supported host for `vaibot plugin add/remove/update`.** Cursor
  has no plugin-install CLI (unlike claude/codex/openclaw), so — now that
  [`vaibot-io/cursor-circuitbreaker-plugin`](https://github.com/vaibot-io/cursor-circuitbreaker-plugin)
  is published — the CLI installs it by **cloning the repo into
  `~/.cursor/plugins/local/vaibot-cursor`**, where Cursor loads local plugins. `add`
  clones (or `git pull --ff-only` if already present), `update` pulls, and `remove`
  deletes the dir — all idempotent; requires `git`. Restart Cursor (and enable
  `vaibot-cursor` in Customize if prompted) to activate. The Cursor MCP server stays
  file-based (`~/.cursor/mcp.json`), so `vaibot mcp connect` skips Cursor.
- **Anonymous install telemetry.** A successful `vaibot plugin add <host>` sends a
  best-effort event to the API (`POST /v2/telemetry/plugin-install`) — just the
  host, CLI version, and platform — so hosts distributed outside npm (notably the
  git-cloned Cursor plugin, which produces no npm download stat) get an adoption
  signal. **It's anonymous: nothing that identifies you or your account is stored —
  only the aggregate host/version/platform count.** Bounded (≤4s), swallows every
  error, and never blocks or fails the install. **Opt out** with
  `VAIBOT_NO_TELEMETRY=1` or the standard `DO_NOT_TRACK=1`. Requires the API's
  migration 032 + endpoint.

## [0.5.0] — 2026-07-05 — account key recovery

### Added
- `vaibot login` now **recovers a lost local API key**: when `credentials.json`
  has no api_key for the resolved env, it mints one via the session just
  established (`POST /v2/api-keys`) and persists it. No-op when a key already
  exists, so routine logins don't churn keys. Best-effort — narrates on failure,
  never fails an otherwise-successful login. No re-bootstrap dependency and no
  takeover surface (only a verified session grants a key).

### Changed
- `vaibot --version` now reflects the **actual crate version** (`0.5.0`). It was
  previously pinned to `"0.3.0"` to mirror the legacy TS CLI, which left
  `--version` frozen while the crate advanced (0.4.x+) — so `--version` no
  longer matched what `cargo install` resolved. Unpinned.

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
