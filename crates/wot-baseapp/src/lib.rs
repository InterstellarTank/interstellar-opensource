//! WoT 0.6.5 async UDP BaseApp server (`tokio::net::UdpSocket`) with dynamic session handoff.

pub mod battle_host;
pub mod channel;
pub mod payloads;
pub mod protocol;
pub mod registry;

pub use battle_host::BattleHostConfig;

use std::collections::{HashMap, HashSet, VecDeque};
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};

use bw201::bundle::{DecodedMessage, ElementEncoder};
use bw201::interfaces::baseapp_ext_ids;
use bw201::login::LoginSession;
use bw201::stream::{BinaryReader, StreamError};
use bw201::world::RequestEntityUpdate;
use wot065_data::account_state::Preset;
use wot_battle::assets::map_store::MapStore;
use wot_battle::balance::BalanceConfig;

use crate::battle_host::{
    build_peer_unreliable_snapshots, encode_instruction, BattleHost, RoutedAction,
};
use crate::payloads::{AccountPayloads, PayloadOverrides};
use crate::protocol::{
    base_method, decode_do_cmd, emit_reset_entities, BaseAppSession, ProtocolError,
};
use crate::registry::AccountRegistry;

/// Configuration for running `BaseAppServer`.
#[derive(Debug, Clone)]
pub struct BaseAppConfig {
    pub bind: SocketAddr,
    pub session_file: PathBuf,
    pub game_dir: PathBuf,
    pub data_dir: PathBuf,
    pub preset: Preset,
    pub economics_config: Option<PathBuf>,
    pub overrides: PayloadOverrides,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionSnapshot {
    pub raw_json: String,
    pub modified: Option<SystemTime>,
    pub session: LoginSession,
}

/// Read and parse `LoginSession` from `path`, returning `Ok(None)` if not present yet.
pub fn try_load_session(path: &Path) -> anyhow::Result<Option<SessionSnapshot>> {
    match std::fs::metadata(path) {
        Ok(meta) => {
            let modified = meta.modified().ok();
            let raw_json = std::fs::read_to_string(path)?;
            let session: LoginSession = serde_json::from_str(&raw_json)?;
            Ok(Some(SessionSnapshot {
                raw_json,
                modified,
                session,
            }))
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e.into()),
    }
}

const MAX_REVOKED_KEYS: usize = 4096;

pub struct SessionRouter {
    session_file: PathBuf,
    payloads: AccountPayloads,
    registry: AccountRegistry,
    battle_host: BattleHost,
    last_snapshot: Option<SessionSnapshot>,
    known_logins: HashMap<u32, LoginSession>,
    active_owners: HashMap<String, (SocketAddr, u32)>,
    revoked_keys_queue: VecDeque<u32>,
    revoked_keys_set: HashSet<u32>,
    peers: HashMap<SocketAddr, BaseAppSession>,
}

impl SessionRouter {
    pub fn new(session_file: PathBuf, payloads: AccountPayloads, registry: AccountRegistry) -> Self {
        let empty_maps = Arc::new(MapStore::from_preloaded(Vec::new(), BalanceConfig::default()));
        let battle_host = BattleHost::new(BattleHostConfig::default(), empty_maps);
        Self::with_battle(session_file, payloads, registry, battle_host)
    }

    pub fn with_battle(
        session_file: PathBuf,
        payloads: AccountPayloads,
        registry: AccountRegistry,
        battle_host: BattleHost,
    ) -> Self {
        let mut router = Self {
            session_file,
            payloads,
            registry,
            battle_host,
            last_snapshot: None,
            known_logins: HashMap::new(),
            active_owners: HashMap::new(),
            revoked_keys_queue: VecDeque::new(),
            revoked_keys_set: HashSet::new(),
            peers: HashMap::new(),
        };
        router.reload_pending_sessions();
        router
    }

    fn revoke_session_key(&mut self, key: u32) {
        if self.revoked_keys_set.insert(key) {
            self.revoked_keys_queue.push_back(key);
            if self.revoked_keys_queue.len() > MAX_REVOKED_KEYS {
                if let Some(evicted) = self.revoked_keys_queue.pop_front() {
                    self.revoked_keys_set.remove(&evicted);
                }
            }
        }
    }

    fn reload_pending_sessions(&mut self) {
        // 1. Ingest per-session handoffs from `<session_file>.pending/*.json`
        let pending_dir = bw201::pending_session_dir(&self.session_file);
        if pending_dir.exists() {
            if let Ok(entries) = std::fs::read_dir(&pending_dir) {
                for entry in entries.flatten() {
                    let path = entry.path();
                    if path.is_file() && path.extension().and_then(|s| s.to_str()) == Some("json") {
                        if let Ok(raw) = std::fs::read_to_string(&path) {
                            if let Ok(session) = serde_json::from_str::<LoginSession>(&raw) {
                                let key = session.session_key;
                                let _ = std::fs::remove_file(&path);
                                if !self.revoked_keys_set.contains(&key) {
                                    tracing::info!(
                                        username = %session.username,
                                        session_key = key,
                                        path = %path.display(),
                                        "ingested pending login session handoff"
                                    );
                                    self.known_logins.insert(key, session);
                                }
                            }
                        }
                    }
                }
            }
        }

        // 2. Fallback check for legacy single session file
        match try_load_session(&self.session_file) {
            Ok(Some(snap)) => {
                if self.last_snapshot.as_ref() != Some(&snap) {
                    let key = snap.session.session_key;
                    if !self.revoked_keys_set.contains(&key) {
                        tracing::info!(
                            username = %snap.session.username,
                            session_key = key,
                            path = %self.session_file.display(),
                            "loaded fallback login session handoff"
                        );
                        self.known_logins.insert(key, snap.session.clone());
                    }
                    self.last_snapshot = Some(snap);
                }
            }
            Ok(None) => {}
            Err(err) => {
                tracing::debug!(
                    error = %err,
                    path = %self.session_file.display(),
                    "failed reading fallback session handoff"
                );
            }
        }
    }

    fn cleanup_battle_for_addr(
        &mut self,
        addr: SocketAddr,
        now: Instant,
        out: &mut Vec<(SocketAddr, Vec<u8>)>,
    ) {
        let actions = self
            .battle_host
            .handle_disconnect_addr(addr, &mut self.registry);
        self.dispatch_routed_actions(actions, now, out);
    }

    fn dispatch_routed_actions(
        &mut self,
        actions: Vec<RoutedAction>,
        now: Instant,
        out: &mut Vec<(SocketAddr, Vec<u8>)>,
    ) {
        let mut by_addr: HashMap<SocketAddr, Vec<RoutedAction>> = HashMap::new();
        for act in actions {
            by_addr.entry(act.addr).or_default().push(act);
        }
        for (target_addr, target_actions) in by_addr {
            if let Some(target_sess) = self.peers.get_mut(&target_addr) {
                let mut enc = ElementEncoder::new();
                for act in target_actions {
                    encode_instruction(target_sess, &mut enc, act.instruction);
                }
                for pkt in target_sess.send_reliable_encoder(&enc, now) {
                    out.push((target_addr, pkt));
                }
            } else {
                for act in target_actions {
                    tracing::debug!(
                        account_db_id = act.account_db_id,
                        %target_addr,
                        "dropping routed action for disconnected peer"
                    );
                }
            }
        }
    }

    fn activate_owner(
        &mut self,
        user_key: String,
        from: SocketAddr,
        new_key: u32,
        now: Instant,
        out: &mut Vec<(SocketAddr, Vec<u8>)>,
    ) {
        self.known_logins.remove(&new_key);
        if let Some((old_addr, old_key)) = self.active_owners.get(&user_key).copied() {
            if old_addr != from {
                self.cleanup_battle_for_addr(old_addr, now, out);
                if let Some(mut old_sess) = self.peers.remove(&old_addr) {
                    tracing::info!(
                        username = %user_key,
                        old_addr = %old_addr,
                        new_addr = %from,
                        "kicking previous session on relogin"
                    );
                    let kick_pkts = old_sess.kick(
                        "Another session logged in",
                        false,
                        0,
                        now,
                    );
                    for pkt in kick_pkts {
                        out.push((old_addr, pkt));
                    }
                }
                self.known_logins.remove(&old_key);
                self.revoke_session_key(old_key);
            } else if old_key != new_key {
                self.known_logins.remove(&old_key);
                self.revoke_session_key(old_key);
            }
        }
        self.active_owners.insert(user_key, (from, new_key));
    }

    fn remove_owner_for_peer(&mut self, user_key: &str, addr: SocketAddr) {
        if let Some((owner_addr, _)) = self.active_owners.get(user_key) {
            if *owner_addr == addr {
                self.active_owners.remove(user_key);
            }
        }
    }

    pub fn handle_datagram(
        &mut self,
        from: SocketAddr,
        data: &[u8],
        now: Instant,
    ) -> Vec<(SocketAddr, Vec<u8>)> {
        let online_players = self.active_owners.len().max(1) as u32;

        // Case 1: Existing connected peer
        if self.peers.contains_key(&from) {
            let mut deferred = Vec::new();
            let (result, just_enabled, user_info, is_disconnected) = {
                let sess = self.peers.get_mut(&from).unwrap();
                let was_enabled = sess.is_entities_enabled();
                let battle_host = &mut self.battle_host;
                let registry = &mut self.registry;
                let res = sess.process_datagram(data, now, |sess, msg, encoder| {
                    dispatch_peer_message(
                        from,
                        sess,
                        msg,
                        encoder,
                        battle_host,
                        registry,
                        online_players,
                        now,
                        &mut deferred,
                    )
                });
                let just_enabled = !was_enabled && sess.is_entities_enabled();
                let user_info = (
                    sess.login().username.trim().to_ascii_lowercase(),
                    sess.login().session_key,
                );
                (res, just_enabled, user_info, sess.is_disconnected())
            };

            match result {
                Ok(replies) => {
                    let mut out = Vec::new();
                    if just_enabled {
                        let (user_key, new_key) = user_info.clone();
                        self.activate_owner(user_key, from, new_key, now, &mut out);
                    }

                    self.dispatch_routed_actions(deferred, now, &mut out);

                    if is_disconnected {
                        self.cleanup_battle_for_addr(from, now, &mut out);
                        self.remove_owner_for_peer(&user_info.0, from);
                        self.peers.remove(&from);
                    }

                    for pkt in replies {
                        out.push((from, pkt));
                    }
                    return out;
                }
                Err(ProtocolError::Decrypt(_) | ProtocolError::SessionKeyMismatch { .. }) => {
                    // Client may have restarted with a new session on the same UDP port.
                }
                Err(err) => {
                    let prefix_hex = bw201::crypto::hex(&data[..data.len().min(64)]);
                    tracing::warn!(%from, len = data.len(), %prefix_hex, error = %err, "ignored invalid UDP datagram");
                    return Vec::new();
                }
            }
        }

        // Case 2: New peer
        self.reload_pending_sessions();
        if self.known_logins.is_empty() {
            tracing::warn!(%from, len = data.len(), "UDP datagram received before login session handoff appeared");
            return Vec::new();
        }

        let mut last_err = None;
        let logins: Vec<LoginSession> = self.known_logins.values().cloned().collect();
        for login in logins {
            if self.revoked_keys_set.contains(&login.session_key) {
                continue;
            }
            let mut sess = match BaseAppSession::new(login.clone(), self.payloads.clone()) {
                Ok(s) => s,
                Err(err) => {
                    tracing::warn!(
                        error = %err,
                        "invalid Blowfish key in login session handoff"
                    );
                    continue;
                }
            };
            sess.set_player_id(0);

            // Decrypt and verify handshake credentials BEFORE touching self.peers or BattleHost,
            // so failed candidate logins leave existing sessions on `from` untouched, while a
            // valid same-port reconnect cleans up any replaced peer before dispatching ENABLE_ENTITIES.
            let decoded = sess.decode_datagram_messages(data).and_then(|(msgs, ack)| {
                sess.validate_login_messages(&msgs)?;
                Ok((msgs, ack))
            });
            let (msgs, ack_needed) = match decoded {
                Ok(pair) => pair,
                Err(err @ (ProtocolError::Decrypt(_) | ProtocolError::SessionKeyMismatch { .. })) => {
                    last_err = Some(err);
                    continue;
                }
                Err(err) => {
                    let prefix_hex = bw201::crypto::hex(&data[..data.len().min(64)]);
                    tracing::warn!(%from, len = data.len(), %prefix_hex, error = %err, "ignored invalid UDP datagram");
                    return Vec::new();
                }
            };

            let mut out = Vec::new();
            if let Some(old_sess) = self.peers.remove(&from) {
                self.cleanup_battle_for_addr(from, now, &mut out);
                let old_user_key = old_sess.login().username.trim().to_ascii_lowercase();
                self.remove_owner_for_peer(&old_user_key, from);
                let old_key = old_sess.login().session_key;
                if old_key != login.session_key {
                    self.known_logins.remove(&old_key);
                    self.revoke_session_key(old_key);
                }
            }

            let online_players = self.active_owners.len().max(1) as u32;
            let was_enabled = sess.is_entities_enabled();
            let mut deferred = Vec::new();
            let mut encoder = ElementEncoder::new();
            let dispatch_res = {
                let battle_host = &mut self.battle_host;
                let registry = &mut self.registry;
                msgs.iter().try_for_each(|msg| {
                    dispatch_peer_message(
                        from,
                        &mut sess,
                        msg,
                        &mut encoder,
                        battle_host,
                        registry,
                        online_players,
                        now,
                        &mut deferred,
                    )
                })
            };
            if let Err(err) = dispatch_res {
                let prefix_hex = bw201::crypto::hex(&data[..data.len().min(64)]);
                tracing::warn!(%from, len = data.len(), %prefix_hex, error = %err, "ignored invalid UDP datagram");
                return out;
            }
            let replies = sess.encode_datagram_replies(&encoder, ack_needed, now);

            let user_key = sess.login().username.trim().to_ascii_lowercase();
            if !was_enabled && sess.is_entities_enabled() {
                let new_key = sess.login().session_key;
                self.activate_owner(user_key.clone(), from, new_key, now, &mut out);
            }

            self.dispatch_routed_actions(deferred, now, &mut out);

            if !sess.is_disconnected() {
                self.peers.insert(from, sess);
            } else {
                self.cleanup_battle_for_addr(from, now, &mut out);
                self.remove_owner_for_peer(&user_key, from);
            }
            for pkt in replies {
                out.push((from, pkt));
            }
            return out;
        }

        if let Some(err) = last_err {
            let prefix_hex = bw201::crypto::hex(&data[..data.len().min(64)]);
            tracing::warn!(%from, len = data.len(), %prefix_hex, error = %err, "ignored unauthenticated UDP datagram");
        }
        Vec::new()
    }

    pub fn due_retransmits(&mut self, now: Instant, rto: Duration) -> Vec<(SocketAddr, Vec<u8>)> {
        let mut out = Vec::new();
        for (&addr, sess) in &mut self.peers {
            for pkt in sess.due_retransmits(now, rto) {
                out.push((addr, pkt));
            }
        }
        let mut disconnected = Vec::new();
        self.peers.retain(|&addr, sess| {
            if sess.is_disconnected() {
                disconnected.push((sess.login().username.trim().to_ascii_lowercase(), addr));
                false
            } else {
                true
            }
        });
        for (user_key, addr) in disconnected {
            self.cleanup_battle_for_addr(addr, now, &mut out);
            self.remove_owner_for_peer(&user_key, addr);
        }
        out
    }

    /// Periodic tick: flush due retransmits, advance matchmaker queue, step BattleWorld,
    /// emit unreliable position snapshots, and enforce switching timeouts.
    pub fn tick(&mut self, now: Instant, rto: Duration) -> Vec<(SocketAddr, Vec<u8>)> {
        let mut out = self.due_retransmits(now, rto);
        let (actions, snapshots, expired_peers) = self.battle_host.tick(now, &mut self.registry);
        self.dispatch_routed_actions(actions, now, &mut out);

        for (addr, db_id, tick_byte, vehicles) in snapshots {
            if let Some(sess) = self.peers.get_mut(&addr) {
                for encoder in build_peer_unreliable_snapshots(db_id, tick_byte, &vehicles) {
                    if let Some(packet) = sess.send_unreliable_encoder(&encoder) {
                        out.push((addr, packet));
                    }
                }
            }
        }

        for (addr, user_key) in expired_peers {
            self.remove_owner_for_peer(&user_key, addr);
            self.peers.remove(&addr);
        }

        out
    }
}

#[allow(clippy::too_many_arguments)]
fn dispatch_peer_message(
    from: SocketAddr,
    sess: &mut BaseAppSession,
    msg: &DecodedMessage,
    encoder: &mut ElementEncoder,
    battle_host: &mut BattleHost,
    registry: &mut AccountRegistry,
    online_players: u32,
    now: Instant,
    deferred: &mut Vec<RoutedAction>,
) -> Result<(), ProtocolError> {
    if sess.handle_login_or_auth_msg(msg, encoder)? {
        return Ok(());
    }

    match msg.id {
        baseapp_ext_ids::ENABLE_ENTITIES => {
            if !sess.is_entities_enabled() {
                let actions = battle_host
                    .on_enable_entities(from, sess, encoder, registry)
                    .map_err(|_| StreamError::UnexpectedEof)?;
                route_actions_or_inline(from, sess, encoder, actions, deferred);
            }
        }
        baseapp_ext_ids::REQUEST_ENTITY_UPDATE => {
            if let Ok(req) = RequestEntityUpdate::read_from(&mut BinaryReader::new(&msg.data)) {
                battle_host.on_request_entity_update(from, req.id, encoder);
            }
        }
        baseapp_ext_ids::DISCONNECT_CLIENT => {
            tracing::info!(username = %sess.login().username, "client disconnected");
            sess.mark_disconnected();
        }
        baseapp_ext_ids::AVATAR_UPDATE_IMPLICIT
        | baseapp_ext_ids::AVATAR_UPDATE_EXPLICIT
        | baseapp_ext_ids::AVATAR_UPDATE_WARD_IMPLICIT
        | baseapp_ext_ids::AVATAR_UPDATE_WARD_EXPLICIT => {
            // The client controls the Avatar's pose. The arena drives Vehicles from moveWith.
        }
        base_method::CHAT_COMMAND => {
            let actions = battle_host.on_chat_command(from, &msg.data);
            route_actions_or_inline(from, sess, encoder, actions, deferred);
        }
        base_method::INVITE_COMMAND | base_method::ACK_COMMAND => {}
        _ if msg.id >= 0x80 && msg.id <= 0xBF && battle_host.is_in_arena(from) => {
            if msg.data.len() >= 4 {
                let mut r = bw201::stream::BinaryReader::new(&msg.data);
                if let Ok(entity_id) = r.i32() {
                    let method_idx = msg.id & 0x3F;
                    let rest = r.rest();
                    let actions = battle_host.on_cell_method(from, entity_id, method_idx, rest);
                    route_actions_or_inline(from, sess, encoder, actions, deferred);
                }
            }
        }
        _ if msg.id >= 0xC0 => {
            if battle_host.is_in_arena(from) {
                let (actions, left_arena) =
                    battle_host.on_avatar_method(from, msg.id, &msg.data, registry, now);
                route_actions_or_inline(from, sess, encoder, actions, deferred);
                if left_arena {
                    sess.set_entities_enabled(false);
                    emit_reset_entities(encoder, false);
                }
            } else if battle_host.is_switching(from) {
                tracing::debug!(id = msg.id, "ignoring entity method while switching spaces");
            } else {
                match msg.id {
                    base_method::CREATE_TRAINING => {
                        let actions = battle_host.on_create_training(from, &msg.data, registry);
                        route_actions_or_inline(from, sess, encoder, actions, deferred);
                    }
                    base_method::DO_CMD_STR
                    | base_method::DO_CMD_INT3
                    | base_method::DO_CMD_INT4
                    | base_method::DO_CMD_INT2_STR
                    | base_method::DO_CMD_INT_ARR => {
                        let decoded = decode_do_cmd(msg.id, &msg.data)?;
                        if let Some(actions) =
                            battle_host.try_handle_hangar_do_cmd(from, &decoded, registry, now)
                        {
                            route_actions_or_inline(from, sess, encoder, actions, deferred);
                        } else {
                            sess.handle_decoded_cmd(decoded, encoder, registry, online_players)?;
                        }
                    }
                    other => {
                        tracing::debug!(
                            id = other,
                            len = msg.data.len(),
                            "ignoring unhandled Account BaseMethod"
                        );
                    }
                }
            }
        }
        other => {
            tracing::debug!(
                id = other,
                len = msg.data.len(),
                "ignoring unhandled BaseAppExt message"
            );
        }
    }
    Ok(())
}

fn route_actions_or_inline(
    current_addr: SocketAddr,
    sess: &mut BaseAppSession,
    encoder: &mut ElementEncoder,
    actions: Vec<RoutedAction>,
    deferred: &mut Vec<RoutedAction>,
) {
    for act in actions {
        if act.addr == current_addr {
            encode_instruction(sess, encoder, act.instruction);
        } else {
            deferred.push(act);
        }
    }
}

fn load_map_store(game_dir: &Path, battle: &BattleHostConfig) -> Arc<MapStore> {
    if let Some(maps) = &battle.preloaded_maps {
        return Arc::clone(maps);
    }
    let res_dir = if game_dir.join("scripts/arena_defs/_list_.xml").exists() {
        Some(game_dir.to_path_buf())
    } else if game_dir.join("res/scripts/arena_defs/_list_.xml").exists() {
        Some(game_dir.join("res"))
    } else {
        None
    };
    match res_dir.map(|p| MapStore::new(p, battle.balance_config.clone())) {
        Some(Ok(store)) => Arc::new(store),
        Some(Err(err)) => {
            tracing::warn!(error = %err, "failed loading MapStore from game_dir; using empty fallback");
            Arc::new(MapStore::from_preloaded(
                Vec::new(),
                battle.balance_config.clone(),
            ))
        }
        None => Arc::new(MapStore::from_preloaded(
            Vec::new(),
            battle.balance_config.clone(),
        )),
    }
}

/// Run the UDP BaseApp loop until shutdown signal.
pub async fn serve(config: BaseAppConfig) -> anyhow::Result<()> {
    serve_with_ready(config, None).await
}

/// Run the UDP BaseApp loop and notify `ready` with the bound local address once listening.
pub async fn serve_with_ready(
    config: BaseAppConfig,
    ready: Option<tokio::sync::oneshot::Sender<SocketAddr>>,
) -> anyhow::Result<()> {
    serve_with_battle_and_ready(config, BattleHostConfig::default(), ready).await
}

/// Run the UDP BaseApp loop with explicit battle/lobby host configuration.
pub async fn serve_with_battle_and_ready(
    config: BaseAppConfig,
    mut battle: BattleHostConfig,
    ready: Option<tokio::sync::oneshot::Sender<SocketAddr>>,
) -> anyhow::Result<()> {
    let payloads = AccountPayloads::load_with_economics(
        &config.game_dir,
        &config.overrides,
        config.economics_config.as_deref(),
    )?;
    let registry = AccountRegistry::open(
        &config.data_dir,
        payloads.catalog.clone(),
        payloads.economics.clone(),
        config.preset,
    )?;
    battle.dev_features |= config.preset == Preset::Sandbox;
    let map_store = load_map_store(&config.game_dir, &battle);
    let tick_interval = battle.tick_interval.max(Duration::from_millis(10));
    let battle_host = BattleHost::new(battle, map_store);

    let socket = tokio::net::UdpSocket::bind(config.bind).await?;
    let local_addr = socket.local_addr()?;
    tracing::info!(
        bind = %local_addr,
        vehicles = payloads.vehicle_count,
        session_file = %config.session_file.display(),
        "wot-baseapp listening on UDP"
    );
    if let Some(tx) = ready {
        let _ = tx.send(local_addr);
    }

    let mut router = SessionRouter::with_battle(config.session_file, payloads, registry, battle_host);
    let mut buf = vec![0u8; 4096];
    let mut tick_timer = tokio::time::interval(tick_interval);
    let rto = Duration::from_millis(250);

    loop {
        tokio::select! {
            recv = socket.recv_from(&mut buf) => {
                let (len, from) = match recv {
                    Ok(pair) => pair,
                    Err(e) if e.raw_os_error() == Some(10054) || e.kind() == std::io::ErrorKind::ConnectionReset => {
                        // On Windows, ICMP Port Unreachable from a previous UDP send_to causes WSAECONNRESET on next recv.
                        continue;
                    }
                    Err(e) => return Err(e.into()),
                };
                for (dest, pkt) in router.handle_datagram(from, &buf[..len], Instant::now()) {
                    if let Err(err) = socket.send_to(&pkt, dest).await {
                        tracing::warn!(%dest, error = %err, "failed sending UDP reply");
                    }
                }
            }
            _ = tick_timer.tick() => {
                for (addr, pkt) in router.tick(Instant::now(), rto) {
                    if let Err(err) = socket.send_to(&pkt, addr).await {
                        tracing::warn!(%addr, error = %err, "failed sending UDP tick packet");
                    }
                }
            }
            _ = tokio::signal::ctrl_c() => {
                tracing::info!("shutting down wot-baseapp");
                break;
            }
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn session_file_reload_detects_new_login_session() {
        let dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../target/test-tmp-baseapp");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("reload_session.json");
        let _ = std::fs::remove_file(&path);

        assert!(try_load_session(&path).unwrap().is_none());

        let s1 = LoginSession::new("u1", b"key1_0123456789ab", 111, "127.0.0.1:4001");
        s1.write_atomic(&path).unwrap();
        let snap1 = try_load_session(&path).unwrap().unwrap();
        assert_eq!(snap1.session.session_key, 111);

        let s2 = LoginSession::new("u2", b"key2_0123456789ab", 222, "127.0.0.1:4002");
        s2.write_atomic(&path).unwrap();
        let snap2 = try_load_session(&path).unwrap().unwrap();
        assert_ne!(snap1.raw_json, snap2.raw_json);
        assert_eq!(snap2.session.session_key, 222);

        let _ = std::fs::remove_file(&path);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn two_peers_retain_independent_sessions_across_handoff() {
        let game_dir = PathBuf::from(r"C:\Users\qwerty\Desktop\World_of_Tanks");
        if !game_dir.join("res").exists() {
            return;
        }
        let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target/test-tmp-multipeer");
        std::fs::create_dir_all(&dir).unwrap();
        let session_file = dir.join("session.json");
        let _ = std::fs::remove_file(&session_file);

        let (tx, rx) = tokio::sync::oneshot::channel();
        let task = tokio::spawn(serve_with_ready(
            BaseAppConfig {
                bind: "127.0.0.1:0".parse().unwrap(),
                session_file: session_file.clone(),
                game_dir,
                data_dir: dir.join("accounts"),
                preset: Preset::Retail,
                economics_config: None,
                overrides: PayloadOverrides::default(),
            },
            Some(tx),
        ));
        let base_addr = rx.await.expect("baseapp ready");

        let wrap_msg = |enc: bw201::bundle::ElementEncoder, seq: u32, key: &bw201::crypto::BlowfishKey| {
            let mut pkt = enc.finish();
            pkt.config.sequence = Some(seq);
            pkt.config.extra_flags |=
                bw201::packet::FLAG_ON_CHANNEL | bw201::packet::FLAG_IS_RELIABLE;
            pkt.finalize_flags();
            key.packet_encrypt(&pkt.encode(bw201::packet::Endian::Little))
        };

        // Peer 1 connects with session 1
        let s1 = LoginSession::new("p1", b"peer1_key_012345", 0x1111, "127.0.0.1:4001");
        s1.write_atomic(&session_file).unwrap();
        let bf1 = bw201::crypto::BlowfishKey::new(&s1.encryption_key()).unwrap();
        let sock1 = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
        sock1.connect(base_addr).unwrap();
        sock1
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();

        let mut args1 = bw201::stream::BinaryWriter::new();
        args1.u32(0x1111);
        args1.i32(0);
        let mut enc1 = bw201::bundle::ElementEncoder::new();
        enc1.request(
            bw201::interfaces::baseapp_ext_ids::BASE_APP_LOGIN,
            bw201::interfaces::LengthStyle::Variable(2),
            1,
            args1.as_slice(),
        );
        sock1.send(&wrap_msg(enc1, 0, &bf1)).unwrap();
        let mut buf = [0u8; 2048];
        let n1 = sock1.recv(&mut buf).unwrap();
        assert!(bf1.packet_decrypt(&buf[..n1]).is_ok());

        // Peer 2 writes a newer session handoff and connects concurrently
        let s2 = LoginSession::new("p2", b"peer2_key_012345", 0x2222, "127.0.0.1:4002");
        s2.write_atomic(&session_file).unwrap();
        let bf2 = bw201::crypto::BlowfishKey::new(&s2.encryption_key()).unwrap();
        let sock2 = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
        sock2.connect(base_addr).unwrap();
        sock2
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();

        let mut args2 = bw201::stream::BinaryWriter::new();
        args2.u32(0x2222);
        args2.i32(0);
        let mut enc2 = bw201::bundle::ElementEncoder::new();
        enc2.request(
            bw201::interfaces::baseapp_ext_ids::BASE_APP_LOGIN,
            bw201::interfaces::LengthStyle::Variable(2),
            2,
            args2.as_slice(),
        );
        sock2.send(&wrap_msg(enc2, 0, &bf2)).unwrap();
        let n2 = sock2.recv(&mut buf).unwrap();
        assert!(bf2.packet_decrypt(&buf[..n2]).is_ok());

        // Peer 1 continues talking using its existing session key
        let mut w1b = bw201::stream::BinaryWriter::new();
        w1b.i16(124);
        w1b.i16(crate::protocol::cmd::CMD_SET_LANGUAGE);
        w1b.string("en");
        let mut enc1b = bw201::bundle::ElementEncoder::new();
        enc1b.message(
            crate::protocol::base_method::DO_CMD_STR,
            bw201::interfaces::LengthStyle::Variable(2),
            w1b.as_slice(),
        );
        sock1.send(&wrap_msg(enc1b, 1, &bf1)).unwrap();
        let n1b = sock1.recv(&mut buf).expect("peer 1 session should remain active");
        let plain1b = bf1.packet_decrypt(&buf[..n1b]).unwrap();
        let pkt1b = bw201::packet::Packet::decode(&plain1b, bw201::packet::Endian::Little).unwrap();
        assert_eq!(pkt1b.config.sequence, Some(1));

        task.abort();
        let _ = std::fs::remove_file(&session_file);
    }

    #[test]
    fn enable_entities_consumes_login_key_and_tracks_active_owners() {
        let game_dir = PathBuf::from(r"C:\Users\qwerty\Desktop\World_of_Tanks");
        if !game_dir.join("res").exists() {
            return;
        }
        let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../target/test-tmp")
            .join(format!("router-owners-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let session_file = dir.join("session.json");

        let payloads = AccountPayloads::load(&game_dir, &PayloadOverrides::default()).unwrap();
        let registry = AccountRegistry::open(
            &dir.join("accounts"),
            payloads.catalog.clone(),
            payloads.economics.clone(),
            Preset::Retail,
        )
        .unwrap();
        let mut router = SessionRouter::new(session_file.clone(), payloads, registry);

        let wrap_msg = |enc: bw201::bundle::ElementEncoder, seq: u32, key: &bw201::crypto::BlowfishKey| {
            let mut pkt = enc.finish();
            pkt.config.sequence = Some(seq);
            pkt.config.extra_flags |=
                bw201::packet::FLAG_ON_CHANNEL | bw201::packet::FLAG_IS_RELIABLE;
            pkt.finalize_flags();
            key.packet_encrypt(&pkt.encode(bw201::packet::Endian::Little))
        };

        let s1 = LoginSession::new("owner_one", b"owner1_key_01234", 0xA111, "127.0.0.1:4101");
        s1.write_atomic(&session_file).unwrap();
        let bf1 = bw201::crypto::BlowfishKey::new(&s1.encryption_key()).unwrap();
        let addr1: SocketAddr = "127.0.0.1:50001".parse().unwrap();

        // BASE_APP_LOGIN -> known_logins still holds key until ENABLE_ENTITIES
        let mut a1 = bw201::stream::BinaryWriter::new();
        a1.u32(0xA111);
        a1.i32(0);
        let mut enc1 = bw201::bundle::ElementEncoder::new();
        enc1.request(
            bw201::interfaces::baseapp_ext_ids::BASE_APP_LOGIN,
            bw201::interfaces::LengthStyle::Variable(2),
            1,
            a1.as_slice(),
        );
        assert!(!router.handle_datagram(addr1, &wrap_msg(enc1, 0, &bf1), Instant::now()).is_empty());

        // ENABLE_ENTITIES -> consumes key from known_logins and registers active owner
        let mut enc1_ee = bw201::bundle::ElementEncoder::new();
        enc1_ee.fixed_message(bw201::interfaces::baseapp_ext_ids::ENABLE_ENTITIES, 1, &[0]);
        assert!(!router.handle_datagram(addr1, &wrap_msg(enc1_ee, 1, &bf1), Instant::now()).is_empty());
        assert!(!router.known_logins.contains_key(&0xA111), "new_key must be consumed on ENABLE_ENTITIES");
        assert_eq!(router.active_owners.len(), 1);

        // Second distinct owner logs in
        let s2 = LoginSession::new("owner_two", b"owner2_key_01234", 0xB222, "127.0.0.1:4102");
        s2.write_atomic(&session_file).unwrap();
        let bf2 = bw201::crypto::BlowfishKey::new(&s2.encryption_key()).unwrap();
        let addr2: SocketAddr = "127.0.0.1:50002".parse().unwrap();
        let mut a2 = bw201::stream::BinaryWriter::new();
        a2.u32(0xB222);
        a2.i32(0);
        let mut enc2 = bw201::bundle::ElementEncoder::new();
        enc2.request(
            bw201::interfaces::baseapp_ext_ids::BASE_APP_LOGIN,
            bw201::interfaces::LengthStyle::Variable(2),
            1,
            a2.as_slice(),
        );
        assert!(!router.handle_datagram(addr2, &wrap_msg(enc2, 0, &bf2), Instant::now()).is_empty());
        let mut enc2_ee = bw201::bundle::ElementEncoder::new();
        enc2_ee.fixed_message(bw201::interfaces::baseapp_ext_ids::ENABLE_ENTITIES, 1, &[0]);
        assert!(!router.handle_datagram(addr2, &wrap_msg(enc2_ee, 1, &bf2), Instant::now()).is_empty());
        assert_eq!(router.active_owners.len(), 2);

        // Owner 1 queries server stats -> receives clusterOnlinePlayersCount = 2
        let mut stats = bw201::stream::BinaryWriter::new();
        stats.i16(120);
        stats.i16(crate::protocol::cmd::CMD_REQ_SERVER_STATS);
        stats.i32(0);
        stats.i32(0);
        stats.i32(0);
        let mut enc_stats = bw201::bundle::ElementEncoder::new();
        enc_stats.message(
            crate::protocol::base_method::DO_CMD_INT3,
            bw201::interfaces::LengthStyle::Variable(2),
            stats.as_slice(),
        );
        let dgrams = router.handle_datagram(addr1, &wrap_msg(enc_stats, 2, &bf1), Instant::now());
        assert!(!dgrams.is_empty());
        let plain = bf1.packet_decrypt(&dgrams[0].1).unwrap();
        let pkt = bw201::packet::Packet::decode(&plain, bw201::packet::Endian::Little).unwrap();
        let table = bw201::interfaces::client_interface();
        let msgs = bw201::bundle::decode_messages(&pkt, &table, bw201::packet::Endian::Little).unwrap();
        let stats_msg = msgs
            .iter()
            .find(|m| m.id == crate::protocol::client_method::RECEIVE_SERVER_STATS)
            .expect("RECEIVE_SERVER_STATS");
        let mut sr = bw201::stream::BinaryReader::new(&stats_msg.data);
        let _pid = sr.i32().unwrap();
        assert_eq!(sr.u32().unwrap(), 2, "must report both active owners online");

        // Owner 2 sends DISCONNECT_CLIENT -> active_owners shrinks back to 1
        let mut enc_disc = bw201::bundle::ElementEncoder::new();
        enc_disc.fixed_message(bw201::interfaces::baseapp_ext_ids::DISCONNECT_CLIENT, 1, &[0]);
        router.handle_datagram(addr2, &wrap_msg(enc_disc, 2, &bf2), Instant::now());
        assert_eq!(router.active_owners.len(), 1);

        let _ = std::fs::remove_dir_all(&dir);
    }

    fn write_synthetic_router_assets(root: &Path) {
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
    }

    #[test]
    fn same_port_reconnect_coalescing_login_and_enable_entities_preserves_battle_host_session() {
        let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../target/test-tmp")
            .join(format!("router-sameport-coal-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let game_dir = dir.join("game");
        write_synthetic_router_assets(&game_dir);
        let session_file = dir.join("session.json");

        let payloads = AccountPayloads::load(&game_dir, &PayloadOverrides::default()).unwrap();
        let registry = AccountRegistry::open(
            &dir.join("accounts"),
            payloads.catalog.clone(),
            payloads.economics.clone(),
            Preset::Retail,
        )
        .unwrap();
        let mut router = SessionRouter::new(session_file.clone(), payloads, registry);

        let wrap_msg = |enc: bw201::bundle::ElementEncoder, seq: u32, key: &bw201::crypto::BlowfishKey| {
            let mut pkt = enc.finish();
            pkt.config.sequence = Some(seq);
            pkt.config.extra_flags |=
                bw201::packet::FLAG_ON_CHANNEL | bw201::packet::FLAG_IS_RELIABLE;
            pkt.finalize_flags();
            key.packet_encrypt(&pkt.encode(bw201::packet::Endian::Little))
        };

        let addr: SocketAddr = "127.0.0.1:51001".parse().unwrap();

        // 1. Old account logs in on `addr` and enables entities
        let s1 = LoginSession::new("reconn_old", b"old_key_01234567", 0xC111, "127.0.0.1:4201");
        s1.write_atomic(&session_file).unwrap();
        let bf1 = bw201::crypto::BlowfishKey::new(&s1.encryption_key()).unwrap();
        let mut args1 = bw201::stream::BinaryWriter::new();
        args1.u32(0xC111);
        args1.i32(0);
        let mut enc1 = bw201::bundle::ElementEncoder::new();
        enc1.request(
            bw201::interfaces::baseapp_ext_ids::BASE_APP_LOGIN,
            bw201::interfaces::LengthStyle::Variable(2),
            1,
            args1.as_slice(),
        );
        enc1.fixed_message(bw201::interfaces::baseapp_ext_ids::ENABLE_ENTITIES, 1, &[0]);
        assert!(!router
            .handle_datagram(addr, &wrap_msg(enc1, 0, &bf1), Instant::now())
            .is_empty());
        let old_db_id = router
            .battle_host
            .account_for_addr(addr)
            .expect("initial coalesced login must register battle_host session");
        // Enqueue old account so we can also prove replacement cleans up queue & vehicle lock.
        let v_old = *router
            .registry
            .get_profile("reconn_old")
            .unwrap()
            .vehicles
            .keys()
            .next()
            .unwrap();
        let mut enq = bw201::stream::BinaryWriter::new();
        enq.i16(10);
        enq.i16(crate::protocol::cmd::CMD_ENQUEUE_FOR_ARENA);
        enq.i32(v_old as i32);
        enq.i32(0);
        enq.i32(1);
        let mut enc_enq = bw201::bundle::ElementEncoder::new();
        enc_enq.message(
            crate::protocol::base_method::DO_CMD_INT3,
            bw201::interfaces::LengthStyle::Variable(2),
            enq.as_slice(),
        );
        router.handle_datagram(addr, &wrap_msg(enc_enq, 1, &bf1), Instant::now());
        assert_eq!(
            router
                .registry
                .get_profile("reconn_old")
                .unwrap()
                .vehicles[&v_old]
                .lock,
            crate::registry::LOCK_REASON_IN_QUEUE
        );

        // 2. Different account reconnects on the SAME UDP port with BASE_APP_LOGIN + ENABLE_ENTITIES in one datagram
        let s2 = LoginSession::new("reconn_new", b"new_key_01234567", 0xC222, "127.0.0.1:4202");
        s2.write_atomic(&session_file).unwrap();
        let bf2 = bw201::crypto::BlowfishKey::new(&s2.encryption_key()).unwrap();
        let mut args2 = bw201::stream::BinaryWriter::new();
        args2.u32(0xC222);
        args2.i32(0);
        let mut enc2 = bw201::bundle::ElementEncoder::new();
        enc2.request(
            bw201::interfaces::baseapp_ext_ids::BASE_APP_LOGIN,
            bw201::interfaces::LengthStyle::Variable(2),
            1,
            args2.as_slice(),
        );
        enc2.fixed_message(bw201::interfaces::baseapp_ext_ids::ENABLE_ENTITIES, 1, &[0]);
        let replies2 = router.handle_datagram(addr, &wrap_msg(enc2, 0, &bf2), Instant::now());
        assert!(!replies2.is_empty());

        let new_db_id = router
            .battle_host
            .account_for_addr(addr)
            .expect("coalesced same-port reconnect must preserve new BattleHost session");
        assert_ne!(new_db_id, old_db_id);
        assert!(
            router
                .registry
                .get_profile("reconn_old")
                .unwrap()
                .vehicles
                .values()
                .all(|v| v.lock == crate::registry::LOCK_REASON_NONE),
            "replaced account vehicles must be unlocked"
        );
        assert!(
            !router.active_owners.contains_key("reconn_old"),
            "replaced account must be removed from active_owners"
        );
        assert_eq!(router.active_owners.get("reconn_new"), Some(&(addr, 0xC222)));
        assert!(router.revoked_keys_set.contains(&0xC111));

        // Subsequent DO_CMD on new session must succeed and report 1 online player
        let mut stats = bw201::stream::BinaryWriter::new();
        stats.i16(120);
        stats.i16(crate::protocol::cmd::CMD_REQ_SERVER_STATS);
        stats.i32(0);
        stats.i32(0);
        stats.i32(0);
        let mut enc_stats = bw201::bundle::ElementEncoder::new();
        enc_stats.message(
            crate::protocol::base_method::DO_CMD_INT3,
            bw201::interfaces::LengthStyle::Variable(2),
            stats.as_slice(),
        );
        let dgrams = router.handle_datagram(addr, &wrap_msg(enc_stats, 1, &bf2), Instant::now());
        assert!(!dgrams.is_empty());
        let plain = bf2.packet_decrypt(&dgrams[0].1).unwrap();
        let pkt = bw201::packet::Packet::decode(&plain, bw201::packet::Endian::Little).unwrap();
        let table = bw201::interfaces::client_interface();
        let msgs = bw201::bundle::decode_messages(&pkt, &table, bw201::packet::Endian::Little).unwrap();
        let stats_msg = msgs
            .iter()
            .find(|m| m.id == crate::protocol::client_method::RECEIVE_SERVER_STATS)
            .expect("RECEIVE_SERVER_STATS must be handled on new BattleHost session");
        let mut sr = bw201::stream::BinaryReader::new(&stats_msg.data);
        let _pid = sr.i32().unwrap();
        assert_eq!(sr.u32().unwrap(), 1);

        // 3. Same account reconnects again on the SAME UDP port with coalesced BASE_APP_LOGIN + ENABLE_ENTITIES
        let s3 = LoginSession::new("reconn_new", b"new3_key_0123456", 0xC333, "127.0.0.1:4203");
        s3.write_atomic(&session_file).unwrap();
        let bf3 = bw201::crypto::BlowfishKey::new(&s3.encryption_key()).unwrap();
        let mut args3 = bw201::stream::BinaryWriter::new();
        args3.u32(0xC333);
        args3.i32(0);
        let mut enc3 = bw201::bundle::ElementEncoder::new();
        enc3.request(
            bw201::interfaces::baseapp_ext_ids::BASE_APP_LOGIN,
            bw201::interfaces::LengthStyle::Variable(2),
            1,
            args3.as_slice(),
        );
        enc3.fixed_message(bw201::interfaces::baseapp_ext_ids::ENABLE_ENTITIES, 1, &[0]);
        assert!(!router
            .handle_datagram(addr, &wrap_msg(enc3, 0, &bf3), Instant::now())
            .is_empty());
        assert_eq!(
            router.battle_host.account_for_addr(addr),
            Some(new_db_id),
            "same-account coalesced same-port reconnect must keep active BattleHost session"
        );
        assert_eq!(router.active_owners.get("reconn_new"), Some(&(addr, 0xC333)));
        assert!(router.revoked_keys_set.contains(&0xC222));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn same_port_reconnect_before_enable_entities_and_plaintext_login_fallback() {
        let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../target/test-tmp")
            .join(format!("router-sameport-sep-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let game_dir = dir.join("game");
        write_synthetic_router_assets(&game_dir);
        let session_file = dir.join("session.json");

        let payloads = AccountPayloads::load(&game_dir, &PayloadOverrides::default()).unwrap();
        let registry = AccountRegistry::open(
            &dir.join("accounts"),
            payloads.catalog.clone(),
            payloads.economics.clone(),
            Preset::Retail,
        )
        .unwrap();
        let mut router = SessionRouter::new(session_file.clone(), payloads, registry);

        let wrap_msg = |enc: bw201::bundle::ElementEncoder, seq: u32, key: &bw201::crypto::BlowfishKey| {
            let mut pkt = enc.finish();
            pkt.config.sequence = Some(seq);
            pkt.config.extra_flags |=
                bw201::packet::FLAG_ON_CHANNEL | bw201::packet::FLAG_IS_RELIABLE;
            pkt.finalize_flags();
            key.packet_encrypt(&pkt.encode(bw201::packet::Endian::Little))
        };

        let addr: SocketAddr = "127.0.0.1:51002".parse().unwrap();

        // 1. Peer sends BASE_APP_LOGIN only (entities not yet enabled)
        let s1 = LoginSession::new("pre_ee_old", b"pre1_key_0123456", 0xD111, "127.0.0.1:4301");
        s1.write_atomic(&session_file).unwrap();
        let bf1 = bw201::crypto::BlowfishKey::new(&s1.encryption_key()).unwrap();
        let mut a1 = bw201::stream::BinaryWriter::new();
        a1.u32(0xD111);
        a1.i32(0);
        let mut enc1 = bw201::bundle::ElementEncoder::new();
        enc1.request(
            bw201::interfaces::baseapp_ext_ids::BASE_APP_LOGIN,
            bw201::interfaces::LengthStyle::Variable(2),
            1,
            a1.as_slice(),
        );
        assert!(!router
            .handle_datagram(addr, &wrap_msg(enc1, 0, &bf1), Instant::now())
            .is_empty());

        // 2. Client restarts before ENABLE_ENTITIES and sends plaintext off-channel BASE_APP_LOGIN with new key
        let s2 = LoginSession::new("pre_ee_new", b"pre2_key_0123456", 0xD222, "127.0.0.1:4302");
        s2.write_atomic(&session_file).unwrap();
        let bf2 = bw201::crypto::BlowfishKey::new(&s2.encryption_key()).unwrap();
        let mut a2 = bw201::stream::BinaryWriter::new();
        a2.u32(0xD222);
        a2.i32(0);
        let mut enc2 = bw201::bundle::ElementEncoder::new();
        enc2.request(
            bw201::interfaces::baseapp_ext_ids::BASE_APP_LOGIN,
            bw201::interfaces::LengthStyle::Variable(2),
            1,
            a2.as_slice(),
        );
        let plain_pkt = enc2.finish().encode(bw201::packet::Endian::Little);
        let replies_login = router.handle_datagram(addr, &plain_pkt, Instant::now());
        assert!(
            !replies_login.is_empty(),
            "plaintext BASE_APP_LOGIN from restarted client on same port must fall through to new session"
        );

        // Followed by separate ENABLE_ENTITIES encrypted with bf2
        let mut enc2_ee = bw201::bundle::ElementEncoder::new();
        enc2_ee.fixed_message(bw201::interfaces::baseapp_ext_ids::ENABLE_ENTITIES, 1, &[0]);
        assert!(!router
            .handle_datagram(addr, &wrap_msg(enc2_ee, 0, &bf2), Instant::now())
            .is_empty());
        assert!(router.battle_host.account_for_addr(addr).is_some());
        assert_eq!(router.active_owners.get("pre_ee_new"), Some(&(addr, 0xD222)));

        let _ = std::fs::remove_dir_all(&dir);
    }
}
