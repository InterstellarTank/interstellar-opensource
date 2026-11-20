//! End-to-end UDP tests covering LoginApp (RSA) -> JSON session handoff -> BaseApp (Blowfish) ->
//! account/shop/dossier streaming with 130 vehicles and dynamic session hot-reload.

use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4, UdpSocket};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use bw201::bundle::{decode_messages, encode_single_request, ElementEncoder};
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
use rsa::pkcs8::DecodePublicKey;
use rsa::traits::PublicKeyParts;
use rsa::{Oaep, RsaPublicKey};
use sha1::Sha1;
use wot065_data::pickle::{self, Value};
use wot_baseapp::channel::ChannelState;
use wot_baseapp::payloads::PayloadOverrides;
use wot_baseapp::protocol::{base_method, client_method, cmd, ACCOUNT_ENTITY_TYPE_ID};
use wot_baseapp::BaseAppConfig;

fn encrypt_login_request(
    pubkey: &RsaPublicKey,
    username: &[u8],
    password: &[u8],
    blowfish_key: &[u8],
    reply_id: u32,
) -> Vec<u8> {
    let mut plain = BinaryWriter::new();
    plain.u8(1); // LOG_ON_HAS_DIGEST
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
            .expect("RSA-OAEP encrypt");
        cipher.extend_from_slice(&encrypted);
    }

    // Retail 0.6.5 wire layout: u32 version(57) + u32 build_tag + RSA ciphertext
    let mut args = BinaryWriter::new();
    args.u32(57);
    args.u32(0x0000_0605);
    args.raw(&cipher);

    encode_single_request(login_ids::LOGIN, LengthStyle::Variable(2), reply_id, args.as_slice())
}

struct SimClient {
    sock: UdpSocket,
    key: BlowfishKey,
    channel: ChannelState,
    next_out_seq: u32,
}

impl SimClient {
    fn connect(addr: SocketAddr, key: BlowfishKey) -> Self {
        let sock = UdpSocket::bind("127.0.0.1:0").expect("bind client UDP");
        sock.connect(addr).expect("connect UDP");
        sock.set_read_timeout(Some(Duration::from_secs(5)))
            .expect("set read timeout");
        Self {
            sock,
            key,
            channel: ChannelState::new(),
            next_out_seq: 0,
        }
    }

    fn next_out_seq(&mut self) -> u32 {
        let s = self.next_out_seq;
        self.next_out_seq += 1;
        s
    }

    fn send_and_collect_replies(
        &mut self,
        enc: ElementEncoder,
    ) -> Vec<bw201::bundle::DecodedMessage> {
        let mut pkt = enc.finish();
        let seq = self.next_out_seq();
        pkt.config.sequence = Some(seq);
        pkt.config.cumulative_ack = Some(self.channel.in_seq_at());
        pkt.config.extra_flags |= FLAG_ON_CHANNEL | FLAG_IS_RELIABLE;
        pkt.finalize_flags();
        let plain = pkt.encode(Endian::Little);
        let wire = self.key.packet_encrypt(&plain);
        self.sock.send(&wire).expect("send UDP to BaseApp");
        self.collect_replies()
    }

    fn send_off_channel_plaintext_and_collect_replies(
        &mut self,
        enc: ElementEncoder,
    ) -> Vec<bw201::bundle::DecodedMessage> {
        let wire = enc.finish().encode(Endian::Little);
        self.sock
            .send(&wire)
            .expect("send off-channel plaintext UDP to BaseApp");
        self.collect_replies()
    }

    fn collect_replies(&mut self) -> Vec<bw201::bundle::DecodedMessage> {
        // Collect packets belonging to the next logical response bundle (reassembling if fragmented).
        let mut buf = vec![0u8; 4096];
        let table = client_interface();
        let mut collected_msgs = Vec::new();
        self.sock
            .set_read_timeout(Some(Duration::from_secs(3)))
            .unwrap();
        loop {
            match self.sock.recv(&mut buf) {
                Ok(n) => {
                    let dec = self.key.packet_decrypt(&buf[..n]).expect("decrypt BaseApp reply");
                    let rpkt = Packet::decode(&dec, Endian::Little).expect("decode BaseApp packet");
                    assert_eq!(rpkt.flags & FLAG_CREATE_CHANNEL, 0);
                    let rx = self.channel.receive(rpkt);
                    for assembled in rx.packets {
                        let msgs = decode_messages(&assembled, &table, Endian::Little)
                            .expect("decode ClientInterface bundle");
                        collected_msgs.extend(msgs);
                    }
                    // Short timeout to drain any immediate follow-up datagrams before returning.
                    self.sock
                        .set_read_timeout(Some(Duration::from_millis(80)))
                        .unwrap();
                }
                Err(_) if !collected_msgs.is_empty() => break,
                Err(e) => panic!("timed out waiting for BaseApp reply: {e}"),
            }
        }
        // Send an ACK so BaseApp stops retransmitting what we just received.
        let ack = self.channel.send_ack_only(&self.key);
        let _ = self.sock.send(&ack);
        collected_msgs
    }

    fn try_collect_replies(&mut self, timeout: Duration) -> Vec<bw201::bundle::DecodedMessage> {
        let mut buf = vec![0u8; 4096];
        let table = client_interface();
        let mut collected_msgs = Vec::new();
        self.sock.set_read_timeout(Some(timeout)).unwrap();
        loop {
            match self.sock.recv(&mut buf) {
                Ok(n) => {
                    let dec = match self.key.packet_decrypt(&buf[..n]) {
                        Ok(d) => d,
                        Err(_) => continue,
                    };
                    let rpkt = match Packet::decode(&dec, Endian::Little) {
                        Ok(p) => p,
                        Err(_) => continue,
                    };
                    let rx = self.channel.receive(rpkt);
                    for assembled in rx.packets {
                        if let Ok(msgs) = decode_messages(&assembled, &table, Endian::Little) {
                            collected_msgs.extend(msgs);
                        }
                    }
                    self.sock
                        .set_read_timeout(Some(Duration::from_millis(80)))
                        .unwrap();
                }
                Err(_) => break,
            }
        }
        if !collected_msgs.is_empty() {
            let ack = self.channel.send_ack_only(&self.key);
            let _ = self.sock.send(&ack);
        }
        collected_msgs
    }

    fn send_and_try_collect_replies(
        &mut self,
        enc: ElementEncoder,
        timeout: Duration,
    ) -> Vec<bw201::bundle::DecodedMessage> {
        let mut pkt = enc.finish();
        let seq = self.next_out_seq();
        pkt.config.sequence = Some(seq);
        pkt.config.cumulative_ack = Some(self.channel.in_seq_at());
        pkt.config.extra_flags |= FLAG_ON_CHANNEL | FLAG_IS_RELIABLE;
        pkt.finalize_flags();
        let plain = pkt.encode(Endian::Little);
        let wire = self.key.packet_encrypt(&plain);
        let _ = self.sock.send(&wire);
        self.try_collect_replies(timeout)
    }
}

fn test_login_key(game_dir: &std::path::Path) -> (LoginKey, RsaPublicKey) {
    let pub_pem = game_dir.join("res/loginapp_wot.pubkey");
    let priv_pem = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../.scratch/wot-keys/loginapp_priv.pem");
    if priv_pem.exists() && pub_pem.exists() {
        let priv_s = std::fs::read_to_string(&priv_pem).unwrap();
        let pub_s = std::fs::read_to_string(&pub_pem).unwrap();
        (
            LoginKey::from_pkcs8_pem(&priv_s).unwrap(),
            RsaPublicKey::from_public_key_pem(&pub_s).unwrap(),
        )
    } else {
        let key = LoginKey::generate(2048).unwrap();
        let pk = key.public_key().clone();
        (key, pk)
    }
}

fn login_and_connect(
    login_addr: SocketAddr,
    expected_base_port: u16,
    pubkey: &RsaPublicKey,
    username: &[u8],
    blowfish_bytes: &[u8],
) -> (SimClient, u32, i32) {
    let login_sock = UdpSocket::bind("127.0.0.1:0").unwrap();
    login_sock
        .set_read_timeout(Some(Duration::from_secs(8)))
        .unwrap();
    let req = encrypt_login_request(pubkey, username, b"test123", blowfish_bytes, 77);
    login_sock.send_to(&req, login_addr).unwrap();

    let mut lbuf = [0u8; 2048];
    let (n, _) = login_sock.recv_from(&mut lbuf).unwrap();
    let lpkt = Packet::decode(&lbuf[..n], Endian::Little).unwrap();
    let lmsgs = decode_messages(&lpkt, &login_interface(), Endian::Little).unwrap();
    assert_eq!(lmsgs[0].id, REPLY_MESSAGE_IDENTIFIER);
    let mut lr = BinaryReader::new(&lmsgs[0].data);
    assert_eq!(lr.u32().unwrap(), 77);
    assert_eq!(lr.u8().unwrap(), 1); // LOGGED_ON

    let bf = BlowfishKey::new(blowfish_bytes).unwrap();
    let reply_plain = bf.stream_decrypt(lr.rest()).unwrap();
    let ip = Ipv4Addr::new(
        reply_plain[0],
        reply_plain[1],
        reply_plain[2],
        reply_plain[3],
    );
    let port = u16::from_be_bytes([reply_plain[4], reply_plain[5]]);
    let session_key = u32::from_le_bytes([
        reply_plain[8],
        reply_plain[9],
        reply_plain[10],
        reply_plain[11],
    ]);
    assert_eq!(port, expected_base_port);

    let base_addr = SocketAddr::V4(SocketAddrV4::new(ip, port));
    let mut client = SimClient::connect(base_addr, bf);

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
    let msgs = client.send_off_channel_plaintext_and_collect_replies(enc);
    assert_eq!(msgs[0].id, REPLY_MESSAGE_IDENTIFIER);
    assert_eq!(msgs[1].id, client_ids::AUTHENTICATE);

    let mut enc = ElementEncoder::new();
    enc.fixed_message(
        baseapp_ext_ids::AUTHENTICATE,
        4,
        &session_key.to_le_bytes(),
    );
    enc.fixed_message(baseapp_ext_ids::ENABLE_ENTITIES, 1, &[0]);
    let msgs = client.send_and_collect_replies(enc);
    assert_eq!(msgs[0].id, client_ids::BANDWIDTH_NOTIFICATION);
    assert_eq!(msgs[1].id, client_ids::UPDATE_FREQUENCY_NOTIFICATION);
    assert_eq!(msgs[2].id, client_ids::SET_GAME_TIME);
    assert_eq!(msgs[3].id, client_ids::CREATE_BASE_PLAYER);
    let mut pr = BinaryReader::new(&msgs[3].data);
    let player_id = pr.i32().unwrap();
    assert_eq!(pr.u16().unwrap(), ACCOUNT_ENTITY_TYPE_ID);
    assert_eq!(msgs[4].id, client_method::SHOW_GUI);

    (client, session_key, player_id)
}

fn verify_full_login_and_hangar_flow(
    login_addr: SocketAddr,
    expected_base_port: u16,
    pubkey: &RsaPublicKey,
    blowfish_bytes: &[u8],
    expected_vehicles: usize,
) {
    // 1. LoginApp UDP hop
    let login_sock = UdpSocket::bind("127.0.0.1:0").unwrap();
    login_sock
        .set_read_timeout(Some(Duration::from_secs(8)))
        .unwrap();
    let req = encrypt_login_request(pubkey, b"test@test.com", b"test123", blowfish_bytes, 77);
    login_sock.send_to(&req, login_addr).unwrap();

    let mut lbuf = [0u8; 2048];
    let (n, _) = login_sock.recv_from(&mut lbuf).unwrap();
    let lpkt = Packet::decode(&lbuf[..n], Endian::Little).unwrap();
    let lmsgs = decode_messages(&lpkt, &login_interface(), Endian::Little).unwrap();
    assert_eq!(lmsgs.len(), 1);
    assert_eq!(lmsgs[0].id, REPLY_MESSAGE_IDENTIFIER);
    let mut lr = BinaryReader::new(&lmsgs[0].data);
    assert_eq!(lr.u32().unwrap(), 77);
    assert_eq!(lr.u8().unwrap(), 1); // LOGGED_ON

    let bf = BlowfishKey::new(blowfish_bytes).unwrap();
    let reply_plain = bf.stream_decrypt(lr.rest()).unwrap();
    let ip = Ipv4Addr::new(
        reply_plain[0],
        reply_plain[1],
        reply_plain[2],
        reply_plain[3],
    );
    let port = u16::from_be_bytes([reply_plain[4], reply_plain[5]]);
    let session_key = u32::from_le_bytes([
        reply_plain[8],
        reply_plain[9],
        reply_plain[10],
        reply_plain[11],
    ]);
    assert_eq!(port, expected_base_port);

    // 2. BaseApp hop: retail-exact 21-byte plaintext off-channel baseAppLogin
    let base_addr = SocketAddr::V4(SocketAddrV4::new(ip, port));
    let mut client = SimClient::connect(base_addr, bf);

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
    let msgs = client.send_off_channel_plaintext_and_collect_replies(enc);
    assert_eq!(msgs[0].id, REPLY_MESSAGE_IDENTIFIER);
    assert_eq!(msgs[1].id, client_ids::AUTHENTICATE);

    // 3. enableEntities -> setup notifications + createBasePlayer(typeID=0) + showGUI
    let mut enc = ElementEncoder::new();
    enc.fixed_message(
        baseapp_ext_ids::AUTHENTICATE,
        4,
        &session_key.to_le_bytes(),
    );
    enc.fixed_message(baseapp_ext_ids::ENABLE_ENTITIES, 1, &[0]);
    let msgs = client.send_and_collect_replies(enc);
    assert_eq!(msgs[0].id, client_ids::BANDWIDTH_NOTIFICATION);
    assert_eq!(msgs[1].id, client_ids::UPDATE_FREQUENCY_NOTIFICATION);
    assert_eq!(msgs[2].id, client_ids::SET_GAME_TIME);
    assert_eq!(msgs[3].id, client_ids::CREATE_BASE_PLAYER);
    let mut pr = BinaryReader::new(&msgs[3].data);
    let player_id = pr.i32().unwrap();
    assert_eq!(pr.u16().unwrap(), ACCOUNT_ENTITY_TYPE_ID);
    assert_eq!(pr.string().unwrap(), "test@test.com");
    assert_eq!(pr.string().unwrap(), "test@test.com");
    let settings = pickle::loads(&pr.string_bytes().unwrap()).unwrap();
    assert_eq!(
        settings.get("vivoxDomain").and_then(|v| v.as_bytes()),
        Some(&b""[..])
    );
    assert_eq!(msgs[4].id, client_method::SHOW_GUI);

    // 4. CMD_SYNC_DATA phase 1 (rev=0 -> RES_STREAM + resourceHeader(rid=120) + fragments)
    let mut w = BinaryWriter::new();
    w.i16(120);
    w.i16(cmd::CMD_SYNC_DATA);
    w.i32(0);
    w.i32(0);
    w.i32(0);
    let mut enc = ElementEncoder::new();
    enc.message(
        base_method::DO_CMD_INT3,
        LengthStyle::Variable(2),
        w.as_slice(),
    );
    let msgs = client.send_and_collect_replies(enc);
    assert_eq!(msgs[0].id, client_method::ON_CMD_RESPONSE);
    assert_eq!(msgs[1].id, client_ids::RESOURCE_HEADER);
    let hdr_rid = BinaryReader::new(&msgs[1].data).u16().unwrap();
    assert_eq!(hdr_rid, 120);
    let mut compressed = Vec::new();
    for m in &msgs[2..] {
        assert_eq!(m.id, client_ids::RESOURCE_FRAGMENT);
        let mut rf = BinaryReader::new(&m.data);
        assert_eq!(rf.u16().unwrap(), 120);
        let _seq = rf.u8().unwrap();
        let _flags = rf.u8().unwrap();
        compressed.extend_from_slice(rf.rest());
    }
    let sync = pickle::loads(&pickle::zlib_decompress(&compressed).unwrap()).unwrap();
    let inv = sync.get("inventory").unwrap();
    for kind in 1..=11 {
        assert!(
            matches!(inv.get_int(kind), Some(Value::Dict(_))),
            "missing inventory[{kind}] dict in streamed sync"
        );
    }
    let veh = inv.get_int(1).unwrap();
    let comp_descr = veh.get("compDescr").unwrap();
    let actual_vehicles = match comp_descr {
        Value::Dict(items) => items.len(),
        _ => 0,
    };
    assert_eq!(actual_vehicles, expected_vehicles);
    assert_eq!(
        veh.get("crew").unwrap().as_dict().unwrap().len(),
        expected_vehicles
    );
    assert_eq!(
        veh.get("shells").unwrap().as_dict().unwrap().len(),
        expected_vehicles
    );
    assert!(
        inv.get_int(8)
            .unwrap()
            .get("compDescr")
            .unwrap()
            .as_dict()
            .unwrap()
            .len()
            >= expected_vehicles
    );

    let sync_rev = sync.get("rev").and_then(|v| v.as_int()).unwrap_or(1);

    // 5. CMD_SYNC_DATA phase 2 (same rev -> onCmdResponseExt {'rev': sync_rev, 'prevRev': sync_rev})
    let mut w2 = BinaryWriter::new();
    w2.i16(121);
    w2.i16(cmd::CMD_SYNC_DATA);
    w2.i32((sync_rev >> 32) as i32);
    w2.i32((sync_rev & 0xFFFFFFFF) as i32);
    w2.i32(0);
    let mut enc2 = ElementEncoder::new();
    enc2.message(
        base_method::DO_CMD_INT3,
        LengthStyle::Variable(2),
        w2.as_slice(),
    );
    let msgs2 = client.send_and_collect_replies(enc2);
    assert_eq!(msgs2[0].id, client_method::ON_CMD_RESPONSE_EXT);
    let mut r2 = BinaryReader::new(&msgs2[0].data);
    assert_eq!(r2.i32().unwrap(), player_id);
    assert_eq!(r2.i16().unwrap(), 121);
    assert_eq!(r2.i16().unwrap(), cmd::RES_CACHE);
    let delta = pickle::loads(&r2.string_bytes().unwrap()).unwrap();
    assert_eq!(delta.get("rev").and_then(|v| v.as_int()), Some(sync_rev));
    assert_eq!(delta.get("prevRev").and_then(|v| v.as_int()), Some(sync_rev));

    // 6. CMD_SYNC_SHOP, CMD_SYNC_DOSSIERS, CMD_REQ_SERVER_STATS, CMD_SET_LANGUAGE
    let mut enc3 = ElementEncoder::new();
    for (req_id, command_id) in [
        (122, cmd::CMD_SYNC_SHOP),
        (123, cmd::CMD_SYNC_DOSSIERS),
        (cmd::REQUEST_ID_NO_RESPONSE, cmd::CMD_REQ_SERVER_STATS),
    ] {
        let mut bw = BinaryWriter::new();
        bw.i16(req_id);
        bw.i16(command_id);
        bw.i32(0);
        bw.i32(0);
        bw.i32(0);
        enc3.message(
            base_method::DO_CMD_INT3,
            LengthStyle::Variable(2),
            bw.as_slice(),
        );
    }
    let mut lw = BinaryWriter::new();
    lw.i16(124);
    lw.i16(cmd::CMD_SET_LANGUAGE);
    lw.string("uk");
    enc3.message(
        base_method::DO_CMD_STR,
        LengthStyle::Variable(2),
        lw.as_slice(),
    );
    let msgs3 = client.send_and_collect_replies(enc3);
    assert!(msgs3
        .iter()
        .any(|m| m.id == client_method::RECEIVE_SERVER_STATS));
    let shop_and_dossier_rids: Vec<u16> = msgs3
        .iter()
        .filter(|m| m.id == client_ids::RESOURCE_HEADER)
        .map(|m| BinaryReader::new(&m.data).u16().unwrap())
        .collect();
    assert_eq!(shop_and_dossier_rids, vec![122, 123]);

    let mut shop_compressed = Vec::new();
    for m in &msgs3 {
        if m.id == client_ids::RESOURCE_FRAGMENT {
            let mut rf = BinaryReader::new(&m.data);
            if rf.u16().unwrap() == 122 {
                let _seq = rf.u8().unwrap();
                let _flags = rf.u8().unwrap();
                shop_compressed.extend_from_slice(rf.rest());
            }
        }
    }
    let shop = pickle::loads(&pickle::zlib_decompress(&shop_compressed).unwrap()).unwrap();
    let shop_items = shop.get("items").unwrap();
    for nid in [0i64, 1, 2, 15] {
        assert!(
            shop_items.get_int(nid).is_some(),
            "streamed shop['items'] missing nation {nid}"
        );
    }
    assert!(
        !shop_items
            .get_int(0)
            .unwrap()
            .get_int(10)
            .unwrap()
            .as_list()
            .unwrap()[0]
            .as_dict()
            .unwrap()
            .is_empty(),
        "streamed shop['items'][0][10] (ussr shells) must be populated"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn e2e_udp_login_handoff_baseapp_130_vehicles_and_relogin() {
    let game_dir = PathBuf::from(r"C:\Users\qwerty\Desktop\World_of_Tanks");
    if !game_dir.join("res").exists() {
        return;
    }

    let tmp_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target/test-tmp-e2e");
    std::fs::create_dir_all(&tmp_dir).unwrap();
    let session_file = tmp_dir.join("session.json");
    let _ = std::fs::remove_file(&session_file);

    let (ready_tx, ready_rx) = tokio::sync::oneshot::channel();
    let data_dir = tmp_dir.join("accounts");
    let _ = std::fs::remove_dir_all(&data_dir);
    let base_task = tokio::spawn(wot_baseapp::serve_with_ready(
        BaseAppConfig {
            bind: "127.0.0.1:0".parse().unwrap(),
            session_file: session_file.clone(),
            game_dir: game_dir.clone(),
            data_dir,
            preset: wot065_data::account_state::Preset::Retail,
            economics_config: None,
            overrides: PayloadOverrides::default(),
        },
        Some(ready_tx),
    ));
    let base_bound = ready_rx.await.expect("wot-baseapp failed to bind");
    let base_addr = match base_bound {
        SocketAddr::V4(v4) => v4,
        _ => unreachable!(),
    };

    let (login_key, pubkey) = test_login_key(&game_dir);

    let mut login_app = LoginApp::bind(
        LoginAppConfig {
            bind: "127.0.0.1:0".parse().unwrap(),
            base_app: base_addr,
            session_key: 0x0BAD_F00D,
            session_out: Some(session_file.clone()),
        },
        login_key,
    )
    .unwrap();
    let login_addr = login_app.local_addr().unwrap();
    let stop = Arc::new(AtomicBool::new(false));
    let stop_login = stop.clone();
    let login_thread = std::thread::spawn(move || {
        let _ = login_app.run(stop_login);
    });

    tokio::task::spawn_blocking(move || {
        // Session 1
        let key1 = b"first_blowfish_k";
        verify_full_login_and_hangar_flow(login_addr, base_addr.port(), &pubkey, key1, 3);

        // Session 2 (new Blowfish key -> tests dynamic session reload without restarting BaseApp)
        let key2 = b"second_blowfish!";
        verify_full_login_and_hangar_flow(login_addr, base_addr.port(), &pubkey, key2, 3);
    })
    .await
    .expect("blocking client flow");

    stop.store(true, Ordering::Relaxed);
    // Poke LoginApp so recv_from timeout isn't waited needlessly.
    let poke = UdpSocket::bind("127.0.0.1:0").unwrap();
    let _ = poke.send_to(b"\x00", login_addr);
    let _ = login_thread.join();
    base_task.abort();
    let _ = std::fs::remove_file(&session_file);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn duplicate_login_kicks_old_owner_without_bounce() {
    let game_dir = PathBuf::from(r"C:\Users\qwerty\Desktop\World_of_Tanks");
    if !game_dir.join("res").exists() {
        return;
    }

    let tmp_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target/test-tmp-kick");
    std::fs::create_dir_all(&tmp_dir).unwrap();
    let session_file = tmp_dir.join("session.json");
    let _ = std::fs::remove_file(&session_file);

    let (ready_tx, ready_rx) = tokio::sync::oneshot::channel();
    let data_dir = tmp_dir.join("accounts");
    let _ = std::fs::remove_dir_all(&data_dir);
    let base_task = tokio::spawn(wot_baseapp::serve_with_ready(
        BaseAppConfig {
            bind: "127.0.0.1:0".parse().unwrap(),
            session_file: session_file.clone(),
            game_dir: game_dir.clone(),
            data_dir,
            preset: wot065_data::account_state::Preset::Retail,
            economics_config: None,
            overrides: PayloadOverrides::default(),
        },
        Some(ready_tx),
    ));
    let base_bound = ready_rx.await.expect("wot-baseapp failed to bind");
    let base_addr = match base_bound {
        SocketAddr::V4(v4) => v4,
        _ => unreachable!(),
    };

    let (key, pk) = test_login_key(&game_dir);
    let mut login_app = LoginApp::bind(
        LoginAppConfig {
            bind: "127.0.0.1:0".parse().unwrap(),
            base_app: base_addr,
            session_key: 0x0BAD_F00D,
            session_out: Some(session_file.clone()),
        },
        key,
    )
    .unwrap();
    let login_addr = login_app.local_addr().unwrap();
    let stop = Arc::new(AtomicBool::new(false));
    let stop_login = stop.clone();
    let login_thread = std::thread::spawn(move || {
        let _ = login_app.run(stop_login);
    });

    tokio::task::spawn_blocking(move || {
        let username = b"shared_user@test.com";

        // 1. Client A logs in
        let key_a = b"client_a_blowfis";
        let (mut client_a, _session_a, _player_a) =
            login_and_connect(login_addr, base_addr.port(), &pk, username, key_a);

        // 2. Client B logs in with SAME account
        let key_b = b"client_b_blowfis";
        let (mut client_b, _session_b, player_b) =
            login_and_connect(login_addr, base_addr.port(), &pk, username, key_b);

        // 3. Client A should receive ON_KICKED_FROM_SERVER
        let kick_msgs = client_a.try_collect_replies(Duration::from_secs(3));
        assert!(
            !kick_msgs.is_empty(),
            "expected kick packet on old client socket"
        );
        let kick_msg = kick_msgs
            .iter()
            .find(|m| m.id == client_method::ON_KICKED_FROM_SERVER)
            .expect("must contain ON_KICKED_FROM_SERVER (0x84)");
        let mut kr = BinaryReader::new(&kick_msg.data);
        let _kicked_player_id = kr.i32().unwrap();
        assert_eq!(kr.string().unwrap(), "Another session logged in");
        assert_eq!(kr.u8().unwrap(), 0);
        assert_eq!(kr.u32().unwrap(), 0);

        // 4. Trailing datagram from kicked Client A must NOT bounce-kick Client B
        let mut enc_a = ElementEncoder::new();
        let mut sw = BinaryWriter::new();
        sw.i16(125);
        sw.i16(cmd::CMD_SET_LANGUAGE);
        sw.string("de");
        enc_a.message(base_method::DO_CMD_STR, LengthStyle::Variable(2), sw.as_slice());
        let msgs_a = client_a.send_and_try_collect_replies(enc_a, Duration::from_millis(400));
        assert!(
            msgs_a.is_empty(),
            "kicked client must not receive successful response"
        );

        // 5. Client B executes commands normally
        let mut enc_b = ElementEncoder::new();
        let mut sw_b = BinaryWriter::new();
        sw_b.i16(126);
        sw_b.i16(cmd::CMD_SET_LANGUAGE);
        sw_b.string("uk");
        enc_b.message(base_method::DO_CMD_STR, LengthStyle::Variable(2), sw_b.as_slice());
        let msgs_b = client_b.send_and_collect_replies(enc_b);
        assert!(
            msgs_b.iter().any(|m| m.id == client_method::UPDATE),
            "new client should receive UPDATE"
        );
        let resp_b = msgs_b
            .iter()
            .find(|m| m.id == client_method::ON_CMD_RESPONSE)
            .expect("new client receives ON_CMD_RESPONSE");
        let mut br = BinaryReader::new(&resp_b.data);
        assert_eq!(br.i32().unwrap(), player_b);
        assert_eq!(br.i16().unwrap(), 126);
        assert_eq!(br.i16().unwrap(), cmd::RES_SUCCESS);
    })
    .await
    .expect("blocking duplicate login test");

    stop.store(true, Ordering::Relaxed);
    let poke = UdpSocket::bind("127.0.0.1:0").unwrap();
    let _ = poke.send_to(b"\x00", login_addr);
    let _ = login_thread.join();
    base_task.abort();
    let _ = std::fs::remove_file(&session_file);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn full_hangar_persistence_and_commands_roundtrip() {
    let game_dir = PathBuf::from(r"C:\Users\qwerty\Desktop\World_of_Tanks");
    if !game_dir.join("res").exists() {
        return;
    }

    let catalog = wot065_data::catalog::load(&game_dir).unwrap();
    let tmp_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target/test-tmp-roundtrip");
    std::fs::create_dir_all(&tmp_dir).unwrap();
    let session_file = tmp_dir.join("session.json");
    let _ = std::fs::remove_file(&session_file);

    let (ready_tx, ready_rx) = tokio::sync::oneshot::channel();
    let data_dir = tmp_dir.join("accounts");
    let _ = std::fs::remove_dir_all(&data_dir);
    let base_task = tokio::spawn(wot_baseapp::serve_with_ready(
        BaseAppConfig {
            bind: "127.0.0.1:0".parse().unwrap(),
            session_file: session_file.clone(),
            game_dir: game_dir.clone(),
            data_dir: data_dir.clone(),
            preset: wot065_data::account_state::Preset::Retail,
            economics_config: None,
            overrides: PayloadOverrides::default(),
        },
        Some(ready_tx),
    ));
    let base_bound = ready_rx.await.expect("wot-baseapp failed to bind");
    let base_addr = match base_bound {
        SocketAddr::V4(v4) => v4,
        _ => unreachable!(),
    };

    let (key, pk) = test_login_key(&game_dir);
    let mut login_app = LoginApp::bind(
        LoginAppConfig {
            bind: "127.0.0.1:0".parse().unwrap(),
            base_app: base_addr,
            session_key: 0x0BAD_F00D,
            session_out: Some(session_file.clone()),
        },
        key,
    )
    .unwrap();
    let login_addr = login_app.local_addr().unwrap();
    let stop = Arc::new(AtomicBool::new(false));
    let stop_login = stop.clone();
    let login_thread = std::thread::spawn(move || {
        let _ = login_app.run(stop_login);
    });

    let cat_arc = Arc::new(catalog);
    let cat = cat_arc.clone();

    tokio::task::spawn_blocking(move || {
        let username = b"commander_hangar@test.com";
        let bf_key = b"hangar_blowfish!";
        let (mut client, _session_key, player_id) =
            login_and_connect(login_addr, base_addr.port(), &pk, username, bf_key);

        // 1. Initial CMD_SYNC_DATA (rev=0)
        let mut w = BinaryWriter::new();
        w.i16(120);
        w.i16(cmd::CMD_SYNC_DATA);
        w.i32(0);
        w.i32(0);
        w.i32(0);
        let mut enc = ElementEncoder::new();
        enc.message(base_method::DO_CMD_INT3, LengthStyle::Variable(2), w.as_slice());
        let msgs = client.send_and_collect_replies(enc);
        assert_eq!(msgs[0].id, client_method::ON_CMD_RESPONSE);
        assert_eq!(msgs[1].id, client_ids::RESOURCE_HEADER);
        let mut compressed = Vec::new();
        for m in &msgs[2..] {
            assert_eq!(m.id, client_ids::RESOURCE_FRAGMENT);
            let mut rf = BinaryReader::new(&m.data);
            let _rid = rf.u16().unwrap();
            let _seq = rf.u8().unwrap();
            let _flags = rf.u8().unwrap();
            compressed.extend_from_slice(rf.rest());
        }
        let sync = pickle::loads(&pickle::zlib_decompress(&compressed).unwrap()).unwrap();
        let initial_slots = sync.get("stats").unwrap().get("slots").unwrap().as_int().unwrap();

        // 1b. Initial CMD_SYNC_DOSSIERS (version=0, max_change_time=0) -> record baseline change_time
        let mut w = BinaryWriter::new();
        w.i16(1200);
        w.i16(cmd::CMD_SYNC_DOSSIERS);
        w.i32(0);
        w.i32(0);
        w.i32(0);
        let mut enc = ElementEncoder::new();
        enc.message(base_method::DO_CMD_INT3, LengthStyle::Variable(2), w.as_slice());
        let msgs = client.send_and_collect_replies(enc);
        assert_eq!(msgs[0].id, client_method::ON_CMD_RESPONSE);
        assert_eq!(msgs[1].id, client_ids::RESOURCE_HEADER);
        let mut dossier_compressed = Vec::new();
        for m in &msgs[2..] {
            assert_eq!(m.id, client_ids::RESOURCE_FRAGMENT);
            let mut rf = BinaryReader::new(&m.data);
            let _rid = rf.u16().unwrap();
            let _seq = rf.u8().unwrap();
            let _flags = rf.u8().unwrap();
            dossier_compressed.extend_from_slice(rf.rest());
        }
        let dossier_tuple =
            pickle::loads(&pickle::zlib_decompress(&dossier_compressed).unwrap()).unwrap();
        let dossier_list = dossier_tuple.as_list().unwrap();
        let dossier_version = dossier_list[0].as_int().unwrap() as i32;
        let initial_max_change_time = dossier_list[1]
            .as_list()
            .unwrap()
            .chunks_exact(4)
            .filter_map(|chunk| chunk[2].as_int())
            .max()
            .expect("initial dossier records must include change_time")
            as i32;
        assert!(initial_max_change_time > 0);
        let ms1_shells = sync
            .get("inventory")
            .unwrap()
            .get_int(1)
            .unwrap()
            .get("shells")
            .unwrap()
            .as_dict()
            .unwrap();
        let ms1_shell_pairs = ms1_shells
            .iter()
            .find(|(k, _)| k.as_int() == Some(1))
            .and_then(|(_, v)| v.as_list())
            .expect("MS-1 shells list");
        let stock_shell_cd = ms1_shell_pairs[0].as_int().expect("shell cd");

        // 2. Money: CMD_SET_MONEY (credits = 10_000_000, gold = 50_000)
        let mut w = BinaryWriter::new();
        w.i16(121);
        w.i16(cmd::CMD_SET_MONEY);
        w.i32(10_000_000);
        w.i32(50_000);
        w.i32(0);
        let mut enc = ElementEncoder::new();
        enc.message(base_method::DO_CMD_INT3, LengthStyle::Variable(2), w.as_slice());
        let msgs = client.send_and_collect_replies(enc);
        assert_eq!(msgs[0].id, client_method::UPDATE);
        assert_eq!(msgs[1].id, client_method::ON_CMD_RESPONSE);
        let mut r = BinaryReader::new(&msgs[1].data);
        assert_eq!(r.i32().unwrap(), player_id);
        assert_eq!(r.i16().unwrap(), 121);
        assert_eq!(r.i16().unwrap(), cmd::RES_SUCCESS);

        // 2b. Stale shop_rev negative check -> ON_CMD_RESPONSE_EXT with RES_SHOP_DESYNC (-4)
        let mut w = BinaryWriter::new();
        w.i16(200);
        w.i16(cmd::CMD_BUY_SLOT);
        w.i32(999); // stale shop_rev
        w.i32(0);
        w.i32(0);
        let mut enc = ElementEncoder::new();
        enc.message(base_method::DO_CMD_INT3, LengthStyle::Variable(2), w.as_slice());
        let msgs = client.send_and_collect_replies(enc);
        assert_eq!(msgs[0].id, client_method::ON_CMD_RESPONSE_EXT);
        let mut r = BinaryReader::new(&msgs[0].data);
        assert_eq!(r.i32().unwrap(), player_id);
        assert_eq!(r.i16().unwrap(), 200);
        assert_eq!(r.i16().unwrap(), cmd::RES_SHOP_DESYNC);
        let ext = pickle::loads(&r.string_bytes().unwrap()).unwrap();
        assert_eq!(ext.get("shopRev").and_then(|v| v.as_int()), Some(1));

        // 3. Slot: CMD_BUY_SLOT
        let mut w = BinaryWriter::new();
        w.i16(122);
        w.i16(cmd::CMD_BUY_SLOT);
        w.i32(1); // shop_rev
        w.i32(0);
        w.i32(0);
        let mut enc = ElementEncoder::new();
        enc.message(base_method::DO_CMD_INT3, LengthStyle::Variable(2), w.as_slice());
        let msgs = client.send_and_collect_replies(enc);
        assert_eq!(msgs[0].id, client_method::UPDATE);
        assert_eq!(msgs[1].id, client_method::ON_CMD_RESPONSE);
        let mut r = BinaryReader::new(&msgs[1].data);
        assert_eq!(r.i32().unwrap(), player_id);
        assert_eq!(r.i16().unwrap(), 122);
        assert_eq!(r.i16().unwrap(), cmd::RES_SUCCESS);

        // 3b. Add XP and unlock single node on MS-1 via CMD_ADD_XP + CMD_UNLOCK
        let ms1_def = cat.vehicle_def_by_id(0, 13).expect("MS-1 in catalog");
        let mut w = BinaryWriter::new();
        w.i16(201);
        w.i16(cmd::CMD_ADD_XP);
        w.i32(ms1_def.veh_type_comp_descr);
        w.i32(50_000);
        w.i32(0);
        let mut enc = ElementEncoder::new();
        enc.message(base_method::DO_CMD_INT3, LengthStyle::Variable(2), w.as_slice());
        let msgs = client.send_and_collect_replies(enc);
        assert_eq!(msgs[0].id, client_method::UPDATE);
        assert_eq!(msgs[1].id, client_method::ON_CMD_RESPONSE);
        let mut r = BinaryReader::new(&msgs[1].data);
        assert_eq!(r.i32().unwrap(), player_id);
        assert_eq!(r.i16().unwrap(), 201);
        assert_eq!(r.i16().unwrap(), cmd::RES_SUCCESS);

        let mut w = BinaryWriter::new();
        w.i16(202);
        w.i16(cmd::CMD_UNLOCK);
        w.i32(ms1_def.veh_type_comp_descr);
        w.i32(0);
        w.i32(0);
        let mut enc = ElementEncoder::new();
        enc.message(base_method::DO_CMD_INT3, LengthStyle::Variable(2), w.as_slice());
        let msgs = client.send_and_collect_replies(enc);
        assert_eq!(msgs[0].id, client_method::UPDATE);
        assert_eq!(msgs[1].id, client_method::ON_CMD_RESPONSE);
        let mut r = BinaryReader::new(&msgs[1].data);
        assert_eq!(r.i32().unwrap(), player_id);
        assert_eq!(r.i16().unwrap(), 202);
        assert_eq!(r.i16().unwrap(), cmd::RES_SUCCESS);

        // 4. Unlock: CMD_UNLOCK_ALL
        let mut w = BinaryWriter::new();
        w.i16(123);
        w.i16(cmd::CMD_UNLOCK_ALL);
        w.i32(0);
        w.i32(0);
        w.i32(0);
        let mut enc = ElementEncoder::new();
        enc.message(base_method::DO_CMD_INT3, LengthStyle::Variable(2), w.as_slice());
        let msgs = client.send_and_collect_replies(enc);
        assert_eq!(msgs[0].id, client_method::UPDATE);
        assert_eq!(msgs[1].id, client_method::RESYNC_DOSSIERS);
        assert_eq!(msgs[2].id, client_method::ON_CMD_RESPONSE);
        let mut r = BinaryReader::new(&msgs[2].data);
        assert_eq!(r.i32().unwrap(), player_id);
        assert_eq!(r.i16().unwrap(), 123);
        assert_eq!(r.i16().unwrap(), cmd::RES_SUCCESS);

        // 5. Buy vehicle: BT-2
        let bt2 = cat
            .vehicle_defs
            .values()
            .find(|v| v.nation_id == 0 && v.name.eq_ignore_ascii_case("BT-2"))
            .expect("BT-2 in catalog");
        let bt2_cd = bt2.veh_type_comp_descr;

        let mut w = BinaryWriter::new();
        w.i16(124);
        w.i16(cmd::CMD_BUY_VEHICLE);
        w.i32(1); // shop_rev
        w.i32(bt2_cd);
        w.i32(1); // BUY_VEHICLE_FLAG_CREW
        w.i32(0); // tman_cost_idx (50%)
        let mut enc = ElementEncoder::new();
        enc.message(base_method::DO_CMD_INT4, LengthStyle::Variable(2), w.as_slice());
        let msgs = client.send_and_collect_replies(enc);
        assert_eq!(msgs[0].id, client_method::UPDATE);
        assert_eq!(msgs[1].id, client_method::RESYNC_DOSSIERS);
        assert_eq!(msgs[2].id, client_method::ON_CMD_RESPONSE);
        let mut r = BinaryReader::new(&msgs[2].data);
        assert_eq!(r.i32().unwrap(), player_id);
        assert_eq!(r.i16().unwrap(), 124);
        assert_eq!(r.i16().unwrap(), cmd::RES_SUCCESS);

        // Parse UPDATE diff to get BT-2 inventory ID
        let mut ur = BinaryReader::new(&msgs[0].data);
        assert_eq!(ur.i32().unwrap(), player_id);
        let diff_str = ur.string_bytes().unwrap();
        let diff = pickle::loads(&diff_str).unwrap();
        let veh_dict = diff.get("inventory").unwrap().get_int(1).unwrap().get("compDescr").unwrap();
        let mut bt2_inv_id = None;
        if let Value::Dict(map) = veh_dict {
            for (k, _) in map {
                if let Some(id) = k.as_int() {
                    if id != 1 && id != 2 && id != 3 {
                        bt2_inv_id = Some(id);
                    }
                }
            }
        }
        let bt2_inv_id = bt2_inv_id.expect("new BT-2 inv id");

        // 6. Buy and mount items / optdev
        let toolbox = cat
            .optional_devices
            .values()
            .find(|d| d.name == "toolbox")
            .expect("toolbox");
        let non_removable = cat
            .optional_devices
            .values()
            .find(|d| !d.removable && d.weight == 0.0)
            .expect("non-removable opt dev");

        // Buy toolbox
        let mut w = BinaryWriter::new();
        w.i16(125);
        w.i16(cmd::CMD_BUY_ITEM);
        w.i32(1); // shop_rev
        w.i32(toolbox.comp_descr);
        w.i32(1); // count
        let mut enc = ElementEncoder::new();
        enc.message(base_method::DO_CMD_INT3, LengthStyle::Variable(2), w.as_slice());
        let msgs = client.send_and_collect_replies(enc);
        assert_eq!(msgs[0].id, client_method::UPDATE);
        assert_eq!(msgs[1].id, client_method::ON_CMD_RESPONSE);

        // Mount toolbox to MS-1 (inv_id=1) slot 0
        let mut w = BinaryWriter::new();
        w.i16(126);
        w.i16(cmd::CMD_EQUIP_OPTDEV);
        w.i32(1); // veh_inv_id = 1 (MS-1)
        w.i32(toolbox.comp_descr);
        w.i32(0); // slot 0
        w.i32(0); // is_paid_removal = 0
        let mut enc = ElementEncoder::new();
        enc.message(base_method::DO_CMD_INT4, LengthStyle::Variable(2), w.as_slice());
        let msgs = client.send_and_collect_replies(enc);
        assert_eq!(msgs[0].id, client_method::UPDATE);
        assert_eq!(msgs[1].id, client_method::ON_CMD_RESPONSE);

        // Buy non-removable optdev
        let mut w = BinaryWriter::new();
        w.i16(127);
        w.i16(cmd::CMD_BUY_ITEM);
        w.i32(1); // shop_rev
        w.i32(non_removable.comp_descr);
        w.i32(1);
        let mut enc = ElementEncoder::new();
        enc.message(base_method::DO_CMD_INT3, LengthStyle::Variable(2), w.as_slice());
        let msgs = client.send_and_collect_replies(enc);
        assert_eq!(msgs[1].id, client_method::ON_CMD_RESPONSE);

        // Mount non-removable optdev to MS-1 slot 1
        let mut w = BinaryWriter::new();
        w.i16(128);
        w.i16(cmd::CMD_EQUIP_OPTDEV);
        w.i32(1);
        w.i32(non_removable.comp_descr);
        w.i32(1); // slot 1
        w.i32(0);
        let mut enc = ElementEncoder::new();
        enc.message(base_method::DO_CMD_INT4, LengthStyle::Variable(2), w.as_slice());
        let msgs = client.send_and_collect_replies(enc);
        assert_eq!(msgs[1].id, client_method::ON_CMD_RESPONSE);

        // 7. Gold demount of non-removable optdev (slot 1, is_paid_removal = 1)
        let mut w = BinaryWriter::new();
        w.i16(129);
        w.i16(cmd::CMD_EQUIP_OPTDEV);
        w.i32(1);
        w.i32(0); // dev_cd 0 = demount
        w.i32(1); // slot 1
        w.i32(1); // is_paid_removal = 1 (costs 10 gold)
        let mut enc = ElementEncoder::new();
        enc.message(base_method::DO_CMD_INT4, LengthStyle::Variable(2), w.as_slice());
        let msgs = client.send_and_collect_replies(enc);
        assert_eq!(msgs[0].id, client_method::UPDATE);
        assert_eq!(msgs[1].id, client_method::ON_CMD_RESPONSE);
        let mut r = BinaryReader::new(&msgs[1].data);
        assert_eq!(r.i32().unwrap(), player_id);
        assert_eq!(r.i16().unwrap(), 129);
        assert_eq!(r.i16().unwrap(), cmd::RES_SUCCESS);

        // 8. Equip shells and eqs on MS-1
        // Shells: equip stock shells with count 20
        let mut w = BinaryWriter::new();
        w.i16(130);
        w.i16(cmd::CMD_EQUIP_SHELLS);
        let shells_to_equip = vec![1i32 /* veh_inv_id */, stock_shell_cd as i32, 20];
        w.i32(shells_to_equip.len() as i32);
        for x in shells_to_equip {
            w.i32(x);
        }
        let mut enc = ElementEncoder::new();
        enc.message(base_method::DO_CMD_INT_ARR, LengthStyle::Variable(2), w.as_slice());
        let msgs = client.send_and_collect_replies(enc);
        assert_eq!(msgs[0].id, client_method::UPDATE);
        assert_eq!(msgs[1].id, client_method::ON_CMD_RESPONSE);

        // Equip equipments (eqs)
        let mut w = BinaryWriter::new();
        w.i16(131);
        w.i16(cmd::CMD_EQUIP_EQS);
        let eqs = vec![1i32 /* veh_inv_id */, 0, 0, 0];
        w.i32(eqs.len() as i32);
        for x in eqs {
            w.i32(x);
        }
        let mut enc = ElementEncoder::new();
        enc.message(base_method::DO_CMD_INT_ARR, LengthStyle::Variable(2), w.as_slice());
        let msgs = client.send_and_collect_replies(enc);
        assert_eq!(msgs[0].id, client_method::UPDATE);
        assert_eq!(msgs[1].id, client_method::ON_CMD_RESPONSE);

        // 8b. Buy and equip upgraded turret + gun swap on MS-1 via CMD_BUY_ITEM + CMD_EQUIP
        let up_turret = ms1_def
            .turrets
            .iter()
            .find(|t| t.component.id == 24)
            .expect("MS-1 upgraded turret 24");
        let up_gun = up_turret
            .guns
            .iter()
            .find(|g| g.component.id != 23)
            .expect("MS-1 upgraded gun");
        for (req_id, item_cd) in [
            (203i16, up_turret.component.comp_descr),
            (204i16, up_gun.component.comp_descr),
        ] {
            let mut bw = BinaryWriter::new();
            bw.i16(req_id);
            bw.i16(cmd::CMD_BUY_ITEM);
            bw.i32(1);
            bw.i32(item_cd);
            bw.i32(1);
            let mut enc = ElementEncoder::new();
            enc.message(base_method::DO_CMD_INT3, LengthStyle::Variable(2), bw.as_slice());
            let msgs = client.send_and_collect_replies(enc);
            assert_eq!(msgs[0].id, client_method::UPDATE);
            assert_eq!(msgs[1].id, client_method::ON_CMD_RESPONSE);
            let mut r = BinaryReader::new(&msgs[1].data);
            assert_eq!(r.i32().unwrap(), player_id);
            assert_eq!(r.i16().unwrap(), req_id);
            assert_eq!(r.i16().unwrap(), cmd::RES_SUCCESS);
        }
        for (req_id, item_cd) in [
            (205i16, up_turret.component.comp_descr),
            (206i16, up_gun.component.comp_descr),
        ] {
            let mut bw = BinaryWriter::new();
            bw.i16(req_id);
            bw.i16(cmd::CMD_EQUIP);
            bw.i32(1); // MS-1
            bw.i32(item_cd);
            bw.i32(0);
            let mut enc = ElementEncoder::new();
            enc.message(base_method::DO_CMD_INT3, LengthStyle::Variable(2), bw.as_slice());
            let msgs = client.send_and_collect_replies(enc);
            assert_eq!(msgs[0].id, client_method::UPDATE);
            assert_eq!(msgs[1].id, client_method::ON_CMD_RESPONSE);
            let mut r = BinaryReader::new(&msgs[1].data);
            assert_eq!(r.i32().unwrap(), player_id);
            assert_eq!(r.i16().unwrap(), req_id);
            assert_eq!(r.i16().unwrap(), cmd::RES_SUCCESS);
        }

        // 9. Tankman lifecycle: Buy, XP, Skill, Respec, Passport, Equip
        // Buy tankman (driver for BT-2)
        let driver_slot = bt2
            .crew
            .iter()
            .position(|c| c.primary_role == "driver")
            .expect("BT-2 has driver slot");
        let driver_role_id = wot065_data::descr::role_id_by_name("driver").unwrap() as i32;
        let commander_role_id = wot065_data::descr::role_id_by_name("commander").unwrap() as i32;
        let mut w = BinaryWriter::new();
        w.i16(132);
        w.i16(cmd::CMD_BUY_TMAN);
        w.i32(1); // shop_rev
        w.i32(bt2_cd);
        w.i32(driver_role_id);
        w.i32(0); // 50%
        let mut enc = ElementEncoder::new();
        enc.message(base_method::DO_CMD_INT4, LengthStyle::Variable(2), w.as_slice());
        let msgs = client.send_and_collect_replies(enc);
        assert_eq!(msgs[0].id, client_method::UPDATE);
        assert_eq!(msgs[1].id, client_method::RESYNC_DOSSIERS);
        assert_eq!(msgs[2].id, client_method::ON_CMD_RESPONSE_EXT);
        let mut r = BinaryReader::new(&msgs[2].data);
        assert_eq!(r.i32().unwrap(), player_id);
        assert_eq!(r.i16().unwrap(), 132);
        assert_eq!(r.i16().unwrap(), cmd::RES_SUCCESS);
        let ext_bytes = r.string_bytes().unwrap();
        let ext_dict = pickle::loads(&ext_bytes).unwrap();
        let new_tman_id = ext_dict.get("tmanInvID").unwrap().as_int().unwrap();

        // Incremental CMD_SYNC_DOSSIERS after RESYNC_DOSSIERS must include newly recruited tankman
        let mut w = BinaryWriter::new();
        w.i16(1320);
        w.i16(cmd::CMD_SYNC_DOSSIERS);
        w.i32(dossier_version);
        w.i32(initial_max_change_time);
        w.i32(0);
        let mut enc = ElementEncoder::new();
        enc.message(base_method::DO_CMD_INT3, LengthStyle::Variable(2), w.as_slice());
        let msgs = client.send_and_collect_replies(enc);
        assert_eq!(msgs[0].id, client_method::ON_CMD_RESPONSE);
        assert_eq!(msgs[1].id, client_ids::RESOURCE_HEADER);
        let mut incr_compressed = Vec::new();
        for m in &msgs[2..] {
            assert_eq!(m.id, client_ids::RESOURCE_FRAGMENT);
            let mut rf = BinaryReader::new(&m.data);
            let _rid = rf.u16().unwrap();
            let _seq = rf.u8().unwrap();
            let _flags = rf.u8().unwrap();
            incr_compressed.extend_from_slice(rf.rest());
        }
        let incr_tuple =
            pickle::loads(&pickle::zlib_decompress(&incr_compressed).unwrap()).unwrap();
        let incr_records = incr_tuple.as_list().unwrap()[1].as_list().unwrap();
        let has_new_tman_dossier = incr_records.chunks_exact(4).any(|chunk| {
            chunk[0].as_int() == Some(wot065_data::dossier::DOSSIER_TYPE_TANKMAN as i64)
                && chunk[1].as_int() == Some(new_tman_id)
        });
        assert!(
            has_new_tman_dossier,
            "incremental CMD_SYNC_DOSSIERS must include new tankman {new_tman_id}; got {incr_records:?}"
        );

        // Add tankman XP
        let mut w = BinaryWriter::new();
        w.i16(133);
        w.i16(cmd::CMD_ADD_TMAN_XP);
        w.i32(new_tman_id as i32);
        w.i32(100_000);
        w.i32(0);
        let mut enc = ElementEncoder::new();
        enc.message(base_method::DO_CMD_INT3, LengthStyle::Variable(2), w.as_slice());
        let msgs = client.send_and_collect_replies(enc);
        assert_eq!(msgs[0].id, client_method::UPDATE);
        assert_eq!(msgs[1].id, client_method::ON_CMD_RESPONSE);

        // Add skill (6 = repair)
        let mut w = BinaryWriter::new();
        w.i16(134);
        w.i16(cmd::CMD_TMAN_ADD_SKILL);
        w.i32(new_tman_id as i32);
        w.i32(6); // repair
        w.i32(0);
        let mut enc = ElementEncoder::new();
        enc.message(base_method::DO_CMD_INT3, LengthStyle::Variable(2), w.as_slice());
        let msgs = client.send_and_collect_replies(enc);
        assert_eq!(msgs[0].id, client_method::UPDATE);
        assert_eq!(msgs[1].id, client_method::ON_CMD_RESPONSE);

        // Respec tankman
        let mut w = BinaryWriter::new();
        w.i16(135);
        w.i16(cmd::CMD_TMAN_RESPEC);
        w.i32(1); // shop_rev
        w.i32(new_tman_id as i32);
        w.i32(0); // tman_cost_idx (free)
        w.i32(bt2_cd);
        let mut enc = ElementEncoder::new();
        enc.message(base_method::DO_CMD_INT4, LengthStyle::Variable(2), w.as_slice());
        let msgs = client.send_and_collect_replies(enc);
        assert_eq!(msgs[0].id, client_method::UPDATE);
        assert_eq!(msgs[1].id, client_method::ON_CMD_RESPONSE);

        // Passport tankman (using valid USSR name/icon IDs from catalog)
        let ussr_tman_cfg = cat.tankmen_config(0).expect("USSR tankmen config");
        let tman_group = ussr_tman_cfg
            .normal_groups
            .iter()
            .find(|g| !g.is_female && !g.first_names.is_empty() && !g.last_names.is_empty() && !g.icons.is_empty())
            .expect("USSR normal tankman group");
        let pass_fn = tman_group.first_names[0] as i32;
        let pass_ln = tman_group.last_names[0] as i32;
        let pass_ic = tman_group.icons[0] as i32;
        let passport_cost = wot065_data::economics::EconomicsConfig::default().passport_change_cost as i64;
        let mut w = BinaryWriter::new();
        w.i16(136);
        w.i16(cmd::CMD_TMAN_PASSPORT);
        let pass_args = vec![1i32 /* shop_rev */, new_tman_id as i32, 0, pass_fn, pass_ln, pass_ic];
        w.i32(pass_args.len() as i32);
        for x in pass_args {
            w.i32(x);
        }
        let mut enc = ElementEncoder::new();
        enc.message(base_method::DO_CMD_INT_ARR, LengthStyle::Variable(2), w.as_slice());
        let msgs = client.send_and_collect_replies(enc);
        assert_eq!(msgs[0].id, client_method::UPDATE);
        assert_eq!(msgs[1].id, client_method::ON_CMD_RESPONSE);
        let mut rp = BinaryReader::new(&msgs[1].data);
        assert_eq!(rp.i32().unwrap(), player_id);
        assert_eq!(rp.i16().unwrap(), 136);
        assert_eq!(rp.i16().unwrap(), cmd::RES_SUCCESS);

        // Equip tankman into BT-2 driver seat
        let mut w = BinaryWriter::new();
        w.i16(137);
        w.i16(cmd::CMD_EQUIP_TMAN);
        w.i32(bt2_inv_id as i32);
        w.i32(driver_slot as i32);
        w.i32(new_tman_id as i32);
        let mut enc = ElementEncoder::new();
        enc.message(base_method::DO_CMD_INT3, LengthStyle::Variable(2), w.as_slice());
        let msgs = client.send_and_collect_replies(enc);
        assert_eq!(msgs[0].id, client_method::UPDATE);
        assert_eq!(msgs[1].id, client_method::ON_CMD_RESPONSE);

        // 10. Camouflage on MS-1
        let (&camo_id, camo_def) = cat
            .camouflages
            .get(&0)
            .expect("USSR camouflages")
            .iter()
            .find(|(&id, c)| c.show_in_shop && wot065_data::descr::check_camouflage(id, 0, ms1_def, &cat).is_ok())
            .expect("compatible USSR camo for MS-1");
        let camo_cost = camo_def.gold as i64;
        let mut w = BinaryWriter::new();
        w.i16(138);
        w.i16(cmd::CMD_VEH_CAMOUFLAGE);
        w.i32(1); // shop_rev
        w.i32(1); // veh_inv_id (MS-1)
        w.i32(camo_id as i32);
        let mut enc = ElementEncoder::new();
        enc.message(base_method::DO_CMD_INT3, LengthStyle::Variable(2), w.as_slice());
        let msgs = client.send_and_collect_replies(enc);
        assert_eq!(msgs[0].id, client_method::UPDATE);
        assert_eq!(msgs[1].id, client_method::ON_CMD_RESPONSE);
        let mut rcam = BinaryReader::new(&msgs[1].data);
        assert_eq!(rcam.i32().unwrap(), player_id);
        assert_eq!(rcam.i16().unwrap(), 138);
        assert_eq!(rcam.i16().unwrap(), cmd::RES_SUCCESS);

        // 11. Dossier inspection (CMD_REQ_PLAYER_DOSSIER = 503) via DO_CMD_INT2_STR
        let mut w = BinaryWriter::new();
        w.i16(139);
        w.i16(cmd::CMD_REQ_PLAYER_DOSSIER);
        w.i32(1); // DOSSIER_TYPE_ACCOUNT
        w.i32(player_id);
        w.string("commander_hangar@test.com");
        let mut enc = ElementEncoder::new();
        enc.message(base_method::DO_CMD_INT2_STR, LengthStyle::Variable(2), w.as_slice());
        let msgs = client.send_and_collect_replies(enc);
        assert_eq!(msgs[0].id, client_method::ON_CMD_RESPONSE);
        let mut r = BinaryReader::new(&msgs[0].data);
        assert_eq!(r.i32().unwrap(), player_id);
        assert_eq!(r.i16().unwrap(), 139);
        assert_eq!(r.i16().unwrap(), cmd::RES_STREAM);
        assert_eq!(msgs[1].id, client_ids::RESOURCE_HEADER);
        let mut dos_comp = Vec::new();
        for m in &msgs[2..] {
            assert_eq!(m.id, client_ids::RESOURCE_FRAGMENT);
            let mut rf = BinaryReader::new(&m.data);
            let _rid = rf.u16().unwrap();
            let _seq = rf.u8().unwrap();
            let _flags = rf.u8().unwrap();
            dos_comp.extend_from_slice(rf.rest());
        }
        let dos_bytes = pickle::zlib_decompress(&dos_comp).unwrap();
        assert!(!dos_bytes.is_empty(), "streamed account dossier must not be empty");

        // 12. Sell vehicle: sell BT-2 (verify credits payout, crew return to barracks, and dismiss disposable tankman)
        let mut w = BinaryWriter::new();
        w.i16(207);
        w.i16(cmd::CMD_BUY_TMAN);
        w.i32(1);
        w.i32(bt2_cd);
        w.i32(commander_role_id);
        w.i32(0); // 50%
        let mut enc = ElementEncoder::new();
        enc.message(base_method::DO_CMD_INT4, LengthStyle::Variable(2), w.as_slice());
        let msgs = client.send_and_collect_replies(enc);
        assert_eq!(msgs[2].id, client_method::ON_CMD_RESPONSE_EXT);
        let mut rr = BinaryReader::new(&msgs[2].data);
        let _pid = rr.i32().unwrap();
        let _rq = rr.i16().unwrap();
        let _rc = rr.i16().unwrap();
        let disposable_tman_id = pickle::loads(&rr.string_bytes().unwrap())
            .unwrap()
            .get("tmanInvID")
            .unwrap()
            .as_int()
            .unwrap();

        let mut w = BinaryWriter::new();
        w.i16(208);
        w.i16(cmd::CMD_DISMISS_TMAN);
        w.i32(disposable_tman_id as i32);
        w.i32(0);
        w.i32(0);
        let mut enc = ElementEncoder::new();
        enc.message(base_method::DO_CMD_INT3, LengthStyle::Variable(2), w.as_slice());
        let msgs = client.send_and_collect_replies(enc);
        assert_eq!(msgs[0].id, client_method::UPDATE);
        assert_eq!(msgs[1].id, client_method::ON_CMD_RESPONSE);

        // Read exact credits before selling BT-2
        let mut w_pre = BinaryWriter::new();
        w_pre.i16(209);
        w_pre.i16(cmd::CMD_SYNC_DATA);
        w_pre.i32(0);
        w_pre.i32(0);
        w_pre.i32(0);
        let mut enc_pre = ElementEncoder::new();
        enc_pre.message(base_method::DO_CMD_INT3, LengthStyle::Variable(2), w_pre.as_slice());
        let msgs_pre = client.send_and_collect_replies(enc_pre);
        assert_eq!(msgs_pre[0].id, client_method::ON_CMD_RESPONSE);
        let mut pre_comp = Vec::new();
        for m in &msgs_pre[2..] {
            let mut rf = BinaryReader::new(&m.data);
            let _ = (rf.u16().unwrap(), rf.u8().unwrap(), rf.u8().unwrap());
            pre_comp.extend_from_slice(rf.rest());
        }
        let credits_before_sell = pickle::loads(&pickle::zlib_decompress(&pre_comp).unwrap())
            .unwrap()
            .get("stats")
            .unwrap()
            .get("credits")
            .unwrap()
            .as_int()
            .unwrap();

        let mut w = BinaryWriter::new();
        w.i16(140);
        w.i16(cmd::CMD_SELL_VEHICLE);
        w.i32(1); // shop_rev
        w.i32(bt2_inv_id as i32);
        w.i32(0); // SELL_VEHICLE_FLAG_NONE (crew returns to barracks)
        let mut enc = ElementEncoder::new();
        enc.message(base_method::DO_CMD_INT3, LengthStyle::Variable(2), w.as_slice());
        let msgs = client.send_and_collect_replies(enc);
        assert_eq!(msgs[0].id, client_method::UPDATE);
        assert_eq!(msgs[1].id, client_method::ON_CMD_RESPONSE);
        let mut r = BinaryReader::new(&msgs[1].data);
        assert_eq!(r.i32().unwrap(), player_id);
        assert_eq!(r.i16().unwrap(), 140);
        assert_eq!(r.i16().unwrap(), cmd::RES_SUCCESS);
        let mut ur_sell = BinaryReader::new(&msgs[0].data);
        assert_eq!(ur_sell.i32().unwrap(), player_id);
        let sell_diff = pickle::loads(&ur_sell.string_bytes().unwrap()).unwrap();
        let credits_after_sell = sell_diff
            .get("stats")
            .unwrap()
            .get("credits")
            .unwrap()
            .as_int()
            .unwrap();
        assert!(
            credits_after_sell > credits_before_sell,
            "selling BT-2 must increase credits ({credits_before_sell} -> {credits_after_sell})"
        );

        // Drop Client A (disconnect)
        drop(client);

        // 13. Reconnect with new Client C and verify full persisted state on CMD_SYNC_DATA
        let bf_key_c = b"persisted_sync_k";
        let (mut client_c, _session_c, player_c) =
            login_and_connect(login_addr, base_addr.port(), &pk, username, bf_key_c);
        assert_eq!(player_c, player_id);

        let mut w = BinaryWriter::new();
        w.i16(141);
        w.i16(cmd::CMD_SYNC_DATA);
        w.i32(0);
        w.i32(0);
        w.i32(0);
        let mut enc = ElementEncoder::new();
        enc.message(base_method::DO_CMD_INT3, LengthStyle::Variable(2), w.as_slice());
        let msgs = client_c.send_and_collect_replies(enc);
        assert_eq!(msgs[0].id, client_method::ON_CMD_RESPONSE);
        assert_eq!(msgs[1].id, client_ids::RESOURCE_HEADER);
        let mut sync_comp = Vec::new();
        for m in &msgs[2..] {
            assert_eq!(m.id, client_ids::RESOURCE_FRAGMENT);
            let mut rf = BinaryReader::new(&m.data);
            let _rid = rf.u16().unwrap();
            let _seq = rf.u8().unwrap();
            let _flags = rf.u8().unwrap();
            sync_comp.extend_from_slice(rf.rest());
        }
        let sync_c = pickle::loads(&pickle::zlib_decompress(&sync_comp).unwrap()).unwrap();
        let stats_c = sync_c.get("stats").unwrap();
        let final_credits = stats_c.get("credits").unwrap().as_int().unwrap();
        assert!(final_credits > 0, "credits must remain positive");
        let expected_gold = 50_000 - 300 - 10 - passport_cost - camo_cost;
        assert_eq!(stats_c.get("gold").unwrap().as_int(), Some(expected_gold));
        assert_eq!(stats_c.get("slots").unwrap().as_int(), Some(initial_slots + 1));

        let inv_c = sync_c.get("inventory").unwrap();
        let veh_comp_descr = inv_c.get_int(1).unwrap().get("compDescr").unwrap().as_dict().unwrap();
        assert_eq!(veh_comp_descr.len(), 3, "sold BT-2 must not be in vehicles");
        assert!(veh_comp_descr.iter().all(|(k, _)| k.as_int() != Some(bt2_inv_id)));

        let tman_comp_descr = inv_c.get_int(8).unwrap().get("compDescr").unwrap().as_dict().unwrap();
        assert!(
            tman_comp_descr.iter().any(|(k, _)| k.as_int() == Some(new_tman_id)),
            "new tankman must be in persisted inventory"
        );
    })
    .await
    .expect("blocking full hangar persistence test");

    stop.store(true, Ordering::Relaxed);
    let poke = UdpSocket::bind("127.0.0.1:0").unwrap();
    let _ = poke.send_to(b"\x00", login_addr);
    let _ = login_thread.join();
    base_task.abort();
    let _ = std::fs::remove_file(&session_file);
}

#[test]
#[ignore = "hits live wot-server daemon on 127.0.0.1:20016 / 20017"]
fn live_daemon_udp_smoke() {
    let pub_pem = std::fs::read_to_string(r"C:\Users\qwerty\Desktop\World_of_Tanks\res\loginapp_wot.pubkey")
        .expect("read retail client pubkey");
    let pubkey = RsaPublicKey::from_public_key_pem(&pub_pem).expect("parse pubkey");
    verify_full_login_and_hangar_flow(
        "127.0.0.1:20016".parse().unwrap(),
        20017,
        &pubkey,
        b"live_e2e_key_065",
        3,
    );
}
