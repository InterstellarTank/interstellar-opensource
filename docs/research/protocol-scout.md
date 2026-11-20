# WoT 0.6.5 network protocol — actionable reference for a Rust server

Task-1 deliverable. Detailed wire specs:

* `docs/research/bw201-login-wire.md` — **loginapp hop, byte-exact** (login request/reply, RSA, Blowfish, LogOnParams, Mercury framing, test vectors).
* `docs/research/bw201-baseapp-wire.md` — **baseapp hop** (handshake, interface tables, entity messages/properties, createBasePlayer, resource streams, garage path).
* `.scratch/research/protocol-scout/bw201-interfaces.json` — machine-readable interface/ID tables.
* Prior-art source copies: `.scratch/research/protocol-scout/repos/` (BW 2.0.1 full source, wg-toolkit-rs, cyberjois 0.9.7 client+scripts, tuxedo 0.9.22 mod, Fedar459 0.6.2.8 offline hangar, juho debugserver, BWXML packer).
* Raw evidence: `.scratch/research/protocol-scout/wot065-exe-strings.txt` (52 669 strings from `WorldOfTanks.exe`).

**TL;DR verdict — the 0.6.5 client speaks unmodified BigWorld 2.0.1 `lib/connection` protocol with WG config on top.** Build the server as: Mercury UDP transport (BW 2.0.1 framing) → loginapp (`login`/`probe`) → baseapp (`baseAppLogin`/`authenticate` + client interface 0..57 + entity messages) → create `Account` base player → answer `CMD_SYNC_DATA` with a zlib(pickle) Resource stream → client builds the hangar locally. wg-toolkit-rs (MIT, Rust) is the right skeleton for transport/codecs but its **login codec and element tables are the modern (v2.3) protocol and must be replaced/regenerated** for 0.6.5.

## 1. Target environment facts

| Fact | Value | Evidence |
|---|---|---|
| Game | World of Tanks v0.6.5 (EU, SD), client 65.95961 | `version.xml` in game root; `wot_0.6.5_readme.txt` |
| Client Python | 2.6.4 bytecode, compiled 2011-05-26 | decompiled headers `.scratch/research/decompiled/client/**.py` |
| Engine | BigWorld 2.x; exe build path `d:\WoT\game_releases\beta_6.5\programming\sources\bigworld\src\...` | exe strings |
| Login endpoint | `W.o.T.login-master.worldoftanks.com:20016` | packed `res/scripts_config.xml` (magic `45 4E A1 62`), strings `login`, `host`, `url`, `public_key_path`, `loginapp_wot.pubkey`, `updateUrl` |
| Login public key | `res/loginapp_wot.pubkey` — 451-byte **plain PEM** `-----BEGIN PUBLIC KEY-----`, RSA-2048 | read directly from game dir |
| Login trigger | `client/gui/Scaleform/Login.py` reads `scripts_config.xml` section (`url` list, `public_key_path`), calls `ConnectionManager.connect(host,user,password,publicKey)` → `BigWorld.connect(host, login, watcher)` | `.scratch/research/decompiled/client/ConnectionManager.py`, `gui/Scaleform/Login.py:140-155,397-405` |
| Login params | `LoginInfo{username, password, inactivityTimeout=40, publicKeyPath}` — exactly the attributes `BigWorld.connect` C++ reads | `ConnectionManager.py:70-86`; `common/constants.py CLIENT_INACTIVITY_TIMEOUT = 40`; BW `client/connection_control.cpp` (`GET_KEYWORD_ARG(username/password/publicKeyPath/inactivityTimeout)`) |
| Client interface message tables | embedded as strings in `WorldOfTanks.exe` in registration order (= ids) | `.scratch/research/protocol-scout/wot065-exe-strings.txt` idx 24382-24451 (`BaseAppExtInterface`, `ClientInterface`), 24375-24470 |

### 1.1 Evidence that 0.6.5 == BigWorld 2.0.1 (not just "similar")

1. The exe contains BigWorld 2.0.1 `lib/connection` class-name RTTI: `?AVLoginHandler@@`,
   `?AVLoginAppLoginRequest@@`, `?AVBaseAppLoginRequest@@`, `?AVRetryingRequest@@`,
   `?AVRSAStreamEncoder@@`, `BaseAppExtInterface` vtable, plus `baseAppLogin`, `authenticate`,
   `restoreClient`, `switchBaseApp`, `switchInterface`, `loggedOff` and the whole
   `LOGIN_REJECTED_*` enum from `lib/connection/log_on_status.hpp`.
2. The exe message-name cluster for `BaseAppExtInterface` is exactly BigWorld 2.0.1's
   declaration order and count (14 entries), and the `ClientInterface` cluster is exactly
   BW 2.0.1's 58-entry order (100+ names including the 32 `avatarUpdate*` variants, ending
   `voiceData, restoreClient, switchBaseApp, resourceHeader, resourceFragment, loggedOff`).
   Compare with `repos/bw201/src/lib/connection/{baseapp_ext,client}_interface.hpp` — 1:1.
3. The client has only `login` + `probe` on the LoginInterface (no `ping`, `challengeResponse`,
   `mtuProbe`, no `context`, no post-2020 challenge (`cuckoo_cycle`)) — all strings absent from
   the exe. That is exactly BW 2.0.1's 2-message LoginInterface, not the modern one.
4. The decompiled login code (`ConnectionManager.py`) builds the object with exactly the four
   attributes `ConnectionControl::connect` in BW 2.0.1 extracts, and BW 2.0.1's
   `ServerConnection::logOnBegin` is the only client-side code that constructs `LogOnParams`.
5. Login public key file name/layout (`loginapp.pubkey` default; WG sets `publicKeyPath` to
   `loginapp_wot.pubkey`) matches `ConnectionControl::initLogOnParamsEncoder` +
   `PublicKeyCipher::setKeyFromResource` (PEM via OpenSSL, `libeay32.dll` present in game dir).

## 2. Prior-art survey (what exists, what is actually usable)

| Project | Language | What it is | Usable for us? |
|---|---|---|---|
| **v2v3v4/BigWorld-Engine-2.0.1** (public GitHub mirror of the engine source drop; 412 MB, `src/` 711 files used) | C++ | Complete client+server BigWorld engine source, incl. `lib/connection`, `lib/network`, `server/loginapp`, `server/baseapp`, `lib/entitydef`, client entity code | **Authoritative protocol reference.** Research-only: BigWorld is commercial code; do not vendor C++ sources into the product — reimplement from behaviour/spec (our Rust code is independent). |
| **v2v3v4/BigWorld-Engine-1.9.1** (2008) | C++ | Older engine; useful to diff version-gated protocol features (LOGIN_VERSION history in `login_interface.hpp` comments) | Reference for "which protocol version has what" |
| **theorzr/wg-toolkit-rs** (MIT, active 2026) | Rust | Modern WoT (v2.3.1.3) Core/BigWorld protocol library + server emulator: packed-XML codec, script-def loader, Mercury packet/bundle/element codec, Blowfish+RSA filters, login app, base app with dynamic entity dispatch, hangar/vehicles, docs `doc/APPS.md`, `doc/HANGAR_LOADING.md` | **Best Rust skeleton**: reuse `net` (framing/bundle/filters), `res`/`pxml` (packed XML), `script` (defs), `app` dispatch pattern; **rewrite** login codec + element id tables for 0.6.5 (see §3), and adapt transport (BW 2.0.1 has no 4-byte packet prefix; per-message length styles are BW's, not modern CALLBACK tables). |
| **cyberjois/private-wot-server** ("Locally hosted WoT offline server for 0.9.7") | Python (client scripts copy) | Actually a **0.9.7 client distribution + BigWorld engine scripts** (`bigworld/`, `wot/`), no server executable and no releases. Contains **`bigworld/loginapp.pubkey` and matching `bigworld/server/loginapp.privkey`** (RSA-2048 PEM) plus `bigworld/server/bigworld.key` (128-byte Blowfish-ish key), `development_defaults.xml`/`production_defaults.xml` (`loginApp` config flags). | Not server code. But it proves the "swap pubkey + point client at localhost" model (it ships the keypair in-tree for 0.9.7) and gives us BW config defaults. The 0.9.7 keypair is **not** the 0.6.5 one (different modulus), so we still ship our own key. |
| **the-tuxedo-cat/wot-offline-server** (0.9.22) | Python mod | Client-side emulation of the server as a `res_mods` mod: `offhangar` (initialServerSettings.json), `AvatarServer.py`, `VehicleMover`, `DumbPhysics`. Hides the network rather than speaking it. | Useful for the *shape* of account/garage data and client expectations; not for wire protocol. |
| **Fedar459/WoT-Legacy-Offline-Simulation-0.6.2.8** | Python | Offline hangar for **0.6.2.8** by patching `Login.py` to route to `Offline/Manager.py`; ships `Login.pyc` (patched) + `Manager.py` (262 KB) + battle simulation. Explicitly "does not emulate network protocols". | Proves the era's offline approach and gives a 0.6.2.8 client pointer (`archive.org` 0.06.02.08 EU SD). Not a server. |
| **juho-p/wot-debugserver**, **chirimenmonster/wotmods-replserver** | Python mod | Injects a TCP REPL into the live BigWorld client (`BigWorld.player()`, entity internals) | Debugging tool for experimenting with the running client if needed. |
| **wot-offline.com / wot-offline.global** | community | Archive of unmodified WoT clients; their offline-server effort targets **0.8.2** (leaked BW server binaries era) via `res_mods`. No 0.6.5 server exists publicly. | Client archive + evidence that no 0.6.5 emulator exists (we are first). |
| **cyberjois/BWXML** | C++ | BigWorld packed-XML reader/writer (0.9.x era), magic `EN A1 62` identical to 0.6.5 files | Reference for the packer; gamedata agent is writing our Python/Rust packer. |

No existing 0.6.5 (or 0.6.x) network server/emulator was found on GitHub, archive.org, forums or
DuckDuckGo (RU/EN). The known private-server scene skipped the period by shipping leaked BigWorld
`bwserver.exe` binaries (0.8.x/0.9.x) – not applicable to 0.6.5.

## 3. Protocol summary (0.6.5 vs modern wg-toolkit — the deltas that matter)

Transport (BW 2.0.1, LOGIN_VERSION 56):
* UDP; packet = `u16 flags` (LE) + elements; max 1472 B; **no 4-byte prefix**
  (wg-toolkit `net/packet.rs` implements the modern prefix — strip it).
* Element = `u8 id` + optional LE length (`lengthParam` bytes; BW fixed/variable/CALLBACK styles).
  Named ids 0..127; `0xFF` REPLY; entity method/property ids 128..254.
* Requests: `[id][len][i32 reply_id][u16 next_request_offset=0][args]`, footer
  `u16 firstRequestOffset`; replies: element `0xFF` + `u32 len` + echoed reply id.
* Flags: HAS_REQUESTS..HAS_CUMULATIVE_ACK (see login doc §1); checksums off in WoT;
  Blowfish packet filter on BaseApp channel (magic `0xdeadbeef` + wastage trailer), Blowfish
  **stream** filter for the login reply (no trailer), both with XOR-previous-plaintext chaining.

Login impl (BW 2.0.1 `lib/connection`):
* `u32 LOGIN_VERSION` + RSA-OAEP-SHA1(LogOnParams), 214-byte clear chunks → 256-byte cipher
  chunks; no encryption flag byte (modern has one — do not copy wg-toolkit's
  `LoginRequest` codec verbatim).
* `LogOnParams` = flags `u8`, username/password/encryptionKey as BigWorld **packed-int-prefixed
  strings**, optional 16-byte MD5 digest, `u32 nonce`.
* Reply success = `u8 status 1` + Blowfish(Address ip/port/salt + u32 sessionKey).
* Then BaseApp: `baseAppLogin(u32 loginKey, i32 attempt)` request → reply `u32 newKey` →
  `authenticate(newKey)` both ways.

IDs/orders (0-based, BW 2.0.1 declaration order — see JSON for full tables):
* `BaseAppExtInterface`: 0 `baseAppLogin` (VAR2, u32 sessionKey + i32 attempt),
  1 `authenticate` (FIXED4), 2-5 avatar updates, 6-7 physics acks, 8 `switchInterface` (FIXED0),
  9 `requestEntityUpdate` (VAR2), 10 `enableEntities` (FIXED1), 11 `restoreClientAck` (FIXED4),
  12 `disconnectClient` (FIXED1), 13+ entity messages (128..254).
  (Modern wg-toolkit: 0 login, 1 `pingDatacenter`, 2 `authenticate`, plus NRL/base ranges — do not use.)
* `ClientInterface`: 0 `authenticate`(F4), 1 `bandwidthNotification`(F4), 2 `updateFrequencyNotification`(F1),
  3 `setGameTime`(F4), 4 `resetEntities`(F1), 5 `createBasePlayer`(V2), 6 `createCellPlayer`(V2),
  7 `spaceData`(V2), 8 `createEntity`(V2), 9 `updateEntity`(V2), 10 `enterAoI`(F5),
  11 `enterAoIOnVehicle`(F9), 12 `leaveAoI`(V2), 13 `tickSync`(F1),
  14 `relativePositionReference`(F1), 15 `relativePosition`(F12), 16 `setVehicle`(F8),
  17-48 the 32 `avatarUpdate*` variants, 49 `detailedPosition`, 50 `forcedPosition`,
  51 `controlEntity`, 52 `voiceData`, 53 `restoreClient`, 54 `switchBaseApp`,
  55 `resourceHeader`(V2), 56 `resourceFragment`(V2), 57 `loggedOff`(F1), 58+ entity messages.
  (Modern has ~25 extra WG messages: dummyPacket, spaceProperty, geometry mappings,
  `selectEntity`/`selectPlayerEntity`, NRL*, cell-suspension, etc. — none exist in 0.6.5.)
* Entity method/property framing: ids `0x80|exposedID` / `0xC0|exposedID` with identical bits
  but opposite meaning per direction (see baseapp doc §3); payload starts `i32 entityID`.
  `exposedID` comes from the **0.6.5** entity defs, never from modern tables.

## 4. Garage data flow (what makes the hangar appear)

Chain (all confirmed against 0.6.5 decompiled client + `AccountCommands.pyc` disassembly):

1. BaseApp creates the `Account` base player (`createBasePlayer`: `i32 entityID; u16 typeID;
   name; serverSettings; …` positional client properties) and calls `Account.showGUI(pickled
   ctx)`.
2. Client `PlayerAccount.onBecomePlayer` (`decompiled/client/Account.py:59-77`) starts
   `AccountSyncData`, `Inventory`, `Stats`, `Trader`, `Shop`, `DossierCache` sync helpers.
3. `AccountSyncData` → `SyncController.request` → `Account._doCmdInt3(CMD_SYNC_DATA=100,
   revHi, revLo, 0)` as a **base entity method** (`0xC0|exposedID` of `doCmdInt3`).
4. Server answers `ClientCommandsPort.onCmdResponse(requestID, RES_STREAM=1)`; the client
   subscribes stream `rid = requestID` (`Account._subscribeForStream`).
   `AccountCommands`: RES_SUCCESS=0, RES_STREAM=1, RES_CACHE=2; CMD_SYNC_DATA=100, CMD_SYNC_SHOP=200…
5. Server sends `ClientInterface.resourceHeader(rid)` + `resourceFragment(rid, seq, flags)`
   chunks; concatenated bytes = **zlib-compressed Python pickle** of the sync diff dict
   (client `SyncController`: `zlib.decompress` + `cPickle.loads`).
6. `Account._update` fans the dict out to `inventory.synchronize`, `stats.synchronize`,
   `shop.synchronize`, `dossierCache.synchronize`, …; the hangar then renders **100 % locally**
   from that cache (same architecture the wg-toolkit HANGAR doc confirmed for v2.3: no
   `Vehicle`/`HangarVehicle` entity traffic is needed to show the garage; `HangarVehicle` is a
   purely local client entity).
7. `onCmdResponseExt(rid, resultID, pickleExt)` acks revisions (`prevRev`/`rev`).

Garage data needed in the pickle: `stats` (credits, gold, freeXP, slots, …) and `inventory`
(category `1` = vehicle instances keyed by `vehInvID`, each with `compDescr` binary compact
descriptor, `crew`, `shells`, `eqs`, …; see wg-toolkit `doc/HANGAR_LOADING.md` for the fully
decoded modern format — 0.6.5's item/descriptor encoding must be regenerated from 0.6.5
`scripts/common/items/` + `item_defs`, which the client-reverse/gamedata agents own).
The 0.6.5 `AccountCommands.pyc` constants and 0.6.5 client helpers are the authoritative shape;
modern field names mostly carry over but 0.6.5 has fewer features.

## 5. Deployment: pointing the retail 0.6.5 client at our server

Requirements:
1. **Our RSA keypair replaces the loginapp public key.** `res/loginapp_wot.pubkey` is a loose
   plain-PEM file (not packed): drop in a PEM `PUBLIC KEY` (X.509 SubjectPublicKeyInfo) for our
   generated RSA-2048 key, keep the corresponding private key on the server. The client only
   uses it via `PublicKeyCipher::setKeyFromResource` (accepts PEM from ResMgr). No signature of
   the file itself is checked.
2. **The client must connect to 127.0.0.1:20016.** `scripts_config.xml` is a packed BigWorld
   XML file (magic `45 4E A1 62`, 594 bytes) containing the `login` section:
   `url = W.o.T.login-master.worldoftanks.com:20016`, `public_key_path = loginapp_wot.pubkey`,
   `updateUrl = http://game.worldoftanks.com/update_objects/fake_model.model`.
   Options, cheapest first:
   * **hosts-file redirect** (no game-file edit): map `W.o.T.login-master.worldoftanks.com` to
     `127.0.0.1` (the port 20016 is already in the config; the client is not using TLS for the
     loginapp). Only the pubkey swap is needed; reversible; no re-packing.
   * **paths.xml override** (no packed-file edit): `paths.xml` at game root is loose plain XML
     listing `./res`, `./res_bw`. BigWorld searches its resource paths in order; add a
     `./res_local` path *before* `./res` and place `res_local/scripts_config.xml` (packed with
     the gamedata agent's packer, or possibly plain XML if the loader accepts text XML — verify)
     + `res_local/loginapp_wot.pubkey` there. Requires confirming BW 2.0.1 path precedence
     (`lib/resmgr/bwresource.cpp` — earlier path entries win for resource lookup; verify
     `Paths/Path` ordering semantics before relying on it).
   * **repack `res/scripts_config.xml`** in place with the new URL: works for sure if the packer
     reproduces the format, but it edits the install and must be reversible (keep a backup).
3. **No other anti-tamper was found in this path**: the client does not hash its res files
   before login (no such strings/functions in the exe's connection code); the launcher/updater
   is a separate process and not involved when starting `WorldOfTanks.exe` directly. The exe
   command line was not found to contain a server-address override (BigWorld app config comes
   from `res/engine_config.xml`; the address comes from `scripts_config.xml` in Python). Treat
   that as "no CLI override" until someone disassembles `App::init` command-line parsing.
4. After login the client will try HTTP things (updateUrl, WGNI/WoT API calls, telemetry). For a
   minimal garage they should fail silently; if the client hangs on a blocking request, stub the
   URL in `scripts_config.xml` too (e.g. point `updateUrl` at 127.0.0.1:1) / block it.

## 6. Rust implementation strategy (recommended)

**Base:** fork/vendor `theorzr/wg-toolkit-rs` (MIT) under `vendor/` (Lead is doing this), keep its
`util`, `pickle`, `pxml`, `res`, `script` crates (packed XML magic matches 0.6.5; verify the
0.6.5 def schema loads — 0.6.5 defs are simpler, no extension/component `<ofEntity>` machinery),
and reuse its `net::element`/`bundle`/`codec`/`filter` design. Adapt the transport:

1. **`net` (transport):** parameterise by a `ProtocolVersion` describing (a) no 4-byte packet
   prefix (BW 2.0.1), (b) element length styles from BW 2.0.1 `InterfaceElement` (fixed /
   variable N / callback), (c) BW packet flags and footer order, (d) no cuckoo challenge.
   The Blowfish/RSA filters are already correct (`net/filter/*` matches
   `public_key_cipher.cpp`/`encryption_filter.cpp` byte-for-byte; only the packet filter's
   `0xdeadbeef`+wastage trailer must be implemented — login stream filter has none).
2. **LoginApp (`app/login`):** reimplement `element.rs` for the 2-element BW 2.0.1 interface and
   the login wire spec (`docs/research/bw201-login-wire.md`). In-memory account check for now
   (any username/password accepted; optionally validate a fixed hash). Reply with the current
   BaseApp external address (config) and a random session key; keep
   `(client_addr -> blowfish_key, session_key, username)` in shared state for the BaseApp.
3. **BaseApp (`app/base`)**: implement the handshake + interfaces from
   `bw201-baseapp-wire.md`; entity dispatch driven by a **0.6.5-specific generated table**:
   `entities.xml` order → `entityTypeID`; `Account.def`/`Avatar.def` → exposed ids/arg types.
   Use the wg-toolkit `script::defs` loader on unpacked 0.6.5 defs (or generate a Rust table at
   build time; both fine). Minimal handlers: `baseAppLogin`, `authenticate`, entity base
   methods `doCmdInt*` on Account, client methods `showGUI`/`onCmdResponse`/`onCmdResponseExt`/
   `onStreamComplete`.
4. **Account data:** implement `CMD_SYNC_DATA` answering from a JSON/DB "garage fixture":
   build the diff dict (rev, stats, inventory) → Python pickle (use wg-toolkit `pickle` ser) →
   zlib → chunk into `resourceFragment`. The Lead's `account-data` agent owns the dict shape
   (0.6.5 item descriptors from gamedata).
5. **Milestones / acceptance**
   * M0 `cargo test`: unit tests for packed ints, LogOnParams round-trip, login packet layout,
     RSA decrypt, Blowfish both directions, reply framing (vectors in login doc §5).
   * M1 loginapp accepts the real 0.6.5 client up to "connecting to BaseApp" (client stops at
     LOGIN_REJECTED_* or advances) — validate with a local UDP capture.
   * M2 BaseApp handshake succeeds; client shows login progress / reaches `createBasePlayer`.
   * M3 `showGUI` + `CMD_SYNC_DATA` stream answered; client enters hangar.
   * M4 garage lists all seeded tanks; `--give-vehicle`/fixture drives the inventory.
6. **Testing without the game:** BW-source-derived fixtures; a small "fake" client harness in
   Rust implementing the client side of the login+baseapp handshake (reuses the same codecs,
   gives CI coverage); a pcap/hex golden test for the first client packet once available.

## 7. Risk register

| Risk | Impact | Mitigation |
|---|---|---|
| 0.6.5's client LOGIN_VERSION value unknown (likely 56, could be 52-56) | Wrong assumptions about width/ordering in rarely-used parts | Read the u32 from the first packet at runtime (log it). No version-dependent framing in the login path; Mercury 2-byte header is certain for >=43. |
| BW 2.0.1 source is a 2012 drop; WG's `beta_6.5` engine branch may differ in small details | Some parser edge cases | The interface table match in the exe is strong evidence; verify first live capture. |
| First BaseApp channel packet (`FLAG_CREATE_CHANNEL`/`INDEXED_CHANNEL` handshake) may need exact channel establishment replies | Login succeeds but BaseApp connect stalls | Audit `lib/network/channel.cpp` + capture; wg-toolkit has a modern channel implementation to port from. |
| Entity-def/property order for 0.6.5 `Account` differs from modern | Client exception on createBasePlayer / garage empty | Use unpacked 0.6.5 defs (gamedata) + client-reverse's confirmed `name`/`serverSettings`; client logs exceptions to `python.log` – easy to iterate. |
| `onBecomePlayer` may need a cell player for a base-only Account | ShowGUI/garage never triggered | Test early; can send a dummy `createCellPlayer` with an empty space if needed (spaceData is client-tolerated). |
| Packed `scripts_config.xml` replacement | Client still dials WG | Use hosts redirect first (zero game edits); pubkey swap is required regardless. |
| Commercial BigWorld license | Legal | Do not copy C++ sources into product; implement from observable behaviour/spec. wg-toolkit (MIT) may be vendored with attribution. |
| Client side-blocking HTTP (update/WGNI) | Login UI hangs | Point `updateUrl`/web endpoints to dead local addresses via config override; check `python.log`/`Influx_PS.log` for blocked requests. |

## 8. Open questions / next experiments

1. **Live capture**: run the 0.6.5 client with our pubkey + hosts redirect and capture the first
   UDP datagram; assert `flags=0x0001`, element id 0, `u32 LOGIN_VERSION`, then decrypt with our
   private key and re-encode with the Rust codec. This single test removes most remaining risk.
2. Verify BW 2.0.1 `Paths/Path` precedence for a `paths.xml` override (and whether the resource
   loader accepts plain-text XML sections for `scripts_config.xml`), else use the packer.
3. Confirm the 0.6.5 `Account.def` client-visible property list/order and client/base method
   exposed ids from the unpacked defs (gamedata + client-reverse) and diff against this doc.
4. Confirm whether 0.6.5 loginapp ever emits `LOGIN_REJECTED_BAD_DIGEST` checks it can't skip
   (we will never send it) and whether the client requires any `probe` support (it uses
   ServerDiscovery/`BigWorld.serverDiscovery` with its own UDP probe path — likely not needed
   for login: `ConnectionManager` only searches servers for the server list UI).
5. Channel establishment details for the BaseApp connection (see risk table).
6. Whether the client's `ClientInterface` expects any of the optional early messages
   (`bandwidthNotification`, `updateFrequencyNotification`, `setGameTime`, `enableEntities`)
   before it will run `onBecomePlayer`; add them incrementally while watching `python.log`.

## 9. Source map (local, this workspace)

```
.scratch/research/protocol-scout/
  wot065-exe-strings.txt                  # 0.6.5 WorldOfTanks.exe ASCII strings (52 669)
  bw201-interfaces.json                   # machine-readable tables (this task)
  repos/bw201/                            # BigWorld-Engine-2.0.1 (sparse src/lib/connection,
                                          #   src/lib/network, src/lib/entitydef, src/lib/cstdmf,
                                          #   src/server/loginapp, src/server/baseapp, src/client)
  repos/wg-toolkit-rs/                    # MIT Rust WG toolkit (modern protocol; reusable core)
  repos/cyberjois-097/                    # 0.9.7 client + BigWorld server-side scripts/keys
  repos/tuxedo-0922/                      # 0.9.22 client-side offline server mod
  repos/fedar459-0628/                    # 0.6.2.8 offline hangar
  repos/juho-debugserver/                 # in-client REPL
  repos/bwxml/                            # packed-XML C++ tool (cyberjois/BWXML)
docs/research/bw201-login-wire.md         # login hop, byte-exact
docs/research/bw201-baseapp-wire.md       # baseapp hop, entity/garage layouts
```

Upstream URLs: `https://github.com/v2v3v4/BigWorld-Engine-2.0.1`,
`https://github.com/theorzr/wg-toolkit-rs`, `https://github.com/cyberjois/private-wot-server`,
`https://github.com/the-tuxedo-cat/wot-offline-server`,
`https://github.com/Fedar459/WoT-Legacy-Offline-Simulation-0.6.2.8`,
`https://github.com/juho-p/wot-debugserver`, `https://github.com/cyberjois/BWXML`,
`https://wot-offline.com`.
