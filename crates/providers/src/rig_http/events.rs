//! A streamed reply's server-sent events, checked one whole event at a time before the chat
//! client reads them: what a dialect must refuse before the crate sizes anything by it, and what
//! it adds where its servers leave something out.

use crate::{ErrorKind, ProviderError};

/// Looks at one event's joined `data:` text: refuses it, or returns a whole event (with its
/// blank line) to read just before it.
pub(crate) type Guard = Box<dyn FnMut(&str) -> Result<Option<String>, ProviderError> + Send>;

/// Cap on one event's size; a server streaming an endless line can't exhaust memory.
pub(crate) const MAX_EVENT_BYTES: usize = 4 * 1024 * 1024;

/// Cuts a byte stream into events and runs each past a [`Guard`]. Events go on unchanged.
pub(crate) struct Events {
    guard: Guard,
    /// The line being read.
    line: Vec<u8>,
    /// The event so far, its lines as they came.
    event: Vec<u8>,
    /// Its `data:` lines, joined.
    data: Option<String>,
}

impl Events {
    pub(crate) fn new(guard: Guard) -> Self {
        Self { guard, line: Vec::new(), event: Vec::new(), data: None }
    }

    /// The whole events `chunk` completes, as bytes to read.
    pub(crate) fn push(&mut self, chunk: &[u8]) -> Result<Vec<u8>, ProviderError> {
        let mut out = Vec::new();
        let mut rest = chunk;
        while let Some(at) = memchr::memchr(b'\n', rest) {
            let (line, tail) = rest.split_at(at);
            self.line.extend_from_slice(line);
            rest = tail.get(1..).unwrap_or_default();
            self.end_line(&mut out)?;
        }
        self.line.extend_from_slice(rest);
        self.check_size()?;
        Ok(out)
    }

    /// What the stream ended with: a last event left without its blank line still counts.
    pub(crate) fn finish(&mut self) -> Result<Vec<u8>, ProviderError> {
        let mut out = Vec::new();
        if !self.line.is_empty() {
            self.end_line(&mut out)?;
        }
        if !self.event.is_empty() {
            self.end_event(&mut out)?;
        }
        Ok(out)
    }

    fn check_size(&self) -> Result<(), ProviderError> {
        if self.event.len().saturating_add(self.line.len()) > MAX_EVENT_BYTES {
            return Err(ProviderError::new(ErrorKind::Malformed, format!("a streamed event is larger than {MAX_EVENT_BYTES} bytes")));
        }
        Ok(())
    }

    fn end_line(&mut self, out: &mut Vec<u8>) -> Result<(), ProviderError> {
        let mut line = std::mem::take(&mut self.line);
        if line.last() == Some(&b'\r') {
            line.pop();
        }
        if line.is_empty() {
            return self.end_event(out);
        }
        if let Some(value) = line.strip_prefix(b"data:") {
            let value = value.strip_prefix(b" ").unwrap_or(value);
            let value = String::from_utf8_lossy(value);
            match &mut self.data {
                Some(data) => {
                    data.push('\n');
                    data.push_str(&value);
                }
                None => self.data = Some(value.into_owned()),
            }
        }
        self.event.extend_from_slice(&line);
        self.event.push(b'\n');
        self.check_size()
    }

    fn end_event(&mut self, out: &mut Vec<u8>) -> Result<(), ProviderError> {
        if let Some(data) = self.data.take()
            && let Some(before) = (self.guard)(&data)?
        {
            out.extend_from_slice(before.as_bytes());
        }
        out.append(&mut self.event);
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

    #[test]
    fn events_pass_unchanged_whatever_the_split() {
        let stream = b"event: a\ndata: {\"x\":1}\n\n: ping\n\ndata: x\r\ndata: y\r\n\r\ndata: last";
        for split in 0..stream.len() {
            let mut e = Events::new(pass());
            let mut out = e.push(&stream[..split]).unwrap();
            out.extend(e.push(&stream[split..]).unwrap());
            out.extend(e.finish().unwrap());
            assert_eq!(String::from_utf8(out).unwrap(), "event: a\ndata: {\"x\":1}\n\n: ping\n\ndata: x\ndata: y\n\ndata: last\n\n", "split at {split}");
        }
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
        assert_eq!(e.push(b"data: a\ndata: b\n\ndata: stop\n\n").unwrap(), b"data: a\ndata: b\n\ndata: added\n\ndata: stop\n\n");
        assert_eq!(e.push(b"data: ba").unwrap(), b"");
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
