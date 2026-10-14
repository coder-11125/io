//! A small HTML → readable-text converter for the web tools. It is not a
//! full parser: it drops non-content elements, keeps headings, lists, links
//! and table cells in a markdown-ish form, strips the remaining tags and
//! decodes entities — enough to cut a web page down to what a model should read.

use regex::Regex;
use std::sync::LazyLock;

fn re(pattern: &str) -> Regex {
    Regex::new(pattern).expect("static regex is valid")
}

static COMMENTS: LazyLock<Regex> = LazyLock::new(|| re(r"(?s)<!--.*?-->"));
static NON_CONTENT: LazyLock<Regex> = LazyLock::new(|| {
    re(
        r"(?is)<(script|style|noscript|svg|head|template|iframe)\b[^>]*>.*?</\s*(script|style|noscript|svg|head|template|iframe)\s*>",
    )
});
static TITLE: LazyLock<Regex> = LazyLock::new(|| re(r"(?is)<title[^>]*>(.*?)</title>"));
static LINK: LazyLock<Regex> = LazyLock::new(|| {
    re(r#"(?is)<a\b[^>]*?\bhref\s*=\s*(?:"([^"]*)"|'([^']*)')[^>]*>(.*?)</a\s*>"#)
});
static HEADING: LazyLock<Regex> = LazyLock::new(|| re(r"(?is)<h([1-6])\b[^>]*>(.*?)</h[1-6]\s*>"));
static LIST_ITEM: LazyLock<Regex> = LazyLock::new(|| re(r"(?i)<li\b[^>]*>"));
static CELL: LazyLock<Regex> = LazyLock::new(|| re(r"(?i)</t[dh]\s*>"));
static BREAKS: LazyLock<Regex> = LazyLock::new(|| {
    re(
        r"(?i)<br\s*/?>|</(p|div|section|article|header|footer|nav|main|ul|ol|tr|table|pre|blockquote|h[1-6])\s*>|<(p|div|section|article|tr|pre|blockquote)\b[^>]*>",
    )
});
static TAGS: LazyLock<Regex> = LazyLock::new(|| re(r"(?s)<[^>]*>"));
static NUMERIC_ENTITY: LazyLock<Regex> = LazyLock::new(|| re(r"&#(x[0-9a-fA-F]+|[0-9]+);"));
static HORIZONTAL_SPACE: LazyLock<Regex> = LazyLock::new(|| re(r"[ \t\u{a0}]+"));
static BLANK_RUNS: LazyLock<Regex> = LazyLock::new(|| re(r"\n{3,}"));

/// Whether `body` looks like an HTML document (for responses with a missing
/// or generic content type).
pub fn looks_like_html(body: &str) -> bool {
    let head: String = body.trim_start().chars().take(200).collect();
    let head = head.to_lowercase();
    head.starts_with("<!doctype html") || head.starts_with("<html") || head.contains("<head")
}

/// The document `<title>`, if any.
pub fn title(html: &str) -> Option<String> {
    TITLE
        .captures(html)
        .map(|c| decode_entities(&strip_tags(&c[1])).trim().to_string())
        .filter(|t| !t.is_empty())
}

fn strip_tags(s: &str) -> String {
    TAGS.replace_all(s, "").into_owned()
}

/// Convert an HTML document to readable text.
pub fn to_text(html: &str) -> String {
    let page_title = title(html);
    let s = COMMENTS.replace_all(html, "");
    let s = NON_CONTENT.replace_all(&s, "");
    let s = LINK.replace_all(&s, |c: &regex::Captures| {
        let href = c.get(1).or_else(|| c.get(2)).map_or("", |m| m.as_str());
        let text = strip_tags(&c[3]);
        let text = text.trim();
        if text.is_empty()
            || href.is_empty()
            || href.starts_with('#')
            || href.starts_with("javascript:")
        {
            text.to_string()
        } else {
            format!("[{text}]({href})")
        }
    });
    let s = HEADING.replace_all(&s, |c: &regex::Captures| {
        let level: usize = c[1].parse().unwrap_or(1);
        format!("\n\n{} {}\n\n", "#".repeat(level), strip_tags(&c[2]).trim())
    });
    let s = LIST_ITEM.replace_all(&s, "\n- ");
    let s = CELL.replace_all(&s, " | ");
    let s = BREAKS.replace_all(&s, "\n");
    let s = strip_tags(&s);
    let s = decode_entities(&s);
    let s = HORIZONTAL_SPACE.replace_all(&s, " ");
    let lines: Vec<&str> = s.lines().map(str::trim).collect();
    let s = lines.join("\n");
    let s = BLANK_RUNS.replace_all(&s, "\n\n");
    let body = s.trim();
    match page_title {
        Some(t) => format!("Title: {t}\n\n{body}"),
        None => body.to_string(),
    }
}

/// Decode the common named entities and all numeric ones.
pub fn decode_entities(s: &str) -> String {
    let s = NUMERIC_ENTITY.replace_all(s, |c: &regex::Captures| {
        let code = &c[1];
        let n = match code.strip_prefix('x') {
            Some(hex) => u32::from_str_radix(hex, 16).ok(),
            None => code.parse().ok(),
        };
        n.and_then(char::from_u32)
            .map(String::from)
            .unwrap_or_else(|| c[0].to_string())
    });
    // &amp; last so "&amp;lt;" decodes to "&lt;", not "<".
    s.replace("&nbsp;", " ")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&apos;", "'")
        .replace("&hellip;", "…")
        .replace("&mdash;", "—")
        .replace("&ndash;", "–")
        .replace("&amp;", "&")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_scripts_styles_and_comments() {
        let html = "<html><head><title>T</title><style>p{}</style></head><body>\
                    <script>alert(1)</script><!-- hidden --><p>Visible</p></body></html>";
        let text = to_text(html);
        assert_eq!(text, "Title: T\n\nVisible");
    }

    #[test]
    fn keeps_headings_lists_and_links() {
        let html = r##"<h1>Docs</h1><p>See <a href="https://x.dev/a">the guide</a> or
            <a href="#top">top</a>.</p><ul><li>one</li><li>two &amp; three</li></ul>
            <h3>Deep</h3>"##;
        let text = to_text(html);
        assert!(text.contains("# Docs"), "{text}");
        assert!(text.contains("[the guide](https://x.dev/a)"), "{text}");
        // In-page anchors keep their text only.
        assert!(text.contains("top."), "{text}");
        assert!(!text.contains("(#top)"));
        assert!(text.contains("- one\n- two & three"), "{text}");
        assert!(text.contains("### Deep"), "{text}");
    }

    #[test]
    fn tables_become_pipe_separated_rows() {
        let text =
            to_text("<table><tr><th>a</th><th>b</th></tr><tr><td>1</td><td>2</td></tr></table>");
        assert!(text.contains("a | b |"), "{text}");
        assert!(text.contains("1 | 2 |"), "{text}");
    }

    #[test]
    fn decodes_entities_once() {
        assert_eq!(
            decode_entities("a &lt; b &amp;&amp; c &#65;&#x42;"),
            "a < b && c AB"
        );
        assert_eq!(decode_entities("&amp;lt;"), "&lt;");
        assert_eq!(decode_entities("&#xFFFFFFFF;"), "&#xFFFFFFFF;");
    }

    #[test]
    fn collapses_whitespace_and_blank_runs() {
        let text = to_text("<div>a   b</div>\n\n\n\n<div>c</div>");
        assert_eq!(text, "a b\n\nc");
    }

    #[test]
    fn detects_html_documents() {
        assert!(looks_like_html("  <!DOCTYPE html><html>"));
        assert!(looks_like_html("<html lang=en>"));
        assert!(!looks_like_html("{\"json\": true}"));
        assert!(!looks_like_html("# markdown"));
    }

    #[test]
    fn extracts_the_title() {
        assert_eq!(
            title("<title> Hi &amp; bye </title>").as_deref(),
            Some("Hi & bye")
        );
        assert_eq!(title("<p>none</p>"), None);
    }
}
