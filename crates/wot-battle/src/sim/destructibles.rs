//! BigWorld `AreaDestructibles` sync, chunk index, bit-packing, and destructible damage states.

use std::collections::{HashMap, HashSet};
use crate::assets::destructibles::DestructibleKind;
use crate::assets::map_store::{ArenaMap, ChunkDestructible};

/// Base offset for `AreaDestructibles` entity IDs so 16-bit `chunk_id`s never collide with
/// Account, Avatar, or Vehicle entity IDs allocated from `1..`.
pub const DESTRUCTIBLE_ENTITY_ID_BASE: i32 = 30_000;

/// Map a 16-bit `chunk_id` to its BigWorld `AreaDestructibles` entity ID.
pub fn chunk_entity_id(chunk_id: u16) -> i32 {
    DESTRUCTIBLE_ENTITY_ID_BASE + chunk_id as i32
}

/// Decode a BigWorld `AreaDestructibles` entity ID back into a 16-bit `chunk_id`.
pub fn entity_id_to_chunk_id(entity_id: i32) -> Option<u16> {
    let offset = entity_id - DESTRUCTIBLE_ENTITY_ID_BASE;
    if (0..=u16::MAX as i32).contains(&offset) {
        Some(offset as u16)
    } else {
        None
    }
}

/// Encode `(grid_x, grid_z)` into BigWorld 16-bit chunk ID: `((gx + 127) << 8) | (gz + 127)`.
pub fn encode_chunk_id(grid_x: i32, grid_z: i32) -> u16 {
    (((grid_x + 127) as u16) << 8) | ((grid_z + 127) as u16 & 0xff)
}

/// Decode a 16-bit chunk ID into `(grid_x, grid_z)`.
pub fn decode_chunk_id(chunk_id: u16) -> (i32, i32) {
    ((chunk_id >> 8) as i32 - 127, (chunk_id & 0xff) as i32 - 127)
}

/// Discretize yaw angle into 64 discrete bins: `int(64 * (yaw + PI) / 2PI) % 64`.
pub fn discretize_yaw(yaw: f32) -> u8 {
    let pi = std::f32::consts::PI;
    let pi2 = 2.0 * pi;
    (((64.0 * (yaw + pi) / pi2) as i32).rem_euclid(64)) as u8
}

/// Restore continuous yaw angle from 64 discrete bins.
pub fn restore_discrete_yaw(discrete: u8) -> f32 {
    let pi = std::f32::consts::PI;
    let pi2 = 2.0 * pi;
    ((discrete & 63) as f32 / 64.0) * pi2 - pi
}

/// Encode `fallenParams`: `(discreteYaw << 2) | (fallSpeed & 3)`.
pub fn encode_fallen_params(fall_dir_yaw: f32, fall_speed: u8) -> u8 {
    let discrete_yaw = discretize_yaw(fall_dir_yaw);
    (discrete_yaw << 2) | (fall_speed.min(3) & 3)
}

/// Decode `fallenParams`: returns `(fall_dir_yaw, fall_speed)`.
pub fn decode_fallen_params(params: u8) -> (f32, u8) {
    let yaw = restore_discrete_yaw(params >> 2);
    let speed = params & 3;
    (yaw, speed)
}

/// Encode fallen destructible: `(destr_id << 8) | params`.
pub fn encode_fallen_destructible(destr_id: u8, fall_dir_yaw: f32, fall_speed: u8) -> u16 {
    let params = encode_fallen_params(fall_dir_yaw, fall_speed);
    ((destr_id as u16) << 8) | (params as u16)
}

/// Decode fallen destructible: returns `(destr_id, fall_dir_yaw, fall_speed)`.
pub fn decode_fallen_destructible(data: u16) -> (u8, f32, u8) {
    let destr_id = (data >> 8) as u8;
    let (yaw, speed) = decode_fallen_params((data & 0xff) as u8);
    (destr_id, yaw, speed)
}

/// Encode destructible module: `(destr_id << 8) | (mat_kind << 1) | is_shot_damage`.
pub fn encode_destructible_module(destr_id: u8, mat_kind: u8, is_shot_damage: bool) -> u16 {
    ((destr_id as u16) << 8) | (((mat_kind as u16) & 127) << 1) | (is_shot_damage as u16 & 1)
}

/// Decode destructible module: returns `(destr_id, mat_kind, is_shot_damage)`.
pub fn decode_destructible_module(data: u16) -> (u8, u8, bool) {
    let destr_id = (data >> 8) as u8;
    let mat_kind = ((data >> 1) & 127) as u8;
    let is_shot = (data & 1) != 0;
    (destr_id, mat_kind, is_shot)
}

/// Scaled destructible health: `ceil(scale * scale * ref_health)`.
pub fn scaled_destructible_health(scale: f32, ref_health: i32) -> i32 {
    (scale * scale * ref_health as f32).ceil() as i32
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DestructibleDeltaKind {
    Module(u16),
    Fragile(u8),
    Fallen(u16),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DestructibleDelta {
    pub chunk_id: u16,
    pub kind: DestructibleDeltaKind,
}

#[derive(Debug, Clone, Default)]
pub struct ChunkDestructiblesState {
    pub destroyed_modules: Vec<u16>,
    pub destroyed_fragiles: Vec<u8>,
    pub fallen_destructibles: Vec<u16>,
    pub module_health: HashMap<(u8, u8), i32>,
    pub fragile_health: HashMap<u8, i32>,
    pub fallen_set: HashSet<u8>,
    pub fragile_set: HashSet<u8>,
    pub module_set: HashSet<(u8, u8)>,
}

/// Retail `destructibles.xml` `unitVehicleMass`.
pub const DEFAULT_UNIT_VEHICLE_MASS: f32 = 28_000.0;

/// `Vehicle._isDestructibleBroken`: kinetic damage a hull of `mass_kg` deals by ramming at
/// `speed`. Trees, falling atoms and fragiles scale it by
/// `(mass / unitVehicleMass) ^ kineticDamageCorrection`; structure modules (`None`) do not.
pub fn ramming_instant_damage(
    mass_kg: f32,
    speed: f32,
    unit_vehicle_mass: f32,
    kinetic_damage_correction: Option<f32>,
) -> f32 {
    let damage = 0.5 * mass_kg * speed * speed * 0.000_15;
    match kinetic_damage_correction {
        Some(correction) if correction != 0.0 => {
            damage * (mass_kg / unit_vehicle_mass).powf(correction)
        }
        _ => damage,
    }
}

/// `kineticDamageCorrection` of the item, `None` for a structure, whose modules take none.
fn ramming_correction(kind: &DestructibleKind) -> Option<f32> {
    match kind {
        DestructibleKind::Tree { kinetic_damage_correction, .. }
        | DestructibleKind::FallingAtom { kinetic_damage_correction, .. }
        | DestructibleKind::Fragile { kinetic_damage_correction, .. } => Some(*kinetic_damage_correction),
        DestructibleKind::Structure { .. } => None,
    }
}

#[derive(Debug, Clone)]
pub struct DestructiblesState {
    pub chunks: HashMap<u16, ChunkDestructiblesState>,
    pub desc_by_chunk_and_id: HashMap<(u16, u8), (DestructibleKind, f32)>, // kind, scale
    pub unit_vehicle_mass: f32,
}

impl Default for DestructiblesState {
    fn default() -> Self {
        Self {
            chunks: HashMap::new(),
            desc_by_chunk_and_id: HashMap::new(),
            unit_vehicle_mass: DEFAULT_UNIT_VEHICLE_MASS,
        }
    }
}

impl DestructiblesState {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn from_map(map: &ArenaMap) -> Self {
        Self::from_destructibles(map.unit_vehicle_mass, map.destructibles_by_chunk.values().flatten())
    }

    /// Intact state of `destructibles`. An item scales by its matrix y axis, the tree scale
    /// the client's `AreaDestructibles` derives from `wg_getDestructibleMatrix`.
    pub fn from_destructibles<'a>(
        unit_vehicle_mass: f32,
        destructibles: impl IntoIterator<Item = &'a ChunkDestructible>,
    ) -> Self {
        let mut state = Self {
            unit_vehicle_mass,
            ..Self::new()
        };
        for d in destructibles {
            let Ok(destr_id) = u8::try_from(d.index_in_chunk) else {
                continue;
            };
            let chunk_id = d.chunk_id;
            let scale = d.world_transform.y_axis.truncate().length();
            let scale = if scale.is_finite() && scale > 1e-4 { scale } else { 1.0 };
            state
                .desc_by_chunk_and_id
                .insert((chunk_id, destr_id), (d.kind.clone(), scale));

            let chunk_state = state.chunks.entry(chunk_id).or_default();
            match &d.kind {
                DestructibleKind::Structure { modules, .. } => {
                    for (&mat_kind, module) in modules {
                        let hp = scaled_destructible_health(scale, module.health);
                        chunk_state.module_health.insert((destr_id, mat_kind), hp);
                    }
                }
                DestructibleKind::Fragile { health, .. } => {
                    let hp = scaled_destructible_health(scale, *health);
                    chunk_state.fragile_health.insert(destr_id, hp);
                }
                _ => {}
            }
        }
        state
    }

    /// Whether a hull of `mass_kg` ramming the item at `speed` breaks it, as the client's
    /// `Vehicle._isDestructibleBroken` decides. Unknown and already broken items do not.
    pub fn ramming_breaks(&self, chunk_id: u16, destr_id: u8, mass_kg: f32, speed: f32) -> bool {
        let Some((kind, scale)) = self.desc_by_chunk_and_id.get(&(chunk_id, destr_id)) else {
            return false;
        };
        let damage = ramming_instant_damage(mass_kg, speed, self.unit_vehicle_mass, ramming_correction(kind));
        let breaks = |health: i32| (scaled_destructible_health(*scale, health) as f32) < damage;
        let chunk = self.chunks.get(&chunk_id);
        match kind {
            DestructibleKind::Tree { health, .. } | DestructibleKind::FallingAtom { health, .. } => {
                !chunk.is_some_and(|c| c.fallen_set.contains(&destr_id)) && breaks(*health)
            }
            DestructibleKind::Fragile { health, .. } => {
                !chunk.is_some_and(|c| c.fragile_set.contains(&destr_id)) && breaks(*health)
            }
            DestructibleKind::Structure { modules, .. } => {
                modules.iter().any(|(&mat_kind, module)| {
                    !chunk.is_some_and(|c| c.module_set.contains(&(destr_id, mat_kind))) && breaks(module.health)
                })
            }
        }
    }

    /// A structure stands while any of its modules is intact.
    pub fn structure_is_standing(&self, chunk_id: u16, destr_id: u8) -> bool {
        let Some((DestructibleKind::Structure { modules, .. }, _)) = self.desc_by_chunk_and_id.get(&(chunk_id, destr_id))
        else {
            return false;
        };
        let chunk = self.chunks.get(&chunk_id);
        modules
            .keys()
            .any(|&mat_kind| !chunk.is_some_and(|c| c.module_set.contains(&(destr_id, mat_kind))))
    }

    pub fn tree_is_standing(&self, chunk_id: u16, destr_id: usize) -> bool {
        let d_id = (destr_id & 0xff) as u8;
        if let Some(chunk) = self.chunks.get(&chunk_id) {
            !chunk.fallen_set.contains(&d_id) && !chunk.fragile_set.contains(&d_id)
        } else {
            true
        }
    }

    pub fn is_destructible_broken(&self, chunk_id: u16, destr_id: u8) -> bool {
        if let Some(chunk) = self.chunks.get(&chunk_id) {
            chunk.fallen_set.contains(&destr_id) || chunk.fragile_set.contains(&destr_id)
        } else {
            false
        }
    }

    pub fn break_by_ramming(
        &mut self,
        chunk_id: u16,
        destr_id: u8,
        mass_kg: f32,
        speed: f32,
        fall_yaw: f32,
        fall_speed: u8,
    ) -> Vec<DestructibleDelta> {
        let Some((kind, scale)) = self.desc_by_chunk_and_id.get(&(chunk_id, destr_id)).cloned() else {
            return Vec::new();
        };
        let instant_damage =
            ramming_instant_damage(mass_kg, speed, self.unit_vehicle_mass, ramming_correction(&kind));
        // The client breaks an item only when its scaled health is strictly below the damage.
        let breaks = |health: i32| (scaled_destructible_health(scale, health) as f32) < instant_damage;

        let chunk = self.chunks.entry(chunk_id).or_default();
        match kind {
            DestructibleKind::Tree { health, .. } | DestructibleKind::FallingAtom { health, .. } => {
                if chunk.fallen_set.contains(&destr_id) {
                    return Vec::new();
                }
                if breaks(health) {
                    chunk.fallen_set.insert(destr_id);
                    let encoded = encode_fallen_destructible(destr_id, fall_yaw, fall_speed);
                    chunk.fallen_destructibles.push(encoded);
                    vec![DestructibleDelta {
                        chunk_id,
                        kind: DestructibleDeltaKind::Fallen(encoded),
                    }]
                } else {
                    Vec::new()
                }
            }
            DestructibleKind::Fragile { health, .. } => {
                if chunk.fragile_set.contains(&destr_id) {
                    return Vec::new();
                }
                if breaks(health) {
                    chunk.fragile_set.insert(destr_id);
                    chunk.destroyed_fragiles.push(destr_id);
                    vec![DestructibleDelta {
                        chunk_id,
                        kind: DestructibleDeltaKind::Fragile(destr_id),
                    }]
                } else {
                    Vec::new()
                }
            }
            DestructibleKind::Structure {
                modules,
                destroy_depends,
                ..
            } => {
                let mut deltas = Vec::new();
                for (&mat_kind, module) in &modules {
                    if chunk.module_set.contains(&(destr_id, mat_kind)) {
                        continue;
                    }
                    if breaks(module.health) {
                        chunk.module_set.insert((destr_id, mat_kind));
                        let encoded = encode_destructible_module(destr_id, mat_kind, false);
                        chunk.destroyed_modules.push(encoded);
                        deltas.push(DestructibleDelta {
                            chunk_id,
                            kind: DestructibleDeltaKind::Module(encoded),
                        });
                        // Cascade dependencies
                        if let Some(deps) = destroy_depends.get(&mat_kind) {
                            for &dep_mat in deps {
                                if chunk.module_set.insert((destr_id, dep_mat)) {
                                    let dep_enc = encode_destructible_module(destr_id, dep_mat, false);
                                    chunk.destroyed_modules.push(dep_enc);
                                    deltas.push(DestructibleDelta {
                                        chunk_id,
                                        kind: DestructibleDeltaKind::Module(dep_enc),
                                    });
                                }
                            }
                        }
                    }
                }
                deltas
            }
        }
    }

    pub fn break_by_shot(
        &mut self,
        chunk_id: u16,
        destr_id: u8,
        mat_kind: u8,
        damage: f32,
        shot_dir_yaw: f32,
    ) -> Vec<DestructibleDelta> {
        let Some((kind, scale)) = self.desc_by_chunk_and_id.get(&(chunk_id, destr_id)).cloned() else {
            return Vec::new();
        };

        let chunk = self.chunks.entry(chunk_id).or_default();
        match kind {
            DestructibleKind::Tree { health, .. } | DestructibleKind::FallingAtom { health, .. } => {
                if chunk.fallen_set.contains(&destr_id) {
                    return Vec::new();
                }
                let req_health = scaled_destructible_health(scale, health);
                if damage >= req_health as f32 {
                    chunk.fallen_set.insert(destr_id);
                    let encoded = encode_fallen_destructible(destr_id, shot_dir_yaw, 2);
                    chunk.fallen_destructibles.push(encoded);
                    vec![DestructibleDelta {
                        chunk_id,
                        kind: DestructibleDeltaKind::Fallen(encoded),
                    }]
                } else {
                    Vec::new()
                }
            }
            DestructibleKind::Fragile { health, .. } => {
                if chunk.fragile_set.contains(&destr_id) {
                    return Vec::new();
                }
                let req_hp = scaled_destructible_health(scale, health);
                let curr_hp = chunk.fragile_health.entry(destr_id).or_insert(req_hp);
                *curr_hp -= damage as i32;
                if *curr_hp <= 0 {
                    chunk.fragile_set.insert(destr_id);
                    chunk.destroyed_fragiles.push(destr_id);
                    vec![DestructibleDelta {
                        chunk_id,
                        kind: DestructibleDeltaKind::Fragile(destr_id),
                    }]
                } else {
                    Vec::new()
                }
            }
            DestructibleKind::Structure {
                modules,
                destroy_depends,
                ..
            } => {
                let actual_mat = if mat_kind == 0 {
                    modules.keys().next().copied().unwrap_or(73)
                } else {
                    mat_kind
                };
                if chunk.module_set.contains(&(destr_id, actual_mat)) {
                    return Vec::new();
                }
                let req_hp = modules
                    .get(&actual_mat)
                    .map(|m| scaled_destructible_health(scale, m.health))
                    .unwrap_or(50);
                let curr_hp = chunk.module_health.entry((destr_id, actual_mat)).or_insert(req_hp);
                *curr_hp -= damage as i32;
                if *curr_hp <= 0 {
                    let mut deltas = Vec::new();
                    chunk.module_set.insert((destr_id, actual_mat));
                    let enc = encode_destructible_module(destr_id, actual_mat, true);
                    chunk.destroyed_modules.push(enc);
                    deltas.push(DestructibleDelta {
                        chunk_id,
                        kind: DestructibleDeltaKind::Module(enc),
                    });
                    if let Some(deps) = destroy_depends.get(&actual_mat) {
                        for &dep_mat in deps {
                            if chunk.module_set.insert((destr_id, dep_mat)) {
                                let dep_enc = encode_destructible_module(destr_id, dep_mat, true);
                                chunk.destroyed_modules.push(dep_enc);
                                deltas.push(DestructibleDelta {
                                    chunk_id,
                                    kind: DestructibleDeltaKind::Module(dep_enc),
                                });
                            }
                        }
                    }
                    deltas
                } else {
                    Vec::new()
                }
            }
        }
    }

    pub fn destroyed_modules(&self, chunk_id: u16) -> &[u16] {
        self.chunks.get(&chunk_id).map(|c| c.destroyed_modules.as_slice()).unwrap_or(&[])
    }

    pub fn destroyed_fragiles(&self, chunk_id: u16) -> &[u8] {
        self.chunks.get(&chunk_id).map(|c| c.destroyed_fragiles.as_slice()).unwrap_or(&[])
    }

    pub fn fallen_destructibles(&self, chunk_id: u16) -> &[u16] {
        self.chunks.get(&chunk_id).map(|c| c.fallen_destructibles.as_slice()).unwrap_or(&[])
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::assets::destructibles::{DestructiblePhysicParams, StructureModule};
    use std::collections::BTreeMap;

    fn tree(health: i32) -> DestructibleKind {
        DestructibleKind::Tree {
            health,
            density: 0.5,
            kinetic_damage_correction: 0.0,
            physic_params: DestructiblePhysicParams {
                mass: 500.0,
                height: 8.0,
                spring_angle: 0.2,
                spring_stiffness: 1000.0,
                spring_resist: 200.0,
                air_resist: 10.0,
                bury_depth: 0.5,
            },
        }
    }

    #[test]
    fn ramming_instant_damage_follows_client_formula() {
        // Vehicle._isDestructibleBroken: 0.5 * mass * v^2 * 0.00015, times
        // (mass / unitVehicleMass) ^ kineticDamageCorrection unless it is a structure module.
        let cases: [(f32, f32, Option<f32>, f32); 4] = [
            (11_500.0, 7.0, Some(0.0), 42.2625),
            (11_500.0, 7.5, Some(0.0), 48.515_625),
            (56_000.0, 2.0, Some(0.5), 23.758_787),
            (56_000.0, 2.0, None, 16.8),
        ];
        for (mass, speed, correction, want) in cases {
            let got = ramming_instant_damage(mass, speed, 28_000.0, correction);
            assert!((got - want).abs() < 1e-3, "{mass} kg at {speed} m/s: {got}, want {want}");
        }
    }

    #[test]
    fn light_tank_breaks_spruce_only_above_client_break_speed() {
        // Karelia Spruce: health 46, scale 1; an 11.5 t hull needs more than 7.3 m/s.
        let mut state = DestructiblesState::new();
        let chunk_id = encode_chunk_id(-3, -3);
        state.desc_by_chunk_and_id.insert((chunk_id, 27), (tree(46), 1.0));

        assert!(state.break_by_ramming(chunk_id, 27, 11_500.0, 7.0, 0.0, 2).is_empty());
        assert!(state.tree_is_standing(chunk_id, 27));
        assert_eq!(state.break_by_ramming(chunk_id, 27, 11_500.0, 7.5, 0.0, 2).len(), 1);
        assert!(!state.tree_is_standing(chunk_id, 27));
    }

    #[test]
    fn flattened_bush_breaks_by_its_height_scale() {
        // AreaDestructibles scales a tree by its matrix y axis: a bush stretched 4 x 5 in the
        // ground plane but 1 m tall keeps health ceil(1 * 3) = 3, not ceil(3.33^2 * 3) = 34.
        use crate::assets::map_store::ChunkDestructible;
        use glam::{Mat4, Vec3};
        let transform = Mat4::from_scale(Vec3::new(4.0, 1.0, 5.0));
        let chunk_id = encode_chunk_id(0, 0);
        let bush = ChunkDestructible {
            chunk_id,
            index_in_chunk: 2,
            desc_id: 0,
            filename: "speedtree/01_Karelia/Bush2_1.spt".into(),
            kind: tree(3),
            world_transform: transform,
            position: Vec3::ZERO,
            world_aabb_min: Vec3::splat(-1.0),
            world_aabb_max: Vec3::splat(1.0),
            bsp: None,
            pieces: Vec::new(),
        };
        let state = DestructiblesState::from_destructibles(DEFAULT_UNIT_VEHICLE_MASS, [&bush]);
        // 4691 kg (MS-1) at 3 m/s deals 3.17.
        assert!(state.ramming_breaks(chunk_id, 2, 4_691.0, 3.0));
    }

    #[test]
    fn structure_module_takes_ramming_damage_without_mass_correction() {
        let mut state = DestructiblesState::new();
        let chunk_id = encode_chunk_id(1, 1);
        let module = StructureModule {
            mat_kind: 71,
            destroyed_mat_kind: 72,
            surface: String::new(),
            module_id: 0,
            health: 17,
        };
        let house = DestructibleKind::Structure {
            modules: BTreeMap::from([(71, module)]),
            destroy_depends: BTreeMap::new(),
            state_presets: BTreeMap::new(),
        };
        state.desc_by_chunk_and_id.insert((chunk_id, 4), (house, 1.0));

        // 56 t at 2.0 m/s deals 16.8 (23.8 with a heavy-hull correction): the module stands.
        assert!(state.break_by_ramming(chunk_id, 4, 56_000.0, 2.0, 0.0, 1).is_empty());
        // At 2.1 m/s it deals 18.52 and breaks.
        assert_eq!(state.break_by_ramming(chunk_id, 4, 56_000.0, 2.1, 0.0, 1).len(), 1);
    }

    #[test]
    fn destructible_bitpack_matches_client_decoders() {
        // 1. Chunk ID formula check
        let gx = -2;
        let gz = 5;
        let cid = encode_chunk_id(gx, gz);
        assert_eq!(decode_chunk_id(cid), (gx, gz));
        assert_eq!(cid, ((-2 + 127) << 8 | (5 + 127)) as u16);

        // 2. Fallen destructible bitpack check
        let yaw = 1.234f32;
        let speed = 2u8;
        let destr_id = 42u8;
        let encoded_fallen = encode_fallen_destructible(destr_id, yaw, speed);
        let (dec_id, dec_yaw, dec_speed) = decode_fallen_destructible(encoded_fallen);
        assert_eq!(dec_id, destr_id);
        assert_eq!(dec_speed, speed);
        assert!((dec_yaw - yaw).abs() < (2.0 * std::f32::consts::PI / 64.0) + 1e-4);

        // 3. Module bitpack check
        let mat_kind = 75u8;
        let is_shot = true;
        let encoded_mod = encode_destructible_module(destr_id, mat_kind, is_shot);
        let (dec_id, dec_mat, dec_shot) = decode_destructible_module(encoded_mod);
        assert_eq!(dec_id, destr_id);
        assert_eq!(dec_mat, mat_kind);
        assert_eq!(dec_shot, is_shot);

        // 4. Ramming breaks tree when instant_damage > ceil(scale^2 * ref_health)
        let mut state = DestructiblesState::new();
        let chunk_id = encode_chunk_id(0, 0);
        let tree_id = 1u8;
        let scale = 1.2f32;
        let ref_health = 100i32;
        let req_health = scaled_destructible_health(scale, ref_health);
        assert_eq!(req_health, (1.2 * 1.2 * 100.0f32).ceil() as i32); // 144

        state.desc_by_chunk_and_id.insert(
            (chunk_id, tree_id),
            (
                DestructibleKind::Tree {
                    health: ref_health,
                    density: 0.5,
                    kinetic_damage_correction: 0.0,
                    physic_params: crate::assets::destructibles::DestructiblePhysicParams {
                        mass: 500.0,
                        height: 8.0,
                        spring_angle: 0.2,
                        spring_stiffness: 1000.0,
                        spring_resist: 200.0,
                        air_resist: 10.0,
                        bury_depth: 0.5,
                    },
                },
                scale,
            ),
        );

        // 28 t at 8 m/s deals 134.4 < 144 -> tree still standing
        let deltas_low = state.break_by_ramming(chunk_id, tree_id, 28_000.0, 8.0, 0.5, 1);
        assert!(deltas_low.is_empty(), "damage 134.4 < 144 should not break tree");
        assert!(state.tree_is_standing(chunk_id, tree_id as usize));

        // 28 t at 8.5 m/s deals 151.7 -> tree breaks and falls
        let deltas = state.break_by_ramming(chunk_id, tree_id, 28_000.0, 8.5, 0.5, 2);
        assert_eq!(deltas.len(), 1);
        assert!(!state.tree_is_standing(chunk_id, tree_id as usize));
        assert_eq!(
            deltas[0],
            DestructibleDelta {
                chunk_id,
                kind: DestructibleDeltaKind::Fallen(encode_fallen_destructible(tree_id, 0.5, 2)),
            }
        );
        assert_eq!(state.fallen_destructibles(chunk_id).len(), 1);
    }
}
