use std::io::ErrorKind;
use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4, UdpSocket};
use std::time::{Duration, Instant};

use anyhow::{bail, ensure, Context, Result};
use bw201::bundle::{decode_messages, encode_single_request, DecodedMessage, ElementEncoder};
use bw201::crypto::{BlowfishKey, BLOWFISH_DEFAULT_KEY};
use bw201::interfaces::{
    baseapp_ext_ids, client_ids, client_interface, login_ids, login_interface, InterfaceTable,
    LengthStyle,
};
use bw201::login::{LogOnParams, LogOnStatus, LOG_ON_HAS_DIGEST};
use bw201::packet::{Endian, Packet, FLAG_ON_CHANNEL, PACKET_MAX_SIZE};
use bw201::stream::{Address, BinaryReader, BinaryWriter};
use rand::{rngs::OsRng, RngCore};
use rsa::{traits::PublicKeyParts, Oaep, RsaPublicKey};
use serde::Serialize;
use sha1::Sha1;
use wot_baseapp::channel::ChannelState;

// Retail login header and entity digest from docs/research/login-request-capture.md.
const LOGIN_VERSION: u32 = 57;
const LOGIN_BUILD_WORD: u32 = 0x2eca_0334;
const ENTITY_DIGEST: [u8; 16] = [
    0x9d, 0x4e, 0x36, 0x22, 0x9a, 0xb1, 0xe0, 0xc3, 0x4a, 0x4b, 0xd6, 0x1f, 0x8d, 0x76, 0x0f, 0xfd,
];
const REQUEST_RETRY: Duration = Duration::from_secs(1);
const CHANNEL_RTO: Duration = Duration::from_millis(250);
const DISCONNECT_TIMEOUT: Duration = Duration::from_millis(750);

/// UDP datagram counts for both login hops and the BaseApp channel.
#[derive(Clone, Copy, Debug, Default, Serialize)]
pub struct WireStats {
    pub sent: u64,
    pub received: u64,
    pub retransmitted: u64,
}

pub(crate) struct Transport {
    io: DatagramIo,
    base_addr: SocketAddrV4,
    session_key: u32,
    blowfish: BlowfishKey,
    channel: ChannelState,
    client_interface: InterfaceTable,
    pending_messages: Vec<DecodedMessage>,
}

impl Transport {
    pub(crate) fn connect(
        login: SocketAddrV4,
        key: &RsaPublicKey,
        username: &str,
        password: &str,
        timeout: Duration,
    ) -> Result<Self> {
        ensure!(!timeout.is_zero(), "connection timeout must be positive");
        let deadline = Instant::now()
            .checked_add(timeout)
            .context("connection timeout is too large")?;
        let mut io = DatagramIo {
            socket: UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0))
                .context("bind emulator UDP socket")?,
            stats: WireStats::default(),
        };
        let mut channel_key = [0; BLOWFISH_DEFAULT_KEY];
        OsRng.fill_bytes(&mut channel_key);
        let nonce = OsRng.next_u32();
        let args = build_login_args(key, username, password, &channel_key, nonce)?;
        let blowfish = BlowfishKey::new(&channel_key)?;
        let (base_addr, session_key) = io
            .request(
                login,
                deadline,
                |reply_id, _| {
                    encode_single_request(
                        login_ids::LOGIN,
                        LengthStyle::Variable(2),
                        reply_id,
                        &args,
                    )
                },
                |_, issued, data| decode_login_reply(data, issued, &blowfish),
            )
            .with_context(|| format!("LoginApp login at {login}"))?;

        let mut transport = Self {
            io,
            base_addr,
            session_key,
            blowfish,
            channel: ChannelState::new(),
            client_interface: client_interface(),
            pending_messages: Vec::new(),
        };
        transport
            .authenticate_base_app(deadline)
            .with_context(|| format!("BaseApp login at {base_addr}"))?;
        Ok(transport)
    }

    fn authenticate_base_app(&mut self, deadline: Instant) -> Result<()> {
        let peer = self.base_addr;
        let session_key = self.session_key.to_le_bytes();
        let mut reply_seen = false;
        let mut authenticate_seen = false;
        self.io.request(
            peer,
            deadline,
            |reply_id, attempt| {
                let mut args = BinaryWriter::new();
                args.raw(&session_key);
                args.i32(attempt);
                // baseAppLogin is sent in plaintext off-channel. The reply starts
                // the encrypted channel and the client's reliable sequence stays at zero.
                encode_single_request(
                    baseapp_ext_ids::BASE_APP_LOGIN,
                    LengthStyle::Variable(2),
                    reply_id,
                    args.as_slice(),
                )
            },
            |io, issued, data| {
                let messages = read_channel_datagram(
                    io,
                    peer,
                    &self.blowfish,
                    &mut self.channel,
                    &self.client_interface,
                    data,
                )?;
                for message in messages {
                    if message.is_reply() {
                        if let Some(payload) = matching_reply(&message, issued)? {
                            ensure!(payload == session_key, "BaseApp login reply key mismatch");
                            reply_seen = true;
                        }
                    } else if message.id == client_ids::AUTHENTICATE {
                        validate_authenticate(&message, &session_key)?;
                        authenticate_seen = true;
                    } else {
                        self.pending_messages.push(message);
                    }
                }
                Ok((reply_seen && authenticate_seen).then_some(()))
            },
        )
    }

    pub(crate) fn base_addr(&self) -> SocketAddrV4 {
        self.base_addr
    }

    pub(crate) fn stats(&self) -> WireStats {
        self.io.stats
    }

    pub(crate) fn send(&mut self, encode: impl FnOnce(&mut ElementEncoder)) -> Result<()> {
        let mut bundle = ElementEncoder::new();
        bundle.fixed_message(
            baseapp_ext_ids::AUTHENTICATE,
            4,
            &self.session_key.to_le_bytes(),
        );
        encode(&mut bundle);
        for datagram in self.channel.send_reliable(
            bundle.body(),
            bundle.config().first_request_offset,
            &self.blowfish,
            Instant::now(),
        ) {
            self.io.send(self.base_addr, &datagram, false)?;
        }
        Ok(())
    }

    pub(crate) fn receive(&mut self, deadline: Instant) -> Result<Vec<DecodedMessage>> {
        if !self.pending_messages.is_empty() {
            return Ok(std::mem::take(&mut self.pending_messages));
        }
        while Instant::now() < deadline {
            let messages = self.pump_channel(deadline)?;
            if !messages.is_empty() {
                return Ok(messages);
            }
        }
        Ok(Vec::new())
    }

    fn pump_channel(&mut self, deadline: Instant) -> Result<Vec<DecodedMessage>> {
        let now = Instant::now();
        for datagram in self
            .channel
            .due_retransmits(&self.blowfish, now, CHANNEL_RTO)
        {
            self.io.send(self.base_addr, &datagram, true)?;
        }
        let deadline = deadline.min(now + CHANNEL_RTO);
        let Some(datagram) = self.io.receive(self.base_addr, deadline)? else {
            return Ok(Vec::new());
        };
        let mut messages = read_channel_datagram(
            &mut self.io,
            self.base_addr,
            &self.blowfish,
            &mut self.channel,
            &self.client_interface,
            &datagram,
        )?;
        for message in &messages {
            if message.id == client_ids::AUTHENTICATE {
                validate_authenticate(message, &self.session_key.to_le_bytes())?;
            }
        }
        messages.retain(|message| message.id != client_ids::AUTHENTICATE);
        Ok(messages)
    }

    pub(crate) fn disconnect(&mut self) -> Result<()> {
        self.send(|bundle| {
            bundle.fixed_message(baseapp_ext_ids::DISCONNECT_CLIENT, 1, &[0]);
        })?;
        let deadline = Instant::now() + DISCONNECT_TIMEOUT;
        // BaseApp removes the peer after disconnect. The final ACK may be lost
        // even when disconnect succeeded, so give retransmits a bounded window.
        while self.channel.unacked_len() != 0 && Instant::now() < deadline {
            self.pump_channel(deadline)?;
        }
        Ok(())
    }
}

fn validate_authenticate(message: &DecodedMessage, session_key: &[u8; 4]) -> Result<()> {
    ensure!(
        message.data.as_slice() == session_key,
        "BaseApp authenticate key mismatch"
    );
    Ok(())
}

fn read_channel_datagram(
    io: &mut DatagramIo,
    peer: SocketAddrV4,
    blowfish: &BlowfishKey,
    channel: &mut ChannelState,
    interface: &InterfaceTable,
    datagram: &[u8],
) -> Result<Vec<DecodedMessage>> {
    let plain = blowfish
        .packet_decrypt(datagram)
        .context("decrypt BaseApp datagram")?;
    let packet = Packet::decode(&plain, Endian::Little).context("decode BaseApp packet")?;
    ensure!(
        packet.flags & FLAG_ON_CHANNEL != 0,
        "off-channel BaseApp reply"
    );
    let received = channel.receive(packet);
    if received.ack_needed {
        io.send(peer, &channel.send_ack_only(blowfish), false)?;
    }
    let mut messages = Vec::new();
    for packet in received.packets {
        messages.extend(
            decode_messages(&packet, interface, Endian::Little)
                .context("decode BaseApp messages")?,
        );
    }
    Ok(messages)
}

struct DatagramIo {
    socket: UdpSocket,
    stats: WireStats,
}

impl DatagramIo {
    fn send(&mut self, peer: SocketAddrV4, datagram: &[u8], retry: bool) -> Result<()> {
        ensure!(
            datagram.len() <= PACKET_MAX_SIZE,
            "Mercury datagram exceeds {PACKET_MAX_SIZE} bytes"
        );
        let sent = self
            .socket
            .send_to(datagram, peer)
            .with_context(|| format!("send UDP datagram to {peer}"))?;
        ensure!(sent == datagram.len(), "incomplete UDP send to {peer}");
        self.stats.sent += 1;
        self.stats.retransmitted += u64::from(retry);
        Ok(())
    }

    fn receive(&mut self, peer: SocketAddrV4, deadline: Instant) -> Result<Option<Vec<u8>>> {
        // Winsock errors on truncation, so receive a full UDP datagram before
        // applying Mercury's smaller limit.
        let mut buffer = [0; u16::MAX as usize];
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Ok(None);
            }
            self.socket
                .set_read_timeout(Some(remaining))
                .context("set UDP read timeout")?;
            match self.socket.recv_from(&mut buffer) {
                Ok((len, sender)) if sender == SocketAddr::V4(peer) => {
                    self.stats.received += 1;
                    if len <= PACKET_MAX_SIZE {
                        return Ok(Some(buffer[..len].to_vec()));
                    }
                }
                Ok(_) => {}
                Err(err) => match err.kind() {
                    ErrorKind::TimedOut | ErrorKind::WouldBlock => return Ok(None),
                    // Windows reports earlier UDP ICMP errors on recv_from.
                    ErrorKind::Interrupted
                    | ErrorKind::ConnectionReset
                    | ErrorKind::ConnectionRefused => {}
                    _ => return Err(err).with_context(|| format!("receive UDP from {peer}")),
                },
            }
        }
    }

    fn request<T>(
        &mut self,
        peer: SocketAddrV4,
        deadline: Instant,
        mut encode: impl FnMut(u32, i32) -> Vec<u8>,
        mut decode: impl FnMut(&mut Self, &[u32], &[u8]) -> Result<Option<T>>,
    ) -> Result<T> {
        let first_id = OsRng.next_u32();
        let mut issued = Vec::new();
        while Instant::now() < deadline {
            let attempt = issued.len();
            let reply_id = first_id.wrapping_add(attempt as u32) & 0x7fff_ffff;
            let datagram = encode(reply_id, attempt as i32);
            self.send(peer, &datagram, attempt != 0)?;
            issued.push(reply_id);

            let retry_at = deadline.min(Instant::now() + REQUEST_RETRY);
            while let Some(datagram) = self.receive(peer, retry_at)? {
                // A valid first reply may arrive after a later request was sent.
                if let Some(response) = decode(self, &issued, &datagram)? {
                    return Ok(response);
                }
            }
        }
        bail!("timed out awaiting a matching reply")
    }
}

fn build_login_args(
    key: &RsaPublicKey,
    username: &str,
    password: &str,
    channel_key: &[u8],
    nonce: u32,
) -> Result<Vec<u8>> {
    ensure!(
        username.len().saturating_add(password.len()) <= PACKET_MAX_SIZE,
        "login credentials exceed the datagram size"
    );
    let params = LogOnParams {
        flags: LOG_ON_HAS_DIGEST,
        username: username.as_bytes().to_vec(),
        password: password.as_bytes().to_vec(),
        encryption_key: channel_key.to_vec(),
        digest: Some(ENTITY_DIGEST),
        nonce,
    };
    let mut clear = BinaryWriter::new();
    params.write_to(&mut clear);

    // OAEP-SHA1 uses 2 SHA-1 digests and 2 separator bytes per RSA block.
    let chunk_size = key
        .size()
        .checked_sub(42)
        .filter(|&size| size > 0)
        .context("LoginApp RSA modulus is too small for OAEP-SHA1")?;
    let ciphertext_len = clear.len().div_ceil(chunk_size) * key.size();
    // 13 bytes of Mercury framing and the two 4-byte login words.
    ensure!(
        ciphertext_len + 21 <= PACKET_MAX_SIZE,
        "encrypted login request exceeds the datagram size"
    );
    let mut args = BinaryWriter::with_capacity(ciphertext_len + 8);
    args.u32(LOGIN_VERSION);
    args.u32(LOGIN_BUILD_WORD);
    for chunk in clear.as_slice().chunks(chunk_size) {
        let encrypted = key
            .encrypt(&mut OsRng, Oaep::new::<Sha1>(), chunk)
            .context("encrypt LoginApp parameters")?;
        args.raw(&encrypted);
    }
    Ok(args.into_inner())
}

fn decode_login_reply(
    datagram: &[u8],
    issued: &[u32],
    key: &BlowfishKey,
) -> Result<Option<(SocketAddrV4, u32)>> {
    let packet = Packet::decode(datagram, Endian::Little).context("decode LoginApp packet")?;
    for message in decode_messages(&packet, &login_interface(), Endian::Little)? {
        let Some(payload) = matching_reply(&message, issued)? else {
            continue;
        };
        let (&status, ciphertext) = payload.split_first().context("missing LoginApp status")?;
        ensure!(
            status == LogOnStatus::LoggedOn.as_u8(),
            "LoginApp rejected login with status {status}"
        );
        ensure!(ciphertext.len() == 16, "invalid LoginApp reply length");
        let record = key
            .stream_decrypt(ciphertext)
            .context("decrypt LoginApp reply")?;
        let mut reader = BinaryReader::new(&record);
        let address = Address::read_from(&mut reader)?.to_socket();
        let session_key = reader.u32()?;
        ensure!(reader.rest() == [0; 4], "invalid LoginApp reply padding");
        let ip = address.ip();
        ensure!(
            address.port() != 0 && !ip.is_unspecified() && !ip.is_multicast() && !ip.is_broadcast(),
            "LoginApp returned an invalid BaseApp address"
        );
        ensure!(session_key != 0, "LoginApp returned a zero session key");
        return Ok(Some((address, session_key)));
    }
    Ok(None)
}

fn matching_reply<'a>(message: &'a DecodedMessage, issued: &[u32]) -> Result<Option<&'a [u8]>> {
    if !message.is_reply() {
        return Ok(None);
    }
    let mut reader = BinaryReader::new(&message.data);
    Ok(issued.contains(&reader.u32()?).then(|| reader.rest()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use bw201::crypto::LoginKey;
    use bw201::login::{encode_reply_packet, parse_login_args};

    #[test]
    fn login_request_encrypts_retail_parameters() {
        let key = LoginKey::generate(1024).unwrap();
        let args = build_login_args(
            key.public_key(),
            "emulator-user",
            "local-only",
            &[0xa3; 16],
            0x1234_5678,
        )
        .unwrap();

        assert!(args.starts_with(&[57, 0, 0, 0, 0x34, 0x03, 0xca, 0x2e]));
        assert_eq!(args.len(), 8 + 128);
        let request = parse_login_args(&args, &key).unwrap();
        assert!(request.encrypted);
        assert_eq!(request.params.username, b"emulator-user");
        assert_eq!(request.params.password, b"local-only");
        assert_eq!(request.params.encryption_key, [0xa3; 16]);
        assert_eq!(
            request.params.digest,
            Some([
                0x9d, 0x4e, 0x36, 0x22, 0x9a, 0xb1, 0xe0, 0xc3, 0x4a, 0x4b, 0xd6, 0x1f, 0x8d, 0x76,
                0x0f, 0xfd,
            ]),
        );
        assert_eq!(request.params.nonce, 0x1234_5678);
    }

    #[test]
    fn login_reply_matches_request_and_decodes_network_order_port() {
        let key = BlowfishKey::new(b"test-channel-key").unwrap();
        let mut payload = vec![1];
        payload
            .extend_from_slice(&key.stream_encrypt(&[127, 0, 0, 1, 0x4e, 0x31, 0, 0, 4, 3, 2, 1]));
        let datagram = encode_reply_packet(1234, &payload);

        assert_eq!(decode_login_reply(&datagram, &[4321], &key).unwrap(), None);
        assert_eq!(
            decode_login_reply(&datagram, &[1234, 4321], &key).unwrap(),
            Some(("127.0.0.1:20017".parse().unwrap(), 0x0102_0304))
        );
    }

    #[test]
    fn polling_retransmits_a_reliable_bundle_when_the_ack_is_lost() {
        let peer = UdpSocket::bind("127.0.0.1:0").unwrap();
        let SocketAddr::V4(base_addr) = peer.local_addr().unwrap() else {
            unreachable!()
        };
        peer.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
        let mut transport = Transport {
            io: DatagramIo {
                socket: UdpSocket::bind("127.0.0.1:0").unwrap(),
                stats: WireStats::default(),
            },
            base_addr,
            session_key: 1,
            blowfish: BlowfishKey::new(b"test-channel-key").unwrap(),
            channel: ChannelState::new(),
            client_interface: client_interface(),
            pending_messages: Vec::new(),
        };
        transport
            .send(|bundle| {
                bundle.fixed_message(baseapp_ext_ids::ENABLE_ENTITIES, 1, &[0]);
            })
            .unwrap();
        let mut original = [0; PACKET_MAX_SIZE];
        let (original_size, _) = peer.recv_from(&mut original).unwrap();
        let deadline = Instant::now() + CHANNEL_RTO + Duration::from_millis(150);
        assert!(transport.receive(deadline).unwrap().is_empty());
        let mut repeated = [0; PACKET_MAX_SIZE];
        let (repeated_size, _) = peer.recv_from(&mut repeated).unwrap();
        assert_eq!(&original[..original_size], &repeated[..repeated_size]);
        assert!(transport.stats().retransmitted > 0);
    }
}
