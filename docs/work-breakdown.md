# 工程拆解

整份規格切成 **19 個段落**（W19 是反方審查補上的缺口）。切的依據是**可獨立審查**：一個段落 = 一張卡 = 一份 loop 檔 =
一支 `feature/<slug>` 分支 = 一個能單獨驗收的成果。

段落之間只透過**已合進 `dev` 的介面**相依，不共用未合併的分支。

> 現況與進度看 `loops/README.md`，不看這份。這份只定義邊界與順序。

## 依賴順序

```text
W01 client-compatibility ──────────────┐  (研究，可與 W02 並行)
                                       │
W02 workspace-foundation ──┬───────────┼──→ W07 observability-otel
                           │           │
                           ├─→ W03 identity-credential ──┬──→ W08 admin-api-rbac ──→ W09 admin-web
                           │                             │
                           ├─→ W04 usage-ledger ─────────┤
                           │                             │
                           └─→ W10 model-registry-router ┴──→ W11 policy-engine ──→ W12 quota-ratelimit
                                                                                          │
                     W03,W04 ──→ W05 anthropic-native ──→ W06 openai-native                │
                                        │                      │                           ▼
                                        └──────────┬───────────┘                  W13 scheduler-admission
                                                   │                                       │
                                                   ▼                                       ▼
                                          W14 canonical-layer                     W18 test-harness-load
                     W05 ──→ W19 process-lifecycle
                                                   │
                                    ┌──────────────┴──────────────┐
                                    ▼                             ▼
                            W15 cross-provider          W16 providers-expansion
                                                                  │
                                                                  ▼
                                                          W17 advanced-routing
```

**關鍵路徑**：`W02 → W03/W04 → W05 → W06 → W14`。其它段落都掛在旁邊，可並行。
`W19 process-lifecycle` 掛在 W05 之後（要有真的長命串流才驗得出排空）。

**兩個先於實作的決定**（拖到寫程式時才問就來不及）：

1. **單實例還是分散式計數器**（屬於 W19）。`docs/architecture.md` 原本寫「至少兩個實例」，
   而 W12 寫「不做分散式計數器」——兩個實例各自記憶體計數，等於所有上限實際上是兩倍，
   而且**沒有任何錯誤訊息**。第一版的答案是**單實例**，理由見 `docs/task-ledger.md`。
2. **要不要自建 Admin 認證**（屬於 W08）。取決於採不採用第一方閘道，見 `docs/feasibility.md` 第四節。

## 段落定義

### Phase 0 — 前置

| 編號 | Slug | 優先度 | 前置 |
| --- | --- | --- | --- |
| W01 | `client-compatibility` | P0 | 無 |

**目的**：回答「哪些 Client 真的能指向自架閘道」，以及「訂閱席次和 API 計費的邊界在哪」。
**交付**：`docs/feasibility.md`、`docs/client-compatibility.md`，每條結論附官方 URL ＋ 查證日期 ＋
「這證明什麼、證明不了什麼」。查不到官方來源就標 `unknown`，不推理補洞。
**為什麼要先做**：這是整個架構的可行性前提。某個 Client 不支援就標 unsupported，**不阻塞其它段落**。

| 編號 | Slug | 優先度 | 前置 |
| --- | --- | --- | --- |
| W02 | `workspace-foundation` | P0 | 無 |

**目的**：一個指令就能跑起 Postgres ＋ 閘道 ＋ 可觀測性後端，且 crate 邊界立好。
**交付**：單 crate 拆成 workspace（`gateway-server` / `gateway-core` / `gateway-auth` /
`gateway-policy` / `gateway-scheduler` / `gateway-routing` / `gateway-accounting` /
`protocol-*` / `provider-*`）；設定集中一處；錯誤統一出口；`/health` 與 `/ready`；
`scripts/check.sh` 進 CI；`deploy/docker-compose.yml`；migration 綁在啟動流程。
**不做**：任何 Provider 呼叫邏輯。

### Phase 1 — 資料面最小垂直切片

| 編號 | Slug | 優先度 | 前置 |
| --- | --- | --- | --- |
| W03 | `identity-credential` | P0 | W02 |

**目的**：一把金鑰對應一個人類看得懂的 Principal（人／設備／Client／Agent），可簽發、可撤銷、可輪替。
**交付**：`users` `devices` `clients` `agents` `credentials` schema；金鑰只在建立當下顯示一次、
DB 只存雜湊 ＋ 前綴 ＋ 指紋；`PrincipalContext` 解析中介層；有上限的憑證快取（未命中時 fail closed）。
**不做**：Admin UI、政策合併。

| 編號 | Slug | 優先度 | 前置 |
| --- | --- | --- | --- |
| W04 | `usage-ledger` | P0 | W02 |

**目的**：每一次請求、每一次 Provider 嘗試、每一筆用量都落在 PostgreSQL，且 DB 掛掉不會無聲遺失。
**交付**：`gateway_requests` `provider_attempts` `usage_records` `audit_logs` schema；
邏輯請求與 Provider 嘗試分開兩張表；有上限的持久化 outbox（不是無上限記憶體佇列）。
**不做**：報表查詢介面（那是 W09）。

| 編號 | Slug | 優先度 | 前置 |
| --- | --- | --- | --- |
| W05 | `anthropic-native` | P0 | W03, W04 |

**目的**：Claude Code 指向閘道就能正常工作，串流不卡、取消會傳到上游、用量有落帳。
**交付**：`POST /v1/messages`、`POST /v1/messages/count_tokens` 直連 Anthropic（**不經 LiteLLM**）；
SSE 增量轉送（不得整包 buffer）；Client 斷線 → 取消上游；時間戳欄位全部開好
（收到／進佇列／離佇列／送上游／上游首字／Client 首字／完成，其中兩個 queue 欄位在 W13 之前寫 NULL）；
`GET /v1/models` 由**設定檔**供應、contract 在此定死（W10 只換來源）；
estimated 與 provider-reported 用量分開記。
**不做**：跨 Provider 轉換。

| 編號 | Slug | 優先度 | 前置 |
| --- | --- | --- | --- |
| W06 | `openai-native` | P0 | W05 |

**目的**：Codex 與 OpenAI 相容 Client 指向閘道就能正常工作。
**交付**：`POST /v1/responses`、`POST /v1/chat/completions`、`GET /v1/models` 直連 OpenAI；
串流／取消／落帳與 W05 同一套骨架。
**為什麼排在 W05 之後**：串流與取消的骨架先在一個 Provider 上做對，再複用；兩個一起做會把兩邊的
協定細節混進同一次除錯。

| 編號 | Slug | 優先度 | 前置 |
| --- | --- | --- | --- |
| W07 | `observability-otel` | P1 | W02 |

**目的**：任何一次請求都查得到「卡在哪一步、花多久、誰發的」。
**交付**：OTLP trace／metric／log 三路；span 樹（`gateway.request` → `auth.resolve` →
`policy.evaluate` → `token.estimate` → `scheduler.queue` → `router.select` → `provider.attempt`）；
metric 命名與 cardinality 規則（`request_id` 這類只進 trace／log，不當 metric label）；
結構化 JSON 日誌 ＋ 敏感值遮蔽。
**約束**：閘道只認 OTLP，不綁任何後端。SigNoz 只是開發環境的預設選擇。

### Phase 2 — 控制面

| 編號 | Slug | 優先度 | 前置 |
| --- | --- | --- | --- |
| W08 | `admin-api-rbac` | P1 | W03, W04 |

**目的**：後台的權限在 API 層擋得住，不是靠前端藏按鈕。
**交付**：Admin 登入（短效 token／安全 session，與 Client API Key 分開兩套語意）；
`admin` `operator` `viewer` 三角色在 API 層強制；控制面 CRUD；所有控制面操作寫 audit
（actor／action／target／before／after／來源 IP／request_id）。
**完成判準必含陰性對照**：用 `viewer` 的憑證直接打 `admin` 的端點，必須 403。

| 編號 | Slug | 優先度 | 前置 |
| --- | --- | --- | --- |
| W09 | `admin-web` | P1 | W08 |

**目的**：管理者打開頁面就能回答「誰在用、用什麼模型、花多少、誰在排隊」。
**交付**：React ＋ TypeScript ＋ Vite ＋ TanStack Query。九個頁面：
Overview／Identities／Credentials／Requests／Usage／Models & Providers／
Policies & Scheduler／System Health／Audit。
**明確不追求**：設計感。第一版只要可操作、可分析、可除錯。

### Phase 3 — 排程與政策

| 編號 | Slug | 優先度 | 前置 |
| --- | --- | --- | --- |
| W10 | `model-registry-router` | P1 | W02 |

**目的**：模型名稱不再散在程式碼裡，換模型是改設定不是改程式。
**交付**：`models` 表（能力、context window、成本、延遲級別、開關）；quality alias
（`economy`／`standard`／`advanced`／`premium`）由設定決定；**確定性** Router 回答 WHERE。
**不做**：健康感知、成本感知、自動品質路由（那是 W17）。第一版不做 AI Router。

| 編號 | Slug | 優先度 | 前置 |
| --- | --- | --- | --- |
| W11 | `policy-engine` | P1 | W03, W10 |

**目的**：同一把金鑰的請求，效力政策是可預測、可解釋、可稽核的。
**交付**：六層政策（Global → User → Device → Client → Agent → Credential → Request hint）；
明確合併優先序；**Request hint 不得突破上層 hard constraint**；`EffectivePolicy` 寫進請求記錄。
**完成判準必含陰性對照**：Agent 上限 `standard`、請求要 `premium` → 實際必須是 `standard`。

| 編號 | Slug | 優先度 | 前置 |
| --- | --- | --- | --- |
| W12 | `quota-ratelimit` | P1 | W11, W04 |

**目的**：超量會被擋或被降級，且擋的理由查得到。
**交付**：RPM／估算 TPM／時日 token／日月成本／併發數，維度到 User／Device／Client／Agent／
Credential／Model／Provider；soft limit 走降級階梯（premium → advanced → standard → economy）或排隊；
hard limit 直接拒絕。
**不做**：分散式計數器。單機先做對，測到瓶頸再談。

| 編號 | Slug | 優先度 | 前置 |
| --- | --- | --- | --- |
| W13 | `scheduler-admission` | P1 | W11, W12 |

**目的**：100 個請求同時進來時，互動式工作不會被背景工作餓死，背景工作也不會永遠排不到。
**交付**：Admission control（憑證／配額／容量／佇列滿／deadline 已過期／模型允許／預算允許
→ accept／queue／downgrade／reroute／reject）；**有上限**的佇列；
Priority class ＋ 加權公平佇列 ＋ aging；保留容量（premium 併發 20：互動 12／共用 4／背景 4）；
多維併發上限；deadline 過期取消。
**Scheduler 回答 WHEN，Router 回答 WHERE，兩者不得混成同一個 module。**

### Phase 4–7 — 展開

| 編號 | Slug | 優先度 | 前置 |
| --- | --- | --- | --- |
| W14 | `canonical-layer` | P2 | W05, W06 |

**目的**：協定轉換從 N×M 變成 N+M，且**現有行為不得改變**。
**交付**：`CanonicalRequest` / `CanonicalResponse` / `CanonicalStreamEvent` / `CanonicalMessage` /
`CanonicalToolDefinition` / `CanonicalToolCall` / `CanonicalUsage` / `CanonicalError`；
Protocol Adapter 與 Provider Adapter 分離；`provider_options.<vendor>` 保留原廠能力。
**完成判準**：W05／W06 的既有 contract test 全部沿用、逐位元對照（黃金測試），不得放寬。

| 編號 | Slug | 優先度 | 前置 |
| --- | --- | --- | --- |
| W15 | `cross-provider` | P2 | W14 |

**目的**：Anthropic 協定的 Client 能用 OpenAI 模型，反之亦然。
**交付順序（每項獨立 contract test，逐項宣布）**：① 文字 ② 串流 ③ 工具呼叫
④ 結構化輸出 ⑤ 推理／extended thinking。
**鐵則**：轉不過去的能力**禁止無聲丟棄**。只能拒絕、明示降級、或改路由到支援的模型。

| 編號 | Slug | 優先度 | 前置 |
| --- | --- | --- | --- |
| W16 | `providers-expansion` | P2 | W14 |

**目的**：加一個 Provider 不需要改 Core。
**交付**：Gemini、xAI、Local（vLLM／Ollama）。
**驗收即設計檢查**：如果新增 Provider 必須大改 Core，代表 Adapter 抽象設計錯了，退回 W14 修。

| 編號 | Slug | 優先度 | 前置 |
| --- | --- | --- | --- |
| W17 | `advanced-routing` | P2 | W16 |

**目的**：Provider 變慢或壞掉時自動換路，且換路的理由查得到。
**交付**：Retry（同 Provider 重試）與 Fallback（換 Provider／模型）**遙測分開**；
健康感知、負載感知、成本感知、配額感知路由；自動品質路由。
**約束**：只對明確可重試的錯誤重試。已開始串流、已產生 token 成本、工具有副作用 → 不重試。

| 編號 | Slug | 優先度 | 前置 |
| --- | --- | --- | --- |
| W18 | `test-harness-load` | P1 | W06（第一階段）、W13（第二階段） |

**目的**：拿數字證明「不互串、不漏、不爆、不餓死」，不是靠綠燈。
**交付（兩階段）**：
- 階段一（W06 後）：100 條同時串流不互串、記憶體不持續成長、FD 不洩漏、取消正常；
  故障注入（timeout／429／500／串流中斷／DB 不可用／OTel 不可用／Client 斷線）；
  閘道自身 overhead 的 P50／P95／P99 與 TTFT 基準。
- 階段二（W13 後）：排程器公平性（10 互動 premium ＋ 30 互動 economy ＋ 30 背景 premium ＋
  30 批次 economy）→ 互動優先、保留槽生效、背景不霸佔且最後仍被服務、deadline 過期取消、佇列滿正確拒絕。
**每個數字都要有量測方法與 journal 紀錄**，不留「大概」。

| 編號 | Slug | 優先度 | 前置 |
| --- | --- | --- | --- |
| W19 | `process-lifecycle` | P0 | W05 |

**目的**：部署一次不會砍掉全公司正在跑的 Claude Code session，也不會讓行程永遠退不掉。
**交付**：有上限的排空（SIGTERM → 停收新請求、既有串流繼續、`drain_timeout` 到才強制關閉）；
排空期間 `/ready` 非 200 而 `/health` 仍 200；被砍的串流留下記錄；request body／header 上限；
所有上游呼叫有逾時；**配額視窗的時區定義**；**多實例的併發與配額語意二選一並記錄理由**。
**為什麼是 P0**：現況 `src/startup.rs:32` 只等 `ctrl_c()` 就交給 axum 的 graceful shutdown，
而 axum 會等 in-flight 結束——**一條 SSE 不會自己結束**。所以現在部署的行為是
「要嘛永遠不退出、要嘛被外部砍掉」，兩種都不對。這是反方審查抓出來的缺口
（原本 18 個段落沒有人擁有這件事）。

## 既有看板卡對照

看板上 15 張卡開在 LiteLLM 架構下（2026-07-29）。新規格移除 LiteLLM 執行期依賴，因此：

| 既有卡 | 處置 | 對應段落 |
| --- | --- | --- |
| #3 威脅模型與 Provider 合約核准 | 保留 | W01 |
| #18 Claude Code／Desktop 整合路徑 | 保留 | W01 |
| #15 Hermes 與 VS Code 整合矩陣 | 保留 | W01 |
| #5 Codex 自訂 Provider 路徑 | 保留 | W01 |
| #7 Orca 目標與轉接器 | 保留 | W01 |
| #14 PostgreSQL schema 與唯讀用量帳本 | 保留 | W04 |
| #16 虛擬金鑰生命週期與 RBAC | 拆成兩段 | W03（生命週期）＋ W08（RBAC） |
| #1 Secret manager 整合與輪替 | 保留 | W02（介面）＋ W03（輪替） |
| #4 OpenAI 相容串流與錯誤契約 | 保留 | W06 |
| #17 階層式政策引擎與模型白名單 | 保留 | W11 |
| #13 RPM／TPM／併發與預算保留 | 保留 | W12 |
| #10 用量對帳、稽核匯出與成本分攤 | 保留 | W04＋W09 |
| #8 Admin console ＋ SSO ＋ 核准流程 | 縮小 | W08＋W09（第一版不做 SSO／核准流程） |
| #19 HA 部署、SLO 與事故控制 | 保留 | W02（compose）＋W18（數字） |
| **#6 LiteLLM 部署註冊表、健康路由與 fallback** | **作廢** | 由 W10（註冊表）＋W16（Provider）＋W17（健康路由／fallback）取代 |

## 明確不做（第一版）

Kubernetes、Kafka、Redis Cluster、ClickHouse、Service Mesh、SSO／企業 IAM、
計費 SaaS、AI Router、Prompt marketplace、自己做可觀測性資料庫、精緻後台 UI。

除非**實際量測**證明需要。要加就先給數字。
