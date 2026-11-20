# `wot065-data` — WoT 0.6.5 account/garage data layer

Rust implementation of the **account data layer** the retail World of Tanks 0.6.5 client
(Python 2.6) expects at login and in the hangar:

* vehicle **catalog** parsed from the game's own `item_defs/vehicles` files (packed
  BigWorld XML, magic `45 4E A1 62`), including both compact-descriptor encodings;
* **CMD_SYNC_DATA full sync diff** that hands the client **all 130 vehicle types as owned**
  with their stock components;
* **`showGUI` ctx**, **CMD_SYNC_SHOP shop cache** and **CMD_SYNC_DOSSIERS payload**;
* a hand-rolled **Python-2 protocol-2 pickle writer** (+ zlib + BigWorld stream
  description) with *no Python dependency at runtime*.

The client-side contract was reverse-engineered from the decompiled 0.6.5 scripts; see
`docs/research/client-analysis.md` for the full narrative. Byte-exact Python-2 reference
payloads produced during that analysis live in
`.scratch/research/decompiled/_generated/` (read-only research input) and are used by the
test suite for comparison.

## Quick start

```powershell
# from the repository root
cargo build -p wot065-data

# catalog counts (reads the real game installation, packed XML)
cargo run -p wot065-data -- count --game-dir "C:\Users\qwerty\Desktop\World_of_Tanks"
#   vehicle catalog: 130 types
#     ussr      45
#     germany   44
#     usa       41

# generate every payload the baseapp / login app needs
cargo run -p wot065-data -- all --game-dir "C:\Users\qwerty\Desktop\World_of_Tanks" `
    --out-dir crates/wot065-data/out
```

`--game-dir` accepts the installation root, the `res` directory, `.../item_defs`, or the
`item_defs/vehicles` directory; it defaults to `$WOT065_GAME_DIR` and then to the local
install location.

### Subcommands

| Command | Output |
|---|---|
| `count` | per-nation + total vehicle counts |
| `catalog --out vehicles_catalog.json` | catalog JSON (same schema as the research reference) |
| `diff --out account_sync_full.pickle [--stream] [--unlock-all] [--rev N] [--prev-rev N]` | pickled full-sync diff |
| `gui --out showgui_ctx.pickle [--database-id N] [--server-utc N]` | pickled `showGUI` ctx |
| `shop --out shop_cache.pickle [--stream]` | pickled shop cache |
| `dossier --out dossier_sync.pickle [--stream] [--version N]` | pickled dossier sync payload |
| `all --out-dir DIR` | all of the above, with reference-style file names |

`--stream` writes `zlib(pickle)` to `--out` and the matching BigWorld stream description
to `<out>.desc`. Without it, `--out` receives the raw pickle (`onCmdResponseExt` `ext`
argument, or the `showGUI` argument).

### Generated artifacts (`out/`, produced by `all`)

| File | Bytes | What the baseapp does with it |
|---|---|---|
| `account_sync_full.pickle` | 3070 | pickle for `onCmdResponseExt(rid, RES_SUCCESS=0, ext)` |
| `account_sync_full.pickle.z` | 1794 | resource-stream payload for the retail `RES_STREAM` path |
| `account_sync_full.desc` | 12 | stream description pickle `(len, signed crc32)` |
| `showgui_ctx.pickle` | 36 | argument of `Account.showGUI(ctx)` (not compressed) |
| `shop_cache.pickle` / `.z` / `.desc` | 371 / 274 / 12 | CMD_SYNC_SHOP (RES_STREAM **only**) |
| `dossier_sync.pickle` / `.z` / `.desc` | 7 / 15 / 11 | CMD_SYNC_DOSSIERS (RES_STREAM) |
| `vehicles_catalog.json` | 110557 | catalog dump for tooling/debugging |

Sizes are from this crate's own generator and are stable for a given catalog/options
(smaller than the Python-2 references because the Python pickler emits more memoization
opcodes; the unpickled objects are identical — enforced by test).

## Library API

```rust
use wot065_data::{account, catalog, pickle};

let catalog = catalog::load(std::path::Path::new(game_dir))?;      // 130 vehicle types
let pickled = account::full_sync_pickle(&catalog, &account::FullSyncOptions::default());
let gui     = account::showgui_ctx_pickle(database_id, server_utc);
let shop    = account::shop_cache_pickle();
let dossier = account::dossier_sync_pickle(0);
let (z, desc) = account::stream_bundle(&pickled);                  // RES_STREAM pair
```

Low level: `pickle::dumps(&Value)` (protocol 2), `pickle::loads` (tests),
`pickle::zlib_compress`, `pickle::signed_crc32`, `pickle::stream_description`.

## Vehicle catalog

Parsed from `<game>/res/scripts/item_defs/vehicles`:

* `<nation>/list.xml` – one child per vehicle type; the tag is the internal name, `<id>`
  is the nation-local type id, plus `<level>`, `<price>`, `<tags>`;
* `<nation>/<Name>.xml` – stock components: first child of `<chassis>`, `<engines>`,
  `<fuelTanks>`, `<radios>`, and of `<turrets0>`; the stock gun is the first gun of the
  stock turret. 0.6.5 has **exactly one turret position** for every vehicle (the parser
  rejects multi-turret definitions);
* `<nation>/components/{chassis,engines,fuelTanks,radios,turrets,guns}.xml` – `<ids>`
  tables giving nation-local component ids (`<shared>` blocks exist but are empty in
  0.6.5; the parser still resolves them for robustness).

The unpacked development copies (`.scratch/research/unpacked/.../*.xml.xml`) are read by
the same code (the reader tries `.xml` then `.xml.xml`). Packed XML is decoded with the
MIT `wg-toolkit` crate (`wgtk::pxml::from_bytes`), whose API maps exactly onto BigWorld's
packed-XML codec; a small plain-XML parser in `src/xml.rs` is the fallback.

Result: **130 vehicle types — ussr 45, germany 44, usa 41**, sorted by
`(nationID, typeID)`; nation ids are `ussr=0, germany=1, usa=2`
(`common/nations.py`). Type ids are sparse (e.g. germany `Lowe` has type id 212) and every
one fits the one-byte field of `vehCompDescr`.

### Compact descriptors

`vehTypeCompDescr` (int32) — how the client refers to a vehicle *type*
(`vehicles.makeIntCompactDescrByID`, decompiled `common/items/vehicles.py:793-795`;
`parseIntCompactDescr` at `:747-751`):

```text
vehTypeCompDescr = (typeID << 8) | (nationID << 4) | 1     # 1 = ITEM_TYPE_NAMES index of 'vehicle'
```

`vehCompDescr` (Python-2 **binary `str`**, 15 bytes in 0.6.5) — the configured vehicle
stored in `inventory['compDescr'][vehInvID]`
(`VehicleDescr.makeCompactDescr`, `common/items/vehicles.py:345-385`; decode at
`:566-631`):

```text
struct.pack('<2B4H', 1 | (nationID << 4), typeID, chassisID, engineID, fuelTankID, radioID)
+ struct.pack('<2H', turretID, gunID)   # once per turret position (always 1 in 0.6.5)
+ chr(flags)                            # 0x00: no optional devices/emblem/horn/camo
= 15 bytes
```

Component ids are nation-local ids from the `components/*.xml` `<ids>` blocks (e.g. ussr
fuel tanks are ids 201-205). A descriptor with an unknown id is silently dropped by the
client's carousel builder (`client/gui/Scaleform/Hangar.py:443-460`), so the tests decode
every generated descriptor and compare all 130 against the Python-2 reference catalog,
field by field.

## CMD_SYNC_DATA full-sync diff

Reference semantics: `client/Account.py:483-503` (`_update`), `client/AccountSyncData.py`
(revision handling), `client/account_helpers/{Inventory,Stats,Trader}.py`.

```text
{
  'rev': 1,                 # new account revision; client stores it, next sync sends it
  'prevRev': 0,             # revision the diff applies to (0 on a fresh login)
  'inventory': {
      1:  {                 # item type 1 = vehicle
          'compDescr':    {vehInvID: <15-byte str>},   # ALL 130 vehicles
          'shellsLayout': {}, 'shells': {}, 'crew': {},
          'repair': {}, 'eqsLayout': {}, 'eqs': {},
          'settings': {}, 'lock': {},
      },
      8:  {'compDescr': {}, 'vehicle': {}},           # tankmen
      9:  {}, 10: {}, 11: {},                         # optional devices/shells/equipment
  },
  'stats': {credits, gold, slots, berths, freeXP, dossier(''), clanInfo(None), accOnline,
            accOffline, freeTMenLeft, freeVehiclesLeft, captchaTriesLeft, hasFinPassword,
            tkillIsSuspected, vehTypeXP{}, unlocks(set), eliteVehicles(set),
            doubleXPVehs(set)},
  'cache':   {'battlesTillCaptcha': 0},
  'account': {'clanDBID': 0, 'attrs': 0, 'premiumExpiryTime': 0, 'autoBanTime': 0,
              'restrictions': []},
}
```

Why these shapes (all decompiled-client citations):

* `inventory[1]` must contain **all nine** sub-dicts even when empty because
  `InventoryParser.parseVehicles` (`gui/Scaleform/utils/requesters.py:39-56`) indexes
  `data['compDescr']`, `data['shellsLayout']`, `data['shells']`, `data['crew']`,
  `data['repair']`, `data['eqsLayout']`, `data['eqs']`, `data['settings']`, `data['lock']`
  without defaults. Only `compDescr` needs entries.
* The hangar carousel is exactly this dict: `Hangar.__updateVehicles`
  (`gui/Scaleform/Hangar.py:414-469`) enumerates `Requester('vehicle').getFromInventory()`,
  i.e. `inventory[1]['compDescr']`. Giving all 130 keys is what makes every tank appear.
* `inventory[8]` needs `{compDescr, vehicle}` for the tankmen parser
  (`requesters.py:58-66`); simple item types can be `{}` (`requesters.py:68-74`).
* `stats` keys and their kinds are fixed by `Stats.synchronize`
  (`client/account_helpers/Stats.py:25-31,54-100`): simple values, `vehTypeXP` dict, and
  the three **growing sets** `unlocks`/`eliteVehicles`/`doubleXPVehs`, which the client
  stores in a Python `set` (`Stats.py:73-78`; later `set |= diff[...]` in
  `gui/Scaleform/ConstructionDepartment.py:78-86` requires a real set).
* `cache['battlesTillCaptcha']` is a Stats cache stat; `cache['vehsLock']` would be the
  vehicle-lock sub-cache (`Inventory.py:64-70`) — omitted = no locks.
* `account` keys feed `Stats._ACCOUNT_STATS` (`Stats.py:30-31,86-94`); `attrs` must be an
  int because `account_helpers.isPremiumAccount(attrs)` bit-tests it.
* `prevRev`/`rev` are checked in `client/AccountSyncData.py:84-88`: on the first sync the
  client revision is 0, so `prevRev` must be 0. Note that the presence of **any**
  `prevRev` key makes `Account._update` treat the diff as incremental
  (`Account.py:485`); the reference payload includes `prevRev: 0`, and since a fresh
  client has empty caches, the result is identical to a full sync. Our builder reproduces
  the reference. (For a *reconnect*, a baseapp should either send `prevRev=<client rev>`
  with the full content, or answer with `RES_STREAM` and a payload without `prevRev` to
  force `Inventory`/`Stats` cache clearing.)
* `showGUI` ctx (`client/Account.py:208-221`): `{'databaseID': int, 'serverUTC': int}`
  pickled **without** zlib. `databaseID` is mandatory; `serverUTC` seeds the client's
  clock correction.
* Shop cache (`client/account_helpers/Shop.py`, required for the hangar):
  `slotsPrices` / `exchangeRate` are read by `Hangar.__updateVehicles`
  (`Hangar.py:425`) and `CommonPage.updateAccountInfo` (`CommonPage.py:714-715`). The
  cache is assigned **only** in `Shop.__onSyncComplete` (`Shop.py:264-276`), which runs
  only for the `RES_STREAM` path — a plain `RES_SUCCESS` reply leaves the cache empty and
  the hangar throws while building the carousel. Hence CMD_SYNC_SHOP must use
  `RES_STREAM` + the streamed zlib payload.
* Dossier payload `(version, flatList)` with `len(flatList) % 4 == 0`
  (`client/account_helpers/DossierCache.py:110-133`); `(0, [])` is a valid empty cache.
* BigWorld stream description: `(len(payload), crc32(payload))` pickled, CRC as a
  **signed** 32-bit int; `client/game.py:151-157` receives it via `onStreamComplete` and
  the engine validates the stream against it before handing the bytes to Python.

## Pickle encoding (Python 2.6 compatible)

`src/pickle.rs` implements the protocol-2 subset used by these payloads, matching
CPython's `cPickle.dumps(obj, 2)` semantics that the client relies on:

* `str` is **bytes** (`SHORT_BINSTRING`/`BINSTRING`), never `unicode` — the client
  `struct.unpack`s these values;
* ints use `BININT1/2/BININT` and `LONG1` for values outside int32 (e.g. large DB ids);
  a positive long whose top byte would read as a sign bit gets the extra `0x00` byte;
* floats use `BINFLOAT`;
* dicts use `EMPTY_DICT` + `MARK`/`SETITEMS`; lists `EMPTY_LIST`/`APPENDS`; tuples
  `MARK`/`TUPLE`;
* sets are emitted as genuine Python-2 sets:
  `GLOBAL __builtin__.set` + `REDUCE(set, (list,))`, because the client later does
  `set |= diff[...]` and union-assign only accepts a set.

The reader (`pickle::loads`) exists for tests/cross-checking and handles metadata opcodes
(memo/PUT/GET), `GLOBAL`/`REDUCE` for sets, and the string/integer encodings above.

## Validation

```powershell
cargo test -p wot065-data            # 13 unit + 5 integration tests
cargo run -p wot065-data -- all --game-dir <install> --out-dir crates/wot065-data/out

# structural load of every generated pickle under Python 3 (bundled venv)
.scratch/research/decompiled/_tools/venv/Scripts/python.exe `
    crates/wot065-data/tests/python_validate.py --dir crates/wot065-data/out --expect-vehicles 130

# semantic comparison with the Python-2 reference payloads, under CPython 2.7
.scratch/research/decompiled/_tools/py27/inst/python.exe `
    crates/wot065-data/tools/compare_ref.py --out crates/wot065-data/out
```

What the tests prove:

* catalog counts are 130 (45/44/41) and the generated `vehicles_catalog.json` is field-for
  -field identical to the reference catalog for all 130 vehicles;
* `vehCompDescr` encoding/decoding round-trips and matches hand-checked golden values
  (T-34, MS-1, T-34-85, PzIV, Maus, Löwe, T1_Cunningham);
* the full-sync diff is **structurally equal** (Rust-side pickle reader) to the Python-2
  reference pickle, includes all 130 owned vehicles and all nine required inventory keys;
* the generated artifacts load under Python 3, and under CPython 2.7 they compare equal to
  the reference objects (`compare_ref.py` → `ALL CHECKS PASSED`);
* stream descriptions are `(len(zlib), signed crc32(zlib))` for **our own** payload bytes;
* the CLI `all` command end-to-end produces all contract files.

The integration tests skip (with a `skip:` message) the parts that need the retail
installation or the research payloads, so the crate still builds/tests on a machine
without them.

## Versioning / compatibility notes

* The payload *format* is dictated by the 0.6.5 client; this crate targets exactly that
  client (Python 2.6, `AccountDataDefs` in the client scripts). `rev`/`prevRev` are
  runtime values (CLI: `--rev`/`--prev-rev`), not a format version.
* `vehInvID`s are `sorted position + 1` over `(nationID, typeID)`, reproducing the
  reference `account_sync_full.json` mapping (T-34=1 … M22_Locust=130). A baseapp may
  choose different ids as long as they are stable positive ints.
* `--unlock-all` additionally fills `unlocks` (all vehicles + every stock component's int
  compact descriptor), `eliteVehicles` (all vehicles) and `vehTypeXP` (100k per vehicle).
  The default output deliberately matches the reference payload (empty sets).
* Type ids above 255 are rejected explicitly: the binary descriptor stores
  `vehicleTypeID` in one byte (0.6.5 max is 212).

## Open questions / follow-ups

1. **Crew**: vehicles are handed out with empty `crew` dicts (matches the reference
   payload, and the carousel renders). Recruiting valid tankmen requires tankman compact
   descriptors; the "battle" button stays disabled without a full crew — out of scope for
   "show all tanks in the garage".
2. **Shop items**: the reference shop cache has empty `items`; the hangar only needs
   `slotsPrices`/`exchangeRate` to render the carousel. Buying vehicles/modules from the
   shop UI will need real prices per nation/type (`items[nation][itemType] = ({id: (credits,
   gold)}, hiddenSet)`); the catalog already carries vehicle prices and component ids, so
   that is an additive builder.
3. **Dossier cache**: empty for a fresh account; profile UI will show zeros. Vehicle/account
   dossiers are opaque compact descriptors decoded by `dossiers/__init__.py`.
4. **Vehicle state** (`repair`, `shells`, `eqs`, `settings`, `lock`): omitted/sub-empty, so
   every vehicle shows as undamaged, unlocked and unconfigured — sufficient for the
   carousel; a baseapp that wants ammo/fitting screens populated must fill those sub-dicts.
5. **Wire transport** (how the stream packets are framed) is the baseapp/loginapp
   teammates' concern; this crate only produces the validated payload bytes + description.
6. The packed-XML reader depends on `wg-toolkit`'s `pxml` crate, which has been verified
   against every 0.6.5 item-def file (all 130 vehicles parse). Should a future file use a
   different variant, `src/xml.rs` can host a local port of
   `.scratch/research/bwxml/bwxml.py` (read/unpack logic documented there).

## Layout

```text
crates/wot065-data/
  Cargo.toml
  README.md
  src/
    lib.rs        crate docs + module wiring
    catalog.rs    item_defs parser + compact descriptor codec + golden tests
    account.rs    diff / showGUI / shop / dossier builders
    pickle.rs     Python-2 protocol-2 writer/reader, zlib, stream description
    xml.rs        packed-XML (wg-toolkit) + plain-XML tree reader
    testutil.rs   locate repo/game/reference/python locations for tests
    main.rs       CLI
  tests/
    integration.rs   real-install + reference comparison + CLI/python E2E tests
    python_validate.py  Python-3 structural validation of artifacts
  tools/
    compare_ref.py   CPython-2.7 semantic comparison with the research references
    dump_ref.py      dump the structure of the reference payloads
  out/            generated artifacts (from `all`, committed for the baseapp teammate)
```
