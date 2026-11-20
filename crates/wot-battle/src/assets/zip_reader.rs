//! Minimal ZIP reader supporting Stored (0) and Deflate (8) entries.

use std::io::Read;

use flate2::read::DeflateDecoder;

use crate::assets::AssetError;

const LOCAL_FILE_HEADER_SIG: u32 = 0x0403_4b50;
const CENTRAL_DIR_HEADER_SIG: u32 = 0x0201_4b50;
const EOCD_SIG: u32 = 0x0605_4b50;
const MAX_INITIAL_ZIP_CAPACITY: usize = 16 * 1024 * 1024;
const MAX_ZIP_ENTRY_BYTES: usize = 64 * 1024 * 1024;

/// Metadata for one entry in a ZIP archive's Central Directory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ZipEntry {
    pub name: String,
    pub method: u16,
    pub compressed_size: u32,
    pub uncompressed_size: u32,
    pub local_header_offset: u32,
}

/// Zero-copy view over an in-memory ZIP archive (e.g., BigWorld `.cdata`).
#[derive(Debug, Clone)]
pub struct ZipArchive<'a> {
    data: &'a [u8],
    entries: Vec<ZipEntry>,
}

impl<'a> ZipArchive<'a> {
    /// Parse a ZIP archive from raw bytes by reading its End-of-Central-Directory
    /// and Central Directory records.
    pub fn parse(data: &'a [u8]) -> Result<Self, AssetError> {
        let eocd = find_eocd(data)?;
        let total_entries = read_u16(data, eocd + 10)? as usize;
        let cd_size = read_u32(data, eocd + 12)? as usize;
        let cd_offset = read_u32(data, eocd + 16)? as usize;
        let cd_end = cd_offset
            .checked_add(cd_size)
            .filter(|&end| end <= eocd)
            .ok_or_else(|| AssetError::Zip("central directory exceeds archive bounds".into()))?;

        let mut entries = Vec::with_capacity(total_entries.min(cd_size / 46));
        let mut pos = cd_offset;
        while pos < cd_end {
            if read_u32(data, pos)? != CENTRAL_DIR_HEADER_SIG {
                return Err(AssetError::Zip(format!(
                    "bad central directory signature at offset {pos}"
                )));
            }
            if pos + 46 > cd_end {
                return Err(AssetError::Zip("truncated central directory header".into()));
            }
            let method = read_u16(data, pos + 10)?;
            let compressed_size = read_u32(data, pos + 20)?;
            let uncompressed_size = read_u32(data, pos + 24)?;
            let name_len = read_u16(data, pos + 28)? as usize;
            let extra_len = read_u16(data, pos + 30)? as usize;
            let comment_len = read_u16(data, pos + 32)? as usize;
            let local_header_offset = read_u32(data, pos + 42)?;
            let name_start = pos + 46;
            let next_pos = name_start + name_len + extra_len + comment_len;
            if next_pos > cd_end {
                return Err(AssetError::Zip("truncated central directory record".into()));
            }
            let name = String::from_utf8_lossy(&data[name_start..name_start + name_len]).into_owned();
            entries.push(ZipEntry {
                name,
                method,
                compressed_size,
                uncompressed_size,
                local_header_offset,
            });
            pos = next_pos;
        }

        Ok(Self { data, entries })
    }

    pub fn entries(&self) -> &[ZipEntry] {
        &self.entries
    }

    /// Read and decompress the exact entry matching `name` (if duplicated, uses the
    /// last Central Directory entry).
    pub fn read(&self, name: &str) -> Result<Vec<u8>, AssetError> {
        let entry = self
            .entries
            .iter()
            .rfind(|e| e.name == name)
            .ok_or_else(|| AssetError::NotFound(format!("zip entry `{name}`")))?;
        self.decompress_entry(entry)
    }

    /// Read the entry matching `base_name`, falling back to the last Central Directory
    /// entry whose name matches `base_name` case-insensitively with an optional
    /// `" <digits>"` duplicate suffix (as seen in retail `15_komarin` and
    /// `38_mannerheim_line` `.cdata` files).
    pub fn read_latest_matching(&self, base_name: &str) -> Result<Vec<u8>, AssetError> {
        if let Some(entry) = self.entries.iter().rfind(|e| e.name == base_name) {
            return self.decompress_entry(entry);
        }
        let entry = self
            .entries
            .iter()
            .rfind(|e| {
                let candidate = strip_duplicate_suffix(&e.name);
                e.name.eq_ignore_ascii_case(base_name)
                    || candidate.eq_ignore_ascii_case(base_name)
            })
            .ok_or_else(|| AssetError::NotFound(format!("zip entry `{base_name}`")))?;
        self.decompress_entry(entry)
    }

    fn decompress_entry(&self, entry: &ZipEntry) -> Result<Vec<u8>, AssetError> {
        let off = entry.local_header_offset as usize;
        if off + 30 > self.data.len() {
            return Err(AssetError::Zip("truncated local file header".into()));
        }
        if read_u32(self.data, off)? != LOCAL_FILE_HEADER_SIG {
            return Err(AssetError::Zip(format!(
                "bad local file header signature for `{}`",
                entry.name
            )));
        }
        let local_name_len = read_u16(self.data, off + 26)? as usize;
        let local_extra_len = read_u16(self.data, off + 28)? as usize;
        let data_start = off + 30 + local_name_len + local_extra_len;
        let data_end = data_start
            .checked_add(entry.compressed_size as usize)
            .filter(|&end| end <= self.data.len())
            .ok_or_else(|| {
                AssetError::Zip(format!("entry `{}` payload out of bounds", entry.name))
            })?;
        let raw = &self.data[data_start..data_end];
        match entry.method {
            0 => {
                if entry.uncompressed_size != entry.compressed_size {
                    return Err(AssetError::Zip(format!(
                        "stored entry `{}` size mismatch ({} vs {})",
                        entry.name, entry.compressed_size, entry.uncompressed_size
                    )));
                }
                Ok(raw.to_vec())
            }
            8 => {
                let cap = (entry.uncompressed_size as usize).min(MAX_INITIAL_ZIP_CAPACITY);
                let mut out = Vec::with_capacity(cap);
                let limit = (entry.uncompressed_size as u64).min(MAX_ZIP_ENTRY_BYTES as u64) + 1;
                let mut dec = DeflateDecoder::new(raw).take(limit);
                dec.read_to_end(&mut out)
                    .map_err(|e| AssetError::Zip(format!("deflate `{}`: {e}", entry.name)))?;
                if out.len() != entry.uncompressed_size as usize {
                    return Err(AssetError::Zip(format!(
                        "deflated entry `{}` size mismatch ({} vs {})",
                        entry.name,
                        entry.uncompressed_size,
                        out.len()
                    )));
                }
                Ok(out)
            }
            other => Err(AssetError::Zip(format!(
                "unsupported zip compression method {other} for `{}`",
                entry.name
            ))),
        }
    }
}

fn strip_duplicate_suffix(name: &str) -> &str {
    if let Some((prefix, suffix)) = name.rsplit_once(' ') {
        if !suffix.is_empty() && suffix.bytes().all(|b| b.is_ascii_digit()) {
            return prefix;
        }
    }
    name
}

fn find_eocd(data: &[u8]) -> Result<usize, AssetError> {
    if data.len() < 22 {
        return Err(AssetError::Zip("file too short to be a ZIP archive".into()));
    }
    let min_pos = data.len().saturating_sub(22 + 65_535);
    let mut pos = data.len() - 22;
    loop {
        if data[pos..pos + 4] == EOCD_SIG.to_le_bytes() {
            let comment_len = read_u16(data, pos + 20)? as usize;
            if pos + 22 + comment_len == data.len() {
                return Ok(pos);
            }
        }
        if pos == min_pos {
            break;
        }
        pos -= 1;
    }
    Err(AssetError::Zip("end of central directory signature not found".into()))
}

fn read_u16(data: &[u8], pos: usize) -> Result<u16, AssetError> {
    let slice = data
        .get(pos..pos + 2)
        .ok_or_else(|| AssetError::Zip("unexpected EOF reading u16".into()))?;
    Ok(u16::from_le_bytes([slice[0], slice[1]]))
}

fn read_u32(data: &[u8], pos: usize) -> Result<u32, AssetError> {
    let slice = data
        .get(pos..pos + 4)
        .ok_or_else(|| AssetError::Zip("unexpected EOF reading u32".into()))?;
    Ok(u32::from_le_bytes([slice[0], slice[1], slice[2], slice[3]]))
}

#[cfg(test)]
mod tests {
    use super::*;
    use flate2::{write::DeflateEncoder, Compression};
    use std::io::Write;

    fn build_synthetic_zip(items: &[(&str, &[u8], bool)]) -> Vec<u8> {
        let mut out = Vec::new();
        let mut cd = Vec::new();
        for (name, payload, deflate) in items {
            let offset = out.len() as u32;
            let (method, compressed) = if *deflate {
                let mut enc = DeflateEncoder::new(Vec::new(), Compression::default());
                enc.write_all(payload).unwrap();
                (8u16, enc.finish().unwrap())
            } else {
                (0u16, payload.to_vec())
            };
            let name_bytes = name.as_bytes();
            out.extend_from_slice(&0x0403_4b50u32.to_le_bytes());
            out.extend_from_slice(&20u16.to_le_bytes());
            out.extend_from_slice(&0u16.to_le_bytes());
            out.extend_from_slice(&method.to_le_bytes());
            out.extend_from_slice(&0u16.to_le_bytes());
            out.extend_from_slice(&0u16.to_le_bytes());
            out.extend_from_slice(&0u32.to_le_bytes());
            out.extend_from_slice(&(compressed.len() as u32).to_le_bytes());
            out.extend_from_slice(&(payload.len() as u32).to_le_bytes());
            out.extend_from_slice(&(name_bytes.len() as u16).to_le_bytes());
            out.extend_from_slice(&0u16.to_le_bytes());
            out.extend_from_slice(name_bytes);
            out.extend_from_slice(&compressed);

            cd.extend_from_slice(&0x0201_4b50u32.to_le_bytes());
            cd.extend_from_slice(&20u16.to_le_bytes());
            cd.extend_from_slice(&20u16.to_le_bytes());
            cd.extend_from_slice(&0u16.to_le_bytes());
            cd.extend_from_slice(&method.to_le_bytes());
            cd.extend_from_slice(&0u16.to_le_bytes());
            cd.extend_from_slice(&0u16.to_le_bytes());
            cd.extend_from_slice(&0u32.to_le_bytes());
            cd.extend_from_slice(&(compressed.len() as u32).to_le_bytes());
            cd.extend_from_slice(&(payload.len() as u32).to_le_bytes());
            cd.extend_from_slice(&(name_bytes.len() as u16).to_le_bytes());
            cd.extend_from_slice(&0u16.to_le_bytes());
            cd.extend_from_slice(&0u16.to_le_bytes());
            cd.extend_from_slice(&0u16.to_le_bytes());
            cd.extend_from_slice(&0u16.to_le_bytes());
            cd.extend_from_slice(&0u32.to_le_bytes());
            cd.extend_from_slice(&offset.to_le_bytes());
            cd.extend_from_slice(name_bytes);
        }
        let cd_offset = out.len() as u32;
        let cd_size = cd.len() as u32;
        out.extend_from_slice(&cd);
        out.extend_from_slice(&0x0605_4b50u32.to_le_bytes());
        out.extend_from_slice(&0u16.to_le_bytes());
        out.extend_from_slice(&0u16.to_le_bytes());
        out.extend_from_slice(&(items.len() as u16).to_le_bytes());
        out.extend_from_slice(&(items.len() as u16).to_le_bytes());
        out.extend_from_slice(&cd_size.to_le_bytes());
        out.extend_from_slice(&cd_offset.to_le_bytes());
        out.extend_from_slice(&0u16.to_le_bytes());
        out
    }

    #[test]
    fn stored_and_deflate_roundtrip() {
        let zip_bytes = build_synthetic_zip(&[
            ("plain.txt", b"hello stored", false),
            ("comp.bin", b"hello deflated payload repeated repeated", true),
        ]);
        let archive = ZipArchive::parse(&zip_bytes).unwrap();
        assert_eq!(archive.entries().len(), 2);
        assert_eq!(archive.read("plain.txt").unwrap(), b"hello stored");
        assert_eq!(
            archive.read("comp.bin").unwrap(),
            b"hello deflated payload repeated repeated"
        );
    }

    #[test]
    fn latest_suffixed_entry_fallback() {
        let zip_bytes = build_synthetic_zip(&[
            ("terrain2/dominanttextures 1", b"old", false),
            ("terrain2/dominantTextures 1", b"newest", true),
            ("terrain2/heights1", b"lod1", false),
        ]);
        let archive = ZipArchive::parse(&zip_bytes).unwrap();
        assert_eq!(
            archive
                .read_latest_matching("terrain2/dominantTextures")
                .unwrap(),
            b"newest"
        );
        assert!(matches!(
            archive.read_latest_matching("terrain2/heights"),
            Err(AssetError::NotFound(_))
        ));
    }

    #[test]
    fn rejects_invalid_and_missing() {
        assert!(ZipArchive::parse(b"not a zip").is_err());
        let zip_bytes = build_synthetic_zip(&[("a", b"1", false)]);
        let archive = ZipArchive::parse(&zip_bytes).unwrap();
        assert!(matches!(archive.read("missing"), Err(AssetError::NotFound(_))));
    }

    #[test]
    fn rejects_bogus_uncompressed_size_without_oom() {
        let mut deflate_zip = build_synthetic_zip(&[("tiny.bin", b"small", true)]);
        let cd_pos = deflate_zip
            .windows(4)
            .rposition(|w| w == CENTRAL_DIR_HEADER_SIG.to_le_bytes())
            .unwrap();
        deflate_zip[cd_pos + 24..cd_pos + 28].copy_from_slice(&u32::MAX.to_le_bytes());
        let archive = ZipArchive::parse(&deflate_zip).unwrap();
        assert!(matches!(archive.read("tiny.bin"), Err(AssetError::Zip(_))));

        let mut stored_zip = build_synthetic_zip(&[("plain.bin", b"small", false)]);
        let cd_pos = stored_zip
            .windows(4)
            .rposition(|w| w == CENTRAL_DIR_HEADER_SIG.to_le_bytes())
            .unwrap();
        stored_zip[cd_pos + 24..cd_pos + 28].copy_from_slice(&u32::MAX.to_le_bytes());
        let archive = ZipArchive::parse(&stored_zip).unwrap();
        assert!(matches!(archive.read("plain.bin"), Err(AssetError::Zip(_))));
    }
}
