//! Memory files: the `AGENTS.md` / `CLAUDE.md` instructions the model starts
//! every conversation with.
//!
//! Loaded, lowest precedence first (later text wins when instructions clash):
//! 1. the user's own `~/.io/CLAUDE.md` and `~/.io/AGENTS.md`;
//! 2. the project root's `AGENTS.md`, `CLAUDE.md` and `.io/CLAUDE.md`;
//! 3. the same files in each directory between the root and the working
//!    directory (when io is started from a subdirectory);
//! 4. `CLAUDE.local.md` at the root — personal notes, kept out of version control.
//!
//! A line may pull in another file with `@path/to/file` (relative to the
//! importing file, or `~/…`/absolute). Imports nest up to five deep, skip code
//! blocks and cycles, and are confined: a file inside the project can only
//! import from the project, and the user's own files only from their home
//! directory — a repository's memory file must not be able to feed
//! `~/.ssh/id_rsa` to a model. Memory files in directories *below* the
//! working directory are not loaded up front; they are surfaced when the
//! model first touches a file there ([`nested_for`]).

use std::collections::HashSet;
use std::path::{Path, PathBuf};

const NAMES: &[&str] = &["AGENTS.md", "CLAUDE.md"];
const MAX_IMPORT_DEPTH: usize = 5;
const MAX_IMPORTED_FILES: usize = 20;
const MAX_IMPORT_BYTES: u64 = 100 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MemoryScope {
    User,
    Project,
    Local,
    Nested,
}

#[derive(Debug, Clone)]
pub struct MemoryFile {
    pub path: PathBuf,
    pub label: String,
    pub scope: MemoryScope,
    /// The file that imported this one, if it was imported.
    pub imported_by: Option<String>,
    pub content: String,
}

fn label_for(path: &Path, root: &Path, home: Option<&Path>) -> String {
    if let Ok(rel) = path.strip_prefix(root) {
        return rel.display().to_string();
    }
    if let Some(rel) = home.and_then(|h| path.strip_prefix(h).ok()) {
        return format!("~/{}", rel.display());
    }
    path.display().to_string()
}

fn read_trimmed(path: &Path) -> Option<String> {
    let text = std::fs::read_to_string(path).ok()?;
    let trimmed = text.trim();
    (!trimmed.is_empty()).then(|| trimmed.to_string())
}

/// Every memory file for a session started in `cwd` inside project `root`.
pub fn discover(root: &Path, cwd: &Path, home: Option<&Path>) -> Vec<MemoryFile> {
    let mut out = Vec::new();
    let mut seen: HashSet<PathBuf> = HashSet::new();
    let mut add = |path: PathBuf, scope: MemoryScope, allowed: &Path, out: &mut Vec<MemoryFile>| {
        let Some(content) = read_trimmed(&path) else {
            return;
        };
        let canonical = std::fs::canonicalize(&path).unwrap_or_else(|_| path.clone());
        if !seen.insert(canonical.clone()) {
            return;
        }
        let label = label_for(&path, root, home);
        out.push(MemoryFile {
            path: path.clone(),
            label: label.clone(),
            scope,
            imported_by: None,
            content: content.clone(),
        });
        let mut ctx = ImportCtx {
            root,
            home,
            allowed,
            scope,
            seen: &mut seen,
            out,
            budget: MAX_IMPORTED_FILES,
        };
        expand_imports(&path, &content, &label, 1, &mut ctx);
    };

    if let Some(home) = home {
        let dir = home.join(".io");
        for name in NAMES {
            add(dir.join(name), MemoryScope::User, home, &mut out);
        }
    }
    for name in NAMES {
        add(root.join(name), MemoryScope::Project, root, &mut out);
    }
    add(
        root.join(".io").join("CLAUDE.md"),
        MemoryScope::Project,
        root,
        &mut out,
    );

    // Directories between the root and where io was started, outermost first.
    if let Ok(rel) = cwd.strip_prefix(root) {
        let mut dir = root.to_path_buf();
        for part in rel.components() {
            dir.push(part);
            for name in NAMES {
                add(dir.join(name), MemoryScope::Project, root, &mut out);
            }
        }
    }
    add(
        root.join("CLAUDE.local.md"),
        MemoryScope::Local,
        root,
        &mut out,
    );
    out
}

/// The `<project-context>` block for `files`, or `None` if there are none.
pub fn render(files: &[MemoryFile]) -> Option<String> {
    if files.is_empty() {
        return None;
    }
    let sections: Vec<String> = files
        .iter()
        .map(|f| match &f.imported_by {
            Some(by) => format!("### {} (imported by {by})\n\n{}", f.label, f.content),
            None => format!("### {}\n\n{}", f.label, f.content),
        })
        .collect();
    Some(format!(
        "<project-context>\n{}\n</project-context>",
        sections.join("\n\n")
    ))
}

/// Discover and render in one step: what `build_agent` hands the agent.
pub fn load(root: &Path, cwd: &Path) -> (Option<String>, Vec<MemoryFile>) {
    let home = dirs::home_dir();
    let files = discover(root, cwd, home.as_deref());
    (render(&files), files)
}

// ── Imports ────────────────────────────────────────────────────────────────────

struct ImportCtx<'a> {
    root: &'a Path,
    home: Option<&'a Path>,
    /// Imports must resolve inside this directory.
    allowed: &'a Path,
    scope: MemoryScope,
    seen: &'a mut HashSet<PathBuf>,
    out: &'a mut Vec<MemoryFile>,
    budget: usize,
}

/// `@path` tokens in `content`, skipping fenced code blocks and `inline code`.
fn import_candidates(content: &str) -> Vec<String> {
    let mut found = Vec::new();
    let mut in_fence = false;
    for line in content.lines() {
        if line.trim_start().starts_with("```") {
            in_fence = !in_fence;
            continue;
        }
        if in_fence {
            continue;
        }
        // Blank out `inline code` so examples aren't imported.
        let mut visible = String::new();
        let mut in_code = false;
        for c in line.chars() {
            if c == '`' {
                in_code = !in_code;
            } else if !in_code {
                visible.push(c);
            }
        }
        for token in visible.split_whitespace() {
            let Some(path) = token.strip_prefix('@') else {
                continue;
            };
            let path = path.trim_end_matches(['.', ',', ';', ':', ')', '!', '?']);
            let looks_like_path = path.starts_with("~/")
                || path.starts_with('/')
                || path.starts_with("./")
                || path.starts_with("../")
                || path.contains('/');
            if looks_like_path && path.len() > 1 && !found.contains(&path.to_string()) {
                found.push(path.to_string());
            }
        }
    }
    found
}

fn expand_imports(from: &Path, content: &str, from_label: &str, depth: usize, ctx: &mut ImportCtx) {
    if depth > MAX_IMPORT_DEPTH {
        return;
    }
    let base = from.parent().unwrap_or_else(|| Path::new("."));
    let allowed = std::fs::canonicalize(ctx.allowed).unwrap_or_else(|_| ctx.allowed.to_path_buf());
    for candidate in import_candidates(content) {
        if ctx.budget == 0 {
            return;
        }
        let target = match candidate.strip_prefix("~/") {
            Some(rest) => match ctx.home {
                Some(h) => h.join(rest),
                None => continue,
            },
            None => base.join(&candidate),
        };
        let Ok(real) = std::fs::canonicalize(&target) else {
            continue;
        };
        if !real.starts_with(&allowed) || is_secret_path(&real) {
            continue;
        }
        let Ok(meta) = std::fs::metadata(&real) else {
            continue;
        };
        if !meta.is_file() || meta.len() > MAX_IMPORT_BYTES {
            continue;
        }
        if !ctx.seen.insert(real.clone()) {
            continue;
        }
        let Some(text) = read_trimmed(&real) else {
            continue;
        };
        ctx.budget -= 1;
        let label = label_for(&real, ctx.root, ctx.home);
        ctx.out.push(MemoryFile {
            path: real.clone(),
            label: label.clone(),
            scope: ctx.scope,
            imported_by: Some(from_label.to_string()),
            content: text.clone(),
        });
        expand_imports(&real, &text, &label, depth + 1, ctx);
    }
}

fn is_secret_path(path: &Path) -> bool {
    let norm = path.display().to_string().replace('\\', "/");
    [
        "/.ssh/",
        "/.gnupg/",
        "/.aws/",
        "/.netrc",
        "/.env",
        "/id_rsa",
        "/id_ed25519",
        "/.io/keys.toml",
        "/.io/oauth.toml",
    ]
    .iter()
    .any(|s| norm.contains(s))
}

// ── Nested files, loaded when first needed ────────────────────────────────────

/// Memory files for the directories between `root` (exclusive) and `file`'s
/// own directory that have not been loaded yet. `loaded` is updated so each is
/// returned once. Used when the model reads a file deep in the tree.
pub fn nested_for(root: &Path, file: &Path, loaded: &mut HashSet<PathBuf>) -> Vec<MemoryFile> {
    let file = if file.is_absolute() {
        file.to_path_buf()
    } else {
        root.join(file)
    };
    let Ok(rel) = file.strip_prefix(root) else {
        return Vec::new();
    };
    let Some(dir_rel) = rel.parent() else {
        return Vec::new();
    };
    let home = dirs::home_dir();
    let mut out = Vec::new();
    let mut dir = root.to_path_buf();
    for part in dir_rel.components() {
        let std::path::Component::Normal(part) = part else {
            return Vec::new();
        };
        dir.push(part);
        for name in NAMES {
            let path = dir.join(name);
            let canonical = std::fs::canonicalize(&path).unwrap_or_else(|_| path.clone());
            if loaded.contains(&canonical) {
                continue;
            }
            let Some(content) = read_trimmed(&path) else {
                continue;
            };
            loaded.insert(canonical);
            out.push(MemoryFile {
                label: label_for(&path, root, home.as_deref()),
                path,
                scope: MemoryScope::Nested,
                imported_by: None,
                content,
            });
        }
    }
    out
}

/// The canonical paths of `files`, for seeding [`nested_for`]'s `loaded` set.
pub fn loaded_set(files: &[MemoryFile]) -> HashSet<PathBuf> {
    files
        .iter()
        .map(|f| std::fs::canonicalize(&f.path).unwrap_or_else(|_| f.path.clone()))
        .collect()
}

/// How a nested memory file is shown to the model after a tool result.
pub fn render_nested(files: &[MemoryFile]) -> String {
    files
        .iter()
        .map(|f| {
            format!(
                "\n\n<nested-instructions file=\"{}\">\n{}\n</nested-instructions>",
                f.label, f.content
            )
        })
        .collect()
}

// ── Writing ────────────────────────────────────────────────────────────────────

/// Where `#` quick-add and `/memory` can write.
pub fn targets(root: &Path, home: Option<&Path>) -> Vec<(String, PathBuf)> {
    let mut v = vec![
        (
            "Project memory (CLAUDE.md)".to_string(),
            root.join("CLAUDE.md"),
        ),
        (
            "Project memory, personal (CLAUDE.local.md)".to_string(),
            root.join("CLAUDE.local.md"),
        ),
    ];
    if let Some(home) = home {
        v.push((
            "User memory (~/.io/CLAUDE.md)".to_string(),
            home.join(".io").join("CLAUDE.md"),
        ));
    }
    v
}

/// Append `- note` to the memory file at `path`, creating it if needed.
pub fn append_note(path: &Path, note: &str) -> std::io::Result<()> {
    use std::io::Write;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let existing = std::fs::read_to_string(path).unwrap_or_default();
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)?;
    if !existing.is_empty() && !existing.ends_with('\n') {
        writeln!(file)?;
    }
    writeln!(file, "- {}", note.trim())
}

/// The prompt behind `/init`.
pub const INIT_PROMPT: &str = "Please analyze this codebase and create a CLAUDE.md file at the project root \
that future AI coding sessions will load automatically. If one already exists, improve it instead of \
replacing it. Include: the common commands (build, lint, run, test — and how to run a single test); a \
high-level picture of the architecture that needs reading several files to understand; and the \
conventions that are not obvious from the code. Do not list every file or restate what any developer \
would assume, do not invent sections, and do not make up information — only include what you verified \
by reading the code. Prefix the file with a one-line description of what it is. If there are README.md \
rules, Cursor rules (.cursorrules, .cursor/rules/) or Copilot instructions (.github/copilot-instructions.md), \
fold in the important parts.";

#[cfg(test)]
mod tests {
    use super::*;

    struct Tree(PathBuf);

    impl Tree {
        fn new() -> Self {
            let nanos = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0);
            let dir =
                std::env::temp_dir().join(format!("io_memory_{}_{nanos}", std::process::id()));
            std::fs::create_dir_all(dir.join("home/.io")).unwrap();
            std::fs::create_dir_all(dir.join("proj")).unwrap();
            Tree(std::fs::canonicalize(dir).unwrap())
        }

        fn write(&self, rel: &str, text: &str) {
            let p = self.0.join(rel);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, text).unwrap();
        }

        fn root(&self) -> PathBuf {
            self.0.join("proj")
        }

        fn home(&self) -> PathBuf {
            self.0.join("home")
        }

        fn labels(&self, cwd: &str) -> Vec<String> {
            let cwd = self.0.join("proj").join(cwd);
            discover(&self.root(), &cwd, Some(&self.home()))
                .into_iter()
                .map(|f| f.label)
                .collect()
        }
    }

    impl Drop for Tree {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn nothing_found_means_no_context() {
        let t = Tree::new();
        assert!(t.labels("").is_empty());
        assert!(render(&[]).is_none());
    }

    #[test]
    fn files_load_user_then_project_then_local_in_that_order() {
        let t = Tree::new();
        t.write("home/.io/CLAUDE.md", "user rules");
        t.write("proj/AGENTS.md", "agents");
        t.write("proj/CLAUDE.md", "claude");
        t.write("proj/.io/CLAUDE.md", "dot io");
        t.write("proj/CLAUDE.local.md", "mine");
        assert_eq!(
            t.labels(""),
            [
                "~/.io/CLAUDE.md",
                "AGENTS.md",
                "CLAUDE.md",
                ".io/CLAUDE.md",
                "CLAUDE.local.md"
            ]
        );
    }

    #[test]
    fn empty_files_are_skipped() {
        let t = Tree::new();
        t.write("proj/CLAUDE.md", "  \n\n ");
        assert!(t.labels("").is_empty());
    }

    #[test]
    fn directories_between_root_and_cwd_are_loaded_outermost_first() {
        let t = Tree::new();
        t.write("proj/CLAUDE.md", "root");
        t.write("proj/a/CLAUDE.md", "a");
        t.write("proj/a/b/AGENTS.md", "b");
        t.write("proj/a/b/c/CLAUDE.md", "below cwd");
        assert_eq!(
            t.labels("a/b"),
            ["CLAUDE.md", "a/CLAUDE.md", "a/b/AGENTS.md"]
        );
    }

    #[test]
    fn rendering_keeps_the_established_block_format() {
        let t = Tree::new();
        t.write("proj/CLAUDE.md", "be nice\n");
        let (ctx, files) = load_for(&t, "");
        assert_eq!(files.len(), 1);
        assert_eq!(
            ctx.unwrap(),
            "<project-context>\n### CLAUDE.md\n\nbe nice\n</project-context>"
        );
    }

    fn load_for(t: &Tree, cwd: &str) -> (Option<String>, Vec<MemoryFile>) {
        let files = discover(&t.root(), &t.root().join(cwd), Some(&t.home()));
        (render(&files), files)
    }

    #[test]
    fn imports_are_inlined_after_their_importer() {
        let t = Tree::new();
        t.write(
            "proj/CLAUDE.md",
            "See @docs/style.md for style.\nemail me@example.com",
        );
        t.write("proj/docs/style.md", "use tabs");
        let (ctx, files) = load_for(&t, "");
        assert_eq!(files.len(), 2);
        assert_eq!(files[1].label, "docs/style.md");
        assert_eq!(files[1].imported_by.as_deref(), Some("CLAUDE.md"));
        assert!(ctx
            .unwrap()
            .contains("### docs/style.md (imported by CLAUDE.md)\n\nuse tabs"));
    }

    #[test]
    fn imports_nest_skip_cycles_and_code() {
        let t = Tree::new();
        t.write(
            "proj/CLAUDE.md",
            "@a/one.md\n```\n@a/code.md\n```\nInline `@a/inline.md` here",
        );
        t.write("proj/a/one.md", "one @./two.md");
        t.write("proj/a/two.md", "two @../CLAUDE.md @./one.md");
        t.write("proj/a/code.md", "code");
        t.write("proj/a/inline.md", "inline");
        let labels: Vec<String> = load_for(&t, "").1.into_iter().map(|f| f.label).collect();
        assert_eq!(labels, ["CLAUDE.md", "a/one.md", "a/two.md"]);
    }

    #[test]
    fn imports_stop_five_levels_deep() {
        let t = Tree::new();
        t.write("proj/CLAUDE.md", "@./l1.md");
        for i in 1..=8 {
            t.write(
                &format!("proj/l{i}.md"),
                &format!("level {i} @./l{}.md", i + 1),
            );
        }
        let n = load_for(&t, "").1.len();
        assert_eq!(n, 1 + MAX_IMPORT_DEPTH);
    }

    #[test]
    fn project_files_cannot_import_from_outside_the_project() {
        let t = Tree::new();
        t.write("home/.ssh/id_rsa", "SECRET");
        t.write("home/notes.md", "home notes");
        t.write("outside.md", "outside");
        t.write(
            "proj/CLAUDE.md",
            "@~/.ssh/id_rsa @~/notes.md @../outside.md @/etc/hostname",
        );
        let labels: Vec<String> = load_for(&t, "").1.into_iter().map(|f| f.label).collect();
        assert_eq!(labels, ["CLAUDE.md"]);
    }

    #[test]
    fn user_files_may_import_from_home_but_never_secrets() {
        let t = Tree::new();
        t.write("home/.ssh/id_rsa", "SECRET");
        t.write("home/notes.md", "home notes");
        t.write("home/.io/CLAUDE.md", "@~/notes.md @~/.ssh/id_rsa");
        let labels: Vec<String> = load_for(&t, "").1.into_iter().map(|f| f.label).collect();
        assert_eq!(labels, ["~/.io/CLAUDE.md", "~/notes.md"]);
    }

    #[test]
    fn nested_files_surface_once_when_a_file_below_them_is_touched() {
        let t = Tree::new();
        t.write("proj/CLAUDE.md", "root");
        t.write("proj/src/CLAUDE.md", "src rules");
        t.write("proj/src/deep/CLAUDE.md", "deep rules");
        let (_, files) = load_for(&t, "");
        let mut loaded = loaded_set(&files);
        let hit = nested_for(&t.root(), Path::new("src/deep/x.rs"), &mut loaded);
        let labels: Vec<_> = hit.iter().map(|f| f.label.as_str()).collect();
        assert_eq!(labels, ["src/CLAUDE.md", "src/deep/CLAUDE.md"]);
        assert!(nested_for(&t.root(), Path::new("src/deep/y.rs"), &mut loaded).is_empty());
        assert!(nested_for(&t.root(), Path::new("top.rs"), &mut loaded).is_empty());
        assert!(render_nested(&hit).contains("<nested-instructions file=\"src/CLAUDE.md\">"));
    }

    #[test]
    fn files_outside_the_root_surface_nothing() {
        let t = Tree::new();
        let mut loaded = HashSet::new();
        assert!(nested_for(&t.root(), Path::new("/etc/passwd"), &mut loaded).is_empty());
        assert!(nested_for(&t.root(), Path::new("../x/CLAUDE.md"), &mut loaded).is_empty());
    }

    #[test]
    fn notes_are_appended_as_bullets() {
        let t = Tree::new();
        let path = t.root().join("CLAUDE.md");
        append_note(&path, "use tabs").unwrap();
        append_note(&path, " run clippy ").unwrap();
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "- use tabs\n- run clippy\n"
        );
        std::fs::write(&path, "no newline").unwrap();
        append_note(&path, "x").unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "no newline\n- x\n");
    }
}
