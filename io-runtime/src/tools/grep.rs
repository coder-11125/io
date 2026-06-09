use super::{Tool, ToolInput, ToolOutput};

pub struct GrepTool;

#[async_trait::async_trait]
impl Tool for GrepTool {
    fn name(&self) -> &str { "grep" }

    fn description(&self) -> &str {
        "Search file contents using regular expressions. Returns file paths and line numbers of matches."
    }

    fn input_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "pattern": { "type": "string", "description": "Regex pattern to search for" },
                "include": { "type": "string", "description": "File glob pattern to filter (e.g. *.rs, *.{ts,tsx})", "default": "" },
                "path": { "type": "string", "description": "Root directory to search in", "default": "." }
            },
            "required": ["pattern"]
        })
    }

    async fn execute(&self, input: ToolInput) -> ToolOutput {
        let pattern = match input.args.get("pattern").and_then(|v| v.as_str()) {
            Some(p) => p.to_string(),
            None => return ToolOutput::err("missing required argument: pattern"),
        };

        let include = input.args.get("include").and_then(|v| v.as_str()).unwrap_or("").to_string();
        let root = input.args.get("path").and_then(|v| v.as_str()).unwrap_or(".");

        let re = match regex::Regex::new(&pattern) {
            Ok(r) => r,
            Err(e) => return ToolOutput::err(format!("invalid regex: {e}")),
        };

        let include_glob = if include.is_empty() {
            None
        } else {
            match globset::Glob::new(&include) {
                Ok(g) => Some(g.compile_matcher()),
                Err(e) => return ToolOutput::err(format!("invalid include pattern: {e}")),
            }
        };

        let walker = ignore::WalkBuilder::new(root).standard_filters(true).build();
        let mut results: Vec<String> = Vec::new();
        let mut file_count = 0u32;
        let mut match_count = 0u32;

        for entry in walker {
            let entry = match entry { Ok(e) => e, Err(_) => continue };
            let path = entry.path();
            if path.is_dir() { continue; }

            if let Some(ref glob) = include_glob {
                if !glob.is_match(path) { continue; }
            }

            let contents = match std::fs::read_to_string(path) { Ok(c) => c, Err(_) => continue };
            let path_str = path.to_string_lossy();
            let mut file_matches = 0;

            for (line_num, line) in contents.lines().enumerate() {
                if re.is_match(line) {
                    results.push(format!("{}:{}: {}", path_str, line_num + 1, line.trim()));
                    file_matches += 1;
                }
            }

            if file_matches > 0 {
                file_count += 1;
                match_count += file_matches;
            }
        }

        if results.is_empty() {
            ToolOutput::ok(format!("no matches for pattern: {pattern}"))
        } else {
            let mut output = results.join("\n");
            output.push_str(&format!("\n--- {match_count} matches across {file_count} files ---"));
            ToolOutput::ok(output)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn input(args: serde_json::Value) -> ToolInput {
        ToolInput {
            name: "grep".into(),
            args: args.as_object().unwrap().iter().map(|(k, v)| (k.clone(), v.clone())).collect(),
        }
    }

    fn make_temp_dir_with_file(filename: &str, content: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("io_grep_test_{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join(filename), content).unwrap();
        dir
    }

    #[tokio::test]
    async fn missing_pattern_returns_error() {
        let out = GrepTool.execute(input(serde_json::json!({}))).await;
        assert!(!out.success);
        assert!(out.data.contains("missing required argument: pattern"));
    }

    #[tokio::test]
    async fn invalid_regex_returns_error() {
        let out = GrepTool.execute(input(serde_json::json!({ "pattern": "[invalid" }))).await;
        assert!(!out.success);
        assert!(out.data.contains("invalid regex"));
    }

    #[tokio::test]
    async fn finds_matches_with_line_numbers() {
        let dir = make_temp_dir_with_file("sample.txt", "alpha\nbeta\nalpha again\n");
        let out = GrepTool.execute(input(serde_json::json!({
            "pattern": "alpha",
            "path": dir.to_str().unwrap()
        }))).await;
        std::fs::remove_dir_all(&dir).ok();
        assert!(out.success, "{}", out.data);
        assert!(out.data.contains("alpha"));
        assert!(out.data.contains(":1:") || out.data.contains(":3:"));
        assert!(out.data.contains("2 matches"));
    }

    #[tokio::test]
    async fn no_matches_returns_message() {
        let dir = make_temp_dir_with_file("empty.txt", "nothing relevant\n");
        let out = GrepTool.execute(input(serde_json::json!({
            "pattern": "ZZZNOMATCH",
            "path": dir.to_str().unwrap()
        }))).await;
        std::fs::remove_dir_all(&dir).ok();
        assert!(out.success);
        assert!(out.data.contains("no matches"));
    }
}
