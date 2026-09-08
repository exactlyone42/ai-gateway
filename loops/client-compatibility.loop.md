# LOOP: client-compatibility

> 主體上限 200 行（不含「已做」）、實時更新。方法見全域 `start-work` skill。
> 卡 #TBD｜分支 `feature/client-compatibility`｜段落 `W01`

| 欄位 | 值 |
| --- | --- |
| 狀態 | 進行中 |
| 優先度 | P0 |
| 前置 | 無 |
| 開始 | `2026-09-08T21:50+08:00` |
| 結束 | — |
| Token | 由 `./scripts/token-ledger.sh` 填，不要手寫 |

## 目的
回答「哪些 Client 真的能指向自架閘道」，以及「訂閱席次與 API 計費的邊界在哪」，
每條結論都附得出官方來源。

## 完成判準
- [ ] `docs/client-compatibility.md` 每個 Client 一條，欄位齊全：
      client／version／verification_date／custom_endpoint／protocol／authentication／streaming／
      tools／reasoning／seat_subscription_behavior／configuration_example／known_limitations／official_source。
- [ ] 每條結論標明「這證明什麼、證明不了什麼」。
- [ ] 查不到官方來源的一律標 `unknown`，**不寫推測**。
- [ ] 明確區分「閘道支援這個模型」與「Client UI 顯示這個模型」。
- [ ] 訂閱席次邊界寫進 `docs/security-and-compliance.md`，附官方定價／方案頁原文。
- [ ] **實測**：至少一個 Client 真的指向本機閘道並成功往返一次（不是只讀文件）。

## 已知
- 待查的 Client：Claude Code CLI、Claude Desktop、Codex CLI、Codex Desktop／IDE、
  VS Code（Copilot Chat）、Cursor。
- 既有 `docs/client-compatibility.md` 只有 17 行，是 LiteLLM 時期的粗略描述，沒有版本、
  沒有查證日期、沒有官方來源 → 不能當證據，要重寫。
- 硬約束（規格 §6）：不做 TLS 中間人、不注入憑證、不破解 Client、不改執行檔、
  不攔截訂閱憑證。某個 Client 不支援就標 unsupported，不硬 hack。

## 已做
- `2026-09-08` 起 `compat-research` 子 Agent 查證 9 組問題（各 Client 自訂端點、席次邊界、
  SSE 事件形狀、取消語意、`count_tokens` 是否存在）。要求輸出官方 URL ＋ 逐字引用 ＋ 邊界。

## 已確定不可行
- （待填）

## 證據
- （待填：每條要有官方 URL ＋ 查證日期，或本機實測輸出）

## 來源
- （待填）

## Trade-off
| 決策 | 選了什麼 | 放棄了什麼 | 為什麼 |
| --- | --- | --- | --- |
| 子 Agent 查證 | 子 Agent 廣搜 → 主 session 覆核關鍵 URL | 主 session 自己全查 | 全查會把數十頁文件讀進主 context；但「其它 Agent 的報告」不算證據，關鍵條目要自己覆核 |

## 下一步
1. 收子 Agent 結果，覆核最關鍵的兩條（Claude Code 自訂 base URL、Codex 自訂 provider）。
2. 重寫 `docs/client-compatibility.md`。
3. 席次邊界寫進 `docs/security-and-compliance.md`。
4. 實測一個 Client 對本機閘道往返（等 W05 有 `/v1/messages` 之後）。
