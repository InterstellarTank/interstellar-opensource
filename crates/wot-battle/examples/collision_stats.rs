//! Diagnostic: load time and hull-band collision piece counts of an arena.
//!
//! `cargo run -p wot-battle --example collision_stats -- <res_dir> <arena_type_id>...`

use std::path::PathBuf;
use std::time::Instant;

use wot_battle::assets::map_store::MapStore;
use wot_battle::BalanceConfig;

fn main() {
    let mut args = std::env::args().skip(1);
    let res = PathBuf::from(args.next().expect("res dir"));
    let store = MapStore::new(&res, BalanceConfig::default()).expect("map store");
    for arena in args {
        let id: i32 = arena.parse().unwrap();
        let started = Instant::now();
        let map = store.get_or_load(id).expect("arena map");
        let elapsed = started.elapsed();
        let obstacle_pieces: usize = map.obstacles.iter().map(|o| o.pieces.len()).sum();
        let destructibles: Vec<_> = map.destructibles_by_chunk.values().flatten().collect();
        let destructible_pieces: usize = destructibles.iter().map(|d| d.pieces.len()).sum();
        let without_geometry = destructibles.iter().filter(|d| d.pieces.is_empty()).count();
        println!(
            "{} ({id}): load {:.2?}, obstacles {} with {obstacle_pieces} pieces, destructibles {} with {destructible_pieces} pieces, {without_geometry} without band geometry",
            map.def.name,
            elapsed,
            map.obstacles.len(),
            destructibles.len()
        );
        let all: Vec<_> = map
            .obstacles
            .iter()
            .flat_map(|o| o.pieces.iter())
            .chain(destructibles.iter().flat_map(|d| d.pieces.iter()))
            .collect();
        let segments = all.iter().filter(|p| p.points.len() == 2).count();
        let tiny = all
            .iter()
            .filter(|p| (p.max - p.min).max_element() < 0.05)
            .count();
        let mut per: Vec<(usize, &str)> = map.obstacles.iter().map(|o| (o.pieces.len(), o.model_path.as_str())).collect();
        per.sort_unstable_by(|a, b| b.0.cmp(&a.0));
        println!("   segments {segments}, polygons {}, smaller than 5 cm {tiny}", all.len() - segments);
        for (n, path) in per.iter().take(5) {
            println!("   {n:>6} {path}");
        }
        println!("   BandPiece size {} bytes", std::mem::size_of::<wot_battle::sim::collision::BandPiece>());
    }
}
