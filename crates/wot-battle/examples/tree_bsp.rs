//! Diagnostic: summarize speedtree collision `.bsp2` files.
//!
//! `cargo run -p wot-battle --example tree_bsp -- <file.bsp2>...`
//!
//! Prints the AABB, triangle count, and the horizontal radius of the geometry inside the hull
//! height band (0.12 m .. 2.5 m above the tree origin).

use wot_battle::assets::bsp2::BspTree;

fn main() {
    for path in std::env::args().skip(1) {
        let bytes = std::fs::read(&path).unwrap();
        match BspTree::parse(&bytes, &[]) {
            Ok(tree) => {
                let (min, max) = tree.aabb();
                let mut band_radius = 0.0f32;
                let mut band_tris = 0;
                for tri in tree.triangles() {
                    let verts = [tri.v0, tri.v1, tri.v2];
                    let in_band = verts.iter().any(|v| v.y > 0.12 && v.y < 2.5);
                    if in_band {
                        band_tris += 1;
                        for v in &verts {
                            band_radius = band_radius.max((v.x * v.x + v.z * v.z).sqrt());
                        }
                    }
                }
                println!(
                    "{path}: tris {} aabb min {min:?} max {max:?} band tris {band_tris} band radius {band_radius:.3}",
                    tree.triangles().len()
                );
            }
            Err(err) => println!("{path}: {err}"),
        }
    }
}
