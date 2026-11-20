//! Pure `AccountEngine` for WoT 0.6.5 hangar operations.
//!
//! Provides deterministic state transition logic, minimal incremental diff generation
//! (`DiffBuilder`), and command handling covering shop, economy, research, fitting,
//! crew, dossiers, and server dev commands.

pub mod crew;
pub mod dossiers_dev;
pub mod fitting;
pub mod shop_economy;

use std::collections::{BTreeMap, BTreeSet};

use crate::account_state::AccountProfile;
use crate::catalog::Catalog;
use crate::descr::{
    parse_int_comp_descr, ITEM_TYPE_EQUIPMENT, ITEM_TYPE_OPTIONAL_DEVICE, ITEM_TYPE_SHELL,
    ITEM_TYPE_VEHICLE,
};
use crate::economics::EconomicsConfig;
use crate::pickle::Value;

// =============================================================================
// Result codes (`common/AccountCommands.py`)
// =============================================================================

pub const RES_FAILURE: i32 = -1;
pub const RES_WRONG_ARGS: i32 = -2;
pub const RES_NON_PLAYER: i32 = -3;
pub const RES_SHOP_DESYNC: i32 = -4;
pub const RES_COOLDOWN: i32 = -5;
pub const RES_HIDDEN_DOSSIER: i32 = -6;
pub const RES_SUCCESS: i32 = 0;
pub const RES_STREAM: i32 = 1;
pub const RES_CACHE: i32 = 2;

// =============================================================================
// Command identifiers (`common/AccountCommands.py`)
// =============================================================================

pub const CMD_RESERVED: i32 = 0;
pub const CMD_SYNC_DATA: i32 = 100;
pub const CMD_EQUIP: i32 = 101;
pub const CMD_EQUIP_OPTDEV: i32 = 102;
pub const CMD_EQUIP_SHELLS: i32 = 103;
pub const CMD_EQUIP_EQS: i32 = 104;
pub const CMD_EQUIP_TMAN: i32 = 105;
pub const CMD_REPAIR: i32 = 106;
pub const CMD_VEH_SETTINGS: i32 = 107;
pub const CMD_VEH_CAMOUFLAGE: i32 = 120;
pub const CMD_VEH_HORN: i32 = 121;
pub const CMD_TMAN_ADD_SKILL: i32 = 151;
pub const CMD_TMAN_DROP_SKILL: i32 = 152;
pub const CMD_TMAN_RESPEC: i32 = 153;
pub const CMD_TMAN_PASSPORT: i32 = 154;
pub const CMD_UNLOCK: i32 = 201;
pub const CMD_EXCHANGE: i32 = 202;
pub const CMD_FREE_XP_CONV: i32 = 203;
pub const CMD_PREMIUM: i32 = 204;
pub const CMD_BUY_SLOT: i32 = 205;
pub const CMD_BUY_BERTHS: i32 = 206;
pub const CMD_SYNC_SHOP: i32 = 300;
pub const CMD_BUY_VEHICLE: i32 = 301;
pub const CMD_BUY_ITEM: i32 = 302;
pub const CMD_BUY_TMAN: i32 = 303;
pub const CMD_SELL_VEHICLE: i32 = 304;
pub const CMD_SELL_ITEM: i32 = 305;
pub const CMD_DISMISS_TMAN: i32 = 306;
pub const CMD_REQ_SERVER_STATS: i32 = 501;
pub const CMD_REQ_PLAYER_DOSSIER: i32 = 503;
pub const CMD_REQ_PLAYER_CLAN_INFO: i32 = 504;
pub const CMD_SYNC_DOSSIERS: i32 = 600;
pub const CMD_SET_LANGUAGE: i32 = 1000;
pub const CMD_SET_MONEY: i32 = 10001;
pub const CMD_ADD_XP: i32 = 10002;
pub const CMD_ADD_TMAN_XP: i32 = 10003;
pub const CMD_UNLOCK_ALL: i32 = 10004;

// =============================================================================
// Buy / Sell / Settings bitflags (`common/AccountCommands.py`)
// =============================================================================

pub const BUY_VEHICLE_FLAG_NONE: i32 = 0;
pub const BUY_VEHICLE_FLAG_CREW: i32 = 1;
pub const BUY_VEHICLE_FLAG_SHELLS: i32 = 16;

pub const SELL_VEHICLE_FLAG_NONE: i32 = 0;
pub const SELL_VEHICLE_FLAG_CREW: i32 = 1;
pub const SELL_VEHICLE_FLAG_SHELLS: i32 = 16;
pub const SELL_VEHICLE_FLAG_EQS: i32 = 32;

pub const VEHICLE_SETTINGS_FLAG_NONE: i32 = 0;
pub const VEHICLE_SETTINGS_FLAG_XP_TO_TMAN: i32 = 1;
pub const VEHICLE_SETTINGS_FLAG_AUTO_REPAIR: i32 = 2;
pub const VEHICLE_SETTINGS_FLAG_AUTO_LOAD: i32 = 4;
pub const VEHICLE_SETTINGS_FLAG_AUTO_EQUIP: i32 = 8;

// =============================================================================
// Typed commands
// =============================================================================

/// Full enumeration of all client/server commands processed by the AccountEngine.
#[derive(Debug, Clone, PartialEq)]
pub enum EngineCmd {
    // --- Task 5: Shop, Economy, Research & Vehicle Buy/Sell ---
    BuySlot {
        shop_rev: i64,
    },
    BuyBerths {
        shop_rev: i64,
    },
    Exchange {
        shop_rev: i64,
        gold: i64,
    },
    FreeXpConv {
        shop_rev: i64,
        xp: i64,
        veh_cds: Vec<i32>,
    },
    Premium {
        shop_rev: i64,
        days: i32,
    },
    Unlock {
        veh_type_cd: i32,
        unlock_idx: usize,
    },
    BuyVehicle {
        shop_rev: i64,
        veh_type_cd: i32,
        flags: i32,
        tman_cost_idx: usize,
    },
    SellVehicle {
        shop_rev: i64,
        veh_inv_id: i64,
        flags: i32,
    },
    BuyItem {
        shop_rev: i64,
        item_cd: i32,
        count: i32,
    },
    SellItem {
        shop_rev: i64,
        item_type_idx: u8,
        item_cd: i32,
        count: i32,
    },

    // --- Task 6: Fitting, Crew, Dev Commands ---
    Equip {
        veh_inv_id: i64,
        item_cd: i32,
        gun_cd: i32,
    },
    EquipOptDev {
        veh_inv_id: i64,
        dev_cd: i32,
        slot_idx: usize,
        is_paid_removal: bool,
    },
    EquipShells {
        veh_inv_id: i64,
        shells: Vec<(i32, i32)>,
    },
    EquipEqs {
        veh_inv_id: i64,
        eqs: [i32; 3],
    },
    EquipTman {
        veh_inv_id: i64,
        slot: usize,
        tman_inv_id: i64,
    },
    Repair {
        veh_inv_id: i64,
    },
    VehSettings {
        veh_inv_id: i64,
        flag: i32,
        is_on: bool,
    },
    VehCamouflage {
        shop_rev: i64,
        veh_inv_id: i64,
        camo_id: u16,
    },
    VehHorn {
        shop_rev: i64,
        veh_inv_id: i64,
        horn_id: u8,
    },
    TmanAddSkill {
        tman_inv_id: i64,
        skill_idx: u8,
    },
    TmanDropSkill {
        tman_inv_id: i64,
        skill_idx: u8,
    },
    TmanRespec {
        shop_rev: i64,
        tman_inv_id: i64,
        tman_cost_idx: usize,
        veh_type_cd: i32,
    },
    TmanPassport {
        shop_rev: i64,
        tman_inv_id: i64,
        is_female_flag: i32,
        first_name_id: i32,
        last_name_id: i32,
        icon_id: i32,
    },
    BuyTman {
        shop_rev: i64,
        veh_type_cd: i32,
        role_idx: u8,
        tman_cost_idx: usize,
    },
    DismissTman {
        tman_inv_id: i64,
    },
    SetMoney {
        credits: i64,
        gold: i64,
    },
    AddXp {
        veh_type_cd: i32,
        xp: i32,
    },
    AddTmanXp {
        tman_inv_id: i64,
        xp: i32,
    },
    UnlockAll,

    // --- Account metadata ---
    SetLanguage {
        lang: String,
    },
}

impl EngineCmd {
    /// Return the corresponding numeric WoT command ID (`AccountCommands.CMD_*`).
    pub fn cmd_id(&self) -> i32 {
        match self {
            EngineCmd::BuySlot { .. } => CMD_BUY_SLOT,
            EngineCmd::BuyBerths { .. } => CMD_BUY_BERTHS,
            EngineCmd::Exchange { .. } => CMD_EXCHANGE,
            EngineCmd::FreeXpConv { .. } => CMD_FREE_XP_CONV,
            EngineCmd::Premium { .. } => CMD_PREMIUM,
            EngineCmd::Unlock { .. } => CMD_UNLOCK,
            EngineCmd::BuyVehicle { .. } => CMD_BUY_VEHICLE,
            EngineCmd::SellVehicle { .. } => CMD_SELL_VEHICLE,
            EngineCmd::BuyItem { .. } => CMD_BUY_ITEM,
            EngineCmd::SellItem { .. } => CMD_SELL_ITEM,
            EngineCmd::Equip { .. } => CMD_EQUIP,
            EngineCmd::EquipOptDev { .. } => CMD_EQUIP_OPTDEV,
            EngineCmd::EquipShells { .. } => CMD_EQUIP_SHELLS,
            EngineCmd::EquipEqs { .. } => CMD_EQUIP_EQS,
            EngineCmd::EquipTman { .. } => CMD_EQUIP_TMAN,
            EngineCmd::Repair { .. } => CMD_REPAIR,
            EngineCmd::VehSettings { .. } => CMD_VEH_SETTINGS,
            EngineCmd::VehCamouflage { .. } => CMD_VEH_CAMOUFLAGE,
            EngineCmd::VehHorn { .. } => CMD_VEH_HORN,
            EngineCmd::TmanAddSkill { .. } => CMD_TMAN_ADD_SKILL,
            EngineCmd::TmanDropSkill { .. } => CMD_TMAN_DROP_SKILL,
            EngineCmd::TmanRespec { .. } => CMD_TMAN_RESPEC,
            EngineCmd::TmanPassport { .. } => CMD_TMAN_PASSPORT,
            EngineCmd::BuyTman { .. } => CMD_BUY_TMAN,
            EngineCmd::DismissTman { .. } => CMD_DISMISS_TMAN,
            EngineCmd::SetMoney { .. } => CMD_SET_MONEY,
            EngineCmd::AddXp { .. } => CMD_ADD_XP,
            EngineCmd::AddTmanXp { .. } => CMD_ADD_TMAN_XP,
            EngineCmd::UnlockAll => CMD_UNLOCK_ALL,
            EngineCmd::SetLanguage { .. } => CMD_SET_LANGUAGE,
        }
    }
}

// =============================================================================
// Command outcome and error types
// =============================================================================

/// Successful outcome of applying an `EngineCmd`.
#[derive(Debug, Clone, PartialEq)]
pub struct CmdOutcome {
    pub result_id: i32,
    pub prev_rev: i64,
    pub rev: i64,
    pub diff: Value,
    pub ext: Option<Value>,
    pub resync_dossiers: bool,
}

/// Command execution failure with exact WoT result code and optional extension payload.
#[derive(Debug, Clone, PartialEq)]
pub struct CmdError {
    pub result_id: i32,
    pub ext: Option<Value>,
    pub message: String,
}

impl CmdError {
    pub fn failure(msg: impl Into<String>) -> Self {
        Self {
            result_id: RES_FAILURE,
            ext: None,
            message: msg.into(),
        }
    }

    pub fn wrong_args(msg: impl Into<String>) -> Self {
        Self {
            result_id: RES_WRONG_ARGS,
            ext: None,
            message: msg.into(),
        }
    }

    pub fn shop_desync(server_shop_rev: i64) -> Self {
        let ext = Value::dict(vec![(
            Value::str("shopRev"),
            Value::Int(server_shop_rev),
        )]);
        Self {
            result_id: RES_SHOP_DESYNC,
            ext: Some(ext),
            message: format!("shop desync: expected shop revision {server_shop_rev}"),
        }
    }
}

impl std::fmt::Display for CmdError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "cmd error (res={}): {}", self.result_id, self.message)
    }
}

impl std::error::Error for CmdError {}

// =============================================================================
// DiffBuilder
// =============================================================================

/// Minimal incremental `Account.update(diff)` generator.
///
/// Ensures exact client synchronization semantics:
/// - Emits `'prevRev'` and `'rev'` for non-full-sync updates.
/// - Deleted dictionary keys are serialized as `Value::None` (matching `Inventory.py:414-422`).
/// - Growing sets (`unlocks`, `eliteVehicles`, `doubleXPVehs`) emit incremental additions
///   under `'unlocks'` or full replacements under `'unlocks_r'`.
#[derive(Debug, Clone, Default)]
pub struct DiffBuilder {
    // Stats
    pub credits: Option<i64>,
    pub gold: Option<i64>,
    pub free_xp: Option<i64>,
    pub slots: Option<i32>,
    pub berths: Option<i32>,
    pub veh_type_xp: BTreeMap<i32, Option<i32>>,
    pub unlocks: BTreeSet<i32>,
    pub unlocks_r: Option<BTreeSet<i32>>,
    pub elite_vehicles: BTreeSet<i32>,
    pub elite_vehicles_r: Option<BTreeSet<i32>>,
    pub double_xp_vehs: BTreeSet<i32>,
    pub double_xp_vehs_r: Option<BTreeSet<i32>>,

    // Account
    pub attrs: Option<u64>,
    pub premium_expiry_time: Option<u32>,

    // Inventory section 1 (vehicles)
    pub veh_comp_descr: BTreeMap<i64, Option<Vec<u8>>>,
    pub veh_shells_layout: BTreeMap<i64, Option<Vec<((i32, i32), Vec<(i32, i32)>)>>>,
    pub veh_shells: BTreeMap<i64, Option<Vec<(i32, i32)>>>,
    pub veh_crew: BTreeMap<i64, Option<Vec<Option<i64>>>>,
    pub veh_repair: BTreeMap<i64, Option<(i32, i32)>>,
    pub clear_veh_repair: BTreeSet<i64>,
    pub veh_eqs_layout: BTreeMap<i64, Option<[i32; 3]>>,
    pub veh_eqs: BTreeMap<i64, Option<[i32; 3]>>,
    pub veh_settings: BTreeMap<i64, Option<i32>>,
    pub veh_lock: BTreeMap<i64, Option<i32>>,
    pub delete_vehicles: BTreeSet<i64>,

    // Inventory section 8 (tankmen)
    pub tman_comp_descr: BTreeMap<i64, Option<Vec<u8>>>,
    pub tman_vehicle: BTreeMap<i64, Option<i64>>,
    pub delete_tankmen: BTreeSet<i64>,

    // Inventory sections 2..=7, 9, 10, 11 (depot items by item_type)
    pub depot_items: BTreeMap<u8, BTreeMap<i32, Option<i32>>>,
}

impl DiffBuilder {
    pub fn new() -> Self {
        Self::default()
    }

    /// Record a vehicle lock reason (`0` clears the lock on the client).
    pub fn veh_lock(&mut self, veh_inv_id: i64, lock: i32) -> &mut Self {
        self.veh_lock.insert(veh_inv_id, Some(lock));
        self
    }

    /// Build the pickled diff `Value::dict` from populated deltas.
    pub fn build(&self, prev_rev: i64, rev: i64) -> Value {
        let mut root = Vec::new();
        root.push((Value::str("prevRev"), Value::Int(prev_rev)));
        root.push((Value::str("rev"), Value::Int(rev)));

        // 1. Stats diff
        let mut stats_entries = Vec::new();
        if let Some(c) = self.credits {
            stats_entries.push((Value::str("credits"), Value::Int(c)));
        }
        if let Some(g) = self.gold {
            stats_entries.push((Value::str("gold"), Value::Int(g)));
        }
        if let Some(fxp) = self.free_xp {
            stats_entries.push((Value::str("freeXP"), Value::Int(fxp)));
        }
        if let Some(s) = self.slots {
            stats_entries.push((Value::str("slots"), Value::Int(s as i64)));
        }
        if let Some(b) = self.berths {
            stats_entries.push((Value::str("berths"), Value::Int(b as i64)));
        }
        if !self.veh_type_xp.is_empty() {
            let mut vxp_dict = Vec::new();
            for (&cd, opt_xp) in &self.veh_type_xp {
                let val = match opt_xp {
                    Some(xp) => Value::Int(*xp as i64),
                    None => Value::None,
                };
                vxp_dict.push((Value::Int(cd as i64), val));
            }
            stats_entries.push((Value::str("vehTypeXP"), Value::dict(vxp_dict)));
        }
        if let Some(r_set) = &self.unlocks_r {
            let set_vals = r_set.iter().map(|&cd| Value::Int(cd as i64)).collect();
            stats_entries.push((Value::str("unlocks_r"), Value::set(set_vals)));
        } else if !self.unlocks.is_empty() {
            let set_vals = self.unlocks.iter().map(|&cd| Value::Int(cd as i64)).collect();
            stats_entries.push((Value::str("unlocks"), Value::set(set_vals)));
        }
        if let Some(r_set) = &self.elite_vehicles_r {
            let set_vals = r_set.iter().map(|&cd| Value::Int(cd as i64)).collect();
            stats_entries.push((Value::str("eliteVehicles_r"), Value::set(set_vals)));
        } else if !self.elite_vehicles.is_empty() {
            let set_vals = self.elite_vehicles.iter().map(|&cd| Value::Int(cd as i64)).collect();
            stats_entries.push((Value::str("eliteVehicles"), Value::set(set_vals)));
        }
        if let Some(r_set) = &self.double_xp_vehs_r {
            let set_vals = r_set.iter().map(|&cd| Value::Int(cd as i64)).collect();
            stats_entries.push((Value::str("doubleXPVehs_r"), Value::set(set_vals)));
        } else if !self.double_xp_vehs.is_empty() {
            let set_vals = self.double_xp_vehs.iter().map(|&cd| Value::Int(cd as i64)).collect();
            stats_entries.push((Value::str("doubleXPVehs"), Value::set(set_vals)));
        }
        if !stats_entries.is_empty() {
            root.push((Value::str("stats"), Value::dict(stats_entries)));
        }

        // 2. Account diff
        let mut account_entries = Vec::new();
        if let Some(a) = self.attrs {
            account_entries.push((Value::str("attrs"), Value::Int(a as i64)));
        }
        if let Some(t) = self.premium_expiry_time {
            account_entries.push((Value::str("premiumExpiryTime"), Value::Int(t as i64)));
        }
        if !account_entries.is_empty() {
            root.push((Value::str("account"), Value::dict(account_entries)));
        }

        // 3. Inventory diff
        let mut inv_entries = Vec::new();

        // 3.1 Inventory section 1 (vehicles)
        let has_veh_diffs = !self.delete_vehicles.is_empty()
            || !self.veh_comp_descr.is_empty()
            || !self.veh_shells_layout.is_empty()
            || !self.veh_shells.is_empty()
            || !self.veh_crew.is_empty()
            || !self.veh_repair.is_empty()
            || !self.clear_veh_repair.is_empty()
            || !self.veh_eqs_layout.is_empty()
            || !self.veh_eqs.is_empty()
            || !self.veh_settings.is_empty();

        if has_veh_diffs {
            let mut sub_comp = Vec::new();
            let mut sub_shells_layout = Vec::new();
            let mut sub_shells = Vec::new();
            let mut sub_crew = Vec::new();
            let mut sub_repair = Vec::new();
            let mut sub_eqs_layout = Vec::new();
            let mut sub_eqs = Vec::new();
            let mut sub_settings = Vec::new();
            let mut sub_lock = Vec::new();

            for &del_id in &self.delete_vehicles {
                sub_comp.push((Value::Int(del_id), Value::None));
                sub_shells_layout.push((Value::Int(del_id), Value::None));
                sub_shells.push((Value::Int(del_id), Value::None));
                sub_crew.push((Value::Int(del_id), Value::None));
                sub_repair.push((Value::Int(del_id), Value::None));
                sub_eqs_layout.push((Value::Int(del_id), Value::None));
                sub_eqs.push((Value::Int(del_id), Value::None));
                sub_settings.push((Value::Int(del_id), Value::None));
                sub_lock.push((Value::Int(del_id), Value::None));
            }

            for (&id, opt_b) in &self.veh_comp_descr {
                let val = match opt_b {
                    Some(b) => Value::Str(b.clone()),
                    None => Value::None,
                };
                sub_comp.push((Value::Int(id), val));
            }
            for (&id, opt_l) in &self.veh_shells_layout {
                let val = match opt_l {
                    Some(l) => {
                        let l_dict: Vec<(Value, Value)> = l
                            .iter()
                            .map(|((t_cd, g_cd), shells)| {
                                let key = Value::tuple(vec![
                                    Value::Int(*t_cd as i64),
                                    Value::Int(*g_cd as i64),
                                ]);
                                let flat: Vec<Value> = shells
                                    .iter()
                                    .flat_map(|&(cd, cnt)| {
                                        vec![Value::Int(cd as i64), Value::Int(cnt as i64)]
                                    })
                                    .collect();
                                (key, Value::list(flat))
                            })
                            .collect();
                        Value::dict(l_dict)
                    }
                    None => Value::None,
                };
                sub_shells_layout.push((Value::Int(id), val));
            }
            for (&id, opt_s) in &self.veh_shells {
                let val = match opt_s {
                    Some(s) => {
                        let flat: Vec<Value> = s
                            .iter()
                            .flat_map(|&(cd, cnt)| {
                                vec![Value::Int(cd as i64), Value::Int(cnt as i64)]
                            })
                            .collect();
                        Value::list(flat)
                    }
                    None => Value::None,
                };
                sub_shells.push((Value::Int(id), val));
            }
            for (&id, opt_c) in &self.veh_crew {
                let val = match opt_c {
                    Some(c) => {
                        let c_vals: Vec<Value> = c
                            .iter()
                            .map(|&opt| opt.map_or(Value::None, Value::Int))
                            .collect();
                        Value::list(c_vals)
                    }
                    None => Value::None,
                };
                sub_crew.push((Value::Int(id), val));
            }
            for &id in &self.clear_veh_repair {
                sub_repair.push((Value::Int(id), Value::None));
            }
            for (&id, opt_r) in &self.veh_repair {
                let val = match opt_r {
                    Some((cost, health)) => {
                        Value::tuple(vec![Value::Int(*cost as i64), Value::Int(*health as i64)])
                    }
                    None => Value::None,
                };
                sub_repair.push((Value::Int(id), val));
            }
            for (&id, opt_e) in &self.veh_eqs_layout {
                let val = match opt_e {
                    Some(eqs) => Value::list(eqs.iter().map(|&x| Value::Int(x as i64)).collect()),
                    None => Value::None,
                };
                sub_eqs_layout.push((Value::Int(id), val));
            }
            for (&id, opt_e) in &self.veh_eqs {
                let val = match opt_e {
                    Some(eqs) => Value::list(eqs.iter().map(|&x| Value::Int(x as i64)).collect()),
                    None => Value::None,
                };
                sub_eqs.push((Value::Int(id), val));
            }
            for (&id, opt_s) in &self.veh_settings {
                let val = match opt_s {
                    Some(s) => Value::Int(*s as i64),
                    None => Value::None,
                };
                sub_settings.push((Value::Int(id), val));
            }

            let veh_dict = Value::dict(vec![
                (Value::str("compDescr"), Value::dict(sub_comp)),
                (Value::str("shellsLayout"), Value::dict(sub_shells_layout)),
                (Value::str("shells"), Value::dict(sub_shells)),
                (Value::str("crew"), Value::dict(sub_crew)),
                (Value::str("repair"), Value::dict(sub_repair)),
                (Value::str("eqsLayout"), Value::dict(sub_eqs_layout)),
                (Value::str("eqs"), Value::dict(sub_eqs)),
                (Value::str("settings"), Value::dict(sub_settings)),
                (Value::str("lock"), Value::dict(sub_lock)),
            ]);
            inv_entries.push((Value::Int(1), veh_dict));
        }

        // 3.2 Inventory section 8 (tankmen)
        let has_tman_diffs = !self.delete_tankmen.is_empty()
            || !self.tman_comp_descr.is_empty()
            || !self.tman_vehicle.is_empty();

        if has_tman_diffs {
            let mut sub_comp = Vec::new();
            let mut sub_veh = Vec::new();

            for &del_id in &self.delete_tankmen {
                sub_comp.push((Value::Int(del_id), Value::None));
                sub_veh.push((Value::Int(del_id), Value::None));
            }

            for (&id, opt_b) in &self.tman_comp_descr {
                let val = match opt_b {
                    Some(b) => Value::Str(b.clone()),
                    None => Value::None,
                };
                sub_comp.push((Value::Int(id), val));
            }

            for (&id, opt_v) in &self.tman_vehicle {
                let val = match opt_v {
                    Some(veh_id) => Value::Int(*veh_id),
                    None => Value::None,
                };
                sub_veh.push((Value::Int(id), val));
            }

            let tman_dict = Value::dict(vec![
                (Value::str("compDescr"), Value::dict(sub_comp)),
                (Value::str("vehicle"), Value::dict(sub_veh)),
            ]);
            inv_entries.push((Value::Int(8), tman_dict));
        }

        // 3.3 Inventory sections 2..=7, 9, 10, 11 (depot items)
        for (&item_type, items) in &self.depot_items {
            if !items.is_empty() {
                let mut type_dict = Vec::new();
                for (&comp_descr, opt_cnt) in items {
                    let val = match opt_cnt {
                        Some(cnt) => Value::Int(*cnt as i64),
                        None => Value::None,
                    };
                    type_dict.push((Value::Int(comp_descr as i64), val));
                }
                inv_entries.push((Value::Int(item_type as i64), Value::dict(type_dict)));
            }
        }

        if !inv_entries.is_empty() {
            root.push((Value::str("inventory"), Value::dict(inv_entries)));
        }

        // 4. Cache diff (client/Account.py:498-501 & client/account_helpers/Inventory.py:66-70)
        if !self.veh_lock.is_empty() {
            let vehs_lock_entries: Vec<(Value, Value)> = self
                .veh_lock
                .iter()
                .map(|(&id, opt_l)| (Value::Int(id), Value::Int(opt_l.unwrap_or(0) as i64)))
                .collect();
            let cache_entries = vec![(Value::str("vehsLock"), Value::dict(vehs_lock_entries))];
            root.push((Value::str("cache"), Value::dict(cache_entries)));
        }

        Value::dict(root)
    }
}

// =============================================================================
// Shared helpers
// =============================================================================

/// Verify that the client's shop revision matches the active server shop revision.
#[inline]
pub fn require_shop_rev(client_rev: i64, server_rev: i64) -> Result<(), CmdError> {
    if client_rev != server_rev {
        Err(CmdError::shop_desync(server_rev))
    } else {
        Ok(())
    }
}

/// Retrieve price `(credits, gold)` and `show_in_shop` flag for any catalog item.
pub fn get_item_price_and_shop_flag(
    catalog: &Catalog,
    item_cd: i32,
) -> Option<((i64, i64), bool)> {
    let (item_type, nation_id, _) = parse_int_comp_descr(item_cd);
    match item_type {
        ITEM_TYPE_VEHICLE => {
            let veh = catalog.vehicle_def(item_cd)?;
            Some((veh.price, veh.show_in_shop))
        }
        2..=7 => {
            if let Some(comp) = catalog.component(item_cd) {
                Some((comp.price, comp.show_in_shop))
            } else if let Some(p) = catalog
                .extras
                .shop_items
                .get(&(nation_id, item_type as i64, item_cd as i64))
            {
                Some(((p.credits, p.gold), !p.hidden))
            } else {
                None
            }
        }
        ITEM_TYPE_OPTIONAL_DEVICE => {
            let opt = catalog.optional_device(item_cd)?;
            Some((opt.price, opt.show_in_shop))
        }
        ITEM_TYPE_SHELL => {
            if let Some(comp) = catalog.component(item_cd) {
                Some((comp.price, comp.show_in_shop))
            } else if let Some(p) = catalog
                .extras
                .shop_items
                .get(&(nation_id, 10, item_cd as i64))
            {
                Some(((p.credits, p.gold), !p.hidden))
            } else {
                None
            }
        }
        ITEM_TYPE_EQUIPMENT => {
            let eq = catalog.equipment(item_cd)?;
            Some((eq.price, eq.show_in_shop))
        }
        _ => None,
    }
}

/// Check and promote any catalog vehicles whose unlocks are now fully complete
/// into `profile.elite_vehicles`, returning the newly promoted vehicle CDs.
pub fn recompute_elite_vehicles(profile: &mut AccountProfile, catalog: &Catalog) -> Vec<i32> {
    let mut newly_elite = Vec::new();
    for &unlocked_cd in &profile.unlocks {
        let (item_type, _, _) = parse_int_comp_descr(unlocked_cd);
        if item_type == ITEM_TYPE_VEHICLE {
            if profile.elite_vehicles.contains(&unlocked_cd) {
                continue;
            }
            if let Some(veh_def) = catalog.vehicle_def(unlocked_cd) {
                if veh_def.is_default_elite
                    || veh_def
                        .unlocks_descrs
                        .iter()
                        .all(|u| profile.unlocks.contains(&u.target_cd))
                {
                    profile.elite_vehicles.insert(unlocked_cd);
                    newly_elite.push(unlocked_cd);
                }
            }
        }
    }
    newly_elite
}

/// Take `count` items from depot (`profile.items`), emitting the updated count or deletion in `diff`.
pub fn depot_take(
    profile: &mut AccountProfile,
    diff: &mut DiffBuilder,
    item_cd: i32,
    count: i32,
) -> Result<(), CmdError> {
    if count <= 0 {
        return Ok(());
    }
    let (item_type, _, _) = parse_int_comp_descr(item_cd);
    let cur = profile.items.get(&item_cd).copied().unwrap_or(0);
    if cur < count {
        return Err(CmdError::failure(format!(
            "insufficient items in depot: item {item_cd} has {cur}, need {count}"
        )));
    }
    let remaining = cur - count;
    if remaining > 0 {
        profile.items.insert(item_cd, remaining);
        diff.depot_items
            .entry(item_type)
            .or_default()
            .insert(item_cd, Some(remaining));
    } else {
        profile.items.remove(&item_cd);
        diff.depot_items
            .entry(item_type)
            .or_default()
            .insert(item_cd, None);
    }
    Ok(())
}

/// Give `count` items to depot (`profile.items`), emitting the updated count in `diff`.
pub fn depot_give(
    profile: &mut AccountProfile,
    diff: &mut DiffBuilder,
    item_cd: i32,
    count: i32,
) -> Result<(), CmdError> {
    if count <= 0 {
        return Ok(());
    }
    let (item_type, _, _) = parse_int_comp_descr(item_cd);
    let cur = profile.items.entry(item_cd).or_insert(0);
    *cur = cur
        .checked_add(count)
        .ok_or_else(|| CmdError::failure("depot item count overflow"))?;
    diff.depot_items
        .entry(item_type)
        .or_default()
        .insert(item_cd, Some(*cur));
    Ok(())
}

/// Require that the vehicle is alive and not broken (`repair.is_none()`) and not locked.
pub fn require_vehicle_not_broken(
    veh_entry: &crate::account_state::VehicleEntry,
) -> Result<(), CmdError> {
    if veh_entry.repair.is_some() {
        return Err(CmdError::failure("vehicle is damaged and requires repair"));
    }
    if veh_entry.lock != 0 {
        return Err(CmdError::failure("vehicle is locked"));
    }
    Ok(())
}

/// Count number of tankmen in barracks (unseated).
pub fn barracks_used(profile: &AccountProfile) -> usize {
    profile
        .tankmen
        .values()
        .filter(|t| t.vehicle_inv_id.is_none())
        .count()
}

// =============================================================================
// AccountEngine
// =============================================================================

/// Pure transactional command engine.
pub struct AccountEngine;

impl AccountEngine {
    /// Apply an `EngineCmd` against `profile`.
    ///
    /// Validates all preconditions and computes all changes before mutating `profile`.
    /// On any error (`RES_FAILURE`, `RES_WRONG_ARGS`, `RES_SHOP_DESYNC`), `profile`
    /// remains strictly untouched. On success, `profile.rev` is incremented by 1 and
    /// a minimal incremental `Account.update(diff)` is generated.
    pub fn apply(
        profile: &mut AccountProfile,
        cmd: &EngineCmd,
        catalog: &Catalog,
        economics: &EconomicsConfig,
        shop_rev: i64,
        now_utc: u32,
    ) -> Result<CmdOutcome, CmdError> {
        match cmd {
            // Task 5: Shop & Economy
            EngineCmd::BuySlot { shop_rev: r } => {
                shop_economy::buy_slot(profile, *r, catalog, economics, shop_rev, now_utc)
            }
            EngineCmd::BuyBerths { shop_rev: r } => {
                shop_economy::buy_berths(profile, *r, catalog, economics, shop_rev, now_utc)
            }
            EngineCmd::Exchange { shop_rev: r, gold } => {
                shop_economy::exchange(profile, *r, *gold, catalog, economics, shop_rev, now_utc)
            }
            EngineCmd::FreeXpConv {
                shop_rev: r,
                xp,
                veh_cds,
            } => shop_economy::convert_free_xp(
                profile, *r, *xp, veh_cds, catalog, economics, shop_rev, now_utc,
            ),
            EngineCmd::Premium { shop_rev: r, days } => {
                shop_economy::upgrade_to_premium(
                    profile, *r, *days, catalog, economics, shop_rev, now_utc,
                )
            }
            EngineCmd::Unlock {
                veh_type_cd,
                unlock_idx,
            } => shop_economy::unlock(
                profile,
                *veh_type_cd,
                *unlock_idx,
                catalog,
                economics,
                shop_rev,
                now_utc,
            ),
            EngineCmd::BuyVehicle {
                shop_rev: r,
                veh_type_cd,
                flags,
                tman_cost_idx,
            } => shop_economy::buy_vehicle(
                profile,
                *r,
                *veh_type_cd,
                *flags,
                *tman_cost_idx,
                catalog,
                economics,
                shop_rev,
                now_utc,
            ),
            EngineCmd::SellVehicle {
                shop_rev: r,
                veh_inv_id,
                flags,
            } => shop_economy::sell_vehicle(
                profile,
                *r,
                *veh_inv_id,
                *flags,
                catalog,
                economics,
                shop_rev,
                now_utc,
            ),
            EngineCmd::BuyItem {
                shop_rev: r,
                item_cd,
                count,
            } => shop_economy::buy_item(
                profile, *r, *item_cd, *count, catalog, economics, shop_rev, now_utc,
            ),
            EngineCmd::SellItem {
                shop_rev: r,
                item_type_idx,
                item_cd,
                count,
            } => shop_economy::sell_item(
                profile,
                *r,
                *item_type_idx,
                *item_cd,
                *count,
                catalog,
                economics,
                shop_rev,
                now_utc,
            ),

            // Task 6: Fitting
            EngineCmd::Equip {
                veh_inv_id,
                item_cd,
                gun_cd,
            } => fitting::equip(
                profile, *veh_inv_id, *item_cd, *gun_cd, catalog, economics, shop_rev, now_utc,
            ),
            EngineCmd::EquipOptDev {
                veh_inv_id,
                dev_cd,
                slot_idx,
                is_paid_removal,
            } => fitting::equip_opt_dev(
                profile,
                *veh_inv_id,
                *dev_cd,
                *slot_idx,
                *is_paid_removal,
                catalog,
                economics,
                shop_rev,
                now_utc,
            ),
            EngineCmd::EquipShells { veh_inv_id, shells } => fitting::equip_shells(
                profile, *veh_inv_id, shells, catalog, economics, shop_rev, now_utc,
            ),
            EngineCmd::EquipEqs { veh_inv_id, eqs } => fitting::equip_eqs(
                profile, *veh_inv_id, eqs, catalog, economics, shop_rev, now_utc,
            ),
            EngineCmd::Repair { veh_inv_id } => fitting::repair(
                profile, *veh_inv_id, catalog, economics, shop_rev, now_utc,
            ),
            EngineCmd::VehSettings {
                veh_inv_id,
                flag,
                is_on,
            } => fitting::veh_settings(
                profile, *veh_inv_id, *flag, *is_on, catalog, economics, shop_rev, now_utc,
            ),
            EngineCmd::VehCamouflage {
                shop_rev: r,
                veh_inv_id,
                camo_id,
            } => fitting::veh_camouflage(
                profile, *r, *veh_inv_id, *camo_id, catalog, economics, shop_rev, now_utc,
            ),
            EngineCmd::VehHorn {
                shop_rev: r,
                veh_inv_id,
                horn_id,
            } => fitting::veh_horn(
                profile, *r, *veh_inv_id, *horn_id, catalog, economics, shop_rev, now_utc,
            ),

            // Task 6: Crew
            EngineCmd::BuyTman {
                shop_rev: r,
                veh_type_cd,
                role_idx,
                tman_cost_idx,
            } => crew::buy_tankman(
                profile,
                *r,
                *veh_type_cd,
                *role_idx,
                *tman_cost_idx,
                catalog,
                economics,
                shop_rev,
                now_utc,
            ),
            EngineCmd::EquipTman {
                veh_inv_id,
                slot,
                tman_inv_id,
            } => crew::equip_tankman(
                profile, *veh_inv_id, *slot, *tman_inv_id, catalog, economics, shop_rev, now_utc,
            ),
            EngineCmd::DismissTman { tman_inv_id } => crew::dismiss_tankman(
                profile, *tman_inv_id, catalog, economics, shop_rev, now_utc,
            ),
            EngineCmd::TmanRespec {
                shop_rev: r,
                tman_inv_id,
                tman_cost_idx,
                veh_type_cd,
            } => crew::tman_respec(
                profile,
                *r,
                *tman_inv_id,
                *tman_cost_idx,
                *veh_type_cd,
                catalog,
                economics,
                shop_rev,
                now_utc,
            ),
            EngineCmd::TmanAddSkill {
                tman_inv_id,
                skill_idx,
            } => crew::tman_add_skill(
                profile, *tman_inv_id, *skill_idx, catalog, economics, shop_rev, now_utc,
            ),
            EngineCmd::TmanDropSkill {
                tman_inv_id,
                skill_idx,
            } => crew::tman_drop_skill(
                profile, *tman_inv_id, *skill_idx, catalog, economics, shop_rev, now_utc,
            ),
            EngineCmd::TmanPassport {
                shop_rev: r,
                tman_inv_id,
                is_female_flag,
                first_name_id,
                last_name_id,
                icon_id,
            } => crew::tman_passport(
                profile,
                *r,
                *tman_inv_id,
                *is_female_flag,
                *first_name_id,
                *last_name_id,
                *icon_id,
                catalog,
                economics,
                shop_rev,
                now_utc,
            ),

            // Task 6: Dev commands
            EngineCmd::SetMoney { credits, gold } => dossiers_dev::set_money(
                profile, *credits, *gold, catalog, economics, shop_rev, now_utc,
            ),
            EngineCmd::AddXp { veh_type_cd, xp } => dossiers_dev::add_xp(
                profile, *veh_type_cd, *xp, catalog, economics, shop_rev, now_utc,
            ),
            EngineCmd::AddTmanXp { tman_inv_id, xp } => dossiers_dev::add_tman_xp(
                profile, *tman_inv_id, *xp, catalog, economics, shop_rev, now_utc,
            ),
            EngineCmd::UnlockAll => dossiers_dev::unlock_all(
                profile, catalog, economics, shop_rev, now_utc,
            ),

            // Metadata
            EngineCmd::SetLanguage { lang } => {
                profile.language = Some(lang.clone());
                profile.rev += 1;
                let diff = DiffBuilder::new();
                Ok(CmdOutcome {
                    result_id: RES_SUCCESS,
                    prev_rev: profile.rev - 1,
                    rev: profile.rev,
                    diff: diff.build(profile.rev - 1, profile.rev),
                    ext: None,
                    resync_dossiers: false,
                })
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_diff_builder_veh_lock_emits_cache_vehs_lock() {
        let diff = DiffBuilder::new().veh_lock(101, 16).build(1, 2);
        let cache = diff.get("cache").expect("diff must include 'cache'");
        let vehs_lock = cache.get("vehsLock").expect("cache must include 'vehsLock'");
        assert_eq!(vehs_lock.get_int(101), Some(&Value::Int(16)));
        assert!(
            diff.get("inventory").is_none(),
            "veh_lock-only diff should not emit empty inventory[1]"
        );

        let clear_diff = DiffBuilder::new().veh_lock(101, 0).build(2, 3);
        let clear_cache = clear_diff.get("cache").expect("clear diff must include 'cache'");
        let clear_locks = clear_cache.get("vehsLock").expect("cache must include 'vehsLock'");
        assert_eq!(clear_locks.get_int(101), Some(&Value::Int(0)));
    }
}
pub mod battle;
