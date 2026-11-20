//! Handlers for shop, economy, research (unlock), and vehicle buy/sell commands.

use std::collections::BTreeSet;

use crate::account_state::{AccountProfile, TankmanEntry, VehicleEntry};
use crate::catalog::Catalog;
use crate::descr::{
    parse_int_comp_descr, TankmanDescr, VehicleDescr,
    ITEM_TYPE_EQUIPMENT, ITEM_TYPE_SHELL, ITEM_TYPE_VEHICLE,
};
use crate::dossier::{TankmanDossier, VehicleDossier};
use crate::economics::EconomicsConfig;
use crate::ops::{
    get_item_price_and_shop_flag, recompute_elite_vehicles, require_shop_rev,
    CmdError, CmdOutcome, DiffBuilder, BUY_VEHICLE_FLAG_CREW, BUY_VEHICLE_FLAG_SHELLS,
    RES_SUCCESS, SELL_VEHICLE_FLAG_CREW, SELL_VEHICLE_FLAG_EQS,
    SELL_VEHICLE_FLAG_SHELLS,
};

/// Buy additional garage slot (`CMD_BUY_SLOT = 205`).
pub fn buy_slot(
    profile: &mut AccountProfile,
    client_shop_rev: i64,
    _catalog: &Catalog,
    economics: &EconomicsConfig,
    server_shop_rev: i64,
    _now_utc: u32,
) -> Result<CmdOutcome, CmdError> {
    require_shop_rev(client_shop_rev, server_shop_rev)?;
    let cost = economics.next_slot_price(profile.slots) as i64;
    if profile.gold < cost {
        return Err(CmdError::failure(format!(
            "insufficient gold for slot: have {}, need {cost}",
            profile.gold
        )));
    }

    let prev_rev = profile.rev;
    profile.gold -= cost;
    profile.slots += 1;
    profile.rev += 1;

    let mut diff = DiffBuilder::new();
    diff.gold = Some(profile.gold);
    diff.slots = Some(profile.slots);

    Ok(CmdOutcome {
        result_id: RES_SUCCESS,
        prev_rev,
        rev: profile.rev,
        diff: diff.build(prev_rev, profile.rev),
        ext: None,
        resync_dossiers: false,
    })
}

/// Buy additional barracks berths pack (`CMD_BUY_BERTHS = 206`).
pub fn buy_berths(
    profile: &mut AccountProfile,
    client_shop_rev: i64,
    _catalog: &Catalog,
    economics: &EconomicsConfig,
    server_shop_rev: i64,
    _now_utc: u32,
) -> Result<CmdOutcome, CmdError> {
    require_shop_rev(client_shop_rev, server_shop_rev)?;
    let cost = economics.next_berth_pack_price(profile.berths) as i64;
    if profile.gold < cost {
        return Err(CmdError::failure(format!(
            "insufficient gold for berths: have {}, need {cost}",
            profile.gold
        )));
    }
    let pack_size = economics.berths_prices.1;

    let prev_rev = profile.rev;
    profile.gold -= cost;
    profile.berths += pack_size;
    profile.rev += 1;

    let mut diff = DiffBuilder::new();
    diff.gold = Some(profile.gold);
    diff.berths = Some(profile.berths);

    Ok(CmdOutcome {
        result_id: RES_SUCCESS,
        prev_rev,
        rev: profile.rev,
        diff: diff.build(prev_rev, profile.rev),
        ext: None,
        resync_dossiers: false,
    })
}

/// Exchange gold for credits (`CMD_EXCHANGE = 202`).
pub fn exchange(
    profile: &mut AccountProfile,
    client_shop_rev: i64,
    gold: i64,
    _catalog: &Catalog,
    economics: &EconomicsConfig,
    server_shop_rev: i64,
    _now_utc: u32,
) -> Result<CmdOutcome, CmdError> {
    require_shop_rev(client_shop_rev, server_shop_rev)?;
    if gold <= 0 {
        return Err(CmdError::wrong_args("gold must be positive"));
    }
    if profile.gold < gold {
        return Err(CmdError::failure(format!(
            "insufficient gold for exchange: have {}, need {gold}",
            profile.gold
        )));
    }
    let credits_gain = gold
        .checked_mul(economics.exchange_rate as i64)
        .ok_or_else(|| CmdError::failure("credits calculation overflow"))?;

    let new_gold = profile
        .gold
        .checked_sub(gold)
        .ok_or_else(|| CmdError::failure("insufficient gold"))?;
    let new_credits = profile
        .credits
        .checked_add(credits_gain)
        .ok_or_else(|| CmdError::failure("credits overflow"))?;

    let prev_rev = profile.rev;
    profile.gold = new_gold;
    profile.credits = new_credits;
    profile.rev += 1;

    let mut diff = DiffBuilder::new();
    diff.gold = Some(profile.gold);
    diff.credits = Some(profile.credits);

    Ok(CmdOutcome {
        result_id: RES_SUCCESS,
        prev_rev,
        rev: profile.rev,
        diff: diff.build(prev_rev, profile.rev),
        ext: None,
        resync_dossiers: false,
    })
}

/// Convert elite vehicle XP to free XP (`CMD_FREE_XP_CONV = 203`).
pub fn convert_free_xp(
    profile: &mut AccountProfile,
    client_shop_rev: i64,
    xp: i64,
    veh_cds: &[i32],
    catalog: &Catalog,
    economics: &EconomicsConfig,
    server_shop_rev: i64,
    _now_utc: u32,
) -> Result<CmdOutcome, CmdError> {
    require_shop_rev(client_shop_rev, server_shop_rev)?;
    if xp <= 0 || veh_cds.is_empty() {
        return Err(CmdError::wrong_args("xp must be positive and vehicles specified"));
    }
    let (discrecity, gold_cost) = economics.free_xp_conversion;
    if discrecity <= 0 || xp % (discrecity as i64) != 0 {
        return Err(CmdError::wrong_args(format!(
            "xp {xp} is not divisible by conversion discrecity {discrecity}"
        )));
    }

    // Check duplicate vehicles in array
    let mut seen = BTreeSet::new();
    for &cd in veh_cds {
        if !seen.insert(cd) {
            return Err(CmdError::wrong_args(format!("duplicate vehicle cd {cd}")));
        }
    }

    // Verify all vehicles are elite
    for &cd in veh_cds {
        let is_elite = profile.elite_vehicles.contains(&cd)
            || catalog.vehicle_def(cd).map_or(false, |v| v.is_default_elite);
        if !is_elite {
            return Err(CmdError::failure(format!("vehicle {cd} is not elite")));
        }
    }

    // Check total XP available on the vehicles
    let total_avail_xp: i64 = veh_cds
        .iter()
        .map(|cd| profile.veh_type_xp.get(cd).copied().unwrap_or(0) as i64)
        .sum();
    if total_avail_xp < xp {
        return Err(CmdError::failure(format!(
            "insufficient xp on selected vehicles: available {total_avail_xp}, needed {xp}"
        )));
    }

    // Compute gold cost
    let gold_needed = (xp / (discrecity as i64))
        .checked_mul(gold_cost as i64)
        .ok_or_else(|| CmdError::failure("gold calculation overflow"))?;
    if profile.gold < gold_needed {
        return Err(CmdError::failure(format!(
            "insufficient gold: available {}, needed {gold_needed}",
            profile.gold
        )));
    }

    let new_gold = profile
        .gold
        .checked_sub(gold_needed)
        .ok_or_else(|| CmdError::failure("insufficient gold"))?;
    let new_free_xp = profile
        .free_xp
        .checked_add(xp)
        .ok_or_else(|| CmdError::failure("free_xp overflow"))?;

    let prev_rev = profile.rev;
    profile.gold = new_gold;
    profile.free_xp = new_free_xp;

    let mut diff = DiffBuilder::new();
    diff.gold = Some(profile.gold);
    diff.free_xp = Some(profile.free_xp);

    let mut remaining_xp: i64 = xp;
    for &cd in veh_cds {
        if remaining_xp <= 0 {
            break;
        }
        let cur = profile.veh_type_xp.get(&cd).copied().unwrap_or(0);
        let take = (cur as i64).min(remaining_xp) as i32;
        let new_xp = cur - take;
        if new_xp == 0 {
            profile.veh_type_xp.remove(&cd);
            diff.veh_type_xp.insert(cd, None);
        } else {
            profile.veh_type_xp.insert(cd, new_xp);
            diff.veh_type_xp.insert(cd, Some(new_xp));
        }
        remaining_xp -= take as i64;
    }

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

/// Upgrade account to premium (`CMD_PREMIUM = 204`).
pub fn upgrade_to_premium(
    profile: &mut AccountProfile,
    client_shop_rev: i64,
    days: i32,
    _catalog: &Catalog,
    economics: &EconomicsConfig,
    server_shop_rev: i64,
    now_utc: u32,
) -> Result<CmdOutcome, CmdError> {
    require_shop_rev(client_shop_rev, server_shop_rev)?;
    let cost = match economics.premium_cost.get(&days) {
        Some(&c) => c as i64,
        None => return Err(CmdError::wrong_args(format!("invalid premium days: {days}"))),
    };
    if profile.gold < cost {
        return Err(CmdError::failure("insufficient gold for premium"));
    }

    let duration_secs = (days as u32)
        .checked_mul(86400)
        .ok_or_else(|| CmdError::failure("duration overflow"))?;

    let prev_rev = profile.rev;
    profile.gold -= cost;
    profile.attrs |= 1u64 << 32;
    profile.premium_expiry_time = profile
        .premium_expiry_time
        .max(now_utc)
        .saturating_add(duration_secs);
    profile.rev += 1;

    let mut diff = DiffBuilder::new();
    diff.gold = Some(profile.gold);
    diff.attrs = Some(profile.attrs);
    diff.premium_expiry_time = Some(profile.premium_expiry_time);

    Ok(CmdOutcome {
        result_id: RES_SUCCESS,
        prev_rev,
        rev: profile.rev,
        diff: diff.build(prev_rev, profile.rev),
        ext: None,
        resync_dossiers: false,
    })
}

/// Research item or vehicle (`CMD_UNLOCK = 201`).
pub fn unlock(
    profile: &mut AccountProfile,
    veh_type_cd: i32,
    unlock_idx: usize,
    catalog: &Catalog,
    _economics: &EconomicsConfig,
    _server_shop_rev: i64,
    now_utc: u32,
) -> Result<CmdOutcome, CmdError> {
    // 1. Source vehicle must be unlocked
    if !profile.unlocks.contains(&veh_type_cd) {
        return Err(CmdError::failure(format!(
            "source vehicle {veh_type_cd} is not unlocked"
        )));
    }
    let veh_def = catalog
        .vehicle_def(veh_type_cd)
        .ok_or_else(|| CmdError::failure(format!("vehicle {veh_type_cd} not found in catalog")))?;

    // 2. Validate unlock_idx
    if unlock_idx >= veh_def.unlocks_descrs.len() {
        return Err(CmdError::wrong_args(format!(
            "unlock_idx {unlock_idx} out of range ({})",
            veh_def.unlocks_descrs.len()
        )));
    }
    let entry = &veh_def.unlocks_descrs[unlock_idx];
    let target_cd = entry.target_cd;
    let cost = entry.xp_cost;

    // 3. Target not already unlocked
    if profile.unlocks.contains(&target_cd) {
        return Err(CmdError::failure(format!(
            "target {target_cd} is already unlocked"
        )));
    }

    // 4. Prerequisites must be unlocked
    for &prereq in &entry.prereq_cds {
        if !profile.unlocks.contains(&prereq) {
            return Err(CmdError::failure(format!(
                "prerequisite {prereq} is not unlocked"
            )));
        }
    }

    // 5. XP check: vehTypeXP first, then freeXP
    let avail_veh_xp = profile.veh_type_xp.get(&veh_type_cd).copied().unwrap_or(0) as i64;
    let total_xp = avail_veh_xp + profile.free_xp;
    if total_xp < cost {
        return Err(CmdError::failure(format!(
            "insufficient xp: needed {cost}, available {total_xp} (veh: {avail_veh_xp}, free: {})",
            profile.free_xp
        )));
    }

    let prev_rev = profile.rev;
    let mut diff = DiffBuilder::new();

    let take_veh_xp = avail_veh_xp.min(cost);
    let take_free_xp = cost - take_veh_xp;

    if take_veh_xp > 0 {
        let new_veh_xp = (avail_veh_xp - take_veh_xp) as i32;
        if new_veh_xp == 0 {
            profile.veh_type_xp.remove(&veh_type_cd);
            diff.veh_type_xp.insert(veh_type_cd, None);
        } else {
            profile.veh_type_xp.insert(veh_type_cd, new_veh_xp);
            diff.veh_type_xp.insert(veh_type_cd, Some(new_veh_xp));
        }
    }

    if take_free_xp > 0 {
        profile.free_xp -= take_free_xp;
        diff.free_xp = Some(profile.free_xp);
    }

    // Add target to unlocks
    profile.unlocks.insert(target_cd);
    diff.unlocks.insert(target_cd);

    // If target is a vehicle, expand its autounlocked_items and ensure VehicleDossier exists
    let mut resync_dossiers = false;
    let (target_type, _, _) = parse_int_comp_descr(target_cd);
    if target_type == ITEM_TYPE_VEHICLE {
        if let Some(target_veh_def) = catalog.vehicle_def(target_cd) {
            for &auto_cd in &target_veh_def.autounlocked_items {
                if profile.unlocks.insert(auto_cd) {
                    diff.unlocks.insert(auto_cd);
                }
            }
        }
        if !profile.vehicle_dossiers.contains_key(&target_cd) {
            let v_dossier = VehicleDossier::new().to_bytes();
            profile.vehicle_dossiers.insert(target_cd, (now_utc, v_dossier));
            resync_dossiers = true;
        }
    }

    // Recompute elite vehicles
    let newly_elite = recompute_elite_vehicles(profile, catalog);
    for cd in newly_elite {
        diff.elite_vehicles.insert(cd);
    }

    profile.rev += 1;

    Ok(CmdOutcome {
        result_id: RES_SUCCESS,
        prev_rev,
        rev: profile.rev,
        diff: diff.build(prev_rev, profile.rev),
        ext: None,
        resync_dossiers,
    })
}

/// Buy vehicle (`CMD_BUY_VEHICLE = 301`).
pub fn buy_vehicle(
    profile: &mut AccountProfile,
    client_shop_rev: i64,
    veh_type_cd: i32,
    flags: i32,
    tman_cost_idx: usize,
    catalog: &Catalog,
    economics: &EconomicsConfig,
    server_shop_rev: i64,
    now_utc: u32,
) -> Result<CmdOutcome, CmdError> {
    require_shop_rev(client_shop_rev, server_shop_rev)?;

    // 1. Validate vehicle exists and is unlocked & in shop
    let veh_def = catalog
        .vehicle_def(veh_type_cd)
        .ok_or_else(|| CmdError::failure("vehicle not found"))?;

    if !profile.unlocks.contains(&veh_type_cd) {
        return Err(CmdError::failure("vehicle is not unlocked"));
    }
    if !veh_def.show_in_shop {
        return Err(CmdError::failure("vehicle is not available in shop"));
    }

    // 2. Garage capacity
    if profile.vehicles.len() >= profile.slots as usize {
        return Err(CmdError::failure("no empty garage slots"));
    }

    // 3. Flags and tman cost validation
    let recruit_crew = (flags & BUY_VEHICLE_FLAG_CREW) != 0;
    let buy_shells = (flags & BUY_VEHICLE_FLAG_SHELLS) != 0;
    if recruit_crew && tman_cost_idx >= economics.tankman_cost.len() {
        return Err(CmdError::wrong_args(format!(
            "invalid tman_cost_idx {tman_cost_idx}"
        )));
    }

    // 4. Calculate total prices
    let (mut total_credits, mut total_gold) = veh_def.price;

    let tman_tier = if recruit_crew {
        let tier = &economics.tankman_cost[tman_cost_idx];
        let crew_count = veh_def.crew.len() as i64;
        total_credits += (tier.credits as i64) * crew_count;
        total_gold += (tier.gold as i64) * crew_count;
        Some(tier)
    } else {
        None
    };

    let mut shells = Vec::new();
    let stock_turret = &veh_def.turrets[0];
    let stock_gun = &stock_turret.guns[0];
    let turret_cd = stock_turret.component.comp_descr;
    let gun_cd = stock_gun.component.comp_descr;

    if buy_shells {
        for (shell_cd, count) in crate::item_defs::compute_default_ammo_for_gun(stock_gun) {
            if count > 0 {
                shells.push((shell_cd, count));
                if let Some(((c, g), _)) = get_item_price_and_shop_flag(catalog, shell_cd) {
                    total_credits = total_credits
                        .checked_add(
                            c.checked_mul(count as i64)
                                .ok_or_else(|| CmdError::failure("credits overflow"))?,
                        )
                        .ok_or_else(|| CmdError::failure("credits overflow"))?;
                    total_gold = total_gold
                        .checked_add(
                            g.checked_mul(count as i64)
                                .ok_or_else(|| CmdError::failure("gold overflow"))?,
                        )
                        .ok_or_else(|| CmdError::failure("gold overflow"))?;
                }
            }
        }
    }

    // Check currencies
    if profile.credits < total_credits || profile.gold < total_gold {
        return Err(CmdError::failure(format!(
            "insufficient currency: need ({} credits, {} gold), have ({} credits, {} gold)",
            total_credits, total_gold, profile.credits, profile.gold
        )));
    }

    let dossier_change_time = profile.next_dossier_change_time(now_utc);
    let prev_rev = profile.rev;
    profile.credits -= total_credits;
    profile.gold -= total_gold;

    let mut diff = DiffBuilder::new();
    diff.credits = Some(profile.credits);
    diff.gold = Some(profile.gold);

    // Allocate vehInvID
    let veh_inv_id = profile.next_inv_id;
    profile.next_inv_id += 1;

    // Build crew
    let mut crew = Vec::with_capacity(veh_def.crew.len());
    if let Some(tier) = tman_tier {
        let pool = catalog.extras.tankman_pools.get(&veh_def.nation_id);
        for crew_slot in &veh_def.crew {
            let tman_inv_id = profile.next_inv_id;
            profile.next_inv_id += 1;

            let seed = (tman_inv_id - 1) as usize;
            let fn_id = match pool {
                Some(p) if !p.first_names.is_empty() => p.first_names[seed % p.first_names.len()],
                _ => 1,
            };
            let ln_id = match pool {
                Some(p) if !p.last_names.is_empty() => p.last_names[seed % p.last_names.len()],
                _ => 1,
            };
            let ic_id = match pool {
                Some(p) if !p.icons.is_empty() => p.icons[seed % p.icons.len()],
                _ => 1,
            };

            let role_id = match crew_slot.primary_role.as_str() {
                "commander" => 1,
                "radioman" => 2,
                "driver" => 3,
                "gunner" => 4,
                "loader" => 5,
                _ => 1,
            };

            let tman_descr = TankmanDescr {
                nation_id: veh_def.nation_id,
                vehicle_type_id: veh_def.type_id as u8,
                role_id,
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
                dossier: TankmanDossier::new().to_bytes(),
            };

            let tman_bytes = tman_descr.to_bytes();
            profile.tankmen.insert(
                tman_inv_id,
                TankmanEntry {
                    comp_descr: tman_bytes.clone(),
                    vehicle_inv_id: Some(veh_inv_id),
                    change_time: dossier_change_time,
                },
            );
            diff.tman_comp_descr.insert(tman_inv_id, Some(tman_bytes));
            diff.tman_vehicle.insert(tman_inv_id, Some(veh_inv_id));
            crew.push(Some(tman_inv_id));
        }
    } else {
        crew = vec![None; veh_def.crew.len()];
    }

    let veh_descr = VehicleDescr::stock(veh_def)
        .map_err(|e| CmdError::failure(format!("failed to construct stock vehicle: {e:?}")))?;
    let comp_descr = veh_descr.to_bytes();
    let shells_layout = vec![((turret_cd, gun_cd), shells.clone())];

    let veh_entry = VehicleEntry {
        comp_descr: comp_descr.clone(),
        shells: shells.clone(),
        shells_layout: shells_layout.clone(),
        crew: crew.clone(),
        repair: None,
        eqs: [0, 0, 0],
        eqs_layout: [0, 0, 0],
        settings: 0,
        lock: 0,
    };

    profile.vehicles.insert(veh_inv_id, veh_entry);

    diff.veh_comp_descr.insert(veh_inv_id, Some(comp_descr));
    diff.veh_shells_layout.insert(veh_inv_id, Some(shells_layout));
    diff.veh_shells.insert(veh_inv_id, Some(shells));
    diff.veh_crew.insert(veh_inv_id, Some(crew));
    diff.veh_repair.insert(veh_inv_id, None);
    diff.veh_eqs_layout.insert(veh_inv_id, Some([0, 0, 0]));
    diff.veh_eqs.insert(veh_inv_id, Some([0, 0, 0]));
    diff.veh_settings.insert(veh_inv_id, Some(0));
    diff.veh_lock.insert(veh_inv_id, Some(0));

    // Ensure VehicleDossier exists
    let mut is_new_vehicle_dossier = false;
    if !profile.vehicle_dossiers.contains_key(&veh_type_cd) {
        let v_dossier = VehicleDossier::new().to_bytes();
        profile
            .vehicle_dossiers
            .insert(veh_type_cd, (dossier_change_time, v_dossier));
        is_new_vehicle_dossier = true;
    }
    let resync_dossiers = recruit_crew || is_new_vehicle_dossier;

    profile.rev += 1;

    Ok(CmdOutcome {
        result_id: RES_SUCCESS,
        prev_rev,
        rev: profile.rev,
        diff: diff.build(prev_rev, profile.rev),
        ext: None,
        resync_dossiers,
    })
}

/// Sell vehicle (`CMD_SELL_VEHICLE = 304`).
pub fn sell_vehicle(
    profile: &mut AccountProfile,
    client_shop_rev: i64,
    veh_inv_id: i64,
    flags: i32,
    catalog: &Catalog,
    economics: &EconomicsConfig,
    server_shop_rev: i64,
    _now_utc: u32,
) -> Result<CmdOutcome, CmdError> {
    require_shop_rev(client_shop_rev, server_shop_rev)?;

    let veh_entry = profile
        .vehicles
        .get(&veh_inv_id)
        .ok_or_else(|| CmdError::failure("vehicle not found"))?;

    // Cannot sell damaged vehicle
    if veh_entry.repair.is_some() {
        return Err(CmdError::failure("cannot sell damaged vehicle"));
    }
    // Cannot sell locked vehicle
    if veh_entry.lock != 0 {
        return Err(CmdError::failure("vehicle is locked"));
    }

    let veh_descr = VehicleDescr::from_bytes(&veh_entry.comp_descr)
        .map_err(|e| CmdError::failure(format!("invalid vehicle descriptor: {e:?}")))?;
    let veh_def = catalog
        .vehicle_def_by_id(veh_descr.nation_id, veh_descr.type_id as u16)
        .ok_or_else(|| CmdError::failure("vehicle definition not found"))?;

    let dismiss_crew = (flags & SELL_VEHICLE_FLAG_CREW) != 0;
    let sell_shells = (flags & SELL_VEHICLE_FLAG_SHELLS) != 0;
    let sell_eqs = (flags & SELL_VEHICLE_FLAG_EQS) != 0;

    let seated_tman_ids: Vec<i64> = veh_entry.crew.iter().filter_map(|&c| c).collect();

    // If not dismissing crew, check barracks capacity
    if !dismiss_crew && !seated_tman_ids.is_empty() {
        let unseated_tman_count = profile
            .tankmen
            .values()
            .filter(|t| t.vehicle_inv_id.is_none())
            .count();
        let available_berths = (profile.berths as usize).saturating_sub(unseated_tman_count);
        if available_berths < seated_tman_ids.len() {
            return Err(CmdError::failure(format!(
                "insufficient barracks berths: available {available_berths}, need {}",
                seated_tman_ids.len()
            )));
        }
    }

    // Calculate vehicle sell price matching Shop.__getVehiclePriceFromCache
    let mut net_price = veh_def.price;
    let (def_comps, inst_comps, opt_devices) = veh_descr.get_devices(veh_def);

    for (def_cd, inst_cd) in def_comps.iter().zip(inst_comps.iter()) {
        if def_cd != inst_cd {
            let def_p = get_item_price_and_shop_flag(catalog, *def_cd).map(|(p, _)| p).unwrap_or((0, 0));
            let inst_p = get_item_price_and_shop_flag(catalog, *inst_cd).map(|(p, _)| p).unwrap_or((0, 0));
            net_price.0 = net_price.0 - def_p.0 + inst_p.0;
            net_price.1 = net_price.1 - def_p.1 + inst_p.1;
        }
    }

    for opt_cd in opt_devices {
        let opt_p = get_item_price_and_shop_flag(catalog, opt_cd).map(|(p, _)| p).unwrap_or((0, 0));
        net_price.0 += opt_p.0;
        net_price.1 += opt_p.1;
    }

    let base_sell_credits = economics.sell_price_credits(net_price.0, net_price.1).max(0);

    let mut shells_sell_credits = 0i64;
    for &(shell_cd, count) in &veh_entry.shells {
        if count > 0 && sell_shells {
            let shell_p = get_item_price_and_shop_flag(catalog, shell_cd).map(|(p, _)| p).unwrap_or((0, 0));
            let unit_sell = economics.sell_price_credits(shell_p.0, shell_p.1);
            shells_sell_credits += unit_sell * (count as i64);
        }
    }

    let mut eqs_sell_credits = 0i64;
    for &eq_cd in &veh_entry.eqs {
        if eq_cd != 0 && sell_eqs {
            let eq_p = get_item_price_and_shop_flag(catalog, eq_cd).map(|(p, _)| p).unwrap_or((0, 0));
            let eq_sell = economics.sell_price_credits(eq_p.0, eq_p.1);
            eqs_sell_credits += eq_sell;
        }
    }

    let total_credits_gain = base_sell_credits
        .checked_add(shells_sell_credits)
        .and_then(|v| v.checked_add(eqs_sell_credits))
        .ok_or_else(|| CmdError::failure("sell credits overflow"))?;
    let new_credits = profile
        .credits
        .checked_add(total_credits_gain)
        .ok_or_else(|| CmdError::failure("credits overflow"))?;

    // Validate depot overflow before mutating profile.
    if !sell_shells {
        for &(shell_cd, count) in &veh_entry.shells {
            if count > 0 {
                let cur = profile.items.get(&shell_cd).copied().unwrap_or(0);
                cur.checked_add(count)
                    .ok_or_else(|| CmdError::failure("depot shell count overflow"))?;
            }
        }
    }
    if !sell_eqs {
        for &eq_cd in &veh_entry.eqs {
            if eq_cd != 0 {
                let cur = profile.items.get(&eq_cd).copied().unwrap_or(0);
                cur.checked_add(1)
                    .ok_or_else(|| CmdError::failure("depot equipment count overflow"))?;
            }
        }
    }

    let prev_rev = profile.rev;
    profile.credits = new_credits;

    let mut diff = DiffBuilder::new();
    diff.credits = Some(profile.credits);

    // Remove vehicle
    let old_veh = profile.vehicles.remove(&veh_inv_id).unwrap();
    diff.delete_vehicles.insert(veh_inv_id);

    // Crew handling
    if dismiss_crew {
        for tman_id in seated_tman_ids {
            profile.tankmen.remove(&tman_id);
            diff.delete_tankmen.insert(tman_id);
        }
    } else {
        for tman_id in seated_tman_ids {
            if let Some(tman) = profile.tankmen.get_mut(&tman_id) {
                tman.vehicle_inv_id = None;
                diff.tman_vehicle.insert(tman_id, None);
            }
        }
    }

    // Shells return to depot if not sold
    if !sell_shells {
        for &(shell_cd, count) in &old_veh.shells {
            if count > 0 {
                let cur = profile.items.entry(shell_cd).or_insert(0);
                *cur += count;
                diff.depot_items.entry(ITEM_TYPE_SHELL).or_default().insert(shell_cd, Some(*cur));
            }
        }
    }

    // Equipments return to depot if not sold
    if !sell_eqs {
        for &eq_cd in &old_veh.eqs {
            if eq_cd != 0 {
                let cur = profile.items.entry(eq_cd).or_insert(0);
                *cur += 1;
                diff.depot_items.entry(ITEM_TYPE_EQUIPMENT).or_default().insert(eq_cd, Some(*cur));
            }
        }
    }

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

/// Buy item into depot (`CMD_BUY_ITEM = 302`).
pub fn buy_item(
    profile: &mut AccountProfile,
    client_shop_rev: i64,
    item_cd: i32,
    count: i32,
    catalog: &Catalog,
    _economics: &EconomicsConfig,
    server_shop_rev: i64,
    _now_utc: u32,
) -> Result<CmdOutcome, CmdError> {
    require_shop_rev(client_shop_rev, server_shop_rev)?;
    if count <= 0 {
        return Err(CmdError::wrong_args("count must be positive"));
    }
    let (item_type, _, _) = parse_int_comp_descr(item_cd);
    if !matches!(item_type, 2..=7 | 9..=11) {
        return Err(CmdError::wrong_args(format!(
            "invalid item type {item_type} for buy_item"
        )));
    }

    // Modules 2..=7 must be unlocked
    if (2..=7).contains(&item_type) && !profile.unlocks.contains(&item_cd) {
        return Err(CmdError::failure(format!(
            "module {item_cd} is not unlocked"
        )));
    }

    let ((buy_credits, buy_gold), show_in_shop) = get_item_price_and_shop_flag(catalog, item_cd)
        .ok_or_else(|| CmdError::failure(format!("item {item_cd} not found in catalog")))?;
    if !show_in_shop {
        return Err(CmdError::failure(format!("item {item_cd} not in shop")));
    }

    let total_credits = buy_credits
        .checked_mul(count as i64)
        .ok_or_else(|| CmdError::failure("credits calculation overflow"))?;
    let total_gold = buy_gold
        .checked_mul(count as i64)
        .ok_or_else(|| CmdError::failure("gold calculation overflow"))?;

    let new_credits = profile
        .credits
        .checked_sub(total_credits)
        .ok_or_else(|| CmdError::failure(format!(
            "insufficient currency for item: need ({total_credits} cr, {total_gold} g)"
        )))?;
    let new_gold = profile
        .gold
        .checked_sub(total_gold)
        .ok_or_else(|| CmdError::failure(format!(
            "insufficient currency for item: need ({total_credits} cr, {total_gold} g)"
        )))?;

    let cur = profile.items.get(&item_cd).copied().unwrap_or(0);
    let new_count = cur
        .checked_add(count)
        .ok_or_else(|| CmdError::failure("depot count overflow"))?;

    let prev_rev = profile.rev;
    profile.credits = new_credits;
    profile.gold = new_gold;
    profile.items.insert(item_cd, new_count);

    let mut diff = DiffBuilder::new();
    diff.credits = Some(profile.credits);
    diff.gold = Some(profile.gold);
    diff.depot_items.entry(item_type).or_default().insert(item_cd, Some(new_count));

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

/// Sell item from depot (`CMD_SELL_ITEM = 305`).
pub fn sell_item(
    profile: &mut AccountProfile,
    client_shop_rev: i64,
    item_type_idx: u8,
    item_cd: i32,
    count: i32,
    catalog: &Catalog,
    economics: &EconomicsConfig,
    server_shop_rev: i64,
    _now_utc: u32,
) -> Result<CmdOutcome, CmdError> {
    require_shop_rev(client_shop_rev, server_shop_rev)?;
    if count <= 0 {
        return Err(CmdError::wrong_args("count must be positive"));
    }
    let (item_type, _, _) = parse_int_comp_descr(item_cd);
    if item_type != item_type_idx || !matches!(item_type, 2..=7 | 9..=11) {
        return Err(CmdError::wrong_args(format!(
            "item type mismatch: expected {item_type_idx}, got {item_type}"
        )));
    }

    let cur_count = profile.items.get(&item_cd).copied().unwrap_or(0);
    if cur_count < count {
        return Err(CmdError::failure(format!(
            "insufficient items in depot: available {cur_count}, trying to sell {count}"
        )));
    }

    let ((buy_credits, buy_gold), _) = get_item_price_and_shop_flag(catalog, item_cd)
        .ok_or_else(|| CmdError::failure(format!("item {item_cd} not found in catalog")))?;

    let unit_sell = economics.sell_price_credits(buy_credits, buy_gold);
    let total_credits_gain = unit_sell
        .checked_mul(count as i64)
        .ok_or_else(|| CmdError::failure("credits calculation overflow"))?;

    let new_credits = profile
        .credits
        .checked_add(total_credits_gain)
        .ok_or_else(|| CmdError::failure("credits overflow"))?;

    let remaining = cur_count - count;

    let prev_rev = profile.rev;
    profile.credits = new_credits;

    let mut diff = DiffBuilder::new();
    diff.credits = Some(profile.credits);

    if remaining == 0 {
        profile.items.remove(&item_cd);
        diff.depot_items.entry(item_type).or_default().insert(item_cd, None);
    } else {
        profile.items.insert(item_cd, remaining);
        diff.depot_items.entry(item_type).or_default().insert(item_cd, Some(remaining));
    }

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
    use crate::descr::ITEM_TYPE_SHELL;
    use crate::economics::EconomicsConfig;
    use crate::ops::{
        AccountEngine, DiffBuilder, EngineCmd, BUY_VEHICLE_FLAG_CREW,
        BUY_VEHICLE_FLAG_SHELLS, RES_FAILURE, RES_SHOP_DESYNC, RES_SUCCESS, RES_WRONG_ARGS,
        SELL_VEHICLE_FLAG_CREW, SELL_VEHICLE_FLAG_NONE, SELL_VEHICLE_FLAG_SHELLS,
    };
    use crate::pickle::Value;
    use crate::testutil;

    const SERVER_SHOP_REV: i64 = 42;
    const NOW_UTC: u32 = 1_700_000_000;

    fn load_real_catalog() -> Option<Catalog> {
        let dir = testutil::game_dir()?;
        crate::catalog::load(&dir).ok()
    }

    fn make_test_profile(preset: Preset) -> Option<(Catalog, EconomicsConfig, AccountProfile)> {
        let catalog = load_real_catalog()?;
        let economics = EconomicsConfig::default();
        let profile = AccountProfile::new_with_preset(
            1001,
            "TestCommander",
            preset,
            &catalog,
            &economics,
            NOW_UTC,
        )
        .ok()?;
        Some((catalog, economics, profile))
    }

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

    #[test]
    fn test_diff_builder_basic_and_deletions() {
        let mut builder = DiffBuilder::new();
        builder.credits = Some(15_000);
        builder.gold = Some(500);

        // Deletions across all 9 vehicle keys
        builder.delete_vehicles.insert(10);

        // Deletion in tankman
        builder.delete_tankmen.insert(20);

        // Depot item updates & deletions
        builder
            .depot_items
            .entry(ITEM_TYPE_SHELL)
            .or_default()
            .insert(1001, Some(25));
        builder
            .depot_items
            .entry(ITEM_TYPE_SHELL)
            .or_default()
            .insert(1002, None); // deleted!

        // Unlocks addition & replacement
        builder.unlocks.insert(501);
        builder.elite_vehicles.insert(502);

        let val = builder.build(1, 2);
        assert_eq!(dict_get(&val, "prevRev").and_then(|v| v.as_int()), Some(1));
        assert_eq!(dict_get(&val, "rev").and_then(|v| v.as_int()), Some(2));

        let stats = dict_get(&val, "stats").expect("stats entry");
        assert_eq!(dict_get(stats, "credits").and_then(|v| v.as_int()), Some(15_000));
        assert_eq!(dict_get(stats, "gold").and_then(|v| v.as_int()), Some(500));

        let inv = dict_get(&val, "inventory").expect("inventory entry");

        // Vehicle 10 should have Value::None in all 9 subdicts
        let veh_sec = dict_get_int(inv, 1).expect("vehicle section");
        let subkeys = [
            "compDescr",
            "shellsLayout",
            "shells",
            "crew",
            "repair",
            "eqsLayout",
            "eqs",
            "settings",
            "lock",
        ];
        for k in subkeys {
            let sub = dict_get(veh_sec, k).unwrap_or_else(|| panic!("missing {k}"));
            assert_eq!(
                dict_get_int(sub, 10),
                Some(&Value::None),
                "subdict {k} must contain veh 10 mapped to Value::None"
            );
        }

        // Tankman 20 should have Value::None in compDescr and vehicle
        let tman_sec = dict_get_int(inv, 8).expect("tankman section");
        let tman_comp = dict_get(tman_sec, "compDescr").expect("compDescr");
        assert_eq!(dict_get_int(tman_comp, 20), Some(&Value::None));
        let tman_veh = dict_get(tman_sec, "vehicle").expect("vehicle");
        assert_eq!(dict_get_int(tman_veh, 20), Some(&Value::None));

        // Depot items
        let shell_sec = dict_get_int(inv, ITEM_TYPE_SHELL as i64).expect("shell section");
        assert_eq!(dict_get_int(shell_sec, 1001).and_then(|v| v.as_int()), Some(25));
        assert_eq!(dict_get_int(shell_sec, 1002), Some(&Value::None));
    }

    #[test]
    fn test_cmd_buy_slot() {
        let Some((catalog, economics, mut profile)) = make_test_profile(Preset::Retail) else {
            return;
        };
        let initial_rev = profile.rev;
        let initial_slots = profile.slots;

        // 1. Insufficient gold
        profile.gold = 100;
        let res = AccountEngine::apply(
            &mut profile,
            &EngineCmd::BuySlot {
                shop_rev: SERVER_SHOP_REV,
            },
            &catalog,
            &economics,
            SERVER_SHOP_REV,
            NOW_UTC,
        );
        assert!(res.is_err());
        let err = res.unwrap_err();
        assert_eq!(err.result_id, RES_FAILURE);
        assert_eq!(profile.rev, initial_rev);
        assert_eq!(profile.slots, initial_slots);

        // 2. Shop desync
        profile.gold = 500;
        let res = AccountEngine::apply(
            &mut profile,
            &EngineCmd::BuySlot { shop_rev: 999 },
            &catalog,
            &economics,
            SERVER_SHOP_REV,
            NOW_UTC,
        );
        let err = res.unwrap_err();
        assert_eq!(err.result_id, RES_SHOP_DESYNC);
        assert_eq!(
            err.ext
                .as_ref()
                .and_then(|x| dict_get(x, "shopRev"))
                .and_then(|v| v.as_int()),
            Some(SERVER_SHOP_REV)
        );
        assert_eq!(profile.rev, initial_rev);

        // 3. Success
        let outcome = AccountEngine::apply(
            &mut profile,
            &EngineCmd::BuySlot {
                shop_rev: SERVER_SHOP_REV,
            },
            &catalog,
            &economics,
            SERVER_SHOP_REV,
            NOW_UTC,
        )
        .expect("buy slot success");
        assert_eq!(outcome.result_id, RES_SUCCESS);
        assert_eq!(profile.slots, initial_slots + 1);
        assert_eq!(profile.gold, 200); // 500 - 300
        assert_eq!(profile.rev, initial_rev + 1);
        assert_eq!(dict_get(&outcome.diff, "rev").and_then(|v| v.as_int()), Some(profile.rev));
        let stats = dict_get(&outcome.diff, "stats").expect("stats");
        assert_eq!(dict_get(stats, "slots").and_then(|v| v.as_int()), Some(profile.slots as i64));
        assert_eq!(dict_get(stats, "gold").and_then(|v| v.as_int()), Some(200));
    }

    #[test]
    fn test_cmd_buy_berths() {
        let Some((catalog, economics, mut profile)) = make_test_profile(Preset::Retail) else {
            return;
        };
        let initial_rev = profile.rev;
        let initial_berths = profile.berths;
        profile.gold = 1000;

        // 1. Success
        let outcome = AccountEngine::apply(
            &mut profile,
            &EngineCmd::BuyBerths {
                shop_rev: SERVER_SHOP_REV,
            },
            &catalog,
            &economics,
            SERVER_SHOP_REV,
            NOW_UTC,
        )
        .expect("buy berths success");

        assert_eq!(outcome.result_id, RES_SUCCESS);
        assert_eq!(profile.berths, initial_berths + 16);
        assert_eq!(profile.gold, 700); // 1000 - 300
        assert_eq!(profile.rev, initial_rev + 1);
        let stats = dict_get(&outcome.diff, "stats").expect("stats");
        assert_eq!(dict_get(stats, "berths").and_then(|v| v.as_int()), Some(profile.berths as i64));
        assert_eq!(dict_get(stats, "gold").and_then(|v| v.as_int()), Some(700));

        // 2. Insufficient gold
        profile.gold = 100;
        let res = AccountEngine::apply(
            &mut profile,
            &EngineCmd::BuyBerths {
                shop_rev: SERVER_SHOP_REV,
            },
            &catalog,
            &economics,
            SERVER_SHOP_REV,
            NOW_UTC,
        );
        assert_eq!(res.unwrap_err().result_id, RES_FAILURE);
    }

    #[test]
    fn test_cmd_exchange() {
        let Some((catalog, economics, mut profile)) = make_test_profile(Preset::Retail) else {
            return;
        };
        let initial_rev = profile.rev;
        let initial_credits = profile.credits;
        profile.gold = 100;

        // 1. Wrong args (gold <= 0)
        let res = AccountEngine::apply(
            &mut profile,
            &EngineCmd::Exchange {
                shop_rev: SERVER_SHOP_REV,
                gold: 0,
            },
            &catalog,
            &economics,
            SERVER_SHOP_REV,
            NOW_UTC,
        );
        assert_eq!(res.unwrap_err().result_id, RES_WRONG_ARGS);
        assert_eq!(profile.rev, initial_rev);

        // 2. Insufficient gold
        let res = AccountEngine::apply(
            &mut profile,
            &EngineCmd::Exchange {
                shop_rev: SERVER_SHOP_REV,
                gold: 200,
            },
            &catalog,
            &economics,
            SERVER_SHOP_REV,
            NOW_UTC,
        );
        assert_eq!(res.unwrap_err().result_id, RES_FAILURE);
        assert_eq!(profile.rev, initial_rev);

        // 3. Success: 50 gold * 400 = 20_000 credits
        let outcome = AccountEngine::apply(
            &mut profile,
            &EngineCmd::Exchange {
                shop_rev: SERVER_SHOP_REV,
                gold: 50,
            },
            &catalog,
            &economics,
            SERVER_SHOP_REV,
            NOW_UTC,
        )
        .expect("exchange success");
        assert_eq!(outcome.result_id, RES_SUCCESS);
        assert_eq!(profile.gold, 50);
        assert_eq!(profile.credits, initial_credits + 20_000);
        assert_eq!(profile.rev, initial_rev + 1);
        let stats = dict_get(&outcome.diff, "stats").expect("stats");
        assert_eq!(dict_get(stats, "gold").and_then(|v| v.as_int()), Some(50));
        assert_eq!(dict_get(stats, "credits").and_then(|v| v.as_int()), Some(profile.credits));
    }

    #[test]
    fn test_cmd_premium() {
        let Some((catalog, economics, mut profile)) = make_test_profile(Preset::Retail) else {
            return;
        };
        let initial_rev = profile.rev;
        profile.gold = 3000;

        // 1. Invalid days
        let res = AccountEngine::apply(
            &mut profile,
            &EngineCmd::Premium {
                shop_rev: SERVER_SHOP_REV,
                days: 5, // invalid tier
            },
            &catalog,
            &economics,
            SERVER_SHOP_REV,
            NOW_UTC,
        );
        assert_eq!(res.unwrap_err().result_id, RES_WRONG_ARGS);

        // 2. Success: 30 days = 2500 gold
        let outcome = AccountEngine::apply(
            &mut profile,
            &EngineCmd::Premium {
                shop_rev: SERVER_SHOP_REV,
                days: 30,
            },
            &catalog,
            &economics,
            SERVER_SHOP_REV,
            NOW_UTC,
        )
        .expect("premium success");
        assert_eq!(outcome.result_id, RES_SUCCESS);
        assert_eq!(profile.gold, 500); // 3000 - 2500
        assert_eq!(profile.attrs & (1u64 << 32), 1u64 << 32);
        assert!(profile.premium_expiry_time >= NOW_UTC + 30 * 86400);
        assert_eq!(profile.rev, initial_rev + 1);

        let acct = dict_get(&outcome.diff, "account").expect("account");
        assert_eq!(dict_get(acct, "attrs").and_then(|v| v.as_int()), Some(profile.attrs as i64));
        assert_eq!(
            dict_get(acct, "premiumExpiryTime").and_then(|v| v.as_int()),
            Some(profile.premium_expiry_time as i64)
        );
    }

    #[test]
    fn test_cmd_free_xp_conv() {
        let Some((catalog, economics, mut profile)) = make_test_profile(Preset::Sandbox) else {
            return;
        };
        let initial_rev = profile.rev;
        profile.gold = 100;
        profile.free_xp = 0;

        // Pick an elite vehicle that has vehTypeXP
        let (&elite_veh_cd, &avail_xp) = profile
            .veh_type_xp
            .iter()
            .find(|(cd, _)| profile.elite_vehicles.contains(cd))
            .expect("must have an elite tank with XP in sandbox");
        assert!(avail_xp >= 100);

        // 1. XP not divisible by 25
        let res = AccountEngine::apply(
            &mut profile,
            &EngineCmd::FreeXpConv {
                shop_rev: SERVER_SHOP_REV,
                xp: 30,
                veh_cds: vec![elite_veh_cd],
            },
            &catalog,
            &economics,
            SERVER_SHOP_REV,
            NOW_UTC,
        );
        assert_eq!(res.unwrap_err().result_id, RES_WRONG_ARGS);

        // 2. Duplicate vehicles in list
        let res = AccountEngine::apply(
            &mut profile,
            &EngineCmd::FreeXpConv {
                shop_rev: SERVER_SHOP_REV,
                xp: 50,
                veh_cds: vec![elite_veh_cd, elite_veh_cd],
            },
            &catalog,
            &economics,
            SERVER_SHOP_REV,
            NOW_UTC,
        );
        assert_eq!(res.unwrap_err().result_id, RES_WRONG_ARGS);

        // 3. Success: convert 50 XP -> costs 2 gold
        let outcome = AccountEngine::apply(
            &mut profile,
            &EngineCmd::FreeXpConv {
                shop_rev: SERVER_SHOP_REV,
                xp: 50,
                veh_cds: vec![elite_veh_cd],
            },
            &catalog,
            &economics,
            SERVER_SHOP_REV,
            NOW_UTC,
        )
        .expect("free xp conv success");
        assert_eq!(outcome.result_id, RES_SUCCESS);
        assert_eq!(profile.gold, 98); // 100 - 2
        assert_eq!(profile.free_xp, 50);
        assert_eq!(
            profile.veh_type_xp.get(&elite_veh_cd).copied().unwrap_or(0),
            avail_xp - 50
        );
        assert_eq!(profile.rev, initial_rev + 1);

        let stats = dict_get(&outcome.diff, "stats").expect("stats");
        assert_eq!(dict_get(stats, "gold").and_then(|v| v.as_int()), Some(98));
        assert_eq!(dict_get(stats, "freeXP").and_then(|v| v.as_int()), Some(50));
        let vxp = dict_get(stats, "vehTypeXP").expect("vehTypeXP");
        assert_eq!(
            dict_get_int(vxp, elite_veh_cd as i64).and_then(|v| v.as_int()),
            Some((avail_xp - 50) as i64)
        );
    }

    #[test]
    fn test_cmd_unlock_module_and_vehicle_auto_elite() {
        let Some((catalog, economics, mut profile)) = make_test_profile(Preset::Retail) else {
            return;
        };

        // Find starter tank ussr:MS-1
        let ms1_def = catalog
            .vehicle_defs
            .values()
            .find(|v| v.nation_id == 0 && v.name.eq_ignore_ascii_case("MS-1"))
            .expect("MS-1 exists");
        let ms1_cd = ms1_def.veh_type_comp_descr;

        // Give plenty of free XP
        profile.free_xp = 1_000_000;
        let initial_rev = profile.rev;

        assert!(!ms1_def.unlocks_descrs.is_empty());

        // Unlock first unlockable entry
        let u0 = &ms1_def.unlocks_descrs[0];
        assert!(!profile.unlocks.contains(&u0.target_cd));

        let outcome = AccountEngine::apply(
            &mut profile,
            &EngineCmd::Unlock {
                veh_type_cd: ms1_cd,
                unlock_idx: 0,
            },
            &catalog,
            &economics,
            SERVER_SHOP_REV,
            NOW_UTC,
        )
        .expect("unlock first entry success");

        assert_eq!(outcome.result_id, RES_SUCCESS);
        assert!(profile.unlocks.contains(&u0.target_cd));
        assert_eq!(profile.rev, initial_rev + 1);

        // Attempting to unlock again should fail
        let res = AccountEngine::apply(
            &mut profile,
            &EngineCmd::Unlock {
                veh_type_cd: ms1_cd,
                unlock_idx: 0,
            },
            &catalog,
            &economics,
            SERVER_SHOP_REV,
            NOW_UTC,
        );
        assert_eq!(res.unwrap_err().result_id, RES_FAILURE);

        // Unlock all remaining unlocks on MS-1 in dependency order to test auto-elite promotion
        let mut progress = true;
        while progress {
            progress = false;
            for idx in 0..ms1_def.unlocks_descrs.len() {
                let u = &ms1_def.unlocks_descrs[idx];
                if !profile.unlocks.contains(&u.target_cd) {
                    if AccountEngine::apply(
                        &mut profile,
                        &EngineCmd::Unlock {
                            veh_type_cd: ms1_cd,
                            unlock_idx: idx,
                        },
                        &catalog,
                        &economics,
                        SERVER_SHOP_REV,
                        NOW_UTC,
                    )
                    .is_ok()
                    {
                        progress = true;
                    }
                }
            }
        }

        // Verify all targets are indeed in profile.unlocks
        for u in &ms1_def.unlocks_descrs {
            assert!(
                profile.unlocks.contains(&u.target_cd),
                "target {} must be unlocked",
                u.target_cd
            );
        }

        // MS-1 should now be elite!
        assert!(
            profile.elite_vehicles.contains(&ms1_cd),
            "source tank must be promoted to elite once all its unlocksDescrs are unlocked"
        );
    }

    #[test]
    fn test_cmd_buy_vehicle() {
        let Some((catalog, economics, mut profile)) = make_test_profile(Preset::Retail) else {
            return;
        };

        // Give plenty of credits & gold
        profile.credits = 10_000_000;
        profile.gold = 10_000;
        profile.slots = 10;
        let initial_rev = profile.rev;
        let initial_veh_count = profile.vehicles.len();

        // Unlock a vehicle to buy: ussr:BT-2
        let bt2_def = catalog
            .vehicle_defs
            .values()
            .find(|v| v.nation_id == 0 && v.name.eq_ignore_ascii_case("BT-2"))
            .expect("BT-2 exists");
        let bt2_cd = bt2_def.veh_type_comp_descr;
        profile.unlocks.insert(bt2_cd);

        // Buy BT-2 with CREW (tier 0 = 50%) and SHELLS
        let outcome = AccountEngine::apply(
            &mut profile,
            &EngineCmd::BuyVehicle {
                shop_rev: SERVER_SHOP_REV,
                veh_type_cd: bt2_cd,
                flags: BUY_VEHICLE_FLAG_CREW | BUY_VEHICLE_FLAG_SHELLS,
                tman_cost_idx: 0,
            },
            &catalog,
            &economics,
            SERVER_SHOP_REV,
            NOW_UTC,
        )
        .expect("buy vehicle success");

        assert_eq!(outcome.result_id, RES_SUCCESS);
        assert_eq!(outcome.resync_dossiers, true);
        assert_eq!(profile.vehicles.len(), initial_veh_count + 1);
        assert_eq!(profile.rev, initial_rev + 1);

        // Find the new vehicle in profile
        let (_new_veh_inv_id, new_veh) = profile
            .vehicles
            .iter()
            .find(|(&id, _)| id >= 4)
            .expect("new vehicle found");

        assert!(!new_veh.shells.is_empty(), "shells should be loaded");
        assert_eq!(new_veh.crew.len(), bt2_def.crew.len());
        for &tman_id in &new_veh.crew {
            assert!(tman_id.is_some(), "all crew slots should be populated");
        }
        assert!(profile.vehicle_dossiers.contains_key(&bt2_cd));
    }

    #[test]
    fn test_cmd_sell_vehicle() {
        let Some((catalog, economics, mut profile)) = make_test_profile(Preset::Retail) else {
            return;
        };

        // Pick starter tank 1
        let veh_inv_id = 1;
        assert!(profile.vehicles.contains_key(&veh_inv_id));
        let initial_credits = profile.credits;
        let initial_rev = profile.rev;

        // 1. Damaged vehicle cannot be sold
        profile.vehicles.get_mut(&veh_inv_id).unwrap().repair = Some((100, 50));
        let res = AccountEngine::apply(
            &mut profile,
            &EngineCmd::SellVehicle {
                shop_rev: SERVER_SHOP_REV,
                veh_inv_id,
                flags: SELL_VEHICLE_FLAG_NONE,
            },
            &catalog,
            &economics,
            SERVER_SHOP_REV,
            NOW_UTC,
        );
        assert_eq!(res.unwrap_err().result_id, RES_FAILURE);
        profile.vehicles.get_mut(&veh_inv_id).unwrap().repair = None;

        // 2. Successful sale dismissing crew and selling shells
        let outcome = AccountEngine::apply(
            &mut profile,
            &EngineCmd::SellVehicle {
                shop_rev: SERVER_SHOP_REV,
                veh_inv_id,
                flags: SELL_VEHICLE_FLAG_CREW | SELL_VEHICLE_FLAG_SHELLS,
            },
            &catalog,
            &economics,
            SERVER_SHOP_REV,
            NOW_UTC,
        )
        .expect("sell vehicle success");

        assert_eq!(outcome.result_id, RES_SUCCESS);
        assert!(!profile.vehicles.contains_key(&veh_inv_id));
        assert!(profile.credits >= initial_credits);
        assert_eq!(profile.rev, initial_rev + 1);

        // Diff must contain Value::None across all 9 vehicle keys
        let inv = dict_get(&outcome.diff, "inventory").expect("inventory");
        let veh_sec = dict_get_int(inv, 1).expect("veh sec");
        for k in ["compDescr", "shellsLayout", "shells", "crew", "repair", "eqsLayout", "eqs", "settings", "lock"] {
            let sub = dict_get(veh_sec, k).unwrap();
            assert_eq!(dict_get_int(sub, veh_inv_id), Some(&Value::None));
        }
    }

    #[test]
    fn test_cmd_buy_item_and_sell_item() {
        let Some((catalog, economics, mut profile)) = make_test_profile(Preset::Retail) else {
            return;
        };
        profile.credits = 1_000_000;
        let initial_rev = profile.rev;

        // Pick a shell from catalog that has price
        let (&(_, _, shell_cd_i64), _) = catalog
            .extras
            .shop_items
            .iter()
            .find(|(&(nation_id, item_type, _), price)| item_type == 10 && !price.hidden && nation_id == 0)
            .expect("shell found in shop_items");
        let shell_cd = shell_cd_i64 as i32;

        let buy_count = 10;
        let outcome = AccountEngine::apply(
            &mut profile,
            &EngineCmd::BuyItem {
                shop_rev: SERVER_SHOP_REV,
                item_cd: shell_cd,
                count: buy_count,
            },
            &catalog,
            &economics,
            SERVER_SHOP_REV,
            NOW_UTC,
        )
        .expect("buy item success");

        assert_eq!(outcome.result_id, RES_SUCCESS);
        assert_eq!(profile.items.get(&shell_cd), Some(&buy_count));
        assert_eq!(profile.rev, initial_rev + 1);

        // Sell all 10 shells
        let outcome2 = AccountEngine::apply(
            &mut profile,
            &EngineCmd::SellItem {
                shop_rev: SERVER_SHOP_REV,
                item_type_idx: ITEM_TYPE_SHELL,
                item_cd: shell_cd,
                count: buy_count,
            },
            &catalog,
            &economics,
            SERVER_SHOP_REV,
            NOW_UTC,
        )
        .expect("sell item success");

        assert_eq!(outcome2.result_id, RES_SUCCESS);
        assert!(!profile.items.contains_key(&shell_cd));
        assert_eq!(profile.rev, initial_rev + 2);

        // In diff, count reaching 0 must emit Value::None!
        let inv2 = dict_get(&outcome2.diff, "inventory").expect("inventory");
        let shell_sec = dict_get_int(inv2, ITEM_TYPE_SHELL as i64).expect("shell sec");
        assert_eq!(dict_get_int(shell_sec, shell_cd as i64), Some(&Value::None));
    }

    #[test]
    fn test_diff_builder_unlocks_r_and_elite_vehicles_r() {
        let mut builder = DiffBuilder::new();
        let mut unlocks_r = BTreeSet::new();
        unlocks_r.insert(101);
        unlocks_r.insert(102);
        builder.unlocks_r = Some(unlocks_r);

        let mut elite_r = BTreeSet::new();
        elite_r.insert(201);
        builder.elite_vehicles_r = Some(elite_r);

        let val = builder.build(10, 11);
        let stats = dict_get(&val, "stats").expect("stats");
        assert!(dict_get(stats, "unlocks").is_none());
        assert!(dict_get(stats, "unlocks_r").is_some());
        assert!(dict_get(stats, "eliteVehicles").is_none());
        assert!(dict_get(stats, "eliteVehicles_r").is_some());
    }

    #[test]
    fn test_cmd_sell_vehicle_barracks_capacity_and_upgraded_payout() {
        let Some((catalog, economics, mut profile)) = make_test_profile(Preset::Retail) else {
            return;
        };

        let veh_inv_id = 1;
        let veh = profile.vehicles.get(&veh_inv_id).unwrap();
        let seated_crew_count = veh.crew.iter().filter(|c| c.is_some()).count();
        assert!(seated_crew_count > 0);

        // 1. Force barracks capacity to 0 berths
        profile.berths = 0;
        let res = AccountEngine::apply(
            &mut profile,
            &EngineCmd::SellVehicle {
                shop_rev: SERVER_SHOP_REV,
                veh_inv_id,
                flags: SELL_VEHICLE_FLAG_NONE, // keep crew in barracks!
            },
            &catalog,
            &economics,
            SERVER_SHOP_REV,
            NOW_UTC,
        );
        assert_eq!(res.unwrap_err().result_id, RES_FAILURE);
        assert!(profile.vehicles.contains_key(&veh_inv_id)); // untouched

        // Restore berths
        profile.berths = 16;

        // 2. Add an optional device and an equipment to the vehicle
        let (&opt_dev_cd, _opt_dev) = catalog
            .optional_devices
            .iter()
            .find(|(_, d)| d.show_in_shop)
            .expect("opt dev");
        let (_, _, opt_dev_id) = parse_int_comp_descr(opt_dev_cd);

        let (&eq_cd, _eq) = catalog
            .equipments
            .iter()
            .find(|(_, e)| e.show_in_shop)
            .expect("eq");

        let veh_mut = profile.vehicles.get_mut(&veh_inv_id).unwrap();
        let mut descr = VehicleDescr::from_bytes(&veh_mut.comp_descr).unwrap();
        descr.optional_devices[0] = Some(opt_dev_id);
        veh_mut.comp_descr = descr.to_bytes();
        veh_mut.eqs[0] = eq_cd;

        let initial_credits = profile.credits;

        // Sell without selling eqs (eqs should return to depot[11]) and with dismissing crew
        let outcome = AccountEngine::apply(
            &mut profile,
            &EngineCmd::SellVehicle {
                shop_rev: SERVER_SHOP_REV,
                veh_inv_id,
                flags: SELL_VEHICLE_FLAG_CREW | SELL_VEHICLE_FLAG_SHELLS, // eqs not sold
            },
            &catalog,
            &economics,
            SERVER_SHOP_REV,
            NOW_UTC,
        )
        .expect("sell vehicle with opt dev success");

        assert_eq!(outcome.result_id, RES_SUCCESS);
        assert!(!profile.vehicles.contains_key(&veh_inv_id));

        // Payout must include opt_dev price in base price
        assert!(profile.credits > initial_credits);

        // Equipment should have been returned to depot
        assert_eq!(profile.items.get(&eq_cd), Some(&1));
        let inv = dict_get(&outcome.diff, "inventory").expect("inventory");
        let eq_sec = dict_get_int(inv, crate::descr::ITEM_TYPE_EQUIPMENT as i64).expect("eq sec");
        assert_eq!(dict_get_int(eq_sec, eq_cd as i64).and_then(|v| v.as_int()), Some(1));
    }

    #[test]
    fn test_cmd_buy_item_unlock_gate_and_no_gate_for_optdev_eq() {
        let Some((catalog, economics, mut profile)) = make_test_profile(Preset::Retail) else {
            return;
        };
        profile.credits = 10_000_000;

        // Pick a gun (item_type = 4) that is NOT unlocked
        let (&gun_cd, _gun_comp) = catalog
            .components
            .iter()
            .find(|(cd, c)| parse_int_comp_descr(**cd).0 == 4 && c.show_in_shop && !profile.unlocks.contains(*cd))
            .expect("locked gun");

        // 1. Buying locked module fails
        let res = AccountEngine::apply(
            &mut profile,
            &EngineCmd::BuyItem {
                shop_rev: SERVER_SHOP_REV,
                item_cd: gun_cd,
                count: 1,
            },
            &catalog,
            &economics,
            SERVER_SHOP_REV,
            NOW_UTC,
        );
        assert_eq!(res.unwrap_err().result_id, RES_FAILURE);

        // 2. Unlocking it allows buy
        profile.unlocks.insert(gun_cd);
        let outcome = AccountEngine::apply(
            &mut profile,
            &EngineCmd::BuyItem {
                shop_rev: SERVER_SHOP_REV,
                item_cd: gun_cd,
                count: 1,
            },
            &catalog,
            &economics,
            SERVER_SHOP_REV,
            NOW_UTC,
        )
        .expect("buy unlocked gun");
        assert_eq!(outcome.result_id, RES_SUCCESS);
        assert_eq!(profile.items.get(&gun_cd), Some(&1));

        // 3. Equipment (item_type = 11) has NO unlock gate
        let (&eq_cd, _) = catalog
            .equipments
            .iter()
            .find(|(_, e)| e.show_in_shop)
            .expect("eq");
        assert!(!profile.unlocks.contains(&eq_cd));

        let outcome_eq = AccountEngine::apply(
            &mut profile,
            &EngineCmd::BuyItem {
                shop_rev: SERVER_SHOP_REV,
                item_cd: eq_cd,
                count: 2,
            },
            &catalog,
            &economics,
            SERVER_SHOP_REV,
            NOW_UTC,
        )
        .expect("buy equipment without unlock gate");
        assert_eq!(outcome_eq.result_id, RES_SUCCESS);
        assert_eq!(profile.items.get(&eq_cd), Some(&2));
    }

    #[test]
    fn test_cmd_exchange_overflow_preserves_profile_transactionality() {
        let Some((catalog, economics, mut profile)) = make_test_profile(Preset::Retail) else {
            return;
        };
        profile.gold = 100;
        profile.credits = i64::MAX - 100;
        let rev_before = profile.rev;

        // 1 gold -> 400 credits gain; (i64::MAX - 100) + 400 overflows i64::MAX
        let res = AccountEngine::apply(
            &mut profile,
            &EngineCmd::Exchange {
                shop_rev: SERVER_SHOP_REV,
                gold: 1,
            },
            &catalog,
            &economics,
            SERVER_SHOP_REV,
            NOW_UTC,
        );

        assert!(res.is_err());
        assert_eq!(res.unwrap_err().result_id, RES_FAILURE);
        // Transactionality check: neither gold, credits, nor rev was mutated
        assert_eq!(profile.gold, 100);
        assert_eq!(profile.credits, i64::MAX - 100);
        assert_eq!(profile.rev, rev_before);
    }

    #[test]
    fn test_cmd_free_xp_conv_overflow_prevention() {
        let Some((catalog, economics, mut profile)) = make_test_profile(Preset::Retail) else {
            return;
        };
        let ms1_def = catalog
            .vehicle_defs
            .values()
            .find(|v| v.nation_id == 0 && v.name.eq_ignore_ascii_case("MS-1"))
            .expect("MS-1 exists");
        let ms1_cd = ms1_def.veh_type_comp_descr;

        profile.elite_vehicles.insert(ms1_cd);
        profile.veh_type_xp.insert(ms1_cd, 10_000);
        profile.gold = 100;
        profile.free_xp = i64::MAX - 100;
        let rev_before = profile.rev;

        // Trying to convert 250 XP when free_xp is near i64::MAX should fail on overflow
        let res = AccountEngine::apply(
            &mut profile,
            &EngineCmd::FreeXpConv {
                shop_rev: SERVER_SHOP_REV,
                veh_cds: vec![ms1_cd],
                xp: 250,
            },
            &catalog,
            &economics,
            SERVER_SHOP_REV,
            NOW_UTC,
        );

        assert!(res.is_err());
        assert_eq!(res.unwrap_err().result_id, RES_FAILURE);
        // Transactionality check: no fields or XP balances touched
        assert_eq!(profile.gold, 100);
        assert_eq!(profile.free_xp, i64::MAX - 100);
        assert_eq!(profile.veh_type_xp.get(&ms1_cd), Some(&10_000));
        assert_eq!(profile.rev, rev_before);
    }

    #[test]
    fn test_cmd_buy_vehicle_ammo_proportions_and_zero_gold_shells() {
        let Some((catalog, economics, mut profile)) = make_test_profile(Preset::Retail) else {
            return;
        };
        profile.credits = 10_000_000;
        profile.gold = 500; // Track gold strictly
        profile.slots = 10;

        let bt2_def = catalog
            .vehicle_defs
            .values()
            .find(|v| v.nation_id == 0 && v.name.eq_ignore_ascii_case("BT-2"))
            .expect("BT-2 exists");
        let bt2_cd = bt2_def.veh_type_comp_descr;
        profile.unlocks.insert(bt2_cd);

        let stock_gun = &bt2_def.turrets[0].guns[0];
        // Confirm stock gun has multiple shots including one with default_portion == 0.0 (gold shell)
        assert!(stock_gun.shots.len() >= 2);
        let gold_shell = stock_gun.shots.iter().find(|s| s.default_portion == 0.0);
        assert!(gold_shell.is_some(), "gun should have a gold shell with default_portion == 0.0");
        let gold_shell_cd = gold_shell.unwrap().shell_cd;

        let initial_gold = profile.gold;

        let outcome = AccountEngine::apply(
            &mut profile,
            &EngineCmd::BuyVehicle {
                shop_rev: SERVER_SHOP_REV,
                veh_type_cd: bt2_cd,
                flags: BUY_VEHICLE_FLAG_SHELLS, // No crew recruited, only ammo bought
                tman_cost_idx: 0,
            },
            &catalog,
            &economics,
            SERVER_SHOP_REV,
            NOW_UTC,
        )
        .expect("buy vehicle with ammo success");

        assert_eq!(outcome.result_id, RES_SUCCESS);
        // BT-2 is a credit vehicle; buying default ammo should NOT spend any gold
        assert_eq!(profile.gold, initial_gold, "no gold should be spent on default ammo");

        let (_, new_veh) = profile
            .vehicles
            .iter()
            .find(|(&id, _)| id >= 4)
            .expect("new vehicle found");

        let total_shells: i32 = new_veh.shells.iter().map(|&(_, count)| count).sum();
        assert!(
            total_shells > 0 && total_shells <= stock_gun.max_ammo as i32,
            "total loaded shells ({total_shells}) should be > 0 and <= max_ammo ({})",
            stock_gun.max_ammo
        );

        // Gold shell should have 0 ammo loaded
        let gold_shell_loaded = new_veh
            .shells
            .iter()
            .find(|&&(cd, _)| cd == gold_shell_cd)
            .map(|&(_, c)| c)
            .unwrap_or(0);
        assert_eq!(gold_shell_loaded, 0, "gold shell should not be loaded by default");
    }

    #[test]
    fn test_cmd_buy_vehicle_with_crew_resyncs_dossiers_even_if_vehicle_dossier_already_exists() {
        use crate::dossier::VehicleDossier;

        let Some((catalog, economics, mut profile)) = make_test_profile(Preset::Retail) else {
            return;
        };
        profile.credits = 10_000_000;
        profile.gold = 10_000;
        profile.slots = 10;

        let bt2_def = catalog
            .vehicle_defs
            .values()
            .find(|v| v.nation_id == 0 && v.name.eq_ignore_ascii_case("BT-2"))
            .expect("BT-2 exists");
        let bt2_cd = bt2_def.veh_type_comp_descr;
        profile.unlocks.insert(bt2_cd);

        // Pre-insert VehicleDossier for BT-2 (simulating previous unlock or ownership)
        let v_dossier = VehicleDossier::new().to_bytes();
        profile.vehicle_dossiers.insert(bt2_cd, (NOW_UTC, v_dossier));

        // 1. Buy BT-2 with CREW: resync_dossiers MUST be true because fresh crew dossiers were created
        let outcome_crew = AccountEngine::apply(
            &mut profile,
            &EngineCmd::BuyVehicle {
                shop_rev: SERVER_SHOP_REV,
                veh_type_cd: bt2_cd,
                flags: BUY_VEHICLE_FLAG_CREW,
                tman_cost_idx: 0,
            },
            &catalog,
            &economics,
            SERVER_SHOP_REV,
            NOW_UTC,
        )
        .expect("buy vehicle with crew");

        assert_eq!(outcome_crew.result_id, RES_SUCCESS);
        assert!(
            outcome_crew.resync_dossiers,
            "resync_dossiers must be true when recruiting crew"
        );

        // 2. Buy BT-2 WITHOUT crew when vehicle dossier already exists: resync_dossiers should be false
        let outcome_no_crew = AccountEngine::apply(
            &mut profile,
            &EngineCmd::BuyVehicle {
                shop_rev: SERVER_SHOP_REV,
                veh_type_cd: bt2_cd,
                flags: 0, // no crew, no shells
                tman_cost_idx: 0,
            },
            &catalog,
            &economics,
            SERVER_SHOP_REV,
            NOW_UTC,
        )
        .expect("buy vehicle without crew");

        assert_eq!(outcome_no_crew.result_id, RES_SUCCESS);
        assert!(
            !outcome_no_crew.resync_dossiers,
            "resync_dossiers should be false when no crew recruited and vehicle dossier already exists"
        );
    }

    #[test]
    fn test_cmd_sell_vehicle_overflow_preserves_profile_transactionality() {
        let Some((catalog, economics, mut profile)) = make_test_profile(Preset::Sandbox) else {
            return;
        };
        let bt2_cd = catalog
            .vehicle_defs
            .values()
            .find(|v| v.nation_id == 0 && v.name.eq_ignore_ascii_case("BT-2"))
            .expect("BT-2 exists")
            .veh_type_comp_descr;
        profile.unlocks.insert(bt2_cd);
        AccountEngine::apply(
            &mut profile,
            &EngineCmd::BuyVehicle {
                shop_rev: SERVER_SHOP_REV,
                veh_type_cd: bt2_cd,
                flags: 0,
                tman_cost_idx: 0,
            },
            &catalog,
            &economics,
            SERVER_SHOP_REV,
            NOW_UTC,
        )
        .expect("buy BT-2");
        let bt2_inv_id = *profile.vehicles.keys().max().unwrap();
        profile.credits = i64::MAX - 1;
        let rev_before = profile.rev;
        let veh_count_before = profile.vehicles.len();

        let res = AccountEngine::apply(
            &mut profile,
            &EngineCmd::SellVehicle {
                shop_rev: SERVER_SHOP_REV,
                veh_inv_id: bt2_inv_id,
                flags: SELL_VEHICLE_FLAG_SHELLS | SELL_VEHICLE_FLAG_EQS,
            },
            &catalog,
            &economics,
            SERVER_SHOP_REV,
            NOW_UTC,
        );
        assert!(res.is_err());
        assert_eq!(res.unwrap_err().result_id, RES_FAILURE);
        assert_eq!(profile.credits, i64::MAX - 1);
        assert_eq!(profile.rev, rev_before);
        assert_eq!(profile.vehicles.len(), veh_count_before);
    }
}
