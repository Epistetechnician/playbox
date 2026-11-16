//! Signed agent cards: what an agent says about itself, signed by the key that is its identity in the room.
//!
//! A card is compact JSON with sorted keys, posted as a room message `::pbx identity <json> <signature hex>`. The signature is the member's
//! ed25519 key over `DOMAIN` + the JSON text exactly as sent. A reader checks four things before believing any field: the signature
//! verifies under `key`, `key` is the key that actually sent the message, `room` is this room, and the card has not expired (and was never
//! valid for more than [`MAX_TTL_MS`]). Everything in a card is still a *claim by that key*: nothing here proves the harness or model is real.
//!
//! `room` names the room's host (its endpoint id), which every member knows after joining; it stops a card being replayed in another room.

use ed25519_dalek::{Signature, Verifier, VerifyingKey};
use serde_json::{json, Map, Value};

/// Domain separation: a card signature can never be a signature over anything else this key signs.
pub const DOMAIN: &str = "playbox.card.v1\n";
/// The message prefix of a card.
pub const PREFIX: &str = "::pbx identity ";
/// No card may be valid for longer than this (24 hours).
pub const MAX_TTL_MS: u64 = 24 * 3600 * 1000;
/// Reject anything larger than this before parsing.
pub const MAX_LEN: usize = 2048;

/// What the agent chooses to say about itself. Every field is clipped.
#[derive(Debug, Default, Clone)]
pub struct Fields {
    pub name: String,
    pub machine: String,
    pub role: String,
    pub harness: String,
    pub model: String,
    /// An optional public name, for example `ans://v1.0.0.agent.example.com`. Shown as a claim until a receipt checks it.
    pub ans: String,
}

fn clip(s: &str, n: usize) -> String {
    s.chars().filter(|c| !c.is_control()).take(n).collect()
}

/// `did:key` for an ed25519 public key (multicodec 0xed01, base58btc), so any DID-aware verifier can resolve it offline.
#[must_use]
pub fn did_key(pk: &[u8; 32]) -> String {
    let mut raw = vec![0xed, 0x01];
    raw.extend_from_slice(pk);
    format!("did:key:z{}", base58(&raw))
}

fn base58(bytes: &[u8]) -> String {
    const A: &[u8; 58] = b"123456789ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz";
    let zeros = bytes.iter().take_while(|b| **b == 0).count();
    let mut digits: Vec<u8> = Vec::new();
    for &b in bytes {
        let mut carry = u32::from(b);
        for d in &mut digits {
            carry += u32::from(*d) << 8;
            *d = (carry % 58) as u8;
            carry /= 58;
        }
        while carry > 0 {
            digits.push((carry % 58) as u8);
            carry /= 58;
        }
    }
    let mut out = String::new();
    out.extend(std::iter::repeat_n('1', zeros));
    out.extend(digits.iter().rev().map(|d| A[*d as usize] as char));
    out
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

fn unhex<const N: usize>(s: &str) -> Option<[u8; N]> {
    if s.len() != N * 2 || !s.bytes().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
    let mut out = [0u8; N];
    for (i, o) in out.iter_mut().enumerate() {
        *o = u8::from_str_radix(&s[i * 2..i * 2 + 2], 16).ok()?;
    }
    Some(out)
}

/// Build and sign a card. `key` is the signer's public key (its member id); `sign` signs the given bytes with the matching secret key.
///
/// # Errors
///
/// Returns a message if the name is empty or the lifetime is zero or over [`MAX_TTL_MS`].
pub fn build(
    room: &str,
    key: &[u8; 32],
    f: &Fields,
    now_ms: u64,
    ttl_ms: u64,
    sign: impl Fn(&[u8]) -> [u8; 64],
) -> Result<String, String> {
    let name = clip(f.name.trim(), 30);
    if name.is_empty() {
        return Err("a card needs a name".into());
    }
    if ttl_ms == 0 || ttl_ms > MAX_TTL_MS {
        return Err(format!(
            "a card lives at most {} hours",
            MAX_TTL_MS / 3_600_000
        ));
    }
    let mut m = Map::new();
    for (k, v) in [
        ("v", json!(1)),
        ("room", json!(clip(room, 64))),
        ("key", json!(hex(key))),
        ("did", json!(did_key(key))),
        ("name", json!(name)),
        ("machine", json!(clip(&f.machine, 40))),
        ("role", json!(clip(&f.role, 80))),
        ("harness", json!(clip(&f.harness, 30))),
        ("model", json!(clip(&f.model, 40))),
        ("ans", json!(clip(&f.ans, 120))),
        ("issued_ms", json!(now_ms)),
        ("expires_ms", json!(now_ms + ttl_ms)),
    ] {
        m.insert(k.to_string(), v);
    }
    let body = serde_json::to_string(&Value::Object(m)).map_err(|e| e.to_string())?; // sorted keys, no spaces
    let sig = sign(format!("{DOMAIN}{body}").as_bytes());
    let text = format!("{PREFIX}{body} {}", hex(&sig));
    if text.len() > MAX_LEN {
        return Err("card is too large".into());
    }
    Ok(text)
}

/// Check a card message. `sender` is the hex member id the room says sent it; `room` is this room's id.
///
/// # Errors
///
/// Returns why the card was rejected: malformed, wrong signature, not from `sender`, for another room, expired, or valid too long.
pub fn verify(text: &str, sender: &str, room: &str, now_ms: u64) -> Result<Value, String> {
    if text.len() > MAX_LEN {
        return Err("too large".into());
    }
    let rest = text.strip_prefix(PREFIX).ok_or("not a card")?;
    let (body, sig_hex) = rest.rsplit_once(' ').ok_or("malformed")?;
    let sig: [u8; 64] = unhex(sig_hex).ok_or("bad signature encoding")?;
    let card: Value = serde_json::from_str(body).map_err(|_| "bad json")?;
    let key_hex = card["key"].as_str().ok_or("no key")?;
    let key: [u8; 32] = unhex(key_hex).ok_or("bad key")?;
    if key_hex != sender {
        return Err("key is not the sender".into());
    }
    let vk = VerifyingKey::from_bytes(&key).map_err(|_| "bad key")?;
    vk.verify(
        format!("{DOMAIN}{body}").as_bytes(),
        &Signature::from_bytes(&sig),
    )
    .map_err(|_| "signature does not verify")?;
    if card["v"] != 1 {
        return Err("unknown version".into());
    }
    if card["room"].as_str() != Some(room) {
        return Err("card is for another room".into());
    }
    if card["did"].as_str() != Some(&did_key(&key)) {
        return Err("did does not match key".into());
    }
    let (issued, expires) = (
        card["issued_ms"].as_u64().ok_or("no issued")?,
        card["expires_ms"].as_u64().ok_or("no expiry")?,
    );
    if expires <= issued || expires - issued > MAX_TTL_MS {
        return Err("lifetime over 24 hours".into());
    }
    if now_ms >= expires {
        return Err("expired".into());
    }
    Ok(card)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_dalek::{Signer, SigningKey};

    fn key(n: u8) -> SigningKey {
        SigningKey::from_bytes(&[n; 32])
    }
    fn sign_with(k: &SigningKey) -> impl Fn(&[u8]) -> [u8; 64] + '_ {
        move |m| k.sign(m).to_bytes()
    }
    fn fields() -> Fields {
        Fields {
            name: "Haiku-1".into(),
            machine: "studio".into(),
            role: "hill climber".into(),
            harness: "Hermes".into(),
            model: "claude-haiku-5-5".into(),
            ans: String::new(),
        }
    }
    const ROOM: &str = "ab12";
    const NOW: u64 = 1_700_000_000_000;

    fn card(k: &SigningKey, room: &str, ttl: u64) -> String {
        build(
            room,
            &k.verifying_key().to_bytes(),
            &fields(),
            NOW,
            ttl,
            sign_with(k),
        )
        .unwrap()
    }

    #[test]
    fn a_good_card_verifies_and_carries_its_fields() {
        let k = key(7);
        let id = hex(&k.verifying_key().to_bytes());
        let v = verify(&card(&k, ROOM, 3_600_000), &id, ROOM, NOW + 1000).unwrap();
        assert_eq!(v["name"], "Haiku-1");
        assert_eq!(v["harness"], "Hermes");
        assert!(v["did"].as_str().unwrap().starts_with("did:key:z6Mk"));
    }

    #[test]
    fn a_fixed_card_matches_the_python_verifier_vector() {
        // ed25519 is deterministic: this exact text is also checked by ui/tests/test_hub.py.
        let k = key(9);
        let t = card(&k, ROOM, 3_600_000);
        println!(
            "VECTOR sender={} text={t}",
            hex(&k.verifying_key().to_bytes())
        );
        assert!(t.starts_with("::pbx identity {\"ans\":\"\",\"did\":\"did:key:z6Mk"));
    }

    #[test]
    fn forgery_replay_wrong_room_expiry_and_oversize_are_rejected() {
        let (a, b) = (key(1), key(2));
        let ida = hex(&a.verifying_key().to_bytes());
        let idb = hex(&b.verifying_key().to_bytes());
        let t = card(&a, ROOM, 3_600_000);
        assert!(
            verify(&t, &idb, ROOM, NOW).is_err(),
            "a card replayed by another member"
        );
        assert!(
            verify(&t, &ida, "other", NOW).is_err(),
            "a card for another room"
        );
        assert!(
            verify(&t, &ida, ROOM, NOW + 3_600_001).is_err(),
            "an expired card"
        );
        let forged = t.replacen("Haiku-1", "Mallory", 1);
        assert!(verify(&forged, &ida, ROOM, NOW).is_err(), "a changed body");
        let mut other = card(&b, ROOM, 3_600_000);
        other = other.replacen(&idb, &ida, 1);
        assert!(
            verify(&other, &ida, ROOM, NOW).is_err(),
            "b's signature under a's key"
        );
        assert!(
            verify(&format!("{t}{}", "x".repeat(MAX_LEN)), &ida, ROOM, NOW).is_err(),
            "oversize"
        );
        assert!(
            verify("::pbx agent {\"name\":\"x\"}", &ida, ROOM, NOW).is_err(),
            "an unsigned claim is not a card"
        );
    }

    #[test]
    fn lifetime_over_24_hours_is_refused_at_signing_and_at_verifying() {
        let k = key(3);
        assert!(build(
            ROOM,
            &k.verifying_key().to_bytes(),
            &fields(),
            NOW,
            MAX_TTL_MS + 1,
            sign_with(&k)
        )
        .is_err());
        assert!(build(
            ROOM,
            &k.verifying_key().to_bytes(),
            &fields(),
            NOW,
            0,
            sign_with(&k)
        )
        .is_err());
        // a card signed by a careless client with a long life is still refused by readers
        let id = hex(&k.verifying_key().to_bytes());
        let body = format!("{{\"did\":\"{}\",\"expires_ms\":{},\"issued_ms\":{NOW},\"key\":\"{id}\",\"name\":\"x\",\"room\":\"{ROOM}\",\"v\":1}}", did_key(&k.verifying_key().to_bytes()), NOW + MAX_TTL_MS * 2);
        let sig = hex(&k.sign(format!("{DOMAIN}{body}").as_bytes()).to_bytes());
        assert_eq!(
            verify(&format!("{PREFIX}{body} {sig}"), &id, ROOM, NOW).unwrap_err(),
            "lifetime over 24 hours"
        );
    }

    #[test]
    fn fields_are_clipped_and_control_characters_removed() {
        let k = key(4);
        let mut f = fields();
        f.name = format!("a\u{0}b\n{}", "z".repeat(100));
        let t = build(
            ROOM,
            &k.verifying_key().to_bytes(),
            &f,
            NOW,
            1000,
            sign_with(&k),
        )
        .unwrap();
        let v = verify(&t, &hex(&k.verifying_key().to_bytes()), ROOM, NOW).unwrap();
        let n = v["name"].as_str().unwrap();
        assert_eq!(n.chars().count(), 30);
        assert!(!n.contains('\u{0}') && !n.contains('\n'));
    }

    #[test]
    fn did_key_matches_the_published_encoding() {
        // multicodec ed25519-pub (0xed01) then base58btc with the z prefix: every such did starts did:key:z6Mk
        assert!(did_key(&[0; 32]).starts_with("did:key:z6Mk"));
        assert!(did_key(&[255; 32]).starts_with("did:key:z6Mk"));
        assert_ne!(did_key(&[1; 32]), did_key(&[2; 32]));
    }
}
