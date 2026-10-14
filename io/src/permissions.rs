//! Permission rules and extra directories as the CLI manages them: applying
//! `settings.json` to a [`PermissionChecker`], `/permissions`, `--add-dir` and
//! `/add-dir`.

use io_runtime::rules::RuleSet;
use io_runtime::sandbox::PermissionChecker;
use io_runtime::settings::{self, RuleList, Settings, SettingsPaths, SettingsScope};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

/// Directories added for this run with `--add-dir` or `/add-dir`, on top of
/// the ones in settings. They are not persisted.
static EXTRA_DIRS: Mutex<Vec<PathBuf>> = Mutex::new(Vec::new());

/// Add `dir` (must exist) to the directories file tools may reach. Returns the
/// canonical path.
pub fn add_dir(dir: &Path) -> anyhow::Result<PathBuf> {
    let canonical =
        std::fs::canonicalize(dir).map_err(|e| anyhow::anyhow!("{}: {e}", dir.display()))?;
    if !canonical.is_dir() {
        anyhow::bail!("{} is not a directory", canonical.display());
    }
    let mut dirs = EXTRA_DIRS.lock().unwrap();
    if !dirs.contains(&canonical) {
        dirs.push(canonical.clone());
    }
    Ok(canonical)
}

fn runtime_dirs() -> Vec<PathBuf> {
    EXTRA_DIRS.lock().unwrap().clone()
}

/// Layer `settings` (and any `--add-dir`s) onto `checker`.
pub fn apply(checker: PermissionChecker, settings: &Settings, root: &Path) -> PermissionChecker {
    let (rules, _) = RuleSet::from_strings(
        &settings.rules_in(RuleList::Allow),
        &settings.rules_in(RuleList::Ask),
        &settings.rules_in(RuleList::Deny),
        Some(root.to_path_buf()),
    );
    let mut dirs = settings.additional_directories.clone();
    dirs.extend(runtime_dirs());
    checker.with_rules(rules).with_extra_roots(dirs)
}

// ── /permissions ───────────────────────────────────────────────────────────────

const USAGE: &str =
    "usage: /permissions [allow|ask|deny <rule> | remove <rule>]   e.g. /permissions allow Bash(npm test:*)";

/// Run a `/permissions` argument and return the lines to show. Changes are
/// written to `settings.local.json`, so they apply to this project only and
/// stay out of version control.
pub fn command(arg: &str, paths: &SettingsPaths, root: &Path) -> anyhow::Result<Vec<String>> {
    let arg = arg.trim();
    let mut lines = Vec::new();
    if !arg.is_empty() {
        let (verb, rule) = arg.split_once(char::is_whitespace).unwrap_or((arg, ""));
        let rule = rule.trim();
        match (verb, RuleList::parse(verb)) {
            (_, Some(list)) if !rule.is_empty() => {
                let added =
                    settings::add_rule(&paths.local, list, rule).map_err(anyhow::Error::msg)?;
                lines.push(if added {
                    format!(
                        "Added {} rule {rule} to {}",
                        list.key(),
                        paths.local.display()
                    )
                } else {
                    format!("{rule} is already a {} rule", list.key())
                });
            }
            ("remove", _) if !rule.is_empty() => {
                let mut removed = false;
                for scope in [SettingsScope::Local, SettingsScope::User] {
                    removed |= settings::remove_rule(paths.path(scope), rule)
                        .map_err(anyhow::Error::msg)?;
                }
                lines.push(if removed {
                    format!("Removed {rule}")
                } else {
                    format!(
                        "No local or user rule {rule} (project and managed rules are edited in their files)"
                    )
                });
            }
            _ => return Ok(vec![USAGE.to_string()]),
        }
    }

    let current = Settings::load_from(paths, root);
    lines.push("Permission rules (deny > ask > allow):".to_string());
    if current.rules.is_empty() {
        lines.push("  none — add one with /permissions allow Bash(npm test:*)".to_string());
    }
    for list in [RuleList::Deny, RuleList::Ask, RuleList::Allow] {
        for entry in current.rules.iter().filter(|r| r.list == list) {
            lines.push(format!(
                "  {:<5} {}  [{}]",
                list.key(),
                entry.rule,
                entry.scope.label()
            ));
        }
    }
    let mut dirs: Vec<String> = current
        .additional_directories
        .iter()
        .map(|d| d.display().to_string())
        .collect();
    dirs.extend(
        runtime_dirs()
            .iter()
            .map(|d| format!("{} (this run)", d.display())),
    );
    if !dirs.is_empty() {
        lines.push(format!("Extra directories: {}", dirs.join(", ")));
    }
    if let Some(mode) = current.default_mode {
        lines.push(format!("Default mode: {}", mode.label()));
    }
    for problem in &current.problems {
        lines.push(format!("warning: {problem}"));
    }
    Ok(lines)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn unique() -> String {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        format!("{}_{nanos}", std::process::id())
    }

    fn fixture() -> (PathBuf, SettingsPaths) {
        let dir = std::env::temp_dir().join(format!("io_perm_{}", unique()));
        let paths = SettingsPaths {
            user: dir.join("home/settings.json"),
            project: dir.join("proj/.io/settings.json"),
            local: dir.join("proj/.io/settings.local.json"),
            managed: dir.join("managed.json"),
        };
        (dir, paths)
    }

    #[test]
    fn rules_can_be_added_listed_and_removed() {
        let (dir, paths) = fixture();
        let root = dir.join("proj");
        let out = command("allow Bash(npm test:*)", &paths, &root).unwrap();
        assert!(out[0].starts_with("Added allow rule"));
        assert!(out
            .iter()
            .any(|l| l.contains("Bash(npm test:*)") && l.contains("[local]")));
        command("deny Read(./.env)", &paths, &root).unwrap();
        let out = command("", &paths, &root).unwrap();
        let deny_at = out.iter().position(|l| l.contains("Read(./.env)")).unwrap();
        let allow_at = out.iter().position(|l| l.contains("npm test")).unwrap();
        assert!(deny_at < allow_at, "deny rules are listed first");
        let out = command("remove Bash(npm test:*)", &paths, &root).unwrap();
        assert_eq!(out[0], "Removed Bash(npm test:*)");
        assert!(!out[1..].iter().any(|l| l.contains("npm test")));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn bad_input_gets_usage_or_an_error() {
        let (dir, paths) = fixture();
        let root = dir.join("proj");
        assert!(command("bogus", &paths, &root).unwrap()[0].starts_with("usage"));
        assert!(command("allow", &paths, &root).unwrap()[0].starts_with("usage"));
        assert!(command("allow Bash(oops", &paths, &root).is_err());
        let out = command("remove Nope", &paths, &root).unwrap();
        assert!(out[0].starts_with("No local or user rule"));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn add_dir_requires_an_existing_directory() {
        assert!(add_dir(Path::new("/definitely/not/here")).is_err());
        let dir = std::env::temp_dir();
        let added = add_dir(&dir).unwrap();
        assert!(runtime_dirs().contains(&added));
        let file = dir.join(format!("io_adddir_{}", unique()));
        std::fs::write(&file, "x").unwrap();
        assert!(add_dir(&file).is_err());
        let _ = std::fs::remove_file(file);
    }
}
