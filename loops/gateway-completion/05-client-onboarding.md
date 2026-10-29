# 05 工具連線與 Agent 邊界

目標：支援自訂端點的 IDE、CLI、Agent 容易設定 Gateway 網址與個人 Gateway 金鑰。

- 逐項核對 Claude Code、Codex、VS Code、其他可取得 Client 的官方設定與實際支援，清楚告知不支援與未知；不要求共享帳號或修改 Client 執行檔。
- Agent 自己處理本機工具、服務連線與授權；Gateway 保留工具要求、識別與結果，不能聲稱自動沿用 claude.ai 連接工具。
- 驗收：原 Client 真送、工具呼叫與結果完整往返、串流、取消、錯誤；本機與遠端 Gateway 分別驗，列出未安裝、未授權或未實測產品。

狀態 PENDING；依賴 01、02、03。
