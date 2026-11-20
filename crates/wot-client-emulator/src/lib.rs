//! Headless WoT 0.6.5 client for exercising LoginApp and BaseApp over UDP.

mod account;
mod arena;
mod transport;

use std::collections::{BTreeMap, VecDeque};
use std::net::SocketAddrV4;
use std::time::{Duration, Instant};

use anyhow::{bail, ensure, Context, Result};
use bw201::bundle::ElementEncoder;
use bw201::interfaces::{baseapp_ext_ids, client_ids, LengthStyle};
use bw201::stream::{BinaryReader, BinaryWriter};
use bw201::world::{CreateBasePlayer, RequestEntityUpdate, ResetEntities};
use rsa::RsaPublicKey;
use serde::Deserialize;
use wot065_data::pickle::{self, Value};
use wot_baseapp::protocol::{
    base_method, client_method, cmd, ACCOUNT_ENTITY_TYPE_ID, AVATAR_ENTITY_TYPE_ID,
};

use account::{AccountCache, Downloads};
pub use arena::{Arena, BattleResult, Entity, Pose};
use transport::Transport;
pub use transport::WireStats;

const EVENT_HISTORY: usize = 2048;
const EVENT_PREFIX: usize = 256;

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Phase {
    Loading,
    Hangar,
    Arena,
}

/// A received message with its context at the time of delivery.
#[derive(Debug)]
pub struct Event {
    pub sequence: u64,
    pub id: u8,
    pub phase: Phase,
    pub player_method: bool,
    pub payload_prefix: Vec<u8>,
}

/// The five Account.doCmd argument layouts supported by the retail client.
#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CommandArgs {
    Str(String),
    Int3([i32; 3]),
    Int4([i32; 4]),
    Int2Str(i32, i32, String),
    IntArr(Vec<i32>),
}

impl Default for CommandArgs {
    fn default() -> Self {
        Self::Int3([0; 3])
    }
}

impl CommandArgs {
    fn encode(&self, rid: i16, command: i16) -> Result<(u8, Vec<u8>)> {
        let mut w = BinaryWriter::new();
        w.i16(rid);
        w.i16(command);
        let method = match self {
            Self::Str(value) => {
                ensure!(
                    value.len() <= 60_000,
                    "Account string argument is too large"
                );
                w.string(value);
                base_method::DO_CMD_STR
            }
            Self::Int3(values) => {
                values.iter().for_each(|&value| w.i32(value));
                base_method::DO_CMD_INT3
            }
            Self::Int4(values) => {
                values.iter().for_each(|&value| w.i32(value));
                base_method::DO_CMD_INT4
            }
            Self::Int2Str(first, second, value) => {
                ensure!(
                    value.len() <= 60_000,
                    "Account string argument is too large"
                );
                w.i32(*first);
                w.i32(*second);
                w.string(value);
                base_method::DO_CMD_INT2_STR
            }
            Self::IntArr(values) => {
                ensure!(values.len() <= 15_000, "Account integer array is too large");
                w.i32(values.len() as i32);
                values.iter().for_each(|&value| w.i32(value));
                base_method::DO_CMD_INT_ARR
            }
        };
        Ok((method, w.into_inner()))
    }
}

#[derive(Debug)]
pub struct CommandReply {
    pub code: i16,
    pub payload: Option<Value>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Request {
    Account,
    ConfirmAccount(i64),
    Shop,
    Dossiers,
    Command,
}

impl Request {
    fn label(self) -> &'static str {
        match self {
            Self::Account => "CMD_SYNC_DATA",
            Self::ConfirmAccount(_) => "CMD_SYNC_DATA (revision confirmation)",
            Self::Shop => "CMD_SYNC_SHOP",
            Self::Dossiers => "CMD_SYNC_DOSSIERS",
            Self::Command => "Account command",
        }
    }
}

struct Pending {
    request: Request,
    reply: Option<CommandReply>,
    resource: Option<Value>,
}

/// Drives account synchronization and entity lifecycle callbacks for one live session.
///
/// Call `wait_for` or `poll_for` while idle to keep processing Mercury packets.
pub struct Client {
    transport: Transport,
    player: Option<(i32, Phase)>,
    account: AccountCache,
    account_confirmed: bool,
    show_gui: Option<Value>,
    shop: Option<Value>,
    dossiers: Option<Value>,
    arena: Option<Arena>,
    battle_result: Option<BattleResult>,
    downloads: Downloads,
    pending: BTreeMap<i16, Pending>,
    completed: BTreeMap<i16, CommandReply>,
    next_request: i16,
    events: VecDeque<Event>,
    event_sequence: u64,
    closed: bool,
}

impl Client {
    pub fn connect(
        login: SocketAddrV4,
        key: &RsaPublicKey,
        username: &str,
        password: &str,
        timeout: Duration,
    ) -> Result<Self> {
        let mut transport = Transport::connect(login, key, username, password, timeout)?;
        transport
            .send(enable_entities)
            .context("enable BaseApp entities")?;
        Ok(Self {
            transport,
            player: None,
            account: AccountCache::default(),
            account_confirmed: false,
            show_gui: None,
            shop: None,
            dossiers: None,
            arena: None,
            battle_result: None,
            downloads: Downloads::default(),
            pending: BTreeMap::new(),
            completed: BTreeMap::new(),
            next_request: cmd::REQUEST_ID_UNRESERVED_MIN,
            events: VecDeque::new(),
            event_sequence: 0,
            closed: false,
        })
    }

    pub fn base_addr(&self) -> SocketAddrV4 {
        self.transport.base_addr()
    }

    pub fn wire_stats(&self) -> WireStats {
        self.transport.stats()
    }

    pub fn phase(&self) -> Phase {
        self.player.map_or(Phase::Loading, |(_, phase)| phase)
    }

    pub fn player_id(&self) -> Option<i32> {
        self.player.map(|(id, _)| id)
    }

    pub fn show_gui(&self) -> Option<&Value> {
        self.show_gui.as_ref()
    }

    pub fn account(&self) -> Option<&Value> {
        self.account.snapshot()
    }

    pub fn shop(&self) -> Option<&Value> {
        self.shop.as_ref()
    }

    pub fn dossiers(&self) -> Option<&Value> {
        self.dossiers.as_ref()
    }

    pub fn arena(&self) -> Option<&Arena> {
        self.arena.as_ref()
    }

    pub fn battle_result(&self) -> Option<BattleResult> {
        self.battle_result
    }

    pub fn hangar_ready(&self) -> bool {
        self.phase() == Phase::Hangar
            && self.show_gui.is_some()
            && self.account_confirmed
            && self.shop.is_some()
            && self.dossiers.is_some()
            && self
                .pending
                .values()
                .all(|entry| entry.request == Request::Command)
    }

    pub fn event_cursor(&self) -> u64 {
        self.event_sequence
    }

    pub fn events_since(&self, cursor: u64) -> impl Iterator<Item = &Event> {
        self.events
            .iter()
            .filter(move |event| event.sequence > cursor)
    }

    pub fn wait_for<T>(
        &mut self,
        timeout: Duration,
        description: &str,
        mut get: impl FnMut(&Self) -> Option<T>,
    ) -> Result<T> {
        self.check_open()?;
        let deadline = Instant::now()
            .checked_add(timeout)
            .context("wait duration is too large")?;
        loop {
            if let Some(value) = get(self) {
                return Ok(value);
            }
            if Instant::now() >= deadline {
                let pending: Vec<_> = self
                    .pending
                    .iter()
                    .map(|(rid, entry)| (rid, entry.request.label()))
                    .collect();
                bail!(
                    "timed out waiting for {description} (phase {:?}, pending {pending:?}, UDP {:?})",
                    self.phase(), self.wire_stats()
                );
            }
            self.pump(deadline)
                .with_context(|| format!("while waiting for {description}"))?;
        }
    }

    pub fn poll_for(&mut self, duration: Duration) -> Result<()> {
        self.check_open()?;
        let deadline = Instant::now()
            .checked_add(duration)
            .context("poll duration is too large")?;
        while Instant::now() < deadline {
            self.pump(deadline)?;
        }
        Ok(())
    }

    pub fn command(
        &mut self,
        id: i16,
        args: CommandArgs,
        timeout: Duration,
    ) -> Result<CommandReply> {
        self.check_account()?;
        let rid = self.request(&[(Request::Command, id, args)])?[0];
        let outcome = self.wait_for(timeout, &format!("Account command {id}"), |client| {
            client.completed.contains_key(&rid).then_some(())
        });
        self.pending.remove(&rid);
        outcome?;
        self.completed
            .remove(&rid)
            .context("command reply disappeared")
    }

    /// Send a command that reports via a callback, such as onEnqueued or receiveQueueInfo.
    pub fn notify(&mut self, id: i16, args: CommandArgs) -> Result<()> {
        self.check_account()?;
        let (method, data) = args.encode(cmd::REQUEST_ID_NO_RESPONSE, id)?;
        self.call_base(method, &data)
    }

    pub fn call_base(&mut self, method: u8, data: &[u8]) -> Result<()> {
        self.check_open()?;
        ensure!(
            (0xC0..=0xFE).contains(&method),
            "invalid BaseMethod {method:#04x}"
        );
        ensure!(data.len() <= 60_000, "BaseMethod payload is too large");
        self.transport.send(|encoder| {
            encoder.message(method, LengthStyle::Variable(2), data);
        })
    }

    pub fn call_cell(&mut self, entity: i32, method: u8, data: &[u8]) -> Result<()> {
        self.check_open()?;
        ensure!(
            self.phase() == Phase::Arena,
            "CellMethod requires an Avatar"
        );
        ensure!(method <= 0x3F, "invalid CellMethod index {method}");
        ensure!(data.len() <= 60_000, "CellMethod payload is too large");
        let mut w = BinaryWriter::new();
        w.i32(entity);
        w.raw(data);
        self.transport.send(|encoder| {
            encoder.message(0x80 | method, LengthStyle::Variable(2), w.as_slice());
        })
    }

    pub fn move_vehicle(&mut self, flags: u8) -> Result<()> {
        ensure!(self.phase() == Phase::Arena, "movement requires an Avatar");
        self.call_base(base_method::AVATAR_VEHICLE_MOVE_WITH, &[flags])
    }

    pub fn aim(&mut self, point: [f32; 3]) -> Result<()> {
        ensure!(self.phase() == Phase::Arena, "aiming requires an Avatar");
        ensure!(
            point.into_iter().all(f32::is_finite),
            "aim point must be finite"
        );
        let mut w = BinaryWriter::new();
        point.into_iter().for_each(|value| w.f32(value));
        self.call_base(
            base_method::AVATAR_VEHICLE_TRACK_POINT_WITH_GUN,
            w.as_slice(),
        )
    }

    pub fn shoot(&mut self) -> Result<()> {
        ensure!(self.phase() == Phase::Arena, "shooting requires an Avatar");
        self.call_base(base_method::AVATAR_VEHICLE_SHOOT, &[])
    }

    pub fn leave_arena(&mut self) -> Result<()> {
        ensure!(
            self.phase() == Phase::Arena,
            "leaving an arena requires an Avatar"
        );
        self.call_base(base_method::AVATAR_LEAVE_ARENA, &[])
    }

    pub fn disconnect(&mut self) -> Result<()> {
        if !self.closed {
            self.closed = true;
            self.transport.disconnect()?;
        }
        Ok(())
    }

    fn check_open(&self) -> Result<()> {
        ensure!(!self.closed, "BaseApp session is closed");
        Ok(())
    }

    fn check_account(&self) -> Result<()> {
        self.check_open()?;
        ensure!(
            self.phase() == Phase::Hangar,
            "Account command requires an Account"
        );
        Ok(())
    }

    fn allocate_request(&mut self) -> i16 {
        loop {
            let id = self.next_request;
            self.next_request = id.checked_add(1).unwrap_or(cmd::REQUEST_ID_UNRESERVED_MIN);
            if !self.pending.contains_key(&id) && !self.completed.contains_key(&id) {
                return id;
            }
        }
    }

    fn request(&mut self, requests: &[(Request, i16, CommandArgs)]) -> Result<Vec<i16>> {
        let messages: Vec<_> = requests
            .iter()
            .map(|(request, id, args)| {
                let rid = self.allocate_request();
                let (method, data) = args.encode(rid, *id)?;
                Ok((rid, *request, method, data))
            })
            .collect::<Result<_>>()?;
        self.transport.send(|encoder| {
            for (_, _, method, data) in &messages {
                encoder.message(*method, LengthStyle::Variable(2), data);
            }
        })?;
        let mut ids = Vec::with_capacity(messages.len());
        for (rid, request, _, _) in messages {
            ids.push(rid);
            self.pending.insert(
                rid,
                Pending {
                    request,
                    reply: None,
                    resource: None,
                },
            );
        }
        Ok(ids)
    }

    fn has_request(&self, request: Request) -> bool {
        self.pending.values().any(|entry| entry.request == request)
    }

    fn pump(&mut self, deadline: Instant) -> Result<()> {
        for msg in self.transport.receive(deadline)? {
            self.message(msg.id, &msg.data).with_context(|| {
                format!(
                    "handling BaseApp message {:#04x} in {:?}",
                    msg.id,
                    self.phase()
                )
            })?;
        }
        if let Some(arena) = &mut self.arena {
            let requests = arena.pending_entity_requests();
            if !requests.is_empty() {
                self.transport.send(|encoder| {
                    for id in requests {
                        let mut body = Vec::with_capacity(4);
                        RequestEntityUpdate {
                            id,
                            cache_stamps: Vec::new(),
                        }
                        .write_to(&mut body);
                        encoder.message(
                            baseapp_ext_ids::REQUEST_ENTITY_UPDATE,
                            LengthStyle::Variable(2),
                            &body,
                        );
                    }
                })?;
            }
            if arena.can_signal_ready() {
                self.transport.send(|encoder| {
                    encoder.message(
                        base_method::AVATAR_ON_CLIENT_READY,
                        LengthStyle::Variable(2),
                        &[],
                    );
                })?;
                arena.ready_sent = true;
            }
        }
        Ok(())
    }

    fn message(&mut self, id: u8, data: &[u8]) -> Result<()> {
        let mut r = BinaryReader::new(data);
        let phase = self.phase();
        let player_method = (0x80..=0xFE).contains(&id)
            && r.i32().ok() == self.player_id()
            && self.player_id().is_some();
        self.event_sequence += 1;
        if self.events.len() == EVENT_HISTORY {
            self.events.pop_front();
        }
        self.events.push_back(Event {
            sequence: self.event_sequence,
            id,
            phase,
            player_method,
            payload_prefix: data[..data.len().min(EVENT_PREFIX)].to_vec(),
        });

        match id {
            client_ids::LOGGED_OFF => {
                self.closed = true;
                bail!("BaseApp logged off the client (reason {})", r.u8()?);
            }
            client_ids::RESET_ENTITIES => {
                let reset = ResetEntities::read_from(&mut r)?;
                if reset.keep_player_on_base == 0 {
                    self.player = None;
                    self.arena = None;
                    self.show_gui = None;
                    self.account_confirmed = false;
                    self.pending.clear();
                    self.downloads = Downloads::default();
                } else if let Some(arena) = &mut self.arena {
                    *arena = Arena::for_map(arena.map_id);
                }
                self.transport.send(enable_entities)?;
            }
            client_ids::CREATE_BASE_PLAYER => self.create_player(&mut r)?,
            client_ids::RESOURCE_HEADER | client_ids::RESOURCE_FRAGMENT => {
                let rid = r.u16()? as i16;
                if self.pending.contains_key(&rid) {
                    let finished = if id == client_ids::RESOURCE_HEADER {
                        self.downloads.header(data)?
                    } else {
                        self.downloads.fragment(data)?
                    };
                    if let Some((rid, resource)) = finished {
                        if let Some(entry) = self.pending.get_mut(&rid) {
                            entry.resource = Some(resource);
                            self.finish_request(rid)?;
                        }
                    }
                }
            }
            _ if player_method && phase == Phase::Hangar => self.account_method(id, &mut r)?,
            _ if player_method && phase == Phase::Arena => {
                if id == client_method::AVATAR_ON_KICKED_FROM_SERVER {
                    bail!("server kicked Avatar: {}", r.string()?);
                }
                if id == client_method::AVATAR_ON_VEHICLE_LEFT_ARENA {
                    if let Some(result) = BattleResult::read(&mut r)? {
                        self.battle_result = Some(result);
                    }
                } else if let Some(arena) = &mut self.arena {
                    arena.player_method(id, &mut r)?;
                }
            }
            _ => {
                if let Some(arena) = &mut self.arena {
                    arena.message(id, data)?;
                }
            }
        }
        Ok(())
    }

    fn create_player(&mut self, r: &mut BinaryReader<'_>) -> Result<()> {
        let player = CreateBasePlayer::read_from(r)?;
        let mut props = BinaryReader::new(&player.value);
        let name = props.string()?;
        ensure!(!name.is_empty(), "createBasePlayer has no name");
        match player.entity_type {
            ACCOUNT_ENTITY_TYPE_ID => {
                let _normalized_name = props.string()?;
                let settings = pickle::loads(&props.string_bytes()?)
                    .context("decode Account.serverSettings")?;
                ensure!(
                    settings
                        .get("vivoxDomain")
                        .and_then(Value::as_bytes)
                        .is_some(),
                    "Account.serverSettings has no vivoxDomain string"
                );
                self.player = Some((player.entity_id, Phase::Hangar));
                self.arena = None;
                self.show_gui = None;
                self.shop = None;
                self.dossiers = None;
                self.account_confirmed = false;
                self.request(&[
                    (Request::Account, cmd::CMD_SYNC_DATA, CommandArgs::default()),
                    (Request::Shop, cmd::CMD_SYNC_SHOP, CommandArgs::default()),
                    (
                        Request::Dossiers,
                        cmd::CMD_SYNC_DOSSIERS,
                        CommandArgs::default(),
                    ),
                ])?;
            }
            AVATAR_ENTITY_TYPE_ID => {
                let map = props.i32()?;
                let _gui_type = props.u8()?;
                pickle::loads(&props.string_bytes()?).context("decode Avatar.arenaExtraData")?;
                let _weather = props.u8()?;
                self.player = Some((player.entity_id, Phase::Arena));
                self.arena = Some(Arena::for_map(map));
                self.battle_result = None;
            }
            kind => bail!("unexpected player entity type {kind}"),
        }
        Ok(())
    }

    fn account_method(&mut self, id: u8, r: &mut BinaryReader<'_>) -> Result<()> {
        match id {
            client_method::SHOW_GUI => {
                let ctx = pickle::loads(&r.string_bytes()?).context("decode Account.showGUI")?;
                ensure!(
                    ctx.get("databaseID").and_then(Value::as_int).is_some(),
                    "showGUI has no databaseID"
                );
                ensure!(
                    ctx.get("serverUTC").and_then(Value::as_int).is_some(),
                    "showGUI has no serverUTC"
                );
                self.show_gui = Some(ctx);
            }
            client_method::ON_CMD_RESPONSE | client_method::ON_CMD_RESPONSE_EXT => {
                let rid = r.i16()?;
                let code = r.i16()?;
                if let Some(entry) = self.pending.get_mut(&rid) {
                    let payload = if id == client_method::ON_CMD_RESPONSE_EXT {
                        Some(pickle::loads(&r.string_bytes()?).context("decode onCmdResponseExt")?)
                    } else {
                        None
                    };
                    entry.reply = Some(CommandReply { code, payload });
                    self.finish_request(rid)?;
                }
            }
            client_method::UPDATE => {
                let diff = pickle::loads(&r.string_bytes()?).context("decode Account.update")?;
                if !self.account.apply(diff)? && !self.has_request(Request::Account) {
                    self.account_confirmed = false;
                    self.request(&[(
                        Request::Account,
                        cmd::CMD_SYNC_DATA,
                        CommandArgs::default(),
                    )])?;
                }
            }
            client_method::RESYNC_DOSSIERS => {
                if !self.has_request(Request::Dossiers) {
                    self.dossiers = None;
                    self.request(&[(
                        Request::Dossiers,
                        cmd::CMD_SYNC_DOSSIERS,
                        CommandArgs::default(),
                    )])?;
                }
            }
            client_method::ON_KICKED_FROM_SERVER => bail!("server kicked Account: {}", r.string()?),
            _ => {}
        }
        Ok(())
    }

    fn finish_request(&mut self, rid: i16) -> Result<()> {
        let Some(entry) = self.pending.get(&rid) else {
            return Ok(());
        };
        let Some(reply) = &entry.reply else {
            return Ok(());
        };
        if reply.code == cmd::RES_STREAM && entry.resource.is_none() {
            return Ok(());
        }
        let entry = self.pending.remove(&rid).expect("pending request exists");
        let mut reply = entry.reply.expect("command reply exists");
        if reply.code == cmd::RES_STREAM {
            reply.payload = entry.resource;
        }

        let label = entry.request.label();
        match entry.request {
            Request::Command => {
                self.completed.insert(rid, reply);
            }
            Request::Account => {
                ensure!(
                    matches!(reply.code, cmd::RES_STREAM | cmd::RES_SUCCESS),
                    "{label} returned {}",
                    reply.code
                );
                self.account.install(
                    reply
                        .payload
                        .context("CMD_SYNC_DATA has no full account data")?,
                )?;
                if reply.code == cmd::RES_STREAM {
                    self.confirm_account_revision()?;
                } else {
                    self.account_confirmed = true;
                }
            }
            Request::ConfirmAccount(requested_rev) => match reply.code {
                cmd::RES_STREAM => {
                    let full = reply.payload.context("sync stream has no account data")?;
                    let rev = full
                        .get("rev")
                        .and_then(Value::as_int)
                        .context("sync stream has no revision")?;
                    ensure!(rev > requested_rev, "{label} streamed revision {rev} after request for revision {requested_rev}");
                    if self.account.revision().is_none_or(|current| rev >= current) {
                        self.account.install(full)?;
                    }
                    self.confirm_account_revision()?;
                }
                cmd::RES_CACHE | cmd::RES_SUCCESS => {
                    if let Some(diff) = reply.payload {
                        ensure!(
                            self.account.apply(diff)?,
                            "{label} did not match the current revision"
                        );
                    }
                    self.account_confirmed = true;
                }
                _ => bail!("{label} returned {}", reply.code),
            },
            Request::Shop => {
                let shop = reply.payload.context("CMD_SYNC_SHOP has no shop cache")?;
                ensure!(
                    reply.code == cmd::RES_STREAM
                        && shop.get("rev").and_then(Value::as_int).is_some()
                        && shop.get("items").and_then(Value::as_dict).is_some(),
                    "invalid CMD_SYNC_SHOP payload (result {})",
                    reply.code
                );
                self.shop = Some(shop);
            }
            Request::Dossiers => {
                let dossiers = reply
                    .payload
                    .context("CMD_SYNC_DOSSIERS has no cache records")?;
                ensure!(
                    reply.code == cmd::RES_STREAM && dossier_count(&dossiers).is_some(),
                    "invalid CMD_SYNC_DOSSIERS payload (result {})",
                    reply.code
                );
                self.dossiers = Some(dossiers);
            }
        }
        Ok(())
    }

    fn confirm_account_revision(&mut self) -> Result<()> {
        let rev = self
            .account
            .revision()
            .context("account data has no revision")?;
        self.account_confirmed = false;
        self.request(&[(
            Request::ConfirmAccount(rev),
            cmd::CMD_SYNC_DATA,
            CommandArgs::Int3([(rev >> 32) as i32, rev as i32, 0]),
        )])?;
        Ok(())
    }
}

impl Drop for Client {
    fn drop(&mut self) {
        let _ = self.disconnect();
    }
}

fn enable_entities(encoder: &mut ElementEncoder) {
    encoder.fixed_message(baseapp_ext_ids::ENABLE_ENTITIES, 1, &[0]);
}

/// Count the flat records from a retail DossierCache sync reply.
pub fn dossier_count(payload: &Value) -> Option<usize> {
    let [version, records] = payload.as_list()? else {
        return None;
    };
    version.as_int()?;
    let records = records.as_list()?;
    let mut chunks = records.chunks_exact(4);
    for row in &mut chunks {
        row[0].as_int()?;
        row[1].as_int()?;
        row[2].as_int()?;
        row[3].as_bytes()?;
    }
    chunks.remainder().is_empty().then_some(records.len() / 4)
}
