//! Random Battle matchmaker queue (`QueueManager`) with hybrid weight balancing
//! and configurable bot backfill.

use std::time::{Duration, Instant};

use wot065_data::catalog::Catalog;
use wot065_data::descr::VehicleDescr;
use wot065_data::item_defs::VehicleDef;

use crate::assets::arena_defs::ArenaDefsCatalog;
use crate::balance::MatchmakerBalanceConfig;
use crate::lobby::{
    ArenaLaunchSpec, BattleMode, ParticipantSpec, ARENA_BONUS_TYPE_REGULAR, ARENA_GUI_TYPE_RANDOM,
};

/// Exact class tag order expected by `common/constants.py:VEHICLE_CLASSES` and
/// `RANDOMS_QUEUE_INFO.classes` (`lightTank, mediumTank, heavyTank, SPG, AT-SPG`).
pub const VEHICLE_CLASS_TAGS: [&str; 5] = ["lightTank", "mediumTank", "heavyTank", "SPG", "AT-SPG"];

pub const QUEUE_TYPE_RANDOMS: i32 = 1;
pub const QUEUE_TYPE_COMPANIES: i32 = 2;
pub const QUEUE_TYPE_VOLUNTEERS: i32 = 3;

pub const JOIN_FAILURE_TIME_OUT: u8 = 1;
pub const JOIN_FAILURE_NOT_FOUND: u8 = 2;
pub const JOIN_FAILURE_ACCOUNT_LOCK: u8 = 3;
pub const JOIN_FAILURE_WRONG_VEHICLE: u8 = 4;
pub const JOIN_FAILURE_TEAM_IS_FULL: u8 = 5;
pub const JOIN_FAILURE_WRONG_ARGS: u8 = 6;
pub const JOIN_FAILURE_ARENA_IS_NOT_RUNNING: u8 = 8;
pub const JOIN_FAILURE_CANNOT_CREATE: u8 = 9;

/// Matchmaker configuration for `QueueManager`.
#[derive(Debug, Clone)]
pub struct QueueConfig {
    pub team_size: usize,
    pub queue_wait_timeout: Duration,
    pub bot_fill: bool,
}

impl Default for QueueConfig {
    fn default() -> Self {
        Self {
            team_size: 15,
            queue_wait_timeout: Duration::from_secs(5),
            bot_fill: true,
        }
    }
}

/// Queued human player waiting for a Random Battle.
#[derive(Debug, Clone)]
pub struct QueueEntry {
    pub account_db_id: i64,
    pub player_name: String,
    pub veh_inv_id: i64,
    pub veh_comp_descr: Vec<u8>,
    pub veh_type_cd: i32,
    pub tier: u8,
    pub class_idx: usize,
    pub mm_weight: f32,
    pub requested_map_id: Option<i32>,
    pub enqueued_at: Instant,
}

/// Snapshot used to populate `Account.receiveQueueInfo`.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct QueueInfoSnapshot {
    pub by_levels: [u32; 10],
    pub by_classes: [u32; 5],
}

impl QueueInfoSnapshot {
    /// Encode `(RANDOMS_QUEUE_INFO, COMPANIES_QUEUE_INFO)` matching BW 2.0.1 wire layout.
    pub fn encode_wire(&self) -> (Vec<u8>, Vec<u8>) {
        // RANDOMS_QUEUE_INFO: FIXED_DICT(AllowNone=true) -> u8(1) + length(u32) +
        // levels(ARRAY<UINT32>(11)) + classes(ARRAY<UINT32>(5)).
        // `levels` is indexed by vehicle level: `gui/Scaleform/Prebattle.py`
        // shows `levels.reverse(); levels[:-1]`, which drops the level-0 slot.
        let total_players: u32 = self.by_levels.iter().sum();
        let mut randoms = Vec::with_capacity(1 + 4 + 4 + 11 * 4 + 4 + 5 * 4);
        randoms.push(1u8);
        randoms.extend_from_slice(&total_players.to_le_bytes());
        randoms.extend_from_slice(&11u32.to_le_bytes());
        randoms.extend_from_slice(&0u32.to_le_bytes());
        for &cnt in &self.by_levels {
            randoms.extend_from_slice(&cnt.to_le_bytes());
        }
        randoms.extend_from_slice(&5u32.to_le_bytes());
        for &cnt in &self.by_classes {
            randoms.extend_from_slice(&cnt.to_le_bytes());
        }

        // COMPANIES_QUEUE_INFO: FIXED_DICT(AllowNone=true) -> u8(1) + length(u32 = 0)
        let companies = vec![1u8, 0, 0, 0, 0];
        (randoms, companies)
    }
}

/// Compute server matchmaker weight for `veh` using `MatchmakerBalanceConfig`.
pub fn compute_vehicle_weight(veh: &VehicleDef, mm: &MatchmakerBalanceConfig) -> f32 {
    let key = format!("{}:{}", veh.nation, veh.name);
    if let Some(&override_w) = mm.by_vehicle_overrides.get(&key) {
        return override_w;
    }
    let tier_idx = (veh.level.clamp(1, 10) - 1) as usize;
    let level_w = mm.by_component_levels.get(tier_idx).copied().unwrap_or(2.0);
    let class_mult = VEHICLE_CLASS_TAGS
        .iter()
        .find(|&&tag| veh.tags.contains(tag))
        .and_then(|&tag| mm.by_vehicle_classes.get(tag).copied())
        .unwrap_or(1.0);
    let veh_mult = mm
        .modules_weight_multipliers
        .get("vehicle")
        .copied()
        .unwrap_or(1.0);
    level_w * class_mult * veh_mult
}

/// Determine zero-based index into [`VEHICLE_CLASS_TAGS`] for `veh`.
pub fn vehicle_class_index(veh: &VehicleDef) -> usize {
    VEHICLE_CLASS_TAGS
        .iter()
        .position(|&tag| veh.tags.contains(tag))
        .unwrap_or(0)
}

/// Hybrid queue matchmaker balancing humans by weight and backfilling bots on timeout/force.
#[derive(Debug)]
pub struct QueueManager {
    config: QueueConfig,
    mm: MatchmakerBalanceConfig,
    entries: Vec<QueueEntry>,
    next_bot_id: i64,
    map_cursor: usize,
}

#[derive(Default)]
struct Roster {
    size: usize,
    weight: f32,
    tiers: [usize; 10],
    classes: [usize; 5],
}

impl QueueManager {
    pub fn new(mut config: QueueConfig, mm: MatchmakerBalanceConfig) -> Self {
        config.team_size = config.team_size.clamp(1, 15);
        Self {
            config,
            mm,
            entries: Vec::new(),
            next_bot_id: -1,
            map_cursor: 0,
        }
    }

    pub fn enqueue(&mut self, entry: QueueEntry) -> Result<(), u8> {
        if self.contains(entry.account_db_id) {
            return Err(JOIN_FAILURE_ACCOUNT_LOCK);
        }
        self.entries.push(entry);
        Ok(())
    }

    pub fn dequeue(&mut self, account_db_id: i64) -> Option<QueueEntry> {
        let pos = self
            .entries
            .iter()
            .position(|e| e.account_db_id == account_db_id)?;
        Some(self.entries.remove(pos))
    }

    pub fn contains(&self, account_db_id: i64) -> bool {
        self.entries
            .iter()
            .any(|e| e.account_db_id == account_db_id)
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn queue_info(&self) -> QueueInfoSnapshot {
        let mut snap = QueueInfoSnapshot::default();
        for entry in &self.entries {
            let lvl_idx = (entry.tier.clamp(1, 10) - 1) as usize;
            snap.by_levels[lvl_idx] += 1;
            if entry.class_idx < snap.by_classes.len() {
                snap.by_classes[entry.class_idx] += 1;
            }
        }
        snap
    }

    pub fn tick(
        &mut self,
        now: Instant,
        arenas: &ArenaDefsCatalog,
        catalog: &Catalog,
    ) -> Vec<ArenaLaunchSpec> {
        let mut launches = Vec::new();
        if arenas.maps.is_empty() {
            return launches;
        }
        let team_size = self.config.team_size;
        let full_match_size = team_size * 2;

        while !self.entries.is_empty() {
            let full_ready = self.entries.len() >= full_match_size;
            let timed_out = self.config.bot_fill
                && now.saturating_duration_since(self.entries[0].enqueued_at)
                    >= self.config.queue_wait_timeout;
            if !full_ready && !timed_out {
                break;
            }
            let batch = self.drain_match_batch(full_match_size);
            launches.push(self.build_match(batch, arenas, catalog));
        }

        launches
    }

    pub fn force_match(
        &mut self,
        arenas: &ArenaDefsCatalog,
        catalog: &Catalog,
    ) -> Option<ArenaLaunchSpec> {
        if self.entries.is_empty() || arenas.maps.is_empty() {
            return None;
        }
        let full_match_size = self.config.team_size * 2;
        let batch = self.drain_match_batch(full_match_size);
        Some(self.build_match(batch, arenas, catalog))
    }

    fn drain_match_batch(&mut self, max_players: usize) -> Vec<QueueEntry> {
        let anchor_tier = self.entries[0].tier.clamp(1, 10);
        let mut ranked: Vec<_> = (1..self.entries.len()).collect();
        ranked.sort_unstable_by_key(|&i| {
            let entry = &self.entries[i];
            (
                entry.tier.clamp(1, 10).abs_diff(anchor_tier),
                entry.enqueued_at,
                i,
            )
        });
        let mut selected = vec![false; self.entries.len()];
        selected[0] = true;
        for index in ranked.into_iter().take(max_players.saturating_sub(1)) {
            selected[index] = true;
        }
        let mut batch = Vec::with_capacity(max_players.min(self.entries.len()));
        for (entry, take) in std::mem::take(&mut self.entries).into_iter().zip(selected) {
            if take {
                batch.push(entry);
            } else {
                self.entries.push(entry);
            }
        }
        batch
    }

    fn build_match(
        &mut self,
        mut humans: Vec<QueueEntry>,
        arenas: &ArenaDefsCatalog,
        catalog: &Catalog,
    ) -> ArenaLaunchSpec {
        let map_def = humans
            .iter()
            .filter_map(|h| h.requested_map_id)
            .find_map(|id| arenas.by_id(id))
            .unwrap_or_else(|| {
                let def = &arenas.maps[self.map_cursor % arenas.maps.len()];
                self.map_cursor = self.map_cursor.wrapping_add(1);
                def
            });

        let team_size = self.config.team_size;
        let mut tiers: Vec<_> = humans.iter().map(|h| h.tier.clamp(1, 10)).collect();
        tiers.sort_unstable();
        let target_tier = tiers.get(tiers.len() / 2).copied().unwrap_or(1);
        humans.sort_by(|a, b| {
            b.tier
                .cmp(&a.tier)
                .then_with(|| b.mm_weight.total_cmp(&a.mm_weight))
                .then_with(|| a.enqueued_at.cmp(&b.enqueued_at))
                .then_with(|| a.account_db_id.cmp(&b.account_db_id))
        });

        let mut teams = [Roster::default(), Roster::default()];
        let mut participants = Vec::with_capacity(team_size * 2);
        for h in humans {
            let tier = usize::from(h.tier.clamp(1, 10) - 1);
            let class = h.class_idx.min(4);
            let index = (0..2)
                .filter(|&i| teams[i].size < team_size)
                .min_by(|&a, &b| {
                    teams[a].tiers[tier]
                        .cmp(&teams[b].tiers[tier])
                        .then_with(|| teams[a].weight.total_cmp(&teams[b].weight))
                        .then_with(|| teams[a].classes[class].cmp(&teams[b].classes[class]))
                        .then_with(|| teams[a].size.cmp(&teams[b].size))
                })
                .expect("a match batch is bounded by both rosters");
            let roster = &mut teams[index];
            roster.tiers[tier] += 1;
            roster.classes[class] += 1;
            roster.size += 1;
            roster.weight += h.mm_weight.max(0.0);

            participants.push(ParticipantSpec {
                account_db_id: h.account_db_id,
                player_name: h.player_name,
                veh_inv_id: h.veh_inv_id,
                veh_comp_descr: h.veh_comp_descr,
                team: (index + 1) as u8,
                ..Default::default()
            });
        }

        if self.config.bot_fill {
            let bot_templates = collect_bot_templates(catalog, i32::from(target_tier), &self.mm);
            let fallback = participants
                .first()
                .map(|p| p.veh_comp_descr.clone())
                .unwrap_or_default();
            let mut template = 0;
            while let Some(index) =
                (0..2)
                    .filter(|&i| teams[i].size < team_size)
                    .min_by(|&a, &b| {
                        teams[a]
                            .size
                            .cmp(&teams[b].size)
                            .then_with(|| teams[a].weight.total_cmp(&teams[b].weight))
                    })
            {
                let (descr, weight) = bot_templates
                    .get(template % bot_templates.len().max(1))
                    .map_or((&fallback, 2.0), |(descr, weight)| (descr, *weight));
                let bot_id = self.next_bot_id;
                self.next_bot_id -= 1;
                teams[index].size += 1;
                teams[index].weight += weight;

                participants.push(ParticipantSpec {
                    account_db_id: bot_id,
                    player_name: format!("Bot_{}", bot_id.unsigned_abs()),
                    veh_inv_id: bot_id,
                    veh_comp_descr: descr.clone(),
                    team: (index + 1) as u8,
                    is_bot: true,
                    ..Default::default()
                });
                // Give both rosters the same stock template when both need a slot.
                if teams[0].size == teams[1].size || teams[1 - index].size == team_size {
                    template += 1;
                }
            }
        }

        ArenaLaunchSpec {
            arena_type_id: map_def.id,
            arena_gui_type: ARENA_GUI_TYPE_RANDOM,
            arena_bonus_type: ARENA_BONUS_TYPE_REGULAR,
            mode: BattleMode::Random,
            round_length_secs: map_def.round_length_secs,
            prebattle_id: 0,
            participants,
        }
    }
}

fn collect_bot_templates(
    catalog: &Catalog,
    target_tier: i32,
    mm: &MatchmakerBalanceConfig,
) -> Vec<(Vec<u8>, f32)> {
    let mut defs: Vec<&VehicleDef> = catalog.vehicle_defs.values().collect();
    defs.sort_by_key(|v| ((v.level as i32 - target_tier).abs(), v.nation_id, v.type_id));
    defs.into_iter()
        .filter_map(|v| {
            let descr = VehicleDescr::stock(v).ok()?.to_bytes();
            let weight = compute_vehicle_weight(v, mm);
            Some((descr, weight))
        })
        .take(12)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use wot065_data::catalog;
    use wot065_data::descr::VehicleDescr;
    use wot065_data::item_defs::make_int_comp_descr;
    use crate::assets::arena_defs::ArenaDef;

    fn test_catalog_and_arenas(name: &str) -> (Catalog, ArenaDefsCatalog, PathBuf) {
        let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../target/test-tmp")
            .join(format!("queue_{name}_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        for (nation, veh, tags, lvl) in [
            ("ussr", "MS-1", "lightTank", 1),
            ("germany", "Ltraktor", "heavyTank", 5),
            ("usa", "T1_Cunningham", "SPG", 1),
        ] {
            let ndir = dir.join(nation);
            let cdir = ndir.join("components");
            std::fs::create_dir_all(&cdir).unwrap();
            std::fs::write(ndir.join("customization.xml"), "<root/>").unwrap();
            std::fs::write(
                ndir.join("list.xml"),
                format!("<root><{veh}><id>0</id><userString>{veh}</userString><level>{lvl}</level><price>1000</price><tags>{tags}</tags></{veh}></root>"),
            ).unwrap();
            std::fs::write(cdir.join("shells.xml"), "<root><AP><id>1</id><price>5</price><kind>ARMOR_PIERCING</kind><caliber>20</caliber><isTracer>true</isTracer><effects>smallAP</effects><damage><armor>12</armor><devices>20</devices></damage></AP></root>").unwrap();
            std::fs::write(cdir.join("chassis.xml"), "<root><ids><Ch>1</Ch></ids><shared><Ch><weight>1000</weight><level>1</level><price>100</price><maxLoad>10000</maxLoad></Ch></shared></root>").unwrap();
            std::fs::write(cdir.join("engines.xml"), "<root><ids><Eng>1</Eng></ids><shared><Eng><weight>500</weight><level>1</level><price>100</price><power>100</power></Eng></shared></root>").unwrap();
            std::fs::write(cdir.join("fuelTanks.xml"), "<root><ids><Fuel>1</Fuel></ids><shared><Fuel><weight>100</weight><level>1</level><price>50</price></Fuel></shared></root>").unwrap();
            std::fs::write(cdir.join("radios.xml"), "<root><ids><Rad>1</Rad></ids><shared><Rad><weight>50</weight><level>1</level><price>50</price><distance>300</distance></Rad></shared></root>").unwrap();
            std::fs::write(cdir.join("guns.xml"), "<root><ids><G>1</G></ids><shared><G><level>1</level><price>100</price><weight>50</weight><maxAmmo>50</maxAmmo><pitchLimits>-10 20</pitchLimits><rotationSpeed>30</rotationSpeed><reloadTime>2.0</reloadTime><aimingTime>1.5</aimingTime><shotDispersionRadius>0.4</shotDispersionRadius><effects>small</effects><shots><AP><defaultPortion>1.0</defaultPortion><speed>600</speed><gravity>9.81</gravity><maxDistance>400</maxDistance><piercingPower>30 20</piercingPower></AP></shots></G></shared></root>").unwrap();
            std::fs::write(cdir.join("turrets.xml"), "<root><ids><T>1</T></ids><shared><T><level>1</level><price>100</price><weight>200</weight><maxHealth>50</maxHealth><yawLimits>-180 180</yawLimits><guns><G>shared</G></guns></T></shared></root>").unwrap();
            std::fs::write(
                ndir.join(format!("{veh}.xml")),
                "<root><crew><commander/><driver/></crew><speedLimits><forward>30</forward><backward>10</backward></speedLimits><hull><weight>3000</weight><maxHealth>100</maxHealth><armor><armor_1>15</armor_1></armor><turretPositions><turret>0 0.5 0</turret></turretPositions></hull><chassis><Ch>shared</Ch></chassis><engines><Eng>shared</Eng></engines><fuelTanks><Fuel>shared</Fuel></fuelTanks><radios><Rad>shared</Rad></radios><turrets0><T>shared</T></turrets0></root>",
            ).unwrap();
        }
        let cat = catalog::load_from_vehicles_dir(&dir).unwrap();
        let arenas = ArenaDefsCatalog {
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
        };
        (cat, arenas, dir)
    }

    fn make_entry(cat: &Catalog, mm: &MatchmakerBalanceConfig, db_id: i64, nation_id: u8, now: Instant) -> QueueEntry {
        let vdef = cat.vehicle_def_by_id(nation_id, 0).unwrap();
        let descr = VehicleDescr::stock(vdef).unwrap();
        let class_idx = vehicle_class_index(vdef);
        QueueEntry {
            account_db_id: db_id,
            player_name: format!("player_{db_id}"),
            veh_inv_id: db_id * 10,
            veh_comp_descr: descr.to_bytes(),
            veh_type_cd: make_int_comp_descr(1, nation_id, 0),
            tier: vdef.level as u8,
            class_idx,
            mm_weight: compute_vehicle_weight(vdef, mm),
            requested_map_id: None,
            enqueued_at: now,
        }
    }

    /// Parse `RANDOMS_QUEUE_INFO` as the client does: `AllowNone` flag, `length`,
    /// `levels` (`ARRAY<UINT32>`), `classes` (`ARRAY<UINT32>`), with no trailing bytes.
    fn decode_randoms_queue_info(wire: &[u8]) -> (u32, Vec<u32>, Vec<u32>) {
        let mut pos = 0;
        let next_u32 = |pos: &mut usize| {
            let value = u32::from_le_bytes(wire[*pos..*pos + 4].try_into().unwrap());
            *pos += 4;
            value
        };
        assert_eq!(wire[0], 1, "RANDOMS_QUEUE_INFO must not be None");
        pos += 1;
        let length = next_u32(&mut pos);
        let mut levels = Vec::new();
        for _ in 0..next_u32(&mut pos) {
            levels.push(next_u32(&mut pos));
        }
        let mut classes = Vec::new();
        for _ in 0..next_u32(&mut pos) {
            classes.push(next_u32(&mut pos));
        }
        assert_eq!(pos, wire.len(), "trailing bytes after RANDOMS_QUEUE_INFO");
        (length, levels, classes)
    }

    /// `gui/Scaleform/Prebattle.onQueueInfoReceived` shows
    /// `levels.reverse(); levels[:-1]` in the queue window, so `levels` must be
    /// indexed by vehicle level with an unused level-0 slot.
    #[test]
    fn client_queue_window_lists_all_ten_tiers() {
        let snapshot = QueueInfoSnapshot {
            by_levels: [1, 2, 3, 4, 5, 6, 7, 8, 9, 10],
            by_classes: [5, 4, 3, 2, 1],
        };
        let (randoms, _) = snapshot.encode_wire();
        let (length, levels, classes) = decode_randoms_queue_info(&randoms);

        let mut shown = levels;
        shown.reverse();
        shown.pop();
        assert_eq!(shown, [10, 9, 8, 7, 6, 5, 4, 3, 2, 1]);
        assert_eq!(length, 55);
        assert_eq!(classes, [5, 4, 3, 2, 1]);
    }

    #[test]
    fn queue_tracks_info_dequeue_and_backfills_bots_on_timeout() {
        let (cat, arenas, dir) = test_catalog_and_arenas("timeout");
        let mm = MatchmakerBalanceConfig::default();
        let mut q = QueueManager::new(
            QueueConfig {
                team_size: 3,
                queue_wait_timeout: Duration::from_millis(200),
                bot_fill: true,
            },
            mm.clone(),
        );

        let t0 = Instant::now();
        let e1 = make_entry(&cat, &mm, 1, 0, t0);
        let e2 = make_entry(&cat, &mm, 2, 2, t0);
        q.enqueue(e1).unwrap();
        assert_eq!(q.enqueue(make_entry(&cat, &mm, 1, 0, t0)), Err(JOIN_FAILURE_ACCOUNT_LOCK));
        q.enqueue(e2).unwrap();

        let info = q.queue_info();
        assert_eq!(info.by_levels[0], 2); // tier 1
        assert_eq!(info.by_classes[0], 1); // lightTank
        assert_eq!(info.by_classes[3], 1); // SPG (index 3, AT-SPG is 4!)

        let (r_wire, c_wire) = info.encode_wire();
        assert_eq!(r_wire[0], 1);
        assert_eq!(u32::from_le_bytes(r_wire[1..5].try_into().unwrap()), 2);
        assert_eq!(c_wire, vec![1, 0, 0, 0, 0]);

        assert!(q.tick(t0 + Duration::from_millis(50), &arenas, &cat).is_empty());
        let matches = q.tick(t0 + Duration::from_millis(250), &arenas, &cat);
        assert_eq!(matches.len(), 1);
        assert!(q.is_empty());

        let m = &matches[0];
        assert_eq!(m.arena_type_id, 3);
        assert_eq!(m.participants.len(), 6);
        assert_eq!(m.participants.iter().filter(|p| p.team == 1).count(), 3);
        assert_eq!(m.participants.iter().filter(|p| p.team == 2).count(), 3);
        assert_eq!(m.participants.iter().filter(|p| !p.is_bot).count(), 2);
        assert_eq!(m.participants.iter().filter(|p| p.is_bot).count(), 4);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn match_prefers_the_closest_tiers_to_the_oldest_player() {
        let (cat, arenas, dir) = test_catalog_and_arenas("closest_tiers");
        let mm = MatchmakerBalanceConfig::default();
        let mut queue = QueueManager::new(
            QueueConfig {
                team_size: 2,
                queue_wait_timeout: Duration::from_secs(30),
                bot_fill: false,
            },
            mm.clone(),
        );
        let now = Instant::now();
        for (id, tier) in [(1, 5), (2, 7), (3, 3), (4, 5), (5, 5)] {
            let mut entry = make_entry(&cat, &mm, id, 0, now);
            entry.tier = tier;
            queue.enqueue(entry).unwrap();
        }
        let launches = queue.tick(now, &arenas, &cat);
        let mut players: Vec<_> = launches[0]
            .participants
            .iter()
            .map(|p| p.account_db_id)
            .collect();
        players.sort_unstable();
        assert_eq!(players, [1, 2, 4, 5]);
        assert_eq!(queue.queue_info().by_levels[2], 1);
        assert_eq!(queue.queue_info().by_levels[4], 0);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn queue_matches_immediately_when_full_and_balances_weights() {
        let (cat, arenas, dir) = test_catalog_and_arenas("weights");
        let mm = MatchmakerBalanceConfig::default();
        let mut q = QueueManager::new(
            QueueConfig {
                team_size: 2,
                queue_wait_timeout: Duration::from_secs(30),
                bot_fill: true,
            },
            mm.clone(),
        );
        let t0 = Instant::now();
        // 4 players: two heavy tier-5 (12 * 1.45 = 17.4) and two light tier-1 (2.0)
        q.enqueue(make_entry(&cat, &mm, 1, 1, t0)).unwrap();
        q.enqueue(make_entry(&cat, &mm, 2, 1, t0)).unwrap();
        q.enqueue(make_entry(&cat, &mm, 3, 0, t0)).unwrap();
        q.enqueue(make_entry(&cat, &mm, 4, 0, t0)).unwrap();

        let matches = q.tick(t0, &arenas, &cat);
        assert_eq!(matches.len(), 1);
        let t1_heavies = matches[0]
            .participants
            .iter()
            .filter(|p| p.team == 1 && (p.account_db_id == 1 || p.account_db_id == 2))
            .count();
        let t2_heavies = matches[0]
            .participants
            .iter()
            .filter(|p| p.team == 2 && (p.account_db_id == 1 || p.account_db_id == 2))
            .count();
        assert_eq!(t1_heavies, 1);
        assert_eq!(t2_heavies, 1);

        // Force match on single player + dequeue
        q.enqueue(make_entry(&cat, &mm, 10, 0, t0)).unwrap();
        assert!(q.dequeue(10).is_some());
        assert!(q.force_match(&arenas, &cat).is_none());

        q.enqueue(make_entry(&cat, &mm, 11, 0, t0)).unwrap();
        let forced = q.force_match(&arenas, &cat).expect("forced match");
        assert_eq!(forced.participants.len(), 4);
        assert!(q.is_empty());

        let _ = std::fs::remove_dir_all(&dir);
    }
}
