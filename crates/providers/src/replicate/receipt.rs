//! Complete bounded lifecycle receipts, never output/identity/billing admission.
//! Ignored strings are checked without unescaping or allocating a JSON tree.

pub(super) struct Receipt<'a> {
    pub id: &'a str,
    pub terminal: bool,
}
struct Scan<'a> {
    text: &'a str,
    at: usize,
    id: Option<&'a str>,
    status_seen: bool,
    status_duplicate: bool,
    terminal: bool,
}
impl<'a> Scan<'a> {
    fn byte(&self) -> Option<u8> {
        self.text.as_bytes().get(self.at).copied()
    }
    fn advance(&mut self) {
        self.at = self.at.saturating_add(1);
    }
    fn ws(&mut self) {
        while self.byte().is_some_and(|b| matches!(b, b' ' | b'\t' | b'\r' | b'\n')) {
            self.advance();
        }
    }
    fn need(&mut self, byte: u8) -> Result<(), ()> {
        self.ws();
        if self.byte() != Some(byte) {
            return Err(());
        }
        self.advance();
        Ok(())
    }
    fn hex(&mut self) -> Result<u16, ()> {
        let mut n = 0u16;
        for _ in 0..4 {
            let digit = match self.byte().ok_or(())? {
                b'0'..=b'9' => self.byte().ok_or(())?.saturating_sub(b'0'),
                b'a'..=b'f' => self.byte().ok_or(())?.saturating_sub(b'a').saturating_add(10),
                b'A'..=b'F' => self.byte().ok_or(())?.saturating_sub(b'A').saturating_add(10),
                _ => return Err(()),
            };
            n = n.checked_mul(16).and_then(|n| n.checked_add(u16::from(digit))).ok_or(())?;
            self.advance();
        }
        Ok(n)
    }
    fn string(&mut self, key: bool) -> Result<&'a str, ()> {
        self.need(b'"')?;
        let start = self.at;
        loop {
            let byte = self.byte().ok_or(())?;
            if byte == b'"' {
                let end = self.at;
                self.advance();
                return self.text.get(start..end).ok_or(());
            }
            if byte < 32 {
                return Err(());
            }
            self.advance();
            if byte == b'\\' {
                if key {
                    return Err(());
                }
                let escaped = self.byte().ok_or(())?;
                self.advance();
                match escaped {
                    b'"' | b'\\' | b'/' | b'b' | b'f' | b'n' | b'r' | b't' => {}
                    b'u' => {
                        let high = self.hex()?;
                        if (0xd800..=0xdbff).contains(&high) {
                            // Surrogate pairs must be adjacent, not separated by JSON whitespace.
                            if self.byte() != Some(b'\\') {
                                return Err(());
                            }
                            self.advance();
                            if self.byte() != Some(b'u') {
                                return Err(());
                            }
                            self.advance();
                            if !(0xdc00..=0xdfff).contains(&self.hex()?) {
                                return Err(());
                            }
                        } else if (0xdc00..=0xdfff).contains(&high) {
                            return Err(());
                        }
                    }
                    _ => return Err(()),
                }
            }
            if key && self.at.saturating_sub(start) > 256 {
                return Err(());
            }
        }
    }
    fn literal(&mut self, value: &str) -> Result<(), ()> {
        let end = self.at.checked_add(value.len()).ok_or(())?;
        if self.text.get(self.at..end) != Some(value) {
            return Err(());
        }
        self.at = end;
        Ok(())
    }
    fn digits(&mut self) -> Result<(), ()> {
        let start = self.at;
        while self.byte().is_some_and(|b| b.is_ascii_digit()) {
            self.advance();
        }
        if self.at == start {
            return Err(());
        }
        Ok(())
    }
    fn number(&mut self) -> Result<(), ()> {
        let start = self.at;
        if self.byte() == Some(b'-') {
            self.advance();
        }
        match self.byte() {
            Some(b'0') => self.advance(),
            Some(b'1'..=b'9') => self.digits()?,
            _ => return Err(()),
        }
        if self.byte() == Some(b'.') {
            self.advance();
            self.digits()?;
        }
        if matches!(self.byte(), Some(b'e' | b'E')) {
            self.advance();
            if matches!(self.byte(), Some(b'+' | b'-')) {
                self.advance();
            }
            self.digits()?;
        }
        if self.at.saturating_sub(start) > 64 {
            return Err(());
        }
        Ok(())
    }
    fn value(&mut self, depth: usize) -> Result<(), ()> {
        if depth > 16 {
            return Err(());
        }
        self.ws();
        match self.byte() {
            Some(b'{') => self.object(depth),
            Some(b'[') => {
                self.advance();
                self.ws();
                if self.byte() == Some(b']') {
                    self.advance();
                    return Ok(());
                }
                loop {
                    self.value(depth.saturating_add(1))?;
                    self.ws();
                    match self.byte() {
                        Some(b']') => {
                            self.advance();
                            return Ok(());
                        }
                        Some(b',') => self.advance(),
                        _ => return Err(()),
                    }
                }
            }
            Some(b'"') => self.string(false).map(|_| ()),
            Some(b'n') => self.literal("null"),
            Some(b't') => self.literal("true"),
            Some(b'f') => self.literal("false"),
            Some(b'-' | b'0'..=b'9') => self.number(),
            _ => Err(()),
        }
    }
    fn object(&mut self, depth: usize) -> Result<(), ()> {
        self.need(b'{')?;
        self.ws();
        if self.byte() == Some(b'}') {
            self.advance();
            return Ok(());
        }
        loop {
            let key = self.string(true)?;
            self.need(b':')?;
            self.ws();
            if depth == 1 && key == "id" {
                if self.id.is_some() {
                    return Err(());
                }
                let id = self.string(false)?;
                if id.is_empty() || id.len() > 128 || !id.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_')) {
                    return Err(());
                }
                self.id = Some(id);
            } else if depth == 1 && key == "status" {
                self.status_duplicate |= self.status_seen;
                self.status_seen = true;
                if self.byte() == Some(b'"') {
                    self.terminal = matches!(self.string(false)?, "succeeded" | "failed" | "canceled" | "aborted");
                } else {
                    self.value(depth.saturating_add(1))?;
                    self.terminal = false;
                }
            } else {
                self.value(depth.saturating_add(1))?;
            }
            self.ws();
            match self.byte() {
                Some(b'}') => {
                    self.advance();
                    return Ok(());
                }
                Some(b',') => self.advance(),
                _ => return Err(()),
            }
        }
    }
}
pub(super) fn parse(bytes: &[u8], cap: usize) -> Option<Receipt<'_>> {
    if bytes.len() > cap {
        return None;
    }
    let text = std::str::from_utf8(bytes).ok()?;
    let mut scan = Scan { text, at: 0, id: None, status_seen: false, status_duplicate: false, terminal: false };
    scan.ws();
    if scan.byte() != Some(b'{') {
        return None;
    }
    scan.value(1).ok()?;
    scan.ws();
    if scan.at != text.len() {
        return None;
    }
    Some(Receipt { id: scan.id?, terminal: scan.terminal && !scan.status_duplicate })
}

#[cfg(test)]
mod tests;
