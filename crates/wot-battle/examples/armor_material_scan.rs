//! Diagnostic: every vehicle's hit testers load, span a plausible hull footprint, and carry
//! BSP materials that name armor zones of their component.
//!
//! `cargo run -p wot-battle --example armor_material_scan -- <game_dir>`

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

use glam::Vec3;
use wot065_data::catalog;
use wot065_data::descr::VehicleDescr;
use wot_battle::assets::map_store::VehicleCollisionCache;
use wot_battle::assets::resolve_res_dir;
use wot_battle::sim::collision::Footprint;

fn main() {
    let game = PathBuf::from(std::env::args().nth(1).expect("game dir"));
    let res = resolve_res_dir(&game).expect("res dir");
    let cat = catalog::load(&game).expect("catalog");
    let cache = VehicleCollisionCache::new(&res);

    let mut configs = 0;
    let mut load_failures = Vec::new();
    let mut odd_footprints = Vec::new();
    let mut unresolved: BTreeMap<(&str, String), BTreeSet<String>> = BTreeMap::new();
    let mut resolved_hits: BTreeMap<&str, (usize, usize)> = BTreeMap::new();
    let mut footprints = Vec::new();
    let mut first_hits: BTreeMap<&str, (usize, usize)> = BTreeMap::new();
    let mut first_hit_misses: BTreeMap<(&str, String), usize> = BTreeMap::new();

    for type_cd in cat.type_comp_descrs() {
        let def = cat.vehicle_def(type_cd as i32).expect("listed vehicle");
        let stock = VehicleDescr::stock(def).expect("stock descr");
        let mut top = stock.clone();
        let top_turret = def.turrets.last().expect("turret");
        top.chassis_id = def.chassis.last().expect("chassis").id;
        top.turret_id = top_turret.component.id;
        top.gun_id = top_turret.guns.last().expect("gun").component.id;
        for descr in [stock, top] {
            configs += 1;
            let chassis = def.chassis.iter().find(|c| c.id == descr.chassis_id).expect("chassis");
            let turret = def.turrets.iter().find(|t| t.component.id == descr.turret_id).expect("turret");
            let gun = turret.guns.iter().find(|g| g.component.id == descr.gun_id).expect("gun");
            let set = match cache.get_or_load_by_ids(def, descr.chassis_id, descr.turret_id, descr.gun_id) {
                Ok(set) => set,
                Err(err) => {
                    load_failures.push(format!("{} {}: {err}", def.name, chassis.name));
                    continue;
                }
            };
            let hull_position = chassis
                .hull_position
                .map_or(Vec3::ZERO, |p| Vec3::new(p[0] as f32, p[1] as f32, p[2] as f32));
            let footprint = Footprint::from_hit_boxes(set.chassis.local_aabb(), set.hull.local_aabb(), hull_position);
            let half_length = footprint.max.y.max(-footprint.min.y);
            footprints.push((footprint.half_width(), half_length));
            if !(0.6..=2.2).contains(&footprint.half_width()) || !(1.5..=5.5).contains(&half_length) {
                odd_footprints.push(format!("{} {}: {footprint:?}", def.name, chassis.name));
            }
            let parts = [
                ("chassis", &set.chassis, chassis.armor.as_ref()),
                ("hull", &set.hull, def.hull_armor.as_ref()),
                ("turret", &set.turret, turret.armor.as_ref()),
                ("gun", &set.gun, gun.armor.as_ref()),
            ];
            for (component, tester, armor) in parts {
                // Shells from outside: a grid of parallel rays from 26 directions at the box.
                let (low, high) = tester.local_aabb();
                let center = (low + high) * 0.5;
                let reach = (high - low).length();
                for dx in -1..=1 {
                    for dy in -1..=1 {
                        for dz in -1..=1 {
                            let dir = -Vec3::new(dx as f32, dy as f32, dz as f32).normalize_or_zero();
                            if dir == Vec3::ZERO {
                                continue;
                            }
                            let side = dir.any_orthonormal_vector();
                            let up = dir.cross(side);
                            for i in -3..=3 {
                                for j in -3..=3 {
                                    let offset = (side * i as f32 + up * j as f32) * reach / 8.0;
                                    let origin = center - dir * reach + offset;
                                    let Some(hit) = tester.raycast(origin, dir, reach * 2.0) else {
                                        continue;
                                    };
                                    let name = tester
                                        .bsp
                                        .materials()
                                        .get(hit.material_index as usize)
                                        .cloned()
                                        .unwrap_or_default();
                                    let tally = first_hits.entry(component).or_default();
                                    tally.1 += 1;
                                    if armor.is_some_and(|zones| zones.get(&name).is_some()) {
                                        tally.0 += 1;
                                    } else {
                                        *first_hit_misses.entry((component, name)).or_default() += 1;
                                    }
                                }
                            }
                        }
                    }
                }
                let mut per_material: BTreeMap<u32, usize> = BTreeMap::new();
                for triangle in tester.bsp.triangles() {
                    *per_material.entry(triangle.material_index).or_default() += 1;
                }
                for (index, triangles) in per_material {
                    let name = tester
                        .bsp
                        .materials()
                        .get(index as usize)
                        .cloned()
                        .unwrap_or_else(|| format!("#{index}"));
                    let entry = resolved_hits.entry(component).or_default();
                    entry.1 += triangles;
                    if armor.is_some_and(|zones| zones.get(&name).is_some()) {
                        entry.0 += triangles;
                    } else {
                        unresolved.entry((component, name)).or_default().insert(def.name.clone());
                    }
                }
            }
        }
    }

    println!("configurations {configs}, hit testers failing to load {}", load_failures.len());
    for failure in &load_failures {
        println!("  load: {failure}");
    }
    let widths: Vec<f32> = footprints.iter().map(|f| f.0).collect();
    let lengths: Vec<f32> = footprints.iter().map(|f| f.1).collect();
    let range = |v: &[f32]| (v.iter().copied().fold(f32::MAX, f32::min), v.iter().copied().fold(f32::MIN, f32::max));
    println!(
        "footprint half width {:?}, half length {:?}, outside plausible bounds {}",
        range(&widths),
        range(&lengths),
        odd_footprints.len()
    );
    for odd in &odd_footprints {
        println!("  footprint: {odd}");
    }
    for (component, (resolved, total)) in &resolved_hits {
        println!("{component}: {resolved} of {total} triangles name an armor zone");
    }
    for ((component, material), vehicles) in &unresolved {
        let sample: Vec<_> = vehicles.iter().take(4).collect();
        println!("  {component} material {material:?}: {} vehicles, e.g. {sample:?}", vehicles.len());
    }
    println!("shells from outside, first triangle hit:");
    for (component, (resolved, total)) in &first_hits {
        println!(
            "{component}: {resolved} of {total} first hits name an armor zone ({:.2}% do not)",
            100.0 * (total - resolved) as f64 / *total as f64
        );
    }
    for ((component, material), count) in &first_hit_misses {
        println!("  {component} material {material:?}: {count} first hits");
    }
}
