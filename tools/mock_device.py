"""假打卡機（Mock SOYAL controller）—— 用來離線驗證 punch_admin.py。

沒有實機時，先跑這個模擬器，再對它下指令，就能確認「送收 → 解析 → 顯示」整條路徑正常：

    # 視窗 1：啟動模擬器（預設 127.0.0.1:1621）
    python tools/mock_device.py --port 1621

    # 視窗 2：對模擬器下指令（輸出格式與真機相同）
    python tools/punch_admin.py info     --host 127.0.0.1 --port 1601
    python tools/punch_admin.py add-user --host 127.0.0.1 --port 1601 `
        --addr 100 --uid 00000000D4B81403 --yes
    python tools/punch_admin.py get-user --host 127.0.0.1 --port 1601 --addr 100
    python tools/punch_admin.py dump-log --host 127.0.0.1 --port 1601

支援指令：18H 輪詢、24H 讀時間、25H 讀事件記錄、37H 移除事件記錄、
83H/84H 新增人員（存記憶體）、85H 刪除、87H 讀取人員、2EH 寫姓名。

⚠️ 這是模擬器，只驗證「封包格式與流程」，**不代表真機的位元組順序**。
   真機佈署前務必用 punch_admin.py verify 回讀確認（--addr-order / --uid-order）。
"""

from __future__ import annotations

import argparse
import socket
import socketserver
import sys
import time

import soyal_proto as sp

if sys.platform == "win32":
    for _stream in (sys.stdout, sys.stderr):
        try:
            _stream.reconfigure(encoding="utf-8")  # type: ignore[union-attr]
        except Exception:  # pragma: no cover
            pass

#: 25H 事件記錄資料（取自 靈活使用技巧 p.59 官方範例：2021-02-25 18:01:14、Port 17）
DEFAULT_LOG = bytes.fromhex("010E011205190215114AFE000010008EA101004AFE0000000000000000")


class MockState:
    """模擬控制器內部狀態。"""

    def __init__(
        self,
        did: int = 1,
        controller_type: int = 0xC0,
        firmware: int = 0x45,
        record_len: int = 26,
    ):
        self.did = did
        self.controller_type = controller_type
        self.firmware = firmware
        self.record_len = record_len
        self.users: dict[int, bytes] = {}   # user address -> 26 bytes record
        self.aliases: dict[int, bytes] = {}  # user address -> 16 bytes name
        self.logs: list[bytes] = [DEFAULT_LOG]


def _status_ack(state: MockState) -> bytes:
    """ACK（0x04）附帶狀態 —— 完全比照實機擷取框：

    實機：``7E 0F 00 04 01 C3 46 0F 91 10 10 00 00 00 00 E1 AF``
    （data = source, type, firmware, input port, relay, 10, 10, 00, 00, 00, 00）
    """
    data = bytes(
        [
            0x01,  # source node
            state.controller_type,
            state.firmware,
            0x0F,  # input port status
            0x91,  # relay status
            0x10,
            0x10,
            0x00,
            0x00,
            0x00,
            0x00,
        ]
    )
    return sp.build_short(0x00, 0x04, data)


def _reader_status(state: MockState) -> bytes:
    """18H 回覆（echo 0x09）—— 比照實機：``7E 0A 00 09 01 00 01 00 10 40 A6 01``"""
    return sp.build_short(0x00, 0x09, bytes([0x01, 0x00, 0x01, 0x00, 0x10, 0x40]))


def _data_response(payload: bytes) -> bytes:
    """資料回覆（echo 0x03）：DID=00 + payload。"""
    return sp.build_short(0x00, 0x03, payload)


def handle(state: MockState, pkt: sp.Packet) -> bytes | None:
    """依指令產生回覆。"""
    cmd, did, data = pkt.cmd, pkt.did, pkt.data

    if cmd == 0x18:  # Hosting Polling
        return _reader_status(state)

    if cmd == 0x24:  # Get RTC（比照實機：source + 7 時間欄位 + 版本 + 7 其他 + 16×0x80）
        now = time.localtime()
        payload = bytes(
            [
                0x01,  # source node
                now.tm_sec,
                now.tm_min,
                now.tm_hour,
                (now.tm_wday + 1) % 7 + 1,
                now.tm_mday,
                now.tm_mon,
                now.tm_year % 100,
                state.firmware,
                0x01,
                0x02,
                0x00,
                state.controller_type,
                0x00,
                0x04,
                0x01,
            ]
        ) + b"\x80" * 16
        return _data_response(payload)

    if cmd == 0x25:  # Get oldest event log
        if not state.logs:
            return _status_ack(state)  # 無記錄時回 ACK（與真機行為相符）
        return _data_response(state.logs[0])

    if cmd == 0x37:  # Remove oldest event log
        if state.logs:
            state.logs.pop(0)
        return _status_ack(state)

    if cmd in (0x83, 0x84):  # Set User Parameters
        if not data:
            return sp.build_short(0x00, 0x05)
        records = data[0]
        rec_len = state.record_len
        for i in range(records):
            chunk = data[1 + i * rec_len : 1 + (i + 1) * rec_len]
            if len(chunk) < rec_len:
                return sp.build_short(0x00, 0x05)
            state.users[sp.decode_addr(chunk[0:2])] = chunk
        print(f"  [mock] 新增/更新 {records} 筆人員，目前共 {len(state.users)} 筆")
        return _status_ack(state)

    if cmd == 0x85:  # Erase user data
        if len(data) < 4:
            return sp.build_short(0x00, 0x05)
        start = sp.decode_addr(data[0:2])
        end = sp.decode_addr(data[2:4])
        removed = [a for a in state.users if start <= a <= end]
        for a in removed:
            del state.users[a]
        print(f"  [mock] 刪除位址 {start}~{end}，共 {len(removed)} 筆")
        return _status_ack(state)

    if cmd == 0x87:  # Get User Parameters（實機回傳 24 bytes/筆，且不含位址）
        addr = sp.decode_addr(data[0:2])
        nums = data[2] if len(data) > 2 else 1
        payload = b""
        found = 0
        for i in range(nums):
            rec = state.users.get(addr + i)
            if rec is None:
                break
            payload += rec[2:]  # 去掉開頭 2 bytes 位址（與實機一致）
            found += 1
        if found == 0:
            # 實機對未使用位址回傳 24 bytes 全 0xFF（最後 4 bytes 為 0x00）
            print(f"  [mock] 位址 {addr} 空白 → 回傳 0xFF 記錄")
            empty = b"\xff" * 20 + b"\x00" * 4
            return _data_response(bytes([did]) + empty)
        # 資料回覆：Source ID(1) + N*record(24)
        return _data_response(bytes([did]) + payload)

    if cmd == 0x2E:  # Write user alias
        index = sp.decode_addr(data[0:3])
        records = data[3] if len(data) > 3 else 0
        for i in range(records):
            state.aliases[index + i] = data[4 + i * 16 : 4 + (i + 1) * 16]
        print(f"  [mock] 寫入姓名 index={index} records={records}")
        return _status_ack(state)

    print(f"  [mock] 未支援的指令 0x{cmd:02X} → NACK")
    return sp.build_short(0x00, 0x05)


class Handler(socketserver.BaseRequestHandler):
    """每個 TCP 連線一個 handler（真機也是長連線模式）。"""

    def handle(self) -> None:
        state: MockState = self.server.state  # type: ignore[attr-defined]
        peer = f"{self.client_address[0]}:{self.client_address[1]}"
        print(f"[mock] 連線來自 {peer}")
        buf = b""
        self.request.settimeout(30)
        while True:
            try:
                chunk = self.request.recv(4096)
            except (socket.timeout, ConnectionResetError):
                break
            if not chunk:
                break
            buf += chunk
            while len(buf) >= 5:
                if buf[0] != sp.HEAD_SHORT:
                    buf = buf[1:]
                    continue
                total = buf[1] + 2
                if len(buf) < total:
                    break
                raw, buf = buf[:total], buf[total:]
                try:
                    pkt = sp.parse_packet(raw)
                except sp.SoyalError as e:
                    print(f"  [mock] 解析失敗: {e}")
                    continue
                print(f"  [mock] RX {pkt.hex()} → CMD 0x{pkt.cmd:02X}")
                reply = handle(state, pkt)
                if reply:
                    print(f"  [mock] TX {reply.hex(' ').upper()}")
                    self.request.sendall(reply)
        print(f"[mock] 連線結束 {peer}")


class Server(socketserver.ThreadingTCPServer):
    allow_reuse_address = True
    daemon_threads = True


def main() -> int:
    ap = argparse.ArgumentParser(description="假 SOYAL 控制器（測試 punch_admin.py 用）")
    ap.add_argument("--bind", default="127.0.0.1")
    ap.add_argument("--port", type=int, default=1621)
    ap.add_argument("--did", type=lambda s: int(s, 0), default=1, help="模擬控制器 Node ID")
    ap.add_argument("--record-len", type=int, default=26, help="人員記錄長度（預設 26）")
    ap.add_argument(
        "--controller-type", type=lambda s: int(s, 0), default=0xC0, help="0xC0=AR-881E（預設）"
    )
    args = ap.parse_args()

    state = MockState(
        did=args.did, controller_type=args.controller_type, record_len=args.record_len
    )
    with Server((args.bind, args.port), Handler) as srv:
        srv.state = state  # type: ignore[attr-defined]
        print(f"[mock] 假打卡機已啟動 {args.bind}:{args.port}（Ctrl-C 結束）")
        try:
            srv.serve_forever()
        except KeyboardInterrupt:
            print("\n[mock] 結束")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())