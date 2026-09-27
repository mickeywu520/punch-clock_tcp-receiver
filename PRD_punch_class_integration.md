# PRD — 打卡機課堂出席整合：接收端（中介程式）轉拋契約

- 版本：v0.3（草稿，供討論）
- 日期：2026-09-25
- 對應文件：
  - **本文件**＝接收端（`punch-clock_tcp-receiver`，Rust）側契約。
  - **後台版**＝`cramSchool_angular/backend/PRD_punch_class_integration.md`，負責「刷卡→課堂對應→出席判定」。
  - 本專案總 PRD＝`PRD.md`（v1.6，接收端整體規格與 `GcpPunchEvent` 資料契約）。

| 版本 | 日期 | 修訂內容 |
|---|---|---|
| v0.1 | 2026-09-19 | 初稿：接收端轉拋契約（GCP 後台課堂出席整合之必要欄位與可靠性保證）。 |
| v0.2 | 2026-09-25 | 補註 Free Access 情境：本機已啟用 Free Access 後，刷卡事件恆為 M03 Invalid card 但含完整 UID，後台比對以卡號為準、M03 亦須納入出席判定（主 PRD §2.9）。 |
| v0.3 | 2026-09-25 | 中轉端 v1.6：送出 `card.site_code/card_code` 與 `device.port_number` 語意別名；push/pull 跨通道去重（10 分鐘視窗）防重複計出席；重連後補拉離線期間事件避免漏出席。補 POST 實際格式圖錄（§10，實機樣本）；端點路徑更正為 `/api/v1/punch-events`。 |

---

## 1. 背景與定位

補習班一天多個時段（例：下午、晚上兩堂）。後台要由刷卡事件對應「哪一堂課」並產出逐課出席（準時/遲到/缺席），
所需比對依據＝**卡號 → 學生**、**`occurred_at` → 課堂時段**。

本文件只規範**接收端（本專案中介程式）的職責**——把打卡事件**完整、一致、冪等**地轉拋到 GCP 後台，
由後台負責課堂對應與出席判定。接收端**不做、也不該做**課堂邏輯（卡籤對應關係在後台、課表在後台）。

## 2. 轉拋基本原則（本專案責任）

1. **不篩選**：未知卡、查無學生的卡、異常事件（M01 無效卡等）一律照常轉送；去留由後台決定（後台回 `unknown_card`／`inactive`）。
2. **不改判**：`punch.punch_type`（本端時間窗 classify）只是選用資訊；後台課堂比對**只認 `occurred_at + card`**，不依賴 `punch_type`。
3. **一事件一 `event_id`**：解析時產生一次，重試／spool 重送沿用同一 id → 後台可冪等去重，`punch_count` 不重複計。
4. **欄位齊全**：後台課堂出席所需的欄位全部出現（§3 對照表），含 `raw_message` 稽核原行。

> 📌 **Free Access 情境（v0.2）**：本機已以 TCP 指令埠啟用 Free Access（`20H` sub `19H`，詳主 PRD §2.9），
> 每次刷卡都會登錄事件並轉送；因卡非完全有效註冊用戶（時區禁用），事件碼恆為 **M03 Invalid card** 但**含完整卡 UID**。
> 後台課堂比對以卡號為準（§3 對照），**M03 亦須照常納入出席判定**，不可只認 M11。

## 3. 轉拋資料 → 後台課堂出席需求對照（Alignment）

| `GcpPunchEvent` 欄位 (src/model.rs) | 後台課堂出席的用途（後台版 PRD） | 本端保證 |
|---|---|---|
| `event_id` (uuid) | 冪等去重（後台 `duplicate`） | 解析時固定；重試/spool 沿用 |
| `occurred_at` (RFC3339) | 課堂匹配、遲到判定（`start+5min`）、日期分組 | 帶台北 `+08:00`；來源為機端 RTC（**需校時**，§6） |
| `received_at` (RFC3339) | 校時／稽核對照 | 接收端時鐘 |
| `card.uid_hex` / `card_number_hi/lo` | 卡號 ⇄ 學生比對（`students.card_number` → low32） | TEXT 大端 16 碼＋hi/lo 拆解，三欄一致送出 |
| `device.ip` / `device.node_id` | 多機／分校對應（後台 device registry，未來） | 來源 IP 自動填入 |
| `event.door_no` | （未來）進出分讀卡機 | 有則帶、無則 `null` |
| `event.event_code` / `description` | 異常稽核（未來選用） | TEXT `(M11)…` 解出 |
| `event.function_code` | 對照 | 十進位事件碼 |
| `person.alias` | 顯示（後台以卡號為準，alias 僅參考） | 機端別名（含顯示前綴如 `r`，未正規化） |
| `punch.punch_type` / `duty_code` / `duty_label` | **後台不使用**（課堂以時間比對） | 選用欄位 |
| `ingested_by.receiver_id` | 多實例辨識 | 設定值 |
| `raw_message` | 稽核 | 原始行保留 |

> ✅ 對照結果：**現有 `src/model.rs`（`GcpPunchEvent::from_punch`）已送出全部所需欄位，無需程式碼更動。**

## 4. 可靠性契約

| 項目 | 行為（本端 `src/forwarder.rs`） | 後台對應 |
|---|---|---|
| HTTP 回應 | 一律回 **200 + per-item status**（`ok`/`duplicate`/`unknown_card`/`inactive`/`error`） | 避免 receiver 誤判 4xx 而重送 |
| 重試 | 5xx / 429 / timeout：指數退避（`retry_attempts`，預設 5） | 後台以 `event_id` 去重，重送安全 |
| 永久失敗 | 4xx（非 429）：不重試（回原 retry 錯誤） | 後台應回 200 避免造成 spool |
| 最終失敗 | 落 `spool/`（JSONL，含原 `event_id`），啟動時 replay | 補送沿用同一 `event_id`，後台仍去重 |
| 批次 | `batch_enabled`：每 `batch_max_items` 或 `batch_flush_interval_secs` flush | 後台 `events:[]` 單筆/批次皆可收 |

> ⚠️ 後台端點應**永不回 4xx**（除 429）；若回 4xx，本端視為永久失敗不重送、不撤 spool → 需人工介入。
> `status` 五種定義與回應範例見 [readme.md §7.4](readme.md)。

## 5. 認證

- 後台：`verify_punch_api_key` 檢查 `X-Api-Key`（＝`PUNCH_API_KEY`）；亦可接受 Bearer。
- 本端設定：`gcp.endpoint_url`＝後台 `POST /api/v1/punch-events`、`api_key_header`＝`X-Api-Key`、`api_key_value`＝後台 `PUNCH_API_KEY`（勿進 git，用 `.env`／Secret Manager）、`bearer_token`＝選用。完整對照表與 401 行為見 [readme.md §3.1、§7.3](readme.md)。

## 6. 校時（出席判定正確性的先決條件）

- `occurred_at` 來源＝機端 RTC；實測機端時間為 `2010-11-09`（未校時）。**未校時則課堂對應與遲到判定全錯**。
- 校時方式：打卡機網頁 `Network Setting` 或 `23H`；校時後誤差 ≤1 秒。
- 建議後台（選用）：比對 `occurred_at` vs `received_at` 誤差超標（例 >5 分鐘）時警示未校時裝置。
- 細節與驗收項見主 `PRD.md` §8、readme §4。

## 7. 本次整合對接收端之調整清單

**結論：無需程式碼更動。** 僅部署設定確認：

1. `PUNCH_GCP_URL` → 後台 `/api/v1/punch-events`。
2. `PUNCH_GCP_API_KEY_HEADER=X-Api-Key`、`PUNCH_GCP_API_KEY` → 後台 `PUNCH_API_KEY`。
3. （視現場）`PUNCH_UI_ENABLED`、`PUNCH_RECEIVER_ID`。
4. 打卡機校時 RTC。

選用增強（非本次必須；屬本專案 `PRD.md` §7 既有規劃）：
- `card.site_code`/`card_code` 語意別名、`device.port_number` 已於 v1.6 送出（後台欄位已預留）；`card.user_address` 仍未送（需 8033 HEX 或 `87H` 反查）。

## 8. 驗收（對應後台版 PRD §7 情境）

1. 實機刷卡 → 後台收到 JSON：`card.uid_hex/hi/lo`、`occurred_at(+08:00)`、`event_code`、`device.ip` 正確。
2. 情境 A~F（兩堂課匹配／遲到／缺席+請假／未排課忽略／冪等／連堂限制）由後台驗收；本端僅確保資料不缺。
3. 中途中斷 GCP → spool 落盤 → 重啟補送；後台以 `event_id` 去重，`punch_count` 不重複。
4. 未知卡照常送出且後台回 `unknown_card`（不觸發本端重送）。
5. 校時後 `occurred_at` 與實際刷卡時間一致。

## 9. 相關文件

- `cramSchool_angular/backend/PRD_punch_class_integration.md`（後台版：課堂對應與出席判定）
- 本專案 `PRD.md` v1.6（`GcpPunchEvent` 契約、轉拋/重試/spool、UI、校時）
- 本專案 `config.example.json` / `src/forwarder.rs` / `src/model.rs`

---

## 10. 附錄：POST /api/v1/punch-events 實際請求／回應格式

> **已統整（2026-09-27）**：實際請求樣本（M11 一般刷卡、M24 開機無卡）、端點與標頭、回應 per-item `status`、
> 欄位型別（`GcpPunchEvent`）、冪等與送出速查（spool replay／批次），一律以 [readme.md §7「GCP JSON Protocol」](readme.md) 為**唯一權威版本**。
> 後台 REST API 三個端點一覽見 readme §8。

- 端點：`POST /api/v1/punch-events`，`X-Api-Key` 認證，Body `{"events": [...]}`→ readme §7.1、§7.3。
- 回應：HTTP 200 + `results[]`（`status` ∈ `ok | duplicate | unknown_card | inactive | error`）→ readme §7.4。
- 冪等：`event_id`（UUID）為唯一鍵；重試／spool 重送沿用同一 id → readme §7.5。
