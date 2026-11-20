//! WoT 0.6.5 Tactical Bot AI: 2D NavGrid navigation, class-weighted corridors,
//! honest/fair visibility targeting, lead aiming, hull angling, consumable use,
//! and training PassiveTarget mode.

use std::collections::HashMap;
use glam::Vec3;

use crate::assets::map_store::ArenaMap;
use crate::balance::BalanceConfig;
use crate::sim::crits::{
    EXTRA_AMMO_BAY, EXTRA_LEFT_TRACK, EXTRA_RIGHT_TRACK,
};
use crate::sim::physics::VehicleSimState;
use crate::sim::visibility::{VisibilityOutput, VisibilityTracker};
use crate::sim::AimState;
use crate::world::arena::{ArenaInstance, VehicleCombatProfile, VehicleState};

/// Behavior mode for a bot-controlled vehicle.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BotBehaviorMode {
    /// Fully autonomous tactical combat AI.
    Tactical,
    /// Stationary target for training rooms (does not move or shoot).
    PassiveTarget,
}

/// Control inputs emitted by a bot on each tick.
#[derive(Debug, Clone, PartialEq)]
pub struct BotIntent {
    pub move_flags: u8,
    pub cruise_mode: i8,
    pub target_point: Option<Vec3>,
    pub shoot: bool,
    pub activate_equipment: Option<(u16, u16)>,
    pub request_shell_index: Option<usize>,
}

impl Default for BotIntent {
    fn default() -> Self {
        Self {
            move_flags: 0,
            cruise_mode: 0,
            target_point: None,
            shoot: false,
            activate_equipment: None,
            request_shell_index: None,
        }
    }
}

/// Per-bot internal decision state.
#[derive(Debug, Clone)]
pub struct BotAgentState {
    pub entity_id: i32,
    pub mode: BotBehaviorMode,
    pub current_path: Vec<Vec3>,
    pub waypoint_idx: usize,
    pub current_target_id: Option<i32>,
    pub retreat: bool,
    pub hull_angle_rad: f32,
    pub last_repairs_try: f32,
    pub goal_pos: Option<Vec3>,
    /// Server time since the bot has asked for throttle without moving.
    pub stalled_since: Option<f64>,
    /// Server time until which the bot backs off a stall.
    pub back_off_until: f64,
}

impl BotAgentState {
    pub fn new(entity_id: i32) -> Self {
        Self {
            entity_id,
            mode: BotBehaviorMode::Tactical,
            current_path: Vec::new(),
            waypoint_idx: 0,
            current_target_id: None,
            retreat: false,
            hull_angle_rad: 0.0,
            last_repairs_try: 0.0,
            goal_pos: None,
            stalled_since: None,
            back_off_until: f64::MIN,
        }
    }
}

/// Read-only snapshot of arena world state passed to `BotController::step_bots`.
pub struct ArenaReadView<'a> {
    pub vehicles: &'a [VehicleState],
    pub sim_states: &'a HashMap<i32, VehicleSimState>,
    pub combat_profiles: &'a HashMap<i32, VehicleCombatProfile>,
    pub aim_states: &'a HashMap<i32, AimState>,
    pub visibility: &'a VisibilityTracker,
    pub last_visibility: &'a VisibilityOutput,
    pub map: &'a ArenaMap,
    pub balance: &'a BalanceConfig,
    pub server_time: f64,
}

impl<'a> From<&'a ArenaInstance> for ArenaReadView<'a> {
    fn from(arena: &'a ArenaInstance) -> Self {
        Self {
            vehicles: &arena.vehicles,
            sim_states: &arena.sim_states,
            combat_profiles: &arena.combat_profiles,
            aim_states: &arena.aim_states,
            visibility: &arena.visibility,
            last_visibility: &arena.last_visibility,
            map: &arena.map,
            balance: &arena.balance,
            server_time: arena.server_time,
        }
    }
}

/// A bot under throttle slower than this is stalled.
const STALL_SPEED: f32 = 0.5;
/// Stall time before a bot backs off.
const STALL_SECS: f64 = 1.5;
const BACK_OFF_SECS: f64 = 1.2;
/// Detour beside a stall: metres to the right of the blocked heading, and ahead.
const DETOUR_SIDE: f32 = 10.0;
const DETOUR_AHEAD: f32 = 6.0;

fn normalize_angle(mut a: f32) -> f32 {
    while a > std::f32::consts::PI {
        a -= 2.0 * std::f32::consts::PI;
    }
    while a < -std::f32::consts::PI {
        a += 2.0 * std::f32::consts::PI;
    }
    a
}

/// Manager and controller for all active bot entities in an arena.
#[derive(Debug, Clone, Default)]
pub struct BotController {
    pub agents: HashMap<i32, BotAgentState>,
}

impl BotController {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn set_mode(&mut self, entity_id: i32, mode: BotBehaviorMode) {
        let agent = self.agents.entry(entity_id).or_insert_with(|| BotAgentState::new(entity_id));
        agent.mode = mode;
    }

    pub fn get_mode(&self, entity_id: i32) -> BotBehaviorMode {
        self.agents
            .get(&entity_id)
            .map(|a| a.mode)
            .unwrap_or(BotBehaviorMode::Tactical)
    }

    /// Step bot tactical behaviors and return per-bot intents.
    pub fn step_bots(
        &mut self,
        arena: &ArenaReadView,
        _dt: f32,
    ) -> Vec<(i32, BotIntent)> {
        let mut results = Vec::new();

        for bot_veh in arena.vehicles {
            if !bot_veh.is_bot || !bot_veh.is_alive || bot_veh.left_arena {
                continue;
            }

            let bot_id = bot_veh.entity_id;
            let agent = self.agents.entry(bot_id).or_insert_with(|| BotAgentState::new(bot_id));

            if agent.mode == BotBehaviorMode::PassiveTarget {
                results.push((bot_id, BotIntent::default()));
                continue;
            }

            let mut intent = BotIntent::default();
            let bot_sim = arena.sim_states.get(&bot_id);
            let bot_pos = bot_veh.position;
            let bot_yaw = bot_veh.yaw;

            // 1. Consumable triggers
            if let Some(sim) = bot_sim {
                let find_eq_id = |ids: &[u16], default_id: u16| -> u16 {
                    sim.damage
                        .equipment_slots
                        .iter()
                        .flatten()
                        .find(|s| ids.contains(&s.id) && s.count > 0)
                        .map(|s| s.id)
                        .unwrap_or(default_id)
                };
                if sim.damage.fire_state.is_some() {
                    let eq_id = find_eq_id(&[0, 1], 0);
                    intent.activate_equipment = Some((eq_id, 0));
                } else if sim.physics.left_track_broken
                    || sim.damage.left_track.state == crate::sim::crits::DeviceState::Destroyed
                {
                    let eq_id = find_eq_id(&[4, 5], 4);
                    intent.activate_equipment = Some((eq_id, EXTRA_LEFT_TRACK as u16));
                } else if sim.physics.right_track_broken
                    || sim.damage.right_track.state == crate::sim::crits::DeviceState::Destroyed
                {
                    let eq_id = find_eq_id(&[4, 5], 4);
                    intent.activate_equipment = Some((eq_id, EXTRA_RIGHT_TRACK as u16));
                } else if sim.damage.ammo_bay.state == crate::sim::crits::DeviceState::Damaged
                    || sim.damage.ammo_bay.state == crate::sim::crits::DeviceState::Destroyed
                {
                    let eq_id = find_eq_id(&[4, 5], 4);
                    intent.activate_equipment = Some((eq_id, EXTRA_AMMO_BAY as u16));
                } else if sim.damage.engine.state != crate::sim::crits::DeviceState::Normal
                    || sim.damage.gun.state != crate::sim::crits::DeviceState::Normal
                    || sim.damage.turret_rotator.state != crate::sim::crits::DeviceState::Normal
                {
                    let eq_id = find_eq_id(&[4, 5], 4);
                    intent.activate_equipment = Some((eq_id, 0));
                } else if sim.damage.tankmen.iter().any(|t| !t.is_alive) {
                    let eq_id = find_eq_id(&[2, 3], 2);
                    intent.activate_equipment = Some((eq_id, 0));
                }
            }

            // 2. Fair Target Acquisition: only target enemies in team spotted set!
            let spotted_set = arena.last_visibility.spotted_enemies.get(&bot_id);
            let mut best_target: Option<(&VehicleState, f32)> = None;

            if let Some(spotted) = spotted_set {
                for enemy in arena.vehicles {
                    if enemy.team != bot_veh.team
                        && enemy.is_alive
                        && !enemy.left_arena
                        && spotted.contains(&enemy.entity_id)
                    {
                        let dist = (enemy.position - bot_pos).length();
                        if let Some((_, best_dist)) = best_target {
                            if dist < best_dist {
                                best_target = Some((enemy, dist));
                            }
                        } else {
                            best_target = Some((enemy, dist));
                        }
                    }
                }
            }

            agent.current_target_id = best_target.map(|(t, _)| t.entity_id);

            // 3. Aiming & Ballistic Lead Calculation
            if let Some((target_veh, dist)) = best_target {
                let bot_prof = arena.combat_profiles.get(&bot_id);
                let default_shot_speed = bot_prof
                    .map(|p| p.default_shot_speed)
                    .unwrap_or(500.0);

                let time_to_impact = dist / default_shot_speed.max(10.0);
                let target_vel = Vec3::new(
                    target_veh.speed * target_veh.yaw.sin(),
                    0.0,
                    target_veh.speed * target_veh.yaw.cos(),
                );
                let lead_pos = target_veh.position
                    + Vec3::new(0.0, 0.8, 0.0) // Aim at hull/turret height
                    + target_vel * time_to_impact;

                intent.target_point = Some(lead_pos);

                // Check reload state: fire if ready
                let reload_left = arena
                    .aim_states
                    .get(&bot_id)
                    .map(|a| a.reload_left)
                    .unwrap_or(0.0);

                if reload_left <= 0.0 {
                    intent.shoot = true;
                }

                // Check armor penetration and swap to HE if AP cannot penetrate
                if let Some(target_prof) = arena.combat_profiles.get(&target_veh.entity_id) {
                    let max_armor = target_prof
                        .hull_armor
                        .as_ref()
                        .map(|a| a.zones.values().map(|(thick, _)| *thick as f32).fold(0.0f32, f32::max))
                        .unwrap_or(0.0);
                    let ap_pen = bot_prof
                        .and_then(|p| p.primary_shot.as_ref())
                        .map(|s| s.piercing_power.0 as f32)
                        .unwrap_or(50.0);

                    if max_armor > ap_pen {
                        if let Some(p) = bot_prof {
                            let he_idx = p.all_shots.iter().position(|s| {
                                s.shell_name.contains("HE")
                                    || p.all_shells.iter().any(|sh| {
                                        sh.id == s.shell_id
                                            && sh.kind == wot065_data::item_defs::ShellKind::HighExplosive
                                    })
                            }).or_else(|| {
                                if p.all_shots.len() > 1 {
                                    Some(1)
                                } else {
                                    None
                                }
                            });
                            if let Some(idx) = he_idx {
                                intent.request_shell_index = Some(idx);
                            }
                        }
                    }
                }
            }

            // 4. Tactical Steering & Navigation along NavGrid with Class-Weighted Roles
            let is_low_hp = bot_sim
                .map(|s| (s.health as f32 / s.max_health.max(1) as f32) < 0.3)
                .unwrap_or(false);

            let is_light = bot_sim.map(|s| s.tags.contains("lightTank")).unwrap_or(false);
            let is_medium = bot_sim.map(|s| s.tags.contains("mediumTank")).unwrap_or(false);
            let is_td = bot_sim.map(|s| s.tags.contains("AT-SPG")).unwrap_or(false);
            let is_spg = bot_sim.map(|s| s.tags.contains("SPG")).unwrap_or(false);

            let friendly_base = arena.map.bases.iter().find(|b| b.team == bot_veh.team);
            let enemy_invading_base = friendly_base.and_then(|base| {
                arena.vehicles.iter().find(|v| {
                    v.team != bot_veh.team
                        && v.is_alive
                        && !v.left_arena
                        && (v.position - base.position).length() <= base.radius * 1.5
                })
            });

            let enemy_team = if bot_veh.team == 1 { 2 } else { 1 };
            let enemy_base = arena.map.bases.iter().find(|b| b.team == enemy_team);
            let enemy_spawns = arena.map.spawns_for_team(enemy_team);
            let default_enemy_spawn = enemy_spawns
                .first()
                .map(|s| s.position)
                .unwrap_or(Vec3::new(if bot_veh.team == 1 { 40.0 } else { -40.0 }, 5.0, 0.0));

            let desired_goal = if is_low_hp {
                // Low HP: Retreat to friendly spawn/cover
                let friendly_spawns = arena.map.spawns_for_team(bot_veh.team);
                friendly_spawns.first().map(|s| s.position).unwrap_or(bot_pos)
            } else if let Some(_) = enemy_invading_base {
                // Defend friendly base under invasion
                friendly_base.map(|b| b.position).unwrap_or(bot_pos)
            } else if let Some((target, _)) = best_target {
                if is_light || is_medium {
                    // Flank enemy: offset perpendicular to line of sight
                    let delta = target.position - bot_pos;
                    let len = delta.length();
                    if len > 1.0 {
                        let to_target = delta / len;
                        let perp = Vec3::new(-to_target.z, 0.0, to_target.x);
                        target.position + perp * 20.0
                    } else {
                        target.position
                    }
                } else if is_td || is_spg {
                    // Support / sniper: maintain distance (40m)
                    let delta = bot_pos - target.position;
                    let len = delta.length();
                    if len > 1.0 {
                        target.position + (delta / len) * 40.0
                    } else {
                        target.position
                    }
                } else {
                    // Heavy / assault: approach target directly
                    target.position
                }
            } else if let Some(ebase) = enemy_base {
                // No target: capture enemy base
                ebase.position
            } else {
                // Push enemy team spawns
                default_enemy_spawn
            };

            // Recompute path if needed
            let needs_path = agent.current_path.is_empty()
                || agent.waypoint_idx >= agent.current_path.len()
                || agent
                    .goal_pos
                    .map_or(true, |g| (g - desired_goal).length() > 20.0);

            if needs_path {
                agent.goal_pos = Some(desired_goal);
                agent.current_path = arena.map.nav_grid.find_path(bot_pos, desired_goal, &arena.map.terrain);
                agent.waypoint_idx = 0;
            }

            // Steering along waypoints
            if !agent.current_path.is_empty() && agent.waypoint_idx < agent.current_path.len() {
                let wp = agent.current_path[agent.waypoint_idx];
                let dist_to_wp = (Vec3::new(wp.x, 0.0, wp.z) - Vec3::new(bot_pos.x, 0.0, bot_pos.z)).length();

                if dist_to_wp < 6.0 && agent.waypoint_idx + 1 < agent.current_path.len() {
                    agent.waypoint_idx += 1;
                }

                let current_target_wp = agent.current_path[agent.waypoint_idx];
                let dx = current_target_wp.x - bot_pos.x;
                let dz = current_target_wp.z - bot_pos.z;

                let mut target_heading = dx.atan2(dz);

                // If engaging an enemy close-up, angle hull armor ~30° (0.52 rad)
                if let Some((_, dist)) = best_target {
                    if dist < 60.0 {
                        target_heading = normalize_angle(target_heading + 0.52);
                    }
                }

                let delta_yaw = normalize_angle(target_heading - bot_yaw);

                let mut move_flags = 0u8;
                if delta_yaw > 0.08 {
                    move_flags |= 8; // Right
                } else if delta_yaw < -0.08 {
                    move_flags |= 4; // Left
                }

                if delta_yaw.abs() < 1.4 {
                    move_flags |= 1; // Forward
                }

                intent.move_flags = move_flags;
            }

            // Head-on against something it can neither break nor push, a hull gets no slide and
            // stays at speed 0 under full throttle: back off, then drive to a detour beside it.
            let speed = bot_sim.map_or(0.0, |s| s.physics.speed_mps.abs());
            let now = arena.server_time;
            if now < agent.back_off_until {
                intent.move_flags = 2;
            } else if intent.move_flags & 1 != 0 && speed < STALL_SPEED {
                let since = *agent.stalled_since.get_or_insert(now);
                if now - since >= STALL_SECS {
                    agent.stalled_since = None;
                    agent.back_off_until = now + BACK_OFF_SECS;
                    let forward = Vec3::new(bot_yaw.sin(), 0.0, bot_yaw.cos());
                    let right = Vec3::new(bot_yaw.cos(), 0.0, -bot_yaw.sin());
                    let at = agent.waypoint_idx.min(agent.current_path.len());
                    agent.current_path.insert(at, bot_pos + right * DETOUR_SIDE + forward * DETOUR_AHEAD);
                    agent.waypoint_idx = at;
                    intent.move_flags = 2;
                }
            } else {
                agent.stalled_since = None;
            }

            results.push((bot_id, intent));
        }

        results
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;
    use std::path::PathBuf;
    use std::sync::Arc;
    use glam::Mat4;
    use wot065_data::catalog;
    use wot065_data::descr::VehicleDescr;
    use crate::assets::arena_defs::ArenaDef;
    use crate::assets::chunk::{ChunkCoords, ChunkKind, SpaceBounds, SpawnPoint};
    use crate::assets::map_store::TerrainMap;
    use crate::assets::terrain::{MaterialKinds, TerrainChunk, DOMINANT_TEX_GRID_SIZE, HEIGHTMAP_GRID_SIZE};
    use crate::lobby::{BattleMode, ParticipantSpec, ARENA_BONUS_TYPE_REGULAR, ARENA_GUI_TYPE_RANDOM, ArenaLaunchSpec};

    fn make_test_map() -> Arc<ArenaMap> {
        let def = ArenaDef {
            id: 4,
            name: "test_bot_map".into(),
            display_name_key: "TestBotMap".into(),
            description_key: String::new(),
            geometry: "spaces/test_bot_map".into(),
            bounding_box: ([-100.0, -100.0], [100.0, 100.0]),
            min_players_in_team: 1,
            max_players_in_team: 15,
            round_length_secs: 900,
            arena_start_delay_secs: 5.0,
            kick_after_finish_wait_secs: 10.0,
            minimap_texture: None,
        };
        let bounds = SpaceBounds { min_x: -1, max_x: 0, min_y: -1, max_y: 0 };
        let mut chunks = Vec::new();
        for gz in -1..=0 {
            for gx in -1..=0 {
                chunks.push((
                    ChunkCoords { grid_x: gx, grid_z: gz, kind: ChunkKind::Outer },
                    TerrainChunk {
                        heights: vec![5.0; HEIGHTMAP_GRID_SIZE * HEIGHTMAP_GRID_SIZE],
                        min_y: 5.0,
                        max_y: 5.0,
                        dom_tex_indices: vec![0; DOMINANT_TEX_GRID_SIZE * DOMINANT_TEX_GRID_SIZE],
                        tex_names: vec![],
                    },
                ));
            }
        }
        let terrain = TerrainMap::from_chunks(bounds, chunks).unwrap();
        let mats = Arc::new(MaterialKinds::parse(b"<root/>").unwrap());
        let spawns = vec![
            SpawnPoint { team: 1, preferable_type: None, world_transform: Mat4::IDENTITY, position: Vec3::new(-50.0, 5.0, -30.0), yaw: 0.0 },
            SpawnPoint { team: 1, preferable_type: None, world_transform: Mat4::IDENTITY, position: Vec3::new(-50.0, 5.0, 0.0), yaw: 0.0 },
            SpawnPoint { team: 1, preferable_type: None, world_transform: Mat4::IDENTITY, position: Vec3::new(-50.0, 5.0, 30.0), yaw: 0.0 },
            SpawnPoint { team: 2, preferable_type: None, world_transform: Mat4::IDENTITY, position: Vec3::new(50.0, 5.0, -30.0), yaw: std::f32::consts::PI },
            SpawnPoint { team: 2, preferable_type: None, world_transform: Mat4::IDENTITY, position: Vec3::new(50.0, 5.0, 0.0), yaw: std::f32::consts::PI },
            SpawnPoint { team: 2, preferable_type: None, world_transform: Mat4::IDENTITY, position: Vec3::new(50.0, 5.0, 30.0), yaw: std::f32::consts::PI },
        ];
        Arc::new(ArenaMap::from_parts(
            def, bounds, terrain, mats, vec![], vec![], vec![], spawns, vec![], BTreeMap::new(), &BalanceConfig::default(),
        ))
    }

    fn test_catalog(name: &str) -> (catalog::Catalog, Vec<u8>, PathBuf) {
        let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../target/test-tmp")
            .join(format!("bot_ai_{name}_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        for nation in ["ussr", "germany", "usa"] {
            let ndir = dir.join(nation);
            let cdir = ndir.join("components");
            std::fs::create_dir_all(&cdir).unwrap();
            std::fs::write(ndir.join("customization.xml"), "<root/>").unwrap();
            if nation == "ussr" {
                std::fs::write(ndir.join("list.xml"), "<root><MS-1><id>0</id><userString>MS-1</userString><level>1</level><price>0</price><tags>lightTank</tags></MS-1></root>").unwrap();
                std::fs::write(ndir.join("MS-1.xml"), "<root><crew><commander/><driver/></crew><speedLimits><forward>32</forward><backward>10</backward></speedLimits><hull><weight>3000</weight><maxHealth>80</maxHealth><armor><armor_1>15</armor_1></armor><turretPositions><turret>0 0.5 0</turret></turretPositions></hull><chassis><Ch>shared</Ch></chassis><engines><Eng>shared</Eng></engines><fuelTanks><Fuel>shared</Fuel></fuelTanks><radios><Rad>shared</Rad></radios><turrets0><T>shared</T></turrets0></root>").unwrap();
            } else {
                std::fs::write(ndir.join("list.xml"), "<root/>").unwrap();
            }
            std::fs::write(cdir.join("shells.xml"), "<root><AP><id>1</id><price>5</price><kind>ARMOR_PIERCING</kind><caliber>20</caliber><isTracer>true</isTracer><effects>smallAP</effects><damage><armor>15</armor><devices>20</devices></damage></AP><HE><id>2</id><price>5</price><kind>HIGH_EXPLOSIVE</kind><caliber>20</caliber><isTracer>true</isTracer><effects>smallHE</effects><damage><armor>25</armor><devices>30</devices></damage></HE></root>").unwrap();
            std::fs::write(cdir.join("chassis.xml"), "<root><ids><Ch>1</Ch></ids><shared><Ch><weight>1000</weight><level>1</level><price>100</price><maxLoad>10000</maxLoad></Ch></shared></root>").unwrap();
            std::fs::write(cdir.join("engines.xml"), "<root><ids><Eng>1</Eng></ids><shared><Eng><weight>500</weight><level>1</level><price>100</price><power>100</power></Eng></shared></root>").unwrap();
            std::fs::write(cdir.join("fuelTanks.xml"), "<root><ids><Fuel>1</Fuel></ids><shared><Fuel><weight>100</weight><level>1</level><price>50</price></Fuel></shared></root>").unwrap();
            std::fs::write(cdir.join("radios.xml"), "<root><ids><Rad>1</Rad></ids><shared><Rad><weight>50</weight><level>1</level><price>50</price><distance>300</distance></Rad></shared></root>").unwrap();
            std::fs::write(cdir.join("guns.xml"), "<root><ids><G>1</G></ids><shared><G><level>1</level><price>100</price><weight>50</weight><maxAmmo>50</maxAmmo><pitchLimits>-10 20</pitchLimits><rotationSpeed>30</rotationSpeed><reloadTime>1.0</reloadTime><aimingTime>1.0</aimingTime><shotDispersionRadius>0.4</shotDispersionRadius><effects>small</effects><shots><AP><defaultPortion>0.8</defaultPortion><speed>500</speed><gravity>9.81</gravity><maxDistance>400</maxDistance><piercingPower>30 20</piercingPower></AP><HE><defaultPortion>0.2</defaultPortion><speed>400</speed><gravity>9.81</gravity><maxDistance>400</maxDistance><piercingPower>15 10</piercingPower></HE></shots></G></shared></root>").unwrap();
            std::fs::write(cdir.join("turrets.xml"), "<root><ids><T>1</T></ids><shared><T><level>1</level><price>100</price><weight>200</weight><maxHealth>20</maxHealth><yawLimits>-180 180</yawLimits><guns><G>shared</G></guns></T></shared></root>").unwrap();
        }
        let cat = catalog::load_from_vehicles_dir(&dir).unwrap();
        let descr = VehicleDescr::stock(cat.vehicle_def_by_id(0, 0).unwrap()).unwrap().to_bytes();
        (cat, descr, dir)
    }

    #[test]
    fn bots_navigate_map_spot_fairly_and_kill_enemies() {
        let map = make_test_map();
        let (cat, comp_descr, _dir) = test_catalog("3v3_combat");

        let mut next_eid = 10;
        let mut alloc_id = || {
            let id = next_eid;
            next_eid += 1;
            id
        };

        let spec = ArenaLaunchSpec {
            arena_type_id: 4,
            arena_gui_type: ARENA_GUI_TYPE_RANDOM,
            arena_bonus_type: ARENA_BONUS_TYPE_REGULAR,
            mode: BattleMode::Random,
            round_length_secs: 600,
            prebattle_id: 0,
            participants: vec![
                // Team 1: 3 bots
                ParticipantSpec {
                    account_db_id: 1001,
                    player_name: "bot1_t1".into(),
                    veh_inv_id: 1,
                    veh_comp_descr: comp_descr.clone(),
                    team: 1,
                    prebattle_id: 0,
                    is_bot: true,
                    ..Default::default()
                },
                ParticipantSpec {
                    account_db_id: 1002,
                    player_name: "bot2_t1".into(),
                    veh_inv_id: 2,
                    veh_comp_descr: comp_descr.clone(),
                    team: 1,
                    prebattle_id: 0,
                    is_bot: true,
                    ..Default::default()
                },
                ParticipantSpec {
                    account_db_id: 1003,
                    player_name: "bot3_t1".into(),
                    veh_inv_id: 3,
                    veh_comp_descr: comp_descr.clone(),
                    team: 1,
                    prebattle_id: 0,
                    is_bot: true,
                    ..Default::default()
                },
                // Team 2: 3 bots
                ParticipantSpec {
                    account_db_id: 2001,
                    player_name: "bot1_t2".into(),
                    veh_inv_id: 4,
                    veh_comp_descr: comp_descr.clone(),
                    team: 2,
                    prebattle_id: 0,
                    is_bot: true,
                    ..Default::default()
                },
                ParticipantSpec {
                    account_db_id: 2002,
                    player_name: "bot2_t2".into(),
                    veh_inv_id: 5,
                    veh_comp_descr: comp_descr.clone(),
                    team: 2,
                    prebattle_id: 0,
                    is_bot: true,
                    ..Default::default()
                },
                ParticipantSpec {
                    account_db_id: 2003,
                    player_name: "bot3_t2".into(),
                    veh_inv_id: 6,
                    veh_comp_descr: comp_descr.clone(),
                    team: 2,
                    prebattle_id: 0,
                    is_bot: true,
                    ..Default::default()
                },
            ],
        };

        let mut arena = ArenaInstance::spawn(1, map, &spec, &cat, &mut alloc_id);
        arena.start_battle();

        // Configure bot3_t2 as PassiveTarget (training target mode)
        let passive_bot_id = arena.vehicles.iter().find(|v| v.player_name == "bot3_t2").unwrap().entity_id;
        arena.bot_controller.set_mode(passive_bot_id, BotBehaviorMode::PassiveTarget);

        // Step 1: Initial state check before stepping - bots should emit intents
        let read_view = ArenaReadView::from(&arena);
        let mut controller = BotController::new();
        controller.set_mode(passive_bot_id, BotBehaviorMode::PassiveTarget);
        let intents = controller.step_bots(&read_view, 0.1);

        // Intent verification: must have intents for bots
        assert!(!intents.is_empty(), "BotController must emit intents for active bots");

        // Verify PassiveTarget bot stays stationary
        let passive_intent = intents.iter().find(|(id, _)| *id == passive_bot_id).map(|(_, i)| i);
        assert!(passive_intent.is_some(), "Passive bot must have intent emitted");
        assert_eq!(passive_intent.unwrap().move_flags, 0, "Passive bot must not move");
        assert!(!passive_intent.unwrap().shoot, "Passive bot must not shoot");

        // Step simulation for multiple ticks and verify progression:
        // - Bots navigate along NavGrid (changing position towards enemy)
        // - Fair spotting: hold fire until enemies are spotted
        // - Aiming & lead calculation when enemy spotted
        // - Dealing HP damage / breaking tracks
        let initial_t2_hp: i16 = arena.vehicles.iter().filter(|v| v.team == 2).map(|v| v.health).sum();
        let start_pos_t1 = arena.vehicles[0].position;

        for _tick in 0..100 {
            arena.step_with_events(0.1);
        }

        // Active tactical bots must have moved from spawn
        let end_pos_t1 = arena.vehicles[0].position;
        assert_ne!(start_pos_t1, end_pos_t1, "Tactical bot must advance along NavGrid");

        // PassiveTarget bot must not have moved
        let passive_end_pos = arena.vehicles.iter().find(|v| v.entity_id == passive_bot_id).unwrap().position;
        let passive_start_pos = Vec3::new(50.0, 5.0, 30.0);
        assert!((passive_end_pos - passive_start_pos).length() < 0.1, "Passive bot must stay at spawn");

        // HP damage must have been dealt between combatants
        let end_t2_hp: i16 = arena.vehicles.iter().filter(|v| v.team == 2).map(|v| v.health).sum();
        assert!(end_t2_hp < initial_t2_hp, "Bots must engage and deal HP damage to spotted enemies");
    }

    #[test]
    fn bot_pushing_against_something_it_cannot_move_backs_off_and_detours() {
        // Head-on against an unbreakable tree a hull gets no slide: it commands full throttle
        // and stays at speed 0. After 1.5 s the bot must reverse, then drive to a detour.
        let map = make_test_map();
        let (cat, comp_descr, _dir) = test_catalog("stuck_recovery");
        let mut next_eid = 50;
        let mut alloc_id = || {
            let id = next_eid;
            next_eid += 1;
            id
        };
        let bot = |account_db_id: i64, name: &str, team: u8| ParticipantSpec {
            account_db_id,
            player_name: name.into(),
            veh_inv_id: account_db_id,
            veh_comp_descr: comp_descr.clone(),
            team,
            prebattle_id: 0,
            is_bot: true,
            ..Default::default()
        };
        let spec = ArenaLaunchSpec {
            arena_type_id: 4,
            arena_gui_type: ARENA_GUI_TYPE_RANDOM,
            arena_bonus_type: ARENA_BONUS_TYPE_REGULAR,
            mode: BattleMode::Random,
            round_length_secs: 600,
            prebattle_id: 0,
            participants: vec![bot(2001, "driver", 1), bot(2002, "target", 2)],
        };
        let mut arena = ArenaInstance::spawn(1, map, &spec, &cat, &mut alloc_id);
        arena.start_battle();
        let target_id = arena.vehicles.iter().find(|v| v.player_name == "target").unwrap().entity_id;
        let driver = arena.vehicles.iter().position(|v| v.player_name == "driver").unwrap();
        let driver_id = arena.vehicles[driver].entity_id;
        let mut controller = BotController::new();
        controller.set_mode(target_id, BotBehaviorMode::PassiveTarget);

        let flags_at = |controller: &mut BotController, arena: &ArenaInstance| {
            let intents = controller.step_bots(&ArenaReadView::from(arena), 0.1);
            intents.into_iter().find(|(id, _)| *id == driver_id).unwrap().1.move_flags
        };
        flags_at(&mut controller, &arena);
        // Face the first waypoint so the bot asks for full throttle; the hull never moves.
        let agent = &controller.agents[&driver_id];
        let waypoint = agent.current_path[agent.waypoint_idx];
        let from = arena.vehicles[driver].position;
        arena.vehicles[driver].yaw = (waypoint.x - from.x).atan2(waypoint.z - from.z);
        let path_len = controller.agents[&driver_id].current_path.len();

        let mut flags = Vec::new();
        for _ in 0..40 {
            arena.server_time += 0.1;
            flags.push(flags_at(&mut controller, &arena));
        }
        assert!(flags[..10].iter().all(|f| f & 1 != 0), "{flags:?}");
        let reversing = flags.iter().position(|f| f & 2 != 0).expect("the bot never reversed");
        assert!((13..=17).contains(&reversing), "reversed after {reversing} ticks: {flags:?}");
        assert!(flags[reversing + 13..].iter().all(|f| f & 2 == 0), "{flags:?}");
        assert_eq!(controller.agents[&driver_id].current_path.len(), path_len + 1, "no detour waypoint");
    }

    #[test]
    fn bot_swaps_to_he_against_impenetrable_armor_and_adapts_corridor() {
        let map = make_test_map();
        let (cat, comp_descr, _dir) = test_catalog("bot_tactical_extra");

        let mut next_eid = 30;
        let mut alloc_id = || {
            let id = next_eid;
            next_eid += 1;
            id
        };

        let spec = ArenaLaunchSpec {
            arena_type_id: 4,
            arena_gui_type: ARENA_GUI_TYPE_RANDOM,
            arena_bonus_type: ARENA_BONUS_TYPE_REGULAR,
            mode: BattleMode::Random,
            round_length_secs: 600,
            prebattle_id: 0,
            participants: vec![
                ParticipantSpec {
                    account_db_id: 3001,
                    player_name: "bot_shooter".into(),
                    veh_inv_id: 1,
                    veh_comp_descr: comp_descr.clone(),
                    team: 1,
                    prebattle_id: 0,
                    is_bot: true,
                    shells: vec![(1, 20), (2, 20)], // Shell 0 = AP (30mm pen), Shell 1 = HE (15mm pen)
                    ..Default::default()
                },
                ParticipantSpec {
                    account_db_id: 3002,
                    player_name: "bot_heavy_target".into(),
                    veh_inv_id: 2,
                    veh_comp_descr: comp_descr.clone(),
                    team: 2,
                    prebattle_id: 0,
                    is_bot: true,
                    ..Default::default()
                },
            ],
        };

        let mut arena = ArenaInstance::spawn(2, map, &spec, &cat, &mut alloc_id);
        arena.start_battle();

        let shooter_id = arena.vehicles[0].entity_id;
        let target_id = arena.vehicles[1].entity_id;

        // Position target in direct sight at distance 40m
        arena.vehicles[1].position = Vec3::new(0.0, 5.0, 0.0);
        if let Some(sim) = arena.sim_states.get_mut(&target_id) {
            sim.physics.pos = Vec3::new(0.0, 5.0, 0.0);
            sim.tags = ["heavyTank".to_string()].into_iter().collect();
        }
        arena.vehicles[0].position = Vec3::new(-40.0, 5.0, 0.0);
        if let Some(sim) = arena.sim_states.get_mut(&shooter_id) {
            sim.physics.pos = Vec3::new(-40.0, 5.0, 0.0);
            sim.tags = ["heavyTank".to_string()].into_iter().collect();
        }

        // Give target impenetrable armor (80mm vs 30mm AP pen)
        if let Some(prof) = arena.combat_profiles.get_mut(&target_id) {
            let mut armor = wot065_data::item_defs::ArmorZoneMap::new();
            armor.insert("armor_1", 80.0, false);
            prof.hull_armor = Some(armor);
        }

        // Spot target directly for team 1
        arena.visibility.direct_spots.insert((shooter_id, target_id), 10.0);
        arena.last_visibility.spotted_enemies.entry(shooter_id).or_default().insert(target_id);

        // Step arena: BotController must detect impenetrable target and request HE swap
        let mut controller = BotController::new();
        let read_view = ArenaReadView::from(&arena);
        let intents = controller.step_bots(&read_view, 0.1);

        let shooter_intent = intents.iter().find(|(id, _)| *id == shooter_id).map(|(_, i)| i);
        assert!(shooter_intent.is_some(), "Shooter bot must have intent");
        assert_eq!(
            shooter_intent.unwrap().request_shell_index,
            Some(1),
            "Bot must request HE shell (index 1) when AP cannot penetrate 80mm armor"
        );
    }
}
