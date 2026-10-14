//! `web_search`: search the web and return titles, URLs and snippets.
//!
//! The backend is chosen from the environment — Brave Search (`BRAVE_API_KEY`)
//! or Tavily (`TAVILY_API_KEY`) when a key is present, otherwise DuckDuckGo's
//! keyless HTML endpoint. `IO_SEARCH_BACKEND=duckduckgo|brave|tavily` forces
//! one. Domain allow/block lists are applied to the results here, so they work
//! the same on every backend.

use super::html::{decode_entities, to_text};
use super::{Tool, ToolInput, ToolOutput};
use regex::Regex;
use std::sync::LazyLock;

const DEFAULT_RESULTS: usize = 8;
const MAX_RESULTS: usize = 15;
const REQUEST_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(15);
const USER_AGENT: &str = "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) \
                          Chrome/124.0 Safari/537.36";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchResult {
    pub title: String,
    pub url: String,
    pub snippet: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Backend {
    DuckDuckGo,
    Brave(String),
    Tavily(String),
}

impl Backend {
    fn name(&self) -> &'static str {
        match self {
            Backend::DuckDuckGo => "duckduckgo",
            Backend::Brave(_) => "brave",
            Backend::Tavily(_) => "tavily",
        }
    }

    /// Pick a backend from `(IO_SEARCH_BACKEND, BRAVE_API_KEY, TAVILY_API_KEY)`.
    fn choose(forced: Option<&str>, brave: Option<String>, tavily: Option<String>) -> Self {
        let brave = brave.filter(|k| !k.is_empty());
        let tavily = tavily.filter(|k| !k.is_empty());
        match forced.map(str::to_ascii_lowercase).as_deref() {
            Some("duckduckgo") | Some("ddg") => Backend::DuckDuckGo,
            Some("brave") => brave.map_or(Backend::DuckDuckGo, Backend::Brave),
            Some("tavily") => tavily.map_or(Backend::DuckDuckGo, Backend::Tavily),
            _ => match (brave, tavily) {
                (Some(k), _) => Backend::Brave(k),
                (None, Some(k)) => Backend::Tavily(k),
                _ => Backend::DuckDuckGo,
            },
        }
    }

    fn from_env() -> Self {
        Self::choose(
            std::env::var("IO_SEARCH_BACKEND").ok().as_deref(),
            std::env::var("BRAVE_API_KEY").ok(),
            std::env::var("TAVILY_API_KEY").ok(),
        )
    }
}

pub struct WebSearchTool {
    description: String,
}

impl WebSearchTool {
    pub fn new() -> Self {
        Self {
            description: format!(
                "Search the web and get back titles, URLs and snippets. Use it for current \
                 information beyond your training data, such as recent releases, docs or news. \
                 Today's date is {}, so include the current year in queries about recent \
                 things. Optionally restrict results with `allowed_domains` or exclude with \
                 `blocked_domains`. Use the fetch tool to read a result in full. When you use \
                 results in an answer, cite the source URLs.",
                chrono::Utc::now().format("%Y-%m-%d")
            ),
        }
    }
}

impl Default for WebSearchTool {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait::async_trait]
impl Tool for WebSearchTool {
    fn name(&self) -> &str {
        "web_search"
    }

    fn description(&self) -> &str {
        &self.description
    }

    fn input_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "query": { "type": "string", "description": "The search query" },
                "allowed_domains": {
                    "type": "array", "items": { "type": "string" },
                    "description": "Only return results from these domains (subdomains included)"
                },
                "blocked_domains": {
                    "type": "array", "items": { "type": "string" },
                    "description": "Never return results from these domains (subdomains included)"
                },
                "max_results": {
                    "type": "integer",
                    "description": "How many results to return (default 8, at most 15)"
                }
            },
            "required": ["query"]
        })
    }

    async fn execute(&self, input: ToolInput) -> ToolOutput {
        let Some(query) = input.args.get("query").and_then(|v| v.as_str()) else {
            return ToolOutput::err("missing required argument: query");
        };
        let query = query.trim();
        if query.chars().count() < 2 {
            return ToolOutput::err("query must be at least 2 characters");
        }
        let domains = |key: &str| -> Vec<String> {
            input
                .args
                .get(key)
                .and_then(|v| v.as_array())
                .map(|a| {
                    a.iter()
                        .filter_map(|d| d.as_str())
                        .map(normalize_domain)
                        .collect()
                })
                .unwrap_or_default()
        };
        let allowed = domains("allowed_domains");
        let blocked = domains("blocked_domains");
        let wanted = input
            .args
            .get("max_results")
            .and_then(|v| v.as_u64())
            .map_or(DEFAULT_RESULTS, |n| (n as usize).clamp(1, MAX_RESULTS));

        let backend = Backend::from_env();
        // Filters are applied after the fact, so ask for extra when they're set.
        let fetch_count = if allowed.is_empty() && blocked.is_empty() {
            wanted
        } else {
            (wanted * 3).min(20)
        };
        let raw = match search(&backend, query, fetch_count).await {
            Ok(r) => r,
            Err(e) => {
                return ToolOutput::err(format!("web search failed ({}): {e}", backend.name()))
            }
        };
        let results: Vec<SearchResult> = filter_results(raw, &allowed, &blocked)
            .into_iter()
            .take(wanted)
            .collect();
        ToolOutput::ok(format_results(query, backend.name(), &results))
    }
}

// ── Backends ───────────────────────────────────────────────────────────────────

async fn search(backend: &Backend, query: &str, count: usize) -> Result<Vec<SearchResult>, String> {
    let client = reqwest::Client::builder()
        .timeout(REQUEST_TIMEOUT)
        .user_agent(USER_AGENT)
        .build()
        .map_err(|e| format!("failed to build HTTP client: {e}"))?;
    match backend {
        Backend::DuckDuckGo => {
            let resp = client
                .get("https://html.duckduckgo.com/html/")
                .query(&[("q", query)])
                .send()
                .await
                .map_err(|e| e.to_string())?;
            let status = resp.status();
            let body = resp.text().await.map_err(|e| e.to_string())?;
            if !status.is_success() && status.as_u16() != 202 {
                return Err(format!("HTTP {status}"));
            }
            let results = parse_duckduckgo(&body);
            if results.is_empty() && (status.as_u16() == 202 || body.contains("anomaly")) {
                return Err(
                    "DuckDuckGo is rate-limiting this client. Set BRAVE_API_KEY or TAVILY_API_KEY \
                     for a dependable backend"
                        .to_string(),
                );
            }
            Ok(results)
        }
        Backend::Brave(key) => {
            let resp = client
                .get("https://api.search.brave.com/res/v1/web/search")
                .query(&[("q", query), ("count", &count.min(20).to_string())])
                .header("X-Subscription-Token", key)
                .header("Accept", "application/json")
                .send()
                .await
                .map_err(|e| e.to_string())?;
            let status = resp.status();
            let body = resp.text().await.map_err(|e| e.to_string())?;
            if !status.is_success() {
                return Err(format!(
                    "HTTP {status}: {}",
                    body.chars().take(200).collect::<String>()
                ));
            }
            parse_brave(&body)
        }
        Backend::Tavily(key) => {
            let resp = client
                .post("https://api.tavily.com/search")
                .json(&serde_json::json!({
                    "api_key": key, "query": query, "max_results": count.min(20),
                }))
                .send()
                .await
                .map_err(|e| e.to_string())?;
            let status = resp.status();
            let body = resp.text().await.map_err(|e| e.to_string())?;
            if !status.is_success() {
                return Err(format!(
                    "HTTP {status}: {}",
                    body.chars().take(200).collect::<String>()
                ));
            }
            parse_tavily(&body)
        }
    }
}

// ── Parsing ────────────────────────────────────────────────────────────────────

static DDG_ANCHOR: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"(?is)<a\b([^>]*)>(.*?)</a\s*>"#).expect("valid regex"));
static HREF: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"(?is)\bhref\s*=\s*(?:"([^"]*)"|'([^']*)')"#).expect("valid regex")
});
static DDG_SNIPPET_CELL: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"(?is)<td\b[^>]*class\s*=\s*"[^"]*result-snippet[^"]*"[^>]*>(.*?)</td\s*>"#)
        .expect("valid regex")
});

/// Resolve a DuckDuckGo result link to the real target. Results are wrapped as
/// `//duckduckgo.com/l/?uddg=<percent-encoded target>&rut=…`; ads go through
/// `/y.js` and are dropped (`None`).
fn ddg_target(href: &str) -> Option<String> {
    let href = decode_entities(href);
    if href.contains("duckduckgo.com/y.js") {
        return None;
    }
    if let Some(i) = href.find("uddg=") {
        let encoded = href[i + 5..].split('&').next().unwrap_or("");
        let decoded = percent_encoding::percent_decode_str(encoded)
            .decode_utf8()
            .ok()?
            .into_owned();
        return decoded.starts_with("http").then_some(decoded);
    }
    let direct = href
        .strip_prefix("//")
        .map_or(href.clone(), |h| format!("https://{h}"));
    direct.starts_with("http").then_some(direct)
}

fn clean_text(fragment: &str) -> String {
    let text = to_text(fragment);
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Parse DuckDuckGo's HTML results page. A title anchor (`result__a`) starts a
/// result; the next snippet (`result__snippet` anchor or `result-snippet`
/// cell) before the following title belongs to it.
pub fn parse_duckduckgo(page: &str) -> Vec<SearchResult> {
    struct Hit {
        title: String,
        url: String,
        snippet: String,
    }
    let mut hits: Vec<Hit> = Vec::new();
    let mut skipping_ad = false;

    // Snippets in lite-style markup are table cells, not anchors; index them by
    // position so they can be paired the same way.
    let cell_snippets: Vec<(usize, String)> = DDG_SNIPPET_CELL
        .captures_iter(page)
        .map(|c| (c.get(0).unwrap().start(), clean_text(&c[1])))
        .collect();
    let mut cell_iter = cell_snippets.iter().peekable();

    for caps in DDG_ANCHOR.captures_iter(page) {
        let attrs = &caps[1];
        let inner = &caps[2];
        let pos = caps.get(0).unwrap().start();
        if attrs.contains("result__a") {
            let href = HREF
                .captures(attrs)
                .and_then(|h| h.get(1).or_else(|| h.get(2)))
                .map(|m| m.as_str())
                .unwrap_or("");
            match ddg_target(href) {
                Some(url) => {
                    skipping_ad = false;
                    hits.push(Hit {
                        title: clean_text(inner),
                        url,
                        snippet: String::new(),
                    });
                }
                None => skipping_ad = true,
            }
        } else if attrs.contains("result__snippet") && !skipping_ad {
            if let Some(last) = hits.last_mut() {
                if last.snippet.is_empty() {
                    last.snippet = clean_text(inner);
                }
            }
        }
        // Table-cell snippets that appear before the next anchor.
        while let Some((cell_pos, text)) = cell_iter.peek() {
            if *cell_pos > pos {
                break;
            }
            if let (Some(last), false) = (hits.last_mut(), skipping_ad) {
                if last.snippet.is_empty() {
                    last.snippet = text.clone();
                }
            }
            cell_iter.next();
        }
    }
    hits.into_iter()
        .filter(|h| !h.title.is_empty())
        .map(|h| SearchResult {
            title: h.title,
            url: h.url,
            snippet: h.snippet,
        })
        .collect()
}

fn parse_brave(body: &str) -> Result<Vec<SearchResult>, String> {
    let v: serde_json::Value =
        serde_json::from_str(body).map_err(|e| format!("unreadable response: {e}"))?;
    let results = v["web"]["results"].as_array().cloned().unwrap_or_default();
    Ok(results
        .iter()
        .filter_map(|r| {
            Some(SearchResult {
                title: clean_text(r["title"].as_str()?),
                url: r["url"].as_str()?.to_string(),
                snippet: clean_text(r["description"].as_str().unwrap_or("")),
            })
        })
        .collect())
}

fn parse_tavily(body: &str) -> Result<Vec<SearchResult>, String> {
    let v: serde_json::Value =
        serde_json::from_str(body).map_err(|e| format!("unreadable response: {e}"))?;
    let results = v["results"].as_array().cloned().unwrap_or_default();
    Ok(results
        .iter()
        .filter_map(|r| {
            Some(SearchResult {
                title: clean_text(r["title"].as_str()?),
                url: r["url"].as_str()?.to_string(),
                snippet: clean_text(r["content"].as_str().unwrap_or("")),
            })
        })
        .collect())
}

// ── Filtering and formatting ───────────────────────────────────────────────────

/// Lowercase a domain and drop any scheme, `www.` prefix, path and port, so
/// `https://www.Example.com/docs` and `example.com` compare equal.
fn normalize_domain(d: &str) -> String {
    let d = d.trim().to_ascii_lowercase();
    let d = d.split("://").last().unwrap_or(&d);
    let d = d.split(['/', '?', '#']).next().unwrap_or(d);
    let d = d.rsplit('@').next().unwrap_or(d);
    let d = d.split(':').next().unwrap_or(d);
    d.strip_prefix("www.").unwrap_or(d).to_string()
}

fn host_of(url: &str) -> Option<String> {
    reqwest::Url::parse(url)
        .ok()
        .and_then(|u| u.host_str().map(normalize_domain))
}

/// Whether `host` is `domain` or one of its subdomains.
fn domain_matches(host: &str, domain: &str) -> bool {
    !domain.is_empty() && (host == domain || host.ends_with(&format!(".{domain}")))
}

/// Apply allow/block domain lists. Blocking wins over allowing; results with
/// an unparseable URL are dropped when an allow list is set.
pub fn filter_results(
    results: Vec<SearchResult>,
    allowed: &[String],
    blocked: &[String],
) -> Vec<SearchResult> {
    results
        .into_iter()
        .filter(|r| {
            let Some(host) = host_of(&r.url) else {
                return allowed.is_empty();
            };
            if blocked.iter().any(|d| domain_matches(&host, d)) {
                return false;
            }
            allowed.is_empty() || allowed.iter().any(|d| domain_matches(&host, d))
        })
        .collect()
}

fn format_results(query: &str, backend: &str, results: &[SearchResult]) -> String {
    if results.is_empty() {
        return format!(
            "No web results for \"{query}\" (via {backend}). Try different keywords, or relax \
             the domain filters."
        );
    }
    let mut out = format!("Web search results for \"{query}\" (via {backend}):\n");
    for (i, r) in results.iter().enumerate() {
        out.push_str(&format!("\n{}. {}\n   {}\n", i + 1, r.title, r.url));
        if !r.snippet.is_empty() {
            out.push_str(&format!("   {}\n", r.snippet));
        }
    }
    out.push_str("\nCite the source URLs when you use these results.");
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const DDG_PAGE: &str = r#"
<div class="results">
 <div class="result results_links results_links_deep web-result">
  <h2 class="result__title">
   <a rel="nofollow" class="result__a" href="//duckduckgo.com/l/?uddg=https%3A%2F%2Fdoc.rust-lang.org%2Fbook%2F&amp;rut=abc">The Rust <b>Programming</b> Language</a>
  </h2>
  <a class="result__snippet" href="//duckduckgo.com/l/?uddg=https%3A%2F%2Fdoc.rust-lang.org%2Fbook%2F&amp;rut=abc">An introductory <b>book</b> about Rust &amp; its tooling.</a>
 </div>
 <div class="result results_links result--ad">
  <a rel="nofollow" class="result__a" href="https://duckduckgo.com/y.js?ad_domain=spam.example&amp;u3=zzz">Buy stuff</a>
  <a class="result__snippet" href="https://duckduckgo.com/y.js?x=1">Ad snippet</a>
 </div>
 <div class="result results_links">
  <a class="result__a" href="https://blog.example.org/post?a=1&amp;b=2">Blog post</a>
  <a class="result__snippet" href="https://blog.example.org/post">Thoughts on <b>Rust</b>.</a>
 </div>
 <div class="result">
  <a rel="nofollow" class="result__a" href="//duckduckgo.com/l/?uddg=https%3A%2F%2Fcrates.io%2F">crates.io</a>
 </div>
</div>"#;

    fn result(title: &str, url: &str) -> SearchResult {
        SearchResult {
            title: title.into(),
            url: url.into(),
            snippet: String::new(),
        }
    }

    #[test]
    fn duckduckgo_results_are_parsed_unwrapped_and_cleaned() {
        let results = parse_duckduckgo(DDG_PAGE);
        assert_eq!(results.len(), 3, "the ad is dropped: {results:#?}");
        assert_eq!(results[0].title, "The Rust Programming Language");
        assert_eq!(results[0].url, "https://doc.rust-lang.org/book/");
        assert_eq!(
            results[0].snippet,
            "An introductory book about Rust & its tooling."
        );
        // A direct (non-wrapped) link keeps its query string, entity-decoded.
        assert_eq!(results[1].url, "https://blog.example.org/post?a=1&b=2");
        assert_eq!(results[1].snippet, "Thoughts on Rust.");
        // A result with no snippet is still returned.
        assert_eq!(results[2].url, "https://crates.io/");
        assert_eq!(results[2].snippet, "");
    }

    #[test]
    fn an_ads_snippet_is_not_attached_to_the_previous_result() {
        let page = r#"
          <a class="result__a" href="//duckduckgo.com/l/?uddg=https%3A%2F%2Fa.dev%2F">A</a>
          <a class="result__a" href="https://duckduckgo.com/y.js?ad=1">Ad</a>
          <a class="result__snippet" href="x">ad snippet</a>"#;
        let results = parse_duckduckgo(page);
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].snippet, "");
    }

    #[test]
    fn pages_with_no_results_parse_to_nothing() {
        assert!(parse_duckduckgo("<html><body>No results.</body></html>").is_empty());
    }

    #[test]
    fn brave_and_tavily_responses_parse() {
        let brave = parse_brave(
            r#"{"web":{"results":[
                {"title":"A <strong>hit</strong>","url":"https://a.dev/x","description":"desc &amp; more"},
                {"url":"https://no-title.dev"}]}}"#,
        )
        .unwrap();
        assert_eq!(brave.len(), 1);
        assert_eq!(brave[0].title, "A hit");
        assert_eq!(brave[0].snippet, "desc & more");

        let tavily = parse_tavily(
            r#"{"results":[{"title":"T","url":"https://t.dev","content":"body text"}]}"#,
        )
        .unwrap();
        assert_eq!(tavily[0].snippet, "body text");

        assert!(parse_brave("not json").is_err());
        assert!(parse_tavily("{}").unwrap().is_empty());
    }

    #[test]
    fn domain_filters_match_subdomains_and_block_wins() {
        let results = vec![
            result("docs", "https://docs.rust-lang.org/std"),
            result("main", "https://www.rust-lang.org/"),
            result("other", "https://example.com/rust-lang.org"),
            result("evil", "https://notrust-lang.org/"),
        ];
        let only_rust = filter_results(results.clone(), &["rust-lang.org".to_string()], &[]);
        let titles: Vec<&str> = only_rust.iter().map(|r| r.title.as_str()).collect();
        assert_eq!(titles, ["docs", "main"]);

        let no_docs = filter_results(
            results.clone(),
            &["rust-lang.org".to_string()],
            &["docs.rust-lang.org".to_string()],
        );
        assert_eq!(no_docs.len(), 1);
        assert_eq!(no_docs[0].title, "main");

        let blocked_only = filter_results(results, &[], &["example.com".to_string()]);
        assert_eq!(blocked_only.len(), 3);
    }

    #[test]
    fn domains_are_normalized() {
        for raw in [
            "https://www.Example.com/docs?x=1",
            "EXAMPLE.com",
            "example.com:8080",
            "www.example.com",
        ] {
            assert_eq!(normalize_domain(raw), "example.com", "{raw}");
        }
    }

    #[test]
    fn backend_selection_follows_keys_and_the_override() {
        use Backend::*;
        let k = |s: &str| Some(s.to_string());
        assert_eq!(Backend::choose(None, None, None), DuckDuckGo);
        assert_eq!(Backend::choose(None, k("b"), k("t")), Brave("b".into()));
        assert_eq!(Backend::choose(None, None, k("t")), Tavily("t".into()));
        assert_eq!(Backend::choose(None, k(""), None), DuckDuckGo);
        assert_eq!(
            Backend::choose(Some("duckduckgo"), k("b"), None),
            DuckDuckGo
        );
        assert_eq!(
            Backend::choose(Some("TAVILY"), k("b"), k("t")),
            Tavily("t".into())
        );
        // Forcing a backend without its key falls back rather than failing.
        assert_eq!(Backend::choose(Some("brave"), None, k("t")), DuckDuckGo);
    }

    #[test]
    fn results_are_formatted_for_the_model() {
        let text = format_results(
            "rust book",
            "duckduckgo",
            &[SearchResult {
                title: "The Book".into(),
                url: "https://doc.rust-lang.org/book/".into(),
                snippet: "Learn Rust".into(),
            }],
        );
        assert!(text.starts_with("Web search results for \"rust book\" (via duckduckgo):"));
        assert!(text.contains("1. The Book\n   https://doc.rust-lang.org/book/\n   Learn Rust"));
        assert!(text.contains("Cite the source URLs"));
        assert!(format_results("zzz", "brave", &[]).starts_with("No web results"));
    }

    #[tokio::test]
    async fn validates_the_query() {
        let tool = WebSearchTool::new();
        let call = |args: serde_json::Value| {
            let args = args
                .as_object()
                .unwrap()
                .iter()
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect();
            tool.execute(ToolInput {
                name: "web_search".into(),
                args,
            })
        };
        assert!(call(serde_json::json!({}))
            .await
            .data
            .contains("missing required argument: query"));
        assert!(call(serde_json::json!({"query": " a "}))
            .await
            .data
            .contains("at least 2 characters"));
    }

    #[test]
    fn description_mentions_todays_date() {
        let today = chrono::Utc::now().format("%Y-%m-%d").to_string();
        assert!(WebSearchTool::new().description().contains(&today));
    }
}
