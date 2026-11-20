//! Fixed-step round simulation and match outcomes.

use std::collections::{BTreeMap, BTreeSet};
use std::time::Duration;

use glam::Vec3;
use wot065_data::pickle::{self, Value};

use super::{
    ArenaDrivingEvents, ArenaFinished, ArenaInstance, BattleDamage, FinishReason,
    ARENA_PERIOD_AFTERBATTLE, ARENA_PERIOD_BATTLE, ARENA_PERIOD_PREBATTLE, ARENA_PERIOD_WAITING,
    ARENA_UPDATE_BASE_CAPTURED, ARENA_UPDATE_BASE_POINTS, ARENA_UPDATE_PERIOD,
    ARENA_UPDATE_TEAM_KILLER, ARENA_UPDATE_VEHICLE_KILLED, ARENA_UPDATE_VEHICLE_STATISTICS,
};
use crate::lobby::BattleMode;
use crate::sim::bot_ai::ArenaReadView;
use crate::sim::capture::BaseCaptureEvent;
use crate::sim::physics::{normalize_angle, step_arena_vehicles};
use crate::sim::visibility::{AoIChange, VisibilityOutput};
use crate::sim::{activate_equipment, step_damage_state, DamageInfoCode, VehicleSimState};

const FRAME: Duration = Duration::from_millis(50);

#[derive(Debug, Default)]
pub(super) struct RoundClock {
    elapsed: Duration,
    deadline: Duration,
    simulated_until: Duration,
}

impl ArenaInstance {
    /// Arena time advanced so far; the simulation state reflects this instant once every
    /// advance covers whole frames.
    pub fn clock_elapsed(&self) -> Duration {
        self.clock.elapsed
    }

    fn enter_period(&mut self, period: u8, start: Duration, length: Duration) {
        self.clock.deadline = start.saturating_add(length);
        self.period = period;
        self.period_length_secs = length.as_secs_f64().ceil() as u32;
        self.period_end_time = self.clock.deadline.as_secs_f64();
    }

    pub(super) fn check_all_ready(&mut self, updates: &mut Vec<(u8, Vec<u8>)>) {
        if self.period != ARENA_PERIOD_WAITING
            || !self.vehicles.iter().any(|v| !v.left_arena)
            || self
                .vehicles
                .iter()
                .any(|v| !v.is_bot && !v.left_arena && !v.is_avatar_ready)
        {
            return;
        }
        if self.prebattle_countdown_secs == 0 {
            self.start_battle_at(self.clock.elapsed);
        } else {
            self.enter_period(
                ARENA_PERIOD_PREBATTLE,
                self.clock.elapsed,
                Duration::from_secs(u64::from(self.prebattle_countdown_secs)),
            );
        }
        updates.push((ARENA_UPDATE_PERIOD, self.period_pickle()));
    }

    #[cfg(test)]
    pub(crate) fn start_battle(&mut self) {
        self.start_battle_at(self.clock.elapsed);
    }

    fn start_battle_at(&mut self, at: Duration) {
        self.battle_started_at = at.as_secs_f64();
        self.clock.simulated_until = at;
        self.enter_period(
            ARENA_PERIOD_BATTLE,
            at,
            Duration::from_secs(u64::from(self.round_length_secs)),
        );
    }

    fn finish_round(&mut self, result: ArenaFinished) {
        self.finished = Some(result);
        let postbattle = Duration::try_from_secs_f32(self.map.def.kick_after_finish_wait_secs)
            .unwrap_or(Duration::ZERO);
        self.enter_period(
            ARENA_PERIOD_AFTERBATTLE,
            self.clock.simulated_until,
            postbattle,
        );
        self.projectiles.clear();
        self.fire_requests.clear();
        let duration = self.server_time - self.battle_started_at;
        for (&id, stats) in &mut self.statistics {
            stats.capture_points = self.capture.points_for(id);
            if stats.killer_id.is_none() {
                stats.lifetime_secs = duration;
            }
        }
        for v in &mut self.vehicles {
            v.move_flags = 0;
            v.speed = 0.0;
            v.rspeed = 0.0;
            v.engine_mode = [u8::from(v.is_alive), 0];
            if let Some(sim) = self.sim_states.get_mut(&v.entity_id) {
                sim.physics.speed_mps = 0.0;
                sim.physics.rot_speed_rps = 0.0;
                sim.physics.move_flags = 0;
                sim.physics.cruise_mode = 0;
                sim.physics.engine_mode = v.engine_mode;
            }
        }
        self.arena_updates
            .push((ARENA_UPDATE_PERIOD, self.period_pickle()));
    }

    pub fn step_with_events(&mut self, dt_secs: f32) -> ArenaDrivingEvents {
        let seconds = (f64::from(dt_secs) * 1_000_000.0).round() / 1_000_000.0;
        let Ok(elapsed) = Duration::try_from_secs_f64(seconds) else {
            return ArenaDrivingEvents::default();
        };
        self.advance(elapsed)
    }

    /// Advance the round clock and run every complete simulation frame in this interval.
    pub fn advance(&mut self, elapsed: Duration) -> ArenaDrivingEvents {
        if elapsed.is_zero() {
            return ArenaDrivingEvents::default();
        }

        let until = self.clock.elapsed.saturating_add(elapsed);
        let mut updates = Vec::new();
        self.check_all_ready(&mut updates);
        self.arena_updates.extend(updates);
        if self.period == ARENA_PERIOD_PREBATTLE && self.clock.deadline <= until {
            self.start_battle_at(self.clock.deadline);
            self.arena_updates
                .push((ARENA_UPDATE_PERIOD, self.period_pickle()));
        }

        let mut events = ArenaDrivingEvents::default();
        let mut pushed_ids = BTreeSet::new();
        let mut began_frames = false;
        while self.period == ARENA_PERIOD_BATTLE {
            let at = self
                .clock
                .simulated_until
                .saturating_add(FRAME)
                .min(self.clock.deadline);
            if at > until {
                break;
            }
            if !began_frames {
                self.begin_frames();
                began_frames = true;
            }
            self.server_time = at.as_secs_f64();
            let dt = (at - self.clock.simulated_until).as_secs_f32();
            self.clock.simulated_until = at;
            if dt == 0.0 {
                self.update_battle_lifecycle(&[], dt);
                break;
            }
            let frame = self.run_battle_frame(dt);
            pushed_ids.extend(frame.pushed_vehicles.into_iter().map(|(id, _)| id));
            events.ramming_damages.extend(frame.ramming_damages);
            events
                .broken_destructibles
                .extend(frame.broken_destructibles);
        }
        self.clock.elapsed = until;
        self.server_time = until.as_secs_f64();

        if began_frames {
            self.in_step = false;
            self.last_state_changed_vehicles.sort_unstable();
            self.last_state_changed_vehicles.dedup();
            events.pushed_vehicles = pushed_ids
                .into_iter()
                .filter_map(|id| {
                    let position = self.vehicle_by_id(id)?.position;
                    Some((id, (position.x, position.z)))
                })
                .collect();
            self.last_driving_events = events.clone();
        }
        events
    }

    fn begin_frames(&mut self) {
        self.last_destructible_deltas.clear();
        self.last_impact_outcomes.clear();
        self.last_visibility.aoi_changes.clear();
        self.last_visibility.minimap_frames.clear();
        self.last_visibility.first_spotted_events.clear();
        self.last_damage_events = std::mem::take(&mut self.pending_external_damage_events);
        self.last_state_changed_vehicles =
            std::mem::take(&mut self.pending_external_state_changed_vehicles);
        self.last_shots = std::mem::take(&mut self.pending_external_shots);
        self.in_step = true;
    }

    fn run_battle_frame(&mut self, dt: f32) -> ArenaDrivingEvents {
        self.collect_bot_controls(dt);
        let driving = self.drive_frame(dt);

        let mut rng = std::mem::replace(&mut self.rng, rand::SeedableRng::seed_from_u64(0));
        self.step_combat(dt, None, None, &mut rng);
        self.rng = rng;

        self.step_vehicle_effects(dt);
        // Every arena vehicle, leavers included: the client keeps them in its vehicle list
        // (minimap indices), and a Random leaver's tank stays on the field.
        let mut field: Vec<_> = self
            .vehicles
            .iter()
            .filter_map(|v| self.sim_states.get(&v.entity_id).cloned())
            .collect();
        field.sort_unstable_by_key(|state| state.entity_id);
        self.refresh_visibility(&field, dt);
        self.update_battle_lifecycle(&field, dt);
        driving
    }

    fn collect_bot_controls(&mut self, dt: f32) {
        let view = ArenaReadView {
            vehicles: &self.vehicles,
            sim_states: &self.sim_states,
            combat_profiles: &self.combat_profiles,
            aim_states: &self.aim_states,
            visibility: &self.visibility,
            last_visibility: &self.last_visibility,
            map: &self.map,
            balance: &self.balance,
            server_time: self.server_time,
        };
        for (id, intent) in self.bot_controller.step_bots(&view, dt) {
            let Some(vehicle) = self.vehicles.iter_mut().find(|v| v.entity_id == id) else {
                continue;
            };
            vehicle.move_flags = intent.move_flags;
            if let Some(sim) = self.sim_states.get_mut(&id) {
                sim.physics.cruise_mode = intent.cruise_mode;
                if let Some((equipment, extra)) = intent.activate_equipment {
                    if let Ok(events) = activate_equipment(sim, equipment, extra) {
                        vehicle.is_burning = sim.damage.fire_state.is_some();
                        vehicle.is_crew_active = sim.damage.is_crew_active;
                        self.last_state_changed_vehicles.push(id);
                        self.last_damage_events.extend(events);
                    }
                }
            }
            if let Some(aim) = self.aim_states.get_mut(&id) {
                if let Some((index, cd)) = intent
                    .request_shell_index
                    .and_then(|index| aim.ammo_counts.get(index).map(|&(cd, _)| (index, cd)))
                {
                    aim.active_shell_idx = index;
                    aim.current_shell_cd = cd;
                    aim.ammo_dirty = true;
                }
                if let Some(target) = intent.target_point {
                    aim.target_point = target;
                    aim.has_target_point = true;
                }
            }
            if intent.shoot {
                self.fire_requests.insert(id);
            }
        }
    }

    fn drive_frame(&mut self, dt: f32) -> ArenaDrivingEvents {
        let mut controls = Vec::with_capacity(self.vehicles.len());
        for v in &mut self.vehicles {
            let Some(sim) = self.sim_states.get_mut(&v.entity_id) else {
                continue;
            };
            sim.health = i32::from(v.health);
            sim.is_alive = v.is_alive;
            sim.physics.pos = v.position;
            sim.physics.yaw = v.yaw;
            sim.physics.speed_mps = v.speed;
            sim.physics.rot_speed_rps = v.rspeed;
            sim.physics.move_flags = v.move_flags;
            if !v.is_alive || v.left_arena {
                sim.physics.speed_mps = 0.0;
                sim.physics.rot_speed_rps = 0.0;
                sim.physics.engine_mode = [0, 0];
            }
            if !v.left_arena || self.mode == BattleMode::Random {
                let assist = gun_steering(
                    &self.aim_states,
                    &self.combat_profiles,
                    v.entity_id,
                    v.yaw,
                    v.position,
                );
                controls.push((v.entity_id, !v.left_arena, assist));
            }
        }
        let frame = step_arena_vehicles(
            &mut self.sim_states,
            controls,
            &self.map,
            &self.balance,
            Some(&self.destructibles),
            dt,
        );
        for v in &mut self.vehicles {
            if let Some(sim) = self.sim_states.get(&v.entity_id) {
                let state = &sim.physics;
                v.position = state.pos;
                v.yaw = state.yaw;
                v.pitch = state.pitch;
                v.roll = state.roll;
                v.speed = state.speed_mps;
                v.rspeed = state.rot_speed_rps;
                v.engine_mode = state.engine_mode;
            }
        }

        let mut events = ArenaDrivingEvents::default();
        events
            .pushed_vehicles
            .extend(frame.pushes.into_iter().map(|(id, p)| (id, (p.x, p.z))));
        for hit in frame.crushed {
            let Ok(index) = u8::try_from(hit.index) else {
                continue;
            };
            let fall_speed = if hit.speed > 10.0 {
                3
            } else if hit.speed > 5.0 {
                2
            } else {
                1
            };
            let deltas = self
                .destructibles
                .break_by_ramming(hit.chunk, index, hit.mass_kg, hit.speed, hit.yaw, fall_speed);
            if !deltas.is_empty() {
                events.broken_destructibles.push((hit.chunk, hit.index));
                self.last_destructible_deltas.extend(deltas);
            }
        }
        for ram in frame.rams {
            for (target, source, amount) in [
                (ram.first, ram.second, ram.first_damage),
                (ram.second, ram.first, ram.second_damage),
            ] {
                if let Some(applied) = self.apply_ram(target, source, amount) {
                    events.ramming_damages.push((target, applied));
                }
            }
        }
        events
    }

    fn apply_ram(&mut self, target_id: i32, attacker_id: i32, damage: i16) -> Option<i16> {
        let target = self
            .vehicles
            .iter_mut()
            .find(|v| v.entity_id == target_id && v.is_alive)?;
        let hp = damage.min(target.health).max(0);
        if hp == 0 {
            return None;
        }
        target.health -= hp;
        target.is_alive = target.health > 0;
        if let Some(sim) = self.sim_states.get_mut(&target_id) {
            sim.health = i32::from(target.health);
            sim.is_alive = target.is_alive;
        }
        self.last_state_changed_vehicles.push(target_id);
        self.battle_damage.push(BattleDamage {
            attacker_id,
            target_id,
            hp: hp as u16,
            module_damage: false,
            death: (!target.is_alive).then_some(DamageInfoCode::DeathFromRamming),
        });
        Some(hp)
    }

    fn step_vehicle_effects(&mut self, dt: f32) {
        for v in &mut self.vehicles {
            if !v.is_alive || (v.left_arena && self.mode != BattleMode::Random) {
                continue;
            }
            let Some(sim) = self.sim_states.get_mut(&v.entity_id) else {
                continue;
            };
            let before = (v.health, v.is_crew_active, v.is_burning);
            let attacker_id = sim
                .damage
                .fire_state
                .as_ref()
                .map_or(0, |fire| fire.attacker_id);
            self.last_damage_events
                .extend(step_damage_state(sim, &self.balance, dt));
            v.health = sim.health.clamp(0, i32::from(v.max_health)) as i16;
            v.is_alive = sim.is_alive;
            v.is_crew_active = sim.damage.is_crew_active;
            v.is_burning = sim.damage.fire_state.is_some();
            if v.health < before.0 || !v.is_alive {
                let cause = if attacker_id != 0 {
                    DamageInfoCode::DeathFromFire
                } else {
                    DamageInfoCode::DeathFromInactiveCrew
                };
                self.battle_damage.push(BattleDamage {
                    attacker_id,
                    target_id: v.entity_id,
                    hp: (before.0 - v.health).max(0) as u16,
                    module_damage: false,
                    death: (!v.is_alive).then_some(cause),
                });
            }
            if before != (v.health, v.is_crew_active, v.is_burning) {
                self.last_state_changed_vehicles.push(v.entity_id);
            }
        }
    }

    fn refresh_visibility(&mut self, field: &[VehicleSimState], dt: f32) {
        let VisibilityOutput {
            aoi_changes,
            spotted_enemies,
            first_spotted_events,
            minimap_frames,
        } = self.visibility.step_visibility_with_observers(
            field,
            &self.map,
            &self.destructibles,
            dt,
            &self.observer_focus,
        );
        for spotted in &first_spotted_events {
            self.statistics
                .entry(spotted.spotter_id)
                .or_default()
                .spotted_vehicles
                .insert(spotted.target_id);
        }
        self.last_visibility
            .first_spotted_events
            .extend(first_spotted_events);
        self.last_visibility.spotted_enemies = spotted_enemies;
        self.last_visibility.minimap_frames.extend(minimap_frames);
        for (observer, changes) in aoi_changes {
            let pending = self
                .last_visibility
                .aoi_changes
                .entry(observer)
                .or_default();
            for change in changes {
                let opposite = match change {
                    AoIChange::Enter(id) => AoIChange::Leave(id),
                    AoIChange::Leave(id) => AoIChange::Enter(id),
                };
                if let Some(index) = pending.iter().position(|event| *event == opposite) {
                    pending.remove(index);
                } else {
                    pending.push(change);
                }
            }
        }
        let targets: BTreeMap<_, _> = self
            .aim_states
            .iter()
            .filter_map(|(&id, aim)| aim.auto_aim_target.map(|target| (id, target)))
            .collect();
        for (id, target) in targets {
            if let Some(reason) = self.set_auto_aim(id, target) {
                self.auto_aim_losses.push((id, reason));
            }
        }
    }

    pub(super) fn update_battle_lifecycle(&mut self, field: &[VehicleSimState], dt: f32) {
        let mut damage_interrupts = Vec::new();
        for damage in std::mem::take(&mut self.battle_damage) {
            self.record_damage(damage, &mut damage_interrupts);
        }
        let mut captured = [false; 3];
        for event in self.capture.step_capture(field, &damage_interrupts, dt) {
            let (kind, payload) = match event {
                BaseCaptureEvent::Points {
                    team,
                    base_id,
                    points,
                } => (
                    ARENA_UPDATE_BASE_POINTS,
                    Value::tuple(vec![
                        Value::Int(i64::from(team)),
                        Value::Int(base_id as i64),
                        Value::Int(i64::from(points)),
                    ]),
                ),
                BaseCaptureEvent::Captured { team } => {
                    if matches!(team, 1 | 2) {
                        captured[usize::from(3 - team)] = true;
                    }
                    (ARENA_UPDATE_BASE_CAPTURED, Value::Int(i64::from(team)))
                }
                BaseCaptureEvent::Reset {
                    attacker_id,
                    points,
                } => {
                    if let Some(stats) = self.statistics.get_mut(&attacker_id) {
                        stats.dropped_capture_points += points;
                    }
                    continue;
                }
            };
            self.arena_updates.push((kind, pickle::dumps(&payload)));
        }

        let mut roster = [false; 3];
        let mut survivors = [false; 3];
        for v in self.vehicles.iter().filter(|v| matches!(v.team, 1 | 2)) {
            let team = usize::from(v.team);
            roster[team] = true;
            survivors[team] |= v.is_alive;
        }
        let winner = |flags: [bool; 3]| match (flags[1], flags[2]) {
            (true, false) => 1,
            (false, true) => 2,
            _ => 0,
        };
        let result = if roster[1] && roster[2] && (!survivors[1] || !survivors[2]) {
            Some((winner(survivors), FinishReason::Extermination))
        } else if captured[1] || captured[2] {
            Some((winner(captured), FinishReason::Base))
        } else if self.clock.simulated_until >= self.clock.deadline {
            Some((0, FinishReason::Timeout))
        } else {
            None
        };
        if let Some((winner_team, reason)) = result {
            self.finish_round(ArenaFinished {
                winner_team,
                reason,
            });
        }
    }

    fn record_damage(&mut self, hit: BattleDamage, interrupts: &mut Vec<(i32, i32)>) {
        let Some(target) = self.vehicle_by_id(hit.target_id) else {
            return;
        };
        let target_team = target.team;
        let target_max_hp = target.max_health.max(1) as u16;
        let attacker_team = self.vehicle_by_id(hit.attacker_id).map(|v| v.team);
        let enemy = attacker_team.is_some_and(|team| team != target_team);
        if let Some(victim) = self.statistics.get_mut(&hit.target_id) {
            victim.damage_received += u32::from(hit.hp);
        }
        if hit.hp > 0 || hit.module_damage || hit.death.is_some() {
            interrupts.push((if enemy { hit.attacker_id } else { 0 }, hit.target_id));
        }
        if enemy {
            if let Some(stats) = self.statistics.get_mut(&hit.attacker_id) {
                stats.damage_dealt += u32::from(hit.hp);
                if hit.hp > 0 || hit.module_damage {
                    stats.damaged_vehicles.insert(hit.target_id);
                }
            }
        } else if attacker_team == Some(target_team)
            && self.tkill.record(
                hit.attacker_id,
                hit.target_id,
                hit.hp,
                target_max_hp,
                hit.death.is_some(),
            )
        {
            if let Some(attacker) = self
                .vehicles
                .iter_mut()
                .find(|v| v.entity_id == hit.attacker_id)
            {
                attacker.is_team_killer = true;
                self.arena_updates.push((
                    ARENA_UPDATE_TEAM_KILLER,
                    pickle::dumps(&Value::Int(i64::from(hit.attacker_id))),
                ));
            }
        }

        let Some(cause) = hit.death else {
            return;
        };
        if let Some(stats) = self.statistics.get_mut(&hit.target_id) {
            if stats.killer_id.is_some() {
                return;
            }
            stats.killer_id = Some(hit.attacker_id);
            stats.lifetime_secs = self.server_time - self.battle_started_at;
        }
        self.observer_focus.entry(hit.target_id).or_default();
        let killed = Value::tuple(vec![
            Value::Int(i64::from(hit.target_id)),
            Value::Int(i64::from(hit.attacker_id)),
            Value::Int(i64::from(cause as u8)),
        ]);
        self.arena_updates
            .push((ARENA_UPDATE_VEHICLE_KILLED, pickle::dumps(&killed)));
        if enemy {
            if let Some(stats) = self.statistics.get_mut(&hit.attacker_id) {
                stats.frags += 1;
                stats.destroyed_vehicles.insert(hit.target_id);
            }
            self.arena_updates.push((
                ARENA_UPDATE_VEHICLE_STATISTICS,
                pickle::dumps(&self.vehicle_statistics_value(hit.attacker_id)),
            ));
        }
    }
}

fn gun_steering(
    aims: &std::collections::HashMap<i32, crate::sim::AimState>,
    profiles: &std::collections::HashMap<i32, super::VehicleCombatProfile>,
    id: i32,
    yaw: f32,
    position: Vec3,
) -> f32 {
    let Some(aim) = aims
        .get(&id)
        .filter(|aim| aim.autorotation && aim.has_target_point)
    else {
        return 0.0;
    };
    let Some((min, max)) = profiles
        .get(&id)
        .and_then(|profile| profile.turret_yaw_limits)
    else {
        return 0.0;
    };
    let direction = aim.target_point - position;
    let relative = normalize_angle(direction.x.atan2(direction.z) - yaw);
    (3.0 * (relative - relative.clamp(min, max))).clamp(-1.0, 1.0)
}
