//! Prebattle rooms (Training rooms, Squad/Company foundations) mirroring `ClientPrebattle.py`.

use std::collections::HashMap;

use wot065_data::pickle::{self, Value};

use crate::assets::arena_defs::ArenaDefsCatalog;
use crate::lobby::queue::{
    JOIN_FAILURE_ACCOUNT_LOCK, JOIN_FAILURE_NOT_FOUND, JOIN_FAILURE_TEAM_IS_FULL,
    JOIN_FAILURE_WRONG_ARGS,
};
use crate::lobby::{
    ArenaLaunchSpec, BattleMode, ParticipantSpec, ARENA_BONUS_TYPE_TRAINING,
    ARENA_GUI_TYPE_TRAINING,
};

const RES_FAILURE: i16 = -1;
const RES_WRONG_ARGS: i16 = -2;
pub const PREBATTLE_KICK_REASON_CREATOR_LEFT: u8 = 9;
pub const MAX_PREBATTLE_COMMENT_BYTES: usize = 256;

fn clamp_comment_utf8(comment: &str) -> String {
    if comment.len() <= MAX_PREBATTLE_COMMENT_BYTES {
        return comment.to_string();
    }
    let mut end = MAX_PREBATTLE_COMMENT_BYTES;
    while end > 0 && !comment.is_char_boundary(end) {
        end -= 1;
    }
    comment[..end].to_string()
}

pub const PREBATTLE_TYPE_SQUAD: i32 = 1;
pub const PREBATTLE_TYPE_TRAINING: i32 = 2;
pub const PREBATTLE_TYPE_COMPANY: i32 = 3;

pub const PREBATTLE_START_TYPE_DIRECT: i32 = 1;
pub const PREBATTLE_START_TYPE_SQUAD: i32 = 2;
pub const PREBATTLE_START_TYPE_COMPANY: i32 = 3;

pub const PREBATTLE_TEAM_STATE_NOT_READY: u8 = 1;
pub const PREBATTLE_TEAM_STATE_READY: u8 = 2;
pub const PREBATTLE_TEAM_STATE_LOCKED: u8 = 3;

pub const PREBATTLE_ACCOUNT_STATE_UNKNOWN: u8 = 0;
pub const PREBATTLE_ACCOUNT_STATE_NOT_READY: u8 = 1;
pub const PREBATTLE_ACCOUNT_STATE_READY: u8 = 4;
pub const PREBATTLE_ACCOUNT_STATE_IN_BATTLE: u8 = 8;
pub const PREBATTLE_ACCOUNT_STATE_OFFLINE: u8 = 16;

pub const PREBATTLE_ROSTER_ASSIGNED: u8 = 0;
pub const PREBATTLE_ROSTER_UNASSIGNED: u8 = 16;
pub const PREBATTLE_ROSTER_ASSIGNED_IN_TEAM1: u8 = 1;
pub const PREBATTLE_ROSTER_ASSIGNED_IN_TEAM2: u8 = 2;
pub const PREBATTLE_ROSTER_UNASSIGNED_IN_TEAM1: u8 = 17;
pub const PREBATTLE_ROSTER_UNASSIGNED_IN_TEAM2: u8 = 18;

pub const PREBATTLE_UPDATE_ROSTER: u8 = 1;
pub const PREBATTLE_UPDATE_PLAYER_ADDED: u8 = 2;
pub const PREBATTLE_UPDATE_PLAYER_REMOVED: u8 = 3;
pub const PREBATTLE_UPDATE_PLAYER_STATE: u8 = 4;
pub const PREBATTLE_UPDATE_PLAYER_ROSTER: u8 = 5;
pub const PREBATTLE_UPDATE_TEAM_STATES: u8 = 6;
pub const PREBATTLE_UPDATE_SETTINGS: u8 = 7;
pub const PREBATTLE_UPDATE_SETTING: u8 = 8;
pub const PREBATTLE_UPDATE_KICKED_FROM_QUEUE: u8 = 9;
pub const PREBATTLE_UPDATE_PROPERTIES: u8 = 10;
pub const PREBATTLE_UPDATE_PROPERTY: u8 = 11;

pub const PREBATTLE_ROLE_NONE: i64 = 0;
pub const PREBATTLE_ROLE_TRAINING_CREATOR: i64 = 63487;

pub const PREBATTLE_CACHE_KEY_TYPE: i64 = 1;
pub const PREBATTLE_CACHE_KEY_IS_OPENED: i64 = 2;
pub const PREBATTLE_CACHE_KEY_STATE: i64 = 3;
pub const PREBATTLE_CACHE_KEY_IS_FULL: i64 = 4;
pub const PREBATTLE_CACHE_KEY_PLAYER_COUNT: i64 = 5;
pub const PREBATTLE_CACHE_KEY_CREATOR: i64 = 6;
pub const PREBATTLE_CACHE_KEY_CREATE_TIME: i64 = 7;
pub const PREBATTLE_CACHE_KEY_DURATION: i64 = 8;
pub const PREBATTLE_CACHE_KEY_COMMENT: i64 = 9;
pub const PREBATTLE_CACHE_KEY_BOTS: i64 = 10;
pub const PREBATTLE_CACHE_KEY_ARENA_TYPE_ID: i64 = 11;
pub const PREBATTLE_CACHE_KEY_ROUND_LENGTH: i64 = 12;
pub const PREBATTLE_CACHE_KEY_PREBATTLES_COUNT: i64 = 13;
pub const PREBATTLE_CACHE_KEY_CREATOR_CLAN_DB_ID: i64 = 14;
pub const PREBATTLE_CACHE_KEY_CREATOR_CLAN_ABBREV: i64 = 15;

#[derive(Debug, Clone)]
pub struct PrebattleSettings {
    pub prb_type: i32,
    pub creator: String,
    pub creator_db_id: i64,
    pub arena_type_id: i32,
    pub round_length: i32,
    pub is_opened: bool,
    pub comment: String,
    pub default_roster: u8,
    pub arena_voip_channels: i32,
    pub create_time: i64,
}

#[derive(Debug, Clone)]
pub struct PrebattleMember {
    pub actor_id: i64,
    pub name: String,
    pub db_id: i64,
    pub roster: u8,
    pub state: u8,
    pub join_time: i64,
    pub veh_inv_id: i64,
    pub veh_comp_descr: Vec<u8>,
    pub clan_db_id: i64,
    pub clan_abbrev: String,
    pub is_bot: bool,
}

impl PrebattleMember {
    fn to_pickle_tuple(&self) -> Value {
        Value::tuple(vec![
            Value::Int(self.actor_id),
            Value::str(self.name.as_str()),
            Value::Int(self.db_id),
            Value::Int(self.roster as i64),
            Value::Int(self.state as i64),
            Value::Int(self.join_time),
            Value::Str(self.veh_comp_descr.clone()),
            Value::Int(self.clan_db_id),
            Value::str(self.clan_abbrev.as_str()),
        ])
    }

    fn team(&self) -> u8 {
        match self.roster {
            PREBATTLE_ROSTER_ASSIGNED_IN_TEAM2 | PREBATTLE_ROSTER_UNASSIGNED_IN_TEAM2 => 2,
            _ => 1,
        }
    }

    fn is_assigned(&self) -> bool {
        self.roster == PREBATTLE_ROSTER_ASSIGNED_IN_TEAM1
            || self.roster == PREBATTLE_ROSTER_ASSIGNED_IN_TEAM2
    }
}

#[derive(Debug, Clone)]
pub struct PrebattleRoom {
    pub id: i32,
    pub settings: PrebattleSettings,
    pub team_states: [u8; 3], // 1-indexed: [0, team1, team2]
    pub members: Vec<PrebattleMember>,
    pub in_battle: bool,
}

impl PrebattleRoom {
    fn human_recipients(&self) -> Vec<i64> {
        self.members
            .iter()
            .filter(|m| !m.is_bot)
            .map(|m| m.db_id)
            .collect()
    }

    pub fn settings_pickle(&self) -> Vec<u8> {
        let roles = Value::dict(vec![(
            Value::Int(self.settings.creator_db_id),
            Value::Int(PREBATTLE_ROLE_TRAINING_CREATOR),
        )]);
        let dict = Value::dict(vec![
            (Value::str("type"), Value::Int(self.settings.prb_type as i64)),
            (Value::str("creator"), Value::str(self.settings.creator.as_str())),
            (
                Value::str("arenaTypeID"),
                Value::Int(self.settings.arena_type_id as i64),
            ),
            (
                Value::str("roundLength"),
                Value::Int(self.settings.round_length as i64),
            ),
            (
                Value::str("isOpened"),
                Value::Bool(self.settings.is_opened),
            ),
            (Value::str("comment"), Value::str(self.settings.comment.as_str())),
            (
                Value::str("defaultRoster"),
                Value::Int(self.settings.default_roster as i64),
            ),
            (
                Value::str("arenaVoipChannels"),
                Value::Int(self.settings.arena_voip_channels as i64),
            ),
            (Value::str("roles"), roles),
        ]);
        pickle::dumps(&dict)
    }

    fn roster_list_value(&self) -> Value {
        Value::list(self.members.iter().map(PrebattleMember::to_pickle_tuple).collect())
    }

    pub fn roster_pickle(&self) -> Vec<u8> {
        pickle::dumps(&self.roster_list_value())
    }

    pub fn team_states_pickle(&self) -> Vec<u8> {
        pickle::dumps(&Value::tuple(vec![
            Value::Int(self.team_states[1] as i64),
            Value::Int(self.team_states[2] as i64),
        ]))
    }

    fn cache_entry_tuple(&self) -> Value {
        let human_or_all_count = self.members.len() as i64;
        let dict = Value::dict(vec![
            (
                Value::Int(PREBATTLE_CACHE_KEY_TYPE),
                Value::Int(self.settings.prb_type as i64),
            ),
            (
                Value::Int(PREBATTLE_CACHE_KEY_IS_OPENED),
                Value::Bool(self.settings.is_opened),
            ),
            (
                Value::Int(PREBATTLE_CACHE_KEY_STATE),
                Value::Int(if self.in_battle { 1 } else { 0 }),
            ),
            (
                Value::Int(PREBATTLE_CACHE_KEY_IS_FULL),
                Value::Bool(self.members.len() >= 30),
            ),
            (
                Value::Int(PREBATTLE_CACHE_KEY_PLAYER_COUNT),
                Value::Int(human_or_all_count),
            ),
            (
                Value::Int(PREBATTLE_CACHE_KEY_CREATOR),
                Value::str(self.settings.creator.as_str()),
            ),
            (
                Value::Int(PREBATTLE_CACHE_KEY_CREATE_TIME),
                Value::Int(self.settings.create_time),
            ),
            (
                Value::Int(PREBATTLE_CACHE_KEY_COMMENT),
                Value::str(self.settings.comment.as_str()),
            ),
            (
                Value::Int(PREBATTLE_CACHE_KEY_ARENA_TYPE_ID),
                Value::Int(self.settings.arena_type_id as i64),
            ),
            (
                Value::Int(PREBATTLE_CACHE_KEY_ROUND_LENGTH),
                Value::Int(self.settings.round_length as i64),
            ),
            (
                Value::Int(PREBATTLE_CACHE_KEY_CREATOR_CLAN_DB_ID),
                Value::Int(0),
            ),
            (
                Value::Int(PREBATTLE_CACHE_KEY_CREATOR_CLAN_ABBREV),
                Value::str(""),
            ),
        ]);
        Value::tuple(vec![
            Value::Int(self.settings.create_time),
            Value::Int(self.id as i64),
            dict,
        ])
    }
}

/// Outbound prebattle state notification emitted by [`PrebattleManager`].
#[derive(Debug, Clone, PartialEq)]
pub enum PrebattleEvent {
    Joined {
        account_db_id: i64,
        prebattle_id: i32,
        initial_updates: Vec<(u8, Vec<u8>)>,
    },
    Left {
        account_db_id: i64,
        kicked_reason: Option<u8>,
    },
    Update {
        recipients: Vec<i64>,
        update_type: u8,
        payload: Vec<u8>,
    },
    LaunchBattle(ArenaLaunchSpec),
}

/// Manages active Training Rooms and other prebattle lobbies.
#[derive(Debug, Default)]
pub struct PrebattleManager {
    rooms: HashMap<i32, PrebattleRoom>,
    account_to_room: HashMap<i64, i32>,
    next_prb_id: i32,
}

impl PrebattleManager {
    pub fn new() -> Self {
        Self {
            rooms: HashMap::new(),
            account_to_room: HashMap::new(),
            next_prb_id: 1,
        }
    }

    pub fn room_for_account(&self, account_db_id: i64) -> Option<i32> {
        self.account_to_room.get(&account_db_id).copied()
    }

    pub fn room(&self, prebattle_id: i32) -> Option<&PrebattleRoom> {
        self.rooms.get(&prebattle_id)
    }

    #[allow(clippy::too_many_arguments)]
    pub fn create_training(
        &mut self,
        creator_db_id: i64,
        creator_actor_id: i64,
        creator_name: &str,
        veh_inv_id: i64,
        veh_comp_descr: Vec<u8>,
        arena_type_id: i32,
        round_length_secs: i32,
        is_opened: bool,
        comment: &str,
        bot_veh_comp_descr: Vec<u8>,
        arenas: &ArenaDefsCatalog,
        now_unix: i64,
    ) -> Result<(i32, Vec<PrebattleEvent>), u8> {
        if self.account_to_room.contains_key(&creator_db_id) {
            return Err(JOIN_FAILURE_ACCOUNT_LOCK);
        }
        if arenas.by_id(arena_type_id).is_none() {
            return Err(crate::lobby::queue::JOIN_FAILURE_WRONG_ARGS);
        }
        let comment = clamp_comment_utf8(comment);
        let prb_id = self.next_prb_id.max(1);
        self.next_prb_id = prb_id.wrapping_add(1).max(1);

        let room = PrebattleRoom {
            id: prb_id,
            settings: PrebattleSettings {
                prb_type: PREBATTLE_TYPE_TRAINING,
                creator: creator_name.to_string(),
                creator_db_id,
                arena_type_id,
                round_length: round_length_secs.max(60),
                is_opened,
                comment,
                default_roster: PREBATTLE_ROSTER_UNASSIGNED_IN_TEAM1,
                arena_voip_channels: 0,
                create_time: now_unix,
            },
            team_states: [0, PREBATTLE_TEAM_STATE_NOT_READY, PREBATTLE_TEAM_STATE_READY],
            members: vec![
                PrebattleMember {
                    actor_id: creator_actor_id,
                    name: creator_name.to_string(),
                    db_id: creator_db_id,
                    roster: PREBATTLE_ROSTER_ASSIGNED_IN_TEAM1,
                    state: PREBATTLE_ACCOUNT_STATE_NOT_READY,
                    join_time: now_unix,
                    veh_inv_id,
                    veh_comp_descr,
                    clan_db_id: 0,
                    clan_abbrev: String::new(),
                    is_bot: false,
                },
                PrebattleMember {
                    actor_id: -10_000 - prb_id as i64,
                    name: "Bot_Opponent".to_string(),
                    db_id: -1,
                    roster: PREBATTLE_ROSTER_ASSIGNED_IN_TEAM2,
                    state: PREBATTLE_ACCOUNT_STATE_READY,
                    join_time: now_unix,
                    veh_inv_id: -1,
                    veh_comp_descr: bot_veh_comp_descr,
                    clan_db_id: 0,
                    clan_abbrev: String::new(),
                    is_bot: true,
                },
            ],
            in_battle: false,
        };

        let initial_updates = vec![
            (PREBATTLE_UPDATE_SETTINGS, room.settings_pickle()),
            (PREBATTLE_UPDATE_TEAM_STATES, room.team_states_pickle()),
            (PREBATTLE_UPDATE_ROSTER, room.roster_pickle()),
        ];

        self.rooms.insert(prb_id, room);
        self.account_to_room.insert(creator_db_id, prb_id);

        Ok((
            prb_id,
            vec![PrebattleEvent::Joined {
                account_db_id: creator_db_id,
                prebattle_id: prb_id,
                initial_updates,
            }],
        ))
    }

    pub fn join(
        &mut self,
        account_db_id: i64,
        actor_id: i64,
        name: &str,
        veh_inv_id: i64,
        veh_comp_descr: Vec<u8>,
        prebattle_id: i32,
        now_unix: i64,
    ) -> Result<Vec<PrebattleEvent>, u8> {
        if self.account_to_room.contains_key(&account_db_id) {
            return Err(JOIN_FAILURE_ACCOUNT_LOCK);
        }
        let room = self
            .rooms
            .get_mut(&prebattle_id)
            .ok_or(JOIN_FAILURE_NOT_FOUND)?;
        if !room.settings.is_opened && account_db_id != room.settings.creator_db_id {
            return Err(JOIN_FAILURE_WRONG_ARGS);
        }
        if room.members.len() >= 30 {
            return Err(JOIN_FAILURE_TEAM_IS_FULL);
        }

        let existing_recipients = room.human_recipients();
        let member = PrebattleMember {
            actor_id,
            name: name.to_string(),
            db_id: account_db_id,
            roster: room.settings.default_roster,
            state: PREBATTLE_ACCOUNT_STATE_NOT_READY,
            join_time: now_unix,
            veh_inv_id,
            veh_comp_descr,
            clan_db_id: 0,
            clan_abbrev: String::new(),
            is_bot: false,
        };
        let added_payload = pickle::dumps(&member.to_pickle_tuple());
        room.members.push(member);
        self.account_to_room.insert(account_db_id, prebattle_id);

        let initial_updates = vec![
            (PREBATTLE_UPDATE_SETTINGS, room.settings_pickle()),
            (PREBATTLE_UPDATE_TEAM_STATES, room.team_states_pickle()),
            (PREBATTLE_UPDATE_ROSTER, room.roster_pickle()),
        ];

        let mut events = Vec::new();
        if !existing_recipients.is_empty() {
            events.push(PrebattleEvent::Update {
                recipients: existing_recipients,
                update_type: PREBATTLE_UPDATE_PLAYER_ADDED,
                payload: added_payload,
            });
        }
        events.push(PrebattleEvent::Joined {
            account_db_id,
            prebattle_id,
            initial_updates,
        });
        Ok(events)
    }

    pub fn leave(&mut self, account_db_id: i64) -> Result<Vec<PrebattleEvent>, i16> {
        let Some(prb_id) = self.account_to_room.remove(&account_db_id) else {
            return Err(RES_FAILURE);
        };
        let Some(room) = self.rooms.get_mut(&prb_id) else {
            return Err(RES_FAILURE);
        };

        let is_creator = room.settings.creator_db_id == account_db_id;
        let pos = room
            .members
            .iter()
            .position(|m| m.db_id == account_db_id && !m.is_bot)
            .ok_or(RES_FAILURE)?;
        let removed = room.members.remove(pos);
        let remaining_humans = room.human_recipients();

        let mut events = vec![PrebattleEvent::Left {
            account_db_id,
            kicked_reason: None,
        }];

        if remaining_humans.is_empty() {
            self.rooms.remove(&prb_id);
        } else if is_creator {
            for human_db_id in &remaining_humans {
                self.account_to_room.remove(human_db_id);
                events.push(PrebattleEvent::Left {
                    account_db_id: *human_db_id,
                    kicked_reason: Some(PREBATTLE_KICK_REASON_CREATOR_LEFT),
                });
            }
            self.rooms.remove(&prb_id);
        } else {
            let payload = pickle::dumps(&Value::tuple(vec![
                Value::Int(removed.actor_id),
                Value::Int(removed.roster as i64),
            ]));
            events.push(PrebattleEvent::Update {
                recipients: remaining_humans,
                update_type: PREBATTLE_UPDATE_PLAYER_REMOVED,
                payload,
            });
        }
        Ok(events)
    }

    /// Synchronize a room member's active `Account` entity ID (`actor_id`), e.g.
    /// after returning from an arena or recovering from a preferred-ID collision.
    /// Emits `PREBATTLE_UPDATE_ROSTER` to other human room members if changed.
    pub fn set_member_actor_id(&mut self, account_db_id: i64, actor_id: i64) -> Vec<PrebattleEvent> {
        let Some(prb_id) = self.account_to_room.get(&account_db_id).copied() else {
            return Vec::new();
        };
        let Some(room) = self.rooms.get_mut(&prb_id) else {
            return Vec::new();
        };
        let Some(member) = room
            .members
            .iter_mut()
            .find(|m| m.db_id == account_db_id && !m.is_bot)
        else {
            return Vec::new();
        };
        if member.actor_id == actor_id {
            return Vec::new();
        }
        member.actor_id = actor_id;
        let other_recipients: Vec<i64> = room
            .human_recipients()
            .into_iter()
            .filter(|id| *id != account_db_id)
            .collect();
        if other_recipients.is_empty() {
            return Vec::new();
        }
        vec![PrebattleEvent::Update {
            recipients: other_recipients,
            update_type: PREBATTLE_UPDATE_ROSTER,
            payload: room.roster_pickle(),
        }]
    }

    pub fn set_ready(
        &mut self,
        account_db_id: i64,
        veh_inv_id: i64,
        veh_comp_descr: Vec<u8>,
        ready: bool,
    ) -> Result<Vec<PrebattleEvent>, i16> {
        let prb_id = self.account_to_room.get(&account_db_id).copied().ok_or(RES_FAILURE)?;
        let room = self.rooms.get_mut(&prb_id).ok_or(RES_FAILURE)?;
        let member = room
            .members
            .iter_mut()
            .find(|m| m.db_id == account_db_id && !m.is_bot)
            .ok_or(RES_FAILURE)?;

        member.veh_inv_id = veh_inv_id;
        member.veh_comp_descr = veh_comp_descr.clone();
        member.state = if ready {
            PREBATTLE_ACCOUNT_STATE_READY
        } else {
            PREBATTLE_ACCOUNT_STATE_NOT_READY
        };

        let payload = pickle::dumps(&Value::tuple(vec![
            Value::Int(member.actor_id),
            Value::Int(member.roster as i64),
            Value::Int(member.state as i64),
            Value::Str(veh_comp_descr),
        ]));
        let team = member.team() as usize;
        let mut events = vec![PrebattleEvent::Update {
            recipients: room.human_recipients(),
            update_type: PREBATTLE_UPDATE_PLAYER_STATE,
            payload,
        }];

        if !ready && room.team_states[team] == PREBATTLE_TEAM_STATE_READY {
            room.team_states[team] = PREBATTLE_TEAM_STATE_NOT_READY;
            events.push(PrebattleEvent::Update {
                recipients: room.human_recipients(),
                update_type: PREBATTLE_UPDATE_TEAM_STATES,
                payload: room.team_states_pickle(),
            });
        }
        Ok(events)
    }

    pub fn assign(
        &mut self,
        actor_db_id: i64,
        target_db_id: i64,
        roster: u8,
    ) -> Result<Vec<PrebattleEvent>, i16> {
        if !matches!(
            roster,
            PREBATTLE_ROSTER_ASSIGNED_IN_TEAM1
                | PREBATTLE_ROSTER_ASSIGNED_IN_TEAM2
                | PREBATTLE_ROSTER_UNASSIGNED_IN_TEAM1
                | PREBATTLE_ROSTER_UNASSIGNED_IN_TEAM2
        ) {
            return Err(RES_WRONG_ARGS);
        }
        let prb_id = self.account_to_room.get(&actor_db_id).copied().ok_or(RES_FAILURE)?;
        let room = self.rooms.get_mut(&prb_id).ok_or(RES_FAILURE)?;
        let target_idx = room
            .members
            .iter()
            .position(|m| m.actor_id == target_db_id)
            .or_else(|| room.members.iter().position(|m| m.db_id == target_db_id))
            .ok_or(RES_FAILURE)?;
        // Client sends Account `actor_id` (which changes after returning from battle), so resolve
        // the target member before checking non-creator self-assignment ownership.
        let target_member = &room.members[target_idx];
        if room.settings.creator_db_id != actor_db_id
            && (target_member.is_bot || target_member.db_id != actor_db_id)
        {
            return Err(RES_FAILURE);
        }
        let acting_actor_id = room
            .members
            .iter()
            .find(|m| m.db_id == actor_db_id && !m.is_bot)
            .map(|m| m.actor_id)
            .unwrap_or(actor_db_id);
        let member = &mut room.members[target_idx];
        let prev_roster = member.roster;
        member.roster = roster;
        let payload = pickle::dumps(&Value::tuple(vec![
            Value::Int(member.actor_id),
            Value::Int(prev_roster as i64),
            Value::Int(roster as i64),
            Value::Int(acting_actor_id),
        ]));
        Ok(vec![PrebattleEvent::Update {
            recipients: room.human_recipients(),
            update_type: PREBATTLE_UPDATE_PLAYER_ROSTER,
            payload,
        }])
    }

    pub fn swap_team(&mut self, actor_db_id: i64) -> Result<Vec<PrebattleEvent>, i16> {
        let prb_id = self.account_to_room.get(&actor_db_id).copied().ok_or(RES_FAILURE)?;
        let room = self.rooms.get_mut(&prb_id).ok_or(RES_FAILURE)?;
        if room.settings.creator_db_id != actor_db_id {
            return Err(RES_FAILURE);
        }
        for m in &mut room.members {
            m.roster = match m.roster {
                PREBATTLE_ROSTER_ASSIGNED_IN_TEAM1 => PREBATTLE_ROSTER_ASSIGNED_IN_TEAM2,
                PREBATTLE_ROSTER_ASSIGNED_IN_TEAM2 => PREBATTLE_ROSTER_ASSIGNED_IN_TEAM1,
                PREBATTLE_ROSTER_UNASSIGNED_IN_TEAM1 => PREBATTLE_ROSTER_UNASSIGNED_IN_TEAM2,
                PREBATTLE_ROSTER_UNASSIGNED_IN_TEAM2 => PREBATTLE_ROSTER_UNASSIGNED_IN_TEAM1,
                other => other,
            };
        }
        Ok(vec![PrebattleEvent::Update {
            recipients: room.human_recipients(),
            update_type: PREBATTLE_UPDATE_ROSTER,
            payload: room.roster_pickle(),
        }])
    }

    pub fn change_arena(
        &mut self,
        actor_db_id: i64,
        arena_type_id: i32,
        arenas: &ArenaDefsCatalog,
    ) -> Result<Vec<PrebattleEvent>, i16> {
        if arenas.by_id(arena_type_id).is_none() {
            return Err(RES_WRONG_ARGS);
        }
        self.update_setting(actor_db_id, "arenaTypeID", Value::Int(arena_type_id as i64), |s| {
            s.arena_type_id = arena_type_id;
        })
    }

    pub fn change_round_length(
        &mut self,
        actor_db_id: i64,
        round_length: i32,
    ) -> Result<Vec<PrebattleEvent>, i16> {
        let clamped = round_length.max(60);
        self.update_setting(actor_db_id, "roundLength", Value::Int(clamped as i64), |s| {
            s.round_length = clamped;
        })
    }

    pub fn change_open_status(
        &mut self,
        actor_db_id: i64,
        is_opened: bool,
    ) -> Result<Vec<PrebattleEvent>, i16> {
        self.update_setting(actor_db_id, "isOpened", Value::Bool(is_opened), |s| {
            s.is_opened = is_opened;
        })
    }

    pub fn change_comment(
        &mut self,
        actor_db_id: i64,
        comment: &str,
    ) -> Result<Vec<PrebattleEvent>, i16> {
        let clamped = clamp_comment_utf8(comment);
        let owned = clamped.clone();
        self.update_setting(actor_db_id, "comment", Value::str(clamped.as_str()), move |s| {
            s.comment = owned.clone();
        })
    }

    pub fn change_arena_voip(
        &mut self,
        actor_db_id: i64,
        voip_channels: i32,
    ) -> Result<Vec<PrebattleEvent>, i16> {
        self.update_setting(
            actor_db_id,
            "arenaVoipChannels",
            Value::Int(voip_channels as i64),
            |s| {
                s.arena_voip_channels = voip_channels;
            },
        )
    }

    fn update_setting(
        &mut self,
        actor_db_id: i64,
        key: &str,
        val: Value,
        apply: impl FnOnce(&mut PrebattleSettings),
    ) -> Result<Vec<PrebattleEvent>, i16> {
        let prb_id = self.account_to_room.get(&actor_db_id).copied().ok_or(RES_FAILURE)?;
        let room = self.rooms.get_mut(&prb_id).ok_or(RES_FAILURE)?;
        if room.settings.creator_db_id != actor_db_id {
            return Err(RES_FAILURE);
        }
        apply(&mut room.settings);
        let payload = pickle::dumps(&Value::tuple(vec![Value::str(key), val]));
        Ok(vec![PrebattleEvent::Update {
            recipients: room.human_recipients(),
            update_type: PREBATTLE_UPDATE_SETTING,
            payload,
        }])
    }

    pub fn set_team_ready(
        &mut self,
        actor_db_id: i64,
        team: u8,
        ready: bool,
        force: bool,
        arenas: &ArenaDefsCatalog,
    ) -> Result<Vec<PrebattleEvent>, i16> {
        if team != 1 && team != 2 {
            return Err(RES_WRONG_ARGS);
        }
        let prb_id = self.account_to_room.get(&actor_db_id).copied().ok_or(RES_FAILURE)?;
        let room = self.rooms.get_mut(&prb_id).ok_or(RES_FAILURE)?;
        if room.settings.creator_db_id != actor_db_id {
            return Err(RES_FAILURE);
        }
        if room.in_battle {
            return Ok(Vec::new());
        }

        let new_state = if ready {
            PREBATTLE_TEAM_STATE_READY
        } else {
            PREBATTLE_TEAM_STATE_NOT_READY
        };

        if ready && !force {
            let team_has_ready = room
                .members
                .iter()
                .any(|m| m.team() == team && m.is_assigned() && m.state == PREBATTLE_ACCOUNT_STATE_READY);
            if !team_has_ready {
                return Err(RES_FAILURE);
            }
        }

        room.team_states[team as usize] = new_state;
        let recipients = room.human_recipients();
        let mut events = vec![PrebattleEvent::Update {
            recipients: recipients.clone(),
            update_type: PREBATTLE_UPDATE_TEAM_STATES,
            payload: room.team_states_pickle(),
        }];

        if room.team_states[1] == PREBATTLE_TEAM_STATE_READY
            && room.team_states[2] == PREBATTLE_TEAM_STATE_READY
        {
            let has_t1 = room
                .members
                .iter()
                .any(|m| m.team() == 1 && m.is_assigned());
            let has_t2 = room
                .members
                .iter()
                .any(|m| m.team() == 2 && m.is_assigned());
            if has_t1 && has_t2 {
                room.in_battle = true;
                for m in &mut room.members {
                    if m.is_assigned() && !m.is_bot {
                        m.state = PREBATTLE_ACCOUNT_STATE_IN_BATTLE;
                    }
                }
                let arena_def = arenas.by_id(room.settings.arena_type_id);
                let round_length_secs = if room.settings.round_length > 0 {
                    room.settings.round_length as u32
                } else {
                    arena_def.map(|d| d.round_length_secs).unwrap_or(900)
                };
                let participants = room
                    .members
                    .iter()
                    .filter(|m| m.is_assigned())
                    .map(|m| ParticipantSpec {
                        account_db_id: m.db_id,
                        player_name: m.name.clone(),
                        veh_inv_id: m.veh_inv_id,
                        veh_comp_descr: m.veh_comp_descr.clone(),
                        team: m.team(),
                        prebattle_id: room.id,
                        is_bot: m.is_bot,
                        ..Default::default()
                    })
                    .collect();
                events.push(PrebattleEvent::Update {
                    recipients,
                    update_type: PREBATTLE_UPDATE_ROSTER,
                    payload: room.roster_pickle(),
                });
                events.push(PrebattleEvent::LaunchBattle(ArenaLaunchSpec {
                    arena_type_id: room.settings.arena_type_id,
                    arena_gui_type: ARENA_GUI_TYPE_TRAINING,
                    arena_bonus_type: ARENA_BONUS_TYPE_TRAINING,
                    mode: BattleMode::Training,
                    round_length_secs,
                    prebattle_id: room.id,
                    participants,
                }));
            }
        }
        Ok(events)
    }

    pub fn kick(&mut self, actor_db_id: i64, target_db_id: i64) -> Result<Vec<PrebattleEvent>, i16> {
        let prb_id = self.account_to_room.get(&actor_db_id).copied().ok_or(RES_FAILURE)?;
        let room = self.rooms.get_mut(&prb_id).ok_or(RES_FAILURE)?;
        if room.settings.creator_db_id != actor_db_id {
            return Err(RES_FAILURE);
        }
        let pos = room
            .members
            .iter()
            .position(|m| !m.is_bot && m.actor_id == target_db_id)
            .or_else(|| {
                room.members
                    .iter()
                    .position(|m| !m.is_bot && m.db_id == target_db_id)
            })
            .ok_or(RES_FAILURE)?;
        if room.members[pos].db_id == actor_db_id {
            return Err(RES_FAILURE);
        }
        let removed = room.members.remove(pos);
        self.account_to_room.remove(&removed.db_id);
        let remaining = room.human_recipients();
        let payload = pickle::dumps(&Value::tuple(vec![
            Value::Int(removed.actor_id),
            Value::Int(removed.roster as i64),
        ]));
        let mut events = vec![PrebattleEvent::Left {
            account_db_id: removed.db_id,
            kicked_reason: Some(1),
        }];
        if !remaining.is_empty() {
            events.push(PrebattleEvent::Update {
                recipients: remaining,
                update_type: PREBATTLE_UPDATE_PLAYER_REMOVED,
                payload,
            });
        }
        Ok(events)
    }

    pub fn notify_arena_finished(&mut self, prebattle_id: i32) -> Vec<PrebattleEvent> {
        let Some(room) = self.rooms.get_mut(&prebattle_id) else {
            return Vec::new();
        };
        room.in_battle = false;
        room.team_states[1] = PREBATTLE_TEAM_STATE_NOT_READY;
        let team2_bot_only = !room.members.is_empty()
            && room
                .members
                .iter()
                .filter(|m| m.team() == 2)
                .all(|m| m.is_bot);
        room.team_states[2] = if team2_bot_only {
            PREBATTLE_TEAM_STATE_READY
        } else {
            PREBATTLE_TEAM_STATE_NOT_READY
        };
        for m in &mut room.members {
            if !m.is_bot {
                m.state = PREBATTLE_ACCOUNT_STATE_NOT_READY;
            }
        }
        let recipients = room.human_recipients();
        if recipients.is_empty() {
            return Vec::new();
        }
        vec![
            PrebattleEvent::Update {
                recipients: recipients.clone(),
                update_type: PREBATTLE_UPDATE_TEAM_STATES,
                payload: room.team_states_pickle(),
            },
            PrebattleEvent::Update {
                recipients,
                update_type: PREBATTLE_UPDATE_ROSTER,
                payload: room.roster_pickle(),
            },
        ]
    }

    pub fn query_prebattles_pickle(
        &self,
        prb_type: i32,
        _sort_key: i32,
        idle: bool,
        start: usize,
        end: usize,
    ) -> Vec<u8> {
        let mut items: Vec<&PrebattleRoom> = self
            .rooms
            .values()
            .filter(|r| (prb_type == 0 || r.settings.prb_type == prb_type) && (!idle || !r.in_battle))
            .collect();
        items.sort_by_key(|r| r.id);
        let total = items.len();
        let slice = items
            .into_iter()
            .skip(start)
            .take(end.saturating_sub(start))
            .map(PrebattleRoom::cache_entry_tuple)
            .collect();
        pickle::dumps(&Value::tuple(vec![
            Value::Int(prb_type as i64),
            Value::Int(total as i64),
            Value::list(slice),
        ]))
    }

    pub fn query_prebattles_by_creator_pickle(
        &self,
        prb_type: i32,
        idle: bool,
        creator_mask: &str,
    ) -> Vec<u8> {
        let mask = creator_mask.trim().to_ascii_lowercase();
        let mut items: Vec<&PrebattleRoom> = self
            .rooms
            .values()
            .filter(|r| {
                (prb_type == 0 || r.settings.prb_type == prb_type)
                    && (!idle || !r.in_battle)
                    && (mask.is_empty() || r.settings.creator.to_ascii_lowercase().contains(&mask))
            })
            .collect();
        items.sort_by_key(|r| r.id);
        let total = items.len();
        let entries = items.into_iter().map(PrebattleRoom::cache_entry_tuple).collect();
        pickle::dumps(&Value::tuple(vec![
            Value::Int(prb_type as i64),
            Value::Int(total as i64),
            Value::list(entries),
        ]))
    }

    pub fn query_roster_pickle(&self, prebattle_id: i32) -> Result<Vec<u8>, i16> {
        let room = self.rooms.get(&prebattle_id).ok_or(RES_FAILURE)?;
        Ok(pickle::dumps(&Value::tuple(vec![
            Value::Int(prebattle_id as i64),
            room.roster_list_value(),
        ])))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::assets::arena_defs::ArenaDef;
    use crate::lobby::BattleMode;
    use wot065_data::pickle;

    fn sample_arenas() -> ArenaDefsCatalog {
        ArenaDefsCatalog {
            maps: vec![ArenaDef {
                id: 3,
                name: "04_himmelsdorf".into(),
                display_name_key: "Himmelsdorf".into(),
                description_key: "".into(),
                geometry: "spaces/04_himmelsdorf".into(),
                bounding_box: ([-300.0, -300.0], [400.0, 400.0]),
                min_players_in_team: 1,
                max_players_in_team: 15,
                round_length_secs: 900,
                arena_start_delay_secs: 5.0,
                kick_after_finish_wait_secs: 10.0,
                minimap_texture: None,
            }],
        }
    }

    #[test]
    fn training_room_lifecycle_bot_readiness_queries_and_launch() {
        let mut mgr = PrebattleManager::new();
        let arenas = sample_arenas();
        let veh = vec![0x01u8; 15];
        let bot_veh = vec![0x02u8; 15];

        let (prb_id, events) = mgr
            .create_training(100, 100, "Tester", 1, veh.clone(), 3, 900, true, "test room", bot_veh, &arenas, 1_700_000_000)
            .unwrap();
        assert!(prb_id > 0);
        assert_eq!(mgr.room_for_account(100), Some(prb_id));

        let joined = events.iter().find_map(|e| match e {
            PrebattleEvent::Joined { initial_updates, .. } => Some(initial_updates),
            _ => None,
        }).expect("Joined event");

        // Verify SETTINGS, TEAM_STATES, and ROSTER with Team-2 ready bot
        let mut saw_settings = false;
        let mut saw_roster = false;
        let mut saw_team_states = false;
        for (utype, payload) in joined {
            let val = pickle::loads(payload).unwrap();
            match *utype {
                PREBATTLE_UPDATE_SETTINGS => {
                    saw_settings = true;
                    assert_eq!(val.get("type").and_then(|v| v.as_int()), Some(PREBATTLE_TYPE_TRAINING as i64));
                    assert_eq!(val.get("arenaTypeID").and_then(|v| v.as_int()), Some(3));
                    let roles = val.get("roles").unwrap();
                    assert_eq!(roles.get_int(100).and_then(|v| v.as_int()), Some(PREBATTLE_ROLE_TRAINING_CREATOR));
                }
                PREBATTLE_UPDATE_ROSTER => {
                    saw_roster = true;
                    let entries = val.as_list().unwrap();
                    assert_eq!(entries.len(), 2);
                    // Team-2 bot must be roster=2, state=READY(4) with byte string vehCompDescr
                    let bot = entries.iter().find(|t| t.as_list().unwrap()[3].as_int() == Some(2)).unwrap().as_list().unwrap();
                    assert_eq!(bot[4].as_int(), Some(PREBATTLE_ACCOUNT_STATE_READY as i64));
                    assert!(bot[6].as_bytes().is_some());
                }
                PREBATTLE_UPDATE_TEAM_STATES => {
                    saw_team_states = true;
                    let tuple = val.as_list().unwrap();
                    assert_eq!(tuple[0].as_int(), Some(PREBATTLE_TEAM_STATE_NOT_READY as i64));
                    assert_eq!(tuple[1].as_int(), Some(PREBATTLE_TEAM_STATE_READY as i64));
                }
                _ => {}
            }
        }
        assert!(saw_settings && saw_roster && saw_team_states);

        // Query list & roster
        let list_val = pickle::loads(&mgr.query_prebattles_pickle(PREBATTLE_TYPE_TRAINING, 0, false, 0, 20)).unwrap();
        let list_tuple = list_val.as_list().unwrap();
        assert_eq!(list_tuple[0].as_int(), Some(PREBATTLE_TYPE_TRAINING as i64));
        assert_eq!(list_tuple[1].as_int(), Some(1));

        let roster_val = pickle::loads(&mgr.query_roster_pickle(prb_id).unwrap()).unwrap();
        assert_eq!(roster_val.as_list().unwrap()[0].as_int(), Some(prb_id as i64));

        // Change comment, assign, ready, team_ready -> LaunchBattle
        let comment_evs = mgr.change_comment(100, "updated").unwrap();
        assert!(comment_evs.iter().any(|e| matches!(e, PrebattleEvent::Update { update_type: PREBATTLE_UPDATE_SETTING, .. })));

        let ready_evs = mgr.set_ready(100, 1, veh.clone(), true).unwrap();
        assert!(ready_evs.iter().any(|e| matches!(e, PrebattleEvent::Update { update_type: PREBATTLE_UPDATE_PLAYER_STATE, .. })));

        // Training.py sets team 1 & team 2 ready on Start
        let t1_evs = mgr.set_team_ready(100, 1, true, true, &arenas).unwrap();
        let launch_from_t1 = t1_evs.iter().find_map(|e| match e {
            PrebattleEvent::LaunchBattle(spec) => Some(spec.clone()),
            _ => None,
        });
        let launch = if let Some(spec) = launch_from_t1 {
            spec
        } else {
            let t2_evs = mgr.set_team_ready(100, 2, true, true, &arenas).unwrap();
            t2_evs.into_iter().find_map(|e| match e {
                PrebattleEvent::LaunchBattle(spec) => Some(spec),
                _ => None,
            }).expect("LaunchBattle when both teams ready")
        };

        assert_eq!(launch.mode, BattleMode::Training);
        assert_eq!(launch.prebattle_id, prb_id);
        assert_eq!(launch.participants.len(), 2);
        assert!(mgr.notify_arena_finished(prb_id).iter().any(|e| matches!(e, PrebattleEvent::Update { update_type: PREBATTLE_UPDATE_TEAM_STATES, .. })));
        let leave_evs = mgr.leave(100).unwrap();
        assert!(leave_evs.iter().any(|e| matches!(e, PrebattleEvent::Left { account_db_id: 100, .. })));
        assert!(mgr.room_for_account(100).is_none());
    }

    #[test]
    fn creator_leave_disbands_room_for_remaining_humans() {
        let mut mgr = PrebattleManager::new();
        let arenas = sample_arenas();
        let veh = vec![0x01u8; 15];
        let bot_veh = vec![0x02u8; 15];
        let (prb_id, _) = mgr
            .create_training(100, 100, "Creator", 1, veh.clone(), 3, 900, true, "room", bot_veh, &arenas, 1_700_000_000)
            .unwrap();
        mgr.join(101, 101, "Guest", 2, veh, prb_id, 1_700_000_001).unwrap();

        let evs = mgr.leave(100).unwrap();
        assert!(evs.iter().any(|e| matches!(
            e,
            PrebattleEvent::Left {
                account_db_id: 100,
                kicked_reason: None
            }
        )));
        assert!(evs.iter().any(|e| matches!(
            e,
            PrebattleEvent::Left {
                account_db_id: 101,
                kicked_reason: Some(9) // KICK_REASON.CREATOR_LEFT
            }
        )));
        assert!(mgr.room_for_account(101).is_none(), "guest must be removed from disbanded room");
        assert!(mgr.room(prb_id).is_none(), "disbanded room must be deleted");
    }

    #[test]
    fn assign_emits_acting_player_actor_id_in_tuple_slot_three() {
        let mut mgr = PrebattleManager::new();
        let arenas = sample_arenas();
        let veh = vec![0x01u8; 15];
        let bot_veh = vec![0x02u8; 15];
        let (prb_id, _) = mgr
            .create_training(100, 5100, "Creator", 1, veh.clone(), 3, 900, true, "room", bot_veh, &arenas, 1_700_000_000)
            .unwrap();
        mgr.join(101, 5101, "Guest", 2, veh, prb_id, 1_700_000_001).unwrap();

        let evs = mgr.assign(100, 5101, PREBATTLE_ROSTER_ASSIGNED_IN_TEAM2).unwrap();
        let payload = evs
            .iter()
            .find_map(|e| match e {
                PrebattleEvent::Update {
                    update_type: PREBATTLE_UPDATE_PLAYER_ROSTER,
                    payload,
                    ..
                } => Some(payload),
                _ => None,
            })
            .expect("PLAYER_ROSTER update");
        let tuple = pickle::loads(payload).unwrap();
        let items = tuple.as_list().unwrap();
        let target_actor_id = items[0].as_int().unwrap();
        let acting_actor_id = items[3].as_int().unwrap();
        assert_eq!(target_actor_id, 5101);
        assert_eq!(acting_actor_id, 5100, "slot 3 must carry acting player's actor_id");
    }

    #[test]
    fn comment_clamped_safely_at_utf8_char_boundary() {
        let mut mgr = PrebattleManager::new();
        let arenas = sample_arenas();
        let veh = vec![0x01u8; 15];
        let bot_veh = vec![0x02u8; 15];
        let long_ascii = "a".repeat(320);
        let (prb_id, _) = mgr
            .create_training(100, 100, "Creator", 1, veh, 3, 900, true, &long_ascii, bot_veh, &arenas, 1_700_000_000)
            .unwrap();
        assert!(mgr.room(prb_id).unwrap().settings.comment.len() <= 256);

        let long_emoji = "🦀".repeat(80); // 320 bytes, 4 bytes per char
        mgr.change_comment(100, &long_emoji).unwrap();
        let saved = &mgr.room(prb_id).unwrap().settings.comment;
        assert!(saved.len() <= 256);
        assert_eq!(saved.len(), 256);
        assert!(std::str::from_utf8(saved.as_bytes()).is_ok());
    }

    #[test]
    fn change_arena_kick_and_set_member_actor_id_validate_and_sync() {
        let mut arenas = sample_arenas();
        let mut second_map = arenas.maps[0].clone();
        second_map.id = 5;
        second_map.name = "05_prohorovka".into();
        arenas.maps.push(second_map);

        let mut mgr = PrebattleManager::new();
        let veh = vec![0x01u8; 15];
        let bot_veh = vec![0x02u8; 15];
        assert_eq!(
            mgr.create_training(100, 900, "Creator", 1, veh.clone(), 999, 900, true, "bad", bot_veh.clone(), &arenas, 1_700_000_000),
            Err(JOIN_FAILURE_WRONG_ARGS)
        );
        let (prb_id, _) = mgr
            .create_training(100, 900, "Creator", 1, veh.clone(), 3, 900, true, "room", bot_veh, &arenas, 1_700_000_000)
            .unwrap();
        mgr.join(101, 901, "Guest", 2, veh, prb_id, 1_700_000_001).unwrap();

        // Unknown arena rejected, valid arena updates settings + notifies peers
        assert_eq!(mgr.change_arena(100, 999, &arenas), Err(RES_WRONG_ARGS));
        let ch_evs = mgr.change_arena(100, 5, &arenas).unwrap();
        assert_eq!(mgr.room(prb_id).unwrap().settings.arena_type_id, 5);
        assert!(ch_evs.iter().any(|e| matches!(
            e,
            PrebattleEvent::Update { update_type: PREBATTLE_UPDATE_SETTING, .. }
        )));

        // Non-creator kick fails; creator kick removes guest and notifies creator
        assert_eq!(mgr.kick(101, 100), Err(RES_FAILURE));
        let kick_evs = mgr.kick(100, 901).unwrap();
        assert!(mgr.room_for_account(101).is_none());
        assert!(kick_evs.iter().any(|e| matches!(
            e,
            PrebattleEvent::Left { account_db_id: 101, kicked_reason: Some(1) }
        )));
        assert!(kick_evs.iter().any(|e| matches!(
            e,
            PrebattleEvent::Update { update_type: PREBATTLE_UPDATE_PLAYER_REMOVED, .. }
        )));

        // Re-join guest and test set_member_actor_id updates roster and notifies other humans
        mgr.join(102, 902, "Observer", 3, vec![0x01u8; 15], prb_id, 1_700_000_002).unwrap();
        let sync_evs = mgr.set_member_actor_id(100, 1234);
        assert_eq!(sync_evs.len(), 1);
        match &sync_evs[0] {
            PrebattleEvent::Update { recipients, update_type, payload } => {
                assert_eq!(recipients, &vec![102]);
                assert_eq!(*update_type, PREBATTLE_UPDATE_ROSTER);
                let val = pickle::loads(payload).unwrap();
                let entries = val.as_list().unwrap();
                assert!(entries.iter().any(|t| t.as_list().unwrap()[0].as_int() == Some(1234)));
            }
            other => panic!("unexpected event {other:?}"),
        }
        assert!(mgr.set_member_actor_id(100, 1234).is_empty(), "unchanged actor_id emits nothing");
    }

    #[test]
    fn non_creator_self_assign_succeeds_after_actor_id_change_and_rejects_other_targets() {
        let mut mgr = PrebattleManager::new();
        let arenas = sample_arenas();
        let veh = vec![0x01u8; 15];
        let bot_veh = vec![0x02u8; 15];
        let (prb_id, _) = mgr
            .create_training(100, 100, "Creator", 1, veh.clone(), 3, 900, true, "room", bot_veh, &arenas, 1_700_000_000)
            .unwrap();
        mgr.join(101, 101, "Guest", 2, veh, prb_id, 1_700_000_001).unwrap();

        // Guest returns from battle and receives a fresh Account entity ID != db_id.
        mgr.set_member_actor_id(101, 7777);

        let evs = mgr
            .assign(101, 7777, PREBATTLE_ROSTER_ASSIGNED_IN_TEAM1)
            .expect("non-creator self-assign by actor_id != db_id must succeed");
        let payload = evs
            .iter()
            .find_map(|e| match e {
                PrebattleEvent::Update {
                    update_type: PREBATTLE_UPDATE_PLAYER_ROSTER,
                    payload,
                    ..
                } => Some(payload),
                _ => None,
            })
            .expect("PLAYER_ROSTER update");
        let tuple = pickle::loads(payload).unwrap();
        let items = tuple.as_list().unwrap();
        assert_eq!(items[0].as_int(), Some(7777));
        assert_eq!(items[2].as_int(), Some(PREBATTLE_ROSTER_ASSIGNED_IN_TEAM1 as i64));
        assert_eq!(items[3].as_int(), Some(7777));

        // Non-creator still cannot assign another player (creator).
        assert_eq!(
            mgr.assign(101, 100, PREBATTLE_ROSTER_ASSIGNED_IN_TEAM2),
            Err(RES_FAILURE)
        );
    }
}
