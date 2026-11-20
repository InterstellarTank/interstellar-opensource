# World of Tanks 0.6.5 Full Hangar Server — Design Specification

- **Date:** 2026-11-13
- **Status:** Ready for user approval (builder check passed: nothing important left)
- **Target Client:** Retail World of Tanks 0.6.5 (`C:\Users\qwerty\Desktop\World_of_Tanks`)
- **Workspace Root:** `c:\Users\qwerty\Documents\dstest`

---

## 1. Goals, Scope & Settled Decisions

This specification defines the complete implementation of the World of Tanks 0.6.5 hangar server in Rust so the retail client can perform full garage workflows—buying and selling vehicles/modules/shells/consumables/equipment, researching modules and branches, fitting turrets/guns/ammo/equipment/camouflage, managing crew (recruit, seat/unload/swap, skills, retrain, passport, dismiss), repairing vehicles, converting XP/gold/premium, inspecting dossiers—with persistent per-account state across sessions.

### 1.1 Settled Design Decisions
1. **Architecture (Variant 1):** Extend `wot065-data` with the full static item-defs catalog, binary descriptor/dossier codecs, economics config, pure deterministic account state + command engine, incremental `Account.update(diff)` builder, and atomic per-account JSON store; keep `wot-baseapp` as the async Mercury UDP adapter + live `AccountRegistry`; wire `--preset`, `--data-dir`, `--economics-config`, and offline `wot-server account ...` CLI in `wot-server`.
2. **Account Identity & Auth:** Any login username works without password enforcement and auto-creates a new persistent profile on first login under `<data-dir>/<safe_normalized_name>.json`.
3. **Single-Owner Duplicate Login:** Only one active BaseApp peer may own an account at a time. When the same `normalized_name` completes login on a new socket address while an old session is active, BaseApp sends `Account.onKickedFromServer` to the old peer, detaches it, and binds the new peer to the latest persisted profile.
4. **Starting Presets (`--preset`):**
   - `retail` *(default)*: 3 tier-1 starter vehicles (`ussr:MS-1`, `germany:Ltraktor`, `usa:T1_Cunningham`), stock modules, default gun ammo (`getDefaultAmmoForGun`), 100% male crews (`0` skills, `freeXP = 0`), `slots = 6`, `berths = 16`, `credits = 10_000`, `gold = 0`, `freeXP = 0`, starter vehicles + their `autounlockedItems` in `unlocks`, tech trees otherwise locked.
   - `sandbox`: Same 3 starter vehicles (keeping tech tree unlocking, shop purchases, and crew training testable), `slots = 6`, `berths = 16`, `credits = 50_000_000`, `gold = 1_000_000`, `freeXP = 5_000_000`, `vehTypeXP = 500_000` on starter + default-elite tanks, `freeXP = 1_000_000` on each starter tankman.
5. **Testing Without Battles:** Server supports WG dev commands (`CMD_SET_MONEY = 10001`, `CMD_ADD_XP = 10002`, `CMD_ADD_TMAN_XP = 10003`, `CMD_UNLOCK_ALL = 10004`) over the wire **and** offline CLI commands (`wot-server account show|reset|grant|damage`) operating on persisted JSON files between server runs.
6. **Validation & Anti-Abuse Caps:** Enforce all vanilla WoT 0.6.5 gameplay rules (weight limits, turret/gun/shell/filter compatibility, unlock prerequisites, rank/XP/respec/passport formulas, currency/slot/berth bounds). Do **not** enforce daily anti-abuse caps (`freeTMenLeft`, `freeVehiclesLeft`, daily vehicle sell caps): initialize `freeTMenLeft` and `freeVehiclesLeft` to high non-blocking allowances (`1000`) and do not reject sales due to daily limits.
7. **Economics Config:** Hardcode vanilla 0.6.5 economics defaults in Rust with an optional `--economics-config <PATH>` JSON override.
8. **Out of Scope:** `CellApp`/`Avatar` battles, battle queue/matchmaker, prebattles (squads/companies/training rooms), clan wars / clan treasury mutations, and player-to-player `Trader` offers.

---

## 2. Crate & Module Architecture

### 2.0 `crates/bw201` (Unique Session Key & Concurrent Login Handoff)
- **`crates/bw201/src/loginapp.rs`:**
  - `LoginApp` allocates a unique non-zero `session_key: u32` per accepted login (`config.session_key.wrapping_add(seq)`, skipping `0`, so the first login keeps `config.session_key` and existing single-login tests remain compatible).
  - When `--session-out <PATH>` is configured, `LoginApp` writes BOTH `<PATH>` atomically (existing fallback contract) **and** a per-login pending handoff file `<PATH>.pending/<session_key:08x>.json` (atomically via temp file in `<PATH>.pending/`). Preserves fail-fast `LoginRejectedServerNotReady` if either persistence write fails (`AGENTS.md`). This guarantees two rapid logins before reaching BaseApp never overwrite each other.

### 2.1 `crates/wot065-data` (Domain Model, Codecs, Engine & Store)
All domain logic is synchronous, side-effect-free (except `store.rs` file I/O), and directly unit-testable without sockets:

- `item_defs.rs` & `catalog.rs` — Full static game data parsed from `<game_dir>/res/scripts/item_defs/`:
  - Nations: `0 = ussr`, `1 = germany`, `2 = usa`, `15 = NONE_INDEX`.
  - Vehicles (`vehicles/<nation>/list.xml` + `<Vehicle>.xml`): `innationID`, `vehTypeCompDescr`, `level`, `tags`, `price: (credits, gold)`, `showInShop`, `repairCost`, `hull { weight, maxHealth, turretPositions }`, `crew` (primary role + secondary roles per slot), `defaultPlayerEmblemID`, all `chassis`, `engines`, `fuelTanks`, `radios`, and per-turret `turrets` + `guns` (handling `<shared>` inheritance and per-vehicle/per-turret local overrides including `maxAmmo`, `unlocks`, `guns`, `yawLimits`), `unlocksDescrs` built in exact Python `VehicleType.__init__` traversal order, `autounlockedItems`, and `is_default_elite` (`unlocksDescrs.is_empty()`).
  - Components (`components/{chassis,engines,fuelTanks,radios,turrets,guns,shells}.xml`): shared component prices, `notInShop`, `weight`, `maxLoad` (`chassis`), `level`, `tags`, gun `shots` (`shell` `compDescr` + `defaultPortion`) and `maxAmmo`, `installableComponents` set per vehicle.
  - Artefacts (`vehicles/common/optional_devices.xml`, `equipments.xml`): `id`, `compDescr`, `price`, `showInShop`, `removable` (`optionalDevice`), `vehWeightFraction`, `weight`, `chassisMaxLoadFactor` (`EnhancedSuspension`), `tags`, `incompatibleTags.installed` (`equipment`), and `VehicleFilter` (`exclude` and `include` matchers across `nations`, `vehicle.tags`/`minLevel`/`maxLevel`, and `chassis`/`engine`/`fuelTank`/`radio` `tags`/`minLevel`/`maxLevel`).
  - Customization (`vehicles/<nation>/customization.xml`, `vehicles/common/player_emblems.xml`, `vehicles/common/horns.xml`): camouflage `id`, `gold`, `showInShop`, `allow` & `deny` sets of `vehTypeCompDescr`; emblem IDs; horn IDs & `vehicleTags`.
  - Tankmen (`tankmen/{tankmen,ussr,germany,usa}.xml`): per-nation `roleRanks` length (`max_rank_idx = ranks.len() - 1`), weighted `normalGroups` and `premiumGroups` (preserving `<sex>female</sex>` flag and per-group sets of `firstNameID`, `lastNameID`, `iconID`).
- `descr.rs` — Binary compact descriptor codecs & fit checks:
  - `IntCompDescr`: `make_item_comp_descr(item_type, nation_id, item_id)` & `parse_item_comp_descr(cd)`.
  - `VehicleDescr`: packs/unpacks `15..=25` byte wire descriptors:
    - `@0` `u8` `header = 1 + (nationID << 4)`, `@1` `u8` `vehicleTypeID`
    - `@2..10` `<4H`: `chassisID`, `engineID`, `fuelTankID`, `radioID`
    - `@10..14` `<2H` per turret slot (`T=1` in 0.6.5): `turretID`, `gunID`
    - `@14` `u8` `flags`:
      - bits `0..=2`: optional device slot bitmask where **bit 2 (`4`) = slot 0, bit 1 (`2`) = slot 1, bit 0 (`1`) = slot 2**
      - bit `4` (`16`): non-default `playerEmblemID` (`+1B`)
      - bit `6` (`64`): `hornID` present (`+1B`)
      - bit `7` (`128`): `camouflageID` present (`+2B` `<H`)
    - Tail byte order after `flags`: `[optDevIDs (<H) in descending slot order: slot 2, slot 1, slot 0][emblemID][hornID][camouflageID]`.
    - Weight & compatibility engine:
      - `weight = hull.weight + chassis.weight + engine.weight + fuel_tank.weight + radio.weight + sum(turret.weight + gun.weight)`
      - `total_weight = weight * (1.0 + sum(fraction)) + sum(addition)`
      - `max_weight = chassis.maxLoad + sum(maxWeightChange)` (where `EnhancedSuspension` gives `chassis.maxLoad * (factor - 1.0)`).
      - Implements `may_install_component`, `may_install_turret`, `may_install_optdev`, `may_remove_optdev`, `check_equipments`, and `check_camouflage`.
  - `TankmanDescr`: packs/unpacks `19 + numSkills + len(dossier)` byte descriptors:
    - `@0` `u8` `8 + (nationID << 4)`, `@1` `u8` `vehicleTypeID`, `@2` `u8` `roleID (1..=5)`, `@3` `u8` `roleLevel (50..=100)`, `@4` `u8` `numSkills`, `@5..5+K` `K * u8` `skillID`, `@5+K` `u8` `lastSkillLevel` (`0` if `K==0`), `@6+K..19+K` `<B3H2Bi` (`flags` bit 0 `isFemale`, bit 1 `isPremium`; `firstNameID`, `lastNameID`, `iconID`; `rankIdx`, `numLevelsToNextRank`; `freeXP`), followed by Tankman Dossier bytes.
    - Pure XP/rank/skill/respec/passport methods matching `common/items/tankmen.py`.
- `dossier.rs` — Canonical flat little-endian binary dossier codecs (`Account` v19 >= `154B`, `Vehicle` v17 >= `138B`, `Tankman` v10 = `132B`) with dynamic tables `vehTypeFrags` (`<H` count + `count*<I` keys + `count*<H` frags) and `vehDossiersCut` (`<H` count + `count*<3I`) and exact `_dynRecPos_*` offsets (`(150, 150 + 2 + 6*N)` for Account, `136` for Vehicle). Also builds `DossierCache` sync stream payloads `(version, [dossierType, ownerID, changeTime, bytes]...)`.
- `economics.rs` — `EconomicsConfig` (serde-serializable) with hardcoded vanilla 0.6.5 defaults and optional `--economics-config <PATH>` loader/validator.
- `account_state.rs` — `AccountProfile` serde model, `Preset` (`Retail`, `Sandbox`), preset factory `create_profile(username, db_id, preset, catalog, economics, now_utc)`, `build_full_sync(profile, catalog)` and `build_shop_cache(catalog, economics, shop_rev)`.
- `ops/` (`mod.rs`, `shop_economy.rs`, `fitting.rs`, `crew.rs`, `dossiers_dev.rs`) — Pure `AccountEngine::apply(&mut AccountProfile, &EngineCmd, &Catalog, &EconomicsConfig, shop_rev, now_utc) -> Result<CommandOutcome, CmdError>` returning `AccountDiff` (tracking exact client `prevRev`/`rev`, `_r` / `None` / dict-merge diffs), optional `ext` response payload, `resync_dossiers: bool`, and exact WoT `RES_*` code on error.
- `store.rs` — `JsonAccountStore` backed by `<data-dir>/<safe_normalized_name>.json` with atomic write-flush-replace inside `<data-dir>` (zero `%TEMP%` usage for Low-IL Windows compatibility).
- `account.rs` — Kept as thin backwards-compatible re-export/helper module so existing `crates/wot065-data/src/main.rs` CLI subcommands keep compiling and working (`shop` and `dossier` subcommands benefit from fixed economics/dossier defaults).

### 2.2 `crates/wot-baseapp` (Mercury UDP Adapter & Session Registry)
- `protocol.rs`:
  - Adds client methods `ON_KICKED_FROM_SERVER = 0x84`, `UPDATE = 0x95`, `RESYNC_DOSSIERS = 0x96` alongside `ON_CMD_RESPONSE = 0x82`, `ON_CMD_RESPONSE_EXT = 0x83`, `SHOW_GUI = 0x90`, `RECEIVE_SERVER_STATS = 0x92`.
  - Decodes all five `doCmd*` payloads (`0xC3..=0xC7`) including `String` and `Vec<i32>` args (without any `entity_id` header, matching `ServerConnection::startProxyMessage`), converts them into typed `DoCmdRequest`, and dispatches against `AccountRegistry` + `AccountProfile`.
  - Enforces response wire contracts:
    - Mutating commands commit to `AccountProfile`, write atomically via `JsonAccountStore`, enqueue `Account.update(diff)` **before** `onCmdResponse`/`onCmdResponseExt` in the same reliable reply bundle so `inventory`/`stats` are updated before client callbacks fire.
    - If `resync_dossiers == true`, enqueue `Account.resyncDossiers()` (`0x96`, `[i32 entity_id]`) in the reply bundle.
    - `CMD_BUY_TMAN` (`303`) always sends `onCmdResponseExt` with pickled `{'tmanInvID': i64, 'tmanCompDescr': bytes}`.
    - `CMD_REQ_PLAYER_DOSSIER` (`503`) and `CMD_REQ_PLAYER_CLAN_INFO` (`504`) ALWAYS send 2-arg `onCmdResponse` (never `onCmdResponseExt`, avoiding `TypeError` on the client's 2-arg lambda!).
    - Any shop-versioned command failing `client_shop_rev != server_shop_rev` replies with `onCmdResponseExt(rid, RES_SHOP_DESYNC=-4, pickle({'shopRev': server_shop_rev}))`.
    - `CMD_SYNC_DOSSIERS` (`600`) ALWAYS replies with `RES_STREAM` (`1`) + `RESOURCE_HEADER`/`RESOURCE_FRAGMENT` compressed `(version, flatList)`.
    - `CMD_SYNC_DATA` (`100`) implements the two-step handshake: `client_rev == 0` (or `client_rev != profile.rev`) returns `RES_STREAM` (`1`) with full sync (`rev = profile.rev`, `prevRev` omitted); when `client_rev == profile.rev`, returns `RES_CACHE` (`2`) via `onCmdResponseExt` with pickled `{'prevRev': profile.rev, 'rev': profile.rev}` (or `onCmdResponse(rid, RES_CACHE)`).
  - Manual file overrides (`PayloadOverrides`: `--account-diff`, `--showgui-ctx`, `--shop-cache`, `--dossier-sync`) remain purely opt-in when explicitly passed via CLI.
- `registry.rs` & `lib.rs`:
  - `BaseAppConfig` adds `data_dir: PathBuf`, `preset: Preset`, `economics_config: Option<PathBuf>`.
  - `AccountRegistry` loads `Catalog`, `EconomicsConfig`, and `JsonAccountStore`, assigns monotonically increasing `databaseID` per new account (persisted inside `AccountProfile`), ingests pending handoff files from `<session_file>.pending/*.json` (deleting each consumed pending file) alongside `--session-file` fallback into `known_logins: HashMap<u32, LoginSession>`, and maintains `active_owners: HashMap<String, ActiveAccountOwner>` (`normalized_name -> (SocketAddr, u32 session_key, entity_id, database_id)`) plus `live_profiles: HashMap<String, AccountProfile>` and a bounded `revoked_session_keys` set (`MAX = 1024`).
  - **Duplicate-login kick & eviction lifecycle:** A new peer provisionally registers on `BASE_APP_LOGIN` matching its `session_key` (ignoring revoked keys). Once the peer authenticates (`AUTHENTICATE`) and sends **`ENABLE_ENTITIES`** over the Blowfish-encrypted channel for `normalized_name`:
    1. If `active_owners` already maps `normalized_name -> old_owner` (`old_owner.addr != new_addr`):
       - Build and transmit `Account.onKickedFromServer("Another session logged in", 0, 0)` on `old_owner.addr`'s `ChannelState` encrypted with `old_owner`'s Blowfish key immediately over the UDP socket;
       - Remove `old_owner.addr` from `peers`, evict `old_owner.session_key` from `known_logins`, and insert `old_owner.session_key` into `revoked_session_keys` so trailing/keepalive datagrams or retries from the kicked client are ignored and never re-spawn a session or bounce-kick the new owner.
    2. Consume the new peer's `session_key` from `known_logins`, bind `active_owners[normalized_name] = new_owner`, and attach the latest live/persisted `AccountProfile` before sending `CREATE_BASE_PLAYER` and `SHOW_GUI`.
  - Peer disconnect (`DISCONNECT_CLIENT = 12` or channel timeout): removes `peers` entry and clears `active_owners[name]` only if `owner.addr == disconnected_addr`.

### 2.3 `wot-server` (`src/main.rs` + `src/account_cli.rs`)
- `Cargo.toml` adds `wot065-data = { path = "crates/wot065-data" }`.
- `wot-server serve`: adds `--preset <retail|sandbox>` (default `retail`), `--data-dir <PATH>` (default `.scratch/accounts`), `--economics-config <PATH>`, passing all three to the spawned `wot-baseapp` child process while keeping fail-fast `--session-out` on `bw201-login`.
- `wot-server account` offline CLI subcommands (for inspecting or modifying `<data-dir>/<name>.json` between server sessions):
  - `wot-server account show <USERNAME> [--data-dir PATH]`
  - `wot-server account reset <USERNAME> [--preset retail|sandbox] [--data-dir PATH] [--game-dir PATH] [--economics-config PATH]`
  - `wot-server account grant <USERNAME> [--credits N] [--gold N] [--free-xp N] [--veh-xp <NATION:NAME|ID>=<XP>]... [--tman-xp <TMAN_INV_ID>=<XP>]... [--unlock-all] [--data-dir PATH] [--game-dir PATH] [--economics-config PATH]`
  - `wot-server account damage <USERNAME> (--veh-inv-id <ID> | --all) [--hp-fraction <0.0..0.99>] [--data-dir PATH] [--game-dir PATH]`
  - Note: CLI prints a clear warning/message if an active server might overwrite offline edits, and executes atomically against `JsonAccountStore`.

---

## 3. Economics Defaults & Presets

### 3.1 Vanilla 0.6.5 `EconomicsConfig` Defaults
All defaults mirror `.scratch/research/decompiled/common/Economics.py` & retail client UI expectations:
- `sell_price_modif: f64 = 0.5` (`ceil(0.5 * (credits + gold * exchange_rate))` credits)
- `exchange_rate: i32 = 400` (credits per 1 gold)
- `free_xp_conversion: (i32, i32) = (25, 1)` (`discrecity = 25` XP per `gold_cost = 1` gold; matches `CommonPage.py:1030` `price[1] * xp / price[0]`, fixing the existing `(25, 100)` bug in `account.rs`)
- `slots_prices: (i32, Vec<i32>) = (6, vec![300])` (`initial_slots = 6`, price index `max(0, slots - 6)` clamped to last element -> `300` gold per extra slot)
- `berths_prices: (i32, i32, Vec<i32>) = (16, 16, vec![300])` (`initial_berths = 16`, `berths_in_pack = 16`, `300` gold per 16 berths; fixes the existing `(2, 1, (300, 900, 1500))` bug in `account.rs`)
- `premium_cost: Vec<(i32, i32)>` (days -> gold): `[(1, 250), (3, 650), (7, 1250), (30, 2500), (180, 13500), (360, 24000)]`
- `tankman_cost`: tuple of 3 tiers sorted ascending by `roleLevel`:
  1. `roleLevel: 50, credits: 0, gold: 0, baseRoleLoss: 0.2, classChangeRoleLoss: 0.4, isPremium: false`
  2. `roleLevel: 75, credits: 20_000, gold: 0, baseRoleLoss: 0.1, classChangeRoleLoss: 0.2, isPremium: false`
  3. `roleLevel: 100, credits: 0, gold: 200, baseRoleLoss: 0.0, classChangeRoleLoss: 0.0, isPremium: true`
- `passport_change_cost: i32 = 50` (gold)
- `paid_removal_cost: i32 = 10` (gold)
- `trade_fees`: `default: (0.05, 0.05)`, `sameClan: (0.0, 0.0)`
- `retail_starting`: `credits = 10_000, gold = 0, free_xp = 0`
- `sandbox_starting`: `credits = 50_000_000, gold = 1_000_000, free_xp = 5_000_000, starter_veh_xp = 500_000, starter_tman_free_xp = 1_000_000`
- Overridable via JSON file passed to `--economics-config <PATH>` (partial fields allowed via serde defaults; invalid divisors like `free_xp_conversion.0 <= 0` or unsorted `tankman_cost` fail fast at startup).

### 3.2 Shop Cache (`CMD_SYNC_SHOP`) Schema
Emits exact `shop` dict structure expected by `client/account_helpers/Shop.py` and `utils/requesters.py`:
- `'rev': shop_rev` (`u32`, bumps only if economics config / catalog changes; deterministic hash or monotonic server rev)
- `'items'`: nested dict `{nationIdx: {itemTypeIdx: (pricesDict, hiddenSet)}}` where:
  - Nations `0..=2` contain itemTypeIdx `1..=7, 10`:
    - `1` (`vehicle`): keyed by **`innationIdx`** (`u8` vehicle ID), value `(credits, gold)`, hidden entries excluded from shop purchase if `<notInShop>` is true.
    - `2..=7, 10` (components & shells): keyed by **integer `compDescr`**, value `(credits, gold)`.
  - Nation `15` (`NATION_NONE`) contains `9` (`optionalDevice`) and `11` (`equipment`), keyed by **integer `compDescr`**, value `(credits, gold)`.
  - Unused `(nationIdx, itemTypeIdx)` pairs store `({}, set())` (never `None`!).
- `'sellPriceModif'`, `'exchangeRate'`, `'slotsPrices'`, `'berthsPrices'`, `'freeXPConversion'`, `'premiumCost'` (`dict`), `'tankmanCost'` (`tuple` of 3 dicts), `'passportChangeCost'`, `'paidRemovalCost'`, `'tradeFees'`, `'camouflageCost'` (`{nationIdx: {camoId: gold}}`), `'hornCost'` (`[]`).

---

## 4. Persistence Schema & Initial Account Structure

### 4.1 Normalized Identity & Atomic JSON Store
- `safe_account_filename(normalized_name)`: validates/sanitizes `normalized_name` (lowercased UTF-8 trimmed; lowercase ASCII `[a-z0-9_.-]` kept verbatim, any other byte hex-encoded as `_xx` and forbidden `..` segments escaped) so arbitrary login usernames map uniquely to safe filenames `<data-dir>/<key>.json` without path traversal.
- Every persistence write serializes JSON -> writes `<key>.json.tmp.<pid>` inside `<data-dir>` -> `sync_all()` -> atomic rename/replace over `<key>.json` (`Write` + `ReplaceFile`/overwrite compatible with Windows Low-IL `0x1000`, never touching `%TEMP%`).

### 4.2 `AccountProfile` Schema & `full_sync` Mapping
`AccountProfile` stores:
- `schema_version: u32 = 1`
- `database_id: i64`
- `username: String`, `normalized_name: String`
- `rev: i64` (starts at `1`, increments on every committed server mutation)
- `creation_time: u32`, `last_battle_time: u32`
- `credits: i64`, `gold: i64`, `free_xp: i64`
- `slots: i32`, `berths: i32`
- `attrs: i64` (`0` standard, `1i64 << 32` (`4294967296`) when premium active), `premium_expiry_time: u32` (`> now_utc` when `PREMIUM` bit set, else `0` — required by `CommonPage.py:672`)
- `unlocks: BTreeSet<i32>` (contains unlocked item/veh `compDescr`s **plus** all `autounlockedItems` of unlocked/owned vehicles, matching `AccountDataDefs.txt:138-143`)
- `veh_type_xp: BTreeMap<i32, i64>` (`vehTypeCompDescr -> xp`)
- `elite_vehicles: BTreeSet<i32>` (tracks vehicles whose `unlocksDescrs` targets are all in `unlocks`, plus zero-unlock default-elite vehicles)
- `vehicles: BTreeMap<i32, VehicleEntry>` (key `vehInvID` >= 1; each entry stores `comp_descr: Vec<u8>`, `shells: Vec<(i32, i32)>`, `shells_layout: BTreeMap<(i32, i32), Vec<(i32, i32)>>`, `crew: Vec<Option<i32>>`, `repair: Option<(i32, i32)>`, `eqs: [i32; 3]`, `eqs_layout: [i32; 3]`, `settings: i32`)
- `tankmen: BTreeMap<i32, TankmanEntry>` (key `tmanInvID` >= 1; stores `comp_descr: Vec<u8>`, `vehicle_inv_id: Option<i32>`)
- `depot: BTreeMap<u8, BTreeMap<i32, i32>>` (itemType `2..=7, 9, 10, 11` -> `compDescr -> count > 0`)
- `next_inv_id: i32` (monotonic allocator for unique `vehInvID` / `tmanInvID`)
- `language: Option<String>`
- `account_dossier: Vec<u8>` (Account Dossier v19 binary)
- `vehicle_dossiers: BTreeMap<i32, (u32, Vec<u8>)>` (`vehTypeCompDescr -> (changeTime, VehicleDossier v17)`)
- Note: Tankman dossiers are embedded at tail of each tankman's `comp_descr` (`TankmanDossier v10`, 132B) and returned in `CMD_SYNC_DOSSIERS` as `(DOSSIER_TYPE.TANKMAN=3, tmanInvID, changeTime, dossier_bytes)`.

When serialized by `build_full_sync`, `inventory` always contains all keys `1..=11`, `inventory[1]` always contains all 9 subdicts (`compDescr`, `shellsLayout`, `shells`, `crew`, `repair`, `eqsLayout`, `eqs`, `settings`, `lock`), `inventory[8]` contains `compDescr` + `vehicle`, and `stats` initializes `freeTMenLeft: 1000`, `freeVehiclesLeft: 1000`, `clanInfo: None`, `doubleXPVehs: set()`, `captchaTriesLeft: 0`, `hasFinPassword: false`, `tkillIsSuspected: false`, `dossier: account_dossier_bytes`.

---

## 5. Wire Protocol & Exact Command Contracts

### 5.1 EntityDef Framing & Ordering Rules
- `Account` client methods (`0x80 + idx`, `Variable(2)`): `ON_CMD_RESPONSE = 0x82`, `ON_CMD_RESPONSE_EXT = 0x83`, `ON_KICKED_FROM_SERVER = 0x84` (`[i32 eid][STRING reason][u8 isBan][u32 expiryTime]`), `SHOW_GUI = 0x90`, `RECEIVE_SERVER_STATS = 0x92`, `UPDATE = 0x95` (`[i32 eid][STRING pickled_diff]`), `RESYNC_DOSSIERS = 0x96` (`[i32 eid]`).
- `Account` base methods (`0xC0 + idx`, `Variable(2)`, **no** `entity_id`): `DO_CMD_STR = 0xC3`, `DO_CMD_INT3 = 0xC4`, `DO_CMD_INT4 = 0xC5`, `DO_CMD_INT2_STR = 0xC6`, `DO_CMD_INT_ARR = 0xC7` (`ARRAY<INT32>` uses 4-byte LE signed `i32` count).
- Incremental `Account.update(diff)` always carries `'prevRev': old_rev, 'rev': new_rev` so `isFullSync = False` (`Account.py:485`), `events.onClientUpdated` fires, dict deletions serialize as `None` (never `_d`), and growing sets (`unlocks`, `eliteVehicles`) emit new additions directly or full replacements via `<key>_r`.
- Mutating commands send `Account.update(diff)` (plus `resyncDossiers()` if needed) **before** `onCmdResponse`/`onCmdResponseExt` in the same bundle.

### 5.2 Command Specification Table

| Cmd ID | Name | Transport & Exact Args | Validation Rules & State Mutation | Response & Diff Keys |
|---|---|---|---|---|
| `100` | `CMD_SYNC_DATA` | `Int3 [rev_hi, rev_lo, 0]` | Combine `u64` rev. If `0` or `!= profile.rev`: full sync stream; if `== profile.rev`: cache hit. | `rev!=cur`: `onCmdResponse(RES_STREAM)` + compressed full dict (`rev=profile.rev`, no `prevRev`). `rev==cur`: `onCmdResponseExt(RES_CACHE, {'prevRev': cur, 'rev': cur})`. |
| `300` | `CMD_SYNC_SHOP` | `Int3 [client_rev, 0, 0]` | Shop cache built from `catalog + economics`. | Always `RES_STREAM` if `client_rev != shop_rev` (or `RES_CACHE` if matched and non-zero, though `RES_STREAM` when different works cleanly). |
| `600` | `CMD_SYNC_DOSSIERS` | `Int3 [version, maxChangeTime, 0]` | Collect `(1, db_id)` (optional/account), `(2, vehTypeCD)` for owned/history vehicles, `(3, tmanInvID)` for all crew. Filter by `changeTime > maxChangeTime` if `version == CACHE_VER`, else send all. | **ALWAYS** `onCmdResponse(RES_STREAM)` + compressed `(CACHE_VER, flat_quads)` (never `RES_CACHE`, avoiding `DossierCache.__isSynchronizing` hang). |
| `1000` | `CMD_SET_LANGUAGE` | `Str [lang]` | Store `profile.language = lang`; ignore silently. | `onCmdResponse(RES_SUCCESS)` (optional since client passes `callback=None`). |
| `501` | `CMD_REQ_SERVER_STATS` | `Int3 [0,0,0]` (`rid=102`) | Count connected owners (`playersCount`). | `receiveServerStats` (`[i32 eid][u32 players][u32 arenas=0]`). |
| `205` | `CMD_BUY_SLOT` | `Int3 [shopRev, 0, 0]` | Verify `shopRev`; charge `slots_prices` next price; `slots += 1`. | `update({'stats': {'slots', 'gold'}})` + `onCmdResponse`. |
| `206` | `CMD_BUY_BERTHS` | `Int3 [shopRev, 0, 0]` | Verify `shopRev`; charge `berths_prices` next price; `berths += berths_in_pack`. | `update({'stats': {'berths', 'gold'}})` + `onCmdResponse`. |
| `202` | `CMD_EXCHANGE` | `Int3 [shopRev, gold, 0]` | Verify `shopRev`, `gold > 0`, `profile.gold >= gold`, overflow-safe `credits += gold * exchange_rate`. | `update({'stats': {'gold', 'credits'}})` + `onCmdResponse`. |
| `203` | `CMD_FREE_XP_CONV` | `IntArr [shopRev, xp, vehCDs...]` | Verify `shopRev`, `xp > 0`, `xp % discrecity == 0`, deduplicated `vehCDs` are elite vehicles (`is_default_elite` or in `elite_vehicles`), sum of `vehTypeXP >= xp`, `profile.gold >= (xp / discrecity) * gold_cost`. Drain `vehTypeXP` in listed order, add `free_xp`. | `update({'stats': {'gold', 'freeXP', 'vehTypeXP'}})` + `onCmdResponse`. |
| `204` | `CMD_PREMIUM` | `Int3 [shopRev, days, 0]` | Verify `shopRev`, `days` in `premium_cost`, `gold >= cost`. Set `attrs |= (1 << 32)`, extend `premium_expiry_time = max(now_utc, premium_expiry_time) + days * 86400`. | `update({'stats': {'gold'}, 'account': {'attrs', 'premiumExpiryTime'}})` + `onCmdResponse`. |
| `201` | `CMD_UNLOCK` | `Int3 [vehTypeCD, unlockIdx, 0]` | Lookup `vehType.unlocksDescrs[unlockIdx]` `(cost, targetCD, prereqs...)`; validate vehicle unlocked, all `prereqs` in `unlocks`, `targetCD` not already in `unlocks`, `vehTypeXP[vehTypeCD] + freeXP >= cost`. Drain `vehTypeXP` first then `freeXP`. Add `targetCD` (and if target is a vehicle, add its `autounlockedItems` + empty `VehicleDossier` if absent + set `resync_dossiers = true`) to `unlocks`. Recompute `eliteVehicles` for `vehTypeCD` (and any vehicle whose prereqs are now all unlocked). | `update({'stats': {'unlocks', 'vehTypeXP', 'freeXP', 'eliteVehicles'}})` + optional `resyncDossiers` + `onCmdResponse`. |
| `301` | `CMD_BUY_VEHICLE` | `Int4 [shopRev, vehTypeCD, flags, tmanCostIdx]` | Verify `shopRev`, `vehTypeCD` unlocked in `unlocks` & in shop (`!not_in_shop`), free garage slot (`vehicles.len() < slots`), valid `tmanCostIdx`. Charge vehicle price + (`CREW` flag ? `crew_len * tman_cost[tmanCostIdx]` : `0`) + (`SHELLS` flag ? default gun ammo cost : `0`). Allocate `vehInvID`, stock `VehicleDescr`, default `shells`/`shellsLayout` (if `SHELLS` else empty shots), `eqs=[0,0,0]`, create tankmen if `CREW` flag (else `[None; crew_len]`), ensure `VehicleDossier` exists (`resync_dossiers = true`). Do **not** reject on `freeVehiclesLeft` / `freeTMenLeft`. | `update({'stats': {...}, 'inventory': {1: ..., 8: ...}})` + `resyncDossiers` + `onCmdResponse`. |
| `304` | `CMD_SELL_VEHICLE` | `Int3 [shopRev, vehInvID, flags]` | Verify `shopRev`, `vehInvID` exists, and **`repair.is_none()`** (`gui_items.py:798-801` disables selling damaged vehicles). Compute base vehicle sale value matching `Shop.__getVehiclePriceFromCache` (`§5.3`: hull + net mounted component swaps + **all** mounted `optionalDevices`, including removable ones — players who wish to keep removable devices demount them before selling). All mounted `optionalDevices` are sold with the tank and removed. For `shells`: if `flags & 16 (SHELLS)` sell and consume, else return to `depot[10]`. For `eqs`: if `flags & 32 (EQS)` sell and consume, else return to `depot[11]`. For `crew`: if `flags & 1 (CREW)` dismiss all seated crew (`inventory[8]` deletes), else require sufficient free barracks berths and unload seated crew (`inventory[8]['vehicle']` deletes). Erase `vehInvID` across all 9 `inventory[1]` subdicts via `None`. | `update({'stats': {'credits'}, 'inventory': {1: None-deletes, 8: ..., 10: ..., 11: ...}})` + `onCmdResponse`. |
| `302` | `CMD_BUY_ITEM` | `Int3 [shopRev, itemCD, count]` | Verify `shopRev`, `count > 0`, simple item type `2..=7, 9..=11`, exists in shop (`!not_in_shop`); if module `2..=7`, must be in `unlocks`. Deduct `(credits * count, gold * count)` and increment `depot[type][itemCD]`. | `update({'stats': {'credits'/'gold'}, 'inventory': {type: {itemCD: new}}})` + `onCmdResponse`. |
| `305` | `CMD_SELL_ITEM` | `Int4 [shopRev, itemTypeIdx, itemCD, count]` | Verify `shopRev`, `count > 0`, `itemTypeIdx in 2..=7, 9..=11`, `depot[type][itemCD] >= count`. Credit `count * sell_price(item)`, decrement depot (`None` when `0`). | `update({'stats': {'credits'}, 'inventory': {type: {itemCD: new|None}}})` + `onCmdResponse`. |
| `101` | `CMD_EQUIP` | `Int3 [vehInvID, itemCD, gunCD]` | Verify `repair.is_none()`. Require `itemCD` owned in `depot` (`depot[itemType][itemCD] >= 1`, returning replaced component +1 to `depot`). For non-turret: `gunCD == 0`, run `may_install_component` (nation, installable, weight, optDev filter). For turret (`itemType == 3`): run `may_install_turret(itemCD, gunCD)`. If `gunCD == 0` or `gunCD == current_gun_cd`, keep current gun untouched in depot; if `gunCD != current_gun_cd`, also require `depot[4][gunCD] >= 1` and return `current_gun_cd` +1 to `depot[4]`. **If active gun changed:** unload old `vehicle.shells` back to `depot[10]` (preserving old `(turretCD, gunCD)` entry in `shellsLayout`), then load `shellsLayout[(newTurretCD, newGunCD)]` from `depot[10]` up to available depot counts (`[]` if none; client follows with `CMD_EQUIP_SHELLS` via `loadShells(None)`). | `update({'inventory': {1: {'compDescr', 'shells', 'shellsLayout'}, ...depot...}})` + `onCmdResponse`. |
| `102` | `CMD_EQUIP_OPTDEV` | `Int4 [vehInvID, devCD, slotIdx, isPaidRemoval]` | Verify alive, `slotIdx in 0..3`. If `devCD > 0`: verify no duplicate in other slots, `depot[9][devCD] >= 1`, `may_install_optdev`; if slot occupied cleanly handle replacement (demount removable / destroy non-removable, checking weight), consume 1 from `depot[9]`. If `devCD == 0`: verify slot occupied, `may_remove_optdev` (weight + remaining optDev compatibility): if `removable` -> free return to `depot[9]`; else if `isPaidRemoval != 0` -> check `gold >= paidRemovalCost`, deduct gold, return to `depot[9]`; else -> free destroy (do not return to depot). | `update({'stats': {'gold'}?, 'inventory': {1: {'compDescr'}, 9: ...}})` + `onCmdResponse`. |
| `103` | `CMD_EQUIP_SHELLS` | `IntArr [vehInvID, cd0, cnt0, ...]` | Verify alive; parse pairs `(shellCD, count)` with unique `shellCD`, all `count >= 0`, all `shellCD` compatible with current gun's `shots`, `sum(cnt) <= gun.maxAmmo`. Compute per-shell delta vs currently loaded `vehicle.shells`: refund freed shells to `depot[10]`, require `depot[10] >= deficit` for newly loaded shells, deduct from `depot[10]`, store in `vehicle.shells` & `vehicle.shellsLayout[(turretCD, gunCD)]`. | `update({'inventory': {1: {'shells', 'shellsLayout'}, 10: ...}})` + `onCmdResponse`. |
| `104` | `CMD_EQUIP_EQS` | `IntArr [vehInvID, eq0, eq1, eq2]` | Verify alive; check exact length 3, no duplicate non-zero `eqCD`, each non-zero `eqCD` exists, passes `VehicleFilter` & `EquipmentFilter` (`incompatibleTags` pairwise check across slots). Refund old non-zero slots to `depot[11]`, verify `depot[11] >= 1` per net-added `eqCD`, deduct from `depot[11]`, update `eqs` and `eqsLayout`. | `update({'inventory': {1: {'eqs', 'eqsLayout'}, 11: ...}})` + `onCmdResponse`. |
| `106` | `CMD_REPAIR` | `Int3 [vehInvID, 0, 0]` | Verify `vehicle.repair == Some((cost, _health))`, `credits >= cost`. Deduct `credits`, set `repair = None`. | `update({'stats': {'credits'}, 'inventory': {1: {'repair': {vehInvID: None}}}})` + `onCmdResponse`. |
| `107` | `CMD_VEH_SETTINGS` | `Int3 [vehInvID, settingFlag, isOn]` | Verify `settingFlag` is valid single bit in `{1, 2, 4, 8}`, `vehInvID` exists; if setting `XP_TO_TMAN (1)`, verify vehicle is elite (`unlocksDescrs.is_empty()` or in `elite_vehicles`). Update `settings` bitmask. | `update({'inventory': {1: {'settings'}}})` + `onCmdResponse`. |
| `120` | `CMD_VEH_CAMOUFLAGE` | `Int3 [shopRev, vehInvID, camoID]` | Verify `shopRev`, `vehInvID` exists & alive; if `camoID == 0`: clear camouflage (free); else validate `camoID` in nation `customization.xml`, `showInShop`, vehicle allowed (not in `deny`, and in `allow` if non-empty), `gold >= camo.gold`, deduct `gold`, repack `VehicleDescr` with flag `128` + `<H camoID`. *(`121 CMD_VEH_HORN` returns `RES_FAILURE = -1` because client `HornInterface` is disabled in 0.6.5.)* | `update({'stats': {'gold'}?, 'inventory': {1: {'compDescr'}}})` + `onCmdResponse`. |
| `303` | `CMD_BUY_TMAN` | `Int4 [shopRev, vehTypeCD, roleIdx, tmanCostIdx]` | Verify `shopRev`, `vehTypeCD` exists, `roleIdx in 1..=5` exists in `vehType.crew_roles`, valid `tmanCostIdx` (`0..=2`), barracks capacity `(unseated_tankmen < berths)`. Deduct `credits`/`gold`, generate male passport from nation `normalGroups`, build `TankmanDescr` (`roleLevel`, empty skills, initial v10 dossier), insert in `tankmen`, set `resync_dossiers = true`. | `update({'stats': ..., 'inventory': {8: {'compDescr'}}})` + `resyncDossiers` + **`onCmdResponseExt` `{'tmanInvID', 'tmanCompDescr'}`**. |
| `105` | `CMD_EQUIP_TMAN` | `Int3 [vehInvID, slot, tmanInvID]` | Verify `vehInvID` alive, `slot < vehType.crew_roles.len()`. If `tmanInvID == -1` (unload): verify slot has current occupant `oldID` and `unseated_tankmen < berths`, set `crew[slot] = None` and remove `oldID` from `inventory[8]['vehicle']`. If `tmanInvID >= 0`: verify tankman exists, same `nationID`, `tankman.roleID == vehType.crew_roles[slot]`, and source vehicle (if seated elsewhere) is alive. Compute net unseated barracks delta `delta = (oldID.is_some() as i32) - (tman.vehicle_inv_id.is_none() as i32)` and verify `unseated_tankmen + delta <= berths`. Clear source vehicle's slot (`None`) if moved from another vehicle, unseat `oldID` if present, and bind `tmanInvID` to `(vehInvID, slot)`. | `update({'inventory': {1: {'crew'}, 8: {'vehicle'}}})` + `onCmdResponse`. |
| `306` | `CMD_DISMISS_TMAN` | `Int3 [tmanInvID, 0, 0]` | Verify tankman exists (if seated in a vehicle, replace its `crew[slot]` with `None` as long as vehicle is alive). Remove from `tankmen`, emit `None` in `inventory[8]['compDescr']` & `'vehicle'`. | `update({'inventory': {1: {'crew'}?, 8: {'compDescr': {id: None}, 'vehicle': {id: None}}}})` + `onCmdResponse`. |
| `153` | `CMD_TMAN_RESPEC` | `Int4 [shopRev, tmanInvID, tmanCostIdx, vehTypeCD]` | Verify `shopRev`, tankman exists (if seated, vehicle alive), valid `tmanCostIdx`, valid `vehTypeCD` (same `nationID`, compatible `roleID` present in target vehicle's `crew_roles`). Deduct cost; apply vanilla `respecialize` (`baseRoleLoss`, `classChangeRoleLoss`, `minRoleLevel`, clamp `>= 50`, recompute `rankIdx`/`numLevelsToNextRank`, apply `freeXP` via `add_xp(0)`). | `update({'stats': {...}, 'inventory': {8: {'compDescr'}}})` + `onCmdResponse`. |
| `151` | `CMD_TMAN_ADD_SKILL` | `Int3 [tmanInvID, skillIdx, 0]` | Verify `skillIdx in 6..=8` (`repair`, `fireFighting`, `camouflage`), not already learned, `roleLevel == 100`, (`skills.is_empty()` or `lastSkillLevel == 100`). Append skill at level `0`, consume `freeXP` via `add_xp(0)`. | `update({'inventory': {8: {'compDescr'}}})` + `onCmdResponse`. |
| `152` | `CMD_TMAN_DROP_SKILL` | `Int3 [tmanInvID, skillIdx, 0]` | Verify `skillIdx` learned on tankman. Remove skill (`lastSkillLevel` becomes `0` if now empty, or `100` if dropping the last skill; preserve remaining `freeXP` unchanged). | `update({'inventory': {8: {'compDescr'}}})` + `onCmdResponse`. |
| `154` | `CMD_TMAN_PASSPORT` | `IntArr [shopRev, tmanInvID, isFemaleFlag, firstNameID, lastNameID, iconID]` | Verify `shopRev`, tankman exists, `gold >= passport_change_cost`. Resolve `-1` fields from current tankman values (or keep `isFemale` if `-1`; note female IDs reside in `premiumGroups` female groups). Validate `(firstNameID, lastNameID, iconID)` belong to a single group of matching sex (`normalGroups` or `premiumGroups`). Deduct `passport_change_cost` gold, update descriptor flags + IDs. | `update({'stats': {'gold'}, 'inventory': {8: {'compDescr'}}})` + `onCmdResponse`. |
| `503` | `CMD_REQ_PLAYER_DOSSIER` | `Int2Str [dossierType, ownerID, name]` | Lookup `name` in live/persisted `JsonAccountStore`. If not found: return `RES_FAILURE = -1`. If found: for `DOSSIER_TYPE.ACCOUNT (1)` stream raw pickled `account_dossier` bytes (`str`); for `DOSSIER_TYPE.VEHICLE (2)` stream pickled `(CACHE_VER, (2, ownerID, changeTime, veh_dossier_bytes))` (empty v17 if absent). | **STRICTLY 2-arg `onCmdResponse(rid, RES_STREAM=1)`** + compressed pickle stream. |
| `504` | `CMD_REQ_PLAYER_CLAN_INFO` | `Int2Str [0, 0, name]` | Return empty clan tuple `(0, None)` for known accounts or `RES_FAILURE = -1` if unknown. | **STRICTLY 2-arg `onCmdResponse`** + stream `(0, None)`. |
| `10001` | `CMD_SET_MONEY` | `Int3 [credits, gold, 0]` | Verify `credits >= 0, gold >= 0`; overwrite `profile.credits`, `profile.gold`. | `update({'stats': {'credits', 'gold'}})` + `onCmdResponse`. |
| `10002` | `CMD_ADD_XP` | `Int3 [vehTypeCD, xp, 0]` | Verify `vehTypeCD` valid & `xp >= 0`; add `xp` to `veh_type_xp[vehTypeCD]`. | `update({'stats': {'vehTypeXP'}})` + `onCmdResponse`. |
| `10003` | `CMD_ADD_TMAN_XP` | `Int3 [tmanInvID, xp, 0]` | Verify tankman exists & `xp >= 0`; run `tankman.add_xp(xp)`. | `update({'inventory': {8: {'compDescr'}}})` + `onCmdResponse`. |
| `10004` | `CMD_UNLOCK_ALL` | `Int3 [0, 0, 0]` | Add all catalog vehicles, modules, and `autounlockedItems` to `unlocks`; populate `eliteVehicles` for all unlocked catalog vehicles; ensure empty v17 dossiers exist (`resync_dossiers = true`). | `update({'stats': {'unlocks_r', 'eliteVehicles_r'}})` + `resyncDossiers` + `onCmdResponse`. |

### 5.3 Sell Price Detail (`CMD_SELL_VEHICLE` & `CMD_SELL_ITEM`)
Matches `Shop.__getVehiclePriceFromCache` (`.scratch/research/decompiled/client/account_helpers/Shop.py:403-432`) and `VehicleDescr.getDevices` (`common/items/vehicles.py:414-438`) to the exact single credit:
- **Item sell price (`CMD_SELL_ITEM`, `SHELLS`, `EQS`):** `sell_credits(item) = ceil(sellPriceModif * (item_buy_credits + item_buy_gold * exchangeRate))`.
- **Vehicle sell price (`CMD_SELL_VEHICLE`):**
  1. Start with `net_price = (veh_buy_credits, veh_buy_gold)` of the vehicle type.
  2. For each of the 6 component pairs `(default_cd, installed_cd)` where `default_cd != installed_cd`:
     - `chassis`: stock `type.chassis[0]` vs installed
     - `engine`: stock `type.engines[0]` vs installed
     - `fuelTank`: stock `type.fuelTanks[0]` vs installed
     - `radio`: stock `type.radios[0]` vs installed
     - `turret`: stock `type.turrets[0]` vs installed
     - `gun`: **installed turret's stock gun** (`installed_turret.guns[0]`) vs installed gun
     Subtract `shop_price(default_cd)` and add `shop_price(installed_cd)` component-wise `(credits, gold)`.
  3. For each mounted `optionalDevice` in slots `0..3`: add `shop_price(optdev_cd)`.
  4. Base payout = `max(0, ceil(sellPriceModif * (net_credits + net_gold * exchangeRate)))`, plus `sell_credits(shell) * count` for loaded shells when `flags & 16`, plus `sell_credits(eq)` for non-zero equipments when `flags & 32`.

---

## 6. Windows / Sandbox & Safety Constraints

1. **Low Integrity Level (`0x1000`) Path Safety:** Neither `wot065-data`, `wot-baseapp`, nor `wot-server` may call `std::env::temp_dir()`. All account JSON temp files write directly into `--data-dir` (default `.scratch/accounts`). All tests place temporary directories under `target/test-tmp/`.
2. **Atomic Persistence Ordering:** Every successful command writes `<data-dir>/<name>.json.tmp.<pid>` + `sync_all()` + atomic replace **before** dispatching UDP `Account.update(diff)` / `onCmdResponse`. Failed writes fail the command cleanly (`RES_FAILURE`) without mutating persisted disk state out-of-sync with in-memory state.
3. **Live Binary Locking:** Build scripts and tests remind callers on Windows to verify `wot-server.exe` / `bw201-login.exe` / `wot-baseapp.exe` aren't running before `cargo build` / `cargo test`.
4. **No Silent Fixture Auto-Loading:** `wot-server serve` never silently picks up `.scratch/research/decompiled/_generated/*`.

---

## 7. Verification Strategy

1. **`wot065-data` Unit & Golden Tests:**
   - Binary `VehicleDescr` round-trip tests (`15B` stock, 1/2/3 optDevs verifying slot-bit & descending-ID order, emblem/horn/camo combinations) validated against `common/items/vehicles.py` reference byte vectors.
   - Binary `TankmanDescr` round-trip & formula tests (`levelUpXpCost`, `addXP`, `addSkill`, `dropSkill`, `respecialize`, `replacePassport` male & female `premiumGroups`) cross-checked against Python 2 / `common/items/tankmen.py`.
   - Binary dossier tests verifying byte lengths and offsets (`154B` Account v19, `138B` Vehicle v17, `132B` Tankman v10, plus populated `vehTypeFrags` / `vehDossiersCut`).
   - Full `AccountEngine` command matrix tests (happy paths + every rejection rule: overweight chassis, incompatible gun/shell/optDev/eq filter, duplicate optDev/eq tags, insufficient gold/credits/XP/prereqs/slots/berths, missing `shopRev`, broken vehicle fitting block, `isPaidRemoval` gold vs free destroy).
   - `JsonAccountStore` round-trip, username normalization & path-traversal safety tests.
2. **`wot-baseapp` UDP E2E Tests (`crates/wot-baseapp/tests/`):**
   - Full two-step `CMD_SYNC_DATA` handshake (`rev=0` -> stream, `rev=cur` -> `RES_CACHE`), `CMD_SYNC_SHOP`, `CMD_SYNC_DOSSIERS` (`RES_STREAM`).
   - Multi-command lifecycle over live UDP packets verifying ordered `Account.update(diff)` + `onCmdResponse`/`onCmdResponseExt` + disk persistence + reconnect reload (`rev` preserved, bought tanks/crews/camo intact).
   - Duplicate login kick test: peer A owns `"player1"`, peer B logs in as `"player1"`, verify peer A receives `ON_KICKED_FROM_SERVER (0x84)` and peer B receives full persisted state cleanly.
   - `RES_SHOP_DESYNC (-4)` recovery test.
3. **Retail WoT 0.6.5 Client Live Smoke Test:**
   - Launch `wot-server serve --preset retail` and `--preset sandbox` against `C:\Users\qwerty\Desktop\World_of_Tanks`, verify full hangar operations visually in UI, then verify `wot-server account damage` + restart + `CMD_REPAIR`, plus duplicate client instance kick.
