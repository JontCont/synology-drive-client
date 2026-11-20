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

    let updated = storage::update_operation_stage(app, &claimed.id, "done")?;
    Ok(ExecutorRunResult {
        processed: true,
        operation_id: Some(updated.id),
        stage: Some(updated.stage),
        message: "已完成一筆 operation（骨架流程）".to_string(),
    })
}
