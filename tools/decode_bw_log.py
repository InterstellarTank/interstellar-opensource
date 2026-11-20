#!/usr/bin/env python3
"""Decode captured BigWorld packets (UDP hex logs) for debugging.

Usage:
  python tools/decode_bw_log.py .scratch/logs/udp-20017.hex --session .scratch/e2e/session.json
  python tools/decode_bw_log.py .scratch/logs/udp-20016.hex --no-crypt

The log format is the one written by tools/udp_log.py and tools/loginapp_proto.py:
  <ISO timestamp> RX from=<ip:port> len=<n>
  <hex lines, 32 bytes each>
"""
import argparse
import json
import re
import struct
import sys

try:
    from cryptography.hazmat.decrepit.ciphers.algorithms import Blowfish
except Exception:  # noqa: BLE001
    from cryptography.hazmat.primitives.ciphers.algorithms import Blowfish  # type: ignore
from cryptography.hazmat.primitives.ciphers import Cipher, modes

FLAGS = {
    0x0001: "HAS_REQUESTS",
    0x0002: "HAS_PIGGYBACKS",
    0x0004: "HAS_ACKS",
    0x0008: "ON_CHANNEL",
    0x0010: "IS_RELIABLE",
    0x0020: "IS_FRAGMENT",
    0x0040: "HAS_SEQUENCE_NUMBER",
    0x0080: "INDEXED_CHANNEL",
    0x0100: "HAS_CHECKSUM",
    0x0200: "CREATE_CHANNEL",
    0x0400: "HAS_CUMULATIVE_ACK",
}


def flags_str(f: int) -> str:
    return "|".join(name for bit, name in FLAGS.items() if f & bit) or "0"


def blowfish_decrypt(key: bytes, data: bytes) -> bytes:
    cipher = Cipher(Blowfish(key), modes.ECB())
    dec = cipher.decryptor()
    out = bytearray()
    prev = b"\x00" * 8
    for i in range(0, len(data) - len(data) % 8, 8):
        block = dec.update(data[i : i + 8])
        out += bytes(a ^ b for a, b in zip(block, prev))
        prev = bytes(a ^ b for a, b in zip(block, prev))
    dec.finalize()
    return bytes(out)


def strip_bw_encryption(key: bytes, data: bytes):
    """Decrypt a BW encrypted packet; returns (plain_packet, wastage) or (None, err)."""
    if not key or len(data) % 8:
        return None, "not block-aligned"
    dec = blowfish_decrypt(key, data)
    wastage = dec[-1]
    if not (1 <= wastage <= 8) or len(dec) < wastage + 4:
        return None, f"bad wastage {wastage}"
    magic = dec[-5:-1]
    if magic != b"\xef\xbe\xad\xde":
        return None, f"bad magic {magic.hex()}"
    return dec[: len(dec) - 4 - wastage], None


def parse_packet(p: bytes):
    if len(p) < 2:
        return None
    f = int.from_bytes(p[:2], "little")
    out = [f"flags=0x{f:04x} ({flags_str(f)}) body={len(p)-2}B"]
    off = 2
    # attempt to iterate messages: needs footer knowledge, so just show head
    while off + 3 <= len(p) and off < 2 + 64:
        mid = p[off]
        if mid == 0xFF:
            ln = int.from_bytes(p[off + 1 : off + 5], "little")
            out.append(f"  REPLY len={ln} data={p[off+5:off+5+min(ln,32)].hex()}")
            break
        # assume variable-2 for unknown; will be wrong for fixed-length interfaces
        ln = int.from_bytes(p[off + 1 : off + 3], "little")
        body = p[off + 3 : off + 3 + min(ln, 32)]
        out.append(f"  msg id={mid} len={ln} body={body.hex()}")
        break
    return "\n".join(out)


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("log")
    ap.add_argument("--session", help="session json with encryption_key_hex")
    ap.add_argument("--key-hex")
    ap.add_argument("--no-crypt", action="store_true")
    a = ap.parse_args()

    key = None
    if a.key_hex:
        key = bytes.fromhex(a.key_hex)
    elif a.session and not a.no_crypt:
        key = bytes.fromhex(json.load(open(a.session))["encryption_key_hex"])

    pkts = []
    cur = None
    for ln in open(a.log, encoding="utf-8", errors="replace").read().splitlines():
        if re.match(r"^\d{4}-\d\d-\d\d", ln) or ln.startswith("#"):
            cur = []
            pkts.append(cur)
            continue
        if cur is not None and re.fullmatch(r"[0-9a-f]+", ln):
            cur.append(bytes.fromhex(ln))
    datas = [b"".join(p) for p in pkts if p]

    for i, data in enumerate(datas):
        print(f"--- packet {i}: {len(data)} bytes: {data[:24].hex()}...")
        plain = None
        if key:
            plain, err = strip_bw_encryption(key, data)
            if plain is None:
                print(f"  decrypt failed: {err}")
        if plain is not None:
            print(f"  decrypted: {plain.hex()}")
            print(parse_packet(plain))
        else:
            print(parse_packet(data))
    return 0


if __name__ == "__main__":
    sys.exit(main())
