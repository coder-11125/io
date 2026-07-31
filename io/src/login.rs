//! `io login <provider>` — OAuth sign-in for OpenAI (ChatGPT) and Anthropic
//! (Claude subscription) models, in addition to API keys.
//!
//! Tokens are stored in `~/.io/oauth.toml` and refreshed automatically by the
//! providers when they expire. Run `io connect` for API-key setup.

use std::collections::HashMap;
use std::io::Write;
use std::process::Command;
use std::time::Duration;

use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::{TcpListener, TcpStream};

use io_runtime::config::{AuthMethod, Config};
use io_runtime::oauth::{self, OAuthToken};

/// How long to wait for the OpenAI browser callback.
const CALLBACK_TIMEOUT: Duration = Duration::from_secs(300);

pub async fn run(provider: &str) -> anyhow::Result<()> {
    match provider {
        "openai" => login_openai().await,
        "anthropic" => login_anthropic().await,
        _ => anyhow::bail!(
            "OAuth login is only supported for openai (ChatGPT) and anthropic (Claude). \
             Got: {provider}"
        ),
    }
}

// ── OpenAI (ChatGPT / Codex) — local callback server ─────────────────────────

async fn login_openai() -> anyhow::Result<()> {
    let pkce = oauth::generate_pkce();
    let (listener, port) = bind_callback_port().await?;
    let auth_url = oauth::openai_authorize_url(&pkce, port);

    println!();
    println!("  1. Open this URL in your browser:");
    println!("     {auth_url}");
    open_browser(&auth_url);
    println!("  2. Sign in with ChatGPT and approve access.");
    println!("     Waiting for the browser callback…");
    println!();

    let code = wait_for_openai_callback(listener, &pkce.state).await?;
    let token = oauth::exchange_openai_code(port, &code, &pkce).await?;
    save_token("openai", token)?;
    Ok(())
}

async fn bind_callback_port() -> anyhow::Result<(TcpListener, u16)> {
    for port in oauth::OPENAI_REDIRECT_PORTS {
        if let Ok(listener) = TcpListener::bind(("127.0.0.1", *port)).await {
            return Ok((listener, *port));
        }
    }
    anyhow::bail!(
        "could not bind a local callback server on ports {:?} — close whatever is \
         using them and retry `io login openai`",
        oauth::OPENAI_REDIRECT_PORTS
    )
}

/// Wait for the OAuth redirect back to `http://localhost:<port>/auth/callback`.
/// Validates the CSRF state, ignores stray requests (favicon etc.), and times
/// out if the user never finishes the browser flow.
async fn wait_for_openai_callback(
    listener: TcpListener,
    expected_state: &str,
) -> anyhow::Result<String> {
    loop {
        let (mut socket, _) = tokio::time::timeout(CALLBACK_TIMEOUT, listener.accept())
            .await
            .map_err(|_| {
                anyhow::anyhow!(
                    "timed out waiting for the browser callback ({}s). Try `io login openai` again.",
                    CALLBACK_TIMEOUT.as_secs()
                )
            })??;

        let mut buf = [0u8; 8192];
        let n = socket.read(&mut buf).await?;
        let request = String::from_utf8_lossy(&buf[..n]).to_string();
        let path = request
            .lines()
            .next()
            .and_then(|l| l.split_whitespace().nth(1))
            .unwrap_or("/");
        let params: HashMap<String, String> = url::Url::parse(&format!("http://localhost{path}"))
            .map(|u| u.query_pairs().into_owned().collect())
            .unwrap_or_default();

        if let Some(err) = params.get("error") {
            let desc = params
                .get("error_description")
                .map(String::as_str)
                .unwrap_or("");
            respond(&mut socket, 400, "Authorization failed").await?;
            anyhow::bail!("OpenAI authorization failed: {err} {desc}");
        }
        if params.get("state").map(String::as_str) != Some(expected_state) {
            // Stray request (e.g. favicon); keep listening.
            respond(&mut socket, 404, "Not found").await?;
            continue;
        }
        let Some(code) = params.get("code") else {
            respond(&mut socket, 400, "Missing authorization code").await?;
            anyhow::bail!("OpenAI callback was missing the authorization code");
        };
        respond(
            &mut socket,
            200,
            "You're signed in. Close this tab and return to the terminal.",
        )
        .await?;
        return Ok(code.clone());
    }
}

// ── Anthropic (Claude subscription) — copy/paste flow ────────────────────────

async fn login_anthropic() -> anyhow::Result<()> {
    let pkce = oauth::generate_pkce();
    let auth_url = oauth::anthropic_authorize_url(&pkce);

    println!();
    println!("  1. Open this URL in your browser:");
    println!("     {auth_url}");
    open_browser(&auth_url);
    println!("  2. Sign in with Claude and approve access.");
    println!(
        "  3. Paste the code shown on the page (if it shows CODE#STATE, paste the full string):"
    );
    print!("     Code: ");
    std::io::stdout().flush()?;

    let mut lines = BufReader::new(tokio::io::stdin()).lines();
    let input = lines.next_line().await?.unwrap_or_default();
    let input = input.trim().to_string();
    if input.is_empty() {
        anyhow::bail!("no code pasted — login aborted");
    }

    let (code, state) = match input.split_once('#') {
        Some((code, state)) => (code.to_string(), Some(state.to_string())),
        None => (input, None),
    };
    let token = oauth::exchange_anthropic_code(&code, state.as_deref(), &pkce).await?;
    save_token("anthropic", token)?;
    Ok(())
}

// ── Shared ───────────────────────────────────────────────────────────────────

/// Persist the token, mark the provider as OAuth-authenticated, and make it
/// the active provider.
fn save_token(provider: &str, token: OAuthToken) -> anyhow::Result<()> {
    let mut store = oauth::OAuthStore::load();
    store.set(provider, token);
    store.save()?;

    let mut config = Config::load()?;
    config.provider.default = provider.to_string();
    match provider {
        "openai" => {
            let c = config.provider.openai.get_or_insert_with(Default::default);
            c.auth = AuthMethod::OAuth;
        }
        "anthropic" => {
            let c = config
                .provider
                .anthropic
                .get_or_insert_with(Default::default);
            c.auth = AuthMethod::OAuth;
        }
        _ => unreachable!("provider validated in run()"),
    }
    config.save()?;

    println!();
    println!(
        "  Saved OAuth login for {provider} to {}",
        oauth::OAuthStore::path().display()
    );
    println!("  Active provider set to: {provider}");
    println!();
    Ok(())
}

async fn respond(socket: &mut TcpStream, status: u16, message: &str) -> anyhow::Result<()> {
    let reason = if status == 200 { "OK" } else { "Error" };
    let body = format!(
        "<html><body style='font-family:system-ui;padding:2rem'><h2>io</h2><p>{message}</p></body></html>"
    );
    let resp = format!(
        "HTTP/1.1 {status} {reason}\r\nContent-Type: text/html; charset=utf-8\r\n\
         Content-Length: {}\r\nConnection: close\r\n\r\n{}",
        body.len(),
        body
    );
    socket.write_all(resp.as_bytes()).await?;
    Ok(())
}

fn open_browser(url: &str) {
    #[cfg(target_os = "macos")]
    let _ = Command::new("open").arg(url).spawn();
    #[cfg(target_os = "linux")]
    let _ = Command::new("xdg-open").arg(url).spawn();
    #[cfg(target_os = "windows")]
    let _ = Command::new("cmd").args(["/C", "start", "", url]).spawn();
    #[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
    let _ = url;
}
