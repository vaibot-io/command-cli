//! `account` group — identity. `claim` links this machine's account to your real
//! (email-owned) account via the verified two-step: it requests the claim, and if
//! the email already has an account the server emails a 6-digit code, which you
//! enter to complete the merge. After it, the CLI's key operates as that account.

use clap::Subcommand;

use crate::error::CliError;

#[derive(Subcommand, Debug)]
pub enum AccountCmd {
    /// Link this machine to your real account (verified by an emailed code).
    Claim {
        /// Email to link. Omit to be prompted.
        #[arg(long)]
        email: Option<String>,
    },
}

pub async fn dispatch(cmd: AccountCmd, api_url: Option<String>) -> Result<(), CliError> {
    match cmd {
        AccountCmd::Claim { email } => crate::commands::setup::claim_command(email, api_url).await,
    }
}
