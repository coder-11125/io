//! Prompt history: what the user typed before, for Up/Down and Ctrl+R.
//! Stored one JSON string per line in `~/.io/history.jsonl` (so multi-line
//! prompts survive), newest last, capped at [`MAX_ENTRIES`].

use std::path::PathBuf;

pub const MAX_ENTRIES: usize = 1000;

#[derive(Debug, Default)]
pub struct History {
    entries: Vec<String>,
    path: Option<PathBuf>,
}

impl History {
    pub fn default_path() -> PathBuf {
        dirs::home_dir()
            .unwrap_or_else(|| PathBuf::from("~"))
            .join(".io")
            .join("history.jsonl")
    }

    pub fn load() -> Self {
        Self::load_from(Self::default_path())
    }

    pub fn load_from(path: PathBuf) -> Self {
        let mut entries: Vec<String> = std::fs::read_to_string(&path)
            .unwrap_or_default()
            .lines()
            .filter_map(|l| serde_json::from_str::<String>(l).ok())
            .collect();
        if entries.len() > MAX_ENTRIES {
            entries.drain(..entries.len() - MAX_ENTRIES);
        }
        Self {
            entries,
            path: Some(path),
        }
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn get(&self, index: usize) -> Option<&str> {
        self.entries.get(index).map(String::as_str)
    }

    /// Remember `entry`. Blank input and an immediate repeat are skipped.
    pub fn push(&mut self, entry: &str) {
        let entry = entry.trim_end();
        if entry.trim().is_empty() || self.entries.last().map(String::as_str) == Some(entry) {
            return;
        }
        self.entries.push(entry.to_string());
        if self.entries.len() > MAX_ENTRIES {
            self.entries.remove(0);
        }
        if let Some(path) = &self.path {
            use std::io::Write;
            if let Some(parent) = path.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            if let Ok(mut file) = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(path)
            {
                let _ = writeln!(file, "{}", serde_json::to_string(entry).unwrap_or_default());
            }
        }
    }

    /// The most recent entry before `before` (exclusive) containing `query`,
    /// ignoring case. An empty query matches the previous entry.
    pub fn search_back(&self, query: &str, before: usize) -> Option<usize> {
        let needle = query.to_lowercase();
        (0..before.min(self.entries.len()))
            .rev()
            .find(|&i| self.entries[i].to_lowercase().contains(&needle))
    }
}

/// Where Up/Down browsing is: nothing, or an entry with the draft that was
/// being typed when browsing began.
#[derive(Debug, Default)]
pub struct Browse {
    at: Option<usize>,
    draft: String,
}

impl Browse {
    /// Up: the previous entry, remembering `current` as the draft the first time.
    pub fn up(&mut self, history: &History, current: &str) -> Option<String> {
        let next = match self.at {
            None => history.len().checked_sub(1)?,
            Some(0) => return None,
            Some(i) => i - 1,
        };
        if self.at.is_none() {
            self.draft = current.to_string();
        }
        self.at = Some(next);
        history.get(next).map(str::to_string)
    }

    /// Down: the next entry, or the draft once past the newest one.
    pub fn down(&mut self, history: &History) -> Option<String> {
        let i = self.at?;
        if i + 1 < history.len() {
            self.at = Some(i + 1);
            history.get(i + 1).map(str::to_string)
        } else {
            self.at = None;
            Some(std::mem::take(&mut self.draft))
        }
    }

    pub fn reset(&mut self) {
        self.at = None;
        self.draft.clear();
    }

    pub fn active(&self) -> bool {
        self.at.is_some()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp() -> PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        std::env::temp_dir().join(format!("io_hist_{}_{nanos}.jsonl", std::process::id()))
    }

    #[test]
    fn entries_persist_including_multiline_ones() {
        let path = temp();
        let mut h = History::load_from(path.clone());
        h.push("first");
        h.push("two\nlines");
        h.push("first");
        let again = History::load_from(path.clone());
        assert_eq!(again.len(), 3);
        assert_eq!(again.get(1), Some("two\nlines"));
        std::fs::remove_file(path).ok();
    }

    #[test]
    fn blanks_and_immediate_repeats_are_skipped() {
        let mut h = History::default();
        h.push("a");
        h.push("a");
        h.push("   ");
        h.push("");
        h.push("a  ");
        assert_eq!(h.len(), 1);
        h.push("b");
        h.push("a");
        assert_eq!(h.len(), 3);
    }

    #[test]
    fn the_file_is_capped_when_loaded_and_the_list_when_pushed() {
        let path = temp();
        let lines: String = (0..MAX_ENTRIES + 5)
            .map(|i| format!("{}\n", serde_json::to_string(&format!("e{i}")).unwrap()))
            .collect();
        std::fs::write(&path, lines).unwrap();
        let mut h = History::load_from(path.clone());
        assert_eq!(h.len(), MAX_ENTRIES);
        assert_eq!(h.get(0), Some("e5"));
        h.push("new");
        assert_eq!(h.len(), MAX_ENTRIES);
        assert_eq!(h.get(MAX_ENTRIES - 1), Some("new"));
        std::fs::remove_file(path).ok();
    }

    #[test]
    fn corrupt_lines_are_ignored() {
        let path = temp();
        std::fs::write(&path, "\"ok\"\nnot json\n\"fine\"\n").unwrap();
        assert_eq!(History::load_from(path.clone()).len(), 2);
        std::fs::remove_file(path).ok();
    }

    #[test]
    fn search_finds_the_most_recent_match_before_a_position() {
        let mut h = History::default();
        for e in ["cargo test", "git status", "Cargo build", "ls"] {
            h.push(e);
        }
        assert_eq!(h.search_back("cargo", 4), Some(2));
        assert_eq!(h.search_back("cargo", 2), Some(0));
        assert_eq!(h.search_back("cargo", 0), None);
        assert_eq!(h.search_back("zzz", 4), None);
        assert_eq!(h.search_back("", 4), Some(3));
        assert_eq!(h.search_back("git", 99), Some(1));
    }

    #[test]
    fn browsing_walks_back_and_returns_to_the_draft() {
        let mut h = History::default();
        h.push("one");
        h.push("two");
        let mut b = Browse::default();
        assert!(!b.active());
        assert_eq!(b.up(&h, "draft").as_deref(), Some("two"));
        assert_eq!(b.up(&h, "two").as_deref(), Some("one"));
        assert_eq!(b.up(&h, "one"), None, "already at the oldest");
        assert_eq!(b.down(&h).as_deref(), Some("two"));
        assert_eq!(b.down(&h).as_deref(), Some("draft"));
        assert!(!b.active());
        assert_eq!(b.down(&h), None);
        assert_eq!(Browse::default().up(&History::default(), "x"), None);
    }
}
