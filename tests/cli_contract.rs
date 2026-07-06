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
const STUBS: &[(&str, &[&str])] = &[
    ("update", &["update"]),
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
