//! Policy signing keypair generation for the operator (self-host).
//!
//! The CLI does NOT sign — it generates the Ed25519 keypair (via `openssl`) so
//! the operator can install the PRIVATE key as the server's
//! `VAIBOT_POLICY_SIGNING_KEY` and pin the PUBLIC key (`VAIBOT_POLICY_PUBKEY`) on
//! guards. Private + public are ALWAYS regenerated together — never a
//! stale-pubkey outlier.
//!
//! Overwriting an existing key is a holistic ROTATION. It does NOT invalidate
//! receipts or proofs (those reference policies by content hash, independent of
//! the signature). What it requires: re-install the new private key on the
//! server and re-sign the active policy, and re-pin the new public key on every
//! guard — until then they fail CLOSED to the built-in floor. `vaibot doctor`
//! flags a half-finished rotation.

use std::path::{Path, PathBuf};

use crate::config::creds::VaibotEnv;
use crate::error::CliError;

#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub enum KeygenAction {
    /// No key present — create one.
    Generate,
    /// A key exists and the user confirmed a rotation.
    Overwrite,
    /// A key exists; leave it untouched.
    Keep,
}

/// Decide the action from (key exists, `--yes`, interactive confirm).
///
/// Safety: an existing key is NEVER clobbered non-interactively. `--yes` keeps
/// it; only an explicit interactive confirmation overwrites (rotation). `confirm`
/// is evaluated only on the interactive existing-key path.
pub fn keygen_decision(key_exists: bool, yes: bool, confirm: impl FnOnce() -> bool) -> KeygenAction {
    if !key_exists {
        return KeygenAction::Generate;
    }
    if yes {
        return KeygenAction::Keep;
    }
    if confirm() {
        KeygenAction::Overwrite
    } else {
        KeygenAction::Keep
    }
}

fn env_label(env: VaibotEnv) -> &'static str {
    match env {
        VaibotEnv::Production => "prod",
        VaibotEnv::Staging => "staging",
    }
}

fn fly_app(env: VaibotEnv) -> &'static str {
    match env {
        VaibotEnv::Production => "vaibot-api-v2",
        VaibotEnv::Staging => "vaibot-api-still-silence-9697",
    }
}

pub fn keys_dir() -> PathBuf {
    directories::BaseDirs::new()
        .map(|b| b.home_dir().join(".vaibot").join("keys"))
        .unwrap_or_else(|| PathBuf::from(".vaibot/keys"))
}

pub fn private_key_path(env: VaibotEnv) -> PathBuf {
    keys_dir().join(format!("{}-policy-private.pem", env_label(env)))
}

pub fn public_key_path(env: VaibotEnv) -> PathBuf {
    keys_dir().join(format!("{}-policy-public.pem", env_label(env)))
}

/// The real generator: `openssl` produces BOTH PEMs (private + public together),
/// then the private key is chmod'd 0600.
fn openssl_generate(priv_path: &Path, pub_path: &Path) -> Result<(), CliError> {
    use std::process::Command;
    let gen = Command::new("openssl")
        .args(["genpkey", "-algorithm", "ed25519", "-out"])
        .arg(priv_path)
        .status()
        .map_err(|e| CliError::Runtime(format!("openssl genpkey failed (is openssl installed?): {e}")))?;
    if !gen.success() {
        return Err(CliError::Runtime("openssl genpkey returned non-zero".into()));
    }
    let pubout = Command::new("openssl")
        .arg("pkey")
        .arg("-in")
        .arg(priv_path)
        .arg("-pubout")
        .arg("-out")
        .arg(pub_path)
        .status()
        .map_err(|e| CliError::Runtime(format!("openssl pkey -pubout failed: {e}")))?;
    if !pubout.success() {
        return Err(CliError::Runtime("openssl pkey -pubout returned non-zero".into()));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(priv_path, std::fs::Permissions::from_mode(0o600));
    }
    Ok(())
}

/// Core orchestration with the generator injected (so the clobber logic is
/// testable without openssl). Creates the keys dir + regenerates BOTH PEMs on
/// Generate/Overwrite; a no-op on Keep.
pub fn run_keygen(
    priv_path: &Path,
    pub_path: &Path,
    yes: bool,
    confirm: impl FnOnce() -> bool,
    generator: impl FnOnce(&Path, &Path) -> Result<(), CliError>,
) -> Result<KeygenAction, CliError> {
    let action = keygen_decision(priv_path.exists(), yes, confirm);
    if matches!(action, KeygenAction::Generate | KeygenAction::Overwrite) {
        if let Some(parent) = priv_path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| CliError::Runtime(format!("create {}: {e}", parent.display())))?;
        }
        generator(priv_path, pub_path)?;
    }
    Ok(action)
}

/// `vaibot init` step: generate (or, with confirmation, rotate) the policy
/// signing keypair. Auto — runs on every init; keeps an existing key unless the
/// user explicitly confirms a rotation.
pub fn ensure_signing_key(env: VaibotEnv, yes: bool) -> Result<(), CliError> {
    let priv_path = private_key_path(env);
    let pub_path = public_key_path(env);
    let action = run_keygen(&priv_path, &pub_path, yes, || confirm_rotation(&priv_path), openssl_generate)?;
    report(action, env, &priv_path, &pub_path);
    Ok(())
}

/// Print the rotation warning (true consequences) and prompt — default NO.
fn confirm_rotation(priv_path: &Path) -> bool {
    println!("\n[warn] A policy signing key already exists at {}.", priv_path.display());
    println!("       Overwriting ROTATES the key:");
    println!("         • existing signed policies stop verifying until the new PUBLIC key is");
    println!("           re-pinned on every guard (they fail CLOSED to the built-in floor meanwhile);");
    println!("         • you must re-install the new private key on the server and re-sign the");
    println!("           active policy. `vaibot doctor` flags a half-finished rotation.");
    println!("       It does NOT invalidate prior receipts or proofs (those reference policies by");
    println!("       content hash, independent of the signature).");
    prompt_overwrite()
}

/// y/N prompt, default NO (destructive op).
fn prompt_overwrite() -> bool {
    use std::io::{self, Write};
    print!("       Overwrite the existing key? [y/N]: ");
    let _ = io::stdout().flush();
    let mut line = String::new();
    if io::stdin().read_line(&mut line).is_err() {
        return false;
    }
    matches!(line.trim().to_ascii_lowercase().as_str(), "y" | "yes")
}

fn report(action: KeygenAction, env: VaibotEnv, priv_path: &Path, pub_path: &Path) {
    match action {
        KeygenAction::Keep => {
            println!("[info] Keeping the existing {} policy signing key ({}).", env_label(env), priv_path.display());
        }
        KeygenAction::Generate | KeygenAction::Overwrite => {
            let verb = if action == KeygenAction::Overwrite { "Rotated" } else { "Generated" };
            println!("[ok]   {verb} the {} policy signing keypair (private + public):", env_label(env));
            println!("  private: {}", priv_path.display());
            println!("  public:  {}", pub_path.display());
            println!("\n  Install on your API + pin on guards (run in your own terminal — keeps the key out of logs):");
            println!("    fly secrets set VAIBOT_POLICY_SIGNING_KEY=\"$(cat {})\" -a {}", priv_path.display(), fly_app(env));
            println!("    # pin the public key on guards: VAIBOT_POLICY_PUBKEY=\"$(cat {})\"", pub_path.display());
            if action == KeygenAction::Overwrite {
                println!("\n  [rotation] After the server has the new key, complete it:");
                println!("    vaibot policy preset <flavor>   # re-sign the active policy with the new key");
                println!("    vaibot doctor                   # verify the rotation is complete");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU32, Ordering};

    // ── keygen_decision: the clobber-safety matrix ──
    #[test]
    fn absent_key_generates() {
        assert_eq!(keygen_decision(false, false, || panic!("confirm must not run")), KeygenAction::Generate);
        assert_eq!(keygen_decision(false, true, || panic!("confirm must not run")), KeygenAction::Generate);
    }

    #[test]
    fn existing_key_with_yes_is_kept_never_clobbered() {
        assert_eq!(keygen_decision(true, true, || panic!("confirm must not run under --yes")), KeygenAction::Keep);
    }

    #[test]
    fn existing_key_interactive_confirm_yes_overwrites() {
        assert_eq!(keygen_decision(true, false, || true), KeygenAction::Overwrite);
    }

    #[test]
    fn existing_key_interactive_confirm_no_keeps() {
        assert_eq!(keygen_decision(true, false, || false), KeygenAction::Keep);
    }

    // ── run_keygen: the side-effect wiring, with an injected generator ──
    static SEQ: AtomicU32 = AtomicU32::new(0);
    fn tmp_paths() -> (PathBuf, PathBuf) {
        let n = SEQ.fetch_add(1, Ordering::SeqCst);
        let dir = std::env::temp_dir().join(format!("vaibot-keygen-test-{}-{n}", std::process::id()));
        (dir.join("policy-private.pem"), dir.join("policy-public.pem"))
    }
    fn stub(priv_path: &Path, pub_path: &Path) -> Result<(), CliError> {
        std::fs::write(priv_path, "NEW-PRIVATE").unwrap();
        std::fs::write(pub_path, "NEW-PUBLIC").unwrap();
        Ok(())
    }

    #[test]
    fn generates_both_pems_when_absent() {
        let (pk, pubk) = tmp_paths();
        let action = run_keygen(&pk, &pubk, false, || true, stub).unwrap();
        assert_eq!(action, KeygenAction::Generate);
        assert_eq!(std::fs::read_to_string(&pk).unwrap(), "NEW-PRIVATE");
        assert_eq!(std::fs::read_to_string(&pubk).unwrap(), "NEW-PUBLIC");
        let _ = std::fs::remove_dir_all(pk.parent().unwrap());
    }

    #[test]
    fn keeps_existing_under_yes_without_calling_generator() {
        let (pk, pubk) = tmp_paths();
        std::fs::create_dir_all(pk.parent().unwrap()).unwrap();
        std::fs::write(&pk, "OLD-PRIVATE").unwrap();
        let action = run_keygen(&pk, &pubk, true, || panic!("confirm must not run"), |_, _| panic!("generator must not run")).unwrap();
        assert_eq!(action, KeygenAction::Keep);
        assert_eq!(std::fs::read_to_string(&pk).unwrap(), "OLD-PRIVATE"); // untouched
        let _ = std::fs::remove_dir_all(pk.parent().unwrap());
    }

    #[test]
    fn declining_overwrite_keeps_existing() {
        let (pk, pubk) = tmp_paths();
        std::fs::create_dir_all(pk.parent().unwrap()).unwrap();
        std::fs::write(&pk, "OLD-PRIVATE").unwrap();
        let action = run_keygen(&pk, &pubk, false, || false, |_, _| panic!("generator must not run")).unwrap();
        assert_eq!(action, KeygenAction::Keep);
        assert_eq!(std::fs::read_to_string(&pk).unwrap(), "OLD-PRIVATE");
        let _ = std::fs::remove_dir_all(pk.parent().unwrap());
    }

    #[test]
    fn confirming_overwrite_rotates_both_pems() {
        let (pk, pubk) = tmp_paths();
        std::fs::create_dir_all(pk.parent().unwrap()).unwrap();
        std::fs::write(&pk, "OLD-PRIVATE").unwrap();
        std::fs::write(&pubk, "OLD-PUBLIC").unwrap();
        let action = run_keygen(&pk, &pubk, false, || true, stub).unwrap();
        assert_eq!(action, KeygenAction::Overwrite);
        assert_eq!(std::fs::read_to_string(&pk).unwrap(), "NEW-PRIVATE"); // rotated
        assert_eq!(std::fs::read_to_string(&pubk).unwrap(), "NEW-PUBLIC"); // together — no stale outlier
        let _ = std::fs::remove_dir_all(pk.parent().unwrap());
    }

    #[test]
    fn key_paths_are_env_namespaced() {
        assert!(private_key_path(VaibotEnv::Production).to_string_lossy().ends_with("prod-policy-private.pem"));
        assert!(public_key_path(VaibotEnv::Staging).to_string_lossy().ends_with("staging-policy-public.pem"));
    }
}
