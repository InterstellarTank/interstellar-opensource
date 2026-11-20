# WoT 0.6.5 Battle System Part 3: Capture, Chat, Postmortem/SPG, Economy/Dossiers & E2E Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Complete the World of Tanks 0.6.5 battle lifecycle with `TeamBase` capture & win/timeout rules, battle chat & minimap/radial commands, SPG/postmortem AoI camera switching, friendly fire & `TEAM_KILLER`, `BATTLE_RESULTS_EXT` + XP/credits/repair/auto-maintenance + `Account`/`Vehicle`/`Tankman` `updateDossier` + `LOCK_REASON.ON_ARENA`, `--dev-features` / CLI battle flags, and full UDP multi-client + persistence E2E verification.

**Architecture:** Extend `crates/wot-battle` with base capture/end-game scoring, battle chat routing, TK accounting, and `BattleOutcome` / `BATTLE_RESULTS_EXT` calculators; extend `crates/wot065-data` with atomic post-battle `AccountProfile` mutation (`apply_battle_outcome`, auto-maintenance, and `update_dossier` matching `common/dossiers/helpers.py`); wire background arena completion rewards (both online push via `Account.update` + `resyncDossiers` + `personalSysMessage` and offline pending persistence) into `crates/wot-baseapp`; and expose CLI battle flags in `wot-server`.

**Tech Stack:** Rust 2021 (`crates/wot065-data`, `crates/wot-battle`, `crates/wot-baseapp`, `wot-server`), `clap = "4"`, `tokio`, `serde`/`serde_json`.

**Spec:** [2026-11-14-wot065-battle-system-design.md](file:///c:/Users/qwerty/Documents/GitHub/interstellar/docs/superpowers/specs/2026-11-14-wot065-battle-system-design.md)

## Global Constraints

1. **Windows Low Integrity Level (`0x1000`):** Never call `std::env::temp_dir()`; store test artifacts under `target/test-tmp/` and runtime accounts/logs/sessions under `--data-dir` / `--log-dir` / `.scratch/`.
2. **Locked `.exe` Guard & Sandbox Rule:** Stop running server processes before compiling/testing; inside the sandbox validate via `cargo check --workspace` and `cargo test --workspace` (request human `--release` build outside sandbox before live game client launch).
3. **Atomic Persistence Before Client Notification:** Any post-battle credit/XP/repair/ammo/dossier/lock mutation MUST persist atomically to `JsonAccountStore` before emitting `Account.update(diff)`, `resyncDossiers()`, or `onChatAction(SYS_MESSAGE_TYPE.battleResults)`.
4. **Dossier & Bonus Type Fidelity (`common/dossiers/helpers.py`):**
   - `bonusType` non-regular (e.g. Training `0`) updates ONLY `battleLifeTime`, skipping all kills/shots/wins/medals (`Account`/`Vehicle`/`Tankman` stay untouched otherwise).
   - `survivedBattles` checks `killerID == 0` on `Account`/`Vehicle`, and `isActive` (`isCrewActive`) on `Tankman`.
   - Random `LOCK_REASON.ON_ARENA` (`16`) MUST emit `cache['vehsLock']` diff and stay set on early hangar exit until the arena enters `AFTERBATTLE`.
5. **Dev Feature Gate:** `setDevelopmentFeature("server_marker", ...)` is ALWAYS active; cheat features (`heal`, `reload_gun`, `start_fire`, `break_*_track`, `stop_bot`, `destroy_self`, `CMD_CREATE_ARENA`, `CMD_JOIN_ARENA`) require `--dev-features` or `sandbox` preset.

## Review Focus

1. **Early Hangar Exit While Arena Still Active:** When a killed player leaves early, `LOCK_REASON.ON_ARENA` locks the tank in Hangar until the arena finishes in the background, then unlocks (`0`), applies outcome + auto-maintenance + dossier diff, and delivers the service channel card whether the player stayed online in Hangar or disconnected and re-logged — pinned by Task 3 & Task 4 tests.
2. **Capture Progress & Reset Edge Cases:** Max 3 invaders contribute points (`maxPointsPerSecond`), dealing any damage to a capper drops THAT capper's personal cap contribution to `0` (`droppedCapturePoints`) without erasing other cappers' points — pinned by Task 1 capture tests.
3. **Exact `BATTLE_RESULTS_EXT` 38-Field Serialization:** Field order and primitive widths (`UINT64 arenaUniqueID`, `INT8 isWinner`, `BOOL bonusType`, nested `TKILL_VICTIM`) MUST match `alias.xml.xml:299-442` byte-for-byte — pinned by Task 3 wire test.
4. **Auto-Maintenance Asset Shortfall (`AUTO_MAINTENANCE_RESULT::NOT_ENOUGH_ASSETS`):** When a player lacks credits for full auto-repair or auto-load after a loss, partial depot stock still loads and `autoMaintenance` service notice reports shortfall cleanly without panicking or going negative credits — pinned by Task 3 tests.
5. **Training Room Zero-Side-Effect Isolation:** Training Room kills, shots, damage, TK shots, and consumed shells MUST leave `AccountProfile` credits, XP, depot ammo/eqs, repair cost, `vehsLock`, and all 3 dossiers untouched — pinned by Task 4 UDP E2E test.

---

### Task 1: `TeamBase` Capture Mechanics & `ARENA_UPDATE` Match End Lifecycle

**Files:**
- Create: `crates/wot-battle/src/sim/capture.rs`
- Modify: `crates/wot-battle/src/sim/mod.rs`
- Modify: `crates/wot-battle/src/world/arena.rs`

**Interfaces:**
- Consumes: `TeamBaseSpec`, `VehicleSimState`, `ImpactOutcome`
- Produces:
  - `BaseCaptureTracker` (`step_capture(&mut self, vehicles: &[VehicleSimState], damage_hits: &[(i32, i32)], dt: f32) -> Vec<BaseCaptureEvent>`, `TeamBaseStatus`)
  - `ArenaLifecycleUpdate` handling `WAITING -> PREBATTLE -> BATTLE -> AFTERBATTLE`, `FINISH_REASON::{EXTERMINATION=1, BASE=2, TIMEOUT=3}`, emitting `ARENA_UPDATE::{PERIOD(3), STATISTICS(4), VEHICLE_STATISTICS(5), VEHICLE_KILLED(6), BASE_POINTS(8), BASE_CAPTURED(9)}`

- [ ] **Step 1: Write failing tests for capture speed cap (3 tanks), decap reset, extermination win, and timeout**
  Tests in `crates/wot-battle/src/sim/capture.rs`:
  - `three_tanks_cap_at_max_points_per_second_and_damage_drops_attacker_contribution`: 1 tank on enemy base accumulates `1.0 pt/s`; 4 tanks accumulate `3.0 pt/s` (`maxPointsPerSecond`); hitting tank #1 drops only tank #1's accumulated cap share (`droppedCapturePoints`) while tanks #2–#4 keep theirs; reaching `100.0` triggers `FINISH_REASON::BASE`.
  - `extermination_and_timeout_end_arena_once`: killing last alive enemy transitions `BATTLE -> AFTERBATTLE` with `winnerTeam` and `FINISH_REASON::EXTERMINATION`; expiring `roundLength` transitions with draw (`0`, `TIMEOUT`).

- [ ] **Step 2: Run tests to verify failure**
  Run: `cargo test -p wot-battle sim::capture`
  Expected: FAIL (`sim::capture` module missing).

- [ ] **Step 3: Implement `crates/wot-battle/src/sim/capture.rs` & finish logic in `world/arena.rs`**
  - Track per-base `invaders: BTreeMap<i32, f32>` clamped so total effective contribution per tick <= `max_points_per_second`, emit rounded integer `BASE_POINTS` deltas when changed and `BASE_CAPTURED` at `100`.
  - Compute live `isWinner`, record `capturePoints`/`droppedCapturePoints`/`frags`/`lifeTime`, transition period to `AFTERBATTLE` (`kickAfterFinishWaitTime` countdown), and trigger `ArenaFinished` callback for Task 3 persistence.

- [ ] **Step 4: Run tests to verify pass**
  Run: `cargo test -p wot-battle`
  Expected: PASS (`0` failures).

- [ ] **Step 5: Commit**
  ```bash
  git add crates/wot-battle/src/sim/capture.rs crates/wot-battle/src/sim/mod.rs crates/wot-battle/src/world/arena.rs
  git commit -m "feat(wot-battle): implement TeamBase capture, reset, and arena win/timeout lifecycle"
  ```

---

### Task 2: Battle Chat, Minimap/Radial Commands, Postmortem/SPG AoI & Friendly Fire

**Files:**
- Create: `crates/wot-battle/src/sim/chat.rs`
- Create: `crates/wot-battle/src/sim/tkill.rs`
- Modify: `crates/wot-battle/src/sim/mod.rs`
- Modify: `crates/wot-battle/src/world/arena.rs`
- Modify: `crates/wot-baseapp/src/battle_host.rs`
- Modify: `crates/wot-baseapp/src/protocol.rs`

**Interfaces:**
- Consumes: `Avatar` Base (`chatCommand=0`, `controlAnotherVehicle=14`, `vehicle_useHorn=9`) & Cell (`autoAim=0`, `moveTo=1`, `bindToVehicle=2`), `ImpactOutcome`
- Produces:
  - `BattleChatRouter` managing per-arena `battle_common_channel_id` (`flags=2`) & `battle_team_channel_ids` (`flags=3`), generating `CHAT_ACTION_DATA` pickles for `broadcast(47)`, `ATTENTIONTOCELL(28)`, `ATTACKENEMY(29)`, radial `22..=27`, and `requestChannels`
  - `ObserverFocusState` handling `moveTo`, `bindToVehicle`, `controlAnotherVehicle` (`step = 1|2`), `autoAim` (`onAutoAimVehicleLost`)
  - `TkillTracker` calculating `tkillRating`, friendly-fire penalties (`creditsPenalty`, `xpPenalty`, `creditsContribution`), and `ARENA_UPDATE::TEAM_KILLER (10)` in Random battles (bypassed in Training rooms)

- [ ] **Step 1: Write failing tests for chat/minimap broadcast, postmortem AoI switch, auto-aim loss, and friendly fire TK flag**
  - `wot-battle` unit tests:
    - `chat_team_channel_isolates_teams_while_common_reaches_all`: team message and `ATTENTIONTOCELL` (`row*10 + col`) target only same-team clients; common broadcast hits both teams.
    - `postmortem_bind_and_spg_move_to_shift_observer_aoi`: moving camera via `moveTo` or binding to allied vehicle via `bindToVehicle` updates observer position used by `VisibilityTracker` / AoI filtering; auto-aim drops with `KILLED`/`INVISIBLE` reasons.
    - `friendly_fire_triggers_team_killer_in_random_only`: repeated friendly damage flags `isTeamKiller = true` (`ARENA_UPDATE::TEAM_KILLER`) in Random, while Training Room inflicts HP damage with zero TK flag/penalty.

- [ ] **Step 2: Run tests to verify failure**
  Run: `cargo test -p wot-battle sim::chat sim::tkill`
  Expected: FAIL (`chat` / `tkill` modules missing).

- [ ] **Step 3: Implement `chat.rs`, `tkill.rs`, observer camera RPCs, and wire into `battle_host.rs`**
  - Implement `CHAT_ACTION_DATA` FIXED_DICT serialization matching `alias.xml.xml:489-525` (`requestID, action, actionResponse, time, sentTime, channel, originator, originatorNickName, group, data, flags`).
  - Handle `Account`/`Avatar.chatCommand` `requestChannels`, `broadcast(47)`, `ATTENTIONTOCELL(28)`, `ATTACKENEMY(29)`, `HELPME..NEGATIVE(22..=27)`.
  - Wire `Avatar.cell.{autoAim, moveTo, bindToVehicle}` and `Avatar.base.{controlAnotherVehicle, vehicle_useHorn}`.

- [ ] **Step 4: Run tests to verify pass**
  Run: `cargo test --workspace`
  Expected: PASS (`0` failures).

- [ ] **Step 5: Commit**
  ```bash
  git add crates/wot-battle/src/sim/ crates/wot-battle/src/world/arena.rs crates/wot-baseapp/src/battle_host.rs crates/wot-baseapp/src/protocol.rs
  git commit -m "feat(battle): add battle chat, minimap/radial pings, SPG/postmortem AoI, and TK rules"
  ```

---

### Task 3: Post-Battle Results, Economy, Auto-Maintenance, `updateDossier` & `LOCK_REASON.ON_ARENA`

**Files:**
- Create: `crates/wot-battle/src/sim/results.rs`
- Create: `crates/wot065-data/src/ops/battle.rs`
- Modify: `crates/wot065-data/src/ops/mod.rs`
- Modify: `crates/wot065-data/src/dossier.rs`
- Modify: `crates/wot065-data/src/account_state.rs`
- Modify: `crates/wot065-data/src/account.rs`
- Modify: `crates/wot-baseapp/src/battle_host.rs`
- Modify: `crates/wot-baseapp/src/registry.rs`

**Interfaces:**
- Consumes: `ArenaInstance` final combat telemetry, `Catalog`, `AccountProfile`, `AccountDossier`, `VehicleDossier`, `TankmanDossier`
- Produces:
  - `BattleResultsExt` (38-field exact wire serializer matching `alias.xml.xml:299-442`) + `PersonalBattleOutcome` (`xp, credits, free_xp, repair_cost, remaining_hp, shells_used, eqs_used, kills_comp_descrs, hero_achievements, medals, pop_up_records, sys_msg_dict, tkill`)
  - `wot065_data::dossier::update_dossiers(&mut acct, &mut veh, &mut tmen, &DossierBattleInput)` (exact v19/v17/v10 mutations, `vehTypeFrags`, `vehDossiersCut`, `battleLifeTime` only on non-regular matches)
  - `AccountEngine::apply_battle_outcome(profile, catalog, &outcome, now_utc) -> Result<(AccountDiff, Vec<PendingMaintenanceMsg>), CmdError>` (deducts repair/ammo/eqs, runs `AUTO_MAINTENANCE` from depot then credits handling shortfalls cleanly, adds XP/FreeXP/CrewXP/Credits, updates dossiers, clears `LOCK_REASON::ON_ARENA` -> `0`)
  - `AccountProfile.pending_sys_messages: Vec<Vec<u8>>` (delivered cleanly upon reconnect if player logged out before arena ended)

- [ ] **Step 1: Write failing tests for `BATTLE_RESULTS_EXT`, `update_dossiers`, auto-maintenance shortfall, `ON_ARENA` lock/unlock, and offline pending delivery**
  Tests in `crates/wot065-data/src/ops/battle.rs` & `crates/wot-battle/src/sim/results.rs`:
  - `battle_results_ext_and_sys_message_encode_all_fields`: checks exact wire byte layout of 38-field `BATTLE_RESULTS_EXT` and `SYS_MESSAGE_TYPE.battleResults` dict expected by `BattleResultsFormatter`.
  - `apply_battle_outcome_updates_xp_credits_auto_maint_and_dossiers`:
    - Regular win increments `battlesCount`, `wins`, `survivedBattles`, `frags`, `vehTypeFrags`, `vehDossiersCut`, `xp`, `free_xp`, tankman `xp`, `credits`; deducts component+hull repair (`vehicles.py:405-412`); deducts fired shells and used equipment, then auto-restores ammo/eqs from depot & credits; unlocks `vehsLock` (`16 -> 0`) emitting `cache['vehsLock']` diff!
    - Shortfall test (`credits = 10` below repair cost): keeps partial repair (`repair: Some(...)`), never goes negative credits, emits `AUTO_MAINTENANCE_RESULT::NOT_ENOUGH_ASSETS`.
    - Training Room (`bonusType = 0`) updates ONLY `battleLifeTime` and spends `0` credits/shells/repair.

- [ ] **Step 2: Run tests to verify failure**
  Run: `cargo test -p wot-battle sim::results && cargo test -p wot065-data ops::battle`
  Expected: FAIL (`results` and `ops::battle` modules missing).

- [ ] **Step 3: Implement `results.rs`, `ops/battle.rs`, `dossier::update_dossiers`, and background arena-end reward routing in `wot-baseapp`**
  - Compute WoT 0.6.5 XP/Credits formulas (base per damage/spot/kill/cap/decap + win 1.5x + premium 1.5x + survivor/damage bonuses, `xpFactor`, hero badges `warrior`, `invader`, `sniper`, `defender`, `steelwall`, `supporter`, `scout`, TK penalty transfer).
  - Lock tank with `LOCK_REASON::ON_ARENA` when Random battle spawns; keep locked on early death exit; when arena finishes, lock account store, call `apply_battle_outcome`, commit atomically to `JsonAccountStore`, push `Account.update` + `resyncDossiers` + `onChatAction(personalSysMessage)` if online in Hangar, else push to `profile.pending_sys_messages` flushed on next `ENABLE_ENTITIES`/`showGUI`!

- [ ] **Step 4: Run tests to verify pass**
  Run: `cargo test --workspace`
  Expected: PASS (`0` failures).

- [ ] **Step 5: Commit**
  ```bash
  git add crates/wot065-data/src/ crates/wot-battle/src/sim/results.rs crates/wot-baseapp/src/
  git commit -m "feat(economy): implement BATTLE_RESULTS_EXT, auto-maintenance, updateDossier, and ON_ARENA locks"
  ```

---

### Task 4: CLI Flags, `--dev-features`, Full UDP Multi-Client Battle E2E & Persistence Verification

**Files:**
- Modify: `src/main.rs`
- Modify: `crates/wot-baseapp/src/lib.rs`
- Modify: `crates/wot-baseapp/src/main.rs`
- Modify: `crates/wot-baseapp/src/battle_host.rs`
- Create: `crates/wot-baseapp/tests/e2e_battle_full.rs`

**Interfaces:**
- Consumes: `BaseAppConfig`, `ServeArgs`, `build_baseapp_args`, `BattleWorld`, `AccountRegistry`
- Produces:
  - CLI options wired on both `wot-server serve` and `wot-baseapp`:
    - `--team-size <N>` (default `7`, `1..=15`)
    - `--queue-wait-secs <N>` (default `5`)
    - `--prebattle-countdown-secs <N>` (default `30`)
    - `--dev-features` (bool flag; auto-enabled when `--preset sandbox`)
    - `--battle-config <PATH>` (optional JSON override for `BalanceConfig`)
  - Gated `Avatar.setDevelopmentFeature` (`server_marker` always enabled; `heal`, `reload_gun`, `start_fire`, `explode`, `break_left_track`, `break_right_track`, `destroy_self`, `stop_bot` require `dev_features`) + `CMD_CREATE_ARENA (10006)` / `CMD_JOIN_ARENA (10007)`

- [ ] **Step 1: Write failing full-cycle UDP E2E test (`crates/wot-baseapp/tests/e2e_battle_full.rs`) and CLI flag parsing tests (`src/main.rs`)**
  `e2e_battle_full.rs` verifies under `target/test-tmp/`:
  1. Two live UDP accounts (`p1`, `p2`) log in, queue into Random (`1v1` configured `--team-size 1` + bots or direct 2-player battle on real/fixture map), complete `onClientReady`.
  2. `p1` toggles `server_marker` (`1`), sends team chat + `ATTENTIONTOCELL`, drives, aims, destroys `p2` (or captures enemy `TeamBase` to 100); `p2` dies, leaves early -> verifies `cache['vehsLock'][veh_id] == 16` (`ON_ARENA`) in Hangar.
  3. Match finishes (`AFTERBATTLE`): verifies `p1` gets `onVehicleLeftArena(BATTLE_RESULTS_EXT)` and returns to Hangar; `p2` (waiting in Hangar) receives `Account.update(diff)` unlocking tank (`lock -> 0`), updated `xp`/`credits`/deducted shells & repair, `resyncDossiers`, and `personalSysMessage(SYS_MESSAGE_TYPE.battleResults)`.
  4. Reboot `AccountRegistry` from disk (`JsonAccountStore`) and verify updated `credits`, `xp`, `shells`, `AccountDossier`, and `VehicleDossier` survive restart!
  5. Repeat short flow via Training Room (`createTraining`) confirming zero credit/ammo/repair/dossier changes and `dev_features` gating (`heal` works when `--dev-features` is set, ignored when omitted while `server_marker` always works).

- [ ] **Step 2: Run tests to verify failure**
  Run: `cargo test -p wot-baseapp --test e2e_battle_full`
  Expected: FAIL (CLI args / `setDevelopmentFeature` / background unlock hook missing).

- [ ] **Step 3: Wire CLI flags in `src/main.rs` & `wot-baseapp`, gate dev features, and verify full suite**
  - Add battle flags to `ServeArgs` + forward inside `build_baseapp_args` (`src/main.rs`) and `BaseAppConfig` (`crates/wot-baseapp/src/lib.rs`, `main.rs`).
  - Wire `setDevelopmentFeature` (`server_marker` always -> emit `updateGunMarker`; cheats -> gated by `dev_features`), `CMD_CREATE_ARENA`, `CMD_JOIN_ARENA`.
  - Verify `wot-server serve --help` prints exact flags accurately (`AGENTS.md` CLI verification discipline).

- [ ] **Step 4: Run full workspace checks & E2E verification**
  Run: `cargo check --workspace && cargo test --workspace`
  Run: `cargo run --bin wot-server -- serve --help`
  Expected: PASS (`0` failures across all unit & UDP E2E tests; `--help` lists all new battle flags).

- [ ] **Step 5: Commit**
  ```bash
  git add src/ crates/wot-baseapp/ crates/wot-battle/ crates/wot065-data/
  git commit -m "feat(wot-server): wire full battle lifecycle CLI flags, dev tools, and UDP E2E verification"
  ```
