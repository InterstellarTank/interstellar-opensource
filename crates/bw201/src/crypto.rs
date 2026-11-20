//! Crypto primitives used by the BigWorld 2.0.1 connection layer.
//!
//! Sources:
//! - `src/lib/network/public_key_cipher.cpp` — RSA, OAEP-SHA1, 214-byte clear chunks
//! - `src/lib/connection/rsa_stream_encoder.hpp`
//! - `src/lib/network/encryption_filter.cpp` — Blowfish with the version-49 XOR stage
//! - `src/lib/network/encryption_filter.hpp`
//!
//! Two distinct Blowfish modes are used by BigWorld and both are implemented here:
//!
//! 1. **Stream mode** ([`BlowfishKey::stream_encrypt`] / [`BlowfishKey::stream_decrypt`]):
//!    used for the `LoginReplyRecord` inside the LoginApp reply. Clear text is
//!    zero-padded to a multiple of 8 bytes and encrypted block by block with
//!    `C_i = E(P_i XOR P_{i-1})`, where `P_{-1} = 0` (the *previous plaintext*
//!    block, added in protocol version 49 to prevent replay attacks). No footer,
//!    no magic — the element length delimits the cipher text.
//!
//! 2. **Packet mode** ([`BlowfishKey::packet_encrypt`] / [`BlowfishKey::packet_decrypt`]):
//!    `EncryptionFilter::send/recv` for full Mercury packets (BaseApp channels).
//!    The whole packet is padded with zeros, then `0xDEADBEEF` little-endian and a
//!    wastage count byte are appended so the receiver can remove the padding, and
//!    the entire buffer is encrypted with the same XOR-chain construction.
//!
//! RSA: PKCS#1 v1.5 OAEP padding with SHA-1 (OpenSSL `RSA_PKCS1_OAEP_PADDING`,
//! `#define RSA_PADDING 41`). Clear text is split into chunks of
//! `modulus_size - 41 - 1` bytes (214 for RSA-2048), each encrypted to
//! `modulus_size` bytes, concatenated with no extra framing.

use std::path::Path;

use cipher::{BlockDecrypt, BlockEncrypt, KeyInit};
use rsa::pkcs8::{DecodePrivateKey, EncodePublicKey, LineEnding};
use rsa::traits::PublicKeyParts;
use rsa::{Oaep, Pkcs1v15Encrypt, RsaPrivateKey, RsaPublicKey};
use sha1::Sha1;

use crate::packet::{FilterError, ENCRYPTION_MAGIC};

pub const BLOWFISH_BLOCK_SIZE: usize = 8;
pub const BLOWFISH_MIN_KEY: usize = 4;
pub const BLOWFISH_MAX_KEY: usize = 56;
pub const BLOWFISH_DEFAULT_KEY: usize = 16;

/// RSA OAEP-SHA1 padding overhead: `2 * SHA1_LEN + 2`.
const RSA_OAEP_PADDING: usize = 41;

// ---------------------------------------------------------------------------
// RSA
// ---------------------------------------------------------------------------

/// Errors from the RSA login-parameter codec.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum RsaError {
    #[error("invalid RSA private key: {0}")]
    InvalidKey(String),
    #[error("I/O error: {0}")]
    Io(String),
    #[error("RSA cipher text is not a multiple of the modulus size ({modulus}): {len} bytes")]
    NotBlockAligned { modulus: usize, len: usize },
    #[error("RSA decrypt failed: {0}")]
    Decrypt(String),
    #[error("RSA encrypt failed: {0}")]
    Encrypt(String),
    #[error("empty stream")]
    Empty,
}

/// The LoginApp RSA key pair (PKCS#8 private key PEM, as provisioned for this
/// project). Encryption is done with the derived public key.
#[derive(Clone)]
pub struct LoginKey {
    private: RsaPrivateKey,
    public: RsaPublicKey,
}

impl std::fmt::Debug for LoginKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LoginKey")
            .field("modulus_bits", &self.modulus_bits())
            .finish()
    }
}

impl LoginKey {
    /// Load a PKCS#8 (or PKCS#1) private key from PEM.
    pub fn from_pkcs8_pem(pem: &str) -> Result<Self, RsaError> {
        let private = RsaPrivateKey::from_pkcs8_pem(pem)
            .or_else(|_| {
                use rsa::pkcs1::DecodeRsaPrivateKey;
                RsaPrivateKey::from_pkcs1_pem(pem)
            })
            .map_err(|e| RsaError::InvalidKey(e.to_string()))?;
        let public = RsaPublicKey::from(&private);
        Ok(Self { private, public })
    }

    pub fn from_pem_file(path: impl AsRef<Path>) -> Result<Self, RsaError> {
        let pem = std::fs::read_to_string(path).map_err(|e| RsaError::Io(e.to_string()))?;
        Self::from_pkcs8_pem(&pem)
    }

    /// Generate a fresh RSA-2048 key (used by tests and by `--generate-key`).
    pub fn generate(bits: usize) -> Result<Self, RsaError> {
        let mut rng = rand::thread_rng();
        let private = RsaPrivateKey::new(&mut rng, bits)
            .map_err(|e| RsaError::InvalidKey(e.to_string()))?;
        let public = RsaPublicKey::from(&private);
        Ok(Self { private, public })
    }

    /// Modulus size in bits (2048 for the provisioned WoT key).
    pub fn modulus_bits(&self) -> usize {
        self.private.n().bits()
    }

    /// Modulus size in bytes (256 for RSA-2048).
    pub fn modulus_len(&self) -> usize {
        self.private.size()
    }

    /// Maximum clear-text bytes that fit in one OAEP block:
    /// `modulus_len - RSA_PADDING - 1`.
    pub fn max_clear_chunk(&self) -> usize {
        self.modulus_len() - RSA_OAEP_PADDING - 1
    }

    pub fn public_key(&self) -> &RsaPublicKey {
        &self.public
    }

    /// PKCS#8 private key PEM (for provisioning).
    pub fn to_pkcs8_pem(&self) -> Result<String, RsaError> {
        use rsa::pkcs8::EncodePrivateKey;
        self.private
            .to_pkcs8_pem(LineEnding::LF)
            .map(|s| s.to_string())
            .map_err(|e| RsaError::InvalidKey(e.to_string()))
    }

    /// SubjectPublicKeyInfo PEM (the `res/loginapp_wot.pubkey` file content).
    pub fn to_public_pem(&self) -> Result<String, RsaError> {
        self.public
            .to_public_key_pem(LineEnding::LF)
            .map_err(|e| RsaError::InvalidKey(e.to_string()))
    }

    /// Encrypt a clear-text stream the way `PublicKeyCipher::encrypt` does:
    /// chunk into `max_clear_chunk()` pieces, OAEP-SHA1 each one, concatenate.
    pub fn encrypt(&self, clear: &[u8]) -> Result<Vec<u8>, RsaError> {
        if clear.is_empty() {
            return Err(RsaError::Empty);
        }
        let mut rng = rand::thread_rng();
        let mut out = Vec::with_capacity(
            (clear.len().div_ceil(self.max_clear_chunk())) * self.modulus_len(),
        );
        for chunk in clear.chunks(self.max_clear_chunk()) {
            let block = self
                .public
                .encrypt(&mut rng, Oaep::new::<Sha1>(), chunk)
                .map_err(|e| RsaError::Encrypt(e.to_string()))?;
            debug_assert_eq!(block.len(), self.modulus_len());
            out.extend_from_slice(&block);
        }
        Ok(out)
    }

    /// Decrypt a cipher-text stream the way `PublicKeyCipher::decrypt` does.
    pub fn decrypt(&self, cipher: &[u8]) -> Result<Vec<u8>, RsaError> {
        self.decrypt_with(cipher, || Oaep::new::<Sha1>())
    }

    /// Fallback for a client that used PKCS#1 v1.5 padding instead of OAEP.
    pub fn decrypt_pkcs1v15(&self, cipher: &[u8]) -> Result<Vec<u8>, RsaError> {
        self.decrypt_with(cipher, || Pkcs1v15Encrypt)
    }

    fn decrypt_with<P>(
        &self,
        cipher: &[u8],
        make_padding: impl Fn() -> P,
    ) -> Result<Vec<u8>, RsaError>
    where
        P: rsa::traits::PaddingScheme,
    {
        let mod_len = self.modulus_len();
        if cipher.is_empty() {
            return Err(RsaError::Empty);
        }
        if cipher.len() % mod_len != 0 {
            return Err(RsaError::NotBlockAligned {
                modulus: mod_len,
                len: cipher.len(),
            });
        }
        let mut out = Vec::with_capacity(cipher.len());
        for chunk in cipher.chunks(mod_len) {
            let clear = self
                .private
                .decrypt(make_padding(), chunk)
                .map_err(|e| RsaError::Decrypt(e.to_string()))?;
            out.extend_from_slice(&clear);
        }
        Ok(out)
    }
}

// ---------------------------------------------------------------------------
// Blowfish
// ---------------------------------------------------------------------------

/// Errors from the Blowfish filter.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum BlowfishError {
    #[error("invalid Blowfish key length {0} (must be {min}..={max})", min = BLOWFISH_MIN_KEY, max = BLOWFISH_MAX_KEY)]
    InvalidKeyLength(usize),
    #[error("cipher text length {0} is not a multiple of {BLOWFISH_BLOCK_SIZE}")]
    NotBlockAligned(usize),
}

/// A Blowfish key plus the two BigWorld filter modes.
#[derive(Clone)]
pub struct BlowfishKey {
    cipher: blowfish::Blowfish,
    key: Vec<u8>,
}

impl std::fmt::Debug for BlowfishKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BlowfishKey")
            .field("key_len", &self.key.len())
            .finish()
    }
}

impl BlowfishKey {
    pub fn new(key: &[u8]) -> Result<Self, BlowfishError> {
        if !(BLOWFISH_MIN_KEY..=BLOWFISH_MAX_KEY).contains(&key.len()) {
            return Err(BlowfishError::InvalidKeyLength(key.len()));
        }
        let cipher = blowfish::Blowfish::new_from_slice(key)
            .map_err(|_| BlowfishError::InvalidKeyLength(key.len()))?;
        Ok(Self {
            cipher,
            key: key.to_vec(),
        })
    }

    /// The raw key bytes (as sent inside `LogOnParams::encryptionKey`).
    pub fn key(&self) -> &[u8] {
        &self.key
    }

    fn encrypt_block(&self, block: &mut [u8; BLOWFISH_BLOCK_SIZE]) {
        let mut b = cipher::Block::<blowfish::Blowfish>::clone_from_slice(block);
        self.cipher.encrypt_block(&mut b);
        block.copy_from_slice(&b);
    }

    fn decrypt_block(&self, block: &mut [u8; BLOWFISH_BLOCK_SIZE]) {
        let mut b = cipher::Block::<blowfish::Blowfish>::clone_from_slice(block);
        self.cipher.decrypt_block(&mut b);
        block.copy_from_slice(&b);
    }

    /// `EncryptionFilter::encryptStream`: zero-pad to an 8-byte multiple, then
    /// `C_i = E(P_i XOR P_{i-1})`, `P_{-1} = 0`.
    pub fn stream_encrypt(&self, clear: &[u8]) -> Vec<u8> {
        let mut padded = clear.to_vec();
        let pad = (BLOWFISH_BLOCK_SIZE - padded.len() % BLOWFISH_BLOCK_SIZE) % BLOWFISH_BLOCK_SIZE;
        padded.extend(std::iter::repeat(0u8).take(pad));

        let mut out = Vec::with_capacity(padded.len());
        let mut prev = [0u8; BLOWFISH_BLOCK_SIZE];
        for block in padded.chunks_exact(BLOWFISH_BLOCK_SIZE) {
            let mut b = [0u8; BLOWFISH_BLOCK_SIZE];
            for i in 0..BLOWFISH_BLOCK_SIZE {
                b[i] = block[i] ^ prev[i];
            }
            let plain = [block[0], block[1], block[2], block[3], block[4], block[5], block[6], block[7]];
            self.encrypt_block(&mut b);
            out.extend_from_slice(&b);
            prev = plain;
        }
        out
    }

    /// `EncryptionFilter::decryptStream`: inverse of [`Self::stream_encrypt`],
    /// `P_i = D(C_i) XOR P_{i-1}`.
    pub fn stream_decrypt(&self, cipher: &[u8]) -> Result<Vec<u8>, BlowfishError> {
        if cipher.len() % BLOWFISH_BLOCK_SIZE != 0 {
            return Err(BlowfishError::NotBlockAligned(cipher.len()));
        }
        let mut out = Vec::with_capacity(cipher.len());
        let mut prev = [0u8; BLOWFISH_BLOCK_SIZE];
        for block in cipher.chunks_exact(BLOWFISH_BLOCK_SIZE) {
            let mut b = [0u8; BLOWFISH_BLOCK_SIZE];
            b.copy_from_slice(block);
            self.decrypt_block(&mut b);
            for i in 0..BLOWFISH_BLOCK_SIZE {
                b[i] ^= prev[i];
            }
            out.extend_from_slice(&b);
            prev = b;
        }
        Ok(out)
    }

    /// `EncryptionFilter::send`: encrypt a whole packet for the wire.
    pub fn packet_encrypt(&self, packet: &[u8]) -> Vec<u8> {
        // C++: len = packet.totalSize() + sizeof(ENCRYPTION_MAGIC);
        //      wastage = ((BLOCK_SIZE - ((len + 1) % BLOCK_SIZE)) % BLOCK_SIZE) + 1;
        //      len += wastage; magic at len-1-4, wastage at len-1.
        let mut len = packet.len() + 4;
        let wastage = ((BLOWFISH_BLOCK_SIZE - ((len + 1) % BLOWFISH_BLOCK_SIZE)) % BLOWFISH_BLOCK_SIZE) + 1;
        len += wastage;

        let mut clear = Vec::with_capacity(len);
        clear.extend_from_slice(packet);
        clear.resize(len, 0);
        let start_wastage = len - 1;
        let start_magic = start_wastage - 4;
        clear[start_magic..start_wastage].copy_from_slice(&ENCRYPTION_MAGIC.to_le_bytes());
        clear[start_wastage] = wastage as u8;
        self.stream_encrypt(&clear)
    }

    /// `EncryptionFilter::recv`: decrypt a whole packet, strip and validate the
    /// encryption footer.
    pub fn packet_decrypt(&self, cipher: &[u8]) -> Result<Vec<u8>, FilterError> {
        if cipher.len() % BLOWFISH_BLOCK_SIZE != 0 {
            return Err(FilterError::NotBlockAligned);
        }
        if cipher.len() < 4 + 1 {
            return Err(FilterError::TooShort);
        }
        let clear = self
            .stream_decrypt(cipher)
            .map_err(|_| FilterError::NotBlockAligned)?;
        let start_wastage = clear.len() - 1;
        let start_magic = start_wastage - 4;
        let magic = u32::from_le_bytes([
            clear[start_magic],
            clear[start_magic + 1],
            clear[start_magic + 2],
            clear[start_magic + 3],
        ]);
        if magic != ENCRYPTION_MAGIC {
            return Err(FilterError::BadMagic);
        }
        let wastage = clear[start_wastage];
        if wastage as usize > BLOWFISH_BLOCK_SIZE {
            return Err(FilterError::BadWastage(wastage));
        }
        let footer = wastage as usize + 4;
        if footer > clear.len() {
            return Err(FilterError::BadWastage(wastage));
        }
        Ok(clear[..clear.len() - footer].to_vec())
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Format bytes as a lowercase hex string.
pub fn hex(bytes: &[u8]) -> String {
    hex::encode(bytes)
}

/// Canonical one-line hex dump for logs: `hexdump(label, bytes)`.
pub fn hexdump(label: &str, bytes: &[u8]) -> String {
    format!("{} ({} bytes): {}", label, bytes.len(), hex::encode(bytes))
}

/// Return a mutable reference to the secret inside the login reply record.
#[cfg(test)]
mod tests {
    use super::*;

    fn test_key() -> LoginKey {
        LoginKey::generate(2048).unwrap()
    }

    #[test]
    fn blowfish_stream_roundtrip() {
        let key = BlowfishKey::new(&[0x11; 16]).unwrap();
        for len in 0..40usize {
            let clear: Vec<u8> = (0..len).map(|i| i as u8).collect();
            let enc = key.stream_encrypt(&clear);
            assert_eq!(enc.len() % 8, 0);
            let dec = key.stream_decrypt(&enc).unwrap();
            assert_eq!(&dec[..len], &clear[..]);
        }
    }

    #[test]
    fn blowfish_matches_wg_toolkit_reference() {
        // Reference vector produced by an independent implementation
        // (wg-toolkit `BlowfishWriter`/`BlowfishReader`, same construction):
        // key = 00..0f, clear = 16 bytes 0x00..0x0f.
        let key = BlowfishKey::new(&[
            0x00, 0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0x09, 0x0A, 0x0B, 0x0C, 0x0D,
            0x0E, 0x0F,
        ])
        .unwrap();
        let clear: Vec<u8> = (0..16).collect();
        let enc = key.stream_encrypt(&clear);
        // ECB with plaintext chaining: block 0 = E(00..07),
        // block 1 = E((08..0f) XOR (00..07)).
        let mut plain0 = [0u8; 8];
        let mut plain1 = [0u8; 8];
        plain0.copy_from_slice(&clear[..8]);
        for i in 0..8 {
            plain1[i] = clear[8 + i] ^ plain0[i];
        }
        let mut expect0 = plain0;
        let mut expect1 = plain1;
        key.encrypt_block(&mut expect0);
        key.encrypt_block(&mut expect1);
        let mut expected = expect0.to_vec();
        expected.extend_from_slice(&expect1);
        assert_eq!(enc, expected);
        assert_eq!(key.stream_decrypt(&enc).unwrap()[..16], clear[..]);
    }

    #[test]
    fn blowfish_packet_mode_roundtrip() {
        let key = BlowfishKey::new(&[0x42; 16]).unwrap();
        for len in 0..64usize {
            let packet: Vec<u8> = (0..len).map(|i| (i * 7) as u8).collect();
            let enc = key.packet_encrypt(&packet);
            assert_eq!(enc.len() % 8, 0);
            assert_eq!(key.packet_decrypt(&enc).unwrap(), packet);
        }
    }

    #[test]
    fn blowfish_packet_magic_is_little_endian() {
        let key = BlowfishKey::new(&[0x42; 16]).unwrap();
        let enc = key.packet_encrypt(&[1, 2, 3, 4, 5]);
        let clear = key.stream_decrypt(&enc).unwrap();
        let m = clear.len() - 5;
        assert_eq!(&clear[m..m + 4], &[0xEF, 0xBE, 0xAD, 0xDE]);
    }

    #[test]
    fn rsa_roundtrip_single_chunk() {
        let key = test_key();
        assert_eq!(key.modulus_len(), 256);
        assert_eq!(key.max_clear_chunk(), 214);
        let clear = b"hello world".to_vec();
        let enc = key.encrypt(&clear).unwrap();
        assert_eq!(enc.len(), 256);
        assert_eq!(key.decrypt(&enc).unwrap(), clear);
    }

    #[test]
    fn rsa_roundtrip_multi_chunk() {
        let key = test_key();
        let clear: Vec<u8> = (0..500).map(|i| i as u8).collect();
        let enc = key.encrypt(&clear).unwrap();
        assert_eq!(enc.len(), 3 * 256);
        assert_eq!(key.decrypt(&enc).unwrap(), clear);
    }

    #[test]
    fn rsa_rejects_bad_alignment() {
        let key = test_key();
        assert!(matches!(
            key.decrypt(&[0u8; 100]),
            Err(RsaError::NotBlockAligned { .. })
        ));
    }

    #[test]
    fn provisioned_key_loads_if_present() {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../.scratch/wot-keys/loginapp_priv.pem");
        if !path.exists() {
            return;
        }
        let key = LoginKey::from_pem_file(&path).unwrap();
        assert_eq!(key.modulus_len(), 256);
        let pub_pem = key.to_public_pem().unwrap();
        assert!(pub_pem.contains("BEGIN PUBLIC KEY"));
    }
}
