# LOOP: canonical-layer

> 主體上限 200 行（不含「已做」）、實時更新。方法見全域 `start-work` skill。
> 卡 待開（開工時建）｜分支 `feature/canonical-layer`｜段落 `W14`

| 欄位 | 值 |
| --- | --- |
| 狀態 | 未開始 |
| 優先度 | P2 |
| 前置 | `anthropic-native`, `openai-native` |
| 開始 | — |
| 結束 | — |
| Token | 由 `./scripts/token-ledger.sh` 填，不要手寫 |

## 目的
協定轉換從 N×M 變成 N+M，而且**現有行為一位元都不准變**。

## 完成判準
- [ ] 內部型別齊全：`CanonicalRequest` / `CanonicalResponse` / `CanonicalStreamEvent` /
      `CanonicalMessage` / `CanonicalToolDefinition` / `CanonicalToolCall` / `CanonicalUsage` /
      `CanonicalError`。
- [ ] `CanonicalRequest` 帶得動政策與排程要用的欄位（gateway_request_id／trace_id／session_id／
      principal／requested_model／requested_quality／workload_class／urgency／budget_class／
      deadline／tools／tool_choice／stream／max_output_tokens／provider_options／metadata）。
- [ ] Protocol Adapter（Client 側）與 Provider Adapter（上游側）分離，**不得**互相依賴。
- [ ] `provider_options.<vendor>` 保留原廠能力，**不做最小公約數抽象**。
- [ ] **黃金測試**：W05／W06 既有的 contract test 全部沿用、逐位元比對，不得放寬任何一條。
- [ ] **陰性對照**：故意在 canonical 轉換裡丟掉一個欄位 → 黃金測試必須變紅。

## 已知
- 這一段是**純重構**：行為保持的重構要與功能變更**分開提交**（全域規約）。
- 規格 §16 明確禁止 N×M 轉換；§18 明確禁止 silent drop。
- 前置的 W05／W06 必須先有可靠的 contract test，否則這段沒有安全網——
  所以 W05／W06 的完成判準裡就要求 contract test。

## 已做
- （待填）

## 已確定不可行
- （待填）

## 證據
- （待填）

## 來源
- 規格 §16（Adapter 架構）、§17（Canonical 型別）、§18（Provider 能力）、§19（Provider 介面）、§71（Phase 4）。

## Trade-off
| 決策 | 選了什麼 | 放棄了什麼 | 為什麼 |
| --- | --- | --- | --- |

## 下一步
1. 先確認 W05／W06 的 contract test 覆蓋足夠當安全網（不夠就先補）。
2. 型別定義 ＋ 兩側 Adapter。
3. 黃金測試對照 ＋ 陰性對照。
