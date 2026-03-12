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
// Profile
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Profile {
    pub id: String,
    pub name: String,
    pub proxies: Vec<Proxy>,
    pub active_proxy_id: Option<String>,
}

impl Profile {
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            id: uuid::Uuid::new_v4().to_string(),
            name: name.into(),
            proxies: Vec::new(),
            active_proxy_id: None,
        }
    }

    pub fn active_proxy(&self) -> Option<&Proxy> {
        self.active_proxy_id
            .as_ref()
            .and_then(|id| self.proxies.iter().find(|p| &p.id == id))
    }
}

// ---------------------------------------------------------------------------
// AppData  (persistence root)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppData {
    pub profiles: Vec<Profile>,
    pub active_profile_id: Option<String>,
}

impl Default for AppData {
    fn default() -> Self {
        let default_profile = Profile::new("Default");
        let id = default_profile.id.clone();
        Self {
            profiles: vec![default_profile],
            active_profile_id: Some(id),
        }
    }
}

impl AppData {
    pub fn active_profile(&self) -> Option<&Profile> {
        self.active_profile_id
            .as_ref()
            .and_then(|id| self.profiles.iter().find(|p| &p.id == id))
    }

    pub fn active_profile_mut(&mut self) -> Option<&mut Profile> {
        self.active_profile_id
            .as_ref()
            .cloned()
            .and_then(move |id| self.profiles.iter_mut().find(|p| p.id == id))
    }
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
    fn profile_active_proxy() {
        let mut profile = Profile::new("Test");
        let proxy = Proxy::default();
        let pid = proxy.id.clone();
        profile.proxies.push(proxy);
        assert!(profile.active_proxy().is_none());

        profile.active_proxy_id = Some(pid.clone());
        let active = profile.active_proxy().unwrap();
        assert_eq!(active.id, pid);
    }

    #[test]
    fn app_data_default_has_one_profile() {
        let data = AppData::default();
        assert_eq!(data.profiles.len(), 1);
        assert!(data.active_profile_id.is_some());
        assert!(data.active_profile().is_some());
    }

    #[test]
    fn app_data_active_profile_mut() {
        let mut data = AppData::default();
        let name = {
            let p = data.active_profile_mut().unwrap();
            p.name = "Changed".to_string();
            p.name.clone()
        };
        assert_eq!(data.active_profile().unwrap().name, name);
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
        data.profiles[0].proxies.push(proxy);

        let json = serde_json::to_string_pretty(&data).unwrap();
        let restored: AppData = serde_json::from_str(&json).unwrap();

        assert_eq!(restored.profiles.len(), 1);
        assert_eq!(restored.profiles[0].proxies.len(), 1);
        assert_eq!(restored.profiles[0].proxies[0].name, "Corp Proxy");
        assert_eq!(restored.profiles[0].proxies[0].port_filter.ports, vec![80, 443]);
    }
}
