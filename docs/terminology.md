# 用詞定義

本 repo 的文件、程式訊息與後台畫面，只用下面三個詞指這三樣東西，不混用。

| 用詞 | 是什麼 | 誰發的 | 放在哪裡 | 用來做什麼 |
| --- | --- | --- | --- | --- |
| Gateway 金鑰 | 前綴 `aigw_` 的字串 | Gateway 自己發 | Client 的設定；Gateway 資料庫只存雜湊 | 讓 Gateway 認出「這是哪個人」。跟任何模型帳號無關，拿去問 Anthropic 或 OpenAI 無效 |
| API 金鑰 | 模型供應商發的金鑰，例如 `ANTHROPIC_API_KEY` | Anthropic、OpenAI 等 | 只在 Gateway 伺服器端 | Gateway 代替使用者呼叫該供應商，按用量計費 |
| 權杖 | 前綴 `sk-ant-oat` 的訂閱登入權杖 | Anthropic，由 `claude setup-token` 產生，效期一年 | 只在 Gateway 容器端，存在 `ANTHROPIC_SUBSCRIPTION_TOKEN_FILE` 指的檔案，權限 0600 | Gateway 代替訂閱擁有者呼叫 Anthropic，用訂閱額度 |

## 規則

1. Client 只帶 Gateway 金鑰。API 金鑰與權杖都不給 Client，也不回傳給任何 API 呼叫。
2. 「金鑰」單獨出現時一律指 Gateway 金鑰；另外兩樣必須寫全名「API 金鑰」「權杖」。
3. 程式識別字不變：`aigw_` 金鑰、`*_API_KEY`、`ANTHROPIC_SUBSCRIPTION_TOKEN_FILE`。
4. 舊稱「virtual key」「client key」「credential」指 Gateway 金鑰時，改寫為「Gateway 金鑰」。`credentials` 資料表名稱與 `/admin/v1/credentials` 路徑是程式識別字，維持不動。

## Gateway 金鑰在本機的存放（2026-11-06）

舊金鑰遺失過一次：Gateway 資料庫只存雜湊，原文只在發出當下顯示。現在本機只有一個存放處，所有 Client 都從這裡讀。

| 名稱 | 是什麼 | 在哪裡 |
| --- | --- | --- |
| `gateway-key` 檔 | 本機這把 Gateway 金鑰的原文，一行，無其他內容 | `~/.config/agent-pool-gateway/gateway-key`，權限 0600 |
| `ANTHROPIC_AUTH_TOKEN` | Claude Code 讀取的變數名稱，值就是 Gateway 金鑰，不是權杖 | 執行 `aigw` 時由 `~/.zshrc` 從上一個檔案設定 |
| systemd 環境檔 | `AGENT_API_KEY` 的值，供登入後的程式使用；環境檔不能讀別的檔，只能放一份副本 | `~/.config/environment.d/90-agent-gateway.conf`，權限 0600 |
| Codex | `~/.codex/config.toml` 的 `gateway-subscription` 用 `cat` 讀同一個檔 | 同上 |

- 金鑰原文不寫進文件、日誌、`settings.json` 或 git。
- 部署腳本的原生 Client 探測沿用 `ANTHROPIC_AUTH_TOKEN`；部署前先執行 `aigw`。
- 換金鑰：發新的、同步改寫 `gateway-key` 與 `90-agent-gateway.conf`、再撤銷舊的。除這兩處外不得另建副本。
- 撤銷舊金鑰前，先搜尋 `~/.codex`、`~/.zshrc`、`~/.config` 有沒有其他地方引用它。

## 權杖的更新方式

管理者呼叫 `PUT /admin/v1/provider-tokens/anthropic`，內容 `{"token": "sk-ant-oat..."}`。

- 只有 admin 角色可用；每次更新寫一筆稽核紀錄，內容只有指紋（權杖雜湊前 12 個十六進位字元），不含權杖。
- 回應只回指紋，不回權杖。格式不符（沒有 `sk-ant-oat` 前綴、含空白或過長）回 400，舊權杖保持不變。
- 更新後下一個請求立即使用新權杖，不用重啟容器。
- 沒設定 `ANTHROPIC_SUBSCRIPTION_TOKEN_FILE` 時這個介面回 404。
- 第一次可用 `ANTHROPIC_SUBSCRIPTION_TOKEN` 當種子；檔案已有權杖時，種子被忽略。
- 還沒有權杖時，Anthropic 請求回 `provider_unavailable`。

## 多人各用自己的訂閱（開發中）

要讓多位使用者各用自己的帳號，設定 `ANTHROPIC_PERSONAL_TOKEN_DIR` 為容器內的個人憑證目錄。若要使用 Anthropic 訂閱，另設定 `ANTHROPIC_SUBSCRIPTION_TOKEN_FILE` 為容器內的單人權杖檔路徑；只用 API 金鑰時不需要設定訂閱權杖檔。建議路徑放在 `/var/lib/agent-pool-gateway/provider-secrets/` 下。Docker Compose 已提供這個永久保存目錄，需妥善保護及備份。這些設定預設均未啟用；原本使用的單人模式不會自動切換。

新建的保存 volume 目錄由 Gateway 使用者持有，權限為 0700；既有 volume 不會因映像更新自動改權限，使用前須確認 Gateway 可寫且其他使用者無法讀取。

管理者以 `PUT /admin/v1/provider-tokens/anthropic/{user_id}` 設定該使用者的權杖，內容為 `{"token":"..."}`；以 `DELETE` 撤銷。`user_id` 必須是管理者所屬租戶的啟用使用者。個人模式依 Gateway 金鑰辨識出的使用者選權杖；沒有使用者或個人權杖時拒絕，不會改用單人權杖。使用者自己取得訂閱權杖，不在 Client 傳送供應商權杖。

此模式尚未部署或完成整體驗收；模型來源需使用現有訂閱權杖的供應商設定。檔案更新與稽核分屬檔案系統和資料庫，稽核失敗時可能需要管理者核對結果。已有請求正在送出時，撤銷可能暫時失敗，必須確認回應後再處理。

個人模式也提供每人的 API 金鑰：管理者可用 `PUT /admin/v1/provider-api-keys/{provider}/{user_id}` 設定，內容為 `{"token":"..."}`；用 `DELETE` 撤銷。`provider` 是租戶中已啟用的來源識別值，使用者與來源都必須屬於管理者的租戶。尚未設定個人 API 金鑰時，不會退回共用金鑰；無需憑證的自架模型不受限制。這項功能尚未部署或完成各來源實送驗收。

若設定或撤銷回 503 且錯誤代碼為 `admin_operation_uncertain`，代表操作結果未確認；檔案同步或結果稽核失敗都可能發生。先以相同路徑的 `GET` 查詢；回應只含是否已設定與金鑰指紋（SHA-256 前 12 個十六進位字元），不含金鑰。訂閱權杖也可用其個人權杖路徑的 `GET` 查詢。查詢只能顯示當下狀態，不能證明先前哪一次操作成功，也不能保證同步失敗後遇到斷電仍保留。資料庫尚未恢復或使用者、來源已停用時，查詢也可能失敗，必須先排除該問題。查詢不會補寫缺少的結果稽核。

同時使用 Anthropic 訂閱與 API 金鑰時，需建立兩個不同來源與模型名稱，並在 `EXTRA_PROVIDER_PROFILES_JSON` 增加 API 來源，例如：

```json
[{"kind":"anthropic","credential_ref":"env://ANTHROPIC_PERSONAL_API","provider_key":"anthropic-api","base_url":"https://api.anthropic.com","operations":["anthropic_messages"]}]
```

來源資料中的 `credential_ref`、`provider_key` 與網址必須與設定一致；此處的 `credential_ref` 是來源識別值，不需要建立同名環境變數。Client 透過模型名稱選擇來源。

## 已查證與未查證（2026-10-29）

已查證：Anthropic 官方文件說 `claude setup-token` 產生一年效期的權杖，用於 CI 與腳本等無法瀏覽器登入的環境，只能發模型請求（<https://code.claude.com/docs/en/authentication>）。

未查證：用這種權杖從伺服器代替本人呼叫 Anthropic，是否符合 Anthropic 條款。第三方資料指 Anthropic 在 2026 年 2 月起禁止第三方產品透過使用者的訂閱登入轉送請求，並在 2026 年 1 月起以技術手段封鎖；詳細文字尚未對照 Anthropic 官方法律頁。目前只能說：Gateway 跑在擁有者自己的電腦上、只用擁有者自己的權杖，這是擁有者承擔的風險。
