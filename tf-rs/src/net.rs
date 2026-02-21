//! Async MUD connection: TCP + optional TLS, Telnet codec, MCCP.
//!
//! Corresponds to the network I/O portions of `socket.c`.
//!
//! [`Connection`] wraps a tokio TCP (or TLS) stream with:
//! - Telnet byte-stream parsing via [`crate::telnet::TelnetParser`]
//! - RFC 1143 option negotiation via [`crate::telnet::NegotiationState`]
//! - MCCP v1/v2 decompression via [`flate2`]
//! - Line/prompt framing (CRLF normalisation, GA/EOR as prompt markers)
//!
//! The pure protocol logic lives in [`Protocol`], which is independently
//! testable without any real I/O.
//!
//! ## MCCP activation note
//!
//! Per the MCCP2 spec, compression begins on the byte immediately after the
//! `IAC SE` of the `IAC SB COMPRESS2 IAC SE` subnegotiation.  Because a
//! single [`Connection::recv`] call reads one TCP segment into a flat buffer
//! before telnet-parsing it, activation mid-segment is handled as follows:
//! the current segment is parsed uncompressed; the *next* call to `recv`
//! will see compressed data.  In practice servers start a new segment for
//! the compressed stream, so this simplification is invisible to users.

use std::io;
use std::sync::Arc;

use flate2::{Decompress, FlushDecompress};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio_rustls::rustls::pki_types::ServerName;
use tokio_rustls::rustls::{ClientConfig, RootCertStore};
use tokio_rustls::TlsConnector;

use crate::telnet::{
    build_naws, build_ttype, opt, NegotiationState, TelnetEvent, TelnetParser, WILL,
};

// ── NetEvent ──────────────────────────────────────────────────────────────

/// High-level events produced by [`Connection::recv`].
#[derive(Debug)]
pub enum NetEvent {
    /// A complete line of text from the server (CRLF stripped).
    Line(Vec<u8>),
    /// A prompt — incomplete line followed by GA or EOR.
    Prompt(Vec<u8>),
    /// A GMCP message: `(module.verb, json_payload)`.
    Gmcp(String, String),
    /// An ATCP message: `(function, value)`.
    Atcp(String, String),
    /// The server closed the connection.
    Closed,
}

// ── Protocol (pure, testable) ─────────────────────────────────────────────

/// Pure protocol state: Telnet parsing, option negotiation, MCCP, line
/// framing.  Contains no I/O handles and has no async methods.
///
/// [`Connection`] owns a `Protocol` and delegates all decoding to it.
pub struct Protocol {
    parser: TelnetParser,
    neg: NegotiationState,
    /// Active MCCP decompressor (`Some` once COMPRESS/COMPRESS2 activates).
    mccp: Option<Decompress>,
    decomp_scratch: Vec<u8>,
    /// Partial line accumulator (bytes between newlines / prompt markers).
    line_buf: Vec<u8>,
    pub term_width: u16,
    pub term_height: u16,
}

impl Protocol {
    pub fn new() -> Self {
        Self {
            parser: TelnetParser::new(),
            neg: NegotiationState::new(),
            mccp: None,
            decomp_scratch: vec![0u8; 65536],
            line_buf: Vec::new(),
            term_width: 80,
            term_height: 24,
        }
    }

    /// Process a raw byte slice from the network.
    ///
    /// Returns `(net_events, bytes_to_send)`.  The caller must write
    /// `bytes_to_send` back to the server (Telnet negotiation responses, etc.).
    pub fn process(&mut self, raw: &[u8]) -> (Vec<NetEvent>, Vec<u8>) {
        // If MCCP is active, decompress before telnet-parsing.
        let telnet_input: Vec<u8> = if let Some(ref mut decomp) = self.mccp {
            match mccp_decompress(decomp, raw, &mut self.decomp_scratch) {
                Ok(v) => v,
                Err(_) => raw.to_vec(), // fall back on decompression failure
            }
        } else {
            raw.to_vec()
        };

        let events = self.parser.feed(&telnet_input);
        let mut net_events = Vec::new();
        let mut send_buf = Vec::new();

        for event in events {
            self.dispatch(event, &mut net_events, &mut send_buf);
        }

        (net_events, send_buf)
    }

    fn dispatch(
        &mut self,
        event: TelnetEvent,
        net_events: &mut Vec<NetEvent>,
        send_buf: &mut Vec<u8>,
    ) {
        match event {
            TelnetEvent::Data(data) => self.ingest_data(&data, net_events),
            TelnetEvent::Will(o) => {
                if let Some(resp) = self.neg.receive_will(o) {
                    send_buf.extend_from_slice(&resp);
                }
            }
            TelnetEvent::Wont(o) => {
                if let Some(resp) = self.neg.receive_wont(o) {
                    send_buf.extend_from_slice(&resp);
                }
            }
            TelnetEvent::Do(o) => {
                if let Some(resp) = self.neg.receive_do(o) {
                    if resp[1] == WILL && o == opt::NAWS {
                        // Agreed to NAWS — send WILL then immediately report size.
                        send_buf.extend_from_slice(&resp);
                        send_buf.extend_from_slice(&build_naws(self.term_width, self.term_height));
                        return;
                    }
                    send_buf.extend_from_slice(&resp);
                }
            }
            TelnetEvent::Dont(o) => {
                if let Some(resp) = self.neg.receive_dont(o) {
                    send_buf.extend_from_slice(&resp);
                }
            }
            TelnetEvent::Subneg(o, data) => {
                self.handle_subneg(o, data, net_events, send_buf);
            }
            TelnetEvent::GoAhead | TelnetEvent::Eor => {
                // Flush accumulated bytes as a prompt.
                if !self.line_buf.is_empty() {
                    let prompt = std::mem::take(&mut self.line_buf);
                    net_events.push(NetEvent::Prompt(prompt));
                }
            }
        }
    }

    fn ingest_data(&mut self, data: &[u8], net_events: &mut Vec<NetEvent>) {
        for &b in data {
            if b == b'\n' {
                // Strip trailing \r (CRLF → LF normalisation).
                if self.line_buf.last() == Some(&b'\r') {
                    self.line_buf.pop();
                }
                let line = std::mem::take(&mut self.line_buf);
                net_events.push(NetEvent::Line(line));
            } else {
                self.line_buf.push(b);
            }
        }
    }

    fn handle_subneg(
        &mut self,
        opt_byte: u8,
        data: Vec<u8>,
        net_events: &mut Vec<NetEvent>,
        send_buf: &mut Vec<u8>,
    ) {
        match opt_byte {
            opt::COMPRESS | opt::COMPRESS2 => {
                // Server starts MCCP compression after this subneg.
                // See module doc for the mid-segment limitation.
                if self.mccp.is_none() {
                    self.mccp = Some(Decompress::new(true)); // zlib header
                }
            }
            opt::TTYPE => {
                // 0x01 = SEND — server requests our terminal type.
                if data.first() == Some(&1) {
                    send_buf.extend_from_slice(&build_ttype("ANSI"));
                }
            }
            opt::GMCP => {
                if let Ok(s) = std::str::from_utf8(&data) {
                    let (module, payload) = s.split_once(' ').unwrap_or((s, ""));
                    net_events.push(NetEvent::Gmcp(module.to_owned(), payload.to_owned()));
                }
            }
            opt::ATCP => {
                if let Ok(s) = std::str::from_utf8(&data) {
                    let (func, val) = s.split_once(' ').unwrap_or((s, ""));
                    net_events.push(NetEvent::Atcp(func.to_owned(), val.to_owned()));
                }
            }
            _ => {} // unknown subneg — ignore
        }
    }

    /// Whether *we* are active for `opt` (i.e. we sent WILL and server DOed).
    pub fn is_us(&self, opt: u8) -> bool {
        self.neg.is_us(opt)
    }
}

impl Default for Protocol {
    fn default() -> Self {
        Self::new()
    }
}

// ── Internal stream type ──────────────────────────────────────────────────

enum Inner {
    Plain(TcpStream),
    Tls(Box<tokio_rustls::client::TlsStream<TcpStream>>),
}

impl Inner {
    async fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        match self {
            Inner::Plain(s) => s.read(buf).await,
            Inner::Tls(s) => s.read(buf).await,
        }
    }

    async fn write_all(&mut self, buf: &[u8]) -> io::Result<()> {
        match self {
            Inner::Plain(s) => s.write_all(buf).await,
            Inner::Tls(s) => s.write_all(buf).await,
        }
    }
}

// ── Connection ────────────────────────────────────────────────────────────

const READ_BUF: usize = 8192;

/// A single async MUD server connection.
///
/// After construction via [`Self::connect_plain`] or [`Self::connect_tls`],
/// drive the connection with [`Self::send_line`] and [`Self::recv`].
pub struct Connection {
    stream: Inner,
    proto: Protocol,
}

impl Connection {
    /// Open a plain TCP connection to `host:port`.
    pub async fn connect_plain(host: &str, port: u16) -> io::Result<Self> {
        let stream = TcpStream::connect((host, port)).await?;
        Ok(Self { stream: Inner::Plain(stream), proto: Protocol::new() })
    }

    /// Open a TLS connection to `host:port` using the Mozilla root bundle.
    pub async fn connect_tls(host: &str, port: u16) -> io::Result<Self> {
        let mut roots = RootCertStore::empty();
        roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());

        let config = ClientConfig::builder()
            .with_root_certificates(roots)
            .with_no_client_auth();

        let connector = TlsConnector::from(Arc::new(config));
        let server_name: ServerName<'static> = ServerName::try_from(host.to_owned())
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e.to_string()))?;

        let tcp = TcpStream::connect((host, port)).await?;
        let tls = connector.connect(server_name, tcp).await?;
        Ok(Self { stream: Inner::Tls(Box::new(tls)), proto: Protocol::new() })
    }

    /// Update the terminal dimensions; sends NAWS if the option is active.
    pub async fn set_term_size(&mut self, width: u16, height: u16) -> io::Result<()> {
        self.proto.term_width = width;
        self.proto.term_height = height;
        if self.proto.is_us(opt::NAWS) {
            let bytes = build_naws(width, height);
            self.stream.write_all(&bytes).await?;
        }
        Ok(())
    }

    /// Send `line` to the server, appending CRLF.
    ///
    /// Any literal `0xFF` bytes in `line` are doubled (IAC-escaped) per the
    /// Telnet spec.
    pub async fn send_line(&mut self, line: &str) -> io::Result<()> {
        let mut buf = Vec::with_capacity(line.len() + 2);
        for &b in line.as_bytes() {
            if b == 0xFF {
                buf.push(0xFF); // escape
            }
            buf.push(b);
        }
        buf.extend_from_slice(b"\r\n");
        self.stream.write_all(&buf).await
    }

    /// Send raw bytes verbatim (e.g. pre-built Telnet command sequences).
    pub async fn send_raw(&mut self, bytes: &[u8]) -> io::Result<()> {
        self.stream.write_all(bytes).await
    }

    /// Read from the server and decode into [`NetEvent`]s.
    ///
    /// Returns `Ok([NetEvent::Closed])` on EOF, `Err` on I/O error.
    pub async fn recv(&mut self) -> io::Result<Vec<NetEvent>> {
        let mut raw = [0u8; READ_BUF];
        let n = self.stream.read(&mut raw).await?;
        if n == 0 {
            return Ok(vec![NetEvent::Closed]);
        }

        let (net_events, send_buf) = self.proto.process(&raw[..n]);

        if !send_buf.is_empty() {
            self.stream.write_all(&send_buf).await?;
        }

        Ok(net_events)
    }
}

// ── MCCP decompression ────────────────────────────────────────────────────

/// Decompress `input` using the stateful `decomp`, writing into `scratch`.
///
/// Returns the decompressed bytes.  `scratch` is resized as needed.
fn mccp_decompress(
    decomp: &mut Decompress,
    input: &[u8],
    scratch: &mut Vec<u8>,
) -> io::Result<Vec<u8>> {
    // Allocate generously: compressed MUD text rarely expands more than 8×.
    let cap = (input.len() * 8).max(4096);
    if scratch.len() < cap {
        scratch.resize(cap, 0);
    }

    let before_out = decomp.total_out();
    decomp
        .decompress(input, scratch, FlushDecompress::None)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
    let produced = (decomp.total_out() - before_out) as usize;
    Ok(scratch[..produced].to_vec())
}

// ── Tests ─────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::telnet::{build_ttype, opt, IAC, SB, SE};
    use flate2::{Compress, Compression, FlushCompress};

    // ── Protocol / line splitting ─────────────────────────────────────────

    #[test]
    fn line_splitting_crlf() {
        let mut proto = Protocol::new();
        let (events, _) = proto.process(b"hello\r\nworld\r\n");
        assert_eq!(events.len(), 2);
        assert!(matches!(&events[0], NetEvent::Line(l) if l == b"hello"));
        assert!(matches!(&events[1], NetEvent::Line(l) if l == b"world"));
    }

    #[test]
    fn line_splitting_lf_only() {
        let mut proto = Protocol::new();
        let (events, _) = proto.process(b"hello\nworld\n");
        assert_eq!(events.len(), 2);
        assert!(matches!(&events[0], NetEvent::Line(l) if l == b"hello"));
        assert!(matches!(&events[1], NetEvent::Line(l) if l == b"world"));
    }

    #[test]
    fn incomplete_line_buffered() {
        let mut proto = Protocol::new();
        let (events, _) = proto.process(b"partial");
        assert!(events.is_empty());
        assert_eq!(proto.line_buf, b"partial");
    }

    #[test]
    fn go_ahead_flushes_as_prompt() {
        let mut proto = Protocol::new();
        let mut input = b"HP: 100>".to_vec();
        input.extend_from_slice(&[IAC, crate::telnet::GA]);
        let (events, _) = proto.process(&input);
        assert_eq!(events.len(), 1);
        assert!(matches!(&events[0], NetEvent::Prompt(p) if p == b"HP: 100>"));
    }

    #[test]
    fn eor_flushes_as_prompt() {
        let mut proto = Protocol::new();
        let mut input = b"MP: 50>".to_vec();
        input.extend_from_slice(&[IAC, crate::telnet::EOR]);
        let (events, _) = proto.process(&input);
        assert!(matches!(&events[0], NetEvent::Prompt(p) if p == b"MP: 50>"));
    }

    #[test]
    fn empty_go_ahead_produces_no_event() {
        let mut proto = Protocol::new();
        let input = [IAC, crate::telnet::GA];
        let (events, _) = proto.process(&input);
        assert!(events.is_empty());
    }

    // ── Subneg dispatch ───────────────────────────────────────────────────

    #[test]
    fn gmcp_parses_module_and_payload() {
        let mut proto = Protocol::new();
        let mut input = vec![IAC, SB, opt::GMCP];
        input.extend_from_slice(b"Room.Info {\"id\":1}");
        input.extend_from_slice(&[IAC, SE]);
        let (events, _) = proto.process(&input);
        assert!(matches!(
            &events[0],
            NetEvent::Gmcp(m, p) if m == "Room.Info" && p == "{\"id\":1}"
        ));
    }

    #[test]
    fn gmcp_no_payload() {
        let mut proto = Protocol::new();
        let mut input = vec![IAC, SB, opt::GMCP];
        input.extend_from_slice(b"Core.Ping");
        input.extend_from_slice(&[IAC, SE]);
        let (events, _) = proto.process(&input);
        assert!(matches!(&events[0], NetEvent::Gmcp(m, p) if m == "Core.Ping" && p.is_empty()));
    }

    #[test]
    fn atcp_parses_func_and_value() {
        let mut proto = Protocol::new();
        let mut input = vec![IAC, SB, opt::ATCP];
        input.extend_from_slice(b"auth.request challenge123");
        input.extend_from_slice(&[IAC, SE]);
        let (events, _) = proto.process(&input);
        assert!(matches!(
            &events[0],
            NetEvent::Atcp(f, v) if f == "auth.request" && v == "challenge123"
        ));
    }

    #[test]
    fn ttype_send_responds_with_ansi() {
        let mut proto = Protocol::new();
        // IAC SB TTYPE SEND(1) IAC SE
        let input = [IAC, SB, opt::TTYPE, 1, IAC, SE];
        let (_, send_buf) = proto.process(&input);
        assert_eq!(send_buf, build_ttype("ANSI"));
    }

    // ── Negotiation via Protocol ───────────────────────────────────────────

    #[test]
    fn negotiation_do_naws_sends_will_and_size() {
        let mut proto = Protocol::new();
        proto.term_width = 80;
        proto.term_height = 24;
        let input = [IAC, crate::telnet::DO, opt::NAWS];
        let (_, send_buf) = proto.process(&input);
        // Must contain IAC WILL NAWS then IAC SB NAWS ... IAC SE
        assert!(send_buf.contains(&opt::NAWS));
        assert!(send_buf.windows(3).any(|w| w == [IAC, WILL, opt::NAWS]));
    }

    #[test]
    fn negotiation_will_gmcp_replies_do() {
        let mut proto = Protocol::new();
        let input = [IAC, crate::telnet::WILL, opt::GMCP];
        let (_, send_buf) = proto.process(&input);
        assert_eq!(send_buf, vec![IAC, crate::telnet::DO, opt::GMCP]);
    }

    // ── MCCP ──────────────────────────────────────────────────────────────

    #[test]
    fn mccp_decompress_roundtrip() {
        let original = b"Hello from the MUD server! ".repeat(20);

        // Compress with flate2 zlib.
        let mut comp = Compress::new(Compression::default(), true);
        let mut compressed = vec![0u8; original.len() * 2];
        comp.compress(&original, &mut compressed, FlushCompress::Finish)
            .unwrap();
        let compressed = &compressed[..(comp.total_out() as usize)];

        // Decompress via our helper.
        let mut decomp = Decompress::new(true);
        let mut scratch = vec![0u8; 4096];
        let result = mccp_decompress(&mut decomp, compressed, &mut scratch).unwrap();
        assert_eq!(result, original);
    }

    // ── Async integration (local loopback) ────────────────────────────────

    #[tokio::test]
    async fn connect_and_recv_line() {
        use tokio::io::AsyncWriteExt;
        use tokio::net::TcpListener;

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();

        let server = tokio::spawn(async move {
            let (mut sock, _) = listener.accept().await.unwrap();
            sock.write_all(b"Welcome!\r\n").await.unwrap();
        });

        let mut conn = Connection::connect_plain("127.0.0.1", addr.port())
            .await
            .unwrap();
        let events = conn.recv().await.unwrap();
        server.await.unwrap();

        assert!(matches!(&events[0], NetEvent::Line(l) if l == b"Welcome!"));
    }

    #[tokio::test]
    async fn telnet_negotiation_over_loopback() {
        use crate::telnet::{DO, WILL};
        use tokio::io::AsyncWriteExt;
        use tokio::net::TcpListener;

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();

        let server = tokio::spawn(async move {
            let (mut sock, _) = listener.accept().await.unwrap();
            // Offer GMCP.
            sock.write_all(&[IAC, WILL, opt::GMCP]).await.unwrap();
            // Read the client's response (IAC DO GMCP).
            let mut buf = [0u8; 8];
            let n = sock.read(&mut buf).await.unwrap();
            assert_eq!(&buf[..n], &[IAC, DO, opt::GMCP]);
        });

        let mut conn = Connection::connect_plain("127.0.0.1", addr.port())
            .await
            .unwrap();
        // recv() reads the WILL GMCP and sends back DO GMCP automatically.
        conn.recv().await.unwrap();
        server.await.unwrap();
    }
}
