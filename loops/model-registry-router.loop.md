# LOOP: model-registry-router

> 主體上限 200 行（不含「已做」）、實時更新。方法見全域 `start-work` skill。
> 卡 [#36](https://github.com/DnaBoss/agent-pool-gateway/issues/36)（取代作廢的 [#6](https://github.com/DnaBoss/agent-pool-gateway/issues/6)）｜分支 `feature/model-registry-router`｜段落 `W10`

| 欄位 | 值 |
| --- | --- |
| 狀態 | 進行中 |
| 優先度 | P1 |
| 前置 | `workspace-foundation` |
| 開始 | `2026-09-11T04:04+08:00` |
| 結束 | — |
| Token | 由 `./scripts/token-ledger.sh` 填，不要手寫 |

## 目的
換模型是改設定，不是改程式；Client 可以要指定模型，也可以只要一個品質等級。

## 完成判準
- [ ] `models` 表欄位齊全：public_model_id／provider／provider_model_id／quality_tier／
      context_window／max_output_tokens／capabilities／supports_streaming／supports_tools／
      supports_reasoning／supports_images／supports_structured_output／cost_metadata／
      latency_class／enabled。
- [ ] 模型名稱**不在程式碼任何地方 hardcode**。驗收：`grep -rE 'claude-|gpt-|gemini-|grok-' crates/`
      在非測試程式碼中命中 0 筆。
- [ ] quality alias（`economy`／`standard`／`advanced`／`premium`）由 Admin 設定決定，
      不寫死「premium = 某個固定模型」。
- [ ] Router **確定性**：同樣的（請求、政策、註冊表狀態）必須選出同樣的模型，可單元測試。
- [ ] Router 只回答 WHERE，不做排隊決策（WHEN 屬於 W13），兩者不同 module。
- [ ] 能力不匹配時**禁止無聲丟棄**：只能拒絕、明示降級、或改路由到支援的模型。
- [ ] **陰性對照**：把某模型的 `supports_tools` 設 false，送一個帶工具的請求 →
      必須被拒絕或改路由，**不得**把工具定義默默拿掉送上游。

## 已知
- 現況模型白名單來自環境變數 `ALLOWED_MODELS`（`src/config.rs`），
  只在 provider service 做字串比對，沒有能力、成本、品質等級的概念。
- W08 擁有 Model／Provider 的 tenant-safe draft CRUD schema；W10 接手把 draft 轉成可啟用的
  registry，讓 enable/disable 在下一次推論前真的生效，並補對應 audit，不能另建平行資料表。
- 規格 §20：實際模型名稱會變，所以註冊表必須動態可設定。
- 規格 §34：第一版優先「明確的確定性路由」，之後才加自動路由。**第一版不做 ML Router。**

- W07 移交（2026-09-09）：span 樹缺 `router.select` 與 `token.estimate` 兩個節點（規格 §58），以及 `gen_ai.request.model`／`gen_ai.system` 兩個日誌欄位。這一段有程式碼後補上，作法沿用 provider service 的 `#[tracing::instrument(name = ..., skip_all)]`。
- W06 移交（2026-09-11）：Codex CLI 0.153.0 啟動時會讀 `/v1/models` 的私有 `models[]`
  metadata；一般 OpenAI `data[]` 可讓明確指定模型工作，但會有 fallback warning。W10 必須從
  真實欄位產生 context window／reasoning／能力 metadata，不得用猜值消除 warning。
- 查重：既有 loop 接手；開放卡只有引用 W10 的 #22，沒有專屬卡；遠端無同名分支或 PR，故建立 #36。
- Reviewer 6 high／4 medium：採納單一真相切換、provider trust binding、跨資源狀態交易、wire
  protocol 維度、只改 model 的協定契約、alias/能力單一真相、選擇快照落帳、真 Client discovery
  與觀測欄位實測。完整 idempotency replay 屬 W17；W10 先保存 selection/version，禁止透明改路由。
- Dev bootstrap 是唯一相容例外：只對 nil tenant 使用明示的 development registry；正式 tenant
  沒有 active registry 就 fail closed，不回退 `ALLOWED_MODELS`。
- Provider 啟用必須綁 server-side credential profile 的 origin、wire protocol、auth scheme，禁止
  Admin 任意組合 URL 與 secret ref；redirect 預設關閉，避免 SSRF 與 credential confused-deputy。

## 已做
- `2026-09-11T04:04+08:00` W09 PR #35／merge `333fd01`／CI success 後接手；完成卡、分支、PR 查重，
  建立 #36 並加入 Project #2；獨立 Codex 計畫審查完成（6 high／4 medium）。

## 已確定不可行
- （待填）

## 證據
- （待填）

## 來源
- 規格 §18（Provider 專屬能力）、§20（模型註冊表）、§21（別名）、§33（Router 與 Scheduler 分開）、§34（路由）。

## Trade-off
| 決策 | 選了什麼 | 放棄了什麼 | 為什麼 |
| --- | --- | --- | --- |
| 切換 | 正式 tenant 只讀 DB registry | production fallback 到舊白名單 | hot disable 必須有單一真相 |
| Alias | protocol-scoped alias 是 routing 真相 | 同時用 model quality_tier 隱式選擇 | 避免兩套映射與名稱碰撞 |
| 停用 | commit 後開始 select 的請求立即生效 | 取消已選中的 in-flight request | 不把 W19 lifecycle 混進 Router |

## 下一步
1. migration：provider/model 狀態機、generated enabled、wire protocol、protocol-scoped alias、選擇快照。
2. Config 建不可變 credential profile；Admin-only activate/disable 以固定鎖序、CAS、audit 維持依賴。
3. tenant-scoped Router 一次查詢選擇；能力 fail closed，只允許改寫 model 欄位，其餘語意保持。
4. `/v1/models` 真 metadata、Admin Web 操作、ledger/trace/log；整合、競態、跨 tenant 與 Client 驗收。
