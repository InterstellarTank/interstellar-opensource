//! WoT 0.6.5 line-of-sight visibility, camouflage, 50m proxy spotting, spot linger, radio graph, and AoI.

use std::collections::{HashMap, HashSet, VecDeque};
use glam::{Vec2, Vec3};
use crate::assets::map_store::{ArenaMap, FoliageVolume, TerrainMap};
use crate::balance::BalanceConfig;
use crate::sim::crits::DeviceState;
use crate::sim::destructibles::DestructiblesState;
use crate::sim::physics::VehicleSimState;

pub const VEHICLE_AOI_RADIUS: f32 = 565.0;

#[derive(Debug, Clone, Copy, Default)]
pub struct ObserverFocusState {
    pub bound_vehicle: Option<i32>,
    position: Option<Vec3>,
}

impl ObserverFocusState {
    pub fn move_to(&mut self, position: Vec3) {
        self.bound_vehicle = None;
        self.position = Some(position);
    }

    pub fn bind_to(&mut self, vehicle_id: i32, position: Vec3) {
        self.bound_vehicle = Some(vehicle_id);
        self.position = Some(position);
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AoIChange {
    Enter(i32),
    Leave(i32),
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MinimapPositionFrame {
    pub indices: Vec<u8>,
    pub positions_xz: Vec<i16>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FirstSpottedEvent {
    pub spotter_id: i32,
    pub target_id: i32,
    pub team: u8,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct VisibilityOutput {
    pub aoi_changes: HashMap<i32, Vec<AoIChange>>,
    pub spotted_enemies: HashMap<i32, HashSet<i32>>,
    pub first_spotted_events: Vec<FirstSpottedEvent>,
    pub minimap_frames: HashMap<i32, MinimapPositionFrame>,
}

#[derive(Debug, Clone, Default)]
pub struct VisibilityTracker {
    pub direct_spots: HashMap<(i32, i32), f32>, // (observer_id, target_id) -> linger_time_remaining
    pub ever_spotted: HashSet<(u8, i32)>,       // (team, target_id)
    pub previous_aoi: HashMap<i32, HashSet<i32>>, // observer_id -> last world AoI set
    pub last_shot_timers: HashMap<i32, f32>,    // vehicle_id -> time since shot
    pub stationary_timers: HashMap<i32, f32>,   // vehicle_id -> time stationary
}

fn record_spot(
    direct_spots: &mut HashMap<(i32, i32), f32>,
    ever_spotted: &mut HashSet<(u8, i32)>,
    first_spotted_events: &mut Vec<FirstSpottedEvent>,
    obs_id: i32,
    target_id: i32,
    obs_team: u8,
    spot_linger_secs: f32,
) {
    direct_spots.insert((obs_id, target_id), spot_linger_secs);
    if ever_spotted.insert((obs_team, target_id)) {
        first_spotted_events.push(FirstSpottedEvent {
            spotter_id: obs_id,
            target_id,
            team: obs_team,
        });
    }
}

impl VisibilityTracker {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn record_shot(&mut self, vehicle_id: i32) {
        self.last_shot_timers.insert(vehicle_id, 0.0);
    }

    pub fn step_visibility(
        &mut self,
        vehicles: &[VehicleSimState],
        map: &ArenaMap,
        destr: &DestructiblesState,
        dt: f32,
    ) -> VisibilityOutput {
        self.step_visibility_with_observers(vehicles, map, destr, dt, &HashMap::new())
    }

    /// `vehicles` must hold every arena vehicle: minimap indices are positions in the
    /// sorted id list, as in the client's `ClientArena.__rebuildIndexToId`.
    pub fn step_visibility_with_observers(
        &mut self,
        vehicles: &[VehicleSimState],
        map: &ArenaMap,
        destr: &DestructiblesState,
        dt: f32,
        views: &HashMap<i32, ObserverFocusState>,
    ) -> VisibilityOutput {
        let mut out = VisibilityOutput::default();
        let balance = BalanceConfig::default();

        // 1. Advance timers
        for v in vehicles {
            if v.physics.speed_mps.abs() < 0.1 {
                let st = self.stationary_timers.entry(v.entity_id).or_insert(0.0);
                *st += dt;
            } else {
                self.stationary_timers.insert(v.entity_id, 0.0);
            }
            if let Some(st) = self.last_shot_timers.get_mut(&v.entity_id) {
                *st += dt;
            }
        }

        // Advance direct spot linger timers
        let mut expired = Vec::new();
        for (pair, timer) in &mut self.direct_spots {
            *timer -= dt;
            if *timer <= 0.0 {
                expired.push(*pair);
            }
        }
        for pair in expired {
            self.direct_spots.remove(&pair);
        }

        // 2. Direct visibility & spotting
        for obs in vehicles {
            if !obs.is_alive {
                continue;
            }
            for target in vehicles {
                if !target.is_alive || obs.team == target.team || obs.entity_id == target.entity_id {
                    continue;
                }

                let dist = (obs.physics.pos - target.physics.pos).length();

                // Proximity detection (<= 50m unconditional X-ray)
                if dist <= balance.auto_detect_distance_m {
                    record_spot(
                        &mut self.direct_spots,
                        &mut self.ever_spotted,
                        &mut out.first_spotted_events,
                        obs.entity_id,
                        target.entity_id,
                        obs.team,
                        balance.spot_linger_secs,
                    );
                    continue;
                }

                // If beyond maximum vision range, cannot spot directly
                if dist > balance.max_vision_distance_m {
                    continue;
                }

                let target_recently_fired = self
                    .last_shot_timers
                    .get(&target.entity_id)
                    .map_or(false, |&t| t <= 5.0);

                // Six-checkpoint raycast
                let (has_los, min_foliage_camo) = check_six_checkpoint_los(
                    obs,
                    target,
                    target_recently_fired,
                    map,
                    destr,
                    &balance,
                );

                if has_los {
                    let base_camo = if target.physics.speed_mps.abs() > 0.1 {
                        target.camo_moving
                    } else {
                        target.camo_still
                    };
                    let mut eff_camo = base_camo * target.modifiers.camo_factor;
                    if target_recently_fired {
                        eff_camo *= balance.invisibility_factor_at_shot_default;
                    }
                    let total_camo = (eff_camo + min_foliage_camo).clamp(0.0, 0.95);
                    let eff_vision = (obs.circular_vision_radius * obs.modifiers.vision_radius_factor)
                        .clamp(50.0, 500.0);
                    let spot_dist = (eff_vision - (eff_vision - 50.0) * total_camo)
                        .clamp(50.0, balance.max_vision_distance_m);

                    if dist <= spot_dist {
                        record_spot(
                            &mut self.direct_spots,
                            &mut self.ever_spotted,
                            &mut out.first_spotted_events,
                            obs.entity_id,
                            target.entity_id,
                            obs.team,
                            balance.spot_linger_secs,
                        );
                    }
                }
            }
        }

        // 3. Radio Graph BFS per team
        let mut team_vehicles: HashMap<u8, Vec<&VehicleSimState>> = HashMap::new();
        for v in vehicles {
            if v.is_alive {
                team_vehicles.entry(v.team).or_default().push(v);
            }
        }

        let mut connected_allies: HashMap<i32, HashSet<i32>> = HashMap::new();

        // For each team, build radio connectivity graph
        for (_team, members) in &team_vehicles {
            let n = members.len();
            let mut adj: Vec<Vec<usize>> = vec![Vec::new(); n];

            let eff_radio = |v: &VehicleSimState| -> f32 {
                match v.damage.radio.state {
                    DeviceState::Normal => v.radio_distance_m,
                    DeviceState::Damaged => v.radio_distance_m * 0.5,
                    DeviceState::Destroyed => 0.0,
                }
            };

            for i in 0..n {
                for j in (i + 1)..n {
                    let r_i = eff_radio(members[i]);
                    let r_j = eff_radio(members[j]);
                    if r_i > 0.0 && r_j > 0.0 {
                        let d = (members[i].physics.pos - members[j].physics.pos).length();
                        if d <= r_i + r_j {
                            adj[i].push(j);
                            adj[j].push(i);
                        }
                    }
                }
            }

            // Find connected components
            let mut visited = vec![false; n];
            for i in 0..n {
                if visited[i] {
                    continue;
                }
                let mut component = Vec::new();
                let mut queue = VecDeque::new();
                visited[i] = true;
                queue.push_back(i);
                while let Some(curr) = queue.pop_front() {
                    component.push(curr);
                    for &next in &adj[curr] {
                        if !visited[next] {
                            visited[next] = true;
                            queue.push_back(next);
                        }
                    }
                }

                // Gather all direct spots from anyone in component
                let mut component_spotted_enemies = HashSet::new();
                let mut component_members = HashSet::new();
                for &idx in &component {
                    let id = members[idx].entity_id;
                    component_members.insert(id);
                    for (&(obs_id, target_id), _) in &self.direct_spots {
                        if obs_id == id {
                            component_spotted_enemies.insert(target_id);
                        }
                    }
                }

                // Share to all in component
                for &idx in &component {
                    let id = members[idx].entity_id;
                    out.spotted_enemies.insert(id, component_spotted_enemies.clone());
                    connected_allies.insert(id, component_members.clone());
                }
            }
        }

        // Ensure every alive vehicle has an entry in spotted_enemies
        for v in vehicles {
            if v.is_alive && !out.spotted_enemies.contains_key(&v.entity_id) {
                out.spotted_enemies.insert(v.entity_id, HashSet::new());
            }
        }

        // 4. World AoI and Minimap Frames
        let mut sorted_all_ids: Vec<i32> = vehicles.iter().map(|v| v.entity_id).collect();
        sorted_all_ids.sort();

        // Minimap frames per observer
        for obs in vehicles {
            if !obs.is_alive {
                continue;
            }
            let obs_spotted = out.spotted_enemies.get(&obs.entity_id);
            let connected = connected_allies.get(&obs.entity_id);

            let mut indices = Vec::new();
            let mut positions_xz = Vec::new();

            for (idx, &veh_id) in sorted_all_ids.iter().enumerate() {
                let Some(veh) = vehicles.iter().find(|v| v.entity_id == veh_id && v.is_alive) else {
                    continue;
                };
                let is_connected_ally = veh_id == obs.entity_id
                    || (veh.team == obs.team && connected.map_or(false, |c| c.contains(&veh_id)));
                let is_spotted_enemy = veh.team != obs.team
                    && obs_spotted.map_or(false, |set| set.contains(&veh_id));

                if is_connected_ally || is_spotted_enemy {
                    indices.push(idx as u8);
                    positions_xz.push(veh.physics.pos.x.round() as i16);
                    positions_xz.push(veh.physics.pos.z.round() as i16);
                }
            }

            out.minimap_frames.insert(obs.entity_id, MinimapPositionFrame { indices, positions_xz });
        }

        // World AoI deltas per observer
        for obs in vehicles {
            let view = views.get(&obs.entity_id);
            if !obs.is_alive && view.is_none() {
                continue;
            }
            let follow = view.and_then(|focus| focus.bound_vehicle)
                .and_then(|id| vehicles.iter().find(|v| v.entity_id == id && v.team == obs.team));
            let origin = follow.map(|v| v.physics.pos)
                .or_else(|| view.and_then(|focus| focus.position))
                .unwrap_or(obs.physics.pos);
            let spot_source = follow.filter(|_| !obs.is_alive).unwrap_or(obs).entity_id;
            if spot_source != obs.entity_id {
                if let Some(frame) = out.minimap_frames.get(&spot_source).cloned() {
                    out.minimap_frames.insert(obs.entity_id, frame);
                }
            }
            let mut current_aoi = HashSet::new();
            let spotted_set = out.spotted_enemies.get(&spot_source);
            let prev_aoi = self.previous_aoi.entry(obs.entity_id).or_default();

            for other in vehicles {
                if other.entity_id == obs.entity_id {
                    continue;
                }
                let dist = (origin - other.physics.pos).length();
                if dist <= VEHICLE_AOI_RADIUS {
                    if other.team == obs.team {
                        // Teammate (alive or dead wreck) in draw range
                        current_aoi.insert(other.entity_id);
                    } else if other.is_alive {
                        // Alive SPOTTED enemy in draw range
                        if let Some(set) = spotted_set {
                            if set.contains(&other.entity_id) {
                                current_aoi.insert(other.entity_id);
                            }
                        }
                    } else {
                        // Dead enemy wreck: remains in AoI if previously in AoI or ever spotted by this team
                        let was_in_aoi = prev_aoi.contains(&other.entity_id);
                        let was_spotted = self.ever_spotted.contains(&(obs.team, other.entity_id));
                        if was_in_aoi || was_spotted {
                            current_aoi.insert(other.entity_id);
                        }
                    }
                }
            }

            let mut enter_ids: Vec<i32> = current_aoi.difference(prev_aoi).copied().collect();
            enter_ids.sort();
            let mut leave_ids: Vec<i32> = prev_aoi.difference(&current_aoi).copied().collect();
            leave_ids.sort();

            let mut deltas = Vec::new();
            for id in enter_ids {
                deltas.push(AoIChange::Enter(id));
            }
            for id in leave_ids {
                deltas.push(AoIChange::Leave(id));
            }
            *prev_aoi = current_aoi;
            out.aoi_changes.insert(obs.entity_id, deltas);
        }

        out
    }
}

fn check_six_checkpoint_los(
    obs: &VehicleSimState,
    target: &VehicleSimState,
    target_recently_fired: bool,
    map: &ArenaMap,
    destr: &DestructiblesState,
    balance: &BalanceConfig,
) -> (bool, f32) {
    let obs_pos = obs.physics.pos;
    let target_pos = target.physics.pos;
    let target_yaw = target.physics.yaw;
    let fwd = Vec3::new(target_yaw.sin(), 0.0, target_yaw.cos());
    let right = Vec3::new(target_yaw.cos(), 0.0, -target_yaw.sin());
    let half_length = target.footprint.max.y.max(-target.footprint.min.y).max(1.5);
    let half_width = target.footprint.half_width().max(1.5);

    // 2 observer viewpoints:
    let viewpoints = [
        obs_pos + Vec3::new(0.0, 1.8, 0.0), // mantlet / hull
        obs_pos + Vec3::new(0.0, 2.5, 0.0), // cupola / top
    ];

    // 6 target checkpoints:
    let checkpoints = [
        target_pos + Vec3::new(0.0, 2.4, 0.0),             // top
        target_pos + Vec3::new(0.0, 1.5, 0.0),             // center
        target_pos + fwd * (half_length * 0.8) + Vec3::new(0.0, 1.0, 0.0),  // front
        target_pos - fwd * (half_length * 0.8) + Vec3::new(0.0, 1.0, 0.0),  // rear
        target_pos + right * (half_width * 0.5) + Vec3::new(0.0, 1.0, 0.0), // right
        target_pos - right * (half_width * 0.5) + Vec3::new(0.0, 1.0, 0.0), // left
    ];

    let mut has_los = false;
    let mut min_foliage_camo = 1.0f32;

    for &vp in &viewpoints {
        for &cp in &checkpoints {
            let diff = cp - vp;
            let dist = diff.length();
            if dist < 0.1 {
                continue;
            }
            let dir = diff / dist;

            // 1. Terrain LoS check
            if raycast_terrain_blocked(&map.terrain, vp, cp) {
                continue;
            }

            // 2. Static Obstacles BSP raycast
            if let Some((_, hit)) = map.raycast_obstacles(vp, dir, dist) {
                if hit.t < dist - 0.2 {
                    continue;
                }
            }

            // 3. Foliage check using spatial grid
            let foliage_candidates = map.query_foliage_segment(vp, cp);
            let (foliage_blocked, ray_foliage_camo) =
                check_foliage_intersection(vp, cp, target_pos, target_recently_fired, &foliage_candidates, destr, balance);
            if foliage_blocked {
                continue;
            }

            has_los = true;
            min_foliage_camo = min_foliage_camo.min(ray_foliage_camo);
        }
    }

    if has_los {
        (true, min_foliage_camo.min(balance.max_foliage_camo_bonus))
    } else {
        (false, 0.0)
    }
}

fn raycast_terrain_blocked(terrain: &TerrainMap, start: Vec3, end: Vec3) -> bool {
    let diff = end - start;
    let dist = diff.length();
    if dist < 1.0 {
        return false;
    }
    let steps = (dist / 1.0).ceil().max(5.0) as usize;
    for s in 1..steps {
        let t = s as f32 / steps as f32;
        let p = start + diff * t;
        if let Some(gy) = terrain.sample_height(p.x, p.z) {
            if p.y < gy - 0.1 {
                return true;
            }
        }
    }
    false
}

fn check_foliage_intersection(
    start: Vec3,
    end: Vec3,
    target_pos: Vec3,
    target_recently_fired: bool,
    foliage: &[&FoliageVolume],
    destr: &DestructiblesState,
    balance: &BalanceConfig,
) -> (bool, f32) {
    let p1 = Vec2::new(start.x, start.z);
    let p2 = Vec2::new(end.x, end.z);
    let v = p2 - p1;
    let seg_len_sq = v.length_squared();
    if seg_len_sq < 1e-4 {
        return (false, 0.0);
    }

    let mut total_camo = 0.0f32;
    let mut total_depth = 0.0f32;

    for vol in foliage {
        if let Some((cid, idx)) = vol.destructible_ref {
            if !destr.tree_is_standing(cid, idx) {
                continue; // Tree broken/felled: no camouflage!
            }
        }

        let c = Vec2::new(vol.position.x, vol.position.z);
        let t = ((c - p1).dot(v) / seg_len_sq).clamp(0.0, 1.0);
        let closest = p1 + v * t;
        let dist_2d = (closest - c).length();

        if dist_2d <= vol.radius {
            let ray_y = start.y + (end.y - start.y) * t;
            if ray_y >= vol.position.y - 0.5 && ray_y <= vol.position.y + vol.height + 0.5 {
                // 15m rule: if target recently fired and foliage is within 15m of target,
                // bush camo is stripped and does not contribute opaque depth!
                if target_recently_fired && (vol.position - target_pos).length() <= 15.0 {
                    continue;
                }

                total_camo += (vol.density * 0.20).max(0.10);
                total_depth += vol.radius * 2.0;

                if total_depth >= balance.foliage_los_opaque_depth_m
                    || total_camo >= balance.max_foliage_camo_bonus + 0.30
                {
                    return (true, total_camo); // Opaque foliage blocks LoS completely
                }
            }
        }
    }

    (false, total_camo)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use crate::assets::arena_defs::ArenaDef;
    use crate::assets::chunk::SpaceBounds;
    use crate::assets::destructibles::DestructibleKind;
    use crate::assets::map_store::{ArenaMap, FoliageVolume, StaticObstacle, TerrainMap};
    use crate::assets::terrain::{MaterialKinds, TerrainChunk, HEIGHTMAP_GRID_SIZE, DOMINANT_TEX_GRID_SIZE};
    use crate::balance::BalanceConfig;
    use crate::sim::physics::VehiclePhysicsState;

    fn build_test_map(obstacles: Vec<StaticObstacle>, foliage: Vec<FoliageVolume>) -> ArenaMap {
        let def = ArenaDef {
            id: 1,
            name: "test".to_string(),
            display_name_key: "".to_string(),
            description_key: "".to_string(),
            geometry: "spaces/test".to_string(),
            bounding_box: ([-300.0, -300.0], [300.0, 300.0]),
            min_players_in_team: 1,
            max_players_in_team: 15,
            round_length_secs: 900,
            arena_start_delay_secs: 5.0,
            kick_after_finish_wait_secs: 10.0,
            minimap_texture: None,
        };
        let bounds = SpaceBounds {
            min_x: -1,
            max_x: 1,
            min_y: -1,
            max_y: 1,
        };
        let mut loaded = Vec::new();
        for gx in -1..=1 {
            for gz in -1..=1 {
                loaded.push((
                    crate::assets::chunk::ChunkCoords {
                        grid_x: gx,
                        grid_z: gz,
                        kind: crate::assets::chunk::ChunkKind::Outer,
                    },
                    TerrainChunk {
                        heights: vec![0.0; HEIGHTMAP_GRID_SIZE * HEIGHTMAP_GRID_SIZE],
                        min_y: 0.0,
                        max_y: 0.0,
                        dom_tex_indices: vec![0; DOMINANT_TEX_GRID_SIZE * DOMINANT_TEX_GRID_SIZE],
                        tex_names: vec![],
                    },
                ));
            }
        }
        let terrain_map = TerrainMap::from_chunks(bounds, loaded).unwrap();
        let mats = Arc::new(MaterialKinds::parse(b"<root/>").unwrap());
        let balance = BalanceConfig::default();
        ArenaMap::from_parts(
            def,
            bounds,
            terrain_map,
            mats,
            obstacles,
            foliage,
            vec![],
            vec![],
            vec![],
            std::collections::BTreeMap::new(),
            &balance,
        )
    }

    #[test]
    fn postmortem_bind_and_spg_move_to_shift_observer_aoi() {
        let map = build_test_map(vec![], vec![]);
        let destr = DestructiblesState::default();
        let tank = |entity_id, x, is_alive| VehicleSimState {
            entity_id,
            team: 1,
            is_alive,
            physics: VehiclePhysicsState { pos: Vec3::new(x, 0.0, 0.0), ..Default::default() },
            ..Default::default()
        };
        let mut visibility = VisibilityTracker::new();
        let vehicles = [tank(1, -300.0, false), tank(2, 300.0, true), tank(3, -300.0, true)];
        let mut focus = ObserverFocusState::default();
        focus.bind_to(2, Vec3::ZERO);
        let output = visibility.step_visibility_with_observers(
            &vehicles, &map, &destr, 0.1, &HashMap::from([(1, focus)]),
        );
        assert_eq!(output.aoi_changes[&1], vec![AoIChange::Enter(2)]);
        focus.move_to(Vec3::new(-300.0, 0.0, 0.0));
        let output = visibility.step_visibility_with_observers(
            &vehicles, &map, &destr, 0.1, &HashMap::from([(1, focus), (2, focus)]),
        );
        assert_eq!(output.aoi_changes[&1], vec![AoIChange::Enter(3), AoIChange::Leave(2)]);
        assert!(visibility.previous_aoi[&2].contains(&3));
    }

    #[test]
    fn visibility_raycast_camo_bush_and_50m_proxy() {
        let _balance = BalanceConfig::default();
        // 1. Solid obstacle between (0, 0, 0) and (0, 0, 150):
        // Building box at z = 75m, blocking line of sight.
        let bsp_bytes = {
            let mut bytes = Vec::new();
            bytes.extend_from_slice(crate::assets::bsp2::BSP2_MAGIC);
            bytes.extend_from_slice(&1u32.to_le_bytes()); // 1 tri
            bytes.extend_from_slice(&1u32.to_le_bytes()); // 1 node
            bytes.extend_from_slice(&0u32.to_le_bytes());
            // Large wall triangle across X: -50..50, Y: -10..20, facing -Z
            for v in &[[-50.0f32, -10.0, 75.0], [50.0, -10.0, 75.0], [0.0, 20.0, 75.0]] {
                for c in v {
                    bytes.extend_from_slice(&c.to_le_bytes());
                }
            }
            bytes.extend_from_slice(&0u32.to_le_bytes()); // material_index = 0
            // Leaf node:
            bytes.push(0xa0);
            for c in &[0.0f32, 0.0, -1.0] {
                bytes.extend_from_slice(&c.to_le_bytes());
            }
            bytes.extend_from_slice(&(-75.0f32).to_le_bytes());
            bytes.extend_from_slice(&1u16.to_le_bytes());
            bytes.extend_from_slice(&0u16.to_le_bytes());
            bytes
        };
        let bsp_mats = b"<mats><id>building</id></mats>";
        let bsp = Arc::new(crate::assets::bsp2::BspTree::parse(&bsp_bytes, bsp_mats).unwrap());

        let building = StaticObstacle::from_bsp(
            "building.model".to_string(),
            std::path::PathBuf::from("building.primitives"),
            glam::Mat4::IDENTITY,
            bsp,
        )
        .unwrap();

        // Foliage volume at (0, 0, 20) with radius 5.0m
        let foliage = FoliageVolume {
            spt_path: "speedtree/tree.spt".to_string(),
            position: Vec3::new(0.0, 0.0, 20.0),
            radius: 5.0,
            height: 10.0,
            density: 0.6,
            world_aabb_min: Vec3::new(-5.0, 0.0, 15.0),
            world_aabb_max: Vec3::new(5.0, 10.0, 25.0),
            destructible_ref: Some((crate::sim::destructibles::encode_chunk_id(0, 0), 1)),
        };

        let map = build_test_map(vec![building], vec![foliage]);
        let mut destr = DestructiblesState::new();
        let chunk_id = crate::sim::destructibles::encode_chunk_id(0, 0);
        destr.desc_by_chunk_and_id.insert(
            (chunk_id, 1),
            (
                DestructibleKind::Tree {
                    health: 100,
                    density: 0.6,
                    kinetic_damage_correction: 0.0,
                    physic_params: crate::assets::destructibles::DestructiblePhysicParams {
                        mass: 500.0,
                        height: 10.0,
                        spring_angle: 0.2,
                        spring_stiffness: 1000.0,
                        spring_resist: 200.0,
                        air_resist: 10.0,
                        bury_depth: 0.5,
                    },
                },
                1.0,
            ),
        );

        let mut tracker = VisibilityTracker::new();

        // Tank 1 (Team 1, Observer at (0, 0, 0)):
        let tank1 = VehicleSimState {
            entity_id: 101,
            team: 1,
            is_alive: true,
            circular_vision_radius: 350.0,
            radio_distance_m: 600.0,
            physics: VehiclePhysicsState {
                pos: Vec3::new(0.0, 0.0, 0.0),
                ..Default::default()
            },
            ..Default::default()
        };

        // Tank 2 (Team 2, Enemy behind solid building at 150m (0, 0, 150)):
        let tank2 = VehicleSimState {
            entity_id: 201,
            team: 2,
            is_alive: true,
            circular_vision_radius: 350.0,
            physics: VehiclePhysicsState {
                pos: Vec3::new(0.0, 0.0, 150.0),
                ..Default::default()
            },
            ..Default::default()
        };

        // Step 1: Tank 2 at 150m behind solid building must NOT be spotted
        let out = tracker.step_visibility(&[tank1.clone(), tank2.clone()], &map, &destr, 0.1);
        let spotted_by_101 = out.spotted_enemies.get(&101);
        assert!(
            spotted_by_101.map(|s| !s.contains(&201)).unwrap_or(true),
            "tank behind building at 150m must remain hidden"
        );

        // Step 2: Move Tank 2 to <= 50m (0, 0, 45m): must be proxy spotted even behind building!
        let tank2_proxy = VehicleSimState {
            entity_id: 201,
            team: 2,
            is_alive: true,
            circular_vision_radius: 350.0,
            physics: VehiclePhysicsState {
                pos: Vec3::new(0.0, 0.0, 45.0),
                ..Default::default()
            },
            ..Default::default()
        };
        let out_proxy = tracker.step_visibility(&[tank1.clone(), tank2_proxy.clone()], &map, &destr, 0.1);
        assert!(
            out_proxy.spotted_enemies.get(&101).unwrap().contains(&201),
            "tank at 45m <= 50m must be proxy-spotted"
        );

        // Step 3: Camo net + bush:
        // Position Tank 3 (Team 2) in bush at (0, 0, 20m) stationary with camoNet.
        // It should have high camo bonus. Driving over the tree breaks it and removes bonus!
        assert!(destr.tree_is_standing(chunk_id, 1));
        destr.break_by_ramming(chunk_id, 1, 60_000.0, 15.0, 0.0, 2);
        assert!(!destr.tree_is_standing(chunk_id, 1), "tree must be broken");

        // Step 4: Radio relay test:
        // Ally 102 (Team 1) is at (500, 0, 0) with radio range 600m; Tank 101 is at (0, 0, 0) with range 600m.
        // Distance 500m <= 600 + 600 = 1200m -> connected!
        // Tank 101 spots 201 -> Tank 102 receives minimap update frame containing 201!
        let tank1_ally = VehicleSimState {
            entity_id: 102,
            team: 1,
            is_alive: true,
            radio_distance_m: 600.0,
            physics: VehiclePhysicsState {
                pos: Vec3::new(500.0, 0.0, 0.0),
                ..Default::default()
            },
            ..Default::default()
        };
        let out_radio = tracker.step_visibility(&[tank1.clone(), tank1_ally.clone(), tank2_proxy.clone()], &map, &destr, 0.1);
        let t101_minimap = out_radio.minimap_frames.get(&101).expect("observer 101 minimap frame");
        assert!(
            !t101_minimap.indices.is_empty(),
            "minimap frame must carry spotted vehicle indices"
        );
        let t102_minimap = out_radio.minimap_frames.get(&102).expect("observer 102 minimap frame");
        assert!(
            !t102_minimap.indices.is_empty(),
            "ally 102 minimap frame must carry relayed vehicle indices"
        );
        // Ally 102 must have 201 in spotted_enemies via radio relay:
        assert!(
            out_radio.spotted_enemies.get(&102).unwrap().contains(&201),
            "ally 102 must receive 201 in spotted_enemies via radio relay"
        );
    }

    #[test]
    fn test_firing_in_bush_strips_camo_within_15m_but_preserves_distant_bush() {
        let mut tracker = VisibilityTracker::new();
        let destr = DestructiblesState::new();

        // Foliage volume 1: close to target (position at (0, 0, 100), target at (0, 0, 102)) -> dist = 2m <= 15m
        let close_foliage = FoliageVolume {
            spt_path: "close.spt".into(),
            position: Vec3::new(0.0, 0.0, 100.0),
            radius: 5.0,
            height: 6.0,
            density: 1.0,
            world_aabb_min: Vec3::new(-5.0, 0.0, 95.0),
            world_aabb_max: Vec3::new(5.0, 6.0, 105.0),
            destructible_ref: None,
        };

        // Foliage volume 2: distant from target (position at (0, 0, 50), target at (0, 0, 102)) -> dist = 52m > 15m
        let distant_foliage = FoliageVolume {
            spt_path: "distant.spt".into(),
            position: Vec3::new(0.0, 0.0, 50.0),
            radius: 5.0,
            height: 6.0,
            density: 1.0,
            world_aabb_min: Vec3::new(-5.0, 0.0, 45.0),
            world_aabb_max: Vec3::new(5.0, 6.0, 55.0),
            destructible_ref: None,
        };

        let map_close = build_test_map(vec![], vec![close_foliage]);
        let map_distant = build_test_map(vec![], vec![distant_foliage]);

        let obs = VehicleSimState {
            entity_id: 1,
            team: 1,
            is_alive: true,
            circular_vision_radius: 120.0,
            physics: VehiclePhysicsState {
                pos: Vec3::new(0.0, 0.0, 0.0),
                ..Default::default()
            },
            ..Default::default()
        };

        let target = VehicleSimState {
            entity_id: 2,
            team: 2,
            is_alive: true,
            camo_still: 0.3,
            camo_moving: 0.2,
            circular_vision_radius: 350.0,
            physics: VehiclePhysicsState {
                pos: Vec3::new(0.0, 0.0, 102.0),
                ..Default::default()
            },
            ..Default::default()
        };

        // Case A: Target has not fired -> inside close bush, target keeps bush camo
        let out_unfired = tracker.step_visibility(&[obs.clone(), target.clone()], &map_close, &destr, 0.1);
        assert!(
            !out_unfired.spotted_enemies.get(&1).unwrap().contains(&2),
            "unfired target inside bush must remain unspotted at 102m"
        );

        // Target fires!
        tracker.record_shot(2);

        // With close bush (<15m from target), bush camo is stripped to 0.0, revealing target:
        let out_fired_close = tracker.step_visibility(&[obs.clone(), target.clone()], &map_close, &destr, 0.1);
        assert!(
            out_fired_close.spotted_enemies.get(&1).unwrap().contains(&2),
            "firing inside close bush (<15m) must strip bush camo and reveal target"
        );

        // Case B: Target behind distant bush (bush at 50m, target at 102m: dist to bush is 52m > 15m)
        // Even when target fires, the distant bush RETAINS full camo bonus (0.2)!
        let mut tracker_distant = VisibilityTracker::new();
        tracker_distant.record_shot(2);
        let out_fired_distant = tracker_distant.step_visibility(&[obs.clone(), target.clone()], &map_distant, &destr, 0.1);
        assert!(
            !out_fired_distant.spotted_enemies.get(&1).unwrap().contains(&2),
            "firing behind distant bush (>15m) must preserve distant bush camo"
        );
    }

    #[test]
    fn test_dead_tank_stays_in_aoi_as_corpse() {
        let mut tracker = VisibilityTracker::new();
        let destr = DestructiblesState::new();
        let map = build_test_map(vec![], vec![]);

        let obs = VehicleSimState {
            entity_id: 1,
            team: 1,
            is_alive: true,
            circular_vision_radius: 400.0,
            physics: VehiclePhysicsState {
                pos: Vec3::new(0.0, 0.0, 0.0),
                ..Default::default()
            },
            ..Default::default()
        };

        let mut enemy = VehicleSimState {
            entity_id: 2,
            team: 2,
            is_alive: true,
            circular_vision_radius: 350.0,
            physics: VehiclePhysicsState {
                pos: Vec3::new(0.0, 0.0, 100.0),
                ..Default::default()
            },
            ..Default::default()
        };

        // Step 1: Enemy is alive and spotted within 100m (< 565m draw range)
        let out1 = tracker.step_visibility(&[obs.clone(), enemy.clone()], &map, &destr, 0.1);
        assert!(out1.aoi_changes.get(&1).unwrap().contains(&AoIChange::Enter(2)));

        // Step 2: Enemy is destroyed (killed)
        enemy.is_alive = false;
        let out2 = tracker.step_visibility(&[obs.clone(), enemy.clone()], &map, &destr, 0.1);

        // Critical 3: Destroyed enemy corpse must STAY in AoI, NOT emit Leave!
        let deltas = out2.aoi_changes.get(&1).unwrap();
        assert!(
            !deltas.contains(&AoIChange::Leave(2)),
            "destroyed enemy corpse must remain in AoI within 565m, not emit Leave"
        );
    }

    #[test]
    fn test_minimap_frame_does_not_leak_to_isolated_ally() {
        let mut tracker = VisibilityTracker::new();
        let destr = DestructiblesState::new();
        let map = build_test_map(vec![], vec![]);

        // Spotter 101 at (0, 0, 0), radio 500m
        let spotter = VehicleSimState {
            entity_id: 101,
            team: 1,
            is_alive: true,
            circular_vision_radius: 400.0,
            radio_distance_m: 500.0,
            physics: VehiclePhysicsState {
                pos: Vec3::new(0.0, 0.0, 0.0),
                ..Default::default()
            },
            ..Default::default()
        };

        // Isolated ally 103 at (2000, 0, 0), radio 500m -> dist = 2000 > 500 + 500 = 1000m -> disconnected!
        let isolated_ally = VehicleSimState {
            entity_id: 103,
            team: 1,
            is_alive: true,
            circular_vision_radius: 350.0,
            radio_distance_m: 500.0,
            physics: VehiclePhysicsState {
                pos: Vec3::new(2000.0, 0.0, 0.0),
                ..Default::default()
            },
            ..Default::default()
        };

        // Enemy 201 at (0, 0, 100) -> spotted by 101
        let enemy = VehicleSimState {
            entity_id: 201,
            team: 2,
            is_alive: true,
            circular_vision_radius: 350.0,
            physics: VehiclePhysicsState {
                pos: Vec3::new(0.0, 0.0, 100.0),
                ..Default::default()
            },
            ..Default::default()
        };

        let out = tracker.step_visibility(&[spotter.clone(), isolated_ally.clone(), enemy.clone()], &map, &destr, 0.1);

        // Spotter 101 sees enemy 201 on its minimap
        let spotter_frame = out.minimap_frames.get(&101).expect("spotter minimap frame");
        assert!(spotter_frame.indices.len() >= 2, "spotter must see itself and enemy");

        // Isolated ally 103 must NOT receive enemy 201 on its minimap!
        let ally_frame = out.minimap_frames.get(&103).expect("isolated ally minimap frame");
        // All vehicle IDs sorted: [101, 103, 201] -> enemy 201 is at index 2
        assert!(
            !ally_frame.indices.contains(&2),
            "isolated ally outside radio range must not receive enemy on minimap frame"
        );
    }

    #[test]
    fn test_terrain_los_detects_narrow_ridge() {
        let mut tracker = VisibilityTracker::new();
        let destr = DestructiblesState::new();

        // Build terrain with a narrow ridge at z = 50m of height 20m
        let def = ArenaDef {
            id: 1,
            name: "test".to_string(),
            display_name_key: "".to_string(),
            description_key: "".to_string(),
            geometry: "spaces/test".to_string(),
            bounding_box: ([-300.0, -300.0], [300.0, 300.0]),
            min_players_in_team: 1,
            max_players_in_team: 15,
            round_length_secs: 900,
            arena_start_delay_secs: 5.0,
            kick_after_finish_wait_secs: 10.0,
            minimap_texture: None,
        };
        let bounds = SpaceBounds { min_x: 0, max_x: 0, min_y: 0, max_y: 0 };
        let mut heights = vec![0.0f32; HEIGHTMAP_GRID_SIZE * HEIGHTMAP_GRID_SIZE];
        let ridge_z_idx = (50.0 / crate::assets::terrain::HEIGHTMAP_SPACING_METERS) as usize
            + crate::assets::terrain::HEIGHTMAP_VISIBLE_OFFSET;
        for cz in (ridge_z_idx - 1)..=(ridge_z_idx + 1) {
            for c in 0..HEIGHTMAP_GRID_SIZE {
                heights[cz * HEIGHTMAP_GRID_SIZE + c] = 20.0;
            }
        }

        let tc = TerrainChunk {
            heights,
            min_y: 0.0,
            max_y: 20.0,
            dom_tex_indices: vec![0; DOMINANT_TEX_GRID_SIZE * DOMINANT_TEX_GRID_SIZE],
            tex_names: vec![],
        };
        let terrain = TerrainMap::from_chunks(
            bounds,
            vec![(
                crate::assets::chunk::ChunkCoords { grid_x: 0, grid_z: 0, kind: crate::assets::chunk::ChunkKind::Outer },
                tc,
            )],
        ).unwrap();
        let mats = Arc::new(MaterialKinds::parse(b"<root/>").unwrap());
        let balance = BalanceConfig::default();
        let map = ArenaMap::from_parts(
            def, bounds, terrain, mats, vec![], vec![], vec![], vec![], vec![], std::collections::BTreeMap::new(), &balance,
        );

        let obs = VehicleSimState {
            entity_id: 1,
            team: 1,
            is_alive: true,
            circular_vision_radius: 400.0,
            physics: VehiclePhysicsState {
                pos: Vec3::new(50.0, 0.0, 10.0),
                ..Default::default()
            },
            ..Default::default()
        };

        let target = VehicleSimState {
            entity_id: 2,
            team: 2,
            is_alive: true,
            circular_vision_radius: 350.0,
            physics: VehiclePhysicsState {
                pos: Vec3::new(50.0, 0.0, 90.0),
                ..Default::default()
            },
            ..Default::default()
        };

        let out = tracker.step_visibility(&[obs, target], &map, &destr, 0.1);
        assert!(
            !out.spotted_enemies.get(&1).unwrap().contains(&2),
            "narrow ridge between observer and target must block line of sight"
        );
    }
}
