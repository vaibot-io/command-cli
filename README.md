# vaibot

The VAIBot front-door CLI — one installer, supervisor, and control surface for the
guard, gateway, plugins, policy, MCP, and provenance across the machine-intelligence
lifecycle. Crate `vaibot`, binary `vaibot`, MIT-licensed. A standalone Cargo project
(its own `Cargo.lock`, not a workspace member).

```bash
cargo install vaibot
vaibot init            # log in, install the guard, wire your agents, set a policy floor
```

## What it is (and isn't)

**The CLI is an orchestrator, never a daemon host.** It installs, configures, and
observes the VAIBot stack — but it does not embed a runtime. `guard serve` and
`gateway serve` locate a *separate* binary and exec it in the foreground with
signal + exit-code passthrough; if the binary is absent they print the
separate-binary model and exit non-zero. They never spawn an in-process daemon.

- **guard** — the per-host policy engine + tamper-evident audit log, a singleton on
  `:39111`. Installed from npm (`@vaibot/guard@^2.0.0`, so security patches flow
  without a CLI release). Resolution: `$VAIBOT_GUARD_BIN` → `vaibot-guard` on PATH →
  the node script the systemd unit launches.
- **gateway** — a local-first LLM proxy. Resolution: `$VAIBOT_GATEWAY_BIN` →
  `vaibot-gateway` on PATH. Route an agent through it with
  `export ANTHROPIC_BASE_URL=http://127.0.0.1:8787`.

## Production by default

`vaibot init` targets **production**, and every command runs a preflight
(`enforce_production_env`) before it sends a bearer: a non-admin / non-enterprise
account is refused against a non-production host, and an established production key
can never be diverted to an env-injected URL. Staging / self-host is available to
admins via `--api-url` (or `VAIBOT_GOVERNANCE_URL` with the deliberate-act flag).

## Command surface

Everything below is **wired (REAL)** unless flagged. `serve` commands **shell out**
to the separate daemon; four commands are tracked **stubs** (exit 2).

| Command | Notes |
|---|---|
| `login [--device] [--no-browser]` | Browser loopback PKCE (127.0.0.1:0, S256); `--device` = RFC 8628; auto-fallback when no browser. |
| `logout [--all-hosts]` | Clears the local session. |
| `whoami [--json]` | OAuth / api-key identity. Exit 3 when not logged in. |
| `account claim [--email]` | Link this machine to your real account (verified by an emailed code). |
| `init [-y] [--env] [--api-key] [--skip-login] [--with-mcp] [--preset]` | Log in → install the guard → detect + wire agents → set a governance floor. `--with-mcp` registers the MCP server with every detected agent; `--preset permissive\|balanced\|strict`. `--with-gateway` = *stub*. |
| `status [--json]` | Joined `GET /v2/health` + `/v2/accounts/me` (auth, health, quota). |
| `doctor [--fix]` | Read-only stack checks. (`--fix` remediation = *stub*.) |
| `update` | *stub* |
| `guard install` | `npm i -g @vaibot/guard@^2.0.0` + env file + systemd unit. |
| `guard serve` | *shell-out* to the guard binary. |
| `guard {status, restart, stop, logs, policy}` | systemd control + `/health` + `/v1/policy`. |
| `guard {verify, provision-offline}` | *stub* (offline / air-gapped bundle verify). |
| `gateway serve` | *shell-out* to the Rust gateway. |
| `gateway {status, config, stop, logs}` | systemd + `/healthz`, resolved config, egress log. |
| `plugin add <host> [--skip-guard] [--skip-plugin]` | Install a host circuit-breaker (`claudecode\|codex\|openclaw`) + ensure the shared guard. |
| `plugin list [--json]` | Detect hosts + guard + circuit-breakers. |
| `plugin remove <host> [--with-guard]` / `plugin update <host> [--skip-guard]` | Uninstall / upgrade an integration (the guard is shared across hosts). |
| `policy show` / `policy history` | Active policy (floor + your additions + lock state) / audited change log. |
| `policy preset [flavor]` | Show or set your governance floor (`permissive\|balanced\|strict`). |
| `policy deny <patterns…>` / `policy allow <patterns…>` | Add / remove denials on top of your floor (never below it). |
| `policy edit [--file] [--dry-run]` | Bulk-edit your additions declaratively (the floor stays read-only). |
| `policy lock` / `policy unlock [--permanent]` | Freeze the policy (changes then require email confirmation) / open a 30-min window. |
| `policy {pull, diff, revoke}` | Transitional YAML working-copy + coarse rollback. |
| `mode show` | Live control-plane + guard-enforced mode (observe \| enforce). |
| `mode {enforce, observe}` | Opens the dashboard to switch (email-confirmed there). |
| `mcp connect [host]` / `mcp status` / `mcp disconnect [host]` | Register / show / remove the hosted VAIBot MCP server (`{api_base}/v2/mcp`, api-key bearer) in each agent's native config. Omit host → all detected. |
| `provenance list [--agent --risk --decision --pending --limit]` | Browse governance receipts (`GET /v2/receipts`). |
| `provenance show <id>` | Full event chain for a receipt (id or content-hash prefix). |
| `provenance tail [--agent --risk --decision --type --follow]` | Stream live decisions (SSE `/v2/receipts/stream`, reconnect backoff). |
| `provenance anchor` | *stub* (on-chain anchoring status). |
| `receipts <sub>` | Alias of `provenance` — same subcommands. |

### Exit codes

`0` ok · `1` error · `2` stub (and clap usage errors) · `3` auth. The stub line is
canonical:

```
vaibot <noun> is not yet wired — see <noun> (tracked; this is an orchestrator stub)
```

## State + credentials

All state lives under `~/.vaibot/` (0600, atomic writes):

- `credentials.json` — a single, env-namespaced api-key store (v3): one store with a
  per-environment slot holding the API key, wallet, and optional `governance` /
  `provenance` base-URL overrides.
- `oauth.json` — the interactive user OAuth session sidecar.
- `policy.yaml` — a local policy working copy (transitional `pull` / `diff`).

The account base resolves `--api-url` flag → `VAIBOT_GOVERNANCE_URL` → stored slot →
canonical default; a **production** URL override is suppressed unless
`VAIBOT_ALLOW_URL_OVERRIDE` is set *and* the account is a server-verified admin (the
§5 gate). The deprecated `VAIBOT_API_URL` is used for environment inference only and
overrides no base.

Config-dir precedence: `$VAIBOT_CONFIG_DIR` → `$VAIBOT_CREDS_DIR` →
`$XDG_CONFIG_HOME/vaibot` → `~/.vaibot`. Secret writes go through an atomic
mkdir-0700 → temp-0600 → rename → re-chmod-0600 path, and reads refuse
group/world-readable files. The guard daemon keeps its own creds under
`~/.config/vaibot-guard/` so they survive `vaibot logout`.

Every networked command funnels its bearer through a credential broker — no command
reads tokens directly — so credential handling stays in one auditable place. The
wired broker is full-access today; a least-privilege (scoped-keys) broker is a
drop-in seam behind `get_broker()`, swappable with zero call-site churn.

## Build / test

```bash
cargo build
cargo test
cargo clippy --all-targets
```

No network call happens at build, test, parse, `--help`, or `--version` time. OIDC
discovery + token exchange fire only inside the `login` / refresh flows; V2 API calls
fire only inside the command that invokes them.

## Setup playbooks

Step-by-step, production-by-default guides grounded in real end-to-end runs:

- [Fresh machine](docs/setup-fresh-machine.md) — no VAIBot components yet.
- [Machine with circuit breaker(s) already installed](docs/setup-existing-circuit-breakers.md) — adopt the CLI without disrupting the running guard singleton.
