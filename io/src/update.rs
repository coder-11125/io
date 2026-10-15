//! `io update`: check GitHub for a newer release and say how to install it.

const LATEST_URL: &str = "https://api.github.com/repos/coder-11125/io/releases/latest";
pub const INSTALL_ARGS: &[&str] = &[
    "install",
    "--locked",
    "--force",
    "--git",
    "https://github.com/coder-11125/io",
    "io",
];

/// Numeric `major.minor.patch` of a version like `v1.2.3` or `1.2.3-rc1`.
fn parse(version: &str) -> Option<(u64, u64, u64)> {
    let core = version.trim().trim_start_matches('v');
    let core = core.split(['-', '+']).next()?;
    let mut parts = core.split('.').map(|p| p.parse::<u64>().ok());
    Some((
        parts.next()??,
        parts.next()??,
        parts.next().unwrap_or(Some(0))?,
    ))
}

pub fn is_newer(current: &str, latest: &str) -> bool {
    match (parse(current), parse(latest)) {
        (Some(c), Some(l)) => l > c,
        _ => false,
    }
}

pub async fn run(install: bool) -> anyhow::Result<()> {
    let current = env!("CARGO_PKG_VERSION");
    println!("io {current}");
    let client = reqwest::Client::builder()
        .user_agent(format!("io/{current}"))
        .timeout(std::time::Duration::from_secs(15))
        .build()?;
    let response = client.get(LATEST_URL).send().await?;
    if response.status() == reqwest::StatusCode::NOT_FOUND {
        println!("No releases are published yet.");
        println!("To update from source: git pull && cargo install --path io --locked");
        return Ok(());
    }
    let release: serde_json::Value = response.error_for_status()?.json().await?;
    let tag = release["tag_name"].as_str().unwrap_or("");
    if !is_newer(current, tag) {
        println!(
            "You are up to date (latest release: {}).",
            if tag.is_empty() { "unknown" } else { tag }
        );
        return Ok(());
    }
    println!("A newer release is available: {tag}");
    if let Some(url) = release["html_url"].as_str() {
        println!("Release notes: {url}");
    }
    let command = format!("cargo {}", INSTALL_ARGS.join(" "));
    if !install {
        println!("Install it with: {command}");
        println!("(or run `io update --install` to do that now)");
        return Ok(());
    }
    println!("Running: {command}");
    let status = std::process::Command::new("cargo")
        .args(INSTALL_ARGS)
        .status()?;
    if !status.success() {
        anyhow::bail!("cargo install failed ({status})");
    }
    println!("Updated. Restart io to use {tag}.");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versions_compare_numerically() {
        assert!(is_newer("0.1.0", "v0.2.0"));
        assert!(is_newer("0.9.0", "0.10.0"));
        assert!(is_newer("1.2.3", "1.2.4"));
        assert!(!is_newer("1.2.3", "1.2.3"));
        assert!(!is_newer("1.3.0", "v1.2.9"));
        assert!(is_newer("1.0.0", "2.0"));
        assert!(!is_newer("1.0.0", "nonsense"));
        assert!(!is_newer("1.0.0", ""));
        assert!(is_newer("1.0.0-rc1", "1.0.1"));
    }
}
