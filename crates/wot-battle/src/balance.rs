//! Server-side gameplay balance configuration (`BalanceConfig`).
//!
//! Retail WoT 0.6.5 client `.xml` defs have server-authoritative gameplay
//! constants stripped (`normalizationAngle`, `ricochetAngle`, `terrainResistance`,
//! `brakeForce`, `healthBurnPerSec`, `xpFactor`, `creditsFactor`, etc.).
//! [`BalanceConfig`] provides retail-aligned defaults and supports optional JSON
//! overrides passed via `--battle-config`.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use serde::{Deserialize, Serialize};
use wot065_data::item_defs::ShellKind;

use crate::assets::AssetError;

/// Gravitational acceleration constant in `m/s^2`.
const GRAVITY_MPS2: f32 = 9.81;

/// Default terrain resistance coefficients `[firm, medium, soft]` by vehicle class.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct TerrainResistanceDefaults {
    /// Light tank multipliers `[firm, medium, soft]` (dimensionless).
    pub light_tank: [f32; 3],
    /// Medium tank multipliers `[firm, medium, soft]` (dimensionless).
    pub medium_tank: [f32; 3],
    /// Heavy tank multipliers `[firm, medium, soft]` (dimensionless).
    pub heavy_tank: [f32; 3],
    /// Tank destroyer (`AT-SPG`) multipliers `[firm, medium, soft]` (dimensionless).
    pub at_spg: [f32; 3],
    /// Self-propelled gun (`SPG`) multipliers `[firm, medium, soft]` (dimensionless).
    pub spg: [f32; 3],
    /// Fallback multipliers `[firm, medium, soft]` if no class tag matches.
    pub fallback: [f32; 3],
}

impl Default for TerrainResistanceDefaults {
    fn default() -> Self {
        Self {
            light_tank: [1.0, 1.1, 1.9],
            medium_tank: [1.0, 1.2, 2.1],
            heavy_tank: [1.1, 1.3, 2.4],
            at_spg: [1.1, 1.3, 2.3],
            spg: [1.2, 1.4, 2.6],
            fallback: [1.1, 1.3, 2.2],
        }
    }
}

/// Matchmaker weight tables from `item_defs/vehicles/common/vehicle.xml`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct MatchmakerBalanceConfig {
    /// Base matchmaker weight indexed by tier `1..=10` (0-based slice index `tier - 1`).
    pub by_component_levels: Vec<f32>,
    /// Multiplier per vehicle class (`lightTank`, `mediumTank`, `heavyTank`, `AT-SPG`, `SPG`).
    pub by_vehicle_classes: BTreeMap<String, f32>,
    /// Component weight multipliers (`vehicle`, `gun`, `turret`, `engine`, `chassis`, `radio`).
    pub modules_weight_multipliers: BTreeMap<String, f32>,
    /// Explicit per-vehicle matchmaker weight overrides keyed by `"<nation>:<name>"`.
    pub by_vehicle_overrides: BTreeMap<String, f32>,
    /// Platoon/squad size weight multipliers.
    pub by_squad_size: Vec<f32>,
}

impl Default for MatchmakerBalanceConfig {
    fn default() -> Self {
        Self {
            by_component_levels: vec![2.0, 3.0, 5.0, 8.0, 12.0, 18.0, 27.0, 40.0, 60.0, 100.0],
            by_vehicle_classes: BTreeMap::from([
                ("lightTank".to_string(), 1.0),
                ("mediumTank".to_string(), 1.0),
                ("heavyTank".to_string(), 1.45),
                ("AT-SPG".to_string(), 1.0),
                ("SPG".to_string(), 1.2),
            ]),
            modules_weight_multipliers: BTreeMap::from([
                ("vehicle".to_string(), 1.0),
                ("gun".to_string(), 0.0),
                ("turret".to_string(), 0.0),
                ("engine".to_string(), 0.0),
                ("chassis".to_string(), 0.0),
                ("radio".to_string(), 0.0),
            ]),
            by_vehicle_overrides: BTreeMap::from([
                ("ussr:T-54".to_string(), 87.0),
                ("usa:M46_Patton".to_string(), 87.0),
                ("germany:Panther_II".to_string(), 87.0),
                ("ussr:Object_704".to_string(), 87.0),
                ("germany:JagdTiger".to_string(), 87.0),
                ("germany:H39_captured".to_string(), 7.0),
            ]),
            by_squad_size: vec![1.0, 1.0, 1.0],
        }
    }
}

/// Server-authoritative gameplay balance parameters.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct BalanceConfig {
    /// Armor normalization angle for AP shells in degrees.
    pub ap_normalization_deg: f32,
    /// Armor normalization angle for APCR shells in degrees.
    pub apcr_normalization_deg: f32,
    /// Armor normalization angle for HE/HEAT shells in degrees (`0.0` = none).
    pub he_normalization_deg: f32,
    /// Impact incidence angle threshold (degrees from surface normal) above which AP/APCR ricochets.
    pub ricochet_angle_deg: f32,
    /// Ballistic projectile speed scaling factor from `vehicle.xml` (`0.8` in retail 0.6.5).
    pub projectile_speed_factor: f32,
    /// Uniform shell damage randomization half-width (`0.25` => `0.75..=1.25`).
    pub damage_randomization: f32,
    /// Uniform armor penetration randomization half-width (`0.25` => `0.75..=1.25`).
    pub piercing_randomization: f32,
    /// Gravitational acceleration for ballistic shells in `m/s^2`.
    pub shell_gravity_mps2: f32,
    /// Minimum post-damage hit required to roll engine/fuel fire (`vehicle.xml` `minFireStartingDamage`).
    pub min_fire_starting_damage: i32,
    /// Fraction of max HP burned per second when fire fraction mode applies (`0.2` in retail 0.6.5).
    pub health_burn_per_sec_loss_fraction: f32,
    /// Fallback fire burn rate in `HP/s` when a hull omits `healthBurnPerSec`.
    pub default_health_burn_per_sec: f32,
    /// Dimensionless multiplier converting `vehicle_mass_kg * 9.81` to braking force in Newtons.
    pub default_brake_force_factor: f32,
    /// Class-based terrain resistance defaults stripped from client XML.
    pub terrain_resistance: TerrainResistanceDefaults,
    /// Maximum spotting distance in meters (`500.0` m in 0.6.5).
    pub max_vision_distance_m: f32,
    /// Proximity auto-detection radius ignoring bushes/camouflage in meters (`50.0` m).
    pub auto_detect_distance_m: f32,
    /// Duration in seconds a spotted enemy stays lit after losing line of sight (`5.0..=10.0` s).
    pub spot_linger_secs: f32,
    /// Default camo penalty factor applied when firing main gun (`invisibilityFactorAtShot`).
    pub invisibility_factor_at_shot_default: f32,
    /// Maximum additive foliage camouflage bonus (`0.50`).
    pub max_foliage_camo_bonus: f32,
    /// Cumulative path depth through foliage in meters that completely blocks line-of-sight (`15.0` m).
    pub foliage_los_opaque_depth_m: f32,
    /// Default armor homogenization multiplier when stripped from hull/turret XML.
    pub armor_homogenization_default: f32,
    /// Default critical-hit probability when stripped from internal device XML.
    pub chance_to_hit_default: f32,
    /// Default nominal crew training skill percentage (`100.0`).
    pub default_crew_skill: f32,
    /// Ramming kinetic damage scale factor.
    pub ramming_damage_scale: f32,
    /// Vertical impact speed threshold in `m/s` below which falling deals no hull damage.
    pub fall_damage_min_speed_mps: f32,
    /// Water depth threshold in meters beyond which terrain is unwalkable / vehicle drowns.
    pub max_fordable_water_depth_m: f32,
    /// Maximum climbable ground slope angle in degrees (`25.0` deg).
    pub max_climbable_slope_deg: f32,
    /// Battle XP multiplier (`1.0` = retail baseline).
    pub xp_factor: f32,
    /// Free XP share of earned battle XP (`0.05` = 5%).
    pub free_xp_factor: f32,
    /// Battle credits multiplier (`1.0` = retail baseline).
    pub credits_factor: f32,
    /// Repair cost multiplier (`1.0` = retail baseline).
    pub repair_cost_factor: f32,
    /// Allied damage / kill threshold before flagging a player as `TEAM_KILLER`.
    pub tkill_penalty_threshold: i32,
    /// Credits penalty factor applied to team damage compensation.
    pub tkill_credits_penalty_factor: f32,
    /// Matchmaker weights from `vehicle.xml`.
    pub matchmaker: MatchmakerBalanceConfig,
}

impl Default for BalanceConfig {
    fn default() -> Self {
        Self {
            ap_normalization_deg: 5.0,
            apcr_normalization_deg: 2.0,
            he_normalization_deg: 0.0,
            ricochet_angle_deg: 70.0,
            projectile_speed_factor: 0.8,
            damage_randomization: 0.25,
            piercing_randomization: 0.25,
            shell_gravity_mps2: GRAVITY_MPS2,
            min_fire_starting_damage: 21,
            health_burn_per_sec_loss_fraction: 0.2,
            default_health_burn_per_sec: 15.0,
            default_brake_force_factor: 1.0,
            terrain_resistance: TerrainResistanceDefaults::default(),
            max_vision_distance_m: 500.0,
            auto_detect_distance_m: 50.0,
            spot_linger_secs: 8.0,
            invisibility_factor_at_shot_default: 0.25,
            max_foliage_camo_bonus: 0.50,
            foliage_los_opaque_depth_m: 15.0,
            armor_homogenization_default: 1.0,
            chance_to_hit_default: 0.5,
            default_crew_skill: 100.0,
            ramming_damage_scale: 0.5,
            fall_damage_min_speed_mps: 8.0,
            max_fordable_water_depth_m: 1.5,
            max_climbable_slope_deg: 25.0,
            xp_factor: 1.0,
            free_xp_factor: 0.05,
            credits_factor: 1.0,
            repair_cost_factor: 1.0,
            tkill_penalty_threshold: 3,
            tkill_credits_penalty_factor: 1.0,
            matchmaker: MatchmakerBalanceConfig::default(),
        }
    }
}

impl BalanceConfig {
    /// Load JSON configuration from `path` if `Some`, or return retail-aligned defaults if `None`.
    pub fn load_or_default(path: Option<&Path>) -> Result<Self, AssetError> {
        let Some(path) = path else {
            return Ok(Self::default());
        };
        let bytes = std::fs::read(path)?;
        serde_json::from_slice(&bytes).map_err(|err| {
            AssetError::Config(format!("failed to parse balance config `{}`: {err}", path.display()))
        })
    }

    /// Armor normalization angle in radians for the given shell kind.
    pub fn normalization_rad(&self, kind: ShellKind) -> f32 {
        match kind {
            ShellKind::ArmorPiercing | ShellKind::ArmorPiercingHe => {
                self.ap_normalization_deg.to_radians()
            }
            ShellKind::ArmorPiercingCr => self.apcr_normalization_deg.to_radians(),
            ShellKind::HighExplosive | ShellKind::HollowCharge => {
                self.he_normalization_deg.to_radians()
            }
        }
    }

    /// Cosine of [`Self::ricochet_angle_deg`] (`n · (-dir) < ricochet_cos()` => ricochet).
    pub fn ricochet_cos(&self) -> f32 {
        self.ricochet_angle_deg.to_radians().cos()
    }

    /// Cosine of [`Self::max_climbable_slope_deg`] (`normal.y < max_climbable_slope_cos()` => too steep).
    pub fn max_climbable_slope_cos(&self) -> f32 {
        self.max_climbable_slope_deg.to_radians().cos()
    }

    /// Resolve `[firm, medium, soft]` terrain resistance multipliers from vehicle XML tags.
    pub fn terrain_resistance_for_tags(&self, tags: &BTreeSet<String>) -> [f32; 3] {
        if tags.contains("lightTank") {
            self.terrain_resistance.light_tank
        } else if tags.contains("mediumTank") {
            self.terrain_resistance.medium_tank
        } else if tags.contains("heavyTank") {
            self.terrain_resistance.heavy_tank
        } else if tags.contains("AT-SPG") {
            self.terrain_resistance.at_spg
        } else if tags.contains("SPG") {
            self.terrain_resistance.spg
        } else {
            self.terrain_resistance.fallback
        }
    }

    /// Compute fallback braking force in Newtons for a vehicle of mass `mass_kg`.
    pub fn brake_force_for_mass(&self, mass_kg: f32) -> f32 {
        mass_kg.max(0.0) * GRAVITY_MPS2 * self.default_brake_force_factor
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    struct TempTestDir(PathBuf);

    impl TempTestDir {
        fn new(name: &str) -> Self {
            let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("../../target/test-tmp")
                .join(format!("{name}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).expect("create target/test-tmp subdir");
            Self(dir)
        }
    }

    impl Drop for TempTestDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn balance_config_none_returns_retail_defaults() {
        let cfg = BalanceConfig::load_or_default(None).expect("default BalanceConfig");
        assert!((cfg.ap_normalization_deg - 5.0).abs() < 1e-6);
        assert!((cfg.apcr_normalization_deg - 2.0).abs() < 1e-6);
        assert!((cfg.he_normalization_deg - 0.0).abs() < 1e-6);
        assert!((cfg.ricochet_angle_deg - 70.0).abs() < 1e-6);
        assert!((cfg.projectile_speed_factor - 0.8).abs() < 1e-6);
        assert_eq!(cfg.min_fire_starting_damage, 21);
        assert!((cfg.xp_factor - 1.0).abs() < 1e-6);
        assert!((cfg.credits_factor - 1.0).abs() < 1e-6);
        assert!(
            (cfg.normalization_rad(ShellKind::ArmorPiercing) - 5.0_f32.to_radians()).abs() < 1e-6
        );
        assert!(
            (cfg.normalization_rad(ShellKind::ArmorPiercingHe) - 5.0_f32.to_radians()).abs()
                < 1e-6
        );
        assert!(
            (cfg.normalization_rad(ShellKind::ArmorPiercingCr) - 2.0_f32.to_radians()).abs()
                < 1e-6
        );
        assert!((cfg.normalization_rad(ShellKind::HighExplosive)).abs() < 1e-6);
        assert!((cfg.normalization_rad(ShellKind::HollowCharge)).abs() < 1e-6);
        assert!((cfg.ricochet_cos() - 70.0_f32.to_radians().cos()).abs() < 1e-6);

        let light: BTreeSet<String> = ["lightTank".to_string()].into_iter().collect();
        let heavy: BTreeSet<String> = ["heavyTank".to_string()].into_iter().collect();
        assert_eq!(
            cfg.terrain_resistance_for_tags(&light),
            cfg.terrain_resistance.light_tank
        );
        assert_eq!(
            cfg.terrain_resistance_for_tags(&heavy),
            cfg.terrain_resistance.heavy_tank
        );
        assert!(cfg.brake_force_for_mass(10_000.0) > 90_000.0);
    }

    #[test]
    fn balance_config_loads_partial_json_override_and_rejects_bad_file() {
        let tmp = TempTestDir::new("balance_override");
        let cfg_path = tmp.0.join("balance.json");
        std::fs::write(
            &cfg_path,
            r#"{ "xp_factor": 2.5, "credits_factor": 1.75, "ricochet_angle_deg": 72.0 }"#,
        )
        .unwrap();

        let cfg = BalanceConfig::load_or_default(Some(&cfg_path)).expect("load partial override");
        assert!((cfg.xp_factor - 2.5).abs() < 1e-6);
        assert!((cfg.credits_factor - 1.75).abs() < 1e-6);
        assert!((cfg.ricochet_angle_deg - 72.0).abs() < 1e-6);
        assert!((cfg.ap_normalization_deg - 5.0).abs() < 1e-6);
        assert_eq!(cfg.min_fire_starting_damage, 21);

        let bad_path = tmp.0.join("bad.json");
        std::fs::write(&bad_path, "{ not valid json").unwrap();
        assert!(BalanceConfig::load_or_default(Some(&bad_path)).is_err());

        let missing_path = tmp.0.join("missing.json");
        assert!(BalanceConfig::load_or_default(Some(&missing_path)).is_err());
    }
}
