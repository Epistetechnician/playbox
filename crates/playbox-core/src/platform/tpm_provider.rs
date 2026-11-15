//! The TPM provider: drives `tpm2-tools` to produce quotes and read EK certificates.

use super::tpm_verify::{measurement_digest, parse_quote, MEASUREMENT_PCR, QUOTED_PCRS, TPM_KIND};
use super::{hex, unhex};
use crate::attest::{AttestError, Measurements, PlatformProvider, Produced, Tier};
use crate::tpm_ek;
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    fs,
    io::{self, Read},
    path::PathBuf,
    process::Command,
    sync::Mutex,
};

/// Produces TPM quotes with the `tpm2-tools` programs (Linux). Honors `TPM2TOOLS_TCTI`, so it also
/// works against a software TPM such as `swtpm`. Creates a persistent attestation key on first use.
pub struct TpmProvider {
    /// Persistent handle for the attestation key.
    pub ak_handle: String,
    lock: Mutex<()>,
}

impl Default for TpmProvider {
    fn default() -> Self {
        Self {
            ak_handle: "0x81010002".into(),
            lock: Mutex::new(()),
        }
    }
}

pub(super) fn run(prog: &str, args: &[&str]) -> Result<Vec<u8>, AttestError> {
    let out = Command::new(prog)
        .args(args)
        .output()
        .map_err(|e| format!("could not run {prog}: {e} (is tpm2-tools installed?)"))?;
    if !out.status.success() {
        return Err(format!(
            "{prog} failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        )
        .into());
    }
    Ok(out.stdout)
}

/// Clear transient objects a tool left loaded (see [`TpmProvider::produce`]). Failure is fine.
pub(super) fn flush() {
    let _ = run("tpm2_flushcontext", &["-t"]);
}

pub(super) struct TempDir(PathBuf);

impl TempDir {
    fn new() -> io::Result<Self> {
        let mut b = [0u8; 8];
        fs::File::open("/dev/urandom")?.read_exact(&mut b)?;
        let p = std::env::temp_dir().join(format!("playbox-tpm-{}", hex(&b)));
        fs::create_dir(&p)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&p, fs::Permissions::from_mode(0o700))?;
        }
        Ok(Self(p))
    }
    fn path(&self, name: &str) -> String {
        self.0.join(name).to_string_lossy().into_owned()
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

impl TpmProvider {
    fn ensure_ak(&self, tmp: &TempDir) -> Result<(), AttestError> {
        if run(
            "tpm2_readpublic",
            &["-c", &self.ak_handle, "-o", &tmp.path("probe.pub")],
        )
        .is_ok()
        {
            return Ok(());
        }
        // The attestation key is created under the endorsement key (the TPM's identity key).
        run("tpm2_createek", &["-c", &tmp.path("ek.ctx"), "-G", "ecc"])?;
        let _ = run("tpm2_flushcontext", &["-t"]);
        run(
            "tpm2_createak",
            &[
                "-C",
                &tmp.path("ek.ctx"),
                "-c",
                &tmp.path("ak.ctx"),
                "-G",
                "ecc",
                "-g",
                "sha256",
                "-s",
                "ecdsa",
            ],
        )?;
        run(
            "tpm2_evictcontrol",
            &["-C", "o", "-c", &tmp.path("ak.ctx"), &self.ak_handle],
        )?;
        Ok(())
    }
}

/// NV indices where TCG puts the EK certificates: RSA 2048 first, then ECC P-256.
pub(super) const EK_CERT_NV_INDICES: [&str; 2] = ["0x01c00002", "0x01c0000a"];

/// Intermediate certificates the operator supplies for the EK certificate: `$PLAYBOX_TPM_EK_CHAIN`, a PEM file.
pub(super) fn ek_chain_from_env() -> Option<Vec<Vec<u8>>> {
    let path = std::env::var_os("PLAYBOX_TPM_EK_CHAIN")?;
    let ders = tpm_ek::pem_to_ders(&fs::read_to_string(path).ok()?).ok()?;
    Some(ders.into_iter().take(4).collect())
}

impl TpmProvider {
    fn read_ek_cert(&self, tmp: &TempDir) -> Option<Vec<u8>> {
        for (i, idx) in EK_CERT_NV_INDICES.iter().enumerate() {
            let out = tmp.path(&format!("ek{i}.der"));
            if run("tpm2_nvread", &[idx, "-C", "o", "-o", &out]).is_ok() {
                if let Ok(der) = fs::read(&out) {
                    if let Ok(t) = tpm_ek::trim_der(&der) {
                        return Some(t.to_vec());
                    }
                }
            }
        }
        None
    }
}

impl PlatformProvider for TpmProvider {
    fn kind(&self) -> &str {
        TPM_KIND
    }

    /// Credential activation: decrypt a credential a verifier made for our endorsement key and attestation
    /// key. Only this TPM can, which is the proof.
    fn respond(&self, request: &[u8]) -> Result<Vec<u8>, AttestError> {
        if request.len() > 8192 {
            return Err("challenge is too large".into());
        }
        let text = std::str::from_utf8(request).map_err(|_| "challenge is not utf-8")?;
        let mut f: BTreeMap<&str, &str> = BTreeMap::new();
        for line in text.lines() {
            let (k, v) = line.split_once(' ').ok_or("bad challenge line")?;
            if f.insert(k, v).is_some() {
                return Err("duplicate challenge field".into());
            }
        }
        let ek_kind = *f.get("ek").ok_or("challenge has no `ek`")?;
        if ek_kind != "rsa" && ek_kind != "ecc" {
            return Err("challenge `ek` must be rsa or ecc".into());
        }
        let cred = tpm_ek::Credential {
            blob: unhex(f.get("cred").ok_or("challenge has no `cred`")?)?,
            secret: unhex(f.get("secret").ok_or("challenge has no `secret`")?)?,
        };
        let _guard = self.lock.lock().map_err(|_| "tpm lock poisoned")?;
        let tmp = TempDir::new().map_err(|e| e.to_string())?;
        let (credf, ekctx, sess, out) = (
            tmp.path("cred.bin"),
            tmp.path("ek.ctx"),
            tmp.path("sess.ctx"),
            tmp.path("secret.out"),
        );
        fs::write(&credf, cred.to_file()).map_err(|e| e.to_string())?;
        flush();
        run("tpm2_createek", &["-c", &ekctx, "-G", ek_kind])?;
        flush();
        // The endorsement key can only be used under a policy session that proves knowledge of the
        // endorsement hierarchy's authorization (empty on a normal TPM).
        run("tpm2_startauthsession", &["--policy-session", "-S", &sess])?;
        let result = run("tpm2_policysecret", &["-S", &sess, "-c", "e"]).and_then(|_| {
            run(
                "tpm2_activatecredential",
                &[
                    "-c",
                    &self.ak_handle,
                    "-C",
                    &ekctx,
                    "-i",
                    &credf,
                    "-o",
                    &out,
                    "-P",
                    &format!("session:{sess}"),
                ],
            )
        });
        let _ = run("tpm2_flushcontext", &["-l"]);
        flush();
        result?;
        let secret = fs::read(&out).map_err(|e| e.to_string())?;
        if secret.is_empty() || secret.len() > 64 {
            return Err("TPM returned an unexpected credential".into());
        }
        Ok(secret)
    }

    fn produce(&self, binding: &[u8; 32], m: &Measurements) -> Result<Produced, AttestError> {
        let _guard = self.lock.lock().map_err(|_| "tpm lock poisoned")?;
        let tmp = TempDir::new().map_err(|e| e.to_string())?;
        // Without a resource manager (a software TPM, for one) transient objects from an earlier,
        // interrupted call stay loaded and eventually fill the TPM. Calls are serialised by the lock
        // above, so clearing them is safe. Failure is fine: the kernel resource manager refuses it.
        flush();
        self.ensure_ak(&tmp)?;
        flush();

        // Put the declared measurements into the application PCR. If that is not possible the quote is
        // still useful as T1 evidence, but it must not claim T2.
        let digest = measurement_digest(m);
        let measured = run("tpm2_pcrreset", &[&MEASUREMENT_PCR.to_string()]).is_ok()
            && run(
                "tpm2_pcrextend",
                &[&format!("{MEASUREMENT_PCR}:sha256={}", hex(&digest))],
            )
            .is_ok();

        flush();
        let list = format!(
            "sha256:{}",
            QUOTED_PCRS
                .iter()
                .map(u32::to_string)
                .collect::<Vec<_>>()
                .join(",")
        );
        let (msg, sigf, pcrf) = (
            tmp.path("quote.msg"),
            tmp.path("quote.sig"),
            tmp.path("pcrs.bin"),
        );
        // The quote and the PCR values are read by separate commands, so a PCR could change between
        // them. Check the values hash to the quoted digest, and try once more if not.
        let mut raw = Vec::new();
        let mut quote_bytes = Vec::new();
        for attempt in 0..2 {
            flush();
            run(
                "tpm2_quote",
                &[
                    "-c",
                    &self.ak_handle,
                    "-l",
                    &list,
                    "-q",
                    &hex(binding),
                    "-m",
                    &msg,
                    "-s",
                    &sigf,
                    "-g",
                    "sha256",
                ],
            )?;
            flush();
            run("tpm2_pcrread", &[&list, "-o", &pcrf])?;
            raw = fs::read(&pcrf).map_err(|e| e.to_string())?;
            quote_bytes = fs::read(&msg).map_err(|e| e.to_string())?;
            if raw.len() != 32 * QUOTED_PCRS.len() {
                return Err(AttestError::Platform(format!(
                    "unexpected PCR output size {}",
                    raw.len()
                )));
            }
            let quoted = parse_quote(&quote_bytes)?;
            let digest: [u8; 32] = Sha256::digest(&raw).into();
            if digest.as_slice() == quoted.pcr_digest.as_slice() {
                break;
            }
            if attempt == 1 {
                return Err("PCR values keep changing between the quote and the read".into());
            }
        }
        flush();
        let tpmt_path = tmp.path("ak.tpmt");
        run(
            "tpm2_readpublic",
            &["-c", &self.ak_handle, "-f", "tpmt", "-o", &tpmt_path],
        )?;
        let akpub = fs::read(&tpmt_path).map_err(|e| e.to_string())?;
        // Refuse to produce evidence a verifier would reject anyway, with a clearer reason.
        tpm_ek::parse_ak(&akpub)
            .map_err(|e| format!("this TPM's attestation key is unsuitable: {e}"))?;
        let pcrs: Vec<String> = QUOTED_PCRS
            .iter()
            .zip(raw.chunks(32))
            .map(|(i, v)| format!("{i}:{}", hex(v)))
            .collect();
        let mut evidence = format!(
            "akpub {}\nquote {}\nsig {}\npcrs {}\n",
            hex(&akpub),
            hex(&quote_bytes),
            hex(&fs::read(&sigf).map_err(|e| e.to_string())?),
            pcrs.join(",")
        );
        // The endorsement-key certificate, if the TPM has one, so a verifier with trust anchors can
        // check genuineness. Intermediates are not stored in TPMs; an operator can supply them.
        flush();
        if let Some(cert) = self.read_ek_cert(&tmp) {
            evidence.push_str(&format!("ek_cert {}\n", hex(&cert)));
            if let Some(chain) = ek_chain_from_env() {
                evidence.push_str(&format!(
                    "ek_chain {}\n",
                    chain.iter().map(|c| hex(c)).collect::<Vec<_>>().join(",")
                ));
            }
        }
        Ok(Produced {
            evidence,
            claimed: if measured { Tier::T2 } else { Tier::T1 },
        })
    }
}

// ----------------------------------------------------------------------------------------------
// Choosing a provider
// ----------------------------------------------------------------------------------------------
