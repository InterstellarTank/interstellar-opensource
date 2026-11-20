//! BigWorld 2.0.1 `BinaryStream` primitives.
//!
//! Derived from the authoritative C++ source:
//! - `src/lib/cstdmf/binary_stream.hpp`
//! - `src/lib/cstdmf/binary_stream.ipp`
//! - `src/lib/cstdmf/binary_stream.cpp`
//! - `src/lib/cstdmf/md5.hpp|cpp`
//! - `src/lib/network/basictypes.hpp|cpp`
//!
//! BigWorld's *network byte order is little-endian*, not big-endian (see the
//! comment at the top of `binary_stream.ipp`). On a little-endian host all of
//! `BW_HTONS`/`BW_HTONL` are the identity macro, so every integer streamed
//! through `BinaryOStream`/`BinaryIStream` is little-endian on the wire.
//!
//! One deliberate exception is [`Address`]: it is streamed with
//! `insertRaw`/`extractRaw` for `ip` and `port` (`basictypes.cpp`) and through
//! `operator<<` only for `salt`. The resulting 8-byte wire layout is:
//!
//! ```text
//! offset 0..4   ip        raw bytes, i.e. IPv4 network order (127.0.0.1 = 7f 00 00 01)
//! offset 4..6   port      raw bytes of the network-order (big-endian) u16 port
//! offset 6..8   salt      streamed u16, little-endian on x86
//! ```
//!
//! Strings are a *packed* length followed by the raw bytes:
//! `len < 255` is one byte, otherwise `0xFF` followed by a little-endian u24
//! (`BW_PACK3` on a little-endian host writes the low three bytes in LE order).

use std::fmt;
use std::net::{Ipv4Addr, SocketAddrV4};

/// Errors produced when de-streaming malformed data.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum StreamError {
    /// Tried to read past the end of the stream.
    #[error("unexpected end of stream")]
    UnexpectedEof,
    /// A string was not valid UTF-8 (only produced by [`BinaryReader::string`]).
    #[error("string was not valid UTF-8")]
    InvalidUtf8,
}

/// A `Mercury::Address` as it is streamed by BigWorld 2.0.1.
///
/// The struct stores the address the same way the C++ `Mercury::Address` does
/// logically: `ip` is dotted-quad byte order (network order), `port` is a host
/// order integer and `salt` a host order integer. [`Address::write_to`] and
/// [`Address::read_from`] implement the historical C++ wire layout documented
/// on this module.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Address {
    /// IPv4 address octets in network order, e.g. `[127, 0, 0, 1]`.
    pub ip: [u8; 4],
    /// UDP port in host order.
    pub port: u16,
    /// 16-bit salt, normally 0 for external (client facing) addresses.
    pub salt: u16,
}

impl Address {
    pub const NONE: Address = Address {
        ip: [0, 0, 0, 0],
        port: 0,
        salt: 0,
    };

    pub fn new(ip: [u8; 4], port: u16, salt: u16) -> Self {
        Self { ip, port, salt }
    }

    pub fn from_socket(addr: SocketAddrV4) -> Self {
        Self {
            ip: addr.ip().octets(),
            port: addr.port(),
            salt: 0,
        }
    }

    pub fn to_socket(&self) -> SocketAddrV4 {
        SocketAddrV4::new(Ipv4Addr::from(self.ip), self.port)
    }

    pub fn is_none(&self) -> bool {
        self.ip == [0, 0, 0, 0] && self.port == 0
    }

    /// Wire size of a streamed address (8 bytes).
    pub const WIRE_SIZE: usize = 8;

    /// Write the 8 on-wire bytes into `out`.
    pub fn write_to(&self, out: &mut Vec<u8>) {
        out.extend_from_slice(&self.ip);
        out.extend_from_slice(&self.port.to_be_bytes());
        out.extend_from_slice(&self.salt.to_le_bytes());
    }

    /// Read the 8 on-wire bytes from `r`.
    pub fn read_from(r: &mut BinaryReader<'_>) -> Result<Self, StreamError> {
        let ip_bytes = r.raw(4)?;
        let mut ip = [0u8; 4];
        ip.copy_from_slice(ip_bytes);
        let port = r.u16_be()?;
        let salt = r.u16()?;
        Ok(Self { ip, port, salt })
    }
}

impl fmt::Display for Address {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{}.{}.{}.{}:{}",
            self.ip[0], self.ip[1], self.ip[2], self.ip[3], self.port
        )
    }
}

impl From<SocketAddrV4> for Address {
    fn from(value: SocketAddrV4) -> Self {
        Address::from_socket(value)
    }
}

// ---------------------------------------------------------------------------
// Writer
// ---------------------------------------------------------------------------

/// Growable output stream that mirrors `BinaryOStream`.
#[derive(Debug, Default, Clone)]
pub struct BinaryWriter {
    buf: Vec<u8>,
}

impl BinaryWriter {
    pub fn new() -> Self {
        Self { buf: Vec::new() }
    }

    pub fn with_capacity(cap: usize) -> Self {
        Self {
            buf: Vec::with_capacity(cap),
        }
    }

    pub fn into_inner(self) -> Vec<u8> {
        self.buf
    }

    pub fn as_slice(&self) -> &[u8] {
        &self.buf
    }

    pub fn as_mut_vec(&mut self) -> &mut Vec<u8> {
        &mut self.buf
    }

    pub fn len(&self) -> usize {
        self.buf.len()
    }

    pub fn is_empty(&self) -> bool {
        self.buf.is_empty()
    }

    pub fn clear(&mut self) {
        self.buf.clear();
    }

    pub fn u8(&mut self, v: u8) {
        self.buf.push(v);
    }

    pub fn i8(&mut self, v: i8) {
        self.buf.push(v as u8);
    }

    pub fn bool(&mut self, v: bool) {
        self.buf.push(v as u8);
    }

    pub fn u16(&mut self, v: u16) {
        self.buf.extend_from_slice(&v.to_le_bytes());
    }

    pub fn i16(&mut self, v: i16) {
        self.buf.extend_from_slice(&v.to_le_bytes());
    }

    pub fn u32(&mut self, v: u32) {
        self.buf.extend_from_slice(&v.to_le_bytes());
    }

    pub fn i32(&mut self, v: i32) {
        self.buf.extend_from_slice(&v.to_le_bytes());
    }

    pub fn u64(&mut self, v: u64) {
        self.buf.extend_from_slice(&v.to_le_bytes());
    }

    pub fn i64(&mut self, v: i64) {
        self.buf.extend_from_slice(&v.to_le_bytes());
    }

    pub fn f32(&mut self, v: f32) {
        self.buf.extend_from_slice(&v.to_le_bytes());
    }

    pub fn f64(&mut self, v: f64) {
        self.buf.extend_from_slice(&v.to_le_bytes());
    }

    /// Append raw bytes.
    pub fn raw(&mut self, bytes: &[u8]) {
        self.buf.extend_from_slice(bytes);
    }

    /// `BinaryOStream::writePackedInt` / `writeStringLength`.
    ///
    /// Values below 255 are a single byte; otherwise `0xFF` followed by the
    /// low three bytes in little-endian order.
    pub fn packed_u32(&mut self, value: u32) {
        if value >= 255 {
            self.buf.push(0xFF);
            self.buf.push(value as u8);
            self.buf.push((value >> 8) as u8);
            self.buf.push((value >> 16) as u8);
        } else {
            self.buf.push(value as u8);
        }
    }

    /// Append a string as BigWorld streams `std::string`: packed length + bytes.
    pub fn string_bytes(&mut self, data: &[u8]) {
        self.packed_u32(data.len() as u32);
        self.buf.extend_from_slice(data);
    }

    /// Append a UTF-8 string as BigWorld streams `std::string`.
    pub fn string(&mut self, s: &str) {
        self.string_bytes(s.as_bytes());
    }

    /// Append a raw 16-byte MD5 digest (`MD5::Digest` streaming operator).
    pub fn digest(&mut self, d: &[u8; 16]) {
        self.buf.extend_from_slice(d);
    }

    /// Append a `Mercury::Address` in the 8-byte legacy layout.
    pub fn address(&mut self, a: &Address) {
        a.write_to(&mut self.buf);
    }
}

// ---------------------------------------------------------------------------
// Reader
// ---------------------------------------------------------------------------

/// Cursor over an input stream that mirrors `BinaryIStream`.
#[derive(Debug, Clone)]
pub struct BinaryReader<'a> {
    data: &'a [u8],
    pos: usize,
}

impl<'a> BinaryReader<'a> {
    pub fn new(data: &'a [u8]) -> Self {
        Self { data, pos: 0 }
    }

    pub fn position(&self) -> usize {
        self.pos
    }

    pub fn remaining(&self) -> usize {
        self.data.len().saturating_sub(self.pos)
    }

    pub fn is_empty(&self) -> bool {
        self.remaining() == 0
    }

    pub fn data(&self) -> &'a [u8] {
        self.data
    }

    /// Bytes consumed from the start of the stream. Useful for building
    /// sub-streams (e.g. a raw `MemoryIStream` copy of a message body).
    pub fn consumed(&self) -> &'a [u8] {
        &self.data[..self.pos]
    }

    pub fn rest(&self) -> &'a [u8] {
        &self.data[self.pos..]
    }

    /// Retrieve `n` raw bytes.
    pub fn raw(&mut self, n: usize) -> Result<&'a [u8], StreamError> {
        if self.remaining() < n {
            self.pos = self.data.len();
            return Err(StreamError::UnexpectedEof);
        }
        let out = &self.data[self.pos..self.pos + n];
        self.pos += n;
        Ok(out)
    }

    /// Consume the remaining bytes and return them.
    pub fn rest_vec(&mut self) -> Vec<u8> {
        let out = self.data[self.pos..].to_vec();
        self.pos = self.data.len();
        out
    }

    pub fn u8(&mut self) -> Result<u8, StreamError> {
        Ok(self.raw(1)?[0])
    }

    pub fn i8(&mut self) -> Result<i8, StreamError> {
        Ok(self.u8()? as i8)
    }

    pub fn bool(&mut self) -> Result<bool, StreamError> {
        Ok(self.u8()? != 0)
    }

    pub fn u16(&mut self) -> Result<u16, StreamError> {
        let b = self.raw(2)?;
        Ok(u16::from_le_bytes([b[0], b[1]]))
    }

    pub fn u16_be(&mut self) -> Result<u16, StreamError> {
        let b = self.raw(2)?;
        Ok(u16::from_be_bytes([b[0], b[1]]))
    }

    pub fn i16(&mut self) -> Result<i16, StreamError> {
        Ok(self.u16()? as i16)
    }

    pub fn u32(&mut self) -> Result<u32, StreamError> {
        let b = self.raw(4)?;
        Ok(u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    }

    pub fn u32_be(&mut self) -> Result<u32, StreamError> {
        let b = self.raw(4)?;
        Ok(u32::from_be_bytes([b[0], b[1], b[2], b[3]]))
    }

    pub fn i32(&mut self) -> Result<i32, StreamError> {
        Ok(self.u32()? as i32)
    }

    pub fn u64(&mut self) -> Result<u64, StreamError> {
        let b = self.raw(8)?;
        let mut a = [0u8; 8];
        a.copy_from_slice(b);
        Ok(u64::from_le_bytes(a))
    }

    pub fn i64(&mut self) -> Result<i64, StreamError> {
        Ok(self.u64()? as i64)
    }

    pub fn f32(&mut self) -> Result<f32, StreamError> {
        let b = self.raw(4)?;
        Ok(f32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    }

    pub fn f64(&mut self) -> Result<f64, StreamError> {
        let b = self.raw(8)?;
        let mut a = [0u8; 8];
        a.copy_from_slice(b);
        Ok(f64::from_le_bytes(a))
    }

    /// `BinaryIStream::readPackedInt` / `readStringLength`.
    pub fn packed_u32(&mut self) -> Result<u32, StreamError> {
        let first = self.u8()?;
        if first == 0xFF {
            let b = self.raw(3)?;
            Ok((b[0] as u32) | ((b[1] as u32) << 8) | ((b[2] as u32) << 16))
        } else {
            Ok(first as u32)
        }
    }

    /// Read a `std::string` as raw bytes (length prefix + payload).
    pub fn string_bytes(&mut self) -> Result<Vec<u8>, StreamError> {
        let len = self.packed_u32()? as usize;
        Ok(self.raw(len)?.to_vec())
    }

    /// Read a `std::string` and require valid UTF-8.
    pub fn string(&mut self) -> Result<String, StreamError> {
        let bytes = self.string_bytes()?;
        String::from_utf8(bytes).map_err(|_| StreamError::InvalidUtf8)
    }

    /// Read a raw 16-byte MD5 digest.
    pub fn digest(&mut self) -> Result<[u8; 16], StreamError> {
        let b = self.raw(16)?;
        let mut d = [0u8; 16];
        d.copy_from_slice(b);
        Ok(d)
    }

    /// Read a `Mercury::Address` in the 8-byte legacy layout.
    pub fn address(&mut self) -> Result<Address, StreamError> {
        Address::read_from(self)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn integers_are_little_endian() {
        let mut w = BinaryWriter::new();
        w.u8(0xAB);
        w.u16(0x1234);
        w.u32(0xDEADBEEF);
        w.u64(0x0102030405060708);
        assert_eq!(
            w.as_slice(),
            &[
                0xAB, 0x34, 0x12, 0xEF, 0xBE, 0xAD, 0xDE, 0x08, 0x07, 0x06, 0x05, 0x04, 0x03,
                0x02, 0x01
            ]
        );
        let mut r = BinaryReader::new(w.as_slice());
        assert_eq!(r.u8().unwrap(), 0xAB);
        assert_eq!(r.u16().unwrap(), 0x1234);
        assert_eq!(r.u32().unwrap(), 0xDEADBEEF);
        assert_eq!(r.u64().unwrap(), 0x0102030405060708);
        assert!(r.is_empty());
    }

    #[test]
    fn packed_string_short_form() {
        let mut w = BinaryWriter::new();
        w.string("hello");
        assert_eq!(w.as_slice(), b"\x05hello");
        let mut r = BinaryReader::new(w.as_slice());
        assert_eq!(r.string().unwrap(), "hello");
    }

    #[test]
    fn packed_string_long_form() {
        let payload = vec![b'x'; 300];
        let mut w = BinaryWriter::new();
        w.string_bytes(&payload);
        // 0xFF + 300 (0x012C) packed as u24 LE.
        assert_eq!(&w.as_slice()[..4], &[0xFF, 0x2C, 0x01, 0x00]);
        let mut r = BinaryReader::new(w.as_slice());
        assert_eq!(r.string_bytes().unwrap(), payload);
    }

    #[test]
    fn packed_string_boundary() {
        // 254 uses the short form, 255 the long form (C++: value >= 255).
        let mut w = BinaryWriter::new();
        w.packed_u32(254);
        assert_eq!(w.as_slice(), &[254]);
        w.clear();
        w.packed_u32(255);
        assert_eq!(w.as_slice(), &[0xFF, 0xFF, 0x00, 0x00]);
        w.clear();
        w.packed_u32(65536);
        assert_eq!(w.as_slice(), &[0xFF, 0x00, 0x00, 0x01]);
    }

    #[test]
    fn address_layout_matches_bw() {
        let a = Address::new([127, 0, 0, 1], 20017, 0);
        let mut w = BinaryWriter::new();
        w.address(&a);
        // ip raw network order, port big-endian raw, salt LE.
        assert_eq!(
            w.as_slice(),
            &[127, 0, 0, 1, 0x4E, 0x31, 0x00, 0x00]
        );
        let mut r = BinaryReader::new(w.as_slice());
        assert_eq!(r.address().unwrap(), a);
        assert_eq!(a.to_string(), "127.0.0.1:20017");
    }

    #[test]
    fn eof_sets_error() {
        let mut r = BinaryReader::new(&[1]);
        assert_eq!(r.u32(), Err(StreamError::UnexpectedEof));
        assert_eq!(r.u16(), Err(StreamError::UnexpectedEof));
    }
}
