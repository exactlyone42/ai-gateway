# LOOP: gateway-completion

> 無卡｜分支 `feature/gateway-completion`｜段落 `—`（上層總 LOOP）
> 各 step 細節見 `docs/gateway-completion-plan.md`

| 欄位 | 值 |
| --- | --- |
| 狀態 | RUNNING |
| 優先度 | P0 |
| 開始 | 2026-10-25 |
| 結束 | — |

## 目的
線上版本放進 main，已交付功能都用真實 Client 驗過，剩餘工作各有人接。

## 完成判準
- [ ] A1 `/health` revision 等於 main 上已合併 commit
- [ ] A2 原 Claude Code 經 Gateway 回 200，W21 長文件工作無 60 秒取消、無 499
- [ ] A3 `./scripts/check.sh all` 隔離資料庫全綠
- [ ] A4 S09、S12 線上讀回通過；S13 shadow 達標或記錄未達標；S14、S15 有各自 loop
- [ ] A5 identity-groups 5 項判準過或標延後
- [ ] A6 改程式的 step 皆過簡化審查

## 已知
- 公開入口目前仍由 `c8158e5d2089` 恢復版服務；新版驗收失敗已切回，舊容器保留。
- main a562116 的舊遷移內容與正式庫不同，已實測啟動失敗；本輪恢復原內容並補部署前檢查。
- 無上游金鑰；不接告警，只寫日誌（使用者 2026-10-25）。

## 已做
- 2026-10-26 原 Client 旁路與公開實送成功，暫以 c8158e5d2089 恢復服務；安全部署修改與驗收進行中。

- 2026-10-26 PR #103、#104 已合併；同資料庫雙程序與原 Client 短請求通過。跨切換長請求在 60 秒取消後續傳，原驗收誤認成功，已撤銷並切回舊版。新增單請求正常結束檢查，以實際故障紀錄確認拒絕；W21 仍未完成。

## 已確定不可行
- 無。

## 證據
- `/health` 實測 revision；`git merge-base --is-ancestor` 實測。

## 來源
- `loops/README.md`、`docs/gateway-completion-plan.md`。

## 下一步
| step | 成果 | 依賴 | 狀態 |
| --- | --- | --- | --- |
| S01 | 主 repo git 設定恢復 | — | PENDING |
| S02 | 新版實送驗收通過才切換，舊版請求完成才下線（交付點） | S01 | RUNNING |
| S03 | W21 正式驗收 | S02 | PENDING |
| S04 | 失敗測試查明 | S01 | PENDING |
| S05 | S12、S09 線上讀回 | S02 | PENDING |
| S06 | S13 shadow（交付點） | S02、S04 | PENDING |
| S07 | S14 dispatcher | S06 | PENDING |
| S08 | S15 integrator | S07 | PENDING |
| S09 | identity-groups | S04 | PENDING |
| S10 | 收尾回寫索引 | 其餘 | PENDING |

停止：缺具名授權或做不到才停；卡住先做不相依項。
