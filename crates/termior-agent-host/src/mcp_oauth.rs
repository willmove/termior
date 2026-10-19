//! OAuth 2.1 authorization for remote MCP Streamable HTTP servers.
//!
//! Discovery follows RFC 9728 (Protected Resource Metadata) and RFC 8414
//! (Authorization Server Metadata), with the OpenID Connect metadata document
//! as a fallback. Login is a public-client authorization-code flow with S256
//! PKCE (RFC 7636) and a resource indicator (RFC 8707). The loopback callback
//! must present the same `state` that was issued, and an echoed `resource`
//! must match the MCP server. Access and refresh tokens are stored only
//! through [`termior_ai::SecretStore`] (the UI passes `KeyringSecretStore`).
//! Catalog files, diagnostic logs, and error strings receive redacted text.

use base64::Engine;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::io::{Read, Write};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use url::Url;

use termior_ai::SecretStore;

use crate::mcp::{valid_server_id, OAuthPkceRequest, MCP_PROTOCOL_VERSION};
use crate::BackendError;

pub const MCP_AUTH_DETAIL_REFRESH_REJECTED: &str = "refresh_rejected";
pub const MCP_AUTH_DETAIL_REVOKE_FAILED: &str = "revoke_failed";

const METADATA_LIMIT: usize = 64 * 1024;
const TOKEN_REFRESH_SKEW_SECS: u64 = 30;
const MIN_SECRET_LEN: usize = 8;

pub type McpClock = Arc<dyn Fn() -> SystemTime + Send + Sync>;

pub fn system_clock() -> McpClock {
    Arc::new(SystemTime::now)
}

pub fn mcp_servers_path(data_dir: &Path) -> PathBuf {
    data_dir.join("mcp-servers.json")
}

pub fn mcp_oauth_key(server_id: &str) -> String {
    format!("mcp-oauth:{server_id}")
}

pub fn oauth_code_challenge_s256(verifier: &str) -> String {
    let digest = Sha256::digest(verifier.as_bytes());
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(digest)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum McpLoginStatus {
    #[default]
    LoggedOut,
    LoggedIn,
    ReLoginRequired,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct McpRemoteServer {
    pub id: String,
    pub url: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub oauth_key_ref: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub client_id: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub scopes: Vec<String>,
    #[serde(default)]
    pub auth_status: McpLoginStatus,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub auth_detail: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct McpServerCatalog {
    #[serde(default)]
    pub servers: Vec<McpRemoteServer>,
}

impl McpServerCatalog {
    pub fn load(path: &Path) -> Result<Self, BackendError> {
        if !path.exists() {
            return Ok(Self::default());
        }
        let text = std::fs::read_to_string(path)
            .map_err(|_| BackendError::Transport("MCP server list could not be read".into()))?;
        serde_json::from_str(&text)
            .map_err(|_| BackendError::Protocol("MCP server list is invalid".into()))
    }

    pub fn save(&self, path: &Path, secrets: &[String]) -> Result<(), BackendError> {
        let json = serde_json::to_string_pretty(self)
            .map_err(|_| BackendError::Protocol("MCP server list could not be encoded".into()))?;
        if contains_credential(&json, secrets) {
            return Err(BackendError::Protocol(
                "refusing to write MCP server list because it contains credential material".into(),
            ));
        }
        termior_store::atomic_write(path, &json)
            .map_err(|_| BackendError::Transport("MCP server list could not be saved".into()))
    }
}

#[derive(Clone)]
pub struct McpOAuthBinding {
    pub server_id: String,
    pub store: Arc<dyn SecretStore>,
    pub catalog_path: PathBuf,
    pub now: McpClock,
    pub diagnostic_log: Option<PathBuf>,
}

impl std::fmt::Debug for McpOAuthBinding {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("McpOAuthBinding")
            .field("server_id", &self.server_id)
            .field("catalog_path", &self.catalog_path)
            .finish_non_exhaustive()
    }
}

#[derive(Debug, Clone)]
pub struct PkceLoginOptions {
    pub binding: McpOAuthBinding,
    pub client_id: Option<String>,
    pub scopes: Vec<String>,
    pub timeout: Duration,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiscoveredAuthorization {
    pub resource: String,
    pub issuer: String,
    pub authorization_endpoint: String,
    pub token_endpoint: String,
    pub revocation_endpoint: Option<String>,
    pub registration_endpoint: Option<String>,
    pub scopes_supported: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct McpLogoutReport {
    pub remote_revoked: bool,
}

pub(crate) struct PreparedAccess {
    pub access: String,
    pub secrets: Vec<String>,
    pub refreshed: bool,
}

pub(crate) fn allowed_mcp_http_url(value: &str) -> bool {
    let Ok(url) = Url::parse(value) else {
        return false;
    };
    match url.scheme() {
        "https" => url.host().is_some(),
        "http" => match url.host() {
            Some(url::Host::Domain(host)) => host.eq_ignore_ascii_case("localhost"),
            Some(url::Host::Ipv4(ip)) => ip.is_loopback(),
            _ => false,
        },
        _ => false,
    }
}

pub(crate) fn blocking_http() -> Result<reqwest::blocking::Client, BackendError> {
    reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(30))
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .map_err(|_| BackendError::Transport("MCP HTTP client could not be built".into()))
}

pub fn discover_authorization(mcp_endpoint: &str) -> Result<DiscoveredAuthorization, BackendError> {
    let http = blocking_http()?;
    discover_with(&http, mcp_endpoint)
}

pub fn login_with_browser(
    mcp_endpoint: &str,
    options: &PkceLoginOptions,
    opener: &dyn Fn(&str) -> Result<(), String>,
) -> Result<(), BackendError> {
    let binding = &options.binding;
    if !valid_server_id(&binding.server_id) {
        return Err(BackendError::Protocol(
            "MCP server id is not a safe identifier".into(),
        ));
    }
    if !allowed_mcp_http_url(mcp_endpoint) {
        return Err(BackendError::Transport(
            "remote MCP endpoint must use HTTPS".into(),
        ));
    }
    let http = blocking_http()?;
    let discovered = discover_with(&http, mcp_endpoint)?;
    let listener = TcpListener::bind("127.0.0.1:0")
        .map_err(|_| BackendError::Transport("MCP authorization callback could not bind".into()))?;
    listener
        .set_nonblocking(true)
        .map_err(|_| BackendError::Transport("MCP authorization callback could not bind".into()))?;
    let port = listener
        .local_addr()
        .map_err(|_| BackendError::Transport("MCP authorization callback could not bind".into()))?
        .port();
    let redirect_uri = format!("http://127.0.0.1:{port}/callback");
    let client_id = resolve_client_id(&http, options, &discovered, &redirect_uri)?;
    write_server_state(
        binding,
        Some(mcp_endpoint),
        Some(client_id.clone()),
        Some(options.scopes.clone()),
        McpLoginStatus::LoggedOut,
        "",
        &[],
    )?;
    let verifier = random_token(64)?;
    let state = random_token(32)?;
    let scope = choose_scope(&options.scopes, &discovered.scopes_supported)?;
    let request = OAuthPkceRequest {
        authorization_endpoint: discovered.authorization_endpoint.clone(),
        resource: discovered.resource.clone(),
        code_challenge: oauth_code_challenge_s256(&verifier),
        state,
    };
    let auth_url = authorization_url(&request, &client_id, &redirect_uri, &scope)?;
    let timeout = if options.timeout.is_zero() {
        Duration::from_secs(180)
    } else {
        options.timeout
    };
    let cancel = Arc::new(AtomicBool::new(false));
    let expected = request.clone();
    let cancel_thread = Arc::clone(&cancel);
    let thread =
        std::thread::spawn(move || accept_callback(listener, &expected, timeout, &cancel_thread));
    if let Err(error) = opener(&auth_url) {
        cancel.store(true, Ordering::SeqCst);
        let _ = thread.join();
        return Err(BackendError::Unavailable(public_message(&error)));
    }
    let code = match thread.join() {
        Ok(Ok(code)) => code,
        Ok(Err(error)) => return Err(error),
        Err(_) => {
            return Err(BackendError::Unavailable(
                "MCP authorization callback failed".into(),
            ))
        }
    };
    let now = unix_now((binding.now)());
    let tokens = exchange_code(
        &http,
        &discovered,
        &CodeExchange {
            client_id: &client_id,
            redirect_uri: &redirect_uri,
            verifier: &verifier,
            code: &code,
            scope: &scope,
            now,
        },
    )?;
    let secrets = tokens.secret_values();
    let key = key_for_binding(binding);
    save_tokens(&*binding.store, &key, &tokens)?;
    if let Err(error) = write_server_state(
        binding,
        Some(&discovered.resource),
        Some(client_id),
        None,
        McpLoginStatus::LoggedIn,
        "",
        &secrets,
    ) {
        let _ = binding.store.delete(&key);
        return Err(error);
    }
    log_auth(
        &binding.diagnostic_log,
        &secrets,
        &format!("mcp oauth sign-in completed for {}", binding.server_id),
    );
    Ok(())
}

pub fn logout_mcp_server(binding: &McpOAuthBinding) -> Result<McpLogoutReport, BackendError> {
    if !valid_server_id(&binding.server_id) {
        return Err(BackendError::Protocol(
            "MCP server id is not a safe identifier".into(),
        ));
    }
    let key = key_for_binding(binding);
    let record = load_tokens(&*binding.store, &key).unwrap_or_default();
    let secrets = record
        .as_ref()
        .map(StoredMcpTokens::secret_values)
        .unwrap_or_default();
    let mut remote_revoked = true;
    if let Some(record) = record.as_ref() {
        if record.revocation_endpoint.is_some() {
            let http = blocking_http()?;
            remote_revoked = revoke_remote(&http, record);
        }
    }
    if binding.store.delete(&key).is_err() && record.is_some() {
        return Err(BackendError::Unavailable(
            "MCP credential store could not delete the token".into(),
        ));
    }
    let detail = if remote_revoked {
        ""
    } else {
        MCP_AUTH_DETAIL_REVOKE_FAILED
    };
    write_server_state(
        binding,
        None,
        None,
        None,
        McpLoginStatus::LoggedOut,
        detail,
        &secrets,
    )?;
    log_auth(
        &binding.diagnostic_log,
        &secrets,
        &format!("mcp oauth signed out {}", binding.server_id),
    );
    Ok(McpLogoutReport { remote_revoked })
}

pub(crate) fn prepare_access_token(
    http: &reqwest::blocking::Client,
    binding: &McpOAuthBinding,
    force: bool,
    endpoint: &str,
) -> Result<PreparedAccess, BackendError> {
    let key = key_for_binding(binding);
    let Some(record) = load_tokens(&*binding.store, &key)? else {
        note_missing_credential(binding);
        return Err(BackendError::Unavailable(
            "MCP authorization needs sign-in".into(),
        ));
    };
    if !same_url(&record.resource, endpoint) {
        mark_relogin_required(binding);
        return Err(BackendError::Unavailable(
            "MCP authorization needs sign-in".into(),
        ));
    }
    let now = unix_now((binding.now)());
    if !force && !needs_refresh(record.expires_at_unix, now) {
        return Ok(PreparedAccess {
            access: record.access_token.clone(),
            secrets: record.secret_values(),
            refreshed: false,
        });
    }
    match refresh_tokens(http, &record, now) {
        Ok(updated) => {
            let mut secrets = record.secret_values();
            secrets.extend(updated.secret_values());
            save_tokens(&*binding.store, &key, &updated)?;
            let _ = write_server_state(
                binding,
                None,
                None,
                None,
                McpLoginStatus::LoggedIn,
                "",
                &secrets,
            );
            log_auth(
                &binding.diagnostic_log,
                &secrets,
                &format!("mcp oauth refresh completed for {}", binding.server_id),
            );
            Ok(PreparedAccess {
                access: updated.access_token,
                secrets,
                refreshed: true,
            })
        }
        Err(RefreshFail::Rejected) => {
            mark_relogin_required(binding);
            Err(BackendError::Unavailable(
                "MCP authorization needs sign-in".into(),
            ))
        }
        Err(RefreshFail::Transient(error)) => Err(error),
    }
}

pub(crate) fn mark_relogin_required(binding: &McpOAuthBinding) {
    let key = key_for_binding(binding);
    let secrets = load_tokens(&*binding.store, &key)
        .ok()
        .flatten()
        .map(|tokens| tokens.secret_values())
        .unwrap_or_default();
    let _ = binding.store.delete(&key);
    let _ = write_server_state(
        binding,
        None,
        None,
        None,
        McpLoginStatus::ReLoginRequired,
        MCP_AUTH_DETAIL_REFRESH_REJECTED,
        &secrets,
    );
    log_auth(
        &binding.diagnostic_log,
        &secrets,
        &format!("mcp oauth refresh rejected for {}", binding.server_id),
    );
}

fn discover_with(
    http: &reqwest::blocking::Client,
    mcp_endpoint: &str,
) -> Result<DiscoveredAuthorization, BackendError> {
    if !allowed_mcp_http_url(mcp_endpoint) {
        return Err(BackendError::Transport(
            "remote MCP endpoint must use HTTPS".into(),
        ));
    }
    let endpoint = Url::parse(mcp_endpoint)
        .map_err(|_| BackendError::Transport("remote MCP endpoint must use HTTPS".into()))?;
    let explicit = probe_resource_metadata(http, mcp_endpoint)?;
    let document = if let Some(metadata_url) = explicit {
        let metadata = Url::parse(&metadata_url).map_err(|_| {
            BackendError::Protocol("protected resource metadata URL is invalid".into())
        })?;
        if !allowed_mcp_http_url(metadata.as_str()) || !same_authority(&endpoint, &metadata) {
            return Err(BackendError::Protocol(
                "refusing protected resource metadata from a different host".into(),
            ));
        }
        fetch_json(http, metadata.as_str())?
    } else {
        let mut found = None;
        for candidate in protected_resource_metadata_urls(&endpoint) {
            match fetch_json_optional(http, &candidate) {
                Ok(Some(value)) => {
                    found = Some(value);
                    break;
                }
                Ok(None) => continue,
                Err(error) => return Err(error),
            }
        }
        found.ok_or_else(|| {
            BackendError::Protocol("protected resource metadata was not found".into())
        })?
    };
    let protected = parse_protected_resource(&document)?;
    if !same_url(&protected.resource, mcp_endpoint) {
        return Err(BackendError::Protocol(
            "protected resource metadata is for a different resource".into(),
        ));
    }
    if !protected.bearer_methods_supported.is_empty()
        && !protected
            .bearer_methods_supported
            .iter()
            .any(|method| method == "header")
    {
        return Err(BackendError::Protocol(
            "MCP resource does not accept a bearer header".into(),
        ));
    }
    if protected.authorization_servers.is_empty() {
        return Err(BackendError::Protocol(
            "protected resource metadata has no authorization server".into(),
        ));
    }
    for issuer in &protected.authorization_servers {
        if !allowed_mcp_http_url(issuer) {
            continue;
        }
        let Ok(issuer_url) = Url::parse(issuer) else {
            continue;
        };
        if let Some(discovered) = authorization_server(http, &issuer_url, &protected, mcp_endpoint)
        {
            return Ok(discovered);
        }
    }
    Err(BackendError::Protocol(
        "authorization server metadata was not usable".into(),
    ))
}

struct ProtectedResourceDoc {
    resource: String,
    authorization_servers: Vec<String>,
    scopes_supported: Vec<String>,
    bearer_methods_supported: Vec<String>,
}

fn parse_protected_resource(value: &Value) -> Result<ProtectedResourceDoc, BackendError> {
    let resource = value
        .get("resource")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_owned();
    if resource.is_empty() {
        return Err(BackendError::Protocol(
            "protected resource metadata is missing resource".into(),
        ));
    }
    let authorization_servers = string_list(value.get("authorization_servers"));
    Ok(ProtectedResourceDoc {
        resource,
        authorization_servers,
        scopes_supported: string_list(value.get("scopes_supported")),
        bearer_methods_supported: string_list(value.get("bearer_methods_supported")),
    })
}

fn authorization_server(
    http: &reqwest::blocking::Client,
    issuer: &Url,
    protected: &ProtectedResourceDoc,
    mcp_endpoint: &str,
) -> Option<DiscoveredAuthorization> {
    let mut metadata = None;
    for suffix in ["oauth-authorization-server", "openid-configuration"] {
        let url = well_known_url(issuer, suffix);
        match fetch_json_optional(http, &url) {
            Ok(Some(value)) => {
                metadata = Some(value);
                break;
            }
            Ok(None) => continue,
            Err(_) => return None,
        }
    }
    let value = metadata?;
    let parsed_issuer = value.get("issuer").and_then(Value::as_str).unwrap_or("");
    if !same_url(parsed_issuer, issuer.as_str()) {
        return None;
    }
    let authorization_endpoint = value
        .get("authorization_endpoint")
        .and_then(Value::as_str)
        .unwrap_or("");
    let token_endpoint = value
        .get("token_endpoint")
        .and_then(Value::as_str)
        .unwrap_or("");
    if !allowed_mcp_http_url(authorization_endpoint) || !allowed_mcp_http_url(token_endpoint) {
        return None;
    }
    let methods = string_list(value.get("code_challenge_methods_supported"));
    if !methods.iter().any(|method| method == "S256") {
        return None;
    }
    let grants = string_list(value.get("grant_types_supported"));
    if !grants.is_empty()
        && (!grants.iter().any(|grant| grant == "authorization_code")
            || !grants.iter().any(|grant| grant == "refresh_token"))
    {
        return None;
    }
    let auth_methods = string_list(value.get("token_endpoint_auth_methods_supported"));
    if !auth_methods.is_empty() && !auth_methods.iter().any(|method| method == "none") {
        return None;
    }
    let revocation = optional_allowed_url(value.get("revocation_endpoint"));
    let registration = optional_allowed_url(value.get("registration_endpoint"));
    if value.get("revocation_endpoint").is_some() && revocation.is_none() {
        return None;
    }
    if value.get("registration_endpoint").is_some() && registration.is_none() {
        return None;
    }
    let mut scopes = protected.scopes_supported.clone();
    if scopes.is_empty() {
        scopes = string_list(value.get("scopes_supported"));
    }
    Some(DiscoveredAuthorization {
        resource: canonical_url(mcp_endpoint).unwrap_or_else(|_| mcp_endpoint.to_owned()),
        issuer: canonical_url(issuer.as_str()).unwrap_or_else(|_| issuer.to_string()),
        authorization_endpoint: authorization_endpoint.to_owned(),
        token_endpoint: token_endpoint.to_owned(),
        revocation_endpoint: revocation,
        registration_endpoint: registration,
        scopes_supported: scopes,
    })
}

fn optional_allowed_url(value: Option<&Value>) -> Option<String> {
    let text = value.and_then(Value::as_str)?;
    if text.is_empty() {
        return None;
    }
    allowed_mcp_http_url(text).then(|| text.to_owned())
}

fn probe_resource_metadata(
    http: &reqwest::blocking::Client,
    mcp_endpoint: &str,
) -> Result<Option<String>, BackendError> {
    let response = http
        .post(mcp_endpoint)
        .header("Accept", "application/json, text/event-stream")
        .header("MCP-Protocol-Version", MCP_PROTOCOL_VERSION)
        .json(&serde_json::json!({
            "jsonrpc": "2.0",
            "id": 0,
            "method": "initialize",
            "params": {}
        }))
        .send()
        .map_err(|_| BackendError::Transport("MCP authorization discovery failed".into()))?;
    if response.status() != reqwest::StatusCode::UNAUTHORIZED {
        return Err(BackendError::Protocol(format!(
            "MCP server did not request authorization (HTTP {})",
            response.status()
        )));
    }
    let mut header = String::new();
    for value in response
        .headers()
        .get_all(reqwest::header::WWW_AUTHENTICATE)
    {
        let Ok(text) = value.to_str() else {
            continue;
        };
        if !header.is_empty() {
            header.push(',');
        }
        header.push_str(text);
    }
    drop(response);
    Ok(resource_metadata_from_www_authenticate(&header))
}

pub(crate) fn resource_metadata_from_www_authenticate(header: &str) -> Option<String> {
    let lower = header.to_ascii_lowercase();
    let key = "resource_metadata";
    let start = lower.find(key)?;
    let mut rest = header[start + key.len()..].trim_start();
    rest = rest.strip_prefix('=')?.trim_start();
    if let Some(quoted) = rest.strip_prefix('"') {
        let mut out = String::new();
        let mut chars = quoted.chars();
        while let Some(ch) = chars.next() {
            if ch == '\\' {
                if let Some(escaped) = chars.next() {
                    out.push(escaped);
                }
                continue;
            }
            if ch == '"' {
                break;
            }
            out.push(ch);
        }
        return (!out.is_empty()).then_some(out);
    }
    let end = rest
        .find(|ch: char| ch.is_whitespace() || ch == ',')
        .unwrap_or(rest.len());
    let value = rest[..end].trim();
    (!value.is_empty()).then(|| value.to_owned())
}

fn protected_resource_metadata_urls(resource: &Url) -> Vec<String> {
    let mut urls = Vec::new();
    let path = resource.path().trim_matches('/');
    if !path.is_empty() {
        urls.push(well_known_with_path(
            resource,
            "oauth-protected-resource",
            path,
        ));
    }
    urls.push(well_known_with_path(
        resource,
        "oauth-protected-resource",
        "",
    ));
    urls
}

fn well_known_url(issuer: &Url, suffix: &str) -> String {
    let path = issuer.path().trim_matches('/');
    if path.is_empty() {
        well_known_with_path(issuer, suffix, "")
    } else {
        well_known_with_path(issuer, suffix, path)
    }
}

fn well_known_with_path(base: &Url, suffix: &str, tail: &str) -> String {
    let mut url = base.clone();
    if tail.is_empty() {
        url.set_path(&format!("/.well-known/{suffix}"));
    } else {
        url.set_path(&format!("/.well-known/{suffix}/{tail}"));
    }
    url.set_query(None);
    url.set_fragment(None);
    url.to_string()
}

fn resolve_client_id(
    http: &reqwest::blocking::Client,
    options: &PkceLoginOptions,
    discovered: &DiscoveredAuthorization,
    redirect_uri: &str,
) -> Result<String, BackendError> {
    if let Some(client_id) = options
        .client_id
        .as_ref()
        .filter(|id| !id.is_empty())
        .cloned()
        .or_else(|| {
            McpServerCatalog::load(&options.binding.catalog_path)
                .ok()
                .and_then(|catalog| {
                    catalog
                        .servers
                        .into_iter()
                        .find(|server| server.id == options.binding.server_id)
                        .and_then(|server| server.client_id)
                        .filter(|id| !id.is_empty())
                })
        })
    {
        return Ok(client_id);
    }
    let Some(endpoint) = &discovered.registration_endpoint else {
        return Err(BackendError::Protocol(
            "MCP authorization server requires a client id".into(),
        ));
    };
    let response = http
        .post(endpoint)
        .header("Accept", "application/json")
        .json(&serde_json::json!({
            "client_name": "Termior",
            "redirect_uris": [redirect_uri],
            "grant_types": ["authorization_code", "refresh_token"],
            "response_types": ["code"],
            "token_endpoint_auth_method": "none"
        }))
        .send()
        .map_err(|_| BackendError::Transport("MCP client registration failed".into()))?;
    if !response.status().is_success() {
        return Err(BackendError::Protocol(format!(
            "MCP client registration was rejected (HTTP {})",
            response.status()
        )));
    }
    let body = read_limited(response, METADATA_LIMIT)?;
    let value: Value = serde_json::from_slice(&body).map_err(|_| {
        BackendError::Protocol("MCP client registration response was invalid".into())
    })?;
    if value
        .get("client_secret")
        .and_then(Value::as_str)
        .is_some_and(|secret| !secret.is_empty())
    {
        return Err(BackendError::Protocol(
            "MCP authorization server issued a client secret; Termior only supports public PKCE clients"
                .into(),
        ));
    }
    value
        .get("client_id")
        .and_then(Value::as_str)
        .filter(|id| !id.is_empty())
        .map(str::to_owned)
        .ok_or_else(|| BackendError::Protocol("MCP client registration omitted a client id".into()))
}

fn choose_scope(requested: &[String], supported: &[String]) -> Result<String, BackendError> {
    let requested: Vec<&str> = requested
        .iter()
        .map(String::as_str)
        .filter(|scope| !scope.is_empty())
        .collect();
    if requested.is_empty() {
        return Ok(supported.join(" "));
    }
    if !supported.is_empty()
        && requested
            .iter()
            .any(|scope| !supported.iter().any(|supported| supported == scope))
    {
        return Err(BackendError::Protocol(
            "MCP authorization server does not support the requested scope".into(),
        ));
    }
    Ok(requested.join(" "))
}

fn authorization_url(
    request: &OAuthPkceRequest,
    client_id: &str,
    redirect_uri: &str,
    scope: &str,
) -> Result<String, BackendError> {
    let mut url = Url::parse(&request.authorization_endpoint)
        .map_err(|_| BackendError::Protocol("authorization endpoint is invalid".into()))?;
    {
        let mut query = url.query_pairs_mut();
        query.append_pair("response_type", "code");
        query.append_pair("client_id", client_id);
        query.append_pair("redirect_uri", redirect_uri);
        query.append_pair("code_challenge", &request.code_challenge);
        query.append_pair("code_challenge_method", "S256");
        query.append_pair("state", &request.state);
        query.append_pair("resource", &request.resource);
        if !scope.is_empty() {
            query.append_pair("scope", scope);
        }
    }
    Ok(url.to_string())
}

fn accept_callback(
    listener: TcpListener,
    expected: &OAuthPkceRequest,
    timeout: Duration,
    cancel: &AtomicBool,
) -> Result<String, BackendError> {
    let started = std::time::Instant::now();
    loop {
        if cancel.load(Ordering::SeqCst) {
            return Err(BackendError::Unavailable(
                "MCP authorization was cancelled".into(),
            ));
        }
        if started.elapsed() > timeout {
            return Err(BackendError::Timeout(
                "timed out waiting for the MCP authorization callback".into(),
            ));
        }
        match listener.accept() {
            Ok((stream, address)) => {
                let _ = stream.set_read_timeout(Some(Duration::from_secs(2)));
                if !address.ip().is_loopback() {
                    let _ = write_callback_page(&stream, 403);
                    continue;
                }
                let target = read_request_target(&stream).ok();
                let decision = target
                    .as_deref()
                    .map(|target| judge_callback(expected, target))
                    .unwrap_or(CallbackDecision::Ignore);
                match decision {
                    CallbackDecision::Code(code) => {
                        let _ = write_callback_page(&stream, 200);
                        return Ok(code);
                    }
                    CallbackDecision::Ignore => {
                        let _ = write_callback_page(&stream, 404);
                    }
                    CallbackDecision::Reject => {
                        let _ = write_callback_page(&stream, 400);
                        return Err(BackendError::Protocol(
                            "MCP authorization callback did not match the pending sign-in".into(),
                        ));
                    }
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                std::thread::sleep(Duration::from_millis(20));
            }
            Err(_) => {
                return Err(BackendError::Transport(
                    "MCP authorization callback failed".into(),
                ))
            }
        }
    }
}

enum CallbackDecision {
    Code(String),
    Ignore,
    Reject,
}

fn judge_callback(expected: &OAuthPkceRequest, target: &str) -> CallbackDecision {
    let Ok(url) = Url::parse(&format!("http://127.0.0.1{target}")) else {
        return CallbackDecision::Ignore;
    };
    if url.path() != "/callback" {
        return CallbackDecision::Ignore;
    }
    let mut state = None;
    let mut resource = None;
    let mut code = None;
    let mut failed = false;
    for (key, value) in url.query_pairs() {
        match key.as_ref() {
            "state" => state = Some(value.into_owned()),
            "resource" => resource = Some(value.into_owned()),
            "code" => code = Some(value.into_owned()),
            "error" => failed = true,
            _ => {}
        }
    }
    if failed {
        return CallbackDecision::Reject;
    }
    let Some(state) = state else {
        return CallbackDecision::Reject;
    };
    let resource_ok = resource
        .as_deref()
        .map(|value| value == expected.resource)
        .unwrap_or(true);
    if !expected.validate_callback(&state, &expected.resource) || !resource_ok {
        return CallbackDecision::Reject;
    }
    match code.filter(|value| !value.is_empty()) {
        Some(code) => CallbackDecision::Code(code),
        None => CallbackDecision::Reject,
    }
}

fn read_request_target(stream: &std::net::TcpStream) -> Result<String, ()> {
    let mut reader = stream;
    let mut buf = [0u8; 8192];
    let mut filled = 0usize;
    loop {
        if filled == buf.len() {
            return Err(());
        }
        match reader.read(&mut buf[filled..]) {
            Ok(0) => break,
            Ok(count) => {
                filled += count;
                if buf[..filled].windows(4).any(|bytes| bytes == b"\r\n\r\n") {
                    break;
                }
            }
            Err(error)
                if error.kind() == std::io::ErrorKind::WouldBlock
                    || error.kind() == std::io::ErrorKind::TimedOut =>
            {
                if filled > 0 {
                    break;
                }
                return Err(());
            }
            Err(_) => return Err(()),
        }
    }
    let text = String::from_utf8_lossy(&buf[..filled]);
    let mut parts = text.lines().next().unwrap_or("").split_whitespace();
    let method = parts.next().unwrap_or("");
    let target = parts.next().unwrap_or("");
    if method != "GET" || !target.starts_with('/') {
        return Err(());
    }
    Ok(target.to_owned())
}

fn write_callback_page(mut stream: &std::net::TcpStream, status: u16) -> std::io::Result<()> {
    let (reason, text) = match status {
        200 => ("OK", "Termior signed in. You can close this window."),
        404 => ("Not Found", "Not found."),
        _ => (
            "Bad Request",
            "Authorization failed. Return to Termior and try again.",
        ),
    };
    let body = format!("<!doctype html><html><body><p>{text}</p></body></html>");
    write!(
        stream,
        "HTTP/1.1 {status} {reason}\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\nCache-Control: no-store\r\n\r\n{body}",
        body.len()
    )
}

struct CodeExchange<'a> {
    client_id: &'a str,
    redirect_uri: &'a str,
    verifier: &'a str,
    code: &'a str,
    scope: &'a str,
    now: u64,
}

fn exchange_code(
    http: &reqwest::blocking::Client,
    discovered: &DiscoveredAuthorization,
    exchange: &CodeExchange<'_>,
) -> Result<StoredMcpTokens, BackendError> {
    let mut pairs = vec![
        ("grant_type", "authorization_code"),
        ("code", exchange.code),
        ("redirect_uri", exchange.redirect_uri),
        ("client_id", exchange.client_id),
        ("code_verifier", exchange.verifier),
        ("resource", discovered.resource.as_str()),
    ];
    if !exchange.scope.is_empty() {
        pairs.push(("scope", exchange.scope));
    }
    let body = form_body(&pairs);
    let value = token_response(http, &discovered.token_endpoint, &body)?;
    stored_from_token_response(value, discovered, exchange.client_id, None, exchange.now)
}

fn refresh_tokens(
    http: &reqwest::blocking::Client,
    record: &StoredMcpTokens,
    now: u64,
) -> Result<StoredMcpTokens, RefreshFail> {
    let Some(refresh) = record.refresh_token.as_deref() else {
        return Err(RefreshFail::Rejected);
    };
    if !allowed_mcp_http_url(&record.token_endpoint) {
        return Err(RefreshFail::Rejected);
    }
    let body = form_body(&[
        ("grant_type", "refresh_token"),
        ("refresh_token", refresh),
        ("client_id", record.client_id.as_str()),
        ("resource", record.resource.as_str()),
    ]);
    let response = match http
        .post(&record.token_endpoint)
        .header("Accept", "application/json")
        .header("Content-Type", "application/x-www-form-urlencoded")
        .body(body)
        .send()
    {
        Ok(response) => response,
        Err(_) => {
            return Err(RefreshFail::Transient(BackendError::Transport(
                "MCP token refresh failed".into(),
            )))
        }
    };
    let status = response.status();
    if status == reqwest::StatusCode::BAD_REQUEST || status == reqwest::StatusCode::UNAUTHORIZED {
        let _ = response.bytes();
        return Err(RefreshFail::Rejected);
    }
    if !status.is_success() {
        let _ = response.bytes();
        return Err(RefreshFail::Transient(BackendError::Transport(format!(
            "MCP token refresh failed (HTTP {status})"
        ))));
    }
    let bytes = match read_limited(response, METADATA_LIMIT) {
        Ok(bytes) => bytes,
        Err(error) => return Err(RefreshFail::Transient(error)),
    };
    let value: Value = match serde_json::from_slice(&bytes) {
        Ok(value) => value,
        Err(_) => {
            return Err(RefreshFail::Transient(BackendError::Protocol(
                "MCP token refresh response was invalid".into(),
            )))
        }
    };
    if value.get("error").is_some() {
        return Err(RefreshFail::Rejected);
    }
    let discovered = DiscoveredAuthorization {
        resource: record.resource.clone(),
        issuer: String::new(),
        authorization_endpoint: String::new(),
        token_endpoint: record.token_endpoint.clone(),
        revocation_endpoint: record.revocation_endpoint.clone(),
        registration_endpoint: None,
        scopes_supported: Vec::new(),
    };
    match stored_from_token_response(
        value,
        &discovered,
        &record.client_id,
        record.refresh_token.clone(),
        now,
    ) {
        Ok(tokens) => Ok(tokens),
        Err(_) => Err(RefreshFail::Rejected),
    }
}

fn token_response(
    http: &reqwest::blocking::Client,
    endpoint: &str,
    body: &str,
) -> Result<Value, BackendError> {
    let response = http
        .post(endpoint)
        .header("Accept", "application/json")
        .header("Content-Type", "application/x-www-form-urlencoded")
        .body(body.to_owned())
        .send()
        .map_err(|_| BackendError::Transport("MCP token request failed".into()))?;
    let status = response.status();
    let bytes = read_limited(response, METADATA_LIMIT)?;
    if !status.is_success() {
        return Err(BackendError::Protocol(format!(
            "MCP token request was rejected (HTTP {status})"
        )));
    }
    serde_json::from_slice(&bytes)
        .map_err(|_| BackendError::Protocol("MCP token response was invalid".into()))
}

fn stored_from_token_response(
    value: Value,
    discovered: &DiscoveredAuthorization,
    client_id: &str,
    previous_refresh: Option<String>,
    now: u64,
) -> Result<StoredMcpTokens, BackendError> {
    if let Some(kind) = value.get("token_type").and_then(Value::as_str) {
        if !kind.eq_ignore_ascii_case("bearer") {
            return Err(BackendError::Protocol(
                "MCP token response was not a bearer token".into(),
            ));
        }
    }
    let access_token = value
        .get("access_token")
        .and_then(Value::as_str)
        .filter(|token| !token.is_empty())
        .ok_or_else(|| BackendError::Protocol("MCP token response omitted an access token".into()))?
        .to_owned();
    let refresh_token = value
        .get("refresh_token")
        .and_then(Value::as_str)
        .filter(|token| !token.is_empty())
        .map(str::to_owned)
        .or(previous_refresh);
    let expires_at_unix = expires_in_seconds(&value).map(|seconds| now.saturating_add(seconds));
    let scope = value
        .get("scope")
        .and_then(Value::as_str)
        .filter(|scope| !scope.is_empty())
        .map(str::to_owned);
    Ok(StoredMcpTokens {
        access_token,
        refresh_token,
        expires_at_unix,
        resource: discovered.resource.clone(),
        token_endpoint: discovered.token_endpoint.clone(),
        revocation_endpoint: discovered.revocation_endpoint.clone(),
        client_id: client_id.to_owned(),
        scope,
    })
}

fn revoke_remote(http: &reqwest::blocking::Client, record: &StoredMcpTokens) -> bool {
    let Some(endpoint) = record.revocation_endpoint.as_deref() else {
        return false;
    };
    if !allowed_mcp_http_url(endpoint) {
        return false;
    }
    let (token, hint) = match record.refresh_token.as_deref() {
        Some(refresh) => (refresh, "refresh_token"),
        None => (record.access_token.as_str(), "access_token"),
    };
    let body = form_body(&[
        ("token", token),
        ("token_type_hint", hint),
        ("client_id", record.client_id.as_str()),
    ]);
    let Ok(response) = http
        .post(endpoint)
        .header("Accept", "application/json")
        .header("Content-Type", "application/x-www-form-urlencoded")
        .body(body)
        .send()
    else {
        return false;
    };
    let ok = response.status().is_success();
    let _ = response.bytes();
    ok
}

fn form_body(pairs: &[(&str, &str)]) -> String {
    let mut serializer = url::form_urlencoded::Serializer::new(String::new());
    for (key, value) in pairs {
        serializer.append_pair(key, value);
    }
    serializer.finish()
}

enum RefreshFail {
    Rejected,
    Transient(BackendError),
}

#[derive(Clone)]
struct StoredMcpTokens {
    access_token: String,
    refresh_token: Option<String>,
    expires_at_unix: Option<u64>,
    resource: String,
    token_endpoint: String,
    revocation_endpoint: Option<String>,
    client_id: String,
    scope: Option<String>,
}

impl std::fmt::Debug for StoredMcpTokens {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("StoredMcpTokens")
            .field("resource", &self.resource)
            .field("expires_at_unix", &self.expires_at_unix)
            .field("client_id", &self.client_id)
            .finish_non_exhaustive()
    }
}

impl StoredMcpTokens {
    fn secret_values(&self) -> Vec<String> {
        let mut secrets = vec![self.access_token.clone()];
        if let Some(refresh) = &self.refresh_token {
            secrets.push(refresh.clone());
        }
        secrets
    }
}

#[derive(Serialize, Deserialize)]
struct StoredMcpTokensFile {
    access_token: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    refresh_token: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    expires_at_unix: Option<u64>,
    resource: String,
    token_endpoint: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    revocation_endpoint: Option<String>,
    client_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    scope: Option<String>,
}

impl From<&StoredMcpTokens> for StoredMcpTokensFile {
    fn from(tokens: &StoredMcpTokens) -> Self {
        Self {
            access_token: tokens.access_token.clone(),
            refresh_token: tokens.refresh_token.clone(),
            expires_at_unix: tokens.expires_at_unix,
            resource: tokens.resource.clone(),
            token_endpoint: tokens.token_endpoint.clone(),
            revocation_endpoint: tokens.revocation_endpoint.clone(),
            client_id: tokens.client_id.clone(),
            scope: tokens.scope.clone(),
        }
    }
}

impl From<StoredMcpTokensFile> for StoredMcpTokens {
    fn from(file: StoredMcpTokensFile) -> Self {
        Self {
            access_token: file.access_token,
            refresh_token: file.refresh_token,
            expires_at_unix: file.expires_at_unix,
            resource: file.resource,
            token_endpoint: file.token_endpoint,
            revocation_endpoint: file.revocation_endpoint,
            client_id: file.client_id,
            scope: file.scope,
        }
    }
}

fn load_tokens(
    store: &dyn SecretStore,
    key: &str,
) -> Result<Option<StoredMcpTokens>, BackendError> {
    match store.get(key) {
        Ok(Some(raw)) => {
            let parsed: StoredMcpTokensFile = serde_json::from_str(&raw).map_err(|_| {
                BackendError::Protocol("MCP credential in the keyring is unreadable".into())
            })?;
            Ok(Some(parsed.into()))
        }
        Ok(None) => Ok(None),
        Err(_) => Err(BackendError::Unavailable(
            "MCP credential store is unavailable".into(),
        )),
    }
}

fn save_tokens(
    store: &dyn SecretStore,
    key: &str,
    tokens: &StoredMcpTokens,
) -> Result<(), BackendError> {
    let raw = serde_json::to_string(&StoredMcpTokensFile::from(tokens))
        .map_err(|_| BackendError::Protocol("MCP credential could not be encoded".into()))?;
    store
        .set(key, &raw)
        .map_err(|_| BackendError::Unavailable("MCP credential store rejected the token".into()))
}

fn key_for_binding(binding: &McpOAuthBinding) -> String {
    McpServerCatalog::load(&binding.catalog_path)
        .ok()
        .and_then(|catalog| {
            catalog
                .servers
                .into_iter()
                .find(|server| server.id == binding.server_id)
                .and_then(|server| server.oauth_key_ref)
                .filter(|key| !key.is_empty())
        })
        .unwrap_or_else(|| mcp_oauth_key(&binding.server_id))
}

fn note_missing_credential(binding: &McpOAuthBinding) {
    let Ok(catalog) = McpServerCatalog::load(&binding.catalog_path) else {
        return;
    };
    let logged_in = catalog.servers.iter().any(|server| {
        server.id == binding.server_id && server.auth_status == McpLoginStatus::LoggedIn
    });
    if logged_in {
        let _ = write_server_state(
            binding,
            None,
            None,
            None,
            McpLoginStatus::ReLoginRequired,
            "",
            &[],
        );
    }
}

fn write_server_state(
    binding: &McpOAuthBinding,
    endpoint: Option<&str>,
    client_id: Option<String>,
    scopes: Option<Vec<String>>,
    status: McpLoginStatus,
    detail: &str,
    secrets: &[String],
) -> Result<(), BackendError> {
    let mut catalog = McpServerCatalog::load(&binding.catalog_path)?;
    if let Some(server) = catalog
        .servers
        .iter_mut()
        .find(|server| server.id == binding.server_id)
    {
        if let Some(endpoint) = endpoint {
            if !server.url.is_empty() && !same_url(&server.url, endpoint) {
                return Err(BackendError::Protocol(
                    "MCP server URL does not match the authorization resource".into(),
                ));
            }
            server.url = canonical_url(endpoint).unwrap_or_else(|_| endpoint.to_owned());
        }
        if client_id.is_some() {
            server.client_id = client_id;
        }
        if let Some(scopes) = scopes {
            server.scopes = scopes;
        }
        if server
            .oauth_key_ref
            .as_ref()
            .map(String::is_empty)
            .unwrap_or(true)
        {
            server.oauth_key_ref = Some(mcp_oauth_key(&binding.server_id));
        }
        server.auth_status = status;
        server.auth_detail = detail.to_owned();
    } else {
        let url = endpoint
            .map(|value| canonical_url(value).unwrap_or_else(|_| value.to_owned()))
            .unwrap_or_default();
        catalog.servers.push(McpRemoteServer {
            id: binding.server_id.clone(),
            url,
            oauth_key_ref: Some(mcp_oauth_key(&binding.server_id)),
            client_id,
            scopes: scopes.unwrap_or_default(),
            auth_status: status,
            auth_detail: detail.to_owned(),
        });
    }
    catalog.save(&binding.catalog_path, secrets)
}

fn needs_refresh(expires_at: Option<u64>, now: u64) -> bool {
    expires_at.is_some_and(|expires| now.saturating_add(TOKEN_REFRESH_SKEW_SECS) >= expires)
}

fn expires_in_seconds(value: &Value) -> Option<u64> {
    let expires = value.get("expires_in")?;
    expires
        .as_u64()
        .or_else(|| expires.as_f64().map(|seconds| seconds.max(0.0) as u64))
}

fn unix_now(time: SystemTime) -> u64 {
    time.duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0)
}

fn random_token(len: usize) -> Result<String, BackendError> {
    const ALPHABET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-._~";
    let mut out = String::with_capacity(len);
    let mut bytes = [0u8; 64];
    while out.len() < len {
        getrandom::getrandom(&mut bytes).map_err(|_| {
            BackendError::Unavailable("MCP authorization could not generate a random value".into())
        })?;
        for byte in bytes {
            if out.len() == len {
                break;
            }
            if (byte as usize) < ALPHABET.len() * 3 {
                out.push(ALPHABET[(byte as usize) % ALPHABET.len()] as char);
            }
        }
    }
    Ok(out)
}

fn fetch_json(http: &reqwest::blocking::Client, url: &str) -> Result<Value, BackendError> {
    match fetch_json_optional(http, url)? {
        Some(value) => Ok(value),
        None => Err(BackendError::Protocol(
            "authorization metadata was not found".into(),
        )),
    }
}

fn fetch_json_optional(
    http: &reqwest::blocking::Client,
    url: &str,
) -> Result<Option<Value>, BackendError> {
    if !allowed_mcp_http_url(url) {
        return Err(BackendError::Protocol(
            "authorization metadata URL is not allowed".into(),
        ));
    }
    let response = http
        .get(url)
        .header("Accept", "application/json")
        .send()
        .map_err(|_| BackendError::Transport("authorization metadata request failed".into()))?;
    if response.status() == reqwest::StatusCode::NOT_FOUND {
        let _ = response.bytes();
        return Ok(None);
    }
    if !response.status().is_success() {
        return Err(BackendError::Transport(format!(
            "authorization metadata request failed (HTTP {})",
            response.status()
        )));
    }
    let bytes = read_limited(response, METADATA_LIMIT)?;
    serde_json::from_slice(&bytes)
        .map(Some)
        .map_err(|_| BackendError::Protocol("authorization metadata was not valid JSON".into()))
}

fn read_limited(
    response: reqwest::blocking::Response,
    limit: usize,
) -> Result<Vec<u8>, BackendError> {
    let mut buf = Vec::new();
    response
        .take((limit as u64).saturating_add(1))
        .read_to_end(&mut buf)
        .map_err(|_| BackendError::Transport("authorization response could not be read".into()))?;
    if buf.len() > limit {
        return Err(BackendError::Protocol(
            "authorization response exceeded the size limit".into(),
        ));
    }
    Ok(buf)
}

fn string_list(value: Option<&Value>) -> Vec<String> {
    value
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(Value::as_str)
                .filter(|item| !item.is_empty())
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default()
}

fn same_authority(left: &Url, right: &Url) -> bool {
    left.scheme() == right.scheme()
        && left.host() == right.host()
        && left.port_or_known_default() == right.port_or_known_default()
}

fn same_url(left: &str, right: &str) -> bool {
    match (canonical_url(left), canonical_url(right)) {
        (Ok(left), Ok(right)) => left == right,
        _ => false,
    }
}

fn canonical_url(value: &str) -> Result<String, ()> {
    let mut url = Url::parse(value).map_err(|_| ())?;
    url.set_fragment(None);
    url.set_query(None);
    let path = url.path().to_owned();
    if let Some(trimmed) = path.strip_suffix('/') {
        if !trimmed.is_empty() {
            url.set_path(trimmed);
        }
    }
    Ok(url.to_string().trim_end_matches('/').to_owned())
}

fn contains_credential(text: &str, secrets: &[String]) -> bool {
    if text.contains("\"access_token\"")
        || text.contains("\"refresh_token\"")
        || text.contains("\"code_verifier\"")
    {
        return true;
    }
    secrets
        .iter()
        .any(|secret| secret.len() >= MIN_SECRET_LEN && text.contains(secret))
}

fn public_message(message: &str) -> String {
    let lower = message.to_ascii_lowercase();
    if lower.contains("access_token")
        || lower.contains("refresh_token")
        || lower.contains("code_verifier")
        || lower.contains("code=")
    {
        return "MCP authorization failed".into();
    }
    message.chars().take(300).collect()
}

fn log_auth(path: &Option<PathBuf>, secrets: &[String], message: &str) {
    let Some(path) = path else {
        return;
    };
    let _ =
        termior_store::StreamingRedactor::new(secrets.iter().cloned()).append_record(path, message);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pkce_s256_matches_the_rfc_7636_vector() {
        let verifier = "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk";
        assert_eq!(
            oauth_code_challenge_s256(verifier),
            "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM"
        );
    }

    #[test]
    fn www_authenticate_resource_metadata_is_parsed_and_unquoted() {
        let quoted = r#"Bearer realm="mcp", resource_metadata="http://127.0.0.1:9/.well-known/oauth-protected-resource""#;
        assert_eq!(
            resource_metadata_from_www_authenticate(quoted).as_deref(),
            Some("http://127.0.0.1:9/.well-known/oauth-protected-resource")
        );
        let bare = "Bearer resource_metadata=http://127.0.0.1:9/meta, error=invalid_token";
        assert_eq!(
            resource_metadata_from_www_authenticate(bare).as_deref(),
            Some("http://127.0.0.1:9/meta")
        );
        assert!(resource_metadata_from_www_authenticate("Bearer realm=mcp").is_none());
    }

    #[test]
    fn callback_rejects_a_mismatched_state_or_resource() {
        let expected = OAuthPkceRequest {
            authorization_endpoint: "https://auth.example/authorize".into(),
            resource: "https://mcp.example/mcp".into(),
            code_challenge: "challenge".into(),
            state: "state-1".into(),
        };
        assert!(matches!(
            judge_callback(&expected, "/callback?code=abc&state=state-1"),
            CallbackDecision::Code(_)
        ));
        assert!(matches!(
            judge_callback(&expected, "/callback?code=abc&state=other"),
            CallbackDecision::Reject
        ));
        assert!(matches!(
            judge_callback(
                &expected,
                "/callback?code=abc&state=state-1&resource=https://evil.example"
            ),
            CallbackDecision::Reject
        ));
        assert!(matches!(
            judge_callback(&expected, "/favicon.ico"),
            CallbackDecision::Ignore
        ));
    }

    #[test]
    fn remote_http_must_be_https_and_loopback_stays_allowed() {
        assert!(allowed_mcp_http_url("https://mcp.example/mcp"));
        assert!(allowed_mcp_http_url("http://127.0.0.1:9/mcp"));
        assert!(allowed_mcp_http_url("http://localhost:9/mcp"));
        assert!(!allowed_mcp_http_url("http://mcp.example/mcp"));
        assert!(!allowed_mcp_http_url("http://localhost.evil/mcp"));
        assert!(!allowed_mcp_http_url("http://127.0.0.1.evil/mcp"));
    }

    #[test]
    fn protected_resource_metadata_uses_the_well_known_path() {
        let resource = Url::parse("https://mcp.example/mcp").unwrap();
        let urls = protected_resource_metadata_urls(&resource);
        assert_eq!(
            urls,
            vec![
                "https://mcp.example/.well-known/oauth-protected-resource/mcp".to_owned(),
                "https://mcp.example/.well-known/oauth-protected-resource".to_owned(),
            ]
        );
    }

    #[test]
    fn stored_token_debug_and_catalog_do_not_contain_credentials() {
        let tokens = StoredMcpTokens {
            access_token: "access-secret-value".into(),
            refresh_token: Some("refresh-secret-value".into()),
            expires_at_unix: Some(10),
            resource: "https://mcp.example/mcp".into(),
            token_endpoint: "https://auth.example/token".into(),
            revocation_endpoint: None,
            client_id: "public".into(),
            scope: None,
        };
        let rendered = format!("{tokens:?}");
        assert!(!rendered.contains("access-secret-value"));
        assert!(!rendered.contains("refresh-secret-value"));
        let catalog = McpServerCatalog {
            servers: vec![McpRemoteServer {
                id: "docs".into(),
                url: "https://mcp.example/mcp".into(),
                oauth_key_ref: Some(mcp_oauth_key("docs")),
                client_id: Some("public".into()),
                scopes: vec!["mcp".into()],
                auth_status: McpLoginStatus::LoggedIn,
                auth_detail: String::new(),
            }],
        };
        let json = serde_json::to_string(&catalog).unwrap();
        assert!(!json.contains("access_token"));
        assert!(!json.contains("refresh_token"));
        assert!(json.contains("mcp-oauth:docs"));
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("mcp-servers.json");
        let mut leaked = catalog.clone();
        leaked.servers[0].auth_detail = "access-secret-value".into();
        assert!(leaked.save(&path, &tokens.secret_values()).is_err());
        assert!(!path.exists());
    }
}
