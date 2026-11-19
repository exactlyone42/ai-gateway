# 本機 Gateway 與 Jev

## 使用

`gateway-chat "問題"`：用本機 Qwen3.8 回答。省略問題可連續對話。
`gateway-chat --model local-code "程式問題"`：指定本機 Qwen3 Coder。
`gateway-chat --auto "問題"`：交給 Jev 從目前可用模型中選擇。

Gateway 位於 `http://127.0.0.1:18080`，使用獨立資料庫 `gateway_jev_local`。
設定在 `~/.config/agent-gateway-jev/server.env`；用戶金鑰由聊天程式讀取，不顯示在終端。
服務：`systemctl --user status agent-gateway-jev`；停止：`systemctl --user stop agent-gateway-jev`。

## Jev 分派

此版呼叫 TypeSafe 官方 API。只有請求的模型寫成 `quality:<等級>`（如 `gateway-chat --auto`）時，Gateway 才會讓 Jev 在該等級以下的候選中選模。
Claude Code、Codex 等直接指定模型名稱的請求不做任何模型判斷，請求什麼就用什麼。
不應送出禁止外傳的企業資料。TypeSafe 費用不包含於本機推論；本次沒有付費實測。

## 分派及失敗行為

原有帳號、公司範圍、模型能力及政策先過濾候選，再交給 Jev。
Jev 只回選項；Gateway 保留健康與配額檢查。只有選中的模型能接到問題。
模型描述可用 `JEV_MODEL_DESCRIPTIONS_JSON` 設定；目前依任務適合度選擇，不保證最低價格。
信心低於 0.6、錯誤選項、額度不足、上游錯誤或超過 5 秒時，立即沿用 Client 原本指定的模型。供應商仍只呼叫一次。
0.6 是保守的暫定門檻，尚未用個人資料校準。
同時最多兩筆 Jev 請求，最多 32 個候選，輸入 32 KiB、回應 64 KiB。
選模只送最近 4 段對話與最多 64 個工具的精簡說明，原始請求仍完整送給被選中的模型。

## 模型更新

Gateway 啟動時會向已設定的 Ollama 服務讀取模型與能力，之後每 24 小時再更新一次。已移除的自動發現模型會停用；未設定或無法連線的服務不會被假裝成可用候選。

## 驗證與範圍

已由 Claude Code 經 `https://gateway.exactlyone.dev` 實送，Gateway 經 Jev 選擇本機模型並收到 `GATEWAY_JEV_OK`；服務啟動同步 8 筆模型資料。
驗證收據：`~/.local/state/agent-gateway-jev/delivery.json`。
目前只設定本機 Ollama，尚未設定 Anthropic、OpenAI 等外部服務憑證，因此它們不會成為候選。

來源：https://docs.typesafe.ai/introduction/quickstart （2026-10-20 核對）
