//! Reliable UDP Mercury channel state: ordering, fragmentation, ACKs and retransmit.

use std::collections::{BTreeMap, BTreeSet};
use std::time::{Duration, Instant};

use bw201::bundle::{split_bundle_packets, DEFAULT_MAX_FRAGMENT_BODY};
use bw201::crypto::BlowfishKey;
use bw201::packet::{
    Endian, Packet, PacketConfig, FLAG_HAS_REQUESTS, FLAG_IS_RELIABLE, FLAG_ON_CHANNEL,
};

/// 28-bit Mercury sequence mask (`Channel::SEQ_MASK`).
pub const SEQ_MASK: u32 = 0x0FFF_FFFF;

/// Maximum individual ACKs to pack per outgoing packet.
const MAX_ACKS_PER_PACKET: usize = 16;

/// Maximum automatic retransmissions per unacked reliable packet before giving up.
const MAX_RETRIES: u32 = 20;

/// BigWorld half-window 28-bit modular sequence comparison (`Channel::seqLessThan`).
pub fn seq_less_than(a: u32, b: u32) -> bool {
    let diff = b.wrapping_sub(a) & SEQ_MASK;
    diff != 0 && diff < 0x0800_0000
}

/// Output of ingesting a single decoded inbound Mercury packet.
#[derive(Debug, Default)]
pub struct ReceiveResult {
    /// Zero or more complete packet payloads ready for `decode_messages`, in order.
    pub packets: Vec<Packet>,
    /// True if at least one reliable packet (new or duplicate) needs acknowledgment.
    pub ack_needed: bool,
}

#[derive(Debug, Clone)]
struct UnackedPacket {
    plaintext: Vec<u8>,
    sent_at: Instant,
    retries: u32,
}

#[derive(Debug, Default)]
struct FragmentAssembly {
    first_request_offset: Option<u16>,
    chunks: BTreeMap<u32, Vec<u8>>,
}

/// Per-peer Mercury reliable channel state (BaseApp External UDP).
#[derive(Debug)]
pub struct ChannelState {
    in_seq_at: u32,
    next_out_seq: u32,
    next_unreliable_out_seq: u32,
    pending_acks: BTreeSet<u32>,
    buffered_in: BTreeMap<u32, Packet>,
    fragment_buffers: BTreeMap<(u32, u32), FragmentAssembly>,
    unacked: BTreeMap<u32, UnackedPacket>,
}

impl Default for ChannelState {
    fn default() -> Self {
        Self::new()
    }
}

impl ChannelState {
    pub fn new() -> Self {
        Self {
            in_seq_at: 0,
            next_out_seq: 0,
            next_unreliable_out_seq: 1,
            pending_acks: BTreeSet::new(),
            buffered_in: BTreeMap::new(),
            fragment_buffers: BTreeMap::new(),
            unacked: BTreeMap::new(),
        }
    }

    pub fn in_seq_at(&self) -> u32 {
        self.in_seq_at
    }

    pub fn next_out_seq(&self) -> u32 {
        self.next_out_seq
    }

    pub fn unacked_len(&self) -> usize {
        self.unacked.len()
    }

    /// Process inbound ACKs and packet ordering/reassembly for one decoded packet.
    pub fn receive(&mut self, packet: Packet) -> ReceiveResult {
        self.process_inbound_acks(&packet);

        let mut res = ReceiveResult::default();
        let is_reliable_sequenced =
            (packet.flags & FLAG_IS_RELIABLE) != 0 && packet.config.sequence.is_some();

        if !is_reliable_sequenced {
            if !packet.body.is_empty() {
                if let Some(complete) = self.reassemble_fragment(packet) {
                    res.packets.push(complete);
                }
            }
            return res;
        }

        let seq = packet.config.sequence.unwrap() & SEQ_MASK;
        res.ack_needed = true;

        if seq_less_than(seq, self.in_seq_at) {
            // Duplicate of a previously received packet: caller should re-ACK cleanly.
            return res;
        }

        self.pending_acks.insert(seq);

        if seq != self.in_seq_at {
            self.buffered_in.entry(seq).or_insert(packet);
            return res;
        }

        self.deliver_ordered(packet, &mut res.packets);
        while let Some(next_pkt) = self.buffered_in.remove(&self.in_seq_at) {
            self.deliver_ordered(next_pkt, &mut res.packets);
        }

        // Any pending ACK strictly below `in_seq_at` is covered by `cumulative_ack`.
        self.pending_acks
            .retain(|&s| !seq_less_than(s, self.in_seq_at));
        res
    }

    fn process_inbound_acks(&mut self, packet: &Packet) {
        if let Some(end_seq_raw) = packet.config.cumulative_ack {
            let end_seq = end_seq_raw & SEQ_MASK;
            if !seq_less_than(self.next_out_seq, end_seq) {
                self.unacked
                    .retain(|&seq, _| !seq_less_than(seq, end_seq));
            }
        }
        for &ack in &packet.config.acks {
            self.unacked.remove(&(ack & SEQ_MASK));
        }
    }

    fn deliver_ordered(&mut self, packet: Packet, out: &mut Vec<Packet>) {
        self.in_seq_at = (self.in_seq_at + 1) & SEQ_MASK;
        if !packet.body.is_empty() {
            if let Some(complete) = self.reassemble_fragment(packet) {
                out.push(complete);
            }
        }
    }

    fn reassemble_fragment(&mut self, packet: Packet) -> Option<Packet> {
        let Some(frag) = packet.config.fragment else {
            return Some(packet);
        };
        let begin = frag.begin & SEQ_MASK;
        let end = frag.end & SEQ_MASK;
        let count = end.wrapping_sub(begin).wrapping_add(1) & SEQ_MASK;
        let seq = packet.config.sequence.unwrap_or(begin) & SEQ_MASK;
        let idx = seq.wrapping_sub(begin) & SEQ_MASK;
        if count <= 1 || idx >= count {
            return Some(packet);
        }

        let key = (begin, end);
        let entry = self.fragment_buffers.entry(key).or_default();
        if idx == 0 {
            entry.first_request_offset = packet.config.first_request_offset;
        }
        entry.chunks.insert(idx, packet.body);

        if entry.chunks.len() < count as usize {
            return None;
        }

        let assembled = self.fragment_buffers.remove(&key)?;
        let mut body = Vec::new();
        for chunk in assembled.chunks.into_values() {
            body.extend_from_slice(&chunk);
        }
        let mut cfg = PacketConfig {
            first_request_offset: assembled.first_request_offset,
            extra_flags: FLAG_ON_CHANNEL | FLAG_IS_RELIABLE,
            ..Default::default()
        };
        if cfg.first_request_offset.is_some() {
            cfg.extra_flags |= FLAG_HAS_REQUESTS;
        }
        let mut combined = Packet::new(body, cfg);
        combined.finalize_flags();
        Some(combined)
    }

    fn drain_out_of_order_acks(&mut self) -> Vec<u32> {
        let mut taken = Vec::new();
        while taken.len() < MAX_ACKS_PER_PACKET {
            let Some(&first) = self.pending_acks.iter().next() else {
                break;
            };
            self.pending_acks.remove(&first);
            taken.push(first);
        }
        taken
    }

    /// Frame `body` into one or more reliable on-channel datagrams (`seq` starting at `next_out_seq`),
    /// encrypt with Blowfish, and retain plaintext in `unacked` until ACKed.
    ///
    /// Never sets `FLAG_CREATE_CHANNEL`.
    pub fn send_reliable(
        &mut self,
        body: &[u8],
        first_request_offset: Option<u16>,
        blowfish: &BlowfishKey,
        now: Instant,
    ) -> Vec<Vec<u8>> {
        let acks = self.drain_out_of_order_acks();
        let mut base_config = PacketConfig {
            first_request_offset,
            cumulative_ack: Some(self.in_seq_at),
            acks,
            extra_flags: FLAG_ON_CHANNEL | FLAG_IS_RELIABLE,
            ..Default::default()
        };
        if first_request_offset.is_some() {
            base_config.extra_flags |= FLAG_HAS_REQUESTS;
        }

        let packets =
            split_bundle_packets(body, base_config, self.next_out_seq, DEFAULT_MAX_FRAGMENT_BODY);
        self.next_out_seq = (self.next_out_seq.wrapping_add(packets.len() as u32)) & SEQ_MASK;

        let mut encrypted = Vec::with_capacity(packets.len());
        for pkt in packets {
            let seq = pkt.config.sequence.expect("reliable packet has sequence") & SEQ_MASK;
            let plain = pkt.encode(Endian::Little);
            self.unacked.insert(
                seq,
                UnackedPacket {
                    plaintext: plain.clone(),
                    sent_at: now,
                    retries: 0,
                },
            );
            encrypted.push(blowfish.packet_encrypt(&plain));
        }
        encrypted
    }

    fn alloc_unreliable_seq(&mut self) -> u32 {
        let seq = self.next_unreliable_out_seq;
        self.next_unreliable_out_seq = self.next_unreliable_out_seq.wrapping_add(1) & SEQ_MASK;
        seq
    }

    /// Build a standalone ACK-only on-channel packet (unreliable, consumes no reliable seq,
    /// but carries a valid external unreliable sequence number so BW 2.0.1 accepts it).
    pub fn send_ack_only(&mut self, blowfish: &BlowfishKey) -> Vec<u8> {
        let cfg = PacketConfig {
            sequence: Some(self.alloc_unreliable_seq()),
            cumulative_ack: Some(self.in_seq_at),
            acks: self.drain_out_of_order_acks(),
            extra_flags: FLAG_ON_CHANNEL,
            ..Default::default()
        };
        let mut pkt = Packet::new(Vec::new(), cfg);
        pkt.finalize_flags();
        blowfish.packet_encrypt(&pkt.encode(Endian::Little))
    }

    /// Frame an unreliable on-channel datagram (battle movement / position updates).
    /// Caps the body at `DEFAULT_MAX_FRAGMENT_BODY` (respecting ACK footer room) so
    /// the encrypted datagram never exceeds UDP MTU (`PACKET_MAX_SIZE`).
    /// Consumes `next_unreliable_out_seq`, leaves `next_out_seq` and `unacked` untouched,
    /// and piggybacks `cumulative_ack` + pending ACKs.
    pub fn send_unreliable(&mut self, blowfish: &BlowfishKey, payload: &[u8]) -> Vec<u8> {
        let acks = self.drain_out_of_order_acks();
        let max_body = Self::unreliable_body_limit(acks.len());
        let bounded = &payload[..payload.len().min(max_body)];
        let cfg = PacketConfig {
            sequence: Some(self.alloc_unreliable_seq()),
            cumulative_ack: Some(self.in_seq_at),
            acks,
            extra_flags: FLAG_ON_CHANNEL,
            ..Default::default()
        };
        let mut pkt = Packet::new(bounded.to_vec(), cfg);
        pkt.finalize_flags();
        blowfish.packet_encrypt(&pkt.encode(Endian::Little))
    }

    /// Pack whole unreliable message payloads into one or more on-channel UDP datagrams,
    /// keeping every datagram body `<= DEFAULT_MAX_FRAGMENT_BODY` (within UDP MTU).
    pub fn send_unreliable_batch(
        &mut self,
        blowfish: &BlowfishKey,
        messages: &[&[u8]],
    ) -> Vec<Vec<u8>> {
        let mut out = Vec::new();
        let mut batch = Vec::new();
        let mut current_limit =
            Self::unreliable_body_limit(self.pending_acks.len().min(MAX_ACKS_PER_PACKET));

        for msg in messages {
            if msg.is_empty() {
                continue;
            }
            if !batch.is_empty() && batch.len() + msg.len() > current_limit {
                out.push(self.send_unreliable(blowfish, &batch));
                batch.clear();
                current_limit =
                    Self::unreliable_body_limit(self.pending_acks.len().min(MAX_ACKS_PER_PACKET));
            }
            if msg.len() > current_limit {
                if !batch.is_empty() {
                    out.push(self.send_unreliable(blowfish, &batch));
                    batch.clear();
                }
                out.push(self.send_unreliable(blowfish, msg));
                current_limit =
                    Self::unreliable_body_limit(self.pending_acks.len().min(MAX_ACKS_PER_PACKET));
            } else {
                batch.extend_from_slice(msg);
            }
        }
        if !batch.is_empty() {
            out.push(self.send_unreliable(blowfish, &batch));
        }
        out
    }

    fn unreliable_body_limit(ack_count: usize) -> usize {
        let ack_footer = if ack_count == 0 {
            0
        } else {
            1 + 4 * ack_count.min(MAX_ACKS_PER_PACKET)
        };
        DEFAULT_MAX_FRAGMENT_BODY
            .min(bw201::packet::PACKET_MAX_SIZE.saturating_sub(32 + ack_footer))
    }

    /// Return Blowfish-encrypted retransmits for any unacked packets older than `rto`.
    pub fn due_retransmits(
        &mut self,
        blowfish: &BlowfishKey,
        now: Instant,
        rto: Duration,
    ) -> Vec<Vec<u8>> {
        let mut out = Vec::new();
        for unacked in self.unacked.values_mut() {
            if unacked.retries < MAX_RETRIES && now.saturating_duration_since(unacked.sent_at) >= rto
            {
                unacked.sent_at = now;
                unacked.retries += 1;
                out.push(blowfish.packet_encrypt(&unacked.plaintext));
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bw201::packet::{FLAG_CREATE_CHANNEL, FLAG_HAS_SEQUENCE_NUMBER, FLAG_IS_FRAGMENT};

    fn test_key() -> BlowfishKey {
        BlowfishKey::new(b"0123456789abcdef").unwrap()
    }

    #[test]
    fn outbound_reliable_starts_at_zero_and_never_sets_create_channel() {
        let mut ch = ChannelState::new();
        let key = test_key();
        let now = Instant::now();
        let datagrams = ch.send_reliable(&[0u8, 1, 2, 3, 4], None, &key, now);
        assert_eq!(datagrams.len(), 1);
        let plain = key.packet_decrypt(&datagrams[0]).unwrap();
        let pkt = Packet::decode(&plain, Endian::Little).unwrap();
        assert_ne!(pkt.flags & FLAG_ON_CHANNEL, 0);
        assert_ne!(pkt.flags & FLAG_IS_RELIABLE, 0);
        assert_ne!(pkt.flags & FLAG_HAS_SEQUENCE_NUMBER, 0);
        assert_eq!(pkt.flags & FLAG_CREATE_CHANNEL, 0);
        assert_eq!(pkt.config.sequence, Some(0));
        assert_eq!(ch.next_out_seq(), 1);
        assert_eq!(ch.unacked_len(), 1);
    }

    #[test]
    fn duplicate_inbound_is_not_redelivered_and_acks_clear_unacked() {
        let mut ch = ChannelState::new();
        let key = test_key();
        let now = Instant::now();

        let _ = ch.send_reliable(b"pkt0", None, &key, now);
        let _ = ch.send_reliable(b"pkt1", None, &key, now);
        assert_eq!(ch.unacked_len(), 2);

        let mut p0 = Packet::new(
            b"hello".to_vec(),
            PacketConfig {
                sequence: Some(0),
                cumulative_ack: Some(1),
                extra_flags: FLAG_ON_CHANNEL | FLAG_IS_RELIABLE,
                ..Default::default()
            },
        );
        p0.finalize_flags();
        let res1 = ch.receive(p0.clone());
        assert_eq!(res1.packets.len(), 1);
        assert!(res1.ack_needed);
        assert_eq!(ch.in_seq_at(), 1);
        assert_eq!(ch.unacked_len(), 1); // seq 0 acked (< 1), seq 1 still unacked

        // Retransmitted seq 0 with individual ack for seq 1
        p0.config.acks = vec![1];
        p0.config.cumulative_ack = None;
        let res_dup = ch.receive(p0);
        assert!(res_dup.packets.is_empty());
        assert!(res_dup.ack_needed);
        assert_eq!(ch.unacked_len(), 0);
    }

    #[test]
    fn unacked_packets_retransmit_after_rto() {
        let mut ch = ChannelState::new();
        let key = test_key();
        let t0 = Instant::now();
        let _ = ch.send_reliable(b"pkt0", None, &key, t0);
        assert!(ch
            .due_retransmits(&key, t0 + Duration::from_millis(50), Duration::from_millis(200))
            .is_empty());
        let resends = ch.due_retransmits(
            &key,
            t0 + Duration::from_millis(250),
            Duration::from_millis(200),
        );
        assert_eq!(resends.len(), 1);
        let plain = key.packet_decrypt(&resends[0]).unwrap();
        let pkt = Packet::decode(&plain, Endian::Little).unwrap();
        assert_eq!(pkt.config.sequence, Some(0));
    }

    #[test]
    fn large_bundle_fragments_outbound_and_reassembles_inbound() {
        let mut sender = ChannelState::new();
        let mut receiver = ChannelState::new();
        let key = test_key();
        let payload: Vec<u8> = (0..3000u32).map(|i| (i & 0xFF) as u8).collect();

        let datagrams = sender.send_reliable(&payload, None, &key, Instant::now());
        assert_eq!(datagrams.len(), 3); // 1200 + 1200 + 600

        let mut delivered = Vec::new();
        // Deliver out of order (2, 0, 1) to test both buffering and reassembly.
        for idx in [2usize, 0, 1] {
            let plain = key.packet_decrypt(&datagrams[idx]).unwrap();
            let pkt = Packet::decode(&plain, Endian::Little).unwrap();
            assert_ne!(pkt.flags & FLAG_IS_FRAGMENT, 0);
            let res = receiver.receive(pkt);
            delivered.extend(res.packets);
        }

        assert_eq!(delivered.len(), 1);
        assert_eq!(delivered[0].body, payload);
        assert_eq!(receiver.in_seq_at(), 3);
    }

    #[test]
    fn ack_only_packets_carry_monotonic_unreliable_sequence_numbers() {
        let mut ch = ChannelState::new();
        let key = test_key();

        let d1 = ch.send_ack_only(&key);
        let d2 = ch.send_ack_only(&key);

        let p1 = Packet::decode(&key.packet_decrypt(&d1).unwrap(), Endian::Little).unwrap();
        let p2 = Packet::decode(&key.packet_decrypt(&d2).unwrap(), Endian::Little).unwrap();

        for p in [&p1, &p2] {
            assert_ne!(p.flags & FLAG_ON_CHANNEL, 0);
            assert_eq!(p.flags & FLAG_IS_RELIABLE, 0);
            assert_ne!(p.flags & FLAG_HAS_SEQUENCE_NUMBER, 0);
        }
        let s1 = p1.config.sequence.expect("first ack-only packet needs seq");
        let s2 = p2.config.sequence.expect("second ack-only packet needs seq");
        assert!(s1 <= SEQ_MASK && s2 <= SEQ_MASK);
        assert!(seq_less_than(s1, s2));
        assert_eq!(ch.next_out_seq(), 0);
    }

    #[test]
    fn unreliable_packets_do_not_advance_reliable_seq_or_buffer_unacked() {
        let mut ch = ChannelState::new();
        let key = test_key();

        let d1 = ch.send_unreliable(&key, b"pos-1");
        let d2 = ch.send_unreliable(&key, b"pos-2");

        let p1 = Packet::decode(&key.packet_decrypt(&d1).unwrap(), Endian::Little).unwrap();
        let p2 = Packet::decode(&key.packet_decrypt(&d2).unwrap(), Endian::Little).unwrap();

        assert_eq!(p1.body, b"pos-1");
        assert_eq!(p2.body, b"pos-2");
        for p in [&p1, &p2] {
            assert_ne!(p.flags & FLAG_ON_CHANNEL, 0);
            assert_eq!(p.flags & FLAG_IS_RELIABLE, 0);
            assert_ne!(p.flags & FLAG_HAS_SEQUENCE_NUMBER, 0);
        }
        let s1 = p1.config.sequence.expect("first unreliable packet needs seq");
        let s2 = p2.config.sequence.expect("second unreliable packet needs seq");
        assert!(seq_less_than(s1, s2));
        assert_eq!(ch.next_out_seq(), 0);
        assert_eq!(ch.unacked_len(), 0);
    }

    #[test]
    fn unreliable_oversized_batch_splits_within_mtu_without_touching_reliable_seq() {
        let mut ch = ChannelState::new();
        let key = test_key();

        // Direct oversized send_unreliable caps at DEFAULT_MAX_FRAGMENT_BODY.
        let giant = vec![0xAAu8; DEFAULT_MAX_FRAGMENT_BODY + 500];
        let capped_wire = ch.send_unreliable(&key, &giant);
        assert!(capped_wire.len() <= bw201::packet::PACKET_MAX_SIZE);
        let capped_pkt =
            Packet::decode(&key.packet_decrypt(&capped_wire).unwrap(), Endian::Little).unwrap();
        assert_eq!(capped_pkt.body.len(), DEFAULT_MAX_FRAGMENT_BODY);

        // Batch of 50 x 32-byte messages = 1600 bytes splits across 2 datagrams <= MTU.
        let item = vec![0x42u8; 32];
        let items: Vec<&[u8]> = (0..50).map(|_| item.as_slice()).collect();
        let batch_wires = ch.send_unreliable_batch(&key, &items);
        assert_eq!(batch_wires.len(), 2);

        let mut total_body = 0usize;
        let mut prev_seq = capped_pkt.config.sequence.unwrap();
        for wire in &batch_wires {
            assert!(wire.len() <= bw201::packet::PACKET_MAX_SIZE);
            let pkt = Packet::decode(&key.packet_decrypt(wire).unwrap(), Endian::Little).unwrap();
            assert!(pkt.body.len() <= DEFAULT_MAX_FRAGMENT_BODY);
            assert_eq!(pkt.flags & FLAG_IS_RELIABLE, 0);
            let seq = pkt.config.sequence.unwrap();
            assert!(seq_less_than(prev_seq, seq));
            prev_seq = seq;
            total_body += pkt.body.len();
        }
        assert_eq!(total_body, 50 * 32);
        assert_eq!(ch.next_out_seq(), 0);
        assert_eq!(ch.unacked_len(), 0);
    }
}
