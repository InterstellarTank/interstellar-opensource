# -*- coding: utf-8 -*-
"""Compare wot065-data generated payloads with the Python-2 reference payloads.

Run with Python 2.7 (the bundled research interpreter) so that the reference client's
cPickle semantics apply:

    .scratch/research/decompiled/_tools/py27/inst/python.exe \
        crates/wot065-data/tools/compare_ref.py
"""
import cPickle
import os
import sys
import zlib

HERE = os.path.dirname(os.path.abspath(__file__))
OUT = os.path.join(os.path.dirname(HERE), "out")
REF = os.path.join(os.path.dirname(os.path.dirname(os.path.dirname(HERE))),
                   ".scratch", "research", "decompiled", "_generated")

failures = []


def parse_args(argv):
    """--out DIR --ref DIR override the defaults (used by the Rust integration test)."""
    global OUT, REF
    i = 0
    while i < len(argv):
        if argv[i] == "--out" and i + 1 < len(argv):
            OUT = argv[i + 1]
            i += 2
        elif argv[i] == "--ref" and i + 1 < len(argv):
            REF = argv[i + 1]
            i += 2
        else:
            print("usage: compare_ref.py [--out DIR] [--ref DIR]")
            return False
    return True


def load_raw(path):
    with open(path, "rb") as f:
        return f.read()


def load_pickle(path):
    return cPickle.loads(load_raw(path))


def check(name, ok, detail=""):
    print("%-58s %s %s" % (name, "OK  " if ok else "FAIL", detail))
    if not ok:
        failures.append(name)


def diff_dicts(expected, actual, path="$", out=None):
    """Return a human readable list of structural differences (handles sets/dicts)."""
    if out is None:
        out = []
    if type(expected) is not type(actual):
        # int/long are interchangeable for our purposes
        if isinstance(expected, (int, long)) and isinstance(actual, (int, long)):
            pass
        else:
            out.append("%s: type %s != %s" % (path, type(expected).__name__, type(actual).__name__))
            return out
    if isinstance(expected, dict):
        expected_keys = set(expected.keys())
        actual_keys = set(actual.keys())
        if expected_keys != actual_keys:
            out.append("%s: keys differ, missing=%r extra=%r" % (
                path, sorted(map(repr, expected_keys - actual_keys)),
                sorted(map(repr, actual_keys - expected_keys))))
        for key in expected_keys & actual_keys:
            diff_dicts(expected[key], actual[key], "%s[%r]" % (path, key), out)
    elif isinstance(expected, (set, frozenset)):
        if expected != actual:
            out.append("%s: set %r != %r" % (path, expected, actual))
    elif isinstance(expected, (list, tuple)):
        if len(expected) != len(actual):
            out.append("%s: len %d != %d" % (path, len(expected), len(actual)))
        for index, (e, a) in enumerate(zip(expected, actual)):
            diff_dicts(e, a, "%s[%d]" % (path, index), out)
    else:
        if expected != actual:
            out.append("%s: %r != %r" % (path, expected, actual))
    return out


def main():
    if not parse_args(sys.argv[1:]):
        return 2
    print("reference dir:", REF)
    print("generated dir:", OUT)
    print()

    # ---- account_sync_full ----
    expected = load_pickle(os.path.join(REF, "account_sync_full.pickle"))
    actual = load_pickle(os.path.join(OUT, "account_sync_full.pickle"))
    diffs = diff_dicts(expected, actual)
    check("account_sync_full: structure equal to reference", not diffs)
    for d in diffs[:20]:
        print("   ", d)

    # every reference compact descriptor compares byte for byte
    exp_comp = expected["inventory"][1]["compDescr"]
    act_comp = actual["inventory"][1]["compDescr"]
    mismatched = [(k, exp_comp[k], act_comp[k]) for k in exp_comp
                  if exp_comp.get(k) != act_comp.get(k)]
    check("account_sync_full: 130 vehCompDescr bytes identical to reference",
          not mismatched and len(act_comp) == 130,
          "(%d entries, %d mismatched)" % (len(act_comp), len(mismatched)))
    for k, e, a in mismatched[:10]:
        print("    invID %s: ref %r ours %r" % (k, e, a))

    # unpack the 15-byte descriptor of a few known vehicles through the inventory ids
    sys.path.insert(0, os.path.join(os.path.dirname(REF), "_tools"))
    known = {1: "T-34", 46: "PzIV", 92: "T1_Cunningham"}
    for inv_id, name in known.items():
        descr = act_comp.get(inv_id)
        check("account_sync_full: invID %d is %s 15-byte str" % (inv_id, name),
              isinstance(descr, str) and len(descr) == 15,
              repr(descr))

    # ---- stream payloads and descriptions ----
    for base in ("account_sync_full", "shop_cache", "dossier_sync"):
        ref_z = load_raw(os.path.join(REF, base + ".pickle.z"))
        our_z = load_raw(os.path.join(OUT, base + ".pickle.z"))
        ref_pickled = zlib.decompress(ref_z)
        our_pickled = zlib.decompress(our_z)
        check("%s.pickle.z decompresses to same object as reference" % base,
              cPickle.loads(ref_pickled) == cPickle.loads(our_pickled),
              "(ref zlib %d -> pickle %d, ours zlib %d -> pickle %d)" % (
                  len(ref_z), len(ref_pickled), len(our_z), len(our_pickled)))
        ref_desc = load_pickle(os.path.join(REF, base + ".desc"))
        our_desc = load_pickle(os.path.join(OUT, base + ".desc"))
        check("%s.desc == (len(zlib), crc32(zlib)) of our payload" % base,
              our_desc == (len(our_z), zlib.crc32(our_z) if zlib.crc32(our_z) < 0x80000000
                           else zlib.crc32(our_z) - 0x100000000),
              repr(our_desc))
        check("%s.desc shape matches reference" % base,
              isinstance(our_desc, tuple) and len(our_desc) == 2
              and isinstance(our_desc[0], (int, long)) and isinstance(our_desc[1], (int, long)),
              "ref %r ours %r" % (ref_desc, our_desc))

    # ---- showGUI ctx ----
    ref_gui = load_pickle(os.path.join(REF, "showgui_ctx.pickle"))
    our_gui = load_pickle(os.path.join(OUT, "showgui_ctx.pickle"))
    check("showgui_ctx: databaseID present/int",
          isinstance(our_gui.get("databaseID"), (int, long)),
          repr(our_gui))
    check("showgui_ctx: serverUTC present/int",
          isinstance(our_gui.get("serverUTC"), (int, long)))
    check("showgui_ctx: same keys as reference",
          set(ref_gui.keys()).issubset(set(our_gui.keys())),
          "ref keys %r ours %r" % (sorted(ref_gui.keys()), sorted(our_gui.keys())))

    # ---- catalog json ----
    import json
    ref_cat = json.load(open(os.path.join(REF, "vehicles_catalog.json")))
    our_cat = json.load(open(os.path.join(OUT, "vehicles_catalog.json")))
    check("catalog: count field", our_cat.get("count") == 130 == ref_cat.get("count"),
          repr(our_cat.get("count")))
    ref_vehicles = ref_cat["vehicles"]
    our_vehicles = our_cat["vehicles"]
    check("catalog: 130 vehicles in the same (nationID, typeID) order",
          len(our_vehicles) == 130 and
          [(v["nationID"], v["typeID"]) for v in ref_vehicles] ==
          [(v["nationID"], v["typeID"]) for v in our_vehicles])
    fields = ("nation", "nationID", "typeID", "name", "level", "price", "tags",
              "vehTypeCompDescr", "vehCompDescrHex", "vehCompDescrB64", "vehCompDescrLen",
              "components", "componentIDs")
    bad = []
    for ref_v, our_v in zip(ref_vehicles, our_vehicles):
        for field in fields:
            if ref_v.get(field) != our_v.get(field):
                bad.append("%s.%s: ref %r ours %r" % (ref_v.get("name"), field,
                                                      ref_v.get(field), our_v.get(field)))
    check("catalog: every field of all 130 vehicles identical to reference", not bad,
          "(%d mismatches)" % len(bad))
    for line in bad[:20]:
        print("   ", line)

    print()
    if failures:
        print("FAILED: %d check(s)" % len(failures))
        return 1
    print("ALL CHECKS PASSED")
    return 0


if __name__ == "__main__":
    sys.exit(main())
