//! Steering specs and steering receipts (see `docs/steering.md`).
//!
//! A [`SteerSpec`] says exactly which intervention to apply to which model's activations. It is a
//! signed record, so a proposer cannot deny it, and it has a [`SteerSpec::digest`] that does not depend
//! on the signer, so the same intervention proposed by two members has one identity. A [`SteerReceipt`]
//! is signed by the host that ran it and binds the spec digest to the weights, prompt set, container
//! image and output actually used.
//!
//! Two choices keep the format canonical:
//! - Strengths are integers in thousandths of the mean residual norm at the site (`coef_milli`), never
//!   floats, so there is one text form per spec and a bound means the same thing across models.
//! - `direction` names a unit-norm vector by hash, and the strength is separate, so angle and norm can be
//!   bounded independently.
//!
//! Sites are limited to the residual-stream basis (`resid_post`, `attn_out`, `mlp_out`). Directions at
//! those sites do not change under function-preserving permutations of MLP hidden units, which is the
//! failure `theseus-lab` showed for positional labels. Hidden-unit sites are deliberately not offered.
//!
//! # What this does and does not prove
//! A spec binds a request to weights and a vector by hash; it does not say the vector means what its
//! proposer claims. A receipt is a host's signed claim about what it ran; it is only as strong as the
//! host's attestation. [`Ceiling`] states that on the receipt itself.

use crate::record::{self, hash_parts, hex};
use ed25519_dalek::SigningKey;

const SPEC_DOMAIN: &str = "playbox.steer-spec.v1";
const RECEIPT_DOMAIN: &str = "playbox.steer-receipt.v1";

/// Strength scale: `coef_milli` of 1000 is one mean residual norm.
pub const COEF_UNIT: i64 = 1000;
/// Hard format limits, so a spec cannot be arbitrarily large.
pub const MAX_LAYERS: usize = 256;
pub const MAX_COEF_MILLI: i64 = 100_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Site {
    ResidPost,
    AttnOut,
    MlpOut,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Recipe {
    DiffMean,
    Probe,
    SaeFeature,
    ReftR1,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// Add `coef * direction`.
    Add,
    /// Set the component along `direction` to `coef`.
    Clamp,
    /// Project the component along `direction` out. `coef_milli` must be 0.
    Ablate,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tokens {
    Prompt,
    Generated,
    All,
}

macro_rules! names {
    ($t:ident { $($v:ident => $s:literal),+ $(,)? }) => {
        impl $t {
            fn name(self) -> &'static str { match self { $($t::$v => $s),+ } }
            fn parse(s: &str) -> Result<$t, String> {
                match s { $($s => Ok($t::$v),)+ o => Err(format!("unknown {} {o:?}", stringify!($t))) }
            }
        }
    };
}
names!(Site { ResidPost => "resid_post", AttnOut => "attn_out", MlpOut => "mlp_out" });
names!(Recipe { DiffMean => "diffmean", Probe => "probe", SaeFeature => "sae_feature", ReftR1 => "reft_r1" });
names!(Mode { Add => "add", Clamp => "clamp", Ablate => "ablate" });
names!(Tokens { Prompt => "prompt", Generated => "generated", All => "all" });

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SteerSpec {
    /// Human label, e.g. `gemma-2-2b-it`. Identity is `weights`, not this.
    pub model: String,
    /// Hash of the weights manifest.
    pub weights: [u8; 32],
    pub site: Site,
    /// Strictly increasing layer indices.
    pub layers: Vec<u32>,
    /// Hash of the unit-norm direction file.
    pub direction: [u8; 32],
    pub recipe: Recipe,
    pub mode: Mode,
    /// Strength in thousandths of the mean residual norm at the site. May be 0 (the no-op control).
    pub coef_milli: i64,
    pub tokens: Tokens,
    /// Hash of the prompt-set manifest.
    pub prompts: [u8; 32],
    pub seed: u64,
}

impl SteerSpec {
    pub fn validate(&self) -> Result<(), String> {
        let label_ok = !self.model.is_empty()
            && self.model.len() <= 128
            && self.model.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-' | '/'));
        if !label_ok {
            return Err(format!("bad model label {:?}", self.model));
        }
        if self.layers.is_empty() || self.layers.len() > MAX_LAYERS {
            return Err(format!("need 1 to {MAX_LAYERS} layers"));
        }
        if self.layers.windows(2).any(|w| w[0] >= w[1]) {
            return Err("layers must be strictly increasing".into());
        }
        if self.coef_milli.abs() > MAX_COEF_MILLI {
            return Err(format!("strength beyond {MAX_COEF_MILLI} milli-norms"));
        }
        if self.mode == Mode::Ablate && self.coef_milli != 0 {
            return Err("ablate takes no strength".into());
        }
        Ok(())
    }

    fn fields(&self) -> Vec<(&'static str, String)> {
        vec![
            ("model", self.model.clone()),
            ("weights", hex(&self.weights)),
            ("site", self.site.name().into()),
            ("layers", self.layers.iter().map(u32::to_string).collect::<Vec<_>>().join(",")),
            ("direction", hex(&self.direction)),
            ("recipe", self.recipe.name().into()),
            ("mode", self.mode.name().into()),
            ("coef_milli", self.coef_milli.to_string()),
            ("tokens", self.tokens.name().into()),
            ("prompts", hex(&self.prompts)),
            ("seed", self.seed.to_string()),
        ]
    }

    /// The `key value` lines of the spec, without domain, signer or signature. This is what a sandbox
    /// reads, after the host has checked the signed record.
    pub fn canonical_body(&self) -> Result<String, String> {
        self.validate()?;
        Ok(self.fields().iter().map(|(k, v)| format!("{k} {v}\n")).collect())
    }

    /// Identity of the intervention, independent of who proposed it.
    pub fn digest(&self) -> Result<[u8; 32], String> {
        Ok(hash_parts(SPEC_DOMAIN, &[self.canonical_body()?.as_bytes()]))
    }

    /// The proposer's signed record.
    pub fn sign(&self, proposer: &SigningKey) -> Result<String, String> {
        self.validate()?;
        record::sign(SPEC_DOMAIN, &self.fields(), proposer)
    }

    /// Parse and verify a signed spec. Returns the spec and the proposer's key.
    pub fn open(text: &str) -> Result<(SteerSpec, [u8; 32]), String> {
        let r = record::open(SPEC_DOMAIN, text)?;
        let layers = r
            .list("layers")?
            .into_iter()
            .map(|l| l.parse::<u32>().map_err(|_| format!("bad layer {l:?}")))
            .collect::<Result<Vec<_>, _>>()?;
        let spec = SteerSpec {
            model: r.req("model")?.to_string(),
            weights: r.hash("weights")?,
            site: Site::parse(r.req("site")?)?,
            layers,
            direction: r.hash("direction")?,
            recipe: Recipe::parse(r.req("recipe")?)?,
            mode: Mode::parse(r.req("mode")?)?,
            coef_milli: r.req("coef_milli")?.parse().map_err(|_| "coef_milli is not a number".to_string())?,
            tokens: Tokens::parse(r.req("tokens")?)?,
            prompts: r.hash("prompts")?,
            seed: r.num("seed")?,
        };
        spec.validate()?;
        // A signed record must be the canonical text of the spec it parses to: no extra or reordered fields.
        let canonical: Vec<&str> = spec.fields().iter().map(|(k, _)| *k).collect();
        let got: Vec<&str> = r.fields.iter().map(|(k, _)| k.as_str()).collect();
        if canonical != got {
            return Err("fields are not in canonical order".into());
        }
        Ok((spec, r.signer))
    }
}

/// How strong a claim the receipt's host stands behind. Stated on the receipt so a result cannot be
/// quoted without its ceiling.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Ceiling {
    /// The host says so; nothing verifies it.
    HostClaim,
    /// Run on an operator-controlled machine for internal exploration.
    LocalInternalExploratory,
    /// The host's software and hardware were attested at the tier the room required.
    Attested,
}
names!(Ceiling { HostClaim => "host_claim", LocalInternalExploratory => "local_internal_exploratory", Attested => "attested" });

/// A host's signed record of one steering run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SteerReceipt {
    pub spec_digest: [u8; 32],
    pub weights: [u8; 32],
    pub prompts: [u8; 32],
    /// Digest of the container image that applied the hooks.
    pub image: [u8; 32],
    pub request_hash: [u8; 32],
    pub output_hash: [u8; 32],
    pub ceiling: Ceiling,
    pub time_ms: u64,
    /// The host key that signed this receipt.
    pub host: [u8; 32],
}

impl SteerReceipt {
    /// Sign a run of `spec`. `request` and `output` are the exact bytes sent and returned.
    pub fn sign(spec: &SteerSpec, image: [u8; 32], request: &[u8], output: &[u8], ceiling: Ceiling, time_ms: u64, host: &SigningKey) -> Result<String, String> {
        record::sign(
            RECEIPT_DOMAIN,
            &[
                ("spec", hex(&spec.digest()?)),
                ("weights", hex(&spec.weights)),
                ("prompts", hex(&spec.prompts)),
                ("image", hex(&image)),
                ("request", hex(blake3::hash(request).as_bytes())),
                ("output", hex(blake3::hash(output).as_bytes())),
                ("ceiling", ceiling.name().into()),
                ("time", time_ms.to_string()),
            ],
            host,
        )
    }

    pub fn open(text: &str) -> Result<SteerReceipt, String> {
        let r = record::open(RECEIPT_DOMAIN, text)?;
        Ok(SteerReceipt {
            spec_digest: r.hash("spec")?,
            weights: r.hash("weights")?,
            prompts: r.hash("prompts")?,
            image: r.hash("image")?,
            request_hash: r.hash("request")?,
            output_hash: r.hash("output")?,
            ceiling: Ceiling::parse(r.req("ceiling")?)?,
            time_ms: r.num("time")?,
            host: r.signer,
        })
    }

    /// Check that this receipt is for `spec` and for these output bytes. Does not check the host's key;
    /// the caller decides which hosts it trusts.
    pub fn matches(&self, spec: &SteerSpec, output: &[u8]) -> Result<(), String> {
        if self.spec_digest != spec.digest()? {
            return Err("receipt is for a different spec".into());
        }
        if self.weights != spec.weights || self.prompts != spec.prompts {
            return Err("receipt weights or prompts differ from the spec".into());
        }
        if self.output_hash != *blake3::hash(output).as_bytes() {
            return Err("output does not match the receipt".into());
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(n: u8) -> SigningKey {
        SigningKey::from_bytes(&[n; 32])
    }

    fn spec() -> SteerSpec {
        SteerSpec {
            model: "gemma-2-2b-it".into(),
            weights: [1; 32],
            site: Site::ResidPost,
            layers: vec![12, 13, 14],
            direction: [2; 32],
            recipe: Recipe::DiffMean,
            mode: Mode::Add,
            coef_milli: 6000,
            tokens: Tokens::All,
            prompts: [3; 32],
            seed: 7,
        }
    }

    #[test]
    fn spec_round_trips_and_resists_tampering() {
        let t = spec().sign(&key(1)).unwrap();
        let (s, who) = SteerSpec::open(&t).unwrap();
        assert_eq!(s, spec());
        assert_eq!(who, key(1).verifying_key().to_bytes());
        assert!(SteerSpec::open(&t.replace("coef_milli 6000", "coef_milli 9000")).is_err());
        assert!(SteerSpec::open(&t.replace("layers 12,13,14", "layers 12,13")).is_err());
        assert!(SteerSpec::open(&format!("{t}extra 1\n")).is_err());
    }

    #[test]
    fn digest_ignores_signer_and_covers_every_field() {
        let base = spec().digest().unwrap();
        let a = SteerSpec::open(&spec().sign(&key(1)).unwrap()).unwrap().0;
        let b = SteerSpec::open(&spec().sign(&key(2)).unwrap()).unwrap().0;
        assert_eq!((a.digest().unwrap(), b.digest().unwrap()), (base, base));

        let variants: Vec<SteerSpec> = vec![
            SteerSpec { model: "gemma-2-9b-it".into(), ..spec() },
            SteerSpec { weights: [9; 32], ..spec() },
            SteerSpec { site: Site::MlpOut, ..spec() },
            SteerSpec { layers: vec![12, 13], ..spec() },
            SteerSpec { direction: [9; 32], ..spec() },
            SteerSpec { recipe: Recipe::Probe, ..spec() },
            SteerSpec { mode: Mode::Clamp, ..spec() },
            SteerSpec { coef_milli: 6001, ..spec() },
            SteerSpec { tokens: Tokens::Prompt, ..spec() },
            SteerSpec { prompts: [9; 32], ..spec() },
            SteerSpec { seed: 8, ..spec() },
        ];
        for v in variants {
            assert_ne!(v.digest().unwrap(), base, "{v:?}");
        }
    }

    #[test]
    fn rejects_invalid_specs() {
        for bad in [
            SteerSpec { layers: vec![], ..spec() },
            SteerSpec { layers: vec![14, 12], ..spec() },
            SteerSpec { layers: vec![12, 12], ..spec() },
            SteerSpec { layers: (0..257).collect(), ..spec() },
            SteerSpec { coef_milli: MAX_COEF_MILLI + 1, ..spec() },
            SteerSpec { coef_milli: -MAX_COEF_MILLI - 1, ..spec() },
            SteerSpec { mode: Mode::Ablate, coef_milli: 1, ..spec() },
            SteerSpec { model: "has space".into(), ..spec() },
            SteerSpec { model: String::new(), ..spec() },
        ] {
            assert!(bad.sign(&key(1)).is_err(), "{bad:?}");
            assert!(bad.digest().is_err());
        }
        // The no-op control and a negative strength are valid.
        assert!(SteerSpec { coef_milli: 0, ..spec() }.digest().is_ok());
        assert!(SteerSpec { coef_milli: -4000, ..spec() }.digest().is_ok());
        assert!(SteerSpec { mode: Mode::Ablate, coef_milli: 0, ..spec() }.digest().is_ok());
    }

    #[test]
    fn open_rejects_non_canonical_and_foreign_records() {
        let s = spec();
        let mut fields = s.fields();
        fields.swap(0, 1);
        let reordered = record::sign(SPEC_DOMAIN, &fields, &key(1)).unwrap();
        assert!(SteerSpec::open(&reordered).is_err(), "reordered fields");
        let mut extra = s.fields();
        extra.push(("note", "hi".into()));
        assert!(SteerSpec::open(&record::sign(SPEC_DOMAIN, &extra, &key(1)).unwrap()).is_err(), "extra field");
        let other = record::sign("playbox.model-receipt.v1", &s.fields(), &key(1)).unwrap();
        assert!(SteerSpec::open(&other).is_err(), "wrong domain");
        let bad_layer = s.sign(&key(1)).unwrap().replace("layers 12,13,14", "layers 12,x");
        assert!(SteerSpec::open(&bad_layer).is_err());
    }

    #[test]
    fn receipt_binds_spec_output_and_ceiling() {
        let s = spec();
        let t = SteerReceipt::sign(&s, [4; 32], b"prompt", b"steered text", Ceiling::LocalInternalExploratory, 1000, &key(5)).unwrap();
        let r = SteerReceipt::open(&t).unwrap();
        assert_eq!(r.host, key(5).verifying_key().to_bytes());
        assert_eq!((r.weights, r.prompts, r.image), ([1; 32], [3; 32], [4; 32]));
        assert_eq!(r.ceiling, Ceiling::LocalInternalExploratory);
        r.matches(&s, b"steered text").unwrap();

        assert!(r.matches(&s, b"other text").is_err(), "different output");
        let other = SteerSpec { coef_milli: 7000, ..spec() };
        assert!(r.matches(&other, b"steered text").is_err(), "different spec");
        assert!(SteerReceipt::open(&t.replace("local_internal_exploratory", "attested")).is_err(), "ceiling edited");
        assert!(SteerReceipt::open(&t.replace("time 1000", "time 1001")).is_err(), "time edited");
        assert!(SteerReceipt::open(&s.sign(&key(5)).unwrap()).is_err(), "a spec is not a receipt");
    }

    #[test]
    fn receipt_for_invalid_spec_is_refused() {
        let bad = SteerSpec { layers: vec![], ..spec() };
        assert!(SteerReceipt::sign(&bad, [0; 32], b"", b"", Ceiling::HostClaim, 0, &key(5)).is_err());
    }

    #[test]
    fn ceilings_are_ordered_weakest_first() {
        assert!(Ceiling::HostClaim < Ceiling::LocalInternalExploratory && Ceiling::LocalInternalExploratory < Ceiling::Attested);
    }
}
