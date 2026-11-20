//! BigWorld `.cdata` terrain heightmap (`terrain2/heights`), `dominantTextures`,
//! and `system/data/material_kinds.xml` parsers.

use std::collections::BTreeMap;
use std::io::{Cursor, Read};
use std::path::Path;

use flate2::read::ZlibDecoder;
use glam::Vec3;
use wot065_data::xml;

use crate::assets::zip_reader::ZipArchive;
use crate::assets::AssetError;

/// Stored heightmap grid width and height per chunk (`69 x 69`, including 2-vertex
/// overlap border on each side).
pub const HEIGHTMAP_GRID_SIZE: usize = 69;
/// Border vertex offset to the visible `[0..100m)` origin at `(2, 2)`.
pub const HEIGHTMAP_VISIBLE_OFFSET: usize = 2;
/// Visible quads per chunk axis (`64` quads spanning `100.0 m`).
pub const HEIGHTMAP_VISIBLE_QUADS: usize = 64;
/// World span of a single chunk along X and Z in meters.
pub const CHUNK_SIZE_METERS: f32 = 100.0;
/// Meters between adjacent heightmap vertices (`100.0 / 64.0 = 1.5625`).
pub const HEIGHTMAP_SPACING_METERS: f32 = CHUNK_SIZE_METERS / HEIGHTMAP_VISIBLE_QUADS as f32;
/// Grid width and height of `terrain2/dominantTextures` per chunk (`128 x 128`).
pub const DOMINANT_TEX_GRID_SIZE: usize = 128;

/// Ground strength category from `system/data/material_kinds.xml`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GroundStrength {
    Firm,
    Medium,
    Soft,
}

/// Entry from `system/data/material_kinds.xml`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MaterialKind {
    pub id: u8,
    pub desc: String,
    pub ground_strength: Option<GroundStrength>,
    pub effect_material: Option<String>,
}

/// Parsed `system/data/material_kinds.xml` table with terrain texture path lookup.
#[derive(Debug, Clone)]
pub struct MaterialKinds {
    by_id: BTreeMap<u8, MaterialKind>,
    by_desc: BTreeMap<String, u8>,
    terrain_to_kind: BTreeMap<String, (u8, f32)>,
}

impl MaterialKinds {
    /// Load `system/data/material_kinds.xml` relative to `res_dir`.
    pub fn load(res_dir: &Path) -> Result<Self, AssetError> {
        let path = res_dir.join("system/data/material_kinds.xml");
        let bytes = std::fs::read(&path)?;
        Self::parse(&bytes)
    }

    /// Parse `material_kinds.xml` bytes (packed or plain XML).
    pub fn parse(bytes: &[u8]) -> Result<Self, AssetError> {
        let root = xml::parse_xml_bytes(bytes)?;
        let mut by_id = BTreeMap::new();
        let mut by_desc = BTreeMap::new();
        let mut terrain_to_kind: BTreeMap<String, (u8, f32)> = BTreeMap::new();

        for kind_node in root.children_named("kind") {
            let Some(id) = kind_node.child("id").and_then(|n| n.parse_i64()) else {
                continue;
            };
            let Ok(id) = u8::try_from(id) else {
                continue;
            };
            let desc = kind_node
                .child("desc")
                .map(|n| n.text().to_string())
                .unwrap_or_default();
            let ground_strength =
                kind_node
                    .child("ground_strength")
                    .and_then(|n| match n.text() {
                        "firm" => Some(GroundStrength::Firm),
                        "medium" => Some(GroundStrength::Medium),
                        "soft" => Some(GroundStrength::Soft),
                        _ => None,
                    });
            let effect_material = kind_node
                .child("effect_material")
                .map(|n| n.text().to_string())
                .filter(|s| !s.is_empty());

            for tex_node in kind_node.children_named("terrain") {
                let key = normalize_terrain_tex_key(tex_node.text());
                if key.is_empty() {
                    continue;
                }
                let weight = tex_node
                    .child("weight")
                    .and_then(|w| w.parse_f64())
                    .unwrap_or(1.0) as f32;
                let replace = match terrain_to_kind.get(&key) {
                    Some(&(_, prev_w)) => weight > prev_w,
                    None => true,
                };
                if replace {
                    terrain_to_kind.insert(key, (id, weight));
                }
            }

            if !desc.is_empty() {
                by_desc.insert(desc.clone(), id);
            }
            by_id.insert(
                id,
                MaterialKind {
                    id,
                    desc,
                    ground_strength,
                    effect_material,
                },
            );
        }

        Ok(Self {
            by_id,
            by_desc,
            terrain_to_kind,
        })
    }

    pub fn by_id(&self, id: u8) -> Option<&MaterialKind> {
        self.by_id.get(&id)
    }

    pub fn by_desc(&self, desc: &str) -> Option<&MaterialKind> {
        self.by_desc.get(desc).and_then(|id| self.by_id.get(id))
    }

    /// Resolve a terrain texture path (e.g. `"maps/landscape/00_AllTiles/grass_8.dds"`
    /// or `".tga"`) to its [`MaterialKind`].
    pub fn for_terrain_texture(&self, tex_path: &str) -> Option<&MaterialKind> {
        let key = normalize_terrain_tex_key(tex_path);
        let &(id, _) = self.terrain_to_kind.get(&key)?;
        self.by_id.get(&id)
    }
}

/// Decoded terrain heightmap (`69x69`, meters) and dominant texture grid (`128x128`)
/// for one `100m x 100m` chunk.
#[derive(Debug, Clone)]
pub struct TerrainChunk {
    /// Row-major `69 x 69` heights in meters (`row = Z`, `col = X`).
    pub heights: Vec<f32>,
    pub min_y: f32,
    pub max_y: f32,
    /// Row-major `128 x 128` pure indices into [`Self::tex_names`].
    pub dom_tex_indices: Vec<u8>,
    pub tex_names: Vec<String>,
}

impl TerrainChunk {
    /// Parse a `.cdata` ZIP archive into a [`TerrainChunk`].
    pub fn from_cdata_bytes(cdata_bytes: &[u8]) -> Result<Self, AssetError> {
        let zip = ZipArchive::parse(cdata_bytes)?;
        let heights_bytes = zip.read_latest_matching("terrain2/heights")?;
        let dom_tex_bytes = zip.read_latest_matching("terrain2/dominantTextures")?;
        Self::from_sections(&heights_bytes, &dom_tex_bytes)
    }

    /// Parse raw `terrain2/heights` and `terrain2/dominantTextures` section blobs.
    pub fn from_sections(heights_bytes: &[u8], dom_tex_bytes: &[u8]) -> Result<Self, AssetError> {
        let (heights, min_y, max_y) = decode_heights_section(heights_bytes)?;
        let (tex_names, dom_tex_indices) = decode_dominant_textures_section(dom_tex_bytes)?;
        Ok(Self {
            heights,
            min_y,
            max_y,
            dom_tex_indices,
            tex_names,
        })
    }

    /// Height at raw grid coordinates `col` (X, `0..69`) and `row` (Z, `0..69`) in meters.
    pub fn height_at_grid(&self, col: usize, row: usize) -> f32 {
        self.heights[row * HEIGHTMAP_GRID_SIZE + col]
    }

    /// Sample terrain `(height_m, unit_normal)` at chunk-local `(local_x, local_z)` in
    /// `[0.0, 100.0]` using BigWorld's `(x0 ^ z0) & 1` alternating triangle split.
    pub fn sample_local(&self, local_x: f32, local_z: f32) -> (f32, Vec3) {
        let s = HEIGHTMAP_SPACING_METERS;
        let gx = (local_x / s).clamp(0.0, HEIGHTMAP_VISIBLE_QUADS as f32);
        let gz = (local_z / s).clamp(0.0, HEIGHTMAP_VISIBLE_QUADS as f32);

        let x0 = (gx.floor() as usize).min(HEIGHTMAP_VISIBLE_QUADS - 1);
        let z0 = (gz.floor() as usize).min(HEIGHTMAP_VISIBLE_QUADS - 1);
        let fx = gx - x0 as f32;
        let fz = gz - z0 as f32;

        let cx = x0 + HEIGHTMAP_VISIBLE_OFFSET;
        let cz = z0 + HEIGHTMAP_VISIBLE_OFFSET;
        let h00 = self.height_at_grid(cx, cz);
        let h10 = self.height_at_grid(cx + 1, cz);
        let h01 = self.height_at_grid(cx, cz + 1);
        let h11 = self.height_at_grid(cx + 1, cz + 1);

        let (height, dh_dx, dh_dz) = if ((x0 ^ z0) & 1) == 0 {
            if fx >= fz {
                let dhx = h10 - h00;
                let dhz = h11 - h10;
                (h00 + fx * dhx + fz * dhz, dhx, dhz)
            } else {
                let dhx = h11 - h01;
                let dhz = h01 - h00;
                (h00 + fx * dhx + fz * dhz, dhx, dhz)
            }
        } else if fx + fz <= 1.0 {
            let dhx = h10 - h00;
            let dhz = h01 - h00;
            (h00 + fx * dhx + fz * dhz, dhx, dhz)
        } else {
            let dhx = h11 - h01;
            let dhz = h11 - h10;
            let h = h11 + (1.0 - fx) * (h01 - h11) + (1.0 - fz) * (h10 - h11);
            (h, dhx, dhz)
        };

        let normal = Vec3::new(-dh_dx, s, -dh_dz).normalize_or_zero();
        (height, normal)
    }

    /// Dominant texture path at chunk-local `(local_x, local_z)` in `[0.0, 100.0]`.
    pub fn dominant_texture_local(&self, local_x: f32, local_z: f32) -> Option<&str> {
        let scale = DOMINANT_TEX_GRID_SIZE as f32 / CHUNK_SIZE_METERS;
        let col = ((local_x * scale).floor() as isize).clamp(0, DOMINANT_TEX_GRID_SIZE as isize - 1)
            as usize;
        let row = ((local_z * scale).floor() as isize).clamp(0, DOMINANT_TEX_GRID_SIZE as isize - 1)
            as usize;
        let tex_idx = self.dom_tex_indices[row * DOMINANT_TEX_GRID_SIZE + col] as usize;
        self.tex_names.get(tex_idx).map(String::as_str)
    }

    /// Resolved [`MaterialKind`] at chunk-local `(local_x, local_z)` in `[0.0, 100.0]`.
    pub fn dominant_material_local<'a>(
        &self,
        local_x: f32,
        local_z: f32,
        kinds: &'a MaterialKinds,
    ) -> Option<&'a MaterialKind> {
        let tex = self.dominant_texture_local(local_x, local_z)?;
        kinds.for_terrain_texture(tex)
    }
}

fn decode_heights_section(bytes: &[u8]) -> Result<(Vec<f32>, f32, f32), AssetError> {
    if bytes.len() < 36 || &bytes[0..4] != b"hmp\0" {
        return Err(AssetError::Terrain("invalid terrain2/heights header".into()));
    }
    let width = read_u32_le(bytes, 4) as usize;
    let height = read_u32_le(bytes, 8) as usize;
    if width != HEIGHTMAP_GRID_SIZE || height != HEIGHTMAP_GRID_SIZE {
        return Err(AssetError::Terrain(format!(
            "unexpected heightmap dimensions {width}x{height}"
        )));
    }
    let min_y = read_f32_le(bytes, 20);
    let max_y = read_f32_le(bytes, 24);
    if &bytes[32..36] != b"gnpq" {
        return Err(AssetError::Terrain("missing gnpq marker in heights".into()));
    }

    let decoder = png::Decoder::new(Cursor::new(&bytes[36..]));
    let mut reader = decoder
        .read_info()
        .map_err(|e| AssetError::Png(format!("heights PNG header: {e}")))?;
    let info = reader.info();
    if info.width != width as u32
        || info.height != height as u32
        || info.color_type != png::ColorType::Rgba
        || info.bit_depth != png::BitDepth::Eight
    {
        return Err(AssetError::Png(format!(
            "unexpected heights PNG format: {}x{} {:?} {:?}",
            info.width, info.height, info.color_type, info.bit_depth
        )));
    }
    let mut rgba = vec![0u8; reader.output_buffer_size()];
    let out_info = reader
        .next_frame(&mut rgba)
        .map_err(|e| AssetError::Png(format!("heights PNG frame: {e}")))?;
    let pixel_bytes = &rgba[..out_info.buffer_size()];
    if pixel_bytes.len() != width * height * 4 {
        return Err(AssetError::Png("unexpected heights pixel byte count".into()));
    }

    let heights = pixel_bytes
        .chunks_exact(4)
        .map(|c| i32::from_le_bytes([c[0], c[1], c[2], c[3]]) as f32 * 0.001)
        .collect();
    Ok((heights, min_y, max_y))
}

fn decode_dominant_textures_section(bytes: &[u8]) -> Result<(Vec<String>, Vec<u8>), AssetError> {
    if bytes.len() < 32 || &bytes[0..4] != b"mat\0" {
        return Err(AssetError::Terrain(
            "invalid terrain2/dominantTextures header".into(),
        ));
    }
    let num_tex = read_u32_le(bytes, 8) as usize;
    let str_len = read_u32_le(bytes, 12) as usize;
    let width = read_u32_le(bytes, 16) as usize;
    let height = read_u32_le(bytes, 20) as usize;
    if width != DOMINANT_TEX_GRID_SIZE || height != DOMINANT_TEX_GRID_SIZE {
        return Err(AssetError::Terrain(format!(
            "unexpected dominantTextures dimensions {width}x{height}"
        )));
    }
    if num_tex > 0 && str_len == 0 {
        return Err(AssetError::Terrain(
            "dominantTextures has non-zero entry count with zero string stride".into(),
        ));
    }
    let table_end = num_tex
        .checked_mul(str_len)
        .and_then(|n| 32usize.checked_add(n))
        .and_then(|end| end.checked_add(12).map(|header_end| (end, header_end)))
        .filter(|&(_, header_end)| header_end <= bytes.len())
        .map(|(end, _)| end)
        .ok_or_else(|| AssetError::Terrain("dominantTextures table out of bounds".into()))?;

    let mut tex_names = Vec::with_capacity(num_tex);
    for i in 0..num_tex {
        let slot = &bytes[32 + i * str_len..32 + (i + 1) * str_len];
        let end = slot.iter().position(|&b| b == 0).unwrap_or(slot.len());
        tex_names.push(String::from_utf8_lossy(&slot[..end]).into_owned());
    }

    if &bytes[table_end..table_end + 4] != b"\x00piz" {
        return Err(AssetError::Terrain(
            "missing \\x00piz marker in dominantTextures".into(),
        ));
    }
    let cell_count = DOMINANT_TEX_GRID_SIZE * DOMINANT_TEX_GRID_SIZE;
    let expected_len = read_u32_le(bytes, table_end + 8) as usize;
    if expected_len != cell_count {
        return Err(AssetError::Terrain(format!(
            "dominantTextures declared {expected_len} cells, expected {cell_count}"
        )));
    }
    let mut indices = Vec::with_capacity(cell_count);
    let mut z = ZlibDecoder::new(&bytes[table_end + 12..]).take((cell_count + 1) as u64);
    z.read_to_end(&mut indices)
        .map_err(|e| AssetError::Terrain(format!("dominantTextures zlib: {e}")))?;

    if indices.len() != cell_count {
        return Err(AssetError::Terrain(format!(
            "expected {cell_count} dominant texture cells, got {}{}",
            indices.len().min(cell_count),
            if indices.len() > cell_count { "+" } else { "" }
        )));
    }

    Ok((tex_names, indices))
}

fn normalize_terrain_tex_key(raw: &str) -> String {
    let mut s = raw.trim().replace('\\', "/").to_ascii_lowercase();
    if let Some(last_slash) = s.rfind('/') {
        if let Some(dot_rel) = s[last_slash + 1..].rfind('.') {
            s.truncate(last_slash + 1 + dot_rel);
        }
    } else if let Some(dot) = s.rfind('.') {
        s.truncate(dot);
    }
    s
}

fn read_u32_le(bytes: &[u8], pos: usize) -> u32 {
    u32::from_le_bytes([bytes[pos], bytes[pos + 1], bytes[pos + 2], bytes[pos + 3]])
}

fn read_f32_le(bytes: &[u8], pos: usize) -> f32 {
    f32::from_le_bytes([bytes[pos], bytes[pos + 1], bytes[pos + 2], bytes[pos + 3]])
}

#[cfg(test)]
mod tests {
    use super::*;
    use flate2::{write::ZlibEncoder, Compression};
    use std::io::Write;

    fn build_synthetic_heights_section(grid: &[i32; 69 * 69]) -> Vec<u8> {
        let mut rgba = Vec::with_capacity(69 * 69 * 4);
        let mut min_mm = i32::MAX;
        let mut max_mm = i32::MIN;
        for &mm in grid {
            min_mm = min_mm.min(mm);
            max_mm = max_mm.max(mm);
            rgba.extend_from_slice(&mm.to_le_bytes());
        }
        let mut png_buf = Vec::new();
        {
            let mut enc = png::Encoder::new(&mut png_buf, 69, 69);
            enc.set_color(png::ColorType::Rgba);
            enc.set_depth(png::BitDepth::Eight);
            let mut writer = enc.write_header().unwrap();
            writer.write_image_data(&rgba).unwrap();
        }
        let mut out = Vec::new();
        out.extend_from_slice(b"hmp\0");
        out.extend_from_slice(&69u32.to_le_bytes());
        out.extend_from_slice(&69u32.to_le_bytes());
        out.extend_from_slice(&0u32.to_le_bytes());
        out.extend_from_slice(&4u32.to_le_bytes());
        out.extend_from_slice(&(min_mm as f32 * 0.001).to_le_bytes());
        out.extend_from_slice(&(max_mm as f32 * 0.001).to_le_bytes());
        out.extend_from_slice(&0u32.to_le_bytes());
        out.extend_from_slice(b"gnpq");
        out.extend_from_slice(&png_buf);
        out
    }

    fn build_synthetic_dom_tex(names: &[&str], indices: &[u8; 128 * 128]) -> Vec<u8> {
        let str_len = 48u32;
        let mut out = Vec::new();
        out.extend_from_slice(b"mat\0");
        out.extend_from_slice(&1u32.to_le_bytes());
        out.extend_from_slice(&(names.len() as u32).to_le_bytes());
        out.extend_from_slice(&str_len.to_le_bytes());
        out.extend_from_slice(&128u32.to_le_bytes());
        out.extend_from_slice(&128u32.to_le_bytes());
        out.extend_from_slice(&0u32.to_le_bytes());
        out.extend_from_slice(&0u32.to_le_bytes());
        for name in names {
            let mut slot = vec![0u8; str_len as usize];
            let nb = name.as_bytes();
            slot[..nb.len()].copy_from_slice(nb);
            out.extend_from_slice(&slot);
        }
        out.extend_from_slice(b"\x00piz");
        out.extend_from_slice(&0x42af_9021u32.to_le_bytes());
        out.extend_from_slice(&(128u32 * 128u32).to_le_bytes());
        let mut z = ZlibEncoder::new(Vec::new(), Compression::default());
        z.write_all(indices).unwrap();
        out.extend_from_slice(&z.finish().unwrap());
        out
    }

    #[test]
    fn synthetic_heights_barycentric_alternating_diagonal_and_dom_tex() {
        let mut grid = [0i32; 69 * 69];
        grid[2 * 69 + 3] = 10_000;
        let heights_sec = build_synthetic_heights_section(&grid);

        let mut dom = [0u8; 128 * 128];
        dom[64] = 1;
        let dom_sec = build_synthetic_dom_tex(
            &[
                "maps/landscape/00_AllTiles/grass_8.dds",
                "maps/landscape/04_Himmelsdorf/Color_tex.tga",
            ],
            &dom,
        );

        let chunk = TerrainChunk::from_sections(&heights_sec, &dom_sec).unwrap();
        let s = HEIGHTMAP_SPACING_METERS;
        let (h_a, n_a) = chunk.sample_local(0.75 * s, 0.25 * s);
        assert!((h_a - 5.0).abs() < 1e-4, "got {h_a}");
        assert!(n_a.y > 0.0 && n_a.x < 0.0 && n_a.z > 0.0);
        let (h_b, n_b) = chunk.sample_local(0.25 * s, 0.75 * s);
        assert!(h_b.abs() < 1e-4, "got {h_b}");
        assert!((n_b - Vec3::Y).length() < 1e-4);

        let (h_c, _) = chunk.sample_local(1.25 * s, 0.25 * s);
        assert!((h_c - 5.0).abs() < 1e-4, "got {h_c}");

        assert_eq!(
            chunk.dominant_texture_local(50.1, 0.1),
            Some("maps/landscape/04_Himmelsdorf/Color_tex.tga")
        );
        assert_eq!(
            chunk.dominant_texture_local(0.1, 0.1),
            Some("maps/landscape/00_AllTiles/grass_8.dds")
        );
    }

    #[test]
    fn material_kinds_resolves_tga_and_weights() {
        let xml = br#"<root>
            <kind><id>101</id><desc>grass</desc><ground_strength>medium</ground_strength><effect_material>ground</effect_material>
                <terrain>maps/landscape/00_AllTiles/grass_8</terrain>
            </kind>
            <kind><id>103</id><desc>sand</desc><ground_strength>medium</ground_strength><effect_material>sand</effect_material>
                <terrain>maps/landscape/00_AllTiles/sand_20<weight>2.0</weight></terrain>
            </kind>
            <kind><id>109</id><desc>firm_road</desc><ground_strength>firm</ground_strength>
                <terrain>maps/landscape/04_Himmelsdorf/Color_Tex</terrain>
                <terrain>maps/landscape/00_AllTiles/sand_20</terrain>
            </kind>
        </root>"#;
        let kinds = MaterialKinds::parse(xml).unwrap();
        assert_eq!(
            kinds
                .for_terrain_texture("maps/landscape/04_Himmelsdorf/Color_tex.tga")
                .map(|k| k.id),
            Some(109)
        );
        assert_eq!(
            kinds
                .for_terrain_texture("maps/landscape/00_AllTiles/sand_20.dds")
                .map(|k| k.id),
            Some(103)
        );
    }

    #[test]
    fn rejects_bogus_dominant_textures_lengths_and_oversized_zlib() {
        let mut bogus_expected = build_synthetic_dom_tex(&["a.dds"], &[0u8; 128 * 128]);
        let piz_pos = bogus_expected.windows(4).position(|w| w == b"\x00piz").unwrap();
        bogus_expected[piz_pos + 8..piz_pos + 12].copy_from_slice(&u32::MAX.to_le_bytes());
        assert!(matches!(
            decode_dominant_textures_section(&bogus_expected),
            Err(AssetError::Terrain(_))
        ));

        let mut oversized = bogus_expected[..piz_pos + 12].to_vec();
        oversized[piz_pos + 8..piz_pos + 12].copy_from_slice(&((128 * 128) as u32).to_le_bytes());
        let mut z = ZlibEncoder::new(Vec::new(), Compression::default());
        z.write_all(&vec![0u8; 64 * 1024]).unwrap();
        oversized.extend_from_slice(&z.finish().unwrap());
        assert!(matches!(
            decode_dominant_textures_section(&oversized),
            Err(AssetError::Terrain(_))
        ));

        let mut zero_stride = Vec::new();
        zero_stride.extend_from_slice(b"mat\0");
        zero_stride.extend_from_slice(&1u32.to_le_bytes());
        zero_stride.extend_from_slice(&u32::MAX.to_le_bytes());
        zero_stride.extend_from_slice(&0u32.to_le_bytes());
        zero_stride.extend_from_slice(&128u32.to_le_bytes());
        zero_stride.extend_from_slice(&128u32.to_le_bytes());
        zero_stride.extend_from_slice(&0u32.to_le_bytes());
        zero_stride.extend_from_slice(&0u32.to_le_bytes());
        zero_stride.extend_from_slice(b"\x00piz");
        zero_stride.extend_from_slice(&0u32.to_le_bytes());
        zero_stride.extend_from_slice(&0u32.to_le_bytes());
        assert!(matches!(
            decode_dominant_textures_section(&zero_stride),
            Err(AssetError::Terrain(_))
        ));
    }
}
