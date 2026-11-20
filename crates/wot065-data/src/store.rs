use std::fmt;
use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicI64, AtomicU64, Ordering};

use crate::account_state::{create_profile, AccountProfile, Preset};
use crate::catalog::Catalog;
use crate::descr::VehicleDescr;
use crate::economics::EconomicsConfig;

static SAVE_COUNTER: AtomicU64 = AtomicU64::new(1);

const RESERVED_NAMES: &[&str] = &[
    "con", "prn", "aux", "nul", "com1", "com2", "com3", "com4", "com5", "com6", "com7", "com8",
    "com9", "lpt1", "lpt2", "lpt3", "lpt4", "lpt5", "lpt6", "lpt7", "lpt8", "lpt9",
];

/// Store errors encountered during account persistence operations.
#[derive(Debug)]
pub enum StoreError {
    Io(io::Error),
    Json(serde_json::Error),
    NotFound(String),
    Corrupt { path: PathBuf, reason: String },
    Creation(String),
}

impl fmt::Display for StoreError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            StoreError::Io(e) => write!(f, "I/O error: {e}"),
            StoreError::Json(e) => write!(f, "JSON error: {e}"),
            StoreError::NotFound(name) => write!(f, "account '{name}' not found"),
            StoreError::Corrupt { path, reason } => {
                write!(f, "account file '{}' is corrupt: {reason}", path.display())
            }
            StoreError::Creation(e) => write!(f, "failed to create account: {e}"),
        }
    }
}

impl std::error::Error for StoreError {}

impl From<io::Error> for StoreError {
    fn from(e: io::Error) -> Self {
        StoreError::Io(e)
    }
}

impl From<serde_json::Error> for StoreError {
    fn from(e: serde_json::Error) -> Self {
        StoreError::Json(e)
    }
}

/// Convert username to an injective, path-traversal-free, Windows-safe filename key.
///
/// Trims and lowercases the input. Lowercase ASCII alphanumerics `[a-z0-9]` are emitted as-is.
/// Any other byte is hex-encoded as `_{:02x}`. Literal `_` is encoded as `_5f`.
/// If the normalized string matches a Windows reserved device name (e.g. `con`, `aux`, `nul`),
/// its first character is escaped to ensure safe file creation on Windows NTFS/FAT filesystems.
/// An empty username maps to `_empty`.
pub fn safe_account_key(username: &str) -> String {
    let normalized = username.trim().to_lowercase();
    if normalized.is_empty() {
        return "_empty".to_string();
    }

    let is_reserved = RESERVED_NAMES.contains(&normalized.as_str());

    let mut key = String::with_capacity(normalized.len() * 2);
    for (i, &b) in normalized.as_bytes().iter().enumerate() {
        if i == 0 && is_reserved {
            // Escape first character of reserved device name
            key.push_str(&format!("_{:02x}", b));
        } else if b.is_ascii_lowercase() || b.is_ascii_digit() {
            key.push(b as char);
        } else {
            key.push_str(&format!("_{:02x}", b));
        }
    }

    key
}

/// Decode an account key back to its normalized username.
pub fn decode_account_key(key: &str) -> Option<String> {
    if key == "_empty" {
        return Some(String::new());
    }

    let bytes = key.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;

    while i < bytes.len() {
        if bytes[i] == b'_' {
            if i + 2 >= bytes.len() {
                return None;
            }
            let hex_str = std::str::from_utf8(&bytes[i + 1..i + 3]).ok()?;
            let byte = u8::from_str_radix(hex_str, 16).ok()?;
            out.push(byte);
            i += 3;
        } else if bytes[i].is_ascii_lowercase() || bytes[i].is_ascii_digit() {
            out.push(bytes[i]);
            i += 1;
        } else {
            return None;
        }
    }

    String::from_utf8(out).ok()
}

/// Atomic JSON-backed account persistence store.
pub struct JsonAccountStore {
    data_dir: PathBuf,
    next_db_id: AtomicI64,
}

impl JsonAccountStore {
    /// Open the store in `data_dir`, ensuring the directory exists and scanning
    /// existing `*.json` account files to initialize `next_db_id = max_seen + 1` (min 1).
    pub fn open(data_dir: impl AsRef<Path>) -> Result<Self, StoreError> {
        let data_dir = data_dir.as_ref().to_path_buf();
        fs::create_dir_all(&data_dir)?;

        let mut max_db_id: i64 = 0;
        for entry in fs::read_dir(&data_dir)? {
            let entry = entry?;
            let path = entry.path();
            if path.is_file() && path.extension().and_then(|s| s.to_str()) == Some("json") {
                let file_name = entry.file_name().to_string_lossy().to_string();
                if file_name.contains(".tmp.") {
                    continue;
                }
                if let Ok(content) = fs::read_to_string(&path) {
                    #[derive(serde::Deserialize)]
                    struct DbIdOnly {
                        database_id: i64,
                    }
                    if let Ok(item) = serde_json::from_str::<DbIdOnly>(&content) {
                        max_db_id = max_db_id.max(item.database_id);
                    }
                }
            }
        }

        let next_db_id = AtomicI64::new((max_db_id + 1).max(1));
        Ok(Self {
            data_dir,
            next_db_id,
        })
    }

    pub fn data_dir(&self) -> &Path {
        &self.data_dir
    }

    /// Resolve account filename path: `<data_dir>/<safe_account_key>.json`.
    pub fn account_path(&self, username: &str) -> PathBuf {
        let key = safe_account_key(username);
        self.data_dir.join(format!("{key}.json"))
    }

    /// Check if account profile exists on disk.
    pub fn profile_exists(&self, username: &str) -> bool {
        self.account_path(username).is_file()
    }

    /// Load account profile if it exists on disk, or return `Ok(None)` if not found.
    pub fn load(&self, username: &str) -> Result<Option<AccountProfile>, StoreError> {
        let path = self.account_path(username);
        if !path.exists() {
            return Ok(None);
        }
        let content = fs::read_to_string(&path)?;
        let profile = serde_json::from_str::<AccountProfile>(&content).map_err(|e| {
            StoreError::Corrupt {
                path: path.clone(),
                reason: e.to_string(),
            }
        })?;
        Ok(Some(profile))
    }

    /// Load account profile or return `StoreError::NotFound`.
    pub fn load_required(&self, username: &str) -> Result<AccountProfile, StoreError> {
        self.load(username)?
            .ok_or_else(|| StoreError::NotFound(username.to_string()))
    }

    /// Atomically save account profile to `<data_dir>/<key>.json` by writing to
    /// `<data_dir>/<key>.json.tmp.<pid>_<counter>`, syncing data to disk, and renaming over destination.
    /// Cleanly replaces existing files on Windows and leaves zero leftover temp files.
    pub fn save(&self, profile: &AccountProfile) -> Result<(), StoreError> {
        let dest_path = self.account_path(&profile.username);
        let pid = std::process::id();
        let counter = SAVE_COUNTER.fetch_add(1, Ordering::Relaxed);
        let key = safe_account_key(&profile.username);
        let tmp_path = self.data_dir.join(format!("{key}.json.tmp.{pid}_{counter}"));

        let json_bytes = serde_json::to_vec_pretty(profile)?;

        let write_res = (|| -> Result<(), io::Error> {
            let mut file = OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&tmp_path)?;
            file.write_all(&json_bytes)?;
            file.sync_all()?;
            drop(file); // Ensure file handle is closed before renaming on Windows!

            if let Err(e) = fs::rename(&tmp_path, &dest_path) {
                // On Windows, if destination already exists and rename returns an error,
                // remove destination and retry rename.
                if dest_path.exists() {
                    let _ = fs::remove_file(&dest_path);
                    fs::rename(&tmp_path, &dest_path)?;
                } else {
                    return Err(e);
                }
            }
            Ok(())
        })();

        if let Err(e) = write_res {
            let _ = fs::remove_file(&tmp_path);
            return Err(StoreError::Io(e));
        }

        Ok(())
    }

    /// Load existing profile, or create and persist a new one if it does not exist.
    /// Existing profiles are loaded as-is without preset overwrite.
    pub fn load_or_create(
        &self,
        username: &str,
        preset: Preset,
        catalog: &Catalog,
        economics: &EconomicsConfig,
    ) -> Result<AccountProfile, StoreError> {
        if let Some(existing) = self.load(username)? {
            return Ok(existing);
        }
        self.create_new(username, preset, catalog, economics)
    }

    /// Create and persist a new account profile with monotonic `database_id`.
    pub fn create_new(
        &self,
        username: &str,
        preset: Preset,
        catalog: &Catalog,
        economics: &EconomicsConfig,
    ) -> Result<AccountProfile, StoreError> {
        let now_utc = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs() as u32;
        self.create_new_at(username, preset, catalog, economics, now_utc)
    }

    /// Create and persist a new account profile with specified `now_utc`.
    pub fn create_new_at(
        &self,
        username: &str,
        preset: Preset,
        catalog: &Catalog,
        economics: &EconomicsConfig,
        now_utc: u32,
    ) -> Result<AccountProfile, StoreError> {
        let db_id = self.next_db_id.fetch_add(1, Ordering::SeqCst);
        let profile = create_profile(db_id, username, preset, catalog, economics, now_utc)
            .map_err(StoreError::Creation)?;
        self.save(&profile)?;
        Ok(profile)
    }

    /// Delete account file from disk if present. Returns `true` if file existed and was removed.
    pub fn delete(&self, username: &str) -> Result<bool, StoreError> {
        let path = self.account_path(username);
        if path.exists() {
            fs::remove_file(path)?;
            Ok(true)
        } else {
            Ok(false)
        }
    }

    /// Return a sorted list of all account usernames in the store.
    pub fn list(&self) -> Result<Vec<String>, StoreError> {
        let mut usernames = Vec::new();
        if !self.data_dir.exists() {
            return Ok(usernames);
        }

        for entry in fs::read_dir(&self.data_dir)? {
            let entry = entry?;
            let path = entry.path();
            if path.is_file() && path.extension().and_then(|s| s.to_str()) == Some("json") {
                let file_name = entry.file_name().to_string_lossy().to_string();
                if file_name.contains(".tmp.") {
                    continue;
                }
                let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or("");
                if let Ok(content) = fs::read_to_string(&path) {
                    #[derive(serde::Deserialize)]
                    struct QuickProfile {
                        username: String,
                    }
                    if let Ok(quick) = serde_json::from_str::<QuickProfile>(&content) {
                        usernames.push(quick.username);
                        continue;
                    }
                }
                if let Some(decoded) = decode_account_key(stem) {
                    usernames.push(decoded);
                } else {
                    usernames.push(stem.to_string());
                }
            }
        }

        usernames.sort();
        usernames.dedup();
        Ok(usernames)
    }
}

/// Offline helper to damage a vehicle to `hp_fraction` of its max health.
/// Sets `repair = Some((cost, new_hp))` where `cost = ceil((max_hp - new_hp) * repair_cost)`.
/// Increments `profile.rev` and returns `Ok((cost, new_hp))`.
pub fn damage_vehicle(
    profile: &mut AccountProfile,
    veh_inv_id: i64,
    hp_fraction: f64,
    catalog: &Catalog,
) -> Result<(i32, i32), String> {
    if !(0.0..=0.999).contains(&hp_fraction) {
        return Err(format!(
            "hp_fraction must be in 0.0..=0.999, got {hp_fraction}"
        ));
    }

    let veh = profile
        .vehicles
        .get_mut(&veh_inv_id)
        .ok_or_else(|| format!("vehicle {veh_inv_id} not found in profile"))?;

    let veh_descr = VehicleDescr::from_bytes(&veh.comp_descr)
        .map_err(|e| format!("invalid vehicle descriptor: {e:?}"))?;

    let veh_def = catalog
        .vehicle_def_by_id(veh_descr.nation_id, veh_descr.type_id as u16)
        .ok_or_else(|| {
            format!(
                "vehicle definition ({}, {}) not found in catalog",
                veh_descr.nation_id, veh_descr.type_id
            )
        })?;

    let turret = veh_def
        .turrets
        .iter()
        .find(|t| t.component.id == veh_descr.turret_id)
        .ok_or_else(|| {
            format!(
                "turret {} not found on vehicle {}",
                veh_descr.turret_id, veh_def.name
            )
        })?;

    let max_hp = veh_def.hull_max_health + turret.max_health;
    if max_hp <= 0 {
        return Err(format!("invalid vehicle max HP {max_hp}"));
    }

    let new_hp = ((max_hp as f64) * hp_fraction).round() as i32;
    let new_hp = new_hp.clamp(0, max_hp.saturating_sub(1));
    let damage = (max_hp - new_hp).max(0);
    let cost = ((damage as f64) * veh_def.repair_cost).ceil() as i32;

    veh.repair = Some((cost, new_hp));
    profile.rev += 1;

    Ok((cost, new_hp))
}

/// Post-battle hull and internal/external component HP state for repair cost calculation.
#[derive(Debug, Clone, Default)]
pub struct VehicleComponentDamage {
    pub hull_hp: i32,
    pub engine_hp: Option<i32>,
    pub fuel_tank_hp: Option<i32>,
    pub radio_hp: Option<i32>,
    pub ammo_bay_hp: Option<i32>,
    pub left_track_hp: Option<i32>,
    pub right_track_hp: Option<i32>,
    pub turret_rotator_hp: Option<i32>,
    pub surveying_device_hp: Option<i32>,
    pub gun_hp: Option<i32>,
}

fn damaged_component_cost(
    opt_hp: Option<i32>,
    max_health: Option<i32>,
    max_regen_health: Option<i32>,
    repair_cost: Option<f64>,
) -> f64 {
    let Some(hp) = opt_hp else {
        return 0.0;
    };
    let max_h = max_health.unwrap_or(0);
    let max_regen = max_regen_health.unwrap_or(0).clamp(0, max_h);
    let rc = repair_cost.unwrap_or(0.0);
    let clamped_hp = hp.clamp(0, max_h).max(max_regen);
    ((max_h - clamped_hp).max(0) as f64) * rc
}

fn damaged_device_health_cost(
    opt_hp: Option<i32>,
    health: Option<crate::item_defs::DeviceHealthDef>,
) -> f64 {
    match (opt_hp, health) {
        (Some(hp), Some(def)) => damaged_component_cost(
            Some(hp),
            Some(def.max_health),
            Some(def.max_regen_health),
            Some(def.repair_cost),
        ),
        _ => 0.0,
    }
}

/// Apply post-battle hull + component damage matching `common/items/vehicles.py:405-412`
/// (`getMaxRepairCost`), setting `veh.repair = Some((cost, new_hp))` (or `None` if undamaged).
pub fn damage_vehicle_components(
    profile: &mut AccountProfile,
    veh_inv_id: i64,
    damage: &VehicleComponentDamage,
    catalog: &Catalog,
) -> Result<(i32, i32), String> {
    let veh_entry = profile
        .vehicles
        .get_mut(&veh_inv_id)
        .ok_or_else(|| format!("vehicle {veh_inv_id} not found in profile"))?;

    let veh_descr = VehicleDescr::from_bytes(&veh_entry.comp_descr)
        .map_err(|e| format!("invalid vehicle compDescr for {veh_inv_id}: {e}"))?;

    let veh_def = catalog
        .vehicle_def_by_id(veh_descr.nation_id, veh_descr.type_id as u16)
        .ok_or_else(|| {
            format!(
                "missing catalog VehicleDef for ({}, {})",
                veh_descr.nation_id, veh_descr.type_id
            )
        })?;

    let turret = veh_def
        .turrets
        .iter()
        .find(|t| t.component.id == veh_descr.turret_id)
        .ok_or_else(|| format!("missing turret {} in {}", veh_descr.turret_id, veh_def.name))?;
    let gun = turret
        .guns
        .iter()
        .find(|g| g.component.id == veh_descr.gun_id)
        .ok_or_else(|| format!("missing gun {} in {}", veh_descr.gun_id, veh_def.name))?;
    let chassis = veh_def
        .chassis
        .iter()
        .find(|c| c.id == veh_descr.chassis_id)
        .ok_or_else(|| format!("missing chassis {} in {}", veh_descr.chassis_id, veh_def.name))?;
    let engine = veh_def
        .engines
        .iter()
        .find(|e| e.id == veh_descr.engine_id)
        .ok_or_else(|| format!("missing engine {} in {}", veh_descr.engine_id, veh_def.name))?;
    let fuel_tank = veh_def
        .fuel_tanks
        .iter()
        .find(|f| f.id == veh_descr.fuel_tank_id)
        .ok_or_else(|| {
            format!(
                "missing fuel_tank {} in {}",
                veh_descr.fuel_tank_id, veh_def.name
            )
        })?;
    let radio = veh_def
        .radios
        .iter()
        .find(|r| r.id == veh_descr.radio_id)
        .ok_or_else(|| format!("missing radio {} in {}", veh_descr.radio_id, veh_def.name))?;

    let max_hp = veh_def.hull_max_health + turret.max_health;
    if max_hp <= 0 {
        return Err(format!("invalid vehicle max HP {max_hp}"));
    }

    let new_hp = damage.hull_hp.clamp(0, max_hp);
    let hull_damage = (max_hp - new_hp).max(0);
    let total_cost = ((hull_damage as f64) * veh_def.repair_cost)
        + damaged_component_cost(
            damage.gun_hp,
            gun.component.max_health,
            gun.component.max_regen_health,
            gun.component.repair_cost,
        )
        + damaged_device_health_cost(damage.turret_rotator_hp, turret.turret_rotator_health)
        + damaged_device_health_cost(damage.surveying_device_hp, turret.surveying_device_health)
        + damaged_device_health_cost(damage.ammo_bay_hp, veh_def.ammo_bay_health)
        + damaged_component_cost(
            damage.left_track_hp,
            chassis.max_health,
            chassis.max_regen_health,
            chassis.repair_cost,
        )
        + damaged_component_cost(
            damage.right_track_hp,
            chassis.max_health,
            chassis.max_regen_health,
            chassis.repair_cost,
        )
        + damaged_component_cost(
            damage.engine_hp,
            engine.max_health,
            engine.max_regen_health,
            engine.repair_cost,
        )
        + damaged_component_cost(
            damage.fuel_tank_hp,
            fuel_tank.max_health,
            fuel_tank.max_regen_health,
            fuel_tank.repair_cost,
        )
        + damaged_component_cost(
            damage.radio_hp,
            radio.max_health,
            radio.max_regen_health,
            radio.repair_cost,
        );

    let cost = total_cost.ceil() as i32;
    veh_entry.repair = if cost > 0 || new_hp < max_hp {
        Some((cost, new_hp))
    } else {
        None
    };
    profile.rev += 1;

    Ok((cost, new_hp))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil;

    fn test_temp_dir(name: &str) -> PathBuf {
        let base = std::env::var_os("CARGO_TARGET_TMPDIR")
            .map(PathBuf::from)
            .unwrap_or_else(|| {
                Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/test-tmp")
            });
        let dir = base.join(name);
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("create test dir");
        dir
    }

    fn real_catalog() -> Option<Catalog> {
        let dir = testutil::game_dir()?;
        let catalog = crate::catalog::load(&dir).expect("catalog loads from the retail install");
        Some(catalog)
    }

    #[test]
    fn test_safe_account_key_encoding_and_injectivity() {
        let k1 = safe_account_key("Player@Test.com");
        assert_eq!(k1, "player_40test_2ecom");
        assert!(!k1.contains('@'));
        assert!(!k1.contains('.'));
        assert!(!k1.contains('/'));
        assert!(!k1.contains('\\'));

        let k_escape = safe_account_key("../escape");
        assert_eq!(k_escape, "_2e_2e_2fescape");
        assert!(!k_escape.contains(".."));
        assert!(!k_escape.contains('/'));
        assert!(!k_escape.contains('\\'));

        // Collision freedom: a/b vs a_b
        assert_ne!(safe_account_key("a/b"), safe_account_key("a_b"));
        assert_eq!(safe_account_key("a/b"), "a_2fb");
        assert_eq!(safe_account_key("a_b"), "a_5fb");

        // Windows reserved names guarded
        assert_ne!(safe_account_key("con"), "con");
        assert_ne!(safe_account_key("prn"), "prn");
        assert_ne!(safe_account_key("aux"), "aux");
        assert_ne!(safe_account_key("nul"), "nul");

        // Roundtrip across diverse inputs
        let cases = [
            "Player@Test.com",
            "../escape",
            "a/b",
            "a_b",
            "con",
            "prn",
            "aux",
            "nul",
            "com1",
            "lpt1",
            "User_123",
            "hello world",
            "test",
            "",
            "T-34",
            "unicode:Привіт",
        ];

        for &raw in &cases {
            let key = safe_account_key(raw);
            assert!(
                !key.contains('/') && !key.contains('\\') && !key.contains(".."),
                "key '{key}' for raw '{raw}' must not contain path traversal"
            );
            let decoded = decode_account_key(&key).expect("decode succeeds");
            let expected = raw.trim().to_lowercase();
            assert_eq!(decoded, expected, "roundtrip mismatch for '{raw}'");
        }
    }

    #[test]
    fn test_store_save_load_and_atomic_overwrite() {
        let Some(catalog) = real_catalog() else {
            eprintln!("skip: retail game files not available");
            return;
        };
        let dir = test_temp_dir("store-save-load");
        let store = JsonAccountStore::open(&dir).expect("open store");

        let economics = EconomicsConfig::default();
        let profile = create_profile(1, "PlayerOne", Preset::Retail, &catalog, &economics, 1000)
            .expect("create profile");

        // Save
        store.save(&profile).expect("save succeeds");
        assert!(store.profile_exists("PlayerOne"));
        assert!(store.account_path("PlayerOne").exists());

        // Verify no leftover .tmp files
        for entry in fs::read_dir(&dir).expect("read dir") {
            let entry = entry.expect("entry");
            let name = entry.file_name().to_string_lossy().to_string();
            assert!(!name.contains(".tmp."), "leftover tmp file found: {name}");
        }

        // Load
        let loaded = store.load("PlayerOne").expect("load").expect("some");
        assert_eq!(loaded.database_id, 1);
        assert_eq!(loaded.username, "PlayerOne");
        assert_eq!(loaded.credits, profile.credits);
        assert_eq!(loaded.rev, 1);

        let required = store.load_required("PlayerOne").expect("load_required");
        assert_eq!(required.database_id, 1);

        // Atomic overwrite test on Windows (must not fail with AlreadyExists)
        let mut updated = loaded;
        updated.credits += 5000;
        updated.rev += 1;
        store.save(&updated).expect("save overwrite succeeds");

        // Verify no leftover .tmp files after overwrite
        for entry in fs::read_dir(&dir).expect("read dir") {
            let entry = entry.expect("entry");
            let name = entry.file_name().to_string_lossy().to_string();
            assert!(!name.contains(".tmp."), "leftover tmp file found: {name}");
        }

        let reloaded = store.load("PlayerOne").expect("load").expect("some");
        assert_eq!(reloaded.credits, profile.credits + 5000);
        assert_eq!(reloaded.rev, 2);
    }

    #[test]
    fn test_store_load_or_create_and_preset_immutability() {
        let Some(catalog) = real_catalog() else {
            eprintln!("skip: retail game files not available");
            return;
        };
        let dir = test_temp_dir("store-load-create");
        let store = JsonAccountStore::open(&dir).expect("open store");
        let economics = EconomicsConfig::default();

        // 1. First open: creates new profile via Retail preset
        let p1 = store
            .load_or_create("Commander", Preset::Retail, &catalog, &economics)
            .expect("load or create");
        assert_eq!(p1.database_id, 1);
        assert_eq!(p1.credits, economics.retail_starting.credits);

        // Mutate credits on disk
        let mut modified = p1.clone();
        modified.credits = 999_999;
        store.save(&modified).expect("save");

        // 2. Second open: reloads existing profile, ignoring preset overwrite
        let p2 = store
            .load_or_create("Commander", Preset::Sandbox, &catalog, &economics)
            .expect("load or create second time");
        assert_eq!(p2.database_id, 1);
        assert_eq!(p2.credits, 999_999, "existing profile must NOT be overwritten by preset");
    }

    #[test]
    fn test_store_database_id_continuity_across_restarts() {
        let Some(catalog) = real_catalog() else {
            eprintln!("skip: retail game files not available");
            return;
        };
        let dir = test_temp_dir("store-restarts");
        let economics = EconomicsConfig::default();

        // Session 1: create two accounts
        {
            let store1 = JsonAccountStore::open(&dir).expect("open store1");
            let p1 = store1
                .create_new("user1", Preset::Retail, &catalog, &economics)
                .expect("user1");
            assert_eq!(p1.database_id, 1);

            let p2 = store1
                .create_new("user2", Preset::Retail, &catalog, &economics)
                .expect("user2");
            assert_eq!(p2.database_id, 2);
        }

        // Session 2: store restarts, scans data_dir, continues database_id sequence
        {
            let store2 = JsonAccountStore::open(&dir).expect("open store2");
            let p3 = store2
                .create_new("user3", Preset::Retail, &catalog, &economics)
                .expect("user3");
            assert_eq!(p3.database_id, 3, "database_id must continue monotonically across restarts");

            // Verify prior accounts still load intact
            let loaded1 = store2.load_required("user1").expect("load user1");
            assert_eq!(loaded1.database_id, 1);
            let loaded2 = store2.load_required("user2").expect("load user2");
            assert_eq!(loaded2.database_id, 2);
        }
    }

    #[test]
    fn test_store_delete_and_list_and_profile_exists() {
        let Some(catalog) = real_catalog() else {
            eprintln!("skip: retail game files not available");
            return;
        };
        let dir = test_temp_dir("store-delete-list");
        let store = JsonAccountStore::open(&dir).expect("open store");
        let economics = EconomicsConfig::default();

        store.create_new("alice", Preset::Retail, &catalog, &economics).expect("alice");
        store.create_new("bob", Preset::Retail, &catalog, &economics).expect("bob");
        store.create_new("charlie", Preset::Retail, &catalog, &economics).expect("charlie");

        assert!(store.profile_exists("alice"));
        assert!(store.profile_exists("bob"));
        assert!(store.profile_exists("charlie"));
        assert!(!store.profile_exists("unknown"));

        let list = store.list().expect("list");
        assert_eq!(list, vec!["alice", "bob", "charlie"]);

        // Delete bob
        assert!(store.delete("bob").expect("delete"));
        assert!(!store.profile_exists("bob"));
        assert_eq!(store.load("bob").expect("load"), None);

        let list_after = store.list().expect("list after");
        assert_eq!(list_after, vec!["alice", "charlie"]);

        // Deleting non-existent returns false
        assert!(!store.delete("bob").expect("delete non-existent"));
    }

    #[test]
    fn test_damage_vehicle_calculation_and_repair() {
        let Some(catalog) = real_catalog() else {
            eprintln!("skip: retail game files not available");
            return;
        };

        let economics = EconomicsConfig::default();
        let mut profile = create_profile(1, "Tanker", Preset::Retail, &catalog, &economics, 1000)
            .expect("create profile");

        // Profile has MS-1 as first vehicle
        let ms1_inv_id = 1;
        let orig_rev = profile.rev;

        // Damage vehicle to 25% HP
        let (cost, new_hp) = damage_vehicle(&mut profile, ms1_inv_id, 0.25, &catalog)
            .expect("damage vehicle succeeds");

        assert_eq!(profile.rev, orig_rev + 1, "profile.rev must increment on damage");
        assert_eq!(profile.vehicles[&ms1_inv_id].repair, Some((cost, new_hp)));

        // Verify calculation against catalog definition
        let veh_entry = &profile.vehicles[&ms1_inv_id];
        let veh_descr = VehicleDescr::from_bytes(&veh_entry.comp_descr).expect("descr");
        let veh_def = catalog
            .vehicle_def_by_id(veh_descr.nation_id, veh_descr.type_id as u16)
            .expect("veh_def");
        let turret = veh_def
            .turrets
            .iter()
            .find(|t| t.component.id == veh_descr.turret_id)
            .expect("turret");

        let max_hp = veh_def.hull_max_health + turret.max_health;
        let expected_new_hp = ((max_hp as f64) * 0.25).round() as i32;
        let expected_damage = max_hp - expected_new_hp;
        let expected_cost = ((expected_damage as f64) * veh_def.repair_cost).ceil() as i32;

        assert_eq!(new_hp, expected_new_hp);
        assert_eq!(cost, expected_cost);

        // Verify that ops::fitting::repair successfully repairs it
        profile.credits = 100_000;
        let outcome = crate::ops::fitting::repair(&mut profile, ms1_inv_id, &catalog, &economics, 1, 1050)
            .expect("repair succeeds");

        assert_eq!(outcome.result_id, 0); // RES_SUCCESS
        assert_eq!(profile.credits, 100_000 - cost as i64);
        assert!(profile.vehicles[&ms1_inv_id].repair.is_none());
    }

    #[test]
    fn test_damage_vehicle_components_matches_vehicles_py_max_repair_cost() {
        let Some(catalog) = real_catalog() else {
            eprintln!("skip: retail game files not available");
            return;
        };

        let economics = EconomicsConfig::default();
        let mut profile = create_profile(1, "Tanker", Preset::Retail, &catalog, &economics, 1000)
            .expect("create profile");

        // Point starter slot 1 at stock T-34 (nation_id == 0, type_id == 0) which has non-zero hull repairCost.
        let t34_def = catalog.vehicle_def_by_id(0, 0).expect("T-34 def");
        let t34_inv_id = 1i64;
        profile.vehicles.get_mut(&t34_inv_id).unwrap().comp_descr =
            VehicleDescr::stock(t34_def).expect("T-34 stock").to_bytes();

        // Destroy hull + all components (including both tracks -> chassis * 2)
        let total_destruction = VehicleComponentDamage {
            hull_hp: 0,
            engine_hp: Some(0),
            fuel_tank_hp: Some(0),
            radio_hp: Some(0),
            ammo_bay_hp: Some(0),
            left_track_hp: Some(0),
            right_track_hp: Some(0),
            turret_rotator_hp: Some(0),
            surveying_device_hp: Some(0),
            gun_hp: Some(0),
        };
        let (cost, new_hp) =
            damage_vehicle_components(&mut profile, t34_inv_id, &total_destruction, &catalog)
                .expect("damage_vehicle_components succeeds");

        let veh_descr = VehicleDescr::from_bytes(&profile.vehicles[&t34_inv_id].comp_descr).unwrap();
        let veh_def = catalog
            .vehicle_def_by_id(veh_descr.nation_id, veh_descr.type_id as u16)
            .unwrap();
        let turret = veh_def
            .turrets
            .iter()
            .find(|t| t.component.id == veh_descr.turret_id)
            .unwrap();
        let gun = turret
            .guns
            .iter()
            .find(|g| g.component.id == veh_descr.gun_id)
            .unwrap();
        let chassis = veh_def
            .chassis
            .iter()
            .find(|c| c.id == veh_descr.chassis_id)
            .unwrap();
        let engine = veh_def
            .engines
            .iter()
            .find(|e| e.id == veh_descr.engine_id)
            .unwrap();
        let fuel_tank = veh_def
            .fuel_tanks
            .iter()
            .find(|f| f.id == veh_descr.fuel_tank_id)
            .unwrap();
        let radio = veh_def
            .radios
            .iter()
            .find(|r| r.id == veh_descr.radio_id)
            .unwrap();

        let comp_max_cost = |max_h: i32, max_regen: i32, rc: f64| -> f64 {
            (max_h - max_regen).max(0) as f64 * rc
        };
        let max_hp = veh_def.hull_max_health + turret.max_health;
        let expected_max_cost = ((max_hp as f64) * veh_def.repair_cost
            + comp_max_cost(
                gun.component.max_health.unwrap(),
                gun.component.max_regen_health.unwrap(),
                gun.component.repair_cost.unwrap(),
            )
            + turret.turret_rotator_health.unwrap().max_repair_cost()
            + turret.surveying_device_health.unwrap().max_repair_cost()
            + veh_def.ammo_bay_health.unwrap().max_repair_cost()
            + comp_max_cost(
                chassis.max_health.unwrap(),
                chassis.max_regen_health.unwrap(),
                chassis.repair_cost.unwrap(),
            ) * 2.0
            + comp_max_cost(
                engine.max_health.unwrap(),
                engine.max_regen_health.unwrap(),
                engine.repair_cost.unwrap(),
            )
            + comp_max_cost(
                fuel_tank.max_health.unwrap(),
                fuel_tank.max_regen_health.unwrap(),
                fuel_tank.repair_cost.unwrap(),
            )
            + comp_max_cost(
                radio.max_health.unwrap(),
                radio.max_regen_health.unwrap(),
                radio.repair_cost.unwrap(),
            ))
        .ceil() as i32;

        assert_eq!(new_hp, 0);
        assert_eq!(cost, expected_max_cost);
        assert!(
            cost > ((max_hp as f64) * veh_def.repair_cost).ceil() as i32,
            "destroyed components must increase repair cost beyond hull-only damage"
        );
        assert_eq!(profile.vehicles[&t34_inv_id].repair, Some((cost, 0)));

        // Single destroyed track costs exactly 1x chassis max repair cost
        let one_track = VehicleComponentDamage {
            hull_hp: max_hp,
            left_track_hp: Some(0),
            ..Default::default()
        };
        let expected_track_cost = comp_max_cost(
            chassis.max_health.unwrap(),
            chassis.max_regen_health.unwrap(),
            chassis.repair_cost.unwrap(),
        )
        .ceil() as i32;
        let (track_cost, track_hp) =
            damage_vehicle_components(&mut profile, t34_inv_id, &one_track, &catalog)
                .expect("single track damage");
        assert_eq!(track_hp, max_hp);
        assert_eq!(track_cost, expected_track_cost);

        // Fully undamaged clears repair
        let undamaged = VehicleComponentDamage {
            hull_hp: max_hp,
            ..Default::default()
        };
        let (zero_cost, full_hp) =
            damage_vehicle_components(&mut profile, t34_inv_id, &undamaged, &catalog)
                .expect("undamaged clears repair");
        assert_eq!(zero_cost, 0);
        assert_eq!(full_hp, max_hp);
        assert!(profile.vehicles[&t34_inv_id].repair.is_none());
    }

    #[test]
    fn test_damage_vehicle_components_synthetic_regen_and_hull_clamp() {
        use crate::account_state::{AccountProfile, Preset, VehicleEntry};
        use crate::catalog::{Catalog, make_type_comp_descr};
        use crate::item_defs::{
            ComponentDef, DeviceHealthDef, TurretDef, TurretGunEntry, VehicleDef,
            make_int_comp_descr,
        };
        use std::collections::{BTreeMap, BTreeSet};

        let mk_comp = |id: u16, item_type_id: u8, max_h: i32, max_regen: i32, repair_cost: f64| ComponentDef {
            id,
            comp_descr: make_int_comp_descr(item_type_id, 0, id),
            name: format!("comp_{id}"),
            price: (0, 0),
            show_in_shop: true,
            weight: 100.0,
            level: 1,
            tags: BTreeSet::new(),
            max_load: Some(10_000.0),
            max_ammo: Some(50),
            shots: Vec::new(),
            max_health: Some(max_h),
            repair_cost: Some(repair_cost),
            max_regen_health: Some(max_regen),
            hit_tester: None,
            armor: None,
            engine_power: None,
            fire_starting_chance: None,
            radio_distance: None,
            max_climb_angle: None,
            rotation_speed: None,
            rotation_is_around_center: None,
            top_right_carrying_point: None,
            hull_position: None,
            shot_dispersion_factors: None,
            gun_shot_dispersion_factors: None,
            burst: None,
            effects: None,
        };

        let gun = TurretGunEntry {
            component: mk_comp(5, 4, 80, 50, 1.0),
            max_ammo: 50,
            shots: Vec::new(),
            pitch_limits: None,
            rotation_speed: None,
            reload_time: None,
            aiming_time: None,
            shot_disp_radius: None,
            hit_tester: None,
            armor: None,
            shot_dispersion_factors: None,
            burst: None,
            effects: None,
        };
        let turret = TurretDef {
            component: mk_comp(4, 3, 50, 50, 0.0),
            guns: vec![gun],
            yaw_limits: None,
            max_health: 50,
            gun_position: None,
            circular_vision_radius: None,
            rotation_speed: None,
            armor: None,
            primary_armor: Vec::new(),
            turret_rotator_health: Some(DeviceHealthDef {
                max_health: 80,
                max_regen_health: 40,
                repair_cost: 0.5,
            }),
            surveying_device_health: Some(DeviceHealthDef {
                max_health: 80,
                max_regen_health: 40,
                repair_cost: 0.5,
            }),
            hit_tester: None,
        };
        let veh_cd = make_type_comp_descr(0, 7);
        let veh_def = VehicleDef {
            nation: "ussr".into(),
            nation_id: 0,
            type_id: 7,
            name: "ussr:Synthetic".into(),
            veh_type_comp_descr: veh_cd,
            hull_weight: 5000.0,
            hull_max_health: 200,
            repair_cost: 2.0,
            tags: BTreeSet::new(),
            level: 5,
            default_player_emblem_id: 0,
            crew: Vec::new(),
            chassis: vec![mk_comp(1, 2, 100, 70, 1.0)],
            engines: vec![mk_comp(2, 5, 120, 80, 2.0)],
            fuel_tanks: vec![mk_comp(3, 6, 100, 60, 1.0)],
            radios: vec![mk_comp(6, 7, 90, 60, 0.5)],
            turrets: vec![turret],
            unlocks_descrs: Vec::new(),
            autounlocked_items: Vec::new(),
            installable_components: BTreeSet::new(),
            is_default_elite: false,
            price: (0, 0),
            show_in_shop: true,
            speed_limits: None,
            invisibility: None,
            hull_armor: None,
            hull_primary_armor: Vec::new(),
            hull_hit_tester: None,
            hull_position: None,
            turret_positions: Vec::new(),
            ammo_bay_health: Some(DeviceHealthDef {
                max_health: 100,
                max_regen_health: 60,
                repair_cost: 1.5,
            }),
            battle_def: None,
        };
        let stock_bytes = VehicleDescr::stock(&veh_def).expect("stock descr").to_bytes();
        let mut catalog = Catalog::default();
        catalog.vehicle_defs.insert(veh_cd, veh_def);

        let mut vehicles = BTreeMap::new();
        vehicles.insert(
            1,
            VehicleEntry {
                comp_descr: stock_bytes,
                shells: Vec::new(),
                shells_layout: Vec::new(),
                crew: Vec::new(),
                repair: None,
                eqs: [0; 3],
                eqs_layout: [0; 3],
                settings: 0,
                lock: 0,
            },
        );
        let mut profile = AccountProfile {
            schema_version: 1,
            database_id: 1,
            username: "Synthetic".into(),
            normalized_name: "synthetic".into(),
            preset: Preset::Retail,
            rev: 1,
            creation_time: 1000,
            last_battle_time: 0,
            credits: 0,
            gold: 0,
            free_xp: 0,
            slots: 1,
            berths: 0,
            attrs: 0,
            premium_expiry_time: 0,
            unlocks: BTreeSet::new(),
            elite_vehicles: BTreeSet::new(),
            double_xp_vehs: BTreeSet::new(),
            veh_type_xp: BTreeMap::new(),
            vehicles,
            tankmen: BTreeMap::new(),
            items: BTreeMap::new(),
            next_inv_id: 2,
            account_dossier: Vec::new(),
            account_dossier_change_time: 1000,
            vehicle_dossiers: BTreeMap::new(),
            applied_battles: Default::default(),
            pending_sys_messages: Vec::new(),
            language: None,
        };

        // Partial component damage clamps each device below max_regen_health up to max_regen_health.
        // Hull: (250 - 200) * 2.0 = 100.0
        // Ammo bay (hp=0 -> clamped to regen 60): (100 - 60) * 1.5 = 60.0
        // Engine (hp=100 > regen 80): (120 - 100) * 2.0 = 40.0
        // Left track (hp=10 -> clamped to regen 70): (100 - 70) * 1.0 = 30.0
        // Right track (hp=90 > regen 70): (100 - 90) * 1.0 = 10.0
        let partial = VehicleComponentDamage {
            hull_hp: 200,
            ammo_bay_hp: Some(0),
            engine_hp: Some(100),
            left_track_hp: Some(10),
            right_track_hp: Some(90),
            ..Default::default()
        };
        let (cost, hp) =
            damage_vehicle_components(&mut profile, 1, &partial, &catalog).expect("partial damage");
        assert_eq!((cost, hp), (240, 200));
        assert_eq!(profile.vehicles[&1].repair, Some((240, 200)));

        // Negative hull_hp clamps to 0; hull_hp > max_hp clears repair
        let clamped_zero = VehicleComponentDamage {
            hull_hp: -25,
            ..Default::default()
        };
        let (cost_zero, hp_zero) =
            damage_vehicle_components(&mut profile, 1, &clamped_zero, &catalog).expect("clamp low");
        assert_eq!((cost_zero, hp_zero), (500, 0));

        let full = VehicleComponentDamage {
            hull_hp: 999,
            ..Default::default()
        };
        let (cost_full, hp_full) =
            damage_vehicle_components(&mut profile, 1, &full, &catalog).expect("clamp high");
        assert_eq!((cost_full, hp_full), (0, 250));
        assert!(profile.vehicles[&1].repair.is_none());
    }
}
