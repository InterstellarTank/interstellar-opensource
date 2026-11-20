//! Full Random battle, early Hangar return, and persistent account settlement.

include!("support/battle_udp.rs");

use wot065_data::dossier::{AccountDossier, VehicleDossier};
use wot065_data::store::JsonAccountStore;
use wot_battle::world::{ARENA_UPDATE_PERIOD, ARENA_UPDATE_VEHICLE_KILLED};

fn call(method: u8, data: &[u8]) -> ElementEncoder {
    let mut encoder = ElementEncoder::new();
    encoder.message(method, LengthStyle::Variable(2), data);
    encoder
}

fn do_cmd(command: i16, args: [i32; 3]) -> ElementEncoder {
    let mut body = BinaryWriter::new();
    body.i16(cmd::REQUEST_ID_NO_RESPONSE);
    body.i16(command);
    for value in args {
        body.i32(value);
    }
    call(base_method::DO_CMD_INT3, body.as_slice())
}

fn development(name: &str, value: i32) -> ElementEncoder {
    let mut body = BinaryWriter::new();
    body.string(name);
    body.i32(value);
    call(base_method::AVATAR_SET_DEVELOPMENT_FEATURE, body.as_slice())
}

fn enable_entities(client: &mut SimClient, expected: u8) -> Vec<DecodedMessage> {
    let mut encoder = ElementEncoder::new();
    encoder.fixed_message(baseapp_ext_ids::ENABLE_ENTITIES, 1, &[0]);
    let messages = client
        .send_reliable_and_collect_until(encoder, Duration::from_secs(5), |m| has(m, expected));
    assert!(
        has(&messages, expected),
        "missing entity entry message {expected:#x}"
    );
    messages
}

fn has(messages: &[DecodedMessage], id: u8) -> bool {
    messages.iter().any(|message| message.id == id)
}

fn period(messages: &[DecodedMessage], expected: i64) -> bool {
    messages.iter().any(|message| {
        if message.id != client_method::AVATAR_UPDATE_ARENA {
            return false;
        }
        let mut reader = BinaryReader::new(&message.data);
        let (_, Ok(ARENA_UPDATE_PERIOD), Ok(bytes)) =
            (reader.i32(), reader.u8(), reader.string_bytes())
        else {
            return false;
        };
        pickle::loads(&bytes).ok().is_some_and(|value| {
            value
                .as_list()
                .and_then(|items| items.first())
                .and_then(Value::as_int)
                == Some(expected)
        })
    })
}

fn chat_data(messages: &[DecodedMessage], action: u8) -> Option<Value> {
    messages.iter().find_map(|message| {
        if message.id != client_method::ON_CHAT_ACTION {
            return None;
        }
        let mut reader = BinaryReader::new(&message.data);
        reader.i32().ok()?;
        reader.i64().ok()?;
        if reader.u8().ok()? != action {
            return None;
        }
        reader.u8().ok()?;
        reader.f64().ok()?;
        reader.f64().ok()?;
        reader.i32().ok()?;
        reader.i64().ok()?;
        reader.string().ok()?;
        reader.u8().ok()?;
        pickle::loads(&reader.string_bytes().ok()?).ok()
    })
}

fn battle_service_card(messages: &[DecodedMessage]) -> bool {
    chat_data(messages, 22).is_some_and(|v| v.get("type").and_then(Value::as_int) == Some(2))
}

fn disconnect(client: &mut SimClient) {
    let mut encoder = ElementEncoder::new();
    encoder.fixed_message(baseapp_ext_ids::DISCONNECT_CLIENT, 1, &[0]);
    client.send_reliable_only(encoder);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn random_battle_preserves_an_early_lock_and_settles_both_accounts() {
    let battle = BattleHostConfig {
        team_size: 2,
        queue_wait_timeout: Duration::from_millis(500),
        prebattle_countdown_secs: 1,
        dev_features: true,
        tick_interval: Duration::from_millis(40),
        preloaded_maps: Some(build_synthetic_map_store(9)),
        ..BattleHostConfig::default()
    };
    let harness = TestHarness::start("full_lifecycle", battle).await;
    let (login, port, key) = (
        harness.login_addr,
        harness.base_port,
        harness.pubkey.clone(),
    );
    let accounts = harness._tmp.0.join("accounts");
    tokio::task::spawn_blocking(move || {
        let (mut alpha, _) = login_to_hangar(login, port, &key, b"alpha", b"alpha-065");
        let (mut bravo, _) = login_to_hangar(login, port, &key, b"bravo", b"bravo-065");
        let alpha_vehicle = read_first_veh_inv_id(&sync_account_data(&mut alpha, 120));
        let bravo_vehicle = read_first_veh_inv_id(&sync_account_data(&mut bravo, 121));
        let initial_store = JsonAccountStore::open(&accounts).unwrap();
        let alpha_before = initial_store.load_required("alpha").unwrap();
        let bravo_before = initial_store.load_required("bravo").unwrap();
        for (client, vehicle) in [(&mut alpha, alpha_vehicle), (&mut bravo, bravo_vehicle)] {
            client.send_reliable_only(do_cmd(cmd::CMD_ENQUEUE_FOR_ARENA, [vehicle as i32, 3, 1]));
        }
        for client in [&mut alpha, &mut bravo] {
            let reset = client.collect_until(Duration::from_secs(5), |m| {
                has(m, client_ids::RESET_ENTITIES)
            });
            assert!(has(&reset, client_ids::RESET_ENTITIES));
            enable_entities(client, client_ids::CREATE_CELL_PLAYER);
            client.send_reliable_only(call(base_method::AVATAR_ON_CLIENT_READY, &[]));
        }
        let countdown = alpha.collect_until(Duration::from_secs(3), |m| period(m, 2));
        assert!(period(&countdown, 2), "configured Prebattle countdown");
        let started = alpha.collect_until(Duration::from_secs(3), |m| period(m, 3));
        assert!(period(&started, 3));

        alpha.send_reliable_only(development("stop_bot", 1));
        alpha.send_reliable_only(development("server_marker", 1));
        let mut request = BinaryWriter::new();
        request.i64(1);
        request.u8(39);
        let channel_messages = alpha.send_reliable_and_collect_until(
            call(base_method::CHAT_COMMAND, request.as_slice()),
            Duration::from_secs(2),
            |messages| chat_data(messages, 6).is_some(),
        );
        let channels = chat_data(&channel_messages, 6).expect("battle channel list");
        let team_channel = channels
            .as_list()
            .unwrap()
            .iter()
            .find(|channel| channel.get("flags").and_then(Value::as_int) == Some(3))
            .and_then(|channel| channel.get("id").and_then(Value::as_int))
            .unwrap();
        let mut ping = BinaryWriter::new();
        ping.i64(2);
        ping.u8(28);
        ping.i32(team_channel as i32);
        ping.i64(0);
        ping.i16(74);
        ping.string("");
        ping.string("");
        let team_ping = alpha.send_reliable_and_collect_until(
            call(base_method::CHAT_COMMAND, ping.as_slice()),
            Duration::from_secs(2),
            |messages| chat_data(messages, 20).is_some(),
        );
        assert_eq!(
            chat_data(&team_ping, 20).unwrap().as_list().unwrap()[2],
            Value::Int(74)
        );
        let shot = alpha.send_reliable_and_collect_until(
            call(base_method::AVATAR_VEHICLE_SHOOT, &[]),
            Duration::from_secs(2),
            |m| has(m, client_method::AVATAR_SHOW_TRACER),
        );
        assert!(has(&shot, client_method::AVATAR_SHOW_TRACER));
        let killed = |m: &[DecodedMessage]| {
            m.iter().any(|event| {
                event.id == client_method::AVATAR_UPDATE_ARENA
                    && event.data.get(4) == Some(&ARENA_UPDATE_VEHICLE_KILLED)
            })
        };
        let defeat = bravo.send_reliable_and_collect_until(
            development("destroy_self", 1),
            Duration::from_secs(2),
            killed,
        );
        assert!(
            killed(&defeat),
            "developer destroy_self should emit a vehicle death"
        );
        assert!(
            chat_data(&defeat, 20).is_none(),
            "opposing team must not receive the minimap ping"
        );
        let exited = bravo.send_reliable_and_collect_until(
            call(base_method::AVATAR_LEAVE_ARENA, &[]),
            Duration::from_secs(2),
            |m| has(m, client_ids::RESET_ENTITIES),
        );
        assert!(has(&exited, client_ids::RESET_ENTITIES));
        enable_entities(&mut bravo, client_method::SHOW_GUI);
        assert_eq!(
            initial_store.load_required("bravo").unwrap().vehicles[&bravo_vehicle].lock,
            16
        );

        let (settled, results) = std::thread::scope(|scope| {
            let results = scope.spawn(|| {
                alpha.collect_until(Duration::from_secs(12), |m| {
                    has(m, client_method::AVATAR_ON_VEHICLE_LEFT_ARENA)
                })
            });
            let settled = bravo.collect_until(Duration::from_secs(12), |m| {
                has(m, client_method::UPDATE)
                    && has(m, client_method::RESYNC_DOSSIERS)
                    && battle_service_card(m)
            });
            (settled, results.join().unwrap())
        });
        assert!(
            has(&settled, client_method::UPDATE) && has(&settled, client_method::RESYNC_DOSSIERS)
        );
        assert!(battle_service_card(&settled));
        assert!(has(&results, client_method::AVATAR_ON_VEHICLE_LEFT_ARENA));
        let restored = JsonAccountStore::open(accounts).unwrap();
        let alpha_after = restored.load_required("alpha").unwrap();
        let bravo_after = restored.load_required("bravo").unwrap();
        assert_eq!(bravo_after.vehicles[&bravo_vehicle].lock, 0);
        assert!(bravo_after.credits > bravo_before.credits);
        assert!(bravo_after.veh_type_xp.values().any(|value| *value > 0));
        assert_eq!(
            AccountDossier::from_bytes(&bravo_after.account_dossier)
                .unwrap()
                .battles_count,
            1
        );
        assert!(bravo_after
            .vehicle_dossiers
            .values()
            .any(|(_, bytes)| VehicleDossier::from_bytes(bytes).unwrap().battles_count == 1));
        assert_eq!(
            alpha_after.vehicles[&alpha_vehicle].shells[0].1,
            alpha_before.vehicles[&alpha_vehicle].shells[0].1 - 1
        );
        assert_eq!(alpha_after.applied_battles, bravo_after.applied_battles);
        assert_eq!(alpha_after.applied_battles.len(), 1);
        assert!(!alpha_after.pending_sys_messages.is_empty());
        assert!(bravo_after.pending_sys_messages.is_empty());
        disconnect(&mut alpha);
        let (_, _, relogin) =
            login_and_capture_hangar(login, port, &key, b"alpha", b"alpha-return");
        assert!(battle_service_card(&relogin));
        assert!(restored
            .load_required("alpha")
            .unwrap()
            .pending_sys_messages
            .is_empty());
    })
    .await
    .unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn random_battle_resumes_the_parked_vehicle_then_settles_while_offline() {
    let battle = BattleHostConfig {
        team_size: 1,
        queue_wait_timeout: Duration::from_millis(80),
        prebattle_countdown_secs: 0,
        tick_interval: Duration::from_millis(40),
        preloaded_maps: Some(build_synthetic_map_store(8)),
        ..BattleHostConfig::default()
    };
    let harness = TestHarness::start("offline_settlement", battle).await;
    let (login, port, key) = (
        harness.login_addr,
        harness.base_port,
        harness.pubkey.clone(),
    );
    let accounts = harness._tmp.0.join("accounts");
    tokio::task::spawn_blocking(move || {
        let (mut client, _) = login_to_hangar(login, port, &key, b"offline", b"offline-start");
        let inv_id = read_first_veh_inv_id(&sync_account_data(&mut client, 120));
        let reset = client.send_reliable_and_collect_until(
            do_cmd(cmd::CMD_ENQUEUE_FOR_ARENA, [inv_id as i32, 3, 1]),
            Duration::from_secs(5),
            |messages| has(messages, client_ids::RESET_ENTITIES),
        );
        assert!(has(&reset, client_ids::RESET_ENTITIES));
        let world = enable_entities(&mut client, client_ids::CREATE_CELL_PLAYER);
        let space = world
            .iter()
            .find(|m| m.id == client_ids::CREATE_CELL_PLAYER)
            .unwrap();
        let started = client.send_reliable_and_collect_until(
            call(base_method::AVATAR_ON_CLIENT_READY, &[]),
            Duration::from_secs(3),
            |messages| period(messages, 3),
        );
        assert!(period(&started, 3));
        disconnect(&mut client);
        let (mut resumed, reentry) =
            login_and_capture_entities(login, port, &key, b"offline", b"offline-resume");
        let resumed_cell = reentry
            .iter()
            .find(|m| m.id == client_ids::CREATE_CELL_PLAYER)
            .expect("active battle re-entry");
        assert_eq!(&space.data[..4], &resumed_cell.data[..4]);
        assert_eq!(
            &space.data[space.data.len() - 4..],
            &resumed_cell.data[resumed_cell.data.len() - 4..]
        );
        assert!(!has(&reentry, client_method::SHOW_GUI));
        disconnect(&mut resumed);

        let store = JsonAccountStore::open(accounts).unwrap();
        let deadline = Instant::now() + Duration::from_secs(12);
        loop {
            let account = store.load_required("offline").unwrap();
            if !account.applied_battles.is_empty() {
                assert_eq!(account.vehicles[&inv_id].lock, 0);
                assert!(!account.pending_sys_messages.is_empty());
                break;
            }
            assert!(
                Instant::now() < deadline,
                "offline Random battle did not settle"
            );
            std::thread::sleep(Duration::from_millis(80));
        }
        let (_, _, reentry) =
            login_and_capture_hangar(login, port, &key, b"offline", b"offline-finish");
        assert!(battle_service_card(&reentry));
        assert!(store
            .load_required("offline")
            .unwrap()
            .pending_sys_messages
            .is_empty());
    })
    .await
    .unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn developer_training_arena_can_be_joined_and_leaves_no_rewards_or_damage() {
    let battle = BattleHostConfig {
        dev_features: true,
        tick_interval: Duration::from_millis(40),
        preloaded_maps: Some(build_synthetic_map_store(7)),
        ..BattleHostConfig::default()
    };
    let harness = TestHarness::start("training_settlement", battle).await;
    let (login, port, key) = (
        harness.login_addr,
        harness.base_port,
        harness.pubkey.clone(),
    );
    let accounts = harness._tmp.0.join("accounts");
    tokio::task::spawn_blocking(move || {
        let (mut host, _) = login_to_hangar(login, port, &key, b"trainer", b"trainer065");
        let (mut guest, _) = login_to_hangar(login, port, &key, b"trainee", b"trainee065");
        let guest_vehicle = read_first_veh_inv_id(&sync_account_data(&mut guest, 121));
        let store = JsonAccountStore::open(accounts).unwrap();
        let before = store.load_required("trainee").unwrap();
        let enter = |client: &mut SimClient, command| {
            let messages =
                client.send_reliable_and_collect_until(command, Duration::from_secs(3), |m| {
                    has(m, client_ids::RESET_ENTITIES)
                });
            assert!(has(&messages, client_ids::RESET_ENTITIES));
            enable_entities(client, client_ids::CREATE_CELL_PLAYER)
        };
        let world = enter(&mut host, do_cmd(cmd::CMD_CREATE_ARENA, [12, 0, 3]));
        let space = world
            .iter()
            .find(|m| m.id == client_ids::CREATE_CELL_PLAYER)
            .unwrap();
        let space_id = BinaryReader::new(&space.data).i32().unwrap();
        enter(
            &mut guest,
            do_cmd(cmd::CMD_JOIN_ARENA, [space_id, 2, guest_vehicle as i32]),
        );
        for client in [&mut host, &mut guest] {
            client.send_reliable_only(call(base_method::AVATAR_ON_CLIENT_READY, &[]));
        }
        let started = guest.collect_until(Duration::from_secs(3), |m| period(m, 3));
        assert!(period(&started, 3));
        guest.send_reliable_only(development("start_fire", 1));
        let hull_health = |messages: &[DecodedMessage]| {
            messages
                .iter()
                .filter_map(|message| {
                    (message.id == client_method::AVATAR_UPDATE_VEHICLE_HEALTH)
                        .then(|| BinaryReader::new(&message.data[4..]).i16().unwrap())
                })
                .last()
        };
        let damaged = guest.collect_until(Duration::from_secs(2), |m| {
            hull_health(m).is_some_and(|hp| hp < 95)
        });
        let damaged_hp = hull_health(&damaged).expect("fire damages the hull");
        assert!(damaged_hp < 95);
        let healed = guest.send_reliable_and_collect_until(
            development("heal", 0),
            Duration::from_secs(2),
            |m| hull_health(m).is_some_and(|hp| hp > damaged_hp),
        );
        assert!(hull_health(&healed).is_some_and(|hp| hp > damaged_hp));
        let shot = guest.send_reliable_and_collect_until(
            call(base_method::AVATAR_VEHICLE_SHOOT, &[]),
            Duration::from_secs(2),
            |m| has(m, client_method::AVATAR_SHOW_TRACER),
        );
        assert!(has(&shot, client_method::AVATAR_SHOW_TRACER));
        let finished = guest.send_reliable_and_collect_until(
            development("destroy_self", 1),
            Duration::from_secs(5),
            |m| has(m, client_method::AVATAR_ON_VEHICLE_LEFT_ARENA),
        );
        assert!(has(&finished, client_method::AVATAR_ON_VEHICLE_LEFT_ARENA));
        let exited = guest.send_reliable_and_collect_until(
            call(base_method::AVATAR_LEAVE_ARENA, &[]),
            Duration::from_secs(2),
            |m| has(m, client_ids::RESET_ENTITIES),
        );
        assert!(has(&exited, client_ids::RESET_ENTITIES));
        enable_entities(&mut guest, client_method::SHOW_GUI);
        let after = store.load_required("trainee").unwrap();
        assert_eq!(after.credits, before.credits);
        assert_eq!(after.free_xp, before.free_xp);
        assert_eq!(
            after.vehicles[&guest_vehicle].shells,
            before.vehicles[&guest_vehicle].shells
        );
        assert_eq!(
            after.vehicles[&guest_vehicle].repair,
            before.vehicles[&guest_vehicle].repair
        );
        assert_eq!(after.vehicles[&guest_vehicle].lock, 0);
        assert_eq!(after.account_dossier, before.account_dossier);
        assert_eq!(after.vehicle_dossiers, before.vehicle_dossiers);
        assert!(after.applied_battles.is_empty());
        assert!(after.pending_sys_messages.is_empty());
    })
    .await
    .unwrap();
}
