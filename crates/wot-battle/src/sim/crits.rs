//! Internal modules, crew crits, fire, ammo rack, consumables, and optional devices.

use std::collections::BTreeSet;
use glam::Vec3;
use rand::Rng;
use wot065_data::descr::{TankmanDescr, VehicleDescr};

use crate::assets::map_store::VehicleCollisionSet;
use crate::balance::BalanceConfig;
use crate::sim::ballistics::{
    InternalCritRay, COMPONENT_CHASSIS, COMPONENT_GUN, COMPONENT_HULL, COMPONENT_TURRET,
};
use crate::sim::physics::{EffectiveModifiers, VehiclePhysicsState, VehicleSimState};

/// Exact Damage Info Codes (0..=25) matching retail `constants.py:DAMAGE_INFO_CODES`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum DamageInfoCode {
    DeviceCritical = 0,
    DeviceDestroyed = 1,
    TankmanHit = 2,
    Fire = 3,
    DeviceCriticalAtShot = 4,
    DeviceCriticalAtRamming = 5,
    DeviceDestroyedAtShot = 6,
    DeviceDestroyedAtRamming = 7,
    DeviceStartedFireAtShot = 8,
    DeviceStartedFireAtRamming = 9,
    TankmanHitAtShot = 10,
    DeathFromDeviceExplosionAtShot = 11,
    DeviceCriticalAtFire = 12,
    DeviceDestroyedAtFire = 13,
    DeviceRepairedToCritical = 14,
    DeviceRepaired = 15,
    TankmanRestored = 16,
    DeathFromDeviceExplosionAtFire = 17,
    EngineCriticalAtUnlimitedRpm = 18,
    EngineDestroyedAtUnlimitedRpm = 19,
    DeathFromShot = 20,
    DeathFromInactiveCrewAtShot = 21,
    DeathFromRamming = 22,
    DeathFromFire = 23,
    DeathFromInactiveCrew = 24,
    FireStopped = 25,
}

impl DamageInfoCode {
    pub fn from_u8(val: u8) -> Option<Self> {
        match val {
            0 => Some(Self::DeviceCritical),
            1 => Some(Self::DeviceDestroyed),
            2 => Some(Self::TankmanHit),
            3 => Some(Self::Fire),
            4 => Some(Self::DeviceCriticalAtShot),
            5 => Some(Self::DeviceCriticalAtRamming),
            6 => Some(Self::DeviceDestroyedAtShot),
            7 => Some(Self::DeviceDestroyedAtRamming),
            8 => Some(Self::DeviceStartedFireAtShot),
            9 => Some(Self::DeviceStartedFireAtRamming),
            10 => Some(Self::TankmanHitAtShot),
            11 => Some(Self::DeathFromDeviceExplosionAtShot),
            12 => Some(Self::DeviceCriticalAtFire),
            13 => Some(Self::DeviceDestroyedAtFire),
            14 => Some(Self::DeviceRepairedToCritical),
            15 => Some(Self::DeviceRepaired),
            16 => Some(Self::TankmanRestored),
            17 => Some(Self::DeathFromDeviceExplosionAtFire),
            18 => Some(Self::EngineCriticalAtUnlimitedRpm),
            19 => Some(Self::EngineDestroyedAtUnlimitedRpm),
            20 => Some(Self::DeathFromShot),
            21 => Some(Self::DeathFromInactiveCrewAtShot),
            22 => Some(Self::DeathFromRamming),
            23 => Some(Self::DeathFromFire),
            24 => Some(Self::DeathFromInactiveCrew),
            25 => Some(Self::FireStopped),
            _ => None,
        }
    }
}

// 1-based extra indices matching `vehicle.xml:extras`
pub const EXTRA_NONE: u8 = 0;
pub const EXTRA_SHOOT: u8 = 1;
pub const EXTRA_NO_AMMO: u8 = 2;
pub const EXTRA_GUN_RELOAD: u8 = 3;
pub const EXTRA_FIRE: u8 = 4;
pub const EXTRA_STEREOSCOPE: u8 = 5;
pub const EXTRA_CAMOUFLAGE_NET: u8 = 6;
pub const EXTRA_HAND_EXTINGUISHERS: u8 = 7;
pub const EXTRA_AUTO_EXTINGUISHERS: u8 = 8;
pub const EXTRA_SMALL_MEDKIT: u8 = 9;
pub const EXTRA_LARGE_MEDKIT: u8 = 10;
pub const EXTRA_SMALL_REPAIRKIT: u8 = 11;
pub const EXTRA_LARGE_REPAIRKIT: u8 = 12;
pub const EXTRA_LEND_LEASE_OIL: u8 = 13;
pub const EXTRA_GASOLINE_100: u8 = 14;
pub const EXTRA_GASOLINE_105: u8 = 15;
pub const EXTRA_CHOCOLATE: u8 = 16;
pub const EXTRA_COCA_COLA: u8 = 17;
pub const EXTRA_RATION: u8 = 18;
pub const EXTRA_REMOVED_RPM_LIMITER: u8 = 19;
pub const EXTRA_ENGINE: u8 = 20;
pub const EXTRA_AMMO_BAY: u8 = 21;
pub const EXTRA_FUEL_TANK: u8 = 22;
pub const EXTRA_RADIO: u8 = 23;
pub const EXTRA_LEFT_TRACK: u8 = 24;
pub const EXTRA_RIGHT_TRACK: u8 = 25;
pub const EXTRA_GUN: u8 = 26;
pub const EXTRA_TURRET_ROTATOR: u8 = 27;
pub const EXTRA_SURVEYING_DEVICE: u8 = 28;
pub const EXTRA_COMMANDER: u8 = 29;
pub const EXTRA_DRIVER: u8 = 30;
pub const EXTRA_RADIOMAN1: u8 = 31;
pub const EXTRA_RADIOMAN2: u8 = 32;
pub const EXTRA_GUNNER1: u8 = 33;
pub const EXTRA_GUNNER2: u8 = 34;
pub const EXTRA_LOADER1: u8 = 35;
pub const EXTRA_LOADER2: u8 = 36;

/// Typed damage event broadcast to `Avatar.showVehicleDamageInfo` and combat handlers.
#[derive(Debug, Clone, PartialEq)]
pub struct DamageInfoEvent {
    pub code: DamageInfoCode,
    pub extra_index: u8,
    pub attacker_id: i32,
    pub target_id: i32,
}

/// Operational state of a vehicle device / internal module.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeviceState {
    Normal,
    Damaged,
    Destroyed,
}

/// Dynamic health and repair state of one vehicle device.
#[derive(Debug, Clone, PartialEq)]
pub struct DeviceDamageState {
    pub health: i32,
    pub max_health: i32,
    pub max_regen_health: i32,
    pub repair_cost: f32,
    pub state: DeviceState,
    pub repair_progress: f32,
}

impl Default for DeviceDamageState {
    fn default() -> Self {
        Self {
            health: 100,
            max_health: 100,
            max_regen_health: 50,
            repair_cost: 0.0,
            state: DeviceState::Normal,
            repair_progress: 0.0,
        }
    }
}

impl DeviceDamageState {
    pub fn new(max_health: i32, max_regen_health: i32, repair_cost: f32) -> Self {
        Self {
            health: max_health,
            max_health,
            max_regen_health,
            repair_cost,
            state: DeviceState::Normal,
            repair_progress: 0.0,
        }
    }
}

/// Dynamic health state of one crew member.
#[derive(Debug, Clone, PartialEq)]
pub struct TankmanDamageState {
    pub role: String,
    pub index_in_role: u8,
    pub is_alive: bool,
    pub role_level: u8,
}

impl TankmanDamageState {
    pub fn new(role: impl Into<String>, index_in_role: u8, role_level: u8) -> Self {
        Self {
            role: role.into(),
            index_in_role,
            is_alive: true,
            role_level,
        }
    }
}

/// Active fire state burning vehicle health and ticking internal damage.
#[derive(Debug, Clone, PartialEq)]
pub struct ActiveFire {
    pub attacker_id: i32,
    pub burn_damage_per_sec: f32,
    pub elapsed_secs: f32,
    pub duration_secs: f32,
    pub tick_accumulator: f32,
}

/// Equipment / consumable slot definition and runtime state (up to 3 slots).
#[derive(Debug, Clone, PartialEq)]
pub struct EquipmentSlot {
    pub comp_descr: i32,
    pub id: u16,
    pub name: String,
    pub count: u16,
    pub cooldown_left: f32,
    pub is_active: bool,
}

/// Comprehensive vehicle damage and critical-state tracking.
#[derive(Debug, Clone, PartialEq)]
pub struct VehicleDamageState {
    pub engine: DeviceDamageState,
    pub ammo_bay: DeviceDamageState,
    pub fuel_tank: DeviceDamageState,
    pub radio: DeviceDamageState,
    pub left_track: DeviceDamageState,
    pub right_track: DeviceDamageState,
    pub gun: DeviceDamageState,
    pub turret_rotator: DeviceDamageState,
    pub surveying_device: DeviceDamageState,
    pub tankmen: Vec<TankmanDamageState>,
    pub is_crew_active: bool,
    pub fire_state: Option<ActiveFire>,
    pub equipment_slots: [Option<EquipmentSlot>; 3],
    pub public_state_modifiers: BTreeSet<u8>,
    pub stationary_time: f32,
    pub rpm_limiter_damage_acc: f32,
    pub fire_damage_acc: f32,
}

impl Default for VehicleDamageState {
    fn default() -> Self {
        Self {
            engine: DeviceDamageState::new(100, 50, 0.0),
            ammo_bay: DeviceDamageState::new(100, 50, 0.0),
            fuel_tank: DeviceDamageState::new(100, 50, 0.0),
            radio: DeviceDamageState::new(100, 50, 0.0),
            left_track: DeviceDamageState::new(100, 50, 0.0),
            right_track: DeviceDamageState::new(100, 50, 0.0),
            gun: DeviceDamageState::new(100, 50, 0.0),
            turret_rotator: DeviceDamageState::new(100, 50, 0.0),
            surveying_device: DeviceDamageState::new(100, 50, 0.0),
            tankmen: vec![
                TankmanDamageState::new("commander", 1, 100),
                TankmanDamageState::new("driver", 1, 100),
                TankmanDamageState::new("gunner", 1, 100),
                TankmanDamageState::new("loader", 1, 100),
                TankmanDamageState::new("radioman", 1, 100),
            ],
            is_crew_active: true,
            fire_state: None,
            equipment_slots: [None, None, None],
            public_state_modifiers: BTreeSet::new(),
            stationary_time: 0.0,
            rpm_limiter_damage_acc: 0.0,
            fire_damage_acc: 0.0,
        }
    }
}

/// Normalize an equipment compact descriptor (`(item_id << 8) | 0xFB`) or bare `item_id`
/// into `(item_id, comp_descr)`.
pub fn normalize_equipment_id(raw_eq: u16) -> (u16, i32) {
    if (raw_eq & 0xFF) == 0xFB || (raw_eq > 12 && (raw_eq & 0x0F) == 11) {
        let item_id = raw_eq >> 8;
        (item_id, raw_eq as i32)
    } else {
        let item_id = raw_eq;
        let comp_descr = ((raw_eq as i32) << 8) | 0xFB;
        (item_id, comp_descr)
    }
}

/// Normalize an optional device compact descriptor (`(item_id << 8) | 0xF9`) or bare `item_id`
/// into `item_id`.
pub fn normalize_opt_device_id(raw_dev: u16) -> u16 {
    if (raw_dev & 0xFF) == 0xF9 || (raw_dev > 31 && (raw_dev & 0x0F) == 9) {
        raw_dev >> 8
    } else {
        raw_dev
    }
}

/// Calculate the effective crew skill percentage for a specific role (1=commander, 2=radioman, 3=driver, 4=gunner, 5=loader).
pub fn effective_crew_skill(
    role_id: u8,
    veh_descr: &VehicleDescr,
    crew: &[TankmanDescr],
    dmg: &VehicleDamageState,
) -> f32 {
    let role_name = match role_id {
        1 => "commander",
        2 => "radioman",
        3 => "driver",
        4 => "gunner",
        5 => "loader",
        _ => return 100.0,
    };

    // Check if tankman is alive
    let is_alive = dmg
        .tankmen
        .iter()
        .find(|t| t.role == role_name)
        .map_or(true, |t| t.is_alive);
    if !is_alive {
        return 0.0;
    }

    // Base level from TankmanDescr
    let base_level = crew
        .iter()
        .find(|c| c.role_id == role_id)
        .map_or(100.0, |c| c.role_level as f32);

    // Improved Ventilation bonus (+5%)
    let mut vent_bonus = 0.0f32;
    for &opt in &veh_descr.optional_devices {
        if let Some(raw_opt) = opt {
            let id = normalize_opt_device_id(raw_opt);
            // IDs 17, 18, 19: improvedVentilation_class1..3
            if (17..=19).contains(&id) {
                vent_bonus = 5.0;
                break;
            }
        }
    }

    // Stimulator consumable bonus (+10% e.g. chocolate, cocacola, ration)
    let mut stim_bonus = 0.0f32;
    for slot in dmg.equipment_slots.iter().flatten() {
        let (slot_id, _) = normalize_equipment_id(slot.id);
        if slot_id == 9
            || slot_id == 10
            || slot_id == 100
            || slot_id == 11
            || slot.name == "chocolate"
            || slot.name == "cocacola"
            || slot.name == "ration"
        {
            stim_bonus = 10.0;
            break;
        }
    }

    let direct_level = base_level + vent_bonus + stim_bonus;

    if role_id == 1 {
        // Commander does not receive commander bonus
        direct_level
    } else {
        // Other crew members receive 10% of commander's effective skill if commander is alive
        let cmd_alive = dmg
            .tankmen
            .iter()
            .find(|t| t.role == "commander")
            .map_or(true, |t| t.is_alive);
        let cmd_bonus = if cmd_alive {
            let cmd_base = crew
                .iter()
                .find(|c| c.role_id == 1)
                .map_or(100.0, |c| c.role_level as f32);
            (cmd_base + vent_bonus + stim_bonus) * 0.10
        } else {
            0.0
        };
        direct_level + cmd_bonus
    }
}

/// Compute effective passive modifiers for physics, ballistics, and vision.
pub fn compute_effective_modifiers(
    veh_descr: &VehicleDescr,
    crew: &[TankmanDescr],
    dmg: &VehicleDamageState,
) -> EffectiveModifiers {
    let mut mods = EffectiveModifiers::default();

    // 1. Optional Devices
    for &opt in &veh_descr.optional_devices {
        if let Some(raw_opt) = opt {
            let id = normalize_opt_device_id(raw_opt);
            match id {
                0 => {
                    // toolbox: +25% repair speed
                    mods.repair_speed_factor *= 1.25;
                }
                1 | 2 | 3 => {
                    // antifragmentationLining: +15% armor protection against HE/ramming
                    mods.antifragmentation_lining_factor *= 1.15;
                }
                4 => {
                    // stereoscope: +25% vision radius when stationary >= 3.0s
                    if dmg.stationary_time >= 3.0 {
                        mods.vision_radius_factor *= 1.25;
                    }
                }
                5 => {
                    // coatedOptics: +10% vision radius
                    mods.vision_radius_factor *= 1.10;
                }
                6 => {
                    // camouflageNet: +25% camo when stationary >= 3.0s
                    if dmg.stationary_time >= 3.0 {
                        mods.camo_factor *= 1.25;
                    }
                }
                20 => {
                    // grousers: soft ground 0.86, medium ground 0.91 resistance factor
                    mods.terrain_resistance_factors[2] *= 0.86;
                    mods.terrain_resistance_factors[1] *= 0.91;
                }
                21..=24 => {
                    // tankRammer: -10% gun reload time
                    mods.gun_reload_time_factor *= 0.90;
                }
                25 => {
                    // enhancedAimDrives: -10% aiming time
                    mods.gun_aiming_time_factor *= 0.90;
                }
                30 | 31 => {
                    // verticalStabilizer: -20% dispersion
                    mods.shot_dispersion_factor *= 0.80;
                }
                _ => {}
            }
        }
    }

    // 2. Consumables / Equipment slots
    for slot in dmg.equipment_slots.iter().flatten() {
        let (slot_id, _) = normalize_equipment_id(slot.id);
        if slot_id == 5 || slot.name == "largeRepairkit" {
            // Large Repairkit provides passive +10% repair speed
            mods.repair_speed_factor *= 1.10;
        } else if slot_id == 6
            || slot_id == 7
            || slot_id == 8
            || slot.name.contains("Oil")
            || slot.name.contains("gasoline")
        {
            // Fuel consumables provide +5% engine power and +5% turret rotation speed
            mods.engine_power_factor *= 1.05;
            mods.turret_rotation_speed_factor *= 1.05;
        } else if (slot_id == 12 || slot.name == "removedRpmLimiter") && slot.is_active {
            // Removed RPM Limiter provides +10% engine power when active
            mods.engine_power_factor *= 1.10;
        }
    }

    // 3. Crew skills & training level scaling (around nominal 100% baseline)
    let eff_cmd = effective_crew_skill(1, veh_descr, crew, dmg);
    if eff_cmd > 0.0 {
        let cmd_ratio = eff_cmd / 100.0;
        mods.vision_radius_factor *= cmd_ratio;
    } else {
        mods.vision_radius_factor *= 0.50; // dead commander cuts vision in half
    }

    let eff_gunner = effective_crew_skill(4, veh_descr, crew, dmg);
    if eff_gunner > 0.0 {
        let gunner_ratio = eff_gunner / 110.0;
        mods.gun_aiming_time_factor *= 1.0 / gunner_ratio;
        mods.shot_dispersion_factor *= 1.0 / gunner_ratio;
    } else {
        mods.gun_aiming_time_factor *= 2.0;
        mods.shot_dispersion_factor *= 2.0;
    }

    let eff_loader = effective_crew_skill(5, veh_descr, crew, dmg);
    if eff_loader > 0.0 {
        let loader_ratio = eff_loader / 110.0;
        mods.gun_reload_time_factor *= 1.0 / loader_ratio;
    } else {
        mods.gun_reload_time_factor *= 2.0;
    }

    let eff_driver = effective_crew_skill(3, veh_descr, crew, dmg);
    if eff_driver > 0.0 {
        let driver_ratio = eff_driver / 110.0;
        mods.engine_power_factor *= driver_ratio;
        mods.chassis_rotation_speed_factor *= driver_ratio;
    } else {
        mods.engine_power_factor *= 0.50;
        mods.chassis_rotation_speed_factor *= 0.50;
    }

    // 4. Damaged / Destroyed module states
    match dmg.engine.state {
        DeviceState::Destroyed => mods.engine_power_factor = 0.0,
        DeviceState::Damaged => mods.engine_power_factor *= 0.50,
        DeviceState::Normal => {}
    }

    match dmg.gun.state {
        DeviceState::Destroyed => mods.shot_dispersion_factor *= 3.0,
        DeviceState::Damaged => mods.shot_dispersion_factor *= 2.0,
        DeviceState::Normal => {}
    }

    match dmg.turret_rotator.state {
        DeviceState::Destroyed => mods.turret_rotation_speed_factor = 0.0,
        DeviceState::Damaged => mods.turret_rotation_speed_factor *= 0.50,
        DeviceState::Normal => {}
    }

    match dmg.surveying_device.state {
        DeviceState::Destroyed => mods.vision_radius_factor *= 0.50,
        DeviceState::Damaged => mods.vision_radius_factor *= 0.85,
        DeviceState::Normal => {}
    }

    match dmg.ammo_bay.state {
        DeviceState::Damaged => mods.gun_reload_time_factor *= 2.0,
        _ => {}
    }

    mods
}

/// Activate an equipment consumable by ID or slot.
pub fn activate_equipment(
    veh: &mut VehicleSimState,
    eq_id: u16,
    extra_arg: u16,
) -> Result<Vec<DamageInfoEvent>, &'static str> {
    if !veh.is_alive {
        return Err("vehicle is destroyed");
    }

    let (norm_eq_id, _) = normalize_equipment_id(eq_id);

    // 1. Locate slot matching eq_id or by slot index
    let slot_idx = veh
        .damage
        .equipment_slots
        .iter()
        .position(|s| {
            s.as_ref().map_or(false, |slot| {
                let (slot_norm_id, _) = normalize_equipment_id(slot.id);
                slot_norm_id == norm_eq_id
                    || slot.id == eq_id
                    || slot.comp_descr as u16 == eq_id
            })
        })
        .or_else(|| {
            if (eq_id as usize) < 3 && veh.damage.equipment_slots[eq_id as usize].is_some() {
                Some(eq_id as usize)
            } else {
                None
            }
        })
        .ok_or("equipment not equipped")?;

    let slot_ref = veh.damage.equipment_slots[slot_idx]
        .as_ref()
        .ok_or("equipment slot empty")?;

    if slot_ref.count == 0 {
        return Err("no consumable charges left");
    }
    if slot_ref.cooldown_left > 0.0 {
        return Err("consumable is on cooldown");
    }

    let (item_id, _) = normalize_equipment_id(slot_ref.id);
    let mut events = Vec::new();

    match item_id {
        // handExtinguishers (0) or autoExtinguishers (1)
        0 | 1 => {
            if veh.damage.fire_state.is_some() {
                veh.damage.fire_state = None;
                veh.damage.public_state_modifiers.remove(&EXTRA_FIRE);
                if let Some(slot) = veh.damage.equipment_slots[slot_idx].as_mut() {
                    slot.count = slot.count.saturating_sub(1);
                }
                events.push(DamageInfoEvent {
                    code: DamageInfoCode::FireStopped,
                    extra_index: EXTRA_FIRE,
                    attacker_id: 0,
                    target_id: veh.entity_id,
                });
            } else {
                return Err("vehicle is not on fire");
            }
        }
        // smallRepairkit (4)
        4 => {
            let extra = extra_arg as u8;
            let repaired_extra = repair_single_device(&mut veh.damage, &mut veh.physics, extra)?;
            if let Some(slot) = veh.damage.equipment_slots[slot_idx].as_mut() {
                slot.count = slot.count.saturating_sub(1);
            }
            events.push(DamageInfoEvent {
                code: DamageInfoCode::DeviceRepaired,
                extra_index: repaired_extra,
                attacker_id: 0,
                target_id: veh.entity_id,
            });
        }
        // largeRepairkit (5)
        5 => {
            repair_all_devices(&mut veh.damage, &mut veh.physics);
            if let Some(slot) = veh.damage.equipment_slots[slot_idx].as_mut() {
                slot.count = slot.count.saturating_sub(1);
            }
            events.push(DamageInfoEvent {
                code: DamageInfoCode::DeviceRepaired,
                extra_index: EXTRA_LARGE_REPAIRKIT,
                attacker_id: 0,
                target_id: veh.entity_id,
            });
        }
        // smallMedkit (2)
        2 => {
            let extra = extra_arg as u8;
            let healed_extra = heal_single_tankman(&mut veh.damage, extra)?;
            if let Some(slot) = veh.damage.equipment_slots[slot_idx].as_mut() {
                slot.count = slot.count.saturating_sub(1);
            }
            events.push(DamageInfoEvent {
                code: DamageInfoCode::TankmanRestored,
                extra_index: healed_extra,
                attacker_id: 0,
                target_id: veh.entity_id,
            });
        }
        // largeMedkit (3)
        3 => {
            for t in &mut veh.damage.tankmen {
                t.is_alive = true;
            }
            veh.damage.is_crew_active = true;
            if let Some(slot) = veh.damage.equipment_slots[slot_idx].as_mut() {
                slot.count = slot.count.saturating_sub(1);
            }
            events.push(DamageInfoEvent {
                code: DamageInfoCode::TankmanRestored,
                extra_index: EXTRA_LARGE_MEDKIT,
                attacker_id: 0,
                target_id: veh.entity_id,
            });
        }
        // removedRpmLimiter (12)
        12 => {
            if let Some(slot) = veh.damage.equipment_slots[slot_idx].as_mut() {
                if extra_arg != 0 {
                    slot.is_active = true;
                } else if extra_arg == 0 && slot.is_active {
                    slot.is_active = false;
                } else {
                    slot.is_active = !slot.is_active;
                }
            }
        }
        _ => {
            // passive consumables
        }
    }

    // Refresh vehicle modifiers
    let default_descr = VehicleDescr {
        nation_id: 0,
        type_id: 0,
        chassis_id: 0,
        engine_id: 0,
        fuel_tank_id: 0,
        radio_id: 0,
        turret_id: 0,
        gun_id: 0,
        optional_devices: [None, None, None],
        custom_emblem_id: None,
        horn_id: None,
        camouflage_id: None,
    };
    let descr = veh.descr.as_ref().unwrap_or(&default_descr);
    let crew = &veh.crew[..];
    veh.modifiers = compute_effective_modifiers(descr, crew, &veh.damage);

    Ok(events)
}

fn repair_single_device(
    damage: &mut VehicleDamageState,
    physics: &mut VehiclePhysicsState,
    extra: u8,
) -> Result<u8, &'static str> {
    match extra {
        EXTRA_LEFT_TRACK => {
            if damage.left_track.state == DeviceState::Normal && !physics.left_track_broken && damage.left_track.health >= damage.left_track.max_health {
                return Err("device is already normal");
            }
            damage.left_track.health = damage.left_track.max_health;
            damage.left_track.state = DeviceState::Normal;
            damage.left_track.repair_progress = 0.0;
            physics.left_track_broken = false;
            damage.public_state_modifiers.remove(&EXTRA_LEFT_TRACK);
            Ok(EXTRA_LEFT_TRACK)
        }
        EXTRA_RIGHT_TRACK => {
            if damage.right_track.state == DeviceState::Normal && !physics.right_track_broken && damage.right_track.health >= damage.right_track.max_health {
                return Err("device is already normal");
            }
            damage.right_track.health = damage.right_track.max_health;
            damage.right_track.state = DeviceState::Normal;
            damage.right_track.repair_progress = 0.0;
            physics.right_track_broken = false;
            damage.public_state_modifiers.remove(&EXTRA_RIGHT_TRACK);
            Ok(EXTRA_RIGHT_TRACK)
        }
        EXTRA_ENGINE => {
            if damage.engine.state == DeviceState::Normal && damage.engine.health >= damage.engine.max_health {
                return Err("device is already normal");
            }
            damage.engine.health = damage.engine.max_health;
            damage.engine.state = DeviceState::Normal;
            damage.engine.repair_progress = 0.0;
            damage.public_state_modifiers.remove(&EXTRA_ENGINE);
            Ok(EXTRA_ENGINE)
        }
        EXTRA_AMMO_BAY => {
            if damage.ammo_bay.state == DeviceState::Normal && damage.ammo_bay.health >= damage.ammo_bay.max_health {
                return Err("device is already normal");
            }
            damage.ammo_bay.health = damage.ammo_bay.max_health;
            damage.ammo_bay.state = DeviceState::Normal;
            damage.ammo_bay.repair_progress = 0.0;
            damage.public_state_modifiers.remove(&EXTRA_AMMO_BAY);
            Ok(EXTRA_AMMO_BAY)
        }
        EXTRA_FUEL_TANK => {
            if damage.fuel_tank.state == DeviceState::Normal && damage.fuel_tank.health >= damage.fuel_tank.max_health {
                return Err("device is already normal");
            }
            damage.fuel_tank.health = damage.fuel_tank.max_health;
            damage.fuel_tank.state = DeviceState::Normal;
            damage.fuel_tank.repair_progress = 0.0;
            damage.public_state_modifiers.remove(&EXTRA_FUEL_TANK);
            Ok(EXTRA_FUEL_TANK)
        }
        EXTRA_RADIO => {
            if damage.radio.state == DeviceState::Normal && damage.radio.health >= damage.radio.max_health {
                return Err("device is already normal");
            }
            damage.radio.health = damage.radio.max_health;
            damage.radio.state = DeviceState::Normal;
            damage.radio.repair_progress = 0.0;
            damage.public_state_modifiers.remove(&EXTRA_RADIO);
            Ok(EXTRA_RADIO)
        }
        EXTRA_GUN => {
            if damage.gun.state == DeviceState::Normal && damage.gun.health >= damage.gun.max_health {
                return Err("device is already normal");
            }
            damage.gun.health = damage.gun.max_health;
            damage.gun.state = DeviceState::Normal;
            damage.gun.repair_progress = 0.0;
            damage.public_state_modifiers.remove(&EXTRA_GUN);
            Ok(EXTRA_GUN)
        }
        EXTRA_TURRET_ROTATOR => {
            if damage.turret_rotator.state == DeviceState::Normal && damage.turret_rotator.health >= damage.turret_rotator.max_health {
                return Err("device is already normal");
            }
            damage.turret_rotator.health = damage.turret_rotator.max_health;
            damage.turret_rotator.state = DeviceState::Normal;
            damage.turret_rotator.repair_progress = 0.0;
            damage.public_state_modifiers.remove(&EXTRA_TURRET_ROTATOR);
            Ok(EXTRA_TURRET_ROTATOR)
        }
        EXTRA_SURVEYING_DEVICE => {
            if damage.surveying_device.state == DeviceState::Normal && damage.surveying_device.health >= damage.surveying_device.max_health {
                return Err("device is already normal");
            }
            damage.surveying_device.health = damage.surveying_device.max_health;
            damage.surveying_device.state = DeviceState::Normal;
            damage.surveying_device.repair_progress = 0.0;
            damage.public_state_modifiers.remove(&EXTRA_SURVEYING_DEVICE);
            Ok(EXTRA_SURVEYING_DEVICE)
        }
        0 => {
            // Auto repair first broken or damaged device
            if damage.left_track.state != DeviceState::Normal
                || physics.left_track_broken
                || damage.left_track.health < damage.left_track.max_health
            {
                repair_single_device(damage, physics, EXTRA_LEFT_TRACK)
            } else if damage.right_track.state != DeviceState::Normal
                || physics.right_track_broken
                || damage.right_track.health < damage.right_track.max_health
            {
                repair_single_device(damage, physics, EXTRA_RIGHT_TRACK)
            } else if damage.engine.state != DeviceState::Normal
                || damage.engine.health < damage.engine.max_health
            {
                repair_single_device(damage, physics, EXTRA_ENGINE)
            } else if damage.gun.state != DeviceState::Normal
                || damage.gun.health < damage.gun.max_health
            {
                repair_single_device(damage, physics, EXTRA_GUN)
            } else if damage.turret_rotator.state != DeviceState::Normal
                || damage.turret_rotator.health < damage.turret_rotator.max_health
            {
                repair_single_device(damage, physics, EXTRA_TURRET_ROTATOR)
            } else if damage.ammo_bay.state != DeviceState::Normal
                || damage.ammo_bay.health < damage.ammo_bay.max_health
            {
                repair_single_device(damage, physics, EXTRA_AMMO_BAY)
            } else if damage.fuel_tank.state != DeviceState::Normal
                || damage.fuel_tank.health < damage.fuel_tank.max_health
            {
                repair_single_device(damage, physics, EXTRA_FUEL_TANK)
            } else if damage.radio.state != DeviceState::Normal
                || damage.radio.health < damage.radio.max_health
            {
                repair_single_device(damage, physics, EXTRA_RADIO)
            } else if damage.surveying_device.state != DeviceState::Normal
                || damage.surveying_device.health < damage.surveying_device.max_health
            {
                repair_single_device(damage, physics, EXTRA_SURVEYING_DEVICE)
            } else {
                Err("no damaged devices to repair")
            }
        }
        _ => Err("unknown device extra index"),
    }
}

fn repair_all_devices(damage: &mut VehicleDamageState, physics: &mut VehiclePhysicsState) {
    let devices = [
        &mut damage.engine,
        &mut damage.ammo_bay,
        &mut damage.fuel_tank,
        &mut damage.radio,
        &mut damage.left_track,
        &mut damage.right_track,
        &mut damage.gun,
        &mut damage.turret_rotator,
        &mut damage.surveying_device,
    ];
    for d in devices {
        d.health = d.max_health;
        d.state = DeviceState::Normal;
        d.repair_progress = 0.0;
    }
    physics.left_track_broken = false;
    physics.right_track_broken = false;
    damage.public_state_modifiers.remove(&EXTRA_LEFT_TRACK);
    damage.public_state_modifiers.remove(&EXTRA_RIGHT_TRACK);
    damage.public_state_modifiers.remove(&EXTRA_ENGINE);
    damage.public_state_modifiers.remove(&EXTRA_AMMO_BAY);
    damage.public_state_modifiers.remove(&EXTRA_FUEL_TANK);
    damage.public_state_modifiers.remove(&EXTRA_RADIO);
    damage.public_state_modifiers.remove(&EXTRA_GUN);
    damage.public_state_modifiers.remove(&EXTRA_TURRET_ROTATOR);
    damage.public_state_modifiers.remove(&EXTRA_SURVEYING_DEVICE);
}

fn heal_single_tankman(damage: &mut VehicleDamageState, extra: u8) -> Result<u8, &'static str> {
    if extra == 0 {
        // Heal first dead tankman
        if let Some(t) = damage.tankmen.iter_mut().find(|t| !t.is_alive) {
            t.is_alive = true;
            damage.is_crew_active = true;
            let role_extra = match (t.role.as_str(), t.index_in_role) {
                ("commander", _) => EXTRA_COMMANDER,
                ("driver", _) => EXTRA_DRIVER,
                ("gunner", 2) => EXTRA_GUNNER2,
                ("gunner", _) => EXTRA_GUNNER1,
                ("loader", 2) => EXTRA_LOADER2,
                ("loader", _) => EXTRA_LOADER1,
                ("radioman", 2) => EXTRA_RADIOMAN2,
                ("radioman", _) => EXTRA_RADIOMAN1,
                _ => EXTRA_NONE,
            };
            return Ok(role_extra);
        }
        return Err("no dead tankmen to heal");
    }

    let target_role = match extra {
        EXTRA_COMMANDER => ("commander", 1),
        EXTRA_DRIVER => ("driver", 1),
        EXTRA_GUNNER1 => ("gunner", 1),
        EXTRA_GUNNER2 => ("gunner", 2),
        EXTRA_LOADER1 => ("loader", 1),
        EXTRA_LOADER2 => ("loader", 2),
        EXTRA_RADIOMAN1 => ("radioman", 1),
        EXTRA_RADIOMAN2 => ("radioman", 2),
        _ => return Err("invalid tankman extra"),
    };

    if let Some(t) = damage
        .tankmen
        .iter_mut()
        .find(|t| t.role == target_role.0 && t.index_in_role == target_role.1)
    {
        if t.is_alive {
            return Err("tankman is already alive");
        }
        t.is_alive = true;
        damage.is_crew_active = true;
        Ok(extra)
    } else {
        Err("tankman role not found on vehicle")
    }
}

/// Advance damage state timers (fire burn, auto-repair, consumables) for one tick `dt`.
pub fn step_damage_state(
    veh: &mut VehicleSimState,
    _balance: &BalanceConfig,
    dt: f32,
) -> Vec<DamageInfoEvent> {
    if dt <= 0.0 || !dt.is_finite() {
        return Vec::new();
    }

    let mut events = Vec::new();

    // 1. Instant destruction checks
    if veh.damage.ammo_bay.state == DeviceState::Destroyed || veh.damage.ammo_bay.health <= 0 {
        veh.is_alive = false;
        events.push(DamageInfoEvent {
            code: DamageInfoCode::DeathFromDeviceExplosionAtShot,
            extra_index: EXTRA_AMMO_BAY,
            attacker_id: 0,
            target_id: veh.entity_id,
        });
        return events;
    }

    if veh.damage.tankmen.iter().all(|t| !t.is_alive) {
        veh.damage.is_crew_active = false;
        veh.is_alive = false;
        events.push(DamageInfoEvent {
            code: DamageInfoCode::DeathFromInactiveCrewAtShot,
            extra_index: EXTRA_NONE,
            attacker_id: 0,
            target_id: veh.entity_id,
        });
        return events;
    }

    // 2. Fire tick & auto-extinguisher
    if let Some(fire) = &mut veh.damage.fire_state {
        // Check if autoExtinguishers is installed
        let has_auto_ext = veh.damage.equipment_slots.iter_mut().find(|s| {
            s.as_ref().map_or(false, |slot| {
                normalize_equipment_id(slot.id).0 == 1 && slot.count > 0
            })
        });
        if let Some(Some(slot)) = has_auto_ext {
            slot.count = slot.count.saturating_sub(1);
            veh.damage.fire_state = None;
            veh.damage.public_state_modifiers.remove(&EXTRA_FIRE);
            events.push(DamageInfoEvent {
                code: DamageInfoCode::FireStopped,
                extra_index: EXTRA_FIRE,
                attacker_id: 0,
                target_id: veh.entity_id,
            });
        } else {
            fire.elapsed_secs += dt;
            fire.tick_accumulator += dt;

            // Deal fractional HP burn loss to vehicle
            let hp_loss_float = fire.burn_damage_per_sec * dt;
            veh.damage.fire_damage_acc += hp_loss_float;
            let whole_hp_loss = veh.damage.fire_damage_acc.floor() as i32;
            if whole_hp_loss > 0 {
                veh.damage.fire_damage_acc -= whole_hp_loss as f32;
                veh.health = (veh.health - whole_hp_loss).max(0);
            }

            // Deal periodic fire damage to internal modules
            if fire.tick_accumulator >= 1.0 {
                fire.tick_accumulator -= 1.0;
                veh.damage.engine.health = (veh.damage.engine.health - 15).max(0);
                if veh.damage.engine.health == 0 && veh.damage.engine.state != DeviceState::Destroyed {
                    veh.damage.engine.state = DeviceState::Destroyed;
                    events.push(DamageInfoEvent {
                        code: DamageInfoCode::DeviceDestroyedAtFire,
                        extra_index: EXTRA_ENGINE,
                        attacker_id: fire.attacker_id,
                        target_id: veh.entity_id,
                    });
                }
            }

            if veh.health <= 0 {
                veh.is_alive = false;
                let attacker = fire.attacker_id;
                veh.damage.fire_state = None;
                veh.damage.public_state_modifiers.remove(&EXTRA_FIRE);
                events.push(DamageInfoEvent {
                    code: DamageInfoCode::DeathFromFire,
                    extra_index: EXTRA_FIRE,
                    attacker_id: attacker,
                    target_id: veh.entity_id,
                });
                return events;
            }

            if fire.elapsed_secs >= fire.duration_secs {
                veh.damage.fire_state = None;
                veh.damage.public_state_modifiers.remove(&EXTRA_FIRE);
                events.push(DamageInfoEvent {
                    code: DamageInfoCode::FireStopped,
                    extra_index: EXTRA_FIRE,
                    attacker_id: 0,
                    target_id: veh.entity_id,
                });
            }
        }
    }

    // 3. Auto-repair of destroyed modules
    let repair_speed_factor = veh.modifiers.repair_speed_factor;
    let base_repair_rate = 12.5f32; // HP/sec base repair rate
    let regen_step = base_repair_rate * repair_speed_factor * dt;

    // Left track
    if veh.damage.left_track.state == DeviceState::Destroyed {
        veh.damage.left_track.repair_progress += regen_step;
        if veh.damage.left_track.repair_progress >= veh.damage.left_track.max_regen_health as f32 {
            veh.damage.left_track.health = veh.damage.left_track.max_regen_health;
            veh.damage.left_track.state = DeviceState::Damaged;
            veh.damage.left_track.repair_progress = 0.0;
            veh.physics.left_track_broken = false;
            veh.damage.public_state_modifiers.remove(&EXTRA_LEFT_TRACK);
            events.push(DamageInfoEvent {
                code: DamageInfoCode::DeviceRepairedToCritical,
                extra_index: EXTRA_LEFT_TRACK,
                attacker_id: 0,
                target_id: veh.entity_id,
            });
        }
    }

    // Right track
    if veh.damage.right_track.state == DeviceState::Destroyed {
        veh.damage.right_track.repair_progress += regen_step;
        if veh.damage.right_track.repair_progress >= veh.damage.right_track.max_regen_health as f32 {
            veh.damage.right_track.health = veh.damage.right_track.max_regen_health;
            veh.damage.right_track.state = DeviceState::Damaged;
            veh.damage.right_track.repair_progress = 0.0;
            veh.physics.right_track_broken = false;
            veh.damage.public_state_modifiers.remove(&EXTRA_RIGHT_TRACK);
            events.push(DamageInfoEvent {
                code: DamageInfoCode::DeviceRepairedToCritical,
                extra_index: EXTRA_RIGHT_TRACK,
                attacker_id: 0,
                target_id: veh.entity_id,
            });
        }
    }

    // Gun
    if veh.damage.gun.state == DeviceState::Destroyed {
        veh.damage.gun.repair_progress += regen_step;
        if veh.damage.gun.repair_progress >= veh.damage.gun.max_regen_health as f32 {
            veh.damage.gun.health = veh.damage.gun.max_regen_health;
            veh.damage.gun.state = DeviceState::Damaged;
            veh.damage.gun.repair_progress = 0.0;
            events.push(DamageInfoEvent {
                code: DamageInfoCode::DeviceRepairedToCritical,
                extra_index: EXTRA_GUN,
                attacker_id: 0,
                target_id: veh.entity_id,
            });
        }
    }

    // Turret rotator
    if veh.damage.turret_rotator.state == DeviceState::Destroyed {
        veh.damage.turret_rotator.repair_progress += regen_step;
        if veh.damage.turret_rotator.repair_progress >= veh.damage.turret_rotator.max_regen_health as f32 {
            veh.damage.turret_rotator.health = veh.damage.turret_rotator.max_regen_health;
            veh.damage.turret_rotator.state = DeviceState::Damaged;
            veh.damage.turret_rotator.repair_progress = 0.0;
            events.push(DamageInfoEvent {
                code: DamageInfoCode::DeviceRepairedToCritical,
                extra_index: EXTRA_TURRET_ROTATOR,
                attacker_id: 0,
                target_id: veh.entity_id,
            });
        }
    }

    // Engine
    if veh.damage.engine.state == DeviceState::Destroyed {
        veh.damage.engine.repair_progress += regen_step;
        if veh.damage.engine.repair_progress >= veh.damage.engine.max_regen_health as f32 {
            veh.damage.engine.health = veh.damage.engine.max_regen_health;
            veh.damage.engine.state = DeviceState::Damaged;
            veh.damage.engine.repair_progress = 0.0;
            events.push(DamageInfoEvent {
                code: DamageInfoCode::DeviceRepairedToCritical,
                extra_index: EXTRA_ENGINE,
                attacker_id: 0,
                target_id: veh.entity_id,
            });
        }
    }

    // Radio
    if veh.damage.radio.state == DeviceState::Destroyed {
        veh.damage.radio.repair_progress += regen_step;
        if veh.damage.radio.repair_progress >= veh.damage.radio.max_regen_health as f32 {
            veh.damage.radio.health = veh.damage.radio.max_regen_health;
            veh.damage.radio.state = DeviceState::Damaged;
            veh.damage.radio.repair_progress = 0.0;
            events.push(DamageInfoEvent {
                code: DamageInfoCode::DeviceRepairedToCritical,
                extra_index: EXTRA_RADIO,
                attacker_id: 0,
                target_id: veh.entity_id,
            });
        }
    }

    // Surveying device
    if veh.damage.surveying_device.state == DeviceState::Destroyed {
        veh.damage.surveying_device.repair_progress += regen_step;
        if veh.damage.surveying_device.repair_progress >= veh.damage.surveying_device.max_regen_health as f32 {
            veh.damage.surveying_device.health = veh.damage.surveying_device.max_regen_health;
            veh.damage.surveying_device.state = DeviceState::Damaged;
            veh.damage.surveying_device.repair_progress = 0.0;
            events.push(DamageInfoEvent {
                code: DamageInfoCode::DeviceRepairedToCritical,
                extra_index: EXTRA_SURVEYING_DEVICE,
                attacker_id: 0,
                target_id: veh.entity_id,
            });
        }
    }

    // 4. Removed RPM Limiter tick
    let rpm_active = veh
        .damage
        .equipment_slots
        .iter()
        .any(|s| {
            s.as_ref().map_or(false, |slot| {
                normalize_equipment_id(slot.id).0 == 12 && slot.is_active
            })
        });
    if rpm_active {
        veh.damage.rpm_limiter_damage_acc += 2.0 * dt;
        let whole_dmg = veh.damage.rpm_limiter_damage_acc.floor() as i32;
        if whole_dmg > 0 {
            veh.damage.rpm_limiter_damage_acc -= whole_dmg as f32;
            veh.damage.engine.health = (veh.damage.engine.health - whole_dmg).max(0);
            if veh.damage.engine.health <= 0 && veh.damage.engine.state != DeviceState::Destroyed {
                veh.damage.engine.state = DeviceState::Destroyed;
                events.push(DamageInfoEvent {
                    code: DamageInfoCode::EngineDestroyedAtUnlimitedRpm,
                    extra_index: EXTRA_ENGINE,
                    attacker_id: 0,
                    target_id: veh.entity_id,
                });
            } else if veh.damage.engine.health < veh.damage.engine.max_regen_health && veh.damage.engine.state == DeviceState::Normal {
                veh.damage.engine.state = DeviceState::Damaged;
                events.push(DamageInfoEvent {
                    code: DamageInfoCode::EngineCriticalAtUnlimitedRpm,
                    extra_index: EXTRA_ENGINE,
                    attacker_id: 0,
                    target_id: veh.entity_id,
                });
            }
        }
    }

    // 5. Stationary timer for stereoscope & camouflageNet
    if veh.physics.speed_mps.abs() < 0.05 && veh.physics.rot_speed_rps.abs() < 0.01 {
        veh.damage.stationary_time += dt;
        if veh.damage.stationary_time >= 3.0 {
            let has_stereoscope = veh.descr.as_ref().map_or(false, |d| {
                d.optional_devices
                    .iter()
                    .flatten()
                    .any(|&raw| normalize_opt_device_id(raw) == 4)
            });
            let has_camonet = veh.descr.as_ref().map_or(false, |d| {
                d.optional_devices
                    .iter()
                    .flatten()
                    .any(|&raw| normalize_opt_device_id(raw) == 6)
            });

            if has_stereoscope {
                veh.damage.public_state_modifiers.insert(EXTRA_STEREOSCOPE);
            }
            if has_camonet {
                veh.damage.public_state_modifiers.insert(EXTRA_CAMOUFLAGE_NET);
            }
        }
    } else {
        veh.damage.stationary_time = 0.0;
        veh.damage.public_state_modifiers.remove(&EXTRA_STEREOSCOPE);
        veh.damage.public_state_modifiers.remove(&EXTRA_CAMOUFLAGE_NET);
    }

    // Refresh modifiers
    let default_descr = VehicleDescr {
        nation_id: 0,
        type_id: 0,
        chassis_id: 0,
        engine_id: 0,
        fuel_tank_id: 0,
        radio_id: 0,
        turret_id: 0,
        gun_id: 0,
        optional_devices: [None, None, None],
        custom_emblem_id: None,
        horn_id: None,
        camouflage_id: None,
    };
    let descr = veh.descr.as_ref().unwrap_or(&default_descr);
    let crew = &veh.crew[..];
    veh.modifiers = compute_effective_modifiers(descr, crew, &veh.damage);

    events
}

/// Apply internal crit ray through vehicle component materials.
pub fn apply_internal_crit_ray<R: Rng + ?Sized>(
    ray: &InternalCritRay,
    veh: &mut VehicleSimState,
    collision_set: Option<&VehicleCollisionSet>,
    balance: &BalanceConfig,
    rng: &mut R,
) -> Vec<DamageInfoEvent> {
    let mut events = Vec::new();

    let col_set = match collision_set {
        Some(c) => c,
        None => return events,
    };

    let hit_tester = match ray.component_idx {
        COMPONENT_CHASSIS => &col_set.chassis,
        COMPONENT_HULL => &col_set.hull,
        COMPONENT_TURRET => &col_set.turret,
        COMPONENT_GUN => &col_set.gun,
        _ => return events,
    };

    let hits = hit_tester.raycast_all(ray.local_origin, ray.local_dir, ray.max_depth);
    let mut seen_materials = BTreeSet::new();

    for hit in hits {
        let mat_name = match hit_tester.bsp.material_name(&hit) {
            Some(m) => m,
            None => continue,
        };

        // Skip exterior armor triangles
        if mat_name.starts_with("armor") {
            continue;
        }

        // Deduplicate along the same projectile path
        if !seen_materials.insert(mat_name.to_string()) {
            continue;
        }

        let damage_val = ray.devices_damage.max(1.0) as i32;

        match mat_name {
            "ammoBay" | "ammoBayHealth" => {
                let roll: f32 = rng.gen();
                if roll <= 0.27 {
                    veh.damage.ammo_bay.health = (veh.damage.ammo_bay.health - damage_val).max(0);
                    if veh.damage.ammo_bay.health <= 0 {
                        veh.damage.ammo_bay.state = DeviceState::Destroyed;
                        veh.is_alive = false;
                        veh.damage.is_crew_active = false;
                        events.push(DamageInfoEvent {
                            code: DamageInfoCode::DeathFromDeviceExplosionAtShot,
                            extra_index: EXTRA_AMMO_BAY,
                            attacker_id: ray.attacker_id,
                            target_id: veh.entity_id,
                        });
                        return events;
                    } else if veh.damage.ammo_bay.health < veh.damage.ammo_bay.max_health {
                        veh.damage.ammo_bay.state = DeviceState::Damaged;
                        events.push(DamageInfoEvent {
                            code: DamageInfoCode::DeviceCriticalAtShot,
                            extra_index: EXTRA_AMMO_BAY,
                            attacker_id: ray.attacker_id,
                            target_id: veh.entity_id,
                        });
                    }
                }
            }
            "fuelTank" | "fuelTankHealth" => {
                let roll: f32 = rng.gen();
                if roll <= 0.45 {
                    veh.damage.fuel_tank.health = (veh.damage.fuel_tank.health - damage_val).max(0);
                    if veh.damage.fuel_tank.health <= 0 {
                        veh.damage.fuel_tank.state = DeviceState::Destroyed;
                        veh.damage.fire_state = Some(ActiveFire {
                            attacker_id: ray.attacker_id,
                            burn_damage_per_sec: balance.default_health_burn_per_sec,
                            elapsed_secs: 0.0,
                            duration_secs: 10.0,
                            tick_accumulator: 0.0,
                        });
                        veh.damage.public_state_modifiers.insert(EXTRA_FIRE);
                        events.push(DamageInfoEvent {
                            code: DamageInfoCode::DeviceStartedFireAtShot,
                            extra_index: EXTRA_FUEL_TANK,
                            attacker_id: ray.attacker_id,
                            target_id: veh.entity_id,
                        });
                    } else if veh.damage.fuel_tank.health < veh.damage.fuel_tank.max_health {
                        veh.damage.fuel_tank.state = DeviceState::Damaged;
                        events.push(DamageInfoEvent {
                            code: DamageInfoCode::DeviceCriticalAtShot,
                            extra_index: EXTRA_FUEL_TANK,
                            attacker_id: ray.attacker_id,
                            target_id: veh.entity_id,
                        });
                    }
                }
            }
            "engine" | "engineHealth" => {
                let roll: f32 = rng.gen();
                if roll <= 0.45 {
                    veh.damage.engine.health = (veh.damage.engine.health - damage_val).max(0);
                    if damage_val >= balance.min_fire_starting_damage {
                        let fire_roll: f32 = rng.gen();
                        if fire_roll <= 0.20 && veh.damage.fire_state.is_none() {
                            veh.damage.fire_state = Some(ActiveFire {
                                attacker_id: ray.attacker_id,
                                burn_damage_per_sec: balance.default_health_burn_per_sec,
                                elapsed_secs: 0.0,
                                duration_secs: 10.0,
                                tick_accumulator: 0.0,
                            });
                            veh.damage.public_state_modifiers.insert(EXTRA_FIRE);
                            events.push(DamageInfoEvent {
                                code: DamageInfoCode::DeviceStartedFireAtShot,
                                extra_index: EXTRA_ENGINE,
                                attacker_id: ray.attacker_id,
                                target_id: veh.entity_id,
                            });
                        }
                    }
                    if veh.damage.engine.health <= 0 {
                        veh.damage.engine.state = DeviceState::Destroyed;
                        events.push(DamageInfoEvent {
                            code: DamageInfoCode::DeviceDestroyedAtShot,
                            extra_index: EXTRA_ENGINE,
                            attacker_id: ray.attacker_id,
                            target_id: veh.entity_id,
                        });
                    } else if veh.damage.engine.health < veh.damage.engine.max_health {
                        veh.damage.engine.state = DeviceState::Damaged;
                        events.push(DamageInfoEvent {
                            code: DamageInfoCode::DeviceCriticalAtShot,
                            extra_index: EXTRA_ENGINE,
                            attacker_id: ray.attacker_id,
                            target_id: veh.entity_id,
                        });
                    }
                }
            }
            "leftTrack" | "leftTrackHealth" => {
                veh.damage.left_track.health = (veh.damage.left_track.health - damage_val).max(0);
                if veh.damage.left_track.health <= 0 {
                    veh.damage.left_track.state = DeviceState::Destroyed;
                    veh.physics.left_track_broken = true;
                    veh.damage.public_state_modifiers.insert(EXTRA_LEFT_TRACK);
                    events.push(DamageInfoEvent {
                        code: DamageInfoCode::DeviceDestroyedAtShot,
                        extra_index: EXTRA_LEFT_TRACK,
                        attacker_id: ray.attacker_id,
                        target_id: veh.entity_id,
                    });
                } else {
                    veh.damage.left_track.state = DeviceState::Damaged;
                    events.push(DamageInfoEvent {
                        code: DamageInfoCode::DeviceCriticalAtShot,
                        extra_index: EXTRA_LEFT_TRACK,
                        attacker_id: ray.attacker_id,
                        target_id: veh.entity_id,
                    });
                }
            }
            "rightTrack" | "rightTrackHealth" => {
                veh.damage.right_track.health = (veh.damage.right_track.health - damage_val).max(0);
                if veh.damage.right_track.health <= 0 {
                    veh.damage.right_track.state = DeviceState::Destroyed;
                    veh.physics.right_track_broken = true;
                    veh.damage.public_state_modifiers.insert(EXTRA_RIGHT_TRACK);
                    events.push(DamageInfoEvent {
                        code: DamageInfoCode::DeviceDestroyedAtShot,
                        extra_index: EXTRA_RIGHT_TRACK,
                        attacker_id: ray.attacker_id,
                        target_id: veh.entity_id,
                    });
                } else {
                    veh.damage.right_track.state = DeviceState::Damaged;
                    events.push(DamageInfoEvent {
                        code: DamageInfoCode::DeviceCriticalAtShot,
                        extra_index: EXTRA_RIGHT_TRACK,
                        attacker_id: ray.attacker_id,
                        target_id: veh.entity_id,
                    });
                }
            }
            "gun" | "gunHealth" => {
                let roll: f32 = rng.gen();
                if roll <= 0.33 {
                    veh.damage.gun.health = (veh.damage.gun.health - damage_val).max(0);
                    if veh.damage.gun.health <= 0 {
                        veh.damage.gun.state = DeviceState::Destroyed;
                        events.push(DamageInfoEvent {
                            code: DamageInfoCode::DeviceDestroyedAtShot,
                            extra_index: EXTRA_GUN,
                            attacker_id: ray.attacker_id,
                            target_id: veh.entity_id,
                        });
                    } else {
                        veh.damage.gun.state = DeviceState::Damaged;
                        events.push(DamageInfoEvent {
                            code: DamageInfoCode::DeviceCriticalAtShot,
                            extra_index: EXTRA_GUN,
                            attacker_id: ray.attacker_id,
                            target_id: veh.entity_id,
                        });
                    }
                }
            }
            "turretRotator" | "turretRotatorHealth" => {
                let roll: f32 = rng.gen();
                if roll <= 0.45 {
                    veh.damage.turret_rotator.health = (veh.damage.turret_rotator.health - damage_val).max(0);
                    if veh.damage.turret_rotator.health <= 0 {
                        veh.damage.turret_rotator.state = DeviceState::Destroyed;
                        events.push(DamageInfoEvent {
                            code: DamageInfoCode::DeviceDestroyedAtShot,
                            extra_index: EXTRA_TURRET_ROTATOR,
                            attacker_id: ray.attacker_id,
                            target_id: veh.entity_id,
                        });
                    } else {
                        veh.damage.turret_rotator.state = DeviceState::Damaged;
                        events.push(DamageInfoEvent {
                            code: DamageInfoCode::DeviceCriticalAtShot,
                            extra_index: EXTRA_TURRET_ROTATOR,
                            attacker_id: ray.attacker_id,
                            target_id: veh.entity_id,
                        });
                    }
                }
            }
            "radio" | "radioHealth" => {
                let roll: f32 = rng.gen();
                if roll <= 0.45 {
                    veh.damage.radio.health = (veh.damage.radio.health - damage_val).max(0);
                    if veh.damage.radio.health <= 0 {
                        veh.damage.radio.state = DeviceState::Destroyed;
                        events.push(DamageInfoEvent {
                            code: DamageInfoCode::DeviceDestroyedAtShot,
                            extra_index: EXTRA_RADIO,
                            attacker_id: ray.attacker_id,
                            target_id: veh.entity_id,
                        });
                    } else {
                        veh.damage.radio.state = DeviceState::Damaged;
                        events.push(DamageInfoEvent {
                            code: DamageInfoCode::DeviceCriticalAtShot,
                            extra_index: EXTRA_RADIO,
                            attacker_id: ray.attacker_id,
                            target_id: veh.entity_id,
                        });
                    }
                }
            }
            "surveyingDevice" | "surveyingDeviceHealth" => {
                let roll: f32 = rng.gen();
                if roll <= 0.45 {
                    veh.damage.surveying_device.health = (veh.damage.surveying_device.health - damage_val).max(0);
                    if veh.damage.surveying_device.health <= 0 {
                        veh.damage.surveying_device.state = DeviceState::Destroyed;
                        events.push(DamageInfoEvent {
                            code: DamageInfoCode::DeviceDestroyedAtShot,
                            extra_index: EXTRA_SURVEYING_DEVICE,
                            attacker_id: ray.attacker_id,
                            target_id: veh.entity_id,
                        });
                    } else {
                        veh.damage.surveying_device.state = DeviceState::Damaged;
                        events.push(DamageInfoEvent {
                            code: DamageInfoCode::DeviceCriticalAtShot,
                            extra_index: EXTRA_SURVEYING_DEVICE,
                            attacker_id: ray.attacker_id,
                            target_id: veh.entity_id,
                        });
                    }
                }
            }
            "commander" | "commanderHealth" => {
                let roll: f32 = rng.gen();
                if roll <= 0.33 {
                    if let Some(t) = veh.damage.tankmen.iter_mut().find(|t| t.role == "commander") {
                        t.is_alive = false;
                        events.push(DamageInfoEvent {
                            code: DamageInfoCode::TankmanHitAtShot,
                            extra_index: EXTRA_COMMANDER,
                            attacker_id: ray.attacker_id,
                            target_id: veh.entity_id,
                        });
                    }
                }
            }
            "driver" | "driverHealth" => {
                let roll: f32 = rng.gen();
                if roll <= 0.33 {
                    if let Some(t) = veh.damage.tankmen.iter_mut().find(|t| t.role == "driver") {
                        t.is_alive = false;
                        events.push(DamageInfoEvent {
                            code: DamageInfoCode::TankmanHitAtShot,
                            extra_index: EXTRA_DRIVER,
                            attacker_id: ray.attacker_id,
                            target_id: veh.entity_id,
                        });
                    }
                }
            }
            "gunner" | "gunner_1" | "gunner1" | "gunner1Health" => {
                let roll: f32 = rng.gen();
                if roll <= 0.33 {
                    if let Some(t) = veh.damage.tankmen.iter_mut().find(|t| t.role == "gunner" && t.index_in_role == 1) {
                        t.is_alive = false;
                        events.push(DamageInfoEvent {
                            code: DamageInfoCode::TankmanHitAtShot,
                            extra_index: EXTRA_GUNNER1,
                            attacker_id: ray.attacker_id,
                            target_id: veh.entity_id,
                        });
                    }
                }
            }
            "gunner_2" | "gunner2" | "gunner2Health" => {
                let roll: f32 = rng.gen();
                if roll <= 0.33 {
                    if let Some(t) = veh.damage.tankmen.iter_mut().find(|t| t.role == "gunner" && t.index_in_role == 2) {
                        t.is_alive = false;
                        events.push(DamageInfoEvent {
                            code: DamageInfoCode::TankmanHitAtShot,
                            extra_index: EXTRA_GUNNER2,
                            attacker_id: ray.attacker_id,
                            target_id: veh.entity_id,
                        });
                    }
                }
            }
            "loader" | "loader_1" | "loader1" | "loader1Health" => {
                let roll: f32 = rng.gen();
                if roll <= 0.33 {
                    if let Some(t) = veh.damage.tankmen.iter_mut().find(|t| t.role == "loader" && t.index_in_role == 1) {
                        t.is_alive = false;
                        events.push(DamageInfoEvent {
                            code: DamageInfoCode::TankmanHitAtShot,
                            extra_index: EXTRA_LOADER1,
                            attacker_id: ray.attacker_id,
                            target_id: veh.entity_id,
                        });
                    }
                }
            }
            "loader_2" | "loader2" | "loader2Health" => {
                let roll: f32 = rng.gen();
                if roll <= 0.33 {
                    if let Some(t) = veh.damage.tankmen.iter_mut().find(|t| t.role == "loader" && t.index_in_role == 2) {
                        t.is_alive = false;
                        events.push(DamageInfoEvent {
                            code: DamageInfoCode::TankmanHitAtShot,
                            extra_index: EXTRA_LOADER2,
                            attacker_id: ray.attacker_id,
                            target_id: veh.entity_id,
                        });
                    }
                }
            }
            "radioman" | "radioman_1" | "radioman1" | "radioman1Health" => {
                let roll: f32 = rng.gen();
                if roll <= 0.33 {
                    if let Some(t) = veh.damage.tankmen.iter_mut().find(|t| t.role == "radioman" && t.index_in_role == 1) {
                        t.is_alive = false;
                        events.push(DamageInfoEvent {
                            code: DamageInfoCode::TankmanHitAtShot,
                            extra_index: EXTRA_RADIOMAN1,
                            attacker_id: ray.attacker_id,
                            target_id: veh.entity_id,
                        });
                    }
                }
            }
            "radioman_2" | "radioman2" | "radioman2Health" => {
                let roll: f32 = rng.gen();
                if roll <= 0.33 {
                    if let Some(t) = veh.damage.tankmen.iter_mut().find(|t| t.role == "radioman" && t.index_in_role == 2) {
                        t.is_alive = false;
                        events.push(DamageInfoEvent {
                            code: DamageInfoCode::TankmanHitAtShot,
                            extra_index: EXTRA_RADIOMAN2,
                            attacker_id: ray.attacker_id,
                            target_id: veh.entity_id,
                        });
                    }
                }
            }
            _ => {}
        }

        // Check if all crew are now dead
        if veh.damage.tankmen.iter().all(|t| !t.is_alive) {
            veh.damage.is_crew_active = false;
            veh.is_alive = false;
            events.push(DamageInfoEvent {
                code: DamageInfoCode::DeathFromInactiveCrewAtShot,
                extra_index: EXTRA_NONE,
                attacker_id: ray.attacker_id,
                target_id: veh.entity_id,
            });
            return events;
        }
    }

    events
}

/// Apply external HE splash damage to tracks and gun within `explosion_radius`.
pub fn apply_external_splash_damage(
    veh: &mut VehicleSimState,
    splash_center: Vec3,
    explosion_radius: f32,
    devices_damage: f32,
    attacker_id: i32,
) -> Vec<DamageInfoEvent> {
    let mut events = Vec::new();
    if explosion_radius <= 0.0 || devices_damage <= 0.0 {
        return events;
    }

    // Vehicle approximate module positions relative to physics position
    let veh_pos = veh.physics.pos;
    let yaw = veh.physics.yaw;
    let right = Vec3::new(yaw.cos(), 0.0, -yaw.sin());
    let forward = Vec3::new(yaw.sin(), 0.0, yaw.cos());

    let left_track_pos = veh_pos - right * veh.track_center_offset;
    let right_track_pos = veh_pos + right * veh.track_center_offset;
    let gun_pos = veh_pos + forward * 2.0 + Vec3::new(0.0, 1.5, 0.0);

    let targets = [
        (left_track_pos, EXTRA_LEFT_TRACK),
        (right_track_pos, EXTRA_RIGHT_TRACK),
        (gun_pos, EXTRA_GUN),
    ];

    for (pos, extra) in targets {
        let dist = (pos - splash_center).length();
        if dist <= explosion_radius {
            let falloff = 1.0 - (dist / explosion_radius);
            let dmg = (devices_damage * falloff).round() as i32;
            if dmg <= 0 {
                continue;
            }

            match extra {
                EXTRA_LEFT_TRACK => {
                    veh.damage.left_track.health = (veh.damage.left_track.health - dmg).max(0);
                    if veh.damage.left_track.health <= 0 {
                        veh.damage.left_track.state = DeviceState::Destroyed;
                        veh.physics.left_track_broken = true;
                        veh.damage.public_state_modifiers.insert(EXTRA_LEFT_TRACK);
                        events.push(DamageInfoEvent {
                            code: DamageInfoCode::DeviceDestroyedAtShot,
                            extra_index: EXTRA_LEFT_TRACK,
                            attacker_id,
                            target_id: veh.entity_id,
                        });
                    } else if veh.damage.left_track.state == DeviceState::Normal {
                        veh.damage.left_track.state = DeviceState::Damaged;
                        events.push(DamageInfoEvent {
                            code: DamageInfoCode::DeviceCriticalAtShot,
                            extra_index: EXTRA_LEFT_TRACK,
                            attacker_id,
                            target_id: veh.entity_id,
                        });
                    }
                }
                EXTRA_RIGHT_TRACK => {
                    veh.damage.right_track.health = (veh.damage.right_track.health - dmg).max(0);
                    if veh.damage.right_track.health <= 0 {
                        veh.damage.right_track.state = DeviceState::Destroyed;
                        veh.physics.right_track_broken = true;
                        veh.damage.public_state_modifiers.insert(EXTRA_RIGHT_TRACK);
                        events.push(DamageInfoEvent {
                            code: DamageInfoCode::DeviceDestroyedAtShot,
                            extra_index: EXTRA_RIGHT_TRACK,
                            attacker_id,
                            target_id: veh.entity_id,
                        });
                    } else if veh.damage.right_track.state == DeviceState::Normal {
                        veh.damage.right_track.state = DeviceState::Damaged;
                        events.push(DamageInfoEvent {
                            code: DamageInfoCode::DeviceCriticalAtShot,
                            extra_index: EXTRA_RIGHT_TRACK,
                            attacker_id,
                            target_id: veh.entity_id,
                        });
                    }
                }
                EXTRA_GUN => {
                    veh.damage.gun.health = (veh.damage.gun.health - dmg).max(0);
                    if veh.damage.gun.health <= 0 {
                        veh.damage.gun.state = DeviceState::Destroyed;
                        events.push(DamageInfoEvent {
                            code: DamageInfoCode::DeviceDestroyedAtShot,
                            extra_index: EXTRA_GUN,
                            attacker_id,
                            target_id: veh.entity_id,
                        });
                    } else if veh.damage.gun.state == DeviceState::Normal {
                        veh.damage.gun.state = DeviceState::Damaged;
                        events.push(DamageInfoEvent {
                            code: DamageInfoCode::DeviceCriticalAtShot,
                            extra_index: EXTRA_GUN,
                            attacker_id,
                            target_id: veh.entity_id,
                        });
                    }
                }
                _ => {}
            }
        }
    }

    events
}

#[cfg(test)]
mod tests {
    use super::*;
    use glam::Vec3;

    #[test]
    fn rammer_ventilation_optics_modify_reload_and_vision() {
        let descr = VehicleDescr {
            nation_id: 0,
            type_id: 1,
            chassis_id: 1,
            engine_id: 1,
            fuel_tank_id: 1,
            radio_id: 1,
            turret_id: 1,
            gun_id: 1,
            optional_devices: [Some(21), Some(17), Some(5)], // tankRammer (21), improvedVentilation (17), coatedOptics (5)
            custom_emblem_id: None,
            horn_id: None,
            camouflage_id: None,
        };
        let crew = vec![
            TankmanDescr {
                nation_id: 0,
                vehicle_type_id: 1,
                role_id: 1, // commander
                role_level: 100,
                skills: vec![],
                last_skill_level: 100,
                is_female: false,
                is_premium: false,
                first_name_id: 0,
                last_name_id: 0,
                icon_id: 0,
                rank_idx: 0,
                num_levels_to_next_rank: 0,
                free_xp: 0,
                dossier: vec![],
            },
            TankmanDescr {
                nation_id: 0,
                vehicle_type_id: 1,
                role_id: 4, // gunner
                role_level: 100,
                skills: vec![],
                last_skill_level: 100,
                is_female: false,
                is_premium: false,
                first_name_id: 0,
                last_name_id: 0,
                icon_id: 0,
                rank_idx: 0,
                num_levels_to_next_rank: 0,
                free_xp: 0,
                dossier: vec![],
            },
        ];
        let dmg = VehicleDamageState::default();

        let mods = compute_effective_modifiers(&descr, &crew, &dmg);

        // tankRammer (0.9) and ventilation (+5% to loader: 110 -> 115.5, factor 1/1.05)
        let expected_reload = 0.9 * (110.0 / 115.5);
        assert!(
            (mods.gun_reload_time_factor - expected_reload).abs() < 1e-3,
            "expected gun_reload_time_factor {}, got {}",
            expected_reload,
            mods.gun_reload_time_factor
        );
        // coatedOptics (1.1) and ventilation (+5% to commander: 100 -> 105, factor 1.05)
        let expected_vision = 1.1 * 1.05;
        assert!(
            (mods.vision_radius_factor - expected_vision).abs() < 1e-3,
            "expected vision_radius_factor {}, got {}",
            expected_vision,
            mods.vision_radius_factor
        );

        // improvedVentilation increases effective commander level by +5% (100 -> 105)
        let eff_cmd = effective_crew_skill(1, &descr, &crew, &dmg);
        assert_eq!(eff_cmd, 105.0, "commander gets +5% from ventilation");

        // Gunner gets +5% from ventilation + 10% commander bonus (10.5%) = 115.5
        let eff_gunner = effective_crew_skill(4, &descr, &crew, &dmg);
        assert!((eff_gunner - 115.5).abs() < 1e-3, "gunner gets ventilation + cmd bonus");
    }

    #[test]
    fn track_destruction_and_repair_kit() {
        let mut veh = VehicleSimState {
            entity_id: 101,
            team: 1,
            is_alive: true,
            physics: VehiclePhysicsState {
                pos: Vec3::ZERO,
                left_track_broken: false,
                right_track_broken: false,
                ..Default::default()
            },
            damage: VehicleDamageState::default(),
            modifiers: EffectiveModifiers::default(),
            mass_kg: 30_000.0,
            engine_power_hp: 400.0,
            speed_limits: (10.0, 4.0),
            max_climb_angle_deg: 25.0,
            rotation_speed_rad: 0.5,
            rotation_is_around_center: true,
            track_center_offset: 1.5,
            default_weight: 30_000.0,
            tags: ["mediumTank".to_string()].into_iter().collect(),
            ..Default::default()
        };

        // Break left track
        veh.damage.left_track.health = 0;
        veh.damage.left_track.state = DeviceState::Destroyed;
        veh.physics.left_track_broken = true;
        veh.damage.public_state_modifiers.insert(EXTRA_LEFT_TRACK);

        // Add smallRepairkit to slot 0
        veh.damage.equipment_slots[0] = Some(EquipmentSlot {
            comp_descr: 1234,
            id: 4, // smallRepairkit
            name: "smallRepairkit".to_string(),
            count: 1,
            cooldown_left: 0.0,
            is_active: false,
        });

        // Activating repair kit repairs track immediately
        let events = activate_equipment(&mut veh, 4, EXTRA_LEFT_TRACK as u16).expect("activate repairkit");
        assert_eq!(veh.damage.left_track.state, DeviceState::Normal);
        assert_eq!(veh.damage.left_track.health, veh.damage.left_track.max_health);
        assert!(!veh.physics.left_track_broken, "left track broken must be false after repair");
        assert_eq!(veh.damage.equipment_slots[0].as_ref().unwrap().count, 0, "quantity consumed");
        assert!(events.iter().any(|e| e.code == DamageInfoCode::DeviceRepaired));

        // Test auto-repair timer progression with Toolbox (1.25x)
        let balance = BalanceConfig::default();
        let mut veh_auto = VehicleSimState {
            entity_id: 105,
            team: 1,
            is_alive: true,
            physics: VehiclePhysicsState {
                pos: Vec3::ZERO,
                left_track_broken: true,
                ..Default::default()
            },
            damage: VehicleDamageState {
                left_track: DeviceDamageState::new(100, 50, 0.0),
                ..Default::default()
            },
            modifiers: EffectiveModifiers {
                repair_speed_factor: 1.25, // Toolbox equipped
                ..Default::default()
            },
            ..Default::default()
        };
        veh_auto.damage.left_track.health = 0;
        veh_auto.damage.left_track.state = DeviceState::Destroyed;

        // Step 4.0 seconds (4.0 * 12.5 * 1.25 = 62.5 >= max_regen_health 50)
        let events_auto = step_damage_state(&mut veh_auto, &balance, 4.0);
        assert_eq!(veh_auto.damage.left_track.state, DeviceState::Damaged);
        assert_eq!(veh_auto.damage.left_track.health, 50);
        assert!(!veh_auto.physics.left_track_broken, "left track repaired to critical enables movement");
        assert!(events_auto.iter().any(|e| e.code == DamageInfoCode::DeviceRepairedToCritical));
    }

    #[test]
    fn engine_fire_burn_and_extinguisher() {
        let mut veh = VehicleSimState {
            entity_id: 102,
            team: 1,
            is_alive: true,
            physics: VehiclePhysicsState::default(),
            damage: VehicleDamageState::default(),
            modifiers: EffectiveModifiers::default(),
            mass_kg: 30_000.0,
            engine_power_hp: 400.0,
            speed_limits: (10.0, 4.0),
            max_climb_angle_deg: 25.0,
            rotation_speed_rad: 0.5,
            rotation_is_around_center: true,
            track_center_offset: 1.5,
            default_weight: 30_000.0,
            tags: ["mediumTank".to_string()].into_iter().collect(),
            ..Default::default()
        };

        let balance = BalanceConfig::default();

        // Start fire on vehicle
        veh.damage.fire_state = Some(ActiveFire {
            attacker_id: 99,
            burn_damage_per_sec: 50.0,
            elapsed_secs: 0.0,
            duration_secs: 10.0,
            tick_accumulator: 0.0,
        });
        veh.damage.public_state_modifiers.insert(EXTRA_FIRE);

        // Add handExtinguishers to slot 0
        veh.damage.equipment_slots[0] = Some(EquipmentSlot {
            comp_descr: 5678,
            id: 0, // handExtinguishers
            name: "handExtinguishers".to_string(),
            count: 1,
            cooldown_left: 0.0,
            is_active: false,
        });

        // Step one tick with fire burning
        let _ = step_damage_state(&mut veh, &balance, 0.1);
        assert!(veh.damage.fire_state.is_some());

        // Use extinguisher
        let events_ext = activate_equipment(&mut veh, 0, 0).expect("activate extinguisher");
        assert!(veh.damage.fire_state.is_none(), "fire must be stopped");
        assert!(!veh.damage.public_state_modifiers.contains(&EXTRA_FIRE));
        assert!(events_ext.iter().any(|e| e.code == DamageInfoCode::FireStopped));
    }

    #[test]
    fn ammo_rack_detonation_and_crew_knockout() {
        let mut veh = VehicleSimState {
            entity_id: 103,
            team: 1,
            is_alive: true,
            physics: VehiclePhysicsState::default(),
            damage: VehicleDamageState::default(),
            modifiers: EffectiveModifiers::default(),
            mass_kg: 30_000.0,
            engine_power_hp: 400.0,
            speed_limits: (10.0, 4.0),
            max_climb_angle_deg: 25.0,
            rotation_speed_rad: 0.5,
            rotation_is_around_center: true,
            track_center_offset: 1.5,
            default_weight: 30_000.0,
            tags: ["mediumTank".to_string()].into_iter().collect(),
            ..Default::default()
        };
        let balance = BalanceConfig::default();

        // 1. Zeroing ammoBay triggers instant kill
        veh.damage.ammo_bay.health = 0;
        veh.damage.ammo_bay.state = DeviceState::Destroyed;
        let events = step_damage_state(&mut veh, &balance, 0.1);
        assert!(!veh.is_alive, "zero ammo bay kills vehicle");
        assert!(events.iter().any(|e| e.code == DamageInfoCode::DeathFromDeviceExplosionAtShot));

        // 2. Knocking out all living crew triggers inactive crew kill
        let mut veh2 = VehicleSimState {
            entity_id: 104,
            team: 1,
            is_alive: true,
            physics: VehiclePhysicsState::default(),
            damage: VehicleDamageState::default(),
            modifiers: EffectiveModifiers::default(),
            mass_kg: 30_000.0,
            engine_power_hp: 400.0,
            speed_limits: (10.0, 4.0),
            max_climb_angle_deg: 25.0,
            rotation_speed_rad: 0.5,
            rotation_is_around_center: true,
            track_center_offset: 1.5,
            default_weight: 30_000.0,
            tags: ["mediumTank".to_string()].into_iter().collect(),
            ..Default::default()
        };
        for t in &mut veh2.damage.tankmen {
            t.is_alive = false;
        }
        let events2 = step_damage_state(&mut veh2, &balance, 0.1);
        assert!(!veh2.is_alive, "all crew dead kills vehicle");
        assert!(!veh2.damage.is_crew_active, "crew is inactive");
        assert!(events2.iter().any(|e| e.code == DamageInfoCode::DeathFromInactiveCrewAtShot));
    }

    fn make_test_collision_set() -> VehicleCollisionSet {
        let bsp_bytes = {
            let mut bytes = Vec::new();
            bytes.extend_from_slice(crate::assets::bsp2::BSP2_MAGIC);
            bytes.extend_from_slice(&5u32.to_le_bytes()); // 5 triangles
            bytes.extend_from_slice(&1u32.to_le_bytes()); // 1 node
            bytes.extend_from_slice(&0u32.to_le_bytes());

            // Tri 0 at x = 2.0: material_index 0 ("armor_1")
            for v in &[[2.0f32, -1.0, -1.0], [2.0, 1.0, -1.0], [2.0, 0.0, 1.0]] {
                for c in v { bytes.extend_from_slice(&c.to_le_bytes()); }
            }
            bytes.extend_from_slice(&0u32.to_le_bytes());

            // Tri 1 at x = 1.0: material_index 1 ("ammoBay")
            for v in &[[1.0f32, -1.0, -1.0], [1.0, 1.0, -1.0], [1.0, 0.0, 1.0]] {
                for c in v { bytes.extend_from_slice(&c.to_le_bytes()); }
            }
            bytes.extend_from_slice(&1u32.to_le_bytes());

            // Tri 2 at x = 0.5: material_index 2 ("driver")
            for v in &[[0.5f32, -1.0, -1.0], [0.5, 1.0, -1.0], [0.5, 0.0, 1.0]] {
                for c in v { bytes.extend_from_slice(&c.to_le_bytes()); }
            }
            bytes.extend_from_slice(&2u32.to_le_bytes());

            // Tri 3 at x = 0.0: material_index 3 ("gunner_2")
            for v in &[[0.0f32, -1.0, -1.0], [0.0, 1.0, -1.0], [0.0, 0.0, 1.0]] {
                for c in v { bytes.extend_from_slice(&c.to_le_bytes()); }
            }
            bytes.extend_from_slice(&3u32.to_le_bytes());

            // Tri 4 at x = -0.5: material_index 4 ("loader_2")
            for v in &[[-0.5f32, -1.0, -1.0], [-0.5, 1.0, -1.0], [-0.5, 0.0, 1.0]] {
                for c in v { bytes.extend_from_slice(&c.to_le_bytes()); }
            }
            bytes.extend_from_slice(&4u32.to_le_bytes());

            // Leaf node pointing to all 5 triangles
            bytes.push(0xa0);
            for c in &[1.0f32, 0.0, 0.0] { bytes.extend_from_slice(&c.to_le_bytes()); }
            bytes.extend_from_slice(&1.0f32.to_le_bytes());
            bytes.extend_from_slice(&5u16.to_le_bytes()); // 5 triangles in leaf
            for i in 0..5u16 {
                bytes.extend_from_slice(&i.to_le_bytes());
            }
            bytes
        };
        let bsp_mats = b"<mats><id>armor_1</id><id>ammoBay</id><id>driver</id><id>gunner_2</id><id>loader_2</id></mats>";
        let bsp_tree = std::sync::Arc::new(crate::assets::bsp2::BspTree::parse(&bsp_bytes, bsp_mats).unwrap());
        let hit_tester = crate::assets::map_store::HitTester {
            model_path: "hull.model".into(),
            bsp: bsp_tree,
            enclosing_box: Some((Vec3::new(-3.0, -2.0, -2.0), Vec3::new(3.0, 2.0, 2.0))),
        };

        let empty_bsp_bytes = {
            let mut bytes = Vec::new();
            bytes.extend_from_slice(crate::assets::bsp2::BSP2_MAGIC);
            bytes.extend_from_slice(&0u32.to_le_bytes());
            bytes.extend_from_slice(&0u32.to_le_bytes());
            bytes.extend_from_slice(&0u32.to_le_bytes());
            bytes
        };
        let empty_bsp = std::sync::Arc::new(crate::assets::bsp2::BspTree::parse(&empty_bsp_bytes, b"").unwrap());
        let empty_hit_tester = crate::assets::map_store::HitTester {
            model_path: "empty.model".into(),
            bsp: empty_bsp,
            enclosing_box: None,
        };

        VehicleCollisionSet {
            hull: hit_tester,
            chassis: empty_hit_tester.clone(),
            turret: empty_hit_tester.clone(),
            gun: empty_hit_tester,
        }
    }

    #[test]
    fn apply_internal_crit_ray_steps_through_internal_materials() {
        let col_set = make_test_collision_set();

        let mut veh = VehicleSimState {
            entity_id: 201,
            team: 1,
            is_alive: true,
            physics: VehiclePhysicsState::default(),
            damage: VehicleDamageState::default(),
            modifiers: EffectiveModifiers::default(),
            ..Default::default()
        };

        let ray = InternalCritRay {
            world_origin: Vec3::new(2.5, 0.0, 0.0),
            world_dir: Vec3::new(-1.0, 0.0, 0.0),
            local_origin: Vec3::new(2.5, 0.0, 0.0),
            local_dir: Vec3::new(-1.0, 0.0, 0.0),
            max_depth: 5.0,
            vehicle_id: 201,
            attacker_id: 100,
            component_idx: COMPONENT_HULL,
            devices_damage: 200.0, // enough to destroy ammoBay
            caliber: 0.100,
        };

        let balance = BalanceConfig::default();
        // Deterministic RNG that rolls 0.0 (always passes saving throw)
        struct ZeroRng;
        impl rand::RngCore for ZeroRng {
            fn next_u32(&mut self) -> u32 { 0 }
            fn next_u64(&mut self) -> u64 { 0 }
            fn fill_bytes(&mut self, dest: &mut [u8]) { dest.fill(0); }
            fn try_fill_bytes(&mut self, dest: &mut [u8]) -> Result<(), rand::Error> { dest.fill(0); Ok(()) }
        }

        let events = apply_internal_crit_ray(&ray, &mut veh, Some(&col_set), &balance, &mut ZeroRng);
        // Armor tri is ignored; ammoBay is destroyed with 200 damage -> instant explosion!
        assert!(!veh.is_alive, "instant explosion kills vehicle");
        assert!(events.iter().any(|e| e.code == DamageInfoCode::DeathFromDeviceExplosionAtShot));
    }

    #[test]
    fn removed_rpm_limiter_damages_engine_over_time() {
        let mut veh = VehicleSimState {
            entity_id: 202,
            team: 1,
            is_alive: true,
            physics: VehiclePhysicsState::default(),
            damage: VehicleDamageState {
                engine: DeviceDamageState::new(100, 50, 0.0),
                ..Default::default()
            },
            modifiers: EffectiveModifiers::default(),
            ..Default::default()
        };

        veh.damage.equipment_slots[0] = Some(EquipmentSlot {
            comp_descr: 999,
            id: 12, // removedRpmLimiter
            name: "removedRpmLimiter".to_string(),
            count: 1,
            cooldown_left: 0.0,
            is_active: false,
        });

        // Toggle on
        let _ = activate_equipment(&mut veh, 12, 1).expect("toggle rpm limiter");
        assert!(veh.damage.equipment_slots[0].as_ref().unwrap().is_active);

        let balance = BalanceConfig::default();
        // Advance 30 seconds (30 * 2.0 = 60 damage -> health drops from 100 to 40 < max_regen_health 50)
        let events = step_damage_state(&mut veh, &balance, 30.0);
        assert_eq!(veh.damage.engine.state, DeviceState::Damaged);
        assert!(events.iter().any(|e| e.code == DamageInfoCode::EngineCriticalAtUnlimitedRpm));

        // Advance another 25 seconds (25 * 2.0 = 50 damage -> health drops to 0)
        let events_dest = step_damage_state(&mut veh, &balance, 25.0);
        assert_eq!(veh.damage.engine.state, DeviceState::Destroyed);
        assert!(events_dest.iter().any(|e| e.code == DamageInfoCode::EngineDestroyedAtUnlimitedRpm));
    }

    #[test]
    fn medkit_restores_knocked_out_crew() {
        let mut veh = VehicleSimState {
            entity_id: 203,
            team: 1,
            is_alive: true,
            physics: VehiclePhysicsState::default(),
            damage: VehicleDamageState::default(),
            modifiers: EffectiveModifiers::default(),
            ..Default::default()
        };

        // Knock out driver
        veh.damage.tankmen[1].is_alive = false;

        // Equip smallMedkit
        veh.damage.equipment_slots[0] = Some(EquipmentSlot {
            comp_descr: 222,
            id: 2, // smallMedkit
            name: "smallMedkit".to_string(),
            count: 1,
            cooldown_left: 0.0,
            is_active: false,
        });

        let events = activate_equipment(&mut veh, 2, EXTRA_DRIVER as u16).expect("heal driver");
        assert!(veh.damage.tankmen[1].is_alive, "driver must be alive");
        assert_eq!(veh.damage.equipment_slots[0].as_ref().unwrap().count, 0);
        assert!(events.iter().any(|e| e.code == DamageInfoCode::TankmanRestored));
    }

    #[test]
    fn auto_extinguisher_automatically_stops_fire() {
        let mut veh = VehicleSimState {
            entity_id: 204,
            team: 1,
            is_alive: true,
            physics: VehiclePhysicsState::default(),
            damage: VehicleDamageState::default(),
            modifiers: EffectiveModifiers::default(),
            ..Default::default()
        };

        // Equip autoExtinguishers
        veh.damage.equipment_slots[0] = Some(EquipmentSlot {
            comp_descr: 111,
            id: 1, // autoExtinguishers
            name: "autoExtinguishers".to_string(),
            count: 1,
            cooldown_left: 0.0,
            is_active: false,
        });

        // Start fire
        veh.damage.fire_state = Some(ActiveFire {
            attacker_id: 42,
            burn_damage_per_sec: 50.0,
            elapsed_secs: 0.0,
            duration_secs: 10.0,
            tick_accumulator: 0.0,
        });
        veh.damage.public_state_modifiers.insert(EXTRA_FIRE);

        let balance = BalanceConfig::default();
        let events = step_damage_state(&mut veh, &balance, 0.1);

        assert!(veh.damage.fire_state.is_none(), "auto extinguisher stops fire");
        assert!(!veh.damage.public_state_modifiers.contains(&EXTRA_FIRE));
        assert_eq!(veh.damage.equipment_slots[0].as_ref().unwrap().count, 0, "consumed 1 charge");
        assert!(events.iter().any(|e| e.code == DamageInfoCode::FireStopped));
    }

    #[test]
    fn stationary_activates_stereoscope_and_camo_net() {
        let mut veh = VehicleSimState {
            entity_id: 205,
            team: 1,
            is_alive: true,
            descr: Some(VehicleDescr {
                nation_id: 0,
                type_id: 1,
                chassis_id: 1,
                engine_id: 1,
                fuel_tank_id: 1,
                radio_id: 1,
                turret_id: 1,
                gun_id: 1,
                optional_devices: [Some(4), Some(6), None], // stereoscope (4), camoNet (6)
                custom_emblem_id: None,
                horn_id: None,
                camouflage_id: None,
            }),
            physics: VehiclePhysicsState {
                speed_mps: 0.0,
                rot_speed_rps: 0.0,
                ..Default::default()
            },
            damage: VehicleDamageState::default(),
            modifiers: EffectiveModifiers::default(),
            ..Default::default()
        };

        let balance = BalanceConfig::default();
        // Sitting still for 3.1 seconds
        let _ = step_damage_state(&mut veh, &balance, 3.1);
        assert!(veh.damage.public_state_modifiers.contains(&EXTRA_STEREOSCOPE));
        assert!(veh.damage.public_state_modifiers.contains(&EXTRA_CAMOUFLAGE_NET));

        // Moving resets stationary timer and clears modifiers
        veh.physics.speed_mps = 5.0;
        let _ = step_damage_state(&mut veh, &balance, 0.1);
        assert!(!veh.damage.public_state_modifiers.contains(&EXTRA_STEREOSCOPE));
        assert!(!veh.damage.public_state_modifiers.contains(&EXTRA_CAMOUFLAGE_NET));
        assert_eq!(veh.damage.stationary_time, 0.0);
    }

    #[test]
    fn test_activate_equipment_preserves_optional_devices_and_crew() {
        let descr = VehicleDescr {
            nation_id: 0,
            type_id: 1,
            chassis_id: 1,
            engine_id: 1,
            fuel_tank_id: 1,
            radio_id: 1,
            turret_id: 1,
            gun_id: 1,
            optional_devices: [Some(21), Some(5), None], // rammer (21), optics (5)
            custom_emblem_id: None,
            horn_id: None,
            camouflage_id: None,
        };
        let mut veh = VehicleSimState {
            entity_id: 101,
            team: 1,
            is_alive: true,
            descr: Some(descr),
            physics: VehiclePhysicsState {
                left_track_broken: true,
                ..Default::default()
            },
            damage: VehicleDamageState {
                left_track: DeviceDamageState {
                    health: 0,
                    max_health: 100,
                    max_regen_health: 50,
                    repair_cost: 0.0,
                    state: DeviceState::Destroyed,
                    repair_progress: 0.0,
                },
                equipment_slots: [
                    Some(EquipmentSlot {
                        comp_descr: 1,
                        id: 4, // smallRepairkit
                        name: "smallRepairkit".to_string(),
                        count: 1,
                        cooldown_left: 0.0,
                        is_active: false,
                    }),
                    None,
                    None,
                ],
                ..Default::default()
            },
            ..Default::default()
        };

        let res = activate_equipment(&mut veh, 4, EXTRA_LEFT_TRACK as u16);
        assert!(res.is_ok());
        // Verify modifiers: rammer should reduce reload (<= 0.90) and optics should increase vision (>= 1.10)
        assert!(
            veh.modifiers.gun_reload_time_factor <= 0.90 + 1e-4,
            "gun_reload_time_factor should preserve rammer (<=0.90), got {}",
            veh.modifiers.gun_reload_time_factor
        );
        assert!(
            veh.modifiers.vision_radius_factor >= 1.10 - 1e-4,
            "vision_radius_factor should preserve optics (>=1.10), got {}",
            veh.modifiers.vision_radius_factor
        );
    }

    #[test]
    fn test_rpm_limiter_10hz_ticks_damage_engine() {
        let mut veh = VehicleSimState {
            entity_id: 101,
            team: 1,
            is_alive: true,
            damage: VehicleDamageState {
                engine: DeviceDamageState::new(100, 50, 0.0),
                equipment_slots: [
                    Some(EquipmentSlot {
                        comp_descr: 12,
                        id: 12,
                        name: "removedRpmLimiter".to_string(),
                        count: 1,
                        cooldown_left: 0.0,
                        is_active: true,
                    }),
                    None,
                    None,
                ],
                ..Default::default()
            },
            ..Default::default()
        };
        let balance = BalanceConfig::default();

        // 100 ticks at 10 Hz (0.1s each = 10.0s)
        for _ in 0..100 {
            step_damage_state(&mut veh, &balance, 0.1);
        }

        // At 1.5 - 2.0 dmg/sec, 10s should inflict 15 - 20 damage!
        assert!(
            veh.damage.engine.health <= 85,
            "engine health should have decreased from 100 to <= 85, got {}",
            veh.damage.engine.health
        );
    }

    #[test]
    fn test_internal_crit_ray_attacker_attribution() {
        let col_set = make_test_collision_set();
        let mut veh = VehicleSimState {
            entity_id: 201,
            team: 2,
            is_alive: true,
            damage: VehicleDamageState::default(),
            ..Default::default()
        };
        let ray = InternalCritRay {
            world_origin: Vec3::new(2.5, 0.0, 0.0),
            world_dir: Vec3::new(-1.0, 0.0, 0.0),
            local_origin: Vec3::new(2.5, 0.0, 0.0),
            local_dir: Vec3::new(-1.0, 0.0, 0.0),
            max_depth: 5.0,
            vehicle_id: 201, // victim
            attacker_id: 999, // shooter
            component_idx: COMPONENT_HULL,
            devices_damage: 200.0,
            caliber: 0.100,
        };
        let balance = BalanceConfig::default();
        struct ZeroRng;
        impl rand::RngCore for ZeroRng {
            fn next_u32(&mut self) -> u32 { 0 }
            fn next_u64(&mut self) -> u64 { 0 }
            fn fill_bytes(&mut self, dest: &mut [u8]) { dest.fill(0); }
            fn try_fill_bytes(&mut self, _dest: &mut [u8]) -> Result<(), rand::Error> { Ok(()) }
        }

        let mut rng = ZeroRng;
        let events = apply_internal_crit_ray(&ray, &mut veh, Some(&col_set), &balance, &mut rng);
        assert!(!events.is_empty(), "should emit damage events");
        for ev in &events {
            assert_eq!(
                ev.attacker_id, 999,
                "event attacker_id must match shooter (999), got {}",
                ev.attacker_id
            );
        }
    }

    #[test]
    fn test_secondary_crew_crits_gunner2_loader2() {
        let col_set = make_test_collision_set();
        let mut veh = VehicleSimState {
            entity_id: 201,
            team: 2,
            is_alive: true,
            damage: VehicleDamageState {
                tankmen: vec![
                    TankmanDamageState::new("commander", 1, 100),
                    TankmanDamageState::new("driver", 1, 100),
                    TankmanDamageState::new("gunner", 1, 100),
                    TankmanDamageState::new("gunner", 2, 100),
                    TankmanDamageState::new("loader", 1, 100),
                    TankmanDamageState::new("loader", 2, 100),
                ],
                ammo_bay: DeviceDamageState::new(1000, 500, 0.0), // high health so ammo bay survives
                ..Default::default()
            },
            ..Default::default()
        };
        // Ray starts at x=0.2, passing through x=0.0 ("gunner_2") and x=-0.5 ("loader_2")
        let ray = InternalCritRay {
            world_origin: Vec3::new(0.2, 0.0, 0.0),
            world_dir: Vec3::new(-1.0, 0.0, 0.0),
            local_origin: Vec3::new(0.2, 0.0, 0.0),
            local_dir: Vec3::new(-1.0, 0.0, 0.0),
            max_depth: 3.0,
            vehicle_id: 201,
            attacker_id: 55,
            component_idx: COMPONENT_HULL,
            devices_damage: 50.0,
            caliber: 0.100,
        };
        let balance = BalanceConfig::default();
        struct ZeroRng;
        impl rand::RngCore for ZeroRng {
            fn next_u32(&mut self) -> u32 { 0 }
            fn next_u64(&mut self) -> u64 { 0 }
            fn fill_bytes(&mut self, dest: &mut [u8]) { dest.fill(0); }
            fn try_fill_bytes(&mut self, _dest: &mut [u8]) -> Result<(), rand::Error> { Ok(()) }
        }

        let mut rng = ZeroRng;
        let events = apply_internal_crit_ray(&ray, &mut veh, Some(&col_set), &balance, &mut rng);
        assert!(
            events.iter().any(|e| e.code == DamageInfoCode::TankmanHitAtShot && e.extra_index == EXTRA_GUNNER2 && e.attacker_id == 55),
            "expected EXTRA_GUNNER2 hit event with attacker 55, got {:?}",
            events
        );
        assert!(
            events.iter().any(|e| e.code == DamageInfoCode::TankmanHitAtShot && e.extra_index == EXTRA_LOADER2 && e.attacker_id == 55),
            "expected EXTRA_LOADER2 hit event with attacker 55, got {:?}",
            events
        );
        assert!(
            !veh.damage.tankmen.iter().find(|t| t.role == "gunner" && t.index_in_role == 2).unwrap().is_alive,
            "gunner_2 should be dead"
        );
        assert!(
            !veh.damage.tankmen.iter().find(|t| t.role == "loader" && t.index_in_role == 2).unwrap().is_alive,
            "loader_2 should be dead"
        );
    }

    #[test]
    fn test_external_he_splash_damages_tracks_and_gun() {
        let mut veh = VehicleSimState {
            entity_id: 101,
            track_center_offset: 1.5,
            physics: VehiclePhysicsState {
                pos: Vec3::ZERO,
                yaw: 0.0,
                ..Default::default()
            },
            damage: VehicleDamageState {
                left_track: DeviceDamageState::new(100, 50, 0.0),
                right_track: DeviceDamageState::new(100, 50, 0.0),
                gun: DeviceDamageState::new(100, 50, 0.0),
                ..Default::default()
            },
            ..Default::default()
        };

        // Splash right next to left track (-1.5, 0.0, 0.0)
        let splash_pos = Vec3::new(-1.6, 0.0, 0.0);
        let events = apply_external_splash_damage(&mut veh, splash_pos, 2.0, 150.0, 88);

        assert_eq!(veh.damage.left_track.state, DeviceState::Destroyed);
        assert!(veh.physics.left_track_broken);
        assert!(
            events.iter().any(|e| e.code == DamageInfoCode::DeviceDestroyedAtShot && e.extra_index == EXTRA_LEFT_TRACK && e.attacker_id == 88),
            "expected left track destruction event with attacker 88, got {:?}",
            events
        );
    }

    #[test]
    fn test_active_fire_burns_vehicle_hp_and_emits_death_from_fire() {
        let mut veh = VehicleSimState {
            entity_id: 101,
            team: 1,
            is_alive: true,
            health: 50,
            max_health: 50,
            damage: VehicleDamageState {
                fire_state: Some(ActiveFire {
                    attacker_id: 77,
                    burn_damage_per_sec: 30.0,
                    elapsed_secs: 0.0,
                    duration_secs: 10.0,
                    tick_accumulator: 0.0,
                }),
                ..Default::default()
            },
            ..Default::default()
        };
        let balance = BalanceConfig::default();

        // Step for 2 seconds (60 damage > 50 HP)
        let mut events = Vec::new();
        for _ in 0..20 {
            events.extend(step_damage_state(&mut veh, &balance, 0.1));
        }

        assert_eq!(veh.health, 0, "vehicle health should reach 0");
        assert!(!veh.is_alive, "vehicle should be marked dead");
        assert!(
            events.iter().any(|e| e.code == DamageInfoCode::DeathFromFire && e.attacker_id == 77),
            "expected DeathFromFire event with attacker_id 77, got {:?}",
            events
        );
    }

    #[test]
    fn test_damaged_ammo_bay_doubles_reload() {
        let descr = VehicleDescr {
            nation_id: 0,
            type_id: 1,
            chassis_id: 1,
            engine_id: 1,
            fuel_tank_id: 1,
            radio_id: 1,
            turret_id: 1,
            gun_id: 1,
            optional_devices: [None, None, None],
            custom_emblem_id: None,
            horn_id: None,
            camouflage_id: None,
        };
        let mut dmg = VehicleDamageState::default();
        dmg.ammo_bay.state = DeviceState::Damaged;

        let mods = compute_effective_modifiers(&descr, &[], &dmg);
        assert!(
            (mods.gun_reload_time_factor - 2.0).abs() < 1e-3,
            "damaged ammo bay must double reload time factor (2.0), got {}",
            mods.gun_reload_time_factor
        );
    }

    #[test]
    fn test_auto_repair_includes_radio_and_surveying_device() {
        let mut veh = VehicleSimState {
            entity_id: 101,
            team: 1,
            is_alive: true,
            damage: VehicleDamageState {
                radio: DeviceDamageState {
                    health: 0,
                    max_health: 100,
                    max_regen_health: 50,
                    repair_cost: 0.0,
                    state: DeviceState::Destroyed,
                    repair_progress: 0.0,
                },
                surveying_device: DeviceDamageState {
                    health: 0,
                    max_health: 100,
                    max_regen_health: 50,
                    repair_cost: 0.0,
                    state: DeviceState::Destroyed,
                    repair_progress: 0.0,
                },
                ..Default::default()
            },
            ..Default::default()
        };
        let balance = BalanceConfig::default();

        let mut events = Vec::new();
        // 50 HP regen / 12.5 HP/s = 4.0s
        for _ in 0..50 {
            events.extend(step_damage_state(&mut veh, &balance, 0.1));
        }

        assert_eq!(veh.damage.radio.state, DeviceState::Damaged);
        assert_eq!(veh.damage.radio.health, 50);
        assert_eq!(veh.damage.surveying_device.state, DeviceState::Damaged);
        assert_eq!(veh.damage.surveying_device.health, 50);

        assert!(
            events.iter().any(|e| e.code == DamageInfoCode::DeviceRepairedToCritical && e.extra_index == EXTRA_RADIO),
            "expected EXTRA_RADIO repair event"
        );
        assert!(
            events.iter().any(|e| e.code == DamageInfoCode::DeviceRepairedToCritical && e.extra_index == EXTRA_SURVEYING_DEVICE),
            "expected EXTRA_SURVEYING_DEVICE repair event"
        );
    }

    #[test]
    fn test_stationary_extras_require_mounted_devices() {
        let mut veh = VehicleSimState {
            entity_id: 101,
            team: 1,
            is_alive: true,
            descr: Some(VehicleDescr {
                nation_id: 0,
                type_id: 1,
                chassis_id: 1,
                engine_id: 1,
                fuel_tank_id: 1,
                radio_id: 1,
                turret_id: 1,
                gun_id: 1,
                optional_devices: [None, None, None], // NO stereoscope or camo net
                custom_emblem_id: None,
                horn_id: None,
                camouflage_id: None,
            }),
            physics: VehiclePhysicsState {
                speed_mps: 0.0,
                rot_speed_rps: 0.0,
                ..Default::default()
            },
            damage: VehicleDamageState::default(),
            ..Default::default()
        };
        let balance = BalanceConfig::default();

        let _ = step_damage_state(&mut veh, &balance, 3.5);
        assert!(
            !veh.damage.public_state_modifiers.contains(&EXTRA_STEREOSCOPE),
            "should NOT broadcast EXTRA_STEREOSCOPE without stereoscope mounted"
        );
        assert!(
            !veh.damage.public_state_modifiers.contains(&EXTRA_CAMOUFLAGE_NET),
            "should NOT broadcast EXTRA_CAMOUFLAGE_NET without camo net mounted"
        );
    }
}
