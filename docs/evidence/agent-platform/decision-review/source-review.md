# 本輪來源核對

2026-10-24。官方頁面已讀取，支持用途，不代表本機採用或驗收。

- https://docs.ollama.com/capabilities/tool-calling：模型提出工具要求，回傳結果再生成回答。
- https://github.com/QwenLM/qwen-code：候選終端工作助手。
- https://github.com/OpenHands/software-agent-sdk：可組合任務執行套件。
- https://github.com/pgvector/pgvector：PostgreSQL 相似內容搜尋擴充。
- https://docs.vllm.ai/en/stable/features/automatic_prefix_caching/：重用相同開頭計算。
- https://playwright.dev/docs/locators：依頁面結構定位。
- https://www.postgresql.org/docs/current/textsearch.html：全文搜尋，讀取時為 PostgreSQL 18。
- https://github.com/bytedance/UI-TARS：畫面操作模型；未本機實測。

原始碼讀取：src/services/sse.rs:58-70、src/services/openai.rs:910-923 確實解析快取用量；src/adapters/provider.rs 有本地供應來源契約。僅證程式存在，不證正式執行。

原件讀回：followup/model-tool-results.json 兩次回覆 prompt_eval_cached_count 為 0、334；沒有藉此推算加速或費用。summary-checks.json 含摘要與缺限制對照；cancellation/tool-results.json 顯示取消仍寫檔；container-results.json 顯示獨立容器對照可停。

獨立工作已完成七區比較、現況目標圖、依賴與驗收草案。後續長上下文、網頁／桌面與完整基準需先選定接入底座，否則會擴成非必要平行研究。正式快取測量與雲端費用也不在本輪授權內。等待使用者決定任務管理位置及首版優先方向；不是因取消問題停工。
