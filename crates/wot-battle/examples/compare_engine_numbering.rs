//! Diagnostic: check the server's destructible numbering (`ArenaMap::load`) against the client
//! engine's per-chunk listing recorded by the live probe in `destructibles.log`.
//!
//! `cargo run -p wot-battle --example compare_engine_numbering -- <res_dir> <arena_type_id> <destructibles.log>`
//!
//! Every engine slot holding a destructibles.xml item must be a server entry with the same
//! `index_in_chunk`, file and position; every other slot must have no server entry.

use std::collections::BTreeMap;
use std::path::PathBuf;

use glam::Vec3;
use wot_battle::assets::destructibles::{chunk_indexes_from_id, DestructiblesCatalog};
use wot_battle::assets::map_store::MapStore;
use wot_battle::BalanceConfig;

type EngineSlot = (String, [f32; 3]);

fn parse_engine_log(log: &str) -> BTreeMap<u16, Vec<EngineSlot>> {
    // A chunk may load more than once; keep its last non-empty listing.
    let mut engine = BTreeMap::new();
    let mut current: Option<(u16, Vec<EngineSlot>)> = None;
    let mut flush = |current: &mut Option<(u16, Vec<EngineSlot>)>| {
        if let Some((id, list)) = current.take() {
            if !list.is_empty() {
                engine.insert(id, list);
            }
        }
    };
    for line in log.lines() {
        if let Some(rest) = line.strip_prefix("chunk ") {
            flush(&mut current);
            let id = rest.split_whitespace().next().unwrap().parse().unwrap();
            current = Some((id, Vec::new()));
        } else if let Some(rest) = line.strip_prefix("  ") {
            let mut parts = rest.splitn(3, ' ');
            let _slot = parts.next();
            let file = parts.next().unwrap().to_string();
            let nums: Vec<f32> = parts
                .next()
                .unwrap_or("")
                .trim_start_matches('(')
                .split(')')
                .next()
                .unwrap_or("")
                .split(',')
                .filter_map(|s| s.trim().parse().ok())
                .collect();
            let pos = if nums.len() == 3 { [nums[0], nums[1], nums[2]] } else { [f32::NAN; 3] };
            if let Some((_, list)) = current.as_mut() {
                list.push((file, pos));
            }
        } else {
            flush(&mut current);
        }
    }
    flush(&mut current);
    engine
}

fn main() {
    let mut args = std::env::args().skip(1);
    let res = PathBuf::from(args.next().expect("res dir"));
    let arena: i32 = args.next().expect("arena type id").parse().unwrap();
    let engine = parse_engine_log(&std::fs::read_to_string(args.next().expect("log")).unwrap());
    let catalog = DestructiblesCatalog::load(&res).expect("destructibles.xml");
    let store = MapStore::new(&res, BalanceConfig::default()).expect("map store");
    let map = store.get_or_load(arena).expect("arena map");

    let (mut ok_chunks, mut bad_chunks, mut matched_slots) = (0usize, 0usize, 0usize);
    for (chunk, slots) in &engine {
        let (gx, gz) = chunk_indexes_from_id(*chunk);
        let origin = Vec3::new(gx as f32 * 100.0, 0.0, gz as f32 * 100.0);
        let server: BTreeMap<usize, (String, Vec3)> = map
            .destructibles_by_chunk
            .get(chunk)
            .into_iter()
            .flatten()
            .map(|d| (d.index_in_chunk, (d.filename.clone(), d.position - origin)))
            .collect();
        let mut problems = Vec::new();
        for (slot, (file, pos)) in slots.iter().enumerate() {
            let in_catalog = catalog.by_filename(file).is_some();
            match (server.get(&slot), in_catalog) {
                (Some((server_file, local)), true) => {
                    let close = (0..3).all(|k| (local[k] - pos[k]).abs() < 0.05);
                    let same_desc = catalog.by_filename(file).map(|d| &d.filename) == Some(server_file);
                    if close && same_desc {
                        matched_slots += 1;
                    } else {
                        problems.push(format!("slot {slot}: engine {file} {pos:?}, server {server_file} {local:?}"));
                    }
                }
                (None, false) => {}
                (Some((server_file, _)), false) => {
                    problems.push(format!("slot {slot}: engine non-destructible {file}, server {server_file}"))
                }
                (None, true) => problems.push(format!("slot {slot}: engine {file}, no server entry")),
            }
        }
        for slot in server.keys().filter(|&&s| s >= slots.len()) {
            problems.push(format!("server slot {slot} beyond engine count {}", slots.len()));
        }
        if problems.is_empty() {
            ok_chunks += 1;
        } else {
            bad_chunks += 1;
            println!("chunk {chunk}:");
            for p in problems.iter().take(4) {
                println!("   {p}");
            }
        }
    }
    println!(
        "engine chunks {}: match {ok_chunks}, mismatch {bad_chunks}, destructible slots matched {matched_slots}",
        engine.len()
    );
}
