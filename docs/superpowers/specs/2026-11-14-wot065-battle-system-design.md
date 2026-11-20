# World of Tanks 0.6.5 Full Battle System Design Specification

**Date:** 2026-11-14
**Status:** Approved for implementation planning
**Baseline:** LoginApp + persistent Hangar BaseApp (`wot-server`, `crates/bw201`, `crates/wot-baseapp`, `crates/wot065-data`)

---

## 1. Goals & Settled Scope

Implement the complete **World of Tanks 0.6.5 battle system** supporting both solo play against server-side bots and multiplayer (`2+` live clients): Hangar -> Queue or Training Room -> 3D Map Load -> Driving, Physics, Shooting, Penetration, Module/Crew/Fire Crits, Consumables, Destructibles, Spotting/Radio, Base Capture, Battle Chat, Postmortem/SPG Camera -> Battle Results -> Hangar Persistence & Dossiers.

### Settled Design Decisions
- **Topology:** New pure-Rust simulation crate **`crates/wot-battle`** running **in-process inside `wot-baseapp`** (clean boundary between deterministic world simulation and async Mercury/UDP networking; no inter-process CellApp IPC overhead; socket-free unit testing).
- **Entry Modes:**
  - **Random Battle ("В бой!", `CMD_ENQUEUE_FOR_ARENA`):** Hybrid matchmaker waits a configurable window for concurrent live players, balances by retail `vehicle.xml` weight/tier rules, and backfills remaining slots with tier-appropriate server bots up to a configurable team size.
  - **Training Room (`createTraining`, `Prebattle`):** Choose any of the 20 retail arenas, set round length/privacy/comment, swap teams, auto-populate a `READY` virtual bot in Team 2 so solo hosts pass `Training.py`'s `minPlayersInTeam >= 1` check, and allow `addBotToArena` + active/passive bot toggle. Squads (`createSquad`) and Companies (`createCompany`) remain out of scope for this milestone.
- **World Geometry & Tank Hitboxes:** Parsed directly from retail client assets (`--game-dir/res/`):
  - `.cdata` heightmaps (`terrain2/heights`) + ground types (`terrain2/dominantTextures` + `system/data/material_kinds.xml`)
  - `.chunk` object placements (`<model>`, `<speedtree>`), `SpawnPoint` UserDataObjects, and `<ControlPoint>` base zones
  - `.primitives` static/collision BSP trees (`bsp2`, `bsp2_materials`) + `.model`/`.visual` metadata
  - `scripts/destructibles.xml` via `AreaDestructibles` entities
  - Lazy per-map parsing on first arena launch with an in-memory `Arc<ArenaMap>` cache (~4–21 MB raw chunks per map).
- **Physics, Combat & Visibility:** Full retail WoT 0.6.5 rules — engine power/weight/terrain resistance/max climb slope/heightmap pitch-roll/tracks/collisions/ramming (`onPushed`); adaptive gravity parabola (`computeProjectileTrajectory`), dispersion/aim-shrink, AP/APCR/HE BSP armor penetration (normalization, 70° ricochet, 2x/3x overmatch, HE splash), internal module & crew crits, fires, ammo-rack detonations, active `ACTIVATE_EQUIPMENT` + passive `OptionalDevices`; LoS visibility rays across terrain/BSP/foliage, camo (`moving`/`still`/`atShot`/camo net/optics), 50m proxy spotting, spot linger, `radio.distance` AoI replication & minimap `updatePositions`.
- **Bot AI:** Derived 2D coarse passability `NavGrid` (built from heightmap + BSP obstacles since `.chunk` files contain zero `PatrolNode` UDOs), class-aware routing, lead-aimed shooting against strictly team-spotted enemies, base capture/decap, plus passive target mode option in Training Rooms.
- **Economy, Lifecycle & Social:**
  - **Random Battles:** Configurable prebattle countdown (default `30s`), win by extermination/base capture (`100` pts)/timeout, XP/FreeXP/CrewXP/Credits calculation, exact repair & spent shells/consumables deduction (+ auto-repair/load/equip), `AccountDossier` v19 / `VehicleDossier` v17 / `TankmanDossier` v10 updates & medals, `SYS_MESSAGE_TYPE.battleResults` service channel card, and `LOCK_REASON.ON_ARENA` until arena completion on early hangar exit.
  - **Training Rooms:** Free ammo/repair/consumables, zero XP/credits/dossiers, zero tank lock after exit, no friendly-fire penalties.
  - **In-Battle Social & Camera:** Team/All battle chat, minimap sector pings (`ATTENTIONTOCELL`), radial commands (`CHAT_COMMANDS`), postmortem & SPG AoI camera focus (`bindToVehicle`, `moveTo`), and friendly fire with `TEAM_KILLER` penalties in Random battles.
  - **Dev & Verification:** `server_marker` always enabled; `setDevelopmentFeature` cheats (`heal`, `reload_gun`, `start_fire`, `break_*_track`, `stop_bot`, `destroy_self`) & `CMD_CREATE_ARENA` / `CMD_JOIN_ARENA` gated by CLI `--dev-features` (or `sandbox` preset).

---

## 2. Crate Topology & Responsibilities

1. **`crates/bw201` (Wire Transport Fixes & Battle Codecs)**
   - Fix `BaseAppExtInterface` C2S `AVATAR_UPDATE_*` sizes in `crates/bw201/src/interfaces.rs` (`Coord` is `3 * f32 = 12B`, not `3B`):
     - `AVATAR_UPDATE_IMPLICIT (2)`: `Fixed(16)`
     - `AVATAR_UPDATE_EXPLICIT (3)`: `Fixed(25)`
     - `AVATAR_UPDATE_WARD_IMPLICIT (4)`: `Fixed(19)`
     - `AVATAR_UPDATE_WARD_EXPLICIT (5)`: `Fixed(28)`
   - Fix `ClientInterface::AVATAR_UPDATE_FIRST..=AVATAR_UPDATE_LAST (17..=48)` in `crates/bw201/src/interfaces.rs` from `Variable(2)` to `Fixed(id_size + pos_size + dir_size)` (`2 id_kinds x 4 pos_kinds x 4 dir_kinds`, sizes `1..=12` bytes, `id = 17 + id_kind*16 + pos_kind*4 + dir_kind`).
   - Fix `bundle.rs` `special_oversize` length reading (`u32 LE` at end of body).
   - Add typed binary encoders/decoders in new module `crates/bw201/src/world.rs`: `PackedXZ`, `PackedXHZ`, `PackedXYZ`, `Yaw`, `YawPitch`, `YawPitchRoll` (`angleToInt8 = pi/128`, `halfAngleToInt8 = pi/254` for `YawPitchRoll.pitch`), `Direction3D` (`roll, pitch, yaw` order), `SpaceData`, `CreateBasePlayer`, `CreateCellPlayer`, `EnterAoI`, `EnterAoIOnVehicle`, `LeaveAoI`, `CreateEntity` (`u8 compressionType` + poses + `TAGGED_CELL_ENTITY_DATA`), `UpdateEntity`, `TickSync`, `SetVehicle`, `DetailedPosition`, `ForcedPosition`, `ControlEntity`, `S2CAvatarUpdate`, `C2SAvatarUpdate`, `RequestEntityUpdate`.

2. **`crates/wot065-data` (Catalog Battle Extensions & Account Persistence)**
   - Extend `item_defs.rs` & `catalog.rs` (serde backward-compatible defaults for JSON snapshots):
     - Full `ShellDef` (`kind: ShellKind`, `caliber: f64`, `armor_damage: f64`, `devices_damage: f64`, `explosion_radius: f64` with fallback `caliber^2 / 5555.0`, `effects_index`, `is_tracer`; apply `projectileSpeedFactor = 0.8` -> `shot.speed * 0.8`, `shot.gravity * 0.64`).
     - Vehicle/component battle attributes: `speedLimits`, `invisibility: (f64, f64)`, `hull` (`armor: Vec<f64>`, `primary_armor`, `turret_positions`, `ammo_bay_health`, `hit_tester`), `chassis` (`hull_position`, `top_right_carrying_point`, `max_climb_angle`, `rotation_speed`, `rotation_is_around_center`, `shot_dispersion_factors`, `hit_tester`, `armor`), `turret` (`gun_position`, `circular_vision_radius`, `rotation_speed`, `armor`, `primary_armor`, `turret_rotator_health`, `surveying_device_health`, `hit_tester`), `gun` (`burst`, `shot_dispersion_angle`, `shot_dispersion_factors`, `armor`, `hit_tester`, `effects_index`, `gun_health`), `engine` (`power`, `fire_starting_chance`, `max_health`), `fuel_tank` (`max_health`), `radio` (`distance`, `max_health`), per-component `max_regen_health` & `repair_cost`.
     - Parsed `OptionalDeviceDef` & `EquipmentDef` runtime modifiers from XML `<script>` blocks.
   - Fix `DiffBuilder::build` bug in `ops/mod.rs`: `veh_lock` updates must emit `cache['vehsLock']` diff entries (`Account.py:498-501`) rather than only `inventory[1]['lock']`.
   - Fix `store::damage_vehicle` / add `BattleOutcome` application API in `ops/battle.rs`:
     - Apply Random battle outcome (hull/component `repair` cost matching `vehicles.py:405-412`, deducted `shells` & `eqs`, `xp`, `free_xp`, `credits`, `tman_xp`, `last_battle_time`), trigger `AUTO_MAINTENANCE` (`REPAIR=1, LOAD_AMMO=2, EQUIP=3`) from depot then shop, update `AccountDossier` v19 / `VehicleDossier` v17 / `TankmanDossier` v10 + medals (`helpers.py` parity), unlock `LOCK_REASON::ON_ARENA` -> `0`, and return `DiffBuilder` deltas.

3. **`crates/wot-battle` (New Workspace Member — Pure World & Battle Simulation)**
   - `balance.rs`: Server balance config & fallback tables for tags stripped from retail XML (`normalizationAngle`, `ricochetAngle`, `armorHomogenization`, `terrainResistance` per class/tier, `brakeForce`, `healthBurnPerSec`, `healthRegenPerSec`, `hysteresisHealth`, `xpFactor`, `creditsFactor`, crew efficiency scaling), loaded cleanly with optional external JSON override (`--battle-config`).
   - `assets/`:
     - `bin_section.rs` & `bsp2.rs`: `BinSection` (`0x42a14e65`), `bsp2` (`b"BSP\0"`, pre-order node flags `0x01/0x02/0x04`), `bsp2_materials` XML, `vertices`/`indices`, bounding boxes, fast ray/segment intersection.
     - `terrain.rs`: ZIP `.cdata` parser -> `terrain2/heights` (`b"hmp\0"` + `b"gnpq"` 69x69 PNG RGBA `i32` LE mm, `VISIBLE_OFFSET = 2`, `(x0^z0)&1` triangle interpolation) + `terrain2/dominantTextures` (`b"mat\0"` + `b"\x00piz"` zlib `u8[16384]` indices -> `system/data/material_kinds.xml`).
     - `chunk.rs`: `arena_defs` (`_list_.xml`, `_common_.xml`, map XMLs), `space.settings`, `xxxxzzzz'o'` chunk coord decoding (`(gx*100, gz*100)`), `<model>`, `<speedtree>`, `<UserDataObject>SpawnPoint`, `<ControlPoint>` TeamBase zones, `scripts/destructibles.xml`.
     - `map_store.rs`: Lazy on-demand `Arc<ArenaMap>` cache + derived 2D `NavGrid` (heightmap slope `<= maxClimbAngle`, water depth, static obstacle footprint inflation) + Foliage canopy volumes.
   - `world/`: `BattleWorld`, `Arena` state machine (`WAITING` -> `PREBATTLE` -> `BATTLE` -> `AFTERBATTLE`), `QueueManager` (Random matchmaker using `vehicle.xml` `<balance>` weights + bot fill), `PrebattleManager` (Training rooms, solo Team-2 virtual bot, rosters, settings), `TeamBase` capture logic, `DestructiblesState` (`AreaDestructibles` chunk controllers).
   - `sim/`: `physics.rs` (WoT 0.6.5 driving, terrain resistance, slope limits, collisions & ramming), `ballistics.rs` (turret/gun aiming, dispersion, `computeProjectileTrajectory`, AP/APCR/HE BSP armor/internal hit testing, splash), `crits.rs` (modules, crew, fire, ammo rack, repairs, consumables, optdevs), `visibility.rs` (checkpoint LoS rays, foliage attenuation, camo, optics/binocs, 50m proxy, linger, `radio.distance` AoI), `bot_ai.rs` (`NavGrid` A* navigation, class roles, honest spotting, lead aiming, cap/decap, training passive toggle), `results.rs` (XP/credits/medals/tkill/`BATTLE_RESULTS_EXT`).

4. **`crates/wot-baseapp` (Session Routing, Battle Loop & Channel Adapter)**
   - Add `battle_host.rs` & extend `channel.rs`, `protocol.rs`, `registry.rs`:
     - Allocate unique non-colliding `EntityID`s (`1..`) per session/arena instead of hardcoded `player_id = 1`.
     - Wire `Account` Exposed BaseMethods `chatCommand (0)`, `inviteCommand (1)`, `ackCommand (2)`, `createTraining (9)`, and `CMD_PRB_* (400..412)`, `CMD_REQ_ARENA_LIST (500)`, `CMD_REQ_QUEUE_INFO (502)`, `CMD_REQ_PREBATTLES* (520..522)`, `CMD_ENQUEUE_FOR_ARENA (700)`, `CMD_DEQUEUE (701)`, `CMD_CREATE_ARENA (10006)`, `CMD_JOIN_ARENA (10007)`, `CMD_FORCE_QUEUE (10008)`.
     - Transition `Account <-> Avatar`: `RESET_ENTITIES(keepPlayerOnBase=false)` -> wait client `ENABLE_ENTITIES` -> `CREATE_BASE_PLAYER(Avatar)` -> `SPACE_DATA` (`weather=8192`, `ToD=0`, `mapping=1`) -> `CREATE_CELL_PLAYER(Avatar)` -> `ENTER_AOI` + (`REQUEST_ENTITY_UPDATE` -> `CREATE_ENTITY` for own `Vehicle`, visible `Vehicle`s, `AreaDestructibles`).
     - Run `100ms` (`SERVER_TICK_LENGTH = 0.1s`) deterministic battle tick driving `TICK_SYNC`, `AVATAR_UPDATE_*` unreliable position broadcasts, `Avatar` client RPCs (`updateOwnVehiclePosition`, `updateTargetingInfo`, `updateGunMarker`, `updateArena`, `updatePositions`, `showTracer`, `stopTracer`, `explodeProjectile`, `updateVehicleHealth/Ammo/Reload/Setting`, `showVehicleDamageInfo`, `onVehicleLeftArena`), `Vehicle` RPCs (`showShooting`, `showDamageFromShot/Explosion`, `showFireStartedMessage`, `onPushed`), `UPDATE_ENTITY` deltas (`health`, `isCrewActive`, `engineMode`, `damageStickers`, `publicStateModifiers`, `AreaDestructibles`), `onChatAction`, and `onKickedFromArena` / `leaveArena`.
     - Handle client disconnects/timeouts mid-battle cleanly (tank remains in arena controlled/parked, reconnect re-enters arena if still running, or post-battle results queue to persisted `AccountProfile`).

5. **`wot-server` (`src/main.rs`)**
   - Expose battle configuration flags on `wot-server serve` (and forward to `wot-baseapp`):
     - `--team-size <N>` (default `7`, valid `1..=15`)
     - `--queue-wait-secs <N>` (default `5`)
     - `--prebattle-countdown-secs <N>` (default `30`, `5` convenient in tests)
     - `--dev-features` (auto-enabled in `sandbox` preset)
     - `--battle-config <PATH>` (optional JSON override for stripped balance constants)

---

## 3. Exact Protocol & Binary Specifications

### 3.1 0-Based ClientTypeIDs (`entities.xml`)
`Account = 0`, `Avatar = 1`, `Vehicle = 5`, `AreaDestructibles = 7` (`Admin = 2`, `OfflineEntity = 11`; `Arena`, `ArenaMgr`, `Projectile`, `TeamBase`, `Prebattle` are server-only).

### 3.2 `Avatar` (`typeID = 1`) Wire Contract
- **Properties (`clientServerIndex`):**
  - `BASE_AND_CLIENT` (in `createBasePlayer`): `0: name (STRING)`, `1: arenaTypeID (INT32)`, `2: arenaGuiType (UINT8)`, `3: arenaExtraData (PYTHON)`, `4: weatherPresetID (UINT8)`.
  - `OWN_CLIENT` (in `createCellPlayer`): `5: team (UINT8)`, `6: playerVehicleID (OBJECT_ID / i32)`.
- **Exposed BaseMethods (`0xC0 | id`, no `entityID`):**
  `0: chatCommand`, `1: inviteCommand`, `2: ackCommand`, `3: vehicle_moveWith(u8)`, `4: vehicle_shoot()`, `5: vehicle_trackPointWithGun(Vec3)`, `6: vehicle_stopTrackingWithGun(f32, f32)`, `7: vehicle_changeSetting(u8, i32)`, `8: vehicle_teleport(Vec3, f32)`, `9: vehicle_useHorn(bool)`, `10: onClientReady()`, `11: leaveArena()`, `12: setDevelopmentFeature(String, i32)`, `13: addBotToArena(String, u8, String)`, `14: controlAnotherVehicle(i32, u8)`, `15: receiveFakeShot`.
- **Exposed CellMethods (`0x80 | id` + `i32 entityID`):** `0: autoAim(i32)`, `1: moveTo(Vec3)`, `2: bindToVehicle(i32, Vec3)`.
- **ClientMethods (`0x80 | id` + `i32 avatarID`):** `0: onChatAction`, `1: onKickedFromServer`, `2: onAutoAimVehicleLost`, `3: updateVehicleHealth`, `4: updateVehicleGunReloadTime`, `5: updateVehicleAmmo`, `6: updateVehicleSetting`, `7: updateTargetingInfo`, `8: updateGunMarker`, `9: updateOwnVehiclePosition`, `10: showVehicleDamageInfo`, `11: showDevelopmentInfo`, `12: showTracer`, `13: stopTracer`, `14: explodeProjectile`, `15: receiveHorn`, `16: onKickedFromArena`, `17: onVehicleLeftArena`, `18: updateArena`, `19: updatePositions`.

### 3.3 `Vehicle` (`typeID = 5`) & `AreaDestructibles` (`typeID = 7`) Wire Contract
- **`Vehicle` `ALL_CLIENTS` properties:**
  - `0: publicInfo` (`name:STRING, compDescr:STRING, team:UINT8, prebattleID:OBJECT_ID`)
  - `1: health` (`i16`)
  - `2: isCrewActive` (`u8`)
  - `3: engineMode` (**2 inline `u8` bytes**, `[power_mode, dir_flags]` — NOT a length-prefixed array!)
  - `4: damageStickers` (`ARRAY<u64>`, `i32 len` / `u32 len` + `u64` segments packed as `result | (compIdx<<8) | (startXYZ<<16) | (endXYZ<<40)`)
  - `5: publicStateModifiers` (`ARRAY<u8>`, extra IDs)
- **`Vehicle` Exposed CellMethods (`0x80 | id` + `i32 vehicleID`):** `0: moveWith(u8)`, `1: trackPointWithGun(Vec3)`, `2: stopTrackingWithGun(f32, f32)`, `3: changeSetting(u8, i32)`, `4: sendVisibilityDevelopmentInfo(i32, Vec3)`.
- **`Vehicle` ClientMethods (`0x80 | id` + `i32 vehicleID`):** `0: showShooting()`, `1: showDamageFromShot(i32, Vec<u64>, u8)`, `2: showDamageFromExplosion(i32, Vec3, u8)`, `3: showFireStartedMessage(i32)`, `4: onPushed(f32, f32)`.
- **`AreaDestructibles` `ALL_CLIENTS` properties:**
  - `0: destroyedModules (ARRAY<u16>)`: `(destrID << 8) | (matKind << 1) | isShot`
  - `1: destroyedFragiles (ARRAY<u8>)`: `itemIndex`
  - `2: fallenDestructibles (ARRAY<u16>)`: `(destrID << 8) | ((discreteYaw << 2) | (fallSpeed & 3))`
  - Chunk ID formula: `((gridX + 127) << 8) | (gridZ + 127)`, entity world position = `(gridX * 100 + 50, 0, gridZ * 100 + 50)`.

---

## 4. Platform Constraints & Invariants

1. **Windows Low Integrity Level (`0x1000`):** Never call `std::env::temp_dir()`; scratch/test/session/account files always reside inside `.scratch/`, `--log-dir`, `--data-dir`, or `target/test-tmp/`.
2. **Locked `.exe` Check & Sandbox Build Rule:** Terminate running `wot-server.exe`, `bw201-login.exe`, `wot-baseapp.exe` before building. Inside the sandbox, validate with `cargo check --workspace` / `cargo test --workspace`; ask the human to build `--release` outside the sandbox (`zerocopy` build-script limit inside sandbox).
3. **No Silent Fixture Auto-Loading:** `wot-server serve` loads assets directly from `--game-dir` (`res/`) and never silently pulls `.scratch/research/decompiled/_generated/*`.
4. **Atomic Persistence Before Notification:** Hangar/post-battle mutations commit atomically to `JsonAccountStore` before emitting `Account.update(diff)` or `SYS_MESSAGE_TYPE.battleResults`.
5. **Empirically Sensitive Client Edge Cases:**
   - Verify exact chunk destructible enumeration order (`wg_getChunkDestrFilenames`: document order vs type-grouped) against a live WoT 0.6.5 client during Plan B smoke testing so `destrIndex` aligns 1:1.
   - `BaseApp` sends `createBasePlayer(Avatar)` -> `spaceData` (weather `8192`, ToD `0`, mapping `1` with identity `4x4` matrix + `spaces/<map>`) -> `createCellPlayer` -> `enterAoI` + (`createEntity` on `requestEntityUpdate`) so `Avatar.__stepsTillInit == 4` reaches `0` and fires `onClientReady()`.

---

## 5. Phased Implementation Breakdown

Because the battle system spans multiple distinct subsystems, implementation is decomposed into three self-contained, testable plans — each leaving the workspace compiling, passing all automated tests, and delivering playable functionality:

1. **Part 1 — World Assets, Wire Protocol, Queue/Training & Battle Entry (`2026-11-14-wot065-battle-part1-world-and-entry.md`)**
   - `bw201` wire fixes & world/AoI/movement codecs
   - `wot065-data` battle XML attributes + `vehsLock` diff bugfix
   - `crates/wot-battle` `.cdata`/`.chunk`/`bsp2`/`destructibles.xml`/`arena_defs` parsers, `MapStore` cache, `NavGrid`
   - Matchmaker Queue (`700/701/502`) + Training Rooms (`createTraining`/`CMD_PRB_*`/solo Team-2 bot) + `Account <-> Avatar` entity switch & world spawn (`onClientReady` barrier).
2. **Part 2 — Physics, Ballistics, Crits, Destructibles, Spotting & Bot AI (`2026-11-14-wot065-battle-part2-combat-and-ai.md`)**
   - WoT 0.6.5 tank physics, obstacles, tank-tank collisions & ramming
   - Aiming/gun rotation, projectile parabola, AP/APCR/HE BSP armor penetration & splash
   - Module/crew/fire/ammo-rack crits, repairs, `ACTIVATE_EQUIPMENT`, `OptionalDevices`
   - `AreaDestructibles` state sync & tree felling
   - Spotting/camo/radio/AoI & `updatePositions`
   - Tactical 2D `NavGrid` Bot AI & `addBotToArena`.
3. **Part 3 — Capture, Chat, Postmortem/SPG, Economy/Dossiers & E2E (`2026-11-14-wot065-battle-part3-lifecycle-chat-economy.md`)**
   - `TeamBase` capture & win/timeout lifecycle
   - Battle Chat (`battleCommon`/`battleTeam`), Minimap/Radial commands, `bindToVehicle`/`moveTo`/`autoAim`, Friendly Fire & `TEAM_KILLER`
   - `BATTLE_RESULTS_EXT`, XP/Credits/Repair/Auto-Maintenance, `Account/Vehicle/Tankman` `updateDossier` + medals, `SYS_MESSAGE_TYPE.battleResults`, `LOCK_REASON.ON_ARENA`
   - CLI flags, `--dev-features` (`setDevelopmentFeature`, `CMD_CREATE_ARENA`), multi-client UDP E2E full-battle tests & live client smoke verification.
