//! Matchmaking queue and prebattle (Training room) lobby modules.

pub mod prebattle;
pub mod queue;

pub use prebattle::{
    PrebattleEvent, PrebattleManager, PrebattleMember, PrebattleRoom, PrebattleSettings,
    PREBATTLE_ACCOUNT_STATE_IN_BATTLE, PREBATTLE_ACCOUNT_STATE_NOT_READY,
    PREBATTLE_ACCOUNT_STATE_OFFLINE, PREBATTLE_ACCOUNT_STATE_READY,
    PREBATTLE_ACCOUNT_STATE_UNKNOWN, PREBATTLE_CACHE_KEY_ARENA_TYPE_ID,
    PREBATTLE_CACHE_KEY_BOTS, PREBATTLE_CACHE_KEY_COMMENT, PREBATTLE_CACHE_KEY_CREATE_TIME,
    PREBATTLE_CACHE_KEY_CREATOR, PREBATTLE_CACHE_KEY_CREATOR_CLAN_ABBREV,
    PREBATTLE_CACHE_KEY_CREATOR_CLAN_DB_ID, PREBATTLE_CACHE_KEY_DURATION,
    PREBATTLE_CACHE_KEY_IS_FULL, PREBATTLE_CACHE_KEY_IS_OPENED, PREBATTLE_CACHE_KEY_PLAYER_COUNT,
    PREBATTLE_CACHE_KEY_PREBATTLES_COUNT, PREBATTLE_CACHE_KEY_ROUND_LENGTH,
    PREBATTLE_CACHE_KEY_STATE, PREBATTLE_CACHE_KEY_TYPE, PREBATTLE_ROLE_NONE,
    PREBATTLE_ROLE_TRAINING_CREATOR, PREBATTLE_ROSTER_ASSIGNED_IN_TEAM1,
    PREBATTLE_ROSTER_ASSIGNED_IN_TEAM2, PREBATTLE_ROSTER_UNASSIGNED,
    PREBATTLE_ROSTER_UNASSIGNED_IN_TEAM1, PREBATTLE_ROSTER_UNASSIGNED_IN_TEAM2,
    PREBATTLE_START_TYPE_DIRECT, PREBATTLE_TEAM_STATE_LOCKED, PREBATTLE_TEAM_STATE_NOT_READY,
    PREBATTLE_TEAM_STATE_READY, PREBATTLE_TYPE_COMPANY, PREBATTLE_TYPE_SQUAD,
    PREBATTLE_TYPE_TRAINING, PREBATTLE_UPDATE_KICKED_FROM_QUEUE, PREBATTLE_UPDATE_PLAYER_ADDED,
    PREBATTLE_UPDATE_PLAYER_REMOVED, PREBATTLE_UPDATE_PLAYER_ROSTER,
    PREBATTLE_UPDATE_PLAYER_STATE, PREBATTLE_UPDATE_PROPERTIES, PREBATTLE_UPDATE_PROPERTY,
    PREBATTLE_UPDATE_ROSTER, PREBATTLE_UPDATE_SETTING, PREBATTLE_UPDATE_SETTINGS,
    PREBATTLE_UPDATE_TEAM_STATES,
};
pub use queue::{
    compute_vehicle_weight, vehicle_class_index, QueueConfig, QueueEntry, QueueInfoSnapshot,
    QueueManager, JOIN_FAILURE_ACCOUNT_LOCK, JOIN_FAILURE_ARENA_IS_NOT_RUNNING,
    JOIN_FAILURE_CANNOT_CREATE, JOIN_FAILURE_NOT_FOUND, JOIN_FAILURE_TEAM_IS_FULL,
    JOIN_FAILURE_TIME_OUT, JOIN_FAILURE_WRONG_ARGS, JOIN_FAILURE_WRONG_VEHICLE,
    QUEUE_TYPE_COMPANIES, QUEUE_TYPE_RANDOMS, QUEUE_TYPE_VOLUNTEERS, VEHICLE_CLASS_TAGS,
};

pub const ARENA_GUI_TYPE_UNKNOWN: u8 = 0;
pub const ARENA_GUI_TYPE_RANDOM: u8 = 1;
pub const ARENA_GUI_TYPE_TRAINING: u8 = 2;
pub const ARENA_GUI_TYPE_COMPANY: u8 = 3;

pub const ARENA_BONUS_TYPE_UNKNOWN: u8 = 0;
pub const ARENA_BONUS_TYPE_REGULAR: u8 = 1;
pub const ARENA_BONUS_TYPE_TRAINING: u8 = 2;

/// High-level game mode for a spawned arena.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum BattleMode {
    Random,
    Training,
}

/// One vehicle/account participant scheduled into a newly spawned arena.
#[derive(Debug, Clone, PartialEq)]
pub struct ParticipantSpec {
    pub account_db_id: i64,
    pub player_name: String,
    pub veh_inv_id: i64,
    pub veh_comp_descr: Vec<u8>,
    pub team: u8,
    pub prebattle_id: i32,
    pub is_bot: bool,
    pub shells: Vec<(i32, u32)>,
    pub eqs: Vec<u16>,
    pub crew: Vec<wot065_data::descr::TankmanDescr>,
}

impl Default for ParticipantSpec {
    fn default() -> Self {
        Self {
            account_db_id: 0,
            player_name: String::new(),
            veh_inv_id: 0,
            veh_comp_descr: Vec::new(),
            team: 1,
            prebattle_id: 0,
            is_bot: false,
            shells: Vec::new(),
            eqs: Vec::new(),
            crew: Vec::new(),
        }
    }
}

/// Complete match launch description handed off from Lobby (`queue`/`prebattle`) to `BattleWorld`.
#[derive(Debug, Clone, PartialEq)]
pub struct ArenaLaunchSpec {
    pub arena_type_id: i32,
    pub arena_gui_type: u8,
    pub arena_bonus_type: u8,
    pub mode: BattleMode,
    pub round_length_secs: u32,
    pub prebattle_id: i32,
    pub participants: Vec<ParticipantSpec>,
}
