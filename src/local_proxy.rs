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
use smoltcp::iface::{Config, Interface, SocketHandle, SocketSet};
use smoltcp::phy::{Device, DeviceCapabilities, Medium, RxToken, TxToken};
use smoltcp::socket::tcp::{self as smol_tcp, State as TcpState};
use smoltcp::time::Instant as SmolInstant;
use smoltcp::wire::{HardwareAddress, IpAddress, IpCidr, Ipv4Address};
use std::collections::{HashMap, HashSet, VecDeque};
use std::net::{Ipv4Addr, SocketAddrV4};
use std::sync::{Arc, Mutex};
use tokio::sync::broadcast;

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

const TUN_ADDR: &str = "10.0.85.1";
const TUN_NAME: &str = "proxyswitch0";
const TUN_MTU: u16 = 1500;
const SMOL_TCP_BUF: usize = 65535;

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

    pub fn should_proxy(&self, dest_port: u16) -> bool {
        if !self.filter_enabled || self.filter_ports.is_empty() {
            true
        } else {
            self.filter_ports.contains(&dest_port)
        }
    }

    pub fn proxy_addr(&self) -> String {
        format!("{}:{}", self.host, self.port)
    }
}

// ---------------------------------------------------------------------------
// TunBridge – smoltcp PHY device backed by packet queues.
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
// Relay state – per-connection tracking
// ---------------------------------------------------------------------------

struct RelayState {
    dest: SocketAddrV4,
    /// Data arriving from the upstream proxy, waiting to be fed into smoltcp socket.
    from_upstream: Arc<Mutex<VecDeque<Vec<u8>>>>,
    /// Data from smoltcp socket, to be sent to the upstream proxy.
    to_upstream: tokio::sync::mpsc::UnboundedSender<Vec<u8>>,
    /// Whether the upstream connection has been initiated.
    upstream_started: bool,
    /// Whether the upstream reported connection closed.
    upstream_closed: Arc<Mutex<bool>>,
}

// ---------------------------------------------------------------------------
// ProxyHandle – public API
// ---------------------------------------------------------------------------

pub struct ProxyHandle {
    shutdown_tx: broadcast::Sender<()>,
}

impl ProxyHandle {
    pub fn stop(&self) {
        let _ = self.shutdown_tx.send(());
    }
}

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
    log::info!("Creating TUN device: name={}, addr={}/24, mtu={}",
        TUN_NAME, TUN_ADDR, TUN_MTU);
    log::info!("Platform: {}, Arch: {}", std::env::consts::OS, std::env::consts::ARCH);

    let mut builder = tun_rs::DeviceBuilder::new();
    builder = builder
        .name(TUN_NAME)
        .ipv4(TUN_ADDR, 24, None)
        .mtu(TUN_MTU);

    #[cfg(target_os = "windows")]
    {
        let dll_path = find_wintun_dll();
        match &dll_path {
            Some(path) => {
                log::info!("wintun.dll found: {}", path);
                if let Ok(meta) = std::fs::metadata(path) {
                    log::info!("wintun.dll size: {} bytes", meta.len());
                }
                log_dll_arch(path);
            }
            None => log::warn!("wintun.dll not found, using tun-rs default"),
        }
        if let Some(path) = dll_path {
            builder = builder.wintun_file(path);
        }
    }

    log::info!("Calling tun-rs build_async()...");
    let result = builder.build_async();

    match &result {
        Ok(_) => log::info!("TUN device created successfully"),
        Err(e) => {
            log::error!("TUN device creation failed: {e}");
            log::error!("Error debug: {e:?}");
        }
    }

    result.map_err(|e| {
        let msg = format!("{e}");
        let debug_msg = format!("{e:?}");

        if debug_msg.contains("193") && debug_msg.contains("LoadLibrary") {
            let arch = std::env::consts::ARCH;
            let need = match arch {
                "x86_64" => "amd64",
                "x86" => "x86",
                "aarch64" => "arm64",
                other => other,
            };
            format!(
                "wintun.dll architecture mismatch! This app is {arch}, \
                 use wintun/bin/{need}/wintun.dll from the Wintun ZIP. \
                 Download: https://www.wintun.net/"
            )
        } else if msg.contains("LoadLibrary") || debug_msg.contains("LoadLibrary") {
            format!(
                "Wintun driver not found. Download wintun.dll from \
                 https://www.wintun.net/ and place next to exe. ({msg})"
            )
        } else if msg.contains("permission") || msg.contains("denied") {
            format!("Permission denied. Run as Administrator/root. ({msg})")
        } else {
            format!("Failed to create TUN device: {msg} ({debug_msg})")
        }
    })
}

#[cfg(target_os = "windows")]
fn find_wintun_dll() -> Option<String> {
    log::info!("Searching for wintun.dll...");

    if let Ok(exe_path) = std::env::current_exe() {
        log::info!("  exe path: {}", exe_path.display());
        if let Some(exe_dir) = exe_path.parent() {
            let candidate = exe_dir.join("wintun.dll");
            log::info!("  checking: {} -> exists={}", candidate.display(), candidate.exists());
            if candidate.exists() {
                return Some(candidate.to_string_lossy().into_owned());
            }
        }
    }

    if let Ok(cwd) = std::env::current_dir() {
        let candidate = cwd.join("wintun.dll");
        log::info!("  checking: {} -> exists={}", candidate.display(), candidate.exists());
        if candidate.exists() {
            return Some(candidate.to_string_lossy().into_owned());
        }
    }

    if let Ok(path_var) = std::env::var("PATH") {
        for dir in path_var.split(';') {
            let candidate = std::path::PathBuf::from(dir).join("wintun.dll");
            if candidate.exists() {
                log::info!("  found in PATH: {}", candidate.display());
                return Some(candidate.to_string_lossy().into_owned());
            }
        }
    }

    log::warn!("wintun.dll not found anywhere");
    None
}

#[cfg(target_os = "windows")]
fn log_dll_arch(path: &str) {
    use std::io::{Read, Seek, SeekFrom};
    let Ok(mut f) = std::fs::File::open(path) else { return };
    let mut buf = [0u8; 4];
    if f.seek(SeekFrom::Start(0x3C)).is_err() || f.read_exact(&mut buf).is_err() { return }
    let pe_offset = u32::from_le_bytes(buf) as u64;
    if f.seek(SeekFrom::Start(pe_offset)).is_err() { return }
    let mut header = [0u8; 6];
    if f.read_exact(&mut header).is_err() { return }
    let machine = u16::from_le_bytes([header[4], header[5]]);
    let arch_str = match machine {
        0x014C => "x86 (32-bit)",
        0x8664 => "x86_64 (64-bit)",
        0xAA64 => "ARM64",
        _ => { log::info!("wintun.dll PE Machine: 0x{:04X}", machine); return }
    };
    log::info!("wintun.dll arch: {} (app: {})", arch_str, std::env::consts::ARCH);
}

// ---------------------------------------------------------------------------
// Route management
// ---------------------------------------------------------------------------

fn run_cmd(program: &str, args: &[&str]) -> Result<String, String> {
    use std::process::Command;
    log::info!("  exec: {} {}", program, args.join(" "));
    let output = Command::new(program)
        .args(args)
        .output()
        .map_err(|e| format!("{} failed to start: {e}", program))?;
    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
    if !stdout.trim().is_empty() {
        log::info!("  stdout: {}", stdout.trim());
    }
    if !stderr.trim().is_empty() {
        log::warn!("  stderr: {}", stderr.trim());
    }
    if !output.status.success() {
        log::warn!("  exit code: {:?}", output.status.code());
    }
    Ok(stdout)
}

fn add_routes(upstream_host: &str) -> Result<(), String> {
    log::info!("Adding routes (upstream_host={})", upstream_host);

    #[cfg(target_os = "linux")]
    {
        // Step 1: Find the original default gateway FIRST
        let output = run_cmd("ip", &["route", "show", "default"])?;
        let mut original_gw: Option<String> = None;
        let mut original_dev: Option<String> = None;
        if let Some(gw_line) = output.lines().next() {
            let parts: Vec<&str> = gw_line.split_whitespace().collect();
            if let (Some("via"), Some(gw), Some("dev"), Some(dev)) =
                (parts.get(1).copied(), parts.get(2).copied(), parts.get(3).copied(), parts.get(4).copied())
            {
                log::info!("Original gateway: {} via dev {}", gw, dev);
                original_gw = Some(gw.to_string());
                original_dev = Some(dev.to_string());
            }
        }

        // Step 2: Add specific route for upstream proxy host via original gateway
        // This MUST be done BEFORE adding the default TUN route to prevent routing loop
        if let (Some(gw), Some(dev)) = (&original_gw, &original_dev) {
            log::info!("Adding host route for upstream proxy {} via {} dev {}", upstream_host, gw, dev);
            let _ = run_cmd("ip", &["route", "add", upstream_host, "via", gw, "dev", dev]);
        } else {
            log::warn!("No original gateway found! Upstream proxy traffic may loop through TUN.");
        }

        // Step 3: Add default route through TUN with low metric
        log::info!("Adding default route via TUN device {}", TUN_NAME);
        let _ = run_cmd("ip", &["route", "add", "default", "dev", TUN_NAME, "metric", "1"]);

        // Step 4: Verify routing
        let _ = run_cmd("ip", &["route", "show"]);

        Ok(())
    }

    #[cfg(target_os = "windows")]
    {
        // Step 1: Read existing route table to find original default gateway FIRST
        log::info!("Reading existing route table...");
        let route_table = run_cmd("route", &["print", "0.0.0.0"])?;
        let mut original_gw: Option<String> = None;
        for line in route_table.lines() {
            let parts: Vec<&str> = line.split_whitespace().collect();
            // Windows route table: Network Destination  Netmask  Gateway  Interface  Metric
            if parts.first() == Some(&"0.0.0.0") && parts.get(1) == Some(&"0.0.0.0") {
                if let Some(gw) = parts.get(2) {
                    if *gw != TUN_ADDR && *gw != "On-link" {
                        log::info!("Found original gateway: {}", gw);
                        original_gw = Some(gw.to_string());
                        break;
                    }
                }
            }
        }

        // Step 2: Add specific route for upstream proxy host via original gateway
        // This MUST be done BEFORE the default TUN route to prevent routing loop
        if let Some(gw) = &original_gw {
            log::info!("Adding host route for upstream proxy {} via {}", upstream_host, gw);
            let _ = run_cmd("route", &["add", upstream_host, "mask", "255.255.255.255", gw, "metric", "1"]);
        } else {
            log::warn!("No original gateway found! Upstream proxy traffic may loop through TUN.");
        }

        // Step 3: Add default route via TUN with very low metric (1) to override existing routes
        // Use two /1 routes (0.0.0.0/1 and 128.0.0.0/1) instead of a single 0.0.0.0/0 route.
        // This is more reliable because it's more specific than any existing 0.0.0.0/0 default
        // route and will always win in the routing table regardless of metric.
        log::info!("Adding split default routes via TUN (0.0.0.0/1 + 128.0.0.0/1)");
        let _ = run_cmd("route", &["add", "0.0.0.0", "mask", "128.0.0.0", TUN_ADDR, "metric", "1"]);
        let _ = run_cmd("route", &["add", "128.0.0.0", "mask", "128.0.0.0", TUN_ADDR, "metric", "1"]);

        // Step 4: Verify routing
        log::info!("Verifying route table after changes:");
        let _ = run_cmd("route", &["print", "0.0.0.0"]);

        Ok(())
    }

    #[cfg(not(any(target_os = "linux", target_os = "windows")))]
    { let _ = upstream_host; Ok(()) }
}

fn remove_routes(upstream_host: &str) {
    log::info!("Removing routes");
    #[cfg(target_os = "linux")]
    {
        let _ = run_cmd("ip", &["route", "del", "default", "dev", TUN_NAME]);
        let _ = run_cmd("ip", &["route", "del", upstream_host]);
    }
    #[cfg(target_os = "windows")]
    {
        // Remove the split /1 routes
        let _ = run_cmd("route", &["delete", "0.0.0.0", "mask", "128.0.0.0", TUN_ADDR]);
        let _ = run_cmd("route", &["delete", "128.0.0.0", "mask", "128.0.0.0", TUN_ADDR]);
        // Remove upstream host route
        let _ = run_cmd("route", &["delete", upstream_host]);
    }
    #[cfg(not(any(target_os = "linux", target_os = "windows")))]
    { let _ = upstream_host; }
}

// ---------------------------------------------------------------------------
// Upstream connectors
// ---------------------------------------------------------------------------

async fn connect_upstream(
    config: &UpstreamConfig,
    dest: SocketAddrV4,
) -> Result<tokio::net::TcpStream, String> {
    if !config.should_proxy(dest.port()) {
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

async fn connect_socks5(config: &UpstreamConfig, dest: SocketAddrV4) -> Result<tokio::net::TcpStream, String> {
    let proxy_addr = config.proxy_addr();
    let dest_ip = dest.ip().to_string();
    let target = (&*dest_ip, dest.port());
    if config.username.is_empty() {
        tokio_socks::tcp::Socks5Stream::connect(&*proxy_addr, target)
            .await.map(|s| s.into_inner()).map_err(|e| format!("SOCKS5: {e}"))
    } else {
        tokio_socks::tcp::Socks5Stream::connect_with_password(
            &*proxy_addr, target, &config.username, &config.password,
        ).await.map(|s| s.into_inner()).map_err(|e| format!("SOCKS5 auth: {e}"))
    }
}

async fn connect_socks4(config: &UpstreamConfig, dest: SocketAddrV4) -> Result<tokio::net::TcpStream, String> {
    let proxy_addr = config.proxy_addr();
    let dest_ip = dest.ip().to_string();
    let target = (&*dest_ip, dest.port());
    tokio_socks::tcp::Socks4Stream::connect(&*proxy_addr, target)
        .await.map(|s| s.into_inner()).map_err(|e| format!("SOCKS4: {e}"))
}

async fn connect_http_proxy(config: &UpstreamConfig, dest: SocketAddrV4) -> Result<tokio::net::TcpStream, String> {
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
    let proxy_addr = config.proxy_addr();
    let mut stream = tokio::net::TcpStream::connect(&proxy_addr)
        .await.map_err(|e| format!("HTTP proxy connect: {e}"))?;
    let connect_req = if config.username.is_empty() {
        format!("CONNECT {dest} HTTP/1.1\r\nHost: {dest}\r\n\r\n")
    } else {
        use std::io::Write;
        let mut creds = Vec::new();
        write!(creds, "{}:{}", config.username, config.password).unwrap();
        let b64 = base64_encode(&creds);
        format!("CONNECT {dest} HTTP/1.1\r\nHost: {dest}\r\nProxy-Authorization: Basic {b64}\r\n\r\n")
    };
    stream.write_all(connect_req.as_bytes()).await.map_err(|e| format!("CONNECT write: {e}"))?;
    let mut reader = BufReader::new(&mut stream);
    let mut line = String::new();
    reader.read_line(&mut line).await.map_err(|e| format!("CONNECT read: {e}"))?;
    if !line.contains("200") {
        return Err(format!("CONNECT rejected: {}", line.trim()));
    }
    loop {
        let mut hdr = String::new();
        reader.read_line(&mut hdr).await.map_err(|e| format!("{e}"))?;
        if hdr.trim().is_empty() { break; }
    }
    drop(reader);
    Ok(stream)
}

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
        if chunk.len() > 1 { result.push(CHARS[((triple >> 6) & 0x3F) as usize] as char); }
        else { result.push('='); }
        if chunk.len() > 2 { result.push(CHARS[(triple & 0x3F) as usize] as char); }
        else { result.push('='); }
    }
    result
}

// ---------------------------------------------------------------------------
// Packet parsing helpers
// ---------------------------------------------------------------------------

/// Parse an IP packet and extract TCP src/dst + SYN flag.
fn parse_tcp_packet(pkt: &[u8]) -> Option<(SocketAddrV4, SocketAddrV4, bool)> {
    let parsed = etherparse::SlicedPacket::from_ip(pkt).ok()?;
    let (src_ip, dst_ip) = match &parsed.net {
        Some(etherparse::NetSlice::Ipv4(ipv4)) => {
            let hdr = ipv4.header();
            (hdr.source_addr(), hdr.destination_addr())
        }
        _ => return None,
    };
    match &parsed.transport {
        Some(etherparse::TransportSlice::Tcp(tcp)) => {
            let src = SocketAddrV4::new(src_ip, tcp.source_port());
            let dst = SocketAddrV4::new(dst_ip, tcp.destination_port());
            let is_syn = tcp.syn() && !tcp.ack();
            Some((src, dst, is_syn))
        }
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// Main proxy loop
// ---------------------------------------------------------------------------

pub fn start(
    rt: &tokio::runtime::Runtime,
    config: UpstreamConfig,
    status: Arc<Mutex<ProxyStatus>>,
    ctx: egui::Context,
) -> Result<ProxyHandle, String> {
    let tun_device = create_tun_device()?;
    add_routes(&config.host)?;

    let (shutdown_tx, shutdown_rx) = broadcast::channel::<()>(1);
    let listen_info = format!("TUN {} ({})", TUN_NAME, TUN_ADDR);
    let upstream_host = config.host.clone();

    {
        let mut s = status.lock().unwrap();
        s.running = true;
        s.tun_addr = listen_info;
        s.error = None;
    }
    ctx.request_repaint();

    rt.spawn(proxy_loop(tun_device, config, shutdown_rx, status, ctx, upstream_host));

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
    let config = Arc::new(config);

    // smoltcp interface: use 0.0.0.0/0 so it accepts packets to ANY destination IP.
    // This makes smoltcp "pretend" to be every server on the internet.
    let mut bridge = TunBridge::new(TUN_MTU as usize);
    let smol_config = Config::new(HardwareAddress::Ip);
    let mut iface = Interface::new(smol_config, &mut bridge, SmolInstant::now());
    iface.update_ip_addrs(|addrs| {
        let _ = addrs.push(IpCidr::new(IpAddress::v4(0, 0, 0, 0), 0));
    });
    // Default route so smoltcp sends responses
    iface
        .routes_mut()
        .add_default_ipv4_route(Ipv4Address::new(0, 0, 0, 1))
        .ok();

    let mut sockets = SocketSet::new(Vec::new());
    let mut relays: HashMap<SocketHandle, RelayState> = HashMap::new();
    // Track ports that currently have a LISTEN socket (to avoid duplicates)
    let mut listening_ports: HashSet<u16> = HashSet::new();
    let mut buf = vec![0u8; 65535];
    let mut pkt_count: u64 = 0;
    let mut tcp_pkt_count: u64 = 0;
    let mut syn_count: u64 = 0;
    let mut last_stats = std::time::Instant::now();

    log::info!("Proxy loop started, waiting for traffic...");

    loop {
        // Check shutdown
        if shutdown.try_recv().is_ok() {
            log::info!("Shutdown signal received");
            break;
        }

        // Periodic stats logging
        if last_stats.elapsed() >= std::time::Duration::from_secs(10) {
            log::info!(
                "Stats: pkts_from_tun={}, tcp_pkts={}, syns={}, active_conns={}",
                pkt_count, tcp_pkt_count, syn_count, relays.len()
            );
            last_stats = std::time::Instant::now();
        }

        // 1. Read packet from TUN (with short timeout to keep loop responsive)
        let read_result = tokio::time::timeout(
            std::time::Duration::from_millis(5),
            tun.recv(&mut buf),
        )
        .await;

        if let Ok(Ok(n)) = read_result {
            if n > 0 {
                pkt_count += 1;
                let pkt = &buf[..n];

                // Parse TCP packet to detect new connections (SYN)
                if let Some((_src, dst, is_syn)) = parse_tcp_packet(pkt) {
                    tcp_pkt_count += 1;
                    let dst_port = dst.port();

                    if is_syn {
                        syn_count += 1;
                        log::info!("SYN detected: -> {} (port {}), listening_ports={:?}", dst, dst_port, listening_ports);
                    }

                    if is_syn && !listening_ports.contains(&dst_port) {
                        // Create a new smoltcp TCP socket listening on this port.
                        // smoltcp will handle the TCP handshake (SYN-ACK).
                        let rx_buf = smol_tcp::SocketBuffer::new(vec![0u8; SMOL_TCP_BUF]);
                        let tx_buf = smol_tcp::SocketBuffer::new(vec![0u8; SMOL_TCP_BUF]);
                        let mut socket = smol_tcp::Socket::new(rx_buf, tx_buf);

                        if socket.listen(dst_port).is_ok() {
                            let handle = sockets.add(socket);
                            listening_ports.insert(dst_port);

                            // Prepare relay channels
                            let (to_up_tx, mut to_up_rx) =
                                tokio::sync::mpsc::unbounded_channel::<Vec<u8>>();
                            let from_upstream = Arc::new(Mutex::new(VecDeque::<Vec<u8>>::new()));
                            let upstream_closed = Arc::new(Mutex::new(false));

                            relays.insert(handle, RelayState {
                                dest: dst,
                                from_upstream: from_upstream.clone(),
                                to_upstream: to_up_tx,
                                upstream_started: false,
                                upstream_closed: upstream_closed.clone(),
                            });

                            log::info!("New TCP connection to {} (port {}), socket created", dst, dst_port);

                            // Spawn upstream relay task
                            let cfg = config.clone();
                            let from_up = from_upstream;
                            let up_closed = upstream_closed;
                            tokio::spawn(async move {
                                match connect_upstream(&cfg, dst).await {
                                    Ok(stream) => {
                                        log::info!("Upstream connected to {}", dst);
                                        let (mut reader, mut writer) = tokio::io::split(stream);

                                        // Bidirectional relay
                                        let from_up2 = from_up.clone();
                                        let up_closed2 = up_closed.clone();

                                        // upstream → smoltcp (read from upstream, push to from_upstream queue)
                                        let read_task = tokio::spawn(async move {
                                            use tokio::io::AsyncReadExt;
                                            let mut rbuf = vec![0u8; 8192];
                                            loop {
                                                match reader.read(&mut rbuf).await {
                                                    Ok(0) => {
                                                        log::debug!("Upstream read EOF for {}", dst);
                                                        *up_closed2.lock().unwrap() = true;
                                                        break;
                                                    }
                                                    Ok(n) => {
                                                        from_up2.lock().unwrap().push_back(rbuf[..n].to_vec());
                                                    }
                                                    Err(e) => {
                                                        log::debug!("Upstream read error for {}: {}", dst, e);
                                                        *up_closed2.lock().unwrap() = true;
                                                        break;
                                                    }
                                                }
                                            }
                                        });

                                        // smoltcp → upstream (read from channel, write to upstream)
                                        let write_task = tokio::spawn(async move {
                                            use tokio::io::AsyncWriteExt;
                                            while let Some(data) = to_up_rx.recv().await {
                                                if writer.write_all(&data).await.is_err() {
                                                    break;
                                                }
                                            }
                                        });

                                        let _ = tokio::join!(read_task, write_task);
                                    }
                                    Err(e) => {
                                        log::error!("Upstream connect failed for {}: {}", dst, e);
                                        *up_closed.lock().unwrap() = true;
                                    }
                                }
                            });
                        }
                    }
                }

                // Feed packet to smoltcp
                bridge.push_rx(pkt.to_vec());
            }
        }

        // 2. Poll smoltcp (processes TCP state machine)
        let now = SmolInstant::now();
        iface.poll(now, &mut bridge, &mut sockets);

        // 3. Process relay data for each connection
        let mut to_remove: Vec<SocketHandle> = Vec::new();

        for (&handle, relay) in relays.iter_mut() {
            let socket = sockets.get_mut::<smol_tcp::Socket>(handle);

            // Once socket transitions from LISTEN → active, free the port for new listeners
            if !relay.upstream_started && socket.state() != TcpState::Listen {
                relay.upstream_started = true;
                listening_ports.remove(&relay.dest.port());
            }

            // smoltcp socket → upstream: read data from smoltcp and send to upstream
            if socket.can_recv() {
                let mut data = vec![0u8; 8192];
                match socket.recv_slice(&mut data) {
                    Ok(n) if n > 0 => {
                        data.truncate(n);
                        let _ = relay.to_upstream.send(data);
                    }
                    _ => {}
                }
            }

            // upstream → smoltcp socket: write data from upstream into smoltcp
            if socket.can_send() {
                let mut queue = relay.from_upstream.lock().unwrap();
                while let Some(data) = queue.pop_front() {
                    match socket.send_slice(&data) {
                        Ok(sent) if sent < data.len() => {
                            // Partial send, push remainder back
                            queue.push_front(data[sent..].to_vec());
                            break;
                        }
                        Err(_) => {
                            queue.push_front(data);
                            break;
                        }
                        _ => {}
                    }
                }
            }

            // If upstream closed and all data drained, close smoltcp socket
            if *relay.upstream_closed.lock().unwrap()
                && relay.from_upstream.lock().unwrap().is_empty()
                && socket.can_send()
            {
                socket.close();
            }

            // Clean up closed sockets
            if socket.state() == TcpState::Closed {
                to_remove.push(handle);
            }
        }

        for handle in to_remove {
            if let Some(relay) = relays.remove(&handle) {
                log::debug!("Connection to {} closed", relay.dest);
            }
            sockets.remove(handle);
        }

        // 4. Write outgoing packets from smoltcp to TUN
        while let Some(pkt) = bridge.pop_tx() {
            let _ = tun.send(&pkt).await;
        }

        // 5. Update GUI status
        {
            let mut s = status.lock().unwrap();
            s.connections = relays.len();
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
    log::info!("Proxy loop stopped");
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
        let now = SmolInstant::from_millis(0);
        assert!(bridge.receive(now).is_some());
    }

    #[test]
    fn format_ip_works() {
        assert_eq!(format_ip(&[10, 0, 85, 1]), "10.0.85.1");
        assert_eq!(format_ip(&[192, 168, 1, 1]), "192.168.1.1");
    }

    #[test]
    fn parse_tcp_syn_packet() {
        // Minimal IPv4 + TCP SYN packet
        // IPv4 header (20 bytes): version=4, ihl=5, total_len=40, proto=6 (TCP)
        // TCP header (20 bytes): src_port=12345, dst_port=443, SYN flag set
        let mut pkt = vec![0u8; 40];
        // IPv4 header
        pkt[0] = 0x45; // version=4, ihl=5
        pkt[2] = 0; pkt[3] = 40; // total length = 40
        pkt[8] = 64; // TTL
        pkt[9] = 6;  // protocol = TCP
        // src IP: 192.168.1.100
        pkt[12] = 192; pkt[13] = 168; pkt[14] = 1; pkt[15] = 100;
        // dst IP: 8.8.8.8
        pkt[16] = 8; pkt[17] = 8; pkt[18] = 8; pkt[19] = 8;
        // TCP header at offset 20
        pkt[20] = (12345 >> 8) as u8; pkt[21] = (12345 & 0xFF) as u8; // src port
        pkt[22] = (443 >> 8) as u8; pkt[23] = (443 & 0xFF) as u8; // dst port
        pkt[32] = 0x50; // data offset = 5 (20 bytes)
        pkt[33] = 0x02; // SYN flag

        let result = parse_tcp_packet(&pkt);
        assert!(result.is_some());
        let (src, dst, is_syn) = result.unwrap();
        assert_eq!(*src.ip(), Ipv4Addr::new(192, 168, 1, 100));
        assert_eq!(src.port(), 12345);
        assert_eq!(*dst.ip(), Ipv4Addr::new(8, 8, 8, 8));
        assert_eq!(dst.port(), 443);
        assert!(is_syn);
    }
}
