//! Pure deterministic account state models, presets, profile-based sync builder,
//! and fixed shop cache.
//!
//! Mirrors WoT 0.6.5 client data structures across `client/Account.py`,
//! `client/account_helpers/Inventory.py`, `client/account_helpers/Stats.py`,
//! `client/account_helpers/Shop.py`, and `client/account_helpers/DossierCache.py`.

use std::collections::{BTreeMap, BTreeSet};
use std::str::FromStr;
use serde::{Deserialize, Serialize};

use crate::catalog::Catalog;
use crate::descr::{self, TankmanDescr, VehicleDescr};
use crate::dossier::{
    AccountDossier, DossierSyncRecord, TankmanDossier, VehicleDossier,
    DOSSIER_CACHE_VERSION, DOSSIER_TYPE_ACCOUNT,
    DOSSIER_TYPE_TANKMAN, DOSSIER_TYPE_VEHICLE,
};
use crate::economics::EconomicsConfig;
use crate::item_defs::{make_int_comp_descr, parse_int_comp_descr, VehicleDef};
use crate::pickle::Value;

/// Bitflag for premium account in `ACCOUNT_ATTR` (`common/constants.py:257`).
pub const ACCOUNT_ATTR_PREMIUM: u64 = 1u64 << 32;

/// Account starting preset (`--preset <retail|sandbox>`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Preset {
    Retail,
    Sandbox,
}

impl FromStr for Preset {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.trim().to_ascii_lowercase().as_str() {
            "retail" => Ok(Preset::Retail),
            "sandbox" => Ok(Preset::Sandbox),
            other => Err(format!(
                "unknown preset: {other:?} (expected 'retail' or 'sandbox')"
            )),
        }
    }
}

impl std::fmt::Display for Preset {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Preset::Retail => write!(f, "retail"),
            Preset::Sandbox => write!(f, "sandbox"),
        }
    }
}

/// Owned vehicle entry inside an `AccountProfile`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct VehicleEntry {
    pub comp_descr: Vec<u8>,
    pub shells: Vec<(i32, i32)>,
    pub shells_layout: Vec<((i32, i32), Vec<(i32, i32)>)>,
    pub crew: Vec<Option<i64>>,
    pub repair: Option<(i32, i32)>,
    pub eqs: [i32; 3],
    pub eqs_layout: [i32; 3],
    pub settings: i32,
    pub lock: i32,
}

impl VehicleEntry {
    pub fn get_shells_layout(&self, turret_cd: i32, gun_cd: i32) -> Option<&[(i32, i32)]> {
        self.shells_layout
            .iter()
            .find(|((t, g), _)| *t == turret_cd && *g == gun_cd)
            .map(|(_, shells)| shells.as_slice())
    }

    pub fn set_shells_layout(&mut self, turret_cd: i32, gun_cd: i32, shells: Vec<(i32, i32)>) {
        if let Some(pos) = self
            .shells_layout
            .iter()
            .position(|((t, g), _)| *t == turret_cd && *g == gun_cd)
        {
            self.shells_layout[pos].1 = shells;
        } else {
            self.shells_layout.push(((turret_cd, gun_cd), shells));
        }
    }
}

/// Owned tankman entry inside an `AccountProfile`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TankmanEntry {
    pub comp_descr: Vec<u8>,
    pub vehicle_inv_id: Option<i64>,
    /// Unix timestamp used by `CMD_SYNC_DOSSIERS` incremental sync (`> max_change_time`).
    #[serde(default)]
    pub change_time: u32,
}

/// Complete persistent account profile.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AccountProfile {
    pub schema_version: u32,
    pub database_id: i64,
    pub username: String,
    pub normalized_name: String,
    pub preset: Preset,
    pub rev: i64,
    pub creation_time: u32,
    pub last_battle_time: u32,
    pub credits: i64,
    pub gold: i64,
    pub free_xp: i64,
    pub slots: i32,
    pub berths: i32,
    pub attrs: u64,
    pub premium_expiry_time: u32,
    pub unlocks: BTreeSet<i32>,
    pub elite_vehicles: BTreeSet<i32>,
    pub double_xp_vehs: BTreeSet<i32>,
    pub veh_type_xp: BTreeMap<i32, i32>,
    pub vehicles: BTreeMap<i64, VehicleEntry>,
    pub tankmen: BTreeMap<i64, TankmanEntry>,
    pub items: BTreeMap<i32, i32>,
    pub next_inv_id: i64,
    pub account_dossier: Vec<u8>,
    pub account_dossier_change_time: u32,
    pub vehicle_dossiers: BTreeMap<i32, (u32, Vec<u8>)>,
    #[serde(default)]
    pub applied_battles: BTreeSet<u64>,
    #[serde(default)]
    pub pending_sys_messages: Vec<Vec<u8>>,
    pub language: Option<String>,
}

impl AccountProfile {
    /// Factory for a new profile seeded with a starting preset.
    pub fn new_with_preset(
        database_id: i64,
        username: impl Into<String>,
        preset: Preset,
        catalog: &Catalog,
        economics: &EconomicsConfig,
        now_utc: u32,
    ) -> Result<Self, String> {
        create_profile(database_id, username, preset, catalog, economics, now_utc)
    }

    /// Check if this account has premium status active.
    pub fn is_premium(&self) -> bool {
        (self.attrs & ACCOUNT_ATTR_PREMIUM) != 0
    }

    /// Compute a dossier `change_time` strictly newer than all existing dossier timestamps
    /// on this account so same-second commands appear in incremental `CMD_SYNC_DOSSIERS`.
    pub fn next_dossier_change_time(&self, now_utc: u32) -> u32 {
        let max_t = std::iter::once(self.creation_time)
            .chain(std::iter::once(self.account_dossier_change_time))
            .chain(self.vehicle_dossiers.values().map(|&(t, _)| t))
            .chain(self.tankmen.values().map(|t| t.change_time))
            .max()
            .unwrap_or(now_utc);
        now_utc.max(max_t.saturating_add(1))
    }
}

/// Find a vehicle definition in `catalog` by nation ID and name (falling back to tier 1).
fn find_starter_vehicle<'a>(
    catalog: &'a Catalog,
    nation_id: u8,
    name: &str,
) -> Result<&'a VehicleDef, String> {
    if let Some(v) = catalog
        .vehicle_defs
        .values()
        .find(|v| v.nation_id == nation_id && v.name.eq_ignore_ascii_case(name))
    {
        return Ok(v);
    }
    if let Some(v) = catalog
        .vehicle_defs
        .values()
        .find(|v| v.nation_id == nation_id && v.level == 1)
    {
        return Ok(v);
    }
    Err(format!(
        "starter vehicle {name} (nation {nation_id}) not found in catalog"
    ))
}

fn pick_pool_id(pool: Option<&[u16]>, seed: usize) -> u16 {
    match pool {
        Some(ids) if !ids.is_empty() => ids[seed % ids.len()],
        _ => 1,
    }
}

/// Create a new persistent account profile with initial vehicles, modules, and currencies.
pub fn create_profile(
    database_id: i64,
    username: impl Into<String>,
    preset: Preset,
    catalog: &Catalog,
    economics: &EconomicsConfig,
    now_utc: u32,
) -> Result<AccountProfile, String> {
    let username = username.into();
    let normalized_name = username.trim().to_ascii_lowercase();

    let starting = match preset {
        Preset::Retail => &economics.retail_starting,
        Preset::Sandbox => &economics.sandbox_starting,
    };

    let starter_specs = [
        (0u8, "MS-1"),
        (1u8, "Ltraktor"),
        (2u8, "T1_Cunningham"),
    ];

    let mut next_inv_id = 1i64;
    let mut vehicles = BTreeMap::new();
    let mut tankmen = BTreeMap::new();
    let mut unlocks = BTreeSet::new();
    let mut elite_vehicles = BTreeSet::new();
    let mut veh_type_xp = BTreeMap::new();
    let mut vehicle_dossiers = BTreeMap::new();

    // 1. Instantiate the 3 starter vehicles
    for &(nation_id, name) in &starter_specs {
        let veh_def = find_starter_vehicle(catalog, nation_id, name)?;
        let veh_inv_id = next_inv_id;
        next_inv_id += 1;

        let veh_descr = VehicleDescr::stock(veh_def).map_err(|e| format!("{name}: {e}"))?;
        let comp_descr = veh_descr.to_bytes();

        // Default ammo for gun
        let mut shells = Vec::new();
        if let Some(default_shells) = catalog
            .extras
            .default_shells
            .get(&(veh_def.nation_id, veh_def.type_id))
        {
            for &(cd, count) in default_shells {
                shells.push((cd as i32, count as i32));
            }
        } else if let Some(turret) = veh_def.turrets.first() {
            if let Some(gun) = turret.guns.first() {
                for shot in &gun.shots {
                    let portion = if shot.default_portion > 0.0 {
                        shot.default_portion
                    } else {
                        1.0
                    };
                    let max_ammo = gun.max_ammo as f64;
                    let count = (max_ammo * portion).round() as i32;
                    shells.push((shot.shell_cd, count));
                }
            }
        }

        let turret_cd = if let Some(t) = veh_def.turrets.first() {
            make_int_comp_descr(3, veh_def.nation_id, t.component.id)
        } else {
            0
        };
        let gun_cd = if let Some(t) = veh_def.turrets.first() {
            if let Some(g) = t.guns.first() {
                make_int_comp_descr(4, veh_def.nation_id, g.component.id)
            } else {
                0
            }
        } else {
            0
        };
        let shells_layout = vec![((turret_cd, gun_cd), shells.clone())];

        // 100% male crews with 132B TankmanDossier v10 tail
        let mut crew = Vec::with_capacity(veh_def.crew.len());
        let pool = catalog.extras.tankman_pools.get(&veh_def.nation_id);

        for (_slot_idx, crew_spec) in veh_def.crew.iter().enumerate() {
            let tman_inv_id = next_inv_id;
            next_inv_id += 1;

            let role_id = descr::role_id_by_name(&crew_spec.primary_role).unwrap_or(1);
            let seed = (tman_inv_id - 1) as usize;
            let first_name_id = pick_pool_id(pool.map(|p| p.first_names.as_slice()), seed);
            let last_name_id = pick_pool_id(pool.map(|p| p.last_names.as_slice()), seed);
            let icon_id = pick_pool_id(pool.map(|p| p.icons.as_slice()), seed);

            let tman_dossier = TankmanDossier::new().to_bytes();
            let tman_descr = TankmanDescr {
                nation_id: veh_def.nation_id,
                vehicle_type_id: veh_def.type_id as u8,
                role_id,
                role_level: 100,
                skills: Vec::new(),
                last_skill_level: 0,
                is_female: false,
                is_premium: false,
                first_name_id,
                last_name_id,
                icon_id,
                rank_idx: 1,
                num_levels_to_next_rank: 50,
                free_xp: starting.starter_tman_free_xp,
                dossier: tman_dossier,
            };

            let tman_entry = TankmanEntry {
                comp_descr: tman_descr.to_bytes(),
                vehicle_inv_id: Some(veh_inv_id),
                change_time: now_utc,
            };
            tankmen.insert(tman_inv_id, tman_entry);
            crew.push(Some(tman_inv_id));
        }

        let veh_entry = VehicleEntry {
            comp_descr,
            shells,
            shells_layout,
            crew,
            repair: None,
            eqs: [0, 0, 0],
            eqs_layout: [0, 0, 0],
            settings: 0,
            lock: 0,
        };
        vehicles.insert(veh_inv_id, veh_entry);

        // Unlocks: starter vehicle + its autounlocked items
        unlocks.insert(veh_def.veh_type_comp_descr);
        for &item_cd in &veh_def.autounlocked_items {
            unlocks.insert(item_cd);
        }

        // Vehicle dossier v17
        let v_dossier = VehicleDossier::new().to_bytes();
        vehicle_dossiers.insert(veh_def.veh_type_comp_descr, (now_utc, v_dossier));

        if preset == Preset::Sandbox {
            veh_type_xp.insert(veh_def.veh_type_comp_descr, starting.starter_veh_xp);
        }
    }

    // 2. Default-elite vehicles
    for veh_def in catalog.vehicle_defs.values() {
        if veh_def.is_default_elite {
            elite_vehicles.insert(veh_def.veh_type_comp_descr);
            if preset == Preset::Sandbox {
                veh_type_xp
                    .entry(veh_def.veh_type_comp_descr)
                    .or_insert(starting.starter_veh_xp);
            }
        }
    }

    // 3. Account dossier v19
    let account_dossier = AccountDossier::new(now_utc).to_bytes();
    let account_dossier_change_time = now_utc;

    Ok(AccountProfile {
        schema_version: 1,
        database_id,
        username,
        normalized_name,
        preset,
        rev: 1,
        creation_time: now_utc,
        last_battle_time: 0,
        credits: starting.credits,
        gold: starting.gold,
        free_xp: starting.free_xp,
        slots: starting.slots,
        berths: starting.berths,
        attrs: 0,
        premium_expiry_time: 0,
        unlocks,
        elite_vehicles,
        double_xp_vehs: BTreeSet::new(),
        veh_type_xp,
        vehicles,
        tankmen,
        items: BTreeMap::new(),
        next_inv_id,
        account_dossier,
        account_dossier_change_time,
        vehicle_dossiers,
        applied_battles: BTreeSet::new(),
        pending_sys_messages: Vec::new(),
        language: None,
    })
}

/// Build the full-sync Python dictionary from an `AccountProfile`.
pub fn build_full_sync_from_profile(profile: &AccountProfile, _catalog: &Catalog) -> Value {
    // 1. Inventory
    // 1.1 Vehicles (key 1) with all 9 subdicts
    let mut comp_descrs = Vec::with_capacity(profile.vehicles.len());
    let mut shells_layout_by_veh = Vec::with_capacity(profile.vehicles.len());
    let mut shells_by_veh = Vec::with_capacity(profile.vehicles.len());
    let mut crew_by_veh = Vec::with_capacity(profile.vehicles.len());
    let mut repair_by_veh = Vec::new();
    let mut eqs_layout_by_veh = Vec::with_capacity(profile.vehicles.len());
    let mut eqs_by_veh = Vec::with_capacity(profile.vehicles.len());
    let mut settings_by_veh = Vec::with_capacity(profile.vehicles.len());
    let mut lock_by_veh = Vec::new();

    for (&veh_inv_id, veh_entry) in &profile.vehicles {
        comp_descrs.push((Value::Int(veh_inv_id), Value::Str(veh_entry.comp_descr.clone())));

        // shellsLayout: { (turret_cd, gun_cd): [(shell_cd, count), ...] }
        let layout_dict: Vec<(Value, Value)> = veh_entry
            .shells_layout
            .iter()
            .map(|((t_cd, g_cd), shells)| {
                let key = Value::tuple(vec![Value::Int(*t_cd as i64), Value::Int(*g_cd as i64)]);
                let val_shells: Vec<Value> = shells
                    .iter()
                    .flat_map(|&(cd, count)| vec![Value::Int(cd as i64), Value::Int(count as i64)])
                    .collect();
                (key, Value::list(val_shells))
            })
            .collect();
        shells_layout_by_veh.push((Value::Int(veh_inv_id), Value::dict(layout_dict)));

        // shells: [shell_cd, count, ...]
        let flat_shells: Vec<Value> = veh_entry
            .shells
            .iter()
            .flat_map(|&(cd, count)| vec![Value::Int(cd as i64), Value::Int(count as i64)])
            .collect();
        shells_by_veh.push((Value::Int(veh_inv_id), Value::list(flat_shells)));

        // crew: [tman_id_or_none, ...]
        let crew_list: Vec<Value> = veh_entry
            .crew
            .iter()
            .map(|&opt| opt.map_or(Value::None, Value::Int))
            .collect();
        crew_by_veh.push((Value::Int(veh_inv_id), Value::list(crew_list)));

        // repair: (cost, health)
        if let Some((cost, health)) = veh_entry.repair {
            repair_by_veh.push((
                Value::Int(veh_inv_id),
                Value::tuple(vec![Value::Int(cost as i64), Value::Int(health as i64)]),
            ));
        }

        // eqsLayout & eqs: (eq0, eq1, eq2)
        let eqs_layout_vals = veh_entry.eqs_layout.iter().map(|&x| Value::Int(x as i64)).collect();
        let eqs_vals = veh_entry.eqs.iter().map(|&x| Value::Int(x as i64)).collect();
        eqs_layout_by_veh.push((Value::Int(veh_inv_id), Value::list(eqs_layout_vals)));
        eqs_by_veh.push((Value::Int(veh_inv_id), Value::list(eqs_vals)));

        settings_by_veh.push((Value::Int(veh_inv_id), Value::Int(veh_entry.settings as i64)));

        if veh_entry.lock != 0 {
            lock_by_veh.push((Value::Int(veh_inv_id), Value::Int(veh_entry.lock as i64)));
        }
    }

    let vehicle_section = Value::dict(vec![
        (Value::str("compDescr"), Value::dict(comp_descrs)),
        (Value::str("shellsLayout"), Value::dict(shells_layout_by_veh)),
        (Value::str("shells"), Value::dict(shells_by_veh)),
        (Value::str("crew"), Value::dict(crew_by_veh)),
        (Value::str("repair"), Value::dict(repair_by_veh)),
        (Value::str("eqsLayout"), Value::dict(eqs_layout_by_veh)),
        (Value::str("eqs"), Value::dict(eqs_by_veh)),
        (Value::str("settings"), Value::dict(settings_by_veh)),
        (Value::str("lock"), Value::dict(lock_by_veh)),
    ]);

    // 1.8 Tankmen
    let mut tman_comp_descrs = Vec::with_capacity(profile.tankmen.len());
    let mut tman_vehicles = Vec::new();
    for (&tman_id, tman_entry) in &profile.tankmen {
        tman_comp_descrs.push((Value::Int(tman_id), Value::Str(tman_entry.comp_descr.clone())));
        if let Some(veh_inv_id) = tman_entry.vehicle_inv_id {
            tman_vehicles.push((Value::Int(tman_id), Value::Int(veh_inv_id)));
        }
    }
    let tankman_section = Value::dict(vec![
        (Value::str("compDescr"), Value::dict(tman_comp_descrs)),
        (Value::str("vehicle"), Value::dict(tman_vehicles)),
    ]);

    // 1.2..=1.7, 1.9, 1.10, 1.11 Depot items partitioned by item_type
    let mut depot_by_type: BTreeMap<u8, Vec<(Value, Value)>> = BTreeMap::new();
    for (&comp_descr, &count) in &profile.items {
        if count > 0 {
            let item_type = parse_int_comp_descr(comp_descr).0;
            depot_by_type
                .entry(item_type)
                .or_default()
                .push((Value::Int(comp_descr as i64), Value::Int(count as i64)));
        }
    }

    let mut inventory_entries = vec![
        (Value::Int(1), vehicle_section),
        (Value::Int(8), tankman_section),
    ];
    for kind in [2u8, 3, 4, 5, 6, 7, 9, 10, 11] {
        let items_list = depot_by_type.remove(&kind).unwrap_or_default();
        inventory_entries.push((Value::Int(kind as i64), Value::dict(items_list)));
    }
    inventory_entries.sort_by_key(|(k, _)| match k {
        Value::Int(i) => *i,
        _ => 0,
    });
    let inventory = Value::dict(inventory_entries);

    // 2. Stats
    let veh_type_xp: Vec<(Value, Value)> = profile
        .veh_type_xp
        .iter()
        .map(|(&cd, &xp)| (Value::Int(cd as i64), Value::Int(xp as i64)))
        .collect();
    let unlocks: Vec<Value> = profile
        .unlocks
        .iter()
        .map(|&cd| Value::Int(cd as i64))
        .collect();
    let elite: Vec<Value> = profile
        .elite_vehicles
        .iter()
        .map(|&cd| Value::Int(cd as i64))
        .collect();
    let double_xp: Vec<Value> = profile
        .double_xp_vehs
        .iter()
        .map(|&cd| Value::Int(cd as i64))
        .collect();

    let stats = Value::dict(vec![
        (Value::str("credits"), Value::Int(profile.credits)),
        (Value::str("gold"), Value::Int(profile.gold)),
        (Value::str("slots"), Value::Int(profile.slots as i64)),
        (Value::str("berths"), Value::Int(profile.berths as i64)),
        (Value::str("freeXP"), Value::Int(profile.free_xp)),
        (Value::str("dossier"), Value::Str(profile.account_dossier.clone())),
        (Value::str("clanInfo"), Value::None),
        (Value::str("accOnline"), Value::Int(0)),
        (Value::str("accOffline"), Value::Int(0)),
        (Value::str("freeTMenLeft"), Value::Int(1000)),
        (Value::str("freeVehiclesLeft"), Value::Int(1000)),
        (Value::str("captchaTriesLeft"), Value::Int(0)),
        (Value::str("hasFinPassword"), Value::Bool(false)),
        (Value::str("tkillIsSuspected"), Value::Bool(false)),
        (Value::str("vehTypeXP"), Value::dict(veh_type_xp)),
        (Value::str("unlocks"), Value::set(unlocks)),
        (Value::str("eliteVehicles"), Value::set(elite)),
        (Value::str("doubleXPVehs"), Value::set(double_xp)),
    ]);

    // 3. Cache
    // Zero triggers a challenge before queueing. Local accounts use the client's initial count.
    let cache = Value::dict(vec![(Value::str("battlesTillCaptcha"), Value::Int(99))]);

    // 4. Account (ensuring premiumExpiryTime > 0 when ACCOUNT_ATTR.PREMIUM is set)
    let premium_expiry_time = if (profile.attrs & ACCOUNT_ATTR_PREMIUM) != 0
        && profile.premium_expiry_time == 0
    {
        profile.creation_time.saturating_add(86400 * 30)
    } else {
        profile.premium_expiry_time
    };

    let account = Value::dict(vec![
        (Value::str("clanDBID"), Value::Int(0)),
        (Value::str("attrs"), Value::Int(profile.attrs as i64)),
        (Value::str("premiumExpiryTime"), Value::Int(premium_expiry_time as i64)),
        (Value::str("autoBanTime"), Value::Int(0)),
        (Value::str("restrictions"), Value::list(Vec::new())),
    ]);

    Value::dict(vec![
        (Value::str("rev"), Value::Int(profile.rev)),
        (Value::str("inventory"), inventory),
        (Value::str("stats"), stats),
        (Value::str("cache"), cache),
        (Value::str("account"), account),
    ])
}

/// Build the shop cache dict (CMD_SYNC_SHOP) using economics config and catalog.
pub fn build_shop_cache(catalog: &Catalog, economics: &EconomicsConfig, shop_rev: i64) -> Value {
    let nation_item_types = [1i64, 2, 3, 4, 5, 6, 7, 10];
    let common_item_types = [9i64, 11];

    let mut buckets: BTreeMap<i64, BTreeMap<i64, (Vec<(Value, Value)>, Vec<Value>)>> =
        BTreeMap::new();

    for nation_id in [0i64, 1, 2] {
        let nation_map = buckets.entry(nation_id).or_default();
        for &item_type_id in &nation_item_types {
            nation_map.entry(item_type_id).or_default();
        }
    }
    let common_map = buckets.entry(15).or_default();
    for &item_type_id in &common_item_types {
        common_map.entry(item_type_id).or_default();
    }

    // Populate catalog vehicles, modules, shells from extras.shop_items
    for (&(nation_id, item_type_id, shop_key), price) in &catalog.extras.shop_items {
        let (items_dict, hidden) = buckets
            .entry(nation_id as i64)
            .or_default()
            .entry(item_type_id)
            .or_default();
        items_dict.push((
            Value::Int(shop_key),
            Value::tuple(vec![Value::Int(price.credits), Value::Int(price.gold)]),
        ));
        if price.hidden {
            hidden.push(Value::Int(shop_key));
        }
    }

    // Optional devices (type 9) in common nation 15
    for opt_dev in catalog.optional_devices.values() {
        let (items_dict, hidden) = buckets.entry(15).or_default().entry(9).or_default();
        items_dict.push((
            Value::Int(opt_dev.comp_descr as i64),
            Value::tuple(vec![
                Value::Int(opt_dev.price.0),
                Value::Int(opt_dev.price.1),
            ]),
        ));
        if !opt_dev.show_in_shop {
            hidden.push(Value::Int(opt_dev.comp_descr as i64));
        }
    }

    // Equipments (type 11) in common nation 15
    for eq in catalog.equipments.values() {
        let (items_dict, hidden) = buckets.entry(15).or_default().entry(11).or_default();
        items_dict.push((
            Value::Int(eq.comp_descr as i64),
            Value::tuple(vec![Value::Int(eq.price.0), Value::Int(eq.price.1)]),
        ));
        if !eq.show_in_shop {
            hidden.push(Value::Int(eq.comp_descr as i64));
        }
    }

    let nations: Vec<(Value, Value)> = buckets
        .into_iter()
        .map(|(nation_id, types)| {
            let type_entries: Vec<(Value, Value)> = types
                .into_iter()
                .map(|(item_type_id, (items_dict, hidden))| {
                    let bucket = Value::tuple(vec![Value::dict(items_dict), Value::Set(hidden)]);
                    (Value::Int(item_type_id), bucket)
                })
                .collect();
            (Value::Int(nation_id), Value::dict(type_entries))
        })
        .collect();

    // Camouflage cost per nation
    let mut camo_by_nation: BTreeMap<i64, Vec<(Value, Value)>> = BTreeMap::new();
    for nation_id in [0i64, 1, 2] {
        camo_by_nation.entry(nation_id).or_default();
    }
    for (&nation_id, nation_camos) in &catalog.camouflages {
        for (&camo_id, camo) in nation_camos {
            if camo.show_in_shop {
                camo_by_nation
                    .entry(nation_id as i64)
                    .or_default()
                    .push((Value::Int(camo_id as i64), Value::Int(camo.gold as i64)));
            }
        }
    }
    let camouflage_cost: Vec<(Value, Value)> = camo_by_nation
        .into_iter()
        .map(|(nid, list)| (Value::Int(nid), Value::dict(list)))
        .collect();

    let tankman_cost_tuples: Vec<Value> = economics
        .tankman_cost
        .iter()
        .map(|tier| {
            Value::dict(vec![
                (Value::str("roleLevel"), Value::Int(tier.role_level as i64)),
                (Value::str("credits"), Value::Int(tier.credits as i64)),
                (Value::str("gold"), Value::Int(tier.gold as i64)),
                (Value::str("baseRoleLoss"), Value::Float(tier.base_role_loss)),
                (
                    Value::str("classChangeRoleLoss"),
                    Value::Float(tier.class_change_role_loss),
                ),
                (Value::str("isPremium"), Value::Bool(tier.is_premium)),
            ])
        })
        .collect();

    let premium_cost_entries: Vec<(Value, Value)> = economics
        .premium_cost
        .iter()
        .map(|(&days, &cost)| (Value::Int(days as i64), Value::Int(cost as i64)))
        .collect();

    let trade_fees_dict = Value::dict(vec![
        (
            Value::str("default"),
            Value::tuple(vec![
                Value::Float(economics.trade_fees.default.0),
                Value::Float(economics.trade_fees.default.1),
            ]),
        ),
        (
            Value::str("sameClan"),
            Value::tuple(vec![
                Value::Float(economics.trade_fees.same_clan.0),
                Value::Float(economics.trade_fees.same_clan.1),
            ]),
        ),
    ]);

    let slots_prices = Value::tuple(vec![
        Value::Int(economics.slots_prices.0 as i64),
        Value::tuple(
            economics
                .slots_prices
                .1
                .iter()
                .map(|&p| Value::Int(p as i64))
                .collect(),
        ),
    ]);

    let berths_prices = Value::tuple(vec![
        Value::Int(economics.berths_prices.0 as i64),
        Value::Int(economics.berths_prices.1 as i64),
        Value::tuple(
            economics
                .berths_prices
                .2
                .iter()
                .map(|&p| Value::Int(p as i64))
                .collect(),
        ),
    ]);

    Value::dict(vec![
        (Value::str("rev"), Value::Int(shop_rev)),
        (Value::str("items"), Value::dict(nations)),
        (Value::str("sellPriceModif"), Value::Float(economics.sell_price_modif)),
        (Value::str("exchangeRate"), Value::Int(economics.exchange_rate as i64)),
        (Value::str("slotsPrices"), slots_prices),
        (Value::str("berthsPrices"), berths_prices),
        (Value::str("passportChangeCost"), Value::Int(economics.passport_change_cost as i64)),
        (Value::str("paidRemovalCost"), Value::Int(economics.paid_removal_cost as i64)),
        (
            Value::str("freeXPConversion"),
            Value::tuple(vec![
                Value::Int(economics.free_xp_conversion.0 as i64),
                Value::Int(economics.free_xp_conversion.1 as i64),
            ]),
        ),
        (Value::str("premiumCost"), Value::dict(premium_cost_entries)),
        (Value::str("tankmanCost"), Value::tuple(tankman_cost_tuples)),
        (Value::str("camouflageCost"), Value::dict(camouflage_cost)),
        (Value::str("hornCost"), Value::list(Vec::new())),
        (Value::str("tradeFees"), trade_fees_dict),
    ])
}

/// Collect dossier synchronization records for CMD_SYNC_DOSSIERS.
pub fn collect_dossier_records(
    profile: &AccountProfile,
    version: i64,
    max_change_time: u32,
) -> Vec<DossierSyncRecord> {
    let filter_by_time = version == DOSSIER_CACHE_VERSION;
    let mut records = Vec::new();

    // 1. Account dossier
    if !filter_by_time || profile.account_dossier_change_time > max_change_time {
        records.push(DossierSyncRecord {
            dossier_type: DOSSIER_TYPE_ACCOUNT,
            owner_id: profile.database_id,
            change_time: profile.account_dossier_change_time,
            comp_descr: profile.account_dossier.clone(),
        });
    }

    // 2. Vehicle dossiers
    for (&veh_type_cd, &(change_time, ref bytes)) in &profile.vehicle_dossiers {
        if !filter_by_time || change_time > max_change_time {
            records.push(DossierSyncRecord {
                dossier_type: DOSSIER_TYPE_VEHICLE,
                owner_id: veh_type_cd as i64,
                change_time,
                comp_descr: bytes.clone(),
            });
        }
    }

    // 3. Tankman dossiers
    for (&tman_inv_id, tman_entry) in &profile.tankmen {
        let change_time = if tman_entry.change_time != 0 {
            tman_entry.change_time
        } else {
            profile.creation_time
        };
        if !filter_by_time || change_time > max_change_time {
            if let Ok(descr) = TankmanDescr::from_bytes(&tman_entry.comp_descr) {
                records.push(DossierSyncRecord {
                    dossier_type: DOSSIER_TYPE_TANKMAN,
                    owner_id: tman_inv_id,
                    change_time,
                    comp_descr: descr.dossier,
                });
            } else if tman_entry.comp_descr.len() >= 132 {
                let tail = tman_entry.comp_descr[tman_entry.comp_descr.len() - 132..].to_vec();
                records.push(DossierSyncRecord {
                    dossier_type: DOSSIER_TYPE_TANKMAN,
                    owner_id: tman_inv_id,
                    change_time,
                    comp_descr: tail,
                });
            }
        }
    }

    records
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dossier::ACCOUNT_DOSSIER_VERSION;
    use crate::testutil;

    fn load_real_catalog() -> Option<Catalog> {
        let dir = testutil::game_dir()?;
        crate::catalog::load(&dir).ok()
    }

    #[test]
    fn test_retail_preset_initialization() {
        let Some(catalog) = load_real_catalog() else {
            eprintln!("skip: retail game files absent");
            return;
        };
        let economics = EconomicsConfig::default();
        let now_utc = 1_700_000_000u32;

        let profile = AccountProfile::new_with_preset(
            42,
            "Player1",
            Preset::Retail,
            &catalog,
            &economics,
            now_utc,
        )
        .expect("create retail profile");

        assert_eq!(profile.database_id, 42);
        assert_eq!(profile.username, "Player1");
        assert_eq!(profile.normalized_name, "player1");
        assert_eq!(profile.preset, Preset::Retail);
        assert_eq!(profile.credits, 10_000);
        assert_eq!(profile.gold, 0);
        assert_eq!(profile.free_xp, 0);
        assert_eq!(profile.slots, 6);
        assert_eq!(profile.berths, 16);
        assert_eq!(profile.vehicles.len(), 3);

        // Account dossier is valid v19 with creation_time == now_utc
        assert!(profile.account_dossier.len() >= 154);
        let acc_dossier = AccountDossier::from_bytes(&profile.account_dossier).expect("acc dossier");
        assert_eq!(acc_dossier.version, ACCOUNT_DOSSIER_VERSION);
        assert_eq!(acc_dossier.creation_time, now_utc);

        // Check vehicles: MS-1 (0, 13), Ltraktor (1, ...), T1_Cunningham (2, 2)
        let ms1 = catalog.vehicle_defs.values().find(|v| v.nation_id == 0 && v.name == "MS-1").unwrap();
        let ltr = catalog.vehicle_defs.values().find(|v| v.nation_id == 1 && v.name == "Ltraktor").unwrap();
        let t1 = catalog.vehicle_defs.values().find(|v| v.nation_id == 2 && v.name == "T1_Cunningham").unwrap();

        let starter_cds = BTreeSet::from([
            ms1.veh_type_comp_descr,
            ltr.veh_type_comp_descr,
            t1.veh_type_comp_descr,
        ]);

        let mut expected_unlocks = starter_cds.clone();
        for &cd in &ms1.autounlocked_items {
            expected_unlocks.insert(cd);
        }
        for &cd in &ltr.autounlocked_items {
            expected_unlocks.insert(cd);
        }
        for &cd in &t1.autounlocked_items {
            expected_unlocks.insert(cd);
        }
        assert_eq!(profile.unlocks, expected_unlocks);

        // Check tankmen have non-empty 132B TankmanDossier v10 tail
        for tman in profile.tankmen.values() {
            let descr = TankmanDescr::from_bytes(&tman.comp_descr).expect("tman descr");
            assert_eq!(descr.role_level, 100);
            assert!(!descr.is_female);
            assert_eq!(descr.free_xp, 0);
            assert_eq!(descr.dossier.len(), 132);
            let d = TankmanDossier::from_bytes(&descr.dossier).expect("tman dossier");
            assert_eq!(d.version, 10);
        }
    }

    #[test]
    fn test_sandbox_preset_initialization() {
        let Some(catalog) = load_real_catalog() else {
            eprintln!("skip: retail game files absent");
            return;
        };
        let economics = EconomicsConfig::default();
        let now_utc = 1_700_000_000u32;

        let profile = AccountProfile::new_with_preset(
            99,
            "SandboxTester",
            Preset::Sandbox,
            &catalog,
            &economics,
            now_utc,
        )
        .expect("create sandbox profile");

        assert_eq!(profile.credits, 50_000_000);
        assert_eq!(profile.gold, 1_000_000);
        assert_eq!(profile.free_xp, 5_000_000);
        assert_eq!(profile.vehicles.len(), 3);

        // Starter tankmen have 1_000_000 free_xp
        for tman in profile.tankmen.values() {
            let descr = TankmanDescr::from_bytes(&tman.comp_descr).expect("tman descr");
            assert_eq!(descr.free_xp, 1_000_000);
            assert_eq!(descr.dossier.len(), 132);
        }

        // Starter tanks have 500_000 veh_type_xp
        let ms1 = catalog.vehicle_defs.values().find(|v| v.nation_id == 0 && v.name == "MS-1").unwrap();
        assert_eq!(
            profile.veh_type_xp.get(&ms1.veh_type_comp_descr),
            Some(&500_000)
        );

        // Default-elite vehicles also have 500_000 veh_type_xp
        for &elite_cd in &profile.elite_vehicles {
            assert_eq!(profile.veh_type_xp.get(&elite_cd), Some(&500_000));
        }
    }

    #[test]
    fn test_build_full_sync_from_profile_structure() {
        let Some(catalog) = load_real_catalog() else {
            eprintln!("skip: retail game files absent");
            return;
        };
        let economics = EconomicsConfig::default();
        let profile = AccountProfile::new_with_preset(
            1,
            "Player1",
            Preset::Retail,
            &catalog,
            &economics,
            1_700_000_000,
        )
        .unwrap();

        let sync = build_full_sync_from_profile(&profile, &catalog);

        // Check inventory keys 1..=11
        let inv = sync.get("inventory").expect("inventory");
        for kind in 1..=11 {
            assert!(inv.get_int(kind).is_some(), "missing inventory[{kind}]");
        }

        // Check inventory[1] 9 subdicts
        let veh = inv.get_int(1).unwrap();
        for key in [
            "compDescr",
            "shellsLayout",
            "shells",
            "crew",
            "repair",
            "eqsLayout",
            "eqs",
            "settings",
            "lock",
        ] {
            assert!(veh.get(key).is_some(), "inventory[1] missing {key}");
        }

        // Stats
        let stats = sync.get("stats").expect("stats");
        assert_eq!(stats.get("freeVehiclesLeft").unwrap().as_int(), Some(1000));
        assert_eq!(stats.get("freeTMenLeft").unwrap().as_int(), Some(1000));

        // Account premium check
        let account = sync.get("account").expect("account");
        assert_eq!(account.get("attrs").unwrap().as_int(), Some(0));
        assert_eq!(account.get("premiumExpiryTime").unwrap().as_int(), Some(0));

        // Test with PREMIUM attribute active
        let mut premium_profile = profile.clone();
        premium_profile.attrs |= ACCOUNT_ATTR_PREMIUM;
        let prem_sync = build_full_sync_from_profile(&premium_profile, &catalog);
        let prem_acc = prem_sync.get("account").unwrap();
        assert!(prem_acc.get("premiumExpiryTime").unwrap().as_int().unwrap() > 0);
    }

    #[test]
    fn test_build_shop_cache_content() {
        let Some(catalog) = load_real_catalog() else {
            eprintln!("skip: retail game files absent");
            return;
        };
        let economics = EconomicsConfig::default();
        let shop = build_shop_cache(&catalog, &economics, 1);

        assert_eq!(shop.get("rev").unwrap().as_int(), Some(1));
        assert_eq!(shop.get("exchangeRate").unwrap().as_int(), Some(400));

        let items = shop.get("items").expect("items");
        // Nation 0 vehicles keyed by innation_id (type_id)
        let n0 = items.get_int(0).unwrap();
        let n0_veh = n0.get_int(1).unwrap().as_list().unwrap();
        let n0_veh_dict = n0_veh[0].as_dict().unwrap();
        // MS-1 has type_id 13
        assert!(n0_veh_dict.iter().any(|(k, _)| k.as_int() == Some(13)));

        // Nation 15 optional devices (9) and equipments (11) keyed by compDescr
        let n15 = items.get_int(15).unwrap();
        let n15_opt = n15.get_int(9).unwrap().as_list().unwrap();
        assert!(!n15_opt[0].as_dict().unwrap().is_empty());
        let n15_eq = n15.get_int(11).unwrap().as_list().unwrap();
        assert!(!n15_eq[0].as_dict().unwrap().is_empty());

        // CamouflageCost populated per nation
        let camo = shop.get("camouflageCost").expect("camouflageCost");
        assert!(camo.get_int(0).is_some());
        assert!(camo.get_int(1).is_some());
        assert!(camo.get_int(2).is_some());
    }

    #[test]
    fn test_collect_dossier_records() {
        let Some(catalog) = load_real_catalog() else {
            eprintln!("skip: retail game files absent");
            return;
        };
        let economics = EconomicsConfig::default();
        let profile = AccountProfile::new_with_preset(
            123,
            "PlayerDossier",
            Preset::Retail,
            &catalog,
            &economics,
            1_000,
        )
        .unwrap();

        // Query with version=0 (forced full sync)
        let records = collect_dossier_records(&profile, 0, 0);
        assert!(!records.is_empty());

        // Must include account (type 1)
        assert!(records.iter().any(|r| r.dossier_type == DOSSIER_TYPE_ACCOUNT && r.owner_id == 123));

        // Must include vehicles (type 2)
        assert!(records.iter().any(|r| r.dossier_type == DOSSIER_TYPE_VEHICLE));

        // Must include tankmen with DOSSIER_TYPE_TANKMAN (4) and 132B comp_descr
        let tman_recs: Vec<_> = records
            .iter()
            .filter(|r| r.dossier_type == DOSSIER_TYPE_TANKMAN)
            .collect();
        assert_eq!(tman_recs.len(), profile.tankmen.len());
        for rec in tman_recs {
            assert_eq!(rec.comp_descr.len(), 132);
        }

        // Query with version == DOSSIER_CACHE_VERSION and max_change_time >= 1000: skips unchanged records
        let cached = collect_dossier_records(&profile, DOSSIER_CACHE_VERSION, 1_000);
        assert!(cached.is_empty());
    }

    #[test]
    fn test_collect_dossier_records_includes_newly_bought_tankman_same_second() {
        let Some(catalog) = load_real_catalog() else {
            eprintln!("skip: retail game files absent");
            return;
        };
        let economics = EconomicsConfig::default();
        let mut profile = AccountProfile::new_with_preset(
            456,
            "DossierRecruit",
            Preset::Sandbox,
            &catalog,
            &economics,
            1_000,
        )
        .unwrap();
        let ms1 = catalog
            .vehicle_defs
            .values()
            .find(|v| v.nation_id == 0 && v.name == "MS-1")
            .unwrap();

        let outcome = crate::ops::AccountEngine::apply(
            &mut profile,
            &crate::ops::EngineCmd::BuyTman {
                shop_rev: 1,
                veh_type_cd: ms1.veh_type_comp_descr,
                role_idx: 1, // commander
                tman_cost_idx: 0,
            },
            &catalog,
            &economics,
            1,
            1_000, // same wall-clock second as creation_time
        )
        .expect("recruit tankman");
        assert!(outcome.resync_dossiers);
        let new_tman_id = outcome
            .ext
            .as_ref()
            .and_then(|v| v.get("tmanInvID"))
            .and_then(|v| v.as_int())
            .expect("tmanInvID in ext");

        let delta_records = collect_dossier_records(&profile, DOSSIER_CACHE_VERSION, 1_000);
        assert!(
            delta_records
                .iter()
                .any(|r| r.dossier_type == DOSSIER_TYPE_TANKMAN && r.owner_id == new_tman_id),
            "incremental CMD_SYNC_DOSSIERS after BUY_TMAN must include new tankman {new_tman_id}; got {delta_records:?}"
        );
    }

    #[test]
    fn test_account_profile_json_roundtrip() {
        let Some(catalog) = load_real_catalog() else {
            eprintln!("skip: retail game files absent");
            return;
        };
        let economics = EconomicsConfig::default();
        let mut profile = AccountProfile::new_with_preset(
            777,
            "SerdeTest",
            Preset::Retail,
            &catalog,
            &economics,
            1_700_000_000,
        )
        .unwrap();
        profile.items.insert(make_int_comp_descr(10, 0, 1), 50);

        let json = serde_json::to_string_pretty(&profile).expect("serialize profile");
        let parsed: AccountProfile = serde_json::from_str(&json).expect("deserialize profile");
        assert_eq!(profile, parsed);
    }
}
