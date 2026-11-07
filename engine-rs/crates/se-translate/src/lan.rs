//! `.local` name resolution and the /24 subnet scan fallback (spec translate-llm.md §6.4, §6.5).
//!
//! The scan only opens and closes TCP connections: no byte (so no API key) is ever sent to a probed host, and
//! the translator skips it altogether when an API key is configured.

use std::io;
use std::net::{IpAddr, Ipv4Addr, SocketAddr, TcpStream, ToSocketAddrs, UdpSocket};
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use crate::pyurl;

/// The network operations `_resolve_lan_url` / `_discover_lan_server_ip` use (test seam: Python tests patch
/// `socket.getaddrinfo`, `socket.socket`, `socket.create_connection` and `_discover_lan_server_ip`).
pub trait LanNet: Send + Sync {
    /// `socket.getaddrinfo(host, port, AF_INET, SOCK_STREAM)`: the IPv4 addresses in resolver order.
    fn getaddrinfo_v4(&self, host: &str, port: u16) -> io::Result<Vec<Ipv4Addr>>;
    /// `_discover_lan_server_ip(port)`.
    fn discover_lan_server_ip(&self, port: u16) -> Option<String>;
}

/// The real network.
pub struct SystemLanNet;

impl LanNet for SystemLanNet {
    fn getaddrinfo_v4(&self, host: &str, port: u16) -> io::Result<Vec<Ipv4Addr>> {
        let addrs = (host, port).to_socket_addrs()?;
        Ok(addrs
            .filter_map(|a| match a {
                SocketAddr::V4(v4) => Some(*v4.ip()),
                SocketAddr::V6(_) => None,
            })
            .collect())
    }

    fn discover_lan_server_ip(&self, port: u16) -> Option<String> {
        discover_lan_server_ip_with(port, &udp_local_ip, &|ip, port| {
            TcpStream::connect_timeout(&SocketAddr::new(IpAddr::V4(ip), port), Duration::from_millis(350)).is_ok()
        })
    }
}

/// The local IPv4 address the default route uses (`connect(("8.8.8.8", 80))` on a UDP socket sends nothing).
pub fn udp_local_ip() -> io::Result<String> {
    let socket = UdpSocket::bind("0.0.0.0:0")?;
    socket.connect(("8.8.8.8", 80))?;
    Ok(socket.local_addr()?.ip().to_string())
}

/// `_discover_lan_server_ip(port)` with injectable seams: `local_ip()` (UDP trick) and `probe(ip, port)`
/// (TCP connect then close; `true` when the port is open). 64 workers probe `.1`-`.254` except the own
/// address; the lowest open last octet wins (Python `pool.map` yields in input order) after all probes ran.
pub fn discover_lan_server_ip_with(
    port: u16,
    local_ip: &dyn Fn() -> io::Result<String>,
    probe: &(dyn Fn(Ipv4Addr, u16) -> bool + Sync),
) -> Option<String> {
    let my_ip = local_ip().ok()?;
    let parts: Vec<&str> = my_ip.split('.').collect();
    let prefix = format!("{}.", parts.iter().take(3).copied().collect::<Vec<_>>().join("."));
    let candidates: Vec<(u32, String)> =
        (1..255u32).map(|n| (n, format!("{prefix}{n}"))).filter(|(_, ip)| *ip != my_ip).collect();
    let next = AtomicUsize::new(0);
    let found: Mutex<Vec<u32>> = Mutex::new(Vec::new());
    std::thread::scope(|scope| {
        for _ in 0..64 {
            scope.spawn(|| {
                loop {
                    let i = next.fetch_add(1, Ordering::SeqCst);
                    let Some((octet, ip)) = candidates.get(i) else { break };
                    // A prefix that is not an IPv4 (unexpected local address) probes nothing, like OSError.
                    if let Ok(addr) = ip.parse::<Ipv4Addr>() {
                        if probe(addr, port) {
                            found.lock().unwrap_or_else(|e| e.into_inner()).push(*octet);
                        }
                    }
                }
            });
        }
    });
    let found = found.into_inner().unwrap_or_else(|e| e.into_inner());
    found.into_iter().min().map(|o| format!("{prefix}{o}"))
}

/// `_resolve_lan_url(url, allow_scan)`: a `.local` host is replaced by its first IPv4 address (mDNS through
/// the system resolver), else, when `allow_scan`, by the subnet-scan result; the port is kept, userinfo dropped.
/// Anything else (or any failure) returns `url` unchanged.
pub fn resolve_lan_url_with(url: &str, allow_scan: bool, net: &dyn LanNet) -> String {
    let Ok(parts) = pyurl::urlsplit(url) else { return url.to_string() };
    let Some(host) = parts.hostname() else { return url.to_string() };
    let Ok(explicit_port) = parts.port() else { return url.to_string() };
    let port = match explicit_port {
        Some(p) if p != 0 => p,
        _ => {
            if parts.scheme == "https" {
                443
            } else {
                80
            }
        }
    };
    if !host.to_lowercase().ends_with(".local") {
        return url.to_string();
    }
    let with_ip = |ip: &str| {
        let netloc = match explicit_port {
            Some(p) if p != 0 => format!("{ip}:{p}"),
            _ => ip.to_string(),
        };
        pyurl::replace_netloc(&parts, &netloc)
    };
    if let Ok(infos) = net.getaddrinfo_v4(&host, port) {
        if let Some(first) = infos.first() {
            return with_ip(&first.to_string());
        }
    }
    let discovered = if allow_scan { net.discover_lan_server_ip(port) } else { None };
    match discovered {
        Some(ip) if !ip.is_empty() => with_ip(&ip),
        _ => url.to_string(),
    }
}

/// `_resolve_lan_url(url, allow_scan)` on the real network.
pub fn resolve_lan_url(url: &str, allow_scan: bool) -> String {
    resolve_lan_url_with(url, allow_scan, &SystemLanNet)
}

/// What the translator calls on every external request (Python tests replace `_resolve_lan_url`).
pub trait LanResolver {
    fn resolve(&self, url: &str, allow_scan: bool) -> String;
}

/// [`resolve_lan_url_with`] over a [`LanNet`].
pub struct NetResolver<N: LanNet>(pub N);

impl<N: LanNet> LanResolver for NetResolver<N> {
    fn resolve(&self, url: &str, allow_scan: bool) -> String {
        resolve_lan_url_with(url, allow_scan, &self.0)
    }
}

impl<F: Fn(&str, bool) -> String> LanResolver for F {
    fn resolve(&self, url: &str, allow_scan: bool) -> String {
        self(url, allow_scan)
    }
}
