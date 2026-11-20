use std::collections::BTreeMap;

use crate::assets::TeamBaseSpec;
use crate::sim::VehicleSimState;

const CAPTURE_POINTS: f64 = 100.0;

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum BaseCaptureEvent {
    Points {
        team: u8,
        base_id: usize,
        points: u8,
    },
    Captured {
        team: u8,
    },
    Reset {
        attacker_id: i32,
        points: f32,
    },
}

#[derive(Debug)]
struct TeamBaseStatus {
    spec: TeamBaseSpec,
    invaders: BTreeMap<i32, f64>,
    points: u8,
    captured: bool,
}

#[derive(Debug)]
pub struct BaseCaptureTracker {
    bases: Vec<TeamBaseStatus>,
}

impl BaseCaptureTracker {
    pub fn new(bases: &[TeamBaseSpec]) -> Self {
        Self {
            bases: bases
                .iter()
                .cloned()
                .map(|spec| TeamBaseStatus {
                    spec,
                    invaders: BTreeMap::new(),
                    points: 0,
                    captured: false,
                })
                .collect(),
        }
    }

    pub fn points_for(&self, vehicle_id: i32) -> f32 {
        self.bases
            .iter()
            .filter_map(|base| base.invaders.get(&vehicle_id))
            .sum::<f64>() as f32
    }

    pub fn step_capture(
        &mut self,
        vehicles: &[VehicleSimState],
        damage_hits: &[(i32, i32)],
        dt: f32,
    ) -> Vec<BaseCaptureEvent> {
        let mut interrupted = BTreeMap::new();
        for &(attacker, target) in damage_hits {
            interrupted.entry(target).or_insert(attacker);
        }
        let seconds = if dt.is_finite() {
            f64::from(dt.max(0.0))
        } else {
            0.0
        };
        let mut events = Vec::new();
        for (base_id, base) in self.bases.iter_mut().enumerate() {
            if base.captured || !matches!(base.spec.team, 1 | 2) {
                continue;
            }
            base.score(base_id, vehicles, &interrupted, seconds, &mut events);
        }
        events
    }
}

impl TeamBaseStatus {
    fn score(
        &mut self,
        base_id: usize,
        vehicles: &[VehicleSimState],
        interrupted: &BTreeMap<i32, i32>,
        seconds: f64,
        events: &mut Vec<BaseCaptureEvent>,
    ) {
        for (&vehicle, &attacker_id) in interrupted {
            if let Some(points) = self.invaders.remove(&vehicle).filter(|&n| n > 0.0) {
                events.push(BaseCaptureEvent::Reset {
                    attacker_id,
                    points: points as f32,
                });
            }
        }

        let owner = self.spec.team;
        let enemy = 3 - owner;
        let mut current = BTreeMap::new();
        let mut contested = false;
        for v in vehicles.iter().filter(|v| v.is_alive && v.health > 0) {
            let offset = v.physics.pos - self.spec.position;
            if offset.x * offset.x + offset.z * offset.z > self.spec.radius.powi(2) {
                continue;
            }
            if v.team == enemy {
                current.insert(
                    v.entity_id,
                    self.invaders.get(&v.entity_id).copied().unwrap_or(0.0),
                );
            } else if v.team == owner && self.spec.stop_invaders {
                contested = true;
            }
        }

        let before: f64 = current.values().sum();
        let mut gain = 0.0;
        if !contested {
            let count = current
                .keys()
                .filter(|id| !interrupted.contains_key(id))
                .count() as f64;
            let rate = (f64::from(self.spec.points_per_second) * count)
                .min(f64::from(self.spec.max_points_per_second))
                .max(0.0);
            if count > 0.0 {
                gain = (rate * seconds).min((CAPTURE_POINTS - before).max(0.0));
                for (id, points) in &mut current {
                    if !interrupted.contains_key(id) {
                        *points += gain / count;
                    }
                }
            }
        }
        self.invaders = current;
        let total = before + gain;
        self.captured = total >= CAPTURE_POINTS - 1e-8;
        let points = if self.captured {
            100
        } else {
            (total.round() as u8).min(99)
        };
        if points != self.points {
            self.points = points;
            events.push(BaseCaptureEvent::Points {
                team: owner,
                base_id,
                points,
            });
        }
        if self.captured {
            events.push(BaseCaptureEvent::Captured { team: owner });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{BaseCaptureEvent, BaseCaptureTracker};
    use crate::assets::TeamBaseSpec;
    use crate::sim::VehicleSimState;
    use glam::Vec3;

    #[test]
    fn only_an_opposing_playing_team_can_score_on_the_base() {
        let mut capture = BaseCaptureTracker::new(&[TeamBaseSpec {
            team: 2,
            position: Vec3::ZERO,
            radius: 10.0,
            points_per_second: 1.0,
            max_points_per_second: 3.0,
            stop_invaders: true,
        }]);
        let observer = VehicleSimState {
            entity_id: 9,
            team: 0,
            ..Default::default()
        };
        let attacker = VehicleSimState {
            entity_id: 1,
            team: 1,
            ..Default::default()
        };
        let defender = VehicleSimState {
            entity_id: 2,
            team: 2,
            ..Default::default()
        };
        assert!(capture
            .step_capture(&[observer.clone(), attacker.clone(), defender], &[], 2.0)
            .is_empty());
        assert_eq!(
            capture.step_capture(&[observer, attacker], &[], 2.0),
            [BaseCaptureEvent::Points {
                team: 2,
                base_id: 0,
                points: 2
            },]
        );
        assert_eq!(capture.points_for(9), 0.0);
    }

    #[test]
    fn three_tanks_cap_at_max_points_per_second_and_damage_drops_attacker_contribution() {
        let mut capture = BaseCaptureTracker::new(&[TeamBaseSpec {
            team: 2,
            position: Vec3::ZERO,
            radius: 10.0,
            points_per_second: 1.0,
            max_points_per_second: 3.0,
            stop_invaders: false,
        }]);
        let tanks: Vec<_> = (1..=4)
            .map(|entity_id| VehicleSimState {
                entity_id,
                team: 1,
                is_alive: true,
                ..Default::default()
            })
            .collect();

        assert_eq!(
            capture.step_capture(&tanks[..1], &[], 1.0),
            vec![BaseCaptureEvent::Points {
                team: 2,
                base_id: 0,
                points: 1,
            }]
        );
        assert_eq!(
            capture.step_capture(&tanks, &[], 1.0),
            vec![BaseCaptureEvent::Points {
                team: 2,
                base_id: 0,
                points: 4,
            }]
        );
        assert_eq!(capture.points_for(1), 1.75);
        assert_eq!(capture.points_for(2), 0.75);
        let reset = capture.step_capture(&tanks, &[(9, 1)], 0.0);
        assert!(reset.contains(&BaseCaptureEvent::Reset {
            attacker_id: 9,
            points: 1.75,
        }));
        assert_eq!(capture.points_for(1), 0.0);
        assert_eq!(capture.points_for(2), 0.75);
        assert_eq!(capture.points_for(3), 0.75);
        assert_eq!(capture.points_for(4), 0.75);
        let completed = capture.step_capture(&tanks, &[], 40.0);
        assert!(completed.contains(&BaseCaptureEvent::Captured { team: 2 }));
        assert!(capture.step_capture(&tanks, &[], 1.0).is_empty());
    }
}
