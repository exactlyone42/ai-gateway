# 原 Client 傳輸驗收

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
