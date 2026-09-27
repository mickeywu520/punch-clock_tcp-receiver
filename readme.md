# punch-clock_tcp-receiver

Rust 實作的 **SOYAL AR837EF 打卡機 TCP Receiver**。不透過官方 701Server，直接聆聽打卡機「Message Server」主動推播的 8031 TEXT 記錄，解析後以共用 JSON Protocol 送往 GCP。

## 文件對照（本倉庫四份 markdown 的唯一權威歸屬）

| 文件 | 角色 | 內容 | 唯一權威章節 |
|---|---|---|---|
| **readme.md（本文件）** | 入口／上手／**契約統整** | 建置、設定、機端設定、工具與實機驗證、**GCP JSON 契約**、**後台 REST API**、認證、冪等 | 全部（§7、§8 為統整重點） |
| `PRD.md` | 完整 PRD | 需求 FR-1~12、§2 設備與協定（8031 TEXT／83H/84H/2EH/87H／人員與卡片資料結構／實測）、附錄 A/B 事件碼、驗收標準 | PRD §1–4、§6–8、附錄 |
| `PRD_punch_class_integration.md` | 接收端↔後台課堂出席轉拋契約 | 轉拋原則、欄位對照（後台用途）、可靠性契約、後台驗收情境 | 整合 PRD §1–9 |
| `backend_punch_api.md` | 後台 REST API 索引 | 三個端點清單與權威章節位置 | 見 readme §7、§8 |

> **統整原則（2026-09-27）**：凡跨文件重複之內容（GcpPunchEvent 欄位定義／範例、`POST /api/v1/punch-events` 格式、回應 per-item status、冪等與重試、認證設定、校時警告）一律以 **readme §7、§8** 為唯一權威版本；另三份文件已同步瘦身、改為指向本文件，避免多版本漂移。協定細部（封包格式、事件碼表）仍以 `PRD.md` 為準。

## 1. 架構

```
AR837EF ──TCP 8031 (TEXT, 主動連出)──▶ receiver ──HTTPS JSON──▶ GCP (Cloud Run / API GW / Pub/Sub)
```

## 2. 需求

- Rust 1.70+（`cargo --version`）

## 3. Build & Run

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

### 3.1 設定（唯一權威：config 結構見 `PRD.md` §6）

| 參數 | 環境變數 | 預設 | 說明 |
|---|---|---|---|
| `listen.bind` | `PUNCH_BIND` | `0.0.0.0` | 監聽位址 |
| `listen.port` | `PUNCH_PORT` | `8031` | 監聽埠（TEXT 模式） |
| `listen.mode` | - | `text` | 目前僅支援 `text` |
| `ui.enabled` | `PUNCH_UI_ENABLED` | `true` | `0`＝關閉 UI，等同 `--headless` |
| `punch_clock.ip` / `command_port` | - | - | 卡鐘指令埠（校時／寫入人員用，實測 `192.168.1.127:1621`） |
| `gcp.endpoint_url` | `PUNCH_GCP_URL` | 無 | GCP 收件 URL（POST JSON，後台為 `/api/v1/punch-events`） |
| `gcp.bearer_token` | `PUNCH_GCP_TOKEN` | 無 | `Authorization: Bearer` |
| `gcp.api_key_value` | `PUNCH_GCP_API_KEY` | 無 | header 驗證（後台 `PUNCH_API_KEY`） |
| `gcp.api_key_header` | `PUNCH_GCP_API_KEY_HEADER` | `X-Api-Key` | header 名稱 |
| `receiver_id` | `PUNCH_RECEIVER_ID` | `punch-clock-01` | 實例識別 |
| `spool_dir` | - | `./spool` | 送達失敗暫存目錄（JSONL） |

> Token／API Key 之值勿進 git（`.env`／Secret Manager）。認證細節與 401 行為見 §7.3。

## 4. 打卡機（機端）設定

瀏覽器進入控制器 Web（`Network Setting`）：

- `Message Server IP 1st` = receiver 的 IP
- `Message Port 1st` = `8031`（單向 TEXT，不需 ACK）
- `Message Server IP 2nd` = （選填）備援
- 儲存後，每次刷卡即自動推送一筆記錄。

> **現場流程**：施工人員以 receiver 的 **iced UI** 查閱本機 IPv4（啟動時自動列舉、大字顯示 `IP : 8031`），填入後台 `Message Server IP 1st`、`Message Port 1st`＝`8031` 儲存即可。後台預設 `0.0.0.0`／`0`＝推播關閉。
>
> ⚠️ **實測更正（AR-821EFv5／4V6）**：8031 文字推播僅在**重開機／開機**時出現，刷卡當下不會推送。即時打卡改以 **25H/37H 主動拉取**補足（見 `PRD.md` §2.9）；寫入／讀取人員走另一支 TCP 指令埠（實測 `1621`），不需 8033 雙向 hosting。
>
> ⚠️ **機端 RTC 不可信**：實測本機時間為 `2010-11-09`（未校時）。`occurred_at` 正確性取決於校時（網路頁或 `23H`）；未校時則課堂對應／遲到判定全錯（§7.2 `occurred_at`、§8）。

## 5. 驗證（不接真實打卡機）

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

## 6. 與真實打卡機連線測試（`tools/`）

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

詳細逐封包紀錄與掃描／整表清除／Free Access 實測見 `PRD.md` §2.7、§2.9。

**重要結論（校正先前假設）**

1. 寫入型指令（`83H/84H/85H/87H/2EH`）**在一般 TCP 指令埠（1621）就能用，走的是普通 TCP 長連線 + ACK**，
   **不需要** 8033「雙向 hosting」模式。
2. `83H/84H` 下載的人員記錄為 **26 bytes**（開頭 2 bytes 為人員位址）；
   `87H` 回讀為 **24 bytes 且不含位址**（其餘欄位位移相同）。
3. 位址與 UID 皆為**大端順序**（與 8031 TEXT 事件中看到的 `00000000D4B81403` 相同），
   `--addr-order be` / `--uid-order be` 是正確的。
4. `87H` 多筆讀取上限 **10**（`nums>10` 回應截斷）；`85H` 整表清除（`0~16383`）耗時 >3s、可冪等重送（掃描工具已自動降級）。
5. 姓名以 `2EH` 寫入（User Alias，Big5、16 bytes），與卡片（`84H`）分開寫；離線回讀姓名工具未實作，以後台顯示為準。
6. 本機 RTC 顯示 `2010-11-09`，**`occurred_at` 不可直接信任**，驗收前需先校時（23H 或網頁）。

---

## 7. GCP JSON Protocol（完整契約；唯一權威）

> 本節統整原散落於 `PRD.md` §5、`PRD_punch_class_integration.md` §3/§10、`backend_punch_api.md` 之
> `GcpPunchEvent` 欄位定義、傳輸／認證、回應格式、冪等／重試契約。Schema 版本規則：`gcp.punch.event.v1`，
> 任何新增欄位採向後相容（optional），不可刪除既有欄位。

### 7.1 單筆事件物件 `GcpPunchEvent`

單筆事件封包在 HTTP 固定包一層：`POST {endpoint_url}`，body `{"events": [ … ]}`。實機樣本（2026-09-25，card `FD6374F6`）：

```json
{
  "events": [
    {
      "schema_version": "v1",
      "event_id": "e92e2a62-5cfd-4ead-a60d-8168c1f867b5",
      "message_type": "punch_event",
      "occurred_at": "2026-09-25T22:30:23+08:00",
      "received_at": "2026-09-25T14:30:25.559073+00:00",
      "device": { "maker": "SOYAL", "model": "AR837EF", "node_id": 0, "ip": "192.168.1.127", "source_sub_code": 17, "port_number": 17 },
      "event":  { "function_code": 11, "event_code": "M11", "description": "Normal Access", "door_no": 1 },
      "card":   { "uid_hex": "00000000FD6374F6", "uid_decimal": 4251153654, "card_number_hi": 64867, "card_number_lo": 29942, "site_code": 64867, "card_code": 29942 },
      "person": { "alias": "", "user_id": null },
      "punch":  { "punch_type": "check_out", "duty_code": null, "duty_label": null },
      "ingested_by": { "receiver_id": "punch-clock-01" },
      "raw_message": "26'09/25 22:30:23 [000.17:0B](1)00000000FD6374F6 (M11)Normal Access"
    }
  ]
}
```

系統事件（無卡，例：M24 開機）時 `card` 全空，`port_number`/`site_code`/`card_code` 為 `null`：

```json
{
  "events": [
    {
      "schema_version": "v1",
      "event_id": "b32f8246-0ac1-4870-9e3b-f26ae18ad378",
      "message_type": "punch_event",
      "occurred_at": "2026-09-25T21:59:31+08:00",
      "received_at": "2026-09-25T13:59:51.462279+00:00",
      "device": { "maker": "SOYAL", "model": "AR837EF", "node_id": 1, "ip": "192.168.1.127", "source_sub_code": 17, "port_number": null },
      "event":  { "function_code": 24, "event_code": "M24", "description": "Controller Power On", "door_no": 0 },
      "card":   { "uid_hex": "", "uid_decimal": null, "card_number_hi": null, "card_number_lo": null, "site_code": null, "card_code": null },
      "person": { "alias": "", "user_id": null },
      "punch":  { "punch_type": "unknown", "duty_code": null, "duty_label": null },
      "ingested_by": { "receiver_id": "punch-clock-01" },
      "raw_message": "26'09/25 21:59:31 [001.17:18](0) (M24)Controller Power On"
    }
  ]
}
```

> `occurred_at` **必帶時區**（`+08:00`），來源為機端 RTC（**需校時**）；`received_at` 為接收端時鐘（RFC3339，可含小數秒，如實機樣本為 UTC）。

### 7.2 欄位定義

| 欄位 | 型別 | 必填 | 說明 |
|---|---|---|---|
| `schema_version` | string | 是 | 固定 `v1` |
| `event_id` | string (uuid) | 是 | 接收端產生之唯一 ID（冪等識別，重試／spool 沿用） |
| `message_type` | string | 是 | 固定 `punch_event` |
| `occurred_at` | string ISO8601 | 是 | 機端刷卡時間＋設備時區偏移（**帶 `+08:00`**；機端 RTC 需校時） |
| `received_at` | string ISO8601 | 是 | 接收端時間 |
| `device.maker` | string | 是 | 固定 `SOYAL` |
| `device.model` | string | 是 | 設備型號，由設定提供 |
| `device.node_id` | int | 是 | 機端 Node ID（`[001...]`） |
| `device.ip` | string | 是 | 連線來源 IP |
| `device.source_sub_code` | int | 是 | TEXT `[ ]` 中間欄＝**Port Number**（`881E §4.1 Data 8`：17 主埠、18 WG1、19 WG2、1~16 多門子機）。欄位名沿用 v1 實作，語意化別名見下行，**不刪除** |
| `device.port_number` | int\|null | 否 | 與 `device.source_sub_code` 同值之語意化別名。**v1.6 已產生**（無值時 `null`） |
| `event.function_code` | int | 是 | 十進位事件碼（M 碼數值） |
| `event.event_code` | string | 是 | `M{code}` |
| `event.description` | string | 是 | 事件描述（機端提供者優先，缺省用內建對照表） |
| `event.door_no` | int\|null | 否 | `( )` 內之門號 |
| `card.uid_hex` | string | 是 | 8 bytes Tag UID HEX（大端呈現 16 碼）；bit31~16 = **Site Code**、bit15~0 = **Card Code** |
| `card.uid_decimal` | int\|null | 否 | HEX 之 u64 十進位 |
| `card.card_number_hi` | int\|null | 否 | Tag UID bit31~16（＝Site Code，十進位） |
| `card.card_number_lo` | int\|null | 否 | Tag UID bit15~0（＝Card Code，十進位） |
| `card.site_code` | int\|null | 否 | `card_number_hi` 之語意化別名。**v1.6 已產生** |
| `card.card_code` | int\|null | 否 | `card_number_lo` 之語意化別名。**v1.6 已產生** |
| `card.user_address` | int\|null | 否 | 機端人員索引（`881E §4.1 Data 9/10`；無效卡片事件時為 Tag ID bit15~08/07~00）。**TEXT 模式一律 `null`**（需 8033 HEX 或 `87H` 反查） |
| `card.user_level` | int\|null | 否 | 使用者等級（`Data 14`）。**TEXT 模式一律 `null`** |
| `person.alias` | string\|null | 否 | 用戶別名（機端顯示前綴如 `r` 未正規化；後台以卡號為準） |
| `person.user_id` | string\|int\|null | 否 | 雲端映射之學生學號（由 GCP 側填補） |
| `punch.punch_type` | string | 是 | `check_in`\|`check_out`\|`unknown`（時間窗分類，**選用**；後台課堂比對只認 `occurred_at + card`） |
| `punch.duty_code` | int\|null | 否 | Duty code（Sub Code bit7~5，0~7）。**v1（8031 TEXT）一律 `null`** |
| `punch.duty_label` | string\|null | 否 | Duty 文字（On Duty/Off Duty…）。**同上一律 `null`** |
| `ingested_by.receiver_id` | string | 是 | 接收端實例名稱 |
| `raw_message` | string | 是 | 原始接收行（保留稽核） |

> 後台解析必須**容忍缺欄位**（上表必填外皆可缺，不視為錯誤）。後端也可接受「僅 `event_id`、`occurred_at`、`card`」三個必填；`card` 至少一種：`card_number_hi+lo`（優先）、或 `uid_hex`、或 `uid_decimal`。

### 7.3 傳輸與認證

| 項目 | 值 |
|---|---|
| Method | `POST`，Path 由 `gcp.endpoint_url` 指定（後台＝`/api/v1/punch-events`） |
| `Content-Type` | `application/json` |
| 認證 | `X-Api-Key: <PUNCH_API_KEY>`（後台 `verify_punch_api_key` 對 `settings.PUNCH_API_KEY`）；或 `Authorization: Bearer <token>`（選用） |
| Body | `{"events": [GcpPunchEvent, ...]}` — 單筆或批次皆可（`batch_enabled` 時每 `batch_max_items` 或 `batch_flush_interval_secs` flush；預設關閉＝逐筆即送） |
| 401 | `X-Api-Key` 錯或未設 `PUNCH_API_KEY` → 後台回 401；正式機 `.env` 必須設置 |

### 7.4 回應格式（後台一律 HTTP 200 + per-item status）

```json
{
  "received": 2,
  "stored": 1,
  "results": [
    { "event_id": "e92e2a62-…", "status": "ok", "student_id": 12, "student_name": "王小明" },
    { "event_id": "9a7178a1-…", "status": "unknown_card", "student_id": null, "student_name": null }
  ]
}
```

`status` 共五種：

| status | 意義 |
|---|---|
| `ok` | 已存入（含更新首末筆） |
| `duplicate` | `event_id` 重送，冪等跳過、不重複計 |
| `unknown_card` | 對不到卡號，不存檔 |
| `inactive` | 非在籍，不更新 daily |
| `error` | 卡號完全無法解析 |

> 後台端點**永不回 4xx（除 429）**——若回 4xx，接收端視為永久失敗不重試、不撤 spool，需人工介入。

### 7.5 錯誤處理與冪等

- **冪等**：以 `event_id`（UUID v4）為唯一鍵；重試／spool 重送沿用同一 id → 後台回 `duplicate` 不重複入帳計次。
- **重試**：5xx／429／timeout → 指數退避（`retry_attempts`，預設 5）。
- **永久失敗**：4xx（非 429）不重試（回原 retry 錯誤）。
- **最終失敗**：落 `spool/*.jsonl`（含原 `event_id`），啟動時最佳努力 replay。
- **未設 `gcp.endpoint_url`**：只落 `spool/` 不送出。
- **批次**：`batch_enabled=true` 時，單筆 buffer 達 `batch_max_items` 或間隔 `batch_flush_interval_secs` flush；後台單筆／批次皆收。

---

## 8. 後台 REST API 一覽（backend 端點）

> 唯一權威：本節（統整自 `backend_punch_api.md`）。`POST /api/v1/punch-events` 的請求／回應格式與 §7 相同（後台端點回 200 + per-item status，見 §7.4）。

### 8.1 `POST /api/v1/punch-events`（中介軟體 → 後台）

- 請求：`X-Api-Key` 認證；Body `{"events": [...]}`（§7.1 格式可直接送，不用改）。
- 回應：`200 + results[]`（§7.4）。
- 必填：`event_id`、`occurred_at`、`card`；其他缺失後端 tolerant 接受。

### 8.2 `GET /api/v1/attendance/daily?punch_date=2026-09-19`（後台／老師端，JWT）

`Authorization: Bearer <JWT>`。回應 200：

```json
[
  {
    "student_id": 12,
    "student_name": "王小明",
    "card_number": "64867:29942",
    "punch_date": "2026-09-19",
    "first_punch": "2026-09-19T08:00:00+08:00",
    "last_punch": "2026-09-19T16:30:00+08:00",
    "punch_count": 2
  }
]
```

### 8.3 `GET /api/v1/student/attendance?date_from=2026-09-01&date_to=2026-09-19`（學生端，JWT）

`Authorization: Bearer <JWT>`。回應格式同上；多筆、日期倒序，最多 62 筆。

> 兩點提醒：`X-Api-Key` 錯或沒設 `PUNCH_API_KEY` 會回 401（§7.3）；`occurred_at` 一定要帶時區（`+08:00`），receiver 預設即有，沿用即可。

---

## 9. 可靠性（摘要）

- 送達失敗：指數退避重試 → spool 落盤 → 啟動補送；冪等去重、批次模式。
- 詳細契約（重試／spool／冪等／批次／回應 status）見 §7.4、§7.5。

## 10. 限制（v1）

- 僅支援 8031 **TEXT 單向**模式；8033 HEX 雙向（需 ACK）未實作（見 `PRD.md` §7）。
- 上/下班分類 `punch_type` 為時間窗啟發式（`classify.windows`），最終判定請於 GCP 側以業務規則覆核（`PRD.md` 附錄 B：TEXT 模式無 Duty code）。
- 8031 TEXT 取不到之欄位（`user_address`／`user_level`／`duty_code`／`duty_label`）一律 `null`（§7.2）。
- 人員寫入／讀取（`tools/punch_admin.py` 與 GUI 人員管理）直接操作卡鐘指令埠；人員／白名單同步之完整前提見 `PRD.md` §7。