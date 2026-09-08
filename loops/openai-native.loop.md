# LOOP: openai-native

> 主體上限 200 行（不含「已做」）、實時更新。方法見全域 `start-work` skill。
> 卡 #TBD｜分支 `feature/openai-native`｜段落 `W06`

| 欄位 | 值 |
| --- | --- |
| 狀態 | 未開始 |
| 優先度 | P0 |
| 前置 | `anthropic-native` |
| 開始 | — |
| 結束 | — |
| Token | 由 `./scripts/token-ledger.sh` 填，不要手寫 |

## 目的
Codex 與任何 OpenAI 相容 Client 指向閘道就能正常工作，串流／取消／落帳與 Anthropic 那條路同一套骨架。

## 完成判準
- [ ] `POST /v1/responses`、`POST /v1/chat/completions`、`GET /v1/models` 直連 OpenAI。
- [ ] 串流／取消／時間戳／ID／落帳**複用** W05 的骨架，不另寫一套
      （驗收方式：`cargo tree` ／程式碼審查確認共用同一個 streaming 模組）。
- [ ] `GET /v1/models` 回的清單來自模型註冊表，不是 hardcode 字串。
- [ ] 錯誤契約：上游 4xx／5xx 對應到穩定的 HTTP status ＋ error code，不吞、不一律回 200。
- [ ] 串流 contract test：Responses SSE 與 Chat Completions SSE 各一組 fixture。
- [ ] **陰性對照**：mock provider 回 malformed SSE（缺 `data:` 前綴／截斷的 JSON）→
      閘道必須回明確錯誤並記 log，**不得**當成正常結束。

## 已知
- 現況已有 `/v1/chat/completions`，但走 LiteLLM 且非串流（`src/gateway.rs:64`）。
- 規格 §66：要區分「閘道支援這個模型」與「Client UI 顯示這個模型」。
  某些 Client 的 model picker 不顯示自訂模型，不代表閘道不支援。
- Responses API 與 Chat Completions 是兩套 wire 格式，Codex 用哪一套要看 W01 的查證結果。

## 已做
- （待填）

## 已確定不可行
- （待填）

## 證據
- （待填）

## 來源
- 規格 §5（端點）、§66（模型發現）。
- OpenAI Responses／Chat Completions 官方文件（URL 待 W01 覆核後填入）。

## Trade-off
| 決策 | 選了什麼 | 放棄了什麼 | 為什麼 |
| --- | --- | --- | --- |

## 下一步
1. 先確認 Codex 實際用哪套 wire API（W01 產出）。
2. 複用 W05 streaming 模組接上 OpenAI。
3. `/v1/models` 接模型註冊表（若 W10 未完成，先讀設定檔，介面留一致）。
