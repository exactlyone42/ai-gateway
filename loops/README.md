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
| [gateway-blueprint](./gateway-blueprint.loop.md) | W00 | 已交付（#20 留作範圍決策） | P0 | 把 92 節規格拆成 18 個可獨立審查的段落，並先確認可行性。卡 #20 |
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
| [policy-engine](./policy-engine.loop.md) | W11 | 未開始 | P1 | 效力政策可預測、可解釋、可稽核，查得到「為什麼只給 standard」 |
| [quota-ratelimit](./quota-ratelimit.loop.md) | W12 | 未開始 | P1 | 超量會被擋或降級，而且擋的理由查得到是哪一條 |
| [scheduler-admission](./scheduler-admission.loop.md) | W13 | 未開始 | P1 | 互動工作不被背景餓死，背景工作也不會永遠排不到 |
| [canonical-layer](./canonical-layer.loop.md) | W14 | 未開始 | P2 | 協定轉換從 N×M 變 N+M，且現有行為一位元不變 |
| [cross-provider](./cross-provider.loop.md) | W15 | 未開始 | P2 | 跨協定用模型，能力轉不過去時明說而不是默默吞掉 |
| [providers-expansion](./providers-expansion.loop.md) | W16 | 未開始 | P2 | 加 Gemini／xAI／Local 不需要改 Core（改了就是抽象設計錯了） |
| [advanced-routing](./advanced-routing.loop.md) | W17 | 未開始 | P2 | Provider 變慢就換路，換路的理由與代價查得到 |
| [test-harness-load](./test-harness-load.loop.md) | W18 | 未開始 | P1 | 拿數字證明不互串、不漏、不爆、不餓死 |
| [process-lifecycle](./process-lifecycle.loop.md) | W19 | 未開始 | P0 | 部署不砍掉正在跑的串流，行程也退得掉 |

## 現在該做哪一個

看 `docs/work-breakdown.md` 的依賴圖。關鍵路徑是
`W02 → W03/W04 → W05 → W06 → W14`；W00–W10 已交付，下一段是 W11。依前置條件
逐段推進 W11–W19，並回收 W07 的移交判準。

一次只推進一個段落到底，不要同時開四個分支——**收斂為目標，不是進度看起來多**。
