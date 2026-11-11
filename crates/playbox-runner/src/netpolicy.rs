//! What a sandboxed guest may reach on the network, as a level and a nftables rule set.
//!
//! The levels and the always-rejected ranges follow the approach of Peerspan's runtime (`crates/runtime/src/net.rs`,
//! Apache-2.0): a guest's packets leave through a host process (passt) that opens ordinary sockets from the host's network
//! namespace, so without a filter the guest can reach anything the host can, including loopback, the LAN and the cloud
//! metadata service. The filter sits on the host: the per-run cgroup of that process is matched by nftables, every run gets a
//! chain of its own, and traffic from a runner cgroup with no chain is rejected, so a missing chain fails closed.
//!
//! This module only decides and renders. It opens no sockets and runs no commands, so the classification and the rule text are
//! tested on any platform; applying the rules is the Linux runner's job.

use anyhow::{bail, Result};
use std::fmt::Write as _;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};

/// What a run's guest may reach. Ordered from least to most, so a request can be compared with the host owner's maximum.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Default)]
pub enum Level {
    /// No network device: the guest has only loopback.
    #[default]
    None,
    /// Public addresses only.
    Internet,
    /// Public addresses and private ranges (a LAN registry, say). Loopback, link-local and metadata stay unreachable.
    InternetLan,
}

impl std::str::FromStr for Level {
    type Err = anyhow::Error;
    fn from_str(s: &str) -> Result<Self> {
        match s {
            "none" => Ok(Self::None),
            "internet" => Ok(Self::Internet),
            "internet+lan" => Ok(Self::InternetLan),
            other => bail!("unknown network level {other:?} (none, internet, internet+lan)"),
        }
    }
}

impl std::fmt::Display for Level {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::None => "none",
            Self::Internet => "internet",
            Self::InternetLan => "internet+lan",
        })
    }
}

/// An IPv4-mapped address (`::ffff:a.b.c.d`). A guest has no reason to send one on the wire, and it reaches its IPv4 target
/// through the host's IPv4 stack, so the whole range is rejected.
fn mapped(ip: Ipv6Addr) -> bool {
    let s = ip.segments();
    s[..5] == [0, 0, 0, 0, 0] && s[5] == 0xffff
}

/// An address behind the well-known NAT64 prefix (`64:ff9b::/96`) reaches its IPv4 target through a NAT64 gateway, so it is
/// judged as that IPv4 address.
fn embedded_v4(ip: Ipv6Addr) -> Option<Ipv4Addr> {
    let s = ip.segments();
    (s[0] == 0x64 && s[1] == 0xff9b && s[2..6] == [0, 0, 0, 0]).then(|| {
        let o = ip.octets();
        Ipv4Addr::new(o[12], o[13], o[14], o[15])
    })
}

/// The local-use NAT64 prefix (`64:ff9b:1::/48`) may translate to any IPv4 address, private ones included.
fn local_nat64(ip: Ipv6Addr) -> bool {
    let s = ip.segments();
    s[0] == 0x64 && s[1] == 0xff9b && s[2] == 1
}

/// Never reachable at any level: unspecified, loopback, link-local (the cloud metadata service lives at 169.254.169.254),
/// multicast, reserved and broadcast, and IPv6 forms that reach those through IPv4.
#[must_use]
pub fn always_rejected(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(a) => {
            let o = a.octets();
            o[0] == 0 || a.is_loopback() || a.is_link_local() || a.is_multicast() || o[0] >= 240
        }
        IpAddr::V6(a) => {
            if mapped(a) {
                return true;
            }
            if let Some(v4) = embedded_v4(a) {
                return always_rejected(IpAddr::V4(v4));
            }
            let s = a.segments();
            a.is_unspecified()
                || a.is_loopback()
                || (s[0] & 0xffc0) == 0xfe80 // fe80::/10 link-local
                || (s[0] & 0xff00) == 0xff00 // ff00::/8 multicast
        }
    }
}

/// Private ranges: RFC 1918, shared address space (CGNAT), IPv6 unique-local, and what NAT64 may translate to them.
#[must_use]
pub fn private(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(a) => {
            let o = a.octets();
            a.is_private() || (o[0] == 100 && (o[1] & 0xc0) == 64)
        }
        IpAddr::V6(a) => {
            if let Some(v4) = embedded_v4(a) {
                return private(IpAddr::V4(v4));
            }
            (a.segments()[0] & 0xfe00) == 0xfc00 || local_nat64(a)
        }
    }
}

/// Whether a guest at `level` may open a connection to `ip` when no destination list narrows it further.
#[must_use]
pub fn allowed(level: Level, ip: IpAddr) -> bool {
    match level {
        Level::None => false,
        Level::Internet => !always_rejected(ip) && !private(ip),
        Level::InternetLan => !always_rejected(ip),
    }
}

const REJECT: &str = "reject with icmpx admin-prohibited";
const V4_ALWAYS: &str = "{ 0.0.0.0/8, 127.0.0.0/8, 169.254.0.0/16, 224.0.0.0/4, 240.0.0.0/4 }";
const V6_ALWAYS: &str = "{ ::/127, fe80::/10, ff00::/8, ::ffff:0:0/96, 64:ff9b::/104, 64:ff9b::7f00:0/104, \
    64:ff9b::a9fe:0/112, 64:ff9b::e000:0/100, 64:ff9b::f000:0/100 }";
const V4_PRIVATE: &str = "{ 10.0.0.0/8, 100.64.0.0/10, 172.16.0.0/12, 192.168.0.0/16 }";
const V6_PRIVATE: &str = "{ fc00::/7, 64:ff9b:1::/48, 64:ff9b::a00:0/104, 64:ff9b::6440:0/106, 64:ff9b::ac10:0/108, \
    64:ff9b::c0a8:0/112 }";

/// A run id that is safe to put in a rule: lowercase hex, 8 to 64 characters.
fn valid_id(id: &str) -> bool {
    (8..=64).contains(&id.len()) && id.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// The nftables table every run shares. Loading it is idempotent. Traffic from any cgroup below `cgroup_root` is rejected
/// unless a run's own chain accepts it, so a run whose chain was never installed has no network.
///
/// # Errors
///
/// Returns an error if `cgroup_root` is not a plain relative path such as `playbox/runs`.
pub fn base_ruleset(cgroup_root: &str) -> Result<String> {
    let depth = cgroup_depth(cgroup_root)?;
    Ok(format!(
        "table inet playbox_runner {{\n  chain output {{\n    type filter hook output priority filter; policy accept;\n    \
         socket cgroupv2 level {depth} \"{cgroup_root}\" jump runs\n  }}\n  chain runs {{\n    {REJECT}\n  }}\n}}\n"
    ))
}

fn cgroup_depth(path: &str) -> Result<usize> {
    let ok = !path.is_empty()
        && !path.starts_with('/')
        && path
            .split('/')
            .all(|c| !c.is_empty() && c != "." && c != ".." && c.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.')));
    if !ok {
        bail!("bad cgroup path {path:?}");
    }
    Ok(path.split('/').count())
}

/// The rules for one run, as `nft -f` input that adds a chain and routes the run's cgroup to it.
///
/// With `dests` empty the level decides. With a list, the guest may reach exactly those addresses and ports and nothing else
/// (names are resolved once, by the caller, and pinned here, so a later DNS change cannot move a destination onto a private
/// address). An address that is always rejected cannot be listed: that is an error, not a silent drop.
///
/// # Errors
///
/// Returns an error for a bad run id or cgroup path, a destination the level forbids, or `Level::None` with destinations.
pub fn run_chain(cgroup_root: &str, run_id: &str, level: Level, dests: &[SocketAddr]) -> Result<String> {
    if !valid_id(run_id) {
        bail!("bad run id {run_id:?}");
    }
    let depth = cgroup_depth(cgroup_root)? + 1;
    let chain = format!("run_{run_id}");
    let mut out = String::new();
    let _ = writeln!(out, "add chain inet playbox_runner {chain}");
    let _ = writeln!(
        out,
        "insert rule inet playbox_runner runs socket cgroupv2 level {depth} \"{cgroup_root}/{run_id}\" jump {chain}"
    );
    let add = |out: &mut String, rule: &str| {
        let _ = writeln!(out, "add rule inet playbox_runner {chain} {rule}");
    };
    if level == Level::None {
        if !dests.is_empty() {
            bail!("a run with no network cannot list destinations");
        }
        add(&mut out, REJECT);
        return Ok(out);
    }
    // Stateless protections first: they apply whatever else is allowed.
    add(&mut out, &format!("ip daddr {V4_ALWAYS} {REJECT}"));
    add(&mut out, &format!("ip6 daddr {V6_ALWAYS} {REJECT}"));
    if level == Level::Internet {
        add(&mut out, &format!("ip daddr {V4_PRIVATE} {REJECT}"));
        add(&mut out, &format!("ip6 daddr {V6_PRIVATE} {REJECT}"));
    }
    if dests.is_empty() {
        add(&mut out, "accept");
        return Ok(out);
    }
    for d in dests {
        if !allowed(level, d.ip()) {
            bail!("{} is not reachable at network level {level}", d.ip());
        }
        let (family, proto) = if d.is_ipv4() { ("ip", "daddr") } else { ("ip6", "daddr") };
        add(&mut out, &format!("{family} {proto} {} tcp dport {} accept", d.ip(), d.port()));
        add(&mut out, &format!("{family} {proto} {} udp dport {} accept", d.ip(), d.port()));
    }
    add(&mut out, REJECT);
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ip(s: &str) -> IpAddr {
        s.parse().unwrap()
    }

    #[test]
    fn the_cloud_metadata_service_loopback_and_link_local_are_never_reachable() {
        for a in [
            "169.254.169.254", "127.0.0.1", "127.255.255.254", "0.0.0.0", "0.1.2.3", "224.0.0.1", "239.255.255.250",
            "240.0.0.1", "255.255.255.255", "::", "::1", "fe80::1", "febf::1", "ff02::1",
        ] {
            assert!(always_rejected(ip(a)), "{a}");
            for l in [Level::None, Level::Internet, Level::InternetLan] {
                assert!(!allowed(l, ip(a)), "{a} at {l}");
            }
        }
    }

    #[test]
    fn ipv6_forms_that_reach_ipv4_are_judged_as_that_ipv4_address() {
        for a in [
            "::ffff:127.0.0.1", "::ffff:169.254.169.254", "::ffff:8.8.8.8", "::ffff:10.0.0.1", "64:ff9b::7f00:1",
            "64:ff9b::a9fe:a9fe", "64:ff9b::e000:1",
        ] {
            assert!(always_rejected(ip(a)), "{a}");
        }
        for a in ["64:ff9b::a00:1", "64:ff9b::c0a8:1", "64:ff9b:1::1", "fd00::1", "fc00::1"] {
            assert!(private(ip(a)) && !always_rejected(ip(a)), "{a}");
            assert!(!allowed(Level::Internet, ip(a)), "{a}");
            assert!(allowed(Level::InternetLan, ip(a)), "{a}");
        }
        assert!(!allowed(Level::Internet, ip("::ffff:8.8.8.8")));
        assert!(allowed(Level::Internet, ip("64:ff9b::808:808")));
    }

    #[test]
    fn private_ranges_need_the_lan_level_and_public_addresses_need_only_internet() {
        for a in ["10.1.2.3", "172.16.0.1", "172.31.255.255", "192.168.1.1", "100.64.0.1", "100.127.255.254"] {
            assert!(private(ip(a)), "{a}");
            assert!(!allowed(Level::Internet, ip(a)), "{a}");
            assert!(allowed(Level::InternetLan, ip(a)), "{a}");
        }
        for a in ["8.8.8.8", "1.1.1.1", "172.15.255.255", "172.32.0.1", "100.63.255.255", "100.128.0.1", "2606:4700::1111"] {
            assert!(!private(ip(a)) && !always_rejected(ip(a)), "{a}");
            assert!(allowed(Level::Internet, ip(a)), "{a}");
        }
        assert!(!allowed(Level::None, ip("8.8.8.8")));
    }

    #[test]
    fn levels_are_ordered_parse_and_print() {
        assert!(Level::None < Level::Internet && Level::Internet < Level::InternetLan);
        for s in ["none", "internet", "internet+lan"] {
            assert_eq!(s.parse::<Level>().unwrap().to_string(), s);
        }
        assert!("lan".parse::<Level>().is_err());
        assert_eq!(Level::default(), Level::None);
    }

    /// Every address in the rule text's always-rejected sets agrees with `always_rejected`, so the filter and the classifier
    /// cannot drift apart.
    #[test]
    fn the_rule_text_and_the_classifier_agree_on_a_sweep_of_addresses() {
        let mut checked = 0;
        for a in 0..=255u32 {
            for b in [0u32, 1, 63, 64, 100, 127, 128, 168, 169, 254, 255] {
                let v4 = Ipv4Addr::new(a as u8, b as u8, 1, 1);
                let in_rules = [(0, 8), (127, 8), (169, 16), (224, 4), (240, 4)].iter().any(|&(base, bits)| {
                    let (net, ip) = match (base, bits) {
                        (169, 16) => (u32::from(Ipv4Addr::new(169, 254, 0, 0)), u32::from(v4)),
                        _ => (u32::from(Ipv4Addr::new(base as u8, 0, 0, 0)), u32::from(v4)),
                    };
                    let mask = u32::MAX << (32 - bits);
                    ip & mask == net & mask
                });
                assert_eq!(always_rejected(IpAddr::V4(v4)), in_rules, "{v4}");
                checked += 1;
            }
        }
        assert!(checked > 2000);
    }

    #[test]
    fn a_run_with_no_network_gets_a_chain_that_rejects_everything() {
        let r = run_chain("playbox/runs", "0123456789abcdef", Level::None, &[]).unwrap();
        assert!(r.contains("add chain inet playbox_runner run_0123456789abcdef"));
        assert!(r.contains("jump run_0123456789abcdef"));
        assert!(r.contains("cgroupv2 level 3 \"playbox/runs/0123456789abcdef\""));
        assert!(r.trim_end().ends_with("reject with icmpx admin-prohibited"));
        assert!(!r.contains("accept"));
        assert!(run_chain("playbox/runs", "0123456789abcdef", Level::None, &["8.8.8.8:443".parse().unwrap()]).is_err());
    }

    #[test]
    fn the_protections_come_before_any_accept_and_a_destination_list_ends_in_a_reject() {
        let d: Vec<SocketAddr> = vec!["93.184.216.34:443".parse().unwrap(), "[2606:4700::1111]:443".parse().unwrap()];
        let r = run_chain("playbox/runs", "deadbeef00", Level::Internet, &d).unwrap();
        let first_accept = r.find("accept").unwrap();
        for needle in ["169.254.0.0/16", "127.0.0.0/8", "10.0.0.0/8", "fc00::/7", "fe80::/10"] {
            assert!(r.find(needle).unwrap() < first_accept, "{needle} must precede the first accept");
        }
        assert!(r.contains("ip daddr 93.184.216.34 tcp dport 443 accept"));
        assert!(r.contains("ip6 daddr 2606:4700::1111 udp dport 443 accept"));
        assert!(r.trim_end().ends_with("admin-prohibited"));
        // Without a list at the internet level the chain accepts what the protections let through.
        let open = run_chain("playbox/runs", "deadbeef00", Level::Internet, &[]).unwrap();
        assert!(open.trim_end().ends_with("accept"));
    }

    #[test]
    fn a_destination_the_level_forbids_is_an_error_not_a_silent_drop() {
        for (level, addr) in [
            (Level::Internet, "10.0.0.5:22"),
            (Level::Internet, "127.0.0.1:8080"),
            (Level::InternetLan, "169.254.169.254:80"),
            (Level::InternetLan, "[::1]:80"),
            (Level::Internet, "[64:ff9b::a9fe:a9fe]:80"),
        ] {
            let d: SocketAddr = addr.parse().unwrap();
            assert!(run_chain("playbox/runs", "deadbeef00", level, &[d]).is_err(), "{addr} at {level}");
        }
        assert!(run_chain("playbox/runs", "deadbeef00", Level::InternetLan, &["10.0.0.5:22".parse().unwrap()]).is_ok());
    }

    #[test]
    fn run_ids_and_cgroup_paths_that_could_break_out_of_a_rule_are_refused() {
        for id in ["", "short", "XYZ12345", "dead beef00", "dead\"beef00", "deadbeef00; flush ruleset", "../../x1234567"] {
            assert!(run_chain("playbox/runs", id, Level::Internet, &[]).is_err(), "{id:?}");
        }
        assert!(base_ruleset("user.slice/user-1000.slice/playbox").is_ok(), "systemd slice names contain dots");
        for path in ["", "/abs", "a//b", "a/../b", "a/./b", "a\"b", "a b", "a;b", "a\nb", "a\\b"] {
            assert!(base_ruleset(path).is_err(), "{path:?}");
            assert!(run_chain(path, "deadbeef00", Level::Internet, &[]).is_err(), "{path:?}");
        }
    }

    #[test]
    fn the_base_table_rejects_runner_traffic_that_has_no_chain() {
        let t = base_ruleset("playbox/runs").unwrap();
        assert!(t.contains("socket cgroupv2 level 2 \"playbox/runs\" jump runs"));
        let runs = t.split("chain runs").nth(1).unwrap();
        assert!(runs.contains("admin-prohibited") && !runs.contains("accept"));
    }
}
