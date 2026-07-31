//! OAuth 2.0 authorization-code + PKCE login for model providers that offer
//! subscription-based access in addition to API keys:
//!
//! - **OpenAI** — ChatGPT/Codex OAuth. Tokens are issued by
//!   `auth.openai.com` and used as a `Bearer` credential on the OpenAI API.
//! - **Anthropic** — Claude subscription OAuth. The flow authorizes at
//!   `claude.ai`, exchanges the code at `console.anthropic.com`, and the
//!   resulting token is sent as `Authorization: Bearer` (or an API key via
//!   `x-api-key`) on the Messages API.
//!
//! This module owns PKCE generation, authorize-URL construction, token
//! exchange/refresh, and the on-disk token store (`~/.io/oauth.toml`,
//! permissions `0600`). It deliberately does not know about the interactive
//! CLI — the `io` crate drives the browser/paste flow and calls into here.

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use percent_encoding::utf8_percent_encode;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

// ── Public OAuth clients (matching the official CLI apps) ────────────────────

/// Public client ID used by the OpenAI Codex CLI.
pub const OPENAI_CLIENT_ID: &str = "app_EMoamEEZ73f0CkXaXp7hrann";
/// OAuth issuer for ChatGPT/Codex logins.
pub const OPENAI_ISSUER: &str = "https://auth.openai.com";
/// Scopes requested by the Codex CLI (offline_access enables refresh tokens).
pub const OPENAI_SCOPES: &str =
    "openid profile email offline_access api.connectors.read api.connectors.invoke";
/// Local ports the OpenAI authorize server is configured to redirect to.
pub const OPENAI_REDIRECT_PORTS: &[u16] = &[1455, 1457];

/// Public client ID used by Claude Code / the Claude SDK OAuth flow.
pub const ANTHROPIC_CLIENT_ID: &str = "9d1c250a-e61b-44d9-88ed-5944d1962f5e";
/// Anthropic authorization endpoint.
pub const ANTHROPIC_AUTHORIZE_URL: &str = "https://claude.ai/oauth/authorize";
/// Anthropic token endpoint.
pub const ANTHROPIC_TOKEN_URL: &str = "https://console.anthropic.com/v1/oauth/token";
/// Hosted redirect URI for the copy/paste Anthropic flow.
pub const ANTHROPIC_REDIRECT_URI: &str = "https://console.anthropic.com/oauth/code/callback";
/// Scopes requested by the Claude subscription flow.
pub const ANTHROPIC_SCOPES: &str = "org:create_api_key user:profile user:inference";

/// Access tokens are refreshed when they have this little time left (seconds),
/// absorbing clock skew and in-flight request latency.
const REFRESH_SKEW_SECS: u64 = 60;

// ── PKCE ──────────────────────────────────────────────────────────────────────

/// PKCE challenge pair plus a CSRF `state` value for one login attempt.
#[derive(Debug, Clone)]
pub struct Pkce {
    pub code_verifier: String,
    pub code_challenge: String,
    pub state: String,
}

/// Generate a fresh PKCE verifier/challenge pair (S256) and a random state.
pub fn generate_pkce() -> Pkce {
    let mut bytes = [0u8; 64];
    let _ = getrandom::getrandom(&mut bytes);
    // 43..128 chars of [A-Za-z0-9-._~]: 64 bytes -> 86 base64url chars.
    let code_verifier = URL_SAFE_NO_PAD.encode(bytes);
    let digest = Sha256::digest(code_verifier.as_bytes());
    let code_challenge = URL_SAFE_NO_PAD.encode(digest);
    let mut state_bytes = [0u8; 32];
    let _ = getrandom::getrandom(&mut state_bytes);
    let state = URL_SAFE_NO_PAD.encode(state_bytes);
    Pkce {
        code_verifier,
        code_challenge,
        state,
    }
}

/// Percent-encode only what a URL query must escape, leaving unreserved
/// characters (`A-Za-z0-9-._~`) untouched. This matches what OAuth servers
/// expect for `client_id`, `redirect_uri`, and scope values.
static QUERY_ENCODE_SET: percent_encoding::AsciiSet = percent_encoding::CONTROLS
    .add(b' ')
    .add(b'"')
    .add(b'#')
    .add(b'%')
    .add(b'&')
    .add(b'\'')
    .add(b'+')
    .add(b',')
    .add(b'/')
    .add(b':')
    .add(b';')
    .add(b'<')
    .add(b'=')
    .add(b'>')
    .add(b'?')
    .add(b'@')
    .add(b'[')
    .add(b'\\')
    .add(b']')
    .add(b'^')
    .add(b'`')
    .add(b'{')
    .add(b'|')
    .add(b'}');

fn encode_query_param(value: &str) -> String {
    utf8_percent_encode(value, &QUERY_ENCODE_SET).to_string()
}

/// Build the OpenAI (ChatGPT/Codex) authorization URL. `port` must be one of
/// [`OPENAI_REDIRECT_PORTS`]; the redirect URI must exactly match the callback
/// used during token exchange.
pub fn openai_authorize_url(pkce: &Pkce, port: u16) -> String {
    let redirect_uri = format!("http://localhost:{port}/auth/callback");
    let params = [
        ("response_type", "code"),
        ("client_id", OPENAI_CLIENT_ID),
        ("redirect_uri", &redirect_uri),
        ("scope", OPENAI_SCOPES),
        ("code_challenge", &pkce.code_challenge),
        ("code_challenge_method", "S256"),
        ("id_token_add_organizations", "true"),
        ("codex_cli_simplified_flow", "true"),
        ("state", &pkce.state),
    ];
    let qs: Vec<String> = params
        .iter()
        .map(|(k, v)| format!("{k}={}", encode_query_param(v)))
        .collect();
    format!("{}/oauth/authorize?{}", OPENAI_ISSUER, qs.join("&"))
}

/// Build the Anthropic (Claude subscription) authorization URL. This flow uses
/// Anthropic's hosted redirect; after authorizing, the user pastes the code
/// (optionally `CODE#STATE`) back into the CLI.
pub fn anthropic_authorize_url(pkce: &Pkce) -> String {
    let params = [
        ("code", "true"),
        ("response_type", "code"),
        ("client_id", ANTHROPIC_CLIENT_ID),
        ("redirect_uri", ANTHROPIC_REDIRECT_URI),
        ("scope", ANTHROPIC_SCOPES),
        ("code_challenge", &pkce.code_challenge),
        ("code_challenge_method", "S256"),
        ("state", &pkce.state),
    ];
    let qs: Vec<String> = params
        .iter()
        .map(|(k, v)| format!("{k}={}", encode_query_param(v)))
        .collect();
    format!("{ANTHROPIC_AUTHORIZE_URL}?{}", qs.join("&"))
}

// ── Token model and store ─────────────────────────────────────────────────────

/// An OAuth token set for one provider.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct OAuthToken {
    pub access_token: String,
    #[serde(default)]
    pub refresh_token: Option<String>,
    /// Unix seconds at which the access token expires. `None` means unknown
    /// (treated as expired, so a refresh is attempted).
    #[serde(default)]
    pub expires_at: Option<u64>,
}

impl OAuthToken {
    /// Whether the access token is expired (or about to be).
    pub fn is_expired(&self) -> bool {
        match self.expires_at {
            Some(exp) => now_secs().saturating_add(REFRESH_SKEW_SECS) >= exp,
            None => true,
        }
    }
}

/// Persistent store of OAuth tokens, keyed by provider id.
/// File: `~/.io/oauth.toml` with `0600` permissions.
#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct OAuthStore {
    #[serde(default)]
    pub tokens: HashMap<String, OAuthToken>,
}

impl OAuthStore {
    pub fn load() -> Self {
        let path = Self::path();
        if path.exists() {
            std::fs::read_to_string(&path)
                .ok()
                .and_then(|s| toml::from_str(&s).ok())
                .unwrap_or_default()
        } else {
            Self::default()
        }
    }

    pub fn path() -> PathBuf {
        dirs::home_dir()
            .unwrap_or_else(|| PathBuf::from("~"))
            .join(".io")
            .join("oauth.toml")
    }

    pub fn get(&self, provider: &str) -> Option<&OAuthToken> {
        self.tokens.get(provider)
    }

    pub fn set(&mut self, provider: &str, token: OAuthToken) {
        self.tokens.insert(provider.to_string(), token);
    }

    pub fn remove(&mut self, provider: &str) {
        self.tokens.remove(provider);
    }

    pub fn save(&self) -> anyhow::Result<()> {
        let path = Self::path();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let contents = toml::to_string_pretty(self)?;
        std::fs::write(&path, contents)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))?;
        }
        Ok(())
    }
}

// ── Token exchange / refresh ──────────────────────────────────────────────────

/// Exchange an authorization code for tokens at the OpenAI token endpoint.
pub async fn exchange_openai_code(
    port: u16,
    code: &str,
    pkce: &Pkce,
) -> anyhow::Result<OAuthToken> {
    let redirect_uri = format!("http://localhost:{port}/auth/callback");
    let client = reqwest::Client::new();
    let resp = client
        .post(format!("{}/oauth/token", OPENAI_ISSUER))
        .form(&[
            ("grant_type", "authorization_code"),
            ("code", code),
            ("redirect_uri", &redirect_uri),
            ("client_id", OPENAI_CLIENT_ID),
            ("code_verifier", &pkce.code_verifier),
        ])
        .send()
        .await?;
    parse_token_response("OpenAI", resp).await
}

/// Exchange an authorization code (optionally `CODE#STATE`) for tokens at the
/// Anthropic token endpoint. When `state` is absent, the PKCE verifier is
/// forwarded as the state, matching the copy/paste flow's expectations.
pub async fn exchange_anthropic_code(
    code: &str,
    state: Option<&str>,
    pkce: &Pkce,
) -> anyhow::Result<OAuthToken> {
    let state = state.unwrap_or(&pkce.code_verifier);
    let client = reqwest::Client::new();
    let body = serde_json::json!({
        "grant_type": "authorization_code",
        "code": code,
        "state": state,
        "client_id": ANTHROPIC_CLIENT_ID,
        "redirect_uri": ANTHROPIC_REDIRECT_URI,
        "code_verifier": pkce.code_verifier,
    });
    let resp = client
        .post(ANTHROPIC_TOKEN_URL)
        .header("content-type", "application/json")
        .header("user-agent", "anthropic")
        .json(&body)
        .send()
        .await?;
    parse_token_response("Anthropic", resp).await
}

/// Refresh an access token from the OpenAI token endpoint using the stored
/// refresh token. The response may omit a new refresh token, in which case the
/// caller keeps the existing one.
pub async fn refresh_openai_token(token: &OAuthToken) -> anyhow::Result<OAuthToken> {
    let refresh_token = token
        .refresh_token
        .as_deref()
        .ok_or_else(|| anyhow::anyhow!("no OpenAI refresh token stored; run `io login openai`"))?;
    let client = reqwest::Client::new();
    let resp = client
        .post(format!("{}/oauth/token", OPENAI_ISSUER))
        .form(&[
            ("grant_type", "refresh_token"),
            ("client_id", OPENAI_CLIENT_ID),
            ("refresh_token", refresh_token),
        ])
        .send()
        .await?;
    parse_token_response("OpenAI", resp).await
}

/// Refresh an access token from the Anthropic token endpoint.
pub async fn refresh_anthropic_token(token: &OAuthToken) -> anyhow::Result<OAuthToken> {
    let refresh_token = token.refresh_token.as_deref().ok_or_else(|| {
        anyhow::anyhow!("no Anthropic refresh token stored; run `io login anthropic`")
    })?;
    let client = reqwest::Client::new();
    let body = serde_json::json!({
        "grant_type": "refresh_token",
        "refresh_token": refresh_token,
        "client_id": ANTHROPIC_CLIENT_ID,
    });
    let resp = client
        .post(ANTHROPIC_TOKEN_URL)
        .header("content-type", "application/json")
        .header("user-agent", "anthropic")
        .json(&body)
        .send()
        .await?;
    parse_token_response("Anthropic", resp).await
}

/// Provider id → refresh dispatch used by [`oauth_access_token`].
pub async fn refresh_token(provider: &str, token: &OAuthToken) -> anyhow::Result<OAuthToken> {
    match provider {
        "openai" => refresh_openai_token(token).await,
        "anthropic" => refresh_anthropic_token(token).await,
        other => anyhow::bail!("unsupported OAuth provider: {other}"),
    }
}

async fn parse_token_response(
    provider: &str,
    resp: reqwest::Response,
) -> anyhow::Result<OAuthToken> {
    let status = resp.status();
    let json: serde_json::Value = resp.json().await.map_err(|e| {
        anyhow::anyhow!("{provider} OAuth token response was not JSON ({status}): {e}")
    })?;
    if !status.is_success() {
        let fallback = json.to_string();
        let message = json["error_description"]
            .as_str()
            .or_else(|| json["error"].as_str())
            .unwrap_or(&fallback);
        anyhow::bail!("{provider} OAuth failed ({status}): {message}");
    }
    let access_token = json["access_token"]
        .as_str()
        .ok_or_else(|| anyhow::anyhow!("{provider} OAuth response missing access_token"))?
        .to_string();
    let expires_in = json["expires_in"].as_u64().unwrap_or(3600);
    Ok(OAuthToken {
        access_token,
        refresh_token: json["refresh_token"].as_str().map(str::to_string),
        expires_at: Some(now_secs().saturating_add(expires_in)),
    })
}

// ── Runtime credential resolution ─────────────────────────────────────────────

/// Load the provider's access token, refreshing and persisting it first when
/// it is expired (or about to expire). Errors explain how to recover, e.g.
/// re-running `io login`.
pub async fn oauth_access_token(provider: &str) -> anyhow::Result<String> {
    let mut store = OAuthStore::load();
    let token = store.get(provider).cloned().ok_or_else(|| {
        anyhow::anyhow!(
            "no OAuth login for {provider}; run `io login {provider}` or configure an API key"
        )
    })?;
    if token.is_expired() {
        match refresh_token(provider, &token).await {
            Ok(refreshed) => {
                let mut final_token = refreshed;
                // Some issuers don't rotate refresh tokens on refresh; keep the
                // existing one so we don't lock ourselves out.
                if final_token.refresh_token.is_none() {
                    final_token.refresh_token = token.refresh_token;
                }
                store.set(provider, final_token.clone());
                store.save()?;
                return Ok(final_token.access_token);
            }
            Err(e) => {
                anyhow::bail!(
                    "OAuth token for {provider} expired and refresh failed: {e}\n\
                     Re-run `io login {provider}` to sign in again."
                );
            }
        }
    }
    Ok(token.access_token)
}

/// Whether the provider has a stored, currently-valid OAuth login.
pub fn is_logged_in(provider: &str) -> bool {
    OAuthStore::load()
        .get(provider)
        .is_some_and(|t| !t.access_token.is_empty())
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pkce_verifier_is_valid_length_and_challenge_is_s256() {
        let pkce = generate_pkce();
        assert!((43..=128).contains(&pkce.code_verifier.len()));
        assert!(!pkce.state.is_empty() && pkce.state != pkce.code_verifier);
        // code_challenge == BASE64URL(SHA256(verifier)) without padding.
        let digest = Sha256::digest(pkce.code_verifier.as_bytes());
        assert_eq!(pkce.code_challenge, URL_SAFE_NO_PAD.encode(digest));
    }

    #[test]
    fn openai_authorize_url_has_expected_params() {
        let pkce = generate_pkce();
        let url = openai_authorize_url(&pkce, 1455);
        assert!(url.starts_with("https://auth.openai.com/oauth/authorize?"));
        for expected in [
            "response_type=code",
            &format!("client_id={OPENAI_CLIENT_ID}"),
            "redirect_uri=http%3A%2F%2Flocalhost%3A1455%2Fauth%2Fcallback",
            "code_challenge_method=S256",
            &format!("code_challenge={}", pkce.code_challenge),
            &format!("state={}", pkce.state),
        ] {
            assert!(url.contains(expected), "missing {expected} in {url}");
        }
    }

    #[test]
    fn anthropic_authorize_url_has_expected_params() {
        let pkce = generate_pkce();
        let url = anthropic_authorize_url(&pkce);
        assert!(url.starts_with("https://claude.ai/oauth/authorize?"));
        for expected in [
            "code=true",
            "response_type=code",
            &format!("client_id={ANTHROPIC_CLIENT_ID}"),
            &format!(
                "redirect_uri={}",
                encode_query_param(ANTHROPIC_REDIRECT_URI)
            ),
            "code_challenge_method=S256",
            &format!("code_challenge={}", pkce.code_challenge),
        ] {
            assert!(url.contains(expected), "missing {expected} in {url}");
        }
    }

    #[test]
    fn token_expiry_logic() {
        let mut token = OAuthToken {
            access_token: "a".into(),
            refresh_token: None,
            expires_at: Some(now_secs() + 3600),
        };
        assert!(!token.is_expired());
        token.expires_at = Some(now_secs() - 1);
        assert!(token.is_expired());
        // Unknown expiry is treated as expired so a refresh is attempted.
        token.expires_at = None;
        assert!(token.is_expired());
    }

    #[test]
    fn store_roundtrip_and_remove() {
        let mut store = OAuthStore::default();
        store.set(
            "openai",
            OAuthToken {
                access_token: "sk-oauth-123".into(),
                refresh_token: Some("rt-456".into()),
                expires_at: Some(now_secs() + 3600),
            },
        );
        assert_eq!(store.get("openai").unwrap().access_token, "sk-oauth-123");
        store.remove("openai");
        assert!(store.get("openai").is_none());
    }
}
