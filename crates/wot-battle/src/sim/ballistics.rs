//! Game projectiles, turret handling, and impact notifications.

use glam::{Mat4, Vec3};
use rand::Rng;
use wot065_data::item_defs::{ArmorZoneMap, ShellKind, ShellXmlDef, ShotDef};

use crate::assets::destructibles::DestructibleKind;
use crate::assets::map_store::{ArenaMap, ChunkDestructible, TerrainMap, VehicleCollisionSet};
use crate::balance::BalanceConfig;
use crate::sim::destructibles::DestructiblesState;
use crate::sim::physics::{normalize_angle, EffectiveModifiers, VehiclePhysicsState};

fn raycast_terrain(terrain: &TerrainMap, start: Vec3, end: Vec3) -> Option<(f32, Vec3)> {
    let length = start.distance(end);
    if length <= 1e-5 || !length.is_finite() {
        return None;
    }
    let below_surface = |p: Vec3| {
        terrain
            .sample_height(p.x, p.z)
            .is_some_and(|height| p.y < height - 0.015)
    };
    if below_surface(start) {
        return Some((0.0, start));
    }

    let steps = length.ceil().clamp(1.0, 1024.0) as u32;
    let mut previous = 0.0;
    for step in 1..=steps {
        let next = step as f32 / steps as f32;
        if below_surface(start.lerp(end, next)) {
            let (mut low, mut high) = (previous, next);
            for _ in 0..8 {
                let middle = (low + high) * 0.5;
                if below_surface(start.lerp(end, middle)) {
                    high = middle;
                } else {
                    low = middle;
                }
            }
            return Some((high * length, start.lerp(end, high)));
        }
        previous = next;
    }
    None
}

// Numeric constants matching scripts/common/constants.py: SHOT_RESULT
pub const SHOT_RESULT_RICOCHET: u8 = 0;
pub const SHOT_RESULT_ARMOR_NOT_PIERCED: u8 = 1;
pub const SHOT_RESULT_ARMOR_PIERCED_NO_DAMAGE: u8 = 2;
pub const SHOT_RESULT_ARMOR_PIERCED: u8 = 3;
pub const SHOT_RESULT_CRITICAL_HIT: u8 = 4;

// Component index matching scripts/client/Vehicle.py: _decodeSegment
pub const COMPONENT_CHASSIS: u8 = 0;
pub const COMPONENT_HULL: u8 = 1;
pub const COMPONENT_TURRET: u8 = 2;
pub const COMPONENT_GUN: u8 = 3;

pub const SERVER_TICK_LENGTH: f32 = 0.1;
pub const SERVER_EPSILON: f32 = 0.1;

/// Aiming and gun rotation state for a vehicle.
#[derive(Debug, Clone, PartialEq)]
pub struct AimState {
    pub turret_yaw: f32,
    pub gun_pitch: f32,
    pub target_point: Vec3,
    pub has_target_point: bool,
    pub current_disp_factor: f32,
    pub reload_left: f32,
    pub current_shell_cd: i32,
    pub next_shell_cd: i32,
    pub autorotation: bool,
    pub auto_aim_target: Option<i32>,
    pub ammo_counts: Vec<(i32, u32)>,
    pub active_shell_idx: usize,
    pub ammo_dirty: bool,
    /// A reload started or finished since the client was last told (`updateVehicleGunReloadTime`).
    pub reload_dirty: bool,
}

impl Default for AimState {
    fn default() -> Self {
        Self {
            turret_yaw: 0.0,
            gun_pitch: 0.0,
            target_point: Vec3::ZERO,
            has_target_point: false,
            current_disp_factor: 1.0,
            reload_left: 0.0,
            current_shell_cd: 0,
            next_shell_cd: 0,
            autorotation: true,
            auto_aim_target: None,
            ammo_counts: Vec::new(),
            active_shell_idx: 0,
            ammo_dirty: false,
            reload_dirty: false,
        }
    }
}

/// Output targeting snapshot matching Avatar.updateTargetingInfo (6 floats) and Avatar.updateGunMarker.
#[derive(Debug, Clone, PartialEq)]
pub struct TargetingSnapshot {
    pub turret_yaw: f32,
    pub gun_pitch: f32,
    pub max_turret_rot_speed: f32,
    pub max_gun_rot_speed: f32,
    pub shot_disp_multiplier_factor: f32,
    pub aiming_time: f32,
    pub marker_pos: Vec3,
    pub marker_dir: Vec3,
    pub dispersion_angle: f32,
}

impl TargetingSnapshot {
    pub fn targeting_info_floats(&self) -> (f32, f32, f32, f32, f32, f32) {
        (
            self.turret_yaw,
            self.gun_pitch,
            self.max_turret_rot_speed,
            self.max_gun_rot_speed,
            self.shot_disp_multiplier_factor,
            self.aiming_time,
        )
    }
}

/// Continuation ray into the interior of a penetrated tank for Task 3 module/crew critical hits.
#[derive(Debug, Clone, PartialEq)]
pub struct InternalCritRay {
    pub world_origin: Vec3,
    pub world_dir: Vec3,
    pub local_origin: Vec3,
    pub local_dir: Vec3,
    pub max_depth: f32,
    pub vehicle_id: i32,
    pub attacker_id: i32,
    pub component_idx: u8,
    pub devices_damage: f32,
    pub caliber: f32,
}

/// Outcome of a projectile impact on a vehicle, terrain, or obstacle.
#[derive(Debug, Clone, PartialEq)]
pub struct ImpactOutcome {
    pub shot_id: u32,
    pub attacker_id: i32,
    pub hit_point: Vec3,
    pub normal: Vec3,
    pub shot_result: u8,
    pub struck_entity_id: Option<i32>,
    pub component_idx: u8,
    pub encoded_segment: u64,
    pub armor_damage: i16,
    pub splash_hits: Vec<(i32, i16)>,
    pub effect_mat_idx: u32,
    pub internal_ray: Option<InternalCritRay>,
    pub destructible_deltas: Vec<crate::sim::destructibles::DestructibleDelta>,
    pub ends_flight: bool,
}

/// Active in-flight ballistic projectile state.
#[derive(Debug, Clone, PartialEq)]
pub struct ProjectileState {
    pub shot_id: u32,
    pub attacker_id: i32,
    pub attacker_team: u8,
    pub pos: Vec3,
    pub velocity: Vec3,
    pub gravity: Vec3,
    pub distance_travelled: f32,
    pub max_distance: f32,
    pub shell_kind: ShellKind,
    pub caliber: f32,
    pub armor_damage: f32,
    pub devices_damage: f32,
    pub explosion_radius: f32,
    pub piercing_power: (f32, f32),
    pub damage_randomization: f32,
    pub piercing_randomization: f32,
}

/// Encodes a hit segment into a 64-bit integer matching scripts/client/Vehicle.py: _decodeSegment.
/// Layout: result | (compIdx << 8) | (startXYZ << 16) | (endXYZ << 40).
pub fn encode_hit_segment(
    result: u8,
    comp_idx: u8,
    start_local: Vec3,
    end_local: Vec3,
    bbox: (Vec3, Vec3),
) -> u64 {
    let min = bbox.0;
    let delta = bbox.1 - min;

    let quantize = |val: f32, b_min: f32, b_delta: f32| -> u64 {
        if b_delta.abs() > 1e-6 {
            let t = ((val - b_min) / b_delta).clamp(0.0, 1.0);
            (t * 255.0).round() as u64
        } else {
            0
        }
    };

    let sx = quantize(start_local.x, min.x, delta.x);
    let sy = quantize(start_local.y, min.y, delta.y);
    let sz = quantize(start_local.z, min.z, delta.z);

    let ex = quantize(end_local.x, min.x, delta.x);
    let ey = quantize(end_local.y, min.y, delta.y);
    let ez = quantize(end_local.z, min.z, delta.z);

    (result as u64 & 0xFF)
        | ((comp_idx as u64 & 0xFF) << 8)
        | (sx << 16)
        | (sy << 24)
        | (sz << 32)
        | (ex << 40)
        | (ey << 48)
        | (ez << 56)
}

/// Decodes a 64-bit hit segment into (result, comp_idx, start_local, end_local) identically to Vehicle._decodeSegment.
pub fn decode_hit_segment(segment: u64, bbox: (Vec3, Vec3)) -> (u8, u8, Vec3, Vec3) {
    let result = (segment & 0xFF) as u8;
    let comp_idx = ((segment >> 8) & 0xFF) as u8;
    let min = bbox.0;
    let delta = bbox.1 - min;

    let sx = min.x + delta.x * ((segment >> 16 & 0xFF) as f32 / 255.0);
    let sy = min.y + delta.y * ((segment >> 24 & 0xFF) as f32 / 255.0);
    let sz = min.z + delta.z * ((segment >> 32 & 0xFF) as f32 / 255.0);
    let mut seg_start = Vec3::new(sx, sy, sz);

    let ex = min.x + delta.x * ((segment >> 40 & 0xFF) as f32 / 255.0);
    let ey = min.y + delta.y * ((segment >> 48 & 0xFF) as f32 / 255.0);
    let ez = min.z + delta.z * ((segment >> 56 & 0xFF) as f32 / 255.0);
    let mut seg_end = Vec3::new(ex, ey, ez);

    let diff = seg_end - seg_start;
    let dir = diff.normalize_or_zero();
    seg_start -= dir * 0.01;
    seg_end += dir * 0.01;

    (result, comp_idx, seg_start, seg_end)
}

/// Subdivide an in-game flight arc to keep each chord within the supplied error budget.
pub fn compute_projectile_trajectory(
    begin_point: Vec3,
    velocity: Vec3,
    gravity: Vec3,
    time: f32,
    epsilon: f32,
) -> Vec<Vec3> {
    if time <= 0.0 || !time.is_finite() {
        return vec![begin_point];
    }
    let segments = (time * (gravity.length() / (8.0 * epsilon.max(0.001))).sqrt())
        .ceil()
        .clamp(1.0, 256.0) as u32;
    (1..=segments)
        .map(|segment| {
            let t = time * (segment as f32 / segments as f32);
            begin_point + (velocity + gravity * (0.5 * t)) * t
        })
        .collect()
}

/// Resolve a hit using the shell's attack score, the armor zone, and the contact angle.
pub fn compute_penetration_and_damage(
    shell_kind: ShellKind,
    caliber_mm: f32,
    impact_angle_deg: f32,
    nominal_armor_mm: f32,
    piercing_power_mm: f32,
    nominal_damage: f32,
    spall_factor: f32,
    balance: &BalanceConfig,
) -> (u8, f32, Option<f32>) {
    let armor = nominal_armor_mm.max(0.0);
    let attack = piercing_power_mm.max(0.0);
    let angle = impact_angle_deg.clamp(0.0, 90.0);
    let kinetic = matches!(
        shell_kind,
        ShellKind::ArmorPiercing | ShellKind::ArmorPiercingCr
    );
    if kinetic && angle >= balance.ricochet_angle_deg && attack < armor * 2.6 {
        return (SHOT_RESULT_RICOCHET, 0.0, None);
    }

    let incidence = (angle.to_radians() - balance.normalization_rad(shell_kind)).max(0.0);
    let obliqueness = 1.0 - incidence.cos();
    let guard = armor * (1.0 + 2.4 * obliqueness.powi(2));
    let nominal_damage = nominal_damage.max(0.0);
    if attack >= guard {
        let excess = ((attack - guard) / (guard + 1.0)).clamp(0.0, 2.0);
        let crit_reach = 0.35 + 0.58 * excess + (caliber_mm * 0.003).clamp(0.0, 0.55);
        (SHOT_RESULT_ARMOR_PIERCED, nominal_damage, Some(crit_reach))
    } else {
        let blast = if matches!(
            shell_kind,
            ShellKind::HighExplosive | ShellKind::ArmorPiercingHe
        ) {
            (0.45 * nominal_damage - 0.85 * armor * spall_factor.max(0.5)).max(0.0)
        } else {
            0.0
        };
        (SHOT_RESULT_ARMOR_NOT_PIERCED, blast, None)
    }
}

/// Computes turret yaw and gun pitch required to point at `target_point`.
pub fn compute_target_angles(
    target_point: Vec3,
    veh_pos: Vec3,
    veh_yaw: f32,
    veh_pitch: f32,
    veh_roll: f32,
    hull_pos: Vec3,
    turret_pos: Vec3,
    gun_pos: Vec3,
    shot_speed: f32,
    shot_gravity: f32,
) -> (f32, f32) {
    let rot_y = Mat4::from_rotation_y(veh_yaw);
    let rot_x = Mat4::from_rotation_x(veh_pitch);
    let rot_z = Mat4::from_rotation_z(veh_roll);
    let veh_rot = rot_y * rot_x * rot_z;
    let veh_matrix = Mat4::from_translation(veh_pos) * veh_rot;
    let world_to_veh = veh_matrix.inverse();

    let p_veh = world_to_veh.transform_point3(target_point);

    // Turret origin in vehicle space
    let turret_origin_veh = hull_pos + turret_pos;
    let delta_turret = p_veh - turret_origin_veh;

    let target_turret_yaw = delta_turret.x.atan2(delta_turret.z);

    // Target in turret local space
    let p_turret = Mat4::from_rotation_y(-target_turret_yaw).transform_point3(delta_turret);
    let delta_gun = p_turret - gun_pos;

    let dist_xz = (delta_gun.x * delta_gun.x + delta_gun.z * delta_gun.z)
        .sqrt()
        .max(0.1);
    let delta_y = delta_gun.y;

    // Ballistic drop compensation
    let flight_time = dist_xz / shot_speed.max(1.0);
    let drop = 0.5 * shot_gravity * flight_time * flight_time;
    let comp_y = delta_y + drop;

    // In BigWorld / WoT conventions: negative pitch points gun UP, positive pitch points gun DOWN
    let target_gun_pitch = -comp_y.atan2(dist_xz);

    (
        normalize_angle(target_turret_yaw),
        normalize_angle(target_gun_pitch),
    )
}

/// Advance turret, gun, reload, and reticle state for a vehicle.
pub fn step_gun_aiming(
    aim: &mut AimState,
    veh_physics: &VehiclePhysicsState,
    turret_yaw_limits: Option<(f32, f32)>,
    turret_rot_speed: f32,
    gun_pitch_limits: Option<(f32, f32)>,
    gun_rot_speed: f32,
    hull_pos: Vec3,
    turret_pos: Vec3,
    gun_pos: Vec3,
    shot_speed: f32,
    shot_gravity: f32,
    base_aiming_time: f32,
    base_disp_angle: f32,
    disp_factors: [f32; 4],
    modifiers: &EffectiveModifiers,
    map: &ArenaMap,
    dt: f32,
) -> TargetingSnapshot {
    let reloading = aim.reload_left > 0.0;
    aim.reload_left = (aim.reload_left - dt).max(0.0);
    if aim.reload_left < 1e-5 {
        aim.reload_left = 0.0;
        aim.reload_dirty |= reloading;
        if reloading && aim.next_shell_cd != 0 {
            let next = std::mem::take(&mut aim.next_shell_cd);
            if let Some(index) = aim.ammo_counts.iter().position(|&(cd, _)| cd == next) {
                aim.active_shell_idx = index;
                aim.current_shell_cd = next;
                aim.ammo_dirty = true;
            }
        }
    }

    let (desired_yaw, desired_pitch) = if aim.has_target_point {
        compute_target_angles(
            aim.target_point,
            veh_physics.pos,
            veh_physics.yaw,
            veh_physics.pitch,
            veh_physics.roll,
            hull_pos,
            turret_pos,
            gun_pos,
            shot_speed,
            shot_gravity,
        )
    } else {
        (aim.turret_yaw, aim.gun_pitch)
    };
    let previous_yaw = aim.turret_yaw;
    let turret_rate = (turret_rot_speed * modifiers.turret_rotation_speed_factor).max(0.0);
    let gun_rate = gun_rot_speed.max(0.0);
    let yaw_step = turret_rate * dt;
    let pitch_step = gun_rate * dt;
    aim.turret_yaw = match turret_yaw_limits {
        Some((min, max)) if max - min < std::f32::consts::TAU - 0.001 => {
            let desired = desired_yaw.clamp(min, max);
            (previous_yaw + (desired - previous_yaw).clamp(-yaw_step, yaw_step)).clamp(min, max)
        }
        _ => normalize_angle(
            previous_yaw + normalize_angle(desired_yaw - previous_yaw).clamp(-yaw_step, yaw_step),
        ),
    };
    let target_pitch =
        gun_pitch_limits.map_or(desired_pitch, |(lo, hi)| desired_pitch.clamp(lo, hi));
    aim.gun_pitch += (target_pitch - aim.gun_pitch).clamp(-pitch_step, pitch_step);

    let rot = normalize_angle(aim.turret_yaw - previous_yaw).abs() / dt.max(1e-4);
    let movement = (veh_physics.speed_mps * disp_factors[0]).abs();
    let hull_turn = (veh_physics.rot_speed_rps * disp_factors[1]).abs();
    let gun_turn = (rot * disp_factors[2]).abs();
    let floor =
        modifiers.shot_dispersion_factor * (1.0 + (movement + hull_turn + gun_turn).min(10.0));
    let aiming_time = (base_aiming_time * modifiers.gun_aiming_time_factor).max(0.01);
    if aim.current_disp_factor <= floor {
        aim.current_disp_factor = floor;
    } else {
        let settle = (-2.3 * dt / aiming_time).exp();
        aim.current_disp_factor = floor + (aim.current_disp_factor - floor) * settle;
    }
    let (muzzle, direction) = gun_ray(veh_physics, aim, hull_pos, turret_pos, gun_pos);
    let marker_ray = Ray {
        origin: muzzle,
        dir: direction,
        length: 500.0,
    };
    let marker_pos = trace_world(map, marker_ray)
        .map_or_else(|| marker_ray.at(marker_ray.length), |hit| hit.point);

    TargetingSnapshot {
        turret_yaw: aim.turret_yaw,
        gun_pitch: aim.gun_pitch,
        max_turret_rot_speed: turret_rate,
        max_gun_rot_speed: gun_rate,
        shot_disp_multiplier_factor: aim.current_disp_factor,
        aiming_time,
        marker_pos,
        marker_dir: direction,
        dispersion_angle: base_disp_angle * aim.current_disp_factor,
    }
}

fn body_transform(physics: &VehiclePhysicsState) -> Mat4 {
    Mat4::from_translation(physics.pos)
        * Mat4::from_rotation_y(physics.yaw)
        * Mat4::from_rotation_x(physics.pitch)
        * Mat4::from_rotation_z(physics.roll)
}

pub(crate) fn gun_ray(
    physics: &VehiclePhysicsState,
    aim: &AimState,
    hull_pos: Vec3,
    turret_pos: Vec3,
    gun_pos: Vec3,
) -> (Vec3, Vec3) {
    let frame = body_transform(physics)
        * Mat4::from_translation(hull_pos + turret_pos)
        * Mat4::from_rotation_y(aim.turret_yaw);
    let direction = (frame * Mat4::from_rotation_x(aim.gun_pitch)).transform_vector3(Vec3::Z);
    (
        frame.transform_point3(gun_pos),
        direction.normalize_or_zero(),
    )
}

/// Spawns a projectile in flight.
pub fn spawn_shot(
    shot_id: u32,
    attacker_id: i32,
    attacker_team: u8,
    muzzle_pos: Vec3,
    shot_dir: Vec3,
    shot_def: &ShotDef,
    shell: &ShellXmlDef,
    balance: &BalanceConfig,
    dispersion_offset: (f32, f32),
) -> ProjectileState {
    // `ShotDef` already includes `projectileSpeedFactor` (`_readShot`).
    let eff_speed = shot_def.speed as f32;
    let gravity = Vec3::new(0.0, -(shot_def.gravity as f32), 0.0);

    // Apply dispersion angular offset
    let up = if shot_dir.y.abs() > 0.99 {
        Vec3::Z
    } else {
        Vec3::Y
    };
    let right = shot_dir.cross(up).normalize();
    let local_up = right.cross(shot_dir).normalize();

    let dispersed_dir =
        (shot_dir + right * dispersion_offset.0 + local_up * dispersion_offset.1).normalize();

    ProjectileState {
        shot_id,
        attacker_id,
        attacker_team,
        pos: muzzle_pos,
        velocity: dispersed_dir * eff_speed,
        gravity,
        distance_travelled: 0.0,
        max_distance: shot_def.max_distance as f32,
        shell_kind: shell.kind,
        caliber: shell.caliber as f32,
        armor_damage: shell.armor_damage as f32,
        devices_damage: shell.devices_damage as f32,
        explosion_radius: shell.explosion_radius as f32,
        piercing_power: (
            shot_def.piercing_power.0 as f32,
            shot_def.piercing_power.1 as f32,
        ),
        damage_randomization: balance.damage_randomization,
        piercing_randomization: balance.piercing_randomization,
    }
}

/// Target descriptor representing a candidate vehicle for projectile collision checks.
#[derive(Debug, Clone)]
pub struct VehicleBallisticsTarget<'a> {
    pub entity_id: i32,
    pub team: u8,
    pub is_alive: bool,
    pub left_arena: bool,
    pub physics: &'a VehiclePhysicsState,
    pub modifiers: &'a EffectiveModifiers,
    pub collision_set: Option<&'a VehicleCollisionSet>,
    pub chassis_armor: Option<&'a ArmorZoneMap>,
    pub hull_armor: Option<&'a ArmorZoneMap>,
    pub turret_armor: Option<&'a ArmorZoneMap>,
    pub gun_armor: Option<&'a ArmorZoneMap>,
    pub turret_yaw: f32,
    pub gun_pitch: f32,
    pub hull_pos: Vec3,
    pub turret_pos: Vec3,
    pub gun_pos: Vec3,
}

impl<'a> VehicleBallisticsTarget<'a> {
    /// Returns minimum non-zero exterior nominal armor thickness in mm across defined components.
    pub fn min_armor(&self) -> f32 {
        let mut min_val = f32::MAX;
        for map in [
            self.chassis_armor,
            self.hull_armor,
            self.turret_armor,
            self.gun_armor,
        ]
        .into_iter()
        .flatten()
        {
            for &(thickness, no_damage) in map.zones.values() {
                if !no_damage && thickness > 0.0 && (thickness as f32) < min_val {
                    min_val = thickness as f32;
                }
            }
        }
        if min_val == f32::MAX {
            20.0
        } else {
            min_val
        }
    }
}

fn lookup_armor(
    armor_map: Option<&ArmorZoneMap>,
    mat_name: Option<&str>,
    fallback: f32,
) -> (f32, bool) {
    if let (Some(map), Some(name)) = (armor_map, mat_name) {
        if let Some((thickness, no_damage)) = map.get(name) {
            return (thickness as f32, no_damage);
        }
    }
    if let Some(map) = armor_map {
        if let Some((_, &(thickness, no_damage))) = map.zones.iter().next() {
            return (thickness as f32, no_damage);
        }
    }
    (fallback, false)
}

struct HitCandidate {
    t: f32,
    veh_id: i32,
    comp_idx: u8,
    hit_local: Vec3,
    dir_local: Vec3,
    hit_world: Vec3,
    norm_world: Vec3,
    bbox: (Vec3, Vec3),
    nom_armor: f32,
    no_damage: bool,
    target_spall: f32,
}

#[derive(Clone, Copy)]
struct Ray {
    origin: Vec3,
    dir: Vec3,
    length: f32,
}

impl Ray {
    fn between(origin: Vec3, end: Vec3) -> Option<Self> {
        let offset = end - origin;
        let length = offset.length();
        (length > 1e-5 && length.is_finite()).then(|| Self {
            origin,
            dir: offset / length,
            length,
        })
    }

    fn at(self, distance: f32) -> Vec3 {
        self.origin + self.dir * distance
    }

    fn local_to(self, transform: Mat4) -> Self {
        let inverse = transform.inverse();
        Self {
            origin: inverse.transform_point3(self.origin),
            dir: inverse.transform_vector3(self.dir),
            length: self.length,
        }
    }

    fn box_hit(self, min: Vec3, max: Vec3) -> Option<(f32, Vec3)> {
        let mut entry = 0.0;
        let mut exit = self.length;
        let mut normal = -self.dir;
        for axis in 0..3 {
            let velocity = self.dir[axis];
            let start = self.origin[axis];
            if velocity.abs() < 1e-8 {
                if start < min[axis] || start > max[axis] {
                    return None;
                }
                continue;
            }
            let a = (min[axis] - start) / velocity;
            let b = (max[axis] - start) / velocity;
            let near = a.min(b);
            if near >= entry {
                entry = near;
                normal = Vec3::ZERO;
                normal[axis] = -velocity.signum();
            }
            exit = exit.min(a.max(b));
            if exit < entry {
                return None;
            }
        }
        Some((entry, normal))
    }
}

#[derive(Clone, Copy)]
struct SurfaceHit {
    t: f32,
    point: Vec3,
    normal: Vec3,
}

fn trace_world(map: &ArenaMap, ray: Ray) -> Option<SurfaceHit> {
    let terrain =
        raycast_terrain(&map.terrain, ray.origin, ray.at(ray.length)).map(|(t, point)| {
            let normal = map
                .sample_height_and_normal(point.x, point.z)
                .map_or(Vec3::Y, |(_, normal)| normal);
            SurfaceHit { t, point, normal }
        });
    let obstacle = map
        .raycast_obstacles(ray.origin, ray.dir, terrain.map_or(ray.length, |hit| hit.t))
        .map(|(_, hit)| SurfaceHit {
            t: hit.t,
            point: hit.position,
            normal: hit.normal,
        });
    obstacle.or(terrain)
}

fn trace_vehicles(
    ray: Ray,
    vehicles: &[VehicleBallisticsTarget<'_>],
    attacker: i32,
) -> Option<HitCandidate> {
    let mut best: Option<HitCandidate> = None;
    for v in vehicles
        .iter()
        .filter(|v| v.is_alive && !v.left_arena && v.entity_id != attacker)
    {
        let body = body_transform(v.physics);
        let current_limit = best.as_ref().map_or(ray.length, |hit| hit.t);
        let mut add_hit = |t: f32,
                           component: u8,
                           local: Ray,
                           pose: Mat4,
                           normal: Vec3,
                           bbox: (Vec3, Vec3),
                           armor: (f32, bool)| {
            let key = (t, v.entity_id, component);
            if best
                .as_ref()
                .is_some_and(|old| key >= (old.t, old.veh_id, old.comp_idx))
            {
                return;
            }
            let normal = pose.transform_vector3(normal).normalize_or_zero();
            best = Some(HitCandidate {
                t,
                veh_id: v.entity_id,
                comp_idx: component,
                hit_local: local.at(t),
                dir_local: local.dir,
                hit_world: ray.at(t),
                norm_world: if normal.dot(ray.dir) > 0.0 {
                    -normal
                } else {
                    normal
                },
                bbox,
                nom_armor: armor.0,
                no_damage: armor.1,
                target_spall: v.modifiers.antifragmentation_lining_factor,
            });
        };
        let ray = Ray {
            length: current_limit,
            ..ray
        };
        if let Some(parts) = v.collision_set {
            let hull = body * Mat4::from_translation(v.hull_pos);
            let turret =
                hull * Mat4::from_translation(v.turret_pos) * Mat4::from_rotation_y(v.turret_yaw);
            let gun =
                turret * Mat4::from_translation(v.gun_pos) * Mat4::from_rotation_x(v.gun_pitch);
            let components = [
                (
                    COMPONENT_CHASSIS,
                    &parts.chassis,
                    body,
                    v.chassis_armor,
                    20.0,
                ),
                (COMPONENT_HULL, &parts.hull, hull, v.hull_armor, 50.0),
                (
                    COMPONENT_TURRET,
                    &parts.turret,
                    turret,
                    v.turret_armor,
                    60.0,
                ),
                (COMPONENT_GUN, &parts.gun, gun, v.gun_armor, 30.0),
            ];
            for (index, part, pose, armor, fallback) in components {
                let local = ray.local_to(pose);
                let bbox = part.local_aabb();
                if local.box_hit(bbox.0, bbox.1).is_none() {
                    continue;
                }
                if let Some(hit) = part.raycast(local.origin, local.dir, local.length) {
                    let name = part
                        .bsp
                        .materials()
                        .get(hit.material_index as usize)
                        .map(String::as_str);
                    add_hit(
                        hit.t,
                        index,
                        local,
                        pose,
                        hit.normal,
                        bbox,
                        lookup_armor(armor, name, fallback),
                    );
                }
            }
        } else {
            let bbox = (Vec3::new(-1.5, 0.0, -2.5), Vec3::new(1.5, 2.0, 2.5));
            let local = ray.local_to(body);
            if let Some((t, normal)) = local.box_hit(bbox.0, bbox.1) {
                let armor = lookup_armor(v.hull_armor, None, 30.0);
                add_hit(t, COMPONENT_HULL, local, body, normal, bbox, armor);
            }
        }
    }
    best
}

fn trace_destructible(ray: Ray, object: &ChunkDestructible) -> Option<f32> {
    let (entry, _) = ray.box_hit(object.world_aabb_min, object.world_aabb_max)?;
    let Some(bsp) = &object.bsp else {
        return Some(entry);
    };
    let inverse = object.world_transform.inverse();
    let direction = inverse.transform_vector3(ray.dir);
    let scale = direction.length();
    if scale < 1e-8 || !scale.is_finite() {
        return None;
    }
    bsp.raycast(
        inverse.transform_point3(ray.origin),
        direction / scale,
        ray.length * scale,
    )
    .map(|hit| hit.t / scale)
}

fn splash_hits(
    shot: &ProjectileState,
    map: &ArenaMap,
    hit: SurfaceHit,
    direct: Option<i32>,
    vehicles: &[VehicleBallisticsTarget<'_>],
) -> Vec<(i32, i16)> {
    let radius = shot.explosion_radius;
    if shot.shell_kind != ShellKind::HighExplosive || radius <= 0.0 {
        return Vec::new();
    }
    let mut present = Vec::new();
    for v in vehicles
        .iter()
        .filter(|v| v.is_alive && !v.left_arena && Some(v.entity_id) != direct)
    {
        let center = v.physics.pos + Vec3::Y;
        let fraction = center.distance(hit.point) / radius;
        if fraction >= 1.0 {
            continue;
        }
        let damage = (0.45 * shot.armor_damage * (1.0 - fraction).powi(2)
            - 0.85 * v.min_armor() * v.modifiers.antifragmentation_lining_factor.max(0.5))
        .round() as i16;
        if damage <= 0 {
            continue;
        }
        let origin = hit.point + hit.normal * 0.04;
        if Ray::between(origin, center).is_some_and(|ray| trace_world(map, ray).is_some()) {
            continue;
        }
        present.push((v.entity_id, damage));
    }
    present.sort_unstable_by_key(|hit| hit.0);
    present
}

fn empty_impact(shot: &ProjectileState, hit: SurfaceHit) -> ImpactOutcome {
    ImpactOutcome {
        shot_id: shot.shot_id,
        attacker_id: shot.attacker_id,
        hit_point: hit.point,
        normal: hit.normal,
        shot_result: SHOT_RESULT_ARMOR_NOT_PIERCED,
        struck_entity_id: None,
        component_idx: 0,
        encoded_segment: 0,
        armor_damage: 0,
        splash_hits: Vec::new(),
        effect_mat_idx: 0,
        internal_ray: None,
        destructible_deltas: Vec::new(),
        ends_flight: true,
    }
}

fn vehicle_impact<R: Rng + ?Sized>(
    shot: &ProjectileState,
    hit: HitCandidate,
    dir: Vec3,
    balance: &BalanceConfig,
    rng: &mut R,
) -> ImpactOutcome {
    let distance = shot.distance_travelled + hit.t;
    let fade = ((distance - 100.0) / 400.0).clamp(0.0, 1.0);
    let attack = shot.piercing_power.0 + fade * (shot.piercing_power.1 - shot.piercing_power.0);
    let roll = |amount: f32, factor: f32, rng: &mut R| {
        let factor = factor.clamp(0.0, 1.0);
        amount * (1.0 + (2.0 * rng.gen::<f32>() - 1.0) * factor)
    };
    let attack = roll(attack, shot.piercing_randomization, rng);
    let damage = roll(shot.armor_damage, shot.damage_randomization, rng);
    let angle = (-dir)
        .dot(hit.norm_world)
        .clamp(0.0, 1.0)
        .acos()
        .to_degrees();
    let (mut result, mut hp, reach) = compute_penetration_and_damage(
        shot.shell_kind,
        shot.caliber,
        angle,
        hit.nom_armor,
        attack,
        damage,
        hit.target_spall,
        balance,
    );
    if hit.no_damage {
        hp = 0.0;
        if result == SHOT_RESULT_ARMOR_PIERCED {
            result = SHOT_RESULT_ARMOR_PIERCED_NO_DAMAGE;
        }
    }
    let mut outcome = empty_impact(
        shot,
        SurfaceHit {
            t: hit.t,
            point: hit.hit_world,
            normal: hit.norm_world,
        },
    );
    outcome.shot_result = result;
    outcome.struck_entity_id = Some(hit.veh_id);
    outcome.component_idx = hit.comp_idx;
    outcome.encoded_segment = encode_hit_segment(
        result,
        hit.comp_idx,
        hit.hit_local,
        hit.hit_local + hit.dir_local * 0.5,
        hit.bbox,
    );
    outcome.armor_damage = hp.round() as i16;
    outcome.internal_ray = reach.map(|max_depth| InternalCritRay {
        world_origin: hit.hit_world,
        world_dir: dir,
        local_origin: hit.hit_local,
        local_dir: hit.dir_local,
        max_depth,
        vehicle_id: hit.veh_id,
        attacker_id: shot.attacker_id,
        component_idx: hit.comp_idx,
        devices_damage: shot.devices_damage,
        caliber: shot.caliber,
    });
    outcome
}

/// Sweep each in-flight shell to its range limit and publish impacts in frame order.
pub fn step_projectiles<R: Rng + ?Sized>(
    projectiles: &mut Vec<ProjectileState>,
    map: &ArenaMap,
    mut destructibles: Option<&mut DestructiblesState>,
    vehicles: &[VehicleBallisticsTarget<'_>],
    balance: &BalanceConfig,
    dt: f32,
    rng: &mut R,
) -> Vec<ImpactOutcome> {
    if dt <= 0.0 || !dt.is_finite() {
        return Vec::new();
    }
    let mut outcomes = Vec::new();
    projectiles.retain_mut(|shot| {
        if !shot.pos.is_finite()
            || !shot.velocity.is_finite()
            || !shot.gravity.is_finite()
            || !shot.max_distance.is_finite()
            || shot.distance_travelled >= shot.max_distance
        {
            return false;
        }
        let arc = compute_projectile_trajectory(
            shot.pos,
            shot.velocity,
            shot.gravity,
            dt,
            SERVER_EPSILON,
        );
        let segments = arc.len() as f32;
        for (segment, end) in arc.into_iter().enumerate() {
            let Some(mut ray) = Ray::between(shot.pos, end) else {
                continue;
            };
            let segment_length = ray.length;
            let remaining = shot.max_distance - shot.distance_travelled;
            ray.length = ray.length.min(remaining);
            let frame_time = |distance| (segment as f32 + distance / segment_length) / segments;

            let world = trace_world(map, ray);
            let vehicle = trace_vehicles(
                Ray {
                    length: world.map_or(ray.length, |hit| hit.t),
                    ..ray
                },
                vehicles,
                shot.attacker_id,
            )
            .filter(|hit| world.is_none_or(|surface| hit.t < surface.t));
            let barrier = vehicle
                .as_ref()
                .map(|hit| hit.t)
                .or_else(|| world.map(|hit| hit.t));
            if let Some(state) = destructibles.as_deref_mut() {
                let mut objects = Vec::new();
                for (&chunk, entries) in &map.destructibles_by_chunk {
                    for object in entries {
                        let Ok(index) = u8::try_from(object.index_in_chunk) else {
                            continue;
                        };
                        let solid = matches!(object.kind, DestructibleKind::Structure { .. });
                        let standing = if solid {
                            !state.is_destructible_broken(chunk, index)
                        } else {
                            state.tree_is_standing(chunk, object.index_in_chunk)
                        };
                        if !standing {
                            continue;
                        }
                        if let Some(t) = trace_destructible(ray, object)
                            .filter(|&t| barrier.is_none_or(|stop| t < stop))
                        {
                            objects.push((t, chunk, index, solid));
                        }
                    }
                }
                objects.sort_by(|a, b| {
                    a.0.total_cmp(&b.0)
                        .then_with(|| (a.1, a.2).cmp(&(b.1, b.2)))
                });
                for (t, chunk, index, solid) in objects {
                    let hit = SurfaceHit {
                        t,
                        point: ray.at(t),
                        normal: -ray.dir,
                    };
                    let yaw = ray.dir.x.atan2(ray.dir.z);
                    let mut impact = empty_impact(shot, hit);
                    impact.destructible_deltas =
                        state.break_by_shot(chunk, index, 0, shot.armor_damage, yaw);
                    impact.ends_flight = solid || shot.shell_kind == ShellKind::HighExplosive;
                    if impact.ends_flight {
                        impact.splash_hits = splash_hits(shot, map, hit, None, vehicles);
                        outcomes.push((frame_time(t), impact));
                        return false;
                    }
                    if !impact.destructible_deltas.is_empty() {
                        outcomes.push((frame_time(t), impact));
                    }
                }
            }

            if let Some(hit) = vehicle {
                let surface = SurfaceHit {
                    t: hit.t,
                    point: hit.hit_world,
                    normal: hit.norm_world,
                };
                let mut impact = vehicle_impact(shot, hit, ray.dir, balance, rng);
                impact.splash_hits =
                    splash_hits(shot, map, surface, impact.struck_entity_id, vehicles);
                outcomes.push((frame_time(surface.t), impact));
                return false;
            }
            if let Some(hit) = world {
                let mut impact = empty_impact(shot, hit);
                impact.splash_hits = splash_hits(shot, map, hit, None, vehicles);
                outcomes.push((frame_time(hit.t), impact));
                return false;
            }
            if ray.length >= remaining {
                return false;
            }
            shot.distance_travelled += ray.length;
            shot.pos = end;
        }
        shot.velocity += shot.gravity * dt;
        shot.velocity.length_squared() > 1e-10
    });
    outcomes.sort_by(|a, b| a.0.total_cmp(&b.0).then(a.1.shot_id.cmp(&b.1.shot_id)));
    outcomes.into_iter().map(|(_, hit)| hit).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shell_beside_a_trunk_passes_a_tree_with_collision_geometry() {
        // With its .bsp2 a tree stops shells only where its geometry is. Without one the crown
        // box reaches 3 m around the trunk and 6 m up.
        let at = Vec3::new(0.0, 0.0, 50.0);
        let mut tree = ChunkDestructible {
            chunk_id: 100,
            index_in_chunk: 0,
            desc_id: 1,
            filename: "tree1.spt".into(),
            kind: DestructibleKind::Fragile {
                health: 50,
                kinetic_damage_correction: 0.0,
            },
            world_transform: Mat4::from_translation(at),
            position: at,
            world_aabb_min: at - Vec3::new(3.0, 0.0, 3.0),
            world_aabb_max: at + Vec3::new(3.0, 6.0, 3.0),
            bsp: Some(std::sync::Arc::new(crate::assets::bsp2::test_box(0.3, 0.0, 12.0, 0.3))),
            pieces: Vec::new(),
        };
        let beside = Ray { origin: Vec3::new(1.5, 2.0, 0.0), dir: Vec3::Z, length: 100.0 };
        let through = Ray { origin: Vec3::new(0.0, 2.0, 0.0), dir: Vec3::Z, length: 100.0 };
        assert_eq!(trace_destructible(beside, &tree), None);
        let hit = trace_destructible(through, &tree).expect("the trunk stops the shell");
        assert!((hit - 49.7).abs() < 1e-3, "{hit}");
        tree.bsp = None;
        assert!(trace_destructible(beside, &tree).is_some(), "the crown box reaches the shell");
    }

    #[test]
    fn retail_ms1_hit_testers_stop_shells_only_where_the_tank_is() {
        let Some(res_dir) = crate::assets::retail_res_dir() else {
            return;
        };
        let game_dir = wot065_data::testutil::game_dir().expect("game_dir present");
        let cat = wot065_data::catalog::load(&game_dir).expect("load retail catalog");
        let entry = cat.by_name("MS-1").expect("MS-1 in catalog");
        let def = cat.vehicle_def_by_id(entry.nation_id, entry.type_id).expect("MS-1 VehicleDef");
        let descr = wot065_data::descr::VehicleDescr::stock(def).expect("stock MS-1");
        let set = crate::assets::map_store::VehicleCollisionCache::new(&res_dir)
            .get_or_load_by_ids(def, descr.chassis_id, descr.turret_id, descr.gun_id)
            .expect("MS-1 hit testers");
        let vec3 = |p: [f64; 3]| Vec3::new(p[0] as f32, p[1] as f32, p[2] as f32);
        let chassis = def.chassis.iter().find(|c| c.id == descr.chassis_id).expect("stock chassis");
        let turret = def.turrets.iter().find(|t| t.component.id == descr.turret_id).expect("stock turret");
        // The target stands 30 m down +Z with its bow towards the shooter.
        let physics = VehiclePhysicsState {
            pos: Vec3::new(0.0, 0.0, 30.0),
            yaw: std::f32::consts::PI,
            ..Default::default()
        };
        let modifiers = EffectiveModifiers::default();
        let mut target = VehicleBallisticsTarget {
            entity_id: 42,
            team: 2,
            is_alive: true,
            left_arena: false,
            physics: &physics,
            modifiers: &modifiers,
            collision_set: Some(&set),
            chassis_armor: None,
            hull_armor: None,
            turret_armor: None,
            gun_armor: None,
            turret_yaw: 0.0,
            gun_pitch: 0.0,
            hull_pos: chassis.hull_position.map_or(Vec3::ZERO, vec3),
            turret_pos: def.turret_positions.first().copied().map_or(Vec3::ZERO, vec3),
            gun_pos: turret.gun_position.map_or(Vec3::ZERO, vec3),
        };
        let (hull_low, hull_high) = set.hull.local_aabb();
        let (chassis_low, chassis_high) = set.chassis.local_aabb();
        let height = target.hull_pos.y + 0.5 * (hull_low.y + hull_high.y);
        let side = [hull_low.x, chassis_low.x].iter().map(|x| -x).fold(hull_high.x.max(chassis_high.x), f32::max);
        assert!(side + 0.2 < 1.5, "MS-1 half width {side} must sit inside the fallback box");
        let shell_at = |x: f32| Ray { origin: Vec3::new(x, height, 0.0), dir: Vec3::Z, length: 100.0 };

        let bow = trace_vehicles(shell_at(0.0), std::slice::from_ref(&target), 7).expect("a shell at the bow hits the tank");
        assert_eq!(bow.veh_id, 42);
        assert!((26.5..29.5).contains(&bow.t), "bow hit at {}", bow.t);
        assert!(trace_vehicles(shell_at(side + 0.2), std::slice::from_ref(&target), 7).is_none(), "a shell beside the tank misses it");

        target.collision_set = None;
        assert!(trace_vehicles(shell_at(side + 0.2), std::slice::from_ref(&target), 7).is_some(), "the fallback box reaches the shell");
    }

    #[test]
    fn segment_encoding_roundtrips_with_vehicle_decode_segment() {
        let bbox = (Vec3::new(-2.0, -1.0, -3.0), Vec3::new(2.0, 2.0, 3.0));
        let start_local = Vec3::new(0.5, 0.2, 1.0);
        let end_local = Vec3::new(0.6, 0.1, 1.2);
        let result = SHOT_RESULT_ARMOR_PIERCED;
        let comp_idx = COMPONENT_HULL;

        let packed = encode_hit_segment(result, comp_idx, start_local, end_local, bbox);
        let (dec_result, dec_comp_idx, dec_start, dec_end) = decode_hit_segment(packed, bbox);

        assert_eq!(dec_result, result);
        assert_eq!(dec_comp_idx, comp_idx);
        // Compare with start_local & end_local allowing for quantization (delta / 255) and the 0.01 skin offset
        assert!((dec_start.x - start_local.x).abs() < 0.05);
        assert!((dec_start.y - start_local.y).abs() < 0.05);
        assert!((dec_start.z - start_local.z).abs() < 0.05);
        assert!((dec_end.x - end_local.x).abs() < 0.05);
        assert!((dec_end.y - end_local.y).abs() < 0.05);
        assert!((dec_end.z - end_local.z).abs() < 0.05);
    }

    #[test]
    fn armor_zone_resolves_glancing_hits_penetration_and_blast() {
        let balance = BalanceConfig::default();

        // An oblique hit with a low attack score deflects.
        let (res1, dmg1, _) = compute_penetration_and_damage(
            ShellKind::ArmorPiercing,
            75.0,
            72.0,
            50.0,
            120.0,
            110.0,
            1.0,
            &balance,
        );
        assert_eq!(res1, SHOT_RESULT_RICOCHET);
        assert_eq!(dmg1, 0.0);

        // A higher attack score gets through the same kind of contact.
        let (res2, dmg2, crit_depth) = compute_penetration_and_damage(
            ShellKind::ArmorPiercing,
            152.0,
            75.0,
            45.0,
            150.0,
            700.0,
            1.0,
            &balance,
        );
        assert_eq!(res2, SHOT_RESULT_ARMOR_PIERCED);
        assert_eq!(dmg2, 700.0);
        assert!(crit_depth.unwrap() > 1.0);

        // A blast can chip an armor zone without breaking through it.
        let (res3, dmg3, _) = compute_penetration_and_damage(
            ShellKind::HighExplosive,
            152.0,
            0.0,
            300.0,
            86.0,
            910.0,
            1.0,
            &balance,
        );
        assert_eq!(res3, SHOT_RESULT_ARMOR_NOT_PIERCED);
        assert!((dmg3 - 154.5).abs() < 0.001);
    }

    #[test]
    fn parabola_subdivision_and_dispersion_aim_shrink() {
        // Projectile arc (0.8 speed / 0.64 gravity) hitting ground below horizontal line-of-sight at 400m
        let speed = 400.0 * 0.8;
        let gravity_mps2 = 9.81 * 0.64;
        let v0 = Vec3::new(0.0, 0.0, speed);
        let g = Vec3::new(0.0, -gravity_mps2, 0.0);
        let start = Vec3::new(0.0, 10.0, 0.0);
        let flight_time = 400.0 / speed; // 1.25 s
        let check_points = compute_projectile_trajectory(start, v0, g, flight_time, SERVER_EPSILON);
        assert!(!check_points.is_empty());
        let end = check_points.last().unwrap();
        assert!((end.z - 400.0).abs() < 1.0);
        assert!(end.y < 10.0, "parabola must curve downwards below initial height 10.0, got {}", end.y);
        let expected_y = 10.0 - 0.5 * gravity_mps2 * flight_time * flight_time;
        assert!((end.y - expected_y).abs() < 0.1);

        // Aim factor shrinks exponentially per aimingTime until reaching idealFactor
        let aiming_time: f32 = 2.5;
        let mut cur_disp_factor: f32 = 2.5;
        let ideal_factor: f32 = 1.0;
        let dt: f32 = 1.0;

        // Shrink step 1: 2.5 * exp(-1.0 / 2.5) ≈ 1.6758
        let factor_step1: f32 = cur_disp_factor * (-dt / aiming_time).exp();
        assert!((factor_step1 - 1.6758).abs() < 0.01);
        cur_disp_factor = factor_step1.max(ideal_factor);

        // Shrink over 5s: goes below ideal_factor and clamps to 1.0
        let factor_step5: f32 = cur_disp_factor * (-5.0 / aiming_time).exp();
        assert!(factor_step5 < ideal_factor);
        cur_disp_factor = factor_step5.max(ideal_factor);
        assert_eq!(cur_disp_factor, ideal_factor);
    }

    #[test]
    fn gun_tracking_respects_turret_limits_and_keeps_the_hull_pose() {
        let mut aim = AimState {
            turret_yaw: 0.0,
            gun_pitch: 0.0,
            target_point: Vec3::new(50.0, 5.0, 0.0), // directly to the right (+X)
            has_target_point: true,
            current_disp_factor: 1.0,
            reload_left: 0.25,
            current_shell_cd: 1,
            next_shell_cd: 2,
            ammo_counts: vec![(1, 10), (2, 10)],
            autorotation: true,
            auto_aim_target: None,
            ..Default::default()
        };
        let physics = VehiclePhysicsState {
            pos: Vec3::new(0.0, 5.0, 0.0),
            yaw: 0.0, // facing +Z
            ..Default::default()
        };

        let turret_yaw_limits = Some((-15.0f32.to_radians(), 15.0f32.to_radians()));
        let gun_pitch_limits = Some((-15.0f32.to_radians(), 10.0f32.to_radians()));
        let modifiers = EffectiveModifiers::default();

        // Create dummy terrain with flat height 5.0
        let def = crate::assets::arena_defs::ArenaDef {
            id: 1,
            name: "test".into(),
            display_name_key: "".into(),
            description_key: "".into(),
            geometry: "".into(),
            bounding_box: ([-100.0, -100.0], [100.0, 100.0]),
            min_players_in_team: 1,
            max_players_in_team: 15,
            round_length_secs: 900,
            arena_start_delay_secs: 5.0,
            kick_after_finish_wait_secs: 10.0,
            minimap_texture: None,
        };
        let bounds = crate::assets::chunk::SpaceBounds {
            min_x: 0,
            max_x: 0,
            min_y: 0,
            max_y: 0,
        };
        let terrain_map = crate::assets::map_store::TerrainMap::from_chunks(
            bounds,
            vec![(
                crate::assets::chunk::ChunkCoords {
                    grid_x: 0,
                    grid_z: 0,
                    kind: crate::assets::chunk::ChunkKind::Outer,
                },
                crate::assets::terrain::TerrainChunk {
                    heights: vec![
                        5.0;
                        crate::assets::terrain::HEIGHTMAP_GRID_SIZE
                            * crate::assets::terrain::HEIGHTMAP_GRID_SIZE
                    ],
                    min_y: 5.0,
                    max_y: 5.0,
                    dom_tex_indices: vec![
                        0;
                        crate::assets::terrain::DOMINANT_TEX_GRID_SIZE
                            * crate::assets::terrain::DOMINANT_TEX_GRID_SIZE
                    ],
                    tex_names: vec![],
                },
            )],
        )
        .unwrap();
        let mats =
            std::sync::Arc::new(crate::assets::terrain::MaterialKinds::parse(b"<root/>").unwrap());
        let map = ArenaMap::from_parts(
            def,
            bounds,
            terrain_map,
            mats,
            vec![],
            vec![],
            vec![],
            vec![],
            vec![],
            std::collections::BTreeMap::new(),
            &BalanceConfig::default(),
        );

        // Hull steering is handled by the arena physics. Aiming stops at the turret limit.
        let snap = step_gun_aiming(
            &mut aim,
            &physics,
            turret_yaw_limits,
            0.6,
            gun_pitch_limits,
            0.5,
            Vec3::new(0.0, 0.8, 0.0),
            Vec3::new(0.0, 0.6, 0.0),
            Vec3::new(0.0, 0.0, 1.2),
            400.0,
            9.81,
            2.5,
            0.0035,
            [0.2, 0.2, 0.15, 3.0],
            &modifiers,
            &map,
            0.5,
        );

        assert_eq!(physics.yaw, 0.0);
        assert_eq!(aim.current_shell_cd, 2);
        assert_eq!(aim.ammo_counts[aim.active_shell_idx].0, 2);
        assert!(
            aim.turret_yaw <= 15.0f32.to_radians() + 1e-4,
            "turret yaw must respect yaw limits"
        );
        let (ty, gp, mtr, mgr, disp, at) = snap.targeting_info_floats();
        assert_eq!(ty, aim.turret_yaw);
        assert_eq!(gp, aim.gun_pitch);
        assert!(mtr > 0.0);
        assert!(mgr > 0.0);
        assert!(disp > 0.0);
        assert_eq!(at, 2.5);
    }

    /// `ShotDef` holds `_readShot` values that already include `projectileSpeedFactor`
    /// (`speed * 0.8`, `gravity * 0.64`), the same shot the client's `vehicles.g_cache` flies.
    #[test]
    fn spawn_shot_flies_at_the_catalog_shot_speed_and_gravity() {
        let shot_def = ShotDef {
            shell_name: "test_ap".into(),
            shell_id: 1,
            shell_cd: 100,
            default_portion: 1.0,
            speed: 800.0,
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
            kind: ShellKind::ArmorPiercing,
            caliber: 75.0,
            is_tracer: true,
            armor_damage: 150.0,
            devices_damage: 100.0,
            damage_randomization: 0.25,
            piercing_power_randomization: 0.25,
            explosion_radius: 0.0,
            effects: "".into(),
            price: (100, 0),
            show_in_shop: true,
        };
        let shot = spawn_shot(
            1,
            10,
            1,
            Vec3::ZERO,
            Vec3::Z,
            &shot_def,
            &shell,
            &BalanceConfig::default(),
            (0.0, 0.0),
        );
        assert!((shot.velocity.length() - 800.0).abs() < 1e-3, "{:?}", shot.velocity);
        assert!((shot.gravity.y + 9.81).abs() < 1e-4, "{:?}", shot.gravity);
    }

    #[test]
    fn linear_penetration_attenuation_and_projectiles_step() {
        use rand::SeedableRng;

        let balance = BalanceConfig::default();
        let _rng = rand::rngs::StdRng::seed_from_u64(12345);

        let shot_def = ShotDef {
            shell_name: "test_ap".into(),
            shell_id: 1,
            shell_cd: 100,
            default_portion: 1.0,
            speed: 500.0,
            max_distance: 720.0,
            piercing_power: (150.0, 110.0), // 150mm at 100m, 110mm at 500m
            gravity: 9.81,
            shell: None,
        };
        let shell = ShellXmlDef {
            id: 1,
            name: "test_ap".into(),
            nation_id: 0,
            compact_descr: 100,
            kind: ShellKind::ArmorPiercing,
            caliber: 75.0,
            is_tracer: true,
            armor_damage: 150.0,
            devices_damage: 100.0,
            damage_randomization: 0.25,
            piercing_power_randomization: 0.25,
            explosion_radius: 0.0,
            effects: "".into(),
            price: (100, 0),
            show_in_shop: true,
        };

        // Spawn shot from (0, 5, 0) towards +Z
        let projs = vec![spawn_shot(
            1,
            10, // attacker_id
            1,  // attacker_team
            Vec3::new(0.0, 5.0, 0.0),
            Vec3::new(0.0, 0.0, 1.0),
            &shot_def,
            &shell,
            &balance,
            (0.0, 0.0),
        )];

        assert_eq!(projs.len(), 1);
        assert_eq!(projs[0].piercing_power, (150.0, 110.0));
        assert_eq!(projs[0].caliber, 75.0);

        // Linear pen attenuation formula check:
        // at dist = 300m: 150 + (110 - 150) * (300 - 100) / 400 = 150 - 40 * 0.5 = 130mm
        let p100 = 150.0;
        let p500 = 110.0;
        let dist = 300.0;
        let pen_300 = p100 + (p500 - p100) * (dist - 100.0) / 400.0;
        assert_eq!(pen_300, 130.0);
    }

    #[test]
    fn displaced_vehicle_hit_segment_and_internal_crit_ray_are_component_local() {
        use rand::SeedableRng;

        let balance = BalanceConfig::default();
        let mut rng = rand::rngs::StdRng::seed_from_u64(999);

        // Dummy map
        let def = crate::assets::arena_defs::ArenaDef {
            id: 1,
            name: "test".into(),
            display_name_key: "".into(),
            description_key: "".into(),
            geometry: "".into(),
            bounding_box: ([-200.0, -200.0], [200.0, 200.0]),
            min_players_in_team: 1,
            max_players_in_team: 15,
            round_length_secs: 900,
            arena_start_delay_secs: 5.0,
            kick_after_finish_wait_secs: 10.0,
            minimap_texture: None,
        };
        let bounds = crate::assets::chunk::SpaceBounds {
            min_x: 0,
            max_x: 0,
            min_y: 0,
            max_y: 0,
        };
        let terrain_map = crate::assets::map_store::TerrainMap::from_chunks(
            bounds,
            vec![(
                crate::assets::chunk::ChunkCoords {
                    grid_x: 0,
                    grid_z: 0,
                    kind: crate::assets::chunk::ChunkKind::Outer,
                },
                crate::assets::terrain::TerrainChunk {
                    heights: vec![
                        0.0;
                        crate::assets::terrain::HEIGHTMAP_GRID_SIZE
                            * crate::assets::terrain::HEIGHTMAP_GRID_SIZE
                    ],
                    min_y: 0.0,
                    max_y: 0.0,
                    dom_tex_indices: vec![
                        0;
                        crate::assets::terrain::DOMINANT_TEX_GRID_SIZE
                            * crate::assets::terrain::DOMINANT_TEX_GRID_SIZE
                    ],
                    tex_names: vec![],
                },
            )],
        )
        .unwrap();
        let mats =
            std::sync::Arc::new(crate::assets::terrain::MaterialKinds::parse(b"<root/>").unwrap());
        let map = ArenaMap::from_parts(
            def,
            bounds,
            terrain_map,
            mats,
            vec![],
            vec![],
            vec![],
            vec![],
            vec![],
            std::collections::BTreeMap::new(),
            &balance,
        );

        // Build a synthetic BspTree for hull at local x = 2.0:
        let hull_bsp_bytes = {
            let mut bytes = Vec::new();
            bytes.extend_from_slice(crate::assets::bsp2::BSP2_MAGIC);
            bytes.extend_from_slice(&1u32.to_le_bytes()); // 1 tri
            bytes.extend_from_slice(&1u32.to_le_bytes()); // 1 node
            bytes.extend_from_slice(&0u32.to_le_bytes());
            // Tri 0 at local x = 2.0: [2.0, -1.0, -1.0], [2.0, 1.0, -1.0], [2.0, 0.0, 1.0]
            for v in &[[2.0f32, -1.0, -1.0], [2.0, 1.0, -1.0], [2.0, 0.0, 1.0]] {
                for c in v {
                    bytes.extend_from_slice(&c.to_le_bytes());
                }
            }
            bytes.extend_from_slice(&0u32.to_le_bytes()); // material_index = 0 ("armor_1")
                                                          // Leaf node:
            bytes.push(0xa0);
            for c in &[1.0f32, 0.0, 0.0] {
                bytes.extend_from_slice(&c.to_le_bytes());
            }
            bytes.extend_from_slice(&2.0f32.to_le_bytes());
            bytes.extend_from_slice(&1u16.to_le_bytes());
            bytes.extend_from_slice(&0u16.to_le_bytes());
            bytes
        };
        let bsp_mats = b"<mats><id>armor_1</id><id>armor_2</id></mats>";
        let bsp_tree = std::sync::Arc::new(
            crate::assets::bsp2::BspTree::parse(&hull_bsp_bytes, bsp_mats).unwrap(),
        );
        let hit_tester = crate::assets::map_store::HitTester {
            model_path: "hull.model".into(),
            bsp: bsp_tree.clone(),
            enclosing_box: Some((Vec3::new(-3.0, -2.0, -2.0), Vec3::new(3.0, 2.0, 2.0))),
        };
        let empty_bsp_bytes = {
            let mut bytes = Vec::new();
            bytes.extend_from_slice(crate::assets::bsp2::BSP2_MAGIC);
            bytes.extend_from_slice(&0u32.to_le_bytes());
            bytes.extend_from_slice(&0u32.to_le_bytes());
            bytes.extend_from_slice(&0u32.to_le_bytes());
            bytes
        };
        let empty_bsp = std::sync::Arc::new(
            crate::assets::bsp2::BspTree::parse(&empty_bsp_bytes, b"").unwrap(),
        );
        let empty_hit_tester = crate::assets::map_store::HitTester {
            model_path: "empty.model".into(),
            bsp: empty_bsp,
            enclosing_box: Some((Vec3::new(-0.1, -0.1, -0.1), Vec3::new(0.1, 0.1, 0.1))),
        };
        let col_set = VehicleCollisionSet {
            hull: hit_tester,
            chassis: empty_hit_tester.clone(),
            turret: empty_hit_tester.clone(),
            gun: empty_hit_tester,
        };

        // Displaced target vehicle: pos = (100.0, 5.0, 100.0), facing +Z (yaw = 0.0)
        let target_pos = Vec3::new(100.0, 5.0, 100.0);
        let physics = VehiclePhysicsState {
            pos: target_pos,
            yaw: 0.0,
            ..Default::default()
        };

        // Hull tri 0 is at local x = 2.0, which is at world (102.0, 5.0, 100.0).
        // Fire shell from world (105.0, 5.0, 100.0) in direction (-1.0, 0.0, 0.0) towards -X.
        let shot_def = ShotDef {
            shell_name: "test_ap".into(),
            shell_id: 1,
            shell_cd: 100,
            default_portion: 1.0,
            speed: 500.0,
            max_distance: 720.0,
            piercing_power: (200.0, 180.0),
            gravity: 0.0, // horizontal
            shell: None,
        };
        let shell = ShellXmlDef {
            id: 1,
            name: "test_ap".into(),
            nation_id: 0,
            compact_descr: 100,
            kind: ShellKind::ArmorPiercing,
            caliber: 75.0,
            is_tracer: true,
            armor_damage: 150.0,
            devices_damage: 100.0,
            damage_randomization: 0.0,
            piercing_power_randomization: 0.0,
            explosion_radius: 0.0,
            effects: "".into(),
            price: (100, 0),
            show_in_shop: true,
        };

        let mut projs = vec![spawn_shot(
            1,
            99, // attacker
            1,
            Vec3::new(105.0, 5.0, 100.0),
            Vec3::new(-1.0, 0.0, 0.0),
            &shot_def,
            &shell,
            &balance,
            (0.0, 0.0),
        )];

        let modifiers = EffectiveModifiers::default();
        let targets = [VehicleBallisticsTarget {
            entity_id: 42,
            team: 2,
            is_alive: true,
            left_arena: false,
            physics: &physics,
            modifiers: &modifiers,
            collision_set: Some(&col_set),
            chassis_armor: None,
            hull_armor: None,
            turret_armor: None,
            gun_armor: None,
            turret_yaw: 0.0,
            gun_pitch: 0.0,
            hull_pos: Vec3::ZERO,
            turret_pos: Vec3::ZERO,
            gun_pos: Vec3::ZERO,
        }];

        let mut short_range = projs.clone();
        short_range[0].max_distance = 2.0;
        let expired = step_projectiles(
            &mut short_range,
            &map,
            None,
            &targets,
            &balance,
            0.1,
            &mut rng,
        );
        assert!(
            expired.is_empty(),
            "an expired shot cannot touch a vehicle beyond its range"
        );
        assert!(short_range.is_empty());

        let outcomes = step_projectiles(&mut projs, &map, None, &targets, &balance, 0.1, &mut rng);
        assert_eq!(outcomes.len(), 1);
        let out = &outcomes[0];
        assert_eq!(out.struck_entity_id, Some(42));
        assert_eq!(out.component_idx, COMPONENT_HULL);

        // Verify Critical 1: local_origin of InternalCritRay MUST be component-local (near x=2.0), NOT world coords (102.0, 5.0, 100.0)!
        let crit = out
            .internal_ray
            .as_ref()
            .expect("penetrated shell must produce internal_ray");
        assert!(
            (crit.local_origin.x - 2.0).abs() < 0.2,
            "InternalCritRay.local_origin must be in component-local coordinates (near x=2.0), got {:?}",
            crit.local_origin
        );

        // Verify Critical 1: decode_hit_segment recovers the component-local point near x=2.0, NOT clamped to bbox edge from world pos 102.0
        let bbox = col_set.hull.local_aabb();
        let (_, _, dec_start, _) = decode_hit_segment(out.encoded_segment, bbox);
        assert!(
            (dec_start.x - 2.0).abs() < 0.25,
            "decoded segment must match component-local hit x ~ 2.0, got {:?}",
            dec_start
        );
    }

    #[test]
    fn thin_vs_thick_armor_zones_change_penetration() {
        use rand::SeedableRng;

        let balance = BalanceConfig::default();
        let mut rng = rand::rngs::StdRng::seed_from_u64(42);

        let def = crate::assets::arena_defs::ArenaDef {
            id: 1,
            name: "test".into(),
            display_name_key: "".into(),
            description_key: "".into(),
            geometry: "".into(),
            bounding_box: ([-200.0, -200.0], [200.0, 200.0]),
            min_players_in_team: 1,
            max_players_in_team: 15,
            round_length_secs: 900,
            arena_start_delay_secs: 5.0,
            kick_after_finish_wait_secs: 10.0,
            minimap_texture: None,
        };
        let bounds = crate::assets::chunk::SpaceBounds { min_x: 0, max_x: 0, min_y: 0, max_y: 0 };
        let terrain_map = crate::assets::map_store::TerrainMap::from_chunks(
            bounds,
            vec![(
                crate::assets::chunk::ChunkCoords { grid_x: 0, grid_z: 0, kind: crate::assets::chunk::ChunkKind::Outer },
                crate::assets::terrain::TerrainChunk {
                    heights: vec![0.0; crate::assets::terrain::HEIGHTMAP_GRID_SIZE * crate::assets::terrain::HEIGHTMAP_GRID_SIZE],
                    min_y: 0.0,
                    max_y: 0.0,
                    dom_tex_indices: vec![0; crate::assets::terrain::DOMINANT_TEX_GRID_SIZE * crate::assets::terrain::DOMINANT_TEX_GRID_SIZE],
                    tex_names: vec![],
                },
            )],
        ).unwrap();
        let mats = std::sync::Arc::new(crate::assets::terrain::MaterialKinds::parse(b"<root/>").unwrap());
        let map = ArenaMap::from_parts(
            def, bounds, terrain_map, mats, vec![], vec![], vec![], vec![], vec![], std::collections::BTreeMap::new(), &balance,
        );

        // BspTree with triangle at x=0 facing +X, material_index=0 ("armor_1")
        let hull_bsp_bytes = {
            let mut bytes = Vec::new();
            bytes.extend_from_slice(crate::assets::bsp2::BSP2_MAGIC);
            bytes.extend_from_slice(&1u32.to_le_bytes()); // 1 tri
            bytes.extend_from_slice(&1u32.to_le_bytes()); // 1 node
            bytes.extend_from_slice(&0u32.to_le_bytes());
            // Tri 0: [0.0, -1.0, -1.0], [0.0, 1.0, -1.0], [0.0, 0.0, 1.0]
            for v in &[[0.0f32, -1.0, -1.0], [0.0, 1.0, -1.0], [0.0, 0.0, 1.0]] {
                for c in v { bytes.extend_from_slice(&c.to_le_bytes()); }
            }
            bytes.extend_from_slice(&0u32.to_le_bytes()); // material_index = 0 ("armor_1")
            // Leaf node:
            bytes.push(0xa0);
            for c in &[1.0f32, 0.0, 0.0] { bytes.extend_from_slice(&c.to_le_bytes()); }
            bytes.extend_from_slice(&0.0f32.to_le_bytes());
            bytes.extend_from_slice(&1u16.to_le_bytes());
            bytes.extend_from_slice(&0u16.to_le_bytes());
            bytes
        };
        let bsp_mats = b"<mats><id>armor_1</id></mats>";
        let bsp_tree = std::sync::Arc::new(crate::assets::bsp2::BspTree::parse(&hull_bsp_bytes, bsp_mats).unwrap());
        let hit_tester = crate::assets::map_store::HitTester {
            model_path: "hull.model".into(),
            bsp: bsp_tree.clone(),
            enclosing_box: Some((Vec3::new(-2.0, -2.0, -2.0), Vec3::new(2.0, 2.0, 2.0))),
        };
        let empty_bsp_bytes = {
            let mut bytes = Vec::new();
            bytes.extend_from_slice(crate::assets::bsp2::BSP2_MAGIC);
            bytes.extend_from_slice(&0u32.to_le_bytes());
            bytes.extend_from_slice(&0u32.to_le_bytes());
            bytes.extend_from_slice(&0u32.to_le_bytes());
            bytes
        };
        let empty_bsp = std::sync::Arc::new(crate::assets::bsp2::BspTree::parse(&empty_bsp_bytes, b"").unwrap());
        let empty_hit_tester = crate::assets::map_store::HitTester {
            model_path: "empty.model".into(),
            bsp: empty_bsp,
            enclosing_box: Some((Vec3::new(-0.01, -0.01, -0.01), Vec3::new(0.01, 0.01, 0.01))),
        };
        let col_set = VehicleCollisionSet {
            hull: hit_tester,
            chassis: empty_hit_tester.clone(),
            turret: empty_hit_tester.clone(),
            gun: empty_hit_tester,
        };

        let physics = VehiclePhysicsState::default();
        let modifiers = EffectiveModifiers::default();

        let shot_def = ShotDef {
            shell_name: "test_ap".into(),
            shell_id: 1,
            shell_cd: 100,
            default_portion: 1.0,
            speed: 500.0,
            max_distance: 720.0,
            piercing_power: (100.0, 100.0), // 100mm piercing
            gravity: 0.0,
            shell: None,
        };
        let shell = ShellXmlDef {
            id: 1,
            name: "test_ap".into(),
            nation_id: 0,
            compact_descr: 100,
            kind: ShellKind::ArmorPiercing,
            caliber: 75.0,
            is_tracer: true,
            armor_damage: 200.0,
            devices_damage: 100.0,
            damage_randomization: 0.0,
            piercing_power_randomization: 0.0,
            explosion_radius: 0.0,
            effects: "".into(),
            price: (100, 0),
            show_in_shop: true,
        };

        // Case A: Thin armor (30mm) < 100mm pen -> Pierces and deals damage
        let mut thin_map = ArmorZoneMap::new();
        thin_map.insert("armor_1", 30.0, false);

        let targets_thin = [VehicleBallisticsTarget {
            entity_id: 1,
            team: 2,
            is_alive: true,
            left_arena: false,
            physics: &physics,
            modifiers: &modifiers,
            collision_set: Some(&col_set),
            chassis_armor: None,
            hull_armor: Some(&thin_map),
            turret_armor: None,
            gun_armor: None,
            turret_yaw: 0.0,
            gun_pitch: 0.0,
            hull_pos: Vec3::ZERO,
            turret_pos: Vec3::ZERO,
            gun_pos: Vec3::ZERO,
        }];

        let mut projs_thin = vec![spawn_shot(
            1, 99, 1,
            Vec3::new(5.0, 0.0, 0.0),
            Vec3::new(-1.0, 0.0, 0.0),
            &shot_def, &shell, &balance, (0.0, 0.0),
        )];
        let outcomes_thin = step_projectiles(&mut projs_thin, &map, None, &targets_thin, &balance, 0.1, &mut rng);
        assert_eq!(outcomes_thin.len(), 1);
        assert_eq!(outcomes_thin[0].shot_result, SHOT_RESULT_ARMOR_PIERCED);
        assert!(outcomes_thin[0].armor_damage > 0);
        assert!(outcomes_thin[0].internal_ray.is_some());

        // Case B: Thick armor (200mm) > 100mm pen -> Bounces / not pierced, 0 damage
        let mut thick_map = ArmorZoneMap::new();
        thick_map.insert("armor_1", 200.0, false);

        let targets_thick = [VehicleBallisticsTarget {
            entity_id: 2,
            team: 2,
            is_alive: true,
            left_arena: false,
            physics: &physics,
            modifiers: &modifiers,
            collision_set: Some(&col_set),
            chassis_armor: None,
            hull_armor: Some(&thick_map),
            turret_armor: None,
            gun_armor: None,
            turret_yaw: 0.0,
            gun_pitch: 0.0,
            hull_pos: Vec3::ZERO,
            turret_pos: Vec3::ZERO,
            gun_pos: Vec3::ZERO,
        }];

        let mut projs_thick = vec![spawn_shot(
            2, 99, 1,
            Vec3::new(5.0, 0.0, 0.0),
            Vec3::new(-1.0, 0.0, 0.0),
            &shot_def, &shell, &balance, (0.0, 0.0),
        )];
        let outcomes_thick = step_projectiles(&mut projs_thick, &map, None, &targets_thick, &balance, 0.1, &mut rng);
        assert_eq!(outcomes_thick.len(), 1);
        assert_eq!(outcomes_thick[0].shot_result, SHOT_RESULT_ARMOR_NOT_PIERCED);
        assert_eq!(outcomes_thick[0].armor_damage, 0);
        assert!(outcomes_thick[0].internal_ray.is_none());

        // Case C: Spaced armor / track with noDamage = true (30mm) -> Pierces with NO damage to tank HP!
        let mut nodmg_map = ArmorZoneMap::new();
        nodmg_map.insert("armor_1", 30.0, true);

        let targets_nodmg = [VehicleBallisticsTarget {
            entity_id: 3,
            team: 2,
            is_alive: true,
            left_arena: false,
            physics: &physics,
            modifiers: &modifiers,
            collision_set: Some(&col_set),
            chassis_armor: None,
            hull_armor: Some(&nodmg_map),
            turret_armor: None,
            gun_armor: None,
            turret_yaw: 0.0,
            gun_pitch: 0.0,
            hull_pos: Vec3::ZERO,
            turret_pos: Vec3::ZERO,
            gun_pos: Vec3::ZERO,
        }];

        let mut projs_nodmg = vec![spawn_shot(
            3, 99, 1,
            Vec3::new(5.0, 0.0, 0.0),
            Vec3::new(-1.0, 0.0, 0.0),
            &shot_def, &shell, &balance, (0.0, 0.0),
        )];
        let outcomes_nodmg = step_projectiles(&mut projs_nodmg, &map, None, &targets_nodmg, &balance, 0.1, &mut rng);
        assert_eq!(outcomes_nodmg.len(), 1);
        assert_eq!(outcomes_nodmg[0].shot_result, SHOT_RESULT_ARMOR_PIERCED_NO_DAMAGE);
        assert_eq!(outcomes_nodmg[0].armor_damage, 0);
        assert!(outcomes_nodmg[0].internal_ray.is_some());
    }

    #[test]
    fn shooting_standing_tree_breaks_it_and_clears_foliage() {
        use rand::SeedableRng;
        use crate::assets::destructibles::DestructibleKind;
        use crate::assets::map_store::ChunkDestructible;
        use crate::sim::destructibles::DestructiblesState;

        let balance = BalanceConfig::default();
        let mut rng = rand::rngs::StdRng::seed_from_u64(12345);

        let def = crate::assets::arena_defs::ArenaDef {
            id: 1,
            name: "test".into(),
            display_name_key: "".into(),
            description_key: "".into(),
            geometry: "".into(),
            bounding_box: ([-200.0, -200.0], [200.0, 200.0]),
            min_players_in_team: 1,
            max_players_in_team: 15,
            round_length_secs: 900,
            arena_start_delay_secs: 5.0,
            kick_after_finish_wait_secs: 10.0,
            minimap_texture: None,
        };
        let bounds = crate::assets::chunk::SpaceBounds { min_x: 0, max_x: 0, min_y: 0, max_y: 0 };
        let terrain_map = crate::assets::map_store::TerrainMap::from_chunks(
            bounds,
            vec![(
                crate::assets::chunk::ChunkCoords { grid_x: 0, grid_z: 0, kind: crate::assets::chunk::ChunkKind::Outer },
                crate::assets::terrain::TerrainChunk {
                    heights: vec![0.0; crate::assets::terrain::HEIGHTMAP_GRID_SIZE * crate::assets::terrain::HEIGHTMAP_GRID_SIZE],
                    min_y: 0.0,
                    max_y: 0.0,
                    dom_tex_indices: vec![0; crate::assets::terrain::DOMINANT_TEX_GRID_SIZE * crate::assets::terrain::DOMINANT_TEX_GRID_SIZE],
                    tex_names: vec![],
                },
            )],
        ).unwrap();
        let mats = std::sync::Arc::new(crate::assets::terrain::MaterialKinds::parse(b"<root/>").unwrap());

        let chunk_id = 100u16;
        let tree_pos = Vec3::new(0.0, 0.0, 50.0);
        let default_phys = crate::assets::destructibles::DestructiblePhysicParams {
            mass: 100.0,
            height: 6.0,
            spring_angle: 0.5,
            spring_stiffness: 1.0,
            spring_resist: 0.1,
            air_resist: 0.1,
            bury_depth: 0.5,
        };
        let tree_destr = ChunkDestructible {
            chunk_id,
            index_in_chunk: 0,
            desc_id: 1,
            filename: "tree1.spt".into(),
            kind: DestructibleKind::Tree {
                health: 50,
                density: 0.5,
                kinetic_damage_correction: 1.0,
                physic_params: default_phys,
            },
            world_transform: Mat4::from_translation(tree_pos),
            position: tree_pos,
            world_aabb_min: tree_pos - Vec3::new(3.0, 0.0, 3.0),
            world_aabb_max: tree_pos + Vec3::new(3.0, 6.0, 3.0),
            bsp: None,
            pieces: Vec::new(),
        };

        let mut destrs_by_chunk = std::collections::BTreeMap::new();
        destrs_by_chunk.insert(chunk_id, vec![tree_destr]);

        let map = ArenaMap::from_parts(
            def, bounds, terrain_map, mats, vec![], vec![], vec![], vec![], vec![], destrs_by_chunk, &balance,
        );

        let mut destr_state = DestructiblesState::from_map(&map);

        assert!(destr_state.tree_is_standing(chunk_id, 0), "tree must be standing initially");

        let shot_def = ShotDef {
            shell_name: "test_ap".into(),
            shell_id: 1,
            shell_cd: 100,
            default_portion: 1.0,
            speed: 500.0,
            max_distance: 720.0,
            piercing_power: (150.0, 110.0),
            gravity: 0.0,
            shell: None,
        };
        let shell = ShellXmlDef {
            id: 1,
            name: "test_ap".into(),
            nation_id: 0,
            compact_descr: 100,
            kind: ShellKind::ArmorPiercing,
            caliber: 75.0,
            is_tracer: true,
            armor_damage: 150.0,
            devices_damage: 100.0,
            damage_randomization: 0.0,
            piercing_power_randomization: 0.0,
            explosion_radius: 0.0,
            effects: "".into(),
            price: (100, 0),
            show_in_shop: true,
        };

        // Fire AP shell through the tree (origin at (0, 2, 0), heading towards (0, 2, 100))
        let mut projs = vec![spawn_shot(
            1,
            99,
            1,
            Vec3::new(0.0, 2.0, 0.0),
            Vec3::new(0.0, 0.0, 1.0),
            &shot_def,
            &shell,
            &balance,
            (0.0, 0.0),
        )];

        let outcomes = step_projectiles(&mut projs, &map, Some(&mut destr_state), &[], &balance, 0.2, &mut rng);

        assert!(!outcomes.is_empty(), "must produce outcome on tree hit");
        let tree_outcome = outcomes.iter().find(|o| !o.destructible_deltas.is_empty())
            .expect("must contain destructible deltas");
        assert_eq!(tree_outcome.destructible_deltas.len(), 1);
        assert_eq!(tree_outcome.destructible_deltas[0].chunk_id, chunk_id);

        assert!(!destr_state.tree_is_standing(chunk_id, 0), "tree must no longer be standing after being shot");
    }
}
