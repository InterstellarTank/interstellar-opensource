# BigWorld 2.0.1 BaseApp wire layouts (for WoT 0.6.5 clients)

Companion of `docs/research/bw201-login-wire.md`. Machine-readable tables:
`.scratch/research/protocol-scout/bw201-interfaces.json`. Source tree:
`.scratch/research/protocol-scout/repos/bw201/src/` (BigWorld 2.0.1). Client evidence:
`.scratch/research/decompiled/` (0.6.5 Python) and
`.scratch/research/protocol-scout/wot065-exe-strings.txt`.

All integers little-endian. Element framing and the Mercury packet header are identical to
the login hop (see login doc §1) with these differences:

* After the first BaseApp packet the client talks over a **Channel**: packets carry
  `FLAG_IS_RELIABLE | FLAG_HAS_SEQUENCE_NUMBER | FLAG_ON_CHANNEL` and, while the channel is
  being created, `FLAG_CREATE_CHANNEL` (+ `FLAG_INDEXED_CHANNEL` footer with `ChannelID i32` +
  `ChannelVersion u32`); footers are packed backwards from the end of the packet
  (`lib/network/packet.hpp`, `channel.cpp`, `network_interface.cpp:883-1090`).
* The whole BaseApp channel is **Blowfish packet-filtered** with the client's session key
  (the one it sent inside LogOnParams; BW protocol >= 46 makes this mandatory). Packet filter
  format differs from the login *stream* filter: the encrypted blob is the packet body plus a
  `u32 0xdeadbeef` magic and a `u8 wastage` count, zero-padded to an 8-byte multiple
  (`EncryptionFilter::send/recv`, `lib/network/encryption_filter.cpp`). The client expects this
  on every packet after the first BaseApp login exchange; our server must apply the same
  filter per packet (same XOR-previous-plaintext block chaining).
* BaseApp address and the login session key come from the (decrypted) loginapp success reply
  (`docs/research/bw201-login-wire.md` §3, §4).
* Checksums are off by default (`HAS_CHECKSUM` 0x0100 only if configured); WoT clients run
  without them.

## 1. Handshake (client -> BaseApp), exact order

1. **Channel + `baseAppLogin` request.** Client creates a new external `Mercury::NetworkInterface`
   and `Channel` to the BaseApp (filter = its Blowfish session filter), then sends request:
   `BaseAppExtInterface::baseAppLogin`, id **0x00**, VARIABLE(2 bytes length).
   Args: `u32 sessionKey` (the key from the login reply), `i32 attempt` (0,1,2…).
   Wire payload = 8 bytes, LE. Element framing = request framing from login doc §1.3
   (reply_id, next_request_offset=0), packet flags include `HAS_REQUESTS`; plus channel flags.
   `BaseAppLoginRequest` retries by creating a new request object/channel, up to
   `MAX_BASEAPP_LOGIN_ATTEMPTS = 10` (`lib/connection/baseapp_login_request.cpp`,
   `login_handler.cpp`).
2. **Server reply**: Mercury REPLY (element 0xFF) whose payload is a single `u32 newSessionKey`
   (LE). The client stores it as `ServerConnection::sessionKey_`
   (`BaseAppLoginRequest::onSuccess`: `data >> sessionKey`). It also adopts that channel.
3. **`BaseAppExtInterface::authenticate`**, id **0x01**, FIXED 4 bytes: `u32 sessionKey`
   (same value the server just returned). The client primes *every* outgoing bundle with this
   message as `RELIABLE_PASSENGER` (`ServerConnection::primeBundle`,
   `lib/connection/server_connection.cpp:1407`).
4. **Server sends `ClientInterface::authenticate`**, id **0x00**, FIXED 4: `u32 key`
   (same value). The client compares it with `sessionKey_` and errors if different
   (`server_connection.cpp:1679`). This is "the server authenticated itself".
5. Server then creates the player entity (see §4) and drives `ClientInterface`.

> There are **two session keys**: `login_session_key` (loginapp -> client, presented in
> `baseAppLogin`) and `baseapp_session_key` (baseapp -> client in the `baseAppLogin` reply,
> then echoed by both sides in `authenticate`). Do not mix them up.

## 2. `BaseAppExtInterface` — client -> BaseApp (14 registrations, ids 0-based)

Source: `lib/connection/baseapp_ext_interface.hpp` (declaration order = registration order;
matches the 0.6.5 exe string table, indexes 24382-24395, exactly).

| id | name | len | args |
|---|---|---|---|
| 0x00 | baseAppLogin | VAR(2) | `u32 sessionKey; i32 attempt` (8 B), **request** |
| 0x01 | authenticate | FIXED 4 | `u32 key` |
| 0x02 | avatarUpdateImplicit | FIXED | `EntityID id(i32); Coord pos(12); YawPitchRoll dir(3); u8 refNum` |
| 0x03 | avatarUpdateExplicit | FIXED | `i32 spaceID; i32 vehicleID; Coord(12); YawPitchRoll(3); u8 onGround; u8 refNum` |
| 0x04 | avatarUpdateWardImplicit | FIXED | `i32 ward; Coord(12); YawPitchRoll(3)` |
| 0x05 | avatarUpdateWardExplicit | FIXED | `i32 ward; i32 spaceID; i32 vehicleID; Coord(12); YawPitchRoll(3); u8 onGround` |
| 0x06 | ackPhysicsCorrection | FIXED 1 | `u8 dummy` |
| 0x07 | ackWardPhysicsCorrection | FIXED 5 | `i32 ward; u8 dummy` |
| 0x08 | switchInterface | FIXED 0 | – |
| 0x09 | requestEntityUpdate | VAR(2) | `i32 entityID; i32[] eventNumbers` |
| 0x0A | enableEntities | FIXED 1 | `u8 dummy` |
| 0x0B | restoreClientAck | FIXED 4 | `i32 id` |
| 0x0C | disconnectClient | FIXED 1 | `u8 reason` |
| 0x0D + 128..254 | entityMessage | VAR(2) | entity method messages, see §3 |

`switchInterface`, `requestEntityUpdate`, `enableEntities`, `restoreClientAck` are login/
entity-cache machinery: a normal 0.6.5 hangar session used them in early BW, but the
baseapp-impl only needs them if the client sends them (it will send `enableEntities` as part of
entity initialisation — check `ConnectionControl`, `server_connection.cpp`). Log and ignore
unknown/irrelevant ones until the garage works.

## 3. Entity messages (methods and properties)

Dispatch source: `lib/connection/message_handlers.hpp` (`EntityMessageHandler`) and
`server_connection.cpp:826` (`handleEntityMessage`). The C++ handler does:

```cpp
pServConn->handleEntityMessage( header.identifier & 0x7F, data );
...
const int PROPERTY_FLAG = 0x40;
if (messageID & PROPERTY_FLAG) pHandler_->onEntityProperty(objectID, messageID & ~PROPERTY_FLAG, data);
else                           pHandler_->onEntityMethod(objectID, messageID, data);
```
and all entity messages first read `EntityID objectID (i32)`.

### 3.1 client -> server (BaseAppExtInterface.entityMessage, ids 128..254)

* Element id = `0x80 | (0x40 if base-entity else 0) | exposedID`:
  * Account/base-entity methods (WoT `Account` `<BaseMethods>`, e.g. `doCmdInt3`, `doCmd*`):
    **0xC0 | exposedID** (`ServerConnection::startProxyMessage`: "0x80 to indicate it is an
    entity message, 0x40 to indicate that it is for the base").
  * cell/Avatar methods: **0x80 | exposedID**.
* Payload: `i32 entityID` (0 = client's own avatar) then the method args from the entity def.
* `exposedID` = 0-based index of the method among the entity def's `<BaseMethods>` (or
  `<CellMethods>`) **in declaration order**, exposed methods only (all Base/Cell methods are
  exposed). BigWorld allows at most 62/63 per entity part before the "2-byte identifiers" /
  sub-slot encoding kicks in (`lib/entitydef/entity_method_descriptions.cpp:103-125`;
  comment in `interface_element.hpp`). 0.6.5 `Account` has well under 63 base methods, so the
  simple form applies.
* The wire args of `Account.doCmdInt3` are `i32 entityID; i32 requestID; i32 cmd; i32 i1;
  i32 i2; i32 i3` (the base method signature is `doCmdInt3(requestID, cmd, i1, i2, i3)`,
  see `decompiled/client/Account.py` `_doCmdInt3`/`__doCmd`). The entity defs (gamedata)
  define the exact exposed ids; the client's call order is:
  `self.base.doCmdInt3(requestID, cmd, *args)`.
* **First command after login**: `AccountSyncData.__sendSyncRequest` calls
  `_doCmdInt3(AccountCommands.CMD_SYNC_DATA=100, revHi, revLo, 0, proxy)`; `Inventory`,
  `Shop`, `DossierCache`, `Stats` follow with their own commands
  (`decompiled/client/AccountSyncData.py`, `SyncController.py`; constants from
  `res/scripts/common/AccountCommands.pyc` — see `bw201-interfaces.json` `account_commands_0_6_5`).
* Response path expected by the client: the server must call the **Account client methods**
  `onCmdResponse(requestID, resultID)` and `onCmdResponseExt(requestID, resultID, extPickleBytes)`
  and `onStreamComplete(id, data)` — these are `Account` `<ClientMethods>`; the exact
  exposed ids come from the unpacked `Account.def` / `entities.xml` (gamedata agent).
  Call them with `ClientInterface` entity method framing (§3.2).

### 3.2 server -> client (ClientInterface.entityMessage, ids 128..254)

* Element id = `0x80 | exposedID` for a **client method**; `0x80 | 0x40 | exposedID`
  (= `0xC0 | exposedID`) for a **client property update**. (Same 0x40 property flag as above,
  but here it is the server sending: `ServerConnection::handleEntityMessage` tests it.)
* Payload: `i32 entityID`, then method args / single property value.
* `exposedID` = 0-based index among `<ClientMethods>` of the entity (declaration order), or
  among the entity's client-visible properties (declaration order).
* WoT garage-critical client methods on `Account` (0.6.5): `showGUI(pickledCtx)`,
  `onCmdResponse(requestID, resultID)`, `onCmdResponseExt(requestID, resultID, pickleBytes)`,
  `onStreamComplete(id, data)`; `ClientCommandsPort` is a component held by the client-side
  `Account` and its methods are also invoked remotely by the server
  (`decompiled/client/Account.py:102-140,504`; constant table in `bw201-interfaces.json`).
  `Account.showGUI` is what the Lead's client-reverse reported as the trigger for the garage
  UI after `onBecomePlayer` → it takes a **pickled dict** (ctx: databaseID etc.).

> Property updates: BW 2.0.1 does not have a separate "entityProperty" element; a property
> change on the wire is an entityMessage whose masked id has bit 0x40 set, followed by
> `i32 entityID` + the new value. `createBasePlayer` carries *all* client-visible base
> properties positionally instead (see §4).

## 4. Player creation and property streams

### 4.1 `createBasePlayer` (ClientInterface id 0x05, VAR 2)

Client parser (`lib/connection/server_connection.cpp:1785`):

```cpp
EntityID playerID = 0;  stream >> playerID;          // i32 LE
EntityTypeID playerType = EntityTypeID(-1); stream >> playerType;  // u16 LE
pHandler_->onBasePlayerCreate(id_, playerType, stream);  // rest of element = properties
```
Payload: `i32 playerEntityID; u16 entityTypeID;` then the properties (see §4.3).
`entityTypeID` = 1-based index in `scripts/entities.xml`'s client-server entity declaration
order (for 0.6.5 `Account` is the first client-server entity, so likely 1 — **verify from
gamedata**). The client creates the local Python entity and calls its `__init__`; then it calls
`onBecomePlayer` **after the player has both base and cell parts** in stock BW
(`client/player.cpp`, `entity.cpp:1911`). For WoT's garage the Account is base-only; the
Lead's client-reverse found the observable contract is: create Account base player with
client-visible properties `name` and `serverSettings`, then call `Account.showGUI(pickled ctx)`.
If `onBecomePlayer` does not fire from a base-only player create, the server may need
`createCellPlayer` with a dummy space/entity — confirm against the real client early.

### 4.2 `createCellPlayer` (ClientInterface id 0x06, VAR 2)

Client parser (`server_connection.cpp:1810`):

```cpp
stream >> spaceID_ >> vehicleID >> pos >> dir;   // i32, i32, Vector3(3xf32), Direction3D(3xf32)
pHandler_->onCellPlayerCreate(..., stream);      // rest = cell+client properties
```
Payload: `i32 spaceID; i32 vehicleID; f32 x; f32 y; f32 z; f32 yaw; f32 pitch; f32 roll;`
then the cell-side property values (positional, §4.3). After this the client marks the channel
"regular" (`channel().isLocalRegular(true)`), i.e. normal reliable traffic.

### 4.3 Creation property stream (createBasePlayer / createCellPlayer)

No count, no ids: the values of **all** client-visible properties are streamed positionally in
entity-def declaration order, filtered by data domain:
`EntityDescription::readStreamToDict(stream, BASE_DATA|CLIENT_DATA|EXACT_MATCH)` for the base
player, `CELL_DATA|CLIENT_DATA` for the cell player
(`lib/entitydef/entity_description.cpp:1061`, `client/entity_manager.cpp:158`,
`client/entity_type.cpp:newDictionary`). Each value is encoded per its data type from the
entity def (`lib/entitydef/data_types.cpp`): e.g. `STRING` = packed-int length + raw bytes;
`UINT8/16/32/64`, `INT*`, `FLOAT` raw fixed width; `VECTOR3` = 3 × f32; `PYTHON` = Python
pickle bytes (in element payloads the pickle is delimited by the element length; inside a
property *run* the type's size is known by decoding the pickle).
So for 0.6.5 `Account`, list the `<Properties>` entries with client data flags from the
unpacked def and emit them in order (`name` STRING first, `serverSettings` next if that is the
order in the def; must be verified from `entity_defs/Account.def`).

### 4.4 `updateEntity` (ClientInterface id 0x09, VAR 2) and later property updates

Client parser (`server_connection.cpp:1982`): reads `i32 entityID`, then passes the rest to
`onEntityProperties`. Format of the rest (`wg-toolkit` validated this against live WoT traffic,
identical in BW 2.0.1):
`u8 count`, then `count × (u8 exposedPropertyID, value)`, ids strictly ascending; a
count/table mismatch desyncs the entity stream. An empty payload means "nothing changed"
(no count byte at all in some senders — wg-toolkit documents both variants; for our own
writes always send count+run). See
`wg-toolkit/src/app/base/element.rs` `read_changed_properties`/`write_changed_properties`
(modern client but same engine format) and BW `entitydef/property_change.cpp`.

### 4.5 `createEntity` (ClientInterface id 0x08, VAR 2)

Payload, BW protocol 56 (which supports optional zlib compression):
```
u8 compression_type      # BW_COMPRESSION_NONE=0; zlib levels 1..9 -> rest is zlib-compressed
[i32 entityID; u16 entityTypeID; f32 x; f32 y; f32 z; i8 yaw; i8 pitch; i8 roll; props...]
```
(`server_connection.cpp:1944` wraps the payload in `CompressionIStream`, which reads that
leading byte; `lib/network/compression_stream.cpp`). Only needed for AoI entities, not for the
garage.

## 5. Resource streams (`resourceHeader` id 0x37 / `resourceFragment` id 0x38)

These carry the big account/shop/dossier blobs (the modern equivalent is documented in
wg-toolkit `doc/HANGAR_LOADING.md`, but 0.6.5 has the same mechanism with these simpler frames).

* `resourceHeader` payload: `u16 streamID; std::string description;` then the rest of the
  element is the description payload. BW 2.0.1: `ServerConnection::resourceHeader` reads
  `u16 id` then `DataDownload::setDesc(stream)` which does `is >> *pDesc_` — a std::string
  (packed-int length + bytes). The C++ treats it as opaque and forwards it to the script
  `onStreamComplete` callback (`client/entity_manager.cpp:1661`). **0.6.5's Python
  `Account.onStreamComplete(self, id, data)` takes only two arguments**, and
  `SyncController` immediately does `zlib.decompress(data)` + `cPickle.loads(data)` on the
  received bytes — so the 0.6.5 script layer ignores the description. Recommendation: send a
  deterministic description (e.g. empty std::string, or the modern-style pickled
  `{len, crc32}` dict for symmetry) and verify with the real client; the important part is the
  concatenated fragment bytes.
* `resourceFragment` payload: `u16 rid; u8 seq; u8 flags` then raw bytes (rest of element).
  `flags == 1` marks the first fragment (`ResourceFragmentArgs` in
  `lib/connection/client_interface.hpp:164`). Chunks are inserted by `seq`; when all are present
  the client calls `onStreamComplete(id, concatBytes)` (`server_connection.cpp:2126`,
  `lib/connection/data_download.cpp`).
* For WoT 0.6.5 garage, the account-sync stream payload is **zlib-compressed Python pickle**
  of the diff dict; client applies it in `AccountSyncData.__onSyncComplete`/`_update`
  (`decompiled/client/SyncController.py`, `AccountSyncData.py`, `Account.py:1145-1206`).
  Response trigger: entity method `ClientCommandsPort.onCmdResponse(requestID, resultID)` with
  `resultID == AccountCommands.RES_STREAM == 1`; the client then subscribes
  `rid = requestID`. `CMD_SYNC_DATA = 100`.
* Packet-size note: a 100 KB+ blob must be chunked to fit `PACKET_MAX_SIZE = 1472`; send one
  fragment per bundle/element as needed (fragment data = payload tail).

## 6. Garage path summary (what baseapp-impl must implement)

1. Answer `baseAppLogin` -> `ClientInterface.authenticate`.
2. Optionally `createBasePlayer(Account)`: `i32 accountEntityID; u16 accountTypeID;
   name (STRING); serverSettings (…);` — order/values from the unpacked 0.6.5 `Account.def`.
3. Send `Account.showGUI(pickled ctx)` client method (ctx dict with at least `databaseID`,
   `serverUTC`, `sessionStartedAt`; see wg-toolkit HANGAR doc for the modern shape; 0.6.5
   client only logs/uses a few keys — check `decompiled/client/Account.py:207`).
4. Client sends `doCmdInt3(rid, CMD_SYNC_DATA=100, revHi, revLo, 0)` as a base-entity method
   (`0xC0|exposedID_of_doCmdInt3`).
5. Reply with `ClientCommandsPort.onCmdResponse(rid, 1)`; then `resourceHeader(rid)` +
   `resourceFragment(rid, seq, flags)` chunks containing zlib(pickle(diffDict)) with the
   account `stats` and `inventory` sections (structure from 0.6.5 client helpers:
   `Account._update` fans out `inventory`, `stats`, `shop`, `dossiers`, …).
6. Client acknowledges via `onCmdResponseExt(rid, resultID, pickleExt)`; the garage then builds
   itself client-side from the cache (see wg-toolkit HANGAR doc: hangar is 100% client-local,
   no further entity traffic needed).

## 7. Sanity checklist before trusting a build

* `baseAppLogin` reply key == `authenticate` key on both directions.
* Blowfish packet filter: ciphertext length always multiple of 8, ends with `deadbeef` magic +
  wastage byte after decryption; key validated to 4..56 bytes.
* Channel first packet: handle `CREATE_CHANNEL`/`INDEXED_CHANNEL` footers; if the client keeps
  retrying `baseAppLogin`, capture its packet flags — this is the most likely early failure.
* Entity ids: `exposedID` order must come from the 0.6.5 defs, not modern WoT tables
  (modern client defs have many extra methods).
* Do not invent `createBasePlayer` `entityTypeID`: take it from the 0.6.5 `entities.xml`
  client-server entity order.
