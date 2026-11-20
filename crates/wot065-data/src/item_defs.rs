//! Parsers for WoT 0.6.5 shop prices, shells, crew slots, tankmen pools, and common artefacts.
//!
//! Mirrors the client/baseapp readers in `common/items/vehicles.py`,
//! `common/items/artefacts.py`, `common/items/tankmen.py`, and `common/items/_xml.py`.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::Path;

use crate::catalog::read_stem;
use crate::xml::XmlNode;

/// `nations.NONE_INDEX` (`common/nations.py:10`), used for nation-neutral items
/// (`optionalDevice` = 9, `equipment` = 11).
pub const NATION_NONE: u8 = 15;

/// Tankman primary role indices (`common/items/tankmen.py`, `SKILL_INDICES`).
pub fn role_name_to_id(name: &str) -> Result<u8, String> {
    match name {
        "commander" => Ok(1),
        "radioman" => Ok(2),
        "driver" => Ok(3),
        "gunner" => Ok(4),
        "loader" => Ok(5),
        other => Err(format!("unknown crew role `{other}`")),
    }
}

/// Encode an integer compact descriptor matching `vehicles.makeIntCompactDescrByID`
/// (`common/items/vehicles.py:793-795`) returning an `i64`.
pub fn make_item_comp_descr(item_type_id: u8, nation_id: u8, item_id: u16) -> i64 {
    ((item_id as i64) << 8) | ((nation_id as i64) << 4) | (item_type_id as i64)
}

/// Encode an integer compact descriptor matching `vehicles.makeIntCompactDescrByID`
/// (`common/items/vehicles.py:793-795`) returning an `i32`.
pub fn make_int_comp_descr(item_type_id: u8, nation_id: u8, item_id: u16) -> i32 {
    ((item_id as i32) << 8) | ((nation_id as i32) << 4) | (item_type_id as i32)
}

/// Decode an integer compact descriptor matching `vehicles.parseIntCompactDescr`
/// (`common/items/vehicles.py:747-751`). Returns `(item_type_id, nation_id, item_id)`.
pub fn parse_int_comp_descr(compact_descr: i32) -> (u8, u8, u16) {
    (
        (compact_descr & 15) as u8,
        ((compact_descr >> 4) & 15) as u8,
        ((compact_descr >> 8) & 0xffff) as u16,
    )
}

/// Price `(credits, gold)` and shop visibility (`notInShop`) of a catalog item.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ShopItemPrice {
    pub credits: i64,
    pub gold: i64,
    pub hidden: bool,
}

/// Nation-specific pool of valid tankman name/icon IDs from `item_defs/tankmen/<nation>.xml`.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct TankmanNamePool {
    pub first_names: Vec<u16>,
    pub last_names: Vec<u16>,
    pub icons: Vec<u16>,
}

/// Auxiliary item-def data loaded alongside [`crate::catalog::Catalog`] to populate
/// shop prices, default vehicle shells, and full tank crews without altering the
/// serialized `vehicles_catalog.json` schema.
#[derive(Debug, Clone, Default)]
pub struct ItemCatalogExtras {
    /// `(nation_id, item_type_id, shop_key) -> ShopItemPrice`.
    /// Vehicle entries use `type_id` as `shop_key`; all other item types use `compact_descr`.
    pub shop_items: BTreeMap<(u8, i64, i64), ShopItemPrice>,
    /// Ordered primary role IDs (`1..=5`) for `(nation_id, type_id)`.
    pub crew_roles: BTreeMap<(u8, u16), Vec<u8>>,
    /// Per-nation name and portrait IDs from `<normalGroups>` in `tankmen/<nation>.xml`.
    pub tankman_pools: BTreeMap<u8, TankmanNamePool>,
    /// Stock gun default ammo pairs `(shell_compact_descr, count)` for `(nation_id, type_id)`.
    pub default_shells: BTreeMap<(u8, u16), Vec<(i64, i64)>>,
}

/// Shell kind / armor penetration behavior from `components/shells.xml`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Serialize, serde::Deserialize)]
pub enum ShellKind {
    ArmorPiercing,
    ArmorPiercingCr,
    ArmorPiercingHe,
    HighExplosive,
    HollowCharge,
}

impl ShellKind {
    pub fn parse_kind(s: &str) -> Option<Self> {
        match s {
            "ARMOR_PIERCING" => Some(Self::ArmorPiercing),
            "ARMOR_PIERCING_CR" => Some(Self::ArmorPiercingCr),
            "ARMOR_PIERCING_HE" => Some(Self::ArmorPiercingHe),
            "HIGH_EXPLOSIVE" => Some(Self::HighExplosive),
            "HOLLOW_CHARGE" => Some(Self::HollowCharge),
            _ => None,
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            Self::ArmorPiercing => "ARMOR_PIERCING",
            Self::ArmorPiercingCr => "ARMOR_PIERCING_CR",
            Self::ArmorPiercingHe => "ARMOR_PIERCING_HE",
            Self::HighExplosive => "HIGH_EXPLOSIVE",
            Self::HollowCharge => "HOLLOW_CHARGE",
        }
    }

    /// True for all armor-piercing variants (`vehicles.py:1867`: `kind.startswith('ARMOR_PIERCING')`).
    pub fn is_armor_piercing(&self) -> bool {
        matches!(
            self,
            Self::ArmorPiercing | Self::ArmorPiercingCr | Self::ArmorPiercingHe
        )
    }
}

/// Parsed shell specification from `vehicles/<nation>/components/shells.xml`.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ShellXmlDef {
    pub id: u16,
    pub name: String,
    pub nation_id: u8,
    pub compact_descr: i32,
    pub kind: ShellKind,
    pub caliber: f64,
    pub is_tracer: bool,
    pub armor_damage: f64,
    pub devices_damage: f64,
    pub damage_randomization: f64,
    pub piercing_power_randomization: f64,
    pub explosion_radius: f64,
    pub effects: String,
    pub price: (i64, i64),
    pub show_in_shop: bool,
}

pub type ShellDef = ShellXmlDef;

/// Map of armor zone names (`armor_1`, `leftTrack`, `gun`, etc.) to `(thickness_mm, no_damage)`.
#[derive(Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ArmorZoneMap {
    pub zones: BTreeMap<String, (f64, bool)>,
}

impl ArmorZoneMap {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn insert(&mut self, zone: impl Into<String>, thickness: f64, no_damage: bool) {
        self.zones.insert(zone.into(), (thickness, no_damage));
    }

    pub fn get(&self, zone: &str) -> Option<(f64, bool)> {
        self.zones.get(zone).copied()
    }

    pub fn thickness(&self, zone: &str) -> Option<f64> {
        self.zones.get(zone).map(|&(t, _)| t)
    }

    pub fn is_empty(&self) -> bool {
        self.zones.is_empty()
    }

    pub fn len(&self) -> usize {
        self.zones.len()
    }
}

/// Collision model specification from `<hitTester>`.
#[derive(Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct HitTesterSpec {
    pub model: String,
    pub enclosing_box: Option<([f32; 3], [f32; 3])>,
}

/// Device health parameters (`maxHealth`, `maxRegenHealth`, `repairCost`).
#[derive(Debug, Clone, Copy, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct DeviceHealthDef {
    pub max_health: i32,
    pub max_regen_health: i32,
    pub repair_cost: f64,
}

impl DeviceHealthDef {
    pub fn max_repair_cost(&self) -> f64 {
        self.max_health.saturating_sub(self.max_regen_health).max(0) as f64 * self.repair_cost
    }
}

/// Runtime effect parsed from `optional_devices.xml` `<script>`.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum OptionalDeviceEffect {
    StaticFactor {
        attribute: String,
        factor: f64,
    },
    StaticAdditive {
        attribute: String,
        value: f64,
    },
    StillVehicleFactor {
        attribute: String,
        factor: f64,
        activate_when_still_sec: f64,
    },
    EnhancedSuspension {
        chassis_max_load_factor: f64,
        chassis_health_factor: f64,
    },
    Grousers {
        soft_ground_resistance_factor: f64,
        medium_ground_resistance_factor: f64,
    },
    Other {
        script: String,
    },
}

/// Runtime effect parsed from `equipments.xml` `<script>`.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum EquipmentEffect {
    Extinguisher {
        fire_starting_chance_factor: Option<f64>,
        autoactivate: bool,
    },
    Repairkit {
        repair_all: bool,
    },
    Fuel {
        engine_power_factor: f64,
        turret_rotation_speed_factor: f64,
    },
    Stimulator {
        crew_level_increase: i32,
    },
    RemovedRpmLimiter {
        engine_power_factor: f64,
        engine_hp_loss_per_second: f64,
    },
    Other {
        script: String,
    },
}

/// Aggregate vehicle battle attributes.
#[derive(Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct VehicleBattleDef {
    pub speed_limits: (f64, f64),
    pub invisibility: (f64, f64),
    pub hull_armor: ArmorZoneMap,
    pub primary_armor: Vec<String>,
    pub hull_position: [f64; 3],
    pub turret_positions: Vec<[f64; 3]>,
    pub hit_tester: Option<HitTesterSpec>,
    pub ammo_bay_health: Option<DeviceHealthDef>,
}

/// A parsed gun shot specification (`common/items/vehicles.py:_readShot`).
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ShotDef {
    pub shell_name: String,
    pub shell_id: u16,
    pub shell_cd: i32,
    pub default_portion: f64,
    pub speed: f64,
    pub max_distance: f64,
    pub piercing_power: (f64, f64),
    #[serde(skip, default)]
    pub gravity: f64,
    #[serde(skip, default)]
    pub shell: Option<ShellXmlDef>,
}

/// A vehicle component (chassis, engine, fuelTank, radio, turret, gun).
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ComponentDef {
    pub id: u16,
    pub comp_descr: i32,
    pub name: String,
    pub price: (i64, i64),
    pub show_in_shop: bool,
    pub weight: f64,
    pub level: u8,
    pub tags: BTreeSet<String>,
    pub max_load: Option<f64>,
    pub max_ammo: Option<u32>,
    pub shots: Vec<ShotDef>,
    #[serde(skip, default)]
    pub max_health: Option<i32>,
    #[serde(skip, default)]
    pub repair_cost: Option<f64>,
    #[serde(skip, default)]
    pub max_regen_health: Option<i32>,
    #[serde(skip, default)]
    pub hit_tester: Option<HitTesterSpec>,
    #[serde(skip, default)]
    pub armor: Option<ArmorZoneMap>,
    #[serde(skip, default)]
    pub engine_power: Option<f64>,
    #[serde(skip, default)]
    pub fire_starting_chance: Option<f64>,
    #[serde(skip, default)]
    pub radio_distance: Option<f64>,
    #[serde(skip, default)]
    pub max_climb_angle: Option<f64>,
    #[serde(skip, default)]
    pub rotation_speed: Option<f64>,
    #[serde(skip, default)]
    pub rotation_is_around_center: Option<bool>,
    #[serde(skip, default)]
    pub top_right_carrying_point: Option<[f64; 2]>,
    #[serde(skip, default)]
    pub hull_position: Option<[f64; 3]>,
    #[serde(skip, default)]
    pub shot_dispersion_factors: Option<[f64; 2]>,
    #[serde(skip, default)]
    pub gun_shot_dispersion_factors: Option<[f64; 3]>,
    #[serde(skip, default)]
    pub burst: Option<(i32, f64)>,
    #[serde(skip, default)]
    pub effects: Option<String>,
}

/// A gun mounted on a turret, with turret-local overrides.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct TurretGunEntry {
    pub component: ComponentDef,
    pub max_ammo: u32,
    pub shots: Vec<ShotDef>,
    pub pitch_limits: Option<(f64, f64)>,
    pub rotation_speed: Option<f64>,
    pub reload_time: Option<f64>,
    pub aiming_time: Option<f64>,
    pub shot_disp_radius: Option<f64>,
    #[serde(skip, default)]
    pub hit_tester: Option<HitTesterSpec>,
    #[serde(skip, default)]
    pub armor: Option<ArmorZoneMap>,
    #[serde(skip, default)]
    pub shot_dispersion_factors: Option<[f64; 3]>,
    #[serde(skip, default)]
    pub burst: Option<(i32, f64)>,
    #[serde(skip, default)]
    pub effects: Option<String>,
}

/// Compute default ammo counts matching `common/items/vehicles.py:822-837` (`getDefaultAmmoForGun`).
pub fn compute_default_ammo_for_gun(gun: &TurretGunEntry) -> Vec<(i32, i32)> {
    let mut ammo = Vec::with_capacity(gun.shots.len());
    let max_count = gun.max_ammo as i32;
    let mut curr_count = 0i32;

    for shot in &gun.shots {
        let mut shot_count = (shot.default_portion * (max_count as f64) + 0.5).floor() as i32;
        if curr_count + shot_count > max_count {
            shot_count = (max_count - curr_count).max(0);
        }
        curr_count += shot_count;
        ammo.push((shot.shell_cd, shot_count));
    }

    if (ammo.is_empty() || curr_count == 0) && !gun.shots.is_empty() {
        if !ammo.is_empty() {
            ammo[0].1 = max_count;
        } else {
            ammo.push((gun.shots[0].shell_cd, max_count));
        }
    }

    ammo
}

/// A turret definition including installable guns and turret yaw limits.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct TurretDef {
    pub component: ComponentDef,
    pub guns: Vec<TurretGunEntry>,
    pub yaw_limits: Option<(f64, f64)>,
    #[serde(skip, default)]
    pub max_health: i32,
    #[serde(skip, default)]
    pub gun_position: Option<[f64; 3]>,
    #[serde(skip, default)]
    pub circular_vision_radius: Option<f64>,
    #[serde(skip, default)]
    pub rotation_speed: Option<f64>,
    #[serde(skip, default)]
    pub armor: Option<ArmorZoneMap>,
    #[serde(skip, default)]
    pub primary_armor: Vec<String>,
    #[serde(skip, default)]
    pub turret_rotator_health: Option<DeviceHealthDef>,
    #[serde(skip, default)]
    pub surveying_device_health: Option<DeviceHealthDef>,
    #[serde(skip, default)]
    pub hit_tester: Option<HitTesterSpec>,
}

/// An unlock entry in vehicle research (`common/items/vehicles.py:_readUnlocks`).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct UnlockEntry {
    pub xp_cost: i64,
    pub target_cd: i32,
    pub prereq_cds: Vec<i32>,
}

/// A single crew seat specification (`primary_role`, and secondary roles if any).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct CrewSlotSpec {
    pub primary_role: String,
    pub secondary_roles: Vec<String>,
}

impl CrewSlotSpec {
    pub fn primary_role_id(&self) -> Result<u8, String> {
        role_name_to_id(&self.primary_role)
    }
}

/// A complete static vehicle definition matching WoT 0.6.5 client/server data.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct VehicleDef {
    pub nation: String,
    pub nation_id: u8,
    pub type_id: u16,
    pub name: String,
    pub veh_type_comp_descr: i32,
    pub hull_weight: f64,
    pub hull_max_health: i32,
    pub repair_cost: f64,
    pub tags: BTreeSet<String>,
    pub level: u8,
    pub default_player_emblem_id: u8,
    pub crew: Vec<CrewSlotSpec>,
    pub chassis: Vec<ComponentDef>,
    pub engines: Vec<ComponentDef>,
    pub fuel_tanks: Vec<ComponentDef>,
    pub radios: Vec<ComponentDef>,
    pub turrets: Vec<TurretDef>,
    pub unlocks_descrs: Vec<UnlockEntry>,
    pub autounlocked_items: Vec<i32>,
    pub installable_components: BTreeSet<i32>,
    pub is_default_elite: bool,
    pub price: (i64, i64),
    pub show_in_shop: bool,
    #[serde(skip, default)]
    pub speed_limits: Option<(f64, f64)>,
    #[serde(skip, default)]
    pub invisibility: Option<(f64, f64)>,
    #[serde(skip, default)]
    pub hull_armor: Option<ArmorZoneMap>,
    #[serde(skip, default)]
    pub hull_primary_armor: Vec<String>,
    #[serde(skip, default)]
    pub hull_hit_tester: Option<HitTesterSpec>,
    #[serde(skip, default)]
    pub hull_position: Option<[f64; 3]>,
    #[serde(skip, default)]
    pub turret_positions: Vec<[f64; 3]>,
    #[serde(skip, default)]
    pub ammo_bay_health: Option<DeviceHealthDef>,
    #[serde(skip, default)]
    pub battle_def: Option<VehicleBattleDef>,
}

/// Criteria for filtering a specific vehicle module or vehicle body in `VehicleFilter`.
#[derive(Debug, Clone, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
pub struct FilterItem {
    pub tags: Option<BTreeSet<String>>,
    pub min_level: Option<u8>,
    pub max_level: Option<u8>,
}

/// Single pattern in a `VehicleFilter` include or exclude list.
#[derive(Debug, Clone, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
pub struct FilterPattern {
    pub nations: Option<Vec<u8>>,
    pub vehicle: Option<FilterItem>,
    pub chassis: Option<FilterItem>,
    pub engine: Option<FilterItem>,
    pub fuel_tank: Option<FilterItem>,
    pub radio: Option<FilterItem>,
}

/// Vehicle compatibility filter matching `common/items/artefacts.py:_VehicleFilter`.
#[derive(Debug, Clone, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
pub struct VehicleFilter {
    pub exclude: Vec<FilterPattern>,
    pub include: Vec<FilterPattern>,
}

impl VehicleFilter {
    /// Evaluate vehicle compatibility against this filter matching `common/items/artefacts.py:295-414`.
    pub fn check_compatibility(
        &self,
        nation_id: u8,
        vehicle_tags: &BTreeSet<String>,
        vehicle_level: u8,
        chassis_tags: &BTreeSet<String>,
        chassis_level: u8,
        engine_tags: &BTreeSet<String>,
        engine_level: u8,
        fuel_tank_tags: &BTreeSet<String>,
        fuel_tank_level: u8,
        radio_tags: &BTreeSet<String>,
        radio_level: u8,
    ) -> Result<(), &'static str> {
        let match_subfilter = |patterns: &[FilterPattern]| -> (bool, bool) {
            let mut has_vehicle_type_ok = false;
            for entry in patterns {
                if let Some(ref nations) = entry.nations {
                    if !nations.contains(&nation_id) {
                        continue;
                    }
                }
                if let Some(ref veh) = entry.vehicle {
                    if let Some(ref tags) = veh.tags {
                        if tags.is_disjoint(vehicle_tags) {
                            continue;
                        }
                    }
                    if let Some(min) = veh.min_level {
                        if min > vehicle_level {
                            continue;
                        }
                    }
                    if let Some(max) = veh.max_level {
                        if max < vehicle_level {
                            continue;
                        }
                    }
                    has_vehicle_type_ok = true;
                } else {
                    has_vehicle_type_ok = true;
                }

                let check_comp = |item: &Option<FilterItem>, c_tags: &BTreeSet<String>, c_level: u8| -> bool {
                    if let Some(ref it) = item {
                        if let Some(ref tags) = it.tags {
                            if tags.is_disjoint(c_tags) {
                                return false;
                            }
                        }
                        if let Some(min) = it.min_level {
                            if min > c_level {
                                return false;
                            }
                        }
                        if let Some(max) = it.max_level {
                            if max < c_level {
                                return false;
                            }
                        }
                    }
                    true
                };

                let is_vehicle_ok = check_comp(&entry.chassis, chassis_tags, chassis_level)
                    && check_comp(&entry.engine, engine_tags, engine_level)
                    && check_comp(&entry.fuel_tank, fuel_tank_tags, fuel_tank_level)
                    && check_comp(&entry.radio, radio_tags, radio_level);

                if is_vehicle_ok {
                    return (true, true);
                }
            }
            (has_vehicle_type_ok, false)
        };

        if !self.exclude.is_empty() {
            let (is_veh_type_matched, is_veh_matched) = match_subfilter(&self.exclude);
            if is_veh_matched {
                return Err("not for current vehicle");
            }
            if is_veh_type_matched {
                return Err("not for this vehicle type");
            }
        }

        if !self.include.is_empty() {
            let (is_veh_type_matched, is_veh_matched) = match_subfilter(&self.include);
            if !is_veh_matched {
                if !is_veh_type_matched {
                    return Err("not for this vehicle type");
                }
                return Err("not for current vehicle");
            }
        }

        Ok(())
    }

    /// Convenience checker for a `VehicleDef` with its stock configuration.
    pub fn check_vehicle_def(&self, veh: &VehicleDef) -> Result<(), &'static str> {
        let empty_set = BTreeSet::new();
        let ch_tags = veh.chassis.first().map(|c| &c.tags).unwrap_or(&empty_set);
        let ch_lvl = veh.chassis.first().map(|c| c.level).unwrap_or(veh.level);
        let eng_tags = veh.engines.first().map(|c| &c.tags).unwrap_or(&empty_set);
        let eng_lvl = veh.engines.first().map(|c| c.level).unwrap_or(veh.level);
        let ft_tags = veh.fuel_tanks.first().map(|c| &c.tags).unwrap_or(&empty_set);
        let ft_lvl = veh.fuel_tanks.first().map(|c| c.level).unwrap_or(veh.level);
        let rad_tags = veh.radios.first().map(|c| &c.tags).unwrap_or(&empty_set);
        let rad_lvl = veh.radios.first().map(|c| c.level).unwrap_or(veh.level);
        self.check_compatibility(
            veh.nation_id,
            &veh.tags,
            veh.level,
            ch_tags,
            ch_lvl,
            eng_tags,
            eng_lvl,
            ft_tags,
            ft_lvl,
            rad_tags,
            rad_lvl,
        )
    }
}

/// Optional device definition (`vehicles/common/optional_devices.xml`).
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct OptionalDeviceDef {
    pub id: u16,
    pub comp_descr: i32,
    pub name: String,
    pub price: (i64, i64),
    pub show_in_shop: bool,
    pub removable: bool,
    pub weight: f64,
    pub veh_weight_fraction: f64,
    pub chassis_max_load_factor: f64,
    pub vehicle_filter: VehicleFilter,
    #[serde(skip, default)]
    pub effect: Option<OptionalDeviceEffect>,
}

/// Consumable equipment definition (`vehicles/common/equipments.xml`).
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct EquipmentDef {
    pub id: u16,
    pub comp_descr: i32,
    pub name: String,
    pub price: (i64, i64),
    pub show_in_shop: bool,
    pub tags: BTreeSet<String>,
    pub incompatible_installed: BTreeSet<String>,
    pub incompatible_active: BTreeSet<String>,
    pub vehicle_filter: VehicleFilter,
    #[serde(skip, default)]
    pub effect: Option<EquipmentEffect>,
}

/// Camouflage definition from `vehicles/<nation>/customization.xml`.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct CamouflageDef {
    pub id: u16,
    pub gold: i32,
    pub show_in_shop: bool,
    pub allow: BTreeSet<i32>,
    pub deny: BTreeSet<i32>,
    pub groups: Vec<String>,
}

/// Player emblem definition from `vehicles/common/player_emblems.xml`.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct PlayerEmblemDef {
    pub id: u8,
    pub name: String,
    pub gold: i32,
    pub is_restricted: bool,
}

/// Horn definition from `vehicles/common/horns.xml`.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct HornDef {
    pub id: u8,
    pub gold: i32,
    pub distance: f64,
    pub vehicle_tags: BTreeSet<String>,
}

/// Tankman passport group from `tankmen/<nation>.xml`.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct TankmanGroupDef {
    pub name: String,
    pub is_female: bool,
    pub weight: f64,
    pub first_names: Vec<u16>,
    pub last_names: Vec<u16>,
    pub icons: Vec<u16>,
}

/// Tankmen rank progression and passport groups configuration for a nation.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct TankmenConfig {
    pub role_ranks: BTreeMap<String, Vec<u8>>,
    pub normal_groups: Vec<TankmanGroupDef>,
    pub premium_groups: Vec<TankmanGroupDef>,
}

/// Parse `_xml.readPrice` + `<notInShop>` from an item XML element (`_xml.py:138-145`).
pub(crate) fn read_price(node: &XmlNode) -> ShopItemPrice {
    let hidden = node
        .child("notInShop")
        .map(|n| {
            let t = n.text().trim();
            t.eq_ignore_ascii_case("true") || t == "1"
        })
        .unwrap_or(false);
    let (credits, gold) = match node.child("price") {
        Some(price_node) => {
            let amount = price_node.parse_i64().unwrap_or(0);
            if price_node.child("gold").is_some() {
                (0, amount)
            } else {
                (amount, 0)
            }
        }
        None => (0, 0),
    };
    ShopItemPrice {
        credits,
        gold,
        hidden,
    }
}

/// Helper to parse 2-element float vectors from XML text.
pub(crate) fn parse_vector2(node: Option<&XmlNode>) -> Option<(f64, f64)> {
    let node = node?;
    let list = node.parse_list();
    if list.len() >= 2 {
        let x = list[0].parse().ok()?;
        let y = list[1].parse().ok()?;
        Some((x, y))
    } else {
        None
    }
}

/// Parse `<speedLimits>` (`<forward>` / `<backward>` sub-tags, with vector2 text fallback).
pub(crate) fn parse_speed_limits(node: Option<&XmlNode>) -> Option<(f64, f64)> {
    let node = node?;
    if let (Some(fwd), Some(bwd)) = (
        node.child("forward").and_then(XmlNode::parse_f64),
        node.child("backward").and_then(XmlNode::parse_f64),
    ) {
        return Some((fwd, bwd));
    }
    parse_vector2(Some(node))
}

/// Parse `<invisibility>` (`<moving>` and `<still>`).
pub(crate) fn parse_invisibility(node: Option<&XmlNode>) -> Option<(f64, f64)> {
    let node = node?;
    let moving = node.child("moving").and_then(XmlNode::parse_f64)?;
    let still = node.child("still").and_then(XmlNode::parse_f64)?;
    Some((moving, still))
}

#[inline]
pub(crate) fn parse_bool_text(node: &XmlNode) -> bool {
    let t = node.text().trim();
    t.eq_ignore_ascii_case("true") || t == "1"
}

#[inline]
pub(crate) fn parse_bool_child(node: &XmlNode, name: &str) -> bool {
    node.child(name).is_some_and(parse_bool_text)
}

pub(crate) fn parse_vec2_arr(node: Option<&XmlNode>) -> Option<[f64; 2]> {
    parse_vector2(node).map(|(x, y)| [x, y])
}

pub(crate) fn parse_vec3_arr(node: Option<&XmlNode>) -> Option<[f64; 3]> {
    let list = node?.parse_list();
    if list.len() >= 3 {
        Some([
            list[0].parse().ok()?,
            list[1].parse().ok()?,
            list[2].parse().ok()?,
        ])
    } else {
        None
    }
}

/// Parse `<armor>` child zones (`armor_1`, `leftTrack`, etc.) into `(thickness_mm, no_damage)`.
pub(crate) fn parse_armor_zones(node: &XmlNode) -> ArmorZoneMap {
    let mut map = ArmorZoneMap::new();
    for child in &node.children {
        if let Some(thickness) = child.parse_f64() {
            let no_damage = parse_bool_child(child, "noDamage");
            map.insert(child.name.clone(), thickness, no_damage);
        }
    }
    map
}

/// Parse `<primaryArmor>` whitespace list, matching `vehicles.py:1941-1945` defaults if omitted.
pub(crate) fn parse_primary_armor(node: Option<&XmlNode>) -> Vec<String> {
    match node {
        Some(n) => {
            let list: Vec<String> = n.parse_list().into_iter().map(str::to_string).collect();
            if list.is_empty() {
                vec!["armor_1".into(), "armor_3".into(), "armor_2".into()]
            } else {
                list
            }
        }
        None => vec!["armor_1".into(), "armor_3".into(), "armor_2".into()],
    }
}

/// Parse `<hitTester>` (`<collisionModel>` and optional `<enclosingBox>`).
pub(crate) fn parse_hit_tester(node: &XmlNode) -> Option<HitTesterSpec> {
    let model = node
        .child("collisionModel")
        .map(|n| n.text().to_string())
        .filter(|s| !s.is_empty())?;
    let enclosing_box = node.child("enclosingBox").and_then(|eb| {
        if let (Some(min), Some(max)) = (parse_vec3_arr(eb.child("min")), parse_vec3_arr(eb.child("max"))) {
            return Some((
                [min[0] as f32, min[1] as f32, min[2] as f32],
                [max[0] as f32, max[1] as f32, max[2] as f32],
            ));
        }
        let nums: Vec<f32> = eb
            .parse_list()
            .into_iter()
            .filter_map(|s| s.parse::<f32>().ok())
            .collect();
        if nums.len() >= 6 {
            Some(([nums[0], nums[1], nums[2]], [nums[3], nums[4], nums[5]]))
        } else {
            None
        }
    });
    Some(HitTesterSpec {
        model,
        enclosing_box,
    })
}

/// Parse device health block (`<maxHealth>`, `<maxRegenHealth>`, `<repairCost>`).
pub(crate) fn parse_device_health(node: &XmlNode) -> Option<DeviceHealthDef> {
    if node.child("maxHealth").is_none()
        && node.child("maxRegenHealth").is_none()
        && node.child("repairCost").is_none()
    {
        return None;
    }
    Some(DeviceHealthDef {
        max_health: node.child("maxHealth").and_then(XmlNode::parse_i64).unwrap_or(1) as i32,
        max_regen_health: node
            .child("maxRegenHealth")
            .and_then(XmlNode::parse_i64)
            .unwrap_or(0) as i32,
        repair_cost: node
            .child("repairCost")
            .and_then(XmlNode::parse_f64)
            .unwrap_or(0.0),
    })
}

pub(crate) fn parse_chassis_shot_dispersion_factors(node: &XmlNode) -> Option<[f64; 2]> {
    let mov = node.child("vehicleMovement").and_then(XmlNode::parse_f64)?;
    let rot = node.child("vehicleRotation").and_then(XmlNode::parse_f64)?;
    Some([mov, rot])
}

pub(crate) fn parse_gun_shot_dispersion_factors(node: &XmlNode) -> Option<[f64; 3]> {
    let turret_rot = node.child("turretRotation").and_then(XmlNode::parse_f64)?;
    let after_shot = node.child("afterShot").and_then(XmlNode::parse_f64)?;
    let while_damaged = node.child("whileGunDamaged").and_then(XmlNode::parse_f64)?;
    Some([turret_rot, after_shot, while_damaged])
}

pub(crate) fn parse_gun_burst(node: Option<&XmlNode>) -> (i32, f64) {
    let Some(b) = node else {
        return (1, 0.0);
    };
    let count = b.child("count").and_then(XmlNode::parse_i64).unwrap_or(1) as i32;
    let rate = b.child("rate").and_then(XmlNode::parse_f64).unwrap_or(0.0);
    let interval = if count > 1 && rate > 0.0 {
        60.0 / rate
    } else {
        0.0
    };
    (count, interval)
}

/// Read `<miscParams><projectileSpeedFactor>` from `vehicles/common/vehicle.xml` (default `0.8`).
pub(crate) fn read_projectile_speed_factor(common_dir: &Path) -> f64 {
    let Ok(root) = read_stem(common_dir, "vehicle") else {
        return 0.8;
    };
    root.child("miscParams")
        .and_then(|m| m.child("projectileSpeedFactor"))
        .and_then(XmlNode::parse_f64)
        .filter(|&f| f > 0.0)
        .unwrap_or(0.8)
}

/// Parse `<script>` effect from `optional_devices.xml`.
pub(crate) fn parse_optional_device_effect(script: &XmlNode) -> OptionalDeviceEffect {
    let name = script.text();
    match name {
        "StaticFactorDevice" => OptionalDeviceEffect::StaticFactor {
            attribute: script
                .child("attribute")
                .map(|n| n.text().to_string())
                .unwrap_or_default(),
            factor: script.child("factor").and_then(XmlNode::parse_f64).unwrap_or(1.0),
        },
        "StaticAdditiveDevice" => OptionalDeviceEffect::StaticAdditive {
            attribute: script
                .child("attribute")
                .map(|n| n.text().to_string())
                .unwrap_or_default(),
            value: script.child("value").and_then(XmlNode::parse_f64).unwrap_or(0.0),
        },
        "StillVehicleFactorDevice" => OptionalDeviceEffect::StillVehicleFactor {
            attribute: script
                .child("attribute")
                .map(|n| n.text().to_string())
                .unwrap_or_default(),
            factor: script.child("factor").and_then(XmlNode::parse_f64).unwrap_or(1.0),
            activate_when_still_sec: script
                .child("activateWhenStillSec")
                .and_then(XmlNode::parse_f64)
                .unwrap_or(0.0),
        },
        "EnhancedSuspension" => OptionalDeviceEffect::EnhancedSuspension {
            chassis_max_load_factor: script
                .child("chassisMaxLoadFactor")
                .and_then(XmlNode::parse_f64)
                .unwrap_or(1.0),
            chassis_health_factor: script
                .child("chassisHealthFactor")
                .and_then(XmlNode::parse_f64)
                .unwrap_or(1.0),
        },
        "Grousers" => OptionalDeviceEffect::Grousers {
            soft_ground_resistance_factor: script
                .child("softGroundResistanceFactor")
                .and_then(XmlNode::parse_f64)
                .unwrap_or(1.0),
            medium_ground_resistance_factor: script
                .child("mediumGroundResistanceFactor")
                .and_then(XmlNode::parse_f64)
                .unwrap_or(1.0),
        },
        other => OptionalDeviceEffect::Other {
            script: other.to_string(),
        },
    }
}

/// Parse `<script>` effect from `equipments.xml`.
pub(crate) fn parse_equipment_effect(script: &XmlNode) -> EquipmentEffect {
    let name = script.text();
    match name {
        "Extinguisher" => EquipmentEffect::Extinguisher {
            fire_starting_chance_factor: script
                .child("fireStartingChanceFactor")
                .and_then(XmlNode::parse_f64),
            autoactivate: parse_bool_child(script, "autoactivate"),
        },
        "Repairkit" => EquipmentEffect::Repairkit {
            repair_all: parse_bool_child(script, "repairAll"),
        },
        "Fuel" => EquipmentEffect::Fuel {
            engine_power_factor: script
                .child("enginePowerFactor")
                .and_then(XmlNode::parse_f64)
                .unwrap_or(1.0),
            turret_rotation_speed_factor: script
                .child("turretRotationSpeedFactor")
                .and_then(XmlNode::parse_f64)
                .unwrap_or(1.0),
        },
        "Stimulator" => EquipmentEffect::Stimulator {
            crew_level_increase: script
                .child("crewLevelIncrease")
                .and_then(XmlNode::parse_i64)
                .unwrap_or(0) as i32,
        },
        "RemovedRpmLimiter" => EquipmentEffect::RemovedRpmLimiter {
            engine_power_factor: script
                .child("enginePowerFactor")
                .and_then(XmlNode::parse_f64)
                .unwrap_or(1.0),
            engine_hp_loss_per_second: script
                .child("engineHpLossPerSecond")
                .and_then(XmlNode::parse_f64)
                .unwrap_or(0.0),
        },
        other => EquipmentEffect::Other {
            script: other.to_string(),
        },
    }
}

/// Parse `<crew>` primary role IDs in slot order (`MS-1.xml` -> `[1, 3]`).
pub(crate) fn parse_crew_roles(vehicle_root: &XmlNode) -> Result<Vec<u8>, String> {
    let crew = vehicle_root
        .child("crew")
        .ok_or_else(|| "vehicle xml has no <crew> section".to_string())?;
    let mut roles = Vec::with_capacity(crew.children.len());
    for child in &crew.children {
        roles.push(role_name_to_id(&child.name)?);
    }
    if roles.is_empty() {
        return Err("vehicle xml <crew> section is empty".to_string());
    }
    Ok(roles)
}

/// Parse shot definition from a gun `<shots>` child element using default `projectileSpeedFactor = 0.8`.
pub fn parse_shot_def(node: &XmlNode, shell_id: u16, nation_id: u8) -> ShotDef {
    parse_shot_def_with_factor(node, shell_id, nation_id, 0.8, None)
}

/// Parse shot definition from a gun `<shots>` child element (`common/items/vehicles.py:_readShot`),
/// scaling `speed` by `projectile_speed_factor` and `gravity` by `projectile_speed_factor^2`.
pub fn parse_shot_def_with_factor(
    node: &XmlNode,
    shell_id: u16,
    nation_id: u8,
    projectile_speed_factor: f64,
    shell_defs: Option<&BTreeMap<i32, ShellXmlDef>>,
) -> ShotDef {
    let shell_name = node.name.clone();
    let shell_cd = make_int_comp_descr(10, nation_id, shell_id);
    let default_portion = node
        .child("defaultPortion")
        .and_then(XmlNode::parse_f64)
        .unwrap_or(0.0);
    let raw_speed = node
        .child("speed")
        .and_then(XmlNode::parse_f64)
        .unwrap_or(0.0);
    let raw_gravity = node
        .child("gravity")
        .and_then(XmlNode::parse_f64)
        .unwrap_or(0.0);
    let max_distance = node
        .child("maxDistance")
        .and_then(XmlNode::parse_f64)
        .unwrap_or(0.0);
    let piercing_power = parse_vector2(node.child("piercingPower")).unwrap_or((0.0, 0.0));
    let speed = raw_speed * projectile_speed_factor;
    let gravity = raw_gravity * projectile_speed_factor * projectile_speed_factor;
    let shell = shell_defs.and_then(|defs| defs.get(&shell_cd).cloned());
    ShotDef {
        shell_name,
        shell_id,
        shell_cd,
        default_portion,
        speed,
        max_distance,
        piercing_power,
        gravity,
        shell,
    }
}

/// Parse a component definition from an XML node (either shared or vehicle-local).
pub fn parse_component_def(
    node: &XmlNode,
    id: u16,
    item_type_id: u8,
    nation_id: u8,
    shell_ids: Option<&HashMap<String, u16>>,
) -> ComponentDef {
    parse_component_def_full(node, id, item_type_id, nation_id, shell_ids, None, 0.8)
}

/// Parse a component definition including shell references and `projectileSpeedFactor` scaling.
pub(crate) fn parse_component_def_full(
    node: &XmlNode,
    id: u16,
    item_type_id: u8,
    nation_id: u8,
    shell_ids: Option<&HashMap<String, u16>>,
    shell_defs: Option<&BTreeMap<i32, ShellXmlDef>>,
    projectile_speed_factor: f64,
) -> ComponentDef {
    let comp_descr = make_int_comp_descr(item_type_id, nation_id, id);
    let name = node.name.clone();
    let price_info = read_price(node);
    let price = (price_info.credits, price_info.gold);
    let show_in_shop = !price_info.hidden;
    let weight = node
        .child("weight")
        .and_then(XmlNode::parse_f64)
        .unwrap_or(0.0);
    let level = node
        .child("level")
        .and_then(XmlNode::parse_i64)
        .unwrap_or(1) as u8;
    let tags: BTreeSet<String> = node
        .child("tags")
        .map(|n| n.parse_list().into_iter().map(str::to_string).collect())
        .unwrap_or_default();
    let max_load = node.child("maxLoad").and_then(XmlNode::parse_f64);
    let max_ammo = node
        .child("maxAmmo")
        .and_then(XmlNode::parse_i64)
        .map(|x| x as u32);
    let max_health = node
        .child("maxHealth")
        .and_then(XmlNode::parse_i64)
        .map(|x| x as i32);
    let repair_cost = node.child("repairCost").and_then(XmlNode::parse_f64);
    let max_regen_health = node
        .child("maxRegenHealth")
        .and_then(XmlNode::parse_i64)
        .map(|x| x as i32);
    let hit_tester = node.child("hitTester").and_then(parse_hit_tester);
    let armor = node.child("armor").map(parse_armor_zones);
    let engine_power = if item_type_id == 5 {
        node.child("power").and_then(XmlNode::parse_f64)
    } else {
        None
    };
    let fire_starting_chance = node.child("fireStartingChance").and_then(XmlNode::parse_f64);
    let radio_distance = if item_type_id == 7 {
        node.child("distance").and_then(XmlNode::parse_f64)
    } else {
        None
    };
    let max_climb_angle = node.child("maxClimbAngle").and_then(XmlNode::parse_f64);
    let rotation_speed = node.child("rotationSpeed").and_then(XmlNode::parse_f64);
    let rotation_is_around_center = node.child("rotationIsAroundCenter").map(parse_bool_text);
    let top_right_carrying_point = parse_vec2_arr(node.child("topRightCarryingPoint"));
    let hull_position = parse_vec3_arr(node.child("hullPosition"));
    let shot_dispersion_factors = if item_type_id == 2 {
        node.child("shotDispersionFactors")
            .and_then(parse_chassis_shot_dispersion_factors)
    } else {
        None
    };
    let gun_shot_dispersion_factors = if item_type_id == 4 {
        node.child("shotDispersionFactors")
            .and_then(parse_gun_shot_dispersion_factors)
    } else {
        None
    };
    let burst = if item_type_id == 4 {
        node.child("burst").map(|b| parse_gun_burst(Some(b)))
    } else {
        None
    };
    let effects = node
        .child("effects")
        .map(|n| n.text().to_string())
        .filter(|s| !s.is_empty());

    let mut shots = Vec::new();
    if let Some(shots_node) = node.child("shots") {
        if let Some(s_ids) = shell_ids {
            for s in &shots_node.children {
                if let Some(&sid) = s_ids.get(&s.name) {
                    shots.push(parse_shot_def_with_factor(
                        s,
                        sid,
                        nation_id,
                        projectile_speed_factor,
                        shell_defs,
                    ));
                }
            }
        }
    }
    ComponentDef {
        id,
        comp_descr,
        name,
        price,
        show_in_shop,
        weight,
        level,
        tags,
        max_load,
        max_ammo,
        shots,
        max_health,
        repair_cost,
        max_regen_health,
        hit_tester,
        armor,
        engine_power,
        fire_starting_chance,
        radio_distance,
        max_climb_angle,
        rotation_speed,
        rotation_is_around_center,
        top_right_carrying_point,
        hull_position,
        shot_dispersion_factors,
        gun_shot_dispersion_factors,
        burst,
        effects,
    }
}

/// Load nation shells (`components/shells.xml`), returning `(name -> id)` and `compactDescr -> ShellXmlDef`.
pub(crate) fn load_nation_shells_with_defs(
    components_dir: &Path,
    nation_id: u8,
    extras: &mut ItemCatalogExtras,
) -> Result<(HashMap<String, u16>, BTreeMap<i32, ShellXmlDef>), String> {
    let root = read_stem(components_dir, "shells")?;
    let mut by_name = HashMap::new();
    let mut defs = BTreeMap::new();
    for child in &root.children {
        if child.name == "icons" {
            continue;
        }
        let Some(id) = child.child("id").and_then(XmlNode::parse_i64) else {
            continue;
        };
        if !(0..=u16::MAX as i64).contains(&id) {
            return Err(format!("shell {} id {id} out of range", child.name));
        }
        let shell_id = id as u16;
        by_name.insert(child.name.clone(), shell_id);
        let cd = make_item_comp_descr(10, nation_id, shell_id);
        let cd_i32 = make_int_comp_descr(10, nation_id, shell_id);
        let price_info = read_price(child);
        let price = (price_info.credits, price_info.gold);
        let show_in_shop = !price_info.hidden;
        extras.shop_items.insert((nation_id, 10, cd), price_info);

        let kind_str = child.child("kind").map(XmlNode::text).unwrap_or("");
        let kind = ShellKind::parse_kind(kind_str).ok_or_else(|| {
            format!("shell {} has unknown <kind> '{kind_str}'", child.name)
        })?;
        let caliber = child
            .child("caliber")
            .and_then(XmlNode::parse_f64)
            .unwrap_or(0.0);
        let is_tracer = parse_bool_child(child, "isTracer");
        let damage_node = child.child("damage");
        let armor_damage = damage_node
            .and_then(|d| d.child("armor"))
            .and_then(XmlNode::parse_f64)
            .unwrap_or(0.0);
        let devices_damage = damage_node
            .and_then(|d| d.child("devices"))
            .and_then(XmlNode::parse_f64)
            .unwrap_or(0.0);
        let damage_randomization = child
            .child("damageRandomization")
            .and_then(XmlNode::parse_f64)
            .unwrap_or(0.25);
        let piercing_power_randomization = child
            .child("piercingPowerRandomization")
            .and_then(XmlNode::parse_f64)
            .unwrap_or(0.25);
        let explosion_radius = if kind == ShellKind::HighExplosive {
            child
                .child("explosionRadius")
                .and_then(XmlNode::parse_f64)
                .filter(|&r| r > 0.0)
                .unwrap_or_else(|| caliber * caliber / 5555.0)
        } else {
            0.0
        };
        let effects = child
            .child("effects")
            .map(|n| n.text().to_string())
            .unwrap_or_default();

        defs.insert(
            cd_i32,
            ShellXmlDef {
                id: shell_id,
                name: child.name.clone(),
                nation_id,
                compact_descr: cd_i32,
                kind,
                caliber,
                is_tracer,
                armor_damage,
                devices_damage,
                damage_randomization,
                piercing_power_randomization,
                explosion_radius,
                effects,
                price,
                show_in_shop,
            },
        );
    }
    Ok((by_name, defs))
}

/// Compute `getDefaultAmmoForGun` (`common/items/vehicles.py:822-837`) for the stock gun.
pub(crate) fn parse_default_shells_for_gun(
    gun_element: &XmlNode,
    shared_guns: &HashMap<String, XmlNode>,
    shell_ids: &HashMap<String, u16>,
    nation_id: u8,
) -> Result<Vec<(i64, i64)>, String> {
    let base_gun = if gun_element.text().trim() == "shared" {
        Some(
            shared_guns
                .get(&gun_element.name)
                .ok_or_else(|| format!("gun {} is shared but missing from guns.xml", gun_element.name))?,
        )
    } else {
        shared_guns.get(&gun_element.name)
    };

    let max_ammo = gun_element
        .child("maxAmmo")
        .or_else(|| base_gun.and_then(|g| g.child("maxAmmo")))
        .and_then(XmlNode::parse_i64)
        .ok_or_else(|| format!("gun {} has no <maxAmmo>", gun_element.name))?;

    let shots_node = gun_element
        .child("shots")
        .or_else(|| base_gun.and_then(|g| g.child("shots")))
        .ok_or_else(|| format!("gun {} has no <shots>", gun_element.name))?;

    if shots_node.children.is_empty() {
        return Err(format!("gun {} has empty <shots>", gun_element.name));
    }

    let mut ammo = Vec::with_capacity(shots_node.children.len());
    let mut curr_count = 0i64;
    for shot in &shots_node.children {
        let shell_id = shell_ids
            .get(&shot.name)
            .ok_or_else(|| format!("gun {} references unknown shell {}", gun_element.name, shot.name))?;
        let shell_cd = make_item_comp_descr(10, nation_id, *shell_id);
        let portion = shot
            .child("defaultPortion")
            .and_then(XmlNode::parse_f64)
            .unwrap_or(0.0);
        let mut shot_count = (portion * (max_ammo as f64) + 0.5).floor() as i64;
        if curr_count + shot_count > max_ammo {
            shot_count = max_ammo - curr_count;
        }
        curr_count += shot_count;
        ammo.push((shell_cd, shot_count));
    }
    Ok(ammo)
}

/// Record `<shared>` entries for a component table file into `extras.shop_items`.
pub(crate) fn record_shared_component_prices(
    root: &XmlNode,
    ids: &HashMap<String, u16>,
    nation_id: u8,
    item_type_id: i64,
    extras: &mut ItemCatalogExtras,
) {
    let Some(shared) = root.child("shared") else {
        return;
    };
    for child in &shared.children {
        if let Some(&id) = ids.get(&child.name) {
            let cd = make_item_comp_descr(item_type_id as u8, nation_id, id);
            extras
                .shop_items
                .insert((nation_id, item_type_id, cd), read_price(child));
        }
    }
}

/// Record inline (non-shared) modules defined inside a vehicle XML into `extras.shop_items`.
pub(crate) fn record_vehicle_component_prices(
    vehicle_root: &XmlNode,
    nation_id: u8,
    component_maps: &[(&str, i64, &HashMap<String, u16>)],
    turret_ids: &HashMap<String, u16>,
    gun_ids: &HashMap<String, u16>,
    extras: &mut ItemCatalogExtras,
) {
    for &(section_name, item_type_id, id_map) in component_maps {
        if let Some(section) = vehicle_root.child(section_name) {
            for child in &section.children {
                if child.text().trim() != "shared" {
                    if let Some(&id) = id_map.get(&child.name) {
                        let cd = make_item_comp_descr(item_type_id as u8, nation_id, id);
                        extras
                            .shop_items
                            .insert((nation_id, item_type_id, cd), read_price(child));
                    }
                }
            }
        }
    }

    for child in &vehicle_root.children {
        if child.name.starts_with("turrets") {
            for turret in &child.children {
                if turret.text().trim() != "shared" {
                    if let Some(&tid) = turret_ids.get(&turret.name) {
                        let cd = make_item_comp_descr(3, nation_id, tid);
                        extras
                            .shop_items
                            .insert((nation_id, 3, cd), read_price(turret));
                    }
                }
                if let Some(guns) = turret.child("guns") {
                    for gun in &guns.children {
                        if gun.text().trim() != "shared" {
                            if let Some(&gid) = gun_ids.get(&gun.name) {
                                let cd = make_item_comp_descr(4, nation_id, gid);
                                extras
                                    .shop_items
                                    .insert((nation_id, 4, cd), read_price(gun));
                            }
                        }
                    }
                }
            }
        }
    }
}

/// Load nation tankman firstName/lastName/icon ID pools from `<normalGroups>`.
pub(crate) fn load_tankman_pool(tankmen_dir: &Path, nation: &str) -> Result<TankmanNamePool, String> {
    let root = read_stem(tankmen_dir, nation)?;
    let normal_groups = root
        .child("normalGroups")
        .ok_or_else(|| format!("tankmen/{nation}.xml missing <normalGroups>"))?;

    let group = normal_groups
        .children
        .iter()
        .find(|g| g.child("sex").map(|s| s.text().trim() != "female").unwrap_or(true))
        .or_else(|| normal_groups.first_child())
        .ok_or_else(|| format!("tankmen/{nation}.xml <normalGroups> is empty"))?;

    let pool = TankmanNamePool {
        first_names: parse_underscore_ids(group.child("firstNames")),
        last_names: parse_underscore_ids(group.child("lastNames")),
        icons: parse_underscore_ids(group.child("icons")),
    };
    if pool.first_names.is_empty() || pool.last_names.is_empty() || pool.icons.is_empty() {
        return Err(format!(
            "tankmen/{nation}.xml <normalGroups>/{} has empty name/icon list",
            group.name
        ));
    }
    Ok(pool)
}

/// Load `vehicles/common/{optional_devices,equipments}.xml` under nation `NATION_NONE` (15).
pub(crate) fn load_common_artefacts(
    common_dir: &Path,
    extras: &mut ItemCatalogExtras,
) -> Result<(), String> {
    for (stem, item_type_id) in [("optional_devices", 9i64), ("equipments", 11i64)] {
        let root = read_stem(common_dir, stem)?;
        for child in &root.children {
            let Some(id) = child.child("id").and_then(XmlNode::parse_i64) else {
                continue;
            };
            if !(0..=u16::MAX as i64).contains(&id) {
                return Err(format!("{stem}/{} id {id} out of range", child.name));
            }
            let cd = make_item_comp_descr(item_type_id as u8, NATION_NONE, id as u16);
            extras
                .shop_items
                .insert((NATION_NONE, item_type_id, cd), read_price(child));
        }
    }
    Ok(())
}

fn read_filter_item(node: &XmlNode) -> FilterItem {
    let tags = node
        .child("tags")
        .map(|n| n.parse_list().into_iter().map(str::to_string).collect());
    let min_level = node
        .child("minLevel")
        .and_then(XmlNode::parse_i64)
        .map(|l| l as u8);
    let max_level = node
        .child("maxLevel")
        .and_then(XmlNode::parse_i64)
        .map(|l| l as u8);
    FilterItem {
        tags,
        min_level,
        max_level,
    }
}

fn read_vehicle_filter_pattern(node: &XmlNode) -> Result<FilterPattern, String> {
    let mut pat = FilterPattern::default();
    for child in &node.children {
        match child.name.as_str() {
            "nations" => {
                let mut nat_ids = Vec::new();
                for name in child.parse_list() {
                    let id = match name {
                        "ussr" => 0u8,
                        "germany" => 1u8,
                        "usa" => 2u8,
                        other => return Err(format!("unknown nation in filter: {other}")),
                    };
                    nat_ids.push(id);
                }
                pat.nations = Some(nat_ids);
            }
            "vehicle" => pat.vehicle = Some(read_filter_item(child)),
            "chassis" => pat.chassis = Some(read_filter_item(child)),
            "engine" => pat.engine = Some(read_filter_item(child)),
            "fuelTank" => pat.fuel_tank = Some(read_filter_item(child)),
            "radio" => pat.radio = Some(read_filter_item(child)),
            _ => {}
        }
    }
    Ok(pat)
}

/// Parse `<vehicleFilter>` element with `<include>` and `<exclude>` sub-patterns.
pub fn read_vehicle_filter(node: Option<&XmlNode>) -> Result<VehicleFilter, String> {
    let mut filter = VehicleFilter::default();
    let Some(node) = node else {
        return Ok(filter);
    };
    for child in &node.children {
        if child.name == "include" {
            filter.include.push(read_vehicle_filter_pattern(child)?);
        } else if child.name == "exclude" {
            filter.exclude.push(read_vehicle_filter_pattern(child)?);
        }
    }
    Ok(filter)
}

/// Parse `vehicles/common/optional_devices.xml` into a map keyed by compact descriptor.
pub fn load_optional_devices(common_dir: &Path) -> Result<BTreeMap<i32, OptionalDeviceDef>, String> {
    let root = read_stem(common_dir, "optional_devices")?;
    let mut out = BTreeMap::new();
    for child in &root.children {
        let Some(id) = child.child("id").and_then(XmlNode::parse_i64) else {
            continue;
        };
        if !(0..=u16::MAX as i64).contains(&id) {
            return Err(format!("optional_devices/{} id {id} out of range", child.name));
        }
        let id = id as u16;
        let comp_descr = make_int_comp_descr(9, NATION_NONE, id);
        let name = child.name.clone();
        let price_info = read_price(child);
        let price = (price_info.credits, price_info.gold);
        let show_in_shop = !price_info.hidden;
        let removable = child
            .child("removable")
            .map(|n| {
                let t = n.text();
                t.eq_ignore_ascii_case("true") || t == "1"
            })
            .unwrap_or(false);
        let script = child.child("script");
        let veh_weight_fraction = script
            .and_then(|s| s.child("vehicleWeightFraction"))
            .and_then(XmlNode::parse_f64)
            .unwrap_or(0.0);
        let weight = script
            .and_then(|s| s.child("weight"))
            .and_then(XmlNode::parse_f64)
            .unwrap_or(0.0);
        let chassis_max_load_factor = script
            .and_then(|s| s.child("chassisMaxLoadFactor"))
            .and_then(XmlNode::parse_f64)
            .unwrap_or(1.0);
        let vehicle_filter = read_vehicle_filter(child.child("vehicleFilter"))?;
        let effect = script.map(parse_optional_device_effect);
        out.insert(
            comp_descr,
            OptionalDeviceDef {
                id,
                comp_descr,
                name,
                price,
                show_in_shop,
                removable,
                weight,
                veh_weight_fraction,
                chassis_max_load_factor,
                vehicle_filter,
                effect,
            },
        );
    }
    Ok(out)
}

/// Parse `vehicles/common/equipments.xml` into a map keyed by compact descriptor.
pub fn load_equipments(common_dir: &Path) -> Result<BTreeMap<i32, EquipmentDef>, String> {
    let root = read_stem(common_dir, "equipments")?;
    let mut out = BTreeMap::new();
    for child in &root.children {
        let Some(id) = child.child("id").and_then(XmlNode::parse_i64) else {
            continue;
        };
        if !(0..=u16::MAX as i64).contains(&id) {
            return Err(format!("equipments/{} id {id} out of range", child.name));
        }
        let id = id as u16;
        let comp_descr = make_int_comp_descr(11, NATION_NONE, id);
        let name = child.name.clone();
        let price_info = read_price(child);
        let price = (price_info.credits, price_info.gold);
        let show_in_shop = !price_info.hidden;
        let tags: BTreeSet<String> = child
            .child("tags")
            .map(|n| n.parse_list().into_iter().map(str::to_string).collect())
            .unwrap_or_default();
        let mut incompatible_installed = BTreeSet::new();
        let mut incompatible_active = BTreeSet::new();
        if let Some(incomp) = child.child("incompatibleTags") {
            if let Some(inst) = incomp.child("installed") {
                incompatible_installed.extend(inst.parse_list().into_iter().map(str::to_string));
            }
            if let Some(act) = incomp.child("active") {
                incompatible_active.extend(act.parse_list().into_iter().map(str::to_string));
            }
        }
        let vehicle_filter = read_vehicle_filter(child.child("vehicleFilter"))?;
        let effect = child.child("script").map(parse_equipment_effect);
        out.insert(
            comp_descr,
            EquipmentDef {
                id,
                comp_descr,
                name,
                price,
                show_in_shop,
                tags,
                incompatible_installed,
                incompatible_active,
                vehicle_filter,
                effect,
            },
        );
    }
    Ok(out)
}

/// Parse `vehicles/common/player_emblems.xml` returning emblems by ID and by name.
pub fn load_player_emblems(
    common_dir: &Path,
) -> Result<(BTreeMap<u8, PlayerEmblemDef>, BTreeMap<String, u8>), String> {
    let root = read_stem(common_dir, "player_emblems")?;
    let mut defs = BTreeMap::new();
    let mut ids_by_name = BTreeMap::new();
    for (group_name, is_restricted) in [("common", false), ("restricted", true)] {
        if let Some(group) = root.child(group_name) {
            for child in &group.children {
                let Some(id) = child.child("id").and_then(XmlNode::parse_i64) else {
                    continue;
                };
                if !(0..=255).contains(&id) {
                    return Err(format!("player_emblems/{} id {id} out of range", child.name));
                }
                let id = id as u8;
                let gold = child.child("gold").and_then(XmlNode::parse_i64).unwrap_or(0) as i32;
                let name = child.name.clone();
                defs.insert(
                    id,
                    PlayerEmblemDef {
                        id,
                        name: name.clone(),
                        gold,
                        is_restricted,
                    },
                );
                ids_by_name.insert(name, id);
            }
        }
    }
    Ok((defs, ids_by_name))
}

/// Parse `vehicles/common/horns.xml` into a map keyed by horn ID.
pub fn load_horns(common_dir: &Path) -> Result<BTreeMap<u8, HornDef>, String> {
    let root = read_stem(common_dir, "horns")?;
    let mut out = BTreeMap::new();
    for child in &root.children {
        let Some(id) = child.child("id").and_then(XmlNode::parse_i64) else {
            continue;
        };
        if !(0..=255).contains(&id) {
            return Err(format!("horns/{} id {id} out of range", child.name));
        }
        let id = id as u8;
        let gold = child.child("gold").and_then(XmlNode::parse_i64).unwrap_or(1) as i32;
        let distance = child.child("distance").and_then(XmlNode::parse_f64).unwrap_or(0.0);
        let vehicle_tags: BTreeSet<String> = child
            .child("vehicleTags")
            .map(|n| n.parse_list().into_iter().map(str::to_string).collect())
            .unwrap_or_default();
        out.insert(
            id,
            HornDef {
                id,
                gold,
                distance,
                vehicle_tags,
            },
        );
    }
    Ok(out)
}

/// Parse `vehicles/<nation>/customization.xml` into a map keyed by camouflage ID.
pub fn load_nation_customization(
    nation_dir: &Path,
    nation_id: u8,
    veh_names_to_ids: &HashMap<String, u16>,
) -> Result<BTreeMap<u16, CamouflageDef>, String> {
    let root = read_stem(nation_dir, "customization")?;
    let mut out = BTreeMap::new();
    if let Some(camouflages) = root.child("camouflages") {
        for child in &camouflages.children {
            let Some(id) = child.child("id").and_then(XmlNode::parse_i64) else {
                continue;
            };
            if !(0..=u16::MAX as i64).contains(&id) {
                return Err(format!("customization/{} id {id} out of range", child.name));
            }
            let id = id as u16;
            let gold = child.child("gold").and_then(XmlNode::parse_i64).unwrap_or(0) as i32;
            let show_in_shop = !child
                .child("notInShop")
                .map(|n| {
                    let t = n.text();
                    t.eq_ignore_ascii_case("true") || t == "1"
                })
                .unwrap_or(false);
            let groups = child
                .child("groups")
                .map(|n| n.parse_list().into_iter().map(str::to_string).collect())
                .unwrap_or_default();
            let mut allow = BTreeSet::new();
            if let Some(allow_node) = child.child("allow") {
                for name in allow_node.parse_list() {
                    if let Some(&vid) = veh_names_to_ids.get(name) {
                        allow.insert(make_int_comp_descr(1, nation_id, vid));
                    }
                }
            }
            let mut deny = BTreeSet::new();
            if let Some(deny_node) = child.child("deny") {
                for name in deny_node.parse_list() {
                    if let Some(&vid) = veh_names_to_ids.get(name) {
                        deny.insert(make_int_comp_descr(1, nation_id, vid));
                    }
                }
            }
            out.insert(
                id,
                CamouflageDef {
                    id,
                    gold,
                    show_in_shop,
                    allow,
                    deny,
                    groups,
                },
            );
        }
    }
    Ok(out)
}

/// Parse `tankmen/<nation>.xml` rank progression and passport groups.
pub fn load_nation_tankmen_config(tankmen_dir: &Path, nation: &str) -> Result<TankmenConfig, String> {
    let root = read_stem(tankmen_dir, nation)?;
    let mut rank_ids_by_name = HashMap::new();
    if let Some(ranks_node) = root.child("ranks") {
        for (idx, child) in ranks_node.children.iter().enumerate() {
            rank_ids_by_name.insert(child.name.clone(), idx as u8);
        }
    }

    let mut role_ranks = BTreeMap::new();
    if let Some(rr_node) = root.child("roleRanks") {
        for child in &rr_node.children {
            let mut rank_list = Vec::new();
            for rname in child.parse_list() {
                if let Some(&rid) = rank_ids_by_name.get(rname) {
                    rank_list.push(rid);
                }
            }
            role_ranks.insert(child.name.clone(), rank_list);
        }
    }

    let parse_groups = |sec_name: &str| -> Vec<TankmanGroupDef> {
        let mut groups = Vec::new();
        let Some(groups_node) = root.child(sec_name) else {
            return groups;
        };
        let mut total_weight = 0.0;
        for child in &groups_node.children {
            let is_female = child
                .child("sex")
                .map(|s| s.text().trim() == "female")
                .unwrap_or(false);
            let weight = child.child("weight").and_then(XmlNode::parse_f64).unwrap_or(1.0);
            total_weight += weight;
            groups.push(TankmanGroupDef {
                name: child.name.clone(),
                is_female,
                weight,
                first_names: parse_underscore_ids(child.child("firstNames")),
                last_names: parse_underscore_ids(child.child("lastNames")),
                icons: parse_underscore_ids(child.child("icons")),
            });
        }
        let total_weight = if total_weight > 0.001 { total_weight } else { 1.0 };
        for g in &mut groups {
            g.weight /= total_weight;
        }
        groups
    };

    let normal_groups = parse_groups("normalGroups");
    let premium_groups = parse_groups("premiumGroups");

    Ok(TankmenConfig {
        role_ranks,
        normal_groups,
        premium_groups,
    })
}

fn parse_underscore_ids(section: Option<&XmlNode>) -> Vec<u16> {
    let mut out = Vec::new();
    if let Some(node) = section {
        for child in &node.children {
            if let Some(rest) = child.name.strip_prefix('_') {
                if let Ok(id) = rest.parse::<u16>() {
                    out.push(id);
                }
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_int_comp_descr_encoding_roundtrip() {
        // vehicle: item_type=1, nation=0, item_id=0 (T-34)
        let cd = make_int_comp_descr(1, 0, 0);
        assert_eq!(cd, 1);
        assert_eq!(parse_int_comp_descr(cd), (1, 0, 0));

        // gun: item_type=4, nation=1, item_id=51 (Lowe gun)
        let cd = make_int_comp_descr(4, 1, 51);
        assert_eq!(parse_int_comp_descr(cd), (4, 1, 51));

        // optional device: item_type=9, nation=15, item_id=10
        let cd = make_int_comp_descr(9, NATION_NONE, 10);
        assert_eq!(parse_int_comp_descr(cd), (9, NATION_NONE, 10));

        // shell: item_type=10, nation=2, item_id=200
        let cd = make_int_comp_descr(10, 2, 200);
        assert_eq!(parse_int_comp_descr(cd), (10, 2, 200));

        // equipment: item_type=11, nation=15, item_id=1
        let cd = make_int_comp_descr(11, NATION_NONE, 1);
        assert_eq!(parse_int_comp_descr(cd), (11, NATION_NONE, 1));
    }

    #[test]
    fn test_vehicle_filter_matching_and_rejections() {
        let filter = VehicleFilter {
            exclude: vec![FilterPattern {
                nations: Some(vec![1]), // exclude Germany
                ..Default::default()
            }],
            include: vec![FilterPattern {
                nations: Some(vec![0, 2]), // include USSR and USA
                vehicle: Some(FilterItem {
                    tags: Some(["mediumTank".to_string()].into_iter().collect()),
                    min_level: Some(5),
                    max_level: Some(8),
                }),
                chassis: Some(FilterItem {
                    tags: None,
                    min_level: Some(4),
                    max_level: None,
                }),
                ..Default::default()
            }],
        };

        let empty = BTreeSet::new();
        let med_tag: BTreeSet<String> = ["mediumTank".to_string()].into_iter().collect();
        let heavy_tag: BTreeSet<String> = ["heavyTank".to_string()].into_iter().collect();

        // USSR T-34 (tier 5 medium, chassis level 5) -> should PASS
        let res = filter.check_compatibility(0, &med_tag, 5, &empty, 5, &empty, 5, &empty, 5, &empty, 5);
        assert_eq!(res, Ok(()));

        // Germany tier 5 medium -> matches exclude pattern completely -> "not for current vehicle"
        let res = filter.check_compatibility(1, &med_tag, 5, &empty, 5, &empty, 5, &empty, 5, &empty, 5);
        assert_eq!(res, Err("not for current vehicle"));

        // USSR tier 5 heavy -> rejected by tag in include filter -> "not for this vehicle type"
        let res = filter.check_compatibility(0, &heavy_tag, 5, &empty, 5, &empty, 5, &empty, 5, &empty, 5);
        assert_eq!(res, Err("not for this vehicle type"));

        // USSR tier 4 medium -> rejected by level in include filter -> "not for this vehicle type"
        let res = filter.check_compatibility(0, &med_tag, 4, &empty, 5, &empty, 5, &empty, 5, &empty, 5);
        assert_eq!(res, Err("not for this vehicle type"));

        // USSR tier 5 medium, but chassis tier 2 (< 4) -> chassis level mismatch in include -> "not for current vehicle"
        let res = filter.check_compatibility(0, &med_tag, 5, &empty, 2, &empty, 5, &empty, 5, &empty, 5);
        assert_eq!(res, Err("not for current vehicle"));

        // Exclude pattern where vehicle type matches but module doesn't -> "not for this vehicle type"
        let filter_exclude_mod = VehicleFilter {
            exclude: vec![FilterPattern {
                nations: Some(vec![0]),
                vehicle: Some(FilterItem {
                    tags: Some(["mediumTank".to_string()].into_iter().collect()),
                    min_level: None,
                    max_level: None,
                }),
                chassis: Some(FilterItem {
                    tags: Some(["fastChassis".to_string()].into_iter().collect()),
                    min_level: None,
                    max_level: None,
                }),
                ..Default::default()
            }],
            include: vec![],
        };
        let res = filter_exclude_mod.check_compatibility(0, &med_tag, 5, &empty, 5, &empty, 5, &empty, 5, &empty, 5);
        assert_eq!(res, Err("not for this vehicle type"));
    }

    #[test]
    fn test_synthetic_battle_xml_tags_and_effects() {
        let invis_node = crate::xml::parse_plain(
            br#"<root><invisibility><moving>0.12</moving><still>0.16</still></invisibility></root>"#,
        )
        .expect("parse invisibility xml");
        assert_eq!(
            parse_invisibility(invis_node.child("invisibility")),
            Some((0.12, 0.16))
        );
        assert_eq!(parse_invisibility(None), None);

        let ht_minmax = crate::xml::parse_plain(
            br#"<hitTester>
                <collisionModel>vehicles/ussr/R04_T-34/collision/Hull.model</collisionModel>
                <enclosingBox><min>-1.2 -0.5 -2.4</min><max>1.2 1.8 2.4</max></enclosingBox>
            </hitTester>"#,
        )
        .expect("parse min/max hitTester");
        assert_eq!(
            parse_hit_tester(&ht_minmax),
            Some(HitTesterSpec {
                model: "vehicles/ussr/R04_T-34/collision/Hull.model".to_string(),
                enclosing_box: Some(([-1.2, -0.5, -2.4], [1.2, 1.8, 2.4])),
            })
        );

        let ht_flat = crate::xml::parse_plain(
            br#"<hitTester>
                <collisionModel>vehicles/ussr/turret.model</collisionModel>
                <enclosingBox>-1.0 0.0 -2.0 1.0 1.5 2.0</enclosingBox>
            </hitTester>"#,
        )
        .expect("parse flat enclosingBox");
        assert_eq!(
            parse_hit_tester(&ht_flat),
            Some(HitTesterSpec {
                model: "vehicles/ussr/turret.model".to_string(),
                enclosing_box: Some(([-1.0, 0.0, -2.0], [1.0, 1.5, 2.0])),
            })
        );

        let armor_node = crate::xml::parse_plain(
            br#"<armor><armor_1>50</armor_1><armor_12>30<noDamage>true</noDamage></armor_12></armor>"#,
        )
        .expect("parse armor");
        let zones = parse_armor_zones(&armor_node);
        assert_eq!(zones.get("armor_1"), Some((50.0, false)));
        assert_eq!(zones.get("armor_12"), Some((30.0, true)));

        let burst_node = crate::xml::parse_plain(br#"<burst><count>5</count><rate>120</rate></burst>"#)
            .expect("parse burst");
        assert_eq!(parse_gun_burst(Some(&burst_node)), (5, 0.5));
        assert_eq!(parse_gun_burst(None), (1, 0.0));

        let speed_dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("..")
            .join("target")
            .join("test-tmp")
            .join("item-defs-proj-speed");
        let _ = std::fs::remove_dir_all(&speed_dir);
        std::fs::create_dir_all(&speed_dir).expect("create target/test-tmp dir");
        assert_eq!(read_projectile_speed_factor(&speed_dir), 0.8);
        std::fs::write(
            speed_dir.join("vehicle.xml"),
            "<root><miscParams><projectileSpeedFactor>0.75</projectileSpeedFactor></miscParams></root>",
        )
        .expect("write vehicle.xml");
        assert_eq!(read_projectile_speed_factor(&speed_dir), 0.75);
        let _ = std::fs::remove_dir_all(&speed_dir);

        let opt_other =
            crate::xml::parse_plain(br#"<root><script>UnknownDevice</script></root>"#)
                .expect("parse opt other");
        assert_eq!(
            parse_optional_device_effect(opt_other.child("script").expect("script")),
            OptionalDeviceEffect::Other {
                script: "UnknownDevice".to_string()
            }
        );
        let eq_other = crate::xml::parse_plain(br#"<root><script>UnknownEq</script></root>"#)
            .expect("parse eq other");
        assert_eq!(
            parse_equipment_effect(eq_other.child("script").expect("script")),
            EquipmentEffect::Other {
                script: "UnknownEq".to_string()
            }
        );
        let eq_rpm = crate::xml::parse_plain(
            br#"<root><script>RemovedRpmLimiter<enginePowerFactor>1.1</enginePowerFactor><engineHpLossPerSecond>5</engineHpLossPerSecond></script></root>"#,
        )
        .expect("parse rpm limiter");
        assert_eq!(
            parse_equipment_effect(eq_rpm.child("script").expect("script")),
            EquipmentEffect::RemovedRpmLimiter {
                engine_power_factor: 1.1,
                engine_hp_loss_per_second: 5.0,
            }
        );
    }

    #[test]
    fn test_armor_piercing_he_shell_kind_and_no_splash_radius() {
        let tmp_dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("..")
            .join("target")
            .join("test-tmp")
            .join("aphe-shell-test");
        let _ = std::fs::remove_dir_all(&tmp_dir);
        std::fs::create_dir_all(&tmp_dir).expect("create target/test-tmp dir");
        std::fs::write(
            tmp_dir.join("shells.xml"),
            r#"<root>
  <_122mm_APHE>
    <id>55</id>
    <price>200</price>
    <kind>ARMOR_PIERCING_HE</kind>
    <caliber>122</caliber>
    <isTracer>true</isTracer>
    <effects>largeArmorPiercing</effects>
    <damage><armor>390</armor><devices>165</devices></damage>
  </_122mm_APHE>
</root>"#,
        )
        .expect("write shells.xml");

        let mut extras = ItemCatalogExtras::default();
        let (_by_name, defs) =
            load_nation_shells_with_defs(&tmp_dir, 0, &mut extras).expect("load shells.xml");
        let _ = std::fs::remove_dir_all(&tmp_dir);

        let cd = make_int_comp_descr(10, 0, 55);
        let aphe = defs.get(&cd).expect("APHE shell def");
        assert_eq!(aphe.kind, ShellKind::ArmorPiercingHe);
        assert!(aphe.kind.is_armor_piercing());
        assert_ne!(
            aphe.kind,
            ShellKind::HighExplosive,
            "ARMOR_PIERCING_HE must not parse as HighExplosive"
        );
        assert_eq!(aphe.kind.as_str(), "ARMOR_PIERCING_HE");
        assert_eq!(
            aphe.explosion_radius, 0.0,
            "ARMOR_PIERCING_HE must not receive HE caliber^2/5555 splash radius"
        );
    }
}
