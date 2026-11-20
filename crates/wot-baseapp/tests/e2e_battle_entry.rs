//! Self-contained E2E UDP tests for Task 5:
//! 1. Random Battle entry (`Account` -> queue -> `Avatar` + `SpaceData` + `Vehicle` -> `leaveArena` -> `Account` + unlock).
//! 2. Training Room entry (`createTraining` -> Team-2 virtual bot -> `CMD_PRB_READY` / `CMD_PRB_TEAM_READY` -> `Avatar` -> `Account` + unlock).
//! 3. Disconnect / timeout cleanup during `SwitchingToArena` + clean tank unlock + subsequent re-login.
//! 4. Retail asset smoke test (skip-safe if retail install missing).

use std::collections::BTreeMap;
use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4, UdpSocket};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use bw201::bundle::{decode_messages, DecodedMessage, ElementEncoder};
use bw201::crypto::{BlowfishKey, LoginKey};
use bw201::interfaces::{
    baseapp_ext_ids, client_ids, client_interface, login_ids, login_interface, LengthStyle,
    REPLY_MESSAGE_IDENTIFIER,
};
use bw201::loginapp::{LoginApp, LoginAppConfig};
use bw201::packet::{
    Endian, Packet, FLAG_CREATE_CHANNEL, FLAG_IS_RELIABLE, FLAG_ON_CHANNEL,
};
use bw201::stream::{BinaryReader, BinaryWriter};
use bw201::world::{
    C2SAvatarUpdate, CreateCellPlayer, IdKind, PosKind, S2CAvatarUpdate, SpaceData, YawPitchRoll,
    SPACE_DATA_MAPPING_KEY_CLIENT_SERVER, SPACE_DATA_TOD_KEY, SPACE_DATA_WEATHER,
};
use glam::{Mat4, Vec3};
use rsa::traits::PublicKeyParts;
use rsa::{Oaep, RsaPublicKey};
use sha1::Sha1;
use wot065_data::account_state::Preset;
use wot065_data::pickle::{self, Value};
use wot_baseapp::channel::ChannelState;
use wot_baseapp::payloads::PayloadOverrides;
use wot_baseapp::protocol::{
    base_method, client_method, cmd, ACCOUNT_ENTITY_TYPE_ID, AREA_DESTRUCTIBLES_ENTITY_TYPE_ID,
    AVATAR_ENTITY_TYPE_ID, VEHICLE_ENTITY_TYPE_ID,
};
use wot_baseapp::{BaseAppConfig, BattleHostConfig};
use wot_battle::assets::arena_defs::ArenaDef;
use wot_battle::assets::chunk::{ChunkCoords, ChunkKind, SpaceBounds, SpawnPoint};
use wot_battle::assets::map_store::{ArenaMap, MapStore, TerrainMap};
use wot_battle::assets::terrain::{
    MaterialKinds, TerrainChunk, DOMINANT_TEX_GRID_SIZE, HEIGHTMAP_GRID_SIZE,
};
use wot_battle::balance::BalanceConfig;
use wot_battle::lobby::{
    ARENA_GUI_TYPE_RANDOM, ARENA_GUI_TYPE_TRAINING, PREBATTLE_ACCOUNT_STATE_READY,
    PREBATTLE_CACHE_KEY_STATE, PREBATTLE_ROSTER_ASSIGNED_IN_TEAM1, PREBATTLE_UPDATE_ROSTER,
    PREBATTLE_UPDATE_SETTINGS, PREBATTLE_UPDATE_TEAM_STATES,
};
use wot_battle::world::{
    ARENA_UPDATE_AVATAR_READY, ARENA_UPDATE_PERIOD, ARENA_UPDATE_VEHICLE_LIST,
};

struct TempDir(PathBuf);

impl TempDir {
    fn new(name: &str) -> Self {
        let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../target/test-tmp")
            .join(format!("battle_e2e_{name}_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        Self(dir)
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn write_synthetic_game_dir(root: &Path) {
    let vehicles_dir = root.join("res/scripts/item_defs/vehicles");
    for (nation, veh) in [
        ("ussr", "MS-1"),
        ("germany", "Ltraktor"),
        ("usa", "T1_Cunningham"),
    ] {
        let ndir = vehicles_dir.join(nation);
        let cdir = ndir.join("components");
        std::fs::create_dir_all(&cdir).unwrap();
        std::fs::write(ndir.join("customization.xml"), "<root/>").unwrap();
        std::fs::write(
            ndir.join("list.xml"),
            format!("<root><{veh}><id>0</id><userString>{veh}</userString><level>1</level><price>0</price><tags>lightTank</tags></{veh}></root>"),
        )
        .unwrap();
        std::fs::write(
            cdir.join("shells.xml"),
            "<root><AP><id>1</id><price>5</price><kind>ARMOR_PIERCING</kind><caliber>20</caliber><isTracer>true</isTracer><effects>smallAP</effects><damage><armor>12</armor><devices>20</devices></damage></AP></root>",
        )
        .unwrap();
        std::fs::write(
            cdir.join("chassis.xml"),
            "<root><ids><Ch>1</Ch></ids><shared><Ch><weight>1000</weight><level>1</level><price>100</price><maxLoad>10000</maxLoad></Ch></shared></root>",
        )
        .unwrap();
        std::fs::write(
            cdir.join("engines.xml"),
            "<root><ids><Eng>1</Eng></ids><shared><Eng><weight>500</weight><level>1</level><price>100</price><power>100</power></Eng></shared></root>",
        )
        .unwrap();
        std::fs::write(
            cdir.join("fuelTanks.xml"),
            "<root><ids><Fuel>1</Fuel></ids><shared><Fuel><weight>100</weight><level>1</level><price>50</price></Fuel></shared></root>",
        )
        .unwrap();
        std::fs::write(
            cdir.join("radios.xml"),
            "<root><ids><Rad>1</Rad></ids><shared><Rad><weight>50</weight><level>1</level><price>50</price><distance>300</distance></Rad></shared></root>",
        )
        .unwrap();
        std::fs::write(
            cdir.join("guns.xml"),
            "<root><ids><G>1</G></ids><shared><G><level>1</level><price>100</price><weight>50</weight><maxAmmo>50</maxAmmo><pitchLimits>-10 20</pitchLimits><rotationSpeed>30</rotationSpeed><reloadTime>2.0</reloadTime><aimingTime>1.5</aimingTime><shotDispersionRadius>0.4</shotDispersionRadius><effects>small</effects><shots><AP><defaultPortion>1.0</defaultPortion><speed>600</speed><gravity>9.81</gravity><maxDistance>400</maxDistance><piercingPower>30 20</piercingPower></AP></shots></G></shared></root>",
        )
        .unwrap();
        std::fs::write(
            cdir.join("turrets.xml"),
            "<root><ids><T>1</T></ids><shared><T><level>1</level><price>100</price><weight>200</weight><maxHealth>20</maxHealth><yawLimits>-180 180</yawLimits><guns><G>shared</G></guns></T></shared></root>",
        )
        .unwrap();
        std::fs::write(
            ndir.join(format!("{veh}.xml")),
            "<root><crew><commander/><driver/></crew><speedLimits><forward>32</forward><backward>10</backward></speedLimits><hull><weight>3000</weight><maxHealth>80</maxHealth><armor><armor_1>15</armor_1></armor><turretPositions><turret>0 0.5 0</turret></turretPositions></hull><chassis><Ch>shared</Ch></chassis><engines><Eng>shared</Eng></engines><fuelTanks><Fuel>shared</Fuel></fuelTanks><radios><Rad>shared</Rad></radios><turrets0><T>shared</T></turrets0></root>",
        )
        .unwrap();
    }
}

fn build_synthetic_map_store() -> Arc<MapStore> {
    let def = ArenaDef {
        id: 3,
        name: "04_himmelsdorf".into(),
        display_name_key: "Himmelsdorf".into(),
        description_key: String::new(),
        geometry: "spaces/04_himmelsdorf".into(),
        bounding_box: ([-100.0, -100.0], [100.0, 100.0]),
        min_players_in_team: 1,
        max_players_in_team: 15,
        round_length_secs: 900,
        arena_start_delay_secs: 5.0,
        kick_after_finish_wait_secs: 10.0,
        minimap_texture: None,
    };
    let bounds = SpaceBounds {
        min_x: -1,
        max_x: 0,
        min_y: -1,
        max_y: 0,
    };
    let mut chunks = Vec::new();
    for gz in -1..=0 {
        for gx in -1..=0 {
            chunks.push((
                ChunkCoords {
                    grid_x: gx,
                    grid_z: gz,
                    kind: ChunkKind::Outer,
                },
                TerrainChunk {
                    heights: vec![4.0; HEIGHTMAP_GRID_SIZE * HEIGHTMAP_GRID_SIZE],
                    min_y: 4.0,
                    max_y: 4.0,
                    dom_tex_indices: vec![0; DOMINANT_TEX_GRID_SIZE * DOMINANT_TEX_GRID_SIZE],
                    tex_names: vec![],
                },
            ));
        }
    }
    let terrain = TerrainMap::from_chunks(bounds, chunks).unwrap();
    let mats = Arc::new(MaterialKinds::parse(b"<root/>").unwrap());
    let spawns = vec![
        SpawnPoint {
            team: 1,
            preferable_type: None,
            world_transform: Mat4::IDENTITY,
            position: Vec3::new(-45.0, 4.0, -45.0),
            yaw: 0.4,
        },
        SpawnPoint {
            team: 2,
            preferable_type: None,
            world_transform: Mat4::IDENTITY,
            position: Vec3::new(45.0, 4.0, 45.0),
            yaw: -2.7,
        },
    ];
    let arena_map = Arc::new(ArenaMap::from_parts(
        def,
        bounds,
        terrain,
        mats,
        vec![],
        vec![],
        vec![],
        spawns,
        vec![],
        BTreeMap::new(),
        &BalanceConfig::default(),
    ));
    Arc::new(MapStore::from_preloaded(
        vec![arena_map],
        BalanceConfig::default(),
    ))
}

struct SimClient {
    sock: UdpSocket,
    key: BlowfishKey,
    channel: ChannelState,
    out_seq: u32,
}

impl SimClient {
    fn connect(addr: SocketAddr, key: BlowfishKey) -> Self {
        let sock = UdpSocket::bind("127.0.0.1:0").unwrap();
        sock.connect(addr).unwrap();
        sock.set_read_timeout(Some(Duration::from_secs(4))).unwrap();
        Self {
            sock,
            key,
            channel: ChannelState::new(),
            out_seq: 0,
        }
    }

    fn next_out_seq(&mut self) -> u32 {
        let s = self.out_seq;
        self.out_seq += 1;
        s
    }

    fn send_off_channel_plaintext_and_collect(&mut self, enc: ElementEncoder) -> Vec<DecodedMessage> {
        let mut pkt = enc.finish();
        pkt.finalize_flags();
        let wire = pkt.encode(Endian::Little);
        self.sock.send(&wire).unwrap();
        self.collect_replies(Duration::from_secs(3))
    }

    fn send_reliable_and_collect(&mut self, enc: ElementEncoder, wait: Duration) -> Vec<DecodedMessage> {
        self.send_reliable_only(enc);
        self.collect_replies(wait)
    }

    fn send_reliable_and_collect_until(
        &mut self,
        enc: ElementEncoder,
        wait: Duration,
        done: impl FnMut(&[DecodedMessage]) -> bool,
    ) -> Vec<DecodedMessage> {
        self.send_reliable_only(enc);
        self.collect_until(wait, done)
    }

    fn send_reliable_only(&mut self, enc: ElementEncoder) {
        let mut pkt = enc.finish();
        let seq = self.next_out_seq();
        pkt.config.sequence = Some(seq);
        if seq == 0 {
            pkt.config.extra_flags |= FLAG_CREATE_CHANNEL;
        }
        pkt.config.cumulative_ack = Some(self.channel.in_seq_at());
        pkt.config.extra_flags |= FLAG_ON_CHANNEL | FLAG_IS_RELIABLE;
        pkt.finalize_flags();
        let plain = pkt.encode(Endian::Little);
        let wire = self.key.packet_encrypt(&plain);
        self.sock.send(&wire).unwrap();
    }

    fn collect_replies(&mut self, timeout: Duration) -> Vec<DecodedMessage> {
        self.collect_inner(timeout, None::<fn(&[DecodedMessage]) -> bool>)
    }

    fn collect_until(
        &mut self,
        timeout: Duration,
        done: impl FnMut(&[DecodedMessage]) -> bool,
    ) -> Vec<DecodedMessage> {
        self.collect_inner(timeout, Some(done))
    }

    fn collect_inner(
        &mut self,
        timeout: Duration,
        mut done: Option<impl FnMut(&[DecodedMessage]) -> bool>,
    ) -> Vec<DecodedMessage> {
        let mut buf = vec![0u8; 4096];
        let table = client_interface();
        let mut collected = Vec::new();
        let deadline = Instant::now() + timeout;
        let mut settled_deadline: Option<Instant> = None;
        loop {
            let now = Instant::now();
            let stop_at = settled_deadline.map(|s| s.min(deadline)).unwrap_or(deadline);
            if now >= stop_at {
                break;
            }
            let remaining = if settled_deadline.is_some() {
                stop_at - now
            } else if done.is_some() {
                Duration::from_millis(100).min(stop_at - now)
            } else if collected.is_empty() {
                stop_at - now
            } else {
                Duration::from_millis(80).min(stop_at - now)
            };
            self.sock.set_read_timeout(Some(remaining)).unwrap();
            match self.sock.recv(&mut buf) {
                Ok(n) => {
                    let Ok(dec) = self.key.packet_decrypt(&buf[..n]) else {
                        continue;
                    };
                    let Ok(rpkt) = Packet::decode(&dec, Endian::Little) else {
                        continue;
                    };
                    let rx = self.channel.receive(rpkt);
                    for assembled in rx.packets {
                        if let Ok(msgs) = decode_messages(&assembled, &table, Endian::Little) {
                            collected.extend(msgs);
                        }
                    }
                    if let Some(pred) = done.as_mut() {
                        if settled_deadline.is_none() && pred(&collected) {
                            settled_deadline = Some(Instant::now() + Duration::from_millis(40));
                        }
                    }
                }
                Err(_) => {
                    if settled_deadline.is_some() || done.is_none() {
                        break;
                    }
                }
            }
        }
        if !collected.is_empty() {
            let ack = self.channel.send_ack_only(&self.key);
            let _ = self.sock.send(&ack);
        }
        collected
    }
}

fn encrypt_login_request(
    pubkey: &RsaPublicKey,
    username: &[u8],
    password: &[u8],
    blowfish_key: &[u8],
    reply_id: u32,
) -> Vec<u8> {
    let mut plain = BinaryWriter::new();
    plain.u8(1);
    plain.string_bytes(username);
    plain.string_bytes(password);
    plain.string_bytes(blowfish_key);
    plain.raw(&[0xAB; 16]);
    plain.u32(0x1122_3344);

    let mut rng = rand::thread_rng();
    let max_chunk = pubkey.size().saturating_sub(42);
    let mut cipher = Vec::new();
    for chunk in plain.as_slice().chunks(max_chunk) {
        let encrypted = pubkey
            .encrypt(&mut rng, Oaep::new::<Sha1>(), chunk)
            .unwrap();
        cipher.extend_from_slice(&encrypted);
    }

    let mut args = BinaryWriter::new();
    args.u32(57);
    args.u32(0x0000_0605);
    args.raw(&cipher);

    let mut enc = ElementEncoder::new();
    enc.request(
        login_ids::LOGIN,
        LengthStyle::Variable(2),
        reply_id,
        args.as_slice(),
    );
    let mut pkt = enc.finish();
    pkt.finalize_flags();
    pkt.encode(Endian::Little)
}

fn login_to_hangar(
    login_addr: SocketAddr,
    expected_base_port: u16,
    pubkey: &RsaPublicKey,
    username: &[u8],
    blowfish_bytes: &[u8],
) -> (SimClient, i32) {
    let lsock = UdpSocket::bind("127.0.0.1:0").unwrap();
    lsock.set_read_timeout(Some(Duration::from_secs(15))).unwrap();
    let req = encrypt_login_request(pubkey, username, b"pass", blowfish_bytes, 77);
    lsock.send_to(&req, login_addr).unwrap();
    let mut lbuf = [0u8; 2048];
    let (n, _) = lsock.recv_from(&mut lbuf).unwrap();
    let lpkt = Packet::decode(&lbuf[..n], Endian::Little).unwrap();
    let lmsgs = decode_messages(&lpkt, &login_interface(), Endian::Little).unwrap();
    let mut lr = BinaryReader::new(&lmsgs[0].data);
    assert_eq!(lr.u32().unwrap(), 77);
    assert_eq!(lr.u8().unwrap(), 1);
    let bf = BlowfishKey::new(blowfish_bytes).unwrap();
    let plain = bf.stream_decrypt(lr.rest()).unwrap();
    let ip = Ipv4Addr::new(plain[0], plain[1], plain[2], plain[3]);
    let port = u16::from_be_bytes([plain[4], plain[5]]);
    let session_key = u32::from_le_bytes([plain[8], plain[9], plain[10], plain[11]]);
    assert_eq!(port, expected_base_port);

    let mut client = SimClient::connect(SocketAddr::V4(SocketAddrV4::new(ip, port)), bf);
    let mut login_args = BinaryWriter::new();
    login_args.u32(session_key);
    login_args.i32(0);
    let mut enc = ElementEncoder::new();
    enc.request(
        baseapp_ext_ids::BASE_APP_LOGIN,
        LengthStyle::Variable(2),
        10,
        login_args.as_slice(),
    );
    let m0 = client.send_off_channel_plaintext_and_collect(enc);
    assert_eq!(m0[0].id, REPLY_MESSAGE_IDENTIFIER);

    let mut enc2 = ElementEncoder::new();
    enc2.fixed_message(baseapp_ext_ids::AUTHENTICATE, 4, &session_key.to_le_bytes());
    enc2.fixed_message(baseapp_ext_ids::ENABLE_ENTITIES, 1, &[0]);
    let msgs = client.send_reliable_and_collect(enc2, Duration::from_secs(2));
    let create_bp = msgs
        .iter()
        .find(|m| m.id == client_ids::CREATE_BASE_PLAYER)
        .expect("CREATE_BASE_PLAYER(Account)");
    let mut pr = BinaryReader::new(&create_bp.data);
    let account_id = pr.i32().unwrap();
    assert_eq!(pr.u16().unwrap(), ACCOUNT_ENTITY_TYPE_ID);
    assert!(msgs.iter().any(|m| m.id == client_method::SHOW_GUI));
    (client, account_id)
}

fn sync_account_data(client: &mut SimClient, req_id: i16) -> Value {
    let mut bw = BinaryWriter::new();
    bw.i16(req_id);
    bw.i16(cmd::CMD_SYNC_DATA);
    bw.i32(0);
    bw.i32(0);
    bw.i32(0);
    let mut enc = ElementEncoder::new();
    enc.message(base_method::DO_CMD_INT3, LengthStyle::Variable(2), bw.as_slice());
    let msgs = client.send_reliable_and_collect(enc, Duration::from_secs(2));
    let mut compressed = Vec::new();
    for m in &msgs {
        if m.id == client_ids::RESOURCE_FRAGMENT {
            let mut r = BinaryReader::new(&m.data);
            if r.u16().unwrap() == req_id as u16 {
                let _seq = r.u8().unwrap();
                let _flags = r.u8().unwrap();
                compressed.extend_from_slice(r.rest());
            }
        }
    }
    pickle::loads(&pickle::zlib_decompress(&compressed).unwrap()).unwrap()
}

fn read_first_veh_inv_and_lock(sync: &Value) -> (i64, i64) {
    let inv = sync.get("inventory").unwrap().get_int(1).unwrap();
    let comp_dict = inv.get("compDescr").unwrap().as_dict().unwrap();
    let veh_inv_id = comp_dict[0].0.as_int().unwrap();
    let locks = sync
        .get("cache")
        .and_then(|c| c.get("vehsLock"))
        .and_then(|d| d.get_int(veh_inv_id))
        .and_then(|v| v.as_int())
        .unwrap_or(0);
    (veh_inv_id, locks)
}

fn shared_test_login_key() -> &'static LoginKey {
    static KEY: std::sync::OnceLock<LoginKey> = std::sync::OnceLock::new();
    KEY.get_or_init(|| LoginKey::generate(2048).unwrap())
}

struct TestHarness {
    _tmp: TempDir,
    login_addr: SocketAddr,
    base_port: u16,
    pubkey: RsaPublicKey,
    stop: Arc<AtomicBool>,
    login_thread: Option<std::thread::JoinHandle<()>>,
    base_task: tokio::task::JoinHandle<anyhow::Result<()>>,
}

impl TestHarness {
    async fn start(name: &str, battle_cfg: BattleHostConfig, use_retail: Option<PathBuf>) -> Self {
        let tmp = TempDir::new(name);
        let game_dir = match use_retail {
            Some(dir) => dir,
            None => {
                let synth = tmp.0.join("game");
                write_synthetic_game_dir(&synth);
                synth
            }
        };
        let session_file = tmp.0.join("session.json");
        let data_dir = tmp.0.join("accounts");
        let (ready_tx, ready_rx) = tokio::sync::oneshot::channel();
        let base_task = tokio::spawn(wot_baseapp::serve_with_battle_and_ready(
            BaseAppConfig {
                bind: "127.0.0.1:0".parse().unwrap(),
                session_file: session_file.clone(),
                game_dir,
                data_dir,
                preset: Preset::Retail,
                economics_config: None,
                overrides: PayloadOverrides::default(),
            },
            battle_cfg,
            Some(ready_tx),
        ));
        let base_bound = ready_rx.await.expect("baseapp ready");
        let base_v4 = match base_bound {
            SocketAddr::V4(v4) => v4,
            _ => unreachable!(),
        };

        let key = shared_test_login_key().clone();
        let pubkey = key.public_key().clone();
        let mut login_app = LoginApp::bind(
            LoginAppConfig {
                bind: "127.0.0.1:0".parse().unwrap(),
                base_app: base_v4,
                session_key: 0x0BAD_F00D,
                session_out: Some(session_file),
            },
            key,
        )
        .unwrap();
        let login_addr = login_app.local_addr().unwrap();
        let stop = Arc::new(AtomicBool::new(false));
        let stop_l = stop.clone();
        let login_thread = std::thread::spawn(move || {
            let _ = login_app.run(stop_l);
        });

        Self {
            _tmp: tmp,
            login_addr,
            base_port: base_v4.port(),
            pubkey,
            stop,
            login_thread: Some(login_thread),
            base_task,
        }
    }
}

impl Drop for TestHarness {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        let poke = UdpSocket::bind("127.0.0.1:0").unwrap();
        let _ = poke.send_to(b"\x00", self.login_addr);
        if let Some(t) = self.login_thread.take() {
            let _ = t.join();
        }
        self.base_task.abort();
    }
}

fn verify_avatar_world_and_return_to_hangar(
    client: &mut SimClient,
    expected_gui_type: u8,
    expected_vehicles: usize,
) {
    // 1. Client honors RESET_ENTITIES(0) by sending ENABLE_ENTITIES
    let expected_aoi_count = if expected_vehicles > 2 { expected_vehicles / 2 } else { 1 };
    let mut enc = ElementEncoder::new();
    enc.fixed_message(baseapp_ext_ids::ENABLE_ENTITIES, 1, &[0]);
    let msgs = client.send_reliable_and_collect_until(enc, Duration::from_secs(5), |ms| {
        ms.iter().any(|m| m.id == client_ids::CREATE_BASE_PLAYER)
            && ms.iter().any(|m| m.id == client_ids::CREATE_CELL_PLAYER)
            && ms.iter().filter(|m| m.id == client_ids::ENTER_AOI).count() >= expected_aoi_count
    });

    let bp = msgs
        .iter()
        .find(|m| m.id == client_ids::CREATE_BASE_PLAYER)
        .expect("CREATE_BASE_PLAYER(Avatar)");
    let mut br = BinaryReader::new(&bp.data);
    let avatar_id = br.i32().unwrap();
    assert_eq!(br.u16().unwrap(), AVATAR_ENTITY_TYPE_ID);
    assert!(avatar_id > 0 && avatar_id < (1 << 30));
    let _name = br.string().unwrap();
    let _arena_type_id = br.i32().unwrap();
    let arena_gui_type = br.u8().unwrap();
    assert_eq!(arena_gui_type, expected_gui_type);

    let space_data: Vec<SpaceData> = msgs
        .iter()
        .filter(|m| m.id == client_ids::SPACE_DATA)
        .map(|m| SpaceData::read_from(&mut BinaryReader::new(&m.data)).unwrap())
        .collect();
    assert!(
        space_data
            .iter()
            .any(|entry| entry.key == SPACE_DATA_MAPPING_KEY_CLIENT_SERVER),
        "space geometry"
    );
    assert!(
        space_data.iter().all(|entry| entry.key != SPACE_DATA_TOD_KEY),
        "time-of-day space data crashes the 0.6.5 client"
    );
    assert!(space_data.iter().any(|entry| entry.key == SPACE_DATA_WEATHER));

    let cp = msgs
        .iter()
        .find(|m| m.id == client_ids::CREATE_CELL_PLAYER)
        .expect("CREATE_CELL_PLAYER(Avatar)");
    let mut cr = BinaryReader::new(&cp.data);
    let space_id = cr.i32().unwrap();
    assert!(space_id > 0);
    let _veh_passenger = cr.i32().unwrap();
    let _pos = [cr.f32().unwrap(), cr.f32().unwrap(), cr.f32().unwrap()];
    let _dir = [cr.f32().unwrap(), cr.f32().unwrap(), cr.f32().unwrap()];
    let _team = cr.u8().unwrap();
    let player_vehicle_id = cr.i32().unwrap();
    assert!(player_vehicle_id > 0 && player_vehicle_id < (1 << 30));

    // Verify Avatar.updateArena(VEHICLE_LIST & PERIOD)
    let mut saw_vlist = false;
    let mut saw_period = false;
    for m in &msgs {
        if m.id == client_method::AVATAR_UPDATE_ARENA {
            let mut ur = BinaryReader::new(&m.data);
            let eid = ur.i32().unwrap();
            assert_eq!(eid, avatar_id);
            let kind = ur.u8().unwrap();
            let pickled = ur.string_bytes().unwrap();
            let val = pickle::loads(&pickled).unwrap();
            if kind == ARENA_UPDATE_VEHICLE_LIST {
                saw_vlist = true;
                assert_eq!(val.as_list().unwrap().len(), expected_vehicles);
            } else if kind == ARENA_UPDATE_PERIOD {
                saw_period = true;
            }
        }
    }
    assert!(saw_vlist && saw_period);

    // 2. Client processes ENTER_AOI -> REQUEST_ENTITY_UPDATE for each entered entity
    let aoi_ids: Vec<i32> = msgs
        .iter()
        .filter(|m| m.id == client_ids::ENTER_AOI)
        .map(|m| BinaryReader::new(&m.data).i32().unwrap())
        .collect();
    let veh_aoi_ids: Vec<i32> = aoi_ids
        .iter()
        .copied()
        .filter(|&id| id < wot_battle::sim::destructibles::DESTRUCTIBLE_ENTITY_ID_BASE)
        .collect();
    assert!(
        veh_aoi_ids.len() >= expected_aoi_count && veh_aoi_ids.len() <= expected_vehicles,
        "veh_aoi_ids len {} must be between {} and {}",
        veh_aoi_ids.len(),
        expected_aoi_count,
        expected_vehicles
    );
    assert!(veh_aoi_ids.contains(&player_vehicle_id));

    let aoi_count = aoi_ids.len();
    let mut req_enc = ElementEncoder::new();
    for id in &aoi_ids {
        req_enc.message(
            baseapp_ext_ids::REQUEST_ENTITY_UPDATE,
            LengthStyle::Variable(2),
            &id.to_le_bytes(),
        );
    }
    let created_msgs = client.send_reliable_and_collect_until(req_enc, Duration::from_secs(5), |ms| {
        ms.iter().filter(|m| m.id == client_ids::CREATE_ENTITY).count() >= aoi_count
    });
    let created_entities: Vec<i32> = created_msgs
        .iter()
        .filter(|m| m.id == client_ids::CREATE_ENTITY)
        .map(|m| {
            let mut r = BinaryReader::new(&m.data);
            let _comp = r.u8().unwrap();
            let eid = r.i32().unwrap();
            let etype = r.u16().unwrap();
            if eid >= wot_battle::sim::destructibles::DESTRUCTIBLE_ENTITY_ID_BASE {
                assert_eq!(etype, AREA_DESTRUCTIBLES_ENTITY_TYPE_ID);
            } else {
                assert_eq!(etype, VEHICLE_ENTITY_TYPE_ID);
            }
            eid
        })
        .collect();
    assert_eq!(created_entities.len(), aoi_count);

    // 3. Send Avatar.onClientReady(0xCA)
    let mut ready_enc = ElementEncoder::new();
    ready_enc.message(base_method::AVATAR_ON_CLIENT_READY, LengthStyle::Variable(2), &[]);
    let ready_msgs = client.send_reliable_and_collect_until(ready_enc, Duration::from_secs(5), |ms| {
        ms.iter().any(|m| {
            m.id == client_method::AVATAR_UPDATE_ARENA
                && BinaryReader::new(&m.data)
                    .i32()
                    .ok()
                    .and_then(|_| m.data.get(4).copied())
                    == Some(ARENA_UPDATE_AVATAR_READY)
        })
    });
    assert!(ready_msgs.iter().any(|m| {
        if m.id == client_method::AVATAR_UPDATE_ARENA {
            let mut r = BinaryReader::new(&m.data);
            let _ = r.i32();
            r.u8().ok() == Some(ARENA_UPDATE_AVATAR_READY)
        } else {
            false
        }
    }));

    // 4. Send Avatar.leaveArena(0xCB) -> expect RESET_ENTITIES(0)
    let mut leave_enc = ElementEncoder::new();
    leave_enc.message(base_method::AVATAR_LEAVE_ARENA, LengthStyle::Variable(2), &[]);
    let leave_msgs = client.send_reliable_and_collect_until(leave_enc, Duration::from_secs(5), |ms| {
        ms.iter().any(|m| m.id == client_ids::RESET_ENTITIES)
    });
    let reset = leave_msgs
        .iter()
        .find(|m| m.id == client_ids::RESET_ENTITIES)
        .expect("RESET_ENTITIES on leaveArena");
    assert_eq!(reset.data, vec![0]);

    // 5. ENABLE_ENTITIES -> CREATE_BASE_PLAYER(Account) + SHOW_GUI
    let mut back_enc = ElementEncoder::new();
    back_enc.fixed_message(baseapp_ext_ids::ENABLE_ENTITIES, 1, &[0]);
    let back_msgs = client.send_reliable_and_collect_until(back_enc, Duration::from_secs(5), |ms| {
        ms.iter().any(|m| m.id == client_ids::CREATE_BASE_PLAYER)
            && ms.iter().any(|m| m.id == client_method::SHOW_GUI)
    });
    let account_bp = back_msgs
        .iter()
        .find(|m| m.id == client_ids::CREATE_BASE_PLAYER)
        .expect("CREATE_BASE_PLAYER(Account) after leaving arena");
    let mut abr = BinaryReader::new(&account_bp.data);
    let _acc_id = abr.i32().unwrap();
    assert_eq!(abr.u16().unwrap(), ACCOUNT_ENTITY_TYPE_ID);
    assert!(back_msgs.iter().any(|m| m.id == client_method::SHOW_GUI));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn random_battle_queue_to_arena_and_back_to_hangar() {
    let battle_cfg = BattleHostConfig {
        queue_wait_timeout: Duration::from_millis(120),
        team_size: 2,
        bot_fill: true,
        tick_interval: Duration::from_millis(40),
        preloaded_maps: Some(build_synthetic_map_store()),
        ..BattleHostConfig::default()
    };
    let h = TestHarness::start("random_battle", battle_cfg, None).await;
    let login_addr = h.login_addr;
    let base_port = h.base_port;
    let pubkey = h.pubkey.clone();

    tokio::task::spawn_blocking(move || {
        let (mut client, account_id) =
            login_to_hangar(login_addr, base_port, &pubkey, b"rand@test.com", b"rand_blowfish_k!");
        let sync1 = sync_account_data(&mut client, 120);
        let battles_till_captcha = sync1
            .get("cache")
            .and_then(|cache| cache.get("battlesTillCaptcha"))
            .and_then(Value::as_int);
        assert!(
            battles_till_captcha.is_some_and(|count| count > 0),
            "the client must be able to queue without a CAPTCHA challenge, got {battles_till_captcha:?}"
        );
        let (veh_inv_id, lock0) = read_first_veh_inv_and_lock(&sync1);
        assert_eq!(lock0, 0);

        // Enqueue via CMD_ENQUEUE_FOR_ARENA -> expect onEnqueued(0x85), then onArenaCreated(0x88) + RESET_ENTITIES(0)
        let mut bw = BinaryWriter::new();
        bw.i16(cmd::REQUEST_ID_NO_RESPONSE);
        bw.i16(cmd::CMD_ENQUEUE_FOR_ARENA);
        bw.i32(veh_inv_id as i32);
        bw.i32(0); // random map
        bw.i32(1); // QUEUE_TYPE.RANDOMS
        let mut enc = ElementEncoder::new();
        enc.message(base_method::DO_CMD_INT3, LengthStyle::Variable(2), bw.as_slice());

        client.send_reliable_only(enc);
        let msgs = client.collect_until(Duration::from_secs(3), |ms| {
            ms.iter().any(|m| m.id == client_ids::RESET_ENTITIES)
        });
        let got_ids: Vec<u8> = msgs.iter().map(|m| m.id).collect();
        assert!(
            msgs.iter().any(|m| {
                m.id == client_method::ON_ENQUEUED
                    && BinaryReader::new(&m.data).i32().ok() == Some(account_id)
            }),
            "missing ON_ENQUEUED in {got_ids:?}"
        );
        assert!(
            msgs.iter().any(|m| m.id == client_method::ON_ARENA_CREATED),
            "missing ON_ARENA_CREATED in {got_ids:?}"
        );
        assert!(
            msgs.iter()
                .any(|m| m.id == client_ids::RESET_ENTITIES && m.data == vec![0]),
            "missing RESET_ENTITIES(0) in {got_ids:?}"
        );

        // Complete Avatar <-> Vehicle entry and leave back to Hangar
        verify_avatar_world_and_return_to_hangar(&mut client, ARENA_GUI_TYPE_RANDOM, 4);

        // Verify tank is unlocked after leaving arena
        let sync2 = sync_account_data(&mut client, 121);
        let (_, lock_after) = read_first_veh_inv_and_lock(&sync2);
        assert_eq!(lock_after, 0, "vehicle must be unlocked on return to Hangar");
    })
    .await
    .unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn training_room_prebattle_to_arena_and_back_to_hangar() {
    let battle_cfg = BattleHostConfig {
        team_size: 2,
        tick_interval: Duration::from_millis(40),
        preloaded_maps: Some(build_synthetic_map_store()),
        ..BattleHostConfig::default()
    };
    let h = TestHarness::start("training_room", battle_cfg, None).await;
    let login_addr = h.login_addr;
    let base_port = h.base_port;
    let pubkey = h.pubkey.clone();

    tokio::task::spawn_blocking(move || {
        let (mut client, _) = login_to_hangar(
            login_addr,
            base_port,
            &pubkey,
            b"train@test.com",
            b"train_blowfish!!",
        );
        let sync1 = sync_account_data(&mut client, 120);
        let (veh_inv_id, _) = read_first_veh_inv_and_lock(&sync1);

        // Account.createTraining(arenaTypeID=3, roundLength=900, isOpened=1, comment="test")
        let mut cw = BinaryWriter::new();
        cw.i32(3);
        cw.i32(900);
        cw.u8(1);
        cw.string("test");
        let mut enc = ElementEncoder::new();
        enc.message(base_method::CREATE_TRAINING, LengthStyle::Variable(2), cw.as_slice());
        let msgs = client.send_reliable_and_collect_until(enc, Duration::from_secs(5), |ms| {
            ms.iter().any(|m| m.id == client_method::ON_PREBATTLE_JOINED)
                && ms
                    .iter()
                    .filter(|m| m.id == client_method::UPDATE_PREBATTLE)
                    .count()
                    >= 3
        });
        assert!(msgs.iter().any(|m| m.id == client_method::ON_PREBATTLE_JOINED));

        let mut saw_settings = false;
        let mut saw_bot_ready = false;
        let mut saw_team_states = false;
        for m in &msgs {
            if m.id == client_method::UPDATE_PREBATTLE {
                let mut r = BinaryReader::new(&m.data);
                let _ = r.i32().unwrap();
                let utype = r.u8().unwrap();
                let val = pickle::loads(&r.string_bytes().unwrap()).unwrap();
                match utype {
                    PREBATTLE_UPDATE_SETTINGS => saw_settings = true,
                    PREBATTLE_UPDATE_ROSTER => {
                        let list = val.as_list().unwrap();
                        if list.iter().any(|item| {
                            let t = item.as_list().unwrap();
                            t[3].as_int() == Some(2)
                                && t[4].as_int() == Some(PREBATTLE_ACCOUNT_STATE_READY as i64)
                        }) {
                            saw_bot_ready = true;
                        }
                    }
                    PREBATTLE_UPDATE_TEAM_STATES => saw_team_states = true,
                    _ => {}
                }
            }
        }
        assert!(saw_settings && saw_bot_ready && saw_team_states);

        // Ready player + both team readies
        let mut rw = BinaryWriter::new();
        rw.i16(121);
        rw.i16(cmd::CMD_PRB_READY);
        rw.i32(veh_inv_id as i32);
        rw.i32(0);
        rw.i32(0);
        let mut renc = ElementEncoder::new();
        renc.message(base_method::DO_CMD_INT3, LengthStyle::Variable(2), rw.as_slice());
        let _ = client.send_reliable_and_collect(renc, Duration::from_secs(1));

        let mut t1w = BinaryWriter::new();
        t1w.i16(122);
        t1w.i16(cmd::CMD_PRB_TEAM_READY);
        t1w.i32(1);
        t1w.i32(1);
        t1w.i32(0);
        let mut t2w = BinaryWriter::new();
        t2w.i16(123);
        t2w.i16(cmd::CMD_PRB_TEAM_READY);
        t2w.i32(2);
        t2w.i32(1);
        t2w.i32(0);
        let mut tenc = ElementEncoder::new();
        tenc.message(base_method::DO_CMD_INT3, LengthStyle::Variable(2), t1w.as_slice());
        tenc.message(base_method::DO_CMD_INT3, LengthStyle::Variable(2), t2w.as_slice());
        let launch_msgs = client.send_reliable_and_collect_until(tenc, Duration::from_secs(5), |ms| {
            ms.iter().any(|m| m.id == client_ids::RESET_ENTITIES)
        });
        assert!(launch_msgs.iter().any(|m| m.id == client_method::ON_ARENA_CREATED));
        assert!(launch_msgs
            .iter()
            .any(|m| m.id == client_ids::RESET_ENTITIES && m.data == vec![0]));

        // Enter Training arena (1 human + 1 Team-2 virtual bot) and return
        verify_avatar_world_and_return_to_hangar(&mut client, ARENA_GUI_TYPE_TRAINING, 2);

        let sync2 = sync_account_data(&mut client, 124);
        let (_, lock_after) = read_first_veh_inv_and_lock(&sync2);
        assert_eq!(lock_after, 0);
    })
    .await
    .unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn disconnect_during_switching_to_arena_unlocks_and_allows_relogin() {
    let battle_cfg = BattleHostConfig {
        queue_wait_timeout: Duration::from_secs(10),
        team_size: 1,
        bot_fill: true,
        tick_interval: Duration::from_millis(40),
        switching_timeout: Duration::from_millis(250),
        preloaded_maps: Some(build_synthetic_map_store()),
        ..BattleHostConfig::default()
    };
    let h = TestHarness::start("disconnect_switching", battle_cfg, None).await;
    let login_addr = h.login_addr;
    let base_port = h.base_port;
    let pubkey = h.pubkey.clone();

    tokio::task::spawn_blocking(move || {
        let (mut client1, _) = login_to_hangar(
            login_addr,
            base_port,
            &pubkey,
            b"dropper@test.com",
            b"drop_blowfish_1!",
        );
        let sync1 = sync_account_data(&mut client1, 120);
        let (veh_inv_id, _) = read_first_veh_inv_and_lock(&sync1);

        // Enqueue + CMD_FORCE_QUEUE -> receive onArenaCreated + RESET_ENTITIES(0)
        let mut bw1 = BinaryWriter::new();
        bw1.i16(cmd::REQUEST_ID_NO_RESPONSE);
        bw1.i16(cmd::CMD_ENQUEUE_FOR_ARENA);
        bw1.i32(veh_inv_id as i32);
        bw1.i32(0);
        bw1.i32(1);
        let mut bw2 = BinaryWriter::new();
        bw2.i16(cmd::REQUEST_ID_NO_RESPONSE);
        bw2.i16(cmd::CMD_FORCE_QUEUE);
        bw2.i32(0);
        bw2.i32(0);
        bw2.i32(0);
        let mut enc = ElementEncoder::new();
        enc.message(base_method::DO_CMD_INT3, LengthStyle::Variable(2), bw1.as_slice());
        enc.message(base_method::DO_CMD_INT3, LengthStyle::Variable(2), bw2.as_slice());
        let msgs = client1.send_reliable_and_collect_until(enc, Duration::from_secs(5), |ms| {
            ms.iter().any(|m| m.id == client_ids::RESET_ENTITIES)
        });
        assert!(msgs.iter().any(|m| m.id == client_method::ON_ARENA_CREATED));
        assert!(msgs
            .iter()
            .any(|m| m.id == client_ids::RESET_ENTITIES && m.data == vec![0]));

        // Drop BEFORE sending ENABLE_ENTITIES, wait beyond switching_timeout for cleanup
        drop(client1);
        std::thread::sleep(Duration::from_millis(450));

        // Re-login with same account -> must land cleanly in Hangar with unlocked vehicle
        let (mut client2, _) = login_to_hangar(
            login_addr,
            base_port,
            &pubkey,
            b"dropper@test.com",
            b"drop_blowfish_2!",
        );
        let sync2 = sync_account_data(&mut client2, 121);
        let (_, lock_after) = read_first_veh_inv_and_lock(&sync2);
        assert_eq!(lock_after, 0, "abandoned vehicle must be unlocked on re-login");
    })
    .await
    .unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn retail_assets_random_battle_smoke() {
    let Some(retail_dir) = wot065_data::testutil::game_dir() else {
        return;
    };

    let battle_cfg = BattleHostConfig {
        queue_wait_timeout: Duration::from_millis(120),
        team_size: 2,
        bot_fill: true,
        tick_interval: Duration::from_millis(40),
        ..BattleHostConfig::default()
    };
    let h = TestHarness::start("retail_smoke", battle_cfg, Some(retail_dir)).await;
    let login_addr = h.login_addr;
    let base_port = h.base_port;
    let pubkey = h.pubkey.clone();

    tokio::task::spawn_blocking(move || {
        let (mut client, _) = login_to_hangar(
            login_addr,
            base_port,
            &pubkey,
            b"retail@test.com",
            b"retail_blowfish!",
        );
        let sync1 = sync_account_data(&mut client, 120);
        let (veh_inv_id, _) = read_first_veh_inv_and_lock(&sync1);

        let mut bw1 = BinaryWriter::new();
        bw1.i16(cmd::REQUEST_ID_NO_RESPONSE);
        bw1.i16(cmd::CMD_ENQUEUE_FOR_ARENA);
        bw1.i32(veh_inv_id as i32);
        bw1.i32(3); // 04_himmelsdorf
        bw1.i32(1);
        let mut bw2 = BinaryWriter::new();
        bw2.i16(cmd::REQUEST_ID_NO_RESPONSE);
        bw2.i16(cmd::CMD_FORCE_QUEUE);
        bw2.i32(0);
        bw2.i32(0);
        bw2.i32(0);
        let mut enc = ElementEncoder::new();
        enc.message(base_method::DO_CMD_INT3, LengthStyle::Variable(2), bw1.as_slice());
        enc.message(base_method::DO_CMD_INT3, LengthStyle::Variable(2), bw2.as_slice());
        client.send_reliable_only(enc);
        let msgs = client.collect_until(Duration::from_secs(20), |ms| {
            ms.iter().any(|m| {
                m.id == client_ids::RESET_ENTITIES || m.id == client_method::ON_ARENA_JOIN_FAILURE
            })
        });
        let got_ids: Vec<u8> = msgs.iter().map(|m| m.id).collect();
        assert!(
            msgs.iter().any(|m| m.id == client_method::ON_ARENA_CREATED),
            "missing ON_ARENA_CREATED in {got_ids:?}"
        );

        verify_avatar_world_and_return_to_hangar(&mut client, ARENA_GUI_TYPE_RANDOM, 4);
    })
    .await
    .unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn training_room_actor_id_syncs_after_battle_and_arena_clock_matches() {
    let battle_cfg = BattleHostConfig {
        team_size: 2,
        tick_interval: Duration::from_millis(40),
        preloaded_maps: Some(build_synthetic_map_store()),
        ..BattleHostConfig::default()
    };
    let h = TestHarness::start("training_actor_sync", battle_cfg, None).await;
    let login_addr = h.login_addr;
    let base_port = h.base_port;
    let pubkey = h.pubkey.clone();

    tokio::task::spawn_blocking(move || {
        let (mut client, account_id_1) = login_to_hangar(
            login_addr,
            base_port,
            &pubkey,
            b"actor1@test.com",
            b"actor_blowfish1!",
        );
        let sync1 = sync_account_data(&mut client, 120);
        let (veh_inv_id, _) = read_first_veh_inv_and_lock(&sync1);

        // Spend time in Hangar before creating & starting training room
        std::thread::sleep(Duration::from_millis(250));

        let mut cw = BinaryWriter::new();
        cw.i32(3);
        cw.i32(900);
        cw.u8(1);
        cw.string("sync room");
        let mut enc = ElementEncoder::new();
        enc.message(base_method::CREATE_TRAINING, LengthStyle::Variable(2), cw.as_slice());
        let create_msgs = client.send_reliable_and_collect_until(enc, Duration::from_secs(5), |ms| {
            ms.iter().filter(|m| m.id == client_method::UPDATE_PREBATTLE).count() >= 3
        });

        let initial_creator_actor = create_msgs
            .iter()
            .find_map(|m| {
                if m.id == client_method::UPDATE_PREBATTLE {
                    let mut r = BinaryReader::new(&m.data);
                    let _ = r.i32().ok()?;
                    if r.u8().ok()? == PREBATTLE_UPDATE_ROSTER {
                        let list = pickle::loads(&r.string_bytes().ok()?).ok()?;
                        for item in list.as_list()? {
                            let t = item.as_list()?;
                            if t[1].as_bytes() == Some(b"actor1@test.com") {
                                return t[0].as_int();
                            }
                        }
                    }
                }
                None
            })
            .expect("creator roster entry");
        assert_eq!(initial_creator_actor, account_id_1 as i64);

        let mut rw = BinaryWriter::new();
        rw.i16(121);
        rw.i16(cmd::CMD_PRB_READY);
        rw.i32(veh_inv_id as i32);
        rw.i32(0);
        rw.i32(0);
        let mut t1w = BinaryWriter::new();
        t1w.i16(122);
        t1w.i16(cmd::CMD_PRB_TEAM_READY);
        t1w.i32(1);
        t1w.i32(1);
        t1w.i32(0);
        let mut t2w = BinaryWriter::new();
        t2w.i16(123);
        t2w.i16(cmd::CMD_PRB_TEAM_READY);
        t2w.i32(2);
        t2w.i32(1);
        t2w.i32(0);
        let mut start_enc = ElementEncoder::new();
        start_enc.message(base_method::DO_CMD_INT3, LengthStyle::Variable(2), rw.as_slice());
        start_enc.message(base_method::DO_CMD_INT3, LengthStyle::Variable(2), t1w.as_slice());
        start_enc.message(base_method::DO_CMD_INT3, LengthStyle::Variable(2), t2w.as_slice());
        let _ = client.send_reliable_and_collect_until(start_enc, Duration::from_secs(5), |ms| {
            ms.iter().any(|m| m.id == client_ids::RESET_ENTITIES)
        });

        // Arena entry ENABLE_ENTITIES must emit SET_GAME_TIME synced to arena clock
        let mut en_enc = ElementEncoder::new();
        en_enc.fixed_message(baseapp_ext_ids::ENABLE_ENTITIES, 1, &[0]);
        let arena_msgs = client.send_reliable_and_collect_until(en_enc, Duration::from_secs(5), |ms| {
            ms.iter().any(|m| m.id == client_ids::CREATE_CELL_PLAYER)
                && ms.iter().any(|m| m.id == client_method::AVATAR_UPDATE_ARENA)
        });
        let game_time_msg = arena_msgs
            .iter()
            .find(|m| m.id == client_ids::SET_GAME_TIME)
            .expect("SET_GAME_TIME must be emitted on SwitchingToArena -> ENABLE_ENTITIES");
        let game_ticks = BinaryReader::new(&game_time_msg.data).u32().unwrap();
        assert!(game_ticks <= 25, "arena SET_GAME_TIME should track arena elapsed time, got {game_ticks}");
        let period_end_time = |messages: &[DecodedMessage], period| messages
            .iter()
            .find_map(|m| {
                if m.id == client_method::AVATAR_UPDATE_ARENA {
                    let mut r = BinaryReader::new(&m.data);
                    let _ = r.i32().ok()?;
                    if r.u8().ok()? == ARENA_UPDATE_PERIOD {
                        let tuple = pickle::loads(&r.string_bytes().ok()?).ok()?;
                        let tuple = tuple.as_list()?;
                        return (tuple[0].as_int()? == period).then(|| tuple[1].as_float()).flatten();
                    }
                }
                None
            });
        assert!(period_end_time(&arena_msgs, 1).is_some(), "arena waits for onClientReady");
        let mut ready = ElementEncoder::new();
        ready.message(base_method::AVATAR_ON_CLIENT_READY, LengthStyle::Variable(2), &[]);
        let period_messages = client.send_reliable_and_collect_until(ready, Duration::from_secs(5), |ms| {
            period_end_time(ms, 3).is_some()
        });
        let period_end_time = period_end_time(&period_messages, 3).expect("battle period after readiness");
        let server_time = (game_ticks as f64) / 10.0;
        assert!(
            period_end_time - server_time > 1.0,
            "Battle periodEndTime ({period_end_time}) must stay ahead of BigWorld.serverTime() ({server_time})"
        );

        // Leave arena back to Hangar -> verify SET_GAME_TIME + new Account entity ID == roster actor_id
        let mut leave_enc = ElementEncoder::new();
        leave_enc.message(base_method::AVATAR_LEAVE_ARENA, LengthStyle::Variable(2), &[]);
        let _ = client.send_reliable_and_collect_until(leave_enc, Duration::from_secs(5), |ms| {
            ms.iter().any(|m| m.id == client_ids::RESET_ENTITIES)
        });

        let mut back_enc = ElementEncoder::new();
        back_enc.fixed_message(baseapp_ext_ids::ENABLE_ENTITIES, 1, &[0]);
        let back_msgs = client.send_reliable_and_collect_until(back_enc, Duration::from_secs(5), |ms| {
            ms.iter().any(|m| m.id == client_ids::CREATE_BASE_PLAYER)
                && ms.iter().any(|m| m.id == client_method::SHOW_GUI)
                && ms.iter().any(|m| m.id == client_method::UPDATE_PREBATTLE)
        });
        assert!(
            back_msgs.iter().any(|m| m.id == client_ids::SET_GAME_TIME),
            "SET_GAME_TIME must be emitted on SwitchingToHangar -> ENABLE_ENTITIES"
        );
        let bp = back_msgs
            .iter()
            .find(|m| m.id == client_ids::CREATE_BASE_PLAYER)
            .unwrap();
        let new_account_id = BinaryReader::new(&bp.data).i32().unwrap();
        assert_ne!(new_account_id, account_id_1);

        let returned_actor_id = back_msgs
            .iter()
            .find_map(|m| {
                if m.id == client_method::UPDATE_PREBATTLE {
                    let mut r = BinaryReader::new(&m.data);
                    let _ = r.i32().ok()?;
                    if r.u8().ok()? == PREBATTLE_UPDATE_ROSTER {
                        let list = pickle::loads(&r.string_bytes().ok()?).ok()?;
                        for item in list.as_list()? {
                            let t = item.as_list()?;
                            if t[1].as_bytes() == Some(b"actor1@test.com") {
                                return t[0].as_int();
                            }
                        }
                    }
                }
                None
            })
            .expect("creator roster entry on return to Hangar");
        assert_eq!(
            returned_actor_id, new_account_id as i64,
            "PrebattleMember.actor_id must match BigWorld.player().id after battle return"
        );

        // Re-ready after battle return -> succeeds with fresh Hangar Account entity ID
        let mut rdy2 = BinaryWriter::new();
        rdy2.i16(202);
        rdy2.i16(cmd::CMD_PRB_READY);
        rdy2.i32(0);
        rdy2.i32(0);
        rdy2.i32(0);
        let mut rdy2_enc = ElementEncoder::new();
        rdy2_enc.message(base_method::DO_CMD_INT3, LengthStyle::Variable(2), rdy2.as_slice());
        let rdy2_msgs = client.send_reliable_and_collect_until(rdy2_enc, Duration::from_secs(3), |ms| {
            ms.iter().any(|m| m.id == client_method::ON_CMD_RESPONSE)
        });
        assert!(rdy2_msgs.iter().any(|m| {
            if m.id == client_method::ON_CMD_RESPONSE {
                let mut r = BinaryReader::new(&m.data);
                let _ = r.i32();
                r.i16().ok() == Some(202) && r.i16().ok() == Some(cmd::RES_SUCCESS)
            } else {
                false
            }
        }));
    })
    .await
    .unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn training_mid_battle_disconnect_clears_in_battle_for_hangar_observer() {
    let battle_cfg = BattleHostConfig {
        team_size: 2,
        tick_interval: Duration::from_millis(40),
        preloaded_maps: Some(build_synthetic_map_store()),
        ..BattleHostConfig::default()
    };
    let h = TestHarness::start("training_mid_drop", battle_cfg, None).await;
    let login_addr = h.login_addr;
    let base_port = h.base_port;
    let pubkey = h.pubkey.clone();

    tokio::task::spawn_blocking(move || {
        let (mut p1, _) = login_to_hangar(
            login_addr,
            base_port,
            &pubkey,
            b"host1@test.com",
            b"host1_blowfish_k",
        );
        let (mut p2, p2_account_id) = login_to_hangar(
            login_addr,
            base_port,
            &pubkey,
            b"guest2@test.com",
            b"guest2_blowfishk",
        );
        let s1 = sync_account_data(&mut p1, 120);
        let db1 = s1.get("databaseID").and_then(|v| v.as_int()).unwrap_or(1);
        let s2 = sync_account_data(&mut p2, 120);
        let db2 = s2.get("databaseID").and_then(|v| v.as_int()).unwrap_or(2);

        let mut cw = BinaryWriter::new();
        cw.i32(3);
        cw.i32(900);
        cw.u8(1);
        cw.string("drop room");
        let mut enc1 = ElementEncoder::new();
        enc1.message(base_method::CREATE_TRAINING, LengthStyle::Variable(2), cw.as_slice());
        let p1_cr = p1.send_reliable_and_collect_until(enc1, Duration::from_secs(5), |ms| {
            ms.iter().any(|m| m.id == client_method::ON_PREBATTLE_JOINED)
        });
        let prb_id = p1_cr
            .iter()
            .find_map(|m| {
                if m.id == client_method::ON_PREBATTLE_JOINED {
                    let mut r = BinaryReader::new(&m.data);
                    let _ = r.i32().ok()?;
                    r.i32().ok()
                } else {
                    None
                }
            })
            .unwrap();

        let mut jw = BinaryWriter::new();
        jw.i16(121);
        jw.i16(cmd::CMD_PRB_JOIN);
        jw.i32(prb_id);
        jw.i32(0);
        jw.i32(0);
        let mut enc2 = ElementEncoder::new();
        enc2.message(base_method::DO_CMD_INT3, LengthStyle::Variable(2), jw.as_slice());
        let _ = p2.send_reliable_and_collect_until(enc2, Duration::from_secs(5), |ms| {
            ms.iter().any(|m| m.id == client_method::ON_PREBATTLE_JOINED)
        });

        let mut aw = BinaryWriter::new();
        aw.i16(122);
        aw.i16(cmd::CMD_PRB_ASSIGN);
        aw.i32(p2_account_id);
        aw.i32(PREBATTLE_ROSTER_ASSIGNED_IN_TEAM1 as i32);
        aw.i32(0);
        let mut enc_a = ElementEncoder::new();
        enc_a.message(base_method::DO_CMD_INT3, LengthStyle::Variable(2), aw.as_slice());
        let _ = p1.send_reliable_and_collect(enc_a, Duration::from_millis(200));

        for (cli, r1, r2) in [(&mut p1, 123i16, db1), (&mut p2, 124i16, db2)] {
            let _ = r2;
            let mut rw = BinaryWriter::new();
            rw.i16(r1);
            rw.i16(cmd::CMD_PRB_READY);
            rw.i32(0);
            rw.i32(0);
            rw.i32(0);
            let mut enc_r = ElementEncoder::new();
            enc_r.message(base_method::DO_CMD_INT3, LengthStyle::Variable(2), rw.as_slice());
            let _ = cli.send_reliable_and_collect(enc_r, Duration::from_millis(150));
        }
        let mut t1w = BinaryWriter::new();
        t1w.i16(125);
        t1w.i16(cmd::CMD_PRB_TEAM_READY);
        t1w.i32(1);
        t1w.i32(1);
        t1w.i32(0);
        let mut t2w = BinaryWriter::new();
        t2w.i16(126);
        t2w.i16(cmd::CMD_PRB_TEAM_READY);
        t2w.i32(2);
        t2w.i32(1);
        t2w.i32(0);
        let mut start_enc = ElementEncoder::new();
        start_enc.message(base_method::DO_CMD_INT3, LengthStyle::Variable(2), t1w.as_slice());
        start_enc.message(base_method::DO_CMD_INT3, LengthStyle::Variable(2), t2w.as_slice());
        p1.send_reliable_only(start_enc);
        let _ = p1.collect_until(Duration::from_secs(5), |ms| {
            ms.iter().any(|m| m.id == client_ids::RESET_ENTITIES)
        });
        let _ = p2.collect_until(Duration::from_secs(5), |ms| {
            ms.iter().any(|m| m.id == client_ids::RESET_ENTITIES)
        });

        // Both enter arena
        for cli in [&mut p1, &mut p2] {
            let mut e = ElementEncoder::new();
            e.fixed_message(baseapp_ext_ids::ENABLE_ENTITIES, 1, &[0]);
            let _ = cli.send_reliable_and_collect_until(e, Duration::from_secs(5), |ms| {
                ms.iter().any(|m| m.id == client_ids::CREATE_CELL_PLAYER)
            });
        }

        // p1 leaves arena back to Hangar while p2 remains InArena
        let mut lv = ElementEncoder::new();
        lv.message(base_method::AVATAR_LEAVE_ARENA, LengthStyle::Variable(2), &[]);
        let _ = p1.send_reliable_and_collect_until(lv, Duration::from_secs(5), |ms| {
            ms.iter().any(|m| m.id == client_ids::RESET_ENTITIES)
        });
        let mut bk = ElementEncoder::new();
        bk.fixed_message(baseapp_ext_ids::ENABLE_ENTITIES, 1, &[0]);
        let _ = p1.send_reliable_and_collect_until(bk, Duration::from_secs(5), |ms| {
            ms.iter().any(|m| m.id == client_method::SHOW_GUI)
        });

        // p2 disconnects mid-battle -> p1 must get UPDATE_PREBATTLE & room state must reset to 0 (idle)
        let mut disc = ElementEncoder::new();
        disc.fixed_message(baseapp_ext_ids::DISCONNECT_CLIENT, 1, &[0]);
        p2.send_reliable_only(disc);

        let p1_updates = p1.collect_until(Duration::from_secs(3), |ms| {
            ms.iter().any(|m| m.id == client_method::UPDATE_PREBATTLE)
        });
        assert!(p1_updates
            .iter()
            .any(|m| m.id == client_method::UPDATE_PREBATTLE));

        let mut qw = BinaryWriter::new();
        qw.i16(130);
        qw.i16(cmd::CMD_REQ_PREBATTLES);
        qw.i32(5);
        qw.i32(2);
        qw.i32(0);
        qw.i32(0);
        qw.i32(0);
        qw.i32(20);
        let mut qenc = ElementEncoder::new();
        qenc.message(base_method::DO_CMD_INT_ARR, LengthStyle::Variable(2), qw.as_slice());
        let qmsgs = p1.send_reliable_and_collect_until(qenc, Duration::from_secs(5), |ms| {
            ms.iter().any(|m| {
                m.id == client_ids::RESOURCE_FRAGMENT
                    && m.data.len() >= 4
                    && u16::from_le_bytes([m.data[0], m.data[1]]) == 130
                    && m.data[3] == 1
            })
        });
        let mut compressed = Vec::new();
        for m in qmsgs {
            if m.id == client_ids::RESOURCE_FRAGMENT {
                let mut r = BinaryReader::new(&m.data);
                if r.u16().unwrap() == 130 {
                    let _seq = r.u8().unwrap();
                    let _flags = r.u8().unwrap();
                    compressed.extend_from_slice(r.rest());
                }
            }
        }
        let raw = pickle::zlib_decompress(&compressed).unwrap();
        let list_val = pickle::loads(&raw).unwrap();
        let entries = list_val.as_list().unwrap()[2].as_list().unwrap();
        assert_eq!(entries.len(), 1);
        let cache = entries[0].as_list().unwrap()[2].clone();
        assert_eq!(
            cache.get_int(PREBATTLE_CACHE_KEY_STATE).and_then(|v| v.as_int()),
            Some(0),
            "prebattle room state must reset from in_battle(1) to idle(0)"
        );
    })
    .await
    .unwrap();
}

fn vehicle_positions(messages: &[DecodedMessage], vehicle_id: i32) -> Vec<Vec3> {
    let mut reference = None;
    let mut positions = Vec::new();
    for message in messages {
        if message.id == client_ids::RELATIVE_POSITION {
            let mut reader = BinaryReader::new(&message.data);
            reference = Some(Vec3::new(
                reader.f32().unwrap(),
                reader.f32().unwrap(),
                reader.f32().unwrap(),
            ));
        } else if (client_ids::AVATAR_UPDATE_FIRST..=client_ids::AVATAR_UPDATE_LAST)
            .contains(&message.id)
        {
            let pose = S2CAvatarUpdate::decode(message.id, &message.data).unwrap();
            if let (IdKind::NoAlias(id), PosKind::FullPos(position)) = (pose.id, pose.pos) {
                if id == vehicle_id {
                    positions.push(reference.expect("vehicle position reference") + Vec3::from_array(position.decode()));
                }
            }
        }
    }
    positions
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn arena_client_movement_and_unqueued_force_queue_rejected() {
    let battle_cfg = BattleHostConfig {
        queue_wait_timeout: Duration::from_secs(10),
        team_size: 1,
        prebattle_countdown_secs: 0,
        bot_fill: true,
        tick_interval: Duration::from_millis(40),
        preloaded_maps: Some(build_synthetic_map_store()),
        ..BattleHostConfig::default()
    };
    let h = TestHarness::start("movement_and_fq", battle_cfg, None).await;
    let login_addr = h.login_addr;
    let base_port = h.base_port;
    let pubkey = h.pubkey.clone();

    tokio::task::spawn_blocking(move || {
        let (mut p1, _) = login_to_hangar(
            login_addr,
            base_port,
            &pubkey,
            b"mover1@test.com",
            b"mover1_blowfish!",
        );
        let (mut p2, _) = login_to_hangar(
            login_addr,
            base_port,
            &pubkey,
            b"bystander@test.com",
            b"mover2_blowfish!",
        );
        let sync1 = sync_account_data(&mut p1, 120);
        let (veh_inv_id, _) = read_first_veh_inv_and_lock(&sync1);

        let mut eq = BinaryWriter::new();
        eq.i16(cmd::REQUEST_ID_NO_RESPONSE);
        eq.i16(cmd::CMD_ENQUEUE_FOR_ARENA);
        eq.i32(veh_inv_id as i32);
        eq.i32(0);
        eq.i32(1);
        let mut enc1 = ElementEncoder::new();
        enc1.message(base_method::DO_CMD_INT3, LengthStyle::Variable(2), eq.as_slice());
        let _ = p1.send_reliable_and_collect_until(enc1, Duration::from_secs(3), |ms| {
            ms.iter().any(|m| m.id == client_method::ON_ENQUEUED)
        });

        // Bystander p2 is NOT queued -> CMD_FORCE_QUEUE must fail
        let mut fq2 = BinaryWriter::new();
        fq2.i16(200);
        fq2.i16(cmd::CMD_FORCE_QUEUE);
        fq2.i32(0);
        fq2.i32(0);
        fq2.i32(0);
        let mut enc2 = ElementEncoder::new();
        enc2.message(base_method::DO_CMD_INT3, LengthStyle::Variable(2), fq2.as_slice());
        let p2_resp = p2.send_reliable_and_collect_until(enc2, Duration::from_secs(2), |ms| {
            ms.iter().any(|m| m.id == client_method::ON_CMD_RESPONSE)
        });
        assert!(p2_resp.iter().any(|m| {
            if m.id == client_method::ON_CMD_RESPONSE {
                let mut r = BinaryReader::new(&m.data);
                let _ = r.i32();
                r.i16().ok() == Some(200) && r.i16().ok() == Some(cmd::RES_FAILURE)
            } else {
                false
            }
        }));

        // Enqueued p1 CAN force queue
        let mut fq1 = BinaryWriter::new();
        fq1.i16(201);
        fq1.i16(cmd::CMD_FORCE_QUEUE);
        fq1.i32(0);
        fq1.i32(0);
        fq1.i32(0);
        let mut enc_fq = ElementEncoder::new();
        enc_fq.message(base_method::DO_CMD_INT3, LengthStyle::Variable(2), fq1.as_slice());
        p1.send_reliable_only(enc_fq);
        let _ = p1.collect_until(Duration::from_secs(5), |ms| {
            ms.iter().any(|m| m.id == client_ids::RESET_ENTITIES)
        });

        let mut en = ElementEncoder::new();
        en.fixed_message(baseapp_ext_ids::ENABLE_ENTITIES, 1, &[0]);
        let arena_msgs = p1.send_reliable_and_collect_until(en, Duration::from_secs(5), |ms| {
            ms.iter().any(|m| m.id == client_ids::CREATE_BASE_PLAYER)
                && ms.iter().any(|m| m.id == client_ids::CREATE_CELL_PLAYER)
        });
        let cell_message = arena_msgs
            .iter()
            .find(|m| m.id == client_ids::CREATE_CELL_PLAYER)
            .unwrap();
        let cell = CreateCellPlayer::read_from(&mut BinaryReader::new(&cell_message.data)).unwrap();
        let vehicle_id = BinaryReader::new(&cell.cell_data[1..]).i32().unwrap();
        let spawn = Vec3::from_array(cell.pos);

        let mut request = ElementEncoder::new();
        request.message(
            baseapp_ext_ids::REQUEST_ENTITY_UPDATE,
            LengthStyle::Variable(2),
            &vehicle_id.to_le_bytes(),
        );
        let created = p1.send_reliable_and_collect_until(request, Duration::from_secs(2), |ms| {
            ms.iter().any(|m| m.id == client_ids::CREATE_ENTITY)
        });
        assert!(created.iter().any(|m| m.id == client_ids::CREATE_ENTITY));

        let mut ready = ElementEncoder::new();
        ready.message(base_method::AVATAR_ON_CLIENT_READY, LengthStyle::Variable(2), &[]);
        let ready_msgs = p1.send_reliable_and_collect_until(ready, Duration::from_secs(2), |ms| {
            !vehicle_positions(ms, vehicle_id).is_empty()
        });
        let ready_positions = vehicle_positions(&ready_msgs, vehicle_id);
        assert!(
            ready_positions.contains(&spawn),
            "own Vehicle {vehicle_id} must receive its spawn position {spawn} through the entity filter, got {ready_positions:?}"
        );

        // The client's Avatar coordinates are independent of the server-driven Vehicle.
        let upd = C2SAvatarUpdate {
            pos: [999.0, 4.0, -999.0],
            dir: YawPitchRoll::encode([0.25, 0.0, 0.0]),
            ref_num: 1,
            space_id: None,
            vehicle_id: None,
            on_ground: None,
            ward: None,
        };
        let mut mv = ElementEncoder::new();
        mv.fixed_message(baseapp_ext_ids::AVATAR_UPDATE_IMPLICIT, 16, &upd.encode_implicit());
        let snaps = p1.send_reliable_and_collect(mv, Duration::from_millis(200));
        let positions = vehicle_positions(&snaps, vehicle_id);
        assert!(!positions.is_empty(), "own Vehicle position samples");
        assert!(
            positions.iter().all(|pos| pos.distance(spawn) < 0.01),
            "Avatar pose overwrote Vehicle position: {positions:?}"
        );

        let mut mv_with = ElementEncoder::new();
        mv_with.message(base_method::AVATAR_VEHICLE_MOVE_WITH, LengthStyle::Variable(2), &[1u8]);
        let forward = Vec3::new(cell.dir.yaw.sin(), 0.0, cell.dir.yaw.cos());
        let advanced = |messages: &[DecodedMessage]| {
            vehicle_positions(messages, vehicle_id)
                .iter()
                .any(|pos| (*pos - spawn).dot(forward) > 0.1)
        };
        let moved_snaps = p1.send_reliable_and_collect_until(mv_with, Duration::from_secs(2), advanced);
        assert!(advanced(&moved_snaps), "moveWith must advance the Vehicle entity");
        assert!(!moved_snaps.iter().any(|m| {
            m.id == client_method::AVATAR_UPDATE_OWN_VEHICLE_POSITION
        }), "camera should keep following the in-world Vehicle");
    })
    .await
    .unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn training_two_player_post_battle_non_creator_self_assign_uses_fresh_actor_id() {
    let battle_cfg = BattleHostConfig {
        team_size: 2,
        tick_interval: Duration::from_millis(40),
        preloaded_maps: Some(build_synthetic_map_store()),
        ..BattleHostConfig::default()
    };
    let h = TestHarness::start("training_post_battle_assign", battle_cfg, None).await;
    let login_addr = h.login_addr;
    let base_port = h.base_port;
    let pubkey = h.pubkey.clone();

    tokio::task::spawn_blocking(move || {
        let (mut p1, _p1_account_id) = login_to_hangar(
            login_addr,
            base_port,
            &pubkey,
            b"host_assign@test.com",
            b"host_assign_bfk1",
        );
        let (mut p2, p2_account_id) = login_to_hangar(
            login_addr,
            base_port,
            &pubkey,
            b"guest_assign@test.com",
            b"guest_assign_bfk",
        );
        let _ = sync_account_data(&mut p1, 120);
        let s2 = sync_account_data(&mut p2, 120);
        let db2 = s2.get("databaseID").and_then(|v| v.as_int()).unwrap_or(2);

        let mut cw = BinaryWriter::new();
        cw.i32(3);
        cw.i32(900);
        cw.u8(1);
        cw.string("assign room");
        let mut enc1 = ElementEncoder::new();
        enc1.message(base_method::CREATE_TRAINING, LengthStyle::Variable(2), cw.as_slice());
        let p1_cr = p1.send_reliable_and_collect_until(enc1, Duration::from_secs(5), |ms| {
            ms.iter().any(|m| m.id == client_method::ON_PREBATTLE_JOINED)
        });
        let prb_id = p1_cr
            .iter()
            .find_map(|m| {
                if m.id == client_method::ON_PREBATTLE_JOINED {
                    let mut r = BinaryReader::new(&m.data);
                    let _ = r.i32().ok()?;
                    r.i32().ok()
                } else {
                    None
                }
            })
            .unwrap();

        let mut jw = BinaryWriter::new();
        jw.i16(121);
        jw.i16(cmd::CMD_PRB_JOIN);
        jw.i32(prb_id);
        jw.i32(0);
        jw.i32(0);
        let mut enc2 = ElementEncoder::new();
        enc2.message(base_method::DO_CMD_INT3, LengthStyle::Variable(2), jw.as_slice());
        let _ = p2.send_reliable_and_collect_until(enc2, Duration::from_secs(5), |ms| {
            ms.iter().any(|m| m.id == client_method::ON_PREBATTLE_JOINED)
        });

        let mut aw = BinaryWriter::new();
        aw.i16(122);
        aw.i16(cmd::CMD_PRB_ASSIGN);
        aw.i32(p2_account_id);
        aw.i32(PREBATTLE_ROSTER_ASSIGNED_IN_TEAM1 as i32);
        aw.i32(0);
        let mut enc_a = ElementEncoder::new();
        enc_a.message(base_method::DO_CMD_INT3, LengthStyle::Variable(2), aw.as_slice());
        let _ = p1.send_reliable_and_collect(enc_a, Duration::from_millis(200));

        for (cli, req) in [(&mut p1, 123i16), (&mut p2, 124i16)] {
            let mut rw = BinaryWriter::new();
            rw.i16(req);
            rw.i16(cmd::CMD_PRB_READY);
            rw.i32(0);
            rw.i32(0);
            rw.i32(0);
            let mut enc_r = ElementEncoder::new();
            enc_r.message(base_method::DO_CMD_INT3, LengthStyle::Variable(2), rw.as_slice());
            let _ = cli.send_reliable_and_collect(enc_r, Duration::from_millis(150));
        }

        let mut t1w = BinaryWriter::new();
        t1w.i16(125);
        t1w.i16(cmd::CMD_PRB_TEAM_READY);
        t1w.i32(1);
        t1w.i32(1);
        t1w.i32(0);
        let mut t2w = BinaryWriter::new();
        t2w.i16(126);
        t2w.i16(cmd::CMD_PRB_TEAM_READY);
        t2w.i32(2);
        t2w.i32(1);
        t2w.i32(0);
        let mut start_enc = ElementEncoder::new();
        start_enc.message(base_method::DO_CMD_INT3, LengthStyle::Variable(2), t1w.as_slice());
        start_enc.message(base_method::DO_CMD_INT3, LengthStyle::Variable(2), t2w.as_slice());
        p1.send_reliable_only(start_enc);
        let _ = p1.collect_until(Duration::from_secs(5), |ms| {
            ms.iter().any(|m| m.id == client_ids::RESET_ENTITIES)
        });
        let _ = p2.collect_until(Duration::from_secs(5), |ms| {
            ms.iter().any(|m| m.id == client_ids::RESET_ENTITIES)
        });

        for cli in [&mut p1, &mut p2] {
            let mut e = ElementEncoder::new();
            e.fixed_message(baseapp_ext_ids::ENABLE_ENTITIES, 1, &[0]);
            let _ = cli.send_reliable_and_collect_until(e, Duration::from_secs(5), |ms| {
                ms.iter().any(|m| m.id == client_ids::CREATE_CELL_PLAYER)
            });
        }

        // Both return to Hangar via AVATAR_LEAVE_ARENA -> ENABLE_ENTITIES
        let mut returned_ids = Vec::new();
        for cli in [&mut p1, &mut p2] {
            let mut lv = ElementEncoder::new();
            lv.message(base_method::AVATAR_LEAVE_ARENA, LengthStyle::Variable(2), &[]);
            let _ = cli.send_reliable_and_collect_until(lv, Duration::from_secs(5), |ms| {
                ms.iter().any(|m| m.id == client_ids::RESET_ENTITIES)
            });
            let mut bk = ElementEncoder::new();
            bk.fixed_message(baseapp_ext_ids::ENABLE_ENTITIES, 1, &[0]);
            let bk_msgs = cli.send_reliable_and_collect_until(bk, Duration::from_secs(5), |ms| {
                ms.iter().any(|m| m.id == client_ids::CREATE_BASE_PLAYER)
                    && ms.iter().any(|m| m.id == client_method::SHOW_GUI)
                    && ms.iter().any(|m| m.id == client_method::UPDATE_PREBATTLE)
            });
            let new_aid = BinaryReader::new(
                &bk_msgs
                    .iter()
                    .find(|m| m.id == client_ids::CREATE_BASE_PLAYER)
                    .unwrap()
                    .data,
            )
            .i32()
            .unwrap();
            returned_ids.push(new_aid);
        }
        let p1_new_actor_id = returned_ids[0];
        let p2_new_actor_id = returned_ids[1];
        assert_ne!(p2_new_actor_id, p2_account_id);
        assert_ne!(p2_new_actor_id as i64, db2);

        // Non-creator p2 self-assigns to Team 2 using its fresh post-battle Account actor_id
        let mut self_aw = BinaryWriter::new();
        self_aw.i16(301);
        self_aw.i16(cmd::CMD_PRB_ASSIGN);
        self_aw.i32(p2_new_actor_id);
        self_aw.i32(wot_battle::lobby::PREBATTLE_ROSTER_ASSIGNED_IN_TEAM2 as i32);
        self_aw.i32(0);
        let mut self_enc = ElementEncoder::new();
        self_enc.message(base_method::DO_CMD_INT3, LengthStyle::Variable(2), self_aw.as_slice());
        let resp_msgs = p2.send_reliable_and_collect_until(self_enc, Duration::from_secs(3), |ms| {
            ms.iter().any(|m| m.id == client_method::ON_CMD_RESPONSE)
        });
        assert!(
            resp_msgs.iter().any(|m| {
                if m.id == client_method::ON_CMD_RESPONSE {
                    let mut r = BinaryReader::new(&m.data);
                    let _ = r.i32();
                    r.i16().ok() == Some(301) && r.i16().ok() == Some(cmd::RES_SUCCESS)
                } else {
                    false
                }
            }),
            "non-creator self-assign with fresh post-battle actor_id must succeed"
        );

        // Non-creator p2 still cannot assign creator p1
        let mut other_aw = BinaryWriter::new();
        other_aw.i16(302);
        other_aw.i16(cmd::CMD_PRB_ASSIGN);
        other_aw.i32(p1_new_actor_id);
        other_aw.i32(wot_battle::lobby::PREBATTLE_ROSTER_ASSIGNED_IN_TEAM2 as i32);
        other_aw.i32(0);
        let mut other_enc = ElementEncoder::new();
        other_enc.message(base_method::DO_CMD_INT3, LengthStyle::Variable(2), other_aw.as_slice());
        let deny_msgs = p2.send_reliable_and_collect_until(other_enc, Duration::from_secs(3), |ms| {
            ms.iter().any(|m| m.id == client_method::ON_CMD_RESPONSE)
        });
        assert!(
            deny_msgs.iter().any(|m| {
                if m.id == client_method::ON_CMD_RESPONSE {
                    let mut r = BinaryReader::new(&m.data);
                    let _ = r.i32();
                    r.i16().ok() == Some(302) && r.i16().ok() == Some(cmd::RES_FAILURE)
                } else {
                    false
                }
            }),
            "non-creator must still be denied assigning another member"
        );
    })
    .await
    .unwrap();
}
