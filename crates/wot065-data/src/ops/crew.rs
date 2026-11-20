//! Tankman recruitment, assignment, dismissal, skills, respecialization, and passport customization.

use crate::account_state::{AccountProfile, TankmanEntry};
use crate::catalog::Catalog;
use crate::descr::{
    role_name, skill_name_by_id, TankmanDescr, VehicleDescr,
};
use crate::dossier::TankmanDossier;
use crate::economics::EconomicsConfig;
use crate::ops::{
    barracks_used, require_shop_rev, require_vehicle_not_broken, CmdError, CmdOutcome,
    DiffBuilder, RES_SUCCESS,
};
use crate::pickle::Value;

/// Recruit a new tankman (`CMD_BUY_TMAN = 303`).
pub fn buy_tankman(
    profile: &mut AccountProfile,
    shop_rev: i64,
    veh_type_cd: i32,
    role_idx: u8,
    tman_cost_idx: usize,
    catalog: &Catalog,
    economics: &EconomicsConfig,
    server_shop_rev: i64,
    now_utc: u32,
) -> Result<CmdOutcome, CmdError> {
    require_shop_rev(shop_rev, server_shop_rev)?;

    let veh_def = catalog
        .vehicle_def(veh_type_cd)
        .ok_or_else(|| CmdError::failure(format!("vehicle {veh_type_cd} not found in catalog")))?;

    let role_s = role_name(role_idx)
        .ok_or_else(|| CmdError::wrong_args(format!("invalid role id {role_idx}")))?;
    if !veh_def.crew.iter().any(|c| c.primary_role == role_s) {
        return Err(CmdError::failure(format!(
            "role {role_s} is not part of vehicle {} crew",
            veh_def.name
        )));
    }

    if tman_cost_idx >= economics.tankman_cost.len() {
        return Err(CmdError::wrong_args(format!(
            "invalid tankman cost tier index {tman_cost_idx}"
        )));
    }
    let tier = &economics.tankman_cost[tman_cost_idx];
    let credits_cost = tier.credits as i64;
    let gold_cost = tier.gold as i64;

    let unseated = barracks_used(profile);
    if unseated >= (profile.berths as usize) {
        return Err(CmdError::failure(format!(
            "barracks is full ({unseated}/{} berths occupied)",
            profile.berths
        )));
    }

    if profile.credits < credits_cost {
        return Err(CmdError::failure(format!(
            "insufficient credits for tankman: have {}, need {credits_cost}",
            profile.credits
        )));
    }
    if profile.gold < gold_cost {
        return Err(CmdError::failure(format!(
            "insufficient gold for tankman: have {}, need {gold_cost}",
            profile.gold
        )));
    }

    let seed = profile.next_inv_id as usize;
    let cfg = catalog.tankmen_config(veh_def.nation_id);
    let (fn_id, ln_id, ic_id) = if let Some(c) = cfg {
        if let Some(group) = c.normal_groups.iter().find(|g| {
            !g.is_female
                && !g.first_names.is_empty()
                && !g.last_names.is_empty()
                && !g.icons.is_empty()
        }) {
            let f = group.first_names[seed % group.first_names.len()];
            let l = group.last_names[seed % group.last_names.len()];
            let i = group.icons[seed % group.icons.len()];
            (f, l, i)
        } else {
            (1, 1, 1)
        }
    } else {
        (1, 1, 1)
    };

    let dossier = TankmanDossier::new();
    let descr = TankmanDescr {
        nation_id: veh_def.nation_id,
        vehicle_type_id: veh_def.type_id as u8,
        role_id: role_idx,
        role_level: tier.role_level,
        skills: Vec::new(),
        last_skill_level: 0,
        is_female: false,
        is_premium: tier.is_premium,
        first_name_id: fn_id,
        last_name_id: ln_id,
        icon_id: ic_id,
        rank_idx: 0,
        num_levels_to_next_rank: 50,
        free_xp: 0,
        dossier: dossier.to_bytes(),
    };

    let descr_bytes = descr.to_bytes();
    let change_time = profile.next_dossier_change_time(now_utc);
    let tman_inv_id = profile.next_inv_id;
    profile.next_inv_id += 1;

    let prev_rev = profile.rev;
    profile.credits -= credits_cost;
    profile.gold -= gold_cost;

    profile.tankmen.insert(
        tman_inv_id,
        TankmanEntry {
            comp_descr: descr_bytes.clone(),
            vehicle_inv_id: None,
            change_time,
        },
    );

    let mut diff = DiffBuilder::new();
    if credits_cost > 0 {
        diff.credits = Some(profile.credits);
    }
    if gold_cost > 0 {
        diff.gold = Some(profile.gold);
    }
    diff.tman_comp_descr
        .insert(tman_inv_id, Some(descr_bytes.clone()));
    diff.tman_vehicle.insert(tman_inv_id, None);

    let ext = Value::dict(vec![
        (Value::str("tmanInvID"), Value::Int(tman_inv_id)),
        (Value::str("tmanCompDescr"), Value::Str(descr_bytes)),
    ]);

    profile.rev += 1;
    Ok(CmdOutcome {
        result_id: RES_SUCCESS,
        prev_rev,
        rev: profile.rev,
        diff: diff.build(prev_rev, profile.rev),
        ext: Some(ext),
        resync_dossiers: true,
    })
}

/// Equip, seat, unload, or swap tankman (`CMD_EQUIP_TMAN = 105`).
pub fn equip_tankman(
    profile: &mut AccountProfile,
    veh_inv_id: i64,
    slot: usize,
    tman_inv_id: i64,
    catalog: &Catalog,
    _economics: &EconomicsConfig,
    _server_shop_rev: i64,
    _now_utc: u32,
) -> Result<CmdOutcome, CmdError> {
    let (veh_descr, old_tman_id) = {
        let veh_entry = profile
            .vehicles
            .get(&veh_inv_id)
            .ok_or_else(|| CmdError::failure(format!("vehicle {veh_inv_id} not found")))?;
        require_vehicle_not_broken(veh_entry)?;
        if slot >= veh_entry.crew.len() {
            return Err(CmdError::wrong_args(format!(
                "slot index {slot} out of bounds (vehicle crew size is {})",
                veh_entry.crew.len()
            )));
        }
        let descr = VehicleDescr::from_bytes(&veh_entry.comp_descr)
            .map_err(|e| CmdError::failure(format!("invalid vehicle descriptor: {e:?}")))?;
        (descr, veh_entry.crew[slot])
    };

    let veh_def = catalog
        .vehicle_def_by_id(veh_descr.nation_id, veh_descr.type_id as u16)
        .ok_or_else(|| CmdError::failure("vehicle definition not found"))?;

    if tman_inv_id < 0 {
        // Unload occupant into barracks
        let old_id = old_tman_id
            .ok_or_else(|| CmdError::failure(format!("slot {slot} is already empty")))?;

        let unseated = barracks_used(profile);
        if unseated >= (profile.berths as usize) {
            return Err(CmdError::failure(format!(
                "barracks is full ({unseated}/{} berths occupied)",
                profile.berths
            )));
        }

        let prev_rev = profile.rev;
        let mut diff = DiffBuilder::new();

        profile.vehicles.get_mut(&veh_inv_id).unwrap().crew[slot] = None;
        profile.tankmen.get_mut(&old_id).unwrap().vehicle_inv_id = None;

        diff.veh_crew
            .insert(veh_inv_id, Some(profile.vehicles[&veh_inv_id].crew.clone()));
        diff.tman_vehicle.insert(old_id, None);

        profile.rev += 1;
        Ok(CmdOutcome {
            result_id: RES_SUCCESS,
            prev_rev,
            rev: profile.rev,
            diff: diff.build(prev_rev, profile.rev),
            ext: None,
            resync_dossiers: false,
        })
    } else {
        // Seat or swap tankman
        let (src_veh_id_opt, tman_nation, tman_role) = {
            let tman_entry = profile
                .tankmen
                .get(&tman_inv_id)
                .ok_or_else(|| CmdError::failure(format!("tankman {tman_inv_id} not found")))?;
            let tman_descr = TankmanDescr::from_bytes(&tman_entry.comp_descr)
                .map_err(|e| CmdError::failure(format!("invalid tankman descriptor: {e:?}")))?;
            (tman_entry.vehicle_inv_id, tman_descr.nation_id, tman_descr.role_id)
        };

        if tman_nation != veh_descr.nation_id {
            return Err(CmdError::failure("tankman nation does not match vehicle"));
        }

        let expected_role_id = veh_def.crew[slot]
            .primary_role_id()
            .map_err(|e| CmdError::failure(e))?;
        if tman_role != expected_role_id {
            return Err(CmdError::failure(format!(
                "tankman role {tman_role} does not match required primary role {expected_role_id} for slot {slot}"
            )));
        }

        // Check source vehicle if seated elsewhere
        if let Some(src_veh_id) = src_veh_id_opt {
            if src_veh_id != veh_inv_id {
                let src_veh = profile.vehicles.get(&src_veh_id).ok_or_else(|| {
                    CmdError::failure(format!("source vehicle {src_veh_id} not found"))
                })?;
                require_vehicle_not_broken(src_veh)?;
            }
        }

        // Net barracks delta check
        let enters_barracks = if old_tman_id.is_some() && old_tman_id != Some(tman_inv_id) {
            1i32
        } else {
            0i32
        };
        let leaves_barracks = if src_veh_id_opt.is_none() {
            1i32
        } else {
            0i32
        };
        let delta = enters_barracks - leaves_barracks;
        let cur_unseated = barracks_used(profile) as i32;
        if cur_unseated + delta > profile.berths {
            return Err(CmdError::failure(format!(
                "barracks is full: unseated {cur_unseated} + delta {delta} > berths {}",
                profile.berths
            )));
        }

        let prev_rev = profile.rev;
        let mut diff = DiffBuilder::new();

        // 1. Clear previous seat if tankman was seated somewhere
        if let Some(src_veh_id) = src_veh_id_opt {
            if src_veh_id == veh_inv_id {
                let veh = profile.vehicles.get_mut(&veh_inv_id).unwrap();
                if let Some(old_slot) = veh.crew.iter().position(|&id| id == Some(tman_inv_id)) {
                    veh.crew[old_slot] = None;
                }
            } else {
                let src_veh = profile.vehicles.get_mut(&src_veh_id).unwrap();
                if let Some(old_slot) = src_veh.crew.iter().position(|&id| id == Some(tman_inv_id)) {
                    src_veh.crew[old_slot] = None;
                }
                diff.veh_crew
                    .insert(src_veh_id, Some(src_veh.crew.clone()));
            }
        }

        // 2. Unseat old occupant if slot was occupied and not the same tankman
        if let Some(old_id) = old_tman_id {
            if old_id != tman_inv_id {
                profile.tankmen.get_mut(&old_id).unwrap().vehicle_inv_id = None;
                diff.tman_vehicle.insert(old_id, None);
            }
        }

        // 3. Bind new tankman to target vehicle and slot
        let veh = profile.vehicles.get_mut(&veh_inv_id).unwrap();
        veh.crew[slot] = Some(tman_inv_id);
        profile
            .tankmen
            .get_mut(&tman_inv_id)
            .unwrap()
            .vehicle_inv_id = Some(veh_inv_id);

        diff.veh_crew
            .insert(veh_inv_id, Some(veh.crew.clone()));
        diff.tman_vehicle.insert(tman_inv_id, Some(veh_inv_id));

        profile.rev += 1;
        Ok(CmdOutcome {
            result_id: RES_SUCCESS,
            prev_rev,
            rev: profile.rev,
            diff: diff.build(prev_rev, profile.rev),
            ext: None,
            resync_dossiers: false,
        })
    }
}

/// Dismiss tankman (`CMD_DISMISS_TMAN = 306`).
pub fn dismiss_tankman(
    profile: &mut AccountProfile,
    tman_inv_id: i64,
    _catalog: &Catalog,
    _economics: &EconomicsConfig,
    _server_shop_rev: i64,
    _now_utc: u32,
) -> Result<CmdOutcome, CmdError> {
    let veh_id_opt = {
        let tman_entry = profile
            .tankmen
            .get(&tman_inv_id)
            .ok_or_else(|| CmdError::failure(format!("tankman {tman_inv_id} not found")))?;
        tman_entry.vehicle_inv_id
    };

    let prev_rev = profile.rev;
    let mut diff = DiffBuilder::new();

    if let Some(veh_id) = veh_id_opt {
        if let Some(veh) = profile.vehicles.get_mut(&veh_id) {
            require_vehicle_not_broken(veh)?;
            if let Some(pos) = veh.crew.iter().position(|&id| id == Some(tman_inv_id)) {
                veh.crew[pos] = None;
                diff.veh_crew.insert(veh_id, Some(veh.crew.clone()));
            }
        }
    }

    profile.tankmen.remove(&tman_inv_id);
    diff.delete_tankmen.insert(tman_inv_id);

    profile.rev += 1;
    Ok(CmdOutcome {
        result_id: RES_SUCCESS,
        prev_rev,
        rev: profile.rev,
        diff: diff.build(prev_rev, profile.rev),
        ext: None,
        resync_dossiers: false,
    })
}

/// Respecialize tankman for a vehicle (`CMD_TMAN_RESPEC = 153`).
pub fn tman_respec(
    profile: &mut AccountProfile,
    shop_rev: i64,
    tman_inv_id: i64,
    tman_cost_idx: usize,
    veh_type_cd: i32,
    catalog: &Catalog,
    economics: &EconomicsConfig,
    server_shop_rev: i64,
    _now_utc: u32,
) -> Result<CmdOutcome, CmdError> {
    require_shop_rev(shop_rev, server_shop_rev)?;

    let (veh_id_opt, mut tman_descr) = {
        let tman_entry = profile
            .tankmen
            .get(&tman_inv_id)
            .ok_or_else(|| CmdError::failure(format!("tankman {tman_inv_id} not found")))?;
        let descr = TankmanDescr::from_bytes(&tman_entry.comp_descr)
            .map_err(|e| CmdError::failure(format!("invalid tankman descriptor: {e:?}")))?;
        (tman_entry.vehicle_inv_id, descr)
    };

    if let Some(veh_id) = veh_id_opt {
        if let Some(veh) = profile.vehicles.get(&veh_id) {
            require_vehicle_not_broken(veh)?;
        }
    }

    if tman_cost_idx >= economics.tankman_cost.len() {
        return Err(CmdError::wrong_args(format!(
            "invalid cost tier index {tman_cost_idx}"
        )));
    }
    let tier = &economics.tankman_cost[tman_cost_idx];
    let credits_cost = tier.credits as i64;
    let gold_cost = tier.gold as i64;

    if profile.credits < credits_cost {
        return Err(CmdError::failure(format!(
            "insufficient credits for respec: have {}, need {credits_cost}",
            profile.credits
        )));
    }
    if profile.gold < gold_cost {
        return Err(CmdError::failure(format!(
            "insufficient gold for respec: have {}, need {gold_cost}",
            profile.gold
        )));
    }

    let target_innation_id = if veh_type_cd == 0 {
        tman_descr.vehicle_type_id
    } else {
        let veh_def = catalog
            .vehicle_def(veh_type_cd)
            .ok_or_else(|| CmdError::failure("target vehicle definition not found"))?;
        if veh_def.nation_id != tman_descr.nation_id {
            return Err(CmdError::failure(
                "cannot respecialize tankman to vehicle of a different nation",
            ));
        }
        let role_s = role_name(tman_descr.role_id).unwrap();
        if !veh_def.crew.iter().any(|c| c.primary_role == role_s) {
            return Err(CmdError::failure(format!(
                "tankman role {role_s} is not part of target vehicle {} crew",
                veh_def.name
            )));
        }
        veh_def.type_id as u8
    };

    let cur_veh_def = catalog
        .vehicle_def_by_id(tman_descr.nation_id, tman_descr.vehicle_type_id as u16)
        .ok_or_else(|| CmdError::failure("current vehicle definition not found"))?;
    let new_veh_def = catalog
        .vehicle_def_by_id(tman_descr.nation_id, target_innation_id as u16)
        .ok_or_else(|| CmdError::failure("new vehicle definition not found"))?;

    let role_s = role_name(tman_descr.role_id).unwrap();
    let cfg = catalog
        .tankmen_config(tman_descr.nation_id)
        .ok_or_else(|| CmdError::failure("tankmen config not found"))?;
    let role_ranks_len = cfg
        .role_ranks
        .get(role_s)
        .map(|r| r.len())
        .unwrap_or(0);

    tman_descr.respecialize(
        target_innation_id,
        tier.role_level,
        tier.base_role_loss,
        tier.class_change_role_loss,
        tier.is_premium,
        &cur_veh_def.tags,
        &new_veh_def.tags,
        role_ranks_len,
    );

    let prev_rev = profile.rev;
    profile.credits -= credits_cost;
    profile.gold -= gold_cost;

    let descr_bytes = tman_descr.to_bytes();
    let tman = profile.tankmen.get_mut(&tman_inv_id).unwrap();
    tman.comp_descr = descr_bytes.clone();

    let mut diff = DiffBuilder::new();
    if credits_cost > 0 {
        diff.credits = Some(profile.credits);
    }
    if gold_cost > 0 {
        diff.gold = Some(profile.gold);
    }
    diff.tman_comp_descr
        .insert(tman_inv_id, Some(descr_bytes));

    profile.rev += 1;
    Ok(CmdOutcome {
        result_id: RES_SUCCESS,
        prev_rev,
        rev: profile.rev,
        diff: diff.build(prev_rev, profile.rev),
        ext: None,
        resync_dossiers: false,
    })
}

/// Add skill to tankman (`CMD_TMAN_ADD_SKILL = 151`).
pub fn tman_add_skill(
    profile: &mut AccountProfile,
    tman_inv_id: i64,
    skill_idx: u8,
    catalog: &Catalog,
    _economics: &EconomicsConfig,
    _server_shop_rev: i64,
    _now_utc: u32,
) -> Result<CmdOutcome, CmdError> {
    let skill_s = skill_name_by_id(skill_idx)
        .ok_or_else(|| CmdError::wrong_args(format!("invalid skill index {skill_idx}")))?;

    let mut tman_descr = {
        let tman_entry = profile
            .tankmen
            .get(&tman_inv_id)
            .ok_or_else(|| CmdError::failure(format!("tankman {tman_inv_id} not found")))?;
        TankmanDescr::from_bytes(&tman_entry.comp_descr)
            .map_err(|e| CmdError::failure(format!("invalid tankman descriptor: {e:?}")))?
    };

    let role_s = role_name(tman_descr.role_id).unwrap();
    let cfg = catalog
        .tankmen_config(tman_descr.nation_id)
        .ok_or_else(|| CmdError::failure("tankmen config not found"))?;
    let role_ranks_len = cfg
        .role_ranks
        .get(role_s)
        .map(|r| r.len())
        .unwrap_or(0);

    tman_descr
        .add_skill(skill_s, role_ranks_len)
        .map_err(|e| CmdError::failure(format!("cannot add skill: {e:?}")))?;

    let prev_rev = profile.rev;
    let descr_bytes = tman_descr.to_bytes();
    let tman = profile.tankmen.get_mut(&tman_inv_id).unwrap();
    tman.comp_descr = descr_bytes.clone();

    let mut diff = DiffBuilder::new();
    diff.tman_comp_descr
        .insert(tman_inv_id, Some(descr_bytes));

    profile.rev += 1;
    Ok(CmdOutcome {
        result_id: RES_SUCCESS,
        prev_rev,
        rev: profile.rev,
        diff: diff.build(prev_rev, profile.rev),
        ext: None,
        resync_dossiers: false,
    })
}

/// Drop skill from tankman (`CMD_TMAN_DROP_SKILL = 152`).
pub fn tman_drop_skill(
    profile: &mut AccountProfile,
    tman_inv_id: i64,
    skill_idx: u8,
    _catalog: &Catalog,
    _economics: &EconomicsConfig,
    _server_shop_rev: i64,
    _now_utc: u32,
) -> Result<CmdOutcome, CmdError> {
    let skill_s = skill_name_by_id(skill_idx)
        .ok_or_else(|| CmdError::wrong_args(format!("invalid skill index {skill_idx}")))?;

    let mut tman_descr = {
        let tman_entry = profile
            .tankmen
            .get(&tman_inv_id)
            .ok_or_else(|| CmdError::failure(format!("tankman {tman_inv_id} not found")))?;
        TankmanDescr::from_bytes(&tman_entry.comp_descr)
            .map_err(|e| CmdError::failure(format!("invalid tankman descriptor: {e:?}")))?
    };

    tman_descr
        .drop_skill(skill_s)
        .map_err(|e| CmdError::failure(format!("cannot drop skill: {e:?}")))?;

    let prev_rev = profile.rev;
    let descr_bytes = tman_descr.to_bytes();
    let tman = profile.tankmen.get_mut(&tman_inv_id).unwrap();
    tman.comp_descr = descr_bytes.clone();

    let mut diff = DiffBuilder::new();
    diff.tman_comp_descr
        .insert(tman_inv_id, Some(descr_bytes));

    profile.rev += 1;
    Ok(CmdOutcome {
        result_id: RES_SUCCESS,
        prev_rev,
        rev: profile.rev,
        diff: diff.build(prev_rev, profile.rev),
        ext: None,
        resync_dossiers: false,
    })
}

/// Replace tankman passport (`CMD_TMAN_PASSPORT = 154`).
pub fn tman_passport(
    profile: &mut AccountProfile,
    shop_rev: i64,
    tman_inv_id: i64,
    is_female_flag: i32,
    first_name_id: i32,
    last_name_id: i32,
    icon_id: i32,
    catalog: &Catalog,
    economics: &EconomicsConfig,
    server_shop_rev: i64,
    _now_utc: u32,
) -> Result<CmdOutcome, CmdError> {
    require_shop_rev(shop_rev, server_shop_rev)?;

    let cost = economics.passport_change_cost as i64;
    if profile.gold < cost {
        return Err(CmdError::failure(format!(
            "insufficient gold for passport change: have {}, need {cost}",
            profile.gold
        )));
    }

    let mut tman_descr = {
        let tman_entry = profile
            .tankmen
            .get(&tman_inv_id)
            .ok_or_else(|| CmdError::failure(format!("tankman {tman_inv_id} not found")))?;
        TankmanDescr::from_bytes(&tman_entry.comp_descr)
            .map_err(|e| CmdError::failure(format!("invalid tankman descriptor: {e:?}")))?
    };

    let is_female = if is_female_flag == -1 {
        None
    } else {
        Some(is_female_flag != 0)
    };
    let fn_id = if first_name_id == -1 {
        None
    } else {
        Some(first_name_id as u16)
    };
    let ln_id = if last_name_id == -1 {
        None
    } else {
        Some(last_name_id as u16)
    };
    let ic_id = if icon_id == -1 {
        None
    } else {
        Some(icon_id as u16)
    };

    let cfg = catalog
        .tankmen_config(tman_descr.nation_id)
        .ok_or_else(|| CmdError::failure("tankmen config not found"))?;

    tman_descr
        .replace_passport(is_female, fn_id, ln_id, ic_id, cfg)
        .map_err(|e| CmdError::failure(format!("invalid passport combination: {e:?}")))?;

    let prev_rev = profile.rev;
    profile.gold -= cost;

    let descr_bytes = tman_descr.to_bytes();
    let tman = profile.tankmen.get_mut(&tman_inv_id).unwrap();
    tman.comp_descr = descr_bytes.clone();

    let mut diff = DiffBuilder::new();
    diff.gold = Some(profile.gold);
    diff.tman_comp_descr
        .insert(tman_inv_id, Some(descr_bytes));

    profile.rev += 1;
    Ok(CmdOutcome {
        result_id: RES_SUCCESS,
        prev_rev,
        rev: profile.rev,
        diff: diff.build(prev_rev, profile.rev),
        ext: None,
        resync_dossiers: false,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::account_state::{AccountProfile, Preset};
    use crate::descr::{make_int_comp_descr, TankmanDescr, VehicleDescr};
    use crate::economics::EconomicsConfig;
    use crate::ops::{AccountEngine, EngineCmd, RES_FAILURE, RES_SUCCESS};
    use crate::pickle::Value;
    use crate::testutil;

    const SERVER_SHOP_REV: i64 = 42;
    const NOW_UTC: u32 = 1_700_000_000;

    fn dict_get<'a>(val: &'a Value, key: &str) -> Option<&'a Value> {
        val.get(key)
    }

    fn dict_get_int<'a>(val: &'a Value, key: i64) -> Option<&'a Value> {
        match val {
            Value::Dict(pairs) => pairs
                .iter()
                .find(|(k, _)| k.as_int() == Some(key))
                .map(|(_, v)| v),
            _ => None,
        }
    }

    fn make_test_profile(preset: Preset) -> Option<(Catalog, EconomicsConfig, AccountProfile)> {
        let dir = testutil::game_dir()?;
        let catalog = crate::catalog::load(&dir).ok()?;
        let economics = EconomicsConfig::default();
        let profile = AccountProfile::new_with_preset(
            1001,
            "CommanderCrew",
            preset,
            &catalog,
            &economics,
            NOW_UTC,
        )
        .ok()?;
        Some((catalog, economics, profile))
    }

    fn find_starter_vehicle_id(profile: &AccountProfile, nation_id: u8, type_id: u8) -> i64 {
        for (&inv_id, veh) in &profile.vehicles {
            let descr = VehicleDescr::from_bytes(&veh.comp_descr).expect("descr");
            if descr.nation_id == nation_id && descr.type_id == type_id {
                return inv_id;
            }
        }
        panic!("starter vehicle ({nation_id}, {type_id}) not found");
    }

    #[test]
    fn test_buy_tankman_happy_path_and_ext_response() {
        let Some((catalog, economics, mut profile)) = make_test_profile(Preset::Retail) else {
            eprintln!("skip: no game install");
            return;
        };
        let ms1_veh_cd = make_int_comp_descr(1, 0, 13);
        profile.credits = 50_000;

        // Buy driver (role 3) for MS-1 at tier 1 (75%, costs 20,000 credits)
        let outcome = AccountEngine::apply(
            &mut profile,
            &EngineCmd::BuyTman {
                shop_rev: SERVER_SHOP_REV,
                veh_type_cd: ms1_veh_cd,
                role_idx: 3,
                tman_cost_idx: 1,
            },
            &catalog,
            &economics,
            SERVER_SHOP_REV,
            NOW_UTC,
        )
        .expect("buy tankman");

        assert_eq!(outcome.result_id, RES_SUCCESS);
        assert!(outcome.resync_dossiers);
        assert!(outcome.ext.is_some());
        let ext = outcome.ext.unwrap();
        assert!(ext.get("tmanInvID").is_some());
        assert!(ext.get("tmanCompDescr").is_some());

        let tman_id = ext.get("tmanInvID").unwrap().as_int().unwrap();
        let tman = profile.tankmen.get(&tman_id).expect("tankman in profile");
        assert!(tman.vehicle_inv_id.is_none());

        let descr = TankmanDescr::from_bytes(&tman.comp_descr).expect("tankman descr");
        assert_eq!(descr.role_level, 75);
        assert_eq!(descr.role_id, 3);
        assert!(!descr.is_female);
        assert_eq!(descr.dossier.len(), 132); // v10 dossier tail

        // Verify outcome.diff has inventory[8]['vehicle'][tman_id] = None
        let inv = dict_get(&outcome.diff, "inventory").expect("inventory in diff");
        let sec8 = dict_get_int(inv, 8).expect("section 8 in diff");
        let veh_dict = dict_get(sec8, "vehicle").expect("vehicle in section 8");
        let tman_veh = dict_get_int(veh_dict, tman_id).expect("tman_id in vehicle dict");
        assert_eq!(tman_veh, &Value::None);
    }

    #[test]
    fn test_buy_tankman_barracks_full_rejected() {
        let Some((catalog, economics, mut profile)) = make_test_profile(Preset::Retail) else {
            eprintln!("skip: no game install");
            return;
        };
        let ms1_veh_cd = make_int_comp_descr(1, 0, 13);

        // Fill barracks completely
        let unseated_count = profile
            .tankmen
            .values()
            .filter(|t| t.vehicle_inv_id.is_none())
            .count();
        profile.berths = unseated_count as i32;

        let err = AccountEngine::apply(
            &mut profile,
            &EngineCmd::BuyTman {
                shop_rev: SERVER_SHOP_REV,
                veh_type_cd: ms1_veh_cd,
                role_idx: 3,
                tman_cost_idx: 0,
            },
            &catalog,
            &economics,
            SERVER_SHOP_REV,
            NOW_UTC,
        )
        .unwrap_err();

        assert_eq!(err.result_id, RES_FAILURE);
    }

    #[test]
    fn test_equip_tankman_seat_unload_and_full_barracks() {
        let Some((catalog, economics, mut profile)) = make_test_profile(Preset::Retail) else {
            eprintln!("skip: no game install");
            return;
        };
        let ms1_id = find_starter_vehicle_id(&profile, 0, 13);
        let seated_crew = profile.vehicles[&ms1_id].crew.clone();
        let cmdr_id = seated_crew[0].expect("cmdr");

        // 1. Unload commander into barracks
        let res_unload = AccountEngine::apply(
            &mut profile,
            &EngineCmd::EquipTman {
                veh_inv_id: ms1_id,
                slot: 0,
                tman_inv_id: -1,
            },
            &catalog,
            &economics,
            SERVER_SHOP_REV,
            NOW_UTC,
        )
        .expect("unload commander");
        assert_eq!(res_unload.result_id, RES_SUCCESS);
        assert_eq!(profile.vehicles[&ms1_id].crew[0], None);
        assert_eq!(profile.tankmen[&cmdr_id].vehicle_inv_id, None);

        // 2. Reseat commander back into slot 0
        let res_seat = AccountEngine::apply(
            &mut profile,
            &EngineCmd::EquipTman {
                veh_inv_id: ms1_id,
                slot: 0,
                tman_inv_id: cmdr_id,
            },
            &catalog,
            &economics,
            SERVER_SHOP_REV,
            NOW_UTC,
        )
        .expect("seat commander");
        assert_eq!(res_seat.result_id, RES_SUCCESS);
        assert_eq!(profile.vehicles[&ms1_id].crew[0], Some(cmdr_id));
        assert_eq!(profile.tankmen[&cmdr_id].vehicle_inv_id, Some(ms1_id));

        // 3. 1-for-1 swap succeeds even when barracks full (delta = 0)
        // Buy a second commander in barracks
        let ms1_veh_cd = make_int_comp_descr(1, 0, 13);
        profile.gold = 500;
        let buy_out = AccountEngine::apply(
            &mut profile,
            &EngineCmd::BuyTman {
                shop_rev: SERVER_SHOP_REV,
                veh_type_cd: ms1_veh_cd,
                role_idx: 1, // commander
                tman_cost_idx: 0,
            },
            &catalog,
            &economics,
            SERVER_SHOP_REV,
            NOW_UTC,
        )
        .expect("buy second cmdr");
        let new_cmdr_id = buy_out.ext.unwrap().get("tmanInvID").unwrap().as_int().unwrap();

        // Artificially set berths equal to currently unseated count (barracks 100% full!)
        let unseated = profile.tankmen.values().filter(|t| t.vehicle_inv_id.is_none()).count();
        profile.berths = unseated as i32;

        // Swapping new_cmdr_id into slot 0 occupied by cmdr_id has delta = 0 -> MUST SUCCEED!
        let res_swap = AccountEngine::apply(
            &mut profile,
            &EngineCmd::EquipTman {
                veh_inv_id: ms1_id,
                slot: 0,
                tman_inv_id: new_cmdr_id,
            },
            &catalog,
            &economics,
            SERVER_SHOP_REV,
            NOW_UTC,
        )
        .expect("1-for-1 swap in full barracks");
        assert_eq!(res_swap.result_id, RES_SUCCESS);
        assert_eq!(profile.vehicles[&ms1_id].crew[0], Some(new_cmdr_id));
        assert_eq!(profile.tankmen[&cmdr_id].vehicle_inv_id, None);

        // 4. Unloading into full barracks MUST FAIL!
        let err_unload = AccountEngine::apply(
            &mut profile,
            &EngineCmd::EquipTman {
                veh_inv_id: ms1_id,
                slot: 0,
                tman_inv_id: -1,
            },
            &catalog,
            &economics,
            SERVER_SHOP_REV,
            NOW_UTC,
        )
        .unwrap_err();
        assert_eq!(err_unload.result_id, RES_FAILURE);
    }

    #[test]
    fn test_dismiss_tankman() {
        let Some((catalog, economics, mut profile)) = make_test_profile(Preset::Retail) else {
            eprintln!("skip: no game install");
            return;
        };
        let ms1_id = find_starter_vehicle_id(&profile, 0, 13);
        let cmdr_id = profile.vehicles[&ms1_id].crew[0].expect("cmdr");

        let outcome = AccountEngine::apply(
            &mut profile,
            &EngineCmd::DismissTman { tman_inv_id: cmdr_id },
            &catalog,
            &economics,
            SERVER_SHOP_REV,
            NOW_UTC,
        )
        .expect("dismiss seated tankman");

        assert_eq!(outcome.result_id, RES_SUCCESS);
        assert!(!profile.tankmen.contains_key(&cmdr_id));
        assert_eq!(profile.vehicles[&ms1_id].crew[0], None);
    }

    #[test]
    fn test_tankman_add_and_drop_skill() {
        let Some((catalog, economics, mut profile)) = make_test_profile(Preset::Retail) else {
            eprintln!("skip: no game install");
            return;
        };
        let ms1_id = find_starter_vehicle_id(&profile, 0, 13);
        let cmdr_id = profile.vehicles[&ms1_id].crew[0].expect("cmdr");

        // Role level is 100 on retail starter crew
        // Add skill 6 = repair
        let outcome = AccountEngine::apply(
            &mut profile,
            &EngineCmd::TmanAddSkill {
                tman_inv_id: cmdr_id,
                skill_idx: 6,
            },
            &catalog,
            &economics,
            SERVER_SHOP_REV,
            NOW_UTC,
        )
        .expect("add repair skill");
        assert_eq!(outcome.result_id, RES_SUCCESS);

        let tman = &profile.tankmen[&cmdr_id];
        let descr = TankmanDescr::from_bytes(&tman.comp_descr).unwrap();
        assert_eq!(descr.skills, vec!["repair".to_string()]);

        // Duplicate skill rejected
        let err = AccountEngine::apply(
            &mut profile,
            &EngineCmd::TmanAddSkill {
                tman_inv_id: cmdr_id,
                skill_idx: 6,
            },
            &catalog,
            &economics,
            SERVER_SHOP_REV,
            NOW_UTC,
        )
        .unwrap_err();
        assert_eq!(err.result_id, RES_FAILURE);

        // Drop skill 6 = repair
        let outcome2 = AccountEngine::apply(
            &mut profile,
            &EngineCmd::TmanDropSkill {
                tman_inv_id: cmdr_id,
                skill_idx: 6,
            },
            &catalog,
            &economics,
            SERVER_SHOP_REV,
            NOW_UTC,
        )
        .expect("drop repair skill");
        assert_eq!(outcome2.result_id, RES_SUCCESS);

        let descr2 = TankmanDescr::from_bytes(&profile.tankmen[&cmdr_id].comp_descr).unwrap();
        assert!(descr2.skills.is_empty());
    }

    #[test]
    fn test_tankman_passport() {
        let Some((catalog, economics, mut profile)) = make_test_profile(Preset::Retail) else {
            eprintln!("skip: no game install");
            return;
        };
        let ms1_id = find_starter_vehicle_id(&profile, 0, 13);
        let cmdr_id = profile.vehicles[&ms1_id].crew[0].expect("cmdr");
        profile.gold = 100;

        let cfg = catalog.tankmen_config(0).expect("ussr tankmen config");
        let group = cfg.normal_groups.iter().find(|g| !g.is_female).expect("normal group");
        let fn_id = group.first_names[0] as i32;
        let ln_id = group.last_names[0] as i32;
        let ic_id = group.icons[0] as i32;

        let outcome = AccountEngine::apply(
            &mut profile,
            &EngineCmd::TmanPassport {
                shop_rev: SERVER_SHOP_REV,
                tman_inv_id: cmdr_id,
                is_female_flag: -1,
                first_name_id: fn_id,
                last_name_id: ln_id,
                icon_id: ic_id,
            },
            &catalog,
            &economics,
            SERVER_SHOP_REV,
            NOW_UTC,
        )
        .expect("change passport");

        assert_eq!(outcome.result_id, RES_SUCCESS);
        assert_eq!(profile.gold, 100 - (economics.passport_change_cost as i64));

        let descr = TankmanDescr::from_bytes(&profile.tankmen[&cmdr_id].comp_descr).unwrap();
        assert_eq!(descr.first_name_id, fn_id as u16);
        assert_eq!(descr.last_name_id, ln_id as u16);
        assert_eq!(descr.icon_id, ic_id as u16);
    }
}
