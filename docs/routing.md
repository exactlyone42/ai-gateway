# Passive provider routing

## Copilot Auto 對齊與超越目標（2026-10-27）

使用者要求：Copilot 有的選模能力我們都要有，並以實測證明更好。本節承接 Auto 選模討論；不把整個 Copilot 的編輯器、補全或雲端開發產品算成本次選模範圍。以下是新增交付要求，不代表已完成。

對照來源：[GitHub Auto 官方說明](https://docs.github.com/en/copilot/concepts/models/auto-model-selection)，2026-10-27 核對。官方公開的是行為，不足以推斷內部演算法。

| 必須對齊的能力 | 我們的交付與驗收 |
| --- | --- |
| 依任務難度與服務健康選模 | 分開判斷工作種類、難度與必要能力；簡單與複雜任務均用獨立案例驗證，不以模型名稱當能力證據 |
| 省錢、平衡、品質三種偏好 | 三種偏好調整成本、品質與等待時間的取捨，不能放寬權限、能力或資料限制；品質模式也不強迫簡單任務用大模型 |
| 避免不必要的模型切換與快取成本 | 保留對話選擇，在難度明顯變化或可安全切換的回合才重新評估；比較含快取失效的完整成本，不切斷串流或工具往返 |
| 不因提問語言而降低選模品質 | 同義的繁中、英文及混合語言任務成組測試，分別報告成功率與選模差異 |
| 管理者限制候選模型 | 能力、權限、資料地區及是否允許實驗模型先過濾，再交選模器；禁止的模型永遠不能入選 |
| 使用者知道實際用了哪個模型 | 原 Client 能呈現的地方顯示實際模型；後台可查原始要求、實際模型、改選理由、用量與成本，不能只顯示 Auto |
| 在各入口使用相同偏好 | 先驗現有三種 API 與原 Client，再由 Agent Platform 的 CLI、桌面共用設定；未測入口不能宣稱支援 |

GitHub 的訂閱範圍與折扣屬商業條件，不是我們能直接複製的程式能力；以使用者實付成本比較，不能自行承諾同折扣或取得未授權模型。

### 要超越的部分

- 本機與雲端共選；禁止外送的工作及其選模內容都留在本機。
- 每次選擇可解釋、可人工指定、可回復原模式；評分資料有版本與來源，未知成本明列未知。
- 複合工作依既有多模型規格拆解、分派及整合，每個子任務各自遵守預算、權限與資料限制；不能用多叫模型冒充品質提升。
- 以我們自己的真實任務改善決策；測試資料與調整資料分開，不用同一批題目調整後再宣稱勝出。

### 如何判定更好

先固定任務、輸入、可用工具、成功判準、版本與計價日期，再比較 Copilot Auto 三種偏好、Client 原指定模型及我們的新選模。每類至少 30 個獨立任務作初始門檻；樣本不足或差異不確定時維持未證明。相同任務跨方案配對，評審不看方案名稱，報告差異與不確定範圍；30 題本身不保證結論可信。

報告完成率、使用者等待時間中位數與第 95 百分位、每個成功任務的總成本。成本包括選模、拆解、整合、失敗、快取及可歸屬的本機開銷；拿不到的資料不得填零。只在成功率沒有可信退步且成本或等待時間至少一項有可信改善時，宣稱該類工作更好；不能外推成所有任務勝出。

保留失敗對照：移除能力過濾、放寬資料限制、永遠選最貴模型、每回合切模及忽略選模費用，對應檢查必須抓到問題。新增付費比較須另有費用授權；未取得實際 Copilot 結果前，只能稱內部驗證。

### 實作順序與目前邊界

1. 沿用任務分類與模型資料，先補固定比較案例、難度判斷、三種偏好與完整成本欄位；先旁路驗證，不直接啟用強制選模。
2. 補對話切換、語言一致性、資料地區與實驗模型政策、原 Client 的實際模型呈現，再逐入口驗收。
3. 單模型選擇達標後，沿用拆解、分派、整合的既有工作，不另建重複實作；分別比較新增成本與成功率。

2026-10-27 本次已讀正式版 `43d5c7c250eb` 的選路程式與正式設定：Jev 啟用，自建分類器有一筆旁路設定，拆解設定無資料列；分派與整合尚無實作。這是本次盤點，後續開工須重新讀回。上列對齊功能尚未逐項驗收，不能因既有路由測試通過就標完成。

Each logical request makes exactly one upstream Provider call. The repository
rule against repeated remote operations prohibits same-request retries and
fallback calls, including hand-written second attempts. A transient failure can
change the route of the next request, but never replays the failed request.

## Decision order

1. Model Registry fetches all capability-compatible candidates in one database
   query.
2. Policy removes candidates the principal may not use.
3. The in-process Routing Engine takes one immutable snapshot and orders the
   remaining candidates by enabled passive signals.
4. Quota remains the sole authority for hard limits and reservations. When
   quota-aware routing is enabled, a soft limit may choose the next candidate.
5. Scheduler decides when the selected route can execute. It never changes the
   Provider or model.

The stable Registry order is the final tie-breaker. Unknown health/latency does
not displace a measured healthy route. Provider state is bounded to the profiles
loaded at startup. Every started request gets a monotonic Provider generation;
an older request completing late cannot overwrite a newer result.

## Switches

| Variable | Default | Effect |
| --- | --- | --- |
| `ROUTING_HEALTH_AWARE` | `true` | Prefer fewer transient failures, then lower measured latency |
| `ROUTING_LOAD_AWARE` | `true` | Prefer fewer active requests |
| `ROUTING_COST_AWARE` | `false` | Prefer lower declared token rates; unknown price sorts last |
| `ROUTING_QUOTA_AWARE` | `true` | Permit soft-quota routing; hard quota always remains enforced |
| `ROUTING_AUTO_QUALITY` | `true` | Permit passive signals to cross quality tiers |
| `ROUTING_FAILURE_COOLDOWN_MS` | `30000` | Half-open one failed route for reevaluation after this interval |

These switches affect ordering independently. Disabling quota-aware routing does
not disable quota enforcement; it changes a soft-limit outcome from routing to
deferred.

## Health and audit

HTTP 408, 500, 502, 503, 504, transport unavailability, body transport
failure, and explicit Provider failure are transient health signals. A completed
request clears the consecutive-failure score. Cancellation does not penalize a
Provider because it may be caused by the client. HTTP 429 is returned to the
caller but does not open the Gateway circuit: the Provider owns its rate limit,
and one rejection must not turn later requests into Gateway-generated 503s.

The durable policy snapshot stores `routing.attempt_kind=initial`, the selected
reason (`registry`, `health`, `load`, `cost`, `quota`, or `quality`), and
`same_request_retry=false`. Admin Request Detail exposes this as Routing History.
There is exactly one Provider attempt for each request; no stream reconnect,
usage lookup, timeout, tool continuation, or ambiguous result makes a second
remote call.
