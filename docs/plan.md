# Synology Drive Client 開發藍圖

## 1) 技術方向
- Desktop：Tauri 2
- Sync Engine：Rust
- UI：React + TypeScript
- 平台：Windows（優先）

## 2) 產品目標（MVP）
- 本機資料夾與 NAS 遠端資料夾雙向同步
- 系統匣常駐、關窗縮到系統匣、單一實例
- 衝突可見且不盲目覆蓋
- 斷線恢復與持久化狀態

## 3) 驗證優先順序
1. 先做登入與 Provider Spike（帳密/OTP + OIDC/SSO）
2. 再決定 Phase 1 的正式 Provider（WebDAV 或 Drive API）
3. 再進同步核心與 UI 整合

## 4) 安全基線
- 全程 HTTPS，預設啟用憑證驗證
- 不提供全域略過 TLS 驗證
- token/憑證只在後端與安全儲存，不能回傳前端
- IPC 最小權限；前端不能拿到敏感資訊
- 外部輸入（檔名/遠端回應）一律視為資料，不當指令執行
- 不把同步內容送第三方 AI

## 5) 同步核心原則
- 三方比對（local / remote / baseline）
- 破壞性操作必須做版本/條件檢查
- 掃描不完整時不可推導刪除
- 大量刪除或大量改名/覆寫要先暫停並提示

## 6) 目前已落地
- Tauri 專案骨架建立完成
- 單一實例完成
- 關窗縮系統匣完成
- 系統匣選單（開啟/結束）完成
- 基礎安全收斂（移除 opener、能力最小化、CSP）
- 設定檔骨架完成（`settings.json`）

## 7) 下一步
1. 補齊設定 UI（編輯/儲存 localSyncPath、remotePath、pollIntervalSeconds）
2. 登入流程骨架（帳密/OTP + SSO）
3. Provider Spike（可重現測試腳本與報告）
4. RemoteProvider 契約與 FakeRemote

