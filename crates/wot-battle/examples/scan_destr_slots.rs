//! Diagnostic: per space, the largest engine destructible slot count per outdoor chunk
//! (`<model>` + `<speedtree>` sections), catalog items past slot 255, and sections that
//! lack a resource.
//!
//! `cargo run -p wot-battle --example scan_destr_slots -- <res_dir>`

use std::path::PathBuf;

use wot_battle::assets::chunk::parse_bigworld_matrix;
use wot_battle::assets::destructibles::{DestructibleKind, DestructiblesCatalog};
use wot065_data::xml;

fn main() {
    let res = PathBuf::from(std::env::args().nth(1).expect("res dir"));
    let catalog = DestructiblesCatalog::load(&res).expect("destructibles.xml");
    let mut spaces: Vec<_> = std::fs::read_dir(res.join("spaces"))
        .unwrap()
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.join("space.settings").is_file())
        .collect();
    spaces.sort();
    for dir in spaces {
        let mut max_slots = (0usize, String::new());
        let mut past_255 = 0usize;
        let mut no_resource = 0usize;
        let mut kinds = [0usize; 4];
        let mut non_uniform = (0usize, 1.0f32, String::new());
        for entry in std::fs::read_dir(&dir).unwrap() {
            let path = entry.unwrap().path();
            let name = path.file_name().unwrap().to_str().unwrap().to_string();
            if !name.ends_with("o.chunk") {
                continue;
            }
            let root = xml::parse_xml_bytes(&std::fs::read(&path).unwrap()).unwrap();
            let mut slot = 0usize;
            for child in &root.children {
                let target = match child.name.as_str() {
                    "model" => child.child("resource"),
                    "speedtree" => child.child("spt"),
                    _ => continue,
                }
                .map(|n| n.text().trim().to_string())
                .unwrap_or_default();
                if target.is_empty() {
                    no_resource += 1;
                }
                if let Some(desc) = catalog.by_filename(&target) {
                    kinds[match desc.kind {
                        DestructibleKind::Fragile { .. } => 0,
                        DestructibleKind::FallingAtom { .. } => 1,
                        DestructibleKind::Tree { .. } => 2,
                        DestructibleKind::Structure { .. } => 3,
                    }] += 1;
                    if let Some(m) = child.child("transform").and_then(|t| parse_bigworld_matrix(t.text()).ok()) {
                        let axes = [m.x_axis.truncate().length(), m.y_axis.truncate().length(), m.z_axis.truncate().length()];
                        let ratio = axes.iter().cloned().fold(0.0f32, f32::max) / axes.iter().cloned().fold(f32::MAX, f32::min).max(1e-6);
                        if ratio > 1.01 {
                            non_uniform.0 += 1;
                            if ratio > non_uniform.1 {
                                non_uniform.1 = ratio;
                                non_uniform.2 = format!("{target} {axes:?}");
                            }
                        }
                    }
                    if slot > 255 {
                        past_255 += 1;
                    }
                }
                slot += 1;
            }
            if slot > max_slots.0 {
                max_slots = (slot, name.clone());
            }
        }
        println!(
            "{:<22} max slots {:>4} ({}) catalog past 255: {past_255}  no resource: {no_resource}  fragile/falling/tree/structure: {kinds:?} non-uniform {} worst {:.2} {}",
            dir.file_name().unwrap().to_str().unwrap(),
            max_slots.0,
            max_slots.1,
            non_uniform.0, non_uniform.1, non_uniform.2
        );
    }
}
