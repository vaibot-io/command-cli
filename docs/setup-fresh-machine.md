# Setup playbook — fresh machine

For a machine with **no VAIBot components installed yet**. End state: the guard
running, your agent(s) wired with the circuit-breaker plugin, the VAIBot MCP
tools connected, and your account claimed — all on **production**.

> Production is the default — and enforced: the CLI **refuses to run** outside
> production for self-serve accounts, and `vaibot init` reconciles every component
> to production. You do **not** pass any `--env` flag. Staging is reserved for
> admin/enterprise accounts (see [Appendix: staging](#appendix-staging-admin-and-enterprise)).

---

## 0. Prerequisites

- **Rust toolchain** (`cargo`) — the CLI installs via `cargo install vaibot`.
  Get it from <https://rustup.rs> if needed.
- **Node.js 18+** on PATH (`node -v`) — the guard ships as `@vaibot/guard` (npm)
  and the MCP fallback can use `npx`.
- **At least one agent CLI** installed and on PATH:
  - Claude Code (`claude`), Codex (`codex`), and/or OpenClaw (`openclaw`).
- **No `VAIBOT_*` environment variables exported.** Check:
  ```bash
  env | grep '^VAIBOT' || echo "clean ✓"
  ```
  An exported `VAIBOT_API_URL` (or `VAIBOT_ENV` / `VAIBOT_API_KEY`) **overrides
  the production default** — a clean customer machine has none of these.

---

## 1. Install the `vaibot` CLI

```bash
cargo install vaibot            # installs the `vaibot` binary into ~/.cargo/bin
vaibot --version                # expect 0.3.0
```

(Before the crate is published — or for local dev — build from the repo instead:
`cargo install --path packages/command-cli`.)

---

## 2. Onboard with `vaibot init`

```bash
vaibot init
```

This single command, in order:

1. Prints `▸ Environment: production` and provisions a free **production**
   account by machine fingerprint (`✔ Account provisioned`, a `vb_live_…` key
   stored in `~/.vaibot/credentials.json`).
2. Offers to **link your email** — type it to claim the account now, or press
   Enter to skip and claim later from the dashboard.
3. Installs the **guard** (`@vaibot/guard`), writes its env file
   (`~/.config/vaibot-guard/vaibot-guard.env`), and enables the systemd user
   service. The guard governs your tool calls locally from the start, writing a
   tamper-evident receipt for each.
4. Detects your agent CLIs and asks **“Wire <agent> now?”** for each — answer
   **Y** to install the circuit-breaker plugin for that agent.

> `vaibot init` never generates signing keys and never mentions Fly.io — those
> were operator/self-host concerns and are off the customer path.

**One-shot variant** (auto-wire MCP + set the floor, no per-agent MCP prompt):

```bash
vaibot init --with-mcp --preset balanced
```

---

## 3. Set your governance floor + mode

`init` sets a **floor** — the baseline policy the guard applies. Pick one at the
prompt, pass `--preset`, or set it any time:

```bash
vaibot init --preset balanced      # or later: vaibot policy preset balanced
```

- **balanced** (recommended) — routine commands (installs, builds, tests, unknown
  commands) run freely; only genuinely high-risk actions pause for approval:
  outbound network / egress, `git push`, package publish, deploys, `sudo`/`su`,
  and out-of-workspace or secret-dir writes. **strict** adds outbound-network /
  privilege / secret-read denials and asks on installs; **permissive** only blocks
  the catastrophic floor (e.g. `rm -rf /`, `curl … | sh`).
- New free-tier accounts start in **observe** mode — the guard classifies every
  tool call and writes a receipt, but does **not** block. Flip to **enforce** (so
  `ask`/`deny` actually stop the agent) from the dashboard: `vaibot mode enforce`
  opens it, `vaibot mode show` reports the live mode.

---

## 4. Connect the MCP governance tools

If you didn't use `--with-mcp`, register the VAIBot MCP server (15 governance
tools: status, pending, approve, deny, receipts, policy, …) with every detected
agent:

```bash
vaibot mcp connect
```

- It registers `https://api.vaibot.io/v2/mcp` with your `vb_live_…` key as a
  bearer header. No browser/OAuth step.
- **Codex only:** Codex reads the token from `$VAIBOT_API_KEY` at runtime. Export
  it in the shell Codex runs in:
  ```bash
  # pull the active-env key straight from the CLI's credential store:
  export VAIBOT_API_KEY="$(jq -r '.environments[.active_env].api_key' ~/.vaibot/credentials.json)"
  # …and persist it (so new shells have it):
  echo 'export VAIBOT_API_KEY="$(jq -r '"'"'.environments[.active_env].api_key'"'"' ~/.vaibot/credentials.json)"' >> ~/.bashrc
  ```
  (Or just paste your `vb_live_…` key directly.)

---

## 5. Claim your account (if you skipped step 2)

Run `vaibot init` again and enter your email at the prompt, or claim from the
dashboard. Until claimed, `vaibot status` shows the account as `(unclaimed)`.

---

## 6. Verify

```bash
vaibot status        # Env production · api.vaibot.io reachable · vb_live_… · quota
vaibot doctor        # CLI presence, guard service, key, policy posture
vaibot plugin list   # which agents have the circuit-breaker installed
vaibot mcp status    # Claude Code / Codex / OpenClaw → registered
```

A healthy fresh install shows:

```
  Env         production
  API         https://api.vaibot.io  reachable
  API key     vb_live_…
  guard service:       active
```

---

## Troubleshooting

| Symptom | Cause / Fix |
|---|---|
| `✗ Staging is reserved for admin + enterprise accounts` (init refused) | `vaibot init` is production-only and ignores a staging shell override, so plain `init` is what you want. `--env staging` is gated — admins testing staging: `VAIBOT_ADMIN_OVERRIDE=1 vaibot init --env staging`. |
| A command other than init/status/doctor is refused with `✗ VAIBot runs only in the production environment` | A component drifted to staging. `unset VAIBOT_ENV VAIBOT_API_URL`, then `vaibot init` to reconcile. |
| `doctor`: `guard /v1/policy: HTTP 404 — outdated build` | The guard daemon predates the `/v1/policy` route. `vaibot guard restart` (or reinstall `@vaibot/guard`). |
| Codex MCP tools 401 / “not authenticated” | `$VAIBOT_API_KEY` isn't exported in the shell Codex runs in (see step 4). |
| `could not send magic link: unauthorized` | Your key and API base disagree (e.g. a staging key on prod). On a clean machine this shouldn't happen; verify `vaibot status` shows `production` + a `vb_live_…` key. |
| OpenClaw plugin “not detected after install” | See the existing-machine playbook's OpenClaw section — usually a stale `path` key in `~/.openclaw/openclaw.json`. |

---

## Appendix: staging (admin and enterprise)

Staging is **reserved for admin and enterprise accounts** — not self-serve
customers. The CLI refuses every command (and `vaibot init --env staging`) outside
production unless the account is `admin`/`enterprise`. Until the backend `admin`
flag is live, admins use the transitional override:

```bash
VAIBOT_ADMIN_OVERRIDE=1 vaibot init --env staging
VAIBOT_ADMIN_OVERRIDE=1 vaibot <any command>     # while operating on staging
```

Everything else in this playbook is identical; URLs become `staging-api.vaibot.io`
and keys are `vb_stg_…`. (Once `admin` ships on `/v2/accounts/me`, the override is
dropped and the gate keys off the account.)
