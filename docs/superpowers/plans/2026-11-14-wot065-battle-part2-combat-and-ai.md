# WoT 0.6.5 Battle System Part 2: Physics, Ballistics, Crits, Destructibles, Spotting & Bot AI Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Implement the complete deterministic World of Tanks 0.6.5 combat simulation inside `crates/wot-battle` (tank driving physics, collisions/ramming, gun aiming/ballistics, BSP armor & HE splash, internal modules/crew/fire/consumables, `AreaDestructibles`, spotting/camo/radio/AoI, and tactical `NavGrid` Bot AI) and stream real-time combat packets through `crates/wot-baseapp`.

**Architecture:** Build pure simulation modules under `crates/wot-battle/src/sim/` driven at `SERVER_TICK_LENGTH = 0.1s` (with deterministic physics sub-steps if delta/speed warrants), producing typed `ArenaEvent`s (`EntityPoseDelta`, `TracerSpawned`, `ShotImpact`, `CritEvent`, `DestructibleBroken`, `AoIAdd`/`Remove`, `MinimapPositions`, etc.) that `wot-baseapp/src/battle_host.rs` translates into `bw201::world`, `Avatar`, `Vehicle`, and `AreaDestructibles` wire packets.

**Tech Stack:** Rust 2021 (`crates/wot-battle`, `crates/wot065-data`, `crates/bw201`, `crates/wot-baseapp`), `glam = "0.29"`, `rand = "0.8"`.

**Spec:** [2026-11-14-wot065-battle-system-design.md](file:///c:/Users/qwerty/Documents/GitHub/interstellar/docs/superpowers/specs/2026-11-14-wot065-battle-system-design.md)

## Global Constraints

1. **Windows Low Integrity Level (`0x1000`):** Never use `std::env::temp_dir()`; use `target/test-tmp/` for tests.
2. **Locked `.exe` Guard & Sandbox Rule:** Stop running server `.exe`s before `cargo test --workspace` / `cargo check --workspace`.
3. **WoT 0.6.5 Exact Wire Contracts:**
   - `Vehicle.engineMode` updates are 2 inline `u8` bytes `[power_mode, dir_flags]`.
   - `_decodeSegment` u64 layout: `result | (compIdx<<8) | (startXYZ<<16) | (endXYZ<<40)` (`0=chassis, 1=hull, 2=turret, 3=gun`).
   - `Avatar.updateTargetingInfo` takes exact 6 floats: `(turretYaw, gunPitch, maxTurretRotSpeed, maxGunRotSpeed, shotDispMultFactor, aimingTime)`.
   - `Avatar.updatePositions` sends `indices` indexed into `sorted(vehicles.keys())` and `positions` `[x0, z0, x1, z1, ...]` as `i16`.
   - `AreaDestructibles` bit-packing: `chunkID = ((gx + 127) << 8) | (gz + 127)`; fallen `(destrID<<8) | ((yaw64<<2) | fallSpeed)`.
4. **Deterministic Seeded PRNG:** Use seeded `rand` RNG per arena (from arena seed in tests / timestamp seed in live runs) so combat trajectories, penetration rolls (`±25%`), and fires are reproducible in unit tests.

## Review Focus

1. **`Grousers` & `dominantTextures` Resistance Mapping:** `Grousers` modifies soft/medium terrain resistance while chassis uses `[firm, medium, soft]`; verify `material_kinds.xml` `<ground_strength>` maps `"firm" -> 0, "medium" -> 1, "soft" -> 2` cleanly — pinned by Task 1 driving tests.
2. **`3x` Overmatch vs `>= 70°` Auto-Ricochet Precedence:** AP/APCR overmatch (`caliber > 3 * nominal_armor`) MUST bypass the `>= 70°` ricochet check while `caliber > 2 * nominal_armor` scales normalization before check — pinned by Task 2 armor angle tests.
3. **Vertical Internal Module Ray Expansion & Broken Track Immobilization:** `showVehicleDamageInfo` MUST report distinct `DAMAGE_INFO_CODES` (`0..=25`), destroyed tracks MUST zero linear speed until repaired, and inactive crew / ammo-rack detonation MUST kill cleanly — pinned by Task 3 crits/consumables tests.
4. **`wg_getChunkDestrFilenames` Chunk Ordering Sensitivity:** Note for live client verification that client `AreaDestructibles` expects the exact per-chunk destructible enumeration order used by `WorldOfTanks.exe`; Task 4 isolates chunk indexing behind `ChunkDestructibleIndex` so any ordering adjustment touches one table lookup — pinned by bit-encoding & foliage-removal spotting tests in Task 4.
5. **AoI Radio Relay & `enterAoI` Before `createEntity` Ordering:** Enemies out of direct vision but within allied `radio.distance` remain hidden in world AoI (no `Vehicle` `enterAoI` when outside draw distance/unspotted) or enter world AoI only when spotted, while minimap receives `updatePositions` — pinned by Task 4 spotting/AoI tests and Task 5 UDP E2E combat test.

---

### Task 1: Tank Driving Physics, Terrain Resistance, Slope Limits & Tank Collisions/Ramming

**Files:**
- Create: `crates/wot-battle/src/sim/mod.rs`
- Create: `crates/wot-battle/src/sim/physics.rs`
- Modify: `crates/wot-battle/src/world/arena.rs`
- Modify: `crates/wot-battle/src/lib.rs`

**Interfaces:**
- Consumes: `ArenaMap`, `VehicleCollisionSet`, `VehicleDef`, `VehicleDescr`, `BalanceConfig`
- Produces:
  - `VehiclePhysicsState { pos: Vec3, yaw: f32, pitch: f32, roll: f32, speed_mps: f32, rot_speed_rps: f32, move_flags: u8, cruise_mode: i8, engine_mode: [u8; 2], left_track_broken: bool, right_track_broken: bool }`
  - `step_vehicle_driving(veh: &mut VehicleSimState, all_vehicles: &[VehicleSnapshot], map: &ArenaMap, dt: f32) -> DrivingStepResult` (yields obstacle/ramming impulses, `on_pushed: Option<(f32, f32)>`, ramming damage pairs, broken destructible intersections)

- [ ] **Step 1: Write failing tests for driving acceleration, slope blocking, terrain resistance, broken tracks, and ramming**
  Tests in `crates/wot-battle/src/sim/physics.rs`:
  - `respects_speed_limits_terrain_resistance_and_max_climb_angle`: tank accelerates proportional to `engine.power / weight` up to `speedLimits`, slows on `soft` vs `firm` terrain (`dominantTextures`), aligns `pitch`/`roll` to terrain normal, stops climbing above `maxClimbAngle`, and cannot move forward when `left_track_broken == true`.
  - `tank_tank_collision_and_ramming_damage`: heavy fast tank colliding with lighter stationary tank resolves overlap without tunneling, deals speed-and-mass-scaled ramming damage (`> 0`) to both weighted inversely by mass, triggers `on_pushed` when displacement exceeds client filter threshold, and emits obstacle stop against static `StaticObstacle` BSP.

- [ ] **Step 2: Run tests to verify failure**
  Run: `cargo test -p wot-battle sim::physics`
  Expected: FAIL (`sim::physics` module missing).

- [ ] **Step 3: Implement `crates/wot-battle/src/sim/physics.rs`**
  - Decode `vehicle_moveWith(flags)` bits (`1=fwd, 2=back, 4=left, 8=right, 16=cruise50, 32=cruise25`, `engineMode = [power_mode, dir_flags]`).
  - Compute traction, drag, braking, and WoT 0.6.5 turn rates (`vehicles.py:711-722`: `rotationEnergy` adjusted by `terrainResistance`, `rotationIsAroundCenter`, engine HP scaling, driver/damaged-engine penalty).
  - Resolve swept/iterative horizontal sphere/OBB pushout against static `StaticObstacle` BSPs and neighbor vehicles, compute kinetic ramming damage (`0.5 * reduced_mass * rel_speed^2 * ram_coeff` with `antifragmentationLining` reduction), and re-project onto `ArenaMap::sample_height_and_normal(x, z)` blocking slopes steeper than `cos(radians(maxClimbAngle))`.

- [ ] **Step 4: Run tests to verify pass**
  Run: `cargo test -p wot-battle sim::physics`
  Expected: PASS (`0` failures).

- [ ] **Step 5: Commit**
  ```bash
  git add crates/wot-battle/src/sim/ crates/wot-battle/src/world/arena.rs crates/wot-battle/src/lib.rs
  git commit -m "feat(wot-battle): implement WoT 0.6.5 driving physics, terrain alignment, and ramming"
  ```

---

### Task 2: Turret/Gun Rotation, Dispersion, Ballistics & AP/APCR/HE BSP Armor Penetration

**Files:**
- Create: `crates/wot-battle/src/sim/ballistics.rs`
- Modify: `crates/wot-battle/src/sim/mod.rs`
- Modify: `crates/wot-battle/src/world/arena.rs`

**Interfaces:**
- Consumes: `VehicleCollisionCache`, `BspTree`, `ShotDef`, `ShellXmlDef`, `ArenaMap`, `BalanceConfig`
- Produces:
  - `AimState { turret_yaw: f32, gun_pitch: f32, target_point: Vec3, current_disp_factor: f32, reload_left: f32, current_shell_cd: i32, next_shell_cd: i32, autorotation: bool, auto_aim_target: Option<i32> }`
  - `step_gun_aiming(...) -> TargetingSnapshot` (exact 6 `updateTargetingInfo` floats + `updateGunMarker` origin/dir/dispersionAngle)
  - `spawn_shot(...) -> ProjectileState` + `step_projectiles(..., rng) -> Vec<ImpactOutcome>` (`ShotImpact` with `u64` encoded hit segment matching `_decodeSegment`, `SHOT_RESULT`, `armor_damage`, `splash_hits`, `effect_mat_idx`, internal continuation ray)
  - `encode_hit_segment(result: u8, comp_idx: u8, start_local: Vec3, end_local: Vec3, bbox: (Vec3, Vec3)) -> u64`

- [ ] **Step 1: Write failing tests for dispersion, trajectory parabola, armor angles (2x/3x overmatch, 70° ricochet), HE splash, and `_decodeSegment` bitpacking**
  Tests in `crates/wot-battle/src/sim/ballistics.rs`:
  - `segment_encoding_roundtrips_with_vehicle_decode_segment`: `encode_hit_segment` decodes identically to `Vehicle._decodeSegment` (`result & 0xff`, `compIdx`, quantized XYZ).
  - `ap_normalization_ricochet_and_overmatch_rules`:
    - `75mm` AP vs `50mm` armor at `72°`: bounces (`SHOT_RESULT::RICOCHET`, `caliber < 3 * 50`, `>= 70°`).
    - `152mm` AP vs `45mm` armor at `75°`: overmatches (`152 > 3 * 45`), bypasses ricochet, calculates `cos` thickness with `2x` boosted normalization, penetrates (`SHOT_RESULT::ARMOR_PIERCED`).
    - `HE` shell hits thick unpenetrated armor (`300mm`) and still deals splash damage attenuated by distance within `explosionRadius` and nominal armor thickness.
  - `parabola_subdivision_and_dispersion_aim_shrink`: computes projectile arc (`0.8` speed / `0.64` gravity) hitting ground below horizontal line-of-sight at 400m; aim factor shrinks exponentially per `aimingTime` until reaching `idealFactor`.

- [ ] **Step 2: Run tests to verify failure**
  Run: `cargo test -p wot-battle sim::ballistics`
  Expected: FAIL (`sim::ballistics` not implemented).

- [ ] **Step 3: Implement `crates/wot-battle/src/sim/ballistics.rs`**
  - Implement turret/gun inverse matrix chain (`chassis` -> `hull` -> `turret` -> `gun` matching `Vehicle.py:257-302`) to convert world aim points to relative `(turretYaw, gunPitch)` respecting `yawLimits` and `pitchLimits` (body hull turn assist if `autorotation`).
  - Implement adaptive parabola subdivision (`projectile_trajectory.py:8-35`, `SERVER_EPSILON = 0.1`), raycast terrain + static BSPs + destructibles + all alive/dead vehicle BSP components along each segment.
  - Compute `100m..500m` linear pen attenuation (`control_modes.py:1689-1702`), `±25%` RNG rolls (`pen` and `damage`), normalization (`AP=5°, APCR=2°, HE=0°`, `2x` boost when `caliber > 2 * armor`), ricochet (`>= 70°` unless `caliber > 3 * armor` or `HE`), effective thickness `armor / cos(eff_angle)`, internal ray continuation (`10 * caliber` depth inside penetrated hull/turret for Task 3 module/crew hits), and HE splash radius falloff (`0.5 * damage * (1 - dist/R) - 1.1 * min_armor * spall`).

- [ ] **Step 4: Run tests to verify pass**
  Run: `cargo test -p wot-battle sim::ballistics`
  Expected: PASS (`0` failures).

- [ ] **Step 5: Commit**
  ```bash
  git add crates/wot-battle/src/sim/ballistics.rs crates/wot-battle/src/sim/mod.rs crates/wot-battle/src/world/arena.rs
  git commit -m "feat(wot-battle): implement turret aiming, dispersion, ballistics, and AP/APCR/HE BSP armor"
  ```

---

### Task 3: Internal Modules, Crew Crits, Fire, Ammo Rack, Consumables & Optional Devices

**Files:**
- Create: `crates/wot-battle/src/sim/crits.rs`
- Modify: `crates/wot-battle/src/sim/mod.rs`
- Modify: `crates/wot-battle/src/sim/ballistics.rs`
- Modify: `crates/wot-battle/src/sim/physics.rs`

**Interfaces:**
- Consumes: `ImpactOutcome`, `EquipmentDef`, `OptionalDeviceDef`, `TankmanDescr`, `BalanceConfig`
- Produces:
  - `VehicleDamageState` (all 9 devices `engine..surveyingDevice` health + state `Normal`/`Damaged`/`Destroyed`, all crew members health + dead state, `fire_state: Option<ActiveFire>`, repair progress timers, cooldowns for up to 3 `EquipmentSlot`s, passive modifier aggregate `EffectiveModifiers`)
  - `apply_internal_crit_ray(...) -> Vec<DamageInfoEvent>`
  - `activate_equipment(veh: &mut VehicleSimState, eq_id: u16, extra_arg: u16) -> Result<(), &'static str>`
  - `compute_effective_modifiers(veh_descr: &VehicleDescr, crew: &[TankmanDescr], dmg: &VehicleDamageState) -> EffectiveModifiers` (affects reload, aim, dispersion, engine power, view range, camo, repair speed)

- [ ] **Step 1: Write failing tests for module/crew crits, fire ignition/extinguish, ammo-rack detonation, crew knock-out, and consumables/optdevs**
  Tests in `crates/wot-battle/src/sim/crits.rs`:
  - `rammer_ventilation_optics_modify_reload_and_vision`: `tankRammer` multiplies reload by `0.9`, `improvedVentilation` increases effective crew skill by `+5%`, `coatedOptics` multiplies view range by `1.1`.
  - `track_destruction_and_repair_kit`: breaking `leftTrackHealth <= 0` emits `DEVICE_DESTROYED_AT_SHOT` and immobilizes tank; activating `Repairkit` (`ACTIVATE_EQUIPMENT`) restores tracks, removes consumable quantity via `updateVehicleAmmo`, and allows movement immediately; `Small/Large` auto-repair timer ticks with `Toolbox` (`1.25x`).
  - `engine_fire_burn_and_extinguisher`: hitting fuel tank (`health == 0`) or engine critical roll starts fire (`DEVICE_STARTED_FIRE_AT_SHOT` + periodic HP burn `healthBurnPerSec`), stops on `Extinguisher` (`FIRE_STOPPED`) or `autoExtinguishers`.
  - `ammo_rack_detonation_and_crew_knockout`: zeroing `ammoBay` triggers instant kill (`DEATH_FROM_DEVICE_EXPLOSION_AT_SHOT`, health=0); knocking out all living crew triggers `DEATH_FROM_INACTIVE_CREW_AT_SHOT` (`isCrewActive = false`).

- [ ] **Step 2: Run tests to verify failure**
  Run: `cargo test -p wot-battle sim::crits`
  Expected: FAIL (`sim::crits` module missing).

- [ ] **Step 3: Implement `crates/wot-battle/src/sim/crits.rs` and connect modifiers to `physics`/`ballistics`**
  - Intersect internal continuation ray against non-armor BSP materials (`fuelTank`, `engine`, `ammoBay`, `radio`, `driver`, `commander`, `gunner_*`, `loader_*`, `radioman_*`, `turretRotator`, `surveyingDevice`, `leftTrack`, `rightTrack`, `gun`).
  - Roll WoT 0.6.5 saving throws (`module hit chance`, crew hit chance), update module `orange`/`red` states (`>= maxRegenHealth` / `<= 0`), track auto-repair queue (`leftTrack` + `rightTrack` + `gun` + `engine` + `ammoBay` + `turretRotator` + `radio` + `surveyingDevice` + `fuelTank`), emit `DAMAGE_INFO_CODES` (`0..=25`) and `Vehicle.publicStateModifiers`.

- [ ] **Step 4: Run tests to verify pass**
  Run: `cargo test -p wot-battle`
  Expected: PASS (`0` failures).

- [ ] **Step 5: Commit**
  ```bash
  git add crates/wot-battle/src/sim/
  git commit -m "feat(wot-battle): implement module/crew crits, fires, ammo rack, consumables, and optdevs"
  ```

---

### Task 4: `AreaDestructibles` Sync & Full Visibility/Camo/Radio/AoI System

**Files:**
- Create: `crates/wot-battle/src/sim/destructibles.rs`
- Create: `crates/wot-battle/src/sim/visibility.rs`
- Modify: `crates/wot-battle/src/sim/mod.rs`
- Modify: `crates/wot-battle/src/world/arena.rs`

**Interfaces:**
- Consumes: `DestructiblesCatalog`, `ArenaMap`, `VehicleSimState`, `EffectiveModifiers`
- Produces:
  - `ChunkDestructibleIndex` & `DestructiblesState` (`break_by_ramming(...) -> Vec<DestructibleDelta>`, `break_by_shot(...) -> Option<DestructibleDelta>`, `tree_is_standing(chunk_id, destr_id) -> bool`)
  - Exact `u16`/`u8` bitpackers matching `DestructiblesCache.py:327-389` (`encode_module`, `encode_fallen`)
  - `VisibilityTracker` (`step_visibility(&mut self, vehicles: &[VehicleSimState], map: &ArenaMap, destr: &DestructiblesState, dt: f32) -> VisibilityOutput` yielding per-observer `Vec<AoIChange>` (`Enter(veh_id)`, `Leave(veh_id)`), spotted credit events for first-spot bonuses, and `MinimapPositionFrame { indices: Vec<u8>, positions_xz: Vec<i16> }`)

- [ ] **Step 1: Write failing tests for `AreaDestructibles` bit-packing, tree-felling foliage loss, camo/optics/bushes, 50m proxy & radio AoI**
  Tests in `sim::destructibles` & `sim::visibility`:
  - `destructible_bitpack_matches_client_decoders`: verifies `chunk_id = ((gx+127)<<8)|(gz+127)`, `fallen` yaw discretization `int(64*(yaw+PI)/2PI)%64`, and `module` encoding round-trip cleanly; tank ramming breaks tree when `instant_damage > ceil(scale^2 * ref_health)` and removes its foliage concealment!
  - `visibility_raycast_camo_bush_and_50m_proxy`:
    - Tank behind solid building BSP at 150m stays hidden; tank within `<= 50m` behind same building IS proxy-spotted.
    - Tank sitting stationary in bush/tree with `camouflageNet` spots enemy far sooner than enemy spots it; firing (`invisibilityFactorAtShot`) drops its camo immediately and reveals it; driving over that tree breaks it (`AreaDestructibles`) and removes the bush bonus!
    - Ally within `radio.distance` shares spotted enemy (`updatePositions` minimap updates emitted cleanly), whereas ally outside radio link receives no minimap update.

- [ ] **Step 2: Run tests to verify failure**
  Run: `cargo test -p wot-battle sim::destructibles sim::visibility`
  Expected: FAIL (`destructibles` / `visibility` modules missing).

- [ ] **Step 3: Implement `destructibles.rs` and `visibility.rs`**
  - Track standing/fallen/broken destructibles per `chunkID`, emitting `AreaDestructibles` delta updates (`destroyedModules`, `destroyedFragiles`, `fallenDestructibles`).
  - Cast 6-checkpoint LoS rays (gun mantlet/cupola -> target hull front/rear/left/right/top/bottom) through terrain + static BSPs + standing `FoliageVolume`s (15m transparent behind observer when firing check applies; cumulative camo reduction up to cap `0.8` when opaque foliage sits along ray); apply linger timer (`5..10s`), `radio.distance` graph BFS per team, per-player `AoI` enter/leave deltas, and sorted `updatePositions` `(indices, [x, z])`.

- [ ] **Step 4: Run tests to verify pass**
  Run: `cargo test -p wot-battle`
  Expected: PASS (`0` failures).

- [ ] **Step 5: Commit**
  ```bash
  git add crates/wot-battle/src/sim/ crates/wot-battle/src/world/arena.rs
  git commit -m "feat(wot-battle): implement AreaDestructibles sync and WoT 0.6.5 spotting/camo/radio AoI"
  ```

---

### Task 5: Tactical Bot AI, `addBotToArena`, & BaseApp Combat Loop Integration + UDP E2E

**Files:**
- Create: `crates/wot-battle/src/sim/bot_ai.rs`
- Modify: `crates/wot-battle/src/sim/mod.rs`
- Modify: `crates/wot-battle/src/world/arena.rs`
- Modify: `crates/wot-baseapp/src/battle_host.rs`
- Modify: `crates/wot-baseapp/src/protocol.rs`
- Create: `crates/wot-baseapp/tests/e2e_battle_combat.rs`

**Interfaces:**
- Consumes: `NavGrid`, `VisibilityTracker`, `AimState`, `VehiclePhysicsState`, `ArenaInstance`
- Produces:
  - `BotController` & `BotBehaviorMode { Tactical, PassiveTarget }` (`step_bots(&mut self, arena: &ArenaReadView, dt: f32) -> Vec<(i32, BotIntent)>` emitting movement flags, aim world point, shoot, shell swap, consumable activation)
  - Full `BattleHost` tick bridge dispatching `TICK_SYNC`, `S2CAvatarUpdate`, `Avatar` RPCs (`updateOwnVehiclePosition`, `updateTargetingInfo`, `updateGunMarker`, `showTracer`, `stopTracer`, `explodeProjectile`, `updateVehicleHealth/Ammo/Reload/Setting`, `showVehicleDamageInfo`, `updatePositions`), `Vehicle` RPCs (`showShooting`, `showDamageFromShot/Explosion`, `showFireStartedMessage`, `onPushed`) + `UPDATE_ENTITY` property deltas, `AreaDestructibles` AoI + `UPDATE_ENTITY`, and handling `Avatar.base.{vehicle_moveWith, vehicle_shoot, vehicle_trackPointWithGun, vehicle_stopTrackingWithGun, vehicle_changeSetting, addBotToArena}` and `Vehicle.cell.*`.

- [ ] **Step 1: Write failing tests for Bot AI (`wot-battle`) and live UDP combat E2E (`e2e_battle_combat.rs`)**
  - `wot-battle`: `bots_navigate_map_spot_fairly_and_kill_enemies`: runs a headless 3v3 simulated match on a terrain+obstacle arena (or Himmelsdorf when retail game dir present) for simulated ticks verifying bots steer along `NavGrid` around obstacles, hold fire until enemy is spotted, lead moving targets, break destructibles/tracks, deal HP damage, and respect `PassiveTarget` training mode.
  - `e2e_battle_combat.rs`: Two real UDP client sessions enter the same arena -> Client A sends `vehicle_trackPointWithGun` + `vehicle_shoot()` (`0xC4`) toward Client B -> verifies Client B receives `ENTER_AOI` + `CREATE_ENTITY(Vehicle)`, `Vehicle.showShooting()`, `Avatar.showTracer()`, `Vehicle.showDamageFromShot()`, `UPDATE_ENTITY(health, damageStickers)`, and Client A receives `updateVehicleAmmo` / `updateVehicleGunReloadTime` / `updateTargetingInfo`.

- [ ] **Step 2: Run tests to verify failure**
  Run: `cargo test -p wot-battle sim::bot_ai && cargo test -p wot-baseapp --test e2e_battle_combat`
  Expected: FAIL (`bot_ai` / combat wire dispatch not implemented).

- [ ] **Step 3: Implement `bot_ai.rs` and wire full combat replication in `crates/wot-baseapp/src/battle_host.rs`**
  - Implement tactical `BotController` selecting class-weighted corridors (`NavGrid::find_path`), flanking/retreating when HP low, defending friendly `TeamBase` if being capped, capturing enemy base when uncontested, angling hull armor ~30°, selecting HE on impenetrable targets, popping Repairkit on broken track/ammo-rack & Extinguisher on fire, and respecting `stop_bot` / `PassiveTarget` training toggle.
  - Wire all `Avatar` Base (`0..=15`) & `Vehicle` Cell (`0..=4`) gameplay RPCs into `battle_host.rs` and emit batched `UPDATE_ENTITY` / unreliable `AVATAR_UPDATE_*` + `TICK_SYNC` frames each 100ms tick.
  - Handle dynamic `addBotToArena(compDescr, team, name)` by validating compact descr, spawning a bot `Vehicle`, and emitting `ARENA_UPDATE.VEHICLE_ADDED` (`2`) + AoI if spotted.

- [ ] **Step 4: Run tests to verify pass**
  Run: `cargo test --workspace`
  Expected: PASS (`0` failures).

- [ ] **Step 5: Commit**
  ```bash
  git add crates/wot-battle/ crates/wot-baseapp/
  git commit -m "feat(battle): wire full physics, ballistics, crits, destructibles, spotting, and bot AI"
  ```
