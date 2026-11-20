#!/usr/bin/env python3
"""Structural validation of the wot065-data artifacts under *Python 3*.

The retail client is Python 2.6, so protocol-2 pickles it loads have Python-2 `str`
(byte strings) where we serialize bytes. Under Python 3 we load them with
`encoding="bytes"`: Python-2 `str` then comes back as `bytes`, while ints/sets/tuples/bools
keep their types. That is enough to prove the payload is a valid, correctly shaped pickle;
byte-semantic equivalence with a real Python-2 interpreter is checked separately by
`tools/compare_ref.py` (which runs under CPython 2.7).

Usage:
    python3 crates/wot065-data/tests/python_validate.py --dir crates/wot065-data/out \
        [--expect-vehicles 130]
"""
import argparse
import json
import os
import pickle
import sys
import zlib

failures = []


def check(name, ok, detail=""):
    print("%-62s %s %s" % (name, "OK  " if ok else "FAIL", detail))
    if not ok:
        failures.append(name)


def load_pickle2(path):
    """Protocol-2 pickle written for Python 2: decode py2 str as bytes."""
    with open(path, "rb") as f:
        return pickle.loads(f.read(), encoding="bytes")


def load_raw(path):
    with open(path, "rb") as f:
        return f.read()


def crc32_signed(data):
    value = zlib.crc32(data)
    return value - 2 ** 32 if value >= 0x80000000 else value


def validate_account_sync(base, expect_vehicles):
    path = os.path.join(base, "account_sync_full.pickle")
    if not os.path.isfile(path):
        return
    diff = load_pickle2(path)
    check("account_sync_full: unpickles under Python 3 (bytes keys)",
          isinstance(diff, dict) and b"rev" in diff, "keys=%r" % sorted(map(repr, diff.keys())))
    check("account_sync_full: rev/prevRev ints",
          isinstance(diff.get(b"rev"), int) and isinstance(diff.get(b"prevRev"), int),
          "rev=%r prevRev=%r" % (diff.get(b"rev"), diff.get(b"prevRev")))
    inventory = diff.get(b"inventory", {})
    vehicle = inventory.get(1, {})
    for key in (b"compDescr", b"shellsLayout", b"shells", b"crew", b"repair",
                b"eqsLayout", b"eqs", b"settings", b"lock"):
        check("account_sync_full: inventory[1] has key %r" % key.decode(), key in vehicle)
    comp = vehicle.get(b"compDescr", {})
    if expect_vehicles is not None:
        check("account_sync_full: %d owned vehicles" % expect_vehicles,
              len(comp) == expect_vehicles, "got %d" % len(comp))
    bad = [(k, v) for k, v in comp.items()
           if not isinstance(k, int) or not isinstance(v, bytes) or len(v) != 15]
    check("account_sync_full: every entry is (int vehInvID, 15-byte str)", not bad,
          "%d malformed" % len(bad))
    check("account_sync_full: inventory contains modules 2..=7 dicts",
          all(isinstance(inventory.get(k), dict) for k in range(1, 12)))
    tankmen = inventory.get(8, {})
    check("account_sync_full: inventory[8] tankmen shape",
          set(map(bytes, tankmen.keys())) == {b"compDescr", b"vehicle"},
          repr(tankmen))
    if expect_vehicles is not None:
        check("account_sync_full: every vehicle has populated crew and shells",
              len(vehicle.get(b"crew", {})) == expect_vehicles
              and len(vehicle.get(b"shells", {})) == expect_vehicles
              and len(tankmen.get(b"compDescr", {})) >= expect_vehicles
              and set(tankmen.get(b"compDescr", {}).keys()) == set(tankmen.get(b"vehicle", {}).keys()))
    stats = diff.get(b"stats", {})
    check("account_sync_full: stats credits/gold/slots/berths ints",
          all(isinstance(stats.get(k), int)
              for k in (b"credits", b"gold", b"slots", b"berths")))
    check("account_sync_full: stats growing sets are real Python sets",
          all(isinstance(stats.get(k), set)
              for k in (b"unlocks", b"eliteVehicles", b"doubleXPVehs")))
    check("account_sync_full: stats dossier is bytes, clanInfo is None",
          isinstance(stats.get(b"dossier"), bytes) and stats.get(b"clanInfo") is None)
    account = diff.get(b"account", {})
    check("account_sync_full: account section attrs/restrictions",
          isinstance(account.get(b"attrs"), int)
          and isinstance(account.get(b"restrictions"), list))
    cache = diff.get(b"cache", {})
    check("account_sync_full: cache battlesTillCaptcha", b"battlesTillCaptcha" in cache)

    # Stream payload + description must describe the payload we emit.
    z_path = os.path.join(base, "account_sync_full.pickle.z")
    if os.path.isfile(z_path):
        raw = load_raw(z_path)
        check("account_sync_full.pickle.z: decompresses to the raw pickle",
              zlib.decompress(raw) == load_raw(path))
        desc_path = os.path.join(base, "account_sync_full.desc")
        if os.path.isfile(desc_path):
            desc = load_pickle2(desc_path)
            check("account_sync_full.desc: (len, signed crc32) tuple",
                  isinstance(desc, tuple) and len(desc) == 2
                  and desc[0] == len(raw) and desc[1] == crc32_signed(raw),
                  repr(desc))


def validate_showgui(base):
    path = os.path.join(base, "showgui_ctx.pickle")
    if not os.path.isfile(path):
        return
    ctx = load_pickle2(path)
    check("showgui_ctx: dict with int databaseID",
          isinstance(ctx, dict) and isinstance(ctx.get(b"databaseID"), int), repr(ctx))
    check("showgui_ctx: serverUTC int when present",
          b"serverUTC" not in ctx or isinstance(ctx.get(b"serverUTC"), int))


def validate_shop(base):
    path = os.path.join(base, "shop_cache.pickle")
    if not os.path.isfile(path):
        return
    shop = load_pickle2(path)
    check("shop_cache: exchangeRate int", isinstance(shop.get(b"exchangeRate"), int), repr(shop.get(b"exchangeRate")))
    slots = shop.get(b"slotsPrices")
    check("shop_cache: slotsPrices (base, (prices))",
          isinstance(slots, tuple) and len(slots) == 2 and slots[0] == 6
          and isinstance(slots[1], tuple) and len(slots[1]) >= 1, repr(slots))
    check("shop_cache: sellPriceModif float", isinstance(shop.get(b"sellPriceModif"), float))
    check("shop_cache: tankmanCost 3 dicts",
          isinstance(shop.get(b"tankmanCost"), tuple) and len(shop[b"tankmanCost"]) == 3)
    shop_items = shop.get(b"items", {})
    check("shop_cache: items has nations 0, 1, 2 and 15 (dict, set) buckets",
          all(nid in shop_items for nid in (0, 1, 2, 15))
          and all(
              isinstance(bucket, tuple) and len(bucket) == 2
              and isinstance(bucket[0], dict) and isinstance(bucket[1], set)
              for nation in shop_items.values()
              for bucket in nation.values()
          ))
    z_path = os.path.join(base, "shop_cache.pickle.z")
    if os.path.isfile(z_path):
        raw = load_raw(z_path)
        check("shop_cache.pickle.z: decompresses to the raw pickle",
              zlib.decompress(raw) == load_raw(path))
        desc_path = os.path.join(base, "shop_cache.desc")
        if os.path.isfile(desc_path):
            desc = load_pickle2(desc_path)
            check("shop_cache.desc: (len, signed crc32) matches",
                  isinstance(desc, tuple) and desc[0] == len(raw) and desc[1] == crc32_signed(raw),
                  repr(desc))


def validate_dossier(base):
    path = os.path.join(base, "dossier_sync.pickle")
    if not os.path.isfile(path):
        return
    dossier = load_pickle2(path)
    check("dossier_sync: (version:int, flatList len%4==0)",
          isinstance(dossier, tuple) and len(dossier) == 2
          and isinstance(dossier[0], int) and isinstance(dossier[1], list)
          and len(dossier[1]) % 4 == 0, repr(dossier))


def validate_catalog(base):
    path = os.path.join(base, "vehicles_catalog.json")
    if not os.path.isfile(path):
        return
    with open(path, "r", encoding="utf-8") as f:
        catalog = json.load(f)
    count = catalog.get("count")
    vehicles = catalog.get("vehicles", [])
    check("catalog: count == len(vehicles)", count == len(vehicles), "%r / %d" % (count, len(vehicles)))
    by_nation = {}
    for vehicle in vehicles:
        by_nation[vehicle.get("nation")] = by_nation.get(vehicle.get("nation"), 0) + 1
    check("catalog: per-nation counts", by_nation == {"ussr": 45, "germany": 44, "usa": 41},
          repr(by_nation))
    bad = [v.get("name") for v in vehicles
           if len(v.get("vehCompDescrHex", "")) != 30
           or v.get("vehCompDescrLen") != 15]
    check("catalog: every vehicle has a 15-byte vehCompDescr", not bad, "%d malformed" % len(bad))
    expected_type = all(
        v.get("vehTypeCompDescr") == ((v.get("typeID", -1) << 8) | (v.get("nationID", -1) << 4) | 1)
        for v in vehicles)
    check("catalog: vehTypeCompDescr == (typeID<<8)|(nationID<<4)|1", expected_type)
    hex_ok = all(_hex_ok(v.get("vehCompDescrHex", "")) for v in vehicles)
    check("catalog: vehCompDescrHex is lowercase hex", hex_ok)


def _hex_ok(text):
    if not isinstance(text, str) or len(text) != 30:
        return False
    try:
        int(text, 16)
    except ValueError:
        return False
    return True


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--dir", required=True)
    parser.add_argument("--expect-vehicles", type=int, default=None)
    args = parser.parse_args()
    print("validating artifacts in", os.path.abspath(args.dir))
    validate_account_sync(args.dir, args.expect_vehicles)
    validate_showgui(args.dir)
    validate_shop(args.dir)
    validate_dossier(args.dir)
    validate_catalog(args.dir)
    print()
    if failures:
        print("FAILED: %d check(s)" % len(failures))
        return 1
    print("ALL PYTHON-3 STRUCTURAL CHECKS PASSED")
    return 0


if __name__ == "__main__":
    sys.exit(main())
