//! vaibot CLI entrypoint.
//!
//! Parse the clap tree, init tracing, dispatch, and map the result to the
//! documented exit codes. CliError carries its own message + code (including the
//! canonical StubError line at exit 2); any other failure is a code-1 fatal.
//! clap's own usage errors keep clap's exit 2 — which coincides with Stub,
//! matching the TS contract where citty usage + STUB are both 2.

use clap::Parser;
use tracing_subscriber::EnvFilter;

use vaibot::cli::Cli;
use vaibot::dispatch;
use vaibot::error::CliError;

#[tokio::main]
async fn main() {
    init_tracing();
    let cli = Cli::parse();
    if let Err(e) = dispatch(cli).await {
        handle_error(&e);
    }
}

fn init_tracing() {
    let filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new("warn,vaibot=info"));
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_target(false)
        .with_writer(std::io::stderr)
        .init();
}

/// Print the error message and exit with its mapped code. CliError owns its
/// wording (StubError prints the canonical "not yet wired" line at exit 2).
fn handle_error(e: &CliError) -> ! {
    eprintln!("{e}");
    std::process::exit(e.exit_code() as i32);
}
