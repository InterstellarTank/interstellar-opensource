use std::collections::{BTreeMap, HashMap};

use crate::lobby::BattleMode;

#[derive(Debug, Clone, Default)]
pub struct FriendlyFireVictim {
    pub damage: u32,
    pub destroyed: bool,
}

#[derive(Debug, Clone, Default)]
pub struct FriendlyFireStats {
    pub rating: f32,
    pub victims: BTreeMap<i32, FriendlyFireVictim>,
    flagged: bool,
}

#[derive(Debug)]
pub struct TkillTracker {
    mode: BattleMode,
    offenders: HashMap<i32, FriendlyFireStats>,
}

impl TkillTracker {
    pub fn new(mode: BattleMode) -> Self {
        Self {
            mode,
            offenders: HashMap::new(),
        }
    }

    /// Track damage already confirmed to be against an ally. Returns true on the first flag.
    pub fn record(
        &mut self,
        attacker: i32,
        target: i32,
        damage: u16,
        target_max_hp: u16,
        destroyed: bool,
    ) -> bool {
        if self.mode != BattleMode::Random || attacker == target || (damage == 0 && !destroyed) {
            return false;
        }
        let stats = self.offenders.entry(attacker).or_default();
        let victim = stats.victims.entry(target).or_default();
        victim.damage += u32::from(damage);
        victim.destroyed |= destroyed;
        stats.rating += f32::from(damage) / f32::from(target_max_hp.max(1));
        let flagged = stats.flagged;
        stats.flagged |= stats.rating >= 0.5 || destroyed;
        stats.flagged && !flagged
    }

    pub fn offenses(&self, attacker: i32) -> Option<&FriendlyFireStats> {
        self.offenders.get(&attacker)
    }

    pub fn compensation_damage(&self, victim_id: i32) -> u32 {
        self.offenders
            .values()
            .filter_map(|s| s.victims.get(&victim_id))
            .map(|victim| victim.damage)
            .sum()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn friendly_fire_triggers_team_killer_in_random_only() {
        let mut random = TkillTracker::new(BattleMode::Random);
        assert!(!random.record(1, 2, 30, 100, false));
        assert!(random.record(1, 2, 30, 100, false));
        assert!(!random.record(1, 2, 20, 100, true));
        assert_eq!(random.offenses(1).unwrap().victims[&2].damage, 80);

        let mut training = TkillTracker::new(BattleMode::Training);
        assert!(!training.record(1, 2, 100, 100, true));
        assert!(training.offenses(1).is_none());
    }
}
