//! Canonical signed text records, shared by the modules that sign things other than history entries
//! (admission capabilities, evaluator manifests, memory entries, model receipts, log checkpoints).
//!
//! A record is a domain line, `key value` lines, then `signer <hex>` and `sig <hex>`. The signature
//! covers the domain line and every line before `sig`, so a record cannot be replayed under another
//! domain and no field can be changed or dropped without breaking it. Empty values are written `-`;
//! lists are comma-joined.

use ed25519_dalek::{Signature, Signer, SigningKey, Verifier, VerifyingKey};

pub fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

pub fn unhex_vec(s: &str) -> Result<Vec<u8>, String> {
    if !s.len().is_multiple_of(2) || !s.is_ascii() {
        return Err("odd-length or non-ascii hex".into());
    }
    (0..s.len() / 2).map(|i| u8::from_str_radix(&s[i * 2..i * 2 + 2], 16).map_err(|e| e.to_string())).collect()
}

pub fn unhex<const N: usize>(s: &str) -> Result<[u8; N], String> {
    let v = unhex_vec(s)?;
    v.try_into().map_err(|_| format!("expected {} hex chars", N * 2))
}

fn clean_value(v: &str) -> Result<&str, String> {
    if v.contains(['\n', '\r']) || v != v.trim() {
        return Err(format!("value {v:?} must be a single trimmed line"));
    }
    Ok(v)
}

fn valid_key(k: &str) -> bool {
    !k.is_empty() && k.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
}

fn body(domain: &str, fields: &[(&str, String)]) -> Result<String, String> {
    clean_value(domain)?;
    let mut s = format!("{domain}\n");
    for (k, v) in fields {
        if !valid_key(k) || matches!(*k, "signer" | "sig") {
            return Err(format!("bad field name {k:?}"));
        }
        s.push_str(&format!("{k} {}\n", if v.is_empty() { "-" } else { clean_value(v)? }));
    }
    Ok(s)
}

/// Sign `fields` under `domain`. The result is the full record text.
pub fn sign(domain: &str, fields: &[(&str, String)], key: &SigningKey) -> Result<String, String> {
    let mut s = body(domain, fields)?;
    s.push_str(&format!("signer {}\n", hex(&key.verifying_key().to_bytes())));
    let sig = key.sign(s.as_bytes());
    s.push_str(&format!("sig {}\n", hex(&sig.to_bytes())));
    Ok(s)
}

/// A record whose signature has been checked.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Signed {
    pub fields: Vec<(String, String)>,
    pub signer: [u8; 32],
}

impl Signed {
    pub fn get(&self, k: &str) -> Option<&str> {
        self.fields.iter().find(|(n, _)| n == k).map(|(_, v)| v.as_str()).map(|v| if v == "-" { "" } else { v })
    }

    pub fn req(&self, k: &str) -> Result<&str, String> {
        self.get(k).ok_or_else(|| format!("missing field {k}"))
    }

    pub fn num(&self, k: &str) -> Result<u64, String> {
        self.req(k)?.parse().map_err(|_| format!("field {k} is not a number"))
    }

    pub fn list(&self, k: &str) -> Result<Vec<&str>, String> {
        let v = self.req(k)?;
        Ok(if v.is_empty() { Vec::new() } else { v.split(',').collect() })
    }

    pub fn hash(&self, k: &str) -> Result<[u8; 32], String> {
        unhex(self.req(k)?)
    }
}

/// Parse and verify a record of `domain`. Fails on any malformed line, duplicate field, wrong domain,
/// trailing data or bad signature.
pub fn open(domain: &str, text: &str) -> Result<Signed, String> {
    let mut lines = text.lines();
    if lines.next() != Some(domain) {
        return Err(format!("not a {domain} record"));
    }
    let (mut fields, mut signer, mut sig) = (Vec::<(String, String)>::new(), None, None);
    for line in lines {
        if sig.is_some() {
            return Err("data after signature".into());
        }
        let (k, v) = line.split_once(' ').ok_or_else(|| format!("bad line {line:?}"))?;
        match k {
            "sig" => sig = Some(unhex::<64>(v)?),
            "signer" if signer.is_none() => signer = Some(unhex::<32>(v)?),
            _ if valid_key(k) && k != "signer" => {
                if fields.iter().any(|(n, _)| n == k) {
                    return Err(format!("duplicate field {k}"));
                }
                fields.push((k.to_string(), v.to_string()));
            }
            _ => return Err(format!("bad field {k:?}")),
        }
    }
    let (signer, sig) = (signer.ok_or("no signer")?, sig.ok_or("no signature")?);
    let end = text.rfind("sig ").ok_or("no signature")?;
    VerifyingKey::from_bytes(&signer)
        .map_err(|_| "invalid signer key".to_string())?
        .verify(&text.as_bytes()[..end], &Signature::from_bytes(&sig))
        .map_err(|_| "signature does not verify".to_string())?;
    Ok(Signed { fields, signer })
}

/// blake3 over length-prefixed parts, so `["ab","c"]` and `["a","bc"]` differ.
pub fn hash_parts(domain: &str, parts: &[&[u8]]) -> [u8; 32] {
    let mut h = blake3::Hasher::new();
    h.update(domain.as_bytes());
    for p in parts {
        h.update(&(p.len() as u64).to_be_bytes());
        h.update(p);
    }
    *h.finalize().as_bytes()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key() -> SigningKey {
        SigningKey::from_bytes(&[3; 32])
    }

    #[test]
    fn round_trip_and_tamper() {
        let t = sign("d.v1", &[("a", "1".into()), ("b", String::new()), ("l", "x,y".into())], &key()).unwrap();
        let r = open("d.v1", &t).unwrap();
        assert_eq!((r.num("a").unwrap(), r.get("b").unwrap(), r.list("l").unwrap()), (1, "", vec!["x", "y"]));
        assert_eq!(r.signer, key().verifying_key().to_bytes());
        assert!(open("d.v2", &t).is_err(), "wrong domain");
        assert!(open("d.v1", &t.replace("a 1", "a 2")).is_err(), "edited field");
        assert!(open("d.v1", &format!("{t}extra 1\n")).is_err(), "trailing data");
        assert!(open("d.v1", &t.replace("l x,y\n", "")).is_err(), "dropped field");
    }

    #[test]
    fn rejects_bad_fields() {
        assert!(sign("d", &[("sig", "x".into())], &key()).is_err());
        assert!(sign("d", &[("a", "two\nlines".into())], &key()).is_err());
        assert!(sign("d", &[("A", "x".into())], &key()).is_err());
    }

    #[test]
    fn hash_parts_is_unambiguous() {
        assert_ne!(hash_parts("d", &[b"ab", b"c"]), hash_parts("d", &[b"a", b"bc"]));
    }
}
