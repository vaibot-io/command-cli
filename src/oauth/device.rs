//! RFC 8628 device-authorization flow (`--device`, or auto-fallback on headless).
//!
//! Prints the verification URI + user code (and `verification_uri_complete` if
//! present), then polls the token endpoint honoring `interval` / `slow_down`.
//! Does NOT auto-open a browser — the user authorizes on a SECOND device.
//!
//! Network calls happen only when this fn runs.

use oauth2::{
    ClientId, DeviceAuthorizationResponse, EmptyExtraDeviceAuthorizationFields, Scope,
    TokenResponse,
};

use super::discovery::{build_client, device_auth_url, discover, http_client};
use super::loopback::token_set_from;
use super::tokens::TokenSet;
use super::{oauth_err, CLI_CLIENT_ID};
use crate::error::CliError;

/// Run the device-code flow.
pub async fn login(
    issuer: &str,
    scope: &str,
    print: &(dyn for<'a> Fn(&'a str) + Sync),
) -> Result<TokenSet, CliError> {
    let meta = discover(issuer).await?;
    let device_url = device_auth_url(&meta)?;
    let client = build_client(ClientId::new(CLI_CLIENT_ID.to_string()), &meta, None)?
        .set_device_authorization_url(device_url);

    let http = http_client()?;

    let details: DeviceAuthorizationResponse<EmptyExtraDeviceAuthorizationFields> = client
        .exchange_device_code()
        .add_scopes(scope.split_whitespace().map(|s| Scope::new(s.to_string())))
        .request_async(&http)
        .await
        .map_err(|e| oauth_err("device authorization", e))?;

    print(&format!(
        "\nTo authorize, visit:\n\n  {}\n\nand enter code: {}",
        details.verification_uri().as_str(),
        details.user_code().secret()
    ));
    if let Some(complete) = details.verification_uri_complete() {
        print(&format!("\nOr open this URL directly:\n\n  {}\n", complete.secret()));
    }
    print("\nWaiting for authorization...");

    let token = client
        .exchange_device_access_token(&details)
        .request_async(&http, tokio::time::sleep, None)
        .await
        .map_err(|e| oauth_err("device token poll", e))?;

    let _ = token.access_token(); // touch to keep import meaningful
    Ok(token_set_from(token))
}
