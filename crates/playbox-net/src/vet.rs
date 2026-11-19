//! Checking a host before showing it an invite: the one place every way of joining (the CLI, the MCP
//! sidecar, reconnects) judges a host, so a stricter rule lands everywhere at once.

use crate::Client;
use anyhow::{bail, Result};
use playbox_core::{
    attest::{PlatformVerifier, Tier, Verified},
    platform,
};
use std::path::Path;

/// What the guest requires of a host.
#[derive(Debug, Clone)]
pub struct VetOpts {
    /// Refuse a host whose attestation achieves less than this.
    pub min_tier: Tier,
    /// Pin the host's device key if it is new to this guest (trust on first use), then judge with it pinned.
    pub trust_device: bool,
}

impl Default for VetOpts {
    fn default() -> Self {
        Self {
            min_tier: Tier::T0,
            trust_device: false,
        }
    }
}

/// Ask the host to attest, check the answer with the platform verifiers and the guest's pinned devices
/// under `home`, and refuse it below `opts.min_tier`. Returns the verified attestation and whether a
/// device was newly pinned. `refusal` starts the error text ("not joining", "not reconnecting").
///
/// Attestation needs no membership, so a host that fails never sees the invite.
///
/// # Errors
///
/// Returns an error if the host does not attest, does not reach the tier, or the pin store cannot be read.
pub async fn vet_host(
    client: &Client,
    home: &Path,
    opts: &VetOpts,
    refusal: &str,
) -> Result<(Verified, bool)> {
    let pins_path = platform::pins_path(home);
    let mut pins = platform::PinStore::load(&pins_path)?;
    let boot = platform::boot_reference_from_env().map_err(|e| anyhow::anyhow!(e))?;
    let make = |pins: &platform::PinStore| {
        platform::verifiers(pins.clone(), boot.clone()).map_err(|e| anyhow::anyhow!(e))
    };
    let mut vs = make(&pins)?;
    let mut att = {
        let refs: Vec<&dyn PlatformVerifier> = vs.iter().map(|v| v.as_ref()).collect();
        match client.verify_host(Tier::T0, &refs).await {
            Ok(v) => v,
            Err(e) => bail!("{refusal}: {e:#}"),
        }
    };
    let mut newly_pinned = false;
    if opts.trust_device {
        if let Some((kind, fp)) = att
            .device
            .clone()
            .as_deref()
            .and_then(|d| d.split_once(':'))
        {
            if !pins.contains(kind, fp) {
                pins.add(kind, fp)?;
                newly_pinned = true;
                vs = make(&pins)?;
                let refs: Vec<&dyn PlatformVerifier> = vs.iter().map(|v| v.as_ref()).collect();
                att = match client.verify_host(Tier::T0, &refs).await {
                    Ok(v) => v,
                    Err(e) => bail!("{refusal}: {e:#}"),
                };
            }
        }
    }
    if att.achieved < opts.min_tier {
        bail!(
            "{refusal}: host achieves {} but {} was required ({})",
            att.achieved,
            opts.min_tier,
            att.notes.join("; ")
        );
    }
    Ok((att, newly_pinned))
}
