//! The jail's network: internet only, nothing local.
//!
//! The guest's network card is backed by passt, a user-space network stack
//! that turns the guest's traffic into ordinary sockets of the passt process.
//! The host therefore needs no bridge, no forwarding and no NAT, and every
//! packet the jail sends leaves the host as a packet of one dedicated account.
//! An nftables output rule on that account drops everything bound for the
//! host itself, the LAN and other non-internet ranges, and turning the
//! internet off is one more rule.

use std::fmt::Write as _;
use std::net::IpAddr;
use std::path::PathBuf;

/// The system account passt runs as; only its traffic is filtered.
pub const NET_USER: &str = "gamejail-net";
pub const TABLE: &str = "gamejail";
pub const PASST: &str = "passt";

/// Public resolvers handed to the guest, so DNS never touches the host's
/// resolver (which listens on loopback and is blocked like the rest).
pub const DNS: [&str; 2] = ["1.1.1.1", "9.9.9.9"];

/// Destinations that are not the internet: this host, the LAN, carrier NAT,
/// link-local, documentation, benchmarking, multicast and reserved space.
const BLOCKED_V4: &[&str] = &[
    "0.0.0.0/8",
    "10.0.0.0/8",
    "100.64.0.0/10",
    "127.0.0.0/8",
    "169.254.0.0/16",
    "172.16.0.0/12",
    "192.0.0.0/24",
    "192.0.2.0/24",
    "192.168.0.0/16",
    "198.18.0.0/15",
    "198.51.100.0/24",
    "203.0.113.0/24",
    "224.0.0.0/4",
    "240.0.0.0/4",
];
const BLOCKED_V6: &[&str] = &[
    "::1/128",
    "::ffff:0:0/96",
    "fc00::/7",
    "fe80::/10",
    "ff00::/8",
];

/// The nftables script for the jail network, applied atomically with
/// `nft -f -`. It replaces any earlier version of the table.
#[must_use]
pub fn ruleset(net_uid: u32, internet: bool) -> String {
    let mut out = String::new();
    let _ = writeln!(out, "table inet {TABLE}");
    let _ = writeln!(out, "delete table inet {TABLE}");
    let _ = writeln!(out, "table inet {TABLE} {{");
    let _ = writeln!(out, "  chain output {{");
    let _ = writeln!(
        out,
        "    type filter hook output priority filter; policy accept;"
    );
    let _ = writeln!(out, "    meta skuid != {net_uid} accept");
    // Replies on connections this host opened into passt, such as the
    // development SSH forward. The jail cannot open such a connection itself:
    // its own connections leave as new ones and meet the rules below.
    let _ = writeln!(
        out,
        "    fib daddr type local ct direction reply ct state established,related accept"
    );
    if !internet {
        let _ = writeln!(out, "    counter drop comment \"internet off\"");
    }
    // Any address of this host, whatever range it is in.
    let _ = writeln!(
        out,
        "    fib daddr type {{ local, broadcast, multicast, anycast }} counter drop"
    );
    let _ = writeln!(
        out,
        "    ip daddr {{ {} }} counter drop",
        BLOCKED_V4.join(", ")
    );
    let _ = writeln!(
        out,
        "    ip6 daddr {{ {} }} counter drop",
        BLOCKED_V6.join(", ")
    );
    let _ = writeln!(out, "    counter accept");
    let _ = writeln!(out, "  }}");
    let _ = writeln!(out, "}}");
    out
}

/// Removes the table; succeeds when it is already gone.
#[must_use]
pub fn remove_ruleset() -> String {
    format!("table inet {TABLE}\ndelete table inet {TABLE}\n")
}

/// A passt instance serving one jail.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Passt {
    pub socket: PathBuf,
    pub net_uid: u32,
    /// Host `127.0.0.1:<port>` forwarded to the guest's SSH port, for
    /// development.
    pub dev_ssh_port: Option<u16>,
}

impl Passt {
    /// The command line, program first. passt stays in the foreground,
    /// serves exactly one guest and exits with it, uses IPv4 only, and does
    /// not map the gateway address to the host.
    #[must_use]
    pub fn command_line(&self) -> Vec<String> {
        let mut args: Vec<String> = [
            PASST,
            "--foreground",
            "--one-off",
            "--ipv4-only",
            "--no-map-gw",
        ]
        .map(String::from)
        .to_vec();
        args.extend(["--runas".into(), self.net_uid.to_string()]);
        args.extend(["--socket".into(), self.socket.display().to_string()]);
        for dns in DNS {
            args.extend(["--dns".into(), dns.into()]);
        }
        let tcp = self
            .dev_ssh_port
            .map_or_else(|| "none".into(), |port| format!("127.0.0.1/{port}:22"));
        args.extend([
            "--tcp-ports".into(),
            tcp,
            "--udp-ports".into(),
            "none".into(),
        ]);
        args
    }
}

/// Whether `addr` is one the jail may reach, by the same ranges the ruleset
/// blocks. Addresses of the host itself are only known to the kernel, so a
/// public address of this host still reads as allowed here.
#[must_use]
pub fn is_internet(addr: IpAddr) -> bool {
    let blocked = match addr {
        IpAddr::V4(_) => BLOCKED_V4,
        IpAddr::V6(_) => BLOCKED_V6,
    };
    !blocked.iter().any(|cidr| contains(cidr, addr))
}

fn contains(cidr: &str, addr: IpAddr) -> bool {
    let Some((net, len)) = cidr.split_once('/') else {
        return false;
    };
    let (Ok(net), Ok(len)) = (net.parse::<IpAddr>(), len.parse::<u32>()) else {
        return false;
    };
    match (net, addr) {
        (IpAddr::V4(net), IpAddr::V4(addr)) => {
            let mask = u32::MAX.checked_shl(32 - len).unwrap_or(0);
            u32::from(net) & mask == u32::from(addr) & mask
        }
        (IpAddr::V6(net), IpAddr::V6(addr)) => {
            let mask = u128::MAX.checked_shl(128 - len).unwrap_or(0);
            u128::from(net) & mask == u128::from(addr) & mask
        }
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_jail_account_is_filtered() {
        let rules = ruleset(978, true);
        assert!(rules.contains("meta skuid != 978 accept"));
        assert!(
            rules.contains("fib daddr type { local, broadcast, multicast, anycast } counter drop")
        );
        assert!(!rules.contains("internet off"));
        assert!(rules.starts_with("table inet gamejail\ndelete table inet gamejail\n"));
    }

    #[test]
    fn internet_off_drops_everything_from_the_jail() {
        let rules = ruleset(978, false);
        let off = rules.find("internet off").unwrap();
        assert!(off > rules.find("skuid != 978").unwrap());
        assert!(off < rules.find("fib daddr type {").unwrap());
    }

    #[test]
    fn only_replies_to_host_initiated_connections_reach_the_host() {
        let rules = ruleset(978, false);
        let replies = rules
            .find("fib daddr type local ct direction reply ct state established,related accept")
            .unwrap();
        assert!(replies < rules.find("internet off").unwrap());
        assert!(replies < rules.find("fib daddr type {").unwrap());
    }

    #[test]
    fn lan_host_and_reserved_ranges_are_not_internet() {
        for blocked in [
            "192.168.88.1",
            "10.0.2.2",
            "127.0.0.53",
            "172.20.0.1",
            "100.64.1.1",
            "169.254.1.1",
            "224.0.0.251",
            "::1",
            "fe80::1",
            "fd00::1",
        ] {
            assert!(
                !is_internet(blocked.parse().unwrap()),
                "{blocked} must be blocked"
            );
        }
        for allowed in [
            "1.1.1.1",
            "9.9.9.9",
            "93.184.215.14",
            "2606:4700:4700::1111",
        ] {
            assert!(
                is_internet(allowed.parse().unwrap()),
                "{allowed} must be allowed"
            );
        }
    }

    #[test]
    fn passt_serves_one_guest_without_inbound_ports() {
        let passt = Passt {
            socket: "/run/gamejail/proto.passt".into(),
            net_uid: 978,
            dev_ssh_port: None,
        };
        let args = passt.command_line();
        assert_eq!(args[0], PASST);
        for flag in ["--one-off", "--no-map-gw", "--ipv4-only", "--foreground"] {
            assert!(args.contains(&flag.to_owned()), "missing {flag}");
        }
        let after = |flag: &str| {
            args.windows(2)
                .filter(|w| w[0] == flag)
                .map(|w| w[1].clone())
                .collect::<Vec<_>>()
        };
        assert_eq!(after("--runas"), ["978"]);
        assert_eq!(after("--tcp-ports"), ["none"]);
        assert_eq!(after("--udp-ports"), ["none"]);
        assert_eq!(after("--dns"), ["1.1.1.1", "9.9.9.9"]);
    }

    #[test]
    fn development_ssh_is_forwarded_from_loopback_only() {
        let passt = Passt {
            socket: "/s".into(),
            net_uid: 978,
            dev_ssh_port: Some(2222),
        };
        let args = passt.command_line();
        let at = args.iter().position(|a| a == "--tcp-ports").unwrap();
        assert_eq!(args[at + 1], "127.0.0.1/2222:22");
    }
}
