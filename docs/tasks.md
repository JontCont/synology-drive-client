# Tasks

## Phase 0（先驗證）
- [ ] T0 確認需求範圍與環境（NAS 版本、Windows 版本、資料規模）
- [ ] T1 登入與 Provider Spike（帳密/OTP + OIDC/SSO）
  - [x] T1a 建立 Spike 記錄模板（`docs/remote-spike.md`）
- [ ] T2 定稿 Provider（WebDAV / Drive API 二擇一）
- [ ] T3 定義 RemoteProvider 契約與錯誤模型

## Phase 1（MVP）
- [x] P1-0 專案骨架（Tauri + React + Rust）
- [x] P1-1 系統匣與單一實例
- [x] P1-2 關窗縮系統匣
- [x] P1-3 安全基線（移除 opener、最小 capability、CSP）
- [x] P1-4 設定檔骨架（`get_app_config` / `update_app_config`）
- [x] P1-5 設定 UI（可修改與儲存設定）
- [x] P1-6 認證流程骨架（帳密/OTP + SSO）
- [ ] P1-7 同步資料模型（baseline/operations/conflicts）
  - [x] P1-7a 三方比對決策模型雛形（純函式 + 單元測試）
  - [x] P1-7b SQLite schema 骨架（sync_roots/items/observations/baseline/tombstones/operations/conflicts）
  - [x] P1-7c default sync_root 寫入/讀取（由設定檔同步到 SQLite）
- [ ] P1-8 掃描與監看（notify + 補掃描）
  - [x] P1-8a 背景同步服務骨架（可啟停、可查狀態）
  - [x] P1-8b 本機監看骨架（notify 可啟停、事件計數）
  - [x] P1-8c watcher 事件轉入 operation 佇列
  - [x] P1-8d 同步服務 tick 結果回報（lastMessage / lastOperationId / lastStage）
  - [x] P1-8e 補掃描骨架（手動掃描 local path 並入列 operation）
- [ ] P1-9 三方比對與 executor
  - [x] P1-9a 操作佇列骨架（SQLite enqueue/list + 前端檢視）
  - [x] P1-9b executor 單步執行骨架（claim planned → done）
  - [x] P1-9c sync cycle 單步入口（先 init storage，再跑 executor）
  - [x] P1-9d operation 重試入口（指定 id 重設為 planned）
  - [x] P1-9e demo operation 批次灌入（快速壓測骨架）
  - [x] P1-9f 一鍵 pipeline（watcher drain + executor）
  - [x] P1-9g executor 失敗階段寫回（failed + last_error）
  - [x] P1-9h retryable/backoff 骨架（next_retry_at + due 重試）
- [ ] P1-10 真實 NAS 故障矩陣驗證
- [x] P1-11 系統快照指令（設定/狀態/佇列數量集中檢視）

## Phase 2（Files On-Demand）
- [ ] P2-1 cfapi Spike（CloudMirror / CfRegisterSyncRoot）
- [ ] P2-2 placeholder 建立與 hydration
- [ ] P2-3 dehydration/釘選與狀態協調

