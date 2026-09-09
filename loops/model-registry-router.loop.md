# LOOP: model-registry-router

> 主體上限 200 行（不含「已做」）、實時更新。方法見全域 `start-work` skill。
> 卡 待開（開工時建；取代作廢的 [#6](https://github.com/DnaBoss/agent-pool-gateway/issues/6)）｜分支 `feature/model-registry-router`｜段落 `W10`

| 欄位 | 值 |
| --- | --- |
| 狀態 | 未開始 |
| 優先度 | P1 |
| 前置 | `workspace-foundation` |
| 開始 | — |
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
- 現況模型白名單來自環境變數 `ALLOWED_MODELS`（`src/config.rs:36`），
  只是字串比對（`src/services/chat.rs:49`），沒有能力、成本、品質等級的概念。
- 規格 §20：實際模型名稱會變，所以註冊表必須動態可設定。
- 規格 §34：第一版優先「明確的確定性路由」，之後才加自動路由。**第一版不做 ML Router。**

## 已做
- （待填）

## 已確定不可行
- （待填）

## 證據
- （待填）

## 來源
- 規格 §18（Provider 專屬能力）、§20（模型註冊表）、§21（別名）、§33（Router 與 Scheduler 分開）、§34（路由）。

## Trade-off
| 決策 | 選了什麼 | 放棄了什麼 | 為什麼 |
| --- | --- | --- | --- |

## 下一步
1. `models` schema ＋ 從環境變數白名單遷移過去（行為保持）。
2. quality alias 對應表 ＋ Admin 可改。
3. 確定性 Router ＋ 能力檢查 ＋ 陰性對照。
