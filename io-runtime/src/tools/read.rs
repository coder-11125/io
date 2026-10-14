use super::file_state::FileTracker;
use super::notebook;
use super::{Tool, ToolInput, ToolOutput};
use base64::{engine::general_purpose::STANDARD as BASE64, Engine as _};
use std::sync::Arc;

/// Largest image sent to the model. Providers cap images at about this size.
const MAX_IMAGE_BYTES: usize = 5 * 1024 * 1024;
/// Pages of a PDF shown when the caller doesn't pick a range.
const DEFAULT_PDF_PAGES: usize = 20;
/// Cap on extracted PDF text per call.
const MAX_PDF_CHARS: usize = 100_000;

pub struct ReadTool {
    tracker: Arc<FileTracker>,
}

impl ReadTool {
    pub fn new(tracker: Arc<FileTracker>) -> Self {
        Self { tracker }
    }
}

impl Default for ReadTool {
    fn default() -> Self {
        Self::new(FileTracker::shared())
    }
}

#[async_trait::async_trait]
impl Tool for ReadTool {
    fn name(&self) -> &str {
        "read"
    }

    fn description(&self) -> &str {
        "Read a file. Text files come back with line numbers (use offset and limit for part of a \
         large file). Images (PNG, JPEG, GIF, WebP) are shown to you visually. PDFs are returned \
         as extracted text; pass `pages` (e.g. \"3\", \"2-5\", \"1,4-6\") to choose pages. \
         Jupyter notebooks (.ipynb) are returned cell by cell with their outputs. A file must be \
         read before it can be edited."
    }

    fn input_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "file_path": { "type": "string", "description": "Absolute path to the file to read" },
                "offset": { "type": "integer", "description": "Line number to start reading from (0-indexed)", "default": 0 },
                "limit": { "type": "integer", "description": "Maximum number of lines to read", "default": 2000 },
                "pages": { "type": "string", "description": "PDF only: pages to read, e.g. \"3\", \"2-5\" or \"1,4-6\" (1-indexed; default: the first 20)" }
            },
            "required": ["file_path"]
        })
    }

    async fn execute(&self, input: ToolInput) -> ToolOutput {
        let file_path = match input.args.get("file_path").and_then(|v| v.as_str()) {
            Some(p) => p,
            None => return ToolOutput::err("missing required argument: file_path"),
        };

        let resolved = match super::resolve_safe_path(file_path) {
            Ok(p) => p,
            Err(e) => return ToolOutput::err(e),
        };

        let offset = input
            .args
            .get("offset")
            .and_then(|v| v.as_i64())
            .unwrap_or(0) as usize;
        let limit = input
            .args
            .get("limit")
            .and_then(|v| v.as_i64())
            .unwrap_or(2000) as usize;

        let bytes = match std::fs::read(&resolved) {
            Ok(b) => b,
            Err(e) => {
                return ToolOutput::err(format!("failed to read {}: {e}", resolved.display()))
            }
        };

        if let Some(media_type) = sniff_image(&bytes) {
            return read_image(&resolved, &bytes, media_type);
        }
        if bytes.starts_with(b"%PDF") {
            let pages = input.args.get("pages").and_then(|v| v.as_str());
            return read_pdf(bytes, pages).await;
        }

        let contents = match String::from_utf8(bytes) {
            Ok(c) => c,
            Err(_) => {
                return ToolOutput::err(format!(
                    "{} is a binary file and can't be read as text",
                    resolved.display()
                ))
            }
        };
        // Reading the whole file (even when only a slice is shown) is what
        // later edits are checked against.
        self.tracker.record(&resolved, &contents);

        if notebook::is_notebook(&resolved) {
            return match notebook::parse(&contents) {
                Ok(nb) => ToolOutput::ok(notebook::render(&nb)),
                Err(e) => ToolOutput::err(e),
            };
        }

        let lines: Vec<&str> = contents.lines().collect();
        let total = lines.len();

        if offset >= total {
            return ToolOutput::ok(format!("[offset {offset} beyond file length {total}]"));
        }

        let end = (offset + limit).min(total);
        let excerpt: Vec<String> = lines[offset..end]
            .iter()
            .enumerate()
            .map(|(i, line)| format!("{i}:{line}"))
            .collect();

        let mut result = excerpt.join("\n");
        if end < total {
            result.push_str(&format!("\n... ({}/{}) lines shown", end - offset, total));
        } else {
            result.push_str(&format!("\n--- {total} total lines ---"));
        }

        ToolOutput::ok(result)
    }
}

// ── Images ─────────────────────────────────────────────────────────────────────

/// The image MIME type of `bytes`, judged by magic number — not the file
/// extension, which can lie.
pub fn sniff_image(bytes: &[u8]) -> Option<&'static str> {
    if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        Some("image/png")
    } else if bytes.starts_with(&[0xFF, 0xD8, 0xFF]) {
        Some("image/jpeg")
    } else if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") {
        Some("image/gif")
    } else if bytes.len() >= 12 && &bytes[0..4] == b"RIFF" && &bytes[8..12] == b"WEBP" {
        Some("image/webp")
    } else {
        None
    }
}

/// Package `bytes` as an image to attach to a message. `Ok(None)` if they are
/// not a supported image; an error if the image is over the size limit.
pub fn encode_image(bytes: &[u8]) -> Result<Option<super::ToolImage>, String> {
    let Some(media_type) = sniff_image(bytes) else {
        return Ok(None);
    };
    if bytes.len() > MAX_IMAGE_BYTES {
        return Err(format!(
            "image is {:.1} MB, over the {} MB limit",
            bytes.len() as f64 / (1024.0 * 1024.0),
            MAX_IMAGE_BYTES / (1024 * 1024)
        ));
    }
    Ok(Some(super::ToolImage {
        media_type: media_type.to_string(),
        data: BASE64.encode(bytes),
    }))
}

/// Pixel dimensions, when cheap to read from the header.
fn image_dimensions(bytes: &[u8], media_type: &str) -> Option<(u32, u32)> {
    match media_type {
        "image/png" if bytes.len() >= 24 => Some((
            u32::from_be_bytes(bytes[16..20].try_into().ok()?),
            u32::from_be_bytes(bytes[20..24].try_into().ok()?),
        )),
        "image/gif" if bytes.len() >= 10 => Some((
            u16::from_le_bytes([bytes[6], bytes[7]]) as u32,
            u16::from_le_bytes([bytes[8], bytes[9]]) as u32,
        )),
        "image/jpeg" => {
            // Walk the marker segments to the first start-of-frame.
            let mut i = 2;
            while i + 9 < bytes.len() {
                if bytes[i] != 0xFF {
                    i += 1;
                    continue;
                }
                let marker = bytes[i + 1];
                if (0xC0..=0xCF).contains(&marker) && !matches!(marker, 0xC4 | 0xC8 | 0xCC) {
                    let h = u16::from_be_bytes([bytes[i + 5], bytes[i + 6]]) as u32;
                    let w = u16::from_be_bytes([bytes[i + 7], bytes[i + 8]]) as u32;
                    return Some((w, h));
                }
                let len = u16::from_be_bytes([bytes[i + 2], bytes[i + 3]]) as usize;
                i += 2 + len;
            }
            None
        }
        _ => None,
    }
}

fn read_image(path: &std::path::Path, bytes: &[u8], media_type: &str) -> ToolOutput {
    if bytes.len() > MAX_IMAGE_BYTES {
        return ToolOutput::err(format!(
            "{} is {:.1} MB, over the {} MB image limit — resize or compress it first",
            path.display(),
            bytes.len() as f64 / (1024.0 * 1024.0),
            MAX_IMAGE_BYTES / (1024 * 1024)
        ));
    }
    let dims = image_dimensions(bytes, media_type)
        .map(|(w, h)| format!(", {w}x{h}"))
        .unwrap_or_default();
    ToolOutput::ok(format!(
        "Image file {} ({media_type}{dims}, {} KB) — shown below.",
        path.display(),
        bytes.len().div_ceil(1024)
    ))
    .with_image(media_type, BASE64.encode(bytes))
}

// ── PDFs ───────────────────────────────────────────────────────────────────────

/// Parse a 1-indexed page selection like `3`, `2-5` or `1,4-6` into sorted,
/// de-duplicated 0-indexed page numbers, clipped to `total`.
fn parse_page_ranges(spec: &str, total: usize) -> Result<Vec<usize>, String> {
    let bad = || format!("invalid pages '{spec}' — use e.g. \"3\", \"2-5\" or \"1,4-6\"");
    let mut out = std::collections::BTreeSet::new();
    for part in spec.split(',') {
        let part = part.trim();
        let (start, end) = match part.split_once('-') {
            Some((a, b)) => (
                a.trim().parse::<usize>().map_err(|_| bad())?,
                b.trim().parse::<usize>().map_err(|_| bad())?,
            ),
            None => {
                let n = part.parse::<usize>().map_err(|_| bad())?;
                (n, n)
            }
        };
        if start == 0 || end < start {
            return Err(bad());
        }
        for page in start..=end.min(total) {
            out.insert(page - 1);
        }
    }
    if out.is_empty() {
        return Err(format!(
            "pages '{spec}' is outside the document ({total} page(s))"
        ));
    }
    Ok(out.into_iter().collect())
}

async fn read_pdf(bytes: Vec<u8>, pages: Option<&str>) -> ToolOutput {
    // The extractor can panic on malformed files; contain that to this call.
    let extracted = tokio::task::spawn_blocking(move || {
        std::panic::catch_unwind(|| pdf_extract::extract_text_from_mem_by_pages(&bytes))
    })
    .await;
    let page_texts = match extracted {
        Ok(Ok(Ok(p))) => p,
        Ok(Ok(Err(e))) => return ToolOutput::err(format!("failed to read PDF: {e}")),
        _ => return ToolOutput::err("failed to read PDF: the file could not be parsed"),
    };
    let total = page_texts.len();
    if total == 0 {
        return ToolOutput::err("the PDF has no pages");
    }

    let (selected, truncated_default) = match pages {
        Some(spec) => match parse_page_ranges(spec, total) {
            Ok(p) => (p, false),
            Err(e) => return ToolOutput::err(e),
        },
        None => (
            (0..total.min(DEFAULT_PDF_PAGES)).collect(),
            total > DEFAULT_PDF_PAGES,
        ),
    };

    let mut out = format!("PDF with {total} page(s).\n");
    let mut any_text = false;
    let mut used = 0usize;
    for &i in &selected {
        let text = page_texts[i].trim();
        any_text |= !text.is_empty();
        let section = format!("\n--- Page {} ---\n{text}\n", i + 1);
        used += section.chars().count();
        if used > MAX_PDF_CHARS {
            out.push_str(&format!(
                "\n… output cut at page {} to stay under {MAX_PDF_CHARS} characters — request \
                 fewer pages with `pages`.\n",
                i + 1
            ));
            break;
        }
        out.push_str(&section);
    }
    if !any_text {
        out.push_str(
            "\nNo text could be extracted from these pages — the PDF is probably scanned images.\n",
        );
    }
    if truncated_default {
        out.push_str(&format!(
            "\nShowing the first {DEFAULT_PDF_PAGES} of {total} pages; use `pages` to read others.\n"
        ));
    }
    ToolOutput::ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn input(args: serde_json::Value) -> ToolInput {
        ToolInput {
            name: "read".into(),
            args: args
                .as_object()
                .unwrap()
                .iter()
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect(),
        }
    }

    fn write_temp(name: &str, content: &str) -> std::path::PathBuf {
        write_temp_bytes(name, content.as_bytes())
    }

    fn write_temp_bytes(name: &str, content: &[u8]) -> std::path::PathBuf {
        let path =
            std::env::temp_dir().join(format!("io_read_test_{}_{}", uuid::Uuid::new_v4(), name));
        std::fs::write(&path, content).unwrap();
        path
    }

    /// A 1x1 transparent PNG.
    const PNG_1X1: &[u8] = &[
        0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A, 0x00, 0x00, 0x00, 0x0D, 0x49, 0x48, 0x44,
        0x52, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x06, 0x00, 0x00, 0x00, 0x1F,
        0x15, 0xC4, 0x89, 0x00, 0x00, 0x00, 0x0D, 0x49, 0x44, 0x41, 0x54, 0x78, 0x9C, 0x63, 0x00,
        0x01, 0x00, 0x00, 0x05, 0x00, 0x01, 0x0D, 0x0A, 0x2D, 0xB4, 0x00, 0x00, 0x00, 0x00, 0x49,
        0x45, 0x4E, 0x44, 0xAE, 0x42, 0x60, 0x82,
    ];

    /// A minimal valid PDF whose pages each show one line of text.
    fn tiny_pdf(pages: &[&str]) -> Vec<u8> {
        let mut objs: Vec<String> = vec![
            "<< /Type /Catalog /Pages 2 0 R >>".into(),
            format!(
                "<< /Type /Pages /Kids [{}] /Count {} >>",
                (0..pages.len())
                    .map(|i| format!("{} 0 R", 4 + i * 2))
                    .collect::<Vec<_>>()
                    .join(" "),
                pages.len()
            ),
            "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>".into(),
        ];
        for (i, text) in pages.iter().enumerate() {
            objs.push(format!(
                "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents {} 0 R \
                 /Resources << /Font << /F1 3 0 R >> >> >>",
                5 + i * 2
            ));
            let stream = format!("BT /F1 24 Tf 72 700 Td ({text}) Tj ET");
            objs.push(format!(
                "<< /Length {} >>\nstream\n{stream}\nendstream",
                stream.len()
            ));
        }
        let mut out = b"%PDF-1.4\n".to_vec();
        let mut offsets = Vec::new();
        for (i, obj) in objs.iter().enumerate() {
            offsets.push(out.len());
            out.extend_from_slice(format!("{} 0 obj\n{obj}\nendobj\n", i + 1).as_bytes());
        }
        let xref = out.len();
        out.extend_from_slice(
            format!("xref\n0 {}\n0000000000 65535 f \n", objs.len() + 1).as_bytes(),
        );
        for off in offsets {
            out.extend_from_slice(format!("{off:010} 00000 n \n").as_bytes());
        }
        out.extend_from_slice(
            format!(
                "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n",
                objs.len() + 1
            )
            .as_bytes(),
        );
        out
    }

    #[tokio::test]
    async fn missing_file_path_returns_error() {
        let out = ReadTool::default()
            .execute(input(serde_json::json!({})))
            .await;
        assert!(!out.success);
        assert!(out.data.contains("missing required argument: file_path"));
    }

    #[tokio::test]
    async fn nonexistent_file_returns_error() {
        let out = ReadTool::default()
            .execute(input(serde_json::json!({
                "file_path": "/tmp/io_test_nonexistent_file_xyz_123"
            })))
            .await;
        assert!(!out.success);
    }

    #[tokio::test]
    async fn reads_file_content() {
        let path = write_temp("basic.txt", "hello\nworld\n");
        let out = ReadTool::default()
            .execute(input(serde_json::json!({
                "file_path": path.to_str().unwrap()
            })))
            .await;
        std::fs::remove_file(&path).ok();
        assert!(out.success);
        assert!(out.data.contains("hello"));
        assert!(out.data.contains("world"));
        assert!(out.images.is_empty());
    }

    #[tokio::test]
    async fn offset_skips_lines() {
        let path = write_temp("offset.txt", "line1\nline2\nline3\n");
        let out = ReadTool::default()
            .execute(input(serde_json::json!({
                "file_path": path.to_str().unwrap(),
                "offset": 1
            })))
            .await;
        std::fs::remove_file(&path).ok();
        assert!(out.success);
        assert!(!out.data.contains("line1"));
        assert!(out.data.contains("line2"));
    }

    #[tokio::test]
    async fn limit_caps_output() {
        let content = (1..=10)
            .map(|i| format!("line{i}"))
            .collect::<Vec<_>>()
            .join("\n");
        let path = write_temp("limit.txt", &content);
        let out = ReadTool::default()
            .execute(input(serde_json::json!({
                "file_path": path.to_str().unwrap(),
                "limit": 3
            })))
            .await;
        std::fs::remove_file(&path).ok();
        assert!(out.success);
        assert!(out.data.contains("line1"));
        assert!(!out.data.contains("line4"));
    }

    #[tokio::test]
    async fn reading_records_the_file_so_it_can_be_edited() {
        let path = write_temp("tracked.txt", "a\nb\n");
        let tracker = FileTracker::shared();
        assert!(tracker.check(&path, "a\nb\n").is_err());
        // Even a partial read counts: the whole file is what's fingerprinted.
        let out = ReadTool::new(tracker.clone())
            .execute(input(serde_json::json!({
                "file_path": path.to_str().unwrap(),
                "limit": 1
            })))
            .await;
        std::fs::remove_file(&path).ok();
        assert!(out.success);
        assert!(tracker.check(&path, "a\nb\n").is_ok());
    }

    #[tokio::test]
    async fn binary_files_are_reported_not_decoded() {
        let path = write_temp_bytes("blob.bin", &[0xFF, 0x00, 0xFE, 0x80, 0x81]);
        let out = ReadTool::default()
            .execute(input(
                serde_json::json!({"file_path": path.to_str().unwrap()}),
            ))
            .await;
        std::fs::remove_file(&path).ok();
        assert!(!out.success);
        assert!(out.data.contains("binary file"));
    }

    #[tokio::test]
    async fn images_are_returned_as_image_data_by_magic_number() {
        // The extension is wrong on purpose: sniffing wins.
        let path = write_temp_bytes("shot.txt", PNG_1X1);
        let out = ReadTool::default()
            .execute(input(
                serde_json::json!({"file_path": path.to_str().unwrap()}),
            ))
            .await;
        std::fs::remove_file(&path).ok();
        assert!(out.success, "{}", out.data);
        assert!(out.data.contains("image/png"));
        assert!(out.data.contains("1x1"), "{}", out.data);
        assert_eq!(out.images.len(), 1);
        assert_eq!(out.images[0].media_type, "image/png");
        assert_eq!(BASE64.decode(&out.images[0].data).unwrap(), PNG_1X1);
    }

    #[tokio::test]
    async fn oversized_images_are_refused() {
        let mut big = PNG_1X1.to_vec();
        big.resize(MAX_IMAGE_BYTES + 1, 0);
        let path = write_temp_bytes("big.png", &big);
        let out = ReadTool::default()
            .execute(input(
                serde_json::json!({"file_path": path.to_str().unwrap()}),
            ))
            .await;
        std::fs::remove_file(&path).ok();
        assert!(!out.success);
        assert!(out.data.contains("image limit"));
        assert!(out.images.is_empty());
    }

    #[test]
    fn sniffing_recognizes_the_supported_formats() {
        assert_eq!(sniff_image(PNG_1X1), Some("image/png"));
        assert_eq!(sniff_image(&[0xFF, 0xD8, 0xFF, 0xE0]), Some("image/jpeg"));
        assert_eq!(sniff_image(b"GIF89a...."), Some("image/gif"));
        assert_eq!(
            sniff_image(b"RIFF\x00\x00\x00\x00WEBPVP8 "),
            Some("image/webp")
        );
        assert_eq!(sniff_image(b"just text"), None);
        assert_eq!(sniff_image(b""), None);
    }

    #[test]
    fn encode_image_packages_images_and_enforces_the_size_limit() {
        let img = encode_image(PNG_1X1).unwrap().expect("a png");
        assert_eq!(img.media_type, "image/png");
        assert_eq!(BASE64.decode(&img.data).unwrap(), PNG_1X1);
        assert_eq!(encode_image(b"not an image").unwrap(), None);
        let mut big = PNG_1X1.to_vec();
        big.resize(MAX_IMAGE_BYTES + 1, 0);
        assert!(encode_image(&big).unwrap_err().contains("limit"));
    }

    #[test]
    fn dimensions_are_read_from_png_gif_and_jpeg_headers() {
        assert_eq!(image_dimensions(PNG_1X1, "image/png"), Some((1, 1)));
        let mut gif = b"GIF89a".to_vec();
        gif.extend_from_slice(&[0x40, 0x01, 0xC8, 0x00]); // 320x200
        assert_eq!(image_dimensions(&gif, "image/gif"), Some((320, 200)));
        // JPEG: SOI, APP0 (len 4), SOF0 with height 0x0010, width 0x0020.
        let jpeg = [
            0xFF, 0xD8, 0xFF, 0xE0, 0x00, 0x04, 0x00, 0x00, 0xFF, 0xC0, 0x00, 0x0B, 0x08, 0x00,
            0x10, 0x00, 0x20, 0x01, 0x01, 0x11, 0x00,
        ];
        assert_eq!(image_dimensions(&jpeg, "image/jpeg"), Some((32, 16)));
    }

    #[tokio::test]
    async fn notebooks_are_rendered_cell_by_cell_and_tracked() {
        let nb = r#"{"cells":[{"cell_type":"code","metadata":{},"execution_count":1,
            "outputs":[{"output_type":"stream","name":"stdout","text":["hi\n"]}],
            "source":["print('hi')"]}],"metadata":{},"nbformat":4,"nbformat_minor":4}"#;
        let path = write_temp("demo.ipynb", nb);
        let tracker = FileTracker::shared();
        let out = ReadTool::new(tracker.clone())
            .execute(input(
                serde_json::json!({"file_path": path.to_str().unwrap()}),
            ))
            .await;
        std::fs::remove_file(&path).ok();
        assert!(out.success, "{}", out.data);
        assert!(out.data.contains("--- Cell 0 [code] ---"));
        assert!(out.data.contains("print('hi')"));
        assert!(out.data.contains("[output]\nhi"));
        assert!(tracker.check(&path, nb).is_ok());
    }

    #[test]
    fn page_ranges_parse_and_clip() {
        assert_eq!(parse_page_ranges("3", 10).unwrap(), vec![2]);
        assert_eq!(parse_page_ranges("2-4", 10).unwrap(), vec![1, 2, 3]);
        assert_eq!(parse_page_ranges("1,4-5, 4", 10).unwrap(), vec![0, 3, 4]);
        assert_eq!(parse_page_ranges("8-20", 10).unwrap(), vec![7, 8, 9]);
        assert!(parse_page_ranges("0", 10).is_err());
        assert!(parse_page_ranges("5-2", 10).is_err());
        assert!(parse_page_ranges("x", 10).is_err());
        assert!(parse_page_ranges("11", 10).unwrap_err().contains("outside"));
    }

    #[tokio::test]
    async fn pdfs_are_returned_as_page_tagged_text() {
        let path = write_temp_bytes(
            "doc.pdf",
            &tiny_pdf(&["First page text", "Second page text"]),
        );
        let out = ReadTool::default()
            .execute(input(
                serde_json::json!({"file_path": path.to_str().unwrap()}),
            ))
            .await;
        assert!(out.success, "{}", out.data);
        assert!(out.data.contains("2 page(s)"), "{}", out.data);
        assert!(out.data.contains("--- Page 1 ---"));
        assert!(out.data.contains("First page text"), "{}", out.data);
        assert!(out.data.contains("Second page text"));

        // `pages` selects just one.
        let out = ReadTool::default()
            .execute(input(serde_json::json!({
                "file_path": path.to_str().unwrap(),
                "pages": "2"
            })))
            .await;
        std::fs::remove_file(&path).ok();
        assert!(out.success, "{}", out.data);
        assert!(!out.data.contains("First page text"));
        assert!(out.data.contains("Second page text"));
    }

    #[tokio::test]
    async fn broken_pdfs_error_instead_of_panicking() {
        let path = write_temp_bytes("broken.pdf", b"%PDF-1.4\nthis is not a pdf at all");
        let out = ReadTool::default()
            .execute(input(
                serde_json::json!({"file_path": path.to_str().unwrap()}),
            ))
            .await;
        std::fs::remove_file(&path).ok();
        assert!(!out.success);
        assert!(out.data.contains("failed to read PDF"), "{}", out.data);
    }
}
