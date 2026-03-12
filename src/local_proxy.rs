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

const TUN_NAME: &str = "ps0";
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
// CIDR parsing helper
// ---------------------------------------------------------------------------

/// Parse "IP/prefix" (e.g. "172.29.0.1/24") or plain IP (defaults to /24).
/// Returns (local_ip, prefix, gateway_ip).
/// The gateway IP is automatically derived as local_ip + 1 (e.g. .1 → .2).
fn parse_tun_cidr(input: &str) -> Result<(Ipv4Addr, u8, Ipv4Addr), String> {
    let (ip, prefix) = if let Some((ip_str, prefix_str)) = input.split_once('/') {
        let ip: Ipv4Addr = ip_str
            .trim()
            .parse()
            .map_err(|e| format!("Invalid TUN IP '{}': {}", ip_str, e))?;
        let prefix: u8 = prefix_str
            .trim()
            .parse()
            .map_err(|e| format!("Invalid prefix length '{}': {}", prefix_str, e))?;
        if prefix > 32 {
            return Err(format!("Prefix length {} exceeds 32", prefix));
        }
        (ip, prefix)
    } else {
        let ip: Ipv4Addr = input
            .trim()
            .parse()
            .map_err(|e| format!("Invalid TUN IP '{}': {}", input, e))?;
        (ip, 24)
    };

    // Derive gateway: local IP + 1 (e.g. 172.29.0.1 → 172.29.0.2)
    let ip_u32 = u32::from(ip);
    let gw_u32 = ip_u32.checked_add(1)
        .ok_or_else(|| format!("Cannot derive gateway from {}", ip))?;
    let gateway = Ipv4Addr::from(gw_u32);

    Ok((ip, prefix, gateway))
}

// ---------------------------------------------------------------------------
// TUN device creation via tun-rs
// ---------------------------------------------------------------------------

fn create_tun_device(tun_addr: &str) -> Result<tun_rs::AsyncDevice, String> {
    let (ip, prefix, gateway) = parse_tun_cidr(tun_addr)?;
    log::info!("Creating TUN device: name={}, addr={}/{}, gateway={}, mtu={}",
        TUN_NAME, ip, prefix, gateway, TUN_MTU);
    log::info!("Platform: {}, Arch: {}", std::env::consts::OS, std::env::consts::ARCH);

    let mut builder = tun_rs::DeviceBuilder::new();
    builder = builder
        .name(TUN_NAME)
        .ipv4(ip, prefix, Some(gateway))
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
// Route management via OS APIs
//
// Windows: IP Helper API (iphlpapi.dll) — GetIpForwardTable / CreateIpForwardEntry / DeleteIpForwardEntry
// Linux:   /proc/net/route for reading + SIOCADDRT / SIOCDELRT ioctl for writing
// ---------------------------------------------------------------------------

/// Resolve hostname to IPv4 address (DNS if needed).
fn resolve_to_ipv4(host: &str) -> Result<Ipv4Addr, String> {
    if let Ok(ip) = host.parse::<Ipv4Addr>() {
        return Ok(ip);
    }
    use std::net::ToSocketAddrs;
    let addr_str = format!("{}:0", host);
    for addr in addr_str
        .to_socket_addrs()
        .map_err(|e| format!("DNS resolve {}: {e}", host))?
    {
        if let std::net::SocketAddr::V4(v4) = addr {
            log::info!("Resolved {} -> {}", host, v4.ip());
            return Ok(*v4.ip());
        }
    }
    Err(format!("Cannot resolve {} to IPv4", host))
}

// ---- Windows: IP Helper API (raw FFI to iphlpapi.dll) ----

#[cfg(target_os = "windows")]
mod win_route {
    use std::net::Ipv4Addr;

    const NO_ERROR: u32 = 0;
    const ERROR_INSUFFICIENT_BUFFER: u32 = 122;
    const ERROR_OBJECT_ALREADY_EXISTS: u32 = 5010;
    const MIB_IPROUTE_TYPE_DIRECT: u32 = 3;
    const MIB_IPROUTE_TYPE_INDIRECT: u32 = 4;
    const MIB_IPPROTO_NETMGMT: u32 = 3;
    const AF_INET: u16 = 2;

    /// NET_LUID is a union (u64). Only the Value member is needed.
    #[repr(C)]
    #[derive(Clone, Copy, Default)]
    pub struct NET_LUID {
        pub value: u64,
    }

    #[repr(C)]
    #[derive(Clone, Copy, Debug, Default)]
    pub struct MIB_IPFORWARDROW {
        pub dwForwardDest: u32,
        pub dwForwardMask: u32,
        pub dwForwardPolicy: u32,
        pub dwForwardNextHop: u32,
        pub dwForwardIfIndex: u32,
        pub dwForwardType: u32,
        pub dwForwardProto: u32,
        pub dwForwardAge: u32,
        pub dwForwardNextHopAS: u32,
        pub dwForwardMetric1: u32,
        pub dwForwardMetric2: u32,
        pub dwForwardMetric3: u32,
        pub dwForwardMetric4: u32,
        pub dwForwardMetric5: u32,
    }

    #[repr(C)]
    pub struct MIB_IPFORWARDTABLE {
        pub dwNumEntries: u32,
        pub table: [MIB_IPFORWARDROW; 1], // variable-length
    }

    #[repr(C)]
    #[derive(Clone, Copy, Debug)]
    pub struct MIB_IPADDRROW {
        pub dwAddr: u32,
        pub dwIndex: u32,
        pub dwMask: u32,
        pub dwBCastAddr: u32,
        pub dwReasmSize: u32,
        pub unused1: u16,
        pub wType: u16,
    }

    #[repr(C)]
    pub struct MIB_IPADDRTABLE {
        pub dwNumEntries: u32,
        pub table: [MIB_IPADDRROW; 1],
    }

    #[link(name = "iphlpapi")]
    extern "system" {
        fn GetIpForwardTable(
            pIpForwardTable: *mut MIB_IPFORWARDTABLE,
            pdwSize: *mut u32,
            bOrder: i32,
        ) -> u32;
        fn CreateIpForwardEntry(pRoute: *const MIB_IPFORWARDROW) -> u32;
        fn DeleteIpForwardEntry(pRoute: *const MIB_IPFORWARDROW) -> u32;
        fn GetIpAddrTable(
            pIpAddrTable: *mut MIB_IPADDRTABLE,
            pdwSize: *mut u32,
            bOrder: i32,
        ) -> u32;
        fn ConvertInterfaceIndexToLuid(
            InterfaceIndex: u32,
            InterfaceLuid: *mut NET_LUID,
        ) -> u32;
    }

    // MIB_IPINTERFACE_ROW (netioapi.h) — repr(C) layout matching the Windows SDK.
    // We define all fields up to and including Metric; the remainder is padded.
    // Reference: https://learn.microsoft.com/en-us/windows/win32/api/netioapi/ns-netioapi-mib_ipinterface_row
    #[repr(C)]
    pub struct MIB_IPINTERFACE_ROW {
        pub family: u16,                              //  0
        _pad0: [u8; 6],                               //  2 (align InterfaceLuid to 8)
        pub interface_luid: NET_LUID,                  //  8
        pub interface_index: u32,                      // 16
        pub max_reassembly_size: u32,                  // 20
        pub interface_identifier: u64,                 // 24
        pub min_router_advertisement_interval: u32,    // 32
        pub max_router_advertisement_interval: u32,    // 36
        pub advertising_enabled: u8,                   // 40
        pub forwarding_enabled: u8,                    // 41
        pub weak_host_send: u8,                        // 42
        pub weak_host_receive: u8,                     // 43
        pub use_automatic_metric: u8,                  // 44
        pub use_neighbor_unreachability_detection: u8,  // 45
        pub managed_address_configuration_supported: u8, // 46
        pub other_stateful_configuration_supported: u8,  // 47
        pub advertise_default_route: u8,               // 48
        _pad1: [u8; 3],                                // 49 (align next i32 to 4)
        pub router_discovery_behavior: i32,            // 52
        pub dad_transmits: u32,                        // 56
        pub base_reachable_time: u32,                  // 60
        pub retransmit_time: u32,                      // 64
        pub path_mtu_discovery_timeout: u32,           // 68
        pub link_local_address_behavior: i32,          // 72
        pub link_local_address_timeout: u32,           // 76
        pub zone_indices: [u32; 16],                   // 80 (64 bytes)
        pub site_prefix_length: u32,                   // 144
        pub metric: u32,                               // 148
        // Remaining: NlMtu(u32), Connected(u8), SupportsWakeUpPatterns(u8),
        // SupportsNeighborDiscovery(u8), SupportsRouterDiscovery(u8),
        // ReachableTime(u32), TransmitOffload(u8), ReceiveOffload(u8),
        // DisableDefaultRoutes(u8), padding(u8)
        _tail: [u8; 16],                               // 152..168 (total = 168 bytes)
    }

    extern "system" {
        fn GetIpInterfaceEntry(row: *mut MIB_IPINTERFACE_ROW) -> u32;
        fn InitializeIpInterfaceEntry(row: *mut MIB_IPINTERFACE_ROW);
    }

    /// Query the interface metric for the given interface index via GetIpInterfaceEntry.
    pub fn get_interface_metric(if_index: u32) -> Result<u32, String> {
        unsafe {
            let mut luid = NET_LUID::default();
            let ret = ConvertInterfaceIndexToLuid(if_index, &mut luid);
            if ret != NO_ERROR {
                return Err(format!("ConvertInterfaceIndexToLuid({}): error {}", if_index, ret));
            }

            let mut row: MIB_IPINTERFACE_ROW = std::mem::zeroed();
            InitializeIpInterfaceEntry(&mut row);
            row.family = AF_INET;
            row.interface_luid = luid;

            let ret = GetIpInterfaceEntry(&mut row);
            if ret != NO_ERROR {
                return Err(format!("GetIpInterfaceEntry(if={}): error {}", if_index, ret));
            }

            Ok(row.metric)
        }
    }

    pub fn ip_to_nbo(ip: Ipv4Addr) -> u32 {
        u32::from_ne_bytes(ip.octets())
    }

    pub fn nbo_to_ip(val: u32) -> Ipv4Addr {
        Ipv4Addr::from(val.to_ne_bytes())
    }

    /// Snapshot of the IPv4 routing table.
    pub struct RouteTable {
        buffer: Vec<u8>,
    }

    impl RouteTable {
        pub fn read() -> Result<Self, String> {
            unsafe {
                let mut size: u32 = 0;
                let ret = GetIpForwardTable(std::ptr::null_mut(), &mut size, 0);
                if ret != ERROR_INSUFFICIENT_BUFFER && ret != NO_ERROR {
                    return Err(format!("GetIpForwardTable size query: error {}", ret));
                }
                let mut buffer = vec![0u8; size as usize];
                let table_ptr = buffer.as_mut_ptr() as *mut MIB_IPFORWARDTABLE;
                let ret = GetIpForwardTable(table_ptr, &mut size, 0);
                if ret != NO_ERROR {
                    return Err(format!("GetIpForwardTable: error {}", ret));
                }
                Ok(RouteTable { buffer })
            }
        }

        pub fn entries(&self) -> &[MIB_IPFORWARDROW] {
            unsafe {
                let table = self.buffer.as_ptr() as *const MIB_IPFORWARDTABLE;
                let num = (*table).dwNumEntries as usize;
                std::slice::from_raw_parts(&(*table).table[0], num)
            }
        }

        /// Find the default gateway route (0.0.0.0/0) that is NOT via the TUN device.
        pub fn find_original_default_gw(&self, tun_ip: Ipv4Addr) -> Option<MIB_IPFORWARDROW> {
            let tun_nbo = ip_to_nbo(tun_ip);
            for entry in self.entries() {
                if entry.dwForwardDest == 0
                    && entry.dwForwardMask == 0
                    && entry.dwForwardNextHop != tun_nbo
                {
                    let gw = nbo_to_ip(entry.dwForwardNextHop);
                    log::info!(
                        "Original default gateway: {} (if_index={}, metric={})",
                        gw,
                        entry.dwForwardIfIndex,
                        entry.dwForwardMetric1
                    );
                    return Some(*entry);
                }
            }
            None
        }

        pub fn log_table(&self) {
            let entries = self.entries();
            log::info!("IP route table ({} entries):", entries.len());
            for (i, e) in entries.iter().enumerate() {
                log::info!(
                    "  [{}] {}/{} gw={} if={} metric={} proto={}",
                    i,
                    nbo_to_ip(e.dwForwardDest),
                    nbo_to_ip(e.dwForwardMask),
                    nbo_to_ip(e.dwForwardNextHop),
                    e.dwForwardIfIndex,
                    e.dwForwardMetric1,
                    e.dwForwardProto
                );
            }
        }
    }

    /// Find the interface index of the TUN device via GetIpAddrTable.
    /// This queries the OS for which interface owns the given IP address — much more
    /// reliable than searching the route table (which may contain stale entries).
    pub fn find_if_index_by_ip(ip: Ipv4Addr) -> Option<u32> {
        let target = ip_to_nbo(ip);
        unsafe {
            let mut size: u32 = 0;
            let ret = GetIpAddrTable(std::ptr::null_mut(), &mut size, 0);
            if ret != ERROR_INSUFFICIENT_BUFFER && ret != NO_ERROR {
                log::error!("GetIpAddrTable size query: error {}", ret);
                return None;
            }
            let mut buffer = vec![0u8; size as usize];
            let table = buffer.as_mut_ptr() as *mut MIB_IPADDRTABLE;
            let ret = GetIpAddrTable(table, &mut size, 0);
            if ret != NO_ERROR {
                log::error!("GetIpAddrTable: error {}", ret);
                return None;
            }
            let num = (*table).dwNumEntries as usize;
            let entries = std::slice::from_raw_parts(&(*table).table[0], num);
            log::info!("IP address table ({} entries):", num);
            for entry in entries {
                let addr = nbo_to_ip(entry.dwAddr);
                let mask = nbo_to_ip(entry.dwMask);
                log::info!("  if={} addr={} mask={}", entry.dwIndex, addr, mask);
                if entry.dwAddr == target {
                    log::info!("  -> TUN interface index: {}", entry.dwIndex);
                    return Some(entry.dwIndex);
                }
            }
        }
        log::warn!("No interface found with IP {}", ip);
        None
    }

    pub fn create_route(
        dest: Ipv4Addr,
        mask: Ipv4Addr,
        gateway_nbo: u32,
        if_index: u32,
        metric: u32,
        on_link: bool,
    ) -> Result<(), String> {
        // On-link (DIRECT): next hop = interface's own IP, type = 3
        // Indirect: next hop = gateway IP, type = 4
        let fwd_type = if on_link { MIB_IPROUTE_TYPE_DIRECT } else { MIB_IPROUTE_TYPE_INDIRECT };
        let row = MIB_IPFORWARDROW {
            dwForwardDest: ip_to_nbo(dest),
            dwForwardMask: ip_to_nbo(mask),
            dwForwardPolicy: 0,
            dwForwardNextHop: gateway_nbo,
            dwForwardIfIndex: if_index,
            dwForwardType: fwd_type,
            dwForwardProto: MIB_IPPROTO_NETMGMT,
            dwForwardAge: 0,
            dwForwardNextHopAS: 0,
            dwForwardMetric1: metric,
            dwForwardMetric2: u32::MAX,
            dwForwardMetric3: u32::MAX,
            dwForwardMetric4: u32::MAX,
            dwForwardMetric5: u32::MAX,
        };

        let ret = unsafe { CreateIpForwardEntry(&row) };
        let gw_ip = nbo_to_ip(gateway_nbo);
        if ret == NO_ERROR || ret == ERROR_OBJECT_ALREADY_EXISTS {
            log::info!(
                "Route created: {}/{} -> {} (if={}, metric={}){}",
                dest,
                mask,
                gw_ip,
                if_index,
                metric,
                if ret == ERROR_OBJECT_ALREADY_EXISTS {
                    " [already existed]"
                } else {
                    ""
                }
            );
            Ok(())
        } else {
            Err(format!(
                "CreateIpForwardEntry {}/{} via {}: error {}",
                dest, mask, gw_ip, ret
            ))
        }
    }

    pub fn delete_route(
        dest: Ipv4Addr,
        mask: Ipv4Addr,
        gateway_nbo: u32,
        if_index: u32,
    ) {
        let row = MIB_IPFORWARDROW {
            dwForwardDest: ip_to_nbo(dest),
            dwForwardMask: ip_to_nbo(mask),
            dwForwardPolicy: 0,
            dwForwardNextHop: gateway_nbo,
            dwForwardIfIndex: if_index,
            ..Default::default()
        };

        let ret = unsafe { DeleteIpForwardEntry(&row) };
        if ret == NO_ERROR {
            log::info!("Route deleted: {}/{}", dest, mask);
        } else {
            log::warn!("DeleteIpForwardEntry {}/{}: error {}", dest, mask, ret);
        }
    }
}

// ---- Linux: /proc/net/route + SIOCADDRT / SIOCDELRT ioctl ----

#[cfg(target_os = "linux")]
mod linux_route {
    use std::net::Ipv4Addr;

    const RTF_UP: u16 = 0x0001;
    const RTF_GATEWAY: u16 = 0x0002;
    const RTF_HOST: u16 = 0x0004;

    /// Kernel `struct rtentry` (see <net/route.h>). Layout must match C on target arch.
    #[repr(C)]
    struct RtEntry {
        rt_pad1: libc::c_ulong,
        rt_dst: libc::sockaddr,
        rt_gateway: libc::sockaddr,
        rt_genmask: libc::sockaddr,
        rt_flags: libc::c_ushort,
        rt_pad2: libc::c_short,
        rt_pad3: libc::c_ulong,
        rt_pad4: *mut libc::c_void,
        rt_metric: libc::c_short,
        rt_dev: *mut libc::c_char,
        rt_mtu: libc::c_ulong,
        rt_window: libc::c_ulong,
        rt_irtt: libc::c_ushort,
    }

    fn make_sockaddr(ip: Ipv4Addr) -> libc::sockaddr {
        unsafe {
            let mut sin: libc::sockaddr_in = std::mem::zeroed();
            sin.sin_family = libc::AF_INET as libc::sa_family_t;
            sin.sin_addr.s_addr = u32::from_ne_bytes(ip.octets());
            *(&sin as *const libc::sockaddr_in as *const libc::sockaddr)
        }
    }

    #[derive(Debug)]
    pub struct DefaultGateway {
        pub gateway: Ipv4Addr,
        pub device: String,
    }

    /// Read /proc/net/route to find the original default gateway (not through TUN).
    pub fn find_default_gateway(exclude_dev: &str) -> Option<DefaultGateway> {
        let content = std::fs::read_to_string("/proc/net/route").ok()?;
        for line in content.lines().skip(1) {
            let fields: Vec<&str> = line.split_whitespace().collect();
            if fields.len() < 8 {
                continue;
            }
            let iface = fields[0];
            let dest = fields[1];
            let gateway = fields[2];
            let mask = fields[7];

            // Default route: Destination=00000000, Mask=00000000
            if dest == "00000000" && mask == "00000000" && iface != exclude_dev {
                if let Ok(gw_val) = u32::from_str_radix(gateway, 16) {
                    let gw_ip = Ipv4Addr::from(gw_val.to_ne_bytes());
                    log::info!(
                        "Default gateway from /proc/net/route: {} dev {}",
                        gw_ip,
                        iface
                    );
                    return Some(DefaultGateway {
                        gateway: gw_ip,
                        device: iface.to_string(),
                    });
                }
            }
        }
        None
    }

    /// Add a route via SIOCADDRT ioctl.
    pub fn add_route(
        dest: Ipv4Addr,
        mask: Ipv4Addr,
        gateway: Option<Ipv4Addr>,
        dev: Option<&str>,
        metric: i16,
    ) -> Result<(), String> {
        unsafe {
            let fd = libc::socket(libc::AF_INET, libc::SOCK_DGRAM, 0);
            if fd < 0 {
                return Err(format!(
                    "socket(): {}",
                    std::io::Error::last_os_error()
                ));
            }

            let mut rt: RtEntry = std::mem::zeroed();
            rt.rt_dst = make_sockaddr(dest);
            rt.rt_genmask = make_sockaddr(mask);
            rt.rt_flags = RTF_UP;
            rt.rt_metric = metric;

            if let Some(gw) = gateway {
                rt.rt_gateway = make_sockaddr(gw);
                rt.rt_flags |= RTF_GATEWAY;
            }

            if mask == Ipv4Addr::new(255, 255, 255, 255) {
                rt.rt_flags |= RTF_HOST;
            }

            let dev_cstr;
            if let Some(d) = dev {
                dev_cstr =
                    std::ffi::CString::new(d).map_err(|e| format!("CString: {e}"))?;
                rt.rt_dev = dev_cstr.as_ptr() as *mut libc::c_char;
            }

            let ret = libc::ioctl(fd, libc::SIOCADDRT, &rt as *const RtEntry);
            libc::close(fd);

            if ret < 0 {
                let err = std::io::Error::last_os_error();
                if err.raw_os_error() == Some(libc::EEXIST) {
                    log::info!("Route already exists: {}/{} (ok)", dest, mask);
                    return Ok(());
                }
                return Err(format!("SIOCADDRT {}/{}: {}", dest, mask, err));
            }

            log::info!(
                "Route added: {}/{} gw={:?} dev={:?} metric={}",
                dest,
                mask,
                gateway,
                dev,
                metric
            );
            Ok(())
        }
    }

    /// Delete a route via SIOCDELRT ioctl.
    pub fn delete_route(dest: Ipv4Addr, mask: Ipv4Addr, dev: Option<&str>) {
        unsafe {
            let fd = libc::socket(libc::AF_INET, libc::SOCK_DGRAM, 0);
            if fd < 0 {
                log::warn!("socket() for SIOCDELRT failed");
                return;
            }

            let mut rt: RtEntry = std::mem::zeroed();
            rt.rt_dst = make_sockaddr(dest);
            rt.rt_genmask = make_sockaddr(mask);
            rt.rt_flags = RTF_UP;

            let dev_cstr;
            if let Some(d) = dev {
                dev_cstr = std::ffi::CString::new(d).ok();
                if let Some(ref c) = dev_cstr {
                    rt.rt_dev = c.as_ptr() as *mut libc::c_char;
                }
            }

            let ret = libc::ioctl(fd, libc::SIOCDELRT, &rt as *const RtEntry);
            libc::close(fd);

            if ret < 0 {
                log::warn!(
                    "SIOCDELRT {}/{}: {}",
                    dest,
                    mask,
                    std::io::Error::last_os_error()
                );
            } else {
                log::info!("Route deleted: {}/{}", dest, mask);
            }
        }
    }

    /// Log current route table from /proc/net/route.
    pub fn log_route_table() {
        match std::fs::read_to_string("/proc/net/route") {
            Ok(content) => {
                log::info!("Route table (/proc/net/route):");
                for line in content.lines() {
                    log::info!("  {}", line);
                }
            }
            Err(e) => log::warn!("Cannot read /proc/net/route: {}", e),
        }
    }
}

// ---- Common route management ----

#[allow(unused_variables)]
fn add_routes(upstream_host: &str, tun_addr: &str, tun_gw_addr: &str) -> Result<(), String> {
    let upstream_ip = resolve_to_ipv4(upstream_host)?;
    log::info!("Adding routes (upstream={} -> {})", upstream_host, upstream_ip);

    #[cfg(target_os = "linux")]
    {
        // Step 1: Find original default gateway (from /proc/net/route)
        let gw = linux_route::find_default_gateway(TUN_NAME);

        // Step 2: Host route for upstream proxy via original gateway (prevents routing loop)
        // Skip for loopback addresses — they already have a more-specific 127.0.0.0/8 route.
        let is_loopback = upstream_ip.octets()[0] == 127;
        if is_loopback {
            log::info!("Upstream {} is loopback, skipping host route", upstream_ip);
        } else if let Some(ref gw_info) = gw {
            log::info!(
                "Adding host route for upstream {} via {} dev {}",
                upstream_ip,
                gw_info.gateway,
                gw_info.device
            );
            linux_route::add_route(
                upstream_ip,
                Ipv4Addr::new(255, 255, 255, 255),
                Some(gw_info.gateway),
                Some(&gw_info.device),
                0,
            )?;
        } else {
            log::warn!("No original default gateway found! Routing loop risk.");
        }

        // Step 3: Split default routes via TUN (0.0.0.0/1 + 128.0.0.0/1)
        // More specific than any /0 default route → always wins regardless of metric.
        linux_route::add_route(
            Ipv4Addr::new(0, 0, 0, 0),
            Ipv4Addr::new(128, 0, 0, 0),
            None,
            Some(TUN_NAME),
            1,
        )?;
        linux_route::add_route(
            Ipv4Addr::new(128, 0, 0, 0),
            Ipv4Addr::new(128, 0, 0, 0),
            None,
            Some(TUN_NAME),
            1,
        )?;

        // Step 4: Verify
        linux_route::log_route_table();
        Ok(())
    }

    #[cfg(target_os = "windows")]
    {
        let tun_ip: Ipv4Addr = tun_addr.parse().map_err(|e| format!("Invalid TUN address: {e}"))?;
        let tun_gw: Ipv4Addr = tun_gw_addr.parse().map_err(|e| format!("Invalid TUN gateway: {e}"))?;
        log::info!("TUN local={}, gateway={}", tun_ip, tun_gw);

        // Step 0: Clean up stale TUN routes from previous runs
        // Match by gateway=TUN_GW (routes use the gateway IP as next hop)
        if let Ok(stale_snap) = win_route::RouteTable::read() {
            let tun_gw_nbo = win_route::ip_to_nbo(tun_gw);
            let tun_ip_nbo = win_route::ip_to_nbo(tun_ip);
            for entry in stale_snap.entries() {
                // Match routes via TUN gateway or TUN IP (legacy cleanup)
                if entry.dwForwardNextHop == tun_gw_nbo || entry.dwForwardNextHop == tun_ip_nbo {
                    let dest = win_route::nbo_to_ip(entry.dwForwardDest);
                    let mask = win_route::nbo_to_ip(entry.dwForwardMask);
                    log::info!("Cleaning stale TUN route: {}/{} if={}", dest, mask, entry.dwForwardIfIndex);
                    win_route::delete_route(dest, mask, entry.dwForwardNextHop, entry.dwForwardIfIndex);
                }
            }
        }

        // Step 1: Wait for TUN IP to register, then read route table.
        // The TUN device may need time to register its IP after creation.
        // Retry up to 3 seconds with 200ms intervals.
        let mut tun_if_idx = None;
        for attempt in 0..15 {
            tun_if_idx = win_route::find_if_index_by_ip(tun_ip);
            if tun_if_idx.is_some() {
                break;
            }
            if attempt < 14 {
                log::info!(
                    "TUN IP {} not yet in address table, retrying ({}/15)...",
                    tun_ip,
                    attempt + 1
                );
                std::thread::sleep(std::time::Duration::from_millis(200));
            }
        }
        let tun_if_idx = tun_if_idx
            .ok_or_else(|| "Cannot find TUN interface in IP address table after 3s".to_string())?;

        // Step 2: Read route table and query interface metric via GetIpInterfaceEntry.
        // On Windows Vista+, CreateIpForwardEntry requires dwForwardMetric1 >=
        // interface_metric, otherwise error 160 (BAD_ARGUMENTS).
        let snapshot = win_route::RouteTable::read()?;
        snapshot.log_table();
        let original_gw = snapshot.find_original_default_gw(tun_ip);
        let tun_if_metric = match win_route::get_interface_metric(tun_if_idx) {
            Ok(m) => {
                log::info!("TUN interface metric from GetIpInterfaceEntry: {} (if_idx={})", m, tun_if_idx);
                m
            }
            Err(e) => {
                // Fallback: derive from existing route on the TUN interface
                let fallback = snapshot.entries()
                    .iter()
                    .filter(|e| e.dwForwardIfIndex == tun_if_idx)
                    .min_by_key(|e| e.dwForwardMetric1)
                    .map(|e| e.dwForwardMetric1)
                    .unwrap_or(0);
                log::warn!(
                    "GetIpInterfaceEntry failed ({}), using route-derived metric: {} (if_idx={})",
                    e, fallback, tun_if_idx
                );
                fallback
            }
        };

        // Step 3: Host route for upstream proxy via original gateway (prevents routing loop)
        // Skip for loopback addresses — 127.0.0.0/8 already routes to loopback interface.
        let is_loopback = upstream_ip.octets()[0] == 127;
        if is_loopback {
            log::info!("Upstream {} is loopback, skipping host route", upstream_ip);
        } else if let Some(gw_row) = &original_gw {
            log::info!("Adding host route for upstream proxy {}", upstream_ip);
            win_route::create_route(
                upstream_ip,
                Ipv4Addr::new(255, 255, 255, 255),
                gw_row.dwForwardNextHop,
                gw_row.dwForwardIfIndex,
                gw_row.dwForwardMetric1, // use original route's metric
                false, // indirect: via gateway
            )?;
        } else {
            log::warn!("No original default gateway found! Routing loop risk.");
        }

        // Step 4: Split default routes via TUN (0.0.0.0/1 + 128.0.0.0/1)
        // Next hop = TUN gateway IP (e.g. 172.29.0.2), NOT TUN's own IP.
        // Metric must be >= TUN interface metric (Vista+ requirement).
        let tun_gw_nbo = win_route::ip_to_nbo(tun_gw);
        win_route::create_route(
            Ipv4Addr::new(0, 0, 0, 0),
            Ipv4Addr::new(128, 0, 0, 0),
            tun_gw_nbo,
            tun_if_idx,
            tun_if_metric,
            false, // indirect: via TUN gateway
        )?;
        win_route::create_route(
            Ipv4Addr::new(128, 0, 0, 0),
            Ipv4Addr::new(128, 0, 0, 0),
            tun_gw_nbo,
            tun_if_idx,
            tun_if_metric,
            false, // indirect: via TUN gateway
        )?;

        // Step 5: Verify
        if let Ok(snap) = win_route::RouteTable::read() {
            snap.log_table();
        }
        Ok(())
    }

    #[cfg(not(any(target_os = "linux", target_os = "windows")))]
    {
        let _ = upstream_ip;
        Ok(())
    }
}

#[allow(unused_variables)]
fn remove_routes(upstream_host: &str, tun_addr: &str, tun_gw_addr: &str) {
    let upstream_ip = match resolve_to_ipv4(upstream_host) {
        Ok(ip) => ip,
        Err(e) => {
            log::warn!("Cannot resolve upstream for route cleanup: {e}");
            return;
        }
    };
    log::info!("Removing routes (upstream={})", upstream_ip);

    #[cfg(target_os = "linux")]
    {
        linux_route::delete_route(
            Ipv4Addr::new(0, 0, 0, 0),
            Ipv4Addr::new(128, 0, 0, 0),
            Some(TUN_NAME),
        );
        linux_route::delete_route(
            Ipv4Addr::new(128, 0, 0, 0),
            Ipv4Addr::new(128, 0, 0, 0),
            Some(TUN_NAME),
        );
        // Skip loopback — we never added a host route for it
        if upstream_ip.octets()[0] != 127 {
            linux_route::delete_route(
                upstream_ip,
                Ipv4Addr::new(255, 255, 255, 255),
                None,
            );
        }
    }

    #[cfg(target_os = "windows")]
    {
        let tun_ip: Ipv4Addr = tun_addr.parse().unwrap_or(Ipv4Addr::new(172, 29, 0, 1));
        let tun_gw: Ipv4Addr = tun_gw_addr.parse().unwrap_or(Ipv4Addr::new(172, 29, 0, 2));

        // Remove TUN split routes (gateway = TUN gateway IP)
        if let Some(tun_if_idx) = win_route::find_if_index_by_ip(tun_ip) {
            let tun_gw_nbo = win_route::ip_to_nbo(tun_gw);
            win_route::delete_route(
                Ipv4Addr::new(0, 0, 0, 0),
                Ipv4Addr::new(128, 0, 0, 0),
                tun_gw_nbo,
                tun_if_idx,
            );
            win_route::delete_route(
                Ipv4Addr::new(128, 0, 0, 0),
                Ipv4Addr::new(128, 0, 0, 0),
                tun_gw_nbo,
                tun_if_idx,
            );
        }
        // Remove upstream host route (skip loopback)
        if upstream_ip.octets()[0] != 127 {
            if let Ok(snapshot) = win_route::RouteTable::read() {
                for entry in snapshot.entries() {
                    let dest = win_route::nbo_to_ip(entry.dwForwardDest);
                    if dest == upstream_ip && entry.dwForwardMask == u32::MAX {
                        win_route::delete_route(
                            upstream_ip,
                            Ipv4Addr::new(255, 255, 255, 255),
                            entry.dwForwardNextHop,
                            entry.dwForwardIfIndex,
                        );
                        break;
                    }
                }
            }
        }
    }

    #[cfg(not(any(target_os = "linux", target_os = "windows")))]
    {
        let _ = upstream_ip;
    }
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
    tun_addr: &str,
) -> Result<ProxyHandle, String> {
    let (tun_ip, prefix, tun_gw) = parse_tun_cidr(tun_addr)?;
    let tun_ip_str = tun_ip.to_string();
    let tun_gw_str = tun_gw.to_string();

    let tun_device = create_tun_device(tun_addr)?;
    add_routes(&config.host, &tun_ip_str, &tun_gw_str)?;

    let (shutdown_tx, shutdown_rx) = broadcast::channel::<()>(1);
    let listen_info = format!("TUN {} ({}/{} gw {})", TUN_NAME, tun_ip, prefix, tun_gw);
    let upstream_host = config.host.clone();

    {
        let mut s = status.lock().unwrap();
        s.running = true;
        s.tun_addr = listen_info;
        s.error = None;
    }
    ctx.request_repaint();

    rt.spawn(proxy_loop(tun_device, config, shutdown_rx, status, ctx, upstream_host, tun_ip_str, tun_gw_str));

    Ok(ProxyHandle { shutdown_tx })
}

async fn proxy_loop(
    tun_device: tun_rs::AsyncDevice,
    config: UpstreamConfig,
    mut shutdown: broadcast::Receiver<()>,
    status: Arc<Mutex<ProxyStatus>>,
    ctx: egui::Context,
    upstream_host: String,
    tun_addr: String,
    tun_gw_addr: String,
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
    remove_routes(&upstream_host, &tun_addr, &tun_gw_addr);
    {
        let mut s = status.lock().unwrap();
        s.running = false;
        s.connections = 0;
    }
    ctx.request_repaint();
    log::info!("Proxy loop stopped");
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
    fn parse_tun_cidr_with_prefix() {
        let (ip, prefix, gw) = parse_tun_cidr("172.29.0.1/24").unwrap();
        assert_eq!(ip, Ipv4Addr::new(172, 29, 0, 1));
        assert_eq!(prefix, 24);
        assert_eq!(gw, Ipv4Addr::new(172, 29, 0, 2)); // auto-derived gateway
    }

    #[test]
    fn parse_tun_cidr_without_prefix() {
        let (ip, prefix, gw) = parse_tun_cidr("10.0.85.1").unwrap();
        assert_eq!(ip, Ipv4Addr::new(10, 0, 85, 1));
        assert_eq!(prefix, 24); // default
        assert_eq!(gw, Ipv4Addr::new(10, 0, 85, 2));
    }

    #[test]
    fn parse_tun_cidr_narrow_prefix() {
        let (ip, prefix, gw) = parse_tun_cidr("172.29.0.1/30").unwrap();
        assert_eq!(ip, Ipv4Addr::new(172, 29, 0, 1));
        assert_eq!(prefix, 30);
        assert_eq!(gw, Ipv4Addr::new(172, 29, 0, 2));
    }

    #[test]
    fn parse_tun_cidr_invalid() {
        assert!(parse_tun_cidr("not.an.ip").is_err());
        assert!(parse_tun_cidr("172.29.0.1/33").is_err());
        assert!(parse_tun_cidr("172.29.0.1/abc").is_err());
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
