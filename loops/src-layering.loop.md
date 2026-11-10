# LOOP: src-layering

| 欄位 | 值 |
| --- | --- |
| 狀態 | 暫停（等 gateway-completion 整合） |
| 開始 | 2026-10-31 |
| 結束 | 尚未完成 |

## 目的
整理 src 分層，SQL 獨立存檔並使用 SQLx 檔案巨集，依職責決定是否拆檔。

## 完成判準
- [ ] SQL 獨立存檔，固定查詢使用 SQLx 檔案巨集。
- [ ] 移除 src 程式註解，保留字串及程式行為，確認文件測試影響。
- [ ] 路由、服務與資料存取分層，維持 API、交易及權限行為。
- [ ] 完整檢查、失敗情境對照及原 Client 實送通過。

## 已知
另一工作樹有重疊的未提交修改；目前工作樹位於 feature/identity-groups。

## 已做
路由大型測試移出主檔；快檢及測試編譯通過。Claude Code 已唯讀審查計畫與差異；補入容器離線建置、註解理由保存與工作樹整合順序，並縮小測試輔助函式可見範圍。路由測試實跑 1 項通過，其餘未跑。

## 已確定不可行
無。

## 證據
`src/routes.rs`、`src/routes/tests.rs`、`./scripts/check.sh gate`。

## 來源
使用者要求與 `docs/src-architecture-refactor-plan.md`。

## 下一步
先整合 `gateway-completion`（見計畫「整合順序決定」），再於整合後版本重做測試搬移；之後以能辨別 Rust 字串的方式移除註解，並在隔離資料庫逐領域搬移查詢。
