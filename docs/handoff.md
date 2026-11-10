# 交接：gateway-completion 現況與接手步驟

盤點時間：2026-11-10 21:33（Asia/Taipei）。範圍：本 repo 的 `dev`（c6b1772）、正式服務、GitHub PR／CI、本機容器與工作樹。
這份只寫「現在能用什麼、還剩什麼、從哪開始」。逐輪流水帳在 `loops/gateway-completion.checkpoints.jsonl`，更詳細的最新敘述在另一個工作樹的 `HANDOFF.md`（見「從這裡接手」第 2 步）。

## 整體工作量與進度

範圍定義：母檔 `loops/gateway-completion.loop.md`（dev 版）要完成的 6 個子檔 01–06。不含：已交付的 W01–W20、S11–S15（任務拆解那條線）、`agent` 另一個 repo 的 Agent 平台、PR #106／#94。這些在別處管理，本次未逐一查證。

估算日期 2026-11-10。依據：各子檔驗收清單的相對大小與剩餘缺項，讀自 `loops/gateway-completion/01–07*.md` 與 checkpoint 第 169 筆。**這是工作量估計，不是精確工時，誤差約 ±10–15 個百分點。** 「已完成」只計有隔離環境實測紀錄的項目；正式環境、真人、跨主機、真實供應商帳號未驗的部分一律算剩餘。我沒有重跑這些紀錄裡的測試。

| 子檔 | 權重 | 已完成 | 剩餘 | 證據與阻擋 |
| --- | --- | --- | --- | --- |
| 01 兩圖與現況對照 | 5% | 4% | 1% | 對照表已寫，但依舊版 d1c29e8 的程式寫成，要照現行 dev 重核 |
| 02 個人與群組憑證 | 20% | 12% | 8% | Claude Code 與 Codex 各 7 項個人／群組隔離在隔離環境通過；缺第二位真人、跨主機、群組權杖整合收尾（S04） |
| 03 模型與協定 | 15% | 8% | 7% | 本機 Ollama 真模型經 Codex 完成工具往返；本人 Codex 訂閱短答通過；OpenAI／Gemini／xAI 沒有金鑰，只有模擬測試；沒看到 vLLM；Claude 本機路線未驗 |
| 04 加密保存、備份、還原 | 30% | 18% | 12% | 隔離環境已驗：先存後送、備份還原、備份強退後自動釋鎖、同批額度加總修正、歷史額度 148 在新副本恢復為 4。缺：正式環境最低權限與持久保存設定、獨立見證回退、S4 正規交付登記（BLOCKED）、一般排隊遇資料庫失聯 30 秒仍被截流（檢查為 fail）、R1／R2、被略過的測試 |
| 05 Client 連線 | 15% | 8% | 7% | Claude Code 2.1.318 與 Codex 0.166.0 對假供應商完成工具往返與取消；缺真來源、遠端 Gateway、VS Code 等其他 Client（此機找不到啟動指令）、自家 Agent 摘要驗收 |
| 06 整合、部署、驗收 | 15% | 3% | 12% | dev 的 CI 綠、目錄閘門已併入；缺 dev→main 發版、測試環境部署、公開版本上原 Client 實送、回退演練、工作樹與舊容器清理 |
| 合計 | 100% | 53% | 47% | 已完成 + 剩餘 = 100% |

## 現在能做什麼

- 正式服務 `https://gateway.exactlyone.dev/health` 回 `revision 153ac60b58b3`、`status ok`（2026-11-10 21:33 實測）。它對應 `main` 的 #112，`src`、`migrations`、`web`、`Cargo.toml` 與現在的 main 沒有差異（`git diff 153ac60..origin/main` 實測為空）。
- `dev`（c6b1772）比 main 多 70 個提交、571 個檔案：含加密保存與備份、群組憑證與管理畫面、額度修正、目錄結構檢查。GitHub CI 對 c6b1772 成功（2026-11-10 09:04Z）。**這些都還沒部署。**
- 隔離環境的驗收原件在 `~/.local/state/ai-gateway/r2-*`、`r3-*`（`ls` 實數 20 個資料夾；最後一輪的 `r3-backup-kill-20261110/summary.json` 為 checkpoint 169 所引）。

## 遺留與停點

| 項目 | 影響 | 下一個動作／誰解除 |
| --- | --- | --- |
| dev 未發版到 main，正式仍是舊程式 | 使用者拿不到 dev 上的新功能 | 母檔判準未全綠，不合 main。先做完上表剩餘項，再依授權發版 |
| 母檔判準 A1–A6 與 01–06 尚未全綠 | 不能宣稱整體完成 | 依 checkpoint 169 的下一步（見接手第 4 步） |
| `feature/identity-groups` 有 8 個提交不在 dev | 最新敘述（R3-S5）只在該分支的 `HANDOFF.md` | 記憶紀錄為併入有 21 檔衝突（2026-11-10 00:45，我沒有重跑）；要在 dev 上開 `feature/merge-identity-groups` 逐檔解並跑 `./scripts/check.sh all` |
| 規則衝突：`CLAUDE.md` 寫「合 main 才部署」，`AGENTS.md` 寫「合入 dev 立即部署」 | 部署時機有兩種說法 | 需 repo 擁有者擇一並改另一份，原因無紀錄 |
| PR #108（→main）CLEAN、rust 檢查成功；PR #116（→main）檢查 sync、rust 失敗 | 兩者的內容已在 dev，對 main 開著容易誤合 | #116 失敗原因：log 只見到結束碼 1，未查到具體訊息。發版後關閉或改目標，需本人決定 |
| 容器 `apg-gateway-1` 一直重啟（2254 次） | 吃資源、洗日誌，但不是正式服務 | 日誌：找不到資料庫主機名稱。`apg-postgres-1` 讀回沒有任何網路。屬 compose 專案 `apg`（`/home/cash/work_space/private/agent-pool-gateway/deploy`），誰建的原因無紀錄。停或刪需授權 |
| 3 個舊版閘道容器還在跑（:18101 63fe6d0、:18102 a46d955、:18088 d1c29e8） | 回滾用，也占資源 | 發版驗收後才可處理，刪除需授權 |

## 從這裡接手

1. 工作目錄 `/home/cash/orca/workspaces/agent-pool-gateway/silverside`，分支 `DnaBoss/chore-recall`（已快轉到 dev c6b1772，加上這份未提交的文件）。成功判準：`git log --oneline -1` 顯示 c6b1772。
2. 讀 `/home/cash/work_space/private/ai-gateway/HANDOFF.md` 最上面兩節（該工作樹在 `feature/identity-groups`）。成功判準：你能說出 R3-S5 剛驗完什麼、還缺什麼。
3. 先確認沒有別人正在寫：母檔要求單一寫入者。本機同時有多個 `codex --no-daemon` 與 `claude` 程序在跑（例如 PID 59760、127892、128109、2531732、2611618），哪個是本任務的監督者未查證；用 Offora 辦公室或直接問使用者，不要自行終止。
4. 下一步照 checkpoint 169：有界、唯讀地查正式與本機部署設定，找出「最低權限、持久保存、見證回退、S4 正規交付登記」各自要改的最小內容，先做隔離可驗的部分；不自動重算正式資料。之後依序：其餘 Io、R1／R2、同版兩個原 Client 用合法真來源實送、測試環境與公開版本驗收。
5. 開新工作：`git fetch` 後 `git switch -c feature/<slug> origin/dev`，新遠端分支靠 `gh api` 建 ref（pre-push 規則寫死 `origin/develop`）。迭代跑 `./scripts/check.sh gate`，PR 前跑 `./scripts/check.sh all`。

## 現場整理

- 已整理：把這個工作樹從 main（68d7569）快轉到 dev（c6b1772），分支沒有獨有提交，沒有丟東西；新增本文件並在 `CLAUDE.md`、`AGENTS.md` 的文件索引補入口；舊 `HANDOFF.md` 備份在 `~/.local/state/recall/reports/recall-handoff-20261110-213311-aigw/before-identity-groups-HANDOFF.md`。
- 刻意保留：`/home/cash/work_space/private/ai-gateway` 工作樹（乾淨，在 `feature/identity-groups`，沒動）；4 個閘道容器（正式 :18105 與 3 個舊版）；`apg-gateway-1`；所有 `codex`、`claude` 程序；所有 `feature/*` 本機分支。
- 待授權：停止或移除 `apg-gateway-1`；移除 3 個舊閘道容器；清理已合併分支。目前都沒做。
- 本次沒有提交、推送、合併、部署，也沒有動正式環境。

## 驗證方式

- 已跑（唯讀）：`curl` 正式與本機 `/health`、`docker ps`／`inspect`／`logs`、`gh pr`／`gh run`、`git merge-base`／`git diff`；文件寫完後跑 `./scripts/check.sh layout` 與 `loops`。
- 沒跑：`cargo` 測試、`check.sh all`、CI 重跑、任何正式環境操作。上表「已完成」來自子檔與 checkpoint 紀錄，沒有重驗；紀錄稱被略過的測試是 Rust 6 項、Python 20 項（未查證現在的數字）。
- 成功判準：`/health` 的 `revision` 等於預期提交；`check.sh all` 結束碼 0；原 Client 實送回 200。

## 已決定，不要重做

- 開發分支一律從 `dev` 切，合回 `dev`；`main` 只在發版時合。
- 不重送已完成的 660 秒長請求；不另建平台或平行寫入者；不用健康頁、收據、模擬來源代替真實驗收。
- 不盲目解 identity-groups 的 Rust 衝突，要在 dev 上開分支、編譯並跑測試後再取捨；母檔以 identity-groups 版為準。
- 沒有金鑰的來源（OpenAI、Gemini、xAI）、第二位真人、跨主機一律標未驗，不能用模擬補。

## 查證資料（各項分開，互不代表）

- Git：origin/dev=c6b1772、origin/main=68d7569，dev 領先 70 個提交（本機 `git rev-list` 實測）。
- PR：#117 已合 dev；#116、#108 開著、base 為 main；#106 有衝突；#94 無衝突（`gh pr list`）。
- CI：dev c6b1772 成功；#116 失敗（原因未查到）；其餘 PR 見 `gh pr view`。
- 部署：正式 :18105 → revision 153ac60b58b3，反向代理設定讀自 caddy 容器的 Caddyfile。
- 真人驗收：無紀錄。
