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
