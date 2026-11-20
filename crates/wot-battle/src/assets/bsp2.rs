//! BigWorld `bsp2` and `bsp2_materials` parsers with fast segment/ray intersection.

use glam::Vec3;
use wot065_data::xml;

use crate::assets::AssetError;

/// BigWorld BSP2 section magic (`b"BSP\0"`).
pub const BSP2_MAGIC: &[u8; 4] = b"BSP\0";

/// Node flag: node has a front child (`dot(p, n) >= d`).
pub const BSP_FLAG_HAS_FRONT: u8 = 0x02;
/// Node flag: node has a back child (`dot(p, n) <= d`).
pub const BSP_FLAG_HAS_BACK: u8 = 0x04;

/// A single triangle stored in a `bsp2` section.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BspTriangle {
    pub v0: Vec3,
    pub v1: Vec3,
    pub v2: Vec3,
    pub material_index: u32,
    pub normal: Vec3,
}

/// Pre-order node in a BigWorld `bsp2` tree.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BspNode {
    pub flags: u8,
    pub plane_normal: Vec3,
    pub plane_d: f32,
    pub front: Option<u32>,
    pub back: Option<u32>,
    pub tri_start: u32,
    pub tri_len: u32,
}

/// Result of a ray-triangle intersection against a [`BspTree`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BspHit {
    /// Distance along the ray direction `dir` (meters when `dir` is unit length).
    pub t: f32,
    pub point: Vec3,
    pub normal: Vec3,
    pub tri_index: u32,
    pub material_index: u32,
}

/// Parsed BigWorld `bsp2` spatial tree paired with its `bsp2_materials` identifier list.
#[derive(Debug, Clone)]
pub struct BspTree {
    triangles: Vec<BspTriangle>,
    nodes: Vec<BspNode>,
    node_tri_indices: Vec<u32>,
    materials: Vec<String>,
    aabb_min: Vec3,
    aabb_max: Vec3,
}

impl BspTree {
    /// Parse raw `bsp2` and `bsp2_materials` sections from a `.primitives` archive.
    pub fn parse(bsp2_bytes: &[u8], bsp2_mats_bytes: &[u8]) -> Result<Self, AssetError> {
        let materials = parse_materials(bsp2_mats_bytes)?;

        if bsp2_bytes.len() < 16 {
            return Err(AssetError::Bsp2("bsp2 header shorter than 16 bytes".into()));
        }
        if &bsp2_bytes[0..4] != BSP2_MAGIC {
            return Err(AssetError::Bsp2(format!(
                "bad bsp2 magic {:02x?}",
                &bsp2_bytes[0..4]
            )));
        }
        let num_tris = read_u32_le(bsp2_bytes, 4) as usize;
        let num_nodes = read_u32_le(bsp2_bytes, 8) as usize;
        let tris_end = num_tris
            .checked_mul(40)
            .and_then(|n| 16usize.checked_add(n))
            .filter(|&end| end <= bsp2_bytes.len())
            .ok_or_else(|| AssetError::Bsp2("triangle table exceeds bsp2 length".into()))?;

        let mut triangles = Vec::with_capacity(num_tris);
        let mut aabb_min = Vec3::splat(f32::INFINITY);
        let mut aabb_max = Vec3::splat(f32::NEG_INFINITY);

        for i in 0..num_tris {
            let off = 16 + i * 40;
            let v0 = read_vec3_le(bsp2_bytes, off);
            let v1 = read_vec3_le(bsp2_bytes, off + 12);
            let v2 = read_vec3_le(bsp2_bytes, off + 24);
            let material_index = read_u32_le(bsp2_bytes, off + 36);
            let normal = (v1 - v0).cross(v2 - v0).normalize_or_zero();
            for v in [v0, v1, v2] {
                aabb_min = aabb_min.min(v);
                aabb_max = aabb_max.max(v);
            }
            triangles.push(BspTriangle {
                v0,
                v1,
                v2,
                material_index,
                normal,
            });
        }
        if triangles.is_empty() {
            aabb_min = Vec3::ZERO;
            aabb_max = Vec3::ZERO;
        }

        let _min_nodes_end = num_nodes
            .checked_mul(19)
            .and_then(|n| tris_end.checked_add(n))
            .filter(|&end| end <= bsp2_bytes.len())
            .ok_or_else(|| AssetError::Bsp2("node table exceeds bsp2 length".into()))?;

        let mut nodes = Vec::with_capacity(num_nodes);
        let mut node_tri_indices = Vec::new();
        let mut pos = tris_end;
        for node_idx in 0..num_nodes {
            if pos + 19 > bsp2_bytes.len() {
                return Err(AssetError::Bsp2(format!(
                    "truncated node {node_idx}/{num_nodes}"
                )));
            }
            let flags = bsp2_bytes[pos];
            let plane_normal = read_vec3_le(bsp2_bytes, pos + 1);
            let plane_d = read_f32_le(bsp2_bytes, pos + 13);
            let tri_len = u16::from_le_bytes([bsp2_bytes[pos + 17], bsp2_bytes[pos + 18]]) as usize;
            pos += 19;

            let idx_bytes_end = pos
                .checked_add(tri_len * 2)
                .filter(|&end| end <= bsp2_bytes.len())
                .ok_or_else(|| {
                    AssetError::Bsp2(format!("truncated triangle index list at node {node_idx}"))
                })?;
            let tri_start = node_tri_indices.len() as u32;
            for j in 0..tri_len {
                let tri_idx = u16::from_le_bytes([bsp2_bytes[pos + j * 2], bsp2_bytes[pos + j * 2 + 1]])
                    as usize;
                if tri_idx >= num_tris {
                    return Err(AssetError::Bsp2(format!(
                        "node {node_idx} references tri {tri_idx} >= {num_tris}"
                    )));
                }
                node_tri_indices.push(tri_idx as u32);
            }
            pos = idx_bytes_end;

            nodes.push(BspNode {
                flags,
                plane_normal,
                plane_d,
                front: None,
                back: None,
                tri_start,
                tri_len: tri_len as u32,
            });
        }

        if !nodes.is_empty() {
            link_preorder(&mut nodes)?;
        }

        Ok(Self {
            triangles,
            nodes,
            node_tri_indices,
            materials,
            aabb_min,
            aabb_max,
        })
    }

    pub fn triangles(&self) -> &[BspTriangle] {
        &self.triangles
    }

    pub fn nodes(&self) -> &[BspNode] {
        &self.nodes
    }

    pub fn materials(&self) -> &[String] {
        &self.materials
    }

    pub fn node_tri_indices(&self) -> &[u32] {
        &self.node_tri_indices
    }

    pub fn aabb(&self) -> (Vec3, Vec3) {
        (self.aabb_min, self.aabb_max)
    }

    pub fn material_name(&self, hit: &BspHit) -> Option<&str> {
        self.materials.get(hit.material_index as usize).map(String::as_str)
    }

    /// Cast a ray (`dir` should be normalized; returns earliest hit with `0 <= t <= max_dist`).
    pub fn raycast(&self, origin: Vec3, dir: Vec3, max_dist: f32) -> Option<BspHit> {
        if self.nodes.is_empty()
            || !origin.is_finite()
            || !dir.is_finite()
            || dir.length_squared() < 1e-12
            || !(max_dist >= 0.0)
        {
            return None;
        }
        let mut best: Option<BspHit> = None;
        let mut best_t = max_dist;
        let mut stack = vec![(0u32, 0.0f32, max_dist)];

        while let Some((node_idx, t0, t1)) = stack.pop() {
            if t0 > best_t + 1e-4 {
                continue;
            }
            let node = &self.nodes[node_idx as usize];
            let start = node.tri_start as usize;
            let end = start + node.tri_len as usize;
            for &tri_idx in &self.node_tri_indices[start..end] {
                let tri = &self.triangles[tri_idx as usize];
                if let Some(t) = intersect_triangle_double_sided(origin, dir, best_t, tri) {
                    best_t = t;
                    best = Some(BspHit {
                        t,
                        point: origin + dir * t,
                        normal: tri.normal,
                        tri_index: tri_idx,
                        material_index: tri.material_index,
                    });
                }
            }
            push_child_intervals(node, origin, dir, t0, t1.min(best_t), &mut stack);
        }

        best
    }

    /// Cast a ray and return all triangle hits in `[0, max_dist]` sorted by `t` ascending.
    pub fn raycast_all(&self, origin: Vec3, dir: Vec3, max_dist: f32) -> Vec<BspHit> {
        if self.nodes.is_empty()
            || !origin.is_finite()
            || !dir.is_finite()
            || dir.length_squared() < 1e-12
            || !(max_dist >= 0.0)
        {
            return Vec::new();
        }
        let mut raw_hits: Vec<BspHit> = Vec::new();
        let mut stack = vec![(0u32, 0.0f32, max_dist)];

        while let Some((node_idx, t0, t1)) = stack.pop() {
            let node = &self.nodes[node_idx as usize];
            let start = node.tri_start as usize;
            let end = start + node.tri_len as usize;
            for &tri_idx in &self.node_tri_indices[start..end] {
                let tri = &self.triangles[tri_idx as usize];
                if let Some(t) = intersect_triangle_double_sided(origin, dir, max_dist, tri) {
                    raw_hits.push(BspHit {
                        t,
                        point: origin + dir * t,
                        normal: tri.normal,
                        tri_index: tri_idx,
                        material_index: tri.material_index,
                    });
                }
            }
            push_child_intervals(node, origin, dir, t0, t1, &mut stack);
        }

        // Deduplicate by tri_index keeping earliest t, then sort by distance ascending.
        raw_hits.sort_by(|a, b| {
            a.tri_index
                .cmp(&b.tri_index)
                .then_with(|| a.t.total_cmp(&b.t))
        });
        raw_hits.dedup_by_key(|h| h.tri_index);
        raw_hits.sort_by(|a, b| a.t.total_cmp(&b.t).then_with(|| a.tri_index.cmp(&b.tri_index)));
        raw_hits
    }
}

enum ChildSlot {
    Root,
    Front(usize),
    Back(usize),
}

fn link_preorder(nodes: &mut [BspNode]) -> Result<(), AssetError> {
    let mut cursor = 0usize;
    let mut stack = Vec::new();
    stack.push(ChildSlot::Root);

    while let Some(slot) = stack.pop() {
        let idx = cursor;
        if idx >= nodes.len() {
            return Err(AssetError::Bsp2("pre-order tree overflowed node list".into()));
        }
        cursor += 1;
        match slot {
            ChildSlot::Root => {}
            ChildSlot::Front(parent) => nodes[parent].front = Some(idx as u32),
            ChildSlot::Back(parent) => nodes[parent].back = Some(idx as u32),
        }

        let flags = nodes[idx].flags;
        // Push Back first so Front is popped and assigned first (pre-order).
        if flags & BSP_FLAG_HAS_BACK != 0 {
            stack.push(ChildSlot::Back(idx));
        }
        if flags & BSP_FLAG_HAS_FRONT != 0 {
            stack.push(ChildSlot::Front(idx));
        }
    }

    if cursor != nodes.len() {
        return Err(AssetError::Bsp2(format!(
            "pre-order tree consumed {cursor} nodes, expected {}",
            nodes.len()
        )));
    }
    Ok(())
}

fn push_child_intervals(
    node: &BspNode,
    origin: Vec3,
    dir: Vec3,
    t0: f32,
    t1: f32,
    stack: &mut Vec<(u32, f32, f32)>,
) {
    if node.front.is_none() && node.back.is_none() {
        return;
    }
    const EPS: f32 = 1e-4;
    let dist = node.plane_normal.dot(origin) - node.plane_d;
    let denom = node.plane_normal.dot(dir);

    let (near, far) = if dist + denom * t0 >= 0.0 {
        (node.front, node.back)
    } else {
        (node.back, node.front)
    };

    if denom.abs() < 1e-7 {
        if dist > EPS {
            if let Some(f) = node.front {
                stack.push((f, t0, t1));
            }
        } else if dist < -EPS {
            if let Some(b) = node.back {
                stack.push((b, t0, t1));
            }
        } else {
            if let Some(b) = far {
                stack.push((b, t0, t1));
            }
            if let Some(a) = near {
                stack.push((a, t0, t1));
            }
        }
        return;
    }

    let t_split = -dist / denom;
    if t_split >= t0 - EPS && t_split <= t1 + EPS {
        // Push far first so near is popped first (DFS front-to-back).
        if let Some(b) = far {
            stack.push((b, (t_split - EPS).max(t0), t1));
        }
        if let Some(a) = near {
            stack.push((a, t0, (t_split + EPS).min(t1)));
        }
    } else {
        let t_mid = 0.5 * (t0 + t1);
        let child = if dist + denom * t_mid >= 0.0 {
            node.front
        } else {
            node.back
        };
        if let Some(c) = child {
            stack.push((c, t0, t1));
        }
    }
}

fn intersect_triangle_double_sided(
    origin: Vec3,
    dir: Vec3,
    max_dist: f32,
    tri: &BspTriangle,
) -> Option<f32> {
    let e1 = tri.v1 - tri.v0;
    let e2 = tri.v2 - tri.v0;
    let pvec = dir.cross(e2);
    let det = e1.dot(pvec);
    if det.abs() < 1e-8 {
        return None;
    }
    let inv_det = 1.0 / det;
    let tvec = origin - tri.v0;
    let u = tvec.dot(pvec) * inv_det;
    if !(-1e-6..=1.0 + 1e-6).contains(&u) {
        return None;
    }
    let qvec = tvec.cross(e1);
    let v = dir.dot(qvec) * inv_det;
    if v < -1e-6 || u + v > 1.0 + 1e-6 {
        return None;
    }
    let t = e2.dot(qvec) * inv_det;
    (t >= 0.0 && t <= max_dist).then_some(t)
}

fn parse_materials(bytes: &[u8]) -> Result<Vec<String>, AssetError> {
    let before_nul = match bytes.iter().position(|&b| b == 0) {
        Some(idx) if !bytes.starts_with(b"\x45\x4e\xa1\x62") => &bytes[..idx],
        _ => bytes,
    };
    let trimmed = trim_ascii_ws(before_nul);
    if trimmed.is_empty() {
        return Ok(Vec::new());
    }
    let root = xml::parse_xml_bytes(trimmed)?;
    Ok(root
        .children_named("id")
        .map(|n| n.text().to_string())
        .collect())
}

fn trim_ascii_ws(mut s: &[u8]) -> &[u8] {
    while let Some(&b) = s.first() {
        if b.is_ascii_whitespace() {
            s = &s[1..];
        } else {
            break;
        }
    }
    while let Some(&b) = s.last() {
        if b.is_ascii_whitespace() {
            s = &s[..s.len() - 1];
        } else {
            break;
        }
    }
    s
}

fn read_u32_le(bytes: &[u8], pos: usize) -> u32 {
    u32::from_le_bytes([bytes[pos], bytes[pos + 1], bytes[pos + 2], bytes[pos + 3]])
}

fn read_f32_le(bytes: &[u8], pos: usize) -> f32 {
    f32::from_le_bytes([bytes[pos], bytes[pos + 1], bytes[pos + 2], bytes[pos + 3]])
}

fn read_vec3_le(bytes: &[u8], pos: usize) -> Vec3 {
    Vec3::new(
        read_f32_le(bytes, pos),
        read_f32_le(bytes, pos + 4),
        read_f32_le(bytes, pos + 8),
    )
}

#[cfg(test)]
/// Closed box BSP `[-half_x, half_x] x [y_min, y_max] x [-half_z, half_z]`.
pub(crate) fn test_box(half_x: f32, y_min: f32, y_max: f32, half_z: f32) -> BspTree {
    let corner = |x: f32, y: f32, z: f32| [x * half_x, if y < 0.0 { y_min } else { y_max }, z * half_z];
    let quads = [
        [(-1.0, -1.0, 1.0), (1.0, -1.0, 1.0), (1.0, 1.0, 1.0), (-1.0, 1.0, 1.0)],
        [(1.0, -1.0, -1.0), (-1.0, -1.0, -1.0), (-1.0, 1.0, -1.0), (1.0, 1.0, -1.0)],
        [(1.0, -1.0, 1.0), (1.0, -1.0, -1.0), (1.0, 1.0, -1.0), (1.0, 1.0, 1.0)],
        [(-1.0, -1.0, -1.0), (-1.0, -1.0, 1.0), (-1.0, 1.0, 1.0), (-1.0, 1.0, -1.0)],
    ];
    let mut tris = Vec::new();
    for q in quads {
        let v = q.map(|(x, y, z)| corner(x, y, z));
        tris.push((v[0], v[1], v[2]));
        tris.push((v[0], v[2], v[3]));
    }
    let mut bsp = Vec::new();
    bsp.extend_from_slice(b"BSP\0");
    bsp.extend_from_slice(&(tris.len() as u32).to_le_bytes());
    bsp.extend_from_slice(&1u32.to_le_bytes());
    bsp.extend_from_slice(&1u32.to_le_bytes());
    let count = tris.len() as u16;
    for (v0, v1, v2) in tris {
        for v in [v0, v1, v2] {
            for c in v {
                bsp.extend_from_slice(&c.to_le_bytes());
            }
        }
        bsp.extend_from_slice(&0u32.to_le_bytes());
    }
    // One leaf node holding every triangle, so raycasts see them all.
    bsp.push(0xa0);
    for c in [1.0f32, 0.0, 0.0, 0.0] {
        bsp.extend_from_slice(&c.to_le_bytes());
    }
    bsp.extend_from_slice(&count.to_le_bytes());
    for index in 0..count {
        bsp.extend_from_slice(&index.to_le_bytes());
    }
    BspTree::parse(&bsp, &[]).expect("parse synthetic box BSP")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn push_tri(buf: &mut Vec<u8>, v0: [f32; 3], v1: [f32; 3], v2: [f32; 3], mat: u32) {
        for v in [v0, v1, v2] {
            for c in v {
                buf.extend_from_slice(&c.to_le_bytes());
            }
        }
        buf.extend_from_slice(&mat.to_le_bytes());
    }

    fn push_node(buf: &mut Vec<u8>, flags: u8, n: [f32; 3], d: f32, idxs: &[u16]) {
        buf.push(flags);
        for c in n {
            buf.extend_from_slice(&c.to_le_bytes());
        }
        buf.extend_from_slice(&d.to_le_bytes());
        buf.extend_from_slice(&(idxs.len() as u16).to_le_bytes());
        for &i in idxs {
            buf.extend_from_slice(&i.to_le_bytes());
        }
    }

    #[test]
    fn synthetic_two_leaf_bsp_raycast_and_materials() {
        let mut bsp = Vec::new();
        bsp.extend_from_slice(BSP2_MAGIC);
        bsp.extend_from_slice(&2u32.to_le_bytes());
        bsp.extend_from_slice(&3u32.to_le_bytes());
        bsp.extend_from_slice(&1u32.to_le_bytes());
        push_tri(&mut bsp, [2.0, -1.0, -1.0], [2.0, 1.0, -1.0], [2.0, 0.0, 1.0], 0);
        push_tri(&mut bsp, [-2.0, -1.0, -1.0], [-2.0, 0.0, 1.0], [-2.0, 1.0, -1.0], 1);
        push_node(&mut bsp, 0xa7, [1.0, 0.0, 0.0], 0.0, &[]);
        push_node(&mut bsp, 0xa0, [1.0, 0.0, 0.0], 0.0, &[0]);
        push_node(&mut bsp, 0xa0, [1.0, 0.0, 0.0], 0.0, &[1]);

        let mats = b"<temp_bsp_materials.xml><id>armor_1</id><id>fuelTank</id></temp_bsp_materials.xml>";
        let tree = BspTree::parse(&bsp, mats).unwrap();
        assert_eq!(tree.triangles().len(), 2);
        assert_eq!(tree.nodes().len(), 3);
        assert_eq!(tree.materials(), &["armor_1", "fuelTank"]);

        let hits = tree.raycast_all(Vec3::new(5.0, 0.0, 0.0), Vec3::NEG_X, 20.0);
        assert_eq!(hits.len(), 2);
        assert!((hits[0].t - 3.0).abs() < 1e-4);
        assert_eq!(tree.material_name(&hits[0]), Some("armor_1"));
        assert!((hits[1].t - 7.0).abs() < 1e-4);
        assert_eq!(tree.material_name(&hits[1]), Some("fuelTank"));

        let first = tree
            .raycast(Vec3::new(5.0, 0.0, 0.0), Vec3::NEG_X, 5.0)
            .unwrap();
        assert!((first.t - 3.0).abs() < 1e-4);
        assert!(tree
            .raycast(Vec3::new(5.0, 0.0, 0.0), Vec3::NEG_X, 2.0)
            .is_none());
    }

    #[test]
    fn trailing_null_materials_tolerated() {
        let mut bsp = Vec::new();
        bsp.extend_from_slice(BSP2_MAGIC);
        bsp.extend_from_slice(&1u32.to_le_bytes());
        bsp.extend_from_slice(&1u32.to_le_bytes());
        bsp.extend_from_slice(&1u32.to_le_bytes());
        push_tri(&mut bsp, [0.0, 0.0, 0.0], [1.0, 0.0, 0.0], [0.0, 0.0, 1.0], 0);
        push_node(&mut bsp, 0xa0, [1.0, 0.0, 0.0], 0.0, &[0]);

        let mats = b"<temp_bsp_materials.xml><id>armor_1</id></temp_bsp_materials.xml>\0\0garbage";
        let tree = BspTree::parse(&bsp, mats).unwrap();
        assert_eq!(tree.materials(), &["armor_1"]);
    }

    #[test]
    fn rejects_oversized_num_nodes_without_oom() {
        let mut bsp = Vec::new();
        bsp.extend_from_slice(BSP2_MAGIC);
        bsp.extend_from_slice(&0u32.to_le_bytes());
        bsp.extend_from_slice(&u32::MAX.to_le_bytes());
        bsp.extend_from_slice(&0u32.to_le_bytes());
        let err = BspTree::parse(&bsp, b"").unwrap_err();
        assert!(matches!(err, AssetError::Bsp2(_)), "unexpected error: {err:?}");
    }

    #[test]
    fn parses_deeply_skewed_bsp_without_stack_overflow() {
        let depth = 25_000usize;
        let mut bsp = Vec::new();
        bsp.extend_from_slice(BSP2_MAGIC);
        bsp.extend_from_slice(&1u32.to_le_bytes());
        bsp.extend_from_slice(&(depth as u32).to_le_bytes());
        bsp.extend_from_slice(&1u32.to_le_bytes());
        push_tri(&mut bsp, [2.0, -1.0, -1.0], [2.0, 1.0, -1.0], [2.0, 0.0, 1.0], 0);
        for i in 0..depth {
            if i + 1 < depth {
                push_node(&mut bsp, BSP_FLAG_HAS_FRONT, [1.0, 0.0, 0.0], 0.0, &[]);
            } else {
                push_node(&mut bsp, 0xa0, [1.0, 0.0, 0.0], 0.0, &[0]);
            }
        }
        let mats = b"<temp_bsp_materials.xml><id>armor_1</id></temp_bsp_materials.xml>";
        let tree = BspTree::parse(&bsp, mats).unwrap();
        assert_eq!(tree.nodes().len(), depth);
        let hit = tree
            .raycast(Vec3::new(5.0, 0.0, 0.0), Vec3::NEG_X, 10.0)
            .expect("deep chain leaf triangle should still be hit");
        assert!((hit.t - 3.0).abs() < 1e-4);
    }
}
