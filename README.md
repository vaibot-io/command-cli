# vaibot

The VAIBot front-door CLI — one installer/supervisor for the guard, gateway,
plugins, policy, and MCP across the machine-intelligence lifecycle. Crate
`vaibot`, binary `vaibot` (`cargo install vaibot`). MIT-licensed. Standalone
Cargo project (its own `Cargo.lock`, **not** a workspace member).

This is the Rust port of the TypeScript `@vaibot/cli` scaffold; it mirrors that
command tree and its REAL / STUB / SHELL-OUT split 1:1. The crate version and
`vaibot --version` are aligned at `0.3.0`.

## Setup playbooks

Step-by-step install guides (production by default), grounded in real end-to-end runs:

- [Fresh machine](docs/setup-fresh-machine.md) — no VAIBot components yet.
- [Machine with circuit breaker(s) already installed](docs/setup-existing-circuit-breakers.md) — adopt the CLI without disrupting the running guard singleton.

## Installer-not-runtime guardrail

**The CLI is an orchestrator, never a daemon host.** `guard serve` and
`gateway serve` locate a *separate* binary and exec it in the foreground with
SIGINT/SIGTERM passthrough and exit-code passthrough (`services::child`). If the
binary is absent, the command prints the separate-binary model and exits
non-zero — it **never** embeds or spawns an in-process daemon.

- **guard**: per-host singleton on `:39111`. Resolution order:
  `$VAIBOT_GUARD_BIN` → `vaibot-guard` on PATH → the node skill-script the
  systemd unit launches.
- **gateway**: local-first Rust LLM proxy. Resolution order:
  `$VAIBOT_GATEWAY_BIN` → `vaibot-gateway` on PATH. Route an agent through it
  with `export ANTHROPIC_BASE_URL=http://127.0.0.1:8787`.

## Command surface

| Command | Status | Notes |
|---|---|---|
| `login [--device] [--no-browser]` | REAL | Loopback PKCE default (127.0.0.1:0, `/callback`, S256); `--device` = RFC 8628; auto-fallback to device when no browser. Issuer-persisted refresh. |
| `logout [--all-hosts]` | REAL | Clears the local session. `--all-hosts` parsed-but-inert (god-key model). |
| `whoami [--json]` | REAL | OAuth session or api_key identity. Exit 3 when not logged in. |
| `init [...]` | REAL composer | Login + guard install + detect/wire agents. `--with-mcp` connects MCP to detected agents; `--with-gateway` is note-only. Defaults to **production** (no signing-key/operator steps). |
| `status [--json]` | REAL | GET /v2/health + /v2/accounts/me (joined). `--json` never throws. |
| `doctor [--fix]` | REAL | Read-only checks. `--fix` prints a note, then runs read-only. |
| `update` | STUB | exit 2 |
| `guard serve` | SHELL-OUT | execs the separate guard binary. |
| `guard status` | REAL | systemd + `/health`. |
| `guard {restart,stop,logs,policy,verify,provision-offline}` | STUB | exit 2 |
| `gateway serve` | SHELL-OUT | execs the separate Rust gateway binary. |
| `gateway {status,config,stop,logs}` | STUB | exit 2 |
| `plugin add [host] [--skip-guard] [--skip-plugin]` | REAL | clawhub + guard.env + systemd unit + openclaw plugin. |
| `plugin list [--json]` | REAL | Detects hosts + guard skill + circuit-breaker. |
| `plugin {remove,update}` | STUB | exit 2 |
| `policy show` | REAL | Active signed policy from the local guard (`/v1/policy`). |
| `policy history` | REAL | GET /v2/policy/history. |
| `policy tighten <patterns...>` | REAL | POST /v2/policy/request (signed **server-side**). |
| `policy revoke` | REAL | POST /v2/policy/revoke. |
| `policy {set,diff,pull}` | STUB | exit 2; `set` carries the Ed25519-sign-path hint. |
| `mode {show,enforce,observe}` | STUB | exit 2 |
| `mcp {connect,status,disconnect} [host]` | REAL | Registers the hosted VAIBot MCP server (`{api_base}/v2/mcp`) with each agent's native MCP config, api-key as a bearer header. Omit host → all detected. |
| `provenance list [--agent --risk --decision --pending --limit]` | REAL | GET /v2/receipts (filtered). |
| `provenance show <id>` | REAL | GET /v2/receipts/:id/events. |
| `provenance tail [--agent --risk --decision --type --hash]` | REAL | SSE `/v2/receipts/stream`, `[3,6,12,24,48]s` reconnect backoff then give-up. |
| `provenance anchor [--status]` | STUB | exit 2 |
| `receipts <sub>` | alias | Twin of `provenance`, same `ProvenanceCmd` dispatch. |

### Exit codes

`0` ok · `1` error · `2` stub (and clap usage errors) · `3` auth.
The canonical stub line is verbatim:

```
vaibot <noun> is not yet wired — see <noun> (tracked; this is an orchestrator stub)
```

## Credential broker (god-key now / scoped later)

Every networked command funnels its bearer through `broker::get_broker().get()`
— no command reads tokens directly. The only wired impl,
`FileCredentialBroker`, is a **god-key** broker: it ignores
`CredentialRequest.audience/scopes` and returns the single full-access
user/api_key credential. `ScopedCredentialBroker` (in `broker/scoped.rs`) is the
least-privilege **seam**: it delegates `login/logout/whoami` and returns a clean
StubError from `get`. Swapping the binding in `get_broker()` (gated on
`VAIBOT_SCOPED_KEYS=1`) is the entire migration — zero call-site churn.

## On-disk state (`~/.vaibot`, 0600 atomic)

- `credentials.json` — env-namespaced api_key store (`config::creds`).
- `oauth.json` — interactive user OAuth session sidecar (`config::token_store`).
- `policy.yaml` — local policy working copy (deferred `set`/`pull`).

Config-dir precedence: `$VAIBOT_CONFIG_DIR` → `$VAIBOT_CREDS_DIR` →
`$XDG_CONFIG_HOME/vaibot` → `~/.vaibot`. All secret writes go through
`config::atomic::write_atomic_0600` (mkdir 0700 → temp 0600 → rename →
re-chmod 0600); reads refuse group/world-readable files. The guard daemon keeps
its own creds under `~/.config/vaibot-guard/` so they survive `vaibot logout`.

## Rust footprint note

Deliberate departures from the `vaibot-gateway` crate's conventions:

- **`directories` instead of `dirs`** — `ProjectDirs`/`BaseDirs` give correct
  XDG resolution; the `~/.vaibot` precedence chain is layered on top in
  `config/mod.rs` to stay aligned with the credential store.
- **`async-trait`** — the broker is held as `&'static dyn CredentialBroker` for
  the binding-swap seam, so the async trait must be `dyn`-safe.
- **manual OIDC discovery** — `oauth2` v5 has no built-in discovery (that lives
  in `openidconnect`); `oauth/discovery.rs` fetches
  `/.well-known/openid-configuration` with a rustls reqwest client and feeds the
  endpoints into an `oauth2::Client` carrying an `id_token` extra field.

`[profile.release]` (`strip` / `lto = "thin"` / `codegen-units = 1`),
`reqwest` rustls-tls, `tokio = ["full"]`, and the two-level clap derive shape
are copied verbatim from the gateway.

### Network gate

No network call happens at build, test, parse, `--help`, or `--version` time.
Discovery + token exchange fire only inside the `login`/refresh flows; V2 API
calls fire only inside the command that invokes them.

## Build / test

```bash
cargo build
cargo test
cargo clippy --all-targets
```
