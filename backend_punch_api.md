RESTful API JSON 格式
1. 打卡寫入（中介軟體 → GCP 後台）
POST /api/v1/punch-events
X-Api-Key: <PUNCH_API_KEY>
Content-Type: application/json
Request（單筆／批次都是同一個 Body，中介軟體現行 {"events": [...]} 可直接送，不用改）：
{
  "events": [
    {
      "schema_version": "v1",
      "event_id": "3f0b6a1e-9c42-4d5e-8b01-2a1c3d4e5f60",
      "message_type": "punch_event",
      "occurred_at": "2026-09-19T08:00:00+08:00",
      "received_at": "2026-09-19T08:00:01+08:00",
      "device": {
        "maker": "SOYAL",
        "model": "AR837EF",
        "node_id": 1,
        "ip": "192.168.1.28",
        "source_sub_code": 17
      },
      "event": {
        "function_code": 11,
        "event_code": "M11",
        "description": "Normal Access",
        "door_no": 0
      },
      "card": {
        "uid_hex": "00000000FD6374F6",
        "uid_decimal": 4251153654,
        "card_number_hi": 64867,
        "card_number_lo": 29942
      },
      "person": { "alias": "Sammi", "user_id": null },
      "punch": { "punch_type": "check_in", "duty_code": null, "duty_label": null },
      "ingested_by": { "receiver_id": "punch-clock-01" },
      "raw_message": "26'09/19 08:00:00 [001.17:0B](0)00000000FD6374F6 rSammi (M11)Normal Access"
    }
  ]
}
必填只有 event_id、occurred_at、card 三個；其他缺了也能收（後端 tolerant 解析）。card 至少要有一種：card_number_hi+card_number_lo（優先採用）、或 uid_hex、或 uid_decimal。
Response（200，每筆獨立狀態，不會整包失敗）：
{
  "received": 2,
  "stored": 1,
  "results": [
    { "event_id": "3f0b...", "status": "ok", "student_id": 12, "student_name": "王小明" },
    { "event_id": "9a71...", "status": "unknown_card", "student_id": null, "student_name": null }
  ]
}
status 共五種：ok（已存＋已更新首末筆）／duplicate（event_id 重送，冪等跳過）／unknown_card（對不到卡號，不存檔）／inactive（非在籍，不更新 daily）／error（卡號完全無法解析）。
2. 每日出勤查詢（後台／前台老師端，JWT）
GET /api/v1/attendance/daily?punch_date=2026-09-19
Authorization: Bearer <JWT>
Response（200）：
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
3. 學生個人出勤（學生端，JWT）
GET /api/v1/student/attendance?date_from=2026-09-01&date_to=2026-09-19
Authorization: Bearer <JWT>
Response 格式同上（多筆、日期倒序，最多 62 筆）。
兩點提醒：X-Api-Key 錯或沒設 PUNCH_API_KEY 會回 401，記得正式機 .env 要設；occurred_at 一定要帶時區（+08:00）， receiver 預設就有，沿用即可。