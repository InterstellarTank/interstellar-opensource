//! Lazy [`MapStore`] world cache, terrain map, spatial index, 2D [`NavGrid`]
//! pathfinder, and [`VehicleCollisionCache`] for WoT 0.6.5 tanks.

use std::cmp::Ordering;
use std::collections::{BTreeMap, BTreeSet, BinaryHeap, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};

use glam::{Mat4, Vec2, Vec3};
use wot065_data::item_defs::{HitTesterSpec, VehicleDef};

use super::arena_defs::{ArenaDef, ArenaDefsCatalog};
use super::bin_section::BinSectionMap;
use super::bsp2::{BspHit, BspTree};
use super::chunk::{
    decode_chunk_coords, load_space_chunks, resolve_model_visual, ChunkCoords, ChunkKind,
    SpaceBounds, SpaceSettings, SpawnPoint, WaterVolume,
};
use super::destructibles::{chunk_id_from_indexes, DestructibleKind, DestructiblesCatalog};
use super::terrain::{MaterialKind, MaterialKinds, TerrainChunk, CHUNK_SIZE_METERS};
use super::AssetError;
use crate::balance::BalanceConfig;
use crate::sim::collision::{band_outline, BandPiece, BAND_TILE};
use crate::sim::destructibles::DEFAULT_UNIT_VEHICLE_MASS;

/// Uniform 2D spatial grid cell size in meters.
pub const SPATIAL_GRID_CELL_METERS: f32 = 25.0;
/// `NavGrid` cell width/height in meters.
pub const NAV_CELL_SIZE_METERS: f32 = 10.0;
/// Maximum terrain grid dimension along one axis (guards against malformed bounds).
const MAX_TERRAIN_CHUNKS_PER_AXIS: usize = 256;
/// Search radius in `NavGrid` cells when snapping blocked start/goal endpoints.
const NAV_ENDPOINT_SNAP_RADIUS_CELLS: usize = 16;

/// Raycast hit transformed into world space.
#[derive(Debug, Clone, PartialEq)]
pub struct WorldBspHit {
    /// Distance from ray origin in world meters (`0 <= t <= max_dist`).
    pub t: f32,
    /// Hit position in world coordinates (meters).
    pub position: Vec3,
    /// Unit outward surface normal in world coordinates.
    pub normal: Vec3,
    /// Material index on the struck BSP2 triangle.
    pub material_index: u32,
    /// Material identifier string from `bsp2_materials` if valid.
    pub material_name: Option<String>,
    /// Triangle index in the underlying [`BspTree`].
    pub tri_index: u32,
}

/// Dense grid of [`TerrainChunk`] heightmaps/material grids covering a space.
#[derive(Debug, Clone)]
pub struct TerrainMap {
    min_grid_x: i32,
    min_grid_z: i32,
    width_chunks: usize,
    height_chunks: usize,
    chunks: Vec<Option<TerrainChunk>>,
}

impl TerrainMap {
    /// Assemble a dense terrain map from space bounds and decoded outer terrain chunks.
    pub fn from_chunks(
        bounds: SpaceBounds,
        loaded: impl IntoIterator<Item = (ChunkCoords, TerrainChunk)>,
    ) -> Result<Self, AssetError> {
        let width_chunks = usize::try_from(bounds.max_x - bounds.min_x + 1).map_err(|_| {
            AssetError::Terrain(format!(
                "invalid space X bounds {}..={}",
                bounds.min_x, bounds.max_x
            ))
        })?;
        let height_chunks = usize::try_from(bounds.max_y - bounds.min_y + 1).map_err(|_| {
            AssetError::Terrain(format!(
                "invalid space Y bounds {}..={}",
                bounds.min_y, bounds.max_y
            ))
        })?;
        if width_chunks == 0
            || height_chunks == 0
            || width_chunks > MAX_TERRAIN_CHUNKS_PER_AXIS
            || height_chunks > MAX_TERRAIN_CHUNKS_PER_AXIS
        {
            return Err(AssetError::Terrain(format!(
                "unreasonable space chunk grid {width_chunks}x{height_chunks}"
            )));
        }
        let mut chunks = vec![None; width_chunks * height_chunks];
        for (coords, chunk) in loaded {
            if coords.kind != ChunkKind::Outer {
                continue;
            }
            if coords.grid_x < bounds.min_x
                || coords.grid_x > bounds.max_x
                || coords.grid_z < bounds.min_y
                || coords.grid_z > bounds.max_y
            {
                continue;
            }
            let col = (coords.grid_x - bounds.min_x) as usize;
            let row = (coords.grid_z - bounds.min_y) as usize;
            chunks[row * width_chunks + col] = Some(chunk);
        }
        Ok(Self {
            min_grid_x: bounds.min_x,
            min_grid_z: bounds.min_y,
            width_chunks,
            height_chunks,
            chunks,
        })
    }

    /// World-space horizontal terrain coverage `([min_x, min_z], [max_x, max_z])` in meters.
    pub fn bounds_meters(&self) -> ([f32; 2], [f32; 2]) {
        let min_x = self.min_grid_x as f32 * CHUNK_SIZE_METERS;
        let min_z = self.min_grid_z as f32 * CHUNK_SIZE_METERS;
        let max_x = (self.min_grid_x + self.width_chunks as i32) as f32 * CHUNK_SIZE_METERS;
        let max_z = (self.min_grid_z + self.height_chunks as i32) as f32 * CHUNK_SIZE_METERS;
        ([min_x, min_z], [max_x, max_z])
    }

    /// Number of loaded outer terrain chunks.
    pub fn loaded_chunk_count(&self) -> usize {
        self.chunks.iter().filter(|c| c.is_some()).count()
    }

    fn locate(&self, x: f32, z: f32) -> Option<(&TerrainChunk, f32, f32)> {
        if !x.is_finite() || !z.is_finite() || self.width_chunks == 0 || self.height_chunks == 0 {
            return None;
        }
        let ([min_x, min_z], [max_x, max_z]) = self.bounds_meters();
        if x < min_x || x > max_x || z < min_z || z > max_z {
            return None;
        }
        let max_gx = self.min_grid_x + self.width_chunks as i32 - 1;
        let max_gz = self.min_grid_z + self.height_chunks as i32 - 1;
        let gx = ((x / CHUNK_SIZE_METERS).floor() as i32).clamp(self.min_grid_x, max_gx);
        let gz = ((z / CHUNK_SIZE_METERS).floor() as i32).clamp(self.min_grid_z, max_gz);
        let col = (gx - self.min_grid_x) as usize;
        let row = (gz - self.min_grid_z) as usize;
        let chunk = self.chunks.get(row * self.width_chunks + col)?.as_ref()?;
        let lx = (x - gx as f32 * CHUNK_SIZE_METERS).clamp(0.0, CHUNK_SIZE_METERS);
        let lz = (z - gz as f32 * CHUNK_SIZE_METERS).clamp(0.0, CHUNK_SIZE_METERS);
        Some((chunk, lx, lz))
    }

    /// Sample terrain height (meters) and unit surface normal at world `(x, z)`.
    pub fn sample_height_and_normal(&self, x: f32, z: f32) -> Option<(f32, Vec3)> {
        let (chunk, lx, lz) = self.locate(x, z)?;
        Some(chunk.sample_local(lx, lz))
    }

    /// Sample terrain elevation (meters) at world `(x, z)`.
    pub fn sample_height(&self, x: f32, z: f32) -> Option<f32> {
        self.sample_height_and_normal(x, z).map(|(h, _)| h)
    }

    /// Sample terrain elevation at world `(x, z)` clamped to map bounds.
    ///
    /// Returns `None` only on non-finite coordinates or an empty terrain grid.
    pub fn sample_height_clamped(&self, x: f32, z: f32) -> Option<f32> {
        if !x.is_finite() || !z.is_finite() || self.width_chunks == 0 || self.height_chunks == 0 {
            return None;
        }
        let ([min_x, min_z], [max_x, max_z]) = self.bounds_meters();
        self.sample_height(x.clamp(min_x, max_x), z.clamp(min_z, max_z))
    }

    /// Look up the dominant terrain texture path at world `(x, z)`.
    pub fn dominant_texture_at(&self, x: f32, z: f32) -> Option<&str> {
        let (chunk, lx, lz) = self.locate(x, z)?;
        chunk.dominant_texture_local(lx, lz)
    }

    /// Look up the dominant [`MaterialKind`] (ground strength, effect material) at world `(x, z)`.
    pub fn dominant_material_at<'a>(
        &self,
        x: f32,
        z: f32,
        kinds: &'a MaterialKinds,
    ) -> Option<&'a MaterialKind> {
        let (chunk, lx, lz) = self.locate(x, z)?;
        chunk.dominant_material_local(lx, lz, kinds)
    }
}

/// Placed static solid model (`<model>`) with shared BSP collision geometry.
#[derive(Debug, Clone)]
pub struct StaticObstacle {
    pub model_path: String,
    pub primitives_path: PathBuf,
    pub world_transform: Mat4,
    pub inv_world_transform: Mat4,
    pub world_aabb_min: Vec3,
    pub world_aabb_max: Vec3,
    pub bsp: Arc<BspTree>,
    /// Where the collision BSP crosses the hull height band; filled when the map is built.
    pub pieces: Vec<BandPiece>,
}

impl StaticObstacle {
    /// Construct from a world transform and shared [`BspTree`] (`None` for singular transforms).
    pub fn from_bsp(
        model_path: String,
        primitives_path: PathBuf,
        world_transform: Mat4,
        bsp: Arc<BspTree>,
    ) -> Option<Self> {
        if world_transform.determinant().abs() < 1e-12 {
            return None;
        }
        let (local_min, local_max) = bsp.aabb();
        let (world_aabb_min, world_aabb_max) =
            transform_aabb(world_transform, local_min, local_max);
        Some(Self {
            model_path,
            primitives_path,
            world_transform,
            inv_world_transform: world_transform.inverse(),
            world_aabb_min,
            world_aabb_max,
            bsp,
            pieces: Vec::new(),
        })
    }

    /// Raycast in world coordinates against this obstacle's world AABB and transformed BSP2.
    pub fn raycast(&self, origin: Vec3, dir: Vec3, max_dist: f32) -> Option<WorldBspHit> {
        if !is_valid_ray(origin, dir, max_dist)
            || !ray_aabb_intersects(
                origin,
                dir,
                max_dist,
                self.world_aabb_min,
                self.world_aabb_max,
            )
        {
            return None;
        }
        let local_origin = self.inv_world_transform.transform_point3(origin);
        let local_dir_scaled = self.inv_world_transform.transform_vector3(dir);
        let scale = local_dir_scaled.length();
        if scale < 1e-8 || !scale.is_finite() {
            return None;
        }
        let local_dir = local_dir_scaled / scale;
        let hit = self.bsp.raycast(local_origin, local_dir, max_dist * scale)?;
        let world_t = hit.t / scale;
        let world_pos = origin + dir * world_t;
        let world_normal = self
            .inv_world_transform
            .transpose()
            .transform_vector3(hit.normal)
            .normalize_or_zero();
        Some(WorldBspHit {
            t: world_t,
            position: world_pos,
            normal: world_normal,
            material_index: hit.material_index,
            material_name: self.bsp.material_name(&hit).map(str::to_string),
            tri_index: hit.tri_index,
        })
    }
}

/// Foliage camouflage volume derived from a chunk `<speedtree>` placement.
#[derive(Debug, Clone, PartialEq)]
pub struct FoliageVolume {
    pub spt_path: String,
    /// Base world position in meters.
    pub position: Vec3,
    /// Effective foliage cylinder radius in meters.
    pub radius: f32,
    /// Effective foliage height in meters.
    pub height: f32,
    /// Camouflage density from `destructibles.xml` (`1.0` fallback).
    pub density: f32,
    pub world_aabb_min: Vec3,
    pub world_aabb_max: Vec3,
    /// Corresponding `(chunk_id, index_in_chunk)` if registered as a destructible tree.
    pub destructible_ref: Option<(u16, usize)>,
}

/// Placed destructible entry indexed by `(chunk_id, index_in_chunk)`.
#[derive(Debug, Clone)]
pub struct ChunkDestructible {
    pub chunk_id: u16,
    pub index_in_chunk: usize,
    pub desc_id: usize,
    pub filename: String,
    pub kind: DestructibleKind,
    pub world_transform: Mat4,
    pub position: Vec3,
    pub world_aabb_min: Vec3,
    pub world_aabb_max: Vec3,
    pub bsp: Option<Arc<BspTree>>,
    /// Where the collision BSP crosses the hull height band; filled when the map is built.
    pub pieces: Vec<BandPiece>,
}

/// Team base capture zone spec extracted from chunk `<ControlPoint>`.
#[derive(Debug, Clone, PartialEq)]
pub struct TeamBaseSpec {
    pub team: u8,
    pub position: Vec3,
    pub radius: f32,
    pub points_per_second: f32,
    pub max_points_per_second: f32,
    pub stop_invaders: bool,
}

/// Uniform 2D `25m x 25m` spatial grid indexing static obstacles and foliage volumes.
#[derive(Debug, Clone)]
pub struct SpatialGrid2D {
    cell_size: f32,
    origin_x: f32,
    origin_z: f32,
    cols: usize,
    rows: usize,
    obstacle_cells: Vec<Vec<usize>>,
    foliage_cells: Vec<Vec<usize>>,
    destructible_cells: Vec<Vec<usize>>,
}

impl SpatialGrid2D {
    /// Build a 2D spatial hash grid covering `[bounds_min_xz, bounds_max_xz]`.
    pub fn build(
        bounds_min_xz: [f32; 2],
        bounds_max_xz: [f32; 2],
        obstacles: &[StaticObstacle],
        foliage: &[FoliageVolume],
    ) -> Self {
        let cell_size = SPATIAL_GRID_CELL_METERS;
        let span_x = (bounds_max_xz[0] - bounds_min_xz[0]).max(cell_size);
        let span_z = (bounds_max_xz[1] - bounds_min_xz[1]).max(cell_size);
        let cols = (span_x / cell_size).ceil().max(1.0) as usize;
        let rows = (span_z / cell_size).ceil().max(1.0) as usize;
        let mut grid = Self {
            cell_size,
            origin_x: bounds_min_xz[0],
            origin_z: bounds_min_xz[1],
            cols,
            rows,
            obstacle_cells: vec![Vec::new(); cols * rows],
            foliage_cells: vec![Vec::new(); cols * rows],
            destructible_cells: vec![Vec::new(); cols * rows],
        };
        for (idx, obs) in obstacles.iter().enumerate() {
            grid.insert_aabb(
                idx,
                [obs.world_aabb_min.x, obs.world_aabb_min.z],
                [obs.world_aabb_max.x, obs.world_aabb_max.z],
                true,
            );
        }
        for (idx, fol) in foliage.iter().enumerate() {
            grid.insert_aabb(
                idx,
                [fol.world_aabb_min.x, fol.world_aabb_min.z],
                [fol.world_aabb_max.x, fol.world_aabb_max.z],
                false,
            );
        }
        grid
    }

    fn cell_range(&self, min_xz: [f32; 2], max_xz: [f32; 2]) -> Option<(usize, usize, usize, usize)> {
        if self.cols == 0 || self.rows == 0 {
            return None;
        }
        let max_x = self.origin_x + self.cols as f32 * self.cell_size;
        let max_z = self.origin_z + self.rows as f32 * self.cell_size;
        let lo_x = min_xz[0].min(max_xz[0]);
        let hi_x = min_xz[0].max(max_xz[0]);
        let lo_z = min_xz[1].min(max_xz[1]);
        let hi_z = min_xz[1].max(max_xz[1]);
        if !lo_x.is_finite() || !hi_x.is_finite() || !lo_z.is_finite() || !hi_z.is_finite() {
            return None;
        }
        if hi_x < self.origin_x || lo_x > max_x || hi_z < self.origin_z || lo_z > max_z {
            return None;
        }
        let c0 = (((lo_x - self.origin_x) / self.cell_size).floor() as isize)
            .clamp(0, self.cols as isize - 1) as usize;
        let c1 = (((hi_x - self.origin_x) / self.cell_size).floor() as isize)
            .clamp(0, self.cols as isize - 1) as usize;
        let r0 = (((lo_z - self.origin_z) / self.cell_size).floor() as isize)
            .clamp(0, self.rows as isize - 1) as usize;
        let r1 = (((hi_z - self.origin_z) / self.cell_size).floor() as isize)
            .clamp(0, self.rows as isize - 1) as usize;
        Some((c0, c1, r0, r1))
    }

    /// Index the collision geometry of `destructibles`; returns `(chunk_id, position)` per index.
    pub fn index_destructibles(
        &mut self,
        destructibles: &BTreeMap<u16, Vec<ChunkDestructible>>,
    ) -> Vec<(u16, usize)> {
        let mut refs = Vec::new();
        for (&chunk_id, entries) in destructibles {
            for (position, d) in entries.iter().enumerate() {
                let Some((min, max)) = pieces_bounds(&d.pieces) else {
                    continue;
                };
                if let Some((c0, c1, r0, r1)) = self.cell_range(min.to_array(), max.to_array()) {
                    for r in r0..=r1 {
                        for c in c0..=c1 {
                            self.destructible_cells[r * self.cols + c].push(refs.len());
                        }
                    }
                }
                refs.push((chunk_id, position));
            }
        }
        refs
    }

    /// Return deduplicated destructible indices overlapping the horizontal AABB.
    pub fn query_destructibles_aabb(&self, min_xz: [f32; 2], max_xz: [f32; 2]) -> Vec<usize> {
        self.query_cells_aabb(&self.destructible_cells, min_xz, max_xz)
    }

    fn insert_aabb(&mut self, index: usize, min_xz: [f32; 2], max_xz: [f32; 2], is_obstacle: bool) {
        let Some((c0, c1, r0, r1)) = self.cell_range(min_xz, max_xz) else {
            return;
        };
        let cells = if is_obstacle {
            &mut self.obstacle_cells
        } else {
            &mut self.foliage_cells
        };
        for r in r0..=r1 {
            for c in c0..=c1 {
                cells[r * self.cols + c].push(index);
            }
        }
    }

    fn query_cells_aabb(
        &self,
        cells: &[Vec<usize>],
        min_xz: [f32; 2],
        max_xz: [f32; 2],
    ) -> Vec<usize> {
        let Some((c0, c1, r0, r1)) = self.cell_range(min_xz, max_xz) else {
            return Vec::new();
        };
        let mut seen = HashSet::new();
        let mut out = Vec::new();
        for r in r0..=r1 {
            for c in c0..=c1 {
                for &idx in &cells[r * self.cols + c] {
                    if seen.insert(idx) {
                        out.push(idx);
                    }
                }
            }
        }
        out
    }

    fn collect_walk_cells(
        &self,
        cells: &[Vec<usize>],
        start: [f32; 2],
        end: [f32; 2],
    ) -> Vec<usize> {
        let mut seen = HashSet::new();
        let mut out = Vec::new();
        walk_grid_segment_2d(
            self.origin_x,
            self.origin_z,
            self.cell_size,
            self.cols,
            self.rows,
            start,
            end,
            false,
            |c, r| {
                let cell_idx = r * self.cols + c;
                for &idx in &cells[cell_idx] {
                    if seen.insert(idx) {
                        out.push(idx);
                    }
                }
                true
            },
        );
        out
    }

    fn query_cells_segment(&self, cells: &[Vec<usize>], p0: Vec3, p1: Vec3) -> Vec<usize> {
        if self.cols == 0
            || self.rows == 0
            || !p0.x.is_finite()
            || !p0.z.is_finite()
            || !p1.x.is_finite()
            || !p1.z.is_finite()
        {
            return Vec::new();
        }
        let max_x = self.origin_x + self.cols as f32 * self.cell_size;
        let max_z = self.origin_z + self.rows as f32 * self.cell_size;
        let Some((t0, t1)) = clip_segment_2d(
            [p0.x, p0.z],
            [p1.x, p1.z],
            [self.origin_x, self.origin_z],
            [max_x, max_z],
        ) else {
            return Vec::new();
        };
        let dx = p1.x - p0.x;
        let dz = p1.z - p0.z;
        let start = [p0.x + dx * t0, p0.z + dz * t0];
        let end = [p0.x + dx * t1, p0.z + dz * t1];
        self.collect_walk_cells(cells, start, end)
    }

    fn query_cells_ray(
        &self,
        cells: &[Vec<usize>],
        origin: Vec3,
        dir: Vec3,
        max_dist: f32,
    ) -> Vec<usize> {
        if self.cols == 0 || self.rows == 0 || !is_valid_ray(origin, dir, max_dist) {
            return Vec::new();
        }
        let max_x = self.origin_x + self.cols as f32 * self.cell_size;
        let max_z = self.origin_z + self.rows as f32 * self.cell_size;
        let Some((t0, t1)) = clip_ray_2d(
            [origin.x, origin.z],
            [dir.x, dir.z],
            max_dist,
            [self.origin_x, self.origin_z],
            [max_x, max_z],
        ) else {
            return Vec::new();
        };
        if !t0.is_finite() {
            return Vec::new();
        }
        let start = if t0 == 0.0 {
            [origin.x, origin.z]
        } else {
            [origin.x + dir.x * t0, origin.z + dir.z * t0]
        };
        let end = if t1.is_finite() {
            [origin.x + dir.x * t1, origin.z + dir.z * t1]
        } else {
            // Vertical ray (dir_xz == 0) with unbounded max_dist stays in origin's XZ cell.
            start
        };
        if !start[0].is_finite()
            || !start[1].is_finite()
            || !end[0].is_finite()
            || !end[1].is_finite()
        {
            return Vec::new();
        }
        self.collect_walk_cells(cells, start, end)
    }

    /// Return deduplicated obstacle indices whose bounding boxes intersect overlapping grid cells.
    pub fn query_obstacles_aabb(&self, min_xz: [f32; 2], max_xz: [f32; 2]) -> Vec<usize> {
        self.query_cells_aabb(&self.obstacle_cells, min_xz, max_xz)
    }

    /// Return deduplicated foliage volume indices overlapping the horizontal AABB.
    pub fn query_foliage_aabb(&self, min_xz: [f32; 2], max_xz: [f32; 2]) -> Vec<usize> {
        self.query_cells_aabb(&self.foliage_cells, min_xz, max_xz)
    }

    /// Traverse grid cells along horizontal segment `p0 -> p1` and return deduplicated obstacles.
    pub fn query_obstacles_segment(&self, p0: Vec3, p1: Vec3) -> Vec<usize> {
        self.query_cells_segment(&self.obstacle_cells, p0, p1)
    }

    /// Traverse grid cells along horizontal ray `(origin, dir, max_dist)` and return deduplicated obstacles.
    pub fn query_obstacles_ray(&self, origin: Vec3, dir: Vec3, max_dist: f32) -> Vec<usize> {
        self.query_cells_ray(&self.obstacle_cells, origin, dir, max_dist)
    }

    /// Traverse grid cells along horizontal segment `p0 -> p1` and return deduplicated foliage.
    pub fn query_foliage_segment(&self, p0: Vec3, p1: Vec3) -> Vec<usize> {
        self.query_cells_segment(&self.foliage_cells, p0, p1)
    }
}

/// Walks a clipped 2D segment `[start, end]` across a uniform grid using monotonic
/// Amanatides–Woo DDA.
///
/// Bounding `(c, r)` monotonically to `(end_c, end_r)` prevents `f32` boundary drift
/// from overshooting an endpoint cell. When `supercover_corners` is `true`, simultaneous
/// or near-simultaneous corner crossings also visit `(c + step_c, r)` and `(c, r + step_r)`
/// so diagonal trajectories cannot graze corner-adjacent obstacles.
#[allow(clippy::too_many_arguments)]
fn walk_grid_segment_2d(
    origin_x: f32,
    origin_z: f32,
    cell_size: f32,
    cols: usize,
    rows: usize,
    start: [f32; 2],
    end: [f32; 2],
    supercover_corners: bool,
    mut visit: impl FnMut(usize, usize) -> bool,
) -> bool {
    if cols == 0 || rows == 0 {
        return true;
    }
    let mut c = (((start[0] - origin_x) / cell_size).floor() as isize).clamp(0, cols as isize - 1);
    let mut r = (((start[1] - origin_z) / cell_size).floor() as isize).clamp(0, rows as isize - 1);
    let end_c = (((end[0] - origin_x) / cell_size).floor() as isize).clamp(0, cols as isize - 1);
    let end_r = (((end[1] - origin_z) / cell_size).floor() as isize).clamp(0, rows as isize - 1);

    let sdx = end[0] - start[0];
    let sdz = end[1] - start[1];
    let step_c: isize = if c < end_c {
        1
    } else if c > end_c {
        -1
    } else {
        0
    };
    let step_r: isize = if r < end_r {
        1
    } else if r > end_r {
        -1
    } else {
        0
    };
    let t_delta_x = if step_c != 0 {
        (cell_size / sdx).abs()
    } else {
        f32::INFINITY
    };
    let t_delta_z = if step_r != 0 {
        (cell_size / sdz).abs()
    } else {
        f32::INFINITY
    };
    let mut t_max_x = if step_c > 0 {
        let next_edge = origin_x + (c + 1) as f32 * cell_size;
        (next_edge - start[0]) / sdx
    } else if step_c < 0 {
        let next_edge = origin_x + c as f32 * cell_size;
        (next_edge - start[0]) / sdx
    } else {
        f32::INFINITY
    };
    let mut t_max_z = if step_r > 0 {
        let next_edge = origin_z + (r + 1) as f32 * cell_size;
        (next_edge - start[1]) / sdz
    } else if step_r < 0 {
        let next_edge = origin_z + r as f32 * cell_size;
        (next_edge - start[1]) / sdz
    } else {
        f32::INFINITY
    };

    let max_steps = cols + rows + 4;
    for _ in 0..max_steps {
        if !visit(c as usize, r as usize) {
            return false;
        }
        let can_x = step_c != 0 && c != end_c;
        let can_z = step_r != 0 && r != end_r;
        if !can_x && !can_z {
            return true;
        }
        if supercover_corners && can_x && can_z && (t_max_x - t_max_z).abs() <= 1e-5 {
            if !visit((c + step_c) as usize, r as usize)
                || !visit(c as usize, (r + step_r) as usize)
            {
                return false;
            }
            c += step_c;
            t_max_x += t_delta_x;
            r += step_r;
            t_max_z += t_delta_z;
        } else if can_x && (!can_z || t_max_x < t_max_z) {
            c += step_c;
            t_max_x += t_delta_x;
        } else {
            r += step_r;
            t_max_z += t_delta_z;
        }
    }
    true
}

/// 2D `10m x 10m` navigation grid with 8-connected A* and line-of-sight string-pulling.
#[derive(Debug, Clone)]
pub struct NavGrid {
    origin_x: f32,
    origin_z: f32,
    cols: usize,
    rows: usize,
    cell_size: f32,
    walkable: Vec<bool>,
}

#[derive(Debug, Clone, Copy)]
struct AStarNode {
    f_cost: f32,
    g_cost: f32,
    idx: usize,
}

impl PartialEq for AStarNode {
    fn eq(&self, other: &Self) -> bool {
        self.idx == other.idx && self.f_cost.to_bits() == other.f_cost.to_bits()
    }
}

impl Eq for AStarNode {}

impl Ord for AStarNode {
    fn cmp(&self, other: &Self) -> Ordering {
        other
            .f_cost
            .total_cmp(&self.f_cost)
            .then_with(|| other.g_cost.total_cmp(&self.g_cost))
            .then_with(|| self.idx.cmp(&other.idx))
    }
}

impl PartialOrd for AStarNode {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl NavGrid {
    /// Rasterize terrain slopes, deep water, and solid obstacle triangles onto a `10m` grid.
    pub fn build(
        bounds_min_xz: [f32; 2],
        bounds_max_xz: [f32; 2],
        terrain: &TerrainMap,
        obstacles: &[StaticObstacle],
        water_volumes: &[WaterVolume],
        max_slope_cos: f32,
        max_water_depth_m: f32,
    ) -> Self {
        let cell_size = NAV_CELL_SIZE_METERS;
        let span_x = (bounds_max_xz[0] - bounds_min_xz[0]).max(cell_size);
        let span_z = (bounds_max_xz[1] - bounds_min_xz[1]).max(cell_size);
        let cols = (span_x / cell_size).ceil().max(1.0) as usize;
        let rows = (span_z / cell_size).ceil().max(1.0) as usize;
        let mut walkable = vec![true; cols * rows];

        // 1. Mark steep slopes, out-of-terrain voids, and deep water unwalkable.
        for r in 0..rows {
            let cz = bounds_min_xz[1] + (r as f32 + 0.5) * cell_size;
            for c in 0..cols {
                let cx = bounds_min_xz[0] + (c as f32 + 0.5) * cell_size;
                let Some((terrain_h, normal)) = terrain.sample_height_and_normal(cx, cz) else {
                    walkable[r * cols + c] = false;
                    continue;
                };
                if normal.y < max_slope_cos {
                    walkable[r * cols + c] = false;
                    continue;
                }
                if water_depth_at_volumes(water_volumes, cx, cz, terrain_h)
                    .is_some_and(|d| d > max_water_depth_m)
                {
                    walkable[r * cols + c] = false;
                }
            }
        }

        // 2. Rasterize blocking wall/solid triangles from static obstacles (ignoring floor slabs
        //    near ground and high overhead archways/bridges above 2.6m tank clearance).
        let grid_max_x = bounds_min_xz[0] + cols as f32 * cell_size;
        let grid_max_z = bounds_min_xz[1] + rows as f32 * cell_size;
        for obs in obstacles {
            if obs.world_aabb_max.x < bounds_min_xz[0]
                || obs.world_aabb_min.x > grid_max_x
                || obs.world_aabb_max.z < bounds_min_xz[1]
                || obs.world_aabb_min.z > grid_max_z
            {
                continue;
            }
            for tri in obs.bsp.triangles() {
                let v0 = obs.world_transform.transform_point3(tri.v0);
                let v1 = obs.world_transform.transform_point3(tri.v1);
                let v2 = obs.world_transform.transform_point3(tri.v2);
                let tri_min = v0.min(v1).min(v2);
                let tri_max = v0.max(v1).max(v2);
                if tri_max.x < bounds_min_xz[0]
                    || tri_min.x > grid_max_x
                    || tri_max.z < bounds_min_xz[1]
                    || tri_min.z > grid_max_z
                {
                    continue;
                }
                let tri_normal = (v1 - v0).cross(v2 - v0).normalize_or_zero();
                let centroid = (v0 + v1 + v2) / 3.0;
                let Some(th) = terrain.sample_height_clamped(centroid.x, centroid.z) else {
                    continue;
                };
                if tri_min.y > th + 2.6 || tri_max.y < th + 0.8 {
                    continue;
                }
                if tri_max.y <= th + 1.1 && tri_normal.y.abs() >= max_slope_cos {
                    continue;
                }
                let c0 = (((tri_min.x - bounds_min_xz[0]) / cell_size).floor() as isize)
                    .clamp(0, cols as isize - 1) as usize;
                let c1 = (((tri_max.x - bounds_min_xz[0]) / cell_size).floor() as isize)
                    .clamp(0, cols as isize - 1) as usize;
                let r0 = (((tri_min.z - bounds_min_xz[1]) / cell_size).floor() as isize)
                    .clamp(0, rows as isize - 1) as usize;
                let r1 = (((tri_max.z - bounds_min_xz[1]) / cell_size).floor() as isize)
                    .clamp(0, rows as isize - 1) as usize;
                for r in r0..=r1 {
                    let cell_z0 = bounds_min_xz[1] + r as f32 * cell_size;
                    let cell_z1 = cell_z0 + cell_size;
                    for c in c0..=c1 {
                        if !walkable[r * cols + c] {
                            continue;
                        }
                        let cell_x0 = bounds_min_xz[0] + c as f32 * cell_size;
                        let cell_x1 = cell_x0 + cell_size;
                        if tri_intersects_rect_xz(
                            [v0.x, v0.z],
                            [v1.x, v1.z],
                            [v2.x, v2.z],
                            [cell_x0, cell_z0],
                            [cell_x1, cell_z1],
                        ) {
                            walkable[r * cols + c] = false;
                        }
                    }
                }
            }
        }

        Self {
            origin_x: bounds_min_xz[0],
            origin_z: bounds_min_xz[1],
            cols,
            rows,
            cell_size,
            walkable,
        }
    }

    /// Grid cell size in meters (`10.0`).
    pub fn cell_size(&self) -> f32 {
        self.cell_size
    }

    /// Grid dimensions `(cols, rows)`.
    pub fn dimensions(&self) -> (usize, usize) {
        (self.cols, self.rows)
    }

    /// Convert world `(x, z)` to `(col, row)` if inside grid bounds.
    pub fn world_to_cell(&self, x: f32, z: f32) -> Option<(usize, usize)> {
        if !x.is_finite() || !z.is_finite() || self.cols == 0 || self.rows == 0 {
            return None;
        }
        let max_x = self.origin_x + self.cols as f32 * self.cell_size;
        let max_z = self.origin_z + self.rows as f32 * self.cell_size;
        if x < self.origin_x || x > max_x || z < self.origin_z || z > max_z {
            return None;
        }
        let c = (((x - self.origin_x) / self.cell_size).floor() as isize)
            .clamp(0, self.cols as isize - 1) as usize;
        let r = (((z - self.origin_z) / self.cell_size).floor() as isize)
            .clamp(0, self.rows as isize - 1) as usize;
        Some((c, r))
    }

    fn world_to_cell_clamped(&self, x: f32, z: f32) -> Option<(usize, usize)> {
        if !x.is_finite() || !z.is_finite() || self.cols == 0 || self.rows == 0 {
            return None;
        }
        let c = (((x - self.origin_x) / self.cell_size).floor() as isize)
            .clamp(0, self.cols as isize - 1) as usize;
        let r = (((z - self.origin_z) / self.cell_size).floor() as isize)
            .clamp(0, self.rows as isize - 1) as usize;
        Some((c, r))
    }

    /// World-space `(x, z)` center of `(col, row)` in meters.
    pub fn cell_center(&self, col: usize, row: usize) -> (f32, f32) {
        (
            self.origin_x + (col as f32 + 0.5) * self.cell_size,
            self.origin_z + (row as f32 + 0.5) * self.cell_size,
        )
    }

    /// Whether cell `(col, row)` is walkable.
    pub fn is_walkable_cell(&self, col: usize, row: usize) -> bool {
        col < self.cols && row < self.rows && self.walkable[row * self.cols + col]
    }

    /// Whether world coordinate `(x, z)` lies inside a walkable grid cell.
    pub fn is_walkable_world(&self, x: f32, z: f32) -> bool {
        let Some((c, r)) = self.world_to_cell(x, z) else {
            return false;
        };
        self.is_walkable_cell(c, r)
    }

    fn nearest_walkable_cell(
        &self,
        col: usize,
        row: usize,
        max_radius: usize,
    ) -> Option<(usize, usize)> {
        if self.is_walkable_cell(col, row) {
            return Some((col, row));
        }
        let mut best: Option<(usize, usize, usize)> = None;
        for rad in 1..=max_radius {
            let r_lo = row.saturating_sub(rad);
            let r_hi = (row + rad).min(self.rows.saturating_sub(1));
            let c_lo = col.saturating_sub(rad);
            let c_hi = (col + rad).min(self.cols.saturating_sub(1));
            for rr in r_lo..=r_hi {
                for cc in c_lo..=c_hi {
                    if !self.is_walkable_cell(cc, rr) {
                        continue;
                    }
                    let dc = cc.abs_diff(col);
                    let dr = rr.abs_diff(row);
                    let d2 = dc * dc + dr * dr;
                    if best.is_none_or(|(_, _, bd2)| d2 < bd2) {
                        best = Some((cc, rr, d2));
                    }
                }
            }
            if let Some((bc, br, _)) = best {
                return Some((bc, br));
            }
        }
        None
    }

    fn has_line_of_sight_world(&self, a: [f32; 2], b: [f32; 2]) -> bool {
        if self.world_to_cell(a[0], a[1]).is_none() || self.world_to_cell(b[0], b[1]).is_none() {
            return false;
        }
        walk_grid_segment_2d(
            self.origin_x,
            self.origin_z,
            self.cell_size,
            self.cols,
            self.rows,
            a,
            b,
            true,
            |c, r| self.is_walkable_cell(c, r),
        )
    }

    /// Compute a smoothed 3D waypoint path from `from` to `to` (empty if unreachable).
    pub fn find_path(&self, from: Vec3, to: Vec3, terrain: &TerrainMap) -> Vec<Vec3> {
        let Some((raw_sc, raw_sr)) = self.world_to_cell_clamped(from.x, from.z) else {
            return Vec::new();
        };
        let Some((raw_gc, raw_gr)) = self.world_to_cell_clamped(to.x, to.z) else {
            return Vec::new();
        };
        let start_exact_walkable =
            self.world_to_cell(from.x, from.z).is_some() && self.is_walkable_cell(raw_sc, raw_sr);
        let goal_exact_walkable =
            self.world_to_cell(to.x, to.z).is_some() && self.is_walkable_cell(raw_gc, raw_gr);
        let Some((sc, sr)) =
            self.nearest_walkable_cell(raw_sc, raw_sr, NAV_ENDPOINT_SNAP_RADIUS_CELLS)
        else {
            return Vec::new();
        };
        let Some((gc, gr)) =
            self.nearest_walkable_cell(raw_gc, raw_gr, NAV_ENDPOINT_SNAP_RADIUS_CELLS)
        else {
            return Vec::new();
        };

        let n_cells = self.cols * self.rows;
        let start_idx = sr * self.cols + sc;
        let goal_idx = gr * self.cols + gc;

        let cell_path: Vec<(usize, usize)> = if start_idx == goal_idx {
            vec![(sc, sr)]
        } else {
            let mut g_score = vec![f32::INFINITY; n_cells];
            let mut came_from = vec![usize::MAX; n_cells];
            let mut closed = vec![false; n_cells];
            let mut open = BinaryHeap::new();

            g_score[start_idx] = 0.0;
            open.push(AStarNode {
                f_cost: octile_distance(sc, sr, gc, gr),
                g_cost: 0.0,
                idx: start_idx,
            });

            let mut found = false;
            const NEIGHBORS: [(isize, isize, f32); 8] = [
                (1, 0, 1.0),
                (-1, 0, 1.0),
                (0, 1, 1.0),
                (0, -1, 1.0),
                (1, 1, std::f32::consts::SQRT_2),
                (1, -1, std::f32::consts::SQRT_2),
                (-1, 1, std::f32::consts::SQRT_2),
                (-1, -1, std::f32::consts::SQRT_2),
            ];

            while let Some(cur) = open.pop() {
                if cur.idx == goal_idx {
                    found = true;
                    break;
                }
                if closed[cur.idx] {
                    continue;
                }
                closed[cur.idx] = true;

                let cc = cur.idx % self.cols;
                let cr = cur.idx / self.cols;
                for &(dc, dr, step_cost) in &NEIGHBORS {
                    let nc_i = cc as isize + dc;
                    let nr_i = cr as isize + dr;
                    if nc_i < 0
                        || nr_i < 0
                        || nc_i >= self.cols as isize
                        || nr_i >= self.rows as isize
                    {
                        continue;
                    }
                    let nc = nc_i as usize;
                    let nr = nr_i as usize;
                    if !self.is_walkable_cell(nc, nr) {
                        continue;
                    }
                    if dc != 0
                        && dr != 0
                        && (!self.is_walkable_cell(nc, cr) || !self.is_walkable_cell(cc, nr))
                    {
                        continue;
                    }
                    let n_idx = nr * self.cols + nc;
                    if closed[n_idx] {
                        continue;
                    }
                    let tentative_g = cur.g_cost + step_cost;
                    if tentative_g < g_score[n_idx] {
                        came_from[n_idx] = cur.idx;
                        g_score[n_idx] = tentative_g;
                        open.push(AStarNode {
                            f_cost: tentative_g + octile_distance(nc, nr, gc, gr),
                            g_cost: tentative_g,
                            idx: n_idx,
                        });
                    }
                }
            }

            if !found {
                return Vec::new();
            }

            let mut rev = Vec::new();
            let mut walk = goal_idx;
            while walk != usize::MAX {
                rev.push((walk % self.cols, walk / self.cols));
                if walk == start_idx {
                    break;
                }
                walk = came_from[walk];
            }
            rev.reverse();
            rev
        };

        // Build a world-space polyline containing the exact walkable endpoints (if inside
        // walkable cells) alongside walkable A* cell centers, then string-pull directly in
        // continuous world space so endpoint offsets never clip blocked cells.
        let mut world_chain: Vec<[f32; 2]> = Vec::with_capacity(cell_path.len() + 2);
        if start_exact_walkable {
            world_chain.push([from.x, from.z]);
        }
        for &(c, r) in &cell_path {
            let (cx, cz) = self.cell_center(c, r);
            if world_chain
                .last()
                .is_none_or(|p| (p[0] - cx).hypot(p[1] - cz) > 1e-4)
            {
                world_chain.push([cx, cz]);
            }
        }
        if goal_exact_walkable {
            let g_pt = [to.x, to.z];
            if world_chain
                .last()
                .is_none_or(|p| (p[0] - g_pt[0]).hypot(p[1] - g_pt[1]) > 1e-4)
            {
                world_chain.push(g_pt);
            }
        }

        let mut smoothed = Vec::with_capacity(world_chain.len());
        let mut anchor = 0usize;
        smoothed.push(world_chain[0]);
        while anchor + 1 < world_chain.len() {
            let mut farthest = anchor + 1;
            for probe in (anchor + 2..world_chain.len()).rev() {
                if self.has_line_of_sight_world(world_chain[anchor], world_chain[probe]) {
                    farthest = probe;
                    break;
                }
            }
            smoothed.push(world_chain[farthest]);
            anchor = farthest;
        }

        let to_world_pt = |wx: f32, wz: f32| -> Vec3 {
            let wy = terrain.sample_height_clamped(wx, wz).unwrap_or(0.0);
            Vec3::new(wx, wy, wz)
        };

        let mut waypoints: Vec<Vec3> = Vec::with_capacity(smoothed.len());
        for [wx, wz] in smoothed {
            if waypoints
                .last()
                .is_none_or(|prev| (prev.x - wx).hypot(prev.z - wz) > 1e-3)
            {
                waypoints.push(to_world_pt(wx, wz));
            }
        }
        waypoints
    }
}

/// Complete immutable runtime representation of one loaded battle arena.
#[derive(Debug, Clone)]
pub struct ArenaMap {
    pub def: ArenaDef,
    pub bounds: SpaceBounds,
    pub terrain: TerrainMap,
    pub material_kinds: Arc<MaterialKinds>,
    pub obstacles: Vec<StaticObstacle>,
    pub foliage: Vec<FoliageVolume>,
    pub water_volumes: Vec<WaterVolume>,
    pub spawns: Vec<SpawnPoint>,
    pub bases: Vec<TeamBaseSpec>,
    pub destructibles_by_chunk: BTreeMap<u16, Vec<ChunkDestructible>>,
    /// `(chunk_id, position in its chunk list)` of each destructible the spatial grid indexes.
    pub destructible_refs: Vec<(u16, usize)>,
    /// `destructibles.xml` `unitVehicleMass` for the client's ramming damage correction.
    pub unit_vehicle_mass: f32,
    pub spatial_grid: SpatialGrid2D,
    pub nav_grid: NavGrid,
}

impl ArenaMap {
    /// Load an arena from retail `res/` directories using shared catalogs and balance thresholds.
    pub fn load(
        res_dir: &Path,
        def: &ArenaDef,
        material_kinds: Arc<MaterialKinds>,
        destructibles: &DestructiblesCatalog,
        balance: &BalanceConfig,
    ) -> Result<Self, AssetError> {
        let space_dir = res_dir.join(&def.geometry);
        let settings = SpaceSettings::load(&space_dir.join("space.settings"))?;
        let chunk_list = load_space_chunks(&space_dir)?;

        // Load all outer terrain .cdata heightmaps/dominantTextures matching SpaceBounds.
        let mut cdata_chunks = Vec::new();
        for entry in std::fs::read_dir(&space_dir)? {
            let path = entry?.path();
            if path.extension().and_then(|e| e.to_str()) != Some("cdata") {
                continue;
            }
            let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
                continue;
            };
            let Ok(coords) = decode_chunk_coords(name) else {
                continue;
            };
            if coords.kind != ChunkKind::Outer {
                continue;
            }
            let bytes = std::fs::read(&path)?;
            let tc = TerrainChunk::from_cdata_bytes(&bytes)?;
            cdata_chunks.push((coords, tc));
        }
        let terrain = TerrainMap::from_chunks(settings.bounds, cdata_chunks)?;

        // Load unique water VLO volumes referenced by outer chunks.
        let mut vlo_uids = BTreeSet::new();
        for chunk in &chunk_list {
            for uid in &chunk.vlo_uids {
                vlo_uids.insert(uid.clone());
            }
        }
        let mut water_volumes = Vec::with_capacity(vlo_uids.len());
        for uid in vlo_uids {
            water_volumes.push(WaterVolume::load(&space_dir, &uid)?);
        }

        let mut bsp_cache: HashMap<PathBuf, Option<Arc<BspTree>>> = HashMap::new();
        let mut tree_bsp_cache: HashMap<String, Option<Arc<BspTree>>> = HashMap::new();
        let mut obstacles = Vec::new();
        let mut foliage = Vec::new();
        let mut spawns = Vec::new();
        let mut bases = Vec::new();
        let mut destructibles_by_chunk: BTreeMap<u16, Vec<ChunkDestructible>> = BTreeMap::new();

        for chunk in chunk_list {
            spawns.extend(chunk.spawns);
            for cp in chunk.control_points {
                bases.push(TeamBaseSpec {
                    team: cp.team,
                    position: cp.position,
                    radius: cp.radius,
                    points_per_second: cp.points_per_second,
                    max_points_per_second: cp.max_points_per_second,
                    stop_invaders: cp.stop_invaders,
                });
            }

            let chunk_id = chunk_id_from_indexes(chunk.coords.grid_x, chunk.coords.grid_z);
            for model in chunk.models {
                let maybe_bsp = load_cached_bsp(res_dir, &mut bsp_cache, &model.resource);
                if let Some(desc) = destructibles
                    .by_filename(&model.resource)
                    .filter(|_| addressable_destr_index(chunk_id, model.destr_index))
                {
                    let (world_aabb_min, world_aabb_max) = match &maybe_bsp {
                        Some((_, bsp)) => {
                            let (lmin, lmax) = bsp.aabb();
                            transform_aabb(model.world_transform, lmin, lmax)
                        }
                        None => {
                            let p = model.world_transform.w_axis.truncate();
                            (p - Vec3::splat(0.5), p + Vec3::splat(0.5))
                        }
                    };
                    let entries = destructibles_by_chunk.entry(chunk_id).or_default();
                    let index_in_chunk = model.destr_index;
                    entries.push(ChunkDestructible {
                        chunk_id,
                        index_in_chunk,
                        desc_id: desc.id,
                        filename: desc.filename.clone(),
                        kind: desc.kind.clone(),
                        world_transform: model.world_transform,
                        position: model.world_transform.w_axis.truncate(),
                        world_aabb_min,
                        world_aabb_max,
                        bsp: maybe_bsp.map(|(_, bsp)| bsp),
                        pieces: Vec::new(),
                    });
                } else if let Some((prim_path, bsp)) = maybe_bsp {
                    if let Some(obs) = StaticObstacle::from_bsp(
                        model.resource,
                        prim_path,
                        model.world_transform,
                        bsp,
                    ) {
                        obstacles.push(obs);
                    }
                }
            }

            for spt in chunk.speedtrees {
                let tree_bsp = load_cached_tree_bsp(res_dir, &mut tree_bsp_cache, &spt.spt);
                let position = spt.world_transform.w_axis.truncate();
                let sx = spt.world_transform.x_axis.truncate().length();
                let sy = spt.world_transform.y_axis.truncate().length();
                let sz = spt.world_transform.z_axis.truncate().length();
                let radius = 3.0 * sx.max(sz).max(0.1);
                let height = 6.0 * sy.max(0.1);
                let world_aabb_min = Vec3::new(position.x - radius, position.y, position.z - radius);
                let world_aabb_max =
                    Vec3::new(position.x + radius, position.y + height, position.z + radius);

                let (density, destructible_ref) = if let Some(desc) = destructibles
                    .by_filename(&spt.spt)
                    .filter(|_| addressable_destr_index(chunk_id, spt.destr_index))
                {
                    let d = match &desc.kind {
                        DestructibleKind::Tree { density, .. } => *density,
                        _ => 1.0,
                    };
                    let entries = destructibles_by_chunk.entry(chunk_id).or_default();
                    let index_in_chunk = spt.destr_index;
                    entries.push(ChunkDestructible {
                        chunk_id,
                        index_in_chunk,
                        desc_id: desc.id,
                        filename: desc.filename.clone(),
                        kind: desc.kind.clone(),
                        world_transform: spt.world_transform,
                        position,
                        world_aabb_min,
                        world_aabb_max,
                        bsp: tree_bsp,
                        pieces: Vec::new(),
                    });
                    (d, Some((chunk_id, index_in_chunk)))
                } else {
                    if let Some(bsp) = tree_bsp {
                        let path = res_dir.join(normalize_res_rel(&spt.spt));
                        if let Some(obs) = StaticObstacle::from_bsp(spt.spt.clone(), path, spt.world_transform, bsp) {
                            obstacles.push(obs);
                        }
                    }
                    (1.0, None)
                };

                foliage.push(FoliageVolume {
                    spt_path: spt.spt,
                    position,
                    radius,
                    height,
                    density,
                    world_aabb_min,
                    world_aabb_max,
                    destructible_ref,
                });
            }
        }

        let mut map = Self::from_parts(
            def.clone(),
            settings.bounds,
            terrain,
            material_kinds,
            obstacles,
            foliage,
            water_volumes,
            spawns,
            bases,
            destructibles_by_chunk,
            balance,
        );
        map.unit_vehicle_mass = destructibles.unit_vehicle_mass;
        Ok(map)
    }

    /// Construct an [`ArenaMap`] directly from parsed parts (used by synthetic tests and loaders).
    #[allow(clippy::too_many_arguments)]
    pub fn from_parts(
        def: ArenaDef,
        bounds: SpaceBounds,
        terrain: TerrainMap,
        material_kinds: Arc<MaterialKinds>,
        obstacles: Vec<StaticObstacle>,
        foliage: Vec<FoliageVolume>,
        water_volumes: Vec<WaterVolume>,
        spawns: Vec<SpawnPoint>,
        bases: Vec<TeamBaseSpec>,
        destructibles_by_chunk: BTreeMap<u16, Vec<ChunkDestructible>>,
        balance: &BalanceConfig,
    ) -> Self {
        let mut obstacles = obstacles;
        let mut destructibles_by_chunk = destructibles_by_chunk;
        cut_to_hull_band(&terrain, &mut obstacles, &mut destructibles_by_chunk);
        let (terrain_min, terrain_max) = terrain.bounds_meters();
        let mut spatial_grid = SpatialGrid2D::build(terrain_min, terrain_max, &obstacles, &foliage);
        let destructible_refs = spatial_grid.index_destructibles(&destructibles_by_chunk);
        let (nav_min, nav_max) = playable_bounds(&def, terrain_min, terrain_max);
        let nav_grid = NavGrid::build(
            nav_min,
            nav_max,
            &terrain,
            &obstacles,
            &water_volumes,
            balance.max_climbable_slope_cos(),
            balance.max_fordable_water_depth_m,
        );
        Self {
            def,
            bounds,
            terrain,
            material_kinds,
            obstacles,
            foliage,
            water_volumes,
            spawns,
            bases,
            destructibles_by_chunk,
            destructible_refs,
            unit_vehicle_mass: DEFAULT_UNIT_VEHICLE_MASS,
            spatial_grid,
            nav_grid,
        }
    }

    /// Recut collision geometry and reindex it after obstacles or destructibles changed.
    pub fn rebuild_collision_index(&mut self) {
        cut_to_hull_band(&self.terrain, &mut self.obstacles, &mut self.destructibles_by_chunk);
        let (terrain_min, terrain_max) = self.terrain.bounds_meters();
        self.spatial_grid = SpatialGrid2D::build(terrain_min, terrain_max, &self.obstacles, &self.foliage);
        self.destructible_refs = self.spatial_grid.index_destructibles(&self.destructibles_by_chunk);
    }

    /// Candidate destructibles whose collision geometry overlaps the horizontal AABB.
    pub fn query_destructibles_aabb(&self, min_xz: [f32; 2], max_xz: [f32; 2]) -> Vec<&ChunkDestructible> {
        self.spatial_grid
            .query_destructibles_aabb(min_xz, max_xz)
            .into_iter()
            .filter_map(|i| {
                let (chunk_id, position) = *self.destructible_refs.get(i)?;
                self.destructibles_by_chunk.get(&chunk_id)?.get(position)
            })
            .collect()
    }

    /// Sample terrain height in meters at world `(x, z)`.
    pub fn sample_height(&self, x: f32, z: f32) -> Option<f32> {
        self.terrain.sample_height(x, z)
    }

    /// Sample terrain height in meters and unit surface normal at world `(x, z)`.
    pub fn sample_height_and_normal(&self, x: f32, z: f32) -> Option<(f32, Vec3)> {
        self.terrain.sample_height_and_normal(x, z)
    }

    /// Water depth in meters above terrain if `(x, z)` lies inside a water volume, else `None`.
    pub fn water_depth_at(&self, x: f32, z: f32) -> Option<f32> {
        let terrain_h = self.terrain.sample_height(x, z)?;
        water_depth_at_volumes(&self.water_volumes, x, z, terrain_h)
    }

    /// Whether `(x, z)` lies submerged inside a [`WaterVolume`].
    pub fn is_in_water(&self, x: f32, z: f32) -> bool {
        self.water_depth_at(x, z).is_some()
    }

    /// Dominant [`MaterialKind`] at world `(x, z)`.
    pub fn dominant_material_at(&self, x: f32, z: f32) -> Option<&MaterialKind> {
        self.terrain.dominant_material_at(x, z, &self.material_kinds)
    }

    /// Candidate static obstacles overlapping the horizontal AABB.
    pub fn query_obstacles_aabb(&self, min_xz: [f32; 2], max_xz: [f32; 2]) -> Vec<&StaticObstacle> {
        self.spatial_grid
            .query_obstacles_aabb(min_xz, max_xz)
            .into_iter()
            .map(|i| &self.obstacles[i])
            .collect()
    }

    /// Candidate static obstacles along horizontal segment `p0 -> p1`.
    pub fn query_obstacles_segment(&self, p0: Vec3, p1: Vec3) -> Vec<&StaticObstacle> {
        self.spatial_grid
            .query_obstacles_segment(p0, p1)
            .into_iter()
            .map(|i| &self.obstacles[i])
            .collect()
    }

    /// Candidate foliage volumes overlapping the horizontal AABB.
    pub fn query_foliage_aabb(&self, min_xz: [f32; 2], max_xz: [f32; 2]) -> Vec<&FoliageVolume> {
        self.spatial_grid
            .query_foliage_aabb(min_xz, max_xz)
            .into_iter()
            .map(|i| &self.foliage[i])
            .collect()
    }

    /// Candidate foliage volumes along horizontal segment `p0 -> p1`.
    pub fn query_foliage_segment(&self, p0: Vec3, p1: Vec3) -> Vec<&FoliageVolume> {
        self.spatial_grid
            .query_foliage_segment(p0, p1)
            .into_iter()
            .map(|i| &self.foliage[i])
            .collect()
    }

    /// Cast a world ray against static obstacles via the 2D spatial grid, returning earliest hit.
    pub fn raycast_obstacles(
        &self,
        origin: Vec3,
        dir: Vec3,
        max_dist: f32,
    ) -> Option<(usize, WorldBspHit)> {
        if !is_valid_ray(origin, dir, max_dist) {
            return None;
        }
        let candidates = self.spatial_grid.query_obstacles_ray(origin, dir, max_dist);
        let mut best: Option<(usize, WorldBspHit)> = None;
        let mut best_t = max_dist;
        for idx in candidates {
            if let Some(hit) = self.obstacles[idx].raycast(origin, dir, best_t) {
                best_t = hit.t;
                best = Some((idx, hit));
            }
        }
        best
    }

    /// Find a smoothed path on [`Self::nav_grid`] between `from` and `to`.
    pub fn find_path(&self, from: Vec3, to: Vec3) -> Vec<Vec3> {
        self.nav_grid.find_path(from, to, &self.terrain)
    }

    /// Spawn points belonging to `team` (`1` or `2`).
    pub fn spawns_for_team(&self, team: u8) -> Vec<&SpawnPoint> {
        self.spawns.iter().filter(|s| s.team == team).collect()
    }

    /// Capture base zone belonging to `team` (`1` or `2`).
    pub fn base_for_team(&self, team: u8) -> Option<&TeamBaseSpec> {
        self.bases.iter().find(|b| b.team == team)
    }
}

/// Thread-safe lazy cache of loaded [`ArenaMap`]s keyed by `arena_type_id`.
#[derive(Debug)]
pub struct MapStore {
    res_dir: PathBuf,
    arenas: ArenaDefsCatalog,
    material_kinds: Arc<MaterialKinds>,
    destructibles: Arc<DestructiblesCatalog>,
    balance: BalanceConfig,
    loaded: RwLock<HashMap<i32, Arc<ArenaMap>>>,
}

impl MapStore {
    /// Load startup arena metadata catalogs (`arena_defs`, `material_kinds`, `destructibles`)
    /// without parsing any individual space chunks until [`Self::get_or_load`].
    pub fn new(res_dir: impl AsRef<Path>, balance: BalanceConfig) -> Result<Self, AssetError> {
        let res_dir = res_dir.as_ref().to_path_buf();
        let arenas = ArenaDefsCatalog::load(&res_dir)?;
        let material_kinds = Arc::new(MaterialKinds::load(&res_dir)?);
        let destructibles = Arc::new(DestructiblesCatalog::load(&res_dir)?);
        Ok(Self {
            res_dir,
            arenas,
            material_kinds,
            destructibles,
            balance,
            loaded: RwLock::new(HashMap::new()),
        })
    }

    /// Construct an in-memory [`MapStore`] from pre-built maps (useful for synthetic tests and headless fixtures).
    pub fn from_preloaded(maps: Vec<Arc<ArenaMap>>, balance: BalanceConfig) -> Self {
        let arenas = ArenaDefsCatalog {
            maps: maps.iter().map(|m| m.def.clone()).collect(),
        };
        let material_kinds = maps
            .first()
            .map(|m| Arc::clone(&m.material_kinds))
            .unwrap_or_else(|| {
                Arc::new(MaterialKinds::parse(b"<root/>").expect("empty material_kinds"))
            });
        let destructibles =
            Arc::new(DestructiblesCatalog::parse(b"<root/>").expect("empty destructibles"));
        let loaded = maps.into_iter().map(|m| (m.def.id, m)).collect();
        Self {
            res_dir: PathBuf::new(),
            arenas,
            material_kinds,
            destructibles,
            balance,
            loaded: RwLock::new(loaded),
        }
    }

    /// Retail `res/` root path.
    pub fn res_dir(&self) -> &Path {
        &self.res_dir
    }

    /// Arena metadata catalog loaded from `scripts/arena_defs/_list_.xml`.
    pub fn arenas(&self) -> &ArenaDefsCatalog {
        &self.arenas
    }

    /// Material kinds catalog loaded from `system/data/material_kinds.xml`.
    pub fn material_kinds(&self) -> &MaterialKinds {
        &self.material_kinds
    }

    /// Destructibles catalog loaded from `scripts/destructibles.xml`.
    pub fn destructibles(&self) -> &DestructiblesCatalog {
        &self.destructibles
    }

    /// Number of currently cached maps.
    pub fn cached_count(&self) -> usize {
        self.loaded
            .read()
            .expect("MapStore lock poisoned")
            .len()
    }

    /// Lazily load or retrieve a cached [`Arc<ArenaMap>`] for `arena_type_id`.
    pub fn get_or_load(&self, arena_type_id: i32) -> Result<Arc<ArenaMap>, AssetError> {
        let existing = self
            .loaded
            .read()
            .expect("MapStore lock poisoned")
            .get(&arena_type_id)
            .cloned();
        if let Some(map) = existing {
            return Ok(map);
        }

        let def = self
            .arenas
            .by_id(arena_type_id)
            .ok_or_else(|| AssetError::NotFound(format!("unknown arena_type_id {arena_type_id}")))?
            .clone();

        let loaded_map = ArenaMap::load(
            &self.res_dir,
            &def,
            Arc::clone(&self.material_kinds),
            &self.destructibles,
            &self.balance,
        )?;

        let mut guard = self.loaded.write().expect("MapStore lock poisoned");
        let entry = guard
            .entry(arena_type_id)
            .or_insert_with(|| Arc::new(loaded_map));
        Ok(Arc::clone(entry))
    }
}

/// Component collision HitTester pairing a shared local-space [`BspTree`] with XML bounds.
#[derive(Debug, Clone)]
pub struct HitTester {
    pub model_path: String,
    pub bsp: Arc<BspTree>,
    pub enclosing_box: Option<(Vec3, Vec3)>,
}

impl HitTester {
    /// Local-space bounding box from XML `enclosing_box` if present, else [`BspTree::aabb`].
    pub fn local_aabb(&self) -> (Vec3, Vec3) {
        self.enclosing_box.unwrap_or_else(|| self.bsp.aabb())
    }

    /// Cast a local-space ray and return the earliest armor/module surface hit.
    pub fn raycast(&self, local_origin: Vec3, local_dir: Vec3, max_dist: f32) -> Option<BspHit> {
        self.bsp.raycast(local_origin, local_dir, max_dist)
    }

    /// Cast a local-space ray and return all sorted armor/module intersections.
    pub fn raycast_all(&self, local_origin: Vec3, local_dir: Vec3, max_dist: f32) -> Vec<BspHit> {
        self.bsp.raycast_all(local_origin, local_dir, max_dist)
    }
}

/// Complete tank collision set for one `(vehicle, chassis, turret, gun)` configuration.
#[derive(Debug, Clone)]
pub struct VehicleCollisionSet {
    pub hull: HitTester,
    pub chassis: HitTester,
    pub turret: HitTester,
    pub gun: HitTester,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
struct VehicleCollisionKey {
    veh_type_cd: i32,
    chassis_id: u16,
    turret_id: u16,
    gun_id: u16,
}

/// Thread-safe lazy cache for tank component [`HitTester`]s and [`VehicleCollisionSet`]s.
#[derive(Debug)]
pub struct VehicleCollisionCache {
    res_dir: PathBuf,
    bsp_by_primitives: RwLock<HashMap<PathBuf, Arc<BspTree>>>,
    vehicles: RwLock<HashMap<VehicleCollisionKey, Arc<VehicleCollisionSet>>>,
}

impl VehicleCollisionCache {
    /// Create an empty collision cache rooted at `res_dir`.
    pub fn new(res_dir: impl AsRef<Path>) -> Self {
        Self {
            res_dir: res_dir.as_ref().to_path_buf(),
            bsp_by_primitives: RwLock::new(HashMap::new()),
            vehicles: RwLock::new(HashMap::new()),
        }
    }

    /// Resolve, parse, and cache the BSP2 collision mesh for one [`HitTesterSpec`].
    pub fn get_or_load_model(&self, spec: &HitTesterSpec) -> Result<HitTester, AssetError> {
        let model_rel = normalize_res_rel(&spec.model);
        let resolved = resolve_model_visual(&self.res_dir, &model_rel)?;
        let prim_path = resolved.primitives_path;

        let cached = self
            .bsp_by_primitives
            .read()
            .expect("VehicleCollisionCache lock poisoned")
            .get(&prim_path)
            .cloned();
        let bsp = match cached {
            Some(existing) => existing,
            None => {
                let parsed = load_bsp_from_primitives(&prim_path)?;
                let mut guard = self
                    .bsp_by_primitives
                    .write()
                    .expect("VehicleCollisionCache lock poisoned");
                let entry = guard
                    .entry(prim_path)
                    .or_insert_with(|| Arc::new(parsed));
                Arc::clone(entry)
            }
        };

        let enclosing_box = spec
            .enclosing_box
            .map(|(min, max)| (Vec3::from_array(min), Vec3::from_array(max)));

        Ok(HitTester {
            model_path: model_rel,
            bsp,
            enclosing_box,
        })
    }

    /// Load and cache all 4 collision meshes (`hull`, `chassis`, `turret`, `gun`) by slice index.
    pub fn get_or_load_vehicle(
        &self,
        veh: &VehicleDef,
        chassis_idx: usize,
        turret_idx: usize,
        gun_idx: usize,
    ) -> Result<Arc<VehicleCollisionSet>, AssetError> {
        let chassis_def = veh.chassis.get(chassis_idx).ok_or_else(|| {
            AssetError::NotFound(format!(
                "vehicle `{}` chassis index {chassis_idx} out of bounds",
                veh.name
            ))
        })?;
        let turret_def = veh.turrets.get(turret_idx).ok_or_else(|| {
            AssetError::NotFound(format!(
                "vehicle `{}` turret index {turret_idx} out of bounds",
                veh.name
            ))
        })?;
        let gun_def = turret_def.guns.get(gun_idx).ok_or_else(|| {
            AssetError::NotFound(format!(
                "vehicle `{}` gun index {gun_idx} out of bounds on turret {turret_idx}",
                veh.name
            ))
        })?;

        let key = VehicleCollisionKey {
            veh_type_cd: veh.veh_type_comp_descr,
            chassis_id: chassis_def.id,
            turret_id: turret_def.component.id,
            gun_id: gun_def.component.id,
        };
        let existing = self
            .vehicles
            .read()
            .expect("VehicleCollisionCache lock poisoned")
            .get(&key)
            .cloned();
        if let Some(set) = existing {
            return Ok(set);
        }

        let hull_spec = veh.hull_hit_tester.as_ref().ok_or_else(|| {
            AssetError::NotFound(format!("vehicle `{}` missing hull hitTester", veh.name))
        })?;
        let chassis_spec = chassis_def.hit_tester.as_ref().ok_or_else(|| {
            AssetError::NotFound(format!(
                "vehicle `{}` chassis `{}` missing hitTester",
                veh.name, chassis_def.name
            ))
        })?;
        let turret_spec = turret_def.hit_tester.as_ref().ok_or_else(|| {
            AssetError::NotFound(format!(
                "vehicle `{}` turret `{}` missing hitTester",
                veh.name, turret_def.component.name
            ))
        })?;
        let gun_spec = gun_def.hit_tester.as_ref().ok_or_else(|| {
            AssetError::NotFound(format!(
                "vehicle `{}` gun `{}` missing hitTester",
                veh.name, gun_def.component.name
            ))
        })?;

        let set = VehicleCollisionSet {
            hull: self.get_or_load_model(hull_spec)?,
            chassis: self.get_or_load_model(chassis_spec)?,
            turret: self.get_or_load_model(turret_spec)?,
            gun: self.get_or_load_model(gun_spec)?,
        };

        let mut guard = self
            .vehicles
            .write()
            .expect("VehicleCollisionCache lock poisoned");
        let entry = guard.entry(key).or_insert_with(|| Arc::new(set));
        Ok(Arc::clone(entry))
    }

    /// Load and cache collision meshes by component numeric IDs `(chassis_id, turret_id, gun_id)`.
    pub fn get_or_load_by_ids(
        &self,
        veh: &VehicleDef,
        chassis_id: u16,
        turret_id: u16,
        gun_id: u16,
    ) -> Result<Arc<VehicleCollisionSet>, AssetError> {
        let chassis_idx = veh
            .chassis
            .iter()
            .position(|c| c.id == chassis_id)
            .ok_or_else(|| {
                AssetError::NotFound(format!(
                    "vehicle `{}` missing chassis id {chassis_id}",
                    veh.name
                ))
            })?;
        let turret_idx = veh
            .turrets
            .iter()
            .position(|t| t.component.id == turret_id)
            .ok_or_else(|| {
                AssetError::NotFound(format!(
                    "vehicle `{}` missing turret id {turret_id}",
                    veh.name
                ))
            })?;
        let gun_idx = veh.turrets[turret_idx]
            .guns
            .iter()
            .position(|g| g.component.id == gun_id)
            .ok_or_else(|| {
                AssetError::NotFound(format!(
                    "vehicle `{}` turret {turret_id} missing gun id {gun_id}",
                    veh.name
                ))
            })?;
        self.get_or_load_vehicle(veh, chassis_idx, turret_idx, gun_idx)
    }
}

fn normalize_res_rel(rel: &str) -> String {
    rel.trim()
        .replace('\\', "/")
        .trim_start_matches('/')
        .to_string()
}

fn load_bsp_from_primitives(prim_path: &Path) -> Result<BspTree, AssetError> {
    let bytes = std::fs::read(prim_path)?;
    let secs = BinSectionMap::parse(&bytes)?;
    let bsp2 = secs.get("bsp2").ok_or_else(|| {
        AssetError::Bsp2(format!(
            "`{}` missing `bsp2` section",
            prim_path.display()
        ))
    })?;
    let mats = secs.get("bsp2_materials").ok_or_else(|| {
        AssetError::Bsp2(format!(
            "`{}` missing `bsp2_materials` section",
            prim_path.display()
        ))
    })?;
    BspTree::parse(bsp2, mats)
}

fn load_cached_bsp(
    res_dir: &Path,
    cache: &mut HashMap<PathBuf, Option<Arc<BspTree>>>,
    model_rel: &str,
) -> Option<(PathBuf, Arc<BspTree>)> {
    let model_rel = normalize_res_rel(model_rel);
    if model_rel.is_empty() {
        return None;
    }
    let resolved = match resolve_model_visual(res_dir, &model_rel) {
        Ok(r) => r,
        Err(err) => {
            tracing::debug!(model = %model_rel, error = %err, "skipping unresolved model");
            return None;
        }
    };
    let prim_path = resolved.primitives_path;
    if let Some(entry) = cache.get(&prim_path) {
        return entry.as_ref().map(|b| (prim_path, Arc::clone(b)));
    }
    let loaded = match load_bsp_from_primitives(&prim_path) {
        Ok(tree) if !tree.triangles().is_empty() => Some(Arc::new(tree)),
        Ok(_) => None,
        Err(err) => {
            tracing::debug!(
                primitives = %prim_path.display(),
                error = %err,
                "skipping model without collision bsp2"
            );
            None
        }
    };
    cache.insert(prim_path.clone(), loaded.clone());
    loaded.map(|b| (prim_path, b))
}

/// Cut every collision BSP still without pieces to the hull band above `terrain`.
fn cut_to_hull_band(
    terrain: &TerrainMap,
    obstacles: &mut [StaticObstacle],
    destructibles: &mut BTreeMap<u16, Vec<ChunkDestructible>>,
) {
    let ground = |x: f32, z: f32| terrain.sample_height_clamped(x, z).unwrap_or(0.0);
    for obstacle in obstacles.iter_mut().filter(|o| o.pieces.is_empty()) {
        obstacle.pieces = band_outline(&obstacle.bsp, obstacle.world_transform, ground, BAND_TILE);
    }
    for d in destructibles.values_mut().flatten().filter(|d| d.pieces.is_empty()) {
        if let Some(bsp) = &d.bsp {
            d.pieces = band_outline(bsp, d.world_transform, ground, BAND_TILE);
        }
    }
}

fn pieces_bounds(pieces: &[BandPiece]) -> Option<(Vec2, Vec2)> {
    let first = pieces.first()?;
    Some(pieces.iter().fold((first.min, first.max), |(lo, hi), p| (lo.min(p.min), hi.max(p.max))))
}

/// Collision BSP of a speedtree: the `.bsp2` file next to its `.spt`.
fn load_cached_tree_bsp(
    res_dir: &Path,
    cache: &mut HashMap<String, Option<Arc<BspTree>>>,
    spt_rel: &str,
) -> Option<Arc<BspTree>> {
    let key = normalize_res_rel(spt_rel);
    if let Some(entry) = cache.get(&key) {
        return entry.clone();
    }
    let path = res_dir.join(&key).with_extension("bsp2");
    let loaded = match std::fs::read(&path).map(|bytes| BspTree::parse(&bytes, &[])) {
        Ok(Ok(tree)) if !tree.triangles().is_empty() => Some(Arc::new(tree)),
        Ok(Ok(_)) => None,
        Ok(Err(err)) => {
            tracing::debug!(tree = %path.display(), error = %err, "skipping unreadable tree bsp2");
            None
        }
        Err(_) => None,
    };
    cache.insert(key, loaded.clone());
    loaded
}

/// `AreaDestructibles` addresses a destructible by a one-byte slot; a catalog item past slot 255
/// cannot be synced, so it stays intact instead of aliasing another object's slot.
fn addressable_destr_index(chunk_id: u16, destr_index: usize) -> bool {
    let addressable = destr_index <= usize::from(u8::MAX);
    if !addressable {
        tracing::warn!(chunk_id, destr_index, "destructible slot exceeds the client's 256 limit");
    }
    addressable
}

fn transform_aabb(m: Mat4, local_min: Vec3, local_max: Vec3) -> (Vec3, Vec3) {
    let corners = [
        Vec3::new(local_min.x, local_min.y, local_min.z),
        Vec3::new(local_max.x, local_min.y, local_min.z),
        Vec3::new(local_min.x, local_max.y, local_min.z),
        Vec3::new(local_max.x, local_max.y, local_min.z),
        Vec3::new(local_min.x, local_min.y, local_max.z),
        Vec3::new(local_max.x, local_min.y, local_max.z),
        Vec3::new(local_min.x, local_max.y, local_max.z),
        Vec3::new(local_max.x, local_max.y, local_max.z),
    ];
    let mut out_min = Vec3::splat(f32::INFINITY);
    let mut out_max = Vec3::splat(f32::NEG_INFINITY);
    for c in corners {
        let p = m.transform_point3(c);
        out_min = out_min.min(p);
        out_max = out_max.max(p);
    }
    (out_min, out_max)
}

fn ray_aabb_intersects(
    origin: Vec3,
    dir: Vec3,
    max_dist: f32,
    aabb_min: Vec3,
    aabb_max: Vec3,
) -> bool {
    let mut t_min = 0.0f32;
    let mut t_max = max_dist;
    for axis in 0..3 {
        let o = origin[axis];
        let d = dir[axis];
        let lo = aabb_min[axis];
        let hi = aabb_max[axis];
        if d.abs() < 1e-12 {
            if o < lo || o > hi {
                return false;
            }
        } else {
            let inv = 1.0 / d;
            let mut ta = (lo - o) * inv;
            let mut tb = (hi - o) * inv;
            if ta > tb {
                std::mem::swap(&mut ta, &mut tb);
            }
            t_min = t_min.max(ta);
            t_max = t_max.min(tb);
            if t_max < t_min {
                return false;
            }
        }
    }
    true
}

fn is_valid_ray(origin: Vec3, dir: Vec3, max_dist: f32) -> bool {
    origin.is_finite() && dir.is_finite() && dir.length_squared() >= 1e-12 && max_dist >= 0.0
}

fn clip_ray_2d(
    origin: [f32; 2],
    dir: [f32; 2],
    max_t: f32,
    min_b: [f32; 2],
    max_b: [f32; 2],
) -> Option<(f32, f32)> {
    let mut t0 = 0.0f32;
    let mut t1 = max_t;
    for axis in 0..2 {
        let p = -dir[axis];
        let q_min = origin[axis] - min_b[axis];
        let r_max = max_b[axis] - origin[axis];
        for (denom, num) in [(p, q_min), (dir[axis], r_max)] {
            if denom.abs() < 1e-12 {
                if num < 0.0 {
                    return None;
                }
            } else {
                let r = num / denom;
                if denom < 0.0 {
                    t0 = t0.max(r);
                } else {
                    t1 = t1.min(r);
                }
                if t0 > t1 {
                    return None;
                }
            }
        }
    }
    Some((t0, t1))
}

fn clip_segment_2d(
    p0: [f32; 2],
    p1: [f32; 2],
    min_b: [f32; 2],
    max_b: [f32; 2],
) -> Option<(f32, f32)> {
    clip_ray_2d(p0, [p1[0] - p0[0], p1[1] - p0[1]], 1.0, min_b, max_b)
}

fn point_in_tri_2d(p: [f32; 2], a: [f32; 2], b: [f32; 2], c: [f32; 2]) -> bool {
    let cross = |u: [f32; 2], v: [f32; 2], w: [f32; 2]| -> f32 {
        (v[0] - u[0]) * (w[1] - u[1]) - (v[1] - u[1]) * (w[0] - u[0])
    };
    let d1 = cross(p, a, b);
    let d2 = cross(p, b, c);
    let d3 = cross(p, c, a);
    let has_neg = d1 < 0.0 || d2 < 0.0 || d3 < 0.0;
    let has_pos = d1 > 0.0 || d2 > 0.0 || d3 > 0.0;
    !(has_neg && has_pos)
}

fn tri_intersects_rect_xz(
    a: [f32; 2],
    b: [f32; 2],
    c: [f32; 2],
    min_b: [f32; 2],
    max_b: [f32; 2],
) -> bool {
    if clip_segment_2d(a, b, min_b, max_b).is_some()
        || clip_segment_2d(b, c, min_b, max_b).is_some()
        || clip_segment_2d(c, a, min_b, max_b).is_some()
    {
        return true;
    }
    let corners = [
        [min_b[0], min_b[1]],
        [max_b[0], min_b[1]],
        [max_b[0], max_b[1]],
        [min_b[0], max_b[1]],
    ];
    corners.iter().any(|&p| point_in_tri_2d(p, a, b, c))
}

fn octile_distance(c0: usize, r0: usize, c1: usize, r1: usize) -> f32 {
    let dc = c0.abs_diff(c1) as f32;
    let dr = r0.abs_diff(r1) as f32;
    let diag = dc.min(dr);
    let straight = dc.max(dr) - diag;
    diag * std::f32::consts::SQRT_2 + straight
}

fn water_depth_at_volumes(
    water_volumes: &[WaterVolume],
    x: f32,
    z: f32,
    terrain_h: f32,
) -> Option<f32> {
    let mut max_depth: Option<f32> = None;
    for vol in water_volumes {
        if !vol.contains_xz(x, z) {
            continue;
        }
        let depth = vol.position.y - terrain_h;
        if depth > 0.0 {
            max_depth = Some(max_depth.map_or(depth, |d| d.max(depth)));
        }
    }
    max_depth
}

fn playable_bounds(
    def: &ArenaDef,
    terrain_min: [f32; 2],
    terrain_max: [f32; 2],
) -> ([f32; 2], [f32; 2]) {
    let (bl, ur) = def.bounding_box;
    if ur[0] > bl[0] + NAV_CELL_SIZE_METERS && ur[1] > bl[1] + NAV_CELL_SIZE_METERS {
        let min_x = bl[0].clamp(terrain_min[0], terrain_max[0]);
        let min_z = bl[1].clamp(terrain_min[1], terrain_max[1]);
        let max_x = ur[0].clamp(min_x, terrain_max[0]);
        let max_z = ur[1].clamp(min_z, terrain_max[1]);
        ([min_x, min_z], [max_x, max_z])
    } else {
        (terrain_min, terrain_max)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::assets::{
        decode_chunk_coords, retail_res_dir, GroundStrength, HEIGHTMAP_GRID_SIZE,
    };
    use flate2::{write::ZlibEncoder, Compression};
    use std::io::Write;
    use wot065_data::catalog;

    struct TempTestDir(PathBuf);

    impl TempTestDir {
        fn new(name: &str) -> Self {
            let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("../../target/test-tmp")
                .join(format!("{name}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).expect("create target/test-tmp subdir");
            Self(dir)
        }
    }

    impl Drop for TempTestDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn flat_heights_section(height_m: f32) -> Vec<u8> {
        let mut grid = [0i32; HEIGHTMAP_GRID_SIZE * HEIGHTMAP_GRID_SIZE];
        let mm = (height_m * 1000.0).round() as i32;
        grid.fill(mm);
        heights_section_from_mm(&grid)
    }

    fn heights_section_from_mm(grid: &[i32; HEIGHTMAP_GRID_SIZE * HEIGHTMAP_GRID_SIZE]) -> Vec<u8> {
        let mut rgba = Vec::with_capacity(HEIGHTMAP_GRID_SIZE * HEIGHTMAP_GRID_SIZE * 4);
        let mut min_mm = i32::MAX;
        let mut max_mm = i32::MIN;
        for &mm in grid {
            min_mm = min_mm.min(mm);
            max_mm = max_mm.max(mm);
            rgba.extend_from_slice(&mm.to_le_bytes());
        }
        let mut png_buf = Vec::new();
        {
            let mut enc = png::Encoder::new(
                &mut png_buf,
                HEIGHTMAP_GRID_SIZE as u32,
                HEIGHTMAP_GRID_SIZE as u32,
            );
            enc.set_color(png::ColorType::Rgba);
            enc.set_depth(png::BitDepth::Eight);
            let mut writer = enc.write_header().unwrap();
            writer.write_image_data(&rgba).unwrap();
        }
        let mut out = Vec::new();
        out.extend_from_slice(b"hmp\0");
        out.extend_from_slice(&(HEIGHTMAP_GRID_SIZE as u32).to_le_bytes());
        out.extend_from_slice(&(HEIGHTMAP_GRID_SIZE as u32).to_le_bytes());
        out.extend_from_slice(&0u32.to_le_bytes());
        out.extend_from_slice(&4u32.to_le_bytes());
        out.extend_from_slice(&(min_mm as f32 * 0.001).to_le_bytes());
        out.extend_from_slice(&(max_mm as f32 * 0.001).to_le_bytes());
        out.extend_from_slice(&0u32.to_le_bytes());
        out.extend_from_slice(b"gnpq");
        out.extend_from_slice(&png_buf);
        out
    }

    fn dom_tex_section(tex_name: &str) -> Vec<u8> {
        let str_len = 48u32;
        let mut out = Vec::new();
        out.extend_from_slice(b"mat\0");
        out.extend_from_slice(&1u32.to_le_bytes());
        out.extend_from_slice(&1u32.to_le_bytes());
        out.extend_from_slice(&str_len.to_le_bytes());
        out.extend_from_slice(&128u32.to_le_bytes());
        out.extend_from_slice(&128u32.to_le_bytes());
        out.extend_from_slice(&0u32.to_le_bytes());
        out.extend_from_slice(&0u32.to_le_bytes());
        let mut slot = vec![0u8; str_len as usize];
        let nb = tex_name.as_bytes();
        slot[..nb.len()].copy_from_slice(nb);
        out.extend_from_slice(&slot);
        out.extend_from_slice(b"\x00piz");
        out.extend_from_slice(&0x42af_9021u32.to_le_bytes());
        out.extend_from_slice(&(128u32 * 128u32).to_le_bytes());
        let mut enc = ZlibEncoder::new(Vec::new(), Compression::default());
        enc.write_all(&vec![0u8; 128 * 128]).unwrap();
        out.extend_from_slice(&enc.finish().unwrap());
        out
    }

    fn build_stored_zip(entries: &[(&str, &[u8])]) -> Vec<u8> {
        let mut out = Vec::new();
        let mut central = Vec::new();
        for (name, data) in entries {
            let offset = out.len() as u32;
            let nb = name.as_bytes();
            out.extend_from_slice(&0x0403_4b50u32.to_le_bytes());
            out.extend_from_slice(&20u16.to_le_bytes());
            out.extend_from_slice(&0u16.to_le_bytes());
            out.extend_from_slice(&0u16.to_le_bytes());
            out.extend_from_slice(&0u16.to_le_bytes());
            out.extend_from_slice(&0u16.to_le_bytes());
            out.extend_from_slice(&0u32.to_le_bytes());
            out.extend_from_slice(&(data.len() as u32).to_le_bytes());
            out.extend_from_slice(&(data.len() as u32).to_le_bytes());
            out.extend_from_slice(&(nb.len() as u16).to_le_bytes());
            out.extend_from_slice(&0u16.to_le_bytes());
            out.extend_from_slice(nb);
            out.extend_from_slice(data);

            central.extend_from_slice(&0x0201_4b50u32.to_le_bytes());
            central.extend_from_slice(&20u16.to_le_bytes());
            central.extend_from_slice(&20u16.to_le_bytes());
            central.extend_from_slice(&0u16.to_le_bytes());
            central.extend_from_slice(&0u16.to_le_bytes());
            central.extend_from_slice(&0u16.to_le_bytes());
            central.extend_from_slice(&0u16.to_le_bytes());
            central.extend_from_slice(&0u32.to_le_bytes());
            central.extend_from_slice(&(data.len() as u32).to_le_bytes());
            central.extend_from_slice(&(data.len() as u32).to_le_bytes());
            central.extend_from_slice(&(nb.len() as u16).to_le_bytes());
            central.extend_from_slice(&0u16.to_le_bytes());
            central.extend_from_slice(&0u16.to_le_bytes());
            central.extend_from_slice(&0u16.to_le_bytes());
            central.extend_from_slice(&0u16.to_le_bytes());
            central.extend_from_slice(&0u32.to_le_bytes());
            central.extend_from_slice(&offset.to_le_bytes());
            central.extend_from_slice(nb);
        }
        let cd_offset = out.len() as u32;
        let cd_size = central.len() as u32;
        out.extend_from_slice(&central);
        out.extend_from_slice(&0x0605_4b50u32.to_le_bytes());
        out.extend_from_slice(&0u16.to_le_bytes());
        out.extend_from_slice(&0u16.to_le_bytes());
        out.extend_from_slice(&(entries.len() as u16).to_le_bytes());
        out.extend_from_slice(&(entries.len() as u16).to_le_bytes());
        out.extend_from_slice(&cd_size.to_le_bytes());
        out.extend_from_slice(&cd_offset.to_le_bytes());
        out.extend_from_slice(&0u16.to_le_bytes());
        out
    }

    fn build_bin_section(entries: &[(&str, &[u8])]) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(&0x42a1_4e65u32.to_le_bytes());
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

    fn build_wall_bsp(half_x: f32, y_min: f32, y_max: f32, half_z: f32) -> Arc<BspTree> {
        let mut bsp = Vec::new();
        bsp.extend_from_slice(b"BSP\0");
        let tris = [
            ([-half_x, y_min, half_z], [half_x, y_min, half_z], [half_x, y_max, half_z]),
            ([-half_x, y_min, half_z], [half_x, y_max, half_z], [-half_x, y_max, half_z]),
            ([-half_x, y_min, -half_z], [-half_x, y_max, -half_z], [half_x, y_max, -half_z]),
            ([-half_x, y_min, -half_z], [half_x, y_max, -half_z], [half_x, y_min, -half_z]),
            ([half_x, y_min, -half_z], [half_x, y_max, -half_z], [half_x, y_max, half_z]),
            ([-half_x, y_min, -half_z], [-half_x, y_min, half_z], [-half_x, y_max, half_z]),
        ];
        bsp.extend_from_slice(&(tris.len() as u32).to_le_bytes());
        bsp.extend_from_slice(&1u32.to_le_bytes());
        bsp.extend_from_slice(&1u32.to_le_bytes());
        for (v0, v1, v2) in tris {
            for v in [v0, v1, v2] {
                for c in v {
                    bsp.extend_from_slice(&c.to_le_bytes());
                }
            }
            bsp.extend_from_slice(&0u32.to_le_bytes());
        }
        bsp.push(0xa0);
        for c in [1.0f32, 0.0, 0.0, 0.0] {
            bsp.extend_from_slice(&c.to_le_bytes());
        }
        bsp.extend_from_slice(&(tris.len() as u16).to_le_bytes());
        for i in 0..tris.len() as u16 {
            bsp.extend_from_slice(&i.to_le_bytes());
        }
        let mats = b"<mats><id>stone</id></mats>";
        Arc::new(BspTree::parse(&bsp, mats).unwrap())
    }

    fn build_synthetic_material_kinds() -> Arc<MaterialKinds> {
        let xml = br#"<material_kinds.xml>
            <kind>
                <id>1</id>
                <desc>firm_stone</desc>
                <ground_strength>firm</ground_strength>
                <terrain>maps/landscape/road.dds</terrain>
            </kind>
            <kind>
                <id>2</id>
                <desc>soft_mud</desc>
                <ground_strength>soft</ground_strength>
                <terrain>maps/landscape/mud.dds</terrain>
            </kind>
        </material_kinds.xml>"#;
        Arc::new(MaterialKinds::parse(xml).unwrap())
    }

    #[test]
    fn terrain_map_seam_continuity_material_and_oob() {
        let bounds = SpaceBounds {
            min_x: -1,
            max_x: 0,
            min_y: 0,
            max_y: 0,
        };
        let chunk_left = TerrainChunk::from_sections(
            &flat_heights_section(12.5),
            &dom_tex_section("maps/landscape/road.dds"),
        )
        .unwrap();
        let chunk_right = TerrainChunk::from_sections(
            &flat_heights_section(12.5),
            &dom_tex_section("maps/landscape/mud.dds"),
        )
        .unwrap();

        let map = TerrainMap::from_chunks(
            bounds,
            [
                (decode_chunk_coords("ffff0000o").unwrap(), chunk_left),
                (decode_chunk_coords("00000000o").unwrap(), chunk_right),
            ],
        )
        .unwrap();

        let (min_xz, max_xz) = map.bounds_meters();
        assert_eq!(min_xz, [-100.0, 0.0]);
        assert_eq!(max_xz, [100.0, 100.0]);

        let h_left = map.sample_height(-0.01, 50.0).unwrap();
        let h_right = map.sample_height(0.01, 50.0).unwrap();
        assert!((h_left - 12.5).abs() < 1e-3);
        assert!((h_left - h_right).abs() < 1e-3);

        let kinds = build_synthetic_material_kinds();
        assert_eq!(
            map.dominant_material_at(-25.0, 50.0, &kinds)
                .unwrap()
                .ground_strength,
            Some(GroundStrength::Firm)
        );
        assert_eq!(
            map.dominant_material_at(25.0, 50.0, &kinds)
                .unwrap()
                .ground_strength,
            Some(GroundStrength::Soft)
        );

        assert!(map.sample_height(-100.1, 50.0).is_none());
        assert!(map.sample_height(100.1, 50.0).is_none());
        assert!(map.sample_height(f32::NAN, 50.0).is_none());
        assert!(map.sample_height_clamped(500.0, 50.0).is_some());
        assert!(map.sample_height_clamped(f32::NAN, 50.0).is_none());
    }

    #[test]
    fn nav_grid_avoids_obstacle_steep_slope_and_deep_water_and_pulls_string() {
        let bounds = SpaceBounds {
            min_x: 0,
            max_x: 0,
            min_y: 0,
            max_y: 0,
        };
        let mut grid = [0i32; HEIGHTMAP_GRID_SIZE * HEIGHTMAP_GRID_SIZE];
        for r in 0..HEIGHTMAP_GRID_SIZE {
            for c in 0..HEIGHTMAP_GRID_SIZE {
                let lx = (c as isize - 2) as f32 * 1.5625;
                let lz = (r as isize - 2) as f32 * 1.5625;
                if (65.0..=98.0).contains(&lx) && (35.0..=65.0).contains(&lz) {
                    let h = (lx - 65.0) * 0.8;
                    grid[r * HEIGHTMAP_GRID_SIZE + c] = (h * 1000.0) as i32;
                }
            }
        }
        let chunk = TerrainChunk::from_sections(
            &heights_section_from_mm(&grid),
            &dom_tex_section("maps/landscape/road.dds"),
        )
        .unwrap();
        let terrain =
            TerrainMap::from_chunks(bounds, [(decode_chunk_coords("00000000o").unwrap(), chunk)])
                .unwrap();

        let bsp = build_wall_bsp(4.5, 0.0, 6.0, 24.0);
        let obs = StaticObstacle::from_bsp(
            "wall.model".into(),
            PathBuf::from("wall.primitives"),
            Mat4::from_translation(Vec3::new(50.0, 0.0, 50.0)),
            bsp,
        )
        .unwrap();

        let water = WaterVolume {
            uid: "pool".into(),
            position: Vec3::new(50.0, 2.0, 11.0),
            orientation: 0.0,
            size: Vec3::new(40.0, 0.0, 22.0),
            depth: 2.0,
        };

        let balance = BalanceConfig::default();
        let nav = NavGrid::build(
            [0.0, 0.0],
            [100.0, 100.0],
            &terrain,
            &[obs],
            &[water],
            balance.max_climbable_slope_cos(),
            balance.max_fordable_water_depth_m,
        );

        assert!(!nav.is_walkable_world(50.0, 50.0), "wall cell must be blocked");
        assert!(!nav.is_walkable_world(50.0, 10.0), "deep water cell must be blocked");
        assert!(!nav.is_walkable_world(80.0, 50.0), "steep cliff cell must be blocked");
        assert!(nav.is_walkable_world(15.0, 85.0));

        let path = nav.find_path(
            Vec3::new(15.0, 0.0, 45.0),
            Vec3::new(85.0, 0.0, 85.0),
            &terrain,
        );
        assert!(path.len() >= 3, "path around obstacle must have turn waypoint(s), got {path:?}");
        for w in &path {
            assert!(nav.is_walkable_world(w.x, w.z), "waypoint {w:?} landed on blocked cell");
        }
        assert!((path.first().unwrap().x - 15.0).abs() < 10.0);
        assert!((path.last().unwrap().x - 85.0).abs() < 10.0);
        assert!((path.last().unwrap().z - 85.0).abs() < 10.0);

        let snapped = nav.find_path(
            Vec3::new(50.0, 0.0, 50.0),
            Vec3::new(15.0, 0.0, 85.0),
            &terrain,
        );
        assert!(snapped.len() >= 2);
    }

    #[test]
    fn spatial_grid_and_raycast_deduplicate_candidates() {
        let bsp = build_wall_bsp(20.0, 0.0, 5.0, 20.0);
        let obs = StaticObstacle::from_bsp(
            "big.model".into(),
            PathBuf::from("big.primitives"),
            Mat4::from_translation(Vec3::new(50.0, 0.0, 50.0)),
            bsp,
        )
        .unwrap();
        let hit = obs
            .raycast(Vec3::new(50.0, 2.5, 10.0), Vec3::Z, 100.0)
            .expect("world obstacle raycast");
        assert!((hit.t - 20.0).abs() < 1e-4);
        assert_eq!(hit.material_name.as_deref(), Some("stone"));

        let foliage = FoliageVolume {
            spt_path: "bush.spt".into(),
            position: Vec3::new(12.0, 0.0, 12.0),
            radius: 4.0,
            height: 5.0,
            density: 0.8,
            world_aabb_min: Vec3::new(8.0, 0.0, 8.0),
            world_aabb_max: Vec3::new(16.0, 5.0, 16.0),
            destructible_ref: None,
        };
        let grid = SpatialGrid2D::build([0.0, 0.0], [100.0, 100.0], &[obs], &[foliage]);
        let obs_hits = grid.query_obstacles_aabb([20.0, 20.0], [80.0, 80.0]);
        assert_eq!(obs_hits, vec![0], "multi-cell obstacle must be deduplicated");
        let seg_hits =
            grid.query_obstacles_segment(Vec3::new(0.0, 2.0, 50.0), Vec3::new(100.0, 2.0, 50.0));
        assert_eq!(seg_hits, vec![0]);
        let fol_hits =
            grid.query_foliage_segment(Vec3::new(0.0, 2.0, 12.0), Vec3::new(30.0, 2.0, 12.0));
        assert_eq!(fol_hits, vec![0]);
    }

    #[test]
    fn map_store_caches_synthetic_space_and_errors_on_unknown_id() {
        let tmp = TempTestDir::new("map_store_synth");
        let res = &tmp.0;
        std::fs::create_dir_all(res.join("scripts/arena_defs")).unwrap();
        std::fs::create_dir_all(res.join("system/data")).unwrap();
        std::fs::create_dir_all(res.join("spaces/test_arena")).unwrap();

        std::fs::write(
            res.join("scripts/arena_defs/_list_.xml"),
            "<_list_.xml><map><id>7</id><name>test_arena</name></map></_list_.xml>",
        )
        .unwrap();
        std::fs::write(
            res.join("scripts/arena_defs/_common_.xml"),
            "<_common_.xml><minPlayersInTeam>1</minPlayersInTeam><maxPlayersInTeam>15</maxPlayersInTeam><roundLength>900</roundLength><arenaStartDelay>30.0</arenaStartDelay><kickAfterFinishWaitTime>60.0</kickAfterFinishWaitTime></_common_.xml>",
        )
        .unwrap();
        std::fs::write(
            res.join("scripts/arena_defs/test_arena.xml"),
            "<test_arena.xml><name>Test</name><description>Test arena</description><geometry>spaces/test_arena</geometry><boundingBox><bottomLeft>0 0</bottomLeft><upperRight>100 100</upperRight></boundingBox></test_arena.xml>",
        )
        .unwrap();
        std::fs::write(
            res.join("system/data/material_kinds.xml"),
            "<material_kinds.xml><kind><id>1</id><desc>firm</desc><ground_strength>firm</ground_strength><terrain>maps/landscape/road.dds</terrain></kind></material_kinds.xml>",
        )
        .unwrap();
        std::fs::write(
            res.join("scripts/destructibles.xml"),
            "<destructibles.xml><defaultLifetimeEffectChance>0.5</defaultLifetimeEffectChance><unitVehicleMass>30</unitVehicleMass><structures/><fragiles/><fallingAtoms/><trees/></destructibles.xml>",
        )
        .unwrap();
        std::fs::write(
            res.join("spaces/test_arena/space.settings"),
            "<space.settings><bounds><minX>0</minX><maxX>0</maxX><minY>0</minY><maxY>0</maxY></bounds></space.settings>",
        )
        .unwrap();
        std::fs::write(
            res.join("spaces/test_arena/00000000o.chunk"),
            "<chunk><terrain/><worldNavmesh/></chunk>",
        )
        .unwrap();
        let h = flat_heights_section(5.0);
        let dt = dom_tex_section("maps/landscape/road.dds");
        let cdata = build_stored_zip(&[
            ("terrain2/heights", &h),
            ("terrain2/dominantTextures", &dt),
        ]);
        std::fs::write(res.join("spaces/test_arena/00000000o.cdata"), cdata).unwrap();

        let store = MapStore::new(res, BalanceConfig::default()).unwrap();
        assert_eq!(store.cached_count(), 0);
        assert!(matches!(store.get_or_load(999), Err(AssetError::NotFound(_))));

        let m1 = store.get_or_load(7).unwrap();
        assert_eq!(store.cached_count(), 1);
        let m2 = store.get_or_load(7).unwrap();
        assert!(Arc::ptr_eq(&m1, &m2));
        assert!((m1.sample_height(50.0, 50.0).unwrap() - 5.0).abs() < 1e-3);
        assert_eq!(
            m1.dominant_material_at(50.0, 50.0)
                .unwrap()
                .ground_strength,
            Some(GroundStrength::Firm)
        );
    }

    #[test]
    fn vehicle_collision_cache_caches_synthetic_hit_testers() {
        let tmp = TempTestDir::new("veh_col_synth");
        let res = &tmp.0;
        std::fs::create_dir_all(res.join("vehicles/test")).unwrap();

        std::fs::write(
            res.join("vehicles/test/Hull.model"),
            "<Hull.model><nodelessVisual>vehicles/test/Hull</nodelessVisual></Hull.model>",
        )
        .unwrap();
        let mut bsp = Vec::new();
        bsp.extend_from_slice(b"BSP\0");
        bsp.extend_from_slice(&1u32.to_le_bytes());
        bsp.extend_from_slice(&1u32.to_le_bytes());
        bsp.extend_from_slice(&1u32.to_le_bytes());
        for v in [[-1.0f32, -1.0, 1.0], [1.0, -1.0, 1.0], [0.0, 1.0, 1.0]] {
            for c in v {
                bsp.extend_from_slice(&c.to_le_bytes());
            }
        }
        bsp.extend_from_slice(&0u32.to_le_bytes());
        bsp.push(0xa0);
        for c in [0.0f32, 0.0, 1.0, 0.0] {
            bsp.extend_from_slice(&c.to_le_bytes());
        }
        bsp.extend_from_slice(&1u16.to_le_bytes());
        bsp.extend_from_slice(&0u16.to_le_bytes());
        let prim = build_bin_section(&[
            ("bsp2", &bsp),
            ("bsp2_materials", b"<mats><id>armor_1</id></mats>"),
        ]);
        std::fs::write(res.join("vehicles/test/Hull.primitives"), prim).unwrap();

        let cache = VehicleCollisionCache::new(res);
        let spec = HitTesterSpec {
            model: "vehicles/test/Hull.model".into(),
            enclosing_box: Some(([-1.0, -1.0, -1.0], [1.0, 1.0, 1.0])),
        };
        let h1 = cache.get_or_load_model(&spec).unwrap();
        let h2 = cache.get_or_load_model(&spec).unwrap();
        assert!(Arc::ptr_eq(&h1.bsp, &h2.bsp));
        let hit = h1
            .raycast(Vec3::new(0.0, 0.0, 3.0), Vec3::NEG_Z, 10.0)
            .expect("synthetic hull hit");
        assert!((hit.t - 2.0).abs() < 1e-4);
        assert_eq!(h1.bsp.material_name(&hit), Some("armor_1"));
    }

    #[test]
    fn retail_01_karelia_destructible_indexes_match_client_engine_slots() {
        let Some(res_dir) = retail_res_dir() else {
            return;
        };
        let store = MapStore::new(&res_dir, BalanceConfig::default()).unwrap();
        let karelia = store.get_or_load(1).unwrap();
        // Chunk fffdfffdo (id 31868) as numbered by the live 0.6.5 client engine
        // (`wg_getDestructibleFilename` / `wg_getDestructibleMatrix`): slots 0..=5 are
        // stones outside destructibles.xml, the destructible speedtrees follow.
        let engine_slots: [(usize, &str, f32, f32); 22] = [
            (6, "Bush2_1", 96.71, 52.89),
            (7, "Bush2_1", 46.54, 90.34),
            (8, "Bush2_1", 72.09, 50.78),
            (9, "Bush2_1", 86.87, 36.75),
            (10, "ShrubSouth_cluster", 70.55, 55.04),
            (11, "BirchFall", 67.28, 52.45),
            (12, "BirchFall", 90.63, 58.11),
            (13, "BirchFall", 10.20, 95.30),
            (14, "BirchFall", 27.60, 98.90),
            (15, "BirchFall", 84.80, 39.70),
            (16, "BirchFall", 92.57, 55.98),
            (17, "BirchFall", 74.53, 15.64),
            (18, "BirchFall", 77.14, 16.63),
            (19, "BirchFall_3", 84.94, 28.57),
            (20, "BirchFall_3", 74.62, 49.14),
            (21, "Fir_1", 65.70, 9.10),
            (22, "Fir_2", 61.00, 14.00),
            (23, "Fir_2", 72.22, 18.96),
            (24, "Spruce", 4.62, 67.91),
            (25, "Spruce", 77.28, 19.27),
            (26, "Spruce", 8.26, 82.42),
            (27, "Spruce", 5.00, 78.33),
        ];
        let mut server: Vec<(usize, String, f32, f32)> = karelia.destructibles_by_chunk[&31868]
            .iter()
            .map(|d| {
                let local = d.position - Vec3::new(-300.0, 0.0, -300.0);
                (d.index_in_chunk, d.filename.clone(), local.x, local.z)
            })
            .collect();
        server.sort_by_key(|entry| entry.0);
        assert_eq!(server.len(), engine_slots.len());
        for ((slot, file, x, z), (want_slot, want_tree, want_x, want_z)) in
            server.iter().zip(engine_slots)
        {
            assert_eq!(
                (*slot, file.as_str()),
                (want_slot, format!("speedtree/01_Karelia/{want_tree}.spt").as_str())
            );
            assert!(
                (x - want_x).abs() < 0.05 && (z - want_z).abs() < 0.05,
                "slot {slot} at ({x}, {z}), engine has ({want_x}, {want_z})"
            );
        }
    }

    #[test]
    fn retail_04_himmelsdorf_map_store_seam_and_navgrid() {
        let Some(res_dir) = retail_res_dir() else {
            return;
        };
        let store = MapStore::new(&res_dir, BalanceConfig::default()).unwrap();
        let himmel = store
            .arenas()
            .by_name("04_himmelsdorf")
            .expect("04_himmelsdorf in retail _list_.xml");
        assert_eq!(himmel.id, 3, "retail 04_himmelsdorf id is 3");

        let map1 = store.get_or_load(himmel.id).unwrap();
        let map2 = store.get_or_load(himmel.id).unwrap();
        assert!(Arc::ptr_eq(&map1, &map2));
        assert!(!map1.obstacles.is_empty());
        assert!(!map1.destructibles_by_chunk.is_empty());
        assert!(!map1.foliage.is_empty());

        let h_a = map1.sample_height(-0.05, 0.0).unwrap();
        let h_b = map1.sample_height(0.05, 0.0).unwrap();
        assert!(
            (h_a - h_b).abs() < 0.5,
            "chunk seam discontinuity across x=0: {h_a} vs {h_b}"
        );
        let h_c = map1.sample_height(-100.05, 50.0).unwrap();
        let h_d = map1.sample_height(-99.95, 50.0).unwrap();
        assert!(
            (h_c - h_d).abs() < 0.5,
            "chunk seam discontinuity across x=-100: {h_c} vs {h_d}"
        );

        let team1 = map1.spawns_for_team(1);
        let team2 = map1.spawns_for_team(2);
        assert!(!team1.is_empty() && !team2.is_empty());
        let path = map1.find_path(team1[0].position, team2[0].position);
        assert!(
            path.len() >= 2,
            "expected valid path between Himmelsdorf spawns, got len {}",
            path.len()
        );
        for wp in &path {
            assert!(
                map1.nav_grid.is_walkable_world(wp.x, wp.z),
                "Himmelsdorf path waypoint {wp:?} is not walkable"
            );
        }
    }

    #[test]
    fn retail_ms1_vehicle_collision_cache_loads_all_parts() {
        let Some(res_dir) = retail_res_dir() else {
            return;
        };
        let game_dir = wot065_data::testutil::game_dir().expect("game_dir present");
        let cat = catalog::load(&game_dir).expect("load retail catalog");
        let ms1_entry = cat.by_name("MS-1").expect("MS-1 in catalog");
        let ms1 = cat
            .vehicle_def_by_id(ms1_entry.nation_id, ms1_entry.type_id)
            .expect("MS-1 VehicleDef");

        let turret_idx = 0;
        let gun_01_idx = ms1.turrets[turret_idx]
            .guns
            .iter()
            .position(|g| {
                g.hit_tester
                    .as_ref()
                    .is_some_and(|h| h.model.ends_with("Gun_01.model"))
            })
            .expect("MS-1 turret has Gun_01.model");

        let cache = VehicleCollisionCache::new(&res_dir);
        let set1 = cache
            .get_or_load_vehicle(ms1, 0, turret_idx, gun_01_idx)
            .expect("load MS-1 Hull/Chassis/Turret_01/Gun_01");
        let set2 = cache
            .get_or_load_by_ids(
                ms1,
                ms1.chassis[0].id,
                ms1.turrets[turret_idx].component.id,
                ms1.turrets[turret_idx].guns[gun_01_idx].component.id,
            )
            .expect("cache hit by component IDs");
        assert!(Arc::ptr_eq(&set1, &set2));
        assert!(set1.hull.model_path.ends_with("Hull.model"));
        assert_eq!(
            set1.chassis.model_path,
            ms1.chassis[0].hit_tester.as_ref().unwrap().model
        );
        assert!(set1.turret.model_path.ends_with("Turret_01.model"));
        assert!(set1.gun.model_path.ends_with("Gun_01.model"));
        assert!(!set1.hull.bsp.triangles().is_empty());
        assert!(!set1.chassis.bsp.triangles().is_empty());
        assert!(!set1.turret.bsp.triangles().is_empty());
        assert!(!set1.gun.bsp.triangles().is_empty());
        let hull_aabb = set1.hull.local_aabb();
        let hull_mid_y = 0.5 * (hull_aabb.0.y + hull_aabb.1.y);
        assert!(set1
            .hull
            .raycast(Vec3::new(0.0, hull_mid_y, 5.0), Vec3::NEG_Z, 10.0)
            .is_some());
    }

    #[test]
    fn arena_map_water_spawns_bases_and_raycast_queries() {
        let bounds = SpaceBounds {
            min_x: 0,
            max_x: 0,
            min_y: 0,
            max_y: 0,
        };
        let chunk = TerrainChunk::from_sections(
            &flat_heights_section(0.0),
            &dom_tex_section("maps/landscape/road.dds"),
        )
        .unwrap();
        let terrain =
            TerrainMap::from_chunks(bounds, [(decode_chunk_coords("00000000o").unwrap(), chunk)])
                .unwrap();
        let obs = StaticObstacle::from_bsp(
            "wall.model".into(),
            PathBuf::from("wall.primitives"),
            Mat4::from_translation(Vec3::new(50.0, 0.0, 50.0)),
            build_wall_bsp(2.0, 0.0, 5.0, 2.0),
        )
        .unwrap();
        let water = WaterVolume {
            uid: "pond".into(),
            position: Vec3::new(20.0, 1.2, 20.0),
            orientation: 0.0,
            size: Vec3::new(10.0, 0.0, 10.0),
            depth: 1.2,
        };
        let def = ArenaDef {
            id: 99,
            name: "synth".into(),
            display_name_key: "synth".into(),
            description_key: "synth".into(),
            geometry: "spaces/synth".into(),
            bounding_box: ([0.0, 0.0], [100.0, 100.0]),
            min_players_in_team: 1,
            max_players_in_team: 15,
            round_length_secs: 900,
            arena_start_delay_secs: 30.0,
            kick_after_finish_wait_secs: 60.0,
            minimap_texture: None,
        };
        let spawns = vec![SpawnPoint {
            team: 1,
            preferable_type: None,
            world_transform: Mat4::from_translation(Vec3::new(10.0, 0.0, 80.0)),
            position: Vec3::new(10.0, 0.0, 80.0),
            yaw: 0.0,
        }];
        let bases = vec![TeamBaseSpec {
            team: 2,
            position: Vec3::new(80.0, 0.0, 80.0),
            radius: 35.0,
            points_per_second: 1.0,
            max_points_per_second: 3.0,
            stop_invaders: false,
        }];
        let map = ArenaMap::from_parts(
            def,
            bounds,
            terrain,
            build_synthetic_material_kinds(),
            vec![obs],
            Vec::new(),
            vec![water],
            spawns,
            bases,
            BTreeMap::new(),
            &BalanceConfig::default(),
        );
        assert!(map.is_in_water(20.0, 20.0));
        assert!((map.water_depth_at(20.0, 20.0).unwrap() - 1.2).abs() < 1e-4);
        assert!(!map.is_in_water(80.0, 80.0));
        assert_eq!(map.query_obstacles_aabb([45.0, 45.0], [55.0, 55.0]).len(), 1);
        assert_eq!(map.spawns_for_team(1).len(), 1);
        assert_eq!(map.base_for_team(2).unwrap().radius, 35.0);
        let (_, hit) = map
            .raycast_obstacles(Vec3::new(50.0, 2.5, 40.0), Vec3::Z, 30.0)
            .expect("arena raycast hits wall");
        assert!((hit.t - 8.0).abs() < 1e-4);
        assert!(map.find_path(Vec3::new(10.0, 0.0, 80.0), Vec3::new(10.0, 0.0, 80.0)).len() == 1);
    }

    #[test]
    fn concurrent_map_store_get_or_load_returns_identical_arc() {
        let tmp = TempTestDir::new("map_store_concurrent");
        let res = &tmp.0;
        std::fs::create_dir_all(res.join("scripts/arena_defs")).unwrap();
        std::fs::create_dir_all(res.join("system/data")).unwrap();
        std::fs::create_dir_all(res.join("spaces/c_arena")).unwrap();
        std::fs::write(
            res.join("scripts/arena_defs/_list_.xml"),
            "<_list_.xml><map><id>11</id><name>c_arena</name></map></_list_.xml>",
        )
        .unwrap();
        std::fs::write(
            res.join("scripts/arena_defs/_common_.xml"),
            "<_common_.xml><minPlayersInTeam>1</minPlayersInTeam><maxPlayersInTeam>15</maxPlayersInTeam><roundLength>900</roundLength><arenaStartDelay>30.0</arenaStartDelay><kickAfterFinishWaitTime>60.0</kickAfterFinishWaitTime></_common_.xml>",
        )
        .unwrap();
        std::fs::write(
            res.join("scripts/arena_defs/c_arena.xml"),
            "<c_arena.xml><name>C</name><description>C</description><geometry>spaces/c_arena</geometry><boundingBox><bottomLeft>0 0</bottomLeft><upperRight>100 100</upperRight></boundingBox></c_arena.xml>",
        )
        .unwrap();
        std::fs::write(
            res.join("system/data/material_kinds.xml"),
            "<material_kinds.xml><kind><id>1</id><desc>firm</desc><ground_strength>firm</ground_strength><terrain>maps/landscape/road.dds</terrain></kind></material_kinds.xml>",
        )
        .unwrap();
        std::fs::write(
            res.join("scripts/destructibles.xml"),
            "<destructibles.xml><defaultLifetimeEffectChance>0.5</defaultLifetimeEffectChance><unitVehicleMass>30</unitVehicleMass><structures/><fragiles/><fallingAtoms/><trees/></destructibles.xml>",
        )
        .unwrap();
        std::fs::write(
            res.join("spaces/c_arena/space.settings"),
            "<space.settings><bounds><minX>0</minX><maxX>0</maxX><minY>0</minY><maxY>0</maxY></bounds></space.settings>",
        )
        .unwrap();
        std::fs::write(
            res.join("spaces/c_arena/00000000o.chunk"),
            "<chunk><terrain/></chunk>",
        )
        .unwrap();
        let h = flat_heights_section(3.0);
        let dt = dom_tex_section("maps/landscape/road.dds");
        let cdata = build_stored_zip(&[
            ("terrain2/heights", &h),
            ("terrain2/dominantTextures", &dt),
        ]);
        std::fs::write(res.join("spaces/c_arena/00000000o.cdata"), cdata).unwrap();

        let store = MapStore::new(res, BalanceConfig::default()).unwrap();
        let maps: Vec<Arc<ArenaMap>> = std::thread::scope(|s| {
            let handles: Vec<_> = (0..6)
                .map(|_| s.spawn(|| store.get_or_load(11).unwrap()))
                .collect();
            handles.into_iter().map(|h| h.join().unwrap()).collect()
        });
        for m in &maps[1..] {
            assert!(Arc::ptr_eq(&maps[0], m));
        }
    }

    #[test]
    fn spatial_grid_segment_does_not_overshoot_endpoint_boundary() {
        let small_bsp = build_wall_bsp(1.0, 0.0, 4.0, 1.0);
        let make_obs = |tag: &str, x: f32, z: f32| {
            StaticObstacle::from_bsp(
                format!("{tag}.model"),
                PathBuf::from(format!("{tag}.primitives")),
                Mat4::from_translation(Vec3::new(x, 0.0, z)),
                Arc::clone(&small_bsp),
            )
            .unwrap()
        };

        // Positive 4x4 grid (cell_size = 25m):
        //   0 -> (1, 1) endpoint cell [25..50, 25..50]
        //   1 -> (0, 0) overshoot cell [0..25, 0..25]
        //   2 -> (0, 1) overshoot cell [0..25, 25..50]
        //   3 -> (3, 0) start cell [75..100, 0..25]
        let obstacles = vec![
            make_obs("end_cell_1_1", 37.5, 37.5),
            make_obs("bad_cell_0_0", 12.5, 12.5),
            make_obs("bad_cell_0_1", 12.5, 37.5),
            make_obs("start_cell_3_0", 87.5, 12.5),
        ];
        let foliage = vec![FoliageVolume {
            spt_path: "bush_1_1.spt".into(),
            position: Vec3::new(37.5, 0.0, 37.5),
            radius: 2.0,
            height: 4.0,
            density: 0.7,
            world_aabb_min: Vec3::new(35.5, 0.0, 35.5),
            world_aabb_max: Vec3::new(39.5, 4.0, 39.5),
            destructible_ref: None,
        }];
        let grid = SpatialGrid2D::build([0.0, 0.0], [100.0, 100.0], &obstacles, &foliage);

        let p0 = Vec3::new(92.0, 0.0, 0.0);
        let p1 = Vec3::new(25.0, 0.0, 25.0);
        let hits = grid.query_obstacles_segment(p0, p1);
        assert!(
            hits.contains(&0),
            "segment [92,0]->[25,25] must visit endpoint cell (1,1); got {hits:?}"
        );
        assert!(hits.contains(&3), "must visit start cell (3,0); got {hits:?}");
        assert!(
            !hits.contains(&1) && !hits.contains(&2),
            "segment [92,0]->[25,25] must not overshoot into col 0; got {hits:?}"
        );

        let rev_hits = grid.query_obstacles_segment(p1, p0);
        assert!(rev_hits.contains(&0) && rev_hits.contains(&3));
        assert!(!rev_hits.contains(&1) && !rev_hits.contains(&2));

        let fol_hits = grid.query_foliage_segment(p0, p1);
        assert_eq!(fol_hits, vec![0]);

        // Also exercise [85,0]->[25,25] and a clipped segment entering at (92, 0).
        let hits_85 =
            grid.query_obstacles_segment(Vec3::new(85.0, 0.0, 0.0), Vec3::new(25.0, 0.0, 25.0));
        assert!(hits_85.contains(&0) && hits_85.contains(&3));
        assert!(!hits_85.contains(&1) && !hits_85.contains(&2));

        let clipped_hits =
            grid.query_obstacles_segment(Vec3::new(159.0, 0.0, -25.0), Vec3::new(25.0, 0.0, 25.0));
        assert!(clipped_hits.contains(&0) && clipped_hits.contains(&3));
        assert!(!clipped_hits.contains(&1) && !clipped_hits.contains(&2));

        // Negative-coordinate origin shifted by (-100, -100).
        let neg_obstacles = vec![
            make_obs("neg_1_1", -62.5, -62.5),
            make_obs("neg_0_0", -87.5, -87.5),
            make_obs("neg_0_1", -87.5, -62.5),
            make_obs("neg_3_0", -12.5, -87.5),
        ];
        let neg_grid = SpatialGrid2D::build([-100.0, -100.0], [0.0, 0.0], &neg_obstacles, &[]);
        let neg_hits = neg_grid
            .query_obstacles_segment(Vec3::new(-8.0, 0.0, -100.0), Vec3::new(-75.0, 0.0, -75.0));
        assert!(neg_hits.contains(&0) && neg_hits.contains(&3));
        assert!(!neg_hits.contains(&1) && !neg_hits.contains(&2));
    }

    #[test]
    fn nav_grid_find_path_world_endpoints_never_clip_blocked_cells() {
        let bounds = SpaceBounds {
            min_x: 0,
            max_x: 1,
            min_y: 0,
            max_y: 0,
        };
        let chunk0 = TerrainChunk::from_sections(
            &flat_heights_section(0.0),
            &dom_tex_section("maps/landscape/road.dds"),
        )
        .unwrap();
        let chunk1 = TerrainChunk::from_sections(
            &flat_heights_section(0.0),
            &dom_tex_section("maps/landscape/road.dds"),
        )
        .unwrap();
        let terrain = TerrainMap::from_chunks(
            bounds,
            [
                (decode_chunk_coords("00000000o").unwrap(), chunk0),
                (decode_chunk_coords("00010000o").unwrap(), chunk1),
            ],
        )
        .unwrap();

        // Wall spanning x=[12..48], z=[12..18] blocks NavGrid cells (1,1)..=(4,1),
        // leaving row 0 ((0,0)..=(5,0)) and (5,1)..=(10,1) walkable.
        let wall_bsp = build_wall_bsp(18.0, 0.0, 5.0, 3.0);
        let wall = StaticObstacle::from_bsp(
            "corridor_wall.model".into(),
            PathBuf::from("corridor_wall.primitives"),
            Mat4::from_translation(Vec3::new(30.0, 0.0, 15.0)),
            wall_bsp,
        )
        .unwrap();

        let balance = BalanceConfig::default();
        let nav = NavGrid::build(
            [0.0, 0.0],
            [110.0, 30.0],
            &terrain,
            &[wall],
            &[],
            balance.max_climbable_slope_cos(),
            balance.max_fordable_water_depth_m,
        );

        for c in 0..=5 {
            assert!(nav.is_walkable_cell(c, 0), "cell ({c}, 0) must be walkable");
        }
        for c in 1..=4 {
            assert!(!nav.is_walkable_cell(c, 1), "cell ({c}, 1) must be blocked");
        }
        for c in 5..=10 {
            assert!(nav.is_walkable_cell(c, 1), "cell ({c}, 1) must be walkable");
        }

        let from = Vec3::new(9.9, 0.0, 9.9);
        let to = Vec3::new(105.0, 0.0, 15.0);
        let path = nav.find_path(from, to, &terrain);
        assert!(path.len() >= 2, "expected non-empty route, got {path:?}");
        assert!((path.first().unwrap().x - from.x).abs() < 1e-4);
        assert!((path.first().unwrap().z - from.z).abs() < 1e-4);
        assert!((path.last().unwrap().x - to.x).abs() < 1e-4);
        assert!((path.last().unwrap().z - to.z).abs() < 1e-4);

        for seg in path.windows(2) {
            for step in 0..=200 {
                let t = step as f32 / 200.0;
                let x = seg[0].x + (seg[1].x - seg[0].x) * t;
                let z = seg[0].z + (seg[1].z - seg[0].z) * t;
                assert!(
                    nav.is_walkable_world(x, z),
                    "path segment {:?} -> {:?} intersects blocked point ({x}, {z}) in path {path:?}",
                    seg[0],
                    seg[1]
                );
            }
        }
    }

    #[test]
    fn raycast_obstacles_handles_infinite_max_dist_and_non_finite_inputs() {
        let bounds = SpaceBounds {
            min_x: 0,
            max_x: 0,
            min_y: 0,
            max_y: 0,
        };
        let chunk = TerrainChunk::from_sections(
            &flat_heights_section(0.0),
            &dom_tex_section("maps/landscape/road.dds"),
        )
        .unwrap();
        let terrain =
            TerrainMap::from_chunks(bounds, [(decode_chunk_coords("00000000o").unwrap(), chunk)])
                .unwrap();
        let obs = StaticObstacle::from_bsp(
            "wall.model".into(),
            PathBuf::from("wall.primitives"),
            Mat4::from_translation(Vec3::new(50.0, 0.0, 50.0)),
            build_wall_bsp(2.0, 0.0, 5.0, 2.0),
        )
        .unwrap();
        let obs_roof = StaticObstacle::from_bsp(
            "roof.model".into(),
            PathBuf::from("roof.primitives"),
            Mat4::from_translation(Vec3::new(20.0, 10.0, 20.0))
                * Mat4::from_rotation_x(std::f32::consts::FRAC_PI_2),
            build_wall_bsp(2.0, -2.0, 2.0, 1.0),
        )
        .unwrap();
        let def = ArenaDef {
            id: 99,
            name: "synth".into(),
            display_name_key: "synth".into(),
            description_key: "synth".into(),
            geometry: "spaces/synth".into(),
            bounding_box: ([0.0, 0.0], [100.0, 100.0]),
            min_players_in_team: 1,
            max_players_in_team: 15,
            round_length_secs: 900,
            arena_start_delay_secs: 30.0,
            kick_after_finish_wait_secs: 60.0,
            minimap_texture: None,
        };
        let map = ArenaMap::from_parts(
            def,
            bounds,
            terrain,
            build_synthetic_material_kinds(),
            vec![obs, obs_roof],
            Vec::new(),
            Vec::new(),
            Vec::new(),
            Vec::new(),
            BTreeMap::new(),
            &BalanceConfig::default(),
        );

        // Axis-aligned rays with f32::INFINITY from inside grid bounds.
        let (_, hit_z) = map
            .raycast_obstacles(Vec3::new(50.0, 2.5, 10.0), Vec3::Z, f32::INFINITY)
            .expect("infinite axis-aligned +Z ray must hit obstacle");
        assert!((hit_z.t - 38.0).abs() < 1e-4, "expected t=38.0, got {}", hit_z.t);

        let (_, hit_x) = map
            .raycast_obstacles(Vec3::new(10.0, 2.5, 50.0), Vec3::X, f32::INFINITY)
            .expect("infinite axis-aligned +X ray must hit obstacle");
        assert!((hit_x.t - 38.0).abs() < 1e-4, "expected t=38.0, got {}", hit_x.t);

        // Diagonal ray with f32::INFINITY from inside grid bounds.
        let diag_dir = Vec3::new(1.0, 0.0, 1.0).normalize();
        let (_, hit_diag_in) = map
            .raycast_obstacles(Vec3::new(10.0, 2.5, 10.0), diag_dir, f32::INFINITY)
            .expect("infinite diagonal ray inside grid must hit obstacle");
        assert!(hit_diag_in.t.is_finite() && hit_diag_in.t > 0.0);

        // Axis-aligned and diagonal rays with f32::INFINITY from OUTSIDE grid bounds.
        let (_, hit_out_z) = map
            .raycast_obstacles(Vec3::new(50.0, 2.5, -50.0), Vec3::Z, f32::INFINITY)
            .expect("infinite axis-aligned +Z ray from outside grid must hit obstacle");
        assert!((hit_out_z.t - 98.0).abs() < 1e-4, "expected t=98.0, got {}", hit_out_z.t);

        let (_, hit_out_diag) = map
            .raycast_obstacles(Vec3::new(-50.0, 2.5, -50.0), diag_dir, f32::INFINITY)
            .expect("infinite diagonal ray from outside grid must hit obstacle");
        assert!(hit_out_diag.t > 100.0 && hit_out_diag.t.is_finite());

        // Outside grid pointing away returns None.
        assert!(map
            .raycast_obstacles(Vec3::new(-50.0, 2.5, -50.0), Vec3::NEG_Z, f32::INFINITY)
            .is_none());

        // Vertical rays with f32::INFINITY must not produce 0.0 * INFINITY = NaN.
        assert!(map
            .raycast_obstacles(Vec3::new(50.0, -5.0, 50.0), Vec3::Y, f32::INFINITY)
            .is_none());
        let (_, hit_vert) = map
            .raycast_obstacles(Vec3::new(20.0, 0.0, 20.0), Vec3::Y, f32::INFINITY)
            .expect("infinite vertical +Y ray must hit overhead obstacle");
        assert!((hit_vert.t - 9.0).abs() < 1e-3, "expected t=9.0, got {}", hit_vert.t);

        // Non-finite origin / dir / NaN or negative max_dist safely return None.
        assert!(map
            .raycast_obstacles(Vec3::new(f32::NAN, 2.5, 10.0), Vec3::Z, 50.0)
            .is_none());
        assert!(map
            .raycast_obstacles(Vec3::new(f32::INFINITY, 2.5, 10.0), Vec3::Z, 50.0)
            .is_none());
        assert!(map
            .raycast_obstacles(Vec3::new(50.0, 2.5, 10.0), Vec3::new(f32::NAN, 0.0, 1.0), 50.0)
            .is_none());
        assert!(map
            .raycast_obstacles(Vec3::new(50.0, 2.5, 10.0), Vec3::Z, f32::NAN)
            .is_none());
        assert!(map
            .raycast_obstacles(Vec3::new(50.0, 2.5, 10.0), Vec3::Z, -1.0)
            .is_none());
    }
}
