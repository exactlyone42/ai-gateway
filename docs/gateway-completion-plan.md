# Gateway 收尾計畫

用途：`loops/gateway-completion.loop.md` 的各 step 細節。順序與狀態以 loop 為準。

## 新增產品要求（2026-10-27）

自動選模須對齊 Copilot Auto，並以同任務比較證明改善；完整能力清單、實作順序與勝出判準見 [選模要求](routing.md#copilot-auto-對齊與超越目標2026-10-27)。這是新增待交付範圍，既有 S11 的旁路完成及本計畫原有收尾完成都不代表這項要求已完成。先補單模型選擇與比較證據，再接 S07、S08 的分派整合；保留既有未提交工作與安全限制。

## 前提
- 線上容器跑 `keyless-c8158e5d2089`，只存在本機分支 `feature/optional-provider-keys`（`a5ea96d`、`c8158e5`，作者 Policy Test）。
- 遠端沒有 `dev`，以 main 為底。
- 擱置：W20 金鑰、告警、離機備份、自動部署執行器。
- 改程式的 step 標 DONE 前，過 `~/.agents/skills/cash-rule/references/simplicity-review.md`。

## S01 修主 repo git 設定
1. 讀 `.git/config` 與 `.git/repo-policy/20261001T031647933551Z`，查不到來源就記「未查證」。
2. 備份 `.git/config`，`bare` 改 `false`，作者改回使用者平常值；hooksPath 若是守衛就保留。
3. 驗收：`git status` 正常、作者正確。

## S02 版本入庫並由 main 部署
1. 從 `origin/main` 重新套用兩個 commit。
2. 比對正式資料庫 migration 紀錄與 main 版，差異只應是兩行 `AND role IN ('admin', 'operator')`；不符先查，不重送。
3. 隔離資料庫跑 `check.sh all`、簡化審查。
4. 開 PR、CI 全綠就合，從 main 部署，`/health` revision 等於合併 commit。
5. 陰性對照：main 舊 migration 套正式資料，啟動應失敗。
6. 原 Claude Code 實送 `/v1/messages` 回 200。
7. 保留舊映像供回滾。

## S03 W21 驗收
確認含心跳程式；Claude Code 跑長文件工作產出兩份檔；日誌無 499、無約 60 秒取消；更新 `claude-first-byte-stall.loop.md` 並關 #81。陰性對照：拿掉心跳，`anthropic_stream_sends_heartbeat_before_delayed_provider_headers` 必須失敗。

## S04 失敗測試
隔離資料庫連跑 `identity::tests::lifecycle_is_tenant_safe_and_revocation_beats_the_cache` 10 次；失敗就追 502 來源並修根因，不放寬測試；全過記「未重現」。

## S05 讀回
S12 讀回後台事故清單與詳情；S09 先查本機憑證是否可用，再驗 Jev 失敗時 60 秒內完成。

## S06 S13 shadow
補三協定等價測試與機密掃描；部署 shadow；先定最低樣本數，再讀 `/admin/v1/task-decomposition-shadow`；樣本不足停在 MONITORING。

## S07、S08
S06 凍結契約後，依各自 loop 實作，本計畫只追狀態。

## S09 identity-groups
先改段落編號（W21 重複）、同步 main，再依該 loop 做 migration、政策配額、後台。

## S10 收尾
回寫各 loop 與 README，`check.sh loops` 全綠，本 loop 標 COMPLETE。

## 交付點
S02、S06 為交付點，見 `loops/gateway-completion.delivery.json`；雜湊到 VALIDATING 時填。
