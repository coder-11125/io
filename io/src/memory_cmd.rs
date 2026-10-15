//! Memory from the REPL: `# note` quick-add and the `/memory` editor menu.
//! (`/init` is just a prompt — see `io_runtime::instructions::INIT_PROMPT`.)

use io_runtime::instructions;
use io_tui::{picker, render};
use std::path::{Path, PathBuf};

/// Whether `input` is a `# note` quick-add (a `#` followed by a space).
pub fn is_quick_add(input: &str) -> bool {
    input.starts_with("# ") && !input[2..].trim().is_empty()
}

/// Ask where to save `note` and append it. `Ok(None)` if the user backed out.
pub fn quick_add(note: &str, root: &Path) -> anyhow::Result<Option<String>> {
    let home = dirs::home_dir();
    let targets = instructions::targets(root, home.as_deref());
    let mut labels: Vec<&str> = targets.iter().map(|(l, _)| l.as_str()).collect();
    labels.push("Cancel");
    println!();
    let idx = match picker::pick(&labels, Some(0)) {
        Ok(i) => i,
        Err(e) if e.is::<picker::Dismissed>() => return Ok(None),
        Err(e) => return Err(e),
    };
    let Some((_, path)) = targets.get(idx) else {
        return Ok(None);
    };
    instructions::append_note(path, note.trim_start_matches('#').trim())?;
    Ok(Some(format!("Saved to {}", path.display())))
}

/// Everything `/memory` can open: the memory files in use, then the standard
/// places a new one would go.
fn menu_entries(root: &Path) -> Vec<(String, PathBuf)> {
    let home = dirs::home_dir();
    let cwd = std::env::current_dir().unwrap_or_else(|_| root.to_path_buf());
    let mut entries: Vec<(String, PathBuf)> = Vec::new();
    for f in instructions::discover(root, &cwd, home.as_deref()) {
        if f.imported_by.is_none() {
            entries.push((f.label, f.path));
        }
    }
    for (label, path) in instructions::targets(root, home.as_deref()) {
        if !entries.iter().any(|(_, p)| *p == path) {
            entries.push((format!("{label} — new"), path));
        }
    }
    entries
}

/// Pick a memory file and open it in `$VISUAL` / `$EDITOR`. `Ok(None)` if the
/// user backed out.
pub fn edit_menu(root: &Path) -> anyhow::Result<Option<String>> {
    let entries = menu_entries(root);
    let items: Vec<&str> = entries.iter().map(|(l, _)| l.as_str()).collect();
    println!();
    let idx = match picker::pick(&items, Some(0)) {
        Ok(i) => i,
        Err(e) if e.is::<picker::Dismissed>() => return Ok(None),
        Err(e) => return Err(e),
    };
    let path = &entries[idx].1;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    open_in_editor(path)?;
    Ok(Some(format!("Edited {}", path.display())))
}

fn open_in_editor(path: &Path) -> anyhow::Result<()> {
    render::exit_tui()?;
    let status = std::process::Command::new("sh")
        .args(["-c", "${VISUAL:-${EDITOR:-vi}} \"$1\"", "sh"])
        .arg(path)
        .status();
    render::enter_tui()?;
    let status = status?;
    if !status.success() {
        anyhow::bail!("the editor exited with {status}");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_hash_space_note_is_a_quick_add() {
        assert!(is_quick_add("# always run clippy"));
        assert!(!is_quick_add("#"));
        assert!(!is_quick_add("# "));
        assert!(!is_quick_add("#nospace"));
        assert!(!is_quick_add("## heading"));
        assert!(!is_quick_add("text # not at start"));
    }
}
