//! A ComfyUI server and the HTTP requests NodeCraft sends it.
//!
//! This module is sans-IO: it builds [`Request`]s and the transport (reqwest on desktop and in the
//! browser, a fixture in tests) sends them. Response parsing lives next to the data it reads.

use std::fmt;

use url::Url;

use crate::ComfyError;

/// Where a ComfyUI server lives and how to authenticate to it.
#[derive(Clone, PartialEq, Eq)]
pub struct Server {
    base: Url,
    auth: Option<(String, String)>,
}

impl fmt::Debug for Server {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // A reverse-proxy path can contain credentials too; retain only the origin.
        f.debug_struct("Server").field("origin", &self.base.origin().ascii_serialization()).field("auth", &self.auth.as_ref().map(|(name, _)| name)).finish()
    }
}

/// HTTP method of a [`Request`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Method {
    Get,
    Post,
}

/// The body of a [`Request`].
#[derive(Clone, PartialEq, Eq)]
pub enum Body {
    Empty,
    Json(Vec<u8>),
    /// `multipart/form-data` with this boundary; the bytes are the encoded form.
    Multipart {
        boundary: String,
        bytes: Vec<u8>,
    },
}

impl fmt::Debug for Body {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Prompts may carry account credentials; uploads may contain private media.
        match self {
            Self::Empty => f.write_str("Empty"),
            Self::Json(bytes) => f.debug_struct("Json").field("byte_len", &bytes.len()).finish(),
            Self::Multipart { bytes, .. } => f.debug_struct("Multipart").field("byte_len", &bytes.len()).finish(),
        }
    }
}

/// An HTTP request ready to send.
///
/// Transports must not follow redirects to another origin with these headers: reqwest strips
/// `Authorization` on cross-host redirects but not custom names such as `X-API-Key`, so disable
/// redirects or restrict them to the same origin.
#[derive(Clone, PartialEq, Eq)]
pub struct Request {
    pub method: Method,
    pub url: Url,
    /// Extra headers, the auth header included. `Content-Type` follows from the body.
    pub headers: Vec<(String, String)>,
    pub body: Body,
}

impl fmt::Debug for Request {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let names: Vec<&str> = self.headers.iter().map(|(n, _)| n.as_str()).collect();
        // Public Requests can bypass Server's URL validation. Even paths may contain
        // credentials, so diagnostics retain only the origin (scheme, host and port).
        let origin = self.url.origin().ascii_serialization();
        f.debug_struct("Request").field("method", &self.method).field("origin", &origin).field("headers", &names).field("body", &self.body).finish()
    }
}

impl Request {
    /// The `Content-Type` header value the body needs, if any.
    pub fn content_type(&self) -> Option<String> {
        match &self.body {
            Body::Empty => None,
            Body::Json(_) => Some("application/json".into()),
            Body::Multipart { boundary, .. } => Some(format!("multipart/form-data; boundary={boundary}")),
        }
    }
}

/// Which folder an upload goes to on the server.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UploadKind {
    /// The input folder that `LoadImage` reads from.
    Input,
    Temp,
}

impl UploadKind {
    fn as_str(self) -> &'static str {
        match self {
            Self::Input => "input",
            Self::Temp => "temp",
        }
    }
}

/// A file the server produced or holds, as named in `executed` messages and `/history`.
#[derive(Debug, Clone, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub struct FileRef {
    pub filename: String,
    #[serde(default)]
    pub subfolder: String,
    /// `output`, `temp` or `input`.
    #[serde(rename = "type", default = "default_file_kind")]
    pub kind: String,
}

fn default_file_kind() -> String {
    "output".into()
}

impl Server {
    /// A server at `url` (such as `http://127.0.0.1:8188`, or behind a path prefix).
    pub fn new(url: &str) -> Result<Self, ComfyError> {
        let mut base = Url::parse(url.trim()).map_err(|e| ComfyError::InvalidUrl(e.to_string()))?;
        if !matches!(base.scheme(), "http" | "https") {
            // Don't echo the input: a URL with credentials would put them in the error.
            return Err(ComfyError::InvalidUrl("it must start with http:// or https://".into()));
        }
        if base.host_str().is_none_or(str::is_empty) {
            return Err(ComfyError::InvalidUrl("no host".into()));
        }
        if !base.username().is_empty() || base.password().is_some() {
            return Err(ComfyError::InvalidUrl("put credentials in the auth header, not the URL".into()));
        }
        base.set_query(None);
        base.set_fragment(None);
        if !base.path().ends_with('/') {
            let path = format!("{}/", base.path());
            base.set_path(&path);
        }
        Ok(Self { base, auth: None })
    }

    /// Sends `name: value` with every request (for servers behind an authenticating proxy).
    /// The value comes from the keychain and never appears in `Debug` output or errors.
    pub fn with_auth_header(mut self, name: &str, value: &str) -> Result<Self, ComfyError> {
        let bad = |s: &str| s.is_empty() || s.chars().any(|c| c.is_control());
        if bad(name) || !name.chars().all(|c| c.is_ascii_alphanumeric() || "-_".contains(c)) || bad(value) {
            return Err(ComfyError::InvalidSetting("the auth header name or value is not valid".into()));
        }
        self.auth = Some((name.into(), value.into()));
        Ok(self)
    }

    /// The base URL, always ending in `/`.
    pub fn base_url(&self) -> &Url {
        &self.base
    }

    fn url(&self, path: &str) -> Result<Url, ComfyError> {
        self.base.join(path).map_err(|e| ComfyError::InvalidUrl(e.to_string()))
    }

    fn request(&self, method: Method, url: Url, body: Body) -> Request {
        let headers = self.auth.iter().cloned().collect();
        Request { method, url, headers, body }
    }

    fn get(&self, path: &str) -> Result<Request, ComfyError> {
        Ok(self.request(Method::Get, self.url(path)?, Body::Empty))
    }

    fn post_json(&self, path: &str, json: &serde_json::Value) -> Result<Request, ComfyError> {
        crate::json_bounds::Budget::default().value(json, 0)?;
        let bytes = serde_json::to_vec(json).map_err(|e| ComfyError::Malformed { what: "request", message: e.to_string() })?;
        Ok(self.request(Method::Post, self.url(path)?, Body::Json(bytes)))
    }

    /// `GET /system_stats`: health, versions, GPUs and VRAM. Parse with [`crate::SystemStats::parse`].
    pub fn system_stats(&self) -> Result<Request, ComfyError> {
        self.get("system_stats")
    }

    /// `GET /object_info`: every node class the server has. Parse with [`crate::NodeCatalog::parse`].
    pub fn object_info(&self) -> Result<Request, ComfyError> {
        self.get("object_info")
    }

    /// `GET /queue`. Parse with [`crate::QueueState::parse`].
    pub fn queue(&self) -> Result<Request, ComfyError> {
        self.get("queue")
    }

    /// `POST /queue` deleting pending prompts (cancel before they start).
    pub fn delete_from_queue(&self, prompt_ids: &[String]) -> Result<Request, ComfyError> {
        let mut budget = crate::json_bounds::Budget::default();
        budget.container(1, 0)?;
        budget.key("delete", 1)?;
        budget.container(prompt_ids.len(), 1)?;
        for id in prompt_ids {
            budget.text(id, 2)?;
        }
        self.post_json("queue", &serde_json::json!({ "delete": prompt_ids }))
    }

    /// `POST /interrupt`. With `prompt_id`, newer servers stop only that prompt (and ignore the
    /// request when another one is running), which is what a shared server needs; without it,
    /// whatever is running stops.
    pub fn interrupt(&self, prompt_id: Option<&str>) -> Result<Request, ComfyError> {
        let mut budget = crate::json_bounds::Budget::default();
        budget.container(usize::from(prompt_id.is_some()), 0)?;
        if let Some(id) = prompt_id {
            budget.key("prompt_id", 1)?;
            budget.text(id, 1)?;
        }
        let body = match prompt_id {
            Some(id) => serde_json::json!({ "prompt_id": id }),
            None => serde_json::json!({}),
        };
        self.post_json("interrupt", &body)
    }

    /// `GET /history/{prompt_id}`. Parse with [`crate::HistoryEntry::parse`].
    pub fn history(&self, prompt_id: &str) -> Result<Request, ComfyError> {
        let mut url = self.url("history/")?;
        url.path_segments_mut().map_err(|()| ComfyError::InvalidUrl("cannot be a base".into()))?.pop_if_empty().push(prompt_id);
        Ok(self.request(Method::Get, url, Body::Empty))
    }

    /// `POST /prompt`: queue a workflow. Parse the reply with [`crate::parse_prompt_response`].
    pub fn prompt(&self, prompt: &crate::PromptRequest) -> Result<Request, ComfyError> {
        self.post_json("prompt", &prompt.to_json()?)
    }

    /// `GET /view`: download an output, temp or input file.
    pub fn view(&self, file: &FileRef) -> Result<Request, ComfyError> {
        let mut url = self.url("view")?;
        url.query_pairs_mut().append_pair("filename", &file.filename).append_pair("subfolder", &file.subfolder).append_pair("type", &file.kind);
        Ok(self.request(Method::Get, url, Body::Empty))
    }

    /// `POST /upload/image`: upload an input image (or mask) so a `LoadImage` node can read it.
    /// The reply names the stored file; parse it with [`FileRef`] via [`parse_upload_response`].
    pub fn upload_image(&self, filename: &str, bytes: &[u8], kind: UploadKind, overwrite: bool) -> Result<Request, ComfyError> {
        let filename = sanitize_filename(filename)?;
        let boundary = boundary_for(bytes)?;
        let mut body = Vec::with_capacity(bytes.len().saturating_add(512));
        let mut field = |name: &str, value: &str| {
            body.extend_from_slice(format!("--{boundary}\r\nContent-Disposition: form-data; name=\"{name}\"\r\n\r\n{value}\r\n").as_bytes());
        };
        field("type", kind.as_str());
        field("overwrite", if overwrite { "true" } else { "false" });
        body.extend_from_slice(
            format!(
                "--{boundary}\r\nContent-Disposition: form-data; name=\"image\"; filename=\"{filename}\"\r\nContent-Type: application/octet-stream\r\n\r\n"
            )
            .as_bytes(),
        );
        body.extend_from_slice(bytes);
        body.extend_from_slice(format!("\r\n--{boundary}--\r\n").as_bytes());
        Ok(self.request(Method::Post, self.url("upload/image")?, Body::Multipart { boundary, bytes: body }))
    }

    /// The WebSocket URL for progress messages addressed to `client_id`.
    pub fn websocket_url(&self, client_id: &str) -> Result<Url, ComfyError> {
        let mut url = self.url("ws")?;
        let scheme = if self.base.scheme() == "https" { "wss" } else { "ws" };
        url.set_scheme(scheme).map_err(|()| ComfyError::InvalidUrl("cannot switch to a WebSocket scheme".into()))?;
        url.query_pairs_mut().append_pair("clientId", client_id);
        Ok(url)
    }

    /// Headers the WebSocket handshake needs (the auth header, if any). Browsers can't set
    /// headers on WebSockets, so the browser build needs a server that authenticates another way
    /// (same-origin cookie or a token in the URL).
    pub fn websocket_headers(&self) -> Vec<(String, String)> {
        self.auth.iter().cloned().collect()
    }
}

/// Reads the `/upload/image` reply (`{"name", "subfolder", "type"}`) as the file to reference.
pub fn parse_upload_response(bytes: &[u8]) -> Result<FileRef, ComfyError> {
    #[derive(serde::Deserialize)]
    struct Reply {
        name: String,
        #[serde(default)]
        subfolder: String,
        #[serde(rename = "type", default = "default_input_kind")]
        kind: String,
    }
    fn default_input_kind() -> String {
        "input".into()
    }
    let r: Reply = crate::error::parse_json("upload reply", bytes, 64 * 1024)?;
    Ok(FileRef { filename: r.name, subfolder: r.subfolder, kind: r.kind })
}

/// A file name safe to put in a multipart header: no path, no quotes or control characters.
fn sanitize_filename(name: &str) -> Result<String, ComfyError> {
    let base = name.rsplit(['/', '\\']).next().unwrap_or_default();
    let clean: String = base.chars().filter(|c| !c.is_control() && *c != '"').take(200).collect();
    let clean = clean.trim();
    if clean.is_empty() || clean == "." || clean == ".." {
        return Err(ComfyError::InvalidInput { node: "-".into(), input: "filename".into(), message: format!("{name:?} is not a usable file name") });
    }
    Ok(clean.to_owned())
}

/// A multipart boundary that does not occur in `bytes`. Candidates are derived from the content,
/// so a file can't be crafted to contain them in advance; the attempts are capped.
fn boundary_for(bytes: &[u8]) -> Result<String, ComfyError> {
    let mut n = bytes.iter().fold(0xcbf2_9ce4_8422_2325_u64, |h, b| (h ^ u64::from(*b)).wrapping_mul(0x0100_0000_01b3));
    for _ in 0..8 {
        let boundary = format!("----NodeCraftBoundary{n:016x}");
        if !bytes.windows(boundary.len()).any(|w| w == boundary.as_bytes()) {
            return Ok(boundary);
        }
        n = n.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1_442_695_040_888_963_407);
    }
    Err(ComfyError::InvalidInput { node: "-".into(), input: "upload".into(), message: "could not find a multipart boundary for this file".into() })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn server() -> Server {
        Server::new("http://127.0.0.1:8188").unwrap()
    }

    #[test]
    fn urls_are_validated_and_normalised() {
        assert_eq!(server().base_url().as_str(), "http://127.0.0.1:8188/");
        let prefixed = Server::new(" https://gpu.example.com/comfy?x=1#y ").unwrap();
        assert_eq!(prefixed.base_url().as_str(), "https://gpu.example.com/comfy/");
        assert_eq!(prefixed.system_stats().unwrap().url.as_str(), "https://gpu.example.com/comfy/system_stats");
        for bad in ["", "localhost:8188", "ftp://x", "file:///tmp", "http://user:pw@host", "http://"] {
            assert!(Server::new(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn auth_header_is_sent_but_never_printed() {
        let s = server().with_auth_header("Authorization", "Bearer s3cret").unwrap();
        let r = s.object_info().unwrap();
        assert_eq!(r.headers, vec![("Authorization".to_string(), "Bearer s3cret".to_string())]);
        assert!(!format!("{s:?}{r:?}").contains("s3cret"));
        assert!(server().with_auth_header("X-Key", "a\r\nInjected: 1").is_err());
        assert!(server().with_auth_header("Bad Name", "v").is_err());
    }

    fn assert_private_debug(value: &impl fmt::Debug, secrets: &[&[u8]]) {
        for output in [format!("{value:?}"), format!("{value:#?}")] {
            for secret in secrets {
                assert!(!output.contains(String::from_utf8_lossy(secret).as_ref()), "{output}");
                // Derived Vec<u8> Debug is recoverable in compact and pretty forms.
                let compact: String = output.chars().filter(|c| !c.is_whitespace()).collect();
                let decimal = secret.iter().map(u8::to_string).collect::<Vec<_>>().join(",");
                assert!(!compact.contains(&decimal), "{output}");
            }
        }
    }

    #[test]
    fn server_debug_omits_private_base_path_without_changing_requests() {
        let server = Server::new("https://example.com:8443/fabricated-private-base-path")
            .unwrap()
            .with_auth_header("X-API-Key", "fabricated-private-auth-value")
            .unwrap();
        let base = server.base_url().clone();
        let request = server.object_info().unwrap();
        assert_private_debug(&server, &[b"fabricated-private-base-path", b"fabricated-private-auth-value"]);
        for output in [format!("{server:?}"), format!("{server:#?}")] {
            assert!(output.contains("https://example.com:8443") && output.contains("X-API-Key"));
        }
        assert_eq!(server.base_url(), &base);
        assert_eq!(base.as_str(), "https://example.com:8443/fabricated-private-base-path/");
        assert_eq!(server.object_info().unwrap(), request);
        assert_eq!(request.url.as_str(), "https://example.com:8443/fabricated-private-base-path/object_info");
        assert_eq!(request.headers, vec![("X-API-Key".into(), "fabricated-private-auth-value".into())]);
    }

    #[test]
    fn prompt_debug_redacts_credentials_without_changing_transport() {
        let workflow = crate::ApiWorkflow::parse(crate::fixtures::API_WORKFLOW.as_bytes(), None).unwrap();
        let mut prompt = crate::PromptRequest::new(workflow, "client-1");
        let secret = "fabricated-comfy-account-secret";
        prompt.extra_data.insert("api_key_comfy_org".into(), serde_json::json!(secret));
        let request = server().with_auth_header("Authorization", "fabricated-header-secret").unwrap().prompt(&prompt).unwrap();
        let original = request.clone();
        assert_private_debug(&request.body, &[secret.as_bytes()]);
        assert_private_debug(&request, &[secret.as_bytes(), b"fabricated-header-secret"]);
        let output = format!("{request:?}");
        assert!(output.contains("Post") && output.contains("http://127.0.0.1:8188") && output.contains("Authorization"));
        let expected = serde_json::to_vec(&prompt.to_json().unwrap()).unwrap();
        assert!(output.contains(&format!("Json {{ byte_len: {} }}", expected.len())));
        assert_eq!(request, original);
        assert_eq!(request.body, Body::Json(expected));
        assert_eq!(request.content_type().as_deref(), Some("application/json"));
    }

    #[test]
    fn multipart_debug_redacts_upload_and_boundary_without_changing_transport() {
        let payload = b"fabricated-private-image-content";
        let request = server().upload_image("private.png", payload, UploadKind::Input, false).unwrap();
        let original = request.clone();
        let Body::Multipart { boundary, bytes } = &request.body else { panic!("not multipart") };
        assert_private_debug(&request.body, &[payload, boundary.as_bytes(), b"private.png"]);
        assert_private_debug(&request, &[payload, boundary.as_bytes(), b"private.png"]);
        assert!(format!("{request:?}").contains(&format!("Multipart {{ byte_len: {} }}", bytes.len())));
        assert!(bytes.windows(payload.len()).any(|w| w == payload));
        assert_eq!(request.content_type().unwrap(), format!("multipart/form-data; boundary={boundary}"));
        assert_eq!(request, original);
        // A caller-supplied boundary is private too, even when it is not a generated hash.
        let custom = Body::Multipart { boundary: "fabricated-boundary-secret".into(), bytes: payload.to_vec() };
        assert_private_debug(&custom, &[payload, b"fabricated-boundary-secret"]);
        assert_eq!(format!("{:?}", Body::Empty), "Empty");
    }

    #[test]
    fn public_request_debug_omits_url_credentials_query_and_fragment() {
        let request = Request {
            method: Method::Get,
            url: Url::parse("https://private-user:private-password@example.com:8443/private-path?token=private-query#private-fragment").unwrap(),
            headers: vec![("X-API-Key".into(), "private-header".into())],
            body: Body::Empty,
        };
        let original = request.clone();
        assert_private_debug(&request, &[b"private-user", b"private-password", b"private-query", b"private-fragment", b"private-path", b"private-header"]);
        for output in [format!("{request:?}"), format!("{request:#?}")] {
            assert!(output.contains("https://example.com:8443"));
            assert!(output.contains("Get") && output.contains("X-API-Key"));
        }
        assert_eq!(request, original);
        assert_eq!(request.content_type(), None);
    }

    #[test]
    fn endpoints() {
        let s = server();
        assert_eq!(s.queue().unwrap().url.as_str(), "http://127.0.0.1:8188/queue");
        assert_eq!(s.history("ab/../c d").unwrap().url.as_str(), "http://127.0.0.1:8188/history/ab%2F..%2Fc%20d");
        let file = FileRef { filename: "a b&c.png".into(), subfolder: "x".into(), kind: "output".into() };
        assert_eq!(s.view(&file).unwrap().url.as_str(), "http://127.0.0.1:8188/view?filename=a+b%26c.png&subfolder=x&type=output");
        assert_eq!(s.websocket_url("abc").unwrap().as_str(), "ws://127.0.0.1:8188/ws?clientId=abc");
        let tls = Server::new("https://h/p").unwrap();
        assert_eq!(tls.websocket_url("abc").unwrap().as_str(), "wss://h/p/ws?clientId=abc");
        let del = s.delete_from_queue(&["p1".into()]).unwrap();
        assert_eq!(del.method, Method::Post);
        assert_eq!(del.body, Body::Json(br#"{"delete":["p1"]}"#.to_vec()));
        assert_eq!(del.content_type().as_deref(), Some("application/json"));
        assert_eq!(s.interrupt(None).unwrap().url.as_str(), "http://127.0.0.1:8188/interrupt");
        assert_eq!(s.interrupt(Some("p1")).unwrap().body, Body::Json(br#"{"prompt_id":"p1"}"#.to_vec()));
    }

    #[test]
    fn upload_is_multipart_with_a_safe_name() {
        let r = server().upload_image("../../etc/pa\"ss\r\nwd.png", b"PNGDATA", UploadKind::Input, true).unwrap();
        let Body::Multipart { boundary, bytes } = &r.body else { panic!("not multipart") };
        let text = String::from_utf8_lossy(bytes);
        assert!(text.contains("filename=\"passwd.png\""), "{text}");
        assert!(text.contains("name=\"type\"\r\n\r\ninput\r\n"));
        assert!(text.contains("name=\"overwrite\"\r\n\r\ntrue\r\n"));
        assert!(text.contains("\r\n\r\nPNGDATA\r\n"));
        assert!(text.ends_with(&format!("--{boundary}--\r\n")));
        assert_eq!(r.content_type().unwrap(), format!("multipart/form-data; boundary={boundary}"));
        assert!(server().upload_image("..", b"x", UploadKind::Temp, false).is_err());
        assert!(server().upload_image("dir/", b"x", UploadKind::Temp, false).is_err());
    }

    #[test]
    fn boundary_avoids_the_payload() {
        let b = boundary_for(b"payload").unwrap();
        assert!(!b"payload".windows(b.len()).any(|w| w == b.as_bytes()));
        let tricky = format!("xx{b}yy");
        let other = boundary_for(tricky.as_bytes()).unwrap();
        assert!(!tricky.contains(&other));
    }

    #[test]
    fn upload_reply() {
        let f = parse_upload_response(br#"{"name":"cat (1).png","subfolder":"","type":"input"}"#).unwrap();
        assert_eq!(f, FileRef { filename: "cat (1).png".into(), subfolder: String::new(), kind: "input".into() });
        assert!(parse_upload_response(b"<html>").is_err());
    }
}
