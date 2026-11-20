use std::fs;
use std::path::{Path, PathBuf};

pub fn collect_local_paths(root_path: &str, limit: u32) -> Result<Vec<String>, String> {
    if root_path.trim().is_empty() {
        return Err("本機同步資料夾不可為空".to_string());
    }
    let root = PathBuf::from(root_path);
    if !root.exists() {
        return Err("本機同步資料夾不存在".to_string());
    }
    let max = limit.clamp(1, 1000) as usize;
    let mut stack = vec![root.clone()];
    let mut out = Vec::new();

    while let Some(path) = stack.pop() {
        if out.len() >= max {
            break;
        }
        let entries = match fs::read_dir(&path) {
            Ok(it) => it,
            Err(_) => continue,
        };
        for entry in entries {
            if out.len() >= max {
                break;
            }
            let Ok(entry) = entry else {
                continue;
            };
            let p = entry.path();
            if should_skip(&p) {
                continue;
            }
            if p.is_dir() {
                stack.push(p);
                continue;
            }
            if p.is_file() {
                out.push(p.to_string_lossy().to_string());
            }
        }
    }

    Ok(out)
}

fn should_skip(path: &Path) -> bool {
    path.file_name()
        .and_then(|n| n.to_str())
        .map(|name| name.eq_ignore_ascii_case(".ds_store") || name.eq_ignore_ascii_case("thumbs.db"))
        .unwrap_or(false)
}
