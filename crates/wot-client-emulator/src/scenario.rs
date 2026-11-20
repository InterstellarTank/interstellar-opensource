use std::fs::File;
use std::io::BufReader;
use std::path::Path;
use std::time::Duration;

use anyhow::{ensure, Context, Result};
use serde::{Deserialize, Deserializer};
use serde_json::Value as Json;
use wot065_data::pickle::Value;
use wot_client_emulator::{Client, CommandArgs, Event, Phase};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Scenario {
    steps: Vec<Step>,
}

#[derive(Deserialize)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
enum Step {
    WaitHangar,
    Command {
        id: i16,
        #[serde(default)]
        args: CommandArgs,
        expect: i16,
    },
    Notify {
        id: i16,
        #[serde(default)]
        args: CommandArgs,
    },
    Base {
        method: u8,
        #[serde(default, deserialize_with = "hex_bytes")]
        payload_hex: Vec<u8>,
    },
    Cell {
        entity: EntityTarget,
        method: u8,
        #[serde(default, deserialize_with = "hex_bytes")]
        payload_hex: Vec<u8>,
    },
    WaitEvent(EventFilter),
    WaitArenaPeriod {
        period: u8,
    },
    Move {
        flags: u8,
    },
    Aim {
        point: [f32; 3],
    },
    Shoot,
    Leave,
    Poll {
        ms: u64,
    },
    Assert {
        source: ValueSource,
        path: Vec<PathPart>,
        equals: Json,
    },
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct EventFilter {
    id: u8,
    phase: Option<Phase>,
    player_method: Option<bool>,
    #[serde(default)]
    offset: usize,
    #[serde(default, deserialize_with = "hex_bytes")]
    prefix_hex: Vec<u8>,
}

impl EventFilter {
    fn matches(&self, event: &Event) -> bool {
        event.id == self.id
            && self.phase.is_none_or(|phase| event.phase == phase)
            && self
                .player_method
                .is_none_or(|player_method| event.player_method == player_method)
            && event
                .payload_prefix
                .get(self.offset..)
                .is_some_and(|payload| payload.starts_with(&self.prefix_hex))
    }
}

#[derive(Deserialize)]
#[serde(untagged)]
enum EntityTarget {
    Id(i32),
    Relative(RelativeEntity),
}

#[derive(Deserialize)]
#[serde(rename_all = "snake_case")]
enum RelativeEntity {
    Player,
    OwnVehicle,
}

impl EntityTarget {
    fn resolve(&self, client: &Client) -> Result<i32> {
        match self {
            Self::Id(id) => Ok(*id),
            Self::Relative(RelativeEntity::Player) => {
                client.player_id().context("no player entity")
            }
            Self::Relative(RelativeEntity::OwnVehicle) => client
                .arena()
                .and_then(|arena| arena.vehicle_id)
                .context("no own vehicle entity"),
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize)]
#[serde(rename_all = "snake_case")]
enum ValueSource {
    Account,
    Shop,
    Dossiers,
    ShowGui,
    Reply,
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum PathPart {
    Key(String),
    Number(i64),
}

impl PathPart {
    fn select<'a>(&self, value: &'a Value) -> Option<&'a Value> {
        match self {
            Self::Key(key) => value.get(key),
            Self::Number(number) => match value.as_list() {
                Some(items) => usize::try_from(*number)
                    .ok()
                    .and_then(|index| items.get(index)),
                None => value.get_int(*number),
            },
        }
    }
}

impl Scenario {
    pub(crate) fn read(path: &Path) -> Result<Self> {
        let file = File::open(path).with_context(|| format!("open scenario {}", path.display()))?;
        let scenario: Self = serde_json::from_reader(BufReader::new(file))
            .with_context(|| format!("parse scenario {}", path.display()))?;
        ensure!(
            !scenario.steps.is_empty(),
            "scenario {} has no steps",
            path.display()
        );
        Ok(scenario)
    }

    pub(crate) fn run(&self, client: &mut Client, timeout: Duration) -> Result<usize> {
        let mut state = RunState {
            cursor: client.event_cursor(),
            reply: None,
        };
        for (index, step) in self.steps.iter().enumerate() {
            state
                .step(step, client, timeout)
                .with_context(|| format!("scenario step {}", index + 1))?;
        }
        Ok(self.steps.len())
    }
}

struct RunState {
    cursor: u64,
    reply: Option<Value>,
}

impl RunState {
    fn step(&mut self, step: &Step, client: &mut Client, timeout: Duration) -> Result<()> {
        match step {
            Step::WaitHangar => client.wait_for(timeout, "hangar sync and showGUI", |client| {
                client.hangar_ready().then_some(())
            }),
            Step::Command { id, args, expect } => {
                let reply =
                    self.send(client, |client| client.command(*id, args.clone(), timeout))?;
                ensure!(
                    reply.code == *expect,
                    "Account command {id} returned {}, expected {expect}",
                    reply.code
                );
                self.reply = reply.payload;
                Ok(())
            }
            Step::Notify { id, args } => {
                self.send(client, |client| client.notify(*id, args.clone()))
            }
            Step::Base {
                method,
                payload_hex,
            } => self.send(client, |client| client.call_base(*method, payload_hex)),
            Step::Cell {
                entity,
                method,
                payload_hex,
            } => {
                let id = entity.resolve(client)?;
                self.send(client, |client| client.call_cell(id, *method, payload_hex))
            }
            Step::WaitEvent(filter) => {
                let description = format!("event {:#04x}", filter.id);
                self.cursor = client.wait_for(timeout, &description, |client| {
                    client
                        .events_since(self.cursor)
                        .find(|event| filter.matches(event))
                        .map(|event| event.sequence)
                })?;
                Ok(())
            }
            Step::WaitArenaPeriod { period } => {
                client.wait_for(timeout, &format!("arena period {period}"), |client| {
                    (client.arena()?.period == Some(*period)).then_some(())
                })
            }
            Step::Move { flags } => self.send(client, |client| client.move_vehicle(*flags)),
            Step::Aim { point } => self.send(client, |client| client.aim(*point)),
            Step::Shoot => self.send(client, Client::shoot),
            Step::Leave => self.send(client, Client::leave_arena),
            Step::Poll { ms } => {
                let duration = Duration::from_millis(*ms);
                ensure!(duration <= timeout, "poll duration exceeds --timeout");
                client.poll_for(duration)
            }
            Step::Assert {
                source,
                path,
                equals,
            } => self.assert_value(client, *source, path, equals),
        }
    }

    fn send<T>(
        &mut self,
        client: &mut Client,
        send: impl FnOnce(&mut Client) -> Result<T>,
    ) -> Result<T> {
        // A synchronous command can receive an event before its reply arrives.
        self.cursor = client.event_cursor();
        send(client)
    }

    fn assert_value(
        &self,
        client: &Client,
        source: ValueSource,
        path: &[PathPart],
        expected: &Json,
    ) -> Result<()> {
        let root = match source {
            ValueSource::Account => client.account(),
            ValueSource::Shop => client.shop(),
            ValueSource::Dossiers => client.dossiers(),
            ValueSource::ShowGui => client.show_gui(),
            ValueSource::Reply => self.reply.as_ref(),
        }
        .with_context(|| format!("no {source:?} value available"))?;
        let actual = path
            .iter()
            .try_fold(root, |value, part| part.select(value))
            .with_context(|| format!("missing {source:?} path {path:?}"))?;
        ensure!(
            matches_value(actual, expected),
            "{source:?} value at {path:?} did not equal {expected}"
        );
        Ok(())
    }
}

fn hex_bytes<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Vec<u8>, D::Error> {
    hex::decode(String::deserialize(deserializer)?).map_err(serde::de::Error::custom)
}

fn matches_value(actual: &Value, expected: &Json) -> bool {
    match (actual, expected) {
        (Value::None, Json::Null) => true,
        (Value::Bool(actual), Json::Bool(expected)) => actual == expected,
        (Value::Int(actual), Json::Number(expected)) => expected.as_i64() == Some(*actual),
        (Value::Float(actual), Json::Number(expected)) => expected.as_f64() == Some(*actual),
        (Value::Str(actual), Json::String(expected)) => actual == expected.as_bytes(),
        (Value::List(actual) | Value::Tuple(actual), Json::Array(expected)) => {
            actual.len() == expected.len()
                && actual
                    .iter()
                    .zip(expected)
                    .all(|(value, expected)| matches_value(value, expected))
        }
        (Value::Set(actual), Json::Array(expected)) => {
            actual.len() == expected.len()
                && actual.iter().all(|value| {
                    expected
                        .iter()
                        .any(|expected| matches_value(value, expected))
                })
        }
        (Value::Dict(actual), Json::Object(expected)) => {
            actual.len() == expected.len()
                && actual.iter().all(|(key, value)| {
                    let expected = key
                        .as_bytes()
                        .and_then(|key| std::str::from_utf8(key).ok())
                        .and_then(|key| expected.get(key));
                    expected.is_some_and(|expected| matches_value(value, expected))
                })
        }
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn event_filter_matches_the_argument_after_the_entity_id() {
        let step: Step = serde_json::from_value(json!({
            "action": "wait_event", "id": 146, "phase": "arena", "player_method": true,
            "offset": 4, "prefix_hex": "03",
        }))
        .unwrap();
        let Step::WaitEvent(filter) = step else {
            panic!("wrong step type")
        };
        let mut event = Event {
            sequence: 1,
            id: 146,
            phase: Phase::Arena,
            player_method: true,
            payload_prefix: vec![18, 0, 0, 0, 3, 5],
        };
        assert!(filter.matches(&event));
        event.phase = Phase::Hangar;
        assert!(!filter.matches(&event));
        event.phase = Phase::Arena;
        event.payload_prefix.truncate(4);
        assert!(!filter.matches(&event));
    }

    #[test]
    fn assertion_path_distinguishes_integer_dict_keys_from_list_indices() {
        let snapshot = Value::sd(vec![(
            "inventory",
            Value::dict(vec![(
                Value::Int(1),
                Value::tuple(vec![Value::str("tank"), Value::Int(12)]),
            )]),
        )]);
        let path: Vec<PathPart> = serde_json::from_value(json!(["inventory", 1, 0])).unwrap();
        let selected = path
            .iter()
            .try_fold(&snapshot, |value, part| part.select(value))
            .unwrap();
        assert!(matches_value(selected, &json!("tank")));
        assert!(!matches_value(selected, &json!(12)));
        assert!(PathPart::Key("1".into())
            .select(snapshot.get("inventory").unwrap())
            .is_none());
    }

    #[test]
    fn malformed_hex_fails_before_a_server_connection() {
        assert!(serde_json::from_value::<Scenario>(json!({"steps": [
            {"action": "base", "method": 192, "payload_hex": "xx"}
        ]}))
        .is_err());
    }
}
