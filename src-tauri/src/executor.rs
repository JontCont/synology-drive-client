use serde::Serialize;
use tauri::{AppHandle, Runtime};

use crate::storage;

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ExecutorRunResult {
    pub processed: bool,
    pub operation_id: Option<String>,
    pub stage: Option<String>,
    pub message: String,
}

pub fn run_once<R: Runtime>(app: &AppHandle<R>) -> Result<ExecutorRunResult, String> {
    let claimed = storage::claim_next_operation(app)?;
    let Some(claimed) = claimed else {
        return Ok(ExecutorRunResult {
            processed: false,
            operation_id: None,
            stage: None,
            message: "目前沒有 planned operation".to_string(),
        });
    };

    let supported = matches!(claimed.op_type.as_str(), "upload" | "download" | "delete" | "rename");
    if !supported {
        let error = format!("不支援的 op_type: {}", claimed.op_type);
        let updated =
            storage::update_operation_outcome(app, &claimed.id, "failed", Some(&error))?;
        return Err(format!("operation {} 失敗：{}", updated.id, error));
    }

    let updated = storage::update_operation_outcome(app, &claimed.id, "done", None)?;
    Ok(ExecutorRunResult {
        processed: true,
        operation_id: Some(updated.id),
        stage: Some(updated.stage),
        message: "已完成一筆 operation（骨架流程）".to_string(),
    })
}
