# LOOP: model-registry-router

> 主體上限 200 行（不含「已做」）、實時更新。方法見全域 `start-work` skill。
> 卡 [#36](https://github.com/DnaBoss/agent-pool-gateway/issues/36)（取代作廢的 [#6](https://github.com/DnaBoss/agent-pool-gateway/issues/6)）｜分支 `feature/model-registry-router`｜段落 `W10`

| 欄位 | 值 |
| --- | --- |
| 狀態 | 已交付 |
| 優先度 | P1 |
| 前置 | `workspace-foundation` |
| 開始 | `2026-09-11T04:04+08:00` |
| 結束 | `2026-09-11T05:09:29+08:00` |
| Token | 無法取得精確 token；Codex executor 無 usage export，Claude ledger 對本時窗拒絕回報 0 |

## 目的
換模型是改設定，不是改程式；Client 可以要指定模型，也可以只要一個品質等級。

## 完成判準
- [x] `models` 表欄位齊全：public_model_id／provider／provider_model_id／quality_tier／
      context_window／max_output_tokens／capabilities／supports_streaming／supports_tools／
      supports_reasoning／supports_images／supports_structured_output／cost_metadata／
      latency_class／enabled。
- [x] 模型名稱**不在程式碼任何地方 hardcode**。驗收：`grep -rE 'claude-|gpt-|gemini-|grok-' crates/`
      在非測試程式碼中命中 0 筆。
- [x] quality alias（`economy`／`standard`／`advanced`／`premium`）由 Admin 設定決定，
      不寫死「premium = 某個固定模型」。
- [x] Router **確定性**：同樣的（請求、政策、註冊表狀態）必須選出同樣的模型，可單元測試。
- [x] Router 只回答 WHERE，不做排隊決策（WHEN 屬於 W13），兩者不同 module。
- [x] 能力不匹配時**禁止無聲丟棄**：只能拒絕、明示降級、或改路由到支援的模型。
- [x] **陰性對照**：把某模型的 `supports_tools` 設 false，送一個帶工具的請求 →
      必須被拒絕或改路由，**不得**把工具定義默默拿掉送上游。

## 已知
- 現況模型白名單來自環境變數 `ALLOWED_MODELS`（`src/config.rs`），
  只在 provider service 做字串比對，沒有能力、成本、品質等級的概念。
- W08 擁有 Model／Provider 的 tenant-safe draft CRUD schema；W10 接手把 draft 轉成可啟用的
  registry，讓 enable/disable 在下一次推論前真的生效，並補對應 audit，不能另建平行資料表。
- 規格 §20：實際模型名稱會變，所以註冊表必須動態可設定。
- 規格 §34：第一版優先「明確的確定性路由」，之後才加自動路由。**第一版不做 ML Router。**

- W07 移交（2026-09-09）：span 樹缺 `router.select` 與 `token.estimate` 兩個節點（規格 §58），以及 `gen_ai.request.model`／`gen_ai.system` 兩個日誌欄位。這一段有程式碼後補上，作法沿用 provider service 的 `#[tracing::instrument(name = ..., skip_all)]`。
- W06 移交已由 W10 重驗更正：Codex CLI 0.153.0 的 custom provider 不會讀 gateway
  `/v1/models`；官方設定只提供本機 `model_catalog_json` startup path。明確指定模型可工作，
  但 gateway 無法經 HTTP 消除 fallback warning，也不得猜 reasoning levels。
- 查重：既有 loop 接手；開放卡只有引用 W10 的 #22，沒有專屬卡；遠端無同名分支或 PR，故建立 #36。
- Reviewer 6 high／4 medium：採納單一真相切換、provider trust binding、跨資源狀態交易、wire
  protocol 維度、只改 model 的協定契約、alias/能力單一真相、選擇快照落帳、真 Client discovery
  與觀測欄位實測。完整 idempotency replay 屬 W17；W10 先保存 selection/version，禁止透明改路由。
- Dev bootstrap 是唯一相容例外：只對 nil tenant 使用明示的 development registry；正式 tenant
  沒有 active registry 就 fail closed，不回退 `ALLOWED_MODELS`。
- Provider 啟用必須綁 server-side credential profile 的 origin、wire protocol、auth scheme，禁止
  Admin 任意組合 URL 與 secret ref；redirect 預設關閉，避免 SSRF 與 credential confused-deputy。
- Diff 反方審查提出 5 high／7 medium；採納 tenant/profile binding、初始 durable snapshot、metadata
  上限、operation 維度、disabled edit、原子 alias replacement、tenant lifecycle lock、cascade audit
  與完整 Admin Web。Codex discovery 建議以真 CLI 與官方 config reference 證偽後裁定不可由 HTTP 完成。

## 已做
- `2026-09-11T04:04+08:00` W09 PR #35／merge `333fd01`／CI success 後接手；完成卡、分支、PR 查重，
  建立 #36 並加入 Project #2；獨立 Codex 計畫審查完成（6 high／4 medium）。
- `2026-09-11T05:08+08:00` 完成 registry、deterministic router、profile tenant binding、protocol-scoped
  alias、selection ledger snapshot、Admin lifecycle/CAS/audit、標準模型清單與 Web 操作；diff 審查
  5 high／7 medium 全部修正或以實測裁定；本機完整關卡、真 Client、Playwright desktop/mobile 綠。
- `2026-09-11T05:09:29+08:00` PR #37 合入 `dev`（merge `86395e3`）；head `0e52cf2` 的
  `rust` 與 `sync` CI success。W10 收工，後續由 W11 policy-engine 接手。

## 已確定不可行
- 由 gateway `/v1/models` 消除 Codex 0.153.0 custom-provider fallback warning：真 CLI 往返只
  呼叫 `/v1/responses`，未呼叫 `/v1/models`；官方 config reference 指向本機
  `model_catalog_json`。私有 catalog schema 未受 gateway 契約支持，不能填猜值假裝完成。

## 證據
- `DATABASE_URL=.../apg_w10_final_20260911 ./scripts/check.sh all`：96 Rust unit + 4 schema tests、
  2 frontend tests、Clippy/build/secrets 與 gate self-test 29/29 全綠，0 ignored。
- `model_registry::tests::selection_is_tenant_scoped_deterministic_and_capability_aware` 同時驗 tenant、
  deterministic selection、operation 與 tools false 陰性對照；原始 request 除頂層 model 外不變。
- lifecycle integration 實測 disabled edit/reactivate、alias 原子替換、cascade audit、跨 tenant 404、
  untrusted profile fail closed；32 KiB spool 測試含 255-byte model id 與 4 KiB cost metadata。
- `./scripts/verify-client-roundtrip.sh all` exit 0：Claude Code 2.1.267、Codex CLI 0.153.0 真往返與
  versioned capture 相符；Codex warning 存在且 gateway log 無 `/v1/models`。
- Playwright 真 gateway 1 passed：Admin/operator/viewer、CRUD/edit/archive/lifecycle、stale CAS 409、
  invalid cost JSON、desktop 1440x1000 與 mobile 375x812；截圖無重疊或水平溢出。

## 來源
- 規格 §18（Provider 專屬能力）、§20（模型註冊表）、§21（別名）、§33（Router 與 Scheduler 分開）、§34（路由）。
- Codex config reference：<https://learn.chatgpt.com/docs/config-file/config-reference>。

## Trade-off
| 決策 | 選了什麼 | 放棄了什麼 | 為什麼 |
| --- | --- | --- | --- |
| 切換 | 正式 tenant 只讀 DB registry | production fallback 到舊白名單 | hot disable 必須有單一真相 |
| Alias | protocol-scoped alias 是 routing 真相 | 同時用 model quality_tier 隱式選擇 | 避免兩套映射與名稱碰撞 |
| 停用 | commit 後開始 select 的請求立即生效 | 取消已選中的 in-flight request | 不把 W19 lifecycle 混進 Router |

## 下一步
1. W11 policy-engine：先查重並依現成 loop 接手，不另開重複卡／分支。
