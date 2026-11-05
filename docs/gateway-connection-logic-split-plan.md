# 連線處理與業務邏輯拆分草案

目標是讓接住 Client 串流的容器，與執行身分、權限、配額、模型選擇及記帳的容器分開部署、分別擴充。這是待確認的方案，尚未實作，也尚未證明多容器容量。

Essence 的 `crates/gateway/src/infra/me_tcp.rs` 實際以連線池傳送命令；`crates/matching-engine/src/infra/tcp_server.rs` 在另一程序接收命令，再交給有界佇列；`crates/gateway/src/ws/user_event_router.rs` 把結果送回對應使用者的連線。借用這個程序邊界，不直接搬交易命令、廣播及無限重連行為。

Gateway 現有 `src/services/streaming.rs` 直接產生 HTTP 回應本體，`src/services/request_policy.rs` 依賴 HTTP 標頭，尚未達成業務邏輯獨立。另一工作樹的啟動路徑在開啟排程時取得單實例鎖，不能直接多開業務容器或關閉鎖後宣稱安全。

## 工作順序與驗收

| 工作 | 依賴 | 完成後能驗證什麼 |
| --- | --- | --- |
| 1. 分離業務輸入、輸出與串流事件 | 確認整合基準及 503 修復歸屬 | 業務服務不用 HTTP 型別；原 Client 行為、權限及交易邊界不變 |
| 2. 分開執行連線服務與業務服務 | 1 | 多個連線容器接同一業務服務；回應、錯誤與取消不互串，不額外送出模型請求 |
| 3. 處理多個業務副本的共用狀態 | 2 | 同一配額不重複核發、記帳不重複；個別副本退出後可恢復，保存不遺失 |
| 4. 固定設備規格比較容器數 | 2；業務副本比較另依賴 3 | 比較 1、2、4 個連線容器及業務副本，交付成功承載量、資源與部署成本估算 |

內部介面優先評估沿用 HTTP 串流及連線池；若選用能共用底層連線的 HTTP/2，須先證明取消、慢 Client 及連線故障不影響其他請求，再定案。不為模仿 Essence 預先加入訊息代理或自訂 TCP 協定。內部介面必須驗證呼叫者；供應商憑證只留伺服器端，不能送回 Client。

擴充必須一併定義配額預留、排程容量、身分撤權及加密保存的擁有者與恢復方式。每個副本使用獨立且持久的帳本暫存位置，不直接共用帶有排他鎖的目錄。未完成共用狀態前只允許單一業務副本，不以停用門檻繞過驗收。

測試只使用假模型串流，固定每個容器的 CPU、記憶體與流量條件，分開量入口、連線服務、業務服務、資料庫及保存負擔。包含單容器對照、移除一個副本、Client 取消、慢速讀取、跨副本配額競爭與保存失敗；不能把容器增加兩倍當成容量增加兩倍。正式設備建議須另含故障時餘裕。

503 修復仍由既有 `feature/remove-failure-cooldown` 工作樹處理；本方案不覆蓋它的未提交修改。正式流量、憑證及資料不拿來壓測；不建立付費雲端資源。原有全面 SQL 整理與其他未完成功能不併入本次拆分。

## 圖與來源

- [白底圖](diagrams/connection-logic-split/gateway-split-clean.png)、[暖色圖](diagrams/connection-logic-split/gateway-split-sketch.png)、[深色圖](diagrams/connection-logic-split/gateway-split-blueprint.png)：三版內容相同，均為目標示意。
- [Axum 串流回應本體](https://docs.rs/axum/0.8.6/axum/body/struct.Body.html)、[Reqwest 串流讀取](https://docs.rs/reqwest/0.12.24/reqwest/struct.Response.html)：實作前須再按鎖定版本核對。
- [ECS 服務擴充](https://docs.aws.amazon.com/AmazonECS/latest/developerguide/service-auto-scaling.html)、[PostgreSQL 17 鎖](https://www.postgresql.org/docs/17/explicit-locking.html)：部署與共用狀態的研究來源，不代表已完成雲端部署。

目前僅完成原始碼核對與方案；未修改執行程式、未跑新拆分測試、未部署正式環境。先前隔離單容器測試已停止，不當作本方案的多容器證據。
