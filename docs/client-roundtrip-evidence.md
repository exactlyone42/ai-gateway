# 原 Client 傳輸驗收

## 2026-11-09 R1-S2 真工具與使用者續談

本輪沿用 S1 的 Gateway binary `f8f339c51e80`，只補驗收器，沒有改正式請求程式或部署。原 Claude Code 2.1.319 使用本機 `qwen3.8:27b`；原 Codex 0.166.0 使用已安裝的 `qwen3-coder:30b`。兩者各完成一次真讀檔、工具結果回送及使用者第二輪引用，共三個來源請求。兩側原訊息、工具編號、完整工具結果、串流及實際用量均有保存原件可核。

沿用 `verify-client-identity.py --check-local --check-capture --check-owned-tools`。每個 Client 須使用獨立且新建的本機 `gateway_client_identity_validation_` 測試庫，並明確設定 `CLIENT_IDENTITY_ALLOW_DATABASE_MUTATION=1`；透過既有 `temp-service.py` 登記 30 分鐘。Claude 的 `--source-model` 設 `qwen3.8:27b`，Codex 設 `qwen3-coder:30b`。第一輪及續談各限 600 秒，Gateway 限 1500 秒；失敗不自動重送，也不刪除資料庫或原件。

測試隨機檔的內容不出現在第一輪提示中。原 Client 以本機工具讀檔後，第二輪要求只引用已有結果，不准再次讀檔；Claude 使用 `--continue`，Codex 使用 `exec resume --last`，兩者均使用隔離設定與唯一工作目錄。做法依據 [Claude 官方續談說明](https://code.claude.com/docs/en/headless) 及 [Codex 官方非互動續談說明](https://developers.openai.com/codex/noninteractive)，並已核對本機 CLI 參數。

Claude 續談時會移走舊工具結果上的 `cache_control` 快取提示；本輪原件證明內容、編號與錯誤狀態均未變。驗證器僅在 Claude 的跨輪工具要求與結果比較中排除這個提示，其餘欄位仍逐項相符，原始請求照樣完整保存。這個欄位的用途見 [Claude 官方工具快取說明](https://platform.claude.com/docs/en/agents-and-tools/tool-use/tool-use-with-prompt-caching)。新增十一項對照，本機保存驗證共 53 項通過；包括缺工具結果、改工具編號、遺失原訊息、把 assistant 文字當新指令、再次呼叫工具，以及只保留隨機值卻截掉其餘結果。

兩個 Client 都在最終標記 `FOLLOWUP:` 後多放一個空白，因此原驗收器的精確格式判準仍失敗。沒有改判準或重送來調整措辭，也沒有將完整命令改記為成功。直接重播原件，另行核對檔案全部內容、第二輪歷史及實際用量；分別移除 Client→Gateway 與 Gateway→供應商保存中的工具結果，兩個 Client 共四項缺結果對照都被拒絕。後續另證實初版驗證器會漏抓工具要求被刪的情況，已保留失敗初件並補上原工具要求、名稱及編號的完整保留檢查；新增四項實際移除工具要求對照也全被拒絕。S2 尚未全綠，不標 DONE。

Codex 原先的 `qwen3.8:27b` 路線雖執行了工具，供應商第二個 Responses 回應僅含 `reasoning`，沒有最終 `message` 或 `response.output_text.done`，Client 沒有最終輸出。這項失敗保留，未將推理內容冒充最終答案；改驗同一 Gateway 已支援的非推理模型後，有最終訊息及第二輪結果。Ollama 只支援部分 OpenAI 功能，適用邊界見 [官方相容說明](https://docs.ollama.com/api/openai-compatibility)，不能由健康頁推定每種模型的工具往返都能完成。

原件與重播：`/home/cash/.local/state/ai-gateway/r1-s2-20261109`；Claude 執行目錄 `/tmp/gateway-client-identity-50wts4hj`，Codex 程式模型目錄 `/tmp/gateway-client-identity-w4763nyl`，Codex 推理模型失敗目錄 `/tmp/gateway-client-identity-tzpvcetr`。原件與重播分開保留。工具模式不重跑 S1 已核的模型拒絕與撤權，輸出明列 `not_run`；Claude 撤權逾時仍待 S4。長串流、取消、其他故障、發布及 R2／R3 仍未由本輪驗收。

## 2026-11-09 R1-S1 同版基準

本次使用 Claude Code 2.1.319、Codex CLI 0.166.0 與本機 `qwen3.8:27b`，不呼叫付費來源。整合來源為 #108 的 3ca21da 與 main 的 68d7569；編譯版本 `r1s1-1278965c59eb` 對應整合樹 1278965c59ebd1c91b607eac054fcfdc8537f43f。完整差異與驗收原件位於 `/home/cash/.local/state/ai-gateway/r1-s1-20261109`，尚未發布。

`verify-client-identity.py` 固定核對上述 Client 版本；舊的 `verify-client-roundtrip.sh` 與歷史假供應商紀錄繼續固定 2.1.318，不能當成本次版本證據。兩種驗收不修改使用者的原 Client 設定或登入資料。

兩個 Client 使用獨立且新建的本機測試庫，名稱須以 `gateway_client_identity_validation_` 開頭；先設定 `DATABASE_URL` 指向該庫，再從整合工作樹執行：

```sh
python3 ~/tools/temp-service.py start --name gateway-r1-baseline-claude --ttl 20m --purpose 原Claude同版基準 -- env DATABASE_URL="$DATABASE_URL" CLIENT_IDENTITY_ALLOW_DATABASE_MUTATION=1 python3 scripts/verify-client-identity.py --gateway-binary /home/cash/work_space/private/ai-gateway/target/debug/agent-pool-gateway --revision r1s1-1278965c59eb --client claude --check-local --source-model qwen3.8:27b --check-capture
python3 ~/tools/temp-service.py start --name gateway-r1-baseline-codex --ttl 20m --purpose 原Codex同版基準 -- env DATABASE_URL="$DATABASE_URL" CLIENT_IDENTITY_ALLOW_DATABASE_MUTATION=1 python3 scripts/verify-client-identity.py --gateway-binary /home/cash/work_space/private/ai-gateway/target/debug/agent-pool-gateway --revision r1s1-1278965c59eb --client codex --check-local --source-model qwen3.8:27b --check-capture
```

先完成並停止第一項，再換另一個新庫執行第二項。每個 Client 成功請求上限 600 秒，拒絕請求上限 30 秒，Gateway 上限 900 秒；只送腳本內的隔離短答，預期 Claude 輸出 `W05_CLAUDE_CODE_OK`、Codex 輸出 `W06_CODEX_OK`。每個 Client 各一個成功來源請求；拒絕請求必須沒有上游嘗試。停止測試不刪除資料庫及原件。

初件兩個原 Client 都正常輸出預期文字，保存內容與供應商用量逐筆相符；舊驗證器卻把 Gateway 的保活訊息當作資料差異而停止。原件保留於 `/tmp/gateway-client-identity-h5az6qxf` 與 `/tmp/gateway-client-identity-r3_jn10s`，修正後直接重播原件，不用新模型回覆取代失敗歷史。

Gateway 只在 Client 串流加入完整的 `: keepalive\n\n`。這是 [SSE 規格](https://html.spec.whatwg.org/dev/server-sent-events.html) 定義的註解，讓連線在等待時保持活動。驗證器只排除兩側恰為 `: keepalive` 的完整區塊，其餘資料、順序、註解與分隔符仍逐字比較；原始兩側內容各自完整保存。`stream_legs_equal` 回報原始字串是否相等，`stream_events_equal` 回報排除固定保活後是否相等；不能將後者當成原始位元組相等。

新增六項對照，確認保活可插入，但改資料、刪結尾、加其他註解、改區塊內文字與缺結束分隔符仍被拒絕。相等檢查本身不證明串流完成；既有完成事件與實際用量驗證繼續執行。多輪、工具、長串流、取消、全部故障及正式環境均仍待 R1 後續步驟驗收。

## 2026-11-05 歷史假供應商基準

2026-11-05 以 Claude Code 2.1.318、Codex CLI 0.166.0，經 Gateway d7c7cfd 連本機假供應商。兩個原 Client 各自讀到固定串流並輸出預期文字；沒有呼叫正式供應商或做模型運算。舊版本紀錄保留供追溯。

## 執行方式

只用可拋棄資料庫；腳本會驗名稱及明確寫入授權，啟動時執行遷移。以現有臨時服務工具登記10分鐘期限，結束後確認服務已停止。SQLx使用已驗證的離線查詢描述編譯，執行期仍連真正隔離資料庫。

```sh
python3 ~/tools/temp-service.py start --name gateway-client-validation --ttl 10m --purpose 原Client假串流驗收 -- env SQLX_OFFLINE=true BUILD_REVISION=<目前提交> CLIENT_ROUNDTRIP_ALLOW_DATABASE_MUTATION=1 DATABASE_URL=postgres://user:password@127.0.0.1:5432/disposable bash scripts/verify-client-roundtrip.sh all
python3 ~/tools/temp-service.py list
```

成功結果為 `client roundtrip ok: all`，並列出去除內容的紀錄位置。Claude精確輸出`W05_CLAUDE_CODE_OK`，Codex精確輸出`W06_CODEX_OK`。每個Client最多45秒；結束或中斷會停止Gateway與假供應商，Gateway先複製到本次測試目錄再執行。啟動或健康檢查失敗直接結束，不重試資料庫連線。

## 保存內容

最新版為`tests/fixtures/client-roundtrip/captures/claude-code-2.1.318.json`與`codex-cli-0.166.0.json`。紀錄只保留路徑、查詢參數名稱、模型、串流模式、工具數量及類型、推理參數是否存在、必要標頭名稱，以及憑證替換與Gateway金鑰未轉送的布林結果。不保存提示、回覆、憑證、完整標頭、完整請求或使用者識別。

`check-client-captures.py`檢查允許欄位及預期值；`verify-client-roundtrip.sh`再與當次輸出逐項比對。故意把Codex最新版紀錄的`client_credential_absent`改為false，檢查確實失敗，復原後通過；證據`/tmp/gateway-client-capture-negative.log`。

## 驗證邊界

只證明這兩個版本能設定自訂入口、送出請求、讀完假串流並輸出預期文字。工具非空清單及推理參數是請求格式證據，不代表工具呼叫與結果往返完成；真實帳號、模型、帳務、保存、多人隔離、取消與未來版本均未由本測試驗證。為專注傳輸，測試Gateway明確停用內容保存，不能當作P2證據。

Codex仍對未知模型顯示本機資料不足的提示，沒有查Gateway模型清單；腳本只檢查Codex執行期間的Gateway日誌，避免Claude的請求造成誤報。Claude也會提示測試模型沒有已知上下文大小，不把提示消失當成通過條件。實測工具數量為Claude12個、Codex9個；數量會隨版本變動，不代表完整工具能力。

最終通過的執行目錄為`/home/cash/.cache/tmp/apg-client-roundtrip.nVf7X2`，含當次健康頁、Client輸出、Gateway日誌及去除內容的紀錄。最新重跑刻意在父環境開啟Bedrock與Vertex路由，Client仍僅走本機假供應商。臨時服務登記已讀回為空；另以程序完整參數檢查沒有本次Gateway及假供應商，Gateway連接埠也已關閉，證據`/tmp/gateway-client-process-closeout.json`。

## 原 Client 身分與群組隔離

`verify-client-identity.py`沿用同一假供應商，改以真正管理API建立兩位有個人API金鑰的測試使用者，以及一位尚未設定憑證的使用者。各自的模型白名單、個人憑證、群組及成員都由管理API設定，不用直接SQL繞過產品路徑。只接受本機且名稱以`gateway_client_identity_validation_`開頭的隔離庫，還須明確開啟寫入旗標；不替使用者清除既有資料。

```sh
python3 ~/tools/temp-service.py start --name gateway-client-identity --ttl 10m --purpose 原Client身分隔離驗收 -- env DATABASE_URL=postgres://user:password@127.0.0.1:5432/gateway_client_identity_validation_unique CLIENT_IDENTITY_ALLOW_DATABASE_MUTATION=1 python3 scripts/verify-client-identity.py --gateway-binary /absolute/path/to/built/gateway --revision <該執行檔版本>
```

先依目前版本建置執行檔，腳本再複製到本次測試目錄並核對健康頁版本。供應商在同一程序內只綁本機，Gateway只有本次測試環境，原Client也只收到測試設定；不使用真實供應商憑證。若隔離庫已被其他驗收初始化，會直接失敗，不能盲目清庫後重送。

預期七項為：Alice及Bob各自選對憑證、Alice不能用Bob模型、未設定憑證拒絕、撤銷個人憑證後拒絕、同帳號群組後備可用、移除成員後拒絕。正向逐筆核對本輪模型與唯一認證標頭，不能只讀最後一筆；所有Gateway金鑰及其他假供應商憑證均不得出現在請求。已撤銷的假憑證也保留在檢查及去敏集合內。負向另外直接確認403或503，再由原Client實送，確認上游沒有新增請求。

Claude對503會繼續重試；腳本30秒後停止該次Client並明列退出碼124。這能證明Gateway拒絕及沒有送到上游，不能聲稱Client已正常呈現錯誤或自動停止重試。結果只寫去內容的摘要，Client診斷先移除全部測試憑證；Gateway、假供應商及Client均須在驗收結束後確認停止。這不是第二真人、真實供應商或跨主機的證據。


2026-11-05 追加 `--client codex`，以原Codex CLI 0.166.0、自訂供應商及Responses路徑執行相同七項；預設仍為Claude。Gateway版本706e719，七項全部通過，三個成功案例各送出一次上游請求，四個拒絕案例上游均為零且Client退出碼1。結果位於`/home/cash/.cache/tmp/gateway-client-identity-88cr0em7/result.json`；兩個連接埠與Gateway程序已停止，收尾證據`/tmp/gateway-codex-identity-closeout.json`。本輪僅驗個人及群組API金鑰，不涵蓋ChatGPT的CodexFile帳號持有人綁定、工具往返、內容保存或正式部署。

CodexFile持有人另有`routes::tests::codex_owner::codex_file_owner_is_enforced_by_the_responses_route`：模型、身分、政策及請求經真正HTTP路由，供應商為本機假服務。本人成功、同租戶其他人與刪除供應商憑證檔後均拒絕，後兩者不增加上游請求。故意移除owner比對會使測試失敗。這個測試不啟動原Codex程式、不連ChatGPT，也不代表Gateway金鑰撤銷。
