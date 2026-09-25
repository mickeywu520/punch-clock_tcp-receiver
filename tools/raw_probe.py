"""原始位元組探測工具 —— 用來釐清某個 Port 到底吃什麼格式。

用途：當 `punch_admin.py info` 連得上卻「連線被對方關閉」時，用這支工具確認：
  1. 這個 Port 是「被動監聽」還是「連上就斷」
  2. 該送短封包（0x7E）、長封包（FF 00 5A A5）還是 HTTP
  3. Node ID（DID）是否正確（0 / 1 / 255 廣播）

用法：
    python tools/raw_probe.py --host 192.168.1.127 --port 1621 --scan
    python tools/raw_probe.py --host 192.168.1.127 --port 1621 --send-hex "7E 04 01 18 E6 FF"
"""

from __future__ import annotations

import argparse
import socket
import sys
import time

import soyal_proto as sp

if sys.platform == "win32":
    for _stream in (sys.stdout, sys.stderr):
        try:
            _stream.reconfigure(encoding="utf-8")  # type: ignore[union-attr]
        except Exception:  # pragma: no cover
            pass


def _dump(raw: bytes) -> str:
    printable = "".join(chr(b) if 32 <= b < 127 else "." for b in raw[:120])
    return f"{raw.hex(' ').upper()}\n           |{printable}|" if raw else "(無回應)"


def one_shot(host: str, port: int, payload: bytes | None, timeout: float, wait: float) -> str:
    """連線 →（可選）送出 payload → 讀取回應，回傳結果描述。"""
    try:
        with socket.create_connection((host, port), timeout=timeout) as s:
            s.settimeout(timeout)
            if payload is None:
                # 只觀察連線是否被動關閉
                try:
                    data = s.recv(1024)
                    return "對方主動送資料" if data else "連線被對方關閉（未送任何資料）"
                except socket.timeout:
                    return "連線保持（被動監聽，等我們先送）"
                except ConnectionResetError:
                    return "連線被重置（RST）"
            s.sendall(payload)
            time.sleep(wait)
            try:
                data = s.recv(4096)
            except socket.timeout:
                return "已送出，但等待逾時（無回應）"
            except ConnectionResetError:
                return "已送出 → 連線被重置（RST）"
            if not data:
                return "已送出 → 連線被對方關閉（無回應）"
            return f"已送出 → 收到回應：\n           {_dump(data)}"
    except OSError as e:
        return f"連線失敗：{type(e).__name__}: {e}"


def scan(host: str, port: int, timeout: float) -> int:
    """依序嘗試多種格式，找出這個 Port 能吃哪一種。"""
    print(f"== 掃描 {host}:{port}（timeout {timeout}s） ==\n")

    print("[0] 不送任何資料，觀察是否被動監聽")
    print(f"    → {one_shot(host, port, None, timeout, 0)}\n")

    cases: list[tuple[str, bytes]] = [
        ("短封包 18H Polling（DID=1）", sp.build_short(0x01, 0x18)),
        ("短封包 18H Polling（DID=0）", sp.build_short(0x00, 0x18)),
        ("短封包 18H Polling（DID=255 廣播）", sp.build_short(0xFF, 0x18)),
        ("長封包 18H Polling（DID=1, TCP/IP 格式）", sp.build_large(0x01, 0x18)),
        ("長封包 18H Polling（DID=0）", sp.build_large(0x00, 0x18)),
        ("短封包 24H 讀時間（DID=1）", sp.build_short(0x01, 0x24)),
        ("HTTP GET /", b"GET / HTTP/1.0\r\nHost: " + host.encode() + b"\r\n\r\n"),
    ]
    hits = 0
    for name, payload in cases:
        print(f"[{name}]")
        print(f"    TX {payload.hex(' ').upper()}")
        result = one_shot(host, port, payload, timeout, 0.4)
        print(f"    → {result}")
        if "收到回應" in result:
            hits += 1
            print("    ✅ 這個格式有回應")
        print()

    if hits:
        print(f"共 {hits} 種格式得到回應 —— 請以上面有回應的格式為準調整工具參數。")
        return 0
    print(
        "沒有任何格式得到回應。可能原因：\n"
        "  1. Port 不是 SOYAL 協定埠（請看打卡機網頁 Network Setting 的『TCP Port』）\n"
        "  2. 控制器啟用了加密（Security / DES / 3DES）模式 → 需改用 0x7F 加密封包\n"
        "  3. 該 Port 為 Work Mode=TCP Client（對外連線）而非 TCP Server\n"
        "  4. 需要先通過密碼 / 站號驗證，或 DID（Node ID）不同"
    )
    return 1


def main() -> int:
    ap = argparse.ArgumentParser(description="原始位元組探測（釐清 Port 通訊格式）")
    ap.add_argument("--host", default="192.168.1.127")
    ap.add_argument("--port", type=int, default=1621)
    ap.add_argument("--timeout", type=float, default=3.0)
    ap.add_argument("--scan", action="store_true", help="依序嘗試多種格式（建議先跑這個）")
    ap.add_argument("--send-hex", help="送出指定 HEX 位元組，例如 \"7E 04 01 18 E6 FF\"")
    args = ap.parse_args()

    if args.send_hex:
        payload = bytes.fromhex(args.send_hex.replace(" ", ""))
        print(f"TX {payload.hex(' ').upper()}")
        print(f"→ {one_shot(args.host, args.port, payload, args.timeout, 0.4)}")
        return 0

    # 預設就是掃描；--scan 只是明確標示意圖
    return scan(args.host, args.port, args.timeout)


if __name__ == "__main__":
    raise SystemExit(main())