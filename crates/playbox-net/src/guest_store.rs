//! Persistent guest room grant vault: keeps a joined room's Biscuit member grant, host address,
//! replica hints, and vetting policy in owner-only (`0600`) files under `<home>/guest-rooms/` so
//! agent and CLI restarts auto-resume without spending a new invite.

use crate::state::write_atomic;
use crate::{decode_peer, encode_peer, EndpointAddr, Transport};
use anyhow::{anyhow, Context, Result};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// One persisted guest room membership.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SavedRoom {
    /// Primary host's endpoint id (also the filename stem).
    pub host_id: String,
    /// Encoded primary host peer address (`encode_peer`).
    pub addr: String,
    /// Transport label (`local`, `iroh`, or `relay:<url>`).
    pub transport: String,
    /// The minted Biscuit member grant (bearer token; stored `0600`).
    pub grant: String,
    /// The guest's member id (endpoint id).
    pub member_id: String,
    /// Epoch seconds when the room was joined.
    pub joined_at: i64,
    /// Epoch seconds when the grant expires, if reported by the host.
    #[serde(default)]
    pub expires_at: Option<i64>,
    /// Encoded replica peer addresses known at join time.
    #[serde(default)]
    pub hint_hosts: Vec<String>,
    /// Minimum host attestation tier enforced when reconnecting (`t0`..`t3`).
    #[serde(default = "default_tier")]
    pub min_host_tier: String,
    /// Whether `trust_host_device` was enabled at join.
    #[serde(default)]
    pub trust_host_device: bool,
    /// Optional workspace path used by MCP.
    #[serde(default)]
    pub workspace: Option<String>,
    /// Optional `@handle` registered by this guest in the room.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub handle: Option<String>,
}

fn default_tier() -> String {
    "t0".to_string()
}

impl SavedRoom {
    /// Whether this grant is known to be expired at `now_secs`.
    #[must_use]
    pub fn is_expired(&self, now_secs: i64) -> bool {
        self.expires_at.is_some_and(|exp| now_secs >= exp)
    }

    /// Decode the primary host's [`EndpointAddr`].
    ///
    /// # Errors
    ///
    /// Returns an error if the stored peer address is malformed.
    pub fn host_addr(&self) -> Result<EndpointAddr> {
        decode_peer(&self.addr)
    }

    /// Decode all valid replica hint addresses.
    #[must_use]
    pub fn replica_addrs(&self) -> Vec<EndpointAddr> {
        self.hint_hosts
            .iter()
            .filter_map(|h| decode_peer(h).ok())
            .collect()
    }

    /// Parse the stored [`Transport`].
    ///
    /// # Errors
    ///
    /// Returns an error if the transport string is invalid.
    pub fn parsed_transport(&self) -> Result<Transport> {
        Transport::parse(&self.transport)
    }

    /// Helper to encode a list of replica addresses.
    #[must_use]
    pub fn encode_hosts(hosts: &[EndpointAddr]) -> Vec<String> {
        hosts.iter().map(encode_peer).collect()
    }
}

/// Owner-only persistent store for joined guest rooms under `<home>/guest-rooms/`.
#[derive(Debug, Clone)]
pub struct GuestStore {
    dir: PathBuf,
}

impl GuestStore {
    /// Open the guest room vault rooted under `home` (typically `~/.playbox`).
    #[must_use]
    pub fn new(home: &Path) -> Self {
        Self {
            dir: home.join("guest-rooms"),
        }
    }

    fn room_path(&self, host_id: &str) -> Result<PathBuf> {
        if host_id.is_empty()
            || !host_id
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
        {
            return Err(anyhow!("invalid host_id {host_id:?}"));
        }
        Ok(self.dir.join(format!("{host_id}.json")))
    }

    fn active_path(&self) -> PathBuf {
        self.dir.join("active")
    }

    fn ensure_dir(&self) -> Result<()> {
        std::fs::create_dir_all(&self.dir)
            .with_context(|| format!("creating {}", self.dir.display()))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(&self.dir, std::fs::Permissions::from_mode(0o700));
        }
        Ok(())
    }

    /// Persist a joined room and mark it as the active room to auto-resume.
    ///
    /// # Errors
    ///
    /// Returns an error if the host id is invalid or the file cannot be written.
    pub fn save(&self, room: &SavedRoom) -> Result<()> {
        self.ensure_dir()?;
        let path = self.room_path(&room.host_id)?;
        let bytes = serde_json::to_vec_pretty(room).context("serializing saved room")?;
        write_atomic(&path, &bytes).with_context(|| format!("writing {}", path.display()))?;
        write_atomic(&self.active_path(), room.host_id.as_bytes())
            .with_context(|| format!("writing {}", self.active_path().display()))?;
        Ok(())
    }

    /// Load one saved room by `host_id`.
    ///
    /// # Errors
    ///
    /// Returns an error if the file exists but cannot be read or parsed.
    pub fn load(&self, host_id: &str) -> Result<Option<SavedRoom>> {
        let path = self.room_path(host_id)?;
        let text = match std::fs::read_to_string(&path) {
            Ok(t) => t,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(e).with_context(|| format!("reading {}", path.display())),
        };
        let room: SavedRoom = serde_json::from_str(&text)
            .with_context(|| format!("parsing saved room {}", path.display()))?;
        Ok(Some(room))
    }

    /// Load the most recently joined (active) room, if any.
    ///
    /// # Errors
    ///
    /// Returns an error if the active room file exists but is corrupt.
    pub fn load_active(&self) -> Result<Option<SavedRoom>> {
        let active = match std::fs::read_to_string(self.active_path()) {
            Ok(s) => s.trim().to_string(),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(e).context("reading active guest room pointer"),
        };
        if active.is_empty() {
            return Ok(None);
        }
        self.load(&active)
    }

    /// List all saved rooms, sorted by `joined_at` descending.
    ///
    /// # Errors
    ///
    /// Returns an error if the directory exists but cannot be read.
    pub fn list(&self) -> Result<Vec<SavedRoom>> {
        let entries = match std::fs::read_dir(&self.dir) {
            Ok(e) => e,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(e).with_context(|| format!("reading {}", self.dir.display())),
        };
        let mut out = Vec::new();
        for entry in entries.flatten() {
            let p = entry.path();
            if p.extension().and_then(|e| e.to_str()) != Some("json") {
                continue;
            }
            if let Ok(text) = std::fs::read_to_string(&p) {
                if let Ok(room) = serde_json::from_str::<SavedRoom>(&text) {
                    out.push(room);
                }
            }
        }
        out.sort_by(|a, b| b.joined_at.cmp(&a.joined_at));
        Ok(out)
    }

    /// Remove a saved room and clear `active` if it pointed to `host_id`.
    ///
    /// # Errors
    ///
    /// Returns an error if `host_id` is invalid or removal fails unexpectedly.
    pub fn remove(&self, host_id: &str) -> Result<()> {
        let path = self.room_path(host_id)?;
        match std::fs::remove_file(&path) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e).with_context(|| format!("removing {}", path.display())),
        }
        if let Ok(cur) = std::fs::read_to_string(self.active_path()) {
            if cur.trim() == host_id {
                let _ = std::fs::remove_file(self.active_path());
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    fn sample(host_id: &str, joined_at: i64, expires_at: Option<i64>) -> SavedRoom {
        SavedRoom {
            host_id: host_id.to_string(),
            addr: "pbxpeer1".to_string(),
            transport: "local".to_string(),
            grant: "biscuit-secret-token".to_string(),
            member_id: "member123".to_string(),
            joined_at,
            expires_at,
            hint_hosts: vec![],
            min_host_tier: "t1".to_string(),
            trust_host_device: true,
            workspace: Some("/tmp/ws".to_string()),
            handle: None,
        }
    }

    #[test]
    fn save_load_active_list_remove_and_permissions() {
        let dir = std::env::temp_dir().join(format!("pbx-guest-store-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let store = GuestStore::new(&dir);
        assert!(store.load_active().unwrap().is_none());

        let r1 = sample("host_aaa", 100, Some(200));
        store.save(&r1).unwrap();
        assert_eq!(store.load("host_aaa").unwrap(), Some(r1.clone()));
        assert_eq!(store.load_active().unwrap(), Some(r1.clone()));

        let mode = std::fs::metadata(dir.join("guest-rooms/host_aaa.json"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o600, "saved grant file must be owner-only 0600");

        let r2 = sample("host_bbb", 150, None);
        store.save(&r2).unwrap();
        assert_eq!(store.load_active().unwrap(), Some(r2.clone()));
        let all = store.list().unwrap();
        assert_eq!(all.len(), 2);
        assert_eq!(all[0].host_id, "host_bbb");

        assert!(r1.is_expired(200));
        assert!(!r1.is_expired(199));
        assert!(!r2.is_expired(999_999));

        store.remove("host_bbb").unwrap();
        assert!(store.load("host_bbb").unwrap().is_none());
        assert!(store.load_active().unwrap().is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn rejects_path_traversal_host_id() {
        let dir = std::env::temp_dir().join("pbx-guest-store-traversal");
        let store = GuestStore::new(&dir);
        assert!(store.load("../evil").is_err());
        assert!(store.remove("a/b").is_err());
    }
}
