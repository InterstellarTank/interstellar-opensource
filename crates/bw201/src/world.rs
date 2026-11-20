//! World of Tanks 0.6.5 / BigWorld 2.0.1 world and AoI wire codecs.
//!
//! Authoritative sources:
//! - `src/lib/network/msgtypes.hpp` (`PackedXZ`, `PackedXYZ`, `Yaw`, `YawPitch`, `YawPitchRoll`, `Coord`)
//! - `src/lib/network/basictypes.hpp` (`Direction3D`, `Address`, `EntityTypeID`, `EntityID`)
//! - `src/lib/connection/client_interface.hpp` & `common_client_interface.hpp`
//! - `src/lib/connection/server_connection.cpp`
//! - `src/lib/connection/baseapp_ext_interface.hpp`

use std::f32::consts::PI;

use crate::stream::{Address, BinaryReader, StreamError};

pub const SPACE_DATA_TOD_KEY: u16 = 0;
pub const SPACE_DATA_MAPPING_KEY_CLIENT_SERVER: u16 = 1;
pub const SPACE_DATA_WEATHER: u16 = 8192;
pub const NO_ID_ALIAS: u8 = 0xFF;

// ---------------------------------------------------------------------------
// Angle conversion helpers
// ---------------------------------------------------------------------------

/// Convert an angle in radians `[-pi, pi)` to an `int8` (BigWorld `angleToInt8`).
#[inline]
pub fn angle_to_int8(angle: f32) -> i8 {
    (angle * 128.0 / PI + 0.5).floor() as i8
}

/// Convert an `int8` to radians `[-pi, pi)` (BigWorld `int8ToAngle`).
#[inline]
pub fn int8_to_angle(angle: i8) -> f32 {
    (angle as f32) * (PI / 128.0)
}

/// Convert an angle in radians `[-pi/2, pi/2)` to an `int8` (BigWorld `halfAngleToInt8`).
#[inline]
pub fn half_angle_to_int8(angle: f32) -> i8 {
    let v = (angle * 254.0 / PI + 0.5).floor();
    v.clamp(-128.0, 127.0) as i8
}

/// Convert an `int8` to radians `[-pi/2, pi/2)` (BigWorld `int8ToHalfAngle`).
#[inline]
pub fn int8_to_half_angle(angle: i8) -> f32 {
    (angle as f32) * (PI / 254.0)
}

// ---------------------------------------------------------------------------
// Orientation and direction
// ---------------------------------------------------------------------------

/// 3D direction vector storing roll, pitch, yaw as IEEE 32-bit floats.
///
/// BigWorld `Direction3D` (`src/lib/network/basictypes.hpp:205`) streams in
/// `[roll, pitch, yaw]` order.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Direction3D {
    pub roll: f32,
    pub pitch: f32,
    pub yaw: f32,
}

impl Direction3D {
    pub const WIRE_SIZE: usize = 12;

    pub const ZERO: Self = Self {
        roll: 0.0,
        pitch: 0.0,
        yaw: 0.0,
    };

    pub fn new(roll: f32, pitch: f32, yaw: f32) -> Self {
        Self { roll, pitch, yaw }
    }

    pub fn write_to(&self, out: &mut Vec<u8>) {
        out.extend_from_slice(&self.roll.to_le_bytes());
        out.extend_from_slice(&self.pitch.to_le_bytes());
        out.extend_from_slice(&self.yaw.to_le_bytes());
    }

    pub fn read_from(r: &mut BinaryReader<'_>) -> Result<Self, StreamError> {
        let roll = r.f32()?;
        let pitch = r.f32()?;
        let yaw = r.f32()?;
        Ok(Self { roll, pitch, yaw })
    }
}

/// 8-bit packed yaw angle.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Yaw(pub i8);

impl Yaw {
    pub const WIRE_SIZE: usize = 1;

    pub fn encode(yaw: f32) -> Self {
        Self(angle_to_int8(yaw))
    }

    pub fn decode(&self) -> f32 {
        int8_to_angle(self.0)
    }

    pub fn write_to(&self, out: &mut Vec<u8>) {
        out.push(self.0 as u8);
    }

    pub fn read_from(r: &mut BinaryReader<'_>) -> Result<Self, StreamError> {
        Ok(Self(r.u8()? as i8))
    }
}

/// 8-bit packed yaw and pitch angles.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct YawPitch {
    pub yaw: i8,
    pub pitch: i8,
}

impl YawPitch {
    pub const WIRE_SIZE: usize = 2;

    pub fn encode(yaw: f32, pitch: f32) -> Self {
        Self {
            yaw: angle_to_int8(yaw),
            pitch: angle_to_int8(pitch),
        }
    }

    pub fn decode(&self) -> (f32, f32) {
        (int8_to_angle(self.yaw), int8_to_angle(self.pitch))
    }

    pub fn write_to(&self, out: &mut Vec<u8>) {
        out.push(self.yaw as u8);
        out.push(self.pitch as u8);
    }

    pub fn read_from(r: &mut BinaryReader<'_>) -> Result<Self, StreamError> {
        let yaw = r.u8()? as i8;
        let pitch = r.u8()? as i8;
        Ok(Self { yaw, pitch })
    }
}

/// 24-bit packed yaw, pitch, roll angles.
///
/// Note: pitch uses half-angle compression (`[-pi/2, pi/2)` via `halfAngleToInt8`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct YawPitchRoll {
    pub yaw: i8,
    pub pitch: i8,
    pub roll: i8,
}

impl YawPitchRoll {
    pub const WIRE_SIZE: usize = 3;

    pub fn encode(angles: [f32; 3]) -> Self {
        Self {
            yaw: angle_to_int8(angles[0]),
            pitch: half_angle_to_int8(angles[1]),
            roll: angle_to_int8(angles[2]),
        }
    }

    pub fn new(yaw: f32, pitch: f32, roll: f32) -> Self {
        Self::encode([yaw, pitch, roll])
    }

    pub fn decode(&self) -> (f32, f32, f32) {
        (
            int8_to_angle(self.yaw),
            int8_to_half_angle(self.pitch),
            int8_to_angle(self.roll),
        )
    }

    pub fn write_to(&self, out: &mut Vec<u8>) {
        out.push(self.yaw as u8);
        out.push(self.pitch as u8);
        out.push(self.roll as u8);
    }

    pub fn read_from(r: &mut BinaryReader<'_>) -> Result<Self, StreamError> {
        let yaw = r.u8()? as i8;
        let pitch = r.u8()? as i8;
        let roll = r.u8()? as i8;
        Ok(Self { yaw, pitch, roll })
    }
}

/// 24-bit packed yaw, pitch, roll angles used in `createEntity` where all three
/// angles are encoded with `angle_to_int8` (`[-pi, pi)` via `pi/128`), matching
/// BigWorld `ServerConnection::createEntity` (`server_connection.cpp:1964`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EntityDir {
    pub yaw: i8,
    pub pitch: i8,
    pub roll: i8,
}

impl EntityDir {
    pub const WIRE_SIZE: usize = 3;

    pub fn encode(angles: [f32; 3]) -> Self {
        Self {
            yaw: angle_to_int8(angles[0]),
            pitch: angle_to_int8(angles[1]),
            roll: angle_to_int8(angles[2]),
        }
    }

    pub fn new(yaw: f32, pitch: f32, roll: f32) -> Self {
        Self::encode([yaw, pitch, roll])
    }

    pub fn decode(&self) -> (f32, f32, f32) {
        (
            int8_to_angle(self.yaw),
            int8_to_angle(self.pitch),
            int8_to_angle(self.roll),
        )
    }

    pub fn write_to(&self, out: &mut Vec<u8>) {
        out.push(self.yaw as u8);
        out.push(self.pitch as u8);
        out.push(self.roll as u8);
    }

    pub fn read_from(r: &mut BinaryReader<'_>) -> Result<Self, StreamError> {
        let yaw = r.u8()? as i8;
        let pitch = r.u8()? as i8;
        let roll = r.u8()? as i8;
        Ok(Self { yaw, pitch, roll })
    }
}

impl From<[i8; 3]> for EntityDir {
    fn from([yaw, pitch, roll]: [i8; 3]) -> Self {
        Self { yaw, pitch, roll }
    }
}

impl From<EntityDir> for [i8; 3] {
    fn from(dir: EntityDir) -> Self {
        [dir.yaw, dir.pitch, dir.roll]
    }
}

// ---------------------------------------------------------------------------
// Packed coordinates
// ---------------------------------------------------------------------------

/// 24-bit packed horizontal position (X, Z).
///
/// Implements BigWorld `PackedXZ` (`msgtypes.hpp:230-330`):
/// - 12 bits for X, 12 bits for Z.
/// - Packed into 3 bytes LE via `BW_PACK3`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PackedXZ {
    pub data: [u8; 3],
}

impl PackedXZ {
    pub const WIRE_SIZE: usize = 3;

    pub fn encode(pos: [f32; 2]) -> Self {
        let x_val = pos[0];
        let z_val = pos[1];

        let add_values = [2.0f32, -2.0f32];
        let x_int_neg = (x_val.to_bits() & 0x8000_0000) != 0;
        let z_int_neg = (z_val.to_bits() & 0x8000_0000) != 0;

        let x_biased = x_val + add_values[if x_int_neg { 1 } else { 0 }];
        let z_biased = z_val + add_values[if z_int_neg { 1 } else { 0 }];

        let x_uint = x_biased.to_bits();
        let z_uint = z_biased.to_bits();

        let mut result: u32 = 0;

        let x_ceil = ((x_uint & 0x7c00_0000) != 0x4000_0000) || ((x_uint & 0x3ff_c000) == 0x3ff_c000);
        if x_ceil {
            result |= 0x7ff000;
        }
        let z_ceil = ((z_uint & 0x7c00_0000) != 0x4000_0000) || ((z_uint & 0x3ff_c000) == 0x3ff_c000);
        if z_ceil {
            result |= 0x0007ff;
        }

        result |= ((x_uint >> 3) & 0x7ff000).wrapping_add((x_uint & 0x4000) >> 2);
        result |= ((z_uint >> 15) & 0x0007ff).wrapping_add((z_uint & 0x4000) >> 14);

        result &= 0x7ff7ff;

        result |= (x_uint >> 8) & 0x800000;
        result |= (z_uint >> 20) & 0x000800;

        Self {
            data: [
                result as u8,
                (result >> 8) as u8,
                (result >> 16) as u8,
            ],
        }
    }

    pub fn decode(&self) -> [f32; 2] {
        let data = (self.data[0] as u32)
            | ((self.data[1] as u32) << 8)
            | ((self.data[2] as u32) << 16);

        let mut xu = 0x4000_0000u32;
        let mut zu = 0x4000_0000u32;

        xu |= (data & 0x7ff000) << 3;
        zu |= (data & 0x0007ff) << 15;

        let mut x_float = f32::from_bits(xu) - 2.0;
        let mut z_float = f32::from_bits(zu) - 2.0;

        let x_sign = (data & 0x800000) << 8;
        let z_sign = (data & 0x000800) << 20;

        x_float = f32::from_bits(x_float.to_bits() | x_sign);
        z_float = f32::from_bits(z_float.to_bits() | z_sign);

        [x_float, z_float]
    }

    pub fn write_to(&self, out: &mut Vec<u8>) {
        out.extend_from_slice(&self.data);
    }

    pub fn read_from(r: &mut BinaryReader<'_>) -> Result<Self, StreamError> {
        let raw = r.raw(3)?;
        Ok(Self {
            data: [raw[0], raw[1], raw[2]],
        })
    }
}

/// 40-bit packed 3D coordinate (X, Y, Z).
///
/// Implements BigWorld `PackedXYZ` (`msgtypes.hpp:425-586`):
/// - `PackedXZ` (3 bytes LE) for X and Z.
/// - 16-bit LE float (4-bit exponent, 11-bit mantissa, sign bit) for Y.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PackedXYZ {
    pub xz: PackedXZ,
    pub y_data: [u8; 2],
}

impl PackedXYZ {
    pub const WIRE_SIZE: usize = 5;

    pub fn encode(pos: [f32; 3]) -> Self {
        let xz = PackedXZ::encode([pos[0], pos[2]]);

        let y_val = pos[1];
        let y_int_neg = (y_val.to_bits() & 0x8000_0000) != 0;
        let add_val = if y_int_neg { -2.0f32 } else { 2.0f32 };
        let y_biased = y_val + add_val;
        let y_uint = y_biased.to_bits();

        let mut y_int = ((y_uint >> 12) & 0x7fff) as u16;
        y_int |= ((y_uint >> 16) & 0x8000) as u16;

        Self {
            xz,
            y_data: y_int.to_le_bytes(),
        }
    }

    pub fn decode(&self) -> [f32; 3] {
        let [x, z] = self.xz.decode();

        let y_data_int = u16::from_le_bytes(self.y_data);
        let mut yu = 0x4000_0000u32;
        yu |= ((y_data_int & 0x7fff) as u32) << 12;

        let mut y_float = f32::from_bits(yu) - 2.0;
        let y_sign = ((y_data_int & 0x8000) as u32) << 16;
        y_float = f32::from_bits(y_float.to_bits() | y_sign);

        [x, y_float, z]
    }

    pub fn write_to(&self, out: &mut Vec<u8>) {
        self.xz.write_to(out);
        out.extend_from_slice(&self.y_data);
    }

    pub fn read_from(r: &mut BinaryReader<'_>) -> Result<Self, StreamError> {
        let xz = PackedXZ::read_from(r)?;
        let y_raw = r.raw(2)?;
        Ok(Self {
            xz,
            y_data: [y_raw[0], y_raw[1]],
        })
    }
}

// ---------------------------------------------------------------------------
// World & Entity lifecycle messages
// ---------------------------------------------------------------------------

/// Client message `resetEntities` (id 4, Fixed(1)).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ResetEntities {
    pub keep_player_on_base: u8,
}

impl ResetEntities {
    pub fn write_to(&self, out: &mut Vec<u8>) {
        out.push(self.keep_player_on_base);
    }

    pub fn read_from(r: &mut BinaryReader<'_>) -> Result<Self, StreamError> {
        Ok(Self {
            keep_player_on_base: r.u8()?,
        })
    }
}

/// Client message `createBasePlayer` (id 5, Variable(2)).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CreateBasePlayer {
    pub entity_id: i32,
    pub entity_type: u16,
    pub value: Vec<u8>,
}

impl CreateBasePlayer {
    pub fn write_to(&self, out: &mut Vec<u8>) {
        out.extend_from_slice(&self.entity_id.to_le_bytes());
        out.extend_from_slice(&self.entity_type.to_le_bytes());
        out.extend_from_slice(&self.value);
    }

    pub fn read_from(r: &mut BinaryReader<'_>) -> Result<Self, StreamError> {
        let entity_id = r.i32()?;
        let entity_type = r.u16()?;
        let value = r.rest_vec();
        Ok(Self {
            entity_id,
            entity_type,
            value,
        })
    }
}

/// Client message `createCellPlayer` (id 6, Variable(2)).
#[derive(Debug, Clone, PartialEq)]
pub struct CreateCellPlayer {
    pub space_id: i32,
    pub vehicle_id: i32,
    pub pos: [f32; 3],
    pub dir: Direction3D,
    pub cell_data: Vec<u8>,
}

impl CreateCellPlayer {
    pub fn write_to(&self, out: &mut Vec<u8>) {
        out.extend_from_slice(&self.space_id.to_le_bytes());
        out.extend_from_slice(&self.vehicle_id.to_le_bytes());
        for p in self.pos {
            out.extend_from_slice(&p.to_le_bytes());
        }
        self.dir.write_to(out);
        out.extend_from_slice(&self.cell_data);
    }

    pub fn read_from(r: &mut BinaryReader<'_>) -> Result<Self, StreamError> {
        let space_id = r.i32()?;
        let vehicle_id = r.i32()?;
        let pos = [r.f32()?, r.f32()?, r.f32()?];
        let dir = Direction3D::read_from(r)?;
        let cell_data = r.rest_vec();
        Ok(Self {
            space_id,
            vehicle_id,
            pos,
            dir,
            cell_data,
        })
    }
}

/// Client message `spaceData` (id 7, Variable(2)).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpaceData {
    pub space_id: i32,
    pub entry_id: Address,
    pub key: u16,
    pub payload: Vec<u8>,
}

impl SpaceData {
    pub fn mapping(space_id: i32, entry_id: Address, matrix_4x4: [f32; 16], path: &str) -> Self {
        let mut payload = Vec::with_capacity(64 + path.len());
        for val in matrix_4x4 {
            payload.extend_from_slice(&val.to_le_bytes());
        }
        payload.extend_from_slice(path.as_bytes());
        Self {
            space_id,
            entry_id,
            key: SPACE_DATA_MAPPING_KEY_CLIENT_SERVER,
            payload,
        }
    }

    pub fn write_to(&self, out: &mut Vec<u8>) {
        out.extend_from_slice(&self.space_id.to_le_bytes());
        self.entry_id.write_to(out);
        out.extend_from_slice(&self.key.to_le_bytes());
        out.extend_from_slice(&self.payload);
    }

    pub fn read_from(r: &mut BinaryReader<'_>) -> Result<Self, StreamError> {
        let space_id = r.i32()?;
        let entry_id = Address::read_from(r)?;
        let key = r.u16()?;
        let payload = r.rest_vec();
        Ok(Self {
            space_id,
            entry_id,
            key,
            payload,
        })
    }
}

/// Helper representation for space geometry mapping (`key = SPACE_DATA_MAPPING_KEY_CLIENT_SERVER`).
#[derive(Debug, Clone, PartialEq)]
pub struct SpaceDataMapping {
    pub matrix: [f32; 16],
    pub path: String,
}

impl SpaceDataMapping {
    pub fn decode(payload: &[u8]) -> Result<Self, StreamError> {
        let mut r = BinaryReader::new(payload);
        let mut matrix = [0.0f32; 16];
        for item in &mut matrix {
            *item = r.f32()?;
        }
        let path = String::from_utf8_lossy(r.rest()).to_string();
        Ok(Self { matrix, path })
    }

    pub fn encode(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(64 + self.path.len());
        for val in self.matrix {
            out.extend_from_slice(&val.to_le_bytes());
        }
        out.extend_from_slice(self.path.as_bytes());
        out
    }
}

/// Client message `enterAoI` (id 10, Fixed(5)).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EnterAoI {
    pub id: i32,
    pub id_alias: u8,
}

impl EnterAoI {
    pub const WIRE_SIZE: usize = 5;

    pub fn new(id: i32, id_alias: u8) -> Self {
        Self { id, id_alias }
    }

    pub fn write_to(&self, out: &mut Vec<u8>) {
        out.extend_from_slice(&self.id.to_le_bytes());
        out.push(self.id_alias);
    }

    pub fn read_from(r: &mut BinaryReader<'_>) -> Result<Self, StreamError> {
        let id = r.i32()?;
        let id_alias = r.u8()?;
        Ok(Self { id, id_alias })
    }
}

/// Client message `enterAoIOnVehicle` (id 11, Fixed(9)).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EnterAoIOnVehicle {
    pub id: i32,
    pub vehicle_id: i32,
    pub id_alias: u8,
}

impl EnterAoIOnVehicle {
    pub const WIRE_SIZE: usize = 9;

    pub fn write_to(&self, out: &mut Vec<u8>) {
        out.extend_from_slice(&self.id.to_le_bytes());
        out.extend_from_slice(&self.vehicle_id.to_le_bytes());
        out.push(self.id_alias);
    }

    pub fn read_from(r: &mut BinaryReader<'_>) -> Result<Self, StreamError> {
        let id = r.i32()?;
        let vehicle_id = r.i32()?;
        let id_alias = r.u8()?;
        Ok(Self {
            id,
            vehicle_id,
            id_alias,
        })
    }
}

/// Client message `leaveAoI` (id 12, Variable(2)).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LeaveAoI {
    pub id: i32,
    pub cache_stamps: Vec<i32>,
}

impl LeaveAoI {
    pub fn write_to(&self, out: &mut Vec<u8>) {
        out.extend_from_slice(&self.id.to_le_bytes());
        for stamp in &self.cache_stamps {
            out.extend_from_slice(&stamp.to_le_bytes());
        }
    }

    pub fn read_from(r: &mut BinaryReader<'_>) -> Result<Self, StreamError> {
        let id = r.i32()?;
        let mut cache_stamps = Vec::new();
        while !r.is_empty() {
            cache_stamps.push(r.i32()?);
        }
        Ok(Self { id, cache_stamps })
    }
}

/// Client message `createEntity` (id 8, Variable(2)).
#[derive(Debug, Clone, PartialEq)]
pub struct CreateEntity {
    pub compression: u8,
    pub id: i32,
    pub entity_type: u16,
    pub pos: [f32; 3],
    pub dir: EntityDir,
    pub properties: Vec<u8>,
}

impl CreateEntity {
    pub fn write_to(&self, out: &mut Vec<u8>) {
        out.push(self.compression);
        out.extend_from_slice(&self.id.to_le_bytes());
        out.extend_from_slice(&self.entity_type.to_le_bytes());
        for p in self.pos {
            out.extend_from_slice(&p.to_le_bytes());
        }
        self.dir.write_to(out);
        out.extend_from_slice(&self.properties);
    }

    pub fn read_from(r: &mut BinaryReader<'_>) -> Result<Self, StreamError> {
        let compression = r.u8()?;
        let id = r.i32()?;
        let entity_type = r.u16()?;
        let pos = [r.f32()?, r.f32()?, r.f32()?];
        let dir = EntityDir::read_from(r)?;
        let properties = r.rest_vec();
        Ok(Self {
            compression,
            id,
            entity_type,
            pos,
            dir,
            properties,
        })
    }
}

/// Client message `updateEntity` (id 9, Variable(2)).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UpdateEntity {
    pub id: i32,
    pub properties: Vec<u8>,
}

impl UpdateEntity {
    pub fn write_to(&self, out: &mut Vec<u8>) {
        out.extend_from_slice(&self.id.to_le_bytes());
        out.extend_from_slice(&self.properties);
    }

    pub fn read_from(r: &mut BinaryReader<'_>) -> Result<Self, StreamError> {
        let id = r.i32()?;
        let properties = r.rest_vec();
        Ok(Self { id, properties })
    }
}

/// Client message `tickSync` (id 13, Fixed(1)).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TickSync {
    pub tick_byte: u8,
}

impl TickSync {
    pub const WIRE_SIZE: usize = 1;

    pub fn write_to(&self, out: &mut Vec<u8>) {
        out.push(self.tick_byte);
    }

    pub fn read_from(r: &mut BinaryReader<'_>) -> Result<Self, StreamError> {
        Ok(Self {
            tick_byte: r.u8()?,
        })
    }
}

/// Client message `setVehicle` (id 16, Fixed(8)).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SetVehicle {
    pub passenger_id: i32,
    pub vehicle_id: i32,
}

impl SetVehicle {
    pub const WIRE_SIZE: usize = 8;

    pub fn write_to(&self, out: &mut Vec<u8>) {
        out.extend_from_slice(&self.passenger_id.to_le_bytes());
        out.extend_from_slice(&self.vehicle_id.to_le_bytes());
    }

    pub fn read_from(r: &mut BinaryReader<'_>) -> Result<Self, StreamError> {
        let passenger_id = r.i32()?;
        let vehicle_id = r.i32()?;
        Ok(Self {
            passenger_id,
            vehicle_id,
        })
    }
}

/// Client message `detailedPosition` (id 49, Fixed(28)).
#[derive(Debug, Clone, PartialEq)]
pub struct DetailedPosition {
    pub id: i32,
    pub position: [f32; 3],
    pub direction: Direction3D,
}

impl DetailedPosition {
    pub const WIRE_SIZE: usize = 28;

    pub fn write_to(&self, out: &mut Vec<u8>) {
        out.extend_from_slice(&self.id.to_le_bytes());
        for p in self.position {
            out.extend_from_slice(&p.to_le_bytes());
        }
        self.direction.write_to(out);
    }

    pub fn read_from(r: &mut BinaryReader<'_>) -> Result<Self, StreamError> {
        let id = r.i32()?;
        let position = [r.f32()?, r.f32()?, r.f32()?];
        let direction = Direction3D::read_from(r)?;
        Ok(Self {
            id,
            position,
            direction,
        })
    }
}

/// Client message `forcedPosition` (id 50, Fixed(36)).
#[derive(Debug, Clone, PartialEq)]
pub struct ForcedPosition {
    pub id: i32,
    pub space_id: i32,
    pub vehicle_id: i32,
    pub position: [f32; 3],
    pub direction: Direction3D,
}

impl ForcedPosition {
    pub const WIRE_SIZE: usize = 36;

    pub fn write_to(&self, out: &mut Vec<u8>) {
        out.extend_from_slice(&self.id.to_le_bytes());
        out.extend_from_slice(&self.space_id.to_le_bytes());
        out.extend_from_slice(&self.vehicle_id.to_le_bytes());
        for p in self.position {
            out.extend_from_slice(&p.to_le_bytes());
        }
        self.direction.write_to(out);
    }

    pub fn read_from(r: &mut BinaryReader<'_>) -> Result<Self, StreamError> {
        let id = r.i32()?;
        let space_id = r.i32()?;
        let vehicle_id = r.i32()?;
        let position = [r.f32()?, r.f32()?, r.f32()?];
        let direction = Direction3D::read_from(r)?;
        Ok(Self {
            id,
            space_id,
            vehicle_id,
            position,
            direction,
        })
    }
}

/// Client message `controlEntity` (id 51, Fixed(5)).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ControlEntity {
    pub id: i32,
    pub on: bool,
}

impl ControlEntity {
    pub const WIRE_SIZE: usize = 5;

    pub fn write_to(&self, out: &mut Vec<u8>) {
        out.extend_from_slice(&self.id.to_le_bytes());
        out.push(if self.on { 1 } else { 0 });
    }

    pub fn read_from(r: &mut BinaryReader<'_>) -> Result<Self, StreamError> {
        let id = r.i32()?;
        let on = r.bool()?;
        Ok(Self { id, on })
    }
}

// ---------------------------------------------------------------------------
// Avatar update wire types
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IdKind {
    NoAlias(i32),
    Alias(u8),
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum PosKind {
    FullPos(PackedXYZ),
    OnChunk { xz: PackedXZ, height: i8 },
    OnGround(PackedXZ),
    NoPos,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DirKind {
    YawPitchRoll(YawPitchRoll),
    YawPitch(YawPitch),
    Yaw(Yaw),
    NoDir,
}

/// Server-to-Client movement update (`ClientInterface::avatarUpdate*`, ids 17..=48).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct S2CAvatarUpdate {
    pub id: IdKind,
    pub pos: PosKind,
    pub dir: DirKind,
}

impl S2CAvatarUpdate {
    pub fn message_id(&self) -> u8 {
        let id_kind = match self.id {
            IdKind::NoAlias(_) => 0u8,
            IdKind::Alias(_) => 1u8,
        };
        let pos_kind = match self.pos {
            PosKind::FullPos(_) => 0u8,
            PosKind::OnChunk { .. } => 1u8,
            PosKind::OnGround(_) => 2u8,
            PosKind::NoPos => 3u8,
        };
        let dir_kind = match self.dir {
            DirKind::YawPitchRoll(_) => 0u8,
            DirKind::YawPitch(_) => 1u8,
            DirKind::Yaw(_) => 2u8,
            DirKind::NoDir => 3u8,
        };
        17 + (id_kind * 16) + (pos_kind * 4) + dir_kind
    }

    pub fn encode(&self) -> (u8, Vec<u8>) {
        let msg_id = self.message_id();
        let mut out = Vec::new();
        match self.id {
            IdKind::NoAlias(id) => out.extend_from_slice(&id.to_le_bytes()),
            IdKind::Alias(alias) => out.push(alias),
        }
        match self.pos {
            PosKind::FullPos(p) => p.write_to(&mut out),
            PosKind::OnChunk { xz, height } => {
                xz.write_to(&mut out);
                out.push(height as u8);
            }
            PosKind::OnGround(xz) => xz.write_to(&mut out),
            PosKind::NoPos => {}
        }
        match self.dir {
            DirKind::YawPitchRoll(ypr) => ypr.write_to(&mut out),
            DirKind::YawPitch(yp) => yp.write_to(&mut out),
            DirKind::Yaw(y) => y.write_to(&mut out),
            DirKind::NoDir => {}
        }
        (msg_id, out)
    }

    pub fn decode(msg_id: u8, data: &[u8]) -> Result<Self, StreamError> {
        if !(17..=48).contains(&msg_id) {
            return Err(StreamError::UnexpectedEof);
        }
        let rel = msg_id - 17;
        let id_kind = rel / 16;
        let pos_kind = (rel % 16) / 4;
        let dir_kind = rel % 4;

        let mut r = BinaryReader::new(data);
        let id = match id_kind {
            0 => IdKind::NoAlias(r.i32()?),
            _ => IdKind::Alias(r.u8()?),
        };
        let pos = match pos_kind {
            0 => PosKind::FullPos(PackedXYZ::read_from(&mut r)?),
            1 => {
                let xz = PackedXZ::read_from(&mut r)?;
                let height = r.u8()? as i8;
                PosKind::OnChunk { xz, height }
            }
            2 => PosKind::OnGround(PackedXZ::read_from(&mut r)?),
            _ => PosKind::NoPos,
        };
        let dir = match dir_kind {
            0 => DirKind::YawPitchRoll(YawPitchRoll::read_from(&mut r)?),
            1 => DirKind::YawPitch(YawPitch::read_from(&mut r)?),
            2 => DirKind::Yaw(Yaw::read_from(&mut r)?),
            _ => DirKind::NoDir,
        };
        Ok(Self { id, pos, dir })
    }
}

/// Client-to-Server avatar movement packet (`BaseAppExtInterface`, ids 2..=5).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct C2SAvatarUpdate {
    pub pos: [f32; 3],
    pub dir: YawPitchRoll,
    pub ref_num: u8,
    pub space_id: Option<i32>,
    pub vehicle_id: Option<i32>,
    pub on_ground: Option<bool>,
    pub ward: Option<i32>,
}

impl C2SAvatarUpdate {
    pub fn decode(msg_id: u8, data: &[u8]) -> Result<Self, StreamError> {
        match msg_id {
            2 => Self::decode_implicit(data),
            3 => Self::decode_explicit(data),
            4 => Self::decode_ward_implicit(data),
            5 => Self::decode_ward_explicit(data),
            _ => Err(StreamError::UnexpectedEof),
        }
    }

    pub fn decode_implicit(data: &[u8]) -> Result<Self, StreamError> {
        let mut r = BinaryReader::new(data);
        let pos = [r.f32()?, r.f32()?, r.f32()?];
        let dir = YawPitchRoll::read_from(&mut r)?;
        let ref_num = r.u8()?;
        Ok(Self {
            pos,
            dir,
            ref_num,
            space_id: None,
            vehicle_id: None,
            on_ground: None,
            ward: None,
        })
    }

    pub fn decode_explicit(data: &[u8]) -> Result<Self, StreamError> {
        let mut r = BinaryReader::new(data);
        let space_id = r.i32()?;
        let vehicle_id = r.i32()?;
        let pos = [r.f32()?, r.f32()?, r.f32()?];
        let dir = YawPitchRoll::read_from(&mut r)?;
        let on_ground = r.bool()?;
        let ref_num = r.u8()?;
        Ok(Self {
            pos,
            dir,
            ref_num,
            space_id: Some(space_id),
            vehicle_id: Some(vehicle_id),
            on_ground: Some(on_ground),
            ward: None,
        })
    }

    pub fn decode_ward_implicit(data: &[u8]) -> Result<Self, StreamError> {
        let mut r = BinaryReader::new(data);
        let ward = r.i32()?;
        let pos = [r.f32()?, r.f32()?, r.f32()?];
        let dir = YawPitchRoll::read_from(&mut r)?;
        Ok(Self {
            pos,
            dir,
            ref_num: 0,
            space_id: None,
            vehicle_id: None,
            on_ground: None,
            ward: Some(ward),
        })
    }

    pub fn decode_ward_explicit(data: &[u8]) -> Result<Self, StreamError> {
        let mut r = BinaryReader::new(data);
        let ward = r.i32()?;
        let space_id = r.i32()?;
        let vehicle_id = r.i32()?;
        let pos = [r.f32()?, r.f32()?, r.f32()?];
        let dir = YawPitchRoll::read_from(&mut r)?;
        let on_ground = r.bool()?;
        Ok(Self {
            pos,
            dir,
            ref_num: 0,
            space_id: Some(space_id),
            vehicle_id: Some(vehicle_id),
            on_ground: Some(on_ground),
            ward: Some(ward),
        })
    }

    pub fn encode_implicit(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(16);
        for p in self.pos {
            out.extend_from_slice(&p.to_le_bytes());
        }
        self.dir.write_to(&mut out);
        out.push(self.ref_num);
        out
    }

    pub fn encode_explicit(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(25);
        out.extend_from_slice(&self.space_id.unwrap_or(0).to_le_bytes());
        out.extend_from_slice(&self.vehicle_id.unwrap_or(0).to_le_bytes());
        for p in self.pos {
            out.extend_from_slice(&p.to_le_bytes());
        }
        self.dir.write_to(&mut out);
        out.push(if self.on_ground.unwrap_or(false) { 1 } else { 0 });
        out.push(self.ref_num);
        out
    }
}

/// Client-to-Server message `requestEntityUpdate` (`BaseAppExtInterface::requestEntityUpdate`, id 9, Variable(2)).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RequestEntityUpdate {
    pub id: i32,
    pub cache_stamps: Vec<i32>,
}

impl RequestEntityUpdate {
    pub fn write_to(&self, out: &mut Vec<u8>) {
        out.extend_from_slice(&self.id.to_le_bytes());
        for stamp in &self.cache_stamps {
            out.extend_from_slice(&stamp.to_le_bytes());
        }
    }

    pub fn read_from(r: &mut BinaryReader<'_>) -> Result<Self, StreamError> {
        let id = r.i32()?;
        let mut cache_stamps = Vec::new();
        while !r.is_empty() {
            cache_stamps.push(r.i32()?);
        }
        Ok(Self { id, cache_stamps })
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn packed_xyz_and_angles_preserve_signs_and_limits() {
        let p = PackedXYZ::encode([-450.5, 123.25, 498.75]);
        let out = p.decode();
        assert!((out[0] - (-450.5)).abs() < 2.1);
        assert!((out[1] - 123.25).abs() < 0.25);
        assert!((out[2] - 498.75).abs() < 2.1);
        let ypr = YawPitchRoll::encode([1.5, -0.4, 0.2]);
        let (yaw, pitch, roll) = ypr.decode();
        assert!((yaw - 1.5).abs() < 0.03);
        assert!((pitch - (-0.4)).abs() < 0.02);
        assert!((roll - 0.2).abs() < 0.03);
    }

    #[test]
    fn multi_element_bundle_decode_avatar_tick_update() {
        let cli = crate::interfaces::client_interface();
        let mut enc = crate::bundle::ElementEncoder::new();

        // 1. S2CAvatarUpdate
        let av_update = S2CAvatarUpdate {
            id: IdKind::NoAlias(42),
            pos: PosKind::FullPos(PackedXYZ::encode([10.0, 20.0, 30.0])),
            dir: DirKind::YawPitchRoll(YawPitchRoll::encode([0.5, 0.2, -0.1])),
        };
        let (av_id, av_bytes) = av_update.encode();
        enc.message(av_id, cli.entry(av_id).unwrap().style, &av_bytes);

        // 2. TickSync
        let tick = TickSync { tick_byte: 123 };
        let mut tick_bytes = Vec::new();
        tick.write_to(&mut tick_bytes);
        enc.message(
            crate::interfaces::client_ids::TICK_SYNC,
            cli.entry(crate::interfaces::client_ids::TICK_SYNC).unwrap().style,
            &tick_bytes,
        );

        // 3. UpdateEntity
        let update = UpdateEntity {
            id: 99,
            properties: vec![1, 2, 3, 4, 5],
        };
        let mut upd_bytes = Vec::new();
        update.write_to(&mut upd_bytes);
        enc.message(
            crate::interfaces::client_ids::UPDATE_ENTITY,
            cli.entry(crate::interfaces::client_ids::UPDATE_ENTITY).unwrap().style,
            &upd_bytes,
        );

        let packet = enc.finish();
        let raw = packet.encode(crate::packet::Endian::Little);
        let decoded_pkt = crate::packet::Packet::decode(&raw, crate::packet::Endian::Little).unwrap();
        let msgs = crate::bundle::decode_messages(&decoded_pkt, &cli, crate::packet::Endian::Little).unwrap();

        assert_eq!(msgs.len(), 3);
        assert_eq!(msgs[0].id, av_id);
        let dec_av = S2CAvatarUpdate::decode(msgs[0].id, &msgs[0].data).unwrap();
        assert_eq!(dec_av.id, IdKind::NoAlias(42));

        assert_eq!(msgs[1].id, crate::interfaces::client_ids::TICK_SYNC);
        let dec_tick = TickSync::read_from(&mut BinaryReader::new(&msgs[1].data)).unwrap();
        assert_eq!(dec_tick.tick_byte, 123);

        assert_eq!(msgs[2].id, crate::interfaces::client_ids::UPDATE_ENTITY);
        let dec_upd = UpdateEntity::read_from(&mut BinaryReader::new(&msgs[2].data)).unwrap();
        assert_eq!(dec_upd.id, 99);
        assert_eq!(dec_upd.properties, vec![1, 2, 3, 4, 5]);
    }

    #[test]
    fn space_data_mapping_roundtrip() {
        let mut matrix = [0.0f32; 16];
        for i in 0..4 {
            matrix[i * 5] = 1.0;
        }
        let space_id = 42;
        let entry_id = Address::new([10, 0, 0, 1], 20016, 0);
        let path = "spaces/01_karelia";
        let sd = SpaceData::mapping(space_id, entry_id, matrix, path);

        let mut bytes = Vec::new();
        sd.write_to(&mut bytes);

        let mut reader = BinaryReader::new(&bytes);
        let decoded = SpaceData::read_from(&mut reader).unwrap();
        assert_eq!(decoded.space_id, 42);
        assert_eq!(decoded.entry_id, entry_id);
        assert_eq!(decoded.key, SPACE_DATA_MAPPING_KEY_CLIENT_SERVER);

        let mapping = SpaceDataMapping::decode(&decoded.payload).unwrap();
        assert_eq!(mapping.matrix, matrix);
        assert_eq!(mapping.path, path);
    }

    #[test]
    fn c2s_avatar_update_roundtrip() {
        let update = C2SAvatarUpdate {
            pos: [100.5, 20.25, -300.75],
            dir: YawPitchRoll::encode([1.2, -0.3, 0.4]),
            ref_num: 7,
            space_id: Some(1),
            vehicle_id: Some(2),
            on_ground: Some(true),
            ward: None,
        };
        let explicit_bytes = update.encode_explicit();
        assert_eq!(explicit_bytes.len(), 25);
        let dec_explicit = C2SAvatarUpdate::decode(3, &explicit_bytes).unwrap();
        assert_eq!(dec_explicit.pos, update.pos);
        assert_eq!(dec_explicit.ref_num, 7);
        assert_eq!(dec_explicit.space_id, Some(1));
        assert_eq!(dec_explicit.vehicle_id, Some(2));
        assert_eq!(dec_explicit.on_ground, Some(true));

        let implicit_bytes = update.encode_implicit();
        assert_eq!(implicit_bytes.len(), 16);
        let dec_implicit = C2SAvatarUpdate::decode(2, &implicit_bytes).unwrap();
        assert_eq!(dec_implicit.pos, update.pos);
        assert_eq!(dec_implicit.ref_num, 7);
    }

    #[test]
    fn create_entity_pitch_uses_angle_to_int8_pi_128() {
        let angles = [0.8f32, -0.4f32, 0.5f32];
        let dir = EntityDir::encode(angles);
        // Pitch with angle_to_int8(-0.4):
        // (-0.4 * 128 / PI + 0.5).floor() = (-16.297 + 0.5).floor() = -16
        // With half_angle_to_int8(-0.4), it would be:
        // (-0.4 * 254 / PI + 0.5).floor() = (-32.339 + 0.5).floor() = -32
        assert_eq!(dir.pitch, angle_to_int8(-0.4));
        assert_ne!(dir.pitch, half_angle_to_int8(-0.4));

        let (yaw, pitch, roll) = dir.decode();
        assert!((yaw - 0.8).abs() < 0.03);
        assert!((pitch - (-0.4)).abs() < 0.03);
        assert!((roll - 0.5).abs() < 0.03);

        let entity = CreateEntity {
            compression: 0,
            id: 12345,
            entity_type: 5,
            pos: [10.5, 20.25, -30.75],
            dir,
            properties: vec![0xDE, 0xAD, 0xBE, 0xEF],
        };

        let mut bytes = Vec::new();
        entity.write_to(&mut bytes);

        let mut r = BinaryReader::new(&bytes);
        let decoded = CreateEntity::read_from(&mut r).unwrap();
        assert_eq!(decoded.compression, 0);
        assert_eq!(decoded.id, 12345);
        assert_eq!(decoded.entity_type, 5);
        assert_eq!(decoded.pos, [10.5, 20.25, -30.75]);
        assert_eq!(decoded.dir, dir);
        assert_eq!(decoded.properties, vec![0xDE, 0xAD, 0xBE, 0xEF]);
        assert_eq!(decoded.dir.pitch, angle_to_int8(-0.4));
    }
}
