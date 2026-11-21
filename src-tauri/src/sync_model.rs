#![allow(dead_code)]

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VersionedSnapshot {
    pub exists: bool,
    pub version: Option<String>,
    pub content_hash: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SyncAction {
    Noop,
    Upload,
    Download,
    DeleteRemote,
    DeleteLocal,
    Conflict,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SyncDecision {
    pub action: SyncAction,
    pub reason: &'static str,
}

pub fn decide_action(
    local: &VersionedSnapshot,
    remote: &VersionedSnapshot,
    baseline: &VersionedSnapshot,
) -> SyncDecision {
    if local == baseline && remote == baseline {
        return SyncDecision {
            action: SyncAction::Noop,
            reason: "local and remote unchanged",
        };
    }

    let local_changed = local != baseline;
    let remote_changed = remote != baseline;

    if local_changed && !remote_changed {
        return SyncDecision {
            action: if local.exists {
                SyncAction::Upload
            } else {
                SyncAction::DeleteRemote
            },
            reason: "local changed only",
        };
    }

    if !local_changed && remote_changed {
        return SyncDecision {
            action: if remote.exists {
                SyncAction::Download
            } else {
                SyncAction::DeleteLocal
            },
            reason: "remote changed only",
        };
    }

    if local.content_hash.is_some()
        && remote.content_hash.is_some()
        && local.content_hash == remote.content_hash
    {
        return SyncDecision {
            action: SyncAction::Noop,
            reason: "both changed but content identical",
        };
    }

    SyncDecision {
        action: SyncAction::Conflict,
        reason: "both changed with different content",
    }
}

/// 依同步模式判斷某類 operation 是否允許執行。
/// 單向模式只搬運資料（上傳或下載），永遠不執行刪除與改名，避免對目的端造成刪除效果；
/// 僅下載模式也不接收本機變更。
pub fn is_operation_allowed(mode: &str, op_type: &str) -> bool {
    match mode {
        "upload_only" => matches!(
            op_type,
            "upload" | "upload_retry" | "upload_retry_hard" | "scan_local_path"
        ),
        "download_only" => op_type == "download",
        _ => true,
    }
}

#[cfg(test)]
mod tests {
    use super::{decide_action, is_operation_allowed, SyncAction, VersionedSnapshot};

    #[test]
    fn sync_mode_policy_blocks_delete_in_one_way_modes() {
        for op in ["upload", "download", "delete", "rename", "scan_local_path"] {
            assert!(is_operation_allowed("bidirectional", op));
        }
        assert!(is_operation_allowed("upload_only", "upload"));
        assert!(is_operation_allowed("upload_only", "scan_local_path"));
        assert!(!is_operation_allowed("upload_only", "download"));
        assert!(!is_operation_allowed("upload_only", "delete"));
        assert!(!is_operation_allowed("upload_only", "rename"));
        assert!(is_operation_allowed("download_only", "download"));
        assert!(!is_operation_allowed("download_only", "upload"));
        assert!(!is_operation_allowed("download_only", "delete"));
        assert!(!is_operation_allowed("download_only", "scan_local_path"));
    }

    fn snap(exists: bool, version: Option<&str>, hash: Option<&str>) -> VersionedSnapshot {
        VersionedSnapshot {
            exists,
            version: version.map(ToString::to_string),
            content_hash: hash.map(ToString::to_string),
        }
    }

    #[test]
    fn decides_upload_when_local_changes_only() {
        let baseline = snap(true, Some("v1"), Some("h1"));
        let local = snap(true, Some("v2"), Some("h2"));
        let remote = baseline.clone();

        let decision = decide_action(&local, &remote, &baseline);
        assert_eq!(decision.action, SyncAction::Upload);
    }

    #[test]
    fn decides_conflict_when_both_change_differently() {
        let baseline = snap(true, Some("v1"), Some("h1"));
        let local = snap(true, Some("v2"), Some("h2"));
        let remote = snap(true, Some("v3"), Some("h3"));

        let decision = decide_action(&local, &remote, &baseline);
        assert_eq!(decision.action, SyncAction::Conflict);
    }
}
