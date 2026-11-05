# Loops（每個段落的活現況檔）

> 一個段落一份，**實時更新**：每完成一小段就回寫現況，不停在起點。方法見全域 `start-work` skill。
> 段落邊界與依賴順序看 `docs/work-breakdown.md`；這裡只看「現在做到哪」。

## 規則

- **動手前先建一份**，填完再開始。起手：複製 `_TEMPLATE.loop.md` → 改名 `<slug>.loop.md`。
- `<slug>` 與看板卡、`feature/<slug>` 分支**共用同一個名字**。
- 主體上限 **200 行**（不含「已做」進度 Log）。主體破 200 ＝ 任務太大，先拆。
- 格式由 `./scripts/check.sh loops` 機器檢查：八個必要段落、200 行上限、開始／結束欄、
  必須被本索引收錄。**沒進索引的 loop 檔等於不存在。**
- 「已做」只寫**已完成**的段落，不寫打算做什麼（那是「下一步」）。
- 每個 loop 的「完成判準」必須含**陰性對照**：說得出「注入什麼壞情境，哪個檢查會變紅」。
  說不出來的完成判準是橡皮圖章。
- Token 欄位由 `./scripts/token-ledger.sh --slug <slug>` 產生，**不要手寫估算**。

## 索引

| Loop | 段落 | 狀態 | 優先度 | 目標 |
| --- | --- | --- | --- | --- |
| [src-layering](./src-layering.loop.md) | 整理 | 暫停（等 gateway-completion 整合） | P1 | 整理程式分層並將 SQL 移到獨立檔案 |
| [gateway-blueprint](./gateway-blueprint.loop.md) | W00 | 已交付（#20 留作範圍決策） | P0 | 把 92 節規格拆成 19 個可獨立審查的實作段落，並先確認可行性。卡 #20 |
| [client-compatibility](./client-compatibility.loop.md) | W01 | 已交付 | P0 | 查證哪些 Client 真能指向自架閘道，以及訂閱席次與 API 計費的邊界 |
| [workspace-foundation](./workspace-foundation.loop.md) | W02 | 已交付 | P0 | 讓資料庫可用，解開 W03／W04。卡 #25 |
| [identity-credential](./identity-credential.loop.md) | W03 | 已交付 | P0 | 一把金鑰對應一個人類看得懂的身分，可簽發、可撤銷、可輪替 |
| [usage-ledger](./usage-ledger.loop.md) | W04 | 已交付 | P0 | 每一筆請求與用量都落在 PostgreSQL，DB 短暫不可用也不會無聲遺失 |
| [anthropic-native](./anthropic-native.loop.md) | W05 | 已交付 | P0 | Claude Code 指向閘道就能用：串流即時、取消傳到上游、用量落帳 |
| [openai-native](./openai-native.loop.md) | W06 | 已交付 | P0 | Codex 與 OpenAI 相容 Client 指向閘道就能用，複用同一套串流骨架 |
| [observability-otel](./observability-otel.loop.md) | W07 | 已交付（部分移交） | P1 | 任何一次請求都查得到卡在哪一步、花多久、誰發的 |
| [admin-api-rbac](./admin-api-rbac.loop.md) | W08 | 已交付 | P1 | 後台權限在 API 層擋得住，每個控制面操作都留得下改前改後 |
| [admin-web](./admin-web.loop.md) | W09 | 已交付 | P1 | 打開頁面就能回答誰在用、用什麼模型、花多少、誰在排隊 |
| [model-registry-router](./model-registry-router.loop.md) | W10 | 已交付 | P1 | 換模型是改設定不是改程式；可指定模型也可只要品質等級 |
| [policy-engine](./policy-engine.loop.md) | W11 | 已交付 | P1 | 效力政策可預測、可解釋、可稽核，查得到「為什麼只給 standard」 |
| [quota-ratelimit](./quota-ratelimit.loop.md) | W12 | 已交付 | P1 | 超量會被擋或降級，而且擋的理由查得到是哪一條 |
| [scheduler-admission](./scheduler-admission.loop.md) | W13 | 已交付 | P1 | 互動工作不被背景餓死，背景工作也不會永遠排不到 |
| [canonical-layer](./canonical-layer.loop.md) | W14 | 已交付 | P2 | 協定轉換從 N×M 變 N+M，且現有行為一位元不變 |
| [cross-provider](./cross-provider.loop.md) | W15 | 已交付 | P2 | 跨協定用模型，能力轉不過去時明說而不是默默吞掉 |
| [providers-expansion](./providers-expansion.loop.md) | W16 | 已交付 | P2 | 加 Gemini／xAI／Local 不需要改 Core（改了就是抽象設計錯了） |
| [advanced-routing](./advanced-routing.loop.md) | W17 | 已交付 | P2 | Provider 變慢就換路，換路的理由與代價查得到 |
| [test-harness-load](./test-harness-load.loop.md) | W18 | 已交付 | P1 | 拿數字證明不互串、不漏、不爆、不餓死 |
| [process-lifecycle](./process-lifecycle.loop.md) | W19 | 已交付 | P0 | 部署不砍掉正在跑的串流，行程也退得掉 |
| [production-readiness](./production-readiness.loop.md) | W20 | 等待使用者決定（六條驗收完成；S04／S06／S07 需金鑰、告警接收端與授權） | P0 | 把已交付程式變成可持續使用、可回復、可回退且經真實 Client 驗證的服務 |
| [identity-groups](./identity-groups.loop.md) | W21 | S01–S03 有本機成果；S04 待整合，見總母檔 | P1 | 員工加 email、群組共用額度與權限、credential 啟停不刪除 |

## 現在該做哪一個

2026-11-05 已重查：唯一整體母檔為 [gateway-completion](../../agent-pool-gateway-completion/loops/gateway-completion.loop.md)，盤點與完成門檻見 [優先工作](../../agent-pool-gateway-completion/loops/gateway-completion/07-priority-audit.md)，接續指令見 [執行 prompt](../../agent-pool-gateway-completion/loops/gateway-completion/08-execution-prompt.md)。

優先順序：修復 #108 CI 並整合現有 Codex／群組成果 → 個人與群組憑證隔離及加密保存、可恢復備份 → 原 Client 全流程與發布。分層整理排在整合後。

W21 S01–S03 已有 groups 工作樹提交，不能再寫尚未開工；本工作樹的 W21 檔是舊副本，S04 尚待整合。W20 的舊等待理由須重查：現有 Codex 正式驗收紀錄不等於 W20 全部條件通過。公開版本為 63fe6d0bef32，遠端 main 為 71f746c，舊交接版本已過時。每次只接一個 writer、一張可驗收票，不重複啟動長任務。
