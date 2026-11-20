//! Retail WoT 0.6.5 / BigWorld 2.0.1 world asset parsers.

pub mod arena_defs;
pub mod bin_section;
pub mod bsp2;
pub mod chunk;
pub mod destructibles;
pub mod map_store;
pub mod terrain;
pub mod zip_reader;

use std::path::{Path, PathBuf};

pub use arena_defs::{ArenaDef, ArenaDefsCatalog};
pub use bin_section::{parse_vertex_format, BinSectionMap};
pub use bsp2::{BspHit, BspNode, BspTree, BspTriangle};
pub use chunk::{
    decode_chunk_coords, parse_bigworld_matrix, ChunkCoords, ChunkData, ChunkKind, ChunkModel,
    ChunkSpeedTree, ControlPoint, ResolvedModel, SpaceBounds, SpaceSettings, SpawnPoint,
    WaterVolume,
};
pub use destructibles::{
    chunk_id_from_indexes, chunk_id_from_position, chunk_indexes_from_id, DestructibleDesc,
    DestructibleKind, DestructiblePhysicParams, DestructiblesCatalog, StructureModule,
    DESTR_TYPE_FALLING_ATOM, DESTR_TYPE_FRAGILE, DESTR_TYPE_STRUCTURE, DESTR_TYPE_TREE,
};
pub use map_store::{
    ArenaMap, ChunkDestructible, FoliageVolume, HitTester, MapStore, NavGrid, SpatialGrid2D,
    StaticObstacle, TeamBaseSpec, TerrainMap, VehicleCollisionCache, VehicleCollisionSet,
    WorldBspHit, NAV_CELL_SIZE_METERS, SPATIAL_GRID_CELL_METERS,
};
pub use terrain::{
    GroundStrength, MaterialKind, MaterialKinds, TerrainChunk, CHUNK_SIZE_METERS,
    DOMINANT_TEX_GRID_SIZE, HEIGHTMAP_GRID_SIZE, HEIGHTMAP_SPACING_METERS,
    HEIGHTMAP_VISIBLE_OFFSET, HEIGHTMAP_VISIBLE_QUADS,
};
pub use zip_reader::{ZipArchive, ZipEntry};

/// Errors produced when reading or decoding retail WoT 0.6.5 / BigWorld assets.
#[derive(Debug, thiserror::Error)]
pub enum AssetError {
    #[error("i/o error: {0}")]
    Io(#[from] std::io::Error),
    #[error("xml error: {0}")]
    Xml(#[from] wot065_data::xml::XmlError),
    #[error("zip archive error: {0}")]
    Zip(String),
    #[error("bin section error: {0}")]
    BinSection(String),
    #[error("bsp2 error: {0}")]
    Bsp2(String),
    #[error("terrain error: {0}")]
    Terrain(String),
    #[error("png error: {0}")]
    Png(String),
    #[error("chunk error: {0}")]
    Chunk(String),
    #[error("arena_defs error: {0}")]
    ArenaDef(String),
    #[error("destructibles error: {0}")]
    Destructibles(String),
    #[error("asset not found: {0}")]
    NotFound(String),
    #[error("config error: {0}")]
    Config(String),
}

/// Resolve a game installation root or `res/` path to the directory containing
/// `scripts/`, `spaces/`, `system/`, etc.
pub fn resolve_res_dir(path: &Path) -> Option<PathBuf> {
    let candidates = [path.join("res"), path.to_path_buf()];
    candidates
        .into_iter()
        .find(|p| p.join("scripts").is_dir() && p.join("spaces").is_dir())
}

#[cfg(test)]
pub(crate) fn retail_res_dir() -> Option<PathBuf> {
    let dir = wot065_data::testutil::game_dir()?;
    resolve_res_dir(&dir)
}

#[cfg(test)]
mod tests {
    use super::*;
    use glam::Vec3;

    #[test]
    fn bin_section_and_bsp2_parse_hull_and_building() {
        let Some(res_dir) = retail_res_dir() else {
            return;
        };

        let hull_bytes =
            std::fs::read(res_dir.join("vehicles/russian/R11_MS-1/collision/Hull.primitives"))
                .unwrap();
        let hull_secs = BinSectionMap::parse(&hull_bytes).unwrap();
        assert_eq!(parse_vertex_format(hull_secs["vertices"]), Some("xyznuvtb"));
        let hull_bsp =
            BspTree::parse(hull_secs["bsp2"], hull_secs["bsp2_materials"]).unwrap();
        assert_eq!(hull_bsp.triangles().len(), 340);
        assert_eq!(hull_bsp.nodes().len(), 92);
        assert!(hull_bsp.materials().iter().any(|m| m == "armor_1"));
        assert!(hull_bsp.materials().iter().any(|m| m == "fuelTank"));

        // Cast vertical ray through center of Hull (origin above tank pointing down).
        let hit = hull_bsp
            .raycast(Vec3::new(0.0, 10.0, 0.0), Vec3::NEG_Y, 20.0)
            .expect("vertical ray through Hull center should hit");
        let hits = hull_bsp.raycast_all(Vec3::new(0.0, 10.0, 0.0), Vec3::NEG_Y, 20.0);
        assert!(!hits.is_empty());
        assert!((hits[0].t - hit.t).abs() < 1e-5);
        assert!(hits
            .iter()
            .any(|h| hull_bsp.material_name(h).is_some_and(|m| m.starts_with("armor_"))));

        let bld_bytes = std::fs::read(
            res_dir.join("content/Buildings/bld000_base/normal/lod0/bld000_base.primitives"),
        )
        .unwrap();
        let bld_secs = BinSectionMap::parse(&bld_bytes).unwrap();
        let bld_bsp = BspTree::parse(bld_secs["bsp2"], bld_secs["bsp2_materials"]).unwrap();
        assert_eq!(bld_bsp.triangles().len(), 4369);
        assert_eq!(bld_bsp.nodes().len(), 3882);

        assert_bsp_matches_bruteforce(&hull_bsp);
        assert_bsp_matches_bruteforce(&bld_bsp);
    }

    fn assert_bsp_matches_bruteforce(bsp: &BspTree) {
        let (min, max) = bsp.aabb();
        let center = 0.5 * (min + max);
        let span = max - min;
        let max_dist = span.length() + 20.0;
        let origins_and_dirs = [
            (Vec3::new(center.x, max.y + 5.0, center.z), Vec3::NEG_Y),
            (
                Vec3::new(center.x + span.x * 0.2, max.y + 5.0, center.z - span.z * 0.15),
                Vec3::NEG_Y,
            ),
            (Vec3::new(min.x - 5.0, center.y, center.z), Vec3::X),
            (Vec3::new(center.x, center.y, min.z - 5.0), Vec3::Z),
            (
                max + Vec3::splat(3.0),
                (min - (max + Vec3::splat(3.0))).normalize(),
            ),
        ];

        for (origin, dir) in origins_and_dirs {
            let bsp_hits = bsp.raycast_all(origin, dir, max_dist);
            let bsp_first = bsp.raycast(origin, dir, max_dist);
            assert_eq!(
                bsp_first.as_ref().map(|h| h.tri_index),
                bsp_hits.first().map(|h| h.tri_index),
                "raycast vs raycast_all first hit mismatch for {origin:?} -> {dir:?}"
            );

            let mut brute_tris = Vec::new();
            for (idx, tri) in bsp.triangles().iter().enumerate() {
                let e1 = tri.v1 - tri.v0;
                let e2 = tri.v2 - tri.v0;
                let pvec = dir.cross(e2);
                let det = e1.dot(pvec);
                if det.abs() < 1e-8 {
                    continue;
                }
                let inv_det = 1.0 / det;
                let tvec = origin - tri.v0;
                let u = tvec.dot(pvec) * inv_det;
                if !(-1e-6..=1.0 + 1e-6).contains(&u) {
                    continue;
                }
                let qvec = tvec.cross(e1);
                let v = dir.dot(qvec) * inv_det;
                if v < -1e-6 || u + v > 1.0 + 1e-6 {
                    continue;
                }
                let t = e2.dot(qvec) * inv_det;
                if (0.0..=max_dist).contains(&t) {
                    brute_tris.push((t, idx as u32));
                }
            }
            brute_tris.sort_by(|a, b| a.0.total_cmp(&b.0).then_with(|| a.1.cmp(&b.1)));
            let bsp_tris: Vec<u32> = bsp_hits.iter().map(|h| h.tri_index).collect();
            let exp_tris: Vec<u32> = brute_tris.iter().map(|&(_, idx)| idx).collect();
            assert_eq!(
                bsp_tris, exp_tris,
                "BSP traversal missed or added triangles vs brute force"
            );
        }
    }

    #[test]
    fn cdata_terrain_seam_and_dominant_textures() {
        let Some(res_dir) = retail_res_dir() else {
            return;
        };

        let c0_bytes =
            std::fs::read(res_dir.join("spaces/01_karelia/00000001o.cdata")).unwrap();
        let c1_bytes =
            std::fs::read(res_dir.join("spaces/01_karelia/00010001o.cdata")).unwrap();
        let c2_bytes =
            std::fs::read(res_dir.join("spaces/01_karelia/00000002o.cdata")).unwrap();
        let c0 = TerrainChunk::from_cdata_bytes(&c0_bytes).unwrap();
        let c1 = TerrainChunk::from_cdata_bytes(&c1_bytes).unwrap();
        let c2 = TerrainChunk::from_cdata_bytes(&c2_bytes).unwrap();

        assert_eq!(c0.heights.len(), 69 * 69);
        assert_eq!(c1.heights.len(), 69 * 69);
        for row in 0..69 {
            for c in 0..5 {
                let d_x = (c0.height_at_grid(64 + c, row) - c1.height_at_grid(c, row)).abs();
                assert!(d_x < 1e-3, "X seam mismatch at row={row} col={c}: {d_x}");
                let d_z = (c0.height_at_grid(row, 64 + c) - c2.height_at_grid(row, c)).abs();
                assert!(d_z < 1e-3, "Z seam mismatch at col={row} row={c}: {d_z}");
            }
        }

        let (h, normal) = c0.sample_local(50.0, 50.0);
        assert!(h >= c0.min_y - 1e-3 && h <= c0.max_y + 1e-3);
        assert!(normal.y > 0.0 && (normal.length() - 1.0).abs() < 1e-4);

        let mat_kinds = MaterialKinds::load(&res_dir).unwrap();
        assert_eq!(c0.dom_tex_indices.len(), 128 * 128);
        let center_kind = c0
            .dominant_material_local(50.0, 50.0, &mat_kinds)
            .expect("01_karelia dominant texture must resolve against material_kinds.xml");
        assert!(center_kind.id >= 100);

        // Verify .tga textures in 04_himmelsdorf and suffixed ZIP entry in 15_komarin resolve too.
        let himmel = TerrainChunk::from_cdata_bytes(
            &std::fs::read(res_dir.join("spaces/04_himmelsdorf/00000000o.cdata")).unwrap(),
        )
        .unwrap();
        for tex in &himmel.tex_names {
            assert!(
                mat_kinds.for_terrain_texture(tex).is_some(),
                "unmapped texture in 04_himmelsdorf: {tex}"
            );
        }
        let komarin = TerrainChunk::from_cdata_bytes(
            &std::fs::read(res_dir.join("spaces/15_komarin/00000000o.cdata")).unwrap(),
        )
        .unwrap();
        assert_eq!(komarin.dom_tex_indices.len(), 128 * 128);
        assert!(komarin.dominant_material_local(50.0, 50.0, &mat_kinds).is_some());
    }

    #[test]
    fn chunk_hex_coords_spawns_and_control_points() {
        let coords = decode_chunk_coords("0000fffdo").unwrap();
        assert_eq!(coords.grid_x, 0);
        assert_eq!(coords.grid_z, -3);
        assert_eq!(coords.world_origin_xz(), (0.0, -300.0));

        let Some(res_dir) = retail_res_dir() else {
            return;
        };

        let settings =
            SpaceSettings::load(&res_dir.join("spaces/04_himmelsdorf/space.settings")).unwrap();
        assert_eq!(
            settings.bounds,
            SpaceBounds {
                min_x: -5,
                max_x: 4,
                min_y: -5,
                max_y: 4,
            }
        );

        let chunks = chunk::load_space_chunks(&res_dir.join("spaces/04_himmelsdorf")).unwrap();
        let team1_spawns: Vec<_> = chunks
            .iter()
            .flat_map(|c| &c.spawns)
            .filter(|s| s.team == 1)
            .collect();
        let team2_spawns: Vec<_> = chunks
            .iter()
            .flat_map(|c| &c.spawns)
            .filter(|s| s.team == 2)
            .collect();
        assert_eq!(team1_spawns.len(), 30);
        assert_eq!(team2_spawns.len(), 30);

        let control_points: Vec<_> = chunks.iter().flat_map(|c| &c.control_points).collect();
        assert_eq!(control_points.len(), 2);
        for cp in &control_points {
            assert!((cp.radius - 50.0).abs() < 1e-4);
            assert!((cp.points_per_second - 1.0).abs() < 1e-4);
            assert!((cp.max_points_per_second - 3.0).abs() < 1e-4);
        }

        // Verify model -> .visual -> .primitives resolution on first chunk model.
        let first_model = chunks
            .iter()
            .flat_map(|c| &c.models)
            .next()
            .expect("04_himmelsdorf has models");
        let resolved = chunk::resolve_model_visual(&res_dir, &first_model.resource).unwrap();
        assert!(resolved.primitives_path.is_file());

        // Verify VLO water volumes in 01_karelia.
        let karelia_chunks = chunk::load_space_chunks(&res_dir.join("spaces/01_karelia")).unwrap();
        let uid = karelia_chunks
            .iter()
            .flat_map(|c| &c.vlo_uids)
            .next()
            .expect("01_karelia has water vlo");
        let vlo = WaterVolume::load(&res_dir.join("spaces/01_karelia"), uid).unwrap();
        assert!(vlo.size.x > 0.0 && vlo.size.z > 0.0);

        // Verify all 20 maps in arena_defs/_list_.xml.
        let arenas = ArenaDefsCatalog::load(&res_dir).unwrap();
        assert_eq!(arenas.maps.len(), 20);
        let himmel = arenas.by_name("04_himmelsdorf").unwrap();
        assert_eq!(himmel.id, 3);
        assert_eq!(himmel.round_length_secs, 900);
        assert_eq!(himmel.bounding_box, ([-300.0, -300.0], [400.0, 400.0]));

        // Verify scripts/destructibles.xml.
        let destr = DestructiblesCatalog::load(&res_dir).unwrap();
        assert!((destr.unit_vehicle_mass - 28_000.0).abs() < 1e-3);
        assert!(!destr.entries.is_empty());
        assert!(destr
            .entries
            .iter()
            .any(|e| matches!(e.kind, DestructibleKind::Tree { .. })));
        assert!(destr
            .entries
            .iter()
            .any(|e| matches!(e.kind, DestructibleKind::Structure { .. })));
    }
}
