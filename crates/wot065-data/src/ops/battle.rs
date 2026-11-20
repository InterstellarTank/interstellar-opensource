use std::collections::BTreeMap;

use super::{
    get_item_price_and_shop_flag, AccountEngine, CmdError, DiffBuilder,
    VEHICLE_SETTINGS_FLAG_AUTO_EQUIP, VEHICLE_SETTINGS_FLAG_AUTO_LOAD,
    VEHICLE_SETTINGS_FLAG_AUTO_REPAIR,
};
use crate::account_state::AccountProfile;
use crate::catalog::{make_type_comp_descr, Catalog};
use crate::descr::{parse_int_comp_descr, role_name, TankmanDescr, VehicleDescr};
use crate::dossier::{
    update_dossiers, AccountDossier, DossierBattleInput, TankmanDossier, VehicleDossier,
};
use crate::pickle::{self, Value};
use crate::store::{damage_vehicle_components, VehicleComponentDamage};

pub const LOCK_REASON_ON_ARENA: i32 = 16;

#[derive(Debug, Clone, Default)]
pub struct BattleRewards {
    pub credits: u32,
    pub free_xp: u32,
}

#[derive(Debug, Clone, Default)]
pub struct PersonalBattleOutcome {
    pub arena_unique_id: u64,
    pub vehicle_inv_id: i64,
    pub repair_factor: f32,
    pub damage: VehicleComponentDamage,
    pub rewards: BattleRewards,
    pub shells_used: BTreeMap<i32, u32>,
    pub equipment_used: [bool; 3],
    pub crew_activity: Vec<bool>,
    pub dossier: DossierBattleInput,
}

#[derive(Debug)]
pub struct MaintenanceNotice {
    pub kind: u8,
    pub complete: bool,
    pub credits_spent: i64,
}

impl MaintenanceNotice {
    pub fn service_message(&self, vehicle_type: i32) -> Value {
        Value::sd(vec![
            ("type", Value::Int(10)),
            (
                "data",
                Value::sd(vec![
                    ("vehTypeCD", Value::Int(i64::from(vehicle_type))),
                    ("typeID", Value::Int(i64::from(self.kind))),
                    ("result", Value::Int(i64::from(!self.complete))),
                    (
                        "cost",
                        Value::tuple(vec![Value::Int(self.credits_spent), Value::Int(0)]),
                    ),
                ]),
            ),
        ])
    }
}

#[derive(Debug)]
pub struct BattleAccountChange {
    pub diff_pickle: Vec<u8>,
    pub repair: u32,
    pub vehicle_type: i32,
    pub maintenance: Vec<MaintenanceNotice>,
}

impl AccountEngine {
    pub fn apply_battle_outcome(
        profile: &mut AccountProfile,
        catalog: &Catalog,
        outcome: &PersonalBattleOutcome,
        now_utc: u32,
    ) -> Result<Option<BattleAccountChange>, CmdError> {
        if outcome.dossier.bonus_type != 1
            || profile.applied_battles.contains(&outcome.arena_unique_id)
        {
            return Ok(None);
        }
        let id = outcome.vehicle_inv_id;
        let previous_rev = profile.rev;
        let mut updated = profile.clone();
        let mut vehicle = updated
            .vehicles
            .remove(&id)
            .ok_or_else(|| CmdError::wrong_args("battle vehicle not found"))?;
        if vehicle.lock != LOCK_REASON_ON_ARENA {
            return Err(CmdError::failure(
                "battle vehicle is not locked for settlement",
            ));
        }
        let descr = VehicleDescr::from_bytes(&vehicle.comp_descr)
            .map_err(|e| CmdError::failure(format!("invalid battle vehicle: {e}")))?;
        let vehicle_type = make_type_comp_descr(descr.nation_id, u16::from(descr.type_id));
        let (cost, health) = {
            // Reuse the retail repair calculator on the isolated profile.
            updated.vehicles.insert(id, vehicle);
            let estimate = damage_vehicle_components(&mut updated, id, &outcome.damage, catalog)
                .map_err(CmdError::failure)?;
            vehicle = updated
                .vehicles
                .remove(&id)
                .expect("repair retains the vehicle");
            estimate
        };
        let repair = (f64::from(cost) * f64::from(outcome.repair_factor))
            .ceil()
            .max(0.0) as i32;
        if vehicle.repair.is_some() {
            vehicle.repair = Some((repair, health));
        }

        let change_time = updated.next_dossier_change_time(now_utc);
        let mut diff = DiffBuilder::new();
        let mut maintenance = Vec::new();
        updated.credits = updated
            .credits
            .saturating_add(i64::from(outcome.rewards.credits));
        updated.free_xp = updated
            .free_xp
            .saturating_add(i64::from(outcome.rewards.free_xp));
        updated.last_battle_time = now_utc;
        if outcome.dossier.xp_factor == 2 && updated.double_xp_vehs.remove(&vehicle_type) {
            diff.double_xp_vehs_r = Some(updated.double_xp_vehs.clone());
        }
        let xp = outcome.dossier.xp.min(i32::MAX as u32) as i32;
        let vehicle_xp = updated.veh_type_xp.entry(vehicle_type).or_default();
        *vehicle_xp = vehicle_xp.saturating_add(xp);
        diff.veh_type_xp.insert(vehicle_type, Some(*vehicle_xp));

        if vehicle.settings & VEHICLE_SETTINGS_FLAG_AUTO_REPAIR != 0 && vehicle.repair.is_some() {
            let complete = updated.credits >= i64::from(repair);
            if complete {
                updated.credits -= i64::from(repair);
                vehicle.repair = None;
            }
            maintenance.push(MaintenanceNotice {
                kind: 1,
                complete,
                credits_spent: if complete { i64::from(repair) } else { 0 },
            });
        }
        match vehicle.repair {
            Some(repair) => {
                diff.veh_repair.insert(id, Some(repair));
            }
            None => {
                diff.clear_veh_repair.insert(id);
            }
        }

        let turret = crate::descr::make_int_comp_descr(
            crate::descr::ITEM_TYPE_TURRET,
            descr.nation_id,
            descr.turret_id,
        );
        let gun = crate::descr::make_int_comp_descr(
            crate::descr::ITEM_TYPE_GUN,
            descr.nation_id,
            descr.gun_id,
        );
        let shell_layout = vehicle
            .get_shells_layout(turret, gun)
            .unwrap_or(&vehicle.shells)
            .to_vec();
        for (shell, count) in &mut vehicle.shells {
            let used = outcome
                .shells_used
                .get(shell)
                .copied()
                .unwrap_or(0)
                .min(i32::MAX as u32);
            *count = count.saturating_sub(used as i32).max(0);
        }
        if vehicle.settings & VEHICLE_SETTINGS_FLAG_AUTO_LOAD != 0 {
            let before = updated.credits;
            let mut needed = false;
            let mut complete = true;
            for (shell, wanted) in shell_layout {
                let existing = vehicle
                    .shells
                    .iter()
                    .find(|(cd, _)| *cd == shell)
                    .map_or(0, |(_, count)| *count);
                let missing = wanted.saturating_sub(existing);
                if missing <= 0 {
                    continue;
                }
                let received = replenish(&mut updated, &mut diff, catalog, shell, missing);
                needed = true;
                complete &= received == missing;
                if let Some((_, count)) = vehicle.shells.iter_mut().find(|(cd, _)| *cd == shell) {
                    *count += received;
                } else if received != 0 {
                    vehicle.shells.push((shell, received));
                }
            }
            if needed {
                maintenance.push(MaintenanceNotice {
                    kind: 2,
                    complete,
                    credits_spent: before - updated.credits,
                });
            }
        }
        if profile.vehicles[&id].shells != vehicle.shells {
            diff.veh_shells.insert(id, Some(vehicle.shells.clone()));
        }

        let equipment_credits = updated.credits;
        let mut equip_needed = false;
        let mut equip_complete = true;
        for (index, equipment) in vehicle.eqs.iter_mut().enumerate() {
            if outcome.equipment_used[index] {
                *equipment = 0;
            }
            let desired = vehicle.eqs_layout[index];
            if *equipment == 0
                && desired != 0
                && vehicle.settings & VEHICLE_SETTINGS_FLAG_AUTO_EQUIP != 0
            {
                equip_needed = true;
                if replenish(&mut updated, &mut diff, catalog, desired, 1) == 1 {
                    *equipment = desired;
                } else {
                    equip_complete = false;
                }
            }
        }
        if equip_needed {
            maintenance.push(MaintenanceNotice {
                kind: 3,
                complete: equip_complete,
                credits_spent: equipment_credits - updated.credits,
            });
        }
        if profile.vehicles[&id].eqs != vehicle.eqs {
            diff.veh_eqs.insert(id, Some(vehicle.eqs));
        }
        update_profile_dossiers(
            &mut updated,
            &mut diff,
            &vehicle.crew,
            catalog,
            outcome,
            vehicle_type,
            now_utc,
            change_time,
        )?;
        vehicle.lock = 0;
        diff.veh_lock(id, 0);
        diff.credits = Some(updated.credits);
        diff.free_xp = Some(updated.free_xp);
        updated.vehicles.insert(id, vehicle);
        updated.applied_battles.insert(outcome.arena_unique_id);
        let diff_pickle = pickle::dumps(&diff.build(previous_rev, updated.rev));
        *profile = updated;
        Ok(Some(BattleAccountChange {
            diff_pickle,
            repair: repair as u32,
            vehicle_type,
            maintenance,
        }))
    }
}

fn replenish(
    profile: &mut AccountProfile,
    diff: &mut DiffBuilder,
    catalog: &Catalog,
    item: i32,
    missing: i32,
) -> i32 {
    let stock = profile.items.get(&item).copied().unwrap_or(0).max(0);
    let from_depot = stock.min(missing);
    if from_depot > 0 {
        let remaining = stock - from_depot;
        if remaining == 0 {
            profile.items.remove(&item);
        } else {
            profile.items.insert(item, remaining);
        }
        let (item_type, _, _) = parse_int_comp_descr(item);
        diff.depot_items
            .entry(item_type)
            .or_default()
            .insert(item, (remaining != 0).then_some(remaining));
    }
    let still_missing = missing - from_depot;
    let Some(((price, 0), true)) = get_item_price_and_shop_flag(catalog, item) else {
        return from_depot;
    };
    let from_shop = if price > 0 {
        (profile.credits.max(0) / price).min(i64::from(still_missing)) as i32
    } else if price == 0 {
        still_missing
    } else {
        0
    };
    profile.credits -= i64::from(from_shop) * price;
    from_depot + from_shop
}

#[allow(clippy::too_many_arguments)]
fn update_profile_dossiers(
    profile: &mut AccountProfile,
    diff: &mut DiffBuilder,
    crew: &[Option<i64>],
    catalog: &Catalog,
    outcome: &PersonalBattleOutcome,
    vehicle_type: i32,
    now: u32,
    change_time: u32,
) -> Result<(), CmdError> {
    let mut account = AccountDossier::from_bytes(&profile.account_dossier)
        .map_err(|e| CmdError::failure(format!("invalid account dossier: {e}")))?;
    let mut vehicle = profile
        .vehicle_dossiers
        .get(&vehicle_type)
        .map(|(_, bytes)| VehicleDossier::from_bytes(bytes))
        .transpose()
        .map_err(|e| CmdError::failure(format!("invalid vehicle dossier: {e}")))?
        .unwrap_or_else(VehicleDossier::new);
    let mut people = Vec::new();
    let mut dossiers = Vec::new();
    for (seat, id) in crew.iter().enumerate() {
        let Some(id) = *id else { continue };
        let Some(entry) = profile.tankmen.get(&id) else {
            continue;
        };
        let mut descr = TankmanDescr::from_bytes(&entry.comp_descr)
            .map_err(|e| CmdError::failure(format!("invalid tankman: {e}")))?;
        let dossier = if descr.dossier.is_empty() {
            TankmanDossier::new()
        } else {
            TankmanDossier::from_bytes(&descr.dossier)
                .map_err(|e| CmdError::failure(format!("invalid tankman dossier: {e}")))?
        };
        let rank_count = catalog
            .tankmen_config(descr.nation_id)
            .and_then(|cfg| cfg.role_ranks.get(role_name(descr.role_id)?))
            .map_or(1, Vec::len);
        descr.add_xp(outcome.dossier.xp.min(i32::MAX as u32) as i32, rank_count);
        people.push((id, descr));
        dossiers.push((
            dossier,
            outcome.crew_activity.get(seat).copied().unwrap_or(false),
        ));
    }
    update_dossiers(
        &mut account,
        &mut vehicle,
        &mut dossiers,
        &outcome.dossier,
        vehicle_type as u32,
        catalog,
        now,
    );
    for ((id, mut descr), (dossier, _)) in people.into_iter().zip(dossiers) {
        descr.dossier = dossier.to_bytes();
        let entry = profile.tankmen.get_mut(&id).expect("existing crew member");
        entry.comp_descr = descr.to_bytes();
        entry.change_time = change_time;
        diff.tman_comp_descr
            .insert(id, Some(entry.comp_descr.clone()));
    }
    profile.account_dossier = account.to_bytes();
    profile.account_dossier_change_time = change_time;
    profile
        .vehicle_dossiers
        .insert(vehicle_type, (change_time, vehicle.to_bytes()));
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::account_state::{create_profile, Preset};
    use crate::catalog::{load_from_vehicles_dir, resolve_vehicles_dir};
    use crate::economics::EconomicsConfig;

    #[test]
    fn apply_battle_outcome_updates_xp_credits_auto_maint_and_dossiers() {
        let Some(dir) = crate::testutil::game_dir() else {
            return;
        };
        let mut catalog = load_from_vehicles_dir(&resolve_vehicles_dir(&dir).unwrap()).unwrap();
        let now = 1_700_000_000;
        let mut profile = create_profile(
            1,
            "driver",
            Preset::Retail,
            &catalog,
            &EconomicsConfig::default(),
            now,
        )
        .unwrap();
        let id = *profile.vehicles.keys().next().unwrap();
        let vehicle = profile.vehicles.get_mut(&id).unwrap();
        let (shell, count) = vehicle.shells[0];
        assert!(count >= 2);
        vehicle.settings = VEHICLE_SETTINGS_FLAG_AUTO_REPAIR | VEHICLE_SETTINGS_FLAG_AUTO_LOAD;
        vehicle.lock = 16;
        profile.items.insert(shell, 1);
        profile.credits = 0;
        let (_, nation, _) = parse_int_comp_descr(shell);
        if let Some(component) = catalog.components.get_mut(&shell) {
            component.price = (100, 0);
        } else if let Some(item) =
            catalog
                .extras
                .shop_items
                .get_mut(&(nation, 10, i64::from(shell)))
        {
            item.credits = 100;
        }
        let descr = VehicleDescr::from_bytes(&vehicle.comp_descr).unwrap();
        let type_cd = make_type_comp_descr(descr.nation_id, u16::from(descr.type_id));
        catalog.vehicle_defs.get_mut(&type_cd).unwrap().repair_cost = 1.0;
        let outcome = PersonalBattleOutcome {
            arena_unique_id: 71,
            vehicle_inv_id: id,
            repair_factor: 1.0,
            damage: VehicleComponentDamage {
                hull_hp: 1,
                ..Default::default()
            },
            rewards: BattleRewards {
                free_xp: 4,
                credits: 0,
            },
            shells_used: BTreeMap::from([(shell, 2)]),
            dossier: DossierBattleInput {
                bonus_type: 1,
                xp_factor: 1,
                xp: 80,
                winner: 1,
                life_time: 10,
                killed_types: vec![1],
                heroes: vec![1],
                ..Default::default()
            },
            ..Default::default()
        };
        let change =
            AccountEngine::apply_battle_outcome(&mut profile, &catalog, &outcome, now + 10)
                .unwrap()
                .unwrap();
        let dossier = AccountDossier::from_bytes(&profile.account_dossier).unwrap();
        assert_eq!(
            (dossier.battles_count, dossier.wins, dossier.frags),
            (1, 1, 1)
        );
        assert_eq!(dossier.veh_type_frags[&1], 1);
        assert_eq!((dossier.warrior, dossier.medal_kay), (1, 4));
        assert_eq!(profile.free_xp, 4);
        assert!(profile.credits >= 0);
        assert_eq!(profile.vehicles[&id].shells[0], (shell, count - 1));
        assert!(profile.vehicles[&id].repair.is_some());
        assert_eq!(profile.vehicles[&id].lock, 0);
        assert!(change
            .maintenance
            .iter()
            .any(|notice| notice.kind == 1 && !notice.complete));
        let rev = profile.rev;
        assert!(
            AccountEngine::apply_battle_outcome(&mut profile, &catalog, &outcome, now + 11)
                .unwrap()
                .is_none()
        );
        assert_eq!(profile.rev, rev);
    }
}
