//! A non-payload-copy JSON admission pass before serde allocates escaped strings or vectors.
use super::*;
#[derive(Clone, Copy)]
enum Place {
    Root,
    Steps,
    Step,
    Contents,
    Content,
    Summary,
    Usage,
    Other,
}
struct Scan<'a> {
    text: &'a str,
    at: usize,
    members: usize,
    values: usize,
    strings: usize,
    metadata: usize,
    encoded: usize,
    steps: usize,
    contents: usize,
    l: &'a Limits,
}
impl<'a> Scan<'a> {
    fn byte(&self) -> Option<u8> {
        self.text.as_bytes().get(self.at).copied()
    }
    fn ws(&mut self) {
        while self.byte().is_some_and(|b| b.is_ascii_whitespace()) {
            self.at += 1;
        }
    }
    fn token(&mut self, b: u8) -> Result<(), ProviderError> {
        self.ws();
        if self.byte() != Some(b) {
            return Err(malformed("invalid Gemini JSON structure"));
        }
        self.at += 1;
        Ok(())
    }
    fn string(&mut self, blob: bool) -> Result<&'a str, ProviderError> {
        let start = self.at;
        self.token(b'"')?;
        let content = self.at;
        let mut escaped = false;
        loop {
            match self.byte() {
                Some(b'"') if !escaped => break,
                Some(b'\\') if !escaped => {
                    escaped = true;
                    self.at += 1;
                }
                Some(_) => {
                    escaped = false;
                    self.at += 1;
                }
                None => return Err(malformed("unterminated Gemini JSON string")),
            }
        }
        let len = self.at.saturating_sub(content);
        self.at += 1;
        add(&mut self.strings, 1, self.l.strings)?;
        if blob {
            if len > super::response::encoded_limit(self.l.output_bytes)? {
                return Err(malformed("Gemini encoded image exceeds limit"));
            }
            add(&mut self.encoded, len, self.l.encoded_bytes)?;
        } else {
            add(&mut self.metadata, len, self.l.metadata_bytes)?;
        }
        self.text.get(start..self.at).ok_or_else(|| malformed("invalid Gemini string boundary"))
    }
    fn value(&mut self, depth: usize, place: Place, blob: bool) -> Result<(), ProviderError> {
        if depth > self.l.depth {
            return Err(malformed("Gemini JSON depth exceeded"));
        }
        add(&mut self.values, 1, self.l.values)?;
        self.ws();
        match self.byte() {
            Some(b'{') => {
                self.at += 1;
                self.ws();
                let mut keys: Vec<String> = Vec::new();
                if self.byte() == Some(b'}') {
                    self.at += 1;
                    return Ok(());
                }
                loop {
                    self.ws();
                    let key = self.string(false)?;
                    let key: String = serde_json::from_str(key).map_err(|_| malformed("invalid Gemini JSON key"))?;
                    add(&mut self.members, 1, self.l.members)?;
                    if !matches!(place, Place::Usage) && keys.contains(&key) {
                        return Err(malformed("duplicate Gemini JSON field"));
                    }
                    let child = match (place, key.as_str()) {
                        (Place::Root, "steps") => Place::Steps,
                        (Place::Root, "usage") | (Place::Usage, _) => Place::Usage,
                        (Place::Step, "content") => Place::Contents,
                        (Place::Step, "summary") => Place::Summary,
                        _ => Place::Other,
                    };
                    let payload = matches!(place, Place::Content) && key == "data";
                    self.token(b':')?;
                    self.value(depth + 1, child, payload)?;
                    reserve(&mut keys, 1)?;
                    keys.push(key);
                    self.ws();
                    match self.byte() {
                        Some(b',') => self.at += 1,
                        Some(b'}') => {
                            self.at += 1;
                            break;
                        }
                        _ => return Err(malformed("invalid Gemini JSON object")),
                    }
                }
            }
            Some(b'[') => {
                self.at += 1;
                self.ws();
                if self.byte() == Some(b']') {
                    self.at += 1;
                    return Ok(());
                }
                let child = match place {
                    Place::Steps => Place::Step,
                    Place::Contents | Place::Summary => Place::Content,
                    Place::Usage => Place::Usage,
                    _ => Place::Other,
                };
                loop {
                    match place {
                        Place::Steps => add(&mut self.steps, 1, self.l.steps)?,
                        Place::Contents | Place::Summary => add(&mut self.contents, 1, self.l.contents)?,
                        _ => {}
                    }
                    self.value(depth + 1, child, false)?;
                    self.ws();
                    match self.byte() {
                        Some(b',') => self.at += 1,
                        Some(b']') => {
                            self.at += 1;
                            break;
                        }
                        _ => return Err(malformed("invalid Gemini JSON array")),
                    }
                }
            }
            Some(b'"') => {
                self.string(blob)?;
            }
            Some(_) => {
                let start = self.at;
                while self.byte().is_some_and(|b| !matches!(b, b',' | b'}' | b']') && !b.is_ascii_whitespace()) {
                    self.at += 1;
                }
                if self.at == start {
                    return Err(malformed("missing Gemini JSON scalar"));
                }
                add(&mut self.metadata, self.at - start, self.l.metadata_bytes)?;
            }
            None => return Err(malformed("missing Gemini JSON value")),
        }
        Ok(())
    }
}
pub(super) fn admit<'a>(bytes: &'a [u8], l: &Limits) -> Result<&'a str, ProviderError> {
    if bytes.len() > l.response_bytes {
        return Err(malformed("Gemini response exceeds its byte limit"));
    }
    let text = std::str::from_utf8(bytes).map_err(|_| malformed("Gemini response is not UTF8"))?;
    let mut scan = Scan { text, at: 0, members: 0, values: 0, strings: 0, metadata: 0, encoded: 0, steps: 0, contents: 0, l };
    scan.value(1, Place::Root, false)?;
    scan.ws();
    if scan.at != text.len() {
        return Err(malformed("trailing Gemini JSON"));
    }
    // IgnoredAny validates primitive spelling and escapes without retaining the payload.
    let mut de = serde_json::Deserializer::from_str(text);
    serde::Deserialize::deserialize(&mut de).map(|_: serde::de::IgnoredAny| ()).map_err(|_| malformed("invalid Gemini JSON syntax"))?;
    de.end().map_err(|_| malformed("trailing Gemini JSON"))?;
    Ok(text)
}
