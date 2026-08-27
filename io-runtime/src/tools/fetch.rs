use super::{Tool, ToolInput, ToolOutput};

const MAX_FETCH_BYTES: usize = 100 * 1024;
const CONNECT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);
const REQUEST_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(20);
const MAX_REDIRECTS: usize = 10;

pub struct FetchTool;

#[async_trait::async_trait]
impl Tool for FetchTool {
    fn name(&self) -> &str {
        "fetch"
    }

    fn description(&self) -> &str {
        "Fetch a URL via HTTP(S) GET and return the response body as text (truncated if very \
         large). Best suited for text, HTML, JSON, or markdown content — binary responses \
         (images, PDFs, etc.) will not decode meaningfully. Requests to localhost, private, and \
         link-local addresses are blocked; this is a hostname/literal-IP check, not full SSRF \
         protection (a public hostname that resolves to a private address is not caught)."
    }

    fn input_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "url": { "type": "string", "description": "The http:// or https:// URL to fetch" }
            },
            "required": ["url"]
        })
    }

    async fn execute(&self, input: ToolInput) -> ToolOutput {
        let url_str = match input.args.get("url").and_then(|v| v.as_str()) {
            Some(u) => u,
            None => return ToolOutput::err("missing required argument: url"),
        };

        let url = match reqwest::Url::parse(url_str) {
            Ok(u) => u,
            Err(e) => return ToolOutput::err(format!("invalid URL: {e}")),
        };
        if url.scheme() != "http" && url.scheme() != "https" {
            return ToolOutput::err(format!(
                "unsupported URL scheme '{}' — only http/https are allowed",
                url.scheme()
            ));
        }
        if url.host_str().map(is_blocked_host).unwrap_or(true) {
            return ToolOutput::err(
                "refusing to fetch a localhost, private, or link-local address",
            );
        }

        let client = match reqwest::Client::builder()
            .connect_timeout(CONNECT_TIMEOUT)
            .timeout(REQUEST_TIMEOUT)
            .redirect(reqwest::redirect::Policy::custom(|attempt| {
                if attempt.previous().len() > MAX_REDIRECTS {
                    attempt.error("too many redirects")
                } else if attempt
                    .url()
                    .host_str()
                    .map(is_blocked_host)
                    .unwrap_or(true)
                {
                    attempt.stop()
                } else {
                    attempt.follow()
                }
            }))
            .build()
        {
            Ok(c) => c,
            Err(e) => return ToolOutput::err(format!("failed to build HTTP client: {e}")),
        };

        let response = match client.get(url).send().await {
            Ok(r) => r,
            Err(e) => return ToolOutput::err(format!("request failed: {e}")),
        };

        let status = response.status();
        let body = match response.text().await {
            Ok(b) => b,
            Err(e) => return ToolOutput::err(format!("failed to read response body: {e}")),
        };

        if !status.is_success() {
            return ToolOutput::err(format!("HTTP {status}\n{}", truncate(&body)));
        }

        ToolOutput::ok(truncate(&body))
    }
}

fn truncate(body: &str) -> String {
    if body.len() <= MAX_FETCH_BYTES {
        return body.to_string();
    }
    let mut end = MAX_FETCH_BYTES;
    while end > 0 && !body.is_char_boundary(end) {
        end -= 1;
    }
    format!(
        "{}\n… (truncated, {} bytes total)",
        &body[..end],
        body.len()
    )
}

/// Blocks fetches to localhost, private, and link-local addresses, plus the
/// cloud-metadata IP. This is a hostname/literal-IP string check only — it
/// does not resolve DNS, so a public hostname that resolves to a private
/// address at request time is not caught (no protection against DNS
/// rebinding). Applied to both the initial request and every redirect hop.
fn is_blocked_host(host: &str) -> bool {
    // `Url::host_str()` wraps IPv6 literals in brackets (e.g. "[::1]"),
    // which `IpAddr`'s parser rejects — strip them before parsing.
    let lower = host
        .trim_start_matches('[')
        .trim_end_matches(']')
        .to_ascii_lowercase();
    if lower == "localhost" || lower == "169.254.169.254" {
        return true;
    }
    if let Ok(ip) = lower.parse::<std::net::IpAddr>() {
        return match ip {
            std::net::IpAddr::V4(v4) => {
                v4.is_loopback() || v4.is_private() || v4.is_link_local() || v4.is_unspecified()
            }
            std::net::IpAddr::V6(v6) => v6.is_loopback() || v6.is_unspecified(),
        };
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    fn input(args: serde_json::Value) -> ToolInput {
        ToolInput {
            name: "fetch".into(),
            args: args
                .as_object()
                .unwrap()
                .iter()
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect(),
        }
    }

    #[tokio::test]
    async fn missing_url_returns_error() {
        let out = FetchTool.execute(input(serde_json::json!({}))).await;
        assert!(!out.success);
        assert!(out.data.contains("missing required argument: url"));
    }

    #[tokio::test]
    async fn invalid_url_returns_error() {
        let out = FetchTool
            .execute(input(serde_json::json!({ "url": "not a url" })))
            .await;
        assert!(!out.success);
        assert!(out.data.contains("invalid URL"));
    }

    #[tokio::test]
    async fn unsupported_scheme_returns_error() {
        let out = FetchTool
            .execute(input(serde_json::json!({ "url": "file:///etc/passwd" })))
            .await;
        assert!(!out.success);
        assert!(out.data.contains("unsupported URL scheme"));
    }

    #[tokio::test]
    async fn blocked_hosts_return_error() {
        for url in [
            "http://localhost/",
            "http://127.0.0.1/",
            "http://[::1]/",
            "http://169.254.169.254/latest/meta-data/",
            "http://10.0.0.5/",
            "http://192.168.1.1/",
            "http://0.0.0.0/",
        ] {
            let out = FetchTool
                .execute(input(serde_json::json!({ "url": url })))
                .await;
            assert!(!out.success, "{url} should be blocked");
            assert!(out.data.contains("localhost, private, or link-local"));
        }
    }

    #[test]
    fn is_blocked_host_allows_public_hosts() {
        for host in ["example.com", "api.github.com", "8.8.8.8"] {
            assert!(!is_blocked_host(host), "{host} should not be blocked");
        }
    }

    #[test]
    fn truncate_leaves_short_bodies_untouched() {
        assert_eq!(truncate("hello"), "hello");
    }

    #[test]
    fn truncate_caps_long_bodies() {
        let long = "x".repeat(MAX_FETCH_BYTES + 100);
        let out = truncate(&long);
        assert!(out.len() < long.len());
        assert!(out.contains("truncated"));
    }

    /// Hits the real network — not run by default (`cargo test` skips
    /// `#[ignore]`d tests). Run with `cargo test -- --ignored` to manually
    /// verify the happy path against a live endpoint.
    #[tokio::test]
    #[ignore]
    async fn live_fetch_returns_page_content() {
        let out = FetchTool
            .execute(input(serde_json::json!({ "url": "https://example.com" })))
            .await;
        assert!(out.success, "{}", out.data);
        assert!(out.data.contains("Example Domain"));
    }
}
