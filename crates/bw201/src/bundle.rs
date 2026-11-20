//! Bundle (message) encoding/decoding for Mercury packets.
//!
//! Sources:
//! - `src/lib/network/bundle.cpp` (`startMessage`, `startRequest`, `startReply`,
//!   `newMessage`, `endMessage`, `dispatchMessages`, iterator)
//! - `src/lib/network/interface_element.hpp|cpp` (fixed/variable length headers,
//!   special oversized-length encoding, REPLY element)
//! - `src/lib/network/interface_minder.cpp` (message ids are assigned in
//!   registration order, starting at 0)
//!
//! Layout of one element inside a packet body:
//!
//! ```text
//! message:  u8 id | [u16/u8/.. length] | payload
//! request:  u8 id | [u16/u8/.. length] | u32 reply_id | u16 next_request_offset | payload
//! reply:    u8 id=0xFF | u32 length | u32 reply_id | payload
//! ```
//!
//! For a request, the length field counts **only the payload** (the bytes after
//! the 6-byte request header); the request header sits between the element
//! header and the payload. This was confirmed against a live 0.6.5 client
//! capture (277-byte login datagram: `2 flags + 1 id + 2 len + 6 request header
//! + 264 args + 2 first-request-offset`). Some third-party write-ups claim the
//! length includes the 6 request bytes; it does not.
//!
//! `next_request_offset` and the packet footer `first_request_offset` are
//! **absolute offsets from the start of the packet data** (they include the
//! 2-byte packet header). 0 means "none".

use crate::interfaces::{InterfaceTable, LengthStyle, REPLY_MESSAGE_IDENTIFIER};
use crate::packet::{
    FirstRequestOffset, Fragment, Packet, PacketConfig, PACKET_HEADER_SIZE,
};
use crate::stream::StreamError;

/// Length used by the pseudo `REPLY` interface element: VARIABLE_LENGTH_MESSAGE
/// with a 4-byte length field (`InterfaceElement::REPLY`).
pub const REPLY_LENGTH_WIDTH: u8 = 4;

/// Error while decoding bundle messages.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum BundleError {
    #[error("message header at body offset {offset} truncated")]
    TruncatedHeader { offset: usize },
    #[error("message payload at body offset {offset} truncated (want {want}, have {have})")]
    TruncatedPayload {
        offset: usize,
        want: usize,
        have: usize,
    },
    #[error("unknown message id {0}; register it in the InterfaceTable")]
    UnknownMessage(u8),
    #[error("trailing bytes after the last message (body offset {0}, {1} bytes left)")]
    TrailingBytes(usize, usize),
    #[error("element length overflow")]
    LengthOverflow,
    #[error("stream error: {0}")]
    Stream(#[from] StreamError),
}

/// Header information for an element that is a Mercury request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RequestInfo {
    /// Client-generated reply id; the server must echo this value.
    pub reply_id: u32,
    /// Raw `next_request_offset` field (absolute packet offset), 0 = none.
    pub next_request_offset: u16,
}

/// One decoded bundle element.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DecodedMessage {
    /// Message id (0xFF for replies).
    pub id: u8,
    /// Payload bytes (does not include the element header or request header).
    pub data: Vec<u8>,
    /// `Some` if this element was a request.
    pub request: Option<RequestInfo>,
    /// Offset of the element header within the packet body.
    pub body_offset: usize,
}

impl DecodedMessage {
    /// True if this is the 0xFF reply pseudo-element.
    pub fn is_reply(&self) -> bool {
        self.id == REPLY_MESSAGE_IDENTIFIER
    }
}

/// Read a variable length field of `width` bytes in the packet byte order.
fn read_var_len(
    buf: &[u8],
    pos: usize,
    width: u8,
    order: crate::packet::Endian,
) -> Result<(u32, bool), BundleError> {
    let raw: u32 = match width {
        1 => {
            let b = *buf
                .get(pos)
                .ok_or(BundleError::TruncatedHeader { offset: pos })?;
            b as u32
        }
        2 => {
            let b = buf
                .get(pos..pos + 2)
                .ok_or(BundleError::TruncatedHeader { offset: pos })?;
            order.u16_from([b[0], b[1]]) as u32
        }
        3 => {
            let b = buf
                .get(pos..pos + 3)
                .ok_or(BundleError::TruncatedHeader { offset: pos })?;
            // BW_PACK3 is little-endian on x86.
            match order {
                crate::packet::Endian::Little => {
                    (b[0] as u32) | ((b[1] as u32) << 8) | ((b[2] as u32) << 16)
                }
                crate::packet::Endian::Big => {
                    ((b[0] as u32) << 16) | ((b[1] as u32) << 8) | (b[2] as u32)
                }
            }
        }
        4 => {
            let b = buf
                .get(pos..pos + 4)
                .ok_or(BundleError::TruncatedHeader { offset: pos })?;
            order.u32_from([b[0], b[1], b[2], b[3]])
        }
        _ => return Err(BundleError::LengthOverflow),
    };
    let max = if width >= 4 {
        u32::MAX
    } else {
        (1u32 << (8 * width)) - 1
    };
    Ok((raw, raw == max))
}

/// Decode all messages in a packet body.
///
/// This mirrors `Bundle::iterator` / `Bundle::dispatchMessages`.
pub fn decode_messages(
    packet: &Packet,
    table: &InterfaceTable,
    order: crate::packet::Endian,
) -> Result<Vec<DecodedMessage>, BundleError> {
    let body = &packet.body;
    let mut out = Vec::new();
    let mut offset: usize = 0;
    let mut next_request_raw = packet.config.first_request_offset;

    while offset < body.len() {
        let id = body[offset];
        let style = if id == REPLY_MESSAGE_IDENTIFIER {
            LengthStyle::Variable(REPLY_LENGTH_WIDTH)
        } else {
            table.style(id).ok_or(BundleError::UnknownMessage(id))?
        };

        let is_request = next_request_raw
            .map(|raw| raw as usize == offset + PACKET_HEADER_SIZE)
            .unwrap_or(false);

        let header_size = style.header_size();
        if offset + header_size > body.len() {
            return Err(BundleError::TruncatedHeader { offset });
        }

        // Decode the element length.
        let mut data_len;
        let mut special_oversize = false;
        match style {
            LengthStyle::Fixed(n) => data_len = n as usize,
            LengthStyle::Variable(width) => {
                let (raw, oversize) =
                    read_var_len(body, offset + 1, width, order)?;
                data_len = raw as usize;
                special_oversize = oversize;
            }
        }

        let mut header_end = offset + header_size;
        let mut request_info = None;

        if is_request {
            // Request header: reply id (4) + next request offset (2).
            if header_end + 6 > body.len() {
                return Err(BundleError::TruncatedHeader { offset });
            }
            let rb = &body[header_end..header_end + 4];
            let reply_id = order.u32_from([rb[0], rb[1], rb[2], rb[3]]);
            let nb = &body[header_end + 4..header_end + 6];
            let next = order.u16_from([nb[0], nb[1]]);
            request_info = Some(RequestInfo {
                reply_id,
                next_request_offset: next,
            });
            header_end += 6;

            // The length field counts only the payload. Be tolerant of senders
            // that (incorrectly) include the 6-byte request header.
            if !special_oversize
                && header_end + data_len > body.len()
                && data_len >= 6
                && header_end + data_len - 6 == body.len()
            {
                data_len -= 6;
            }
        }

        if special_oversize {
            // The length field is all-ones: the real u32 length is at the
            // start of the payload and the first four payload bytes were
            // displaced to the end of the message (InterfaceElement::specialCompressLength).
            if header_end + 4 > body.len() {
                return Err(BundleError::TruncatedHeader { offset });
            }
            let lb = &body[header_end..header_end + 4];
            let real = order.u32_from([lb[0], lb[1], lb[2], lb[3]]) as usize;
            if header_end + real + 4 > body.len() {
                return Err(BundleError::TruncatedPayload {
                    offset,
                    want: real + 4,
                    have: body.len() - header_end,
                });
            }
            let mut data = Vec::with_capacity(real);
            if real >= 4 {
                data.extend_from_slice(&body[header_end + real..header_end + real + 4]);
                data.extend_from_slice(&body[header_end + 4..header_end + real]);
            } else {
                data.extend_from_slice(&body[header_end + 4..header_end + 4 + real]);
            }
            out.push(DecodedMessage {
                id,
                data,
                request: request_info,
                body_offset: offset,
            });
            offset = header_end + real + 4;
            if let Some(info) = request_info {
                next_request_raw = (info.next_request_offset != 0)
                    .then_some(info.next_request_offset);
            }
            continue;
        }

        if header_end + data_len > body.len() {
            return Err(BundleError::TruncatedPayload {
                offset,
                want: data_len,
                have: body.len() - header_end,
            });
        }
        let data = body[header_end..header_end + data_len].to_vec();
        if let Some(info) = request_info {
            next_request_raw =
                (info.next_request_offset != 0).then_some(info.next_request_offset);
        }
        out.push(DecodedMessage {
            id,
            data,
            request: request_info,
            body_offset: offset,
        });
        offset = header_end + data_len;
    }

    if offset != body.len() {
        return Err(BundleError::TrailingBytes(offset, body.len() - offset));
    }

    Ok(out)
}

// ---------------------------------------------------------------------------
// Encoding
// ---------------------------------------------------------------------------

/// Builds a packet body out of elements and tracks the request linkage.
#[derive(Debug, Clone)]
pub struct ElementEncoder {
    body: Vec<u8>,
    config: PacketConfig,
    /// Offset of the "next request offset" u16 field of the last request.
    last_request_link: Option<usize>,
}

impl Default for ElementEncoder {
    fn default() -> Self {
        Self::new()
    }
}

impl ElementEncoder {
    pub fn new() -> Self {
        Self {
            body: Vec::new(),
            config: PacketConfig::default(),
            last_request_link: None,
        }
    }

    pub fn body(&self) -> &[u8] {
        &self.body
    }

    pub fn config(&self) -> &PacketConfig {
        &self.config
    }

    pub fn config_mut(&mut self) -> &mut PacketConfig {
        &mut self.config
    }

    /// Add a plain (non-request) message. `payload` is written verbatim (or with
    /// special oversize displacement if payload exceeds the variable length field capacity).
    pub fn message(&mut self, id: u8, style: LengthStyle, payload: &[u8]) -> &mut Self {
        self.begin_element(id, style, payload.len(), false);
        self.append_payload(style, payload);
        self
    }

    /// Add a fixed-length message, checking the payload length.
    pub fn fixed_message(&mut self, id: u8, len: u16, payload: &[u8]) -> &mut Self {
        assert_eq!(payload.len(), len as usize, "fixed message payload length");
        self.begin_element(id, LengthStyle::Fixed(len), payload.len(), false);
        self.body.extend_from_slice(payload);
        self
    }

    /// Add a request. Returns the body offset of the request element.
    pub fn request(
        &mut self,
        id: u8,
        style: LengthStyle,
        reply_id: u32,
        payload: &[u8],
    ) -> usize {
        let request_offset = self.body.len();
        self.begin_element(id, style, payload.len(), true);
        self.body.extend_from_slice(&reply_id.to_le_bytes());
        // next request offset placeholder
        let link = self.body.len();
        self.body.extend_from_slice(&0u16.to_le_bytes());
        self.append_payload(style, payload);

        // Link the previous request to this one.
        match self.last_request_link {
            Some(pos) => {
                let raw = (request_offset + PACKET_HEADER_SIZE) as u16;
                self.body[pos..pos + 2].copy_from_slice(&raw.to_le_bytes());
            }
            None => {
                self.config.first_request_offset =
                    FirstRequestOffset::from_body_offset(request_offset).map(|o| o.0);
            }
        }
        self.last_request_link = Some(link);
        self.config.extra_flags |= crate::packet::FLAG_HAS_REQUESTS;
        request_offset
    }

    /// Add a reply element (id 0xFF) echoing `reply_id`.
    pub fn reply(&mut self, reply_id: u32, payload: &[u8]) -> &mut Self {
        self.body.push(REPLY_MESSAGE_IDENTIFIER);
        let len = 4 + payload.len();
        self.body
            .extend_from_slice(&(len as u32).to_le_bytes());
        self.body.extend_from_slice(&reply_id.to_le_bytes());
        self.body.extend_from_slice(payload);
        self
    }

    fn append_payload(&mut self, style: LengthStyle, payload: &[u8]) {
        let is_oversize = match style {
            LengthStyle::Variable(w) if w < 4 => {
                let max = (1u64 << (8 * w)) - 1;
                (payload.len() as u64) >= max
            }
            _ => false,
        };

        if is_oversize {
            assert!(
                payload.len() >= 4,
                "oversize payload must have at least 4 bytes"
            );
            let len = payload.len() as u32;
            self.body.extend_from_slice(&len.to_le_bytes());
            self.body.extend_from_slice(&payload[4..]);
            self.body.extend_from_slice(&payload[0..4]);
        } else {
            self.body.extend_from_slice(payload);
        }
    }

    fn begin_element(&mut self, id: u8, style: LengthStyle, payload_len: usize, request: bool) {
        self.body.push(id);
        match style {
            LengthStyle::Fixed(_) => {}
            LengthStyle::Variable(width) => {
                let len = payload_len as u32;
                let max = if width >= 4 {
                    u32::MAX
                } else {
                    (1u32 << (8 * width)) - 1
                };
                let is_oversize = width < 4 && len >= max;
                if is_oversize {
                    for _ in 0..width {
                        self.body.push(0xFF);
                    }
                } else {
                    match width {
                        1 => self.body.push(len as u8),
                        2 => self.body.extend_from_slice(&(len as u16).to_le_bytes()),
                        3 => {
                            self.body.push(len as u8);
                            self.body.push((len >> 8) as u8);
                            self.body.push((len >> 16) as u8);
                        }
                        4 => self.body.extend_from_slice(&len.to_le_bytes()),
                        _ => panic!("unsupported variable length width {width}"),
                    }
                }
            }
        }
        let _ = request;
    }

    /// Finish: build a `Packet`. The caller may still adjust the config (e.g.
    /// channel flags, sequence numbers) before encoding.
    pub fn finish(self) -> Packet {
        let mut p = Packet::new(self.body, self.config);
        p.finalize_flags();
        p
    }
}

/// Convenience helper: encode a single request into a full datagram (LE).
pub fn encode_single_request(
    id: u8,
    style: LengthStyle,
    reply_id: u32,
    payload: &[u8],
) -> Vec<u8> {
    let mut enc = ElementEncoder::new();
    enc.request(id, style, reply_id, payload);
    enc.finish().encode(crate::packet::Endian::Little)
}

/// Default conservative maximum body payload per fragmented packet so footers
/// and `EncryptionFilter` padding fit comfortably below `PACKET_MAX_SIZE`.
pub const DEFAULT_MAX_FRAGMENT_BODY: usize = 1200;

/// Split a bundle body across one or more packets for a channel send, tagging
/// every multi-packet fragment with `FLAG_IS_FRAGMENT` (`begin..=end`) and
/// consecutive sequence numbers starting at `first_seq`.
pub fn split_bundle_packets(
    body: &[u8],
    base_config: PacketConfig,
    first_seq: u32,
    max_body: usize,
) -> Vec<Packet> {
    assert!(max_body > 0, "max_body must be positive");
    if body.len() <= max_body {
        let mut cfg = base_config;
        cfg.sequence = Some(first_seq);
        cfg.fragment = None;
        let mut p = Packet::new(body.to_vec(), cfg);
        p.finalize_flags();
        return vec![p];
    }

    let chunks: Vec<&[u8]> = body.chunks(max_body).collect();
    let begin = first_seq;
    let end = first_seq.wrapping_add((chunks.len() - 1) as u32);
    let frag = Some(Fragment { begin, end });

    chunks
        .into_iter()
        .enumerate()
        .map(|(i, chunk)| {
            let mut cfg = if i == 0 {
                base_config.clone()
            } else {
                PacketConfig {
                    channel: base_config.channel,
                    checksum: base_config.checksum,
                    extra_flags: base_config.extra_flags,
                    ..Default::default()
                }
            };
            cfg.sequence = Some(first_seq.wrapping_add(i as u32));
            cfg.fragment = frag;
            let mut p = Packet::new(chunk.to_vec(), cfg);
            p.finalize_flags();
            p
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::interfaces::{login_interface, InterfaceElement};
    use crate::packet::Endian;

    /// The live 0.6.5 capture shape: 2 flags + 1 id + 2 len + 6 request header
    /// + args + 2 first-request-offset = 277 bytes for 264 arg bytes.
    #[test]
    fn request_element_matches_capture_shape() {
        let args = vec![0xAAu8; 264];
        let datagram = encode_single_request(
            0,
            LengthStyle::Variable(2),
            47868,
            &args,
        );
        assert_eq!(datagram.len(), 277);
        assert_eq!(&datagram[..2], &[0x01, 0x00]); // HAS_REQUESTS LE
        assert_eq!(datagram[2], 0x00); // login id
        assert_eq!(&datagram[3..5], &[0x08, 0x01]); // len 264 LE
        assert_eq!(
            &datagram[5..9],
            &47868u32.to_le_bytes(),
            "reply id at body offset 3"
        );
        assert_eq!(&datagram[9..11], &[0x00, 0x00]);
        assert_eq!(&datagram[275..], &[0x02, 0x00]);

        let p = Packet::decode(&datagram, Endian::Little).unwrap();
        let msgs = decode_messages(&p, &login_interface(), Endian::Little).unwrap();
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0].id, 0);
        assert_eq!(msgs[0].request.unwrap().reply_id, 47868);
        assert_eq!(msgs[0].request.unwrap().next_request_offset, 0);
        assert_eq!(msgs[0].data, args);
    }

    #[test]
    fn reply_element_layout() {
        let mut enc = ElementEncoder::new();
        enc.reply(0x1234, &[0x01, 0xAA, 0xBB]);
        let p = enc.finish();
        assert_eq!(p.flags, 0);
        assert_eq!(
            p.body,
            vec![0xFF, 7, 0, 0, 0, 0x34, 0x12, 0, 0, 0x01, 0xAA, 0xBB]
        );
        let bytes = p.encode(Endian::Little);
        assert_eq!(&bytes[..2], &[0x00, 0x00]);
    }

    #[test]
    fn two_requests_are_chained() {
        let mut enc = ElementEncoder::new();
        let first = enc.request(0, LengthStyle::Variable(2), 11, &[1, 2, 3]);
        let second = enc.request(1, LengthStyle::Fixed(0), 22, &[]);
        let p = enc.finish();
        // first-request-offset footer points at the first request (body 0 + 2).
        assert_eq!(p.config.first_request_offset, Some(2));
        // First request's next-request-offset must point at the second one.
        let link_pos = first + 1 + 2 + 4; // id+len+replyID
        let link = u16::from_le_bytes([p.body[link_pos], p.body[link_pos + 1]]);
        assert_eq!(link as usize, second + PACKET_HEADER_SIZE);

        let bytes = p.encode(Endian::Little);
        let q = Packet::decode(&bytes, Endian::Little).unwrap();
        let table = login_interface();
        let msgs = decode_messages(&q, &table, Endian::Little).unwrap();
        assert_eq!(msgs.len(), 2);
        assert_eq!(msgs[0].request.unwrap().reply_id, 11);
        assert_eq!(
            msgs[0].request.unwrap().next_request_offset as usize,
            second + PACKET_HEADER_SIZE
        );
        assert_eq!(msgs[1].request.unwrap().reply_id, 22);
    }

    #[test]
    fn fixed_message_roundtrip() {
        let mut table = InterfaceTable::new();
        table.insert(InterfaceElement::fixed(5, 4, "authenticate"));
        let mut enc = ElementEncoder::new();
        enc.fixed_message(5, 4, &[9, 8, 7, 6]);
        let bytes = enc.finish().encode(Endian::Little);
        let p = Packet::decode(&bytes, Endian::Little).unwrap();
        let msgs = decode_messages(&p, &table, Endian::Little).unwrap();
        assert_eq!(msgs[0].data, vec![9, 8, 7, 6]);
        assert!(msgs[0].request.is_none());
    }

    #[test]
    fn unknown_message_rejected() {
        let mut enc = ElementEncoder::new();
        enc.message(200, LengthStyle::Variable(2), &[1]);
        let bytes = enc.finish().encode(Endian::Little);
        let p = Packet::decode(&bytes, Endian::Little).unwrap();
        assert!(matches!(
            decode_messages(&p, &login_interface(), Endian::Little),
            Err(BundleError::UnknownMessage(200))
        ));
    }

    #[test]
    fn split_single_packet_bundle() {
        use crate::packet::{FLAG_CREATE_CHANNEL, FLAG_IS_FRAGMENT, FLAG_IS_RELIABLE, FLAG_ON_CHANNEL};

        let mut enc = ElementEncoder::new();
        enc.message(0, LengthStyle::Variable(2), &[1, 2, 3, 4, 5]);
        let mut cfg = enc.config().clone();
        cfg.extra_flags = FLAG_ON_CHANNEL | FLAG_IS_RELIABLE;
        let packets = split_bundle_packets(enc.body(), cfg, 42, 100);
        assert_eq!(packets.len(), 1);
        let p = &packets[0];
        assert_eq!(p.config.sequence, Some(42));
        assert!(p.config.fragment.is_none());
        assert_ne!(p.flags & FLAG_ON_CHANNEL, 0);
        assert_ne!(p.flags & FLAG_IS_RELIABLE, 0);
        assert_eq!(p.flags & FLAG_CREATE_CHANNEL, 0);
        assert_eq!(p.flags & FLAG_IS_FRAGMENT, 0);
        let raw = p.encode(Endian::Little);
        let decoded = Packet::decode(&raw, Endian::Little).unwrap();
        let msgs = decode_messages(&decoded, &login_interface(), Endian::Little).unwrap();
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0].data, vec![1, 2, 3, 4, 5]);
    }

    #[test]
    fn split_multi_packet_fragment_bundle() {
        use crate::packet::{FLAG_CREATE_CHANNEL, FLAG_IS_FRAGMENT, FLAG_IS_RELIABLE, FLAG_ON_CHANNEL};

        let payload: Vec<u8> = (0..260u16).map(|i| (i & 0xFF) as u8).collect();
        let mut enc = ElementEncoder::new();
        enc.message(0, LengthStyle::Variable(2), &payload);
        let mut cfg = enc.config().clone();
        cfg.extra_flags = FLAG_ON_CHANNEL | FLAG_IS_RELIABLE;
        let packets = split_bundle_packets(enc.body(), cfg, 10, 100);
        assert_eq!(packets.len(), 3);

        let mut reassembled_body = Vec::new();
        for (i, pkt) in packets.iter().enumerate() {
            assert_ne!(pkt.flags & FLAG_IS_FRAGMENT, 0);
            assert_ne!(pkt.flags & FLAG_ON_CHANNEL, 0);
            assert_eq!(pkt.flags & FLAG_CREATE_CHANNEL, 0);
            let raw = pkt.encode(Endian::Little);
            let decoded = Packet::decode(&raw, Endian::Little).unwrap();
            assert_eq!(decoded.config.sequence, Some(10 + i as u32));
            assert_eq!(
                decoded.config.fragment,
                Some(Fragment {
                    begin: 10,
                    end: 12,
                })
            );
            reassembled_body.extend_from_slice(&decoded.body);
        }

        let synthetic = Packet::new(reassembled_body, PacketConfig::default());
        let msgs = decode_messages(&synthetic, &login_interface(), Endian::Little).unwrap();
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0].data, payload);
    }

    #[test]
    fn oversize_variable_element_roundtrip() {
        let mut table = InterfaceTable::new();
        table.insert(InterfaceElement::variable2(5, "bigMessage"));
        let mut enc = ElementEncoder::new();
        let payload = vec![0x42u8; 70_000];
        enc.message(5, LengthStyle::Variable(2), &payload);
        let bytes = enc.finish().encode(Endian::Little);
        let p = Packet::decode(&bytes, Endian::Little).unwrap();
        let msgs = decode_messages(&p, &table, Endian::Little).unwrap();
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0].id, 5);
        assert_eq!(msgs[0].data.len(), 70_000);
        assert_eq!(msgs[0].data, payload);
    }
}
