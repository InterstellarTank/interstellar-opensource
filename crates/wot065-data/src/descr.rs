//! Binary compact descriptor codecs and fitting validation for WoT 0.6.5.
//!
//! Implements:
//! - [`IntCompDescr`]: 32-bit integer compact descriptors for items and vehicle types.
//! - [`VehicleDescr`]: 15..=25 byte variable-length vehicle compact descriptors and
//!   weight/component/device compatibility validation matching `common/items/vehicles.py`.
//! - [`TankmanDescr`]: (19 + K + dossier_len) byte tankman compact descriptors, XP/rank
//!   formulas, skill management, respecialization, and passport validation matching
//!   `common/items/tankmen.py`.

use std::collections::BTreeSet;
use std::fmt;

pub use crate::item_defs::{make_int_comp_descr, parse_int_comp_descr};
use crate::item_defs::{
    EquipmentDef, TankmenConfig, VehicleDef, NATION_NONE,
};
use crate::catalog::Catalog;

pub const NUM_OPTIONAL_DEVICE_SLOTS: usize = 3;
pub const VEHICLE_CLASS_TAGS: &[&str] = &["lightTank", "mediumTank", "heavyTank", "SPG", "AT-SPG"];

pub const ITEM_TYPE_VEHICLE: u8 = 1;
pub const ITEM_TYPE_CHASSIS: u8 = 2;
pub const ITEM_TYPE_TURRET: u8 = 3;
pub const ITEM_TYPE_GUN: u8 = 4;
pub const ITEM_TYPE_ENGINE: u8 = 5;
pub const ITEM_TYPE_FUEL_TANK: u8 = 6;
pub const ITEM_TYPE_RADIO: u8 = 7;
pub const ITEM_TYPE_TANKMAN: u8 = 8;
pub const ITEM_TYPE_OPTIONAL_DEVICE: u8 = 9;
pub const ITEM_TYPE_SHELL: u8 = 10;
pub const ITEM_TYPE_EQUIPMENT: u8 = 11;

pub const ROLE_COMMANDER: u8 = 1;
pub const ROLE_RADIOMAN: u8 = 2;
pub const ROLE_DRIVER: u8 = 3;
pub const ROLE_GUNNER: u8 = 4;
pub const ROLE_LOADER: u8 = 5;

pub const SKILL_REPAIR: u8 = 6;
pub const SKILL_FIRE_FIGHTING: u8 = 7;
pub const SKILL_CAMOUFLAGE: u8 = 8;

pub const MAX_SKILL_LEVEL: u8 = 100;
pub const MIN_ROLE_LEVEL: u8 = 50;
pub const SKILL_LEVELS_PER_RANK: u8 = 50;
pub const MAX_FREE_XP: i32 = 2_000_000_000;

/// 32-bit integer compact descriptor wrapper (`[16-bit id][4-bit nation][4-bit type]`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct IntCompDescr(pub i32);

impl IntCompDescr {
    pub fn new(item_type_id: u8, nation_id: u8, item_id: u16) -> Self {
        Self(make_int_comp_descr(item_type_id, nation_id, item_id))
    }

    pub fn parse(self) -> (u8, u8, u16) {
        parse_int_comp_descr(self.0)
    }

    pub fn item_type_id(self) -> u8 {
        (self.0 & 0x0f) as u8
    }

    pub fn nation_id(self) -> u8 {
        ((self.0 >> 4) & 0x0f) as u8
    }

    pub fn item_id(self) -> u16 {
        ((self.0 >> 8) & 0xffff) as u16
    }
}

impl From<i32> for IntCompDescr {
    fn from(cd: i32) -> Self {
        Self(cd)
    }
}

impl From<IntCompDescr> for i32 {
    fn from(icd: IntCompDescr) -> Self {
        icd.0
    }
}

pub fn role_name(role_id: u8) -> Option<&'static str> {
    match role_id {
        1 => Some("commander"),
        2 => Some("radioman"),
        3 => Some("driver"),
        4 => Some("gunner"),
        5 => Some("loader"),
        _ => None,
    }
}

pub fn role_id_by_name(name: &str) -> Option<u8> {
    match name {
        "commander" => Some(1),
        "radioman" => Some(2),
        "driver" => Some(3),
        "gunner" => Some(4),
        "loader" => Some(5),
        _ => None,
    }
}

pub fn skill_id_by_name(name: &str) -> Option<u8> {
    match name {
        "repair" => Some(6),
        "fireFighting" => Some(7),
        "camouflage" => Some(8),
        _ => None,
    }
}

pub fn skill_name_by_id(id: u8) -> Option<&'static str> {
    match id {
        6 => Some("repair"),
        7 => Some("fireFighting"),
        8 => Some("camouflage"),
        _ => None,
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DescrError {
    InvalidLength { expected: usize, got: usize },
    InvalidDescriptor(String),
    Fitting(FittingError),
    CannotAddSkill(String),
    CannotDropSkill(String),
    InvalidPassport,
    NotFound(String),
}

impl fmt::Display for DescrError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidLength { expected, got } => {
                write!(f, "invalid descriptor length: expected {expected}, got {got}")
            }
            Self::InvalidDescriptor(s) => write!(f, "invalid descriptor: {s}"),
            Self::Fitting(e) => write!(f, "fitting error: {e}"),
            Self::CannotAddSkill(s) => write!(f, "cannot add skill: {s}"),
            Self::CannotDropSkill(s) => write!(f, "cannot drop skill: {s}"),
            Self::InvalidPassport => write!(f, "invalid passport"),
            Self::NotFound(s) => write!(f, "not found: {s}"),
        }
    }
}

impl std::error::Error for DescrError {}

impl From<FittingError> for DescrError {
    fn from(e: FittingError) -> Self {
        Self::Fitting(e)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FittingError {
    WrongNation,
    WrongItemType,
    NotForThisVehicleType,
    NotForCurrentVehicle,
    AlreadyInstalled,
    TooHeavy,
    IncompatibleVehicleFilter(String),
    IncompatibleEquipment(String),
    IncompatibleCamouflage(String),
}

impl fmt::Display for FittingError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::WrongNation => write!(f, "wrong nation"),
            Self::WrongItemType => write!(f, "wrong item type"),
            Self::NotForThisVehicleType => write!(f, "not for this vehicle type"),
            Self::NotForCurrentVehicle => write!(f, "not for current vehicle"),
            Self::AlreadyInstalled => write!(f, "already installed"),
            Self::TooHeavy => write!(f, "too heavy"),
            Self::IncompatibleVehicleFilter(s) => write!(f, "incompatible vehicle filter: {s}"),
            Self::IncompatibleEquipment(s) => write!(f, "incompatible equipment: {s}"),
            Self::IncompatibleCamouflage(s) => write!(f, "incompatible camouflage: {s}"),
        }
    }
}

impl std::error::Error for FittingError {}

/// Unpacked variable-length vehicle compact descriptor (`15..=25` bytes).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VehicleDescr {
    pub nation_id: u8,
    pub type_id: u8,
    pub chassis_id: u16,
    pub engine_id: u16,
    pub fuel_tank_id: u16,
    pub radio_id: u16,
    pub turret_id: u16,
    pub gun_id: u16,
    pub optional_devices: [Option<u16>; NUM_OPTIONAL_DEVICE_SLOTS],
    pub custom_emblem_id: Option<u8>,
    pub horn_id: Option<u8>,
    pub camouflage_id: Option<u16>,
}

impl VehicleDescr {
    /// Create a stock descriptor for a given vehicle definition.
    pub fn stock(veh_def: &VehicleDef) -> Result<Self, DescrError> {
        let chassis_id = veh_def
            .chassis
            .first()
            .map(|c| c.id)
            .ok_or_else(|| DescrError::NotFound("chassis".into()))?;
        let engine_id = veh_def
            .engines
            .first()
            .map(|c| c.id)
            .ok_or_else(|| DescrError::NotFound("engine".into()))?;
        let fuel_tank_id = veh_def
            .fuel_tanks
            .first()
            .map(|c| c.id)
            .ok_or_else(|| DescrError::NotFound("fuel_tank".into()))?;
        let radio_id = veh_def
            .radios
            .first()
            .map(|c| c.id)
            .ok_or_else(|| DescrError::NotFound("radio".into()))?;
        let turret = veh_def
            .turrets
            .first()
            .ok_or_else(|| DescrError::NotFound("turret".into()))?;
        let turret_id = turret.component.id;
        let gun_id = turret
            .guns
            .first()
            .map(|g| g.component.id)
            .ok_or_else(|| DescrError::NotFound("gun".into()))?;

        Ok(Self {
            nation_id: veh_def.nation_id,
            type_id: veh_def.type_id as u8,
            chassis_id,
            engine_id,
            fuel_tank_id,
            radio_id,
            turret_id,
            gun_id,
            optional_devices: [None; NUM_OPTIONAL_DEVICE_SLOTS],
            custom_emblem_id: None,
            horn_id: None,
            camouflage_id: None,
        })
    }

    /// Decode a variable-length vehicle compact descriptor (`15..=25` bytes).
    /// Mirrors `VehicleDescr.__initFromCompactDescr` (`common/items/vehicles.py:566-626`).
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, DescrError> {
        if bytes.len() < 15 {
            return Err(DescrError::InvalidLength {
                expected: 15,
                got: bytes.len(),
            });
        }
        let header = bytes[0];
        if header & 0x0f != ITEM_TYPE_VEHICLE {
            return Err(DescrError::InvalidDescriptor(format!(
                "not a vehicle descriptor (item type {})",
                header & 0x0f
            )));
        }
        let nation_id = (header >> 4) & 0x0f;
        let type_id = bytes[1];
        let word = |idx: usize| u16::from_le_bytes([bytes[idx], bytes[idx + 1]]);
        let chassis_id = word(2);
        let engine_id = word(4);
        let fuel_tank_id = word(6);
        let radio_id = word(8);
        let turret_id = word(10);
        let gun_id = word(12);

        let flags = bytes[14];
        let mut pos = 15;

        let mut optional_devices_mask = flags & 0x0f;
        let mut optional_devices = [None; NUM_OPTIONAL_DEVICE_SLOTS];
        let mut n = (NUM_OPTIONAL_DEVICE_SLOTS as isize) - 1;
        while optional_devices_mask != 0 {
            if optional_devices_mask & 1 != 0 {
                if pos + 2 > bytes.len() {
                    return Err(DescrError::InvalidDescriptor(
                        "truncated optional device".into(),
                    ));
                }
                if n >= 0 && (n as usize) < NUM_OPTIONAL_DEVICE_SLOTS {
                    optional_devices[n as usize] =
                        Some(u16::from_le_bytes([bytes[pos], bytes[pos + 1]]));
                }
                pos += 2;
            }
            optional_devices_mask >>= 1;
            n -= 1;
        }

        let custom_emblem_id = if flags & 16 != 0 {
            if pos >= bytes.len() {
                return Err(DescrError::InvalidDescriptor("truncated emblem".into()));
            }
            let id = bytes[pos];
            pos += 1;
            Some(id)
        } else {
            None
        };

        let horn_id = if flags & 64 != 0 {
            if pos >= bytes.len() {
                return Err(DescrError::InvalidDescriptor("truncated horn".into()));
            }
            let id = bytes[pos];
            pos += 1;
            Some(id)
        } else {
            None
        };

        let camouflage_id = if flags & 128 != 0 {
            if pos + 2 > bytes.len() {
                return Err(DescrError::InvalidDescriptor(
                    "truncated camouflage".into(),
                ));
            }
            let id = u16::from_le_bytes([bytes[pos], bytes[pos + 1]]);
            pos += 2;
            Some(id)
        } else {
            None
        };

        if pos != bytes.len() {
            return Err(DescrError::InvalidDescriptor(format!(
                "trailing unexpected {} bytes",
                bytes.len() - pos
            )));
        }

        Ok(Self {
            nation_id,
            type_id,
            chassis_id,
            engine_id,
            fuel_tank_id,
            radio_id,
            turret_id,
            gun_id,
            optional_devices,
            custom_emblem_id,
            horn_id,
            camouflage_id,
        })
    }

    /// Encode to binary compact descriptor (`15..=25` bytes).
    /// Mirrors `VehicleDescr.makeCompactDescr` (`common/items/vehicles.py:345-385`).
    pub fn to_bytes(&self) -> Vec<u8> {
        let header = ITEM_TYPE_VEHICLE | ((self.nation_id & 0x0f) << 4);
        let mut out = Vec::with_capacity(25);
        out.push(header);
        out.push(self.type_id);
        out.extend_from_slice(&self.chassis_id.to_le_bytes());
        out.extend_from_slice(&self.engine_id.to_le_bytes());
        out.extend_from_slice(&self.fuel_tank_id.to_le_bytes());
        out.extend_from_slice(&self.radio_id.to_le_bytes());
        out.extend_from_slice(&self.turret_id.to_le_bytes());
        out.extend_from_slice(&self.gun_id.to_le_bytes());

        let mut opt_mask = 0u8;
        let mut opt_bytes = Vec::new();
        for dev in self.optional_devices {
            opt_mask <<= 1;
            if let Some(id) = dev {
                let mut dev_le = id.to_le_bytes().to_vec();
                dev_le.extend(opt_bytes);
                opt_bytes = dev_le;
                opt_mask |= 1;
            }
        }

        let mut flags = opt_mask;
        if self.custom_emblem_id.is_some() {
            flags |= 16;
        }
        if self.horn_id.is_some() {
            flags |= 64;
        }
        if self.camouflage_id.is_some() {
            flags |= 128;
        }

        out.push(flags);
        out.extend(opt_bytes);

        if let Some(emblem) = self.custom_emblem_id {
            out.push(emblem);
        }
        if let Some(horn) = self.horn_id {
            out.push(horn);
        }
        if let Some(camo) = self.camouflage_id {
            out.extend_from_slice(&camo.to_le_bytes());
        }

        out
    }

    /// Compute total vehicle weight and maximum allowed weight matching `vehicles.py:633-650`.
    pub fn compute_weight(
        &self,
        veh_def: &VehicleDef,
        catalog: &Catalog,
    ) -> Result<(f64, f64), DescrError> {
        let chassis = veh_def
            .chassis
            .iter()
            .find(|c| c.id == self.chassis_id)
            .ok_or_else(|| DescrError::NotFound(format!("chassis id {}", self.chassis_id)))?;
        let engine = veh_def
            .engines
            .iter()
            .find(|e| e.id == self.engine_id)
            .ok_or_else(|| DescrError::NotFound(format!("engine id {}", self.engine_id)))?;
        let fuel_tank = veh_def
            .fuel_tanks
            .iter()
            .find(|f| f.id == self.fuel_tank_id)
            .ok_or_else(|| DescrError::NotFound(format!("fuel_tank id {}", self.fuel_tank_id)))?;
        let radio = veh_def
            .radios
            .iter()
            .find(|r| r.id == self.radio_id)
            .ok_or_else(|| DescrError::NotFound(format!("radio id {}", self.radio_id)))?;
        let turret = veh_def
            .turrets
            .iter()
            .find(|t| t.component.id == self.turret_id)
            .ok_or_else(|| DescrError::NotFound(format!("turret id {}", self.turret_id)))?;
        let gun = turret
            .guns
            .iter()
            .find(|g| g.component.id == self.gun_id)
            .ok_or_else(|| DescrError::NotFound(format!("gun id {}", self.gun_id)))?;

        let base_weight = veh_def.hull_weight
            + chassis.weight
            + engine.weight
            + fuel_tank.weight
            + radio.weight
            + turret.component.weight
            + gun.component.weight;

        let mut max_weight = chassis.max_load.unwrap_or(0.0);
        let mut veh_weight_fraction = 0.0;
        let mut veh_weight_addition = 0.0;

        for opt_id_opt in self.optional_devices {
            if let Some(opt_id) = opt_id_opt {
                let opt_cd = make_int_comp_descr(
                    ITEM_TYPE_OPTIONAL_DEVICE,
                    NATION_NONE,
                    opt_id,
                );
                let opt_def = catalog
                    .optional_device(opt_cd)
                    .ok_or_else(|| DescrError::NotFound(format!("optional device id {opt_id}")))?;
                veh_weight_fraction += opt_def.veh_weight_fraction;
                veh_weight_addition += opt_def.weight;
                if opt_def.chassis_max_load_factor > 0.0 {
                    max_weight +=
                        chassis.max_load.unwrap_or(0.0) * (opt_def.chassis_max_load_factor - 1.0);
                }
            }
        }

        let total_weight = base_weight * (1.0 + veh_weight_fraction) + veh_weight_addition;
        Ok((total_weight, max_weight))
    }

    /// Check if any currently mounted optional device is incompatible with current configuration.
    fn have_incompatible_optional_devices(&self, veh_def: &VehicleDef, catalog: &Catalog) -> bool {
        let chassis = match veh_def.chassis.iter().find(|c| c.id == self.chassis_id) {
            Some(c) => c,
            None => return true,
        };
        let engine = match veh_def.engines.iter().find(|e| e.id == self.engine_id) {
            Some(e) => e,
            None => return true,
        };
        let fuel_tank = match veh_def.fuel_tanks.iter().find(|f| f.id == self.fuel_tank_id) {
            Some(f) => f,
            None => return true,
        };
        let radio = match veh_def.radios.iter().find(|r| r.id == self.radio_id) {
            Some(r) => r,
            None => return true,
        };

        for opt_id_opt in self.optional_devices {
            if let Some(opt_id) = opt_id_opt {
                let opt_cd = make_int_comp_descr(
                    ITEM_TYPE_OPTIONAL_DEVICE,
                    NATION_NONE,
                    opt_id,
                );
                let opt_def = match catalog.optional_device(opt_cd) {
                    Some(d) => d,
                    None => return true,
                };
                let res = opt_def.vehicle_filter.check_compatibility(
                    self.nation_id,
                    &veh_def.tags,
                    veh_def.level,
                    &chassis.tags,
                    chassis.level,
                    &engine.tags,
                    engine.level,
                    &fuel_tank.tags,
                    fuel_tank.level,
                    &radio.tags,
                    radio.level,
                );
                if res.is_err() {
                    return true;
                }
            }
        }
        false
    }

    /// Check whether a component can be installed on this vehicle (`vehicles.py:204-249`).
    pub fn may_install_component(
        &self,
        comp_cd: i32,
        veh_def: &VehicleDef,
        catalog: &Catalog,
    ) -> Result<(), FittingError> {
        let (item_type_id, nation_id, comp_id) = parse_int_comp_descr(comp_cd);
        if nation_id != self.nation_id {
            return Err(FittingError::WrongNation);
        }
        let mut temp = self.clone();
        match item_type_id {
            ITEM_TYPE_GUN => {
                let turret = veh_def
                    .turrets
                    .iter()
                    .find(|t| t.component.id == self.turret_id)
                    .ok_or(FittingError::NotForThisVehicleType)?;
                if !turret.guns.iter().any(|g| g.component.id == comp_id) {
                    if !veh_def.installable_components.contains(&comp_cd) {
                        return Err(FittingError::NotForThisVehicleType);
                    }
                    return Err(FittingError::NotForCurrentVehicle);
                }
                temp.gun_id = comp_id;
            }
            ITEM_TYPE_CHASSIS => {
                if !veh_def.chassis.iter().any(|c| c.id == comp_id) {
                    if !veh_def.installable_components.contains(&comp_cd) {
                        return Err(FittingError::NotForThisVehicleType);
                    }
                    return Err(FittingError::NotForCurrentVehicle);
                }
                temp.chassis_id = comp_id;
            }
            ITEM_TYPE_ENGINE => {
                if !veh_def.engines.iter().any(|e| e.id == comp_id) {
                    if !veh_def.installable_components.contains(&comp_cd) {
                        return Err(FittingError::NotForThisVehicleType);
                    }
                    return Err(FittingError::NotForCurrentVehicle);
                }
                temp.engine_id = comp_id;
            }
            ITEM_TYPE_RADIO => {
                if !veh_def.radios.iter().any(|r| r.id == comp_id) {
                    if !veh_def.installable_components.contains(&comp_cd) {
                        return Err(FittingError::NotForThisVehicleType);
                    }
                    return Err(FittingError::NotForCurrentVehicle);
                }
                temp.radio_id = comp_id;
            }
            ITEM_TYPE_FUEL_TANK => {
                if !veh_def.fuel_tanks.iter().any(|f| f.id == comp_id) {
                    if !veh_def.installable_components.contains(&comp_cd) {
                        return Err(FittingError::NotForThisVehicleType);
                    }
                    return Err(FittingError::NotForCurrentVehicle);
                }
                temp.fuel_tank_id = comp_id;
            }
            _ => return Err(FittingError::WrongItemType),
        }

        if temp.have_incompatible_optional_devices(veh_def, catalog) {
            return Err(FittingError::NotForCurrentVehicle);
        }
        let (w, max_w) = temp
            .compute_weight(veh_def, catalog)
            .map_err(|_| FittingError::NotForCurrentVehicle)?;
        if w > max_w {
            return Err(FittingError::TooHeavy);
        }
        Ok(())
    }

    /// Install a component, returning the previous component's compact descriptor.
    pub fn install_component(
        &mut self,
        comp_cd: i32,
        veh_def: &VehicleDef,
        catalog: &Catalog,
    ) -> Result<i32, FittingError> {
        self.may_install_component(comp_cd, veh_def, catalog)?;
        let (item_type_id, _, comp_id) = parse_int_comp_descr(comp_cd);
        let prev_cd = match item_type_id {
            ITEM_TYPE_GUN => {
                let prev = make_int_comp_descr(item_type_id, self.nation_id, self.gun_id);
                self.gun_id = comp_id;
                prev
            }
            ITEM_TYPE_CHASSIS => {
                let prev = make_int_comp_descr(item_type_id, self.nation_id, self.chassis_id);
                self.chassis_id = comp_id;
                prev
            }
            ITEM_TYPE_ENGINE => {
                let prev = make_int_comp_descr(item_type_id, self.nation_id, self.engine_id);
                self.engine_id = comp_id;
                prev
            }
            ITEM_TYPE_RADIO => {
                let prev = make_int_comp_descr(item_type_id, self.nation_id, self.radio_id);
                self.radio_id = comp_id;
                prev
            }
            ITEM_TYPE_FUEL_TANK => {
                let prev = make_int_comp_descr(item_type_id, self.nation_id, self.fuel_tank_id);
                self.fuel_tank_id = comp_id;
                prev
            }
            _ => return Err(FittingError::WrongItemType),
        };
        Ok(prev_cd)
    }

    /// Check whether a turret and gun combination can be installed (`vehicles.py:148-184`).
    pub fn may_install_turret(
        &self,
        turret_cd: i32,
        gun_cd: i32,
        veh_def: &VehicleDef,
        catalog: &Catalog,
    ) -> Result<(), FittingError> {
        let (t_item_type, t_nation, turret_id) = parse_int_comp_descr(turret_cd);
        if t_item_type != ITEM_TYPE_TURRET {
            return Err(FittingError::WrongItemType);
        }
        if t_nation != self.nation_id {
            return Err(FittingError::WrongNation);
        }
        let gun_id = if gun_cd == 0 {
            self.gun_id
        } else {
            let (g_item_type, g_nation, gid) = parse_int_comp_descr(gun_cd);
            if g_item_type != ITEM_TYPE_GUN {
                return Err(FittingError::WrongItemType);
            }
            if g_nation != self.nation_id {
                return Err(FittingError::WrongNation);
            }
            gid
        };

        let turret_def = veh_def
            .turrets
            .iter()
            .find(|t| t.component.id == turret_id)
            .ok_or(FittingError::NotForThisVehicleType)?;
        if !turret_def.guns.iter().any(|g| g.component.id == gun_id) {
            if gun_cd != 0 && !veh_def.installable_components.contains(&gun_cd) {
                return Err(FittingError::NotForThisVehicleType);
            }
            return Err(FittingError::NotForCurrentVehicle);
        }

        let mut temp = self.clone();
        temp.turret_id = turret_id;
        temp.gun_id = gun_id;

        if temp.have_incompatible_optional_devices(veh_def, catalog) {
            return Err(FittingError::NotForCurrentVehicle);
        }
        let (w, max_w) = temp
            .compute_weight(veh_def, catalog)
            .map_err(|_| FittingError::NotForCurrentVehicle)?;
        if w > max_w {
            return Err(FittingError::TooHeavy);
        }
        Ok(())
    }

    /// Install a turret and gun combination, returning removed component descriptors (`vehicles.py:185-202`).
    pub fn install_turret(
        &mut self,
        turret_cd: i32,
        gun_cd: i32,
        veh_def: &VehicleDef,
        catalog: &Catalog,
    ) -> Result<Vec<i32>, FittingError> {
        self.may_install_turret(turret_cd, gun_cd, veh_def, catalog)?;
        let (_, _, new_turret_id) = parse_int_comp_descr(turret_cd);
        let new_gun_id = if gun_cd == 0 {
            self.gun_id
        } else {
            let (_, _, gid) = parse_int_comp_descr(gun_cd);
            gid
        };
        let prev_turret_cd =
            make_int_comp_descr(ITEM_TYPE_TURRET, self.nation_id, self.turret_id);
        let prev_gun_cd =
            make_int_comp_descr(ITEM_TYPE_GUN, self.nation_id, self.gun_id);

        self.turret_id = new_turret_id;
        self.gun_id = new_gun_id;

        let mut removed = vec![prev_turret_cd];
        if gun_cd != 0 && prev_gun_cd != gun_cd {
            removed.push(prev_gun_cd);
        }
        Ok(removed)
    }

    /// Check whether an optional device can be installed in a slot (`vehicles.py:278-304`).
    pub fn may_install_optional_device(
        &self,
        opt_dev_cd: i32,
        slot_idx: usize,
        veh_def: &VehicleDef,
        catalog: &Catalog,
    ) -> Result<(), FittingError> {
        if slot_idx >= NUM_OPTIONAL_DEVICE_SLOTS {
            return Err(FittingError::NotForCurrentVehicle);
        }
        let (item_type_id, _, dev_id) = parse_int_comp_descr(opt_dev_cd);
        if item_type_id != ITEM_TYPE_OPTIONAL_DEVICE {
            return Err(FittingError::WrongItemType);
        }
        let opt_def = catalog
            .optional_device(opt_dev_cd)
            .ok_or(FittingError::NotForThisVehicleType)?;

        for (i, dev) in self.optional_devices.iter().enumerate() {
            if *dev == Some(dev_id) && i != slot_idx {
                return Err(FittingError::AlreadyInstalled);
            }
        }

        let mut temp = self.clone();
        temp.optional_devices[slot_idx] = None;

        let chassis = veh_def
            .chassis
            .iter()
            .find(|c| c.id == temp.chassis_id)
            .ok_or(FittingError::NotForCurrentVehicle)?;
        let engine = veh_def
            .engines
            .iter()
            .find(|e| e.id == temp.engine_id)
            .ok_or(FittingError::NotForCurrentVehicle)?;
        let fuel_tank = veh_def
            .fuel_tanks
            .iter()
            .find(|f| f.id == temp.fuel_tank_id)
            .ok_or(FittingError::NotForCurrentVehicle)?;
        let radio = veh_def
            .radios
            .iter()
            .find(|r| r.id == temp.radio_id)
            .ok_or(FittingError::NotForCurrentVehicle)?;

        opt_def
            .vehicle_filter
            .check_compatibility(
                temp.nation_id,
                &veh_def.tags,
                veh_def.level,
                &chassis.tags,
                chassis.level,
                &engine.tags,
                engine.level,
                &fuel_tank.tags,
                fuel_tank.level,
                &radio.tags,
                radio.level,
            )
            .map_err(|s| FittingError::IncompatibleVehicleFilter(s.to_string()))?;

        temp.optional_devices[slot_idx] = Some(dev_id);

        if temp.have_incompatible_optional_devices(veh_def, catalog) {
            return Err(FittingError::NotForCurrentVehicle);
        }
        let (w, max_w) = temp
            .compute_weight(veh_def, catalog)
            .map_err(|_| FittingError::NotForCurrentVehicle)?;
        if w > max_w {
            return Err(FittingError::TooHeavy);
        }
        Ok(())
    }

    /// Install an optional device in a slot, returning replaced device CD if any (`vehicles.py:305-316`).
    pub fn install_optional_device(
        &mut self,
        opt_dev_cd: i32,
        slot_idx: usize,
        veh_def: &VehicleDef,
        catalog: &Catalog,
    ) -> Result<Option<i32>, FittingError> {
        self.may_install_optional_device(opt_dev_cd, slot_idx, veh_def, catalog)?;
        let (_, _, dev_id) = parse_int_comp_descr(opt_dev_cd);
        let prev_opt_id = self.optional_devices[slot_idx];
        self.optional_devices[slot_idx] = Some(dev_id);
        let prev_cd = prev_opt_id.map(|id| {
            make_int_comp_descr(
                ITEM_TYPE_OPTIONAL_DEVICE,
                NATION_NONE,
                id,
            )
        });
        Ok(prev_cd)
    }

    /// Check whether an optional device can be removed from a slot (`vehicles.py:318-333`).
    pub fn may_remove_optional_device(
        &self,
        slot_idx: usize,
        veh_def: &VehicleDef,
        catalog: &Catalog,
    ) -> Result<(), FittingError> {
        if slot_idx >= NUM_OPTIONAL_DEVICE_SLOTS {
            return Err(FittingError::NotForCurrentVehicle);
        }
        let mut temp = self.clone();
        temp.optional_devices[slot_idx] = None;
        if temp.have_incompatible_optional_devices(veh_def, catalog) {
            return Err(FittingError::NotForCurrentVehicle);
        }
        let (w, max_w) = temp
            .compute_weight(veh_def, catalog)
            .map_err(|_| FittingError::NotForCurrentVehicle)?;
        if w > max_w {
            return Err(FittingError::TooHeavy);
        }
        Ok(())
    }

    /// Remove an optional device from a slot, returning removed device CD if any (`vehicles.py:334-344`).
    pub fn remove_optional_device(
        &mut self,
        slot_idx: usize,
        veh_def: &VehicleDef,
        catalog: &Catalog,
    ) -> Result<Option<i32>, FittingError> {
        self.may_remove_optional_device(slot_idx, veh_def, catalog)?;
        let prev_opt_id = self.optional_devices[slot_idx];
        self.optional_devices[slot_idx] = None;
        let prev_cd = prev_opt_id.map(|id| {
            make_int_comp_descr(
                ITEM_TYPE_OPTIONAL_DEVICE,
                NATION_NONE,
                id,
            )
        });
        Ok(prev_cd)
    }

    /// Collect default, installed, and optional device descriptors matching `vehicles.py:414-438`.
    pub fn get_devices(&self, veh_def: &VehicleDef) -> (Vec<i32>, Vec<i32>, Vec<i32>) {
        let mut def_comps = Vec::with_capacity(6);
        let mut inst_comps = Vec::with_capacity(6);

        // 1. chassis
        inst_comps.push(make_int_comp_descr(
            ITEM_TYPE_CHASSIS,
            self.nation_id,
            self.chassis_id,
        ));
        def_comps.push(veh_def.chassis[0].comp_descr);

        // 2. engine
        inst_comps.push(make_int_comp_descr(
            ITEM_TYPE_ENGINE,
            self.nation_id,
            self.engine_id,
        ));
        def_comps.push(veh_def.engines[0].comp_descr);

        // 3. fuelTank
        inst_comps.push(make_int_comp_descr(
            ITEM_TYPE_FUEL_TANK,
            self.nation_id,
            self.fuel_tank_id,
        ));
        def_comps.push(veh_def.fuel_tanks[0].comp_descr);

        // 4. radio
        inst_comps.push(make_int_comp_descr(
            ITEM_TYPE_RADIO,
            self.nation_id,
            self.radio_id,
        ));
        def_comps.push(veh_def.radios[0].comp_descr);

        // 5. turret
        inst_comps.push(make_int_comp_descr(
            ITEM_TYPE_TURRET,
            self.nation_id,
            self.turret_id,
        ));
        def_comps.push(veh_def.turrets[0].component.comp_descr);

        // 6. gun (installed turret's stock gun vs installed gun)
        let installed_turret = veh_def
            .turrets
            .iter()
            .find(|t| t.component.id == self.turret_id)
            .unwrap_or(&veh_def.turrets[0]);
        inst_comps.push(make_int_comp_descr(
            ITEM_TYPE_GUN,
            self.nation_id,
            self.gun_id,
        ));
        def_comps.push(installed_turret.guns[0].component.comp_descr);

        let mut opt_devices = Vec::new();
        for dev in self.optional_devices {
            if let Some(id) = dev {
                opt_devices.push(make_int_comp_descr(
                    ITEM_TYPE_OPTIONAL_DEVICE,
                    NATION_NONE,
                    id,
                ));
            }
        }

        (def_comps, inst_comps, opt_devices)
    }
}

/// Validate 3-slot equipment loadout matching `common/items/artefacts.py:166-236`.
pub fn check_equipments(
    eq_cds: &[i32; 3],
    veh_descr: &VehicleDescr,
    veh_def: &VehicleDef,
    catalog: &Catalog,
) -> Result<(), FittingError> {
    let chassis = veh_def
        .chassis
        .iter()
        .find(|c| c.id == veh_descr.chassis_id)
        .ok_or(FittingError::NotForCurrentVehicle)?;
    let engine = veh_def
        .engines
        .iter()
        .find(|e| e.id == veh_descr.engine_id)
        .ok_or(FittingError::NotForCurrentVehicle)?;
    let fuel_tank = veh_def
        .fuel_tanks
        .iter()
        .find(|f| f.id == veh_descr.fuel_tank_id)
        .ok_or(FittingError::NotForCurrentVehicle)?;
    let radio = veh_def
        .radios
        .iter()
        .find(|r| r.id == veh_descr.radio_id)
        .ok_or(FittingError::NotForCurrentVehicle)?;

    let mut seen = BTreeSet::new();
    let mut defs: Vec<&EquipmentDef> = Vec::with_capacity(3);

    for &cd in eq_cds {
        if cd == 0 {
            continue;
        }
        if !seen.insert(cd) {
            return Err(FittingError::IncompatibleEquipment(format!(
                "duplicate equipment {cd}"
            )));
        }
        let eq = catalog
            .equipment(cd)
            .ok_or_else(|| FittingError::IncompatibleEquipment(format!("unknown equipment {cd}")))?;
        eq.vehicle_filter
            .check_compatibility(
                veh_descr.nation_id,
                &veh_def.tags,
                veh_def.level,
                &chassis.tags,
                chassis.level,
                &engine.tags,
                engine.level,
                &fuel_tank.tags,
                fuel_tank.level,
                &radio.tags,
                radio.level,
            )
            .map_err(|s| FittingError::IncompatibleEquipment(s.to_string()))?;
        defs.push(eq);
    }

    // Pairwise compatibility
    for i in 0..defs.len() {
        for j in (i + 1)..defs.len() {
            let a = defs[i];
            let b = defs[j];
            if !a.incompatible_installed.is_disjoint(&b.tags)
                || !b.incompatible_installed.is_disjoint(&a.tags)
            {
                return Err(FittingError::IncompatibleEquipment(format!(
                    "{} incompatible with {}",
                    a.name, b.name
                )));
            }
        }
    }
    Ok(())
}

/// Validate camouflage suitability matching `vehicles.py:98-106`.
pub fn check_camouflage(
    camo_id: u16,
    nation_id: u8,
    veh_def: &VehicleDef,
    catalog: &Catalog,
) -> Result<(), FittingError> {
    if camo_id == 0 {
        return Ok(());
    }
    let camo = catalog
        .camouflage(nation_id, camo_id)
        .ok_or_else(|| FittingError::IncompatibleCamouflage(format!("camouflage {camo_id} not found")))?;
    let veh_cd = veh_def.veh_type_comp_descr;
    if camo.deny.contains(&veh_cd) {
        return Err(FittingError::IncompatibleCamouflage(format!(
            "camouflage {camo_id} denied for vehicle {veh_cd}"
        )));
    }
    if !camo.allow.is_empty() && !camo.allow.contains(&veh_cd) {
        return Err(FittingError::IncompatibleCamouflage(format!(
            "camouflage {camo_id} not allowed for vehicle {veh_cd}"
        )));
    }
    Ok(())
}

/// Unpacked tankman compact descriptor (`19 + num_skills + dossier_len` bytes).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TankmanDescr {
    pub nation_id: u8,
    pub vehicle_type_id: u8,
    pub role_id: u8,
    pub role_level: u8,
    pub skills: Vec<String>,
    pub last_skill_level: u8,
    pub is_female: bool,
    pub is_premium: bool,
    pub first_name_id: u16,
    pub last_name_id: u16,
    pub icon_id: u16,
    pub rank_idx: u8,
    pub num_levels_to_next_rank: u8,
    pub free_xp: i32,
    pub dossier: Vec<u8>,
}

/// Calculate the XP required to advance one level matching `tankmen.py:216-218`.
pub fn level_up_xp_cost(from_skill_level: u8, skill_seq_num: usize) -> i32 {
    let base_cost = 50.0 * 100.0f64.powf(from_skill_level as f64 / 100.0);
    (2.0f64.powi(skill_seq_num as i32) * base_cost).round() as i32
}

impl TankmanDescr {
    /// Decode a tankman compact descriptor (`19 + num_skills + dossier_len` bytes).
    /// Mirrors `TankmanDescr.__initFromCompactDescr` (`common/items/tankmen.py:324-370`).
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, DescrError> {
        if bytes.len() < 19 {
            return Err(DescrError::InvalidLength {
                expected: 19,
                got: bytes.len(),
            });
        }
        let header = bytes[0];
        if header & 0x0f != ITEM_TYPE_TANKMAN {
            return Err(DescrError::InvalidDescriptor(format!(
                "not a tankman descriptor (item type {})",
                header & 0x0f
            )));
        }
        let nation_id = (header >> 4) & 0x0f;
        let vehicle_type_id = bytes[1];
        let role_id = bytes[2];
        if !(1..=5).contains(&role_id) {
            return Err(DescrError::InvalidDescriptor(format!(
                "invalid role id {role_id}"
            )));
        }
        let role_level = bytes[3];
        let num_skills = bytes[4] as usize;
        if bytes.len() < 19 + num_skills {
            return Err(DescrError::InvalidLength {
                expected: 19 + num_skills,
                got: bytes.len(),
            });
        }

        let mut skills = Vec::with_capacity(num_skills);
        for i in 0..num_skills {
            let sid = bytes[5 + i];
            let sname = skill_name_by_id(sid)
                .ok_or_else(|| DescrError::InvalidDescriptor(format!("unknown skill id {sid}")))?;
            skills.push(sname.to_string());
        }
        let last_skill_level = bytes[5 + num_skills];

        let tail = 6 + num_skills;
        let flags = bytes[tail];
        let is_female = (flags & 1) != 0;
        let is_premium = (flags & 2) != 0;
        let first_name_id = u16::from_le_bytes([bytes[tail + 1], bytes[tail + 2]]);
        let last_name_id = u16::from_le_bytes([bytes[tail + 3], bytes[tail + 4]]);
        let icon_id = u16::from_le_bytes([bytes[tail + 5], bytes[tail + 6]]);
        let rank_idx = bytes[tail + 7];
        let num_levels_to_next_rank = bytes[tail + 8];
        let free_xp = i32::from_le_bytes([
            bytes[tail + 9],
            bytes[tail + 10],
            bytes[tail + 11],
            bytes[tail + 12],
        ]);
        let dossier = bytes[tail + 13..].to_vec();

        Ok(Self {
            nation_id,
            vehicle_type_id,
            role_id,
            role_level,
            skills,
            last_skill_level,
            is_female,
            is_premium,
            first_name_id,
            last_name_id,
            icon_id,
            rank_idx,
            num_levels_to_next_rank,
            free_xp,
            dossier,
        })
    }

    /// Encode to binary tankman descriptor (`19 + num_skills + dossier_len` bytes).
    /// Mirrors `TankmanDescr.makeCompactDescr` (`common/items/tankmen.py:309-322`).
    pub fn to_bytes(&self) -> Vec<u8> {
        let header = ITEM_TYPE_TANKMAN | ((self.nation_id & 0x0f) << 4);
        let mut out = Vec::with_capacity(19 + self.skills.len() + self.dossier.len());
        out.push(header);
        out.push(self.vehicle_type_id);
        out.push(self.role_id);
        out.push(self.role_level);
        out.push(self.skills.len() as u8);
        for s in &self.skills {
            let sid = skill_id_by_name(s).unwrap_or(0);
            out.push(sid);
        }
        out.push(if self.skills.is_empty() {
            0
        } else {
            self.last_skill_level
        });
        let flags = (self.is_female as u8) | ((self.is_premium as u8) << 1);
        out.push(flags);
        out.extend_from_slice(&self.first_name_id.to_le_bytes());
        out.extend_from_slice(&self.last_name_id.to_le_bytes());
        out.extend_from_slice(&self.icon_id.to_le_bytes());
        out.push(self.rank_idx);
        out.push(self.num_levels_to_next_rank);
        out.extend_from_slice(&self.free_xp.to_le_bytes());
        out.extend_from_slice(&self.dossier);
        out
    }

    /// Internal rank update helper matching `tankmen.py:382-394`.
    fn update_rank_at_skill_level_up(&mut self, mut num_levels: usize, role_ranks_len: usize) {
        if (num_levels as u8) < self.num_levels_to_next_rank {
            self.num_levels_to_next_rank -= num_levels as u8;
            return;
        }
        let max_rank_idx = if role_ranks_len > 0 {
            (role_ranks_len - 1) as u8
        } else {
            0
        };
        while self.num_levels_to_next_rank > 0
            && num_levels >= (self.num_levels_to_next_rank as usize)
        {
            num_levels -= self.num_levels_to_next_rank as usize;
            self.rank_idx = (self.rank_idx + 1).min(max_rank_idx);
            self.num_levels_to_next_rank = if self.rank_idx < max_rank_idx {
                SKILL_LEVELS_PER_RANK
            } else {
                0
            };
        }
        if num_levels > 0 && self.num_levels_to_next_rank > 0 {
            self.num_levels_to_next_rank =
                self.num_levels_to_next_rank.saturating_sub(num_levels as u8);
        }
    }

    /// Add XP to tankman, leveling up role level or last skill matching `tankmen.py:220-232`.
    pub fn add_xp(&mut self, xp: i32, role_ranks_len: usize) {
        self.free_xp = self.free_xp.saturating_add(xp).min(MAX_FREE_XP);
        while self.role_level < MAX_SKILL_LEVEL {
            let cost = level_up_xp_cost(self.role_level, 0);
            if cost > self.free_xp {
                break;
            }
            self.free_xp -= cost;
            self.role_level += 1;
            self.update_rank_at_skill_level_up(1, role_ranks_len);
        }
        if self.role_level == MAX_SKILL_LEVEL && !self.skills.is_empty() {
            while self.last_skill_level < MAX_SKILL_LEVEL {
                let cost = level_up_xp_cost(self.last_skill_level, self.skills.len());
                if cost > self.free_xp {
                    break;
                }
                self.free_xp -= cost;
                self.last_skill_level += 1;
                self.update_rank_at_skill_level_up(1, role_ranks_len);
            }
        }
    }

    /// Add a skill to tankman and consume free XP automatically matching `tankmen.py:234-246`.
    pub fn add_skill(
        &mut self,
        skill_name: &str,
        role_ranks_len: usize,
    ) -> Result<(), DescrError> {
        if self.skills.iter().any(|s| s == skill_name) {
            return Err(DescrError::CannotAddSkill("skill already added".into()));
        }
        if !matches!(skill_name, "repair" | "fireFighting" | "camouflage") {
            return Err(DescrError::CannotAddSkill(format!(
                "invalid skill name {skill_name}"
            )));
        }
        if self.role_level != MAX_SKILL_LEVEL {
            return Err(DescrError::CannotAddSkill("roleLevel must be 100".into()));
        }
        if !self.skills.is_empty() && self.last_skill_level != MAX_SKILL_LEVEL {
            return Err(DescrError::CannotAddSkill(
                "lastSkillLevel must be 100".into(),
            ));
        }
        self.skills.push(skill_name.to_string());
        self.last_skill_level = 0;
        while self.last_skill_level < MAX_SKILL_LEVEL {
            let cost = level_up_xp_cost(self.last_skill_level, self.skills.len());
            if cost > self.free_xp {
                break;
            }
            self.free_xp -= cost;
            self.last_skill_level += 1;
            self.update_rank_at_skill_level_up(1, role_ranks_len);
        }
        Ok(())
    }

    /// Drop a skill from tankman matching `tankmen.py:248-256`.
    pub fn drop_skill(&mut self, skill_name: &str) -> Result<(), DescrError> {
        let idx = self
            .skills
            .iter()
            .position(|s| s == skill_name)
            .ok_or_else(|| DescrError::CannotDropSkill(format!("skill {skill_name} not found")))?;
        let num_skills = self.skills.len();
        if num_skills == 1 {
            self.last_skill_level = 0;
        } else if idx + 1 == num_skills {
            self.last_skill_level = MAX_SKILL_LEVEL;
        }
        self.skills.remove(idx);
        Ok(())
    }

    /// Respecialize tankman for a new vehicle matching `tankmen.py:258-280`.
    pub fn respecialize(
        &mut self,
        new_vehicle_type_id: u8,
        min_new_role_level: u8,
        vehicle_change_role_level_loss: f64,
        class_change_role_level_loss: f64,
        becomes_premium: bool,
        cur_veh_tags: &BTreeSet<String>,
        new_veh_tags: &BTreeSet<String>,
        role_ranks_len: usize,
    ) {
        let mut role_level_loss = if new_vehicle_type_id == self.vehicle_type_id {
            0.0
        } else {
            vehicle_change_role_level_loss
        };
        let is_same_class = VEHICLE_CLASS_TAGS
            .iter()
            .any(|&c| cur_veh_tags.contains(c) && new_veh_tags.contains(c));
        if !is_same_class {
            role_level_loss += class_change_role_level_loss;
        }
        let mut new_role_level = (self.role_level as f64 * (1.0 - role_level_loss)).round() as u8;
        new_role_level = new_role_level.max(min_new_role_level).min(MAX_SKILL_LEVEL);
        self.vehicle_type_id = new_vehicle_type_id;
        if becomes_premium {
            self.is_premium = true;
        }
        if new_role_level > self.role_level {
            let gain = (new_role_level - self.role_level) as usize;
            self.update_rank_at_skill_level_up(gain, role_ranks_len);
            self.role_level = new_role_level;
            self.free_xp = 0;
        } else if new_role_level < self.role_level {
            let loss = self.role_level - new_role_level;
            self.num_levels_to_next_rank = self.num_levels_to_next_rank.saturating_add(loss);
            self.role_level = new_role_level;
            self.add_xp(0, role_ranks_len);
        }
    }

    /// Replace passport matching `tankmen.py:282-307`.
    pub fn replace_passport(
        &mut self,
        is_female: Option<bool>,
        first_name_id: Option<u16>,
        last_name_id: Option<u16>,
        icon_id: Option<u16>,
        config: &TankmenConfig,
    ) -> Result<(), DescrError> {
        let target_female = is_female.unwrap_or(self.is_female);
        let target_first = first_name_id.unwrap_or(self.first_name_id);
        let target_last = last_name_id.unwrap_or(self.last_name_id);
        let target_icon = icon_id.unwrap_or(self.icon_id);

        for group in config.normal_groups.iter().chain(config.premium_groups.iter()) {
            if group.is_female != target_female {
                continue;
            }
            if !group.first_names.contains(&target_first) {
                continue;
            }
            if group.last_names.contains(&target_last) && group.icons.contains(&target_icon) {
                self.is_female = target_female;
                self.first_name_id = target_first;
                self.last_name_id = target_last;
                self.icon_id = target_icon;
                return Ok(());
            }
            break;
        }
        Err(DescrError::InvalidPassport)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hex(bytes: &[u8]) -> String {
        let mut s = String::with_capacity(bytes.len() * 2);
        for b in bytes {
            s.push_str(&format!("{b:02x}"));
        }
        s
    }

    #[test]
    fn test_int_comp_descr_helpers() {
        let cd = IntCompDescr::new(1, 0, 13);
        assert_eq!(cd.item_type_id(), 1);
        assert_eq!(cd.nation_id(), 0);
        assert_eq!(cd.item_id(), 13);
        assert_eq!(cd.parse(), (1, 0, 13));
        assert_eq!(i32::from(cd), 3329);
        assert_eq!(IntCompDescr::from(3329), cd);
    }

    #[test]
    fn test_vehicle_descr_stock_15b_matches_golden_and_roundtrips() {
        let golden_hexes = [
            // (nation, type_id, chassis, engine, fuel, radio, turret, gun, expected_hex)
            (0, 0, 0, 0, 203, 3, 0, 0, "010000000000cb0003000000000000"), // T-34
            (1, 0, 0, 0, 203, 2, 0, 0, "110000000000cb0002000000000000"), // PzIV
            (2, 2, 3, 4, 201, 3, 3, 4, "210203000400c90003000300040000"), // T1 Cunningham
            (0, 13, 26, 14, 201, 0, 23, 23, "010d1a000e00c90000001700170000"), // MS-1
            (1, 212, 85, 40, 205, 16, 67, 51, "11d455002800cd0010004300330000"), // Löwe
        ];

        for (nation, type_id, ch, eng, fuel, rad, tur, gun, exp_hex) in golden_hexes {
            let descr = VehicleDescr {
                nation_id: nation,
                type_id,
                chassis_id: ch,
                engine_id: eng,
                fuel_tank_id: fuel,
                radio_id: rad,
                turret_id: tur,
                gun_id: gun,
                optional_devices: [None, None, None],
                custom_emblem_id: None,
                horn_id: None,
                camouflage_id: None,
            };
            let bytes = descr.to_bytes();
            assert_eq!(bytes.len(), 15);
            assert_eq!(hex(&bytes), exp_hex);

            let unpacked = VehicleDescr::from_bytes(&bytes).expect("unpack stock descr");
            assert_eq!(unpacked, descr);
        }
    }

    #[test]
    fn test_vehicle_descr_optional_devices_wire_order() {
        let base = VehicleDescr {
            nation_id: 0,
            type_id: 13,
            chassis_id: 26,
            engine_id: 14,
            fuel_tank_id: 201,
            radio_id: 0,
            turret_id: 23,
            gun_id: 23,
            optional_devices: [None, None, None],
            custom_emblem_id: None,
            horn_id: None,
            camouflage_id: None,
        };

        // 1. Slot 0 only (opt_id = 0x0111) -> mask = 0x04 (bit 2)
        let mut d_slot0 = base.clone();
        d_slot0.optional_devices[0] = Some(0x0111);
        let b0 = d_slot0.to_bytes();
        assert_eq!(b0.len(), 17);
        assert_eq!(b0[14], 0x04, "flags must have bit 2 set for slot 0");
        assert_eq!(u16::from_le_bytes([b0[15], b0[16]]), 0x0111);
        assert_eq!(VehicleDescr::from_bytes(&b0).unwrap(), d_slot0);

        // 2. Slot 2 only (opt_id = 0x0222) -> mask = 0x01 (bit 0)
        let mut d_slot2 = base.clone();
        d_slot2.optional_devices[2] = Some(0x0222);
        let b2 = d_slot2.to_bytes();
        assert_eq!(b2.len(), 17);
        assert_eq!(b2[14], 0x01, "flags must have bit 0 set for slot 2");
        assert_eq!(u16::from_le_bytes([b2[15], b2[16]]), 0x0222);
        assert_eq!(VehicleDescr::from_bytes(&b2).unwrap(), d_slot2);

        // 3. All 3 slots [0x00AA, 0x00BB, 0x00CC] -> mask = 0x07
        // Wire order must be descending slot order: [slot 2, slot 1, slot 0] -> [0x00CC, 0x00BB, 0x00AA]
        let mut d_all = base.clone();
        d_all.optional_devices = [Some(0x00AA), Some(0x00BB), Some(0x00CC)];
        let b_all = d_all.to_bytes();
        assert_eq!(b_all.len(), 21);
        assert_eq!(b_all[14], 0x07);
        assert_eq!(
            u16::from_le_bytes([b_all[15], b_all[16]]),
            0x00CC,
            "slot 2 is first on wire"
        );
        assert_eq!(
            u16::from_le_bytes([b_all[17], b_all[18]]),
            0x00BB,
            "slot 1 is second on wire"
        );
        assert_eq!(
            u16::from_le_bytes([b_all[19], b_all[20]]),
            0x00AA,
            "slot 0 is third on wire"
        );
        assert_eq!(VehicleDescr::from_bytes(&b_all).unwrap(), d_all);
    }

    #[test]
    fn test_vehicle_descr_flags_combinations_and_tail_order() {
        let full = VehicleDescr {
            nation_id: 1,
            type_id: 0,
            chassis_id: 0,
            engine_id: 0,
            fuel_tank_id: 203,
            radio_id: 2,
            turret_id: 0,
            gun_id: 0,
            optional_devices: [Some(10), Some(20), Some(30)],
            custom_emblem_id: Some(42),
            horn_id: Some(7),
            camouflage_id: Some(105),
        };

        // flags = 7 (optDevs) | 16 (emblem) | 64 (horn) | 128 (camo) = 215 = 0xD7
        let b = full.to_bytes();
        assert_eq!(b.len(), 25);
        assert_eq!(b[14], 7 | 16 | 64 | 128);
        // optDevs: slot 2 (30), slot 1 (20), slot 0 (10)
        assert_eq!(u16::from_le_bytes([b[15], b[16]]), 30);
        assert_eq!(u16::from_le_bytes([b[17], b[18]]), 20);
        assert_eq!(u16::from_le_bytes([b[19], b[20]]), 10);
        // emblemID
        assert_eq!(b[21], 42);
        // hornID
        assert_eq!(b[22], 7);
        // camouflageID
        assert_eq!(u16::from_le_bytes([b[23], b[24]]), 105);

        let parsed = VehicleDescr::from_bytes(&b).unwrap();
        assert_eq!(parsed, full);

        // Test camo only on stock tank: length 17
        let mut camo_only = full.clone();
        camo_only.optional_devices = [None, None, None];
        camo_only.custom_emblem_id = None;
        camo_only.horn_id = None;
        let b_camo = camo_only.to_bytes();
        assert_eq!(b_camo.len(), 17);
        assert_eq!(b_camo[14], 128);
        assert_eq!(u16::from_le_bytes([b_camo[15], b_camo[16]]), 105);
        assert_eq!(VehicleDescr::from_bytes(&b_camo).unwrap(), camo_only);
    }

    #[test]
    fn test_vehicle_weight_and_enhanced_suspension_fitting() {
        let Some(dir) = crate::testutil::game_dir() else {
            eprintln!("skip: no game install");
            return;
        };
        let catalog = crate::catalog::load(&dir).expect("catalog");
        let bt7_def = catalog
            .vehicle_defs
            .values()
            .find(|v| v.name == "BT-7")
            .expect("BT-7 def");

        let mut descr = VehicleDescr::stock(bt7_def).expect("stock BT-7");
        let (stock_w, max_w) = descr.compute_weight(bt7_def, &catalog).unwrap();
        assert!(stock_w <= max_w, "stock weight must not exceed maxLoad");

        // Upgraded conical turret (BT-7_conic) + 45mm_20K gun exceeds stock chassis maxLoad (14210 > 14200)
        let upgraded_turret = &bt7_def.turrets[1];
        let turret_cd = upgraded_turret.component.comp_descr;
        let upgraded_gun_cd = upgraded_turret
            .guns
            .iter()
            .find(|g| g.component.name == "_45mm_20K")
            .or_else(|| upgraded_turret.guns.last())
            .unwrap()
            .component
            .comp_descr;

        // Try installing heavy turret + gun on stock chassis without enhanced suspension -> TooHeavy
        let res = descr.may_install_turret(turret_cd, upgraded_gun_cd, bt7_def, &catalog);
        assert_eq!(res, Err(FittingError::TooHeavy));

        // Now mount enhanced suspension (Christie) into slot 0
        let christie = catalog
            .optional_devices
            .values()
            .find(|d| d.name == "enhancedSuspension_Christie")
            .expect("enhancedSuspension_Christie");
        descr
            .install_optional_device(christie.comp_descr, 0, bt7_def, &catalog)
            .expect("install enhanced suspension");

        let (_w_with_susp, max_w_with_susp) =
            descr.compute_weight(bt7_def, &catalog).unwrap();
        assert!(
            max_w_with_susp > max_w,
            "max load must increase with enhanced suspension"
        );

        // Now heavy turret + gun can be installed!
        descr
            .may_install_turret(turret_cd, upgraded_gun_cd, bt7_def, &catalog)
            .expect("may install turret with enhanced suspension");
        descr
            .install_turret(turret_cd, upgraded_gun_cd, bt7_def, &catalog)
            .expect("install turret");

        // Attempting to remove enhanced suspension while vehicle is over stock maxLoad fails!
        let rem_res = descr.may_remove_optional_device(0, bt7_def, &catalog);
        assert_eq!(rem_res, Err(FittingError::TooHeavy));
    }

    #[test]
    fn test_optional_device_duplicate_and_filter_rejection() {
        let Some(dir) = crate::testutil::game_dir() else {
            eprintln!("skip: no game install");
            return;
        };
        let catalog = crate::catalog::load(&dir).expect("catalog");
        let t34_def = catalog.vehicle_def_by_id(0, 0).expect("T-34 def");
        let mut descr = VehicleDescr::stock(t34_def).expect("stock T-34");

        let toolbox = catalog
            .optional_devices
            .values()
            .find(|d| d.name == "toolbox")
            .expect("toolbox");
        descr
            .install_optional_device(toolbox.comp_descr, 0, t34_def, &catalog)
            .expect("install toolbox");

        // Try installing toolbox again into slot 1 -> rejected as duplicate!
        let dup_res = descr.may_install_optional_device(toolbox.comp_descr, 1, t34_def, &catalog);
        assert_eq!(dup_res, Err(FittingError::AlreadyInstalled));

        // Try installing heavy spall liner on medium tank T-34 -> rejected by filter!
        let heavy_liner = catalog
            .optional_devices
            .values()
            .find(|d| d.name == "antifragmentationLining_heavy")
            .expect("heavy liner");
        let filter_res =
            descr.may_install_optional_device(heavy_liner.comp_descr, 1, t34_def, &catalog);
        assert!(matches!(
            filter_res,
            Err(FittingError::IncompatibleVehicleFilter(_))
        ));
    }

    #[test]
    fn test_check_equipments_rules() {
        let Some(dir) = crate::testutil::game_dir() else {
            eprintln!("skip: no game install");
            return;
        };
        let catalog = crate::catalog::load(&dir).expect("catalog");
        let t34_def = catalog.vehicle_def_by_id(0, 0).expect("T-34 def");
        let descr = VehicleDescr::stock(t34_def).expect("stock T-34");

        let repair_kit = catalog
            .equipments
            .values()
            .find(|e| e.name == "smallRepairkit")
            .expect("repairkit");
        let med_kit = catalog
            .equipments
            .values()
            .find(|e| e.name == "smallMedkit")
            .expect("medkit");
        let extinguisher = catalog
            .equipments
            .values()
            .find(|e| e.name == "handExtinguishers")
            .expect("extinguisher");

        // Compatible 3 items -> Ok
        let ok_eqs = [
            repair_kit.comp_descr,
            med_kit.comp_descr,
            extinguisher.comp_descr,
        ];
        assert!(check_equipments(&ok_eqs, &descr, t34_def, &catalog).is_ok());

        // Duplicate item across slots -> Err
        let dup_eqs = [repair_kit.comp_descr, repair_kit.comp_descr, 0];
        assert!(matches!(
            check_equipments(&dup_eqs, &descr, t34_def, &catalog),
            Err(FittingError::IncompatibleEquipment(_))
        ));

        // Conflicting fuel equipments (lendLeaseOil + gasoline100_oil / lendLeaseOil + lendLeaseOil)
        if let Some(oil) = catalog
            .equipments
            .values()
            .find(|e| e.name == "lendLeaseOil")
        {
            if let Some(other_fuel) = catalog
                .equipments
                .values()
                .find(|e| e.name != "lendLeaseOil" && e.tags.contains("fuel"))
            {
                let fuel_conflict = [oil.comp_descr, other_fuel.comp_descr, 0];
                assert!(matches!(
                    check_equipments(&fuel_conflict, &descr, t34_def, &catalog),
                    Err(FittingError::IncompatibleEquipment(_))
                ));
            }
        }
    }

    #[test]
    fn test_check_camouflage_allow_deny() {
        let Some(dir) = crate::testutil::game_dir() else {
            eprintln!("skip: no game install");
            return;
        };
        let catalog = crate::catalog::load(&dir).expect("catalog");
        let t34_def = catalog.vehicle_def_by_id(0, 0).expect("T-34 def");

        // camo_id = 0 is always allowed (clearing camo)
        assert!(check_camouflage(0, 0, t34_def, &catalog).is_ok());

        // Check each camo in ussr customizations
        if let Some(ussr_camos) = catalog.camouflages.get(&0) {
            for camo in ussr_camos.values().filter(|c| c.show_in_shop) {
                let res = check_camouflage(camo.id, 0, t34_def, &catalog);
                let veh_cd = t34_def.veh_type_comp_descr;
                if camo.deny.contains(&veh_cd) {
                    assert!(matches!(res, Err(FittingError::IncompatibleCamouflage(_))));
                } else if !camo.allow.is_empty() && !camo.allow.contains(&veh_cd) {
                    assert!(matches!(res, Err(FittingError::IncompatibleCamouflage(_))));
                } else {
                    assert!(res.is_ok());
                }
            }
        }
    }

    #[test]
    fn test_tankman_descr_binary_layout_and_roundtrip() {
        let tman = TankmanDescr {
            nation_id: 0,
            vehicle_type_id: 13,
            role_id: 1, // commander
            role_level: 100,
            skills: vec!["repair".to_string(), "camouflage".to_string()],
            last_skill_level: 75,
            is_female: false,
            is_premium: false,
            first_name_id: 12,
            last_name_id: 345,
            icon_id: 7,
            rank_idx: 1,
            num_levels_to_next_rank: 50,
            free_xp: 1234,
            dossier: vec![1, 2, 3, 4],
        };

        let bytes = tman.to_bytes();
        // 19 + 2 skills + 4 dossier = 25 bytes
        assert_eq!(bytes.len(), 25);
        assert_eq!(bytes[0], 8 | (0 << 4)); // header
        assert_eq!(bytes[1], 13);
        assert_eq!(bytes[2], 1);
        assert_eq!(bytes[3], 100);
        assert_eq!(bytes[4], 2); // numSkills
        assert_eq!(bytes[5], 6); // repair
        assert_eq!(bytes[6], 8); // camouflage
        assert_eq!(bytes[7], 75); // lastSkillLevel
        assert_eq!(bytes[8], 0); // flags
        assert_eq!(u16::from_le_bytes([bytes[9], bytes[10]]), 12);
        assert_eq!(u16::from_le_bytes([bytes[11], bytes[12]]), 345);
        assert_eq!(u16::from_le_bytes([bytes[13], bytes[14]]), 7);
        assert_eq!(bytes[15], 1);
        assert_eq!(bytes[16], 50);
        assert_eq!(
            i32::from_le_bytes([bytes[17], bytes[18], bytes[19], bytes[20]]),
            1234
        );
        assert_eq!(&bytes[21..], &[1, 2, 3, 4]);

        let unpacked = TankmanDescr::from_bytes(&bytes).expect("unpack tankman");
        assert_eq!(unpacked, tman);
    }

    #[test]
    fn test_level_up_xp_cost_formula() {
        // level_up_xp_cost(from_level, skill_seq_num) across [0, 50, 99], seq in [0, 1, 2]
        // round(2^seq * 50.0 * 100.0^(from_level/100.0))
        assert_eq!(level_up_xp_cost(0, 0), 50);
        assert_eq!(level_up_xp_cost(50, 0), 500);
        assert_eq!(level_up_xp_cost(99, 0), 4775);

        assert_eq!(level_up_xp_cost(0, 1), 100);
        assert_eq!(level_up_xp_cost(50, 1), 1000);
        assert_eq!(level_up_xp_cost(99, 1), 9550);

        assert_eq!(level_up_xp_cost(0, 2), 200);
        assert_eq!(level_up_xp_cost(50, 2), 2000);
        assert_eq!(level_up_xp_cost(99, 2), 19100);
    }

    #[test]
    fn test_tankman_add_xp_and_rank_progression() {
        let mut tman = TankmanDescr {
            nation_id: 0,
            vehicle_type_id: 13,
            role_id: 1,
            role_level: 50,
            skills: Vec::new(),
            last_skill_level: 0,
            is_female: false,
            is_premium: false,
            first_name_id: 1,
            last_name_id: 1,
            icon_id: 1,
            rank_idx: 0,
            num_levels_to_next_rank: 50,
            free_xp: 0,
            dossier: Vec::new(),
        };

        // Add enough XP to level up role_level from 50 to 100
        // Total XP cost = sum(level_up_xp_cost(lvl, 0) for lvl in 50..100)
        let total_cost: i32 = (50..100).map(|lvl| level_up_xp_cost(lvl, 0)).sum();
        tman.add_xp(total_cost, 9);
        assert_eq!(tman.role_level, 100);
        assert_eq!(tman.rank_idx, 1);
        assert_eq!(tman.num_levels_to_next_rank, 50);
        assert_eq!(tman.free_xp, 0);

        // Add 5000 excess XP -> stays in free_xp since no skills yet
        tman.add_xp(5000, 9);
        assert_eq!(tman.role_level, 100);
        assert_eq!(tman.free_xp, 5000);
    }

    #[test]
    fn test_tankman_add_skill_and_drop_skill() {
        let mut tman = TankmanDescr {
            nation_id: 0,
            vehicle_type_id: 13,
            role_id: 1,
            role_level: 100,
            skills: Vec::new(),
            last_skill_level: 0,
            is_female: false,
            is_premium: false,
            first_name_id: 1,
            last_name_id: 1,
            icon_id: 1,
            rank_idx: 1,
            num_levels_to_next_rank: 50,
            free_xp: 1000,
            dossier: Vec::new(),
        };

        // Adding skill consumes free_xp into new skill automatically
        tman.add_skill("repair", 9).expect("add skill");
        assert_eq!(tman.skills, vec!["repair"]);
        assert!(tman.last_skill_level > 0, "free_xp must have leveled up repair");
        assert!(tman.free_xp < 1000);

        // Drop skill: last_skill_level becomes 0, free_xp preserved
        let remaining_xp = tman.free_xp;
        tman.drop_skill("repair").expect("drop skill");
        assert!(tman.skills.is_empty());
        assert_eq!(tman.last_skill_level, 0);
        assert_eq!(tman.free_xp, remaining_xp);
    }

    #[test]
    fn test_tankman_respecialize() {
        let mut tman = TankmanDescr {
            nation_id: 0,
            vehicle_type_id: 13, // MS-1 (lightTank)
            role_id: 1,
            role_level: 100,
            skills: Vec::new(),
            last_skill_level: 0,
            is_female: false,
            is_premium: false,
            first_name_id: 1,
            last_name_id: 1,
            icon_id: 1,
            rank_idx: 1,
            num_levels_to_next_rank: 50,
            free_xp: 0,
            dossier: Vec::new(),
        };

        let light_tags: BTreeSet<String> = ["lightTank".to_string()].into();
        let medium_tags: BTreeSet<String> = ["mediumTank".to_string()].into();

        // Respecialize from MS-1 (light) to T-34 (medium):
        // vehicleChangeRoleLevelLoss = 0.1, classChangeRoleLevelLoss = 0.2
        // total loss = 0.3 -> new level = round(100 * 0.7) = 70
        tman.respecialize(0, 50, 0.1, 0.2, false, &light_tags, &medium_tags, 9);
        assert_eq!(tman.vehicle_type_id, 0);
        assert_eq!(tman.role_level, 70);
        // num_levels_to_next_rank increased by loss (100 - 70 = 30) -> 50 + 30 = 80
        assert_eq!(tman.num_levels_to_next_rank, 80);
    }

    #[test]
    fn test_tankman_replace_passport() {
        let Some(dir) = crate::testutil::game_dir() else {
            eprintln!("skip: no game install");
            return;
        };
        let catalog = crate::catalog::load(&dir).expect("catalog");
        let config = catalog.tankmen_config(0).expect("ussr tankmen config");

        // Normal male group test
        let normal_group = &config.normal_groups[0];
        let first_id = *normal_group.first_names.iter().next().unwrap();
        let last_id = *normal_group.last_names.iter().next().unwrap();
        let icon_id = *normal_group.icons.iter().next().unwrap();

        let mut tman = TankmanDescr {
            nation_id: 0,
            vehicle_type_id: 13,
            role_id: 1,
            role_level: 100,
            skills: Vec::new(),
            last_skill_level: 0,
            is_female: false,
            is_premium: false,
            first_name_id: 9999,
            last_name_id: 9999,
            icon_id: 9999,
            rank_idx: 0,
            num_levels_to_next_rank: 50,
            free_xp: 0,
            dossier: Vec::new(),
        };

        tman.replace_passport(
            Some(false),
            Some(first_id),
            Some(last_id),
            Some(icon_id),
            config,
        )
        .expect("valid male passport");
        assert_eq!(tman.first_name_id, first_id);
        assert_eq!(tman.last_name_id, last_id);
        assert_eq!(tman.icon_id, icon_id);
        assert!(!tman.is_female);

        // Premium female group test (e.g. women1)
        if let Some(women_group) = config.premium_groups.iter().find(|g| g.is_female) {
            let f_first = *women_group.first_names.iter().next().unwrap();
            let f_last = *women_group.last_names.iter().next().unwrap();
            let f_icon = *women_group.icons.iter().next().unwrap();

            tman.replace_passport(
                Some(true),
                Some(f_first),
                Some(f_last),
                Some(f_icon),
                config,
            )
            .expect("valid female passport");
            assert_eq!(tman.first_name_id, f_first);
            assert_eq!(tman.last_name_id, f_last);
            assert_eq!(tman.icon_id, f_icon);
            assert!(tman.is_female);
        }
    }

    #[test]
    fn test_veh_get_devices() {
        let Some(dir) = crate::testutil::game_dir() else {
            eprintln!("skip: no game install");
            return;
        };
        let catalog = crate::catalog::load(&dir).expect("catalog");
        let t34_def = catalog.vehicle_def_by_id(0, 0).expect("T-34 def");
        let descr = VehicleDescr::stock(t34_def).expect("stock T-34");

        let (def_comps, inst_comps, opt_devices) = descr.get_devices(t34_def);
        assert_eq!(def_comps.len(), 6);
        assert_eq!(inst_comps.len(), 6);
        assert_eq!(
            def_comps, inst_comps,
            "stock vehicle has matching default and installed components"
        );
        assert!(
            opt_devices.is_empty(),
            "stock vehicle has no optional devices"
        );
    }
}
