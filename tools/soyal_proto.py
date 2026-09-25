"""SOYAL 標準通訊協定（Standard mode）最小實作 —— 無外部相依，只用標準庫。

實作依據：`硬體Protocol及範例/Protocol_881E_725Ev2_82xEv5 4V05.pdf`
  - §1.3.1 Standard Short Data Package ：Head 0x7E
  - §1.3.2 Standard Large Data Package ：Head FF 00 5A A5（TCP/IP 資料可到 1400 bytes）
  - §1.4   Command Echo Format        ：ACK 04h / NACK 05h / 資料回覆 03h
  - §2.5   Hosting Polling (18H)      ：ping 用
  - §2.7   Get real time clock (24H)  ：讀時間＋體版本
  - §2.8   Get oldest event log (25H) ：讀事件記錄（含 User Address）
  - §2.19  Set User Parameters (83H/84H)：新增人員／卡片（E 系列每筆 26 bytes）
  - §2.20  Erase user data (85H)
  - §2.22  Get User Parameters (87H)
  - §2.16  Read/Write User Alias (2EH)：寫入姓名

封包格式（短封包，Standard Short）
    Head(0x7E) | Length | DID | CMD | Data... | XOR | SUM
    Length = 「DID 到 SUM」的位元組數（含 XOR 與 SUM）
    XOR    = 0xFF ^ DID ^ CMD ^ Data...（逐 byte XOR）
    SUM    = (DID + CMD + Data... + XOR) & 0xFF

※ 注意：SOYAL 官方文件在不同章節對「位元組順序」的描述彼此不一致（例如 E 系列 83H 寫
   「User Address / Tag UID little endian」，但文件內的實際封包範例是高位在前）。因此本模組
   把順序做成可選參數（`addr_order` / `uid_order`），預設採「與文件範例一致」＝高位在前，
   並提供 87H 回讀驗證；實機佈署前務必用 `punch_admin.py verify` 確認。
"""

from __future__ import annotations

import socket
import struct
import time
from dataclasses import dataclass

# ---------------------------------------------------------------------------
# 常數
# ---------------------------------------------------------------------------

HEAD_SHORT = 0x7E
HEAD_LARGE = b"\xff\x00\x5a\xa5"

#: Echo code（§1.4.1）—— 實測補充：本機 AR-837EF 韌體 4V6 對 25H 是以 0x18 回覆資料
ECHO_CODES: dict[int, str] = {
    0x03: "資料回覆 (Response to request)",
    0x04: "ACK 指令成功",
    0x05: "NACK 指令失敗",
    0x06: "AUTHERR 認證失敗",
    0x07: "NOTAG 無卡片",
    0x08: "NOT LOGIN 使用者未登入",
    0x09: "讀卡機狀態回覆 (reader status)",
    0x0A: "卡片資料區未認證",
    0x0B: "卡片資料區認證錯誤",
    0x0C: "拒絕指令（通訊層級錯誤）",
    0x0D: "TCP Linker 操作逾時",
    0x0E: "TCP Keep Alive",
    0x18: "資料回覆（實測 25H 以此碼回傳事件記錄）",
    0x24: "資料回覆（實測 24H 以此碼回傳時間/版本）",
    0x27: "事件記錄 (event log)",
}

#: 控制器型號（§1.4.2.1 Appended Data 0）
CONTROLLER_TYPES: dict[int, str] = {
    0xC0: "AR-881E",
    0xC1: "AR-725Ev2",
    0xC2: "AR-829Ev5",
    0xC3: "AR-821EFv5",
    0xC4: "AR-727Ev5",
    0xC5: "AR-721Ev2",
}

#: Access Mode（§2.19 Mode byte bit7/6）
ACCESS_MODES: dict[str, int] = {
    "invalid": 0b00,
    "card": 0b01,          # Card Only
    "card-or-pin": 0b10,   # Card or PIN
    "card+pin": 0b11,      # Card + PIN
}

#: Duty code（Message File structure.pdf Note 2；Sub Code bit7~5）
DUTY_LABELS = [
    "On duty 上班",
    "Off duty 下班",
    "Overtime in 加班進",
    "Overtime out 加班出",
    "Break out 外出",
    "Break in 返回",
    "Go out 外出",
    "Return 返回",
]


class SoyalError(Exception):
    """協定或通訊錯誤。"""


# ---------------------------------------------------------------------------
# 封包組裝 / 檢查碼
# ---------------------------------------------------------------------------


def checksum(payload: bytes) -> tuple[int, int]:
    """計算 payload（DID..Data）的 (XOR, SUM)。"""
    xor = 0xFF
    for b in payload:
        xor ^= b
    total = (sum(payload) + xor) & 0xFF
    return xor, total


def build_short(did: int, cmd: int, data: bytes = b"") -> bytes:
    """組出 Standard Short 封包（Head 0x7E）。

    >>> build_short(0x01, 0x18).hex(" ").upper()
    '7E 04 01 18 E6 FF'
    """
    if not 0 <= did <= 0xFF:
        raise SoyalError(f"DID 超出範圍: {did}")
    if not 0 <= cmd <= 0xFF:
        raise SoyalError(f"CMD 超出範圍: {cmd}")
    body = bytes([did, cmd]) + bytes(data)
    length = len(body) + 2  # + XOR + SUM
    if length > 250:
        raise SoyalError(f"短封包長度 {length} 超過上限 250，請改用長封包")
    xor, total = checksum(body)
    return bytes([HEAD_SHORT, length]) + body + bytes([xor, total])


def build_large(did: int, cmd: int, data: bytes = b"", area: int = 0) -> bytes:
    """組出 Standard Large 封包（Head FF 00 5A A5，TCP/IP 用）。

    Length 為 2 bytes 高位在前，且高 nibble 為控制器 Area Code。
    資料長度 TCP/IP 上限 1400 bytes（§1.3.2）。
    """
    body = bytes([did, cmd]) + bytes(data)
    if len(body) > 1400:
        raise SoyalError(f"長封包資料 {len(body)} 超過 TCP/IP 上限 1400")
    length = len(body) + 2
    length_field = ((area & 0x0F) << 12) | (length & 0x0FFF)
    xor, total = checksum(body)
    return HEAD_LARGE + struct.pack(">H", length_field) + body + bytes([xor, total])


def verify_checksum(raw: bytes) -> bool:
    """驗證一個完整短封包的 XOR / SUM。"""
    if len(raw) < 5 or raw[0] != HEAD_SHORT:
        return False
    body = raw[2:-2]
    xor, total = checksum(body)
    return raw[-2] == xor and raw[-1] == total


# ---------------------------------------------------------------------------
# 封包解析
# ---------------------------------------------------------------------------


@dataclass
class Packet:
    """一個解析後的封包。"""

    raw: bytes
    kind: str          # 'short' | 'large'
    did: int           # Destination Node ID（回覆時固定 00 = 回主機）
    cmd: int           # 指令碼 / echo code
    data: bytes
    checksum_ok: bool

    @property
    def echo_name(self) -> str:
        return ECHO_CODES.get(self.cmd, f"未知 (0x{self.cmd:02X})")

    def hex(self) -> str:
        return self.raw.hex(" ").upper()

    def __str__(self) -> str:  # pragma: no cover - 便於互動除錯
        return (
            f"<Packet {self.kind} DID=0x{self.did:02X} CMD=0x{self.cmd:02X} "
            f"({self.echo_name}) len={len(self.raw)} "
            f"ck={'OK' if self.checksum_ok else 'BAD'}>"
        )


def parse_packet(raw: bytes) -> Packet:
    """解析單一封包（短或長）。"""
    if not raw:
        raise SoyalError("空封包")
    if raw[0] == HEAD_SHORT:
        if len(raw) < 5:
            raise SoyalError(f"短封包長度不足: {raw.hex(' ')}")
        length = raw[1]
        # Length = 「DID 到 SUM」的位元組數，故總長 = Head(1) + Length(1) + Length
        if len(raw) != length + 2:
            raise SoyalError(
                f"短封包長度不符: Length 欄位={length}，實際收到 {len(raw) - 2} bytes"
            )
        return Packet(raw, "short", raw[2], raw[3], raw[4:-2], verify_checksum(raw))
    if raw.startswith(HEAD_LARGE):
        if len(raw) < 9:
            raise SoyalError(f"長封包長度不足: {raw.hex(' ')}")
        length_field = struct.unpack(">H", raw[4:6])[0]
        length = length_field & 0x0FFF
        if len(raw) != 6 + length:
            raise SoyalError(
                f"長封包長度不符: Length 欄位={length}，實際收到 {len(raw) - 6} bytes"
            )
        body = raw[6:-2]
        xor, total = checksum(body)
        return Packet(
            raw, "large", raw[6], raw[7], raw[8:-2], raw[-2] == xor and raw[-1] == total
        )
    raise SoyalError(f"未知的封包開頭: {raw[:6].hex(' ')}")


# ---------------------------------------------------------------------------
# TCP 傳輸
# ---------------------------------------------------------------------------


class SoyalClient:
    """對 SOYAL 控制器（TCP/IP）送收指令的簡易 client。

    控制器網頁「Network Setting」內的 TCP Port 通常為 1601；
    實際埠號請以該頁設定為準（可用 `punch_admin.py probe` 掃描候選埠）。
    """

    def __init__(self, host: str, port: int = 1621, timeout: float = 3.0):
        self.host = host
        self.port = port
        self.timeout = timeout
        self._sock: socket.socket | None = None
        self._buf = b""

    # -- 連線管理 ----------------------------------------------------------
    def connect(self) -> None:
        self._sock = socket.create_connection((self.host, self.port), timeout=self.timeout)
        self._sock.settimeout(self.timeout)
        self._buf = b""

    def close(self) -> None:
        if self._sock:
            try:
                self._sock.close()
            finally:
                self._sock = None

    def __enter__(self) -> "SoyalClient":
        if self._sock is None:  # 已連線時不重複建立（避免 socket 洩漏）
            self.connect()
        return self

    def __exit__(self, *exc) -> None:
        self.close()

    # -- 送收 --------------------------------------------------------------
    def send(self, packet: bytes) -> None:
        if not self._sock:
            raise SoyalError("尚未連線，請先呼叫 connect()")
        self._sock.sendall(packet)

    def recv_packet(self) -> Packet:
        """讀出一個完整封包（會處理 TCP 分段與黏包）。"""
        if not self._sock:
            raise SoyalError("尚未連線，請先呼叫 connect()")
        deadline = time.monotonic() + self.timeout
        while True:
            pkt = self._try_extract()
            if pkt is not None:
                return pkt
            if time.monotonic() > deadline:
                raise SoyalError(
                    f"等待回覆逾時（{self.timeout}s），已收到 {len(self._buf)} bytes: "
                    f"{self._buf[:32].hex(' ')}"
                )
            try:
                chunk = self._sock.recv(4096)
            except socket.timeout:
                continue
            if not chunk:
                raise SoyalError("連線被對方關閉")
            self._buf += chunk

    def request(self, packet: bytes) -> Packet:
        """送出並等待一個回覆封包。"""
        self.send(packet)
        return self.recv_packet()

    def _try_extract(self) -> Packet | None:
        buf = self._buf
        if len(buf) < 5:
            return None
        if buf[0] == HEAD_SHORT:
            total = buf[1] + 2
            if len(buf) < total:
                return None
            raw, self._buf = buf[:total], buf[total:]
            return parse_packet(raw)
        if buf.startswith(HEAD_LARGE):
            if len(buf) < 6:
                return None
            length = struct.unpack(">H", buf[4:6])[0] & 0x0FFF
            total = 6 + length
            if len(buf) < total:
                return None
            raw, self._buf = buf[:total], buf[total:]
            return parse_packet(raw)
        # 丟掉開頭的雜訊，直到看到合法標頭
        cands = [i for i in (buf.find(bytes([HEAD_SHORT])), buf.find(HEAD_LARGE)) if i >= 0]
        if not cands:
            self._buf = b""
            return None
        self._buf = buf[min(cands):]
        return None


# ---------------------------------------------------------------------------
# 指令組裝（Request packets）
# ---------------------------------------------------------------------------


def cmd_poll(did: int = 1) -> bytes:
    """18H Hosting Polling（ping）。文件範例：7E 04 01 18 E6 FF"""
    return build_short(did, 0x18)


def cmd_get_rtc(did: int = 1) -> bytes:
    """24H 讀取控制器時間與韌體版本。"""
    return build_short(did, 0x24)


def cmd_get_log(did: int = 1, extend: bool = False) -> bytes:
    """25H 讀取最舊的一筆事件記錄。

    extend=False 時與文件範例相同（7E 04 01 25 DB 01）。
    """
    if extend:
        return build_short(did, 0x25, b"\xff\xff\xff")
    return build_short(did, 0x25)


def cmd_remove_log(did: int = 1, extend: bool = True) -> bytes:
    """37H 移除最舊的一筆事件記錄（讀取後需移除，否則會一直讀到同一筆）。"""
    if extend:
        return build_short(did, 0x37, b"\x44\x45\x4c")
    return build_short(did, 0x37)


def cmd_get_user(did: int, addr: int, nums: int = 1, addr_order: str = "be") -> bytes:
    """87H 讀取人員資料。"""
    return build_short(did, 0x87, encode_addr(addr, addr_order, 2) + bytes([nums & 0xFF]))


def cmd_erase_user(did: int, start: int, end: int, addr_order: str = "be") -> bytes:
    """85H 刪除人員（含起始與結束位址）。"""
    return build_short(
        did, 0x85, encode_addr(start, addr_order, 2) + encode_addr(end, addr_order, 2)
    )


def cmd_set_user(
    did: int,
    records: list[bytes],
    with_apb: bool = False,
    records_field_bytes: int = 1,
) -> bytes:
    """83H / 84H 下載人員資料（新增人員＋卡片）。

    with_apb=True  → 83H（含 anti-passback 旗標，Option byte 有效）
    with_apb=False → 84H（Option byte 會被控制器忽略）
    """
    cmd = 0x83 if with_apb else 0x84
    if not records:
        raise SoyalError("至少需要一筆人員資料")
    head = (len(records) & 0xFFFF).to_bytes(records_field_bytes, "big")
    return build_short(did, cmd, head + b"".join(records))


def cmd_write_alias(did: int, addr: int, names: list[str], encoding: str = "big5") -> bytes:
    """2EH 寫入人員姓名（每筆固定 16 bytes）。

    姓名長度依機端韌體編碼而定；繁中通常為 Big5（每字 2 bytes → 最多 8 字）。
    """
    if not names:
        raise SoyalError("至少需要一個姓名")
    index = encode_addr(addr, "be", 3)
    payload = index + bytes([len(names) & 0xFF])
    for name in names:
        raw = name.encode(encoding, errors="replace")[:16]
        payload += raw.ljust(16, b"\x00")
    return build_short(did, 0x2E, payload)


# ---------------------------------------------------------------------------
# 位址 / 卡片 / 人員記錄編解碼
# ---------------------------------------------------------------------------


def encode_addr(addr: int, order: str = "be", width: int = 2) -> bytes:
    """把人員位址編成 width bytes。order='be' 高位在前（與文件範例一致）。"""
    if not 0 <= addr < (1 << (width * 8)):
        raise SoyalError(f"人員位址 {addr} 超出 {width} bytes 範圍")
    return addr.to_bytes(width, "big" if order == "be" else "little")


def decode_addr(raw: bytes, order: str = "be") -> int:
    return int.from_bytes(raw, "big" if order == "be" else "little")


def uid_to_bytes(uid_hex: str, order: str = "be") -> bytes:
    """把 TEXT 事件記錄內看到的 16 碼 UID 轉成 8 bytes。

    預設 order='be' ＝ 直接照 HEX 字串由左至右寫入，與 8031 TEXT 事件中看到的
    `00000000D4B81403` 相同順序（這也是官方 83H 範例呈現的順序）。
    """
    clean = uid_hex.strip().replace(" ", "").replace("-", "")
    if not clean:
        raise SoyalError("UID 不可為空")
    if len(clean) > 16:
        raise SoyalError(f"UID 超過 8 bytes: {uid_hex}")
    clean = clean.rjust(16, "0")
    raw = bytes.fromhex(clean)
    return raw if order == "be" else raw[::-1]


def card_to_uid_hex(card_spec: str) -> str:
    """把卡片號碼輸入轉成 16 碼 HEX UID（大端、與 8031 TEXT 事件形式相同）。

    支援兩種輸入（自動判別）：

    * ``site:card`` 十進位（如 ``'64867:29942'``）：
      Tag ID(32 bits) = (Site Code << 16) | Card Code
      → ``'64867:29942'`` → tag = ``0xFD6374F6`` → ``'00000000FD6374F6'``
      （與 PRD §2.6 一致：Tag UID bit31~16 = Site Code、bit15~0 = Card Code）
    * 16 碼 HEX UID：去掉空格 / 補零後原樣正規化輸出。

    位元組順序：一律以 'be' 回傳（TEXT 事件所見順序）；若需 little endian 寫入
    打卡機，請再搭配 ``uid_to_bytes(x, 'le')`` 或 ``punch_admin.py --uid-order le``。
    """
    clean = card_spec.strip().replace(" ", "").replace("-", "")
    if ":" in clean:
        site_txt, _, card_txt = clean.partition(":")
        try:
            site = int(site_txt, 10)
            card = int(card_txt, 10)
        except ValueError:
            raise SoyalError(f"site:card 須為十進位數字，收到: {card_spec!r}")
        if not 0 <= site <= 0xFFFF:
            raise SoyalError(f"Site Code 超出 16 bits 範圍: {site}")
        if not 0 <= card <= 0xFFFF:
            raise SoyalError(f"Card Code 超出 16 bits 範圍: {card}")
        tag = (site << 16) | card
        clean = f"00000000{tag:08X}"
    return uid_to_bytes(clean, "be").hex().upper()


def encode_mode(access: str = "card", **flags: bool) -> int:
    """組出 Mode byte（§2.19）。

    access：'invalid' | 'card' | 'card-or-pin' | 'card+pin'
    flags ：patrol_card / skip_card_after_fp / skip_fp_after_card /
            enable_expire / guest_pin_time / allow_pin_change
    """
    key = access.lower()
    if key not in ACCESS_MODES:
        raise SoyalError(f"未知的 Access Mode: {access}（可用 {list(ACCESS_MODES)}）")
    mode = ACCESS_MODES[key] << 6
    for name, bit in (
        ("patrol_card", 5),
        ("skip_card_after_fp", 4),
        ("skip_fp_after_card", 3),
        ("enable_expire", 2),
        ("guest_pin_time", 1),
        ("allow_pin_change", 0),
    ):
        if flags.get(name):
            mode |= 1 << bit
    return mode


def build_user_record(
    addr: int,
    uid_hex: str,
    pin: int = 0,
    mode: int = 0x40,
    zone: int = 0,
    group1: int = 0xFF,
    group2: int = 0xFF,
    year: int = 0x4F,
    month: int = 0x0C,
    day: int = 0x1F,
    level: int = 0,
    option: int = 0,
    addr_order: str = "be",
    uid_order: str = "be",
    trailing: bytes = b"\x00\x00\x00",
) -> bytes:
    """組出 E 系列 83H/84H 的單筆人員記錄（26 bytes）。

    預設值 = 無到期日（2079/12/31）、全部門組可用、等級 0、APB 關閉，
    等同官方 §2.19 範例（8E A1 4A FE 那筆測試卡）。
    """
    rec = encode_addr(addr, addr_order, 2)
    rec += uid_to_bytes(uid_hex, uid_order)        # Tag UID 8 bytes
    rec += (pin & 0xFFFFFFFF).to_bytes(4, "big")   # PIN 4 bytes
    rec += bytes([mode & 0xFF, zone & 0xFF, group1 & 0xFF, group2 & 0xFF])
    rec += bytes([year & 0xFF, month & 0xFF, day & 0xFF])
    rec += bytes([level & 0xFF, option & 0xFF])
    rec += trailing
    if len(rec) != 26:
        raise SoyalError(f"人員記錄長度 {len(rec)} != 26（trailing 需為 3 bytes）")
    return rec


def parse_user_record(
    rec: bytes, addr_order: str = "be", uid_order: str = "be", layout: str = "auto"
) -> dict:
    """解析人員記錄。

    實測（AR-821EFv5 / 韌體 4V6）兩種長度，差異只在最前面 2 bytes 的位址：

    * ``write``（26 bytes，83H/84H 下載用）：
      ``Addr(2) UID(8) PIN(4) Mode(1) Zone(1) G1(1) G2(1) Y(1) M(1) D(1) Level(1) Option(1) ×3``
    * ``read24``（24 bytes，87H 回讀用）：同上但**不含位址**，UID 從 offset 0 開始。

    已用實機驗證：寫入位址 1000 + UID ``000000008EA14AFE`` 後回讀得到
    ``00 00 00 00 8E A1 4A FE | 00 00 00 00 | 40 | 00 | FF | FF | 4F 0C 1F | 00 | 00 | 00 00 00``
    （24 bytes），完全符合上述結構與大端順序。
    """
    if layout == "auto":
        layout = "write" if len(rec) >= 26 else "read24"
    base = 2 if layout == "write" else 0
    if len(rec) < base + 14:
        raise SoyalError(f"人員記錄長度不足: {len(rec)}（layout={layout}）")
    raw_uid = rec[base : base + 8]
    # 空白位址：整筆（或 UID 欄位）都是 0xFF
    if set(rec) == {0xFF} or set(raw_uid) == {0xFF}:
        return {
            "empty": True,
            "addr": None,
            "note": "空白位址（未建人員）",
            "layout": layout,
            "raw": rec.hex(" ").upper(),
        }
    # Tag ID（32 bits）在 UID 欄位的最後 4 bytes：bit31~16 = Site Code、bit15~0 = Card Code
    tag32 = raw_uid[4:8]
    tail = rec[base + 14 :]
    return {
        # write 版面才有位址；87H 回讀（read24）不含位址
        "addr": decode_addr(rec[0:2], addr_order) if layout == "write" else None,
        "addr_note": None if layout == "write" else "87H 回讀不含位址，請自行記錄查詢位址",
        "uid_hex": (raw_uid if uid_order == "be" else raw_uid[::-1]).hex().upper(),
        "layout": layout,
        "tag_id": tag32.hex().upper(),
        "site_code": int.from_bytes(tag32[0:2], "big"),
        "card_code": int.from_bytes(tag32[2:4], "big"),
        "pin": int.from_bytes(rec[base + 8 : base + 12], "big"),
        "mode": rec[base + 12],
        "access_mode": {v: k for k, v in ACCESS_MODES.items()}.get(rec[base + 12] >> 6, "?"),
        "zone": rec[base + 13],
        "group1": tail[0] if len(tail) > 0 else None,
        "group2": tail[1] if len(tail) > 1 else None,
        "expire": (f"20{tail[2]:02d}-{tail[3]:02d}-{tail[4]:02d}" if len(tail) > 4 else None),
        "level": (tail[5] >> 6) if len(tail) > 5 else None,
        "option": tail[6] if len(tail) > 6 else None,
        "raw": rec.hex(" ").upper(),
    }


# ---------------------------------------------------------------------------
# 回覆解讀
# ---------------------------------------------------------------------------


def parse_rtc(pkt: Packet) -> dict:
    """解讀 24H 回覆。

    實機回覆格式（實測 192.168.1.127，韌體 4V6，echo code = 0x03；Length 欄位為 0x24）：
        data[0]      = Source / Reader ID
        data[1]      = Second
        data[2]      = Minute
        data[3]      = Hour
        data[4]      = Weekday（1~7 = Sunday~Saturday）
        data[5]      = Day
        data[6]      = Month
        data[7]      = Year % 100
        data[8]      = Firmware Version（0x46 = 4V6，與文件 Data7 範例 0x17=1V7 相符）
        data[9..]    = 其他控制器資訊（型號 / 網路 / 緩衝區）
    """
    d = pkt.data
    if pkt.cmd != 0x03 or len(d) < 9:
        raise SoyalError(f"非預期的 24H 回覆: {pkt}")

    def o(i: int) -> str:
        return f"{d[i]:02d}" if i < len(d) else "??"

    weekdays = ["?", "Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"]
    wd = d[4] if d[4] < len(weekdays) else 0
    return {
        "source_id": d[0],
        "time": (
            f"20{o(7)}-{o(6)}-{o(5)} {o(3)}:{o(2)}:{o(1)} "
            f"({weekdays[wd]})"
        ),
        "firmware": f"{d[8] >> 4}V{d[8] & 0x0F} (0x{d[8]:02X})",
        "extra": d[9:].hex(" ").upper(),
        "raw": d.hex(" ").upper(),
    }


def parse_event_log(pkt: Packet) -> dict:
    """解讀 25H 帶回的事件記錄（格式見 §4.1）。

    實測：本機對 25H 以 echo code 0x18 回覆（文件範例為 0x03），因此**不檢查 echo code**，
    只要資料長度足夠（>= 21 bytes）就視為事件記錄。
    這裡拿得到 TEXT 模式拿不到的資料：User Address / Sub Code（含 Duty code）/ Level。
    """
    d = pkt.data
    if len(d) < 21:
        raise SoyalError(f"事件記錄長度不足（{len(d)} bytes）: {d.hex(' ')}")
    sub_code = d[11]
    user_address = (d[9] << 8) | d[10]
    return {
        "source_node": d[0],
        "time": f"20{d[7]:02d}-{d[6]:02d}-{d[5]:02d} {d[3]:02d}:{d[2]:02d}:{d[1]:02d}",
        "port_number": d[8],
        "user_address": user_address,
        # 位址 >16383 代表此筆為「無效卡片」事件，Data 9/10 放的是 Tag ID 低 16 bits
        "user_address_note": (
            "正常人員位址" if user_address <= 16383 else "疑似無效卡片（實為 Tag ID bit15~00）"
        ),
        "sub_code": sub_code,
        "duty_code": sub_code >> 5,
        "duty_label": DUTY_LABELS[sub_code >> 5],
        "sub_func": d[12],
        "ext_code": d[13],
        "user_level": d[14],
        "tag_id": bytes(d[15:17] + d[19:21]).hex().upper(),
        "door_no": d[17],
        "raw": d.hex(" ").upper(),
    }