//! BigWorld `.chunk`, `space.settings`, `.model`/`.visual`, and `.vlo` parsers.

use std::path::{Path, PathBuf};

use glam::{Mat4, Vec3, Vec4};
use wot065_data::xml::{self, XmlNode};

use crate::assets::AssetError;

/// Outer vs inner BigWorld chunk filename suffix (`'o'` vs `'i'`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ChunkKind {
    Outer,
    Inner,
}

/// Decoded chunk grid coordinates (`grid_x * 100.0`, `grid_z * 100.0` meters).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ChunkCoords {
    pub grid_x: i32,
    pub grid_z: i32,
    pub kind: ChunkKind,
}

impl ChunkCoords {
    /// World-space `(x, z)` SW corner of this `100m x 100m` chunk in meters.
    pub fn world_origin_xz(&self) -> (f32, f32) {
        (self.grid_x as f32 * 100.0, self.grid_z as f32 * 100.0)
    }

    /// World-space `(x, 0.0, z)` translation vector of this chunk in meters.
    pub fn world_origin(&self) -> Vec3 {
        let (x, z) = self.world_origin_xz();
        Vec3::new(x, 0.0, z)
    }

    /// Packed 16-bit BigWorld / `DestructiblesCache` chunk ID:
    /// `((grid_x + 127) << 8) | (grid_z + 127)`.
    pub fn chunk_id(&self) -> u16 {
        crate::assets::destructibles::chunk_id_from_indexes(self.grid_x, self.grid_z)
    }
}

/// Decode a BigWorld hex chunk stem/filename (e.g. `"0000fffdo"` or `"0000fffdo.chunk"`).
pub fn decode_chunk_coords(name_or_stem: &str) -> Result<ChunkCoords, AssetError> {
    let file_name = Path::new(name_or_stem)
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or(name_or_stem);
    let stem = file_name
        .split('.')
        .next()
        .ok_or_else(|| AssetError::Chunk(format!("empty chunk name `{name_or_stem}`")))?;
    if !stem.is_ascii() || stem.len() != 9 {
        return Err(AssetError::Chunk(format!(
            "expected 9-char ASCII hex chunk stem, got `{stem}`"
        )));
    }
    let kind = match stem.as_bytes()[8] {
        b'o' | b'O' => ChunkKind::Outer,
        b'i' | b'I' => ChunkKind::Inner,
        other => {
            return Err(AssetError::Chunk(format!(
                "invalid chunk suffix byte {other:#x} in `{stem}`"
            )));
        }
    };
    let gx_u16 = u16::from_str_radix(&stem[0..4], 16)
        .map_err(|e| AssetError::Chunk(format!("bad X hex in `{stem}`: {e}")))?;
    let gz_u16 = u16::from_str_radix(&stem[4..8], 16)
        .map_err(|e| AssetError::Chunk(format!("bad Z hex in `{stem}`: {e}")))?;
    Ok(ChunkCoords {
        grid_x: (gx_u16 as i16) as i32,
        grid_z: (gz_u16 as i16) as i32,
        kind,
    })
}

/// Parse BigWorld's 12-float 3x4 row-major matrix string into a [`Mat4`].
///
/// BigWorld transforms points as `p * M_row`, where the 12 floats are
/// `(row0.xyz, row1.xyz, row2.xyz, translation.xyz)`. Putting those 4 vectors
/// into the 4 columns of a column-major `glam::Mat4` ensures `mat.transform_point3(p)`
/// produces the exact same world coordinates.
pub fn parse_bigworld_matrix(text: &str) -> Result<Mat4, AssetError> {
    let vals = parse_f32_list(text, 12, "matrix")?;
    Ok(Mat4::from_cols(
        Vec4::new(vals[0], vals[1], vals[2], 0.0),
        Vec4::new(vals[3], vals[4], vals[5], 0.0),
        Vec4::new(vals[6], vals[7], vals[8], 0.0),
        Vec4::new(vals[9], vals[10], vals[11], 1.0),
    ))
}

/// Chunk grid bounds from `space.settings` `<bounds>` (`minX..=maxX`, `minY..=maxY` where
/// BigWorld's `Y` is map `Z` in 100 m grid units).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SpaceBounds {
    pub min_x: i32,
    pub max_x: i32,
    pub min_y: i32,
    pub max_y: i32,
}

/// Parsed `spaces/<map>/space.settings`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpaceSettings {
    pub bounds: SpaceBounds,
}

impl SpaceSettings {
    pub fn load(path: &Path) -> Result<Self, AssetError> {
        let bytes = std::fs::read(path)?;
        Self::parse(&bytes)
    }

    pub fn parse(bytes: &[u8]) -> Result<Self, AssetError> {
        let root = xml::parse_xml_bytes(bytes)?;
        let bounds = root
            .child("bounds")
            .ok_or_else(|| AssetError::Chunk("missing <bounds> in space.settings".into()))?;
        let read_bound = |name: &str| -> Result<i32, AssetError> {
            bounds
                .child(name)
                .and_then(|n| n.parse_i64())
                .map(|v| v as i32)
                .ok_or_else(|| AssetError::Chunk(format!("missing/invalid <bounds/{name}>")))
        };
        Ok(Self {
            bounds: SpaceBounds {
                min_x: read_bound("minX")?,
                max_x: read_bound("maxX")?,
                min_y: read_bound("minY")?,
                max_y: read_bound("maxY")?,
            },
        })
    }
}

/// Static `<model>` placement inside a `.chunk`.
#[derive(Debug, Clone, PartialEq)]
pub struct ChunkModel {
    pub resource: String,
    /// Client engine destructible slot: position among the chunk's `<model>` and
    /// `<speedtree>` sections in document order.
    pub destr_index: usize,
    pub local_transform: Mat4,
    pub world_transform: Mat4,
}

/// `<speedtree>` placement inside a `.chunk`.
#[derive(Debug, Clone, PartialEq)]
pub struct ChunkSpeedTree {
    pub spt: String,
    pub seed: i32,
    /// Client engine destructible slot, shared with `<model>` sections (see [`ChunkModel`]).
    pub destr_index: usize,
    pub local_transform: Mat4,
    pub world_transform: Mat4,
}

/// `SpawnPoint` `<UserDataObject>` inside a `.chunk`.
#[derive(Debug, Clone, PartialEq)]
pub struct SpawnPoint {
    pub team: u8,
    pub preferable_type: Option<String>,
    pub world_transform: Mat4,
    pub position: Vec3,
    pub yaw: f32,
}

/// `<ControlPoint>` base capture zone inside a `.chunk`.
#[derive(Debug, Clone, PartialEq)]
pub struct ControlPoint {
    pub team: u8,
    pub radius: f32,
    pub points_per_second: f32,
    pub max_points_per_second: f32,
    pub stop_invaders: bool,
    pub world_transform: Mat4,
    pub position: Vec3,
}

/// Parsed contents of one `.chunk` file.
#[derive(Debug, Clone)]
pub struct ChunkData {
    pub coords: ChunkCoords,
    pub models: Vec<ChunkModel>,
    pub speedtrees: Vec<ChunkSpeedTree>,
    pub spawns: Vec<SpawnPoint>,
    pub control_points: Vec<ControlPoint>,
    pub vlo_uids: Vec<String>,
}

/// Water rectangle parsed from `<uid>.vlo`.
#[derive(Debug, Clone, PartialEq)]
pub struct WaterVolume {
    pub uid: String,
    pub position: Vec3,
    pub orientation: f32,
    pub size: Vec3,
    pub depth: f32,
}

impl WaterVolume {
    pub fn load(space_dir: &Path, uid: &str) -> Result<Self, AssetError> {
        let path = space_dir.join(format!("{uid}.vlo"));
        let bytes = std::fs::read(&path)?;
        Self::parse(uid, &bytes)
    }

    pub fn parse(uid: impl Into<String>, bytes: &[u8]) -> Result<Self, AssetError> {
        let root = xml::parse_xml_bytes(bytes)?;
        let water = root
            .child("water")
            .ok_or_else(|| AssetError::Chunk("missing <water> in .vlo".into()))?;
        let position = water
            .child("position")
            .ok_or_else(|| AssetError::Chunk("missing <water/position>".into()))
            .and_then(|n| parse_vec3(n.text()))?;
        let size = water
            .child("size")
            .ok_or_else(|| AssetError::Chunk("missing <water/size>".into()))
            .and_then(|n| parse_vec3(n.text()))?;
        let orientation = water
            .child("orientation")
            .and_then(|n| n.parse_f64())
            .unwrap_or(0.0) as f32;
        let depth = water
            .child("depth")
            .and_then(|n| n.parse_f64())
            .unwrap_or(0.0) as f32;

        Ok(Self {
            uid: uid.into(),
            position,
            orientation,
            size,
            depth,
        })
    }

    /// Check whether `(x, z)` lies inside this rotated water rectangle.
    pub fn contains_xz(&self, x: f32, z: f32) -> bool {
        let dx = x - self.position.x;
        let dz = z - self.position.z;
        let (sin, cos) = self.orientation.sin_cos();
        let lx = dx * cos - dz * sin;
        let lz = dx * sin + dz * cos;
        lx.abs() <= self.size.x * 0.5 && lz.abs() <= self.size.z * 0.5
    }
}

/// Resolved `.model` -> `.visual` / `.primitives` target.
#[derive(Debug, Clone, PartialEq)]
pub struct ResolvedModel {
    pub visual_path: String,
    pub primitives_path: PathBuf,
    pub is_nodefull: bool,
    pub visibility_box: Option<(Vec3, Vec3)>,
}

/// Parse one `.chunk` file's raw bytes.
pub fn parse_chunk_bytes(coords: ChunkCoords, bytes: &[u8]) -> Result<ChunkData, AssetError> {
    let root = xml::parse_xml_bytes(bytes)?;
    let chunk_origin = Mat4::from_translation(coords.world_origin());

    let mut models = Vec::new();
    let mut speedtrees = Vec::new();
    let mut spawns = Vec::new();
    let mut control_points = Vec::new();
    let mut vlo_uids = Vec::new();
    // The client engine numbers destructibles over all `<model>` and `<speedtree>` sections.
    let mut next_destr_index = 0usize;

    for child in &root.children {
        match child.name.as_str() {
            "model" => {
                let Some(resource) = child.child("resource").map(|n| n.text().to_string()) else {
                    continue;
                };
                if resource.is_empty() {
                    continue;
                }
                let local_transform = read_transform_or_identity(child)?;
                models.push(ChunkModel {
                    resource,
                    destr_index: next_destr_index,
                    local_transform,
                    world_transform: chunk_origin * local_transform,
                });
                next_destr_index += 1;
            }
            "speedtree" => {
                let Some(spt) = child.child("spt").map(|n| n.text().to_string()) else {
                    continue;
                };
                if spt.is_empty() {
                    continue;
                }
                let seed = child
                    .child("seed")
                    .and_then(|n| n.parse_i64())
                    .unwrap_or(0) as i32;
                let local_transform = read_transform_or_identity(child)?;
                speedtrees.push(ChunkSpeedTree {
                    spt,
                    seed,
                    destr_index: next_destr_index,
                    local_transform,
                    world_transform: chunk_origin * local_transform,
                });
                next_destr_index += 1;
            }
            "UserDataObject" => {
                let udo_type = child.child("type").map(|n| n.text()).unwrap_or("");
                if udo_type != "SpawnPoint" {
                    continue;
                }
                let local_transform = read_transform_or_identity(child)?;
                let world_transform = chunk_origin * local_transform;
                let props = child.child("properties");
                let team = props
                    .and_then(|p| p.child("team"))
                    .and_then(|n| n.parse_i64())
                    .unwrap_or(0) as u8;
                let preferable_type = props
                    .and_then(|p| p.child("preferableType"))
                    .map(|n| n.text().trim())
                    .filter(|s| !s.is_empty() && *s != "<any>")
                    .map(str::to_string);
                let position = world_transform.w_axis.truncate();
                let fwd = world_transform.z_axis.truncate();
                let yaw = fwd.x.atan2(fwd.z);
                spawns.push(SpawnPoint {
                    team,
                    preferable_type,
                    world_transform,
                    position,
                    yaw,
                });
            }
            "ControlPoint" => {
                let local_transform = read_transform_or_identity(child)?;
                let world_transform = chunk_origin * local_transform;
                let position = world_transform.w_axis.truncate();
                let team = child
                    .child("team")
                    .and_then(|n| n.parse_i64())
                    .unwrap_or(0) as u8;
                let radius = child
                    .child("radius")
                    .and_then(|n| n.parse_f64())
                    .unwrap_or(50.0) as f32;
                let points_per_second = child
                    .child("pointsPerSecond")
                    .and_then(|n| n.parse_f64())
                    .unwrap_or(1.0) as f32;
                let max_points_per_second = child
                    .child("maxPointsPerSecond")
                    .and_then(|n| n.parse_f64())
                    .unwrap_or(3.0) as f32;
                let stop_invaders = child
                    .child("stopInvaders")
                    .map(|n| matches!(n.text(), "true" | "True" | "1"))
                    .unwrap_or(false);
                control_points.push(ControlPoint {
                    team,
                    radius,
                    points_per_second,
                    max_points_per_second,
                    stop_invaders,
                    world_transform,
                    position,
                });
            }
            "vlo" => {
                let vlo_type = child.child("type").map(|n| n.text()).unwrap_or("");
                if vlo_type == "water" {
                    if let Some(uid) = child.child("uid").map(|n| n.text().trim()) {
                        if !uid.is_empty() {
                            vlo_uids.push(uid.to_string());
                        }
                    }
                }
            }
            _ => {}
        }
    }

    Ok(ChunkData {
        coords,
        models,
        speedtrees,
        spawns,
        control_points,
        vlo_uids,
    })
}

/// Load and parse all outer (`*o.chunk`) chunks in `space_dir`.
pub fn load_space_chunks(space_dir: &Path) -> Result<Vec<ChunkData>, AssetError> {
    let mut paths: Vec<PathBuf> = std::fs::read_dir(space_dir)?
        .filter_map(|e| e.ok().map(|ent| ent.path()))
        .filter(|p| {
            p.extension().and_then(|e| e.to_str()) == Some("chunk")
                && p.file_stem()
                    .and_then(|s| s.to_str())
                    .is_some_and(|s| s.ends_with('o') || s.ends_with('O'))
        })
        .collect();
    paths.sort();

    let mut chunks = Vec::with_capacity(paths.len());
    for path in paths {
        let name = path
            .file_name()
            .and_then(|s| s.to_str())
            .ok_or_else(|| AssetError::Chunk(format!("invalid path {}", path.display())))?;
        let coords = decode_chunk_coords(name)?;
        let bytes = std::fs::read(&path)?;
        chunks.push(parse_chunk_bytes(coords, &bytes)?);
    }
    Ok(chunks)
}

/// Resolve a `.model` resource path (following `<parent>` links if needed) to its
/// `.visual` stem and `.primitives` file on disk.
pub fn resolve_model_visual(res_dir: &Path, model_rel: &str) -> Result<ResolvedModel, AssetError> {
    let mut current_rel = model_rel.trim().replace('\\', "/");
    let mut visibility_box: Option<(Vec3, Vec3)> = None;

    for _ in 0..8 {
        let path = res_dir.join(&current_rel);
        let root = xml::read_xml(&path)?;
        if visibility_box.is_none() {
            visibility_box = parse_aabb_node(&root, "visibilityBox");
        }
        if let Some(vis) = root
            .child("nodelessVisual")
            .map(|n| n.text())
            .filter(|s| !s.is_empty())
        {
            return Ok(ResolvedModel {
                visual_path: vis.to_string(),
                primitives_path: res_dir.join(format!("{vis}.primitives")),
                is_nodefull: false,
                visibility_box,
            });
        }
        if let Some(vis) = root
            .child("nodefullVisual")
            .map(|n| n.text())
            .filter(|s| !s.is_empty())
        {
            return Ok(ResolvedModel {
                visual_path: vis.to_string(),
                primitives_path: res_dir.join(format!("{vis}.primitives")),
                is_nodefull: true,
                visibility_box,
            });
        }
        if let Some(parent) = root
            .child("parent")
            .map(|n| n.text())
            .filter(|s| !s.is_empty())
        {
            current_rel = if parent.ends_with(".model") {
                parent.to_string()
            } else {
                format!("{parent}.model")
            };
        } else {
            break;
        }
    }

    Err(AssetError::NotFound(format!(
        "no nodelessVisual/nodefullVisual for model `{model_rel}`"
    )))
}

fn read_transform_or_identity(node: &XmlNode) -> Result<Mat4, AssetError> {
    match node.child("transform") {
        Some(t) if !t.text().is_empty() => parse_bigworld_matrix(t.text()),
        _ => Ok(Mat4::IDENTITY),
    }
}

fn parse_aabb_node(node: &XmlNode, child_name: &str) -> Option<(Vec3, Vec3)> {
    let box_node = node.child(child_name)?;
    let min = parse_vec3(box_node.child("min")?.text()).ok()?;
    let max = parse_vec3(box_node.child("max")?.text()).ok()?;
    Some((min, max))
}

fn parse_vec3(text: &str) -> Result<Vec3, AssetError> {
    let v = parse_f32_list(text, 3, "Vec3")?;
    Ok(Vec3::new(v[0], v[1], v[2]))
}

fn parse_f32_list(text: &str, expected: usize, kind: &str) -> Result<Vec<f32>, AssetError> {
    let mut vals = Vec::with_capacity(expected);
    for tok in text.split_whitespace() {
        let f: f32 = tok
            .parse()
            .map_err(|e| AssetError::Chunk(format!("bad float `{tok}` in {kind}: {e}")))?;
        vals.push(f);
    }
    if vals.len() != expected {
        return Err(AssetError::Chunk(format!(
            "expected {expected} floats in {kind}, got {}",
            vals.len()
        )));
    }
    Ok(vals)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hex_coords_decode_positive_and_negative() {
        let c = decode_chunk_coords("0000fffdo").unwrap();
        assert_eq!(c.grid_x, 0);
        assert_eq!(c.grid_z, -3);
        assert_eq!(c.kind, ChunkKind::Outer);
        assert_eq!(c.world_origin_xz(), (0.0, -300.0));

        let c2 = decode_chunk_coords("fffe0004i.chunk").unwrap();
        assert_eq!(c2.grid_x, -2);
        assert_eq!(c2.grid_z, 4);
        assert_eq!(c2.kind, ChunkKind::Inner);
        assert_eq!(c2.world_origin_xz(), (-200.0, 400.0));
    }

    #[test]
    fn hex_coords_rejects_non_ascii_nine_byte_stems_without_panic() {
        for bad in ["äöüäo", "äa€äo", "äöüäi.chunk"] {
            assert!(
                matches!(decode_chunk_coords(bad), Err(AssetError::Chunk(_))),
                "expected Err for `{bad}`"
            );
        }
    }

    #[test]
    fn bigworld_matrix_transforms_local_points() {
        let m = parse_bigworld_matrix("0 0 -1 0 1 0 1 0 0 10 2 -5").unwrap();
        let p = m.transform_point3(Vec3::new(1.0, 0.0, 0.0));
        assert!((p - Vec3::new(10.0, 2.0, -6.0)).length() < 1e-5);
    }

    #[test]
    fn destructible_slots_follow_document_order_of_models_and_speedtrees() {
        // The client engine numbers every `<model>` and `<speedtree>` of an outdoor chunk in
        // document order, destructible or not; other sections take no slot.
        let chunk_xml = br#"<root>
            <model><resource>content/stone.model</resource></model>
            <particles><resource>particles/butterfly.xml</resource></particles>
            <speedtree><spt>speedtree/tree.spt</spt></speedtree>
            <terrain><resource>0000fffdo.cdata/terrain2</resource></terrain>
            <model><resource>content/house.model</resource></model>
            <speedtree><spt>speedtree/bush.spt</spt></speedtree>
        </root>"#;
        let coords = decode_chunk_coords("0000fffdo").unwrap();
        let parsed = parse_chunk_bytes(coords, chunk_xml).unwrap();
        let models: Vec<_> = parsed
            .models
            .iter()
            .map(|m| (m.resource.as_str(), m.destr_index))
            .collect();
        let trees: Vec<_> = parsed
            .speedtrees
            .iter()
            .map(|t| (t.spt.as_str(), t.destr_index))
            .collect();
        assert_eq!(models, [("content/stone.model", 0), ("content/house.model", 2)]);
        assert_eq!(trees, [("speedtree/tree.spt", 1), ("speedtree/bush.spt", 3)]);
    }

    #[test]
    fn parses_synthetic_chunk_and_water_volume() {
        let chunk_xml = br#"<root>
            <model>
                <resource>content/prop.model</resource>
                <transform>1 0 0 0 1 0 0 0 1 5 1 10</transform>
            </model>
            <speedtree>
                <spt>speedtree/tree.spt</spt>
                <seed>7</seed>
                <transform>1 0 0 0 1 0 0 0 1 12 0 15</transform>
            </speedtree>
            <UserDataObject>
                <type>SpawnPoint</type>
                <transform>1 0 0 0 1 0 0 0 1 20 0 30</transform>
                <properties><team>1</team><preferableType>heavyTank</preferableType></properties>
            </UserDataObject>
            <ControlPoint>
                <transform>1 0 0 0 1 0 0 0 1 50 0 50</transform>
                <radius>50.0</radius>
                <team>2</team>
                <pointsPerSecond>1.0</pointsPerSecond>
                <maxPointsPerSecond>3.0</maxPointsPerSecond>
                <stopInvaders>false</stopInvaders>
            </ControlPoint>
            <vlo><uid>abc.123</uid><type>water</type></vlo>
        </root>"#;
        let coords = decode_chunk_coords("0001fffdo").unwrap();
        let parsed = parse_chunk_bytes(coords, chunk_xml).unwrap();
        assert_eq!(parsed.models.len(), 1);
        assert!(
            (parsed.models[0].world_transform.w_axis.truncate() - Vec3::new(105.0, 1.0, -290.0))
                .length()
                < 1e-4
        );
        assert_eq!(parsed.speedtrees.len(), 1);
        assert_eq!(parsed.spawns.len(), 1);
        assert_eq!(parsed.spawns[0].team, 1);
        assert_eq!(parsed.spawns[0].preferable_type.as_deref(), Some("heavyTank"));
        assert_eq!(parsed.control_points.len(), 1);
        assert!((parsed.control_points[0].position - Vec3::new(150.0, 0.0, -250.0)).length() < 1e-4);
        assert_eq!(parsed.vlo_uids, vec!["abc.123"]);

        let vlo_xml = br#"<root><water>
            <position>100 5 -200</position>
            <orientation>0</orientation>
            <size>20 0 10</size>
            <depth>8</depth>
        </water></root>"#;
        let water = WaterVolume::parse("abc.123", vlo_xml).unwrap();
        assert!(water.contains_xz(105.0, -197.0));
        assert!(!water.contains_xz(115.0, -197.0));
    }
}
