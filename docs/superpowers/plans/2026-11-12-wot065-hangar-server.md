# WoT 0.6.5 Decompilation & Rust Hangar Server Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Finish decompiling and syntax-verifying the WoT 0.6.5 client/engine scripts and complete `bw201`, `wot-baseapp`, and `wot-server` so the retail `WorldOfTanks.exe` logs in, enters the hangar, and displays all 130 vehicles.

**Architecture:** Retires the partial `crates/wot-baseapp/src/transport/` stub and builds `wot-baseapp` directly on `bw201` (Mercury UDP framing, `Address` big-endian port fix, Blowfish `EncryptionFilter`, fragment segmentation) and `wot065-data` (catalog + pickled sync bundles). Live login runs over UDP 20016 (`bw201-login` -> atomic `session.json`) and UDP 20017 (`wot-baseapp` Blowfish-encrypted Mercury channel -> `createBasePlayer(0)` -> `showGUI` -> `RES_STREAM` + `onCmdResponseExt` two-phase sync).

**Tech Stack:** Rust (`bw201`, `wot065-data`, `wot-baseapp`, `wot-server`, Tokio, Clap, Serde, Blowfish, RSA), Python 3.12 (`_tools/venv` with `uncompyle6` 3.9.3 + `xdis` 6.1.8) + Python 2.7.18 (`_tools/py27/inst/python.exe`).

**Spec:** `docs/superpowers/specs/2026-11-12-wot065-hangar-server-design.md`

## Global Constraints

- **Target retail client (`C:\Users\qwerty\Desktop\World_of_Tanks`)**: Never modify client binaries or scripts; `res/scripts_config.xml` (`127.0.0.1:20016`) and `res/loginapp_wot.pubkey` (matching `.scratch/wot-keys/loginapp_priv.pem`) stay intact, and `raw/` stays immutable.
- **No stdlib decompilation bloat**: Skip `res_bw/scripts/common/Lib/` (`692` Python 2.6 stdlib `.pyc` files); decompile only `res/scripts` (`220`), `res_bw/scripts/client` (`132` -> `bw_client/`), and the `4` non-`Lib` files in `res_bw/scripts/common` (`bw_common/`).
- **Exact BW 2.0.1 wire contracts**:
  - `LoginReplyRecord`: big-endian UDP port (`port.to_be_bytes()`), `salt: u16` LE, `session_key: u32` LE (`basictypes.cpp:208-228`).
  - `baseAppLogin` + all BaseApp datagrams: Blowfish `EncryptionFilter` (`BlowfishKey::packet_encrypt` / `packet_decrypt`) is active from the very first `baseAppLogin` packet (`baseapp_login_request.cpp:71`, `server_connection.cpp:109`).
  - External channel flags: server -> client reliable packets MUST set `FLAG_ON_CHANNEL | FLAG_IS_RELIABLE | FLAG_HAS_SEQUENCE_NUMBER`, and MUST NEVER set `FLAG_CREATE_CHANNEL` (`packet_receiver.cpp:315`).
  - `Account` BaseMethods (`0xC0 | exposedId`) carry NO `entityID` prefix (`startProxyMessage`); `Account` ClientMethods (`0x80 | exposedId`) DO carry `playerID: i32` prefix.
  - `ClientInterface::resourceFragment` (`0x38`): `rid: u16, seq: u8, flags: u8, data: [u8]` where `flags == 1` means final segment (`isLast`), `flags == 0` otherwise, and `seq` starts at `0` modulo `0xff`.
  - Two-phase `CMD_SYNC_DATA (100)`: phase 1 (`client_rev == 0`) sends `onCmdResponse(RES_STREAM=1)` + `resourceHeader` + `resourceFragment`; phase 2 (`client_rev == 1`) MUST send `Account.onCmdResponseExt` (`0x83`) with `RES_SUCCESS (0)` and pickled `{'rev': 1, 'prevRev': 1}` so `Account._update` does not treat empty `ext` as `isFullSync=True` and wipe `inventory.__cache`.

## Review Focus

- **Server -> client `FLAG_CREATE_CHANNEL` rejection**: Client drops packets with `FLAG_CREATE_CHANNEL` on external interfaces (`packet_receiver.cpp:315`); unit-test that `Channel` outbound packet builder never sets `FLAG_CREATE_CHANNEL`.
- **Single- vs multi-fragment `ResourceFragment` flag semantics (`flags == 1`)**: Passing `flags = 3` leaves `DataDownload::complete()` false forever (`args.flags == 1` check in `server_connection.cpp:2168`); unit-test both single-chunk (`flags = 1`, `seq = 0`) and multi-chunk (`[0, 0, ..., 1]`, `seq = 0..n-1 mod 0xff`) streams.
- **Two-phase `CMD_SYNC_DATA` cache preservation**: Replying with `onCmdResponse` instead of `onCmdResponseExt({'rev': 1, 'prevRev': 1})` clears all 130 vehicles on sync step 2; unit-test that `client_rev == 1` emits `0x83` with pickled dict containing `'prevRev': 1, 'rev': 1`.
- **Malformed/unencrypted/wrong-key UDP datagrams on `wot-baseapp`**: A stale `session.json`, truncated packet, or garbage UDP datagram must log and drop without panicking or tearing down active sessions; unit-test garbage datagram + session reload path.
- **Keepalive / auxiliary requests & `REQUEST_ID_NO_RESPONSE (102)`**: Rapid 1-second `CMD_REQ_SERVER_STATS` (`requestID=102`), `doCmdStr(CMD_SET_LANGUAGE=1000)`, empty channel ACK/keepalive packets, and `disconnectClient` must never stall or fail framing; unit-test each message path.

---

### Task 1: Complete `.pyc` Decompilation & Python 2.7 Verification

**Files:**
- Modify: `.scratch/research/decompiled/_tools/decompile_all.py:1-119`
- Modify: `.scratch/research/decompiled/_tools/check_py2_syntax.py:1-59`
- Create: `.scratch/research/decompiled/client/post_processing/__init__.py` (replaces `.py.tmp`)
- Create: `.scratch/research/decompiled/bw_client/**` (`132` `.py` + `.dis.txt`)
- Create: `.scratch/research/decompiled/bw_common/*.py` + `.dis.txt` (`bwdebug`, `json`, `UserDataObjectRef`, `validate_shells`)

**Interfaces:**
- Uses: `.scratch/research/decompiled/_tools/venv/Scripts/python.exe` (`xdis 6.1.8`, `uncompyle6 3.9.3`), `.scratch/research/decompiled/_tools/py27/inst/python.exe` (CPython 2.7.18).
- Produces: Clean `.py` + non-empty `.dis.txt` for all `220 + 132 + 4 = 356` target `.pyc` files and `_tools/py2_syntax_report.txt` showing `bad=0`.

- [ ] **Step 1: Verify `check_py2_syntax.py` baseline failure on missing `client/post_processing/__init__.py`**

Run: `& ".\.scratch\research\decompiled\_tools\py27\inst\python.exe" -m py_compile .\.scratch\research\decompiled\_tools\py27\inst\Lib\os.py` (sanity check) and inspect `Test-Path .\.scratch\research\decompiled\client\post_processing\__init__.py` (expects `False`, `.py.tmp` present).

- [ ] **Step 2: Fix `_tools/decompile_all.py` and parameterize source/destination**

Update `_tools/decompile_all.py`:
- Remove invalid `header=True` kwarg from `disassemble_file(pyc, f)` (`xdis 6.1.8` signature does not accept `header`).
- Accept CLI args `--src <dir> --out-subdir <name> [--exclude-dirs Lib] [--report <name>]` (defaulting to WoT `res/scripts` -> `.` and `decompile_report.txt`) via `argparse` so one script cleanly runs over `res/scripts`, `res_bw/scripts/client` (`bw_client`), and `res_bw/scripts/common` excluding `Lib` (`bw_common`), using globals `SRC`/`OUT` passed into workers cleanly (Windows `ProcessPoolExecutor` compatible via worker tuple `(pyc, src_dir, out_dir)`).

- [ ] **Step 3: Reconstruct `.scratch/research/decompiled/client/post_processing/__init__.py` from disassembly**

Recover `_Effect.__isSupported(self, settings)` in `client/post_processing/__init__.py` matching the exact Python 2.6 `xdis` bytecode from offsets `0..125` (ternary `isEnabled = True if self.__optional == '' else settings[self.__optional]`, `mrtEnabled = BigWorld.getGraphicsSetting('MRT_DEPTH') == 0 and not BigWorld.graphicsSettingsNeedRestart()`, `return self.__curQuality in self.__qualityRange and isEnabled and (not self.__isAdvanced or self.__isAdvanced and mrtEnabled)` — verify compiled code object bytecode equivalence with `py27`), keep the remaining decompiled lines from `__init__.py.tmp`, and remove `__init__.py.tmp`.

- [ ] **Step 4: Run `decompile_all.py` on all three target scopes**

Run with `.scratch\research\decompiled\_tools\venv\Scripts\python.exe`:
1. `res/scripts` (produces `.dis.txt` for all `220` files; back up or preserve our recovered `client/post_processing/__init__.py` after if `uncompyle6` skips/overwrites — skip writing `.py` only when existing recovered file is valid and `.pyc` failed, or re-restore `__init__.py` and delete `__init__.py.tmp`).
2. `C:\Users\qwerty\Desktop\World_of_Tanks\res_bw\scripts\client` -> `--out-subdir bw_client --report decompile_report_bw_client.txt` (`132` files).
3. `C:\Users\qwerty\Desktop\World_of_Tanks\res_bw\scripts\common` --exclude-dirs `Lib` -> `--out-subdir bw_common --report decompile_report_bw_common.txt` (`4` files).

- [ ] **Step 5: Run Python 2.7 syntax validation across all `356` decompiled files**

Update `check_py2_syntax.py` exclusions if needed to cover `client`, `common`, `bw_client`, `bw_common` cleanly (skipping `_tools`, `_generated`, `_index`, `res`). Run `& ".\.scratch\research\decompiled\_tools\venv\Scripts\python.exe" .scratch\research\decompiled\_tools\check_py2_syntax.py`. If any file in `bw_client` or `bw_common` fails `py_compile`, inspect its `.dis.txt` and patch the `.py` syntax defect; repeat until `total=356 ok=356 bad=0` and verify all `.dis.txt` are non-empty.

- [ ] **Step 6: Commit Task 1**

Wait: `.scratch/` is gitignored or untracked? Check with `git status` — only tracked repo files get committed (`git add` repo changes if any, leave `.scratch` untracked/ignored if `.gitignore` excludes `.scratch`).

---

### Task 2: Fix `crates/bw201`, `tools/loginapp_proto.py`, and Implement `bw201-login`

**Files:**
- Modify: `crates/bw201/src/lib.rs`
- Modify: `crates/bw201/src/packet.rs:340-357,456-461` (+ fragment helper / unit tests)
- Modify: `crates/bw201/src/login.rs:350-360,550-562`
- Modify: `crates/bw201/src/loginapp.rs:501-511`
- Modify: `tools/loginapp_proto.py:165-170`
- Create: `crates/bw201/src/bin/bw201-login.rs`

**Interfaces:**
- Produces (consumed by Task 3 `wot-baseapp`):
  - `bw201::stream::{Address, BinaryReader, BinaryWriter, StreamError}`
  - `bw201::packet::{Endian, Fragment, Packet, PacketConfig, PacketError, FilterError, PACKET_HEADER_SIZE, PACKET_MAX_SIZE, FLAG_HAS_REQUESTS, FLAG_HAS_PIGGYBACKS, FLAG_HAS_ACKS, FLAG_ON_CHANNEL, FLAG_IS_RELIABLE, FLAG_IS_FRAGMENT, FLAG_HAS_SEQUENCE_NUMBER, FLAG_INDEXED_CHANNEL, FLAG_HAS_CHECKSUM, FLAG_CREATE_CHANNEL, FLAG_HAS_CUMULATIVE_ACK}`
  - `bw201::bundle::{DecodedMessage, ElementEncoder, LengthStyle, RequestInfo, decode_messages, split_bundle_packets}`
  - `bw201::crypto::{BlowfishError, BlowfishKey, LoginKey, RsaError, hexdump}`
  - `bw201::interfaces::{baseapp_ext_ids, baseapp_ext_interface, client_ids, client_interface, login_ids, login_interface, InterfaceTable, REPLY_MESSAGE_IDENTIFIER}`
  - `bw201::login::{LogOnStatus, LoginSession, login_reply_record, build_success_reply, build_failure_reply, parse_login_args}`
  - `bw201::loginapp::{LoginApp, LoginAppConfig}`

- [ ] **Step 1: Expose all modules in `crates/bw201/src/lib.rs` and run `cargo test -p bw201` to surface existing failures**

Populate `crates/bw201/src/lib.rs` with `pub mod` declarations for `bundle`, `crypto`, `interfaces`, `login`, `loginapp`, `packet`, `stream`. Run `cargo test -p bw201` -> Expected: FAIL (compile error in `packet.rs:346` piggybacks tuple, plus logic failures on `acks` order / `login.rs:560` placeholder / BE port tests once compiled).

- [ ] **Step 2: Write failing tests for BE `LoginReplyRecord`, `acks` footer roundtrip, piggybacks decode, and multi-packet bundle fragmentation**

- Update `crates/bw201/src/login.rs:551-561` (`success_reply_layout`) and `crates/bw201/src/loginapp.rs:504-510` to assert `u16::from_be_bytes([record[4], record[5]]) == 20017` and full 12-byte decrypted record equality (`[127, 0, 0, 1]`, `20017` BE, `0` LE, `session_key` LE).
- Add tests in `crates/bw201/src/bundle.rs`:
  - `split_single_packet_bundle`: fits within `max_body` -> 1 packet, no `FLAG_IS_FRAGMENT`, `seq = first_seq`.
  - `split_multi_packet_fragment_bundle`: `2500`-byte body with `max_body = 1000` -> 3 packets (`seq = 10, 11, 12`), each has `FLAG_IS_FRAGMENT | FLAG_ON_CHANNEL | FLAG_IS_RELIABLE | FLAG_HAS_SEQUENCE_NUMBER`, `fragment = Some(Fragment { begin: 10, end: 12 })`, concatenating bodies recovers the exact `2500` bytes; `FLAG_CREATE_CHANNEL` is NEVER set.

- [ ] **Step 3: Fix `packet.rs` piggybacks + `acks`, `login.rs` BE port, `bundle::split_bundle_packets`, and `tools/loginapp_proto.py`**

- In `crates/bw201/src/packet.rs:346-350`: unpack `((!raw) as u16 as usize, true)` when `raw < 0`, else `(raw as u16 as usize, false)`. Wait: double check BW201 `packet.cpp`: one's complement length bit marks the LAST piggyback in C++, and wait — `raw < 0` is signed `i16` (`~len` has top bit set, so `raw < 0` IS last!). And fix `acks` encode at `packet.rs:456-461`: write `config.acks.iter().rev()` before `out.push(config.acks.len() as u8)` (so `decode` reading from back pops `acks[0]`, `acks[1]`, ... in forward slice order, matching `Channel::writeFooter`'s `packFooter(nAcks)` then `packFooter(ack[0..n])`).
- In `crates/bw201/src/login.rs:353-360`: use `Address::new(ip, port, salt).write_to(...)` or `port.to_be_bytes()` and update the doc comment (`big-endian u16 port, 20017 -> 4e 31`).
- In `crates/bw201/src/bundle.rs`: implement `pub fn split_bundle_packets(body: &[u8], max_body: usize, first_seq: u32, extra_flags: u16, piggyback_acks: &[u32]) -> Vec<Packet>`.
- In `tools/loginapp_proto.py:165-169`: change `struct.pack("<H", self.args.base_port)` to `struct.pack(">H", self.args.base_port)`.

- [ ] **Step 4: Implement `crates/bw201/src/bin/bw201-login.rs`**

Accept `--bind <SocketAddr>` (default `127.0.0.1:20016`), `--base-app <SocketAddrV4>` (default `127.0.0.1:20017`), `--priv-key <PathBuf>`, `--session-out <Option<PathBuf>>`, `--session-key <u32>` (default `0x1234_5678`), and `--name <String>` (default `"Player"` — stored/logged). Init `tracing_subscriber`, read PEM via `LoginKey::from_pkcs8_pem`, bind `LoginApp::bind`, and call `LoginApp::run` with a `ctrlc`/Ctrl+C flag (`AtomicBool`).

- [ ] **Step 5: Run tests and verify build**

Run: `cargo test -p bw201 && cargo test -p wot065-data && cargo build --bin bw201-login`
Expected: All unit/integration tests pass with zero warnings/errors.

- [ ] **Step 6: Commit Task 2**

Commit `crates/bw201/*` and `tools/loginapp_proto.py` fixes (`feat(bw201): finish transport crate, fix BE port & footers, add bw201-login`).

---

### Task 3: Implement `crates/wot-baseapp` & Supervisor Fixes

**Files:**
- Remove: `crates/wot-baseapp/src/transport/` (`mod.rs` and folder)
- Modify: `crates/wot-baseapp/Cargo.toml`
- Modify: `crates/wot-baseapp/src/lib.rs`
- Create: `crates/wot-baseapp/src/payloads.rs`
- Create: `crates/wot-baseapp/src/channel.rs`
- Create: `crates/wot-baseapp/src/protocol.rs`
- Create: `crates/wot-baseapp/src/main.rs`
- Modify: `src/main.rs:292-295,340`
- Modify: `wiki/analyses/rust-server-architecture.md` (fix `TCP 20017` -> `UDP 20017`)

**Interfaces:**
- Consumes: `bw201::*` and `wot065_data::{catalog, account, pickle}` (`wot065_data::catalog::load`, `FullSyncOptions`, `full_sync_pickle`, `shop_cache_pickle`, `dossier_sync_pickle`, `stream_bundle`, `showgui_ctx_pickle`, `pickle::{Value, dumps}`).
- Produces: Binary `wot-baseapp` matching `src/main.rs` CLI flags (`--bind`, `--session-file`, `--game-dir`, optional `--account-diff`, `--showgui-ctx`, `--shop-cache`, `--dossier-sync`).

- [ ] **Step 1: Update `crates/wot-baseapp/Cargo.toml` and remove obsolete `src/transport/`**

Depend on `bw201 = { path = "../bw201" }`, `wot065-data = { path = "../wot065-data" }`, `tokio = { version = "1", features = ["net", "rt-multi-thread", "macros", "time", "signal"] }`, `clap = { version = "4", features = ["derive"] }`, `serde = { version = "1", features = ["derive"] }`, `serde_json = "1"`, `anyhow = "1"`, `tracing = "0.1"`, `tracing-subscriber = { version = "0.3", features = ["env-filter"] }`. Delete `crates/wot-baseapp/src/transport/`.

- [ ] **Step 2: Write failing unit/integration tests for `payloads`, `channel`, and `protocol` in `crates/wot-baseapp`**

Include tests pinning all Review Focus cases:
1. `test_handshake_baseapp_login_encrypted_and_create_base_player`: simulates Blowfish-encrypted `baseAppLogin` (`session_key`, `attempt=0`) -> checks encrypted on-channel reply (`0xFF`, `session_key`), followed by client `authenticate` + `enableEntities (0x0A)` -> asserts `ClientInterface::authenticate`, `createBasePlayer` (`entity_type_id == 0`, `name`, `normalizedName`, `serverSettings` containing `'vivoxDomain': ''`), and `Account.showGUI (0x90)`; asserts `FLAG_CREATE_CHANNEL` is NEVER set on outbound packets.
2. `test_two_phase_cmd_sync_data_and_res_stream_flags`: sends `Account.doCmdInt3 (0xC4)` (NO `entityID` prefix!) `request_id=120, cmd=100 (CMD_SYNC_DATA), i1=0, i2=0, i3=0` -> asserts `onCmdResponse (0x82)` with `RES_STREAM=1` + `resourceHeader (0x37, rid=120, desc)` + `resourceFragment (0x38, rid=120, seq=0.., flags==1 on last only)` carrying decompressable full sync pickle; then sends second `doCmdInt3` `request_id=121, cmd=100, i1=0, i2=1` -> asserts `onCmdResponseExt (0x83)` with `RES_SUCCESS=0` and pickled `{'rev': 1, 'prevRev': 1}` (preventing cache wipe).
3. `test_shop_dossier_and_auxiliary_commands`: verifies `CMD_SYNC_SHOP (300)` -> `RES_STREAM` + fragments (`flags==1` on final), `CMD_SYNC_DOSSIERS (600)` -> `RES_STREAM`, `CMD_REQ_SERVER_STATS (501)` with `request_id=102` (`REQUEST_ID_NO_RESPONSE`) -> emits no `onCmdResponse` (or `receiveServerStats` `0x92`), `doCmdStr` (`0xC3`, `CMD_SET_LANGUAGE=1000`), `disconnectClient (0x0C)`, and unimplemented command with `request_id=125` -> `onCmdResponse(125, RES_FAILURE=-1)`.
4. `test_malformed_datagram_and_retransmit`: malformed/wrong-key UDP datagram returns `Ok(None)` without panicking, and `ChannelState::retransmit_due` re-emits unacked reliable packets until ACKed.

- [ ] **Step 3: Implement `payloads.rs`, `channel.rs`, `protocol.rs`, `lib.rs`, and `main.rs` cleanly**

- `payloads.rs`: `BaseAppPayloads::load(game_dir, overrides...)` loads `catalog::load`, builds or reads `full_sync_pickle` (`FullSyncOptions { unlock_all: true, .. }` by default or file override), `shop_cache_pickle`, `dossier_sync_pickle`, `stream_bundle` `(zlib, desc)`, `showgui_ctx` (fresh `serverUTC`), `server_settings_pickle` (`{'vivoxDomain': ''}`), and `sync_ack_ext_pickle` (`{'rev': 1, 'prevRev': 1}`). Wait: ensure `--account-diff` default from `src/main.rs` OR fallback keeps all 130 vehicles!
- `channel.rs`: `PeerChannel` tracks `BlowfishKey`, `session_key`, `in_seq`, `out_seq`, `pending_acks`, `unacked: BTreeMap<u32, UnackedPacket>`, `frag_buf: Option<InboundFragment>`, builds outbound on-channel packets (`split_bundle_packets` with `max_body = 1350`), applies ACKs from incoming packets, handles empty keepalive/ACK-only channel packets (`body.is_empty()`), and `encrypt_packet`.
- `protocol.rs`: `BaseAppServer` / `SessionHandler` dispatches `BaseAppExtInterface` IDs + `0xC3..=0xC7` `Account` BaseMethods (NO `entityID`!), encodes `ClientInterface` + `Account` ClientMethods (`playerID: i32` prefix), chunks large streams cleanly (`1024`-byte fragments in reliable sequence, splitting bundles across packets when needed with `FLAG_IS_FRAGMENT`). Wait: ensure `resourceFragment` chunk loop uses `seq = (idx % 255) as u8` (`0..254`, BW201 uses `% 0xff` in `data_download.cpp:46-48`!) and `flags = if is_last { 1 } else { 0 }`. Note: put `onCmdResponse(rid, RES_STREAM)` BEFORE `resourceHeader`/`resourceFragment` in the reliable bundle stream so `self.__onStreamComplete[rid]` is guaranteed registered before `onStreamComplete(rid, ...)` fires!
- `main.rs`: Tokio UDP receive loop + 200ms retransmission interval + reload `LoginSession` from `--session-file` when an unknown peer packet arrives whose decryption with cached session fails.
- Update `src/main.rs:292` (`if session_file.exists()`), `src/main.rs:340` (`udp`), and `wiki/analyses/rust-server-architecture.md` (`UDP 20017`).

- [ ] **Step 4: Run `cargo test --workspace` & `cargo build --workspace`**

Run: `cargo test --workspace && cargo build --workspace`
Expected: All unit & integration tests pass across `bw201`, `wot065-data`, `wot-baseapp`, and `wot-server`.

- [ ] **Step 5: Commit Task 3**

Commit `crates/wot-baseapp/*`, `src/main.rs`, and `wiki/analyses/rust-server-architecture.md` changes (`feat(baseapp): implement UDP Mercury BaseApp server on bw201`).

---

### Task 4: Live Client E2E Verification (`WorldOfTanks.exe`)

**Files:**
- Inspect: `C:\Users\qwerty\Desktop\World_of_Tanks\python.log`
- Inspect: `.scratch/logs/*.log`, `.scratch/e2e-*.png`

- [ ] **Step 1: Verify client config matches `.scratch/wot-keys/loginapp_priv.pem`**

Run: `cargo run --quiet -- check-client --game-dir C:\Users\qwerty\Desktop\World_of_Tanks --pubkey .scratch\wot-keys\loginapp_pub.pem`
Expected: `127.0.0.1:20016` and SHA-256 `MATCH`.

- [ ] **Step 2: Start `wot-server serve` and launch `WorldOfTanks.exe` + `.scratch/tools/wot-e2e-live.ps1`**

Clear/truncate `C:\Users\qwerty\Desktop\World_of_Tanks\python.log` only? Wait: don't edit retail dir manually if avoidable — instead note its tail size before launch and read new lines after! Start `target\debug\wot-server.exe serve --game-dir C:\Users\qwerty\Desktop\World_of_Tanks` as background daemon, run `.scratch\tools\wot-e2e-live.ps1` (extending screenshot wait if needed so hangar loads completely), and capture screenshots.

- [ ] **Step 3: Inspect logs & screenshots**

Inspect `.scratch\logs\bw201-login.{out,err}.log`, `.scratch\logs\wot-baseapp.{out,err}.log`, `C:\Users\qwerty\Desktop\World_of_Tanks\python.log` new tail, and screenshots via `read_file`. Confirm:
- Login succeeds (`baseAppLogin`, `authenticate`, `enableEntities`, `createBasePlayer`, `showGUI`)
- `AccountSyncData` (`CMD_SYNC_DATA` phase 1 `RES_STREAM` + phase 2 `RES_SUCCESS` via `onCmdResponseExt`), `Shop`, and `DossierCache` complete cleanly
- Hangar UI enters cleanly and carousel shows all 130 vehicles with zero disconnect/traceback in `python.log`.

- [ ] **Step 4: Fix any runtime quirks discovered in live client test**

If `python.log` or screenshots reveal a missing UI response or unexpected opcode, fix in Rust with a new regression unit test, re-run `cargo test --workspace && cargo build --workspace`, and re-run Step 2–3 cleanly.

- [ ] **Step 5: Stop test processes & run `verification-before-completion`**

Stop `WorldOfTanks.exe` and `wot-server`, run final `cargo test --workspace` and `py2_syntax_report` / `raw/` immutability checks, verify `git status` clean except intentional commits/files, and commit any final test/code fixes.
