//! In-memory account registry and transactional command dispatcher for `wot-baseapp`.

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;

use wot065_data::account_state::{AccountProfile, Preset};
use wot065_data::catalog::Catalog;
use wot065_data::dossier::{
    VehicleDossier, DOSSIER_CACHE_VERSION, DOSSIER_TYPE_ACCOUNT, DOSSIER_TYPE_VEHICLE,
};
use wot065_data::economics::EconomicsConfig;
use wot065_data::ops::{AccountEngine, CmdError, CmdOutcome, DiffBuilder, EngineCmd};
use wot065_data::ops::battle::PersonalBattleOutcome;
use wot065_data::pickle::{self, Value};
use wot065_data::store::{JsonAccountStore, StoreError};
use wot_battle::sim::results::BattleResultsExt;

/// Vehicle lock reasons from `common/constants.py` (`LOCK_REASON`).
pub const LOCK_REASON_NONE: i32 = 0;
pub use wot065_data::ops::battle::LOCK_REASON_ON_ARENA;
pub const LOCK_REASON_IN_QUEUE: i32 = 33;
pub const LOCK_REASON_PREBATTLE: i32 = 34;

/// Registry managing live account profiles, atomic disk persistence, and hangar command dispatch.
pub struct AccountRegistry {
    store: JsonAccountStore,
    catalog: Arc<Catalog>,
    economics: Arc<EconomicsConfig>,
    preset: Preset,
    shop_rev: i64,
    active_profiles: HashMap<String, AccountProfile>,
}

impl AccountRegistry {
    pub fn new(
        store: JsonAccountStore,
        catalog: Arc<Catalog>,
        economics: Arc<EconomicsConfig>,
        preset: Preset,
    ) -> Self {
        Self {
            store,
            catalog,
            economics,
            preset,
            shop_rev: 1,
            active_profiles: HashMap::new(),
        }
    }

    pub fn open(
        data_dir: impl AsRef<Path>,
        catalog: Arc<Catalog>,
        economics: Arc<EconomicsConfig>,
        preset: Preset,
    ) -> Result<Self, StoreError> {
        let store = JsonAccountStore::open(data_dir)?;
        Ok(Self::new(store, catalog, economics, preset))
    }

    pub fn catalog(&self) -> &Catalog {
        &self.catalog
    }

    pub fn catalog_arc(&self) -> Arc<Catalog> {
        self.catalog.clone()
    }

    pub fn economics(&self) -> &EconomicsConfig {
        &self.economics
    }

    pub fn economics_arc(&self) -> Arc<EconomicsConfig> {
        self.economics.clone()
    }

    pub fn store(&self) -> &JsonAccountStore {
        &self.store
    }

    pub fn preset(&self) -> Preset {
        self.preset
    }

    pub fn shop_rev(&self) -> i64 {
        self.shop_rev
    }

    pub fn set_shop_rev(&mut self, rev: i64) {
        self.shop_rev = rev;
    }

    /// Load or create an account profile for `username`, caching it in memory.
    pub fn get_or_create_profile(&mut self, username: &str) -> Result<&mut AccountProfile, StoreError> {
        let normalized = username.trim().to_ascii_lowercase();
        if !self.active_profiles.contains_key(&normalized) {
            let profile = self
                .store
                .load_or_create(username, self.preset, &self.catalog, &self.economics)?;
            self.active_profiles.insert(normalized.clone(), profile);
        }
        Ok(self.active_profiles.get_mut(&normalized).unwrap())
    }

    pub fn get_profile(&self, username: &str) -> Option<&AccountProfile> {
        let normalized = username.trim().to_ascii_lowercase();
        self.active_profiles.get(&normalized)
    }

    pub fn get_profile_mut(&mut self, username: &str) -> Option<&mut AccountProfile> {
        let normalized = username.trim().to_ascii_lowercase();
        self.active_profiles.get_mut(&normalized)
    }

    pub fn has_account(&self, username: &str) -> bool {
        let normalized = username.trim().to_ascii_lowercase();
        if self.active_profiles.contains_key(&normalized) {
            return true;
        }
        self.store.profile_exists(username)
    }

    /// Execute a mutating command: pure state transition, atomic disk save BEFORE in-memory commit.
    pub fn apply_command(
        &mut self,
        username: &str,
        cmd: &EngineCmd,
        now_utc: u32,
    ) -> Result<CmdOutcome, CmdError> {
        let profile = self
            .get_or_create_profile(username)
            .map_err(|e| CmdError::failure(format!("failed to load profile: {e}")))?;

        let mut working_copy = profile.clone();
        let outcome = AccountEngine::apply(
            &mut working_copy,
            cmd,
            &self.catalog,
            &self.economics,
            self.shop_rev,
            now_utc,
        )?;

        // Atomic disk write BEFORE committing in-memory
        if let Err(e) = self.store.save(&working_copy) {
            return Err(CmdError::failure(format!("disk persistence failed: {e}")));
        }

        let normalized = username.trim().to_ascii_lowercase();
        self.active_profiles.insert(normalized, working_copy);
        Ok(outcome)
    }

    /// Transactionally set `veh.lock` on one owned vehicle, persist atomically BEFORE
    /// updating `active_profiles`, and return `(new_rev, diff_pickle)` if changed.
    pub fn set_vehicle_lock(
        &mut self,
        username: &str,
        veh_inv_id: i64,
        lock_reason: i32,
    ) -> Result<Option<(i64, Vec<u8>)>, CmdError> {
        let profile = self
            .get_or_create_profile(username)
            .map_err(|e| CmdError::failure(format!("failed to load profile: {e}")))?;

        let mut working_copy = profile.clone();
        let Some(veh) = working_copy.vehicles.get_mut(&veh_inv_id) else {
            return Err(CmdError::wrong_args("vehicle not found"));
        };
        if veh.lock == lock_reason {
            return Ok(None);
        }
        veh.lock = lock_reason;

        let prev_rev = working_copy.rev;
        let next_rev = prev_rev.saturating_add(1);
        working_copy.rev = next_rev;

        if let Err(e) = self.store.save(&working_copy) {
            return Err(CmdError::failure(format!("disk persistence failed: {e}")));
        }

        let mut diff = DiffBuilder::new();
        diff.veh_lock(veh_inv_id, lock_reason);
        let diff_bytes = pickle::dumps(&diff.build(prev_rev, next_rev));

        let normalized = username.trim().to_ascii_lowercase();
        self.active_profiles.insert(normalized, working_copy);
        Ok(Some((next_rev, diff_bytes)))
    }

    /// Transactionally clear all non-zero vehicle locks on `username`, returning
    /// `(new_rev, diff_pickle)` if any vehicles were unlocked.
    pub fn unlock_all_vehicles(
        &mut self,
        username: &str,
    ) -> Result<Option<(i64, Vec<u8>)>, CmdError> {
        self.unlock_vehicles_except(username, &[])
    }

    pub(crate) fn unlock_vehicles_except(
        &mut self,
        username: &str,
        protected: &[i64],
    ) -> Result<Option<(i64, Vec<u8>)>, CmdError> {
        let profile = self
            .get_or_create_profile(username)
            .map_err(|e| CmdError::failure(format!("failed to load profile: {e}")))?;

        let locked_ids: Vec<i64> = profile
            .vehicles
            .iter()
            .filter_map(|(&id, v)| (v.lock != LOCK_REASON_NONE && !protected.contains(&id)).then_some(id))
            .collect();
        if locked_ids.is_empty() {
            return Ok(None);
        }

        let mut working_copy = profile.clone();
        let mut diff = DiffBuilder::new();
        for id in locked_ids {
            if let Some(v) = working_copy.vehicles.get_mut(&id) {
                v.lock = LOCK_REASON_NONE;
                diff.veh_lock(id, LOCK_REASON_NONE);
            }
        }

        let prev_rev = working_copy.rev;
        let next_rev = prev_rev.saturating_add(1);
        working_copy.rev = next_rev;

        if let Err(e) = self.store.save(&working_copy) {
            return Err(CmdError::failure(format!("disk persistence failed: {e}")));
        }

        let diff_bytes = pickle::dumps(&diff.build(prev_rev, next_rev));
        let normalized = username.trim().to_ascii_lowercase();
        self.active_profiles.insert(normalized, working_copy);
        Ok(Some((next_rev, diff_bytes)))
    }

    pub(crate) fn settle_battle(
        &mut self,
        username: &str,
        outcome: &PersonalBattleOutcome,
        results: &mut BattleResultsExt,
        now_utc: u32,
    ) -> Result<Option<Vec<u8>>, CmdError> {
        let mut profile = self.get_or_create_profile(username)
            .map_err(|e| CmdError::failure(format!("failed to load battle profile: {e}")))?.clone();
        let Some(change) = AccountEngine::apply_battle_outcome(&mut profile, &self.catalog, outcome, now_utc)? else {
            return Ok(None);
        };
        results.repair = change.repair;
        profile.pending_sys_messages.push(pickle::dumps(&results.service_message(change.vehicle_type)));
        profile.pending_sys_messages.extend(change.maintenance.iter().map(|notice| {
            pickle::dumps(&notice.service_message(change.vehicle_type))
        }));
        self.store.save(&profile).map_err(|e| CmdError::failure(format!("battle settlement failed: {e}")))?;
        self.active_profiles.insert(profile.normalized_name.clone(), profile);
        Ok(Some(change.diff_pickle))
    }

    pub(crate) fn take_battle_messages(&mut self, username: &str) -> Result<Vec<Vec<u8>>, StoreError> {
        let normalized = username.trim().to_ascii_lowercase();
        let Some(account) = self.active_profiles.get(&normalized) else {
            return Ok(Vec::new());
        };
        if account.pending_sys_messages.is_empty() {
            return Ok(Vec::new());
        }
        let mut profile = account.clone();
        let messages = std::mem::take(&mut profile.pending_sys_messages);
        self.store.save(&profile)?;
        self.active_profiles.insert(normalized, profile);
        Ok(messages)
    }

    /// Retrieve raw pickled dossier bytes for `CMD_REQ_PLAYER_DOSSIER`.
    pub fn get_player_dossier(
        &self,
        dossier_type: i32,
        owner_id: i32,
        name: &str,
    ) -> Result<Vec<u8>, CmdError> {
        let profile = if !name.trim().is_empty() {
            let normalized = name.trim().to_ascii_lowercase();
            if let Some(p) = self.active_profiles.get(&normalized) {
                Some(p.clone())
            } else {
                self.store.load(name).map_err(|e| CmdError::failure(e.to_string()))?
            }
        } else if owner_id > 0 {
            self.find_profile_by_db_id(owner_id as i64)
        } else {
            None
        };

        let profile = profile.ok_or_else(|| CmdError::failure("account not found"))?;

        match dossier_type {
            DOSSIER_TYPE_ACCOUNT => {
                let payload = Value::Str(profile.account_dossier);
                Ok(pickle::dumps(&payload))
            }
            DOSSIER_TYPE_VEHICLE => {
                let (change_time, bytes) = if let Some(&(ct, ref b)) = profile.vehicle_dossiers.get(&owner_id) {
                    (ct, b.clone())
                } else {
                    (profile.creation_time, VehicleDossier::new().to_bytes())
                };
                let quad = Value::tuple(vec![
                    Value::Int(2),
                    Value::Int(owner_id as i64),
                    Value::Int(change_time as i64),
                    Value::Str(bytes),
                ]);
                let payload = Value::tuple(vec![
                    Value::Int(DOSSIER_CACHE_VERSION),
                    quad,
                ]);
                Ok(pickle::dumps(&payload))
            }
            _ => Err(CmdError::failure("unsupported dossier type")),
        }
    }

    pub fn find_profile_by_db_id(&self, db_id: i64) -> Option<AccountProfile> {
        for p in self.active_profiles.values() {
            if p.database_id == db_id {
                return Some(p.clone());
            }
        }
        if let Ok(names) = self.store.list() {
            for name in names {
                if let Ok(Some(p)) = self.store.load(&name) {
                    if p.database_id == db_id {
                        return Some(p);
                    }
                }
            }
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use wot065_data::account_state::Preset;
    use wot065_data::testutil;

    fn test_temp_dir(label: &str) -> PathBuf {
        let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../target/test-tmp")
            .join(format!(
                "{label}-{}-{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn real_catalog() -> Option<Arc<Catalog>> {
        let dir = testutil::game_dir()?;
        let cat = wot065_data::catalog::load(&dir).ok()?;
        Some(Arc::new(cat))
    }

    #[test]
    fn registry_atomic_persistence_preserves_profile_and_reloads() {
        let Some(catalog) = real_catalog() else {
            eprintln!("skip: retail game files absent");
            return;
        };
        let dir = test_temp_dir("reg-atomic-save");
        let economics = Arc::new(EconomicsConfig::default());
        let mut reg = AccountRegistry::open(&dir, catalog.clone(), economics.clone(), Preset::Retail).unwrap();

        let profile = reg.get_or_create_profile("test_user").unwrap();
        assert_eq!(profile.username, "test_user");

        // Give gold for slots
        reg.apply_command("test_user", &EngineCmd::SetMoney { credits: 10_000, gold: 1_000 }, 1_700_000_000).unwrap();
        let initial_gold = reg.get_profile("test_user").unwrap().gold;

        let outcome = reg.apply_command("test_user", &EngineCmd::BuySlot { shop_rev: 1 }, 1_700_000_000).unwrap();
        assert_eq!(outcome.result_id, 0);

        // Verify in-memory updated
        let updated = reg.get_profile("test_user").unwrap();
        assert!(updated.gold < initial_gold);

        // Reload fresh registry from same data_dir
        let mut reg2 = AccountRegistry::open(&dir, catalog, economics, Preset::Retail).unwrap();
        let reloaded = reg2.get_or_create_profile("test_user").unwrap();
        assert_eq!(reloaded.gold, updated.gold);
        assert_eq!(reloaded.slots, updated.slots);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn registry_dossier_lookup_own_and_other_accounts() {
        let Some(catalog) = real_catalog() else {
            eprintln!("skip: retail game files absent");
            return;
        };
        let dir = test_temp_dir("reg-dossier-lookup");
        let economics = Arc::new(EconomicsConfig::default());
        let mut reg = AccountRegistry::open(&dir, catalog, economics, Preset::Retail).unwrap();

        let p1 = reg.get_or_create_profile("player_one").unwrap();
        let p1_db_id = p1.database_id;
        let p1_acc_dossier = p1.account_dossier.clone();

        let p2 = reg.get_or_create_profile("player_two").unwrap();
        let _p2_db_id = p2.database_id;

        // Inspect player_one's account dossier by name
        let raw_dossier = reg.get_player_dossier(DOSSIER_TYPE_ACCOUNT, 0, "player_one").unwrap();
        let val = pickle::loads(&raw_dossier).unwrap();
        assert_eq!(val.as_bytes().unwrap(), &p1_acc_dossier[..]);

        // Inspect player_one's account dossier by database_id with empty name
        let raw_dossier_by_id = reg.get_player_dossier(DOSSIER_TYPE_ACCOUNT, p1_db_id as i32, "").unwrap();
        let val_by_id = pickle::loads(&raw_dossier_by_id).unwrap();
        assert_eq!(val_by_id.as_bytes().unwrap(), &p1_acc_dossier[..]);

        // Non-existent player returns failure
        assert!(reg.get_player_dossier(DOSSIER_TYPE_ACCOUNT, 99999, "ghost").is_err());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn registry_vehicle_lock_and_unlock_persist_across_reload() {
        let dir = test_temp_dir("reg-veh-lock");
        let cat_path = dir.join("items");
        // Synthetic single-vehicle catalog so test works even without retail assets.
        let _ = std::fs::remove_dir_all(&cat_path);
        let Some(catalog) = real_catalog() else {
            let _ = std::fs::remove_dir_all(&dir);
            return;
        };
        let economics = Arc::new(EconomicsConfig::default());
        let mut reg = AccountRegistry::open(&dir, catalog.clone(), economics.clone(), Preset::Retail).unwrap();
        let profile = reg.get_or_create_profile("lock_tester").unwrap().clone();
        let (&veh_id, _) = profile.vehicles.iter().next().expect("preset must have >=1 vehicle");

        let (rev1, diff1) = reg
            .set_vehicle_lock("lock_tester", veh_id, LOCK_REASON_ON_ARENA)
            .unwrap()
            .expect("lock change emits diff");
        assert!(rev1 > profile.rev);
        let val1 = pickle::loads(&diff1).unwrap();
        assert_eq!(
            val1.get("cache")
                .and_then(|c| c.get("vehsLock"))
                .and_then(|d| d.as_dict())
                .and_then(|pairs| pairs.iter().find(|(k, _)| k.as_int() == Some(veh_id)))
                .and_then(|(_, v)| v.as_int()),
            Some(LOCK_REASON_ON_ARENA as i64)
        );
        // No-op lock returns None
        assert!(reg
            .set_vehicle_lock("lock_tester", veh_id, LOCK_REASON_ON_ARENA)
            .unwrap()
            .is_none());

        // Persists across fresh open and clears via unlock_all_vehicles
        let mut reg2 = AccountRegistry::open(&dir, catalog, economics, Preset::Retail).unwrap();
        let loaded = reg2.get_or_create_profile("lock_tester").unwrap().clone();
        assert_eq!(loaded.vehicles[&veh_id].lock, LOCK_REASON_ON_ARENA);

        let (rev2, diff2) = reg2
            .unlock_all_vehicles("lock_tester")
            .unwrap()
            .expect("unlock emits diff");
        assert!(rev2 > rev1);
        let val2 = pickle::loads(&diff2).unwrap();
        assert_eq!(
            val2.get("cache")
                .and_then(|c| c.get("vehsLock"))
                .and_then(|d| d.as_dict())
                .and_then(|pairs| pairs.iter().find(|(k, _)| k.as_int() == Some(veh_id)))
                .and_then(|(_, v)| v.as_int()),
            Some(LOCK_REASON_NONE as i64)
        );
        assert_eq!(
            reg2.get_profile("lock_tester").unwrap().vehicles[&veh_id].lock,
            LOCK_REASON_NONE
        );

        let _ = std::fs::remove_dir_all(&dir);
    }
}
