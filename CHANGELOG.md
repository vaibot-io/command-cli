# Changelog

All notable changes to the `vaibot` CLI (`command-cli`).

## [0.7.0] — 2026-09-28 — the panic switch, Hermes, and updating in place

### Added — updating what you already have
- **`vaibot update` now updates the whole installation**, not just the CLI: the shared
  guard, every installed host circuit-breaker, then the CLI.

  The CLI goes **last** on purpose — a self-update replaces the running binary, so
  anything sequenced after it would be running code that had just been overwritten.

  `update` joins `init` and `doctor` as a whole-stack lifecycle verb, which is the
  pattern this CLI already established: those two do not ask you which component you
  meant either. Component-scoped work stays in its component's group.

  Flags: `--cli-only` (the previous behaviour, kept because it may have been scripted),
  and `--skip-guard` / `--skip-plugins` / `--skip-cli`. `--cli-only` conflicts with all
  three, refused at parse time rather than part-way through reinstalling things.

  Exit code is honest: if a plugin or the guard fails to update, the command reports it
  and exits non-zero rather than printing "complete".

- **`vaibot guard update`** — the guard alone, then a restart.

  Until now the only way to update the guard was as a side effect of
  `vaibot plugin update <host>`, which bundles "guard + that host's plugin". That
  bundling is deliberate and **unchanged** — someone coming back after a while wants
  both refreshed. But the guard is shared across every host, so the one component every
  install has had no command of its own, in the one group where you would look for it.

  A failed restart warns rather than failing: a guard running an older build is still
  governing, and the new version takes effect the next time it starts.

### Internal
- One implementation of "update the guard", in `commands::guard::update`. The plugin
  path calls it and downgrades a failure to a warning — updating a host's plugin should
  not fail because a shared, still-working guard is stale — while `vaibot update` and
  `vaibot guard update` take the error. Two failure policies, one code path.


### Added — Hermes support
- **`vaibot plugin add hermes`** — the fifth host. Hermes has a plugin CLI, but only
  for enabling: the plugin is Python, so the files have to arrive first.

  This delegates placement to the published npm installer
  (`npx @vaibot/hermes-circuitbreaker-plugin install`), which fetches the wheel from
  PyPI and verifies it against a digest pinned at its own publish time. Deliberately
  not `pip`: that fails outright on PEP 668 systems and picks whichever interpreter is
  on `PATH`. And deliberately not a second implementation of the digest check here —
  one verification, for the same reason the breakers call the guard's `classify`
  instead of growing a second classifier.

  Unlike the other hosts it does **not** use the best-effort install loop. A digest
  mismatch has to be able to fail the whole operation, and that loop only warns.

  `update hermes` re-runs the installer, so it re-fetches and re-verifies.
  `remove hermes` disables **and** deletes the directory, because `plugins disable`
  leaves the files in place.

- `supports_mcp_connect()`, separating "no MCP CLI" from "MCP surface unknown".
  **Hermes is excluded from `vaibot mcp connect`**: nobody has established how it
  registers an MCP server and the circuit-breaker plugin does not, so guessing at a
  command and running it against someone's agent config would be worse than not
  offering the feature. `mcp connect hermes` now says that instead of failing silently.

### Fixed
- `mcp connect <host>` gated the single-host path on `is_file_based()` while the
  all-hosts path used a different rule, so a host with no MCP surface was let through
  and then failed quietly inside `connect_one`. Both paths use one predicate now.
- The `plugin add` host list in `README.md` was missing **cursor** as well as hermes.

### Added — the panic switch
- **`vaibot contain [--reason "..."]`** — stop every agent on this account, on
  every machine, right now. Containment is enforced before policy or classifier
  runs and holds even in observe mode; guards are pushed to, so it lands in about
  a second rather than at the next poll.
- **`vaibot release`** — lift containment. A signed-in session plus a second
  factor: an emailed code, or `--recovery-code <CODE>`.
- **`vaibot release --recovery-code <CODE>`** — confirm with one of your saved
  recovery codes instead of an emailed one. This is the way back when you can't
  reach the inbox the code would go to, so it deliberately routes around the email
  path entirely and is checked before any of it runs. Generate a set **before** you
  need one: codes can't be issued while an account is contained.

Arming and clearing are deliberately asymmetric, and the UX follows. `contain`
takes no confirmation: hesitating is the expensive mistake in a panic, and a
false arm is undone in a minute. It is idempotent, so a second pull reports the
state rather than erroring and keeps the original reason on record. `release`
tries the release first and only asks for a code when one is genuinely needed — so
"nothing to lift" never sends an email, and an already-confirmed window just
works.

Release is **not** gated on the platform superuser flag, deliberately. `admin` in
this system is platform-wide, not an account role, so requiring it would have
guaranteed a lockout: any account could arm containment and then never lift it. The
asymmetry is carried by the two things that do matter — a session rather than an api
key, plus a second factor.

These are top-level verbs rather than `vaibot policy contain`: containment is
not policy, it is the thing that ignores policy.

Requires an API serving `/v2/enforcement/*` (governance-api 2.2.0 or later).

## [0.6.2] — 2026-07-10 — CLI self-update

### Added
- **`vaibot update` — self-update.** Checks crates.io for the latest `vaibot`
  release and, if newer, downloads and runs the official installer. Other
  commands also show a best-effort **"update available"** notice (cached 24h,
  bounded to 2s, never blocking). Opt out with `VAIBOT_NO_UPDATE_CHECK=1`.
- **Installer verification.** Before the installer is executed it is fetched over
  HTTPS, its HTTP status is checked, and its payload is validated (non-empty,
  shebang, size cap, not an HTML page). The **SHA-256 is printed** for
  auditability; set `VAIBOT_INSTALL_SHA256` to pin a known-good digest (a
  mismatch aborts), and in interactive mode you confirm the digest before it
  runs.

### Fixed
- **`vaibot update` no longer panics.** The command created a nested tokio
  runtime inside the already-async dispatch and aborted with "Cannot start a
  runtime from within a runtime" on every invocation.
- **Version parsing hardened** so a malformed version string (prerelease,
  `v`-prefix, extra components) can't silently misparse and hide or fabricate an
  update. Double-digit components now compare numerically (`0.10.0 > 0.9.0`).

## [0.6.1] — 2026-07-08 — Platinum `vaibot init` (reliability + clarity)

### Added
- **Anonymous install telemetry.** A successful `vaibot plugin add <host>` (or an
  init-driven plugin install) sends a best-effort event to the API
  (`POST /v2/telemetry/plugin-install`) — just the host, CLI version, and platform —
  so hosts distributed outside npm (notably the git-cloned Cursor plugin, which
  produces no npm download stat) get an adoption signal. **It's anonymous: nothing
  that identifies you or your account is stored — only the aggregate
  host/version/platform count.** (The request is authenticated purely as an abuse
  gate; identity is never read or stored.) Bounded (≤4s), swallows every error, and
  never blocks or fails the install. **Opt out** with `VAIBOT_NO_TELEMETRY=1` or the
  standard `DO_NOT_TRACK=1`. Requires the API's migration 032 + endpoint.

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

## [0.6.0] — 2026-07-06 — Cursor plugin support

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
