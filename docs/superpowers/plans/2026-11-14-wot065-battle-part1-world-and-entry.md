# WoT 0.6.5 Battle System Part 1: Wire, Assets, Queue/Training & Entry Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Deliver real retail World of Tanks 0.6.5 map/tank geometry loading, fixed `bw201` Cell/AoI wire codecs, Random Queue matchmaker + Training Room `Prebattle`, and full Hangar `Account <-> Avatar` arena spawn/exit transitions verified by unit and multi-client UDP E2E tests.

**Architecture:** Fix `bw201` C2S/S2C movement message sizes and add typed BigWorld 2.0.1 world/AoI/pose wire codecs (`crates/bw201/src/world.rs`); extend `crates/wot065-data` with battle-relevant XML attributes and the `cache['vehsLock']` diff fix; create `crates/wot-battle` to parse `.cdata` heights/textures, `.chunk` spawns/`ControlPoint`s/models, `.primitives` `bsp2`, `destructibles.xml`, and `arena_defs`, lazily caching `Arc<ArenaMap>` + `NavGrid`; and wire `QueueManager`, `PrebattleManager` (including a ready Team-2 virtual bot for solo Training starts), and `Account <-> Avatar` entity switching into `crates/wot-baseapp`.

**Tech Stack:** Rust 2021 (`bw201`, `wot065-data`, `wot-battle`, `wot-baseapp`), `wg-toolkit`, `glam = "0.29"`, `png = "0.17"`, `flate2 = "1"`, `tokio`, `serde`/`serde_json`, `rand = "0.8"`.

**Spec:** [2026-11-14-wot065-battle-system-design.md](file:///c:/Users/qwerty/Documents/GitHub/interstellar/docs/superpowers/specs/2026-11-14-wot065-battle-system-design.md)

## Global Constraints

1. **Windows Low Integrity Level (`0x1000`):** Never call `std::env::temp_dir()`; use `target/test-tmp/` for test artifacts and `.scratch/` or `--log-dir`/`--data-dir` at runtime.
2. **Locked `.exe` Guard & Sandbox Rule:** Stop running `wot-server.exe`, `bw201-login.exe`, `wot-baseapp.exe` before building/testing; run `cargo check --workspace` / `cargo test --workspace` (leave `--release` builds outside sandbox).
3. **Preserve Existing Docs & Comments:** Do not remove or alter unrelated docstrings/comments.
4. **0-Based vs 1-Based EntityTypeIDs:** Wire packets (`createBasePlayer`, `createEntity`) MUST use 0-based indices from `entities.xml`: `Account = 0`, `Avatar = 1`, `Vehicle = 5`, `AreaDestructibles = 7`.
5. **Verified Wire Quirks:**
   - `Direction3D` transmits `(roll, pitch, yaw)` (`basictypes.cpp:38-50`).
   - `YawPitchRoll` pitch uses `halfAngleToInt8` (`pi / 254`), while `YawPitch` and `createEntity` use `angleToInt8` (`pi / 128`).
   - `Vehicle.engineMode` wire layout is 2 inline `u8` bytes (NO length prefix).
   - `enableEntities` ACK: Server sends `RESET_ENTITIES(0)` and MUST wait for client `ENABLE_ENTITIES (0x0A)` before sending `CREATE_BASE_PLAYER(Avatar)`.
6. **Offline Cargo Compatibility:** Use only crates already resolved in `Cargo.lock` (`glam`, `png`, `flate2`, `rand`, `serde`, `serde_json`, `thiserror`, `tracing`, `wg-toolkit`).

## Review Focus

1. **`Coord` & `AVATAR_UPDATE_*` Wire Framing:** Incorrect sizes in `crates/bw201/src/interfaces.rs` (`BaseAppExtInterface` IDs `2..=5` or `ClientInterface` IDs `17..=48`) desync multi-element Mercury bundles — pinned by bundle round-trip tests in Task 1.
2. **`Direction3D` & `PackedXYZ` Edge Cases:** Negative coordinates near chunk bounds (`-500..+500`) and signed angle quantization wrapping around `±pi` — pinned by extreme-coordinate pose tests in Task 1.
3. **`terrain2/heights` Seam & `dominantTextures` Indexing:** Reading the `69x69` grid without skipping `VISIBLE_OFFSET = 2` misaligns heightmaps by 3.125 m; shifting `dominantTextures` cells `>> 4` breaks terrain materials — pinned by cross-chunk seam and texture lookup tests in Task 3.
4. **`Training.py` Solo Start Blocker:** `Training.py:451-454` disables Start when either Team 1 or Team 2 has `< 1` `READY` account in `prebattle.rosters` — pinned by solo Training prebattle roster test in Task 5.
5. **`cache['vehsLock']` Diff Key & Disconnect Safety:** Emitting only `inventory[1]['lock']` fails to update client vehicle locks, and abrupt client disconnect during `SwitchingToArena` must not strand orphan arenas or crash `SessionRouter` — pinned by Task 2 and Task 5 tests.

---

### Task 1: Fix `bw201` Movement/Bundle Framing & Add World/AoI Codecs

**Files:**
- Modify: `crates/bw201/src/interfaces.rs`
- Modify: `crates/bw201/src/bundle.rs`
- Create: `crates/bw201/src/world.rs`
- Modify: `crates/bw201/src/lib.rs`

**Interfaces:**
- Consumes: `crate::stream::{Address, BinaryReader, BinaryWriter, StreamError}`, `crate::interfaces::LengthStyle`
- Produces:
  - Fixed `baseapp_ext_interface()` (`AVATAR_UPDATE_IMPLICIT=16`, `AVATAR_UPDATE_EXPLICIT=25`, `AVATAR_UPDATE_WARD_IMPLICIT=19`, `AVATAR_UPDATE_WARD_EXPLICIT=28`) and `client_interface()` (`17..=48` as `LengthStyle::Fixed(id_size + pos_size + dir_size)`)
  - `bw201::world::{Direction3D, PackedXZ, PackedXYZ, Yaw, YawPitch, YawPitchRoll, ResetEntities, CreateBasePlayer, CreateCellPlayer, SpaceData, SpaceDataMapping, EnterAoI, EnterAoIOnVehicle, LeaveAoI, CreateEntity, UpdateEntity, TickSync, SetVehicle, DetailedPosition, ForcedPosition, ControlEntity, S2CAvatarUpdate, C2SAvatarUpdate, RequestEntityUpdate, SPACE_DATA_TOD_KEY, SPACE_DATA_MAPPING_KEY_CLIENT_SERVER, SPACE_DATA_WEATHER, NO_ID_ALIAS}`

- [ ] **Step 1: Write failing tests for interface sizes, `special_oversize`, and world/pose codecs**

  In `crates/bw201/src/interfaces.rs` and `crates/bw201/src/world.rs` tests:
  ```rust
  #[test]
  fn baseapp_ext_and_client_avatar_update_lengths_match_bw201() {
      let ext = baseapp_ext_interface();
      assert_eq!(ext.entry(baseapp_ext_ids::AVATAR_UPDATE_IMPLICIT).unwrap().style, LengthStyle::Fixed(16));
      assert_eq!(ext.entry(baseapp_ext_ids::AVATAR_UPDATE_EXPLICIT).unwrap().style, LengthStyle::Fixed(25));
      assert_eq!(ext.entry(baseapp_ext_ids::AVATAR_UPDATE_WARD_IMPLICIT).unwrap().style, LengthStyle::Fixed(19));
      assert_eq!(ext.entry(baseapp_ext_ids::AVATAR_UPDATE_WARD_EXPLICIT).unwrap().style, LengthStyle::Fixed(28));
      let cli = client_interface();
      assert_eq!(cli.entry(client_ids::AVATAR_UPDATE_FIRST).unwrap().style, LengthStyle::Fixed(12));
      assert_eq!(cli.entry(client_ids::AVATAR_UPDATE_LAST).unwrap().style, LengthStyle::Fixed(1));
  }

  #[test]
  fn packed_xyz_and_angles_preserve_signs_and_limits() {
      let p = PackedXYZ::encode([-450.5, 123.25, 498.75]);
      let out = p.decode();
      assert!((out[0] - (-450.5)).abs() < 2.1);
      assert!((out[1] - 123.25).abs() < 0.25);
      assert!((out[2] - 498.75).abs() < 2.1);
      let ypr = YawPitchRoll::encode([1.5, -0.4, 0.2]);
      let (yaw, pitch, roll) = ypr.decode();
      assert!((yaw - 1.5).abs() < 0.03);
      assert!((pitch - (-0.4)).abs() < 0.02);
      assert!((roll - 0.2).abs() < 0.03);
  }
  ```
  Also add a multi-element bundle decode test combining `S2CAvatarUpdate` + `TickSync` + `UpdateEntity`, and an oversize (`70_000` byte) variable element round-trip in `bundle.rs`.

- [ ] **Step 2: Run tests to verify failure**
  Run: `cargo test -p bw201`
  Expected: FAIL on interface lengths (`10` vs `16`, `Variable(2)` vs `Fixed(12)`), missing `bw201::world`, and oversize trailer bug.

- [ ] **Step 3: Fix `interfaces.rs`, `bundle.rs`, and implement `crates/bw201/src/world.rs`**
  - Update `crates/bw201/src/interfaces.rs` fixed lengths and `avatar_update_id_style(id)` calculation (`id = 17 + (id_kind*16) + (pos_kind*4) + dir_kind`, `id_size = if id_kind == 0 { 4 } else { 1 }`, `pos_size = [5, 4, 3, 0][pos_kind]`, `dir_size = [3, 2, 1, 0][dir_kind]`).
  - Fix `crates/bw201/src/bundle.rs` `special_oversize` handling to read the tail-displaced `u32` length matching `bw201/src/lib/network/interface_element.cpp:196-225` (including encoding side when `len >= 0xFFFF`).
  - Implement `crates/bw201/src/world.rs`:
    - `Direction3D` (`write_to` / `read_from` in `[roll, pitch, yaw]` LE order)
    - `PackedXZ` (24-bit IEEE bias-2 pack/unpack via `BW_PACK3` 3-byte LE) & `PackedXYZ` (`PackedXZ` + 16-bit LE Y) matching `msgtypes.hpp:230-586`
    - Exact structs/codecs for `ResetEntities` (`u8 keep_player_on_base`), `CreateBasePlayer`, `CreateCellPlayer`, `SpaceData` (`i32 space_id`, `Address entry_id`, `u16 key`, payload; helper `SpaceData::mapping(space_id, entry_id, matrix_4x4, path)`), `EnterAoI` (`NO_ID_ALIAS = 0xFF`), `EnterAoIOnVehicle`, `LeaveAoI`, `CreateEntity` (`compression: u8 = 0`, pose + tagged properties vec), `UpdateEntity`, `TickSync`, `SetVehicle`, `DetailedPosition`, `ForcedPosition`, `ControlEntity`, `S2CAvatarUpdate` (`PosKind::{FullPos, OnChunk, OnGround, NoPos}`, `DirKind::{YawPitchRoll, YawPitch, Yaw, NoDir}`, `IdKind::{NoAlias(i32), Alias(u8)}`), `C2SAvatarUpdate` (decoding `Coord` 12B + `YawPitchRoll` 3B), and `RequestEntityUpdate`.
  - Expose `pub mod world;` in `crates/bw201/src/lib.rs`.

- [ ] **Step 4: Run tests to verify pass**
  Run: `cargo test -p bw201`
  Expected: PASS (`0` failures).

- [ ] **Step 5: Commit**
  ```bash
  git add crates/bw201/src/interfaces.rs crates/bw201/src/bundle.rs crates/bw201/src/world.rs crates/bw201/src/lib.rs
  git commit -m "fix(bw201): correct avatarUpdate framing and add world/AoI wire codecs"
  ```

---

### Task 2: Extend `wot065-data` Battle Item Attributes & Fix `vehsLock` Diff

**Files:**
- Modify: `crates/wot065-data/src/item_defs.rs`
- Modify: `crates/wot065-data/src/catalog.rs`
- Modify: `crates/wot065-data/src/ops/mod.rs`
- Modify: `crates/wot065-data/src/store.rs`

**Interfaces:**
- Consumes: Existing `Catalog`, `VehicleDef`, `ShotDef`, `OptionalDeviceDef`, `EquipmentDef`, `DiffBuilder`
- Produces:
  - `ShellKind { ArmorPiercing, ArmorPiercingCr, HighExplosive, HollowCharge }`, `ShellDef` (on `ShotDef.shell: ShellBattleDef` or attached lookup in `Catalog`), `ShotDef.gravity: f64` (with `0.8` speed / `0.64` gravity scaling), `ArmorZoneMap` (`Vec<(String, f64)>` or `BTreeMap<String, (f64, bool)>`), `HitTesterSpec { model: String, enclosing_box: Option<([f32; 3], [f32; 3])> }`, `VehicleBattleDef`, `OptionalDeviceEffect`, `EquipmentEffect`
  - `Catalog::resolve_shell(&self, shell_cd: i32) -> Option<&ShellXmlDef>`
  - Fixed `DiffBuilder::veh_lock(&mut self, veh_inv_id: i64, lock: i32)` emitting `cache['vehsLock'][veh_inv_id] = lock`

- [ ] **Step 1: Write failing tests in `crates/wot065-data`**
  Add unit tests checking:
  1. `ShotDef` on `MS-1` / `T-34` applies `0.8` speed factor (`speed == xml_speed * 0.8`) and `0.64` gravity factor (`gravity == xml_gravity * 0.64`), plus `ShellXmlDef` (`kind`, `caliber`, `armor_damage`, `devices_damage`, `explosion_radius` fallback `caliber^2 / 5555.0`).
  2. `VehicleDef` + `ComponentDef` battle fields parse `hull.armor` (`armor_1..`), `hull_position`, `turret_positions`, `gun_position`, `circular_vision_radius`, `speed_limits`, `invisibility`, `engine.power`, `fire_starting_chance`, `radio.distance`, `max_climb_angle`, `rotation_speed`, `rotation_is_around_center`, `top_right_carrying_point`, `hit_tester` collision `.model` paths, `repair_cost` & `max_regen_health` on every component, and `<script>` effect specs on `OptionalDeviceDef` & `EquipmentDef`.
  3. `DiffBuilder::new().veh_lock(101, 16).build(1, 2)` contains key `"cache"` -> `"vehsLock"` -> `101: 16` (`account_state` lock round-trip).

- [ ] **Step 2: Run tests to verify failure**
  Run: `cargo test -p wot065-data --lib`
  Expected: FAIL on missing fields/methods and `"cache"` missing `"vehsLock"`.

- [ ] **Step 3: Implement XML parsers, `ShotDef` scaling, battle attrs, and `DiffBuilder` fix**
  - Extend `crates/wot065-data/src/item_defs.rs` & `catalog.rs` keeping all new fields `#[serde(default)]` so existing serialized `vehicles_catalog.json` fixtures remain readable, while live `Catalog::load` populates full battle structs.
  - Parse `<shells>` XML entries per nation in `load_nation_shells` (`kind`, `caliber`, `damage(armor, devices)`, `explosionRadius` fallback, `effects`, `isTracer`) and store `shells: BTreeMap<i32, ShellXmlDef>` on `Catalog`.
  - Parse `<hull>`, `<chassis>`, `<engines>`, `<fuelTanks>`, `<radios>`, `<turrets>`, `<guns>` battle parameters, `<invisibility>`, `<speedLimits>`, `maxRegenHealth`, `repairCost`, `<script>` parameters on `optional_devices.xml` and `equipments.xml`, and `projectileSpeedFactor` from `vehicles/common/vehicle.xml` (default `0.8`).
  - Fix `DiffBuilder::veh_lock` in `crates/wot065-data/src/ops/mod.rs` to write `cache['vehsLock'][veh_inv_id] = lock` (clearing to `0` when unlocked), and add `damage_vehicle_components` helper in `store.rs` honoring `vehicles.py:405-412` max repair costs for Task 3/Part 3 readiness.

- [ ] **Step 4: Run tests to verify pass**
  Run: `cargo test -p wot065-data --lib`
  Expected: PASS (`0` failures).

- [ ] **Step 5: Commit**
  ```bash
  git add crates/wot065-data/src/item_defs.rs crates/wot065-data/src/catalog.rs crates/wot065-data/src/ops/mod.rs crates/wot065-data/src/store.rs
  git commit -m "feat(wot065-data): parse battle vehicle/shell/equipment attrs and fix vehsLock diff"
  ```

---

### Task 3: Scaffold `crates/wot-battle` Asset Loaders (`.cdata`, `.chunk`, `.primitives`/`bsp2`, `arena_defs`, `destructibles`)

**Files:**
- Modify: `Cargo.toml`
- Create: `crates/wot-battle/Cargo.toml`
- Create: `crates/wot-battle/src/lib.rs`
- Create: `crates/wot-battle/src/assets/mod.rs`
- Create: `crates/wot-battle/src/assets/zip_reader.rs`
- Create: `crates/wot-battle/src/assets/bin_section.rs`
- Create: `crates/wot-battle/src/assets/bsp2.rs`
- Create: `crates/wot-battle/src/assets/terrain.rs`
- Create: `crates/wot-battle/src/assets/chunk.rs`
- Create: `crates/wot-battle/src/assets/arena_defs.rs`
- Create: `crates/wot-battle/src/assets/destructibles.rs`

**Interfaces:**
- Consumes: `glam::{Mat4, Vec3, Vec4}`, `png`, `flate2`, `wot065_data::xml`
- Produces:
  - `BinSectionMap::parse(bytes: &[u8]) -> Result<BTreeMap<String, &[u8]>, AssetError>`
  - `BspTree::parse(bsp2_bytes: &[u8], bsp2_mats_bytes: &[u8]) -> Result<BspTree, AssetError>` + `BspTree::raycast(&self, origin: Vec3, dir: Vec3, max_dist: f32) -> Option<BspHit>` + `BspTree::raycast_all(&self, origin: Vec3, dir: Vec3, max_dist: f32) -> Vec<BspHit>`
  - `TerrainChunk { heights: Vec<f32>, min_y: f32, max_y: f32, dom_tex_indices: Vec<u8>, tex_names: Vec<String> }` (`69x69` grid, `VISIBLE_OFFSET=2`, bilinear/barycentric `(x0^z0)&1` height+normal sampling)
  - `MaterialKinds::load(res_dir: &Path)` (`system/data/material_kinds.xml`)
  - `ArenaDefsCatalog::load(res_dir: &Path)` (all 20 maps from `scripts/arena_defs/_list_.xml`, `_common_.xml`, map XMLs, bounding boxes)
  - `ChunkData` (`SpawnPoint`, `ControlPoint`, static `<model>` transforms, `<speedtree>` placements, `.vlo` water rectangles)
  - `DestructiblesCatalog::load(res_dir: &Path)` (`scripts/destructibles.xml`)

- [ ] **Step 1: Write failing asset parsing tests against retail files (skip-safe if game_dir missing, asserting exact outputs when present)**

  Tests inside `crates/wot-battle`:
  - `bin_section_and_bsp2_parse_hull_and_building`: loads `vehicles/russian/R11_MS-1/collision/Hull.primitives` (`340` tris, `92` nodes, material `"armor_1"`/`"fuelTank"` present, trailing `\0` format-string garbage tolerated) and `content/Buildings/bld000_base/normal/lod0/bld000_base.primitives` (`4369` tris, `3882` nodes); casts vertical ray through center of `Hull` hitting hull armor triangles.
  - `cdata_terrain_seam_and_dominant_textures`: loads `01_karelia/00000001o.cdata` & `00010001o.cdata`, asserts `69x69` heights overlap cols `64..=68 == 0..=4` within `1e-3`, barycentric height/normal valid, and `dominantTextures` resolves pure indices against `material_kinds.xml` without bitshift errors.
  - `chunk_hex_coords_spawns_and_control_points`: decodes `"0000fffdo"` -> `(0, -3)` world `(0.0, -300.0)`; parses `04_himmelsdorf` chunks extracting team 1 & team 2 `SpawnPoint`s and `<ControlPoint>` bases (`radius = 50.0, pointsPerSecond = 1.0, maxPointsPerSecond = 3.0`); parses all 20 maps from `arena_defs/_list_.xml`; parses `scripts/destructibles.xml`.

- [ ] **Step 2: Run tests to verify compilation failure**
  Run: `cargo test -p wot-battle`
  Expected: FAIL (`wot-battle` crate does not exist yet).

- [ ] **Step 3: Implement `crates/wot-battle` asset parsers**
  - Add `"crates/wot-battle"` to root `Cargo.toml` and create `crates/wot-battle/Cargo.toml` referencing workspace lockfile versions (`glam = "0.29"`, `png = "0.17"`, `flate2 = "1"`, `rand = "0.8"`, `serde = "1"`, `serde_json = "1"`, `thiserror = "1"`, `tracing = "0.1"`, `wot065-data = { path = "../wot065-data" }`, `bw201 = { path = "../bw201" }`).
  - `zip_reader.rs`: read ZIP local/central directory headers + `Stored`/`Deflate` entries in ~70 lines using `flate2`.
  - `bin_section.rs`: `0x42a14e65` tail index decoder.
  - `bsp2.rs`: `b"BSP\0"` triangle + pre-order node tree (`0x01`/`0x02`/`0x04` flags) + `bsp2_materials` `<id>` list + sorted `raycast_all` and earliest `raycast` with face normal.
  - `terrain.rs`: `b"hmp\0"` + `b"gnpq"` PNG RGBA LE `i32` mm decoder (`VISIBLE_OFFSET = 2`, `(x0^z0)&1` triangle normal & height), `b"mat\0"` + `b"\x00piz"` zlib `128x128` dominant-texture resolver against `material_kinds.xml`.
  - `chunk.rs`, `arena_defs.rs`, `destructibles.rs`: `space.settings` bounds (`minX..maxX, minY..maxY`), `.chunk` parser (models resolving `<nodelessVisual>`/`<nodefullVisual>` -> `.primitives`, `SpawnPoint`, `ControlPoint`, `speedtree`), `.vlo` water rectangles, `arena_defs` catalog, and `destructibles.xml` parser.

- [ ] **Step 4: Run tests to verify pass**
  Run: `cargo test -p wot-battle`
  Expected: PASS (`0` failures).

- [ ] **Step 5: Commit**
  ```bash
  git add Cargo.toml crates/wot-battle/Cargo.toml crates/wot-battle/src/
  git commit -m "feat(wot-battle): add retail cdata, chunk, bsp2, destructibles, and arena_defs loaders"
  ```

---

### Task 4: Lazy `MapStore` Cache, `BalanceConfig`, Tank HitTesters & 2D `NavGrid`

**Files:**
- Create: `crates/wot-battle/src/balance.rs`
- Create: `crates/wot-battle/src/assets/map_store.rs`
- Modify: `crates/wot-battle/src/assets/mod.rs`
- Modify: `crates/wot-battle/src/lib.rs`

**Interfaces:**
- Consumes: `crate::assets::*`, `wot065_data::catalog::{Catalog, VehicleDef}`
- Produces:
  - `BalanceConfig` (`load_or_default(path: Option<&Path>)`, holds defaults for client-stripped tags: `normalization_deg: {ap: 5.0, apcr: 2.0, he: 0.0}`, `ricochet_cos`, `terrain_resistance: [f32; 3]` fallbacks, `brake_force_factor`, `burn_fraction_per_sec`, `spot_linger_secs`, `xp_credits_factors`, matchmaker weights)
  - `ArenaMap` (`arena_type_id`, `def`, `bounds`, `terrain`, `obstacles: Vec<StaticObstacle>`, `bsp_cache: Vec<Arc<BspTree>>`, `foliage: Vec<FoliageVolume>`, `spawns: Vec<SpawnPoint>`, `bases: Vec<TeamBaseSpec>`, `destructibles_by_chunk: BTreeMap<u16, Vec<ChunkDestructible>>`, `nav_grid: NavGrid`) + spatial grid index for fast `O(1)` cell lookup on obstacles/foliage
  - `NavGrid` (`cell_size: f32`, `width: usize`, `height: usize`, `find_path(&self, from: Vec3, to: Vec3) -> Vec<Vec3>`)
  - `MapStore::get_or_load(&self, arena_type_id: i32) -> Result<Arc<ArenaMap>, AssetError>`
  - `VehicleCollisionCache::get_or_load(&self, veh_def: &VehicleDef, ...) -> Result<Arc<VehicleCollisionSet>, AssetError>`

- [ ] **Step 1: Write failing tests for lazy caching, spatial obstacle query, and `NavGrid` A* on `04_himmelsdorf`**

  Assert:
  - Repeated `MapStore::get_or_load(4)` (Himmelsdorf) returns the exact same `Arc::ptr_eq` cached map without re-parsing disk assets.
  - `ArenaMap::sample_height(x, z)` returns continuous height across chunk seams; `NavGrid::find_path(team1_spawn, team2_spawn)` returns a non-empty path avoiding static buildings (`bsp2` obstacles) and steep impassable terrain.
  - `VehicleCollisionCache` loads `MS-1` (`Hull`, `Chassis`, `Turret_01`, `Gun_01`) `.primitives` BSPs cleanly.

- [ ] **Step 2: Run tests to verify failure**
  Run: `cargo test -p wot-battle`
  Expected: FAIL (`MapStore` / `NavGrid` / `BalanceConfig` not implemented).

- [ ] **Step 3: Implement `BalanceConfig`, `MapStore`, spatial grid, `NavGrid`, and `VehicleCollisionCache`**
  - Keep `MapStore` thread-safe (`RwLock<HashMap<i32, Arc<ArenaMap>>>`) so first battle on a map parses lazily and caches in memory.
  - Build uniform 2D spatial grid buckets (`25m` cells) over `StaticObstacle` AABBs, `FoliageVolume`s, and `TerrainMap` for fast movement/raycast/LoS queries in Part 2.
  - Derive coarse `NavGrid` (e.g. `10m` cells) marking cells blocked if solid obstacle AABB overlaps tank clearance height (`~2.5m`), slope > `25°`, or deep water, plus A* pathfinder with string-pulling smoothing.

- [ ] **Step 4: Run tests to verify pass**
  Run: `cargo test -p wot-battle`
  Expected: PASS (`0` failures).

- [ ] **Step 5: Commit**
  ```bash
  git add crates/wot-battle/src/balance.rs crates/wot-battle/src/assets/map_store.rs crates/wot-battle/src/assets/mod.rs crates/wot-battle/src/lib.rs
  git commit -m "feat(wot-battle): add lazy MapStore cache, BalanceConfig, NavGrid, and VehicleCollisionCache"
  ```

---

### Task 5: Matchmaker Queue, Training Room `Prebattle`, and `Account <-> Avatar` World Entry & Exit

**Files:**
- Create: `crates/wot-battle/src/lobby/mod.rs`
- Create: `crates/wot-battle/src/lobby/queue.rs`
- Create: `crates/wot-battle/src/lobby/prebattle.rs`
- Create: `crates/wot-battle/src/world/mod.rs`
- Create: `crates/wot-battle/src/world/arena.rs`
- Modify: `crates/wot-battle/src/lib.rs`
- Modify: `crates/wot-baseapp/Cargo.toml`
- Modify: `crates/wot-baseapp/src/channel.rs`
- Create: `crates/wot-baseapp/src/battle_host.rs`
- Modify: `crates/wot-baseapp/src/protocol.rs`
- Modify: `crates/wot-baseapp/src/registry.rs`
- Modify: `crates/wot-baseapp/src/lib.rs`
- Create: `crates/wot-baseapp/tests/e2e_battle_entry.rs`

**Interfaces:**
- Consumes: `bw201::world::*`, `wot065_data::{Catalog, AccountProfile, VehicleDescr}`, `wot_battle::{MapStore, BalanceConfig}`
- Produces:
  - `LobbyCoordinator` handling `QueueCommand::{Enqueue, Dequeue, QueueInfo, ForceQueue}` & `TrainingCommand::{CreateTraining, PrbJoin, PrbLeave, PrbReady, PrbNotReady, PrbAssign, PrbChArena, PrbChRound, PrbOpen, PrbChComment, PrbChArenaVoip, PrbTeamReady, PrbTeamNotReady, PrbKick, ReqPrebattles, ReqPrebattleRoster}`
  - `BattleWorld` & `ArenaInstance` supporting arena creation from queue/training, `SpawnPoint` placement, initial `VEHICLE_LIST` + `PERIOD(WAITING/PREBATTLE/BATTLE)` sync, `on_client_ready(avatar_id)`, basic movement forwarding (`vehicle_moveWith` stub stepping velocity safely inside Part 1 until full Part 2 physics swaps in), and `leave_arena(avatar_id)`
  - `ChannelState::send_unreliable(&mut self, blowfish: &BlowfishKey, payload: &[u8]) -> Vec<u8>`
  - `BaseApp` `EntityIdAllocator`, `SessionState::{Hangar, SwitchingToArena, InArena, SwitchingToHangar}`, and full client `Account <-> Avatar` entity switch sequence

- [ ] **Step 1: Write failing unit tests (`wot-battle`) and multi-client UDP E2E test (`crates/wot-baseapp/tests/e2e_battle_entry.rs`)**

  - `wot-battle` unit tests:
    - Queue hybrid matchmaker forms balanced team pairing 2 concurrent players across tiers using `vehicle.xml` weights, backfilling bots cleanly to `team_size`, and emitting `QUEUE_INFO` lengths/levels/classes.
    - Solo `createTraining` automatically populates Team 2 (`roster = 2`) with a `READY` virtual bot so `Training.py`'s `minActive >= 1` is satisfied immediately once host readies on Team 1; `CMD_PRB_ASSIGN`, `CMD_PRB_CH_ARENA`, `CMD_PRB_CH_COMMENT`, and `CMD_PRB_TEAM_READY` emit valid pickled `PREBATTLE_UPDATE` payloads.
  - `e2e_battle_entry.rs`:
    - Spins up `wot-baseapp` on ephemeral UDP port using a test account store under `target/test-tmp/` and retail `--game-dir` (or minimal test map fixture if absent).
    - **Random Battle flow:** Client logs in -> `CMD_SYNC_DATA` -> `CMD_ENQUEUE_FOR_ARENA` -> receives `onEnqueued` (`0x85`) -> wait expires / `CMD_FORCE_QUEUE` -> receives `onArenaCreated` (`0x88`) + `RESET_ENTITIES(0)` -> sends `ENABLE_ENTITIES` -> receives `CREATE_BASE_PLAYER(type=1 Avatar)` + `SPACE_DATA(weather, ToD, mapping)` + `CREATE_CELL_PLAYER` + `ENTER_AOI` -> sends `REQUEST_ENTITY_UPDATE` -> receives `CREATE_ENTITY(type=5 Vehicle)` + `Avatar.updateArena(VEHICLE_LIST)` -> sends `Avatar.onClientReady` (`0xCA`) -> sends `Avatar.leaveArena` (`0xCB`) -> receives `RESET_ENTITIES(0)` -> sends `ENABLE_ENTITIES` -> receives `CREATE_BASE_PLAYER(type=0 Account)` + `SHOW_GUI`.
    - **Training Room flow:** Client sends `Account.createTraining(arenaTypeID, 900, true, "test")` (`0xC9`) -> receives `onPrebattleJoined` + `updatePrebattle(SETTINGS, ROSTER)` showing ready virtual bot in Team 2 -> sets `CMD_PRB_READY` & `CMD_PRB_TEAM_READY` -> enters battle cleanly -> exits to Hangar with tank unlocked.
    - **Disconnect during `SwitchingToArena`:** Client drops after `onArenaCreated` without sending `ENABLE_ENTITIES`; server handles timeout/cleanup cleanly without crash or stranding subsequent login.

- [ ] **Step 2: Run tests to verify failure**
  Run: `cargo test -p wot-battle && cargo test -p wot-baseapp --test e2e_battle_entry`
  Expected: FAIL (`battle_host` and lobby modules missing).

- [ ] **Step 3: Implement Lobby (`QueueManager` + `PrebattleManager`), initial `BattleWorld`, `ChannelState::send_unreliable`, and `BaseApp` `Account <-> Avatar` Switcher**
  - Implement `QueueManager` (matchmaking tick window, `vehicle.xml` weight balance, level spread, configurable `team_size`, bot team fill) and `PrebattleManager` (Training rooms with solo Team-2 ready bot, rosters `1/2/17/18`, `CMD_PRB_*` `400..412`, `CMD_REQ_PREBATTLES*` `520..522` compressed streams, `updatePrebattle`).
  - Wire `BaseAppExtInterface` (`ENABLE_ENTITIES` state machine distinguishing initial Hangar boot, `SwitchingToArena` -> spawn `Avatar` + `SpaceData` + `CreateCellPlayer` + `EnterAoI`, and `SwitchingToHangar` -> respawn `Account` + `showGUI` + `resyncDossiers`!).
  - Replace hardcoded `player_id = 1` with `EntityIdAllocator` (`1..`, never colliding with client-only `>= 1<<30`).
  - Handle `REQUEST_ENTITY_UPDATE` to reply with compressed/uncompressed `CREATE_ENTITY(Vehicle)` (`typeID = 5`, initial `publicInfo`, `health`, `isCrewActive`, `engineMode` 2-byte inline tuple, empty `damageStickers`, empty `publicStateModifiers`).
  - Stub `Account` chat (`0..=2`) cleanly (replying basic `chatInitialization` / `sysMessage` / empty roster ack as needed so client GUI never hangs waiting for chat init) and wire `100ms` server tick loop inside `wot-baseapp` `serve_with_ready`.

- [ ] **Step 4: Run tests to verify pass**
  Run: `cargo test --workspace`
  Expected: PASS (`0` failures across all workspace crates; `#[ignore] live_daemon_udp_smoke` remains skipped unless live server started).

- [ ] **Step 5: Commit**
  ```bash
  git add crates/wot-battle/ crates/wot-baseapp/
  git commit -m "feat(battle): implement queue matchmaker, training prebattle, and Account<->Avatar entry flow"
  ```
