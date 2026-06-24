//! LAN-quality ranking of the host's interface IPs for the mobile QR code.
//!
//! The phone scans the QR while sitting on the same physical LAN as the hub, so
//! the address it must receive is the hub's IP *on that LAN* — never a VPN tunnel
//! endpoint (e.g. NordLynx `10.5.0.2`), a Hyper-V/WSL host-only switch (`172.x`),
//! or an APIPA link-local. The OS enumerates interfaces in an arbitrary order
//! that routinely lists a VPN tunnel first; that raw order used to flow unranked
//! into the bind list → cert SANs → `exposed_interfaces`, so the frontend's
//! "first IPv4 TLS bind" pick (`buildRemoteAccessUrl`) landed on the tunnel and
//! the phone got `ERR_CONNECTION_ABORTED` against an address it can't route to.
//!
//! We rank by three signals, strongest first:
//!   1. **Default gateway present.** Only an interface with a real upstream
//!      gateway can carry the phone↔hub LAN. Idle tunnels, host-only Hyper-V
//!      switches, and APIPA links have none. This is the signal that survives
//!      whatever VPN/adapter the next machine has — unlike an IP-range allow-list,
//!      which can never tell a `10.x` LAN from a `10.x` VPN.
//!   2. **Physical media.** Among gateway-bearing interfaces a wired/wireless NIC
//!      beats a virtual/tunnel adapter — this demotes a *full-tunnel* VPN that
//!      does carry a gateway, keeping the physical LAN first.
//!   3. **RFC-1918 range.** Final tiebreak only: `192.168/16` > `10/8` > the rest,
//!      with IPv6 last (a bracketed `[…]` URL is a phone-browser dead-end). This
//!      mirrors the historical home/office-LAN heuristic and is the *sole* signal
//!      on non-Windows, where we don't query the route table.
//!
//! Ties beyond that preserve OS enumeration order (stable sort).

use std::collections::HashMap;
use std::net::IpAddr;

/// Per-interface routing signals, keyed by each of the interface's IPs. Built
/// from the OS route table on Windows; empty elsewhere (range heuristic only).
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct IfaceClass {
    /// The interface has at least one upstream default gateway configured.
    pub has_gateway: bool,
    /// The interface is wired/wireless physical media (not a tunnel/virtual NIC).
    pub is_physical: bool,
}

/// Rank the enumerated interface IPs best-LAN-first. Consults the OS route table
/// (Windows) to learn which interface actually owns a default gateway, then falls
/// back to the RFC-1918 range heuristic for ties / unclassified IPs.
pub fn rank_interface_ips(ips: Vec<IpAddr>) -> Vec<IpAddr> {
    // 0 or 1 IP is already ordered; skip the (potentially multi-second)
    // `GetAdaptersAddresses` classification when there's nothing to reorder.
    if ips.len() < 2 {
        return ips;
    }
    rank_with_classes(ips, &interface_classes())
}

/// Pure ranking core, split out so tests can inject a deterministic classifier
/// instead of the real `GetAdaptersAddresses` call.
fn rank_with_classes(mut ips: Vec<IpAddr>, classes: &HashMap<IpAddr, IfaceClass>) -> Vec<IpAddr> {
    // `sort_by_cached_key` computes each IP's key once (not on every comparison)
    // and is stable, so equally-ranked IPs keep OS enumeration order.
    ips.sort_by_cached_key(|ip| rank_key(ip, classes.get(ip)));
    ips
}

/// Lower tuple sorts earlier (= preferred). See the signals in the module docs;
/// each component is 0 for the better state so a missing classification
/// (gateway/physical unknown) degrades gracefully to the range heuristic.
///
/// Key order, strongest first:
///   1. `ipv6` — a phone can't use a bracketed IPv6 URL, so any IPv4 outranks
///      any IPv6 unconditionally.
///   2. `vbox` — VirtualBox's fixed host-only `192.168.56/24` is *never* a shared
///      LAN, so it is demoted above even the gateway signal (honouring the "never
///      a shared LAN even when it carries a gateway" guarantee). Other virtual
///      ranges (Docker/Hyper-V `172.16-31`, tunnels) are NOT hard-demoted here —
///      a real corporate LAN lives on `172.x`, so we trust the gateway signal and
///      only soft-demote them via `range_rank`.
///   3. `gateway` then `4. physical` — the OS route-table signals that actually
///      isolate the physical LAN from idle tunnels / host-only switches.
///   5. `range_rank` — the RFC-1918 tiebreak (and the whole ranking on non-Windows).
fn rank_key(ip: &IpAddr, class: Option<&IfaceClass>) -> (u8, u8, u8, u8, u8) {
    let ipv6 = if ip.is_ipv6() { 1 } else { 0 };
    let vbox = if is_vbox_host_only(ip) { 1 } else { 0 };
    let gateway = if class.is_some_and(|c| c.has_gateway) { 0 } else { 1 };
    let physical = if class.is_some_and(|c| c.is_physical) { 0 } else { 1 };
    (ipv6, vbox, gateway, physical, range_rank(ip))
}

/// VirtualBox's fixed host-only network `192.168.56.0/24` — never a real shared
/// LAN, hard-demoted in `rank_key` above the gateway signal.
fn is_vbox_host_only(ip: &IpAddr) -> bool {
    matches!(ip, IpAddr::V4(v4) if { let o = v4.octets(); o[0] == 192 && o[1] == 168 && o[2] == 56 })
}

/// RFC-1918 home/office-LAN preference, used only as the final tiebreak (and as
/// the whole ranking on non-Windows). `192.168` and all of `10/8` (incl. the
/// `10.0.0.x` consumer-router LANs) rank highest; Docker/Hyper-V `172.16-31` is
/// soft-demoted below generic addresses because it is usually a virtual bridge
/// (a real `172.x` LAN is rescued by its gateway on Windows). VirtualBox
/// `192.168.56` is handled by `is_vbox_host_only`/`rank_key`, and address-family
/// ordering by `rank_key`, so the IPv6 arm here is an inert same-family fallback.
fn range_rank(ip: &IpAddr) -> u8 {
    match ip {
        IpAddr::V4(v4) => {
            let o = v4.octets();
            if o[0] == 172 && (16..=31).contains(&o[1]) {
                3 // Docker/Hyper-V bridge — usually virtual, soft-demote below generic.
            } else if o[0] == 192 && o[1] == 168 {
                0 // Home/office LAN (192.168.56 already hard-demoted in rank_key).
            } else if o[0] == 10 {
                1 // All of 10/8, including 10.0.0.x consumer-router LANs.
            } else {
                2 // CGNAT / public / other.
            }
        }
        IpAddr::V6(_) => 4,
    }
}

/// Build the per-IP routing classification from the OS. Windows reads the live
/// adapter/gateway table; every other platform returns an empty map so ranking
/// falls back to the range heuristic (the VPN-adapter trap is Windows-specific).
#[cfg(not(target_os = "windows"))]
fn interface_classes() -> HashMap<IpAddr, IfaceClass> {
    HashMap::new()
}

#[cfg(target_os = "windows")]
fn interface_classes() -> HashMap<IpAddr, IfaceClass> {
    windows_impl::interface_classes().unwrap_or_else(|e| {
        tracing::warn!("interface gateway classification failed: {e}; ranking by range only");
        HashMap::new()
    })
}

/// Inline `extern "system"` FFI for `GetAdaptersAddresses`, matching the
/// no-`windows-sys`/`winapi` convention used by `sandbox::restricted_token`.
#[cfg(target_os = "windows")]
mod windows_impl {
    // Type/field names mirror the Win32 headers verbatim (as in `sandbox::*`'s
    // inline FFI), so the SCREAMING_CASE aliases are intentional.
    #![allow(non_snake_case, non_camel_case_types, clippy::upper_case_acronyms)]

    use super::IfaceClass;
    use std::collections::HashMap;
    use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
    use std::os::raw::c_void;

    type DWORD = u32;
    type ULONG = u32;

    // GetAdaptersAddresses return codes (winerror.h).
    const ERROR_SUCCESS: DWORD = 0;
    const ERROR_NO_DATA: DWORD = 232;
    const ERROR_BUFFER_OVERFLOW: DWORD = 111;

    /// Microsoft's recommended starting buffer for `GetAdaptersAddresses` so the
    /// common case needs a single call instead of a size-probe + fetch.
    const INITIAL_BUFFER_BYTES: usize = 15 * 1024;

    // Address families (ws2def.h).
    const AF_UNSPEC: ULONG = 0;
    const AF_INET: u16 = 2;
    const AF_INET6: u16 = 23;

    // GetAdaptersAddresses flags (iptypes.h): include gateways, skip the address
    // families we don't read so the returned buffer stays small.
    const GAA_FLAG_SKIP_ANYCAST: ULONG = 0x0002;
    const GAA_FLAG_SKIP_MULTICAST: ULONG = 0x0004;
    const GAA_FLAG_SKIP_DNS_SERVER: ULONG = 0x0008;
    const GAA_FLAG_INCLUDE_GATEWAYS: ULONG = 0x0080;

    // IfOperStatus / IfType values we care about (ifdef.h, ipifcons.h).
    const IF_OPER_STATUS_UP: i32 = 1;
    const IF_TYPE_ETHERNET_CSMACD: DWORD = 6;
    const IF_TYPE_IEEE80211: DWORD = 71;

    /// `SOCKET_ADDRESS` (ws2def.h): a pointer to a `SOCKADDR` plus its length.
    #[repr(C)]
    struct SOCKET_ADDRESS {
        lpSockaddr: *mut c_void,
        iSockaddrLength: i32,
    }

    /// `IP_ADAPTER_UNICAST_ADDRESS_LH` — only the head fields up to `Address` are
    /// laid out; the OS allocates the full struct, we read the prefix.
    #[repr(C)]
    struct IP_ADAPTER_UNICAST_ADDRESS_LH {
        Length: ULONG,
        Flags: DWORD,
        Next: *mut IP_ADAPTER_UNICAST_ADDRESS_LH,
        Address: SOCKET_ADDRESS,
        // ...trailing fields unused.
    }

    /// `IP_ADAPTER_ADDRESSES_LH` (iptypes.h, x64 layout). Every field up to
    /// `FirstGatewayAddress` must be present and correctly typed so the offsets
    /// match what the OS wrote — getting this wrong is UB, so it mirrors the
    /// header exactly through that field. Trailing fields are omitted (we never
    /// read past `FirstGatewayAddress`).
    #[repr(C)]
    struct IP_ADAPTER_ADDRESSES_LH {
        Length: ULONG,
        IfIndex: DWORD,
        Next: *mut IP_ADAPTER_ADDRESSES_LH,
        AdapterName: *mut std::os::raw::c_char,
        FirstUnicastAddress: *mut IP_ADAPTER_UNICAST_ADDRESS_LH,
        FirstAnycastAddress: *mut c_void,
        FirstMulticastAddress: *mut c_void,
        FirstDnsServerAddress: *mut c_void,
        DnsSuffix: *mut u16,
        Description: *mut u16,
        FriendlyName: *mut u16,
        PhysicalAddress: [u8; 8],
        PhysicalAddressLength: ULONG,
        Flags: ULONG,
        Mtu: ULONG,
        IfType: DWORD,
        OperStatus: i32,
        Ipv6IfIndex: DWORD,
        ZoneIndices: [ULONG; 16],
        FirstPrefix: *mut c_void,
        TransmitLinkSpeed: u64,
        ReceiveLinkSpeed: u64,
        FirstWinsServerAddress: *mut c_void,
        FirstGatewayAddress: *mut c_void,
        // ...trailing fields unused.
    }

    #[link(name = "iphlpapi")]
    extern "system" {
        fn GetAdaptersAddresses(
            family: ULONG,
            flags: ULONG,
            reserved: *mut c_void,
            adapter_addresses: *mut IP_ADAPTER_ADDRESSES_LH,
            size_pointer: *mut ULONG,
        ) -> ULONG;
    }

    /// Parse a `SOCKADDR*` into an `IpAddr`. Reads the family discriminant, then
    /// the v4 (`sin_addr` at +4) or v6 (`sin6_addr` at +8) address bytes.
    ///
    /// SAFETY: `sockaddr` must point to a valid `SOCKADDR` of at least the length
    /// implied by its family, as produced by `GetAdaptersAddresses`.
    unsafe fn sockaddr_to_ip(sockaddr: *const c_void) -> Option<IpAddr> {
        if sockaddr.is_null() {
            return None;
        }
        let base = sockaddr as *const u8;
        let family = (base as *const u16).read_unaligned();
        match family {
            AF_INET => {
                let mut octets = [0u8; 4];
                std::ptr::copy_nonoverlapping(base.add(4), octets.as_mut_ptr(), 4);
                Some(IpAddr::V4(Ipv4Addr::from(octets)))
            }
            AF_INET6 => {
                let mut octets = [0u8; 16];
                std::ptr::copy_nonoverlapping(base.add(8), octets.as_mut_ptr(), 16);
                Some(IpAddr::V6(Ipv6Addr::from(octets)))
            }
            _ => None,
        }
    }

    /// Query the adapter table and classify every unicast IP by whether its
    /// interface is operational with a default gateway and is physical media.
    pub(super) fn interface_classes() -> Result<HashMap<IpAddr, IfaceClass>, String> {
        let flags = GAA_FLAG_SKIP_ANYCAST
            | GAA_FLAG_SKIP_MULTICAST
            | GAA_FLAG_SKIP_DNS_SERVER
            | GAA_FLAG_INCLUDE_GATEWAYS;

        // Back the adapter table with a `Vec<u64>` (not `Vec<u8>`): the struct
        // requires 8-byte alignment and `u64`'s allocation guarantees it, whereas a
        // `Vec<u8>` only promises 1-byte alignment, so casting it to the struct and
        // doing aligned field reads would rely on an allocator detail the type
        // system does not. We size in whole `u64` words (round up).
        let words = |bytes: usize| bytes.div_ceil(8);
        let mut buf: Vec<u64> = vec![0u64; words(INITIAL_BUFFER_BYTES)];
        // Pre-size to the Microsoft-recommended 15 KB so the common case is one
        // call; on overflow the OS writes the required byte count into `size` and
        // we re-allocate. Retry a few times in case the table grows mid-call.
        let mut size: ULONG = (buf.len() * 8) as ULONG;
        let mut last_ret = ERROR_BUFFER_OVERFLOW;
        for _ in 0..4 {
            let ptr = buf.as_mut_ptr() as *mut IP_ADAPTER_ADDRESSES_LH;
            // SAFETY: `ptr`/`size` describe `buf` (8-byte aligned, `size` bytes);
            // on ERROR_BUFFER_OVERFLOW the OS writes the required byte count back
            // into `size` and writes nothing into the buffer.
            let ret = unsafe { GetAdaptersAddresses(AF_UNSPEC, flags, std::ptr::null_mut(), ptr, &mut size) };
            last_ret = ret;
            if ret == ERROR_SUCCESS {
                break;
            }
            if ret == ERROR_NO_DATA {
                return Ok(HashMap::new()); // No adapters with addresses.
            }
            if ret == ERROR_BUFFER_OVERFLOW {
                buf = vec![0u64; words(size as usize)];
                continue;
            }
            return Err(format!("GetAdaptersAddresses failed: {ret}"));
        }
        if last_ret != ERROR_SUCCESS {
            return Err(format!("GetAdaptersAddresses still overflowing after retries: {last_ret}"));
        }

        let mut classes = HashMap::new();
        let mut adapter = buf.as_ptr() as *const IP_ADAPTER_ADDRESSES_LH;
        // SAFETY: the linked list lives inside `buf`, which outlives this walk.
        // Each `Next`/`FirstUnicastAddress`/`lpSockaddr` pointer was written by the
        // OS into that buffer (or points to it); we only read, never free.
        unsafe {
            while !adapter.is_null() {
                let a = &*adapter;
                let up = a.OperStatus == IF_OPER_STATUS_UP;
                let has_gateway = up && !a.FirstGatewayAddress.is_null();
                let is_physical = a.IfType == IF_TYPE_ETHERNET_CSMACD || a.IfType == IF_TYPE_IEEE80211;
                let class = IfaceClass { has_gateway, is_physical };

                let mut unicast = a.FirstUnicastAddress;
                while !unicast.is_null() {
                    let u = &*unicast;
                    if let Some(ip) = sockaddr_to_ip(u.Address.lpSockaddr) {
                        classes.insert(ip, class);
                    }
                    unicast = u.Next;
                }
                adapter = a.Next;
            }
        }
        Ok(classes)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::str::FromStr;

    fn ip(s: &str) -> IpAddr {
        IpAddr::from_str(s).unwrap()
    }

    fn classed(pairs: &[(&str, bool, bool)]) -> HashMap<IpAddr, IfaceClass> {
        pairs
            .iter()
            .map(|(addr, has_gateway, is_physical)| {
                (
                    ip(addr),
                    IfaceClass {
                        has_gateway: *has_gateway,
                        is_physical: *is_physical,
                    },
                )
            })
            .collect()
    }

    /// The headline regression (the reported bug): the OS lists the NordLynx VPN
    /// tunnel (`10.5.0.2`) BEFORE the real Ethernet (`192.168.1.10`). Only the
    /// Ethernet owns a default gateway, so it must rank first and become the QR's
    /// first IPv4 TLS bind — even though the VPN appears earlier in enumeration.
    #[test]
    fn gatewayed_lan_beats_earlier_vpn_tunnel() {
        let enumerated = vec![
            ip("10.5.0.2"),     // NordLynx tunnel — no gateway
            ip("192.168.1.10"), // Ethernet — real LAN, owns the gateway
            ip("172.29.160.1"), // Hyper-V Default Switch — host-only, no gateway
            ip("172.23.16.1"),  // Hyper-V WSL switch — host-only, no gateway
        ];
        let classes = classed(&[
            ("10.5.0.2", false, false),
            ("192.168.1.10", true, true),
            ("172.29.160.1", false, true),
            ("172.23.16.1", false, true),
        ]);
        let ranked = rank_with_classes(enumerated, &classes);
        assert_eq!(ranked.first(), Some(&ip("192.168.1.10")));
    }

    /// The "once and for all" property the range heuristic alone cannot give: a
    /// real LAN on `10.x` behind a VPN on `192.168.x`. Range would pick the
    /// `192.168` VPN; the gateway signal correctly keeps the `10.x` LAN first.
    #[test]
    fn gatewayed_10_lan_beats_192_168_vpn() {
        let enumerated = vec![ip("192.168.9.20"), ip("10.1.2.3")];
        let classes = classed(&[
            ("192.168.9.20", false, false), // VPN tunnel — no gateway
            ("10.1.2.3", true, true),       // physical LAN — owns the gateway
        ]);
        let ranked = rank_with_classes(enumerated, &classes);
        assert_eq!(ranked.first(), Some(&ip("10.1.2.3")));
    }

    /// Among two gateway-bearing interfaces (a full-tunnel VPN that carries a
    /// gateway vs. the physical NIC), physical media wins.
    #[test]
    fn physical_beats_virtual_when_both_have_gateway() {
        let enumerated = vec![ip("10.8.0.2"), ip("192.168.1.10")];
        let classes = classed(&[
            ("10.8.0.2", true, false),    // full-tunnel VPN: gateway, but virtual
            ("192.168.1.10", true, true), // physical NIC: gateway + physical
        ]);
        let ranked = rank_with_classes(enumerated, &classes);
        assert_eq!(ranked.first(), Some(&ip("192.168.1.10")));
    }

    /// Non-Windows / classification-unavailable fallback: with no class data the
    /// RFC-1918 range heuristic decides, reproducing the historical behaviour
    /// (192.168 > 10.x) regardless of enumeration order.
    #[test]
    fn falls_back_to_range_heuristic_without_classes() {
        let enumerated = vec![ip("10.5.0.2"), ip("192.168.1.10"), ip("172.23.16.1")];
        let ranked = rank_with_classes(enumerated, &HashMap::new());
        assert_eq!(ranked, vec![ip("192.168.1.10"), ip("10.5.0.2"), ip("172.23.16.1")]);
    }

    /// Equal-ranked IPs keep OS enumeration order (stable sort) — two plain
    /// 192.168 addresses with no class data must not be reordered.
    #[test]
    fn stable_for_equal_rank() {
        let enumerated = vec![ip("192.168.1.50"), ip("192.168.1.10")];
        let ranked = rank_with_classes(enumerated.clone(), &HashMap::new());
        assert_eq!(ranked, enumerated);
    }

    /// IPv4 always sorts ahead of IPv6 (a bracketed IPv6 URL is a phone dead-end),
    /// even when the IPv6 interface owns a gateway and the IPv4 one does not.
    #[test]
    fn ipv4_preferred_over_ipv6() {
        let enumerated = vec![ip("fe80::1"), ip("192.168.1.10")];
        let classes = classed(&[("fe80::1", true, true), ("192.168.1.10", false, false)]);
        let ranked = rank_with_classes(enumerated, &classes);
        assert_eq!(ranked.first(), Some(&ip("192.168.1.10")));
    }

    /// In the range-only fallback, a real `10.0.0.x` consumer-router LAN must NOT
    /// be demoted (it is a real LAN, not a tunnel), while a Docker/Hyper-V
    /// `172.16-31` bridge IS soft-demoted below it.
    #[test]
    fn range_fallback_keeps_10_0_0_x_above_docker_172() {
        let enumerated = vec![ip("172.20.0.1"), ip("10.0.0.5")];
        let ranked = rank_with_classes(enumerated, &HashMap::new());
        assert_eq!(ranked, vec![ip("10.0.0.5"), ip("172.20.0.1")]);
    }

    /// VirtualBox host-only `192.168.56.x` is never a shared LAN, so it must rank
    /// below a real LAN even when the host-only adapter (mis)reports a gateway and
    /// physical media — the demotion outranks the gateway signal.
    #[test]
    fn vbox_host_only_demoted_below_real_lan_even_with_gateway() {
        let enumerated = vec![ip("192.168.56.1"), ip("192.168.1.10")];
        let classes = classed(&[
            ("192.168.56.1", true, true), // host-only adapter pretending to be a real NIC
            ("192.168.1.10", false, false),
        ]);
        let ranked = rank_with_classes(enumerated, &classes);
        assert_eq!(ranked.first(), Some(&ip("192.168.1.10")));
    }
}
