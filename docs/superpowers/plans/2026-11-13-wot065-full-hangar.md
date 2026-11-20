# WoT 0.6.5 Full Hangar Server Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use `subagent-driven-development` (recommended) or `executing-plans` to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

- **Goal:** Implement full vanilla World of Tanks 0.6.5 hangar functionality (vehicle/item trading, research/unlocks, module/ammo/optdev/equipment/camouflage fitting, full crew & barracks operations, repairs, gold/XP/premium economy, binary dossiers, `retail`/`sandbox` presets, persistent per-account JSON state, single-owner duplicate-login kick, WG dev commands, and offline `wot-server account` CLI) matching [2026-11-13-wot065-full-hangar-design.md](file:///c:/Users/qwerty/Documents/dstest/docs/superpowers/specs/2026-11-13-wot065-full-hangar-design.md).
- **Architecture:** Extend `wot065-data` with complete `item_defs` loading, binary descriptor & dossier codecs, economics config, `AccountProfile`/presets, pure deterministic `AccountEngine` + `AccountDiff` builder, and atomic `JsonAccountStore`. Keep `bw201-login` writing unique non-zero `session_key`s + `.pending/` handoffs alongside `--session-out`. Keep `wot-baseapp` as the async Mercury UDP adapter + single-owner `AccountRegistry`. Wire CLI flags and offline `wot-server account` subcommands in `wot-server`.
- **Tech Stack:** Rust 2021 (`wot065-data`, `bw201`, `wot-baseapp`, `wot-server`), `serde`/`serde_json`, `flate2`, `crc32fast`, `clap`, `tokio`, BigWorld 2.0.1 Mercury UDP + Python-2 protocol-2 pickles.

---

## Global Constraints

1. **Windows Low Integrity Level (`0x1000`):** Never call `std::env::temp_dir()` anywhere in production or test code. All account temp files live inside `--data-dir` (default `.scratch/accounts`); all `.pending` handoff temp files live inside `<session_out>.pending/`; all unit/integration test scratch dirs live under `target/test-tmp/`.
2. **Process Lock Guard:** Before building or running tests (`cargo check --workspace` / `cargo test --workspace`), check for running `wot-server.exe`, `bw201-login.exe`, or `wot-baseapp.exe` and stop them if active (`Access is denied` on `.exe` overwrite otherwise). Note inside the sandbox `cargo build --release` is unsupported on `zerocopy`; use dev `cargo check` / `cargo test --workspace`.
3. **No Silent Fixture Auto-Loading:** `wot-server serve` must never auto-load `.scratch/research/decompiled/_generated/*` unless passed explicitly via `--account-diff`/`--shop-cache`/etc.
4. **Fail-Fast Session Handoff:** `bw201-login` must return `LoginRejectedServerNotReady` instead of `LOGGED_ON` if persisting `--session-out` or `.pending/` fails.
5. **Exact WoT 0.6.5 Protocol Invariants:**
   - Do not modify existing comments/docstrings unrelated to changes.
   - `Account` client methods: `0x82` `onCmdResponse`, `0x83` `onCmdResponseExt`, `0x84` `onKickedFromServer`, `0x90` `showGUI`, `0x92` `receiveServerStats`, `0x95` `update`, `0x96` `resyncDossiers`.
   - `CMD_REQ_PLAYER_DOSSIER` (`503`) and `CMD_REQ_PLAYER_CLAN_INFO` (`504`) **MUST** reply via 2-arg `onCmdResponse` (never `onCmdResponseExt`).
   - `CMD_SYNC_DOSSIERS` (`600`) **MUST ALWAYS** reply via `RES_STREAM` (`1`), even when the delta list is empty.
   - `CMD_BUY_TMAN` (`303`) **MUST** reply via `onCmdResponseExt` with `{'tmanInvID': i64, 'tmanCompDescr': bytes}`.
   - Incremental mutations **MUST** persist atomically to JSON first, then send `Account.update(diff)` (with `prevRev` + `rev`, `None` deletions, `_r` set replacements) **before** `onCmdResponse`/`onCmdResponseExt` in the same reliable reply bundle.

## Review Focus

Special verification attention during implementation and final review:
- **Traversal & Codec Parity:** `VehicleType.unlocksDescrs` ordering (`root` -> `chassis` -> `engines` -> `fuelTanks` -> `radios` -> per-turret `guns` then `unlocks`), `VehicleDescr` optDev mask (`slot0=4, slot1=2, slot2=1`) + descending trailing `<H` slot order `[slot2, slot1, slot0]`, `TankmanDescr` (`19 + K + len(dossier)`), and binary dossiers v19/v17/v10 byte offsets.
- **UI Expectancy Quirks:** `inventory` keys `1..=11` all present; `inventory[1]` has all 9 subdicts; `shop['items'][nation][1]` keyed by `innationIdx` (`2..=7, 9..=11` by `compDescr`); empty shop pairs `({}, set())`; `freeXPConversion = (25, 1)`; `berthsPrices = (16, 16, [300])`; future `premiumExpiryTime > now` when `ACCOUNT_ATTR.PREMIUM` (`1i64 << 32`) is set; `CMD_SELL_VEHICLE` matching `Shop.__getVehiclePriceFromCache` + requiring `repair.is_none()`.
- **Multi-Client & Kick Lifecycle:** Concurrent logins before BaseApp arrival don't overwrite `.pending/`; duplicate login kick on `ENABLE_ENTITIES` transmits `onKickedFromServer` to old peer and revokes old `session_key` so trailing datagrams cannot bounce-kick the new owner.

---

### Task 1: Full Static Game Data & XML Parsers (`wot065-data`)

**Files:**
- Modify: `crates/wot065-data/src/item_defs.rs`
- Modify: `crates/wot065-data/src/catalog.rs`

- [ ] **Step 1: Write failing tests for expanded XML parsing in `crates/wot065-data/src/catalog.rs` and `item_defs.rs`**
  Add tests against `C:\Users\qwerty\Desktop\World_of_Tanks` (skipping gracefully only if retail `game_dir` absent, running strictly in CI/local dev where installed) verifying:
  - `T-34` (`nation=0, id=0`): has >1 turrets, >1 guns across turrets, `repairCost > 0`, `hull.weight > 0`, `hull.maxHealth > 0`, non-empty `unlocks_descrs`, `is_default_elite == false`, exact `unlocks_descrs[0]` target & XP cost matching `vehicles/ussr/T-34.xml`, `autounlocked_items` contains stock chassis/engine/fuel/radio/turret/gun.
  - `MS-1` (`nation=0, id=13`): `<price>0</price>`, `level == 1`, `is_default_elite == false`, `repairCost`, crew roles.
  - `OptionalDeviceDef`: `toolbox` has `removable == true, weight == 100.0`; `enhancedSuspension_Christie` has `chassis_max_load_factor > 1.0`; `antifragmentationLining_light` has `VehicleFilter` including `lightTank lightSPG lightAT-SPG`.
  - `EquipmentDef`: `lendLeaseOil` has `tags` containing `"fuel"`, `incompatible_installed` containing `"fuel"`, `VehicleFilter` restricting to `nations: [0]`.
  - `CamouflageDef` from `vehicles/<nation>/customization.xml`: valid `id`, `gold`, `show_in_shop`, `allow` / `deny` sets.
  - `TankmenConfig`: `role_ranks` per role (`commander` max rank = 8 for USSR, 10 for Germany), weighted `normal_groups` and `premium_groups` (`women1` has `is_female == true` with valid first/last/icon IDs).
  - `PlayerEmblems`: `defaultPlayerEmblemID` lookup works (`ussr_star -> 1`, `germany_cross -> 2`, `usa_star -> 3`).

- [ ] **Step 2: Run tests to confirm compilation error / failure**
  Run: `cargo test -p wot065-data --lib`
  Expected: fail because new catalog types/fields don't exist yet.

- [ ] **Step 3: Implement full item-defs structures & parsers in `crates/wot065-data/src/item_defs.rs` & `catalog.rs`**
  - Add clean data models:
    - `ComponentDef { id: u16, comp_descr: i32, name: String, price: (i64, i64), show_in_shop: bool, weight: f64, level: u8, tags: BTreeSet<String>, max_load: Option<f64>, max_ammo: Option<u32>, shots: Vec<ShotDef> }`
    - `TurretDef { component: ComponentDef, guns: Vec<TurretGunEntry> }` where `TurretGunEntry` preserves local `max_ammo`, `shots`, `unlocks`, etc., overriding shared values cleanly.
    - `UnlockEntry { xp_cost: i64, target_cd: i32, prereq_cds: Vec<i32> }`
    - `VehicleDef` storing `hull_weight: f64`, `hull_max_health: i32`, `repair_cost: f64`, `tags: BTreeSet<String>`, `level: u8`, `default_player_emblem_id: u8`, `crew: Vec<CrewSlotSpec>`, `chassis`, `engines`, `fuel_tanks`, `radios`, `turrets`, `unlocks_descrs: Vec<UnlockEntry>`, `autounlocked_items: Vec<i32>`, `installable_components: BTreeSet<i32>`, `is_default_elite: bool`.
    - Follow exact `VehicleType.__init__` XML traversal order for `unlocks_descrs`:
      1. Vehicle root `<unlocks>` (`prereq_cds = []`)
      2. `<chassis>` children in order -> per-item `<unlocks>` (`prereq_cds = [chassis_cd]`)
      3. `<engines>` -> `<unlocks>` (`[engine_cd]`)
      4. `<fuelTanks>` -> `<unlocks>` (`[fuel_tank_cd]`)
      5. `<radios>` -> `<unlocks>` (`[radio_cd]`)
      6. `<turrets0>` (per turret slot): for each turret in order: if local or inline turret (i.e. NOT reusing shared turret without `<guns>`/`<unlocks>` override), parse `<guns>` FIRST (`prereq_cds = [gun_cd, turret_cd]`, using `max_ammo` override if present), THEN parse turret's own `<unlocks>` (`[turret_cd]`). *(Preserve helper decomposition: shared XML subtree parser for component lists.)*
    - Parse `VehicleFilter` (`exclude: Vec<FilterPattern>`, `include: Vec<FilterPattern>`) with exact `common/items/artefacts.py:295-414` semantics (check `exclude` before `include`; empty list passes; match `nations`, `vehicle.{tags,min_level,max_level}`, and `chassis`/`engine`/`fuelTank`/`radio` `{tags,min_level,max_level}`).
    - Parse `vehicles/common/optional_devices.xml` (`removable`, `weight`, `vehicleWeightFraction`, `chassisMaxLoadFactor`), `equipments.xml` (`tags`, `incompatibleTags/installed`), `vehicles/<nation>/customization.xml`, `vehicles/common/player_emblems.xml`, `vehicles/common/horns.xml`, and `tankmen/{tankmen,ussr,germany,usa}.xml` (`roleRanks`, weighted `normalGroups`, weighted `premiumGroups`).
  - Keep existing `Catalog::load`, `Catalog::vehicles`, `make_type_comp_descr`, `parse_type_comp_descr`, `ComponentIds`, etc. working seamlessly (adding richer fields & lookup maps by `veh_type_comp_descr` and `item_comp_descr`).

- [ ] **Step 4: Run `wot065-data` unit tests & `cargo check --workspace`**
  Run: `cargo test -p wot065-data --lib`
  Expected: all catalog parsing tests PASS against retail `item_defs`.

---

### Task 2: Binary `VehicleDescr` & `TankmanDescr` Codecs + Validation (`descr.rs`)

**Files:**
- Create: `crates/wot065-data/src/descr.rs`
- Modify: `crates/wot065-data/src/catalog.rs` (delegate `make_comp_descr`/`parse_comp_descr` to `descr.rs`)
- Modify: `crates/wot065-data/src/lib.rs`

- [ ] **Step 1: Write failing tests in `crates/wot065-data/src/descr.rs`**
  - `VehicleDescr` round-trip tests:
    - 15-byte stock descriptor matches previous `make_comp_descr`.
    - Slot 0 optdev only (`flags = 0x04`), Slot 2 optdev only (`flags = 0x01`), all 3 slots `[optA, optB, optC]` (`flags = 0x07`) verifying trailing `<H` order on wire is **`[optC_u16_le, optB_u16_le, optA_u16_le]`**.
    - Flag bit combinations: `optDevs` + non-default `emblemID` (`bit 4 = 16`, `+1B`) + `hornID` (`bit 6 = 64`, `+1B`) + `camouflageID` (`bit 7 = 128`, `+2B <H`), verifying total length and field order (`optDevs`, then `emblem`, then `horn`, then `camo`).
  - Weight & fitting tests against real `Catalog`:
    - Exceeding `chassis.maxLoad` blocks heavy turret/gun swap (`TooHeavy`).
    - Mounting `EnhancedSuspension` raises `max_weight` by `chassis.maxLoad * (factor - 1.0)` enabling heavy turret swap; attempting to **remove** `EnhancedSuspension` while over stock `maxLoad` fails (`TooHeavy`)!
    - Duplicate optional device across slots or incompatible `VehicleFilter` rejected.
    - Equipment `incompatibleTags` (`lendLeaseOil` + another `fuel`, or duplicate self) rejected.
    - Camouflage `deny` / `allow` vehicle set checks enforced.
  - `TankmanDescr` round-trip & formula tests:
    - Exact binary layout `19 + numSkills + len(dossier)` (`header = 8 | (nation<<4)`, `flags = isFemale | (isPremium << 1)`, `<H` names/icon, `rankIdx`, `numLevelsToNextRank`, `<i` `freeXP`, tail dossier).
    - `level_up_xp_cost(from_level, skill_seq_num)` matches `round(2^seq * 50.0 * 100.0^(from_level/100.0))` across `[0, 50, 99]`, `seq in [0, 1, 2]`.
    - `add_xp`, `add_skill` (requires `roleLevel == 100` & empty or `lastSkillLevel == 100`, consumes `freeXP` into new skill automatically!), `drop_skill` (sets `lastSkillLevel` to `0` if empty or `100` if dropping last, preserves `freeXP`), `respecialize` (class change vs same class role level loss clamped at `>= 50`, recalculates `rankIdx`/`numLevelsToNextRank`, applies `add_xp(0)`), and `replace_passport` (validates `(first, last, icon)` in one group matching `isFemale`, including `<women1>` in `premiumGroups`).

- [ ] **Step 2: Run tests to verify failure**
  Run: `cargo test -p wot065-data descr`

- [ ] **Step 3: Implement `crates/wot065-data/src/descr.rs` and export in `lib.rs`**
  Implement `IntCompDescr`, `VehicleDescr`, `TankmanDescr`, weight/filter helpers (`may_install_component`, `may_install_turret`, `may_install_optdev`, `may_remove_optdev`, `check_equipments`, `check_camouflage`, `veh_get_devices` matching `vehicles.py:414-438`), and rank/XP/passport methods cleanly decomposed into small helpers. Keep `catalog::make_comp_descr` and `catalog::parse_comp_descr` compiling cleanly (routing 15B legacy callers if needed, and updating `parse_comp_descr` or providing `VehicleDescr::from_bytes`).

- [ ] **Step 4: Run `cargo test -p wot065-data` and verify all tests pass**

---

### Task 3: Binary Dossiers (`Account` v19, `Vehicle` v17, `Tankman` v10) (`dossier.rs`)

**Files:**
- Create: `crates/wot065-data/src/dossier.rs`
- Modify: `crates/wot065-data/src/lib.rs`

- [ ] **Step 1: Write failing tests in `crates/wot065-data/src/dossier.rs`**
  - Empty minimal `AccountDossier::new(creation_time)` encodes `154` bytes: `version=19` LE `@0..2`, `creationTime` LE `@2..6`, `_dynRecPos_account = (150, 152)` at `@146..150`, empty `vehTypeFrags` `00 00` `@150..152`, empty `vehDossiersCut` `00 00` `@152..154`.
  - Populated `AccountDossier` with 2 `vehTypeFrags` and 2 `vehDossiersCut` encodes `154 + 12 + 24 = 190` bytes, updates `_dynRecPos_account` offsets `(150, 150 + 2 + 6*2 = 164)`, and decodes identically via `AccountDossier::from_bytes`.
  - Empty minimal `VehicleDossier::new()` encodes `138` bytes (`_version = 17`, `_dynRecPos_vehicle = 136` `@134..136`, empty `vehTypeFrags` `00 00` `@136..138`) and round-trips with populated `vehTypeFrags`.
  - Empty `TankmanDossier::new()` encodes exact `132` bytes (`_version = 10`, zero dynamic records) and round-trips.
  - `build_dossier_sync_payload` builds exact `(DOSSIER_CACHE_VERSION, flat_quads)` `Value::Tuple`.

- [ ] **Step 2: Run tests to verify failure**
  Run: `cargo test -p wot065-data dossier`

- [ ] **Step 3: Implement `crates/wot065-data/src/dossier.rs`**
  Implement `DOSSIER_TYPE_ACCOUNT = 1`, `DOSSIER_TYPE_VEHICLE = 2`, `DOSSIER_TYPE_TANKMAN = 3`, `DOSSIER_CACHE_VERSION` (use consistent cache version e.g. `1`), `AccountDossier`, `VehicleDossier`, `TankmanDossier`, exact static table read/write matching `common/dossiers/__init__.py` byte offsets, and `DossierSyncRecord { dossier_type, owner_id, change_time, comp_descr }` + `build_dossier_sync_value`. *(Bonus parity check: add helper in test or script to verify lengths & version unpacking if Python 2.7/system python is available outside sandbox, and unit-test exact offsets in Rust.)*

- [ ] **Step 4: Run `cargo test -p wot065-data dossier`**
  Expected: PASS.

---

### Task 4: Economics Config (`economics.rs`), `AccountProfile`, Presets (`account_state.rs`) & Fixed `Shop`/`FullSync`

**Files:**
- Create: `crates/wot065-data/src/economics.rs`
- Create: `crates/wot065-data/src/account_state.rs`
- Modify: `crates/wot065-data/src/account.rs`
- Modify: `crates/wot065-data/src/lib.rs`
- Modify: `crates/wot065-data/src/main.rs` (if needed for any new flags/types)

- [ ] **Step 1: Write failing tests for economics validation, `Retail` & `Sandbox` presets, `full_sync`, and `shop_cache`**
  - `EconomicsConfig::default()` passes `validate()`, checks `free_xp_conversion == (25, 1)`, `berths_prices == (16, 16, [300])`, `slots_prices == (6, [300])`, 3 ascending `tankman_cost` tiers with `baseRoleLoss`/`classChangeRoleLoss`/`isPremium`. Invalid override JSON (e.g. `free_xp_conversion: [0, 1]` or unsorted `tankman_cost`) returns clear error.
  - `AccountProfile::new_with_preset(..., Preset::Retail, ...)`:
    - Exactly 3 vehicles (`ussr:MS-1`, `germany:Ltraktor`, `usa:T1_Cunningham`), each stock config with `getDefaultAmmoForGun`, 100% male crews with non-empty `132B` TankmanDossier v10 tail.
    - `slots = 6`, `berths = 16`, `credits = 10_000`, `gold = 0`, `free_xp = 0`.
    - `unlocks` contains ONLY the 3 starter `vehTypeCompDescr`s and their `autounlocked_items`.
    - `account_dossier` is valid v19 (`>= 154B`) with `creation_time == now_utc`.
  - `AccountProfile::new_with_preset(..., Preset::Sandbox, ...)`:
    - Same 3 starter vehicles, `credits = 50_000_000`, `gold = 1_000_000`, `free_xp = 5_000_000`, `veh_type_xp` seeded with `500_000` on starter + default-elite vehicles, starter tankmen have `free_xp = 1_000_000`.
  - `build_full_sync_from_profile`:
    - All `inventory` keys `1..=11` exist.
    - All 9 subdicts in `inventory[1]` exist.
    - `stats['freeVehiclesLeft'] == 1000`, `stats['freeTMenLeft'] == 1000`.
    - When `ACCOUNT_ATTR.PREMIUM` (`1i64 << 32`) set, `account['premiumExpiryTime'] > 0` (`assert` in `CommonPage.py:672`).
  - `build_shop_cache`:
    - `items[0][1]` keyed by `innation_id`; `items[15][9]` and `items[15][11]` keyed by `comp_descr`; empty pairs `(Dict([]), Set([]))`; `camouflageCost` populated per nation.

- [ ] **Step 2: Run tests to verify failure**
  Run: `cargo test -p wot065-data account`

- [ ] **Step 3: Implement `economics.rs`, `account_state.rs`, and refactor `account.rs`**
  - `economics.rs`: `EconomicsConfig`, `TankmanCostTier`, `TradeFees`, `PresetStarting`, `load_from_file`, `validate`, `next_slot_price(slots)`, `next_berth_pack_price(berths)`, `sell_price_credits(buy_credits, buy_gold)` (`ceil(sell_price_modif * (buy_credits + buy_gold * exchange_rate)) as i64`).
  - `account_state.rs`: `Preset` (`Retail`, `Sandbox`, `FromStr`/`ValueEnum`), `AccountProfile`, `VehicleEntry`, `TankmanEntry`, `create_profile(...)`, `build_full_sync_from_profile`, `build_shop_cache`, `collect_dossier_records(profile, version, max_change_time)`.
  - Update existing `account.rs` helpers (`build_full_sync`, `build_shop_cache_for_catalog`) to delegate cleanly to the corrected logic so all existing tests & `crates/wot065-data/src/main.rs` work cleanly (updating `account.rs`'s unit test expecting old buggy `freeXPConversion=(25,100)` or `130` starter vehicles if any — preserve backwards helper signatures where practical, e.g., `FullSyncOptions` tests).

- [ ] **Step 4: Run `cargo test -p wot065-data` and verify PASS**

---

### Task 5: Pure `AccountEngine` — Diff Builder, Shop/Economy & Vehicle Buy/Sell (`ops/mod.rs`, `ops/shop_economy.rs`)

**Files:**
- Create: `crates/wot065-data/src/ops/mod.rs`
- Create: `crates/wot065-data/src/ops/shop_economy.rs`
- Modify: `crates/wot065-data/src/lib.rs`

- [ ] **Step 1: Write failing tests in `ops/shop_economy.rs`**
  - `DiffBuilder` emits `{'prevRev': old_rev, 'rev': new_rev, ...}`, maps deleted keys to `Value::None`, emits `stats['unlocks']` additions and `unlocks_r` replacements properly.
  - `CMD_BUY_SLOT` (`205`), `CMD_BUY_BERTHS` (`206`), `CMD_EXCHANGE` (`202`), `CMD_PREMIUM` (`204`, sets `1i64 << 32` + extends `premiumExpiryTime > now_utc`).
  - `CMD_FREE_XP_CONV` (`203`): checks divisibility by `25`, checks `elite` status, deducts listed `vehTypeXP` in order, computes exact `gold_cost`, rejects non-elite or duplicate/insufficient XP.
  - `CMD_UNLOCK` (`201`): unlocks module (requires prereqs), unlocks next vehicle (expands `autounlocked_items` into `unlocks`, creates initial empty `VehicleDossier`, sets `resync_dossiers = true`), drains `vehTypeXP` then `freeXP`, automatically marks source tank in `elite_vehicles` once all its `unlocks_descrs` targets are unlocked!
  - `CMD_BUY_VEHICLE` (`301`): checks unlocked & in-shop & `vehicles.len() < slots`, handles `BUY_VEHICLE_FLAG::CREW` (`1`) and `BUY_VEHICLE_FLAG::SHELLS` (`16`), allocates new `vehInvID` & `tmanInvID`s, sets `resync_dossiers = true`, ignores daily limits.
  - `CMD_SELL_VEHICLE` (`304`):
    - Rejects broken tank (`repair.is_some()` -> `RES_FAILURE`).
    - Rejects if `!(flags & CREW)` and barracks cannot hold seated crew.
    - Computes exact `Shop.__getVehiclePriceFromCache` sell payout for vehicle with upgraded gun + turret (installed turret's `guns[0]` baseline!) + mounted optDevs (sold with tank, removed), handles `SHELLS (16)` sell vs `depot[10]` return, `EQS (32)` sell vs `depot[11]` return, emits `None` deletions across all 9 `inventory[1]` keys.
  - `CMD_BUY_ITEM` (`302`) & `CMD_SELL_ITEM` (`305`) including shop unlock gate for `2..=7` and no unlock gate for `9..=11`, depot deletion via `None` when reaching `0`, `RES_SHOP_DESYNC (-4)` with `ext = {'shopRev': server_shop_rev}` on shop rev mismatch.

- [ ] **Step 2: Run tests to verify failure**
  Run: `cargo test -p wot065-data ops`

- [ ] **Step 3: Implement `ops/mod.rs` & `ops/shop_economy.rs`**
  - Typed `EngineCmd` covering all command IDs (`101..=107, 120, 121, 151..=154, 201..=206, 301..=306, 10001..=10004`).
  - Transactional validation: compute and validate everything BEFORE mutating `AccountProfile`; on success, increment `profile.rev += 1`, apply mutations + build minimal `AccountDiff` (`DiffBuilder`) atomically.
  - Export shared helpers (`require_shop_rev`, `deduct_currency`, `check_unlock_prerequisites`, `recompute_elite_vehicles`) inside `ops/mod.rs` so no duplication occurs.

- [ ] **Step 4: Run `cargo test -p wot065-data ops::shop_economy`**
  Expected: all tests PASS.

---

### Task 6: `AccountEngine` — Fitting, Crew & Dev Commands (`ops/fitting.rs`, `ops/crew.rs`, `ops/dossiers_dev.rs`)

**Files:**
- Create: `crates/wot065-data/src/ops/fitting.rs`
- Create: `crates/wot065-data/src/ops/crew.rs`
- Create: `crates/wot065-data/src/ops/dossiers_dev.rs`
- Modify: `crates/wot065-data/src/ops/mod.rs`

- [ ] **Step 1: Write failing tests for fitting, crew & dev commands**
  - `ops/fitting.rs`:
    - `CMD_EQUIP` (`101`): chassis/engine/fuel/radio/gun install from depot returns old module +1 to depot; turret install with `gunCD == 0` keeps gun, turret install with `gunCD != current_gun` requires depot stock for new gun and unloads old `shells` back to `depot[10]` (restoring saved `shells_layout` for `(new_turret, new_gun)` if depot permits); blocks overweight / broken vehicle / missing depot item.
    - `CMD_EQUIP_OPTDEV` (`102`): install into free slot, install overwriting occupied slot (respecting removable vs non-removable), remove removable (returns to depot, checks weight after removal!), remove non-removable with `isPaidRemoval=1` (deducts `10` gold, returns to depot), remove non-removable with `isPaidRemoval=0` (destroys, depot unchanged).
    - `CMD_EQUIP_SHELLS` (`103`) & `CMD_EQUIP_EQS` (`104`): gun shot compatibility, `maxAmmo` limit, delta refund/deduct from depot, `eqs` pairwise `incompatibleTags` rejection.
    - `CMD_REPAIR` (`106`), `CMD_VEH_SETTINGS` (`107`, elite gate for `XP_TO_TMAN=1`), `CMD_VEH_CAMOUFLAGE` (`120`, gold deduction, clear when `0`, `allow`/`deny` checks), `CMD_VEH_HORN` (`121` -> `RES_FAILURE`).
  - `ops/crew.rs`:
    - `CMD_BUY_TMAN` (`303`): validates role for vehicle type, berths `< berths`, creates male tankman with v10 dossier tail, emits `ext = {'tmanInvID', 'tmanCompDescr'}` and `resync_dossiers = true`.
    - `CMD_EQUIP_TMAN` (`105`): unload (`-1`), seat, cross-vehicle move clearing previous slot, 1-for-1 swap succeeds even when barracks full (`delta = 0`), unloading into full barracks fails!
    - `CMD_DISMISS_TMAN` (`306`), `CMD_TMAN_RESPEC` (`153`, credits/gold/free tier losses), `CMD_TMAN_ADD_SKILL` (`151`), `CMD_TMAN_DROP_SKILL` (`152`), `CMD_TMAN_PASSPORT` (`154`, `-1` fields keep existing, female `premiumGroups` group check, gold deducted).
  - `ops/dossiers_dev.rs`:
    - `CMD_SET_MONEY` (`10001`), `CMD_ADD_XP` (`10002`), `CMD_ADD_TMAN_XP` (`10003`), `CMD_UNLOCK_ALL` (`10004` emits `unlocks_r` + `eliteVehicles_r` + empty v17 dossiers + `resync_dossiers`).

- [ ] **Step 2: Run tests to verify failure**
  Run: `cargo test -p wot065-data ops`

- [ ] **Step 3: Implement `fitting.rs`, `crew.rs`, `dossiers_dev.rs`, wire into `ops/mod.rs` dispatch**
  Keep each command handler crisp with shared depot delta helpers (`depot_take`, `depot_give`) cleanly emitting `inventory[type][item] = count | None` diffs.

- [ ] **Step 4: Run `cargo test -p wot065-data`**
  Expected: all `wot065-data` unit tests PASS.

---

### Task 7: Atomic `JsonAccountStore` (`store.rs`) & Offline Account Helpers

**Files:**
- Create: `crates/wot065-data/src/store.rs`
- Modify: `crates/wot065-data/src/lib.rs`
- Modify: `crates/wot065-data/Cargo.toml` (only if `rand = "0.8"` needed for weighted random passports)

- [ ] **Step 1: Write failing tests in `crates/wot065-data/src/store.rs` (using `target/test-tmp/`)**
  - `safe_account_key("Player@Test.com")`, `"../escape"` -> no path separators or traversal components, round-trips distinct usernames uniquely without collision.
  - `JsonAccountStore::save` + `load`: writes `<data-dir>/<key>.json.tmp.<pid>`, calls `sync_all()`, atomically replaces `<key>.json`, reloads exact `AccountProfile`.
  - `load_or_create`: creates new profile via `Preset` with unique incrementing `database_id` (scanning existing `.json` files on init so IDs never collide across restarts!), reloads existing profile ignoring `--preset` overwrite on subsequent opens.
  - `damage_vehicle` helper (`hp_fraction = 0.25`): sets `repair = Some((cost, new_hp))` using `max_hp = hull.max_health + turret.max_health` and `ceil((max_hp - new_hp) * repair_cost)`.

- [ ] **Step 2: Run tests to verify failure**
  Run: `cargo test -p wot065-data store`

- [ ] **Step 3: Implement `store.rs` and expose via `lib.rs`**
  Verify Windows-safe atomic replacement (`std::fs::rename`, handling target-exists on Windows cleanly via temp-file replace / `rename` fallback without touching `%TEMP%`).

- [ ] **Step 4: Run `cargo test -p wot065-data`**
  Expected: all unit tests pass cleanly.

---

### Task 8: `crates/bw201` Unique `session_key` & `.pending/` Concurrent Login Handoff

**Files:**
- Modify: `crates/bw201/src/loginapp.rs`

- [ ] **Step 1: Write failing unit tests in `crates/bw201/src/loginapp.rs`**
  - Two consecutive `handle_datagram` calls allocate distinct non-zero session keys (`config.session_key`, `config.session_key.wrapping_add(1)`), preserving `unwritable_session_out_returns_failure_reply` and single-login `session_key == 0x0BADF00D`.
  - When `session_out = Some(dir/session.json)`, both `dir/session.json` and `dir/session.json.pending/<key:08x>.json` exist with valid `LoginSession` JSON (first key & second key files both survive concurrently).

- [ ] **Step 2: Run tests to verify failure**
  Run: `cargo test -p bw201`

- [ ] **Step 3: Implement monotonic unique `session_key` + `.pending/` atomic write in `loginapp.rs`**
  - Track `next_session_offset: u32` inside `LoginApp`. Allocate `let key = self.allocate_session_key();` (skip `0` if wrapping hits `0`).
  - Helper `write_session_handoff(path, &session)`: calls `session.write_atomic(path)` AND writes `<path>.pending/<session_key:08x>.json` atomically (`write_atomic`), creating `<path>.pending` directory if missing, returning `Err` cleanly so `LoginApp` sends `LoginRejectedServerNotReady` on I/O failure (`AGENTS.md`).

- [ ] **Step 4: Run `cargo test -p bw201`**
  Expected: all `bw201` tests PASS.

---

### Task 9: `crates/wot-baseapp` Wire Methods, Full `doCmd*` Dispatch, `AccountRegistry` & Kick-on-Relogin

**Files:**
- Create: `crates/wot-baseapp/src/registry.rs`
- Modify: `crates/wot-baseapp/src/protocol.rs`
- Modify: `crates/wot-baseapp/src/lib.rs`
- Modify: `crates/wot-baseapp/src/payloads.rs`
- Modify: `crates/wot-baseapp/src/main.rs`
- Modify: `crates/wot-baseapp/tests/e2e_udp_handoff.rs`

- [ ] **Step 1: Write failing protocol unit tests (`protocol.rs` / `registry.rs`) and E2E UDP tests (`e2e_udp_handoff.rs` + new E2E tests)**
  - `protocol.rs`:
    - `decode_do_cmd` handles `DO_CMD_STR`, `DO_CMD_INT3`, `DO_CMD_INT4`, `DO_CMD_INT2_STR`, `DO_CMD_INT_ARR` without dropping string/array payloads.
    - Mutating command (`CMD_BUY_SLOT`, `CMD_EXCHANGE`, etc.) emits `UPDATE (0x95)` `[i32 eid][pickled diff]` **before** `ON_CMD_RESPONSE (0x82)` in the same reply bundle.
    - `CMD_BUY_TMAN` emits `UPDATE (0x95)` + `RESYNC_DOSSIERS (0x96)` + `ON_CMD_RESPONSE_EXT (0x83)` containing `'tmanInvID'` and `'tmanCompDescr'`.
    - `CMD_REQ_PLAYER_DOSSIER (503)` & `CMD_REQ_PLAYER_CLAN_INFO (504)` emit strictly `ON_CMD_RESPONSE (0x82)` (never `0x83`) + compressed stream payload (`raw bytes` for `DOSSIER_TYPE.ACCOUNT`, `(CACHE_VER, (2, ownerID, ts, bytes))` for `DOSSIER_TYPE.VEHICLE`).
    - `CMD_SYNC_DOSSIERS (600)` ALWAYS responds `RES_STREAM` (`1`) with populated dossier quads.
    - `CMD_SYNC_DATA (100)`: `rev=0` or stale `!= profile.rev` returns `RES_STREAM`, `rev == profile.rev` returns `ON_CMD_RESPONSE_EXT(rid, RES_SUCCESS, {'rev': cur, 'prevRev': cur})`.
    - Wrong `shopRev` emits `ON_CMD_RESPONSE_EXT(rid, RES_SHOP_DESYNC=-4, {'shopRev': server_shop_rev})`.
  - `e2e_udp_handoff.rs`:
    - Update `verify_full_login_and_hangar_flow` expectation from `130` starter vehicles to `3` (`Retail` preset).
    - Add E2E UDP test `full_hangar_persistence_and_commands_roundtrip`:
      1. Connect in `Sandbox` preset (or `Retail` + `CMD_SET_MONEY` dev command).
      2. Buy slot, unlock tank (`CMD_UNLOCK`), buy tank (`CMD_BUY_VEHICLE`), buy & mount gun/optdev/shells/eqs (`CMD_BUY_ITEM`, `CMD_EQUIP`, `CMD_EQUIP_OPTDEV`, `CMD_EQUIP_SHELLS`, `CMD_EQUIP_EQS`), pay gold to demount non-removable optdev, recruit tankman (`CMD_BUY_TMAN`), seat/respec/add skill/passport tankman, apply camouflage (`CMD_VEH_CAMOUFLAGE`), inspect own & other player's dossier (`CMD_REQ_PLAYER_DOSSIER`), sell tank (`CMD_SELL_VEHICLE`).
      3. Disconnect, open fresh UDP client session, log in again as same username, verify `CMD_SYNC_DATA` reflects exact persisted `credits`/`gold`/`vehicles`/`tankmen`/`unlocks`.
    - Add E2E UDP test `duplicate_login_kicks_old_owner_without_bounce`:
      1. Client A logs in as `"dup_user"` and stays connected.
      2. Client B logs in via `LoginApp` as `"dup_user"` (concurrent `.pending/` handoff) and reaches `ENABLE_ENTITIES`.
      3. Assert Client A receives `ON_KICKED_FROM_SERVER (0x84)` datagram.
      4. Assert trailing datagram sent by Client A on its old channel/key does NOT bounce-kick Client B, and Client B continues executing hangar commands normally.

- [ ] **Step 2: Run tests to verify failure**
  Run: `cargo test -p wot-baseapp`

- [ ] **Step 3: Implement `registry.rs`, update `protocol.rs`, `lib.rs`, `payloads.rs`, `main.rs`**
  - Add `--data-dir` (default `.scratch/accounts`), `--preset` (default `retail`), `--economics-config` (optional) to `BaseAppConfig` & `wot-baseapp/src/main.rs` CLI args.
  - Keep `PayloadOverrides` purely opt-in when explicitly supplied.
  - Wire `SessionRouter` -> `AccountRegistry`:
    - Poll/ingest `<session_file>.pending/*.json` (deleting consumed pending files) + fallback `--session-file` into `known_logins` keyed by `session_key`, ignoring `revoked_session_keys`.
    - On `ENABLE_ENTITIES` after `AUTHENTICATE`: if `active_owners` holds `old_owner` (`old_owner.addr != caller`), encrypt & flush `ON_KICKED_FROM_SERVER("Another session logged in", 0, 0)` on `old_owner.addr` immediately over UDP, remove `old_owner.addr` from `peers`, evict `old_owner.session_key` from `known_logins`, record it in bounded `revoked_session_keys`, and bind the new owner + `AccountProfile`.
    - Persist every mutation atomically to disk **before** sending `Account.update(diff)` + `onCmdResponse`/`Ext`.

- [ ] **Step 4: Run `cargo test -p wot-baseapp`**
  Expected: all unit and UDP E2E tests PASS.

---

### Task 10: `wot-server` CLI Flags (`serve`) & Offline Account Subcommands (`wot-server account`)

**Files:**
- Modify: `Cargo.toml` (add `wot065-data = { path = "crates/wot065-data" }` to root `wot-server` package)
- Create: `src/account_cli.rs`
- Modify: `src/main.rs`

- [ ] **Step 1: Write failing tests in `src/account_cli.rs` (using `target/test-tmp/`)**
  - Test `show`, `reset --preset sandbox`, `grant --credits 5000 --gold 100 --free-xp 200 --veh-xp ussr:MS-1=10000 --tman-xp 1=50000 --unlock-all`, and `damage --all --hp-fraction 0.5` round-tripping through `JsonAccountStore` and computing valid `repair = (cost, hp)` matching `<repairCost>`.

- [ ] **Step 2: Run tests to verify failure**
  Run: `cargo test --bin wot-server`

- [ ] **Step 3: Implement `src/account_cli.rs` and update `src/main.rs`**
  - Extend `wot-server serve` with `--preset <retail|sandbox>` (default `retail`), `--data-dir <PATH>` (default `.scratch/accounts`), `--economics-config <PATH>`, forwarding them to spawned `wot-baseapp`.
  - Add `wot-server account` subcommand tree (`show`, `reset`, `grant`, `damage`) printing clear human-readable summaries (and a note that `account` mutates persisted JSON between live server sessions).

- [ ] **Step 4: Full Workspace Verification**
  1. Check no stray running `.exe` locks (`Get-Process wot-server,bw201-login,wot-baseapp -ErrorAction SilentlyContinue`).
  2. Run: `cargo check --workspace`
  3. Run: `cargo test --workspace`
  4. Verify zero warnings/errors, all workspace unit & E2E tests passing cleanly, zero files written to `%TEMP%` or `raw/`.
  5. Document/execute final live WoT 0.6.5 client E2E verification instructions against `C:\Users\qwerty\Desktop\World_of_Tanks` (`wot-server serve --preset retail` and `--preset sandbox`, shop/fitting/crew/camouflage/research/repair via `account damage`, and duplicate login kick).

---

## Self-Review & Execution Handoff

Plan covers every requirement in [2026-11-13-wot065-full-hangar-design.md](file:///c:/Users/qwerty/Documents/dstest/docs/superpowers/specs/2026-11-13-wot065-full-hangar-design.md) without TBDs or open guesses.

After approval of this plan, execute sequentially or via parallel-safe task subagents using:
- **Recommended:** `subagent-driven-development` (dispatch fresh worker per task / independent group, review spec compliance + code quality at each seam)
- **Alternative:** `executing-plans` (execute tasks inline step-by-step in this session).
