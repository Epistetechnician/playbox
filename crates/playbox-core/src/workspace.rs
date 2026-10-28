//! Versioned workspaces, adopting the useful ideas from LayerFS (Ephemeral-AI-Lab/layerfs):
//! a commit per tool call, cheap branch/fork, content-defined chunking so an edit costs the
//! size of the edit, content-addressed dedup, refuse-on-conflict with `.lww` (newest wins) rules.
//!
//! Pure Rust, no FUSE and no VM: this is the data model. Mounting a workspace into a microVM
//! is a separate layer. Commit tree ids are what the history log records as `fs_snapshot`.

use std::{
    collections::{BTreeMap, HashMap},
    fs,
    io::{self, ErrorKind},
    path::PathBuf,
};
use thiserror::Error;

pub type Id = [u8; 32];

const MIN_CHUNK: usize = 2 * 1024;
const MAX_CHUNK: usize = 64 * 1024;
const MASK: u64 = (1 << 13) - 1; // ~8 KiB average

fn gear() -> [u64; 256] {
    let mut t = [0u64; 256];
    let mut s = 0x9E37_79B9_7F4A_7C15u64;
    for v in t.iter_mut() {
        s = s.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = s;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        *v = z ^ (z >> 31);
    }
    t
}

/// Content-defined chunking: boundaries depend on the last ~13 bytes only, so an edit
/// changes the chunks around it and the rest resynchronise.
pub fn chunk(data: &[u8]) -> Vec<&[u8]> {
    let g = gear();
    let mut out = Vec::new();
    let mut start = 0;
    while start < data.len() {
        let end = (start + MAX_CHUNK).min(data.len());
        let (mut h, mut i, mut cut) = (0u64, start, end);
        while i < end {
            h = (h << 1).wrapping_add(g[data[i] as usize]);
            i += 1;
            if i - start >= MIN_CHUNK && h & MASK == 0 {
                cut = i;
                break;
            }
        }
        out.push(&data[start..cut]);
        start = cut;
    }
    out
}

fn hash(bytes: &[u8]) -> Id {
    *blake3::hash(bytes).as_bytes()
}

#[derive(Debug, Error)]
pub enum Error {
    #[error("conflict on {0:?}")]
    Conflict(Vec<String>),
    #[error("unknown branch {0}")]
    NoBranch(String),
    #[error("branch {0} already exists")]
    BranchExists(String),
    #[error("missing object")]
    Missing,
    #[error("corrupt object")]
    Corrupt,
    #[error(transparent)]
    Io(#[from] io::Error),
}

/// Content-addressed object store, in memory or in a directory. Objects are never rewritten.
#[derive(Debug)]
pub struct Store {
    dir: Option<PathBuf>,
    mem: HashMap<Id, Vec<u8>>,
    stored_bytes: u64,
}

impl Store {
    pub fn memory() -> Self {
        Self { dir: None, mem: HashMap::new(), stored_bytes: 0 }
    }

    pub fn open(dir: impl Into<PathBuf>) -> io::Result<Self> {
        let dir = dir.into();
        fs::create_dir_all(&dir)?;
        Ok(Self { dir: Some(dir), mem: HashMap::new(), stored_bytes: 0 })
    }

    fn path(&self, id: &Id) -> Option<PathBuf> {
        self.dir.as_ref().map(|d| {
            let hex: String = id.iter().map(|b| format!("{b:02x}")).collect();
            d.join(&hex[..2]).join(&hex[2..])
        })
    }

    pub fn put(&mut self, bytes: &[u8]) -> io::Result<Id> {
        let id = hash(bytes);
        match self.path(&id) {
            Some(p) => {
                if !p.exists() {
                    fs::create_dir_all(p.parent().expect("has parent"))?;
                    let tmp = p.with_extension("tmp");
                    fs::write(&tmp, bytes)?;
                    fs::rename(tmp, &p)?;
                    self.stored_bytes += bytes.len() as u64;
                }
            }
            None => {
                if self.mem.insert(id, bytes.to_vec()).is_none() {
                    self.stored_bytes += bytes.len() as u64;
                }
            }
        }
        Ok(id)
    }

    pub fn get(&self, id: &Id) -> Result<Vec<u8>, Error> {
        let bytes = match self.path(id) {
            Some(p) => fs::read(p).map_err(|e| if e.kind() == ErrorKind::NotFound { Error::Missing } else { e.into() })?,
            None => self.mem.get(id).cloned().ok_or(Error::Missing)?,
        };
        if hash(&bytes) != *id {
            return Err(Error::Corrupt);
        }
        Ok(bytes)
    }

    /// Bytes of new objects written through this handle (dedup shows up as not growing).
    pub fn stored_bytes(&self) -> u64 {
        self.stored_bytes
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub len: u64,
    pub chunks: Vec<Id>,
}

pub type Tree = BTreeMap<String, Entry>;

fn encode_tree(t: &Tree) -> Vec<u8> {
    let mut b = Vec::new();
    for (path, e) in t {
        b.extend_from_slice(&(path.len() as u32).to_be_bytes());
        b.extend_from_slice(path.as_bytes());
        b.extend_from_slice(&e.len.to_be_bytes());
        b.extend_from_slice(&(e.chunks.len() as u32).to_be_bytes());
        for c in &e.chunks {
            b.extend_from_slice(c);
        }
    }
    b
}

struct Reader<'a>(&'a [u8]);

impl<'a> Reader<'a> {
    fn take(&mut self, n: usize) -> Result<&'a [u8], Error> {
        if self.0.len() < n {
            return Err(Error::Corrupt);
        }
        let (a, b) = self.0.split_at(n);
        self.0 = b;
        Ok(a)
    }
    fn u32(&mut self) -> Result<u32, Error> {
        Ok(u32::from_be_bytes(self.take(4)?.try_into().expect("4 bytes")))
    }
    fn u64(&mut self) -> Result<u64, Error> {
        Ok(u64::from_be_bytes(self.take(8)?.try_into().expect("8 bytes")))
    }
    fn id(&mut self) -> Result<Id, Error> {
        Ok(self.take(32)?.try_into().expect("32 bytes"))
    }
    fn done(&self) -> bool {
        self.0.is_empty()
    }
}

fn decode_tree(bytes: &[u8]) -> Result<Tree, Error> {
    let mut r = Reader(bytes);
    let mut t = Tree::new();
    while !r.done() {
        let n = r.u32()? as usize;
        let path = String::from_utf8(r.take(n)?.to_vec()).map_err(|_| Error::Corrupt)?;
        let len = r.u64()?;
        let count = r.u32()? as usize;
        let chunks = (0..count).map(|_| r.id()).collect::<Result<Vec<_>, _>>()?;
        t.insert(path, Entry { len, chunks });
    }
    Ok(t)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Commit {
    pub tree: Id,
    pub parents: Vec<Id>,
    pub author: String,
    pub message: String,
    /// The agent tool call that produced this commit, if any.
    pub tool_call: String,
    pub time_ms: u64,
}

fn encode_commit(c: &Commit) -> Vec<u8> {
    let mut b = Vec::new();
    b.extend_from_slice(&c.tree);
    b.extend_from_slice(&(c.parents.len() as u32).to_be_bytes());
    for p in &c.parents {
        b.extend_from_slice(p);
    }
    b.extend_from_slice(&c.time_ms.to_be_bytes());
    for s in [&c.author, &c.message, &c.tool_call] {
        b.extend_from_slice(&(s.len() as u32).to_be_bytes());
        b.extend_from_slice(s.as_bytes());
    }
    b
}

fn decode_commit(bytes: &[u8]) -> Result<Commit, Error> {
    let mut r = Reader(bytes);
    let tree = r.id()?;
    let n = r.u32()? as usize;
    let parents = (0..n).map(|_| r.id()).collect::<Result<Vec<_>, _>>()?;
    let time_ms = r.u64()?;
    let mut s = || -> Result<String, Error> {
        let n = r.u32()? as usize;
        String::from_utf8(r.take(n)?.to_vec()).map_err(|_| Error::Corrupt)
    };
    let (author, message, tool_call) = (s()?, s()?, s()?);
    Ok(Commit { tree, parents, author, message, tool_call, time_ms })
}

/// Does `path` match a `.lww` rule? Rules: exact path, `*.ext` suffix, or `dir/` prefix.
fn lww_match(rules: &[String], path: &str) -> bool {
    rules.iter().any(|r| match (r.strip_prefix('*'), r.ends_with('/')) {
        (Some(suffix), _) => path.ends_with(suffix),
        (None, true) => path.starts_with(r.as_str()),
        _ => path == r,
    })
}

#[derive(Debug)]
pub struct Repo {
    store: Store,
    branches: BTreeMap<String, Id>,
    /// Paths where the newest edit wins instead of conflicting.
    pub lww: Vec<String>,
}

/// A writable view over a branch's head at the time it was opened. Dropping it discards
/// uncommitted changes, never history.
#[derive(Debug)]
pub struct Workspace {
    branch: String,
    base: Id,
    /// `None` = deleted.
    overlay: BTreeMap<String, Option<Vec<u8>>>,
}

impl Workspace {
    pub fn write(&mut self, path: &str, data: &[u8]) {
        self.overlay.insert(path.to_string(), Some(data.to_vec()));
    }

    pub fn remove(&mut self, path: &str) {
        self.overlay.insert(path.to_string(), None);
    }

    pub fn branch(&self) -> &str {
        &self.branch
    }

    pub fn base(&self) -> Id {
        self.base
    }

    pub fn is_dirty(&self) -> bool {
        !self.overlay.is_empty()
    }
}

impl Repo {
    pub fn new(store: Store) -> Self {
        Self { store, branches: BTreeMap::new(), lww: Vec::new() }
    }

    pub fn store(&self) -> &Store {
        &self.store
    }

    pub fn head(&self, branch: &str) -> Result<Id, Error> {
        self.branches.get(branch).copied().ok_or_else(|| Error::NoBranch(branch.to_string()))
    }

    pub fn commit_info(&self, id: &Id) -> Result<Commit, Error> {
        decode_commit(&self.store.get(id)?)
    }

    pub fn tree(&self, commit: &Id) -> Result<Tree, Error> {
        decode_tree(&self.store.get(&self.commit_info(commit)?.tree)?)
    }

    pub fn read(&self, commit: &Id, path: &str) -> Result<Option<Vec<u8>>, Error> {
        let Some(e) = self.tree(commit)?.get(path).cloned() else { return Ok(None) };
        let mut out = Vec::with_capacity(e.len as usize);
        for c in &e.chunks {
            out.extend_from_slice(&self.store.get(c)?);
        }
        Ok(Some(out))
    }

    /// Create a branch with an initial commit from `files`.
    pub fn init(&mut self, branch: &str, files: &[(&str, &[u8])], author: &str, time_ms: u64) -> Result<Id, Error> {
        if self.branches.contains_key(branch) {
            return Err(Error::BranchExists(branch.to_string()));
        }
        let mut ws = Workspace { branch: branch.to_string(), base: [0; 32], overlay: BTreeMap::new() };
        for (p, d) in files {
            ws.write(p, d);
        }
        let tree = self.build_tree(&Tree::new(), &ws.overlay)?;
        let id = self.put_commit(tree, vec![], author, "init", "", time_ms)?;
        self.branches.insert(branch.to_string(), id);
        Ok(id)
    }

    /// Fork copies nothing: the new branch points at the same commit.
    pub fn fork(&mut self, new: &str, from: &str) -> Result<(), Error> {
        if self.branches.contains_key(new) {
            return Err(Error::BranchExists(new.to_string()));
        }
        let head = self.head(from)?;
        self.branches.insert(new.to_string(), head);
        Ok(())
    }

    pub fn workspace(&self, branch: &str) -> Result<Workspace, Error> {
        Ok(Workspace { branch: branch.to_string(), base: self.head(branch)?, overlay: BTreeMap::new() })
    }

    fn build_tree(&mut self, base: &Tree, overlay: &BTreeMap<String, Option<Vec<u8>>>) -> Result<Id, Error> {
        let mut t = base.clone();
        for (path, change) in overlay {
            match change {
                None => {
                    t.remove(path);
                }
                Some(data) => {
                    let chunks = chunk(data).into_iter().map(|c| self.store.put(c)).collect::<Result<Vec<_>, _>>()?;
                    t.insert(path.clone(), Entry { len: data.len() as u64, chunks });
                }
            }
        }
        Ok(self.store.put(&encode_tree(&t))?)
    }

    fn put_commit(&mut self, tree: Id, parents: Vec<Id>, author: &str, message: &str, tool_call: &str, time_ms: u64) -> Result<Id, Error> {
        let c = Commit { tree, parents, author: author.into(), message: message.into(), tool_call: tool_call.into(), time_ms };
        Ok(self.store.put(&encode_commit(&c))?)
    }

    /// Commit the workspace to its branch: one commit per tool call. If the branch moved since the
    /// workspace opened, paths changed on both sides conflict unless a `.lww` rule applies.
    /// On conflict nothing is committed and the workspace keeps its changes.
    pub fn commit(&mut self, ws: &mut Workspace, author: &str, message: &str, tool_call: &str, time_ms: u64) -> Result<Id, Error> {
        let head = self.head(&ws.branch)?;
        let head_tree = self.tree(&head)?;
        let base_tree = if ws.base == [0; 32] { Tree::new() } else { self.tree(&ws.base)? };

        if head != ws.base {
            let conflicts: Vec<String> = ws
                .overlay
                .iter()
                .filter(|(path, ours)| {
                    let theirs_changed = base_tree.get(*path) != head_tree.get(*path as &String);
                    theirs_changed && !lww_match(&self.lww, path) && !self.same_result(&head_tree, path, ours)
                })
                .map(|(p, _)| p.clone())
                .collect();
            if !conflicts.is_empty() {
                return Err(Error::Conflict(conflicts));
            }
        }
        let tree = self.build_tree(&head_tree, &ws.overlay)?;
        let id = self.put_commit(tree, vec![head], author, message, tool_call, time_ms)?;
        self.branches.insert(ws.branch.clone(), id);
        ws.base = id;
        ws.overlay.clear();
        Ok(id)
    }

    /// Both sides made the same change: not a conflict.
    fn same_result(&mut self, head_tree: &Tree, path: &str, ours: &Option<Vec<u8>>) -> bool {
        match (head_tree.get(path), ours) {
            (None, None) => true,
            (Some(theirs), Some(data)) => {
                let chunks: Option<Vec<Id>> = chunk(data).into_iter().map(|c| self.store.put(c).ok()).collect();
                chunks.is_some_and(|c| c == theirs.chunks)
            }
            _ => false,
        }
    }

    /// Every file as the workspace currently sees it: base tree plus uncommitted changes.
    pub fn files(&self, ws: &Workspace) -> Result<BTreeMap<String, Vec<u8>>, Error> {
        let mut out = BTreeMap::new();
        if ws.base != [0; 32] {
            for path in self.tree(&ws.base)?.keys() {
                if let Some(data) = self.read(&ws.base, path)? {
                    out.insert(path.clone(), data);
                }
            }
        }
        for (path, change) in &ws.overlay {
            match change {
                Some(data) => out.insert(path.clone(), data.clone()),
                None => out.remove(path),
            };
        }
        Ok(out)
    }

    /// Commits from `branch` head back to the first commit, newest first.
    pub fn log(&self, branch: &str) -> Result<Vec<(Id, Commit)>, Error> {
        let mut out = Vec::new();
        let mut cur = Some(self.head(branch)?);
        while let Some(id) = cur {
            let c = self.commit_info(&id)?;
            cur = c.parents.first().copied();
            out.push((id, c));
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::history::Log;
    use ed25519_dalek::SigningKey;

    fn noise(n: usize, seed: u64) -> Vec<u8> {
        let mut s = seed;
        (0..n)
            .map(|_| {
                s = s.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
                (s >> 33) as u8
            })
            .collect()
    }

    fn repo() -> Repo {
        Repo::new(Store::memory())
    }

    #[test]
    fn chunking_roundtrips_and_respects_bounds() {
        let data = noise(500_000, 1);
        let parts = chunk(&data);
        assert_eq!(parts.concat(), data);
        assert!(parts.iter().all(|p| p.len() <= MAX_CHUNK));
        assert!(parts[..parts.len() - 1].iter().all(|p| p.len() >= MIN_CHUNK));
    }

    #[test]
    fn small_edit_to_big_file_stores_little() {
        let mut r = repo();
        let big = noise(1 << 20, 7);
        r.init("main", &[("big.bin", &big)], "a", 1).unwrap();
        let before = r.store().stored_bytes();
        let mut edited = big.clone();
        edited[500_000..500_010].copy_from_slice(b"0123456789");
        let mut ws = r.workspace("main").unwrap();
        ws.write("big.bin", &edited);
        r.commit(&mut ws, "a", "edit", "t1", 2).unwrap();
        let added = r.store().stored_bytes() - before;
        assert!(added < 100_000, "stored {added} new bytes for a 10-byte edit of 1 MiB");
        assert_eq!(r.read(&r.head("main").unwrap(), "big.bin").unwrap().unwrap(), edited);
    }

    #[test]
    fn identical_content_is_stored_once_across_branches() {
        let mut r = repo();
        let data = noise(200_000, 3);
        r.init("main", &[("deps/lib.bin", &data)], "a", 1).unwrap();
        r.fork("agent2", "main").unwrap();
        let before = r.store().stored_bytes();
        let mut ws = r.workspace("agent2").unwrap();
        ws.write("copy/lib.bin", &data); // same bytes under another path
        r.commit(&mut ws, "b", "copy", "t", 2).unwrap();
        // only the new tree and commit objects (metadata); the 200 KB of chunks are reused
        assert!(r.store().stored_bytes() - before < 10_000, "chunks must be reused");
    }

    #[test]
    fn fork_is_free_and_branches_are_independent() {
        let mut r = repo();
        r.init("main", &[("a.txt", b"one")], "a", 1).unwrap();
        r.fork("try", "main").unwrap();
        let mut ws = r.workspace("try").unwrap();
        ws.write("a.txt", b"two");
        r.commit(&mut ws, "a", "change", "t", 2).unwrap();
        assert_eq!(r.read(&r.head("main").unwrap(), "a.txt").unwrap().unwrap(), b"one");
        assert_eq!(r.read(&r.head("try").unwrap(), "a.txt").unwrap().unwrap(), b"two");
    }

    #[test]
    fn concurrent_edits_to_different_files_merge() {
        let mut r = repo();
        r.init("main", &[("a", b"1"), ("b", b"1")], "x", 1).unwrap();
        let (mut w1, mut w2) = (r.workspace("main").unwrap(), r.workspace("main").unwrap());
        w1.write("a", b"2");
        w2.write("b", b"2");
        r.commit(&mut w1, "x", "a", "t1", 2).unwrap();
        r.commit(&mut w2, "y", "b", "t2", 3).unwrap();
        let h = r.head("main").unwrap();
        assert_eq!(r.read(&h, "a").unwrap().unwrap(), b"2");
        assert_eq!(r.read(&h, "b").unwrap().unwrap(), b"2");
    }

    #[test]
    fn concurrent_edits_to_same_file_conflict_and_keep_workspace() {
        let mut r = repo();
        r.init("main", &[("a", b"1")], "x", 1).unwrap();
        let (mut w1, mut w2) = (r.workspace("main").unwrap(), r.workspace("main").unwrap());
        w1.write("a", b"mine");
        w2.write("a", b"theirs");
        r.commit(&mut w1, "x", "a", "t1", 2).unwrap();
        let head = r.head("main").unwrap();
        match r.commit(&mut w2, "y", "a", "t2", 3) {
            Err(Error::Conflict(p)) => assert_eq!(p, vec!["a".to_string()]),
            other => panic!("expected conflict, got {other:?}"),
        }
        assert_eq!(r.head("main").unwrap(), head, "nothing committed");
        assert!(w2.is_dirty());
    }

    #[test]
    fn identical_concurrent_change_is_not_a_conflict() {
        let mut r = repo();
        r.init("main", &[("a", b"1")], "x", 1).unwrap();
        let (mut w1, mut w2) = (r.workspace("main").unwrap(), r.workspace("main").unwrap());
        w1.write("a", b"same");
        w2.write("a", b"same");
        r.commit(&mut w1, "x", "a", "t1", 2).unwrap();
        assert!(r.commit(&mut w2, "y", "a", "t2", 3).is_ok());
    }

    #[test]
    fn lww_rule_lets_newest_edit_win() {
        let mut r = repo();
        r.lww.push("*.log".into());
        r.init("main", &[("run.log", b"0")], "x", 1).unwrap();
        let (mut w1, mut w2) = (r.workspace("main").unwrap(), r.workspace("main").unwrap());
        w1.write("run.log", b"first");
        w2.write("run.log", b"newest");
        r.commit(&mut w1, "x", "log", "t1", 2).unwrap();
        r.commit(&mut w2, "y", "log", "t2", 3).unwrap();
        assert_eq!(r.read(&r.head("main").unwrap(), "run.log").unwrap().unwrap(), b"newest");
    }

    #[test]
    fn every_tool_call_is_a_checkpoint_in_the_log() {
        let mut r = repo();
        r.init("main", &[("a", b"0")], "x", 1).unwrap();
        let mut ws = r.workspace("main").unwrap();
        for (i, call) in ["toolu_1", "toolu_2", "toolu_3"].iter().enumerate() {
            ws.write("a", format!("{i}").as_bytes());
            r.commit(&mut ws, "claude", "tool call", call, 10 + i as u64).unwrap();
        }
        let log = r.log("main").unwrap();
        assert_eq!(log.len(), 4);
        assert_eq!(log[0].1.tool_call, "toolu_3");
        assert_eq!(log[0].1.parents, vec![log[1].0]);
    }

    #[test]
    fn files_shows_base_plus_uncommitted_changes() {
        let mut r = repo();
        r.init("main", &[("a", b"1"), ("b", b"2")], "x", 1).unwrap();
        let mut ws = r.workspace("main").unwrap();
        ws.write("a", b"changed");
        ws.write("c", b"new");
        ws.remove("b");
        let f = r.files(&ws).unwrap();
        assert_eq!(f.keys().map(String::as_str).collect::<Vec<_>>(), ["a", "c"]);
        assert_eq!(f["a"], b"changed");
    }

    #[test]
    fn tree_id_is_independent_of_write_order() {
        let (mut a, mut b) = (repo(), repo());
        a.init("main", &[("x", b"1"), ("y", b"2")], "a", 1).unwrap();
        b.init("main", &[("y", b"2"), ("x", b"1")], "a", 1).unwrap();
        assert_eq!(a.commit_info(&a.head("main").unwrap()).unwrap().tree, b.commit_info(&b.head("main").unwrap()).unwrap().tree);
    }

    #[test]
    fn directory_store_roundtrips_and_detects_corruption() {
        let dir = std::env::temp_dir().join(format!("playbox-ws-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        let mut r = Repo::new(Store::open(&dir).unwrap());
        r.init("main", &[("a.txt", b"hello")], "a", 1).unwrap();
        let head = r.head("main").unwrap();
        assert_eq!(r.read(&head, "a.txt").unwrap().unwrap(), b"hello");
        let tree_id = r.commit_info(&head).unwrap().tree;
        let p = r.store().path(&tree_id).unwrap();
        fs::write(p, b"tampered").unwrap();
        assert!(matches!(r.tree(&head), Err(Error::Corrupt)));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn commit_tree_id_feeds_the_signed_history_log() {
        let mut r = repo();
        r.init("main", &[("a", b"1")], "x", 1).unwrap();
        let mut ws = r.workspace("main").unwrap();
        ws.write("a", b"2");
        let c = r.commit(&mut ws, "x", "edit", "toolu_1", 5).unwrap();
        let tree = r.commit_info(&c).unwrap().tree;
        let key = SigningKey::from_bytes(&[7; 32]);
        let mut log = Log::new();
        log.append(&key, "fs_snapshot", &tree, 5);
        assert_eq!(log.verify(), Ok(()));
        assert_eq!(log.entries[0].payload_hash, *blake3::hash(&tree).as_bytes());
    }
}
