# LOOP: claude-first-byte-stall

> 卡 #81｜分支 `feature/claude-first-byte-stall`｜段落 `W21`

| 欄位 | 值 |
| --- | --- |
| 狀態 | 進行中 |
| 優先度 | P0 |
| 前置 | claude-subscription-oauth |
| 開始 | 2026-10-21 15:00+08:00 |
| 結束 | — |
| Token | 由 `./scripts/token-ledger.sh` 填，不要手寫 |

## 目的
Claude Code 經 Gateway 執行長工作時，不再因 Gateway 路徑多出的首筆回應卡頓而反覆 60 秒取消並以 499 結束。

## 完成判準
- [ ] 同一固定長請求的直連與 Gateway 路徑都有去敏分段時間，能指出差異在哪一段。
- [ ] 分開解釋共同的 60 秒慢回應，以及 Gateway 額外造成的重試品質損失。
- [ ] 最小修補不記 prompt、response、OAuth token 或完整 body。
- [ ] 自動測試涵蓋受影響契約；修正前的壞行為會讓測試失敗。
- [ ] 真實 Claude Code 長文件工作經 Gateway 產出兩份目標檔，且無 60 秒取消或 499。
- [ ] 格式、靜態檢查、測試全過並合進 `dev`；未授權前不部署。

## 已知
- 14:20–14:35 正式日誌有 14 筆請求約 60 秒後由 Client 丟棄；Claude Code 重試 10 次後回 499。
- 正式版本 `90cb105a84a9`；延長 Client 等待不能解釋直連與 Gateway 的品質差異。
- `src/adapters/provider.rs` 篩選上游標頭；`src/routes/messages.rs` 收到上游回應標頭後才建立 Client 回應，兩者需以對照確認。

## 已做
- `2026-10-21` 建立 #81，加入 Project #2，設為 P0／Client integrations／In progress。
- `2026-10-21` 從最新 `dev` 建立隔離工作樹與分支。
- `2026-10-21` 同題直連也出現 60 秒重試，證明慢回應不是 Gateway 單獨造成。
- `2026-10-21` 實際 Claude Code 2.1.306 請求帶 session 與 retry 標頭；Gateway 原本會刪除。
- `2026-10-21` 先寫失敗測試，再只放行三個重試識別標頭；本機真實 Client 往返通過。

## 已確定不可行
- 單純把 Client 等待由 60 秒改成 600 秒：只掩蓋卡頓，不是根因修復。

## 證據
- `run2.log`：32 turns、10 次重試、最後 499；證明失敗形狀，不證明根因。
- `journalctl -t agent-gateway-jev`：14 筆約 60 秒 Client cancellation；證明 Client 離開，不證明上游為何未及時回應。
- `run-direct.log`：同一題直連亦出現連續 60 秒重試；推翻「60 秒只由 Gateway 造成」。
- `anthropic_preserves_claude_code_retry_identity_headers`：修正前因標頭不存在而失敗，修正後通過，任意 `x-*` 仍被擋。
- Claude Code 2.1.306 本機 Gateway 往返：去敏擷取確認 session 與 retry 標頭抵達上游。
- PR #61／#62：OAuth 路徑已接通；本任務不重做 OAuth。

## 來源
- <https://docs.anthropic.com/en/docs/claude-code/llm-gateway>，查證 2026-10-21。
- `docs/feasibility.md`、`docs/provider-protocols.md`、`src/adapters/provider.rs`、`src/routes/messages.rs`。

## Trade-off
| 決策 | 選了什麼 | 放棄了什麼 | 為什麼 |
| --- | --- | --- | --- |
| 診斷 | 同請求逐段時間與去敏契約差異 | 猜逾時值 | 找出 Gateway 新增的品質損失 |
| 修補 | 根因所在的最小一層 | 重寫代理 | 降低 OAuth、串流與帳務回歸風險 |

## 下一步
1. 等同題直連完成或達 20 分鐘上限，保存最終重試與產物結果。
2. 在不動正式環境下完成完整檢查與差異審查。
3. 修正版部署需另獲授權；部署後跑真實 Gateway 長文件驗收。
