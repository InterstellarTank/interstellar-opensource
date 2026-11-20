use std::collections::BTreeMap;

use anyhow::{ensure, Context, Result};
use bw201::interfaces::client_ids;
use bw201::stream::BinaryReader;
use bw201::world::{
    CreateCellPlayer, CreateEntity, DetailedPosition, DirKind, EnterAoI, EnterAoIOnVehicle,
    ForcedPosition, IdKind, LeaveAoI, PosKind, S2CAvatarUpdate, SpaceData, SpaceDataMapping,
    UpdateEntity, NO_ID_ALIAS, SPACE_DATA_MAPPING_KEY_CLIENT_SERVER,
};
use serde::Serialize;
use wot065_data::pickle::{self, Value};
use wot_baseapp::protocol::{client_method, VEHICLE_ENTITY_TYPE_ID};
use wot_battle::world::ARENA_UPDATE_PERIOD;

#[derive(Clone, Copy, Debug)]
pub struct Pose {
    pub position: [f32; 3],
    pub yaw: f32,
}

#[derive(Debug)]
pub struct Entity {
    pub kind: u16,
    pub pose: Pose,
    pub properties: Vec<u8>,
    pub update: Option<Vec<u8>>,
}

/// The results delivered to the player's Avatar after battle settlement.
#[derive(Clone, Copy, Debug, Serialize)]
pub struct BattleResult {
    pub vehicle_inventory_id: i32,
    pub vehicle_id: i32,
    pub arena_unique_id: u64,
    pub map_id: u32,
    pub regular: bool,
    pub shots: u16,
    pub xp: u32,
    pub credits: u32,
}

impl BattleResult {
    pub(crate) fn read(r: &mut BinaryReader<'_>) -> Result<Option<Self>> {
        let active = r.u8()?;
        let vehicle_inventory_id = r.i32()?;
        let present = r.u8()?;
        ensure!(active <= 1 && present <= 1, "invalid battle result flags");
        if present == 0 {
            ensure!(r.is_empty(), "unexpected bytes after absent battle result");
            return Ok(None);
        }
        let xp = r.u32()?;
        let credits = r.u32()?;
        r.raw(4 + 1 + 4 + 2)?; // free XP, XP factor, repair cost and remaining health
        skip_result_array(r, 4)?; // shell descriptors and counts
        skip_result_array(r, 1)?; // crew activity
        let vehicle_id = r.i32()?;
        let arena_unique_id = r.u64()?;
        let winner = r.i8()?;
        let regular = r.u8()?;
        ensure!(
            (-1..=1).contains(&winner) && regular <= 1,
            "invalid battle outcome"
        );
        let _killer = r.i32()?;
        for _ in 0..4 {
            // killed entities, killed types, damaged and spotted entities
            skip_result_array(r, 4)?;
        }
        let shots = r.u16()?;
        r.raw(2 + 4 + 4 + 2 + 4 + 4)?; // hits and damage dealt/received
        r.raw(2 + 2 + 2)?; // capture points, dropped capture and lifetime
        let map_id = r.u32()?;
        let _created_at = r.u32()?;
        for size in [1, 4, 1, 4] {
            // achievements, heroes, epic achievements and honor titles
            skip_result_array(r, size)?;
        }
        let _tkill_rating = r.f32()?;
        skip_result_array(r, 22)?; // teamkill records
        r.raw(4 * 4)?; // XP/credit penalties and in/out contribution totals
        ensure!(r.is_empty(), "unexpected bytes after battle result");
        Ok((active == 1).then_some(Self {
            vehicle_inventory_id,
            vehicle_id,
            arena_unique_id,
            map_id,
            regular: regular == 1,
            shots,
            xp,
            credits,
        }))
    }
}

fn skip_result_array(r: &mut BinaryReader<'_>, item_size: usize) -> Result<()> {
    let bytes = (r.u32()? as usize)
        .checked_mul(item_size)
        .context("battle result array is too large")?;
    r.raw(bytes)?;
    Ok(())
}

#[derive(Debug, Default)]
pub struct Arena {
    pub map_id: i32,
    pub space_id: Option<i32>,
    pub geometry: Option<String>,
    pub team: Option<u8>,
    pub vehicle_id: Option<i32>,
    pub period: Option<u8>,
    pub entities: BTreeMap<i32, Entity>,
    pub own_snapshots: u64,
    pub own_shots: u64,
    pub health: Option<i16>,
    pub reload_time: Option<f32>,
    pub ammo: BTreeMap<i32, u16>,
    pub(crate) ready_sent: bool,
    aliases: BTreeMap<u8, i32>,
    mappings: BTreeMap<i32, String>,
    interest: BTreeMap<i32, bool>,
    reference: Option<[f32; 3]>,
    pub arena_updates: BTreeMap<u8, Value>,
}

impl Arena {
    pub(crate) fn for_map(map_id: i32) -> Self {
        Self {
            map_id,
            ..Self::default()
        }
    }

    pub fn is_ready(&self) -> bool {
        self.ready_sent
    }

    pub fn own_vehicle(&self) -> Option<&Entity> {
        self.entities
            .get(&self.vehicle_id?)
            .filter(|e| e.kind == VEHICLE_ENTITY_TYPE_ID)
    }

    pub(crate) fn can_signal_ready(&self) -> bool {
        self.geometry.is_some() && self.own_vehicle().is_some() && !self.ready_sent
    }

    pub(crate) fn pending_entity_requests(&mut self) -> Vec<i32> {
        self.interest
            .iter_mut()
            .filter_map(|(id, requested)| (!std::mem::replace(requested, true)).then_some(*id))
            .collect()
    }

    fn enter(&mut self, id: i32, alias: u8) {
        if alias != NO_ID_ALIAS {
            self.aliases.insert(alias, id);
        }
        self.interest.entry(id).or_insert(false);
    }

    pub(crate) fn message(&mut self, id: u8, data: &[u8]) -> Result<()> {
        let mut r = BinaryReader::new(data);
        match id {
            client_ids::CREATE_CELL_PLAYER => {
                let cell = CreateCellPlayer::read_from(&mut r)?;
                let mut props = BinaryReader::new(&cell.cell_data);
                self.space_id = Some(cell.space_id);
                self.geometry = self.mappings.remove(&cell.space_id);
                self.team = Some(props.u8()?);
                self.vehicle_id = Some(props.i32()?);
            }
            client_ids::SPACE_DATA => {
                let data = SpaceData::read_from(&mut r)?;
                if data.key == SPACE_DATA_MAPPING_KEY_CLIENT_SERVER {
                    let mapping = SpaceDataMapping::decode(&data.payload)?;
                    ensure!(
                        !mapping.path.is_empty(),
                        "space mapping has an empty geometry path"
                    );
                    if self.space_id == Some(data.space_id) {
                        self.geometry = Some(mapping.path);
                    } else {
                        self.mappings.insert(data.space_id, mapping.path);
                    }
                }
            }
            client_ids::ENTER_AOI => {
                let entry = EnterAoI::read_from(&mut r)?;
                self.enter(entry.id, entry.id_alias);
            }
            client_ids::ENTER_AOI_ON_VEHICLE => {
                let entry = EnterAoIOnVehicle::read_from(&mut r)?;
                self.enter(entry.id, entry.id_alias);
            }
            client_ids::LEAVE_AOI => {
                let entry = LeaveAoI::read_from(&mut r)?;
                self.entities.remove(&entry.id);
                self.interest.remove(&entry.id);
                self.aliases.retain(|_, id| *id != entry.id);
            }
            client_ids::CREATE_ENTITY => {
                let entity = CreateEntity::read_from(&mut r)?;
                let (yaw, _, _) = entity.dir.decode();
                self.entities.insert(
                    entity.id,
                    Entity {
                        kind: entity.entity_type,
                        pose: Pose {
                            position: entity.pos,
                            yaw,
                        },
                        properties: entity.properties,
                        update: None,
                    },
                );
            }
            client_ids::UPDATE_ENTITY => {
                let update = UpdateEntity::read_from(&mut r)?;
                if let Some(entity) = self.entities.get_mut(&update.id) {
                    entity.update = Some(update.properties);
                }
            }
            client_ids::RELATIVE_POSITION => {
                self.reference = Some([r.f32()?, r.f32()?, r.f32()?]);
            }
            client_ids::DETAILED_POSITION => {
                let pose = DetailedPosition::read_from(&mut r)?;
                self.update_pose(pose.id, Some(pose.position), Some(pose.direction.yaw))?;
            }
            client_ids::FORCED_POSITION => {
                let pose = ForcedPosition::read_from(&mut r)?;
                self.update_pose(pose.id, Some(pose.position), Some(pose.direction.yaw))?;
            }
            client_ids::AVATAR_UPDATE_FIRST..=client_ids::AVATAR_UPDATE_LAST => {
                self.relative_update(S2CAvatarUpdate::decode(id, data)?)?;
            }
            _ => {}
        }
        Ok(())
    }

    pub(crate) fn player_method(&mut self, id: u8, r: &mut BinaryReader<'_>) -> Result<()> {
        match id {
            client_method::AVATAR_UPDATE_ARENA => {
                let tag = r.u8()?;
                let value = pickle::loads(&r.string_bytes()?).context("decode updateArena")?;
                if tag == ARENA_UPDATE_PERIOD {
                    self.period = Some(
                        value
                            .as_list()
                            .and_then(|items| items.first())
                            .and_then(Value::as_int)
                            .and_then(|period| u8::try_from(period).ok())
                            .context("updateArena is missing the arena period")?,
                    );
                }
                self.arena_updates.insert(tag, value);
            }
            client_method::AVATAR_SHOW_TRACER => {
                let shooter = r.i32()?;
                let _shot = r.i32()?;
                let _effects = r.u8()?;
                for _ in 0..7 {
                    // start point, velocity and gravity
                    ensure!(r.f32()?.is_finite(), "tracer contains a non-finite value");
                }
                ensure!(r.is_empty(), "unexpected bytes after tracer");
                if self.vehicle_id == Some(shooter) {
                    self.own_shots += 1;
                }
            }
            client_method::AVATAR_UPDATE_VEHICLE_HEALTH => {
                self.health = Some(r.i16()?);
            }
            client_method::AVATAR_UPDATE_VEHICLE_GUN_RELOAD_TIME => {
                self.reload_time = Some(r.f32()?);
            }
            client_method::AVATAR_UPDATE_VEHICLE_AMMO => {
                let shell = r.i32()?;
                self.ammo.insert(shell, r.u16()?);
            }
            _ => {}
        }
        Ok(())
    }

    fn relative_update(&mut self, update: S2CAvatarUpdate) -> Result<()> {
        let id = match update.id {
            IdKind::NoAlias(id) => id,
            IdKind::Alias(alias) => match self.aliases.get(&alias) {
                Some(&id) => id,
                None => return Ok(()),
            },
        };
        let Some(entity) = self.entities.get(&id) else {
            return Ok(());
        };
        let position = self.reference.and_then(|[x, y, z]| match update.pos {
            PosKind::FullPos(packed) => {
                let [dx, dy, dz] = packed.decode();
                Some([x + dx, y + dy, z + dz])
            }
            PosKind::OnChunk { xz, .. } | PosKind::OnGround(xz) => {
                let [dx, dz] = xz.decode();
                Some([x + dx, entity.pose.position[1], z + dz])
            }
            PosKind::NoPos => None,
        });
        let yaw = match update.dir {
            DirKind::YawPitchRoll(dir) => Some(dir.decode().0),
            DirKind::YawPitch(dir) => Some(dir.decode().0),
            DirKind::Yaw(dir) => Some(dir.decode()),
            DirKind::NoDir => None,
        };
        self.update_pose(id, position, yaw)
    }

    fn update_pose(&mut self, id: i32, position: Option<[f32; 3]>, yaw: Option<f32>) -> Result<()> {
        if let Some(entity) = self.entities.get_mut(&id) {
            if let Some(position) = position {
                ensure!(
                    position.into_iter().all(f32::is_finite),
                    "entity {id} has a non-finite position"
                );
                entity.pose.position = position;
                if self.vehicle_id == Some(id) {
                    self.own_snapshots += 1;
                }
            }
            if let Some(yaw) = yaw {
                ensure!(yaw.is_finite(), "entity {id} has a non-finite yaw");
                entity.pose.yaw = yaw;
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bw201::stream::BinaryWriter;
    use bw201::world::{EntityDir, PackedXYZ};
    use wot_battle::sim::results::{BattleResultsExt, TkillVictimResult};

    #[test]
    fn own_vehicle_pose_uses_server_reference_and_entity_alias() {
        let mut arena = Arena {
            vehicle_id: Some(10),
            ..Arena::default()
        };
        let vehicle = CreateEntity {
            compression: 0,
            id: 10,
            entity_type: VEHICLE_ENTITY_TYPE_ID,
            pos: [0.0; 3],
            dir: EntityDir::encode([0.0; 3]),
            properties: Vec::new(),
        };
        let mut body = Vec::new();
        vehicle.write_to(&mut body);
        arena.message(client_ids::CREATE_ENTITY, &body).unwrap();
        arena
            .message(client_ids::ENTER_AOI, &[10, 0, 0, 0, 3])
            .unwrap();
        assert_eq!(arena.pending_entity_requests(), vec![10]);
        assert!(arena.pending_entity_requests().is_empty());
        let reference: Vec<_> = [100.0f32, 25.0, 200.0]
            .into_iter()
            .flat_map(f32::to_le_bytes)
            .collect();
        arena
            .message(client_ids::RELATIVE_POSITION, &reference)
            .unwrap();
        let (id, data) = S2CAvatarUpdate {
            id: IdKind::Alias(3),
            pos: PosKind::FullPos(PackedXYZ::encode([5.0, 1.0, -3.0])),
            dir: DirKind::NoDir,
        }
        .encode();
        arena.message(id, &data).unwrap();
        assert_eq!(
            arena.own_vehicle().unwrap().pose.position,
            [105.0, 26.0, 197.0]
        );
        assert_eq!(arena.own_snapshots, 1);
    }

    #[test]
    fn bot_tracer_does_not_count_as_a_player_shot() {
        let mut arena = Arena {
            vehicle_id: Some(10),
            ..Arena::default()
        };
        let mut tracer = [0u8; 37];
        tracer[..4].copy_from_slice(&11i32.to_le_bytes());
        arena
            .player_method(
                client_method::AVATAR_SHOW_TRACER,
                &mut BinaryReader::new(&tracer),
            )
            .unwrap();
        assert_eq!(arena.own_shots, 0);
        tracer[..4].copy_from_slice(&10i32.to_le_bytes());
        arena
            .player_method(
                client_method::AVATAR_SHOW_TRACER,
                &mut BinaryReader::new(&tracer),
            )
            .unwrap();
        assert_eq!(arena.own_shots, 1);
    }

    #[test]
    fn a_truncated_own_tracer_does_not_confirm_a_shot() {
        let mut arena = Arena {
            vehicle_id: Some(10),
            ..Arena::default()
        };
        let result = arena.player_method(
            client_method::AVATAR_SHOW_TRACER,
            &mut BinaryReader::new(&10i32.to_le_bytes()),
        );
        assert!(result.is_err());
        assert_eq!(arena.own_shots, 0);
    }

    #[test]
    fn result_reads_the_server_callback_and_requires_the_complete_payload() {
        let wire = BattleResultsExt {
            xp: 120,
            credits: 920,
            vehicle_id: 10,
            arena_unique_id: 25,
            arena_type_id: 3,
            regular: true,
            shots: 2,
            ammo: vec![12, 9],
            crew_activity: vec![true, false],
            killed: vec![33],
            hero_vehicle_ids: vec![10],
            tkill_log: vec![TkillVictimResult {
                target_id: 12,
                ..Default::default()
            }],
            ..Default::default()
        }
        .to_wire();
        let mut callback = BinaryWriter::new();
        callback.u8(1);
        callback.i32(5);
        callback.raw(&wire);
        let bytes = callback.as_slice();
        let result = BattleResult::read(&mut BinaryReader::new(bytes))
            .unwrap()
            .unwrap();
        assert_eq!(result.vehicle_inventory_id, 5);
        assert_eq!(result.vehicle_id, 10);
        assert_eq!(result.arena_unique_id, 25);
        assert_eq!(result.map_id, 3);
        assert_eq!(result.shots, 2);
        assert_eq!((result.xp, result.credits), (120, 920));
        assert!(result.regular);
        assert!(BattleResult::read(&mut BinaryReader::new(&bytes[..bytes.len() - 1])).is_err());
    }
}
