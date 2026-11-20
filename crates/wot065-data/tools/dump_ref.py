"""Dump structure of reference payloads (Python 2.7) for wot065-data development."""
import cPickle
import os
import sys
import zlib

BASE = os.path.abspath(
    os.path.join(
        os.path.dirname(__file__),
        "..",
        "..",
        "..",
        ".scratch",
        "research",
        "decompiled",
        "_generated",
    )
)


def load(name):
    path = BASE + "\\" + name
    with open(path, "rb") as f:
        data = f.read()
    return data


def summarize(v, depth=0, maxdepth=4):
    ind = "  " * depth
    if isinstance(v, dict):
        print("%sdict[%d]" % (ind, len(v)))
        if depth < maxdepth:
            for k in sorted(v.keys(), key=lambda x: repr(x))[:40]:
                print("%skey=%r:" % (ind, k))
                summarize(v[k], depth + 1, maxdepth)
    elif isinstance(v, (list, tuple)):
        print("%s%s[%d]: %r" % (ind, type(v).__name__, len(v), v if depth >= maxdepth else v[:8]))
        if depth < maxdepth and len(v) < 12:
            for x in v:
                summarize(x, depth + 1, maxdepth)
    elif isinstance(v, set):
        print("%sset[%d]" % (ind, len(v)))
    else:
        print("%s%r" % (ind, v))


def main():
    for name in ("account_sync_full.pickle", "account_sync_full.pickle.z",
                 "showgui_ctx.pickle", "shop_cache.pickle", "shop_cache.pickle.z",
                 "dossier_sync.pickle", "dossier_sync.pickle.z",
                 "account_update_example.pickle",
                 "account_sync_full.desc", "shop_cache.desc", "dossier_sync.desc"):
        data = load(name)
        print("=" * 20, name, "len", len(data))
        obj = data
        if name.endswith(".z"):
            try:
                obj = zlib.decompress(data)
                print("zlib ->", len(obj))
            except Exception as e:
                print("zlib error", e)
        try:
            obj = cPickle.loads(obj)
        except Exception as e:
            print("pickle error:", e)
            continue
        if name.endswith(".desc"):
            print("desc:", repr(obj))
            continue
        summarize(obj)
    return 0


if __name__ == "__main__":
    sys.exit(main())
