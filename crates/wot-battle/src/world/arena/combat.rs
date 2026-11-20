//! Arena weapon inputs, projectile updates, and damage application.

use std::collections::HashMap;

use glam::Vec3;
use wot065_data::catalog::Catalog;
use wot065_data::item_defs::{ShellXmlDef, ShotDef};

use super::{
    ArenaInstance, BattleDamage, ShotFiredEvent, VehicleCombatProfile, ARENA_PERIOD_BATTLE,
};
use crate::assets::map_store::VehicleCollisionCache;
use crate::lobby::BattleMode;
use crate::sim::{
    apply_external_splash_damage, apply_internal_crit_ray, step_gun_aiming, step_projectiles,
    DamageInfoCode, ImpactOutcome, VehicleBallisticsTarget, SHOT_RESULT_ARMOR_PIERCED,
    SHOT_RESULT_ARMOR_PIERCED_NO_DAMAGE,
};

#[derive(Clone, Copy)]
enum ShellEffect<'a> {
    Direct(&'a ImpactOutcome),
    Blast { center: Vec3, radius: f32 },
}

impl ArenaInstance {
    fn alloc_next_shot_id(&mut self) -> u32 {
        let id = self.next_shot_id;
        self.next_shot_id = self.next_shot_id.wrapping_add(1);
        if self.next_shot_id == 0 {
            self.next_shot_id = 1;
        }
        id
    }

    /// Queue at most one shot per ready vehicle for the next battle frame.
    pub fn request_fire(&mut self, account_db_id: i64) -> bool {
        if self.period != ARENA_PERIOD_BATTLE {
            return false;
        }
        let Some(id) = self
            .vehicle_for_account(account_db_id)
            .filter(|v| v.is_alive && !v.left_arena && v.is_avatar_ready && v.is_crew_active)
            .map(|v| v.entity_id)
        else {
            return false;
        };
        let Some(aim) = self.aim_states.get_mut(&id) else {
            return false;
        };
        let ready = aim.reload_left <= 0.0
            && aim
                .ammo_counts
                .get(aim.active_shell_idx)
                .is_some_and(|&(_, count)| count > 0);
        if !ready {
            // The client holds further shoot commands until it hears the reload state.
            aim.reload_dirty = true;
            return false;
        }
        self.fire_requests.insert(id)
    }

    fn fire_requested_shots<R: rand::Rng + ?Sized>(&mut self, rng: &mut R) {
        for id in std::mem::take(&mut self.fire_requests) {
            let Some(aim) = self.aim_states.get(&id) else {
                continue;
            };
            let Some(&(cd, count)) = aim.ammo_counts.get(aim.active_shell_idx) else {
                continue;
            };
            if count == 0 || aim.reload_left > 0.0 {
                continue;
            }
            let Some(profile) = self.combat_profiles.get(&id) else {
                continue;
            };
            let Some((shot, shell)) = profile
                .all_shots
                .iter()
                .zip(&profile.all_shells)
                .find(|(shot, _)| shot.shell_cd == cd)
                .map(|(shot, shell)| (shot.clone(), shell.clone()))
            else {
                continue;
            };
            let radius = (profile.base_disp_angle * aim.current_disp_factor).clamp(0.0, 0.12);
            let amplitude = radius * rng.gen::<f32>().sqrt();
            let angle = rng.gen::<f32>() * std::f32::consts::TAU;
            let (sin, cos) = angle.sin_cos();
            let shot_id = self.alloc_next_shot_id();
            self.shoot_by_entity_id(
                id,
                shot_id,
                &shot,
                &shell,
                (cos * amplitude, sin * amplitude),
            );
        }
    }

    pub(super) fn shoot_by_entity_id(
        &mut self,
        entity_id: i32,
        shot_id: u32,
        shot_def: &ShotDef,
        shell: &ShellXmlDef,
        dispersion_offset: (f32, f32),
    ) -> Option<u32> {
        let v = self.vehicles.iter().find(|v| {
            v.entity_id == entity_id && v.is_alive && !v.left_arena && v.is_crew_active
        })?;
        let veh_id = v.entity_id;
        let veh_team = v.team;
        let aim = self.aim_states.entry(veh_id).or_default();
        let &(shell_cd, count) = aim.ammo_counts.get(aim.active_shell_idx)?;
        if aim.reload_left > 0.0 || count == 0 {
            return None;
        }

        let profile = self.combat_profiles.get(&veh_id)?;
        let pose = crate::sim::VehiclePhysicsState {
            pos: v.position,
            yaw: v.yaw,
            pitch: v.pitch,
            roll: v.roll,
            ..Default::default()
        };
        let (muzzle_pos, shot_dir) = crate::sim::ballistics::gun_ray(
            &pose,
            aim,
            profile.hull_pos,
            profile.turret_pos,
            profile.gun_pos,
        );

        let proj = crate::sim::spawn_shot(
            shot_id,
            veh_id,
            veh_team,
            muzzle_pos,
            shot_dir,
            shot_def,
            shell,
            &self.balance,
            dispersion_offset,
        );

        let shot_speed = proj.velocity.length();
        if !muzzle_pos.is_finite() || !shot_speed.is_finite() || shot_speed <= 0.0 {
            return None;
        }
        let shot_gravity = proj.gravity.y.abs();
        let shot_ev = ShotFiredEvent {
            shot_id,
            attacker_id: veh_id,
            muzzle_pos,
            shot_dir: proj.velocity / shot_speed,
            speed: shot_speed,
            gravity: shot_gravity,
        };
        self.projectiles.push(proj);
        if !self.in_step {
            self.pending_external_shots.push(shot_ev.clone());
        }
        self.last_shots.push(shot_ev);
        self.visibility.record_shot(veh_id);
        let stats = self.statistics.entry(veh_id).or_default();
        stats.shots += 1;

        aim.ammo_counts[aim.active_shell_idx].1 -= 1;
        *stats.shells_used.entry(shell_cd).or_default() += 1;
        aim.ammo_dirty = true;

        let reload_factor = self
            .sim_states
            .get(&veh_id)
            .map(|s| s.modifiers.gun_reload_time_factor)
            .unwrap_or(1.0);
        aim.reload_left = (profile.reload_time * reload_factor).max(0.05);
        aim.reload_dirty = true;

        // Important 5: Bloom aim.current_disp_factor using after_shot factor
        let after_shot = profile.disp_factors[3];
        aim.current_disp_factor = (aim.current_disp_factor.powi(2) + after_shot.powi(2)).sqrt();
        if let Some(snapshot) = self.targeting_snapshots.get_mut(&veh_id) {
            snapshot.shot_disp_multiplier_factor = aim.current_disp_factor;
            snapshot.dispersion_angle = aim.current_disp_factor * profile.base_disp_angle;
        }

        Some(shot_id)
    }

    pub fn step_combat<R: rand::Rng + ?Sized>(
        &mut self,
        dt: f32,
        collision_cache: Option<&VehicleCollisionCache>,
        catalog: Option<&Catalog>,
        rng: &mut R,
    ) -> Vec<ImpactOutcome> {
        if dt <= 0.0 || !dt.is_finite() {
            return Vec::new();
        }
        if let (Some(cache), Some(catalog)) =
            (collision_cache.or(self.collision_cache.as_deref()), catalog)
        {
            for v in &self.vehicles {
                let Ok(descr) = wot065_data::descr::VehicleDescr::from_bytes(&v.veh_comp_descr)
                else {
                    continue;
                };
                if let (Some(def), Some(profile)) = (
                    catalog.vehicle_def_by_id(descr.nation_id, u16::from(descr.type_id)),
                    self.combat_profiles.get_mut(&v.entity_id),
                ) {
                    if let Ok(collision) = cache.get_or_load_by_ids(
                        def,
                        descr.chassis_id,
                        descr.turret_id,
                        descr.gun_id,
                    ) {
                        profile.collision_set = Some(collision);
                    }
                }
            }
        }

        self.update_guns(dt);
        self.fire_requested_shots(rng);
        if self.projectiles.is_empty() {
            return Vec::new();
        }

        let fallback = VehicleCombatProfile::default();
        let targets: Vec<_> = self
            .vehicles
            .iter()
            .filter_map(|vehicle| {
                let id = vehicle.entity_id;
                let sim = self.sim_states.get(&id)?;
                let profile = self.combat_profiles.get(&id).unwrap_or(&fallback);
                let (turret_yaw, gun_pitch) = self
                    .aim_states
                    .get(&id)
                    .map_or((0.0, 0.0), |aim| (aim.turret_yaw, aim.gun_pitch));
                Some(VehicleBallisticsTarget {
                    entity_id: id,
                    team: vehicle.team,
                    is_alive: vehicle.is_alive,
                    left_arena: vehicle.left_arena && self.mode != BattleMode::Random,
                    physics: &sim.physics,
                    modifiers: &sim.modifiers,
                    collision_set: profile.collision_set.as_deref(),
                    chassis_armor: profile.chassis_armor.as_ref(),
                    hull_armor: profile.hull_armor.as_ref(),
                    turret_armor: profile.turret_armor.as_ref(),
                    gun_armor: profile.gun_armor.as_ref(),
                    turret_yaw,
                    gun_pitch,
                    hull_pos: profile.hull_pos,
                    turret_pos: profile.turret_pos,
                    gun_pos: profile.gun_pos,
                })
            })
            .collect();
        let shell_stats: HashMap<_, _> = self
            .projectiles
            .iter()
            .map(|p| {
                (
                    p.shot_id,
                    (p.armor_damage.round().max(0.0) as u32, p.explosion_radius),
                )
            })
            .collect();
        let impacts = step_projectiles(
            &mut self.projectiles,
            &self.map,
            Some(&mut self.destructibles),
            &targets,
            &self.balance,
            dt,
            rng,
        );

        for impact in &impacts {
            self.last_destructible_deltas
                .extend(impact.destructible_deltas.iter().cloned());
            let (potential_hp, radius) = shell_stats[&impact.shot_id];
            if let Some(id) = impact.struck_entity_id {
                let hit = self.apply_shell_damage(
                    impact.attacker_id,
                    id,
                    impact.armor_damage,
                    ShellEffect::Direct(impact),
                    rng,
                );
                if hit {
                    if let Some(attacker) = self.statistics.get_mut(&impact.attacker_id) {
                        attacker.hits += 1;
                        attacker.potential_damage_dealt += potential_hp;
                        if matches!(
                            impact.shot_result,
                            SHOT_RESULT_ARMOR_PIERCED | SHOT_RESULT_ARMOR_PIERCED_NO_DAMAGE
                        ) {
                            attacker.pierced += 1;
                        }
                    }
                    if let Some(victim) = self.statistics.get_mut(&id) {
                        victim.shots_received += 1;
                        victim.potential_damage_received += potential_hp;
                    }
                }
            }
            for &(id, hp) in &impact.splash_hits {
                self.apply_shell_damage(
                    impact.attacker_id,
                    id,
                    hp,
                    ShellEffect::Blast {
                        center: impact.hit_point,
                        radius,
                    },
                    rng,
                );
            }
        }
        self.last_impact_outcomes.extend(impacts.iter().cloned());
        impacts
    }

    fn update_guns(&mut self, dt: f32) {
        let fallback = VehicleCombatProfile::default();
        for vehicle in self.vehicles.iter().filter(|v| v.is_alive && !v.left_arena) {
            let id = vehicle.entity_id;
            let Some(sim) = self.sim_states.get(&id) else {
                continue;
            };
            let Some(aim) = self.aim_states.get_mut(&id) else {
                continue;
            };
            let profile = self.combat_profiles.get(&id).unwrap_or(&fallback);
            let (flight_speed, flight_drop) = profile
                .all_shots
                .iter()
                .find(|shot| shot.shell_cd == aim.current_shell_cd)
                .map_or(
                    (profile.default_shot_speed, profile.default_shot_gravity),
                    |shot| (shot.speed as f32, shot.gravity as f32),
                );
            let snapshot = step_gun_aiming(
                aim,
                &sim.physics,
                profile.turret_yaw_limits,
                profile.turret_rot_speed,
                profile.gun_pitch_limits,
                profile.gun_rot_speed,
                profile.hull_pos,
                profile.turret_pos,
                profile.gun_pos,
                flight_speed,
                flight_drop,
                profile.base_aiming_time,
                profile.base_disp_angle,
                profile.disp_factors,
                &sim.modifiers,
                &self.map,
                dt,
            );
            self.targeting_snapshots.insert(id, snapshot);
        }
    }

    fn apply_shell_damage<R: rand::Rng + ?Sized>(
        &mut self,
        attacker_id: i32,
        target_id: i32,
        amount: i16,
        effect: ShellEffect<'_>,
        rng: &mut R,
    ) -> bool {
        let Some(vehicle) = self
            .vehicles
            .iter_mut()
            .find(|v| v.entity_id == target_id && v.is_alive)
        else {
            return false;
        };
        let Some(sim) = self.sim_states.get_mut(&target_id) else {
            return false;
        };
        let prior_hp = vehicle.health;
        sim.health = i32::from(prior_hp.saturating_sub(amount.max(0)).max(0));
        sim.is_alive = sim.health > 0;

        let module_damage = match effect {
            ShellEffect::Direct(impact) => {
                if impact.encoded_segment != 0
                    && !vehicle.damage_stickers.contains(&impact.encoded_segment)
                {
                    vehicle.damage_stickers.push(impact.encoded_segment);
                }
                if let Some(ray) = &impact.internal_ray {
                    let model = self
                        .combat_profiles
                        .get(&target_id)
                        .and_then(|p| p.collision_set.as_deref());
                    let before = sim.damage.clone();
                    self.last_damage_events.extend(apply_internal_crit_ray(
                        ray,
                        sim,
                        model,
                        &self.balance,
                        rng,
                    ));
                    before != sim.damage
                } else {
                    false
                }
            }
            ShellEffect::Blast { center, radius } => {
                let events = apply_external_splash_damage(
                    sim,
                    center,
                    radius,
                    f32::from(amount),
                    attacker_id,
                );
                let damaged = !events.is_empty();
                self.last_damage_events.extend(events);
                damaged
            }
        };

        vehicle.is_crew_active = sim.damage.is_crew_active;
        vehicle.is_burning = sim.damage.fire_state.is_some();
        vehicle.is_alive = sim.is_alive && sim.health > 0 && vehicle.is_crew_active;
        vehicle.health = if vehicle.is_alive {
            sim.health.clamp(0, i32::from(vehicle.max_health)) as i16
        } else {
            0
        };
        sim.health = i32::from(vehicle.health);
        sim.is_alive = vehicle.is_alive;
        let hp = prior_hp.saturating_sub(vehicle.health).max(0) as u16;
        let death = (!vehicle.is_alive).then_some(if vehicle.is_crew_active {
            DamageInfoCode::DeathFromShot
        } else {
            DamageInfoCode::DeathFromInactiveCrewAtShot
        });
        if hp > 0 || module_damage || death.is_some() {
            self.battle_damage.push(BattleDamage {
                attacker_id,
                target_id,
                hp,
                module_damage,
                death,
            });
        }
        true
    }
}
