//! A small, bounded HTTP/1.1 server side: one request per connection, `Content-Length` bodies
//! only, every size and wait capped. Nothing a client sends can make it allocate past
//! [`Limits`], wait past its deadline or panic.
use std::io::{BufRead, BufReader, Read, Write};
use std::time::{Duration, Instant};

/// What one request may be.
#[derive(Clone, Copy, Debug)]
pub struct Limits {
    /// The request line and headers together.
    pub head_bytes: usize,
    pub headers: usize,
    pub body_bytes: usize,
    /// For the request line and headers, from the first byte read.
    pub head_deadline: Duration,
    /// For the whole request, whatever the client's pace.
    pub deadline: Duration,
}

impl Limits {
    pub const DEFAULT: Self =
        Self { head_bytes: 16 * 1024, headers: 64, body_bytes: 64 * 1024 * 1024, head_deadline: Duration::from_secs(10), deadline: Duration::from_secs(60) };
}

#[derive(Debug, PartialEq, Eq)]
pub struct Request {
    pub method: String,
    /// The path, without its query.
    pub path: String,
    /// Header names lowercased, values trimmed.
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

impl Request {
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers.iter().find(|(n, _)| n == name).map(|(_, v)| v.as_str())
    }
}

/// Why a request couldn't be read; each is answered with its status, then the connection closes.
#[derive(Debug, PartialEq, Eq)]
pub struct Refusal {
    pub status: u16,
    pub kind: &'static str,
    pub message: &'static str,
}

impl Refusal {
    pub const UNAUTHORIZED: Self = Self { status: 401, kind: "unauthorized", message: "send the token from the token file as `Authorization: Bearer <token>`" };
}

fn refuse(status: u16, message: &'static str) -> Refusal {
    Refusal { status, kind: "invalid_request", message }
}

/// A read that gives up at `deadline`; the stream's own timeout bounds each wait.
struct Deadline<R> {
    inner: R,
    deadline: Instant,
}

impl<R: Read> Read for Deadline<R> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        if Instant::now() >= self.deadline {
            return Err(std::io::Error::new(std::io::ErrorKind::TimedOut, "request deadline"));
        }
        self.inner.read(buf)
    }
}

/// One line of the head, CRLF or LF ended, counted against `budget`.
fn line<R: BufRead>(reader: &mut R, budget: &mut usize) -> Result<String, Refusal> {
    let mut bytes = Vec::new();
    let limit = u64::try_from(*budget).unwrap_or(u64::MAX).saturating_add(1);
    let read = reader.take(limit).read_until(b'\n', &mut bytes).map_err(|_| refuse(408, "the request wasn't sent in time"))?;
    if read > *budget {
        return Err(refuse(431, "the request's headers are too large"));
    }
    *budget = budget.saturating_sub(read);
    if bytes.last() != Some(&b'\n') {
        return Err(refuse(400, "the request ended early"));
    }
    bytes.pop();
    if bytes.last() == Some(&b'\r') {
        bytes.pop();
    }
    String::from_utf8(bytes).map_err(|_| refuse(400, "the request's head isn't text"))
}

/// Reads one request from `stream`. `admit` sees the request line and headers before any of the
/// body is read, and can refuse it there. `on_continue` is called once if the client waits for a
/// `100 Continue` before sending its body.
pub fn read<R: Read>(
    stream: R,
    limits: Limits,
    admit: impl FnOnce(&Request) -> Result<(), Refusal>,
    mut on_continue: impl FnMut(),
) -> Result<Request, Refusal> {
    let started = Instant::now();
    let mut reader = BufReader::new(Deadline { inner: stream, deadline: started + limits.head_deadline.min(limits.deadline) });
    let mut budget = limits.head_bytes;
    let start = line(&mut reader, &mut budget)?;
    let mut parts = start.split(' ');
    let (Some(method), Some(target), Some(version), None) = (parts.next(), parts.next(), parts.next(), parts.next()) else {
        return Err(refuse(400, "the request line isn't HTTP"));
    };
    if version != "HTTP/1.1" && version != "HTTP/1.0" {
        return Err(refuse(505, "only HTTP/1.1 is served"));
    }
    if method.is_empty() || !method.bytes().all(|b| b.is_ascii_uppercase()) {
        return Err(refuse(400, "the request's method isn't valid"));
    }
    if !target.starts_with('/') {
        return Err(refuse(400, "the request's target must be a path"));
    }
    let path = target.split(['?', '#']).next().unwrap_or_default().to_owned();
    let mut headers = Vec::new();
    loop {
        let header = line(&mut reader, &mut budget)?;
        if header.is_empty() {
            break;
        }
        if headers.len() >= limits.headers {
            return Err(refuse(431, "the request has too many headers"));
        }
        let Some((name, value)) = header.split_once(':') else { return Err(refuse(400, "a header has no value")) };
        if name.is_empty() || !name.bytes().all(|b| b.is_ascii_graphic()) {
            return Err(refuse(400, "a header's name isn't valid"));
        }
        headers.push((name.to_ascii_lowercase(), value.trim().to_owned()));
    }
    let request = Request { method: method.to_owned(), path, headers, body: Vec::new() };
    admit(&request)?;
    reader.get_mut().deadline = started + limits.deadline;
    if request.header("transfer-encoding").is_some() {
        return Err(refuse(501, "send the body with a Content-Length, not chunked"));
    }
    let lengths: Vec<&str> = request.headers.iter().filter(|(n, _)| n == "content-length").map(|(_, v)| v.as_str()).collect();
    let length = match lengths.as_slice() {
        [] => 0,
        // Too many digits for a u64 is simply too large.
        [one] if !one.is_empty() && one.bytes().all(|b| b.is_ascii_digit()) => one.parse::<u64>().unwrap_or(u64::MAX),
        _ => return Err(refuse(400, "the Content-Length isn't valid")),
    };
    let length = usize::try_from(length).ok().filter(|n| *n <= limits.body_bytes).ok_or_else(|| refuse(413, "the request's body is too large"))?;
    if length > 0 && request.header("expect").is_some_and(|v| v.eq_ignore_ascii_case("100-continue")) {
        on_continue();
    }
    let mut body = Vec::new();
    // Read in pieces, so a client claiming a large body must also send it.
    let read = reader.take(u64::try_from(length).unwrap_or(0)).read_to_end(&mut body).map_err(|_| refuse(408, "the request wasn't sent in time"))?;
    if read != length {
        return Err(refuse(400, "the request's body is shorter than its Content-Length"));
    }
    Ok(Request { body, ..request })
}

/// A reply: its status, content type and body.
#[derive(Debug)]
pub struct Response {
    pub status: u16,
    pub content_type: &'static str,
    pub headers: Vec<(&'static str, String)>,
    pub body: Body,
}

/// A reply's body: bytes in hand, or a reader of a known length (a run's file).
pub enum Body {
    Bytes(Vec<u8>),
    Stream(Box<dyn Read + Send>, u64),
}

impl std::fmt::Debug for Body {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Bytes(bytes) => write!(f, "Bytes({})", bytes.len()),
            Self::Stream(_, len) => write!(f, "Stream({len})"),
        }
    }
}

impl Response {
    pub fn json(status: u16, value: &serde_json::Value) -> Self {
        let body = serde_json::to_vec(value).unwrap_or_else(|_| b"{}".to_vec());
        Self { status, content_type: "application/json", headers: Vec::new(), body: Body::Bytes(body) }
    }

    pub fn header(mut self, name: &'static str, value: String) -> Self {
        self.headers.push((name, value));
        self
    }
}

fn reason(status: u16) -> &'static str {
    match status {
        100 => "Continue",
        200 => "OK",
        201 => "Created",
        202 => "Accepted",
        400 => "Bad Request",
        401 => "Unauthorized",
        402 => "Payment Required",
        404 => "Not Found",
        405 => "Method Not Allowed",
        408 => "Request Timeout",
        409 => "Conflict",
        410 => "Gone",
        411 => "Length Required",
        413 => "Content Too Large",
        415 => "Unsupported Media Type",
        422 => "Unprocessable Content",
        429 => "Too Many Requests",
        431 => "Request Header Fields Too Large",
        500 => "Internal Server Error",
        501 => "Not Implemented",
        503 => "Service Unavailable",
        505 => "HTTP Version Not Supported",
        _ => "Status",
    }
}

/// Writes `response`, then the connection is closed by the caller.
pub fn write<W: Write>(out: &mut W, response: Response) -> std::io::Result<()> {
    let length = match &response.body {
        Body::Bytes(bytes) => u64::try_from(bytes.len()).unwrap_or(u64::MAX),
        Body::Stream(_, len) => *len,
    };
    let out = &mut WriteBy { out, by: Instant::now().checked_add(allowed(length)) };
    let mut head = format!(
        "HTTP/1.1 {} {}\r\nContent-Type: {}\r\nContent-Length: {length}\r\nConnection: close\r\nCache-Control: no-store\r\nX-Content-Type-Options: nosniff\r\n",
        response.status,
        reason(response.status),
        response.content_type
    );
    for (name, value) in &response.headers {
        // Values are the server's own; a stray line break is dropped rather than sent.
        let value: String = value.chars().filter(|c| !c.is_control()).collect();
        head.push_str(&format!("{name}: {value}\r\n"));
    }
    head.push_str("\r\n");
    out.write_all(head.as_bytes())?;
    match response.body {
        Body::Bytes(bytes) => out.write_all(&bytes)?,
        Body::Stream(reader, len) => {
            let copied = std::io::copy(&mut reader.take(len), out)?;
            if copied != len {
                return Err(std::io::Error::new(std::io::ErrorKind::UnexpectedEof, "a run's file ended early"));
            }
        }
    }
    out.flush()
}

/// The slowest a reader may take a response before the connection is dropped.
const SLOWEST_READ: u64 = 256 * 1024;
/// Time any response is given, however small.
const WRITE_GRACE: Duration = Duration::from_secs(30);

/// How long a response of `length` bytes may take to send: a reader as slow as
/// [`SLOWEST_READ`] bytes a second still gets it, but nobody holds a connection forever.
fn allowed(length: u64) -> Duration {
    WRITE_GRACE.saturating_add(Duration::from_secs(length / SLOWEST_READ))
}

/// A writer that refuses to write once `by` has passed (the per-write timeout alone lets a
/// steady trickle hold a connection for as long as the file lasts).
struct WriteBy<'a, W> {
    out: &'a mut W,
    by: Option<Instant>,
}

impl<W: Write> WriteBy<'_, W> {
    fn check(&self) -> std::io::Result<()> {
        match self.by {
            Some(by) if Instant::now() > by => Err(std::io::Error::new(std::io::ErrorKind::TimedOut, "the response took too long to send")),
            _ => Ok(()),
        }
    }
}

impl<W: Write> Write for WriteBy<'_, W> {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.check()?;
        self.out.write(buf)
    }
    fn flush(&mut self) -> std::io::Result<()> {
        self.check()?;
        self.out.flush()
    }
}

/// The interim reply a client sent `Expect: 100-continue` waits for.
pub fn write_continue<W: Write>(out: &mut W) -> std::io::Result<()> {
    out.write_all(b"HTTP/1.1 100 Continue\r\n\r\n")?;
    out.flush()
}

/// A refusal as its JSON reply.
pub fn refusal(refusal: &Refusal) -> Response {
    let response = Response::json(refusal.status, &serde_json::json!({"error": {"kind": refusal.kind, "message": refusal.message}}));
    if refusal.status == 401 { response.header("WWW-Authenticate", "Bearer".into()) } else { response }
}

#[cfg(test)]
mod deadline_tests {
    use super::*;

    #[test]
    fn a_response_is_given_time_by_its_length_and_no_more() {
        assert_eq!(allowed(0), WRITE_GRACE);
        assert_eq!(allowed(SLOWEST_READ * 100), WRITE_GRACE + Duration::from_secs(100));
        assert!(allowed(u64::MAX) > WRITE_GRACE, "no overflow");
        let mut sink = Vec::new();
        let mut late = WriteBy { out: &mut sink, by: Instant::now().checked_sub(Duration::from_secs(1)) };
        assert_eq!(late.write(b"x").unwrap_err().kind(), std::io::ErrorKind::TimedOut);
        let mut open = WriteBy { out: &mut sink, by: Instant::now().checked_add(Duration::from_secs(60)) };
        assert_eq!(open.write(b"x").unwrap(), 1);
    }
}
