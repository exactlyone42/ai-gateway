# LOOP: policy-engine

> 主體上限 200 行（不含「已做」）、實時更新。方法見全域 `start-work` skill。
> 卡 [#17](https://github.com/DnaBoss/agent-pool-gateway/issues/17)｜分支 `feature/policy-engine`｜段落 `W11`

| 欄位 | 值 |
| --- | --- |
| 狀態 | 進行中 |
| 優先度 | P1 |
| 前置 | `identity-credential`, `model-registry-router` |
| 開始 | `2026-09-11T05:11:00+08:00` |
| 結束 | — |
| Token | 由 `./scripts/token-ledger.sh` 填，不要手寫 |

## 目的
同一把金鑰的請求，效力政策是可預測、可解釋、可稽核的——查得到「為什麼這次只給 standard」。

## 完成判準
- [ ] 政策可掛在六個層級：Global／User／Device／Client／Agent／Credential，再加 Request hint。
- [ ] 合併優先序明確且有單元測試覆蓋每一層的覆寫行為。
- [ ] **Request hint 不得突破上層 hard constraint**。
- [ ] 四個維度**互相獨立**，不得壓成單一 `priority = 1~10`：
      workload_class（critical／interactive／foreground／automation／background／batch）、
      urgency（immediate／normal／low／idle）、
      quality（economy／standard／advanced／premium）、
      budget（cheap／normal／unrestricted）。
- [ ] 支援 `deadline_at` 或 `max_queue_time`。
- [ ] 來源預設政策：例如 Claude Code = interactive／immediate／premium；
      VS Code 補全 = interactive／immediate／economy；研究 Agent = background／low／premium；
      記憶 Agent = batch／idle／economy。
- [ ] `EffectivePolicy` 寫進 `gateway_requests`，事後查得到當時的決策依據。
- [ ] **陰性對照 1**：Agent 上限 `standard`、請求 hint 要 `premium` → 實際必須是 `standard`。
- [ ] **陰性對照 2**：把某一層的 hard constraint 檢查移除 → 對應測試必須變紅。

## 已知
- 現況沒有政策概念，只有一份環境變數模型白名單。
- W08 擁有 Policy 的 tenant-safe、immutable-version draft CRUD schema；W11 接手啟用與合併行為，
  不能另建平行資料表，啟用／換版必須留下 audit。
- 規格 §23 的理由很關鍵：`autocomplete` 是「非常急 ＋ economy」，
  `architecture analysis` 是「不急 ＋ premium」。壓成單一優先權數字就表達不出來。
- 規格 §26：合併結果要存進請求記錄，否則事後無法解釋。
- 查重：沿用既有卡 #17 與本 loop；開工時沒有同名 branch 或 open PR，因此建立
  `feature/policy-engine`，不開第二張卡。
- 卡面的 company 對應 tenant-scoped Global；repo 規格與現有 identity schema 沒有 team entity，
  W11 不新增平行 team ownership。若日後引入 team，須以新 identity migration 擴充 policy scope。
- Reviewer 先嘗試 `claude -p --model opus`；若既有訂閱限制重現，改用獨立 `codex exec`。
  計畫定案與完整 diff 各審一次；高／中嚴重度須修正或以規格與實測裁定。
- 每段守門：相關 Rust/DB 測試與陰性對照；提交前 `./scripts/check.sh all`。外部依賴先自行
  打通；只做安全性、必要依賴與 W11 目標；完成後依既有授權合入 `dev`。

## 已做
- `2026-09-11T05:11+08:00` W10 merge `86395e3`、收工 commit `2251a62` 後接手；確認卡 #17、
  loop、分支與 PR ownership，無重複實作，建立 `feature/policy-engine`。

## 已確定不可行
- （待填）

## 證據
- （待填）

## 來源
- 規格 §23（分類）、§24（deadline）、§25（來源政策）、§26（合併）、§54（政策後台）。

## Trade-off
| 決策 | 選了什麼 | 放棄了什麼 | 為什麼 |
| --- | --- | --- | --- |

## 下一步
1. 四個維度的型別 ＋ 政策表 schema。
2. 合併函式 ＋ 每一層的覆寫測試 ＋ hard constraint 陰性對照。
3. `EffectivePolicy` 落帳。
