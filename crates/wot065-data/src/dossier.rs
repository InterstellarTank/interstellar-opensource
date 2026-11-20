//! Binary dossier codecs for WoT 0.6.5:
//! - Account dossier (version 19, >= 154 bytes)
//! - Vehicle dossier (version 17, >= 138 bytes)
//! - Tankman dossier (version 10, exact 132 bytes)
//!
//! Mirrors `.scratch/research/decompiled/common/dossiers/__init__.py`.

use std::collections::BTreeMap;
use std::fmt;
use crate::pickle::Value;

pub const BATTLE_HERO_NAMES: [&str; 8] = [
    "reserved", "warrior", "invader", "sniper", "defender", "steelwall", "supporter", "scout",
];

#[derive(Debug, Clone, Default)]
pub struct DossierBattleInput {
    pub bonus_type: u8,
    pub xp_factor: u8,
    pub xp: u32,
    pub winner: i8,
    pub killer_id: i32,
    pub life_time: u16,
    pub shots: u16,
    pub hits: u16,
    pub spotted: u32,
    pub damage_dealt: u32,
    pub damage_received: u32,
    pub capture_points: u16,
    pub dropped_capture_points: u16,
    pub killed_types: Vec<u32>,
    pub heroes: Vec<u8>,
}

/// Apply the common v19, v17 and v10 counters from 0.6.5 dossiers/helpers.py.
pub fn update_dossiers(
    account: &mut AccountDossier,
    vehicle: &mut VehicleDossier,
    tankmen: &mut [(TankmanDossier, bool)],
    battle: &DossierBattleInput,
    vehicle_type: u32,
    catalog: &crate::catalog::Catalog,
    now: u32,
) {
    let life_time = u32::from(battle.life_time);
    account.battle_life_time = account.battle_life_time.saturating_add(life_time);
    vehicle.battle_life_time = vehicle.battle_life_time.saturating_add(life_time);
    for (tankman, _) in tankmen.iter_mut() {
        tankman.battle_life_time = tankman.battle_life_time.saturating_add(life_time);
    }
    if !matches!(battle.bonus_type, 1 | 3 | 5) {
        return;
    }

    let xp = battle.xp.checked_div(u32::from(battle.xp_factor)).unwrap_or(0);
    let frags = battle.killed_types.len() as u32;
    let wins = u32::from(battle.winner == 1);
    let losses = u32::from(battle.winner == -1);
    let survived = u32::from(battle.killer_id == 0);
    let mut frags_8p = 0;
    let mut frags_beast = 0;
    for &killed in &battle.killed_types {
        for counts in [&mut account.veh_type_frags, &mut vehicle.veh_type_frags] {
            let count = counts.entry(killed).or_default();
            *count = count.saturating_add(1).min(60_001);
        }
        if let Some(def) = catalog.vehicle_def(killed as i32) {
            frags_8p += u32::from(def.level >= 8);
            frags_beast += u32::from(def.tags.iter().any(|tag| tag == "beast"));
        }
    }
    if xp != 0 && xp >= u32::from(account.max_xp) {
        account.max_xp_vehicle = vehicle_type;
    }
    if frags != 0 && frags >= u32::from(account.max_frags) {
        account.max_frags_vehicle = vehicle_type;
    }
    let summary = account.veh_dossiers_cut.entry(vehicle_type).or_default();
    summary.0 = summary.0.saturating_add(1);
    summary.1 = summary.1.saturating_add(wins);

    macro_rules! update {
        ($dossier:expr, $survived:expr) => {{
            let d = $dossier;
            d.last_battle_time = now;
            d.battles_count = d.battles_count.saturating_add(1);
            d.xp = d.xp.saturating_add(xp);
            d.max_xp = d.max_xp.max(xp.min(u32::from(u16::MAX)) as u16);
            d.wins = d.wins.saturating_add(wins);
            d.losses = d.losses.saturating_add(losses);
            d.survived_battles = d.survived_battles.saturating_add($survived);
            d.frags = d.frags.saturating_add(frags);
            d.frags_8p = d.frags_8p.saturating_add(frags_8p);
            d.frags_beast = d.frags_beast.saturating_add(frags_beast);
            d.max_frags = d.max_frags.max(frags.min(u32::from(u8::MAX)) as u8);
            d.shots = d.shots.saturating_add(u32::from(battle.shots));
            d.hits = d.hits.saturating_add(u32::from(battle.hits));
            d.spotted = d.spotted.saturating_add(battle.spotted);
            d.damage_dealt = d.damage_dealt.saturating_add(battle.damage_dealt);
            d.damage_received = d.damage_received.saturating_add(battle.damage_received);
            d.capture_points = d.capture_points.saturating_add(u32::from(battle.capture_points));
            d.dropped_capture_points = d.dropped_capture_points
                .saturating_add(u32::from(battle.dropped_capture_points.min(100)));
            for &hero in &battle.heroes {
                let counter = match hero {
                    1 => &mut d.warrior,
                    2 => &mut d.invader,
                    3 => &mut d.sniper,
                    4 => &mut d.defender,
                    5 => &mut d.steelwall,
                    6 => &mut d.supporter,
                    7 => &mut d.scout,
                    _ => continue,
                };
                *counter = counter.saturating_add(1);
                d.battle_heroes = d.battle_heroes.saturating_add(1);
            }
            promote_medal(&mut d.medal_kay, u32::from(d.battle_heroes), [1, 10, 100, 1_000]);
            promote_medal(&mut d.medal_carius, d.frags, [10, 100, 1_000, 10_000]);
            promote_medal(&mut d.medal_knispel, d.damage_dealt.saturating_add(d.damage_received), [10_000, 100_000, 1_000_000, 10_000_000]);
            promote_medal(&mut d.medal_poppel, d.spotted, [20, 200, 2_000, 20_000]);
            promote_medal(&mut d.medal_abrams, d.win_and_survived, [10, 100, 1_000, 10_000]);
            promote_medal(&mut d.medal_le_clerc, d.capture_points, [100, 1_000, 10_000, 100_000]);
            promote_medal(&mut d.medal_lavrinenko, d.dropped_capture_points, [100, 1_000, 10_000, 100_000]);
            promote_medal(&mut d.medal_ekins, d.frags_8p, [3, 30, 300, 3_000]);
        }};
    }
    account.win_and_survived = account.win_and_survived.saturating_add(wins * survived);
    vehicle.win_and_survived = vehicle.win_and_survived.saturating_add(wins * survived);
    update!(account, survived);
    update!(vehicle, survived);
    for (tankman, is_active) in tankmen {
        update!(tankman, u32::from(*is_active));
    }
}

fn promote_medal(class: &mut u8, score: u32, thresholds: [u32; 4]) {
    if let Some(rank) = thresholds.iter().rev().position(|&minimum| score >= minimum) {
        let rank = rank as u8 + 1;
        if *class == 0 || rank < *class {
            *class = rank;
        }
    }
}

pub const ACCOUNT_DOSSIER_VERSION: u16 = 19;
pub const VEHICLE_DOSSIER_VERSION: u16 = 17;
pub const TANKMAN_DOSSIER_VERSION: u16 = 10;

/// Account dossier type identifier (retail WoT 0.6.5 `constants.DOSSIER_TYPE.ACCOUNT`).
pub const DOSSIER_TYPE_ACCOUNT: i32 = 1;
/// Vehicle dossier type identifier (retail WoT 0.6.5 `constants.DOSSIER_TYPE.VEHICLE`).
pub const DOSSIER_TYPE_VEHICLE: i32 = 2;
/// Tankman dossier type identifier in retail WoT 0.6.5 `constants.DOSSIER_TYPE.TANKMAN = 4`
/// (bitflag 1, 2, 4 tested via `dossierType & DOSSIER_TYPE.TANKMAN` in `common/dossiers/helpers.py`).
pub const DOSSIER_TYPE_TANKMAN: i32 = 4;
/// Spec alias for task-3-brief / design spec §167 which wrote 3 (1, 2, 3 sequential numbering).
pub const DOSSIER_TYPE_TANKMAN_SPEC_3: i32 = 3;

/// Non-zero cache version matching `client/account_helpers/DossierCache.py:117-125`
/// so clients starting with `self.__version = 0` reset cache cleanly on first sync.
pub const DOSSIER_CACHE_VERSION: i64 = 1;

#[derive(Debug, PartialEq, Eq)]
pub enum DossierError {
    BufferTooShort { expected: usize, actual: usize },
    InvalidVersion { expected: u16, actual: u16 },
    TruncatedDynamicRecord { offset: usize },
}

impl fmt::Display for DossierError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::BufferTooShort { expected, actual } => {
                write!(f, "buffer too short: expected at least {expected} bytes, got {actual}")
            }
            Self::InvalidVersion { expected, actual } => {
                write!(f, "invalid dossier version: expected {expected}, got {actual}")
            }
            Self::TruncatedDynamicRecord { offset } => {
                write!(f, "truncated dynamic record at offset {offset}")
            }
        }
    }
}

impl std::error::Error for DossierError {}

// --- Cursor / Buffer helpers ---

struct ByteWriter {
    buf: Vec<u8>,
}

impl ByteWriter {
    fn with_capacity(cap: usize) -> Self {
        Self {
            buf: Vec::with_capacity(cap),
        }
    }

    #[inline]
    fn write_u8(&mut self, val: u8) {
        self.buf.push(val);
    }

    #[inline]
    fn write_u16(&mut self, val: u16) {
        self.buf.extend_from_slice(&val.to_le_bytes());
    }

    #[inline]
    fn write_u32(&mut self, val: u32) {
        self.buf.extend_from_slice(&val.to_le_bytes());
    }

    fn into_inner(self) -> Vec<u8> {
        self.buf
    }
}

struct ByteReader<'a> {
    buf: &'a [u8],
    pos: usize,
}

impl<'a> ByteReader<'a> {
    fn new(buf: &'a [u8]) -> Self {
        Self { buf, pos: 0 }
    }

    #[inline]
    fn read_u8(&mut self) -> Result<u8, DossierError> {
        if self.pos >= self.buf.len() {
            return Err(DossierError::BufferTooShort {
                expected: self.pos + 1,
                actual: self.buf.len(),
            });
        }
        let b = self.buf[self.pos];
        self.pos += 1;
        Ok(b)
    }

    #[inline]
    fn read_u16(&mut self) -> Result<u16, DossierError> {
        if self.pos + 2 > self.buf.len() {
            return Err(DossierError::BufferTooShort {
                expected: self.pos + 2,
                actual: self.buf.len(),
            });
        }
        let val = u16::from_le_bytes(self.buf[self.pos..self.pos + 2].try_into().unwrap());
        self.pos += 2;
        Ok(val)
    }

    #[inline]
    fn read_u32(&mut self) -> Result<u32, DossierError> {
        if self.pos + 4 > self.buf.len() {
            return Err(DossierError::BufferTooShort {
                expected: self.pos + 4,
                actual: self.buf.len(),
            });
        }
        let val = u32::from_le_bytes(self.buf[self.pos..self.pos + 4].try_into().unwrap());
        self.pos += 4;
        Ok(val)
    }
}

// --- Dynamic table codecs ---

fn write_veh_type_frags(w: &mut ByteWriter, frags: &BTreeMap<u32, u16>) {
    let count = frags.len() as u16;
    w.write_u16(count);
    for &k in frags.keys() {
        w.write_u32(k);
    }
    for &v in frags.values() {
        w.write_u16(v);
    }
}

fn read_veh_type_frags(buf: &[u8], offset: usize) -> Result<BTreeMap<u32, u16>, DossierError> {
    if buf.len() < offset + 2 {
        return Err(DossierError::TruncatedDynamicRecord { offset });
    }
    let count = u16::from_le_bytes(buf[offset..offset + 2].try_into().unwrap()) as usize;
    let needed = offset + 2 + 6 * count;
    if buf.len() < needed {
        return Err(DossierError::TruncatedDynamicRecord { offset });
    }
    let keys_offset = offset + 2;
    let vals_offset = keys_offset + 4 * count;
    let mut frags = BTreeMap::new();
    for i in 0..count {
        let k = u32::from_le_bytes(
            buf[keys_offset + 4 * i..keys_offset + 4 * i + 4]
                .try_into()
                .unwrap(),
        );
        let v = u16::from_le_bytes(
            buf[vals_offset + 2 * i..vals_offset + 2 * i + 2]
                .try_into()
                .unwrap(),
        );
        frags.insert(k, v);
    }
    Ok(frags)
}

fn write_veh_dossiers_cut(w: &mut ByteWriter, cut: &BTreeMap<u32, (u32, u32)>) {
    let count = cut.len() as u16;
    w.write_u16(count);
    for (&k, &(battles, wins)) in cut {
        w.write_u32(k);
        w.write_u32(battles);
        w.write_u32(wins);
    }
}

fn read_veh_dossiers_cut(
    buf: &[u8],
    offset: usize,
) -> Result<BTreeMap<u32, (u32, u32)>, DossierError> {
    if buf.len() < offset + 2 {
        return Err(DossierError::TruncatedDynamicRecord { offset });
    }
    let count = u16::from_le_bytes(buf[offset..offset + 2].try_into().unwrap()) as usize;
    let needed = offset + 2 + 12 * count;
    if buf.len() < needed {
        return Err(DossierError::TruncatedDynamicRecord { offset });
    }
    let mut cut = BTreeMap::new();
    let entries_offset = offset + 2;
    for i in 0..count {
        let base = entries_offset + 12 * i;
        let k = u32::from_le_bytes(buf[base..base + 4].try_into().unwrap());
        let battles = u32::from_le_bytes(buf[base + 4..base + 8].try_into().unwrap());
        let wins = u32::from_le_bytes(buf[base + 8..base + 12].try_into().unwrap());
        cut.insert(k, (battles, wins));
    }
    Ok(cut)
}

// --- Account Dossier v19 ---

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccountDossier {
    pub version: u16,
    pub creation_time: u32,
    pub max_xp_vehicle: u32,
    pub max_frags_vehicle: u32,
    pub last_battle_time: u32,
    pub battle_life_time: u32,
    pub max_frags: u8,
    pub xp: u32,
    pub max_xp: u16,
    pub battles_count: u32,
    pub wins: u32,
    pub losses: u32,
    pub survived_battles: u32,
    pub win_and_survived: u32,
    pub frags: u32,
    pub frags_8p: u32,
    pub frags_beast: u32,
    pub shots: u32,
    pub hits: u32,
    pub spotted: u32,
    pub damage_dealt: u32,
    pub damage_received: u32,
    pub trees_cut: u16,
    pub capture_points: u32,
    pub dropped_capture_points: u32,
    pub sniper_series: u16,
    pub max_sniper_series: u16,
    pub invincible_series: u8,
    pub max_invincible_series: u8,
    pub diehard_series: u8,
    pub max_diehard_series: u8,
    pub killing_series: u8,
    pub max_killing_series: u8,
    pub piercing_series: u8,
    pub max_piercing_series: u8,
    pub battle_heroes: u16,
    pub warrior: u16,
    pub invader: u16,
    pub sniper: u16,
    pub defender: u16,
    pub steelwall: u16,
    pub supporter: u16,
    pub scout: u16,
    pub medal_kay: u8,
    pub medal_carius: u8,
    pub medal_knispel: u8,
    pub medal_poppel: u8,
    pub medal_abrams: u8,
    pub medal_le_clerc: u8,
    pub medal_lavrinenko: u8,
    pub medal_ekins: u8,
    pub medal_wittmann: u8,
    pub medal_orlik: u8,
    pub medal_oskin: u8,
    pub medal_halonen: u8,
    pub medal_burda: u8,
    pub medal_billotte: u8,
    pub medal_kolobanov: u8,
    pub medal_fadin: u8,
    pub beasthunter: u8,
    pub mousebane: u8,
    pub tank_expert: u8,
    pub title_sniper: u8,
    pub invincible: u8,
    pub diehard: u8,
    pub raider: u8,
    pub hand_of_death: u8,
    pub armor_piercer: u8,
    pub kamikaze: u8,
    pub lumberjack: u8,
    pub veh_type_frags: BTreeMap<u32, u16>,
    pub veh_dossiers_cut: BTreeMap<u32, (u32, u32)>,
}

impl AccountDossier {
    pub fn new(creation_time: u32) -> Self {
        Self {
            version: ACCOUNT_DOSSIER_VERSION,
            creation_time,
            max_xp_vehicle: 0,
            max_frags_vehicle: 0,
            last_battle_time: 0,
            battle_life_time: 0,
            max_frags: 0,
            xp: 0,
            max_xp: 0,
            battles_count: 0,
            wins: 0,
            losses: 0,
            survived_battles: 0,
            win_and_survived: 0,
            frags: 0,
            frags_8p: 0,
            frags_beast: 0,
            shots: 0,
            hits: 0,
            spotted: 0,
            damage_dealt: 0,
            damage_received: 0,
            trees_cut: 0,
            capture_points: 0,
            dropped_capture_points: 0,
            sniper_series: 0,
            max_sniper_series: 0,
            invincible_series: 0,
            max_invincible_series: 0,
            diehard_series: 0,
            max_diehard_series: 0,
            killing_series: 0,
            max_killing_series: 0,
            piercing_series: 0,
            max_piercing_series: 0,
            battle_heroes: 0,
            warrior: 0,
            invader: 0,
            sniper: 0,
            defender: 0,
            steelwall: 0,
            supporter: 0,
            scout: 0,
            medal_kay: 0,
            medal_carius: 0,
            medal_knispel: 0,
            medal_poppel: 0,
            medal_abrams: 0,
            medal_le_clerc: 0,
            medal_lavrinenko: 0,
            medal_ekins: 0,
            medal_wittmann: 0,
            medal_orlik: 0,
            medal_oskin: 0,
            medal_halonen: 0,
            medal_burda: 0,
            medal_billotte: 0,
            medal_kolobanov: 0,
            medal_fadin: 0,
            beasthunter: 0,
            mousebane: 0,
            tank_expert: 0,
            title_sniper: 0,
            invincible: 0,
            diehard: 0,
            raider: 0,
            hand_of_death: 0,
            armor_piercer: 0,
            kamikaze: 0,
            lumberjack: 0,
            veh_type_frags: BTreeMap::new(),
            veh_dossiers_cut: BTreeMap::new(),
        }
    }

    pub fn to_bytes(&self) -> Vec<u8> {
        let frags_size = 2 + 6 * self.veh_type_frags.len();
        let cut_size = 2 + 12 * self.veh_dossiers_cut.len();
        let total_size = 150 + frags_size + cut_size;
        let mut w = ByteWriter::with_capacity(total_size);

        w.write_u16(self.version);
        w.write_u32(self.creation_time);
        w.write_u32(self.max_xp_vehicle);
        w.write_u32(self.max_frags_vehicle);
        w.write_u32(self.last_battle_time);
        w.write_u32(self.battle_life_time);
        w.write_u8(self.max_frags);
        w.write_u32(self.xp);
        w.write_u16(self.max_xp);
        w.write_u32(self.battles_count);
        w.write_u32(self.wins);
        w.write_u32(self.losses);
        w.write_u32(self.survived_battles);
        w.write_u32(self.win_and_survived);
        w.write_u32(self.frags);
        w.write_u32(self.frags_8p);
        w.write_u32(self.frags_beast);
        w.write_u32(self.shots);
        w.write_u32(self.hits);
        w.write_u32(self.spotted);
        w.write_u32(self.damage_dealt);
        w.write_u32(self.damage_received);
        w.write_u16(self.trees_cut);
        w.write_u32(self.capture_points);
        w.write_u32(self.dropped_capture_points);
        w.write_u16(self.sniper_series);
        w.write_u16(self.max_sniper_series);
        w.write_u8(self.invincible_series);
        w.write_u8(self.max_invincible_series);
        w.write_u8(self.diehard_series);
        w.write_u8(self.max_diehard_series);
        w.write_u8(self.killing_series);
        w.write_u8(self.max_killing_series);
        w.write_u8(self.piercing_series);
        w.write_u8(self.max_piercing_series);
        w.write_u16(self.battle_heroes);
        w.write_u16(self.warrior);
        w.write_u16(self.invader);
        w.write_u16(self.sniper);
        w.write_u16(self.defender);
        w.write_u16(self.steelwall);
        w.write_u16(self.supporter);
        w.write_u16(self.scout);
        w.write_u8(self.medal_kay);
        w.write_u8(self.medal_carius);
        w.write_u8(self.medal_knispel);
        w.write_u8(self.medal_poppel);
        w.write_u8(self.medal_abrams);
        w.write_u8(self.medal_le_clerc);
        w.write_u8(self.medal_lavrinenko);
        w.write_u8(self.medal_ekins);
        w.write_u8(self.medal_wittmann);
        w.write_u8(self.medal_orlik);
        w.write_u8(self.medal_oskin);
        w.write_u8(self.medal_halonen);
        w.write_u8(self.medal_burda);
        w.write_u8(self.medal_billotte);
        w.write_u8(self.medal_kolobanov);
        w.write_u8(self.medal_fadin);
        w.write_u8(self.beasthunter);
        w.write_u8(self.mousebane);
        w.write_u8(self.tank_expert);
        w.write_u8(self.title_sniper);
        w.write_u8(self.invincible);
        w.write_u8(self.diehard);
        w.write_u8(self.raider);
        w.write_u8(self.hand_of_death);
        w.write_u8(self.armor_piercer);
        w.write_u8(self.kamikaze);
        w.write_u8(self.lumberjack);

        // _dynRecPos_account at offset 146..150
        let offset_frags = 150u16;
        let offset_cut = (150 + frags_size) as u16;
        w.write_u16(offset_frags);
        w.write_u16(offset_cut);

        // Dynamic tables
        write_veh_type_frags(&mut w, &self.veh_type_frags);
        write_veh_dossiers_cut(&mut w, &self.veh_dossiers_cut);

        w.into_inner()
    }

    pub fn from_bytes(bytes: &[u8]) -> Result<Self, DossierError> {
        if bytes.len() < 154 {
            return Err(DossierError::BufferTooShort {
                expected: 154,
                actual: bytes.len(),
            });
        }
        let mut r = ByteReader::new(bytes);
        let version = r.read_u16()?;
        if version != ACCOUNT_DOSSIER_VERSION {
            return Err(DossierError::InvalidVersion {
                expected: ACCOUNT_DOSSIER_VERSION,
                actual: version,
            });
        }

        let creation_time = r.read_u32()?;
        let max_xp_vehicle = r.read_u32()?;
        let max_frags_vehicle = r.read_u32()?;
        let last_battle_time = r.read_u32()?;
        let battle_life_time = r.read_u32()?;
        let max_frags = r.read_u8()?;
        let xp = r.read_u32()?;
        let max_xp = r.read_u16()?;
        let battles_count = r.read_u32()?;
        let wins = r.read_u32()?;
        let losses = r.read_u32()?;
        let survived_battles = r.read_u32()?;
        let win_and_survived = r.read_u32()?;
        let frags = r.read_u32()?;
        let frags_8p = r.read_u32()?;
        let frags_beast = r.read_u32()?;
        let shots = r.read_u32()?;
        let hits = r.read_u32()?;
        let spotted = r.read_u32()?;
        let damage_dealt = r.read_u32()?;
        let damage_received = r.read_u32()?;
        let trees_cut = r.read_u16()?;
        let capture_points = r.read_u32()?;
        let dropped_capture_points = r.read_u32()?;
        let sniper_series = r.read_u16()?;
        let max_sniper_series = r.read_u16()?;
        let invincible_series = r.read_u8()?;
        let max_invincible_series = r.read_u8()?;
        let diehard_series = r.read_u8()?;
        let max_diehard_series = r.read_u8()?;
        let killing_series = r.read_u8()?;
        let max_killing_series = r.read_u8()?;
        let piercing_series = r.read_u8()?;
        let max_piercing_series = r.read_u8()?;
        let battle_heroes = r.read_u16()?;
        let warrior = r.read_u16()?;
        let invader = r.read_u16()?;
        let sniper = r.read_u16()?;
        let defender = r.read_u16()?;
        let steelwall = r.read_u16()?;
        let supporter = r.read_u16()?;
        let scout = r.read_u16()?;
        let medal_kay = r.read_u8()?;
        let medal_carius = r.read_u8()?;
        let medal_knispel = r.read_u8()?;
        let medal_poppel = r.read_u8()?;
        let medal_abrams = r.read_u8()?;
        let medal_le_clerc = r.read_u8()?;
        let medal_lavrinenko = r.read_u8()?;
        let medal_ekins = r.read_u8()?;
        let medal_wittmann = r.read_u8()?;
        let medal_orlik = r.read_u8()?;
        let medal_oskin = r.read_u8()?;
        let medal_halonen = r.read_u8()?;
        let medal_burda = r.read_u8()?;
        let medal_billotte = r.read_u8()?;
        let medal_kolobanov = r.read_u8()?;
        let medal_fadin = r.read_u8()?;
        let beasthunter = r.read_u8()?;
        let mousebane = r.read_u8()?;
        let tank_expert = r.read_u8()?;
        let title_sniper = r.read_u8()?;
        let invincible = r.read_u8()?;
        let diehard = r.read_u8()?;
        let raider = r.read_u8()?;
        let hand_of_death = r.read_u8()?;
        let armor_piercer = r.read_u8()?;
        let kamikaze = r.read_u8()?;
        let lumberjack = r.read_u8()?;

        // _dynRecPos_account at 146..150
        let offset_frags = r.read_u16()? as usize;
        let offset_cut = r.read_u16()? as usize;

        let veh_type_frags = read_veh_type_frags(bytes, offset_frags)?;
        let veh_dossiers_cut = read_veh_dossiers_cut(bytes, offset_cut)?;

        Ok(Self {
            version,
            creation_time,
            max_xp_vehicle,
            max_frags_vehicle,
            last_battle_time,
            battle_life_time,
            max_frags,
            xp,
            max_xp,
            battles_count,
            wins,
            losses,
            survived_battles,
            win_and_survived,
            frags,
            frags_8p,
            frags_beast,
            shots,
            hits,
            spotted,
            damage_dealt,
            damage_received,
            trees_cut,
            capture_points,
            dropped_capture_points,
            sniper_series,
            max_sniper_series,
            invincible_series,
            max_invincible_series,
            diehard_series,
            max_diehard_series,
            killing_series,
            max_killing_series,
            piercing_series,
            max_piercing_series,
            battle_heroes,
            warrior,
            invader,
            sniper,
            defender,
            steelwall,
            supporter,
            scout,
            medal_kay,
            medal_carius,
            medal_knispel,
            medal_poppel,
            medal_abrams,
            medal_le_clerc,
            medal_lavrinenko,
            medal_ekins,
            medal_wittmann,
            medal_orlik,
            medal_oskin,
            medal_halonen,
            medal_burda,
            medal_billotte,
            medal_kolobanov,
            medal_fadin,
            beasthunter,
            mousebane,
            tank_expert,
            title_sniper,
            invincible,
            diehard,
            raider,
            hand_of_death,
            armor_piercer,
            kamikaze,
            lumberjack,
            veh_type_frags,
            veh_dossiers_cut,
        })
    }
}

// --- Vehicle Dossier v17 ---

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VehicleDossier {
    pub version: u16,
    pub last_battle_time: u32,
    pub battle_life_time: u32,
    pub max_frags: u8,
    pub xp: u32,
    pub max_xp: u16,
    pub battles_count: u32,
    pub wins: u32,
    pub losses: u32,
    pub survived_battles: u32,
    pub win_and_survived: u32,
    pub frags: u32,
    pub frags_8p: u32,
    pub frags_beast: u32,
    pub shots: u32,
    pub hits: u32,
    pub spotted: u32,
    pub damage_dealt: u32,
    pub damage_received: u32,
    pub trees_cut: u16,
    pub capture_points: u32,
    pub dropped_capture_points: u32,
    pub sniper_series: u16,
    pub max_sniper_series: u16,
    pub invincible_series: u8,
    pub max_invincible_series: u8,
    pub diehard_series: u8,
    pub max_diehard_series: u8,
    pub killing_series: u8,
    pub max_killing_series: u8,
    pub piercing_series: u8,
    pub max_piercing_series: u8,
    pub battle_heroes: u16,
    pub warrior: u16,
    pub invader: u16,
    pub sniper: u16,
    pub defender: u16,
    pub steelwall: u16,
    pub supporter: u16,
    pub scout: u16,
    pub medal_kay: u8,
    pub medal_carius: u8,
    pub medal_knispel: u8,
    pub medal_poppel: u8,
    pub medal_abrams: u8,
    pub medal_le_clerc: u8,
    pub medal_lavrinenko: u8,
    pub medal_ekins: u8,
    pub medal_wittmann: u8,
    pub medal_orlik: u8,
    pub medal_oskin: u8,
    pub medal_halonen: u8,
    pub medal_burda: u8,
    pub medal_billotte: u8,
    pub medal_kolobanov: u8,
    pub medal_fadin: u8,
    pub beasthunter: u8,
    pub mousebane: u8,
    pub tank_expert: u8,
    pub title_sniper: u8,
    pub invincible: u8,
    pub diehard: u8,
    pub raider: u8,
    pub hand_of_death: u8,
    pub armor_piercer: u8,
    pub kamikaze: u8,
    pub lumberjack: u8,
    pub veh_type_frags: BTreeMap<u32, u16>,
}

impl VehicleDossier {
    pub fn new() -> Self {
        Self {
            version: VEHICLE_DOSSIER_VERSION,
            last_battle_time: 0,
            battle_life_time: 0,
            max_frags: 0,
            xp: 0,
            max_xp: 0,
            battles_count: 0,
            wins: 0,
            losses: 0,
            survived_battles: 0,
            win_and_survived: 0,
            frags: 0,
            frags_8p: 0,
            frags_beast: 0,
            shots: 0,
            hits: 0,
            spotted: 0,
            damage_dealt: 0,
            damage_received: 0,
            trees_cut: 0,
            capture_points: 0,
            dropped_capture_points: 0,
            sniper_series: 0,
            max_sniper_series: 0,
            invincible_series: 0,
            max_invincible_series: 0,
            diehard_series: 0,
            max_diehard_series: 0,
            killing_series: 0,
            max_killing_series: 0,
            piercing_series: 0,
            max_piercing_series: 0,
            battle_heroes: 0,
            warrior: 0,
            invader: 0,
            sniper: 0,
            defender: 0,
            steelwall: 0,
            supporter: 0,
            scout: 0,
            medal_kay: 0,
            medal_carius: 0,
            medal_knispel: 0,
            medal_poppel: 0,
            medal_abrams: 0,
            medal_le_clerc: 0,
            medal_lavrinenko: 0,
            medal_ekins: 0,
            medal_wittmann: 0,
            medal_orlik: 0,
            medal_oskin: 0,
            medal_halonen: 0,
            medal_burda: 0,
            medal_billotte: 0,
            medal_kolobanov: 0,
            medal_fadin: 0,
            beasthunter: 0,
            mousebane: 0,
            tank_expert: 0,
            title_sniper: 0,
            invincible: 0,
            diehard: 0,
            raider: 0,
            hand_of_death: 0,
            armor_piercer: 0,
            kamikaze: 0,
            lumberjack: 0,
            veh_type_frags: BTreeMap::new(),
        }
    }

    pub fn to_bytes(&self) -> Vec<u8> {
        let frags_size = 2 + 6 * self.veh_type_frags.len();
        let total_size = 136 + frags_size;
        let mut w = ByteWriter::with_capacity(total_size);

        w.write_u16(self.version);
        w.write_u32(self.last_battle_time);
        w.write_u32(self.battle_life_time);
        w.write_u8(self.max_frags);
        w.write_u32(self.xp);
        w.write_u16(self.max_xp);
        w.write_u32(self.battles_count);
        w.write_u32(self.wins);
        w.write_u32(self.losses);
        w.write_u32(self.survived_battles);
        w.write_u32(self.win_and_survived);
        w.write_u32(self.frags);
        w.write_u32(self.frags_8p);
        w.write_u32(self.frags_beast);
        w.write_u32(self.shots);
        w.write_u32(self.hits);
        w.write_u32(self.spotted);
        w.write_u32(self.damage_dealt);
        w.write_u32(self.damage_received);
        w.write_u16(self.trees_cut);
        w.write_u32(self.capture_points);
        w.write_u32(self.dropped_capture_points);
        w.write_u16(self.sniper_series);
        w.write_u16(self.max_sniper_series);
        w.write_u8(self.invincible_series);
        w.write_u8(self.max_invincible_series);
        w.write_u8(self.diehard_series);
        w.write_u8(self.max_diehard_series);
        w.write_u8(self.killing_series);
        w.write_u8(self.max_killing_series);
        w.write_u8(self.piercing_series);
        w.write_u8(self.max_piercing_series);
        w.write_u16(self.battle_heroes);
        w.write_u16(self.warrior);
        w.write_u16(self.invader);
        w.write_u16(self.sniper);
        w.write_u16(self.defender);
        w.write_u16(self.steelwall);
        w.write_u16(self.supporter);
        w.write_u16(self.scout);
        w.write_u8(self.medal_kay);
        w.write_u8(self.medal_carius);
        w.write_u8(self.medal_knispel);
        w.write_u8(self.medal_poppel);
        w.write_u8(self.medal_abrams);
        w.write_u8(self.medal_le_clerc);
        w.write_u8(self.medal_lavrinenko);
        w.write_u8(self.medal_ekins);
        w.write_u8(self.medal_wittmann);
        w.write_u8(self.medal_orlik);
        w.write_u8(self.medal_oskin);
        w.write_u8(self.medal_halonen);
        w.write_u8(self.medal_burda);
        w.write_u8(self.medal_billotte);
        w.write_u8(self.medal_kolobanov);
        w.write_u8(self.medal_fadin);
        w.write_u8(self.beasthunter);
        w.write_u8(self.mousebane);
        w.write_u8(self.tank_expert);
        w.write_u8(self.title_sniper);
        w.write_u8(self.invincible);
        w.write_u8(self.diehard);
        w.write_u8(self.raider);
        w.write_u8(self.hand_of_death);
        w.write_u8(self.armor_piercer);
        w.write_u8(self.kamikaze);
        w.write_u8(self.lumberjack);

        // _dynRecPos_vehicle at offset 134..136
        let offset_frags = 136u16;
        w.write_u16(offset_frags);

        // Dynamic table
        write_veh_type_frags(&mut w, &self.veh_type_frags);

        w.into_inner()
    }

    pub fn from_bytes(bytes: &[u8]) -> Result<Self, DossierError> {
        if bytes.len() < 138 {
            return Err(DossierError::BufferTooShort {
                expected: 138,
                actual: bytes.len(),
            });
        }
        let mut r = ByteReader::new(bytes);
        let version = r.read_u16()?;
        if version != VEHICLE_DOSSIER_VERSION {
            return Err(DossierError::InvalidVersion {
                expected: VEHICLE_DOSSIER_VERSION,
                actual: version,
            });
        }

        let last_battle_time = r.read_u32()?;
        let battle_life_time = r.read_u32()?;
        let max_frags = r.read_u8()?;
        let xp = r.read_u32()?;
        let max_xp = r.read_u16()?;
        let battles_count = r.read_u32()?;
        let wins = r.read_u32()?;
        let losses = r.read_u32()?;
        let survived_battles = r.read_u32()?;
        let win_and_survived = r.read_u32()?;
        let frags = r.read_u32()?;
        let frags_8p = r.read_u32()?;
        let frags_beast = r.read_u32()?;
        let shots = r.read_u32()?;
        let hits = r.read_u32()?;
        let spotted = r.read_u32()?;
        let damage_dealt = r.read_u32()?;
        let damage_received = r.read_u32()?;
        let trees_cut = r.read_u16()?;
        let capture_points = r.read_u32()?;
        let dropped_capture_points = r.read_u32()?;
        let sniper_series = r.read_u16()?;
        let max_sniper_series = r.read_u16()?;
        let invincible_series = r.read_u8()?;
        let max_invincible_series = r.read_u8()?;
        let diehard_series = r.read_u8()?;
        let max_diehard_series = r.read_u8()?;
        let killing_series = r.read_u8()?;
        let max_killing_series = r.read_u8()?;
        let piercing_series = r.read_u8()?;
        let max_piercing_series = r.read_u8()?;
        let battle_heroes = r.read_u16()?;
        let warrior = r.read_u16()?;
        let invader = r.read_u16()?;
        let sniper = r.read_u16()?;
        let defender = r.read_u16()?;
        let steelwall = r.read_u16()?;
        let supporter = r.read_u16()?;
        let scout = r.read_u16()?;
        let medal_kay = r.read_u8()?;
        let medal_carius = r.read_u8()?;
        let medal_knispel = r.read_u8()?;
        let medal_poppel = r.read_u8()?;
        let medal_abrams = r.read_u8()?;
        let medal_le_clerc = r.read_u8()?;
        let medal_lavrinenko = r.read_u8()?;
        let medal_ekins = r.read_u8()?;
        let medal_wittmann = r.read_u8()?;
        let medal_orlik = r.read_u8()?;
        let medal_oskin = r.read_u8()?;
        let medal_halonen = r.read_u8()?;
        let medal_burda = r.read_u8()?;
        let medal_billotte = r.read_u8()?;
        let medal_kolobanov = r.read_u8()?;
        let medal_fadin = r.read_u8()?;
        let beasthunter = r.read_u8()?;
        let mousebane = r.read_u8()?;
        let tank_expert = r.read_u8()?;
        let title_sniper = r.read_u8()?;
        let invincible = r.read_u8()?;
        let diehard = r.read_u8()?;
        let raider = r.read_u8()?;
        let hand_of_death = r.read_u8()?;
        let armor_piercer = r.read_u8()?;
        let kamikaze = r.read_u8()?;
        let lumberjack = r.read_u8()?;

        // _dynRecPos_vehicle at 134..136
        let offset_frags = r.read_u16()? as usize;

        let veh_type_frags = read_veh_type_frags(bytes, offset_frags)?;

        Ok(Self {
            version,
            last_battle_time,
            battle_life_time,
            max_frags,
            xp,
            max_xp,
            battles_count,
            wins,
            losses,
            survived_battles,
            win_and_survived,
            frags,
            frags_8p,
            frags_beast,
            shots,
            hits,
            spotted,
            damage_dealt,
            damage_received,
            trees_cut,
            capture_points,
            dropped_capture_points,
            sniper_series,
            max_sniper_series,
            invincible_series,
            max_invincible_series,
            diehard_series,
            max_diehard_series,
            killing_series,
            max_killing_series,
            piercing_series,
            max_piercing_series,
            battle_heroes,
            warrior,
            invader,
            sniper,
            defender,
            steelwall,
            supporter,
            scout,
            medal_kay,
            medal_carius,
            medal_knispel,
            medal_poppel,
            medal_abrams,
            medal_le_clerc,
            medal_lavrinenko,
            medal_ekins,
            medal_wittmann,
            medal_orlik,
            medal_oskin,
            medal_halonen,
            medal_burda,
            medal_billotte,
            medal_kolobanov,
            medal_fadin,
            beasthunter,
            mousebane,
            tank_expert,
            title_sniper,
            invincible,
            diehard,
            raider,
            hand_of_death,
            armor_piercer,
            kamikaze,
            lumberjack,
            veh_type_frags,
        })
    }
}

// --- Tankman Dossier v10 ---

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TankmanDossier {
    pub version: u16,
    pub last_battle_time: u32,
    pub battle_life_time: u32,
    pub max_frags: u8,
    pub xp: u32,
    pub max_xp: u16,
    pub battles_count: u32,
    pub wins: u32,
    pub losses: u32,
    pub survived_battles: u32,
    pub win_and_survived: u32,
    pub frags: u32,
    pub frags_8p: u32,
    pub frags_beast: u32,
    pub shots: u32,
    pub hits: u32,
    pub spotted: u32,
    pub damage_dealt: u32,
    pub damage_received: u32,
    pub trees_cut: u16,
    pub capture_points: u32,
    pub dropped_capture_points: u32,
    pub sniper_series: u16,
    pub max_sniper_series: u16,
    pub invincible_series: u8,
    pub max_invincible_series: u8,
    pub diehard_series: u8,
    pub max_diehard_series: u8,
    pub killing_series: u8,
    pub max_killing_series: u8,
    pub piercing_series: u8,
    pub max_piercing_series: u8,
    pub battle_heroes: u16,
    pub warrior: u16,
    pub invader: u16,
    pub sniper: u16,
    pub defender: u16,
    pub steelwall: u16,
    pub supporter: u16,
    pub scout: u16,
    pub medal_kay: u8,
    pub medal_carius: u8,
    pub medal_knispel: u8,
    pub medal_poppel: u8,
    pub medal_abrams: u8,
    pub medal_le_clerc: u8,
    pub medal_lavrinenko: u8,
    pub medal_ekins: u8,
    pub medal_wittmann: u8,
    pub medal_orlik: u8,
    pub medal_oskin: u8,
    pub medal_halonen: u8,
    pub medal_burda: u8,
    pub medal_billotte: u8,
    pub medal_kolobanov: u8,
    pub medal_fadin: u8,
    pub beasthunter: u8,
    // Note: Tankman descriptor does NOT contain mousebane or tank_expert!
    pub title_sniper: u8,
    pub invincible: u8,
    pub diehard: u8,
    pub raider: u8,
    pub hand_of_death: u8,
    pub armor_piercer: u8,
    pub kamikaze: u8,
    pub lumberjack: u8,
}

impl TankmanDossier {
    pub fn new() -> Self {
        Self {
            version: TANKMAN_DOSSIER_VERSION,
            last_battle_time: 0,
            battle_life_time: 0,
            max_frags: 0,
            xp: 0,
            max_xp: 0,
            battles_count: 0,
            wins: 0,
            losses: 0,
            survived_battles: 0,
            win_and_survived: 0,
            frags: 0,
            frags_8p: 0,
            frags_beast: 0,
            shots: 0,
            hits: 0,
            spotted: 0,
            damage_dealt: 0,
            damage_received: 0,
            trees_cut: 0,
            capture_points: 0,
            dropped_capture_points: 0,
            sniper_series: 0,
            max_sniper_series: 0,
            invincible_series: 0,
            max_invincible_series: 0,
            diehard_series: 0,
            max_diehard_series: 0,
            killing_series: 0,
            max_killing_series: 0,
            piercing_series: 0,
            max_piercing_series: 0,
            battle_heroes: 0,
            warrior: 0,
            invader: 0,
            sniper: 0,
            defender: 0,
            steelwall: 0,
            supporter: 0,
            scout: 0,
            medal_kay: 0,
            medal_carius: 0,
            medal_knispel: 0,
            medal_poppel: 0,
            medal_abrams: 0,
            medal_le_clerc: 0,
            medal_lavrinenko: 0,
            medal_ekins: 0,
            medal_wittmann: 0,
            medal_orlik: 0,
            medal_oskin: 0,
            medal_halonen: 0,
            medal_burda: 0,
            medal_billotte: 0,
            medal_kolobanov: 0,
            medal_fadin: 0,
            beasthunter: 0,
            title_sniper: 0,
            invincible: 0,
            diehard: 0,
            raider: 0,
            hand_of_death: 0,
            armor_piercer: 0,
            kamikaze: 0,
            lumberjack: 0,
        }
    }

    pub fn to_bytes(&self) -> Vec<u8> {
        let mut w = ByteWriter::with_capacity(132);

        w.write_u16(self.version);
        w.write_u32(self.last_battle_time);
        w.write_u32(self.battle_life_time);
        w.write_u8(self.max_frags);
        w.write_u32(self.xp);
        w.write_u16(self.max_xp);
        w.write_u32(self.battles_count);
        w.write_u32(self.wins);
        w.write_u32(self.losses);
        w.write_u32(self.survived_battles);
        w.write_u32(self.win_and_survived);
        w.write_u32(self.frags);
        w.write_u32(self.frags_8p);
        w.write_u32(self.frags_beast);
        w.write_u32(self.shots);
        w.write_u32(self.hits);
        w.write_u32(self.spotted);
        w.write_u32(self.damage_dealt);
        w.write_u32(self.damage_received);
        w.write_u16(self.trees_cut);
        w.write_u32(self.capture_points);
        w.write_u32(self.dropped_capture_points);
        w.write_u16(self.sniper_series);
        w.write_u16(self.max_sniper_series);
        w.write_u8(self.invincible_series);
        w.write_u8(self.max_invincible_series);
        w.write_u8(self.diehard_series);
        w.write_u8(self.max_diehard_series);
        w.write_u8(self.killing_series);
        w.write_u8(self.max_killing_series);
        w.write_u8(self.piercing_series);
        w.write_u8(self.max_piercing_series);
        w.write_u16(self.battle_heroes);
        w.write_u16(self.warrior);
        w.write_u16(self.invader);
        w.write_u16(self.sniper);
        w.write_u16(self.defender);
        w.write_u16(self.steelwall);
        w.write_u16(self.supporter);
        w.write_u16(self.scout);
        w.write_u8(self.medal_kay);
        w.write_u8(self.medal_carius);
        w.write_u8(self.medal_knispel);
        w.write_u8(self.medal_poppel);
        w.write_u8(self.medal_abrams);
        w.write_u8(self.medal_le_clerc);
        w.write_u8(self.medal_lavrinenko);
        w.write_u8(self.medal_ekins);
        w.write_u8(self.medal_wittmann);
        w.write_u8(self.medal_orlik);
        w.write_u8(self.medal_oskin);
        w.write_u8(self.medal_halonen);
        w.write_u8(self.medal_burda);
        w.write_u8(self.medal_billotte);
        w.write_u8(self.medal_kolobanov);
        w.write_u8(self.medal_fadin);
        w.write_u8(self.beasthunter);
        w.write_u8(self.title_sniper);
        w.write_u8(self.invincible);
        w.write_u8(self.diehard);
        w.write_u8(self.raider);
        w.write_u8(self.hand_of_death);
        w.write_u8(self.armor_piercer);
        w.write_u8(self.kamikaze);
        w.write_u8(self.lumberjack);

        w.into_inner()
    }

    pub fn from_bytes(bytes: &[u8]) -> Result<Self, DossierError> {
        if bytes.len() < 132 {
            return Err(DossierError::BufferTooShort {
                expected: 132,
                actual: bytes.len(),
            });
        }
        let mut r = ByteReader::new(bytes);
        let version = r.read_u16()?;
        if version != TANKMAN_DOSSIER_VERSION {
            return Err(DossierError::InvalidVersion {
                expected: TANKMAN_DOSSIER_VERSION,
                actual: version,
            });
        }

        let last_battle_time = r.read_u32()?;
        let battle_life_time = r.read_u32()?;
        let max_frags = r.read_u8()?;
        let xp = r.read_u32()?;
        let max_xp = r.read_u16()?;
        let battles_count = r.read_u32()?;
        let wins = r.read_u32()?;
        let losses = r.read_u32()?;
        let survived_battles = r.read_u32()?;
        let win_and_survived = r.read_u32()?;
        let frags = r.read_u32()?;
        let frags_8p = r.read_u32()?;
        let frags_beast = r.read_u32()?;
        let shots = r.read_u32()?;
        let hits = r.read_u32()?;
        let spotted = r.read_u32()?;
        let damage_dealt = r.read_u32()?;
        let damage_received = r.read_u32()?;
        let trees_cut = r.read_u16()?;
        let capture_points = r.read_u32()?;
        let dropped_capture_points = r.read_u32()?;
        let sniper_series = r.read_u16()?;
        let max_sniper_series = r.read_u16()?;
        let invincible_series = r.read_u8()?;
        let max_invincible_series = r.read_u8()?;
        let diehard_series = r.read_u8()?;
        let max_diehard_series = r.read_u8()?;
        let killing_series = r.read_u8()?;
        let max_killing_series = r.read_u8()?;
        let piercing_series = r.read_u8()?;
        let max_piercing_series = r.read_u8()?;
        let battle_heroes = r.read_u16()?;
        let warrior = r.read_u16()?;
        let invader = r.read_u16()?;
        let sniper = r.read_u16()?;
        let defender = r.read_u16()?;
        let steelwall = r.read_u16()?;
        let supporter = r.read_u16()?;
        let scout = r.read_u16()?;
        let medal_kay = r.read_u8()?;
        let medal_carius = r.read_u8()?;
        let medal_knispel = r.read_u8()?;
        let medal_poppel = r.read_u8()?;
        let medal_abrams = r.read_u8()?;
        let medal_le_clerc = r.read_u8()?;
        let medal_lavrinenko = r.read_u8()?;
        let medal_ekins = r.read_u8()?;
        let medal_wittmann = r.read_u8()?;
        let medal_orlik = r.read_u8()?;
        let medal_oskin = r.read_u8()?;
        let medal_halonen = r.read_u8()?;
        let medal_burda = r.read_u8()?;
        let medal_billotte = r.read_u8()?;
        let medal_kolobanov = r.read_u8()?;
        let medal_fadin = r.read_u8()?;
        let beasthunter = r.read_u8()?;
        let title_sniper = r.read_u8()?;
        let invincible = r.read_u8()?;
        let diehard = r.read_u8()?;
        let raider = r.read_u8()?;
        let hand_of_death = r.read_u8()?;
        let armor_piercer = r.read_u8()?;
        let kamikaze = r.read_u8()?;
        let lumberjack = r.read_u8()?;

        Ok(Self {
            version,
            last_battle_time,
            battle_life_time,
            max_frags,
            xp,
            max_xp,
            battles_count,
            wins,
            losses,
            survived_battles,
            win_and_survived,
            frags,
            frags_8p,
            frags_beast,
            shots,
            hits,
            spotted,
            damage_dealt,
            damage_received,
            trees_cut,
            capture_points,
            dropped_capture_points,
            sniper_series,
            max_sniper_series,
            invincible_series,
            max_invincible_series,
            diehard_series,
            max_diehard_series,
            killing_series,
            max_killing_series,
            piercing_series,
            max_piercing_series,
            battle_heroes,
            warrior,
            invader,
            sniper,
            defender,
            steelwall,
            supporter,
            scout,
            medal_kay,
            medal_carius,
            medal_knispel,
            medal_poppel,
            medal_abrams,
            medal_le_clerc,
            medal_lavrinenko,
            medal_ekins,
            medal_wittmann,
            medal_orlik,
            medal_oskin,
            medal_halonen,
            medal_burda,
            medal_billotte,
            medal_kolobanov,
            medal_fadin,
            beasthunter,
            title_sniper,
            invincible,
            diehard,
            raider,
            hand_of_death,
            armor_piercer,
            kamikaze,
            lumberjack,
        })
    }
}

// --- Dossier Synchronization payload builder ---

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DossierSyncRecord {
    pub dossier_type: i32,
    pub owner_id: i64,
    pub change_time: u32,
    pub comp_descr: Vec<u8>,
}

/// Builds the exact `(actualCacheVersion, [dossierType, ownerID, changeTime, dossierCompDescr, ...])`
/// structure expected by `client/account_helpers/DossierCache.py:114-126`.
pub fn build_dossier_sync_value(records: &[DossierSyncRecord]) -> Value {
    let mut flat = Vec::with_capacity(records.len() * 4);
    for r in records {
        flat.push(Value::Int(r.dossier_type as i64));
        flat.push(Value::Int(r.owner_id));
        flat.push(Value::Int(r.change_time as i64));
        flat.push(Value::Str(r.comp_descr.clone()));
    }
    Value::Tuple(vec![Value::Int(DOSSIER_CACHE_VERSION), Value::List(flat)])
}

/// Alias for `build_dossier_sync_value`.
pub fn build_dossier_sync_payload(records: &[DossierSyncRecord]) -> Value {
    build_dossier_sync_value(records)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_constants() {
        assert_eq!(ACCOUNT_DOSSIER_VERSION, 19);
        assert_eq!(VEHICLE_DOSSIER_VERSION, 17);
        assert_eq!(TANKMAN_DOSSIER_VERSION, 10);
        assert_eq!(DOSSIER_CACHE_VERSION, 1);
        assert_eq!(DOSSIER_TYPE_ACCOUNT, 1);
        assert_eq!(DOSSIER_TYPE_VEHICLE, 2);
        assert_eq!(DOSSIER_TYPE_TANKMAN, 4);
        assert_eq!(DOSSIER_TYPE_TANKMAN_SPEC_3, 3);
    }

    #[test]
    fn test_empty_account_dossier_layout() {
        let creation_time = 1_700_000_000u32;
        let dossier = AccountDossier::new(creation_time);
        let bytes = dossier.to_bytes();

        assert_eq!(bytes.len(), 154, "empty AccountDossier must be exactly 154 bytes");
        assert_eq!(
            u16::from_le_bytes([bytes[0], bytes[1]]),
            19,
            "AccountDossier version must be 19 at @0..2"
        );
        assert_eq!(
            u32::from_le_bytes([bytes[2], bytes[3], bytes[4], bytes[5]]),
            creation_time,
            "creationTime must match at @2..6"
        );

        // _dynRecPos_account at @146..150: (150, 152)
        let pos0 = u16::from_le_bytes([bytes[146], bytes[147]]);
        let pos1 = u16::from_le_bytes([bytes[148], bytes[149]]);
        assert_eq!(pos0, 150, "vehTypeFrags offset must be 150");
        assert_eq!(pos1, 152, "vehDossiersCut offset must be 152 when vehTypeFrags is empty");

        // empty vehTypeFrags count @150..152
        let frags_count = u16::from_le_bytes([bytes[150], bytes[151]]);
        assert_eq!(frags_count, 0, "empty vehTypeFrags count must be 0");

        // empty vehDossiersCut count @152..154
        let cut_count = u16::from_le_bytes([bytes[152], bytes[153]]);
        assert_eq!(cut_count, 0, "empty vehDossiersCut count must be 0");

        // Round-trip
        let decoded = AccountDossier::from_bytes(&bytes).expect("decoding empty AccountDossier");
        assert_eq!(decoded, dossier);
    }

    #[test]
    fn test_populated_account_dossier_roundtrip() {
        let mut dossier = AccountDossier::new(1_700_000_000);
        dossier.battles_count = 100;
        dossier.wins = 60;
        dossier.losses = 38;
        dossier.survived_battles = 50;
        dossier.frags = 120;
        dossier.damage_dealt = 150_000;
        dossier.trees_cut = 15;
        dossier.medal_carius = 3;
        dossier.medal_knispel = 2;
        dossier.tank_expert = 1;
        dossier.mousebane = 1;

        // 2 vehTypeFrags: 6 bytes each + 2 bytes header = 14 bytes
        dossier.veh_type_frags.insert(1001, 5);
        dossier.veh_type_frags.insert(2002, 12);

        // 2 vehDossiersCut: 12 bytes each + 2 bytes header = 26 bytes
        dossier.veh_dossiers_cut.insert(1001, (10, 6));
        dossier.veh_dossiers_cut.insert(2002, (20, 15));

        let bytes = dossier.to_bytes();
        // 150 static + 14 frags + 26 cut = 190 bytes (154 + 12 + 24)
        assert_eq!(bytes.len(), 190, "populated AccountDossier must be 190 bytes");

        let pos0 = u16::from_le_bytes([bytes[146], bytes[147]]);
        let pos1 = u16::from_le_bytes([bytes[148], bytes[149]]);
        assert_eq!(pos0, 150);
        assert_eq!(pos1, 164, "vehDossiersCut offset must be 150 + 2 + 6*2 = 164");

        let decoded = AccountDossier::from_bytes(&bytes).expect("decoding populated AccountDossier");
        assert_eq!(decoded, dossier);
        assert_eq!(decoded.to_bytes(), bytes);
    }

    #[test]
    fn test_empty_vehicle_dossier_layout() {
        let dossier = VehicleDossier::new();
        let bytes = dossier.to_bytes();

        assert_eq!(bytes.len(), 138, "empty VehicleDossier must be exactly 138 bytes");
        assert_eq!(
            u16::from_le_bytes([bytes[0], bytes[1]]),
            17,
            "VehicleDossier version must be 17 at @0..2"
        );

        // _dynRecPos_vehicle at @134..136: 136
        let pos0 = u16::from_le_bytes([bytes[134], bytes[135]]);
        assert_eq!(pos0, 136, "vehTypeFrags offset must be 136");

        // empty vehTypeFrags count @136..138
        let frags_count = u16::from_le_bytes([bytes[136], bytes[137]]);
        assert_eq!(frags_count, 0);

        let decoded = VehicleDossier::from_bytes(&bytes).expect("decoding empty VehicleDossier");
        assert_eq!(decoded, dossier);
    }

    #[test]
    fn test_populated_vehicle_dossier_roundtrip() {
        let mut dossier = VehicleDossier::new();
        dossier.battles_count = 50;
        dossier.wins = 30;
        dossier.shots = 500;
        dossier.hits = 380;
        dossier.spotted = 75;
        dossier.damage_dealt = 60_000;
        dossier.max_frags = 6;
        dossier.mousebane = 2;
        dossier.tank_expert = 1;

        dossier.veh_type_frags.insert(3001, 4);
        dossier.veh_type_frags.insert(3002, 1);
        dossier.veh_type_frags.insert(3003, 9);

        let bytes = dossier.to_bytes();
        // 136 static + 2 + 6*3 = 156 bytes (138 + 18)
        assert_eq!(bytes.len(), 156);

        let decoded = VehicleDossier::from_bytes(&bytes).expect("decoding populated VehicleDossier");
        assert_eq!(decoded, dossier);
        assert_eq!(decoded.to_bytes(), bytes);
    }

    #[test]
    fn test_tankman_dossier_roundtrip() {
        let dossier = TankmanDossier::new();
        let bytes = dossier.to_bytes();

        assert_eq!(bytes.len(), 132, "TankmanDossier must be exact 132 bytes");
        assert_eq!(
            u16::from_le_bytes([bytes[0], bytes[1]]),
            10,
            "TankmanDossier version must be 10 at @0..2"
        );

        let decoded = TankmanDossier::from_bytes(&bytes).expect("decoding empty TankmanDossier");
        assert_eq!(decoded, dossier);

        let mut populated = TankmanDossier::new();
        populated.battles_count = 25;
        populated.survived_battles = 18;
        populated.xp = 12_000;
        populated.shots = 200;
        populated.hits = 160;
        populated.medal_kay = 1;
        populated.warrior = 2;

        let populated_bytes = populated.to_bytes();
        assert_eq!(populated_bytes.len(), 132, "TankmanDossier is always 132 bytes");
        let decoded_populated = TankmanDossier::from_bytes(&populated_bytes).unwrap();
        assert_eq!(decoded_populated, populated);
    }

    #[test]
    fn test_build_dossier_sync_payload() {
        let acc_bytes = AccountDossier::new(1_700_000_000).to_bytes();
        let veh_bytes = VehicleDossier::new().to_bytes();
        let tman_bytes = TankmanDossier::new().to_bytes();

        let records = vec![
            DossierSyncRecord {
                dossier_type: DOSSIER_TYPE_ACCOUNT,
                owner_id: 42,
                change_time: 1_700_000_100,
                comp_descr: acc_bytes.clone(),
            },
            DossierSyncRecord {
                dossier_type: DOSSIER_TYPE_VEHICLE,
                owner_id: 101,
                change_time: 1_700_000_200,
                comp_descr: veh_bytes.clone(),
            },
            DossierSyncRecord {
                dossier_type: DOSSIER_TYPE_TANKMAN,
                owner_id: 5,
                change_time: 1_700_000_300,
                comp_descr: tman_bytes.clone(),
            },
        ];

        let val = build_dossier_sync_payload(&records);

        match val {
            Value::Tuple(items) => {
                assert_eq!(items.len(), 2, "dossier sync value must be a 2-tuple");
                assert_eq!(items[0].as_int(), Some(DOSSIER_CACHE_VERSION));
                match &items[1] {
                    Value::List(flat) => {
                        assert_eq!(flat.len(), 12, "3 records * 4 fields = 12 items");
                        // quad 0
                        assert_eq!(flat[0].as_int(), Some(DOSSIER_TYPE_ACCOUNT as i64));
                        assert_eq!(flat[1].as_int(), Some(42));
                        assert_eq!(flat[2].as_int(), Some(1_700_000_100));
                        assert_eq!(flat[3].as_bytes(), Some(acc_bytes.as_slice()));
                        // quad 1
                        assert_eq!(flat[4].as_int(), Some(DOSSIER_TYPE_VEHICLE as i64));
                        assert_eq!(flat[5].as_int(), Some(101));
                        assert_eq!(flat[6].as_int(), Some(1_700_000_200));
                        assert_eq!(flat[7].as_bytes(), Some(veh_bytes.as_slice()));
                        // quad 2
                        assert_eq!(flat[8].as_int(), Some(DOSSIER_TYPE_TANKMAN as i64));
                        assert_eq!(flat[9].as_int(), Some(5));
                        assert_eq!(flat[10].as_int(), Some(1_700_000_300));
                        assert_eq!(flat[11].as_bytes(), Some(tman_bytes.as_slice()));
                    }
                    other => panic!("expected List of flat quadruples, got {other:?}"),
                }
            }
            other => panic!("expected Tuple, got {other:?}"),
        }

        // Empty records test
        let empty_val = build_dossier_sync_value(&[]);
        match empty_val {
            Value::Tuple(items) => {
                assert_eq!(items[0].as_int(), Some(DOSSIER_CACHE_VERSION));
                match &items[1] {
                    Value::List(flat) => assert!(flat.is_empty()),
                    other => panic!("expected empty list, got {other:?}"),
                }
            }
            other => panic!("expected Tuple, got {other:?}"),
        }
    }

    #[test]
    fn test_error_handling() {
        assert!(matches!(
            AccountDossier::from_bytes(&[0u8; 100]),
            Err(DossierError::BufferTooShort { expected: 154, actual: 100 })
        ));
        assert!(matches!(
            VehicleDossier::from_bytes(&[0u8; 100]),
            Err(DossierError::BufferTooShort { expected: 138, actual: 100 })
        ));
        assert!(matches!(
            TankmanDossier::from_bytes(&[0u8; 100]),
            Err(DossierError::BufferTooShort { expected: 132, actual: 100 })
        ));

        // Wrong version
        let mut wrong_acc = [0u8; 154];
        wrong_acc[0..2].copy_from_slice(&99u16.to_le_bytes());
        assert!(matches!(
            AccountDossier::from_bytes(&wrong_acc),
            Err(DossierError::InvalidVersion { expected: 19, actual: 99 })
        ));

        let mut wrong_veh = [0u8; 138];
        wrong_veh[0..2].copy_from_slice(&99u16.to_le_bytes());
        assert!(matches!(
            VehicleDossier::from_bytes(&wrong_veh),
            Err(DossierError::InvalidVersion { expected: 17, actual: 99 })
        ));

        let mut wrong_tman = [0u8; 132];
        wrong_tman[0..2].copy_from_slice(&99u16.to_le_bytes());
        assert!(matches!(
            TankmanDossier::from_bytes(&wrong_tman),
            Err(DossierError::InvalidVersion { expected: 10, actual: 99 })
        ));

        // Truncated dynamic record in account
        let mut truncated_acc = AccountDossier::new(1_700_000_000).to_bytes();
        // Say vehTypeFrags count is 5 (at 150..152), but buffer is only 154 bytes
        truncated_acc[150..152].copy_from_slice(&5u16.to_le_bytes());
        assert!(matches!(
            AccountDossier::from_bytes(&truncated_acc),
            Err(DossierError::TruncatedDynamicRecord { offset: 150 })
        ));
    }

    #[test]
    fn test_exact_static_field_offsets() {
        // Verify AccountDossier field positions by placing sentinel values
        let mut acc = AccountDossier::new(0);
        acc.creation_time = 0x12345678;
        acc.max_xp_vehicle = 0x23456789;
        acc.max_frags_vehicle = 0x3456789A;
        acc.last_battle_time = 0x456789AB;
        acc.battle_life_time = 0x56789ABC;
        acc.max_frags = 0x7F;
        acc.xp = 0x6789ABCD;
        acc.max_xp = 0x1234;
        acc.battles_count = 0x789ABCDE;
        acc.trees_cut = 0x5678;
        acc.sniper_series = 0x9ABC;
        acc.max_sniper_series = 0xBCDE;
        acc.invincible_series = 0x11;
        acc.battle_heroes = 0x2233;
        acc.medal_kay = 0x44;
        acc.beasthunter = 0x55;
        acc.mousebane = 0x66;
        acc.tank_expert = 0x77;
        acc.title_sniper = 0x88;
        acc.lumberjack = 0x99;

        let b = acc.to_bytes();
        assert_eq!(&b[2..6], &0x12345678u32.to_le_bytes(), "creationTime @2..6");
        assert_eq!(&b[6..10], &0x23456789u32.to_le_bytes(), "maxXPVehicle @6..10");
        assert_eq!(&b[10..14], &0x3456789Au32.to_le_bytes(), "maxFragsVehicle @10..14");
        assert_eq!(&b[14..18], &0x456789ABu32.to_le_bytes(), "lastBattleTime @14..18");
        assert_eq!(&b[18..22], &0x56789ABCu32.to_le_bytes(), "battleLifeTime @18..22");
        assert_eq!(b[22], 0x7F, "maxFrags @22");
        assert_eq!(&b[23..27], &0x6789ABCDu32.to_le_bytes(), "xp @23..27");
        assert_eq!(&b[27..29], &0x1234u16.to_le_bytes(), "maxXP @27..29");
        assert_eq!(&b[29..33], &0x789ABCDEu32.to_le_bytes(), "battlesCount @29..33");
        assert_eq!(&b[81..83], &0x5678u16.to_le_bytes(), "treesCut @81..83");
        assert_eq!(&b[91..93], &0x9ABCu16.to_le_bytes(), "sniperSeries @91..93");
        assert_eq!(&b[93..95], &0xBCDEu16.to_le_bytes(), "maxSniperSeries @93..95");
        assert_eq!(b[95], 0x11, "invincibleSeries @95");
        assert_eq!(&b[103..105], &0x2233u16.to_le_bytes(), "battleHeroes @103..105");
        assert_eq!(b[119], 0x44, "medalKay @119");
        assert_eq!(b[135], 0x55, "beasthunter @135");
        assert_eq!(b[136], 0x66, "mousebane @136");
        assert_eq!(b[137], 0x77, "tankExpert @137");
        assert_eq!(b[138], 0x88, "titleSniper @138");
        assert_eq!(b[145], 0x99, "lumberjack @145");
        assert_eq!(&b[146..148], &150u16.to_le_bytes(), "_dynRecPos_account[0] @146..148");
        assert_eq!(&b[148..150], &152u16.to_le_bytes(), "_dynRecPos_account[1] @148..150");

        // Verify VehicleDossier field positions
        let mut veh = VehicleDossier::new();
        veh.last_battle_time = 0x11223344;
        veh.battle_life_time = 0x55667788;
        veh.max_frags = 0x09;
        veh.xp = 0x01020304;
        veh.trees_cut = 0x0A0B;
        veh.medal_kay = 0x33;
        veh.beasthunter = 0x44;
        veh.mousebane = 0x55;
        veh.tank_expert = 0x66;
        veh.title_sniper = 0x77;
        veh.lumberjack = 0x88;

        let vb = veh.to_bytes();
        assert_eq!(&vb[2..6], &0x11223344u32.to_le_bytes(), "veh lastBattleTime @2..6");
        assert_eq!(&vb[6..10], &0x55667788u32.to_le_bytes(), "veh battleLifeTime @6..10");
        assert_eq!(vb[10], 0x09, "veh maxFrags @10");
        assert_eq!(&vb[11..15], &0x01020304u32.to_le_bytes(), "veh xp @11..15");
        assert_eq!(&vb[69..71], &0x0A0Bu16.to_le_bytes(), "veh treesCut @69..71");
        assert_eq!(vb[107], 0x33, "veh medalKay @107");
        assert_eq!(vb[123], 0x44, "veh beasthunter @123");
        assert_eq!(vb[124], 0x55, "veh mousebane @124");
        assert_eq!(vb[125], 0x66, "veh tankExpert @125");
        assert_eq!(vb[126], 0x77, "veh titleSniper @126");
        assert_eq!(vb[133], 0x88, "veh lumberjack @133");
        assert_eq!(&vb[134..136], &136u16.to_le_bytes(), "veh _dynRecPos_vehicle @134..136");

        // Verify TankmanDossier field positions
        let mut tman = TankmanDossier::new();
        tman.last_battle_time = 0x11223344;
        tman.beasthunter = 0x44;
        tman.title_sniper = 0x77; // at 124, right after beasthunter at 123!
        tman.lumberjack = 0x88; // at 131!

        let tb = tman.to_bytes();
        assert_eq!(&tb[2..6], &0x11223344u32.to_le_bytes(), "tman lastBattleTime @2..6");
        assert_eq!(tb[123], 0x44, "tman beasthunter @123");
        assert_eq!(tb[124], 0x77, "tman titleSniper @124 (no mousebane/tankExpert)");
        assert_eq!(tb[131], 0x88, "tman lumberjack @131");
        assert_eq!(tb.len(), 132, "tman exact length 132");
    }
}
