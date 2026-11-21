# Remote Spike（WebDAV / Drive API）

> 目的：在真實 NAS 上驗證「登入可行性 + 安全寫入能力 + 變更偵測能力」，最後只選一個 Provider 進 Phase 1。

## 0. 測試環境
- 日期：
- 測試人：
- DSM 版本：
- Drive Server 版本：
- WebDAV Server 版本：
- NAS 位址（去識別）：
- 憑證型態（正式 / 自簽）：
- 帳號型態（local / domain / LDAP）：
- 2FA 狀態：

## 1. 登入與授權（帳密/OTP）
| 項目 | WebDAV | Drive API | 結果摘要 |
|---|---|---|---|
| 帳密登入 |  |  |  |
| OTP 驗證 |  |  |  |
| session 過期後重登入 |  |  |  |
| 可取得授權資料夾 |  |  |  |

## 2. 寫入安全性（必要）
| 項目 | WebDAV | Drive API | 結論 |
|---|---|---|---|
| 版本前置條件（避免盲覆蓋） |  |  |  |
| 目的檔已改變時是否正確衝突 |  |  |  |
| 刪除前條件檢查 |  |  |  |
| MOVE/rename 語意 |  |  |  |
| 失敗與未知結果恢復策略 |  |  |  |

## 3. 同步能力
| 項目 | WebDAV | Drive API | 結果 |
|---|---|---|---|
| list/stat |  |  |  |
| upload/download |  |  |  |
| Range |  |  |  |
| 變更清單/token |  |  |  |
| 大檔（>2GB） |  |  |  |
| 權限/鎖定錯誤回應 |  |  |  |

## 4. 效能與穩定性
- 小檔批量（項目數 / 耗時）：
- 大檔傳輸（檔案大小 / 耗時 / 重試）：
- 輪詢（間隔 / 請求量）：
- 異常情境（斷線、session 過期、NAS 重啟）：

## 5. 安全檢查
- 憑證驗證是否全程啟用：
- 自簽憑證流程是否可用：
- 是否有 token/密碼寫入 log：
- 是否出現跨 origin 秘密傳送：

## 6. Provider 決策
- 建議 Provider（擇一）：
- 決策理由：
- 不採用項目的主要阻礙：
- 對 Phase 1 的風險與緩解：

## 7. 後續動作
- [ ] 更新 `docs/tasks.md` T1 結果
- [ ] 更新 `docs/plan.md` Provider 決策
- [ ] 將不可行登入路徑標示為限制，不在 MVP 內假承諾

