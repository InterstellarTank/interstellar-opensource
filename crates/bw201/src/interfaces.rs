//! Mercury interface tables for the WoT 0.6.5 session.
//!
//! Mercury message ids are assigned in `InterfaceMinder::add` registration
//! order, starting at 0 (`src/lib/network/interface_minder.cpp`: `elements_.size()`
//! is used as the id). The declarations are therefore authoritative for the
//! numeric ids.
//!
//! Tables provided:
//! - [`login_interface`] — `LoginInterface` (`src/lib/connection/login_interface.hpp`)
//! - [`baseapp_ext_interface`] — `BaseAppExtInterface` (`baseapp_ext_interface.hpp`):
//!   messages sent **client -> BaseApp**
//! - [`client_interface`] — `ClientInterface` (`client_interface.hpp`):
//!   messages sent **BaseApp -> client**
//!
//! Both directions share id space 0..=57 but with different length styles for
//! ids 0 and 1, so a server must parse incoming packets with the interface table
//! matching their direction (this mirrors Mercury's own per-nub interface
//! tables: the client serves `ClientInterface`, the BaseApp serves
//! `BaseAppExtInterface`).

use std::collections::HashMap;

use crate::packet::PACKET_MAX_SIZE;

/// Mercury's reply pseudo-message id (`REPLY_MESSAGE_IDENTIFIER`).
pub const REPLY_MESSAGE_IDENTIFIER: u8 = 0xFF;

/// How an interface element encodes its length field.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LengthStyle {
    /// Fixed payload size in bytes; no length field is written.
    Fixed(u16),
    /// Variable payload size with a length field of `n` bytes (1, 2, 3 or 4).
    Variable(u8),
}

impl LengthStyle {
    /// Bytes occupied by the element header (id + length field).
    pub fn header_size(self) -> usize {
        1 + match self {
            LengthStyle::Fixed(_) => 0,
            LengthStyle::Variable(n) => n as usize,
        }
    }

    /// Nominal body size for fixed messages; 0 for variable ones.
    pub fn nominal_body_size(self) -> usize {
        match self {
            LengthStyle::Fixed(n) => n as usize,
            LengthStyle::Variable(_) => 0,
        }
    }
}

/// One declared Mercury message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InterfaceElement {
    pub id: u8,
    pub style: LengthStyle,
    pub name: &'static str,
}

impl InterfaceElement {
    pub const fn variable2(id: u8, name: &'static str) -> Self {
        Self {
            id,
            style: LengthStyle::Variable(2),
            name,
        }
    }

    pub const fn fixed(id: u8, body: u16, name: &'static str) -> Self {
        Self {
            id,
            style: LengthStyle::Fixed(body),
            name,
        }
    }

    pub fn header_size(&self) -> usize {
        self.style.header_size()
    }
}

/// A set of byte-length formats indexed by message id.
#[derive(Debug, Clone, Default)]
pub struct InterfaceTable {
    elements: HashMap<u8, InterfaceElement>,
    /// Optional fallback used for ids that are not registered (useful for
    /// entity method ids 128..=254, which are dispatched by entity-def tables).
    unknown: Option<LengthStyle>,
    name: String,
}

impl InterfaceTable {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn named(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            ..Default::default()
        }
    }

    /// Treat unregistered ids as variable-length messages of the given width
    /// (BigWorld uses ids 128..=254 for entity method messages, which all use
    /// 2-byte variable lengths on client-facing interfaces).
    pub fn with_unknown_variable2(mut self) -> Self {
        self.unknown = Some(LengthStyle::Variable(2));
        self
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn insert(&mut self, element: InterfaceElement) -> &mut Self {
        self.elements.insert(element.id, element);
        self
    }

    pub fn get(&self, id: u8) -> Option<&InterfaceElement> {
        self.elements.get(&id)
    }

    pub fn entry(&self, id: u8) -> Option<&InterfaceElement> {
        self.get(id)
    }

    pub fn style(&self, id: u8) -> Option<LengthStyle> {
        self.elements
            .get(&id)
            .map(|e| e.style)
            .or(self.unknown)
    }

    pub fn len(&self) -> usize {
        self.elements.len()
    }

    pub fn is_empty(&self) -> bool {
        self.elements.is_empty()
    }

    pub fn iter(&self) -> impl Iterator<Item = &InterfaceElement> {
        self.elements.values()
    }
}

// ---------------------------------------------------------------------------
// LoginInterface
// ---------------------------------------------------------------------------

/// `LOGIN_VERSION` from `login_interface.hpp`. The retail 0.6.5 client actually
/// sends 57 (observed on the wire); the LoginApp accepts any value.
pub const LOGIN_VERSION: u32 = 56;
pub const OLDEST_SUPPORTED_CLIENT_LOGIN_VERSION: u32 = 56;

pub mod login_ids {
    /// `login( uint32 version, LogOnParams )`, VARIABLE(2).
    pub const LOGIN: u8 = 0;
    /// `probe()`, FIXED(0).
    pub const PROBE: u8 = 1;
}

pub fn login_interface() -> InterfaceTable {
    let mut t = InterfaceTable::named("LoginInterface");
    t.insert(InterfaceElement::variable2(login_ids::LOGIN, "login"));
    t.insert(InterfaceElement::fixed(login_ids::PROBE, 0, "probe"));
    t
}

// ---------------------------------------------------------------------------
// BaseAppExtInterface (client -> BaseApp)
// ---------------------------------------------------------------------------

pub mod baseapp_ext_ids {
    pub const BASE_APP_LOGIN: u8 = 0;
    pub const AUTHENTICATE: u8 = 1;
    pub const AVATAR_UPDATE_IMPLICIT: u8 = 2;
    pub const AVATAR_UPDATE_EXPLICIT: u8 = 3;
    pub const AVATAR_UPDATE_WARD_IMPLICIT: u8 = 4;
    pub const AVATAR_UPDATE_WARD_EXPLICIT: u8 = 5;
    pub const ACK_PHYSICS_CORRECTION: u8 = 6;
    pub const ACK_WARD_PHYSICS_CORRECTION: u8 = 7;
    pub const SWITCH_INTERFACE: u8 = 8;
    pub const REQUEST_ENTITY_UPDATE: u8 = 9;
    pub const ENABLE_ENTITIES: u8 = 10;
    pub const RESTORE_CLIENT_ACK: u8 = 11;
    pub const DISCONNECT_CLIENT: u8 = 12;
    pub const ENTITY_MESSAGE: u8 = 13;
}

pub fn baseapp_ext_interface() -> InterfaceTable {
    let mut t = InterfaceTable::named("BaseAppExtInterface");
    // Order matters: it matches the declaration order in
    // baseapp_ext_interface.hpp (which determines the ids).
    use LengthStyle::{Fixed, Variable};
    let decls: &[(u8, &str, LengthStyle)] = &[
        (baseapp_ext_ids::BASE_APP_LOGIN, "baseAppLogin", Variable(2)),
        (baseapp_ext_ids::AUTHENTICATE, "authenticate", Fixed(4)),
        (baseapp_ext_ids::AVATAR_UPDATE_IMPLICIT, "avatarUpdateImplicit", Fixed(12 + 3 + 1)),
        (baseapp_ext_ids::AVATAR_UPDATE_EXPLICIT, "avatarUpdateExplicit", Fixed(4 + 4 + 12 + 3 + 1 + 1)),
        (baseapp_ext_ids::AVATAR_UPDATE_WARD_IMPLICIT, "avatarUpdateWardImplicit", Fixed(4 + 12 + 3)),
        (baseapp_ext_ids::AVATAR_UPDATE_WARD_EXPLICIT, "avatarUpdateWardExplicit", Fixed(4 + 4 + 4 + 12 + 3 + 1)),
        (baseapp_ext_ids::ACK_PHYSICS_CORRECTION, "ackPhysicsCorrection", Fixed(1)),
        (baseapp_ext_ids::ACK_WARD_PHYSICS_CORRECTION, "ackWardPhysicsCorrection", Fixed(4 + 1)),
        (baseapp_ext_ids::SWITCH_INTERFACE, "switchInterface", Fixed(0)),
        (baseapp_ext_ids::REQUEST_ENTITY_UPDATE, "requestEntityUpdate", Variable(2)),
        (baseapp_ext_ids::ENABLE_ENTITIES, "enableEntities", Fixed(1)),
        (baseapp_ext_ids::RESTORE_CLIENT_ACK, "restoreClientAck", Fixed(4)),
        (baseapp_ext_ids::DISCONNECT_CLIENT, "disconnectClient", Fixed(1)),
        (baseapp_ext_ids::ENTITY_MESSAGE, "entityMessage", Variable(2)),
    ];
    for (id, name, style) in decls {
        t.insert(InterfaceElement {
            id: *id,
            style: *style,
            name,
        });
    }
    t.with_unknown_variable2()
}

// ---------------------------------------------------------------------------
// ClientInterface (BaseApp -> client)
// ---------------------------------------------------------------------------

pub mod client_ids {
    pub const AUTHENTICATE: u8 = 0;
    pub const BANDWIDTH_NOTIFICATION: u8 = 1;
    pub const UPDATE_FREQUENCY_NOTIFICATION: u8 = 2;
    pub const SET_GAME_TIME: u8 = 3;
    pub const RESET_ENTITIES: u8 = 4;
    pub const CREATE_BASE_PLAYER: u8 = 5;
    pub const CREATE_CELL_PLAYER: u8 = 6;
    pub const SPACE_DATA: u8 = 7;
    pub const CREATE_ENTITY: u8 = 8;
    pub const UPDATE_ENTITY: u8 = 9;
    pub const ENTER_AOI: u8 = 10;
    pub const ENTER_AOI_ON_VEHICLE: u8 = 11;
    pub const LEAVE_AOI: u8 = 12;
    pub const TICK_SYNC: u8 = 13;
    pub const RELATIVE_POSITION_REFERENCE: u8 = 14;
    pub const RELATIVE_POSITION: u8 = 15;
    pub const SET_VEHICLE: u8 = 16;
    /// First of 32 `avatarUpdate*` variants (ids 17..=48).
    pub const AVATAR_UPDATE_FIRST: u8 = 17;
    pub const AVATAR_UPDATE_LAST: u8 = 48;
    pub const DETAILED_POSITION: u8 = 49;
    pub const FORCED_POSITION: u8 = 50;
    pub const CONTROL_ENTITY: u8 = 51;
    pub const VOICE_DATA: u8 = 52;
    pub const RESTORE_CLIENT: u8 = 53;
    pub const SWITCH_BASE_APP: u8 = 54;
    pub const RESOURCE_HEADER: u8 = 55;
    pub const RESOURCE_FRAGMENT: u8 = 56;
    pub const LOGGED_OFF: u8 = 57;
    pub const ENTITY_MESSAGE: u8 = 58;
}

/// Calculate the fixed LengthStyle for an avatarUpdate message id (17..=48).
///
/// In BigWorld 2.0.1:
/// `id = 17 + (id_kind * 16) + (pos_kind * 4) + dir_kind`
/// - `id_size`: 4 (NoAlias) if id_kind == 0 else 1 (Alias)
/// - `pos_size`: [5, 4, 3, 0][pos_kind] (FullPos=PackedXYZ:5, OnChunk=PackedXHZ:4, OnGround=PackedXZ:3, NoPos=0)
/// - `dir_size`: [3, 2, 1, 0][dir_kind] (YawPitchRoll:3, YawPitch:2, Yaw:1, NoDir:0)
pub fn avatar_update_id_style(id: u8) -> Option<LengthStyle> {
    if (client_ids::AVATAR_UPDATE_FIRST..=client_ids::AVATAR_UPDATE_LAST).contains(&id) {
        let rel = id - client_ids::AVATAR_UPDATE_FIRST;
        let id_kind = rel / 16;
        let pos_kind = (rel % 16) / 4;
        let dir_kind = rel % 4;
        let id_size = if id_kind == 0 { 4 } else { 1 };
        let pos_size = [5, 4, 3, 0][pos_kind as usize];
        let dir_size = [3, 2, 1, 0][dir_kind as usize];
        Some(LengthStyle::Fixed((id_size + pos_size + dir_size) as u16))
    } else {
        None
    }
}

pub fn client_interface() -> InterfaceTable {
    let mut t = InterfaceTable::named("ClientInterface");
    use LengthStyle::{Fixed, Variable};
    let fixed: &[(u8, &str, u16)] = &[
        (client_ids::AUTHENTICATE, "authenticate", 4),
        (client_ids::BANDWIDTH_NOTIFICATION, "bandwidthNotification", 4),
        (client_ids::UPDATE_FREQUENCY_NOTIFICATION, "updateFrequencyNotification", 1),
        (client_ids::SET_GAME_TIME, "setGameTime", 4),
        (client_ids::RESET_ENTITIES, "resetEntities", 1),
        (client_ids::ENTER_AOI, "enterAoI", 4 + 1),
        (client_ids::ENTER_AOI_ON_VEHICLE, "enterAoIOnVehicle", 4 + 4 + 1),
        (client_ids::TICK_SYNC, "tickSync", 1),
        (client_ids::RELATIVE_POSITION_REFERENCE, "relativePositionReference", 1),
        (client_ids::RELATIVE_POSITION, "relativePosition", 12),
        (client_ids::SET_VEHICLE, "setVehicle", 8),
        (client_ids::DETAILED_POSITION, "detailedPosition", 4 + 12 + 12),
        (client_ids::FORCED_POSITION, "forcedPosition", 4 + 4 + 4 + 12 + 12),
        (client_ids::CONTROL_ENTITY, "controlEntity", 4 + 1),
        (client_ids::LOGGED_OFF, "loggedOff", 1),
    ];
    for (id, name, size) in fixed {
        t.insert(InterfaceElement {
            id: *id,
            style: Fixed(*size),
            name,
        });
    }
    let variable: &[(u8, &str)] = &[
        (client_ids::CREATE_BASE_PLAYER, "createBasePlayer"),
        (client_ids::CREATE_CELL_PLAYER, "createCellPlayer"),
        (client_ids::SPACE_DATA, "spaceData"),
        (client_ids::CREATE_ENTITY, "createEntity"),
        (client_ids::UPDATE_ENTITY, "updateEntity"),
        (client_ids::LEAVE_AOI, "leaveAoI"),
        (client_ids::VOICE_DATA, "voiceData"),
        (client_ids::RESTORE_CLIENT, "restoreClient"),
        (client_ids::SWITCH_BASE_APP, "switchBaseApp"),
        (client_ids::RESOURCE_HEADER, "resourceHeader"),
        (client_ids::RESOURCE_FRAGMENT, "resourceFragment"),
        (client_ids::ENTITY_MESSAGE, "entityMessage"),
    ];
    for (id, name) in variable {
        t.insert(InterfaceElement {
            id: *id,
            style: Variable(2),
            name,
        });
    }
    // The 32 avatarUpdate variants (2 id kinds x 4 position kinds x 4 direction kinds).
    // They are all UNRELIABLE fixed-size messages: EntityID/IDAlias + packed
    // position + packed direction.
    for id in client_ids::AVATAR_UPDATE_FIRST..=client_ids::AVATAR_UPDATE_LAST {
        let style = avatar_update_id_style(id).expect("valid avatar update id");
        t.insert(InterfaceElement {
            id,
            style,
            name: "avatarUpdate",
        });
    }
    t.with_unknown_variable2()
}

/// Maximum total packet size (BigWorld `PACKET_MAX_SIZE`).
pub const PACKET_CAPACITY: usize = PACKET_MAX_SIZE;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn baseapp_ids_match_declaration_order() {
        let t = baseapp_ext_interface();
        assert_eq!(
            t.get(baseapp_ext_ids::BASE_APP_LOGIN).unwrap().style,
            LengthStyle::Variable(2)
        );
        assert_eq!(
            t.get(baseapp_ext_ids::AUTHENTICATE).unwrap().style,
            LengthStyle::Fixed(4)
        );
        assert_eq!(
            t.get(baseapp_ext_ids::ENTITY_MESSAGE).unwrap().style,
            LengthStyle::Variable(2)
        );
        assert_eq!(t.get(13).unwrap().name, "entityMessage");
    }

    #[test]
    fn client_ids() {
        let t = client_interface();
        assert_eq!(t.get(0).unwrap().style, LengthStyle::Fixed(4));
        assert_eq!(t.get(5).unwrap().style, LengthStyle::Variable(2));
        assert_eq!(t.get(55).unwrap().name, "resourceHeader");
        assert_eq!(t.get(56).unwrap().name, "resourceFragment");
        assert_eq!(t.get(57).unwrap().style, LengthStyle::Fixed(1));
        assert_eq!(client_ids::AVATAR_UPDATE_LAST - client_ids::AVATAR_UPDATE_FIRST + 1, 32);
    }

    #[test]
    fn login_table() {
        let t = login_interface();
        assert_eq!(t.get(0).unwrap().style, LengthStyle::Variable(2));
        assert_eq!(t.get(1).unwrap().style, LengthStyle::Fixed(0));
        assert!(t.get(2).is_none());
    }

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
}
