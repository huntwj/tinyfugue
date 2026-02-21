//! Telnet protocol parser and option negotiation.
//!
//! Corresponds to the telnet-handling portions of `socket.c`.
//!
//! [`TelnetParser`] is a pure byte-stream FSM: call [`TelnetParser::feed`]
//! with raw bytes from the server to get back a list of [`TelnetEvent`]s.
//!
//! [`NegotiationState`] tracks which options are active and generates the
//! bytes to send in response to WILL/WONT/DO/DONT commands.

// ── Telnet byte constants ──────────────────────────────────────────────────

/// Interpret As Command — starts every Telnet command sequence.
pub const IAC: u8 = 255;
/// Subnegotiation Begin.
pub const SB: u8 = 250;
/// Subnegotiation End.
pub const SE: u8 = 240;
/// Go Ahead — signals end-of-turn / prompt boundary.
pub const GA: u8 = 249;
/// End of Record — alternative prompt boundary used by some servers.
pub const EOR: u8 = 239;
/// WILL — sender will enable the option.
pub const WILL: u8 = 251;
/// WONT — sender will not enable the option.
pub const WONT: u8 = 252;
/// DO — sender requests the receiver to enable the option.
pub const DO: u8 = 253;
/// DONT — sender requests the receiver to disable the option.
pub const DONT: u8 = 254;

/// Well-known Telnet option numbers used by TF.
pub mod opt {
    pub const BINARY: u8 = 0;
    pub const ECHO: u8 = 1;
    pub const SGA: u8 = 3;
    pub const TTYPE: u8 = 24;
    pub const NAWS: u8 = 31;
    pub const CHARSET: u8 = 42;
    pub const COMPRESS: u8 = 85;  // MCCP v1
    pub const COMPRESS2: u8 = 86; // MCCP v2
    pub const OPT102: u8 = 102;
    pub const ATCP: u8 = 200;
    pub const GMCP: u8 = 201;
}

// ── TelnetEvent ───────────────────────────────────────────────────────────

/// A decoded event produced by [`TelnetParser::feed`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TelnetEvent {
    /// Raw data bytes (non-IAC content).
    Data(Vec<u8>),
    /// Server sent `IAC WILL <opt>`.
    Will(u8),
    /// Server sent `IAC WONT <opt>`.
    Wont(u8),
    /// Server sent `IAC DO <opt>`.
    Do(u8),
    /// Server sent `IAC DONT <opt>`.
    Dont(u8),
    /// Server sent `IAC SB <opt> <data> IAC SE`.
    Subneg(u8, Vec<u8>),
    /// Server sent `IAC GA` (go-ahead / prompt marker).
    GoAhead,
    /// Server sent `IAC EOR` (end-of-record / prompt marker).
    Eor,
}

// ── Parser FSM ────────────────────────────────────────────────────────────

#[derive(Debug)]
enum State {
    Normal,
    Iac,
    /// After WILL/WONT/DO/DONT — holds the command byte, awaits option.
    Cmd(u8),
    /// After `IAC SB` — awaits the option byte.
    Sb,
    /// Collecting subnegotiation payload.
    SbData,
    /// Saw `IAC` inside subnegotiation payload.
    SbIac,
}

/// Byte-stream Telnet protocol parser.
///
/// Feed raw server bytes into [`Self::feed`]; receive decoded
/// [`TelnetEvent`]s in return.  The parser holds no I/O handles and is
/// entirely synchronous — suitable for wrapping any byte source.
#[derive(Debug)]
pub struct TelnetParser {
    state: State,
    /// Accumulates normal (non-Telnet) data bytes.
    data_buf: Vec<u8>,
    /// Accumulates subnegotiation payload bytes.
    sb_buf: Vec<u8>,
    /// Option byte for the current subnegotiation.
    sb_opt: u8,
}

impl Default for TelnetParser {
    fn default() -> Self {
        Self::new()
    }
}

impl TelnetParser {
    pub fn new() -> Self {
        Self {
            state: State::Normal,
            data_buf: Vec::new(),
            sb_buf: Vec::new(),
            sb_opt: 0,
        }
    }

    /// Feed a slice of raw bytes; returns all events decoded from them.
    pub fn feed(&mut self, bytes: &[u8]) -> Vec<TelnetEvent> {
        let mut events = Vec::new();
        for &b in bytes {
            self.step(b, &mut events);
        }
        // Flush any trailing data that hasn't been emitted yet.
        if !self.data_buf.is_empty() {
            events.push(TelnetEvent::Data(std::mem::take(&mut self.data_buf)));
        }
        events
    }

    fn flush_data(&mut self, events: &mut Vec<TelnetEvent>) {
        if !self.data_buf.is_empty() {
            events.push(TelnetEvent::Data(std::mem::take(&mut self.data_buf)));
        }
    }

    fn step(&mut self, b: u8, events: &mut Vec<TelnetEvent>) {
        match self.state {
            State::Normal => {
                if b == IAC {
                    self.flush_data(events);
                    self.state = State::Iac;
                } else {
                    self.data_buf.push(b);
                }
            }
            State::Iac => match b {
                IAC => {
                    // IAC IAC — escaped literal 0xFF in data stream.
                    self.data_buf.push(0xFF);
                    self.state = State::Normal;
                }
                WILL | WONT | DO | DONT => {
                    self.state = State::Cmd(b);
                }
                SB => {
                    self.state = State::Sb;
                }
                GA => {
                    events.push(TelnetEvent::GoAhead);
                    self.state = State::Normal;
                }
                EOR => {
                    events.push(TelnetEvent::Eor);
                    self.state = State::Normal;
                }
                _ => {
                    // NOP (241) or other single-byte commands — ignore.
                    self.state = State::Normal;
                }
            },
            State::Cmd(cmd) => {
                let event = match cmd {
                    WILL => TelnetEvent::Will(b),
                    WONT => TelnetEvent::Wont(b),
                    DO   => TelnetEvent::Do(b),
                    DONT => TelnetEvent::Dont(b),
                    _ => unreachable!("only WILL/WONT/DO/DONT reach Cmd state"),
                };
                events.push(event);
                self.state = State::Normal;
            }
            State::Sb => {
                self.sb_opt = b;
                self.sb_buf.clear();
                self.state = State::SbData;
            }
            State::SbData => {
                if b == IAC {
                    self.state = State::SbIac;
                } else {
                    self.sb_buf.push(b);
                }
            }
            State::SbIac => match b {
                SE => {
                    let data = std::mem::take(&mut self.sb_buf);
                    events.push(TelnetEvent::Subneg(self.sb_opt, data));
                    self.state = State::Normal;
                }
                IAC => {
                    // IAC IAC inside SB — literal 0xFF in subneg payload.
                    self.sb_buf.push(0xFF);
                    self.state = State::SbData;
                }
                _ => {
                    // Malformed subnegotiation — discard and recover.
                    self.sb_buf.clear();
                    self.state = State::Normal;
                }
            },
        }
    }
}

// ── NegotiationState ──────────────────────────────────────────────────────

/// Options we will accept a WILL for (by sending DO).
fn should_do(opt: u8) -> bool {
    matches!(
        opt,
        opt::ECHO
            | opt::BINARY
            | opt::SGA
            | opt::COMPRESS
            | opt::COMPRESS2
            | opt::ATCP
            | opt::GMCP
            | opt::OPT102
    )
}

/// Options we will accept a DO for (by sending WILL).
fn should_will(opt: u8) -> bool {
    matches!(opt, opt::TTYPE | opt::NAWS | opt::CHARSET)
}

/// Tracks Telnet option negotiation state.
///
/// Mirrors TF's four bit-arrays `tn_us`, `tn_them`, `tn_us_tog`,
/// `tn_them_tog` from `socket.c`.
///
/// Call [`Self::receive_will`] / [`Self::receive_wont`] /
/// [`Self::receive_do`] / [`Self::receive_dont`] when the corresponding
/// [`TelnetEvent`] arrives; each returns `Some(bytes)` to write back to the
/// server when a response is required.
#[derive(Debug)]
pub struct NegotiationState {
    /// Options *we* are currently active in (server sent DO and we confirmed).
    us: [bool; 256],
    /// Options *they* are currently active in (we sent DO and they confirmed).
    them: [bool; 256],
    /// We sent WILL and are waiting for DO.
    will_pending: [bool; 256],
    /// We sent DO and are waiting for WILL.
    do_pending: [bool; 256],
}

impl Default for NegotiationState {
    fn default() -> Self {
        Self::new()
    }
}

impl NegotiationState {
    pub fn new() -> Self {
        Self {
            us: [false; 256],
            them: [false; 256],
            will_pending: [false; 256],
            do_pending: [false; 256],
        }
    }

    /// Handle incoming `IAC WILL <opt>`.
    ///
    /// Returns bytes to write (IAC DO or IAC DONT), or `None` if no
    /// response is needed.
    pub fn receive_will(&mut self, opt: u8) -> Option<Vec<u8>> {
        let i = opt as usize;
        if self.do_pending[i] {
            // We had already sent DO; this WILL confirms it.
            self.do_pending[i] = false;
            self.them[i] = true;
            None
        } else if !self.them[i] && should_do(opt) {
            self.them[i] = true;
            Some(vec![IAC, DO, opt])
        } else if !self.them[i] {
            Some(vec![IAC, DONT, opt])
        } else {
            None // already active — ignore duplicate
        }
    }

    /// Handle incoming `IAC WONT <opt>`.
    ///
    /// Returns bytes to write (IAC DONT) if the option was active.
    pub fn receive_wont(&mut self, opt: u8) -> Option<Vec<u8>> {
        let i = opt as usize;
        self.do_pending[i] = false;
        if self.them[i] {
            self.them[i] = false;
            Some(vec![IAC, DONT, opt])
        } else {
            None
        }
    }

    /// Handle incoming `IAC DO <opt>`.
    ///
    /// Returns bytes to write (IAC WILL or IAC WONT).
    pub fn receive_do(&mut self, opt: u8) -> Option<Vec<u8>> {
        let i = opt as usize;
        if self.will_pending[i] {
            // We had already sent WILL; this DO confirms it.
            self.will_pending[i] = false;
            self.us[i] = true;
            None
        } else if !self.us[i] && should_will(opt) {
            self.us[i] = true;
            Some(vec![IAC, WILL, opt])
        } else if !self.us[i] {
            Some(vec![IAC, WONT, opt])
        } else {
            None // already active
        }
    }

    /// Handle incoming `IAC DONT <opt>`.
    pub fn receive_dont(&mut self, opt: u8) -> Option<Vec<u8>> {
        let i = opt as usize;
        self.will_pending[i] = false;
        if self.us[i] {
            self.us[i] = false;
            Some(vec![IAC, WONT, opt])
        } else {
            None
        }
    }

    /// Proactively send `IAC WILL <opt>` (e.g. advertising NAWS on connect).
    ///
    /// Returns the bytes to write and marks the option as pending.
    pub fn send_will(&mut self, opt: u8) -> Vec<u8> {
        self.will_pending[opt as usize] = true;
        vec![IAC, WILL, opt]
    }

    /// Proactively send `IAC DO <opt>`.
    pub fn send_do(&mut self, opt: u8) -> Vec<u8> {
        self.do_pending[opt as usize] = true;
        vec![IAC, DO, opt]
    }

    /// Whether *we* are currently active for `opt`.
    pub fn is_us(&self, opt: u8) -> bool {
        self.us[opt as usize]
    }

    /// Whether *they* are currently active for `opt`.
    pub fn is_them(&self, opt: u8) -> bool {
        self.them[opt as usize]
    }
}

// ── Subnegotiation builders ───────────────────────────────────────────────

/// Build an `IAC SB <opt> <data> IAC SE` subnegotiation payload.
///
/// Any `0xFF` bytes in `data` are escaped as `IAC IAC`.
pub fn build_subneg(opt: u8, data: &[u8]) -> Vec<u8> {
    let mut buf = Vec::with_capacity(5 + data.len());
    buf.extend_from_slice(&[IAC, SB, opt]);
    for &b in data {
        if b == IAC {
            buf.push(IAC); // escape
        }
        buf.push(b);
    }
    buf.extend_from_slice(&[IAC, SE]);
    buf
}

/// Build a NAWS subnegotiation advertising `width × height`.
pub fn build_naws(width: u16, height: u16) -> Vec<u8> {
    let data = [
        (width >> 8) as u8,
        width as u8,
        (height >> 8) as u8,
        height as u8,
    ];
    build_subneg(opt::NAWS, &data)
}

/// Build a TTYPE `IS <name>` subnegotiation response.
pub fn build_ttype(name: &str) -> Vec<u8> {
    let mut data = vec![0u8]; // IS = 0
    data.extend_from_slice(name.as_bytes());
    build_subneg(opt::TTYPE, &data)
}

// ── Tests ─────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(bytes: &[u8]) -> Vec<TelnetEvent> {
        TelnetParser::new().feed(bytes)
    }

    fn all_data(events: Vec<TelnetEvent>) -> Vec<u8> {
        events
            .into_iter()
            .flat_map(|e| match e {
                TelnetEvent::Data(d) => d,
                _ => vec![],
            })
            .collect()
    }

    // ── parser ────────────────────────────────────────────────────────────

    #[test]
    fn plain_data_passthrough() {
        let events = parse(b"hello");
        assert_eq!(events, vec![TelnetEvent::Data(b"hello".to_vec())]);
    }

    #[test]
    fn iac_iac_escapes_ff() {
        let events = parse(&[b'x', IAC, IAC, b'y']);
        assert_eq!(all_data(events), vec![b'x', 0xFF, b'y']);
    }

    #[test]
    fn will_command() {
        let events = parse(&[IAC, WILL, opt::GMCP]);
        assert_eq!(events, vec![TelnetEvent::Will(opt::GMCP)]);
    }

    #[test]
    fn wont_command() {
        let events = parse(&[IAC, WONT, opt::ECHO]);
        assert_eq!(events, vec![TelnetEvent::Wont(opt::ECHO)]);
    }

    #[test]
    fn do_command() {
        let events = parse(&[IAC, DO, opt::NAWS]);
        assert_eq!(events, vec![TelnetEvent::Do(opt::NAWS)]);
    }

    #[test]
    fn dont_command() {
        let events = parse(&[IAC, DONT, opt::TTYPE]);
        assert_eq!(events, vec![TelnetEvent::Dont(opt::TTYPE)]);
    }

    #[test]
    fn go_ahead() {
        let events = parse(&[b'>', IAC, GA]);
        assert_eq!(
            events,
            vec![TelnetEvent::Data(b">".to_vec()), TelnetEvent::GoAhead]
        );
    }

    #[test]
    fn eor_event() {
        let events = parse(&[IAC, EOR]);
        assert_eq!(events, vec![TelnetEvent::Eor]);
    }

    #[test]
    fn subneg_gmcp() {
        let payload = b"Core.Hello {}";
        let mut bytes = vec![IAC, SB, opt::GMCP];
        bytes.extend_from_slice(payload);
        bytes.extend_from_slice(&[IAC, SE]);
        let events = parse(&bytes);
        assert_eq!(
            events,
            vec![TelnetEvent::Subneg(opt::GMCP, payload.to_vec())]
        );
    }

    #[test]
    fn subneg_iac_iac_escape() {
        // IAC IAC inside SB payload decodes to 0xFF.
        let bytes = [IAC, SB, opt::BINARY, 0x42, IAC, IAC, 0x43, IAC, SE];
        let events = parse(&bytes);
        assert_eq!(
            events,
            vec![TelnetEvent::Subneg(opt::BINARY, vec![0x42, 0xFF, 0x43])]
        );
    }

    #[test]
    fn mixed_data_and_commands() {
        let mut bytes = b"prompt> ".to_vec();
        bytes.extend_from_slice(&[IAC, GA]);
        let events = parse(&bytes);
        assert_eq!(
            events,
            vec![
                TelnetEvent::Data(b"prompt> ".to_vec()),
                TelnetEvent::GoAhead,
            ]
        );
    }

    #[test]
    fn empty_input_produces_no_events() {
        assert!(parse(&[]).is_empty());
    }

    #[test]
    fn incremental_feeding() {
        // Feeding byte-by-byte must yield the same non-Data events and the
        // same total data bytes as feeding in one call.  Adjacent Data events
        // may be merged differently between the two modes, so we canonicalise
        // by collapsing consecutive Data events before comparing.
        fn canonicalise(events: Vec<TelnetEvent>) -> Vec<TelnetEvent> {
            let mut out: Vec<TelnetEvent> = Vec::new();
            for ev in events {
                match ev {
                    TelnetEvent::Data(d) => {
                        if let Some(TelnetEvent::Data(last)) = out.last_mut() {
                            last.extend_from_slice(&d);
                        } else {
                            out.push(TelnetEvent::Data(d));
                        }
                    }
                    other => out.push(other),
                }
            }
            out
        }

        let full = &[IAC, WILL, opt::GMCP, b'o', b'k'];
        let single = canonicalise(TelnetParser::new().feed(full));

        let mut p = TelnetParser::new();
        let mut incremental: Vec<TelnetEvent> = Vec::new();
        for &b in full {
            incremental.extend(p.feed(&[b]));
        }
        assert_eq!(single, canonicalise(incremental));
    }

    // ── negotiation ───────────────────────────────────────────────────────

    #[test]
    fn will_accepted_for_gmcp() {
        let mut neg = NegotiationState::new();
        let resp = neg.receive_will(opt::GMCP);
        assert_eq!(resp, Some(vec![IAC, DO, opt::GMCP]));
        assert!(neg.is_them(opt::GMCP));
    }

    #[test]
    fn will_rejected_for_ttype() {
        // TTYPE: we WILL it, we don't DO it.
        let mut neg = NegotiationState::new();
        let resp = neg.receive_will(opt::TTYPE);
        assert_eq!(resp, Some(vec![IAC, DONT, opt::TTYPE]));
        assert!(!neg.is_them(opt::TTYPE));
    }

    #[test]
    fn do_accepted_for_naws() {
        let mut neg = NegotiationState::new();
        let resp = neg.receive_do(opt::NAWS);
        assert_eq!(resp, Some(vec![IAC, WILL, opt::NAWS]));
        assert!(neg.is_us(opt::NAWS));
    }

    #[test]
    fn do_rejected_for_gmcp() {
        // GMCP: we DO it, we don't WILL it.
        let mut neg = NegotiationState::new();
        let resp = neg.receive_do(opt::GMCP);
        assert_eq!(resp, Some(vec![IAC, WONT, opt::GMCP]));
        assert!(!neg.is_us(opt::GMCP));
    }

    #[test]
    fn will_after_do_pending_requires_no_response() {
        let mut neg = NegotiationState::new();
        // We proactively sent DO GMCP.
        neg.send_do(opt::GMCP);
        // Server responds with WILL GMCP — no duplicate DO needed.
        let resp = neg.receive_will(opt::GMCP);
        assert!(resp.is_none());
        assert!(neg.is_them(opt::GMCP));
    }

    #[test]
    fn do_after_will_pending_requires_no_response() {
        let mut neg = NegotiationState::new();
        neg.send_will(opt::NAWS);
        let resp = neg.receive_do(opt::NAWS);
        assert!(resp.is_none());
        assert!(neg.is_us(opt::NAWS));
    }

    #[test]
    fn wont_clears_active_option() {
        let mut neg = NegotiationState::new();
        neg.receive_will(opt::GMCP); // activates them[GMCP]
        let resp = neg.receive_wont(opt::GMCP);
        assert_eq!(resp, Some(vec![IAC, DONT, opt::GMCP]));
        assert!(!neg.is_them(opt::GMCP));
    }

    #[test]
    fn dont_clears_active_option() {
        let mut neg = NegotiationState::new();
        neg.receive_do(opt::NAWS); // activates us[NAWS]
        let resp = neg.receive_dont(opt::NAWS);
        assert_eq!(resp, Some(vec![IAC, WONT, opt::NAWS]));
        assert!(!neg.is_us(opt::NAWS));
    }

    // ── builders ──────────────────────────────────────────────────────────

    #[test]
    fn build_naws_correct() {
        let bytes = build_naws(80, 24);
        assert_eq!(bytes, vec![IAC, SB, opt::NAWS, 0, 80, 0, 24, IAC, SE]);
    }

    #[test]
    fn build_naws_wide_dimensions() {
        let bytes = build_naws(256, 100);
        // 256 = 0x01_00
        assert_eq!(bytes, vec![IAC, SB, opt::NAWS, 1, 0, 0, 100, IAC, SE]);
    }

    #[test]
    fn build_ttype_correct() {
        let bytes = build_ttype("ANSI");
        assert_eq!(
            bytes,
            vec![IAC, SB, opt::TTYPE, 0, b'A', b'N', b'S', b'I', IAC, SE]
        );
    }

    #[test]
    fn build_subneg_escapes_iac() {
        let bytes = build_subneg(opt::BINARY, &[0x42, 0xFF, 0x43]);
        assert_eq!(
            bytes,
            vec![IAC, SB, opt::BINARY, 0x42, IAC, 0xFF, 0x43, IAC, SE]
        );
    }
}
