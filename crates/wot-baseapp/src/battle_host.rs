//! BaseApp battle host orchestration: entity ID allocator, lobby coordination,
//! Account <-> Avatar session switching, and BattleWorld simulation.

use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::{Duration, Instant};

use bw201::bundle::{ElementEncoder, DEFAULT_MAX_FRAGMENT_BODY};
use bw201::world::DetailedPosition;
use wot065_data::account::stream_bundle;
use wot065_data::account_state::AccountProfile;
use wot065_data::catalog::Catalog;
use wot065_data::descr::VehicleDescr;
use wot065_data::pickle::{self, Value};
use wot_battle::assets::map_store::{MapStore, VehicleCollisionCache};
use wot_battle::balance::BalanceConfig;
use wot_battle::lobby::{
    compute_vehicle_weight, vehicle_class_index, ArenaLaunchSpec, BattleMode, ParticipantSpec, PrebattleEvent,
    PrebattleManager, QueueConfig, QueueEntry, QueueManager, JOIN_FAILURE_ACCOUNT_LOCK,
    JOIN_FAILURE_NOT_FOUND, JOIN_FAILURE_WRONG_ARGS, PREBATTLE_UPDATE_ROSTER,
    PREBATTLE_UPDATE_SETTINGS, PREBATTLE_UPDATE_TEAM_STATES, QUEUE_TYPE_RANDOMS,
};
use wot_battle::world::{BattleWorld, VehicleState, ARENA_PERIOD_BATTLE, ARENA_PERIOD_PREBATTLE};
use wot_battle::sim::chat::CHAT_CMD_ATTACK_ENEMY;
use wot_battle::sim::results::personal_results;
use wot_battle::sim::visibility::VEHICLE_AOI_RADIUS;

use crate::payloads::AccountPayloads;
use crate::protocol::{
    client_method, cmd, emit_arena_space_data, emit_chat_action, emit_create_avatar_base_player,
    emit_create_avatar_cell_player, emit_create_vehicle_entity, emit_enter_aoi,
    emit_entity_i32_method, emit_entity_u8_bytes_method, emit_entity_u8_method,
    emit_entity_void_method, emit_receive_queue_info, emit_relative_position, emit_reset_entities,
    emit_s2c_avatar_update, emit_tick_sync, BaseAppSession, ChatAction, DecodedDoCmd, DoCmdArgs,
    UPDATE_FREQUENCY_HZ,
};
use crate::registry::{
    AccountRegistry, LOCK_REASON_IN_QUEUE, LOCK_REASON_NONE, LOCK_REASON_ON_ARENA,
    LOCK_REASON_PREBATTLE,
};

/// Strict upper bound for server-allocated entity IDs (`client IDs are >= 1 << 30`).
pub const MAX_SERVER_ENTITY_ID: i32 = (1 << 30) - 1;

/// One client update tick: the period of `TICK_SYNC` bytes and `SET_GAME_TIME` game ticks.
const SERVER_TICK_MS: u64 = 1000 / UPDATE_FREQUENCY_HZ as u64;
/// Ticks one host wake-up may simulate before carrying the backlog to the next wake-up.
const MAX_TICKS_PER_WAKEUP: u64 = 3;

/// Whole client update ticks elapsed on the arena clock.
fn arena_game_ticks(arena: &wot_battle::world::arena::ArenaInstance) -> u32 {
    let ticks = arena.clock_elapsed().as_millis() / u128::from(SERVER_TICK_MS);
    u32::try_from(ticks).unwrap_or(u32::MAX)
}

const CHAT_CMD_REQUEST_USERS_ROSTER: u8 = 9;
const CHAT_CMD_REQUEST_SYSTEM_CHANNELS: u8 = 39;
const CHAT_ACTION_REQUEST_CHANNELS: u8 = 6;
const CHAT_ACTION_REQUEST_USERS_ROSTER: u8 = 13;
const CHAT_ACTION_PERSONAL_SYS_MESSAGE: u8 = 22;

/// Battle/lobby configuration passed to `serve_with_battle_and_ready`.
#[derive(Clone)]
pub struct BattleHostConfig {
    pub queue_wait_timeout: Duration,
    pub team_size: usize,
    pub prebattle_countdown_secs: u32,
    pub dev_features: bool,
    pub bot_fill: bool,
    pub tick_interval: Duration,
    pub switching_timeout: Duration,
    pub balance_config: BalanceConfig,
    pub preloaded_maps: Option<Arc<MapStore>>,
}

impl std::fmt::Debug for BattleHostConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BattleHostConfig")
            .field("queue_wait_timeout", &self.queue_wait_timeout)
            .field("team_size", &self.team_size)
            .field("prebattle_countdown_secs", &self.prebattle_countdown_secs)
            .field("dev_features", &self.dev_features)
            .field("bot_fill", &self.bot_fill)
            .field("tick_interval", &self.tick_interval)
            .field("switching_timeout", &self.switching_timeout)
            .field("has_preloaded_maps", &self.preloaded_maps.is_some())
            .finish()
    }
}

impl Default for BattleHostConfig {
    fn default() -> Self {
        Self {
            queue_wait_timeout: Duration::from_secs(5),
            team_size: 7,
            prebattle_countdown_secs: 30,
            dev_features: false,
            bot_fill: true,
            tick_interval: Duration::from_millis(100),
            switching_timeout: Duration::from_secs(30),
            balance_config: BalanceConfig::default(),
            preloaded_maps: None,
        }
    }
}

/// Allocator for server-side entity IDs strictly below `1 << 30` and monotonic space IDs.
#[derive(Debug)]
pub struct EntityIdAllocator {
    next_id: i32,
    free_ids: VecDeque<i32>,
    in_use: HashSet<i32>,
    next_space_id: i32,
}

impl Default for EntityIdAllocator {
    fn default() -> Self {
        Self::new()
    }
}

impl EntityIdAllocator {
    pub fn new() -> Self {
        Self {
            next_id: 1,
            free_ids: VecDeque::new(),
            in_use: HashSet::new(),
            next_space_id: 1,
        }
    }

    pub fn alloc(&mut self) -> i32 {
        while self.next_id <= MAX_SERVER_ENTITY_ID {
            let candidate = self.next_id;
            self.next_id = self.next_id.saturating_add(1);
            if self.in_use.insert(candidate) {
                return candidate;
            }
        }
        while let Some(recycled) = self.free_ids.pop_front() {
            if self.in_use.insert(recycled) {
                return recycled;
            }
        }
        panic!("entity ID space exhausted");
    }

    pub fn alloc_preferred(&mut self, preferred: i32) -> i32 {
        if (1..=MAX_SERVER_ENTITY_ID).contains(&preferred) && self.in_use.insert(preferred) {
            return preferred;
        }
        self.alloc()
    }

    pub fn reserve(&mut self, id: i32) {
        if (1..=MAX_SERVER_ENTITY_ID).contains(&id) {
            self.in_use.insert(id);
        }
    }

    pub fn release(&mut self, id: i32) {
        if (1..=MAX_SERVER_ENTITY_ID).contains(&id) && self.in_use.remove(&id) {
            self.free_ids.push_back(id);
        }
    }

    pub fn alloc_space_id(&mut self) -> i32 {
        let id = self.next_space_id.max(1);
        self.next_space_id = if id >= MAX_SERVER_ENTITY_ID { 1 } else { id + 1 };
        id
    }
}

/// Per-account entity/transition state across Hangar and Battle spaces.
#[derive(Debug, Clone)]
pub enum SessionState {
    Hangar,
    SwitchingToArena {
        space_id: i32,
        arena_type_id: i32,
        arena_gui_type: u8,
        avatar_id: i32,
        started_at: Instant,
    },
    InArena {
        space_id: i32,
        avatar_id: i32,
        vehicle_id: i32,
    },
    SwitchingToHangar {
        account_id: i32,
        pending_diffs: Vec<Vec<u8>>,
        started_at: Instant,
    },
}

#[derive(Debug, Clone)]
struct PeerBattleRecord {
    account_db_id: i64,
    username: String,
    addr: SocketAddr,
    player_id: i32,
    state: SessionState,
    locked_veh_inv_id: Option<i64>,
}

/// Outbound server instruction directed at a connected peer session.
#[allow(dead_code)]
#[derive(Debug, Clone)]
pub(crate) enum PeerInstruction {
    AccountUpdate(Vec<u8>),
    ResyncDossiers,
    BattleResults {
        avatar_id: i32,
        vehicle_inv_id: i32,
        wire: Vec<u8>,
    },
    FinishedArenaSwitch,
    CmdResponse {
        request_id: i16,
        result_id: i16,
    },
    OnEnqueued,
    OnEnqueueFailure(u8),
    OnDequeued,
    OnArenaJoinFailure(u8),
    StartArenaSwitch,
    OnPrebattleJoined(i32),
    OnPrebattleJoinFailure(u8),
    OnPrebattleLeft,
    OnKickedFromPrebattle(u8),
    UpdatePrebattle {
        update_type: u8,
        payload: Vec<u8>,
    },
    AvatarUpdateArena {
        avatar_id: i32,
        update_type: u8,
        payload: Vec<u8>,
    },
    StreamResource {
        request_id: i16,
        raw_pickle: Vec<u8>,
        send_cmd_response: bool,
    },
    ReceiveQueueInfo {
        randoms_wire: Vec<u8>,
        companies_wire: Vec<u8>,
    },
    ChatAction(ChatAction),
    AvatarAutoAimLost {
        avatar_id: i32,
        reason: u8,
    },
    /// Make the player's Avatar a passenger of `vehicle_id` (`Avatar.vehicle` on the client).
    AvatarBoardVehicle {
        avatar_id: i32,
        vehicle_id: i32,
        space_id: i32,
    },
    AvatarHorn {
        avatar_id: i32,
        vehicle_id: i32,
        start: bool,
    },
    EnterAoI(i32),
    LeaveAoI(i32),
    UpdateEntity {
        entity_id: i32,
        properties: Vec<u8>,
    },
    AvatarTargetingInfo {
        avatar_id: i32,
        turret_yaw: f32,
        gun_pitch: f32,
        max_turret_rot_speed: f32,
        max_gun_rot_speed: f32,
        shot_disp_multiplier_factor: f32,
        aiming_time: f32,
    },
    AvatarGunMarker {
        avatar_id: i32,
        marker_pos: [f32; 3],
        marker_dir: [f32; 3],
        disp_angle: f32,
    },
    AvatarReloadTime {
        avatar_id: i32,
        time_left: f32,
    },
    AvatarAmmo {
        avatar_id: i32,
        compact_descr: i32,
        quantity: u16,
        time_remaining: u16,
    },
    AvatarHealth {
        avatar_id: i32,
        health: i16,
        is_crew_active: bool,
    },
    AvatarDamageInfo {
        avatar_id: i32,
        code: u8,
        extra_index: u8,
        entity_id: i32,
    },
    AvatarVehicleSetting {
        avatar_id: i32,
        code: u8,
        value: i32,
    },
    AvatarPositions {
        avatar_id: i32,
        indices: Vec<u8>,
        positions: Vec<i16>,
    },
    AvatarShowTracer {
        avatar_id: i32,
        shooter_id: i32,
        shot_id: i32,
        effects_index: u8,
        ref_start_point: [f32; 3],
        velocity: [f32; 3],
        gravity: f32,
    },
    AvatarStopTracer {
        avatar_id: i32,
        shot_id: i32,
        end_point: [f32; 3],
    },
    AvatarExplodeProjectile {
        avatar_id: i32,
        shot_id: i32,
        effects_index: u8,
        effect_material_index: u8,
        end_point: [f32; 3],
        velocity_dir: [f32; 3],
    },
    VehicleShowShooting {
        vehicle_id: i32,
    },
    VehicleShowDamageFromShot {
        vehicle_id: i32,
        attacker_id: i32,
        points: Vec<u64>,
        effects_index: u8,
    },
    VehicleShowDamageFromExplosion {
        vehicle_id: i32,
        attacker_id: i32,
        center: [f32; 3],
        effects_index: u8,
    },
    VehicleShowFireStarted {
        vehicle_id: i32,
        attacker_id: i32,
    },
    VehicleOnPushed {
        vehicle_id: i32,
        x: f32,
        z: f32,
    },
}

#[derive(Debug, Clone)]
pub(crate) struct RoutedAction {
    pub account_db_id: i64,
    pub addr: SocketAddr,
    pub instruction: PeerInstruction,
}

/// Apply a [`PeerInstruction`] directly onto `sess`'s [`ElementEncoder`].
pub(crate) fn encode_instruction(
    sess: &mut BaseAppSession,
    encoder: &mut ElementEncoder,
    inst: PeerInstruction,
) {
    let pid = sess.player_id();
    match inst {
        PeerInstruction::AccountUpdate(diff) => sess.emit_update(encoder, &diff),
        PeerInstruction::ResyncDossiers => sess.emit_resync_dossiers(encoder),
        PeerInstruction::BattleResults { avatar_id, vehicle_inv_id, wire } => {
            let mut body = bw201::stream::BinaryWriter::new();
            body.i32(avatar_id);
            body.u8(1); // active vehicle
            body.i32(vehicle_inv_id);
            body.as_mut_vec().extend_from_slice(&wire);
            encoder.message(client_method::AVATAR_ON_VEHICLE_LEFT_ARENA, bw201::interfaces::LengthStyle::Variable(2), body.as_slice());
        }
        PeerInstruction::FinishedArenaSwitch => {
            emit_entity_u8_method(encoder, pid, client_method::AVATAR_ON_KICKED_FROM_ARENA, 8);
            sess.set_entities_enabled(false);
            emit_reset_entities(encoder, false);
        }
        PeerInstruction::CmdResponse {
            request_id,
            result_id,
        } => sess.emit_cmd_response(encoder, request_id, result_id),
        PeerInstruction::OnEnqueued => {
            emit_entity_void_method(encoder, pid, client_method::ON_ENQUEUED);
        }
        PeerInstruction::OnEnqueueFailure(code) => {
            emit_entity_u8_method(encoder, pid, client_method::ON_ENQUEUE_FAILURE, code);
        }
        PeerInstruction::OnDequeued => {
            emit_entity_void_method(encoder, pid, client_method::ON_DEQUEUED);
        }
        PeerInstruction::OnArenaJoinFailure(code) => {
            emit_entity_u8_method(encoder, pid, client_method::ON_ARENA_JOIN_FAILURE, code);
        }
        PeerInstruction::StartArenaSwitch => {
            emit_entity_void_method(encoder, pid, client_method::ON_ARENA_CREATED);
            emit_reset_entities(encoder, false);
            sess.set_entities_enabled(false);
        }
        PeerInstruction::OnPrebattleJoined(prb_id) => {
            emit_entity_i32_method(encoder, pid, client_method::ON_PREBATTLE_JOINED, prb_id);
        }
        PeerInstruction::OnPrebattleJoinFailure(code) => {
            emit_entity_u8_method(encoder, pid, client_method::ON_PREBATTLE_JOIN_FAILURE, code);
        }
        PeerInstruction::OnPrebattleLeft => {
            emit_entity_void_method(encoder, pid, client_method::ON_PREBATTLE_LEFT);
        }
        PeerInstruction::OnKickedFromPrebattle(reason) => {
            emit_entity_u8_method(encoder, pid, client_method::ON_KICKED_FROM_PREBATTLE, reason);
        }
        PeerInstruction::UpdatePrebattle {
            update_type,
            payload,
        } => {
            emit_entity_u8_bytes_method(
                encoder,
                pid,
                client_method::UPDATE_PREBATTLE,
                update_type,
                &payload,
            );
        }
        PeerInstruction::AvatarUpdateArena {
            avatar_id,
            update_type,
            payload,
        } => {
            emit_entity_u8_bytes_method(
                encoder,
                avatar_id,
                client_method::AVATAR_UPDATE_ARENA,
                update_type,
                &payload,
            );
        }
        PeerInstruction::StreamResource {
            request_id,
            raw_pickle,
            send_cmd_response,
        } => {
            let (compressed, desc) = stream_bundle(&raw_pickle);
            if send_cmd_response {
                sess.emit_cmd_response(encoder, request_id, cmd::RES_STREAM);
            }
            sess.emit_resource_stream(encoder, request_id as u16, &compressed, &desc);
        }
        PeerInstruction::ReceiveQueueInfo {
            randoms_wire,
            companies_wire,
        } => {
            emit_receive_queue_info(encoder, pid, &randoms_wire, &companies_wire);
        }
        PeerInstruction::ChatAction(action) => {
            emit_chat_action(encoder, pid, &action);
        }
        PeerInstruction::AvatarAutoAimLost { avatar_id, reason } => {
            emit_entity_u8_method(encoder, avatar_id, client_method::AVATAR_ON_AUTO_AIM_VEHICLE_LOST, reason);
        }
        PeerInstruction::AvatarBoardVehicle { avatar_id, vehicle_id, space_id } => {
            use bw201::interfaces::client_ids;
            use bw201::world::{Direction3D, ForcedPosition, SetVehicle};
            let mut body = Vec::with_capacity(ForcedPosition::WIRE_SIZE);
            SetVehicle { passenger_id: avatar_id, vehicle_id }.write_to(&mut body);
            encoder.fixed_message(client_ids::SET_VEHICLE, SetVehicle::WIRE_SIZE as u16, &body);
            body.clear();
            // The passenger sits at the vehicle origin.
            ForcedPosition {
                id: avatar_id,
                space_id,
                vehicle_id,
                position: [0.0; 3],
                direction: Direction3D::new(0.0, 0.0, 0.0),
            }
            .write_to(&mut body);
            encoder.fixed_message(client_ids::FORCED_POSITION, ForcedPosition::WIRE_SIZE as u16, &body);
        }
        PeerInstruction::AvatarHorn { avatar_id, vehicle_id, start } => {
            let mut body = bw201::stream::BinaryWriter::new();
            body.i32(avatar_id);
            body.i32(vehicle_id);
            body.u8(0);
            body.u8(u8::from(start));
            encoder.message(client_method::AVATAR_RECEIVE_HORN, bw201::interfaces::LengthStyle::Variable(2), body.as_slice());
        }
        PeerInstruction::EnterAoI(entity_id) => {
            crate::protocol::emit_enter_aoi(encoder, entity_id);
        }
        PeerInstruction::LeaveAoI(entity_id) => {
            crate::protocol::emit_leave_aoi(encoder, entity_id);
        }
        PeerInstruction::UpdateEntity {
            entity_id,
            properties,
        } => {
            crate::protocol::emit_update_entity(encoder, entity_id, &properties);
        }
        PeerInstruction::AvatarTargetingInfo {
            avatar_id,
            turret_yaw,
            gun_pitch,
            max_turret_rot_speed,
            max_gun_rot_speed,
            shot_disp_multiplier_factor,
            aiming_time,
        } => {
            crate::protocol::emit_avatar_update_targeting_info(
                encoder,
                avatar_id,
                turret_yaw,
                gun_pitch,
                max_turret_rot_speed,
                max_gun_rot_speed,
                shot_disp_multiplier_factor,
                aiming_time,
            );
        }
        PeerInstruction::AvatarGunMarker {
            avatar_id,
            marker_pos,
            marker_dir,
            disp_angle,
        } => {
            crate::protocol::emit_avatar_update_gun_marker(
                encoder,
                avatar_id,
                marker_pos,
                marker_dir,
                disp_angle,
            );
        }
        PeerInstruction::AvatarReloadTime { avatar_id, time_left } => {
            crate::protocol::emit_avatar_update_vehicle_gun_reload_time(encoder, avatar_id, time_left);
        }
        PeerInstruction::AvatarAmmo {
            avatar_id,
            compact_descr,
            quantity,
            time_remaining,
        } => {
            crate::protocol::emit_avatar_update_vehicle_ammo(
                encoder,
                avatar_id,
                compact_descr,
                quantity,
                time_remaining,
            );
        }
        PeerInstruction::AvatarHealth {
            avatar_id,
            health,
            is_crew_active,
        } => {
            crate::protocol::emit_avatar_update_vehicle_health(
                encoder,
                avatar_id,
                health,
                is_crew_active,
            );
        }
        PeerInstruction::AvatarDamageInfo {
            avatar_id,
            code,
            extra_index,
            entity_id,
        } => {
            crate::protocol::emit_avatar_show_vehicle_damage_info(
                encoder,
                avatar_id,
                code,
                extra_index,
                entity_id,
            );
        }
        PeerInstruction::AvatarVehicleSetting {
            avatar_id,
            code,
            value,
        } => {
            crate::protocol::emit_avatar_update_vehicle_setting(encoder, avatar_id, code, value);
        }
        PeerInstruction::AvatarPositions {
            avatar_id,
            indices,
            positions,
        } => {
            crate::protocol::emit_avatar_update_positions(encoder, avatar_id, &indices, &positions);
        }
        PeerInstruction::AvatarShowTracer {
            avatar_id,
            shooter_id,
            shot_id,
            effects_index,
            ref_start_point,
            velocity,
            gravity,
        } => {
            crate::protocol::emit_avatar_show_tracer(
                encoder,
                avatar_id,
                shooter_id,
                shot_id,
                effects_index,
                ref_start_point,
                velocity,
                gravity,
            );
        }
        PeerInstruction::AvatarStopTracer {
            avatar_id,
            shot_id,
            end_point,
        } => {
            crate::protocol::emit_avatar_stop_tracer(encoder, avatar_id, shot_id, end_point);
        }
        PeerInstruction::AvatarExplodeProjectile {
            avatar_id,
            shot_id,
            effects_index,
            effect_material_index,
            end_point,
            velocity_dir,
        } => {
            crate::protocol::emit_avatar_explode_projectile(
                encoder,
                avatar_id,
                shot_id,
                effects_index,
                effect_material_index,
                end_point,
                velocity_dir,
            );
        }
        PeerInstruction::VehicleShowShooting { vehicle_id } => {
            crate::protocol::emit_vehicle_show_shooting(encoder, vehicle_id);
        }
        PeerInstruction::VehicleShowDamageFromShot {
            vehicle_id,
            attacker_id,
            points,
            effects_index,
        } => {
            crate::protocol::emit_vehicle_show_damage_from_shot(
                encoder,
                vehicle_id,
                attacker_id,
                &points,
                effects_index,
            );
        }
        PeerInstruction::VehicleShowDamageFromExplosion {
            vehicle_id,
            attacker_id,
            center,
            effects_index,
        } => {
            crate::protocol::emit_vehicle_show_damage_from_explosion(
                encoder,
                vehicle_id,
                attacker_id,
                center,
                effects_index,
            );
        }
        PeerInstruction::VehicleShowFireStarted {
            vehicle_id,
            attacker_id,
        } => {
            crate::protocol::emit_vehicle_show_fire_started(encoder, vehicle_id, attacker_id);
        }
        PeerInstruction::VehicleOnPushed { vehicle_id, x, z } => {
            crate::protocol::emit_vehicle_on_pushed(encoder, vehicle_id, x, z);
        }
    }
}

fn pending_battle_messages(registry: &mut AccountRegistry, username: &str) -> Vec<ChatAction> {
    match registry.take_battle_messages(username) {
        Ok(messages) => messages.into_iter().map(|message| {
            ChatAction::reply(-1, CHAT_ACTION_PERSONAL_SYS_MESSAGE, message)
        }).collect(),
        Err(error) => {
            tracing::warn!(%error, "battle notifications remain queued on disk");
            Vec::new()
        }
    }
}

fn emit_pending_battle_messages(
    registry: &mut AccountRegistry,
    username: &str,
    account_id: i32,
    encoder: &mut ElementEncoder,
) {
    for message in pending_battle_messages(registry, username) {
        emit_chat_action(encoder, account_id, &message);
    }
}

fn is_in_observer_aoi(
    arena: &wot_battle::world::ArenaInstance,
    obs_veh_id: i32,
    obs_team: u8,
    target_id: i32,
) -> bool {
    if target_id == obs_veh_id {
        return true;
    }
    if let Some(known) = arena.visibility.previous_aoi.get(&obs_veh_id) {
        return known.contains(&target_id);
    }
    arena.vehicle_by_id(obs_veh_id).zip(arena.vehicle_by_id(target_id)).is_some_and(|(observer, target)| {
        observer.position.distance_squared(target.position) <= VEHICLE_AOI_RADIUS * VEHICLE_AOI_RADIUS
            && (target.team == obs_team || arena.last_visibility.spotted_enemies
                .get(&obs_veh_id).is_some_and(|spotted| spotted.contains(&target_id)))
    })
}

fn encode_vehicle_update_props(v: &VehicleState) -> Vec<u8> {
    let mut props = bw201::stream::BinaryWriter::new();
    props.u8(5); // 5 properties
    props.u8(1); // 1: health (INT16)
    props.i16(v.health);
    props.u8(2); // 2: isCrewActive (BOOL)
    props.u8(if v.is_crew_active { 1 } else { 0 });
    props.u8(3); // 3: engineMode (TUPLE<UINT8, size=2>)
    props.u8(v.engine_mode[0]);
    props.u8(v.engine_mode[1]);
    props.u8(4); // 4: damageStickers (ARRAY<UINT64>)
    props.i32(v.damage_stickers.len() as i32);
    for &s in &v.damage_stickers {
        props.u64(s);
    }
    props.u8(5); // 5: publicStateModifiers (ARRAY<EXTRA_ID>)
    if v.is_burning {
        props.i32(1);
        props.u8(wot_battle::sim::EXTRA_FIRE);
    } else {
        props.i32(0);
    }
    props.as_slice().to_vec()
}

fn encode_destructible_chunk_update_props(
    chunk: &wot_battle::sim::destructibles::ChunkDestructiblesState,
) -> Vec<u8> {
    let mut props = bw201::stream::BinaryWriter::new();
    props.u8(3); // 3 properties
    props.u8(0); // 0: destroyedModules (ARRAY<UINT16>)
    props.i32(chunk.destroyed_modules.len() as i32);
    for &m in &chunk.destroyed_modules {
        props.u16(m);
    }
    props.u8(1); // 1: destroyedFragiles (ARRAY<UINT8>)
    props.i32(chunk.destroyed_fragiles.len() as i32);
    for &f in &chunk.destroyed_fragiles {
        props.u8(f);
    }
    props.u8(2); // 2: fallenDestructibles (ARRAY<UINT16>)
    props.i32(chunk.fallen_destructibles.len() as i32);
    for &fd in &chunk.fallen_destructibles {
        props.u16(fd);
    }
    props.as_slice().to_vec()
}

/// Coordinates matchmaker queue, training prebattles, active arenas, and Account <-> Avatar switches.
struct ArenaSettlement {
    unique_id: u64,
    created_at: u32,
    players: BTreeMap<i64, String>,
    retry_at: Instant,
}

pub struct BattleHost {
    config: BattleHostConfig,
    alloc: EntityIdAllocator,
    maps: Arc<MapStore>,
    /// Vehicle hit testers, loaded once per vehicle configuration for every arena.
    collision_cache: Arc<VehicleCollisionCache>,
    queue: QueueManager,
    prebattle: PrebattleManager,
    world: BattleWorld,
    peers: HashMap<i64, PeerBattleRecord>,
    addr_to_account: HashMap<SocketAddr, i64>,
    last_step_at: Option<Instant>,
    settlements: HashMap<i32, ArenaSettlement>,
}

impl BattleHost {
    pub fn new(config: BattleHostConfig, maps: Arc<MapStore>) -> Self {
        let queue_cfg = QueueConfig {
            queue_wait_timeout: config.queue_wait_timeout,
            team_size: config.team_size.max(1),
            bot_fill: config.bot_fill,
        };
        let queue = QueueManager::new(queue_cfg, config.balance_config.matchmaker.clone());
        let collision_cache = Arc::new(VehicleCollisionCache::new(maps.res_dir()));
        Self {
            config,
            alloc: EntityIdAllocator::new(),
            maps,
            collision_cache,
            queue,
            prebattle: PrebattleManager::new(),
            world: BattleWorld::new(),
            peers: HashMap::new(),
            addr_to_account: HashMap::new(),
            last_step_at: None,
            settlements: HashMap::new(),
        }
    }

    pub fn alloc_entity_id(&mut self) -> i32 {
        self.alloc.alloc()
    }

    pub fn release_entity_id(&mut self, id: i32) {
        self.alloc.release(id);
    }

    pub fn account_for_addr(&self, addr: SocketAddr) -> Option<i64> {
        self.addr_to_account.get(&addr).copied()
    }

    pub fn addr_for_account(&self, account_db_id: i64) -> Option<SocketAddr> {
        self.peers.get(&account_db_id).map(|p| p.addr)
    }

    pub fn is_in_arena(&self, addr: SocketAddr) -> bool {
        self.addr_to_account
            .get(&addr)
            .and_then(|id| self.peers.get(id))
            .is_some_and(|p| matches!(p.state, SessionState::InArena { .. }))
    }

    pub fn is_switching(&self, addr: SocketAddr) -> bool {
        self.addr_to_account
            .get(&addr)
            .and_then(|id| self.peers.get(id))
            .is_some_and(|p| {
                matches!(
                    p.state,
                    SessionState::SwitchingToArena { .. } | SessionState::SwitchingToHangar { .. }
                )
            })
    }

    pub(crate) fn on_enable_entities(
        &mut self,
        addr: SocketAddr,
        sess: &mut BaseAppSession,
        encoder: &mut ElementEncoder,
        registry: &mut AccountRegistry,
    ) -> anyhow::Result<Vec<RoutedAction>> {
        let existing_db_id = self.addr_to_account.get(&addr).copied();
        if let Some(db_id) = existing_db_id {
            let state = self.peers.get(&db_id).map(|p| p.state.clone());
            match state {
                Some(SessionState::SwitchingToArena {
                    space_id,
                    arena_type_id,
                    arena_gui_type,
                    avatar_id,
                    ..
                }) => {
                    let old_player_id = sess.player_id();
                    self.alloc.release(old_player_id);

                    let Some(arena) = self.world.get(space_id) else {
                        tracing::warn!(space_id, db_id, "arena vanished before ENABLE_ENTITIES");
                        return self.rollback_arena_switch_to_hangar(
                            db_id, space_id, avatar_id, sess, encoder, registry,
                        );
                    };

                    let Some(player_veh) = arena.vehicle_for_account(db_id) else {
                        tracing::warn!(space_id, db_id, "player vehicle missing in arena");
                        return self.rollback_arena_switch_to_hangar(
                            db_id, space_id, avatar_id, sess, encoder, registry,
                        );
                    };

                    let vehicle_id = player_veh.entity_id;
                    let player_name = player_veh.player_name.clone();
                    let team = player_veh.team;
                    let spawn_pos = [
                        player_veh.position.x,
                        player_veh.position.y,
                        player_veh.position.z,
                    ];
                    let spawn_yaw = player_veh.yaw;
                    let arena_ticks = arena_game_ticks(arena);
                    let geometry = arena.map.def.geometry.clone();
                    let vehicle_list = arena.vehicle_list_pickle();
                    let period_pickle = arena.period_pickle();

                    sess.set_player_id(avatar_id);
                    sess.set_entities_enabled(true);

                    if let Some(rec) = self.peers.get_mut(&db_id) {
                        rec.player_id = avatar_id;
                        rec.state = SessionState::InArena {
                            space_id,
                            avatar_id,
                            vehicle_id,
                        };
                    }

                    sess.emit_set_game_time(encoder, arena_ticks);
                    emit_create_avatar_base_player(
                        encoder,
                        avatar_id,
                        &player_name,
                        arena_type_id,
                        arena_gui_type,
                        0,
                    );
                    emit_arena_space_data(encoder, space_id, &geometry);
                    emit_create_avatar_cell_player(
                        encoder,
                        space_id,
                        spawn_pos,
                        spawn_yaw,
                        team,
                        vehicle_id,
                    );
                    emit_entity_u8_bytes_method(
                        encoder,
                        avatar_id,
                        client_method::AVATAR_UPDATE_ARENA,
                        wot_battle::world::ARENA_UPDATE_VEHICLE_LIST,
                        &vehicle_list,
                    );
                    emit_entity_u8_bytes_method(
                        encoder,
                        avatar_id,
                        client_method::AVATAR_UPDATE_ARENA,
                        wot_battle::world::ARENA_UPDATE_PERIOD,
                        &period_pickle,
                    );
                    emit_entity_u8_bytes_method(
                        encoder,
                        avatar_id,
                        client_method::AVATAR_UPDATE_ARENA,
                        wot_battle::world::ARENA_UPDATE_STATISTICS,
                        &arena.statistics_pickle(),
                    );
                    for v in &arena.vehicles {
                        if is_in_observer_aoi(arena, vehicle_id, team, v.entity_id) {
                            emit_enter_aoi(encoder, v.entity_id);
                        }
                    }
                    let mut chunk_ids: Vec<u16> =
                        arena.destructibles.chunks.keys().copied().collect();
                    chunk_ids.sort_unstable();
                    for chunk_id in chunk_ids {
                        emit_enter_aoi(
                            encoder,
                            wot_battle::sim::destructibles::chunk_entity_id(chunk_id),
                        );
                    }
                    return Ok(Vec::new());
                }
                Some(SessionState::SwitchingToHangar {
                    account_id,
                    pending_diffs,
                    ..
                }) => {
                    let old_avatar_id = sess.player_id();
                    self.alloc.release(old_avatar_id);
                    return self.complete_hangar_entry(
                        db_id,
                        account_id,
                        pending_diffs,
                        Vec::new(),
                        sess,
                        encoder,
                        registry,
                    );
                }
                Some(SessionState::Hangar | SessionState::InArena { .. }) => {
                    return Ok(Vec::new());
                }
                None => {}
            }
        }

        let username = sess.login().username.clone();
        let db_id = registry
            .get_or_create_profile(&sess.login().username)
            .map_err(|e| anyhow::anyhow!("failed loading profile: {e}"))?.database_id;

        // If this account had a lingering record (e.g. abrupt drop on another port), clean it first.
        let mut actions = self.cleanup_account_internal(db_id, registry);
        if let Some(arena) = self.world.arena_for_account(db_id).filter(|a| {
            a.mode == BattleMode::Random && a.finished.is_none()
        }) {
            let avatar_id = self.alloc.alloc();
            let state = SessionState::SwitchingToArena {
                space_id: arena.space_id,
                arena_type_id: arena.map.def.id,
                arena_gui_type: arena.arena_gui_type,
                avatar_id,
                started_at: Instant::now(),
            };
            self.peers.insert(db_id, PeerBattleRecord {
                account_db_id: db_id, addr, username, player_id: 0, state,
                locked_veh_inv_id: arena.vehicle_for_account(db_id).map(|vehicle| vehicle.veh_inv_id),
            });
            self.addr_to_account.insert(addr, db_id);
            sess.set_player_id(0);
            actions.extend(self.on_enable_entities(addr, sess, encoder, registry)?);
            return Ok(actions);
        }
        let _ = registry.unlock_vehicles_except(&username, &self.protected_locks(db_id));
        let rev = registry.get_profile(&username).map_or(0, |account| account.rev);

        let player_id = self.alloc.alloc_preferred(db_id as i32);
        sess.set_player_id(player_id);
        self.peers.insert(
            db_id,
            PeerBattleRecord {
                account_db_id: db_id,
                username: username.clone(),
                addr,
                player_id,
                state: SessionState::Hangar,
                locked_veh_inv_id: None,
            },
        );
        self.addr_to_account.insert(addr, db_id);

        sess.set_entities_enabled(true);
        sess.set_account_rev(rev);
        sess.emit_hangar_entry(encoder, db_id, None, true);
        emit_pending_battle_messages(registry, &username, player_id, encoder);
        Ok(actions)
    }

    pub(crate) fn on_request_entity_update(
        &self,
        addr: SocketAddr,
        entity_id: i32,
        encoder: &mut ElementEncoder,
    ) {
        let Some(db_id) = self.addr_to_account.get(&addr).copied() else {
            return;
        };
        let Some(rec) = self.peers.get(&db_id) else {
            return;
        };
        let SessionState::InArena { space_id, vehicle_id, .. } = rec.state else {
            return;
        };
        let Some(arena) = self.world.get(space_id) else {
            return;
        };
        if let Some(v) = arena.vehicle_by_id(entity_id) {
            if arena.vehicle_by_id(vehicle_id).is_some_and(|observer| {
                is_in_observer_aoi(arena, vehicle_id, observer.team, entity_id)
            }) {
                emit_create_vehicle_entity(encoder, v);
            }
        } else if let Some(chunk_id) =
            wot_battle::sim::destructibles::entity_id_to_chunk_id(entity_id)
        {
            if let Some(chunk) = arena.destructibles.chunks.get(&chunk_id) {
                let (gx, gz) = wot_battle::sim::destructibles::decode_chunk_id(chunk_id);
                let pos = [gx as f32 * 100.0 + 50.0, 0.0, gz as f32 * 100.0 + 50.0];
                crate::protocol::emit_create_area_destructibles_entity(
                    encoder,
                    entity_id,
                    pos,
                    &chunk.destroyed_modules,
                    &chunk.destroyed_fragiles,
                    &chunk.fallen_destructibles,
                );
            }
        }
    }

    pub(crate) fn on_chat_command(
        &self,
        addr: SocketAddr,
        data: &[u8],
    ) -> Vec<RoutedAction> {
        let Some(db_id) = self.addr_to_account.get(&addr).copied() else {
            return Vec::new();
        };
        let mut r = bw201::stream::BinaryReader::new(data);
        let Ok(request_id) = r.i64() else {
            return Vec::new();
        };
        let Ok(action) = r.u8() else {
            return Vec::new();
        };
        let active_arena = self.peers.get(&db_id).and_then(|peer| match peer.state {
            SessionState::InArena { space_id, vehicle_id, .. } => {
                let arena = self.world.get(space_id)?;
                Some((arena, arena.vehicle_by_id(vehicle_id)?))
            }
            _ => None,
        });
        let resp_action = match action {
            CHAT_CMD_REQUEST_SYSTEM_CHANNELS => Some(CHAT_ACTION_REQUEST_CHANNELS),
            CHAT_CMD_REQUEST_USERS_ROSTER => Some(CHAT_ACTION_REQUEST_USERS_ROSTER),
            _ => None,
        };
        if let Some(reply_action) = resp_action {
            let data = match active_arena {
                Some((arena, vehicle)) if action == CHAT_CMD_REQUEST_SYSTEM_CHANNELS => {
                    arena.chat.channels(vehicle.team)
                }
                _ => Value::list(Vec::new()),
            };
            return vec![RoutedAction {
                account_db_id: db_id,
                addr,
                instruction: PeerInstruction::ChatAction(ChatAction::reply(
                    request_id, reply_action, pickle::dumps(&data),
                )),
            }];
        }
        let Some((arena, sender)) = active_arena else {
            return Vec::new();
        };
        let (Ok(channel), Ok(target), Ok(sector), Ok(text)) =
            (r.i32(), r.i64(), r.i16(), r.string_bytes()) else {
                return Vec::new();
            };
        if action == CHAT_CMD_ATTACK_ENEMY && !i32::try_from(target).ok().is_some_and(|id| {
            arena.vehicle_by_id(id).is_some_and(|v| v.is_alive && v.team != sender.team)
                && is_in_observer_aoi(arena, sender.entity_id, sender.team, id)
        }) {
            return Vec::new();
        }
        let Some(message) = arena.chat.route(sender.team, channel, action, target, sector, &text) else {
            return Vec::new();
        };
        let data_pickle = pickle::dumps(&message.data);
        arena.vehicles.iter().filter(|v| !v.is_bot && !v.left_arena && message.reaches(v.team))
            .filter_map(|v| {
                let peer = self.peers.get(&v.account_db_id)?;
                if !matches!(peer.state, SessionState::InArena { space_id, .. } if space_id == arena.space_id) {
                    return None;
                }
                Some(RoutedAction {
                    account_db_id: v.account_db_id,
                    addr: peer.addr,
                    instruction: PeerInstruction::ChatAction(ChatAction {
                        request_id: if v.account_db_id == db_id { request_id } else { -1 },
                        action: message.action,
                        channel,
                        originator: sender.account_db_id,
                        nickname: sender.player_name.clone(),
                        data_pickle: data_pickle.clone(),
                    }),
                })
            }).collect()
    }

    pub(crate) fn on_cell_method(
        &mut self,
        addr: SocketAddr,
        entity_id: i32,
        method_idx: u8,
        data: &[u8],
    ) -> Vec<RoutedAction> {
        let Some(db_id) = self.addr_to_account.get(&addr).copied() else {
            return Vec::new();
        };
        let Some(rec) = self.peers.get(&db_id) else {
            return Vec::new();
        };
        let SessionState::InArena {
            space_id,
            avatar_id,
            vehicle_id,
        } = rec.state
        else {
            return Vec::new();
        };
        let Some(arena) = self.world.get_mut(space_id) else {
            return Vec::new();
        };

        // The client addresses its own Avatar's cell methods to entity 0.
        if entity_id == avatar_id || entity_id == 0 {
            let mut r = bw201::stream::BinaryReader::new(data);
            match method_idx {
                crate::protocol::cell_method::AVATAR_AUTO_AIM => {
                    if let Ok(target_id) = r.i32() {
                        if let Some(reason) = arena.set_auto_aim(vehicle_id, target_id) {
                            return vec![RoutedAction {
                                account_db_id: db_id, addr,
                                instruction: PeerInstruction::AvatarAutoAimLost { avatar_id, reason },
                            }];
                        }
                    }
                }
                crate::protocol::cell_method::AVATAR_MOVE_TO => {
                    if let (Ok(x), Ok(y), Ok(z)) = (r.f32(), r.f32(), r.f32()) {
                        arena.move_observer(vehicle_id, glam::Vec3::new(x, y, z));
                    }
                }
                crate::protocol::cell_method::AVATAR_BIND_TO_VEHICLE => {
                    if let (Ok(target), Ok(x), Ok(y), Ok(z)) = (r.i32(), r.f32(), r.f32(), r.f32()) {
                        arena.bind_observer(vehicle_id, target, glam::Vec3::new(x, y, z));
                        if target == vehicle_id {
                            return vec![RoutedAction {
                                account_db_id: db_id,
                                addr,
                                instruction: PeerInstruction::AvatarBoardVehicle {
                                    avatar_id,
                                    vehicle_id,
                                    space_id,
                                },
                            }];
                        }
                    }
                }
                _ => {}
            }
        } else if entity_id == vehicle_id {
            // Vehicle.cell methods (0..=4)
            match method_idx {
                crate::protocol::cell_method::VEHICLE_MOVE_WITH => {
                    let mut r = bw201::stream::BinaryReader::new(data);
                    if let Ok(flags) = r.u8() {
                        arena.set_vehicle_move_flags(db_id, flags);
                    }
                }
                crate::protocol::cell_method::VEHICLE_TRACK_POINT_WITH_GUN => {
                    let mut r = bw201::stream::BinaryReader::new(data);
                    if let (Ok(x), Ok(y), Ok(z)) = (r.f32(), r.f32(), r.f32()) {
                        arena.set_vehicle_target_point(db_id, glam::Vec3::new(x, y, z));
                    }
                }
                crate::protocol::cell_method::VEHICLE_STOP_TRACKING_WITH_GUN => {
                    if let Some(aim) = arena.aim_states.get_mut(&vehicle_id) {
                        aim.has_target_point = false;
                    }
                }
                crate::protocol::cell_method::VEHICLE_CHANGE_SETTING => {
                    let mut r = bw201::stream::BinaryReader::new(data);
                    if let (Ok(code), Ok(val)) = (r.u8(), r.i32()) {
                        if let Some((out_code, out_val)) =
                            arena.change_vehicle_setting(vehicle_id, code, val)
                        {
                            return vec![RoutedAction {
                                account_db_id: db_id,
                                addr,
                                instruction: PeerInstruction::AvatarVehicleSetting {
                                    avatar_id,
                                    code: out_code,
                                    value: out_val,
                                },
                            }];
                        }
                    }
                }
                _ => {}
            }
        }
        Vec::new()
    }

    pub(crate) fn on_avatar_method(
        &mut self,
        addr: SocketAddr,
        method_id: u8,
        data: &[u8],
        registry: &mut AccountRegistry,
        now: Instant,
    ) -> (Vec<RoutedAction>, bool) {
        let Some(db_id) = self.addr_to_account.get(&addr).copied() else {
            return (Vec::new(), false);
        };
        match method_id {
            crate::protocol::base_method::AVATAR_SET_DEVELOPMENT_FEATURE => {
                let mut reader = bw201::stream::BinaryReader::new(data);
                if let (Ok(feature), Ok(value)) = (reader.string(), reader.i32()) {
                    if let Some(arena) = self.world.arena_for_account_mut(db_id) {
                        if let Some(id) = arena.vehicle_for_account(db_id).map(|v| v.entity_id) {
                            arena.set_development_feature(id, &feature, value, self.config.dev_features);
                        }
                    }
                }
                (Vec::new(), false)
            }
            crate::protocol::base_method::AVATAR_VEHICLE_TELEPORT if self.config.dev_features => {
                let mut reader = bw201::stream::BinaryReader::new(data);
                if let (Ok(x), Ok(y), Ok(z), Ok(yaw)) =
                    (reader.f32(), reader.f32(), reader.f32(), reader.f32())
                {
                    if let Some(arena) = self.world.arena_for_account_mut(db_id) {
                        arena.apply_client_position(db_id, [x, y, z], yaw, 0.0, 0.0);
                    }
                }
                (Vec::new(), false)
            }
            crate::protocol::base_method::AVATAR_CONTROL_ANOTHER_VEHICLE => {
                let mut reader = bw201::stream::BinaryReader::new(data);
                if let (Ok(target), Ok(1 | 2)) = (reader.i32(), reader.u8()) {
                    if let Some(arena) = self.world.arena_for_account_mut(db_id) {
                        if let Some(observer) = arena.vehicle_for_account(db_id) {
                            arena.bind_observer(observer.entity_id, target, observer.position);
                        }
                    }
                }
                (Vec::new(), false)
            }
            crate::protocol::base_method::AVATAR_VEHICLE_USE_HORN => {
                let Some(start @ (0 | 1)) = data.first().copied() else {
                    return (Vec::new(), false);
                };
                let Some(arena) = self.world.arena_for_account(db_id) else {
                    return (Vec::new(), false);
                };
                let Some(sender) = arena.vehicle_for_account(db_id).filter(|v| v.is_alive) else {
                    return (Vec::new(), false);
                };
                let actions = arena.vehicles.iter().filter_map(|observer| {
                    let peer = self.peers.get(&observer.account_db_id)?;
                    let SessionState::InArena { avatar_id, space_id, .. } = peer.state else {
                        return None;
                    };
                    if space_id != arena.space_id
                        || !is_in_observer_aoi(arena, observer.entity_id, observer.team, sender.entity_id)
                    {
                        return None;
                    }
                    Some(RoutedAction {
                        account_db_id: observer.account_db_id,
                        addr: peer.addr,
                        instruction: PeerInstruction::AvatarHorn {
                            avatar_id, vehicle_id: sender.entity_id, start: start != 0,
                        },
                    })
                }).collect();
                (actions, false)
            }
            crate::protocol::base_method::AVATAR_VEHICLE_MOVE_WITH => {
                if let Ok(flags) = bw201::stream::BinaryReader::new(data).u8() {
                    if let Some(arena) = self.world.arena_for_account_mut(db_id) {
                        arena.set_vehicle_move_flags(db_id, flags);
                    }
                }
                (Vec::new(), false)
            }
            crate::protocol::base_method::AVATAR_VEHICLE_SHOOT => {
                if let Some(arena) = self.world.arena_for_account_mut(db_id) {
                    arena.request_fire(db_id);
                }
                (Vec::new(), false)
            }
            crate::protocol::base_method::AVATAR_VEHICLE_TRACK_POINT_WITH_GUN => {
                let mut r = bw201::stream::BinaryReader::new(data);
                if let (Ok(x), Ok(y), Ok(z)) = (r.f32(), r.f32(), r.f32()) {
                    if let Some(arena) = self.world.arena_for_account_mut(db_id) {
                        arena.set_vehicle_target_point(db_id, glam::Vec3::new(x, y, z));
                    }
                }
                (Vec::new(), false)
            }
            crate::protocol::base_method::AVATAR_VEHICLE_STOP_TRACKING_WITH_GUN => {
                if let Some(arena) = self.world.arena_for_account_mut(db_id) {
                    if let Some(v_id) = arena.vehicle_for_account(db_id).map(|v| v.entity_id) {
                        if let Some(aim) = arena.aim_states.get_mut(&v_id) {
                            aim.has_target_point = false;
                        }
                    }
                }
                (Vec::new(), false)
            }
            crate::protocol::base_method::AVATAR_VEHICLE_CHANGE_SETTING => {
                let mut r = bw201::stream::BinaryReader::new(data);
                if let (Ok(code), Ok(val)) = (r.u8(), r.i32()) {
                    let avatar_id = self.peers.get(&db_id).and_then(|rec| match rec.state {
                        SessionState::InArena { avatar_id, .. } => Some(avatar_id),
                        _ => None,
                    });
                    if let Some(arena) = self.world.arena_for_account_mut(db_id) {
                        if let Some(v_id) = arena.vehicle_for_account(db_id).map(|v| v.entity_id) {
                            if let (Some((out_code, out_val)), Some(avatar_id)) =
                                (arena.change_vehicle_setting(v_id, code, val), avatar_id)
                            {
                                return (
                                    vec![RoutedAction {
                                        account_db_id: db_id,
                                        addr,
                                        instruction: PeerInstruction::AvatarVehicleSetting {
                                            avatar_id,
                                            code: out_code,
                                            value: out_val,
                                        },
                                    }],
                                    false,
                                );
                            }
                        }
                    }
                }
                (Vec::new(), false)
            }
            crate::protocol::base_method::AVATAR_ADD_BOT_TO_ARENA => {
                let mut r = bw201::stream::BinaryReader::new(data);
                let (Ok(comp_descr), Ok(team @ (1 | 2)), Ok(name)) =
                    (r.string_bytes(), r.u8(), r.string()) else {
                        return (Vec::new(), false);
                    };
                let Some(arena) = self.world.arena_for_account_mut(db_id) else {
                    return (Vec::new(), false);
                };
                let training_host = arena.mode == BattleMode::Training
                    && self.prebattle.room(arena.prebattle_id)
                        .is_some_and(|room| room.settings.creator_db_id == db_id);
                let max_team_size = arena.map.def.max_players_in_team.min(15) as usize;
                let limit = if arena.mode == BattleMode::Random {
                    max_team_size.min(self.config.team_size)
                } else {
                    max_team_size
                };
                if (arena.finished.is_some() || !(self.config.dev_features || training_host))
                    || arena.vehicles.iter().filter(|v| v.team == team).count() >= limit
                {
                    return (Vec::new(), false);
                }
                let Ok(descr) = VehicleDescr::from_bytes(&comp_descr) else {
                    return (Vec::new(), false);
                };
                if registry.catalog().vehicle_def_by_id(descr.nation_id, u16::from(descr.type_id)).is_none() {
                    return (Vec::new(), false);
                }
                let bot_entity_id = self.alloc.alloc();
                let (_, added_pickle) = arena.add_bot(bot_entity_id, comp_descr, team, name, registry.catalog());
                let actions = arena.active_human_account_ids().into_iter().filter_map(|recipient| {
                    let peer = self.peers.get(&recipient)?;
                    let SessionState::InArena { avatar_id, .. } = peer.state else { return None };
                    Some(RoutedAction {
                        account_db_id: recipient,
                        addr: peer.addr,
                        instruction: PeerInstruction::AvatarUpdateArena {
                            avatar_id,
                            update_type: wot_battle::world::ARENA_UPDATE_VEHICLE_ADDED,
                            payload: added_pickle.clone(),
                        },
                    })
                }).collect();
                (actions, false)
            }
            crate::protocol::base_method::AVATAR_ON_CLIENT_READY => {
                let mut actions = Vec::new();
                let Some(arena) = self.world.arena_for_account_mut(db_id) else {
                    return (actions, false);
                };
                let updates = arena.mark_client_ready(db_id);
                // `__startGUI` just reset the reload state to -1 and built an empty
                // consumables panel: resend the reload state, the ammo and the loaded shell.
                if let Some(id) = arena.vehicle_for_account(db_id).map(|v| v.entity_id) {
                    if let Some(aim) = arena.aim_states.get_mut(&id) {
                        aim.reload_dirty = true;
                        aim.ammo_dirty = true;
                    }
                }
                let recipients = arena.active_human_account_ids();
                for (update_type, payload) in updates {
                    for &rid in &recipients {
                        if let Some(rec) = self.peers.get(&rid) {
                            if let SessionState::InArena { avatar_id, .. } = rec.state {
                                actions.push(RoutedAction {
                                    account_db_id: rid,
                                    addr: rec.addr,
                                    instruction: PeerInstruction::AvatarUpdateArena {
                                        avatar_id,
                                        update_type,
                                        payload: payload.clone(),
                                    },
                                });
                            }
                        }
                    }
                }
                (actions, false)
            }
            crate::protocol::base_method::AVATAR_LEAVE_ARENA => {
                let actions = self.leave_arena_to_hangar(db_id, registry, now);
                (actions, true)
            }
            _ => (Vec::new(), false),
        }
    }

    pub(crate) fn on_create_training(
        &mut self,
        addr: SocketAddr,
        data: &[u8],
        registry: &mut AccountRegistry,
    ) -> Vec<RoutedAction> {
        let Some(db_id) = self.addr_to_account.get(&addr).copied() else {
            return Vec::new();
        };
        let Some(rec) = self.peers.get(&db_id).cloned() else {
            return Vec::new();
        };
        if !matches!(rec.state, SessionState::Hangar) {
            return vec![RoutedAction {
                account_db_id: db_id,
                addr,
                instruction: PeerInstruction::OnPrebattleJoinFailure(JOIN_FAILURE_ACCOUNT_LOCK),
            }];
        }

        let mut r = bw201::stream::BinaryReader::new(data);
        let (Ok(arena_type_id), Ok(round_length), Ok(is_opened), Ok(comment)) =
            (r.i32(), r.i32(), r.u8(), r.string())
        else {
            return vec![RoutedAction {
                account_db_id: db_id,
                addr,
                instruction: PeerInstruction::OnPrebattleJoinFailure(JOIN_FAILURE_WRONG_ARGS),
            }];
        };

        let Some((veh_inv_id, veh_comp_descr)) =
            self.pick_account_vehicle(&rec.username, None, registry)
        else {
            return vec![RoutedAction {
                account_db_id: db_id,
                addr,
                instruction: PeerInstruction::OnPrebattleJoinFailure(JOIN_FAILURE_WRONG_ARGS),
            }];
        };
        let bot_comp_descr =
            default_bot_comp_descr(&registry.catalog_arc()).unwrap_or_else(|| veh_comp_descr.clone());

        let now_unix = AccountPayloads::unix_now();
        match self.prebattle.create_training(
            db_id,
            rec.player_id as i64,
            &rec.username,
            veh_inv_id,
            veh_comp_descr,
            arena_type_id,
            round_length,
            is_opened != 0,
            &comment,
            bot_comp_descr,
            self.maps.arenas(),
            now_unix,
        ) {
            Ok((_prb_id, events)) => self.collect_prebattle_events(events, registry, Instant::now()),
            Err(code) => vec![RoutedAction {
                account_db_id: db_id,
                addr,
                instruction: PeerInstruction::OnPrebattleJoinFailure(code),
            }],
        }
    }

    /// Handle battle/queue/prebattle `Account.doCmd*` commands; returns `None` if command belongs
    /// to standard hangar inventory/economy dispatch in `protocol.rs`.
    pub(crate) fn try_handle_hangar_do_cmd(
        &mut self,
        addr: SocketAddr,
        cmd: &DecodedDoCmd,
        registry: &mut AccountRegistry,
        now: Instant,
    ) -> Option<Vec<RoutedAction>> {
        let handled_id = matches!(
            cmd.command_id,
            cmd::CMD_ENQUEUE_FOR_ARENA
                | cmd::CMD_DEQUEUE
                | cmd::CMD_FORCE_QUEUE
                | cmd::CMD_REQ_QUEUE_INFO
                | cmd::CMD_CREATE_ARENA
                | cmd::CMD_JOIN_ARENA
                | cmd::CMD_PRB_JOIN
                | cmd::CMD_PRB_LEAVE
                | cmd::CMD_PRB_READY
                | cmd::CMD_PRB_NOT_READY
                | cmd::CMD_PRB_ASSIGN
                | cmd::CMD_PRB_CH_ARENA
                | cmd::CMD_PRB_CH_ROUND
                | cmd::CMD_PRB_OPEN
                | cmd::CMD_PRB_CH_COMMENT
                | cmd::CMD_PRB_CH_ARENAVOIP
                | cmd::CMD_PRB_TEAM_READY
                | cmd::CMD_PRB_TEAM_NOT_READY
                | cmd::CMD_PRB_KICK
                | cmd::CMD_REQ_PREBATTLES
                | cmd::CMD_REQ_PREBATTLES_BY_CREATOR
                | cmd::CMD_REQ_PREBATTLE_ROSTER
        );
        if !handled_id {
            return None;
        }

        let db_id = self.addr_to_account.get(&addr).copied()?;
        let rec = self.peers.get(&db_id)?.clone();

        let actions = match cmd.command_id {
            cmd::CMD_REQ_QUEUE_INFO => {
                let (randoms_wire, companies_wire) = self.queue.queue_info().encode_wire();
                vec![RoutedAction {
                    account_db_id: db_id,
                    addr,
                    instruction: PeerInstruction::ReceiveQueueInfo {
                        randoms_wire,
                        companies_wire,
                    },
                }]
            }
            cmd::CMD_ENQUEUE_FOR_ARENA => {
                self.handle_enqueue(db_id, addr, &rec, cmd, registry, now)
            }
            cmd::CMD_DEQUEUE => self.handle_dequeue(db_id, addr, &rec, registry),
            cmd::CMD_FORCE_QUEUE => self.handle_force_queue(db_id, addr, &rec, cmd, now, registry),
            cmd::CMD_CREATE_ARENA | cmd::CMD_JOIN_ARENA => {
                self.handle_dev_arena(&rec, cmd, registry, now)
            }
            cmd::CMD_REQ_PREBATTLES => {
                let (prb_type, sort_key, idle, start, end) = match &cmd.args {
                    DoCmdArgs::IntArr(v) if v.len() >= 5 => (
                        v[0],
                        v[1],
                        v[2] != 0,
                        v[3].max(0) as usize,
                        v[4].max(0) as usize,
                    ),
                    _ => (0, 0, false, 0, 50),
                };
                let raw_pickle = self
                    .prebattle
                    .query_prebattles_pickle(prb_type, sort_key, idle, start, end);
                vec![RoutedAction {
                    account_db_id: db_id,
                    addr,
                    instruction: PeerInstruction::StreamResource {
                        request_id: cmd.request_id,
                        raw_pickle,
                        send_cmd_response: cmd.request_id >= cmd::REQUEST_ID_UNRESERVED_MIN,
                    },
                }]
            }
            cmd::CMD_REQ_PREBATTLES_BY_CREATOR => {
                let (prb_type, idle, mask) = match &cmd.args {
                    DoCmdArgs::Int2Str(pt, idl, s) => (*pt, *idl != 0, s.as_str()),
                    _ => (0, false, ""),
                };
                let raw_pickle = self
                    .prebattle
                    .query_prebattles_by_creator_pickle(prb_type, idle, mask);
                vec![RoutedAction {
                    account_db_id: db_id,
                    addr,
                    instruction: PeerInstruction::StreamResource {
                        request_id: cmd.request_id,
                        raw_pickle,
                        send_cmd_response: cmd.request_id >= cmd::REQUEST_ID_UNRESERVED_MIN,
                    },
                }]
            }
            cmd::CMD_REQ_PREBATTLE_ROSTER => {
                let prb_id = match &cmd.args {
                    DoCmdArgs::Int3([id, _, _]) => *id,
                    _ => 0,
                };
                match self.prebattle.query_roster_pickle(prb_id) {
                    Ok(raw_pickle) => vec![RoutedAction {
                        account_db_id: db_id,
                        addr,
                        instruction: PeerInstruction::StreamResource {
                            request_id: cmd.request_id,
                            raw_pickle,
                            send_cmd_response: cmd.request_id >= cmd::REQUEST_ID_UNRESERVED_MIN,
                        },
                    }],
                    Err(code) => self.maybe_cmd_response(db_id, addr, cmd.request_id, code),
                }
            }
            cmd::CMD_PRB_JOIN => {
                let prb_id = match &cmd.args {
                    DoCmdArgs::Int3([id, _, _]) => *id,
                    _ => 0,
                };
                let Some((veh_inv_id, veh_comp_descr)) =
                    self.pick_account_vehicle(&rec.username, None, registry)
                else {
                    return Some(vec![RoutedAction {
                        account_db_id: db_id,
                        addr,
                        instruction: PeerInstruction::OnPrebattleJoinFailure(
                            JOIN_FAILURE_WRONG_ARGS,
                        ),
                    }]);
                };
                match self.prebattle.join(
                    db_id,
                    rec.player_id as i64,
                    &rec.username,
                    veh_inv_id,
                    veh_comp_descr,
                    prb_id,
                    AccountPayloads::unix_now(),
                ) {
                    Ok(events) => self.collect_prebattle_events(events, registry, now),
                    Err(code) => vec![RoutedAction {
                        account_db_id: db_id,
                        addr,
                        instruction: PeerInstruction::OnPrebattleJoinFailure(code),
                    }],
                }
            }
            cmd::CMD_PRB_LEAVE => match self.prebattle.leave(db_id) {
                Ok(events) => {
                    let mut out = self.unlock_locked_vehicle(db_id, registry);
                    out.extend(self.collect_prebattle_events(events, registry, now));
                    out.extend(self.maybe_cmd_response(
                        db_id,
                        addr,
                        cmd.request_id,
                        cmd::RES_SUCCESS,
                    ));
                    out
                }
                Err(code) => self.maybe_cmd_response(db_id, addr, cmd.request_id, code),
            },
            cmd::CMD_PRB_READY => self.handle_prb_ready(db_id, addr, &rec, cmd, registry, now),
            cmd::CMD_PRB_NOT_READY => {
                let (veh_inv_id, comp_descr) = self
                    .pick_account_vehicle(&rec.username, rec.locked_veh_inv_id, registry)
                    .unwrap_or((0, Vec::new()));
                match self.prebattle.set_ready(db_id, veh_inv_id, comp_descr, false) {
                    Ok(events) => {
                        let mut out = self.unlock_locked_vehicle(db_id, registry);
                        out.extend(self.collect_prebattle_events(events, registry, now));
                        out.extend(self.maybe_cmd_response(
                            db_id,
                            addr,
                            cmd.request_id,
                            cmd::RES_SUCCESS,
                        ));
                        out
                    }
                    Err(code) => self.maybe_cmd_response(db_id, addr, cmd.request_id, code),
                }
            }
            cmd::CMD_PRB_ASSIGN => match &cmd.args {
                DoCmdArgs::Int3([target_id, roster, _]) => {
                    let res = self.prebattle.assign(db_id, *target_id as i64, *roster as u8);
                    self.finish_prb_cmd(db_id, addr, cmd.request_id, res, registry, now)
                }
                _ => self.maybe_cmd_response(db_id, addr, cmd.request_id, cmd::RES_WRONG_ARGS),
            },
            cmd::CMD_PRB_CH_ARENA => match &cmd.args {
                DoCmdArgs::Int3([arena_id, _, _]) => {
                    let res = self
                        .prebattle
                        .change_arena(db_id, *arena_id, self.maps.arenas());
                    self.finish_prb_cmd(db_id, addr, cmd.request_id, res, registry, now)
                }
                _ => self.maybe_cmd_response(db_id, addr, cmd.request_id, cmd::RES_WRONG_ARGS),
            },
            cmd::CMD_PRB_CH_ROUND => match &cmd.args {
                DoCmdArgs::Int3([len, _, _]) => {
                    let res = self.prebattle.change_round_length(db_id, *len);
                    self.finish_prb_cmd(db_id, addr, cmd.request_id, res, registry, now)
                }
                _ => self.maybe_cmd_response(db_id, addr, cmd.request_id, cmd::RES_WRONG_ARGS),
            },
            cmd::CMD_PRB_OPEN => match &cmd.args {
                DoCmdArgs::Int3([opened, _, _]) => {
                    let res = self.prebattle.change_open_status(db_id, *opened != 0);
                    self.finish_prb_cmd(db_id, addr, cmd.request_id, res, registry, now)
                }
                _ => self.maybe_cmd_response(db_id, addr, cmd.request_id, cmd::RES_WRONG_ARGS),
            },
            cmd::CMD_PRB_CH_COMMENT => match &cmd.args {
                DoCmdArgs::Str(comment) => {
                    let res = self.prebattle.change_comment(db_id, comment);
                    self.finish_prb_cmd(db_id, addr, cmd.request_id, res, registry, now)
                }
                _ => self.maybe_cmd_response(db_id, addr, cmd.request_id, cmd::RES_WRONG_ARGS),
            },
            cmd::CMD_PRB_CH_ARENAVOIP => match &cmd.args {
                DoCmdArgs::Int3([voip, _, _]) => {
                    let res = self.prebattle.change_arena_voip(db_id, *voip);
                    self.finish_prb_cmd(db_id, addr, cmd.request_id, res, registry, now)
                }
                _ => self.maybe_cmd_response(db_id, addr, cmd.request_id, cmd::RES_WRONG_ARGS),
            },
            cmd::CMD_PRB_TEAM_READY => match &cmd.args {
                DoCmdArgs::Int3([team, force, _]) => {
                    let arenas = self.maps.arenas();
                    let res = self.prebattle.set_team_ready(
                        db_id,
                        *team as u8,
                        true,
                        *force != 0,
                        arenas,
                    );
                    self.finish_prb_team_cmd(db_id, addr, cmd.request_id, res, registry, now)
                }
                _ => self.maybe_cmd_response(db_id, addr, cmd.request_id, cmd::RES_WRONG_ARGS),
            },
            cmd::CMD_PRB_TEAM_NOT_READY => match &cmd.args {
                DoCmdArgs::Int3([team, _, _]) => {
                    let arenas = self.maps.arenas();
                    let res = self.prebattle.set_team_ready(
                        db_id,
                        *team as u8,
                        false,
                        false,
                        arenas,
                    );
                    self.finish_prb_team_cmd(db_id, addr, cmd.request_id, res, registry, now)
                }
                _ => self.maybe_cmd_response(db_id, addr, cmd.request_id, cmd::RES_WRONG_ARGS),
            },
            cmd::CMD_PRB_KICK => match &cmd.args {
                DoCmdArgs::Int3([target_id, _, _]) => {
                    let res = self.prebattle.kick(db_id, *target_id as i64);
                    self.finish_prb_cmd(db_id, addr, cmd.request_id, res, registry, now)
                }
                _ => self.maybe_cmd_response(db_id, addr, cmd.request_id, cmd::RES_WRONG_ARGS),
            },
            _ => Vec::new(),
        };

        Some(actions)
    }

    pub(crate) fn tick(
        &mut self,
        now: Instant,
        registry: &mut AccountRegistry,
    ) -> (
        Vec<RoutedAction>,
        Vec<(SocketAddr, i64, u8, Vec<VehicleState>)>,
        Vec<(SocketAddr, String)>,
    ) {
        let mut actions = Vec::new();

        // 1. Check switching timeouts (clients dropping mid Account<->Avatar switch)
        let timed_out_ids: Vec<(i64, SocketAddr, String)> = self
            .peers
            .values()
            .filter_map(|p| match &p.state {
                SessionState::SwitchingToArena { started_at, .. }
                | SessionState::SwitchingToHangar { started_at, .. }
                    if now.saturating_duration_since(*started_at) >= self.config.switching_timeout =>
                {
                    Some((p.account_db_id, p.addr, p.username.trim().to_ascii_lowercase()))
                }
                _ => None,
            })
            .collect();

        let mut expired_peers = Vec::new();
        for (db_id, addr, user_key) in timed_out_ids {
            tracing::warn!(account_db_id = db_id, %addr, "switching timeout elapsed; cleaning up session");
            actions.extend(self.cleanup_account_internal(db_id, registry));
            expired_peers.push((addr, user_key));
        }

        // The client times movement by tick byte, so arenas advance in whole update ticks only.
        // Wake-up jitter rounds to the nearest tick; the remainder and any backlog carry over.
        let previous = self.last_step_at.unwrap_or(now);
        let due_ms = u64::try_from(now.saturating_duration_since(previous).as_millis()).unwrap_or(u64::MAX);
        let ticks = (due_ms.saturating_add(SERVER_TICK_MS / 2) / SERVER_TICK_MS).min(MAX_TICKS_PER_WAKEUP);
        let advanced = Duration::from_millis(ticks * SERVER_TICK_MS);
        self.last_step_at = Some(previous + advanced);
        if ticks > 0 {
            self.world.advance_all(advanced);
        }

        // Launch queued matches after advancing the arenas that already existed.
        let catalog = registry.catalog_arc();
        let launches = self.queue.tick(now, self.maps.arenas(), &catalog);
        for spec in launches {
            actions.extend(self.launch_arena(spec, registry, now));
        }

        let mut snapshots = Vec::new();
        for space_id in self.world.active_space_ids() {
            let Some(arena) = self.world.get_mut(space_id) else {
                continue;
            };

            let human_recipients: Vec<(i64, SocketAddr, i32, u8, i32)> = arena
                .vehicles
                .iter()
                .filter(|v| !v.is_bot && !v.left_arena)
                .filter_map(|v| {
                    let rec = self.peers.get(&v.account_db_id)?;
                    if let SessionState::InArena { avatar_id, .. } = rec.state {
                        Some((v.account_db_id, rec.addr, avatar_id, v.team, v.entity_id))
                    } else {
                        None
                    }
                })
                .collect();

            // 1. AoI Changes & Minimap Positions from visibility step
            for &(obs_db_id, obs_addr, obs_avatar_id, _obs_team, obs_veh_id) in &human_recipients {
                if let Some(changes) = arena.last_visibility.aoi_changes.get(&obs_veh_id) {
                    for ch in changes {
                        match ch {
                            wot_battle::sim::visibility::AoIChange::Enter(enemy_id) => {
                                actions.push(RoutedAction {
                                    account_db_id: obs_db_id,
                                    addr: obs_addr,
                                    instruction: PeerInstruction::EnterAoI(*enemy_id),
                                });
                            }
                            wot_battle::sim::visibility::AoIChange::Leave(enemy_id) => {
                                actions.push(RoutedAction {
                                    account_db_id: obs_db_id,
                                    addr: obs_addr,
                                    instruction: PeerInstruction::LeaveAoI(*enemy_id),
                                });
                            }
                        }
                    }
                }
                if let Some(frame) = arena.last_visibility.minimap_frames.get(&obs_veh_id) {
                    if !frame.indices.is_empty() {
                        actions.push(RoutedAction {
                            account_db_id: obs_db_id,
                            addr: obs_addr,
                            instruction: PeerInstruction::AvatarPositions {
                                avatar_id: obs_avatar_id,
                                indices: frame.indices.clone(),
                                positions: frame.positions_xz.clone(),
                            },
                        });
                    }
                }
            }

            // 2. Shots fired this tick (immediate muzzle flash & tracer)
            for shot in &arena.last_shots {
                for &(obs_db_id, obs_addr, obs_avatar_id, obs_team, obs_veh_id) in &human_recipients {
                    actions.push(RoutedAction {
                        account_db_id: obs_db_id,
                        addr: obs_addr,
                        instruction: PeerInstruction::AvatarShowTracer {
                            avatar_id: obs_avatar_id,
                            shooter_id: shot.attacker_id,
                            shot_id: shot.shot_id as i32,
                            effects_index: 0,
                            ref_start_point: [shot.muzzle_pos.x, shot.muzzle_pos.y, shot.muzzle_pos.z],
                            velocity: [
                                shot.shot_dir.x * shot.speed,
                                shot.shot_dir.y * shot.speed,
                                shot.shot_dir.z * shot.speed,
                            ],
                            gravity: shot.gravity,
                        },
                    });
                    if is_in_observer_aoi(arena, obs_veh_id, obs_team, shot.attacker_id) {
                        actions.push(RoutedAction {
                            account_db_id: obs_db_id,
                            addr: obs_addr,
                            instruction: PeerInstruction::VehicleShowShooting {
                                vehicle_id: shot.attacker_id,
                            },
                        });
                    }
                }
            }

            // 3. Driving events (pushed vehicles and ramming damages)
            for &(pushed_id, (px, pz)) in &arena.last_driving_events.pushed_vehicles {
                for &(obs_db_id, obs_addr, _obs_avatar_id, obs_team, obs_veh_id) in
                    &human_recipients
                {
                    if is_in_observer_aoi(arena, obs_veh_id, obs_team, pushed_id) {
                        actions.push(RoutedAction {
                            account_db_id: obs_db_id,
                            addr: obs_addr,
                            instruction: PeerInstruction::VehicleOnPushed {
                                vehicle_id: pushed_id,
                                x: px,
                                z: pz,
                            },
                        });
                    }
                }
            }
            for &(rammed_id, dmg) in &arena.last_driving_events.ramming_damages {
                if dmg > 0 {
                    if let Some(rammed_veh) =
                        arena.vehicles.iter().find(|v| v.entity_id == rammed_id)
                    {
                        let props = encode_vehicle_update_props(rammed_veh);
                        for &(obs_db_id, obs_addr, obs_avatar_id, obs_team, obs_veh_id) in
                            &human_recipients
                        {
                            if is_in_observer_aoi(arena, obs_veh_id, obs_team, rammed_id) {
                                actions.push(RoutedAction {
                                    account_db_id: obs_db_id,
                                    addr: obs_addr,
                                    instruction: PeerInstruction::UpdateEntity {
                                        entity_id: rammed_id,
                                        properties: props.clone(),
                                    },
                                });
                            }
                            if rammed_id == obs_veh_id {
                                actions.push(RoutedAction {
                                    account_db_id: obs_db_id,
                                    addr: obs_addr,
                                    instruction: PeerInstruction::AvatarHealth {
                                        avatar_id: obs_avatar_id,
                                        health: rammed_veh.health,
                                        is_crew_active: rammed_veh.is_crew_active,
                                    },
                                });
                            }
                        }
                    }
                }
            }

            // 4. Impact outcomes (stopTracer, explodeProjectile, AoI-gated hit damage & splash)
            for outcome in arena.last_impact_outcomes.iter().filter(|outcome| outcome.ends_flight) {
                for &(obs_db_id, obs_addr, obs_avatar_id, obs_team, obs_veh_id) in &human_recipients {
                    actions.push(RoutedAction {
                        account_db_id: obs_db_id,
                        addr: obs_addr,
                        instruction: PeerInstruction::AvatarStopTracer {
                            avatar_id: obs_avatar_id,
                            shot_id: outcome.shot_id as i32,
                            end_point: [
                                outcome.hit_point.x,
                                outcome.hit_point.y,
                                outcome.hit_point.z,
                            ],
                        },
                    });
                    if outcome.struck_entity_id.is_none() || !outcome.splash_hits.is_empty() {
                        actions.push(RoutedAction {
                            account_db_id: obs_db_id,
                            addr: obs_addr,
                            instruction: PeerInstruction::AvatarExplodeProjectile {
                                avatar_id: obs_avatar_id,
                                shot_id: outcome.shot_id as i32,
                                effects_index: 0,
                                effect_material_index: outcome.effect_mat_idx as u8,
                                end_point: [
                                    outcome.hit_point.x,
                                    outcome.hit_point.y,
                                    outcome.hit_point.z,
                                ],
                                velocity_dir: [
                                    outcome.normal.x,
                                    outcome.normal.y,
                                    outcome.normal.z,
                                ],
                            },
                        });
                    }
                    if let Some(struck_id) = outcome.struck_entity_id {
                        if is_in_observer_aoi(arena, obs_veh_id, obs_team, struck_id) {
                            actions.push(RoutedAction {
                                account_db_id: obs_db_id,
                                addr: obs_addr,
                                instruction: PeerInstruction::VehicleShowDamageFromShot {
                                    vehicle_id: struck_id,
                                    attacker_id: outcome.attacker_id,
                                    points: vec![outcome.encoded_segment],
                                    effects_index: 0,
                                },
                            });
                            if let Some(struck_veh) =
                                arena.vehicles.iter().find(|v| v.entity_id == struck_id)
                            {
                                actions.push(RoutedAction {
                                    account_db_id: obs_db_id,
                                    addr: obs_addr,
                                    instruction: PeerInstruction::UpdateEntity {
                                        entity_id: struck_id,
                                        properties: encode_vehicle_update_props(struck_veh),
                                    },
                                });
                            }
                        }
                        if struck_id == obs_veh_id {
                            if let Some(struck_veh) =
                                arena.vehicles.iter().find(|v| v.entity_id == struck_id)
                            {
                                actions.push(RoutedAction {
                                    account_db_id: obs_db_id,
                                    addr: obs_addr,
                                    instruction: PeerInstruction::AvatarHealth {
                                        avatar_id: obs_avatar_id,
                                        health: struck_veh.health,
                                        is_crew_active: struck_veh.is_crew_active,
                                    },
                                });
                            }
                        }
                    }
                    for &(splash_id, splash_dmg) in &outcome.splash_hits {
                        if is_in_observer_aoi(arena, obs_veh_id, obs_team, splash_id) {
                            actions.push(RoutedAction {
                                account_db_id: obs_db_id,
                                addr: obs_addr,
                                instruction: PeerInstruction::VehicleShowDamageFromExplosion {
                                    vehicle_id: splash_id,
                                    attacker_id: outcome.attacker_id,
                                    center: [
                                        outcome.hit_point.x,
                                        outcome.hit_point.y,
                                        outcome.hit_point.z,
                                    ],
                                    effects_index: 0,
                                },
                            });
                            if splash_dmg > 0 {
                                if let Some(splash_veh) =
                                    arena.vehicles.iter().find(|v| v.entity_id == splash_id)
                                {
                                    actions.push(RoutedAction {
                                        account_db_id: obs_db_id,
                                        addr: obs_addr,
                                        instruction: PeerInstruction::UpdateEntity {
                                            entity_id: splash_id,
                                            properties: encode_vehicle_update_props(splash_veh),
                                        },
                                    });
                                }
                            }
                        }
                        if splash_id == obs_veh_id && splash_dmg > 0 {
                            if let Some(splash_veh) =
                                arena.vehicles.iter().find(|v| v.entity_id == splash_id)
                            {
                                actions.push(RoutedAction {
                                    account_db_id: obs_db_id,
                                    addr: obs_addr,
                                    instruction: PeerInstruction::AvatarHealth {
                                        avatar_id: obs_avatar_id,
                                        health: splash_veh.health,
                                        is_crew_active: splash_veh.is_crew_active,
                                    },
                                });
                            }
                        }
                    }
                }
            }

            // 5. Damage state changes (fire burn ticks, extinguish, crew state changes)
            for &changed_id in &arena.last_state_changed_vehicles {
                if let Some(changed_veh) =
                    arena.vehicles.iter().find(|v| v.entity_id == changed_id)
                {
                    let props = encode_vehicle_update_props(changed_veh);
                    for &(obs_db_id, obs_addr, obs_avatar_id, obs_team, obs_veh_id) in
                        &human_recipients
                    {
                        if is_in_observer_aoi(arena, obs_veh_id, obs_team, changed_id) {
                            actions.push(RoutedAction {
                                account_db_id: obs_db_id,
                                addr: obs_addr,
                                instruction: PeerInstruction::UpdateEntity {
                                    entity_id: changed_id,
                                    properties: props.clone(),
                                },
                            });
                        }
                        if changed_id == obs_veh_id {
                            actions.push(RoutedAction {
                                account_db_id: obs_db_id,
                                addr: obs_addr,
                                instruction: PeerInstruction::AvatarHealth {
                                    avatar_id: obs_avatar_id,
                                    health: changed_veh.health,
                                    is_crew_active: changed_veh.is_crew_active,
                                },
                            });
                        }
                    }
                }
            }

            // 6. Destructible chunk deltas
            if !arena.last_destructible_deltas.is_empty() {
                let mut affected_chunks: Vec<u16> = arena
                    .last_destructible_deltas
                    .iter()
                    .map(|d| d.chunk_id)
                    .collect();
                affected_chunks.sort_unstable();
                affected_chunks.dedup();
                for chunk_id in affected_chunks {
                    if let Some(chunk) = arena.destructibles.chunks.get(&chunk_id) {
                        let props = encode_destructible_chunk_update_props(chunk);
                        let entity_id =
                            wot_battle::sim::destructibles::chunk_entity_id(chunk_id);
                        for &(obs_db_id, obs_addr, _, _, _) in &human_recipients {
                            actions.push(RoutedAction {
                                account_db_id: obs_db_id,
                                addr: obs_addr,
                                instruction: PeerInstruction::UpdateEntity {
                                    entity_id,
                                    properties: props.clone(),
                                },
                            });
                        }
                    }
                }
            }

            // 7. Per-vehicle feedback: targeting snapshots, reload time, dirty-gated ammo
            for &(obs_db_id, obs_addr, obs_avatar_id, _obs_team, obs_veh_id) in &human_recipients {
                if let Some(snapshot) = arena.targeting_snapshots.get(&obs_veh_id) {
                    actions.push(RoutedAction {
                        account_db_id: obs_db_id,
                        addr: obs_addr,
                        instruction: PeerInstruction::AvatarTargetingInfo {
                            avatar_id: obs_avatar_id,
                            turret_yaw: snapshot.turret_yaw,
                            gun_pitch: snapshot.gun_pitch,
                            max_turret_rot_speed: snapshot.max_turret_rot_speed,
                            max_gun_rot_speed: snapshot.max_gun_rot_speed,
                            shot_disp_multiplier_factor: snapshot.shot_disp_multiplier_factor,
                            aiming_time: snapshot.aiming_time,
                        },
                    });
                    if arena.server_marker_enabled(obs_veh_id) {
                        actions.push(RoutedAction {
                        account_db_id: obs_db_id,
                        addr: obs_addr,
                        instruction: PeerInstruction::AvatarGunMarker {
                            avatar_id: obs_avatar_id,
                            marker_pos: [
                                snapshot.marker_pos.x,
                                snapshot.marker_pos.y,
                                snapshot.marker_pos.z,
                            ],
                            marker_dir: [
                                snapshot.marker_dir.x,
                                snapshot.marker_dir.y,
                                snapshot.marker_dir.z,
                            ],
                            disp_angle: snapshot.dispersion_angle,
                        },
                        });
                    }
                }
                // The client's battle window, which stores ammo, exists after `onClientReady`.
                let battle_gui_ready = arena
                    .vehicle_by_id(obs_veh_id)
                    .is_some_and(|v| v.is_avatar_ready);
                if let Some(aim) = arena.aim_states.get_mut(&obs_veh_id) {
                    if std::mem::take(&mut aim.reload_dirty) {
                        actions.push(RoutedAction {
                            account_db_id: obs_db_id,
                            addr: obs_addr,
                            instruction: PeerInstruction::AvatarReloadTime {
                                avatar_id: obs_avatar_id,
                                time_left: aim.reload_left,
                            },
                        });
                    }
                    if aim.ammo_dirty && battle_gui_ready {
                        if aim.ammo_counts.is_empty() {
                            actions.push(RoutedAction {
                                account_db_id: obs_db_id,
                                addr: obs_addr,
                                instruction: PeerInstruction::AvatarAmmo {
                                    avatar_id: obs_avatar_id,
                                    compact_descr: aim.current_shell_cd,
                                    quantity: 50,
                                    time_remaining: 0,
                                },
                            });
                        } else {
                            for &(cd, count) in &aim.ammo_counts {
                                actions.push(RoutedAction {
                                    account_db_id: obs_db_id,
                                    addr: obs_addr,
                                    instruction: PeerInstruction::AvatarAmmo {
                                        avatar_id: obs_avatar_id,
                                        compact_descr: cd,
                                        quantity: count.min(u16::MAX as u32) as u16,
                                        time_remaining: 0,
                                    },
                                });
                            }
                        }
                        // `Avatar.shoot` needs `__currShellsIdx`, which only
                        // `updateVehicleSetting(CURRENT_SHELLS)` sets, after the shell is listed.
                        if aim.current_shell_cd != 0 {
                            let next = if aim.next_shell_cd != 0 {
                                aim.next_shell_cd
                            } else {
                                aim.current_shell_cd
                            };
                            for (code, value) in [(0, aim.current_shell_cd), (1, next)] {
                                actions.push(RoutedAction {
                                    account_db_id: obs_db_id,
                                    addr: obs_addr,
                                    instruction: PeerInstruction::AvatarVehicleSetting {
                                        avatar_id: obs_avatar_id,
                                        code,
                                        value,
                                    },
                                });
                            }
                        }
                        aim.ammo_dirty = false;
                    }
                }
            }

            // 8. Damage events (crits & fire started)
            for dmg_ev in &arena.last_damage_events {
                let is_fire_start = matches!(
                    dmg_ev.code,
                    wot_battle::sim::DamageInfoCode::Fire
                        | wot_battle::sim::DamageInfoCode::DeviceStartedFireAtShot
                        | wot_battle::sim::DamageInfoCode::DeviceStartedFireAtRamming
                );
                for &(obs_db_id, obs_addr, obs_avatar_id, obs_team, obs_veh_id) in
                    &human_recipients
                {
                    if obs_veh_id == dmg_ev.target_id {
                        actions.push(RoutedAction {
                            account_db_id: obs_db_id,
                            addr: obs_addr,
                            instruction: PeerInstruction::AvatarDamageInfo {
                                avatar_id: obs_avatar_id,
                                code: dmg_ev.code as u8,
                                extra_index: dmg_ev.extra_index,
                                entity_id: dmg_ev.attacker_id,
                            },
                        });
                    }
                    if is_fire_start
                        && is_in_observer_aoi(arena, obs_veh_id, obs_team, dmg_ev.target_id)
                    {
                        actions.push(RoutedAction {
                            account_db_id: obs_db_id,
                            addr: obs_addr,
                            instruction: PeerInstruction::VehicleShowFireStarted {
                                vehicle_id: dmg_ev.target_id,
                                attacker_id: dmg_ev.attacker_id,
                            },
                        });
                        if let Some(target_veh) =
                            arena.vehicles.iter().find(|v| v.entity_id == dmg_ev.target_id)
                        {
                            actions.push(RoutedAction {
                                account_db_id: obs_db_id,
                                addr: obs_addr,
                                instruction: PeerInstruction::UpdateEntity {
                                    entity_id: dmg_ev.target_id,
                                    properties: encode_vehicle_update_props(target_veh),
                                },
                            });
                        }
                    }
                }
            }

            // 9. Unreliable snapshots (filtered by AoI / spotted status), once per update tick:
            // the byte names the arena instant whose state they carry.
            let game_ticks = arena_game_ticks(arena);
            let fresh_tick = arena.last_movement_tick.replace(game_ticks) != Some(game_ticks);
            let tick_byte = game_ticks as u8;
            for &(obs_db_id, obs_addr, _, obs_team, obs_veh_id) in
                human_recipients.iter().filter(|_| fresh_tick)
            {
                let visible_vehicles: Vec<VehicleState> = arena
                    .vehicles
                    .iter()
                    .filter(|v| !v.left_arena)
                    .filter(|v| is_in_observer_aoi(arena, obs_veh_id, obs_team, v.entity_id))
                    .cloned()
                    .collect();
                snapshots.push((obs_addr, obs_db_id, tick_byte, visible_vehicles));
            }

            for (vehicle_id, reason) in arena.auto_aim_losses.drain(..) {
                if let Some(&(account_db_id, addr, avatar_id, _, _)) =
                    human_recipients.iter().find(|recipient| recipient.4 == vehicle_id)
                {
                    actions.push(RoutedAction {
                        account_db_id, addr,
                        instruction: PeerInstruction::AvatarAutoAimLost { avatar_id, reason },
                    });
                }
            }
            for (update_type, payload) in arena.arena_updates.drain(..) {
                for &(account_db_id, addr, avatar_id, _, _) in &human_recipients {
                    actions.push(RoutedAction {
                        account_db_id,
                        addr,
                        instruction: PeerInstruction::AvatarUpdateArena {
                            avatar_id,
                            update_type,
                            payload: payload.clone(),
                        },
                    });
                }
            }
            arena.last_shots.clear();
            arena.pending_external_shots.clear();
            arena.last_impact_outcomes.clear();
            arena.last_damage_events.clear();
            arena.pending_external_damage_events.clear();
            arena.last_state_changed_vehicles.clear();
            arena.pending_external_state_changed_vehicles.clear();
            arena.last_destructible_deltas.clear();
            arena.last_driving_events = Default::default();
            arena.last_visibility.aoi_changes.clear();
            arena.last_visibility.minimap_frames.clear();
        }

        for space_id in self.world.active_space_ids() {
            self.finalize_arena(space_id, registry, now, &mut actions);
        }
        snapshots.retain(|(_, db_id, _, _)| self.peers.get(db_id)
            .is_some_and(|peer| matches!(peer.state, SessionState::InArena { .. })));
        (actions, snapshots, expired_peers)
    }

    pub(crate) fn handle_disconnect_addr(
        &mut self,
        addr: SocketAddr,
        registry: &mut AccountRegistry,
    ) -> Vec<RoutedAction> {
        let Some(db_id) = self.addr_to_account.get(&addr).copied() else {
            return Vec::new();
        };
        self.cleanup_account_internal(db_id, registry)
    }

    fn protected_locks(&self, account_db_id: i64) -> Vec<i64> {
        self.settlements.iter().filter_map(|(&space_id, settlement)| {
            if !settlement.players.contains_key(&account_db_id) {
                return None;
            }
            let arena = self.world.get(space_id)?;
            (arena.mode == BattleMode::Random)
                .then(|| arena.vehicle_for_account(account_db_id).map(|v| v.veh_inv_id)).flatten()
        }).collect()
    }

    fn handle_dev_arena(
        &mut self,
        peer: &PeerBattleRecord,
        request: &DecodedDoCmd,
        registry: &mut AccountRegistry,
        now: Instant,
    ) -> Vec<RoutedAction> {
        let db_id = peer.account_db_id;
        let addr = peer.addr;
        let reject = |code| vec![RoutedAction {
            account_db_id: db_id, addr, instruction: PeerInstruction::OnArenaJoinFailure(code),
        }];
        if !self.config.dev_features || !matches!(peer.state, SessionState::Hangar)
            || self.queue.contains(db_id) || self.prebattle.room_for_account(db_id).is_some()
            || self.world.arena_for_account(db_id).is_some()
        {
            let mut actions = self.maybe_cmd_response(db_id, addr, request.request_id, cmd::RES_FAILURE);
            actions.extend(reject(JOIN_FAILURE_ACCOUNT_LOCK));
            return actions;
        }
        let DoCmdArgs::Int3([first, second, third]) = &request.args else {
            return reject(JOIN_FAILURE_WRONG_ARGS);
        };
        let create = request.command_id == cmd::CMD_CREATE_ARENA;
        let requested_vehicle = (!create && *third > 0).then_some(i64::from(*third));
        let Some((vehicle_id, comp_descr)) = self.pick_account_vehicle(&peer.username, requested_vehicle, registry) else {
            return reject(JOIN_FAILURE_ACCOUNT_LOCK);
        };
        let Some(profile) = registry.get_profile(&peer.username) else {
            return reject(JOIN_FAILURE_ACCOUNT_LOCK);
        };
        let vehicle = &profile.vehicles[&vehicle_id];
        if vehicle.lock != LOCK_REASON_NONE {
            return reject(JOIN_FAILURE_ACCOUNT_LOCK);
        }
        let team = if create { 1 } else { *second };
        let Ok(team @ (1 | 2)) = u8::try_from(team) else {
            return reject(JOIN_FAILURE_WRONG_ARGS);
        };
        let player = ParticipantSpec {
            account_db_id: db_id,
            veh_inv_id: vehicle_id,
            veh_comp_descr: comp_descr,
            team,
            player_name: peer.username.clone(),
            shells: vehicle.shells.iter().map(|&(cd, count)| (cd, count.max(0) as u32)).collect(),
            eqs: vehicle.eqs.iter().map(|&cd| cd as u16).collect(),
            crew: vehicle.crew.iter().flatten().filter_map(|id| {
                let entry = profile.tankmen.get(id)?;
                wot065_data::descr::TankmanDescr::from_bytes(&entry.comp_descr).ok()
            }).collect(),
            ..Default::default()
        };
        if create {
            let Ok(round_length_secs @ 1..=3600) = u32::try_from(*first) else {
                return reject(JOIN_FAILURE_WRONG_ARGS);
            };
            return self.launch_arena(ArenaLaunchSpec {
                arena_type_id: *third,
                arena_gui_type: wot_battle::lobby::ARENA_GUI_TYPE_TRAINING,
                arena_bonus_type: wot_battle::lobby::ARENA_BONUS_TYPE_TRAINING,
                mode: BattleMode::Training,
                round_length_secs,
                prebattle_id: 0,
                participants: vec![player],
            }, registry, now);
        }

        let space_id = *first;
        let Some(arena) = self.world.get(space_id).filter(|arena| {
            arena.mode == BattleMode::Training && arena.prebattle_id == 0 && arena.finished.is_none()
        }) else {
            return reject(JOIN_FAILURE_NOT_FOUND);
        };
        if arena.vehicles.iter().filter(|v| v.team == team && !v.left_arena).count()
            >= arena.map.def.max_players_in_team as usize
        {
            return reject(wot_battle::lobby::JOIN_FAILURE_TEAM_IS_FULL);
        }
        let arena_type_id = arena.map.def.id;
        let arena_gui_type = arena.arena_gui_type;
        let recipients = arena.active_human_account_ids();
        let lock = match registry.set_vehicle_lock(&peer.username, vehicle_id, LOCK_REASON_ON_ARENA) {
            Ok(lock) => lock,
            Err(_) => return reject(JOIN_FAILURE_ACCOUNT_LOCK),
        };
        let entity_id = self.alloc.alloc();
        let Some(payload) = self.world.join_arena(space_id, entity_id, player, registry.catalog()) else {
            self.alloc.release(entity_id);
            let _ = registry.set_vehicle_lock(&peer.username, vehicle_id, LOCK_REASON_NONE);
            return reject(JOIN_FAILURE_NOT_FOUND);
        };
        if let Some(settlement) = self.settlements.get_mut(&space_id) {
            settlement.players.insert(db_id, peer.username.clone());
        }
        let mut actions = Vec::new();
        for receiver in recipients {
            if let Some(receiver) = self.peers.get(&receiver) {
                if let SessionState::InArena { avatar_id, .. } = receiver.state {
                    actions.push(RoutedAction {
                        account_db_id: receiver.account_db_id, addr: receiver.addr,
                        instruction: PeerInstruction::AvatarUpdateArena {
                            avatar_id, update_type: wot_battle::world::ARENA_UPDATE_VEHICLE_ADDED, payload: payload.clone(),
                        },
                    });
                }
            }
        }
        if let Some((_, diff)) = lock {
            actions.push(RoutedAction { account_db_id: db_id, addr, instruction: PeerInstruction::AccountUpdate(diff) });
        }
        let avatar_id = self.alloc.alloc();
        if let Some(peer) = self.peers.get_mut(&db_id) {
            peer.locked_veh_inv_id = Some(vehicle_id);
            peer.state = SessionState::SwitchingToArena {
                space_id, arena_type_id, arena_gui_type, avatar_id, started_at: now,
            };
        }
        actions.push(RoutedAction { account_db_id: db_id, addr, instruction: PeerInstruction::StartArenaSwitch });
        actions
    }

    fn finalize_arena(
        &mut self,
        space_id: i32,
        registry: &mut AccountRegistry,
        now: Instant,
        actions: &mut Vec<RoutedAction>,
    ) {
        let Some(arena) = self.world.get(space_id) else { return };
        let Some(settlement) = self.settlements.get_mut(&space_id) else { return };
        if arena.finished.is_none() || now < settlement.retry_at {
            return;
        }
        let now_utc = AccountPayloads::unix_now() as u32;
        let mut completed = Vec::new();
        for (&db_id, username) in &settlement.players {
            let Some((vehicle, (mut results, outcome))) = arena.vehicle_for_account(db_id)
                .and_then(|vehicle| {
                    let profile = registry.get_profile(username)?;
                    personal_results(arena, vehicle, profile, settlement.unique_id, settlement.created_at)
                        .map(|results| (vehicle, results))
                })
            else {
                tracing::error!(space_id, db_id, "could not construct battle results");
                continue;
            };
            let diff = if results.regular {
                match registry.settle_battle(username, &outcome, &mut results, now_utc) {
                    Ok(diff) => diff,
                    Err(error) => {
                        tracing::error!(space_id, db_id, %error, "battle settlement will retry");
                        continue;
                    }
                }
            } else {
                None
            };
            completed.push(db_id);
            let Some(peer) = self.peers.get(&db_id) else { continue };
            let mut send = |instruction| actions.push(RoutedAction {
                account_db_id: db_id, addr: peer.addr, instruction,
            });
            match peer.state {
                SessionState::InArena { space_id: peer_space, avatar_id, .. } if peer_space == space_id => {
                    send(PeerInstruction::BattleResults {
                        avatar_id, vehicle_inv_id: vehicle.veh_inv_id as i32, wire: results.to_wire(),
                    });
                }
                SessionState::Hangar => {
                    if let Some(diff) = diff {
                        send(PeerInstruction::AccountUpdate(diff));
                        send(PeerInstruction::ResyncDossiers);
                    }
                    for message in pending_battle_messages(registry, username) {
                        send(PeerInstruction::ChatAction(message));
                    }
                }
                _ => {}
            }
        }
        for db_id in completed {
            settlement.players.remove(&db_id);
        }
        if !settlement.players.is_empty() {
            settlement.retry_at = now + Duration::from_secs(1);
            return;
        }
        if arena.server_time < arena.period_end_time {
            return;
        }

        let peers: Vec<_> = arena.active_human_account_ids().into_iter().filter_map(|db_id| {
            let peer = self.peers.get(&db_id)?;
            matches!(peer.state, SessionState::InArena { space_id: peer_space, .. } if peer_space == space_id)
                .then_some((db_id, peer.addr))
        }).collect();
        let prebattle_id = arena.prebattle_id;
        for (db_id, addr) in peers {
            actions.extend(self.leave_arena_to_hangar(db_id, registry, now));
            actions.push(RoutedAction { account_db_id: db_id, addr, instruction: PeerInstruction::FinishedArenaSwitch });
        }
        self.settlements.remove(&space_id);
        for id in self.world.remove_arena(space_id) {
            self.alloc.release(id);
        }
        if prebattle_id > 0 && self.prebattle.room(prebattle_id).is_some_and(|room| room.in_battle) {
            let events = self.prebattle.notify_arena_finished(prebattle_id);
            actions.extend(self.collect_prebattle_events(events, registry, now));
        }
    }

    fn cleanup_account_internal(
        &mut self,
        account_db_id: i64,
        registry: &mut AccountRegistry,
    ) -> Vec<RoutedAction> {
        let mut actions = Vec::new();
        let Some(rec) = self.peers.remove(&account_db_id) else {
            return actions;
        };
        self.addr_to_account.remove(&rec.addr);

        // 1. Dequeue if queued
        self.queue.dequeue(account_db_id);

        let now = Instant::now();

        // A disconnected Random participant can resume the same parked vehicle.
        let finished_arena_prb_id = if matches!(rec.state, SessionState::InArena { .. })
            && self.world.arena_for_account(account_db_id).is_some_and(|a| {
                a.mode == BattleMode::Random && a.period >= ARENA_PERIOD_PREBATTLE
            })
        {
            self.world.arena_for_account_mut(account_db_id).unwrap().park_player(account_db_id);
            None
        } else {
            self.release_arena_membership(account_db_id)
        };

        // 3. Leave prebattle room if inside one, then clear in_battle if last human left
        let mut prb_events = self.prebattle.leave(account_db_id).unwrap_or_default();
        if let Some(prb_id) = finished_arena_prb_id {
            if prb_id > 0 {
                prb_events.extend(self.prebattle.notify_arena_finished(prb_id));
            }
        }
        actions.extend(self.collect_prebattle_events(prb_events, registry, now));

        // 4. Release owned entity IDs
        self.alloc.release(rec.player_id);
        match rec.state {
            SessionState::SwitchingToArena { avatar_id, .. } => self.alloc.release(avatar_id),
            SessionState::SwitchingToHangar { account_id, .. } => self.alloc.release(account_id),
            SessionState::Hangar | SessionState::InArena { .. } => {}
        }

        // 5. Unlock all vehicles on disk
        let _ = registry.unlock_vehicles_except(&rec.username, &self.protected_locks(account_db_id));
        actions
    }

    /// Leave the active arena for `account_db_id`, reclaiming entities if that player
    /// was the last human. Returns `Some(prebattle_id)` when the destroyed arena had an
    /// associated prebattle room (`> 0`), so the caller can mark it finished.
    fn release_arena_membership(&mut self, account_db_id: i64) -> Option<i32> {
        let prebattle_id = self
            .world
            .arena_for_account(account_db_id)
            .map(|a| a.prebattle_id)
            .unwrap_or(0);
        let cancel_settlement = self.world.arena_for_account(account_db_id).is_some_and(|a| {
            a.mode != BattleMode::Random || a.period < ARENA_PERIOD_BATTLE
        });
        let (space_id, _veh_id, reclaimed) = self.world.leave_player(account_db_id)?;
        if cancel_settlement {
            if let Some(settlement) = self.settlements.get_mut(&space_id) {
                settlement.players.remove(&account_db_id);
            }
        }
        if !reclaimed.is_empty() {
            self.settlements.remove(&space_id);
        }
        let last_human_left = !reclaimed.is_empty();
        for eid in reclaimed {
            self.alloc.release(eid);
        }
        (last_human_left && prebattle_id > 0).then_some(prebattle_id)
    }

    fn leave_arena_to_hangar(
        &mut self,
        account_db_id: i64,
        registry: &mut AccountRegistry,
        now: Instant,
    ) -> Vec<RoutedAction> {
        let mut out = Vec::new();
        if let Some(prb_id) = self.release_arena_membership(account_db_id) {
            let prb_events = self.prebattle.notify_arena_finished(prb_id);
            out.extend(self.collect_prebattle_events(prb_events, registry, now));
        }
        let mut pending_diffs = Vec::new();
        if let Some(rec) = self.peers.get(&account_db_id) {
            if let Ok(Some((_, diff))) =
                registry.unlock_vehicles_except(&rec.username, &self.protected_locks(account_db_id))
            {
                pending_diffs.push(diff);
            }
        }

        let account_id = self.alloc.alloc();
        if let Some(rec) = self.peers.get_mut(&account_db_id) {
            rec.locked_veh_inv_id = None;
            rec.state = SessionState::SwitchingToHangar {
                account_id,
                pending_diffs,
                started_at: now,
            };
        }
        out
    }

    /// Roll back an aborted `SwitchingToArena` transition back to a clean Hangar Account session
    /// when either the arena or the player's arena vehicle vanished before `ENABLE_ENTITIES`.
    fn rollback_arena_switch_to_hangar(
        &mut self,
        db_id: i64,
        space_id: i32,
        avatar_id: i32,
        sess: &mut BaseAppSession,
        encoder: &mut ElementEncoder,
        registry: &mut AccountRegistry,
    ) -> anyhow::Result<Vec<RoutedAction>> {
        self.alloc.release(avatar_id);
        for eid in self.world.cleanup_orphan_player(db_id, space_id) {
            self.alloc.release(eid);
        }

        let mut pending_diffs = Vec::new();
        if let Ok(Some((_, diff))) = registry.unlock_vehicles_except(&sess.login().username, &self.protected_locks(db_id)) {
            pending_diffs.push(diff);
        }

        let mut extra_prb_events = Vec::new();
        if let Some(prb_id) = self.prebattle.room_for_account(db_id) {
            let room_in_battle = self.prebattle.room(prb_id).is_some_and(|r| r.in_battle);
            if room_in_battle && !self.has_active_arena_for_prebattle(prb_id) {
                extra_prb_events = self.prebattle.notify_arena_finished(prb_id);
            }
        }

        let account_id = self.alloc.alloc();
        self.complete_hangar_entry(
            db_id,
            account_id,
            pending_diffs,
            extra_prb_events,
            sess,
            encoder,
            registry,
        )
    }

    fn has_active_arena_for_prebattle(&self, prb_id: i32) -> bool {
        self.world.active_space_ids().into_iter().any(|sid| {
            self.world
                .get(sid)
                .is_some_and(|arena| arena.prebattle_id == prb_id && !arena.all_humans_left())
        })
    }

    /// Complete a transition into Hangar for `db_id` with freshly allocated `account_id`,
    /// flushing account diffs, prebattle actor/room updates, game time `0`, and dossiers.
    #[allow(clippy::too_many_arguments)]
    fn complete_hangar_entry(
        &mut self,
        db_id: i64,
        account_id: i32,
        pending_diffs: Vec<Vec<u8>>,
        extra_prb_events: Vec<PrebattleEvent>,
        sess: &mut BaseAppSession,
        encoder: &mut ElementEncoder,
        registry: &mut AccountRegistry,
    ) -> anyhow::Result<Vec<RoutedAction>> {
        let profile = registry
            .get_or_create_profile(&sess.login().username)
            .map_err(|e| anyhow::anyhow!("failed loading profile: {e}"))?;
        sess.set_account_rev(profile.rev);
        sess.set_player_id(account_id);
        sess.set_entities_enabled(true);

        // Notify already-Hangar peers before marking `db_id` as `Hangar` so `db_id` does not
        // receive duplicate UPDATE_PREBATTLE actions on top of the bundle emitted below.
        let mut prb_sync_events = self.prebattle.set_member_actor_id(db_id, account_id as i64);
        prb_sync_events.extend(extra_prb_events);
        let actions = self.collect_prebattle_events(prb_sync_events, registry, Instant::now());

        if let Some(rec) = self.peers.get_mut(&db_id) {
            rec.player_id = account_id;
            rec.state = SessionState::Hangar;
            rec.locked_veh_inv_id = None;
        }

        let prb_id = self.prebattle.room_for_account(db_id);
        sess.emit_set_game_time(encoder, 0);
        sess.emit_hangar_entry(encoder, db_id, prb_id, false);
        for diff in pending_diffs {
            sess.emit_update(encoder, &diff);
        }
        if let Some(prb_id) = prb_id {
            if let Some(room) = self.prebattle.room(prb_id) {
                for (u_type, payload) in [
                    (PREBATTLE_UPDATE_SETTINGS, room.settings_pickle()),
                    (PREBATTLE_UPDATE_TEAM_STATES, room.team_states_pickle()),
                    (PREBATTLE_UPDATE_ROSTER, room.roster_pickle()),
                ] {
                    emit_entity_u8_bytes_method(
                        encoder,
                        account_id,
                        client_method::UPDATE_PREBATTLE,
                        u_type,
                        &payload,
                    );
                }
            }
        }
        sess.emit_resync_dossiers(encoder);
        emit_pending_battle_messages(registry, &sess.login().username, account_id, encoder);
        Ok(actions)
    }

    fn handle_enqueue(
        &mut self,
        db_id: i64,
        addr: SocketAddr,
        rec: &PeerBattleRecord,
        cmd: &DecodedDoCmd,
        registry: &mut AccountRegistry,
        now: Instant,
    ) -> Vec<RoutedAction> {
        if !matches!(rec.state, SessionState::Hangar)
            || self.queue.contains(db_id)
            || self.prebattle.room_for_account(db_id).is_some()
        {
            return vec![RoutedAction {
                account_db_id: db_id,
                addr,
                instruction: PeerInstruction::OnEnqueueFailure(JOIN_FAILURE_ACCOUNT_LOCK),
            }];
        }

        let (veh_inv_id, req_map, _queue_type) = match &cmd.args {
            DoCmdArgs::Int3([vid, arena_id, qt]) => (
                *vid as i64,
                (*arena_id > 0).then_some(*arena_id),
                *qt,
            ),
            _ => {
                return vec![RoutedAction {
                    account_db_id: db_id,
                    addr,
                    instruction: PeerInstruction::OnEnqueueFailure(JOIN_FAILURE_WRONG_ARGS),
                }];
            }
        };

        let _ = QUEUE_TYPE_RANDOMS;
        let Some((resolved_inv_id, comp_descr)) =
            self.pick_account_vehicle(&rec.username, Some(veh_inv_id), registry)
        else {
            return vec![RoutedAction {
                account_db_id: db_id,
                addr,
                instruction: PeerInstruction::OnEnqueueFailure(JOIN_FAILURE_WRONG_ARGS),
            }];
        };

        let catalog = registry.catalog_arc();
        let (tier, class_idx, mm_weight) =
            resolve_vehicle_mm_stats(&catalog, &comp_descr, &self.config.balance_config);

        let veh_type_cd = i32::from_le_bytes([
            comp_descr[0],
            comp_descr[1],
            comp_descr.get(2).copied().unwrap_or(0),
            comp_descr.get(3).copied().unwrap_or(0),
        ]);

        // Fail fast if vehicle lock persistence fails.
        let lock_diff = match registry.set_vehicle_lock(&rec.username, resolved_inv_id, LOCK_REASON_IN_QUEUE)
        {
            Ok(diff) => diff,
            Err(_) => {
                return vec![RoutedAction {
                    account_db_id: db_id,
                    addr,
                    instruction: PeerInstruction::OnEnqueueFailure(JOIN_FAILURE_WRONG_ARGS),
                }];
            }
        };

        let entry = QueueEntry {
            account_db_id: db_id,
            player_name: rec.username.clone(),
            veh_inv_id: resolved_inv_id,
            veh_comp_descr: comp_descr,
            veh_type_cd,
            tier,
            class_idx,
            mm_weight,
            requested_map_id: req_map,
            enqueued_at: now,
        };

        if let Err(code) = self.queue.enqueue(entry) {
            let _ = registry.set_vehicle_lock(&rec.username, resolved_inv_id, LOCK_REASON_NONE);
            return vec![RoutedAction {
                account_db_id: db_id,
                addr,
                instruction: PeerInstruction::OnEnqueueFailure(code),
            }];
        }

        if let Some(peer) = self.peers.get_mut(&db_id) {
            peer.locked_veh_inv_id = Some(resolved_inv_id);
        }

        let mut out = Vec::new();
        if let Some((_, diff)) = lock_diff {
            out.push(RoutedAction {
                account_db_id: db_id,
                addr,
                instruction: PeerInstruction::AccountUpdate(diff),
            });
        }
        out.push(RoutedAction {
            account_db_id: db_id,
            addr,
            instruction: PeerInstruction::OnEnqueued,
        });
        out
    }

    fn handle_dequeue(
        &mut self,
        db_id: i64,
        addr: SocketAddr,
        rec: &PeerBattleRecord,
        registry: &mut AccountRegistry,
    ) -> Vec<RoutedAction> {
        let mut out = Vec::new();
        if let Some(entry) = self.queue.dequeue(db_id) {
            if let Ok(Some((_, diff))) =
                registry.set_vehicle_lock(&rec.username, entry.veh_inv_id, LOCK_REASON_NONE)
            {
                out.push(RoutedAction {
                    account_db_id: db_id,
                    addr,
                    instruction: PeerInstruction::AccountUpdate(diff),
                });
            }
            if let Some(peer) = self.peers.get_mut(&db_id) {
                peer.locked_veh_inv_id = None;
            }
        }
        out.push(RoutedAction {
            account_db_id: db_id,
            addr,
            instruction: PeerInstruction::OnDequeued,
        });
        out
    }

    fn handle_force_queue(
        &mut self,
        db_id: i64,
        addr: SocketAddr,
        rec: &PeerBattleRecord,
        cmd: &DecodedDoCmd,
        now: Instant,
        registry: &mut AccountRegistry,
    ) -> Vec<RoutedAction> {
        if !matches!(rec.state, SessionState::Hangar) || !self.queue.contains(db_id) {
            return self.maybe_cmd_response(db_id, addr, cmd.request_id, cmd::RES_FAILURE);
        }
        let catalog = registry.catalog_arc();
        if let Some(spec) = self.queue.force_match(self.maps.arenas(), &catalog) {
            let mut out = self.maybe_cmd_response(db_id, addr, cmd.request_id, cmd::RES_SUCCESS);
            out.extend(self.launch_arena(spec, registry, now));
            out
        } else {
            self.maybe_cmd_response(db_id, addr, cmd.request_id, cmd::RES_FAILURE)
        }
    }

    fn handle_prb_ready(
        &mut self,
        db_id: i64,
        addr: SocketAddr,
        rec: &PeerBattleRecord,
        cmd: &DecodedDoCmd,
        registry: &mut AccountRegistry,
        now: Instant,
    ) -> Vec<RoutedAction> {
        if !matches!(rec.state, SessionState::Hangar) || self.prebattle.room_for_account(db_id).is_none()
        {
            return self.maybe_cmd_response(db_id, addr, cmd.request_id, cmd::RES_FAILURE);
        }
        let req_veh_inv_id = match &cmd.args {
            DoCmdArgs::Int3([vid, _, _]) if *vid > 0 => Some(*vid as i64),
            DoCmdArgs::Int3(_) => None,
            _ => {
                return self.maybe_cmd_response(db_id, addr, cmd.request_id, cmd::RES_WRONG_ARGS);
            }
        };
        let Some((veh_inv_id, comp_descr)) =
            self.pick_account_vehicle(&rec.username, req_veh_inv_id, registry)
        else {
            return self.maybe_cmd_response(db_id, addr, cmd.request_id, cmd::RES_FAILURE);
        };

        let already_locked_same = rec.locked_veh_inv_id == Some(veh_inv_id);
        let lock_diff = if already_locked_same {
            None
        } else {
            match registry.set_vehicle_lock(&rec.username, veh_inv_id, LOCK_REASON_PREBATTLE) {
                Ok(diff) => diff,
                Err(_) => {
                    return self.maybe_cmd_response(db_id, addr, cmd.request_id, cmd::RES_FAILURE);
                }
            }
        };

        match self.prebattle.set_ready(db_id, veh_inv_id, comp_descr, true) {
            Ok(events) => {
                let mut out = Vec::new();
                if let Some(prev_id) = rec.locked_veh_inv_id.filter(|&p| p != veh_inv_id) {
                    if let Ok(Some((_, unlock_diff))) =
                        registry.set_vehicle_lock(&rec.username, prev_id, LOCK_REASON_NONE)
                    {
                        out.push(RoutedAction {
                            account_db_id: db_id,
                            addr,
                            instruction: PeerInstruction::AccountUpdate(unlock_diff),
                        });
                    }
                }
                if let Some(peer) = self.peers.get_mut(&db_id) {
                    peer.locked_veh_inv_id = Some(veh_inv_id);
                }
                if let Some((_, diff)) = lock_diff {
                    out.push(RoutedAction {
                        account_db_id: db_id,
                        addr,
                        instruction: PeerInstruction::AccountUpdate(diff),
                    });
                }
                out.extend(self.collect_prebattle_events(events, registry, now));
                out.extend(self.maybe_cmd_response(
                    db_id,
                    addr,
                    cmd.request_id,
                    cmd::RES_SUCCESS,
                ));
                out
            }
            Err(code) => {
                if !already_locked_same {
                    let _ = registry.set_vehicle_lock(&rec.username, veh_inv_id, LOCK_REASON_NONE);
                }
                self.maybe_cmd_response(db_id, addr, cmd.request_id, code)
            }
        }
    }

    fn finish_prb_cmd(
        &mut self,
        db_id: i64,
        addr: SocketAddr,
        request_id: i16,
        res: Result<Vec<PrebattleEvent>, i16>,
        registry: &mut AccountRegistry,
        now: Instant,
    ) -> Vec<RoutedAction> {
        match res {
            Ok(events) => {
                let mut out = self.collect_prebattle_events(events, registry, now);
                out.extend(self.maybe_cmd_response(db_id, addr, request_id, cmd::RES_SUCCESS));
                out
            }
            Err(code) => self.maybe_cmd_response(db_id, addr, request_id, code),
        }
    }

    fn finish_prb_team_cmd(
        &mut self,
        db_id: i64,
        addr: SocketAddr,
        request_id: i16,
        res: Result<Vec<PrebattleEvent>, i16>,
        registry: &mut AccountRegistry,
        now: Instant,
    ) -> Vec<RoutedAction> {
        match res {
            Ok(events) => {
                let mut launches = Vec::new();
                let mut updates = Vec::new();
                for ev in events {
                    match ev {
                        PrebattleEvent::LaunchBattle(spec) => launches.push(spec),
                        other => updates.push(other),
                    }
                }
                let mut out = self.collect_prebattle_events(updates, registry, now);
                out.extend(self.maybe_cmd_response(db_id, addr, request_id, cmd::RES_SUCCESS));
                for spec in launches {
                    out.extend(self.launch_arena(spec, registry, now));
                }
                out
            }
            Err(code) => self.maybe_cmd_response(db_id, addr, request_id, code),
        }
    }

    fn collect_prebattle_events(
        &mut self,
        events: Vec<PrebattleEvent>,
        registry: &mut AccountRegistry,
        now: Instant,
    ) -> Vec<RoutedAction> {
        let mut out = Vec::new();
        for ev in events {
            match ev {
                PrebattleEvent::Joined {
                    account_db_id,
                    prebattle_id,
                    initial_updates,
                } => {
                    if let Some(target) = self.peers.get(&account_db_id) {
                        out.push(RoutedAction {
                            account_db_id,
                            addr: target.addr,
                            instruction: PeerInstruction::OnPrebattleJoined(prebattle_id),
                        });
                        for (update_type, payload) in initial_updates {
                            out.push(RoutedAction {
                                account_db_id,
                                addr: target.addr,
                                instruction: PeerInstruction::UpdatePrebattle {
                                    update_type,
                                    payload,
                                },
                            });
                        }
                    }
                }
                PrebattleEvent::Left {
                    account_db_id,
                    kicked_reason,
                } => {
                    if let Some(target) = self.peers.get(&account_db_id).cloned() {
                        if matches!(target.state, SessionState::Hangar) {
                            out.extend(self.unlock_locked_vehicle(account_db_id, registry));
                            let instruction = match kicked_reason {
                                Some(reason) => PeerInstruction::OnKickedFromPrebattle(reason),
                                None => PeerInstruction::OnPrebattleLeft,
                            };
                            out.push(RoutedAction {
                                account_db_id,
                                addr: target.addr,
                                instruction,
                            });
                        }
                    }
                }
                PrebattleEvent::Update {
                    recipients,
                    update_type,
                    payload,
                } => {
                    for rid in recipients {
                        if let Some(target) = self.peers.get(&rid) {
                            if matches!(target.state, SessionState::Hangar) {
                                out.push(RoutedAction {
                                    account_db_id: rid,
                                    addr: target.addr,
                                    instruction: PeerInstruction::UpdatePrebattle {
                                        update_type,
                                        payload: payload.clone(),
                                    },
                                });
                            }
                        }
                    }
                }
                PrebattleEvent::LaunchBattle(spec) => {
                    out.extend(self.launch_arena(spec, registry, now));
                }
            }
        }
        out
    }

    fn launch_arena(
        &mut self,
        spec: ArenaLaunchSpec,
        registry: &mut AccountRegistry,
        now: Instant,
    ) -> Vec<RoutedAction> {
        let mut out = Vec::new();
        let map = match self.maps.get_or_load(spec.arena_type_id) {
            Ok(m) => m,
            Err(err) => {
                tracing::error!(arena_type_id = spec.arena_type_id, error = %err, "failed loading map for arena");
                if spec.mode == BattleMode::Training && spec.prebattle_id > 0 {
                    let prb_events = self.prebattle.notify_arena_finished(spec.prebattle_id);
                    out.extend(self.collect_prebattle_events(prb_events, registry, now));
                }
                for p in &spec.participants {
                    if p.is_bot {
                        continue;
                    }
                    out.extend(self.unlock_locked_vehicle(p.account_db_id, registry));
                    if let Some(peer) = self.peers.get(&p.account_db_id) {
                        out.push(RoutedAction {
                            account_db_id: p.account_db_id,
                            addr: peer.addr,
                            instruction: PeerInstruction::OnArenaJoinFailure(JOIN_FAILURE_NOT_FOUND),
                        });
                    }
                }
                return out;
            }
        };

        let space_id = self.alloc.alloc_space_id();
        let catalog = registry.catalog_arc();

        let mut enriched_spec = spec.clone();
        let mut players = BTreeMap::new();
        enriched_spec.participants.retain_mut(|p| {
            if p.is_bot {
                return true;
            }
            if let Some(peer) = self.peers.get(&p.account_db_id).cloned() {
                match registry.set_vehicle_lock(&peer.username, p.veh_inv_id, LOCK_REASON_ON_ARENA) {
                    Ok(Some((_, diff))) => out.push(RoutedAction {
                        account_db_id: p.account_db_id, addr: peer.addr,
                        instruction: PeerInstruction::AccountUpdate(diff),
                    }),
                    Ok(None) => {}
                    Err(error) => {
                        tracing::error!(account_db_id = p.account_db_id, %error, "could not lock arena vehicle");
                        out.extend(self.unlock_locked_vehicle(p.account_db_id, registry));
                        out.push(RoutedAction {
                            account_db_id: p.account_db_id, addr: peer.addr,
                            instruction: PeerInstruction::OnArenaJoinFailure(JOIN_FAILURE_ACCOUNT_LOCK),
                        });
                        return false;
                    }
                }
                players.insert(p.account_db_id, peer.username.clone());
                if let Some(profile) = registry.get_profile(&peer.username) {
                    if let Some(veh) = profile.vehicles.get(&p.veh_inv_id) {
                        p.shells = veh
                            .shells
                            .iter()
                            .map(|&(cd, count)| (cd, count.max(0) as u32))
                            .collect();
                        p.eqs = veh.eqs.iter().map(|&e| e as u16).collect();
                        let mut crew = Vec::new();
                        for &t_id_opt in &veh.crew {
                            if let Some(t_id) = t_id_opt {
                                if let Some(t_entry) = profile.tankmen.get(&t_id) {
                                    if let Ok(tdescr) =
                                        wot065_data::descr::TankmanDescr::from_bytes(&t_entry.comp_descr)
                                    {
                                        crew.push(tdescr);
                                    }
                                }
                            }
                        }
                        p.crew = crew;
                    }
                }
                true
            } else {
                false
            }
        });
        if players.is_empty() {
            if spec.prebattle_id > 0 {
                let events = self.prebattle.notify_arena_finished(spec.prebattle_id);
                out.extend(self.collect_prebattle_events(events, registry, now));
            }
            return out;
        }

        let alloc = &mut self.alloc;
        self.world
            .spawn_arena(space_id, map, &enriched_spec, &catalog, &mut || alloc.alloc());
        if let Some(arena) = self.world.get_mut(space_id) {
            arena.balance = self.config.balance_config.clone();
            arena.set_collision_cache(Arc::clone(&self.collision_cache), Some(&catalog));
            if arena.mode == BattleMode::Random {
                arena.prebattle_countdown_secs = self.config.prebattle_countdown_secs;
            }
            // A human's loaded ammunition comes from inventory, including an empty rack.
            for p in enriched_spec.participants.iter().filter(|p| !p.is_bot && p.shells.is_empty()) {
                if let Some(id) = arena.vehicle_for_account(p.account_db_id).map(|v| v.entity_id) {
                    if let Some(aim) = arena.aim_states.get_mut(&id) {
                        aim.ammo_counts.clear();
                        aim.current_shell_cd = 0;
                    }
                }
            }
        }
        self.settlements.insert(space_id, ArenaSettlement {
            unique_id: (rand::random::<u64>() & i64::MAX as u64) | 1,
            created_at: AccountPayloads::unix_now() as u32,
            players,
            retry_at: now,
        });

        for p in &enriched_spec.participants {
            if p.is_bot {
                continue;
            }
            let Some(peer) = self.peers.get(&p.account_db_id).cloned() else {
                continue;
            };
            let avatar_id = self.alloc.alloc();
            if let Some(peer_mut) = self.peers.get_mut(&p.account_db_id) {
                peer_mut.locked_veh_inv_id = Some(p.veh_inv_id);
                peer_mut.state = SessionState::SwitchingToArena {
                    space_id,
                    arena_type_id: spec.arena_type_id,
                    arena_gui_type: spec.arena_gui_type,
                    avatar_id,
                    started_at: now,
                };
            }
            out.push(RoutedAction {
                account_db_id: p.account_db_id,
                addr: peer.addr,
                instruction: PeerInstruction::StartArenaSwitch,
            });
        }
        out
    }

    fn unlock_locked_vehicle(
        &mut self,
        db_id: i64,
        registry: &mut AccountRegistry,
    ) -> Vec<RoutedAction> {
        let Some(peer) = self.peers.get_mut(&db_id) else {
            return Vec::new();
        };
        let Some(veh_inv_id) = peer.locked_veh_inv_id.take() else {
            return Vec::new();
        };
        if let Ok(Some((_, diff))) =
            registry.set_vehicle_lock(&peer.username, veh_inv_id, LOCK_REASON_NONE)
        {
            if matches!(peer.state, SessionState::Hangar) {
                return vec![RoutedAction {
                    account_db_id: db_id,
                    addr: peer.addr,
                    instruction: PeerInstruction::AccountUpdate(diff),
                }];
            }
        }
        Vec::new()
    }

    fn pick_account_vehicle(
        &self,
        username: &str,
        requested_inv_id: Option<i64>,
        registry: &AccountRegistry,
    ) -> Option<(i64, Vec<u8>)> {
        let profile: &AccountProfile = registry.get_profile(username)?;
        if let Some(inv_id) = requested_inv_id {
            return profile
                .vehicles
                .get(&inv_id)
                .map(|v| (inv_id, v.comp_descr.clone()));
        }
        profile
            .vehicles
            .iter()
            .min_by_key(|(id, _)| *id)
            .map(|(id, v)| (*id, v.comp_descr.clone()))
    }

    fn maybe_cmd_response(
        &self,
        db_id: i64,
        addr: SocketAddr,
        request_id: i16,
        result_id: i16,
    ) -> Vec<RoutedAction> {
        if request_id >= cmd::REQUEST_ID_UNRESERVED_MIN {
            vec![RoutedAction {
                account_db_id: db_id,
                addr,
                instruction: PeerInstruction::CmdResponse {
                    request_id,
                    result_id,
                },
            }]
        } else {
            Vec::new()
        }
    }
}

/// Build one or more MTU-bounded movement datagrams, each with its own timestamp and origin.
pub(crate) fn build_peer_unreliable_snapshots(
    account_db_id: i64,
    tick_byte: u8,
    vehicles: &[VehicleState],
) -> Vec<ElementEncoder> {
    let Some(own_vehicle) = vehicles
        .iter()
        .find(|v| v.account_db_id == account_db_id && !v.is_bot && !v.left_arena)
    else {
        return Vec::new();
    };
    let reference = own_vehicle.position;
    let start_datagram = || {
        let mut encoder = ElementEncoder::new();
        emit_tick_sync(&mut encoder, tick_byte);
        emit_relative_position(&mut encoder, reference);
        encoder
    };

    let mut batches = Vec::new();
    let mut current = start_datagram();
    for vehicle in vehicles.iter().filter(|v| !v.left_arena) {
        // Reserve enough space for a message ID and the largest pose message.
        if current.body().len() + 1 + DetailedPosition::WIRE_SIZE > DEFAULT_MAX_FRAGMENT_BODY {
            batches.push(current);
            current = start_datagram();
        }
        emit_s2c_avatar_update(&mut current, vehicle, reference);
    }
    batches.push(current);
    batches
}

fn resolve_vehicle_mm_stats(
    catalog: &Catalog,
    comp_descr: &[u8],
    balance: &BalanceConfig,
) -> (u8, usize, f32) {
    if let Ok(descr) = VehicleDescr::from_bytes(comp_descr) {
        if let Some(vdef) = catalog.vehicle_def_by_id(descr.nation_id, u16::from(descr.type_id)) {
            return (
                vdef.level.clamp(1, 10) as u8,
                vehicle_class_index(vdef),
                compute_vehicle_weight(vdef, &balance.matchmaker),
            );
        }
    }
    (1, 0, 2.0)
}

fn default_bot_comp_descr(catalog: &Catalog) -> Option<Vec<u8>> {
    if let Some(vdef) = catalog
        .vehicle_def_by_id(0, 0)
        .or_else(|| catalog.vehicle_defs.values().next())
    {
        if let Ok(d) = VehicleDescr::stock(vdef) {
            return Some(d.to_bytes());
        }
    }
    catalog.vehicles.first().map(|v| v.comp_descr.clone())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn entity_id_allocator_stays_below_client_id_space_and_reuses_released_ids() {
        let mut alloc = EntityIdAllocator::new();
        let a = alloc.alloc();
        let b = alloc.alloc();
        let c = alloc.alloc();
        assert_eq!((a, b, c), (1, 2, 3));
        assert!(c < (1 << 30));

        alloc.release(b);
        alloc.reserve(10);
        let d = alloc.alloc();
        assert_eq!(d, 4);
        assert_ne!(alloc.alloc(), 10);

        // Exhaust cursor near MAX_SERVER_ENTITY_ID and confirm recycled ID is reused
        alloc.next_id = MAX_SERVER_ENTITY_ID;
        let top = alloc.alloc();
        assert_eq!(top, MAX_SERVER_ENTITY_ID);
        let recycled = alloc.alloc();
        assert_eq!(recycled, b);
    }

    struct TempHostDir(std::path::PathBuf);
    impl TempHostDir {
        fn new(name: &str) -> Self {
            let dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("../../target/test-tmp")
                .join(format!("bh_{name}_{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).unwrap();
            Self(dir)
        }
    }
    impl Drop for TempHostDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn write_synthetic_host_assets(root: &std::path::Path, extra_unloaded_arena: bool) {
        let vehicles_dir = root.join("res/scripts/item_defs/vehicles");
        for (nation, veh) in [
            ("ussr", "MS-1"),
            ("germany", "Ltraktor"),
            ("usa", "T1_Cunningham"),
        ] {
            let ndir = vehicles_dir.join(nation);
            let cdir = ndir.join("components");
            std::fs::create_dir_all(&cdir).unwrap();
            std::fs::write(ndir.join("customization.xml"), "<root/>").unwrap();
            std::fs::write(
                ndir.join("list.xml"),
                format!("<root><{veh}><id>0</id><userString>{veh}</userString><level>1</level><price>0</price><tags>lightTank</tags></{veh}></root>"),
            )
            .unwrap();
            std::fs::write(cdir.join("shells.xml"), "<root><AP><id>1</id><price>5</price><kind>ARMOR_PIERCING</kind><caliber>20</caliber><isTracer>true</isTracer><effects>smallAP</effects><damage><armor>12</armor><devices>20</devices></damage></AP></root>").unwrap();
            std::fs::write(cdir.join("chassis.xml"), "<root><ids><Ch>1</Ch></ids><shared><Ch><weight>1000</weight><level>1</level><price>100</price><maxLoad>10000</maxLoad></Ch></shared></root>").unwrap();
            std::fs::write(cdir.join("engines.xml"), "<root><ids><Eng>1</Eng></ids><shared><Eng><weight>500</weight><level>1</level><price>100</price><power>100</power></Eng></shared></root>").unwrap();
            std::fs::write(cdir.join("fuelTanks.xml"), "<root><ids><Fuel>1</Fuel></ids><shared><Fuel><weight>100</weight><level>1</level><price>50</price></Fuel></shared></root>").unwrap();
            std::fs::write(cdir.join("radios.xml"), "<root><ids><Rad>1</Rad></ids><shared><Rad><weight>50</weight><level>1</level><price>50</price><distance>300</distance></Rad></shared></root>").unwrap();
            std::fs::write(cdir.join("guns.xml"), "<root><ids><G>1</G></ids><shared><G><level>1</level><price>100</price><weight>50</weight><maxAmmo>50</maxAmmo><pitchLimits>-10 20</pitchLimits><rotationSpeed>30</rotationSpeed><reloadTime>2.0</reloadTime><aimingTime>1.5</aimingTime><shotDispersionRadius>0.4</shotDispersionRadius><effects>small</effects><shots><AP><defaultPortion>1.0</defaultPortion><speed>600</speed><gravity>9.81</gravity><maxDistance>400</maxDistance><piercingPower>30 20</piercingPower></AP></shots></G></shared></root>").unwrap();
            std::fs::write(cdir.join("turrets.xml"), "<root><ids><T>1</T></ids><shared><T><level>1</level><price>100</price><weight>200</weight><maxHealth>20</maxHealth><yawLimits>-180 180</yawLimits><guns><G>shared</G></guns></T></shared></root>").unwrap();
            std::fs::write(ndir.join(format!("{veh}.xml")), "<root><crew><commander/><driver/></crew><speedLimits><forward>32</forward><backward>10</backward></speedLimits><hull><weight>3000</weight><maxHealth>80</maxHealth><armor><armor_1>15</armor_1></armor><turretPositions><turret>0 0.5 0</turret></turretPositions></hull><chassis><Ch>shared</Ch></chassis><engines><Eng>shared</Eng></engines><fuelTanks><Fuel>shared</Fuel></fuelTanks><radios><Rad>shared</Rad></radios><turrets0><T>shared</T></turrets0></root>").unwrap();
        }
        if extra_unloaded_arena {
            let res_dir = root.join("res");
            let adir = res_dir.join("scripts/arena_defs");
            std::fs::create_dir_all(&adir).unwrap();
            std::fs::create_dir_all(res_dir.join("system/data")).unwrap();
            std::fs::write(
                adir.join("_list_.xml"),
                "<root><map><id>99</id><name>99_missing</name></map></root>",
            )
            .unwrap();
            std::fs::write(
                adir.join("_common_.xml"),
                "<root><minPlayersInTeam>1</minPlayersInTeam><maxPlayersInTeam>15</maxPlayersInTeam><roundLength>900</roundLength><arenaStartDelay>5.0</arenaStartDelay><kickAfterFinishWaitTime>10.0</kickAfterFinishWaitTime></root>",
            )
            .unwrap();
            std::fs::write(
                adir.join("99_missing.xml"),
                "<root><name>99</name><description>Missing</description><geometry>spaces/missing_map</geometry><boundingBox><bottomLeft>-100 -100</bottomLeft><upperRight>100 100</upperRight></boundingBox></root>",
            )
            .unwrap();
            std::fs::write(
                res_dir.join("system/data/material_kinds.xml"),
                "<root><kind><id>1</id><desc>firm</desc><ground_strength>firm</ground_strength><terrain>maps/landscape/road.dds</terrain></kind></root>",
            )
            .unwrap();
            std::fs::write(
                res_dir.join("scripts/destructibles.xml"),
                "<root><defaultLifetimeEffectChance>0.5</defaultLifetimeEffectChance><unitVehicleMass>30</unitVehicleMass><structures/><fragiles/><fallingAtoms/><trees/></root>",
            )
            .unwrap();
        }
    }

    fn build_host_test_maps() -> Arc<MapStore> {
        use glam::{Mat4, Vec3};
        use std::collections::BTreeMap;
        use wot_battle::assets::arena_defs::ArenaDef;
        use wot_battle::assets::chunk::{ChunkCoords, ChunkKind, SpaceBounds, SpawnPoint};
        use wot_battle::assets::map_store::{ArenaMap, TerrainMap};
        use wot_battle::assets::terrain::{
            MaterialKinds, TerrainChunk, DOMINANT_TEX_GRID_SIZE, HEIGHTMAP_GRID_SIZE,
        };
        let def = ArenaDef {
            id: 3,
            name: "04_himmelsdorf".into(),
            display_name_key: "Himmelsdorf".into(),
            description_key: String::new(),
            geometry: "spaces/04_himmelsdorf".into(),
            bounding_box: ([-100.0, -100.0], [100.0, 100.0]),
            min_players_in_team: 1,
            max_players_in_team: 15,
            round_length_secs: 900,
            arena_start_delay_secs: 5.0,
            kick_after_finish_wait_secs: 10.0,
            minimap_texture: None,
        };
        let bounds = SpaceBounds {
            min_x: -1,
            max_x: 0,
            min_y: -1,
            max_y: 0,
        };
        let mut chunks = Vec::new();
        for gz in -1..=0 {
            for gx in -1..=0 {
                chunks.push((
                    ChunkCoords {
                        grid_x: gx,
                        grid_z: gz,
                        kind: ChunkKind::Outer,
                    },
                    TerrainChunk {
                        heights: vec![4.0; HEIGHTMAP_GRID_SIZE * HEIGHTMAP_GRID_SIZE],
                        min_y: 4.0,
                        max_y: 4.0,
                        dom_tex_indices: vec![0; DOMINANT_TEX_GRID_SIZE * DOMINANT_TEX_GRID_SIZE],
                        tex_names: vec![],
                    },
                ));
            }
        }
        let terrain = TerrainMap::from_chunks(bounds, chunks).unwrap();
        let mats = Arc::new(MaterialKinds::parse(b"<root/>").unwrap());
        let spawns = vec![
            SpawnPoint {
                team: 1,
                preferable_type: None,
                world_transform: Mat4::IDENTITY,
                position: Vec3::new(-40.0, 4.0, -40.0),
                yaw: 0.4,
            },
            SpawnPoint {
                team: 2,
                preferable_type: None,
                world_transform: Mat4::IDENTITY,
                position: Vec3::new(40.0, 4.0, 40.0),
                yaw: -2.7,
            },
        ];
        let arena_map = Arc::new(ArenaMap::from_parts(
            def,
            bounds,
            terrain,
            mats,
            vec![],
            vec![],
            vec![],
            spawns,
            vec![],
            BTreeMap::new(),
            &BalanceConfig::default(),
        ));
        Arc::new(MapStore::from_preloaded(
            vec![arena_map],
            BalanceConfig::default(),
        ))
    }

    fn make_host_and_registry(
        name: &str,
        maps: Arc<MapStore>,
    ) -> (TempHostDir, BattleHost, AccountRegistry, Arc<AccountPayloads>) {
        use wot065_data::account_state::Preset;
        use wot065_data::economics::EconomicsConfig;
        let tmp = TempHostDir::new(name);
        write_synthetic_host_assets(&tmp.0, false);
        let payloads = Arc::new(
            AccountPayloads::load(
                &tmp.0,
                &crate::payloads::PayloadOverrides::default(),
            )
            .unwrap(),
        );
        let economics = Arc::new(EconomicsConfig::default());
        let reg = AccountRegistry::open(
            &tmp.0.join("accounts"),
            payloads.catalog.clone(),
            economics,
            Preset::Retail,
        )
        .unwrap();
        let host = BattleHost::new(BattleHostConfig::default(), maps);
        (tmp, host, reg, payloads)
    }

    fn make_session_for(
        username: &str,
        key_seed: u32,
        addr: SocketAddr,
        payloads: Arc<AccountPayloads>,
    ) -> BaseAppSession {
        let login = bw201::login::LoginSession::new(
            username,
            b"0123456789abcdef",
            key_seed,
            &addr.to_string(),
        );
        BaseAppSession::new(login, (*payloads).clone()).unwrap()
    }

    #[test]
    fn launched_arena_shares_the_host_vehicle_collision_cache() {
        let (_tmp, mut host, mut registry, payloads) =
            make_host_and_registry("collision_cache", build_host_test_maps());
        let addr = "127.0.0.1:40031".parse().unwrap();
        let mut session = make_session_for("collision-cache", 1, addr, payloads);
        let mut encoder = ElementEncoder::new();
        host.on_enable_entities(addr, &mut session, &mut encoder, &mut registry).unwrap();
        let profile = registry.get_profile("collision-cache").unwrap();
        let db_id = profile.database_id;
        let (&veh_inv_id, vehicle) = profile.vehicles.iter().next().unwrap();
        let spec = ArenaLaunchSpec {
            arena_type_id: 3,
            arena_gui_type: 1,
            arena_bonus_type: 1,
            mode: BattleMode::Random,
            prebattle_id: 0,
            round_length_secs: 100,
            participants: vec![ParticipantSpec {
                account_db_id: db_id,
                veh_inv_id,
                veh_comp_descr: vehicle.comp_descr.clone(),
                player_name: "collision-cache".into(),
                team: 1,
                ..Default::default()
            }],
        };

        host.launch_arena(spec, &mut registry, Instant::now());

        let arena = host.world.arena_for_account(db_id).unwrap();
        let cache = arena.collision_cache.as_ref().expect("the arena loads hulls through the host cache");
        assert!(Arc::ptr_eq(cache, &host.collision_cache));
    }

    #[test]
    fn random_bot_injection_requires_developer_features() {
        let (_tmp, mut host, mut registry, payloads) =
            make_host_and_registry("bot_access", build_host_test_maps());
        host.config.prebattle_countdown_secs = 0;
        let addr = "127.0.0.1:40027".parse().unwrap();
        let mut session = make_session_for("bot-access", 1, addr, payloads);
        let mut encoder = ElementEncoder::new();
        host.on_enable_entities(addr, &mut session, &mut encoder, &mut registry).unwrap();
        let profile = registry.get_profile("bot-access").unwrap();
        let db_id = profile.database_id;
        let (&veh_inv_id, vehicle) = profile.vehicles.iter().next().unwrap();
        let descr = vehicle.comp_descr.clone();
        let now = Instant::now();
        let spec = ArenaLaunchSpec {
            arena_type_id: 3,
            arena_gui_type: 1,
            arena_bonus_type: 1,
            mode: BattleMode::Random,
            prebattle_id: 0,
            round_length_secs: 100,
            participants: vec![ParticipantSpec {
                account_db_id: db_id,
                veh_inv_id,
                veh_comp_descr: descr.clone(),
                player_name: "bot-access".into(),
                team: 1,
                ..Default::default()
            }],
        };
        host.launch_arena(spec, &mut registry, now);
        host.on_enable_entities(addr, &mut session, &mut encoder, &mut registry).unwrap();
        host.world.arena_for_account_mut(db_id).unwrap().mark_client_ready(db_id);
        let mut body = bw201::stream::BinaryWriter::new();
        body.string_bytes(&descr);
        body.u8(2);
        body.string("injected");
        let method = crate::protocol::base_method::AVATAR_ADD_BOT_TO_ARENA;
        host.on_avatar_method(addr, method, body.as_slice(), &mut registry, now);
        assert_eq!(host.world.arena_for_account(db_id).unwrap().vehicles.len(), 1);
        host.config.dev_features = true;
        host.on_avatar_method(addr, method, body.as_slice(), &mut registry, now);
        assert_eq!(host.world.arena_for_account(db_id).unwrap().vehicles.len(), 2);
    }

    /// One player in a Random arena with entities enabled, before `onClientReady`.
    struct ArenaPeer {
        _tmp: TempHostDir,
        host: BattleHost,
        registry: AccountRegistry,
        session: BaseAppSession,
        addr: SocketAddr,
        db_id: i64,
        now: Instant,
    }

    impl ArenaPeer {
        fn enter(username: &str, port: u16) -> Self {
            let (tmp, mut host, mut registry, payloads) =
                make_host_and_registry(&username.replace('-', "_"), build_host_test_maps());
            host.config.prebattle_countdown_secs = 0;
            let addr: SocketAddr = format!("127.0.0.1:{port}").parse().unwrap();
            let mut session = make_session_for(username, 1, addr, payloads);
            let mut encoder = ElementEncoder::new();
            host.on_enable_entities(addr, &mut session, &mut encoder, &mut registry).unwrap();
            let profile = registry.get_profile(username).unwrap();
            let db_id = profile.database_id;
            let (&veh_inv_id, vehicle) = profile.vehicles.iter().next().unwrap();
            let spec = ArenaLaunchSpec {
                arena_type_id: 3,
                arena_gui_type: 1,
                arena_bonus_type: 1,
                mode: BattleMode::Random,
                prebattle_id: 0,
                round_length_secs: 100,
                participants: vec![ParticipantSpec {
                    account_db_id: db_id,
                    veh_inv_id,
                    veh_comp_descr: vehicle.comp_descr.clone(),
                    player_name: username.into(),
                    team: 1,
                    ..Default::default()
                }],
            };
            let now = Instant::now();
            host.launch_arena(spec, &mut registry, now);
            host.on_enable_entities(addr, &mut session, &mut encoder, &mut registry).unwrap();
            Self { _tmp: tmp, host, registry, session, addr, db_id, now }
        }

        /// `(avatar_id, vehicle_id)` of the peer.
        fn ids(&self) -> (i32, i32) {
            let SessionState::InArena { avatar_id, vehicle_id, .. } = self.host.peers[&self.db_id].state
            else {
                panic!("the peer must be in the arena");
            };
            (avatar_id, vehicle_id)
        }

        /// `Avatar.base.onClientReady`, sent once `__startGUI` built the battle window.
        fn client_ready(&mut self) {
            let method = crate::protocol::base_method::AVATAR_ON_CLIENT_READY;
            self.host.on_avatar_method(self.addr, method, &[], &mut self.registry, self.now);
        }

        fn request_fire(&mut self) -> bool {
            self.host.world.arena_for_account_mut(self.db_id).unwrap().request_fire(self.db_id)
        }

        /// Run `count` ticks of 100 ms and return what the peer was sent.
        fn ticks(&mut self, count: usize) -> Vec<PeerInstruction> {
            let mut sent = Vec::new();
            for _ in 0..count {
                self.now += Duration::from_millis(100);
                let actions = self.host.tick(self.now, &mut self.registry).0;
                sent.extend(actions.into_iter().map(|action| action.instruction));
            }
            sent
        }
    }

    /// Own-vehicle position of every movement snapshot, keyed by its `TICK_SYNC` byte, while the
    /// tank drives forward and the host wakes at `wakeups_ms` after arena launch.
    fn driven_snapshots(username: &str, port: u16, wakeups_ms: &[u64]) -> Vec<(u8, glam::Vec3)> {
        let mut peer = ArenaPeer::enter(username, port);
        peer.client_ready();
        let start = peer.now;
        let mut sent = Vec::new();
        for &at in wakeups_ms {
            let db_id = peer.db_id;
            peer.host.world.arena_for_account_mut(db_id).unwrap().set_vehicle_move_flags(db_id, 1);
            let (_, snapshots, _) =
                peer.host.tick(start + Duration::from_millis(at), &mut peer.registry);
            for (_, recipient, tick_byte, vehicles) in snapshots {
                let own = vehicles.iter().find(|v| v.account_db_id == recipient).unwrap();
                sent.push((tick_byte, own.position));
            }
        }
        sent
    }

    /// The client times each movement datagram by its `TICK_SYNC` byte at the 10 Hz update
    /// frequency, so byte k must carry the state at arena time k * 100 ms. Host wake-ups jitter
    /// around the tick period; any mismatch makes the camera lag and catch up every tick.
    #[test]
    fn movement_snapshot_tick_byte_names_the_simulated_instant_despite_wake_jitter() {
        let steady: Vec<u64> = (0..=50).map(|k| k * 100).collect();
        let reference: BTreeMap<u8, glam::Vec3> =
            driven_snapshots("tick-sync-steady", 40041, &steady).into_iter().collect();
        let mut at = 0;
        let jittered: Vec<u64> = [93, 109, 98, 151, 49, 104, 95, 112, 88, 101]
            .iter()
            .cycle()
            .take(40)
            .map(|delta| {
                at += delta;
                at
            })
            .collect();
        let sent = driven_snapshots("tick-sync-jitter", 40042, &jittered);

        assert!(sent.len() >= 30, "only {} snapshots", sent.len());
        assert!(
            sent.windows(2).all(|pair| pair[0].0 < pair[1].0),
            "tick bytes must strictly increase: {:?}",
            sent.iter().map(|s| s.0).collect::<Vec<_>>()
        );
        let moved = reference[&40] - reference[&1];
        assert!(moved.length() > 5.0, "the tank must drive, moved {moved:?}");
        for (tick_byte, position) in sent {
            let want = reference[&tick_byte];
            assert!(
                (position - want).length() < 1e-4,
                "tick byte {tick_byte}: sent {position:?}, state at that instant {want:?}"
            );
        }
    }

    fn reload_times(sent: &[PeerInstruction]) -> Vec<f32> {
        sent.iter()
            .filter_map(|instruction| match instruction {
                PeerInstruction::AvatarReloadTime { time_left, .. } => Some(*time_left),
                _ => None,
            })
            .collect()
    }

    /// `AvatarPositionControl.__doBind` calls `Avatar.cell.bindToVehicle(vehicleID, pos)`,
    /// and the client addresses its own Avatar as entity 0. The client binds the camera
    /// (`getOwnVehicleMatrix`) to `vehicle.matrix` only once `Avatar.vehicle` is its tank,
    /// which takes `setVehicle`; otherwise the camera stays where the tank spawned.
    #[test]
    fn own_avatar_bind_to_vehicle_boards_the_avatar_onto_its_tank() {
        let mut peer = ArenaPeer::enter("bind-camera", 40031);
        let (avatar_id, vehicle_id) = peer.ids();

        let mut args = bw201::stream::BinaryWriter::new();
        args.i32(vehicle_id);
        for component in [1.0f32, 2.0, 3.0] {
            args.f32(component);
        }
        let method = crate::protocol::cell_method::AVATAR_BIND_TO_VEHICLE;
        let actions = peer.host.on_cell_method(peer.addr, 0, method, args.as_slice());

        let mut out = ElementEncoder::new();
        for action in actions {
            encode_instruction(&mut peer.session, &mut out, action.instruction);
        }
        let messages = bw201::bundle::decode_messages(
            &out.finish(),
            &bw201::interfaces::client_interface(),
            bw201::packet::Endian::Little,
        )
        .unwrap();
        let ids_of = |id: u8, skip_space: bool| -> Vec<(i32, i32)> {
            messages
                .iter()
                .filter(|m| m.id == id)
                .map(|m| {
                    let mut r = bw201::stream::BinaryReader::new(&m.data);
                    let entity = r.i32().unwrap();
                    if skip_space {
                        r.i32().unwrap();
                    }
                    (entity, r.i32().unwrap())
                })
                .collect()
        };
        use bw201::interfaces::client_ids;
        assert_eq!(ids_of(client_ids::SET_VEHICLE, false), [(avatar_id, vehicle_id)]);
        assert_eq!(ids_of(client_ids::FORCED_POSITION, true), [(avatar_id, vehicle_id)]);
    }

    /// `Avatar.updateVehicleGunReloadTime` plays `gun_reloaded` for `0.0` and restarts the
    /// reload circle otherwise. `__startGUI` leaves the aim at `setReloading(-1)`, which
    /// `shoot()` treats as reloading, so the client needs the state once it is ready; after
    /// that only reload starts and ends.
    #[test]
    fn gun_reload_time_follows_client_ready_and_each_reload() {
        let mut peer = ArenaPeer::enter("reload-notice", 40032);
        assert_eq!(reload_times(&peer.ticks(3)), [0.0; 0], "nothing before the battle GUI");
        peer.client_ready();
        assert_eq!(reload_times(&peer.ticks(4)), [0.0], "one loaded-gun notice after ready");

        assert!(peer.request_fire());
        let sent = reload_times(&peer.ticks(30));
        assert_eq!(sent.len(), 2, "one notice per reload start and end: {sent:?}");
        assert!(sent[0] > 1.0, "the start carries the reload time: {sent:?}");
        assert_eq!(sent[1], 0.0);
    }

    /// `Avatar.shoot` waits up to 2 s for `updateVehicleGunReloadTime` after each shoot
    /// command, so a shot the server refuses is still answered.
    #[test]
    fn a_refused_shot_is_answered_with_the_reload_time() {
        let mut peer = ArenaPeer::enter("reload-refused", 40034);
        peer.client_ready();
        peer.ticks(2);
        assert!(peer.request_fire());
        peer.ticks(2);
        assert!(!peer.request_fire(), "the gun is reloading");
        let sent = reload_times(&peer.ticks(1));
        assert_eq!(sent.len(), 1, "{sent:?}");
        assert!(sent[0] > 1.0, "the answer carries the remaining reload: {sent:?}");
    }

    /// `Avatar.__processVehicleAmmo` reads `g_windowsManager.battleWindow` before it stores
    /// the shell, and the battle window exists only once the client calls
    /// `Avatar.base.onClientReady`. `Avatar.shoot` returns while `__currShellsIdx` is unset,
    /// and only `updateVehicleSetting(CURRENT_SHELLS)` sets it, once the shell is listed.
    #[test]
    fn ammo_and_the_loaded_shell_follow_client_ready() {
        let mut peer = ArenaPeer::enter("ammo-ready", 40033);
        let consumables = |sent: &[PeerInstruction]| {
            sent.iter()
                .filter(|i| {
                    matches!(
                        i,
                        PeerInstruction::AvatarAmmo { .. } | PeerInstruction::AvatarVehicleSetting { .. }
                    )
                })
                .count()
        };
        assert_eq!(consumables(&peer.ticks(3)), 0, "sent before the battle GUI exists");

        peer.client_ready();
        let sent = peer.ticks(1);
        let ammo: Vec<(usize, i32)> = sent
            .iter()
            .enumerate()
            .filter_map(|(at, i)| match i {
                PeerInstruction::AvatarAmmo { compact_descr, .. } => Some((at, *compact_descr)),
                _ => None,
            })
            .collect();
        let settings: Vec<(usize, u8, i32)> = sent
            .iter()
            .enumerate()
            .filter_map(|(at, i)| match i {
                PeerInstruction::AvatarVehicleSetting { code, value, .. } => Some((at, *code, *value)),
                _ => None,
            })
            .collect();
        assert!(!ammo.is_empty());
        let &(current_at, _, current) = settings
            .iter()
            .find(|&&(_, code, _)| code == 0)
            .expect("CURRENT_SHELLS");
        assert!(ammo.iter().any(|&(_, cd)| cd == current), "{current} is not stocked");
        assert!(ammo.iter().all(|&(at, _)| at < current_at), "shells are listed first");
    }

    #[test]
    fn cleanup_mid_battle_clears_prebattle_in_battle_and_skips_in_arena_recipients() {
        let (_tmp, mut host, mut reg, payloads) =
            make_host_and_registry("cleanup_mid_battle", build_host_test_maps());
        let addr1: SocketAddr = "127.0.0.1:40001".parse().unwrap();
        let addr2: SocketAddr = "127.0.0.1:40002".parse().unwrap();
        let addr3: SocketAddr = "127.0.0.1:40003".parse().unwrap();

        let mut s1 = make_session_for("p1@t", 1, addr1, payloads.clone());
        let mut s2 = make_session_for("p2@t", 2, addr2, payloads.clone());
        let mut s3 = make_session_for("p3@t", 3, addr3, payloads.clone());

        host.on_enable_entities(addr1, &mut s1, &mut ElementEncoder::new(), &mut reg)
            .unwrap();
        host.on_enable_entities(addr2, &mut s2, &mut ElementEncoder::new(), &mut reg)
            .unwrap();
        host.on_enable_entities(addr3, &mut s3, &mut ElementEncoder::new(), &mut reg)
            .unwrap();

        let db1 = host.account_for_addr(addr1).unwrap();
        let db2 = host.account_for_addr(addr2).unwrap();
        let db3 = host.account_for_addr(addr3).unwrap();

        let mut cw = bw201::stream::BinaryWriter::new();
        cw.i32(3);
        cw.i32(900);
        cw.u8(1);
        cw.string("room");
        host.on_create_training(addr1, cw.as_slice(), &mut reg);
        let prb_id = host.prebattle.room_for_account(db1).unwrap();

        let now = Instant::now();
        host.try_handle_hangar_do_cmd(
            addr2,
            &DecodedDoCmd {
                request_id: 1,
                command_id: cmd::CMD_PRB_JOIN,
                args: DoCmdArgs::Int3([prb_id, 0, 0]),
            },
            &mut reg,
            now,
        );
        host.try_handle_hangar_do_cmd(
            addr3,
            &DecodedDoCmd {
                request_id: 2,
                command_id: cmd::CMD_PRB_JOIN,
                args: DoCmdArgs::Int3([prb_id, 0, 0]),
            },
            &mut reg,
            now,
        );

        for (addr, db) in [(addr1, db1), (addr2, db2), (addr3, db3)] {
            host.try_handle_hangar_do_cmd(
                addr,
                &DecodedDoCmd {
                    request_id: 3,
                    command_id: cmd::CMD_PRB_ASSIGN,
                    args: DoCmdArgs::Int3([
                        db as i32,
                        wot_battle::lobby::PREBATTLE_ROSTER_ASSIGNED_IN_TEAM1 as i32,
                        0,
                    ]),
                },
                &mut reg,
                now,
            );
            host.try_handle_hangar_do_cmd(
                addr,
                &DecodedDoCmd {
                    request_id: 4,
                    command_id: cmd::CMD_PRB_READY,
                    args: DoCmdArgs::Int3([0, 0, 0]),
                },
                &mut reg,
                now,
            );
        }
        host.try_handle_hangar_do_cmd(
            addr1,
            &DecodedDoCmd {
                request_id: 5,
                command_id: cmd::CMD_PRB_TEAM_READY,
                args: DoCmdArgs::Int3([1, 1, 0]),
            },
            &mut reg,
            now,
        );
        host.try_handle_hangar_do_cmd(
            addr1,
            &DecodedDoCmd {
                request_id: 6,
                command_id: cmd::CMD_PRB_TEAM_READY,
                args: DoCmdArgs::Int3([2, 1, 0]),
            },
            &mut reg,
            now,
        );

        s1.set_entities_enabled(false);
        s2.set_entities_enabled(false);
        s3.set_entities_enabled(false);
        host.on_enable_entities(addr1, &mut s1, &mut ElementEncoder::new(), &mut reg)
            .unwrap();
        host.on_enable_entities(addr2, &mut s2, &mut ElementEncoder::new(), &mut reg)
            .unwrap();
        host.on_enable_entities(addr3, &mut s3, &mut ElementEncoder::new(), &mut reg)
            .unwrap();

        // Guest p3 disconnects while p1 and p2 are still InArena:
        // neither p1 nor p2 may receive Account RPC UpdatePrebattle while InArena.
        let p3_drop_actions = host.handle_disconnect_addr(addr3, &mut reg);
        assert!(
            !p3_drop_actions.iter().any(|a| matches!(a.instruction, PeerInstruction::UpdatePrebattle { .. })),
            "InArena peers must never receive UpdatePrebattle"
        );

        // Creator p1 returns to Hangar while guest p2 stays in arena
        host.on_avatar_method(addr1, crate::protocol::base_method::AVATAR_LEAVE_ARENA, &[], &mut reg, now);
        s1.set_entities_enabled(false);
        host.on_enable_entities(addr1, &mut s1, &mut ElementEncoder::new(), &mut reg)
            .unwrap();
        assert!(host.prebattle.room(prb_id).unwrap().in_battle);

        // Last human in arena (p2) disconnects mid-battle -> room.in_battle must reset to false
        // and p1 (in Hangar) must receive UpdatePrebattle notifications!
        let p2_drop_actions = host.handle_disconnect_addr(addr2, &mut reg);
        assert!(
            !host.prebattle.room(prb_id).unwrap().in_battle,
            "mid-battle disconnect of last arena human must clear prebattle in_battle"
        );
        assert!(
            p2_drop_actions.iter().any(|a| a.account_db_id == db1
                && matches!(a.instruction, PeerInstruction::UpdatePrebattle { .. })),
            "Hangar member p1 must receive prebattle updates when arena finishes on disconnect"
        );
    }

    #[test]
    fn prb_ready_switches_vehicle_without_lock_leak_and_rejects_outside_room() {
        let (_tmp, mut host, mut reg, payloads) =
            make_host_and_registry("prb_ready_locks", build_host_test_maps());
        let addr1: SocketAddr = "127.0.0.1:40011".parse().unwrap();
        let mut s1 = make_session_for("p1@t", 1, addr1, payloads);
        host.on_enable_entities(addr1, &mut s1, &mut ElementEncoder::new(), &mut reg)
            .unwrap();
        let now = Instant::now();

        let prof = reg.get_profile("p1@t").unwrap().clone();
        let mut inv_ids: Vec<i64> = prof.vehicles.keys().copied().collect();
        inv_ids.sort();
        assert!(inv_ids.len() >= 2, "Retail preset provides multiple vehicles");
        let v1 = inv_ids[0];
        let v2 = inv_ids[1];

        // 1. CMD_PRB_READY outside a prebattle room must reject without locking any vehicle
        let out_no_room = host
            .try_handle_hangar_do_cmd(
                addr1,
                &DecodedDoCmd {
                    request_id: 120,
                    command_id: cmd::CMD_PRB_READY,
                    args: DoCmdArgs::Int3([v1 as i32, 0, 0]),
                },
                &mut reg,
                now,
            )
            .unwrap();
        assert!(out_no_room.iter().any(|a| matches!(
            a.instruction,
            PeerInstruction::CmdResponse {
                result_id,
                ..
            } if result_id < 0
        )));
        assert_eq!(reg.get_profile("p1@t").unwrap().vehicles[&v1].lock, LOCK_REASON_NONE);

        // 2. Join training room, ready v1, then switch to v2 -> v1 must be unlocked
        let mut cw = bw201::stream::BinaryWriter::new();
        cw.i32(3);
        cw.i32(900);
        cw.u8(1);
        cw.string("room");
        host.on_create_training(addr1, cw.as_slice(), &mut reg);

        host.try_handle_hangar_do_cmd(
            addr1,
            &DecodedDoCmd {
                request_id: 121,
                command_id: cmd::CMD_PRB_READY,
                args: DoCmdArgs::Int3([v1 as i32, 0, 0]),
            },
            &mut reg,
            now,
        );
        assert_eq!(
            reg.get_profile("p1@t").unwrap().vehicles[&v1].lock,
            LOCK_REASON_PREBATTLE
        );

        let switch_actions = host
            .try_handle_hangar_do_cmd(
                addr1,
                &DecodedDoCmd {
                    request_id: 122,
                    command_id: cmd::CMD_PRB_READY,
                    args: DoCmdArgs::Int3([v2 as i32, 0, 0]),
                },
                &mut reg,
                now,
            )
            .unwrap();
        assert_eq!(
            reg.get_profile("p1@t").unwrap().vehicles[&v1].lock,
            LOCK_REASON_NONE,
            "previous vehicle lock must be released when readying a different vehicle"
        );
        assert_eq!(
            reg.get_profile("p1@t").unwrap().vehicles[&v2].lock,
            LOCK_REASON_PREBATTLE
        );
        assert!(switch_actions
            .iter()
            .filter(|a| matches!(a.instruction, PeerInstruction::AccountUpdate(_)))
            .count()
            >= 2);
    }

    #[test]
    fn launch_arena_map_load_failure_clears_in_battle_and_emits_prebattle_updates() {
        let tmp = TempHostDir::new("map_fail_launch");
        write_synthetic_host_assets(&tmp.0, true);
        let payloads = Arc::new(
            AccountPayloads::load(
                &tmp.0,
                &crate::payloads::PayloadOverrides::default(),
            )
            .unwrap(),
        );
        let reg_econ = Arc::new(wot065_data::economics::EconomicsConfig::default());
        let mut reg = AccountRegistry::open(
            &tmp.0.join("accounts"),
            payloads.catalog.clone(),
            reg_econ,
            wot065_data::account_state::Preset::Retail,
        )
        .unwrap();
        let maps = Arc::new(MapStore::new(tmp.0.join("res"), BalanceConfig::default()).unwrap());
        let mut host = BattleHost::new(BattleHostConfig::default(), maps);

        let addr1: SocketAddr = "127.0.0.1:40021".parse().unwrap();
        let mut s1 = make_session_for("p1@t", 1, addr1, payloads);
        host.on_enable_entities(addr1, &mut s1, &mut ElementEncoder::new(), &mut reg)
            .unwrap();
        let db1 = host.account_for_addr(addr1).unwrap();

        let mut cw = bw201::stream::BinaryWriter::new();
        cw.i32(99); // valid in _list_.xml, missing spaces/missing_map chunks
        cw.i32(900);
        cw.u8(1);
        cw.string("room");
        host.on_create_training(addr1, cw.as_slice(), &mut reg);
        let prb_id = host.prebattle.room_for_account(db1).unwrap();
        let now = Instant::now();

        host.try_handle_hangar_do_cmd(
            addr1,
            &DecodedDoCmd {
                request_id: 1,
                command_id: cmd::CMD_PRB_READY,
                args: DoCmdArgs::Int3([0, 0, 0]),
            },
            &mut reg,
            now,
        );
        let mut actions = host
            .try_handle_hangar_do_cmd(
                addr1,
                &DecodedDoCmd {
                    request_id: 2,
                    command_id: cmd::CMD_PRB_TEAM_READY,
                    args: DoCmdArgs::Int3([1, 1, 0]),
                },
                &mut reg,
                now,
            )
            .unwrap();
        actions.extend(
            host.try_handle_hangar_do_cmd(
                addr1,
                &DecodedDoCmd {
                    request_id: 3,
                    command_id: cmd::CMD_PRB_TEAM_READY,
                    args: DoCmdArgs::Int3([2, 1, 0]),
                },
                &mut reg,
                now,
            )
            .unwrap(),
        );
        assert!(!host.prebattle.room(prb_id).unwrap().in_battle);
        assert!(actions
            .iter()
            .any(|a| matches!(a.instruction, PeerInstruction::OnArenaJoinFailure(_))));
        let last_team_states = actions
            .iter()
            .rev()
            .find_map(|a| match &a.instruction {
                PeerInstruction::UpdatePrebattle {
                    update_type: PREBATTLE_UPDATE_TEAM_STATES,
                    payload,
                } => Some(wot065_data::pickle::loads(payload).unwrap()),
                _ => None,
            })
            .expect("failed arena launch must emit PREBATTLE_UPDATE_TEAM_STATES");
        let ts = last_team_states.as_list().unwrap();
        assert_eq!(
            ts[0].as_int(),
            Some(wot_battle::lobby::PREBATTLE_TEAM_STATE_NOT_READY as i64),
            "notify_arena_finished must reset team1 state back to NOT_READY(1)"
        );
        assert_eq!(
            ts[1].as_int(),
            Some(wot_battle::lobby::PREBATTLE_TEAM_STATE_READY as i64)
        );
    }

    #[test]
    fn force_queue_rejected_when_not_queued() {
        let (_tmp, mut host, mut reg, payloads) =
            make_host_and_registry("force_queue_auth", build_host_test_maps());
        let addr1: SocketAddr = "127.0.0.1:40041".parse().unwrap();
        let addr2: SocketAddr = "127.0.0.1:40042".parse().unwrap();
        let mut s1 = make_session_for("p1@t", 1, addr1, payloads.clone());
        let mut s2 = make_session_for("p2@t", 2, addr2, payloads);
        host.on_enable_entities(addr1, &mut s1, &mut ElementEncoder::new(), &mut reg)
            .unwrap();
        host.on_enable_entities(addr2, &mut s2, &mut ElementEncoder::new(), &mut reg)
            .unwrap();
        let now = Instant::now();
        let v1 = *reg
            .get_profile("p1@t")
            .unwrap()
            .vehicles
            .keys()
            .next()
            .unwrap();

        // Only p1 enqueues
        host.try_handle_hangar_do_cmd(
            addr1,
            &DecodedDoCmd {
                request_id: -1,
                command_id: cmd::CMD_ENQUEUE_FOR_ARENA,
                args: DoCmdArgs::Int3([v1 as i32, 0, 1]),
            },
            &mut reg,
            now,
        );

        // Unqueued p2 attempts CMD_FORCE_QUEUE -> must be rejected cleanly
        let p2_res = host
            .try_handle_hangar_do_cmd(
                addr2,
                &DecodedDoCmd {
                    request_id: 200,
                    command_id: cmd::CMD_FORCE_QUEUE,
                    args: DoCmdArgs::Int3([0, 0, 0]),
                },
                &mut reg,
                now,
            )
            .unwrap();
        assert!(p2_res.iter().any(|a| matches!(
            a.instruction,
            PeerInstruction::CmdResponse {
                request_id: 200,
                result_id,
            } if result_id == cmd::RES_FAILURE
        )));
        assert!(!p2_res
            .iter()
            .any(|a| matches!(a.instruction, PeerInstruction::StartArenaSwitch)));
        assert!(host.queue.contains(host.account_for_addr(addr1).unwrap()));
    }

    #[test]
    fn build_peer_unreliable_snapshots_splits_oversized_vehicle_batch_within_mtu() {
        let vehicles: Vec<wot_battle::world::VehicleState> = (0..150)
            .map(|i| wot_battle::world::VehicleState {
                entity_id: 200 + i,
                account_db_id: (i as i64) + 1,
                player_name: format!("P{i}"),
                veh_inv_id: (i as i64) + 1,
                veh_comp_descr: vec![0x01; 15],
                team: if i % 2 == 0 { 1 } else { 2 },
                prebattle_id: 0,
                is_bot: i > 0,
                is_alive: true,
                is_avatar_ready: true,
                is_team_killer: false,
                health: 100,
                max_health: 100,
                position: if i == 149 {
                    glam::Vec3::new(760.0, 4.0, -250.0)
                } else {
                    glam::Vec3::new(200.0 + i as f32, 4.0, -250.0 - i as f32)
                },
                yaw: 0.1,
                pitch: 0.0,
                roll: 0.0,
                speed: 0.0,
                rspeed: 0.0,
                move_flags: 0,
                engine_mode: [0, 0],
                is_crew_active: true,
                left_arena: false,
                damage_stickers: Vec::new(),
                is_burning: false,
            })
            .collect();

        let batches = build_peer_unreliable_snapshots(1, 200, &vehicles);
        assert!(
            batches.len() >= 2,
            "150 vehicles must split across multiple MTU-bounded unreliable packets, got {}",
            batches.len()
        );

        let cli = bw201::interfaces::client_interface();
        let mut total_msgs = 0usize;
        let mut saw_distant_vehicle = false;
        for enc in batches {
            assert!(
                enc.body().len() <= bw201::bundle::DEFAULT_MAX_FRAGMENT_BODY,
                "snapshot body {} exceeded DEFAULT_MAX_FRAGMENT_BODY",
                enc.body().len()
            );
            let pkt = enc.finish();
            let msgs =
                bw201::bundle::decode_messages(&pkt, &cli, bw201::packet::Endian::Little).unwrap();
            assert_eq!(msgs[0].id, bw201::interfaces::client_ids::TICK_SYNC);
            assert_eq!(msgs[0].data, [200]);
            assert_eq!(msgs[1].id, bw201::interfaces::client_ids::RELATIVE_POSITION);
            let mut reference = bw201::stream::BinaryReader::new(&msgs[1].data);
            assert_eq!(reference.f32().unwrap(), 200.0);
            assert_eq!(reference.f32().unwrap(), 4.0);
            assert_eq!(reference.f32().unwrap(), -250.0);
            for message in &msgs[2..] {
                if message.id == bw201::interfaces::client_ids::DETAILED_POSITION {
                    let mut reader = bw201::stream::BinaryReader::new(&message.data);
                    let pose = bw201::world::DetailedPosition::read_from(&mut reader).unwrap();
                    assert_eq!(pose.id, 349);
                    assert_eq!(pose.position, [760.0, 4.0, -250.0]);
                    saw_distant_vehicle = true;
                }
            }
            total_msgs += msgs.len() - 2;
        }
        assert_eq!(total_msgs, 150);
        assert!(saw_distant_vehicle);
    }

    #[test]
    fn switching_to_arena_fallback_on_missing_arena_restores_hangar_cleanly() {
        let (_tmp, mut host, mut reg, payloads) =
            make_host_and_registry("arena_missing_fallback", build_host_test_maps());
        let addr1: SocketAddr = "127.0.0.1:40051".parse().unwrap();
        let mut s1 = make_session_for("p1@t", 1, addr1, payloads);
        host.on_enable_entities(addr1, &mut s1, &mut ElementEncoder::new(), &mut reg)
            .unwrap();
        let db1 = host.account_for_addr(addr1).unwrap();
        let old_player_id = s1.player_id();
        let rev_before_launch = s1.account_rev();

        let mut cw = bw201::stream::BinaryWriter::new();
        cw.i32(3);
        cw.i32(900);
        cw.u8(1);
        cw.string("fallback room");
        host.on_create_training(addr1, cw.as_slice(), &mut reg);
        let prb_id = host.prebattle.room_for_account(db1).unwrap();
        let now = Instant::now();
        host.try_handle_hangar_do_cmd(
            addr1,
            &DecodedDoCmd {
                request_id: 1,
                command_id: cmd::CMD_PRB_READY,
                args: DoCmdArgs::Int3([0, 0, 0]),
            },
            &mut reg,
            now,
        );
        host.try_handle_hangar_do_cmd(
            addr1,
            &DecodedDoCmd {
                request_id: 2,
                command_id: cmd::CMD_PRB_TEAM_READY,
                args: DoCmdArgs::Int3([1, 1, 0]),
            },
            &mut reg,
            now,
        );
        host.try_handle_hangar_do_cmd(
            addr1,
            &DecodedDoCmd {
                request_id: 3,
                command_id: cmd::CMD_PRB_TEAM_READY,
                args: DoCmdArgs::Int3([2, 1, 0]),
            },
            &mut reg,
            now,
        );
        let avatar_id = match host.peers[&db1].state {
            SessionState::SwitchingToArena { avatar_id, .. } => avatar_id,
            ref other => panic!("expected SwitchingToArena, got {other:?}"),
        };
        assert!(host.prebattle.room(prb_id).unwrap().in_battle);

        // Simulate arena disappearing before ENABLE_ENTITIES arrives.
        let (_, _, reclaimed) = host.world.leave_player(db1).unwrap();
        for eid in reclaimed {
            host.alloc.release(eid);
        }

        s1.set_entities_enabled(false);
        let mut enc = ElementEncoder::new();
        let _actions = host
            .on_enable_entities(addr1, &mut s1, &mut enc, &mut reg)
            .unwrap();

        assert!(s1.is_entities_enabled());
        assert_ne!(s1.player_id(), old_player_id);
        assert_ne!(s1.player_id(), avatar_id);
        let rec = &host.peers[&db1];
        assert!(matches!(rec.state, SessionState::Hangar));
        assert_eq!(rec.player_id, s1.player_id());
        assert!(rec.locked_veh_inv_id.is_none());

        let prof = reg.get_profile("p1@t").unwrap();
        assert!(prof.vehicles.values().all(|v| v.lock == LOCK_REASON_NONE));
        assert_eq!(s1.account_rev(), prof.rev);
        assert!(s1.account_rev() > rev_before_launch);
        assert!(!host.prebattle.room(prb_id).unwrap().in_battle);
        assert!(host
            .prebattle
            .room(prb_id)
            .unwrap()
            .members
            .iter()
            .any(|m| m.db_id == db1 && m.actor_id == s1.player_id() as i64));

        let pkt = enc.finish();
        let msgs = bw201::bundle::decode_messages(
            &pkt,
            &bw201::interfaces::client_interface(),
            bw201::packet::Endian::Little,
        )
        .unwrap();
        let set_time = msgs
            .iter()
            .find(|m| m.id == bw201::interfaces::client_ids::SET_GAME_TIME)
            .expect("SET_GAME_TIME(0) must be emitted on fallback");
        assert_eq!(bw201::stream::BinaryReader::new(&set_time.data).u32().unwrap(), 0);
        let create_bp = msgs
            .iter()
            .find(|m| m.id == bw201::interfaces::client_ids::CREATE_BASE_PLAYER)
            .expect("CREATE_BASE_PLAYER must be emitted on fallback");
        let mut br = bw201::stream::BinaryReader::new(&create_bp.data);
        assert_eq!(br.i32().unwrap(), s1.player_id());
        assert_eq!(br.u16().unwrap(), crate::protocol::ACCOUNT_ENTITY_TYPE_ID);
        assert!(msgs.iter().any(|m| m.id == client_method::SHOW_GUI));
        assert!(msgs.iter().any(|m| m.id == client_method::UPDATE_PREBATTLE));
    }

    #[test]
    fn switching_to_arena_fallback_on_missing_player_vehicle_restores_hangar_cleanly() {
        let (_tmp, mut host, mut reg, payloads) =
            make_host_and_registry("veh_missing_fallback", build_host_test_maps());
        let addr1: SocketAddr = "127.0.0.1:40052".parse().unwrap();
        let mut s1 = make_session_for("p1@t", 1, addr1, payloads);
        host.on_enable_entities(addr1, &mut s1, &mut ElementEncoder::new(), &mut reg)
            .unwrap();
        let db1 = host.account_for_addr(addr1).unwrap();
        let old_player_id = s1.player_id();
        let rev_before_launch = s1.account_rev();

        let mut cw = bw201::stream::BinaryWriter::new();
        cw.i32(3);
        cw.i32(900);
        cw.u8(1);
        cw.string("fallback room 2");
        host.on_create_training(addr1, cw.as_slice(), &mut reg);
        let prb_id = host.prebattle.room_for_account(db1).unwrap();
        let now = Instant::now();
        host.try_handle_hangar_do_cmd(
            addr1,
            &DecodedDoCmd {
                request_id: 1,
                command_id: cmd::CMD_PRB_READY,
                args: DoCmdArgs::Int3([0, 0, 0]),
            },
            &mut reg,
            now,
        );
        host.try_handle_hangar_do_cmd(
            addr1,
            &DecodedDoCmd {
                request_id: 2,
                command_id: cmd::CMD_PRB_TEAM_READY,
                args: DoCmdArgs::Int3([1, 1, 0]),
            },
            &mut reg,
            now,
        );
        host.try_handle_hangar_do_cmd(
            addr1,
            &DecodedDoCmd {
                request_id: 3,
                command_id: cmd::CMD_PRB_TEAM_READY,
                args: DoCmdArgs::Int3([2, 1, 0]),
            },
            &mut reg,
            now,
        );
        let (space_id, avatar_id) = match host.peers[&db1].state {
            SessionState::SwitchingToArena {
                space_id,
                avatar_id,
                ..
            } => (space_id, avatar_id),
            ref other => panic!("expected SwitchingToArena, got {other:?}"),
        };

        // Remove only the human player's vehicle from the live arena.
        host.world
            .get_mut(space_id)
            .unwrap()
            .vehicles
            .retain(|v| v.account_db_id != db1);

        s1.set_entities_enabled(false);
        let mut enc = ElementEncoder::new();
        let _actions = host
            .on_enable_entities(addr1, &mut s1, &mut enc, &mut reg)
            .unwrap();

        assert!(s1.is_entities_enabled(), "client must not be stranded on loading screen");
        assert_ne!(s1.player_id(), old_player_id);
        assert_ne!(s1.player_id(), avatar_id);
        let rec = &host.peers[&db1];
        assert!(matches!(rec.state, SessionState::Hangar));
        assert_eq!(rec.player_id, s1.player_id());
        assert!(rec.locked_veh_inv_id.is_none());

        let prof = reg.get_profile("p1@t").unwrap();
        assert!(prof.vehicles.values().all(|v| v.lock == LOCK_REASON_NONE));
        assert_eq!(s1.account_rev(), prof.rev);
        assert!(s1.account_rev() > rev_before_launch);
        assert!(!host.prebattle.room(prb_id).unwrap().in_battle);
        assert!(host.world.get(space_id).is_none());

        let pkt = enc.finish();
        let msgs = bw201::bundle::decode_messages(
            &pkt,
            &bw201::interfaces::client_interface(),
            bw201::packet::Endian::Little,
        )
        .unwrap();
        let set_time = msgs
            .iter()
            .find(|m| m.id == bw201::interfaces::client_ids::SET_GAME_TIME)
            .expect("SET_GAME_TIME(0) must be emitted on vehicle-missing fallback");
        assert_eq!(bw201::stream::BinaryReader::new(&set_time.data).u32().unwrap(), 0);
        assert!(msgs
            .iter()
            .any(|m| m.id == bw201::interfaces::client_ids::CREATE_BASE_PLAYER));
        assert!(msgs.iter().any(|m| m.id == client_method::SHOW_GUI));
    }

    #[test]
    fn aoi_gating_prevents_unspotted_enemy_leaks_and_routes_damage_info_to_victim() {
        let (_tmp, mut host, mut reg, payloads) =
            make_host_and_registry("aoi_gate_test", build_host_test_maps());
        let addr1: SocketAddr = "127.0.0.1:40061".parse().unwrap();
        let mut s1 = make_session_for("p1@t", 1, addr1, payloads);
        host.on_enable_entities(addr1, &mut s1, &mut ElementEncoder::new(), &mut reg)
            .unwrap();
        let db1 = host.account_for_addr(addr1).unwrap();

        let mut cw = bw201::stream::BinaryWriter::new();
        cw.i32(3);
        cw.i32(900);
        cw.u8(1);
        cw.string("aoi room");
        host.on_create_training(addr1, cw.as_slice(), &mut reg);
        let now = Instant::now();
        host.try_handle_hangar_do_cmd(
            addr1,
            &DecodedDoCmd {
                request_id: 1,
                command_id: cmd::CMD_PRB_READY,
                args: DoCmdArgs::Int3([0, 0, 0]),
            },
            &mut reg,
            now,
        );
        host.try_handle_hangar_do_cmd(
            addr1,
            &DecodedDoCmd {
                request_id: 2,
                command_id: cmd::CMD_PRB_TEAM_READY,
                args: DoCmdArgs::Int3([1, 1, 0]),
            },
            &mut reg,
            now,
        );
        host.try_handle_hangar_do_cmd(
            addr1,
            &DecodedDoCmd {
                request_id: 3,
                command_id: cmd::CMD_PRB_TEAM_READY,
                args: DoCmdArgs::Int3([2, 1, 0]),
            },
            &mut reg,
            now,
        );
        let space_id = match host.peers[&db1].state {
            SessionState::SwitchingToArena { space_id, .. } => space_id,
            ref other => panic!("expected SwitchingToArena, got {other:?}"),
        };
        s1.set_entities_enabled(false);
        host.on_enable_entities(addr1, &mut s1, &mut ElementEncoder::new(), &mut reg)
            .unwrap();

        let (p1_veh_id, enemy_bot_id) = {
            let arena = host.world.get_mut(space_id).unwrap();
            let p1_id = arena.vehicle_for_account(db1).unwrap().entity_id;
            let bot_id = arena.vehicles.iter().find(|v| v.is_bot).unwrap().entity_id;
            // Ensure enemy bot is NOT spotted by p1
            arena.visibility.previous_aoi.clear();
            arena.last_visibility.spotted_enemies.clear();

            // Queue an unspotted enemy shot, impact on the unspotted enemy, and two damage events:
            // one targeting the unspotted enemy (must NOT go to p1) and one targeting p1 (MUST go to p1 with attacker_id = bot_id).
            arena.last_shots.push(wot_battle::world::arena::ShotFiredEvent {
                shot_id: 77,
                attacker_id: bot_id,
                muzzle_pos: glam::Vec3::new(80.0, 5.0, 80.0),
                shot_dir: glam::Vec3::new(-1.0, 0.0, 0.0),
                speed: 500.0,
                gravity: 9.81,
            });
            arena.last_impact_outcomes.push(wot_battle::sim::ImpactOutcome {
                shot_id: 77,
                attacker_id: p1_id,
                hit_point: glam::Vec3::new(80.0, 5.0, 80.0),
                normal: glam::Vec3::new(1.0, 0.0, 0.0),
                shot_result: 2,
                struck_entity_id: Some(bot_id),
                component_idx: 1,
                encoded_segment: 0xDEAD_BEEF,
                armor_damage: 10,
                splash_hits: Vec::new(),
                effect_mat_idx: 0,
                internal_ray: None,
                destructible_deltas: Vec::new(),
                ends_flight: true,
            });
            arena.last_damage_events.push(wot_battle::sim::DamageInfoEvent {
                target_id: bot_id,
                attacker_id: p1_id,
                code: wot_battle::sim::DamageInfoCode::DeviceCriticalAtShot,
                extra_index: 3,
            });
            arena.last_damage_events.push(wot_battle::sim::DamageInfoEvent {
                target_id: p1_id,
                attacker_id: bot_id,
                code: wot_battle::sim::DamageInfoCode::DeviceDestroyedAtShot,
                extra_index: 5,
            });
            (p1_id, bot_id)
        };

        // Call tick with dt_secs == 0.0 (first tick sets last_step_at without overwriting our injected events)
        let (actions, snapshots, _) = host.tick(now, &mut reg);

        // 1. Unspotted enemy must NOT appear in unreliable snapshots for p1
        assert_eq!(snapshots.len(), 1);
        assert!(snapshots[0].3.iter().all(|v| v.entity_id != enemy_bot_id));

        // 2. Unspotted enemy must NOT leak VehicleShowShooting, VehicleShowDamageFromShot, or UpdateEntity to p1
        assert!(!actions.iter().any(|a| matches!(
            a.instruction,
            PeerInstruction::VehicleShowShooting { vehicle_id } if vehicle_id == enemy_bot_id
        )));
        assert!(!actions.iter().any(|a| matches!(
            a.instruction,
            PeerInstruction::VehicleShowDamageFromShot { vehicle_id, .. } if vehicle_id == enemy_bot_id
        )));
        assert!(!actions.iter().any(|a| matches!(
            a.instruction,
            PeerInstruction::UpdateEntity { entity_id, .. } if entity_id == enemy_bot_id
        )));

        // 3. Tracer and StopTracer ARE still delivered
        assert!(actions.iter().any(|a| matches!(
            a.instruction,
            PeerInstruction::AvatarShowTracer { shot_id: 77, .. }
        )));
        assert!(actions.iter().any(|a| matches!(
            a.instruction,
            PeerInstruction::AvatarStopTracer { shot_id: 77, .. }
        )));

        // 4. AvatarDamageInfo is ONLY sent for p1's own vehicle (target_id == p1_veh_id), with extra_index == 5 and entity_id == enemy_bot_id (attacker)
        let dmg_infos: Vec<_> = actions
            .iter()
            .filter_map(|a| match a.instruction {
                PeerInstruction::AvatarDamageInfo {
                    code,
                    extra_index,
                    entity_id,
                    ..
                } => Some((code, extra_index, entity_id)),
                _ => None,
            })
            .collect();
        assert_eq!(
            dmg_infos,
            vec![(
                wot_battle::sim::DamageInfoCode::DeviceDestroyedAtShot as u8,
                5u8,
                enemy_bot_id
            )]
        );
        let _ = p1_veh_id;
    }

    #[test]
    fn encode_vehicle_update_props_includes_engine_mode_and_extra_fire() {
        let v = VehicleState {
            entity_id: 201,
            account_db_id: 1,
            player_name: "P1".into(),
            veh_inv_id: 1,
            veh_comp_descr: vec![0x01; 15],
            team: 1,
            prebattle_id: 0,
            is_bot: false,
            is_alive: true,
            is_avatar_ready: true,
            is_team_killer: false,
            health: 85,
            max_health: 100,
            position: glam::Vec3::new(0.0, 5.0, 0.0),
            yaw: 0.0,
            pitch: 0.0,
            roll: 0.0,
            speed: 0.0,
            rspeed: 0.0,
            move_flags: 1,
            engine_mode: [3, 1],
            is_crew_active: true,
            left_arena: false,
            damage_stickers: Vec::new(),
            is_burning: true,
        };

        let props = encode_vehicle_update_props(&v);
        assert_eq!(props[0], 5, "should encode 5 properties");
        // Last two bytes: length 1 (i32 = 1,0,0,0) followed by EXTRA_FIRE = 4u8
        assert_eq!(
            *props.last().unwrap(),
            wot_battle::sim::EXTRA_FIRE,
            "publicStateModifiers must encode EXTRA_FIRE (4), not EXTRA_SHOOT (1)"
        );
        // Property 3 (engineMode) must be present: [3, 3, 1]
        assert!(
            props.windows(3).any(|w| w == [3, 3, 1]),
            "encode_vehicle_update_props must include property 3 (engineMode = [3, 1]), got {:?}",
            props
        );
    }
}
