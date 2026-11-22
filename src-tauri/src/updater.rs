//! 透過 GitHub Releases API 檢查是否有新版安裝檔，並在背景下載與啟動安裝程式。

use std::io::Write;
use std::path::PathBuf;
use std::time::Duration;

use serde::Serialize;

const GITHUB_LATEST_RELEASE_API: &str =
    "https://api.github.com/repos/JontCont/synology-drive-client/releases/latest";
const ALLOWED_ASSET_PREFIX: &str =
    "https://github.com/JontCont/synology-drive-client/releases/download/";
const ALLOWED_RELEASE_PAGE_PREFIX: &str =
    "https://github.com/JontCont/synology-drive-client/releases/";
const USER_AGENT: &str = "SynologyDriveClient-Updater";

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AppUpdateInfo {
    pub current_version: String,
    pub latest_version: String,
    pub has_update: bool,
    pub release_name: String,
    pub release_notes: String,
    pub release_url: String,
    pub asset_name: Option<String>,
    pub asset_url: Option<String>,
    pub asset_size: Option<u64>,
}

pub fn current_version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

fn parse_semver_tuple(raw: &str) -> Option<(u64, u64, u64)> {
    let trimmed = raw.trim().trim_start_matches(['v', 'V']);
    let core = trimmed.split(['-', '+']).next()?;
    let mut parts = core.split('.');
    let major = parts.next()?.parse::<u64>().ok()?;
    let minor = parts.next().unwrap_or("0").parse::<u64>().ok()?;
    let patch = parts.next().unwrap_or("0").parse::<u64>().ok()?;
    if parts.next().is_some() {
        return None;
    }
    Some((major, minor, patch))
}

/// 比較 `latest` 是否比 `current` 更新
pub fn is_newer_version(current: &str, latest: &str) -> bool {
    match (parse_semver_tuple(current), parse_semver_tuple(latest)) {
        (Some(cur), Some(lat)) => lat > cur,
        _ => false,
    }
}

fn is_allowed_asset_url(url: &str) -> bool {
    url.starts_with(ALLOWED_ASSET_PREFIX)
        && !url.contains("..")
        && !url.chars().any(char::is_whitespace)
}

fn sanitize_installer_name(name: &str) -> Option<String> {
    let trimmed = name.trim();
    if trimmed.is_empty()
        || trimmed.contains("..")
        || trimmed.contains(['/', '\\', ':', '*', '?', '"', '<', '>', '|', '\0'])
    {
        return None;
    }
    let lower = trimmed.to_ascii_lowercase();
    let valid_ext = lower.ends_with(".msi")
        || lower.ends_with(".exe")
        || lower.ends_with(".dmg")
        || lower.ends_with(".deb")
        || lower.ends_with(".appimage");
    valid_ext.then(|| trimmed.to_string())
}

fn asset_priority_for_os(name: &str, target_os: &str) -> Option<u8> {
    let lower = name.to_ascii_lowercase();
    if lower.ends_with(".sig") || lower.ends_with(".zip") || lower.ends_with(".tar.gz") {
        return None;
    }
    match target_os {
        "windows" => {
            if lower.ends_with(".msi") {
                Some(2)
            } else if lower.ends_with(".exe") {
                Some(1)
            } else {
                None
            }
        }
        "macos" => {
            if lower.ends_with(".dmg") {
                Some(2)
            } else {
                None
            }
        }
        "linux" => {
            if lower.ends_with(".appimage") {
                Some(2)
            } else if lower.ends_with(".deb") {
                Some(1)
            } else {
                None
            }
        }
        _ => None,
    }
}

/// 解析 GitHub Release JSON，回傳版本與最適合目前作業系統的安裝檔資訊
pub fn parse_github_release(
    body: &str,
    current_ver: &str,
    target_os: &str,
) -> Result<AppUpdateInfo, String> {
    let value: serde_json::Value =
        serde_json::from_str(body).map_err(|_| "無法解析 GitHub Release 回應".to_string())?;
    let tag_name = value
        .get("tag_name")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| "GitHub Release 缺少版本標籤".to_string())?;

    let latest_version = tag_name.trim_start_matches(['v', 'V']).to_string();
    let has_update = is_newer_version(current_ver, &latest_version);
    let release_name = value
        .get("name")
        .and_then(|v| v.as_str())
        .filter(|s| !s.trim().is_empty())
        .unwrap_or(tag_name)
        .to_string();
    let release_notes = value
        .get("body")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let release_url = value
        .get("html_url")
        .and_then(|v| v.as_str())
        .filter(|u| u.starts_with(ALLOWED_RELEASE_PAGE_PREFIX))
        .unwrap_or("https://github.com/JontCont/synology-drive-client/releases")
        .to_string();

    let mut best_asset: Option<(u8, String, String, Option<u64>)> = None;
    if let Some(assets) = value.get("assets").and_then(|v| v.as_array()) {
        for asset in assets {
            let Some(raw_name) = asset.get("name").and_then(|v| v.as_str()) else {
                continue;
            };
            let Some(safe_name) = sanitize_installer_name(raw_name) else {
                continue;
            };
            let Some(priority) = asset_priority_for_os(&safe_name, target_os) else {
                continue;
            };
            let Some(url) = asset
                .get("browser_download_url")
                .and_then(|v| v.as_str())
                .filter(|u| is_allowed_asset_url(u))
            else {
                continue;
            };
            let size = asset.get("size").and_then(|v| v.as_u64());
            if best_asset.as_ref().is_none_or(|(p, _, _, _)| priority > *p) {
                best_asset = Some((priority, safe_name, url.to_string(), size));
            }
        }
    }

    let (asset_name, asset_url, asset_size) = match best_asset {
        Some((_, name, url, size)) => (Some(name), Some(url), size),
        None => (None, None, None),
    };

    Ok(AppUpdateInfo {
        current_version: current_ver.to_string(),
        latest_version,
        has_update,
        release_name,
        release_notes,
        release_url,
        asset_name,
        asset_url,
        asset_size,
    })
}

pub fn check_for_update() -> Result<AppUpdateInfo, String> {
    let cur = current_version();
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(10)))
        .http_status_as_error(false)
        .build()
        .into();

    let mut response = agent
        .get(GITHUB_LATEST_RELEASE_API)
        .header("Accept", "application/vnd.github+json")
        .header("User-Agent", USER_AGENT)
        .call()
        .map_err(|e| format!("無法連線至 GitHub 檢查更新：{e}"))?;

    let status = response.status().as_u16();
    if status == 404 {
        // 尚無任何正式發佈的 GitHub Release
        return Ok(AppUpdateInfo {
            current_version: cur.to_string(),
            latest_version: cur.to_string(),
            has_update: false,
            release_name: format!("v{cur}"),
            release_notes: String::new(),
            release_url: "https://github.com/JontCont/synology-drive-client/releases".to_string(),
            asset_name: None,
            asset_url: None,
            asset_size: None,
        });
    }
    if status != 200 {
        return Err(format!("GitHub 版本檢查回應異常（HTTP {status}）"));
    }

    let body = response
        .body_mut()
        .read_to_string()
        .map_err(|e| format!("讀取更新資訊失敗：{e}"))?;
    parse_github_release(&body, cur, std::env::consts::OS)
}

/// 確保下載到本機的安裝檔檔名一定帶有版本號（例如 `Synology.Drive.Client_0.1.3_x64-setup.exe`）
fn versioned_installer_name(asset_name: &str, version: &str) -> String {
    let clean_ver = version.trim().trim_start_matches(['v', 'V']);
    if clean_ver.is_empty() || asset_name.contains(clean_ver) {
        return asset_name.to_string();
    }
    match asset_name.rfind('.') {
        Some(dot) if dot > 0 => format!("{}_{clean_ver}{}", &asset_name[..dot], &asset_name[dot..]),
        _ => format!("{asset_name}_{clean_ver}"),
    }
}

/// 重新向 GitHub 查詢最新 Release，下載對應安裝檔至系統暫存目錄並啟動安裝程式
pub fn download_and_install_update() -> Result<String, String> {
    let info = check_for_update()?;
    if !info.has_update {
        return Err("目前已經是最新版本".to_string());
    }
    let raw_asset_name = info
        .asset_name
        .and_then(|n| sanitize_installer_name(&n))
        .ok_or_else(|| "此版本尚未提供目前作業系統的安裝檔".to_string())?;
    let asset_name = versioned_installer_name(&raw_asset_name, &info.latest_version);
    let asset_url = info
        .asset_url
        .filter(|u| is_allowed_asset_url(u))
        .ok_or_else(|| "找不到有效的官方安裝檔下載網址".to_string())?;

    let dest_dir = std::env::temp_dir().join("synology-drive-client-update");
    std::fs::create_dir_all(&dest_dir).map_err(|e| format!("無法建立更新暫存目錄：{e}"))?;
    let dest_path = dest_dir.join(&asset_name);
    let temp_path = dest_dir.join(format!("{asset_name}.part"));
    let _ = std::fs::remove_file(&temp_path);

    let agent: ureq::Agent = ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(300)))
        .build()
        .into();
    let mut response = agent
        .get(&asset_url)
        .header("User-Agent", USER_AGENT)
        .call()
        .map_err(|e| format!("下載更新安裝檔失敗：{e}"))?;

    let mut reader = response.body_mut().as_reader();
    let mut file =
        std::fs::File::create(&temp_path).map_err(|e| format!("建立暫存安裝檔失敗：{e}"))?;
    let bytes = std::io::copy(&mut reader, &mut file).map_err(|e| {
        let _ = std::fs::remove_file(&temp_path);
        format!("寫入更新安裝檔失敗：{e}")
    })?;
    let _ = file.flush();
    drop(file);

    if let Some(expected) = info.asset_size {
        if bytes != expected {
            let _ = std::fs::remove_file(&temp_path);
            return Err(format!("更新安裝檔下載不完整（{bytes} / {expected} bytes）"));
        }
    }
    let _ = std::fs::remove_file(&dest_path);
    std::fs::rename(&temp_path, &dest_path).map_err(|e| {
        let _ = std::fs::remove_file(&temp_path);
        format!("無法完成安裝檔準備：{e}")
    })?;

    launch_installer(&dest_path)?;
    Ok(dest_path.to_string_lossy().to_string())
}

fn launch_installer(path: &PathBuf) -> Result<(), String> {
    let lower = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();

    #[cfg(target_os = "windows")]
    {
        if lower.ends_with(".msi") {
            std::process::Command::new("msiexec")
                .arg("/i")
                .arg(path)
                .spawn()
                .map_err(|e| format!("無法啟動 MSI 安裝程式：{e}"))?;
        } else {
            std::process::Command::new(path)
                .spawn()
                .map_err(|e| format!("無法啟動安裝程式：{e}"))?;
        }
        return Ok(());
    }

    #[cfg(target_os = "macos")]
    {
        let _ = lower;
        std::process::Command::new("open")
            .arg(path)
            .spawn()
            .map_err(|e| format!("無法開啟安裝檔：{e}"))?;
        return Ok(());
    }

    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    {
        let _ = lower;
        std::process::Command::new("xdg-open")
            .arg(path)
            .spawn()
            .map_err(|e| format!("無法開啟安裝檔：{e}"))?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compares_semantic_versions_correctly() {
        assert!(is_newer_version("0.1.0", "0.1.1"));
        assert!(is_newer_version("0.1.0", "v0.2.0"));
        assert!(is_newer_version("v0.1.9", "v1.0.0-rc1"));
        assert!(!is_newer_version("0.1.0", "0.1.0"));
        assert!(!is_newer_version("0.2.0", "0.1.9"));
        assert!(!is_newer_version("0.1.0", "invalid"));
    }

    #[test]
    fn parses_release_and_picks_windows_msi_from_trusted_origin() {
        let json = r#"{
            "tag_name": "v0.2.0",
            "name": "Synology Drive Client v0.2.0",
            "body": "Bug fixes and Files On-Demand",
            "html_url": "https://github.com/JontCont/synology-drive-client/releases/tag/v0.2.0",
            "assets": [
                {
                    "name": "Synology-Drive-Client_0.2.0_x64_en-US.msi",
                    "browser_download_url": "https://github.com/JontCont/synology-drive-client/releases/download/v0.2.0/Synology-Drive-Client_0.2.0_x64_en-US.msi",
                    "size": 4194304
                },
                {
                    "name": "evil.exe",
                    "browser_download_url": "https://evil.example.com/evil.exe",
                    "size": 1024
                }
            ]
        }"#;
        let info = parse_github_release(json, "0.1.0", "windows").unwrap();
        assert!(info.has_update);
        assert_eq!(info.latest_version, "0.2.0");
        assert_eq!(
            info.asset_name.as_deref(),
            Some("Synology-Drive-Client_0.2.0_x64_en-US.msi")
        );
        assert_eq!(info.asset_size, Some(4194304));
    }

    #[test]
    fn rejects_untrusted_asset_origins_and_path_traversal_names() {
        assert!(!is_allowed_asset_url("https://evil.example.com/a.msi"));
        assert!(!is_allowed_asset_url(
            "http://github.com/JontCont/synology-drive-client/releases/download/v1/a.msi"
        ));
        assert!(sanitize_installer_name("../a.msi").is_none());
        assert!(sanitize_installer_name("script.bat").is_none());
        assert_eq!(
            sanitize_installer_name("SynologyDrive_0.2.0.msi").as_deref(),
            Some("SynologyDrive_0.2.0.msi")
        );
        assert_eq!(
            versioned_installer_name("Synology.Drive.Client_0.1.3_x64-setup.exe", "0.1.3"),
            "Synology.Drive.Client_0.1.3_x64-setup.exe"
        );
        assert_eq!(
            versioned_installer_name("Synology.Drive.Client_x64-setup.exe", "v0.1.3"),
            "Synology.Drive.Client_x64-setup_0.1.3.exe"
        );
    }
}
