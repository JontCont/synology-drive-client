# Tasks

## Phase 0（先驗證）
- [ ] T0 確認需求範圍與環境（NAS 版本、Windows 版本、資料規模）
- [ ] T1 登入與 Provider Spike（帳密/OTP + OIDC/SSO）
- [ ] T2 定稿 Provider（WebDAV / Drive API 二擇一）
- [ ] T3 定義 RemoteProvider 契約與錯誤模型

## Phase 1（MVP）
- [x] P1-0 專案骨架（Tauri + React + Rust）
- [x] P1-1 系統匣與單一實例
- [x] P1-2 關窗縮系統匣
- [x] P1-3 安全基線（移除 opener、最小 capability、CSP）
- [x] P1-4 設定檔骨架（`get_app_config` / `update_app_config`）
- [ ] P1-5 設定 UI（可修改與儲存設定）
- [ ] P1-6 認證流程骨架（帳密/OTP + SSO）
- [ ] P1-7 同步資料模型（baseline/operations/conflicts）
- [ ] P1-8 掃描與監看（notify + 補掃描）
- [ ] P1-9 三方比對與 executor
- [ ] P1-10 真實 NAS 故障矩陣驗證

## Phase 2（Files On-Demand）
- [ ] P2-1 cfapi Spike（CloudMirror / CfRegisterSyncRoot）
- [ ] P2-2 placeholder 建立與 hydration
- [ ] P2-3 dehydration/釘選與狀態協調

