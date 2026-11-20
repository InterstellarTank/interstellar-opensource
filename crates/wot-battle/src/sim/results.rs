use bw201::stream::BinaryWriter;
use wot065_data::account_state::AccountProfile;
use wot065_data::catalog::make_type_comp_descr;
use wot065_data::descr::VehicleDescr;
use wot065_data::dossier::DossierBattleInput;
use wot065_data::ops::battle::{BattleRewards, PersonalBattleOutcome};
use wot065_data::pickle::Value;
use wot065_data::store::VehicleComponentDamage;

use crate::lobby::BattleMode;
use crate::world::{ArenaInstance, FinishReason, VehicleState};

#[derive(Debug, Clone, Default)]
pub struct TkillVictimResult {
    pub target_id: i64,
    pub is_kill: bool,
    pub means: u8,
    pub contribution: u32,
    pub credits_penalty: u32,
    pub xp_penalty: u32,
}

/// The inline BigWorld FIXED_DICT declared in the 0.6.5 alias.xml.
#[derive(Debug, Clone, Default)]
pub struct BattleResultsExt {
    pub xp: u32,
    pub credits: u32,
    pub free_xp: u32,
    pub xp_factor: u8,
    pub repair: u32,
    pub health: i16,
    pub ammo: Vec<i32>,
    pub crew_activity: Vec<bool>,
    pub vehicle_id: i32,
    pub arena_unique_id: u64,
    pub is_winner: i8,
    pub regular: bool,
    pub killer_id: i32,
    pub killed: Vec<i32>,
    pub killed_types: Vec<u32>,
    pub damaged: Vec<i32>,
    pub spotted: Vec<i32>,
    pub shots: u16,
    pub hits: u16,
    pub damage_dealt: u32,
    pub potential_damage_dealt: u32,
    pub shots_received: u16,
    pub damage_received: u32,
    pub potential_damage_received: u32,
    pub capture_points: u16,
    pub dropped_capture_points: u16,
    pub life_time: u16,
    pub arena_type_id: u32,
    pub arena_create_time: u32,
    pub achieve_indices: Vec<u8>,
    pub hero_vehicle_ids: Vec<i32>,
    pub epic_achievements: Vec<u8>,
    pub honor_titles: Vec<u32>,
    pub tkill_rating: f32,
    pub tkill_log: Vec<TkillVictimResult>,
    pub xp_penalty: i32,
    pub credits_penalty: i32,
    pub contribution_in: i32,
    pub contribution_out: i32,
}

fn array<T: Copy>(writer: &mut BinaryWriter, items: &[T], write_item: fn(&mut BinaryWriter, T)) {
    writer.u32(items.len() as u32);
    for &item in items {
        write_item(writer, item);
    }
}

impl BattleResultsExt {
    pub fn to_wire(&self) -> Vec<u8> {
        let mut w = BinaryWriter::new();
        w.u8(1); // BATTLE_RESULTS_EXT allows None; this result is present.
        w.u32(self.xp);
        w.u32(self.credits);
        w.u32(self.free_xp);
        w.u8(self.xp_factor);
        w.u32(self.repair);
        w.i16(self.health);
        array(&mut w, &self.ammo, BinaryWriter::i32);
        array(&mut w, &self.crew_activity, |w, active| {
            w.u8(u8::from(active))
        });
        w.i32(self.vehicle_id);
        w.u64(self.arena_unique_id);
        w.u8(self.is_winner as u8);
        w.u8(u8::from(self.regular));
        w.i32(self.killer_id);
        array(&mut w, &self.killed, BinaryWriter::i32);
        array(&mut w, &self.killed_types, BinaryWriter::u32);
        array(&mut w, &self.damaged, BinaryWriter::i32);
        array(&mut w, &self.spotted, BinaryWriter::i32);
        w.u16(self.shots);
        w.u16(self.hits);
        w.u32(self.damage_dealt);
        w.u32(self.potential_damage_dealt);
        w.u16(self.shots_received);
        w.u32(self.damage_received);
        w.u32(self.potential_damage_received);
        w.u16(self.capture_points);
        w.u16(self.dropped_capture_points);
        w.u16(self.life_time);
        w.u32(self.arena_type_id);
        w.u32(self.arena_create_time);
        array(&mut w, &self.achieve_indices, BinaryWriter::u8);
        array(&mut w, &self.hero_vehicle_ids, BinaryWriter::i32);
        array(&mut w, &self.epic_achievements, BinaryWriter::u8);
        array(&mut w, &self.honor_titles, BinaryWriter::u32);
        w.f32(self.tkill_rating);
        w.u32(self.tkill_log.len() as u32);
        for record in &self.tkill_log {
            w.i64(record.target_id);
            w.u8(u8::from(record.is_kill));
            w.u8(record.means);
            w.u32(record.contribution);
            w.u32(record.credits_penalty);
            w.u32(record.xp_penalty);
        }
        w.i32(self.xp_penalty);
        w.i32(self.credits_penalty);
        w.i32(self.contribution_in);
        w.i32(self.contribution_out);
        w.as_slice().to_vec()
    }

    pub fn service_message(&self, vehicle_type: i32) -> Value {
        let records = self
            .achieve_indices
            .iter()
            .zip(&self.hero_vehicle_ids)
            .filter(|(_, id)| **id == self.vehicle_id)
            .filter(|(&code, _)| code != 0 && usize::from(code) < wot065_data::dossier::BATTLE_HERO_NAMES.len())
            .map(|(&code, _)| Value::tuple(vec![Value::Int(33 + i64::from(code)), Value::Int(1)]))
            .collect();
        Value::sd(vec![
            ("type", Value::Int(2)),
            (
                "data",
                Value::sd(vec![
                    ("arenaTypeID", Value::Int(i64::from(self.arena_type_id))),
                    (
                        "arenaCreateTime",
                        Value::Int(i64::from(self.arena_create_time)),
                    ),
                    ("vehTypeCompDescr", Value::Int(i64::from(vehicle_type))),
                    ("isWinner", Value::Int(i64::from(self.is_winner))),
                    ("xp", Value::Int(i64::from(self.xp))),
                    ("credits", Value::Int(i64::from(self.credits))),
                    ("xpFactor", Value::Int(i64::from(self.xp_factor))),
                    ("xpPenalty", Value::Int(i64::from(self.xp_penalty))),
                    (
                        "creditsPenalty",
                        Value::Int(i64::from(self.credits_penalty)),
                    ),
                    (
                        "creditsContributionIn",
                        Value::Int(i64::from(self.contribution_in)),
                    ),
                    (
                        "creditsContributionOut",
                        Value::Int(i64::from(self.contribution_out)),
                    ),
                    ("popUpRecords", Value::list(records)),
                ]),
            ),
        ])
    }
}

pub fn personal_results(
    arena: &ArenaInstance,
    vehicle: &VehicleState,
    profile: &AccountProfile,
    unique_id: u64,
    created_at: u32,
) -> Option<(BattleResultsExt, PersonalBattleOutcome)> {
    let finish = arena.finished?;
    let stats = arena.statistics.get(&vehicle.entity_id)?;
    let sim = arena.sim_states.get(&vehicle.entity_id)?;
    let aim = arena.aim_states.get(&vehicle.entity_id)?;
    let regular = arena.mode == BattleMode::Random;
    let vehicle_type = type_descr(vehicle)?;
    let is_winner = if finish.winner_team == 0 {
        0
    } else if finish.winner_team == vehicle.team {
        1
    } else {
        -1
    };
    let mut result = BattleResultsExt {
        health: vehicle.health,
        ammo: aim
            .ammo_counts
            .iter()
            .flat_map(|&(cd, count)| [cd, count.min(i32::MAX as u32) as i32])
            .collect(),
        crew_activity: sim
            .damage
            .tankmen
            .iter()
            .map(|t| t.is_alive && vehicle.is_crew_active)
            .collect(),
        vehicle_id: vehicle.entity_id,
        arena_unique_id: unique_id,
        is_winner,
        regular,
        killer_id: stats.killer_id.unwrap_or(0),
        killed: stats.destroyed_vehicles.iter().copied().collect(),
        killed_types: stats
            .destroyed_vehicles
            .iter()
            .filter_map(|&id| {
                arena
                    .vehicle_by_id(id)
                    .and_then(type_descr)
                    .map(|cd| cd as u32)
            })
            .collect(),
        damaged: stats.damaged_vehicles.iter().copied().collect(),
        spotted: stats.spotted_vehicles.iter().copied().collect(),
        shots: stats.shots.min(u32::from(u16::MAX)) as u16,
        hits: stats.hits.min(u32::from(u16::MAX)) as u16,
        shots_received: stats.shots_received.min(u32::from(u16::MAX)) as u16,
        damage_dealt: stats.damage_dealt,
        potential_damage_dealt: stats.potential_damage_dealt,
        damage_received: stats.damage_received,
        potential_damage_received: stats.potential_damage_received,
        capture_points: stats.capture_points.floor() as u16,
        dropped_capture_points: stats.dropped_capture_points.floor() as u16,
        life_time: stats.lifetime_secs as u16,
        arena_type_id: arena.map.def.id as u32,
        arena_create_time: created_at,
        ..Default::default()
    };

    if regular {
        let (hero_codes, hero_ids) = battle_heroes(arena);
        result.achieve_indices = hero_codes;
        result.hero_vehicle_ids = hero_ids;
        let win = if is_winner == 1 { 1.5 } else { 1.0 };
        let premium = if profile.is_premium() && profile.premium_expiry_time > created_at {
            1.5
        } else {
            1.0
        };
        result.xp_factor = if is_winner == 1 && profile.double_xp_vehs.contains(&vehicle_type) {
            2
        } else {
            1
        };
        let (xp, credits) = base_rewards(&result, vehicle.is_alive);
        if let Some(offenses) = arena.tkill.offenses(vehicle.entity_id) {
            result.tkill_rating = offenses.rating;
            for (&target, offense) in &offenses.victims {
                let Some(victim) = arena.vehicle_by_id(target) else {
                    continue;
                };
                let contribution = (offense.damage as f32
                    * arena.balance.tkill_credits_penalty_factor)
                    .round() as u32;
                let extra_credits = contribution / 2;
                let xp_penalty = offense.damage / 4;
                result.contribution_out = result
                    .contribution_out
                    .saturating_add(contribution.min(i32::MAX as u32) as i32);
                result.credits_penalty = result
                    .credits_penalty
                    .saturating_add(extra_credits.min(i32::MAX as u32) as i32);
                result.xp_penalty = result.xp_penalty.saturating_add(xp_penalty as i32);
                result.tkill_log.push(TkillVictimResult {
                    target_id: victim.account_db_id,
                    is_kill: offense.destroyed,
                    means: 0,
                    contribution,
                    credits_penalty: extra_credits,
                    xp_penalty,
                });
            }
        }
        result.contribution_in = (arena.tkill.compensation_damage(vehicle.entity_id) as f32
            * arena.balance.tkill_credits_penalty_factor)
            .round() as i32;
        result.xp = ((xp * win * premium * arena.balance.xp_factor).round() as u32)
            .saturating_sub(result.xp_penalty as u32)
            .saturating_mul(u32::from(result.xp_factor));
        result.credits = ((credits * win * premium * arena.balance.credits_factor).round() as u32)
            .saturating_sub(result.credits_penalty as u32)
            .saturating_sub(result.contribution_out as u32)
            .saturating_add(result.contribution_in.max(0) as u32);
        result.free_xp = (result.xp as f32 * arena.balance.free_xp_factor).round() as u32;
    }
    let damage = &sim.damage;
    let outcome = PersonalBattleOutcome {
        arena_unique_id: unique_id,
        vehicle_inv_id: vehicle.veh_inv_id,
        repair_factor: arena.balance.repair_cost_factor,
        damage: VehicleComponentDamage {
            hull_hp: i32::from(vehicle.health),
            engine_hp: Some(damage.engine.health),
            fuel_tank_hp: Some(damage.fuel_tank.health),
            radio_hp: Some(damage.radio.health),
            ammo_bay_hp: Some(damage.ammo_bay.health),
            left_track_hp: Some(damage.left_track.health),
            right_track_hp: Some(damage.right_track.health),
            turret_rotator_hp: Some(damage.turret_rotator.health),
            surveying_device_hp: Some(damage.surveying_device.health),
            gun_hp: Some(damage.gun.health),
        },
        rewards: BattleRewards {
            credits: result.credits,
            free_xp: result.free_xp,
        },
        shells_used: stats.shells_used.clone(),
        equipment_used: std::array::from_fn(|index| {
            damage.equipment_slots[index].as_ref().is_some_and(|slot| {
                slot.count == 0
                    || matches!(
                        slot.name.as_str(),
                        "chocolate"
                            | "cocacola"
                            | "lendLeaseOil"
                            | "gasoline100"
                            | "gasoline105"
                            | "ration"
                    )
            })
        }),
        crew_activity: result.crew_activity.clone(),
        dossier: DossierBattleInput {
            bonus_type: arena.arena_bonus_type,
            xp_factor: result.xp_factor,
            xp: result.xp,
            winner: is_winner,
            killer_id: result.killer_id,
            life_time: result.life_time,
            shots: result.shots,
            hits: result.hits,
            spotted: result.spotted.len() as u32,
            damage_dealt: result.damage_dealt,
            damage_received: result.damage_received,
            capture_points: result.capture_points,
            dropped_capture_points: result.dropped_capture_points,
            killed_types: result.killed_types.clone(),
            heroes: result
                .achieve_indices
                .iter()
                .zip(&result.hero_vehicle_ids)
                .filter_map(|(&code, &id)| (id == vehicle.entity_id).then_some(code))
                .collect(),
        },
    };
    Some((result, outcome))
}

fn type_descr(vehicle: &VehicleState) -> Option<i32> {
    let descr = VehicleDescr::from_bytes(&vehicle.veh_comp_descr).ok()?;
    Some(make_type_comp_descr(
        descr.nation_id,
        u16::from(descr.type_id),
    ))
}

fn base_rewards(result: &BattleResultsExt, survived: bool) -> (f32, f32) {
    let spots = result.spotted.len() as f32;
    let kills = result.killed.len() as f32;
    let damage = result.damage_dealt as f32;
    let objectives = f32::from(result.capture_points) + f32::from(result.dropped_capture_points);
    let survival = if survived { 25.0 } else { 0.0 };
    (
        50.0 + damage * 0.2 + spots * 25.0 + kills * 40.0 + objectives * 2.0 + survival,
        300.0 + damage * 5.0 + spots * 100.0 + kills * 150.0 + objectives * 4.0,
    )
}

fn battle_heroes(arena: &ArenaInstance) -> (Vec<u8>, Vec<i32>) {
    let Some(finish) = arena.finished else {
        return (Vec::new(), Vec::new());
    };
    let mut best = [None; 7];
    for vehicle in &arena.vehicles {
        let Some(stat) = arena.statistics.get(&vehicle.entity_id) else {
            continue;
        };
        let won = vehicle.team == finish.winner_team;
        let scores = [
            (stat.frags >= 6).then_some(stat.frags),
            (won && finish.reason == FinishReason::Base && stat.capture_points >= 80.0)
                .then_some(stat.capture_points as u32),
            (stat.shots >= 10
                && stat.hits * 100 >= stat.shots * 85
                && stat.potential_damage_dealt >= 1000
                && arena.tkill.offenses(vehicle.entity_id).is_none())
            .then_some(stat.hits * 1000 / stat.shots.max(1)),
            (stat.dropped_capture_points >= 70.0).then_some(stat.dropped_capture_points as u32),
            (vehicle.is_alive
                && stat.shots_received >= 11
                && stat.potential_damage_received >= 1000)
                .then_some(stat.potential_damage_received),
            (stat.damaged_vehicles.len() >= 6).then_some(stat.damaged_vehicles.len() as u32),
            (won && stat.spotted_vehicles.len() >= 9).then_some(stat.spotted_vehicles.len() as u32),
        ];
        for (best, score) in best.iter_mut().zip(scores) {
            if let Some(score) = score {
                let candidate = (score, std::cmp::Reverse(vehicle.entity_id));
                if best.is_none_or(|prev| candidate > prev) {
                    *best = Some(candidate);
                }
            }
        }
    }
    best.into_iter()
        .enumerate()
        .filter_map(|(code, entry)| entry.map(|(_, id)| (code as u8 + 1, id.0)))
        .unzip()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn battle_results_ext_and_sys_message_encode_all_fields() {
        let result = BattleResultsExt {
            xp: 1,
            credits: 2,
            free_xp: 3,
            xp_factor: 4,
            repair: 5,
            health: -6,
            ammo: vec![7],
            crew_activity: vec![true],
            vehicle_id: 8,
            arena_unique_id: 0x0102_0304_0506_0708,
            is_winner: -1,
            regular: true,
            killer_id: 9,
            killed: vec![10],
            killed_types: vec![11],
            damaged: vec![12],
            spotted: vec![13],
            shots: 14,
            hits: 15,
            damage_dealt: 16,
            potential_damage_dealt: 17,
            shots_received: 18,
            damage_received: 19,
            potential_damage_received: 20,
            capture_points: 21,
            dropped_capture_points: 22,
            life_time: 23,
            arena_type_id: 24,
            arena_create_time: 25,
            achieve_indices: vec![26],
            hero_vehicle_ids: vec![27],
            epic_achievements: vec![28],
            honor_titles: vec![29],
            tkill_rating: 1.25,
            tkill_log: vec![TkillVictimResult {
                target_id: 30,
                is_kill: true,
                means: 31,
                contribution: 32,
                credits_penalty: 33,
                xp_penalty: 34,
            }],
            xp_penalty: 35,
            credits_penalty: 36,
            contribution_in: 37,
            contribution_out: 38,
        };
        let hex = concat!(
            "01 01000000 02000000 03000000 04 05000000 faff ",
            "01000000 07000000 01000000 01 08000000 0807060504030201 ff 01 09000000 ",
            "01000000 0a000000 01000000 0b000000 01000000 0c000000 01000000 0d000000 ",
            "0e00 0f00 10000000 11000000 1200 13000000 14000000 1500 1600 1700 18000000 19000000 ",
            "01000000 1a 01000000 1b000000 01000000 1c 01000000 1d000000 0000a03f ",
            "01000000 1e00000000000000 01 1f 20000000 21000000 22000000 ",
            "23000000 24000000 25000000 26000000",
        );
        let expected: Vec<u8> = hex
            .split_whitespace()
            .flat_map(|word| {
                (0..word.len())
                    .step_by(2)
                    .map(|ix| u8::from_str_radix(&word[ix..ix + 2], 16).unwrap())
            })
            .collect();
        assert_eq!(result.to_wire(), expected);
        let message = result.service_message(257);
        assert_eq!(message.get("type").and_then(Value::as_int), Some(2));
        let data = message.get("data").unwrap();
        assert_eq!(
            data.get("vehTypeCompDescr").and_then(Value::as_int),
            Some(257)
        );
        assert_eq!(data.get("isWinner").and_then(Value::as_int), Some(-1));
        assert_eq!(
            data.get("creditsContributionOut").and_then(Value::as_int),
            Some(38)
        );
    }
}
