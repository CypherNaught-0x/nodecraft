//! `wss://` for the relay (#1067): a browser on an HTTPS page may only open a secure WebSocket,
//! so the relay can serve TLS itself with a certificate the operator supplies, and no proxy is
//! needed in front of it. Plain `ws://` stays the default, for local development.
//!
//! The TLS stack is the one the rest of the workspace uses on each platform: rustls (with
//! aws-lc) on Linux and other Unix, the operating system's own (native-tls) on Windows and macOS.
//! Each connection's handshake runs on that connection's own thread, under the same hello
//! deadline as the WebSocket handshake, so a client that stalls in it holds only its own slot.
use std::io::{Read, Write};
use std::net::TcpStream;
use std::path::Path;

/// The most bytes read from a certificate or key file: a chain of a few certificates.
const MAX_PEM_BYTES: u64 = 1 << 20;

/// What the relay serves its connections with: TLS from the operator's certificate.
pub struct Tls(backend::Acceptor);

impl Tls {
    /// Loads the PEM certificate chain at `cert` and the PEM private key at `key`. The message
    /// says which file is wrong, never what is in it.
    pub fn load(cert: &Path, key: &Path) -> Result<Self, String> {
        let cert = read(cert, "the certificate file")?;
        let key = read(key, "the key file")?;
        backend::Acceptor::new(&cert, &key).map(Self)
    }

    /// Runs the TLS handshake on `tcp`; `None` when it fails or times out.
    pub fn accept(&self, tcp: TcpStream) -> Option<Stream> {
        self.0.accept(tcp).map(|tls| Stream::Tls(Box::new(tls)))
    }
}

fn read(path: &Path, what: &str) -> Result<Vec<u8>, String> {
    let file = std::fs::File::open(path).map_err(|_| format!("the relay can't read {what}"))?;
    let mut bytes = Vec::new();
    file.take(MAX_PEM_BYTES.saturating_add(1)).read_to_end(&mut bytes).map_err(|_| format!("the relay can't read {what}"))?;
    if u64::try_from(bytes.len()).unwrap_or(u64::MAX) > MAX_PEM_BYTES {
        return Err(format!("{what} is larger than a certificate or key can be"));
    }
    Ok(bytes)
}

/// One connection, plain or secure.
pub enum Stream {
    Plain(TcpStream),
    Tls(Box<backend::Stream>),
}

impl Stream {
    /// The socket under it, for its timeouts.
    pub fn tcp(&self) -> &TcpStream {
        match self {
            Self::Plain(tcp) => tcp,
            Self::Tls(tls) => backend::tcp(tls),
        }
    }
}

impl Read for Stream {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        match self {
            Self::Plain(tcp) => tcp.read(buf),
            Self::Tls(tls) => tls.read(buf),
        }
    }
}

impl Write for Stream {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        match self {
            Self::Plain(tcp) => tcp.write(buf),
            Self::Tls(tls) => tls.write(buf),
        }
    }
    fn flush(&mut self) -> std::io::Result<()> {
        match self {
            Self::Plain(tcp) => tcp.flush(),
            Self::Tls(tls) => tls.flush(),
        }
    }
}

#[cfg(not(any(windows, target_os = "macos")))]
mod backend {
    use rustls::pki_types::pem::PemObject;
    use rustls::pki_types::{CertificateDer, PrivateKeyDer};
    use rustls::{ServerConfig, ServerConnection, StreamOwned};
    use std::net::TcpStream;
    use std::sync::Arc;

    pub type Stream = StreamOwned<ServerConnection, TcpStream>;

    pub struct Acceptor(Arc<ServerConfig>);

    impl Acceptor {
        pub fn new(cert: &[u8], key: &[u8]) -> Result<Self, String> {
            let chain: Vec<CertificateDer<'static>> =
                CertificateDer::pem_slice_iter(cert).collect::<Result<_, _>>().map_err(|_| "the certificate file isn't a PEM certificate chain".to_owned())?;
            if chain.is_empty() {
                return Err("the certificate file holds no certificate".into());
            }
            let key = PrivateKeyDer::from_pem_slice(key).map_err(|_| "the key file isn't a PEM private key".to_owned())?;
            let provider = Arc::new(rustls::crypto::aws_lc_rs::default_provider());
            let config = ServerConfig::builder_with_provider(provider)
                .with_safe_default_protocol_versions()
                .map_err(|_| "the relay's TLS can't start".to_owned())?
                .with_no_client_auth()
                .with_single_cert(chain, key)
                .map_err(|_| "the certificate and the key don't belong together".to_owned())?;
            Ok(Self(Arc::new(config)))
        }

        /// rustls shakes hands as the stream is first read, under the socket's read timeout.
        pub fn accept(&self, tcp: TcpStream) -> Option<Stream> {
            let mut connection = ServerConnection::new(Arc::clone(&self.0)).ok()?;
            let mut tcp = tcp;
            while connection.is_handshaking() {
                connection.complete_io(&mut tcp).ok()?;
            }
            Some(StreamOwned::new(connection, tcp))
        }
    }

    pub fn tcp(stream: &Stream) -> &TcpStream {
        &stream.sock
    }
}

#[cfg(any(windows, target_os = "macos"))]
mod backend {
    use std::net::TcpStream;

    pub type Stream = native_tls::TlsStream<TcpStream>;

    pub struct Acceptor(native_tls::TlsAcceptor);

    impl Acceptor {
        pub fn new(cert: &[u8], key: &[u8]) -> Result<Self, String> {
            let identity =
                native_tls::Identity::from_pkcs8(cert, key).map_err(|_| "the certificate and key aren't a PEM certificate and its PKCS #8 key".to_owned())?;
            native_tls::TlsAcceptor::new(identity).map(Self).map_err(|_| "the relay's TLS can't start with that certificate".to_owned())
        }

        /// The handshake runs now, under the socket's read timeout.
        pub fn accept(&self, tcp: TcpStream) -> Option<Stream> {
            self.0.accept(tcp).ok()
        }
    }

    pub fn tcp(stream: &Stream) -> &TcpStream {
        stream.get_ref()
    }
}
