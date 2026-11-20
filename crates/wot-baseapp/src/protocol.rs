//! WoT 0.6.5 BaseApp session state machine and Account wire codecs.

use std::time::{Duration, Instant};

use bw201::bundle::{decode_messages, DecodedMessage, ElementEncoder};
use bw201::crypto::{BlowfishError, BlowfishKey};
use bw201::interfaces::{
    avatar_update_id_style, baseapp_ext_ids, baseapp_ext_interface, client_ids, InterfaceTable,
    LengthStyle,
};
use bw201::login::LoginSession;
use bw201::packet::{
    Endian, FilterError, Packet, PacketError, FLAG_HAS_PIGGYBACKS, FLAG_IS_FRAGMENT,
    FLAG_IS_RELIABLE, FLAG_ON_CHANNEL,
};
use bw201::stream::{Address, BinaryReader, BinaryWriter, StreamError};
use bw201::world::{
    CreateCellPlayer, CreateEntity, DetailedPosition, DirKind, Direction3D, EnterAoI, EntityDir,
    IdKind, PackedXYZ, PosKind, ResetEntities, S2CAvatarUpdate, SpaceData, YawPitchRoll, NO_ID_ALIAS,
    SPACE_DATA_WEATHER,
};
use glam::Vec3;

use crate::channel::ChannelState;
use crate::payloads::{AccountPayloads, RESOURCE_CHUNK_SIZE};
use crate::registry::AccountRegistry;
use wot065_data::account::{build_showgui_ctx_ext, stream_bundle};
use wot065_data::account_state::build_full_sync_from_profile;
use wot065_data::dossier::build_dossier_sync_payload;
use wot065_data::pickle::{self, Value};
use wot_battle::world::VehicleState;

/// Client entity type IDs: `scripts/entities.xml` order counting only the types with a
/// client script (`Account`, `Avatar`, `Vehicle`, `AreaDestructibles`, `OfflineEntity`,
/// `Flock`). Server-only types (`Admin`, `Arena`, `ArenaMgr`, `Projectile`, ...) take no index.
/// `updateFrequencyNotification`: the client times `TICK_SYNC` bytes and `SET_GAME_TIME`
/// game ticks at this rate.
pub(crate) const UPDATE_FREQUENCY_HZ: u8 = 10;

pub const ACCOUNT_ENTITY_TYPE_ID: u16 = 0;
pub const AVATAR_ENTITY_TYPE_ID: u16 = 1;
pub const VEHICLE_ENTITY_TYPE_ID: u16 = 2;
pub const AREA_DESTRUCTIBLES_ENTITY_TYPE_ID: u16 = 3;

/// Account / Avatar exposed BaseMethod IDs (`0xC0 | exposedIndex`).
pub mod base_method {
    // Account (and shared Chat interface 0..=2)
    pub const CHAT_COMMAND: u8 = 0xC0;
    pub const INVITE_COMMAND: u8 = 0xC1;
    pub const ACK_COMMAND: u8 = 0xC2;
    pub const DO_CMD_STR: u8 = 0xC3;
    pub const DO_CMD_INT3: u8 = 0xC4;
    pub const DO_CMD_INT4: u8 = 0xC5;
    pub const DO_CMD_INT2_STR: u8 = 0xC6;
    pub const DO_CMD_INT_ARR: u8 = 0xC7;
    pub const MAKE_TRADE_OFFER_BY_CLIENT: u8 = 0xC8;
    pub const CREATE_TRAINING: u8 = 0xC9;
    pub const CREATE_SQUAD: u8 = 0xCA;
    pub const CREATE_COMPANY: u8 = 0xCB;

    // Avatar
    pub const AVATAR_VEHICLE_MOVE_WITH: u8 = 0xC3;
    pub const AVATAR_VEHICLE_SHOOT: u8 = 0xC4;
    pub const AVATAR_VEHICLE_TRACK_POINT_WITH_GUN: u8 = 0xC5;
    pub const AVATAR_VEHICLE_STOP_TRACKING_WITH_GUN: u8 = 0xC6;
    pub const AVATAR_VEHICLE_CHANGE_SETTING: u8 = 0xC7;
    pub const AVATAR_VEHICLE_TELEPORT: u8 = 0xC8;
    pub const AVATAR_VEHICLE_USE_HORN: u8 = 0xC9;
    pub const AVATAR_ON_CLIENT_READY: u8 = 0xCA;
    pub const AVATAR_LEAVE_ARENA: u8 = 0xCB;
    pub const AVATAR_SET_DEVELOPMENT_FEATURE: u8 = 0xCC;
    pub const AVATAR_ADD_BOT_TO_ARENA: u8 = 0xCD;
    pub const AVATAR_CONTROL_ANOTHER_VEHICLE: u8 = 0xCE;
}

/// Avatar / Vehicle exposed CellMethod indices (called with message ID `0x80 | cellIndex` + `i32 entity_id`).
pub mod cell_method {
    // Avatar Exposed CellMethods (0x80 | cellIndex)
    pub const AVATAR_AUTO_AIM: u8 = 0;
    pub const AVATAR_MOVE_TO: u8 = 1;
    pub const AVATAR_BIND_TO_VEHICLE: u8 = 2;

    // Vehicle Exposed CellMethods (0x80 | cellIndex)
    pub const VEHICLE_MOVE_WITH: u8 = 0;
    pub const VEHICLE_TRACK_POINT_WITH_GUN: u8 = 1;
    pub const VEHICLE_STOP_TRACKING_WITH_GUN: u8 = 2;
    pub const VEHICLE_CHANGE_SETTING: u8 = 3;
    pub const VEHICLE_SEND_VISIBILITY_DEVELOPMENT_INFO: u8 = 4;
}

/// Account / Avatar / Vehicle exposed ClientMethod IDs (`0x80 | exposedIndex`).
pub mod client_method {
    // Account
    pub const ON_CHAT_ACTION: u8 = 0x80;
    pub const ON_CMD_RESPONSE: u8 = 0x82;
    pub const ON_CMD_RESPONSE_EXT: u8 = 0x83;
    pub const ON_KICKED_FROM_SERVER: u8 = 0x84;
    pub const ON_ENQUEUED: u8 = 0x85;
    pub const ON_ENQUEUE_FAILURE: u8 = 0x86;
    pub const ON_DEQUEUED: u8 = 0x87;
    pub const ON_ARENA_CREATED: u8 = 0x88;
    pub const ON_ARENA_JOIN_FAILURE: u8 = 0x89;
    pub const ON_PREBATTLE_JOINED: u8 = 0x8A;
    pub const ON_PREBATTLE_JOIN_FAILURE: u8 = 0x8B;
    pub const ON_PREBATTLE_LEFT: u8 = 0x8C;
    pub const ON_KICKED_FROM_QUEUE: u8 = 0x8D;
    pub const ON_KICKED_FROM_ARENA: u8 = 0x8E;
    pub const ON_KICKED_FROM_PREBATTLE: u8 = 0x8F;
    pub const SHOW_GUI: u8 = 0x90;
    pub const RECEIVE_ACTIVE_ARENAS: u8 = 0x91;
    pub const RECEIVE_SERVER_STATS: u8 = 0x92;
    pub const RECEIVE_QUEUE_INFO: u8 = 0x93;
    pub const UPDATE_PREBATTLE: u8 = 0x94;
    pub const UPDATE: u8 = 0x95;
    pub const RESYNC_DOSSIERS: u8 = 0x96;

    // Avatar
    pub const AVATAR_ON_CHAT_ACTION: u8 = 0x80;
    pub const AVATAR_ON_KICKED_FROM_SERVER: u8 = 0x81;
    pub const AVATAR_ON_AUTO_AIM_VEHICLE_LOST: u8 = 0x82;
    pub const AVATAR_UPDATE_VEHICLE_HEALTH: u8 = 0x83;
    pub const AVATAR_UPDATE_VEHICLE_GUN_RELOAD_TIME: u8 = 0x84;
    pub const AVATAR_UPDATE_VEHICLE_AMMO: u8 = 0x85;
    pub const AVATAR_UPDATE_VEHICLE_SETTING: u8 = 0x86;
    pub const AVATAR_UPDATE_TARGETING_INFO: u8 = 0x87;
    pub const AVATAR_UPDATE_GUN_MARKER: u8 = 0x88;
    pub const AVATAR_UPDATE_OWN_VEHICLE_POSITION: u8 = 0x89;
    pub const AVATAR_SHOW_VEHICLE_DAMAGE_INFO: u8 = 0x8A;
    pub const AVATAR_SHOW_DEVELOPMENT_INFO: u8 = 0x8B;
    pub const AVATAR_SHOW_TRACER: u8 = 0x8C;
    pub const AVATAR_STOP_TRACER: u8 = 0x8D;
    pub const AVATAR_EXPLODE_PROJECTILE: u8 = 0x8E;
    pub const AVATAR_RECEIVE_HORN: u8 = 0x8F;
    pub const AVATAR_ON_KICKED_FROM_ARENA: u8 = 0x90;
    pub const AVATAR_ON_VEHICLE_LEFT_ARENA: u8 = 0x91;
    pub const AVATAR_UPDATE_ARENA: u8 = 0x92;
    pub const AVATAR_UPDATE_POSITIONS: u8 = 0x93;

    // Vehicle ClientMethods
    pub const VEHICLE_SHOW_SHOOTING: u8 = 0x80;
    pub const VEHICLE_SHOW_DAMAGE_FROM_SHOT: u8 = 0x81;
    pub const VEHICLE_SHOW_DAMAGE_FROM_EXPLOSION: u8 = 0x82;
    pub const VEHICLE_SHOW_FIRE_STARTED_MESSAGE: u8 = 0x83;
    pub const VEHICLE_ON_PUSHED: u8 = 0x84;
}

/// `common/AccountCommands.py` command and result constants.
pub mod cmd {
    pub const RES_FAILURE: i16 = -1;
    pub const RES_WRONG_ARGS: i16 = -2;
    pub const RES_NON_PLAYER: i16 = -3;
    pub const RES_SHOP_DESYNC: i16 = -4;
    pub const RES_COOLDOWN: i16 = -5;
    pub const RES_HIDDEN_DOSSIER: i16 = -6;
    pub const RES_SUCCESS: i16 = 0;
    pub const RES_STREAM: i16 = 1;
    pub const RES_CACHE: i16 = 2;

    pub const CMD_SYNC_DATA: i16 = 100;
    pub const CMD_EQUIP: i16 = 101;
    pub const CMD_EQUIP_OPTDEV: i16 = 102;
    pub const CMD_EQUIP_SHELLS: i16 = 103;
    pub const CMD_EQUIP_EQS: i16 = 104;
    pub const CMD_EQUIP_TMAN: i16 = 105;
    pub const CMD_REPAIR: i16 = 106;
    pub const CMD_VEH_SETTINGS: i16 = 107;
    pub const CMD_VEH_CAMOUFLAGE: i16 = 120;
    pub const CMD_VEH_HORN: i16 = 121;
    pub const CMD_TMAN_ADD_SKILL: i16 = 151;
    pub const CMD_TMAN_DROP_SKILL: i16 = 152;
    pub const CMD_TMAN_RESPEC: i16 = 153;
    pub const CMD_TMAN_PASSPORT: i16 = 154;
    pub const CMD_UNLOCK: i16 = 201;
    pub const CMD_EXCHANGE: i16 = 202;
    pub const CMD_FREE_XP_CONV: i16 = 203;
    pub const CMD_PREMIUM: i16 = 204;
    pub const CMD_BUY_SLOT: i16 = 205;
    pub const CMD_BUY_BERTHS: i16 = 206;
    pub const CMD_SYNC_SHOP: i16 = 300;
    pub const CMD_BUY_VEHICLE: i16 = 301;
    pub const CMD_BUY_ITEM: i16 = 302;
    pub const CMD_BUY_TMAN: i16 = 303;
    pub const CMD_SELL_VEHICLE: i16 = 304;
    pub const CMD_SELL_ITEM: i16 = 305;
    pub const CMD_DISMISS_TMAN: i16 = 306;
    pub const CMD_PRB_JOIN: i16 = 400;
    pub const CMD_PRB_LEAVE: i16 = 401;
    pub const CMD_PRB_READY: i16 = 402;
    pub const CMD_PRB_NOT_READY: i16 = 403;
    pub const CMD_PRB_ASSIGN: i16 = 404;
    pub const CMD_PRB_CH_ARENA: i16 = 405;
    pub const CMD_PRB_CH_ROUND: i16 = 406;
    pub const CMD_PRB_OPEN: i16 = 407;
    pub const CMD_PRB_CH_COMMENT: i16 = 408;
    pub const CMD_PRB_CH_ARENAVOIP: i16 = 409;
    pub const CMD_PRB_TEAM_READY: i16 = 410;
    pub const CMD_PRB_TEAM_NOT_READY: i16 = 411;
    pub const CMD_PRB_KICK: i16 = 412;
    pub const CMD_REQ_SERVER_STATS: i16 = 501;
    pub const CMD_REQ_QUEUE_INFO: i16 = 502;
    pub const CMD_REQ_PLAYER_DOSSIER: i16 = 503;
    pub const CMD_REQ_PLAYER_CLAN_INFO: i16 = 504;
    pub const CMD_REQ_PREBATTLES: i16 = 520;
    pub const CMD_REQ_PREBATTLES_BY_CREATOR: i16 = 521;
    pub const CMD_REQ_PREBATTLE_ROSTER: i16 = 522;
    pub const CMD_SYNC_DOSSIERS: i16 = 600;
    pub const CMD_ENQUEUE_FOR_ARENA: i16 = 700;
    pub const CMD_DEQUEUE: i16 = 701;
    pub const CMD_SET_LANGUAGE: i16 = 1000;
    pub const CMD_SET_MONEY: i16 = 10001;
    pub const CMD_ADD_XP: i16 = 10002;
    pub const CMD_ADD_TMAN_XP: i16 = 10003;
    pub const CMD_UNLOCK_ALL: i16 = 10004;
    pub const CMD_CREATE_ARENA: i16 = 10006;
    pub const CMD_JOIN_ARENA: i16 = 10007;
    pub const CMD_FORCE_QUEUE: i16 = 10008;

    pub const REQUEST_ID_PREBATTLES: i16 = 100;
    pub const REQUEST_ID_PREBATTLE_ROSTER: i16 = 101;
    pub const REQUEST_ID_NO_RESPONSE: i16 = 102;
    pub const REQUEST_ID_UNRESERVED_MIN: i16 = 120;
}

#[derive(Debug, Clone, PartialEq)]
pub struct DecodedDoCmd {
    pub request_id: i16,
    pub command_id: i16,
    pub args: DoCmdArgs,
}

#[derive(Debug, Clone, PartialEq)]
pub enum DoCmdArgs {
    Str(String),
    Int3([i32; 3]),
    Int4([i32; 4]),
    Int2Str(i32, i32, String),
    IntArr(Vec<i32>),
}

/// Decode an inbound `Account.doCmd*` base method payload without dropping string or array contents.
pub fn decode_do_cmd(method_id: u8, data: &[u8]) -> Result<DecodedDoCmd, StreamError> {
    let mut r = BinaryReader::new(data);
    let request_id = r.i16()?;
    let command_id = r.i16()?;
    let args = match method_id {
        base_method::DO_CMD_STR => {
            let s = r.string()?;
            DoCmdArgs::Str(s)
        }
        base_method::DO_CMD_INT3 => {
            let i1 = r.i32()?;
            let i2 = r.i32()?;
            let i3 = r.i32()?;
            DoCmdArgs::Int3([i1, i2, i3])
        }
        base_method::DO_CMD_INT4 => {
            let i1 = r.i32()?;
            let i2 = r.i32()?;
            let i3 = r.i32()?;
            let i4 = r.i32()?;
            DoCmdArgs::Int4([i1, i2, i3, i4])
        }
        base_method::DO_CMD_INT2_STR => {
            let i1 = r.i32()?;
            let i2 = r.i32()?;
            let s = r.string()?;
            DoCmdArgs::Int2Str(i1, i2, s)
        }
        base_method::DO_CMD_INT_ARR => {
            let count = r.i32()?;
            if count < 0 || (count as usize).saturating_mul(4) > r.remaining() {
                return Err(StreamError::UnexpectedEof);
            }
            let mut ints = Vec::with_capacity(count as usize);
            for _ in 0..count {
                ints.push(r.i32()?);
            }
            DoCmdArgs::IntArr(ints)
        }
        _ => return Err(StreamError::UnexpectedEof),
    };
    Ok(DecodedDoCmd {
        request_id,
        command_id,
        args,
    })
}

impl DecodedDoCmd {
    pub fn to_engine_cmd(&self) -> Result<wot065_data::ops::EngineCmd, wot065_data::ops::CmdError> {
        use wot065_data::ops::{CmdError, EngineCmd};
        match self.command_id {
            cmd::CMD_BUY_SLOT => match &self.args {
                DoCmdArgs::Int3([shop_rev, _, _]) => Ok(EngineCmd::BuySlot {
                    shop_rev: *shop_rev as i64,
                }),
                _ => Err(CmdError::wrong_args("invalid args for CMD_BUY_SLOT")),
            },
            cmd::CMD_BUY_BERTHS => match &self.args {
                DoCmdArgs::Int3([shop_rev, _, _]) => Ok(EngineCmd::BuyBerths {
                    shop_rev: *shop_rev as i64,
                }),
                _ => Err(CmdError::wrong_args("invalid args for CMD_BUY_BERTHS")),
            },
            cmd::CMD_EXCHANGE => match &self.args {
                DoCmdArgs::Int3([shop_rev, gold, _]) => Ok(EngineCmd::Exchange {
                    shop_rev: *shop_rev as i64,
                    gold: *gold as i64,
                }),
                _ => Err(CmdError::wrong_args("invalid args for CMD_EXCHANGE")),
            },
            cmd::CMD_FREE_XP_CONV => match &self.args {
                DoCmdArgs::IntArr(ints) if ints.len() >= 2 => Ok(EngineCmd::FreeXpConv {
                    shop_rev: ints[0] as i64,
                    xp: ints[1] as i64,
                    veh_cds: ints[2..].to_vec(),
                }),
                _ => Err(CmdError::wrong_args("invalid args for CMD_FREE_XP_CONV")),
            },
            cmd::CMD_PREMIUM => match &self.args {
                DoCmdArgs::Int3([shop_rev, days, _]) => Ok(EngineCmd::Premium {
                    shop_rev: *shop_rev as i64,
                    days: *days,
                }),
                _ => Err(CmdError::wrong_args("invalid args for CMD_PREMIUM")),
            },
            cmd::CMD_UNLOCK => match &self.args {
                DoCmdArgs::Int3([veh_type_cd, unlock_idx, _]) => Ok(EngineCmd::Unlock {
                    veh_type_cd: *veh_type_cd,
                    unlock_idx: *unlock_idx as usize,
                }),
                _ => Err(CmdError::wrong_args("invalid args for CMD_UNLOCK")),
            },
            cmd::CMD_BUY_VEHICLE => match &self.args {
                DoCmdArgs::Int4([shop_rev, veh_type_cd, flags, tman_cost_idx]) => {
                    Ok(EngineCmd::BuyVehicle {
                        shop_rev: *shop_rev as i64,
                        veh_type_cd: *veh_type_cd,
                        flags: *flags,
                        tman_cost_idx: *tman_cost_idx as usize,
                    })
                }
                _ => Err(CmdError::wrong_args("invalid args for CMD_BUY_VEHICLE")),
            },
            cmd::CMD_SELL_VEHICLE => match &self.args {
                DoCmdArgs::Int3([shop_rev, veh_inv_id, flags]) => Ok(EngineCmd::SellVehicle {
                    shop_rev: *shop_rev as i64,
                    veh_inv_id: *veh_inv_id as i64,
                    flags: *flags,
                }),
                _ => Err(CmdError::wrong_args("invalid args for CMD_SELL_VEHICLE")),
            },
            cmd::CMD_BUY_ITEM => match &self.args {
                DoCmdArgs::Int3([shop_rev, item_cd, count]) => Ok(EngineCmd::BuyItem {
                    shop_rev: *shop_rev as i64,
                    item_cd: *item_cd,
                    count: *count,
                }),
                _ => Err(CmdError::wrong_args("invalid args for CMD_BUY_ITEM")),
            },
            cmd::CMD_SELL_ITEM => match &self.args {
                DoCmdArgs::Int4([shop_rev, item_type_idx, item_cd, count]) => {
                    Ok(EngineCmd::SellItem {
                        shop_rev: *shop_rev as i64,
                        item_type_idx: *item_type_idx as u8,
                        item_cd: *item_cd,
                        count: *count,
                    })
                }
                _ => Err(CmdError::wrong_args("invalid args for CMD_SELL_ITEM")),
            },
            cmd::CMD_EQUIP => match &self.args {
                DoCmdArgs::Int3([veh_inv_id, item_cd, gun_cd]) => Ok(EngineCmd::Equip {
                    veh_inv_id: *veh_inv_id as i64,
                    item_cd: *item_cd,
                    gun_cd: *gun_cd,
                }),
                _ => Err(CmdError::wrong_args("invalid args for CMD_EQUIP")),
            },
            cmd::CMD_EQUIP_OPTDEV => match &self.args {
                DoCmdArgs::Int4([veh_inv_id, dev_cd, slot_idx, is_paid_removal]) => {
                    Ok(EngineCmd::EquipOptDev {
                        veh_inv_id: *veh_inv_id as i64,
                        dev_cd: *dev_cd,
                        slot_idx: *slot_idx as usize,
                        is_paid_removal: *is_paid_removal != 0,
                    })
                }
                _ => Err(CmdError::wrong_args("invalid args for CMD_EQUIP_OPTDEV")),
            },
            cmd::CMD_EQUIP_SHELLS => match &self.args {
                DoCmdArgs::IntArr(ints) if !ints.is_empty() && (ints.len() - 1) % 2 == 0 => {
                    let mut shells = Vec::new();
                    for chunk in ints[1..].chunks_exact(2) {
                        shells.push((chunk[0], chunk[1]));
                    }
                    Ok(EngineCmd::EquipShells {
                        veh_inv_id: ints[0] as i64,
                        shells,
                    })
                }
                _ => Err(CmdError::wrong_args("invalid args for CMD_EQUIP_SHELLS")),
            },
            cmd::CMD_EQUIP_EQS => match &self.args {
                DoCmdArgs::IntArr(ints) if ints.len() == 4 => Ok(EngineCmd::EquipEqs {
                    veh_inv_id: ints[0] as i64,
                    eqs: [ints[1], ints[2], ints[3]],
                }),
                _ => Err(CmdError::wrong_args("invalid args for CMD_EQUIP_EQS")),
            },
            cmd::CMD_REPAIR => match &self.args {
                DoCmdArgs::Int3([veh_inv_id, _, _]) => Ok(EngineCmd::Repair {
                    veh_inv_id: *veh_inv_id as i64,
                }),
                _ => Err(CmdError::wrong_args("invalid args for CMD_REPAIR")),
            },
            cmd::CMD_VEH_SETTINGS => match &self.args {
                DoCmdArgs::Int3([veh_inv_id, flag, is_on]) => Ok(EngineCmd::VehSettings {
                    veh_inv_id: *veh_inv_id as i64,
                    flag: *flag,
                    is_on: *is_on != 0,
                }),
                _ => Err(CmdError::wrong_args("invalid args for CMD_VEH_SETTINGS")),
            },
            cmd::CMD_VEH_CAMOUFLAGE => match &self.args {
                DoCmdArgs::Int3([shop_rev, veh_inv_id, camo_id]) => {
                    Ok(EngineCmd::VehCamouflage {
                        shop_rev: *shop_rev as i64,
                        veh_inv_id: *veh_inv_id as i64,
                        camo_id: *camo_id as u16,
                    })
                }
                _ => Err(CmdError::wrong_args("invalid args for CMD_VEH_CAMOUFLAGE")),
            },
            cmd::CMD_VEH_HORN => match &self.args {
                DoCmdArgs::Int3([shop_rev, veh_inv_id, horn_id]) => Ok(EngineCmd::VehHorn {
                    shop_rev: *shop_rev as i64,
                    veh_inv_id: *veh_inv_id as i64,
                    horn_id: *horn_id as u8,
                }),
                _ => Err(CmdError::wrong_args("invalid args for CMD_VEH_HORN")),
            },
            cmd::CMD_BUY_TMAN => match &self.args {
                DoCmdArgs::Int4([shop_rev, veh_type_cd, role_idx, tman_cost_idx]) => {
                    Ok(EngineCmd::BuyTman {
                        shop_rev: *shop_rev as i64,
                        veh_type_cd: *veh_type_cd,
                        role_idx: *role_idx as u8,
                        tman_cost_idx: *tman_cost_idx as usize,
                    })
                }
                _ => Err(CmdError::wrong_args("invalid args for CMD_BUY_TMAN")),
            },
            cmd::CMD_EQUIP_TMAN => match &self.args {
                DoCmdArgs::Int3([veh_inv_id, slot, tman_inv_id]) => Ok(EngineCmd::EquipTman {
                    veh_inv_id: *veh_inv_id as i64,
                    slot: *slot as usize,
                    tman_inv_id: *tman_inv_id as i64,
                }),
                _ => Err(CmdError::wrong_args("invalid args for CMD_EQUIP_TMAN")),
            },
            cmd::CMD_DISMISS_TMAN => match &self.args {
                DoCmdArgs::Int3([tman_inv_id, _, _]) => Ok(EngineCmd::DismissTman {
                    tman_inv_id: *tman_inv_id as i64,
                }),
                _ => Err(CmdError::wrong_args("invalid args for CMD_DISMISS_TMAN")),
            },
            cmd::CMD_TMAN_RESPEC => match &self.args {
                DoCmdArgs::Int4([shop_rev, tman_inv_id, tman_cost_idx, veh_type_cd]) => {
                    Ok(EngineCmd::TmanRespec {
                        shop_rev: *shop_rev as i64,
                        tman_inv_id: *tman_inv_id as i64,
                        tman_cost_idx: *tman_cost_idx as usize,
                        veh_type_cd: *veh_type_cd,
                    })
                }
                _ => Err(CmdError::wrong_args("invalid args for CMD_TMAN_RESPEC")),
            },
            cmd::CMD_TMAN_ADD_SKILL => match &self.args {
                DoCmdArgs::Int3([tman_inv_id, skill_idx, _]) => Ok(EngineCmd::TmanAddSkill {
                    tman_inv_id: *tman_inv_id as i64,
                    skill_idx: *skill_idx as u8,
                }),
                _ => Err(CmdError::wrong_args("invalid args for CMD_TMAN_ADD_SKILL")),
            },
            cmd::CMD_TMAN_DROP_SKILL => match &self.args {
                DoCmdArgs::Int3([tman_inv_id, skill_idx, _]) => Ok(EngineCmd::TmanDropSkill {
                    tman_inv_id: *tman_inv_id as i64,
                    skill_idx: *skill_idx as u8,
                }),
                _ => Err(CmdError::wrong_args("invalid args for CMD_TMAN_DROP_SKILL")),
            },
            cmd::CMD_TMAN_PASSPORT => match &self.args {
                DoCmdArgs::IntArr(ints) if ints.len() == 6 => Ok(EngineCmd::TmanPassport {
                    shop_rev: ints[0] as i64,
                    tman_inv_id: ints[1] as i64,
                    is_female_flag: ints[2],
                    first_name_id: ints[3],
                    last_name_id: ints[4],
                    icon_id: ints[5],
                }),
                _ => Err(CmdError::wrong_args("invalid args for CMD_TMAN_PASSPORT")),
            },
            cmd::CMD_SET_MONEY => match &self.args {
                DoCmdArgs::Int3([credits, gold, _]) => Ok(EngineCmd::SetMoney {
                    credits: *credits as i64,
                    gold: *gold as i64,
                }),
                _ => Err(CmdError::wrong_args("invalid args for CMD_SET_MONEY")),
            },
            cmd::CMD_ADD_XP => match &self.args {
                DoCmdArgs::Int3([veh_type_cd, xp, _]) => Ok(EngineCmd::AddXp {
                    veh_type_cd: *veh_type_cd,
                    xp: *xp,
                }),
                _ => Err(CmdError::wrong_args("invalid args for CMD_ADD_XP")),
            },
            cmd::CMD_ADD_TMAN_XP => match &self.args {
                DoCmdArgs::Int3([tman_inv_id, xp, _]) => Ok(EngineCmd::AddTmanXp {
                    tman_inv_id: *tman_inv_id as i64,
                    xp: *xp,
                }),
                _ => Err(CmdError::wrong_args("invalid args for CMD_ADD_TMAN_XP")),
            },
            cmd::CMD_UNLOCK_ALL => Ok(EngineCmd::UnlockAll),
            cmd::CMD_SET_LANGUAGE => match &self.args {
                DoCmdArgs::Str(lang) => Ok(EngineCmd::SetLanguage { lang: lang.clone() }),
                _ => Err(CmdError::wrong_args("invalid args for CMD_SET_LANGUAGE")),
            },
            other => Err(CmdError::failure(format!("unsupported command ID {other}"))),
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ProtocolError {
    #[error("blowfish decrypt failed: {0}")]
    Decrypt(#[from] FilterError),
    #[error("mercury packet decode failed: {0}")]
    Packet(#[from] PacketError),
    #[error("bundle decode failed: {0}")]
    Bundle(#[from] bw201::bundle::BundleError),
    #[error("stream parse failed: {0}")]
    Stream(#[from] StreamError),
    #[error("session key mismatch: got 0x{got:08X}, expected 0x{expected:08X}")]
    SessionKeyMismatch { got: u32, expected: u32 },
}

/// Stateful handler for a connected player's BaseApp Mercury session.
pub struct BaseAppSession {
    login: LoginSession,
    blowfish: BlowfishKey,
    channel: ChannelState,
    ext_table: InterfaceTable,
    payloads: AccountPayloads,
    player_id: i32,
    account_rev: i64,
    logged_in: bool,
    entities_enabled: bool,
    disconnected: bool,
}

impl BaseAppSession {
    pub fn new(login: LoginSession, payloads: AccountPayloads) -> Result<Self, BlowfishError> {
        let blowfish = BlowfishKey::new(&login.encryption_key())?;
        Ok(Self {
            login,
            blowfish,
            channel: ChannelState::new(),
            ext_table: baseapp_ext_interface(),
            payloads,
            player_id: 1,
            account_rev: 1,
            logged_in: false,
            entities_enabled: false,
            disconnected: false,
        })
    }

    pub fn login(&self) -> &LoginSession {
        &self.login
    }

    pub fn blowfish(&self) -> &BlowfishKey {
        &self.blowfish
    }

    pub fn channel(&self) -> &ChannelState {
        &self.channel
    }

    pub fn channel_mut(&mut self) -> &mut ChannelState {
        &mut self.channel
    }

    pub fn is_entities_enabled(&self) -> bool {
        self.entities_enabled
    }

    pub fn set_entities_enabled(&mut self, enabled: bool) {
        self.entities_enabled = enabled;
    }

    pub fn is_disconnected(&self) -> bool {
        self.disconnected
    }

    pub fn mark_disconnected(&mut self) {
        self.disconnected = true;
    }

    pub fn player_id(&self) -> i32 {
        self.player_id
    }

    pub fn set_player_id(&mut self, player_id: i32) {
        self.player_id = player_id;
    }

    pub fn is_logged_in(&self) -> bool {
        self.logged_in
    }

    pub fn account_rev(&self) -> i64 {
        self.account_rev
    }

    pub fn set_account_rev(&mut self, rev: i64) {
        self.account_rev = rev;
    }

    pub fn payloads(&self) -> &AccountPayloads {
        &self.payloads
    }

    pub fn send_reliable_encoder(&mut self, encoder: &ElementEncoder, now: Instant) -> Vec<Vec<u8>> {
        if encoder.body().is_empty() {
            return Vec::new();
        }
        let first_req = encoder.config().first_request_offset;
        self.channel
            .send_reliable(encoder.body(), first_req, &self.blowfish, now)
    }

    pub fn send_unreliable_encoder(&mut self, encoder: &ElementEncoder) -> Option<Vec<u8>> {
        if encoder.body().is_empty() {
            return None;
        }
        Some(self.channel.send_unreliable(&self.blowfish, encoder.body()))
    }

    pub fn send_unreliable_encoders(&mut self, encoders: &[ElementEncoder]) -> Vec<Vec<u8>> {
        let chunks: Vec<&[u8]> = encoders
            .iter()
            .map(ElementEncoder::body)
            .filter(|b| !b.is_empty())
            .collect();
        self.channel.send_unreliable_batch(&self.blowfish, &chunks)
    }

    /// Decrypt/decode an inbound UDP datagram, allowing BW 2.0.1's unencrypted
    /// off-channel `baseAppLogin` request (`RetryingRequest::send`) before
    /// entities are enabled while requiring Blowfish packet encryption on all
    /// channel traffic.
    fn decode_inbound_datagram(&self, datagram: &[u8]) -> Result<Packet, ProtocolError> {
        match self.blowfish.packet_decrypt(datagram) {
            Ok(plain) => Ok(Packet::decode(&plain, Endian::Little)?),
            Err(decrypt_err) => {
                if !self.entities_enabled {
                    if let Some(pkt) = self.try_decode_plaintext_base_app_login(datagram) {
                        return Ok(pkt);
                    }
                }
                Err(ProtocolError::Decrypt(decrypt_err))
            }
        }
    }

    fn try_decode_plaintext_base_app_login(&self, datagram: &[u8]) -> Option<Packet> {
        let packet = Packet::decode(datagram, Endian::Little).ok()?;
        let disallowed_flags =
            FLAG_ON_CHANNEL | FLAG_IS_RELIABLE | FLAG_IS_FRAGMENT | FLAG_HAS_PIGGYBACKS;
        if (packet.flags & disallowed_flags) != 0 || packet.config.sequence.is_some() {
            return None;
        }
        let msgs = decode_messages(&packet, &self.ext_table, Endian::Little).ok()?;
        if msgs.is_empty()
            || !msgs
                .iter()
                .all(|m| m.id == baseapp_ext_ids::BASE_APP_LOGIN && m.request.is_some())
        {
            return None;
        }
        Some(packet)
    }

    /// Decrypt an inbound UDP datagram, advance channel ordering, and decode all reliable/ordered
    /// messages without mutating session entity/login state. Returns `(messages, ack_needed)`.
    pub(crate) fn decode_datagram_messages(
        &mut self,
        datagram: &[u8],
    ) -> Result<(Vec<DecodedMessage>, bool), ProtocolError> {
        let packet = self.decode_inbound_datagram(datagram)?;
        let rx = self.channel.receive(packet);
        let mut messages = Vec::new();
        for ordered_pkt in rx.packets {
            messages.extend(decode_messages(&ordered_pkt, &self.ext_table, Endian::Little)?);
        }
        Ok((messages, rx.ack_needed))
    }

    /// Validate any `BASE_APP_LOGIN` messages in `msgs` against `self.login.session_key` without
    /// mutating session state.
    pub(crate) fn validate_login_messages(
        &self,
        msgs: &[DecodedMessage],
    ) -> Result<(), ProtocolError> {
        for msg in msgs {
            if msg.id == baseapp_ext_ids::BASE_APP_LOGIN {
                let mut r = BinaryReader::new(&msg.data);
                let client_session_key = r.u32()?;
                if client_session_key != self.login.session_key {
                    return Err(ProtocolError::SessionKeyMismatch {
                        got: client_session_key,
                        expected: self.login.session_key,
                    });
                }
            }
        }
        Ok(())
    }

    /// Encode outbound reliable packets from `encoder` (or a standalone ACK if `ack_needed`).
    pub(crate) fn encode_datagram_replies(
        &mut self,
        encoder: &ElementEncoder,
        ack_needed: bool,
        now: Instant,
    ) -> Vec<Vec<u8>> {
        if !encoder.body().is_empty() {
            self.send_reliable_encoder(encoder, now)
        } else if ack_needed {
            vec![self.channel.send_ack_only(&self.blowfish)]
        } else {
            Vec::new()
        }
    }

    /// Decrypt and process an incoming UDP datagram with a custom per-message handler,
    /// returning outbound encrypted UDP datagrams.
    pub fn process_datagram<F>(
        &mut self,
        datagram: &[u8],
        now: Instant,
        mut handler: F,
    ) -> Result<Vec<Vec<u8>>, ProtocolError>
    where
        F: FnMut(&mut Self, &DecodedMessage, &mut ElementEncoder) -> Result<(), ProtocolError>,
    {
        let (messages, ack_needed) = self.decode_datagram_messages(datagram)?;
        let mut encoder = ElementEncoder::new();
        for msg in &messages {
            handler(self, msg, &mut encoder)?;
        }
        Ok(self.encode_datagram_replies(&encoder, ack_needed, now))
    }

    /// Decrypt and process an incoming UDP datagram, returning outbound encrypted UDP datagrams.
    pub fn handle_datagram(
        &mut self,
        datagram: &[u8],
        now: Instant,
        registry: &mut AccountRegistry,
    ) -> Result<Vec<Vec<u8>>, ProtocolError> {
        self.handle_datagram_with_online_players(datagram, now, registry, 1)
    }

    /// Decrypt and process an incoming UDP datagram with the live online player count.
    pub fn handle_datagram_with_online_players(
        &mut self,
        datagram: &[u8],
        now: Instant,
        registry: &mut AccountRegistry,
        online_players: u32,
    ) -> Result<Vec<Vec<u8>>, ProtocolError> {
        self.process_datagram(datagram, now, |sess, msg, encoder| {
            sess.dispatch_message(msg, encoder, registry, online_players.max(1))
        })
    }

    /// Collect any unacknowledged reliable packets whose RTO has expired.
    pub fn due_retransmits(&mut self, now: Instant, rto: Duration) -> Vec<Vec<u8>> {
        self.channel.due_retransmits(&self.blowfish, now, rto)
    }

    pub(crate) fn handle_login_or_auth_msg(
        &mut self,
        msg: &DecodedMessage,
        encoder: &mut ElementEncoder,
    ) -> Result<bool, ProtocolError> {
        match msg.id {
            baseapp_ext_ids::BASE_APP_LOGIN => {
                let mut r = BinaryReader::new(&msg.data);
                let client_session_key = r.u32()?;
                let attempt = if r.remaining() >= 4 { r.i32()? } else { 0 };
                if client_session_key != self.login.session_key {
                    return Err(ProtocolError::SessionKeyMismatch {
                        got: client_session_key,
                        expected: self.login.session_key,
                    });
                }
                tracing::info!(
                    username = %self.login.username,
                    session_key = client_session_key,
                    attempt,
                    "baseAppLogin accepted"
                );
                let key_bytes = self.login.session_key.to_le_bytes();
                if let Some(req) = msg.request {
                    encoder.reply(req.reply_id, &key_bytes);
                }
                if !self.logged_in {
                    self.logged_in = true;
                }
                encoder.fixed_message(client_ids::AUTHENTICATE, 4, &key_bytes);
                Ok(true)
            }
            baseapp_ext_ids::AUTHENTICATE => {
                let mut r = BinaryReader::new(&msg.data);
                let client_session_key = r.u32()?;
                if client_session_key != self.login.session_key {
                    tracing::warn!(
                        got = client_session_key,
                        expected = self.login.session_key,
                        "authenticate message had unexpected session key"
                    );
                }
                Ok(true)
            }
            _ => Ok(false),
        }
    }

    fn dispatch_message(
        &mut self,
        msg: &DecodedMessage,
        encoder: &mut ElementEncoder,
        registry: &mut AccountRegistry,
        online_players: u32,
    ) -> Result<(), ProtocolError> {
        if self.handle_login_or_auth_msg(msg, encoder)? {
            return Ok(());
        }
        match msg.id {
            baseapp_ext_ids::ENABLE_ENTITIES => {
                if !self.entities_enabled {
                    self.entities_enabled = true;
                    let profile = registry
                        .get_or_create_profile(&self.login.username)
                        .map_err(|_| StreamError::UnexpectedEof)?;
                    self.account_rev = profile.rev;
                    let db_id = profile.database_id;
                    tracing::info!(
                        username = %self.login.username,
                        player_id = self.player_id,
                        database_id = db_id,
                        vehicles = self.payloads.vehicle_count,
                        "enableEntities -> initializing connection, Account player and showGUI"
                    );
                    self.emit_hangar_entry(encoder, db_id, None, true);
                }
            }
            baseapp_ext_ids::REQUEST_ENTITY_UPDATE => {
                let mut r = BinaryReader::new(&msg.data);
                let entity_id = if r.remaining() >= 4 { Some(r.i32()?) } else { None };
                tracing::debug!(
                    entity_id,
                    len = msg.data.len(),
                    "ignoring requestEntityUpdate"
                );
            }
            baseapp_ext_ids::DISCONNECT_CLIENT => {
                tracing::info!(username = %self.login.username, "client disconnected");
                self.disconnected = true;
            }
            base_method::DO_CMD_STR
            | base_method::DO_CMD_INT3
            | base_method::DO_CMD_INT4
            | base_method::DO_CMD_INT2_STR
            | base_method::DO_CMD_INT_ARR => {
                let decoded = decode_do_cmd(msg.id, &msg.data)?;
                self.handle_decoded_cmd(decoded, encoder, registry, online_players)?;
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

    pub(crate) fn emit_connection_setup(&self, encoder: &mut ElementEncoder) {
        encoder.fixed_message(
            client_ids::BANDWIDTH_NOTIFICATION,
            4,
            &1_000_000u32.to_le_bytes(),
        );
        encoder.fixed_message(client_ids::UPDATE_FREQUENCY_NOTIFICATION, 1, &[UPDATE_FREQUENCY_HZ]);
        self.emit_set_game_time(encoder, 0);
    }

    /// Emit `SET_GAME_TIME` (`game_ticks` at 10 Hz) so client `BigWorld.serverTime()`
    /// aligns with server arena time (`game_ticks / 10.0`).
    pub(crate) fn emit_set_game_time(&self, encoder: &mut ElementEncoder, game_ticks: u32) {
        encoder.fixed_message(client_ids::SET_GAME_TIME, 4, &game_ticks.to_le_bytes());
    }

    pub(crate) fn emit_create_base_player(&self, encoder: &mut ElementEncoder) {
        let mut w = BinaryWriter::new();
        w.i32(self.player_id);
        w.u16(ACCOUNT_ENTITY_TYPE_ID);
        w.string(&self.login.username);
        w.string(&self.login.username.to_lowercase());
        w.string_bytes(&self.payloads.server_settings_pickle);
        encoder.message(
            client_ids::CREATE_BASE_PLAYER,
            LengthStyle::Variable(2),
            w.as_slice(),
        );
    }

    pub(crate) fn emit_show_gui_with_prebattle(
        &self,
        encoder: &mut ElementEncoder,
        database_id: i64,
        prebattle_id: Option<i32>,
    ) {
        let now_utc = AccountPayloads::unix_now();
        let ctx = match prebattle_id {
            Some(prb_id) if self.payloads.showgui_ctx_override.is_none() => {
                let mut val = build_showgui_ctx_ext(database_id, now_utc, now_utc);
                if let Value::Dict(ref mut entries) = val {
                    entries.push((Value::str("prebattleID"), Value::Int(prb_id as i64)));
                }
                pickle::dumps(&val)
            }
            _ => self.payloads.showgui_ctx_bytes(database_id, now_utc),
        };
        let mut w = BinaryWriter::new();
        w.i32(self.player_id);
        w.string_bytes(&ctx);
        encoder.message(
            client_method::SHOW_GUI,
            LengthStyle::Variable(2),
            w.as_slice(),
        );
    }

    pub(crate) fn emit_hangar_entry(
        &self,
        encoder: &mut ElementEncoder,
        database_id: i64,
        prebattle_id: Option<i32>,
        with_connection_setup: bool,
    ) {
        if with_connection_setup {
            self.emit_connection_setup(encoder);
        }
        self.emit_create_base_player(encoder);
        self.emit_show_gui_with_prebattle(encoder, database_id, prebattle_id);
    }

    pub fn emit_update(&self, encoder: &mut ElementEncoder, diff_pickle: &[u8]) {
        let mut w = BinaryWriter::new();
        w.i32(self.player_id);
        w.string_bytes(diff_pickle);
        encoder.message(
            client_method::UPDATE,
            LengthStyle::Variable(2),
            w.as_slice(),
        );
    }

    pub fn emit_resync_dossiers(&self, encoder: &mut ElementEncoder) {
        let mut w = BinaryWriter::new();
        w.i32(self.player_id);
        encoder.message(
            client_method::RESYNC_DOSSIERS,
            LengthStyle::Variable(2),
            w.as_slice(),
        );
    }

    pub fn emit_kicked_from_server(
        &mut self,
        encoder: &mut ElementEncoder,
        reason: &str,
        is_ban: bool,
        expiry_time: u32,
    ) {
        let mut w = BinaryWriter::new();
        w.i32(self.player_id);
        w.string(reason);
        w.u8(if is_ban { 1 } else { 0 });
        w.u32(expiry_time);
        encoder.message(
            client_method::ON_KICKED_FROM_SERVER,
            LengthStyle::Variable(2),
            w.as_slice(),
        );
        self.disconnected = true;
    }

    pub fn kick(
        &mut self,
        reason: &str,
        is_ban: bool,
        expiry_time: u32,
        now: Instant,
    ) -> Vec<Vec<u8>> {
        let mut encoder = ElementEncoder::new();
        self.emit_kicked_from_server(&mut encoder, reason, is_ban, expiry_time);
        let first_req = encoder.config().first_request_offset;
        self.channel.send_reliable(encoder.body(), first_req, &self.blowfish, now)
    }

    pub(crate) fn emit_cmd_response(
        &self,
        encoder: &mut ElementEncoder,
        request_id: i16,
        result_id: i16,
    ) {
        let mut w = BinaryWriter::new();
        w.i32(self.player_id);
        w.i16(request_id);
        w.i16(result_id);
        encoder.message(
            client_method::ON_CMD_RESPONSE,
            LengthStyle::Variable(2),
            w.as_slice(),
        );
    }

    pub(crate) fn emit_cmd_response_ext(
        &self,
        encoder: &mut ElementEncoder,
        request_id: i16,
        result_id: i16,
        ext: &[u8],
    ) {
        let mut w = BinaryWriter::new();
        w.i32(self.player_id);
        w.i16(request_id);
        w.i16(result_id);
        w.string_bytes(ext);
        encoder.message(
            client_method::ON_CMD_RESPONSE_EXT,
            LengthStyle::Variable(2),
            w.as_slice(),
        );
    }

    pub(crate) fn emit_resource_stream(
        &self,
        encoder: &mut ElementEncoder,
        rid: u16,
        compressed: &[u8],
        desc: &[u8],
    ) {
        let mut hdr = BinaryWriter::new();
        hdr.u16(rid);
        hdr.string_bytes(desc);
        encoder.message(
            client_ids::RESOURCE_HEADER,
            LengthStyle::Variable(2),
            hdr.as_slice(),
        );

        let chunks: Vec<&[u8]> = if compressed.is_empty() {
            vec![&[]]
        } else {
            compressed.chunks(RESOURCE_CHUNK_SIZE).collect()
        };
        let total = chunks.len();
        for (idx, chunk) in chunks.into_iter().enumerate() {
            let flags = if idx + 1 == total { 1u8 } else { 0u8 };
            let mut frag = BinaryWriter::new();
            frag.u16(rid);
            frag.u8((idx % 255) as u8);
            frag.u8(flags);
            frag.raw(chunk);
            encoder.message(
                client_ids::RESOURCE_FRAGMENT,
                LengthStyle::Variable(2),
                frag.as_slice(),
            );
        }
    }

    pub(crate) fn handle_decoded_cmd(
        &mut self,
        cmd: DecodedDoCmd,
        encoder: &mut ElementEncoder,
        registry: &mut AccountRegistry,
        online_players: u32,
    ) -> Result<(), ProtocolError> {
        let rid = cmd.request_id as u16;
        match cmd.command_id {
            cmd::CMD_SYNC_DATA => {
                let (rev_hi, rev_lo) = match &cmd.args {
                    DoCmdArgs::Int3([hi, lo, _]) => (*hi, *lo),
                    _ => (0, 0),
                };
                let client_rev = ((rev_hi as u32 as u64) << 32) | (rev_lo as u32 as u64);

                if self.payloads.overrides.account_diff.is_some() {
                    let (compressed, desc) = self.payloads.full_sync_stream.clone();
                    self.emit_cmd_response(encoder, cmd.request_id, cmd::RES_STREAM);
                    self.emit_resource_stream(encoder, rid, &compressed, &desc);
                    return Ok(());
                }

                let catalog = registry.catalog_arc();
                let profile = registry
                    .get_or_create_profile(&self.login.username)
                    .map_err(|_| StreamError::UnexpectedEof)?;
                let cur_rev = profile.rev;
                self.account_rev = cur_rev;

                if client_rev == 0 || client_rev != cur_rev as u64 {
                    let sync_dict = build_full_sync_from_profile(profile, &catalog);
                    let (compressed, desc) = stream_bundle(&pickle::dumps(&sync_dict));
                    self.emit_cmd_response(encoder, cmd.request_id, cmd::RES_STREAM);
                    self.emit_resource_stream(encoder, rid, &compressed, &desc);
                } else {
                    let delta = AccountPayloads::noop_delta_pickle(cur_rev);
                    self.emit_cmd_response_ext(encoder, cmd.request_id, cmd::RES_CACHE, &delta);
                }
            }
            cmd::CMD_SYNC_SHOP => {
                if self.payloads.overrides.shop_cache.is_some() {
                    let (compressed, desc) = self.payloads.shop_cache_stream.clone();
                    self.emit_cmd_response(encoder, cmd.request_id, cmd::RES_STREAM);
                    self.emit_resource_stream(encoder, rid, &compressed, &desc);
                    return Ok(());
                }
                let client_rev = match &cmd.args {
                    DoCmdArgs::Int3([rev, _, _]) => *rev as i64,
                    _ => 0,
                };
                let shop_rev = registry.shop_rev();
                if client_rev != 0 && client_rev == shop_rev {
                    self.emit_cmd_response(encoder, cmd.request_id, cmd::RES_CACHE);
                } else {
                    let (compressed, desc) = if shop_rev == 1 {
                        self.payloads.shop_cache_stream.clone()
                    } else {
                        stream_bundle(&pickle::dumps(&wot065_data::account_state::build_shop_cache(
                            registry.catalog(),
                            registry.economics(),
                            shop_rev,
                        )))
                    };
                    self.emit_cmd_response(encoder, cmd.request_id, cmd::RES_STREAM);
                    self.emit_resource_stream(encoder, rid, &compressed, &desc);
                }
            }
            cmd::CMD_SYNC_DOSSIERS => {
                if self.payloads.overrides.dossier_sync.is_some() {
                    let (compressed, desc) = self.payloads.dossier_sync_stream.clone();
                    self.emit_cmd_response(encoder, cmd.request_id, cmd::RES_STREAM);
                    self.emit_resource_stream(encoder, rid, &compressed, &desc);
                    return Ok(());
                }

                let (version, max_change_time) = match &cmd.args {
                    DoCmdArgs::Int3([v, ct, _]) => (*v, *ct as u32),
                    _ => (0, 0),
                };

                let profile = registry
                    .get_or_create_profile(&self.login.username)
                    .map_err(|_| StreamError::UnexpectedEof)?;
                let records = wot065_data::account_state::collect_dossier_records(
                    profile,
                    version as i64,
                    max_change_time,
                );
                let payload = build_dossier_sync_payload(&records);
                let (compressed, desc) = stream_bundle(&pickle::dumps(&payload));
                self.emit_cmd_response(encoder, cmd.request_id, cmd::RES_STREAM);
                self.emit_resource_stream(encoder, rid, &compressed, &desc);
            }
            cmd::CMD_REQ_SERVER_STATS => {
                let mut w = BinaryWriter::new();
                w.i32(self.player_id);
                w.u32(online_players);
                w.u32(0);
                encoder.message(
                    client_method::RECEIVE_SERVER_STATS,
                    LengthStyle::Variable(2),
                    w.as_slice(),
                );
                if cmd.request_id >= cmd::REQUEST_ID_UNRESERVED_MIN {
                    self.emit_cmd_response(encoder, cmd.request_id, cmd::RES_SUCCESS);
                }
            }
            cmd::CMD_REQ_PLAYER_DOSSIER => {
                let (dossier_type, owner_id, name) = match &cmd.args {
                    DoCmdArgs::Int2Str(dt, oid, name) => (*dt, *oid, name.as_str()),
                    _ => (0, 0, ""),
                };
                match registry.get_player_dossier(dossier_type, owner_id, name) {
                    Ok(pickled_bytes) => {
                        let (compressed, desc) = stream_bundle(&pickled_bytes);
                        self.emit_cmd_response(encoder, cmd.request_id, cmd::RES_STREAM);
                        self.emit_resource_stream(encoder, rid, &compressed, &desc);
                    }
                    Err(_) => {
                        self.emit_cmd_response(encoder, cmd.request_id, cmd::RES_FAILURE);
                    }
                }
            }
            cmd::CMD_REQ_PLAYER_CLAN_INFO => {
                let (arg0, arg1, name) = match &cmd.args {
                    DoCmdArgs::Int2Str(a, b, name) => (*a, *b, name.as_str()),
                    _ => (0, 0, ""),
                };
                let exists = if !name.trim().is_empty() {
                    registry.has_account(name)
                } else {
                    (arg0 > 0 && registry.find_profile_by_db_id(arg0 as i64).is_some())
                        || (arg1 > 0 && registry.find_profile_by_db_id(arg1 as i64).is_some())
                };
                if exists {
                    let clan_tuple = Value::tuple(vec![Value::Int(0), Value::None]);
                    let (compressed, desc) = stream_bundle(&pickle::dumps(&clan_tuple));
                    self.emit_cmd_response(encoder, cmd.request_id, cmd::RES_STREAM);
                    self.emit_resource_stream(encoder, rid, &compressed, &desc);
                } else {
                    self.emit_cmd_response(encoder, cmd.request_id, cmd::RES_FAILURE);
                }
            }
            _ => {
                match cmd.to_engine_cmd() {
                    Ok(engine_cmd) => {
                        let now_utc = AccountPayloads::unix_now() as u32;
                        match registry.apply_command(&self.login.username, &engine_cmd, now_utc) {
                            Ok(outcome) => {
                                self.account_rev = outcome.rev;
                                let diff_bytes = pickle::dumps(&outcome.diff);
                                self.emit_update(encoder, &diff_bytes);
                                if outcome.resync_dossiers {
                                    self.emit_resync_dossiers(encoder);
                                }
                                if cmd.request_id != cmd::REQUEST_ID_NO_RESPONSE {
                                    if let Some(ext) = &outcome.ext {
                                        self.emit_cmd_response_ext(
                                            encoder,
                                            cmd.request_id,
                                            outcome.result_id as i16,
                                            &pickle::dumps(ext),
                                        );
                                    } else {
                                        self.emit_cmd_response(
                                            encoder,
                                            cmd.request_id,
                                            outcome.result_id as i16,
                                        );
                                    }
                                }
                            }
                            Err(err) => {
                                tracing::warn!(
                                    username = %self.login.username,
                                    command_id = cmd.command_id,
                                    error = %err,
                                    "engine command failed"
                                );
                                if cmd.request_id != cmd::REQUEST_ID_NO_RESPONSE {
                                    if let Some(ext) = &err.ext {
                                        self.emit_cmd_response_ext(
                                            encoder,
                                            cmd.request_id,
                                            err.result_id as i16,
                                            &pickle::dumps(ext),
                                        );
                                    } else {
                                        self.emit_cmd_response(
                                            encoder,
                                            cmd.request_id,
                                            err.result_id as i16,
                                        );
                                    }
                                }
                            }
                        }
                    }
                    Err(err) => {
                        tracing::debug!(
                            request_id = cmd.request_id,
                            command_id = cmd.command_id,
                            error = %err,
                            "unsupported or invalid Account command"
                        );
                        if cmd.request_id >= cmd::REQUEST_ID_UNRESERVED_MIN {
                            self.emit_cmd_response(encoder, cmd.request_id, cmd::RES_FAILURE);
                        }
                    }
                }
            }
        }
        Ok(())
    }
}

pub(crate) fn emit_reset_entities(encoder: &mut ElementEncoder, keep_player_on_base: bool) {
    let msg = ResetEntities {
        keep_player_on_base: if keep_player_on_base { 1 } else { 0 },
    };
    let mut body = Vec::with_capacity(1);
    msg.write_to(&mut body);
    encoder.fixed_message(client_ids::RESET_ENTITIES, 1, &body);
}

pub(crate) fn emit_entity_void_method(encoder: &mut ElementEncoder, entity_id: i32, method_id: u8) {
    let mut w = BinaryWriter::new();
    w.i32(entity_id);
    encoder.message(method_id, LengthStyle::Variable(2), w.as_slice());
}

pub(crate) fn emit_entity_u8_method(
    encoder: &mut ElementEncoder,
    entity_id: i32,
    method_id: u8,
    arg: u8,
) {
    let mut w = BinaryWriter::new();
    w.i32(entity_id);
    w.u8(arg);
    encoder.message(method_id, LengthStyle::Variable(2), w.as_slice());
}

pub(crate) fn emit_entity_i32_method(
    encoder: &mut ElementEncoder,
    entity_id: i32,
    method_id: u8,
    arg: i32,
) {
    let mut w = BinaryWriter::new();
    w.i32(entity_id);
    w.i32(arg);
    encoder.message(method_id, LengthStyle::Variable(2), w.as_slice());
}

pub(crate) fn emit_entity_u8_bytes_method(
    encoder: &mut ElementEncoder,
    entity_id: i32,
    method_id: u8,
    tag: u8,
    payload: &[u8],
) {
    let mut w = BinaryWriter::new();
    w.i32(entity_id);
    w.u8(tag);
    w.string_bytes(payload);
    encoder.message(method_id, LengthStyle::Variable(2), w.as_slice());
}

pub(crate) fn emit_receive_queue_info(
    encoder: &mut ElementEncoder,
    entity_id: i32,
    randoms_wire: &[u8],
    companies_wire: &[u8],
) {
    let mut w = BinaryWriter::new();
    w.i32(entity_id);
    w.raw(randoms_wire);
    w.raw(companies_wire);
    encoder.message(
        client_method::RECEIVE_QUEUE_INFO,
        LengthStyle::Variable(2),
        w.as_slice(),
    );
}

#[derive(Debug, Clone)]
pub(crate) struct ChatAction {
    pub request_id: i64,
    pub action: u8,
    pub channel: i32,
    pub originator: i64,
    pub nickname: String,
    pub data_pickle: Vec<u8>,
}

impl ChatAction {
    pub fn reply(request_id: i64, action: u8, data_pickle: Vec<u8>) -> Self {
        Self {
            request_id,
            action,
            channel: 0,
            originator: 0,
            nickname: String::new(),
            data_pickle,
        }
    }
}

pub(crate) fn emit_chat_action(
    encoder: &mut ElementEncoder,
    entity_id: i32,
    action: &ChatAction,
) {
    let timestamp = AccountPayloads::unix_now() as f64;
    let mut w = BinaryWriter::new();
    w.i32(entity_id);
    w.i64(action.request_id);
    w.u8(action.action);
    w.u8(0);
    w.f64(timestamp);
    w.f64(timestamp);
    w.i32(action.channel);
    w.i64(action.originator);
    w.string(&action.nickname);
    w.u8(0);
    w.string_bytes(&action.data_pickle);
    w.u8(0);
    encoder.message(
        client_method::ON_CHAT_ACTION,
        LengthStyle::Variable(2),
        w.as_slice(),
    );
}

pub(crate) fn emit_create_avatar_base_player(
    encoder: &mut ElementEncoder,
    avatar_id: i32,
    player_name: &str,
    arena_type_id: i32,
    arena_gui_type: u8,
    weather_preset_id: u8,
) {
    let empty_dict = pickle::dumps(&Value::dict(Vec::new()));
    let mut w = BinaryWriter::new();
    w.i32(avatar_id);
    w.u16(AVATAR_ENTITY_TYPE_ID);
    w.string(player_name);
    w.i32(arena_type_id);
    w.u8(arena_gui_type);
    w.string_bytes(&empty_dict);
    w.u8(weather_preset_id);
    encoder.message(
        client_ids::CREATE_BASE_PLAYER,
        LengthStyle::Variable(2),
        w.as_slice(),
    );
}

/// Send the arena geometry mapping and weather. No time-of-day entry: the 0.6.5 client
/// crashed on it right after `PlayerAvatar.onBecomePlayer` and lights the map itself.
pub(crate) fn emit_arena_space_data(encoder: &mut ElementEncoder, space_id: i32, geometry: &str) {
    let weather = SpaceData {
        space_id,
        entry_id: Address::new([0, 0, 0, 0], 0, 2),
        key: SPACE_DATA_WEATHER,
        payload: b"Clear".to_vec(),
    };
    let mapping = SpaceData::mapping(
        space_id,
        Address::new([0, 0, 0, 0], 0, 3),
        [
            1.0, 0.0, 0.0, 0.0, //
            0.0, 1.0, 0.0, 0.0, //
            0.0, 0.0, 1.0, 0.0, //
            0.0, 0.0, 0.0, 1.0,
        ],
        geometry,
    );

    for sd in [mapping, weather] {
        let mut body = Vec::new();
        sd.write_to(&mut body);
        encoder.message(client_ids::SPACE_DATA, LengthStyle::Variable(2), &body);
    }
}

pub(crate) fn emit_create_avatar_cell_player(
    encoder: &mut ElementEncoder,
    space_id: i32,
    pos: [f32; 3],
    yaw: f32,
    team: u8,
    player_vehicle_id: i32,
) {
    let mut cell_data = Vec::with_capacity(5);
    cell_data.push(team);
    cell_data.extend_from_slice(&player_vehicle_id.to_le_bytes());
    let ccp = CreateCellPlayer {
        space_id,
        vehicle_id: 0,
        pos,
        dir: Direction3D::new(0.0, 0.0, yaw),
        cell_data,
    };
    let mut body = Vec::new();
    ccp.write_to(&mut body);
    encoder.message(
        client_ids::CREATE_CELL_PLAYER,
        LengthStyle::Variable(2),
        &body,
    );
}

pub(crate) fn emit_enter_aoi(encoder: &mut ElementEncoder, entity_id: i32) {
    let msg = EnterAoI::new(entity_id, NO_ID_ALIAS);
    let mut body = Vec::with_capacity(5);
    msg.write_to(&mut body);
    encoder.fixed_message(client_ids::ENTER_AOI, 5, &body);
}

pub(crate) fn emit_create_vehicle_entity(encoder: &mut ElementEncoder, v: &VehicleState) {
    let mut props = BinaryWriter::new();
    props.u8(6u8);
    // 0: publicInfo (PUBLIC_VEHICLE_INFO: name, compDescr, team, prebattleID)
    props.u8(0u8);
    props.string(&v.player_name);
    props.string_bytes(&v.veh_comp_descr);
    props.u8(v.team);
    props.i32(v.prebattle_id);
    // 1: health (INT16)
    props.u8(1u8);
    props.i16(v.health);
    // 2: isCrewActive (BOOL)
    props.u8(2u8);
    props.u8(if v.is_crew_active { 1 } else { 0 });
    // 3: engineMode (TUPLE<UINT8, size=2> -> 2 inline bytes)
    props.u8(3u8);
    props.raw(&v.engine_mode);
    // 4: damageStickers (ARRAY<UINT64>)
    props.u8(4u8);
    props.i32(v.damage_stickers.len() as i32);
    for &sticker in &v.damage_stickers {
        props.u64(sticker);
    }
    // 5: publicStateModifiers (ARRAY<EXTRA_ID>)
    props.u8(5u8);
    if v.is_burning {
        props.i32(1);
        props.u8(wot_battle::sim::EXTRA_FIRE);
    } else {
        props.i32(0);
    }

    let ce = CreateEntity {
        compression: 0,
        id: v.entity_id,
        entity_type: VEHICLE_ENTITY_TYPE_ID,
        pos: [v.position.x, v.position.y, v.position.z],
        dir: EntityDir::encode([v.yaw, v.pitch, v.roll]),
        properties: props.as_slice().to_vec(),
    };
    let mut body = Vec::new();
    ce.write_to(&mut body);
    encoder.message(client_ids::CREATE_ENTITY, LengthStyle::Variable(2), &body);
}

pub(crate) fn emit_relative_position(encoder: &mut ElementEncoder, reference: Vec3) {
    let mut writer = BinaryWriter::new();
    for component in reference.to_array() {
        writer.f32(component);
    }
    encoder.fixed_message(client_ids::RELATIVE_POSITION, 12, writer.as_slice());
}

pub(crate) fn emit_s2c_avatar_update(
    encoder: &mut ElementEncoder,
    vehicle: &VehicleState,
    reference: Vec3,
) {
    let offset = vehicle.position - reference;
    // PackedXYZ's horizontal components saturate at +/-509 meters.
    if offset.x.abs() > 509.0 || offset.z.abs() > 509.0 {
        let pose = DetailedPosition {
            id: vehicle.entity_id,
            position: vehicle.position.to_array(),
            direction: Direction3D::new(vehicle.roll, vehicle.pitch, vehicle.yaw),
        };
        let mut body = Vec::with_capacity(DetailedPosition::WIRE_SIZE);
        pose.write_to(&mut body);
        encoder.fixed_message(
            client_ids::DETAILED_POSITION,
            DetailedPosition::WIRE_SIZE as u16,
            &body,
        );
        return;
    }

    let upd = S2CAvatarUpdate {
        id: IdKind::NoAlias(vehicle.entity_id),
        pos: PosKind::FullPos(PackedXYZ::encode(offset.to_array())),
        dir: DirKind::YawPitchRoll(YawPitchRoll::encode([vehicle.yaw, vehicle.pitch, vehicle.roll])),
    };
    let (msg_id, body) = upd.encode();
    let style = avatar_update_id_style(msg_id).expect("S2C avatar update has a fixed format");
    encoder.message(msg_id, style, &body);
}

pub(crate) fn emit_leave_aoi(encoder: &mut ElementEncoder, entity_id: i32) {
    let msg = bw201::world::LeaveAoI {
        id: entity_id,
        cache_stamps: Vec::new(),
    };
    let mut body = Vec::with_capacity(4);
    msg.write_to(&mut body);
    encoder.message(client_ids::LEAVE_AOI, LengthStyle::Variable(2), &body);
}

pub(crate) fn emit_update_entity(encoder: &mut ElementEncoder, entity_id: i32, properties: &[u8]) {
    let msg = bw201::world::UpdateEntity {
        id: entity_id,
        properties: properties.to_vec(),
    };
    let mut body = Vec::with_capacity(4 + properties.len());
    msg.write_to(&mut body);
    encoder.message(client_ids::UPDATE_ENTITY, LengthStyle::Variable(2), &body);
}

pub(crate) fn emit_avatar_update_targeting_info(
    encoder: &mut ElementEncoder,
    avatar_id: i32,
    turret_yaw: f32,
    gun_pitch: f32,
    max_turret_rot_speed: f32,
    max_gun_rot_speed: f32,
    shot_disp_multiplier_factor: f32,
    aiming_time: f32,
) {
    let mut w = BinaryWriter::new();
    w.i32(avatar_id);
    w.f32(turret_yaw);
    w.f32(gun_pitch);
    w.f32(max_turret_rot_speed);
    w.f32(max_gun_rot_speed);
    w.f32(shot_disp_multiplier_factor);
    w.f32(aiming_time);
    encoder.message(
        client_method::AVATAR_UPDATE_TARGETING_INFO,
        LengthStyle::Variable(2),
        w.as_slice(),
    );
}

pub(crate) fn emit_avatar_update_gun_marker(
    encoder: &mut ElementEncoder,
    avatar_id: i32,
    marker_pos: [f32; 3],
    marker_dir: [f32; 3],
    disp_angle: f32,
) {
    let mut w = BinaryWriter::new();
    w.i32(avatar_id);
    for p in marker_pos {
        w.f32(p);
    }
    for d in marker_dir {
        w.f32(d);
    }
    w.f32(disp_angle);
    encoder.message(
        client_method::AVATAR_UPDATE_GUN_MARKER,
        LengthStyle::Variable(2),
        w.as_slice(),
    );
}

pub(crate) fn emit_avatar_update_vehicle_gun_reload_time(
    encoder: &mut ElementEncoder,
    avatar_id: i32,
    time_left: f32,
) {
    let mut w = BinaryWriter::new();
    w.i32(avatar_id);
    w.f32(time_left);
    encoder.message(
        client_method::AVATAR_UPDATE_VEHICLE_GUN_RELOAD_TIME,
        LengthStyle::Variable(2),
        w.as_slice(),
    );
}

pub(crate) fn emit_avatar_update_vehicle_ammo(
    encoder: &mut ElementEncoder,
    avatar_id: i32,
    compact_descr: i32,
    quantity: u16,
    time_remaining: u16,
) {
    let mut w = BinaryWriter::new();
    w.i32(avatar_id);
    w.i32(compact_descr);
    w.u16(quantity);
    w.u16(time_remaining);
    encoder.message(
        client_method::AVATAR_UPDATE_VEHICLE_AMMO,
        LengthStyle::Variable(2),
        w.as_slice(),
    );
}

pub(crate) fn emit_avatar_update_vehicle_health(
    encoder: &mut ElementEncoder,
    avatar_id: i32,
    health: i16,
    is_crew_active: bool,
) {
    let mut w = BinaryWriter::new();
    w.i32(avatar_id);
    w.i16(health);
    w.u8(if is_crew_active { 1 } else { 0 });
    encoder.message(
        client_method::AVATAR_UPDATE_VEHICLE_HEALTH,
        LengthStyle::Variable(2),
        w.as_slice(),
    );
}

pub(crate) fn emit_avatar_update_vehicle_setting(
    encoder: &mut ElementEncoder,
    avatar_id: i32,
    code: u8,
    value: i32,
) {
    let mut w = BinaryWriter::new();
    w.i32(avatar_id);
    w.u8(code);
    w.i32(value);
    encoder.message(
        client_method::AVATAR_UPDATE_VEHICLE_SETTING,
        LengthStyle::Variable(2),
        w.as_slice(),
    );
}

pub(crate) fn emit_avatar_show_vehicle_damage_info(
    encoder: &mut ElementEncoder,
    avatar_id: i32,
    code: u8,
    extra_index: u8,
    entity_id: i32,
) {
    let mut w = BinaryWriter::new();
    w.i32(avatar_id);
    w.u8(code);
    w.u8(extra_index);
    w.i32(entity_id);
    encoder.message(
        client_method::AVATAR_SHOW_VEHICLE_DAMAGE_INFO,
        LengthStyle::Variable(2),
        w.as_slice(),
    );
}

pub(crate) fn emit_avatar_show_tracer(
    encoder: &mut ElementEncoder,
    avatar_id: i32,
    shooter_id: i32,
    shot_id: i32,
    effects_index: u8,
    ref_start_point: [f32; 3],
    velocity: [f32; 3],
    gravity: f32,
) {
    let mut w = BinaryWriter::new();
    w.i32(avatar_id);
    w.i32(shooter_id);
    w.i32(shot_id);
    w.u8(effects_index);
    for p in ref_start_point {
        w.f32(p);
    }
    for v in velocity {
        w.f32(v);
    }
    w.f32(gravity);
    encoder.message(
        client_method::AVATAR_SHOW_TRACER,
        LengthStyle::Variable(2),
        w.as_slice(),
    );
}

pub(crate) fn emit_avatar_stop_tracer(
    encoder: &mut ElementEncoder,
    avatar_id: i32,
    shot_id: i32,
    end_point: [f32; 3],
) {
    let mut w = BinaryWriter::new();
    w.i32(avatar_id);
    w.i32(shot_id);
    for p in end_point {
        w.f32(p);
    }
    encoder.message(
        client_method::AVATAR_STOP_TRACER,
        LengthStyle::Variable(2),
        w.as_slice(),
    );
}

pub(crate) fn emit_avatar_explode_projectile(
    encoder: &mut ElementEncoder,
    avatar_id: i32,
    shot_id: i32,
    effects_index: u8,
    effect_material_index: u8,
    end_point: [f32; 3],
    velocity_dir: [f32; 3],
) {
    let mut w = BinaryWriter::new();
    w.i32(avatar_id);
    w.i32(shot_id);
    w.u8(effects_index);
    w.u8(effect_material_index);
    for p in end_point {
        w.f32(p);
    }
    for v in velocity_dir {
        w.f32(v);
    }
    encoder.message(
        client_method::AVATAR_EXPLODE_PROJECTILE,
        LengthStyle::Variable(2),
        w.as_slice(),
    );
}

pub(crate) fn emit_avatar_update_positions(
    encoder: &mut ElementEncoder,
    avatar_id: i32,
    indices: &[u8],
    positions: &[i16],
) {
    let mut w = BinaryWriter::new();
    w.i32(avatar_id);
    w.i32(indices.len() as i32);
    for &idx in indices {
        w.u8(idx);
    }
    w.i32(positions.len() as i32);
    for &p in positions {
        w.i16(p);
    }
    encoder.message(
        client_method::AVATAR_UPDATE_POSITIONS,
        LengthStyle::Variable(2),
        w.as_slice(),
    );
}

pub(crate) fn emit_vehicle_show_shooting(
    encoder: &mut ElementEncoder,
    vehicle_id: i32,
) {
    let mut w = BinaryWriter::new();
    w.i32(vehicle_id);
    encoder.message(
        client_method::VEHICLE_SHOW_SHOOTING,
        LengthStyle::Variable(2),
        w.as_slice(),
    );
}

pub(crate) fn emit_tick_sync(encoder: &mut ElementEncoder, tick_byte: u8) {
    encoder.message(client_ids::TICK_SYNC, LengthStyle::Fixed(1), &[tick_byte]);
}

pub(crate) fn emit_vehicle_show_damage_from_shot(
    encoder: &mut ElementEncoder,
    vehicle_id: i32,
    attacker_id: i32,
    points: &[u64],
    effects_index: u8,
) {
    let mut w = BinaryWriter::new();
    w.i32(vehicle_id);
    w.i32(attacker_id);
    w.i32(points.len() as i32);
    for &pt in points {
        w.u64(pt);
    }
    w.u8(effects_index);
    encoder.message(
        client_method::VEHICLE_SHOW_DAMAGE_FROM_SHOT,
        LengthStyle::Variable(2),
        w.as_slice(),
    );
}

pub(crate) fn emit_vehicle_show_damage_from_explosion(
    encoder: &mut ElementEncoder,
    vehicle_id: i32,
    attacker_id: i32,
    center: [f32; 3],
    effects_index: u8,
) {
    let mut w = BinaryWriter::new();
    w.i32(vehicle_id);
    w.i32(attacker_id);
    for c in center {
        w.f32(c);
    }
    w.u8(effects_index);
    encoder.message(
        client_method::VEHICLE_SHOW_DAMAGE_FROM_EXPLOSION,
        LengthStyle::Variable(2),
        w.as_slice(),
    );
}

pub(crate) fn emit_vehicle_show_fire_started(
    encoder: &mut ElementEncoder,
    vehicle_id: i32,
    attacker_id: i32,
) {
    let mut w = BinaryWriter::new();
    w.i32(vehicle_id);
    w.i32(attacker_id);
    encoder.message(
        client_method::VEHICLE_SHOW_FIRE_STARTED_MESSAGE,
        LengthStyle::Variable(2),
        w.as_slice(),
    );
}

pub(crate) fn emit_vehicle_on_pushed(
    encoder: &mut ElementEncoder,
    vehicle_id: i32,
    x: f32,
    z: f32,
) {
    let mut w = BinaryWriter::new();
    w.i32(vehicle_id);
    w.f32(x);
    w.f32(z);
    encoder.message(
        client_method::VEHICLE_ON_PUSHED,
        LengthStyle::Variable(2),
        w.as_slice(),
    );
}

pub(crate) fn emit_create_area_destructibles_entity(
    encoder: &mut ElementEncoder,
    entity_id: i32,
    pos: [f32; 3],
    destroyed_modules: &[u16],
    destroyed_fragiles: &[u8],
    fallen_destructibles: &[u16],
) {
    let mut props = BinaryWriter::new();
    props.u8(3u8); // 3 properties
    // 0: destroyedModules (ARRAY<UINT16>)
    props.u8(0u8);
    props.i32(destroyed_modules.len() as i32);
    for &m in destroyed_modules {
        props.u16(m);
    }
    // 1: destroyedFragiles (ARRAY<UINT8>)
    props.u8(1u8);
    props.i32(destroyed_fragiles.len() as i32);
    for &f in destroyed_fragiles {
        props.u8(f);
    }
    // 2: fallenDestructibles (ARRAY<UINT16>)
    props.u8(2u8);
    props.i32(fallen_destructibles.len() as i32);
    for &f in fallen_destructibles {
        props.u16(f);
    }

    let ce = CreateEntity {
        compression: 0,
        id: entity_id,
        entity_type: AREA_DESTRUCTIBLES_ENTITY_TYPE_ID,
        pos,
        dir: EntityDir::encode([0.0, 0.0, 0.0]),
        properties: props.as_slice().to_vec(),
    };
    let mut body = Vec::new();
    ce.write_to(&mut body);
    encoder.message(client_ids::CREATE_ENTITY, LengthStyle::Variable(2), &body);
}

#[cfg(test)]
mod tests {
    use super::*;
    use bw201::bundle::decode_messages;
    use bw201::interfaces::client_interface;
    use bw201::world::SPACE_DATA_TOD_KEY;
    use bw201::packet::{
        Packet, FLAG_CREATE_CHANNEL, FLAG_HAS_SEQUENCE_NUMBER, FLAG_IS_RELIABLE, FLAG_ON_CHANNEL,
    };
    use wot065_data::account::{full_sync_pickle, FullSyncOptions};
    use wot065_data::catalog::Catalog;
    use wot065_data::ops::EngineCmd;
    use wot065_data::pickle::{self, Value};
    use crate::payloads::PayloadOverrides;

    fn test_temp_dir(label: &str) -> std::path::PathBuf {
        let dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../target/test-tmp")
            .join(format!(
                "{label}-{}-{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn test_payloads() -> AccountPayloads {
        if let Some(dir) = wot065_data::testutil::game_dir() {
            if let Ok(cat) = wot065_data::catalog::load(&dir) {
                let catalog = std::sync::Arc::new(cat);
                let economics = std::sync::Arc::new(wot065_data::economics::EconomicsConfig::default());
                let full_sync = full_sync_pickle(&catalog, &FullSyncOptions::default());
                let full_sync_stream = wot065_data::account::stream_bundle(&full_sync);
                let shop_cache_stream =
                    wot065_data::account::stream_bundle(&wot065_data::account::shop_cache_pickle());
                let dossier_sync_stream =
                    wot065_data::account::stream_bundle(&wot065_data::account::dossier_sync_pickle(0));

                return AccountPayloads {
                    vehicle_count: catalog.count,
                    full_sync_pickle: full_sync,
                    full_sync_stream,
                    shop_cache_stream,
                    dossier_sync_stream,
                    showgui_ctx_override: None,
                    server_settings_pickle: AccountPayloads::default_server_settings_pickle(),
                    catalog,
                    economics,
                    overrides: PayloadOverrides::default(),
                };
            }
        }
        panic!("test_payloads: retail game files absent in test environment");
    }

    fn make_test_registry(payloads: &AccountPayloads) -> AccountRegistry {
        let dir = test_temp_dir("proto-test-reg");
        AccountRegistry::open(
            &dir,
            payloads.catalog.clone(),
            payloads.economics.clone(),
            wot065_data::account_state::Preset::Retail,
        )
        .unwrap()
    }

    fn make_session() -> BaseAppSession {
        let login =
            LoginSession::new("tester", b"0123456789abcdef", 0xCAFE_BABEu32, "127.0.0.1:5000");
        BaseAppSession::new(login, test_payloads()).unwrap()
    }

    fn client_wrap(enc: ElementEncoder, seq: u32, key: &BlowfishKey) -> Vec<u8> {
        let mut pkt = enc.finish();
        pkt.config.sequence = Some(seq);
        pkt.config.extra_flags |= FLAG_ON_CHANNEL | FLAG_IS_RELIABLE;
        pkt.finalize_flags();
        let plain = pkt.encode(Endian::Little);
        key.packet_encrypt(&plain)
    }

    fn decode_server_dgrams(
        dgrams: &[Vec<u8>],
        key: &BlowfishKey,
    ) -> Vec<(Packet, Vec<bw201::bundle::DecodedMessage>)> {
        let table = client_interface();
        dgrams
            .iter()
            .map(|d| {
                let plain = key.packet_decrypt(d).unwrap();
                let pkt = Packet::decode(&plain, Endian::Little).unwrap();
                let msgs = decode_messages(&pkt, &table, Endian::Little).unwrap();
                (pkt, msgs)
            })
            .collect()
    }

    #[test]
    fn base_app_login_returns_reply_and_authenticate_without_create_channel() {
        let mut sess = make_session();
        let mut reg = make_test_registry(&sess.payloads);
        let key = sess.blowfish().clone();

        let mut args = BinaryWriter::new();
        args.u32(sess.login().session_key);
        args.i32(0);

        let mut enc = ElementEncoder::new();
        enc.request(
            baseapp_ext_ids::BASE_APP_LOGIN,
            LengthStyle::Variable(2),
            42,
            args.as_slice(),
        );
        let dgrams = sess
            .handle_datagram(&client_wrap(enc, 0, &key), Instant::now(), &mut reg)
            .unwrap();
        let decoded = decode_server_dgrams(&dgrams, &key);
        assert_eq!(decoded.len(), 1);
        let (pkt, msgs) = &decoded[0];
        assert_ne!(pkt.flags & FLAG_ON_CHANNEL, 0);
        assert_ne!(pkt.flags & FLAG_IS_RELIABLE, 0);
        assert_ne!(pkt.flags & FLAG_HAS_SEQUENCE_NUMBER, 0);
        assert_eq!(pkt.flags & FLAG_CREATE_CHANNEL, 0);
        assert_eq!(pkt.config.sequence, Some(0));

        assert_eq!(msgs[0].id, 0xFF);
        let mut r = BinaryReader::new(&msgs[0].data);
        assert_eq!(r.u32().unwrap(), 42);
        let server_session_key = r.u32().unwrap();
        assert_eq!(server_session_key, sess.login().session_key);

        assert_eq!(msgs[1].id, client_ids::AUTHENTICATE);
        let mut r2 = BinaryReader::new(&msgs[1].data);
        assert_eq!(r2.u32().unwrap(), server_session_key);
    }

    fn battle_vehicle_fixture() -> VehicleState {
        VehicleState {
            entity_id: 7,
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
            health: 100,
            max_health: 100,
            position: Vec3::new(1.0, 2.0, 3.0),
            yaw: 0.0,
            pitch: 0.0,
            roll: 0.0,
            speed: 0.0,
            rspeed: 0.0,
            move_flags: 0,
            engine_mode: [1, 0],
            is_crew_active: true,
            left_arena: false,
            damage_stickers: Vec::new(),
            is_burning: false,
        }
    }

    fn decode_client_messages(encoder: ElementEncoder) -> Vec<DecodedMessage> {
        decode_messages(&encoder.finish(), &client_interface(), Endian::Little).unwrap()
    }

    /// The client numbers entity types in `entities.xml` order, counting only types
    /// with a client script (`res/scripts/client`): Account 0, Avatar 1, Vehicle 2,
    /// AreaDestructibles 3, OfflineEntity 4, Flock 5. With the server order (5) the
    /// client parsed a tank as `Flock` and aborted on
    /// `MF_ASSERT_DEV FAILED: n < clientServerProperties_.size()`.
    #[test]
    fn battle_entities_use_client_entity_type_indices() {
        let mut encoder = ElementEncoder::new();
        emit_create_vehicle_entity(&mut encoder, &battle_vehicle_fixture());
        emit_create_area_destructibles_entity(&mut encoder, 9, [0.0; 3], &[], &[], &[]);
        let types: Vec<(i32, u16)> = decode_client_messages(encoder)
            .iter()
            .filter(|m| m.id == client_ids::CREATE_ENTITY)
            .map(|m| {
                let entity = CreateEntity::read_from(&mut BinaryReader::new(&m.data)).unwrap();
                (entity.id, entity.entity_type)
            })
            .collect();
        assert_eq!(types, [(7, 2), (9, 3)]);
    }

    /// A time-of-day space data entry crashed the client right after
    /// `PlayerAvatar.onBecomePlayer` (access violation at `worldoftanks.exe+0x48d17f`);
    /// without it the client loads the arena with the map's own lighting.
    #[test]
    fn arena_space_data_omits_time_of_day() {
        let mut encoder = ElementEncoder::new();
        emit_arena_space_data(&mut encoder, 1, "spaces/04_himmelsdorf");
        let keys: Vec<u16> = decode_client_messages(encoder)
            .iter()
            .filter(|m| m.id == client_ids::SPACE_DATA)
            .map(|m| SpaceData::read_from(&mut BinaryReader::new(&m.data)).unwrap().key)
            .collect();
        assert!(!keys.is_empty(), "the arena needs its geometry mapping");
        assert!(!keys.contains(&SPACE_DATA_TOD_KEY), "space data keys {keys:?}");
    }

    #[test]
    fn enable_entities_emits_create_base_player_and_show_gui_once() {
        let mut sess = make_session();
        let mut reg = make_test_registry(&sess.payloads);
        let key = sess.blowfish().clone();

        // seq 0: baseAppLogin
        let mut args = BinaryWriter::new();
        args.u32(sess.login().session_key);
        args.i32(0);
        let mut enc0 = ElementEncoder::new();
        enc0.request(
            baseapp_ext_ids::BASE_APP_LOGIN,
            LengthStyle::Variable(2),
            1,
            args.as_slice(),
        );
        sess.handle_datagram(&client_wrap(enc0, 0, &key), Instant::now(), &mut reg)
            .unwrap();

        // seq 1: enableEntities
        let mut enc1 = ElementEncoder::new();
        enc1.fixed_message(baseapp_ext_ids::ENABLE_ENTITIES, 1, &[0]);
        let dgrams = sess
            .handle_datagram(&client_wrap(enc1, 1, &key), Instant::now(), &mut reg)
            .unwrap();
        let decoded = decode_server_dgrams(&dgrams, &key);
        let msgs = &decoded[0].1;
        assert_eq!(msgs[0].id, client_ids::BANDWIDTH_NOTIFICATION);
        assert_eq!(msgs[0].data, 1_000_000u32.to_le_bytes());
        assert_eq!(msgs[1].id, client_ids::UPDATE_FREQUENCY_NOTIFICATION);
        assert_eq!(msgs[1].data, vec![10u8]);
        assert_eq!(msgs[2].id, client_ids::SET_GAME_TIME);
        assert_eq!(msgs[2].data, 0u32.to_le_bytes());

        assert_eq!(msgs[3].id, client_ids::CREATE_BASE_PLAYER);
        let mut r = BinaryReader::new(&msgs[3].data);
        assert_eq!(r.i32().unwrap(), 1);
        assert_eq!(r.u16().unwrap(), ACCOUNT_ENTITY_TYPE_ID);
        assert_eq!(r.string().unwrap(), "tester");
        assert_eq!(r.string().unwrap(), "tester");
        let settings = pickle::loads(&r.string_bytes().unwrap()).unwrap();
        assert_eq!(
            settings.get("vivoxDomain").and_then(|v| v.as_bytes()),
            Some(&b""[..])
        );

        assert_eq!(msgs[4].id, client_method::SHOW_GUI);
        let mut gr = BinaryReader::new(&msgs[4].data);
        assert_eq!(gr.i32().unwrap(), 1);
        let ctx = pickle::loads(&gr.string_bytes().unwrap()).unwrap();
        assert_eq!(ctx.get("databaseID").and_then(|v| v.as_int()), Some(1));

        // Duplicate retransmit of seq 1 must not re-send createBasePlayer/showGUI
        let mut enc1_dup = ElementEncoder::new();
        enc1_dup.fixed_message(baseapp_ext_ids::ENABLE_ENTITIES, 1, &[0]);
        let dup_dgrams = sess
            .handle_datagram(&client_wrap(enc1_dup, 1, &key), Instant::now(), &mut reg)
            .unwrap();
        let dup_decoded = decode_server_dgrams(&dup_dgrams, &key);
        assert!(dup_decoded.iter().all(|(_, m)| m.is_empty()));
    }

    #[test]
    fn cmd_sync_data_returns_stream_when_stale_and_res_cache_when_matching() {
        let mut sess = make_session();
        let mut reg = make_test_registry(&sess.payloads);
        let key = sess.blowfish().clone();

        let profile = reg.get_or_create_profile("tester").unwrap();
        let current_rev = profile.rev;

        // 1. Stale / rev = 0 -> RES_STREAM
        let mut w0 = BinaryWriter::new();
        w0.i16(120);
        w0.i16(cmd::CMD_SYNC_DATA);
        w0.i32(0);
        w0.i32(0);
        w0.i32(0);

        let mut enc0 = ElementEncoder::new();
        enc0.message(base_method::DO_CMD_INT3, LengthStyle::Variable(2), w0.as_slice());

        let dgrams0 = sess
            .handle_datagram(&client_wrap(enc0, 0, &key), Instant::now(), &mut reg)
            .unwrap();
        let decoded0 = decode_server_dgrams(&dgrams0, &key);
        let msgs0 = &decoded0[0].1;

        assert_eq!(msgs0[0].id, client_method::ON_CMD_RESPONSE);
        let mut r0 = BinaryReader::new(&msgs0[0].data);
        assert_eq!(r0.i32().unwrap(), 1);
        assert_eq!(r0.i16().unwrap(), 120);
        assert_eq!(r0.i16().unwrap(), cmd::RES_STREAM);
        assert_eq!(msgs0[1].id, client_ids::RESOURCE_HEADER);

        // 2. Matching rev -> ON_CMD_RESPONSE_EXT with RES_CACHE (2)
        let mut w_match = BinaryWriter::new();
        w_match.i16(121);
        w_match.i16(cmd::CMD_SYNC_DATA);
        w_match.i32((current_rev >> 32) as i32);
        w_match.i32((current_rev & 0xFFFFFFFF) as i32);
        w_match.i32(0);

        let mut enc_match = ElementEncoder::new();
        enc_match.message(base_method::DO_CMD_INT3, LengthStyle::Variable(2), w_match.as_slice());

        let dgrams_match = sess
            .handle_datagram(&client_wrap(enc_match, 1, &key), Instant::now(), &mut reg)
            .unwrap();
        let decoded_match = decode_server_dgrams(&dgrams_match, &key);
        let msgs_match = &decoded_match[0].1;

        assert_eq!(msgs_match[0].id, client_method::ON_CMD_RESPONSE_EXT);
        let mut rm = BinaryReader::new(&msgs_match[0].data);
        assert_eq!(rm.i32().unwrap(), 1);
        assert_eq!(rm.i16().unwrap(), 121);
        assert_eq!(rm.i16().unwrap(), cmd::RES_CACHE); // RES_CACHE = 2
        let delta = pickle::loads(&rm.string_bytes().unwrap()).unwrap();
        assert_eq!(delta.get("rev").and_then(|v| v.as_int()), Some(current_rev));
        assert_eq!(delta.get("prevRev").and_then(|v| v.as_int()), Some(current_rev));
    }

    #[test]
    fn mutating_command_emits_update_before_on_cmd_response() {
        let mut sess = make_session();
        let mut reg = make_test_registry(&sess.payloads);
        let key = sess.blowfish().clone();

        reg.apply_command("tester", &EngineCmd::SetMoney { credits: 100_000, gold: 10_000 }, 1_700_000_000).unwrap();

        let mut w = BinaryWriter::new();
        w.i16(120);
        w.i16(cmd::CMD_BUY_SLOT);
        w.i32(1); // shop_rev
        w.i32(0);
        w.i32(0);

        let mut enc = ElementEncoder::new();
        enc.message(base_method::DO_CMD_INT3, LengthStyle::Variable(2), w.as_slice());

        let dgrams = sess
            .handle_datagram(&client_wrap(enc, 0, &key), Instant::now(), &mut reg)
            .unwrap();
        let decoded = decode_server_dgrams(&dgrams, &key);
        let msgs = &decoded[0].1;

        assert_eq!(msgs.len(), 2);
        assert_eq!(msgs[0].id, client_method::UPDATE);
        assert_eq!(msgs[1].id, client_method::ON_CMD_RESPONSE);

        let mut r = BinaryReader::new(&msgs[1].data);
        assert_eq!(r.i32().unwrap(), 1);
        assert_eq!(r.i16().unwrap(), 120);
        assert_eq!(r.i16().unwrap(), cmd::RES_SUCCESS);
    }

    #[test]
    fn buy_tankman_emits_update_then_resync_dossiers_then_on_cmd_response_ext() {
        let mut sess = make_session();
        let mut reg = make_test_registry(&sess.payloads);
        let key = sess.blowfish().clone();

        reg.apply_command("tester", &EngineCmd::SetMoney { credits: 100_000, gold: 10_000 }, 1_700_000_000).unwrap();

        let ms1_cd = reg.catalog().vehicles.iter().find(|v| v.name == "MS-1").unwrap().veh_type_comp_descr;

        let mut w = BinaryWriter::new();
        w.i16(121);
        w.i16(cmd::CMD_BUY_TMAN);
        w.i32(1); // shop_rev
        w.i32(ms1_cd); // veh_type_cd
        w.i32(1); // role_idx (commander)
        w.i32(0); // tman_cost_idx (free 50%)

        let mut enc = ElementEncoder::new();
        enc.message(base_method::DO_CMD_INT4, LengthStyle::Variable(2), w.as_slice());

        let dgrams = sess
            .handle_datagram(&client_wrap(enc, 0, &key), Instant::now(), &mut reg)
            .unwrap();
        let decoded = decode_server_dgrams(&dgrams, &key);
        let msgs = &decoded[0].1;

        assert_eq!(msgs.len(), 3);
        assert_eq!(msgs[0].id, client_method::UPDATE);
        assert_eq!(msgs[1].id, client_method::RESYNC_DOSSIERS);
        assert_eq!(msgs[2].id, client_method::ON_CMD_RESPONSE_EXT);

        let mut r = BinaryReader::new(&msgs[2].data);
        assert_eq!(r.i32().unwrap(), 1);
        assert_eq!(r.i16().unwrap(), 121);
        assert_eq!(r.i16().unwrap(), cmd::RES_SUCCESS);
        let ext = pickle::loads(&r.string_bytes().unwrap()).unwrap();
        assert!(ext.get("tmanInvID").and_then(|v| v.as_int()).is_some());
        assert!(ext.get("tmanCompDescr").and_then(|v| v.as_bytes()).is_some());
    }

    #[test]
    fn request_player_dossier_and_clan_info_emit_strictly_on_cmd_response_and_stream() {
        let mut sess = make_session();
        let mut reg = make_test_registry(&sess.payloads);
        let key = sess.blowfish().clone();

        reg.get_or_create_profile("tester").unwrap();

        // 1. CMD_REQ_PLAYER_DOSSIER (503)
        let mut w_doss = BinaryWriter::new();
        w_doss.i16(122);
        w_doss.i16(cmd::CMD_REQ_PLAYER_DOSSIER);
        w_doss.i32(1); // DOSSIER_TYPE_ACCOUNT
        w_doss.i32(0);
        w_doss.string("tester");

        let mut enc_doss = ElementEncoder::new();
        enc_doss.message(base_method::DO_CMD_INT2_STR, LengthStyle::Variable(2), w_doss.as_slice());

        let dgrams = sess
            .handle_datagram(&client_wrap(enc_doss, 0, &key), Instant::now(), &mut reg)
            .unwrap();
        let decoded = decode_server_dgrams(&dgrams, &key);
        let msgs = &decoded[0].1;

        assert_eq!(msgs[0].id, client_method::ON_CMD_RESPONSE); // Strictly 0x82!
        let mut r = BinaryReader::new(&msgs[0].data);
        assert_eq!(r.i32().unwrap(), 1);
        assert_eq!(r.i16().unwrap(), 122);
        assert_eq!(r.i16().unwrap(), cmd::RES_STREAM);
        assert_eq!(msgs[1].id, client_ids::RESOURCE_HEADER);

        // 2. CMD_REQ_PLAYER_CLAN_INFO (504)
        let mut w_clan = BinaryWriter::new();
        w_clan.i16(123);
        w_clan.i16(cmd::CMD_REQ_PLAYER_CLAN_INFO);
        w_clan.i32(0);
        w_clan.i32(0);
        w_clan.string("tester");

        let mut enc_clan = ElementEncoder::new();
        enc_clan.message(base_method::DO_CMD_INT2_STR, LengthStyle::Variable(2), w_clan.as_slice());

        let dgrams_clan = sess
            .handle_datagram(&client_wrap(enc_clan, 1, &key), Instant::now(), &mut reg)
            .unwrap();
        let decoded_clan = decode_server_dgrams(&dgrams_clan, &key);
        let msgs_clan = &decoded_clan[0].1;

        assert_eq!(msgs_clan[0].id, client_method::ON_CMD_RESPONSE); // Strictly 0x82!
        let mut r_clan = BinaryReader::new(&msgs_clan[0].data);
        assert_eq!(r_clan.i32().unwrap(), 1);
        assert_eq!(r_clan.i16().unwrap(), 123);
        assert_eq!(r_clan.i16().unwrap(), cmd::RES_STREAM);
        assert_eq!(msgs_clan[1].id, client_ids::RESOURCE_HEADER);
    }

    #[test]
    fn cmd_sync_dossiers_always_responds_res_stream() {
        let mut sess = make_session();
        let mut reg = make_test_registry(&sess.payloads);
        let key = sess.blowfish().clone();

        let mut w = BinaryWriter::new();
        w.i16(124);
        w.i16(cmd::CMD_SYNC_DOSSIERS);
        w.i32(1); // DOSSIER_CACHE_VERSION
        w.i32(0);
        w.i32(0);

        let mut enc = ElementEncoder::new();
        enc.message(base_method::DO_CMD_INT3, LengthStyle::Variable(2), w.as_slice());

        let dgrams = sess
            .handle_datagram(&client_wrap(enc, 0, &key), Instant::now(), &mut reg)
            .unwrap();
        let decoded = decode_server_dgrams(&dgrams, &key);
        let msgs = &decoded[0].1;

        assert_eq!(msgs[0].id, client_method::ON_CMD_RESPONSE);
        let mut r = BinaryReader::new(&msgs[0].data);
        assert_eq!(r.i32().unwrap(), 1);
        assert_eq!(r.i16().unwrap(), 124);
        assert_eq!(r.i16().unwrap(), cmd::RES_STREAM);
        assert_eq!(msgs[1].id, client_ids::RESOURCE_HEADER);
    }

    #[test]
    fn wrong_shop_rev_emits_on_cmd_response_ext_shop_desync() {
        let mut sess = make_session();
        let mut reg = make_test_registry(&sess.payloads);
        let key = sess.blowfish().clone();

        let mut w = BinaryWriter::new();
        w.i16(127);
        w.i16(cmd::CMD_BUY_SLOT);
        w.i32(999); // Wrong shop_rev
        w.i32(0);
        w.i32(0);

        let mut enc = ElementEncoder::new();
        enc.message(base_method::DO_CMD_INT3, LengthStyle::Variable(2), w.as_slice());

        let dgrams = sess
            .handle_datagram(&client_wrap(enc, 0, &key), Instant::now(), &mut reg)
            .unwrap();
        let decoded = decode_server_dgrams(&dgrams, &key);
        let msgs = &decoded[0].1;

        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0].id, client_method::ON_CMD_RESPONSE_EXT);
        let mut r = BinaryReader::new(&msgs[0].data);
        assert_eq!(r.i32().unwrap(), 1);
        assert_eq!(r.i16().unwrap(), 127);
        assert_eq!(r.i16().unwrap(), cmd::RES_SHOP_DESYNC);
        let ext = pickle::loads(&r.string_bytes().unwrap()).unwrap();
        assert_eq!(ext.get("shopRev").and_then(|v| v.as_int()), Some(1));
    }

    #[test]
    fn shop_dossiers_stats_and_set_language_commands_respond_correctly() {
        let mut sess = make_session();
        let mut reg = make_test_registry(&sess.payloads);
        let key = sess.blowfish().clone();

        let mut enc = ElementEncoder::new();

        let mut shop = BinaryWriter::new();
        shop.i16(122);
        shop.i16(cmd::CMD_SYNC_SHOP);
        shop.i32(0);
        shop.i32(0);
        shop.i32(0);
        enc.message(
            base_method::DO_CMD_INT3,
            LengthStyle::Variable(2),
            shop.as_slice(),
        );

        let mut doss = BinaryWriter::new();
        doss.i16(123);
        doss.i16(cmd::CMD_SYNC_DOSSIERS);
        doss.i32(0);
        doss.i32(0);
        doss.i32(0);
        enc.message(
            base_method::DO_CMD_INT3,
            LengthStyle::Variable(2),
            doss.as_slice(),
        );

        let mut stats = BinaryWriter::new();
        stats.i16(cmd::REQUEST_ID_NO_RESPONSE);
        stats.i16(cmd::CMD_REQ_SERVER_STATS);
        stats.i32(0);
        stats.i32(0);
        stats.i32(0);
        enc.message(
            base_method::DO_CMD_INT3,
            LengthStyle::Variable(2),
            stats.as_slice(),
        );

        let mut lang = BinaryWriter::new();
        lang.i16(124);
        lang.i16(cmd::CMD_SET_LANGUAGE);
        lang.string("en");
        enc.message(
            base_method::DO_CMD_STR,
            LengthStyle::Variable(2),
            lang.as_slice(),
        );

        let dgrams = sess
            .handle_datagram(&client_wrap(enc, 0, &key), Instant::now(), &mut reg)
            .unwrap();
        let all_msgs: Vec<_> = decode_server_dgrams(&dgrams, &key)
            .into_iter()
            .flat_map(|(_, m)| m)
            .collect();

        assert!(all_msgs.iter().any(|m| {
            if m.id != client_method::ON_CMD_RESPONSE {
                return false;
            }
            let mut r = BinaryReader::new(&m.data);
            r.i32().ok() == Some(1)
                && r.i16().ok() == Some(122)
                && r.i16().ok() == Some(cmd::RES_STREAM)
        }));
        assert!(all_msgs.iter().any(|m| {
            if m.id != client_method::ON_CMD_RESPONSE {
                return false;
            }
            let mut r = BinaryReader::new(&m.data);
            r.i32().ok() == Some(1)
                && r.i16().ok() == Some(123)
                && r.i16().ok() == Some(cmd::RES_STREAM)
        }));
        assert!(all_msgs.iter().any(|m| {
            if m.id != client_method::RECEIVE_SERVER_STATS {
                return false;
            }
            let mut r = BinaryReader::new(&m.data);
            r.i32().ok() == Some(1) && r.u32().ok() == Some(1) && r.u32().ok() == Some(0)
        }));
        assert!(all_msgs.iter().any(|m| {
            if m.id != client_method::ON_CMD_RESPONSE {
                return false;
            }
            let mut r = BinaryReader::new(&m.data);
            r.i32().ok() == Some(1)
                && r.i16().ok() == Some(124)
                && r.i16().ok() == Some(cmd::RES_SUCCESS)
        }));

        let resource_rids: Vec<u16> = all_msgs
            .iter()
            .filter(|m| m.id == client_ids::RESOURCE_HEADER)
            .map(|m| BinaryReader::new(&m.data).u16().unwrap())
            .collect();
        assert_eq!(resource_rids, vec![122, 123]);
    }

    #[test]
    fn unhandled_commands_return_failure_and_extended_messages_decode() {
        let mut sess = make_session();
        let mut reg = make_test_registry(&sess.payloads);
        let key = sess.blowfish().clone();

        let mut enc = ElementEncoder::new();

        // doCmdInt2Str (unknown cmd -> RES_FAILURE = -1)
        let mut w_i2s = BinaryWriter::new();
        w_i2s.i16(130);
        w_i2s.i16(9999);
        w_i2s.i32(10);
        w_i2s.i32(20);
        w_i2s.string("clan_tag");
        enc.message(
            base_method::DO_CMD_INT2_STR,
            LengthStyle::Variable(2),
            w_i2s.as_slice(),
        );

        // doCmdIntArr (unknown cmd -> RES_FAILURE = -1)
        let mut w_arr = BinaryWriter::new();
        w_arr.i16(131);
        w_arr.i16(9998);
        w_arr.i32(2);
        w_arr.i32(7);
        w_arr.i32(8);
        enc.message(
            base_method::DO_CMD_INT_ARR,
            LengthStyle::Variable(2),
            w_arr.as_slice(),
        );

        // Reserved request_id (< 120) must never emit onCmdResponse
        let mut w_reserved = BinaryWriter::new();
        w_reserved.i16(100);
        w_reserved.i16(9997);
        w_reserved.i32(0);
        w_reserved.i32(0);
        w_reserved.i32(0);
        enc.message(
            base_method::DO_CMD_INT3,
            LengthStyle::Variable(2),
            w_reserved.as_slice(),
        );

        // requestEntityUpdate (0x09)
        let mut w_reu = BinaryWriter::new();
        w_reu.i32(1);
        enc.message(
            baseapp_ext_ids::REQUEST_ENTITY_UPDATE,
            LengthStyle::Variable(2),
            w_reu.as_slice(),
        );

        let dgrams = sess
            .handle_datagram(&client_wrap(enc, 0, &key), Instant::now(), &mut reg)
            .unwrap();
        let msgs: Vec<_> = decode_server_dgrams(&dgrams, &key)
            .into_iter()
            .flat_map(|(_, m)| m)
            .collect();
        let responses: Vec<(i16, i16)> = msgs
            .iter()
            .filter(|m| m.id == client_method::ON_CMD_RESPONSE)
            .map(|m| {
                let mut r = BinaryReader::new(&m.data);
                let _entity = r.i32().unwrap();
                (r.i16().unwrap(), r.i16().unwrap())
            })
            .collect();
        assert_eq!(responses, vec![(130, cmd::RES_FAILURE), (131, cmd::RES_FAILURE)]);
    }

    #[test]
    fn large_130_vehicle_sync_fragments_across_udp_and_reassembles_cleanly() {
        let mut catalog = Catalog::default();
        for i in 0..130usize {
            let nation_id = match i {
                0 => 0,
                1 => 1,
                2 => 2,
                _ => 0,
            };
            let name = match i {
                0 => "MS-1".into(),
                1 => "Ltraktor".into(),
                2 => "T1_Cunningham".into(),
                _ => format!("Tank_{i}"),
            };
            catalog.vehicles.push(wot065_data::catalog::VehicleType {
                nation: "ussr".into(),
                nation_id,
                type_id: i as u16,
                name,
                level: ((i % 10) + 1) as u8,
                price: 1000 * (i as i64 + 1),
                tags: vec!["mediumTank".into()],
                veh_type_comp_descr: ((i as i32) << 8) | 1,
                veh_comp_descr_hex: format!("{:030x}", i + 1),
                veh_comp_descr_b64: String::new(),
                veh_comp_descr_len: 15,
                components: wot065_data::catalog::Components {
                    chassis: "c".into(),
                    engines: "e".into(),
                    fuel_tanks: "f".into(),
                    radios: "r".into(),
                    turrets: "t".into(),
                    guns: "g".into(),
                },
                component_ids: wot065_data::catalog::ComponentIds {
                    chassis: i as u16 + 1,
                    engine: i as u16 + 2,
                    fuel_tank: i as u16 + 3,
                    radio: i as u16 + 4,
                    turret: i as u16 + 5,
                    gun: i as u16 + 6,
                },
                comp_descr: vec![(i & 0xFF) as u8; 15],
            });
        }
        catalog.count = 130;

        let full_sync = full_sync_pickle(
            &catalog,
            &FullSyncOptions {
                unlock_all: true,
                ..FullSyncOptions::default()
            },
        );
        let payloads = AccountPayloads {
            vehicle_count: 130,
            full_sync_stream: wot065_data::account::stream_bundle(&full_sync),
            full_sync_pickle: full_sync,
            shop_cache_stream: wot065_data::account::stream_bundle(
                &wot065_data::account::shop_cache_pickle(),
            ),
            dossier_sync_stream: wot065_data::account::stream_bundle(
                &wot065_data::account::dossier_sync_pickle(0),
            ),
            showgui_ctx_override: None,
            server_settings_pickle: AccountPayloads::default_server_settings_pickle(),
            catalog: std::sync::Arc::new(catalog),
            economics: std::sync::Arc::new(wot065_data::economics::EconomicsConfig::default()),
            overrides: PayloadOverrides {
                account_diff: Some(std::path::PathBuf::from("mock_130_override")),
                ..PayloadOverrides::default()
            },
        };
        let mut reg = make_test_registry(&payloads);
        let login =
            LoginSession::new("tester", b"0123456789abcdef", 0xCAFE_BABEu32, "127.0.0.1:5000");
        let mut sess = BaseAppSession::new(login, payloads).unwrap();
        let key = sess.blowfish().clone();

        let mut w = BinaryWriter::new();
        w.i16(120);
        w.i16(cmd::CMD_SYNC_DATA);
        w.i32(0);
        w.i32(0);
        w.i32(0);
        let mut enc = ElementEncoder::new();
        enc.message(
            base_method::DO_CMD_INT3,
            LengthStyle::Variable(2),
            w.as_slice(),
        );
        let dgrams = sess
            .handle_datagram(&client_wrap(enc, 0, &key), Instant::now(), &mut reg)
            .unwrap();
        assert!(dgrams.len() > 1, "expected multi-packet UDP fragmentation, got {}", dgrams.len());

        // Simulate client ChannelState reassembling the multi-packet fragments.
        let mut client_rx = ChannelState::new();
        let mut reassembled_bundles = Vec::new();
        for d in &dgrams {
            let plain = key.packet_decrypt(d).unwrap();
            let pkt = Packet::decode(&plain, Endian::Little).unwrap();
            let rx = client_rx.receive(pkt);
            reassembled_bundles.extend(rx.packets);
        }
        assert_eq!(reassembled_bundles.len(), 1);

        let msgs = decode_messages(&reassembled_bundles[0], &client_interface(), Endian::Little)
            .unwrap();
        assert_eq!(msgs[0].id, client_method::ON_CMD_RESPONSE);
        assert_eq!(msgs[1].id, client_ids::RESOURCE_HEADER);
        let mut compressed = Vec::new();
        for m in &msgs[2..] {
            assert_eq!(m.id, client_ids::RESOURCE_FRAGMENT);
            let mut rf = BinaryReader::new(&m.data);
            let _rid = rf.u16().unwrap();
            let _seq = rf.u8().unwrap();
            let _flags = rf.u8().unwrap();
            compressed.extend_from_slice(rf.rest());
        }
        let parsed = pickle::loads(&pickle::zlib_decompress(&compressed).unwrap()).unwrap();
        let comp = parsed
            .get("inventory")
            .unwrap()
            .get_int(1)
            .unwrap()
            .get("compDescr")
            .unwrap();
        assert_eq!(
            match comp {
                Value::Dict(items) => items.len(),
                _ => 0,
            },
            130
        );
    }

    #[test]
    fn malformed_or_wrong_key_datagram_returns_err_without_panic() {
        let mut sess = make_session();
        let mut reg = make_test_registry(&sess.payloads);
        assert!(sess.handle_datagram(&[1, 2, 3], Instant::now(), &mut reg).is_err());
        let wrong_key = BlowfishKey::new(b"wrong-key-999999").unwrap();
        let mut enc = ElementEncoder::new();
        enc.fixed_message(baseapp_ext_ids::ENABLE_ENTITIES, 1, &[0]);
        let bad = client_wrap(enc, 0, &wrong_key);
        assert!(sess.handle_datagram(&bad, Instant::now(), &mut reg).is_err());
    }

    #[test]
    fn plaintext_off_channel_base_app_login_21_bytes_is_accepted_and_replied_encrypted() {
        let mut sess = make_session();
        let mut reg = make_test_registry(&sess.payloads);
        let key = sess.blowfish().clone();

        let mut args = BinaryWriter::new();
        args.u32(sess.login().session_key);
        args.i32(0);

        let mut enc = ElementEncoder::new();
        enc.request(
            baseapp_ext_ids::BASE_APP_LOGIN,
            LengthStyle::Variable(2),
            0x0000_CAFE,
            args.as_slice(),
        );
        let raw_pkt = enc.finish();
        let raw = raw_pkt.encode(Endian::Little);
        assert_eq!(raw.len(), 21);

        let dgrams = sess.handle_datagram(&raw, Instant::now(), &mut reg).unwrap();
        let decoded = decode_server_dgrams(&dgrams, &key);
        assert_eq!(decoded.len(), 1);
        let (pkt, msgs) = &decoded[0];
        assert_ne!(pkt.flags & FLAG_ON_CHANNEL, 0);
        assert_ne!(pkt.flags & FLAG_IS_RELIABLE, 0);
        assert_ne!(pkt.flags & FLAG_HAS_SEQUENCE_NUMBER, 0);
        assert_eq!(pkt.flags & FLAG_CREATE_CHANNEL, 0);
        assert_eq!(pkt.config.sequence, Some(0));
        assert_eq!(sess.channel().in_seq_at(), 0);

        assert_eq!(msgs[0].id, 0xFF);
        let mut r = BinaryReader::new(&msgs[0].data);
        assert_eq!(r.u32().unwrap(), 0x0000_CAFE);
        assert_eq!(r.u32().unwrap(), sess.login().session_key);
        assert_eq!(msgs[1].id, client_ids::AUTHENTICATE);

        // Next packet from client on channel starts at seq=0 and stays Blowfish encrypted.
        let mut enc_en = ElementEncoder::new();
        enc_en.fixed_message(baseapp_ext_ids::ENABLE_ENTITIES, 1, &[0]);
        let mut pkt_en = enc_en.finish();
        pkt_en.config.sequence = Some(0);
        pkt_en.config.cumulative_ack = Some(1);
        pkt_en.config.extra_flags |= FLAG_ON_CHANNEL | FLAG_IS_RELIABLE;
        pkt_en.finalize_flags();
        let dgram_en = key.packet_encrypt(&pkt_en.encode(Endian::Little));

        let dgrams2 = sess.handle_datagram(&dgram_en, Instant::now(), &mut reg).unwrap();
        assert_eq!(sess.channel().unacked_len(), 1);
        let decoded2 = decode_server_dgrams(&dgrams2, &key);
        assert_eq!(decoded2[0].0.config.sequence, Some(1));
        assert!(decoded2[0]
            .1
            .iter()
            .any(|m| m.id == client_ids::CREATE_BASE_PLAYER));
        assert!(decoded2[0]
            .1
            .iter()
            .any(|m| m.id == client_method::SHOW_GUI));
    }

    #[test]
    fn plaintext_non_login_packet_is_rejected() {
        let mut sess = make_session();
        let mut reg = make_test_registry(&sess.payloads);

        let mut enc_en = ElementEncoder::new();
        enc_en.fixed_message(baseapp_ext_ids::ENABLE_ENTITIES, 1, &[0]);
        let raw_en = enc_en.finish().encode(Endian::Little);
        assert!(matches!(
            sess.handle_datagram(&raw_en, Instant::now(), &mut reg),
            Err(ProtocolError::Decrypt(_))
        ));

        let mut args = BinaryWriter::new();
        args.u32(sess.login().session_key);
        args.i32(0);
        let mut enc_mixed = ElementEncoder::new();
        enc_mixed.request(
            baseapp_ext_ids::BASE_APP_LOGIN,
            LengthStyle::Variable(2),
            1,
            args.as_slice(),
        );
        enc_mixed.fixed_message(baseapp_ext_ids::ENABLE_ENTITIES, 1, &[0]);
        let raw_mixed = enc_mixed.finish().encode(Endian::Little);
        assert!(matches!(
            sess.handle_datagram(&raw_mixed, Instant::now(), &mut reg),
            Err(ProtocolError::Decrypt(_))
        ));
    }

    #[test]
    fn decode_do_cmd_handles_all_five_wire_methods() {
        // 1. DO_CMD_STR (0xC3)
        let mut w_str = BinaryWriter::new();
        w_str.i16(120);
        w_str.i16(cmd::CMD_SET_LANGUAGE);
        w_str.string("uk");
        let decoded_str = decode_do_cmd(base_method::DO_CMD_STR, w_str.as_slice()).unwrap();
        assert_eq!(decoded_str.request_id, 120);
        assert_eq!(decoded_str.command_id, cmd::CMD_SET_LANGUAGE);
        assert_eq!(decoded_str.args, DoCmdArgs::Str("uk".into()));

        // 2. DO_CMD_INT3 (0xC4)
        let mut w_int3 = BinaryWriter::new();
        w_int3.i16(121);
        w_int3.i16(cmd::CMD_BUY_SLOT);
        w_int3.i32(1);
        w_int3.i32(0);
        w_int3.i32(0);
        let decoded_int3 = decode_do_cmd(base_method::DO_CMD_INT3, w_int3.as_slice()).unwrap();
        assert_eq!(decoded_int3.request_id, 121);
        assert_eq!(decoded_int3.command_id, cmd::CMD_BUY_SLOT);
        assert_eq!(decoded_int3.args, DoCmdArgs::Int3([1, 0, 0]));

        // 3. DO_CMD_INT4 (0xC5)
        let mut w_int4 = BinaryWriter::new();
        w_int4.i16(122);
        w_int4.i16(cmd::CMD_BUY_VEHICLE);
        w_int4.i32(1);
        w_int4.i32(2);
        w_int4.i32(17);
        w_int4.i32(0);
        let decoded_int4 = decode_do_cmd(base_method::DO_CMD_INT4, w_int4.as_slice()).unwrap();
        assert_eq!(decoded_int4.request_id, 122);
        assert_eq!(decoded_int4.command_id, cmd::CMD_BUY_VEHICLE);
        assert_eq!(decoded_int4.args, DoCmdArgs::Int4([1, 2, 17, 0]));

        // 4. DO_CMD_INT2_STR (0xC6)
        let mut w_int2_str = BinaryWriter::new();
        w_int2_str.i16(123);
        w_int2_str.i16(cmd::CMD_REQ_PLAYER_DOSSIER);
        w_int2_str.i32(1);
        w_int2_str.i32(0);
        w_int2_str.string("Player");
        let decoded_int2_str = decode_do_cmd(base_method::DO_CMD_INT2_STR, w_int2_str.as_slice()).unwrap();
        assert_eq!(decoded_int2_str.request_id, 123);
        assert_eq!(decoded_int2_str.command_id, cmd::CMD_REQ_PLAYER_DOSSIER);
        assert_eq!(decoded_int2_str.args, DoCmdArgs::Int2Str(1, 0, "Player".into()));

        // 5. DO_CMD_INT_ARR (0xC7)
        let mut w_arr = BinaryWriter::new();
        w_arr.i16(124);
        w_arr.i16(cmd::CMD_EQUIP_SHELLS);
        w_arr.i32(5);
        w_arr.i32(100);
        w_arr.i32(1);
        w_arr.i32(20);
        w_arr.i32(2);
        w_arr.i32(15);
        let decoded_arr = decode_do_cmd(base_method::DO_CMD_INT_ARR, w_arr.as_slice()).unwrap();
        assert_eq!(decoded_arr.request_id, 124);
        assert_eq!(decoded_arr.command_id, cmd::CMD_EQUIP_SHELLS);
        assert_eq!(decoded_arr.args, DoCmdArgs::IntArr(vec![100, 1, 20, 2, 15]));
    }

    #[test]
    fn cmd_sync_shop_returns_res_cache_when_rev_matches() {
        let payloads = test_payloads();
        let mut reg = make_test_registry(&payloads);
        let mut sess = make_session();
        let key = sess.blowfish().clone();

        // Initial client_rev = 0 -> RES_STREAM + RESOURCE_HEADER
        let mut w0 = BinaryWriter::new();
        w0.i16(120);
        w0.i16(cmd::CMD_SYNC_SHOP);
        w0.i32(0);
        w0.i32(0);
        w0.i32(0);
        let mut enc0 = ElementEncoder::new();
        enc0.message(base_method::DO_CMD_INT3, LengthStyle::Variable(2), w0.as_slice());
        let dgrams0 = sess
            .handle_datagram(&client_wrap(enc0, 0, &key), Instant::now(), &mut reg)
            .unwrap();
        let msgs0: Vec<_> = decode_server_dgrams(&dgrams0, &key)
            .into_iter()
            .flat_map(|(_, m)| m)
            .collect();
        let mut r0 = BinaryReader::new(&msgs0[0].data);
        assert_eq!(msgs0[0].id, client_method::ON_CMD_RESPONSE);
        assert_eq!(r0.i32().unwrap(), 1);
        assert_eq!(r0.i16().unwrap(), 120);
        assert_eq!(r0.i16().unwrap(), cmd::RES_STREAM);
        assert!(msgs0.iter().any(|m| m.id == client_ids::RESOURCE_HEADER));

        // Follow-up client_rev = 1 -> RES_CACHE without resource stream
        let mut w1 = BinaryWriter::new();
        w1.i16(121);
        w1.i16(cmd::CMD_SYNC_SHOP);
        w1.i32(1);
        w1.i32(0);
        w1.i32(0);
        let mut enc1 = ElementEncoder::new();
        enc1.message(base_method::DO_CMD_INT3, LengthStyle::Variable(2), w1.as_slice());
        let dgrams1 = sess
            .handle_datagram(&client_wrap(enc1, 1, &key), Instant::now(), &mut reg)
            .unwrap();
        let msgs1: Vec<_> = decode_server_dgrams(&dgrams1, &key)
            .into_iter()
            .flat_map(|(_, m)| m)
            .collect();
        assert_eq!(msgs1.len(), 1);
        assert_eq!(msgs1[0].id, client_method::ON_CMD_RESPONSE);
        let mut r1 = BinaryReader::new(&msgs1[0].data);
        assert_eq!(r1.i32().unwrap(), 1);
        assert_eq!(r1.i16().unwrap(), 121);
        assert_eq!(r1.i16().unwrap(), cmd::RES_CACHE);
    }

    #[test]
    fn cmd_req_server_stats_reports_online_players_count() {
        let payloads = test_payloads();
        let mut reg = make_test_registry(&payloads);
        let mut sess = make_session();
        let key = sess.blowfish().clone();

        let mut w = BinaryWriter::new();
        w.i16(125);
        w.i16(cmd::CMD_REQ_SERVER_STATS);
        w.i32(0);
        w.i32(0);
        w.i32(0);
        let mut enc = ElementEncoder::new();
        enc.message(base_method::DO_CMD_INT3, LengthStyle::Variable(2), w.as_slice());

        let dgrams = sess
            .handle_datagram_with_online_players(&client_wrap(enc, 0, &key), Instant::now(), &mut reg, 7)
            .unwrap();
        let msgs: Vec<_> = decode_server_dgrams(&dgrams, &key)
            .into_iter()
            .flat_map(|(_, m)| m)
            .collect();
        let stats_msg = msgs
            .iter()
            .find(|m| m.id == client_method::RECEIVE_SERVER_STATS)
            .expect("RECEIVE_SERVER_STATS");
        let mut r = BinaryReader::new(&stats_msg.data);
        assert_eq!(r.i32().unwrap(), 1); // player_id
        assert_eq!(r.u32().unwrap(), 7); // clusterOnlinePlayersCount
        assert_eq!(r.u32().unwrap(), 0);
    }
}
