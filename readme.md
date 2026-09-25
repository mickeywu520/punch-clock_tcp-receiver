# punch-clock_tcp-receiver

Rust 實作的 **SOYAL AR837EF 打卡機 TCP Receiver**。不透過官方 701Server，直接聆聽打卡機「Message Server」主動推播的 8031 TEXT 記錄，解析後以共用 JSON Protocol 送往 GCP。

詳細規格見 [PRD.md](PRD.md)。

## 架構

```
AR837EF ──TCP 8031 (TEXT, 主動連出)──▶ receiver ──HTTPS JSON──▶ GCP (Cloud Run / API GW / Pub/Sub)
```

## 需求

- Rust 1.70+（`cargo --version`）

## Build & Run

```bash
cargo build --release

# 以 config.json 啟動（未提供則用預設值 + 環境變數）
.\target\release\punch-clock-tcp-receiver.exe config.json
# 或無參數（使用預設 0.0.0.0:8031）
.\target\release\punch-clock-tcp-receiver.exe
```

複製設定檔：

```bash
Copy-Item config.example.json config.json   # PowerShell
```

### 設定

| 參數 | 環境變數 | 預設 | 說明 |
|---|---|---|---|
| `listen.bind` | `PUNCH_BIND` | `0.0.0.0` | 監聽位址 |
| `listen.port` | `PUNCH_PORT` | `8031` | 監聽埠（TEXT 模式） |
| `listen.mode` | - | `text` | 目前僅支援 `text` |
| `gcp.endpoint_url` | `PUNCH_GCP_URL` | 無 | GCP 收件 URL（POST JSON） |
| `gcp.bearer_token` | `PUNCH_GCP_TOKEN` | 無 | `Authorization: Bearer` |
| `gcp.api_key_value` | `PUNCH_GCP_API_KEY` | 無 | header 驗證 |
| `gcp.api_key_header` | `PUNCH_GCP_API_KEY_HEADER` | `X-Api-Key` | header 名稱 |
| `receiver_id` | `PUNCH_RECEIVER_ID` | `punch-clock-01` | 實例識別 |
| `spool_dir` | - | `./spool` | 送達失敗暫存目錄（JSONL） |

## 打卡機（機端）設定

瀏覽器進入控制器 Web（`Network Setting`）：

- `Message Server IP 1st` = receiver 的 IP
- `Message Port 1st` = `8031`（單向 TEXT，不需 ACK）
- `Message Server IP 2nd` = （選填）備援
- 儲存後，每刷一次卡 receiver 即收到一筆記錄。

## 驗證（不接真實打卡機）

**1) 起一個假的 GCP 收件端**（另開視窗）：

```bash
python -m http.server 9000
# 或用 mock_gcp.py (Tools/python simple POST echo)
```

**2) 啟動 receiver**（指向假 GCP）：

```bash
$env:PUNCH_GCP_URL="http://127.0.0.1:9000/api/punch-events"
.\target\release\punch-clock-tcp-receiver.exe
```

**3) 模擬打卡機推記錄**：

```bash
python tools/mock_sender.py --host 127.0.0.1 --port 8031 --count 3 --interval 1
```

**4) 執行單元測試**：

```bash
cargo test
```

測試含官方範例行：`21'05/12 13:38:54 [001.17:0B](0)00000000D4B81403 rSammi (M11)Normal Access`。

## 與真實打卡機連線測試（`tools/`）

`tools/` 內有四支 Python 工具（僅用標準庫）：

| 工具 | 用途 |
|---|---|
| `soyal_proto.py` | SOYAL 標準封包組裝/解析（0x7E 短封包、FF 00 5A A5 長封包、XOR/SUM、TCP 串流切包） |
| `punch_admin.py` | CLI：通訊測試、新增／讀取／刪除人員與卡片、讀取事件記錄 |
| `mock_device.py` | 假打卡機（沒有實機時可離線驗證整條路徑） |
| `raw_probe.py` | 原始位元組探測（釐清某個 Port 到底吃哪種格式） |

### 1) 離線自我測試（不需連線）

```bash
python tools/punch_admin.py selftest     # 38 項：datasheet 範例 + 實機擷取向量
```

### 2) 沒有實機時，先跑假打卡機

```bash
python tools/mock_device.py --port 1621                        # 視窗 A
python tools/punch_admin.py info --host 127.0.0.1 --port 1621  # 視窗 B
```

### 3) 對真實打卡機（實測 192.168.1.127:1621）

```bash
python tools/punch_admin.py probe --host 192.168.1.127 --ports 1621,1601,80
python tools/punch_admin.py info  --host 192.168.1.127 --port 1621       # 24H 時間/版本 + 18H
python tools/punch_admin.py dump-log --host 192.168.1.127 --port 1621    # 25H 讀事件記錄
python tools/punch_admin.py scan-users --host 192.168.1.127 --port 1621 --start 1 --end 30

# 寫入類預設 dry-run（只印封包），一定要加 --yes 才會真的寫入
python tools/punch_admin.py add-user --host 192.168.1.127 --port 1621 \
    --addr 1000 --uid 00000000D4B81403 --yes
# --uid 也可直接吃 site:card 十進位（Site Code : Card Code，PRD §2.6）：
python tools/punch_admin.py add-user --host 192.168.1.127 --port 1621 --addr 1 --uid 64867:29942 --yes
python tools/punch_admin.py verify   --host 192.168.1.127 --port 1621 \
    --addr 1 --expect-uid 64867:29942
python tools/punch_admin.py del-user --host 192.168.1.127 --port 1621 \
    --start 1000 --end 1000 --yes
```

### 4) 實機驗證結果（2026-09-19；AR-821EFv5、韌體 4V6、Node ID 1、TCP Port 1621）

| 項目 | 送出的封包 | 實機回覆 | 結果 |
|---|---|---|---|
| 18H 輪詢 | `7E 04 01 18 E6 FF` | `7E 0A 00 09 01 00 01 00 10 40 A6 01` | ✅ 可用 |
| 24H 讀時間/版本 | `7E 04 01 24 DA FF` | `7E 24 00 03 01 1A 0C 00 03 09 0B 0A 46 …` | ✅ 韌體 4V6；**機端 RTC 未校時（2010-11-09）** |
| 25H 讀事件記錄 | `7E 04 01 25 DB 01` | `7E 21 00 18 01 32 22 0E …` | ✅ 可讀（echo code 為 `0x18`，非文件寫的 `0x03`） |
| 84H 新增人員+卡片 | `7E 1F 01 84 01 …` | `7E 0F 00 04 01 C3 46 0F 91 10 10 00 …` | ✅ ACK |
| 87H 回讀人員 | `7E 07 01 87 03 E8 01 93 07` | `7E 1D 00 03 01 00 00 00 00 8E A1 4A FE …` | ✅ 與寫入內容一致 |
| 85H 刪除人員 | `7E 08 01 85 03 E8 03 E8 7B D7` | `7E 0F 00 04 01 C3 46 …` | ✅ ACK，回讀變空白 |

**重要結論（校正先前假設）**

1. 寫入型指令（`83H/84H/85H/87H/2EH`）**在一般 TCP 指令埠（1621）就能用，走的是普通 TCP 長連線 + ACK**，
   **不需要** 8033「雙向 hosting」模式。
2. `83H/84H` 下載的人員記錄為 **26 bytes**（開頭 2 bytes 為人員位址）；
   `87H` 回讀為 **24 bytes 且不含位址**（其餘欄位位移相同）。
3. 位址與 UID 皆為**大端順序**（與 8031 TEXT 事件中看到的 `00000000D4B81403` 相同），
   `--addr-order be` / `--uid-order be` 是正確的。
4. 本機 RTC 顯示 `2010-11-09`，**`occurred_at` 不可直接信任**，驗收前需先校時（23H 或網頁）。

## GCP JSON Protocol（摘要）

每筆事件：

```json
{
  "schema_version": "v1",
  "event_id": "3f0b6a1e-9c42-4d5e-8b01-2a1c3d4e5f60",
  "message_type": "punch_event",
  "occurred_at": "2021-05-12T13:38:54+08:00",
  "received_at": "2026-09-19T10:12:00+08:00",
  "device":     { "maker": "SOYAL", "model": "AR837EF", "node_id": 1, "ip": "...", "source_sub_code": 17 },
  "event":      { "function_code": 11, "event_code": "M11", "description": "Normal Access", "door_no": 0 },
  "card":       { "uid_hex": "00000000D4B81403", "uid_decimal": 356701004291, "card_number_hi": 54456, "card_number_lo": 5123 },
  "person":     { "alias": "Sammi", "user_id": null },
  "punch":      { "punch_type": "check_in", "duty_code": null, "duty_label": null },
  "ingested_by":{ "receiver_id": "punch-clock-01" },
  "raw_message": "21'05/12 13:38:54 [001.17:0B](0)00000000D4B81403 rSammi (M11)Normal Access"
}
```

HTTP 傳輸固定包一層：`POST {endpoint_url}`，body `{"events": [ ... ]}`。完整欄位規範與錯誤處理見 [PRD.md §5](PRD.md)。

## 可靠性

- 送達失敗：指數退避重試（預設最多 5 次）→ 仍失敗寫入 `spool/*.jsonl`。
- 啟動時最佳努力重送 spool 內未送事件。
- 冪等：以 `event_id` 去重，重送不會重複入帳。

## 限制（v1）

- 僅支援 8031 **TEXT 單向**模式；8033 HEX 雙向（需 ACK）未實作（見 PRD §7）。
- 上/下班分類 `punch_type` 為時間窗啟發式（`classify.windows`），最終判定請於 GCP 側以業務規則覆核。