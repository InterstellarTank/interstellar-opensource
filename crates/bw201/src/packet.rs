//! Mercury packet framing for BigWorld 2.0.1 / WoT 0.6.5 (`LOGIN_VERSION` 56).
//!
//! Sources:
//! - `src/lib/network/packet.hpp|cpp` — flags, header size, footer packing.
//! - `src/lib/network/network_interface.cpp` (`NetworkInterface::send`) — footer write order.
//! - `src/lib/network/packet_receiver.cpp` (`processFilteredPacket`) — footer strip order.
//! - `src/lib/network/channel.cpp` (`writeFlags` / `writeFooter`) — on-channel footers.
//! - `src/lib/network/binary_stream.ipp` — byte order.
//!
//! A packet is:
//!
//! ```text
//! +--------+-----------------------------------------+-----------------------------+
//! | flags  | bundle messages / element data          | footers (written backwards) |
//! +--------+-----------------------------------------+-----------------------------+
//!  2 bytes                 body                              variable
//! ```
//!
//! `Packet::HEADER_SIZE == sizeof(Flags) == 2`. **No 4-byte prefix exists in
//! BigWorld 2.0.1** (version 43 of the protocol made headers 2 bytes; the
//! 4-byte prefix seen in modern WoT 1.x/2.x is a later addition and must not be
//! used for 0.6.5).
//!
//! BigWorld streams all integers little-endian on x86 (`BW_HTONS`/`BW_HTONL`
//! are identity macros on little-endian hosts). Out of caution this module
//! supports both byte orders via [`Endian`]; the default is [`Endian::Little`]
//! and the LoginApp auto-detects what the real client sends.
//!
//! Footer order *on the wire*, low address to high address, after the body:
//!
//! ```text
//! [fragment begin u32][fragment end u32]        if FLAG_IS_FRAGMENT
//! [first request offset u16]                    if FLAG_HAS_REQUESTS
//! [sequence number u32]                         if FLAG_HAS_SEQUENCE_NUMBER
//! [ack0 u32][ack1 u32]...[ackN u32][ack count u8] if FLAG_HAS_ACKS
//! [cumulative ack u32]                          if FLAG_HAS_CUMULATIVE_ACK
//! [channel version u32][channel id u32]         if FLAG_INDEXED_CHANNEL
//! [piggyback data][piggyback len i16]...        if FLAG_HAS_PIGGYBACKS
//! [checksum u32]                                if FLAG_HAS_CHECKSUM
//! ```
//!
//! This is the reverse of the order the C++ `packFooter()` calls are made in
//! `NetworkInterface::send`, because `packFooter` walks backwards from the end
//! of the packet. The `first request offset` value includes the 2-byte packet
//! header (i.e. the body-relative offset of the first request plus 2), and the
//! "next request offset" stored in a request element also includes it; a value
//! of 0 means "no next request".

use crate::stream::StreamError;

// ---------------------------------------------------------------------------
// Flags
// ---------------------------------------------------------------------------

pub const FLAG_HAS_REQUESTS: u16 = 0x0001;
pub const FLAG_HAS_PIGGYBACKS: u16 = 0x0002;
pub const FLAG_HAS_ACKS: u16 = 0x0004;
pub const FLAG_ON_CHANNEL: u16 = 0x0008;
pub const FLAG_IS_RELIABLE: u16 = 0x0010;
pub const FLAG_IS_FRAGMENT: u16 = 0x0020;
pub const FLAG_HAS_SEQUENCE_NUMBER: u16 = 0x0040;
pub const FLAG_INDEXED_CHANNEL: u16 = 0x0080;
pub const FLAG_HAS_CHECKSUM: u16 = 0x0100;
pub const FLAG_CREATE_CHANNEL: u16 = 0x0200;
pub const FLAG_HAS_CUMULATIVE_ACK: u16 = 0x0400;
/// `KNOWN_FLAGS` in BigWorld 2.0.1 `packet.hpp`.
pub const KNOWN_FLAGS: u16 = 0x07FF;

pub const PACKET_MAX_SIZE: usize = 1472;
pub const PACKET_HEADER_SIZE: usize = 2;

pub const ENCRYPTION_MAGIC: u32 = 0xDEADBEEF;

/// Byte order used for packet-level integers.
///
/// BigWorld 2.0.1 is little-endian on the wire; [`Endian::Big`] exists only so
/// the LoginApp can mirror a client that was built with a big-endian network
/// order assumption (this is detected on the first packet and logged).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Endian {
    #[default]
    Little,
    Big,
}

impl Endian {
    pub fn u16_from(self, b: [u8; 2]) -> u16 {
        match self {
            Endian::Little => u16::from_le_bytes(b),
            Endian::Big => u16::from_be_bytes(b),
        }
    }

    pub fn u16_to(self, v: u16) -> [u8; 2] {
        match self {
            Endian::Little => v.to_le_bytes(),
            Endian::Big => v.to_be_bytes(),
        }
    }

    pub fn u32_from(self, b: [u8; 4]) -> u32 {
        match self {
            Endian::Little => u32::from_le_bytes(b),
            Endian::Big => u32::from_be_bytes(b),
        }
    }

    pub fn u32_to(self, v: u32) -> [u8; 4] {
        match self {
            Endian::Little => v.to_le_bytes(),
            Endian::Big => v.to_be_bytes(),
        }
    }

    pub fn i16_from(self, b: [u8; 2]) -> i16 {
        self.u16_from(b) as i16
    }

    pub fn i16_to(self, v: i16) -> [u8; 2] {
        self.u16_to(v as u16)
    }

    /// Best-effort guess of the packet byte order from the flags word of a
    /// datagram whose first packet carries only `FLAG_HAS_REQUESTS` (the login
    /// request sent by the retail client has flags `0x0001`).
    ///
    /// Returns `None` when the datagram is too short.
    pub fn sniff(datagram: &[u8]) -> Option<Endian> {
        if datagram.len() < 2 {
            return None;
        }
        let (a, b) = (datagram[0], datagram[1]);
        // Little-endian 0x0001 -> [0x01, 0x00]; big-endian -> [0x00, 0x01].
        if b == 0x00 && a != 0x00 {
            Some(Endian::Little)
        } else if a == 0x00 && b != 0x00 {
            Some(Endian::Big)
        } else {
            None
        }
    }
}

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PacketError {
    #[error("packet too short: {0} bytes")]
    TooShort(usize),
    #[error("packet has unknown flags 0x{0:04X}")]
    UnknownFlags(u16),
    #[error("packet footer is corrupted: {0}")]
    Corrupted(&'static str),
    #[error("stream error while decoding packet: {0}")]
    Stream(#[from] StreamError),
}

// ---------------------------------------------------------------------------
// Packet
// ---------------------------------------------------------------------------

/// A decoded/encodable Mercury packet.
///
/// `body` holds the bundle element bytes (everything between the 2-byte flags
/// word and the footers). `config` holds the footer fields.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Packet {
    /// Raw flags word. Maintained by [`Packet::finalize_flags`]; callers may set
    /// extra flags directly (e.g. channel flags).
    pub flags: u16,
    /// Bundle element bytes.
    pub body: Vec<u8>,
    /// Footer configuration.
    pub config: PacketConfig,
}

/// Footer values carried by a packet.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ChannelRef {
    pub id: u16,
    pub version: u32,
}

/// Fragment footer pair (`FLAG_IS_FRAGMENT`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Fragment {
    pub begin: u32,
    pub end: u32,
}

/// Footer configuration of a packet. Fields are `Option`s; a `Some` implies the
/// matching flag and footer will be written.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct PacketConfig {
    /// Raw value of the first-request-offset footer (includes the 2-byte packet
    /// header, as BigWorld writes it). `None` means no requests on this packet.
    pub first_request_offset: Option<u16>,
    /// Reliable/on-channel sequence number (`FLAG_HAS_SEQUENCE_NUMBER`).
    pub sequence: Option<u32>,
    /// Single ACKs to piggyback (each one is a sequence number).
    pub acks: Vec<u32>,
    /// Cumulative ACK footer value (`FLAG_HAS_CUMULATIVE_ACK`).
    pub cumulative_ack: Option<u32>,
    /// Indexed channel footer (`FLAG_INDEXED_CHANNEL`).
    pub channel: Option<ChannelRef>,
    /// Fragment footer (`FLAG_IS_FRAGMENT`).
    pub fragment: Option<Fragment>,
    /// Piggybacked packets; each entry is a full packet image (`flags + body +
    /// footers`, i.e. exactly the bytes as they appeared on the wire).
    pub piggybacks: Vec<Vec<u8>>,
    /// Packet checksum footer. When `Some`, the checksum is recomputed on
    /// encode; when decoding, this holds the checksum found on the wire.
    pub checksum: Option<u32>,
    /// Extra raw flags that are not implied by the fields above (e.g.
    /// `FLAG_ON_CHANNEL`, `FLAG_IS_RELIABLE`, `FLAG_CREATE_CHANNEL`).
    pub extra_flags: u16,
}

impl PacketConfig {
    pub fn new() -> Self {
        Self::default()
    }

    /// Return the flag bits implied by this configuration.
    pub fn implied_flags(&self) -> u16 {
        let mut f = self.extra_flags;
        if self.first_request_offset.is_some() {
            f |= FLAG_HAS_REQUESTS;
        }
        if self.sequence.is_some() {
            f |= FLAG_HAS_SEQUENCE_NUMBER;
        }
        if !self.acks.is_empty() {
            f |= FLAG_HAS_ACKS;
        }
        if self.cumulative_ack.is_some() {
            f |= FLAG_HAS_CUMULATIVE_ACK;
        }
        if self.channel.is_some() {
            f |= FLAG_INDEXED_CHANNEL;
        }
        if self.fragment.is_some() {
            f |= FLAG_IS_FRAGMENT;
        }
        if !self.piggybacks.is_empty() {
            f |= FLAG_HAS_PIGGYBACKS;
        }
        if self.checksum.is_some() {
            f |= FLAG_HAS_CHECKSUM;
        }
        f
    }
}

/// Location of the first request inside the body, used by [`crate::bundle`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FirstRequestOffset(pub u16);

impl FirstRequestOffset {
    /// `first_request_offset` footer value for a request element that starts at
    /// `body_offset` (0-based within the body).
    pub fn from_body_offset(body_offset: usize) -> Option<Self> {
        let raw = body_offset + PACKET_HEADER_SIZE;
        u16::try_from(raw).ok().map(FirstRequestOffset)
    }

    /// 0-based offset of the request inside the body.
    pub fn body_offset(self) -> usize {
        (self.0 as usize).saturating_sub(PACKET_HEADER_SIZE)
    }
}

impl Packet {
    /// Create a packet with the given body and footer config.
    pub fn new(body: Vec<u8>, mut config: PacketConfig) -> Self {
        let mut packet = Packet {
            flags: 0,
            body,
            config: PacketConfig::default(),
        };
        std::mem::swap(&mut packet.config, &mut config);
        packet
    }

    fn take_end<'a>(tail: &mut usize, buf: &'a [u8], n: usize) -> Result<&'a [u8], PacketError> {
        if *tail < n || *tail < PACKET_HEADER_SIZE {
            return Err(PacketError::Corrupted("footer overruns packet start"));
        }
        *tail -= n;
        Ok(&buf[*tail..*tail + n])
    }

    /// Decode a full datagram.
    pub fn decode(datagram: &[u8], order: Endian) -> Result<Packet, PacketError> {
        if datagram.len() < PACKET_HEADER_SIZE + 1 {
            return Err(PacketError::TooShort(datagram.len()));
        }

        let flags = order.u16_from([datagram[0], datagram[1]]);
        if flags & !KNOWN_FLAGS != 0 {
            return Err(PacketError::UnknownFlags(flags & !KNOWN_FLAGS));
        }

        // Strip footers from the back. `tail` starts at the end of the datagram.
        let mut tail = datagram.len();
        let mut config = PacketConfig::default();
        let mut checksum_found = None;

        if flags & FLAG_HAS_CHECKSUM != 0 {
            let b = Self::take_end(&mut tail, datagram, 4)?;
            let expected = order.u32_from([b[0], b[1], b[2], b[3]]);
            // Checksum is XOR of all 4-byte words from packet start up to the
            // checksum offset with trailing bytes beyond the checksum offset zeroed
            // (BigWorld `Packet::validateChecksum`).
            let sum = xor_checksum(&datagram[..tail], order);
            if sum != expected {
                return Err(PacketError::Corrupted("checksum mismatch"));
            }
            checksum_found = Some(expected);
        }

        // Piggybacks: one or more [payload len][payload] pairs from the back;
        // the last one has its length one's-complemented.
        if flags & FLAG_HAS_PIGGYBACKS != 0 {
            let mut done = false;
            let mut piggies_rev = Vec::new();
            while !done {
                let lb = Self::take_end(&mut tail, datagram, 2)?;
                let raw = order.i16_from([lb[0], lb[1]]);
                let (len, last) = if raw < 0 {
                    ((!raw) as u16 as usize, true)
                } else {
                    (raw as u16 as usize, false)
                };
                done = last;
                let payload = Self::take_end(&mut tail, datagram, len)?;
                piggies_rev.push(payload.to_vec());
            }
            piggies_rev.reverse();
            config.piggybacks = piggies_rev;
        }

        // Indexed channel: channel id (nearest the back) then version.
        if flags & FLAG_INDEXED_CHANNEL != 0 {
            let b = Self::take_end(&mut tail, datagram, 4)?;
            let id = order.u32_from([b[0], b[1], b[2], b[3]]) as u16;
            let b = Self::take_end(&mut tail, datagram, 4)?;
            let version = order.u32_from([b[0], b[1], b[2], b[3]]);
            config.channel = Some(ChannelRef { id, version });
        }

        // Cumulative ACK.
        if flags & FLAG_HAS_CUMULATIVE_ACK != 0 {
            let b = Self::take_end(&mut tail, datagram, 4)?;
            config.cumulative_ack = Some(order.u32_from([b[0], b[1], b[2], b[3]]));
        }

        // Single ACKs: count byte nearest the back, then each u32 (the first
        // one read from the back is the first one written by the sender).
        if flags & FLAG_HAS_ACKS != 0 {
            let b = Self::take_end(&mut tail, datagram, 1)?;
            let count = b[0];
            if count == 0 {
                return Err(PacketError::Corrupted("FLAG_HAS_ACKS with 0 acks"));
            }
            let mut acks = Vec::with_capacity(count as usize);
            for _ in 0..count {
                let b = Self::take_end(&mut tail, datagram, 4)?;
                acks.push(order.u32_from([b[0], b[1], b[2], b[3]]));
            }
            config.acks = acks;
        }

        // Sequence number.
        if flags & FLAG_HAS_SEQUENCE_NUMBER != 0 {
            let b = Self::take_end(&mut tail, datagram, 4)?;
            config.sequence = Some(order.u32_from([b[0], b[1], b[2], b[3]]));
        }

        // First request offset.
        if flags & FLAG_HAS_REQUESTS != 0 {
            let b = Self::take_end(&mut tail, datagram, 2)?;
            config.first_request_offset = Some(order.u16_from([b[0], b[1]]));
        }

        // Fragment begin/end: end is nearest the back, begin before it.
        if flags & FLAG_IS_FRAGMENT != 0 {
            let b = Self::take_end(&mut tail, datagram, 4)?;
            let end = order.u32_from([b[0], b[1], b[2], b[3]]);
            let b = Self::take_end(&mut tail, datagram, 4)?;
            let begin = order.u32_from([b[0], b[1], b[2], b[3]]);
            if begin >= end {
                return Err(PacketError::Corrupted("invalid fragment range"));
            }
            config.fragment = Some(Fragment { begin, end });
        }

        if tail < PACKET_HEADER_SIZE {
            return Err(PacketError::Corrupted("footers consumed packet header"));
        }

        config.checksum = checksum_found;
        let body = datagram[PACKET_HEADER_SIZE..tail].to_vec();

        Ok(Packet {
            flags,
            body,
            config,
        })
    }

    /// Update [`Packet::flags`] from the footer configuration.
    pub fn finalize_flags(&mut self) {
        self.flags = self.config.implied_flags();
    }

    /// Encode this packet into wire bytes.
    pub fn encode(&self, order: Endian) -> Vec<u8> {
        let config = &self.config;
        let flags = config.implied_flags() | (self.flags & !KNOWN_FLAGS);

        let mut out = Vec::with_capacity(PACKET_HEADER_SIZE + self.body.len() + 64);
        out.extend_from_slice(&order.u16_to(flags));
        out.extend_from_slice(&self.body);

        // Footers are appended from the front-most to the back-most as they
        // appear on the wire.
        if let Some(frag) = config.fragment {
            out.extend_from_slice(&order.u32_to(frag.begin));
            out.extend_from_slice(&order.u32_to(frag.end));
        }
        if let Some(off) = config.first_request_offset {
            out.extend_from_slice(&order.u16_to(off));
        }
        if let Some(seq) = config.sequence {
            out.extend_from_slice(&order.u32_to(seq));
        }
        if !config.acks.is_empty() {
            for ack in config.acks.iter().rev() {
                out.extend_from_slice(&order.u32_to(*ack));
            }
            out.push(config.acks.len() as u8);
        }
        if let Some(ca) = config.cumulative_ack {
            out.extend_from_slice(&order.u32_to(ca));
        }
        if let Some(ch) = config.channel {
            out.extend_from_slice(&order.u32_to(ch.version));
            out.extend_from_slice(&order.u32_to(ch.id as u32));
        }
        if !config.piggybacks.is_empty() {
            for (i, piggy) in config.piggybacks.iter().enumerate() {
                let last = i + 1 == config.piggybacks.len();
                let len = piggy.len() as u16;
                let wire_len = if last { !len } else { len };
                out.extend_from_slice(piggy);
                out.extend_from_slice(&order.i16_to(wire_len as i16));
            }
        }
        if flags & FLAG_HAS_CHECKSUM != 0 {
            // Checksum is XOR of all 4-byte words from packet start up to
            // (excluding) the checksum field, with the field itself zeroed.
            let sum = xor_checksum(&out, order);
            out.extend_from_slice(&order.u32_to(sum));
        }

        out
    }

    /// Total wire size if encoded.
    pub fn wire_len(&self, order: Endian) -> usize {
        self.encode(order).len()
    }
}

fn xor_checksum(bytes: &[u8], order: Endian) -> u32 {
    let mut sum = 0u32;
    for chunk in bytes.chunks(4) {
        let mut word = [0u8; 4];
        word[..chunk.len()].copy_from_slice(chunk);
        sum ^= order.u32_from(word);
    }
    sum
}

// ---------------------------------------------------------------------------
// Packet-level Blowfish filter (EncryptionFilter::send/recv equivalent)
// ---------------------------------------------------------------------------

/// Errors for the packet encryption filter.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum FilterError {
    #[error("ciphertext length is not a multiple of the Blowfish block size")]
    NotBlockAligned,
    #[error("packet encryption magic mismatch (expected 0xDEADBEEF)")]
    BadMagic,
    #[error("illegal wastage count {0}")]
    BadWastage(u8),
    #[error("ciphertext too short")]
    TooShort,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn flags_sniff() {
        assert_eq!(Endian::sniff(&[0x01, 0x00, 0xAA]), Some(Endian::Little));
        assert_eq!(Endian::sniff(&[0x00, 0x01, 0xAA]), Some(Endian::Big));
        assert_eq!(Endian::sniff(&[0x11, 0x22]), None);
        assert_eq!(Endian::sniff(&[0x01]), None);
    }

    #[test]
    fn first_request_offset_semantics() {
        // A request at body offset 0 sits at packet offset 2 -> footer value 2.
        let off = FirstRequestOffset::from_body_offset(0).unwrap();
        assert_eq!(off.0, 2);
        assert_eq!(off.body_offset(), 0);
        let off = FirstRequestOffset::from_body_offset(7).unwrap();
        assert_eq!(off.0, 9);
        assert_eq!(off.body_offset(), 7);
    }

    #[test]
    fn packet_roundtrip_no_footers() {
        let p = Packet::new(vec![0xFF, 1, 2, 3], PacketConfig::default());
        let bytes = p.encode(Endian::Little);
        assert_eq!(&bytes[..2], &[0x00, 0x00]);
        let q = Packet::decode(&bytes, Endian::Little).unwrap();
        assert_eq!(q.flags, 0);
        assert_eq!(q.body, vec![0xFF, 1, 2, 3]);
        assert!(q.config.acks.is_empty());
        assert!(q.config.first_request_offset.is_none());
    }

    #[test]
    fn packet_roundtrip_all_footers() {
        let mut cfg = PacketConfig {
            first_request_offset: Some(12),
            sequence: Some(0x11223344),
            acks: vec![7, 9, 11],
            cumulative_ack: Some(42),
            channel: Some(ChannelRef {
                id: 0x1234,
                version: 0x0000_0005,
            }),
            fragment: Some(Fragment {
                begin: 10,
                end: 12,
            }),
            piggybacks: vec![vec![0x01, 0x00, 0xAA, 0xBB]],
            ..Default::default()
        };
        cfg.extra_flags = FLAG_ON_CHANNEL | FLAG_IS_RELIABLE;
        let mut p = Packet::new(vec![0x00, 0x05, 0x68, 0x65, 0x6C, 0x6C, 0x6F], cfg);
        p.finalize_flags();
        let bytes = p.encode(Endian::Little);
        let q = Packet::decode(&bytes, Endian::Little).unwrap();
        assert_eq!(q.flags, p.flags);
        assert_eq!(q.body, p.body);
        assert_eq!(q.config.first_request_offset, Some(12));
        assert_eq!(q.config.sequence, Some(0x11223344));
        assert_eq!(q.config.acks, vec![7, 9, 11]);
        assert_eq!(q.config.cumulative_ack, Some(42));
        assert_eq!(q.config.channel.unwrap().id, 0x1234);
        assert_eq!(q.config.channel.unwrap().version, 5);
        assert_eq!(q.config.fragment.unwrap(), Fragment { begin: 10, end: 12 });
        assert_eq!(q.config.piggybacks, vec![vec![0x01, 0x00, 0xAA, 0xBB]]);
    }

    #[test]
    fn packet_roundtrip_big_endian_mirror() {
        let mut cfg = PacketConfig {
            first_request_offset: Some(2),
            ..Default::default()
        };
        cfg.extra_flags = FLAG_ON_CHANNEL;
        let mut p = Packet::new(vec![0x00, 0x03, 0x04, 0x00, 0xAA, 0xBB], cfg);
        p.finalize_flags();
        let le = p.encode(Endian::Little);
        let be = p.encode(Endian::Big);
        assert_ne!(le, be);
        assert_eq!(Packet::decode(&be, Endian::Big).unwrap().body, p.body);
        assert_eq!(
            Packet::decode(&le, Endian::Little)
                .unwrap()
                .config
                .first_request_offset,
            Some(2)
        );
    }

    #[test]
    fn checksum_footer() {
        let cfg = PacketConfig {
            checksum: Some(0),
            ..Default::default()
        };
        let p = Packet::new(vec![0xDE, 0xAD, 0xBE, 0xEF], cfg);
        let bytes = p.encode(Endian::Little);
        let q = Packet::decode(&bytes, Endian::Little).unwrap();
        assert!(q.config.checksum.is_some());

        // Corrupt the body and expect a checksum failure.
        let mut bad = bytes.clone();
        bad[3] ^= 0xFF;
        assert!(matches!(
            Packet::decode(&bad, Endian::Little),
            Err(PacketError::Corrupted("checksum mismatch"))
        ));
    }

    #[test]
    fn unknown_flags_rejected() {
        let bytes = [0x00, 0x80, 0x00]; // 0x8000
        assert!(matches!(
            Packet::decode(&bytes, Endian::Little),
            Err(PacketError::UnknownFlags(_))
        ));
    }
}
