//! `io --worktree [name]`: start in a fresh git worktree on its own branch, so
//! an agent can work without touching the main checkout.

use std::path::{Path, PathBuf};
use std::process::Command;

/// Names become directory and branch names, so keep them boring.
pub fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 60
        && !name.starts_with(['.', '-'])
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
}

pub fn default_name() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    format!("io-{secs}")
}

pub fn branch_for(name: &str) -> String {
    format!("worktree-{name}")
}

fn git(repo: &Path, args: &[&str]) -> anyhow::Result<String> {
    let out = Command::new("git").current_dir(repo).args(args).output()?;
    if !out.status.success() {
        anyhow::bail!(
            "git {}: {}",
            args.join(" "),
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// Create (or reuse) the worktree `name` under `<repo>/.io/worktrees/` and
/// return its path.
pub fn create(repo: &Path, name: Option<&str>) -> anyhow::Result<PathBuf> {
    let name = name.map(str::to_string).unwrap_or_else(default_name);
    if !valid_name(&name) {
        anyhow::bail!("invalid worktree name '{name}' (letters, digits, '-', '_' and '.')");
    }
    if !repo.join(".git").exists() {
        anyhow::bail!("{} is not a git repository", repo.display());
    }
    let path = repo.join(".io").join("worktrees").join(&name);
    if path.exists() {
        return Ok(path);
    }
    git(
        repo,
        &[
            "worktree",
            "add",
            "-b",
            &branch_for(&name),
            &path.display().to_string(),
        ],
    )?;
    // Keep the worktrees out of `git status` in the main checkout.
    if let Ok(exclude) = git(repo, &["rev-parse", "--git-path", "info/exclude"]) {
        let exclude = repo.join(exclude);
        let current = std::fs::read_to_string(&exclude).unwrap_or_default();
        if !current.lines().any(|l| l.trim() == ".io/worktrees/") {
            if let Some(parent) = exclude.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            let sep = if current.is_empty() || current.ends_with('\n') {
                ""
            } else {
                "\n"
            };
            let _ = std::fs::write(&exclude, format!("{current}{sep}.io/worktrees/\n"));
        }
    }
    Ok(path)
}

/// The repository root for the current directory.
pub fn repo_root() -> anyhow::Result<PathBuf> {
    let cwd = std::env::current_dir()?;
    git(&cwd, &["rev-parse", "--show-toplevel"]).map(PathBuf::from)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_are_restricted() {
        for ok in ["fix-login", "a_b.c", "io-123", "X"] {
            assert!(valid_name(ok), "{ok}");
        }
        for bad in [
            "",
            "-x",
            ".hidden",
            "a/b",
            "a b",
            "a..b/../c",
            "é",
            &"x".repeat(61),
        ] {
            assert!(!valid_name(bad), "{bad}");
        }
        assert_eq!(branch_for("x"), "worktree-x");
        assert!(valid_name(&default_name()));
    }

    #[test]
    fn a_worktree_is_created_on_its_own_branch_then_reused() {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let repo = std::env::temp_dir().join(format!("io_wt_{}_{nanos}", std::process::id()));
        std::fs::create_dir_all(&repo).unwrap();
        let run = |args: &[&str]| {
            let out = Command::new("git")
                .current_dir(&repo)
                .args([
                    "-c",
                    "user.name=t",
                    "-c",
                    "user.email=t@t",
                    "-c",
                    "commit.gpgsign=false",
                ])
                .args(args)
                .output();
            matches!(out, Ok(o) if o.status.success())
        };
        if !(run(&["init", "-q"]) && run(&["commit", "-q", "--allow-empty", "-m", "init"])) {
            eprintln!("skipped: git unavailable");
            std::fs::remove_dir_all(&repo).ok();
            return;
        }
        let path = create(&repo, Some("try-it")).unwrap();
        assert_eq!(path, repo.join(".io/worktrees/try-it"));
        assert!(path.join(".git").exists());
        let branch = git(&path, &["rev-parse", "--abbrev-ref", "HEAD"]).unwrap();
        assert_eq!(branch, "worktree-try-it");
        let exclude = std::fs::read_to_string(repo.join(".git/info/exclude")).unwrap();
        assert!(exclude.contains(".io/worktrees/"));
        // Asking again reuses it instead of failing on the existing branch.
        assert_eq!(create(&repo, Some("try-it")).unwrap(), path);
        assert!(create(&repo, Some("../evil")).is_err());
        std::fs::remove_dir_all(&repo).ok();
    }

    #[test]
    fn a_directory_that_is_not_a_repo_is_refused() {
        let err = create(&std::env::temp_dir(), Some("x"))
            .unwrap_err()
            .to_string();
        assert!(err.contains("not a git repository"), "{err}");
    }
}
