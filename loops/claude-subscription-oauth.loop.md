# LOOP: claude-subscription-oauth

| 欄位 | 值 |
| --- | --- |
| 開始 | 2026-10-20T21:20+08:00 |
| 結束 | — |

## 目的
讓本人的 Claude Code 訂閱登入經 Gateway 與 Jev 後，仍由 Anthropic Claude 回覆。

## 完成判準
- [ ] Gateway 驗證 Claude OAuth，但不保存或記錄 OAuth。
- [ ] OAuth 對應既有個人身分，政策、用量與稽核仍可追蹤。
- [ ] Jev 只選可用的 Anthropic Claude，請求不落到本機模型。
- [ ] 真實 Claude Code 經公開入口收到成功回覆，服務日誌能對到 Anthropic Provider。
- [ ] 格式、靜態檢查與全套測試通過，修改合進 `dev`。

## 已知
Claude Code 的 OAuth 已能直連 Anthropic 並取得模型；同一登入送到 Gateway 目前被 401 擋下。
Gateway 目前會移除 Client 的 Authorization，並改用伺服器 Provider 憑證。

## 計畫
| Step | 交付 | 狀態 |
| --- | --- | --- |
| S01 | OAuth 驗證、個人身分對應、只向 Anthropic 轉送 | 完成 |
| S02 | 本機部署與真實 Claude Code 往返 | 完成 |
| S03 | 全套檢查、提交、推送、合併 `dev` | 待開始 |

## 目前狀態
本機服務已接通；進行 S03 全套檢查與合併。

## 已做
OAuth 驗證、個人 Principal 對應、Client bearer 限定轉送 Anthropic，以及 14 個 Claude 模型登記。

## 已確定不可行
OAuth 少了 Anthropic 指定的 OAuth 與直接存取標頭時，上游會拒絕；Gateway 已集中補齊。

## 證據
直接請求經 Gateway 回 `GATEWAY_SUBSCRIPTION_OK`；Claude Code 2.1.304 經公開入口回
`CLAUDE_CODE_GATEWAY_OK`。日誌同次請求記錄 Provider `anthropic-subscription` 與 HTTP 200。

## 風險
OAuth refresh token 會輪替；驗收只能共用原登入，不可複製成兩份同時刷新。

## 來源
Anthropic `/v1/models` 與 `/v1/messages` 真實回應（2026-10-20）。

## 下一步
跑完整關卡，提交、推送並合進 `dev`。
