//! An incremental Server-Sent Events parser: feed bytes as they arrive, get complete events.

/// One event: its `event:` name (if any) and the joined `data:` lines.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SseEvent {
    pub event: Option<String>,
    pub data: String,
}

/// Cap on one event's size; a server streaming an endless line can't exhaust memory.
pub const MAX_EVENT_BYTES: usize = 4 * 1024 * 1024;

#[derive(Debug, Default)]
pub struct SseParser {
    buffer: Vec<u8>,
    event: Option<String>,
    data: Vec<String>,
}

impl SseParser {
    pub fn new() -> Self {
        Self::default()
    }

    /// Adds bytes; returns the events they complete. Errors when one event outgrows the cap.
    pub fn feed(&mut self, bytes: &[u8]) -> Result<Vec<SseEvent>, String> {
        self.buffer.extend_from_slice(bytes);
        let mut out = Vec::new();
        while let Some(pos) = self.buffer.iter().position(|b| *b == b'\n') {
            let line: Vec<u8> = self.buffer.drain(..=pos).collect();
            let line = String::from_utf8_lossy(&line);
            let line = line.trim_end_matches(['\n', '\r']);
            if line.is_empty() {
                if !self.data.is_empty() || self.event.is_some() {
                    out.push(SseEvent { event: self.event.take(), data: std::mem::take(&mut self.data).join("\n") });
                }
                continue;
            }
            if line.starts_with(':') {
                continue; // comment / keep-alive
            }
            let (field, value) = line.split_once(':').unwrap_or((line, ""));
            let value = value.strip_prefix(' ').unwrap_or(value);
            match field {
                "event" => self.event = Some(value.to_owned()),
                "data" => self.data.push(value.to_owned()),
                _ => {}
            }
        }
        let pending = self.buffer.len().saturating_add(self.data.iter().map(String::len).sum::<usize>());
        if pending > MAX_EVENT_BYTES {
            return Err(format!("a streamed event is larger than {MAX_EVENT_BYTES} bytes"));
        }
        Ok(out)
    }

    /// Flushes an event the stream ended without a blank line after.
    pub fn finish(&mut self) -> Option<SseEvent> {
        if !self.buffer.is_empty() {
            let _ = self.feed(b"\n");
        }
        (!self.data.is_empty()).then(|| SseEvent { event: self.event.take(), data: std::mem::take(&mut self.data).join("\n") })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_events_split_anywhere() {
        let stream = b"event: message_start\ndata: {\"a\":1}\n\n: ping\n\ndata: x\r\ndata: y\r\n\r\ndata: [DONE]\n\n";
        for split in 0..stream.len() {
            let mut p = SseParser::new();
            let mut events = p.feed(&stream[..split]).unwrap();
            events.extend(p.feed(&stream[split..]).unwrap());
            assert_eq!(
                events,
                vec![
                    SseEvent { event: Some("message_start".into()), data: "{\"a\":1}".into() },
                    SseEvent { event: None, data: "x\ny".into() },
                    SseEvent { event: None, data: "[DONE]".into() },
                ],
                "split at {split}"
            );
        }
    }

    #[test]
    fn unterminated_tail_and_caps() {
        let mut p = SseParser::new();
        assert!(p.feed(b"data: last").unwrap().is_empty());
        assert_eq!(p.finish(), Some(SseEvent { event: None, data: "last".into() }));
        let mut p = SseParser::new();
        assert!(p.feed(&vec![b'x'; MAX_EVENT_BYTES + 1]).is_err());
    }
}
