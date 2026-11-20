//! Hull footprints against level geometry cut to the hull height band.
//!
//! A hull is an oriented rectangle on the ground plane. A collision BSP blocks it where one
//! of its triangles passes through the band from [`BAND_LOW`] to [`BAND_HIGH`] above the
//! terrain under that triangle.

use glam::{Mat4, Vec2, Vec3};

use crate::assets::bsp2::BspTree;

/// Geometry lower than this above the ground passes under the hull.
pub const BAND_LOW: f32 = 0.12;
/// Geometry higher than this above the ground passes over the hull.
pub const BAND_HIGH: f32 = 2.5;

/// Hull rectangle in vehicle space: `x` to the right, `y` forward (the vehicle's `z`).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Footprint {
    pub min: Vec2,
    pub max: Vec2,
}

impl Footprint {
    pub fn centered(half_width: f32, half_length: f32) -> Self {
        Self {
            min: Vec2::new(-half_width, -half_length),
            max: Vec2::new(half_width, half_length),
        }
    }

    /// Ground-plane union of the chassis and hull hit tester boxes, the hull placed at
    /// `hull_position` on the chassis.
    pub fn from_hit_boxes(chassis: (Vec3, Vec3), hull: (Vec3, Vec3), hull_position: Vec3) -> Self {
        let hull_min = hull.0 + hull_position;
        let hull_max = hull.1 + hull_position;
        Self {
            min: Vec2::new(chassis.0.x.min(hull_min.x), chassis.0.z.min(hull_min.z)),
            max: Vec2::new(chassis.1.x.max(hull_max.x), chassis.1.z.max(hull_max.z)),
        }
    }

    pub fn half_width(&self) -> f32 {
        (self.max.x - self.min.x) * 0.5
    }

    /// Distance from the vehicle origin to the farthest corner.
    pub fn reach(&self) -> f32 {
        self.min.abs().max(self.max.abs()).length()
    }

    /// The rectangle of a hull at `pos` facing `yaw`.
    pub fn at(&self, pos: Vec2, yaw: f32) -> HullBox {
        let (sin, cos) = yaw.sin_cos();
        let forward = Vec2::new(sin, cos);
        let right = Vec2::new(cos, -sin);
        let local_center = (self.min + self.max) * 0.5;
        HullBox {
            center: pos + right * local_center.x + forward * local_center.y,
            right,
            forward,
            half: (self.max - self.min) * 0.5,
        }
    }
}

/// Oriented hull rectangle on the ground plane.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct HullBox {
    pub center: Vec2,
    pub right: Vec2,
    pub forward: Vec2,
    pub half: Vec2,
}

impl HullBox {
    fn interval(&self, axis: Vec2) -> (f32, f32) {
        let middle = self.center.dot(axis);
        let radius = self.half.x * self.right.dot(axis).abs() + self.half.y * self.forward.dot(axis).abs();
        (middle - radius, middle + radius)
    }

    pub fn corners(&self) -> [Vec2; 4] {
        let r = self.right * self.half.x;
        let f = self.forward * self.half.y;
        [self.center - r - f, self.center + r - f, self.center + r + f, self.center - r + f]
    }

    /// Ground-plane bounds of the rectangle.
    pub fn bounds(&self) -> (Vec2, Vec2) {
        let extent = Vec2::new(
            self.half.x * self.right.x.abs() + self.half.y * self.forward.x.abs(),
            self.half.x * self.right.y.abs() + self.half.y * self.forward.y.abs(),
        );
        (self.center - extent, self.center + extent)
    }
}

/// Convex ground-plane polygon where one collision triangle crosses the hull band.
#[derive(Debug, Clone, PartialEq)]
pub struct BandPiece {
    pub points: Vec<Vec2>,
    /// BSP material index of the triangle.
    pub material: u32,
    pub min: Vec2,
    pub max: Vec2,
}

impl BandPiece {
    pub fn new(points: Vec<Vec2>, material: u32) -> Self {
        let min = points.iter().fold(Vec2::splat(f32::MAX), |m, p| m.min(*p));
        let max = points.iter().fold(Vec2::splat(f32::MIN), |m, p| m.max(*p));
        Self { points, material, min, max }
    }
}

/// Ground-plane tile of [`consolidate`].
pub const BAND_TILE: f32 = 2.0;

/// Merge band pieces into one convex polygon per `tile` square of the ground plane. Detailed
/// models cut to hundreds of pieces; this bounds memory and contact cost, and fills a concave
/// outline only within one tile, so openings wider than a tile stay open.
pub fn consolidate(pieces: &[BandPiece], tile: f32) -> Vec<BandPiece> {
    merge_tiles(pieces.iter().map(|p| (&p.points[..], p.material, p.min, p.max)), tile)
}

fn merge_tiles<'a>(cuts: impl Iterator<Item = (&'a [Vec2], u32, Vec2, Vec2)> + Clone, tile: f32) -> Vec<BandPiece> {
    let Some((origin, extent)) = cuts.clone().fold(None, |acc: Option<(Vec2, Vec2)>, (_, _, lo, hi)| {
        Some(acc.map_or((lo, hi), |(a, b)| (a.min(lo), b.max(hi))))
    }) else {
        return Vec::new();
    };
    let cols = (((extent.x - origin.x) / tile).floor() as usize + 1).max(1);
    let rows = (((extent.y - origin.y) / tile).floor() as usize + 1).max(1);
    let mut tiles: Vec<(Vec<Vec2>, u32)> = vec![(Vec::new(), 0); cols * rows];
    let (mut scratch, mut clipped) = (Vec::with_capacity(16), Vec::with_capacity(16));
    for (points, material, lo, hi) in cuts {
        // Tiles are half-open: an edge on a tile border belongs to the tile below it.
        let from = ((lo - origin) / tile).floor();
        let to = (((hi - origin) / tile).ceil() - Vec2::ONE).max(from);
        for ty in from.y as usize..=(to.y as usize).min(rows - 1) {
            for tx in from.x as usize..=(to.x as usize).min(cols - 1) {
                let tile_lo = origin + Vec2::new(tx as f32, ty as f32) * tile;
                clip_to_rect(points, tile_lo, tile_lo + Vec2::splat(tile), &mut scratch, &mut clipped);
                if clipped.is_empty() {
                    continue;
                }
                let entry = &mut tiles[ty * cols + tx];
                if entry.0.is_empty() {
                    entry.1 = material;
                }
                entry.0.extend_from_slice(&clipped);
            }
        }
    }
    tiles
        .into_iter()
        .filter(|(points, _)| !points.is_empty())
        .filter_map(|(points, material)| {
            let hull = convex_hull(points);
            (hull.len() >= 2).then(|| BandPiece::new(hull, material))
        })
        .collect()
}

/// Clip a convex polygon (or a segment) to the rectangle `[lo, hi]` into `out`.
fn clip_to_rect(points: &[Vec2], lo: Vec2, hi: Vec2, scratch: &mut Vec<Vec2>, out: &mut Vec<Vec2>) {
    out.clear();
    out.extend_from_slice(points);
    let sides: [(Vec2, f32); 4] = [(Vec2::X, lo.x), (Vec2::NEG_X, -hi.x), (Vec2::Y, lo.y), (Vec2::NEG_Y, -hi.y)];
    for (normal, offset) in sides {
        if out.is_empty() {
            return;
        }
        std::mem::swap(scratch, out);
        out.clear();
        let side = |p: Vec2| p.dot(normal) - offset;
        for (i, &current) in scratch.iter().enumerate() {
            let next = scratch[(i + 1) % scratch.len()];
            let (a, b) = (side(current), side(next));
            if a >= 0.0 {
                out.push(current);
            }
            if (a >= 0.0) != (b >= 0.0) {
                out.push(current + (next - current) * (a / (a - b)));
            }
        }
    }
}

/// Andrew's monotone chain; collinear points reduce to the two ends.
fn convex_hull(mut points: Vec<Vec2>) -> Vec<Vec2> {
    points.sort_by(|a, b| a.x.total_cmp(&b.x).then(a.y.total_cmp(&b.y)));
    points.dedup_by(|a, b| a.distance_squared(*b) <= 1e-10);
    if points.len() < 3 {
        return points;
    }
    let cross = |o: Vec2, a: Vec2, b: Vec2| (a - o).perp_dot(b - o);
    let mut hull: Vec<Vec2> = Vec::with_capacity(points.len() + 1);
    for pass in [&points[..], &points.iter().rev().copied().collect::<Vec<_>>()[..]] {
        let start = hull.len();
        for &p in pass {
            while hull.len() >= start + 2 && cross(hull[hull.len() - 2], hull[hull.len() - 1], p) <= 1e-9 {
                hull.pop();
            }
            hull.push(p);
        }
        hull.pop();
    }
    if hull.len() < 2 {
        // Every point on one line: keep its two ends.
        return vec![points[0], points[points.len() - 1]];
    }
    hull
}

/// The part of `triangle` between heights `low` and `high`, projected onto the ground plane.
/// A vertical triangle projects to a segment.
pub fn band_piece(triangle: [Vec3; 3], low: f32, high: f32) -> Option<Vec<Vec2>> {
    let above = clip_half_space(&triangle, |p| p.y - low);
    let inside = clip_half_space(&above, |p| high - p.y);
    if inside.len() < 2 {
        return None;
    }
    let mut points: Vec<Vec2> = Vec::with_capacity(inside.len());
    for p in inside {
        let q = Vec2::new(p.x, p.z);
        if points.last().is_none_or(|last| last.distance_squared(q) > 1e-10) {
            points.push(q);
        }
    }
    if points.len() > 1 && points[0].distance_squared(*points.last().unwrap()) <= 1e-10 {
        points.pop();
    }
    (points.len() >= 2).then_some(points)
}

/// Sutherland–Hodgman clip of a convex polygon to `side(p) >= 0`.
fn clip_half_space(polygon: &[Vec3], side: impl Fn(Vec3) -> f32) -> Vec<Vec3> {
    let mut out = Vec::with_capacity(polygon.len() + 1);
    for (i, &current) in polygon.iter().enumerate() {
        let next = polygon[(i + 1) % polygon.len()];
        let (a, b) = (side(current), side(next));
        if a >= 0.0 {
            out.push(current);
        }
        if (a >= 0.0) != (b >= 0.0) {
            out.push(current + (next - current) * (a / (a - b)));
        }
    }
    out
}

/// Band outline of a collision BSP placed by `transform`: each triangle cut to the band above
/// `ground` under its centroid, the cuts merged into one convex polygon per `tile`.
pub fn band_outline(bsp: &BspTree, transform: Mat4, ground: impl Fn(f32, f32) -> f32, tile: f32) -> Vec<BandPiece> {
    let mut points: Vec<Vec2> = Vec::new();
    let mut cuts: Vec<(usize, usize, u32, Vec2, Vec2)> = Vec::new();
    for tri in bsp.triangles() {
        let world = [tri.v0, tri.v1, tri.v2].map(|v| transform.transform_point3(v));
        let centroid = (world[0] + world[1] + world[2]) / 3.0;
        let base = ground(centroid.x, centroid.z);
        if let Some(cut) = band_piece(world, base + BAND_LOW, base + BAND_HIGH) {
            let lo = cut.iter().fold(Vec2::splat(f32::MAX), |m, p| m.min(*p));
            let hi = cut.iter().fold(Vec2::splat(f32::MIN), |m, p| m.max(*p));
            cuts.push((points.len(), cut.len(), tri.material_index, lo, hi));
            points.extend(cut);
        }
    }
    merge_tiles(
        cuts.iter().map(|&(start, len, material, lo, hi)| (&points[start..start + len], material, lo, hi)),
        tile,
    )
}

fn polygon_interval(points: &[Vec2], axis: Vec2) -> (f32, f32) {
    points.iter().fold((f32::MAX, f32::MIN), |(lo, hi), p| {
        let d = p.dot(axis);
        (lo.min(d), hi.max(d))
    })
}

/// Overlap of `hull` with the convex `points` (two or more): the unit normal that pushes the
/// hull out and the depth along it. Touching is not overlap.
pub fn penetration(hull: &HullBox, points: &[Vec2]) -> Option<(Vec2, f32)> {
    let mut axes = vec![hull.right, hull.forward];
    for (i, &a) in points.iter().enumerate() {
        let edge = points[(i + 1) % points.len()] - a;
        if let Some(normal) = Vec2::new(-edge.y, edge.x).try_normalize() {
            axes.push(normal);
        }
    }
    min_overlap(axes, |axis| hull.interval(axis), |axis| polygon_interval(points, axis))
}

/// Overlap of two hulls: the unit normal that pushes `a` away from `b`, and the depth.
pub fn hull_penetration(a: &HullBox, b: &HullBox) -> Option<(Vec2, f32)> {
    let axes = vec![a.right, a.forward, b.right, b.forward];
    min_overlap(axes, |axis| a.interval(axis), |axis| b.interval(axis))
}

/// Separating-axis test: the shortest push of the first shape out of the second.
fn min_overlap(
    axes: Vec<Vec2>,
    first: impl Fn(Vec2) -> (f32, f32),
    second: impl Fn(Vec2) -> (f32, f32),
) -> Option<(Vec2, f32)> {
    let mut best: Option<(Vec2, f32)> = None;
    for axis in axes {
        let (a_lo, a_hi) = first(axis);
        let (b_lo, b_hi) = second(axis);
        let backward = a_hi - b_lo;
        let onward = b_hi - a_lo;
        if backward <= 0.0 || onward <= 0.0 {
            return None;
        }
        let push = if backward < onward { (-axis, backward) } else { (axis, onward) };
        if best.is_none_or(|(_, depth)| push.1 < depth) {
            best = Some(push);
        }
    }
    best
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::f32::consts::FRAC_PI_2;

    fn close(a: Vec2, b: Vec2) -> bool {
        a.distance(b) < 1e-4
    }

    #[test]
    fn band_piece_keeps_only_geometry_between_clearance_and_hull_top() {
        // Vertical wall triangle in the plane z = 5: at height y it spans x from y/2 to 2.
        let wall = [Vec3::new(0.0, 0.0, 5.0), Vec3::new(2.0, 0.0, 5.0), Vec3::new(2.0, 4.0, 5.0)];
        let piece = band_piece(wall, 0.12, 2.5).expect("the wall crosses the band");
        let xs: Vec<f32> = piece.iter().map(|p| p.x).collect();
        assert!(piece.iter().all(|p| (p.y - 5.0).abs() < 1e-5), "{piece:?}");
        assert!((xs.iter().cloned().fold(f32::MAX, f32::min) - 0.06).abs() < 1e-4, "{piece:?}");
        assert!((xs.iter().cloned().fold(f32::MIN, f32::max) - 2.0).abs() < 1e-4, "{piece:?}");

        let canopy = wall.map(|v| v + Vec3::Y * 3.0);
        assert_eq!(band_piece(canopy, 0.12, 2.5), None);
        let decal = [Vec3::new(0.0, 0.05, 0.0), Vec3::new(1.0, 0.05, 0.0), Vec3::new(0.0, 0.1, 1.0)];
        assert_eq!(band_piece(decal, 0.12, 2.5), None);
    }

    #[test]
    fn hull_overlaps_a_wall_segment_only_past_its_bow() {
        // 2 m wide, 4 m long hull at the origin facing +z: its bow is at z = 2.
        let hull = Footprint::centered(1.0, 2.0).at(Vec2::ZERO, 0.0);
        let inside = [Vec2::new(-5.0, 1.9), Vec2::new(5.0, 1.9)];
        let (normal, depth) = penetration(&hull, &inside).expect("bow past the wall");
        assert!(close(normal, Vec2::new(0.0, -1.0)), "{normal:?}");
        assert!((depth - 0.1).abs() < 1e-4, "{depth}");
        assert_eq!(penetration(&hull, &[Vec2::new(-5.0, 2.1), Vec2::new(5.0, 2.1)]), None);
    }

    #[test]
    fn turned_hull_reaches_with_its_length_along_its_heading() {
        // Facing +x after a quarter turn: 2 m of length reach x = 1.9, the 1 m half width
        // does not reach z = 1.5.
        let hull = Footprint::centered(1.0, 2.0).at(Vec2::ZERO, FRAC_PI_2);
        assert!(penetration(&hull, &[Vec2::new(1.9, -5.0), Vec2::new(1.9, 5.0)]).is_some());
        assert_eq!(penetration(&hull, &[Vec2::new(-5.0, 1.5), Vec2::new(5.0, 1.5)]), None);
    }

    #[test]
    fn hit_boxes_offset_the_footprint_with_the_hull_position() {
        let chassis = (Vec3::new(-1.4, 0.0, -2.8), Vec3::new(1.4, 0.8, 2.6));
        let hull = (Vec3::new(-1.2, -0.3, -3.0), Vec3::new(1.2, 1.0, 2.5));
        let footprint = Footprint::from_hit_boxes(chassis, hull, Vec3::new(0.0, 0.9, 0.4));
        assert_eq!(footprint, Footprint { min: Vec2::new(-1.4, -2.8), max: Vec2::new(1.4, 2.9) });
        // The rectangle centre sits 0.05 m ahead of the origin along the heading.
        assert!(close(footprint.at(Vec2::ZERO, 0.0).center, Vec2::new(0.0, 0.05)));
    }

    fn piece(points: Vec<Vec2>) -> BandPiece {
        BandPiece::new(points, 0)
    }

    #[test]
    fn consolidated_gate_keeps_its_opening_and_pillars() {
        // Two 1 m pillars 4 m apart, each sliced into many small pieces.
        let mut pieces = Vec::new();
        for x0 in [0.0f32, 5.0] {
            for i in 0..10 {
                let y = i as f32 * 0.1;
                pieces.push(piece(vec![
                    Vec2::new(x0, y),
                    Vec2::new(x0 + 1.0, y),
                    Vec2::new(x0 + 1.0, y + 0.1),
                    Vec2::new(x0, y + 0.1),
                ]));
            }
        }
        let merged = consolidate(&pieces, 2.0);
        assert_eq!(merged.len(), 2, "{merged:?}");
        let through_gate = Footprint::centered(1.0, 2.0).at(Vec2::new(3.0, 0.5), 0.0);
        assert!(merged.iter().all(|p| penetration(&through_gate, &p.points).is_none()));
        let on_pillar = Footprint::centered(1.0, 2.0).at(Vec2::new(1.9, 0.5), 0.0);
        assert!(merged.iter().any(|p| penetration(&on_pillar, &p.points).is_some()));
    }

    #[test]
    fn consolidated_wall_segment_splits_at_tiles_without_gaps() {
        let wall: Vec<BandPiece> = (0..5)
            .map(|i| piece(vec![Vec2::new(i as f32, 0.0), Vec2::new(i as f32 + 1.0, 0.0)]))
            .collect();
        let merged = consolidate(&wall, 2.0);
        let mut spans: Vec<(f32, f32)> = merged.iter().map(|p| (p.min.x, p.max.x)).collect();
        spans.sort_by(|a, b| a.0.total_cmp(&b.0));
        assert_eq!(spans, vec![(0.0, 2.0), (2.0, 4.0), (4.0, 5.0)]);
        assert!(merged.iter().all(|p| p.min.y == 0.0 && p.max.y == 0.0));
    }

    #[test]
    fn side_by_side_hulls_separate_across_their_width() {
        let footprint = Footprint::centered(1.5, 3.0);
        let left = footprint.at(Vec2::new(-1.4, 0.0), 0.0);
        let right = footprint.at(Vec2::new(1.4, 0.0), 0.0);
        let (normal, depth) = hull_penetration(&left, &right).expect("0.2 m overlap");
        assert!(close(normal, Vec2::new(-1.0, 0.0)), "{normal:?}");
        assert!((depth - 0.2).abs() < 1e-4, "{depth}");
        assert_eq!(hull_penetration(&footprint.at(Vec2::new(-1.6, 0.0), 0.0), &right), None);
    }
}
