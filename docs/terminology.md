# 用詞定義

本 repo 的文件、程式訊息與後台畫面，只用下面三個詞指這三樣東西，不混用。

| 用詞 | 是什麼 | 誰發的 | 放在哪裡 | 用來做什麼 |
| --- | --- | --- | --- | --- |
| Gateway 金鑰 | 前綴 `aigw_` 的字串 | Gateway 自己發 | Client 的設定；Gateway 資料庫只存雜湊 | 讓 Gateway 認出「這是哪個人」。跟任何模型帳號無關，拿去問 Anthropic 或 OpenAI 無效 |
| API 金鑰 | 模型供應商發的金鑰，例如 `ANTHROPIC_API_KEY` | Anthropic、OpenAI 等 | 只在 Gateway 伺服器端 | Gateway 代替使用者呼叫該供應商，按用量計費 |
| 權杖 | 前綴 `sk-ant-oat` 的訂閱登入權杖 | Anthropic，由 `claude setup-token` 產生，效期一年 | 只在 Gateway 容器端，存在 `ANTHROPIC_SUBSCRIPTION_TOKEN_FILE` 指的檔案，權限 0600 | Gateway 代替訂閱擁有者呼叫 Anthropic，用訂閱額度 |

## 規則

1. Client 只帶 Gateway 金鑰。API 金鑰與權杖不回傳給 Client；Gateway 需要使用時，會把對應憑證送給模型供應商。
2. 「金鑰」單獨出現時一律指 Gateway 金鑰；另外兩樣必須寫全名「API 金鑰」「權杖」。
3. 程式識別字不變：`aigw_` 金鑰、`*_API_KEY`、`ANTHROPIC_SUBSCRIPTION_TOKEN_FILE`。
4. 舊稱「virtual key」「client key」「credential」指 Gateway 金鑰時，改寫為「Gateway 金鑰」。`credentials` 資料表名稱與 `/admin/v1/credentials` 路徑是程式識別字，維持不動。

## Gateway 金鑰在本機的存放（2026-11-06）

舊金鑰遺失過一次：Gateway 資料庫只存雜湊，原文只在發出當下顯示，`aigw` 函式又只放在終端機記憶體。現在固定這樣放：

| 名稱 | 是什麼 | 在哪裡 |
| --- | --- | --- |
| `AGENT_POOL_GATEWAY_KEY` | 本機這把 Gateway 金鑰的原文 | `~/.config/agent-pool-gateway/gateway.env`，權限 0600，內容 `export AGENT_POOL_GATEWAY_KEY=aigw_...` |
| `ANTHROPIC_AUTH_TOKEN` | Claude Code 讀取的變數名稱，值就是 Gateway 金鑰，不是權杖 | 執行 `aigw` 時由 `~/.zshrc` 從上一個檔案設定 |

- 金鑰原文不寫進文件、日誌、`settings.json` 或 git。
- 部署腳本的原生 Client 探測沿用 `ANTHROPIC_AUTH_TOKEN`；部署前先執行 `aigw`。
- 要換金鑰：發新的、改寫該檔、撤銷舊的。舊金鑰撤銷前先確認已沒有 Client 在用。

## 權杖的更新方式

管理者呼叫 `PUT /admin/v1/provider-tokens/anthropic`，內容 `{"token": "sk-ant-oat..."}`。

- 只有 admin 角色可用；每次更新寫一筆稽核紀錄，內容只有指紋（權杖雜湊前 12 個十六進位字元），不含權杖。
- 回應只回指紋，不回權杖。格式不符（沒有 `sk-ant-oat` 前綴、含空白或過長）回 400，舊權杖保持不變。
- 更新後下一個請求立即使用新權杖，不用重啟容器。
- 沒設定 `ANTHROPIC_SUBSCRIPTION_TOKEN_FILE` 時這個介面回 404。
- 第一次可用 `ANTHROPIC_SUBSCRIPTION_TOKEN` 當種子；檔案已有權杖時，種子被忽略。
- 還沒有權杖時，Anthropic 請求回 `provider_unavailable`。

## 已查證與未查證（2026-10-29）

已查證：Anthropic 官方文件說 `claude setup-token` 產生一年效期的權杖，用於 CI 與腳本等無法瀏覽器登入的環境，只能發模型請求（<https://code.claude.com/docs/en/authentication>）。

Anthropic 官方說 Claude Code 可使用符合其 API 規格的企業代理；官方並未在這段說明中確認 Gateway 持有各人訂閱權杖的具體方式。每人只用自己的訂閱是專案要求；目前正式版只有一份權杖，尚未實作多人各自對應。參考 [Claude Code FAQ](https://support.claude.com/en/articles/12386420-claude-code-faq)。
