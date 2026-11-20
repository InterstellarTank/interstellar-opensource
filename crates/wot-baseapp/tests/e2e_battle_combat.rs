//! Multi-client UDP E2E combat and bot tests for Task 5:
//! Two real UDP client sessions enter the same arena -> Client A tracks gun & shoots at Client B ->
//! verifies wire replication of shooting, tracers, damage, ammo/reload updates, AoI, and `addBotToArena`.

include!("support/battle_udp.rs");

use wot_baseapp::protocol::AREA_DESTRUCTIBLES_ENTITY_TYPE_ID;
use wot_battle::lobby::PREBATTLE_ROSTER_ASSIGNED_IN_TEAM2;
use wot_battle::world::ARENA_UPDATE_VEHICLE_ADDED;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn combat_simulation_udp_two_clients_and_add_bot() {
    let battle_cfg = BattleHostConfig {
        team_size: 2,
        tick_interval: Duration::from_millis(40),
        preloaded_maps: Some(build_synthetic_map_store(900)),
        ..BattleHostConfig::default()
    };
    let h = TestHarness::start("combat_e2e", battle_cfg).await;
    let login_addr = h.login_addr;
    let base_port = h.base_port;
    let pubkey = h.pubkey.clone();

    tokio::task::spawn_blocking(move || {
        // 1. Log in Client A (Host) and Client B (Guest)
        let (mut client_a, _) = login_to_hangar(
            login_addr,
            base_port,
            &pubkey,
            b"client_a@test.com",
            b"client_a_secret!",
        );
        let sync_a = sync_account_data(&mut client_a, 120);
        let veh_a = read_first_veh_inv_id(&sync_a);

        let (mut client_b, client_b_id) = login_to_hangar(
            login_addr,
            base_port,
            &pubkey,
            b"client_b@test.com",
            b"client_b_secret!",
        );
        let sync_b = sync_account_data(&mut client_b, 121);
        let veh_b = read_first_veh_inv_id(&sync_b);

        // 2. Client A creates training room
        let mut cw = BinaryWriter::new();
        cw.i32(3); // Himmelsdorf
        cw.i32(900);
        cw.u8(1); // isOpened
        cw.string("Combat E2E");
        let mut enc_create = ElementEncoder::new();
        enc_create.message(base_method::CREATE_TRAINING, LengthStyle::Variable(2), cw.as_slice());
        let msgs_a = client_a.send_reliable_and_collect_until(enc_create, Duration::from_secs(5), |ms| {
            ms.iter().any(|m| m.id == client_method::ON_PREBATTLE_JOINED)
        });
        let joined = msgs_a.iter().find(|m| m.id == client_method::ON_PREBATTLE_JOINED).unwrap();
        let mut jr = BinaryReader::new(&joined.data);
        let prb_id = jr.i32().unwrap();

        // 3. Client B joins training room and is assigned to Team 2
        let mut jw = BinaryWriter::new();
        jw.i16(130);
        jw.i16(cmd::CMD_PRB_JOIN);
        jw.i32(prb_id);
        jw.i32(0);
        jw.i32(0);
        let mut enc_join = ElementEncoder::new();
        enc_join.message(base_method::DO_CMD_INT3, LengthStyle::Variable(2), jw.as_slice());
        let _ = client_b.send_reliable_and_collect_until(enc_join, Duration::from_secs(5), |ms| {
            ms.iter().any(|m| m.id == client_method::ON_PREBATTLE_JOINED)
        });

        // Client A assigns Client B to Team 2
        let mut aw = BinaryWriter::new();
        aw.i16(131);
        aw.i16(cmd::CMD_PRB_ASSIGN);
        aw.i32(client_b_id);
        aw.i32(PREBATTLE_ROSTER_ASSIGNED_IN_TEAM2 as i32);
        aw.i32(0);
        let mut enc_assign = ElementEncoder::new();
        enc_assign.message(base_method::DO_CMD_INT3, LengthStyle::Variable(2), aw.as_slice());
        let _ = client_a.send_reliable_and_collect(enc_assign, Duration::from_millis(500));

        // 4. Both clients set ready
        let mut r_a = BinaryWriter::new();
        r_a.i16(132);
        r_a.i16(cmd::CMD_PRB_READY);
        r_a.i32(veh_a as i32);
        r_a.i32(0);
        r_a.i32(0);
        let mut enc_ra = ElementEncoder::new();
        enc_ra.message(base_method::DO_CMD_INT3, LengthStyle::Variable(2), r_a.as_slice());
        let _ = client_a.send_reliable_and_collect(enc_ra, Duration::from_millis(500));

        let mut r_b = BinaryWriter::new();
        r_b.i16(133);
        r_b.i16(cmd::CMD_PRB_READY);
        r_b.i32(veh_b as i32);
        r_b.i32(0);
        r_b.i32(0);
        let mut enc_rb = ElementEncoder::new();
        enc_rb.message(base_method::DO_CMD_INT3, LengthStyle::Variable(2), r_b.as_slice());
        let _ = client_b.send_reliable_and_collect(enc_rb, Duration::from_millis(500));

        // Start battle (both teams ready)
        let mut t1w = BinaryWriter::new();
        t1w.i16(134);
        t1w.i16(cmd::CMD_PRB_TEAM_READY);
        t1w.i32(1);
        t1w.i32(1);
        t1w.i32(0);
        let mut t2w = BinaryWriter::new();
        t2w.i16(135);
        t2w.i16(cmd::CMD_PRB_TEAM_READY);
        t2w.i32(2);
        t2w.i32(1);
        t2w.i32(0);
        let mut launch_enc = ElementEncoder::new();
        launch_enc.message(base_method::DO_CMD_INT3, LengthStyle::Variable(2), t1w.as_slice());
        launch_enc.message(base_method::DO_CMD_INT3, LengthStyle::Variable(2), t2w.as_slice());
        let msgs_launch_a = client_a.send_reliable_and_collect_until(launch_enc, Duration::from_secs(5), |ms| {
            ms.iter().any(|m| m.id == client_ids::RESET_ENTITIES)
        });

        assert!(msgs_launch_a.iter().any(|m| m.id == client_ids::RESET_ENTITIES));

        let msgs_launch_b = client_b.collect_until(Duration::from_secs(5), |ms| {
            ms.iter().any(|m| m.id == client_ids::RESET_ENTITIES)
        });

        assert!(msgs_launch_b.iter().any(|m| m.id == client_ids::RESET_ENTITIES));

        // 5. Both enter arena via ENABLE_ENTITIES
        let mut en_a = ElementEncoder::new();
        en_a.fixed_message(baseapp_ext_ids::ENABLE_ENTITIES, 1, &[0]);
        let arena_msgs_a = client_a.send_reliable_and_collect_until(en_a, Duration::from_secs(5), |ms| {
            ms.iter().any(|m| m.id == client_ids::CREATE_CELL_PLAYER)
        });
        let cp_a = arena_msgs_a.iter().find(|m| m.id == client_ids::CREATE_CELL_PLAYER).unwrap();
        let mut cr_a = BinaryReader::new(&cp_a.data);
        let _ = cr_a.i32().unwrap(); // space
        let _ = cr_a.i32().unwrap(); // pass
        let _ = [cr_a.f32().unwrap(), cr_a.f32().unwrap(), cr_a.f32().unwrap()]; // pos
        let _ = [cr_a.f32().unwrap(), cr_a.f32().unwrap(), cr_a.f32().unwrap()]; // dir
        let _ = cr_a.u8().unwrap(); // team
        let veh_a_id = cr_a.i32().unwrap();

        let destr_chunk_id = wot_battle::sim::destructibles::encode_chunk_id(0, 0);
        let destr_entity_id = wot_battle::sim::destructibles::chunk_entity_id(destr_chunk_id);
        let a_enter_aoi_ids: Vec<i32> = arena_msgs_a
            .iter()
            .filter(|m| m.id == client_ids::ENTER_AOI)
            .map(|m| BinaryReader::new(&m.data).i32().unwrap())
            .collect();
        assert!(
            a_enter_aoi_ids.contains(&veh_a_id),
            "Client A must receive ENTER_AOI for own vehicle {veh_a_id}, got {a_enter_aoi_ids:?}"
        );
        assert!(
            a_enter_aoi_ids.contains(&destr_entity_id),
            "Client A must receive ENTER_AOI for AreaDestructibles chunk entity {destr_entity_id}, got {a_enter_aoi_ids:?}"
        );

        // Request entity creation for AreaDestructibles chunk entity
        let mut req_destr_enc = ElementEncoder::new();
        req_destr_enc.message(
            baseapp_ext_ids::REQUEST_ENTITY_UPDATE,
            LengthStyle::Variable(2),
            &destr_entity_id.to_le_bytes(),
        );
        let destr_create_msgs =
            client_a.send_reliable_and_collect_until(req_destr_enc, Duration::from_secs(3), |ms| {
                ms.iter().any(|m| {
                    if m.id == client_ids::CREATE_ENTITY {
                        let mut r = BinaryReader::new(&m.data);
                        let _comp = r.u8().unwrap();
                        r.i32().unwrap() == destr_entity_id
                    } else {
                        false
                    }
                })
            });
        let destr_ce = destr_create_msgs
            .iter()
            .find(|m| {
                if m.id == client_ids::CREATE_ENTITY {
                    let mut r = BinaryReader::new(&m.data);
                    let _ = r.u8().unwrap();
                    r.i32().unwrap() == destr_entity_id
                } else {
                    false
                }
            })
            .expect("CREATE_ENTITY for AreaDestructibles");
        {
            let mut r = BinaryReader::new(&destr_ce.data);
            let _comp = r.u8().unwrap();
            assert_eq!(r.i32().unwrap(), destr_entity_id);
            assert_eq!(r.u16().unwrap(), AREA_DESTRUCTIBLES_ENTITY_TYPE_ID);
        }

        let mut en_b = ElementEncoder::new();
        en_b.fixed_message(baseapp_ext_ids::ENABLE_ENTITIES, 1, &[0]);
        let arena_msgs_b = client_b.send_reliable_and_collect_until(en_b, Duration::from_secs(5), |ms| {
            ms.iter().any(|m| m.id == client_ids::CREATE_CELL_PLAYER)
        });
        let cp_b = arena_msgs_b.iter().find(|m| m.id == client_ids::CREATE_CELL_PLAYER).unwrap();
        let mut cr_b = BinaryReader::new(&cp_b.data);
        let _ = cr_b.i32().unwrap();
        let _ = cr_b.i32().unwrap();
        let target = [cr_b.f32().unwrap(), cr_b.f32().unwrap(), cr_b.f32().unwrap()];
        let _ = [cr_b.f32().unwrap(), cr_b.f32().unwrap(), cr_b.f32().unwrap()];
        let _ = cr_b.u8().unwrap();
        let veh_b_id = cr_b.i32().unwrap();

        for client in [&mut client_a, &mut client_b] {
            let mut ready = ElementEncoder::new();
            ready.message(base_method::AVATAR_ON_CLIENT_READY, LengthStyle::Variable(2), &[]);
            client.send_reliable_only(ready);
        }
        let started = |messages: &[DecodedMessage]| messages.iter().any(|m| {
            if m.id != client_method::AVATAR_UPDATE_ARENA {
                return false;
            }
            let mut reader = BinaryReader::new(&m.data);
            let (_, Ok(3), Ok(payload)) = (reader.i32(), reader.u8(), reader.string_bytes()) else {
                return false;
            };
            pickle::loads(&payload).ok().is_some_and(|period| {
                period.as_list().and_then(|values| values.first()).and_then(Value::as_int) == Some(3)
            })
        });
        assert!(started(&client_a.collect_until(Duration::from_secs(3), started)));
        let mut forbidden = BinaryWriter::new();
        forbidden.string("destroy_self");
        forbidden.i32(1);
        let mut developer_rpc = ElementEncoder::new();
        developer_rpc.message(base_method::AVATAR_SET_DEVELOPMENT_FEATURE, LengthStyle::Variable(2), forbidden.as_slice());
        client_a.send_reliable_only(developer_rpc);

        // Verify vehicle_changeSetting -> updateVehicleSetting (0x86)
        let mut setting_w = BinaryWriter::new();
        setting_w.u8(0); // CURRENT_SHELLS
        setting_w.i32(0); // slot 0
        let mut setting_enc = ElementEncoder::new();
        setting_enc.message(
            base_method::AVATAR_VEHICLE_CHANGE_SETTING,
            LengthStyle::Variable(2),
            setting_w.as_slice(),
        );
        let setting_msgs =
            client_a.send_reliable_and_collect_until(setting_enc, Duration::from_secs(3), |ms| {
                ms.iter()
                    .any(|m| m.id == client_method::AVATAR_UPDATE_VEHICLE_SETTING)
            });
        assert!(
            setting_msgs
                .iter()
                .any(|m| m.id == client_method::AVATAR_UPDATE_VEHICLE_SETTING),
            "Client A must receive AVATAR_UPDATE_VEHICLE_SETTING (0x86)"
        );

        // 6. Combat interaction:
        // Aim above the target's ground contact so dispersion cannot make this a ground shot.
        let mut track_w = BinaryWriter::new();
        track_w.f32(target[0]);
        track_w.f32(target[1] + 1.0);
        track_w.f32(target[2]);
        let mut shoot_enc = ElementEncoder::new();
        shoot_enc.message(
            base_method::AVATAR_VEHICLE_TRACK_POINT_WITH_GUN,
            LengthStyle::Variable(2),
            track_w.as_slice(),
        );
        shoot_enc.message(
            base_method::AVATAR_VEHICLE_SHOOT,
            LengthStyle::Variable(2),
            &[],
        );

        let a_combat_msgs =
            client_a.send_reliable_and_collect_until(shoot_enc, Duration::from_secs(3), |ms| {
                ms.iter().any(|m| m.id == client_ids::TICK_SYNC)
                    && ms
                        .iter()
                        .any(|m| m.id == client_method::AVATAR_UPDATE_TARGETING_INFO)
                    && ms
                        .iter()
                        .any(|m| m.id == client_method::AVATAR_UPDATE_GUN_MARKER)
                    && ms
                        .iter()
                        .any(|m| m.id == client_method::AVATAR_UPDATE_VEHICLE_GUN_RELOAD_TIME)
                    && ms
                        .iter()
                        .any(|m| m.id == client_method::AVATAR_UPDATE_VEHICLE_AMMO)
                    && ms
                        .iter()
                        .any(|m| m.id == client_method::AVATAR_UPDATE_POSITIONS)
            });

        assert!(
            a_combat_msgs.iter().any(|m| m.id == client_ids::TICK_SYNC),
            "Client A must receive TICK_SYNC"
        );
        assert!(
            a_combat_msgs
                .iter()
                .any(|m| m.id == client_method::AVATAR_UPDATE_TARGETING_INFO),
            "Client A must receive AVATAR_UPDATE_TARGETING_INFO (0x87)"
        );
        assert!(
            a_combat_msgs
                .iter()
                .any(|m| m.id == client_method::AVATAR_UPDATE_GUN_MARKER),
            "Client A must receive AVATAR_UPDATE_GUN_MARKER (0x88)"
        );
        assert!(
            a_combat_msgs
                .iter()
                .any(|m| m.id == client_method::AVATAR_UPDATE_VEHICLE_GUN_RELOAD_TIME),
            "Client A must receive AVATAR_UPDATE_VEHICLE_GUN_RELOAD_TIME (0x84)"
        );
        assert!(
            a_combat_msgs
                .iter()
                .any(|m| m.id == client_method::AVATAR_UPDATE_VEHICLE_AMMO),
            "Client A must receive AVATAR_UPDATE_VEHICLE_AMMO (0x85)"
        );
        let pos_msg = a_combat_msgs
            .iter()
            .find(|m| m.id == client_method::AVATAR_UPDATE_POSITIONS)
            .expect("Client A must receive AVATAR_UPDATE_POSITIONS (0x93)");
        {
            let mut r = BinaryReader::new(&pos_msg.data);
            let _avatar_id = r.i32().unwrap();
            let idx_len = r.i32().unwrap();
            assert!(idx_len >= 1, "indices array must be non-empty");
            for _ in 0..idx_len {
                let _ = r.u8().unwrap();
            }
            let pos_len = r.i32().unwrap();
            assert_eq!(pos_len, idx_len * 2, "positions must have 2 i16 coords per index");
            for _ in 0..pos_len {
                let _ = r.i16().unwrap();
            }
            assert!(r.rest().is_empty(), "updatePositions must have no trailing bytes");
        }

        // Client B must receive all combat events individually:
        // showShooting (0x80), showTracer (0x8C), stopTracer (0x8D), showDamageFromShot (0x81), and UPDATE_ENTITY (0x09)
        let b_combat_msgs = client_b.collect_until(Duration::from_secs(3), |ms| {
            ms.iter().any(|m| {
                m.id == client_method::VEHICLE_SHOW_SHOOTING
                    && m.data.len() == 4
                    && BinaryReader::new(&m.data).i32().unwrap_or(0) == veh_a_id
            }) && ms.iter().any(|m| m.id == client_method::AVATAR_SHOW_TRACER)
                && ms.iter().any(|m| m.id == client_method::AVATAR_STOP_TRACER)
                && ms.iter().any(|m| {
                    m.id == client_method::VEHICLE_SHOW_DAMAGE_FROM_SHOT
                        && BinaryReader::new(&m.data).i32().unwrap_or(0) == veh_b_id
                })
                && ms.iter().any(|m| {
                    m.id == client_ids::UPDATE_ENTITY
                        && BinaryReader::new(&m.data).i32().unwrap_or(0) == veh_b_id
                })
        });

        assert!(
            b_combat_msgs.iter().any(|m| {
                m.id == client_method::VEHICLE_SHOW_SHOOTING
                    && m.data.len() == 4
                    && BinaryReader::new(&m.data).i32().unwrap_or(0) == veh_a_id
            }),
            "Client B must receive 0-arg Vehicle.showShooting (0x80, 4 bytes) for attacker {veh_a_id}"
        );
        assert!(
            b_combat_msgs
                .iter()
                .any(|m| m.id == client_method::AVATAR_SHOW_TRACER),
            "Client B must receive Avatar.showTracer (0x8C)"
        );
        assert!(
            b_combat_msgs
                .iter()
                .any(|m| m.id == client_method::AVATAR_STOP_TRACER),
            "Client B must receive Avatar.stopTracer (0x8D)"
        );
        assert!(
            b_combat_msgs.iter().any(|m| {
                m.id == client_method::VEHICLE_SHOW_DAMAGE_FROM_SHOT
                    && BinaryReader::new(&m.data).i32().unwrap_or(0) == veh_b_id
            }),
            "Client B must receive Vehicle.showDamageFromShot (0x81) for struck vehicle {veh_b_id}"
        );
        assert!(
            b_combat_msgs.iter().any(|m| {
                m.id == client_ids::UPDATE_ENTITY
                    && BinaryReader::new(&m.data).i32().unwrap_or(0) == veh_b_id
            }),
            "Client B must receive UPDATE_ENTITY (0x09) for struck vehicle {veh_b_id}"
        );

        // 7. Test addBotToArena: Client A invokes Avatar.base.addBotToArena(compDescr, team=2, name="TestBot")
        let mut bot_w = BinaryWriter::new();
        let bot_comp_descr = sync_a.get("inventory").unwrap().get_int(1).unwrap()
            .get("compDescr").unwrap().get_int(veh_a).unwrap().as_bytes().unwrap();
        bot_w.string_bytes(bot_comp_descr);
        bot_w.u8(2); // Team 2
        bot_w.string("TestBot");
        let mut add_bot_enc = ElementEncoder::new();
        add_bot_enc.message(
            base_method::AVATAR_ADD_BOT_TO_ARENA,
            LengthStyle::Variable(2),
            bot_w.as_slice(),
        );

        let bot_added_msgs =
            client_a.send_reliable_and_collect_until(add_bot_enc, Duration::from_secs(3), |ms| {
                ms.iter().any(|m| {
                    if m.id == client_method::AVATAR_UPDATE_ARENA {
                        let mut r = BinaryReader::new(&m.data);
                        let _ = r.i32();
                        r.u8().ok() == Some(ARENA_UPDATE_VEHICLE_ADDED)
                    } else {
                        false
                    }
                })
            });

        assert!(
            bot_added_msgs.iter().any(|m| {
                if m.id == client_method::AVATAR_UPDATE_ARENA {
                    let mut r = BinaryReader::new(&m.data);
                    let _ = r.i32().unwrap();
                    let utype = r.u8().unwrap();
                    utype == ARENA_UPDATE_VEHICLE_ADDED
                } else {
                    false
                }
            }),
            "Client A must receive ARENA_UPDATE_VEHICLE_ADDED upon addBotToArena"
        );
    })
    .await
    .unwrap();
}
