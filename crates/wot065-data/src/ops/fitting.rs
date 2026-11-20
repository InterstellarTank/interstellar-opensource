//! Vehicle fitting, optional devices, ammo, consumables, repair, settings, and customization.

use std::collections::BTreeSet;

use crate::account_state::AccountProfile;
use crate::catalog::Catalog;
use crate::descr::{
    check_camouflage, check_equipments, make_int_comp_descr, parse_int_comp_descr,
    VehicleDescr, ITEM_TYPE_GUN, ITEM_TYPE_OPTIONAL_DEVICE, ITEM_TYPE_TURRET,
    ITEM_TYPE_VEHICLE, NUM_OPTIONAL_DEVICE_SLOTS,
};
use crate::economics::EconomicsConfig;
use crate::item_defs::NATION_NONE;
use crate::ops::{
    depot_give, depot_take, require_shop_rev, require_vehicle_not_broken, CmdError,
    CmdOutcome, DiffBuilder, RES_SUCCESS, VEHICLE_SETTINGS_FLAG_XP_TO_TMAN,
};

/// Equip vehicle component or turret (`CMD_EQUIP = 101`).
pub fn equip(
    profile: &mut AccountProfile,
    veh_inv_id: i64,
    item_cd: i32,
    gun_cd: i32,
    catalog: &Catalog,
    _economics: &EconomicsConfig,
    _server_shop_rev: i64,
    _now_utc: u32,
) -> Result<CmdOutcome, CmdError> {
    let (item_type, nation_id, _) = parse_int_comp_descr(item_cd);
    let mut veh_descr = {
        let veh_entry = profile
            .vehicles
            .get(&veh_inv_id)
            .ok_or_else(|| CmdError::failure(format!("vehicle {veh_inv_id} not found")))?;
        require_vehicle_not_broken(veh_entry)?;
        VehicleDescr::from_bytes(&veh_entry.comp_descr)
            .map_err(|e| CmdError::failure(format!("invalid vehicle descriptor: {e:?}")))?
    };

    if nation_id != veh_descr.nation_id {
        return Err(CmdError::failure("nation mismatch for installed item"));
    }

    let veh_def = catalog
        .vehicle_def_by_id(veh_descr.nation_id, veh_descr.type_id as u16)
        .ok_or_else(|| CmdError::failure("vehicle definition not found"))?;

    // Pre-check depot availability of target component
    if profile.items.get(&item_cd).copied().unwrap_or(0) < 1 {
        return Err(CmdError::failure(format!(
            "item {item_cd} is not available in depot"
        )));
    }

    if item_type == ITEM_TYPE_TURRET {
        let prev_turret_cd =
            make_int_comp_descr(ITEM_TYPE_TURRET, veh_descr.nation_id, veh_descr.turret_id);
        let prev_gun_cd =
            make_int_comp_descr(ITEM_TYPE_GUN, veh_descr.nation_id, veh_descr.gun_id);

        let swap_gun = gun_cd != 0 && gun_cd != prev_gun_cd;
        if swap_gun && profile.items.get(&gun_cd).copied().unwrap_or(0) < 1 {
            return Err(CmdError::failure(format!(
                "new gun {gun_cd} is not available in depot"
            )));
        }

        // Validate turret and gun combination
        veh_descr
            .may_install_turret(item_cd, gun_cd, veh_def, catalog)
            .map_err(|e| CmdError::failure(format!("cannot install turret: {e:?}")))?;

        // Local descriptor mutation before touching profile state
        veh_descr
            .install_turret(item_cd, gun_cd, veh_def, catalog)
            .map_err(|e| CmdError::failure(format!("install turret error: {e:?}")))?;

        // All validations passed, execute mutation
        let prev_rev = profile.rev;
        let mut diff = DiffBuilder::new();

        depot_take(profile, &mut diff, item_cd, 1)?;
        if swap_gun {
            depot_take(profile, &mut diff, gun_cd, 1)?;
        }

        depot_give(profile, &mut diff, prev_turret_cd, 1)?;
        if swap_gun {
            depot_give(profile, &mut diff, prev_gun_cd, 1)?;

            // Active gun changed: save old ammo layout, unload old shells, and restore new layout
            let (old_shells, saved_layout) = {
                let veh = profile.vehicles.get_mut(&veh_inv_id).unwrap();
                let old_shells = std::mem::take(&mut veh.shells);
                veh.set_shells_layout(prev_turret_cd, prev_gun_cd, old_shells.clone());
                let saved = veh.get_shells_layout(item_cd, gun_cd).map(|s| s.to_vec());
                (old_shells, saved)
            };

            for (scd, count) in old_shells {
                if count > 0 {
                    depot_give(profile, &mut diff, scd, count)?;
                }
            }

            let mut restored_shells = Vec::new();
            if let Some(saved) = saved_layout {
                for (scd, desired) in saved {
                    let in_depot = profile.items.get(&scd).copied().unwrap_or(0);
                    let take = desired.min(in_depot);
                    if take > 0 {
                        depot_take(profile, &mut diff, scd, take)?;
                        restored_shells.push((scd, take));
                    }
                }
            }

            let veh = profile.vehicles.get_mut(&veh_inv_id).unwrap();
            veh.shells = restored_shells;
            diff.veh_shells.insert(veh_inv_id, Some(veh.shells.clone()));
            diff.veh_shells_layout
                .insert(veh_inv_id, Some(veh.shells_layout.clone()));
        }

        let veh = profile.vehicles.get_mut(&veh_inv_id).unwrap();
        veh.comp_descr = veh_descr.to_bytes();
        diff.veh_comp_descr
            .insert(veh_inv_id, Some(veh.comp_descr.clone()));

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
        if gun_cd != 0 {
            return Err(CmdError::wrong_args(
                "gun_cd must be 0 for non-turret component installation",
            ));
        }

        // Validate component installation
        veh_descr
            .may_install_component(item_cd, veh_def, catalog)
            .map_err(|e| CmdError::failure(format!("cannot install component: {e:?}")))?;

        // Local descriptor mutation before touching profile state
        let prev_cd = veh_descr
            .install_component(item_cd, veh_def, catalog)
            .map_err(|e| CmdError::failure(format!("install component error: {e:?}")))?;

        let prev_rev = profile.rev;
        let mut diff = DiffBuilder::new();

        depot_take(profile, &mut diff, item_cd, 1)?;
        depot_give(profile, &mut diff, prev_cd, 1)?;

        if item_type == ITEM_TYPE_GUN {
            let cur_turret_cd =
                make_int_comp_descr(ITEM_TYPE_TURRET, veh_descr.nation_id, veh_descr.turret_id);

            let (old_shells, saved_layout) = {
                let veh = profile.vehicles.get_mut(&veh_inv_id).unwrap();
                let old_shells = std::mem::take(&mut veh.shells);
                veh.set_shells_layout(cur_turret_cd, prev_cd, old_shells.clone());
                let saved = veh.get_shells_layout(cur_turret_cd, item_cd).map(|s| s.to_vec());
                (old_shells, saved)
            };

            for (scd, count) in old_shells {
                if count > 0 {
                    depot_give(profile, &mut diff, scd, count)?;
                }
            }

            let mut restored_shells = Vec::new();
            if let Some(saved) = saved_layout {
                for (scd, desired) in saved {
                    let in_depot = profile.items.get(&scd).copied().unwrap_or(0);
                    let take = desired.min(in_depot);
                    if take > 0 {
                        depot_take(profile, &mut diff, scd, take)?;
                        restored_shells.push((scd, take));
                    }
                }
            }

            let veh = profile.vehicles.get_mut(&veh_inv_id).unwrap();
            veh.shells = restored_shells;
            diff.veh_shells.insert(veh_inv_id, Some(veh.shells.clone()));
            diff.veh_shells_layout
                .insert(veh_inv_id, Some(veh.shells_layout.clone()));
        }

        let veh = profile.vehicles.get_mut(&veh_inv_id).unwrap();
        veh.comp_descr = veh_descr.to_bytes();
        diff.veh_comp_descr
            .insert(veh_inv_id, Some(veh.comp_descr.clone()));

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

/// Equip or remove optional device (`CMD_EQUIP_OPTDEV = 102`).
pub fn equip_opt_dev(
    profile: &mut AccountProfile,
    veh_inv_id: i64,
    dev_cd: i32,
    slot_idx: usize,
    is_paid_removal: bool,
    catalog: &Catalog,
    economics: &EconomicsConfig,
    _server_shop_rev: i64,
    _now_utc: u32,
) -> Result<CmdOutcome, CmdError> {
    if slot_idx >= NUM_OPTIONAL_DEVICE_SLOTS {
        return Err(CmdError::wrong_args(format!(
            "invalid slot index {slot_idx} (must be 0..3)"
        )));
    }

    let mut veh_descr = {
        let veh_entry = profile
            .vehicles
            .get(&veh_inv_id)
            .ok_or_else(|| CmdError::failure(format!("vehicle {veh_inv_id} not found")))?;
        require_vehicle_not_broken(veh_entry)?;
        VehicleDescr::from_bytes(&veh_entry.comp_descr)
            .map_err(|e| CmdError::failure(format!("invalid vehicle descriptor: {e:?}")))?
    };

    let veh_def = catalog
        .vehicle_def_by_id(veh_descr.nation_id, veh_descr.type_id as u16)
        .ok_or_else(|| CmdError::failure("vehicle definition not found"))?;

    if dev_cd == 0 {
        // Demount device
        let old_dev_id = veh_descr.optional_devices[slot_idx]
            .ok_or_else(|| CmdError::failure(format!("slot {slot_idx} is already empty")))?;

        veh_descr
            .may_remove_optional_device(slot_idx, veh_def, catalog)
            .map_err(|e| CmdError::failure(format!("cannot remove device: {e:?}")))?;

        let old_dev_cd =
            make_int_comp_descr(ITEM_TYPE_OPTIONAL_DEVICE, NATION_NONE, old_dev_id);
        let old_dev_def = catalog
            .optional_device(old_dev_cd)
            .ok_or_else(|| CmdError::failure("device definition not found"))?;

        if !old_dev_def.removable && is_paid_removal {
            let cost = economics.paid_removal_cost as i64;
            if profile.gold < cost {
                return Err(CmdError::failure(format!(
                    "insufficient gold for paid removal: have {}, need {cost}",
                    profile.gold
                )));
            }
        }

        let prev_rev = profile.rev;
        let mut diff = DiffBuilder::new();

        veh_descr
            .remove_optional_device(slot_idx, veh_def, catalog)
            .map_err(|e| CmdError::failure(format!("remove device error: {e:?}")))?;

        if old_dev_def.removable {
            depot_give(profile, &mut diff, old_dev_cd, 1)?;
        } else if is_paid_removal {
            profile.gold -= economics.paid_removal_cost as i64;
            diff.gold = Some(profile.gold);
            depot_give(profile, &mut diff, old_dev_cd, 1)?;
        } // else: destroyed, depot unchanged

        let veh = profile.vehicles.get_mut(&veh_inv_id).unwrap();
        veh.comp_descr = veh_descr.to_bytes();
        diff.veh_comp_descr
            .insert(veh_inv_id, Some(veh.comp_descr.clone()));

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
        // Mount device
        if profile.items.get(&dev_cd).copied().unwrap_or(0) < 1 {
            return Err(CmdError::failure(format!(
                "optional device {dev_cd} not available in depot"
            )));
        }

        veh_descr
            .may_install_optional_device(dev_cd, slot_idx, veh_def, catalog)
            .map_err(|e| CmdError::failure(format!("cannot install device: {e:?}")))?;

        let old_dev_info = if let Some(old_id) = veh_descr.optional_devices[slot_idx] {
            let old_cd = make_int_comp_descr(ITEM_TYPE_OPTIONAL_DEVICE, NATION_NONE, old_id);
            let old_def = catalog
                .optional_device(old_cd)
                .ok_or_else(|| CmdError::failure("old device definition not found"))?;
            if !old_def.removable && is_paid_removal {
                let cost = economics.paid_removal_cost as i64;
                if profile.gold < cost {
                    return Err(CmdError::failure(format!(
                        "insufficient gold for paid removal: have {}, need {cost}",
                        profile.gold
                    )));
                }
            }
            Some((old_cd, old_def.removable))
        } else {
            None
        };

        // Local descriptor mutation before touching profile state
        veh_descr
            .install_optional_device(dev_cd, slot_idx, veh_def, catalog)
            .map_err(|e| CmdError::failure(format!("install device error: {e:?}")))?;

        let prev_rev = profile.rev;
        let mut diff = DiffBuilder::new();

        depot_take(profile, &mut diff, dev_cd, 1)?;

        if let Some((old_cd, removable)) = old_dev_info {
            if removable {
                depot_give(profile, &mut diff, old_cd, 1)?;
            } else if is_paid_removal {
                profile.gold -= economics.paid_removal_cost as i64;
                diff.gold = Some(profile.gold);
                depot_give(profile, &mut diff, old_cd, 1)?;
            }
        }

        let veh = profile.vehicles.get_mut(&veh_inv_id).unwrap();
        veh.comp_descr = veh_descr.to_bytes();
        diff.veh_comp_descr
            .insert(veh_inv_id, Some(veh.comp_descr.clone()));

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

/// Equip ammunition shells on vehicle (`CMD_EQUIP_SHELLS = 103`).
pub fn equip_shells(
    profile: &mut AccountProfile,
    veh_inv_id: i64,
    shells: &[(i32, i32)],
    catalog: &Catalog,
    _economics: &EconomicsConfig,
    _server_shop_rev: i64,
    _now_utc: u32,
) -> Result<CmdOutcome, CmdError> {
    let (veh_descr, old_shells) = {
        let veh_entry = profile
            .vehicles
            .get(&veh_inv_id)
            .ok_or_else(|| CmdError::failure(format!("vehicle {veh_inv_id} not found")))?;
        require_vehicle_not_broken(veh_entry)?;
        let descr = VehicleDescr::from_bytes(&veh_entry.comp_descr)
            .map_err(|e| CmdError::failure(format!("invalid vehicle descriptor: {e:?}")))?;
        (descr, veh_entry.shells.clone())
    };

    let veh_def = catalog
        .vehicle_def_by_id(veh_descr.nation_id, veh_descr.type_id as u16)
        .ok_or_else(|| CmdError::failure("vehicle definition not found"))?;

    let turret = veh_def
        .turrets
        .iter()
        .find(|t| t.component.id == veh_descr.turret_id)
        .ok_or_else(|| CmdError::failure("turret definition not found"))?;
    let gun = turret
        .guns
        .iter()
        .find(|g| g.component.id == veh_descr.gun_id)
        .ok_or_else(|| CmdError::failure("gun definition not found"))?;

    let valid_shot_cds: BTreeSet<i32> = gun.shots.iter().map(|s| s.shell_cd).collect();
    let mut seen_cds = BTreeSet::new();
    let mut total_ammo = 0i64;

    for &(scd, count) in shells {
        if count < 0 {
            return Err(CmdError::wrong_args("shell count cannot be negative"));
        }
        if !valid_shot_cds.contains(&scd) {
            return Err(CmdError::failure(format!(
                "shell {scd} is incompatible with gun {}",
                gun.component.id
            )));
        }
        if !seen_cds.insert(scd) {
            return Err(CmdError::wrong_args(format!(
                "duplicate shell descriptor {scd}"
            )));
        }
        total_ammo += count as i64;
    }

    if total_ammo > (gun.max_ammo as i64) {
        return Err(CmdError::failure(format!(
            "total shells ({total_ammo}) exceed gun maxAmmo ({})",
            gun.max_ammo
        )));
    }

    // Verify depot stock for net additions
    for &(scd, count) in shells {
        let old_cnt = old_shells
            .iter()
            .find(|(c, _)| *c == scd)
            .map(|(_, n)| *n)
            .unwrap_or(0);
        if count > old_cnt {
            let deficit = count - old_cnt;
            let available = profile.items.get(&scd).copied().unwrap_or(0);
            if available < deficit {
                return Err(CmdError::failure(format!(
                    "insufficient shells {scd} in depot: have {available}, need {deficit}"
                )));
            }
        }
    }

    // Apply mutations
    let prev_rev = profile.rev;
    let mut diff = DiffBuilder::new();

    // 1. Refunds
    for &(old_scd, old_cnt) in &old_shells {
        let new_cnt = shells
            .iter()
            .find(|(c, _)| *c == old_scd)
            .map(|(_, n)| *n)
            .unwrap_or(0);
        if old_cnt > new_cnt {
            depot_give(profile, &mut diff, old_scd, old_cnt - new_cnt)?;
        }
    }

    // 2. Deductions
    for &(new_scd, new_cnt) in shells {
        let old_cnt = old_shells
            .iter()
            .find(|(c, _)| *c == new_scd)
            .map(|(_, n)| *n)
            .unwrap_or(0);
        if new_cnt > old_cnt {
            depot_take(profile, &mut diff, new_scd, new_cnt - old_cnt)?;
        }
    }

    let clean_shells: Vec<(i32, i32)> =
        shells.iter().copied().filter(|(_, c)| *c > 0).collect();
    let turret_cd =
        make_int_comp_descr(ITEM_TYPE_TURRET, veh_descr.nation_id, veh_descr.turret_id);
    let gun_cd = make_int_comp_descr(ITEM_TYPE_GUN, veh_descr.nation_id, veh_descr.gun_id);

    let veh = profile.vehicles.get_mut(&veh_inv_id).unwrap();
    veh.shells = clean_shells.clone();
    veh.set_shells_layout(turret_cd, gun_cd, clean_shells);

    diff.veh_shells.insert(veh_inv_id, Some(veh.shells.clone()));
    diff.veh_shells_layout
        .insert(veh_inv_id, Some(veh.shells_layout.clone()));

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

/// Equip consumable equipments on vehicle (`CMD_EQUIP_EQS = 104`).
pub fn equip_eqs(
    profile: &mut AccountProfile,
    veh_inv_id: i64,
    eqs: &[i32; 3],
    catalog: &Catalog,
    _economics: &EconomicsConfig,
    _server_shop_rev: i64,
    _now_utc: u32,
) -> Result<CmdOutcome, CmdError> {
    let (veh_descr, old_eqs) = {
        let veh_entry = profile
            .vehicles
            .get(&veh_inv_id)
            .ok_or_else(|| CmdError::failure(format!("vehicle {veh_inv_id} not found")))?;
        require_vehicle_not_broken(veh_entry)?;
        let descr = VehicleDescr::from_bytes(&veh_entry.comp_descr)
            .map_err(|e| CmdError::failure(format!("invalid vehicle descriptor: {e:?}")))?;
        (descr, veh_entry.eqs)
    };

    let veh_def = catalog
        .vehicle_def_by_id(veh_descr.nation_id, veh_descr.type_id as u16)
        .ok_or_else(|| CmdError::failure("vehicle definition not found"))?;

    check_equipments(eqs, &veh_descr, veh_def, catalog)
        .map_err(|e| CmdError::failure(format!("invalid equipments loadout: {e:?}")))?;

    // Check depot stock for net additions
    let mut all_cds = BTreeSet::new();
    for &e in old_eqs.iter().chain(eqs.iter()) {
        if e != 0 {
            all_cds.insert(e);
        }
    }

    for &eq_cd in &all_cds {
        let old_count = old_eqs.iter().filter(|&&e| e == eq_cd).count() as i32;
        let new_count = eqs.iter().filter(|&&e| e == eq_cd).count() as i32;
        if new_count > old_count {
            let deficit = new_count - old_count;
            let available = profile.items.get(&eq_cd).copied().unwrap_or(0);
            if available < deficit {
                return Err(CmdError::failure(format!(
                    "insufficient equipment {eq_cd} in depot: have {available}, need {deficit}"
                )));
            }
        }
    }

    // Apply mutations
    let prev_rev = profile.rev;
    let mut diff = DiffBuilder::new();

    for &eq_cd in &all_cds {
        let old_count = old_eqs.iter().filter(|&&e| e == eq_cd).count() as i32;
        let new_count = eqs.iter().filter(|&&e| e == eq_cd).count() as i32;
        if old_count > new_count {
            depot_give(profile, &mut diff, eq_cd, old_count - new_count)?;
        } else if new_count > old_count {
            depot_take(profile, &mut diff, eq_cd, new_count - old_count)?;
        }
    }

    let veh = profile.vehicles.get_mut(&veh_inv_id).unwrap();
    veh.eqs = *eqs;
    veh.eqs_layout = *eqs;

    diff.veh_eqs.insert(veh_inv_id, Some(veh.eqs));
    diff.veh_eqs_layout.insert(veh_inv_id, Some(veh.eqs_layout));

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

/// Repair damaged vehicle (`CMD_REPAIR = 106`).
pub fn repair(
    profile: &mut AccountProfile,
    veh_inv_id: i64,
    _catalog: &Catalog,
    _economics: &EconomicsConfig,
    _server_shop_rev: i64,
    _now_utc: u32,
) -> Result<CmdOutcome, CmdError> {
    let (cost, _health) = {
        let veh_entry = profile
            .vehicles
            .get(&veh_inv_id)
            .ok_or_else(|| CmdError::failure(format!("vehicle {veh_inv_id} not found")))?;
        veh_entry
            .repair
            .ok_or_else(|| CmdError::failure("vehicle is not damaged and does not require repair"))?
    };
    let cost = cost as i64;

    if profile.credits < cost {
        return Err(CmdError::failure(format!(
            "insufficient credits for repair: have {}, need {cost}",
            profile.credits
        )));
    }

    let prev_rev = profile.rev;
    profile.credits -= cost;

    let veh = profile.vehicles.get_mut(&veh_inv_id).unwrap();
    veh.repair = None;

    let mut diff = DiffBuilder::new();
    diff.credits = Some(profile.credits);
    diff.clear_veh_repair.insert(veh_inv_id);

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

/// Change vehicle setting bitflag (`CMD_VEH_SETTINGS = 107`).
pub fn veh_settings(
    profile: &mut AccountProfile,
    veh_inv_id: i64,
    flag: i32,
    is_on: bool,
    catalog: &Catalog,
    _economics: &EconomicsConfig,
    _server_shop_rev: i64,
    _now_utc: u32,
) -> Result<CmdOutcome, CmdError> {
    if !matches!(flag, 1 | 2 | 4 | 8) {
        return Err(CmdError::wrong_args(format!(
            "invalid vehicle setting flag {flag} (expected 1, 2, 4, or 8)"
        )));
    }

    let veh_entry = profile
        .vehicles
        .get(&veh_inv_id)
        .ok_or_else(|| CmdError::failure(format!("vehicle {veh_inv_id} not found")))?;

    if flag == VEHICLE_SETTINGS_FLAG_XP_TO_TMAN && is_on {
        let veh_descr = VehicleDescr::from_bytes(&veh_entry.comp_descr)
            .map_err(|e| CmdError::failure(format!("invalid vehicle descriptor: {e:?}")))?;
        let veh_cd = make_int_comp_descr(
            ITEM_TYPE_VEHICLE,
            veh_descr.nation_id,
            veh_descr.type_id as u16,
        );
        let veh_def = catalog
            .vehicle_def(veh_cd)
            .ok_or_else(|| CmdError::failure("vehicle definition not found"))?;

        let is_elite =
            veh_def.is_default_elite || profile.elite_vehicles.contains(&veh_cd);
        if !is_elite {
            return Err(CmdError::failure(
                "accelerated crew training requires vehicle to be elite",
            ));
        }
    }

    let prev_rev = profile.rev;
    let veh = profile.vehicles.get_mut(&veh_inv_id).unwrap();
    if is_on {
        veh.settings |= flag;
    } else {
        veh.settings &= !flag;
    }

    let mut diff = DiffBuilder::new();
    diff.veh_settings.insert(veh_inv_id, Some(veh.settings));

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

/// Apply or remove vehicle camouflage (`CMD_VEH_CAMOUFLAGE = 120`).
pub fn veh_camouflage(
    profile: &mut AccountProfile,
    shop_rev: i64,
    veh_inv_id: i64,
    camo_id: u16,
    catalog: &Catalog,
    _economics: &EconomicsConfig,
    server_shop_rev: i64,
    _now_utc: u32,
) -> Result<CmdOutcome, CmdError> {
    require_shop_rev(shop_rev, server_shop_rev)?;

    let mut veh_descr = {
        let veh_entry = profile
            .vehicles
            .get(&veh_inv_id)
            .ok_or_else(|| CmdError::failure(format!("vehicle {veh_inv_id} not found")))?;
        require_vehicle_not_broken(veh_entry)?;
        VehicleDescr::from_bytes(&veh_entry.comp_descr)
            .map_err(|e| CmdError::failure(format!("invalid vehicle descriptor: {e:?}")))?
    };

    let veh_def = catalog
        .vehicle_def_by_id(veh_descr.nation_id, veh_descr.type_id as u16)
        .ok_or_else(|| CmdError::failure("vehicle definition not found"))?;

    let prev_rev = profile.rev;
    let mut diff = DiffBuilder::new();

    if camo_id == 0 {
        // Free removal
        veh_descr.camouflage_id = None;
    } else {
        let camo = catalog
            .camouflage(veh_descr.nation_id, camo_id)
            .ok_or_else(|| CmdError::failure(format!("camouflage {camo_id} not found")))?;
        if !camo.show_in_shop {
            return Err(CmdError::failure("camouflage is not available in shop"));
        }
        check_camouflage(camo_id, veh_descr.nation_id, veh_def, catalog)
            .map_err(|e| CmdError::failure(format!("camouflage not compatible: {e:?}")))?;

        let cost = camo.gold as i64;
        if profile.gold < cost {
            return Err(CmdError::failure(format!(
                "insufficient gold for camouflage: have {}, need {cost}",
                profile.gold
            )));
        }

        profile.gold -= cost;
        diff.gold = Some(profile.gold);
        veh_descr.camouflage_id = Some(camo_id);
    }

    let veh = profile.vehicles.get_mut(&veh_inv_id).unwrap();
    veh.comp_descr = veh_descr.to_bytes();
    diff.veh_comp_descr
        .insert(veh_inv_id, Some(veh.comp_descr.clone()));

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

/// Change vehicle horn (`CMD_VEH_HORN = 121`).
pub fn veh_horn(
    _profile: &mut AccountProfile,
    _shop_rev: i64,
    _veh_inv_id: i64,
    _horn_id: u8,
    _catalog: &Catalog,
    _economics: &EconomicsConfig,
    _server_shop_rev: i64,
    _now_utc: u32,
) -> Result<CmdOutcome, CmdError> {
    Err(CmdError::failure("horn interface is disabled in 0.6.5"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::account_state::{AccountProfile, Preset};
    use crate::descr::{
        make_int_comp_descr, VehicleDescr, ITEM_TYPE_ENGINE, ITEM_TYPE_GUN,
        ITEM_TYPE_TURRET,
    };
    use crate::economics::EconomicsConfig;
    use crate::ops::{
        AccountEngine, EngineCmd, RES_FAILURE, RES_SUCCESS,
        VEHICLE_SETTINGS_FLAG_AUTO_REPAIR, VEHICLE_SETTINGS_FLAG_XP_TO_TMAN,
    };
    use crate::testutil;

    const SERVER_SHOP_REV: i64 = 42;
    const NOW_UTC: u32 = 1_700_000_000;

    fn make_test_profile(preset: Preset) -> Option<(Catalog, EconomicsConfig, AccountProfile)> {
        let dir = testutil::game_dir()?;
        let catalog = crate::catalog::load(&dir).ok()?;
        let economics = EconomicsConfig::default();
        let profile = AccountProfile::new_with_preset(
            1001,
            "CommanderFitting",
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
    fn test_equip_component_happy_path_and_depot_swap() {
        let Some((catalog, economics, mut profile)) = make_test_profile(Preset::Retail) else {
            eprintln!("skip: no game install");
            return;
        };

        // MS-1 is nation 0, type_id 13
        let ms1_id = find_starter_vehicle_id(&profile, 0, 13);
        let stock_eng_cd = make_int_comp_descr(ITEM_TYPE_ENGINE, 0, 14); // GAZ-T17
        let upgraded_eng_cd = make_int_comp_descr(ITEM_TYPE_ENGINE, 0, 15); // GAZ-T26

        // Put upgraded engine into depot
        profile.items.insert(upgraded_eng_cd, 1);

        let outcome = AccountEngine::apply(
            &mut profile,
            &EngineCmd::Equip {
                veh_inv_id: ms1_id,
                item_cd: upgraded_eng_cd,
                gun_cd: 0,
            },
            &catalog,
            &economics,
            SERVER_SHOP_REV,
            NOW_UTC,
        )
        .expect("equip upgraded engine");

        assert_eq!(outcome.result_id, RES_SUCCESS);
        assert_eq!(profile.items.get(&upgraded_eng_cd), None);
        assert_eq!(profile.items.get(&stock_eng_cd), Some(&1));

        let veh = &profile.vehicles[&ms1_id];
        let descr = VehicleDescr::from_bytes(&veh.comp_descr).unwrap();
        assert_eq!(descr.engine_id, 15);
    }

    #[test]
    fn test_equip_component_missing_from_depot_rejected_transactional() {
        let Some((catalog, economics, mut profile)) = make_test_profile(Preset::Retail) else {
            eprintln!("skip: no game install");
            return;
        };
        let ms1_id = find_starter_vehicle_id(&profile, 0, 13);
        let upgraded_eng_cd = make_int_comp_descr(ITEM_TYPE_ENGINE, 0, 15);
        let rev_before = profile.rev;

        let err = AccountEngine::apply(
            &mut profile,
            &EngineCmd::Equip {
                veh_inv_id: ms1_id,
                item_cd: upgraded_eng_cd,
                gun_cd: 0,
            },
            &catalog,
            &economics,
            SERVER_SHOP_REV,
            NOW_UTC,
        )
        .unwrap_err();

        assert_eq!(err.result_id, RES_FAILURE);
        assert_eq!(profile.rev, rev_before);
    }

    #[test]
    fn test_equip_component_broken_vehicle_blocked() {
        let Some((catalog, economics, mut profile)) = make_test_profile(Preset::Retail) else {
            eprintln!("skip: no game install");
            return;
        };
        let ms1_id = find_starter_vehicle_id(&profile, 0, 13);
        let upgraded_eng_cd = make_int_comp_descr(ITEM_TYPE_ENGINE, 0, 15);
        profile.items.insert(upgraded_eng_cd, 1);

        // Mark vehicle as broken
        profile.vehicles.get_mut(&ms1_id).unwrap().repair = Some((300, 10));

        let err = AccountEngine::apply(
            &mut profile,
            &EngineCmd::Equip {
                veh_inv_id: ms1_id,
                item_cd: upgraded_eng_cd,
                gun_cd: 0,
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
    fn test_equip_turret_keep_gun_and_swap_with_ammo() {
        let Some((catalog, economics, mut profile)) = make_test_profile(Preset::Retail) else {
            eprintln!("skip: no game install");
            return;
        };
        let ms1_id = find_starter_vehicle_id(&profile, 0, 13);
        let veh_def = catalog.vehicle_def_by_id(0, 13).unwrap();
        let stock_turret_cd = make_int_comp_descr(ITEM_TYPE_TURRET, 0, 23);
        let stock_gun_cd = make_int_comp_descr(ITEM_TYPE_GUN, 0, 23);
        let up_turret_cd = make_int_comp_descr(ITEM_TYPE_TURRET, 0, 24);
        let up_turret = veh_def
            .turrets
            .iter()
            .find(|t| t.component.id == 24)
            .expect("turret 24");
        let up_gun = up_turret
            .guns
            .iter()
            .find(|g| g.component.id != 23)
            .expect("upgraded gun on turret 24");
        let up_gun_cd = up_gun.component.comp_descr;

        // Case A: Turret install with gun_cd == 0 keeps gun
        profile.items.insert(up_turret_cd, 1);

        let outcome = AccountEngine::apply(
            &mut profile,
            &EngineCmd::Equip {
                veh_inv_id: ms1_id,
                item_cd: up_turret_cd,
                gun_cd: 0,
            },
            &catalog,
            &economics,
            SERVER_SHOP_REV,
            NOW_UTC,
        )
        .expect("equip turret keeping gun");

        assert_eq!(outcome.result_id, RES_SUCCESS);
        assert_eq!(profile.items.get(&stock_turret_cd), Some(&1));
        assert_eq!(profile.items.get(&up_turret_cd), None);
        let descr = VehicleDescr::from_bytes(&profile.vehicles[&ms1_id].comp_descr).unwrap();
        assert_eq!(descr.turret_id, 24);
        assert_eq!(descr.gun_id, 23);

        // Case B: Swapping gun unloads ammo back to depot
        profile.items.insert(up_gun_cd, 1);
        let initial_loaded_shells = profile.vehicles[&ms1_id].shells.clone();
        assert!(!initial_loaded_shells.is_empty());

        let outcome_gun = AccountEngine::apply(
            &mut profile,
            &EngineCmd::Equip {
                veh_inv_id: ms1_id,
                item_cd: up_gun_cd,
                gun_cd: 0,
            },
            &catalog,
            &economics,
            SERVER_SHOP_REV,
            NOW_UTC,
        )
        .expect("equip new gun");

        assert_eq!(outcome_gun.result_id, RES_SUCCESS);
        assert_eq!(profile.items.get(&stock_gun_cd), Some(&1));
        // Verify old shells unloaded to depot
        for (scd, count) in initial_loaded_shells {
            assert!(profile.items.get(&scd).copied().unwrap_or(0) >= count);
        }
    }

    #[test]
    fn test_equip_optdev_mount_demount_and_paid_removal() {
        let Some((catalog, economics, mut profile)) = make_test_profile(Preset::Retail) else {
            eprintln!("skip: no game install");
            return;
        };
        let ms1_id = find_starter_vehicle_id(&profile, 0, 13);

        let toolbox = catalog
            .optional_devices
            .values()
            .find(|d| d.name == "toolbox")
            .expect("toolbox");
        assert!(toolbox.removable);

        let non_removable = catalog
            .optional_devices
            .values()
            .find(|d| !d.removable && d.weight == 0.0)
            .expect("non-removable device with zero weight");
        assert!(!non_removable.removable);

        // Put devices in depot
        profile.items.insert(toolbox.comp_descr, 1);
        profile.items.insert(non_removable.comp_descr, 1);
        profile.gold = 50;

        // 1. Mount toolbox in slot 0
        let res1 = AccountEngine::apply(
            &mut profile,
            &EngineCmd::EquipOptDev {
                veh_inv_id: ms1_id,
                dev_cd: toolbox.comp_descr,
                slot_idx: 0,
                is_paid_removal: false,
            },
            &catalog,
            &economics,
            SERVER_SHOP_REV,
            NOW_UTC,
        )
        .expect("mount toolbox");
        assert_eq!(res1.result_id, RES_SUCCESS);
        assert_eq!(profile.items.get(&toolbox.comp_descr), None);

        // 2. Demount removable toolbox (free, returns to depot)
        let res2 = AccountEngine::apply(
            &mut profile,
            &EngineCmd::EquipOptDev {
                veh_inv_id: ms1_id,
                dev_cd: 0,
                slot_idx: 0,
                is_paid_removal: false,
            },
            &catalog,
            &economics,
            SERVER_SHOP_REV,
            NOW_UTC,
        )
        .expect("demount removable toolbox");
        assert_eq!(res2.result_id, RES_SUCCESS);
        assert_eq!(profile.items.get(&toolbox.comp_descr), Some(&1));

        // 3. Mount non-removable device in slot 0
        let res3 = AccountEngine::apply(
            &mut profile,
            &EngineCmd::EquipOptDev {
                veh_inv_id: ms1_id,
                dev_cd: non_removable.comp_descr,
                slot_idx: 0,
                is_paid_removal: false,
            },
            &catalog,
            &economics,
            SERVER_SHOP_REV,
            NOW_UTC,
        )
        .expect("mount non-removable device");
        assert_eq!(res3.result_id, RES_SUCCESS);

        // 4. Demount non-removable with is_paid_removal = true (costs 10 gold, returns to depot)
        let gold_before = profile.gold;
        let res4 = AccountEngine::apply(
            &mut profile,
            &EngineCmd::EquipOptDev {
                veh_inv_id: ms1_id,
                dev_cd: 0,
                slot_idx: 0,
                is_paid_removal: true,
            },
            &catalog,
            &economics,
            SERVER_SHOP_REV,
            NOW_UTC,
        )
        .expect("paid removal of non-removable device");
        assert_eq!(res4.result_id, RES_SUCCESS);
        assert_eq!(profile.gold, gold_before - (economics.paid_removal_cost as i64));
        assert_eq!(profile.items.get(&non_removable.comp_descr), Some(&1));
    }

    #[test]
    fn test_equip_shells_compatibility_and_delta() {
        let Some((catalog, economics, mut profile)) = make_test_profile(Preset::Retail) else {
            eprintln!("skip: no game install");
            return;
        };
        let ms1_id = find_starter_vehicle_id(&profile, 0, 13);
        let veh = &profile.vehicles[&ms1_id];
        let descr = VehicleDescr::from_bytes(&veh.comp_descr).unwrap();
        let veh_def = catalog.vehicle_def_by_id(0, 13).unwrap();
        let turret = veh_def.turrets.iter().find(|t| t.component.id == descr.turret_id).unwrap();
        let gun = turret.guns.iter().find(|g| g.component.id == descr.gun_id).unwrap();

        let valid_shell_cd = gun.shots[0].shell_cd;
        // Give 20 shells in depot
        profile.items.insert(valid_shell_cd, 20);

        let new_shells = vec![(valid_shell_cd, 15)];
        let outcome = AccountEngine::apply(
            &mut profile,
            &EngineCmd::EquipShells {
                veh_inv_id: ms1_id,
                shells: new_shells,
            },
            &catalog,
            &economics,
            SERVER_SHOP_REV,
            NOW_UTC,
        )
        .expect("equip shells");

        assert_eq!(outcome.result_id, RES_SUCCESS);
        assert_eq!(profile.vehicles[&ms1_id].shells, vec![(valid_shell_cd, 15)]);

        // Try incompatible shell
        let err = AccountEngine::apply(
            &mut profile,
            &EngineCmd::EquipShells {
                veh_inv_id: ms1_id,
                shells: vec![(999999, 10)],
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
    fn test_repair_damaged_vehicle() {
        let Some((catalog, economics, mut profile)) = make_test_profile(Preset::Retail) else {
            eprintln!("skip: no game install");
            return;
        };
        let ms1_id = find_starter_vehicle_id(&profile, 0, 13);
        profile.credits = 10_000;
        profile.vehicles.get_mut(&ms1_id).unwrap().repair = Some((350, 10));

        let outcome = AccountEngine::apply(
            &mut profile,
            &EngineCmd::Repair { veh_inv_id: ms1_id },
            &catalog,
            &economics,
            SERVER_SHOP_REV,
            NOW_UTC,
        )
        .expect("repair vehicle");

        assert_eq!(outcome.result_id, RES_SUCCESS);
        assert_eq!(profile.credits, 10_000 - 350);
        assert!(profile.vehicles[&ms1_id].repair.is_none());

        // Second repair on fully repaired vehicle fails
        let err = AccountEngine::apply(
            &mut profile,
            &EngineCmd::Repair { veh_inv_id: ms1_id },
            &catalog,
            &economics,
            SERVER_SHOP_REV,
            NOW_UTC,
        )
        .unwrap_err();
        assert_eq!(err.result_id, RES_FAILURE);
    }

    #[test]
    fn test_vehicle_settings_elite_gate() {
        let Some((catalog, economics, mut profile)) = make_test_profile(Preset::Retail) else {
            eprintln!("skip: no game install");
            return;
        };
        let ms1_id = find_starter_vehicle_id(&profile, 0, 13);
        let veh_cd = make_int_comp_descr(1, 0, 13);
        profile.elite_vehicles.remove(&veh_cd);

        // Setting XP_TO_TMAN fails if not elite
        let err = AccountEngine::apply(
            &mut profile,
            &EngineCmd::VehSettings {
                veh_inv_id: ms1_id,
                flag: VEHICLE_SETTINGS_FLAG_XP_TO_TMAN,
                is_on: true,
            },
            &catalog,
            &economics,
            SERVER_SHOP_REV,
            NOW_UTC,
        )
        .unwrap_err();
        assert_eq!(err.result_id, RES_FAILURE);

        // Mark elite
        profile.elite_vehicles.insert(veh_cd);
        let outcome = AccountEngine::apply(
            &mut profile,
            &EngineCmd::VehSettings {
                veh_inv_id: ms1_id,
                flag: VEHICLE_SETTINGS_FLAG_XP_TO_TMAN,
                is_on: true,
            },
            &catalog,
            &economics,
            SERVER_SHOP_REV,
            NOW_UTC,
        )
        .expect("set xp to tman on elite vehicle");
        assert_eq!(outcome.result_id, RES_SUCCESS);
        assert_eq!(
            profile.vehicles[&ms1_id].settings & VEHICLE_SETTINGS_FLAG_XP_TO_TMAN,
            VEHICLE_SETTINGS_FLAG_XP_TO_TMAN
        );

        // Setting auto repair works on any vehicle
        let outcome2 = AccountEngine::apply(
            &mut profile,
            &EngineCmd::VehSettings {
                veh_inv_id: ms1_id,
                flag: VEHICLE_SETTINGS_FLAG_AUTO_REPAIR,
                is_on: true,
            },
            &catalog,
            &economics,
            SERVER_SHOP_REV,
            NOW_UTC,
        )
        .expect("set auto repair");
        assert_eq!(outcome2.result_id, RES_SUCCESS);
    }

    #[test]
    fn test_vehicle_camouflage_and_horn() {
        let Some((catalog, economics, mut profile)) = make_test_profile(Preset::Retail) else {
            eprintln!("skip: no game install");
            return;
        };
        let ms1_id = find_starter_vehicle_id(&profile, 0, 13);

        // Horn command always returns RES_FAILURE in 0.6.5
        let err = AccountEngine::apply(
            &mut profile,
            &EngineCmd::VehHorn {
                shop_rev: SERVER_SHOP_REV,
                veh_inv_id: ms1_id,
                horn_id: 1,
            },
            &catalog,
            &economics,
            SERVER_SHOP_REV,
            NOW_UTC,
        )
        .unwrap_err();
        assert_eq!(err.result_id, RES_FAILURE);

        // Clearing camouflage when none set succeeds for free
        let clear_res = AccountEngine::apply(
            &mut profile,
            &EngineCmd::VehCamouflage {
                shop_rev: SERVER_SHOP_REV,
                veh_inv_id: ms1_id,
                camo_id: 0,
            },
            &catalog,
            &economics,
            SERVER_SHOP_REV,
            NOW_UTC,
        )
        .expect("clear camo");
        assert_eq!(clear_res.result_id, RES_SUCCESS);
    }
}
