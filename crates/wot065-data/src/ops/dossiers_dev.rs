//! Server developer commands and dossier synchronization helpers.

use crate::account_state::AccountProfile;
use crate::catalog::Catalog;
use crate::descr::{role_name, TankmanDescr};
use crate::dossier::VehicleDossier;
use crate::economics::EconomicsConfig;
use crate::ops::{CmdError, CmdOutcome, DiffBuilder, RES_SUCCESS};

/// Set account credits and gold directly (`CMD_SET_MONEY = 10001`).
pub fn set_money(
    profile: &mut AccountProfile,
    credits: i64,
    gold: i64,
    _catalog: &Catalog,
    _economics: &EconomicsConfig,
    _server_shop_rev: i64,
    _now_utc: u32,
) -> Result<CmdOutcome, CmdError> {
    if credits < 0 || gold < 0 {
        return Err(CmdError::wrong_args(
            "credits and gold amounts must be non-negative",
        ));
    }

    let prev_rev = profile.rev;
    profile.credits = credits;
    profile.gold = gold;

    let mut diff = DiffBuilder::new();
    diff.credits = Some(profile.credits);
    diff.gold = Some(profile.gold);

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

/// Add vehicle combat XP directly (`CMD_ADD_XP = 10002`).
pub fn add_xp(
    profile: &mut AccountProfile,
    veh_type_cd: i32,
    xp: i32,
    catalog: &Catalog,
    _economics: &EconomicsConfig,
    _server_shop_rev: i64,
    _now_utc: u32,
) -> Result<CmdOutcome, CmdError> {
    if xp < 0 {
        return Err(CmdError::wrong_args("xp must be non-negative"));
    }

    if !catalog.vehicle_defs.contains_key(&veh_type_cd) {
        return Err(CmdError::failure(format!(
            "vehicle {veh_type_cd} not found in catalog"
        )));
    }

    let prev_rev = profile.rev;
    let cur = profile.veh_type_xp.entry(veh_type_cd).or_insert(0);
    *cur = cur.saturating_add(xp);

    let mut diff = DiffBuilder::new();
    diff.veh_type_xp.insert(veh_type_cd, Some(*cur));

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

/// Add tankman combat XP directly (`CMD_ADD_TMAN_XP = 10003`).
pub fn add_tman_xp(
    profile: &mut AccountProfile,
    tman_inv_id: i64,
    xp: i32,
    catalog: &Catalog,
    _economics: &EconomicsConfig,
    _server_shop_rev: i64,
    _now_utc: u32,
) -> Result<CmdOutcome, CmdError> {
    if xp < 0 {
        return Err(CmdError::wrong_args("xp must be non-negative"));
    }

    let tman_entry = profile
        .tankmen
        .get_mut(&tman_inv_id)
        .ok_or_else(|| CmdError::failure(format!("tankman {tman_inv_id} not found")))?;

    let mut tman_descr = TankmanDescr::from_bytes(&tman_entry.comp_descr)
        .map_err(|e| CmdError::failure(format!("invalid tankman descriptor: {e:?}")))?;

    let role_s = role_name(tman_descr.role_id).unwrap();
    let cfg = catalog
        .tankmen_config(tman_descr.nation_id)
        .ok_or_else(|| CmdError::failure("tankmen config not found"))?;
    let role_ranks_len = cfg
        .role_ranks
        .get(role_s)
        .map(|r| r.len())
        .unwrap_or(0);

    tman_descr.add_xp(xp, role_ranks_len);

    let prev_rev = profile.rev;
    tman_entry.comp_descr = tman_descr.to_bytes();

    let mut diff = DiffBuilder::new();
    diff.tman_comp_descr
        .insert(tman_inv_id, Some(tman_entry.comp_descr.clone()));

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

/// Unlock all catalog vehicles, modules, and set all vehicles elite (`CMD_UNLOCK_ALL = 10004`).
pub fn unlock_all(
    profile: &mut AccountProfile,
    catalog: &Catalog,
    _economics: &EconomicsConfig,
    _server_shop_rev: i64,
    now_utc: u32,
) -> Result<CmdOutcome, CmdError> {
    let prev_rev = profile.rev;

    // 1. Add all catalog vehicles and their components to unlocks
    for veh_def in catalog.vehicle_defs.values() {
        profile.unlocks.insert(veh_def.veh_type_comp_descr);
        for &auto_cd in &veh_def.autounlocked_items {
            profile.unlocks.insert(auto_cd);
        }
        for c in &veh_def.chassis {
            profile.unlocks.insert(c.comp_descr);
        }
        for e in &veh_def.engines {
            profile.unlocks.insert(e.comp_descr);
        }
        for f in &veh_def.fuel_tanks {
            profile.unlocks.insert(f.comp_descr);
        }
        for r in &veh_def.radios {
            profile.unlocks.insert(r.comp_descr);
        }
        for t in &veh_def.turrets {
            profile.unlocks.insert(t.component.comp_descr);
            for g in &t.guns {
                profile.unlocks.insert(g.component.comp_descr);
                for s in &g.shots {
                    profile.unlocks.insert(s.shell_cd);
                }
            }
        }
        for &inst_cd in &veh_def.installable_components {
            profile.unlocks.insert(inst_cd);
        }

        // All catalog vehicles are now elite
        profile.elite_vehicles.insert(veh_def.veh_type_comp_descr);

        // Ensure empty v17 vehicle dossier exists
        profile
            .vehicle_dossiers
            .entry(veh_def.veh_type_comp_descr)
            .or_insert_with(|| (now_utc, VehicleDossier::new().to_bytes()));
    }

    // 2. Add any standalone catalog components
    for &comp_cd in catalog.components.keys() {
        profile.unlocks.insert(comp_cd);
    }

    let mut diff = DiffBuilder::new();
    diff.unlocks_r = Some(profile.unlocks.clone());
    diff.elite_vehicles_r = Some(profile.elite_vehicles.clone());

    profile.rev += 1;
    Ok(CmdOutcome {
        result_id: RES_SUCCESS,
        prev_rev,
        rev: profile.rev,
        diff: diff.build(prev_rev, profile.rev),
        ext: None,
        resync_dossiers: true,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::account_state::{AccountProfile, Preset};
    use crate::descr::{make_int_comp_descr, TankmanDescr, VehicleDescr};
    use crate::economics::EconomicsConfig;
    use crate::ops::{AccountEngine, EngineCmd, RES_SUCCESS, RES_WRONG_ARGS};
    use crate::testutil;

    const SERVER_SHOP_REV: i64 = 42;
    const NOW_UTC: u32 = 1_700_000_000;

    fn make_test_profile(preset: Preset) -> Option<(Catalog, EconomicsConfig, AccountProfile)> {
        let dir = testutil::game_dir()?;
        let catalog = crate::catalog::load(&dir).ok()?;
        let economics = EconomicsConfig::default();
        let profile = AccountProfile::new_with_preset(
            1001,
            "CommanderDev",
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
    fn test_set_money() {
        let Some((catalog, economics, mut profile)) = make_test_profile(Preset::Retail) else {
            eprintln!("skip: no game install");
            return;
        };

        let outcome = AccountEngine::apply(
            &mut profile,
            &EngineCmd::SetMoney {
                credits: 1_234_567,
                gold: 98_765,
            },
            &catalog,
            &economics,
            SERVER_SHOP_REV,
            NOW_UTC,
        )
        .expect("set money");

        assert_eq!(outcome.result_id, RES_SUCCESS);
        assert_eq!(profile.credits, 1_234_567);
        assert_eq!(profile.gold, 98_765);

        // Negative values rejected
        let err = AccountEngine::apply(
            &mut profile,
            &EngineCmd::SetMoney {
                credits: -1,
                gold: 100,
            },
            &catalog,
            &economics,
            SERVER_SHOP_REV,
            NOW_UTC,
        )
        .unwrap_err();
        assert_eq!(err.result_id, RES_WRONG_ARGS);
    }

    #[test]
    fn test_add_xp() {
        let Some((catalog, economics, mut profile)) = make_test_profile(Preset::Retail) else {
            eprintln!("skip: no game install");
            return;
        };
        let ms1_veh_cd = make_int_comp_descr(1, 0, 13);

        let outcome = AccountEngine::apply(
            &mut profile,
            &EngineCmd::AddXp {
                veh_type_cd: ms1_veh_cd,
                xp: 25_000,
            },
            &catalog,
            &economics,
            SERVER_SHOP_REV,
            NOW_UTC,
        )
        .expect("add xp");

        assert_eq!(outcome.result_id, RES_SUCCESS);
        assert_eq!(profile.veh_type_xp.get(&ms1_veh_cd), Some(&25_000));

        // Negative XP rejected
        let err = AccountEngine::apply(
            &mut profile,
            &EngineCmd::AddXp {
                veh_type_cd: ms1_veh_cd,
                xp: -50,
            },
            &catalog,
            &economics,
            SERVER_SHOP_REV,
            NOW_UTC,
        )
        .unwrap_err();
        assert_eq!(err.result_id, RES_WRONG_ARGS);
    }

    #[test]
    fn test_add_tman_xp() {
        let Some((catalog, economics, mut profile)) = make_test_profile(Preset::Retail) else {
            eprintln!("skip: no game install");
            return;
        };
        let ms1_id = find_starter_vehicle_id(&profile, 0, 13);
        let cmdr_id = profile.vehicles[&ms1_id].crew[0].expect("cmdr");

        let outcome = AccountEngine::apply(
            &mut profile,
            &EngineCmd::AddTmanXp {
                tman_inv_id: cmdr_id,
                xp: 50_000,
            },
            &catalog,
            &economics,
            SERVER_SHOP_REV,
            NOW_UTC,
        )
        .expect("add tman xp");

        assert_eq!(outcome.result_id, RES_SUCCESS);
        let descr = TankmanDescr::from_bytes(&profile.tankmen[&cmdr_id].comp_descr).unwrap();
        assert!(descr.free_xp > 0 || descr.role_level == 100);
    }

    #[test]
    fn test_unlock_all() {
        let Some((catalog, economics, mut profile)) = make_test_profile(Preset::Retail) else {
            eprintln!("skip: no game install");
            return;
        };

        let outcome = AccountEngine::apply(
            &mut profile,
            &EngineCmd::UnlockAll,
            &catalog,
            &economics,
            SERVER_SHOP_REV,
            NOW_UTC,
        )
        .expect("unlock all");

        assert_eq!(outcome.result_id, RES_SUCCESS);
        assert!(outcome.resync_dossiers);

        // Every vehicle in catalog should be in unlocks and elite_vehicles
        for &veh_cd in catalog.vehicle_defs.keys() {
            assert!(profile.unlocks.contains(&veh_cd), "vehicle {veh_cd} in unlocks");
            assert!(profile.elite_vehicles.contains(&veh_cd), "vehicle {veh_cd} in elite");
            assert!(profile.vehicle_dossiers.contains_key(&veh_cd), "dossier for {veh_cd}");
        }
    }
}
