//! The LoginApp UDP server.
//!
//! BigWorld 2.0.1's LoginApp external interface is a UDP Mercury nub
//! (`src/server/loginapp/loginapp.cpp` constructs `Mercury::NetworkInterface`
//! with `NETWORK_INTERFACE_EXTERNAL`); it is **not** TCP. Each datagram is one
//! Mercury packet, so the server is stateless per request: parse the packet,
//! answer with a Mercury reply packet, done. If a reply is lost the client
//! retries (each retry carries a new reply id), so no Mercury reliability/ACKs
//! are needed for the login hop.
//!
//! This module also writes the BaseApp session handoff JSON
//! ([`crate::login::LoginSession`]) right before sending the `LOGGED_ON` reply,
//! because the BaseApp needs the client's Blowfish channel key which is only
//! known to the LoginApp hop.

use std::net::{SocketAddr, SocketAddrV4, UdpSocket};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use crate::bundle::decode_messages;
use crate::crypto::{hexdump, LoginKey};
use crate::interfaces::{login_ids, login_interface, LengthStyle};
use crate::login::{
    build_failure_reply, build_success_reply, parse_login_args, LogOnStatus, LoginSession,
};
pub use crate::login::{pending_session_dir, pending_session_path, write_session_handoff};
use crate::packet::{Endian, Packet};
use crate::stream::Address;

/// LoginApp configuration.
#[derive(Debug, Clone)]
pub struct LoginAppConfig {
    /// UDP address to bind, normally `127.0.0.1:20016` (or `0.0.0.0:20016`).
    pub bind: SocketAddr,
    /// BaseApp address handed to the client in the login reply.
    pub base_app: SocketAddrV4,
    /// Session key the client must present to the BaseApp.
    pub session_key: u32,
    /// Where to atomically write the BaseApp session handoff JSON.
    pub session_out: Option<PathBuf>,
}

impl Default for LoginAppConfig {
    fn default() -> Self {
        Self {
            bind: SocketAddr::V4(SocketAddrV4::new([127, 0, 0, 1].into(), 20016)),
            base_app: SocketAddrV4::new([127, 0, 0, 1].into(), 20017),
            session_key: 0x1234_5678,
            session_out: None,
        }
    }
}

/// Outcome of handling one datagram.
#[derive(Debug, Clone)]
pub struct LoginOutcome {
    pub client_addr: SocketAddr,
    pub version: u32,
    pub username: String,
    pub encrypted: bool,
    pub session_key: u32,
    pub reply_len: usize,
    pub session_written: bool,
}

/// Statistics for logging.
#[derive(Debug, Default, Clone, Copy)]
pub struct LoginStats {
    pub datagrams: u64,
    pub logins_ok: u64,
    pub logins_failed: u64,
    pub malformed: u64,
    pub probes: u64,
}

/// The LoginApp server.
#[derive(Debug)]
pub struct LoginApp {
    config: LoginAppConfig,
    key: LoginKey,
    socket: UdpSocket,
    stats: LoginStats,
    last_reported_version: Option<u32>,
    next_session_offset: u32,
}

impl LoginApp {
    /// Bind the UDP socket. Use port 0 for an ephemeral port in tests.
    pub fn bind(config: LoginAppConfig, key: LoginKey) -> std::io::Result<Self> {
        let socket = UdpSocket::bind(config.bind)?;
        socket.set_read_timeout(Some(std::time::Duration::from_millis(500)))?;
        Ok(Self {
            config,
            key,
            socket,
            stats: LoginStats::default(),
            last_reported_version: None,
            next_session_offset: 0,
        })
    }

    pub fn local_addr(&self) -> std::io::Result<SocketAddr> {
        self.socket.local_addr()
    }

    pub fn stats(&self) -> LoginStats {
        self.stats
    }

    pub fn config(&self) -> &LoginAppConfig {
        &self.config
    }

    /// Allocate a monotonic non-zero session key.
    pub fn allocate_session_key(&mut self) -> u32 {
        loop {
            let offset = self.next_session_offset;
            self.next_session_offset = self.next_session_offset.wrapping_add(1);
            let key = self.config.session_key.wrapping_add(offset);
            if key != 0 {
                return key;
            }
        }
    }

    /// Handle a single datagram. Returns the response bytes and outcome when a
    /// response should be sent.
    ///
    /// This is pure with respect to the socket, so the caller may decide how to
    /// send the reply (the built-in loop uses `send_to`). It does write the
    /// session JSON file when a login succeeds.
    pub fn handle_datagram(
        &mut self,
        data: &[u8],
        from: SocketAddr,
    ) -> Option<(Vec<u8>, LoginOutcome)> {
        self.stats.datagrams += 1;
        tracing::debug!("{}", hexdump(&format!("RX datagram from {from}"), data));

        let packet = match self.decode_packet(data) {
            Ok(p) => p,
            Err(e) => {
                self.stats.malformed += 1;
                tracing::warn!(%from, len = data.len(), error = %e, "dropping undecodable datagram");
                return None;
            }
        };
        let order = self.detect_order(data);
        tracing::debug!(
            flags = format_args!("0x{:04X}", packet.flags),
            body_len = packet.body.len(),
            order = ?order,
            "decoded Mercury packet"
        );

        let table = login_interface();
        let messages = match decode_messages(&packet, &table, order) {
            Ok(m) => m,
            Err(e) => {
                self.stats.malformed += 1;
                tracing::warn!(%from, error = %e, "dropping packet with undecodable bundle");
                return None;
            }
        };

        let mut response: Option<Vec<u8>> = None;
        let mut outcome: Option<LoginOutcome> = None;

        for message in messages {
            match message.id {
                login_ids::LOGIN => {
                    let Some(request) = message.request else {
                        // Not a request: the LoginApp cannot address a reply.
                        tracing::warn!(%from, "login element received without request header; ignoring");
                        continue;
                    };
                    match parse_login_args(&message.data, &self.key) {
                        Ok(req) => {
                            req.log_version();
                            if self.last_reported_version != Some(req.version) {
                                self.last_reported_version = Some(req.version);
                                tracing::info!(
                                    version = req.version,
                                    encrypted = req.encrypted,
                                    rsa_offset = req.rsa_offset,
                                    "client LOGIN_VERSION observed (not enforced)"
                                );
                            }
                            let username = req
                                .params
                                .username_str()
                                .map(|s| s.to_string())
                                .unwrap_or_else(|_| hex::encode(&req.params.username));
                            tracing::info!(
                                %from,
                                version = req.version,
                                encrypted = req.encrypted,
                                username = %username,
                                password_len = req.params.password.len(),
                                encryption_key = %hex::encode(&req.params.encryption_key),
                                nonce = req.params.nonce,
                                "login request accepted"
                            );

                            let session_key = self.allocate_session_key();
                            let session = LoginSession::new(
                                username.clone(),
                                &req.params.encryption_key,
                                session_key,
                                from.to_string(),
                            );
                            let mut session_written = false;
                            if let Some(path) = &self.config.session_out {
                                match write_session_handoff(path, &session) {
                                    Ok(()) => {
                                        session_written = true;
                                        tracing::info!(
                                            path = %path.display(),
                                            session_key,
                                            "wrote BaseApp session handoff"
                                        );
                                    }
                                    Err(e) => {
                                        tracing::error!(
                                            path = %path.display(),
                                            error = %e,
                                            "failed to write session handoff; rejecting login"
                                        );
                                        self.stats.logins_failed += 1;
                                        let r = build_failure_reply(
                                            request.reply_id,
                                            LogOnStatus::LoginRejectedServerNotReady,
                                            "Server failed to persist BaseApp session handoff",
                                        );
                                        response = Some(r);
                                        outcome = None;
                                        continue;
                                    }
                                }
                            }

                            let reply = match build_success_reply(
                                request.reply_id,
                                self.config.base_app.ip().octets(),
                                self.config.base_app.port(),
                                session_key,
                                &req.params.encryption_key,
                            ) {
                                Ok(r) => r,
                                Err(e) => {
                                    tracing::error!(%from, error = %e, "failed to build success reply");
                                    let r = build_failure_reply(
                                        request.reply_id,
                                        LogOnStatus::LoginMalformedRequest,
                                        "Invalid encryption key in login request",
                                    );
                                    response = Some(r);
                                    outcome = None;
                                    continue;
                                }
                            };
                            tracing::info!(
                                %from,
                                reply_id = request.reply_id,
                                base_app = %self.config.base_app,
                                session_key,
                                reply = %hex::encode(&reply),
                                "sending LOGGED_ON reply"
                            );
                            self.stats.logins_ok += 1;
                            outcome = Some(LoginOutcome {
                                client_addr: from,
                                version: req.version,
                                username,
                                encrypted: req.encrypted,
                                session_key,
                                reply_len: reply.len(),
                                session_written,
                            });
                            response = Some(reply);
                        }
                        Err(e) => {
                            self.stats.logins_failed += 1;
                            tracing::warn!(
                                %from,
                                error = %e,
                                payload = %hex::encode(&message.data),
                                "failed to parse login request; sending LOGIN_MALFORMED_REQUEST"
                            );
                            if let Some(request) = message.request {
                                response = Some(build_failure_reply(
                                    request.reply_id,
                                    LogOnStatus::LoginMalformedRequest,
                                    "Could not destream login parameters. Possibly caused by a mis-matching LoginApp keypair.",
                                ));
                            }
                        }
                    }
                }
                login_ids::PROBE => {
                    self.stats.probes += 1;
                    tracing::info!(%from, "probe received");
                    if let Some(request) = message.request {
                        // BigWorld's probe reply is a list of key/string pairs.
                        let mut payload = Vec::new();
                        for (k, v) in [
                            ("hostName", "wot-rust-loginapp"),
                            ("ownerName", "bw201"),
                            ("usersCount", "0"),
                            ("universeName", ""),
                            ("spaceName", ""),
                            ("binaryID", "0"),
                        ] {
                            crate::stream::BinaryWriter::new();
                            let mut w = crate::stream::BinaryWriter::new();
                            w.string(k);
                            w.string(v);
                            payload.extend_from_slice(w.as_slice());
                        }
                        response = Some(crate::login::encode_reply_packet(request.reply_id, &payload));
                    }
                }
                other => {
                    tracing::debug!(%from, id = other, len = message.data.len(), "ignoring unexpected login-interface message");
                }
            }
        }

        response.map(|r| {
            let reply_len = r.len();
            let outcome = outcome.unwrap_or_else(|| LoginOutcome {
                client_addr: from,
                version: 0,
                username: String::new(),
                encrypted: false,
                session_key: self.config.session_key,
                reply_len,
                session_written: false,
            });
            (r, outcome)
        })
    }

    fn decode_packet(&self, data: &[u8]) -> Result<Packet, crate::packet::PacketError> {
        // The retail client is little-endian like stock BigWorld; still, if a
        // little-endian decode fails try big-endian so a nonstandard build is
        // diagnosed instead of silently dropped.
        Packet::decode(data, Endian::Little).or_else(|first| {
            Packet::decode(data, Endian::Big).map_err(|_| first)
        })
    }

    fn detect_order(&self, data: &[u8]) -> Endian {
        Endian::sniff(data).unwrap_or(Endian::Little)
    }

    /// Send one datagram reply.
    pub fn send_reply(&self, to: SocketAddr, reply: &[u8]) -> std::io::Result<usize> {
        tracing::debug!("{}", hexdump(&format!("TX datagram to {to}"), reply));
        self.socket.send_to(reply, to)
    }

    /// Blocking serve loop. Returns when `shutdown` becomes true.
    pub fn run(&mut self, shutdown: Arc<AtomicBool>) -> std::io::Result<()> {
        let mut buf = [0u8; 65535];
        tracing::info!(addr = %self.socket.local_addr()?, "LoginApp UDP server listening");
        while !shutdown.load(Ordering::Relaxed) {
            match self.socket.recv_from(&mut buf) {
                Ok((len, from)) => {
                    if let Some((reply, _outcome)) = self.handle_datagram(&buf[..len], from) {
                        if let Err(e) = self.send_reply(from, &reply) {
                            tracing::warn!(%from, error = %e, "failed to send login reply");
                        }
                    }
                }
                Err(e)
                    if matches!(
                        e.kind(),
                        std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                    ) => {}
                Err(e) => {
                    tracing::warn!(error = %e, "recv_from failed");
                }
            }
        }
        tracing::info!(stats = ?self.stats, "LoginApp UDP server stopped");
        Ok(())
    }
}

/// Small helper so `loginapp` can report the exact byte form it expects for a
/// reply without exposing too many internals; used by tests and tooling.
pub fn describe_reply_layout() -> String {
    format!(
        "u16 flags=0x0000 | u8 0xFF | u32 LE len=4+payload | u32 LE reply_id | payload(status+blowfish record); \
         login element = id {} {:?}, probe = id {} {:?}",
        login_ids::LOGIN,
        LengthStyle::Variable(2),
        login_ids::PROBE,
        LengthStyle::Fixed(0),
    )
}

/// Debug helper used by `bw201-login --dump-request <file>`: decode a captured
/// raw datagram and log its structure without answering.
pub fn dump_datagram(key: &LoginKey, data: &[u8]) -> Result<String, String> {
    let packet = Packet::decode(data, Endian::Little).map_err(|e| e.to_string())?;
    let messages =
        decode_messages(&packet, &login_interface(), Endian::Little).map_err(|e| e.to_string())?;
    let mut out = String::new();
    out.push_str(&format!(
        "flags=0x{:04X} body_len={} messages={}\n",
        packet.flags,
        packet.body.len(),
        messages.len()
    ));
    for m in messages {
        out.push_str(&format!(
            "  id={} len={} request={:?}\n",
            m.id,
            m.data.len(),
            m.request
        ));
        if m.id == login_ids::LOGIN {
            match parse_login_args(&m.data, key) {
                Ok(req) => out.push_str(&format!(
                    "  version={} encrypted={} rsa_offset={} username={:?} password={:?} key={} nonce=0x{:X}\n",
                    req.version,
                    req.encrypted,
                    req.rsa_offset,
                    req.params.username_str().unwrap_or("<non-utf8>"),
                    req.params.password_str().unwrap_or("<non-utf8>"),
                    hex::encode(&req.params.encryption_key),
                    req.params.nonce,
                )),
                Err(e) => out.push_str(&format!("  login parse error: {e}\n")),
            }
        }
    }
    Ok(out)
}

/// Utility: parse a `ip:port` string into a `Mercury::Address`.
pub fn parse_mercury_address(s: &str) -> Result<Address, String> {
    let addr: SocketAddr = s.parse().map_err(|e: std::net::AddrParseError| e.to_string())?;
    match addr {
        SocketAddr::V4(v4) => Ok(Address::from_socket(v4)),
        SocketAddr::V6(_) => Err("IPv6 is not supported by BigWorld Mercury".to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bundle::ElementEncoder;
    use crate::interfaces::LengthStyle;
    use crate::login::LogOnParams;
    use crate::stream::BinaryWriter;
    use std::net::{Ipv4Addr, SocketAddrV4};

    fn setup() -> (LoginApp, LoginKey) {
        let key = LoginKey::generate(2048).unwrap();
        let config = LoginAppConfig {
            bind: SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::LOCALHOST, 0)),
            base_app: SocketAddrV4::new(Ipv4Addr::LOCALHOST, 20017),
            session_key: 0x0BADF00D,
            session_out: None,
        };
        let app = LoginApp::bind(config, key.clone()).unwrap();
        (app, key)
    }

    /// Build a client-side login datagram in the observed 0.6.5 layout:
    /// version | u32 build constant | RSA cipher text.
    fn client_login_datagram(
        public: &rsa::RsaPublicKey,
        reply_id: u32,
        params: &LogOnParams,
    ) -> Vec<u8> {
        let mut clear = BinaryWriter::new();
        params.write_to(&mut clear);
        let mut rng = rand::thread_rng();
        let padding = rsa::Oaep::new::<sha1::Sha1>();
        let cipher = public
            .encrypt(&mut rng, padding, clear.as_slice())
            .unwrap();
        let mut args = Vec::new();
        args.extend_from_slice(&57u32.to_le_bytes());
        args.extend_from_slice(&0xCA030334u32.to_le_bytes());
        args.extend_from_slice(&cipher);
        let mut enc = ElementEncoder::new();
        enc.request(login_ids::LOGIN, LengthStyle::Variable(2), reply_id, &args);
        enc.finish().encode(Endian::Little)
    }

    fn sample_params(key: &[u8]) -> LogOnParams {
        LogOnParams {
            flags: 1,
            username: b"test@test.com".to_vec(),
            password: b"test123".to_vec(),
            encryption_key: key.to_vec(),
            digest: Some([0x42; 16]),
            nonce: 0x4233,
        }
    }

    #[test]
    fn handles_synthetic_login_and_builds_decryptable_reply() {
        let (mut app, login_key) = setup();
        let enc_key: Vec<u8> = (0..16).map(|i| i as u8 * 3).collect();
        let params = sample_params(&enc_key);
        let datagram = client_login_datagram(login_key.public_key(), 0x11223344, &params);
        let from: SocketAddr = "127.0.0.1:55555".parse().unwrap();
        let (reply, outcome) = app.handle_datagram(&datagram, from).expect("reply");
        assert_eq!(outcome.version, 57);
        assert_eq!(outcome.username, "test@test.com");
        assert_eq!(outcome.session_key, 0x0BADF00D);

        // Reply packet: flags 0, id 0xFF, u32 len, echoed reply id, status 1.
        assert_eq!(&reply[0..2], &[0x00, 0x00]);
        assert_eq!(reply[2], 0xFF);
        assert_eq!(&reply[3..7], &21u32.to_le_bytes());
        assert_eq!(&reply[7..11], &0x11223344u32.to_le_bytes());
        assert_eq!(reply[11], LogOnStatus::LoggedOn.as_u8());

        // Decrypt the 16-byte record with the client's key.
        let bf = crate::crypto::BlowfishKey::new(&enc_key).unwrap();
        let record = bf.stream_decrypt(&reply[12..]).unwrap();
        assert_eq!(&record[0..4], &[127, 0, 0, 1]);
        assert_eq!(u16::from_be_bytes([record[4], record[5]]), 20017);
        assert_eq!(u16::from_le_bytes([record[6], record[7]]), 0);
        assert_eq!(
            u32::from_le_bytes([record[8], record[9], record[10], record[11]]),
            0x0BADF00D
        );
    }

    #[test]
    fn malformed_payload_gets_failure_reply() {
        let (mut app, _key) = setup();
        // A version and garbage: nothing decrypts.
        let args = vec![57, 0, 0, 0, 1, 2, 3, 4];
        let mut enc = ElementEncoder::new();
        enc.request(login_ids::LOGIN, LengthStyle::Variable(2), 7, &args);
        let datagram = enc.finish().encode(Endian::Little);
        let from: SocketAddr = "127.0.0.1:55556".parse().unwrap();
        let (reply, _) = app.handle_datagram(&datagram, from).expect("failure reply");
        assert_eq!(&reply[0..2], &[0x00, 0x00]);
        assert_eq!(reply[2], 0xFF);
        assert_eq!(&reply[7..11], &7u32.to_le_bytes());
        assert_eq!(reply[11], LogOnStatus::LoginMalformedRequest.as_u8());
        // Failure payload then carries a packed string description.
        let mut r = crate::stream::BinaryReader::new(&reply[12..]);
        assert!(r.string().unwrap().contains("login parameters"));
    }

    #[test]
    fn session_json_is_written_atomically() {
        let dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../target/test-tmp")
            .join(format!(
                "bw201-session-test-{}-{:?}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            ));
        let path = dir.join("session.json");
        let s = LoginSession::new("user", &[1, 2, 3, 4], 0xAABBCCDD, "127.0.0.1:5555");
        s.write_atomic(&path).unwrap();
        // Overwriting an existing session file must also succeed cleanly.
        let s2 = LoginSession::new("user2", &[5, 6, 7, 8], 0x11223344, "127.0.0.1:5556");
        s2.write_atomic(&path).unwrap();
        let raw = std::fs::read_to_string(&path).unwrap();
        let parsed: LoginSession = serde_json::from_str(&raw).unwrap();
        assert_eq!(parsed, s2);
        assert_eq!(parsed.encryption_key(), vec![5, 6, 7, 8]);
        // No temp file left behind.
        let leftovers: Vec<_> = std::fs::read_dir(&dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_name().to_string_lossy().contains("tmp"))
            .collect();
        assert!(leftovers.is_empty());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn udp_roundtrip() {
        let (mut app, login_key) = setup();
        let addr = app.local_addr().unwrap();
        let shutdown = Arc::new(AtomicBool::new(false));
        let shutdown2 = shutdown.clone();
        let handle = std::thread::spawn(move || {
            app.run(shutdown2).unwrap();
        });

        let client = UdpSocket::bind("127.0.0.1:0").unwrap();
        client
            .set_read_timeout(Some(std::time::Duration::from_secs(5)))
            .unwrap();
        let params = sample_params(&[9u8; 16]);
        let datagram = client_login_datagram(login_key.public_key(), 0xABCDEF01, &params);
        client.send_to(&datagram, addr).unwrap();

        let mut buf = [0u8; 2048];
        let (len, from) = client.recv_from(&mut buf).unwrap();
        assert_eq!(from, addr);
        let reply = &buf[..len];
        assert_eq!(reply[2], 0xFF);
        assert_eq!(reply[11], 1);

        shutdown.store(true, Ordering::Relaxed);
        handle.join().unwrap();
    }

    #[test]
    fn unwritable_session_out_returns_failure_reply() {
        let dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../target/test-tmp")
            .join(format!(
                "bw201-unwritable-handoff-{}-{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            ));
        std::fs::create_dir_all(&dir).unwrap();
        // Using an existing regular file as a parent path forces `write_atomic` to fail.
        let blocker = dir.join("not_a_dir");
        std::fs::write(&blocker, b"blocker").unwrap();

        let key = LoginKey::generate(2048).unwrap();
        let config = LoginAppConfig {
            bind: SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::LOCALHOST, 0)),
            base_app: SocketAddrV4::new(Ipv4Addr::LOCALHOST, 20017),
            session_key: 0x0BADF00D,
            session_out: Some(blocker.join("session.json")),
        };
        let mut app = LoginApp::bind(config, key.clone()).unwrap();
        let params = sample_params(&[0x11; 16]);
        let datagram = client_login_datagram(key.public_key(), 0x22334455, &params);
        let from: SocketAddr = "127.0.0.1:55557".parse().unwrap();
        let (reply, outcome) = app.handle_datagram(&datagram, from).expect("failure reply");

        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(reply[11], LogOnStatus::LoginRejectedServerNotReady.as_u8());
        assert!(!outcome.session_written);
        assert_eq!(app.stats().logins_ok, 0);
        assert_eq!(app.stats().logins_failed, 1);
    }

    #[test]
    fn two_consecutive_logins_allocate_distinct_keys_and_preserve_pending_handoffs() {
        let dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../target/test-tmp")
            .join(format!(
                "bw201-concurrent-handoff-{}-{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            ));
        std::fs::create_dir_all(&dir).unwrap();
        let session_out = dir.join("session.json");

        let key = LoginKey::generate(2048).unwrap();
        let config = LoginAppConfig {
            bind: SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::LOCALHOST, 0)),
            base_app: SocketAddrV4::new(Ipv4Addr::LOCALHOST, 20017),
            session_key: 0x0BADF00D,
            session_out: Some(session_out.clone()),
        };
        let mut app = LoginApp::bind(config, key.clone()).unwrap();

        // Datagram 1
        let enc_key1 = vec![0x11; 16];
        let mut params1 = sample_params(&enc_key1);
        params1.username = b"player1@test.com".to_vec();
        let datagram1 = client_login_datagram(key.public_key(), 0x1111, &params1);
        let from1: SocketAddr = "127.0.0.1:50001".parse().unwrap();

        let (reply1, outcome1) = app.handle_datagram(&datagram1, from1).expect("reply 1");
        assert_eq!(outcome1.session_key, 0x0BADF00D);
        assert!(outcome1.session_written);
        assert_eq!(outcome1.username, "player1@test.com");

        // Verify reply 1 carries key 0x0BADF00D
        let bf1 = crate::crypto::BlowfishKey::new(&enc_key1).unwrap();
        let record1 = bf1.stream_decrypt(&reply1[12..]).unwrap();
        assert_eq!(
            u32::from_le_bytes([record1[8], record1[9], record1[10], record1[11]]),
            0x0BADF00D
        );

        // Datagram 2
        let enc_key2 = vec![0x22; 16];
        let mut params2 = sample_params(&enc_key2);
        params2.username = b"player2@test.com".to_vec();
        let datagram2 = client_login_datagram(key.public_key(), 0x2222, &params2);
        let from2: SocketAddr = "127.0.0.1:50002".parse().unwrap();

        let (reply2, outcome2) = app.handle_datagram(&datagram2, from2).expect("reply 2");
        assert_eq!(outcome2.session_key, 0x0BADF00Du32.wrapping_add(1));
        assert!(outcome2.session_written);
        assert_eq!(outcome2.username, "player2@test.com");

        // Verify reply 2 carries key 0x0BADF00E
        let bf2 = crate::crypto::BlowfishKey::new(&enc_key2).unwrap();
        let record2 = bf2.stream_decrypt(&reply2[12..]).unwrap();
        assert_eq!(
            u32::from_le_bytes([record2[8], record2[9], record2[10], record2[11]]),
            0x0BADF00Du32.wrapping_add(1)
        );

        // Verify session.json has latest login (player2)
        let raw_main = std::fs::read_to_string(&session_out).unwrap();
        let main_session: LoginSession = serde_json::from_str(&raw_main).unwrap();
        assert_eq!(main_session.session_key, 0x0BADF00Du32.wrapping_add(1));
        assert_eq!(main_session.username, "player2@test.com");

        // Verify BOTH pending files exist concurrently
        let pending1 = pending_session_path(&session_out, 0x0BADF00D);
        let pending2 = pending_session_path(&session_out, 0x0BADF00Du32.wrapping_add(1));
        assert!(pending1.exists(), "pending file 1 should exist");
        assert!(pending2.exists(), "pending file 2 should exist");

        let p1_raw = std::fs::read_to_string(&pending1).unwrap();
        let p1_session: LoginSession = serde_json::from_str(&p1_raw).unwrap();
        assert_eq!(p1_session.session_key, 0x0BADF00D);
        assert_eq!(p1_session.username, "player1@test.com");

        let p2_raw = std::fs::read_to_string(&pending2).unwrap();
        let p2_session: LoginSession = serde_json::from_str(&p2_raw).unwrap();
        assert_eq!(p2_session.session_key, 0x0BADF00Du32.wrapping_add(1));
        assert_eq!(p2_session.username, "player2@test.com");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn session_key_allocation_skips_zero_on_wrapping() {
        let key = LoginKey::generate(2048).unwrap();
        let mut config = LoginAppConfig {
            bind: SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::LOCALHOST, 0)),
            base_app: SocketAddrV4::new(Ipv4Addr::LOCALHOST, 20017),
            session_key: 0xFFFF_FFFF,
            session_out: None,
        };
        let mut app = LoginApp::bind(config.clone(), key.clone()).unwrap();
        assert_eq!(app.allocate_session_key(), 0xFFFF_FFFF);
        // wrapping_add(1) would be 0, so it must skip 0 and allocate 1:
        assert_eq!(app.allocate_session_key(), 1);
        assert_eq!(app.allocate_session_key(), 2);

        // If config.session_key is 0, first allocation must skip 0:
        config.session_key = 0;
        let mut app0 = LoginApp::bind(config, key).unwrap();
        assert_eq!(app0.allocate_session_key(), 1);
        assert_eq!(app0.allocate_session_key(), 2);
    }

    #[test]
    fn unwritable_pending_dir_returns_failure_reply() {
        let dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../target/test-tmp")
            .join(format!(
                "bw201-unwritable-pending-{}-{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            ));
        std::fs::create_dir_all(&dir).unwrap();
        let session_out = dir.join("session.json");
        // Create a regular file at session.json.pending so directory creation fails
        let blocker = pending_session_dir(&session_out);
        std::fs::write(&blocker, b"blocking directory creation").unwrap();

        let key = LoginKey::generate(2048).unwrap();
        let config = LoginAppConfig {
            bind: SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::LOCALHOST, 0)),
            base_app: SocketAddrV4::new(Ipv4Addr::LOCALHOST, 20017),
            session_key: 0x0BADF00D,
            session_out: Some(session_out),
        };
        let mut app = LoginApp::bind(config, key.clone()).unwrap();
        let params = sample_params(&[0x33; 16]);
        let datagram = client_login_datagram(key.public_key(), 0x3344, &params);
        let from: SocketAddr = "127.0.0.1:55558".parse().unwrap();
        let (reply, outcome) = app.handle_datagram(&datagram, from).expect("failure reply");

        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(reply[11], LogOnStatus::LoginRejectedServerNotReady.as_u8());
        assert!(!outcome.session_written);
        assert_eq!(app.stats().logins_ok, 0);
        assert_eq!(app.stats().logins_failed, 1);
    }

    #[test]
    fn pending_session_path_helpers() {
        let p = std::path::Path::new("some/nested/session.json");
        let dir = pending_session_dir(p);
        assert_eq!(dir, std::path::PathBuf::from("some/nested/session.json.pending"));
        let file = pending_session_path(p, 0x0BADF00D);
        assert_eq!(
            file,
            std::path::PathBuf::from("some/nested/session.json.pending/0badf00d.json")
        );
    }
}
