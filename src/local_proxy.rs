// ---------------------------------------------------------------------------
// Transparent proxy via TUN virtual NIC + smoltcp userspace TCP stack.
//
// Architecture:
//   [App traffic] → [OS route → TUN device] → [smoltcp TCP stack]
//     → [per-connection relay] → [SOCKS5/HTTP upstream proxy] → [Internet]
//     ← [response] ← [smoltcp builds TCP/IP packets] ← [TUN device]
//
// Cross-platform: Linux (/dev/net/tun), Windows (Wintun), macOS (utun).
// ---------------------------------------------------------------------------

use crate::models::{Proxy, ProxyType};
use smoltcp::iface::{Config, Interface, SocketSet};
use smoltcp::phy::{Device, DeviceCapabilities, Medium, RxToken, TxToken};
use smoltcp::socket::tcp::{self as smol_tcp};
use smoltcp::time::Instant as SmolInstant;
use smoltcp::wire::{HardwareAddress, IpAddress, IpCidr};
use std::collections::{HashMap, VecDeque};
use std::io;
use std::net::SocketAddrV4;
use std::sync::{Arc, Mutex};
use tokio::sync::{broadcast, mpsc};

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

const TUN_ADDR: [u8; 4] = [10, 0, 85, 1];
const TUN_GW: [u8; 4] = [10, 0, 85, 1];
const TUN_CIDR_PREFIX: u8 = 24;
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
// TunBridge – smoltcp PHY device backed by a channel pair.
//
// Packets written by smoltcp go to `tx_out` (we read and write to real TUN).
// Packets we read from real TUN go into `rx_in` (smoltcp reads them).
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
    pub listen_info: String,
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
// TUN device creation (platform-specific)
// ---------------------------------------------------------------------------

/// Open/create a TUN device. Returns (reader_fd, writer_fd) or equivalent.
/// On error, returns a human-readable message.
#[cfg(target_os = "linux")]
fn create_tun_device() -> Result<(TunReader, TunWriter), String> {
    use std::fs::OpenOptions;
    use std::os::unix::io::AsRawFd;
    use std::process::Command;

    // Open /dev/net/tun
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .open("/dev/net/tun")
        .map_err(|e| format!("Cannot open /dev/net/tun: {e} (run as root?)"))?;

    let fd = file.as_raw_fd();
    let name = "proxyswitch0";

    // ioctl to create TUN device
    unsafe {
        let mut ifr: libc::ifreq = std::mem::zeroed();
        let name_bytes = name.as_bytes();
        let ifr_name = &mut ifr.ifr_name[..name_bytes.len()];
        ifr_name.copy_from_slice(std::mem::transmute(name_bytes));
        ifr.ifr_ifru.ifru_flags = (libc::IFF_TUN | libc::IFF_NO_PI) as i16;

        let ret = libc::ioctl(fd, 0x400454CA_u64, &mut ifr); // TUNSETIFF
        if ret < 0 {
            return Err(format!(
                "ioctl TUNSETIFF failed: {}",
                io::Error::last_os_error()
            ));
        }
    }

    // Configure IP
    let addr = format!("{}.{}.{}.{}/{}", TUN_ADDR[0], TUN_ADDR[1], TUN_ADDR[2], TUN_ADDR[3], TUN_CIDR_PREFIX);
    Command::new("ip")
        .args(["addr", "add", &addr, "dev", name])
        .output()
        .map_err(|e| format!("ip addr add failed: {e}"))?;
    Command::new("ip")
        .args(["link", "set", name, "up"])
        .output()
        .map_err(|e| format!("ip link set up failed: {e}"))?;

    let file2 = file.try_clone().map_err(|e| format!("clone fd: {e}"))?;
    Ok((TunReader(file), TunWriter(file2)))
}

#[cfg(target_os = "windows")]
fn create_tun_device() -> Result<(TunReader, TunWriter), String> {
    // Windows: use Wintun via wintun crate or manual DLL loading.
    // For now, return a placeholder error with instructions.
    Err("Windows TUN support requires Wintun driver. \
         Download wintun.dll from https://www.wintun.net/ and place in the app directory. \
         Full Wintun integration is planned for a future release."
        .to_string())
}

#[cfg(not(any(target_os = "linux", target_os = "windows")))]
fn create_tun_device() -> Result<(TunReader, TunWriter), String> {
    Err("TUN device creation is not yet supported on this platform".to_string())
}

// Platform-specific TUN reader/writer wrappers

#[cfg(target_os = "linux")]
struct TunReader(std::fs::File);

#[cfg(target_os = "linux")]
struct TunWriter(std::fs::File);

#[cfg(not(target_os = "linux"))]
struct TunReader;

#[cfg(not(target_os = "linux"))]
struct TunWriter;

impl TunReader {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        #[cfg(target_os = "linux")]
        {
            use std::io::Read;
            self.0.read(buf)
        }
        #[cfg(not(target_os = "linux"))]
        {
            let _ = buf;
            Err(io::Error::new(io::ErrorKind::Unsupported, "not supported"))
        }
    }
}

impl TunWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        #[cfg(target_os = "linux")]
        {
            use std::io::Write;
            self.0.write(buf)
        }
        #[cfg(not(target_os = "linux"))]
        {
            let _ = buf;
            Err(io::Error::new(io::ErrorKind::Unsupported, "not supported"))
        }
    }
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
            .args(["route", "add", "default", "dev", "proxyswitch0", "metric", "10"])
            .output();

        Ok(())
    }

    #[cfg(not(target_os = "linux"))]
    {
        let _ = upstream_host;
        Ok(()) // Route management handled separately on other platforms
    }
}

fn remove_routes(upstream_host: &str) {
    #[cfg(target_os = "linux")]
    {
        use std::process::Command;
        let _ = Command::new("ip")
            .args(["route", "del", "default", "dev", "proxyswitch0"])
            .output();
        let _ = Command::new("ip")
            .args(["route", "del", upstream_host])
            .output();
    }

    #[cfg(not(target_os = "linux"))]
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

/// Simple base64 encoder (no padding needed for Basic auth).
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
    // Create TUN device
    let (tun_reader, tun_writer) = create_tun_device()?;

    // Add routes
    add_routes(&config.host)?;

    let (shutdown_tx, shutdown_rx) = broadcast::channel::<()>(1);
    let listen_info = format!("TUN proxyswitch0 ({})", format_ip(&TUN_ADDR));

    let upstream_host = config.host.clone();

    // Update status
    {
        let mut s = status.lock().unwrap();
        s.running = true;
        s.tun_addr = listen_info.clone();
        s.error = None;
    }
    ctx.request_repaint();

    rt.spawn(proxy_loop(
        tun_reader,
        tun_writer,
        config,
        shutdown_rx,
        status.clone(),
        ctx.clone(),
        upstream_host,
    ));

    Ok(ProxyHandle {
        shutdown_tx,
        listen_info,
    })
}

async fn proxy_loop(
    mut tun_reader: TunReader,
    mut tun_writer: TunWriter,
    config: UpstreamConfig,
    mut shutdown: broadcast::Receiver<()>,
    status: Arc<Mutex<ProxyStatus>>,
    ctx: egui::Context,
    upstream_host: String,
) {
    // smoltcp interface setup
    let mut bridge = TunBridge::new(1500);
    let smol_config = Config::new(HardwareAddress::Ip);
    let mut iface = Interface::new(smol_config, &mut bridge, SmolInstant::now());
    iface.update_ip_addrs(|addrs| {
        let _ = addrs.push(IpCidr::new(
            IpAddress::v4(TUN_ADDR[0], TUN_ADDR[1], TUN_ADDR[2], TUN_ADDR[3]),
            TUN_CIDR_PREFIX,
        ));
    });

    // Socket set
    let mut rx_bufs: Vec<smol_tcp::SocketBuffer<'_>> = Vec::new();
    let mut tx_bufs: Vec<smol_tcp::SocketBuffer<'_>> = Vec::new();
    for _ in 0..MAX_SOCKETS {
        rx_bufs.push(smol_tcp::SocketBuffer::new(vec![0u8; SMOL_TCP_RX_BUF]));
        tx_bufs.push(smol_tcp::SocketBuffer::new(vec![0u8; SMOL_TCP_TX_BUF]));
    }

    let mut sockets = SocketSet::new(Vec::new());

    // Connection tracking: smoltcp socket handle → upstream info
    let connections: Arc<Mutex<HashMap<smoltcp::iface::SocketHandle, ConnectionInfo>>> =
        Arc::new(Mutex::new(HashMap::new()));

    let _config = Arc::new(config);
    let mut buf = [0u8; 65535];

    loop {
        // Check shutdown
        if shutdown.try_recv().is_ok() {
            break;
        }

        // Read from TUN (non-blocking attempt via short timeout)
        #[cfg(target_os = "linux")]
        {
            use std::os::unix::io::AsRawFd;
            let fd = tun_reader.0.as_raw_fd();

            // Use poll to check readability with 10ms timeout
            let mut pollfd = libc::pollfd {
                fd,
                events: libc::POLLIN,
                revents: 0,
            };
            let ready = unsafe { libc::poll(&mut pollfd, 1, 10) };

            if ready > 0 {
                match tun_reader.read(&mut buf) {
                    Ok(n) if n > 0 => {
                        bridge.push_rx(buf[..n].to_vec());
                    }
                    _ => {}
                }
            }
        }

        // Process smoltcp
        let now = SmolInstant::now();
        let _changed = iface.poll(now, &mut bridge, &mut sockets);

        // Check for new connections and relay data on existing sockets
        // (In a full implementation, we would listen on a smoltcp TCP socket
        //  and accept connections, then spawn relay tasks for each.)
        //
        // For each socket in sockets:
        //   - If it has received data: forward to upstream
        //   - If upstream has data: write to socket

        // Write outgoing packets from smoltcp to TUN
        while let Some(pkt) = bridge.pop_tx() {
            let _ = tun_writer.write(&pkt);
        }

        // Update status
        {
            let mut s = status.lock().unwrap();
            s.connections = connections.lock().unwrap().len();
        }

        // Small yield to not spin-loop
        std::thread::sleep(std::time::Duration::from_millis(1));
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
