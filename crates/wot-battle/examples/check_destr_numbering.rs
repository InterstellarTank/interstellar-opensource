//! Diagnostic: test candidate per-chunk destructible numberings against client error pairs.
//!
//! `cargo run -p wot-battle --example check_destr_numbering -- <res_dir> "<chunk> <idx>;<chunk> <idx>;..."`
//!
//! Each pair is a `(chunkID, destrIndex)` for which the server sent a fallen tree and the
//! client logged "Destructible descriptor is not available".

use std::collections::BTreeMap;
use std::path::PathBuf;

use wot_battle::assets::chunk::decode_chunk_coords;
use wot_battle::assets::destructibles::{chunk_id_from_indexes, DestructibleKind, DestructiblesCatalog};
use wot065_data::xml;

#[derive(Clone)]
struct Item {
    section: String,
    target: String,
    falls: bool,
    catalog: bool,
}

fn main() {
    let mut args = std::env::args().skip(1);
    let res = PathBuf::from(args.next().expect("res dir"));
    let pairs: Vec<(u16, usize)> = args
        .next()
        .expect("pairs")
        .split(';')
        .filter(|s| !s.trim().is_empty())
        .map(|s| {
            let mut it = s.split_whitespace();
            (it.next().unwrap().parse().unwrap(), it.next().unwrap().parse().unwrap())
        })
        .collect();
    let catalog = DestructiblesCatalog::load(&res).expect("destructibles.xml");
    let mut spaces: Vec<_> = std::fs::read_dir(res.join("spaces"))
        .unwrap()
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.join("space.settings").is_file())
        .collect();
    spaces.sort();
    for dir in spaces {
        let mut chunks: BTreeMap<u16, Vec<Item>> = BTreeMap::new();
        for entry in std::fs::read_dir(&dir).unwrap() {
            let path = entry.unwrap().path();
            let name = path.file_name().unwrap().to_str().unwrap().to_string();
            if !name.ends_with("o.chunk") {
                continue;
            }
            let Ok(coords) = decode_chunk_coords(&name) else {
                continue;
            };
            let root = xml::parse_xml_bytes(&std::fs::read(&path).unwrap()).unwrap();
            let items = root
                .children
                .iter()
                .filter(|c| c.name == "model" || c.name == "speedtree")
                .map(|c| {
                    let target = c
                        .child("resource")
                        .or_else(|| c.child("spt"))
                        .map(|n| n.text().to_string())
                        .unwrap_or_default();
                    let desc = catalog.by_filename(&target);
                    Item {
                        section: c.name.clone(),
                        falls: desc.is_some_and(|d| {
                            matches!(
                                d.kind,
                                DestructibleKind::Tree { .. } | DestructibleKind::FallingAtom { .. }
                            )
                        }),
                        catalog: desc.is_some(),
                        target,
                    }
                })
                .collect();
            chunks.insert(chunk_id_from_indexes(coords.grid_x, coords.grid_z), items);
        }
        if !pairs.iter().all(|(c, _)| chunks.contains_key(c)) {
            continue;
        }
        let order = |items: &[Item], name: &str| -> Vec<Item> {
            let models = items.iter().filter(|i| i.section == "model");
            let trees = items.iter().filter(|i| i.section == "speedtree");
            match name {
                "server" => models
                    .filter(|i| i.catalog)
                    .chain(trees.filter(|i| i.catalog))
                    .cloned()
                    .collect(),
                "models+trees" => models.chain(trees).cloned().collect(),
                "trees+models" => trees.chain(models).cloned().collect(),
                "document" => items.to_vec(),
                "catalog-document" => items.iter().filter(|i| i.catalog).cloned().collect(),
                _ => unreachable!(),
            }
        };
        println!("== {}", dir.file_name().unwrap().to_str().unwrap());
        for candidate in ["server", "models+trees", "trees+models", "document", "catalog-document"] {
            let mut server_tree = 0;
            let mut client_missing = 0;
            for &(chunk, idx) in &pairs {
                let items = &chunks[&chunk];
                if order(items, "server").get(idx).is_some_and(|i| i.falls) {
                    server_tree += 1;
                }
                if order(items, candidate).get(idx).is_none_or(|i| !i.catalog) {
                    client_missing += 1;
                }
            }
            println!(
                "  {candidate:<17} server-sends-tree {server_tree}/{n}  candidate-slot-not-in-catalog {client_missing}/{n}",
                n = pairs.len()
            );
        }
        for &(chunk, idx) in pairs.iter().take(6) {
            let items = &chunks[&chunk];
            let s = order(items, "server");
            let a = order(items, "models+trees");
            println!(
                "    {chunk} #{idx}: server={} | models+trees={}",
                s.get(idx).map(|i| i.target.as_str()).unwrap_or("<none>"),
                a.get(idx).map(|i| i.target.as_str()).unwrap_or("<none>")
            );
        }
    }
}
