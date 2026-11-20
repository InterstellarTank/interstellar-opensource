# WoT 0.6.5 client reverse engineering: login → Account sync → garage

Research note produced by the `client-reverse` teammate. This is the **client-side contract** the
server must satisfy. It is derived from the retail 0.6.5 Python 2.6 scripts shipped with the game
(`World_of_Tanks/res/scripts`, 220 `.pyc`, pyc magic `d1 f2 0d 0a` = CPython 2.6/62161) and from the
unpacked BigWorld entity definitions (produced by `bwxml-gamedata`).

Everything below was read out of the decompiled code; **the decompiled code is the source of truth**
for 0.6.5. A modern reference implementation (`wg-toolkit-rs`, WoT 2.3-era) was only used to
cross-check the BigWorld mechanics; where it differs it is called out explicitly, because the 0.6.5
client does **not** match the modern one in several important details.

## 0. TL;DR — the minimum the server must do to reach a garage with all tanks

1. Complete the BigWorld **loginapp → baseapp** handshake so the client's `BigWorld.connect()`
   reports stage 2 (`onConnected`). The client then has an entity manager and waits for a player
   entity (engine-level, see §2).
2. Create the base **`Account`** entity for this client (BigWorld "create base player"), with
   properties `name`, `normalizedName` (STRING) and `serverSettings` (PYTHON dict, must contain
   `'vivoxDomain': ''`). The engine instantiates the client-side class
   `client/Account.py::PlayerAccount` and calls `__init__` + `onBecomePlayer()`.
3. `onBecomePlayer()` makes the client send three account-command messages back-to-back
   (each with a fresh client-allocated `requestID`):
   * `doCmdInt3(rid, 100 /*CMD_SYNC_DATA*/, revHi=0, revLo=0, 0)`
   * `doCmdInt3(rid, 300 /*CMD_SYNC_SHOP*/, clientShopRev=0, 0, 0)`
   * `doCmdInt3(rid, 600 /*CMD_SYNC_DOSSIERS*/, cacheVersion=0, maxChangeTime=0, 0)`
4. Answer them:
   * **CMD_SYNC_DATA**: either
     * `onCmdResponseExt(rid, RES_SUCCESS=0, cPickle.dumps(full_diff_dict))` — one shot, no stream
       needed (client applies the diff from the pickled `ext`, see §4.3), **or**
     * retail style: `onCmdResponse(rid, RES_STREAM=1)` + push a BigWorld resource stream with
       id=rid whose payload is `zlib.compress(cPickle.dumps(full_diff_dict))`; then the client sends
       a follow-up CMD_SYNC_DATA that must be answered with `onCmdResponse(rid, RES_SUCCESS)`.
   * **CMD_SYNC_SHOP**: must use the retail stream path — `onCmdResponse(rid, RES_STREAM=1)` + a
     resource stream whose payload is `zlib.compress(cPickle.dumps(shop_cache_dict))`. The client
     only installs the shop cache from the stream-completion callback, there is no `ext` fallback.
   * **CMD_SYNC_DOSSIERS**: `onCmdResponse(rid, RES_STREAM=1)` + stream payload
     `zlib.compress(cPickle.dumps((version:int, [])))`. Optional for the garage to render, but cheap.
5. Call `Account.showGUI(cPickle.dumps({'databaseID': 1, 'serverUTC': <utc epoch int>}))`.
6. Provide full-sync data containing (a) the vehicle inventory as `compDescr` blobs for every tank
   and (b) the handful of stats/shop keys the hangar UI reads (§5, §6).

Byte-exact Python-2 reference payloads are in
`.scratch/research/decompiled/_generated/` (see §9). A ready catalog of the 130 vehicle types with
both compact descriptor encodings is in
`.scratch/research/decompiled/_generated/vehicles_catalog.json`.

## 1. Decompilation toolchain and coverage

* Toolchain: bundled Python 3.12 venv at
  `.scratch/research/decompiled/_tools/venv` with `xdis 6.1.8` + `uncompyle6 3.9.3`.
  `uncompyle6` decompiles CPython 2.6 bytecode without complaint when driven through
  `uncompyle6.main.decompile_file` (the console entry point's API is different in 3.9.3, so the
  batch driver calls the library function and falls back to the console executable).
* Python 2.7.18 was additionally extracted with `msiexec /a` into
  `.scratch/research/decompiled/_tools/py27/inst` and is used to generate exact Python-2 pickles
  (`_tools/gen_py2_samples.py`).
* Coverage: **219/220 files decompiled to `.py`**, all 220 also have opcode/const listings
  (`.dis.txt`); only `client/post_processing/__init__.py` failed to decompile (disassembly is
  present). Report: `.scratch/research/decompiled/_tools/decompile_report.txt`.
  A global identifier/constant index across every code object is at
  `.scratch/research/decompiled/_index/strings.txt` (`<file>\t<string>` lines) — fastest way to
  locate a protocol string.
* Decompiled sources keep a trailing module-level `return` that `uncompyle6` emits for the
  2.6 module code object; that one line makes the files non-compilable as-is (`SyntaxError:
  'return' outside function`). Strip the final `return` before syntax-checking/importing.
* **Quality check:** with that single line stripped, all **219/219** decompiled sources compile
  under CPython 2.7.18 (`_tools/check_py2_syntax.py`, report `_tools/py2_syntax_report.txt`).
* Output layout mirrors the client: `.scratch/research/decompiled/client/...`,
  `.scratch/research/decompiled/common/...`, plus `<file>.dis.txt` next to each `<file>.py`.
* Tooling: `_tools/decompile_all.py` (batch decompile+disassemble), `_tools/check_py2_syntax.py`,
  `_tools/gen_vehicle_catalog.py`, `_tools/validate_catalog.py`, `_tools/gen_py2_samples.py`.
* Entity definitions are authoritative and were unpacked by the gamedata teammate into
  `.scratch/research/unpacked/res/scripts/entity_defs/*.def.xml` (the retail files are BWXML-packed;
  the `.pyc` of the client are the other half of the contract).

## 2. Login: what the client does before a player entity exists

Chain of calls on the client (Python side only; the BigWorld wire protocol is
`protocol-scout`'s domain):

1. `client/game.py` starts the GUI (`g_windowsManager.start()`); `gui/WindowsManager.py:57-79`
   shows the login page through `CommonPage.processLogin()` → flash `loadLogin`.
2. The flash login screen calls back `login.Login(id, user, password, host)`
   (`client/gui/Scaleform/Login.py:381-406`). Host comes from `scriptConfig['login']` entries and/or
   from `BigWorld.serverDiscovery` (LAN discovery, `client/ConnectionManager.py:29-67`).
3. `ConnectionManager.connect(host, user, password, publicKeyPath)` builds a `LoginInfo` object with
   `username`, `password`, `inactivityTimeout = CLIENT_INACTIVITY_TIMEOUT (40)`, optional
   `publicKeyPath`, and calls `BigWorld.connect(host, login, self.connectionWatcher)` —
   `client/ConnectionManager.py:76-87`, `common/constants.py:341`.
4. `connectionWatcher(stage, status, serverMsg)` → `ConnectionManager.__connectionStatusCallback`
   (`client/ConnectionManager.py:96-120`):
   * stage `0`: ignored;
   * stage `1`: loginapp result. `status == 'LOGGED_ON'` means accepted; anything else disconnects
     (`isUpdateClientSoftwareNeeded()` special-cases `LOGIN_BAD_PROTOCOL_VERSION` and
     `LOGIN_REJECTED_BAD_DIGEST`);
   * stage `2`: **connection to the base app is up** → `onConnected()` is fired. This is the point
     the server GUI handover relies on;
   * stage `6`: disconnected → `onDisconnected()`.
5. Login status strings handled by `Login.py:303-362` (BigWorld loginapp status vocabulary, useful
   when implementing login app errors): `LOGGED_ON`, `LOGIN_REJECTED_BAN` (message format
   `"<expiryTime>;<reason>"`), `LOGIN_REJECTED_LOGIN_QUEUE` (message = order in queue),
   `LOGIN_CUSTOM_DEFINED_ERROR`, `LOGIN_REJECTED_RATE_LIMITED`, `LOGIN_REJECTED_LOGINS_NOT_ALLOWED`,
   `LOGIN_REJECTED_BASEAPP_TIMEOUT`, `CONNECTION_FAILED`, `DNS_LOOKUP_FAILED`,
   `LOGIN_BAD_PROTOCOL_VERSION`, `LOGIN_REJECTED_BAD_DIGEST`. Unknown statuses fall through
   `handleLogOnFailed` and render `#menu:login/status/<status>`.
6. After stage 2, the engine enables entities. The client expects the server to create the player
   entity (retail `Account`); the client binds BigWorld entity type `Account` (`entities.xml`) to
   `client/Account.py::PlayerAccount` and runs `__init__` then `onBecomePlayer()`.

Important detail: the client runs `Account.onBecomePlayer` (`client/Account.py:59-77`) with an
`@process` (adisp coroutine) wrapper. The synchronous part — which includes starting the three sync
requests — runs before the first `yield`, so the server may receive all three commands immediately
after creating the entity. Only then is the GUI shown (`showGUI`).

## 3. Account entity contract (0.6.5)

Authoritative file: `.scratch/research/unpacked/res/scripts/entity_defs/Account.def.xml`.

### 3.1 Properties

| Name | Type | Flags | Notes |
|---|---|---|---|
| `name` | STRING | BASE_AND_CLIENT | client `Account.__init__` reads `self.name` **before** the sync completes (it seeds `DossierCache` and the repository) — send it with the entity |
| `normalizedName` | STRING | BASE_AND_CLIENT, Identifier | |
| `role` | UINT32 | BASE | |
| `accountType` | UINT8 | BASE | not client-visible; `account_helpers.getAccountTypeName()` only falls back to it |
| `attrs` | UINT64 | BASE | delivered to the client through the account-stats diff (`account.attrs`), not as a property |
| `premiumExpiryTime`, `autoBanTime`, `unlockedVehicleLevel`, `restrictions`, `clanDBID` | | BASE | same — delivered through the stats diff |
| `pdata` | PYTHON | BASE, persistent | server-side account blob (used by private servers as DB record) |
| `bp`, `lastUserMessageID`, `lastSystemMessageID`, `vivoxCredentials` | | BASE | |
| `serverSettings` | PYTHON | BASE_AND_CLIENT | **must be sent**; must be a dict containing `'vivoxDomain'` (`''` disables Vivox gracefully — `gui/Scaleform/VoiceChatInterface.py:84-92`) |

The client never reads `attrs`/`premiumExpiryTime` from the entity property: `Stats.synchronize`
takes them from the diff's `account` section (§4.4). Do not rely on property sync alone.

### 3.2 Server → client methods (ClientMethods)

| Method | Args | Client handler |
|---|---|---|
| `version_eu6501` | — | `client/Account.py:27-28` (no-op, version marker) |
| `onCmdResponse` | INT16 requestID, INT16 resultID | `Account.onCmdResponse` (2 args, **no error string** in 0.6.5) |
| `onCmdResponseExt` | INT16 requestID, INT16 resultID, STRING ext (pickled) | `Account.onCmdResponseExt` → `cPickle.loads(ext)` |
| `onKickedFromServer` | STRING reason, BOOL isBan, UINT32 expiryTime | `Account.onKickedFromServer` |
| `onEnqueued`, `onEnqueueFailure(UINT8)`, `onDequeued` | | queue lifecycle |
| `onArenaCreated`, `onArenaJoinFailure(UINT8)` | | battle hand-off |
| `onPrebattleJoined(OBJECT_ID)`, `onPrebattleJoinFailure(UINT8)`, `onPrebattleLeft`, `onKickedFromQueue`, `onKickedFromArena(UINT8)`, `onKickedFromPrebattle(UINT8)` | | prebattle lifecycle |
| `showGUI` | STRING ctx (pickled dict) | **main lobby trigger**, §4.5 |
| `receiveActiveArenas` | ARRAY<PUBLIC_ARENA_INFO> | training/arena list |
| `receiveServerStats` | SERVER_STATISTICS | server stats panel |
| `receiveQueueInfo` | RANDOMS_QUEUE_INFO, COMPANIES_QUEUE_INFO | queue counters |
| `updatePrebattle` | UINT8, STRING | prebattle delta |
| `update` | STRING diff (pickled) | **incremental account update**, §4.6 |
| `resyncDossiers` | — | forces dossier resync |

### 3.3 Client → server methods (BaseMethods, `Exposed`)

| Method | Args | Used for |
|---|---|---|
| `doCmdStr` | INT16 requestID, INT16 cmd, STRING | string commands (language, comments…) |
| `doCmdInt3` | INT16 requestID, INT16 cmd, INT32, INT32, INT32 | most commands incl. all three syncs |
| `doCmdInt4` | INT16 requestID, INT16 cmd, INT32×4 | buy vehicle / sell / respec |
| `doCmdInt2Str` | INT16 requestID, INT16 cmd, INT32, INT32, STRING | dossier/clan requests, trade offers |
| `doCmdIntArr` | INT16 requestID, INT16 cmd, ARRAY<INT32> | shell/equipment arrays, XP conversion arrays |
| `makeTradeOfferByClient`, `createTraining`, `createSquad`, `createCompany` | | prebattle/trading, not needed for garage |

Also inherited through interfaces: `Chat` (`onChatAction` client method; exposed `chatCommand`,
`inviteCommand`, `ackCommand`) and account-editor/transaction methods. None of these are needed to
reach the garage.

`requestID` allocation on the client (`client/Account.py:509-516`, `common/AccountCommands.py:110-114`,
`common/streamIDs.py:6-10`):
* first request id is `REQUEST_ID_UNRESERVED_MIN = STREAM_ID_ACCOUNT_CMDS_MIN + 20 = 120`,
* increments by one per command, wraps to 120 when it reaches `REQUEST_ID_UNRESERVED_MAX = 30000`,
* fixed ids: `REQUEST_ID_PREBATTLES = 100`, `REQUEST_ID_PREBATTLE_ROSTER = 101`,
  `REQUEST_ID_NO_RESPONSE = 102`. **102 means "fire and forget — do not answer"** (it is used for
  server-stats polls, arena list, queue info, enqueue/dequeue, create/join arena).
* The server must echo the request id in `onCmdResponse`/`onCmdResponseExt`/stream id; the client
  keys its callbacks by it.

## 4. Account synchronisation mechanics

### 4.1 What the client sends

`Account.onBecomePlayer` → `AccountSyncData.onAccountBecomePlayer`
(`client/AccountSyncData.py:23-27,56-63`) → `_synchronize()` → `SyncController.request(syncID, None)`
→ `AccountSyncData.__sendSyncRequest`:

```python
first  = self.revision >> 32
second = self.revision & 0xFFFFFFFF
self.__account._doCmdInt3(AccountCommands.CMD_SYNC_DATA, first, second, 0, proxy)
# -> base.doCmdInt3(requestID, 100, revHi, revLo, 0)
```
(`client/AccountSyncData.py:116-122`; `revision` starts at 0; a `dossier`/`update` diff may bump it.)

Note the 0.6.5 argument split. The modern WoT code sends `(revision, cacheLen, crc32)`; **do not copy
that** — for 0.6.5 arg0/arg1 are the two halves of a 64-bit revision and arg2 is 0.

Then, from the same `onBecomePlayer` loop:
* `Shop.onAccountBecomePlayer` → `doCmdInt3(rid, 300, clientRev, 0, 0)` (`client/account_helpers/Shop.py:390-395`),
* `DossierCache.onAccountBecomePlayer` → `doCmdInt3(rid, 600, cacheVersion, maxChangeTime, 0)`
  (`client/account_helpers/DossierCache.py:155-159`).
* Inventory/Stats/Trader have no own sync request; they are fed by the CMD_SYNC_DATA reply.

### 4.2 Reply paths accepted by the client

All three syncers wrap `SyncController(account, sendRequest, onSyncResponse, onSyncComplete)`
(`client/SyncController.py`). `Account.onCmdResponse`/`onCmdResponseExt` route to per-request
callbacks; `Account.onStreamComplete` routes stream data (`client/Account.py:103-137`).

For a request that got `resultID == RES_STREAM (1)`, `SyncController.__onSyncResponse` subscribes
`requestID` for a stream; when the engine finishes delivering that stream it calls
`game.onStreamComplete(id, desc, data)` (`client/game.py:151-157`), which forwards to
`player.onStreamComplete(id, data)`; `SyncController.__onSyncStreamComplete` does
`zlib.decompress(data)` then `cPickle.loads(data)` and then calls the owner's complete-handler.

For any other non-negative `resultID`, the owner's `__onSyncResponse(syncID, resultID, ext)` runs
immediately; negative results are errors.

Resource stream format expected by the client (identical mechanic to later BigWorld versions;
cross-checked against the modern MIT `wg-toolkit-rs` implementation, which documents it as the
client's `game.onStreamComplete` description):
* stream id = the account-command `requestID` (a u16),
* stream payload = `zlib.compress(pickle.dumps(obj, 2))`,
* stream "description" = `pickle.dumps((len(payload), crc32(payload)), 2)` where the CRC is encoded
  as a **signed** 32-bit int (Python 2 `zlib.crc32` can be negative). The client engine validates
  payload length + CRC before handing the bytes to `onStreamComplete`.

### 4.3 Delivering a full sync without a stream (recommended for the first integration)

`SyncController.__onSyncResponse` accepts any `resultID != RES_STREAM` and calls
`AccountSyncData.__onSyncResponse(syncID, resultID, ext)`
(`client/AccountSyncData.py:75-97`):

```python
if resultID < 0: resynchronise
if self.revision != ext.get('prevRev', self.revision): resynchronise
self.revision = ext.get('rev', self.revision)
self.__isSynchronized = True
self.__account._update(not self.__isFirstSync, ext)
```

So a first full sync can be delivered as
`onCmdResponseExt(requestID, 0 /*RES_SUCCESS*/, cPickle.dumps(diff))` where `diff` contains
`prevRev: 0` and `rev: <n>` plus all data sections. Conditions:
* first request has client revision 0, so `prevRev` must be 0;
* include `rev` (client stores it as its revision);
* do **not** use `RES_STREAM` and do not use a negative result;
* after this single call the client is synchronised; it does not send a second CMD_SYNC_DATA.

If the retail stream path is used instead: after the stream completes,
`AccountSyncData.__onSyncComplete` (`client/AccountSyncData.py:99-108`) stores
`revision = data['rev']`, applies `data` through `Account._update(isFullSync, data)` (full sync =
`data` has no `prevRev` key), then calls `_synchronize()` again, i.e. **the client sends a second
CMD_SYNC_DATA with the new revision**; answer that one with `onCmdResponse(rid, RES_SUCCESS)` (empty
`ext` → `prevRev` defaults to current revision, `rev` defaults to current revision, all fine) to
clear the "synchronising" flag and release waiting callers.

Warning: answering the *first* CMD_SYNC_DATA with a plain `onCmdResponse(rid, RES_SUCCESS)` is
accepted but applies an empty diff (`ext = {}`) — the account ends up with empty inventory/stats.
That is a valid "empty account" but not what we want for the garage.

### 4.4 Full diff schema

`Account._update(triggerEvents, diff)` (`client/Account.py:483-503`) forwards the diff to the
syncers:
* `diff['inventory']` → `account_helpers/Inventory.synchronize(isFullSync, diff)` (§5.1),
* `diff['stats']`, `diff['cache']`, `diff['account']`, `diff['economics']` →
  `account_helpers/Stats.synchronize` (§5.2),
* `diff['offers']` → `account_helpers/Trader.synchronize` (trade offers `{in, out}`),
* `diff['cache']['vehsLock']` → vehicle `lock` sub-cache,
* `diff['cache']['specialPrebattles']` / `diff['cache']['clanMembers']` → cache dicts,
* after applying, if `triggerEvents`, fires `g_playerEvents.onClientUpdated(diff)` and per-vehicle
  `onVehicleBecomeElite`/`onVehicleLockChanged` events built from the diff.

Sync payload keys are ordinary Python objects and must be pickled with Python 2 protocol ≤2
(the client is Python 2.6 with the pure/c `cPickle`; protocol 2 is the safe choice; **sets**,
**tuples**, **binary `str`** and **dicts keyed by ints** are all used by the client code).

### 4.5 `showGUI(ctx)` — the lobby trigger

`Account.showGUI` (`client/Account.py:208-221`):
```python
ctx = cPickle.loads(ctx)
self.databaseID = ctx['databaseID']          # REQUIRED (DB_ID of the account)
if 'prebattleID' in ctx: ...
if 'queueID' in ctx: self.isInQueue = True
if 'serverUTC' in ctx: helpers.time_utils.setTimeCorrection(ctx['serverUTC'])
events.isPlayerEntityChanging = False
events.onAccountShowGUI(ctx)
```
* `ctx` = pickled dict; `databaseID` is mandatory, type DB_ID (integer).
* Optional: `prebattleID` (int), `queueID` (int), `serverUTC` (int UTC epoch seconds — the client
  computes a time correction and uses it for all "local server time" math).
* `onAccountShowGUI` → `gui/WindowsManager.py:81-90::showLobby` → `CommonPage.processLobby`
  (`client/gui/Scaleform/CommonPage.py:610-622`):
  1. `yield g_currentVehicle.getFromServer()` — this **waits until the account sync has completed**
     and the vehicle inventory is available (it does `Requester('vehicle').getFromInventory()`);
  2. subscribes UI events;
  3. `movie.invoke(('loginSuccess',))` — flash loads the lobby/hangar page;
  4. `onStatsReceived({})`, `updateAccountInfo()`, `battleDispatcher.start(isInQueue)`,
     `Waiting.hide('enter')`.
* Flash then loads the hangar clip; `CommonPage.onLoadInit` constructs `main_interfaces.idict['hangar']`
  = `gui/Scaleform/Hangar.py::Hangar` and calls `populateUI`, which finally builds the carousel.
* If `showGUI` never arrives the client sits at the "enter" waiting screen forever.

### 4.6 Incremental updates

Server may push `Account.update(pickled_diff)` (ClientMethod, STRING). The client runs
`Account._update(True, diff)`; `diff` must carry `prevRev` equal to the client's current revision
or the client logs `'Incorrect diff received'` and resynchronises (a full CMD_SYNC_DATA with
revision 0 appears again). Typical usage after the first sync: currency changes, new purchases,
exp/elite changes. `diff['stats']` may contain only changed keys — the syncers merge.

## 5. Data the garage needs — exact requirements

### 5.1 Inventory (vehicle list) — `account_helpers/Inventory.py`

Cache shape (client): `Inventory.__cache[itemTypeIdx]`; for item type `1` (vehicle) the value is a
dict with sub-dicts, mirroring `AccountDataDefs.txt` (`res/scripts/common/AccountDataDefs.txt:64-98`).

Full-sync `diff['inventory']` is `{itemTypeIdx: subdiff}` and is merged recursively
(`Inventory.__synchronizeDicts`, `client/account_helpers/Inventory.py:414-423`): `None` deletes,
nested dicts merge, scalars replace; an **empty** dict still creates the key in the cache
(`setdefault`), which matters because the parser indexes all keys without defaults.

The client reads the vehicle inventory through
`gui/Scaleform/utils/requesters.py::InventoryParser.parseVehicles` (lines 39-56):

```python
for (id, vehCompDescr) in data['compDescr'].items():
    ammoLayout        = dict(data['shellsLayout'].get(id, {}))
    shells            = list(data['shells'].get(id, []))
    crew              = list(data['crew'].get(id, []))
    (repairCost, health) = data['repair'].get(id, (0, 0))
    equipmentsLayout  = data['eqsLayout'].get(id, [0, 0, 0])
    equipments        = data['eqs'].get(id, [0, 0, 0]) or [0, 0, 0]
    settings          = data['settings'].get(id, 0)
    lock              = data['lock'].get(id, 0)
    InventoryVehicle(compactDescr=vehCompDescr, id=id, crew=..., ...)
```

Therefore `diff['inventory'][1]` **must contain all nine keys**, even if empty:
`compDescr`, `shellsLayout`, `shells`, `crew`, `repair`, `eqsLayout`, `eqs`, `settings`, `lock`.
Only `compDescr` needs entries. An empty `diff['inventory'][8]` etc. is fine for the parser of
tankmen (`{ 'compDescr': {}, 'vehicle': {} }`) and simple item types (plain `{}`), but including
them avoids `TypeError` when the UI later asks for tankmen/shells (the parser would receive `None`
if the cache has no entry for that item type).

The same cache is what `BigWorld.player().inventory.getItems(itemTypeIdx, cb)` returns **after**
`syncData.waitForSync(...)` (`client/account_helpers/Inventory.py:73-75`), so `Hangar` blocks on the
account sync before it can list anything.

### 5.2 Stats — `account_helpers/Stats.py`

`Stats.synchronize` (`client/account_helpers/Stats.py:54-100`) recognises:

| Diff key | Kind | Client handling |
|---|---|---|
| `stats['credits']`, `gold`, `slots`, `berths`, `freeXP`, `dossier`, `clanInfo`, `accOnline`, `accOffline`, `freeTMenLeft`, `freeVehiclesLeft`, `captchaTriesLeft`, `hasFinPassword`, `tkillIsSuspected` | simple values | replace |
| `stats['vehTypeXP']` | dict `{vehTypeCompDescr: xp}` | merge, `None` deletes |
| `stats['unlocks']`, `['eliteVehicles']`, `['doubleXPVehs']` | growing sets | `.update()`; a `<name>_r` key replaces the whole set |
| `cache['battlesTillCaptcha']` | int | replace |
| `account['clanDBID','attrs','premiumExpiryTime','autoBanTime','restrictions']` | | replace; `account['unlockedVehicleLevel']` triggers a dev auto-unlock path |
| `economics['unlocks','eliteVehicles']` | sets | `.update()` (client-side union) |

Types used by the client: `credits/gold/slots/berths/freeXP` ints, `attrs` an unsigned int (premium
bit is `ACCOUNT_ATTR.PREMIUM`), `premiumExpiryTime` UTC int, `restrictions` a list,
`clanInfo` `None` or a tuple/dict (raw `AccountClan.Info`), `dossier` a string (compact dossier
descriptor; `''` is accepted), `vehTypeXP` a dict, the three "sets" Python `set`s,
`hasFinPassword`/`tkillIsSuspected` bools.

Which keys are actually consumed on the lobby/hangar path:
* `Hangar.populateUI` → `BigWorld.player().stats.get('attrs', cb)` (`gui/Scaleform/Hangar.py:80`);
* `CommonPage.updateAccountInfo` → shop `exchangeRate`, stats `credits`, `gold`, `freeXP`,
  `clanInfo`, `attrs`, `premiumExpiryTime` (`client/gui/Scaleform/CommonPage.py:712-742`);
* `Hangar.__updateVehicles` → stats `slots`, `doubleXPVehs`, `eliteVehicles`
  (`gui/Scaleform/Hangar.py:414-469`);
* `CurrentVehicle`/`CommonPage.__changeCurrentVehicle` → stats `vehTypeXP`, shop `exchangeRate`
  (only for displays);
* `StatsRequester.getRestrictions` → `restrictions` (`gui/Scaleform/utils/requesters.py:490-494`).

`Stats.get(stat, cb)` waits for the sync first (`Stats.py:102-109`), so all these calls queue until
the account sync reply above is processed.

### 5.3 Shop cache — `account_helpers/Shop.py` (required for the hangar!)

`Shop` has its own `SyncController`; its cache is assigned **only** from the stream-completion path
(`Shop.__onSyncComplete`, `client/account_helpers/Shop.py:264-276`). The plain-response path only
clears the "synchronising" flag (`Shop.__onSyncResponse:252-262`). Consequence: **CMD_SYNC_SHOP must
be answered with `RES_STREAM` + a resource stream**, otherwise `shop.getSlotsPrices()` returns
`None` and `Hangar.__updateVehicles` raises while building the carousel
(`price[1][0]` on `None`, `Hangar.py:425`).

Shop cache keys used by the code (`client/account_helpers/Shop.py`):
* `rev`: int — cache revision; compared with what the client sends next time,
* `items`: `{nationIdx: {itemTypeIdx: ({id: (credits, gold)}, hiddenSet)}}` — shop prices
  (for vehicles `id` is the nation-local vehicle type id; for modules/shells the compact descriptor),
* `slotsPrices`: `(initialSlots:int, tuple(prices))` — **required by the hangar** to show slot price,
* `berthsPrices`: `(initialBerths:int, packSize:int, tuple(prices))`,
* `exchangeRate`: int (gold→credits), **required by `CommonPage.updateAccountInfo`**,
* `sellPriceModif`: float (0.5 in retail), used when computing sell prices,
* `premiumCost`: `{days:int: gold:int}`,
* `tankmanCost`: tuple of three `{'credits':…, 'gold':…, 'roleLevel':…}` dicts,
* `passportChangeCost`, `paidRemovalCost`, `freeXPConversion`, `camouflageCost`, `hornCost`,
  `tradeFees`: optional for the garage.

`Shop` waits for sync in `waitForSync` (`Shop.py:82-93`); `getSlotsPrices` goes through
`__getValue` and transparently waits for the sync.

### 5.4 Dossier cache — `account_helpers/DossierCache.py`

CMD_SYNC_DOSSIERS (600) args are `(self.__version, self.__maxChangeTime, 0)`. The reply on the
stream path must be `pickle((version:int, flatList))` where `len(flatList) % 4 == 0`; entries are
quadruples `(dossierType, ownerID, changeTime, dossierCompDescr)` (`DossierCache.py:110-133`).
For a new account `(0, [])` is fine. Not required for the garage to render, but answering keeps the
dossier requesters from hanging.

### 5.5 Trader/offers

`diff['offers'] = {'in': {...}, 'out': {...}}` merges into `Trader` (`account_helpers/Trader.py:32-51`).
Not needed for the garage; omitting the key is fine.

## 6. How the hangar builds the tank list

`gui/Scaleform/Hangar.py` (decompiled: `.scratch/research/decompiled/client/gui/Scaleform/Hangar.py`).

1. `CommonPage.processLobby` first awaits `g_currentVehicle.getFromServer()`
   (`client/CurrentVehicle.py:164-174`): it reads the locally stored favourite vehicle id and then
   `Requester('vehicle').getFromInventory()`, i.e. it needs the account sync to have completed with
   a vehicle list. It picks the matching inventory id, else the first vehicle (sorted); if there are
   no vehicles, `g_currentVehicle` stays empty and the lobby would show "no vehicle".
2. Flash loads the hangar page; `Hangar.populateUI` (`Hangar.py:46-86`) registers the UI callbacks,
   calls `g_currentVehicle.update()`, sets the carousel filter, reads `stats.attrs`, and hides the
   waiting screen.
3. `Hangar.__updateVehicles` (`Hangar.py:414-469`) is the carousel builder:
   * `StatsRequester().getSlotsCount()` → `stats['slots']`,
   * `StatsRequester().getSlotsPrices()` → `shop['slotsPrices']`,
   * `StatsRequester().getDoubleXPVehicles()` → `stats['doubleXPVehs']`,
   * `Requester('vehicle').getFromInventory()` → the vehicle list,
   * `StatsRequester().getEliteVehicles()` → `stats['eliteVehicles']`,
   * for each vehicle it builds `[compactItem(vehicle), name, icon, nation, level, state,
     isDoubleXP, tooltip, vehTypeCompDescrInt, favorite, isElite]` and finally calls
     `flash 'hangar.vehiclesResponse'` with `[slotPrice, slots, count, currentVehicleCompactItem,
     per-vehicle rows...]`.
   * Any per-vehicle exception (e.g. a `compDescr` that does not decode) is caught and the tank is
     **silently skipped** (`try: ... except: LOG_CURRENT_EXCEPTION(); continue`, `Hangar.py:443-460`).
     This is the main way a broken descriptor "hides" a tank.
4. `CurrentVehicle.getFromServer`/`__findCurrent` uses `vehicles.sort()`; `InventoryVehicle.__cmp__`
   sorts by level/type, so at least one valid vehicle must be present.
5. Hangar 3D scene: entirely client-side — `gui/ClientHangarSpace.py` creates an offline BigWorld
   space, adds a geometry mapping from local resources and spawns an `OfflineEntity` for the
   current vehicle (`ClientHangarSpace.py:98-112`). No server-side space/entity is involved.
6. Elite/state/lock/crew only affect labels and the "battle" button:
   `InventoryVehicle.isElite()` compares `descriptor.type.compactDescr` with `eliteVehicles`,
   `CurrentVehicle` computes `repairCost`/`lock`/crew-full status, but the carousel entry is
   rendered regardless. `rules/lock`: `LOCK_REASON` values from `common/AccountCommands.py:81-86`;
   default 0 is shown as unlocked.

## 7. Compact descriptors (0.6.5) — the thing that makes "all tanks" work

Sources of truth: `.scratch/research/decompiled/common/items/__init__.py`,
`.scratch/research/decompiled/common/items/vehicles.py`,
`.scratch/research/decompiled/common/nations.py`, plus the unpacked item defs
`.scratch/research/unpacked/res/scripts/item_defs/`.

* Item type ids (`items.ITEM_TYPE_NAMES`): `0 reserved, 1 vehicle, 2 vehicleChassis, 3 vehicleTurret,
  4 vehicleGun, 5 vehicleEngine, 6 vehicleFuelTank, 7 vehicleRadio, 8 tankman, 9 optionalDevice,
  10 shell, 11 equipment`. `getTypeOfCompactDescr(cd) == cd & 15`.
* Nations (`common/nations.py`): `ussr=0, germany=1, usa=2`.

### 7.1 `vehTypeCompDescr` (int)

Used wherever the client refers to a *vehicle type*: `stats['vehTypeXP']` keys,
`stats['unlocks']`/`eliteVehicles`/`doubleXPVehs` members, shop vehicle prices, dossier keys.
Encoding (`vehicles.makeIntCompactDescrByID`, `vehicles.py:793-795`, and
`parseIntCompactDescr`, `vehicles.py:747-751`):

```
vehTypeCompDescr = (vehicleTypeID << 8) | (nationID << 4) | 1
itemType  =  cd        & 0xF   == 1 (vehicle)
nationID  = (cd >> 4)  & 0xF
typeID    = (cd >> 8)  & 0xFFFF      (vehicles.list ids are 0..255 in 0.6.5)
```
Examples from the generated catalog: ussr T-34 (typeID 0) → `1`; germany PzIV (typeID 0) → `17`;
usa T34_hvy (typeID 11) → `2849`; germany Löwe (typeID 212) → `54289`.

### 7.2 Inventory `vehCompDescr` (binary string, 15 bytes in 0.6.5)

This is the value stored in `inventory['compDescr'][vehInvID]` — a **binary Python-2 `str`**, not an
int. It carries the *configured* vehicle: chassis/engine/fuel-tank/radio/turret/gun ids plus flags.
Writing (`vehicles.VehicleDescr.makeCompactDescr`, `vehicles.py:345-385`); reading
(`VehicleDescr.__initFromCompactDescr`, `vehicles.py:566-631`):

```
struct.pack('<2B4H', header, vehicleTypeID, chassisID, engineID, fuelTankID, radioID)
+ struct.pack('<2H', turretID, gunID)      # repeated once per turret position
+ chr(flags)                               # bit0-3 optionalDeviceMask, bit4 emblem,
                                          # bit6 horn, bit7 camouflage
+ optional devices (one <H> per set flag bit, highest slot first)
+ optional emblem byte, horn byte, camouflage <H>   (only when their flag is set)
header = itemTypeID(1) | (nationID << 4)
```
All 0.6.5 vehicles have exactly **one** turret position (verified against every unpacked vehicle
XML), so a minimal descriptor is always **15 bytes**: header/typeID + 4 component `<H>` ids +
turret/gun `<H>` pair + flags `0x00`. Component ids are **nation-local** ids read from
`item_defs/vehicles/<nation>/components/<type>.xml` `<ids>` sections (e.g. ussr fuel tanks are ids
201-205). The client resolves them through `g_cache`, which is built from the same XML; a descriptor
with any unknown id raises in `_descrByID` and the vehicle is dropped from the carousel
(`Hangar.py` catches it); the same 15-byte string is what the client hands back in commands such as
`CMD_EQUIP`/`CMD_SELL_VEHICLE`.

Generation rule for any vehicle type:
1. `nation, typeID` from that nation's `list.xml` (`<name><id>`);
2. default components = first child element name of `<chassis>`, `<engines>`, `<fuelTanks>`,
   `<radios>` in the vehicle's XML; default turret = first child of `<turrets0>`; default gun =
   first child of that turret's `<guns>` (if the turret/gun is marked `shared`, resolve the
   definition from `components/turrets.xml`/`guns.xml` `<shared>`; the ids always come from the
   `<ids>` block of the components file);
3. look up the ids, pack as above with `flags=0`.

The full catalog for all 130 vehicles (sssr 45, germany 44, usa 41) was generated with
`.scratch/research/decompiled/_tools/gen_vehicle_catalog.py` and validated against the XML:
`.scratch/research/decompiled/_generated/vehicles_catalog.json` (+ `.csv`) fields:
`nation, nationID, typeID, name, level, price, tags, vehTypeCompDescr, vehCompDescrHex,
vehCompDescrB64, vehCompDescrLen, components, componentIDs`.
Examples:
```
ussr   T-34  typeCD=1     hex=010000000000cb0003000000000000
germany PzIV typeCD=17    hex=110000000000cb0002000000000000
usa    T34_hvy typeCD=2849 hex=210b16001600cd0008001500100000
```
Caveat: component ids and names come from the client's packed item defs as unpacked by
`bwxml-gamedata`; that unpacking is the source of truth for the ids. Cross-check against their
catalog when it is published (extend `gen_vehicle_catalog.py` with a diff step).

## 8. Other client/server interactions seen around login (not required for the garage)

* After login the client polls the server stats every second when `IS_SHOW_SERVER_STATS`
  (`common/constants.py:23`, `CommonPage.__requestServerStats`, `CommonPage.py:761-775`):
  `doCmdInt3(102 /*REQUEST_ID_NO_RESPONSE*/, 501 /*CMD_REQ_SERVER_STATS*/, 0,0,0)` — no reply
  expected; the server pushes `receiveServerStats(SERVER_STATISTICS)` on its own schedule.
* `CMD_SET_LANGUAGE` (1000) is sent with a picked string command from
  `WindowsManager.showLobby` (`setLanguage`); it is a normal command and may be answered with
  `onCmdResponse(rid, RES_SUCCESS)`.
* Queue/prebattle/arena commands (`CMD_REQ_QUEUE_INFO` 502, `CMD_ENQUEUE_FOR_ARENA` 700,
  `CMD_DEQUEUE` 701, prebattle commands …) only matter when the player queues.
* Chat: `Account` implements the `Chat` interface and `chatManager.switchPlayerProxy(self)` runs on
  `onBecomePlayer`; chat channel data is streamed through `ClientChat` with ids in
  `[STREAM_ID_CHAT_MIN=1, STREAM_ID_CHAT_MAX=99]` and `CHAT_INITIALIZATION_ID=50`
  (`common/streamIDs.py`). Empty chat is harmless for the garage.
* `VoiceChatInterface.initialize(serverSettings['vivoxDomain'])`: `''` is a supported
  "Vivox not supported" path (`VoiceChatInterface.py:84-92`), so a stub server can pass `''`.

## 9. Generated reference artifacts

All under `.scratch/research/decompiled/_generated/`:

| File | Contents |
|---|---|
| `vehicles_catalog.json` / `.csv` | 130 vehicle types: `vehTypeCompDescr` + 15-byte `vehCompDescrHex` + component ids |
| `account_sync_full.pickle` | Python-2 pickle (proto 2) of the full account sync diff (rev 1, prevRev 0, 130 vehicles) — 3134 bytes |
| `account_sync_full.pickle.z` / `.desc` | zlib-compressed payload + stream description pickle `(len, crc32)` — 1899 bytes |
| `shop_cache.pickle` / `.pickle.z` / `.desc` | shop cache with `slotsPrices`/`exchangeRate` minimal keys |
| `dossier_sync.pickle` / `.pickle.z` / `.desc` | `(0, [])` dossier sync payload |
| `showgui_ctx.pickle` | example `showGUI` ctx `{'databaseID': 1, 'serverUTC': …}` |
| `account_update_example.pickle` | example incremental `Account.update` diff |
| `*.json` | human-readable structure of the above |

Regeneration:
```
_tools/venv/Scripts/python.exe _tools/gen_py2_samples.py        # needs Python 2.7 for cPickle
_tools/venv/Scripts/python.exe _tools/gen_vehicle_catalog.py    # Python 3, reads unpacked item_defs
```
The Python-2 samples are produced with the extracted CPython 2.7.18
(`_tools/py27/inst/python.exe`) so that `str` vs `unicode`, set/tuple pickling and signed CRC are
byte-exact for a 2.x client.

## 9.1 Validation performed

* Vehicle catalog: `_tools/validate_catalog.py` re-parses the unpacked item defs and checks, for all
  130 vehicles, that `vehTypeCompDescr` matches the bit formula, the 15-byte descriptor decodes to the
  catalog component ids, the type id fits a byte, and every component/turret/gun id is actually
  listed as installable by that vehicle (and belongs to that turret). Result: **130/130 ok, 0
  problems**. Descriptor bytes are compared against an independent XML re-read, not against the
  generator's own output.
* Payloads: the `.pickle/.z/.desc` files were re-loaded with CPython 2.7 (same runtime family as the
  client) and checked: expected top-level keys, 130 `compDescr` entries of exactly 15 bytes,
  `set` instances where the syncers expect sets, `desc == (len(zlib_data), crc32(zlib_data))`, and
  `zlib.decompress` round-trips. All pass.
* Sources: 219/219 decompiled `.py` compile under CPython 2.7.18 after stripping the
  `uncompyle6` module-level `return` artifact.

## 10. Gaps / open questions

1. **BigWorld resource stream wire format** for 0.6.5: the client validates `(len, crc32)` and calls
   `game.onStreamComplete`; the exact packet/fragment framing must be confirmed against the 0.6.5
   client executable/replay captures (`protocol-scout` owns this). The modern toolkit implementation
   (MIT) documents the same semantics and can be used as a template.
2. **Loginapp → baseapp hand-off** (RSA session key, `LOGIN_REJECTED_*` codes, `requiredVersion`):
   not in the Python scripts; `protocol-scout` covers it. Client-side status vocabulary is listed
   in §2.5.
3. **Entity creation details** (`createBasePlayer`/`enableEntities`): server-side BigWorld mechanics;
   the client-side expectation is only "player entity of type Account appears, properties name and
   serverSettings set". Exact property order/first-sync message ordering not visible in the scripts.
4. **`version_eu6501`**: a client method marker (probably a hotfix/version gate). Presence in
   `Account.def` means the retail server presumably called it on some clients; not on the garage
   path. Not needed initially.
5. **Dossier compact descriptors** (account/vehicle dossiers) are opaque strings to the server at
   first; `dossiers/__init__.py` decodes them if the profile UI is used. Not needed for the garage.
6. **Unpacked item defs vs. retail packed files**: the catalog depends on `bwxml-gamedata`'s
   unpacking. If a descriptor is rejected by the client, diff the unpacked XML against the original
   around that vehicle's component ids first.
7. `accountType`, `role`, `vivoxCredentials` are server-only properties; `Account` code calls
   `account_helpers.getAccountTypeName()` only when no explicit index is passed — not on the garage
   path.

## 11. File cheat-sheet (decompiled, repo-relative)

| Concern | File |
|---|---|
| login UI / status strings | `.scratch/research/decompiled/client/gui/Scaleform/Login.py` |
| connection state machine | `.scratch/research/decompiled/client/ConnectionManager.py` |
| client app entry | `.scratch/research/decompiled/client/game.py` |
| lobby/hangar page orchestration | `.scratch/research/decompiled/client/gui/Scaleform/CommonPage.py` |
| window manager / showLobby | `.scratch/research/decompiled/client/gui/WindowsManager.py` |
| account entity | `.scratch/research/decompiled/client/Account.py` |
| account sync | `.scratch/research/decompiled/client/AccountSyncData.py` |
| sync plumbing (+stream) | `.scratch/research/decompiled/client/SyncController.py` |
| commands/results constants | `.scratch/research/decompiled/common/AccountCommands.py` |
| stream ids/ranges | `.scratch/research/decompiled/common/streamIDs.py` |
| inventory cache/commands | `.scratch/research/decompiled/client/account_helpers/Inventory.py` |
| stats cache | `.scratch/research/decompiled/client/account_helpers/Stats.py` |
| shop cache/sync | `.scratch/research/decompiled/client/account_helpers/Shop.py` |
| dossier cache/sync | `.scratch/research/decompiled/client/account_helpers/DossierCache.py` |
| trade offers | `.scratch/research/decompiled/client/account_helpers/Trader.py` |
| hangar UI + carousel | `.scratch/research/decompiled/client/gui/Scaleform/Hangar.py` |
| hangar 3D space (client-only) | `.scratch/research/decompiled/client/gui/ClientHangarSpace.py` |
| inventory/stat requesters (parser!) | `.scratch/research/decompiled/client/gui/Scaleform/utils/requesters.py` |
| current vehicle selection | `.scratch/research/decompiled/client/CurrentVehicle.py` |
| item type ids | `.scratch/research/decompiled/common/items/__init__.py` |
| vehicle/component descriptor codec | `.scratch/research/decompiled/common/items/vehicles.py` |
| nations | `.scratch/research/decompiled/common/nations.py` |
| account data format (retail doc) | `.scratch/research/…/res/scripts/common/AccountDataDefs.txt` (read-only game file) |
| 0.6.5 entity contract | `.scratch/research/unpacked/res/scripts/entity_defs/Account.def.xml`, `Avatar.def.xml` |
| all decompiled sources + disassembly | `.scratch/research/decompiled/client|common/**` |
| decompile report | `.scratch/research/decompiled/_tools/decompile_report.txt` |
| string/name index | `.scratch/research/decompiled/_index/strings.txt` |

## 12. Beyond the garage: entering a battle (context for the next phase)

The garage path never touches `Avatar`; `Avatar` is only created when the player queues for battle.
Sequence the client expects (for orientation, not needed for the garage milestone):

1. The player presses battle; `BattleDispatcherInterface` (`client/gui/Scaleform/BattleDispatcherInterface.py:474-486`)
   calls `Account.enqueueForArena(vehInvID, arenaTypeID, queueType)` →
   `base.doCmdInt3(102 /*REQUEST_ID_NO_RESPONSE*/, 700 /*CMD_ENQUEUE_FOR_ARENA*/, vehInvID,
   arenaTypeID, queueType)` (`client/Account.py:320-323`; `QUEUE_TYPE` in `common/constants.py`).
   The server acknowledges by calling the client method `Account.onEnqueued()`.
2. Queue UI polls `doCmdInt3(102, 502 /*CMD_REQ_QUEUE_INFO*/, 0,0,0)`; the server pushes
   `Account.receiveQueueInfo(randomsQueueInfo, companiesQueueInfo)`.
3. When the arena is made, the server calls `Account.onArenaCreated()` (no args). Client:
   `prebattle = None`, `g_playerEvents.isPlayerEntityChanging = True`, fires
   `onPlayerEntityChanging` (`client/Account.py:156-162`) — the UI switches to battle loading and
   `CommonPage.processBattleLoading` runs.
4. The server switches the client's **base app** (BigWorld-level) and creates an `Avatar` base
   player entity there. The client binds entity type `Avatar` to `client/Avatar.py::PlayerAvatar`;
   `Avatar.onBecomePlayer` (`Avatar.py:59-114`) requires these entity properties to be set first
   (they are BASE_AND_CLIENT in `Avatar.def.xml`): `name` (STRING), `arenaTypeID` (INT32),
   `arenaGuiType` (UINT8), `arenaExtraData` (PYTHON), `weatherPresetID` (UINT8); plus `team` (UINT8)
   and `playerVehicleID` (OBJECT_ID) which are OWN_CLIENT. It builds `ClientArena` from these, tears
   down the hangar space, switches chat proxy and fires `g_playerEvents.onAvatarBecomePlayer` →
   `WindowsManager.showBattleLoading` → flash battle loading page.
5. `Avatar.onEnterWorld(prereqs)` then runs once the client's cell side is in a space; the client
   reports readiness with the exposed base method `Avatar.onClientReady()` (`Avatar.def.xml`), and
   afterwards the battle proceeds via `updateArena`/`updatePositions`/cell methods.
6. Client→server exposed Avatar base methods in `Avatar.def.xml:271-348`: `onClientReady`,
   `leaveArena`, `vehicle_moveWith(UINT8)`, `vehicle_shoot`, `vehicle_trackPointWithGun(VECTOR3)`,
   `vehicle_stopTrackingWithGun(F32,F32)`, `vehicle_changeSetting(UINT8,INT32)`,
   `vehicle_teleport(VECTOR3,F32)`, `vehicle_useHorn(BOOL)`, `setDevelopmentFeature(STRING,INT32)`,
   `addBotToArena(STRING,UINT8,STRING)`, `controlAnotherVehicle(OBJECT_ID,UINT8)`,
   `receiveFakeShot(...)`. Vehicle entities themselves are `Vehicle.def` — out of scope here.

Garage-only server work should not need `Avatar`, spaces, `Arena` entities or any cell app at all;
that is the next protocol milestone after the hangar shows up.

