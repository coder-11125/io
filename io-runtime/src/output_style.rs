//! Output styles: named blocks of instructions added to the system prompt that
//! change *how* the agent talks (terse, explanatory, teaching) without
//! changing what it can do.
//!
//! Besides the built-in styles, markdown files in `~/.io/output-styles/` and
//! `<project>/.io/output-styles/` define custom ones. A file may start with
//! `---` front matter holding `name:` and `description:`; the body is the
//! instruction text. The file name (without `.md`) is the name otherwise.
//! A project style overrides a user style, and either overrides a built-in.

use std::path::{Path, PathBuf};

pub const DEFAULT_STYLE: &str = "default";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutputStyle {
    pub name: String,
    pub description: String,
    /// Added to the system prompt; empty for the default style.
    pub prompt: String,
    /// Where it came from, `None` for built-ins.
    pub source: Option<PathBuf>,
}

fn builtin(name: &str, description: &str, prompt: &str) -> OutputStyle {
    OutputStyle {
        name: name.to_string(),
        description: description.to_string(),
        prompt: prompt.to_string(),
        source: None,
    }
}

pub fn builtin_styles() -> Vec<OutputStyle> {
    vec![
        builtin(DEFAULT_STYLE, "Efficient, task-focused answers", ""),
        builtin(
            "concise",
            "As few words as possible",
            "Be extremely concise. Answer in the fewest words that are still correct and \
             complete. No preamble, no recap of what you just did, no restating the question. \
             Prefer a single sentence or a short list; show code, not prose about code.",
        ),
        builtin(
            "explanatory",
            "Explains the why behind its choices",
            "While you work, briefly explain the implementation choices you make and the \
             codebase patterns you notice: why this approach rather than another, and how the \
             pieces fit together. Keep the explanations short and attached to the work — a few \
             sentences, not an essay — and never let them delay the task itself.",
        ),
        builtin(
            "learning",
            "Collaborative: leaves small pieces for you to write",
            "You are teaching while you work. Explain your reasoning as you go. When a change \
             contains a small, self-contained piece that is a good learning opportunity (a \
             function body, a test case, a decision about naming or structure), do everything \
             else yourself and leave a clearly marked `TODO(you)` with the exact signature and \
             guidance, then ask the user to fill it in before continuing. Do not hand over \
             anything critical to correctness.",
        ),
    ]
}

fn parse_file(path: &Path) -> Option<OutputStyle> {
    let text = std::fs::read_to_string(path).ok()?;
    let stem = path.file_stem()?.to_str()?.to_string();
    let (mut name, mut description, mut body) = (stem, String::new(), text.trim().to_string());
    if let Some(rest) = text.trim_start().strip_prefix("---") {
        if let Some((front, after)) = rest.split_once("\n---") {
            for line in front.lines() {
                if let Some((key, value)) = line.split_once(':') {
                    let value = value.trim().trim_matches(['"', '\'']).to_string();
                    match key.trim() {
                        "name" if !value.is_empty() => name = value,
                        "description" => description = value,
                        _ => {}
                    }
                }
            }
            body = after.trim().to_string();
        }
    }
    if body.is_empty() {
        return None;
    }
    if description.is_empty() {
        description = "Custom style".to_string();
    }
    Some(OutputStyle {
        name,
        description,
        prompt: body,
        source: Some(path.to_path_buf()),
    })
}

fn from_dir(dir: &Path) -> Vec<OutputStyle> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut paths: Vec<PathBuf> = entries
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().and_then(|e| e.to_str()) == Some("md"))
        .collect();
    paths.sort();
    paths.iter().filter_map(|p| parse_file(p)).collect()
}

/// All styles available to a project, built-ins first.
pub fn discover(root: &Path, home: Option<&Path>) -> Vec<OutputStyle> {
    let mut styles = builtin_styles();
    let mut layers = Vec::new();
    if let Some(home) = home {
        layers.push(from_dir(&home.join(".io").join("output-styles")));
    }
    layers.push(from_dir(&root.join(".io").join("output-styles")));
    for style in layers.into_iter().flatten() {
        match styles.iter_mut().find(|s| s.name == style.name) {
            Some(existing) => *existing = style,
            None => styles.push(style),
        }
    }
    styles
}

/// The system-prompt addition for `name` (empty for the default or an
/// unknown style).
pub fn prompt_for(name: Option<&str>, styles: &[OutputStyle]) -> String {
    let Some(style) = name.and_then(|n| styles.iter().find(|s| s.name == n)) else {
        return String::new();
    };
    if style.prompt.is_empty() {
        return String::new();
    }
    format!("\n\n## Output style: {}\n\n{}", style.name, style.prompt)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp() -> PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let dir = std::env::temp_dir().join(format!("io_styles_{}_{nanos}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn built_ins_are_present_and_default_adds_nothing() {
        let styles = builtin_styles();
        let names: Vec<_> = styles.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, ["default", "concise", "explanatory", "learning"]);
        assert_eq!(prompt_for(Some("default"), &styles), "");
        assert_eq!(prompt_for(None, &styles), "");
        assert_eq!(prompt_for(Some("nope"), &styles), "");
        assert!(prompt_for(Some("concise"), &styles).contains("## Output style: concise"));
    }

    #[test]
    fn custom_styles_come_from_markdown_files() {
        let dir = temp();
        let styles_dir = dir.join(".io/output-styles");
        std::fs::create_dir_all(&styles_dir).unwrap();
        std::fs::write(
            styles_dir.join("pirate.md"),
            "---\nname: pirate\ndescription: Talk like a pirate\n---\nArr. Speak like a pirate.\n",
        )
        .unwrap();
        std::fs::write(styles_dir.join("terse.md"), "Say little.").unwrap();
        std::fs::write(styles_dir.join("empty.md"), "---\nname: x\n---\n").unwrap();
        std::fs::write(styles_dir.join("notes.txt"), "ignored").unwrap();

        let styles = discover(&dir, None);
        let pirate = styles.iter().find(|s| s.name == "pirate").unwrap();
        assert_eq!(pirate.description, "Talk like a pirate");
        assert_eq!(pirate.prompt, "Arr. Speak like a pirate.");
        let terse = styles.iter().find(|s| s.name == "terse").unwrap();
        assert_eq!(terse.prompt, "Say little.");
        assert!(styles.iter().all(|s| s.name != "x" && s.name != "notes"));
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn project_styles_override_user_styles_which_override_built_ins() {
        let dir = temp();
        let home = dir.join("home");
        let root = dir.join("proj");
        for (base, text) in [(&home, "user version"), (&root, "project version")] {
            let d = base.join(".io/output-styles");
            std::fs::create_dir_all(&d).unwrap();
            std::fs::write(d.join("concise.md"), text).unwrap();
        }
        let styles = discover(&root, Some(&home));
        assert_eq!(
            styles.iter().filter(|s| s.name == "concise").count(),
            1,
            "no duplicates"
        );
        assert_eq!(
            styles.iter().find(|s| s.name == "concise").unwrap().prompt,
            "project version"
        );
        std::fs::remove_dir_all(dir).ok();
    }
}
