//! Diagnostic: print every top-level item of a space's outdoor chunks in document order.
//!
//! `cargo run -p wot-battle --example dump_chunk_items -- <res_dir> <space_dir_name> [chunk_stem]`

use std::path::PathBuf;

use wot_battle::assets::chunk::{decode_chunk_coords, parse_bigworld_matrix};
use wot_battle::assets::destructibles::{chunk_id_from_indexes, DestructiblesCatalog};
use wot065_data::xml;

fn main() {
    let mut args = std::env::args().skip(1);
    let res = PathBuf::from(args.next().expect("res dir"));
    let space = args.next().expect("space dir name");
    let only = args.next();
    let catalog = DestructiblesCatalog::load(&res).expect("destructibles.xml");
    let dir = res.join("spaces").join(&space);
    let mut paths: Vec<_> = std::fs::read_dir(&dir)
        .unwrap()
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().and_then(|e| e.to_str()) == Some("chunk"))
        .collect();
    paths.sort();
    for path in paths {
        let name = path.file_name().unwrap().to_str().unwrap().to_string();
        let stem = name.split('.').next().unwrap().to_string();
        if only.as_deref().is_some_and(|o| o != stem) {
            continue;
        }
        let Ok(coords) = decode_chunk_coords(&name) else {
            continue;
        };
        let root = xml::parse_xml_bytes(&std::fs::read(&path).unwrap()).unwrap();
        let id = chunk_id_from_indexes(coords.grid_x, coords.grid_z);
        println!("== {stem} id={id} grid=({}, {})", coords.grid_x, coords.grid_z);
        let mut destr = 0usize;
        for (doc, child) in root.children.iter().enumerate() {
            let target = child
                .child("resource")
                .or_else(|| child.child("spt"))
                .map(|n| n.text().to_string())
                .unwrap_or_default();
            let pos = child
                .child("transform")
                .and_then(|t| parse_bigworld_matrix(t.text()).ok())
                .map(|m| m.w_axis.truncate());
            let kind = catalog.by_filename(&target).map(|d| match d.kind {
                wot_battle::assets::destructibles::DestructibleKind::Tree { .. } => "TREE",
                wot_battle::assets::destructibles::DestructibleKind::FallingAtom { .. } => "FALL",
                wot_battle::assets::destructibles::DestructibleKind::Fragile { .. } => "FRAG",
                wot_battle::assets::destructibles::DestructibleKind::Structure { .. } => "STRU",
            });
            let exact = catalog.by_filename(&target).is_some_and(|d| d.filename == target);
            let tag = match kind {
                Some(k) => {
                    destr += 1;
                    format!("{k}{}", if exact { "" } else { "(lod)" })
                }
                None => "-".into(),
            };
            println!(
                "{doc:3} {:<16} {tag:<10} {:<70} {}",
                child.name,
                target,
                pos.map(|p| format!("({:.2}, {:.2}, {:.2})", p.x, p.y, p.z))
                    .unwrap_or_default()
            );
        }
        println!("   destructible-by-catalog: {destr}");
    }
}
