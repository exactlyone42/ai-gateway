# 2026-11-05 優先工作與查證邊界

前次僅盤點並更新計畫；2026-11-05 本輪使用者要求 /goal 執行，已開始查證與全套檢查，尚未合併或部署。正式服務僅唯讀查詢健康頁。母檔為 `../gateway-completion.loop.md`；本檔保存判斷依據，歷史子檔保留。

## 目標與架構判斷

目標是每人用自己的帳號，Gateway 管理身分、政策、額度、模型連線與加密紀錄；工具及對話壓縮由 Client／Agent 決定。現有程式已具串流、認證、政策、帳本與多供應商骨架，因此優先收斂帳號隔離、資料可恢復與完整使用流程。先做大規模分層會與既有改動衝突，不能解除目前 CI 或交付缺口，排在整合後。

## 現場證據

- `git worktree list --porcelain`：主工作樹 feature/identity-groups @18660d2，有暫存及未提交修改；groups @1c80096、completion @34b7db0、codex @e2081c0。後三者本次 `git status --short` 均無輸出。保留所有工作樹。
- 公開 `https://gateway.exactlyone.dev/health` 回 revision=63fe6d0bef32、status=ok。GitHub main=71f746c（#110），故原始碼主幹與服務版本不同；#110 的部署資源標記與驗收紀錄是否需要再部署須按實際差異判斷，不能直接寫相同。
- Docker 顯示 63fe6d0 候選服務綁 127.0.0.1:18101，舊 d1c29e8 容器亦仍執行。accepted.json 的 accepted=true、old_stopped=false 只證明收據內容；此次未重跑 Client，未判定舊容器可刪。
- `gh pr list`：#108 feature/gateway-completion 待合到 main；#106、#94 不屬本工作，不處理。`gh pr view 108` 的 rust=FAILURE，mergeable=UNKNOWN。run 41794503227 直接顯示 SQLx 查詢相關編譯錯誤，capture_remote_witness.rs:56 有 E0599；先找完整 log 中第一個錯誤再決定修法，不能僅憑尾端連帶錯誤改型別或壓警告。
- `git ls-remote --heads origin dev develop main` 只回 main；本地 origin/develop 是快取，不能照舊文件假設遠端仍有 dev/develop。現有 #108 目標為 main；沿用當前有效 PR 與保護規則，不自行重建整合分支。
- groups 的兩個提交可由 git log 查得；其 LOOP 記載完整測試及瀏覽器驗收，但本次未重跑，故只確認產物存在，不宣稱目前整合版本通過。
- completion 的 held_token.rs 已有 PersonalTokens 及個人讀取／撤銷；src 有 capture、備份屏障與服務模組，腳本有備份還原測試。存在不等於正式啟用或隔離驗收通過。
- 04 子檔最近紀錄有不停機備份取鎖、3 秒串流排空、程序被終止及隔離還原；本次未重跑。該紀錄也保留 7 項略過及正式驗收缺口，不再把舊的「98 個未提交檔／尚未推送／仍暫停」當現況。
- Codex #109、#110 已合；codex-held-auth LOOP 與正式 accepted.json 存在。本次僅核對原始碼及收據，不把舊實送當新整合版本的驗收。

## 三項優先工作及完成門檻

| 順序 | 工作與理由 | 完成門檻 |
| --- | --- | --- |
| P1 | 修復 #108 CI 並整合現有成果；目前成果分散且 CI 紅，後續功能沒有可信共同基準 | 查清第一個 CI 失敗；以相同工具鏈及隔離資料庫重現，修復並通過 all／CI；接入最新 main 的 Codex 支援與 groups S01–S03，核對 migration、SQLx 離線資料、API／快取／權限及前端；保留群組硬限制、個人預設優先與嚴格額度覆蓋，全部陰性對照通過。每個整合切片過關才合，禁止紅燈部署 |
| P2 | 完成個人／群組權杖隔離與加密保存、備份還原；這決定是否誤用帳號、遺失或復活已刪內容 | 先核現有隔離測試，再實作 W21 S04：個人優先、本人帳號的群組後備、缺綁定拒絕、跨租戶／撤權／並行更新拒絕且不洩憑證；確認所有寫入者受備份契約保護，最小權限、同內容可重播／不同內容拒絕／整批回滾、損毀與中斷恢復、到期與刪除及備份不可復活、容量及串流超過鎖等待時間均有測試。隔離還原需對回事件與帳本，不以 receipt 代替 |
| P3 | 完成原 Client 全流程並推出；證明整合成果可持續使用 | Claude Code、Codex 各跑對話→工具→回傳→最終答覆，涵蓋串流、取消、錯誤、用量對帳及加密讀回；本機／遠端與各來源分開列；測試環境、全套、CI、部署、公開版本、原 Client、回退與告警分別有證據。自家 Agent 摘要工作歸 agent-platform，以現有母檔接續，不在 Gateway 重做 runtime；跨 repo 項未驗仍保留未完成 |

P1 的 CI 修復先做；群組整合遇重疊時先完成 completion 同路徑整合，再移入群組成果。P2 可先核對已有測試，避免重寫；P3 所需測試工具可提早準備，整體驗收須針對最後整合版本。

## 取捨與授權邊界

沿用 2026-11-01 群組權杖、2026-10-29 先存後送及供應商憑證只在伺服器、2026-10-31 Client 自管壓縮的決策。每人僅用自己帳號，不把群組後備變成不同人的席次共用。採原有不停機備份設計：短暫拒絕有明確重試時間，串流持鎖超時則備份失敗；不為舊 guardian 設計另造常駐服務。

本次要求是盤點、更新母檔及提供執行 prompt，未直接啟動 P1–P3。未來執行沿用部署及本人 PR 合併授權，但不新增付費、刪除、替他人登入或其他外部寫入授權。正式加密備份若涉及原資料複製、保留位置或獨立見證服務尚未獲明確授權，先完成隔離演練並保留該缺口，繼續其他工作；不能因部署授權推定這些也全被授權。

第二位真實訂閱者、另一台主機、缺少的 Client／金鑰、告警接收端與自家 Agent 驗收若無現成授權資源，標明未驗，不用模擬替代；繼續所有無依賴工作，最後一次彙整。資源或 Claude 額度不足改用程式與測試查證，不反覆重試也不以此停整個任務。

## 2026-11-05 執行前複核：以核心功能定優先序

三項以使用者能得到的結果命名，CI 與整合是前置，不列為獨立產品成果。

1. **各帳號身分、權限與憑證隔離（P1）**：沒有這項，換使用者可能誤用別人的帳號或突破額度。先整合群組管理及 main 的 Codex 支援，再完成 S04 的個人優先、同帳號群組後備與撤銷邊界；陰性對照須證明跨租戶、無綁定、撤權、群組硬限制都能拒絕且沒有上游請求。管理者設定群組不構成分享不同人訂閱的授權。
2. **對話與用量可靠保存及恢復（P2）**：沒有這項，使用紀錄可能遺失、重複入帳或復活已刪內容。沿用現有加密與帳本，先驗同內容可重播、異內容拒絕、整批回滾，再驗隔離備份還原對回事件內容、帳本與刪除紀錄。正式備份未獲額外授權時不執行，保留缺口。
3. **Claude Code／Codex 日常工具操作可用（P3）**：沒有工具要求、結果及最終回覆的完整往返，健康頁不能證明可工作。針對最後整合版本跑兩個原 Client，分別記本機、公開入口、串流、取消、失敗與帳務證據；通過發布門檻後部署，核對公開版本，保存全部異動與忽略產物後清理閒置工作樹。

本輪直接證據：fetch 後 origin/main=71f746c；#108 為 CONFLICTING、rust FAILURE；完整 CI 首個編譯錯誤為 capture_erasure_marks 不存在，並非 remote witness 的型別錯誤。CI 建立空資料庫且編譯前沒有遷移；分支已有 .sqlx 離線資料，優先採官方 SQLx 0.8.6 離線編譯方法，執行期測試仍連隔離資料庫，不能用離線編譯取代資料庫測試。主工作樹 all 的程序及輸出在 /tmp/gateway-closeout-primary-check.log，結果須等結束碼確認。

目前順序：保存全部工作 → P1 的共同版本與隔離 → P2 → P3。程式分層、額外供應商、智慧路由與摘要研究不搶先；原範圍與未驗項保留，不藉重排宣稱整體完成。

## P1 執行期審查發現

唯讀審查後親讀程式確認：services/openai.rs:257 與 routes/messages.rs:233 對正式設定的串流且啟用內容保存回 CaptureUnavailable。P2 必須補齊並以正式設定驗串流；不能只靠 dev 或 cfg(test) 測試宣稱正式可用。

S04 必守：個人讀取只有 Ok(None) 才可找群組；讀取錯誤、損壞或鎖忙須拒絕。群組使用獨立檔案命名空間；多個可用群組憑證拒絕歧義。查會員至上游回標頭間保留會員列共享鎖及權杖檔鎖，避免移除成功後仍送出舊憑證；整合不可遺失 user 政策不快取、群組硬限制、嚴格額度覆蓋與租戶鎖交易。來源是 held_token.rs、state.rs、groups 的 admin/groups.rs、policy/evaluate.sql 及 quota/active.sql，具體實作前再次核對。

## P1 檢查紀錄

主樹 all 通過，Rust 246 項、前端 2 項、守門正反對照 32 項；提交 aded13a 已推送。推送首回缺 DATABASE_URL 導致 57 項失敗，補隔離資料庫設定後推送通過。目標 PDF 轉透明 PNG 檢查，頁內 alpha 全 255。被忽略的驗收資料與 var 已存 ~/.local/state/gateway-closeout-20261105/ignored-evidence.tar.gz，7 個檔案雜湊已讀回。

completion 首次 all 主程式443通過／7略過；兩項 capture_startup 在跨樹推送同時執行時跑到不同版本執行檔，單獨重驗2通過。後續 all 又遇 missing_record_with_deleted_spool_must_not_be_imported 及 an_unreachable_upstream_is_logged_with_its_cause 失敗；未當作通過。日誌測試沿用 groups 既有空收集器初始化，子程序的全域收集器改獨立建立；刪除測試在負向鎖驗證後明確 unlock，避免只 drop 時被並行建立子程序暫時繼承。這些只動測試；機制仍需區分推論與直接失敗。子程序日誌測試1通過／449未跑；最新全套 /tmp/gateway-closeout-completion-check-stable.log 待完成。

測試修正的官方參考（2026-11-05）：https://doc.rust-lang.org/std/fs/struct.File.html 的 unlock／lock（File 檔案鎖與解鎖）；https://docs.rs/tracing/latest/tracing/dispatcher/fn.set_global_default.html（程序全域收集器只能設定一次）。沿用 groups 現成初始化，保留獨立子程序測試，未修改正式日誌路徑。

最新全套已結束碼0：/tmp/gateway-closeout-completion-check-stable.log。Rust 490 次測試通過（含日誌檢查子程序）、7 略過；Python 25＋157 項中20略過；前端2項與守門33/33通過。未執行需獨立見證／特定擁有者／額外環境開關的驗收，不能當作正式完成。正式 /health 實讀仍為63fe6d0bef32。

main 已於0fa238b整合到工作分支；群組整合的 gate、Rust workspace 520 次通過／7 略過、前端 lint／型別／3測試／建置通過（/tmp/gateway-closeout-merge-groups-tests.log、-frontend.log、-gate.log）。這不是 main 合併或部署；群組權杖、Codex 使用者綁定及正式串流保存仍是未完成門檻。
