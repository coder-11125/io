//! Pasting images from the system clipboard (Ctrl+V) and recognizing pasted
//! image paths (drag-and-drop into the terminal). Both end up as an
//! `@path` mention in the prompt, which [`crate::input`] turns into an
//! attached image.
//!
//! Reading the clipboard shells out to whichever helper is installed:
//! `pngpaste` (macOS), `wl-paste` (Wayland) or `xclip` (X11).

use io_runtime::tools::read::sniff_image;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// How long a clipboard helper may run before it is given up on.
const HELPER_TIMEOUT: Duration = Duration::from_millis(1500);
/// Pasted images older than this are deleted when a new one is saved.
const KEEP_FOR: Duration = Duration::from_secs(24 * 60 * 60);

/// Run `program args…` and return its stdout, or `None` if it is missing,
/// fails, produces nothing, or takes too long.
fn capture(program: &str, args: &[&str]) -> Option<Vec<u8>> {
    let mut child = Command::new(program)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    // Drain stdout on a thread so a large image can't fill the pipe and
    // deadlock the child while we wait for it to exit.
    let mut stdout = child.stdout.take()?;
    let reader = std::thread::spawn(move || {
        let mut buf = Vec::new();
        let _ = stdout.read_to_end(&mut buf);
        buf
    });
    let started = Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if started.elapsed() < HELPER_TIMEOUT => {
                std::thread::sleep(Duration::from_millis(15))
            }
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
        }
    };
    let bytes = reader.join().ok()?;
    (status.success() && !bytes.is_empty()).then_some(bytes)
}

/// The image on the clipboard, as `(media type, bytes)`.
pub fn read_image() -> Option<(&'static str, Vec<u8>)> {
    let candidates: [(&str, &[&str]); 3] = [
        ("pngpaste", &["-"]),
        ("wl-paste", &["--no-newline", "--type", "image/png"]),
        (
            "xclip",
            &["-selection", "clipboard", "-t", "image/png", "-o"],
        ),
    ];
    candidates.iter().find_map(|(program, args)| {
        let bytes = capture(program, args)?;
        sniff_image(&bytes).map(|media_type| (media_type, bytes))
    })
}

fn extension_for(media_type: &str) -> &'static str {
    match media_type {
        "image/png" => "png",
        "image/jpeg" => "jpg",
        "image/gif" => "gif",
        "image/webp" => "webp",
        _ => "img",
    }
}

fn paste_dir() -> PathBuf {
    std::env::temp_dir().join("io-pasted")
}

/// Write pasted image bytes to a fresh file under the temp dir and return its
/// path, after clearing out day-old pastes.
pub fn save_image(media_type: &str, bytes: &[u8]) -> std::io::Result<PathBuf> {
    let dir = paste_dir();
    std::fs::create_dir_all(&dir)?;
    if let Ok(entries) = std::fs::read_dir(&dir) {
        for entry in entries.flatten() {
            let stale = entry
                .metadata()
                .and_then(|m| m.modified())
                .ok()
                .and_then(|t| t.elapsed().ok())
                .is_some_and(|age| age > KEEP_FOR);
            if stale {
                let _ = std::fs::remove_file(entry.path());
            }
        }
    }
    let name = format!("image-{}.{}", uuid_like(), extension_for(media_type));
    let path = dir.join(name);
    std::fs::write(&path, bytes)?;
    Ok(path)
}

/// Eight hex characters, unique per call within a process (the low byte is
/// a counter) and unlikely to collide across processes (the rest is time).
/// A file name, not a secret.
fn uuid_like() -> String {
    use std::sync::atomic::{AtomicU32, Ordering};
    static COUNTER: AtomicU32 = AtomicU32::new(0);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.subsec_nanos())
        .unwrap_or(0);
    format!(
        "{:06x}{:02x}",
        nanos & 0xFF_FFFF,
        COUNTER.fetch_add(1, Ordering::Relaxed) & 0xFF
    )
}

/// `@path` mention text for `path`, quoted when it contains whitespace.
pub fn mention_for(path: &Path) -> String {
    let p = path.display().to_string();
    if p.chars().any(char::is_whitespace) {
        format!("@\"{p}\" ")
    } else {
        format!("@{p} ")
    }
}

/// If pasted `text` is just the path of an existing image file — what a
/// terminal inserts when a file is dragged into it, possibly quoted or with
/// backslash-escaped spaces — return that path.
pub fn pasted_image_path(text: &str) -> Option<PathBuf> {
    let t = text.trim();
    if t.is_empty() || t.contains('\n') {
        return None;
    }
    let unquoted = t
        .strip_prefix('\'')
        .and_then(|s| s.strip_suffix('\''))
        .or_else(|| t.strip_prefix('"').and_then(|s| s.strip_suffix('"')))
        .unwrap_or(t);
    let candidate = unquoted.replace("\\ ", " ");
    let path = PathBuf::from(candidate.strip_prefix("file://").unwrap_or(&candidate));
    if !path.is_file() {
        return None;
    }
    let mut header = [0u8; 16];
    let n = std::fs::File::open(&path).ok()?.read(&mut header).ok()?;
    sniff_image(&header[..n]).map(|_| path)
}

#[cfg(test)]
mod tests {
    use super::*;

    const PNG_HEADER: &[u8] = b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR";

    fn temp(name: &str, bytes: &[u8]) -> PathBuf {
        let p = std::env::temp_dir().join(format!("io_clip_test_{}_{name}", uuid_like()));
        std::fs::write(&p, bytes).unwrap();
        p
    }

    #[test]
    fn pasted_paths_to_images_are_recognized_in_common_shell_forms() {
        let img = temp("a b.png", PNG_HEADER);
        let plain = img.display().to_string();
        for pasted in [
            plain.clone(),
            format!("  {plain}\n"),
            format!("'{plain}'"),
            format!("\"{plain}\""),
            plain.replace(' ', "\\ "),
            format!("file://{plain}"),
        ] {
            assert_eq!(
                pasted_image_path(&pasted).as_deref(),
                Some(img.as_path()),
                "{pasted:?}"
            );
        }
        std::fs::remove_file(&img).ok();
    }

    #[test]
    fn other_pastes_are_not_mistaken_for_images() {
        let text = temp("note.txt", b"just words");
        assert_eq!(pasted_image_path(&text.display().to_string()), None);
        std::fs::remove_file(&text).ok();
        assert_eq!(pasted_image_path("/no/such/file.png"), None);
        assert_eq!(pasted_image_path("fn main() {}"), None);
        assert_eq!(pasted_image_path("two\nlines"), None);
        assert_eq!(pasted_image_path(""), None);
    }

    #[test]
    fn mentions_quote_paths_with_spaces() {
        assert_eq!(mention_for(Path::new("/tmp/a.png")), "@/tmp/a.png ");
        assert_eq!(mention_for(Path::new("/tmp/a b.png")), "@\"/tmp/a b.png\" ");
    }

    #[test]
    fn saved_images_land_in_the_paste_dir_with_a_matching_extension() {
        let path = save_image("image/jpeg", b"\xff\xd8\xff\xe0data").unwrap();
        assert!(path.starts_with(paste_dir()));
        assert_eq!(path.extension().and_then(|e| e.to_str()), Some("jpg"));
        assert_eq!(std::fs::read(&path).unwrap(), b"\xff\xd8\xff\xe0data");
        std::fs::remove_file(&path).ok();
        assert_ne!(uuid_like(), uuid_like());
    }

    #[test]
    fn missing_helpers_and_failures_yield_nothing() {
        assert_eq!(capture("definitely-not-a-real-program-xyz", &[]), None);
        assert_eq!(capture("false", &[]), None);
        assert_eq!(capture("true", &[]), None, "empty output is nothing");
        assert_eq!(capture("echo", &["hi"]).as_deref(), Some(&b"hi\n"[..]));
    }

    #[test]
    fn a_hung_helper_is_killed_after_the_timeout() {
        let started = Instant::now();
        assert_eq!(capture("sleep", &["30"]), None);
        assert!(started.elapsed() < Duration::from_secs(10));
    }
}
