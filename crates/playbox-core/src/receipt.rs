//! Receipts: evidence that a recorded model response really came from a model.
//!
//! The history log proves a session recorded these `model_response` entries, in this order, signed by
//! this key. It cannot prove the text was ever produced by a model: a guest can write a convincing
//! transcript for a patch they made by hand. A [`Receipt`] closes that gap by having someone *other than
//! the session key* sign over the request and response hashes:
//!
//! | [`Level`] | Signed by | Believe |
//! |---|---|---|
//! | `ProviderSigned` | the model provider's pinned key | the provider says it produced this response for this request |
//! | `Notarized` | a pinned TLS notary that watched the API call | the call was made and returned this, as the notary saw it |
//! | `SelfClaimed` | the session key itself | nothing beyond the log: the author asserts it |
//! | `None` | unknown key | nothing |
//!
//! No provider signs responses today, so in practice the useful levels are `Notarized` (a TLSNotary-style
//! service, or a TEE proxy that relays the API call) and `SelfClaimed`. The type exists so that when a
//! provider does sign, rooms can require it without a format change. [`coverage`] counts how much of a
//! session's model output is backed at each level, and [`require`] turns that into an accept/reject
//! decision per room policy.
//!
//! # What this does and does not prove
//! A receipt binds *a response to a request*. It does not prove the request was the real prompt the agent
//! saw (the prompt is only matched to the preceding `prompt` entry's hash), that the model was not
//! prompted into a bad answer, or that the agent acted on the response. And a notary or provider key is a
//! trust root you pinned: its compromise breaks everything resting on it.

use crate::record::RecordError;
use crate::{
    history::Log,
    record::{self, hex},
};
use ed25519_dalek::SigningKey;
use std::collections::BTreeMap;

const DOMAIN: &str = "playbox.model-receipt.v1";

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Level {
    None,
    SelfClaimed,
    Notarized,
    ProviderSigned,
}

impl Level {
    /// # Errors
    ///
    /// Returns a [`RecordError`] if the text or field is malformed, or its signature does not verify.
    pub fn parse(s: &str) -> Result<Level, RecordError> {
        Ok(match s.to_ascii_lowercase().as_str() {
            "none" => Level::None,
            "self" => Level::SelfClaimed,
            "notarized" => Level::Notarized,
            "provider" => Level::ProviderSigned,
            o => {
                return Err(format!(
                    "unknown receipt level {o:?}; use none, self, notarized or provider"
                )
                .into())
            }
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Receipt {
    /// Who produced the response, e.g. `api.anthropic.com`.
    pub provider: String,
    pub model: String,
    pub request_hash: [u8; 32],
    pub response_hash: [u8; 32],
    pub time_ms: u64,
    /// The key that signed this receipt.
    pub attester: [u8; 32],
}

impl Receipt {
    /// Sign over `request` and `response` (the exact bytes the history's `prompt` and `model_response`
    /// payloads hash).
    ///
    /// # Errors
    ///
    /// Returns a [`RecordError`] if the input is malformed or breaks a rule of the record's domain.
    pub fn sign(
        provider: &str,
        model: &str,
        request: &[u8],
        response: &[u8],
        time_ms: u64,
        attester: &SigningKey,
    ) -> Result<String, RecordError> {
        record::sign(
            DOMAIN,
            &[
                ("provider", provider.into()),
                ("model", model.into()),
                ("request", hex(blake3::hash(request).as_bytes())),
                ("response", hex(blake3::hash(response).as_bytes())),
                ("time", time_ms.to_string()),
            ],
            attester,
        )
    }

    /// # Errors
    ///
    /// Returns a [`RecordError`] if the text or field is malformed, or its signature does not verify.
    pub fn open(text: &str) -> Result<Receipt, RecordError> {
        let r = record::open(DOMAIN, text)?;
        Ok(Receipt {
            provider: r.req("provider")?.to_string(),
            model: r.req("model")?.to_string(),
            request_hash: r.hash("request")?,
            response_hash: r.hash("response")?,
            time_ms: r.num("time")?,
            attester: r.signer,
        })
    }
}

/// Whose receipts count for what.
#[derive(Debug, Clone, Default)]
pub struct Trust {
    /// Provider name -> its pinned signing keys.
    providers: BTreeMap<String, Vec<[u8; 32]>>,
    notaries: Vec<[u8; 32]>,
}

impl Trust {
    #[must_use]
    pub fn provider(mut self, name: &str, key: [u8; 32]) -> Self {
        self.providers
            .entry(name.to_string())
            .or_default()
            .push(key);
        self
    }

    #[must_use]
    pub fn notary(mut self, key: [u8; 32]) -> Self {
        self.notaries.push(key);
        self
    }

    /// The level `r` earns for a session signed by `session_key`.
    #[must_use]
    pub fn level(&self, r: &Receipt, session_key: &[u8; 32]) -> Level {
        if self
            .providers
            .get(&r.provider)
            .is_some_and(|ks| ks.contains(&r.attester))
        {
            Level::ProviderSigned
        } else if self.notaries.contains(&r.attester) {
            Level::Notarized
        } else if &r.attester == session_key {
            Level::SelfClaimed
        } else {
            Level::None
        }
    }
}

/// How much of a session's model output is backed by receipts, and at what level.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Coverage {
    pub responses: usize,
    pub provider_signed: usize,
    pub notarized: usize,
    pub self_claimed: usize,
    /// No receipt, or a receipt from an unknown key.
    pub unbacked: usize,
    /// A receipt matched the response but not the prompt before it: replayed for a different prompt.
    pub mismatched: usize,
}

impl Coverage {
    /// Responses backed at `min` or better.
    #[must_use]
    pub fn at_least(&self, min: Level) -> usize {
        match min {
            Level::None => self.responses,
            Level::SelfClaimed => self.provider_signed + self.notarized + self.self_claimed,
            Level::Notarized => self.provider_signed + self.notarized,
            Level::ProviderSigned => self.provider_signed,
        }
    }
}

/// Match a session's `model_response` entries against receipts. A response is backed by the best receipt
/// whose `response_hash` equals the entry's payload hash and, when the previous entry is a `prompt`,
/// whose `request_hash` equals that prompt's payload hash.
#[must_use]
pub fn coverage(log: &Log, receipts: &[Receipt], trust: &Trust) -> Coverage {
    let session = log.entries.first().map_or([0; 32], |e| e.signer);
    let mut c = Coverage::default();
    for (i, e) in log
        .entries
        .iter()
        .enumerate()
        .filter(|(_, e)| e.kind == "model_response")
    {
        c.responses += 1;
        let prompt = i
            .checked_sub(1)
            .map(|j| &log.entries[j])
            .filter(|p| p.kind == "prompt");
        let candidates: Vec<&Receipt> = receipts
            .iter()
            .filter(|r| r.response_hash == e.payload_hash)
            .collect();
        let matching: Vec<&&Receipt> = candidates
            .iter()
            .filter(|r| prompt.is_none_or(|p| r.request_hash == p.payload_hash))
            .collect();
        match matching.iter().map(|r| trust.level(r, &session)).max() {
            Some(Level::ProviderSigned) => c.provider_signed += 1,
            Some(Level::Notarized) => c.notarized += 1,
            Some(Level::SelfClaimed) => c.self_claimed += 1,
            Some(Level::None) | None if !candidates.is_empty() && matching.is_empty() => {
                c.mismatched += 1;
            }
            _ => c.unbacked += 1,
        }
    }
    c
}

/// Accept a session only if at least `numerator`/`denominator` of its model responses are backed at `min`
/// or better. A session with no model responses passes (there is nothing to back).
///
/// # Errors
///
/// Returns a [`RecordError`] if the input is malformed or breaks a rule of the record's domain.
pub fn require(
    c: &Coverage,
    min: Level,
    numerator: usize,
    denominator: usize,
) -> Result<(), RecordError> {
    if c.responses == 0 {
        return Ok(());
    }
    let have = c.at_least(min);
    if have * denominator >= c.responses * numerator {
        Ok(())
    } else {
        Err(format!(
            "{have} of {} model responses are backed at {min:?} or better (need {numerator}/{denominator}); {} unbacked, {} mismatched",
            c.responses, c.unbacked, c.mismatched
        ).into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn k(n: u8) -> SigningKey {
        SigningKey::from_bytes(&[n; 32])
    }
    const SESSION: u8 = 1;
    const PROVIDER: u8 = 2;
    const NOTARY: u8 = 3;

    fn trust() -> Trust {
        Trust::default()
            .provider("api.anthropic.com", k(PROVIDER).verifying_key().to_bytes())
            .notary(k(NOTARY).verifying_key().to_bytes())
    }

    type Exchange = (Vec<u8>, Vec<u8>);

    /// A session with `n` prompt/response exchanges.
    fn session(n: usize) -> (Log, Vec<Exchange>) {
        let mut log = Log::new();
        let mut pairs = Vec::new();
        for i in 0..n {
            let (q, a) = (
                format!("prompt {i}").into_bytes(),
                format!("answer {i}").into_bytes(),
            );
            log.append(&k(SESSION), "prompt", &q, i as u64 * 2);
            log.append(&k(SESSION), "model_response", &a, i as u64 * 2 + 1);
            pairs.push((q, a));
        }
        (log, pairs)
    }

    fn receipt(who: u8, provider: &str, q: &[u8], a: &[u8]) -> Receipt {
        Receipt::open(&Receipt::sign(provider, "m", q, a, 1, &k(who)).unwrap()).unwrap()
    }

    #[test]
    fn each_level_is_credited_to_the_right_signer() {
        let (log, p) = session(4);
        let rs = vec![
            receipt(PROVIDER, "api.anthropic.com", &p[0].0, &p[0].1),
            receipt(NOTARY, "api.anthropic.com", &p[1].0, &p[1].1),
            receipt(SESSION, "api.anthropic.com", &p[2].0, &p[2].1),
        ];
        let c = coverage(&log, &rs, &trust());
        assert_eq!(
            (
                c.responses,
                c.provider_signed,
                c.notarized,
                c.self_claimed,
                c.unbacked
            ),
            (4, 1, 1, 1, 1)
        );
        assert_eq!(c.at_least(Level::Notarized), 2);
    }

    #[test]
    fn a_provider_key_only_counts_for_its_own_provider() {
        let (log, p) = session(1);
        let rs = vec![receipt(PROVIDER, "api.evil.example", &p[0].0, &p[0].1)];
        let c = coverage(&log, &rs, &trust());
        assert_eq!(
            (c.provider_signed, c.unbacked),
            (0, 1),
            "right key, wrong provider name"
        );
    }

    #[test]
    fn unknown_signers_back_nothing_and_a_replayed_receipt_is_a_mismatch() {
        let (log, p) = session(2);
        let stranger = receipt(9, "api.anthropic.com", &p[0].0, &p[0].1);
        assert_eq!(coverage(&log, &[stranger], &trust()).unbacked, 2);
        // A real notarized receipt for answer 0, but for a different prompt than the one before it.
        let replay = receipt(NOTARY, "api.anthropic.com", b"some other prompt", &p[0].1);
        let c = coverage(&log, &[replay], &trust());
        assert_eq!((c.mismatched, c.notarized), (1, 0));
    }

    #[test]
    fn receipts_cannot_be_forged_or_edited() {
        let t = Receipt::sign("p", "m", b"q", b"a", 1, &k(NOTARY)).unwrap();
        assert!(Receipt::open(&t).is_ok());
        assert!(Receipt::open(&t.replace("model m", "model other")).is_err());
    }

    #[test]
    fn policy_requires_a_fraction_at_a_level() {
        let (log, p) = session(4);
        let rs: Vec<_> = (0..3)
            .map(|i| receipt(NOTARY, "api.anthropic.com", &p[i].0, &p[i].1))
            .collect();
        let c = coverage(&log, &rs, &trust());
        assert!(require(&c, Level::Notarized, 3, 4).is_ok());
        let err = require(&c, Level::Notarized, 1, 1).unwrap_err().to_string();
        assert!(err.contains("3 of 4"), "{err}");
        assert!(
            require(&c, Level::ProviderSigned, 1, 4).is_err(),
            "nobody provider-signed"
        );
        assert!(
            require(&Coverage::default(), Level::ProviderSigned, 1, 1).is_ok(),
            "nothing to back"
        );
        assert_eq!(Level::parse("notarized").unwrap(), Level::Notarized);
    }
}
