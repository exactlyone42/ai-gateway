# LOOP: request-evidence

> 卡 [#64](https://github.com/DnaBoss/agent-pool-gateway/issues/64)｜分支 `feature/request-evidence`｜段落 `S08`

| 欄位 | 值 |
| --- | --- |
| 狀態 | 已完成 |
| 優先度 | P1 |
| 前置 | usage-ledger、canonical-layer |
| 開始 | 2026-10-21T08:45+08:00 |
| 結束 | 2026-10-21T08:51+08:00 |
| Token | 由 `./scripts/token-ledger.sh` 填 |

## 目的
以不保存內容的參數快照與既有用量、延遲、成本資料，提供可比較的改善證據。

## 完成判準
- [x] 每筆已解碼請求保存進站與實際出站的安全參數快照，含模型、生成限制、思考模式及結構計數。
- [x] 快照不含 prompt、response、system 內容、工具名稱／參數、header 或憑證，大小由 DB constraint 限制。
- [x] Admin request detail 顯示進站／出站差異；文件定義可靠度、延遲、成本可證明與不可證明的範圍。
- [x] migration、後端與前端契約、格式、靜態檢查及完整測試通過。
- [x] 陰性對照：哨兵 prompt、system、工具名稱未進快照；未知及巢狀秘密欄位被 DB 拒絕。

## 已知
- `gateway_requests` 已記原始／選中／Provider 模型、路由、延遲、用量與成本，但未記轉送前後參數。
- 預設不保存 request／response body；本工作只允許固定白名單的純量與數量。
- 既有 request detail 直接回傳 `gateway_requests`，新增欄位可沿用 tenant 隔離與權限。

## 已做
- 2026-10-21：完成 loop／issue／branch 查重，建立 #64 與唯一 feature branch；載入 DB migration 規範。
- 2026-10-21：完成兩階段 append-only 快照、Admin 顯示與改善量測契約；隔離資料庫全套驗收通過。

## 已確定不可行
- 保存完整 JSON request／provider body：會違反內容不落盤與秘密防護規則。

## 證據
- `gateway_requests` schema 與 `src/ledger.rs`：目前只有模型與營運 metadata，沒有生成參數快照。
- `src/admin/reports.rs::request_report`：詳情端點已有 tenant scope，可安全擴充固定欄位。
- `./scripts/check.sh all`：Rust 249、DB 5、前端 2、守門 32 全綠，秘密掃描無命中；0 skipped。
- DB 陰性對照拒絕未知欄位、巢狀秘密與 UPDATE；故障恢復測試保存兩份快照且重播不重複。
- 本地 reviewer 先因輸入 72,195 tokens 被拒，縮小至核心兩檔後 45 秒逾時；未把模型意見當驗收。

## 來源
- 使用者 2026-10-21 指示：記錄真實改善數據，支撐可靠度與節省經費主張。

## Trade-off
| 決策 | 選了什麼 | 放棄了什麼 | 為什麼 |
| --- | --- | --- | --- |
| 儲存 | 兩份有大小限制的白名單 JSONB | 完整 body 或大量 nullable 欄位 | 跨協定可演進，且不保存內容 |
| 證據 | request-level 原始／出站快照 | 只記彙總 | 可追查 Gateway 是否改參數 |

## 下一步
1. 合併後由部署流程套用 migration；部署前不會開始收集新資料。
