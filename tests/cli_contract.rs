//! End-to-end contract tests for the `vaibot` binary: the stub-noun set (exit 2
//! with the canonical line), the pinned version string, and the
//! installer-not-runtime guardrail for `gateway serve` with no binary.
//!
//! These drive the built binary, but make NO network calls — every asserted path
//! is a stub, a usage error, or a binary-not-found message that returns before
//! any request.

use assert_cmd::Command;
use predicates::prelude::*;

fn vaibot() -> Command {
    let mut c = Command::cargo_bin("vaibot").expect("binary builds");
    // Bypass the production-environment gate so these contract tests stay
    // hermetic (no /v2/accounts/me call) and exercise the command path itself.
    c.env("VAIBOT_ADMIN_OVERRIDE", "1");
    c
}

#[test]
fn version_matches_crate() {
    // `--version` now tracks the crate version (was pinned to "0.3.0").
    vaibot()
        .arg("--version")
        .assert()
        .success()
        .stdout(predicate::str::contains(env!("CARGO_PKG_VERSION")));
}

/// noun ⇒ args. Every entry must exit 2 with the canonical "not yet wired" line.
// `update` was a stub until 0.6.2 implemented self-update. It is deliberately NOT
// listed here any more: leaving it in made this test fail on main, and it also made
// the suite reach out to crates.io on every run.
const STUBS: &[(&str, &[&str])] = &[
    ("guard verify", &["guard", "verify"]),
    ("guard provision-offline", &["guard", "provision-offline"]),
    ("provenance anchor", &["provenance", "anchor"]),
];

#[test]
fn every_stub_exits_2_with_canonical_line() {
    for (noun, args) in STUBS {
        let expected = format!(
            "vaibot {noun} is not yet wired — see {noun} (tracked; this is an orchestrator stub)"
        );
        vaibot()
            .args(*args)
            .assert()
            .code(2)
            .stderr(predicate::str::contains(expected));
    }
}

#[test]
fn gateway_serve_without_binary_prints_model_and_exits_nonzero() {
    // Force "no binary": point the override at a non-existent path and ensure
    // PATH lookup also fails by clearing PATH for this invocation.
    vaibot()
        .args(["gateway", "serve"])
        .env("VAIBOT_GATEWAY_BIN", "/nonexistent/vaibot-gateway-xyz")
        .env("PATH", "/nonexistent-bin-dir")
        .assert()
        .code(1)
        .stderr(predicate::str::contains("vaibot-gateway binary not found"))
        .stderr(predicate::str::contains("ANTHROPIC_BASE_URL"));
}

#[test]
fn receipts_is_an_alias_of_provenance() {
    // `receipts --help` should list the same subcommands as provenance.
    vaibot()
        .args(["receipts", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("list"))
        .stdout(predicate::str::contains("tail"))
        .stdout(predicate::str::contains("anchor"));
}

// ── `vaibot update` ──────────────────────────────────────────────────────────
//
// This command's meaning changed: it used to update only the CLI, and now updates the
// whole installation. These pin the contract WITHOUT running it — an actual `vaibot
// update` would reinstall the guard and every plugin on the machine, which is not
// something a test suite should do to whoever runs it.

#[test]
fn update_help_says_it_updates_everything() {
    vaibot()
        .args(["update", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("guard"))
        .stdout(predicate::str::contains("plugin"))
        .stdout(predicate::str::contains("CLI"));
}

#[test]
fn update_offers_a_way_back_to_the_old_behaviour() {
    // Someone may have scripted `vaibot update` expecting a CLI-only self-update.
    // --cli-only is that, and the help says so in those words.
    vaibot()
        .args(["update", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("--cli-only"))
        .stdout(predicate::str::contains("before this release"));
}

#[test]
fn update_has_a_skip_for_each_component() {
    let out = vaibot().args(["update", "--help"]).output().expect("runs");
    let text = String::from_utf8_lossy(&out.stdout);
    for flag in ["--skip-guard", "--skip-plugins", "--skip-cli"] {
        assert!(text.contains(flag), "missing {flag} in update --help");
    }
}

#[test]
fn cli_only_cannot_be_combined_with_the_skips() {
    // Nonsense combinations should be refused by argument parsing, before anything
    // reinstalls itself. Each pairing is checked, not just one.
    for other in ["--skip-guard", "--skip-plugins", "--skip-cli"] {
        vaibot()
            .args(["update", "--cli-only", other])
            .assert()
            .failure()
            .stderr(predicate::str::contains("cannot be used with"));
    }
}

// ── `vaibot guard update` ────────────────────────────────────────────────────
//
// The guard is shared across every host, and until now it could only be updated as a
// side effect of `vaibot plugin update <host>`. These pin that it has a command of its
// own, in the group where someone would look for it. As above, nothing here RUNS the
// update — that would reinstall the guard on whoever's machine runs the suite.

#[test]
fn guard_group_lists_update() {
    vaibot()
        .args(["guard", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("update"));
}

#[test]
fn guard_update_is_a_real_subcommand() {
    // An unknown guard subcommand is a usage error; `update` must not be one. This
    // fails if the variant is added to the enum but never wired into the dispatch.
    vaibot()
        .args(["guard", "update", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("guard"));

    vaibot()
        .args(["guard", "updaet"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("unrecognized subcommand"));
}

/// The long flags in a `--help` Options section.
fn long_flags(args: &[&str]) -> std::collections::BTreeSet<String> {
    let out = vaibot().args(args).output().expect("runs");
    let text = String::from_utf8_lossy(&out.stdout);
    text.split("Options:")
        .nth(1)
        .unwrap_or("")
        .split_whitespace()
        .filter(|w| w.starts_with("--"))
        .map(|w| w.trim_end_matches(',').to_string())
        .collect()
}

#[test]
fn guard_update_takes_no_flags_of_its_own() {
    // It does one thing. A flag here would mean the contract drifted from the design,
    // where anything selective belongs to `vaibot update`. Compared against the
    // top-level globals rather than a hardcoded list, so adding a global does not
    // fail this and adding a flag to `guard update` does.
    let globals = long_flags(&["--help"]);
    assert!(globals.contains("--api-url"), "global set looks wrong: {globals:?}");

    let own: Vec<_> = long_flags(&["guard", "update", "--help"])
        .difference(&globals)
        .cloned()
        .collect();
    assert!(own.is_empty(), "guard update grew flags of its own: {own:?}");
}

