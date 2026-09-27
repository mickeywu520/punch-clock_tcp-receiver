# 後台 REST API（index）

> **已統整（2026-09-27）**：本檔原為後台 REST API 之 JSON 格式規格（打卡寫入、每日出勤查詢、學生個人出勤）。
> 其中 `POST /api/v1/punch-events` 之請求／回應完整格式（`GcpPunchEvent` 欄位定義、回應 per-item `status`、冪等、認證），
> 與後台三個端點之一覽，已統一移至 [readme.md §7「GCP JSON Protocol」、§8「後台 REST API 一覽」](readme.md) 為**唯一權威版本**。
> 本檔僅保留端點索引與重點提醒。

## 端點一覽

| 端點 | 用途 | 認證 | 權威章節 |
|---|---|---|---|
| `POST /api/v1/punch-events` | 打卡寫入（中介軟體 → 後台） | `X-Api-Key`（可接受 Bearer） | readme §7（完整格式）、§8.1 |
| `GET /api/v1/attendance/daily?punch_date=YYYY-MM-DD` | 每日出勤查詢（後台／老師端） | Bearer JWT | readme §8.2 |
| `GET /api/v1/student/attendance?date_from&date_to` | 學生個人出勤（學生端） | Bearer JWT | readme §8.3 |

## 重點提醒（原規格要點）

- `X-Api-Key` 錯或後台未設 `PUNCH_API_KEY` → 回 401；正式機 `.env` 必須設置（readme §7.3）。
- 後台端點一律回 **HTTP 200 + per-item `status`**（`ok` / `duplicate` / `unknown_card` / `inactive` / `error`），**永不回 4xx（除 429）**（readme §7.4）。
- 必填僅 `event_id`、`occurred_at`、`card`；`card` 至少一種：`card_number_hi+lo`（優先）、或 `uid_hex`、或 `uid_decimal`（readme §7.2、§8.1）。
- `occurred_at` 一定要帶時區（`+08:00`），receiver 預設即有，沿用即可。
- 冪等：以 `event_id` 去重，重送回 `duplicate`，`punch_count` 不重複計（readme §7.5）。