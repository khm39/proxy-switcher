use serde::{Deserialize, Serialize};
use std::fmt;

// ---------------------------------------------------------------------------
// ProxyType
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ProxyType {
    Http,
    Https,
    Socks4,
    Socks5,
}

impl ProxyType {
    pub const ALL: [ProxyType; 4] = [
        ProxyType::Http,
        ProxyType::Https,
        ProxyType::Socks4,
        ProxyType::Socks5,
    ];

    pub fn default_port(self) -> u16 {
        match self {
            ProxyType::Http => 8080,
            ProxyType::Https => 443,
            ProxyType::Socks4 | ProxyType::Socks5 => 1080,
        }
    }

    pub fn scheme(self) -> &'static str {
        match self {
            ProxyType::Http => "http",
            ProxyType::Https => "https",
            ProxyType::Socks4 => "socks4",
            ProxyType::Socks5 => "socks5",
        }
    }
}

impl fmt::Display for ProxyType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ProxyType::Http => write!(f, "HTTP"),
            ProxyType::Https => write!(f, "HTTPS"),
            ProxyType::Socks4 => write!(f, "SOCKS4"),
            ProxyType::Socks5 => write!(f, "SOCKS5"),
        }
    }
}

// ---------------------------------------------------------------------------
// TestStatus
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub enum TestStatus {
    Idle,
    Testing,
    Success(u64),
    Failed(String),
}

impl Default for TestStatus {
    fn default() -> Self {
        TestStatus::Idle
    }
}

// ---------------------------------------------------------------------------
// PortFilter
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PortFilter {
    pub enabled: bool,
    pub ports: Vec<u16>,
    pub raw_input: String,
}

impl Default for PortFilter {
    fn default() -> Self {
        Self {
            enabled: false,
            ports: Vec::new(),
            raw_input: String::new(),
        }
    }
}

impl PortFilter {
    /// Parse `raw_input` (comma-separated port numbers) and update `ports`.
    pub fn parse_raw_input(&mut self) {
        self.ports = self
            .raw_input
            .split(',')
            .filter_map(|s| s.trim().parse::<u16>().ok())
            .filter(|&p| p >= 1)
            .collect();
    }

    pub fn toggle_port(&mut self, port: u16) {
        if let Some(pos) = self.ports.iter().position(|&p| p == port) {
            self.ports.remove(pos);
        } else {
            self.ports.push(port);
        }
        self.ports.sort();
        self.raw_input = self
            .ports
            .iter()
            .map(|p| p.to_string())
            .collect::<Vec<_>>()
            .join(", ");
    }
}

// ---------------------------------------------------------------------------
// Proxy
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Proxy {
    pub id: String,
    pub name: String,
    pub proxy_type: ProxyType,
    pub host: String,
    pub port: u16,
    pub username: String,
    pub password: String,
    pub port_filter: PortFilter,
    pub note: String,
    #[serde(skip)]
    pub test_status: TestStatus,
}

impl Default for Proxy {
    fn default() -> Self {
        Self {
            id: uuid::Uuid::new_v4().to_string(),
            name: "New Proxy".to_string(),
            proxy_type: ProxyType::Http,
            host: String::new(),
            port: ProxyType::Http.default_port(),
            username: String::new(),
            password: String::new(),
            port_filter: PortFilter::default(),
            note: String::new(),
            test_status: TestStatus::default(),
        }
    }
}

impl Proxy {
    /// Build proxy URL: `scheme://[user:pass@]host:port`
    pub fn url(&self) -> String {
        let scheme = self.proxy_type.scheme();
        if self.username.is_empty() {
            format!("{scheme}://{}:{}", self.host, self.port)
        } else {
            format!(
                "{scheme}://{}:{}@{}:{}",
                self.username, self.password, self.host, self.port
            )
        }
    }
}

// ---------------------------------------------------------------------------
// AppData  (persistence root)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppData {
    pub proxies: Vec<Proxy>,
    pub active_proxy_id: Option<String>,
    #[serde(default = "default_tun_addr")]
    pub tun_addr: String,
}

fn default_tun_addr() -> String {
    "172.29.0.1/24".to_string()
}

impl Default for AppData {
    fn default() -> Self {
        Self {
            proxies: Vec::new(),
            active_proxy_id: None,
            tun_addr: default_tun_addr(),
        }
    }
}

impl AppData {
    pub fn active_proxy(&self) -> Option<&Proxy> {
        self.active_proxy_id
            .as_ref()
            .and_then(|id| self.proxies.iter().find(|p| &p.id == id))
    }
}

// ---------------------------------------------------------------------------
// Legacy format migration
// ---------------------------------------------------------------------------

/// Old config format that used profiles.
#[derive(Deserialize)]
struct LegacyAppData {
    profiles: Vec<LegacyProfile>,
    #[serde(default)]
    active_profile_id: Option<String>,
    #[serde(default = "default_tun_addr")]
    tun_addr: String,
}

#[derive(Deserialize)]
struct LegacyProfile {
    #[serde(default)]
    proxies: Vec<Proxy>,
    #[serde(default)]
    active_proxy_id: Option<String>,
}

/// Try to parse as new flat format first; fall back to legacy profile format.
pub fn parse_config(json: &str) -> Result<AppData, String> {
    // Try new format
    if let Ok(data) = serde_json::from_str::<AppData>(json) {
        return Ok(data);
    }
    // Try legacy profile format
    if let Ok(legacy) = serde_json::from_str::<LegacyAppData>(json) {
        let mut proxies = Vec::new();
        let mut active_proxy_id = None;
        // Find active profile and merge all proxies
        for profile in &legacy.profiles {
            let is_active = legacy.active_profile_id.as_ref()
                .map_or(false, |_aid| legacy.profiles.iter()
                    .position(|p| std::ptr::eq(p, profile))
                    .map_or(false, |_| true));
            proxies.extend(profile.proxies.iter().cloned());
            if is_active || active_proxy_id.is_none() {
                if let Some(ref id) = profile.active_proxy_id {
                    active_proxy_id = Some(id.clone());
                }
            }
        }
        return Ok(AppData {
            proxies,
            active_proxy_id,
            tun_addr: legacy.tun_addr,
        });
    }
    Err("Failed to parse config".to_string())
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn proxy_type_default_ports() {
        assert_eq!(ProxyType::Http.default_port(), 8080);
        assert_eq!(ProxyType::Https.default_port(), 443);
        assert_eq!(ProxyType::Socks4.default_port(), 1080);
        assert_eq!(ProxyType::Socks5.default_port(), 1080);
    }

    #[test]
    fn proxy_type_schemes() {
        assert_eq!(ProxyType::Http.scheme(), "http");
        assert_eq!(ProxyType::Https.scheme(), "https");
        assert_eq!(ProxyType::Socks4.scheme(), "socks4");
        assert_eq!(ProxyType::Socks5.scheme(), "socks5");
    }

    #[test]
    fn proxy_type_display() {
        assert_eq!(ProxyType::Http.to_string(), "HTTP");
        assert_eq!(ProxyType::Socks5.to_string(), "SOCKS5");
    }

    #[test]
    fn port_filter_parse_raw_input() {
        let mut pf = PortFilter {
            enabled: true,
            ports: vec![],
            raw_input: "80, 443, 8080".to_string(),
        };
        pf.parse_raw_input();
        assert_eq!(pf.ports, vec![80, 443, 8080]);
    }

    #[test]
    fn port_filter_parse_ignores_invalid() {
        let mut pf = PortFilter {
            enabled: true,
            ports: vec![],
            raw_input: "80, abc, 99999, 443".to_string(),
        };
        pf.parse_raw_input();
        // 99999 exceeds u16 max (65535), so it's filtered out by parse failure
        assert_eq!(pf.ports, vec![80, 443]);
    }

    #[test]
    fn port_filter_toggle() {
        let mut pf = PortFilter::default();
        pf.toggle_port(443);
        assert_eq!(pf.ports, vec![443]);
        assert_eq!(pf.raw_input, "443");

        pf.toggle_port(80);
        assert_eq!(pf.ports, vec![80, 443]);
        assert_eq!(pf.raw_input, "80, 443");

        // Toggle off
        pf.toggle_port(443);
        assert_eq!(pf.ports, vec![80]);
        assert_eq!(pf.raw_input, "80");
    }

    #[test]
    fn proxy_url_without_auth() {
        let mut p = Proxy::default();
        p.host = "proxy.example.com".to_string();
        p.port = 8080;
        p.proxy_type = ProxyType::Http;
        assert_eq!(p.url(), "http://proxy.example.com:8080");
    }

    #[test]
    fn proxy_url_with_auth() {
        let mut p = Proxy::default();
        p.host = "proxy.example.com".to_string();
        p.port = 1080;
        p.proxy_type = ProxyType::Socks5;
        p.username = "user".to_string();
        p.password = "pass".to_string();
        assert_eq!(p.url(), "socks5://user:pass@proxy.example.com:1080");
    }

    #[test]
    fn app_data_default_is_empty() {
        let data = AppData::default();
        assert!(data.proxies.is_empty());
        assert!(data.active_proxy_id.is_none());
    }

    #[test]
    fn app_data_active_proxy() {
        let mut data = AppData::default();
        let proxy = Proxy::default();
        let pid = proxy.id.clone();
        data.proxies.push(proxy);
        assert!(data.active_proxy().is_none());

        data.active_proxy_id = Some(pid.clone());
        let active = data.active_proxy().unwrap();
        assert_eq!(active.id, pid);
    }

    #[test]
    fn proxy_serialization_roundtrip() {
        let mut proxy = Proxy::default();
        proxy.host = "test.local".to_string();
        proxy.port = 3128;
        proxy.test_status = TestStatus::Success(42);

        let json = serde_json::to_string(&proxy).unwrap();
        let restored: Proxy = serde_json::from_str(&json).unwrap();

        assert_eq!(restored.host, "test.local");
        assert_eq!(restored.port, 3128);
        // test_status is skipped, should be default (Idle)
        assert!(matches!(restored.test_status, TestStatus::Idle));
    }

    #[test]
    fn app_data_serialization_roundtrip() {
        let mut data = AppData::default();
        let mut proxy = Proxy::default();
        proxy.name = "Corp Proxy".to_string();
        proxy.host = "proxy.corp.local".to_string();
        proxy.port = 8080;
        proxy.port_filter = PortFilter {
            enabled: true,
            ports: vec![80, 443],
            raw_input: "80, 443".to_string(),
        };
        data.proxies.push(proxy);

        let json = serde_json::to_string_pretty(&data).unwrap();
        let restored: AppData = serde_json::from_str(&json).unwrap();

        assert_eq!(restored.proxies.len(), 1);
        assert_eq!(restored.proxies[0].name, "Corp Proxy");
        assert_eq!(restored.proxies[0].port_filter.ports, vec![80, 443]);
    }

    #[test]
    fn parse_config_legacy_format() {
        let legacy_json = r#"{
            "profiles": [{
                "id": "p1",
                "name": "Default",
                "proxies": [{"id":"x1","name":"Test","proxy_type":"Http","host":"h","port":80,"username":"","password":"","port_filter":{"enabled":false,"ports":[],"raw_input":""},"note":""}],
                "active_proxy_id": "x1"
            }],
            "active_profile_id": "p1",
            "tun_addr": "10.0.0.1/24"
        }"#;
        let data = parse_config(legacy_json).unwrap();
        assert_eq!(data.proxies.len(), 1);
        assert_eq!(data.proxies[0].name, "Test");
        assert_eq!(data.active_proxy_id, Some("x1".to_string()));
        assert_eq!(data.tun_addr, "10.0.0.1/24");
    }
}
