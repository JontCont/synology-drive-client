use serde::Serialize;
use tauri::{AppHandle, Runtime};

use crate::{config, storage, sync_model};

const RETRYABLE_MAX_ATTEMPTS: i64 = 3;

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ExecutorRunResult {
    pub processed: bool,
    pub operation_id: Option<String>,
    pub stage: Option<String>,
    pub outcome: String,
    pub message: String,
}

pub fn run_once<R: Runtime>(app: &AppHandle<R>) -> Result<ExecutorRunResult, String> {
    let claimed = storage::claim_next_operation(app)?;
    let Some(claimed) = claimed else {
        return Ok(ExecutorRunResult {
            processed: false,
            operation_id: None,
            stage: None,
            outcome: "idle".to_string(),
            message: "目前沒有 planned operation".to_string(),
        });
    };

    let sync_mode = config::load(app)?.sync_mode;
    if !sync_model::is_operation_allowed(&sync_mode, &claimed.op_type) {
        let updated = storage::update_operation_outcome(app, &claimed.id, "done", None)?;
        return Ok(ExecutorRunResult {
            processed: true,
            operation_id: Some(updated.id),
            stage: Some(updated.stage),
            outcome: "skipped".to_string(),
            message: format!("依同步模式（{sync_mode}）略過 {}", claimed.op_type),
        });
    }

    if claimed.op_type == "upload_retry" {
        if claimed.attempts < 2 {
            let error = "模擬暫時性錯誤，稍後重試".to_string();
            let updated = storage::schedule_operation_retry(app, &claimed.id, &error, 10)?;
            return Ok(ExecutorRunResult {
                processed: true,
                operation_id: Some(updated.id),
                stage: Some(updated.stage),
                outcome: "retry_scheduled".to_string(),
                message: error,
            });
        }
        let updated = storage::update_operation_outcome(app, &claimed.id, "done", None)?;
        return Ok(ExecutorRunResult {
            processed: true,
            operation_id: Some(updated.id),
            stage: Some(updated.stage),
            outcome: "done".to_string(),
            message: "retryable operation 重試成功".to_string(),
        });
    }

    if claimed.op_type == "upload_retry_hard" {
        if claimed.attempts < RETRYABLE_MAX_ATTEMPTS {
            let error = format!(
                "模擬持續性錯誤（第 {}/{} 次）",
                claimed.attempts, RETRYABLE_MAX_ATTEMPTS
            );
            let updated = storage::schedule_operation_retry(app, &claimed.id, &error, 10)?;
            return Ok(ExecutorRunResult {
                processed: true,
                operation_id: Some(updated.id),
                stage: Some(updated.stage),
                outcome: "retry_scheduled".to_string(),
                message: error,
            });
        }
        let error = format!("超過最大重試次數（{}）", RETRYABLE_MAX_ATTEMPTS);
        let updated = storage::update_operation_outcome(app, &claimed.id, "failed", Some(&error))?;
        return Ok(ExecutorRunResult {
            processed: true,
            operation_id: Some(updated.id),
            stage: Some(updated.stage),
            outcome: "failed".to_string(),
            message: error,
        });
    }

    let supported = matches!(
        claimed.op_type.as_str(),
        "upload" | "download" | "delete" | "rename" | "scan_local_path"
    );
    if !supported {
        let error = format!("不支援的 op_type: {}", claimed.op_type);
        let updated =
            storage::update_operation_outcome(app, &claimed.id, "failed", Some(&error))?;
        return Ok(ExecutorRunResult {
            processed: true,
            operation_id: Some(updated.id),
            stage: Some(updated.stage),
            outcome: "failed".to_string(),
            message: error,
        });
    }

    let updated = storage::update_operation_outcome(app, &claimed.id, "done", None)?;
    Ok(ExecutorRunResult {
        processed: true,
        operation_id: Some(updated.id),
        stage: Some(updated.stage),
        outcome: "done".to_string(),
        message: "已完成一筆 operation（骨架流程）".to_string(),
    })
}
