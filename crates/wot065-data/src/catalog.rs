//! Parser for the WoT 0.6.5 vehicle definitions (`res/scripts/item_defs/vehicles`).
//!
//! Source of truth: the retail game's own tree, which exists twice on a development
//! machine and is byte-for-byte the same data:
//! * packed BigWorld XML in the game install (`...\World_of_Tanks\res\scripts\item_defs\
//!   vehicles\<nation>\...`), read here through [`crate::xml`];
//! * pre-unpacked `.xml.xml` copies used while reverse-engineering
//!   (`.scratch/research/unpacked/...`), read by the same code.
//!
//! ## Layout
//!
//! `vehicles/<nation>/list.xml` has one child per vehicle type; the child's tag is the
//! internal name and its `<id>` is the **nation-local vehicle type id** (sequential from
//! zero). Each vehicle has `<nation>/<Name>.xml` with sections `<chassis>`, `<engines>`,
//! `<fuelTanks>`, `<radios>` and `<turrets0>` (0.6.5 always has exactly one turret
//! position; verified for all 130 vehicles). The first child of each section is the stock
//! (top of the research tree) module; the stock gun is the first gun of the stock turret.
//! Component ids are nation-local and come from
//! `<nation>/components/{chassis,engines,fuelTanks,radios,turrets,guns}.xml` `<ids>`.
//!
//! ## Compact descriptors (see `common/items/vehicles.py` in the decompiled client)
//!
//! * `vehTypeCompDescr` (int32): `(typeID << 8) | (nationID << 4) | 1`
//!   (`makeIntCompactDescrByID`, vehicles.py:793-795; item type 1 = vehicle).
//! * `vehCompDescr` (Python-2 binary `str`, 15 bytes for every 0.6.5 vehicle):
//!   ```text
//!   struct.pack('<2B4H', header, typeID, chassisID, engineID, fuelTankID, radioID)
//!   + struct.pack('<2H', turretID, gunID)   # once per turret position (always 1 here)
//!   + chr(flags)                            # 0x00: no optional devices/emblem/horn/camo
//!   header = ITEM_TYPE_IDS.vehicle(1) | (nationID << 4)
//!   ```
//!   (`VehicleDescr.makeCompactDescr`, vehicles.py:345-385; decoding at
//!   `VehicleDescr.__initFromCompactDescr`, vehicles.py:566-631.)

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::{Path, PathBuf};

use base64::Engine;

use crate::item_defs;
pub use crate::item_defs::{
    make_int_comp_descr, make_item_comp_descr, parse_int_comp_descr, ArmorZoneMap,
    CamouflageDef, ComponentDef, CrewSlotSpec, DeviceHealthDef, EquipmentDef,
    EquipmentEffect, FilterItem, FilterPattern, HitTesterSpec, HornDef, ItemCatalogExtras,
    OptionalDeviceDef, OptionalDeviceEffect, PlayerEmblemDef, ShellDef, ShellKind,
    ShellXmlDef, ShopItemPrice, ShotDef, TankmanGroupDef, TankmanNamePool, TankmenConfig,
    TurretDef, TurretGunEntry, UnlockEntry, VehicleBattleDef, VehicleDef, VehicleFilter,
    NATION_NONE,
};
use crate::xml::{self, XmlNode};

/// Nations in the order the client indexes them (`common/nations.py`: NAMES and INDICES).
/// The order matters for `nationID = (cd >> 4) & 15` and for the catalog/inventory order.
pub const NATIONS: [(&str, u8); 3] = [("ussr", 0), ("germany", 1), ("usa", 2)];

/// Item type id of a vehicle (`common/items/__init__.py`: ITEM_TYPE_NAMES[1]).
pub const ITEM_TYPE_VEHICLE: i32 = 1;

/// Errors that prevent loading the catalog at all. Per-vehicle problems are collected in
/// [`Catalog::problems`] instead: a vehicle whose component ids cannot be resolved is
/// skipped (and reported), the rest of the catalog loads.
#[derive(Debug)]
pub struct CatalogError(pub String);

impl std::fmt::Display for CatalogError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "catalog error: {}", self.0)
    }
}

impl std::error::Error for CatalogError {}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Components {
    pub chassis: String,
    pub engines: String,
    #[serde(rename = "fuelTanks")]
    pub fuel_tanks: String,
    pub radios: String,
    pub turrets: String,
    pub guns: String,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ComponentIds {
    pub chassis: u16,
    pub engine: u16,
    #[serde(rename = "fuelTank")]
    pub fuel_tank: u16,
    pub radio: u16,
    pub turret: u16,
    pub gun: u16,
}

/// One vehicle type with everything the account layer needs: identity, tier/price/tags and
/// both compact descriptors. The JSON field names intentionally mirror the reference
/// `vehicles_catalog.json` produced during the reverse-engineering pass.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct VehicleType {
    pub nation: String,
    #[serde(rename = "nationID")]
    pub nation_id: u8,
    #[serde(rename = "typeID")]
    pub type_id: u16,
    pub name: String,
    pub level: u8,
    pub price: i64,
    pub tags: Vec<String>,
    #[serde(rename = "vehTypeCompDescr")]
    pub veh_type_comp_descr: i32,
    #[serde(rename = "vehCompDescrHex")]
    pub veh_comp_descr_hex: String,
    #[serde(rename = "vehCompDescrB64")]
    pub veh_comp_descr_b64: String,
    #[serde(rename = "vehCompDescrLen")]
    pub veh_comp_descr_len: usize,
    pub components: Components,
    #[serde(rename = "componentIDs")]
    pub component_ids: ComponentIds,
    /// Raw 15-byte `vehCompDescr` (not serialized; `veh_comp_descr_hex` carries it in JSON).
    #[serde(skip)]
    pub comp_descr: Vec<u8>,
}

impl VehicleType {
    /// Python-2 `str` value stored in `inventory['compDescr'][vehInvID]`.
    pub fn comp_descr(&self) -> &[u8] {
        &self.comp_descr
    }

    /// 1-based position in the catalog (= the default `vehInvID` we hand out).
    pub fn index_in(&self, catalog: &Catalog) -> Option<usize> {
        catalog
            .vehicles
            .iter()
            .position(|v| v.nation_id == self.nation_id && v.type_id == self.type_id)
    }
}

/// Parsed vehicle catalog, sorted by `(nationID, typeID)` exactly like the reference
/// `vehicles_catalog.json`. The inventory builder uses the sorted position as `vehInvID`
/// (1-based), which reproduces the reference `account_sync_full.pickle`.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct Catalog {
    pub source: String,
    #[serde(rename = "count")]
    pub count: usize,
    pub vehicles: Vec<VehicleType>,
    #[serde(skip)]
    pub problems: Vec<String>,
    #[serde(skip)]
    pub extras: ItemCatalogExtras,
    #[serde(skip)]
    pub vehicle_defs: BTreeMap<i32, VehicleDef>,
    #[serde(skip)]
    pub optional_devices: BTreeMap<i32, OptionalDeviceDef>,
    #[serde(skip)]
    pub equipments: BTreeMap<i32, EquipmentDef>,
    #[serde(skip)]
    pub camouflages: BTreeMap<u8, BTreeMap<u16, CamouflageDef>>,
    #[serde(skip)]
    pub player_emblems: BTreeMap<u8, PlayerEmblemDef>,
    #[serde(skip)]
    pub player_emblem_ids: BTreeMap<String, u8>,
    #[serde(skip)]
    pub horns: BTreeMap<u8, HornDef>,
    #[serde(skip)]
    pub tankmen_configs: BTreeMap<u8, TankmenConfig>,
    #[serde(skip)]
    pub components: BTreeMap<i32, ComponentDef>,
    #[serde(skip)]
    pub shells: BTreeMap<i32, ShellXmlDef>,
}

impl Catalog {
    pub fn len(&self) -> usize {
        self.vehicles.len()
    }

    pub fn is_empty(&self) -> bool {
        self.vehicles.is_empty()
    }

    /// `(nation name, vehicle count)` in nation order, always all three nations.
    pub fn counts_by_nation(&self) -> Vec<(&'static str, usize)> {
        NATIONS
            .iter()
            .map(|(name, id)| {
                (
                    *name,
                    self.vehicles.iter().filter(|v| v.nation_id == *id).count(),
                )
            })
            .collect()
    }

    /// Find a vehicle type by internal name (e.g. `"T-34"`).
    pub fn by_name(&self, name: &str) -> Option<&VehicleType> {
        self.vehicles.iter().find(|v| v.name == name)
    }

    /// Find a vehicle type by `(nationID, typeID)`.
    pub fn by_type_id(&self, nation_id: u8, type_id: u16) -> Option<&VehicleType> {
        self.vehicles
            .iter()
            .find(|v| v.nation_id == nation_id && v.type_id == type_id)
    }

    /// Default inventory ids: sorted position + 1 (`T-34` -> 1, ..., `M22_Locust` -> 130).
    pub fn vehicle_inv_ids(&self) -> Vec<(String, i64)> {
        self.vehicles
            .iter()
            .enumerate()
            .map(|(i, v)| (v.name.clone(), i as i64 + 1))
            .collect()
    }

    /// All `vehTypeCompDescr` ints (for `stats.unlocks` / `eliteVehicles`).
    pub fn type_comp_descrs(&self) -> Vec<i64> {
        self.vehicles
            .iter()
            .map(|v| v.veh_type_comp_descr as i64)
            .collect()
    }

    /// Lookup static `VehicleDef` by integer compact descriptor (`vehTypeCompDescr`).
    pub fn vehicle_def(&self, veh_type_cd: i32) -> Option<&VehicleDef> {
        self.vehicle_defs.get(&veh_type_cd)
    }

    /// Lookup static `VehicleDef` by `(nation_id, type_id)`.
    pub fn vehicle_def_by_id(&self, nation_id: u8, type_id: u16) -> Option<&VehicleDef> {
        let cd = make_int_comp_descr(1, nation_id, type_id);
        self.vehicle_defs.get(&cd)
    }

    /// Lookup static `ComponentDef` by compact descriptor.
    pub fn component(&self, comp_descr: i32) -> Option<&ComponentDef> {
        self.components.get(&comp_descr)
    }

    /// Lookup static `OptionalDeviceDef` by compact descriptor.
    pub fn optional_device(&self, comp_descr: i32) -> Option<&OptionalDeviceDef> {
        self.optional_devices.get(&comp_descr)
    }

    /// Lookup static `EquipmentDef` by compact descriptor.
    pub fn equipment(&self, comp_descr: i32) -> Option<&EquipmentDef> {
        self.equipments.get(&comp_descr)
    }

    /// Lookup static `ShellXmlDef` by compact descriptor.
    pub fn resolve_shell(&self, shell_cd: i32) -> Option<&ShellXmlDef> {
        self.shells.get(&shell_cd)
    }

    /// Lookup static `CamouflageDef` by nation ID and camouflage ID.
    pub fn camouflage(&self, nation_id: u8, camo_id: u16) -> Option<&CamouflageDef> {
        self.camouflages.get(&nation_id)?.get(&camo_id)
    }

    /// Lookup static `PlayerEmblemDef` by emblem ID.
    pub fn player_emblem(&self, emblem_id: u8) -> Option<&PlayerEmblemDef> {
        self.player_emblems.get(&emblem_id)
    }

    /// Lookup static `HornDef` by horn ID.
    pub fn horn(&self, horn_id: u8) -> Option<&HornDef> {
        self.horns.get(&horn_id)
    }

    /// Lookup `TankmenConfig` for a nation.
    pub fn tankmen_config(&self, nation_id: u8) -> Option<&TankmenConfig> {
        self.tankmen_configs.get(&nation_id)
    }
}

/// Resolve the `item_defs/vehicles` directory from whatever the user passed as
/// `--game-dir`: the installation root, the `res` directory, `.../item_defs`, or the
/// vehicles directory itself.
pub fn resolve_vehicles_dir(dir: &Path) -> Option<PathBuf> {
    let candidates = [
        dir.join("res/scripts/item_defs/vehicles"),
        dir.join("scripts/item_defs/vehicles"),
        dir.join("item_defs/vehicles"),
        dir.join("vehicles"),
        dir.to_path_buf(),
    ];
    for candidate in candidates {
        if candidate.is_dir()
            && NATIONS
                .iter()
                .any(|(nation, _)| candidate.join(nation).is_dir())
        {
            return Some(candidate);
        }
    }
    None
}

/// Load and parse the complete vehicle catalog.
pub fn load(game_dir: &Path) -> Result<Catalog, CatalogError> {
    let vehicles_dir = resolve_vehicles_dir(game_dir).ok_or_else(|| {
        CatalogError(format!(
            "could not find item_defs/vehicles under {} (expected res/scripts/item_defs/vehicles)",
            game_dir.display()
        ))
    })?;
    load_from_vehicles_dir(&vehicles_dir)
}

/// Load from an already-resolved `item_defs/vehicles` directory.
pub fn load_from_vehicles_dir(vehicles_dir: &Path) -> Result<Catalog, CatalogError> {
    let mut catalog = Catalog {
        source: format!(
            "item_defs/vehicles ({}) + decompiled common/items/vehicles.py",
            vehicles_dir.display()
        ),
        ..Catalog::default()
    };

    let common_dir = vehicles_dir.join("common");
    let projectile_speed_factor = if common_dir.is_dir() {
        item_defs::load_common_artefacts(&common_dir, &mut catalog.extras)
            .map_err(|e| CatalogError(format!("common artefacts: {e}")))?;
        catalog.optional_devices = item_defs::load_optional_devices(&common_dir)
            .map_err(|e| CatalogError(format!("optional devices: {e}")))?;
        catalog.equipments = item_defs::load_equipments(&common_dir)
            .map_err(|e| CatalogError(format!("equipments: {e}")))?;
        let (emblems, emblem_ids) = item_defs::load_player_emblems(&common_dir)
            .map_err(|e| CatalogError(format!("player emblems: {e}")))?;
        catalog.player_emblems = emblems;
        catalog.player_emblem_ids = emblem_ids;
        catalog.horns = item_defs::load_horns(&common_dir)
            .map_err(|e| CatalogError(format!("horns: {e}")))?;
        item_defs::read_projectile_speed_factor(&common_dir)
    } else {
        0.8
    };
    let tankmen_dir = vehicles_dir
        .parent()
        .map(|p| p.join("tankmen"))
        .filter(|p| p.is_dir());

    for (nation, nation_id) in NATIONS {
        let nation_dir = vehicles_dir.join(nation);
        if !nation_dir.is_dir() {
            return Err(CatalogError(format!(
                "missing nation directory {}",
                nation_dir.display()
            )));
        }
        if let Some(tdir) = &tankmen_dir {
            let pool = item_defs::load_tankman_pool(tdir, nation)
                .map_err(|e| CatalogError(format!("{nation}: tankmen: {e}")))?;
            catalog.extras.tankman_pools.insert(nation_id, pool);
            let tman_cfg = item_defs::load_nation_tankmen_config(tdir, nation)
                .map_err(|e| CatalogError(format!("{nation}: tankmen config: {e}")))?;
            catalog.tankmen_configs.insert(nation_id, tman_cfg);
        }
        let ids = ComponentTables::load(
            &nation_dir,
            nation_id,
            projectile_speed_factor,
            &mut catalog.extras,
            &mut catalog.components,
            &mut catalog.shells,
        )
        .map_err(|e| CatalogError(format!("{nation}: components: {e}")))?;
        let list = read_stem(&nation_dir, "list")
            .map_err(|e| CatalogError(format!("{nation}: {e}")))?;

        let mut veh_names_to_ids = HashMap::new();
        for entry in &list.children {
            if let Some(id) = entry.child("id").and_then(XmlNode::parse_i64) {
                veh_names_to_ids.insert(entry.name.clone(), id as u16);
            }
        }

        let camos = item_defs::load_nation_customization(
            &nation_dir,
            nation_id,
            &veh_names_to_ids,
        )
        .map_err(|e| CatalogError(format!("{nation}: customization: {e}")))?;
        catalog.camouflages.insert(nation_id, camos);

        for entry in &list.children {
            let name = entry.name.clone();
            match parse_vehicle(
                &nation_dir,
                nation,
                nation_id,
                &name,
                entry,
                &ids,
                &veh_names_to_ids,
                &catalog.player_emblem_ids,
                &mut catalog.extras,
                &mut catalog.components,
            ) {
                Ok((vehicle, veh_def)) => {
                    catalog.vehicles.push(vehicle);
                    catalog
                        .vehicle_defs
                        .insert(veh_def.veh_type_comp_descr, veh_def);
                }
                Err(problem) => catalog.problems.push(format!("{nation}/{name}: {problem}")),
            }
        }
    }

    catalog
        .vehicles
        .sort_by_key(|v| (v.nation_id, v.type_id));
    catalog.count = catalog.vehicles.len();
    Ok(catalog)
}

/// Read a resource file given its stem: the retail tree packs it as `<stem>.xml`, the
/// pre-unpacked development tree (`.scratch/research/unpacked/...`) uses
/// `<stem>.xml.xml`. Both parse to the same tree.
pub(crate) fn read_stem(dir: &Path, stem: &str) -> Result<XmlNode, String> {
    for suffix in [".xml", ".xml.xml"] {
        let path = dir.join(format!("{stem}{suffix}"));
        if path.is_file() {
            return xml::read_xml(&path).map_err(|e| format!("{}: {e}", path.display()));
        }
    }
    Err(format!(
        "missing {}/{stem}.xml (looked for .xml and .xml.xml)",
        dir.display()
    ))
}

/// Per-nation component id tables + `<shared>` element maps.
struct ComponentTables {
    chassis: HashMap<String, u16>,
    engines: HashMap<String, u16>,
    fuel_tanks: HashMap<String, u16>,
    radios: HashMap<String, u16>,
    turrets: HashMap<String, u16>,
    guns: HashMap<String, u16>,
    shells: HashMap<String, u16>,
    shell_defs: BTreeMap<i32, ShellXmlDef>,
    projectile_speed_factor: f64,
    shared_chassis: HashMap<String, ComponentDef>,
    shared_engines: HashMap<String, ComponentDef>,
    shared_fuel_tanks: HashMap<String, ComponentDef>,
    shared_radios: HashMap<String, ComponentDef>,
    shared_turrets: HashMap<String, ComponentDef>,
    shared_guns: HashMap<String, ComponentDef>,
    shared_turret_nodes: HashMap<String, XmlNode>,
    shared_gun_nodes: HashMap<String, XmlNode>,
}

impl ComponentTables {
    fn load(
        nation_dir: &Path,
        nation_id: u8,
        projectile_speed_factor: f64,
        extras: &mut ItemCatalogExtras,
        components_out: &mut BTreeMap<i32, ComponentDef>,
        shells_out: &mut BTreeMap<i32, ShellXmlDef>,
    ) -> Result<Self, String> {
        let components_dir = nation_dir.join("components");
        let read = |file: &str| read_stem(&components_dir, file);
        let chassis_xml = read("chassis")?;
        let engines_xml = read("engines")?;
        let fuel_tanks_xml = read("fuelTanks")?;
        let radios_xml = read("radios")?;
        let turrets_xml = read("turrets")?;
        let guns_xml = read("guns")?;

        let chassis = read_ids(&chassis_xml);
        let engines = read_ids(&engines_xml);
        let fuel_tanks = read_ids(&fuel_tanks_xml);
        let radios = read_ids(&radios_xml);
        let turrets = read_ids(&turrets_xml);
        let guns = read_ids(&guns_xml);

        for (root, ids, item_type_id) in [
            (&chassis_xml, &chassis, 2i64),
            (&turrets_xml, &turrets, 3i64),
            (&guns_xml, &guns, 4i64),
            (&engines_xml, &engines, 5i64),
            (&fuel_tanks_xml, &fuel_tanks, 6i64),
            (&radios_xml, &radios, 7i64),
        ] {
            item_defs::record_shared_component_prices(root, ids, nation_id, item_type_id, extras);
        }

        let (shells, shell_defs) =
            item_defs::load_nation_shells_with_defs(&components_dir, nation_id, extras)?;
        shells_out.extend(shell_defs.iter().map(|(&k, v)| (k, v.clone())));

        let mut parse_shared_list = |root: &XmlNode,
                                     id_map: &HashMap<String, u16>,
                                     item_type_id: u8,
                                     shell_ids: Option<&HashMap<String, u16>>|
         -> HashMap<String, ComponentDef> {
            let mut map = HashMap::new();
            if let Some(shared) = root.child("shared") {
                for child in &shared.children {
                    if let Some(&id) = id_map.get(&child.name) {
                        let def = item_defs::parse_component_def_full(
                            child,
                            id,
                            item_type_id,
                            nation_id,
                            shell_ids,
                            shell_ids.map(|_| &shell_defs),
                            projectile_speed_factor,
                        );
                        components_out.insert(def.comp_descr, def.clone());
                        map.insert(child.name.clone(), def);
                    }
                }
            }
            map
        };

        let shared_chassis = parse_shared_list(&chassis_xml, &chassis, 2, None);
        let shared_engines = parse_shared_list(&engines_xml, &engines, 5, None);
        let shared_fuel_tanks = parse_shared_list(&fuel_tanks_xml, &fuel_tanks, 6, None);
        let shared_radios = parse_shared_list(&radios_xml, &radios, 7, None);
        let shared_turrets = parse_shared_list(&turrets_xml, &turrets, 3, None);
        let shared_guns = parse_shared_list(&guns_xml, &guns, 4, Some(&shells));

        Ok(Self {
            chassis,
            engines,
            fuel_tanks,
            radios,
            turrets,
            guns,
            shells,
            shell_defs,
            projectile_speed_factor,
            shared_chassis,
            shared_engines,
            shared_fuel_tanks,
            shared_radios,
            shared_turrets,
            shared_guns,
            shared_turret_nodes: read_shared(&turrets_xml),
            shared_gun_nodes: read_shared(&guns_xml),
        })
    }
}

fn read_ids(root: &XmlNode) -> HashMap<String, u16> {
    let mut out = HashMap::new();
    if let Some(ids) = root.child("ids") {
        for child in &ids.children {
            if let Some(id) = child.parse_i64() {
                if id >= 0 && id <= u16::MAX as i64 {
                    out.insert(child.name.clone(), id as u16);
                }
            }
        }
    }
    out
}

fn read_shared(root: &XmlNode) -> HashMap<String, XmlNode> {
    let mut out = HashMap::new();
    if let Some(shared) = root.child("shared") {
        for child in &shared.children {
            out.insert(child.name.clone(), child.clone());
        }
    }
    out
}

fn parse_unlocks_into(
    unlocks_node: &XmlNode,
    prereq_cds: &[i32],
    nation_id: u8,
    veh_names_to_ids: &HashMap<String, u16>,
    tables: &ComponentTables,
    out: &mut Vec<UnlockEntry>,
) {
    for child in &unlocks_node.children {
        let item_type = match child.name.as_str() {
            "vehicle" => 1u8,
            "chassis" => 2u8,
            "turret" => 3u8,
            "gun" => 4u8,
            "engine" => 5u8,
            "fuelTank" => 6u8,
            "radio" => 7u8,
            _ => continue,
        };
        let target_name = child.value.trim();
        let target_id = match item_type {
            1 => veh_names_to_ids.get(target_name).copied(),
            2 => tables.chassis.get(target_name).copied(),
            3 => tables.turrets.get(target_name).copied(),
            4 => tables.guns.get(target_name).copied(),
            5 => tables.engines.get(target_name).copied(),
            6 => tables.fuel_tanks.get(target_name).copied(),
            7 => tables.radios.get(target_name).copied(),
            _ => None,
        };
        if let Some(id) = target_id {
            let target_cd = make_int_comp_descr(item_type, nation_id, id);
            let xp_cost = child.child("cost").and_then(XmlNode::parse_i64).unwrap_or(0);
            out.push(UnlockEntry {
                xp_cost,
                target_cd,
                prereq_cds: prereq_cds.to_vec(),
            });
        }
    }
}

fn pick_child<'a>(local: &'a XmlNode, shared: Option<&'a XmlNode>, tag: &str) -> Option<&'a XmlNode> {
    local.child(tag).or_else(|| shared.and_then(|s| s.child(tag)))
}

fn parse_vehicle(
    nation_dir: &Path,
    nation: &str,
    nation_id: u8,
    name: &str,
    list_entry: &XmlNode,
    tables: &ComponentTables,
    veh_names_to_ids: &HashMap<String, u16>,
    player_emblem_ids: &BTreeMap<String, u8>,
    extras: &mut ItemCatalogExtras,
    components_out: &mut BTreeMap<i32, ComponentDef>,
) -> Result<(VehicleType, VehicleDef), String> {
    let type_id = list_entry
        .child("id")
        .and_then(XmlNode::parse_i64)
        .ok_or_else(|| "list.xml entry has no <id>".to_string())?;
    if !(0..=u16::MAX as i64).contains(&type_id) {
        return Err(format!("type id {type_id} out of range"));
    }
    let type_id = type_id as u16;
    if type_id > u8::MAX as u16 {
        // The binary vehCompDescr stores vehicleTypeID in one byte (struct '<2B...').
        return Err(format!(
            "type id {type_id} does not fit the one-byte field of vehCompDescr"
        ));
    }
    let level = list_entry
        .child("level")
        .and_then(XmlNode::parse_i64)
        .ok_or_else(|| "list.xml entry has no <level>".to_string())?;
    let price = list_entry
        .child("price")
        .and_then(XmlNode::parse_i64)
        .unwrap_or(0);
    let tags: Vec<String> = list_entry
        .child("tags")
        .map(|n| n.parse_list().into_iter().map(str::to_string).collect())
        .unwrap_or_default();

    let root = read_stem(nation_dir, name).map_err(|e| format!("{e}"))?;

    let stock = |section: &str| -> Result<String, String> {
        let node = root
            .child(section)
            .ok_or_else(|| format!("vehicle xml has no <{section}> section"))?;
        node.first_child()
            .map(|c| c.name.clone())
            .ok_or_else(|| format!("<{section}> section has no components"))
    };
    let chassis_name = stock("chassis")?;
    let engine_name = stock("engines")?;
    let fuel_tank_name = stock("fuelTanks")?;
    let radio_name = stock("radios")?;

    let (turret_name, gun_name, gun_element) = stock_turret_and_gun(&root, tables)?;

    let lookup = |map: &HashMap<String, u16>, section: &str, name: &str| -> Result<u16, String> {
        map.get(name)
            .copied()
            .ok_or_else(|| format!("component {section}/{name} has no id in components/{section}.xml"))
    };
    let chassis_id = lookup(&tables.chassis, "chassis", &chassis_name)?;
    let engine_id = lookup(&tables.engines, "engines", &engine_name)?;
    let fuel_tank_id = lookup(&tables.fuel_tanks, "fuelTanks", &fuel_tank_name)?;
    let radio_id = lookup(&tables.radios, "radios", &radio_name)?;
    let turret_id = lookup(&tables.turrets, "turrets", &turret_name)?;
    let gun_id = lookup(&tables.guns, "guns", &gun_name)?;

    let crew_roles = item_defs::parse_crew_roles(&root)?;
    let default_shells = item_defs::parse_default_shells_for_gun(
        gun_element,
        &tables.shared_gun_nodes,
        &tables.shells,
        nation_id,
    )?;

    // --- vehCompDescr (15 bytes, see module docs) ---
    let comp_descr = make_comp_descr(nation_id, type_id, &ComponentIds {
        chassis: chassis_id,
        engine: engine_id,
        fuel_tank: fuel_tank_id,
        radio: radio_id,
        turret: turret_id,
        gun: gun_id,
    })?;
    let type_comp_descr = make_type_comp_descr(nation_id, type_id);

    extras.shop_items.insert(
        (nation_id, ITEM_TYPE_VEHICLE as i64, type_id as i64),
        item_defs::read_price(list_entry),
    );
    extras.crew_roles.insert((nation_id, type_id), crew_roles);
    extras
        .default_shells
        .insert((nation_id, type_id), default_shells);
    item_defs::record_vehicle_component_prices(
        &root,
        nation_id,
        &[
            ("chassis", 2, &tables.chassis),
            ("engines", 5, &tables.engines),
            ("fuelTanks", 6, &tables.fuel_tanks),
            ("radios", 7, &tables.radios),
        ],
        &tables.turrets,
        &tables.guns,
        extras,
    );

    // --- Build full VehicleDef ---
    let mut parse_component_list = |sec_name: &str,
                                    item_type_id: u8,
                                    id_map: &HashMap<String, u16>,
                                    shared_map: &HashMap<String, ComponentDef>|
     -> Result<Vec<ComponentDef>, String> {
        let mut out = Vec::new();
        let sec = root
            .child(sec_name)
            .ok_or_else(|| format!("vehicle xml has no <{sec_name}> section"))?;
        for child in &sec.children {
            let id = id_map
                .get(&child.name)
                .copied()
                .ok_or_else(|| format!("unknown {sec_name} {}", child.name))?;
            let comp = if child.text().trim() == "shared" {
                shared_map
                    .get(&child.name)
                    .cloned()
                    .ok_or_else(|| format!("missing shared {sec_name} {}", child.name))?
            } else if let Some(shared) = shared_map.get(&child.name) {
                shared.clone()
            } else {
                let def = item_defs::parse_component_def_full(
                    child,
                    id,
                    item_type_id,
                    nation_id,
                    Some(&tables.shells),
                    Some(&tables.shell_defs),
                    tables.projectile_speed_factor,
                );
                components_out
                    .entry(def.comp_descr)
                    .or_insert_with(|| def.clone());
                def
            };
            out.push(comp);
        }
        if out.is_empty() {
            return Err(format!("empty section <{sec_name}>"));
        }
        Ok(out)
    };

    let chassis = parse_component_list("chassis", 2, &tables.chassis, &tables.shared_chassis)?;
    let engines = parse_component_list("engines", 5, &tables.engines, &tables.shared_engines)?;
    let fuel_tanks =
        parse_component_list("fuelTanks", 6, &tables.fuel_tanks, &tables.shared_fuel_tanks)?;
    let radios = parse_component_list("radios", 7, &tables.radios, &tables.shared_radios)?;

    let mut turrets = Vec::new();
    for child in &root.children {
        if child.name.starts_with("turrets") {
            for turret_node in &child.children {
                let tid = tables
                    .turrets
                    .get(&turret_node.name)
                    .copied()
                    .ok_or_else(|| format!("unknown turret {}", turret_node.name))?;
                let t_def = if turret_node.text().trim() == "shared" {
                    tables
                        .shared_turrets
                        .get(&turret_node.name)
                        .cloned()
                        .ok_or_else(|| format!("missing shared turret {}", turret_node.name))?
                } else if let Some(shared) = tables.shared_turrets.get(&turret_node.name) {
                    shared.clone()
                } else {
                    let def = item_defs::parse_component_def_full(
                        turret_node,
                        tid,
                        3,
                        nation_id,
                        None,
                        None,
                        tables.projectile_speed_factor,
                    );
                    components_out
                        .entry(def.comp_descr)
                        .or_insert_with(|| def.clone());
                    def
                };
                let shared_t_node = tables.shared_turret_nodes.get(&turret_node.name);
                let yaw_limits = item_defs::parse_vector2(pick_child(turret_node, shared_t_node, "yawLimits"));

                let guns_node = pick_child(turret_node, shared_t_node, "guns");
                let mut guns = Vec::new();
                if let Some(gsec) = guns_node {
                    for gun_node in &gsec.children {
                        let gid = tables
                            .guns
                            .get(&gun_node.name)
                            .copied()
                            .ok_or_else(|| format!("unknown gun {}", gun_node.name))?;
                        let (is_shared_gun, base_gun) = if gun_node.text().trim() == "shared" {
                            let shared = tables
                                .shared_guns
                                .get(&gun_node.name)
                                .cloned()
                                .ok_or_else(|| format!("missing shared gun {}", gun_node.name))?;
                            (true, shared)
                        } else if let Some(shared) = tables.shared_guns.get(&gun_node.name) {
                            (true, shared.clone())
                        } else {
                            let def = item_defs::parse_component_def_full(
                                gun_node,
                                gid,
                                4,
                                nation_id,
                                Some(&tables.shells),
                                Some(&tables.shell_defs),
                                tables.projectile_speed_factor,
                            );
                            (false, def)
                        };
                        let shared_g_node = tables.shared_gun_nodes.get(&gun_node.name);
                        let max_ammo = pick_child(gun_node, shared_g_node, "maxAmmo")
                            .and_then(XmlNode::parse_i64)
                            .map(|x| x as u32)
                            .or(base_gun.max_ammo)
                            .unwrap_or(0);
                        let shots = if let Some(sn) = gun_node.child("shots") {
                            let mut sh = Vec::new();
                            for s in &sn.children {
                                if let Some(&sid) = tables.shells.get(&s.name) {
                                    sh.push(item_defs::parse_shot_def_with_factor(
                                        s,
                                        sid,
                                        nation_id,
                                        tables.projectile_speed_factor,
                                        Some(&tables.shell_defs),
                                    ));
                                }
                            }
                            sh
                        } else {
                            base_gun.shots.clone()
                        };
                        let pitch_limits =
                            item_defs::parse_vector2(pick_child(gun_node, shared_g_node, "pitchLimits"));
                        let rotation_speed = pick_child(gun_node, shared_g_node, "rotationSpeed")
                            .and_then(XmlNode::parse_f64);
                        let reload_time = pick_child(gun_node, shared_g_node, "reloadTime")
                            .and_then(XmlNode::parse_f64);
                        let aiming_time = pick_child(gun_node, shared_g_node, "aimingTime")
                            .and_then(XmlNode::parse_f64);
                        let shot_disp_radius =
                            pick_child(gun_node, shared_g_node, "shotDispersionRadius")
                                .and_then(XmlNode::parse_f64);
                        let gun_hit_tester = pick_child(gun_node, shared_g_node, "hitTester")
                            .and_then(item_defs::parse_hit_tester)
                            .or_else(|| base_gun.hit_tester.clone());
                        let gun_armor = pick_child(gun_node, shared_g_node, "armor")
                            .map(item_defs::parse_armor_zones)
                            .or_else(|| base_gun.armor.clone());
                        let gun_shot_disp_factors =
                            pick_child(gun_node, shared_g_node, "shotDispersionFactors")
                                .and_then(item_defs::parse_gun_shot_dispersion_factors)
                                .or(base_gun.gun_shot_dispersion_factors);
                        let gun_burst = pick_child(gun_node, shared_g_node, "burst")
                            .map(|b| item_defs::parse_gun_burst(Some(b)))
                            .or(base_gun.burst)
                            .or(Some((1, 0.0)));
                        let gun_effects = pick_child(gun_node, shared_g_node, "effects")
                            .map(|n| n.text().to_string())
                            .filter(|s| !s.is_empty())
                            .or_else(|| base_gun.effects.clone());

                        // Preserve shared entries in `components_out`; only register genuinely local guns once.
                        if !is_shared_gun {
                            components_out
                                .entry(base_gun.comp_descr)
                                .or_insert_with(|| base_gun.clone());
                        }

                        let mut effective_gun = base_gun;
                        effective_gun.max_ammo = Some(max_ammo);
                        effective_gun.shots = shots.clone();
                        effective_gun.hit_tester = gun_hit_tester.clone();
                        effective_gun.armor = gun_armor.clone();
                        effective_gun.gun_shot_dispersion_factors = gun_shot_disp_factors;
                        effective_gun.burst = gun_burst;
                        effective_gun.effects = gun_effects.clone();

                        guns.push(TurretGunEntry {
                            component: effective_gun,
                            max_ammo,
                            shots,
                            pitch_limits,
                            rotation_speed,
                            reload_time,
                            aiming_time,
                            shot_disp_radius,
                            hit_tester: gun_hit_tester,
                            armor: gun_armor,
                            shot_dispersion_factors: gun_shot_disp_factors,
                            burst: gun_burst,
                            effects: gun_effects,
                        });
                    }
                }
                if guns.is_empty() {
                    return Err(format!("turret {} has no guns", turret_node.name));
                }
                let turret_max_health = pick_child(turret_node, shared_t_node, "maxHealth")
                    .and_then(XmlNode::parse_i64)
                    .map(|x| x as i32)
                    .or(t_def.max_health)
                    .unwrap_or(0);
                let gun_position =
                    item_defs::parse_vec3_arr(pick_child(turret_node, shared_t_node, "gunPosition"));
                let circular_vision_radius =
                    pick_child(turret_node, shared_t_node, "circularVisionRadius")
                        .and_then(XmlNode::parse_f64);
                let turret_rotation_speed =
                    pick_child(turret_node, shared_t_node, "rotationSpeed")
                        .and_then(XmlNode::parse_f64);
                let turret_armor =
                    pick_child(turret_node, shared_t_node, "armor").map(item_defs::parse_armor_zones);
                let turret_primary_armor =
                    item_defs::parse_primary_armor(pick_child(turret_node, shared_t_node, "primaryArmor"));
                let turret_rotator_health =
                    pick_child(turret_node, shared_t_node, "turretRotatorHealth")
                        .and_then(item_defs::parse_device_health);
                let surveying_device_health =
                    pick_child(turret_node, shared_t_node, "surveyingDeviceHealth")
                        .and_then(item_defs::parse_device_health);
                let turret_hit_tester = pick_child(turret_node, shared_t_node, "hitTester")
                    .and_then(item_defs::parse_hit_tester);

                turrets.push(TurretDef {
                    component: t_def,
                    guns,
                    yaw_limits,
                    max_health: turret_max_health,
                    gun_position,
                    circular_vision_radius,
                    rotation_speed: turret_rotation_speed,
                    armor: turret_armor,
                    primary_armor: turret_primary_armor,
                    turret_rotator_health,
                    surveying_device_health,
                    hit_tester: turret_hit_tester,
                });
            }
        }
    }
    if turrets.is_empty() {
        return Err("vehicle xml has no turrets".to_string());
    }

    // --- Traversal Order for unlocks_descrs (VehicleType.__init__) ---
    let mut unlocks_descrs = Vec::new();

    // 1. Vehicle root <unlocks>
    if let Some(u) = root.child("unlocks") {
        parse_unlocks_into(u, &[], nation_id, veh_names_to_ids, tables, &mut unlocks_descrs);
    }

    // 2. <chassis>
    if let Some(chassis_sec) = root.child("chassis") {
        for child in &chassis_sec.children {
            if let Some(&cid) = tables.chassis.get(&child.name) {
                let cd = make_int_comp_descr(2, nation_id, cid);
                if let Some(u) = child.child("unlocks") {
                    parse_unlocks_into(u, &[cd], nation_id, veh_names_to_ids, tables, &mut unlocks_descrs);
                }
            }
        }
    }

    // 3. <engines>
    if let Some(engines_sec) = root.child("engines") {
        for child in &engines_sec.children {
            if let Some(&eid) = tables.engines.get(&child.name) {
                let cd = make_int_comp_descr(5, nation_id, eid);
                if let Some(u) = child.child("unlocks") {
                    parse_unlocks_into(u, &[cd], nation_id, veh_names_to_ids, tables, &mut unlocks_descrs);
                }
            }
        }
    }

    // 4. <fuelTanks>
    if let Some(ft_sec) = root.child("fuelTanks") {
        for child in &ft_sec.children {
            if let Some(&fid) = tables.fuel_tanks.get(&child.name) {
                let cd = make_int_comp_descr(6, nation_id, fid);
                if let Some(u) = child.child("unlocks") {
                    parse_unlocks_into(u, &[cd], nation_id, veh_names_to_ids, tables, &mut unlocks_descrs);
                }
            }
        }
    }

    // 5. <radios>
    if let Some(radios_sec) = root.child("radios") {
        for child in &radios_sec.children {
            if let Some(&rid) = tables.radios.get(&child.name) {
                let cd = make_int_comp_descr(7, nation_id, rid);
                if let Some(u) = child.child("unlocks") {
                    parse_unlocks_into(u, &[cd], nation_id, veh_names_to_ids, tables, &mut unlocks_descrs);
                }
            }
        }
    }

    // 6. <turrets0>
    for child in &root.children {
        if child.name.starts_with("turrets") {
            for turret_node in &child.children {
                if let Some(&tid) = tables.turrets.get(&turret_node.name) {
                    let turret_cd = make_int_comp_descr(3, nation_id, tid);
                    let guns_sec = turret_node.child("guns").or_else(|| {
                        tables.shared_turret_nodes.get(&turret_node.name).and_then(|st| st.child("guns"))
                    });
                    if let Some(gsec) = guns_sec {
                        for gun_node in &gsec.children {
                            if let Some(&gid) = tables.guns.get(&gun_node.name) {
                                let gun_cd = make_int_comp_descr(4, nation_id, gid);
                                if let Some(u) = gun_node.child("unlocks") {
                                    parse_unlocks_into(u, &[gun_cd, turret_cd], nation_id, veh_names_to_ids, tables, &mut unlocks_descrs);
                                }
                            }
                        }
                    }
                    if let Some(u) = turret_node.child("unlocks") {
                        parse_unlocks_into(u, &[turret_cd], nation_id, veh_names_to_ids, tables, &mut unlocks_descrs);
                    }
                }
            }
        }
    }

    let autounlocked_items = vec![
        chassis[0].comp_descr,
        engines[0].comp_descr,
        fuel_tanks[0].comp_descr,
        radios[0].comp_descr,
        turrets[0].component.comp_descr,
        turrets[0].guns[0].component.comp_descr,
    ];

    let mut installable_components = BTreeSet::new();
    for c in &chassis {
        installable_components.insert(c.comp_descr);
    }
    for e in &engines {
        installable_components.insert(e.comp_descr);
    }
    for f in &fuel_tanks {
        installable_components.insert(f.comp_descr);
    }
    for r in &radios {
        installable_components.insert(r.comp_descr);
    }
    for t in &turrets {
        installable_components.insert(t.component.comp_descr);
        for g in &t.guns {
            installable_components.insert(g.component.comp_descr);
        }
    }

    let is_default_elite = unlocks_descrs.is_empty();

    let hull = root.child("hull");
    let hull_weight = hull
        .and_then(|h| h.child("weight"))
        .and_then(XmlNode::parse_f64)
        .unwrap_or(0.0);
    let hull_max_health = hull
        .and_then(|h| h.child("maxHealth"))
        .and_then(XmlNode::parse_i64)
        .unwrap_or(1) as i32;
    let repair_cost = root
        .child("repairCost")
        .and_then(XmlNode::parse_f64)
        .unwrap_or(0.0);

    let default_player_emblem_id = root
        .child("emblems")
        .and_then(|e| e.child("default"))
        .and_then(|d| player_emblem_ids.get(d.text()).copied())
        .unwrap_or(1);

    let mut crew = Vec::new();
    if let Some(crew_node) = root.child("crew") {
        for child in &crew_node.children {
            crew.push(CrewSlotSpec {
                primary_role: child.name.clone(),
                secondary_roles: child.parse_list().into_iter().map(str::to_string).collect(),
            });
        }
    }

    let price_info = item_defs::read_price(list_entry);
    let veh_price = (price_info.credits, price_info.gold);
    let show_in_shop = !price_info.hidden;

    let speed_limits = item_defs::parse_speed_limits(root.child("speedLimits"));
    let invisibility = item_defs::parse_invisibility(root.child("invisibility"));
    let hull_armor = hull.and_then(|h| h.child("armor")).map(item_defs::parse_armor_zones);
    let hull_primary_armor =
        item_defs::parse_primary_armor(hull.and_then(|h| h.child("primaryArmor")));
    let hull_hit_tester = hull
        .and_then(|h| h.child("hitTester"))
        .and_then(item_defs::parse_hit_tester);
    let hull_position = chassis.first().and_then(|c| c.hull_position);
    let turret_positions: Vec<[f64; 3]> = hull
        .and_then(|h| h.child("turretPositions"))
        .map(|tp| {
            tp.children
                .iter()
                .filter_map(|c| item_defs::parse_vec3_arr(Some(c)))
                .collect()
        })
        .unwrap_or_default();
    let ammo_bay_health = hull
        .and_then(|h| h.child("ammoBayHealth"))
        .and_then(item_defs::parse_device_health);
    let battle_def = Some(VehicleBattleDef {
        speed_limits: speed_limits.unwrap_or((0.0, 0.0)),
        invisibility: invisibility.unwrap_or((0.0, 0.0)),
        hull_armor: hull_armor.clone().unwrap_or_default(),
        primary_armor: hull_primary_armor.clone(),
        hull_position: hull_position.unwrap_or([0.0; 3]),
        turret_positions: turret_positions.clone(),
        hit_tester: hull_hit_tester.clone(),
        ammo_bay_health,
    });

    let veh_def = VehicleDef {
        nation: nation.to_string(),
        nation_id,
        type_id,
        name: name.to_string(),
        veh_type_comp_descr: type_comp_descr,
        hull_weight,
        hull_max_health,
        repair_cost,
        tags: tags.iter().cloned().collect(),
        level: level as u8,
        default_player_emblem_id,
        crew,
        chassis,
        engines,
        fuel_tanks,
        radios,
        turrets,
        unlocks_descrs,
        autounlocked_items,
        installable_components,
        is_default_elite,
        price: veh_price,
        show_in_shop,
        speed_limits,
        invisibility,
        hull_armor,
        hull_primary_armor,
        hull_hit_tester,
        hull_position,
        turret_positions,
        ammo_bay_health,
        battle_def,
    };

    let veh_type = VehicleType {
        nation: nation.to_string(),
        nation_id,
        type_id,
        name: name.to_string(),
        level: level as u8,
        price,
        tags,
        veh_type_comp_descr: type_comp_descr,
        veh_comp_descr_hex: hex(&comp_descr),
        veh_comp_descr_b64: base64::engine::general_purpose::STANDARD.encode(&comp_descr),
        veh_comp_descr_len: comp_descr.len(),
        components: Components {
            chassis: chassis_name,
            engines: engine_name,
            fuel_tanks: fuel_tank_name,
            radios: radio_name,
            turrets: turret_name,
            guns: gun_name,
        },
        component_ids: ComponentIds {
            chassis: chassis_id,
            engine: engine_id,
            fuel_tank: fuel_tank_id,
            radio: radio_id,
            turret: turret_id,
            gun: gun_id,
        },
        comp_descr,
    };

    Ok((veh_type, veh_def))
}

/// Find the (single) turret position section (`turrets0`), its stock turret, and the stock
/// turret's first gun element.
fn stock_turret_and_gun<'a>(
    root: &'a XmlNode,
    tables: &'a ComponentTables,
) -> Result<(String, String, &'a XmlNode), String> {
    let mut positions: Vec<(u32, &XmlNode)> = root
        .children
        .iter()
        .filter_map(|child| {
            let rest = child.name.strip_prefix("turrets")?;
            let index = if rest.is_empty() {
                0
            } else {
                rest.parse::<u32>().ok()?
            };
            Some((index, child))
        })
        .collect();
    positions.sort_by_key(|(index, _)| *index);
    let Some((_, position)) = positions.first() else {
        return Err("vehicle xml has no <turretsN> section".to_string());
    };
    if positions.len() != 1 {
        return Err(format!(
            "expected exactly one turret position, found {} (0.6.5 always has one)",
            positions.len()
        ));
    }
    let turret_element = position
        .first_child()
        .ok_or_else(|| "turret position has no turrets".to_string())?;
    let guns = if let Some(local_guns) = turret_element.child("guns") {
        local_guns
    } else if turret_element.text().trim() == "shared" {
        let shared = tables
            .shared_turret_nodes
            .get(&turret_element.name)
            .ok_or_else(|| {
                format!(
                    "turret {} is shared but not in the shared table",
                    turret_element.name
                )
            })?;
        shared
            .child("guns")
            .ok_or_else(|| format!("turret {} has no <guns> section", shared.name))?
    } else {
        return Err(format!("turret {} has no <guns> section", turret_element.name));
    };
    let gun_element = guns
        .first_child()
        .ok_or_else(|| format!("turret {} has no guns", turret_element.name))?;
    Ok((turret_element.name.clone(), gun_element.name.clone(), gun_element))
}

/// The int `vehTypeCompDescr`: `(typeID << 8) | (nationID << 4) | 1`
/// (`vehicles.makeIntCompactDescrByID`, decompiled `common/items/vehicles.py:793-795`).
pub fn make_type_comp_descr(nation_id: u8, type_id: u16) -> i32 {
    ((type_id as i32) << 8) | ((nation_id as i32) << 4) | ITEM_TYPE_VEHICLE
}

/// Decode a `vehTypeCompDescr` into `(itemTypeID, nationID, typeID)`; mirrors
/// `vehicles.parseIntCompactDescr` (`common/items/vehicles.py:747-751`).
pub fn parse_type_comp_descr(compact_descr: i32) -> (i32, u8, u16) {
    (
        compact_descr & 15,
        ((compact_descr >> 4) & 15) as u8,
        ((compact_descr >> 8) & 0xffff) as u16,
    )
}

/// The binary `vehCompDescr` stored in `inventory['compDescr']` (15 bytes for every 0.6.5
/// vehicle: one turret position, no optional devices/emblem/horn/camo). Mirrors
/// `VehicleDescr.makeCompactDescr` (`common/items/vehicles.py:345-385`).
pub fn make_comp_descr(
    nation_id: u8,
    type_id: u16,
    components: &ComponentIds,
) -> Result<Vec<u8>, String> {
    if nation_id > 15 {
        return Err(format!("nation id {nation_id} does not fit 4 bits"));
    }
    if type_id > u8::MAX as u16 {
        return Err(format!(
            "type id {type_id} does not fit the one-byte field of vehCompDescr"
        ));
    }
    let descr = crate::descr::VehicleDescr {
        nation_id,
        type_id: type_id as u8,
        chassis_id: components.chassis,
        engine_id: components.engine,
        fuel_tank_id: components.fuel_tank,
        radio_id: components.radio,
        turret_id: components.turret,
        gun_id: components.gun,
        optional_devices: [None; crate::descr::NUM_OPTIONAL_DEVICE_SLOTS],
        custom_emblem_id: None,
        horn_id: None,
        camouflage_id: None,
    };
    Ok(descr.to_bytes())
}

/// A decoded 15-byte vehicle `vehCompDescr` (single turret position).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VehicleCompDescr {
    pub nation_id: u8,
    pub type_id: u8,
    pub chassis: u16,
    pub engine: u16,
    pub fuel_tank: u16,
    pub radio: u16,
    pub turret: u16,
    pub gun: u16,
    pub flags: u8,
}

/// Decode a vehicle `vehCompDescr`. Delegates to [`crate::descr::VehicleDescr::from_bytes`],
/// verifying the 15-byte stock format expected by legacy 0.6.5 callers.
pub fn parse_comp_descr(bytes: &[u8]) -> Result<VehicleCompDescr, String> {
    if bytes.len() != 15 {
        return Err(format!(
            "expected a 15-byte vehCompDescr (0.6.5 single-turret layout), got {} bytes",
            bytes.len()
        ));
    }
    let descr = crate::descr::VehicleDescr::from_bytes(bytes).map_err(|e| e.to_string())?;
    Ok(VehicleCompDescr {
        nation_id: descr.nation_id,
        type_id: descr.type_id,
        chassis: descr.chassis_id,
        engine: descr.engine_id,
        fuel_tank: descr.fuel_tank_id,
        radio: descr.radio_id,
        turret: descr.turret_id,
        gun: descr.gun_id,
        flags: bytes[14],
    })
}

fn hex(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        s.push_str(&format!("{b:02x}"));
    }
    s
}

/// Serialize the catalog as JSON with the same schema as the reference
/// `vehicles_catalog.json` (useful for golden comparisons).
pub fn to_json(catalog: &Catalog) -> String {
    serde_json::to_string_pretty(catalog).expect("catalog serialization cannot fail")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ids(chassis: u16, engine: u16, fuel_tank: u16, radio: u16, turret: u16, gun: u16) -> ComponentIds {
        ComponentIds {
            chassis,
            engine,
            fuel_tank,
            radio,
            turret,
            gun,
        }
    }

    /// Golden `vehCompDescr` values, hand-checked against the retail item defs and the
    /// reference catalog (`.scratch/research/decompiled/_generated/vehicles_catalog.json`).
    #[test]
    fn golden_vehicle_compact_descrs() {
        // ussr T-34, tier 5 (typeID 0)
        let t34 = make_comp_descr(0, 0, &ids(0, 0, 203, 3, 0, 0)).unwrap();
        assert_eq!(hex(&t34), "010000000000cb0003000000000000");
        assert_eq!(make_type_comp_descr(0, 0), 1);

        // germany PzIV, tier 5 (typeID 0)
        let pz4 = make_comp_descr(1, 0, &ids(0, 0, 203, 2, 0, 0)).unwrap();
        assert_eq!(hex(&pz4), "110000000000cb0002000000000000");
        assert_eq!(make_type_comp_descr(1, 0), 17);

        // usa T1_Cunningham, tier 1 (typeID 2)
        let t1 = make_comp_descr(2, 2, &ids(3, 4, 201, 3, 3, 4)).unwrap();
        assert_eq!(hex(&t1), "210203000400c90003000300040000");
        assert_eq!(make_type_comp_descr(2, 2), 545);

        // ussr MS-1, tier 1 (typeID 13)
        let ms1 = make_comp_descr(0, 13, &ids(26, 14, 201, 0, 23, 23)).unwrap();
        assert_eq!(hex(&ms1), "010d1a000e00c90000001700170000");
        assert_eq!(make_type_comp_descr(0, 13), 3329);

        // germany Löwe, tier 8 - type ids are sparse, 212 fits the one-byte field.
        let lowe = make_comp_descr(1, 212, &ids(85, 40, 205, 16, 67, 51)).unwrap();
        assert_eq!(hex(&lowe), "11d455002800cd0010004300330000");
        assert_eq!(make_type_comp_descr(1, 212), 54289);
    }

    #[test]
    fn compact_descr_roundtrip() {
        let original = make_comp_descr(1, 212, &ids(85, 40, 205, 16, 67, 51)).unwrap();
        let parsed = parse_comp_descr(&original).unwrap();
        assert_eq!(
            parsed,
            VehicleCompDescr {
                nation_id: 1,
                type_id: 212,
                chassis: 85,
                engine: 40,
                fuel_tank: 205,
                radio: 16,
                turret: 67,
                gun: 51,
                flags: 0,
            }
        );
        // and the int form round-trips through the client's rules
        let (item_type, nation, type_id) = parse_type_comp_descr(make_type_comp_descr(2, 2));
        assert_eq!((item_type, nation, type_id), (ITEM_TYPE_VEHICLE, 2, 2));
    }

    #[test]
    fn rejects_malformed_descrs() {
        assert!(parse_comp_descr(&[0u8; 14]).is_err(), "wrong length");
        let mut wrong_type = make_comp_descr(0, 0, &ids(0, 0, 0, 0, 0, 0)).unwrap();
        wrong_type[0] = 0x02; // chassis item type, not vehicle
        assert!(parse_comp_descr(&wrong_type).is_err(), "wrong item type");
        assert!(make_comp_descr(0, 300, &ids(0, 0, 0, 0, 0, 0)).is_err(), "type id must fit one byte");
    }

    #[test]
    fn test_full_catalog_retail_parsing() {
        let Some(dir) = crate::testutil::game_dir() else {
            eprintln!("skip: no game install");
            return;
        };
        let catalog = load(&dir).expect("load catalog from retail install");

        // 1. T-34 (nation 0, type_id 0)
        let t34 = catalog.vehicle_def_by_id(0, 0).expect("T-34 def");
        assert!(t34.turrets.len() > 1, "T-34 must have >1 turrets");
        let total_guns: usize = t34.turrets.iter().map(|t| t.guns.len()).sum();
        assert!(total_guns > 1, "T-34 must have >1 guns across turrets");
        assert!(t34.repair_cost > 0.0, "T-34 repairCost > 0");
        assert!(t34.hull_weight > 0.0, "T-34 hull weight > 0");
        assert!(t34.hull_max_health > 0, "T-34 hull maxHealth > 0");
        assert!(!t34.unlocks_descrs.is_empty(), "T-34 has unlocks");
        assert!(!t34.is_default_elite, "T-34 is not default elite");
        assert_eq!(t34.unlocks_descrs[0].xp_cost, 1940);
        assert_eq!(t34.unlocks_descrs[0].target_cd, item_defs::make_int_comp_descr(2, 0, 1));
        assert_eq!(t34.unlocks_descrs[0].prereq_cds, vec![t34.chassis[0].comp_descr]);
        assert_eq!(
            t34.autounlocked_items,
            vec![
                t34.chassis[0].comp_descr,
                t34.engines[0].comp_descr,
                t34.fuel_tanks[0].comp_descr,
                t34.radios[0].comp_descr,
                t34.turrets[0].component.comp_descr,
                t34.turrets[0].guns[0].component.comp_descr,
            ]
        );

        // 2. MS-1 (nation 0, type_id 13)
        let ms1 = catalog.vehicle_def_by_id(0, 13).expect("MS-1 def");
        assert_eq!(ms1.price, (0, 0));
        assert_eq!(ms1.level, 1);
        assert!(!ms1.is_default_elite);
        assert_eq!(ms1.repair_cost, 0.0);
        assert_eq!(ms1.crew.len(), 2);
        assert_eq!(ms1.crew[0].primary_role, "commander");
        assert_eq!(ms1.crew[1].primary_role, "driver");

        // 3. OptionalDeviceDef
        let toolbox = catalog.optional_devices.values().find(|d| d.name == "toolbox").expect("toolbox");
        assert!(toolbox.removable);
        assert_eq!(toolbox.weight, 100.0);

        let christie = catalog.optional_devices.values().find(|d| d.name == "enhancedSuspension_Christie").expect("christie");
        assert!(christie.chassis_max_load_factor > 1.0);

        let lining = catalog.optional_devices.values().find(|d| d.name == "antifragmentationLining_light").expect("lining");
        let filter_tags: std::collections::BTreeSet<_> = lining
            .vehicle_filter
            .include
            .iter()
            .filter_map(|p| p.vehicle.as_ref())
            .filter_map(|v| v.tags.as_ref())
            .flatten()
            .map(|s| s.as_str())
            .collect();
        assert!(filter_tags.contains("lightTank"));
        assert!(filter_tags.contains("lightSPG"));
        assert!(filter_tags.contains("lightAT-SPG"));

        // 4. EquipmentDef
        let oil = catalog.equipments.values().find(|e| e.name == "lendLeaseOil").expect("lendLeaseOil");
        assert!(oil.tags.contains("fuel"));
        assert!(oil.incompatible_installed.contains("fuel"));
        assert_eq!(oil.vehicle_filter.include[0].nations.as_ref().unwrap(), &vec![0]);

        // 5. CamouflageDef
        let ussr_camos = catalog.camouflages.get(&0).expect("ussr camos");
        assert!(!ussr_camos.is_empty());
        for (id, camo) in ussr_camos {
            assert_eq!(*id, camo.id);
            assert!(camo.gold >= 0);
        }

        // 6. TankmenConfig
        let ussr_tman = catalog.tankmen_configs.get(&0).expect("ussr tankmen");
        let ussr_cmd_ranks = ussr_tman.role_ranks.get("commander").unwrap();
        assert_eq!(ussr_cmd_ranks.len() - 1, 8, "USSR commander max rank index = 8");
        let ger_tman = catalog.tankmen_configs.get(&1).expect("germany tankmen");
        let ger_cmd_ranks = ger_tman.role_ranks.get("commander").unwrap();
        assert_eq!(ger_cmd_ranks.len() - 1, 10, "Germany commander max rank index = 10");
        let women1 = ussr_tman.premium_groups.iter().find(|g| g.name == "women1").expect("women1");
        assert!(women1.is_female);
        assert!(!women1.first_names.is_empty());
        assert!(!women1.last_names.is_empty());
        assert!(!women1.icons.is_empty());

        // 7. PlayerEmblems
        assert_eq!(catalog.player_emblem_ids.get("ussr_star"), Some(&1));
        assert_eq!(catalog.player_emblem_ids.get("germany_cross"), Some(&2));
        assert_eq!(catalog.player_emblem_ids.get("usa_star"), Some(&3));
        assert_eq!(t34.default_player_emblem_id, 1);

        // 8. Horns
        assert!(!catalog.horns.is_empty());
    }

    #[test]
    fn test_shot_def_speed_gravity_scaling_and_shell_xml_def() {
        let Some(dir) = crate::testutil::game_dir() else {
            eprintln!("skip: no game install");
            return;
        };
        let catalog = load(&dir).expect("load catalog from retail install");

        // MS-1 stock gun `_37mm_Gochkins` -> `_37mm_UBRT1`: XML speed=442, gravity=9.81
        let ms1 = catalog.vehicle_def_by_id(0, 13).expect("MS-1 def");
        let stock_gun = &ms1.turrets[0].guns[0];
        let ubrt1 = stock_gun
            .shots
            .iter()
            .find(|s| s.shell_name == "_37mm_UBRT1")
            .expect("_37mm_UBRT1 shot");
        assert!(
            (ubrt1.speed - 442.0 * 0.8).abs() < 1e-6,
            "expected scaled speed {}, got {}",
            442.0 * 0.8,
            ubrt1.speed
        );
        assert!(
            (ubrt1.gravity - 9.81 * 0.64).abs() < 1e-6,
            "expected scaled gravity {}, got {}",
            9.81 * 0.64,
            ubrt1.gravity
        );
        let ubrt1_shell = ubrt1.shell.as_ref().expect("shot.shell must be populated");
        assert_eq!(ubrt1_shell.kind, ShellKind::ArmorPiercing);
        assert_eq!(ubrt1_shell.caliber, 37.0);
        assert!(ubrt1_shell.is_tracer);
        assert_eq!(ubrt1_shell.armor_damage, 30.0);
        assert_eq!(ubrt1_shell.devices_damage, 50.0);
        assert_eq!(ubrt1_shell.explosion_radius, 0.0);
        assert!(!ubrt1_shell.effects.is_empty());
        assert_eq!(
            catalog.resolve_shell(ubrt1.shell_cd),
            Some(ubrt1_shell),
            "Catalog::resolve_shell must resolve shot shell_cd"
        );

        let uot1 = stock_gun
            .shots
            .iter()
            .find(|s| s.shell_name == "_37mm_UOT1")
            .expect("_37mm_UOT1 HE shot");
        let uot1_shell = uot1.shell.as_ref().expect("HE shot.shell");
        assert_eq!(uot1_shell.kind, ShellKind::HighExplosive);
        assert_eq!(uot1_shell.explosion_radius, 0.25);

        // T-34 stock gun `_76mm_L-11` scaled speed & gravity + shell lookup
        let t34 = catalog.vehicle_def_by_id(0, 0).expect("T-34 def");
        let t34_shot = &t34.turrets[0].guns[0].shots[0];
        assert!(t34_shot.gravity > 0.0, "T-34 shot gravity must be populated");
        let t34_shell = catalog
            .resolve_shell(t34_shot.shell_cd)
            .expect("T-34 shell in catalog.shells");
        assert_eq!(t34_shell.caliber, 76.0);
        assert_eq!(t34_shot.shell.as_ref(), Some(t34_shell));

        // Synthetic shells.xml fallback test: HE shell missing <explosionRadius> falls back to caliber^2 / 5555.0
        let tmp_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("..")
            .join("target")
            .join("test-tmp")
            .join("shell-fallback-test");
        let _ = std::fs::remove_dir_all(&tmp_dir);
        std::fs::create_dir_all(&tmp_dir).expect("create test-tmp");
        std::fs::write(
            tmp_dir.join("shells.xml"),
            r#"<root>
  <_152mm_HE>
    <id>42</id>
    <price>100</price>
    <kind>HIGH_EXPLOSIVE</kind>
    <caliber>152</caliber>
    <isTracer>false</isTracer>
    <effects>largeHighExplosive</effects>
    <damage><armor>910</armor><devices>200</devices></damage>
  </_152mm_HE>
</root>"#,
        )
        .expect("write synthetic shells.xml");
        let mut extras = ItemCatalogExtras::default();
        let (_by_name, defs) =
            item_defs::load_nation_shells_with_defs(&tmp_dir, 0, &mut extras)
                .expect("load synthetic shells");
        let expected_radius = 152.0 * 152.0 / 5555.0;
        let he_cd = make_int_comp_descr(10, 0, 42);
        let he_def = defs.get(&he_cd).expect("synthetic HE shell");
        assert!((he_def.explosion_radius - expected_radius).abs() < 1e-6);
        assert_eq!(he_def.damage_randomization, 0.25);
        assert_eq!(he_def.piercing_power_randomization, 0.25);
        let _ = std::fs::remove_dir_all(&tmp_dir);
    }

    #[test]
    fn test_vehicle_and_component_battle_attributes_and_scripts() {
        let Some(dir) = crate::testutil::game_dir() else {
            eprintln!("skip: no game install");
            return;
        };
        let catalog = load(&dir).expect("load catalog from retail install");

        // 1. MS-1 battle fields across vehicle, hull, chassis, turret, gun, engine, fuelTank, radio
        let ms1 = catalog.vehicle_def_by_id(0, 13).expect("MS-1 def");
        assert_eq!(ms1.speed_limits, Some((32.0, 7.0)));
        let hull_armor = ms1.hull_armor.as_ref().expect("MS-1 hull_armor");
        assert!(hull_armor.thickness("armor_1").unwrap_or(0.0) > 0.0);
        assert_eq!(
            ms1.hull_primary_armor,
            vec!["armor_1", "armor_3", "armor_4"]
        );
        let hull_ht = ms1.hull_hit_tester.as_ref().expect("MS-1 hull_hit_tester");
        assert!(hull_ht.model.ends_with("Hull.model"));
        assert!(ms1.hull_position.is_some());
        assert_eq!(ms1.turret_positions.len(), 1);
        let ammo_bay = ms1.ammo_bay_health.expect("MS-1 ammo_bay_health");
        assert_eq!(ammo_bay.max_health, 70);
        assert_eq!(ammo_bay.max_regen_health, 60);
        assert_eq!(ammo_bay.repair_cost, 1.0);
        assert!(ms1.battle_def.is_some());

        let chassis = &ms1.chassis[0];
        assert_eq!(chassis.max_climb_angle, Some(25.0));
        assert_eq!(chassis.rotation_speed, Some(34.0));
        assert_eq!(chassis.rotation_is_around_center, Some(false));
        assert!(chassis.top_right_carrying_point.is_some());
        assert!(chassis.hull_position.is_some());
        assert!(chassis.shot_dispersion_factors.is_some());
        assert!(chassis.hit_tester.as_ref().is_some_and(|h| h.model.ends_with("Chassis.model")));
        assert!(chassis.armor.as_ref().is_some_and(|a| !a.is_empty()));
        assert_eq!(chassis.max_health, Some(40));
        assert_eq!(chassis.max_regen_health, Some(30));
        assert_eq!(chassis.repair_cost, Some(1.0));

        let engine = &ms1.engines[0];
        assert!(engine.engine_power.unwrap_or(0.0) > 0.0);
        assert!(engine.fire_starting_chance.is_some());
        assert!(engine.max_health.is_some());
        assert!(engine.max_regen_health.is_some());
        assert!(engine.repair_cost.is_some());

        let fuel = &ms1.fuel_tanks[0];
        assert!(fuel.max_health.is_some());
        assert!(fuel.max_regen_health.is_some());
        assert!(fuel.repair_cost.is_some());

        let radio = &ms1.radios[0];
        assert!(radio.radio_distance.unwrap_or(0.0) > 0.0);
        assert!(radio.max_health.is_some());
        assert!(radio.max_regen_health.is_some());
        assert!(radio.repair_cost.is_some());

        let turret = &ms1.turrets[0];
        assert!(turret.gun_position.is_some());
        assert_eq!(turret.circular_vision_radius, Some(270.0));
        assert_eq!(turret.rotation_speed, Some(50.0));
        assert!(turret.armor.as_ref().is_some_and(|a| !a.is_empty()));
        assert_eq!(turret.primary_armor, vec!["armor_1", "armor_3", "armor_4"]);
        assert!(turret.turret_rotator_health.is_some());
        assert!(turret.surveying_device_health.is_some());
        assert!(turret.hit_tester.as_ref().is_some_and(|h| h.model.ends_with("Turret_01.model")));

        let gun = &turret.guns[0];
        assert!(gun.hit_tester.as_ref().is_some_and(|h| h.model.ends_with(".model")));
        assert!(gun.armor.as_ref().is_some_and(|a| !a.is_empty()));
        assert!(gun.shot_dispersion_factors.is_some());
        assert_eq!(gun.burst, Some((1, 0.0)));
        assert!(gun.component.max_health.is_some());
        assert!(gun.component.max_regen_health.is_some());
        assert!(gun.component.repair_cost.is_some());

        // 2. Ferdinand <noDamage>true</noDamage> zone
        let ferdinand = catalog
            .vehicle_defs
            .values()
            .find(|v| v.name == "Ferdinand")
            .expect("Ferdinand def");
        let ferd_armor12 = ferdinand
            .hull_armor
            .as_ref()
            .and_then(|a| a.get("armor_12"))
            .expect("Ferdinand armor_12");
        assert_eq!(ferd_armor12, (30.0, true));

        // 3. OptionalDeviceDef & EquipmentDef <script> effects
        let toolbox = catalog
            .optional_devices
            .values()
            .find(|d| d.name == "toolbox")
            .expect("toolbox");
        assert_eq!(
            toolbox.effect,
            Some(OptionalDeviceEffect::StaticFactor {
                attribute: "miscAttrs/repairSpeedFactor".to_string(),
                factor: 1.25,
            })
        );
        let stereoscope = catalog
            .optional_devices
            .values()
            .find(|d| d.name == "stereoscope")
            .expect("stereoscope");
        assert_eq!(
            stereoscope.effect,
            Some(OptionalDeviceEffect::StillVehicleFactor {
                attribute: "circularVisionRadius".to_string(),
                factor: 1.25,
                activate_when_still_sec: 3.0,
            })
        );
        let vent1 = catalog
            .optional_devices
            .values()
            .find(|d| d.name == "improvedVentilation_class1")
            .expect("improvedVentilation_class1");
        assert_eq!(
            vent1.effect,
            Some(OptionalDeviceEffect::StaticAdditive {
                attribute: "miscAttrs/crewLevelIncrease".to_string(),
                value: 5.0,
            })
        );
        let christie = catalog
            .optional_devices
            .values()
            .find(|d| d.name == "enhancedSuspension_Christie")
            .expect("christie");
        assert_eq!(
            christie.effect,
            Some(OptionalDeviceEffect::EnhancedSuspension {
                chassis_max_load_factor: 1.1,
                chassis_health_factor: 1.3,
            })
        );
        let grousers = catalog
            .optional_devices
            .values()
            .find(|d| d.name == "grousers")
            .expect("grousers");
        assert_eq!(
            grousers.effect,
            Some(OptionalDeviceEffect::Grousers {
                soft_ground_resistance_factor: 0.909,
                medium_ground_resistance_factor: 0.952,
            })
        );

        let auto_ext = catalog
            .equipments
            .values()
            .find(|e| e.name == "autoExtinguishers")
            .expect("autoExtinguishers");
        assert_eq!(
            auto_ext.effect,
            Some(EquipmentEffect::Extinguisher {
                fire_starting_chance_factor: Some(0.9),
                autoactivate: true,
            })
        );
        let large_rep = catalog
            .equipments
            .values()
            .find(|e| e.name == "largeRepairkit")
            .expect("largeRepairkit");
        assert_eq!(
            large_rep.effect,
            Some(EquipmentEffect::Repairkit { repair_all: true })
        );
        let oil = catalog
            .equipments
            .values()
            .find(|e| e.name == "lendLeaseOil")
            .expect("lendLeaseOil");
        assert_eq!(
            oil.effect,
            Some(EquipmentEffect::Fuel {
                engine_power_factor: 1.05,
                turret_rotation_speed_factor: 1.0,
            })
        );
        let cola = catalog
            .equipments
            .values()
            .find(|e| e.name == "cocacola")
            .expect("cocacola");
        assert_eq!(
            cola.effect,
            Some(EquipmentEffect::Stimulator {
                crew_level_increase: 10,
            })
        );
        let rpm = catalog
            .equipments
            .values()
            .find(|e| e.name == "removedRpmLimiter")
            .expect("removedRpmLimiter");
        assert_eq!(
            rpm.effect,
            Some(EquipmentEffect::RemovedRpmLimiter {
                engine_power_factor: 1.1,
                engine_hp_loss_per_second: 1.5,
            })
        );

        // 4. Retail shared gun local overrides (_20mm_TNSH on MS-1 vs shared catalog.components)
        let tnsh_gun = ms1.turrets[0]
            .guns
            .iter()
            .find(|g| g.component.name == "_20mm_TNSH")
            .expect("MS-1 turret 0 _20mm_TNSH");
        assert_eq!(tnsh_gun.max_ammo, 620);
        assert_eq!(tnsh_gun.burst, Some((5, 0.2)));
        assert_eq!(tnsh_gun.effects.as_deref(), Some("shot_TNSH_auto"));
        let shared_tnsh = catalog
            .component(tnsh_gun.component.comp_descr)
            .expect("shared _20mm_TNSH in catalog.components");
        assert_eq!(
            shared_tnsh.max_ammo,
            Some(720),
            "per-turret maxAmmo override must not overwrite shared catalog.components"
        );
        assert!(
            shared_tnsh.hit_tester.is_none(),
            "per-turret hitTester override must not overwrite shared catalog.components"
        );
    }

    #[test]
    fn test_turret_gun_local_overrides_inherit_shared_and_preserve_catalog_components() {
        let root_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("..")
            .join("target")
            .join("test-tmp")
            .join("gun-locals-test");
        let _ = std::fs::remove_dir_all(&root_dir);

        for empty_nation in ["germany", "usa"] {
            let ndir = root_dir.join(empty_nation);
            let cdir = ndir.join("components");
            std::fs::create_dir_all(&cdir).expect("mkdir nation");
            for f in ["list", "customization"] {
                std::fs::write(ndir.join(format!("{f}.xml")), "<root/>").unwrap();
            }
            for f in ["shells", "chassis", "engines", "fuelTanks", "radios", "turrets", "guns"] {
                std::fs::write(cdir.join(format!("{f}.xml")), "<root><ids/><shared/></root>").unwrap();
            }
        }

        let ussr_dir = root_dir.join("ussr");
        let cdir = ussr_dir.join("components");
        std::fs::create_dir_all(&cdir).expect("mkdir ussr");
        std::fs::write(ussr_dir.join("customization.xml"), "<root/>").unwrap();
        std::fs::write(
            ussr_dir.join("list.xml"),
            "<root><TestTank><id>0</id><userString>TestTank</userString><level>1</level><price>1000</price><tags>lightTank</tags></TestTank></root>",
        ).unwrap();
        std::fs::write(
            cdir.join("shells.xml"),
            "<root><_20mm_AP><id>1</id><price>5</price><kind>ARMOR_PIERCING</kind><caliber>20</caliber><isTracer>true</isTracer><effects>smallAP</effects><damage><armor>12</armor><devices>20</devices></damage></_20mm_AP></root>",
        ).unwrap();
        std::fs::write(
            cdir.join("chassis.xml"),
            "<root><ids><Ch>1</Ch></ids><shared><Ch><weight>1000</weight><level>1</level><price>100</price><maxLoad>10000</maxLoad></Ch></shared></root>",
        ).unwrap();
        std::fs::write(
            cdir.join("engines.xml"),
            "<root><ids><Eng>1</Eng></ids><shared><Eng><weight>500</weight><level>1</level><price>100</price><power>100</power></Eng></shared></root>",
        ).unwrap();
        std::fs::write(
            cdir.join("fuelTanks.xml"),
            "<root><ids><Fuel>1</Fuel></ids><shared><Fuel><weight>100</weight><level>1</level><price>50</price></Fuel></shared></root>",
        ).unwrap();
        std::fs::write(
            cdir.join("radios.xml"),
            "<root><ids><Rad>1</Rad></ids><shared><Rad><weight>50</weight><level>1</level><price>50</price><distance>300</distance></Rad></shared></root>",
        ).unwrap();
        std::fs::write(
            cdir.join("guns.xml"),
            r#"<root>
  <ids><_20mm_auto>1</_20mm_auto><_37mm_local>2</_37mm_local></ids>
  <shared>
    <_20mm_auto>
      <tags>auto</tags>
      <level>2</level>
      <price>2580</price>
      <weight>68</weight>
      <maxHealth>15</maxHealth>
      <maxRegenHealth>7</maxRegenHealth>
      <repairCost>2.0</repairCost>
      <maxAmmo>720</maxAmmo>
      <pitchLimits>-25 8</pitchLimits>
      <rotationSpeed>50</rotationSpeed>
      <reloadTime>1.4</reloadTime>
      <aimingTime>1.5</aimingTime>
      <shotDispersionRadius>0.238</shotDispersionRadius>
      <effects>shot_TNSH_auto</effects>
      <burst><count>5</count><rate>300</rate></burst>
      <shotDispersionFactors><turretRotation>0.08</turretRotation><afterShot>1.0</afterShot><whileGunDamaged>2.0</whileGunDamaged></shotDispersionFactors>
      <shots><_20mm_AP><defaultPortion>1.0</defaultPortion><speed>800</speed><gravity>9.81</gravity><maxDistance>360</maxDistance><piercingPower>25 12</piercingPower></_20mm_AP></shots>
    </_20mm_auto>
  </shared>
</root>"#,
        ).unwrap();
        std::fs::write(
            cdir.join("turrets.xml"),
            r#"<root>
  <ids><SharedTurret>1</SharedTurret><LocalTurret>2</LocalTurret></ids>
  <shared>
    <SharedTurret>
      <level>2</level><price>500</price><weight>800</weight><maxHealth>100</maxHealth>
      <yawLimits>-180 180</yawLimits>
      <guns><_20mm_auto>shared<maxAmmo>800</maxAmmo></_20mm_auto></guns>
    </SharedTurret>
  </shared>
</root>"#,
        ).unwrap();
        std::fs::write(
            ussr_dir.join("TestTank.xml"),
            r#"<root>
  <crew><commander/><driver/></crew>
  <speedLimits><forward>30</forward><backward>10</backward></speedLimits>
  <repairCost>2.0</repairCost>
  <hull><weight>3000</weight><maxHealth>150</maxHealth><armor><armor_1>20</armor_1></armor><turretPositions><turret>0 0.5 0</turret></turretPositions></hull>
  <chassis><Ch>shared</Ch></chassis>
  <engines><Eng>shared</Eng></engines>
  <fuelTanks><Fuel>shared</Fuel></fuelTanks>
  <radios><Rad>shared</Rad></radios>
  <turrets0>
    <LocalTurret>
      <level>1</level><price>200</price><weight>500</weight><maxHealth>40</maxHealth>
      <yawLimits>-180 180</yawLimits>
      <guns>
        <_37mm_local>
          <level>1</level><price>300</price><weight>200</weight>
          <maxHealth>20</maxHealth><maxRegenHealth>10</maxRegenHealth><repairCost>1.5</repairCost>
          <maxAmmo>80</maxAmmo><pitchLimits>-10 20</pitchLimits><rotationSpeed>30</rotationSpeed>
          <reloadTime>2.5</reloadTime><aimingTime>2.0</aimingTime><shotDispersionRadius>0.4</shotDispersionRadius>
          <effects>shot_small</effects>
          <shotDispersionFactors><turretRotation>0.12</turretRotation><afterShot>3.0</afterShot><whileGunDamaged>2.0</whileGunDamaged></shotDispersionFactors>
          <shots><_20mm_AP><defaultPortion>1.0</defaultPortion><speed>500</speed><gravity>9.81</gravity><maxDistance>700</maxDistance><piercingPower>40 30</piercingPower></_20mm_AP></shots>
        </_37mm_local>
        <_20mm_auto>
          <maxAmmo>500</maxAmmo>
          <pitchLimits>-15 10</pitchLimits>
        </_20mm_auto>
      </guns>
    </LocalTurret>
    <SharedTurret>shared
      <yawLimits>-45 45</yawLimits>
      <guns>
        <_20mm_auto>shared
          <maxAmmo>920</maxAmmo>
          <hitTester><collisionModel>v/Gun_05.model</collisionModel></hitTester>
          <armor><gun>10</gun></armor>
        </_20mm_auto>
      </guns>
    </SharedTurret>
  </turrets0>
</root>"#,
        ).unwrap();

        let catalog = load_from_vehicles_dir(&root_dir).expect("load synthetic gun locals catalog");
        let _ = std::fs::remove_dir_all(&root_dir);

        let tank = catalog.vehicle_def_by_id(0, 0).expect("TestTank def");
        let local_turret = &tank.turrets[0];
        let local_37 = local_turret
            .guns
            .iter()
            .find(|g| g.component.name == "_37mm_local")
            .expect("_37mm_local");
        assert_eq!(local_37.burst, Some((1, 0.0)));
        assert_eq!(
            catalog.component(local_37.component.comp_descr).and_then(|c| c.burst),
            None,
            "missing <burst> on parsed ComponentDef must stay None until TurretGunEntry fallback"
        );

        let auto_no_shared_text = local_turret
            .guns
            .iter()
            .find(|g| g.component.name == "_20mm_auto")
            .expect("local override _20mm_auto");
        assert_eq!(auto_no_shared_text.max_ammo, 500);
        assert_eq!(auto_no_shared_text.pitch_limits, Some((-15.0, 10.0)));
        assert_eq!(auto_no_shared_text.component.max_health, Some(15));
        assert_eq!(auto_no_shared_text.component.max_regen_health, Some(7));
        assert_eq!(auto_no_shared_text.component.repair_cost, Some(2.0));
        assert_eq!(auto_no_shared_text.component.weight, 68.0);
        assert_eq!(auto_no_shared_text.component.level, 2);
        assert!(auto_no_shared_text.component.tags.contains("auto"));
        assert_eq!(auto_no_shared_text.shots.len(), 1);
        assert_eq!(
            auto_no_shared_text.shot_dispersion_factors,
            Some([0.08, 1.0, 2.0])
        );
        assert_eq!(
            auto_no_shared_text.component.gun_shot_dispersion_factors,
            Some([0.08, 1.0, 2.0])
        );
        assert_eq!(auto_no_shared_text.burst, Some((5, 0.2)));
        assert_eq!(auto_no_shared_text.component.burst, Some((5, 0.2)));
        assert_eq!(
            auto_no_shared_text.effects.as_deref(),
            Some("shot_TNSH_auto")
        );

        let shared_turret = &tank.turrets[1];
        assert_eq!(shared_turret.max_health, 100);
        assert_eq!(shared_turret.yaw_limits, Some((-45.0, 45.0)));
        let auto_shared_text = &shared_turret.guns[0];
        assert_eq!(auto_shared_text.max_ammo, 920);
        assert_eq!(auto_shared_text.burst, Some((5, 0.2)));
        assert_eq!(
            auto_shared_text.hit_tester.as_ref().map(|h| h.model.as_str()),
            Some("v/Gun_05.model")
        );

        // catalog.components must preserve the unmutated shared gun & turret entries
        let shared_gun_comp = catalog
            .component(auto_no_shared_text.component.comp_descr)
            .expect("shared gun in catalog.components");
        assert_eq!(shared_gun_comp.max_ammo, Some(720));
        assert!(shared_gun_comp.hit_tester.is_none());
        assert_eq!(shared_gun_comp.burst, Some((5, 0.2)));

        let shared_turret_comp = catalog
            .component(shared_turret.component.comp_descr)
            .expect("shared turret in catalog.components");
        assert_eq!(shared_turret_comp.max_health, Some(100));
    }
}
