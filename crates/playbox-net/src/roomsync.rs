//! Replicating the room log between hosts (M1 of `docs/host-less-rooms.md`).
//!
//! The leader sends a follower the log and a checkpoint; the follower checks every entry itself, then cosigns
//! only if the checkpoint matches what it replayed. A follower trusts nothing but the room's root public key.
//! Peers are the same hosts the revocation sync already knows (`SyncHandler`'s allow-list).
//!
//! The log and its certificates are kept in a file (`roomlog.jsonl` in the state directory) and re-verified on
//! load. Limits: the leader resends the whole log on each commit (fine for small rooms).

use crate::sync::LockExt;
use anyhow::{anyhow, bail, Context, Result};
use ed25519_dalek::SigningKey;
use iroh::{
    endpoint::Connection,
    protocol::{AcceptError, ProtocolHandler},
    Endpoint, EndpointAddr, EndpointId,
};
use playbox_core::promotion::{Promotion, Receipt, Register};
use playbox_core::roomlog::{
    self, text, Checkpoint, Config, Entry, Equivocation, Proposal, RoomLog, SignedCheckpoint,
};
use serde_json::{json, Value};
use std::{
    collections::HashSet,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

pub const ROOMLOG_ALPN: &[u8] = b"playbox/roomlog/1";
const MAX_MSG: usize = 8 * 1024 * 1024;

/// What this host has promised and seen, kept next to the log. A promise must survive a restart or a host could
/// sign two rival bids for one epoch.
#[derive(Debug, Clone, Default)]
struct Local {
    /// The bid this host last signed (highest epoch).
    promised: Option<Proposal>,
    /// The last checkpoint this host cosigned.
    last: Option<Checkpoint>,
    /// Proofs that a host signed two entries at one position.
    proofs: Vec<Equivocation>,
}

/// This host's copy of the room log, its cosigned checkpoints and the key it cosigns with.
pub struct RoomLogNode {
    room: [u8; 32],
    key: SigningKey,
    log: Mutex<Option<RoomLog>>,
    certs: Mutex<Vec<SignedCheckpoint>>,
    local: Mutex<Local>,
    heard: Mutex<Instant>,
    /// Refuse to sign a rival bid while the leader has been heard from this recently (a leader that is merely
    /// unreachable from one follower should not be replaced by it). Zero disables.
    sticky: Mutex<Duration>,
    /// The program this host runs to measure a candidate, if it takes part in promotions.
    evaluator: Mutex<Option<std::path::PathBuf>>,
    /// One measurement at a time: they time things, and two at once would disturb each other.
    measuring: tokio::sync::Mutex<()>,
    allowed: Arc<Mutex<HashSet<EndpointId>>>,
    store: Option<std::path::PathBuf>,
}

impl std::fmt::Debug for RoomLogNode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("RoomLogNode")
    }
}

/// A host's view of the room log.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Status {
    pub len: u64,
    pub head: String,
    pub committed: u64,
    pub epoch: u64,
    pub leader: String,
    pub hosts: Vec<String>,
    pub k: usize,
    pub members: Vec<String>,
    pub banned: Vec<String>,
    /// Hosts proven to have signed two different entries at one position.
    pub equivocators: Vec<String>,
    /// Each registered problem's frontier as of everything the log holds.
    pub problems: Vec<ProblemStatus>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProblemStatus {
    pub problem: String,
    /// The promoted candidate, or `None` while the pinned baseline is the frontier.
    pub candidate: Option<String>,
    pub metric: i64,
    /// The lowest attestation tier among the hosts that signed the promotion.
    pub strength: u8,
    pub j: usize,
}

fn status_of(l: &RoomLog, certs: &[SignedCheckpoint], proofs: &[Equivocation]) -> Status {
    let r = l.roster();
    let mut eq: Vec<String> = proofs
        .iter()
        .filter_map(|q| q.verify(&l.room()).ok())
        .map(|k| roomlog::hex(&k))
        .collect();
    eq.sort();
    eq.dedup();
    Status {
        len: l.len(),
        head: roomlog::hex(&l.head()),
        committed: l.committed(certs),
        epoch: l.config().epoch,
        leader: roomlog::hex(&l.config().leader),
        hosts: l.config().hosts.iter().map(|h| roomlog::hex(h)).collect(),
        k: l.config().k,
        members: r.members.into_iter().collect(),
        banned: r.banned.into_iter().collect(),
        equivocators: eq,
        problems: l
            .frontier(None)
            .into_iter()
            .map(|(p, (rule, f))| ProblemStatus {
                problem: roomlog::hex(&p),
                candidate: (f.index > 0).then(|| roomlog::hex(&f.candidate)),
                metric: f.metric,
                strength: f.strength,
                j: rule.j,
            })
            .collect(),
    }
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
        .unwrap_or(0)
}

impl RoomLogNode {
    pub fn new(
        room: [u8; 32],
        key: SigningKey,
        allowed: Arc<Mutex<HashSet<EndpointId>>>,
        store: Option<std::path::PathBuf>,
    ) -> Result<Arc<Self>> {
        let (log, certs, local) = match &store {
            Some(p) if p.exists() => load(room, p)?,
            _ => (None, Vec::new(), Local::default()),
        };
        if let Some(l) = &log {
            let mut a = allowed.lock_recover();
            for h in &l.config().hosts {
                if let Ok(id) = EndpointId::from_bytes(h) {
                    a.insert(id);
                }
            }
        }
        Ok(Arc::new(Self {
            room,
            key,
            log: Mutex::new(log),
            certs: Mutex::new(certs),
            local: Mutex::new(local),
            heard: Mutex::new(Instant::now()),
            sticky: Mutex::new(Duration::ZERO),
            evaluator: Mutex::new(None),
            measuring: tokio::sync::Mutex::new(()),
            allowed,
            store,
        }))
    }

    fn me(&self) -> [u8; 32] {
        self.key.verifying_key().to_bytes()
    }

    /// Write the log, certificates, promises and proofs next to each other, atomically. A failed write is
    /// reported, not fatal: the state in memory stays right and the next change writes it again.
    fn save(&self) {
        let Some(p) = &self.store else { return };
        let g = self.log.lock_recover();
        let Some(l) = g.as_ref() else { return };
        let mut out = text::to_text(l);
        for c in self.certs.lock_recover().iter() {
            out.push_str(&json!({ "cert": cert_json(c) }).to_string());
            out.push('\n');
        }
        let loc = self.local.lock_recover();
        out.push_str(
            &json!({ "state": {
                "promised": loc.promised.as_ref().map(proposal_json),
                "last": loc.last.as_ref().map(cp_json),
            } })
            .to_string(),
        );
        out.push('\n');
        for q in &loc.proofs {
            out.push_str(&json!({ "proof": text::equivocation_to_json(q) }).to_string());
            out.push('\n');
        }
        let tmp = p.with_extension("tmp");
        if let Err(e) = std::fs::write(&tmp, out).and_then(|()| std::fs::rename(&tmp, p)) {
            eprintln!("room log not saved to {}: {e}", p.display());
        }
    }

    /// Leader: start the log. `root` is the room's root key (the genesis is signed by it).
    ///
    /// # Errors
    ///
    /// Fails if a log exists, or the host set is invalid.
    pub fn start(
        &self,
        root: &SigningKey,
        hosts: Vec<[u8; 32]>,
        k: usize,
        now_ms: u64,
    ) -> Result<()> {
        let mut g = self.log.lock_recover();
        if g.is_some() {
            bail!("the room already has a log");
        }
        let me = self.me();
        let mut set: std::collections::BTreeSet<[u8; 32]> = hosts.into_iter().collect();
        set.insert(me);
        let cfg = Config {
            epoch: 0,
            leader: me,
            hosts: set,
            k,
        };
        *g = Some(RoomLog::genesis(root, cfg, now_ms).map_err(|e| anyhow!("{e}"))?);
        drop(g);
        self.save();
        Ok(())
    }

    /// Leader: append a membership event. A no-op if this host has no log or is not the leader.
    pub fn record(&self, kind: &str, member: &str, now_ms: u64) {
        let mut g = self.log.lock_recover();
        if let Some(l) = g.as_mut() {
            let _ = l.append(&self.key, kind, &json!({ "member": member }), now_ms);
        }
        drop(g);
        self.save();
    }

    #[must_use]
    pub fn status(&self) -> Option<Status> {
        let g = self.log.lock_recover();
        let l = g.as_ref()?;
        Some(status_of(
            l,
            &self.certs.lock_recover(),
            &self.local.lock_recover().proofs,
        ))
    }

    /// The log and certificates as JSON, for a guest to check from the room key (the `playbox.GetHosts` reply).
    #[must_use]
    pub fn snapshot(&self) -> Value {
        let g = self.log.lock_recover();
        let Some(l) = g.as_ref() else {
            return json!({ "root": roomlog::hex(&self.room), "entries": [], "certs": [] });
        };
        json!({
            "root": roomlog::hex(&self.room),
            "entries": l.entries().iter().map(text::entry_to_json).collect::<Vec<_>>(),
            "certs": self.certs.lock_recover().iter().map(cert_json).collect::<Vec<_>>(),
        })
    }

    /// Whether this host leads the current epoch.
    #[must_use]
    pub fn is_leader(&self) -> bool {
        self.log
            .lock_recover()
            .as_ref()
            .is_some_and(|l| l.config().leader == self.me())
    }

    /// How long since the leader last asked this host to cosign. A host that has not heard yet counts from when it started.
    #[must_use]
    pub fn silence(&self) -> Duration {
        self.heard.lock_recover().elapsed()
    }

    /// The program this host runs to measure a candidate: called with the problem hash and the candidate hash
    /// (64 hex characters each), it prints one JSON line `{"metric": micro-units, "weights": hex, "env": hex,
    /// "tier": n}`. A host without one cannot sign receipts.
    pub fn set_evaluator(&self, program: Option<std::path::PathBuf>) {
        *self.evaluator.lock_recover() = program;
    }

    /// Run this host's own measurement and sign it. Only for a problem this host's log has registered.
    async fn measure(&self, problem: [u8; 32], candidate: [u8; 32]) -> Result<(Receipt, [u8; 32])> {
        let prog = self
            .evaluator
            .lock_recover()
            .clone()
            .ok_or_else(|| anyhow!("this host has no evaluator"))?;
        {
            let g = self.log.lock_recover();
            let l = g.as_ref().ok_or_else(|| anyhow!("no room log"))?;
            if !l.frontier(None).contains_key(&problem) {
                bail!("that problem is not registered in this host's log");
            }
        }
        let _one = self.measuring.lock().await;
        let out = tokio::time::timeout(
            Duration::from_secs(900),
            tokio::process::Command::new(&prog)
                .arg(roomlog::hex(&problem))
                .arg(roomlog::hex(&candidate))
                .kill_on_drop(true)
                .output(),
        )
        .await
        .map_err(|_| anyhow!("the evaluator took more than 15 minutes"))?
        .with_context(|| format!("running {}", prog.display()))?;
        if !out.status.success() {
            bail!(
                "the evaluator failed: {}",
                String::from_utf8_lossy(&out.stderr)
                    .trim()
                    .chars()
                    .take(300)
                    .collect::<String>()
            );
        }
        let line = String::from_utf8_lossy(&out.stdout);
        let v: Value = serde_json::from_str(line.lines().last().unwrap_or(""))
            .context("the evaluator printed no JSON line")?;
        let bytes = |k: &str| {
            v[k].as_str()
                .and_then(roomlog::unhex::<32>)
                .ok_or_else(|| anyhow!("the evaluator's `{k}` is not 64 hex characters"))
        };
        let (weights, env) = (bytes("weights")?, bytes("env")?);
        let metric = v["metric"]
            .as_i64()
            .filter(|m| *m > 0)
            .ok_or_else(|| anyhow!("the evaluator's `metric` is not a positive integer"))?;
        let tier = u8::try_from(v["tier"].as_u64().unwrap_or(0)).unwrap_or(0);
        Ok((
            Receipt::sign(
                &self.room, &self.key, &problem, &candidate, &weights, env, tier, metric,
            ),
            weights,
        ))
    }

    /// Leader: register a problem and its rule in the log.
    ///
    /// # Errors
    ///
    /// Fails if this host is not the leader or the rule is refused (`j` under 2, or more than the hosts).
    pub fn register_problem(&self, reg: &Register, now_ms: u64) -> Result<()> {
        let mut g = self.log.lock_recover();
        let l = g.as_mut().ok_or_else(|| anyhow!("no room log"))?;
        l.register_problem(&self.key, reg, now_ms)
            .map_err(|e| anyhow!("{e}"))?;
        drop(g);
        self.save();
        Ok(())
    }

    /// Leader: measure `candidate` here, ask `peers` to measure it, and record a promotion if the problem's rule
    /// is met. Receipts measured with other weights than the leader's are left out. Returns the frontier metric.
    ///
    /// # Errors
    ///
    /// Fails if this host is not the leader or has no evaluator, or too few hosts agree; the error says which rule failed.
    pub async fn promote(
        &self,
        endpoint: &Endpoint,
        peers: &[EndpointAddr],
        problem: [u8; 32],
        candidate: [u8; 32],
    ) -> Result<i64> {
        if !self.is_leader() {
            bail!("only the leader records promotions");
        }
        let (mine, weights) = self.measure(problem, candidate).await?;
        let req = json!({
            "op": "receipt",
            "problem": roomlog::hex(&problem),
            "candidate": roomlog::hex(&candidate),
        });
        let mut receipts = vec![mine];
        let mut why = Vec::new();
        for (peer, reply) in ask_all(endpoint, peers, &req).await {
            match reply {
                Ok(v) => match (
                    receipt_from_json(&v["receipt"]),
                    v["weights"].as_str().and_then(roomlog::unhex::<32>),
                ) {
                    (Some(r), Some(w)) if w == weights => receipts.push(r),
                    (Some(_), _) => why.push(format!(
                        "{}: measured with other weights",
                        peer.id.fmt_short()
                    )),
                    _ => why.push(format!("{}: no receipt", peer.id.fmt_short())),
                },
                Err(e) => why.push(format!("{}: {e:#}", peer.id.fmt_short())),
            }
        }
        // Hosts that disagree with the rest beyond the problem's noise are left out of the promotion; what is
        // left must still be enough, which the log checks again on every host.
        let noise = {
            let g = self.log.lock_recover();
            g.as_ref()
                .and_then(|l| l.frontier(None).get(&problem).map(|(r, _)| r.noise_ppm))
                .unwrap_or(0)
        };
        let before = receipts.len();
        let receipts = agreeing(receipts, noise);
        if receipts.len() < before {
            why.push(format!(
                "{} receipt(s) differed from the rest by more than {noise} ppm",
                before - receipts.len()
            ));
        }
        let p = Promotion {
            problem,
            candidate,
            weights,
            receipts,
        };
        let mut g = self.log.lock_recover();
        let l = g.as_mut().ok_or_else(|| anyhow!("no room log"))?;
        l.promote(&self.key, &p, now_ms()).map_err(|e| {
            anyhow!(
                "{e} (from peers: {})",
                if why.is_empty() {
                    "all answered".to_string()
                } else {
                    why.join("; ")
                }
            )
        })?;
        let metric = l.frontier(None)[&problem].1.metric;
        drop(g);
        self.save();
        Ok(metric)
    }

    /// Set how long the leader must have been silent before this host signs someone's bid to replace it.
    pub fn set_sticky(&self, d: Duration) {
        *self.sticky.lock_recover() = d;
    }

    /// This host's place among the room's hosts (0 first). Bids are staggered by it so rivals rarely collide.
    #[must_use]
    pub fn rank(&self) -> usize {
        let g = self.log.lock_recover();
        let me = self.me();
        g.as_ref()
            .and_then(|l| l.config().hosts.iter().position(|h| *h == me))
            .unwrap_or(0)
    }

    /// Leader: checkpoint the log, ask each peer to replay it and cosign, and keep the certificate. Returns how many
    /// entries are committed afterwards.
    ///
    /// # Errors
    ///
    /// Fails if this host has no log, is not the leader, or has signed a bid for a later epoch. A peer that is
    /// down or refuses is skipped.
    pub async fn commit(&self, endpoint: &Endpoint, peers: &[EndpointAddr]) -> Result<u64> {
        let (entries, mut sc, cfg) = {
            let g = self.log.lock_recover();
            let l = g.as_ref().ok_or_else(|| anyhow!("no room log"))?;
            if l.config().leader != self.me() {
                bail!("this host is not the leader of epoch {}", l.config().epoch);
            }
            if self
                .local
                .lock_recover()
                .promised
                .as_ref()
                .is_some_and(|p| p.epoch > l.config().epoch)
            {
                bail!("this host has signed a bid for a later epoch; it no longer leads");
            }
            let sc = l
                .sign_checkpoint(&self.key, l.checkpoint())
                .map_err(|e| anyhow!("{e}"))?;
            (l.entries().to_vec(), sc, l.config().clone())
        };
        let req = json!({
            "op": "cosign",
            "entries": entries.iter().map(text::entry_to_json).collect::<Vec<_>>(),
            "cp": cp_json(&sc.cp),
        });
        for (_, reply) in ask_all(endpoint, peers, &req).await {
            let Ok(reply) = reply else { continue };
            let (Some(who), Some(sig)) = (
                reply["signer"].as_str().and_then(roomlog::unhex::<32>),
                reply["sig"].as_str().and_then(roomlog::unhex::<64>),
            ) else {
                continue;
            };
            sc.add(&self.room, &cfg, who, sig);
        }
        {
            let mut certs = self.certs.lock_recover();
            match certs.iter_mut().find(|c| c.cp == sc.cp) {
                Some(have) => {
                    for (k, s) in &sc.sigs {
                        have.add(&self.room, &cfg, *k, *s);
                    }
                }
                None => certs.push(sc),
            }
        }
        self.save();
        let certs = self.certs.lock_recover();
        let g = self.log.lock_recover();
        Ok(g.as_ref().map_or(0, |l| l.committed(&certs)))
    }

    /// Take entries from a peer. Before checking them, look for two different entries at one position from one
    /// key: that is a proof, kept even though the entries are then refused.
    fn adopt(&self, entries: Vec<Entry>) -> Result<()> {
        let mut g = self.log.lock_recover();
        let r = match g.as_mut() {
            Some(l) => {
                if let Some(q) = l.find_equivocation(&entries) {
                    let mut loc = self.local.lock_recover();
                    if !loc.proofs.contains(&q) {
                        loc.proofs.push(q);
                    }
                }
                l.accept_all(entries).map(|_| ())
            }
            None => {
                let mut f = RoomLog::follower(self.room);
                let r = f.accept_all(entries).map(|_| ());
                *g = f.into_log();
                r
            }
        };
        // The hosts the signed log names are the room's hosts: serve them without being told twice.
        if let Some(l) = g.as_ref() {
            let mut allowed = self.allowed.lock_recover();
            for h in &l.config().hosts {
                if let Ok(id) = EndpointId::from_bytes(h) {
                    allowed.insert(id);
                }
            }
        }
        drop(g);
        self.save();
        r.map_err(|e| anyhow!("{e}"))
    }

    fn entries_from(v: &Value) -> Result<Vec<Entry>> {
        let mut entries = Vec::new();
        for e in v.as_array().ok_or_else(|| anyhow!("no entries"))? {
            entries.push(text::entry_from_json(e).ok_or_else(|| anyhow!("bad entry"))?);
        }
        Ok(entries)
    }

    /// Answer a request. A receipt request runs this host's evaluator, which takes a while, so it is async.
    async fn handle_async(&self, req: &Value) -> Result<Value> {
        if req["op"].as_str() != Some("receipt") {
            return self.handle(req);
        }
        let h = |k: &str| {
            req[k]
                .as_str()
                .and_then(roomlog::unhex::<32>)
                .ok_or_else(|| anyhow!("`{k}` is not 64 hex characters"))
        };
        let (r, w) = self.measure(h("problem")?, h("candidate")?).await?;
        Ok(json!({ "receipt": receipt_json(&r), "weights": roomlog::hex(&w) }))
    }

    fn handle(&self, req: &Value) -> Result<Value> {
        match req["op"].as_str() {
            Some("cosign") => {
                let entries = Self::entries_from(&req["entries"])?;
                let cp = cp_from_json(&req["cp"]).ok_or_else(|| anyhow!("bad checkpoint"))?;
                self.adopt(entries)?;
                let g = self.log.lock_recover();
                let l = g.as_ref().ok_or_else(|| anyhow!("no log"))?;
                let mut loc = self.local.lock_recover();
                if loc.promised.as_ref().is_some_and(|p| p.epoch > cp.epoch) {
                    bail!("this host has signed a bid for a later epoch");
                }
                let (who, sig) = l.cosign(&self.key, &cp).map_err(|e| anyhow!("{e}"))?;
                if loc
                    .last
                    .as_ref()
                    .is_none_or(|c| (c.epoch, c.size) < (cp.epoch, cp.size))
                {
                    loc.last = Some(cp);
                }
                drop(loc);
                drop(g);
                *self.heard.lock_recover() = Instant::now();
                self.save();
                Ok(json!({ "signer": roomlog::hex(&who), "sig": roomlog::hex(&sig) }))
            }
            Some("pull") => {
                let g = self.log.lock_recover();
                let l = g.as_ref().ok_or_else(|| anyhow!("no log"))?;
                let from = req["from"].as_u64().unwrap_or(0);
                let certs = self.certs.lock_recover();
                Ok(json!({
                    "entries": l.since(from, 1000).iter().map(text::entry_to_json).collect::<Vec<_>>(),
                    "certs": certs.iter().map(cert_json).collect::<Vec<_>>(),
                }))
            }
            Some("propose") => {
                let entries = Self::entries_from(&req["entries"])?;
                let p =
                    proposal_from_json(&req["proposal"]).ok_or_else(|| anyhow!("bad proposal"))?;
                let mut f = RoomLog::follower(self.room);
                f.accept_all(entries).map_err(|e| anyhow!("{e}"))?;
                let theirs = f.into_log().ok_or_else(|| anyhow!("no entries"))?;
                // Replacing a leader this host still hears is refused; the leader asking to change the host set is not that.
                if p.leader != theirs.config().leader
                    && self.silence() < *self.sticky.lock_recover()
                {
                    bail!("this host has heard from the leader too recently to replace it");
                }
                let mut g = self.log.lock_recover();
                // A host being added has no log yet: it takes the proposer's, which it has just checked from the root key.
                let mine = g.get_or_insert_with(|| theirs.clone());
                let mut loc = self.local.lock_recover();
                let (who, sig) = mine
                    .cosign_proposal(
                        &self.key,
                        &theirs,
                        &p,
                        loc.last.as_ref(),
                        loc.promised.as_ref(),
                    )
                    .map_err(|e| anyhow!("{e}"))?;
                loc.promised = Some(p);
                drop(loc);
                drop(g);
                // The promise is on disk before the signature leaves this host.
                self.save();
                Ok(json!({ "signer": roomlog::hex(&who), "sig": roomlog::hex(&sig) }))
            }
            _ => bail!("unknown op"),
        }
    }

    /// Follower: bid to lead the next epoch. Asks `peers` to sign after replaying this host's log; with `k`
    /// signatures (its own included) it appends the epoch entry and leads. Returns the new epoch.
    ///
    /// # Errors
    ///
    /// Fails if there is no log, this host already leads, or fewer than `k` hosts signed.
    pub async fn propose(&self, endpoint: &Endpoint, peers: &[EndpointAddr]) -> Result<u64> {
        let me = self.me();
        let p = {
            let g = self.log.lock_recover();
            let l = g.as_ref().ok_or_else(|| anyhow!("no room log"))?;
            if l.config().leader == me {
                bail!("this host already leads");
            }
            // Above any epoch this host already signed a bid for, so a bid that lost to a rival can be tried again.
            l.proposal_at(me, self.next_epoch())
        };
        self.bid(endpoint, peers, p).await
    }

    /// Leader: change the host set (add `add`, drop `remove`; this host stays) with a threshold of more than half
    /// of the result. Needs `k` signatures from the old set and `k` from the new one (a host in both counts in
    /// both). Rotating a host's key is dropping the old one and adding the new. Returns the new epoch.
    ///
    /// # Errors
    ///
    /// Fails if there is no log, this host does not lead, the result is not a valid host set, or a quorum is missing.
    pub async fn reconfigure(
        &self,
        endpoint: &Endpoint,
        peers: &[EndpointAddr],
        add: &[[u8; 32]],
        remove: &[[u8; 32]],
    ) -> Result<u64> {
        let me = self.me();
        let p = {
            let g = self.log.lock_recover();
            let l = g.as_ref().ok_or_else(|| anyhow!("no room log"))?;
            if l.config().leader != me {
                bail!("only the leader can change the host set");
            }
            let mut hosts = l.config().hosts.clone();
            hosts.extend(add.iter().copied());
            for r in remove {
                hosts.remove(r);
            }
            if !hosts.contains(&me) {
                bail!("the leader cannot remove itself; hand over first");
            }
            let k = hosts.len() / 2 + 1;
            l.proposal_for(me, self.next_epoch(), hosts, k)
        };
        self.bid(endpoint, peers, p).await
    }

    fn next_epoch(&self) -> u64 {
        self.local
            .lock_recover()
            .promised
            .as_ref()
            .map_or(0, |q| q.epoch + 1)
    }

    /// Sign `p` for this host, ask `peers` to sign it, and append the epoch entry if enough did.
    async fn bid(&self, endpoint: &Endpoint, peers: &[EndpointAddr], p: Proposal) -> Result<u64> {
        let (entries, mut sigs) = {
            let g = self.log.lock_recover();
            let l = g.as_ref().ok_or_else(|| anyhow!("no room log"))?;
            let mut loc = self.local.lock_recover();
            let (who, sig) = l
                .cosign_proposal(&self.key, l, &p, loc.last.as_ref(), loc.promised.as_ref())
                .map_err(|e| anyhow!("{e}"))?;
            loc.promised = Some(p.clone());
            let mut sigs = std::collections::BTreeMap::new();
            sigs.insert(who, sig);
            (l.entries().to_vec(), sigs)
        };
        self.save();
        let req = json!({
            "op": "propose",
            "entries": entries.iter().map(text::entry_to_json).collect::<Vec<_>>(),
            "proposal": proposal_json(&p),
        });
        let mut why = Vec::new();
        for (peer, reply) in ask_all(endpoint, peers, &req).await {
            let reply = match reply {
                Ok(r) => r,
                Err(e) => {
                    why.push(format!("{}: {e:#}", peer.id.fmt_short()));
                    continue;
                }
            };
            if let (Some(who), Some(sig)) = (
                reply["signer"].as_str().and_then(roomlog::unhex::<32>),
                reply["sig"].as_str().and_then(roomlog::unhex::<64>),
            ) {
                sigs.insert(who, sig);
            }
        }
        let mut g = self.log.lock_recover();
        let l = g.as_mut().ok_or_else(|| anyhow!("no room log"))?;
        l.new_epoch(&self.key, &p, &sigs, now_ms()).map_err(|e| {
            anyhow!(
                "{e}: {} signature(s) for epoch {} ({})",
                sigs.len(),
                p.epoch,
                why.join("; ")
            )
        })?;
        drop(g);
        self.save();
        *self.heard.lock_recover() = Instant::now();
        Ok(p.epoch)
    }

    /// Follower: pull entries and certificates from `peer`, checking all of it. Returns the new commit point.
    ///
    /// # Errors
    ///
    /// Fails if the peer cannot be reached or sends an entry that does not check.
    pub async fn pull(&self, endpoint: &Endpoint, peer: &EndpointAddr) -> Result<u64> {
        // Ask from the committed point: what is past it may be an old leader's tail that a new epoch replaces.
        let from = {
            let g = self.log.lock_recover();
            g.as_ref()
                .map_or(0, |l| l.committed(&self.certs.lock_recover()))
        };
        let reply = ask(endpoint, peer, &json!({ "op": "pull", "from": from })).await?;
        self.adopt(Self::entries_from(&reply["entries"])?)?;
        let g = self.log.lock_recover();
        if g.as_ref()
            .is_some_and(|l| l.config().leader == *peer.id.as_bytes())
        {
            // The leader itself answered: it is alive even if it has nothing to commit.
            *self.heard.lock_recover() = Instant::now();
        }
        let Some(l) = g.as_ref() else { return Ok(0) };
        let mut certs = self.certs.lock_recover();
        for c in reply["certs"].as_array().into_iter().flatten() {
            if let Some(sc) = cert_from_json(c) {
                match certs.iter_mut().find(|x| x.cp == sc.cp) {
                    Some(have) => {
                        l.merge(have, &sc);
                    }
                    None => {
                        let mut fresh = SignedCheckpoint::new(sc.cp.clone());
                        l.merge(&mut fresh, &sc);
                        certs.push(fresh);
                    }
                }
            }
        }
        let n = l.committed(&certs);
        drop(certs);
        drop(g);
        self.save();
        Ok(n)
    }
}

/// Read a saved log (entries, certificates, promises, proofs), re-verifying all of it.
fn load(
    room: [u8; 32],
    path: &std::path::Path,
) -> Result<(Option<RoomLog>, Vec<SignedCheckpoint>, Local)> {
    let raw =
        std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
    let (mut entries, mut cert_lines, mut local) = (String::new(), Vec::new(), Local::default());
    let mut proofs = Vec::new();
    for line in raw.lines().filter(|l| !l.trim().is_empty()) {
        match serde_json::from_str::<Value>(line) {
            Ok(v) if v.get("cert").is_some() => cert_lines.push(v["cert"].clone()),
            Ok(v) if v.get("state").is_some() => {
                local.promised = proposal_from_json(&v["state"]["promised"]);
                local.last = cp_from_json(&v["state"]["last"]);
            }
            Ok(v) if v.get("proof").is_some() => {
                if let Some(q) = text::equivocation_from_json(&v["proof"]) {
                    proofs.push(q);
                }
            }
            _ => {
                entries.push_str(line);
                entries.push('\n');
            }
        }
    }
    let log = text::from_text(room, &entries).map_err(|e| {
        anyhow!(
            "{}: {e}; refusing to start on a log that does not check",
            path.display()
        )
    })?;
    // A saved proof is believed no more than one heard from a peer.
    local.proofs = proofs
        .into_iter()
        .filter(|q| q.verify(&room).is_ok())
        .collect();
    let mut certs = Vec::new();
    if let Some(l) = &log {
        for c in cert_lines {
            if let Some(sc) = cert_from_json(&c) {
                let mut fresh = SignedCheckpoint::new(sc.cp.clone());
                l.merge(&mut fresh, &sc);
                certs.push(fresh);
            }
        }
    }
    Ok((log, certs, local))
}

/// What a saved room log says, checked from scratch (for `playbox room-log`).
///
/// # Errors
///
/// Fails if the file is unreadable or any entry or certificate does not check.
pub fn read_status(room: [u8; 32], path: &std::path::Path) -> Result<Option<Status>> {
    let (log, certs, local) = load(room, path)?;
    Ok(log.map(|l| status_of(&l, &certs, &local.proofs)))
}

fn cp_json(c: &Checkpoint) -> Value {
    json!({ "epoch": c.epoch, "size": c.size, "head": roomlog::hex(&c.head) })
}

/// The largest group of receipts whose metrics are within `noise_ppm` of each other (of the group's largest).
fn agreeing(mut rs: Vec<Receipt>, noise_ppm: u64) -> Vec<Receipt> {
    rs.sort_by_key(|r| r.metric);
    let mut best = (0, 0);
    for i in 0..rs.len() {
        let mut j = i;
        while j + 1 < rs.len()
            && i128::from(rs[j + 1].metric - rs[i].metric) * 1_000_000
                <= i128::from(rs[j + 1].metric) * i128::from(noise_ppm)
        {
            j += 1;
        }
        if j + 1 - i > best.1 - best.0 {
            best = (i, j + 1);
        }
    }
    rs[best.0..best.1].to_vec()
}

fn receipt_json(r: &Receipt) -> Value {
    json!({
        "host": roomlog::hex(&r.host), "env": roomlog::hex(&r.env), "tier": r.tier,
        "metric": r.metric, "sig": roomlog::hex(&r.sig),
    })
}

fn receipt_from_json(v: &Value) -> Option<Receipt> {
    Some(Receipt {
        host: roomlog::unhex(v["host"].as_str()?)?,
        env: roomlog::unhex(v["env"].as_str()?)?,
        tier: u8::try_from(v["tier"].as_u64()?).ok()?,
        metric: v["metric"].as_i64()?,
        sig: roomlog::unhex(v["sig"].as_str()?)?,
    })
}

fn proposal_json(p: &Proposal) -> Value {
    json!({
        "epoch": p.epoch, "leader": roomlog::hex(&p.leader), "size": p.size, "head": roomlog::hex(&p.head),
        "k": p.k, "hosts": p.hosts.iter().map(|h| roomlog::hex(h)).collect::<Vec<_>>(),
    })
}

fn proposal_from_json(v: &Value) -> Option<Proposal> {
    Some(Proposal {
        epoch: v["epoch"].as_u64()?,
        leader: roomlog::unhex(v["leader"].as_str()?)?,
        size: v["size"].as_u64()?,
        head: roomlog::unhex(v["head"].as_str()?)?,
        k: usize::try_from(v["k"].as_u64()?).ok()?,
        hosts: v["hosts"]
            .as_array()?
            .iter()
            .map(|h| roomlog::unhex(h.as_str()?))
            .collect::<Option<_>>()?,
    })
}

fn cp_from_json(v: &Value) -> Option<Checkpoint> {
    Some(Checkpoint {
        epoch: v["epoch"].as_u64()?,
        size: v["size"].as_u64()?,
        head: roomlog::unhex(v["head"].as_str()?)?,
    })
}

fn cert_json(c: &SignedCheckpoint) -> Value {
    json!({
        "cp": cp_json(&c.cp),
        "sigs": c.sigs.iter().map(|(k, s)| (roomlog::hex(k), Value::from(roomlog::hex(s)))).collect::<serde_json::Map<_, _>>(),
    })
}

pub(crate) fn cert_from_json(v: &Value) -> Option<SignedCheckpoint> {
    let mut sc = SignedCheckpoint::new(cp_from_json(&v["cp"])?);
    for (k, s) in v["sigs"].as_object()? {
        // Stored unchecked here; `RoomLog::merge` re-verifies each signature before keeping it.
        sc.sigs
            .insert(roomlog::unhex(k)?, roomlog::unhex(s.as_str()?)?);
    }
    Some(sc)
}

/// Ask every peer at once, so one that is down costs a single timeout, not one each. Replies come back in `peers` order.
async fn ask_all(
    endpoint: &Endpoint,
    peers: &[EndpointAddr],
    req: &Value,
) -> Vec<(EndpointAddr, Result<Value>)> {
    let mut set = tokio::task::JoinSet::new();
    for (i, p) in peers.iter().cloned().enumerate() {
        let (ep, req) = (endpoint.clone(), req.clone());
        set.spawn(async move { (i, p.clone(), ask(&ep, &p, &req).await) });
    }
    let mut out = Vec::new();
    while let Some(Ok(r)) = set.join_next().await {
        out.push(r);
    }
    out.sort_by_key(|(i, ..)| *i);
    out.into_iter().map(|(_, p, r)| (p, r)).collect()
}

async fn ask(endpoint: &Endpoint, peer: &EndpointAddr, req: &Value) -> Result<Value> {
    let conn = tokio::time::timeout(
        Duration::from_secs(5),
        endpoint.connect(peer.clone(), ROOMLOG_ALPN),
    )
    .await
    .map_err(|_| anyhow!("timed out connecting to {}", peer.id.fmt_short()))??;
    let (mut send, mut recv) = conn.open_bi().await?;
    send.write_all(req.to_string().as_bytes()).await?;
    send.finish()?;
    let reply = recv.read_to_end(MAX_MSG).await?;
    conn.close(0u32.into(), b"done");
    let v: Value = serde_json::from_slice(&reply).context("bad reply")?;
    if let Some(e) = v["error"].as_str() {
        bail!("{e}");
    }
    Ok(v)
}

#[derive(Clone)]
pub(crate) struct RoomLogHandler {
    node: Arc<RoomLogNode>,
    conns: Arc<tokio::sync::Semaphore>,
}

impl RoomLogHandler {
    pub(crate) fn new(node: Arc<RoomLogNode>) -> Self {
        Self {
            node,
            conns: Arc::new(tokio::sync::Semaphore::new(16)),
        }
    }
}

impl std::fmt::Debug for RoomLogHandler {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("RoomLogHandler")
    }
}

impl ProtocolHandler for RoomLogHandler {
    async fn accept(&self, conn: Connection) -> Result<(), AcceptError> {
        const IO_TIMEOUT: Duration = Duration::from_secs(15);
        if !self.node.allowed.lock_recover().contains(&conn.remote_id()) {
            conn.close(1u32.into(), b"not a room peer");
            return Ok(());
        }
        let Ok(_slot) = self.conns.clone().try_acquire_owned() else {
            conn.close(2u32.into(), b"too many sync streams");
            return Ok(());
        };
        let Ok(Ok((mut send, mut recv))) = tokio::time::timeout(IO_TIMEOUT, conn.accept_bi()).await
        else {
            conn.close(1u32.into(), b"sync stream timed out");
            return Ok(());
        };
        let Ok(Ok(body)) = tokio::time::timeout(IO_TIMEOUT, recv.read_to_end(MAX_MSG)).await else {
            conn.close(1u32.into(), b"sync read timed out");
            return Ok(());
        };
        let out = match serde_json::from_slice::<Value>(&body) {
            Ok(r) => match self.node.handle_async(&r).await {
                Ok(v) => v,
                Err(e) => json!({ "error": format!("{e:#}") }),
            },
            Err(e) => json!({ "error": format!("{e}") }),
        };
        let _ = tokio::time::timeout(IO_TIMEOUT, send.write_all(out.to_string().as_bytes())).await;
        let _ = send.finish();
        let _ = tokio::time::timeout(IO_TIMEOUT, conn.closed()).await;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // REQ: ROOM-13
    #[test]
    fn a_host_shown_two_entries_at_one_position_keeps_the_proof_and_refuses_the_second() {
        let root = SigningKey::from_bytes(&[7; 32]);
        let leader = SigningKey::from_bytes(&[8; 32]);
        let room = root.verifying_key().to_bytes();
        let node =
            |key: &SigningKey| RoomLogNode::new(room, key.clone(), Arc::default(), None).unwrap();
        let (a, b, watcher) = (
            node(&leader),
            node(&leader),
            node(&SigningKey::from_bytes(&[9; 32])),
        );
        let hosts = vec![leader.verifying_key().to_bytes(); 1];
        for n in [&a, &b] {
            n.start(&root, hosts.clone(), 1, 5).unwrap();
        }
        // One leader, two stories for entry 1.
        a.record("member.join", "alice", 10);
        b.record("member.join", "bob", 10);
        let story = |n: &RoomLogNode| n.log.lock_recover().as_ref().unwrap().entries().to_vec();
        watcher.adopt(story(&a)).unwrap();
        assert!(
            watcher.adopt(story(&b)).is_err(),
            "the second story is refused"
        );
        let st = watcher.status().unwrap();
        assert_eq!(st.members, vec!["alice".to_string()]);
        assert_eq!(
            st.equivocators,
            vec![roomlog::hex(&leader.verifying_key().to_bytes())]
        );
    }

    // REQ: ROOM-12
    #[test]
    fn a_host_that_still_hears_the_leader_will_not_sign_a_bid_to_replace_it() {
        let root = SigningKey::from_bytes(&[7; 32]);
        let keys: Vec<SigningKey> = (10..13).map(|i| SigningKey::from_bytes(&[i; 32])).collect();
        let room = root.verifying_key().to_bytes();
        let hosts: Vec<[u8; 32]> = keys.iter().map(|k| k.verifying_key().to_bytes()).collect();
        let node =
            |i: usize| RoomLogNode::new(room, keys[i].clone(), Arc::default(), None).unwrap();
        let (leader, bidder, voter) = (node(0), node(1), node(2));
        leader.start(&root, hosts, 2, 5).unwrap();
        let entries = leader
            .log
            .lock_recover()
            .as_ref()
            .unwrap()
            .entries()
            .to_vec();
        for n in [&bidder, &voter] {
            n.adopt(entries.clone()).unwrap();
        }
        let p = bidder
            .log
            .lock_recover()
            .as_ref()
            .unwrap()
            .proposal(hosts_of(&keys, 1));
        let req = json!({
            "op": "propose",
            "entries": entries.iter().map(text::entry_to_json).collect::<Vec<_>>(),
            "proposal": proposal_json(&p),
        });
        voter.set_sticky(Duration::from_secs(60));
        let e = voter.handle(&req).unwrap_err().to_string();
        assert!(e.contains("too recently"), "{e}");
        voter.set_sticky(Duration::ZERO);
        assert!(voter.handle(&req).is_ok());
    }

    fn hosts_of(keys: &[SigningKey], i: usize) -> [u8; 32] {
        keys[i].verifying_key().to_bytes()
    }
}
