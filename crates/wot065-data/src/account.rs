//! Account payload builders: CMD_SYNC_DATA full diff, `showGUI` ctx, shop cache and
//! dossier sync stream.
//!
//! # Client contract (decompiled 0.6.5 client, authoritative)
//!
//! The client applies a sync response with `Account._update(triggerEvents, diff)`
//! (`client/Account.py:483-503`), which feeds:
//! * `client/account_helpers/Inventory.py:55-71` (`diff['inventory']`, `diff['cache']['vehsLock']`),
//! * `client/account_helpers/Stats.py:54-100` (`diff['stats']`, `diff['cache']`,
//!   `diff['account']`, `diff['economics']`),
//! * `client/account_helpers/Trader.py:32-51` (`diff['offers']`).
//!
//! Full diff layout (see `docs/research/client-analysis.md` §4.4-§6, and the byte-exact
//! Python-2 reference in `.scratch/research/decompiled/_generated/account_sync_full.pickle`):
//!
//! ```text
//! {
//!   'rev':      int,     # new account revision (client stores it; next request sends it)
//!   'prevRev':  int,     # revision the diff applies to; 0 for the first sync
//!   'inventory': {itemTypeIdx: subdiff},   # 1 vehicle, 8 tankman, 9 optDevice, 10 shell, 11 equipment
//!   'stats':     {...},  # see below
//!   'cache':     {'battlesTillCaptcha': 0},
//!   'account':   {'clanDBID': 0, 'attrs': 0, 'premiumExpiryTime': 0, 'autoBanTime': 0, 'restrictions': []},
//! }
//! ```
//!
//! `inventory[1]` **must** contain all nine sub-keys even when empty, because
//! `InventoryParser.parseVehicles` (`gui/Scaleform/utils/requesters.py:39-56`) indexes
//! them directly (`data['compDescr']`, `data['shellsLayout']`, `data['shells']`,
//! `data['crew']`, `data['repair']`, `data['eqsLayout']`, `data['eqs']`, `data['settings']`,
//! `data['lock']`). Only `compDescr` needs entries: `{vehInvID: <15-byte binary str>}`.
//!
//! We hand out **every vehicle type in the game** as owned, one unique `vehInvID` per
//! type, with stock components. That is what makes the hangar carousel show all 130 tanks:
//! `Hangar.__updateVehicles` (`gui/Scaleform/Hangar.py:414-469`) enumerates
//! `Requester('vehicle').getFromInventory()`, i.e. exactly this dict.
//!
//! # Stats keys the hangar reads
//!
//! `Stats.synchronize` (`client/account_helpers/Stats.py:54-100`) recognises:
//! `credits`, `gold`, `slots`, `berths`, `freeXP`, `dossier`, `clanInfo`, `accOnline`,
//! `accOffline`, `freeTMenLeft`, `freeVehiclesLeft`, `captchaTriesLeft`, `hasFinPassword`,
//! `tkillIsSuspected` (simple values), `vehTypeXP` (dict), `unlocks`/`eliteVehicles`/
//! `doubleXPVehs` (**Python sets**; the client later does `set |= diff[...]`), plus
//! `cache['battlesTillCaptcha']` and the `account` section.
//!
//! `CommonPage.updateAccountInfo` (`gui/Scaleform/CommonPage.py:712-742`) reads
//! `credits`, `gold`, `freeXP`, `clanInfo`, `attrs`, `premiumExpiryTime`;
//! `Hangar.__updateVehicles` reads `slots`, `doubleXPVehs`, `eliteVehicles`.

use crate::catalog::Catalog;
use crate::pickle::{self, Value};

/// Item type indices from `common/items/__init__.py:12-14`
/// (`ITEM_TYPE_NAMES`), trimmed to the types the account sync carries.
pub mod item_type {
    pub const VEHICLE: i64 = 1;
    pub const CHASSIS: i64 = 2;
    pub const TURRET: i64 = 3;
    pub const GUN: i64 = 4;
    pub const ENGINE: i64 = 5;
    pub const FUEL_TANK: i64 = 6;
    pub const RADIO: i64 = 7;
    pub const TANKMAN: i64 = 8;
    pub const OPTIONAL_DEVICE: i64 = 9;
    pub const SHELL: i64 = 10;
    pub const EQUIPMENT: i64 = 11;
}

/// Options for the CMD_SYNC_DATA full diff. Defaults reproduce the Python-2 reference
/// payload (`account_sync_full.pickle`) exactly.
#[derive(Debug, Clone)]
pub struct FullSyncOptions {
    /// New account revision. The client stores it and sends it as `prevRev` next time.
    pub rev: i64,
    /// Revision this diff applies to. 0 for a fresh login (`AccountSyncData.revision`
    /// starts at 0; `client/AccountSyncData.py:13,84-88`).
    pub prev_rev: i64,
    pub credits: i64,
    pub gold: i64,
    pub slots: i64,
    pub berths: i64,
    pub free_xp: i64,
    pub captcha_tries_left: i64,
    /// Fill `unlocks`/`eliteVehicles`/`vehTypeXP` for every vehicle type and component
    /// (more permissive than the reference payload; useful for testing fit/unlock UI).
    pub unlock_all: bool,
}

impl Default for FullSyncOptions {
    fn default() -> Self {
        Self {
            rev: 1,
            prev_rev: 0,
            credits: 10_000_000,
            gold: 100_000,
            slots: 200,
            berths: 200,
            free_xp: 0,
            captcha_tries_left: 3,
            unlock_all: false,
        }
    }
}

/// Encode the 19-byte zero-skill `TankmanDescr` binary compact descriptor matching
/// `common/items/tankmen.py:89-106`.
pub fn make_tankman_comp_descr(
    nation_id: u8,
    vehicle_type_id: u16,
    role_id: u8,
    role_level: u8,
    first_name_id: u16,
    last_name_id: u16,
    icon_id: u16,
) -> Vec<u8> {
    let header = (item_type::TANKMAN as u8) | ((nation_id & 0x0f) << 4);
    let clamped_level = role_level.max(50);
    let rank_idx = (clamped_level - 50) / 50;
    let levels_to_next_rank = 50 - ((clamped_level - 50) % 50);

    let mut out = Vec::with_capacity(19);
    out.push(header);
    out.push(vehicle_type_id as u8);
    out.push(role_id);
    out.push(role_level);
    out.push(0); // numSkills
    out.push(0); // lastSkillLevel
    out.push(0); // flags: normal male tankman
    out.extend_from_slice(&first_name_id.to_le_bytes());
    out.extend_from_slice(&last_name_id.to_le_bytes());
    out.extend_from_slice(&icon_id.to_le_bytes());
    out.push(rank_idx);
    out.push(levels_to_next_rank);
    out.extend_from_slice(&0i32.to_le_bytes()); // freeXP
    out
}

fn pick_pool_id(pool: Option<&[u16]>, seed: usize) -> u16 {
    match pool {
        Some(ids) if !ids.is_empty() => ids[seed % ids.len()],
        _ => 1,
    }
}

/// Build the full-sync diff value. Every vehicle type from `catalog` becomes an owned
/// vehicle with its stock components and the default `vehInvID = sorted position + 1`.
pub fn build_full_sync(catalog: &Catalog, options: &FullSyncOptions) -> Value {
    let mut comp_descrs = Vec::with_capacity(catalog.len());
    let mut shells_by_veh = Vec::new();
    let mut crew_by_veh = Vec::new();
    let mut tman_comp_descrs = Vec::new();
    let mut tman_vehicles = Vec::new();
    let mut next_tman_id = 1i64;
    let mut veh_type_xp = Vec::new();
    let mut elite = Vec::new();
    let mut unlocks = Vec::new();

    for (index, vehicle) in catalog.vehicles.iter().enumerate() {
        let inv_id = index as i64 + 1;
        comp_descrs.push((Value::Int(inv_id), Value::Str(vehicle.comp_descr().to_vec())));
        if let Some(shells) = catalog
            .extras
            .default_shells
            .get(&(vehicle.nation_id, vehicle.type_id))
        {
            if !shells.is_empty() {
                let mut flat = Vec::with_capacity(shells.len() * 2);
                for &(cd, count) in shells {
                    flat.push(Value::Int(cd));
                    flat.push(Value::Int(count));
                }
                shells_by_veh.push((Value::Int(inv_id), Value::list(flat)));
            }
        }
        if let Some(roles) = catalog
            .extras
            .crew_roles
            .get(&(vehicle.nation_id, vehicle.type_id))
        {
            let pool = catalog.extras.tankman_pools.get(&vehicle.nation_id);
            let mut tman_ids = Vec::with_capacity(roles.len());
            for &role_id in roles {
                let tman_id = next_tman_id;
                next_tman_id += 1;
                let seed = (tman_id - 1) as usize;
                let first_id = pick_pool_id(pool.map(|p| p.first_names.as_slice()), seed);
                let last_id = pick_pool_id(pool.map(|p| p.last_names.as_slice()), seed);
                let icon_id = pick_pool_id(pool.map(|p| p.icons.as_slice()), seed);
                let cd = make_tankman_comp_descr(
                    vehicle.nation_id,
                    vehicle.type_id,
                    role_id,
                    100,
                    first_id,
                    last_id,
                    icon_id,
                );
                tman_ids.push(Value::Int(tman_id));
                tman_comp_descrs.push((Value::Int(tman_id), Value::Str(cd)));
                tman_vehicles.push((Value::Int(tman_id), Value::Int(inv_id)));
            }
            if !tman_ids.is_empty() {
                crew_by_veh.push((Value::Int(inv_id), Value::list(tman_ids)));
            }
        }
        if options.unlock_all {
            let type_cd = vehicle.veh_type_comp_descr as i64;
            veh_type_xp.push((Value::Int(type_cd), Value::Int(100_000)));
            elite.push(Value::Int(type_cd));
            unlocks.push(Value::Int(type_cd));
        }
    }

    if options.unlock_all {
        for vehicle in &catalog.vehicles {
            for (item_type, id) in [
                (item_type::CHASSIS, vehicle.component_ids.chassis),
                (item_type::TURRET, vehicle.component_ids.turret),
                (item_type::GUN, vehicle.component_ids.gun),
                (item_type::ENGINE, vehicle.component_ids.engine),
                (item_type::FUEL_TANK, vehicle.component_ids.fuel_tank),
                (item_type::RADIO, vehicle.component_ids.radio),
            ] {
                unlocks.push(Value::Int(
                    ((id as i64) << 8) | ((vehicle.nation_id as i64) << 4) | item_type,
                ));
            }
        }
        // De-duplicate while preserving first occurrence (the client uses a set anyway).
        let mut unique: Vec<Value> = Vec::with_capacity(unlocks.len());
        for item in unlocks {
            if !unique.contains(&item) {
                unique.push(item);
            }
        }
        unlocks = unique;
    }

    let vehicle_inventory = Value::dict(vec![
        (Value::str("compDescr"), Value::dict(comp_descrs)),
        (Value::str("shellsLayout"), Value::dict(Vec::new())),
        (Value::str("shells"), Value::dict(shells_by_veh)),
        (Value::str("crew"), Value::dict(crew_by_veh)),
        (Value::str("repair"), Value::dict(Vec::new())),
        (Value::str("eqsLayout"), Value::dict(Vec::new())),
        (Value::str("eqs"), Value::dict(Vec::new())),
        (Value::str("settings"), Value::dict(Vec::new())),
        (Value::str("lock"), Value::dict(Vec::new())),
    ]);

    let inventory = Value::dict(vec![
        (Value::Int(item_type::VEHICLE), vehicle_inventory),
        (Value::Int(item_type::CHASSIS), Value::dict(Vec::new())),
        (Value::Int(item_type::TURRET), Value::dict(Vec::new())),
        (Value::Int(item_type::GUN), Value::dict(Vec::new())),
        (Value::Int(item_type::ENGINE), Value::dict(Vec::new())),
        (Value::Int(item_type::FUEL_TANK), Value::dict(Vec::new())),
        (Value::Int(item_type::RADIO), Value::dict(Vec::new())),
        (
            Value::Int(item_type::TANKMAN),
            Value::dict(vec![
                (Value::str("compDescr"), Value::dict(tman_comp_descrs)),
                (Value::str("vehicle"), Value::dict(tman_vehicles)),
            ]),
        ),
        (Value::Int(item_type::OPTIONAL_DEVICE), Value::dict(Vec::new())),
        (Value::Int(item_type::SHELL), Value::dict(Vec::new())),
        (Value::Int(item_type::EQUIPMENT), Value::dict(Vec::new())),
    ]);

    let stats = Value::dict(vec![
        (Value::str("credits"), Value::Int(options.credits)),
        (Value::str("gold"), Value::Int(options.gold)),
        (Value::str("slots"), Value::Int(options.slots)),
        (Value::str("berths"), Value::Int(options.berths)),
        (Value::str("freeXP"), Value::Int(options.free_xp)),
        (Value::str("dossier"), Value::Str(Vec::new())),
        (Value::str("clanInfo"), Value::None),
        (Value::str("accOnline"), Value::Int(0)),
        (Value::str("accOffline"), Value::Int(0)),
        (Value::str("freeTMenLeft"), Value::Int(0)),
        (Value::str("freeVehiclesLeft"), Value::Int(0)),
        (Value::str("captchaTriesLeft"), Value::Int(options.captcha_tries_left)),
        (Value::str("hasFinPassword"), Value::Bool(false)),
        (Value::str("tkillIsSuspected"), Value::Bool(false)),
        (Value::str("vehTypeXP"), Value::dict(veh_type_xp)),
        (Value::str("unlocks"), Value::set(unlocks)),
        (Value::str("eliteVehicles"), Value::set(elite)),
        (Value::str("doubleXPVehs"), Value::set(Vec::new())),
    ]);

    let cache = Value::dict(vec![(
        Value::str("battlesTillCaptcha"),
        Value::Int(0),
    )]);

    let account = Value::dict(vec![
        (Value::str("clanDBID"), Value::Int(0)),
        (Value::str("attrs"), Value::Int(0)),
        (Value::str("premiumExpiryTime"), Value::Int(0)),
        (Value::str("autoBanTime"), Value::Int(0)),
        (Value::str("restrictions"), Value::list(Vec::new())),
    ]);

    Value::dict(vec![
        (Value::str("rev"), Value::Int(options.rev)),
        (Value::str("prevRev"), Value::Int(options.prev_rev)),
        (Value::str("inventory"), inventory),
        (Value::str("stats"), stats),
        (Value::str("cache"), cache),
        (Value::str("account"), account),
    ])
}

/// Pickled full-sync diff (`cPickle.dumps(diff, 2)` equivalent): what the baseapp returns
/// as the `ext` argument of `onCmdResponseExt(requestID, RES_SUCCESS, ext)`, or what gets
/// zlib-compressed into a resource stream for the retail CMD_SYNC_DATA path.
pub fn full_sync_pickle(catalog: &Catalog, options: &FullSyncOptions) -> Vec<u8> {
    pickle::dumps(&build_full_sync(catalog, options))
}

/// `showGUI(ctx)` argument: pickled dict. `databaseID` is **required**
/// (`client/Account.py:208-221`); `serverUTC` makes the client correct its clock
/// (`helpers.time_utils.setTimeCorrection`). This reproduces the reference
/// `showgui_ctx.pickle` exactly; use [`build_showgui_ctx_ext`] when the AOGAS session
/// timer (`client/gui/AOGAS.py:94-100`) should also be seeded.
pub fn build_showgui_ctx(database_id: i64, server_utc: i64) -> Value {
    Value::dict(vec![
        (Value::str("databaseID"), Value::Int(database_id)),
        (Value::str("serverUTC"), Value::Int(server_utc)),
    ])
}

/// Same as [`build_showgui_ctx`] plus `sessionStartedAt`, which the AOGAS notification
/// controller uses as the reference point for played-session time. Purely optional for the
/// client.
pub fn build_showgui_ctx_ext(database_id: i64, server_utc: i64, session_started_at: i64) -> Value {
    Value::dict(vec![
        (Value::str("databaseID"), Value::Int(database_id)),
        (Value::str("serverUTC"), Value::Int(server_utc)),
        (Value::str("sessionStartedAt"), Value::Int(session_started_at)),
    ])
}

/// Pickled `showGUI` argument. Unlike the sync payloads this is *not* zlib-compressed:
/// `Account.showGUI` does `ctx = cPickle.loads(ctx)` directly (`client/Account.py:208-209`).
pub fn showgui_ctx_pickle(database_id: i64, server_utc: i64) -> Vec<u8> {
    pickle::dumps(&build_showgui_ctx(database_id, server_utc))
}

/// Build the shop cache object (`shop_cache.pickle`) without catalog item prices
/// (every nation/itemType bucket is still initialized to `({}, set())` so no
/// client requester receives `None`).
pub fn build_shop_cache() -> Value {
    build_shop_cache_for_catalog(&Catalog::default())
}

/// Build the shop cache object populated with vehicles, modules, shells, and common artefacts.
pub fn build_shop_cache_for_catalog(catalog: &Catalog) -> Value {
    crate::account_state::build_shop_cache(catalog, &crate::economics::EconomicsConfig::default(), 1)
}

/// Pickled shop cache (`cPickle.dumps(shop_cache, 2)`).
pub fn shop_cache_pickle() -> Vec<u8> {
    pickle::dumps(&build_shop_cache())
}

/// Pickled shop cache populated from `catalog`.
pub fn shop_cache_pickle_for_catalog(catalog: &Catalog) -> Vec<u8> {
    pickle::dumps(&build_shop_cache_for_catalog(catalog))
}

/// Dossier sync payload: `(cacheVersion, flatList)` serialized as a tuple. The list is
/// flat quadruples `(dossierType, ownerID, changeTime, dossierCompDescr)`
/// (`client/account_helpers/DossierCache.py:110-133`); an empty list is a valid
/// "no dossiers cached yet" answer for a fresh account.
pub fn build_dossier_sync(version: i64) -> Value {
    Value::tuple(vec![Value::Int(version), Value::list(Vec::new())])
}

/// Pickled dossier sync payload.
pub fn dossier_sync_pickle(version: i64) -> Vec<u8> {
    pickle::dumps(&build_dossier_sync(version))
}

/// Turn a pickle into the BigWorld resource-stream pair the client expects for
/// `RES_STREAM` responses: `(zlib(pickle), pickle((len(zlib), signed_crc32(zlib))))`.
pub fn stream_bundle(pickled: &[u8]) -> (Vec<u8>, Vec<u8>) {
    let z = pickle::zlib_compress(pickled);
    let desc = pickle::stream_description(&z);
    (z, desc)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tiny_catalog() -> Catalog {
        let mut catalog = Catalog::default();
        catalog.vehicles.push(crate::catalog::VehicleType {
            nation: "ussr".into(),
            nation_id: 0,
            type_id: 0,
            name: "T-34".into(),
            level: 5,
            price: 256_700,
            tags: vec!["mediumTank".into()],
            veh_type_comp_descr: 1,
            veh_comp_descr_hex: "010000000000cb0003000000000000".into(),
            veh_comp_descr_b64: String::new(),
            veh_comp_descr_len: 15,
            components: crate::catalog::Components {
                chassis: "x".into(),
                engines: "x".into(),
                fuel_tanks: "x".into(),
                radios: "x".into(),
                turrets: "x".into(),
                guns: "x".into(),
            },
            component_ids: crate::catalog::ComponentIds {
                chassis: 0,
                engine: 0,
                fuel_tank: 203,
                radio: 3,
                turret: 0,
                gun: 0,
            },
            comp_descr: vec![1, 0, 0, 0, 0, 0, 0xcb, 0, 3, 0, 0, 0, 0, 0, 0],
        });
        catalog.count = 1;
        catalog
    }

    #[test]
    fn full_sync_shape() {
        let catalog = tiny_catalog();
        let diff = build_full_sync(&catalog, &FullSyncOptions::default());
        assert_eq!(diff.get("rev").unwrap().as_int(), Some(1));
        assert_eq!(diff.get("prevRev").unwrap().as_int(), Some(0));
        let inv = diff.get("inventory").unwrap();
        let veh = inv.get_int(item_type::VEHICLE).unwrap();
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
            assert!(veh.contains(key), "inventory[1] must contain {key}");
        }
        let comp = veh.get("compDescr").unwrap();
        assert_eq!(comp.get_int(1).unwrap().as_bytes().unwrap().len(), 15);
        let tankmen = inv.get_int(item_type::TANKMAN).unwrap();
        assert!(tankmen.contains("compDescr"));
        assert!(tankmen.contains("vehicle"));
        let stats = diff.get("stats").unwrap();
        assert_eq!(stats.get("credits").unwrap().as_int(), Some(10_000_000));
        assert_eq!(stats.get("slots").unwrap().as_int(), Some(200));
        assert_eq!(stats.get("clanInfo"), Some(&Value::None));
        assert!(matches!(stats.get("unlocks"), Some(Value::Set(_))));
        assert!(matches!(stats.get("doubleXPVehs"), Some(Value::Set(_))));
        let account = diff.get("account").unwrap();
        assert!(matches!(account.get("restrictions"), Some(Value::List(_))));
    }

    #[test]
    fn unlock_all_is_permissive() {
        let catalog = tiny_catalog();
        let diff = build_full_sync(
            &catalog,
            &FullSyncOptions {
                unlock_all: true,
                ..FullSyncOptions::default()
            },
        );
        let stats = diff.get("stats").unwrap();
        assert_eq!(stats.get("eliteVehicles").unwrap().as_set().unwrap().len(), 1);
        // vehTypeCompDescr + 6 component compact descriptors
        assert_eq!(stats.get("unlocks").unwrap().as_set().unwrap().len(), 7);
        assert!(stats.get("vehTypeXP").unwrap().get_int(1).is_some());
    }

    #[test]
    fn showgui_and_shop_and_dossier_shape() {
        let gui = build_showgui_ctx(1, 1_794_405_656);
        assert_eq!(gui.get("databaseID").unwrap().as_int(), Some(1));
        assert_eq!(gui.get("serverUTC").unwrap().as_int(), Some(1_794_405_656));

        let shop = build_shop_cache();
        assert_eq!(shop.get("exchangeRate").unwrap().as_int(), Some(400));
        let slots = shop.get("slotsPrices").unwrap().as_list().unwrap();
        assert_eq!(slots[0].as_int(), Some(6));
        assert_eq!(slots[1].as_list().unwrap()[0].as_int(), Some(300));

        let dossier = build_dossier_sync(0);
        assert_eq!(dossier.as_list().unwrap().len(), 2);
        assert_eq!(dossier.as_list().unwrap()[0].as_int(), Some(0));
    }

    #[test]
    fn stream_bundle_matches_desc_semantics() {
        let pickled = full_sync_pickle(&tiny_catalog(), &FullSyncOptions::default());
        let (z, desc) = stream_bundle(&pickled);
        assert_eq!(pickle::zlib_decompress(&z).unwrap(), pickled);
        let parsed = pickle::loads(&desc).unwrap();
        let items = parsed.as_list().unwrap();
        assert_eq!(items[0].as_int(), Some(z.len() as i64));
        assert_eq!(items[1].as_int(), Some(pickle::signed_crc32(&z)));
    }

    #[test]
    fn full_sync_includes_module_inventory_sections() {
        let catalog = tiny_catalog();
        let diff = build_full_sync(&catalog, &FullSyncOptions::default());
        let inv = diff.get("inventory").unwrap();
        for kind in [
            item_type::VEHICLE,
            item_type::CHASSIS,
            item_type::TURRET,
            item_type::GUN,
            item_type::ENGINE,
            item_type::FUEL_TANK,
            item_type::RADIO,
            item_type::TANKMAN,
            item_type::OPTIONAL_DEVICE,
            item_type::SHELL,
            item_type::EQUIPMENT,
        ] {
            assert!(
                matches!(inv.get_int(kind), Some(Value::Dict(_))),
                "inventory must include dict for item type {kind}"
            );
        }
    }

    #[test]
    fn shop_cache_initializes_all_nation_and_item_type_buckets() {
        let shop = build_shop_cache();
        let items = shop.get("items").unwrap();
        for (nation_id, item_types) in [
            (
                0,
                &[
                    item_type::VEHICLE,
                    item_type::CHASSIS,
                    item_type::TURRET,
                    item_type::GUN,
                    item_type::ENGINE,
                    item_type::FUEL_TANK,
                    item_type::RADIO,
                    item_type::SHELL,
                ][..],
            ),
            (
                1,
                &[
                    item_type::VEHICLE,
                    item_type::CHASSIS,
                    item_type::TURRET,
                    item_type::GUN,
                    item_type::ENGINE,
                    item_type::FUEL_TANK,
                    item_type::RADIO,
                    item_type::SHELL,
                ][..],
            ),
            (
                2,
                &[
                    item_type::VEHICLE,
                    item_type::CHASSIS,
                    item_type::TURRET,
                    item_type::GUN,
                    item_type::ENGINE,
                    item_type::FUEL_TANK,
                    item_type::RADIO,
                    item_type::SHELL,
                ][..],
            ),
            (
                15,
                &[item_type::OPTIONAL_DEVICE, item_type::EQUIPMENT][..],
            ),
        ] {
            let nation_dict = items
                .get_int(nation_id)
                .unwrap_or_else(|| panic!("shop['items'] missing nation {nation_id}"));
            for &item_type_id in item_types {
                let bucket = nation_dict
                    .get_int(item_type_id)
                    .unwrap_or_else(|| {
                        panic!("shop['items'][{nation_id}] missing item type {item_type_id}")
                    });
            let tuple = bucket.as_list().expect("bucket must be a (dict, set) tuple");
            assert_eq!(tuple.len(), 2);
            assert!(matches!(tuple[0], Value::Dict(_)));
            assert!(matches!(tuple[1], Value::Set(_)));
        }
    }
    }

    #[test]
    fn tankman_comp_descr_matches_client_binary_layout() {
        // nation=0 (ussr), vehicle_type_id=13 (MS-1), role=1 (commander), role_level=100,
        // first_name=12, last_name=345, icon=7
        let descr = make_tankman_comp_descr(0, 13, 1, 100, 12, 345, 7);
        assert_eq!(descr.len(), 19);
        assert_eq!(descr[0], 8); // itemType=8 | (0 << 4)
        assert_eq!(descr[1], 13);
        assert_eq!(descr[2], 1); // commander
        assert_eq!(descr[3], 100);
        assert_eq!(descr[4], 0); // numSkills
        assert_eq!(descr[5], 0); // lastSkillLevel
        assert_eq!(descr[6], 0); // flags (male, normal)
        assert_eq!(u16::from_le_bytes([descr[7], descr[8]]), 12);
        assert_eq!(u16::from_le_bytes([descr[9], descr[10]]), 345);
        assert_eq!(u16::from_le_bytes([descr[11], descr[12]]), 7);
        assert_eq!(descr[13], 1); // rankIdx = (100 - 50) / 50
        assert_eq!(descr[14], 50); // levelsToNextRank
        assert_eq!(i32::from_le_bytes([descr[15], descr[16], descr[17], descr[18]]), 0);
    }

    #[test]
    fn full_sync_and_shop_populate_crew_shells_and_shop_prices_from_extras() {
        let mut catalog = tiny_catalog();
        let shell_cd = crate::catalog::make_item_comp_descr(item_type::SHELL as u8, 0, 5);
        let gun_cd = crate::catalog::make_item_comp_descr(item_type::GUN as u8, 0, 0);
        catalog.extras.crew_roles.insert((0, 0), vec![1, 3, 4, 5]);
        catalog.extras.tankman_pools.insert(
            0,
            crate::catalog::TankmanNamePool {
                first_names: vec![1, 2],
                last_names: vec![10, 20],
                icons: vec![1, 2],
            },
        );
        catalog.extras.default_shells.insert((0, 0), vec![(shell_cd, 77)]);
        catalog.extras.shop_items.insert(
            (0, item_type::VEHICLE, 0),
            crate::catalog::ShopItemPrice {
                credits: 256_700,
                gold: 0,
                hidden: false,
            },
        );
        catalog.extras.shop_items.insert(
            (0, item_type::SHELL, shell_cd),
            crate::catalog::ShopItemPrice {
                credits: 45,
                gold: 0,
                hidden: false,
            },
        );
        catalog.extras.shop_items.insert(
            (0, item_type::GUN, gun_cd),
            crate::catalog::ShopItemPrice {
                credits: 2_500,
                gold: 0,
                hidden: false,
            },
        );

        let diff = build_full_sync(&catalog, &FullSyncOptions::default());
        let inv = diff.get("inventory").unwrap();
        let veh = inv.get_int(item_type::VEHICLE).unwrap();
        let crew_list = veh.get("crew").unwrap().get_int(1).unwrap().as_list().unwrap();
        assert_eq!(crew_list.len(), 4);
        let tman_cds = inv.get_int(item_type::TANKMAN).unwrap().get("compDescr").unwrap();
        let tman_vehs = inv.get_int(item_type::TANKMAN).unwrap().get("vehicle").unwrap();
        for (idx, tman_id) in crew_list.iter().enumerate() {
            let tid = tman_id.as_int().unwrap();
            let cd = tman_cds.get_int(tid).unwrap().as_bytes().unwrap();
            assert_eq!(cd.len(), 19);
            assert_eq!(cd[2], [1, 3, 4, 5][idx]);
            assert_eq!(tman_vehs.get_int(tid).unwrap().as_int(), Some(1));
        }

        let shells = veh.get("shells").unwrap().get_int(1).unwrap().as_list().unwrap();
        assert_eq!(shells.len(), 2);
        assert_eq!(shells[0].as_int(), Some(shell_cd));
        assert_eq!(shells[1].as_int(), Some(77));

        let shop = build_shop_cache_for_catalog(&catalog);
        let ussr = shop.get("items").unwrap().get_int(0).unwrap();
        let veh_bucket = ussr.get_int(item_type::VEHICLE).unwrap().as_list().unwrap();
        assert_eq!(
            veh_bucket[0].get_int(0).unwrap().as_list().unwrap(),
            &[Value::Int(256_700), Value::Int(0)]
        );
        let shell_bucket = ussr.get_int(item_type::SHELL).unwrap().as_list().unwrap();
        assert_eq!(
            shell_bucket[0].get_int(shell_cd).unwrap().as_list().unwrap(),
            &[Value::Int(45), Value::Int(0)]
        );
        let gun_bucket = ussr.get_int(item_type::GUN).unwrap().as_list().unwrap();
        assert_eq!(
            gun_bucket[0].get_int(gun_cd).unwrap().as_list().unwrap(),
            &[Value::Int(2_500), Value::Int(0)]
        );
    }
}
