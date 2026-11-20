//! WoT 0.6.5 real-time battle simulation (physics, combat, vision, AI).

pub mod ballistics;
pub mod bot_ai;
pub mod capture;
pub mod collision;
pub mod chat;
pub mod crits;
pub mod destructibles;
pub mod physics;
pub mod results;
pub mod tkill;
pub mod visibility;

pub use bot_ai::{ArenaReadView, BotAgentState, BotBehaviorMode, BotController, BotIntent};
pub use ballistics::{
    compute_penetration_and_damage, compute_projectile_trajectory, decode_hit_segment,
    encode_hit_segment, spawn_shot, step_gun_aiming, step_projectiles, AimState, ImpactOutcome,
    InternalCritRay, ProjectileState, TargetingSnapshot, VehicleBallisticsTarget,
    COMPONENT_CHASSIS, COMPONENT_GUN, COMPONENT_HULL, COMPONENT_TURRET, SERVER_EPSILON,
    SERVER_TICK_LENGTH, SHOT_RESULT_ARMOR_NOT_PIERCED, SHOT_RESULT_ARMOR_PIERCED,
    SHOT_RESULT_ARMOR_PIERCED_NO_DAMAGE, SHOT_RESULT_CRITICAL_HIT, SHOT_RESULT_RICOCHET,
};
pub use crits::{
    activate_equipment, apply_external_splash_damage, apply_internal_crit_ray,
    compute_effective_modifiers, step_damage_state, ActiveFire, DamageInfoCode, DamageInfoEvent,
    DeviceDamageState, DeviceState, EquipmentSlot, TankmanDamageState, VehicleDamageState,
    EXTRA_AMMO_BAY, EXTRA_AUTO_EXTINGUISHERS, EXTRA_CAMOUFLAGE_NET, EXTRA_CHOCOLATE,
    EXTRA_COCA_COLA, EXTRA_COMMANDER, EXTRA_DRIVER, EXTRA_ENGINE, EXTRA_FIRE, EXTRA_FUEL_TANK,
    EXTRA_GASOLINE_100, EXTRA_GASOLINE_105, EXTRA_GUN, EXTRA_GUNNER1, EXTRA_GUNNER2,
    EXTRA_GUN_RELOAD, EXTRA_HAND_EXTINGUISHERS, EXTRA_LARGE_MEDKIT, EXTRA_LARGE_REPAIRKIT,
    EXTRA_LEFT_TRACK, EXTRA_LEND_LEASE_OIL, EXTRA_LOADER1, EXTRA_LOADER2, EXTRA_NONE,
    EXTRA_NO_AMMO, EXTRA_RADIO, EXTRA_RADIOMAN1, EXTRA_RADIOMAN2, EXTRA_REMOVED_RPM_LIMITER,
    EXTRA_RIGHT_TRACK, EXTRA_SHOOT, EXTRA_SMALL_MEDKIT, EXTRA_SMALL_REPAIRKIT, EXTRA_STEREOSCOPE,
    EXTRA_SURVEYING_DEVICE, EXTRA_TURRET_ROTATOR,
};
pub use physics::{
    step_vehicle_driving, step_vehicle_driving_with_destructibles, DrivingStepResult,
    EffectiveModifiers, VehiclePhysicsState, VehicleSimState, VehicleSnapshot,
};
