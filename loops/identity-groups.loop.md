# LOOP: identity-groups

Protocol version: `1.2.0`

> 分支 `feature/identity-groups`｜段落 `W20`（W03 identity-credential 的延伸）

## 目標與範圍

- 使用者／情境：公司管理者透過 Admin UI 管理員工與群組，統一分配 AI 使用額度與權限
- 問題與整體成果：
  1. 員工只有 `display_name`，公司人多名字可能重複，無法唯一辨識
  2. 沒有群組概念，無法讓一組人共用額度和權限
  3. Credential（Key）只能刪除，不能暫時停用再恢復（已有 status 機制，但 Admin UI 未暴露完整操作）
- 範圍內：DB schema + Rust 後端 CRUD + Admin UI 對應頁面
- 不做：群組之間的繼承或巢狀群組、跨 tenant 群組、群組層級的 credential 自動派發
- 最新使用者決策：2026-10-08，email 為可選欄位但建議填寫

## 整體驗收

- [ ] A1 users 表支援 email（可選、tenant 內唯一），Admin UI 可建立/編輯帶 email 的 user
- [ ] A2 可建群組、加/移成員；群組可綁 policy 和 quota，成員自動繼承
- [ ] A3 Credential 可透過 Admin UI 啟用/停用（不刪除），狀態切換後即時生效
- [ ] A4 Policy/Quota 解析時，先查 user scope，無命中再查 user 所屬群組 scope（群組 fallback）
- [ ] A5 現有測試全過、新功能有整合測試含陰性對照

## 計畫

| step | 成果 | 驗收條件 | 對應整體驗收 | 依賴 | 狀態 | step LOOP |
| --- | --- | --- | --- | --- | --- | --- |
| S01 | DB migration：users 加 email/status；建 groups + group_members 表 | migration 跑過、約束正確（email tenant 內唯一、status enum、FK 完整） | A1, A2 | 無 | PENDING | 無 |
| S02 | DB migration：policies 和 quotas 加 group_id FK | scope 唯一索引和 CASE 擴充 group 層級；migration 跑過 | A2, A4 | S01 | PENDING | 無 |
| S03 | Rust 後端：admin resources 支援 groups CRUD + 成員管理 | API 可建立/列表/更新/刪除群組；可加移成員；有審計紀錄 | A2 | S01 | PENDING | 無 |
| S04 | Rust 後端：users CRUD 支援 email 欄位 | create/update user 接受 email；email 重複回 409 | A1 | S01 | PENDING | 無 |
| S05 | Rust 後端：policy/quota 解析加入 group fallback | 請求帶 user_id → 查 user scope → 無命中 → 查 user 所屬 groups → 合併 | A4 | S02, S03 | PENDING | 無 |
| S06 | Rust 後端：credential 啟停 API 完善 | disable/enable credential 正常運作、cache 即時失效 | A3 | 無 | PENDING | 無 |
| S07 | 整合測試 | 涵蓋 email 唯一性、群組配額繼承、credential 啟停、陰性對照 | A5 | S03–S06 | PENDING | 無 |
| S08 | Admin UI：user 清單加 email 欄位與搜尋 | 建立/編輯 user 可填 email；清單可按 email 搜尋 | A1 | S04 | PENDING | 無 |
| S09 | Admin UI：群組管理頁面 | 群組 CRUD + 成員管理介面 | A2 | S03 | PENDING | 無 |
| S10 | Admin UI：credential 啟停切換 | 一鍵切換 active/disabled，畫面即時反映 | A3 | S06 | PENDING | 無 |
| S11 | 全套驗收 + check.sh all | 格式化 + clippy + 測試全過；dbb 部署驗證 | A1–A5 | S07–S10 | PENDING | 無 |

## 目前狀態（LOOP 投影）

- task 狀態／目前 step：PLANNED／S01
- 已證明：現有 users 表無 email/status 欄位（migration 20260910160000）；quotas/policies 的 scope 用 CASE + COALESCE 唯一索引，擴充 group 層級需改索引
- 未證明：group fallback 對 policy 合併的效能影響；Admin UI 的群組成員管理交互
- 阻塞／等待條件：無
- 下一步：建立 feature/identity-groups 分支，從 S01 開始寫 DB migration
- checkpoint JSONL：loops/identity-groups.checkpoints.jsonl
- protocol version：`1.2.0`

## 所有權與停止條件

- 工作目錄／分支／卡／PR：`/home/cash/work_space/private/agent-pool-gateway`、`feature/identity-groups`、無卡、無 PR
- owner 與 lease：無（單 session）
- 喚醒方式與狀態：由 /loop 動態模式驅動
- 停止條件與授權邊界：A1–A5 全過為 COMPLETE；可自行合 dev，合 main 需 CI 全綠 + 驗收全過
- 證據檔／歷史位置：loops/identity-groups.checkpoints.jsonl
