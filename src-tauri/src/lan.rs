//! 區網直連：人在家裡的區網時，改用 NAS 的區網位址傳輸。
//! 區網位址不經過 Cloudflare 之類的代理，沒有單次請求 100 MB 的上限，速度也快得多。
//! 登入仍走一般的 NAS 位址；只有登入後的檔案操作會切到區網位址，失敗時由同步流程退回一般位址。

use std::net::Ipv4Addr;

use serde::Serialize;
use tauri::{AppHandle, Manager};

use crate::{auth, config};

const LAN_PROBE_TIMEOUT_SECS: u64 = 2;

/// 區網連線同樣要求 HTTPS 與有效憑證，只接受私有網路位址。
pub fn normalize_lan_origin(host: &str) -> Result<String, String> {
    let trimmed = host.trim();
    let origin = auth::normalize_dsm_origin(trimmed)
        .map_err(|_| "區網位址必須使用有效的 https:// 網址與受信任憑證，例如 https://nas.local:5001".to_string())?;
    let authority = origin.trim_start_matches("https://");
    if authority.is_empty() || authority.contains('@') || authority.contains(' ') || authority.contains('[') {
        return Err("區網位址格式不正確".to_string());
    }
    let host_only = match authority.rsplit_once(':') {
        Some((host, port)) if port.chars().all(|c| c.is_ascii_digit()) && !port.is_empty() => host,
        Some(_) => return Err("區網位址的連接埠格式不正確".to_string()),
        None => authority,
    };
    if !is_private_host(host_only) {
        return Err(
            "區網位址必須是私有網路位址（192.168.x.x、10.x.x.x、172.16-31.x.x）或 .local 主機名稱".to_string(),
        );
    }
    Ok(origin)
}

fn is_private_host(host: &str) -> bool {
    if let Ok(ip) = host.parse::<Ipv4Addr>() {
        return ip.is_private() || ip.is_loopback() || ip.is_link_local();
    }
    let lower = host.to_ascii_lowercase();
    lower == "localhost"
        || lower.ends_with(".local")
        || lower.ends_with(".lan")
        || lower.ends_with(".home.arpa")
}

/// 從 `netsh wlan show interfaces` 的輸出取出目前連線的 Wi-Fi 名稱（SSID）
fn parse_netsh_ssids(output: &str) -> Vec<String> {
    output
        .lines()
        .filter_map(|line| {
            let (key, value) = line.split_once(':')?;
            (key.trim() == "SSID").then(|| value.trim().to_string())
        })
        .filter(|ssid| !ssid.is_empty())
        .collect()
}

#[cfg(windows)]
pub fn current_wifi_ssids() -> Vec<String> {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    // chcp 65001 讓 netsh 以 UTF-8 輸出，中文 Wi-Fi 名稱才不會變成亂碼
    let output = std::process::Command::new("cmd")
        .args(["/C", "chcp 65001>nul & netsh wlan show interfaces"])
        .creation_flags(CREATE_NO_WINDOW)
        .output();
    match output {
        Ok(out) => parse_netsh_ssids(&String::from_utf8_lossy(&out.stdout)),
        Err(_) => Vec::new(),
    }
}

#[cfg(not(windows))]
pub fn current_wifi_ssids() -> Vec<String> {
    Vec::new()
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LanStatus {
    pub configured: bool,
    /// 目前是否正在使用區網位址傳輸
    pub active: bool,
    /// 目前連線的 Wi-Fi 名稱（沒有 Wi-Fi 或讀不到時為空）
    pub ssid: Option<String>,
    pub message: String,
}

/// 依設定、目前的 Wi-Fi 與區網位址是否連得到，決定現在要不要使用區網位址，並套用到 NAS 連線
pub fn refresh(app: &AppHandle) -> LanStatus {
    let auth = app.state::<auth::AuthState>();
    let status = |configured: bool, active: bool, ssid: Option<String>, message: &str| LanStatus {
        configured,
        active,
        ssid,
        message: message.to_string(),
    };
    let Ok(cfg) = config::load(app) else {
        auth.set_lan_origin(None);
        return status(false, false, None, "讀取設定失敗");
    };
    let Some(host) = cfg.lan_host.as_deref().map(str::trim).filter(|h| !h.is_empty()) else {
        auth.set_lan_origin(None);
        return status(false, false, None, "未設定區網位址");
    };
    let origin = match normalize_lan_origin(host) {
        Ok(origin) => origin,
        Err(message) => {
            auth.set_lan_origin(None);
            return LanStatus { configured: true, active: false, ssid: None, message };
        }
    };

    let ssids = current_wifi_ssids();
    let ssid = ssids.first().cloned();
    if !cfg.lan_wifi_names.is_empty() && !ssids.iter().any(|s| cfg.lan_wifi_names.contains(s)) {
        auth.set_lan_origin(None);
        return status(true, false, ssid, "目前不在指定的 Wi-Fi，使用一般 NAS 位址");
    }
    let probe = auth::probe_origin(&origin, LAN_PROBE_TIMEOUT_SECS);
    if probe.ok {
        auth.set_lan_origin(Some(origin));
        status(true, true, ssid, "正在使用區網位址傳輸")
    } else {
        auth.set_lan_origin(None);
        status(true, false, ssid, "連不到區網位址，使用一般 NAS 位址")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lan_origin_accepts_private_hosts_only() {
        assert_eq!(
            normalize_lan_origin(" https://192.168.1.100:5001/drive/ ").as_deref(),
            Ok("https://192.168.1.100:5001")
        );
        assert_eq!(normalize_lan_origin("https://10.0.0.5").as_deref(), Ok("https://10.0.0.5"));
        assert_eq!(normalize_lan_origin("https://172.20.1.2:5001").as_deref(), Ok("https://172.20.1.2:5001"));
        assert_eq!(normalize_lan_origin("https://nas.local:5001").as_deref(), Ok("https://nas.local:5001"));
        assert!(normalize_lan_origin("https://8.8.8.8:5001").is_err());
        assert!(normalize_lan_origin("https://172.32.0.1").is_err());
        assert!(normalize_lan_origin("https://nas.example.com").is_err());
        assert!(normalize_lan_origin("https://nas:5001").is_err());
        assert!(normalize_lan_origin("192.168.1.100:5001").is_err());
        assert!(normalize_lan_origin("https://user@192.168.1.100").is_err());
        assert!(normalize_lan_origin("https://192.168.1.100:abc").is_err());
        for host in ["http://192.168.1.100:5000", "http://nas.local:5000", "http://nas:5000"] {
            assert!(normalize_lan_origin(host).is_err());
        }
    }

    #[test]
    fn netsh_output_yields_ssid_but_not_bssid() {
        let output = "    Name                   : Wi-Fi\r\n    State                  : connected\r\n    SSID                   : 我家 WiFi\r\n    BSSID                  : aa:bb:cc:dd:ee:ff\r\n    Signal                 : 90%\r\n";
        assert_eq!(parse_netsh_ssids(output), vec!["我家 WiFi".to_string()]);
        assert!(parse_netsh_ssids("There is 1 interface\r\n    State : disconnected\r\n").is_empty());
    }
}
