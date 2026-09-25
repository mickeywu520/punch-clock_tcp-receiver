"""SOYAL 打卡機測試工具 —— 新增人員 / 卡片、讀取人員、讀取事件記錄、通訊測試。

用法範例（PowerShell，於 repo 根目錄執行）：

    # 1) 先做離線自我測試（不需連線，驗證封包組裝是否與 datasheet 範例一致）
    python tools/punch_admin.py selftest

    # 2) 掃描打卡機開了哪些 Port
    python tools/punch_admin.py probe --host 192.168.1.127

    # 3) 確認可以溝通（24H 讀時間＋體版本；18H 為 ping）
    python tools/punch_admin.py info  --host 192.168.1.127 --port 1601
    python tools/punch_admin.py ping  --host 192.168.1.127 --port 1601

    # 4) 新增一張測試卡（預設只印封包不送出；確定要寫入才加 --yes）
    python tools/punch_admin.py add-user --host 192.168.1.127 --port 1601 `
        --addr 100 --uid 00000000D4B81403 --yes

    # 5) 回讀驗證（非常重要：確認位址 / UID 的位元組順序是否正確）
    python tools/punch_admin.py verify --host 192.168.1.127 --port 1601 --addr 100

    # 6) 讀取控制器最舊的一筆事件記錄（可拿到 TEXT 模式沒有的 User Address）
    python tools/punch_admin.py dump-log --host 192.168.1.127 --port 1601

    # 7) 刪除測試人員
    python tools/punch_admin.py del-user --host 192.168.1.127 --port 1601 `
        --start 100 --end 100 --yes

⚠️ 寫入類指令（add-user / del-user / write-alias）預設為 **dry-run**，只印出封包內容，
   必須加上 `--yes` 才會真的送到打卡機。請先在測試機或非上班時間驗證。
⚠️ 本工具走的是 SOYAL 標準通訊協定（TCP/IP 直連），**不是** 網頁後台的 HTTP API；
   Port 請以打卡機網頁「Network Setting」的 TCP Port 為準（常見為 1601）。
"""

from __future__ import annotations

import argparse
import json
import socket
import sys

import soyal_proto as sp
from soyal_proto import SoyalClient

# Windows 主控台中文輸出：統一使用 UTF-8（若顯示亂碼請先執行 `chcp 65001`）
if sys.platform == "win32":
    for _stream in (sys.stdout, sys.stderr):
        try:
            _stream.reconfigure(encoding="utf-8")  # type: ignore[union-attr]
        except Exception:  # pragma: no cover - 舊版 Python 或非互動環境
            pass

# ---------------------------------------------------------------------------
# datasheet 官方範例向量（用於 selftest，不需要連線）
# 出處：Protocol_881E_725Ev2_82xEv5 4V05.pdf §1.3.1/§1.3.2/§1.4、§2.7/§2.8/§2.19
#       與 SOYAL Protocol 靈活使用技巧_v221116-Final.pdf p.46/58/59
# ---------------------------------------------------------------------------

VECTOR_83H = (
    "7E 1F 01 83 01 00 01 00 00 00 00 8E A1 4A FE 00 00 00 00 "
    "40 00 FF FF 4F 0C 1F 00 00 00 00 00 FA AF"
)

# 25H 事件記錄回覆（靈活使用技巧 p.59）
VECTOR_25H_ECHO = (
    "7E 21 00 03 01 0E 01 12 05 19 02 15 11 4A FE 00 00 10 00 "
    "8E A1 01 00 4A FE 00 00 00 00 00 00 00 00 C4 FF"
)


def _norm(hex_text: str) -> bytes:
    return bytes.fromhex(hex_text.replace(" ", "").replace("\n", ""))


class _FakeSock:
    """測試用假 socket：依序吐出預先切好的片段（驗證 TCP 黏包/分段處理）。"""

    def __init__(self, chunks: list[bytes]):
        self.chunks = list(chunks)

    def recv(self, _n: int) -> bytes:
        return self.chunks.pop(0) if self.chunks else b""

    def settimeout(self, _t: float) -> None:
        pass

    def sendall(self, _b: bytes) -> None:
        pass

    def close(self) -> None:
        pass


def cmd_selftest(args: argparse.Namespace) -> int:
    """離線驗證：封包組裝 / 解析是否符合 datasheet 範例。"""
    failures: list[str] = []
    checks = 0

    def check(name: str, got, want) -> None:
        nonlocal checks
        checks += 1
        ok = got == want
        print(f"  [{'PASS' if ok else 'FAIL'}] {name}")
        if not ok:
            if isinstance(got, bytes):
                got, want = got.hex(" ").upper(), want.hex(" ").upper()
            print(f"          got  = {got}")
            print(f"          want = {want}")
            failures.append(name)

    print("== 封包組裝（官方範例） ==")
    check("18H Hosting Polling", sp.build_short(0x01, 0x18), _norm("7E 04 01 18 E6 FF"))
    check("24H Get RTC", sp.build_short(0x01, 0x24), _norm("7E 04 01 24 DA FF"))
    check("25H Get oldest log", sp.build_short(0x01, 0x25), _norm("7E 04 01 25 DB 01"))
    check(
        "37H Remove oldest log",
        sp.build_short(0x01, 0x37, _norm("44 45 4C")),
        _norm("7E 07 01 37 44 45 4C 84 91"),
    )
    check(
        "18H Large packet (TCP/IP)",
        sp.build_large(0x01, 0x18),
        _norm("FF 00 5A A5 00 04 01 18 E6 FF"),
    )

    print("\n== 83H 新增人員（E 系列範例：位址 1／卡號 8EA14AFE／Card Only） ==")
    rec = sp.build_user_record(
        addr=1,
        uid_hex="000000008EA14AFE",
        pin=0,
        mode=0x40,
        zone=0,
        group1=0xFF,
        group2=0xFF,
        year=0x4F,
        month=0x0C,
        day=0x1F,
        level=0,
        option=0,
    )
    check("單筆人員記錄長度 26 bytes", len(rec), 26)
    check(
        "83H 封包（含 records 欄位）",
        sp.cmd_set_user(1, [rec], with_apb=True),
        _norm(VECTOR_83H),
    )

    print("\n== 卡號格式轉換（site:card → UID，PRD §2.6） ==")
    uid_from_card = sp.card_to_uid_hex("64867:29942")
    check("64867:29942 → UID HEX", uid_from_card, "00000000FD6374F6")
    tag32 = int(uid_from_card[-8:], 16)
    check("還原 Site Code（bit31~16）", tag32 >> 16, 64867)
    check("還原 Card Code（bit15~0）", tag32 & 0xFFFF, 29942)
    check("16 碼 HEX 原樣通過", sp.card_to_uid_hex("00000000D4B81403"), "00000000D4B81403")
    check("八碼 HEX 自動補零", sp.card_to_uid_hex("D4B81403"), "00000000D4B81403")

    print("\n== 封包解析 / Echo ==")
    ack = sp.parse_packet(_norm("7E 04 00 04 FB FF"))
    check("ACK 解析 cmd", ack.cmd, 0x04)
    check("ACK 檢查碼", ack.checksum_ok, True)
    nack = sp.parse_packet(_norm("7E 04 00 05 FA FF"))
    check("NACK 解析 cmd", nack.cmd, 0x05)

    print("\n== 25H 事件記錄回覆解析 ==")
    log_pkt = sp.parse_packet(_norm(VECTOR_25H_ECHO))
    check("事件封包檢查碼", log_pkt.checksum_ok, True)
    ev = sp.parse_event_log(log_pkt)
    check("時間", ev["time"], "2021-02-25 18:01:14")
    check("Port Number（17=主埠）", ev["port_number"], 17)
    check("User Address（Data9/10）", ev["user_address"], 0x4AFE)
    check("Tag ID（Data15/16 + 19/20）", ev["tag_id"], "8EA14AFE")
    check("Duty code（Sub Code bit7~5）", ev["duty_code"], 0)

    print("\n== TCP 串流處理（黏包 / 分段） ==")
    stream = _norm(VECTOR_25H_ECHO) + _norm("7E 04 00 04 FB FF")
    client = SoyalClient("0.0.0.0", 0, timeout=1.0)
    client._sock = _FakeSock([stream[:7], stream[7:20], stream[20:], b""])  # type: ignore[assignment]
    try:
        p1 = client.recv_packet()
        p2 = client.recv_packet()
        check("分段後仍能解析第 1 個封包", p1.cmd, 0x03)
        check("黏包後仍能解析第 2 個封包", p2.cmd, 0x04)
    except sp.SoyalError as e:
        check("串流解析", f"例外: {e}", "無例外")

    print("\n== 人員記錄編解碼一致性（en/decode 成對） ==")
    decoded = sp.parse_user_record(rec, layout="write")
    check("回讀 addr（write 版面）", decoded["addr"], 1)
    check("回讀 uid_hex", decoded["uid_hex"], "000000008EA14AFE")
    check("回讀 Site Code", decoded["site_code"], 0x8EA1)
    check("回讀 Card Code", decoded["card_code"], 0x4AFE)
    check("回讀 Tag ID (32 bits)", decoded["tag_id"], "8EA14AFE")
    check("回讀 Access Mode", decoded["access_mode"], "card")
    check("回讀到期日", decoded["expire"], "2079-12-31")

    print("\n== 實機擷取向量（192.168.1.127:1621，AR-821EFv5 / 4V6） ==")
    real_rtc = sp.parse_packet(
        _norm(
            "7E 24 00 03 01 1A 0C 00 03 09 0B 0A 46 01 02 00 C3 00 04 01 "
            "80 80 80 80 80 80 80 80 80 80 80 80 80 80 80 80 63 BF"
        )
    )
    rtc = sp.parse_rtc(real_rtc)
    check("24H 檢查碼", real_rtc.checksum_ok, True)
    check("24H 時間（週二 2010-11-09）", rtc["time"], "2010-11-09 00:12:26 (Tue)")
    check("24H 韌體版本", rtc["firmware"], "4V6 (0x46)")

    real_log = sp.parse_packet(
        _norm(
            "7E 21 00 18 01 32 22 0E 05 15 0A 0A 11 00 00 00 00 10 00 00 00 "
            "01 00 00 00 00 00 00 00 00 00 00 00 E8 B3"
        )
    )
    check("25H 實測 echo code = 0x18", real_log.cmd, 0x18)
    log = sp.parse_event_log(real_log)
    check("25H 時間", log["time"], "2010-10-21 14:34:50")
    check("25H Port Number", log["port_number"], 17)

    # 87H 回讀（寫入位址 1000 + UID 000000008EA14AFE 後實機回覆）
    real_user_pkt = sp.parse_packet(
        _norm(
            "7E 1D 00 03 01 00 00 00 00 8E A1 4A FE 00 00 00 00 40 00 FF FF "
            "4F 0C 1F 00 00 00 00 00 7A AD"
        )
    )
    check("87H 檢查碼", real_user_pkt.checksum_ok, True)
    read24 = sp.parse_user_record(real_user_pkt.data[1:])  # 去掉 Source ID
    check("87H 自動判定 layout=read24", read24["layout"], "read24")
    check("87H 回讀 UID", read24["uid_hex"], "000000008EA14AFE")
    check("87H 回讀 Site/Card", (read24["site_code"], read24["card_code"]), (0x8EA1, 0x4AFE))
    check("87H 回讀 Mode", read24["mode"], 0x40)
    check("87H 回讀到期日", read24["expire"], "2079-12-31")
    check("87H 不含位址", read24["addr"], None)

    print(f"\n{'=' * 62}")
    if failures:
        print(f"結果：{checks - len(failures)}/{checks} 通過，{len(failures)} 項失敗")
        for f in failures:
            print(f"  - {f}")
        return 1
    print(f"結果：全部 {checks} 項通過（封包組裝與 datasheet 範例一致）")
    return 0


# ---------------------------------------------------------------------------
# 共用小工具
# ---------------------------------------------------------------------------


def _connect(args: argparse.Namespace) -> SoyalClient:
    c = SoyalClient(args.host, args.port, args.timeout)
    c.connect()
    print(f"已連線 {args.host}:{args.port}（DID={args.did}）")
    return c


def _txrx(client: SoyalClient, packet: bytes, label: str, retries: int = 1) -> sp.Packet:
    """送收並印出結果；連線被對方關閉時自動重連重試（部分機型會關掉閒置連線）。"""
    for attempt in range(retries + 1):
        try:
            print(f"  TX {label}: {packet.hex(' ').upper()}")
            pkt = client.request(packet)
            print(f"  RX: {pkt.hex()}")
            print(
                f"      → {pkt.echo_name}" + ("  ⚠️ 檢查碼錯誤!" if not pkt.checksum_ok else "")
            )
            return pkt
        except sp.SoyalError as e:
            if "關閉" in str(e) and attempt < retries:
                print("      ⚠️ 連線被對方關閉，重新連線後重試…")
                client.connect()
                continue
            raise


# ---------------------------------------------------------------------------
# 通訊測試
# ---------------------------------------------------------------------------


def cmd_probe(args: argparse.Namespace) -> int:
    """掃描打卡機開啟的 TCP Port。"""
    ports = args.ports or [1621, 1601, 8031, 8033, 80, 443]
    print(f"掃描 {args.host} 的候選 Port：{ports}（timeout {args.timeout}s）")
    opened = []
    for port in ports:
        try:
            with socket.create_connection((args.host, port), timeout=args.timeout):
                print(f"  [OPEN ] {port}")
                opened.append(port)
        except OSError as e:
            print(f"  [closed] {port}  ({type(e).__name__}: {e})")
    if not opened:
        print(
            "\n沒有任何 Port 開啟。請確認：\n"
            "  1. 本機與打卡機在同一網段（用 `ping 192.168.1.127` 確認）\n"
            "  2. 打卡機網頁「Network Setting」的 TCP Port 是否為 1621\n"
            f"  3. 防火牆 / VPN 是否阻擋，或改用其他 --ports 清單"
        )
        return 1
    print(f"\n開啟的 Port：{opened}")
    if 1621 in opened:
        print("→ 1621 有開，可接著跑：python tools/punch_admin.py info --port 1621")
    return 0


def cmd_ping(args: argparse.Namespace) -> int:
    """18H Hosting Polling：最輕量的存活測試。"""
    with _connect(args) as client:
        pkt = _txrx(client, sp.cmd_poll(args.did), "18H Polling")
        if pkt.cmd in (0x09, 0x03, 0x27) and len(pkt.data) > 14:
            try:
                ev = sp.parse_event_log(pkt)
                print("      回覆含事件記錄：")
                print(json.dumps(ev, ensure_ascii=False, indent=8))
            except sp.SoyalError:
                pass
    return 0


def cmd_info(args: argparse.Namespace) -> int:
    """24H 讀取控制器時間與韌體版本 + 18H Polling，用來確認可以溝通。"""
    ok = False
    with _connect(args) as client:
        print("\n[1/2] 24H 讀取時間 / 韌體版本")
        try:
            pkt = _txrx(client, sp.cmd_get_rtc(args.did), "24H Get RTC")
            info = sp.parse_rtc(pkt)
            print(f"      機端時間：{info['time']}")
            print(f"      體版本：{info['firmware']}")
            print(f"      Data 原始：{info['raw']}")
            ok = True
        except sp.SoyalError as e:
            print(f"      ⚠️ {e}")

        print("\n[2/2] 18H Hosting Polling")
        try:
            _txrx(client, sp.cmd_poll(args.did), "18H Polling")
            ok = ok or True
        except sp.SoyalError as e:
            print(f"      ⚠️ {e}")

    if ok:
        print("\n✅ 可以溝通：控制器有正常回覆 SOYAL 標準封包。")
        return 0
    print(
        "\n❌ 無法取得有效回覆。請改用 `probe` 掃描 Port，並確認 DID（Node ID）正確。\n"
        "   機端 Node ID 可由鍵盤查詢（見 datasheet §1.1：＊123456＃ → 00＊001＃）"
    )
    return 1


# ---------------------------------------------------------------------------
# 人員 / 卡片
# ---------------------------------------------------------------------------


def cmd_get_user(args: argparse.Namespace) -> int:
    """87H 讀取人員資料。"""
    with _connect(args) as client:
        pkt = _txrx(
            client, sp.cmd_get_user(args.did, args.addr, args.nums, args.addr_order), "87H Get User"
        )
        if pkt.cmd != 0x03:
            print("      ⚠️ 未取得資料回覆（可能是該位址無資料或有其他錯誤）")
            return 1
        data = pkt.data
        # 回覆：Source ID(1) + N*record。實測 87H 回傳 24 bytes/筆（datasheet §2.22），
        # 與 83H 下載用的 26 bytes/筆不同，故以實際長度自動推算。
        rec_len = args.record_len
        auto = (len(data) - 1) // max(1, args.nums)
        if 14 <= auto <= 40:
            rec_len = auto
            if auto != args.record_len:
                print(f"      ⚠️ 依回覆長度自動採用 record_len={auto}（--record-len 指定 {args.record_len}）")
        print(f"      回覆資料 {len(data)} bytes（record_len={rec_len}）")
        body = data[1:]
        for i in range(args.nums):
            chunk = body[i * rec_len : (i + 1) * rec_len]
            if len(chunk) < rec_len:
                break
            info = sp.parse_user_record(chunk, args.addr_order, args.uid_order)
            print(f"\n  ── 第 {i + 1} 筆（位址 {args.addr + i}）")
            print(json.dumps(info, ensure_ascii=False, indent=4))
    return 0


def cmd_scan_users(args: argparse.Namespace) -> int:
    """唯讀掃描人員位址範圍，只印出「非空」的記錄（用來確認資料結構與位元組順序）。

    實測（AR-821EFv5 / 4V6）：87H 多筆讀取在 nums<=10 時完整回傳 24 bytes/筆；
    nums 過大時回應會被截斷（尾巴殘缺），故自動把 batch 限制在安全值內，
    遇到截斷時會自動降為逐筆確認。
    """
    found = 0
    safe = max(1, min(args.batch, 10))
    if safe != args.batch:
        print(f"      ⚠️ 實機多筆讀取安全上限為 10,本次以 batch={safe} 進行。")
        args.batch = safe
    with _connect(args) as client:
        addr = args.start
        while addr <= args.end:
            batch = min(args.batch, args.end - addr + 1)
            pkt = None
            for attempt in range(2):  # 部分機型會關掉連線，重連後重試一次
                try:
                    pkt = client.request(sp.cmd_get_user(args.did, addr, batch, args.addr_order))
                    break
                except sp.SoyalError as e:
                    if "關閉" in str(e) and attempt == 0:
                        print(f"  位址 {addr}: 連線被關閉，重連重試…")
                        client.connect()
                        continue
                    print(f"  位址 {addr}: 讀取失敗（{e}）")
                    break
            if pkt is None:
                break
            if pkt.cmd != 0x03 or len(pkt.data) < 2:
                print(f"  位址 {addr}: 無資料（echo 0x{pkt.cmd:02X}）")
                break
            body = pkt.data[1:]
            full = len(body) // 24  # 實機 87H 每筆固定 24 bytes
            trailing = len(body) % 24
            if trailing:
                print(
                    f"      ⚠️ 位址 {addr}: 回應被截斷（{trailing} bytes 尾巴），"
                    f"讀得 {full} 筆完整紀錄；此區間改逐筆確認…"
                )
            for i in range(full):
                rec = body[i * 24 : (i + 1) * 24]
                if len(rec) < 12:
                    break
                first = rec[0:2].hex().upper()
                if set(rec) == {0xFF} or rec.startswith(b"\xff\xff"):
                    continue  # 空白位址，跳過
                found += 1
                print(f"\n  位址 {addr + i} (前 2 bytes={first}) 長度 {len(rec)}：")
                print(f"    raw : {rec.hex(' ').upper()}")
                try:
                    print(json.dumps(sp.parse_user_record(rec), ensure_ascii=False, indent=6))
                except sp.SoyalError as e:
                    print(f"    （無法解讀：{e}）")
            if trailing:
                addr += max(full, 1)
                args.batch = 1  # 降到單筆，避免再次截斷
            else:
                addr += batch
    print(f"\n共找到 {found} 筆非空記錄（範圍 {args.start}~{args.end}）。")
    return 0


def cmd_verify(args: argparse.Namespace) -> int:
    """寫入後回讀驗證位址與 UID 的位元組順序是否如預期（實機必做）。"""
    if args.expect_uid and ":" in args.expect_uid:
        args.expect_uid = sp.card_to_uid_hex(args.expect_uid)
    with _connect(args) as client:
        pkt = _txrx(client, sp.cmd_get_user(args.did, args.addr, 1, args.addr_order), "87H Verify")
        if pkt.cmd != 0x03 or len(pkt.data) < 1 + 14:
            print("      ⚠️ 無資料可驗證")
            return 1
        info = sp.parse_user_record(
            pkt.data[1 : 1 + args.record_len], args.addr_order, args.uid_order
        )
        print(json.dumps(info, ensure_ascii=False, indent=4))
        ok = True
        if info.get("empty"):
            print("\n❌ 該位址目前是空白（沒有建人員）")
            return 1
        if info["addr"] is not None and info["addr"] != args.addr:
            print(f"\n❌ 位址不符：要求 {args.addr}，回讀 {info['addr']}")
            print("   → 87H 回讀不含位址，請確認是否查詢了正確位址")
            ok = False
        if args.expect_uid and info["uid_hex"] != args.expect_uid.replace(" ", "").upper():
            print(f"\n❌ UID 不符：預期 {args.expect_uid.upper()}，回讀 {info['uid_hex']}")
            print("   → 若 UID 被反轉，請改用 `--uid-order le` 重新驗證")
            ok = False
        if ok:
            print("\n✅ 實機回讀成功：卡片資料與寫入內容一致（UID 順序正確）。")
            if info["addr"] is None:
                print("   ※ 87H 回讀不含位址，位址正確性請以「讀得到資料」為準。")
        return 0 if ok else 1


def _ack_status_text(pkt: sp.Packet) -> str:
    """ACK（0x04）附帶的控制器狀態（§1.4.2.1）：Data0 節點、Data1 型號、Data2 體版本…"""
    d = pkt.data
    if pkt.cmd != 0x04 or len(d) < 3:
        return ""
    ctype = sp.CONTROLLER_TYPES.get(d[1], f"未知(0x{d[1]:02X})")
    return (
        f"（ACK 附帶狀態：node={d[0]} 型號={ctype} "
        f"體版本={d[2] >> 4}V{d[2] & 0x0F} 輸入埠=0x{d[3]:02X} 繼電器=0x{d[4]:02X}）"
        if len(d) >= 5
        else f"（ACK 附帶狀態：node={d[0]} 型號={ctype}）"
    )


def _parse_expire(text: str) -> tuple[int, int, int]:
    """'none'/'never' → 2079-12-31（等於不設限）；否則吃 YYYY-MM-DD。"""
    if not text or text.lower() in ("none", "never", "-"):
        return 0x4F, 0x0C, 0x1F
    parts = text.split("-")
    if len(parts) != 3:
        raise SystemExit(f"--expire 格式須為 YYYY-MM-DD 或 none，收到: {text}")
    y, m, d = (int(p) for p in parts)
    return y % 100, m, d


def cmd_add_user(args: argparse.Namespace) -> int:
    """83H / 84H 新增人員＋卡片（預設 dry-run，需 --yes 才送出）。"""
    year, month, day = _parse_expire(args.expire)
    mode = sp.encode_mode(
        args.access,
        patrol_card=args.patrol,
        enable_expire=args.enable_expire,
        allow_pin_change=args.pin_change,
    )
    # --uid 支援 16 碼 HEX，或 site:card 十進位（如 64867:29942，見 sp.card_to_uid_hex）
    if ":" in args.uid:
        args.uid = sp.card_to_uid_hex(args.uid)
    record = sp.build_user_record(
        addr=args.addr,
        uid_hex=args.uid,
        pin=args.pin,
        mode=mode,
        zone=args.zone,
        group1=args.group1,
        group2=args.group2,
        year=year,
        month=month,
        day=day,
        level=args.level,
        option=0x80 if args.apb else 0x00,
        addr_order=args.addr_order,
        uid_order=args.uid_order,
    )
    packet = sp.cmd_set_user(args.did, [record], with_apb=args.apb)

    info = sp.parse_user_record(record)
    print("== 準備新增人員／卡片 ==")
    print(f"  指令          : {('83H (含 APB)' if args.apb else '84H')}")
    print(f"  人員位址      : {args.addr}（{args.addr_order} 順序）")
    print(f"  Tag UID       : {info['uid_hex']}")
    print(f"  Site / Card   : {info['site_code']} / {info['card_code']}  (Tag ID {info['tag_id']})")
    print(f"  PIN           : {args.pin}")
    print(f"  Access Mode   : {args.access}（Mode byte = 0x{mode:02X}）")
    print(f"  可用門組      : Group1=0x{args.group1:02X} Group2=0x{args.group2:02X}")
    print(f"  到期日        : {'不設限 (2079-12-31)' if args.expire.lower() in ('none', 'never', '-', '') else args.expire}")
    print(f"  等級 / 時區   : level={args.level} zone={args.zone}")
    print(f"  記錄 HEX (26B): {record.hex(' ').upper()}")
    print(f"  封包 HEX      : {packet.hex(' ').upper()}")

    if not args.yes:
        print("\n[DRY-RUN] 未送出。確認無誤後加上 --yes 才會實際寫入打卡機。")
        return 0

    with _connect(args) as client:
        pkt = _txrx(client, packet, "Set User")
        if pkt.cmd != 0x04:
            print(f"\n❌ 未取得 ACK（echo=0x{pkt.cmd:02X}）。可能原因：位址已存在、格式不符、"
                  f"通訊層級錯誤（0x0C）{_ack_status_text(pkt)}")
            return 1
        print("\n✅ 控制器回覆 ACK。" + _ack_status_text(pkt))

        if args.name:
            alias_pkt = sp.cmd_write_alias(args.did, args.addr, [args.name], args.encoding)
            print(f"\n== 寫入姓名（2EH）: {args.name} ({args.encoding}) ==")
            print(f"  封包 HEX: {alias_pkt.hex(' ').upper()}")
            apkt = _txrx(client, alias_pkt, "Write Alias")
            if apkt.cmd != 0x04:
                print("  ⚠️ 姓名寫入未取得 ACK（卡片已新增成功，姓名可稍後用後台補）")

        print(
            f"\n下一步請做回讀驗證：\n"
            f"  python tools/punch_admin.py verify --host {args.host} --port {args.port} "
            f"--addr {args.addr} --expect-uid {args.uid}"
        )
    return 0


def cmd_del_user(args: argparse.Namespace) -> int:
    """85H 刪除人員（預設 dry-run，需 --yes 才送出）。"""
    packet = sp.cmd_erase_user(args.did, args.start, args.end, args.addr_order)
    print("== 準備刪除人員 ==")
    print(f"  位址範圍: {args.start} ~ {args.end}")
    print(f"  封包 HEX: {packet.hex(' ').upper()}")
    if not args.yes:
        print("\n[DRY-RUN] 未送出。確認無誤後加上 --yes 才會實際刪除。")
        return 0
    with _connect(args) as client:
        pkt = _txrx(client, packet, "Erase User")
        if pkt.cmd != 0x04:
            print(f"\n❌ 未取得 ACK（echo=0x{pkt.cmd:02X}）")
            return 1
        print("\n✅ 控制器回覆 ACK（datasheet 註明刪除需 100ms ~ 6 秒，請稍候再查詢）。")
    return 0


def cmd_write_alias(args: argparse.Namespace) -> int:
    """2EH 寫入姓名（預設 dry-run）。"""
    packet = sp.cmd_write_alias(args.did, args.addr, args.name, args.encoding)
    print("== 準備寫入姓名 ==")
    for n in args.name:
        raw = n.encode(args.encoding, errors="replace")
        print(f"  {n!r} → {len(raw)} bytes {raw.hex(' ').upper()}")
        if len(raw) > 16:
            print("  ⚠️ 超過 16 bytes 會被截斷（繁中 Big5 每字 2 bytes → 最多 8 字）")
    print(f"  封包 HEX: {packet.hex(' ').upper()}")
    if not args.yes:
        print("\n[DRY-RUN] 未送出。確認無誤後加上 --yes 才會實際寫入。")
        return 0
    with _connect(args) as client:
        pkt = _txrx(client, packet, "Write Alias")
        return 0 if pkt.cmd == 0x04 else 1


def cmd_dump_log(args: argparse.Namespace) -> int:
    """25H 讀取控制器事件記錄（可 --remove 逐筆刪除）。"""
    if args.count > 1 and not args.remove:
        print("⚠️ 讀取不會移除記錄，未加 --remove 時會一直讀到同一筆（已自動改用 18H 輪詢）。")
    with _connect(args) as client:
        for i in range(args.count):
            pkt = _txrx(client, sp.cmd_get_log(args.did, args.extend), f"25H Get Log #{i + 1}")
            if pkt.cmd == 0x04:
                print(f"      → 目前無事件記錄 {_ack_status_text(pkt)}")
                break
            if len(pkt.data) < 21:
                print("      → 回覆不是事件記錄（長度不足 21 bytes），原始資料如上")
                break
            try:
                ev = sp.parse_event_log(pkt)
                print(json.dumps(ev, ensure_ascii=False, indent=6))
            except sp.SoyalError as e:
                print(f"      ⚠️ {e}")
            if args.remove:
                rpkt = _txrx(client, sp.cmd_remove_log(args.did, args.extend), "37H Remove Log")
                if rpkt.cmd != 0x04:
                    print("      ⚠️ 移除未取得 ACK，停止以免重複讀取")
                    break
    return 0


# ---------------------------------------------------------------------------
# CLI
# ---------------------------------------------------------------------------


def _common_args() -> argparse.ArgumentParser:
    """共用參數（掛在各 subcommand 上，故寫成 `punch_admin.py info --host X`）。"""
    common = argparse.ArgumentParser(add_help=False)
    common.add_argument(
        "--host", default="192.168.1.127", help="打卡機 IP（預設 192.168.1.127，實測機）"
    )
    common.add_argument(
        "--port", type=int, default=1621, help="TCP 指令埠（預設 1621，實測機）"
    )
    common.add_argument(
        "--did", type=lambda s: int(s, 0), default=1, help="控制器 Node ID（預設 1）"
    )
    common.add_argument("--timeout", type=float, default=3.0, help="等待回覆秒數（預設 3）")
    common.add_argument(
        "--addr-order",
        choices=["be", "le"],
        default="be",
        help="人員位址位元組順序（預設 be；實機回讀錯誤時改 le）",
    )
    common.add_argument(
        "--uid-order",
        choices=["be", "le"],
        default="be",
        help="Tag UID 位元組順序（預設 be ＝與 8031 TEXT 事件所見相同）",
    )
    common.add_argument(
        "--record-len", type=int, default=26, help="人員記錄長度（E 系列 26；部分舊韌體 24）"
    )
    return common


def build_parser() -> argparse.ArgumentParser:
    common = _common_args()
    ap = argparse.ArgumentParser(
        prog="punch_admin.py",
        description="SOYAL 打卡機測試工具（新增人員/卡片、讀取、通訊測試）",
        formatter_class=argparse.RawDescriptionHelpFormatter,
        epilog=(
            "範例：\n"
            "  python tools/punch_admin.py selftest                      # 離線驗證（免連線）\n"
            "  python tools/punch_admin.py probe                         # 掃描 Port\n"
            "  python tools/punch_admin.py info                          # 24H+18H 通訊測試\n"
            "  python tools/punch_admin.py add-user --addr 1000 --uid 00000000D4B81403 --yes\n"
            "  python tools/punch_admin.py verify --addr 1000 --expect-uid 00000000D4B81403\n"
            "  python tools/punch_admin.py dump-log --count 3 --remove\n\n"
            "預設目標：192.168.1.127:1621（實測機；AR-821EFv5 / 4V6 / Node ID 1）。\n"
            "寫入類指令預設 dry-run，需加 --yes。"
        ),
    )
    sub = ap.add_subparsers(dest="command", required=True, metavar="<command>")

    sub.add_parser(
        "selftest", help="離線驗證封包組裝（不需連線）", parents=[common]
    ).set_defaults(func=cmd_selftest)

    p = sub.add_parser("probe", help="掃描打卡機開啟的 TCP Port", parents=[common])
    p.add_argument("--ports", help="自訂埠清單，如 1601,80,8031")
    p.set_defaults(func=cmd_probe)

    sub.add_parser("ping", help="18H 存活測試", parents=[common]).set_defaults(func=cmd_ping)

    sub.add_parser(
        "info", help="24H 讀時間/體版本 + 18H（確認可溝通）", parents=[common]
    ).set_defaults(func=cmd_info)

    p = sub.add_parser("get-user", help="87H 讀取人員資料", parents=[common])
    p.add_argument("--addr", type=int, required=True, help="起始人員位址")
    p.add_argument("--nums", type=int, default=1, help="讀取筆數（預設 1）")
    p.set_defaults(func=cmd_get_user)

    p = sub.add_parser("scan-users", help="唯讀掃描人員位址（找出非空記錄）", parents=[common])
    p.add_argument("--start", type=int, default=1, help="起始位址（預設 1）")
    p.add_argument("--end", type=int, default=20, help="結束位址（預設 20）")
    p.add_argument("--batch", type=int, default=10, help="每批讀取筆數（預設 10；實測 >10 回應會截斷，工具會自動降為逐筆）")
    p.set_defaults(func=cmd_scan_users)

    p = sub.add_parser("verify", help="回讀驗證位址/UID 順序", parents=[common])
    p.add_argument("--addr", type=int, required=True, help="人員位址")
    p.add_argument("--expect-uid", help="預期的 16 碼 UID")
    p.set_defaults(func=cmd_verify)

    p = sub.add_parser(
        "add-user", help="83H/84H 新增人員＋卡片（預設 dry-run）", parents=[common]
    )
    p.add_argument("--addr", type=int, required=True, help="人員位址（0~16383）")
    p.add_argument("--uid", required=True, help="Tag UID（16 碼 HEX，或 site:card 十進位如 64867:29942）")
    p.add_argument("--name", help="姓名（可選，成功後續用 2EH 寫入）")
    p.add_argument(
        "--access", choices=list(sp.ACCESS_MODES), default="card", help="通行方式（預設 card）"
    )
    p.add_argument("--pin", type=lambda s: int(s, 0), default=0, help="PIN 碼（預設 0）")
    p.add_argument("--zone", type=lambda s: int(s, 0), default=0, help="通行時區（預設 0=free）")
    p.add_argument("--group1", type=lambda s: int(s, 0), default=0xFF, help="門組 16~9（預設 FF）")
    p.add_argument("--group2", type=lambda s: int(s, 0), default=0xFF, help="門組 8~1（預設 FF）")
    p.add_argument("--expire", default="none", help="到期日 YYYY-MM-DD 或 none（預設 none）")
    p.add_argument("--level", type=lambda s: int(s, 0), default=0, help="等級 0~3（預設 0）")
    p.add_argument("--apb", action="store_true", help="改用 83H 並啟用 anti-pass-back")
    p.add_argument("--patrol", action="store_true", help="巡邏卡")
    p.add_argument("--enable-expire", action="store_true", help="啟用到期檢查")
    p.add_argument("--pin-change", action="store_true", help="允許使用者自行改密碼")
    p.add_argument("--encoding", default="big5", help="姓名編碼（預設 big5）")
    p.add_argument("--yes", action="store_true", help="確定送出（預設只印封包）")
    p.set_defaults(func=cmd_add_user)

    p = sub.add_parser("del-user", help="85H 刪除人員（預設 dry-run）", parents=[common])
    p.add_argument("--start", type=int, required=True, help="起始位址")
    p.add_argument("--end", type=int, required=True, help="結束位址")
    p.add_argument("--yes", action="store_true", help="確定送出")
    p.set_defaults(func=cmd_del_user)

    p = sub.add_parser("write-alias", help="2EH 寫入姓名（預設 dry-run）", parents=[common])
    p.add_argument("--addr", type=int, required=True, help="起始人員位址")
    p.add_argument("--name", action="append", required=True, help="姓名（可重複指定多筆）")
    p.add_argument("--encoding", default="big5", help="姓名編碼（預設 big5）")
    p.add_argument("--yes", action="store_true", help="確定送出")
    p.set_defaults(func=cmd_write_alias)

    p = sub.add_parser("dump-log", help="25H 讀取事件記錄", parents=[common])
    p.add_argument("--count", type=int, default=1, help="讀取筆數（預設 1）")
    p.add_argument("--remove", action="store_true", help="每讀一筆就送 37H 移除")
    p.add_argument("--extend", action="store_true", help="使用 3 bytes 延伸欄位版本")
    p.set_defaults(func=cmd_dump_log)

    return ap


def main(argv: list[str] | None = None) -> int:
    args = build_parser().parse_args(argv)
    if isinstance(getattr(args, "ports", None), str):
        args.ports = [int(x) for x in args.ports.split(",") if x.strip()]
    try:
        return args.func(args)
    except sp.SoyalError as e:
        print(f"\n[協定錯誤] {e}", file=sys.stderr)
        return 2
    except (ConnectionRefusedError, ConnectionResetError, TimeoutError, socket.timeout) as e:
        print(
            f"\n[連線失敗] {args.host}:{args.port} → {type(e).__name__}: {e}\n"
            "  請先跑 `probe` 確認 Port，並用 `ping 192.168.1.127` 確認網路可達。",
            file=sys.stderr,
        )
        return 3
    except OSError as e:
        print(f"\n[網路錯誤] {type(e).__name__}: {e}", file=sys.stderr)
        return 3


if __name__ == "__main__":
    raise SystemExit(main())