//! A streamed reply's server-sent events, checked one whole event at a time before the chat
//! client reads them: what a dialect must refuse before the crate sizes anything by it, and what
//! it adds where its servers leave something out.

use crate::{ErrorKind, ProviderError};

/// Looks at one event's joined `data:` text: refuses it, or returns a whole event (with its
/// blank line) to read just before it.
pub(crate) type Guard = Box<dyn FnMut(&str) -> Result<Option<String>, ProviderError> + Send>;

/// Cap on one event's size; a server streaming an endless line can't exhaust memory.
pub(crate) const MAX_EVENT_BYTES: usize = 4 * 1024 * 1024;

/// The optional byte-order mark a stream may open with, which the grammar ignores.
const BOM: [u8; 3] = [0xef, 0xbb, 0xbf];

/// Cuts a byte stream into events by the `text/event-stream` grammar exactly as the chat client
/// reads it (a line ends at CR, LF or CRLF; a leading BOM is dropped; a field is the text before
/// the first colon, its value after it less one space), runs each past a [`Guard`], and passes on
/// only what it checked: each event written again as its `event:` and `data:` lines, LF-ended,
/// then a blank line. Comments and other fields, which the client doesn't read, are left out.
pub(crate) struct Events {
    guard: Guard,
    /// The line being read.
    line: Vec<u8>,
    /// The last byte read ended a line at a CR: an LF right after it belongs to that line end.
    after_cr: bool,
    /// How many BOM bytes the stream opened with so far, until it is decided.
    bom: usize,
    bom_done: bool,
    /// The event so far: its type and its data lines, joined.
    event_type: String,
    data: Option<String>,
    /// Bytes of the event so far, for its size cap.
    size: usize,
}

impl Events {
    pub(crate) fn new(guard: Guard) -> Self {
        Self { guard, line: Vec::new(), after_cr: false, bom: 0, bom_done: false, event_type: String::new(), data: None, size: 0 }
    }

    /// The whole events `chunk` completes, as bytes to read.
    pub(crate) fn push(&mut self, chunk: &[u8]) -> Result<Vec<u8>, ProviderError> {
        let mut out = Vec::new();
        let mut rest = self.strip_bom(chunk);
        if self.after_cr && !rest.is_empty() {
            self.after_cr = false;
            if let Some(tail) = rest.strip_prefix(b"\n") {
                rest = tail;
            }
        }
        while let Some(at) = memchr::memchr2(b'\n', b'\r', rest) {
            let (line, tail) = rest.split_at(at);
            self.line.extend_from_slice(line);
            rest = match tail.strip_prefix(b"\r\n") {
                Some(after) => after,
                None => {
                    self.after_cr = tail.first() == Some(&b'\r') && tail.len() == 1;
                    tail.get(1..).unwrap_or_default()
                }
            };
            self.end_line(&mut out)?;
        }
        self.line.extend_from_slice(rest);
        self.check_size()?;
        Ok(out)
    }

    /// What the stream ended with: a last event left without its blank line still counts.
    pub(crate) fn finish(&mut self) -> Result<Vec<u8>, ProviderError> {
        let mut out = Vec::new();
        if self.bom > 0 && !self.bom_done {
            // A stream of only part of a BOM: those bytes are text.
            self.line.extend_from_slice(BOM.get(..self.bom).unwrap_or_default());
            self.bom_done = true;
        }
        if !self.line.is_empty() {
            self.end_line(&mut out)?;
        }
        self.end_event(&mut out)?;
        Ok(out)
    }

    /// `chunk` without the stream's leading BOM, which may itself be split across chunks.
    fn strip_bom<'a>(&mut self, chunk: &'a [u8]) -> &'a [u8] {
        let mut rest = chunk;
        while !self.bom_done {
            let Some(&byte) = rest.first() else { return rest };
            if BOM.get(self.bom) != Some(&byte) {
                // Not a BOM after all: the bytes that matched so far are text.
                self.line.extend_from_slice(BOM.get(..self.bom).unwrap_or_default());
                self.bom = 0;
                self.bom_done = true;
                return rest;
            }
            self.bom += 1;
            rest = rest.get(1..).unwrap_or_default();
            if self.bom == BOM.len() {
                self.bom = 0;
                self.bom_done = true;
            }
        }
        rest
    }

    fn check_size(&self) -> Result<(), ProviderError> {
        if self.size.saturating_add(self.line.len()) > MAX_EVENT_BYTES {
            return Err(ProviderError::new(ErrorKind::Malformed, format!("a streamed event is larger than {MAX_EVENT_BYTES} bytes")));
        }
        Ok(())
    }

    fn end_line(&mut self, out: &mut Vec<u8>) -> Result<(), ProviderError> {
        let line = std::mem::take(&mut self.line);
        if line.is_empty() {
            return self.end_event(out);
        }
        self.size = self.size.saturating_add(line.len());
        self.check_size()?;
        let line = String::from_utf8_lossy(&line);
        // A line starting with a colon is a comment.
        if line.starts_with(':') {
            return Ok(());
        }
        let (field, value) = match line.split_once(':') {
            Some((field, value)) => (field, value.strip_prefix(' ').unwrap_or(value)),
            None => (line.as_ref(), ""),
        };
        match field {
            "event" => value.clone_into(&mut self.event_type),
            "data" => match &mut self.data {
                Some(data) => {
                    data.push('\n');
                    data.push_str(value);
                }
                None => self.data = Some(value.to_owned()),
            },
            _ => {}
        }
        Ok(())
    }

    /// A blank line ends an event; one with no data only resets its type, as the grammar says.
    fn end_event(&mut self, out: &mut Vec<u8>) -> Result<(), ProviderError> {
        self.size = 0;
        let event_type = std::mem::take(&mut self.event_type);
        let Some(data) = self.data.take() else { return Ok(()) };
        if let Some(before) = (self.guard)(&data)? {
            out.extend_from_slice(before.as_bytes());
        }
        if !event_type.is_empty() {
            out.extend_from_slice(format!("event: {event_type}\n").as_bytes());
        }
        for line in data.split('\n') {
            out.extend_from_slice(b"data: ");
            out.extend_from_slice(line.as_bytes());
            out.push(b'\n');
        }
        out.push(b'\n');
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pass() -> Guard {
        Box::new(|_| Ok(None))
    }

    /// Everything the guard sees, split anywhere: the client must read exactly these events.
    fn read(stream: &[u8], split: usize) -> (String, Vec<String>) {
        let seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let saw = std::sync::Arc::clone(&seen);
        let mut e = Events::new(Box::new(move |data| {
            saw.lock().unwrap().push(data.to_owned());
            Ok(None)
        }));
        let (a, b) = stream.split_at(split.min(stream.len()));
        let mut out = e.push(a).unwrap();
        out.extend(e.push(b).unwrap());
        out.extend(e.finish().unwrap());
        let seen = seen.lock().unwrap().clone();
        (String::from_utf8(out).unwrap(), seen)
    }

    #[test]
    fn events_are_read_by_the_grammar_whatever_the_split() {
        let stream = "\u{feff}event: a\ndata: {\"x\":1}\n\n: ping\n\ndata: x\r\ndata:y\r\rdata\r\n\r\nid: 7\nretry: 5\ndata: last".as_bytes();
        for split in 0..=stream.len() {
            let (out, seen) = read(stream, split);
            assert_eq!(out, "event: a\ndata: {\"x\":1}\n\ndata: x\ndata: y\n\ndata: \n\ndata: last\n\n", "split at {split}");
            assert_eq!(seen, ["{\"x\":1}", "x\ny", "", "last"], "split at {split}");
        }
    }

    /// A lone CR ends a line, and two of them an event, as the client reads them: what the
    /// guard checks is what is read.
    #[test]
    fn lone_carriage_returns_end_lines_and_events() {
        let (out, seen) = read(b"data: a\r\rdata: b\r\r\n\n", 3);
        assert_eq!(seen, ["a", "b"]);
        assert_eq!(out, "data: a\n\ndata: b\n\n");
    }

    #[test]
    fn only_a_leading_bom_is_dropped() {
        assert_eq!(read("\u{feff}data: a\n\n".as_bytes(), 1).1, ["a"]);
        assert_eq!(read("data: a\u{feff}\n\n".as_bytes(), 1).1, ["a\u{feff}"]);
        assert!(read(b"\xef\xbbdata: a\n\n", 1).1.is_empty(), "not a whole BOM: its bytes are part of the field name, so no data line");
    }
    #[test]
    fn a_guard_sees_whole_data_refuses_or_adds_before() {
        let mut seen = Vec::new();
        let mut e = Events::new(Box::new(move |data| {
            seen.push(data.to_owned());
            match data {
                "bad" => Err(ProviderError::new(ErrorKind::Malformed, "bad")),
                "stop" => Ok(Some("data: added\n\n".into())),
                _ => Ok(None),
            }
        }));
        assert_eq!(e.push(b"data: a\ndata: b\n\ndata: stop\n\n").unwrap(), b"data: a\ndata: b\n\ndata: added\n\ndata: stop\n\n".as_slice());
        assert_eq!(e.push(b"data: ba").unwrap(), b"".as_slice());
        assert!(e.push(b"d\n\n").is_err());
    }

    #[test]
    fn events_are_bounded() {
        let mut e = Events::new(pass());
        assert!(e.push(&vec![b'x'; MAX_EVENT_BYTES + 1]).is_err());
        let mut e = Events::new(pass());
        let line = format!("data: {}\n", "y".repeat(1024));
        let mut failed = false;
        for _ in 0..=MAX_EVENT_BYTES / 1024 {
            if e.push(line.as_bytes()).is_err() {
                failed = true;
                break;
            }
        }
        assert!(failed, "many lines of one event count too");
    }
}
