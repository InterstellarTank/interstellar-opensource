//! Diagnostic: client ramming break threshold (`Vehicle._isDestructibleBroken`) per destructible.
//!
//! `cargo run -p wot-battle --example destr_formula -- <res_dir> <mass_kg> <filename>...`
//!
//! Prints health, kineticDamageCorrection and the speed above which a hull of `mass_kg`
//! breaks the item at scale 1.

use std::path::PathBuf;

use wot_battle::assets::destructibles::{DestructibleKind, DestructiblesCatalog};

fn main() {
    let mut args = std::env::args().skip(1);
    let res = PathBuf::from(args.next().expect("res dir"));
    let mass: f32 = args.next().expect("mass").parse().unwrap();
    let catalog = DestructiblesCatalog::load(&res).expect("destructibles.xml");
    println!("unitVehicleMass {}", catalog.unit_vehicle_mass);
    for file in args {
        let Some(desc) = catalog.by_filename(&file) else {
            println!("{file}: not in catalog");
            continue;
        };
        let (health, kdc) = match &desc.kind {
            DestructibleKind::Tree { health, kinetic_damage_correction, .. }
            | DestructibleKind::FallingAtom { health, kinetic_damage_correction, .. }
            | DestructibleKind::Fragile { health, kinetic_damage_correction } => (*health, *kinetic_damage_correction),
            DestructibleKind::Structure { modules, .. } => {
                let min = modules.values().map(|m| m.health).min().unwrap_or(0);
                (min, 0.0)
            }
        };
        let per_v2 = 0.5 * mass * 0.00015 * (mass / catalog.unit_vehicle_mass).powf(kdc);
        let speed = (health as f32 / per_v2).sqrt();
        println!("{file}: health {health} kdc {kdc} breaks above {speed:.2} m/s ({:.1} km/h)", speed * 3.6);
    }
}
