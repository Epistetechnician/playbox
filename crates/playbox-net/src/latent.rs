//! Hub latent messages in a room (see `docs/latent-channels.md`, section 3 and 6f).
//!
//! A member signs a `PBL2` envelope: a hub id, the sender's label, `k` and the int8 body of one hub vector (`k`
//! bytes, no text). The host checks the envelope and the signature, requires the signer to be the sending
//! member's own key, files it in a bounded mailbox, posts a one-line notice to the room feed and returns a receipt
//! signed with the host key. Members read the mailbox with [`wire::Op::LatentPull`]; every envelope is
//! self-verifying, so a reader does not have to trust the host for its content.
//!
//! The host never decodes a vector: it holds no spoke and cannot read what the vector means. What it vouches for
//! is that this member posted exactly these bytes for this hub.
//!
//! # What this does not do
//! It does not check that the vector is honest (that is the commit-and-challenge audit, not built), and the
//! receiver applies a decoded vector to its own model under its own strength cap and guards.

use crate::{
    steer::{hex, MAX_HUB_K},
    wire::{self, Op},
    Client, EndpointId, RoomHandler,
};
use a2a::{Message, Part, Task, TaskState};
use anyhow::{anyhow, bail, Result};
use base64::{engine::general_purpose::STANDARD as B64S, Engine};
use ed25519_dalek::{Signature, Signer, Verifier, VerifyingKey};
use serde_json::{json, Value};
use std::{collections::VecDeque, sync::Mutex};

pub const MAGIC: &[u8; 4] = b"PBL2";
const MAX_HEADER: usize = 4096;
/// Entries the mailbox keeps in all, and per signer.
const KEEP_ALL: usize = 256;
const KEEP_EACH: usize = 32;
const PULL_MAX: usize = 32;

/// A parsed, verified `PBL2` envelope.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Envelope {
    pub hub: String,
    pub src: String,
    pub k: usize,
    /// The int8 quantization scale as the big-endian bytes of an f32, hex (no float text to disagree on).
    pub scale_f32: String,
    pub signer: [u8; 32],
    pub body: Vec<u8>,
}

impl Envelope {
    pub fn scale(&self) -> f32 {
        let mut b = [0u8; 4];
        for (i, o) in b.iter_mut().enumerate() {
            *o = u8::from_str_radix(&self.scale_f32[2 * i..2 * i + 2], 16).unwrap_or(0);
        }
        f32::from_be_bytes(b)
    }
}

fn signed_text(
    hub: &str,
    src: &str,
    k: usize,
    scale: &str,
    body_hash: &str,
    signer: &str,
) -> String {
    format!("playbox.latent.v1\nhub {hub}\nsrc {src}\nk {k}\nscale {scale}\nbody {body_hash}\nsigner {signer}\n")
}

fn label_ok(s: &str, max: usize) -> bool {
    !s.is_empty()
        && s.len() <= max
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-' | b'.'))
}

/// Check bytes from a peer and return the envelope. Refuses anything malformed, oversized, unsigned or with a body
/// that does not match its header.
///
/// # Errors
///
/// Returns an error describing the first check that failed.
pub fn parse(data: &[u8]) -> Result<Envelope> {
    if data.len() < 8 || &data[..4] != MAGIC {
        bail!("not a latent message");
    }
    let n = u32::from_be_bytes([data[4], data[5], data[6], data[7]]) as usize;
    if n > MAX_HEADER || 8 + n > data.len() {
        bail!("bad header length");
    }
    let head: Value =
        serde_json::from_slice(&data[8..8 + n]).map_err(|_| anyhow!("malformed header"))?;
    let s = |key: &str| -> Result<String> {
        head[key]
            .as_str()
            .map(str::to_string)
            .ok_or_else(|| anyhow!("header has no {key}"))
    };
    let (hub, src, scale, body_hash, signer_hex, sig_hex) = (
        s("hub")?,
        s("src")?,
        s("scale_f32")?,
        s("body_blake3")?,
        s("signer")?,
        s("sig")?,
    );
    let k = head["k"]
        .as_u64()
        .ok_or_else(|| anyhow!("header has no k"))? as usize;
    if !(label_ok(&hub, 64) && hub.bytes().all(|b| b.is_ascii_hexdigit())) {
        bail!("bad hub id");
    }
    if !label_ok(&src, 64) {
        bail!("bad sender label");
    }
    if k == 0 || k > MAX_HUB_K {
        bail!("k out of range");
    }
    let body = &data[8 + n..];
    if body.len() != k {
        bail!("the body is not k bytes");
    }
    if scale.len() != 8 || !scale.bytes().all(|b| b.is_ascii_hexdigit()) {
        bail!("bad scale");
    }
    if hex(blake3::hash(body).as_bytes()) != body_hash {
        bail!("the body does not match its hash");
    }
    let signer =
        playbox_core::record::unhex::<32>(&signer_hex).map_err(|e| anyhow!("bad signer: {e}"))?;
    let sig = crate::steer::hex_to_64(&sig_hex)?;
    VerifyingKey::from_bytes(&signer)
        .map_err(|e| anyhow!("{e}"))?
        .verify(
            signed_text(&hub, &src, k, &scale, &body_hash, &signer_hex).as_bytes(),
            &Signature::from_bytes(&sig),
        )
        .map_err(|_| anyhow!("the signature is not valid"))?;
    Ok(Envelope {
        hub,
        src,
        k,
        scale_f32: scale,
        signer,
        body: body.to_vec(),
    })
}

/// Build and sign an envelope (what a Rust sender and the tests use; the Python side has the same layout).
#[must_use]
pub fn pack(
    key: &ed25519_dalek::SigningKey,
    hub: &str,
    src: &str,
    scale: f32,
    body: &[u8],
) -> Vec<u8> {
    let scale_hex = hex(&scale.to_be_bytes());
    let body_hash = hex(blake3::hash(body).as_bytes());
    let signer = hex(key.verifying_key().as_bytes());
    let sig =
        key.sign(signed_text(hub, src, body.len(), &scale_hex, &body_hash, &signer).as_bytes());
    let head = serde_json::to_vec(&json!({
        "hub": hub, "src": src, "k": body.len(), "scale_f32": scale_hex, "body_blake3": body_hash,
        "signer": signer, "sig": hex(&sig.to_bytes()),
    }))
    .expect("header serializes");
    let mut out = MAGIC.to_vec();
    out.extend_from_slice(
        &u32::try_from(head.len())
            .expect("header is small")
            .to_be_bytes(),
    );
    out.extend_from_slice(&head);
    out.extend_from_slice(body);
    out
}

/// What the host signs to say it filed exactly these bytes from this member.
fn receipt_body(host: &[u8], signer: &[u8], hub: &str, envelope: &[u8], seq: u64) -> String {
    format!(
        "playbox.latent-receipt.v1\nhost {}\nsigner {}\nhub {hub}\nenvelope {}\nseq {seq}\n",
        hex(host),
        hex(signer),
        hex(blake3::hash(envelope).as_bytes()),
    )
}

/// Check a host's signed receipt for these envelope bytes from `signer`; returns the mailbox sequence number.
///
/// # Errors
///
/// Returns an error if the receipt is not signed by the host or is about different bytes.
pub fn verify_receipt(
    text: &str,
    host: &[u8; 32],
    signer: &[u8; 32],
    hub: &str,
    envelope: &[u8],
) -> Result<u64> {
    let (body, sig) = text
        .rsplit_once("sig ")
        .ok_or_else(|| anyhow!("the receipt has no signature"))?;
    let sig = crate::steer::hex_to_64(sig.trim())?;
    VerifyingKey::from_bytes(host)
        .map_err(|e| anyhow!("{e}"))?
        .verify(body.as_bytes(), &Signature::from_bytes(&sig))
        .map_err(|_| anyhow!("the receipt is not signed by the host"))?;
    let seq: u64 = body
        .lines()
        .find_map(|l| l.strip_prefix("seq "))
        .and_then(|v| v.parse().ok())
        .ok_or_else(|| anyhow!("the receipt has no sequence number"))?;
    if body != receipt_body(host, signer, hub, envelope, seq) {
        bail!("the receipt is about a different message");
    }
    Ok(seq)
}

struct Entry {
    seq: u64,
    from: EndpointId,
    hub: String,
    bytes: Vec<u8>,
}

/// Recent envelopes, bounded in all and per signer. Not persisted.
#[derive(Default)]
pub struct Mailbox {
    inner: Mutex<(u64, VecDeque<Entry>)>,
}

impl std::fmt::Debug for Mailbox {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Mailbox")
    }
}

impl Mailbox {
    fn put(&self, from: EndpointId, hub: &str, bytes: Vec<u8>) -> u64 {
        let mut g = self.inner.lock().expect("mailbox lock");
        g.0 += 1;
        let seq = g.0;
        g.1.push_back(Entry {
            seq,
            from,
            hub: hub.to_string(),
            bytes,
        });
        while g.1.iter().filter(|e| e.from == from).count() > KEEP_EACH {
            if let Some(i) = g.1.iter().position(|e| e.from == from) {
                g.1.remove(i);
            }
        }
        while g.1.len() > KEEP_ALL {
            g.1.pop_front();
        }
        seq
    }

    fn since(&self, hub: &str, since: u64) -> (Vec<(u64, EndpointId, Vec<u8>)>, u64) {
        let g = self.inner.lock().expect("mailbox lock");
        let items: Vec<_> =
            g.1.iter()
                .filter(|e| e.seq > since && e.hub == hub)
                .take(PULL_MAX)
                .map(|e| (e.seq, e.from, e.bytes.clone()))
                .collect();
        let next = items.last().map_or(since, |i| i.0);
        (items, next)
    }
}

impl RoomHandler {
    /// File a signed latent message from `from`, post a notice to the feed, and sign a receipt.
    pub(crate) fn latent_deposit(&self, from: EndpointId, msg: &Message) -> Result<Task> {
        let parts = wire::raw_parts(msg);
        let [data] = parts.as_slice() else {
            return Err(crate::refuse("a latent message needs exactly one raw part"));
        };
        let env = parse(data).map_err(|e| crate::refuse(format!("{e:#}")))?;
        if env.signer != *from.as_bytes() {
            return Err(crate::refuse(
                "the message must be signed by the member who sends it",
            ));
        }
        let seq = self.latent.put(from, &env.hub, data.to_vec());
        let _ = self.audit.chat.say(
            &from.to_string(),
            None,
            &format!(
                "[latent] hub {} from {} k={} seq={seq}",
                &env.hub[..env.hub.len().min(16)],
                env.src,
                env.k
            ),
        );
        let body = receipt_body(
            self.attest_key.verifying_key().as_bytes(),
            from.as_bytes(),
            &env.hub,
            data,
            seq,
        );
        let sig = self.attest_key.sign(body.as_bytes());
        Ok(wire::task(
            TaskState::Completed,
            None,
            vec![wire::text_artifact(
                "receipt",
                format!("{body}sig {}", hex(&sig.to_bytes())),
            )],
            vec![("kind", json!("latent"))],
        ))
    }

    /// The envelopes for `hub` after `since`, as JSON with each envelope in base64.
    pub(crate) fn latent_pull(&self, hub: &str, since: u64) -> Task {
        let (items, next) = self.latent.since(hub, since);
        let items: Vec<Value> = items
            .into_iter()
            .map(|(seq, from, bytes)| json!({ "seq": seq, "from": from.to_string(), "envelope": B64S.encode(bytes) }))
            .collect();
        wire::task(
            TaskState::Completed,
            None,
            vec![wire::text_artifact(
                "latent",
                json!({ "items": items, "next": next }).to_string(),
            )],
            vec![("kind", json!("latent_pull"))],
        )
    }
}

/// A message read from the room, already verified.
#[derive(Debug, Clone)]
pub struct Delivered {
    pub seq: u64,
    pub from: String,
    pub envelope: Envelope,
    /// The envelope exactly as the host delivered it (already verified), for passing on.
    pub raw: Vec<u8>,
}

impl Client {
    /// Post a signed `PBL2` envelope. Returns its sequence number after checking the host's signed receipt.
    ///
    /// # Errors
    ///
    /// Returns an error if the host refused, or its receipt is not valid for exactly these bytes.
    pub async fn latent_send(&self, envelope: &[u8]) -> Result<u64> {
        let env = parse(envelope)?;
        let t = self
            .send_task("latent", &Op::Latent, vec![Part::raw(envelope.to_vec())])
            .await?;
        let receipt = wire::artifact_text(&t, "receipt")
            .ok_or_else(|| anyhow!("the host sent no receipt"))?;
        verify_receipt(
            &receipt,
            self.host_id().as_bytes(),
            self.endpoint.id().as_bytes(),
            &env.hub,
            envelope,
        )
    }

    /// Read the envelopes for `hub` after `since`, each verified. Returns them and the `since` for the next call.
    ///
    /// # Errors
    ///
    /// Returns an error if the host refused or sent something that is not valid.
    pub async fn latent_pull(&self, hub: &str, since: u64) -> Result<(Vec<Delivered>, u64)> {
        let op = Op::LatentPull {
            hub: hub.to_string(),
            since,
        };
        let t = self.send_task("latent pull", &op, vec![]).await?;
        let v: Value = serde_json::from_str(
            &wire::artifact_text(&t, "latent")
                .ok_or_else(|| anyhow!("the host sent no mailbox"))?,
        )?;
        let mut out = Vec::new();
        for it in v["items"]
            .as_array()
            .ok_or_else(|| anyhow!("bad mailbox"))?
        {
            let bytes = B64S.decode(it["envelope"].as_str().ok_or_else(|| anyhow!("bad item"))?)?;
            let envelope = parse(&bytes)?;
            if envelope.hub != hub {
                bail!("the host sent a message for another hub");
            }
            out.push(Delivered {
                seq: it["seq"].as_u64().unwrap_or(0),
                from: it["from"].as_str().unwrap_or_default().to_string(),
                envelope,
                raw: bytes,
            });
        }
        Ok((out, v["next"].as_u64().unwrap_or(since)))
    }
}
