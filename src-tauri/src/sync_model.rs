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

#[cfg(test)]
mod tests {
    use super::{decide_action, SyncAction, VersionedSnapshot};

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
