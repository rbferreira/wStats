use super::model::{Net, NetIf, Wifi};
use crate::win::wide_to_string;
use std::collections::HashMap;
use std::net::{Ipv4Addr, Ipv6Addr};
use std::time::{Duration, Instant};
use windows::Win32::Foundation::HANDLE;
use windows::Win32::NetworkManagement::IpHelper::*;
use windows::Win32::NetworkManagement::WiFi::*;
use windows::Win32::Networking::WinSock::SOCKET_ADDRESS;
use windows::core::GUID;

const IF_TYPE_ETHERNET: u32 = 6;
const IF_TYPE_WIFI: u32 = 71;
const IF_TYPE_WWAN: [u32; 2] = [243, 244];
const OPER_UP: i32 = 1;
const HARDWARE_INTERFACE: u8 = 0b01;
const FILTER_INTERFACE: u8 = 0b10;
/// Addresses, gateways and Wi-Fi details change rarely; querying them every second is waste.
/// `wlan_intf_opcode_rssi`: lives in the private opcode range, so the crate has no constant.
const WLAN_INTF_OPCODE_RSSI: WLAN_INTF_OPCODE = WLAN_INTF_OPCODE(0x1000_0102);
const DETAILS_REFRESH: Duration = Duration::from_secs(5);

#[derive(Clone, Default)]
struct Details {
    ipv4: Vec<String>,
    ipv6: Vec<String>,
    gateway: Option<String>,
    dns: Vec<String>,
}

pub struct NetCollector {
    prev: HashMap<u64, (u64, u64)>,
    prev_at: Option<Instant>,
    session_up: u64,
    session_down: u64,
    details: HashMap<u64, Details>,
    wifi: HashMap<GUID, Wifi>,
    details_at: Option<Instant>,
    wlan: Option<HANDLE>,
}

unsafe impl Send for NetCollector {}

/// Bytes/second between two readings of a monotonically increasing counter. A counter
/// that went backwards (adapter reset, driver reload) counts as zero, not as a spike.
pub fn rate(prev: u64, cur: u64, secs: f64) -> f64 {
    if cur < prev || secs <= 0.0 { 0.0 } else { (cur - prev) as f64 / secs }
}

impl NetCollector {
    pub fn new() -> Self {
        Self {
            prev: HashMap::new(),
            prev_at: None,
            session_up: 0,
            session_down: 0,
            details: HashMap::new(),
            wifi: HashMap::new(),
            details_at: None,
            wlan: open_wlan(),
        }
    }

    pub fn sample(&mut self) -> Net {
        let now = Instant::now();
        let secs = self.prev_at.map(|t| (now - t).as_secs_f64()).unwrap_or(0.0);
        self.prev_at = Some(now);

        if self.details_at.is_none_or(|t| t.elapsed() >= DETAILS_REFRESH) {
            self.details = adapter_details();
            // The WLAN service may start after wStats, or restart: a handle that is
            // missing or no longer answers is opened again for the next round.
            self.wifi = match self.wlan.and_then(wifi_details) {
                Some(wifi) => wifi,
                None => {
                    if let Some(h) = self.wlan.take() {
                        unsafe { WlanCloseHandle(h, None) };
                    }
                    self.wlan = open_wlan();
                    HashMap::new()
                }
            };
            self.details_at = Some(now);
        }

        let mut table: *mut MIB_IF_TABLE2 = std::ptr::null_mut();
        if unsafe { GetIfTable2(&mut table) }.is_err() || table.is_null() {
            return Net::default();
        }
        let rows = unsafe { std::slice::from_raw_parts((*table).Table.as_ptr(), (*table).NumEntries as usize) };

        let mut out = Net::default();
        let mut seen = HashMap::new();
        for row in rows {
            let flags = row.InterfaceAndOperStatusFlags._bitfield;
            if flags & HARDWARE_INTERFACE == 0 || flags & FILTER_INTERFACE != 0 {
                continue;
            }
            let luid = unsafe { row.InterfaceLuid.Value };
            let (rx, tx) = (row.InOctets, row.OutOctets);
            seen.insert(luid, (rx, tx));
            let (down, up) = match self.prev.get(&luid) {
                Some(&(prx, ptx)) => (rate(prx, rx, secs), rate(ptx, tx, secs)),
                None => (0.0, 0.0),
            };
            out.down += down;
            out.up += up;
            self.session_down += (down * secs) as u64;
            self.session_up += (up * secs) as u64;

            let connected = row.OperStatus.0 == OPER_UP;
            if !connected {
                continue;
            }
            let d = self.details.get(&luid).cloned().unwrap_or_default();
            out.interfaces.push(NetIf {
                name: wide_to_string(&row.Alias),
                description: wide_to_string(&row.Description),
                kind: match row.Type {
                    IF_TYPE_WIFI => "wifi",
                    IF_TYPE_ETHERNET => "ethernet",
                    t if IF_TYPE_WWAN.contains(&t) => "cellular",
                    _ => "other",
                },
                connected,
                up,
                down,
                total_up: tx,
                total_down: rx,
                link_bps: row.ReceiveLinkSpeed.max(row.TransmitLinkSpeed),
                mac: row.PhysicalAddress[..row.PhysicalAddressLength.min(32) as usize]
                    .iter()
                    .map(|b| format!("{b:02X}"))
                    .collect::<Vec<_>>()
                    .join(":"),
                ipv4: d.ipv4,
                ipv6: d.ipv6,
                gateway: d.gateway,
                dns: d.dns,
                wifi: self.wifi.get(&row.InterfaceGuid).cloned(),
            });
        }
        unsafe { FreeMibTable(table.cast()) };
        self.prev = seen;

        // Busiest interface first: that is the one people care about.
        out.interfaces.sort_by(|a, b| {
            let has_ip = |i: &NetIf| !i.ipv4.is_empty() || !i.ipv6.is_empty();
            has_ip(b).cmp(&has_ip(a)).then((b.total_down + b.total_up).cmp(&(a.total_down + a.total_up)))
        });
        out.session_up = self.session_up;
        out.session_down = self.session_down;
        out
    }
}

impl Drop for NetCollector {
    fn drop(&mut self) {
        if let Some(h) = self.wlan {
            unsafe { WlanCloseHandle(h, None) };
        }
    }
}

fn sockaddr(a: &SOCKET_ADDRESS) -> Option<String> {
    if a.lpSockaddr.is_null() {
        return None;
    }
    // Read the raw sockaddr bytes rather than going through the IN_ADDR unions.
    let bytes = unsafe { std::slice::from_raw_parts(a.lpSockaddr.cast::<u8>(), a.iSockaddrLength as usize) };
    let family = u16::from_le_bytes([bytes[0], bytes[1]]);
    match family {
        2 if bytes.len() >= 8 => Some(Ipv4Addr::new(bytes[4], bytes[5], bytes[6], bytes[7]).to_string()),
        23 if bytes.len() >= 24 => {
            let mut octets = [0u8; 16];
            octets.copy_from_slice(&bytes[8..24]);
            Some(Ipv6Addr::from(octets).to_string())
        }
        _ => None,
    }
}

fn adapter_details() -> HashMap<u64, Details> {
    let mut map = HashMap::new();
    let flags = GAA_FLAG_INCLUDE_GATEWAYS | GAA_FLAG_SKIP_ANYCAST | GAA_FLAG_SKIP_MULTICAST;
    let mut size = 16 * 1024u32;
    let mut buf: Vec<u64>;
    loop {
        buf = vec![0u64; (size as usize).div_ceil(8)];
        let rc = unsafe { GetAdaptersAddresses(0, flags, None, Some(buf.as_mut_ptr().cast()), &mut size) };
        match rc {
            0 => break,
            111 => continue, // ERROR_BUFFER_OVERFLOW: `size` now holds what is needed
            _ => return map,
        }
    }
    let mut cur = buf.as_ptr().cast::<IP_ADAPTER_ADDRESSES_LH>();
    while !cur.is_null() {
        let a = unsafe { &*cur };
        let mut d = Details::default();
        let mut u = a.FirstUnicastAddress;
        while !u.is_null() {
            let ua = unsafe { &*u };
            if let Some(ip) = sockaddr(&ua.Address) {
                if ip.contains(':') { d.ipv6.push(ip) } else { d.ipv4.push(ip) }
            }
            u = ua.Next;
        }
        // Prefer the IPv4 gateway; IPv6 ones are usually link-local and say little.
        let mut g = a.FirstGatewayAddress;
        while !g.is_null() {
            let ga = unsafe { &*g };
            if let Some(ip) = sockaddr(&ga.Address) {
                if d.gateway.as_deref().is_none_or(|cur| cur.contains(':') && !ip.contains(':')) {
                    d.gateway = Some(ip);
                }
            }
            g = ga.Next;
        }
        let mut s = a.FirstDnsServerAddress;
        while !s.is_null() {
            let sa = unsafe { &*s };
            d.dns.extend(sockaddr(&sa.Address));
            s = sa.Next;
        }
        map.insert(unsafe { a.Luid.Value }, d);
        cur = a.Next;
    }
    map
}

fn phy_name(t: DOT11_PHY_TYPE) -> &'static str {
    [(dot11_phy_type_eht, "Wi-Fi 7"), (dot11_phy_type_he, "Wi-Fi 6"), (dot11_phy_type_vht, "Wi-Fi 5"), (dot11_phy_type_ht, "Wi-Fi 4")]
        .into_iter()
        .find_map(|(k, name)| (k == t).then_some(name))
        .unwrap_or("802.11")
}

fn open_wlan() -> Option<HANDLE> {
    let mut version = 0u32;
    let mut handle = HANDLE::default();
    (unsafe { WlanOpenHandle(2, None, &mut version, &mut handle) } == 0).then_some(handle)
}

/// `None` when the handle no longer works.
fn wifi_details(handle: HANDLE) -> Option<HashMap<GUID, Wifi>> {
    let mut map = HashMap::new();
    let mut list: *mut WLAN_INTERFACE_INFO_LIST = std::ptr::null_mut();
    if unsafe { WlanEnumInterfaces(handle, None, &mut list) } != 0 || list.is_null() {
        return None;
    }
    let ifaces = unsafe { std::slice::from_raw_parts((*list).InterfaceInfo.as_ptr(), (*list).dwNumberOfItems as usize) };
    for iface in ifaces {
        if iface.isState != wlan_interface_state_connected {
            continue;
        }
        let mut size = 0u32;
        let mut data: *mut core::ffi::c_void = std::ptr::null_mut();
        let rc = unsafe {
            WlanQueryInterface(handle, &iface.InterfaceGuid, wlan_intf_opcode_current_connection, None, &mut size, &mut data, None)
        };
        if rc != 0 || data.is_null() {
            // Every few seconds for as long as Windows refuses: say it once per code.
            static LAST: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
            if LAST.swap(rc, std::sync::atomic::Ordering::Relaxed) != rc {
                log::debug!("wlan: current_connection failed ({rc})");
            }
            continue;
        }
        let conn = unsafe { &*data.cast::<WLAN_CONNECTION_ATTRIBUTES>() };
        let assoc = &conn.wlanAssociationAttributes;
        let ssid_len = (assoc.dot11Ssid.uSSIDLength as usize).min(32);
        let mut wifi = Wifi {
            ssid: String::from_utf8_lossy(&assoc.dot11Ssid.ucSSID[..ssid_len]).into_owned(),
            bssid: assoc.dot11Bssid.iter().map(|b| format!("{b:02X}")).collect::<Vec<_>>().join(":"),
            signal: assoc.wlanSignalQuality,
            rx_mbps: assoc.ulRxRate as f32 / 1000.0,
            tx_mbps: assoc.ulTxRate as f32 / 1000.0,
            phy: phy_name(assoc.dot11PhyType).to_string(),
            channel: 0,
            rssi: None,
        };
        unsafe { WlanFreeMemory(data) };

        let mut data: *mut core::ffi::c_void = std::ptr::null_mut();
        if unsafe {
            WlanQueryInterface(handle, &iface.InterfaceGuid, wlan_intf_opcode_channel_number, None, &mut size, &mut data, None)
        } == 0
            && !data.is_null()
        {
            wifi.channel = unsafe { *data.cast::<u32>() };
            unsafe { WlanFreeMemory(data) };
        }
        let mut data: *mut core::ffi::c_void = std::ptr::null_mut();
        if unsafe { WlanQueryInterface(handle, &iface.InterfaceGuid, WLAN_INTF_OPCODE_RSSI, None, &mut size, &mut data, None) } == 0
            && !data.is_null()
        {
            wifi.rssi = Some(unsafe { *data.cast::<i32>() });
            unsafe { WlanFreeMemory(data) };
        }
        map.insert(iface.InterfaceGuid, wifi);
    }
    unsafe { WlanFreeMemory(list.cast()) };
    Some(map)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rate_per_second() {
        assert_eq!(rate(1_000, 3_000, 2.0), 1_000.0);
        assert_eq!(rate(5_000, 100, 1.0), 0.0, "counter reset is not a spike");
        assert_eq!(rate(0, 10, 0.0), 0.0);
    }

    #[test]
    fn live_table_reads() {
        let mut c = NetCollector::new();
        c.sample();
        let n = c.sample();
        assert!(n.up >= 0.0 && n.down >= 0.0);
    }
}
