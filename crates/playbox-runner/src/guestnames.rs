//! Name lookup for a guest that has a network.
//!
//! A root filesystem exported from Docker has an empty `/etc/resolv.conf`, so a task with a network could reach addresses but
//! not names. Before the guest boots, the host writes two files into the task's private clone:
//!
//! - `/etc/hosts`, when the task has a host list: each listed name with the addresses the host resolved and pinned for the
//!   network filter. The guest then needs no resolver, and cannot be answered with an address the filter did not pin.
//! - `/etc/resolv.conf`, when the task has an open network: the resolvers the host itself uses that the task is allowed to
//!   reach, or two public ones when none is (a home router's address is private, and a filtered task may not reach it).
//!   `PLAYBOX_GUEST_DNS` replaces the choice; set it empty for no resolver at all.

use anyhow::{bail, Context, Result};
use std::{
    io::Write,
    net::IpAddr,
    path::{Path, PathBuf},
};

/// Used when the host has no resolver the task may reach: Cloudflare and Quad9.
pub const FALLBACK_RESOLVERS: [&str; 2] = ["1.1.1.1", "9.9.9.9"];
/// What a libc reads from `resolv.conf`.
const MAX_RESOLVERS: usize = 3;

/// The addresses on the `nameserver` lines of a `resolv.conf`.
#[must_use]
pub fn nameservers(resolv_conf: &str) -> Vec<IpAddr> {
    resolv_conf
        .lines()
        .filter_map(|l| l.trim().strip_prefix("nameserver"))
        // A scoped address (`fe80::1%eth0`) means nothing inside the guest.
        .filter_map(|rest| rest.split_whitespace().next()?.parse().ok())
        .collect()
}

/// The resolvers to give a guest: the owner's choice if there is one, else the host's own that `may_reach` allows, else the
/// public fallback. IPv4 first, at most three, no repeats.
///
/// # Errors
///
/// Fails if `owner_choice` holds something that is not an address.
pub fn choose(owner_choice: Option<&str>, host_resolv_conf: &str, may_reach: impl Fn(IpAddr) -> bool) -> Result<Vec<IpAddr>> {
    let mut picked: Vec<IpAddr> = if let Some(list) = owner_choice {
        list.split(',')
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(|s| s.parse().with_context(|| format!("PLAYBOX_GUEST_DNS: {s:?} is not an IP address")))
            .collect::<Result<_>>()?
    } else {
        let mut own: Vec<IpAddr> = nameservers(host_resolv_conf).into_iter().filter(|ip| may_reach(*ip)).collect();
        if own.is_empty() {
            own = FALLBACK_RESOLVERS.iter().filter_map(|s| s.parse().ok()).collect();
        }
        own
    };
    picked.sort_by_key(IpAddr::is_ipv6); // stable: keeps the given order within each family
    let mut seen = Vec::new();
    picked.retain(|ip| !seen.contains(ip) && { seen.push(*ip); true });
    picked.truncate(MAX_RESOLVERS);
    Ok(picked)
}

/// The host's own `resolv.conf`. Where systemd-resolved runs, `/etc/resolv.conf` names only its loopback stub, so the file
/// with the real upstream resolvers is read first.
#[must_use]
pub fn host_resolv_conf() -> String {
    ["/run/systemd/resolve/resolv.conf", "/etc/resolv.conf"]
        .iter()
        .find_map(|p| std::fs::read_to_string(p).ok().filter(|s| !nameservers(s).is_empty()))
        .unwrap_or_default()
}

/// The text of a guest's `/etc/resolv.conf`.
#[must_use]
pub fn resolv_conf(resolvers: &[IpAddr]) -> String {
    resolvers.iter().map(|ip| format!("nameserver {ip}\n")).collect()
}

/// The text of a guest's `/etc/hosts`: localhost, then each pinned name.
#[must_use]
pub fn hosts_file(pinned: &[(String, IpAddr)]) -> String {
    let mut out = String::from("127.0.0.1 localhost\n::1 localhost\n");
    let mut seen = Vec::new();
    for (name, ip) in pinned {
        // Names come from a grant: anything that is not a plain host name would be a way to add lines to the file.
        let plain = !name.is_empty() && name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'.' || b == b'-');
        if plain && !seen.contains(&(name, ip)) {
            seen.push((name, ip));
            out.push_str(&format!("{ip} {name}\n"));
        }
    }
    out
}

/// Replace `etc/<name>` in a task's root with `text`.
///
/// The root comes from an image, so nothing in it is followed: `etc` must be a real directory, and whatever is at the path
/// (a file, or a link pointing anywhere) is removed and a new file made in its place.
///
/// # Errors
///
/// Fails if `etc` is missing or is a link, or the file cannot be written.
pub fn write_etc(root: &Path, name: &str, text: &str) -> Result<PathBuf> {
    let etc = root.join("etc");
    let meta = std::fs::symlink_metadata(&etc).with_context(|| format!("the image has no /etc ({})", etc.display()))?;
    if !meta.file_type().is_dir() {
        bail!("the image's /etc is not a directory, so /etc/{name} was not written");
    }
    let path = etc.join(name);
    match std::fs::symlink_metadata(&path) {
        Ok(m) if m.file_type().is_dir() => bail!("the image's /etc/{name} is a directory"),
        Ok(_) => std::fs::remove_file(&path)?,
        Err(_) => {}
    }
    let mut f = std::fs::OpenOptions::new().write(true).create_new(true).open(&path)?;
    f.write_all(text.as_bytes())?;
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::netpolicy::{allowed, Level};

    fn ip(s: &str) -> IpAddr {
        s.parse().unwrap()
    }

    // REQ: BOX-8
    #[test]
    fn a_filtered_guest_gets_only_resolvers_it_may_reach_and_a_public_one_when_there_is_none() {
        let internet = |a| allowed(Level::Internet, a);
        // A stub on loopback and a home router: neither is reachable at the internet level.
        let home = "# comment\nnameserver 127.0.0.53\nnameserver 192.168.1.1\nsearch lan\n";
        assert_eq!(choose(None, home, internet).unwrap(), vec![ip("1.1.1.1"), ip("9.9.9.9")]);
        // A public resolver the host uses is kept, IPv4 first, and the unreachable ones dropped.
        let mixed = "nameserver 2606:4700:4700::1111\nnameserver 10.0.0.2\nnameserver 8.8.8.8\nnameserver 8.8.8.8\n";
        assert_eq!(choose(None, mixed, internet).unwrap(), vec![ip("8.8.8.8"), ip("2606:4700:4700::1111")]);
        // An unfiltered guest shares the host's network, so the host's own resolvers work as they are.
        assert_eq!(choose(None, home, |_| true).unwrap(), vec![ip("127.0.0.53"), ip("192.168.1.1")]);
        assert_eq!(choose(None, "", |_| true).unwrap().len(), 2);
    }

    // REQ: BOX-8
    #[test]
    fn the_owner_can_name_the_resolvers_or_none() {
        assert_eq!(choose(Some("10.0.0.2, 9.9.9.9"), "nameserver 8.8.8.8", |_| false).unwrap(), vec![ip("10.0.0.2"), ip("9.9.9.9")]);
        assert!(choose(Some(""), "nameserver 8.8.8.8", |_| true).unwrap().is_empty());
        assert!(choose(Some("dns.example"), "", |_| true).is_err());
        assert_eq!(choose(Some("1.1.1.1,1.0.0.1,8.8.8.8,9.9.9.9"), "", |_| true).unwrap().len(), 3);
    }

    // REQ: BOX-8
    #[test]
    fn the_hosts_file_lists_pinned_names_and_nothing_a_name_could_smuggle_in() {
        let text = hosts_file(&[
            ("api.example.com".into(), ip("93.184.216.34")),
            ("api.example.com".into(), ip("93.184.216.34")),
            ("api.example.com".into(), ip("2606:2800:220:1::1")),
            ("evil\n10.0.0.1 bank.example".into(), ip("10.0.0.1")),
            ("two words".into(), ip("10.0.0.1")),
        ]);
        assert_eq!(
            text,
            "127.0.0.1 localhost\n::1 localhost\n93.184.216.34 api.example.com\n2606:2800:220:1::1 api.example.com\n"
        );
        assert_eq!(resolv_conf(&[ip("1.1.1.1"), ip("9.9.9.9")]), "nameserver 1.1.1.1\nnameserver 9.9.9.9\n");
    }

    // REQ: BOX-8
    #[test]
    fn writing_into_a_task_root_never_follows_a_link_the_image_planted() {
        let dir = std::env::temp_dir().join(format!("playbox-guestnames-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let (root, outside) = (dir.join("root"), dir.join("outside"));
        std::fs::create_dir_all(root.join("etc")).unwrap();
        std::fs::create_dir_all(&outside).unwrap();
        std::fs::write(outside.join("victim"), "host file").unwrap();

        // The file is a link to something outside the root: the link is replaced, the target untouched.
        std::os::unix::fs::symlink(outside.join("victim"), root.join("etc/resolv.conf")).unwrap();
        let written = write_etc(&root, "resolv.conf", "nameserver 1.1.1.1\n").unwrap();
        assert_eq!(std::fs::read_to_string(outside.join("victim")).unwrap(), "host file");
        assert_eq!(std::fs::read_to_string(&written).unwrap(), "nameserver 1.1.1.1\n");
        assert!(!std::fs::symlink_metadata(&written).unwrap().file_type().is_symlink());

        // `etc` itself is a link out of the root: nothing is written anywhere.
        let linked = dir.join("linked");
        std::fs::create_dir_all(&linked).unwrap();
        std::os::unix::fs::symlink(&outside, linked.join("etc")).unwrap();
        assert!(write_etc(&linked, "hosts", "x").is_err());
        assert!(!outside.join("hosts").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
