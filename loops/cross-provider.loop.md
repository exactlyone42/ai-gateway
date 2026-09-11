# LOOP: cross-provider

> 主體上限 200 行（不含「已做」）、實時更新。方法見全域 `start-work` skill。
> 卡 [#44](https://github.com/DnaBoss/agent-pool-gateway/issues/44)｜分支 `feature/cross-provider`｜段落 `W15`

| 欄位 | 值 |
| --- | --- |
| 狀態 | 進行中 |
| 優先度 | P2 |
| 前置 | `canonical-layer` |
| 開始 | `2026-09-11T08:50:31+08:00` |
| 結束 | — |
| Token | 由 `./scripts/token-ledger.sh` 填，不要手寫 |

## 目的
Anthropic 協定的 Client 能用 OpenAI 模型，反之亦然——而且能力轉不過去時會明說，不是默默吞掉。

## 完成判準（**逐項驗收、逐項宣布，不准一次宣布相容**）
- [ ] ① 文字：`Messages → OpenAI`、`Responses → Anthropic` 各一組 contract test。
- [ ] ② 串流：SSE 事件序列逐項比對。
- [ ] ③ 工具呼叫：定義、呼叫、結果回填三段都比對。
- [ ] ④ 結構化輸出。
- [ ] ⑤ 推理／extended thinking。
- [ ] 轉不過去的能力**禁止 silent drop**。只允許三種處置：拒絕、明示能力降級、改路由到支援的模型。
- [ ] **陰性對照**：把 silent drop 的防護拿掉 → 必須有測試變紅（證明防護在生效，不只是寫了）。

## 已知
- **這一段有官方政策風險，不只是技術問題**：Anthropic 明文
  「doesn't support routing Claude Code to non-Claude models through any gateway」
  （<https://code.claude.com/docs/en/llm-gateway>，2026-09-08）。
  技術上做得到，但「Claude Code ＋ 非 Claude 模型」不在 Anthropic 支援範圍。
  → 這段的產出必須標註此風險，不得當一般功能宣傳；要不要做由 cash 決定。
- 規格 §72 明確給了順序：文字 → 串流 → 工具 → 結構化輸出 → 推理。
- 規格 §18 列出不一定能一對一轉換的能力：reasoning／extended thinking／prompt cache／
  tool calling／structured outputs／computer use／image／provider hosted tools／MCP 語意。
- 這段開始前，`docs/provider-protocols.md` 必須已記錄兩邊的實際事件形狀（W05／W06 的產出）。
- 查重：2026-09-11 開工時只有本 loop；沒有同題 open issue／PR、遠端 branch 或其他
  worktree owner。建立唯一卡 #44 與 `feature/cross-provider`。
- Reviewer 使用獨立 `codex exec`；能力矩陣／轉換策略定案前與完整 diff 各審一次，高／中風險
  必須修正或以規格及 contract 實測裁定。
- 每項先跑精確 contract test；PR 前跑 `./scripts/check.sh all`、loops 與 `git diff --check`。
  每輪記錄已做、證據、不可行、trade-off、下一步；完成後 AI 可自行合入 `dev`。
- 外部依賴先自行打通；只做安全性、W15 必需依賴與 W15 目標，不順手擴增第三方 Provider。

## 已做
- 2026-09-11T08:50+08:00：完成 cashloop 查重，建立卡 #44、Project #2 item、唯一 feature 分支。

## 已確定不可行
- （待填）

## 證據
- （待填）

## 來源
- 規格 §18、§72（Phase 5）、§75（contract test）。

## Trade-off
| 決策 | 選了什麼 | 放棄了什麼 | 為什麼 |
| --- | --- | --- | --- |

## 下一步
1. 盤點 canonical request/stream 與兩邊 native wire，寫能力對照矩陣及拒絕策略。
2. 把查證事實與 ①→⑤ 實作順序送獨立 reviewer。
3. 先完成雙向文字 request/response contract，再進串流；每項獨立驗收。
