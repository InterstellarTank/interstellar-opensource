#!/usr/bin/env python3
"""WoT 0.6.5 LoginApp reference prototype (validation harness, NOT the deliverable).

Purpose: prove the BW 2.0.1 login wire format against the real retail client and
capture the client's BaseApp handshake for the Rust implementation.

Flow implemented:
  client -> UDP 20016:  [u16 flags=1][u8 id=0][u16 len][i32 reply_id][u16 next=0]
                        [u32 LOGIN_VERSION][u32 unknown][RSA-OAEP-SHA1(LogOnParams)]
                        [u16 first_request_offset=2]
  LogOnParams plaintext: u8 flags, packed-string username, password, encryption_key,
                         [16B digest if flags&1], u32 nonce
  reply -> UDP:         [u16 flags=0][u8 id=0xFF][u32 len=4+payload][i32 reply_id]
                        [u8 LOGGED_ON=1][Blowfish(Address{ip,port,salt}+u32 session_key)]
  Blowfish: ECB with XOR chaining on the previous *plaintext* block (IV=0), zero padded.

Also binds UDP 20017 to log the client's post-login BaseApp packets (hex dump).
Writes the session handoff JSON for the Rust BaseApp:
  {"username","encryption_key_hex","session_key","client_addr"}
"""
import argparse
import datetime
import json
import re
import socket
import struct
import sys
import threading
import time

from cryptography.hazmat.primitives import serialization, hashes
from cryptography.hazmat.primitives.asymmetric import padding
from cryptography.hazmat.primitives.ciphers import Cipher, modes

try:  # cryptography >= 43 moved Blowfish to decrepit
    from cryptography.hazmat.decrepit.ciphers.algorithms import Blowfish
except Exception:  # noqa: BLE001
    from cryptography.hazmat.primitives.ciphers.algorithms import Blowfish  # type: ignore

MAGIC = b"\x45\x4E\xA1\x62"


def log(msg: str) -> None:
    print(f"[{datetime.datetime.now().isoformat(timespec='milliseconds')}] {msg}", flush=True)


def blowfish_encrypt(key: bytes, data: bytes) -> bytes:
    if len(data) % 8:
        data += b"\x00" * (8 - len(data) % 8)
    out = bytearray()
    prev = b"\x00" * 8
    cipher = Cipher(Blowfish(key), modes.ECB())
    enc = cipher.encryptor()
    for i in range(0, len(data), 8):
        block = bytes(a ^ b for a, b in zip(data[i : i + 8], prev))
        out += enc.update(block)
        prev = data[i : i + 8]
    out += enc.finalize()
    return bytes(out)


def blowfish_decrypt(key: bytes, data: bytes) -> bytes:
    cipher = Cipher(Blowfish(key), modes.ECB())
    dec = cipher.decryptor()
    out = bytearray()
    prev = b"\x00" * 8
    for i in range(0, len(data), 8):
        block = dec.update(data[i : i + 8])
        plain = bytes(a ^ b for a, b in zip(block, prev))
        out += plain
        prev = plain
    dec.finalize()
    return bytes(out)


def read_packed_int(buf: bytes, off: int):
    v = buf[off]
    if v == 0xFF:
        return buf[off + 1] | (buf[off + 2] << 8) | (buf[off + 3] << 16), off + 4
    return v, off + 1


def parse_logon_params(buf: bytes):
    off = 0
    flags = buf[off]
    off += 1
    fields = {}
    for name in ("username", "password", "encryption_key"):
        ln, off = read_packed_int(buf, off)
        fields[name] = buf[off : off + ln]
        off += ln
    digest = b""
    if flags & 1:
        digest = buf[off : off + 16]
        off += 16
    nonce = int.from_bytes(buf[off : off + 4], "little")
    off += 4
    fields.update(flags=flags, digest=digest, nonce=nonce, consumed=off)
    return fields


class LoginProto:
    def __init__(self, args):
        self.args = args
        self.priv = serialization.load_pem_private_key(open(args.priv_key, "rb").read(), password=None)
        self.pad = padding.OAEP(mgf=padding.MGF1(algorithm=hashes.SHA1()), algorithm=hashes.SHA1(), label=None)
        self.sock = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
        self.sock.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
        self.sock.bind((args.bind_host, args.bind_port))
        self.seen = {}
        log(f"loginapp prototype listening on {args.bind_host}:{args.bind_port}")

    def try_decrypt(self, args: bytes):
        """Find the RSA ciphertext: try candidate offsets, return (offset, plaintext)."""
        for skip in (4, 0, 8, 2, 1, 3, 5, 6):
            blob = args[skip:]
            if len(blob) < 256 or len(blob) % 256:
                # tolerate a small trailing slack
                if len(blob) < 256:
                    continue
                blob = blob[: 256 * (len(blob) // 256)]
            try:
                plain = b""
                for i in range(0, len(blob), 256):
                    plain += self.priv.decrypt(blob[i : i + 256], self.pad)
                return skip, plain
            except Exception:  # noqa: BLE001
                continue
        return None, None

    def handle(self, data: bytes, addr):
        if len(data) < 13:
            log(f"short packet {len(data)} from {addr}")
            return
        flags = int.from_bytes(data[0:2], "little")
        mid = data[2]
        length = int.from_bytes(data[3:5], "little")
        reply_id = int.from_bytes(data[5:9], "little", signed=True)
        nxt = int.from_bytes(data[9:11], "little")
        args = data[11 : 11 + length]
        footer = data[11 + length :]
        log(
            f"RX {len(data)}B from {addr} flags=0x{flags:04x} id={mid} len={length} "
            f"reply_id={reply_id} next={nxt} footer={footer.hex()}"
        )
        version = int.from_bytes(args[0:4], "little")
        log(f"  LOGIN_VERSION={version} args_head={args[4:8].hex()} args_len={len(args)}")
        skip, plain = self.try_decrypt(args)
        if plain is None:
            log("  RSA decrypt FAILED (no offset worked)")
            return
        log(f"  RSA plaintext (skip={skip}, {len(plain)}B): {plain.hex()}")
        params = parse_logon_params(plain)
        log(
            f"  username={params['username']!r} password={params['password']!r} "
            f"enc_key={params['encryption_key'].hex()} nonce={params['nonce']}"
        )
        self.answer(addr, reply_id, params, version)

    def answer(self, addr, reply_id, params, version):
        session_key = self.args.session_key
        record = struct.pack(
            "<IIHH", 0x0100007F, 0, 0, 0
        )  # placeholder, replaced below
        # Mercury::Address = raw u32 ip (LE), u16 port in network byte order (BE), u16 salt (LE)
        record = struct.pack("<I", 0x0100007F)  # 127.0.0.1 raw bytes 7f 00 00 01
        record += struct.pack(">H", self.args.base_port)
        record += struct.pack("<H", 0)
        record += struct.pack("<I", session_key)
        enc = blowfish_encrypt(params["encryption_key"], record)
        payload = bytes([1]) + enc  # LOGGED_ON
        pkt = struct.pack("<H", 0) + bytes([0xFF]) + struct.pack("<I", 4 + len(payload))
        pkt += struct.pack("<i", reply_id) + payload
        self.sock.sendto(pkt, addr)
        log(f"  TX {len(pkt)}B -> {addr}: {pkt.hex()}")
        # session handoff for the Rust BaseApp
        session = {
            "username": params["username"].decode("latin-1"),
            "encryption_key_hex": params["encryption_key"].hex(),
            "session_key": session_key,
            "client_addr": f"{addr[0]}:{addr[1]}",
            "login_version": version,
            "login_time": datetime.datetime.now().isoformat(timespec="seconds"),
        }
        with open(self.args.session_out, "w", encoding="utf-8") as f:
            json.dump(session, f)
        log(f"  wrote session file {self.args.session_out}")

    def run(self):
        while True:
            data, addr = self.sock.recvfrom(65535)
            try:
                self.handle(data, addr)
            except Exception as exc:  # noqa: BLE001
                log(f"error handling packet from {addr}: {exc!r}")


def baseapp_logger(args):
    sock = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
    sock.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
    sock.bind((args.bind_host, args.base_port))
    log(f"baseapp UDP logger listening on {args.bind_host}:{args.base_port}")
    with open(args.base_log, "a", encoding="utf-8") as out:
        while True:
            data, addr = sock.recvfrom(65535)
            ts = datetime.datetime.now().isoformat(timespec="milliseconds")
            out.write(f"{ts} RX from={addr[0]}:{addr[1]} len={len(data)}\n")
            for i in range(0, len(data), 32):
                out.write(data[i : i + 32].hex() + "\n")
            out.write("\n")
            out.flush()
            log(f"BASEAPP RX {len(data)}B from {addr}: {data[:32].hex()}...")


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--priv-key", default=r".scratch/wot-keys/loginapp_priv.pem")
    ap.add_argument("--bind-host", default="127.0.0.1")
    ap.add_argument("--bind-port", type=int, default=20016)
    ap.add_argument("--base-port", type=int, default=20017)
    ap.add_argument("--session-key", type=lambda s: int(s, 0), default=0x12345678)
    ap.add_argument("--session-out", default=r".scratch/e2e/session.json")
    ap.add_argument("--base-log", default=r".scratch/logs/udp-20017.hex")
    a = ap.parse_args()
    import os

    os.makedirs(os.path.dirname(a.session_out) or ".", exist_ok=True)
    threading.Thread(target=baseapp_logger, args=(a,), daemon=True).start()
    LoginProto(a).run()


if __name__ == "__main__":
    sys.exit(main())
