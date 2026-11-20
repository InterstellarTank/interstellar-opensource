//! Diagnostic: wall time of battle update ticks in a full arena of bots.
//!
//! `cargo run -p wot-battle --example arena_frame_time -- <game_dir> [arena_type_id] [vehicles per team] [seconds]`

use std::sync::Arc;
use std::time::{Duration, Instant};

use wot065_data::catalog;
use wot065_data::descr::VehicleDescr;
use wot_battle::assets::map_store::{MapStore, VehicleCollisionCache};
use wot_battle::assets::resolve_res_dir;
use wot_battle::lobby::{
    ArenaLaunchSpec, BattleMode, ParticipantSpec, ARENA_BONUS_TYPE_REGULAR, ARENA_GUI_TYPE_RANDOM,
};
use wot_battle::world::ArenaInstance;
use wot_battle::BalanceConfig;

const TICK: Duration = Duration::from_millis(100);

fn main() {
    let mut args = std::env::args().skip(1);
    let game = std::path::PathBuf::from(args.next().expect("game dir"));
    let arena_type_id: i32 = args.next().map_or(3, |a| a.parse().expect("arena type id"));
    let per_team: usize = args.next().map_or(15, |a| a.parse().expect("vehicles per team"));
    let seconds: u64 = args.next().map_or(60, |a| a.parse().expect("seconds"));

    let res = resolve_res_dir(&game).expect("res dir");
    let cat = catalog::load(&game).expect("catalog");
    let store = MapStore::new(&res, BalanceConfig::default()).expect("map store");
    let loading = Instant::now();
    let map = store.get_or_load(arena_type_id).expect("arena map");
    println!("{}: map load {:.2?}", map.def.name, loading.elapsed());

    let defs: Vec<_> = cat
        .type_comp_descrs()
        .into_iter()
        .filter_map(|cd| cat.vehicle_def(cd as i32))
        .collect();
    let participants = (0..per_team * 2)
        .map(|i| ParticipantSpec {
            account_db_id: -(i as i64) - 1,
            veh_inv_id: -(i as i64) - 1,
            player_name: format!("Bot_{i}"),
            veh_comp_descr: VehicleDescr::stock(defs[(i * 7) % defs.len()])
                .expect("stock vehicle")
                .to_bytes(),
            team: if i < per_team { 1 } else { 2 },
            is_bot: true,
            ..Default::default()
        })
        .collect();
    let spec = ArenaLaunchSpec {
        arena_type_id,
        arena_gui_type: ARENA_GUI_TYPE_RANDOM,
        arena_bonus_type: ARENA_BONUS_TYPE_REGULAR,
        mode: BattleMode::Random,
        round_length_secs: 900,
        prebattle_id: 0,
        participants,
    };
    let mut next_id = 100;
    let mut arena = ArenaInstance::spawn(1, map, &spec, &cat, &mut || {
        next_id += 1;
        next_id
    });
    let loading = Instant::now();
    arena.set_collision_cache(Arc::new(VehicleCollisionCache::new(&res)), Some(&cat));
    println!("vehicle hit testers load {:.2?}", loading.elapsed());

    let starts: Vec<_> = arena.vehicles.iter().map(|v| v.position).collect();
    let mut ticks = Vec::new();
    let mut broken = 0;
    let mut rams = 0;
    for _ in 0..seconds * 10 {
        let started = Instant::now();
        let events = arena.advance(TICK);
        ticks.push(started.elapsed());
        broken += events.broken_destructibles.len();
        rams += events.ramming_damages.len();
        if arena.finished.is_some() {
            break;
        }
    }
    let alive = arena.vehicles.iter().filter(|v| v.is_alive).count();
    let travelled: f32 = arena
        .vehicles
        .iter()
        .zip(&starts)
        .map(|(v, start)| v.position.distance(*start))
        .sum();
    println!("mean distance from spawn {:.1} m", travelled / arena.vehicles.len() as f32);
    ticks.sort_unstable();
    let pick = |q: f64| ticks[((ticks.len() - 1) as f64 * q) as usize];
    let total: Duration = ticks.iter().sum();
    println!(
        "{} ticks of {TICK:?} ({} vehicles, {alive} alive at the end): mean {:.2?}, p50 {:.2?}, p95 {:.2?}, p99 {:.2?}, max {:.2?}",
        ticks.len(),
        per_team * 2,
        total / ticks.len() as u32,
        pick(0.5),
        pick(0.95),
        pick(0.99),
        pick(1.0),
    );
    let team_killers = arena.vehicles.iter().filter(|v| v.is_team_killer).count();
    println!("broken destructibles {broken}, ramming hits {rams}, team killers {team_killers}");
}
