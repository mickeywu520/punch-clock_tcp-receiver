# SOYAL 打卡機通訊協定整理（protocol.md）

本文件整理自 **`Protocol_881E_725Ev2_82xEv5 4V05.pdf`**（SOYAL，2024/4/8，114 頁），
只收錄本專案**用得到／可能用到**的指令，並補上我們在實機
（**AR-821EF v5、韌體 4V6、Node ID=1、192.168.1.127:1621**）驗證出來的差異。
純語言無關的協定規格；程式對應位置見文末。

其他可參考的原始檔：`硬體Protocol及範例/`（另有 401ROxDIx、721_727H、Message File structure、
SOYAL Protocol 靈活使用技巧）。

---

## 0. 常用問答

| 問題 | 結論 |
| --- | --- |
| 87H 取得人員有含姓名嗎？ | **沒有**。87H 只回 UID/PIN/Mode/Zone/Group/到期日/Level/Option。姓名要用 **2EH Read user alias** 另外讀（已實作於匯出 CSV）。 |
| 人臉特徵是否用同一協定？ | **否**。生物特徵（指紋/指靜脈/人臉）走 **8FH**，以 Sub Code 分子指令；人臉為 **Mode-EA**，見 §9。 |
| 位址與 UID 的位元組序？ | 文件寫 little endian，但**實機 AR-821EFv5 實測為 big endian**（見 §11）。 |
| 一台設備可同時幾個連線？ | 只接受**單一 master**；寫入/讀取前須先釋放輪詢連線（見 §11）。 |

---

## 1. 封包格式（Data Package）

「詢問／回覆」（interrogation/reply）模式，可走 RS485 或 TCP/IP。**本專案用 Standard 模式**（非加密）。

### 1.1 Standard Short（短封包，≤250 bytes）

| 欄位 | 長度 | 說明 |
| --- | --- | --- |
| Head | 1 | 固定 `0x7E` |
| Length | 1 | 從 DID 到 SUM 的位元組數（含 XOR、SUM） |
| DID | 1 | Destination Node ID（本機 Node ID=1；`00`=bus master，`FF`=廣播） |
| Command | 1 | 功能碼（如 `0x24`） |
| Data | n | 依指令而異 |
| XOR | 1 | 從 DID 到 Data 逐 byte 與 `0xFF` 起值做 XOR |
| SUM | 1 | 從 DID 到 XOR 逐 byte 累加（初始 0），取低 8 bits |

範例（18H Polling）：`7E 04 01 18 E6 FF`
- XOR = `FF ^ 01 ^ 18` = `E6`
- SUM = `01 + 18 + E6` = `FF`（取低 byte）

### 1.2 Standard Large（長封包）

| 欄位 | 長度 | 說明 |
| --- | --- | --- |
| Head | 4 | `FF 00 5A A5`（MSB first） |
| Length | 2 | 從下一位元組到封包結尾，MSB first；**高位 nibble 為 area code**。TCP/IP ≤ 1400 |
| DID | 1 | 同上 |
| Command | 1 | 同上 |
| Data | n | 同上 |
| XOR / SUM | 1 / 1 | 同上 |

### 1.3 Security 模式（本專案未用）

`7F`（短）或 `FF 00 55 AA`（長）開頭，含 RDN(4) 隨機碼與 CRC16-Modbus；設備安全碼 8 bytes 全 `0xFF` 時才會以 Standard 模式啟動。**目前實機走 Standard。**

---

## 2. 連線與節點

- Node ID（DID）設定：`＊123456＃` → `00＊001＃` → `＊＃`（AR-821EFv5 為手動）。
- TCP 命令埠：**1621**（以卡鐘網頁「Network Setting → TCP Port」為準）。
- 通訊格式 9600,N,8,1（RS485）；TCP/IP 直連不受此限。

## 3. 回應／回覆碼（本專案會遇到的）

| 回覆 Command | 名稱 | 說明 |
| --- | --- | --- |
| `0x03` | Data echo（資料回覆） | 回應讀取請求（24H/25H/2EH/87H/8FH…） |
| `0x04` | ACK（成功） | 寫入/設定指令成功；可附控制器狀態 bytes |
| `0x05` | NACK（拒絕） | 指令被拒 |
| `0x06` | Protocol error | 格式錯誤 |
| `0x0C` | Communication level / auth error | 通訊層級或認證錯誤 |
| `0x09` | 18H echo | Hosting polling 的裝置狀態／事件回覆 |

> 資料回覆（`0x03`）的 Data 通常**第一位是 Source ID**，之後才是 payload。87H/8FH/2EH 讀取皆如此。
> 實機對 84H/2EH 寫入會**直接回 `0x04`，沒有先導 echo**（見 §11）。

---

## 4. 指令速查表（本專案相關）

| 指令 | 名稱 | 用途 | 本專案 |
| --- | --- | --- | --- |
| `18H` | Hosting Polling | 輪詢裝置狀態；可帶時間同步欄位 | 參考 |
| `23H` | Set RTC | 設定裝置時間 | 校時 |
| `24H` | Get RTC | 讀裝置時間＋韌體版本 | 校時、**暖身指令** |
| `25H` | Get oldest event log | 讀最舊一筆事件 | 事件拉取 |
| `2DH` | Empty event log | 清空全部事件 | 未用 |
| `2EH` | Read/Write User Alias | **讀/寫姓名** | 姓名 |
| `37H` | Remove oldest event log | 刪除最舊一筆 | 事件拉取 |
| `83H` | Set User（含 APB） | 新增/覆寫人員＋反潛回 | 未用（改 84H） |
| `84H` | Set User（無 APB） | **新增/覆寫人員** | 新增人員 |
| `85H` | Erase user data | 刪除位址區間人員 | 參考 |
| `86H` | Init anti-pass-back | 重設反潛回 | 未用 |
| `87H` | Get User Parameters | **回讀人員** | 匯出 CSV |
| `8FH` | Biometric data | 指紋/指靜脈/**人臉**模板 | 人臉（規劃） |
| `90H` | Black UID management | 黑名單 UID | 未用 |
| `A6H` | System command | 重開機 / 參數重置 | 未用 |

---

## 5. 人員資料指令詳解

### 5.1 84H（=83H 無 APB）Set User Parameters

請求 Data：

| 欄位 | 長度 | 說明 |
| --- | --- | --- |
| DID | 1 | Node ID |
| CMD | 1 | `83`=含 APB、`84`=不含（APB 旗標放 Option byte） |
| Records | 1 | 本封包要下載幾筆（本專案每次 1 筆） |
| User Addr | 2 | 人員位址（0~16383） |
| Tag UID | 8 | 卡片 UID |
| PIN | 4 | PIN 碼 |
| Mode | 1 | 通行模式（見 §6） |
| Zone | 1 | 通行時區 |
| Group1 | 1 | 可用門組 Door16~9（bit7~0） |
| Group2 | 1 | 可用門組 Door8~1（bit7~0） |
| Year / Month / Day | 1/1/1 | 到期日（2 位數年） |
| Level | 1 | Bit7~6 等級(0~3)、Bit5~0 WG 埠 Zone2 |
| Option | 1 | Bit7 反潛回開關（僅 83H 有效；84H 丟棄） |
| 保留 | 3 | |

- 26 bytes/筆，多筆時連續接續。
- Index 與 Records 全為 0 = 清空整個人員資料庫。
- Echo：`0x04` ACK 或 `0x05` NACK。

### 5.2 87H Get User Parameters

請求 Data：`Addr H(1) Addr L(1) Nums(1)`（起始位址＋筆數）。

回覆（Command `0x03`）：`Source ID(1)` ＋ N×**24 bytes** 記錄。

- 24 bytes／筆（**不含位址欄**），結構＝§5.1 去掉 User Addr 的後 24 bytes。
- 空白位址：整筆或 UID 欄全 `0xFF`。
- 實機特性：**nums ≤ 10 才完整**；過大回應會被截斷（見 §11）。

### 5.3 85H Erase user data

Data：`Start-H Start-L End-H End-L`（要刪的位址區間）。Echo `0x04`/`0x05`。
刪除需 100ms~6s，一次建議刪 < 1000 筆以免逾時。

---

## 6. 人員記錄欄位位元定義

### 6.1 Mode（通行模式）

| 位元 | 定義 |
| --- | --- |
| Bit7~6 | Access Mode：`00`無效、`01`卡片(0x40)、`10`卡片或密碼(0x80)、`11`卡片＋密碼(0xC0) |
| Bit5 | 巡邏卡 |
| Bit4 | 指紋後免刷卡的卡 |
| Bit3 | 刷卡後免指紋 |
| Bit2 | 啟用到期日檢查 |
| Bit1 | Guest 使用者＋PIN 時段（Ver4.04+） |
| Bit0 | 允許自行改密碼 |

> **⚠️ 實機重要發現（AR-821EFv5，含人臉模組）**：若 **Bit3／Bit4 為 0**，控制器會把每次刷卡
> 視為「卡＋生物特徵」多因子，**卡單獨刷無法完成**（面板停在「影像 + 讀卡/密碼」），且
> **不產生 M11**。本機自行登錄的人員會帶這兩個位元（Mode=`0x58`）。因此主機寫入卡片模式時
> 應設 **Bit3＋Bit4**：`0x40→0x58`、`0x80→0x98`、`0xC0→0xD8`。本專案已比照（見 §11-10）。

### 6.2 Zone

- Bit7：多門機＝各節點獨立時區；單門機＝是否 WG 埠另用 Zone2。
- Bit6：保留，必須 0。
- Bit5~0：通行時區（0＝free，不受時區管制）。

### 6.3 Group1 / Group2

門組位元對應，設 `1` 表示允許通行該門：Group1 = Door16~9、Group2 = Door8~1。

### 6.4 Level / Option

- Level：Bit7~6 等級（0~3），Bit5~0 為 WG 埠 Zone2。
- Option：Bit7 = 啟用反潛回檢查（僅 83H 有效）。

> 到期日「不設限」慣例：`Y=0x4F(79)、M=0x0C(12)、D=0x1F(31)` → 2079-12-31。

---

## 7. 姓名（User Alias, 2EH）

姓名 16 bytes／筆（繁中 Big5，最多 8 字），與人員記錄分開存。

### 7.1 Write（Download）

Data：`Index H(1) Index M(1) Index L(1) Records(1)` ＋ 每筆 16 bytes。
Index 為 24-bit 起始索引（本專案用位址）；Records＝筆數。
Index 與 Records 全 0 = 清除全部姓名。

### 7.2 Read（Upload）

請求 Data：`Index H M L(3) Records(1)`。
回覆（`0x03`）：每筆 **16 bytes** 姓名（Big5）。

> 支援韌體 2.07 之後。**這是唯一能回讀姓名的管道**（87H 不含姓名）。
> 讀/寫共用指令 `2E`，靠封包總長區分：**只有 4 bytes 參數＝讀**；帶 16×N bytes 資料＝寫。
> 實機（AR-821EF v5 / 4V6）驗證：回覆 `0x03`、**無 Source ID**、每筆固定 16 bytes、
> 空位址＝16×`FF`；寫入 ASCII/Big5 後回讀完全一致。姓名以首個 `0x00` 結束。

---

## 8. 事件記錄（Event Log）

### 8.1 25H Get oldest event log

請求：`7E 04 01 25 DB 01`（Data 可留空）。
- 無事件時回 `0x04` ACK。
- 有事件時回 `0x27`（事件記錄，見 §8.3）。
- 延伸用法（2.07+）：Data 填 `FF FF FF` 可查目前佇列狀態（event counter / input point / output point）。

### 8.2 37H Remove oldest / 2DH Empty

- `37H`：刪除最舊一筆（讀完要刪，否則會一直讀到同一筆）。Echo `0x04`/`0x05`。
- `2DH`：清空全部事件。

### 8.3 事件記錄結構（Function code `0x27`，共 29 bytes data）

| Data | 說明 |
| --- | --- |
| 0 | Source Node ID |
| 1~2~3 | 秒 / 分 / 時 |
| 4~5~6~7 | 星期 / 日 / 月 / 年(00~99 = 2000~2099) |
| 8 | Port Number（17=主埠、18=WG1、19=WG2） |
| 9~10 | 正常通行：User address hi/lo；無效卡：Tag ID hi/lo |
| 11 | Sub Code（子訊息碼） |
| 12 | Sub Func（function 17 警報＝force open） |
| 13 | Ext Code（事件埠選項） |
| 14 | User level（bit5~0 等級、bit6 free access、bit7 多門/WG） |
| 15~16 | Tag ID bit31~24 / bit23~16 |
| 17 | Door Number |
| 19~20 | Tag ID bit15~08 / bit07~00 |
| 21~24 | 扣點/餘額 或 8-byte UID 高位 |
| 25~28 | 使用者輸入碼（PIN 事件） |

> Tag ID（32-bit）= `(Data15<<24)|(Data16<<16)|(Data19<<8)|Data20`；
> User Address = `(Data9<<8)|Data10`（合法卡刷卡時為位址，非卡號）。

### 8.4 Function code（事件碼）常用對照

| 碼 | 名稱 | 中文 |
| --- | --- | --- |
| M00 | Site code error | 系統識別碼錯誤 |
| M01 | Invalid user PIN | 無效用戶位址/密碼 |
| M03 | Invalid card | 無效卡片 |
| M04 | Time Zone error | 通行時段錯誤 |
| M05 | Door Group error | 通行門組錯誤 |
| M06 | Expiry Date | 通行日期管制 |
| M10 | Access by Card and PIN | 刷卡加密碼通行 |
| **M11** | **Normal Access (by tag)** | **正常進出** |
| M16 | Egress | 外出按鈕開門 |
| M19 | Guard Access | 巡邏員刷卡 |
| M24 | Controller Power On | 控制器開機 |
| M25 | Force Relay On/Off Error | 連控超出有效範圍 |
| M28 | Access by PIN | 密碼通行 |
| M30 | Anti-pass back Error | 違反進出管制 |
| M39 | Access by fingerprint/vein | 指紋/靜脈通行 |
| M55 | Free Access Enable/Disable | 見卡即開 |
| M100 | Access ok via vein | 靜脈通行成功 |
| M101 | Access reject via vein | 靜脈通行失敗 |
| **M108** | **Face ID Passed** | **人臉辨識通行成功** |
| M109 | Face ID Rejected | 人臉辨識通行失敗 |
| M112 | Black list of Face ID | 人臉識別黑名單 |

> 本專案 `src/function_codes.rs` 已涵蓋 M00~M39、M100/101、**M108/109/112（人臉）** 等。

---

## 9. 生物特徵資料（8FH，Fingerprint / Vein / Face）

Command `8F`，以 Sub Code 區分。依硬體模組有不同可用集：

| Sub Code | 功能 | 適用模組 |
| --- | --- | --- |
| `01` | 檢查模板 ID 是否存在 | 3DO |
| `02` | 刪除某使用者全部模板（`FFFFFFFF`＝全刪） | 3DO |
| `03` | 讀取（上傳）指紋模板（Template ID、Offset、Bytes） | 3DO |
| `04` | 寫入（下載）指紋模板（498 bytes/模板，建議長封包） | 3DO |
| `06` | 取得已註冊模板總數 | 3DO |
| `11` | 取得指定使用者指靜脈註冊數／是否註冊 | 2000 |
| `12` | 刪除指靜脈模板 | 2000 |
| `13` / `14` | 讀取 / 寫入指靜脈模板（1728 bytes/人） | 2000 |
| `21` | 取得已註冊指紋模板數 | 9000 |
| `22` / `23` / `24` | 刪除 / 讀取 / 寫入指紋模板 | 9000 |
| **`31`** | **取得已註冊人臉數**（可全體或指定位址） | **EA** |
| **`32`** | **刪除人臉**（下載前必先刪） | **EA** |
| **`33`** | **讀取（上傳）人臉資料** | **EA** |
| **`34`** | **寫入（下載）人臉資料** | **EA** |

### 9.1 人臉（Mode-EA）細節

- **每人 784 bytes**，每次最多讀/寫 **200 bytes**，分段進行且每段間隔 **≥ 500ms**。
- 讀取（Sub `33`）：`Addr HH HL LH LL(4) | Offset H L(2) | Bytes H L(2)`。
  例：`ff 00 5a a5 00 0d 01 8f 33 00 00 03 78 00 00 00 c8 …`（讀位址 0x378、offset 0、200 bytes）。
- 寫入（Sub `34`）：`Addr(4) | Total(2, 固定 0x0310) | Offset H L(2) | Bytes H L(2) | Data`；Echo `0x04`。
  下載前必須先 `32` 刪除已註冊人臉。
- 取得數量（Sub `31`）：`Addr(4)`；位址填 `0xFFFFFFFF` 回全體總數，否則回該使用者是否註冊。

### 9.2 指紋 / 指靜脈

- 指紋模板 498 bytes／模板（3DO）；9000 為另一套子指令集。
- 指靜脈 1728 bytes／人（2000）。
- 上傳前須先停止模組自動掃描（9000：Sub `24` Pause/Restart Auto Scan）。

> **注意**：人臉/指紋模板是**二進位模板**，不是可讀的圖像；跨機移轉需同模組同版本。

---

## 10. 其他可能用到

- **90H Black UID Management**（僅 16384 Users 模式）：Sub `00` 讀、`01` 寫、`02` 加一筆、`03` 刪一筆、`04` 全刪；每筆 8 bytes UID。
- **A6H System Command**：Sub 可重開機 / 參數重置。
- **26H Buzzer/LED**、**28H 送字到 LCD**：可用於現場提示。

---

## 11. 本專案實機驗證備註（重要！與文件不同處）

實機環境：AR-821EF v5、韌體 4V6、Node ID 1、`192.168.1.127:1621`。

1. **位元組序為 Big Endian**（文件寫 little endian）：
   - 寫入位址 1000 → record 前 2 bytes `03 E8`（BE）。
   - `64867:29942` → UID `00000000FD6374F6`（BE，與 8031 TEXT 所見一致）。
   - 工具提供 `--addr-order` / `--uid-order` 可切換。
2. **87H 不能當新連線的第一道指令**：直接送會 **0 bytes 逾時**。必須先送一道無副作用指令
   （本專案送 **24H** 暖身），之後 87H 才回 `0x03`。
3. **87H 每批 ≤ 10 筆**；nums 過大回應會被截斷（或部分機型關閉連線），需降為逐筆。
4. **84H/2EH 直接回終結碼 `0x04`**，沒有先導 echo。程式需把「第一個回覆」若已是終結碼就採用，
   否則會多等一包而逾時。
5. **空槽形狀非全 FF**：實機空槽 24B 回讀為
   `00 00 00 00 FF FF FF FF 00 00 00 00 00 80 FF FF 4F 0C 1F 00 00 00 00 00`
   → 判定空槽：UID 低 4 bytes（offset 4~7）全 `FF`。
6. **單一 master**：控制器同時間只接受一個連線；即時輪詢（worker）要寫入/回讀前必須先釋放
   自己的 1621 session，完成後再重連。
7. **合法卡刷卡可能不寫入事件**：本機實測「已註冊卡」正常刷 M11 有記錄；
   但某些設定下控制器只記 M24（開機）——務必在卡鐘後台確認「合法進出是否需要記錄」。
8. **權威對照工具**：`tools/soyal_proto.py`、`tools/punch_admin.py`（實機驗證過的封包組裝/解析）。
9. **2EH 姓名可回讀**（實機驗證）：16 bytes/筆、無 Source ID、空＝`FF`、
   以首個 `0x00` 結束；匯出 CSV 會一併帶出姓名（讀寫成對一致）。
10. **Mode 必須帶 Bit3＋Bit4（0x18）**：實機（含人臉模組）若 Mode 少了這兩個位元
    （例如只寫 `0x40`），刷卡會被當成「卡＋生物」多因子而**無法完成、不產生 M11**
   （面板顯示「影像 + 讀卡/密碼」）。以同一張卡 A/B 實測：`0x40`→無事件、`0x58`→M11。
    本專案 `AccessMode::mode_byte()` 已對三個模式分別加上 0x18。

---

## 12. 對應程式碼位置

| 功能 | 位置 |
| --- | --- |
| 封包組裝/解析、checksum、87H/84H/2EH | `src/punch_writer.rs` |
| 人員記錄結構（26B）、空槽判定 | `src/punch_writer.rs`（`build_user_record` / `is_empty_record`） |
| 回讀人員並匯出 CSV | `src/punch_writer.rs`（`read_users` / `users_csv`） |
| RTC 校時、事件拉取 worker | `src/ua.rs` |
| 事件碼對照 M00~ | `src/function_codes.rs` |
| 測試/驗證工具（權威） | `tools/soyal_proto.py`、`tools/punch_admin.py` |

---

## 13. 參考原始檔

- `硬體Protocol及範例/Protocol_881E_725Ev2_82xEv5 4V05.pdf`（本文件主要來源，2024/4/8）
- `硬體Protocol及範例/Message File structure.pdf`（事件訊息格式）
- `硬體Protocol及範例/SOYAL Protocol 靈活使用技巧_v221116-Final.pdf`
- `硬體Protocol及範例/721_727H Protocol_EN.pdf`、`401ROxDIx protocol EN.pdf`
