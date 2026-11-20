//! Active battle arena instances (`BattleWorld`, `ArenaInstance`, `VehicleState`).

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::Arc;
use std::time::Duration;

use glam::Vec3;
use wot065_data::catalog::Catalog;
use wot065_data::descr::VehicleDescr;
use wot065_data::item_defs::{ArmorZoneMap, ShellXmlDef, ShotDef};
use wot065_data::pickle::{self, Value};

use crate::assets::map_store::{ArenaMap, VehicleCollisionCache, VehicleCollisionSet};
use crate::lobby::{ArenaLaunchSpec, BattleMode, ParticipantSpec};
use crate::sim::capture::BaseCaptureTracker;
use crate::sim::chat::BattleChatRouter;
use crate::sim::tkill::TkillTracker;
use crate::sim::visibility::{ObserverFocusState, VEHICLE_AOI_RADIUS};
use crate::sim::{DamageInfoCode, TargetingSnapshot};

mod combat;
mod round;

/// Static combat attributes extracted from vehicle catalog definition.
#[derive(Debug, Clone)]
pub struct VehicleCombatProfile {
    pub turret_yaw_limits: Option<(f32, f32)>,
    pub turret_rot_speed: f32,
    pub gun_pitch_limits: Option<(f32, f32)>,
    pub gun_rot_speed: f32,
    pub hull_pos: Vec3,
    pub turret_pos: Vec3,
    pub gun_pos: Vec3,
    pub base_aiming_time: f32,
    pub base_disp_angle: f32,
    pub disp_factors: [f32; 4], // [speed, rspeed, turret_rot, after_shot]
    pub reload_time: f32,
    pub chassis_armor: Option<ArmorZoneMap>,
    pub hull_armor: Option<ArmorZoneMap>,
    pub turret_armor: Option<ArmorZoneMap>,
    pub gun_armor: Option<ArmorZoneMap>,
    pub default_shot_speed: f32,
    pub default_shot_gravity: f32,
    pub collision_set: Option<Arc<VehicleCollisionSet>>,
    pub primary_shot: Option<ShotDef>,
    pub primary_shell: Option<ShellXmlDef>,
    pub all_shots: Vec<ShotDef>,
    pub all_shells: Vec<ShellXmlDef>,
    pub veh_type_cd: i32,
    pub chassis_id: u16,
    pub turret_id: u16,
    pub gun_id: u16,
}

impl Default for VehicleCombatProfile {
    fn default() -> Self {
        Self {
            turret_yaw_limits: None,
            turret_rot_speed: 0.6,
            gun_pitch_limits: Some((-15.0f32.to_radians(), 10.0f32.to_radians())),
            gun_rot_speed: 0.5,
            hull_pos: Vec3::new(0.0, 0.8, 0.0),
            turret_pos: Vec3::new(0.0, 0.6, 0.0),
            gun_pos: Vec3::new(0.0, 0.0, 1.2),
            base_aiming_time: 2.5,
            base_disp_angle: 0.38 / 100.0,
            disp_factors: [0.2, 0.2, 0.16, 4.0],
            reload_time: 5.0,
            chassis_armor: None,
            hull_armor: None,
            turret_armor: None,
            gun_armor: None,
            default_shot_speed: 500.0,
            default_shot_gravity: 9.81,
            collision_set: None,
            primary_shot: None,
            primary_shell: None,
            all_shots: Vec::new(),
            all_shells: Vec::new(),
            veh_type_cd: 0,
            chassis_id: 0,
            turret_id: 0,
            gun_id: 0,
        }
    }
}

pub const ARENA_PERIOD_WAITING: u8 = 1;
pub const ARENA_PERIOD_PREBATTLE: u8 = 2;
pub const ARENA_PERIOD_BATTLE: u8 = 3;
pub const ARENA_PERIOD_AFTERBATTLE: u8 = 4;

pub const ARENA_UPDATE_VEHICLE_LIST: u8 = 1;
pub const ARENA_UPDATE_VEHICLE_ADDED: u8 = 2;
pub const ARENA_UPDATE_PERIOD: u8 = 3;
pub const ARENA_UPDATE_STATISTICS: u8 = 4;
pub const ARENA_UPDATE_VEHICLE_STATISTICS: u8 = 5;
pub const ARENA_UPDATE_VEHICLE_KILLED: u8 = 6;
pub const ARENA_UPDATE_AVATAR_READY: u8 = 7;
pub const ARENA_UPDATE_BASE_POINTS: u8 = 8;
pub const ARENA_UPDATE_BASE_CAPTURED: u8 = 9;
pub const ARENA_UPDATE_TEAM_KILLER: u8 = 10;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum FinishReason {
    Extermination = 1,
    Base = 2,
    Timeout = 3,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ArenaFinished {
    pub winner_team: u8,
    pub reason: FinishReason,
}

#[derive(Debug, Clone, Default)]
pub struct VehicleBattleStats {
    pub frags: u32,
    pub shots: u32,
    pub hits: u32,
    pub shots_received: u32,
    pub pierced: u32,
    pub damage_dealt: u32,
    pub damage_received: u32,
    pub potential_damage_dealt: u32,
    pub potential_damage_received: u32,
    pub damaged_vehicles: BTreeSet<i32>,
    pub destroyed_vehicles: BTreeSet<i32>,
    pub spotted_vehicles: BTreeSet<i32>,
    pub shells_used: BTreeMap<i32, u32>,
    pub capture_points: f32,
    pub dropped_capture_points: f32,
    pub lifetime_secs: f64,
    pub killer_id: Option<i32>,
}

#[derive(Debug, Clone, Copy)]
struct BattleDamage {
    attacker_id: i32,
    target_id: i32,
    hp: u16,
    module_damage: bool,
    death: Option<DamageInfoCode>,
}

/// Live entity state for one vehicle inside an [`ArenaInstance`].
#[derive(Debug, Clone)]
pub struct VehicleState {
    pub entity_id: i32,
    pub account_db_id: i64,
    pub player_name: String,
    pub veh_inv_id: i64,
    pub veh_comp_descr: Vec<u8>,
    pub team: u8,
    pub prebattle_id: i32,
    pub is_bot: bool,
    pub is_alive: bool,
    pub is_avatar_ready: bool,
    pub is_team_killer: bool,
    pub health: i16,
    pub max_health: i16,
    pub position: Vec3,
    pub yaw: f32,
    pub pitch: f32,
    pub roll: f32,
    pub speed: f32,
    pub rspeed: f32,
    pub move_flags: u8,
    pub engine_mode: [u8; 2],
    pub is_crew_active: bool,
    pub left_arena: bool,
    pub damage_stickers: Vec<u64>,
    pub is_burning: bool,
}

/// Active arena space instance.
#[derive(Debug)]
pub struct ArenaInstance {
    pub space_id: i32,
    pub map: Arc<ArenaMap>,
    pub arena_gui_type: u8,
    pub arena_bonus_type: u8,
    pub mode: BattleMode,
    pub prebattle_id: i32,
    pub round_length_secs: u32,
    pub period: u8,
    pub period_end_time: f64,
    pub period_length_secs: u32,
    pub server_time: f64,
    /// Client update tick of the last movement snapshot the host sent; a tick is sent once.
    pub last_movement_tick: Option<u32>,
    pub prebattle_countdown_secs: u32,
    pub finished: Option<ArenaFinished>,
    pub statistics: HashMap<i32, VehicleBattleStats>,
    pub arena_updates: Vec<(u8, Vec<u8>)>,
    pub chat: BattleChatRouter,
    pub tkill: TkillTracker,
    pub auto_aim_losses: Vec<(i32, u8)>,
    disabled_server_markers: BTreeSet<i32>,
    observer_focus: HashMap<i32, ObserverFocusState>,
    clock: round::RoundClock,
    fire_requests: BTreeSet<i32>,
    battle_started_at: f64,
    capture: BaseCaptureTracker,
    battle_damage: Vec<BattleDamage>,
    pub vehicles: Vec<VehicleState>,
    pub balance: crate::balance::BalanceConfig,
    pub sim_states: HashMap<i32, crate::sim::VehicleSimState>,
    pub last_driving_events: ArenaDrivingEvents,
    pub aim_states: HashMap<i32, crate::sim::AimState>,
    pub targeting_snapshots: HashMap<i32, TargetingSnapshot>,
    pub combat_profiles: HashMap<i32, VehicleCombatProfile>,
    pub collision_cache: Option<Arc<VehicleCollisionCache>>,
    pub rng: rand::rngs::StdRng,
    pub projectiles: Vec<crate::sim::ProjectileState>,
    pub last_shots: Vec<ShotFiredEvent>,
    pub pending_external_shots: Vec<ShotFiredEvent>,
    pub pending_external_damage_events: Vec<crate::sim::DamageInfoEvent>,
    pub pending_external_state_changed_vehicles: Vec<i32>,
    pub in_step: bool,
    pub last_impact_outcomes: Vec<crate::sim::ImpactOutcome>,
    pub last_damage_events: Vec<crate::sim::DamageInfoEvent>,
    pub last_state_changed_vehicles: Vec<i32>,
    pub destructibles: crate::sim::destructibles::DestructiblesState,
    pub visibility: crate::sim::visibility::VisibilityTracker,
    pub last_visibility: crate::sim::visibility::VisibilityOutput,
    pub last_destructible_deltas: Vec<crate::sim::destructibles::DestructibleDelta>,
    pub bot_controller: crate::sim::BotController,
    pub next_shot_id: u32,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ShotFiredEvent {
    pub shot_id: u32,
    pub attacker_id: i32,
    pub muzzle_pos: Vec3,
    pub shot_dir: Vec3,
    pub speed: f32,
    pub gravity: f32,
}

/// Events emitted during arena vehicle driving physics simulation steps.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ArenaDrivingEvents {
    pub ramming_damages: Vec<(i32, i16)>,
    pub pushed_vehicles: Vec<(i32, (f32, f32))>,
    pub broken_destructibles: Vec<(u16, usize)>,
}

impl ArenaInstance {
    pub fn spawn(
        space_id: i32,
        map: Arc<ArenaMap>,
        spec: &ArenaLaunchSpec,
        catalog: &Catalog,
        alloc_entity_id: &mut impl FnMut() -> i32,
    ) -> Self {
        let mut occupied = Vec::with_capacity(spec.participants.len());
        let mut poses = vec![(Vec3::ZERO, 0.0); spec.participants.len()];
        // Claim authored spawn points for players before placing bot reinforcements.
        for is_bot in [false, true] {
            for (index, participant) in spec.participants.iter().enumerate().filter(|(_, p)| p.is_bot == is_bot) {
                let pose = spawn_pose(&map, participant.team, &occupied);
                poses[index] = pose;
                occupied.push(pose.0);
            }
        }
        let capture = BaseCaptureTracker::new(&map.bases);
        let destructibles = crate::sim::destructibles::DestructiblesState::from_map(&map);
        let visibility = crate::sim::visibility::VisibilityTracker::new();
        let mut arena = Self {
            space_id,
            map,
            arena_gui_type: spec.arena_gui_type,
            arena_bonus_type: spec.arena_bonus_type,
            mode: spec.mode,
            prebattle_id: spec.prebattle_id,
            round_length_secs: spec.round_length_secs,
            period: ARENA_PERIOD_WAITING,
            period_end_time: 0.0,
            period_length_secs: 0,
            server_time: 0.0,
            last_movement_tick: None,
            prebattle_countdown_secs: 0,
            finished: None,
            statistics: HashMap::with_capacity(spec.participants.len()),
            arena_updates: Vec::new(),
            chat: BattleChatRouter::new(space_id),
            tkill: TkillTracker::new(spec.mode),
            auto_aim_losses: Vec::new(),
            disabled_server_markers: BTreeSet::new(),
            observer_focus: HashMap::new(),
            clock: round::RoundClock::default(),
            fire_requests: BTreeSet::new(),
            battle_started_at: 0.0,
            capture,
            battle_damage: Vec::new(),
            vehicles: Vec::with_capacity(spec.participants.len()),
            balance: crate::balance::BalanceConfig::default(),
            sim_states: HashMap::with_capacity(spec.participants.len()),
            last_driving_events: ArenaDrivingEvents::default(),
            aim_states: HashMap::with_capacity(spec.participants.len()),
            targeting_snapshots: HashMap::new(),
            combat_profiles: HashMap::with_capacity(spec.participants.len()),
            collision_cache: None,
            rng: rand::SeedableRng::seed_from_u64(0xBA771E ^ u64::from(space_id as u32)),
            projectiles: Vec::new(),
            last_shots: Vec::new(),
            pending_external_shots: Vec::new(),
            pending_external_damage_events: Vec::new(),
            pending_external_state_changed_vehicles: Vec::new(),
            in_step: false,
            last_impact_outcomes: Vec::new(),
            last_damage_events: Vec::new(),
            last_state_changed_vehicles: Vec::new(),
            destructibles,
            visibility,
            last_visibility: crate::sim::visibility::VisibilityOutput::default(),
            last_destructible_deltas: Vec::new(),
            bot_controller: crate::sim::BotController::default(),
            next_shot_id: 1,
        };
        for (participant, pose) in spec.participants.iter().cloned().zip(poses) {
            arena.register_vehicle(alloc_entity_id(), participant, catalog, pose, true);
        }
        arena
    }

    pub fn set_collision_cache(
        &mut self,
        cache: Arc<VehicleCollisionCache>,
        catalog: Option<&Catalog>,
    ) {
        if let Some(cat) = catalog {
            for v in &self.vehicles {
                let Some((set, footprint)) = load_hull_collision(&cache, cat, &v.veh_comp_descr) else {
                    continue;
                };
                if let Some(profile) = self.combat_profiles.get_mut(&v.entity_id) {
                    profile.collision_set = Some(set);
                }
                if let Some(sim) = self.sim_states.get_mut(&v.entity_id) {
                    sim.footprint = footprint;
                }
            }
        }
        self.collision_cache = Some(cache);
    }

    pub fn vehicle_for_account(&self, account_db_id: i64) -> Option<&VehicleState> {
        self.vehicles
            .iter()
            .find(|v| v.account_db_id == account_db_id)
    }

    pub fn vehicle_by_id(&self, entity_id: i32) -> Option<&VehicleState> {
        self.vehicles.iter().find(|v| v.entity_id == entity_id)
    }

    pub fn vehicle_tuple_value(v: &VehicleState) -> Value {
        Value::tuple(vec![
            Value::Int(v.entity_id as i64),
            Value::Str(v.veh_comp_descr.clone()),
            Value::str(v.player_name.as_str()),
            Value::Int(v.team as i64),
            Value::Bool(v.is_alive),
            Value::Bool(v.is_avatar_ready),
            Value::Bool(v.is_team_killer),
            Value::Int(v.account_db_id),
            Value::str(""),
            Value::Int(0),
            Value::Int(v.prebattle_id as i64),
        ])
    }

    pub fn vehicle_added_pickle(v: &VehicleState) -> Vec<u8> {
        pickle::dumps(&Self::vehicle_tuple_value(v))
    }

    /// 11-tuple per vehicle: `(vehID, compDescr_bytes, name, team, isAlive, isAvatarReady, isTeamKiller, accountDBID, clanAbbrev, clanDBID, prebattleID)`.
    pub fn vehicle_list_pickle(&self) -> Vec<u8> {
        let tuples = self
            .vehicles
            .iter()
            .map(|v| Self::vehicle_tuple_value(v))
            .collect();
        pickle::dumps(&Value::list(tuples))
    }

    pub fn add_bot(
        &mut self,
        entity_id: i32,
        comp_descr: Vec<u8>,
        team: u8,
        name: String,
        catalog: &Catalog,
    ) -> (VehicleState, Vec<u8>) {
        self.add_participant(entity_id, ParticipantSpec {
            account_db_id: -i64::from(entity_id),
            veh_inv_id: -i64::from(entity_id),
            player_name: name,
            veh_comp_descr: comp_descr,
            team,
            prebattle_id: self.prebattle_id,
            is_bot: true,
            ..Default::default()
        }, catalog)
    }

    pub fn add_participant(
        &mut self,
        entity_id: i32,
        player: ParticipantSpec,
        catalog: &Catalog,
    ) -> (VehicleState, Vec<u8>) {
        let occupied: Vec<_> = self.vehicles.iter().filter(|v| v.is_alive).map(|v| v.position).collect();
        let pose = spawn_pose(&self.map, player.team, &occupied);
        let vehicle = self.register_vehicle(entity_id, player, catalog, pose, false).clone();
        let update = Self::vehicle_added_pickle(&vehicle);
        (vehicle, update)
    }

    fn register_vehicle(
        &mut self,
        entity_id: i32,
        player: ParticipantSpec,
        catalog: &Catalog,
        (pos, yaw): (Vec3, f32),
        stock_ammo_for_launch: bool,
    ) -> &VehicleState {
        let ParticipantSpec {
            account_db_id, player_name, veh_inv_id, veh_comp_descr, team, prebattle_id,
            is_bot, shells, eqs, crew,
        } = player;
        let max_health = resolve_max_health(catalog, &veh_comp_descr);
        let v = VehicleState {
            entity_id,
            account_db_id,
            player_name,
            veh_inv_id,
            veh_comp_descr,
            team,
            prebattle_id,
            is_bot,
            is_alive: true,
            is_avatar_ready: is_bot,
            is_team_killer: false,
            health: max_health,
            max_health,
            position: pos,
            yaw,
            pitch: 0.0,
            roll: 0.0,
            speed: 0.0,
            rspeed: 0.0,
            move_flags: 0,
            engine_mode: [1, 0],
            is_crew_active: true,
            left_arena: false,
            damage_stickers: Vec::new(),
            is_burning: false,
        };

        let mut prof = resolve_combat_profile(catalog, &v.veh_comp_descr);
        let mut footprint = None;
        let collision = self
            .collision_cache
            .as_deref()
            .and_then(|cache| load_hull_collision(cache, catalog, &v.veh_comp_descr));
        if let Some((set, hull)) = collision {
            prof.collision_set = Some(set);
            footprint = Some(hull);
        }
        let mut aim = crate::sim::AimState { ammo_counts: shells, ..Default::default() };
        if (is_bot || stock_ammo_for_launch) && aim.ammo_counts.is_empty() {
            for shot in &prof.all_shots {
                let count = if prof.all_shots.len() == 1 { 50 } else { 25 };
                aim.ammo_counts.push((shot.shell_cd, count));
            }
        }
        aim.active_shell_idx = 0;
        aim.current_shell_cd = aim.ammo_counts.first().map(|(cd, _)| *cd).unwrap_or(0);
        aim.ammo_dirty = true;
        self.combat_profiles.insert(entity_id, prof);

        let mut sim = crate::sim::VehicleSimState {
            entity_id,
            team,
            is_alive: true,
            health: max_health as i32,
            max_health: max_health as i32,
            descr: VehicleDescr::from_bytes(&v.veh_comp_descr).ok(),
            crew,
            physics: crate::sim::VehiclePhysicsState {
                pos,
                yaw,
                ..Default::default()
            },
            engine_power_hp: 500.0,
            speed_limits: (12.0, 4.0),
            rotation_speed_rad: 0.6,
            ..Default::default()
        };
        if let Some(footprint) = footprint {
            sim.footprint = footprint;
        }
        configure_sim_from_catalog(&mut sim, catalog, &v.veh_comp_descr, &eqs, is_bot);
        self.sim_states.insert(entity_id, sim);
        self.aim_states.insert(entity_id, aim);

        self.statistics.insert(entity_id, VehicleBattleStats::default());
        self.vehicles.push(v);
        self.vehicles.last().expect("the inserted vehicle")
    }

    pub fn period_pickle(&self) -> Vec<u8> {
        let additional_info = match self.finished {
            Some(result) => Value::tuple(vec![
                Value::Int(i64::from(result.winner_team)),
                Value::Int(i64::from(result.reason as u8)),
            ]),
            None => Value::None,
        };
        pickle::dumps(&Value::tuple(vec![
            Value::Int(self.period as i64),
            Value::Float(self.period_end_time),
            Value::Int(self.period_length_secs as i64),
            additional_info,
        ]))
    }

    pub fn avatar_ready_pickle(vehicle_id: i32) -> Vec<u8> {
        pickle::dumps(&Value::Int(vehicle_id as i64))
    }

    pub fn mark_client_ready(&mut self, account_db_id: i64) -> Vec<(u8, Vec<u8>)> {
        let mut updates = Vec::new();
        if let Some(v) = self
            .vehicles
            .iter_mut()
            .find(|v| v.account_db_id == account_db_id && !v.is_bot && !v.left_arena)
        {
            if v.is_avatar_ready {
                return updates;
            }
            v.is_avatar_ready = true;
            updates.push((
                ARENA_UPDATE_AVATAR_READY,
                Self::avatar_ready_pickle(v.entity_id),
            ));
        } else {
            return updates;
        }
        self.check_all_ready(&mut updates);
        updates
    }

    fn vehicle_statistics_value(&self, id: i32) -> Value {
        Value::tuple(vec![
            Value::Int(i64::from(id)),
            Value::Int(self.statistics.get(&id).map_or(0, |s| i64::from(s.frags))),
        ])
    }

    pub fn statistics_pickle(&self) -> Vec<u8> {
        pickle::dumps(&Value::List(self.vehicles.iter().map(|v| {
            self.vehicle_statistics_value(v.entity_id)
        }).collect()))
    }

    pub fn set_vehicle_move_flags(&mut self, account_db_id: i64, flags: u8) {
        if self.finished.is_some() {
            return;
        }
        let flags = flags & 63;
        if let Some(v) = self
            .vehicles
            .iter_mut()
            .find(|v| v.account_db_id == account_db_id && !v.left_arena)
        {
            v.move_flags = flags;
            if let Some(sim) = self.sim_states.get_mut(&v.entity_id) {
                sim.physics.move_flags = flags;
            }
        }
    }

    pub fn move_observer(&mut self, vehicle_id: i32, position: Vec3) {
        let can_move_camera = self.vehicle_by_id(vehicle_id).is_some_and(|vehicle| {
            !vehicle.is_alive || self.sim_states.get(&vehicle_id).is_some_and(|sim| sim.tags.contains("SPG"))
        });
        if !can_move_camera || !position.is_finite() {
            return;
        }
        let position = self.clamp_observer_position(position);
        self.observer_focus.entry(vehicle_id).or_default().move_to(position);
    }

    pub fn server_marker_enabled(&self, vehicle_id: i32) -> bool {
        !self.disabled_server_markers.contains(&vehicle_id)
    }

    pub fn set_development_feature(&mut self, vehicle_id: i32, feature: &str, value: i32, dev: bool) {
        if feature == "server_marker" {
            if value != 0 {
                self.disabled_server_markers.remove(&vehicle_id);
            } else {
                self.disabled_server_markers.insert(vehicle_id);
            }
            return;
        }
        if !dev || self.period != ARENA_PERIOD_BATTLE {
            return;
        }
        if feature == "stop_bot" {
            for bot in self.vehicles.iter().filter(|v| v.is_bot) {
                let mode = if value == 0 {
                    crate::sim::BotBehaviorMode::Tactical
                } else {
                    crate::sim::BotBehaviorMode::PassiveTarget
                };
                self.bot_controller.set_mode(bot.entity_id, mode);
            }
            return;
        }
        let Some(v) = self.vehicles.iter_mut().find(|v| v.entity_id == vehicle_id && v.is_alive) else {
            return;
        };
        let Some(sim) = self.sim_states.get_mut(&vehicle_id) else { return };
        match feature {
            "heal" => {
                v.health = v.max_health;
                sim.health = i32::from(v.health);
            }
            "reload_gun" => {
                if let Some(aim) = self.aim_states.get_mut(&vehicle_id) {
                    aim.reload_dirty |= aim.reload_left > 0.0;
                    aim.reload_left = 0.0;
                }
            }
            "start_fire" => {
                sim.damage.fire_state = Some(crate::sim::ActiveFire {
                    attacker_id: vehicle_id,
                    burn_damage_per_sec: self.balance.default_health_burn_per_sec,
                    elapsed_secs: 0.0,
                    duration_secs: 10.0,
                    tick_accumulator: 0.0,
                });
                sim.damage.public_state_modifiers.insert(crate::sim::EXTRA_FIRE);
                v.is_burning = true;
            }
            "break_left_track" | "break_right_track" => {
                let track = if feature == "break_left_track" {
                    &mut sim.damage.left_track
                } else {
                    &mut sim.damage.right_track
                };
                track.health = 0;
                track.state = crate::sim::DeviceState::Destroyed;
                track.repair_progress = 0.0;
            }
            "destroy_self" | "explode" => {
                let reason = if feature == "explode" {
                    sim.damage.ammo_bay.health = 0;
                    sim.damage.ammo_bay.state = crate::sim::DeviceState::Destroyed;
                    DamageInfoCode::DeathFromDeviceExplosionAtShot
                } else {
                    DamageInfoCode::DeathFromInactiveCrew
                };
                self.battle_damage.push(BattleDamage {
                    attacker_id: vehicle_id,
                    target_id: vehicle_id,
                    hp: v.health.max(0) as u16,
                    module_damage: feature == "explode",
                    death: Some(reason),
                });
                v.health = 0;
                v.is_alive = false;
                sim.health = 0;
                sim.is_alive = false;
            }
            _ => return,
        }
        if !self.pending_external_state_changed_vehicles.contains(&vehicle_id) {
            self.pending_external_state_changed_vehicles.push(vehicle_id);
        }
        if !self.last_state_changed_vehicles.contains(&vehicle_id) {
            self.last_state_changed_vehicles.push(vehicle_id);
        }
    }

    pub fn bind_observer(&mut self, vehicle_id: i32, target_id: i32, position: Vec3) {
        if target_id == 0 {
            self.move_observer(vehicle_id, position);
            return;
        }
        let Some(observer) = self.vehicle_by_id(vehicle_id) else {
            return;
        };
        let Some(target) = self.vehicle_by_id(target_id) else {
            return;
        };
        if observer.team == target.team && (!observer.is_alive || vehicle_id == target_id) {
            let fallback = target.position;
            self.observer_focus.entry(vehicle_id).or_default().bind_to(target_id, fallback);
        }
    }

    fn clamp_observer_position(&self, position: Vec3) -> Vec3 {
        let (x, z) = clamp_xz_to_box(self.map.def.bounding_box, position.x, position.z);
        Vec3::new(x, position.y.clamp(-500.0, 500.0), z)
    }

    pub fn set_auto_aim(&mut self, vehicle_id: i32, target_id: i32) -> Option<u8> {
        let target = if target_id == 0 {
            Ok(None)
        } else {
            self.auto_aim_position(vehicle_id, target_id).map(Some)
        };
        let aim = self.aim_states.get_mut(&vehicle_id)?;
        aim.auto_aim_target = matches!(target, Ok(Some(_))).then_some(target_id);
        aim.has_target_point = matches!(target, Ok(Some(_)));
        if let Ok(Some(position)) = target {
            aim.target_point = position;
        }
        target.err()
    }

    fn auto_aim_position(&self, vehicle_id: i32, target_id: i32) -> Result<Vec3, u8> {
        const TOO_FAR: u8 = 1;
        const INVISIBLE: u8 = 2;
        const KILLED: u8 = 3;
        let observer = self.vehicle_by_id(vehicle_id).filter(|v| v.is_alive).ok_or(INVISIBLE)?;
        let target = self.vehicle_by_id(target_id).filter(|v| v.team != observer.team).ok_or(INVISIBLE)?;
        if !target.is_alive {
            return Err(KILLED);
        }
        if observer.position.distance_squared(target.position) > VEHICLE_AOI_RADIUS * VEHICLE_AOI_RADIUS {
            return Err(TOO_FAR);
        }
        if !self.last_visibility.spotted_enemies.get(&vehicle_id).is_some_and(|set| set.contains(&target_id)) {
            return Err(INVISIBLE);
        }
        Ok(target.position + Vec3::Y)
    }

    pub fn apply_client_position(
        &mut self,
        account_db_id: i64,
        pos: [f32; 3],
        yaw: f32,
        pitch: f32,
        roll: f32,
    ) {
        if !pos.iter().all(|v| v.is_finite())
            || !yaw.is_finite()
            || !pitch.is_finite()
            || !roll.is_finite()
        {
            return;
        }
        let (x, z) = clamp_xz_to_box(self.map.def.bounding_box, pos[0], pos[2]);
        if let Some(v) = self
            .vehicles
            .iter_mut()
            .find(|v| v.account_db_id == account_db_id && !v.left_arena && v.is_alive)
        {
            let y = self
                .map
                .terrain
                .sample_height_clamped(x, z)
                .unwrap_or(pos[1]);
            v.position = Vec3::new(x, y, z);
            v.yaw = yaw;
            v.pitch = pitch;
            v.roll = roll;
            if let Some(sim) = self.sim_states.get_mut(&v.entity_id) {
                sim.physics.pos = v.position;
                sim.physics.yaw = yaw;
                sim.physics.pitch = pitch;
                sim.physics.roll = roll;
            }
        }
    }

    pub fn set_vehicle_target_point(&mut self, account_db_id: i64, target_point: Vec3) {
        if !target_point.is_finite() || target_point.abs().max_element() > 100_000.0 {
            return;
        }
        if let Some(v) = self
            .vehicles
            .iter()
            .find(|v| v.account_db_id == account_db_id && !v.left_arena)
        {
            let aim = self.aim_states.entry(v.entity_id).or_default();
            aim.target_point = target_point;
            aim.has_target_point = true;
        }
    }

    pub fn set_vehicle_autorotation(&mut self, account_db_id: i64, enabled: bool) {
        if let Some(v) = self
            .vehicles
            .iter()
            .find(|v| v.account_db_id == account_db_id && !v.left_arena)
        {
            let aim = self.aim_states.entry(v.entity_id).or_default();
            aim.autorotation = enabled;
        }
    }

    pub fn change_vehicle_setting(&mut self, vehicle_id: i32, code: u8, val: i32) -> Option<(u8, i32)> {
        match code {
            0 => {
                // CURRENT_SHELLS
                let aim = self.aim_states.entry(vehicle_id).or_default();
                let index = aim.ammo_counts.iter().position(|&(cd, _)| cd == val)
                    .or_else(|| usize::try_from(val).ok().filter(|&i| i < aim.ammo_counts.len()))?;
                aim.active_shell_idx = index;
                aim.current_shell_cd = aim.ammo_counts[index].0;
                aim.next_shell_cd = 0;
                aim.ammo_dirty = true;
                Some((0, aim.current_shell_cd))
            }
            1 => {
                // NEXT_SHELLS
                let aim = self.aim_states.entry(vehicle_id).or_default();
                if val != 0 && !aim.ammo_counts.iter().any(|&(cd, _)| cd == val) {
                    return None;
                }
                aim.next_shell_cd = val;
                Some((1, val))
            }
            2 => {
                // AUTOROTATION_ENABLED
                let aim = self.aim_states.entry(vehicle_id).or_default();
                aim.autorotation = val != 0;
                None
            }
            16 => {
                // ACTIVATE_EQUIPMENT
                let raw_item = ((val as u32) & 0xFFFF) as u16;
                let extra_idx = ((val as u32) >> 16) as u16;
                let (item_id, _) = crate::sim::crits::normalize_equipment_id(raw_item);
                if let Some(sim) = self.sim_states.get_mut(&vehicle_id) {
                    if let Ok(events) = crate::sim::crits::activate_equipment(sim, item_id, extra_idx) {
                        if let Some(v) = self.vehicles.iter_mut().find(|v| v.entity_id == vehicle_id) {
                            v.is_burning = sim.damage.fire_state.is_some();
                            v.is_crew_active = sim.damage.is_crew_active;
                        }
                        if !self.last_state_changed_vehicles.contains(&vehicle_id) {
                            self.last_state_changed_vehicles.push(vehicle_id);
                        }
                        self.last_damage_events.extend(events.clone());
                        if !self.in_step {
                            if !self.pending_external_state_changed_vehicles.contains(&vehicle_id) {
                                self.pending_external_state_changed_vehicles.push(vehicle_id);
                            }
                            self.pending_external_damage_events.extend(events);
                        }
                    }
                }
                None
            }
            _ => Some((code, val)),
        }
    }

    pub fn step(&mut self, dt_secs: f32) {
        self.step_with_events(dt_secs);
    }

    pub fn leave_player(&mut self, account_db_id: i64) -> Option<i32> {
        let id = self.park_player(account_db_id)?;
        let v = self.vehicles.iter_mut().find(|v| v.entity_id == id)?;
        v.left_arena = true;
        if self.period < ARENA_PERIOD_BATTLE || self.mode != BattleMode::Random {
            v.is_alive = false;
            v.health = 0;
            if let Some(sim) = self.sim_states.get_mut(&id) {
                sim.is_alive = false;
                sim.health = 0;
            }
        }
        Some(id)
    }

    pub fn park_player(&mut self, account_db_id: i64) -> Option<i32> {
        let v = self
            .vehicles
            .iter_mut()
            .find(|v| v.account_db_id == account_db_id && !v.is_bot)?;
        v.move_flags = 0;
        v.speed = 0.0;
        v.rspeed = 0.0;
        v.engine_mode = [1, 0];
        self.fire_requests.remove(&v.entity_id);
        if let Some(sim) = self.sim_states.get_mut(&v.entity_id) {
            sim.physics.move_flags = 0;
            sim.physics.cruise_mode = 0;
            sim.physics.speed_mps = 0.0;
            sim.physics.rot_speed_rps = 0.0;
            sim.physics.engine_mode = v.engine_mode;
        }
        Some(v.entity_id)
    }

    pub fn active_human_account_ids(&self) -> Vec<i64> {
        self.vehicles
            .iter()
            .filter(|v| !v.is_bot && !v.left_arena)
            .map(|v| v.account_db_id)
            .collect()
    }

    pub fn all_humans_left(&self) -> bool {
        !self
            .vehicles
            .iter()
            .any(|v| !v.is_bot && !v.left_arena)
    }
}

fn configure_sim_from_catalog(
    sim: &mut crate::sim::VehicleSimState,
    catalog: &Catalog,
    comp_descr: &[u8],
    raw_eqs: &[u16],
    is_bot: bool,
) {
    let default_bot_eqs: [u16; 3] = [251, 763, 1275];
    let eqs_slice: &[u16] = if is_bot && raw_eqs.iter().all(|&e| e == 0) {
        &default_bot_eqs
    } else {
        raw_eqs
    };
    for (idx, &raw_eq) in eqs_slice.iter().enumerate() {
        if idx < 3 && raw_eq > 0 {
            let (item_id, eq_comp_descr) = crate::sim::crits::normalize_equipment_id(raw_eq);
            sim.damage.equipment_slots[idx] = Some(crate::sim::crits::EquipmentSlot {
                comp_descr: eq_comp_descr,
                id: item_id,
                name: format!("equipment_{item_id}"),
                count: 1,
                cooldown_left: 0.0,
                is_active: false,
            });
        }
    }

    if let Ok(descr) = VehicleDescr::from_bytes(comp_descr) {
        if let Some(vdef) = catalog.vehicle_def_by_id(descr.nation_id, u16::from(descr.type_id)) {
            if !vdef.tags.is_empty() {
                sim.tags = vdef.tags.clone();
            }
            if let Some((fwd_kmh, bwd_kmh)) = vdef
                .speed_limits
                .or_else(|| vdef.battle_def.as_ref().map(|b| b.speed_limits))
            {
                if fwd_kmh > 0.0 {
                    let bwd = if bwd_kmh > 0.0 { bwd_kmh } else { fwd_kmh * 0.3 };
                    sim.speed_limits = ((fwd_kmh / 3.6) as f32, (bwd / 3.6) as f32);
                }
            }
            let chassis = vdef
                .chassis
                .iter()
                .find(|c| c.id == descr.chassis_id)
                .or_else(|| vdef.chassis.first());
            let engine = vdef
                .engines
                .iter()
                .find(|e| e.id == descr.engine_id)
                .or_else(|| vdef.engines.first());
            let fuel_tank = vdef
                .fuel_tanks
                .iter()
                .find(|f| f.id == descr.fuel_tank_id)
                .or_else(|| vdef.fuel_tanks.first());
            let radio = vdef
                .radios
                .iter()
                .find(|r| r.id == descr.radio_id)
                .or_else(|| vdef.radios.first());
            let turret = vdef
                .turrets
                .iter()
                .find(|t| t.component.id == descr.turret_id)
                .or_else(|| vdef.turrets.first());
            let gun = turret.and_then(|t| {
                t.guns
                    .iter()
                    .find(|g| g.component.id == descr.gun_id)
                    .or_else(|| t.guns.first())
            });

            if let Some(c) = chassis {
                if let Some(rot_deg) = c.rotation_speed {
                    if rot_deg > 0.0 {
                        sim.rotation_speed_rad = (rot_deg as f32).to_radians();
                    }
                }
                if let Some(climb) = c.max_climb_angle {
                    if climb > 0.0 {
                        sim.max_climb_angle_deg = climb as f32;
                    }
                }
                if let Some(rac) = c.rotation_is_around_center {
                    sim.rotation_is_around_center = rac;
                }
                if let Some(trcp) = c.top_right_carrying_point {
                    if trcp[0] > 0.0 {
                        sim.track_center_offset = trcp[0] as f32;
                    }
                }
            }
            if let Some(e) = engine {
                if let Some(power) = e.engine_power {
                    if power > 0.0 {
                        sim.engine_power_hp = power as f32;
                    }
                }
            }
            let total_weight = vdef.hull_weight
                + chassis.map_or(0.0, |c| c.weight)
                + turret.map_or(0.0, |t| t.component.weight)
                + gun.map_or(0.0, |g| g.component.weight)
                + engine.map_or(0.0, |e| e.weight)
                + fuel_tank.map_or(0.0, |f| f.weight)
                + radio.map_or(0.0, |r| r.weight);
            if total_weight > 1000.0 {
                sim.mass_kg = total_weight as f32;
                sim.default_weight = total_weight as f32;
            }
            if let Some(bdef) = &vdef.battle_def {
                sim.camo_moving = bdef.invisibility.0 as f32;
                sim.camo_still = bdef.invisibility.1 as f32;
            } else if let Some(invis) = vdef.invisibility {
                sim.camo_moving = invis.0 as f32;
                sim.camo_still = invis.1 as f32;
            }
            if let Some(t) = turret {
                if let Some(cvr) = t.circular_vision_radius {
                    sim.circular_vision_radius = cvr as f32;
                }
            }
            if let Some(r) = radio {
                if let Some(rd) = r.radio_distance {
                    sim.radio_distance_m = rd as f32;
                }
            }
        }
    }

    let default_descr = VehicleDescr {
        nation_id: 0,
        type_id: 0,
        chassis_id: 0,
        engine_id: 0,
        fuel_tank_id: 0,
        radio_id: 0,
        turret_id: 0,
        gun_id: 0,
        optional_devices: [None, None, None],
        custom_emblem_id: None,
        horn_id: None,
        camouflage_id: None,
    };
    let descr_ref = sim.descr.as_ref().unwrap_or(&default_descr);
    sim.modifiers = crate::sim::crits::compute_effective_modifiers(
        descr_ref,
        &sim.crew,
        &sim.damage,
    );
}

fn resolve_max_health(catalog: &Catalog, comp_descr: &[u8]) -> i16 {
    let Ok(descr) = VehicleDescr::from_bytes(comp_descr) else {
        return 100;
    };
    let Some(vdef) = catalog.vehicle_def_by_id(descr.nation_id, u16::from(descr.type_id)) else {
        return 100;
    };
    let turret_hp = vdef
        .turrets
        .iter()
        .find(|t| t.component.id == descr.turret_id)
        .or_else(|| vdef.turrets.first())
        .map(|t| t.max_health)
        .unwrap_or(0);
    (vdef.hull_max_health + turret_hp).clamp(1, i16::MAX as i32) as i16
}

/// Hit testers of the vehicle `comp_descr` names and the hull footprint they span.
fn load_hull_collision(
    cache: &VehicleCollisionCache,
    catalog: &Catalog,
    comp_descr: &[u8],
) -> Option<(Arc<VehicleCollisionSet>, crate::sim::collision::Footprint)> {
    let descr = VehicleDescr::from_bytes(comp_descr).ok()?;
    let def = catalog.vehicle_def_by_id(descr.nation_id, u16::from(descr.type_id))?;
    let set = cache.get_or_load_by_ids(def, descr.chassis_id, descr.turret_id, descr.gun_id).ok()?;
    let hull_position = def
        .chassis
        .iter()
        .find(|c| c.id == descr.chassis_id)
        .and_then(|c| c.hull_position)
        .map_or(Vec3::ZERO, |p| Vec3::new(p[0] as f32, p[1] as f32, p[2] as f32));
    let footprint = crate::sim::collision::Footprint::from_hit_boxes(
        set.chassis.local_aabb(),
        set.hull.local_aabb(),
        hull_position,
    );
    Some((set, footprint))
}

fn resolve_combat_profile(catalog: &Catalog, comp_descr: &[u8]) -> VehicleCombatProfile {
    let mut profile = VehicleCombatProfile::default();
    let Ok(descr) = VehicleDescr::from_bytes(comp_descr) else {
        return profile;
    };
    profile.chassis_id = descr.chassis_id;
    profile.turret_id = descr.turret_id;
    profile.gun_id = descr.gun_id;

    let Some(vdef) = catalog.vehicle_def_by_id(descr.nation_id, u16::from(descr.type_id)) else {
        return profile;
    };
    profile.veh_type_cd = vdef.veh_type_comp_descr;

    let chassis = vdef.chassis.iter().find(|c| c.id == descr.chassis_id).or_else(|| vdef.chassis.first());
    let turret = vdef.turrets.iter().find(|t| t.component.id == descr.turret_id).or_else(|| vdef.turrets.first());
    let gun = turret.and_then(|t| t.guns.iter().find(|g| g.component.id == descr.gun_id).or_else(|| t.guns.first()));

    // Positions
    if let Some(pos) = vdef.hull_position.or_else(|| chassis.and_then(|c| c.hull_position)) {
        profile.hull_pos = Vec3::new(pos[0] as f32, pos[1] as f32, pos[2] as f32);
    }
    if let Some(pos) = vdef.turret_positions.first() {
        profile.turret_pos = Vec3::new(pos[0] as f32, pos[1] as f32, pos[2] as f32);
    }
    if let Some(pos) = turret.and_then(|t| t.gun_position) {
        profile.gun_pos = Vec3::new(pos[0] as f32, pos[1] as f32, pos[2] as f32);
    }

    // Limits & rotation speeds
    if let Some(t) = turret {
        if let Some((min_y, max_y)) = t.yaw_limits {
            profile.turret_yaw_limits = Some(((min_y as f32).to_radians(), (max_y as f32).to_radians()));
        }
        if let Some(rot) = t.rotation_speed {
            if rot > 0.0 {
                profile.turret_rot_speed = (rot as f32).to_radians();
            }
        }
    }

    if let Some(g) = gun {
        if let Some((min_p, max_p)) = g.pitch_limits {
            profile.gun_pitch_limits = Some(((min_p as f32).to_radians(), (max_p as f32).to_radians()));
        }
        if let Some(rot) = g.rotation_speed {
            if rot > 0.0 {
                profile.gun_rot_speed = (rot as f32).to_radians();
            }
        }
        if let Some(aim) = g.aiming_time {
            profile.base_aiming_time = aim as f32;
        }
        if let Some(disp) = g.shot_disp_radius {
            profile.base_disp_angle = (disp as f32) / 100.0;
        }
        if let Some(rel) = g.reload_time {
            profile.reload_time = rel as f32;
        }
        if let Some(shot) = g.shots.first() {
            profile.default_shot_speed = shot.speed as f32;
            profile.default_shot_gravity = if shot.gravity > 0.0 { shot.gravity as f32 } else { 9.81 };
            profile.primary_shot = Some(shot.clone());
            profile.primary_shell = catalog.resolve_shell(shot.shell_cd).cloned();
        }
        profile.all_shots = g.shots.clone();
        profile.all_shells = g
            .shots
            .iter()
            .filter_map(|s| catalog.resolve_shell(s.shell_cd).cloned())
            .collect();
    }

    // Dispersion factors: [speed, rspeed, turret_rot, after_shot]
    let (s_factor, rs_factor) = chassis.and_then(|c| c.shot_dispersion_factors).map(|f| (f[0] as f32, f[1] as f32)).unwrap_or((0.2, 0.2));
    let (t_rot_factor, after_shot_factor) = gun.and_then(|g| g.shot_dispersion_factors).map(|f| (f[0] as f32, f[1] as f32)).unwrap_or((0.16, 4.0));
    profile.disp_factors = [s_factor, rs_factor, t_rot_factor, after_shot_factor];

    // Armor maps
    profile.chassis_armor = chassis.and_then(|c| c.armor.clone());
    profile.hull_armor = vdef.hull_armor.clone();
    profile.turret_armor = turret.and_then(|t| t.armor.clone());
    profile.gun_armor = gun.and_then(|g| g.armor.clone());

    profile
}

fn spawn_pose(map: &ArenaMap, team: u8, occupied: &[Vec3]) -> (Vec3, f32) {
    const SPACING: f32 = 6.5;
    let (lower, upper) = map.def.bounding_box;
    let midpoint = (glam::Vec2::from_array(lower) + glam::Vec2::from_array(upper)) * 0.5;
    let half_width = (upper[0] - lower[0]).abs() * 0.5;
    let side = if team == 1 { -1.0 } else { 1.0 };
    let fallback = (
        Vec3::new(midpoint.x + half_width * side * 0.5, 0.0, midpoint.y),
        -side * std::f32::consts::FRAC_PI_2,
    );
    let mut seeds: Vec<_> = map.spawns_for_team(team)
        .into_iter().map(|sp| (sp.position, sp.yaw)).collect();
    if seeds.is_empty() {
        seeds.push(fallback);
    }
    let grounded = |p: Vec3| {
        let (x, z) = clamp_xz_to_box(map.def.bounding_box, p.x, p.z);
        Vec3::new(x, map.terrain.sample_height_clamped(x, z).unwrap_or(p.y), z)
    };
    let available = |p: Vec3| {
        if occupied.iter().any(|q| (p.x - q.x).powi(2) + (p.z - q.z).powi(2) < SPACING.powi(2) - 0.01) {
            return false;
        }
        let radius = 2.0;
        map.query_obstacles_aabb([p.x - radius, p.z - radius], [p.x + radius, p.z + radius])
            .iter().all(|obs| {
                let min = obs.world_aabb_min;
                let max = obs.world_aabb_max;
                max.x <= p.x - radius || min.x >= p.x + radius
                    || max.z <= p.z - radius || min.z >= p.z + radius
                    || max.y <= p.y + 0.12 || min.y >= p.y + 2.5
            })
    };

    for &(seed, yaw) in &seeds {
        let pose = grounded(seed);
        if available(pose) {
            return (pose, yaw);
        }
    }
    for ring in 1..=8 {
        for &(seed, yaw) in &seeds {
            let (sin, cos) = yaw.sin_cos();
            let right = Vec3::new(cos, 0.0, -sin);
            let forward = Vec3::new(sin, 0.0, cos);
            for depth in 0_i32..=ring {
                for lane in -ring..=ring {
                    if depth != ring && lane.abs() != ring {
                        continue;
                    }
                    let pose = grounded(seed + (right * lane as f32 - forward * depth as f32) * SPACING);
                    if available(pose) {
                        return (pose, yaw);
                    }
                }
            }
        }
    }
    (grounded(seeds[0].0), seeds[0].1)
}

fn clamp_xz_to_box(bounding_box: ([f32; 2], [f32; 2]), x: f32, z: f32) -> (f32, f32) {
    let (bl, ur) = bounding_box;
    let (min_x, max_x) = (bl[0].min(ur[0]), bl[0].max(ur[0]));
    let (min_z, max_z) = (bl[1].min(ur[1]), bl[1].max(ur[1]));
    (x.clamp(min_x, max_x), z.clamp(min_z, max_z))
}

/// Registry of all running battle arenas keyed by `space_id`.
#[derive(Debug, Default)]
pub struct BattleWorld {
    arenas: HashMap<i32, ArenaInstance>,
    account_to_space: HashMap<i64, i32>,
}

impl BattleWorld {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn spawn_arena(
        &mut self,
        space_id: i32,
        map: Arc<ArenaMap>,
        spec: &ArenaLaunchSpec,
        catalog: &Catalog,
        alloc_entity_id: &mut impl FnMut() -> i32,
    ) -> &ArenaInstance {
        let instance = ArenaInstance::spawn(space_id, map, spec, catalog, alloc_entity_id);
        for v in &instance.vehicles {
            if !v.is_bot {
                self.account_to_space.insert(v.account_db_id, space_id);
            }
        }
        self.arenas.insert(space_id, instance);
        self.arenas.get(&space_id).expect("just inserted")
    }

    pub fn get(&self, space_id: i32) -> Option<&ArenaInstance> {
        self.arenas.get(&space_id)
    }

    pub fn get_mut(&mut self, space_id: i32) -> Option<&mut ArenaInstance> {
        self.arenas.get_mut(&space_id)
    }

    pub fn join_arena(
        &mut self,
        space_id: i32,
        id: i32,
        player: ParticipantSpec,
        catalog: &Catalog,
    ) -> Option<Vec<u8>> {
        let db_id = player.account_db_id;
        if self.account_to_space.contains_key(&db_id) {
            return None;
        }
        let arena = self.arenas.get_mut(&space_id)?;
        let (_, payload) = arena.add_participant(id, player, catalog);
        self.account_to_space.insert(db_id, space_id);
        Some(payload)
    }

    pub fn arena_for_account(&self, account_db_id: i64) -> Option<&ArenaInstance> {
        let space_id = self.account_to_space.get(&account_db_id)?;
        self.arenas.get(space_id)
    }

    pub fn arena_for_account_mut(&mut self, account_db_id: i64) -> Option<&mut ArenaInstance> {
        let space_id = self.account_to_space.get(&account_db_id)?;
        self.arenas.get_mut(space_id)
    }

    pub fn step_all(&mut self, dt_secs: f32) {
        for arena in self.arenas.values_mut() {
            arena.step(dt_secs);
        }
    }

    pub fn advance_all(&mut self, elapsed: Duration) {
        for arena in self.arenas.values_mut() {
            arena.advance(elapsed);
        }
    }

    pub fn active_space_ids(&self) -> Vec<i32> {
        self.arenas.keys().copied().collect()
    }

    /// Remove `account_db_id` from its active arena.
    ///
    /// Returns `Some((space_id, player_vehicle_id, reclaimed_entity_ids))`. If the last human
    /// left a waiting or training arena, its entities can be reclaimed immediately. A started
    /// Random battle stays alive until it finishes and its results are settled.
    pub fn leave_player(&mut self, account_db_id: i64) -> Option<(i32, i32, Vec<i32>)> {
        let space_id = self.account_to_space.remove(&account_db_id)?;
        let arena = self.arenas.get_mut(&space_id)?;
        let vehicle_id = arena.leave_player(account_db_id)?;
        let reclaimed = if arena.all_humans_left()
            && (arena.mode != BattleMode::Random || arena.period < ARENA_PERIOD_BATTLE)
        {
            let removed = self.arenas.remove(&space_id).unwrap();
            removed.vehicles.into_iter().map(|v| v.entity_id).collect()
        } else {
            Vec::new()
        };
        Some((space_id, vehicle_id, reclaimed))
    }

    pub fn remove_arena(&mut self, space_id: i32) -> Vec<i32> {
        let Some(arena) = self.arenas.remove(&space_id) else { return Vec::new() };
        self.account_to_space.retain(|_, id| *id != space_id);
        arena.vehicles.into_iter().map(|vehicle| vehicle.entity_id).collect()
    }

    /// Clean up stale `account_to_space` membership when an arena or player vehicle vanished
    /// before `ENABLE_ENTITIES` completed.
    ///
    /// If `space_id` still exists and no active human vehicles remain, the arena is removed and
    /// its vehicle entity IDs are returned so the caller can release them cleanly.
    pub fn cleanup_orphan_player(&mut self, account_db_id: i64, space_id: i32) -> Vec<i32> {
        if self.account_to_space.get(&account_db_id) == Some(&space_id) {
            self.account_to_space.remove(&account_db_id);
        }
        let Some(arena) = self.arenas.get_mut(&space_id) else {
            return Vec::new();
        };
        let _ = arena.leave_player(account_db_id);
        if !arena.all_humans_left() {
            return Vec::new();
        }
        let removed = self.arenas.remove(&space_id).unwrap();
        removed.vehicles.into_iter().map(|v| v.entity_id).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;
    use std::path::PathBuf;
    use glam::Mat4;
    use wot065_data::catalog;
    use wot065_data::descr::VehicleDescr;
    use wot065_data::pickle;
    use crate::assets::arena_defs::ArenaDef;
    use crate::assets::chunk::{ChunkCoords, ChunkKind, SpaceBounds, SpawnPoint};
    use crate::assets::map_store::TerrainMap;
    use crate::assets::terrain::{
        MaterialKinds, TerrainChunk, DOMINANT_TEX_GRID_SIZE, HEIGHTMAP_GRID_SIZE,
    };
    use crate::balance::BalanceConfig;
    use crate::lobby::{BattleMode, ParticipantSpec, ARENA_BONUS_TYPE_REGULAR, ARENA_GUI_TYPE_RANDOM};

    fn make_flat_map() -> Arc<ArenaMap> {
        let def = ArenaDef {
            id: 3,
            name: "04_himmelsdorf".into(),
            display_name_key: "Himmelsdorf".into(),
            description_key: "".into(),
            geometry: "spaces/04_himmelsdorf".into(),
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
            SpawnPoint { team: 1, preferable_type: None, world_transform: Mat4::IDENTITY, position: Vec3::new(-40.0, 5.0, -40.0), yaw: 0.5 },
            SpawnPoint { team: 2, preferable_type: None, world_transform: Mat4::IDENTITY, position: Vec3::new(40.0, 5.0, 40.0), yaw: -2.5 },
        ];
        Arc::new(ArenaMap::from_parts(
            def, bounds, terrain, mats, vec![], vec![], vec![], spawns, vec![], BTreeMap::new(), &BalanceConfig::default(),
        ))
    }

    fn minimal_catalog(name: &str) -> (Catalog, Vec<u8>, PathBuf) {
        let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../target/test-tmp")
            .join(format!("arena_{name}_{}", std::process::id()));
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
            std::fs::write(cdir.join("shells.xml"), "<root><AP><id>1</id><price>5</price><kind>ARMOR_PIERCING</kind><caliber>20</caliber><isTracer>true</isTracer><effects>smallAP</effects><damage><armor>12</armor><devices>20</devices></damage></AP></root>").unwrap();
            std::fs::write(cdir.join("chassis.xml"), "<root><ids><Ch>1</Ch></ids><shared><Ch><weight>1000</weight><level>1</level><price>100</price><maxLoad>10000</maxLoad></Ch></shared></root>").unwrap();
            std::fs::write(cdir.join("engines.xml"), "<root><ids><Eng>1</Eng></ids><shared><Eng><weight>500</weight><level>1</level><price>100</price><power>100</power></Eng></shared></root>").unwrap();
            std::fs::write(cdir.join("fuelTanks.xml"), "<root><ids><Fuel>1</Fuel></ids><shared><Fuel><weight>100</weight><level>1</level><price>50</price></Fuel></shared></root>").unwrap();
            std::fs::write(cdir.join("radios.xml"), "<root><ids><Rad>1</Rad></ids><shared><Rad><weight>50</weight><level>1</level><price>50</price><distance>300</distance></Rad></shared></root>").unwrap();
            std::fs::write(cdir.join("guns.xml"), "<root><ids><G>1</G></ids><shared><G><level>1</level><price>100</price><weight>50</weight><maxAmmo>50</maxAmmo><pitchLimits>-10 20</pitchLimits><rotationSpeed>30</rotationSpeed><reloadTime>2.0</reloadTime><aimingTime>1.5</aimingTime><shotDispersionRadius>0.4</shotDispersionRadius><effects>small</effects><shots><AP><defaultPortion>1.0</defaultPortion><speed>600</speed><gravity>9.81</gravity><maxDistance>400</maxDistance><piercingPower>30 20</piercingPower></AP></shots></G></shared></root>").unwrap();
            std::fs::write(cdir.join("turrets.xml"), "<root><ids><T>1</T></ids><shared><T><level>1</level><price>100</price><weight>200</weight><maxHealth>20</maxHealth><yawLimits>-180 180</yawLimits><guns><G>shared</G></guns></T></shared></root>").unwrap();
        }
        let cat = catalog::load_from_vehicles_dir(&dir).unwrap();
        let descr = VehicleDescr::stock(cat.vehicle_def_by_id(0, 0).unwrap()).unwrap().to_bytes();
        (cat, descr, dir)
    }

    #[test]
    fn retail_collision_cache_gives_a_spawned_hull_its_own_footprint() {
        let Some(res_dir) = crate::assets::retail_res_dir() else {
            return;
        };
        let game_dir = wot065_data::testutil::game_dir().expect("game_dir present");
        let cat = catalog::load(&game_dir).expect("load retail catalog");
        let ms1 = cat.by_name("MS-1").expect("MS-1 in catalog");
        let ms1_def = cat.vehicle_def_by_id(ms1.nation_id, ms1.type_id).expect("MS-1 VehicleDef");
        let spec = ArenaLaunchSpec {
            arena_type_id: 3,
            arena_gui_type: ARENA_GUI_TYPE_RANDOM,
            arena_bonus_type: ARENA_BONUS_TYPE_REGULAR,
            mode: BattleMode::Random,
            round_length_secs: 900,
            prebattle_id: 0,
            participants: vec![ParticipantSpec {
                account_db_id: 101,
                player_name: "Alice".into(),
                veh_inv_id: 1,
                veh_comp_descr: VehicleDescr::stock(ms1_def).unwrap().to_bytes(),
                team: 1,
                ..Default::default()
            }],
        };
        let mut next_id = 100i32;
        let mut arena = ArenaInstance::spawn(1, make_flat_map(), &spec, &cat, &mut || {
            next_id += 1;
            next_id
        });
        let entity_id = arena.vehicles[0].entity_id;

        arena.set_collision_cache(Arc::new(VehicleCollisionCache::new(&res_dir)), Some(&cat));

        let footprint = arena.sim_states[&entity_id].footprint;
        let half_length = footprint.max.y.max(-footprint.min.y);
        assert!((1.8..=2.8).contains(&half_length), "MS-1 half length {half_length} in {footprint:?}");
        assert!((0.7..=1.2).contains(&footprint.half_width()), "MS-1 half width in {footprint:?}");
    }

    #[test]
    fn arena_spawns_vehicles_emits_pickles_steps_and_cleans_up_on_leave() {
        let map = make_flat_map();
        let (cat, comp_descr, dir) = minimal_catalog("spawn");
        let spec = ArenaLaunchSpec {
            arena_type_id: 3,
            arena_gui_type: ARENA_GUI_TYPE_RANDOM,
            arena_bonus_type: ARENA_BONUS_TYPE_REGULAR,
            mode: BattleMode::Random,
            round_length_secs: 900,
            prebattle_id: 0,
            participants: vec![
                ParticipantSpec {
                    account_db_id: 101,
                    player_name: "Alice".into(),
                    veh_inv_id: 1,
                    veh_comp_descr: comp_descr.clone(),
                    team: 1,
                    prebattle_id: 0,
                    is_bot: false,
                    ..Default::default()
                },
                ParticipantSpec {
                    account_db_id: -1,
                    player_name: "Bot_1".into(),
                    veh_inv_id: -1,
                    veh_comp_descr: comp_descr,
                    team: 2,
                    prebattle_id: 0,
                    is_bot: true,
                    ..Default::default()
                },
            ],
        };

        let mut next_id = 100i32;
        let mut world = BattleWorld::new();
        world.spawn_arena(1, map, &spec, &cat, &mut || {
            let id = next_id;
            next_id += 1;
            id
        });

        let arena = world.arena_for_account(101).unwrap();
        let v1 = arena.vehicle_for_account(101).unwrap();
        assert_eq!(v1.max_health, 100); // hull 80 + turret 20
        assert!((v1.position.x + 40.0).abs() < 1e-4);
        assert!((v1.position.y - 5.0).abs() < 1e-4);

        let vlist = pickle::loads(&arena.vehicle_list_pickle()).unwrap();
        assert_eq!(vlist.as_list().unwrap().len(), 2);

        let arena_mut = world.arena_for_account_mut(101).unwrap();
        let updates = arena_mut.mark_client_ready(101);
        assert!(updates.iter().any(|(u, _)| *u == ARENA_UPDATE_AVATAR_READY));
        assert!(updates.iter().any(|(u, _)| *u == ARENA_UPDATE_PERIOD));
        assert_eq!(arena_mut.period, ARENA_PERIOD_BATTLE);

        arena_mut.set_vehicle_move_flags(101, 1); // forward
        let start_pos = arena_mut.vehicle_for_account(101).unwrap().position;
        world.step_all(0.5);
        let moved_pos = world.arena_for_account(101).unwrap().vehicle_for_account(101).unwrap().position;
        assert!((moved_pos - start_pos).length() > 0.1);

        let (_, _, reclaimed) = world.leave_player(101).expect("leave player");
        assert!(reclaimed.is_empty());
        assert!(world.arena_for_account(101).is_none());
        assert_eq!(world.remove_arena(1).len(), 2);

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// `ClientArena.__rebuildIndexToId` numbers minimap entries by position in
    /// `sorted(arena.vehicles)`, and the arena vehicle list keeps players who left. In a
    /// Random battle the leaver's tank stays on the field, so the enemy can still spot it.
    #[test]
    fn minimap_indices_and_spotting_keep_players_who_left_a_random_battle() {
        let (cat, descr, dir) = minimal_catalog("leavers");
        let player = |account_db_id: i64, team: u8| ParticipantSpec {
            account_db_id,
            player_name: format!("P{account_db_id}"),
            veh_inv_id: account_db_id,
            veh_comp_descr: descr.clone(),
            team,
            ..Default::default()
        };
        let spec = ArenaLaunchSpec {
            arena_type_id: 3,
            arena_gui_type: ARENA_GUI_TYPE_RANDOM,
            arena_bonus_type: ARENA_BONUS_TYPE_REGULAR,
            mode: BattleMode::Random,
            round_length_secs: 900,
            prebattle_id: 0,
            participants: vec![player(101, 1), player(102, 1), player(103, 2)],
        };
        let mut next_id = 10;
        let mut arena = ArenaInstance::spawn(1, make_flat_map(), &spec, &cat, &mut || {
            next_id += 1;
            next_id
        });
        for account in [101, 102, 103] {
            arena.mark_client_ready(account);
        }
        assert_eq!(arena.period, ARENA_PERIOD_BATTLE);
        for (vehicle, z) in arena.vehicles.iter_mut().zip([0.0, 10.0, 30.0]) {
            vehicle.position = Vec3::new(0.0, 5.0, z);
        }
        let [observer, leaver, enemy] = [101, 102, 103].map(|account| {
            arena.vehicle_for_account(account).unwrap().entity_id
        });
        assert_eq!(arena.leave_player(102), Some(leaver));

        arena.step(0.05);

        let frame = &arena.last_visibility.minimap_frames[&observer];
        assert_eq!(frame.indices, [0, 1, 2]);
        assert_eq!(frame.positions_xz[4..], [0, 30], "index 2 is the enemy at z=30");
        assert!(arena.last_visibility.spotted_enemies[&enemy].contains(&leaver));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn fixed_frames_preserve_movement_across_network_tick_boundaries() {
        let (cat, descr, dir) = minimal_catalog("fixed_frames");
        let spec = ArenaLaunchSpec {
            arena_type_id: 3,
            arena_gui_type: 1,
            arena_bonus_type: 1,
            mode: BattleMode::Training,
            prebattle_id: 0,
            round_length_secs: 120,
            participants: vec![ParticipantSpec {
                account_db_id: 1,
                team: 1,
                veh_comp_descr: descr,
                ..Default::default()
            }],
        };
        let spawn = || ArenaInstance::spawn(1, make_flat_map(), &spec, &cat, &mut || 10);
        let mut batched = spawn();
        let mut split = spawn();
        for arena in [&mut batched, &mut split] {
            arena.mark_client_ready(1);
            arena.set_vehicle_move_flags(1, 1 | 8);
        }
        let initial = split.vehicles[0].position;
        split.step(0.025);
        assert_eq!(split.vehicles[0].position, initial);
        for _ in 1..20 {
            split.step(0.025);
        }
        batched.step(0.5);
        assert_ne!(batched.vehicles[0].position, initial);
        assert_eq!(batched.vehicles[0].position, split.vehicles[0].position);
        assert_eq!(batched.vehicles[0].yaw, split.vehicles[0].yaw);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn countdown_carries_elapsed_time_and_queued_fire_is_consumed_once() {
        let (cat, descr, dir) = minimal_catalog("fire_frame");
        let spec = ArenaLaunchSpec {
            arena_type_id: 3,
            arena_gui_type: 1,
            arena_bonus_type: 1,
            mode: BattleMode::Random,
            prebattle_id: 0,
            round_length_secs: 2,
            participants: vec![ParticipantSpec {
                account_db_id: 1,
                team: 1,
                veh_comp_descr: descr,
                ..Default::default()
            }],
        };
        let mut arena = ArenaInstance::spawn(1, make_flat_map(), &spec, &cat, &mut || 10);
        arena.prebattle_countdown_secs = 1;
        arena.mark_client_ready(1);
        let initial_position = arena.vehicles[0].position;
        let initial_ammo = arena.aim_states[&10].ammo_counts[0].1;
        arena.set_vehicle_move_flags(1, 1);
        assert!(!arena.request_fire(1));
        arena.advance(Duration::from_millis(1_150));
        assert_ne!(arena.vehicles[0].position, initial_position);
        assert_eq!(arena.period_end_time, 3.0);
        assert_eq!(arena.statistics[&10].shots, 0);
        assert!(arena.request_fire(1));
        assert!(!arena.request_fire(1));
        arena.advance(Duration::from_millis(25));
        assert_eq!(arena.aim_states[&10].ammo_counts[0].1, initial_ammo);
        arena.advance(Duration::from_millis(75));
        assert_eq!(arena.aim_states[&10].ammo_counts[0].1, initial_ammo - 1);
        assert_eq!(arena.last_shots.len(), 1);
        assert_eq!(arena.statistics[&10].shots, 1);
        assert!(!arena.request_fire(1));
        arena.advance(Duration::from_secs(2));
        assert_eq!(arena.finished, Some(ArenaFinished { winner_team: 0, reason: FinishReason::Timeout }));
        assert_eq!(arena.period_end_time, 13.0);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn extermination_and_timeout_end_arena_once() {
        let (cat, descr, dir) = minimal_catalog("finish");
        let spec = ArenaLaunchSpec {
            arena_type_id: 3,
            arena_gui_type: 1,
            arena_bonus_type: 1,
            mode: BattleMode::Random,
            prebattle_id: 0,
            round_length_secs: 2,
            participants: (1..=2)
                .map(|team| crate::lobby::ParticipantSpec {
                    account_db_id: i64::from(team),
                    player_name: format!("player{team}"),
                    veh_inv_id: 1,
                    veh_comp_descr: descr.clone(),
                    team,
                    ..Default::default()
                })
                .collect(),
        };
        for exterminated in [true, false] {
            let mut next_id = 10;
            let mut arena = ArenaInstance::spawn(1, make_flat_map(), &spec, &cat, &mut || {
                next_id += 1;
                next_id
            });
            arena.mark_client_ready(1);
            arena.mark_client_ready(2);
            if exterminated {
                arena.vehicles[1].is_alive = false;
                arena.vehicles[1].health = 0;
            }
            arena.step(if exterminated { 0.1 } else { 2.0 });
            assert_eq!(arena.period, ARENA_PERIOD_AFTERBATTLE);
            let first_period = arena.period_pickle();
            let period = pickle::loads(&first_period).unwrap();
            let fields = period.as_list().unwrap();
            assert_eq!(fields[2], Value::Int(10));
            assert_eq!(
                fields[3],
                Value::tuple(vec![
                    Value::Int(if exterminated { 1 } else { 0 }),
                    Value::Int(if exterminated { 1 } else { 3 }),
                ])
            );
            arena.step(0.5);
            assert_eq!(arena.period_pickle(), first_period);
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_module_only_hit_breaks_capture_progress() {
        let (cat, descr, dir) = minimal_catalog("capture_module_damage");
        let spec = ArenaLaunchSpec {
            arena_type_id: 3,
            arena_gui_type: 1,
            arena_bonus_type: 1,
            mode: BattleMode::Random,
            prebattle_id: 0,
            round_length_secs: 120,
            participants: (1..=2).map(|team| ParticipantSpec {
                account_db_id: i64::from(team),
                team,
                veh_comp_descr: descr.clone(),
                ..Default::default()
            }).collect(),
        };
        let mut next = 5;
        let mut arena = ArenaInstance::spawn(1, make_flat_map(), &spec, &cat, &mut || {
            next += 1;
            next
        });
        arena.start_battle();
        let capper = arena.vehicles[0].entity_id;
        let defender = arena.vehicles[1].entity_id;
        arena.capture = BaseCaptureTracker::new(&[crate::assets::TeamBaseSpec {
            team: 2, position: arena.vehicles[0].position, radius: 5.0,
            points_per_second: 1.0, max_points_per_second: 3.0, stop_invaders: false,
        }]);
        let vehicles: Vec<_> = arena.sim_states.values().cloned().collect();
        arena.update_battle_lifecycle(&vehicles, 10.0);
        assert_eq!(arena.capture.points_for(capper), 10.0);
        for module_damage in [false, true] {
            arena.battle_damage.push(BattleDamage {
                attacker_id: defender, target_id: capper, hp: 0, module_damage, death: None,
            });
            arena.update_battle_lifecycle(&vehicles, 0.0);
            assert_eq!(arena.capture.points_for(capper), if module_damage { 0.0 } else { 10.0 });
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn apply_client_position_clamps_to_map_bounding_box() {
        let map = make_flat_map(); // bounding_box = ([-100.0, -100.0], [100.0, 100.0])
        let (cat, comp_descr, dir) = minimal_catalog("clamp_pos");
        let spec = ArenaLaunchSpec {
            arena_type_id: 3,
            arena_gui_type: ARENA_GUI_TYPE_RANDOM,
            arena_bonus_type: ARENA_BONUS_TYPE_REGULAR,
            mode: BattleMode::Random,
            round_length_secs: 900,
            prebattle_id: 0,
            participants: vec![ParticipantSpec {
                account_db_id: 101,
                player_name: "Alice".into(),
                veh_inv_id: 1,
                veh_comp_descr: comp_descr,
                team: 1,
                prebattle_id: 0,
                is_bot: false,
                ..Default::default()
            }],
        };

        let mut next_id = 100i32;
        let mut world = BattleWorld::new();
        world.spawn_arena(1, map, &spec, &cat, &mut || {
            let id = next_id;
            next_id += 1;
            id
        });

        let arena = world.arena_for_account_mut(101).unwrap();
        arena.apply_client_position(101, [999.0, 5.0, -999.0], 0.5, 0.1, -0.1);
        let v = arena.vehicle_for_account(101).unwrap();
        assert!(
            (-100.0..=100.0).contains(&v.position.x) && (-100.0..=100.0).contains(&v.position.z),
            "position should be clamped inside map bounding box, got {:?}",
            v.position
        );
        assert!((v.position.x - 100.0).abs() < 1e-4);
        assert!((v.position.z + 100.0).abs() < 1e-4);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn arena_step_applies_ramming_damage_and_reduces_vehicle_health() {
        let map = make_flat_map();
        let (cat, comp_descr, dir) = minimal_catalog("ramming_dmg");
        let spec = ArenaLaunchSpec {
            arena_type_id: 3,
            arena_gui_type: ARENA_GUI_TYPE_RANDOM,
            arena_bonus_type: ARENA_BONUS_TYPE_REGULAR,
            mode: BattleMode::Random,
            round_length_secs: 900,
            prebattle_id: 0,
            participants: vec![
                ParticipantSpec {
                    account_db_id: 101,
                    player_name: "Alice".into(),
                    veh_inv_id: 1,
                    veh_comp_descr: comp_descr.clone(),
                    team: 1,
                    prebattle_id: 0,
                    is_bot: false,
                    ..Default::default()
                },
                ParticipantSpec {
                    account_db_id: -1,
                    player_name: "Bot_1".into(),
                    veh_inv_id: -1,
                    veh_comp_descr: comp_descr,
                    team: 2,
                    prebattle_id: 0,
                    is_bot: true,
                    ..Default::default()
                },
            ],
        };

        let mut next_id = 100i32;
        let mut world = BattleWorld::new();
        world.spawn_arena(1, map, &spec, &cat, &mut || {
            let id = next_id;
            next_id += 1;
            id
        });

        let arena = world.arena_for_account_mut(101).unwrap();
        arena.vehicles[0].position = Vec3::new(0.0, 5.0, 0.0);
        arena.vehicles[0].yaw = 0.0;
        arena.vehicles[0].speed = 10.0;
        arena.vehicles[0].move_flags = 1;

        arena.vehicles[1].position = Vec3::new(0.0, 5.0, 3.0);
        arena.vehicles[1].yaw = 0.0;
        arena.vehicles[1].speed = 0.0;
        arena.vehicles[1].move_flags = 0;

        let initial_h0 = arena.vehicles[0].health;
        let initial_h1 = arena.vehicles[1].health;

        arena.start_battle();
        arena.step(0.1);

        assert!(
            arena.vehicles[1].health < initial_h1,
            "rammed vehicle must take damage, got health {} (initial {})",
            arena.vehicles[1].health,
            initial_h1
        );
        assert!(
            arena.vehicles[0].health < initial_h0,
            "ramming vehicle must also take damage, got health {} (initial {})",
            arena.vehicles[0].health,
            initial_h0
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn arena_gun_aiming_and_projectile_combat() {
        use rand::SeedableRng;

        let map = make_flat_map();
        let (cat, comp_descr, dir) = minimal_catalog("combat_test");

        let spec = ArenaLaunchSpec {
            arena_type_id: 3,
            arena_gui_type: ARENA_GUI_TYPE_RANDOM,
            arena_bonus_type: ARENA_BONUS_TYPE_REGULAR,
            mode: BattleMode::Random,
            round_length_secs: 900,
            prebattle_id: 0,
            participants: vec![
                ParticipantSpec {
                    account_db_id: 101,
                    player_name: "Player_1".into(),
                    veh_inv_id: 1,
                    veh_comp_descr: comp_descr.clone(),
                    team: 1,
                    prebattle_id: 0,
                    is_bot: false,
                    ..Default::default()
                },
                ParticipantSpec {
                    account_db_id: -1,
                    player_name: "Bot_1".into(),
                    veh_inv_id: -1,
                    veh_comp_descr: comp_descr,
                    team: 2,
                    prebattle_id: 0,
                    is_bot: true,
                    ..Default::default()
                },
            ],
        };

        let mut next_id = 100i32;
        let mut arena = ArenaInstance::spawn(1, map, &spec, &cat, &mut || {
            let id = next_id;
            next_id += 1;
            id
        });

        // Set player at (0, 5, 0) facing +Z, target at (0, 5, 30) (Bot position)
        arena.vehicles[0].position = Vec3::new(0.0, 5.0, 0.0);
        arena.vehicles[0].yaw = 0.0;
        arena.vehicles[1].position = Vec3::new(0.0, 5.0, 30.0);
        arena.vehicles[1].yaw = std::f32::consts::PI;

        arena.set_vehicle_target_point(101, Vec3::new(0.0, 5.0, 30.0));
        let aim = arena.aim_states.get(&arena.vehicles[0].entity_id).unwrap();
        assert_eq!(aim.target_point, Vec3::new(0.0, 5.0, 30.0));

        let shot_def = ShotDef {
            shell_name: "test_ap".into(),
            shell_id: 1,
            shell_cd: 100,
            default_portion: 1.0,
            speed: 500.0,
            max_distance: 720.0,
            piercing_power: (150.0, 110.0),
            gravity: 9.81,
            shell: None,
        };
        let shell = ShellXmlDef {
            id: 1,
            name: "test_ap".into(),
            nation_id: 0,
            compact_descr: 100,
            kind: wot065_data::item_defs::ShellKind::ArmorPiercing,
            caliber: 45.0,
            is_tracer: true,
            armor_damage: 50.0,
            devices_damage: 30.0,
            damage_randomization: 0.25,
            piercing_power_randomization: 0.25,
            explosion_radius: 0.0,
            effects: "".into(),
            price: (100, 0),
            show_in_shop: true,
        };

        // Fire shot from player
        let shot_id = arena.shoot_by_entity_id(arena.vehicles[0].entity_id, 1, &shot_def, &shell, (0.0, 0.0));
        assert_eq!(shot_id, Some(1));
        assert_eq!(arena.projectiles.len(), 1);

        let mut rng = rand::rngs::StdRng::seed_from_u64(42);
        // Step combat
        let _ = arena.step_combat(0.1, None, Some(&cat), &mut rng);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn arena_step_advances_combat_and_populates_targeting_snapshots() {
        let map = make_flat_map();
        let (cat, comp_descr, dir) = minimal_catalog("step_combat_test");

        let spec = ArenaLaunchSpec {
            arena_type_id: 3,
            arena_gui_type: ARENA_GUI_TYPE_RANDOM,
            arena_bonus_type: ARENA_BONUS_TYPE_REGULAR,
            mode: BattleMode::Random,
            round_length_secs: 900,
            prebattle_id: 0,
            participants: vec![
                ParticipantSpec {
                    account_db_id: 101,
                    player_name: "Player_1".into(),
                    veh_inv_id: 1,
                    veh_comp_descr: comp_descr.clone(),
                    team: 1,
                    prebattle_id: 0,
                    is_bot: false,
                    ..Default::default()
                },
                ParticipantSpec {
                    account_db_id: -1,
                    player_name: "Bot_1".into(),
                    veh_inv_id: -1,
                    veh_comp_descr: comp_descr,
                    team: 2,
                    prebattle_id: 0,
                    is_bot: true,
                    ..Default::default()
                },
            ],
        };

        let mut next_id = 100i32;
        let mut world = BattleWorld::new();
        world.spawn_arena(1, map, &spec, &cat, &mut || {
            let id = next_id;
            next_id += 1;
            id
        });

        let arena = world.arena_for_account_mut(101).unwrap();
        arena.mark_client_ready(101);
        let p_id = arena.vehicles[0].entity_id;
        let b_id = arena.vehicles[1].entity_id;

        // Verify combat profiles were extracted from catalog
        assert_eq!(arena.combat_profiles.len(), 2);
        let prof = &arena.combat_profiles[&p_id];
        assert_eq!(prof.base_aiming_time, 1.5);
        assert_eq!(prof.reload_time, 2.0);

        arena.set_vehicle_target_point(101, Vec3::new(50.0, 5.0, 0.0));

        // Step world via BattleWorld::step_all (which calls arena.step -> step_with_events)
        world.step_all(0.2);

        let arena_after = world.arena_for_account(101).unwrap();
        // Targeting snapshots must be populated for all alive vehicles
        assert!(arena_after.targeting_snapshots.contains_key(&p_id));
        assert!(arena_after.targeting_snapshots.contains_key(&b_id));

        let snap_p = &arena_after.targeting_snapshots[&p_id];
        assert_eq!(snap_p.aiming_time, 1.5);
        // Player turret turned towards target at +X
        assert!(snap_p.turret_yaw > 0.0);

        // The bot may acquire a target on a simulation frame before the network tick ends.
        let snap_b = &arena_after.targeting_snapshots[&b_id];
        assert!(snap_b.turret_yaw.abs() <= snap_b.max_turret_rot_speed * 0.2 + 1e-5);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn reticle_bloom_on_shoot() {
        let map = make_flat_map();
        let (cat, comp_descr, dir) = minimal_catalog("reticle_bloom");

        let spec = ArenaLaunchSpec {
            arena_type_id: 3,
            arena_gui_type: ARENA_GUI_TYPE_RANDOM,
            arena_bonus_type: ARENA_BONUS_TYPE_REGULAR,
            mode: BattleMode::Random,
            round_length_secs: 900,
            prebattle_id: 0,
            participants: vec![
                ParticipantSpec {
                    account_db_id: 101,
                    player_name: "Player_1".into(),
                    veh_inv_id: 1,
                    veh_comp_descr: comp_descr,
                    team: 1,
                    prebattle_id: 0,
                    is_bot: false,
                    ..Default::default()
                },
            ],
        };

        let mut next_id = 100i32;
        let mut arena = ArenaInstance::spawn(1, map, &spec, &cat, &mut || {
            let id = next_id;
            next_id += 1;
            id
        });

        let p_id = arena.vehicles[0].entity_id;
        let aim = arena.aim_states.entry(p_id).or_default();
        aim.current_disp_factor = 1.0;

        let shot_def = ShotDef {
            shell_name: "test_ap".into(),
            shell_id: 1,
            shell_cd: 100,
            default_portion: 1.0,
            speed: 500.0,
            max_distance: 720.0,
            piercing_power: (150.0, 110.0),
            gravity: 9.81,
            shell: None,
        };
        let shell = ShellXmlDef {
            id: 1,
            name: "test_ap".into(),
            nation_id: 0,
            compact_descr: 100,
            kind: wot065_data::item_defs::ShellKind::ArmorPiercing,
            caliber: 45.0,
            is_tracer: true,
            armor_damage: 50.0,
            devices_damage: 30.0,
            damage_randomization: 0.25,
            piercing_power_randomization: 0.25,
            explosion_radius: 0.0,
            effects: "".into(),
            price: (100, 0),
            show_in_shop: true,
        };

        let shot_id = arena.shoot_by_entity_id(p_id, 10, &shot_def, &shell, (0.0, 0.0));
        assert_eq!(shot_id, Some(10));
        assert_eq!(arena.projectiles.len(), 1);

        let aim_after = &arena.aim_states[&p_id];
        // Reticle bloomed significantly (disp_factors[3] is after_shot = 4.0, sqrt(1^2 + 4^2) = 4.123)
        assert!(
            aim_after.current_disp_factor >= 4.0,
            "aim dispersion factor should bloom to >= 4.0 on shoot, got {}",
            aim_after.current_disp_factor
        );
        // Reload time set from profile (2.0s)
        assert!(aim_after.reload_left > 1.9 && aim_after.reload_left <= 2.0);

        // Cannot shoot again while reloading
        let shot_id_blocked = arena.shoot_by_entity_id(p_id, 11, &shot_def, &shell, (0.0, 0.0));
        assert_eq!(shot_id_blocked, None);

        // Advance world by 0.5s -> projectile moves, reload ticks down
        arena.start_battle();
        arena.step(0.5);
        let aim_after_step = &arena.aim_states[&p_id];
        assert!(aim_after_step.reload_left < 1.6);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn arena_crits_and_fire_tick_loop() {
        let map = make_flat_map();
        let (cat, comp_descr, dir) = minimal_catalog("arena_crits_test");

        let spec = ArenaLaunchSpec {
            arena_type_id: 3,
            arena_gui_type: ARENA_GUI_TYPE_RANDOM,
            arena_bonus_type: ARENA_BONUS_TYPE_REGULAR,
            mode: BattleMode::Random,
            round_length_secs: 900,
            prebattle_id: 0,
            participants: vec![
                ParticipantSpec {
                    account_db_id: 101,
                    player_name: "P1".into(),
                    veh_inv_id: 1,
                    veh_comp_descr: comp_descr.clone(),
                    team: 1,
                    prebattle_id: 0,
                    is_bot: false,
                    ..Default::default()
                },
                ParticipantSpec {
                    account_db_id: 102,
                    player_name: "P2".into(),
                    veh_inv_id: 2,
                    veh_comp_descr: comp_descr,
                    team: 2,
                    prebattle_id: 0,
                    is_bot: false,
                    ..Default::default()
                },
            ],
        };

        let mut next_id = 100i32;
        let mut arena = ArenaInstance::spawn(1, map, &spec, &cat, &mut || {
            let id = next_id;
            next_id += 1;
            id
        });

        let p2_id = arena.vehicles[1].entity_id;
        let p1_id = arena.vehicles[0].entity_id;
        arena.start_battle();
        arena.step(0.1); // initialize sim_states
        arena.sim_states.get_mut(&p2_id).unwrap().damage.fire_state = Some(crate::sim::ActiveFire {
            attacker_id: p1_id,
            burn_damage_per_sec: 50.0,
            elapsed_secs: 0.0,
            duration_secs: 10.0,
            tick_accumulator: 0.0,
        });

        let init_hp = arena.vehicles[1].health;
        // Step arena for 1.0s -> fire ticks and damages vehicle HP
        arena.step(1.0);

        assert!(
            arena.vehicles[1].health < init_hp,
            "vehicle HP must decrease due to active fire burn in step_with_events, was {}, now {}",
            init_hp,
            arena.vehicles[1].health
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn arena_equipment_compact_descriptors_catalog_stats_and_reciprocal_ramming_dedup() {
        let map = make_flat_map();
        let (cat, comp_descr, dir) = minimal_catalog("eq_catalog_ram_dedup");

        // Participant 0: human with Hangar equipment compact descriptors:
        // 251 = handExtinguishers (id 0), 763 = smallMedkit (id 2), 1275 = smallRepairkit (id 4)
        // Participant 1: bot with default (empty) eqs -> should get seeded with [251, 763, 1275]
        let spec = ArenaLaunchSpec {
            arena_type_id: 3,
            arena_gui_type: ARENA_GUI_TYPE_RANDOM,
            arena_bonus_type: ARENA_BONUS_TYPE_REGULAR,
            mode: BattleMode::Random,
            round_length_secs: 900,
            prebattle_id: 0,
            participants: vec![
                ParticipantSpec {
                    account_db_id: 101,
                    player_name: "Human_1".into(),
                    veh_inv_id: 1,
                    veh_comp_descr: comp_descr.clone(),
                    team: 1,
                    prebattle_id: 0,
                    is_bot: false,
                    eqs: vec![251, 763, 1275],
                    ..Default::default()
                },
                ParticipantSpec {
                    account_db_id: -1,
                    player_name: "Bot_1".into(),
                    veh_inv_id: -1,
                    veh_comp_descr: comp_descr,
                    team: 2,
                    prebattle_id: 0,
                    is_bot: true,
                    ..Default::default()
                },
            ],
        };

        let mut next_id = 100i32;
        let mut arena = ArenaInstance::spawn(1, map, &spec, &cat, &mut || {
            let id = next_id;
            next_id += 1;
            id
        });

        let human_id = arena.vehicles[0].entity_id;
        let bot_id = arena.vehicles[1].entity_id;

        // 1. Verify catalog stats, tags, modifiers, and decoded equipment slot IDs
        let human_sim = &arena.sim_states[&human_id];
        assert!(
            human_sim.tags.contains("lightTank"),
            "MS-1 must have lightTank tag from catalog, got {:?}",
            human_sim.tags
        );
        assert!(
            (human_sim.mass_kg - 4900.0).abs() < 1.0,
            "MS-1 total mass should be 4900 kg from catalog components, got {}",
            human_sim.mass_kg
        );
        assert!(
            (human_sim.engine_power_hp - 100.0).abs() < 0.1,
            "MS-1 engine power should be 100 hp from catalog, got {}",
            human_sim.engine_power_hp
        );
        assert!(
            (human_sim.speed_limits.0 - 32.0 / 3.6).abs() < 1e-3,
            "MS-1 forward speed limit should be 32/3.6 m/s, got {}",
            human_sim.speed_limits.0
        );
        assert_eq!(human_sim.damage.equipment_slots[0].as_ref().unwrap().id, 0);
        assert_eq!(human_sim.damage.equipment_slots[0].as_ref().unwrap().comp_descr, 251);
        assert_eq!(human_sim.damage.equipment_slots[1].as_ref().unwrap().id, 2);
        assert_eq!(human_sim.damage.equipment_slots[2].as_ref().unwrap().id, 4);

        let bot_sim = &arena.sim_states[&bot_id];
        assert_eq!(bot_sim.damage.equipment_slots[0].as_ref().unwrap().id, 0);
        assert_eq!(bot_sim.damage.equipment_slots[1].as_ref().unwrap().id, 2);
        assert_eq!(bot_sim.damage.equipment_slots[2].as_ref().unwrap().id, 4);

        // 2. Verify human change_vehicle_setting(veh_id, 16, ...) extinguishes fire and emits FireStopped
        arena.vehicles[0].is_burning = true;
        arena.sim_states.get_mut(&human_id).unwrap().damage.fire_state = Some(crate::sim::ActiveFire {
            attacker_id: bot_id,
            burn_damage_per_sec: 10.0,
            elapsed_secs: 0.0,
            duration_secs: 5.0,
            tick_accumulator: 0.0,
        });
        let setting_echo = arena.change_vehicle_setting(human_id, 16, 251);
        assert_eq!(setting_echo, None, "ACTIVATE_EQUIPMENT (16) must not echo updateVehicleSetting");
        assert!(!arena.vehicles[0].is_burning, "human fire must be extinguished");
        assert!(arena.last_state_changed_vehicles.contains(&human_id));
        assert!(arena.last_damage_events.iter().any(|e| {
            e.target_id == human_id && e.code == crate::sim::crits::DamageInfoCode::FireStopped
        }));

        // Verify AUTOROTATION_ENABLED (code 2) updates aim.autorotation and returns None
        assert_eq!(arena.change_vehicle_setting(human_id, 2, 1), None);
        assert!(arena.aim_states[&human_id].autorotation);

        // Verify bot consumable activation repairs broken track and emits DeviceRepaired
        {
            let bsim = arena.sim_states.get_mut(&bot_id).unwrap();
            bsim.physics.left_track_broken = true;
            bsim.damage.left_track.state = crate::sim::crits::DeviceState::Destroyed;
            bsim.damage.left_track.health = 0;
        }
        arena.start_battle();
        arena.step_with_events(0.1);
        assert!(
            !arena.sim_states[&bot_id].physics.left_track_broken,
            "bot must activate repairkit to fix broken left track"
        );
        assert!(arena.last_damage_events.iter().any(|e| {
            e.target_id == bot_id && e.code == crate::sim::crits::DamageInfoCode::DeviceRepaired
        }));

        // 3. Verify simultaneous head-on ramming between two moving vehicles applies damage once per vehicle
        arena.bot_controller.set_mode(bot_id, crate::sim::BotBehaviorMode::PassiveTarget);
        arena.vehicles[0].position = Vec3::new(0.0, 5.0, 0.0);
        arena.vehicles[0].yaw = 0.0; // facing +Z
        arena.vehicles[0].speed = 8.0;
        arena.vehicles[0].move_flags = 1;

        arena.vehicles[1].position = Vec3::new(0.0, 5.0, 3.5);
        arena.vehicles[1].yaw = std::f32::consts::PI; // facing -Z (head-on)
        arena.vehicles[1].speed = 8.0;
        arena.vehicles[1].move_flags = 1;

        let ev = arena.step_with_events(0.1);
        assert_eq!(
            ev.ramming_damages.len(),
            2,
            "simultaneous head-on collision must emit ramming damage once per vehicle (2 entries total), got {:?}",
            ev.ramming_damages
        );

        let _ = std::fs::remove_dir_all(&dir);
    }
}
