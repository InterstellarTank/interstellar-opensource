# WoT 0.6.5 Decompilation & Hangar Server — Design

**Date:** 2026-11-12
**Status:** approved (autonomous user go-ahead, verified against BW 2.0.1 C++ & WoT 0.6.5 Python scripts)
**Project:** World of Tanks 0.6.5 game server (`c:\Users\qwerty\Documents\dstest`)
**Target Client:** `C:\Users\qwerty\Desktop\World_of_Tanks` (`v.0.6.5`)

## Intent

Complete the remaining `.pyc` script decompilation for the unpacked World of Tanks 0.6.5 retail client and finish implementing the Rust login + baseapp server (`bw201`, `wot-baseapp`, `wot-server`) so that the unmodified retail executable (configured only via `res/scripts_config.xml` and `res/loginapp_wot.pubkey`) can log in, enter the hangar/garage, and display all 130 vehicles from the game data.

## Non-goals

- Combat / CellApp arena simulation, battles, bots, or matchmaking.
- Persistent database storage or multi-account state beyond in-memory session handling.
- Garage economy mutations (buying/selling tanks, crew/module changes persisted across sessions) — non-sync UI queries return safe no-op/failure or empty responses so the client never hangs or disconnects.
- Decompiling Python 2.6 standard library files in `res_bw/scripts/common/Lib/` (`692` `.pyc` files).

---

## Findings Driving the Design

Source verification against BigWorld 2.0.1 C++ (`.scratch/research/protocol-scout/repos/bw201/src/`) and WoT 0.6.5 scripts resolved four critical blockers from earlier prototypes:

1. **BaseApp port endianness bug**: `crates/bw201/src/login.rs` (`login_reply_record`) and `tools/loginapp_proto.py` wrote `port` in little-endian (`to_le_bytes()`), whereas BigWorld 2.0.1 `Mercury::Address` (`lib/network/basictypes.cpp`) stores and serializes `ip` and `port` in network byte order (`to_be_bytes()`). Port `20017` (`0x4E31`) sent little-endian (`31 4E`) directed the client to port `12622`, dropping all BaseApp traffic.
2. **0-based `EntityTypeID`**: BigWorld 2.0.1 `EntityDescriptionMap::parse` + `EntityType::init` assign `clientIndex` starting at `0` for `isClientType()` entities in `res/scripts/entities.xml`. Because `Account` is first in `entities.xml`, `createBasePlayer` must pass `entityTypeID = 0` (not `1`).
3. **UDP Mercury Channel for BaseApp**: BaseApp uses UDP (`PACKET_MAX_SIZE`, Blowfish packet encryption with `0xDEADBEEF` + 1-byte wastage trailer, reliable sequence numbers, ACKs, and bundle fragmentation), not TCP.
4. **Two-phase `CMD_SYNC_DATA` over `RES_STREAM` + `onCmdResponseExt` (`rev`/`prevRev`)**: On `RES_STREAM (1)`, `SyncController` decompresses and unpickles `full_sync_pickle` (`rev: 1, prevRev: 0`), updates `AccountSyncData.revision = 1`, calls `Account._update(False, data)`, and immediately sends a second `CMD_SYNC_DATA` with `revision = 1`. Crucially, `AccountSyncData.__onSyncResponse` calls `Account._update(..., ext)` where missing `'prevRev'` triggers `isFullSync = True` -> `Inventory.synchronize(True, {})` -> `self.__cache.clear()`! Therefore, on the second `CMD_SYNC_DATA` (`client_rev == server_rev`), the server MUST reply with `Account.onCmdResponseExt` (`0x83`) carrying `resultID = RES_SUCCESS (0)` and pickled `ext = {'rev': 1, 'prevRev': 1}` (not plain `onCmdResponse` with default `ext = {}`) so `AccountSyncData.__onSyncResponse` marks `self.__isSynchronized = True` without wiping the inventory/stats caches. `CMD_SYNC_SHOP (300)` and `CMD_SYNC_DOSSIERS (600)` use `RES_STREAM (1)` + `resourceHeader`/`resourceFragment`.
5. **Exact `startProxyMessage` & `ResourceFragmentArgs` wire layout (`BW 2.0.1` C++)**: Client -> BaseApp `Account.base.*` calls use `startProxyMessage(msgID)` (`0xC0 | exposedId`) with **NO `entityID` prefix** (`py_server.cpp:111-116`, `server_connection.cpp:680-695`); e.g. `doCmdInt3` (`0xC4`) is `16` bytes: `requestID: i16, cmd: i16, i1: i32, i2: i32, i3: i32`. Conversely, Server -> Client `Account` calls (`0x80 | exposedId`) prepend `playerID: i32`. For `ClientInterface::resourceFragment` (`0x38`), `ResourceFragmentArgs` is `rid: u16, seq: u8, flags: u8` followed by raw chunk bytes, and `server_connection.cpp:2168` checks `args.flags == 1` (`isLast: bool`) — so `flags` MUST be `1` on the final (or single) fragment and `0` on earlier fragments, with `seq` starting at `0` (`DataDownload.expected_ == 0`, modulo `0xff`).

---

## Component Architecture

### 1. Client Script Decompilation (`.scratch/research/decompiled/`)

- **Tooling fix**: Fix `.scratch/research/decompiled/_tools/decompile_all.py` `disassemble_file` call (remove invalid `header=True` kwarg for `xdis 6.1.8`) so `.dis.txt` disassembly files are generated properly alongside `.py`.
- **Gap recovery in `res/scripts` (`220` `.pyc` files)**: Currently `219/220` `.py` files exist; recover `client/post_processing/__init__.pyc` (decompile or reconstruct clean Python 2.6 module verified from its bytecode disassembly, removing stale `__init__.py.tmp`).
- **Engine script decompilation (`res_bw/scripts`)**: Decompile `132` `.pyc` files from `res_bw/scripts/client/` into `.scratch/research/decompiled/bw_client/` and `4` non-stdlib `.pyc` files from `res_bw/scripts/common/` (`bwdebug.pyc`, `json.pyc`, `UserDataObjectRef.pyc`, `validate_shells.pyc`) into `.scratch/research/decompiled/bw_common/`. Skip `res_bw/scripts/common/Lib/` (`692` stdlib modules).
- **Validation**: Extend/run Python 2.7 syntax check (`py27/inst/python.exe`) over all decompiled `.py` files under `client/`, `common/`, `bw_client/`, and `bw_common/`.

### 2. Shared BigWorld 2.0.1 Protocol Crate (`crates/bw201`)

Retire the incomplete duplicate `crates/wot-baseapp/src/transport/` stub and make `crates/bw201` the single wire/channel/crypto library for both LoginApp and BaseApp:

- **Library root (`crates/bw201/src/lib.rs`)**: Re-export `stream`, `packet`, `bundle`, `crypto`, `interfaces`, `login`, `loginapp`, plus any BaseApp channel/resource framing helpers.
- **Endianness & test fixes**:
  - Update `login_reply_record` in `crates/bw201/src/login.rs` to write `port.to_be_bytes()`, and fix the placeholder test assertion at `login.rs:560` as well as `loginapp.rs:505`.
  - Update `tools/loginapp_proto.py` to pack `port` big-endian (`>H`).
- **`bw201-login` binary (`crates/bw201/src/bin/bw201-login.rs`)**:
  - Implement CLI accepting `--bind`, `--base-app`, `--priv-key`, `--session-out`, `--name` (matching `src/main.rs` supervisor args), plus optional `--dump-request` diagnostic mode.
  - Run `LoginApp::run`, writing `session.json` atomically via `LoginSession::write_atomic` and printing ready/login logs.
- **Mercury Packet & Fragmentation Support**:
  - Fix compile/roundtrip defects in `crates/bw201/src/packet.rs`: fix `piggybacks` decode tuple syntax (`packet.rs:346-351`) and `acks` encode order (`packet.rs:457-460`) so they match BW 2.0.1 C++ (`lib/network/channel.cpp:1604-1631`, `lib/network/network_interface.cpp:1041-1086`).
  - Add multi-packet bundle segmentation (`BundleSplitter` / multi-packet `ElementEncoder`) so large variable messages (notably `Account.update` if used, large `resourceFragment` batches, or `createBasePlayer`) split cleanly into sequential `FLAG_IS_FRAGMENT | FLAG_HAS_SEQUENCE | FLAG_IS_RELIABLE` packets with `fragBegin`/`fragEnd` footers.

### 3. Game Data Crate (`crates/wot065-data`)

Reuse existing builders unchanged where already accurate (`wot065_data::catalog::load`, `full_sync_pickle`, `showgui_ctx_pickle`, `shop_cache_pickle`, `dossier_sync_pickle`, `stream_bundle`), and verify against `AccountSyncData.py` + `Shop.py` + `DossierCache.py`:

- Ensure `serverSettings` Python dict for `createBasePlayer` carries `'vivoxDomain': ''` (serialized via `pickle::dumps` as `STRING`).
- Verify `full_sync_pickle` with `FullSyncOptions { unlock_all: true, ..Default::default() }` (`prevRev: 0, rev: 1`, all `130` vehicles unlocked with modules and in `inventory[VEHICLE]['compDescr']`, `slots: 200`, `credits`, `gold`, etc.) works cleanly through `AccountSyncData.__onSyncComplete` when delivered via `RES_STREAM`.

### 4. BaseApp Server (`crates/wot-baseapp`)

Depend directly on `bw201`, `wot065-data`, `tokio`, `clap`, `serde_json`. Expose binary `wot-baseapp` (`crates/wot-baseapp/src/main.rs`) matching `src/main.rs`:
`--bind <addr> --session-file <path> --game-dir <path> [--account-diff <path>] [--showgui-ctx <path>] [--shop-cache <path>] [--dossier-sync <path>]`.

#### Wire Lifecycle & Channel State Machine

1. **Startup data prep**:
   - Load `wot065_data::catalog::load(&game_dir)` once at startup (`130` vehicles).
   - Build (or load from optional override path) `full_sync_pickle`, `shop_cache_pickle`, `dossier_sync_pickle`, and precompute `(zlib, desc)` pairs with `wot065_data::account::stream_bundle`.
2. **UDP Receive & Encryption Demux**:
   - Bind Tokio `UdpSocket` on `--bind`.
   - Maintain per-`SocketAddr` peer state + latest `LoginSession` reloaded from `--session-file` on unknown/new login packets.
   - Note on `baseAppLogin` encryption (`baseapp_login_request.cpp:71`, `server_connection.cpp:109`, `packet_receiver.cpp:237-240`): `ServerConnection` instantiates `EncryptionFilter` using `encryption_key` BEFORE sending `baseAppLogin`. Therefore `BaseAppExtInterface::baseAppLogin` and its reply (and every packet on the external channel thereafter) are encrypted with `BlowfishKey::packet_encrypt` / `packet_decrypt`. On packet receipt from an unknown `SocketAddr`, attempt `BlowfishKey::packet_decrypt` using the loaded `LoginSession` (reloading `--session-file` from disk if needed so a fresh login is picked up immediately); if decryption succeeds and `baseAppLogin` (`0x00` request) carries matching `session_key`, send an encrypted on-channel reliable reply (`0xFF`, echoed `reply_id`, `session_key: u32`) and establish the peer channel state.
   - Maintain per-`SocketAddr` peer channel state (`in_seq`, `out_seq`, `acks_to_send`, outbound reliable retransmit queue, inbound fragment buffer): validate channel sequence numbers (`FLAG_HAS_SEQUENCE_NUMBER`), record reliable packet receipts for piggybacked/dedicated `FLAG_HAS_ACKS`, handle fragmented inbound bundles (`FLAG_IS_FRAGMENT`), and retransmit unacked outbound reliable packets on timer.
3. **Handshake -> Player Creation -> Hangar Sync**:
   - Handle `BaseAppExtInterface::authenticate` (`0x01`, `session_key: u32`) and `enableEntities` (`0x0A`, `dummy: u8`):
     - Send reliable bundle with:
       - `ClientInterface::authenticate` (`0x00`, `session_key: u32`)
       - `ClientInterface::bandwidthNotification` (`0x01`, e.g. `1_000_000`) & `ClientInterface::updateFrequencyNotification` (`0x02`, `10` Hz) & `ClientInterface::setGameTime` (`0x03`, `0`)
       - `ClientInterface::createBasePlayer` (`0x05`, VAR 2): `playerID: i32`, `entityTypeID: u16 = 0` (`Account`), `name` (`STRING`), `normalizedName` (`STRING`), `serverSettings` (`PYTHON` = `STRING` pickled `{'vivoxDomain': ''}`).
       - `Account.showGUI` (`0x80 | 16`): `playerID: i32`, `ctx` (`STRING` pickled `{'databaseID': 1, 'serverUTC': now}`).
4. **`Account` Method Dispatch & Resource Streaming**:
   - Parse `Account.doCmdInt3` (exposedId `4`, element `0xC4`, NO `entityID` prefix: `requestID: i16, cmd: i16, i1: i32, i2: i32, i3: i32`):
     - `CMD_SYNC_DATA (100)`: reconstruct 64-bit `client_rev = ((i1 as u32 as u64) << 32) | (i2 as u32 as u64)`.
       - If `client_rev == 0`: send `Account.onCmdResponse` (`0x82`: `playerID, requestID, RES_STREAM=1`) followed by `resourceHeader` (`0x37`: `rid=requestID as u16`, `description=full_sync_desc`) and chunked `resourceFragment` (`0x38`: `rid=requestID as u16, seq: u8, flags: u8`, `flags=1` only on final/single chunk, `chunk`) over reliable Mercury packets.
       - If `client_rev == 1` (client's follow-up check after `RES_STREAM`): send `Account.onCmdResponseExt` (`0x83`: `playerID: i32, requestID: i16, resultID: i16 = RES_SUCCESS (0), ext: STRING = pickled {'rev': 1, 'prevRev': 1}`).
     - `CMD_SYNC_SHOP (300)` / `CMD_SYNC_DOSSIERS (600)`: send `Account.onCmdResponse` (`RES_STREAM=1`) + `resourceHeader`/`resourceFragment` (`flags=1` on final/single chunk) with `shop_cache` / `dossier_sync` stream bundles.
     - Other commands (`CMD_SET_LANGUAGE (1000)` via `doCmdStr`, `CMD_REQ_SERVER_STATS (501)`, `CMD_REQ_QUEUE_INFO (502)`, `CMD_REQ_ARENA_LIST (500)`, `CMD_REQ_PREBATTLES (520)`, `CMD_REQ_PLAYER_DOSSIER (503)`, `CMD_REQ_PLAYER_CLAN_INFO (504)`, `CMD_EQUIP_*`): ignore when `requestID == REQUEST_ID_NO_RESPONSE (102)` (`100` prebattles / `101` roster / `102` no-response per `common/streamIDs.py`); for `CMD_REQ_SERVER_STATS` reply with `Account.receiveServerStats (0x80 | 18)` if appropriate, and otherwise reply `onCmdResponse(requestID, RES_FAILURE=-1)` cleanly for unimplemented UI calls (`requestID >= REQUEST_ID_UNRESERVED_MIN (120)`) without closing the channel.
   - Also decode `doCmdStr` (`0xC3`: `requestID: i16, cmd: i16, s: STRING`), `doCmdInt4` (`0xC5`), `doCmdInt2Str` (`0xC6`), `doCmdIntArr` (`0xC7`), `BaseAppExtInterface::requestEntityUpdate` (`0x09`), and `BaseAppExtInterface::disconnectClient` (`0x0C`) cleanly (and handle empty/keepalive UDP channel packets) so auxiliary calls never break stream framing.

### 5. Top-level CLI Supervisor (`src/main.rs`) & Docs

- Update `src/main.rs` log line from `"base app : tcp {}"` to `"base app : udp {}"`.
- Verify key handling between `deploy.rs` (`keys/loginapp_priv.pem`) and `.scratch/wot-keys/loginapp_priv.pem` (client already uses `.scratch/wot-keys/loginapp_pub.pem`; ensure `wot-server serve` works seamlessly or defaults cleanly / copies matching keys if appropriate without breaking `restore-client`).
- Fix `wiki/analyses/rust-server-architecture.md` port label (`TCP 20017` -> `UDP 20017`).

---

## Verification Criteria

1. **Decompilation**: Zero syntax failures (`py27/inst/python.exe`) across `res/scripts` (`220` files including recovered `client/post_processing/__init__.py`) and `res_bw/scripts/{client,common}` non-`Lib` (`136` files); `.dis.txt` files non-empty.
2. **Rust unit & protocol tests**: `cargo test --workspace` passes with tests covering big-endian `LoginReplyRecord`, Blowfish packet filter, Mercury packet footer + multi-packet bundle fragmentation/reassembly, `createBasePlayer(entityTypeID=0)`, `resourceHeader`/`resourceFragment`, and simulated login → sync state machine.
3. **Live E2E client verification**: Starting `wot-server serve` against `C:\Users\qwerty\Desktop\World_of_Tanks` and launching `WorldOfTanks.exe` allows logging in, completes `AccountSyncData` / `Shop` / `DossierCache` synchronization, enters the hangar, and shows all 130 vehicles in the garage with zero unhandled disconnect/protocol errors in server logs or `python.log`.
