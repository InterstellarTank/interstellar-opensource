use wot065_data::pickle::Value;

pub const CHAT_CMD_BROADCAST: u8 = 47;
pub const CHAT_CMD_ATTACK_ENEMY: u8 = 29;
pub const CHAT_ACTION_BROADCAST: u8 = 1;
pub const CHAT_ACTION_USER_COMMAND: u8 = 20;

#[derive(Debug)]
pub struct BattleChatMessage {
    pub action: u8,
    pub data: Value,
    team: Option<u8>,
}

impl BattleChatMessage {
    pub fn reaches(&self, team: u8) -> bool {
        self.team.is_none_or(|required| team == required)
    }
}

#[derive(Debug, Clone, Copy)]
pub struct BattleChatRouter {
    common: i32,
}

impl BattleChatRouter {
    pub fn new(space_id: i32) -> Self {
        Self {
            common: space_id.rem_euclid(i32::MAX / 4) * 4 + 1,
        }
    }

    pub fn common_channel(&self) -> i32 {
        self.common
    }

    pub fn team_channel(&self, team: u8) -> Option<i32> {
        matches!(team, 1 | 2).then(|| self.common + i32::from(team))
    }

    pub fn channels(&self, team: u8) -> Value {
        let info = |id: i32, name: &str, flags| {
            Value::sd(vec![
                ("id", Value::Int(i64::from(id))),
                ("channelName", Value::str(name)),
                ("isReadOnly", Value::Bool(false)),
                ("isSecured", Value::Bool(false)),
                ("flags", Value::Int(flags)),
            ])
        };
        let mut channels = vec![info(self.common, "battleCommon", 2)];
        if let Some(id) = self.team_channel(team) {
            channels.push(info(id, "battleTeam", 3));
        }
        Value::list(channels)
    }

    pub fn route(
        &self,
        team: u8,
        channel: i32,
        command: u8,
        target: i64,
        sector: i16,
        text: &[u8],
    ) -> Option<BattleChatMessage> {
        let for_team = self.team_channel(team)? == channel;
        if !for_team && channel != self.common {
            return None;
        }
        let (action, data) = match command {
            CHAT_CMD_BROADCAST if !text.is_empty() && text.len() <= 512 => {
                (CHAT_ACTION_BROADCAST, Value::str(text))
            }
            22..=29 if for_team => {
                let (target, sector) = match command {
                    28 if (0..100).contains(&sector) => (0, sector),
                    CHAT_CMD_ATTACK_ENEMY if (1..=i64::from(i32::MAX)).contains(&target) => {
                        (target, 0)
                    }
                    22..=27 => (0, 0),
                    _ => return None,
                };
                (
                    CHAT_ACTION_USER_COMMAND,
                    Value::tuple(vec![
                        Value::Int(i64::from(command)),
                        Value::Int(target),
                        Value::Int(i64::from(sector)),
                        Value::str(""),
                        Value::str(""),
                    ]),
                )
            }
            _ => return None,
        };
        Some(BattleChatMessage {
            action,
            data,
            team: for_team.then_some(team),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chat_team_channel_isolates_teams_while_common_reaches_all() {
        let chat = BattleChatRouter::new(7);
        let recipients = |message: &BattleChatMessage| {
            [1, 1, 2]
                .into_iter()
                .filter(|&team| message.reaches(team))
                .count()
        };
        let team = chat.team_channel(1).unwrap();
        let team_message = chat.route(1, team, 47, 0, 0, b"hold").unwrap();
        assert_eq!(recipients(&team_message), 2);
        assert_eq!(team_message.action, 1);
        let ping = chat.route(1, team, 28, 0, 74, b"").unwrap();
        assert_eq!(recipients(&ping), 2);
        assert_eq!(ping.action, 20);
        assert_eq!(ping.data.as_list().unwrap()[2], Value::Int(74));
        let common = chat
            .route(1, chat.common_channel(), 47, 0, 0, b"hello")
            .unwrap();
        assert_eq!(recipients(&common), 3);
        assert!(chat.route(2, team, 47, 0, 0, b"spoof").is_none());
        assert!(chat.route(1, team, 28, 0, 100, b"").is_none());
    }
}
