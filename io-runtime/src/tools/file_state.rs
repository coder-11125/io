//! Edit safety: remembers what each file looked like when the model last read
//! (or wrote) it, so `write`, `edit`, `multi_edit` and `notebook_edit` can
//! refuse to clobber a file the model never saw, or one that changed on disk
//! since — by the user, a formatter hook, or another process.
//!
//! One tracker is shared by every file tool in a registry (see
//! [`super::filtered_registry`]); a sub-agent's registry has its own, so a
//! sub-agent must read what it edits too.

use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

/// What the model last saw of a file: a fingerprint of its whole content.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Stamp(u64);

impl Stamp {
    fn of(content: &str) -> Self {
        let mut h = std::collections::hash_map::DefaultHasher::new();
        content.hash(&mut h);
        Stamp(h.finish())
    }
}

#[derive(Debug, Default)]
pub struct FileTracker {
    seen: Mutex<HashMap<PathBuf, Stamp>>,
}

impl FileTracker {
    pub fn shared() -> Arc<Self> {
        Arc::new(Self::default())
    }

    /// Record that the model has now seen `content` at `path`. Called after a
    /// read (of the whole file, even if only a slice was shown) and after the
    /// tool's own writes, so consecutive edits don't each demand a re-read.
    pub fn record(&self, path: &Path, content: &str) {
        self.seen
            .lock()
            .unwrap()
            .insert(path.to_path_buf(), Stamp::of(content));
    }

    /// Whether editing `path`, whose content on disk is `current`, is safe.
    /// The error is written for the model: it says what to do next.
    pub fn check(&self, path: &Path, current: &str) -> Result<(), String> {
        match self.seen.lock().unwrap().get(path) {
            None => Err(format!(
                "{} has not been read yet. Read it with the read tool before modifying it.",
                path.display()
            )),
            Some(stamp) if *stamp != Stamp::of(current) => Err(format!(
                "{} has been modified since it was last read (by the user, a formatter or \
                 another process). Read it again before modifying it.",
                path.display()
            )),
            Some(_) => Ok(()),
        }
    }

    /// Forget a file, e.g. after it was deleted.
    pub fn forget(&self, path: &Path) {
        self.seen.lock().unwrap().remove(path);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unread_files_are_rejected() {
        let t = FileTracker::default();
        let err = t.check(Path::new("/a/b.rs"), "x").unwrap_err();
        assert!(err.contains("has not been read yet"), "{err}");
    }

    #[test]
    fn unchanged_files_pass_and_changed_files_are_rejected() {
        let t = FileTracker::default();
        let p = Path::new("/a/b.rs");
        t.record(p, "fn main() {}");
        assert!(t.check(p, "fn main() {}").is_ok());
        let err = t.check(p, "fn main() { println!() }").unwrap_err();
        assert!(err.contains("modified since it was last read"), "{err}");
    }

    #[test]
    fn recording_again_accepts_the_new_content() {
        let t = FileTracker::default();
        let p = Path::new("/a/b.rs");
        t.record(p, "one");
        t.record(p, "two");
        assert!(t.check(p, "two").is_ok());
        assert!(t.check(p, "one").is_err());
    }

    #[test]
    fn paths_are_tracked_independently_and_can_be_forgotten() {
        let t = FileTracker::default();
        t.record(Path::new("/a"), "x");
        assert!(t.check(Path::new("/b"), "x").is_err());
        t.forget(Path::new("/a"));
        assert!(t.check(Path::new("/a"), "x").is_err());
    }
}
