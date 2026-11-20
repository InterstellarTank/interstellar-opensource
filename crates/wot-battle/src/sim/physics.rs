//! Tracked vehicle dynamics and shared arena contacts.

use crate::assets::destructibles::DestructibleKind;
use crate::assets::map_store::{ArenaMap, ChunkDestructible};
use crate::assets::terrain::GroundStrength;
use crate::balance::BalanceConfig;
use crate::sim::collision::{hull_penetration, penetration, BandPiece, Footprint, HullBox, BAND_HIGH};
use crate::sim::destructibles::DestructiblesState;
use glam::{Vec2, Vec3};
use std::collections::{BTreeMap, BTreeSet, HashMap};
use wot065_data::descr::{TankmanDescr, VehicleDescr};

/// Normalizes an angle in radians into `[-PI, PI)`.
#[inline]
pub fn normalize_angle(mut rad: f32) -> f32 {
    let two_pi = std::f32::consts::TAU;
    let pi = std::f32::consts::PI;
    rad = (rad + pi) % two_pi;
    if rad < 0.0 {
        rad += two_pi;
    }
    rad - pi
}

/// Server-authoritative physics state for a simulated vehicle.
#[derive(Debug, Clone, PartialEq)]
pub struct VehiclePhysicsState {
    pub pos: Vec3,
    pub yaw: f32,
    pub pitch: f32,
    pub roll: f32,
    pub speed_mps: f32,
    pub rot_speed_rps: f32,
    pub move_flags: u8,
    pub cruise_mode: i8,
    pub engine_mode: [u8; 2],
    pub left_track_broken: bool,
    pub right_track_broken: bool,
}

impl Default for VehiclePhysicsState {
    fn default() -> Self {
        Self {
            pos: Vec3::ZERO,
            yaw: 0.0,
            pitch: 0.0,
            roll: 0.0,
            speed_mps: 0.0,
            rot_speed_rps: 0.0,
            move_flags: 0,
            cruise_mode: 0,
            engine_mode: [1, 0],
            left_track_broken: false,
            right_track_broken: false,
        }
    }
}

/// Dynamic attribute scaling modifiers applied from crew skills, consumables, damaged modules, and optional devices.
#[derive(Debug, Clone, PartialEq)]
pub struct EffectiveModifiers {
    pub engine_power_factor: f32,
    pub chassis_rotation_speed_factor: f32,
    pub turret_rotation_speed_factor: f32,
    pub forward_max_speed_factor: f32,
    pub backward_max_speed_factor: f32,
    pub terrain_resistance_factors: [f32; 3],
    pub antifragmentation_lining_factor: f32,
    pub ramming_damage_factor: f32,
    pub brake_force_factor: f32,
    pub repair_speed_factor: f32,
    pub gun_reload_time_factor: f32,
    pub gun_aiming_time_factor: f32,
    pub shot_dispersion_factor: f32,
    pub vision_radius_factor: f32,
    pub camo_factor: f32,
}

impl Default for EffectiveModifiers {
    fn default() -> Self {
        Self {
            engine_power_factor: 1.0,
            chassis_rotation_speed_factor: 1.0,
            turret_rotation_speed_factor: 1.0,
            forward_max_speed_factor: 1.0,
            backward_max_speed_factor: 1.0,
            terrain_resistance_factors: [1.0, 1.0, 1.0],
            antifragmentation_lining_factor: 1.0,
            ramming_damage_factor: 1.0,
            brake_force_factor: 1.0,
            repair_speed_factor: 1.0,
            gun_reload_time_factor: 1.0,
            gun_aiming_time_factor: 1.0,
            shot_dispersion_factor: 1.0,
            vision_radius_factor: 1.0,
            camo_factor: 1.0,
        }
    }
}

/// Full simulation entity state used during real-time battle ticks.
#[derive(Debug, Clone)]
pub struct VehicleSimState {
    pub entity_id: i32,
    pub team: u8,
    pub is_alive: bool,
    pub health: i32,
    pub max_health: i32,
    pub descr: Option<VehicleDescr>,
    pub crew: Vec<TankmanDescr>,
    pub physics: VehiclePhysicsState,
    pub damage: crate::sim::crits::VehicleDamageState,
    pub modifiers: EffectiveModifiers,
    pub mass_kg: f32,
    pub engine_power_hp: f32,
    pub speed_limits: (f32, f32), // (forward_mps, backward_mps)
    pub max_climb_angle_deg: f32,
    pub rotation_speed_rad: f32,
    pub rotation_is_around_center: bool,
    pub track_center_offset: f32,
    pub default_weight: f32,
    /// Hull rectangle on the ground plane, from the chassis and hull hit testers.
    pub footprint: Footprint,
    pub tags: BTreeSet<String>,
    pub camo_moving: f32,
    pub camo_still: f32,
    pub circular_vision_radius: f32,
    pub radio_distance_m: f32,
}

impl Default for VehicleSimState {
    fn default() -> Self {
        Self {
            entity_id: 0,
            team: 0,
            is_alive: true,
            health: 1000,
            max_health: 1000,
            descr: None,
            crew: Vec::new(),
            physics: VehiclePhysicsState::default(),
            damage: crate::sim::crits::VehicleDamageState::default(),
            modifiers: EffectiveModifiers::default(),
            mass_kg: 30_000.0,
            engine_power_hp: 400.0,
            speed_limits: (10.0, 4.0),
            max_climb_angle_deg: 25.0,
            rotation_speed_rad: 0.5,
            rotation_is_around_center: true,
            track_center_offset: 1.5,
            default_weight: 30_000.0,
            footprint: Footprint::centered(2.0, 2.0),
            tags: ["mediumTank".to_string()].into_iter().collect(),
            camo_moving: 0.10,
            camo_still: 0.15,
            circular_vision_radius: 350.0,
            radio_distance_m: 400.0,
        }
    }
}

/// Lightweight snapshot of another vehicle in the arena for spatial collision detection.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct VehicleSnapshot {
    pub entity_id: i32,
    pub team: u8,
    pub is_alive: bool,
    pub pos: Vec3,
    pub yaw: f32,
    pub speed_mps: f32,
    pub mass_kg: f32,
    pub footprint: Footprint,
    pub left_track_broken: bool,
    pub right_track_broken: bool,
    pub antifragmentation_lining_factor: f32,
}

impl VehicleSnapshot {
    pub fn from_sim_state(state: &VehicleSimState) -> Self {
        Self {
            entity_id: state.entity_id,
            team: state.team,
            is_alive: state.is_alive,
            pos: state.physics.pos,
            yaw: state.physics.yaw,
            speed_mps: state.physics.speed_mps,
            mass_kg: state.mass_kg,
            footprint: state.footprint,
            left_track_broken: state.physics.left_track_broken,
            right_track_broken: state.physics.right_track_broken,
            antifragmentation_lining_factor: state.modifiers.antifragmentation_lining_factor,
        }
    }
}

/// Result of stepping vehicle driving physics for one simulation delta `dt`.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct DrivingStepResult {
    pub impulse: Vec3,
    pub on_pushed: Option<(f32, f32)>,
    pub pushed_entity_id: Option<i32>,
    pub ramming_damages: Vec<(i32, i16)>,
    pub broken_destructibles: Vec<(u16, usize)>,
}

const HULL_HEIGHT: f32 = BAND_HIGH;
const CONTACT_PASSES: usize = 4;
const RAM_SPEED: f32 = 1.8;
const SKIN: f32 = 0.002;

#[derive(Clone, Copy)]
struct DriveInput {
    throttle: f32,
    steer: f32,
}

impl DriveInput {
    fn from_controls(flags: u8, cruise: i8, assist: f32) -> Self {
        let throttle = match flags & 3 {
            1 if flags & 32 != 0 => 0.25,
            1 if flags & 16 != 0 => 0.5,
            1 => 1.0,
            2 if flags & 16 != 0 => -0.5,
            2 => -1.0,
            3 => 0.0,
            _ => match cruise {
                ..=-2 => -1.0,
                -1 => -0.5,
                1 => 0.25,
                2 => 0.5,
                3.. => 1.0,
                _ => 0.0,
            },
        };
        let steer = match flags & 12 {
            4 => -1.0,
            8 => 1.0,
            12 => 0.0,
            _ => assist.clamp(-1.0, 1.0),
        };
        Self { throttle, steer }
    }
}

#[derive(Clone, Copy)]
struct Body {
    tank: VehicleSnapshot,
    start: Vec3,
    drive_end: Vec3,
    end: Vec3,
    velocity: Vec2,
}

impl Body {
    fn parked(tank: VehicleSnapshot) -> Self {
        Self {
            tank,
            start: tank.pos,
            drive_end: tank.pos,
            end: tank.pos,
            velocity: facing(tank.yaw) * tank.speed_mps,
        }
    }

    /// Hull rectangle at the end of the frame.
    fn hull(self) -> HullBox {
        self.tank.footprint.at(xz(self.end), self.tank.yaw)
    }

    fn mass(self) -> f32 {
        self.tank.mass_kg.max(1.0)
    }

    fn write_back(self, state: &mut VehiclePhysicsState, map: &ArenaMap) {
        state.pos = self.end;
        state.speed_mps = self.velocity.dot(facing(state.yaw));
        let (_, normal) = ground_at(map, state.pos);
        let forward = facing(state.yaw);
        let right = Vec2::new(forward.y, -forward.x);
        let rise = -xz(normal) / normal.y.max(0.01);
        // BigWorld: +pitch lowers the nose, +roll raises the right side.
        state.pitch = -rise.dot(forward).atan();
        state.roll = rise.dot(right).atan();
    }
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct RamContact {
    pub first: i32,
    pub second: i32,
    pub first_damage: i16,
    pub second_damage: i16,
}

#[derive(Default)]
pub(crate) struct DriveFrame {
    pub rams: Vec<RamContact>,
    pub pushes: Vec<(i32, Vec3)>,
    pub crushed: Vec<CrushContact>,
}

pub(crate) struct CrushContact {
    pub chunk: u16,
    pub index: usize,
    pub speed: f32,
    pub yaw: f32,
    pub mass_kg: f32,
}

fn xz(p: Vec3) -> Vec2 {
    Vec2::new(p.x, p.z)
}

fn facing(yaw: f32) -> Vec2 {
    let (sin, cos) = yaw.sin_cos();
    Vec2::new(sin, cos)
}

fn approach(current: f32, target: f32, max_delta: f32) -> f32 {
    current + (target - current).clamp(-max_delta, max_delta)
}

fn ground_at(map: &ArenaMap, p: Vec3) -> (f32, Vec3) {
    map.sample_height_and_normal(p.x, p.z).unwrap_or_else(|| {
        (
            map.terrain.sample_height_clamped(p.x, p.z).unwrap_or(p.y),
            Vec3::Y,
        )
    })
}

fn traction(veh: &VehicleSimState, map: &ArenaMap, balance: &BalanceConfig) -> f32 {
    let p = veh.physics.pos;
    let surface = match map
        .dominant_material_at(p.x, p.z)
        .and_then(|m| m.ground_strength)
    {
        Some(GroundStrength::Medium) => 1,
        Some(GroundStrength::Soft) => 2,
        _ => 0,
    };
    let resistance = balance.terrain_resistance_for_tags(&veh.tags);
    let on_surface =
        |i: usize| (resistance[i] * veh.modifiers.terrain_resistance_factors[i]).max(0.1);
    (on_surface(0) / on_surface(surface)).clamp(0.15, 1.0)
}

fn integrate_drive(
    veh: &mut VehicleSimState,
    map: &ArenaMap,
    balance: &BalanceConfig,
    assist: f32,
    destructibles: Option<&DestructiblesState>,
    dt: f32,
) -> (Body, Vec<CrushContact>) {
    let input = DriveInput::from_controls(veh.physics.move_flags, veh.physics.cruise_mode, assist);
    let grip = traction(veh, map, balance);
    let slope_limit = climb_limit(veh, balance);
    let hp = veh.engine_power_hp * veh.modifiers.engine_power_factor;
    let powered = hp > 0.0
        && veh.damage.engine.state != crate::sim::crits::DeviceState::Destroyed
        && veh.damage.is_crew_active;
    let state = &mut veh.physics;
    let working_tracks = u8::from(!state.left_track_broken) + u8::from(!state.right_track_broken);
    let forward_limit = (veh.speed_limits.0 * veh.modifiers.forward_max_speed_factor).max(0.0);
    let reverse_limit = (veh.speed_limits.1 * veh.modifiers.backward_max_speed_factor).max(0.0);
    let speed_ratio = (state.speed_mps.abs() / forward_limit.max(1.0)).min(1.0);
    let weight_ratio = (veh.default_weight / veh.mass_kg.max(1.0)).clamp(0.4, 1.4);

    let turn_limit = (veh.rotation_speed_rad * veh.modifiers.chassis_rotation_speed_factor)
        .max(0.0)
        * grip.sqrt()
        * weight_ratio
        * (1.0 - 0.35 * speed_ratio);
    let turn_target = input.steer
        * turn_limit
        * match (powered, working_tracks) {
            (true, 2) => 1.0,
            (true, 1) => 0.45,
            _ => 0.0,
        };
    state.rot_speed_rps = if powered && working_tracks > 0 {
        approach(state.rot_speed_rps, turn_target, turn_limit * 5.0 * dt)
    } else {
        0.0
    };
    let yaw_change = state.rot_speed_rps * dt;
    let forward = facing(state.yaw + yaw_change * 0.5);

    let speed_before = state.speed_mps;
    let target = if input.throttle >= 0.0 {
        forward_limit
    } else {
        reverse_limit
    } * input.throttle
        * grip.sqrt();
    let brake = (balance.brake_force_for_mass(veh.mass_kg) / veh.mass_kg.max(1.0)
        * veh.modifiers.brake_force_factor)
        .clamp(0.5, 18.0);
    let (ground_y, normal) = ground_at(map, state.pos);
    let heading_grade = -xz(normal).dot(forward) / normal.y.max(0.1);
    let effort = (hp / (veh.mass_kg / 1000.0).max(1.0) * 0.25).clamp(0.25, 9.0);
    let climbing = heading_grade * input.throttle.signum();
    let acceleration =
        (effort * grip - 1.8 * climbing).max(0.25) * if input.throttle < 0.0 { 0.7 } else { 1.0 };
    state.speed_mps = match (powered, working_tracks) {
        (_, 0 | 1) => 0.0,
        (false, _) => approach(speed_before, 0.0, brake * dt),
        _ if target == 0.0 => {
            let drag = 0.55 / grip + speed_before.abs() * 0.12;
            approach(speed_before, 0.0, drag * dt)
        }
        _ if speed_before * target < 0.0 => approach(speed_before, 0.0, brake * dt),
        _ if speed_before.abs() > target.abs() => approach(speed_before, target, brake * dt),
        _ => approach(speed_before, target, acceleration * dt),
    };

    let requested_motion = input.throttle != 0.0 || input.steer != 0.0;
    let flags = state.move_flags & 63;
    let drive_flags = if flags != 0 {
        flags
    } else if input.throttle > 0.0 {
        1
    } else if input.throttle < 0.0 {
        2
    } else if input.steer > 0.0 {
        8
    } else {
        4
    };
    state.engine_mode = if powered && requested_motion {
        [3, drive_flags]
    } else {
        [1, 0]
    };
    state.pos.y = ground_y;
    let start = state.pos;
    let start_yaw = state.yaw;
    let footprint = veh.footprint;
    let pivot_offset = if veh.rotation_is_around_center || working_tracks == 0 {
        0.0
    } else {
        veh.track_center_offset.clamp(0.0, footprint.half_width()) * yaw_change.abs() * 0.35
    };
    let speed = state.speed_mps;
    let motion = forward * (speed * dt + pivot_offset);
    // A hull ramming a destructible hits it with its speed, or with the speed its engine is
    // pushing for: a tank parked against a bush still flattens it under throttle.
    let pushing = if powered && working_tracks == 2 { target.abs() } else { 0.0 };
    let ram = destructibles.map(|state| Ram {
        state,
        mass_kg: veh.mass_kg,
        speed: speed.abs().max(pushing),
    });
    let moved = constrain_motion(map, footprint, start, start_yaw, yaw_change, motion, slope_limit, ram.as_ref());
    if moved.turn_blocked {
        veh.physics.rot_speed_rps = 0.0;
    }
    veh.physics.yaw = normalize_angle(moved.yaw);
    let mut body = Body::parked(VehicleSnapshot::from_sim_state(veh));
    body.drive_end = moved.end;
    body.end = moved.end;
    // Clip the velocity along blocking normals. Keep the along-wall component.
    if moved.blocked {
        body.velocity = (xz(moved.end) - xz(body.start)) / dt;
    }
    (body, moved.crushed)
}

fn climb_limit(veh: &VehicleSimState, balance: &BalanceConfig) -> f32 {
    let degrees = if veh.max_climb_angle_deg > 0.0 {
        veh.max_climb_angle_deg
    } else {
        balance.max_climbable_slope_deg
    };
    degrees.clamp(0.0, 89.0).to_radians().cos()
}

/// Hull ramming standing destructibles: they break when the client's ramming damage of
/// `mass_kg` at `speed` exceeds their scaled health, and block the hull otherwise.
pub(crate) struct Ram<'a> {
    pub state: &'a DestructiblesState,
    pub mass_kg: f32,
    pub speed: f32,
}

/// Geometry that can stop a hull: a static model, or a standing destructible.
struct Blocker<'a> {
    pieces: &'a [BandPiece],
    destructible: Option<&'a ChunkDestructible>,
}

struct Moved {
    end: Vec3,
    yaw: f32,
    blocked: bool,
    turn_blocked: bool,
    crushed: Vec<CrushContact>,
}

fn standing(state: &DestructiblesState, d: &ChunkDestructible) -> bool {
    let Ok(id) = u8::try_from(d.index_in_chunk) else {
        return false;
    };
    match d.kind {
        DestructibleKind::Structure { .. } => state.structure_is_standing(d.chunk_id, id),
        _ => state.tree_is_standing(d.chunk_id, d.index_in_chunk),
    }
}

fn blockers<'a>(map: &'a ArenaMap, lo: Vec2, hi: Vec2, ram: Option<&Ram>) -> Vec<Blocker<'a>> {
    let mut found: Vec<Blocker<'a>> = map
        .query_obstacles_aabb(lo.to_array(), hi.to_array())
        .into_iter()
        .filter(|obstacle| !obstacle.pieces.is_empty())
        .map(|obstacle| Blocker {
            pieces: &obstacle.pieces,
            destructible: None,
        })
        .collect();
    if let Some(ram) = ram {
        found.extend(
            map.query_destructibles_aabb(lo.to_array(), hi.to_array())
                .into_iter()
                .filter(|d| !d.pieces.is_empty() && standing(ram.state, d))
                .map(|d| Blocker {
                    pieces: &d.pieces,
                    destructible: Some(d),
                }),
        );
    }
    found
}

/// First geometry the hull overlaps, with the normal that pushes the hull out.
fn first_overlap(blockers: &[Blocker], skip: &[bool], hull: &HullBox) -> Option<(usize, Vec2)> {
    let (lo, hi) = hull.bounds();
    for (index, blocker) in blockers.iter().enumerate() {
        if skip[index] {
            continue;
        }
        for piece in blocker.pieces {
            if piece.max.x <= lo.x || piece.min.x >= hi.x || piece.max.y <= lo.y || piece.min.y >= hi.y {
                continue;
            }
            if let Some((normal, _)) = penetration(hull, &piece.points) {
                return Some((index, normal));
            }
        }
    }
    None
}

/// Earliest fraction of the move (`motion` and `turn` together) at which the hull meets
/// geometry: the last free fraction, the blocker and its push-out normal.
fn sweep(
    blockers: &[Blocker],
    skip: &[bool],
    footprint: Footprint,
    from: Vec2,
    yaw: f32,
    motion: Vec2,
    turn: f32,
) -> Option<(f32, usize, Vec2)> {
    let travel = motion.length() + footprint.reach() * turn.abs();
    if travel < 1e-6 {
        return None;
    }
    // A zero-thickness wall stays inside the hull over at least its shorter side, so steps of
    // half that side cannot skip it.
    let side = (footprint.max - footprint.min).min_element().max(0.2);
    let samples = ((travel / (0.5 * side)).ceil() as usize).clamp(1, 64);
    let pose = |t: f32| footprint.at(from + motion * t, yaw + turn * t);
    let mut free = 0.0;
    for k in 1..=samples {
        let t = k as f32 / samples as f32;
        let Some(mut hit) = first_overlap(blockers, skip, &pose(t)) else {
            free = t;
            continue;
        };
        let (mut lo, mut hi) = (free, t);
        for _ in 0..14 {
            let mid = 0.5 * (lo + hi);
            match first_overlap(blockers, skip, &pose(mid)) {
                Some(found) => {
                    hi = mid;
                    hit = found;
                }
                None => lo = mid,
            }
        }
        // Rest a skin away from the contact so the next frame starts clear of it.
        return Some(((lo - SKIN / travel).max(0.0), hit.0, hit.1));
    }
    None
}

/// Move a hull by `delta` while turning by `turn`, stopping and sliding at geometry inside the
/// hull band and at slopes steeper than `climb_cos`. A destructible the hull rams hard enough
/// breaks instead of stopping it.
#[allow(clippy::too_many_arguments)]
fn constrain_motion(
    map: &ArenaMap,
    footprint: Footprint,
    start: Vec3,
    yaw: f32,
    turn: f32,
    delta: Vec2,
    climb_cos: f32,
    ram: Option<&Ram>,
) -> Moved {
    let (a, b) = map.def.bounding_box;
    let a = Vec2::from_array(a);
    let b = Vec2::from_array(b);
    let min = a.min(b);
    let max = a.max(b);
    let mut current = xz(start).clamp(min, max);
    let mut height = start.y;
    let mut yaw = yaw;
    let mut turn = turn;
    let mut blocked = false;
    let mut turn_blocked = false;
    let mut remaining = delta;
    let mut crushed = Vec::new();

    let margin = Vec2::splat(footprint.reach() + 0.5);
    let found = blockers(map, current.min(current + delta) - margin, current.max(current + delta) + margin, ram);
    // Geometry the hull already overlaps does not hold it, so it can drive out.
    let start_hull = footprint.at(current, yaw);
    let mut skip: Vec<bool> = found
        .iter()
        .map(|blocker| {
            let only = [Blocker {
                pieces: blocker.pieces,
                destructible: None,
            }];
            first_overlap(&only, &[false], &start_hull).is_some()
        })
        .collect();

    for _ in 0..3 {
        if remaining.length_squared() < 1e-10 && turn.abs() < 1e-7 {
            break;
        }
        let desired = current + remaining;
        let goal = desired.clamp(min, max);
        blocked |= goal != desired;
        let motion = goal - current;
        let (next, next_yaw) = loop {
            let Some((t, index, normal)) = sweep(&found, &skip, footprint, current, yaw, motion, turn) else {
                remaining = Vec2::ZERO;
                break (goal, yaw + std::mem::take(&mut turn));
            };
            if let (Some(ram), Some(d)) = (ram, found[index].destructible) {
                let id = u8::try_from(d.index_in_chunk).unwrap_or(u8::MAX);
                if ram.state.ramming_breaks(d.chunk_id, id, ram.mass_kg, ram.speed) {
                    skip[index] = true;
                    crushed.push(CrushContact {
                        chunk: d.chunk_id,
                        index: d.index_in_chunk,
                        speed: ram.speed,
                        yaw: if motion.length_squared() > 1e-10 { motion.x.atan2(motion.y) } else { yaw },
                        mass_kg: ram.mass_kg,
                    });
                    continue;
                }
            }
            blocked = true;
            turn_blocked |= turn.abs() > 1e-7;
            remaining = motion * (1.0 - t);
            remaining -= normal * remaining.dot(normal).min(0.0);
            let reached = (current + motion * t, yaw + turn * t);
            turn = 0.0;
            break reached;
        };
        let (next_y, normal) = ground_at(map, Vec3::new(next.x, height, next.y));
        let direction = next - current;
        let uphill = next_y > height + 0.001 || -xz(normal).dot(direction) > 0.001;
        if normal.y < climb_cos && uphill {
            blocked = true;
            break;
        }
        current = next;
        yaw = next_yaw;
        height = next_y;
    }
    Moved {
        end: Vec3::new(current.x, height, current.y),
        yaw,
        blocked,
        turn_blocked,
        crushed,
    }
}

fn contact(a: Body, b: Body) -> Option<(Vec2, f32)> {
    if (a.end.y - b.end.y).abs() >= HULL_HEIGHT && (a.start.y - b.start.y).abs() >= HULL_HEIGHT {
        return None;
    }
    hull_penetration(&a.hull(), &b.hull())
}

/// Shove a hull by `delta`; geometry and standing destructibles stop the shove.
fn shove(map: &ArenaMap, body: &Body, delta: Vec2, destructibles: Option<&DestructiblesState>) -> Vec3 {
    let ram = destructibles.map(|state| Ram {
        state,
        mass_kg: body.mass(),
        speed: 0.0,
    });
    constrain_motion(map, body.tank.footprint, body.end, body.tank.yaw, 0.0, delta, 0.0, ram.as_ref()).end
}

fn resolve_contacts(
    bodies: &mut [Body],
    map: &ArenaMap,
    balance: &BalanceConfig,
    destructibles: Option<&DestructiblesState>,
) -> Vec<RamContact> {
    let mut impacts = BTreeMap::new();
    for _ in 0..CONTACT_PASSES {
        for first in 0..bodies.len() {
            let (before, after) = bodies.split_at_mut(first + 1);
            let a = &mut before[first];
            for b in after {
                let Some((normal, depth)) = contact(*a, *b) else {
                    continue;
                };
                let total = a.mass() + b.mass();
                let weight_a = b.mass() / total;
                let weight_b = a.mass() / total;
                let closing = -(a.velocity - b.velocity).dot(normal);
                if closing > 0.0 {
                    a.velocity += normal * closing * weight_a;
                    b.velocity -= normal * closing * weight_b;
                }
                if closing >= RAM_SPEED && balance.ramming_damage_scale > 0.0 {
                    let game_damage = (closing - RAM_SPEED)
                        * closing
                        * (a.mass() * weight_a / 1000.0)
                        * balance.ramming_damage_scale
                        * 0.5;
                    let damage = |body: &Body, share: f32| {
                        (game_damage * share / body.tank.antifragmentation_lining_factor.max(0.5))
                            .round()
                            .clamp(0.0, 250.0) as i16
                    };
                    let impact = RamContact {
                        first: a.tank.entity_id,
                        second: b.tank.entity_id,
                        first_damage: damage(a, weight_a),
                        second_damage: damage(b, weight_b),
                    };
                    if impact.first_damage > 0 || impact.second_damage > 0 {
                        impacts
                            .entry((impact.first, impact.second))
                            .or_insert(impact);
                    }
                }
                if depth > SKIN {
                    let old_a = a.end;
                    let old_b = b.end;
                    let separation = normal * (depth + SKIN);
                    a.end = shove(map, a, separation * weight_a, destructibles);
                    b.end = shove(map, b, -separation * weight_b, destructibles);
                    let achieved = xz((a.end - old_a) - (b.end - old_b)).dot(normal);
                    if achieved < depth - SKIN {
                        // Give the free hull the displacement a wall prevented on the other side.
                        let correction = normal * (depth - achieved);
                        if xz(a.end - old_a).dot(normal) + SKIN < depth * weight_a {
                            b.end = shove(map, b, -correction, destructibles);
                        } else {
                            a.end = shove(map, a, correction, destructibles);
                        }
                    }
                }
            }
        }
    }
    impacts.into_values().collect()
}

/// Integrate every hull before solving contacts against the shared frame.
pub(crate) fn step_arena_vehicles(
    states: &mut HashMap<i32, VehicleSimState>,
    controls: impl IntoIterator<Item = (i32, bool, f32)>,
    map: &ArenaMap,
    balance: &BalanceConfig,
    destructibles: Option<&DestructiblesState>,
    dt: f32,
) -> DriveFrame {
    let mut crushed = Vec::new();
    let mut bodies: Vec<_> = controls
        .into_iter()
        .filter_map(|(id, driving, assist)| {
            let veh = states.get_mut(&id)?;
            if !veh.is_alive {
                return None;
            }
            if driving {
                let (body, hits) = integrate_drive(veh, map, balance, assist, destructibles, dt);
                crushed.extend(hits);
                Some(body)
            } else {
                let mut snapshot = VehicleSnapshot::from_sim_state(veh);
                snapshot.speed_mps = 0.0;
                Some(Body::parked(snapshot))
            }
        })
        .collect();
    bodies.sort_unstable_by_key(|body| body.tank.entity_id);
    let rams = resolve_contacts(&mut bodies, map, balance, destructibles);
    let crushed = strongest_hits(crushed);
    let mut pushes = Vec::new();
    for body in bodies {
        body.write_back(
            &mut states.get_mut(&body.tank.entity_id).unwrap().physics,
            map,
        );
        if xz(body.end - body.drive_end).length_squared() >= 0.05_f32.powi(2) {
            pushes.push((body.tank.entity_id, body.end));
        }
    }
    DriveFrame {
        rams,
        pushes,
        crushed,
    }
}

/// One destructible broken by several hulls in a frame falls from the hardest hit.
fn strongest_hits(hits: Vec<CrushContact>) -> Vec<CrushContact> {
    let mut strongest: BTreeMap<(u16, usize), CrushContact> = BTreeMap::new();
    for hit in hits {
        let entry = strongest.entry((hit.chunk, hit.index)).or_insert(CrushContact { ..hit });
        if hit.mass_kg * hit.speed > entry.mass_kg * entry.speed {
            *entry = hit;
        }
    }
    strongest.into_values().collect()
}

pub fn step_vehicle_driving(
    veh: &mut VehicleSimState,
    all_vehicles: &[VehicleSnapshot],
    map: &ArenaMap,
    balance: &BalanceConfig,
    dt: f32,
) -> DrivingStepResult {
    step_vehicle_driving_with_destructibles(veh, all_vehicles, map, balance, None, dt)
}

pub fn step_vehicle_driving_with_destructibles(
    veh: &mut VehicleSimState,
    all_vehicles: &[VehicleSnapshot],
    map: &ArenaMap,
    balance: &BalanceConfig,
    destructibles: Option<&DestructiblesState>,
    dt: f32,
) -> DrivingStepResult {
    let mut result = DrivingStepResult::default();
    if !dt.is_finite() || dt <= 0.0 || !veh.is_alive {
        return result;
    }
    let mut others: Vec<_> = all_vehicles
        .iter()
        .copied()
        .filter(|other| other.is_alive && other.entity_id != veh.entity_id)
        .map(Body::parked)
        .collect();
    let mut rams = BTreeSet::new();
    let mut crushed = BTreeSet::new();
    let steps = (dt / 0.05).ceil().clamp(1.0, 64.0) as u32;
    let frame = dt / steps as f32;
    for _ in 0..steps {
        let (body, hits) = integrate_drive(veh, map, balance, 0.0, destructibles, frame);
        crushed.extend(hits.into_iter().map(|hit| (hit.chunk, hit.index)));
        others.push(body);
        others.sort_unstable_by_key(|body| body.tank.entity_id);
        for ram in resolve_contacts(&mut others, map, balance, destructibles) {
            if (ram.first == veh.entity_id || ram.second == veh.entity_id)
                && rams.insert((ram.first, ram.second))
            {
                result.ramming_damages.push((ram.first, ram.first_damage));
                result.ramming_damages.push((ram.second, ram.second_damage));
            }
        }
        let index = others
            .iter()
            .position(|body| body.tank.entity_id == veh.entity_id)
            .unwrap();
        let own = others.remove(index);
        own.write_back(&mut veh.physics, map);
        for body in &mut others {
            if xz(body.end - body.start).length_squared() > 1e-8 {
                result.pushed_entity_id = Some(body.tank.entity_id);
                result.on_pushed = Some((body.end.x, body.end.z));
            }
            body.tank.pos = body.end;
            body.start = body.end;
            body.drive_end = body.end;
        }
    }
    result.broken_destructibles.extend(crushed);
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;
    use std::path::PathBuf;
    use std::sync::Arc;
    use glam::Mat4;
    use crate::assets::arena_defs::ArenaDef;
    use crate::assets::bsp2::BspTree;
    use crate::assets::chunk::{decode_chunk_coords, SpaceBounds};
    use crate::assets::map_store::{StaticObstacle, TerrainMap};
    use crate::assets::terrain::{
        MaterialKinds, TerrainChunk, DOMINANT_TEX_GRID_SIZE, HEIGHTMAP_GRID_SIZE,
    };

    fn build_synthetic_materials() -> Arc<MaterialKinds> {
        let xml = br#"<root>
            <kind><id>1</id><desc>firm_road</desc><ground_strength>firm</ground_strength>
                <terrain>maps/landscape/firm</terrain>
            </kind>
            <kind><id>2</id><desc>soft_mud</desc><ground_strength>soft</ground_strength>
                <terrain>maps/landscape/soft</terrain>
            </kind>
        </root>"#;
        Arc::new(MaterialKinds::parse(xml).unwrap())
    }

    fn build_test_map(slope_deg: f32, dom_tex: &str) -> ArenaMap {
        let bounds = SpaceBounds {
            min_x: 0,
            max_x: 0,
            min_y: 0,
            max_y: 0,
        };
        let mut heights = vec![0.0; HEIGHTMAP_GRID_SIZE * HEIGHTMAP_GRID_SIZE];
        let spacing = 100.0 / 64.0;
        let tan_slope = slope_deg.to_radians().tan();
        for r in 0..HEIGHTMAP_GRID_SIZE {
            let z = (r as f32 - 2.0) * spacing;
            for c in 0..HEIGHTMAP_GRID_SIZE {
                heights[r * HEIGHTMAP_GRID_SIZE + c] = z * tan_slope;
            }
        }
        let chunk = TerrainChunk {
            heights,
            min_y: 0.0,
            max_y: 100.0 * tan_slope,
            dom_tex_indices: vec![0; DOMINANT_TEX_GRID_SIZE * DOMINANT_TEX_GRID_SIZE],
            tex_names: vec![dom_tex.to_string()],
        };
        let terrain = TerrainMap::from_chunks(
            bounds,
            [(decode_chunk_coords("00000000o").unwrap(), chunk)],
        )
        .unwrap();

        let def = ArenaDef {
            id: 1,
            name: "test".into(),
            display_name_key: "test".into(),
            description_key: "test".into(),
            geometry: "spaces/test".into(),
            bounding_box: ([0.0, 0.0], [100.0, 100.0]),
            min_players_in_team: 1,
            max_players_in_team: 15,
            round_length_secs: 900,
            arena_start_delay_secs: 5.0,
            kick_after_finish_wait_secs: 10.0,
            minimap_texture: None,
        };

        ArenaMap::from_parts(
            def,
            bounds,
            terrain,
            build_synthetic_materials(),
            vec![],
            vec![],
            vec![],
            vec![],
            vec![],
            BTreeMap::new(),
            &BalanceConfig::default(),
        )
    }

    fn build_wall_obstacle(
        center: Vec3,
        half_x: f32,
        y_min: f32,
        y_max: f32,
        half_z: f32,
    ) -> StaticObstacle {
        let mut bsp = Vec::new();
        bsp.extend_from_slice(b"BSP\0");
        let tris = [
            (
                [-half_x, y_min, half_z],
                [half_x, y_min, half_z],
                [half_x, y_max, half_z],
            ),
            (
                [-half_x, y_min, half_z],
                [half_x, y_max, half_z],
                [-half_x, y_max, half_z],
            ),
            (
                [-half_x, y_min, -half_z],
                [-half_x, y_max, -half_z],
                [half_x, y_max, -half_z],
            ),
            (
                [-half_x, y_min, -half_z],
                [half_x, y_max, -half_z],
                [half_x, y_min, -half_z],
            ),
            (
                [half_x, y_min, -half_z],
                [half_x, y_max, -half_z],
                [half_x, y_max, half_z],
            ),
            (
                [-half_x, y_min, -half_z],
                [-half_x, y_min, half_z],
                [-half_x, y_max, half_z],
            ),
        ];
        bsp.extend_from_slice(&(tris.len() as u32).to_le_bytes());
        bsp.extend_from_slice(&1u32.to_le_bytes());
        bsp.extend_from_slice(&1u32.to_le_bytes());
        for (v0, v1, v2) in tris {
            for v in [v0, v1, v2] {
                for c in v {
                    bsp.extend_from_slice(&c.to_le_bytes());
                }
            }
            bsp.extend_from_slice(&0u32.to_le_bytes());
        }
        bsp.push(0xa0);
        for c in [1.0f32, 0.0, 0.0, 0.0] {
            bsp.extend_from_slice(&c.to_le_bytes());
        }
        bsp.extend_from_slice(&0u32.to_le_bytes());
        bsp.extend_from_slice(&0u32.to_le_bytes());
        let tree = BspTree::parse(&bsp, &[]).expect("parse synthetic wall BSP");
        StaticObstacle::from_bsp(
            "wall.model".into(),
            PathBuf::from("wall.primitives"),
            Mat4::from_translation(center),
            Arc::new(tree),
        )
        .expect("create static obstacle")
    }

    /// BigWorld hull attitude: `+pitch` lowers the nose (retail `pitchLimits -25 8`) and
    /// `+roll` raises the right side. The server uses this frame for hit boxes and the gun
    /// ray, so a gun level with the hull must point up the slope it faces.
    #[test]
    fn hull_attitude_points_a_level_gun_up_the_slope() {
        use crate::sim::ballistics::{gun_ray, AimState};
        let balance = BalanceConfig::default();
        // Terrain rises toward +Z.
        let map = build_test_map(15.0, "maps/landscape/firm.dds");
        let gun_direction = |yaw: f32, turret_yaw: f32| {
            let mut tank = VehicleSimState {
                entity_id: 1,
                ..Default::default()
            };
            tank.physics.pos = Vec3::new(50.0, 0.0, 30.0);
            tank.physics.yaw = yaw;
            step_vehicle_driving(&mut tank, &[], &map, &balance, 0.05);
            let aim = AimState {
                turret_yaw,
                ..Default::default()
            };
            gun_ray(&tank.physics, &aim, Vec3::ZERO, Vec3::ZERO, Vec3::Z).1
        };
        let facing_uphill = gun_direction(0.0, 0.0);
        assert!(facing_uphill.y > 0.2, "nose must rise uphill: {facing_uphill:?}");
        // Facing -X puts +Z on the right; the turret turned right looks uphill.
        let across_slope = gun_direction(-std::f32::consts::FRAC_PI_2, std::f32::consts::FRAC_PI_2);
        assert!(across_slope.y > 0.2, "right side must rise: {across_slope:?}");
    }

    #[test]
    fn no_ram_damage_when_overlapping_bodies_move_apart() {
        let map = build_test_map(0.0, "maps/landscape/firm.dds");
        let mut tank = VehicleSimState {
            entity_id: 1,
            ..Default::default()
        };
        tank.physics.pos = Vec3::new(50.0, 0.0, 50.0);
        tank.physics.speed_mps = -3.0;
        tank.physics.move_flags = 2;
        let mut other = VehicleSimState {
            entity_id: 2,
            ..Default::default()
        };
        other.physics.pos = Vec3::new(50.0, 0.0, 53.5);
        other.physics.speed_mps = 3.0;
        let result = step_vehicle_driving(
            &mut tank,
            &[VehicleSnapshot::from_sim_state(&other)],
            &map,
            &BalanceConfig::default(),
            0.05,
        );
        assert!(
            result.ramming_damages.is_empty(),
            "{:?}",
            result.ramming_damages
        );
    }

    #[test]
    fn respects_speed_limits_terrain_resistance_and_max_climb_angle() {
        let balance = BalanceConfig::default();
        let flat_firm_map = build_test_map(0.0, "maps/landscape/firm.dds");
        let flat_soft_map = build_test_map(0.0, "maps/landscape/soft.dds");
        let steep_map = build_test_map(30.0, "maps/landscape/firm.dds"); // 30 deg > 25 deg max climb

        // 1. Acceleration proportional to engine.power / weight up to speedLimits
        let mut tank_a = VehicleSimState {
            entity_id: 1,
            team: 1,
            is_alive: true,
            physics: VehiclePhysicsState {
                pos: Vec3::new(50.0, 0.0, 20.0),
                move_flags: 1, // forward
                ..Default::default()
            },
            damage: crate::sim::crits::VehicleDamageState::default(),
            modifiers: EffectiveModifiers::default(),
            mass_kg: 10_000.0,
            engine_power_hp: 200.0,
            speed_limits: (10.0, 4.0),
            max_climb_angle_deg: 25.0,
            rotation_speed_rad: 0.6,
            rotation_is_around_center: true,
            track_center_offset: 1.5,
            default_weight: 10_000.0,
            tags: ["mediumTank".to_string()].into_iter().collect(),
            ..Default::default()
        };

        let mut tank_b = VehicleSimState {
            entity_id: 2,
            engine_power_hp: 400.0, // 2x power
            ..tank_a.clone()
        };

        // Step for 1.0 second on firm terrain
        step_vehicle_driving(&mut tank_a, &[], &flat_firm_map, &balance, 1.0);
        step_vehicle_driving(&mut tank_b, &[], &flat_firm_map, &balance, 1.0);

        assert!(
            tank_b.physics.speed_mps > tank_a.physics.speed_mps,
            "tank with 2x power/weight should accelerate faster: A={}, B={}",
            tank_a.physics.speed_mps,
            tank_b.physics.speed_mps
        );
        assert!(tank_a.physics.speed_mps > 0.0);
        assert!(tank_b.physics.speed_mps <= 10.0);

        // Reach top speed with room remaining before the arena boundary.
        for _ in 0..5 {
            step_vehicle_driving(&mut tank_b, &[], &flat_firm_map, &balance, 1.0);
        }
        assert!(
            (tank_b.physics.speed_mps - 10.0).abs() < 1e-2,
            "tank should cap at forward speed limit 10 m/s, got {}",
            tank_b.physics.speed_mps
        );
        for _ in 0..10 {
            step_vehicle_driving(&mut tank_b, &[], &flat_firm_map, &balance, 1.0);
        }
        assert_eq!(tank_b.physics.pos.z, 100.0);
        assert_eq!(tank_b.physics.speed_mps, 0.0);

        // 2. Slows on soft vs firm terrain
        let mut tank_firm = VehicleSimState {
            entity_id: 3,
            physics: VehiclePhysicsState {
                pos: Vec3::new(50.0, 0.0, 20.0),
                move_flags: 1,
                ..Default::default()
            },
            ..tank_a.clone()
        };
        let mut tank_soft = VehicleSimState {
            entity_id: 4,
            physics: VehiclePhysicsState {
                pos: Vec3::new(50.0, 0.0, 20.0),
                move_flags: 1,
                ..Default::default()
            },
            ..tank_a.clone()
        };
        step_vehicle_driving(&mut tank_firm, &[], &flat_firm_map, &balance, 1.0);
        step_vehicle_driving(&mut tank_soft, &[], &flat_soft_map, &balance, 1.0);
        assert!(
            tank_firm.physics.speed_mps > tank_soft.physics.speed_mps,
            "tank on firm terrain ({}) should be faster than on soft ({})",
            tank_firm.physics.speed_mps,
            tank_soft.physics.speed_mps
        );

        // 3. Aligns pitch/roll to terrain normal
        let mut tank_slope = VehicleSimState {
            entity_id: 5,
            physics: VehiclePhysicsState {
                pos: Vec3::new(50.0, 0.0, 20.0),
                yaw: 0.0, // facing +Z (uphill on steep_map)
                move_flags: 0,
                ..Default::default()
            },
            ..tank_a.clone()
        };
        step_vehicle_driving(&mut tank_slope, &[], &steep_map, &balance, 0.1);
        assert!(
            tank_slope.physics.pitch < -0.4,
            "tank facing uphill on 30 deg slope must raise its nose (BigWorld pitch < -0.4 rad), got {}",
            tank_slope.physics.pitch
        );

        // 4. Stops climbing above maxClimbAngle
        let start_z = tank_slope.physics.pos.z;
        tank_slope.physics.move_flags = 1; // try to drive forward up 30-deg slope
        for _ in 0..10 {
            step_vehicle_driving(&mut tank_slope, &[], &steep_map, &balance, 0.1);
        }
        assert!(
            (tank_slope.physics.pos.z - start_z).abs() < 1e-3,
            "tank should not climb slope steeper than maxClimbAngle (25 deg): moved from {} to {}",
            start_z,
            tank_slope.physics.pos.z
        );

        // 5. Cannot move forward when left_track_broken == true
        let mut tank_broken = VehicleSimState {
            entity_id: 6,
            physics: VehiclePhysicsState {
                pos: Vec3::new(50.0, 0.0, 20.0),
                move_flags: 1,
                left_track_broken: true,
                ..Default::default()
            },
            ..tank_a.clone()
        };
        let broken_start_z = tank_broken.physics.pos.z;
        for _ in 0..10 {
            step_vehicle_driving(&mut tank_broken, &[], &flat_firm_map, &balance, 0.1);
        }
        assert_eq!(tank_broken.physics.speed_mps, 0.0);
        assert_eq!(tank_broken.physics.pos.z, broken_start_z);
    }

    #[test]
    fn tank_tank_collision_and_ramming_damage() {
        let balance = BalanceConfig::default();
        let flat_map = build_test_map(0.0, "maps/landscape/firm.dds");

        // 1. Heavy fast tank (50t, 10 m/s) colliding with lighter stationary tank (10t, 0 m/s)
        let mut heavy_tank = VehicleSimState {
            entity_id: 10,
            team: 1,
            is_alive: true,
            physics: VehiclePhysicsState {
                pos: Vec3::new(50.0, 0.0, 47.0),
                yaw: 0.0, // facing +Z
                speed_mps: 10.0,
                move_flags: 1,
                ..Default::default()
            },
            damage: crate::sim::crits::VehicleDamageState::default(),
            modifiers: EffectiveModifiers::default(),
            mass_kg: 50_000.0,
            engine_power_hp: 800.0,
            speed_limits: (12.0, 4.0),
            max_climb_angle_deg: 25.0,
            rotation_speed_rad: 0.5,
            rotation_is_around_center: true,
            track_center_offset: 1.8,
            default_weight: 50_000.0,
            tags: ["heavyTank".to_string()].into_iter().collect(),
            ..Default::default()
        };

        let light_snapshot = VehicleSnapshot {
            entity_id: 20,
            team: 2,
            is_alive: true,
            pos: Vec3::new(50.0, 0.0, 50.0), // 3m ahead along +Z; 2 m half lengths => 1m overlap
            yaw: 0.0,
            speed_mps: 0.0,
            mass_kg: 10_000.0,
            footprint: Footprint::centered(2.0, 2.0),
            left_track_broken: false,
            right_track_broken: false,
            antifragmentation_lining_factor: 1.0,
        };

        let result = step_vehicle_driving(
            &mut heavy_tank,
            &[light_snapshot],
            &flat_map,
            &balance,
            0.1,
        );

        // Resolves overlap without tunneling: distance >= 4.0m
        let light_pos = result
            .on_pushed
            .map(|(x, z)| Vec3::new(x, 0.0, z))
            .unwrap_or(light_snapshot.pos);
        let new_dist = (heavy_tank.physics.pos - light_pos).length();
        assert!(
            new_dist >= 3.99,
            "overlap must be resolved without tunneling, dist={}",
            new_dist
        );

        // Deals speed-and-mass-scaled ramming damage (> 0) to both, weighted inversely by mass
        let heavy_dmg = result
            .ramming_damages
            .iter()
            .find(|(id, _)| *id == 10)
            .map(|(_, d)| *d)
            .unwrap_or(0);
        let light_dmg = result
            .ramming_damages
            .iter()
            .find(|(id, _)| *id == 20)
            .map(|(_, d)| *d)
            .unwrap_or(0);

        assert!(heavy_dmg > 0, "heavy tank takes ramming damage > 0");
        assert!(light_dmg > 0, "light tank takes ramming damage > 0");
        assert!(
            light_dmg > heavy_dmg,
            "lighter tank must take more ramming damage (light={}, heavy={})",
            light_dmg,
            heavy_dmg
        );

        // Triggers on_pushed when displacement exceeds client filter threshold
        assert!(
            result.on_pushed.is_some(),
            "displacement of lighter tank must trigger on_pushed"
        );

        // 2. Obstacle stop against static StaticObstacle BSP
        let wall = build_wall_obstacle(Vec3::new(50.0, 0.0, 55.0), 10.0, 0.0, 5.0, 1.0);
        let mut map_with_wall = flat_map.clone();
        map_with_wall.obstacles.push(wall);
        map_with_wall.rebuild_collision_index();

        let mut tank_into_wall = VehicleSimState {
            entity_id: 30,
            team: 1,
            is_alive: true,
            physics: VehiclePhysicsState {
                pos: Vec3::new(50.0, 0.0, 52.0),
                yaw: 0.0, // facing +Z directly at wall at z=55
                speed_mps: 10.0,
                move_flags: 1,
                ..Default::default()
            },
            ..heavy_tank.clone()
        };

        for _ in 0..10 {
            step_vehicle_driving(
                &mut tank_into_wall,
                &[],
                &map_with_wall,
                &balance,
                0.1,
            );
        }

        assert!(
            tank_into_wall.physics.pos.z < 54.0,
            "tank must be stopped by wall at z=55 (radius 2.0), got z={}",
            tank_into_wall.physics.pos.z
        );
        assert_eq!(
            tank_into_wall.physics.speed_mps, 0.0,
            "tank speed should be 0 against blocking static obstacle"
        );
    }

    #[test]
    fn overhead_obstacle_does_not_block_while_ground_level_obstacle_blocks() {
        let balance = BalanceConfig::default();
        let flat_map = build_test_map(0.0, "maps/landscape/firm.dds");

        // Overhead obstacle (bridge deck) from Y=4.0 to Y=6.0 at Z=55.0. Tank height is 2.5m (Y=0.0..2.5).
        let overhead_bridge = build_wall_obstacle(Vec3::new(50.0, 0.0, 55.0), 10.0, 4.0, 6.0, 1.0);
        let mut map_with_overhead = flat_map.clone();
        map_with_overhead.obstacles.push(overhead_bridge);
        map_with_overhead.rebuild_collision_index();

        let mut tank_under_bridge = VehicleSimState {
            entity_id: 40,
            team: 1,
            is_alive: true,
            physics: VehiclePhysicsState {
                pos: Vec3::new(50.0, 0.0, 52.0),
                yaw: 0.0, // facing +Z directly towards bridge at z=55
                speed_mps: 10.0,
                move_flags: 1,
                ..Default::default()
            },
            damage: crate::sim::crits::VehicleDamageState::default(),
            modifiers: EffectiveModifiers::default(),
            mass_kg: 30_000.0,
            engine_power_hp: 600.0,
            speed_limits: (12.0, 4.0),
            max_climb_angle_deg: 25.0,
            rotation_speed_rad: 0.6,
            rotation_is_around_center: true,
            track_center_offset: 1.5,
            default_weight: 30_000.0,
            tags: ["mediumTank".to_string()].into_iter().collect(),
            ..Default::default()
        };

        // Step for 1.0s: tank should pass underneath without being pushed back or stopped
        for _ in 0..10 {
            step_vehicle_driving(
                &mut tank_under_bridge,
                &[],
                &map_with_overhead,
                &balance,
                0.1,
            );
        }

        assert!(
            tank_under_bridge.physics.pos.z > 56.0,
            "tank must drive under overhead bridge (Z>56), but was stopped at Z={}",
            tank_under_bridge.physics.pos.z
        );
        assert!(
            tank_under_bridge.physics.speed_mps > 0.0,
            "tank speed should remain positive under overhead bridge"
        );

        // Ground-level obstacle at Z=70 from Y=0.0 to Y=3.0: must block the tank
        let ground_wall = build_wall_obstacle(Vec3::new(50.0, 0.0, 70.0), 10.0, 0.0, 3.0, 1.0);
        let mut map_with_ground = map_with_overhead;
        map_with_ground.obstacles.push(ground_wall);
        map_with_ground.rebuild_collision_index();

        for _ in 0..20 {
            step_vehicle_driving(
                &mut tank_under_bridge,
                &[],
                &map_with_ground,
                &balance,
                0.1,
            );
        }

        assert!(
            tank_under_bridge.physics.pos.z < 69.0,
            "tank must be blocked by ground wall at Z=70, got Z={}",
            tank_under_bridge.physics.pos.z
        );
        assert_eq!(
            tank_under_bridge.physics.speed_mps, 0.0,
            "tank speed must be 0 after hitting ground obstacle"
        );
    }

    #[test]
    fn destroyed_engine_immobilizes_tank() {
        let balance = BalanceConfig::default();
        let flat_map = build_test_map(0.0, "maps/landscape/firm.dds");

        let mut tank = VehicleSimState {
            entity_id: 50,
            physics: VehiclePhysicsState {
                pos: Vec3::new(50.0, 0.0, 50.0),
                speed_mps: 5.0, // moving initially
                move_flags: 1, // pressing forward
                ..Default::default()
            },
            modifiers: EffectiveModifiers {
                engine_power_factor: 0.0, // destroyed engine
                ..Default::default()
            },
            ..Default::default()
        };

        // Step driving: cannot accelerate forward and must decelerate to 0
        for _ in 0..30 {
            step_vehicle_driving(&mut tank, &[], &flat_map, &balance, 0.1);
        }

        assert_eq!(
            tank.physics.speed_mps, 0.0,
            "destroyed engine must decelerate tank to complete stop"
        );
        assert_eq!(
            tank.physics.engine_mode[0], 1,
            "engine mode must be idle/stopped [1, 0] when engine is destroyed"
        );
    }

    use crate::assets::bsp2::test_box as box_bsp;

    fn placed_box(bsp: BspTree, transform: Mat4) -> StaticObstacle {
        StaticObstacle::from_bsp("box.model".into(), PathBuf::from("box.primitives"), transform, Arc::new(bsp))
            .expect("create static obstacle")
    }

    fn flat_map(
        obstacles: Vec<StaticObstacle>,
        destructibles: BTreeMap<u16, Vec<crate::assets::map_store::ChunkDestructible>>,
    ) -> ArenaMap {
        let base = build_test_map(0.0, "maps/landscape/firm.dds");
        ArenaMap::from_parts(
            base.def,
            base.bounds,
            base.terrain,
            base.material_kinds,
            obstacles,
            vec![],
            vec![],
            vec![],
            vec![],
            destructibles,
            &BalanceConfig::default(),
        )
    }

    fn driving_hull(pos: Vec3, yaw: f32, footprint: Footprint, mass_kg: f32, speed: f32) -> VehicleSimState {
        VehicleSimState {
            entity_id: 1,
            team: 1,
            physics: VehiclePhysicsState {
                pos,
                yaw,
                speed_mps: speed,
                move_flags: 1,
                ..Default::default()
            },
            footprint,
            mass_kg,
            default_weight: mass_kg,
            engine_power_hp: 600.0,
            speed_limits: (speed.max(1.0), 4.0),
            rotation_speed_rad: 0.6,
            ..Default::default()
        }
    }

    #[test]
    fn hull_meets_a_diagonal_wall_at_its_face_not_its_bounding_box() {
        // A 40 m long, 0.4 m thick wall turned 45 degrees: its bounding box starts at z = 45.7,
        // but on the line x = 40 the wall itself stands at z = 70.
        let centre = Vec3::new(50.0, 0.0, 60.0);
        let wall = placed_box(
            box_bsp(20.0, 0.0, 3.0, 0.2),
            Mat4::from_translation(centre) * Mat4::from_rotation_y(std::f32::consts::FRAC_PI_4),
        );
        let map = flat_map(vec![wall], BTreeMap::new());
        let toward_tank = Vec2::new(-1.0, -1.0).normalize();
        let footprint = Footprint::centered(1.5, 3.5);
        let mut tank = driving_hull(Vec3::new(40.0, 0.0, 20.0), 0.0, footprint, 30_000.0, 8.0);
        let mut deepest = f32::MIN;
        for _ in 0..80 {
            step_vehicle_driving(&mut tank, &[], &map, &BalanceConfig::default(), 0.1);
            let hull = footprint.at(xz(tank.physics.pos), tank.physics.yaw);
            for corner in hull.corners() {
                // Positive past the near face of the wall.
                deepest = deepest.max(0.2 - (corner - xz(centre)).dot(toward_tank));
            }
        }
        assert!(tank.physics.pos.z > 60.0, "stopped at z = {} by the bounding box", tank.physics.pos.z);
        assert!(deepest < 0.05, "a hull corner went {deepest} m into the wall");
    }

    #[test]
    fn long_hull_stops_with_its_bow_at_the_wall() {
        // Wall face at z = 70; the hull reaches 3.5 m ahead of its origin.
        let wall = placed_box(box_bsp(10.0, 0.0, 3.0, 1.0), Mat4::from_translation(Vec3::new(50.0, 0.0, 71.0)));
        let map = flat_map(vec![wall], BTreeMap::new());
        let mut tank = driving_hull(Vec3::new(50.0, 0.0, 55.0), 0.0, Footprint::centered(1.5, 3.5), 30_000.0, 8.0);
        for _ in 0..40 {
            step_vehicle_driving(&mut tank, &[], &map, &BalanceConfig::default(), 0.1);
        }
        let bow = tank.physics.pos.z + 3.5;
        assert!((bow - 70.0).abs() < 0.05, "bow at z = {bow}, wall face at 70");
        assert_eq!(tank.physics.speed_mps, 0.0);
    }

    #[test]
    fn hull_turning_in_place_stops_when_a_corner_meets_the_wall() {
        // The bow is 0.3 m short of the face z = 70. Turning right swings the front left corner
        // to z = centre + 1.5 sin(yaw) + 3.5 cos(yaw): it meets the wall at yaw 0.34 rad.
        let wall = placed_box(box_bsp(10.0, 0.0, 3.0, 1.0), Mat4::from_translation(Vec3::new(50.0, 0.0, 71.0)));
        let map = flat_map(vec![wall], BTreeMap::new());
        let footprint = Footprint::centered(1.5, 3.5);
        let mut tank = driving_hull(Vec3::new(50.0, 0.0, 66.2), 0.0, footprint, 30_000.0, 0.0);
        tank.physics.move_flags = 8;
        for _ in 0..60 {
            step_vehicle_driving(&mut tank, &[], &map, &BalanceConfig::default(), 0.1);
        }
        let yaw = tank.physics.yaw;
        assert!(yaw > 0.30 && yaw < 0.38, "turned to yaw {yaw}");
        let hull = footprint.at(xz(tank.physics.pos), yaw);
        assert!(hull.corners().iter().all(|c| c.y < 70.0 + 0.05), "{:?}", hull.corners());

        tank.physics.move_flags = 2;
        for _ in 0..10 {
            step_vehicle_driving(&mut tank, &[], &map, &BalanceConfig::default(), 0.1);
        }
        assert!(tank.physics.pos.z < 65.5, "reversing out reached z = {}", tank.physics.pos.z);
    }

    fn spruce(chunk_id: u16, slot: usize, at: Vec3) -> crate::assets::map_store::ChunkDestructible {
        let transform = Mat4::from_translation(at);
        crate::assets::map_store::ChunkDestructible {
            chunk_id,
            index_in_chunk: slot,
            desc_id: 0,
            filename: "speedtree/01_Karelia/Spruce.spt".into(),
            kind: crate::assets::destructibles::DestructibleKind::Tree {
                health: 46,
                density: 1.0,
                kinetic_damage_correction: 0.0,
                physic_params: crate::assets::destructibles::DestructiblePhysicParams {
                    mass: 500.0,
                    height: 12.0,
                    spring_angle: 0.2,
                    spring_stiffness: 1000.0,
                    spring_resist: 200.0,
                    air_resist: 10.0,
                    bury_depth: 0.5,
                },
            },
            world_transform: transform,
            position: at,
            world_aabb_min: at - Vec3::new(3.0, 0.0, 3.0),
            world_aabb_max: at + Vec3::new(3.0, 12.0, 3.0),
            bsp: Some(Arc::new(box_bsp(0.3, 0.0, 12.0, 0.3))),
            pieces: Vec::new(),
        }
    }

    #[test]
    fn slow_light_hull_is_stopped_by_a_spruce_that_a_heavy_hull_knocks_down() {
        // Spruce health 46 at scale 1: 11.5 t at 5 m/s deals 21.6, 45 t at 5 m/s deals 84.4.
        let chunk_id = crate::sim::destructibles::encode_chunk_id(0, 0);
        let map = flat_map(vec![], BTreeMap::from([(chunk_id, vec![spruce(chunk_id, 7, Vec3::new(50.0, 0.0, 70.0))])]));
        let standing = crate::sim::destructibles::DestructiblesState::from_map(&map);
        let footprint = Footprint::centered(1.5, 3.0);

        let mut light = driving_hull(Vec3::new(50.0, 0.0, 60.0), 0.0, footprint, 11_500.0, 5.0);
        let mut broken = Vec::new();
        for _ in 0..30 {
            let result = step_vehicle_driving_with_destructibles(
                &mut light, &[], &map, &BalanceConfig::default(), Some(&standing), 0.1,
            );
            broken.extend(result.broken_destructibles);
        }
        assert!(broken.is_empty(), "{broken:?}");
        let bow = light.physics.pos.z + 3.0;
        assert!((bow - 69.7).abs() < 0.05, "light hull bow at z = {bow}, trunk face at 69.7");

        let mut heavy = driving_hull(Vec3::new(50.0, 0.0, 60.0), 0.0, footprint, 45_000.0, 5.0);
        for _ in 0..30 {
            let result = step_vehicle_driving_with_destructibles(
                &mut heavy, &[], &map, &BalanceConfig::default(), Some(&standing), 0.1,
            );
            broken.extend(result.broken_destructibles);
        }
        assert_eq!(broken.first(), Some(&(chunk_id, 7)));
        assert!(heavy.physics.pos.z > 74.0, "heavy hull stuck at z = {}", heavy.physics.pos.z);
    }

    #[test]
    fn hull_starting_against_a_bush_pushes_through_it_under_full_throttle() {
        // Bush health 3 at scale 1. An MS-1 (4691 kg) parked touching it deals 0 at rest, but
        // its engine pushes for 9.7 m/s: 33 > 3. Standing on the brake it never moves.
        let chunk_id = crate::sim::destructibles::encode_chunk_id(0, 0);
        let mut bush = spruce(chunk_id, 3, Vec3::new(50.0, 0.0, 70.0));
        bush.kind = crate::assets::destructibles::DestructibleKind::Tree {
            health: 3,
            density: 1.0,
            kinetic_damage_correction: 0.0,
            physic_params: crate::assets::destructibles::DestructiblePhysicParams {
                mass: 50.0,
                height: 3.0,
                spring_angle: 0.2,
                spring_stiffness: 1000.0,
                spring_resist: 200.0,
                air_resist: 10.0,
                bury_depth: 0.2,
            },
        };
        bush.bsp = Some(Arc::new(box_bsp(2.3, -1.4, 3.3, 2.3)));
        let map = flat_map(vec![], BTreeMap::from([(chunk_id, vec![bush])]));
        let standing = crate::sim::destructibles::DestructiblesState::from_map(&map);
        let footprint = Footprint::centered(1.0, 2.2);
        // Bow 0.05 m short of the bush box.
        let mut tank = driving_hull(Vec3::new(50.0, 0.0, 70.0 - 2.3 - 2.2 - 0.05), 0.0, footprint, 4_691.0, 0.0);
        tank.speed_limits = (9.7, 3.0);
        let mut broken = Vec::new();
        for _ in 0..20 {
            let result = step_vehicle_driving_with_destructibles(
                &mut tank, &[], &map, &BalanceConfig::default(), Some(&standing), 0.1,
            );
            broken.extend(result.broken_destructibles);
        }
        assert_eq!(broken.first(), Some(&(chunk_id, 3)));
        assert!(tank.physics.pos.z > 70.0, "stuck at z = {}", tank.physics.pos.z);
    }

    #[test]
    fn hulls_ram_bow_to_bow_with_their_real_length() {
        // Two 7 m long hulls 6.5 m apart overlap by 0.5 m; circles of radius 2 would not touch.
        let footprint = Footprint::centered(1.5, 3.5);
        let mut rammer = driving_hull(Vec3::new(50.0, 0.0, 40.0), 0.0, footprint, 50_000.0, 10.0);
        rammer.entity_id = 10;
        let target = VehicleSnapshot {
            entity_id: 20,
            team: 2,
            is_alive: true,
            pos: Vec3::new(50.0, 0.0, 46.5),
            yaw: std::f32::consts::PI,
            speed_mps: 0.0,
            mass_kg: 10_000.0,
            footprint,
            left_track_broken: false,
            right_track_broken: false,
            antifragmentation_lining_factor: 1.0,
        };
        let flat = flat_map(vec![], BTreeMap::new());
        let result = step_vehicle_driving(&mut rammer, &[target], &flat, &BalanceConfig::default(), 0.1);
        let target_pos = result.on_pushed.map_or(target.pos, |(x, z)| Vec3::new(x, 0.0, z));
        assert!((target_pos.z - rammer.physics.pos.z) >= 6.99, "{} vs {}", rammer.physics.pos.z, target_pos.z);
        assert!(result.ramming_damages.iter().any(|&(id, damage)| id == 20 && damage > 0));
    }
}
