# PRD — SOYAL AR837EF 打卡機整合 (Rust TCP Receiver → GCP)

- 版本：v1.6
- 日期：2026-09-25
- 狀態：草案 / 待複核

| 版本 | 日期 | 修訂內容 |
|---|---|---|
| v1.0 | 2026-09-19 | 初稿 |
| v1.1 | 2026-09-19 | 依 `硬體Protocol及範例/` datasheet 全文複核後修正：<br>① §2.1 參考文件清單更正（原引用之 `SOYAL Active Message Sending Example-TW.pdf` 不在工作目錄），並補入 `靈活使用技巧`、`721_727H`、`401ROxDIx`；<br>② §2.3 `[ ]` 欄位語意更正（`SubCode` → **Port Number**，補 19=WG2），卡號欄位改以 **Site Code / Card Code** 說明，並新增「TEXT 模式取不到之欄位」限制；<br>③ 新增 §2.5 **人員資料結構（83H/84H）**、§2.6 **卡片資料結構**（datasheet 有、v1 未涵蓋）；<br>④ 新增 §3.2 **v1 範圍外**項目；<br>⑤ §5.2 更正 `device.source_sub_code`、卡號欄位說明，新增 optional 欄位（`site_code`/`card_code`/`user_address`/`user_level`/`port_number`）；<br>⑥ §7 新增「人員／白名單同步」與「卡片資訊擴充」規劃；<br>⑦ 附錄 A 補齊事件碼（M00~M114）與範圍說明、附錄 B 加註 TEXT 模式 Duty code 限制。 |
| v1.2 | 2026-09-19 | **實機驗證後修正**（裝置 `192.168.1.127:1621`、AR-821EFv5、體 4V6、Node ID 1）：<br>① 新增 §2.1 實測設備資訊、§2.7 實機驗證記錄（含實際 TX/RX 封包）；<br>② §2.2 更正：寫入型指令（`83H`/`84H`/`85H`/`87H`/`2EH`）**在一般 TCP 指令埠即可用**，不需 8033 雙向 hosting；<br>③ §2.5 更正人員記錄長度：`83H`/`84H` 下載 26 bytes（含位址）、`87H` 回讀 24 bytes（**不含位址**）；<br>④ §2.5 確認位址與 UID 為**大端順序**；<br>⑤ §3.2、§7 同步修正被推翻的假設；<br>⑥ §8 新增機端 RTC 校時驗收項（實測機端時間為 2010-11-09）。 |
| v1.3 | 2026-09-19 | **實機新增人員／掃描／整表清除驗證後補充**：<br>① §2.7 新增實測：`87H` 多筆讀取上限（`nums≤10` 完整、`>10` 回應截斷）、`85H` 整表清除 `0~16383` 耗時 >3s（冪等可重送）、`2EH` 姓名 Big5 實機寫入成功、`site:card` 十進位卡號建立人員流程；<br>② 新增 §2.8「後台 user list 空／有效狀態對照」；<br>③ §7 補充人員／白名單同步實作前提（位址需明示指派、建議 0 起、批次上限 10、姓名另存於 2EH、中文別名編碼待驗）與 §7.1 **GCP 串接注意事項**；<br>④ 工具支援：`punch_admin.py --uid site:card`、`scan-users` 批次截斷自動降級（selftest 43 項）。 |
| v1.4 | 2026-09-19 | **中介程式 + iced UI（顯示本機 IP／狀態面板）新增**：<br>① §2.2 補「現場設定流程」（後台 `Message Server IP 1st` 預設 `0.0.0.0`、`Port 1st` 預設 `0`＝關閉；填上 receiver 的 IP + 8031 儲存後，**每次刷卡即自動推送**，不需其他設定）；<br>② §3 新增 FR-10（iced UI 大字顯示本機 IPv4，供施工人員照抄填入後台）、FR-11（UI 狀態面板：監聽／連線裝置／最近事件／GCP 上送）、FR-12（`--headless` 無 GUI 環境可用），§3.1 補部署彈性；<br>③ §4 系統流程補 UI 顯示分支；<br>④ §6 設定新增 `ui.enabled`（`PUNCH_UI_ENABLED`）；<br>⑤ §8 驗收補 GUI 施工流程項。 |
| v1.5 | 2026-09-25 | **Free Access 通行模式與事件登錄實測補充**：<br>① 新增加 §2.9——卡片在 User List 但通行時區禁用時，刷卡**不建立事件**；以 TCP 指令埠 `20H`（Set）/`12H`（Get）sub-code `19H` 啟用並確認 Free Access；啟用後每次刷卡記錄為 **M03 Invalid card 但含完整卡 UID**；<br>② 記錄事件佇列**單一來源**（後台 Event Log／µA `25H`／8031 推播同一佇列），與 8031 推播僅在重開機／開機時出現之實測（§2.2「每次刷卡即自動推送」於此機不符）→ 即時打卡改採 **25H/37H 主動拉取**之依據；<br>③ 修正 Access Mode 的 Get 指令為 **`12H`（控制器參數），非 `1BH`**。 |
| v1.6 | 2026-09-25 | **中轉程式完善（去重／語意化別名／事件碼修正／重連 forward 修正）**：<br>① 新增 **push/pull 跨通道去重**（`src/dedup.rs`，以 `occurred_at+UID+event_code` 為鍵、10 分鐘滑動視窗）——8031 推播與 25H/37H 拉取源自同一事件佇列（§2.9），重開機時兩通道可能各轉送同一筆而生不同 `event_id`，改由中轉端去重；<br>② 送出 §5.2 預留之語意化別名：`device.port_number`（＝`source_sub_code`）、`card.site_code`（＝`card_number_hi`）、`card.card_code`（＝`card_number_lo`）——後台版 PRD §7 選用增強交付；<br>③ **修正 `src/function_codes.rs` 30~34 位移錯誤**（附錄 A 已知落差）：30=Anti-pass back、31/32=副讀卡機離線/連線、33/34=用戶修改密碼/失敗；<br>④ 修正重連邏輯：非首次連線一律 forward 離線期間累積事件（原先 `first_session && forward_initial` 使**每次重連都靜默 37H 刪除事件**，與註解「later sessions forward」相反，造成後台 Event Log 有 M11 但 receiver 收不到）；25H 讀取錯誤改為連線層錯誤即拆 session 重連、僅 timeout 保留 session。 |

---

## 1. 背景與目標

校園／機構目前使用 SOYAL AR837EF 網路型門禁／考勤控制器作為學生打卡機。需求是不透過 SOYAL 官方 701Server / 701Client 軟體，直接從打卡機接收即時打卡記錄，解析後送往 GCP（Google Cloud）雲端作考勤資料彙整。

SOYAL 網路型控制器本身提供 **Message Server** 主動推播功能：在後台 `Network Setting` 設定 `Message Server IP` / `Message Port` 後，控制器會主動以 TCP **Client** 的角色連線到我們指定的接收端，並即時把每一次刷卡／通行事件推送到該端口。

本專案即為這台「接收端」：

```
┌─────────────┐   TCP connect (outbound)   ┌──────────────────┐   HTTPS + JSON   ┌─────────────┐
│ AR837EF     │ ─────────────────────────▶ │  punch-clock_tcp- │ ───────────────▶ │  GCP        │
│ (打卡機)     │  Message Server IP=receiver │  receiver (Rust)  │  依共用 JSON       │  (Cloud Run │
│ 8031 TEXT   │    Port 8031 (單向、不需ACK) │  解析 → 正規化      │  Protocol        │   / API GW /│
└─────────────┘                            └──────────────────┘                  │   Pub/Sub)  │
                                                                                   └─────────────┘
```

本文件同時定義「**GCP 共用 JSON Protocol**」，作為打卡機接收端與雲端之間的資料契約（Schema Contract）。雲端團隊依據此 Protocol 開出 API／資料表，雙方可平行開發。

**交付形式 = 中介程式（Rust，含 `iced` 桌面 UI）**：在施工現場的 Windows／Linux 主機執行，啟動即自動顯示本機 IPv4 與監聽埠，讓施工人員不查 `ipconfig` 就能把 IP 填進打卡機後台 `Message Server IP`；同時作為收案端接收推播、轉送 GCP（細節見 §3 FR-10~FR-12）。

---

## 2. 設備與協定（SOYAL 側）

### 2.1 支援設備
- SOYAL 網路型企業版 E 系列控制器：**AR-837EF**（也涵蓋 AR-837E / AR-837EA / AR-837EL / AR-716-E16 多門控制器）。
- **實測設備（2026-09-19）**：`192.168.1.127`，型號 **AR-821EFv5**（ACK 回報 Controller Type `0xC3`），韌體 **4V6**，Node ID **1**，TCP 指令埠 **1621**（網頁 `Network Setting` 可自訂；同機另有 Port 80 為 Web 介面）。
- 參考文件（皆位於 `硬體Protocol及範例/`）：

| 檔案 | 用途 |
|---|---|
| `Protocol_881E_725Ev2_82xEv5 4V05.pdf` | **主力依據**：指令集、§2.19 Set User Parameters（83H/84H）、§4.1 事件記錄欄位（Data 0~28）、§4.2 事件碼表 |
| `Message File structure.pdf` | 701ServerSQL `.msg` 100 bytes 欄位對照、Controller Function code define |
| `SOYAL Protocol 靈活使用技巧_v221116-Final.pdf` | `83H/84H/85H/87H` 中文實作與 H / E 系列白名單下載差異（見 §2.5） |
| `721_727H Protocol_EN.pdf` | 舊 H 系列（AR-721H / AR-721HV3 / AR-727HV3）對照；§3.1 Data Structure of Cards（Site Code / Card Code，見 §2.6） |
| `401ROxDIx protocol EN.pdf` | AR-401RO16 / 401DI16 / 401RO8DI16 等 I/O 模組，**與本案無關**（僅註記） |

- ⚠️ 原 v1.0 引用之 `SOYAL Active Message Sending Example-TW.pdf` **不在工作目錄**（尚未取得），§2.2 之 8031 / 8033「Message Server」敘述待補官方出處後補登連結。

### 2.2 Web 後台設定（機端）
| 項目 | 建議值 | 說明 |
|---|---|---|
| `Message Server IP 1st` | TCP receiver 的 IP（`0.0.0.0`＝全部／無指向，不使用時填 `0.0.0.0`） | 主動推播記錄到第一台 Server |
| `Message Port 1st` | **8031** | 單向 (one-way hosting)，**TEXT 模式**，不需 ACK（本專案 v1 採用）；**`0`＝關閉，`1024~65530` 有效區間，`8031`＝Text Mode** |
| `Message Server IP 2nd` | (選填) 備援 Server | 備援用途 |
| `Message Port 2nd` | 8033 | 雙向 (two-way hosting)，HEX 模式，需交握 ACK（**v1 不支援，見 §7 後續規劃**） |

> 若無整合需求，官方建議 Message Server IP 填 `0.0.0.0`、Port 填 `0` 關閉。我們會改成 receiver 的 IP。

> **現場設定流程（v1.4）**：施工人員於打卡機後台 `Network Setting`，把 `Message Server IP 1st` 填上 **receiver UI 顯示的本機 IP**、`Message Port 1st` 填 `8031`（TEXT）後存檔，此後**每次刷卡即自動推送**到 receiver，不需其他設定。後台預設顯示 `0.0.0.0`／`0` 即為「推播關閉」。receiver 的 **iced UI 會在啟動時列舉本機 IPv4**，大字顯示 `IP : 8031` 供照抄，避免施工人員手查 `ipconfig`。

> ⚠️ **實測更正（v1.5，本機 AR-821EFv5／4V6）**：上述「每次刷卡即自動推送」於此機**實測不符**——8031 文字推播僅在**重開機／開機**時觀察到（後台按 update 觸發 reboot 才送出），刷卡當下不會推送。事件佇列與後台 Event Log 同一來源（見 §2.9），即時打卡需以 **25H/37H 主動拉取**補足。

> **重要（實測更正 v1.2）**：上表是「**機器主動推播事件**」的設定（one-way hosting）。
> 但**讀寫人員／卡片等指令型操作走的是另一個埠**：控制器的 **TCP 指令埠**（實測 `1621`），
> 以「一般 TCP 連線 + `7E` 短封包 / `FF 00 5A A5` 長封包 + ACK」溝通，
> **不需要** 8033 雙向 hosting、也**不需要**交握（見 §2.7）。
> 兩者用途不同：`8031` = 被動收事件；`1621` = 主動下指令。

### 2.3 8031 TEXT 模式資料格式（已確認）

單筆記錄為一行 ASCII 文字，以 `\n`（0x0A）結尾，官方範例原始 HEX：

```
32 31 27 30 35 2F 31 32 20 31 33 3A 33 38 3A 35 34 20 5B 30 30 31 2E 31 37 3A 30 42 5D
28 30 29 30 30 30 30 30 30 30 30 44 34 42 38 31 34 30 33 20 72 53 61 6D 6D 69 20 20 20
20 20 20 20 20 20 20 20 20 20 28 4D 31 31 29 4E 6F 72 6D 61 6C 20 41 63 63 65 73 73 0A
```

解碼為文字：

```
21'05/12 13:38:54 [001.17:0B](0)00000000D4B81403 rSammi                (M11)Normal Access\n
```

**欄位對照表（官方文件 Page 7）：**

| 區段 | 範例值 | 說明 |
|---|---|---|
| 日期 | `21'05/12` | `YY'MM/DD`，年份表示 2000+YY |
| 時間 | `13:38:54` | `HH:MM:SS` |
| 節點資訊 | `[001.17:0B]` | `[NodeID.PortNumber:FuncCode]`：Node ID=001；中間值為 **Port Number**（`Protocol_881E...4V05.pdf` §4.1 Data 8）= **17 主埠 Main port、18 WG1、19 WG2**，1~16 為多門控制器下之 RS485 子機。⚠️ **此值不是** datasheet 的 `Sub Code`（= Data 11，含 Duty code bit7~5，見附錄 B）；`FuncCode` 為 16 進位事件碼，`0B`=M11 |
| 門號/端口 | `(0)` | 事件發生之門／讀卡機號碼（對應 Data 17 `Door Num.`；E 系列多門時會是非 0 值） |
| 卡號 UID | `00000000D4B81403` | 16 位 HEX = 8 bytes Tag UID（**大端呈現**）。其中 bit31~16 = **Site Code**、bit15~0 = **Card Code**（`721_727H Protocol_EN.pdf` §3.1 / §2.19）。低 4 bytes `D4B81403` → Site=`D4B8`(54456)：Card=`1403`(5123)。⚠️ v1.0 寫「Wiegand 格式 Hi 2 bytes : Lo 2 bytes」易誤解（Wiegand 26 為 Site 8 bits + Card 16 bits），已更正 |
| 用戶別名 | `rSammi` | 別名文字（前方 `r` 為機端顯示前綴，不保證固定；無別名則空白） |
| 事件碼＋描述 | `(M11)Normal Access` | `(M{十進位碼}){描述文字}`，描述文字依機端韌體語言而定（繁中/英文） |

> 備註：每筆記錄之間僅以換行分隔；接收端必須支援**單一 TCP 連線內多筆連續送達**與**斷線重連**。

> ⚠️ **TEXT 模式資訊限制（重要）**：8031 TEXT 僅有上表 7 段，下列 datasheet 欄位**在 TEXT 模式下無法取得**，v1 一律留空 / `null`：
> - 機端 `User Address`（人員索引，Data 9 / 10）
> - `User Level`（Data 14）
> - `PIN / User inputted code`（Data 25~28）
> - `Sub Code`（Data 11 → 含 Duty code bit7~5）
> - 卡片 `Access Mode` / 到期日（Year / Month / Day）/ 可用門組（Group 1、2）
> - 8 bytes UID 之 bit32~63（Data 21~24，與 SOR 扣款／餘額共用）
>
> `Site Code` / `Card Code` 可由 UID 推導（見上表），但機端不獨立送出。若需完整人員／卡片資訊，須改用 8033 HEX 雙向模式或另以 `87H` / `2EH` 反查（見 §2.5、§2.6、§7）。

### 2.4 事件碼表（FuncCode ↔ M 碼 ↔ 描述）

解析後會產出正規化 `function_code`（十進位）與 `event_code`（`M{code}`）。對照表請見附錄 A。

- 出處：`Protocol_881E_725Ev2_82xEv5 4V05.pdf` §4.2（PDF p.106–110）與 `Message File structure.pdf` p.3~5。
- 範圍：datasheet 為 **M00~M51+（延伸至 M114 / M200 系列）**，附錄 A 為節錄；v1 內建對照表（`src/function_codes.rs`）涵蓋其中一部分，未涵蓋者以機端回傳之描述文字為準，若機端亦未提供則記為 `Unknown`。
- 描述文字會因機端韌體語言而不同（繁中 / 英文），**不可用描述文字做程式判斷**；請一律以 `function_code` / `event_code` 判斷。

---

### 2.5 人員資料結構（Set User Parameters，83H / 84H）— datasheet 有、v1 未實作

出處：`Protocol_881E_725Ev2_82xEv5 4V05.pdf` §2.19（PDF p.71–73）；中文實作與 H / E 系列差異見 `SOYAL Protocol 靈活使用技巧_v221116-Final.pdf` p.41–48。

| 欄位 | 長度 | 說明 |
|---|---|---|
| `User Addr` | 2 bytes（Little endian） | **機端人員索引／位址（0~16383）**，是「卡片 ↔ 人員」對應主鍵 |
| `Tag UID` | 8 bytes（Little endian） | 卡片 UID |
| `PIN Code` | 4 bytes（H 系列為 2 bytes） | 密碼。Guest 使用者時（V4.4+）bit31~16 = 當日起始分鐘、bit15~0 = 結束分鐘 |
| `Mode` | 1 byte | bit7/6 Access Mode（`00` Invalid / `01` Card Only / `10` Card or PIN / `11` Card + PIN）、bit5 巡邏卡、bit4 指紋後免卡、bit3 刷卡後免指紋、bit2 啟用到期檢查、bit1 Guest + PIN 起訖時間、bit0 可自行改密碼 |
| `Zone` | 1 byte | bit7 多門控制器獨立時區（`89H`）、bit6 保留、bit5~0 通行時區（0 = free） |
| `Group 1` / `Group 2` | 1 byte ×2 | 可用門號 bit map（Door16~9 / Door8~1）；bit=1 允許通行 |
| `Year` / `Month` / `Day` | 3 bytes | 卡片最後允許日期（**到期日**） |
| `Level` | 1 byte | bit7~6 使用者等級（0x00~0x03）、bit5~0 WG 埠 Zone 2 |
| `Option` | 1 byte | Anti-pass-back 開關（bit7）；**僅 `83H` 有效，`84H` 會忽略** |

- 每筆 **26 bytes**（`… N*26` 可連續下載多筆）；`83H` = 含 anti-pass-back 旗標、`84H` = 不含。
- 相關指令：`85H` 刪除人員（起訖位址）、`86H` 重設 APB、`87H` 讀取人員、`89H` 多門各門時區、`8AH` APB 資料庫、`8BH` 訪客通行時段（含樓層）、`2EH` 讀寫 **User Alias（16 bytes／人，即 `person.alias` 之來源）**、`2FH` 樓層、`8FH` 指紋 / 靜脈 / 人臉模板、`90H` 黑名單 UID、`12H` sub-code `15H` 可用人數。
- **實測更正（v1.2）**：
  - `83H`/`84H`（下載）記錄為 **26 bytes，開頭 2 bytes = 人員位址**；
    `87H`（回讀）回傳的是 **24 bytes 且不含位址**（其餘欄位順序相同、整體前移 2 bytes）。
    因此回讀時無法得知位址，須自行記錄查詢的位址（見 §2.7 實測封包）。
  - **位址與 Tag UID 皆為大端（High byte first）**：實測寫入位址 1000 + UID `000000008EA14AFE`，
    回讀得到 `00 00 00 00 8E A1 4A FE …`，與 8031 TEXT 事件中看到的順序一致。
  - 這些指令**在一般 TCP 指令埠就能用**（實測 1621），不必啟用 8033 雙向 hosting。

### 2.6 卡片資料結構（事件封包 ↔ 卡片欄位對照）

| 欄位 | datasheet 出處 | 說明 | v1 是否可得 |
|---|---|---|---|
| `Site Code` | `721_727H Protocol_EN.pdf` §3.1／§2.19；`881E §4.1 Data 15/16` | Tag UID bit31~16 | ✔ 可由 UID 推導 |
| `Card Code` | 同上 | Tag UID bit15~0 | ✔ 可由 UID 推導 |
| `User Address` | `881E §4.1 Data 9/10`；`Message File structure.pdf` field 11/12 | 機端人員索引；**無效卡片事件時改為 Tag ID bit15~08 / bit07~00** | ✘ |
| `User Level` | `881E §4.1 Data 14` | bit5~0 等級、bit6 free access、bit7 多門控制器 / WG 埠事件 | ✘ |
| `Port Number` | `881E §4.1 Data 8` | 17 主埠 / 18 WG1 / 19 WG2 | ✔（TEXT `[ ]` 中間值） |
| `Door Num.` | `881E §4.1 Data 17` | 事件發生門號 | ✔（TEXT `( )`） |
| `PIN / User inputted code` | `881E §4.1 Data 25~28` | 僅 function code 1 / 8 / 10 / 28 / 33 有效 | ✘ |
| 8 bytes UID bit32~63 | `881E §4.1 Data 21~24` | 與 SOR 扣款額 / 餘額共用（bit47~40 / 39~32 / 63~56 / 48~55；後兩者文件順序似有筆誤，實作前須以實機驗證） | ✘ |
| `Area Code` / `Transfer value` / `Tag balance` / `Confirm` | `Message File structure.pdf` field 92 / 23~26 / 99 | 若日後對齊 701ServerSQL `.msg` 格式會用到 | ✘ |

> v1 僅使用可由 TEXT 取得者。若要擴充卡片資訊，須走 §7 規劃（8033 HEX 或 `87H` 反查）。

### 2.7 實機驗證記錄（2026-09-19）

裝置：`192.168.1.127:1621`（TCP 指令埠），AR-821EFv5，韌體 4V6，Node ID `1`。
工具：`tools/punch_admin.py`（可重現，見 readme）。長封包（`FF 00 5A A5`）亦實測可用，以下為短封包。

| # | 指令 | TX | RX（實機） | 結論 |
|---|---|---|---|---|
| 1 | 18H 輪詢 | `7E 04 01 18 E6 FF` | `7E 0A 00 09 01 00 01 00 10 40 A6 01` | 通訊正常；echo code `0x09` |
| 2 | 24H 讀時間/版本 | `7E 04 01 24 DA FF` | `7E 24 00 03 01 1A 0C 00 03 09 0B 0A 46 01 02 00 C3 00 04 01 80×16 63 BF` | 體 **4V6**（`0x46`）、Controller Type `0xC3`、**機端時間 2010-11-09 00:12:26（週二）** |
| 3 | 25H 讀事件記錄 | `7E 04 01 25 DB 01` | `7E 21 00 18 01 32 22 0E 05 15 0A 0A 11 00 00 00 00 10 00 00 00 01 00 … E8 B3` | 可讀；**echo code 為 `0x18`（非文件的 `0x03`）**；內容 2010-10-21 14:34:50、Port 17 |
| 4 | 84H 新增人員+卡片 | `7E 1F 01 84 01 03 E8 00 00 00 00 8E A1 4A FE 00 00 00 00 40 00 FF FF 4F 0C 1F 00 00 00 00 00 17 B7` | `7E 0F 00 04 01 C3 46 0F 91 10 10 00 00 00 00 E1 AF` | **ACK 成功** |
| 5 | 87H 回讀 | `7E 07 01 87 03 E8 01 93 07` | `7E 1D 00 03 01 00 00 00 00 8E A1 4A FE 00 00 00 00 40 00 FF FF 4F 0C 1F 00 00 00 00 00 7A AD` | **24 bytes**，內容與寫入一致（UID/Mode/到期日） |
| 6 | 85H 刪除 | `7E 08 01 85 03 E8 03 E8 7B D7` | `7E 0F 00 04 01 C3 46 0F 91 10 10 00 00 00 00 E1 AF` | **ACK 成功**，回讀變 `FF×20 00×4`（空白） |
| 7 | 84H+2EH 新增人員含姓名（v1.3） | `7E 1F 01 84 01 00 00 00 00 00 00 FD 63 74 F6 …` ＋ `7E 18 01 2E 00 00 00 01 C3 E4 B2 FC AB DF` (補零至16B) | 兩次 ACK `7E 0F 00 04 01 C3 46 0F 91 10 10 00 …` | 位址 0 建立成功；姓名 `邊荷律`＝Big5 6 bytes，2EH 補零至 16 bytes 亦 ACK |
| 8 | 87H 多筆 `nums=10`（v1.3） | `7E 07 01 87 00 00 0A …` | `1 + 10×24 bytes`，完整 | **`nums≤10` 完整回傳**；空格＝`FF×20 00×4`（`parse_user_record` ⇒ `empty=True`） |
| 9 | 87H 多筆 `nums=16/20/30`（v1.3） | 同上，`nums=16/20/30` | 回應**被截斷**（尾巴非 24 的倍數） | **`nums>10` 會截斷**，導致多筆掃描漏記；工具 `scan-users` 已自動降為逐筆 |
| 10 | 85H 整表清除 `0~16383`（v1.3） | `7E 08 01 85 00 00 3F FF BB 7F` | 3s timeout 未回 → `--timeout 60` 重送後 ACK | 整表清除耗時 **>3s**（單筆刪除 100ms~6s 不適用於全表）；指令冪等、重送無害；完成後後台全為 Invalid |

**由此得到的設計結論**

1. **人員／卡片同步不需要 8033**：寫入型指令走一般 TCP 指令埠（實測 1621）即可，
   連線可長時間保持、逐指令回 ACK，與 8031 的推播通道互不影響。
2. **記錄長度不對稱**：下載 26 bytes（含位址）／回讀 24 bytes（不含位址），
   實作時不可用同一個結構硬解（`tools/soyal_proto.py` 以 `layout="auto"` 依長度自動判斷）。
3. **機端 RTC 不可信**：實測機端時間為 2010-11-09（未校時）。`occurred_at` 需以校時後為準（見 §8）。
4. **`87H` 批次讀取上限 10**（v1.3）：`nums≤10` 每筆固定回 24 bytes、空格回 `FF×20 00×4`；
   `nums>10` 回應被截斷（尾巴殘缺）→ 掃描程式必須限批並偵測截斷（`scan-users` 已實作）。
5. **`85H` 整表清除耗時 >3s**（v1.3）：一次清 `0~16383` ACK 需較長時間；工具 `--timeout` 需放大、
   逾時可冪等重送。
6. **姓名（`2EH`）實機驗證通過**（v1.3）：Big5 6 bytes（`邊荷律`→`C3 E4 B2 FC AB DF`）補零至
   16 bytes 寫入成功；**目前無離線回讀姓名工具**（`2EH` read 未實作），確認以後台 user list 為準。
7. **位址無自動分配**（v1.3）：83H/84H 需在指令中明示位址；後台由位址 0 起顯示，
   建議名冊同步採「0 起連續位址」以便與後台對照（§2.8）。

> ⚠️ 未驗證項目（實作前仍需實機確認）：H 系列封包差異、事件記錄中的 Function code（M 碼）
> 確切位置（實測記錄疑似不含 M 碼欄位）、**8031 TEXT 中文別名的實際編碼（Big5？）**。
> （`2EH` 姓名、`87H` 多筆讀取、`85H` 整表清除已於 v1.3 實測，見上表。）

---

### 2.8 後台 user list 狀態對照（v1.3 新增）

後台（Web）顯示的使用者列表與協定層的對應，供對帳／除錯：

| 後台顯示 | 協定層對應 | 解讀 |
|---|---|---|
| Access Mode `Invalid` | Mode byte bit7/6 = `00` | 無有效通行方式＝該位址未設定有效使用者 |
| Card UID `65535:65535` | 卡號 = `FFFF:FFFF`（`FF×…` 空格） | **空位址**；`parse_user_record` ⇒ `empty=True` |
| Card UID `64867:29942` | Site `64867`(0xFD63) / Card `29942`(0x74F6)，UID `00000000FD6374F6` | v1.3 實測有效使用者（`--uid 64867:29942` 建立） |
| Expiry `2099-12-31` | 空位址之到期日預設（`FF` 值） | 工具以 `4F 0C 1F`＝**2079-12-31** 表「不限」 |
| Display 姓名 | `2EH` 寫入之 User Alias（Big5、16 bytes） | 離線回讀工具未實作；以後台顯示為準 |

> 對帳建議：以 `punch_admin.py scan-users` 掃描結果為**協定層事實來源**，後台顯示為輔；
> 兩者不一致時以實機指令回覆為準（例：空白位址後台仍會列出該地址列）。

---

### 2.9 通行模式 Free Access 與刷卡事件登錄（v1.5 實測，2026-09-25）

> 背景情境：卡片已建入後台 User List（§2.8），但刷卡在後台 **Event Log**、`25H` 佇列、8031 推播上**完全沒有事件**。

**根因**：卡片「存在 User List」⇄「通行時區是否允許」是兩件事。該員卡的通行時區在此機為禁用，
刷卡當下被通行檢查擋下、**連事件記錄都不建立**——不是收不到，是事件佇列裡根本沒有那筆。
（啟用 Free Access 之前，凡時區不合格的卡刷卡皆不會留下記錄。）

**解法（TCP 指令埠 1621 直接下達，不需動後台 web）**：啟用 Free Access（主機免費進出）。

| 步驟 | 指令 | 結果（實機） |
|---|---|---|
| 啟用 | `20H` Set Controller Access Mode，sub-code `19H`，data `19 01 00 00 00 08`（後五 byte：`01`=**freeMain**、`00`=free WG1、`00`=black UID、`00`=容量、`08`=Access Mode） | ACK `7E 0F 00 04 01 C3 46 0F 91 10 10 00 00 00 00 E1 AF` |
| 回讀確認 | **`12H`**（⚠️ 是 `12H`，不是 `1BH`）Get Controller parameters，sub-code `19H` | echo `7E 0A 00 03 01 01 00 00 00 08 F4 01` → freeMain byte = `01`（已啟用） |

**啟用後的行為**：每一次刷卡都會建立事件記錄進入事件佇列（後台 Event Log 可見）。
因該卡並非完全有效的註冊用戶（時區仍禁用），記錄為 **M03 Invalid card**，但**含完整卡 UID**。
實測 `25H` 記錄（2026-09-25 19:54:06）：

```
7E 21 00 03 01 06 36 13 06 19 09 1A 11 74 F6 00 00 10 40 FD 63 01 00 74 F6 00 00 00 00 00 00 00 00 0C 37
   └node└func └src └sec└min└hr └wd └day└mon └yy └port
```

- func `0x03`＝M03；sec/min/hour/weekday/day/month/year = `06 36 13 06 19 09 1A`（**raw decimal**）→ 19:54:06;
  port `0x11`＝17；UID = `Data21/Data15/Data16/Data19/Data20` = `00 FD 63 74 F6` → **`00000000FD6374F6`**
  （Site `64867` / Card `29942`）。

**接收端設計含義（後續修改的依據）**

1. **判斷「是否為考勤打卡」以「事件是否帶有效 Tag UID」為準，而不是事件碼**——Free Access 下刷卡恆為 M03，
   若以事件碼（如只認 M11）篩選會全數漏掉；反之不含 UID 的系統事件（例 M24 Power On）才應略過。
2. **事件佇列為單一來源**：後台 Event Log、µA `25H` 讀取、8031 文字推播三處讀到的是**同一個佇列**。
3. **8031 推播此機並不可靠**：實測推播僅在**重開機／開機**（後台按 update 觸發 reboot）時出現，刷卡當下不會推送（見 §2.2 更正）。
   即時打卡須靠 **25H/37H 主動拉取**：25H 讀一筆 → 37H 刪除 → 重複至佇列空。
4. **Access Mode 的 SET/GET 指令**：`20H`＝Set、`12H`＝Get，sub-code 皆 `19H`（Get 用 **`12H`**，非 `1BH`）。

---

## 3. 功能需求（Rust TCP Receiver）

| # | 需求 | 驗收方式 |
|---|---|---|
| FR-1 | 以 **TCP Server** 監聽 8031（或自訂埠），接受 AR837EF 主動連線並隱式送達多筆 TEXT 記錄。作為「中介程式」執行時可視情況額外開啟 **iced 桌面 UI**（可用 `--headless` 關閉，見 FR-12）。 | `nc -l` 模擬連線可收到 raw 行；無 `--headless` 時啟動可見 UI 視窗 |
| FR-2 | 解析 8031 TEXT 記錄為正規化結構（§2.3 欄位）。 | 單元測試覆蓋官方範例行 |
| FR-3 | 將解析結果轉為 **GCP 共用 JSON Protocol**（§5）並經 HTTPS 傳送至 GCP。 | mock GCP endpoint 收到預期 JSON |
| FR-4 | 傳送失敗自動重試（指數退避），最終失敗寫入 **spool 目錄**（JSONL 落盤），啟動時最佳努力補送。 | 停止 GCP 服務再恢復，驗證不丟資料 |
| FR-5 | 可選「上/下班」時間窗分類（`check_in` / `check_out` / `unknown`），由設定檔驅動。 | 設定檔範例 |
| FR-6 | 連線存活維持：同一 TCP 連線長期保持、斷線自動等待重連；每筆記錄帶 `received_at`（接收端時間）。 | 實測連續刷多張卡 |
| FR-7 | 結構化日誌（tracing），可設 log level；含 device IP、Node ID、event code。 | log 輸出 |
| FR-8 | 設定採 `config.json` + 環境變數覆寫，支援 Container 部署。 | 設定檔範例 |
| FR-9 | 優雅關閉（SIGINT/SIGTERM），關閉前 flush 未送佇列。 | Ctrl-C 測試 |
| FR-10 | **UI 顯示本機 IP（iced）**：啟動時列舉本機所有 IPv4 位址（自動挑選「對外連線」IP 優先），大字顯示 `IP : 8031`，供施工人員照抄填入打卡機後台 `Message Server IP 1st` / `Message Port 1st`。 | 實機：依 UI 顯示填入後台後，打卡機能成功連線推送事件 |
| FR-11 | **UI 狀態面板**：顯示監聽埠 bind 狀態、已連線之打卡機（來源 IP／Node ID）、最近收到的事件摘要、GCP 上送狀態（成功／重試中／spool 待補送筆數）。 | 刷卡後 UI 即時更新 |
| FR-12 | 支援 **headless**（`--headless` 或 `PUNCH_UI_ENABLED=0`）：無顯示器環境（Docker／雲端）保持原 CLI 行為，UI 完全不啟動。 | Docker 冒煙測試無 GUI 正常運作 |

### 3.1 非功能需求
- 高吞吐：單機可承受多台打卡機同時連線（每連線獨立 task）。
- 部署：Docker / Cloud Run 均可；`receiver_id` 用於多實例辨識。
- **部署彈性（v1.4）**：`iced` GUI 用於現場中介主機（施工人員需看到 IP）；`--headless`／`PUNCH_UI_ENABLED=0` 用於 Docker / Cloud Run（無顯示器）。
- 資安：GCP 端使用 Bearer Token 或自訂 API Key header；記錄僅存處理後資料，不保存完整卡號明文於第三方日誌（可選遮蔽，見後續）。

### 3.2 v1 範圍外（Out of Scope）

datasheet 有定義、但 v1 明確**不做**的項目（避免驗收爭議；實作規劃見 §7）：

| 項目 | 內容 | 相關章節 |
|---|---|---|
| 人員建檔 / 白名單下載 | `83H` / `84H` Set User Parameters、`2EH` 寫入姓名（User Alias）。**已實測可用**，獨立工具見 `tools/punch_admin.py` | §2.5、§2.7、§7 |
| 人員 / 卡片刪除與查詢 | `85H` Erase user data、`87H` Get User Parameters、`86H` 重設 APB | §2.5、§7 |
| 卡片資訊擴充欄位 | `card.user_address`、`card.user_level`、`card.site_code`、`card.card_code`、`device.port_number` | §2.6、§5.2、§7 |
| 8033 推播 hosting 模式 | 機器主動推播事件的另一通道；v1 只用 8031 單向 | §2.2、§2.7 |
| 生物特徵 / 黑名單 | `8FH` 指紋、靜脈、人臉模板；`90H` 黑名單 UID | §2.5、§7 |
| 訪客 / 樓層 / 多門時區 | `8BH` 訪客時段、`2FH` 樓層、`89H` 多門各門時區 | §2.5 |

> v1 交付範圍 = **中介程式**：`iced` GUI 顯示本機 IP / 狀態（FR-10~FR-12）＋讀取 8031 TEXT 打卡事件並轉送 GCP（FR-1 ~ FR-9）。

---

## 4. 系統流程

```
TCP line ─▶ parser ─▶ PunchEvent ─┬─▶ iced UI 狀態面板（來源 IP / Node / 最近事件 / GCP 上送狀態）
                                  └─▶ classify(可選) ─▶ GcpPunchEvent ─▶ delivery worker
                                                                           ├─ ▶ 立即/批次 POST GCP
                                                                           └─ ▶ 失敗 retry → spool JSONL
```

- UI 為主程式的**展示層**：透過 channel 接收事件與狀態摘要，不參與解析／轉送，視窗關閉不影響核心（FR-12）。
- 每筆產生唯一 `event_id`（UUID v4）。
- `occurred_at`：由機端日期時間＋設定之「設備時區」組出 ISO 8601。
- `received_at`：接收端伺服器時間（ISO 8601）。
- 多份 spool 檔名含日期，可批次重送。

---

## 5. GCP 共用 JSON Protocol（資料契約）

> 版本：`gcp.punch.event.v1`（JSON Schema + 範例如下）。任何欄位新增採向後相容（optional），不可刪除既有欄位。

### 5.1 單筆事件物件（`GcpPunchEvent`）

```json
{
  "schema_version": "v1",
  "event_id": "3f0b6a1e-9c42-4d5e-8b01-2a1c3d4e5f60",
  "message_type": "punch_event",
  "occurred_at": "2021-05-12T13:38:54+08:00",
  "received_at": "2026-09-19T10:12:00+08:00",
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
    "description": "Normal Access by tag",
    "door_no": 0
  },
  "card": {
    "uid_hex": "00000000D4B81403",
    "uid_decimal": 356701004291,
    "card_number_hi": 54456,
    "card_number_lo": 5123
  },
  "person": {
    "alias": "Sammi",
    "user_id": null
  },
  "punch": {
    "punch_type": "check_in",
    "duty_code": null,
    "duty_label": null
  },
  "ingested_by": {
    "receiver_id": "punch-clock-01"
  },
  "raw_message": "21'05/12 13:38:54 [001.17:0B](0)00000000D4B81403 rSammi (M11)Normal Access"
}
```

### 5.2 欄位定義

> 上例為 **v1 實作之實際輸出**。下表標註「v1 未產生」者為向後相容之**預留 optional 欄位**，雲端 parser 必須容忍缺欄位（不視為錯誤）。

| 欄位 | 型別 | 必填 | 說明 |
|---|---|---|---|
| `schema_version` | string | 是 | 固定 `v1` |
| `event_id` | string (uuid) | 是 | 接收端產生之唯一 ID（冪等識別） |
| `message_type` | string | 是 | 固定 `punch_event` |
| `occurred_at` | string ISO8601 | 是 | 機端刷卡時間＋設備時區偏移 |
| `received_at` | string ISO8601 | 是 | 接收端時間 |
| `device.maker` | string | 是 | 固定 `SOYAL` |
| `device.model` | string | 是 | 設備型號，由設定提供 |
| `device.node_id` | int | 是 | 機端 Node ID（`[001...]`） |
| `device.ip` | string | 是 | 連線來源 IP |
| `device.source_sub_code` | int | 是 | TEXT `[ ]` 中間欄＝**Port Number**（`881E §4.1 Data 8`：17 主埠、18 WG1、19 WG2、1~16 多門子機）。※ 欄位名沿用 v1 實作（`src/model.rs`），語意化別名見下一列，**不刪除本欄位** |
| `device.port_number` | int\|null | 否 | 與 `device.source_sub_code` 同值之語意化別名（Port Number）。**v1.6 已產生** |
| `event.function_code` | int | 是 | 十進位事件碼（M 碼數值） |
| `event.event_code` | string | 是 | `M{code}` |
| `event.description` | string | 是 | 事件描述（機端提供者優先，缺省用內建對照表） |
| `event.door_no` | int\|null | 否 | `( )` 內之門號 |
| `card.uid_hex` | string | 是 | 8 bytes Tag UID HEX（大端呈現 16 碼）；bit31~16 = **Site Code**、bit15~0 = **Card Code**（§2.6） |
| `card.uid_decimal` | int\|null | 否 | HEX 之 u64 十進位 |
| `card.card_number_hi` | int\|null | 否 | Tag UID bit31~16（＝**Site Code**，十進位） |
| `card.card_number_lo` | int\|null | 否 | Tag UID bit15~0（＝**Card Code**，十進位） |
| `card.site_code` | int\|null | 否 | `card_number_hi` 之語意化別名。**v1.6 已產生** |
| `card.card_code` | int\|null | 否 | `card_number_lo` 之語意化別名。**v1.6 已產生** |
| `card.user_address` | int\|null | 否 | 機端人員索引（`881E §4.1 Data 9/10`；無效卡片事件時為 Tag ID bit15~08/07~00）。**TEXT 模式一律 null**，需 8033 HEX；**v1 未產生** |
| `card.user_level` | int\|null | 否 | 使用者等級（`Data 14`）。**TEXT 模式一律 null**；**v1 未產生** |
| `person.alias` | string\|null | 否 | 用戶別名（機端下載之姓名） |
| `person.user_id` | string\|int\|null | 否 | 雲端映射之學生學號（由 GCP 側填補） |
| `punch.punch_type` | string | 是 | `check_in`\|`check_out`\|`unknown`（由時間窗分類） |
| `punch.duty_code` | int\|null | 否 | Duty code（Sub Code bit7~5，0~7）。⚠️ **v1（8031 TEXT）一律為 `null`**：Duty code 位於事件封包 Data 11 `Sub Code`，TEXT 行內無此欄位（見附錄 B） |
| `punch.duty_label` | string\|null | 否 | Duty 文字（On Duty/Off Duty…）。同上，**v1 一律 `null`** |
| `ingested_by.receiver_id` | string | 是 | 接收端實例名稱 |
| `raw_message` | string | 是 | 原始接收行（保留稽核） |

### 5.3 傳輸方式（GCP 端實作建議）

Transport 二選一：

1. **HTTPS（本 v1 實作）**：POST `application/json`，Body 為單筆物件或批次陣列：
   - 單筆：`{"events": [ GcpPunchEvent ]}`（batch disabled 時）
   - 批次：`{"events": [ GcpPunchEvent, ... ]}`（batch enabled 時，上限 `batch_max_items`）
   - 驗證：`Authorization: Bearer <token>` 或 `X-Api-Key`（可設定）。
   - GCP 推薦架構：Cloud Run（或 API Gateway）→ Pub/Sub → Dataflow/BigQuery。

2. **Pub/Sub（後續版本）**：接收端以 `google-cloud-pubsub` publish 到 topic；可作為無 HTTP endpoint 時的替代。

### 5.4 錯誤與冪等
- **冪等**：GCP 側以 `event_id` 去重（重送不重複入帳）。
- **重試**：接收端對 5xx/timeout 指數退避（預設最多 5 次）；最終失敗落 `spool/`。
- **格式錯誤（400/422）**：記錄為 metering 錯誤並落 spool，供 Debug。

---

## 6. 設定檔（`config.json`）

```json
{
  "listen": { "bind": "0.0.0.0", "port": 8031, "mode": "text" },
  "ui": { "enabled": true },
  "device": { "maker": "SOYAL", "model": "AR837EF" },
  "receiver_id": "punch-clock-01",
  "timezone_offset_seconds": 28800,
  "spool_dir": "./spool",
  "log_level": "info",
  "classify": {
    "enabled": true,
    "windows": [
      { "from": "05:00", "to": "12:00", "kind": "check_in" },
      { "from": "12:00", "to": "23:59", "kind": "check_out" }
    ]
  },
  "gcp": {
    "endpoint_url": "https://punch-events-xxxxxx.run.app/api/v1/punch-events",
    "bearer_token": "",
    "api_key_header": "X-Api-Key",
    "api_key_value": "",
    "timeout_secs": 10,
    "retry_attempts": 5,
    "batch_enabled": false,
    "batch_max_items": 100,
    "batch_flush_interval_secs": 5
  }
}
```

環境變數覆寫：`PUNCH_BIND`、`PUNCH_PORT`、`PUNCH_UI_ENABLED`（`0` 關閉 GUI，等同 `--headless`）、`PUNCH_GCP_URL`、`PUNCH_GCP_TOKEN`、`PUNCH_GCP_API_KEY`、`PUNCH_GCP_API_KEY_HEADER`、`PUNCH_RECEIVER_ID`。（Token/Key 之值勿進 git，用 `.env` / Secret Manager。）

---

## 7. 後續規劃（不在 v1）

| 項目 | 說明 | 備註 |
|---|---|---|
| 8033 推播 hosting 模式 | 機器主動推播事件的另一通道；v1 只需 8031 即可 | 與指令埠（1621）無關，見 §2.7 |
| 人員／白名單同步 | 由雲端下發 `83H`/`84H`（`User Addr`、`Tag UID`、`PIN`、`Mode`、`Zone`、`Group1/2`、`Year/Month/Day`、`Level`、`Option`，每筆 26 bytes）、`2EH` 寫入姓名（16 bytes）、`85H` 刪除、`87H` 查詢 | **已實測可行：一般 TCP 指令埠 + ACK，不需 8033**；現可用 `tools/punch_admin.py`；H / E 系列封包長度不同（§2.5、§2.7） |
| 卡片資訊擴充 | 新增 `card.user_address`、`card.user_level`（`card.site_code`、`card.card_code`、`device.port_number` 已於 **v1.6** 送出） | 需 8033 HEX 或 `87H` 反查（實測 `87H` 回讀**不含位址**，需自行比對） |
| 多台分機管理 | 依 `device.ip` / Node ID 分派不同雲端 endpoint 或專案 | 設定可多 entry |
| 卡號遮蔽（PII） | Hash UID 原文再上送，避免明文外洩 | 資安強化 |
| 心跳／離線偵測 | 若同 Node 一段時間無事件，上送 offline 告警 | 需機端配合間隔送 CNJ |

**v1.3 已釐清之串接前提（人員／白名單同步實作前必讀）**

- **位址需明示指派**：無自動分配；建議以 0 起連續位址對齊後台顯示（§2.8）。
- **`87H` 多筆讀取批次上限 10**：超過會截斷（§2.7 結論 4；`scan-users` 已實作降級）。
- **姓名獨立存放**：卡片（84H）與姓名（2EH）分開寫入，皆需 ACK；姓名目前只能以後台確認。
- **中文別名編碼待驗**：`8031 TEXT` 的中文別名可能為 Big5，Rust 端目前以 UTF-8 行讀取
  可能解析失敗（記為 unparsable），串接 GCP 前需實機刷卡觀察並在 parser 加容錯
  （byte 級解析或 Big5→UTF-8 轉換）。

### 7.1 GCP 串接注意事項（v1.3 新增）

- `person.alias`：8031 TEXT 的別名可能帶機端顯示前綴（官方範例 `rSammi` 的 `r`，不保證固定）；
  建議在 receiver 去除前綴後再上送，或在 GCP parser 正規化。中文別名編碼（Big5？）尚未實機驗證
  （見上「串接前提」）。
- `card.site_code` / `card.card_code`：名冊建立已可用工具 `--uid site:card` 匯入（§2.6、§2.7）；
  GCP 端對應欄位已在 §5.2 預留，**v1.6 起 `src/model.rs` 已同步輸出**
  `card.site_code`（＝`card_number_hi`）與 `card.card_code`（＝`card_number_lo`），
  `card.card_number_hi` / `card.card_number_lo` 仍保留以向後相容。
- 名冊建立路徑（v1.3 實測可行）：`punch_admin.py add-user --addr N --uid site:card --name 姓名 --yes`
  一次完成「人員＋卡片＋姓名」，為 §7「人員／白名單同步」的離線對照與匯入來源。

---

## 8. 驗收標準（Acceptance）
1. 機端 `Message Server IP 1st` 指向 receiver，Port 8031；刷卡後 **≤1 秒**內 GCP endpoint 收到對應 JSON（`occurred_at` 與刷卡時間一致）。
2. 官方範例行解析通過單元測試（日期、時間、Node、Port Number、FuncCode、UID、Site / Card Code、別名、描述）。
3. 停掉 GCP endpoint 20 秒再啟動，期間所有刷卡記錄最後仍全數送達（spool 補送）且以 `event_id` 去重。
4. 打卡機重新開機或拔插網路後自動重連，無需重啟 receiver。
5. RFC3339 校時：`occurred_at` 屬正確設備時區（+08:00 範例）。
6. 未知 / 未涵蓋事件碼不得中斷服務：記錄 warning 後照常上送（`event_code` 保留原始 M 碼）；且 GCP 端可容忍 §5.2 標註「v1 未產生」之 optional 欄位缺漏。
7. **機端 RTC 需先校時**：實測機端時間為 `2010-11-09`（未校時），校時後 `occurred_at` 須與實際刷卡時間一致（誤差 ≤1 秒），否則不得視為通過（可用 `23H` 或機端網頁校時）。
8. **GUI 施工流程（FR-10）**：無參數啟動 receiver 後出現 iced 視窗並大字顯示本機 IPv4。施工人員依畫面將打卡機後台 `Message Server IP 1st` 填上顯示之 IP、`Message Port 1st` 填 `8031` 並儲存（後台預設 `0.0.0.0`／`0`＝關閉）；刷卡後 **UI 即時列出該事件**，且 GCP ≤1 秒內收到（與第 1 項一致）。
9. **UI 顯示準確（FR-11）**：UI 所列 IP 與本機實際介面 IP（`ipconfig`）一致，且**不得顯示 `0.0.0.0`**；打卡機與 receiver 同網段（或可路由）時能連上並推送。
10. **headless（FR-12）**：`--headless` 或 `PUNCH_UI_ENABLED=0` 時不開啟視窗，服務照常收案與轉送 GCP。

---

## 附錄 A：事件碼對照（M00~M114 節錄）

出處：`Protocol_881E_725Ev2_82xEv5 4V05.pdf` §4.2「Function code list of Event log」（PDF p.106–110，M00~M51+）與 `Message File structure.pdf` p.3~5（延伸 M55~M200）。
**★ = v1 內建對照表（`src/function_codes.rs`）已涵蓋**；未標 ★ 者，`description` 以機端回傳文字為準，機端未提供時記為 `Unknown`。

| FuncCode | M 碼 | 中文 | English | v1 |
|---|---|---|---|---|
| 00 | M00 | 系統識別碼錯誤 | Site code error | ★ |
| 01 | M01 | 無效用戶位址或不允許密碼通行 | Invalid user PIN | ★ |
| 02 | M02 | 連續錯誤，按鍵鎖定 | Keypad Locked by over error limits times | ★ |
| 03 | M03 | 無效卡片 | Invalid card | ★（無效卡片之卡號欄位語意不同，見 §2.6） |
| 04 | M04 | 通行時段錯誤（不准在此時間進出） | Time Zone error | ★ |
| 05 | M05 | 通行門組錯誤（不准由此門進出） | Door Group error | ★ |
| 06 | M06 | 通行日期管制（超過期限） | Expiry Date | ★ |
| 07 | M07 | 超出通行次數限制 | Over access times | ★ |
| 08 | M08 | 密碼輸入錯誤 | PIN Code error | ★ |
| 09 | M09 | 緊急求救已啟動 | Press duress PB | ★ |
| 10 | M10 | 以刷卡加密碼方式通行 | Access by Card and PIN | ★ |
| 11 | M11 | 正常進出（讀卡） | Normal Access by tag | ★ v1 主要事件 |
| 12 / 13 | M12 / M13 | 強制開啟 / 關閉控制器繼電器 | Force Controller Relay ON / Off | ★ |
| 14 / 15 | M14 / M15 | 控制器啟動 / 解除警戒 | Controller armed / disarmed | ★ |
| 16 | M16 | 以外出按鈕開門 | Egress | ★ |
| 17 | M17 | 發生警報 | Alarm event | ★（Sub Func. bit7 = 強制開門警報） |
| 18 | M18 | 限次卡片最後一次進出 | Last Access Time | — |
| 19 | M19 | 巡邏員刷卡 | Guard Access | — |
| 20 / 24 | M20 / M24 | 控制器關閉 / 開啟電源 | Controller Power Off / On | ★（見下方注意） |
| 21 | M21 | 以脅迫密碼求救 | Duress | ★（見下方注意） |
| 22 | M22 | 巡邏員求救 | Guards for help | ★ |
| 23 | M23 | 清潔員刷卡 | Cleaner access | ★ |
| 25 | M25 | DO 連控超出有效範圍 | Force Controller Relay On/Off Error | — |
| 26 | M26 | 控制器回復正常狀態 | Controller Return to Normal (RTN) | — |
| 27 | M27 | 求救按鈕已啟動 | Help push button pressed | — |
| 28 | M28 | 以密碼操作通行 | Access by PIN (Key Only) | ★ |
| 29 | M29 | DI 輸入點動作（SubCode 00=Off / 01=On） | Digital input actives | ★ |
| 30 / 31 / 32 | M30 / M31 / M32 | 違反進出管制 / RS485 讀卡機離線 / 重新連線 | Anti-pass back Error / reader off-line / on-line | ★ |
| 33 / 34 | M33 / M34 | 使用者自行更改密碼 / 更改失敗 | User PIN code changed / error | ★ |
| 35 / 36 | M35 / M36 | 進入 / 結束自動開門程序 | Enter / Exit Auto Door Open Procedure | ★ |
| 37 / 38 | M37 / M38 | 自動解除 / 啟動警戒 | Auto Disarmed / Armed | — |
| 39 / 40 | M39 / M40 | 以指紋或靜脈通行 / 指紋辨識失敗 | Access by fingerprint or finger vein / identify failed | ★ |
| 42 / 43 / 44 / 45 | M42~M45 | 遙控器上鍵 / 停用讀卡機 / 啟用讀卡機 / 遙控器緊急鍵 | Remote Up / Disable Reader / Enable Reader / Remote Panic | ★ |
| 46 / 47 / 48 | M46~M48 | 停車場入車 / 出車 / 計數遞增 | Entrance / Exit / Counter triggered | — |
| 49 / 50 / 51 | M49~M51 | 繼電器鎖定 / 門偵測（關閉）/ 門已開啟 | Latch Relay / Door Closed / Door Open | — |
| 55 | M55 | 全域自由通行 | Global free access | — |
| 60 | M60 | 刷卡但未開門 | Flash Card But No Open Door | — |
| 63~75 | M63~M75 | SOR 規則（扣款成功 / 失敗 / 餘額不足 / 到期…） | SOYAL Open System Rule | — |
| 86 | M86 | 黑名單卡刷卡 | Black table tag accessed | ★ |
| 90 | M90 | 信箱訊息（subfunc 00=新信件、02=未關閉警報） | Mailbox message | — |
| 100 / 101 | M100 / M101 | 靜脈進出成功 / 失敗 | Access ok / reject via vein | ★ |
| 102 | M102 | 門鎖內部鎖定禁止 | Inhibited by internal lock | — |
| 104 | M104 | 火警輸入觸發 | Fire alarm input trigged | ★ |
| 108 / 110 | M108 / M110 | 人臉辨識成功 / 車牌辨識成功 | Face / Car plate Recognize OK | — |
| 114 | M114 | 遠端考勤（SubCode 1 遠端進入 / 2 遠端離開 / 3 修改進入 / 4 修改離開） | Remote Time Attendance | ★ |

> ✅ **已知落差（v1.6 已修正）**：`src/function_codes.rs` 於 30~34 原有位移錯誤（code 30 / 31 對到「副讀卡機離線 / 連線」、code 32 / 33 對到「用戶修改密碼 / 失敗」、code 34 未定義），v1.6 已對齊 datasheet：30=Anti-pass back、31/32=副讀卡機離線/連線、33/34=用戶修改密碼/失敗。
> ⚠️ **M20 / M21 語意衝突**：`Message File structure.pdf` 註明「`message type`(field 98) = 0x00 且 Field 10 function code 為 20 / 21 時，該筆為 701Client **軟體登入 / 登出**，buffer[12~41] 為操作者姓名、buffer[96] 為系統 user index」；而控制器 event log 的 M20 / M21 為「電源關閉 / 被脅迫」。接收端收到 20 / 21 時須以來源（訊息類型）區分，**不可一律視為考勤事件**。
> 完整表請參考 `Protocol_881E_725Ev2_82xEv5 4V05.pdf` §4.2 與 `Message File structure.pdf`「Controller Function code define」。

## 附錄 B：Duty code（Sub Code bit7~5）

出處：`Message File structure.pdf` p.3 Note 2、`Protocol_881E_725Ev2_82xEv5 4V05.pdf` §4.1 Data 11。

| bits7~5 | 說明 |
|---|---|
| 000 | On duty（上班） |
| 001 | Off duty（下班） |
| 010 | Overtime in |
| 011 | Overtime out |
| 100 | Break out |
| 101 | Break in |
| 110 | Go out |
| 111 | Return |

> ⚠️ **v1（8031 TEXT）無法取得 Duty code**：Duty code 位於事件封包 Data 11 `Sub Code` 之 bit7~5，**TEXT 行內沒有此欄位**，因此 v1 的 `punch.duty_code` / `punch.duty_label` 一律為 `null`，上 / 下班判定只能依 §6 `classify.windows` 時間窗（最終仍請於 GCP 側以業務規則覆核）。需精確 Duty code 時須啟用 8033 HEX 模式（§7）。
> `Sub Code` 之 bit4~0 為 sub message code，例如：M17 bit7 = 強制開門警報、M29 `00`=Off / `01`=On、M49 `00`=Relay ON / `01`=Relay OFF、M114 `1`~`4` = 遠端進出 / 修改進出（見附錄 A）。
