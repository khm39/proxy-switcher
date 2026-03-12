use crate::models::Proxy;
use std::process::Command;

/// Result of a system proxy operation.
#[derive(Debug, Clone)]
pub struct ProxyResult {
    pub success: bool,
    pub message: String,
}

impl ProxyResult {
    fn ok(msg: impl Into<String>) -> Self {
        Self {
            success: true,
            message: msg.into(),
        }
    }
    fn err(msg: impl Into<String>) -> Self {
        Self {
            success: false,
            message: msg.into(),
        }
    }
}

/// Apply the given proxy to the system settings.
pub fn apply_proxy(proxy: &Proxy) -> ProxyResult {
    if proxy.host.is_empty() {
        return ProxyResult::err("Proxy host is empty");
    }

    // Try multiple backends; use the first that succeeds
    let results = [
        apply_gnome(proxy),
        apply_kde(proxy),
        apply_env_file(proxy),
    ];

    // Return the first success, or all errors combined
    for r in &results {
        if r.success {
            return r.clone();
        }
    }

    let errors: Vec<&str> = results.iter().map(|r| r.message.as_str()).collect();
    ProxyResult::err(format!("All backends failed: {}", errors.join("; ")))
}

/// Remove system proxy settings (revert to direct connection).
pub fn clear_proxy() -> ProxyResult {
    let results = [
        clear_gnome(),
        clear_kde(),
        clear_env_file(),
    ];

    for r in &results {
        if r.success {
            return r.clone();
        }
    }

    let errors: Vec<&str> = results.iter().map(|r| r.message.as_str()).collect();
    ProxyResult::err(format!("All backends failed: {}", errors.join("; ")))
}

/// Read current system proxy state. Returns Some(host:port) if a proxy is set.
pub fn read_current() -> Option<String> {
    // Try GNOME first
    if let Some(val) = read_gnome() {
        return Some(val);
    }
    // Try KDE
    if let Some(val) = read_kde() {
        return Some(val);
    }
    // Try environment file
    if let Some(val) = read_env_file() {
        return Some(val);
    }
    None
}

// ---------------------------------------------------------------------------
// GNOME / gsettings backend
// ---------------------------------------------------------------------------

fn gsettings_available() -> bool {
    Command::new("gsettings")
        .arg("--version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

fn apply_gnome(proxy: &Proxy) -> ProxyResult {
    if !gsettings_available() {
        return ProxyResult::err("gsettings not available");
    }

    let schema = match proxy.proxy_type {
        crate::models::ProxyType::Http => "org.gnome.system.proxy.http",
        crate::models::ProxyType::Https => "org.gnome.system.proxy.https",
        crate::models::ProxyType::Socks4 | crate::models::ProxyType::Socks5 => {
            "org.gnome.system.proxy.socks"
        }
    };

    // Set mode to 'manual'
    if !run_gsettings(&["set", "org.gnome.system.proxy", "mode", "manual"]) {
        return ProxyResult::err("Failed to set gsettings proxy mode");
    }

    // Set host
    if !run_gsettings(&["set", schema, "host", &proxy.host]) {
        return ProxyResult::err(format!("Failed to set {schema} host"));
    }

    // Set port
    if !run_gsettings(&["set", schema, "port", &proxy.port.to_string()]) {
        return ProxyResult::err(format!("Failed to set {schema} port"));
    }

    // Set authentication if provided
    if !proxy.username.is_empty() {
        let auth_schema = match proxy.proxy_type {
            crate::models::ProxyType::Http => Some("org.gnome.system.proxy.http"),
            _ => None,
        };
        if let Some(auth_s) = auth_schema {
            run_gsettings(&["set", auth_s, "use-authentication", "true"]);
            run_gsettings(&["set", auth_s, "authentication-user", &proxy.username]);
            run_gsettings(&["set", auth_s, "authentication-password", &proxy.password]);
        }
    }

    ProxyResult::ok(format!(
        "GNOME proxy set: {}://{}:{}",
        proxy.proxy_type.scheme(),
        proxy.host,
        proxy.port
    ))
}

fn clear_gnome() -> ProxyResult {
    if !gsettings_available() {
        return ProxyResult::err("gsettings not available");
    }
    if run_gsettings(&["set", "org.gnome.system.proxy", "mode", "none"]) {
        ProxyResult::ok("GNOME proxy cleared")
    } else {
        ProxyResult::err("Failed to clear GNOME proxy")
    }
}

fn read_gnome() -> Option<String> {
    if !gsettings_available() {
        return None;
    }
    let mode = Command::new("gsettings")
        .args(["get", "org.gnome.system.proxy", "mode"])
        .output()
        .ok()?;
    let mode_str = String::from_utf8_lossy(&mode.stdout).trim().to_string();
    if mode_str != "'manual'" {
        return None;
    }

    // Read HTTP proxy as the primary indicator
    let host = Command::new("gsettings")
        .args(["get", "org.gnome.system.proxy.http", "host"])
        .output()
        .ok()?;
    let port = Command::new("gsettings")
        .args(["get", "org.gnome.system.proxy.http", "port"])
        .output()
        .ok()?;

    let h = String::from_utf8_lossy(&host.stdout)
        .trim()
        .trim_matches('\'')
        .to_string();
    let p = String::from_utf8_lossy(&port.stdout).trim().to_string();

    if h.is_empty() {
        return None;
    }
    Some(format!("{h}:{p}"))
}

fn run_gsettings(args: &[&str]) -> bool {
    Command::new("gsettings")
        .args(args)
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

// ---------------------------------------------------------------------------
// KDE / kwriteconfig5 backend
// ---------------------------------------------------------------------------

fn kwriteconfig_available() -> bool {
    Command::new("kwriteconfig5")
        .arg("--help")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

fn apply_kde(proxy: &Proxy) -> ProxyResult {
    if !kwriteconfig_available() {
        return ProxyResult::err("kwriteconfig5 not available");
    }

    let proxy_url = if proxy.username.is_empty() {
        format!("{}:{}", proxy.host, proxy.port)
    } else {
        format!(
            "{}:{}@{}:{}",
            proxy.username, proxy.password, proxy.host, proxy.port
        )
    };

    let proxy_type_num = match proxy.proxy_type {
        crate::models::ProxyType::Http | crate::models::ProxyType::Https => "1",
        crate::models::ProxyType::Socks4 | crate::models::ProxyType::Socks5 => "2",
    };

    let args_type = [
        "--file", "kioslaverc",
        "--group", "Proxy Settings",
        "--key", "ProxyType",
        proxy_type_num,
    ];
    let args_http = [
        "--file", "kioslaverc",
        "--group", "Proxy Settings",
        "--key", "httpProxy",
        &proxy_url,
    ];
    let args_https = [
        "--file", "kioslaverc",
        "--group", "Proxy Settings",
        "--key", "httpsProxy",
        &proxy_url,
    ];

    let ok = run_kwriteconfig(&args_type)
        && run_kwriteconfig(&args_http)
        && run_kwriteconfig(&args_https);

    if ok {
        ProxyResult::ok(format!("KDE proxy set: {proxy_url}"))
    } else {
        ProxyResult::err("Failed to write KDE proxy settings")
    }
}

fn clear_kde() -> ProxyResult {
    if !kwriteconfig_available() {
        return ProxyResult::err("kwriteconfig5 not available");
    }
    let args = [
        "--file", "kioslaverc",
        "--group", "Proxy Settings",
        "--key", "ProxyType",
        "0",
    ];
    if run_kwriteconfig(&args) {
        ProxyResult::ok("KDE proxy cleared")
    } else {
        ProxyResult::err("Failed to clear KDE proxy")
    }
}

fn read_kde() -> Option<String> {
    let output = Command::new("kreadconfig5")
        .args([
            "--file", "kioslaverc",
            "--group", "Proxy Settings",
            "--key", "httpProxy",
        ])
        .output()
        .ok()?;
    let val = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if val.is_empty() {
        None
    } else {
        // Check if proxy is actually enabled (ProxyType != 0)
        let ptype = Command::new("kreadconfig5")
            .args([
                "--file", "kioslaverc",
                "--group", "Proxy Settings",
                "--key", "ProxyType",
            ])
            .output()
            .ok()?;
        let pt = String::from_utf8_lossy(&ptype.stdout).trim().to_string();
        if pt == "0" {
            None
        } else {
            Some(val)
        }
    }
}

fn run_kwriteconfig(args: &[&str]) -> bool {
    Command::new("kwriteconfig5")
        .args(args)
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

// ---------------------------------------------------------------------------
// Environment file backend (~/.config/proxy-manager/proxy.env)
//
// Writes a sourceable shell file that sets http_proxy, https_proxy, etc.
// Users can add `source ~/.config/proxy-manager/proxy.env` to their shell rc.
// This also serves as a fallback when no DE-specific tool is available.
// ---------------------------------------------------------------------------

fn env_file_path() -> Option<std::path::PathBuf> {
    dirs::config_dir().map(|d| d.join("proxy-manager").join("proxy.env"))
}

fn apply_env_file(proxy: &Proxy) -> ProxyResult {
    let Some(path) = env_file_path() else {
        return ProxyResult::err("Cannot determine config directory");
    };

    let url = proxy.url();
    let no_proxy = if proxy.port_filter.enabled && !proxy.port_filter.ports.is_empty() {
        // When port filter is active, we note it as a comment but env vars
        // don't support per-port filtering natively
        "localhost,127.0.0.1,::1"
    } else {
        "localhost,127.0.0.1,::1"
    };

    let content = format!(
        r#"# Generated by Proxy Manager - do not edit manually
# Source this file in your shell: source {path}
export http_proxy="{url}"
export HTTP_PROXY="{url}"
export https_proxy="{url}"
export HTTPS_PROXY="{url}"
export all_proxy="{url}"
export ALL_PROXY="{url}"
export no_proxy="{no_proxy}"
export NO_PROXY="{no_proxy}"
"#,
        path = path.display(),
    );

    if let Some(parent) = path.parent() {
        if let Err(e) = std::fs::create_dir_all(parent) {
            return ProxyResult::err(format!("Cannot create dir: {e}"));
        }
    }

    match std::fs::write(&path, &content) {
        Ok(()) => ProxyResult::ok(format!(
            "Environment proxy file written: {}",
            path.display()
        )),
        Err(e) => ProxyResult::err(format!("Failed to write env file: {e}")),
    }
}

fn clear_env_file() -> ProxyResult {
    let Some(path) = env_file_path() else {
        return ProxyResult::err("Cannot determine config directory");
    };

    let content = r#"# Generated by Proxy Manager - proxy disabled
unset http_proxy HTTP_PROXY https_proxy HTTPS_PROXY all_proxy ALL_PROXY no_proxy NO_PROXY
"#;

    match std::fs::write(&path, content) {
        Ok(()) => ProxyResult::ok("Environment proxy file cleared"),
        Err(e) => ProxyResult::err(format!("Failed to write env file: {e}")),
    }
}

fn read_env_file() -> Option<String> {
    let path = env_file_path()?;
    let content = std::fs::read_to_string(&path).ok()?;
    // Parse out http_proxy value
    for line in content.lines() {
        if let Some(rest) = line.strip_prefix("export http_proxy=\"") {
            if let Some(url) = rest.strip_suffix('"') {
                if !url.is_empty() {
                    return Some(url.to_string());
                }
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::{Proxy, ProxyType};

    #[test]
    fn proxy_result_ok() {
        let r = ProxyResult::ok("test");
        assert!(r.success);
        assert_eq!(r.message, "test");
    }

    #[test]
    fn proxy_result_err() {
        let r = ProxyResult::err("fail");
        assert!(!r.success);
        assert_eq!(r.message, "fail");
    }

    #[test]
    fn apply_rejects_empty_host() {
        let proxy = Proxy::default(); // host is empty
        let result = apply_proxy(&proxy);
        assert!(!result.success);
        assert!(result.message.contains("empty"));
    }

    #[test]
    fn env_file_content_format() {
        let mut proxy = Proxy::default();
        proxy.host = "proxy.test.local".to_string();
        proxy.port = 8080;
        proxy.proxy_type = ProxyType::Http;

        // We can't easily test file writing in unit tests without side effects,
        // but we can verify the URL generation used by the env file backend
        assert_eq!(proxy.url(), "http://proxy.test.local:8080");
    }
}
