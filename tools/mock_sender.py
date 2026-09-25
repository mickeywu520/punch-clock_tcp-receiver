import argparse
import socket
import time

# TEXT 模式範例行（吃字串會在送出時以 UTF-8 編碼）；
# 含中文別名（王小明）以驗證接收端 UTF-8 行解析。
SAMPLES = [
    "21'05/12 13:38:54 [001.17:0B](0)00000000D4B81403 rSammi                (M11)Normal Access",
    "21'05/12 13:39:02 [001.17:0B](0)00000000D4B81403 rSammi                (M11)Normal Access",
    "21'05/12 14:10:33 [002.17:1C](2)0C73A9B1 王小明                (M28)Access by PIN",
    "22'01/03 08:05:01 [010.17:11](0)12345678ABCDEF00 LiHua               (M39)Access by fingerprint",
]


def main():
    ap = argparse.ArgumentParser(description="Simulate SOYAL AR837EF pushing 8031 TEXT records")
    ap.add_argument("--host", default="127.0.0.1")
    ap.add_argument("--port", type=int, default=8031)
    ap.add_argument("--count", type=int, default=0, help="0 = loop forever")
    ap.add_argument("--interval", type=float, default=2.0, help="seconds between records")
    args = ap.parse_args()

    n = 0
    while args.count == 0 or n < args.count:
        try:
            with socket.create_connection((args.host, args.port), timeout=10) as sock:
                print(f"connected to {args.host}:{args.port}")
                i = 0
                while args.count == 0 or n < args.count:
                    payload = SAMPLES[i % len(SAMPLES)].encode("utf-8") + b"\n"
                    sock.sendall(payload)
                    print(f"  [{n+1}] sent: {payload.decode('utf-8', errors='replace').strip()}")
                    n += 1
                    i += 1
                    time.sleep(args.interval)
        except (ConnectionRefusedError, BrokenPipeError) as e:
            print(f"connection issue ({e}); retrying in 3s...")
            time.sleep(3)


if __name__ == "__main__":
    main()