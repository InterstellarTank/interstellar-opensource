#!/usr/bin/env python3
"""Raw UDP datagram logger for the WoT 0.6.5 login endpoint (127.0.0.1:20016).

Logs every datagram as: <timestamp> dir=RX len=<n>\n<hex>\n
This is a protocol-reverse-engineering aid; it replies to nothing.
"""
import argparse
import datetime
import socket
import sys
import time


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--bind", default="127.0.0.1:20016")
    ap.add_argument("--out", default="udp-20016.hex")
    ap.add_argument("--count", type=int, default=0, help="stop after N datagrams (0 = forever)")
    ap.add_argument("--timeout", type=float, default=0.0, help="exit after N seconds idle after first packet")
    args = ap.parse_args()

    host, port = args.bind.rsplit(":", 1)
    port = int(port)
    sock = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
    sock.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
    sock.bind((host, port))
    out = open(args.out, "w", encoding="utf-8", buffering=1)
    out.write(f"# udp logger bound {host}:{port}\n")

    total = 0
    last = None
    while True:
        try:
            data, addr = sock.recvfrom(65535)
        except KeyboardInterrupt:
            break
        total += 1
        now = datetime.datetime.now().isoformat(timespec="milliseconds")
        last = time.time()
        out.write(f"{now} RX from={addr[0]}:{addr[1]} len={len(data)}\n")
        for i in range(0, len(data), 32):
            out.write(data[i : i + 32].hex() + "\n")
        out.write("\n")
        if args.count and total >= args.count:
            break
        if args.timeout:
            sock.settimeout(args.timeout)
            try:
                data2, addr2 = sock.recvfrom(65535)
                # process it normally on next loop iteration
                sock.settimeout(None)
                # push back by handling immediately
                total += 1
                now = datetime.datetime.now().isoformat(timespec="milliseconds")
                out.write(f"{now} RX from={addr2[0]}:{addr2[1]} len={len(data2)}\n")
                for i in range(0, len(data2), 32):
                    out.write(data2[i : i + 32].hex() + "\n")
                out.write("\n")
                last = time.time()
                sock.settimeout(None)
            except socket.timeout:
                out.write("# timeout, exiting\n")
                break
    out.close()
    print(f"logged {total} datagrams to {args.out}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
