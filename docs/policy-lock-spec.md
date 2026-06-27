# VAIBot Policy Lock — Spec

**Status:** Draft for review
**Decided:** freeze (both directions gated) · 30-min change window · **every lock-state change is email-gated** · native CLI OTP step-up

---

## 1. Motivation

Every governed-policy change today (`tighten`, `revoke`, and the planned `set`/`preset`) is
authenticated by the **API key alone** — a single factor. A leaked key can therefore *loosen*
governance (`policy revoke`) — or *sabotage* it by over-tightening — with no second check.

**Policy lock** makes changing a locked policy require a **second, out-of-band factor**: an
email step-up (paste an OTP code, or click a magic link). Combined with the existing email
step-up on **mode** (observe/enforce), it yields the property:

> With **enforce mode + a locked policy**, neither loosening enforcement nor *changing* policy
> (in either direction) is possible with a stolen API key alone — the attacker would also need
> the user's email inbox.

It is a self-binding ("Ulysses pact") control: commit to a policy and make backing out
deliberately hard.

---

## 2. Decisions (locked-in)

| Decision | Choice |
|---|---|
| Gate scope | **Freeze** — *both* tightening and loosening need step-up while locked (not a ratchet) |
| Unlock model | **Temporary window** — a verified step-up permits changes for **30 minutes**, then auto-relocks |
| Lock-state auth | **Email step-up for *every* lock-state change** — locking, unlocking (window), and removing the lock all require email confirmation |
| Re-lock early | **API key** — closing *your own* already-open window early is the only key-gated lock op |
| Lockout risk | **Structurally eliminated** — because locking *is* an email step-up, inbox access is proven *at lock time* |
| CLI step-up UX | **Native** — paste the OTP in the terminal (magic link as fallback), not a dashboard forward |

---

## 3. State model

Per **account** (alongside `enforce_mode` in `vaibotv2.profiles`; per-user later — see §9):

```
policy_locked        boolean        default false   -- durable lock state
policy_unlock_until  timestamptz    null            -- when set & in the future, changes are permitted
```

- **Locked** ⇔ `policy_locked = true`.
- **Change permitted right now** ⇔ `policy_locked = false` **OR** `now() < policy_unlock_until`.
- The lock is **durable**; the window is a **transient permit**. We never silently flip
  `policy_locked` to false on expiry — the window simply lapses, so the policy re-freezes
  with **zero background jobs** and no "forgot to re-lock" hole.
- Lock gates the *capability to change*, independent of whether a signed bundle is currently
  active (you can lock while on built-in defaults — it just means no changes without step-up).

**The one rule:** flipping `policy_locked` (either direction) **or** opening a window requires an
**email step-up**. Closing *your own* open window early is the only key-gated lock op.

---

## 4. State machine

```
            policy lock  (EMAIL step-up)
  UNLOCKED ─────────────────────────────────────────────▶ LOCKED
  locked=false                                            locked=true, unlock_until=null
      ▲                                                       │
      │ unlock --permanent  (EMAIL step-up)                   │ unlock  (EMAIL step-up)
      │                                                       ▼
      └──────────────────────────────────────── LOCKED + WINDOW OPEN
                                                 locked=true, unlock_until = now+30m
                                                    │   ▲
                       change cmds (API key) permit │   │ window lapses (now ≥ unlock_until)
                       while open  ─────────────────┘   │  → re-freezes automatically
                                                        │
                              policy lock (API key) ────┘  (close window early; locked stays true)
```

| From | Action | Auth | To |
|---|---|---|---|
| unlocked | `policy lock` | **email step-up** | locked |
| locked | `policy unlock` | **email step-up** | locked + 30-min window |
| locked + window | change cmd (`tighten`/`revoke`/`set`/`preset`) | API key | (applies) |
| locked + window | `policy lock` (close window early) | API key | locked (window cleared; still locked) |
| locked + window | window lapses | — | locked |
| locked (any) | `policy unlock --permanent` | **email step-up** | unlocked |

---

## 5. Auth model

**Locked means frozen, and the lock itself is email-gated in *both* directions.** Every change to
the durable lock state — turning it on, opening a change window, or removing it — requires an
email step-up. The only key-gated lock op is closing *your own* already-open window early (a
strictly-safer move you're already authorized for).

There is **no "free tighten" carve-out**: over-tightening is itself an attack vector — a
compromised-but-valid key (or a malicious insider) can deny-of-service the agent by tightening
the policy into uselessness — so a frozen policy treats both directions identically.

> Stolen keys are handled separately by **API-key revocation**. The lock makes the *policy*
> immutable without a second factor, full stop — it does not rely on a tighten-vs-loosen asymmetry
> (tightening is **not** "safe": it's a live DoS/tampering vector for a legitimate account).

| Operation | Auth |
|---|---|
| `policy lock` (turn the freeze on) | **email step-up** |
| `policy unlock` (open a 30-min change window) | **email step-up** |
| `policy unlock --permanent` (remove the freeze) | **email step-up** |
| any policy change (`tighten`/`revoke`/`set`/`preset`) | only inside an open window; key-gated for those 30 min |
| `policy lock` while a window is open (close it early) | API key |

**Lockout is structurally impossible.** Because locking *is* an email step-up, the user proves
inbox access at the moment they lock — so they can always unlock later. (Losing email access
*after* locking is the one residual case → §9.)

---

## 6. Server changes (`apps/api`)

### 6.1 State
Add `policy_locked` + `policy_unlock_until` to `vaibotv2.profiles`. `getUserGovernance()`
returns them with `plan` + `accountMode`.

### 6.2 Gate the existing change endpoints
`/v2/policy/request` (tighten) and `/v2/policy/revoke`, before applying:
```
if (locked && !(unlock_until && now < unlock_until))
    → 423 Locked  { ok:false, error:'policy_locked', unlock_with:'vaibot policy unlock' }
```
`423 Locked` (WebDAV) is semantically exact; the CLI maps it to a clear message.

### 6.3 Endpoints (reuse the enforcement step-up machinery)
Generalize the enforcement token with a `scope`/`action` field; reuse `signEnforcementToken` /
`deriveEnforcementCode` / `buildEnforcementConfirmUrl` / `verifyEnforcementToken` + the
`enforcementCodeAttempts` throttle. **One step-up flow** covers all three lock-state changes,
selected by `action`.

| Endpoint | Auth | Effect |
|---|---|---|
| `POST /v2/policy/stepup/activate` `{ action: 'lock' \| 'unlock' \| 'unlock_permanent' }` | API key | signs a scoped token (~10-min expiry); emails OTP **and** magic link → `{ token_id, sent_to:"b…@gmail.com" }`. No `email_claimed` flag needed — there's nowhere to send the code without an inbox, which **is** the lockout guard. |
| `POST /v2/policy/stepup/verify` `{ token_id, code }` | API key | verifies OTP (throttled); applies the action — `locked=true` / open 30-min window / `locked=false`; anchors `policy.lock` / `policy.unlock` / `policy.unlock_permanent` |
| `GET  /v2/policy/stepup/confirm?token=…` | token | magic-link target; same apply as verify; renders a small confirmation page |
| `POST /v2/policy/relock` | API key | requires locked + an open window; clears `unlock_until` (close window early); anchors `policy.relock` |

### 6.4 State read
Extend **`/v2/accounts/me`** (API-key readable) with `policy_locked` + `policy_unlock_until`,
so the CLI can show lock state without the dashboard-session-only `/v2/enforcement/state`.

### 6.5 Auditing
`policy.lock`, `policy.unlock`, `policy.unlock_permanent`, `policy.relock` are provenance-anchored
governance events (mirroring `enforcement.set_mode`), surfaced in `vaibot policy history`.

---

## 7. CLI (`vaibot policy …`)

```
policy lock                 # email step-up → freeze
policy unlock               # email step-up → 30-min change window
policy unlock --permanent   # email step-up → remove the lock entirely
policy show                 # now also prints the lock line
```

### 7.1 `policy lock` (native OTP; link fallback)
```
$ vaibot policy lock
  ▸ Emailed a confirmation code to b…@gmail.com (expires in 10 min).
  Paste the code (or press Enter to open the email link instead): 4F2K9X
  [ok]   Policy frozen. Any change now needs `vaibot policy unlock` first.
```
- **No claimed email** → there's no inbox to confirm with, so locking is refused with a pointer
  to claim one first. (This is also what makes lockout impossible: you can't lock without proving email.)
- **While a window is open**, `policy lock` instead **closes the window early** (API key, no step-up)
  — "Window closed; policy frozen again."

### 7.2 `policy unlock` (native OTP; link fallback)
```
$ vaibot policy unlock
  ▸ Emailed a confirmation code to b…@gmail.com (expires in 10 min).
  Paste the code (or press Enter to open the email link instead): 4F2K9X
  [ok]   Unlocked for 30 min. Make your changes, then `vaibot policy lock` (or it re-locks itself).
```
- **Empty input** → print the confirm URL + `open::that(url)`; then poll `GET /v2/accounts/me`
  up to ~2 min until `policy_unlock_until` is in the future, or tell the user to re-run.
- **Already in a window** → no email; prints "Already unlocked for Xm" (optional `--extend`).

### 7.3 Change while locked
`tighten` / `revoke` / `set` / `preset` map the `423`:
```
$ vaibot policy tighten 'curl | sh'
  [blocked] Policy is locked. Run `vaibot policy unlock` first (opens a 30-min window).
```

### 7.4 `policy unlock --permanent`
```
$ vaibot policy unlock --permanent
  ▸ Emailed a confirmation code to b…@gmail.com.
  Paste the code: 9Q7M2A
  [ok]   Lock removed. Policy changes are back to API-key only.
```

### 7.5 `policy show` lock line
```
  lock:      🔒 locked  (changes need `policy unlock`)
   — or —    🔓 unlocked for 12m (auto-relocks)
   — or —    unlocked
```

---

## 8. Edge cases (ironed out)

1. **Lockout impossible** — locking *is* an email step-up, so inbox access is proven at lock time;
   the user can always unlock later. (No `email_claimed` precondition needed — it's structural.)
2. **Window expiry** — checked at change-time (`now < unlock_until`); no cron. `show` computes remaining minutes.
3. **Close window early** — `policy lock` during an open window clears `unlock_until` (API key; you're
   inside your own authorized window). The one key-gated lock op.
4. **Double unlock** — `unlock` while a window is open is a no-op (no second email); reports remaining time.
5. **Scope isolation** — the step-up token carries `scope`+`action`; a `mode` token can't touch policy, and vice-versa.
6. **Throttling** — reuse `enforcementCodeAttempts` on `verify` (lock out after N bad codes; tokens expire ~10 min).
7. **Multi-key accounts** — lock + window are account-wide; a window the owner opens (via email) lets any
   key change until expiry. Each change still records the acting key (audit intact).
8. **Magic-link vs OTP** — OTP is the synchronous CLI path; the link is a browser fallback after which
   the user re-runs. Both hit the same server effect.
9. **Idempotent lock** — `lock` when already locked (no window) is a no-op success (no email sent).
10. **Lock with no active bundle** — allowed; it freezes the *change capability*, not a particular bundle.

---

## 9. Open questions / out of scope (v1)

- **Lost-email recovery.** Locking now *proves* inbox access at lock time (no never-had-email lockout),
  but losing email access *afterward* still strands a locked policy. v1 recovery = dashboard/support
  with stronger auth.
- **Per-user vs per-account.** Lock is per-account today (with `enforce_mode`). Per-user lock rides the
  deferred per-user-policy (MCP) work.
- **Locking *mode* too.** Out of scope; mode already has its own step-up. A future "freeze governance"
  could lock both axes in one action.
- **Window length config.** Fixed 30 min for v1; could become an account setting later.

---

## 10. Sequencing

Lock depends only on the **step-up-token generalization** + the `policy_locked` state — independent
of the flavors track.

1. **Server**: state columns → generalize the step-up token (`lock`/`unlock`/`unlock_permanent` actions)
   → gate `request`/`revoke` (423) → the `stepup` + `relock` endpoints → `/me` fields → anchoring.
2. **CLI**: `lock` / `unlock` / `unlock --permanent` / the 423 handling on change commands / the
   `show` lock line / the OTP-paste step-up flow.
3. **Then flavors** (`policy preset`) layer on top — "a chosen flavor + a locked policy" is the
   intended serious-user end-state.
