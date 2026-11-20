//! BigWorld `BinSection` (`0x42a14e65`) container parser for `.primitives` files.

use std::collections::BTreeMap;

use crate::assets::AssetError;

/// BigWorld `BinSection` magic (`0x42a14e65` stored little-endian).
pub const BIN_SECTION_MAGIC: u32 = 0x42a1_4e65;

/// Zero-copy BigWorld `BinSection` index decoder.
pub struct BinSectionMap;

impl BinSectionMap {
    /// Parse a BigWorld binary section file into `section_name -> raw_bytes`.
    pub fn parse(bytes: &[u8]) -> Result<BTreeMap<String, &[u8]>, AssetError> {
        if bytes.len() < 8 {
            return Err(AssetError::BinSection("file shorter than 8 bytes".into()));
        }
        let magic = u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
        if magic != BIN_SECTION_MAGIC {
            return Err(AssetError::BinSection(format!(
                "bad BinSection magic {magic:#010x}"
            )));
        }
        let len = bytes.len();
        let idx_len =
            u32::from_le_bytes([bytes[len - 4], bytes[len - 3], bytes[len - 2], bytes[len - 1]])
                as usize;
        if idx_len > len - 8 {
            return Err(AssetError::BinSection(format!(
                "index length {idx_len} exceeds body size {}",
                len - 8
            )));
        }
        let idx_start = len - 4 - idx_len;
        let idx_end = len - 4;

        let mut sections = BTreeMap::new();
        let mut idx_pos = idx_start;
        let mut data_off = 4usize;

        while idx_pos < idx_end {
            if idx_pos + 24 > idx_end {
                return Err(AssetError::BinSection("truncated index entry header".into()));
            }
            let blob_len = read_u32_le(bytes, idx_pos) as usize;
            let name_len = read_u32_le(bytes, idx_pos + 20) as usize;
            idx_pos += 24;

            let name_pad = (4 - (name_len % 4)) % 4;
            let entry_tail_end = idx_pos
                .checked_add(name_len)
                .and_then(|n| n.checked_add(name_pad))
                .filter(|&end| end <= idx_end)
                .ok_or_else(|| AssetError::BinSection("truncated index section name".into()))?;
            let name = String::from_utf8_lossy(&bytes[idx_pos..idx_pos + name_len]).into_owned();
            idx_pos = entry_tail_end;

            let data_end = data_off
                .checked_add(blob_len)
                .filter(|&end| end <= idx_start)
                .ok_or_else(|| {
                    AssetError::BinSection(format!("section `{name}` payload out of bounds"))
                })?;
            sections.insert(name, &bytes[data_off..data_end]);
            let aligned = (blob_len + 3) & !3;
            data_off = data_off.saturating_add(aligned).min(idx_start);
        }

        Ok(sections)
    }
}

/// Extract the null-terminated vertex format identifier from a `.primitives`
/// `vertices` section header (tolerating trailing `\0` + stack garbage in the
/// 64-byte format string).
pub fn parse_vertex_format(vertices_bytes: &[u8]) -> Option<&str> {
    let header = &vertices_bytes[..vertices_bytes.len().min(64)];
    let nul_pos = header.iter().position(|&b| b == 0).unwrap_or(header.len());
    let s = std::str::from_utf8(&header[..nul_pos]).ok()?.trim();
    (!s.is_empty()).then_some(s)
}

fn read_u32_le(bytes: &[u8], pos: usize) -> u32 {
    u32::from_le_bytes([bytes[pos], bytes[pos + 1], bytes[pos + 2], bytes[pos + 3]])
}

#[cfg(test)]
mod tests {
    use super::*;

    fn build_bin_section(entries: &[(&str, &[u8])]) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(&BIN_SECTION_MAGIC.to_le_bytes());
        let mut index = Vec::new();
        for (name, data) in entries {
            out.extend_from_slice(data);
            while out.len() % 4 != 0 {
                out.push(0);
            }
            index.extend_from_slice(&(data.len() as u32).to_le_bytes());
            index.extend_from_slice(&[0u8; 16]);
            let nb = name.as_bytes();
            index.extend_from_slice(&(nb.len() as u32).to_le_bytes());
            index.extend_from_slice(nb);
            while index.len() % 4 != 0 {
                index.push(0);
            }
        }
        let idx_len = index.len() as u32;
        out.extend_from_slice(&index);
        out.extend_from_slice(&idx_len.to_le_bytes());
        out
    }

    #[test]
    fn parses_aligned_sections_and_vertex_format_with_trailing_null_garbage() {
        let mut verts = [0u8; 64];
        verts[..8].copy_from_slice(b"xyznuvtb");
        verts[8] = 0;
        verts[9..20].copy_from_slice(b"urretRotato");
        let blob = build_bin_section(&[("vertices", &verts), ("bsp2", b"BSP\0\x00\x00\x00")]);
        let map = BinSectionMap::parse(&blob).unwrap();
        assert_eq!(map.len(), 2);
        assert_eq!(parse_vertex_format(map["vertices"]), Some("xyznuvtb"));
        assert_eq!(map["bsp2"], b"BSP\0\x00\x00\x00");
    }

    #[test]
    fn rejects_bad_magic_and_truncated_tail() {
        assert!(BinSectionMap::parse(b"bad!1234").is_err());
        let mut bad = BIN_SECTION_MAGIC.to_le_bytes().to_vec();
        bad.extend_from_slice(&999u32.to_le_bytes());
        assert!(BinSectionMap::parse(&bad).is_err());
    }
}
