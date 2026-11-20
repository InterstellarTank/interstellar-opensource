//! Diagnostic: list static obstacles whose world AABB comes within `radius` of a point.
//!
//! `cargo run -p wot-battle --example obstacles_near -- <res_dir> <arena_type_id> <x> <z> <radius>`

use std::path::PathBuf;

use wot_battle::assets::map_store::MapStore;
use wot_battle::BalanceConfig;

fn main() {
    let mut args = std::env::args().skip(1);
    let res = PathBuf::from(args.next().expect("res dir"));
    let arena: i32 = args.next().unwrap().parse().unwrap();
    let x: f32 = args.next().unwrap().parse().unwrap();
    let z: f32 = args.next().unwrap().parse().unwrap();
    let radius: f32 = args.next().unwrap().parse().unwrap();
    let store = MapStore::new(&res, BalanceConfig::default()).expect("map store");
    let map = store.get_or_load(arena).expect("arena map");
    if let Some(yaw) = args.next().and_then(|y| y.parse::<f32>().ok()) {
        let (sin, cos) = yaw.sin_cos();
        for step in 0..=12 {
            let d = step as f32 * 0.25;
            let (px, pz) = (x + sin * d, z + cos * d);
            if let Some((h, n)) = map.sample_height_and_normal(px, pz) {
                println!("ahead {d:.2} m: height {h:.3} slope {:.1} deg", n.y.clamp(-1.0, 1.0).acos().to_degrees());
            }
        }
    }
    for o in &map.obstacles {
        let dx = (o.world_aabb_min.x - x).max(0.0).max(x - o.world_aabb_max.x);
        let dz = (o.world_aabb_min.z - z).max(0.0).max(z - o.world_aabb_max.z);
        if dx.hypot(dz) <= radius {
            let centre = o.world_transform.w_axis.truncate();
            println!(
                "{} centre ({:.2}, {:.2}, {:.2}) aabb min ({:.2}, {:.2}, {:.2}) max ({:.2}, {:.2}, {:.2}) gap {:.2}",
                o.model_path,
                centre.x,
                centre.y,
                centre.z,
                o.world_aabb_min.x,
                o.world_aabb_min.y,
                o.world_aabb_min.z,
                o.world_aabb_max.x,
                o.world_aabb_max.y,
                o.world_aabb_max.z,
                dx.hypot(dz)
            );
            // Closest real geometry inside the hull height band above the ground at the point.
            let ground = map.sample_height(x, z).unwrap_or(0.0);
            let (lo, hi) = (ground + 0.12, ground + 2.5);
            let mut best = f32::MAX;
            for tri in o.bsp.triangles() {
                let w = [tri.v0, tri.v1, tri.v2].map(|v| o.world_transform.transform_point3(v));
                // Sample the triangle densely; keep samples inside the band.
                for i in 0..=20 {
                    for j in 0..=(20 - i) {
                        let (a, b) = (i as f32 / 20.0, j as f32 / 20.0);
                        let p = w[0] + (w[1] - w[0]) * a + (w[2] - w[0]) * b;
                        if p.y >= lo && p.y <= hi {
                            best = best.min((p.x - x).hypot(p.z - z));
                        }
                    }
                }
            }
            println!("   nearest band geometry {best:.2} m from the point (band {lo:.2}..{hi:.2})");
            for piece in &o.pieces {
                let c = (piece.min + piece.max) * 0.5;
                let gap_x = (piece.min.x - x).max(0.0).max(x - piece.max.x);
                let gap_z = (piece.min.y - z).max(0.0).max(z - piece.max.y);
                if gap_x.hypot(gap_z) < 3.0 {
                    println!("   piece centre ({:.2}, {:.2}) min ({:.2}, {:.2}) max ({:.2}, {:.2}) gap {:.2} points {:?}", c.x, c.y, piece.min.x, piece.min.y, piece.max.x, piece.max.y, gap_x.hypot(gap_z), piece.points);
                }
            }
        }
    }
}
