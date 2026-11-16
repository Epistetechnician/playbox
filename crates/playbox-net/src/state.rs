//! The room's membership state (invites, members, revocations, bans) and how it is saved to disk.

use anyhow::{anyhow, Context, Result};
use iroh::EndpointId;
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, HashMap, HashSet},
    path::PathBuf,
};

#[derive(Debug)]
pub(crate) struct Invite {
    pub(crate) uses_left: u32,
    /// If set, only this key may redeem the invite.
    pub(crate) bound_to: Option<EndpointId>,
}

/// What the host knows about how a member got in: the delegation, host to member. Not secret (no token), kept so the hub can show who holds
/// what and until when. A member admitted before this was recorded has none.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Admit {
    /// When the grant was issued, and when it runs out (seconds since the epoch).
    pub issued: i64,
    pub expires: i64,
    /// The first 8 characters of the invite's id, enough to tell which link was used. Never the invite itself.
    pub invite: String,
    pub cpu_milli: i64,
    pub mem_mib: i64,
    pub timeout_secs: i64,
    pub net: bool,
    /// The hosts, secrets (`secret/NAME`) and capabilities (`cap/NAME`) the grant allows.
    pub allowed: Vec<String>,
}

#[derive(Debug, Default)]
pub(crate) struct State {
    /// How each member was admitted (see [`Admit`]).
    pub(crate) admits: HashMap<EndpointId, Admit>,
    /// Unspent invites by id.
    pub(crate) invites: HashMap<String, Invite>,
    /// Members and the id of the grant issued to each.
    pub(crate) members: HashMap<EndpointId, String>,
    /// Revoked grant ids. Revoking an authority block revokes every attenuation of it.
    pub(crate) revoked: HashSet<String>,
    /// Members who may not rejoin.
    pub(crate) banned: HashSet<EndpointId>,
    /// Signed revocation notices by grant id: the room's revocations, kept so they can be passed on.
    pub(crate) notices: BTreeMap<String, String>,
    /// Notices read from disk, not yet verified. `build` verifies and applies them.
    pub(crate) pending: Vec<String>,
    /// Where this state is saved after every change, if the host keeps state on disk.
    path: Option<PathBuf>,
}

/// On-disk form of [`State`].
#[derive(Serialize, Deserialize, Default)]
struct Saved {
    invites: HashMap<String, u32>,
    /// Invite id -> the only key allowed to redeem it.
    #[serde(default)]
    bound: HashMap<String, String>,
    members: HashMap<String, String>,
    revoked: Vec<String>,
    banned: Vec<String>,
    #[serde(default)]
    notices: Vec<String>,
    #[serde(default)]
    admits: HashMap<String, Admit>,
}

impl State {
    pub(crate) fn load(path: &std::path::Path) -> Result<Self> {
        let mut st = State {
            path: Some(path.to_path_buf()),
            ..Default::default()
        };
        let text = match std::fs::read_to_string(path) {
            Ok(t) => t,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(st),
            Err(e) => return Err(e).context("reading host state"),
        };
        let saved: Saved = serde_json::from_str(&text).context("host state file is corrupt")?;
        let id = |s: &str| {
            s.parse::<EndpointId>()
                .map_err(|e| anyhow!("bad endpoint id in state: {e}"))
        };
        for (k, v) in saved.invites {
            let bound_to = saved.bound.get(&k).map(|b| id(b)).transpose()?;
            st.invites.insert(
                k,
                Invite {
                    uses_left: v,
                    bound_to,
                },
            );
        }
        for (m, a) in saved.admits {
            st.admits.insert(id(&m)?, a);
        }
        for (m, g) in saved.members {
            st.members.insert(id(&m)?, g);
        }
        st.revoked = saved.revoked.into_iter().collect();
        for b in saved.banned {
            st.banned.insert(id(&b)?);
        }
        st.pending = saved.notices;
        Ok(st)
    }

    /// Write the state next to its final path, then rename, so a crash never leaves half a file.
    /// A failure is logged, not fatal: the room keeps working but the change may not survive a restart.
    pub(crate) fn save(&self) {
        let Some(path) = &self.path else { return };
        let saved = Saved {
            invites: self
                .invites
                .iter()
                .map(|(k, v)| (k.clone(), v.uses_left))
                .collect(),
            bound: self
                .invites
                .iter()
                .filter_map(|(k, v)| v.bound_to.map(|b| (k.clone(), b.to_string())))
                .collect(),
            members: self
                .members
                .iter()
                .map(|(k, v)| (k.to_string(), v.clone()))
                .collect(),
            revoked: self.revoked.iter().cloned().collect(),
            banned: self.banned.iter().map(ToString::to_string).collect(),
            notices: self.notices.values().cloned().collect(),
            admits: self
                .admits
                .iter()
                .map(|(k, v)| (k.to_string(), v.clone()))
                .collect(),
        };
        let result = serde_json::to_vec_pretty(&saved)
            .map_err(anyhow::Error::from)
            .and_then(|b| write_atomic(path, &b).map_err(Into::into));
        if let Err(e) = result {
            eprintln!(
                "playbox: could not save host state to {}: {e:#}",
                path.display()
            );
        }
    }
}

/// Replace `path` with `bytes` so that a crash or power cut leaves either the old file or the new one, never an empty
/// or half-written one: write a private (0600) temporary file next to it, flush it to disk, rename it over `path`, then
/// flush the directory so the rename itself survives. (A rename alone can reach the disk before the data does.)
pub(crate) fn write_atomic(path: &std::path::Path, bytes: &[u8]) -> std::io::Result<()> {
    use std::{io::Write, os::unix::fs::OpenOptionsExt};
    let tmp = path.with_extension("tmp");
    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(&tmp)?;
    f.write_all(bytes)?;
    f.sync_all()?;
    drop(f);
    std::fs::rename(&tmp, path)?;
    if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
        std::fs::File::open(dir)?.sync_all()?;
    }
    Ok(())
}

#[cfg(test)]
mod write_atomic_tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn state_is_replaced_whole_and_kept_private() {
        let dir = std::env::temp_dir().join(format!("pbx-atomic-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("state.json");
        let st = State {
            path: Some(p.clone()),
            ..Default::default()
        };
        st.save();
        st.save();
        assert!(State::load(&p).is_ok());
        assert_eq!(
            std::fs::metadata(&p).unwrap().permissions().mode() & 0o777,
            0o600,
            "the room's state is for the host only"
        );
        assert!(!p.with_extension("tmp").exists());
        std::fs::remove_dir_all(dir).ok();
    }
}
