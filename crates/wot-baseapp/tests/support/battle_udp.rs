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
use glam::{Mat4, Vec3};
use rsa::traits::PublicKeyParts;
use rsa::{Oaep, RsaPublicKey};
use sha1::Sha1;
use wot065_data::account_state::Preset;
use wot065_data::pickle::{self, Value};
use wot_baseapp::channel::ChannelState;
use wot_baseapp::payloads::PayloadOverrides;
use wot_baseapp::protocol::{
    base_method, client_method, cmd, ACCOUNT_ENTITY_TYPE_ID,
};
use wot_baseapp::{BaseAppConfig, BattleHostConfig};
use wot_battle::assets::arena_defs::ArenaDef;
use wot_battle::assets::chunk::{ChunkCoords, ChunkKind, SpaceBounds, SpawnPoint};
use wot_battle::assets::destructibles::DestructibleKind;
use wot_battle::assets::map_store::{ArenaMap, ChunkDestructible, MapStore, TerrainMap};
use wot_battle::assets::terrain::{
    MaterialKinds, TerrainChunk, DOMINANT_TEX_GRID_SIZE, HEIGHTMAP_GRID_SIZE,
};
use wot_battle::balance::BalanceConfig;

struct TempDir(PathBuf);

impl TempDir {
    fn new(name: &str) -> Self {
        let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../target/test-tmp")
            .join(format!("combat_e2e_{name}_{}", std::process::id()));
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
            "<root><AP><id>1</id><price>5</price><kind>ARMOR_PIERCING</kind><caliber>20</caliber><isTracer>true</isTracer><effects>smallAP</effects><damage><armor>15</armor><devices>20</devices></damage></AP></root>",
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
            "<root><ids><G>1</G></ids><shared><G><level>1</level><price>100</price><weight>50</weight><maxAmmo>50</maxAmmo><pitchLimits>-10 20</pitchLimits><rotationSpeed>30</rotationSpeed><reloadTime>1.0</reloadTime><aimingTime>1.0</aimingTime><shotDispersionRadius>0.4</shotDispersionRadius><effects>small</effects><shots><AP><defaultPortion>1.0</defaultPortion><speed>500</speed><gravity>9.81</gravity><maxDistance>400</maxDistance><piercingPower>30 20</piercingPower></AP></shots></G></shared></root>",
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

fn build_synthetic_map_store(round_length_secs: u32) -> Arc<MapStore> {
    let def = ArenaDef {
        id: 3,
        name: "04_himmelsdorf".into(),
        display_name_key: "Himmelsdorf".into(),
        description_key: String::new(),
        geometry: "spaces/04_himmelsdorf".into(),
        bounding_box: ([-100.0, -100.0], [100.0, 100.0]),
        min_players_in_team: 1,
        max_players_in_team: 15,
        round_length_secs,
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
                    heights: vec![5.0; HEIGHTMAP_GRID_SIZE * HEIGHTMAP_GRID_SIZE],
                    min_y: 5.0,
                    max_y: 5.0,
                    dom_tex_indices: vec![0; DOMINANT_TEX_GRID_SIZE * DOMINANT_TEX_GRID_SIZE],
                    tex_names: vec![],
                },
            ));
        }
    }
    let terrain = TerrainMap::from_chunks(bounds, chunks).unwrap();
    let mats = Arc::new(MaterialKinds::parse(b"<root/>").unwrap());
    // Direct line of sight between spawns for combat testing
    let spawns = vec![
        SpawnPoint {
            team: 1,
            preferable_type: None,
            world_transform: Mat4::IDENTITY,
            position: Vec3::new(-30.0, 5.0, 0.0),
            yaw: std::f32::consts::FRAC_PI_2, // Facing East towards Team 2
        },
        SpawnPoint {
            team: 2,
            preferable_type: None,
            world_transform: Mat4::IDENTITY,
            position: Vec3::new(30.0, 5.0, 0.0),
            yaw: -std::f32::consts::FRAC_PI_2, // Facing West towards Team 1
        },
    ];
    let chunk_id = wot_battle::sim::destructibles::encode_chunk_id(0, 0);
    let mut destructibles_by_chunk = BTreeMap::new();
    destructibles_by_chunk.insert(
        chunk_id,
        vec![ChunkDestructible {
            chunk_id,
            index_in_chunk: 0,
            desc_id: 1,
            filename: "content/MilitaryEnvironment/mle001_fence/mle001_fence.model".into(),
            kind: DestructibleKind::Fragile {
                health: 10,
                kinetic_damage_correction: 0.0,
            },
            world_transform: Mat4::from_translation(Vec3::new(50.0, 5.0, 50.0)),
            position: Vec3::new(50.0, 5.0, 50.0),
            world_aabb_min: Vec3::new(49.0, 5.0, 49.0),
            world_aabb_max: Vec3::new(51.0, 7.0, 51.0),
            bsp: None,
            pieces: Vec::new(),
        }],
    );
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
        destructibles_by_chunk,
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
                    let reliable = rpkt.flags & FLAG_IS_RELIABLE != 0;
                    let rx = self.channel.receive(rpkt);
                    if reliable {
                        let ack = self.channel.send_ack_only(&self.key);
                        let _ = self.sock.send(&ack);
                    }
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
    async fn start(name: &str, battle_cfg: BattleHostConfig) -> Self {
        let tmp = TempDir::new(name);
        let game_dir = tmp.0.join("game");
        write_synthetic_game_dir(&game_dir);

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

fn login_to_hangar(
    login_addr: SocketAddr,
    expected_base_port: u16,
    pubkey: &RsaPublicKey,
    username: &[u8],
    blowfish_bytes: &[u8],
) -> (SimClient, i32) {
    let (client, account_id, _) = login_and_capture_hangar(
        login_addr, expected_base_port, pubkey, username, blowfish_bytes,
    );
    (client, account_id)
}

fn login_and_capture_hangar(
    login_addr: SocketAddr,
    expected_base_port: u16,
    pubkey: &RsaPublicKey,
    username: &[u8],
    blowfish_bytes: &[u8],
) -> (SimClient, i32, Vec<DecodedMessage>) {
    let (client, msgs) = login_and_capture_entities(
        login_addr, expected_base_port, pubkey, username, blowfish_bytes,
    );
    let create_bp = msgs
        .iter()
        .find(|m| m.id == client_ids::CREATE_BASE_PLAYER)
        .expect("CREATE_BASE_PLAYER(Account)");
    let mut pr = BinaryReader::new(&create_bp.data);
    let account_id = pr.i32().unwrap();
    assert_eq!(pr.u16().unwrap(), ACCOUNT_ENTITY_TYPE_ID);
    assert!(msgs.iter().any(|m| m.id == client_method::SHOW_GUI));
    (client, account_id, msgs)
}

fn login_and_capture_entities(
    login_addr: SocketAddr,
    expected_base_port: u16,
    pubkey: &RsaPublicKey,
    username: &[u8],
    blowfish_bytes: &[u8],
) -> (SimClient, Vec<DecodedMessage>) {
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
    (client, msgs)
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

fn read_first_veh_inv_id(sync: &Value) -> i64 {
    let inv = sync.get("inventory").unwrap().get_int(1).unwrap();
    let comp_dict = inv.get("compDescr").unwrap().as_dict().unwrap();
    comp_dict[0].0.as_int().unwrap()
}
