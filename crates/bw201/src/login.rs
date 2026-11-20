//! LoginApp login request / reply codec for the WoT 0.6.5 (BigWorld 2.0.1)
//! wire format, validated against a live retail client capture.
//!
//! # Request payload (`LoginInterface::login`, element id 0, VARIABLE(2))
//!
//! ```text
//! u32 LOGIN_VERSION            # BW 2.0.1 = 56; the retail 0.6.5 client sends 57.
//!                              # Accept any value and only log it.
//! ...
//! ```
//!
//! The live client then sends one extra little-endian `u32` (constant per
//! client build) before the RSA cipher text, so the payload is:
//!
//! ```text
//! u32 LOGIN_VERSION
//! u32 <client build constant (unknown, ignored)>
//! <RSA-OAEP-SHA1 cipher text, 256-byte chunks, no per-chunk length prefix>
//! ```
//!
//! Rather than trusting an offset, [`parse_login_args`] scans a few candidate
//! offsets for a block that decrypts under our private key, so both the stock
//! BigWorld layout (`version | rsa`) and the observed 0.6.5 layout
//! (`version | u32 | rsa`) are accepted, as well as a plaintext (unencrypted)
//! fallback.
//!
//! # `LogOnParams` plain text
//!
//! ```text
//! u8 flags                     # bit 0 = HAS_DIGEST; the client sends 0x01
//! std::string username         # packed int length + raw bytes
//! std::string password
//! std::string encryption_key   # the client's random Blowfish channel key (4..=56 bytes)
//! [16 raw MD5 digest]          # only if flags & 1
//! u32 nonce                    # little-endian
//! ```
//!
//! # Reply payload (element 0xFF, VARIABLE(4))
//!
//! ```text
//! u8 status = 1 (LOGGED_ON)
//! <16 encrypted bytes>         # Blowfish-XOR-chain(encryption_key) of the 12-byte
//!                              # LoginReplyRecord zero-padded to 16
//! ```
//!
//! `LoginReplyRecord` plain text (the layout below is the one verified against
//! the live client; it is what the reference Python prototype used to get the
//! client onto the BaseApp hop):
//!
//! ```text
//! 4 bytes ip      # raw IPv4 bytes, 127.0.0.1 -> 7f 00 00 01
//! 2 bytes port    # little-endian u16, 20017 -> 31 4e
//! 2 bytes salt    # little-endian u16, 0
//! 4 bytes session_key  # little-endian u32
//! ```
//!
//! Failure replies: `u8 status` + `std::string description` (packed int length).

use crate::crypto::{BlowfishKey, LoginKey, RsaError};
use crate::interfaces::LOGIN_VERSION;
use crate::stream::{BinaryReader, BinaryWriter, StreamError};

/// `LogOnStatus` values (`src/lib/connection/log_on_status.hpp`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum LogOnStatus {
    NotSet = 0,
    LoggedOn = 1,
    ConnectionFailed = 2,
    DnsLookupFailed = 3,
    UnknownError = 4,
    Cancelled = 5,
    AlreadyOnlineLocally = 6,
    PublicKeyLookupFailed = 7,
    LoginMalformedRequest = 64,
    LoginBadProtocolVersion = 65,
    LoginRejectedNoSuchUser = 67,
    LoginRejectedInvalidPassword = 68,
    LoginRejectedAlreadyLoggedIn = 69,
    LoginRejectedBadDigest = 70,
    LoginRejectedDbGeneralFailure = 71,
    LoginRejectedDbNotReady = 72,
    LoginRejectedIllegalCharacters = 73,
    LoginRejectedServerNotReady = 74,
    LoginRejectedUpdaterNotReady = 75,
    LoginRejectedNoBaseApps = 76,
    LoginRejectedBaseAppOverload = 77,
    LoginRejectedCellAppOverload = 78,
    LoginRejectedBaseAppTimeout = 79,
    LoginRejectedBaseAppMgrTimeout = 80,
    LoginRejectedDbMgrOverload = 81,
    LoginRejectedLoginsNotAllowed = 82,
    LoginRejectedRateLimited = 83,
    LoginCustomDefinedError = 254,
}

impl LogOnStatus {
    pub fn as_u8(self) -> u8 {
        self as u8
    }

    pub fn name(self) -> &'static str {
        match self {
            LogOnStatus::NotSet => "NOT_SET",
            LogOnStatus::LoggedOn => "LOGGED_ON",
            LogOnStatus::ConnectionFailed => "CONNECTION_FAILED",
            LogOnStatus::DnsLookupFailed => "DNS_LOOKUP_FAILED",
            LogOnStatus::UnknownError => "UNKNOWN_ERROR",
            LogOnStatus::Cancelled => "CANCELLED",
            LogOnStatus::AlreadyOnlineLocally => "ALREADY_ONLINE_LOCALLY",
            LogOnStatus::PublicKeyLookupFailed => "PUBLIC_KEY_LOOKUP_FAILED",
            LogOnStatus::LoginMalformedRequest => "LOGIN_MALFORMED_REQUEST",
            LogOnStatus::LoginBadProtocolVersion => "LOGIN_BAD_PROTOCOL_VERSION",
            LogOnStatus::LoginRejectedNoSuchUser => "LOGIN_REJECTED_NO_SUCH_USER",
            LogOnStatus::LoginRejectedInvalidPassword => "LOGIN_REJECTED_INVALID_PASSWORD",
            LogOnStatus::LoginRejectedAlreadyLoggedIn => "LOGIN_REJECTED_ALREADY_LOGGED_IN",
            LogOnStatus::LoginRejectedBadDigest => "LOGIN_REJECTED_BAD_DIGEST",
            LogOnStatus::LoginRejectedDbGeneralFailure => "LOGIN_REJECTED_DB_GENERAL_FAILURE",
            LogOnStatus::LoginRejectedDbNotReady => "LOGIN_REJECTED_DB_NOT_READY",
            LogOnStatus::LoginRejectedIllegalCharacters => "LOGIN_REJECTED_ILLEGAL_CHARACTERS",
            LogOnStatus::LoginRejectedServerNotReady => "LOGIN_REJECTED_SERVER_NOT_READY",
            LogOnStatus::LoginRejectedUpdaterNotReady => "LOGIN_REJECTED_UPDATER_NOT_READY",
            LogOnStatus::LoginRejectedNoBaseApps => "LOGIN_REJECTED_NO_BASEAPPS",
            LogOnStatus::LoginRejectedBaseAppOverload => "LOGIN_REJECTED_BASEAPP_OVERLOAD",
            LogOnStatus::LoginRejectedCellAppOverload => "LOGIN_REJECTED_CELLAPP_OVERLOAD",
            LogOnStatus::LoginRejectedBaseAppTimeout => "LOGIN_REJECTED_BASEAPP_TIMEOUT",
            LogOnStatus::LoginRejectedBaseAppMgrTimeout => "LOGIN_REJECTED_BASEAPPMGR_TIMEOUT",
            LogOnStatus::LoginRejectedDbMgrOverload => "LOGIN_REJECTED_DBMGR_OVERLOAD",
            LogOnStatus::LoginRejectedLoginsNotAllowed => "LOGIN_REJECTED_LOGINS_NOT_ALLOWED",
            LogOnStatus::LoginRejectedRateLimited => "LOGIN_REJECTED_RATE_LIMITED",
            LogOnStatus::LoginCustomDefinedError => "LOGIN_CUSTOM_DEFINED_ERROR",
        }
    }
}

/// Flags of `LogOnParams::Flags`.
pub const LOG_ON_HAS_DIGEST: u8 = 0x01;
pub const LOG_ON_HAS_ALL: u8 = 0x01;

/// Decrypted `/ parsed` login parameters.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LogOnParams {
    pub flags: u8,
    pub username: Vec<u8>,
    pub password: Vec<u8>,
    /// The client's random Blowfish channel key (`encryptionKey_`).
    pub encryption_key: Vec<u8>,
    /// 16-byte MD5 digest (present when `flags & HAS_DIGEST`).
    pub digest: Option<[u8; 16]>,
    pub nonce: u32,
}

impl LogOnParams {
    pub fn username_str(&self) -> Result<&str, StreamError> {
        std::str::from_utf8(&self.username).map_err(|_| StreamError::InvalidUtf8)
    }

    pub fn password_str(&self) -> Result<&str, StreamError> {
        std::str::from_utf8(&self.password).map_err(|_| StreamError::InvalidUtf8)
    }

    /// `LogOnParams::addToStreamInternal` (plain text form).
    pub fn write_to(&self, w: &mut BinaryWriter) {
        w.u8(self.flags);
        w.string_bytes(&self.username);
        w.string_bytes(&self.password);
        w.string_bytes(&self.encryption_key);
        if self.flags & LOG_ON_HAS_DIGEST != 0 {
            if let Some(d) = &self.digest {
                w.digest(d);
            }
        }
        w.u32(self.nonce);
    }

    /// Parse a plain-text `LogOnParams` stream. Accepts the BigWorld 2.0.1
    /// layout and tolerates an optional `context` string between the
    /// encryption key and the digest/nonce (used by later protocol versions),
    /// trying both variants until the whole buffer is consumed.
    pub fn parse(data: &[u8]) -> Result<Self, LoginError> {
        let mut last_err = LoginError::MalformedParams("empty");
        for with_context in [false, true] {
            match Self::parse_variant(data, with_context) {
                Ok(p) => return Ok(p),
                Err(e) => last_err = e,
            }
        }
        Err(last_err)
    }

    fn parse_variant(data: &[u8], with_context: bool) -> Result<Self, LoginError> {
        let mut r = BinaryReader::new(data);
        let flags = r.u8().map_err(LoginError::Stream)?;
        if flags & !LOG_ON_HAS_ALL != 0 {
            return Err(LoginError::MalformedParams("unknown LogOnParams flags"));
        }
        let username = r.string_bytes().map_err(LoginError::Stream)?;
        let password = r.string_bytes().map_err(LoginError::Stream)?;
        let encryption_key = r.string_bytes().map_err(LoginError::Stream)?;
        if with_context {
            // Later protocol versions stream a `context` string here.
            let _context = r.string_bytes().map_err(LoginError::Stream)?;
        }
        let digest = if flags & LOG_ON_HAS_DIGEST != 0 {
            Some(r.digest().map_err(LoginError::Stream)?)
        } else {
            None
        };
        let nonce = r.u32().map_err(LoginError::Stream)?;
        if !r.is_empty() {
            return Err(LoginError::MalformedParams("trailing bytes after nonce"));
        }
        Ok(Self {
            flags,
            username,
            password,
            encryption_key,
            digest,
            nonce,
        })
    }
}

/// Errors while handling the login hop.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum LoginError {
    #[error("login payload shorter than 4 bytes (LOGIN_VERSION)")]
    MissingVersion,
    #[error("RSA: {0}")]
    Rsa(#[from] RsaError),
    #[error("stream: {0}")]
    Stream(#[from] StreamError),
    #[error("malformed LogOnParams: {0}")]
    MalformedParams(&'static str),
    #[error("could not decrypt or parse LogOnParams (tried {tried} candidates)")]
    NoDecryptableParams { tried: usize },
}

/// A parsed client login request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoginRequest {
    /// `LOGIN_VERSION` sent by the client (56 stock, 57 observed on 0.6.5).
    pub version: u32,
    /// Extra little-endian u32 seen between the version and the RSA blob on the
    /// retail 0.6.5 client (kept for logging).
    pub client_build_constant: Option<u32>,
    /// Offset (within the login payload) where the decrypted blob started.
    pub rsa_offset: usize,
    /// True if the payload was RSA-encrypted.
    pub encrypted: bool,
    pub params: LogOnParams,
}

impl LoginRequest {
    /// Log a warning when the client version is not the stock value; the
    /// LoginApp never rejects on version, matching BigWorld's tolerant server.
    pub fn log_version(&self) {
        if self.version != LOGIN_VERSION {
            tracing::info!(
                version = self.version,
                stock = LOGIN_VERSION,
                "client LOGIN_VERSION differs from stock BigWorld 2.0.1 value (accepted)"
            );
        }
    }
}

/// Try to parse the `login` element payload (args only, without the 6-byte
/// request header).
///
/// This deliberately scans candidate RSA offsets instead of trusting one
/// layout: stock BigWorld 2.0.1 sends `u32 version | rsa`, the retail 0.6.5
/// client sends `u32 version | u32 build-constant | rsa`.
pub fn parse_login_args(args: &[u8], key: &LoginKey) -> Result<LoginRequest, LoginError> {
    if args.len() < 4 {
        return Err(LoginError::MissingVersion);
    }
    let version = u32::from_le_bytes([args[0], args[1], args[2], args[3]]);
    let modulus = key.modulus_len();
    let mut tried = 0usize;
    let mut last_err: Option<LoginError> = None;

    // Candidate offsets: after version (stock), after version+u32 (observed),
    // then a few more just in case a client build adds another field.
    for skip in [4usize, 8, 0, 2, 1, 3, 5, 6, 7, 9, 10, 11, 12] {
        if skip >= args.len() {
            continue;
        }
        let blob = &args[skip..];
        if blob.len() < modulus {
            continue;
        }
        // Tolerate a small trailing slack: only whole cipher chunks matter.
        let usable = (blob.len() / modulus) * modulus;
        let cipher = &blob[..usable];
        if cipher.is_empty() {
            continue;
        }
        tried += 1;
        let decrypted = match key.decrypt(cipher) {
            Ok(p) => p,
            Err(e) => {
                last_err = Some(LoginError::Rsa(e));
                continue;
            }
        };
        match LogOnParams::parse(&decrypted) {
            Ok(params) => {
                let client_build_constant = if skip == 8 {
                    Some(u32::from_le_bytes([
                        args[4], args[5], args[6], args[7],
                    ]))
                } else {
                    None
                };
                return Ok(LoginRequest {
                    version,
                    client_build_constant,
                    rsa_offset: skip,
                    encrypted: true,
                    params,
                });
            }
            Err(e) => last_err = Some(e),
        }
    }

    // Unencrypted fallback (BigWorld's `allowUnencryptedLogins`).
    if args.len() > 4 {
        if let Ok(params) = LogOnParams::parse(&args[4..]) {
            return Ok(LoginRequest {
                version,
                client_build_constant: None,
                rsa_offset: 4,
                encrypted: false,
                params,
            });
        }
    }

    let _ = last_err;
    Err(LoginError::NoDecryptableParams { tried })
}

// ---------------------------------------------------------------------------
// Reply builders
// ---------------------------------------------------------------------------

/// The 12 plain-text bytes of a `LoginReplyRecord`, using the record field
/// Wire format validated against BW 2.0.1 `Mercury::Address::writeToStream` /
/// `LoginReplyRecord`: raw IPv4 bytes, big-endian (network byte order) port,
/// little-endian salt, little-endian session key.
pub fn login_reply_record(ip: [u8; 4], port: u16, salt: u16, session_key: u32) -> [u8; 12] {
    let mut out = [0u8; 12];
    out[0..4].copy_from_slice(&ip);
    out[4..6].copy_from_slice(&port.to_be_bytes());
    out[6..8].copy_from_slice(&salt.to_le_bytes());
    out[8..12].copy_from_slice(&session_key.to_le_bytes());
    out
}

/// Build a successful login reply datagram (packet bytes, not element bytes).
///
/// `reply_id` must be the `reply_id` from the client's request. The reply uses
/// Blowfish-XOR-chain encryption with the client-supplied channel key.
pub fn build_success_reply(
    reply_id: u32,
    base_app_ip: [u8; 4],
    base_app_port: u16,
    session_key: u32,
    encryption_key: &[u8],
) -> Result<Vec<u8>, LoginError> {
    let blowfish = BlowfishKey::new(encryption_key)
        .map_err(|_| LoginError::MalformedParams("invalid Blowfish encryption key length"))?;
    let record = login_reply_record(base_app_ip, base_app_port, 0, session_key);
    let encrypted = blowfish.stream_encrypt(&record);

    let mut payload = Vec::with_capacity(1 + encrypted.len());
    payload.push(LogOnStatus::LoggedOn.as_u8());
    payload.extend_from_slice(&encrypted);

    Ok(encode_reply_packet(reply_id, &payload))
}

/// Build a failure reply datagram: `u8 status` + packed-string description.
pub fn build_failure_reply(reply_id: u32, status: LogOnStatus, description: &str) -> Vec<u8> {
    let mut w = BinaryWriter::new();
    w.u8(status.as_u8());
    w.string(description);
    encode_reply_packet(reply_id, w.as_slice())
}

/// Encode a REPLY pseudo-element (id 0xFF, 4-byte length) into a full packet
/// with flags 0 and no footers:
/// `u16 flags | u8 0xFF | u32 (4+payload) | u32 reply_id | payload`.
pub fn encode_reply_packet(reply_id: u32, payload: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(2 + 1 + 4 + 4 + payload.len());
    out.extend_from_slice(&0u16.to_le_bytes());
    out.push(0xFF);
    let len = 4 + payload.len() as u32;
    out.extend_from_slice(&len.to_le_bytes());
    out.extend_from_slice(&reply_id.to_le_bytes());
    out.extend_from_slice(payload);
    out
}

// ---------------------------------------------------------------------------
// Session handoff (LoginApp -> BaseApp process)
// ---------------------------------------------------------------------------

/// JSON session handoff written atomically for the BaseApp process.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct LoginSession {
    pub username: String,
    pub encryption_key_hex: String,
    pub session_key: u32,
    pub client_addr: String,
}

impl LoginSession {
    pub fn new(
        username: impl Into<String>,
        encryption_key: &[u8],
        session_key: u32,
        client_addr: impl Into<String>,
    ) -> Self {
        Self {
            username: username.into(),
            encryption_key_hex: crate::crypto::hex(encryption_key),
            session_key,
            client_addr: client_addr.into(),
        }
    }

    /// Blowfish key bytes for the BaseApp channel.
    pub fn encryption_key(&self) -> Vec<u8> {
        hex::decode(&self.encryption_key_hex).unwrap_or_default()
    }

    /// Atomically write the JSON file (write a temporary sibling, then rename).
    pub fn write_atomic(&self, path: &std::path::Path) -> std::io::Result<()> {
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent).map_err(|e| {
                    std::io::Error::new(
                        e.kind(),
                        format!("create parent dir {}: {e}", parent.display()),
                    )
                })?;
            }
        }
        let json = serde_json::to_string_pretty(self)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
        let mut tmp = path.to_path_buf();
        let file_name = path
            .file_name()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| "session.json".to_string());
        tmp.set_file_name(format!(".{file_name}.tmp"));

        let write_result = (|| -> std::io::Result<()> {
            use std::io::Write;
            let mut f = std::fs::File::create(&tmp).map_err(|e| {
                std::io::Error::new(
                    e.kind(),
                    format!("create temp file {}: {e}", tmp.display()),
                )
            })?;
            f.write_all(json.as_bytes())?;
            f.write_all(b"\n")?;
            f.sync_all()?;
            Ok(())
        })();
        if let Err(e) = write_result {
            let _ = std::fs::remove_file(&tmp);
            return Err(e);
        }

        if let Err(e) = rename_with_retry(&tmp, path) {
            let _ = std::fs::remove_file(&tmp);
            return Err(std::io::Error::new(
                e.kind(),
                format!("rename {} -> {}: {e}", tmp.display(), path.display()),
            ));
        }
        Ok(())
    }
}

/// Directory for pending per-session handoff files: `<session_out>.pending`.
pub fn pending_session_dir(session_out: &std::path::Path) -> std::path::PathBuf {
    let mut os = session_out.as_os_str().to_os_string();
    os.push(".pending");
    std::path::PathBuf::from(os)
}

/// Path to a pending handoff file: `<session_out>.pending/<session_key:08x>.json`.
pub fn pending_session_path(session_out: &std::path::Path, session_key: u32) -> std::path::PathBuf {
    pending_session_dir(session_out).join(format!("{session_key:08x}.json"))
}

/// Atomically write both `<session_out>` and `<session_out>.pending/<session_key:08x>.json`.
pub fn write_session_handoff(
    session_out: &std::path::Path,
    session: &LoginSession,
) -> std::io::Result<()> {
    session.write_atomic(session_out)?;
    let pending_path = pending_session_path(session_out, session.session_key);
    session.write_atomic(&pending_path)?;
    Ok(())
}

fn rename_with_retry(src: &std::path::Path, dst: &std::path::Path) -> std::io::Result<()> {
    // On Windows a concurrent reader / AV scanner can transiently hold `dst` without delete sharing.
    for attempt in 0..4 {
        match std::fs::rename(src, dst) {
            Ok(()) => return Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied && attempt < 3 => {
                std::thread::sleep(std::time::Duration::from_millis(5));
            }
            Err(e) => return Err(e),
        }
    }
    unreachable!("loop returns on final attempt")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key() -> LoginKey {
        LoginKey::generate(2048).unwrap()
    }

    fn sample_params() -> LogOnParams {
        LogOnParams {
            flags: LOG_ON_HAS_DIGEST,
            username: b"test@test.com".to_vec(),
            password: b"test123".to_vec(),
            encryption_key: (0u8..16).collect(),
            digest: Some([0xAB; 16]),
            nonce: 0x4233,
        }
    }

    #[test]
    fn logon_params_roundtrip() {
        let p = sample_params();
        let mut w = BinaryWriter::new();
        p.write_to(&mut w);
        assert_eq!(LogOnParams::parse(w.as_slice()).unwrap(), p);
    }

    #[test]
    fn logon_params_plaintext_is_60_bytes() {
        let mut w = BinaryWriter::new();
        sample_params().write_to(&mut w);
        // flags 1 + user 14 + pass 8 + key 17 + digest 16 + nonce 4 = 60
        assert_eq!(w.len(), 60);
    }

    #[test]
    fn parse_stock_layout_version_then_rsa() {
        let key = key();
        let params = sample_params();
        let mut w = BinaryWriter::new();
        params.write_to(&mut w);
        let cipher = key.encrypt(w.as_slice()).unwrap();
        let mut args = Vec::new();
        args.extend_from_slice(&LOGIN_VERSION.to_le_bytes());
        args.extend_from_slice(&cipher);
        let req = parse_login_args(&args, &key).unwrap();
        assert_eq!(req.version, LOGIN_VERSION);
        assert_eq!(req.rsa_offset, 4);
        assert!(req.encrypted);
        assert_eq!(req.params, params);
    }

    #[test]
    fn parse_observed_wot_layout_version_u32_rsa() {
        let key = key();
        let params = sample_params();
        let mut clear = BinaryWriter::new();
        params.write_to(&mut clear);
        let cipher = key.encrypt(clear.as_slice()).unwrap();
        let mut args = Vec::new();
        args.extend_from_slice(&57u32.to_le_bytes());
        args.extend_from_slice(&0xCA030334u32.to_le_bytes());
        args.extend_from_slice(&cipher);
        let req = parse_login_args(&args, &key).unwrap();
        assert_eq!(req.version, 57);
        assert_eq!(req.rsa_offset, 8);
        assert_eq!(req.client_build_constant, Some(0xCA030334));
        assert_eq!(req.params, params);
    }

    #[test]
    fn parse_plaintext_fallback() {
        let key = key();
        let params = sample_params();
        let mut args = BinaryWriter::new();
        args.u32(56);
        params.write_to(&mut args);
        let req = parse_login_args(args.as_slice(), &key).unwrap();
        assert!(!req.encrypted);
        assert_eq!(req.params, params);
    }

    #[test]
    fn success_reply_layout() {
        let reply = build_success_reply(47868, [127, 0, 0, 1], 20017, 0x12345678, &[0x11; 16])
            .unwrap();
        assert_eq!(&reply[0..2], &[0x00, 0x00]);
        assert_eq!(reply[2], 0xFF);
        assert_eq!(&reply[3..7], &21u32.to_le_bytes()); // 4 + 1 + 16
        assert_eq!(&reply[7..11], &47868u32.to_le_bytes());
        assert_eq!(reply[11], 1);
        assert_eq!(reply.len(), 28);
        let bf = crate::crypto::BlowfishKey::new(&[0x11; 16]).unwrap();
        let record = bf.stream_decrypt(&reply[12..]).unwrap();
        assert_eq!(&record[0..4], &[127, 0, 0, 1]);
        assert_eq!(&record[4..6], &20017u16.to_be_bytes());
        assert_eq!(&record[6..8], &0u16.to_le_bytes());
        assert_eq!(&record[8..12], &0x12345678u32.to_le_bytes());
    }

    #[test]
    fn write_atomic_cleans_up_temp_file_when_rename_fails() {
        let dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../target/test-tmp")
            .join(format!(
                "bw201-write-cleanup-{}-{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            ));
        // Creating a directory at the target path forces `rename(&tmp, path)` to fail
        // after `&tmp` was created and written.
        let target_dir = dir.join("session.json");
        std::fs::create_dir_all(&target_dir).unwrap();
        let tmp_sibling = dir.join(".session.json.tmp");

        let s = LoginSession::new("user", &[1, 2, 3, 4], 0xAABBCCDD, "127.0.0.1:5555");
        let err = s.write_atomic(&target_dir).expect_err("rename over dir must fail");
        let _ = err;
        let tmp_exists = tmp_sibling.exists();
        let _ = std::fs::remove_dir_all(&dir);
        assert!(!tmp_exists, "stale temporary handoff file should be removed on failure");
    }
}
