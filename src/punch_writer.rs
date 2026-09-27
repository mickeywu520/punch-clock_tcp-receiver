//! SOYAL 控制器寫入人員協定（83H/84H 設定人員＋2EH 寫姓名＋87H 空位掃描）。
//! 由 `tools/soyal_proto.py` + `tools/punch_admin.py` 的實作移植而來：
//!
//! * 短封包：`7E <len> DID CMD <data...> XOR SUM`
//!   * `len` = body(DID..data) 長度 + 2（XOR+SUM）
//!   * `xor` = 0xFF 逐一 XOR body；`sum` = (sum(body) + xor) & 0xFF
//! * 84H 寫入回 ACK（0x04）/ NACK（0x05）/ 認證錯誤（0x06）/ 協定錯誤（0x0C）
//! * 26-byte record：`Addr(2) UID(8) PIN(4) Mode(1) Zone(1) G1(1) G2(1) Y(1) M(1) D(1) Level(1) Option(1) 保留(3)`
//! * 2EH 姓名：`Addr(3) count(1) 16B Big5 字串`（大端）
//! * 87H：`Addr(2) nums(1)`，回 24-byte records（不含位址）

use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

// ---------------------------------------------------------------------------
// 協定常數
// ---------------------------------------------------------------------------

pub const HEAD_SHORT: u8 = 0x7E;
pub const HEAD_LARGE: [u8; 4] = [0xFF, 0x00, 0x5A, 0xA5];

pub const CMD_DATA: u8 = 0x03; // 資料回覆
pub const ECHO_ACK: u8 = 0x04; // 寫入成功
pub const ECHO_NACK: u8 = 0x05; // 拒絕
pub const ECHO_AUTH_ERR: u8 = 0x06; // 認證錯誤
pub const ECHO_PROTO_ERR: u8 = 0x0C; // 協定/格式錯誤

pub const CMD_SET_USER: u8 = 0x84; // 新增/覆寫人員（無 APB）
#[allow(dead_code)]
pub const CMD_SET_USER_APB: u8 = 0x83; // 同上（含 anti-pass-back）
pub const CMD_WRITE_ALIAS: u8 = 0x2E; // 寫入姓名
pub const CMD_READ_USER: u8 = 0x87; // 回讀人員

const OP_TIMEOUT: Duration = Duration::from_secs(3);
const CONNECT_TIMEOUT: Duration = Duration::from_secs(3);
const MAX_AUTO_SCAN: u16 = 300;

/// 通行模式（Mode byte 8.19：bit7~6）
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum AccessMode {
    /// 僅卡片
    #[default]
    Card,
    /// 卡片或密碼
    CardOrPin,
    /// 卡片＋密碼
    CardPlusPin,
}

impl AccessMode {
    pub fn label(self) -> &'static str {
        match self {
            AccessMode::Card => "卡片驗證",
            AccessMode::CardOrPin => "卡片或密碼",
            AccessMode::CardPlusPin => "卡片+密碼",
        }
    }

    pub fn from_label(label: &str) -> Self {
        match label {
            "卡片或密碼" => AccessMode::CardOrPin,
            "卡片+密碼" => AccessMode::CardPlusPin,
            _ => AccessMode::Card,
        }
    }

    fn mode_byte(self) -> u8 {
        match self {
            AccessMode::Card => 0x40,
            AccessMode::CardOrPin => 0x80,
            AccessMode::CardPlusPin => 0xC0,
        }
    }
}

/// 一筆要寫入的人員
#[derive(Debug, Clone)]
pub struct PersonEntry {
    /// `site:card`（十進位）或 16 碼 HEX UID
    pub card_spec: String,
    pub name: Option<String>,
    /// 指定人員位址；`None` = 自動掃描下一個空位
    pub addr: Option<u16>,
    pub mode: AccessMode,
}

/// 單筆寫入結果
#[derive(Debug, Clone)]
pub struct WriteOutcome {
    pub addr: u16,
    pub uid_hex: String,
    pub name: Option<String>,
    pub ok: bool,
    pub detail: String,
}

// ---------------------------------------------------------------------------
// 封包組裝 / 解析（純函式）
// ---------------------------------------------------------------------------

pub fn checksum(body: &[u8]) -> (u8, u8) {
    let mut xor = 0xFFu8;
    let mut total = 0u32;
    for &b in body {
        xor ^= b;
        total += b as u32;
    }
    (xor, ((total + xor as u32) & 0xFF) as u8)
}

/// 組出 SOYAL 短封包。
pub fn build_short(did: u8, cmd: u8, data: &[u8]) -> Vec<u8> {
    let mut body = Vec::with_capacity(data.len() + 2);
    body.push(did);
    body.push(cmd);
    body.extend_from_slice(data);
    let len = body.len() + 2;
    let (xor, total) = checksum(&body);
    let mut pkt = Vec::with_capacity(len + 2);
    pkt.push(HEAD_SHORT);
    pkt.push(len as u8);
    pkt.extend_from_slice(&body);
    pkt.push(xor);
    pkt.push(total);
    pkt
}

/// 已解析的一包資料。
#[derive(Debug, Clone)]
pub struct Reply {
    #[allow(dead_code)]
    pub did: u8,
    pub cmd: u8,
    pub data: Vec<u8>,
    #[allow(dead_code)]
    pub checksum_ok: bool,
}

/// 從緩衝區頭部取出完整一包（短或長）。資料不足或需併收時回 `Err(NeedMore)`。
enum Extract {
    Packet(Reply),
    NeedMore,
    Noise,
}

fn extract(buf: &mut Vec<u8>) -> Extract {
    if buf.is_empty() {
        return Extract::NeedMore;
    }
    if buf[0] == HEAD_SHORT {
        if buf.len() < 5 {
            return Extract::NeedMore;
        }
        let total = buf[1] as usize + 2;
        if buf.len() < total {
            return Extract::NeedMore;
        }
        let raw: Vec<u8> = buf.drain(..total).collect();
        let body = &raw[2..total - 2];
        let (xor, sum) = checksum(body);
        return Extract::Packet(Reply {
            did: raw[2],
            cmd: raw[3],
            data: body[2..].to_vec(),
            checksum_ok: raw[total - 2] == xor && raw[total - 1] == sum,
        });
    }
    if buf.starts_with(&HEAD_LARGE) {
        if buf.len() < 6 {
            return Extract::NeedMore;
        }
        let len_field = u16::from_be_bytes([buf[4], buf[5]]) & 0x0FFF;
        let total = 6 + len_field as usize;
        if buf.len() < total {
            return Extract::NeedMore;
        }
        let raw: Vec<u8> = buf.drain(..total).collect();
        let body = &raw[6..total - 2];
        let (xor, sum) = checksum(body);
        return Extract::Packet(Reply {
            did: raw[6],
            cmd: raw[7],
            data: body[2..].to_vec(),
            checksum_ok: raw[total - 2] == xor && raw[total - 1] == sum,
        });
    }
    // 丟棄擋在開頭的非封包雜訊，直到找到短/長標頭
    if let Some(pos) = buf
        .iter()
        .position(|&b| b == HEAD_SHORT || b == HEAD_LARGE[0])
    {
        buf.drain(..pos);
        Extract::Noise
    } else {
        buf.clear();
        Extract::Noise
    }
}

// ---------------------------------------------------------------------------
// 欄位編碼
// ---------------------------------------------------------------------------

/// `site:card`（十進位）或 16 碼 HEX → 8 bytes UID（big-endian）。
pub fn card_spec_to_uid_bytes(spec: &str) -> Result<[u8; 8], String> {
    let clean = spec.trim().replace(' ', "").replace('-', "");
    if clean.is_empty() {
        return Err("卡號不可為空".to_string());
    }
    let hex: String = if let Some((site_txt, card_txt)) = clean.split_once(':') {
        let site: u32 = site_txt
            .trim()
            .parse()
            .map_err(|_| format!("site:card 的 Site 需為十進位數字，收到 {site_txt:?}"))?;
        let card: u32 = card_txt
            .trim()
            .parse()
            .map_err(|_| format!("site:card 的 Card 需為十進位數字，收到 {card_txt:?}"))?;
        if site > 0xFFFF || card > 0xFFFF {
            return Err(format!("site/card 超出 16 bits 範圍：{site}:{card}"));
        }
        let tag = (site << 16) | card;
        format!("00000000{tag:08X}")
    } else {
        clean
    };
    if hex.len() > 16 {
        return Err(format!("UID 超過 8 bytes：{spec}"));
    }
    if hex.len() % 2 != 0 {
        return Err(format!("UID 需為偶數位 HEX：{spec}"));
    }
    let padded = format!("{:0>16}", hex);
    let mut out = [0u8; 8];
    for (i, ch) in padded.as_bytes().chunks(2).enumerate() {
        let byte = u8::from_str_radix(std::str::from_utf8(ch).unwrap(), 16)
            .map_err(|_| format!("UID 含非 HEX 字元：{spec}"))?;
        out[i] = byte;
    }
    Ok(out)
}

/// 26-byte 人員 record（Mode byte 由 `AccessMode` 提供）。
pub fn build_user_record(addr: u16, uid: &[u8; 8], mode: u8) -> Vec<u8> {
    let mut rec = Vec::with_capacity(26);
    rec.extend_from_slice(&addr.to_be_bytes()); //      Addr(2)
    rec.extend_from_slice(uid); //                       UID(8)
    rec.extend_from_slice(&0u32.to_be_bytes()); //      PIN(4)=0
    rec.push(mode); //                                    Mode(1)
    rec.push(0x00); //                                    Zone(1)
    rec.push(0xFF); //                                    G1(1)
    rec.push(0xFF); //                                    G2(1)
    rec.push(0x4F); //                                    Y=2079
    rec.push(0x0C); //                                    M=12
    rec.push(0x1F); //                                    D=31
    rec.push(0x00); //                                    Level(1)
    rec.push(0x00); //                                    Option(1)
    rec.extend_from_slice(&[0x00, 0x00, 0x00]); //       保留(3)
    rec
}

/// 2EH 姓名封包資料：`Addr(3) count(1) 16B Big5`。
pub fn build_alias_data(addr: u16, name: &str, encoding: &'static encoding_rs::Encoding) -> Result<Vec<u8>, String> {
    let (bytes, _, _) = encoding.encode(name);
    let raw = bytes.into_owned();
    if raw.is_empty() {
        return Err("姓名不可為空".to_string());
    }
    let mut padded = [0u8; 16];
    let n = raw.len().min(16);
    padded[..n].copy_from_slice(&raw[..n]);
    let mut data = Vec::with_capacity(20);
    data.push(0); // 位址 3 bytes 大端，先 MSB（0）
    data.extend_from_slice(&addr.to_be_bytes());
    data.push(1); // count
    data.extend_from_slice(&padded);
    Ok(data)
}

// ---------------------------------------------------------------------------
// TCP 收送
// ---------------------------------------------------------------------------

struct Conn {
    stream: TcpStream,
    buf: Vec<u8>,
}

impl Conn {
    async fn connect(ip: &str, port: u16) -> Result<Self, String> {
        let addr = format!("{ip}:{port}")
            .parse::<std::net::SocketAddr>()
            .map_err(|e| format!("無法解析 {ip}:{port}：{e}"))?;
        let stream = tokio::time::timeout(CONNECT_TIMEOUT, TcpStream::connect(addr))
            .await
            .map_err(|_| format!("連線 {ip}:{port} 逾時"))?
            .map_err(|e| format!("連線 {ip}:{port} 失敗：{e}"))?;
        Ok(Conn {
            stream,
            buf: Vec::new(),
        })
    }

    async fn next_packet(&mut self, deadline: tokio::time::Instant) -> Result<Reply, String> {
        loop {
            match extract(&mut self.buf) {
                Extract::Packet(p) => return Ok(p),
                Extract::NeedMore => {}
                Extract::Noise => {}
            }
            if tokio::time::Instant::now() >= deadline {
                return Err(format!(
                    "等待卡鐘回應逾時（已收 {} bytes）",
                    self.buf.len()
                ));
            }
            let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
            let mut chunk = [0u8; 4096];
            let n = tokio::time::timeout(remaining, self.stream.read(&mut chunk))
                .await
                .map_err(|_| "等待卡鐘回應逾時".to_string())?
                .map_err(|_| "與卡鐘連線中斷".to_string())?;
            if n == 0 {
                return Err("與卡鐘連線中斷（EOF）".to_string());
            }
            self.buf.extend_from_slice(&chunk[..n]);
        }
    }

    async fn request(&mut self, pkt: &[u8], timeout: Duration) -> Result<Reply, String> {
        self.stream
            .write_all(pkt)
            .await
            .map_err(|e| format!("送出封包失敗：{e}"))?;
        let deadline = tokio::time::Instant::now() + timeout;
        self.next_packet(deadline).await
    }

    /// 寫入指令後，跳過 echo（含 0x00 / 自身 cmd），等到終結 code 才回。
    async fn wait_ack(&mut self, timeout: Duration) -> Result<Reply, String> {
        let deadline = tokio::time::Instant::now() + timeout;
        loop {
            let r = self.next_packet(deadline).await?;
            match r.cmd {
                ECHO_ACK | ECHO_NACK | ECHO_AUTH_ERR | ECHO_PROTO_ERR => return Ok(r),
                _ => continue,
            }
        }
    }
}

// ---------------------------------------------------------------------------
// 掃描與寫入
// ---------------------------------------------------------------------------

fn is_empty_record(rec: &[u8]) -> bool {
    rec.iter().all(|&b| b == 0xFF) || rec.iter().take(8).all(|&b| b == 0xFF)
}

/// 87H 從 `start` 找下一個空位（每次讀 20 筆，掃到 `start+MAX_AUTO_SCAN`）。
async fn find_free_addr(
    conn: &mut Conn,
    did: u8,
    start: u16,
) -> Result<u16, String> {
    let mut addr = start;
    let end = start.saturating_add(MAX_AUTO_SCAN);
    while addr < end {
        let mut data = Vec::with_capacity(3);
        data.extend_from_slice(&addr.to_be_bytes());
        data.push(20);
        let pkt = build_short(did, CMD_READ_USER, &data);
        let r = conn.request(&pkt, OP_TIMEOUT).await?;
        match r.cmd {
            CMD_DATA => {
                let recs: Vec<&[u8]> = r.data.chunks(24).collect();
                let free: Vec<u16> = recs
                    .iter()
                    .enumerate()
                    .filter(|(_, rec)| is_empty_record(rec))
                    .map(|(i, _)| addr + i as u16)
                    .collect();
                if let Some(&f) = free.first() {
                    return Ok(f);
                }
                if recs.len() < 20 {
                    return Ok(addr + recs.len() as u16);
                }
                addr = addr.saturating_add(20);
            }
            ECHO_NACK => return Ok(addr),
            _ => {
                // 非預期回覆（例如 0x00），跳過此段再試
                addr = addr.saturating_add(20);
            }
        }
    }
    Err(format!("未找到空位人員位址（已掃描 {MAX_AUTO_SCAN} 個位址）"))
}

/// 批次寫入人員。連接一次、逐筆 84H→ACK、選填 2EH→姓名。
///
/// `entries` 中 `addr: None` 者自動掃描空位（依序遞進寫入位址）。
/// 個別筆失敗會記入 `WriteOutcome` 而不中斷整批。
pub async fn add_people(
    ip: &str,
    port: u16,
    did: u8,
    entries: Vec<PersonEntry>,
) -> Result<Vec<WriteOutcome>, String> {
    if entries.is_empty() {
        return Ok(Vec::new());
    }
    let mut conn = Conn::connect(ip, port).await?;
    let mut next_addr: u16 = 1;
    let mut results = Vec::with_capacity(entries.len());

    for ent in entries {
        // 解析卡號
        let uid = match card_spec_to_uid_bytes(&ent.card_spec) {
            Ok(u) => u,
            Err(msg) => {
                results.push(WriteOutcome {
                    addr: ent.addr.unwrap_or(0),
                    uid_hex: ent.card_spec.clone(),
                    name: ent.name.clone(),
                    ok: false,
                    detail: msg,
                });
                continue;
            }
        };
        let uid_hex = uid.iter().map(|b| format!("{b:02X}")).collect::<String>();

        // 位址
        let addr = match ent.addr {
            Some(a) => a,
            None => match find_free_addr(&mut conn, did, next_addr).await {
                Ok(a) => a,
                Err(msg) => {
                    results.push(WriteOutcome {
                        addr: 0,
                        uid_hex,
                        name: ent.name.clone(),
                        ok: false,
                        detail: msg,
                    });
                    continue;
                }
            },
        };

        // 84H 寫入單筆
        let record = build_user_record(addr, &uid, ent.mode.mode_byte());
        let mut payload = Vec::with_capacity(27);
        payload.push(1); // records count (1 byte BE)
        payload.extend_from_slice(&record);
        let pkt = build_short(did, CMD_SET_USER, &payload);

        let mut detail = String::new();
        let mut ok = true;
        match conn.request(&pkt, OP_TIMEOUT).await {
            Ok(_first) => match conn.wait_ack(OP_TIMEOUT).await {
                Ok(r) if r.cmd == ECHO_ACK => {}
                Ok(r) => {
                    ok = false;
                    detail = format!("卡鐘回應：{} (0x{:02X})", echo_name(r.cmd), r.cmd);
                }
                Err(msg) => {
                    ok = false;
                    detail = msg;
                }
            },
            Err(msg) => {
                ok = false;
                detail = msg;
            }
        }
        if ok {
            next_addr = addr.saturating_add(1);
        }

        // 2EH 姓名（選填）
        if ok {
            if let Some(name) = ent.name.as_deref().map(str::trim).filter(|n| !n.is_empty()) {
                match build_alias_data(addr, name, encoding_rs::BIG5) {
                    Ok(ali) => {
                        let apkt = build_short(did, CMD_WRITE_ALIAS, &ali);
                        match conn.request(&apkt, OP_TIMEOUT).await {
                            Ok(_) => match conn.wait_ack(OP_TIMEOUT).await {
                                Ok(r) if r.cmd == ECHO_ACK => {}
                                Ok(r) => {
                                    detail = format!(
                                        "新增成功但姓名寫入失敗：{} (0x{:02X})",
                                        echo_name(r.cmd),
                                        r.cmd
                                    )
                                }
                                Err(msg) => detail = format!("新增成功但姓名寫入失敗：{msg}"),
                            },
                            Err(msg) => detail = format!("新增成功但姓名寫入失敗：{msg}"),
                        }
                    }
                    Err(msg) => detail = format!("姓名編碼失敗：{msg}"),
                }
            }
        }

        results.push(WriteOutcome {
            addr,
            uid_hex,
            name: ent.name,
            ok,
            detail,
        });
    }

    Ok(results)
}

/// Echo code 人類可讀名稱。
pub fn echo_name(cmd: u8) -> &'static str {
    match cmd {
        CMD_DATA => "資料回覆",
        ECHO_ACK => "ACK（成功）",
        ECHO_NACK => "NACK（拒絕）",
        ECHO_AUTH_ERR => "認證錯誤",
        ECHO_PROTO_ERR => "協定/格式錯誤",
        _ => "未知回應",
    }
}

// ---------------------------------------------------------------------------
// CSV 批次匯入解析
// ---------------------------------------------------------------------------

/// 解析批次匯入 CSV。每行一筆：第一欄卡號（`site:card` 或 HEX），第二欄（選填）姓名。
/// 支援 UTF-8（含 BOM）與 Big5 兩種來源編碼；以逗號/分號/定位鍵分隔。
pub fn parse_csv(bytes: &[u8]) -> Result<Vec<PersonEntry>, String> {
    let text = decode_csv(bytes)?;
    let mut entries = Vec::new();
    for (idx, raw_line) in text.lines().enumerate() {
        let line = raw_line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let (card, name) = split_row(line);
        let card = card.trim();
        if card.is_empty() {
            return Err(format!("第 {} 行缺卡號", idx + 1));
        }
        // 解析以提早報錯（不中斷），但保留原字串給 add_people 重解析
        card_spec_to_uid_bytes(card).map_err(|e| format!("第 {} 行：{e}", idx + 1))?;
        entries.push(PersonEntry {
            card_spec: card.to_string(),
            name: if name.trim().is_empty() {
                None
            } else {
                Some(name.trim().to_string())
            },
            addr: None,
            mode: AccessMode::Card,
        });
    }
    if entries.is_empty() {
        return Err("CSV 中沒有可匯入的資料列".to_string());
    }
    Ok(entries)
}

fn decode_csv(bytes: &[u8]) -> Result<String, String> {
    let bytes = if bytes.starts_with(&[0xEF, 0xBB, 0xBF]) {
        &bytes[3..]
    } else {
        bytes
    };
    match std::str::from_utf8(bytes) {
        Ok(s) => Ok(s.to_string()),
        Err(_) => {
            let (decoded, _, _) = encoding_rs::BIG5.decode(bytes);
            Ok(decoded.into_owned())
        }
    }
}

/// 以逗號/分號/定位鍵切第一與第二欄。
fn split_row(line: &str) -> (&str, &str) {
    for sep in [',', ';', '\t'] {
        if let Some(pos) = line.find(sep) {
            return (&line[..pos], &line[pos + 1..]);
        }
    }
    (line, "")
}

// ---------------------------------------------------------------------------
// 測試
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn checksum_poll_vector() {
        // 官方範例：7E 04 01 18 E6 FF
        let pkt = build_short(1, 0x18, &[]);
        assert_eq!(pkt, vec![0x7E, 0x04, 0x01, 0x18, 0xE6, 0xFF]);
    }

    #[test]
    fn build_short_known_record_packet_checksum() {
        let uid = *b"\x00\x00\x00\x00\xD4\xB8\x14\x03";
        let rec = build_user_record(1000, &uid, 0x40);
        assert_eq!(rec.len(), 26);
        let mut payload = vec![1u8];
        payload.extend_from_slice(&rec);
        let pkt = build_short(1, CMD_SET_USER, &payload);
        let body = &pkt[2..pkt.len() - 2];
        let (xor, sum) = checksum(body);
        assert_eq!(pkt[pkt.len() - 2], xor);
        assert_eq!(pkt[pkt.len() - 1], sum);
    }

    #[test]
    fn card_spec_site_colon_card() {
        // PRD 範例：64867:29942 → tag 0xFD6374F6
        let uid = card_spec_to_uid_bytes("64867:29942").unwrap();
        assert_eq!(uid, [0x00, 0x00, 0x00, 0x00, 0xFD, 0x63, 0x74, 0xF6]);
    }

    #[test]
    fn card_spec_hex_and_whitespace() {
        let a = card_spec_to_uid_bytes("00 00 00 00 FD 63 74 F6").unwrap();
        let b = card_spec_to_uid_bytes("fd6374f6").unwrap();
        assert_eq!(a, b);
        assert_eq!(a, [0x00, 0x00, 0x00, 0x00, 0xFD, 0x63, 0x74, 0xF6]);
    }

    #[test]
    fn card_spec_rejects_oob() {
        assert!(card_spec_to_uid_bytes("70000:1").is_err());
        assert!(card_spec_to_uid_bytes("1:70000").is_err());
        assert!(card_spec_to_uid_bytes("abcd12zz").is_err());
    }

    #[test]
    fn record_layout_flat() {
        // 實測寫入後 87H 讀回：00 00 00 00 8E A1 4A FE | 00 00 00 00 | 40 | 00 | FF | FF | 4F 0C 1F | 00 | 00 | 00 00 00
        let uid = [0x00, 0x00, 0x00, 0x00, 0x8E, 0xA1, 0x4A, 0xFE];
        let rec = build_user_record(1000, &uid, 0x40);
        let expected: Vec<u8> = vec![
            0x03, 0xE8, 0x00, 0x00, 0x00, 0x00, 0x8E, 0xA1, 0x4A, 0xFE, 0x00, 0x00, 0x00, 0x00,
            0x40, 0x00, 0xFF, 0xFF, 0x4F, 0x0C, 0x1F, 0x00, 0x00, 0x00, 0x00, 0x00,
        ];
        assert_eq!(rec, expected);
    }

    #[test]
    fn alias_data_big5_padding() {
        // "測試" Big5 = B4 FA B8 D5
        let data = build_alias_data(123, "測試", encoding_rs::BIG5).unwrap();
        assert_eq!(data.len(), 20);
        assert_eq!(&data[..3], &[0x00, 0x00, 0x7B]); // addr 123 3 bytes BE
        assert_eq!(data[3], 1); // count
        assert_eq!(&data[4..8], &[0xB4, 0xFA, 0xB8, 0xD5]);
        assert!(data[8..].iter().all(|&b| b == 0));
    }

    #[test]
    fn extract_two_packets_from_stream() {
        let mut buf = build_short(1, 0x18, &[]);
        buf.extend_from_slice(&build_short(1, ECHO_ACK, &[]));
        let got = match extract(&mut buf) {
            Extract::Packet(p) => p,
            _ => panic!("應取出一包"),
        };
        assert_eq!(got.cmd, 0x18);
        assert!(got.checksum_ok);
        let got2 = match extract(&mut buf) {
            Extract::Packet(p) => p,
            _ => panic!("應取出第二包"),
        };
        assert_eq!(got2.cmd, ECHO_ACK);
    }

    #[test]
    fn empty_record_detection() {
        assert!(is_empty_record(&[0xFF; 24]));
        assert!(is_empty_record(&[0xFF; 8]));
        assert!(!is_empty_record(&[0x00; 24]));
    }

    #[test]
    fn access_mode_map() {
        assert_eq!(AccessMode::from_label("卡片或密碼"), AccessMode::CardOrPin);
        assert_eq!(AccessMode::Card.mode_byte(), 0x40);
        assert_eq!(AccessMode::CardOrPin.mode_byte(), 0x80);
        assert_eq!(AccessMode::CardPlusPin.mode_byte(), 0xC0);
    }

    #[test]
    fn csv_parse_utf8_and_big5() {
        let utf8 = "64867:29942,王小明\n00 00 00 00 FD 63 74 F6,李四\n";
        let entries = parse_csv(utf8.as_bytes()).unwrap();
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].name.as_deref(), Some("王小明"));
        assert_eq!(entries[1].card_spec, "00 00 00 00 FD 63 74 F6");

        let big5 = encoding_rs::BIG5.encode("64867:29942,測試").0.into_owned();
        let entries = parse_csv(&big5).unwrap();
        assert_eq!(entries[0].name.as_deref(), Some("測試"));
    }

    #[test]
    fn csv_skips_comments_and_blank() {
        let s = "# 註解\n\n64867:1,  \n";
        let entries = parse_csv(s.as_bytes()).unwrap();
        assert_eq!(entries.len(), 1);
        assert!(entries[0].name.is_none());
    }

    #[test]
    fn csv_invalid_line_reports_row() {
        let s = "not-a-card\n";
        assert!(parse_csv(s.as_bytes()).unwrap_err().contains("第 1 行"));
    }
}