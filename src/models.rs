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
