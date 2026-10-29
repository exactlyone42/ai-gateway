# 從現況到目標：要用哪些工具、先做哪些事

研究日期：2026-10-23；文字與範圍修正：2026-10-24。

## 建議：先讓自己的操作介面完成一件工作

現有 Gateway 負責轉送模型請求、控管使用權限與記帳。先接上 Codex 已有的任務執行程式，讓自己的介面可以送出任務、顯示進度、核准操作與停止工作。

這個可供其他程式接入的執行程式叫 Codex app-server。先用它已有的對話續接與工具執行功能。等確定需要本地模型，再比較 Qwen Code，不先同時維護兩套接法。

先前把第二套模型接法、搜尋資料庫與多人平台放得太前面。以下改成按實際需求逐步增加；原先 9–12 週只是初估，不能當成已確認的工期。

## 三條路，讓你選優先順序

| 選擇 | 會用到什麼 | 最值得的地方 | 要付出的代價 |
| --- | --- | --- | --- |
| A 先做出能用的介面，推薦 | Codex app-server + 現有 Gateway | 沿用任務執行、工具與對話功能，自己要寫的程式最少 | 日後換模型仍要驗證相容性 |
| B 優先讓資料留在本機 | Qwen Code + 本機模型執行工具 llama.cpp | 可自行選模型與控制資料送往哪裡 | 要先測硬體、模型品質與速度 |
| C 優先提供多人遠端工作 | 提供遠端任務與工作資料夾的 OpenHands | 已有多人平台可參考的組成 | 新服務多，目前沒有足夠理由先做 |

這是按現況作的工程判斷，不是效能排名。本機硬體與真實任務尚未跑比較，不能保證 B 最便宜或最快。

## 從現況到目標，每一塊用什麼

| 要解決的事 | 技術名稱與用途 | 什麼時候做 |
| --- | --- | --- |
| 轉送請求、權限與帳務 | 沿用以 Rust 編寫的 Gateway；Axum 接收請求，Tokio 處理同時進行的工作 | 保留現況 |
| 自己的操作介面 | 先沿用現有前端工具；接 Codex app-server 顯示任務、進度、核准與停止 | 第一版 |
| 保存和續接對話 | 先沿用選定執行程式的對話功能 | 第一版，不另造儲存系統 |
| 控制餵給模型的資料 | 先限制每次送出的內容量，保留當前任務、必要歷史與有來源的摘要 | 長對話確實影響品質時 |
| 找出相關文件 | 先用現有檔案搜尋；需要按意思找內容時，再加 PostgreSQL 的相似內容搜尋擴充 pgvector | 搜尋確實找不到資料時 |
| 將文件轉成可比對的數字 | Qwen3-Embedding-0.6B，把文字轉成代表意思的數字 | 採用相似內容搜尋時 |
| 把最相關的資料排前面 | Qwen3-Reranker，重新比較問題與搜尋結果 | 找得到資料但排序不好時 |
| 減少重複運算 | 先沿用模型供應商對相同開頭內容的快取 | 先確認帳單與快取用量 |
| 本機跑模型 | llama.cpp，可用壓縮後的模型，候選路線包括 AMD 與 CPU | 選 B，或 A 出現本地需求時 |
| 租顯卡跑模型 | vLLM，負責在 GPU 上接收並處理模型請求 | 實測證明需要租卡時 |
| 操作網頁 | Playwright，透過按鈕名稱、欄位與頁面結構操作 | 第一個網頁任務需要時 |
| 操作只有畫面的程式 | UI-TARS，辨識畫面上的位置並提出操作 | 一般工具無法操作時 |
| 限制工具能碰的資料 | 沿用核准機制與容器，只提供該任務需要的資料夾與權限 | 接入工具時 |
| 查出慢在哪一步 | 沿用 OpenTelemetry，記錄請求、模型與工具各花多少時間 | 第一版 |

本機 Radeon 8060S 是否能順利跑選定模型、跑多快，仍要實測。工具支援 AMD 不等於這台電腦已驗收。

## 上下文與資料品質怎麼改善

上下文就是每次送給模型參考的資料。由執行任務的程式整理，Gateway 負責轉送和控管。

先檢查模型是否拿到了正確檔案、最新內容與必要工具結果。不要一開始把所有文件塞進去，也不要先建一套長期記憶系統。

如果歷史太長，再增加摘要。摘要要能追溯原文，避免把猜測留成事實。若需要搜尋文件，先去除重複與過期內容，再測搜尋結果是否真的幫助任務完成。

先用現有快取量測節省多少。工具結果可能隨檔案修改而過期，尚未確定重複使用規則前，不另外快取答案或操作結果。

## 別人的做法，哪些值得採用

| 專案 | 可以學什麼 | 本次選擇 |
| --- | --- | --- |
| Codex | 任務執行、續接對話、工具、核准與停止 | A 的起點；接公開介面，先不修改其核心 |
| Claude Agent SDK | 讓程式呼叫 Claude 的工具套件，包含權限與對話管理 | 保留為 Claude 專用選項 |
| Qwen Code | 可接不同模型的程式工作助手 | B 的候選，不預先和 A 一起做 |
| Hermes | 搜尋過去對話、保存少量重要記憶 | 先借鏡，不整套搬入 |
| OpenHands | 遠端工作資料夾、任務執行與操作介面 | 真有多人需求再評估 |

Qdrant 是另一套相似內容搜尋資料庫；SGLang 是另一套 GPU 模型服務。兩者先保留研究，不加入建置清單。

集中管理憑證的 Vault、管理多台機器服務的 Kubernetes，以及額外執行隔離工具，也不先列為必要工程。遇到已知需求，再針對該問題評估。

## 進度：一次完成一件可用的事

| 順序 | 做什麼 | 怎樣算完成 |
| --- | --- | --- |
| 1 | 選 5 個現在真的會做的任務，記錄現有耗時與用量 | 有可比較的結果 |
| 2 | 接一套任務執行程式，做出自己的最小操作介面 | 能送出任務、看到結果、核准與停止 |
| 3 | 用同樣 5 個任務檢查最明顯的問題 | 能指出問題是資料、模型或工具造成 |
| 4 | 只補最有幫助的一項能力 | 與修改前相比，完成率、時間或成本確實改善 |

第二步完成後才估下一段工期。後續能力可以是更好的文件搜尋、本地模型或網頁操作，不必照固定順序全部做完。

程式只做當前需要的最小異動，不加註解；必要說明放在白話文件。先處理已遇到的問題，不為假設中的特殊情境增加框架。

## 查證範圍

這次修正文案與工作範圍，沒有執行模型基準，也沒有改動正式服務。下列來源保留供查證；功能描述來自先前研究，實際採用前仍需用我們的任務驗證。

## 主要來源

- OpenAI Codex harness / app-server：[Codex as a platform](https://developers.openai.com/blog/codex-as-a-platform)、[Codex repository](https://github.com/openai/codex)
- Anthropic：[Claude Agent SDK](https://code.claude.com/docs/en/agent-sdk/overview)、[permissions](https://code.claude.com/docs/en/agent-sdk/permissions)
- Qwen：[Qwen Code architecture](https://qwenlm.github.io/qwen-code-docs/en/developers/architecture/)、[sandbox](https://qwenlm.github.io/qwen-code-docs/en/users/features/sandbox/)、[Qwen-Agent context](https://qwenlm.github.io/Qwen-Agent/en/guide/core_moduls/context/)、[Qwen3 Coder](https://qwenlm.github.io/blog/qwen3-coder/)、[Qwen3 Embedding](https://qwenlm.github.io/blog/qwen3-embedding/)
- 參考 Agent：[Hermes Agent](https://github.com/NousResearch/hermes-agent)、[OpenHands software-agent-sdk](https://github.com/OpenHands/software-agent-sdk)
- 資料：[pgvector](https://github.com/pgvector/pgvector)、[PostgreSQL full text search](https://www.postgresql.org/docs/current/textsearch-controls.html)、[Qdrant hybrid query](https://qdrant.tech/documentation/search/hybrid-queries/)
- 推理：[llama.cpp](https://github.com/ggml-org/llama.cpp)、[vLLM](https://docs.vllm.ai/en/stable/getting_started/installation/)、[SGLang](https://sgl-project.github.io/)
- 操作：[Playwright MCP](https://playwright.dev/docs/getting-started-mcp)、[UI-TARS](https://github.com/bytedance/UI-TARS)、[OSWorld 2.0](https://osworld-v2.xlang.ai/)
- 安全與觀測：[OWASP prompt injection](https://cheatsheetseries.owasp.org/cheatsheets/LLM_Prompt_Injection_Prevention_Cheat_Sheet.html)、[gVisor](https://gvisor.dev/docs/architecture_guide/security/)、[OpenTelemetry GenAI](https://opentelemetry.io/docs/specs/semconv/registry/attributes/gen-ai/)
