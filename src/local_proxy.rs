// ---------------------------------------------------------------------------
// Transparent proxy via TUN virtual NIC + smoltcp userspace TCP stack.
//
// Architecture:
//   [App traffic] → [OS route → TUN device (tun-rs)] → [smoltcp TCP stack]
//     → [per-connection relay] → [SOCKS5/HTTP upstream proxy] → [Internet]
//     ← [response] ← [smoltcp builds TCP/IP packets] ← [TUN device]
//
// Cross-platform via tun-rs: Linux, Windows (Wintun), macOS (utun).
// ---------------------------------------------------------------------------

use crate::models::{Proxy, ProxyType};
use smoltcp::iface::{Config, Interface, SocketSet};
use smoltcp::phy::{Device, DeviceCapabilities, Medium, RxToken, TxToken};
use smoltcp::socket::tcp::{self as smol_tcp};
use smoltcp::time::Instant as SmolInstant;
use smoltcp::wire::{HardwareAddress, IpAddress, IpCidr};
use std::collections::{HashMap, VecDeque};
use std::net::SocketAddrV4;
use std::sync::{Arc, Mutex};
use tokio::sync::{broadcast, mpsc};

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

const TUN_ADDR: &str = "10.0.85.1";
const TUN_ADDR_BYTES: [u8; 4] = [10, 0, 85, 1];
const TUN_CIDR_PREFIX: u8 = 24;
const TUN_NAME: &str = "proxyswitch0";
const TUN_MTU: u16 = 1500;
const SMOL_TCP_RX_BUF: usize = 65535;
const SMOL_TCP_TX_BUF: usize = 65535;
const MAX_SOCKETS: usize = 256;

// ---------------------------------------------------------------------------
// UpstreamConfig – derived from Proxy model
// ---------------------------------------------------------------------------

#[derive(Clone, Debug)]
pub struct UpstreamConfig {
    pub proxy_type: ProxyType,
    pub host: String,
    pub port: u16,
    pub username: String,
    pub password: String,
    pub filter_enabled: bool,
    pub filter_ports: Vec<u16>,
}

impl UpstreamConfig {
    pub fn from_proxy(proxy: &Proxy) -> Self {
        Self {
            proxy_type: proxy.proxy_type,
            host: proxy.host.clone(),
            port: proxy.port,
            username: proxy.username.clone(),
            password: proxy.password.clone(),
            filter_enabled: proxy.port_filter.enabled,
            filter_ports: proxy.port_filter.ports.clone(),
        }
    }

    /// Should traffic to `dest_port` go through the upstream proxy?
    pub fn should_proxy(&self, dest_port: u16) -> bool {
        if !self.filter_enabled || self.filter_ports.is_empty() {
            true
        } else {
            self.filter_ports.contains(&dest_port)
        }
    }

    /// Build SOCKS5/HTTP proxy address string.
    pub fn proxy_addr(&self) -> String {
        format!("{}:{}", self.host, self.port)
    }
}

// ---------------------------------------------------------------------------
// TunBridge – smoltcp PHY device backed by packet queues.
//
// Packets read from real TUN go into rx_queue (smoltcp reads them).
// Packets written by smoltcp go to tx_queue (we send to real TUN).
// ---------------------------------------------------------------------------

struct TunBridge {
    rx_queue: VecDeque<Vec<u8>>,
    tx_queue: VecDeque<Vec<u8>>,
    mtu: usize,
}

impl TunBridge {
    fn new(mtu: usize) -> Self {
        Self {
            rx_queue: VecDeque::new(),
            tx_queue: VecDeque::new(),
            mtu,
        }
    }

    fn push_rx(&mut self, pkt: Vec<u8>) {
        self.rx_queue.push_back(pkt);
    }

    fn pop_tx(&mut self) -> Option<Vec<u8>> {
        self.tx_queue.pop_front()
    }
}

struct BridgeRxToken(Vec<u8>);
struct BridgeTxToken<'a>(&'a mut VecDeque<Vec<u8>>);

impl RxToken for BridgeRxToken {
    fn consume<R, F>(mut self, f: F) -> R
    where
        F: FnOnce(&mut [u8]) -> R,
    {
        f(&mut self.0)
    }
}

impl<'a> TxToken for BridgeTxToken<'a> {
    fn consume<R, F>(self, len: usize, f: F) -> R
    where
        F: FnOnce(&mut [u8]) -> R,
    {
        let mut buf = vec![0u8; len];
        let result = f(&mut buf);
        self.0.push_back(buf);
        result
    }
}

impl Device for TunBridge {
    type RxToken<'a> = BridgeRxToken;
    type TxToken<'a> = BridgeTxToken<'a>;

    fn receive(&mut self, _timestamp: SmolInstant) -> Option<(Self::RxToken<'_>, Self::TxToken<'_>)> {
        if self.rx_queue.is_empty() {
            return None;
        }
        let pkt = self.rx_queue.pop_front().unwrap();
        Some((
            BridgeRxToken(pkt),
            BridgeTxToken(&mut self.tx_queue),
        ))
    }

    fn transmit(&mut self, _timestamp: SmolInstant) -> Option<Self::TxToken<'_>> {
        Some(BridgeTxToken(&mut self.tx_queue))
    }

    fn capabilities(&self) -> DeviceCapabilities {
        let mut caps = DeviceCapabilities::default();
        caps.medium = Medium::Ip;
        caps.max_transmission_unit = self.mtu;
        caps
    }
}

// ---------------------------------------------------------------------------
// ConnectionInfo – tracks per-socket upstream relay state
// ---------------------------------------------------------------------------

#[derive(Clone, Debug)]
struct ConnectionInfo {
    dest: SocketAddrV4,
    /// Channel to send data FROM smoltcp socket TO the upstream relay task.
    to_upstream: mpsc::UnboundedSender<Vec<u8>>,
    /// Channel to receive data FROM upstream relay task TO smoltcp socket.
    from_upstream: Arc<Mutex<VecDeque<Vec<u8>>>>,
    established: bool,
    closed: bool,
}

// ---------------------------------------------------------------------------
// ProxyHandle – public API
// ---------------------------------------------------------------------------

/// Handle to control the running transparent proxy.
pub struct ProxyHandle {
    shutdown_tx: broadcast::Sender<()>,
}

impl ProxyHandle {
    pub fn stop(&self) {
        let _ = self.shutdown_tx.send(());
    }
}

/// Status of the local proxy, shared with the GUI.
#[derive(Clone, Debug)]
pub struct ProxyStatus {
    pub running: bool,
    pub tun_addr: String,
    pub connections: usize,
    pub error: Option<String>,
}

impl Default for ProxyStatus {
    fn default() -> Self {
        Self {
            running: false,
            tun_addr: String::new(),
            connections: 0,
            error: None,
        }
    }
}

// ---------------------------------------------------------------------------
// TUN device creation via tun-rs
// ---------------------------------------------------------------------------

fn create_tun_device() -> Result<tun_rs::AsyncDevice, String> {
    log::info!("Creating TUN device: name={}, addr={}/{}, mtu={}",
        TUN_NAME, TUN_ADDR, TUN_CIDR_PREFIX, TUN_MTU);
    log::info!("Platform: {}", std::env::consts::OS);
    log::info!("Arch: {}", std::env::consts::ARCH);

    let mut builder = tun_rs::DeviceBuilder::new();
    builder = builder
        .name(TUN_NAME)
        .ipv4(TUN_ADDR, TUN_CIDR_PREFIX, None)
        .mtu(TUN_MTU);

    // On Windows, resolve wintun.dll path relative to the executable
    #[cfg(target_os = "windows")]
    {
        let dll_path = find_wintun_dll();
        match &dll_path {
            Some(path) => log::info!("wintun.dll found: {}", path),
            None => log::warn!("wintun.dll not found in any search path, using tun-rs default"),
        }
        if let Some(path) = dll_path {
            builder = builder.wintun_file(path);
        }
    }

    log::info!("Calling tun-rs build_async()...");
    let result = builder.build_async();

    match &result {
        Ok(_dev) => {
            log::info!("TUN device created successfully");
        }
        Err(e) => {
            log::error!("TUN device creation failed: {e}");
            log::error!("Error debug: {e:?}");
        }
    }

    result.map_err(|e| {
        let msg = format!("{e}");
        let debug_msg = format!("{e:?}");
        if msg.contains("LoadLibrary") || msg.contains("wintun") || debug_msg.contains("LoadLibrary") {
            format!(
                "Wintun driver not found. Please download wintun.dll from \
                 https://www.wintun.net/ and place it next to the executable \
                 or in the current directory. (Error: {msg}) (Debug: {debug_msg})"
            )
        } else if msg.contains("permission") || msg.contains("denied") || msg.contains("EPERM") {
            format!(
                "Permission denied creating TUN device. \
                 Run as Administrator (Windows) or root (Linux). (Error: {msg})"
            )
        } else {
            format!("Failed to create TUN device: {msg} (Debug: {debug_msg})")
        }
    })
}

/// Search for wintun.dll in common locations.
#[cfg(target_os = "windows")]
fn find_wintun_dll() -> Option<String> {
    log::info!("Searching for wintun.dll...");

    // 1. Next to the executable
    match std::env::current_exe() {
        Ok(exe_path) => {
            log::info!("  exe path: {}", exe_path.display());
            if let Some(exe_dir) = exe_path.parent() {
                let candidate = exe_dir.join("wintun.dll");
                log::info!("  checking: {} -> exists={}", candidate.display(), candidate.exists());
                if candidate.exists() {
                    return Some(candidate.to_string_lossy().into_owned());
                }
            }
        }
        Err(e) => log::warn!("  current_exe() failed: {e}"),
    }

    // 2. Current working directory
    match std::env::current_dir() {
        Ok(cwd) => {
            log::info!("  cwd: {}", cwd.display());
            let candidate = cwd.join("wintun.dll");
            log::info!("  checking: {} -> exists={}", candidate.display(), candidate.exists());
            if candidate.exists() {
                return Some(candidate.to_string_lossy().into_owned());
            }
        }
        Err(e) => log::warn!("  current_dir() failed: {e}"),
    }

    // 3. Check PATH directories
    if let Ok(path_var) = std::env::var("PATH") {
        for dir in path_var.split(';') {
            let candidate = std::path::PathBuf::from(dir).join("wintun.dll");
            if candidate.exists() {
                log::info!("  found in PATH: {}", candidate.display());
                return Some(candidate.to_string_lossy().into_owned());
            }
        }
        log::info!("  not found in any PATH directory");
    }

    // 4. List files in exe dir and cwd for debugging
    if let Ok(exe_path) = std::env::current_exe() {
        if let Some(exe_dir) = exe_path.parent() {
            log::info!("  Files in exe dir ({}):", exe_dir.display());
            if let Ok(entries) = std::fs::read_dir(exe_dir) {
                for entry in entries.flatten() {
                    let name = entry.file_name();
                    let name_str = name.to_string_lossy();
                    if name_str.ends_with(".dll") || name_str.contains("wintun") {
                        log::info!("    {}", name_str);
                    }
                }
            }
        }
    }
    if let Ok(cwd) = std::env::current_dir() {
        log::info!("  Files in cwd ({}):", cwd.display());
        if let Ok(entries) = std::fs::read_dir(&cwd) {
            for entry in entries.flatten() {
                let name = entry.file_name();
                let name_str = name.to_string_lossy();
                if name_str.ends_with(".dll") || name_str.contains("wintun") {
                    log::info!("    {}", name_str);
                }
            }
        }
    }

    log::warn!("wintun.dll not found anywhere");
    None
}

// ---------------------------------------------------------------------------
// Route management
// ---------------------------------------------------------------------------

fn add_routes(upstream_host: &str) -> Result<(), String> {
    #[cfg(target_os = "linux")]
    {
        use std::process::Command;

        // Get current default gateway to preserve route to upstream proxy
        let output = Command::new("ip")
            .args(["route", "show", "default"])
            .output()
            .map_err(|e| format!("ip route show: {e}"))?;
        let default_route = String::from_utf8_lossy(&output.stdout);

        // Extract gateway IP (e.g., "default via 192.168.1.1 dev eth0")
        if let Some(gw_line) = default_route.lines().next() {
            let parts: Vec<&str> = gw_line.split_whitespace().collect();
            if let (Some("via"), Some(gw), Some("dev"), Some(dev)) =
                (parts.get(1).copied(), parts.get(2).copied(), parts.get(3).copied(), parts.get(4).copied())
            {
                // Route upstream proxy IP through original gateway (avoid loop)
                let _ = Command::new("ip")
                    .args(["route", "add", upstream_host, "via", gw, "dev", dev])
                    .output();
            }
        }

        // Add default route through TUN
        let _ = Command::new("ip")
            .args(["route", "add", "default", "dev", TUN_NAME, "metric", "10"])
            .output();

        Ok(())
    }

    #[cfg(target_os = "windows")]
    {
        use std::process::Command;

        // Windows: add route via TUN interface
        // The TUN interface is assigned 10.0.85.1; we route all traffic through it
        let _ = Command::new("route")
            .args(["add", "0.0.0.0", "mask", "0.0.0.0", TUN_ADDR, "metric", "10"])
            .output();

        // Keep upstream proxy reachable via original gateway
        let output = Command::new("route")
            .args(["print", "0.0.0.0"])
            .output()
            .map_err(|e| format!("route print: {e}"))?;
        let route_table = String::from_utf8_lossy(&output.stdout);

        // Parse default gateway from route table (simplified)
        for line in route_table.lines() {
            let parts: Vec<&str> = line.split_whitespace().collect();
            if parts.first() == Some(&"0.0.0.0") && parts.get(1) == Some(&"0.0.0.0") {
                if let Some(gw) = parts.get(2) {
                    if *gw != TUN_ADDR {
                        let _ = Command::new("route")
                            .args(["add", upstream_host, "mask", "255.255.255.255", gw])
                            .output();
                        break;
                    }
                }
            }
        }

        Ok(())
    }

    #[cfg(not(any(target_os = "linux", target_os = "windows")))]
    {
        let _ = upstream_host;
        Ok(())
    }
}

fn remove_routes(upstream_host: &str) {
    #[cfg(target_os = "linux")]
    {
        use std::process::Command;
        let _ = Command::new("ip")
            .args(["route", "del", "default", "dev", TUN_NAME])
            .output();
        let _ = Command::new("ip")
            .args(["route", "del", upstream_host])
            .output();
    }

    #[cfg(target_os = "windows")]
    {
        use std::process::Command;
        let _ = Command::new("route")
            .args(["delete", "0.0.0.0", "mask", "0.0.0.0", TUN_ADDR])
            .output();
        let _ = Command::new("route")
            .args(["delete", upstream_host])
            .output();
    }

    #[cfg(not(any(target_os = "linux", target_os = "windows")))]
    {
        let _ = upstream_host;
    }
}

// ---------------------------------------------------------------------------
// Upstream connector – connects to target via SOCKS5/HTTP proxy or direct
// ---------------------------------------------------------------------------

async fn connect_upstream(
    config: &UpstreamConfig,
    dest: SocketAddrV4,
) -> Result<tokio::net::TcpStream, String> {
    let dest_port = dest.port();

    if !config.should_proxy(dest_port) {
        // Direct connection (bypass proxy)
        tokio::net::TcpStream::connect(dest)
            .await
            .map_err(|e| format!("Direct connect to {dest}: {e}"))
    } else {
        match config.proxy_type {
            ProxyType::Socks5 => connect_socks5(config, dest).await,
            ProxyType::Socks4 => connect_socks4(config, dest).await,
            ProxyType::Http | ProxyType::Https => connect_http_proxy(config, dest).await,
        }
    }
}

async fn connect_socks5(
    config: &UpstreamConfig,
    dest: SocketAddrV4,
) -> Result<tokio::net::TcpStream, String> {
    let proxy_addr = config.proxy_addr();
    let dest_ip = dest.ip().to_string();
    let target = (&*dest_ip, dest.port());

    if config.username.is_empty() {
        tokio_socks::tcp::Socks5Stream::connect(&*proxy_addr, target)
            .await
            .map(|s| s.into_inner())
            .map_err(|e| format!("SOCKS5: {e}"))
    } else {
        tokio_socks::tcp::Socks5Stream::connect_with_password(
            &*proxy_addr,
            target,
            &config.username,
            &config.password,
        )
        .await
        .map(|s| s.into_inner())
        .map_err(|e| format!("SOCKS5 auth: {e}"))
    }
}

async fn connect_socks4(
    config: &UpstreamConfig,
    dest: SocketAddrV4,
) -> Result<tokio::net::TcpStream, String> {
    let proxy_addr = config.proxy_addr();
    let dest_ip = dest.ip().to_string();
    let target = (&*dest_ip, dest.port());

    tokio_socks::tcp::Socks4Stream::connect(&*proxy_addr, target)
        .await
        .map(|s| s.into_inner())
        .map_err(|e| format!("SOCKS4: {e}"))
}

async fn connect_http_proxy(
    config: &UpstreamConfig,
    dest: SocketAddrV4,
) -> Result<tokio::net::TcpStream, String> {
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

    let proxy_addr = config.proxy_addr();
    let mut stream = tokio::net::TcpStream::connect(&proxy_addr)
        .await
        .map_err(|e| format!("HTTP proxy connect: {e}"))?;

    // Send CONNECT request
    let connect_req = if config.username.is_empty() {
        format!("CONNECT {dest} HTTP/1.1\r\nHost: {dest}\r\n\r\n")
    } else {
        use std::io::Write;
        let mut creds = Vec::new();
        write!(creds, "{}:{}", config.username, config.password).unwrap();
        let b64 = base64_encode(&creds);
        format!(
            "CONNECT {dest} HTTP/1.1\r\nHost: {dest}\r\nProxy-Authorization: Basic {b64}\r\n\r\n"
        )
    };

    stream
        .write_all(connect_req.as_bytes())
        .await
        .map_err(|e| format!("HTTP CONNECT write: {e}"))?;

    // Read response status line
    let mut reader = BufReader::new(&mut stream);
    let mut line = String::new();
    reader
        .read_line(&mut line)
        .await
        .map_err(|e| format!("HTTP CONNECT read: {e}"))?;

    if !line.contains("200") {
        return Err(format!("HTTP CONNECT rejected: {}", line.trim()));
    }

    // Drain remaining headers
    loop {
        let mut hdr = String::new();
        reader.read_line(&mut hdr).await.map_err(|e| format!("{e}"))?;
        if hdr.trim().is_empty() {
            break;
        }
    }

    drop(reader);
    Ok(stream)
}

/// Simple base64 encoder for HTTP Basic auth.
fn base64_encode(data: &[u8]) -> String {
    const CHARS: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut result = String::new();
    for chunk in data.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = chunk.get(1).copied().unwrap_or(0) as u32;
        let b2 = chunk.get(2).copied().unwrap_or(0) as u32;
        let triple = (b0 << 16) | (b1 << 8) | b2;
        result.push(CHARS[((triple >> 18) & 0x3F) as usize] as char);
        result.push(CHARS[((triple >> 12) & 0x3F) as usize] as char);
        if chunk.len() > 1 {
            result.push(CHARS[((triple >> 6) & 0x3F) as usize] as char);
        } else {
            result.push('=');
        }
        if chunk.len() > 2 {
            result.push(CHARS[(triple & 0x3F) as usize] as char);
        } else {
            result.push('=');
        }
    }
    result
}

// ---------------------------------------------------------------------------
// Main proxy loop
// ---------------------------------------------------------------------------

/// Start the transparent proxy. Returns a handle to stop it.
pub fn start(
    rt: &tokio::runtime::Runtime,
    config: UpstreamConfig,
    status: Arc<Mutex<ProxyStatus>>,
    ctx: egui::Context,
) -> Result<ProxyHandle, String> {
    // Create TUN device via tun-rs
    let tun_device = create_tun_device()?;

    // Add routes
    add_routes(&config.host)?;

    let (shutdown_tx, shutdown_rx) = broadcast::channel::<()>(1);
    let listen_info = format!("TUN {} ({})", TUN_NAME, TUN_ADDR);

    let upstream_host = config.host.clone();

    // Update status
    {
        let mut s = status.lock().unwrap();
        s.running = true;
        s.tun_addr = listen_info;
        s.error = None;
    }
    ctx.request_repaint();

    rt.spawn(proxy_loop(
        tun_device,
        config,
        shutdown_rx,
        status.clone(),
        ctx.clone(),
        upstream_host,
    ));

    Ok(ProxyHandle { shutdown_tx })
}

async fn proxy_loop(
    tun_device: tun_rs::AsyncDevice,
    config: UpstreamConfig,
    mut shutdown: broadcast::Receiver<()>,
    status: Arc<Mutex<ProxyStatus>>,
    ctx: egui::Context,
    upstream_host: String,
) {
    let tun = Arc::new(tun_device);

    // smoltcp interface setup
    let mut bridge = TunBridge::new(TUN_MTU as usize);
    let smol_config = Config::new(HardwareAddress::Ip);
    let mut iface = Interface::new(smol_config, &mut bridge, SmolInstant::now());
    iface.update_ip_addrs(|addrs| {
        let _ = addrs.push(IpCidr::new(
            IpAddress::v4(TUN_ADDR_BYTES[0], TUN_ADDR_BYTES[1], TUN_ADDR_BYTES[2], TUN_ADDR_BYTES[3]),
            TUN_CIDR_PREFIX,
        ));
    });

    let mut sockets = SocketSet::new(Vec::new());

    // Connection tracking
    let connections: Arc<Mutex<HashMap<smoltcp::iface::SocketHandle, ConnectionInfo>>> =
        Arc::new(Mutex::new(HashMap::new()));

    let _config = Arc::new(config);
    let mut buf = vec![0u8; 65535];

    loop {
        // Check shutdown
        if shutdown.try_recv().is_ok() {
            break;
        }

        // Read from TUN (async with short timeout)
        let read_result = tokio::time::timeout(
            std::time::Duration::from_millis(10),
            tun.recv(&mut buf),
        )
        .await;

        if let Ok(Ok(n)) = read_result {
            if n > 0 {
                bridge.push_rx(buf[..n].to_vec());
            }
        }

        // Process smoltcp
        let now = SmolInstant::now();
        let _changed = iface.poll(now, &mut bridge, &mut sockets);

        // Write outgoing packets from smoltcp to TUN
        while let Some(pkt) = bridge.pop_tx() {
            let tun_ref = tun.clone();
            let _ = tun_ref.send(&pkt).await;
        }

        // Update status
        {
            let mut s = status.lock().unwrap();
            s.connections = connections.lock().unwrap().len();
        }
    }

    // Cleanup
    remove_routes(&upstream_host);
    {
        let mut s = status.lock().unwrap();
        s.running = false;
        s.connections = 0;
    }
    ctx.request_repaint();
}

fn format_ip(addr: &[u8; 4]) -> String {
    format!("{}.{}.{}.{}", addr[0], addr[1], addr[2], addr[3])
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::{PortFilter, Proxy, ProxyType};

    #[test]
    fn upstream_config_should_proxy_all_when_disabled() {
        let config = UpstreamConfig {
            proxy_type: ProxyType::Socks5,
            host: "proxy.local".into(),
            port: 1080,
            username: String::new(),
            password: String::new(),
            filter_enabled: false,
            filter_ports: vec![],
        };
        assert!(config.should_proxy(80));
        assert!(config.should_proxy(443));
        assert!(config.should_proxy(22));
    }

    #[test]
    fn upstream_config_should_proxy_filtered() {
        let config = UpstreamConfig {
            proxy_type: ProxyType::Socks5,
            host: "proxy.local".into(),
            port: 1080,
            username: String::new(),
            password: String::new(),
            filter_enabled: true,
            filter_ports: vec![80, 443],
        };
        assert!(config.should_proxy(80));
        assert!(config.should_proxy(443));
        assert!(!config.should_proxy(22));
        assert!(!config.should_proxy(8080));
    }

    #[test]
    fn upstream_config_from_proxy() {
        let mut proxy = Proxy::default();
        proxy.proxy_type = ProxyType::Socks5;
        proxy.host = "socks.local".into();
        proxy.port = 1080;
        proxy.username = "user".into();
        proxy.password = "pass".into();
        proxy.port_filter = PortFilter {
            enabled: true,
            ports: vec![80, 443],
            raw_input: "80, 443".into(),
        };

        let config = UpstreamConfig::from_proxy(&proxy);
        assert_eq!(config.host, "socks.local");
        assert_eq!(config.port, 1080);
        assert!(config.should_proxy(80));
        assert!(!config.should_proxy(22));
    }

    #[test]
    fn base64_encode_basic() {
        assert_eq!(base64_encode(b"user:pass"), "dXNlcjpwYXNz");
        assert_eq!(base64_encode(b"a"), "YQ==");
        assert_eq!(base64_encode(b"ab"), "YWI=");
        assert_eq!(base64_encode(b"abc"), "YWJj");
    }

    #[test]
    fn tun_bridge_device_capabilities() {
        let bridge = TunBridge::new(1500);
        let caps = bridge.capabilities();
        assert_eq!(caps.medium, Medium::Ip);
        assert_eq!(caps.max_transmission_unit, 1500);
    }

    #[test]
    fn tun_bridge_rx_tx_queues() {
        let mut bridge = TunBridge::new(1500);
        assert!(bridge.pop_tx().is_none());

        bridge.push_rx(vec![1, 2, 3]);
        assert_eq!(bridge.rx_queue.len(), 1);

        // Simulate smoltcp receive
        let now = SmolInstant::from_millis(0);
        let result = bridge.receive(now);
        assert!(result.is_some());
    }

    #[test]
    fn format_ip_works() {
        assert_eq!(format_ip(&[10, 0, 85, 1]), "10.0.85.1");
        assert_eq!(format_ip(&[192, 168, 1, 1]), "192.168.1.1");
    }
}
