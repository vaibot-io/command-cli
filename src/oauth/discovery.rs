//! OIDC discovery + oauth2 client construction.
//!
//! `oauth2` v5 has no built-in OIDC discovery (that lives in `openidconnect`),
//! so we fetch `<issuer>/.well-known/openid-configuration` ourselves with a
//! rustls reqwest client and feed the endpoints into an `oauth2::Client`. We
//! also declare an `id_token` extra field so the token response carries the
//! id_token we decode for `sub`/`email`.
//!
//! NETWORK GATE: `discover` is the only fn that performs a request, and it is
//! only called from inside the login/refresh flows.

use std::time::Duration;

use oauth2::basic::{BasicErrorResponse, BasicRevocationErrorResponse, BasicTokenIntrospectionResponse, BasicTokenType};
use oauth2::{
    AuthUrl, Client, ClientId, DeviceAuthorizationUrl, EndpointNotSet, EndpointSet, RedirectUrl,
    StandardRevocableToken, StandardTokenResponse, TokenUrl,
};
use serde::Deserialize;

use super::oauth_err;
use crate::error::CliError;

/// Discovered issuer metadata (only the fields we use).
#[derive(Debug, Clone, Deserialize)]
pub struct IssuerMeta {
    pub authorization_endpoint: String,
    pub token_endpoint: String,
    #[serde(default)]
    pub device_authorization_endpoint: Option<String>,
}

/// Extra token-response fields — we want the OIDC `id_token`.
#[derive(Debug, Clone, Deserialize, serde::Serialize)]
pub struct ExtraFields {
    #[serde(default)]
    pub id_token: Option<String>,
}

impl oauth2::ExtraTokenFields for ExtraFields {}

/// The concrete token-response type carrying our extra fields.
pub type CliTokenResponse = StandardTokenResponse<ExtraFields, BasicTokenType>;

/// The fully-typed oauth2 client with auth + token endpoints set.
pub type CliClient = Client<
    BasicErrorResponse,
    CliTokenResponse,
    BasicTokenIntrospectionResponse,
    StandardRevocableToken,
    BasicRevocationErrorResponse,
    EndpointSet,      // has auth url
    EndpointNotSet,   // device auth optional, set later
    EndpointNotSet,   // introspection
    EndpointNotSet,   // revocation
    EndpointSet,      // has token url
>;

/// A rustls-backed reqwest client suitable for the oauth2 async http calls.
pub fn http_client() -> Result<reqwest::Client, CliError> {
    reqwest::Client::builder()
        // Disable redirects per oauth2's recommendation (SSRF safety).
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(30))
        .build()
        .map_err(|e| oauth_err("http client", e))
}

/// Fetch `<issuer>/.well-known/openid-configuration`.
pub async fn discover(issuer: &str) -> Result<IssuerMeta, CliError> {
    let url = format!("{}/.well-known/openid-configuration", issuer.trim_end_matches('/'));
    let http = http_client()?;
    let resp = http
        .get(&url)
        .send()
        .await
        .map_err(|e| oauth_err("discovery request", e))?;
    if !resp.status().is_success() {
        return Err(CliError::Runtime(format!(
            "discovery failed: HTTP {} from {url}",
            resp.status()
        )));
    }
    resp.json::<IssuerMeta>()
        .await
        .map_err(|e| oauth_err("discovery parse", e))
}

/// Build the oauth2 client from discovered metadata.
pub fn build_client(
    client_id: ClientId,
    meta: &IssuerMeta,
    redirect: Option<RedirectUrl>,
) -> Result<CliClient, CliError> {
    let auth = AuthUrl::new(meta.authorization_endpoint.clone())
        .map_err(|e| oauth_err("auth url", e))?;
    let token = TokenUrl::new(meta.token_endpoint.clone())
        .map_err(|e| oauth_err("token url", e))?;
    let mut client = Client::new(client_id)
        .set_auth_uri(auth)
        .set_token_uri(token);
    if let Some(r) = redirect {
        client = client.set_redirect_uri(r);
    }
    Ok(client)
}

/// Build the device-authorization endpoint URL from metadata, if advertised.
pub fn device_auth_url(meta: &IssuerMeta) -> Result<DeviceAuthorizationUrl, CliError> {
    let ep = meta
        .device_authorization_endpoint
        .as_ref()
        .ok_or_else(|| CliError::Runtime("issuer does not advertise a device_authorization_endpoint".into()))?;
    DeviceAuthorizationUrl::new(ep.clone()).map_err(|e| oauth_err("device auth url", e))
}
