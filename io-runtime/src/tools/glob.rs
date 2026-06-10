use super::{Tool, ToolInput, ToolOutput};

const MAX_GLOB_RESULTS: usize = 500;

pub struct GlobTool;

#[async_trait::async_trait]
impl Tool for GlobTool {
    fn name(&self) -> &str {
        "glob"
    }

    fn description(&self) -> &str {
        "Find files matching a glob pattern. Returns file paths sorted by modification time, newest first."
    }

    fn input_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "pattern": { "type": "string", "description": "Glob pattern (e.g. **/*.rs, src/**/*.ts)" },
                "path": { "type": "string", "description": "Root directory to search in", "default": "." }
            },
            "required": ["pattern"]
        })
    }

    async fn execute(&self, input: ToolInput) -> ToolOutput {
        let pattern = match input.args.get("pattern").and_then(|v| v.as_str()) {
            Some(p) => p,
            None => return ToolOutput::err("missing required argument: pattern"),
        };

        let root = input
            .args
            .get("path")
            .and_then(|v| v.as_str())
            .unwrap_or(".");

        let glob = match globset::Glob::new(pattern) {
            Ok(g) => g.compile_matcher(),
            Err(e) => return ToolOutput::err(format!("invalid glob pattern: {e}")),
        };

        let walker = ignore::WalkBuilder::new(root)
            .standard_filters(true)
            .build();
        let mut matches: Vec<(String, Option<std::time::SystemTime>)> = Vec::new();

        for entry in walker {
            match entry {
                Ok(entry) => {
                    let path = entry.path();
                    if path.is_dir() {
                        continue;
                    }
                    if glob.is_match(path) {
                        let modified = std::fs::metadata(path).ok().and_then(|m| m.modified().ok());
                        matches.push((path.to_string_lossy().to_string(), modified));
                    }
                }
                Err(_) => continue,
            }
        }

        matches.sort_by_key(|m| std::cmp::Reverse(m.1));

        let total = matches.len();
        let truncated = total > MAX_GLOB_RESULTS;
        let shown: Vec<String> = matches
            .iter()
            .take(MAX_GLOB_RESULTS)
            .map(|m| m.0.clone())
            .collect();

        let output = if shown.is_empty() {
            format!("no files matching pattern: {pattern}")
        } else if truncated {
            format!(
                "{}\n[showing {} of {} matches — narrow the pattern to see more]",
                shown.join("\n"),
                MAX_GLOB_RESULTS,
                total
            )
        } else {
            shown.join("\n")
        };

        ToolOutput::ok(output)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn input(args: serde_json::Value) -> ToolInput {
        ToolInput {
            name: "glob".into(),
            args: args
                .as_object()
                .unwrap()
                .iter()
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect(),
        }
    }

    fn make_temp_dir() -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("io_glob_test_{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[tokio::test]
    async fn missing_pattern_returns_error() {
        let out = GlobTool.execute(input(serde_json::json!({}))).await;
        assert!(!out.success);
        assert!(out.data.contains("missing required argument: pattern"));
    }

    #[tokio::test]
    async fn finds_matching_files() {
        let dir = make_temp_dir();
        std::fs::write(dir.join("a.txt"), "a").unwrap();
        std::fs::write(dir.join("b.txt"), "b").unwrap();
        std::fs::write(dir.join("c.rs"), "c").unwrap();
        let out = GlobTool
            .execute(input(serde_json::json!({
                "pattern": "*.txt",
                "path": dir.to_str().unwrap()
            })))
            .await;
        std::fs::remove_dir_all(&dir).ok();
        assert!(out.success, "{}", out.data);
        assert!(out.data.contains("a.txt"));
        assert!(out.data.contains("b.txt"));
        assert!(!out.data.contains("c.rs"));
    }

    #[tokio::test]
    async fn no_matches_returns_message() {
        let dir = make_temp_dir();
        let out = GlobTool
            .execute(input(serde_json::json!({
                "pattern": "*.xyz",
                "path": dir.to_str().unwrap()
            })))
            .await;
        std::fs::remove_dir_all(&dir).ok();
        assert!(out.success);
        assert!(out.data.contains("no files matching"));
    }

    #[tokio::test]
    async fn invalid_pattern_returns_error() {
        let out = GlobTool
            .execute(input(serde_json::json!({
                "pattern": "[invalid"
            })))
            .await;
        assert!(!out.success);
        assert!(out.data.contains("invalid glob pattern"));
    }
}
