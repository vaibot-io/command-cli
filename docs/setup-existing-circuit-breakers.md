# Setup playbook — machine with circuit breaker(s) already installed

For a machine that **already has one or more VAIBot circuit-breaker plugins**
(Claude Code / Codex / OpenClaw) and possibly a **guard daemon already running**.
The goal is to adopt the unified `vaibot` CLI and bring everything to a coherent,
current state **without disrupting the running guard**.

> Two rules that make this safe:
> 1. **The guard is a per-host singleton on `:39111`.** There is one daemon for
>    the whole machine; every plugin and the CLI *adopt* it. Never start a second
>    one, and **never stop/disable the running guard** to “fix” something.
> 2. **Production is enforced.** The CLI **refuses to run** (exit 1) when any
>    component isn't on production — only `admin`/`enterprise` accounts may use
>    staging (transitional escape: `VAIBOT_ADMIN_OVERRIDE=1`). If a command is
>    refused or reports `staging`, something in your shell is forcing it
>    (`VAIBOT_API_URL` / `VAIBOT_ENV` / a `vb_stg_` key). `vaibot init` reconciles
>    every component back to production (see Troubleshooting). `status` / `doctor`
>    stay runnable so you can always diagnose + fix.

---

## 0. Snapshot the current state first

```bash
env | grep '^VAIBOT' || echo "env clean ✓"
systemctl --user is-active vaibot-guard          # expect: active
curl -s localhost:39111/health                   # expect: HTTP 200 (JSON body; older daemons may omit fields like "version")
```

Note whether the guard is active — you'll confirm it **stays** active at the end.

---

## 1. Install the `vaibot` CLI

```bash
cargo install vaibot         # installs the `vaibot` binary into ~/.cargo/bin
vaibot --version             # expect 0.3.0
```

(Before the crate is published — or for local dev — build from the repo instead:
`cargo install --path packages/command-cli`.)

> If a stale volta-installed `@vaibot/cli` shim shadows the new binary on PATH (it
> prints no version), remove it (`volta uninstall @vaibot/cli`) or make sure
> `~/.cargo/bin` precedes the volta shim dir on `PATH`.

The CLI **adopts** the running guard and existing plugins; installing it does
not spawn a second guard.

---

## 2. See what the CLI sees

```bash
vaibot status        # env, API reachability, your key
vaibot doctor        # CLI presence, guard service, key, policy posture
vaibot plugin list   # which agents already have the circuit-breaker
vaibot mcp status    # which agents already have MCP registered
```

Read `doctor` carefully — `status`/`doctor` are gate-exempt, so they run even on
a non-production host. It surfaces the common drift problems:

- **`environment: ⚠ NOT coherently production`** (with `env (CLI)` / `env (guard)`
  lines) → components disagree or aren't on production. `vaibot init` reconciles
  them (step 1's note + step 4).
- **`guard /v1/policy: HTTP 404 — guard is running but lacks this route
  (outdated build)`** → the running daemon is an older guard. Fix in step 4.
- **`policy: …`** → whether the guard is on the built-in floor or signed policy.

> **If `doctor` shows `environment: ⚠ NOT coherently production`, run `vaibot init`
> now** to reconcile every component (guard + plugins + MCP) to production. The
> gated steps below (`plugin update`, `guard restart`, `mcp connect`) are **refused**
> on a non-production host, so reconcile first. (Admin/enterprise testing staging:
> prefix each command with `VAIBOT_ADMIN_OVERRIDE=1`.)

---

## 3. Update the existing plugins

Each circuit-breaker plugin pairs with the shared guard. `vaibot plugin update <host>`
refreshes both the global `@vaibot/guard` (restarting the singleton) and the host
plugin; pass `--skip-guard` to update only the host plugin.

```bash
vaibot plugin update claudecode      # only the agents you actually have
vaibot plugin update codex
vaibot plugin update openclaw
```

(These call each agent's native updater — for Claude Code, `claude plugin marketplace
update` then `claude plugin update vaibot-governance@vaibot-claudecode`; for Codex,
`codex plugin marketplace add`; for OpenClaw, `openclaw plugins update`.)

> **Side note — recovering a legacy OpenClaw config.** An older plugin build wrote
> a `path` key into `~/.openclaw/openclaw.json` that current OpenClaw rejects
> (`Unrecognized key: "path"`), which invalidates the *entire* config so every
> `openclaw plugins`/`mcp` command — including the update above — fails. This
> self-heals on install: the current **published** plugin's `postinstall` strips
> the rejected `path` key and rewrites a valid config (writing a `.bak` first), so
> a freshly installed or updated plugin repairs the corruption rather than tripping
> over it. The current GA plugin (1.0.0) hardens this further — it strips the rejected
> `path` from *every* plugin entry, not just its own, and refuses to overwrite an
> `openclaw.json` it cannot parse. If a machine is *already* stuck — OpenClaw can't even run to install the
> update — repair it once with a direct npm install (runs the plugin's
> `postinstall`, bypassing OpenClaw's config load):
>
> ```bash
> npm install -g @vaibot/circuit-breaker-openclaw-plugin@^1.0.0
> ```
>
> (Or delete the `"path": "…"` line from the `circuit-breaker-openclaw-plugin`
> entry by hand, leaving `{ "enabled": true }`.) Then `openclaw plugins list`
> should work again; if it lists but shows **disabled**, `openclaw plugins enable
> circuit-breaker-openclaw-plugin` activates it.

---

## 4. Refresh the guard daemon if it's outdated

If step 2's `doctor` showed the `/v1/policy` **404 / outdated** line, the running
daemon is stale (e.g. it predates the `/v1/policy` route or central audit log).
Refresh it **in place** — this restarts the singleton, it does not remove it:

```bash
vaibot guard restart
vaibot guard status                  # expect: systemd active + /health 200
vaibot doctor                        # /v1/policy line should now be reachable
```

If `restart` isn't enough (the on-disk guard itself is old):

```bash
npm install -g @vaibot/guard@^2.0.0  # refresh the npm-global guard to the 2.x GA (confirm the
                                     # systemd unit's ExecStart points at this binary; if it
                                     # launches a different copy, this won't update it)
vaibot guard restart
```

> Do **not** `systemctl --user stop`/`disable` the guard or kill `:39111`. The
> floor stops being governed while it's down. `restart` keeps governance
> continuous.

---

## 4b. Re-apply your floor for the 2.x recalibration

Guard **2.x** ships the recalibrated **balanced** posture — *"medium = safe"*:
routine commands (installs, builds, tests, unknown commands) run freely, and only
genuinely high-risk actions pause for approval (outbound network / egress,
`git push`, package publish, deploys, `sudo`/`su`, out-of-workspace or secret-dir
writes). The threshold that drives this lives in your **signed policy bundle**, so a
bundle signed before 2.0 lacks it and the guard falls back to the stricter default.
**Re-apply your floor once** to pick it up:

```bash
vaibot policy preset balanced      # or your chosen floor: strict | permissive
vaibot policy show                 # confirm the active floor + posture
```

---

## 5. Connect the MCP governance tools

`mcp connect` is **idempotent** — it cleanly replaces any prior `vaibot` entry,
so it's safe to run on a machine that already has MCP partly configured:

```bash
vaibot mcp connect           # registers the active env's /v2/mcp with your API key
                             # (prod default → api.vaibot.io/v2/mcp + vb_live_…; on
                             # staging → staging-api.vaibot.io/v2/mcp + vb_stg_…)
vaibot mcp status            # all detected agents → registered
```

- **Codex:** ensure `$VAIBOT_API_KEY` is exported in your shell (Codex reads the
  bearer from the env var at runtime).
- If you previously had OAuth-based `vaibot*` MCP entries, you can remove the
  stale ones from each agent; the CLI manages a single entry named `vaibot`.

---

## 6. Final verification

```bash
vaibot doctor
vaibot mode show                             # live governance mode (observe | enforce)
systemctl --user is-active vaibot-guard      # STILL active — never went down
vaibot mcp status                            # registered everywhere
vaibot status                                # env production, vb_live_… key
```

You're done when `doctor` is clean, the guard never left `active`, every agent
shows the plugin installed, and `mcp status` shows `registered`.

---

## Troubleshooting

| Symptom | Cause / Fix |
|---|---|
| `✗ VAIBot runs only in the production environment` (a command is refused, exit 1) | A component is non-production. If your shell is forcing it: `unset VAIBOT_ENV VAIBOT_API_URL` (`VAIBOT_ENV` wins if set) and drop any `vb_stg_` `VAIBOT_API_KEY`. Then `vaibot init` to reconcile guard + MCP to production. `status`/`doctor` still run — use `doctor` to see which component is off. Admin/enterprise accounts are exempt; admins testing staging: prefix with `VAIBOT_ADMIN_OVERRIDE=1`. |
| `guard /v1/policy: HTTP 404 (outdated build)` | Stale daemon. `vaibot guard restart`; if needed `npm i -g @vaibot/guard@^2.0.0` then restart. Never stop/kill it. |
| `could not send magic link: unauthorized` when claiming email | Key/API-base env mismatch (e.g. a `vb_stg_` key resolving against prod, or vice-versa). `vaibot status` should show matching env + key prefix (`production` ↔ `vb_live_`). |
| OpenClaw: `Unrecognized key: "path"` | Legacy-build corruption (a `path` key current OpenClaw rejects). The current published plugin's installer self-heals it — repair once with `npm i -g @vaibot/circuit-breaker-openclaw-plugin@^1.0.0` (writes a `.bak`). See the OpenClaw side note under step 3. |
| Codex MCP tools 401 | `$VAIBOT_API_KEY` not exported where Codex runs — see step 5. |
| Two guard processes / port `:39111` conflict | You started a second guard. Stop the *extra* one only; the singleton/systemd unit owns `:39111`. The CLI and all plugins adopt one daemon — never run your own. |
| `vaibot mcp status` shows a host as `not registered` after connect | That agent's CLI errored during `add`/`set` — re-run `vaibot mcp connect <host>` and read the host CLI output. |
