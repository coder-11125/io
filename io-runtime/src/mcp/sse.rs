//! A minimal Server-Sent Events parser, shared by the HTTP and SSE transports.

/// One dispatched event.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SseEvent {
    /// The `event:` name; `message` when the stream didn't give one.
    pub event: String,
    /// The `data:` lines joined with newlines.
    pub data: String,
}

/// Incremental parser: feed it bytes as they arrive, get back the events
/// completed so far. Handles `\n` and `\r\n` line endings, comment lines,
/// events split across chunks, and multi-byte characters split across chunks
/// (a line is only decoded once it is complete).
#[derive(Debug, Default)]
pub struct SseParser {
    pending: Vec<u8>,
    event: Option<String>,
    data: Vec<String>,
}

impl SseParser {
    pub fn feed(&mut self, bytes: &[u8]) -> Vec<SseEvent> {
        self.pending.extend_from_slice(bytes);
        let mut events = Vec::new();
        while let Some(pos) = self.pending.iter().position(|&b| b == b'\n') {
            let line: Vec<u8> = self.pending.drain(..=pos).collect();
            let line = String::from_utf8_lossy(&line);
            let line = line.trim_end_matches(['\n', '\r']);
            if let Some(event) = self.line(line) {
                events.push(event);
            }
        }
        events
    }

    fn line(&mut self, line: &str) -> Option<SseEvent> {
        if line.is_empty() {
            // A blank line dispatches the event, if it carried any data.
            let name = self.event.take();
            if self.data.is_empty() {
                return None;
            }
            return Some(SseEvent {
                event: name.unwrap_or_else(|| "message".to_string()),
                data: std::mem::take(&mut self.data).join("\n"),
            });
        }
        if line.starts_with(':') {
            return None; // comment / keep-alive
        }
        let (field, value) = match line.split_once(':') {
            Some((f, v)) => (f, v.strip_prefix(' ').unwrap_or(v)),
            None => (line, ""),
        };
        match field {
            "event" => self.event = Some(value.to_string()),
            "data" => self.data.push(value.to_string()),
            _ => {} // id, retry: not needed here
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn all(chunks: &[&[u8]]) -> Vec<SseEvent> {
        let mut p = SseParser::default();
        chunks.iter().flat_map(|c| p.feed(c)).collect()
    }

    #[test]
    fn parses_named_and_default_events() {
        let events = all(&[b"event: endpoint\ndata: /messages?s=1\n\ndata: {\"a\":1}\n\n"]);
        assert_eq!(
            events,
            vec![
                SseEvent {
                    event: "endpoint".into(),
                    data: "/messages?s=1".into()
                },
                SseEvent {
                    event: "message".into(),
                    data: "{\"a\":1}".into()
                },
            ]
        );
    }

    #[test]
    fn joins_multiple_data_lines_and_skips_comments() {
        let events = all(&[b": keep-alive\ndata: one\ndata: two\n\n"]);
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].data, "one\ntwo");
    }

    #[test]
    fn handles_crlf_and_events_split_anywhere() {
        let whole = b"event: message\r\ndata: hello\r\n\r\n";
        for split in 1..whole.len() {
            let events = all(&[&whole[..split], &whole[split..]]);
            assert_eq!(events.len(), 1, "split at {split}");
            assert_eq!(events[0].data, "hello");
        }
    }

    #[test]
    fn multibyte_characters_survive_chunk_boundaries() {
        let whole = "data: héllo 日本\n\n".as_bytes();
        for split in 1..whole.len() {
            let events = all(&[&whole[..split], &whole[split..]]);
            assert_eq!(events[0].data, "héllo 日本", "split at {split}");
        }
    }

    #[test]
    fn blank_lines_without_data_dispatch_nothing() {
        assert!(all(&[b"\n\nevent: x\n\n"]).is_empty());
        // An event name does not leak into the next event.
        let events = all(&[b"event: a\n\ndata: b\n\n"]);
        assert_eq!(events[0].event, "message");
    }
}
