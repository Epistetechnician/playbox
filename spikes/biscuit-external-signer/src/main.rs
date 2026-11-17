//! Spike: can a Biscuit's authority block be signed by something other than a biscuit-auth KeyPair?
//! Build with a throwaway root, swap the authority signature for one made by an external signer over the
//! same payload, then verify with biscuit-auth's own verifier under the external public key.
use biscuit_auth::{builder::BiscuitBuilder, format::schema, Biscuit, KeyPair, PublicKey};
use prost::Message;

/// biscuit-auth 6.0.0 `generate_authority_block_signature_payload_v1` (pub(crate), so reproduced here).
fn authority_payload(block: &[u8], next_key: &[u8], version: u32) -> Vec<u8> {
    if version == 0 {
        // v0: payload || algorithm (i32 LE) || next key
        let mut p = block.to_vec();
        p.extend(0i32.to_le_bytes());
        p.extend(next_key);
        return p;
    }
    let mut p = b"\0BLOCK\0\0VERSION\0".to_vec();
    p.extend(version.to_le_bytes());
    p.extend(b"\0PAYLOAD\0");
    p.extend(block);
    p.extend(b"\0ALGORITHM\0");
    p.extend(0i32.to_le_bytes()); // Ed25519
    p.extend(b"\0NEXTKEY\0");
    p.extend(next_key);
    p
}

/// Re-sign the authority block of `token` with `sign`, which sees only the payload.
fn resign(token: &[u8], mut sign: impl FnMut(&[u8]) -> [u8; 64]) -> Vec<u8> {
    let mut b = schema::Biscuit::decode(token).unwrap();
    let version = b.authority.version.unwrap_or(0);
    let payload = authority_payload(&b.authority.block, &b.authority.next_key.key, version);
    b.authority.signature = sign(&payload).to_vec();
    b.encode_to_vec()
}

fn grant(root: &KeyPair) -> Vec<u8> {
    let b = BiscuitBuilder::new()
        .code(r#"member("m1"); right("pull"); check if time($t), $t < 2100-01-01T00:00:00Z;"#)
        .unwrap()
        .build(root)
        .unwrap();
    b.to_vec().unwrap()
}

fn verified(token: &[u8], root: &PublicKey) -> Result<Biscuit, String> {
    Biscuit::from(token, root).map_err(|e| e.to_string())
}

fn main() {
    // 1. A plain ed25519 key outside biscuit-auth stands in for "any external signer".
    use ed25519_dalek::{Signer, SigningKey};
    let ext = SigningKey::generate(&mut rand::rngs::OsRng);
    let ext_pub = PublicKey::from_bytes(ext.verifying_key().as_bytes(), biscuit_auth::Algorithm::Ed25519).unwrap();
    let throwaway = KeyPair::new();
    let t = grant(&throwaway);
    assert!(verified(&t, &ext_pub).is_err(), "must not verify before re-signing");
    let t2 = resign(&t, |p| ext.sign(p).to_bytes());
    let b = verified(&t2, &ext_pub).expect("external ed25519 signature verifies under biscuit-auth");
    println!("1. external ed25519 signer: authority re-signed, verifies ({} block)", b.block_count());

    // 1b. The token is still attenuable by a holder, with no help from the signer.
    let attenuated = b
        .append(biscuit_auth::builder::BlockBuilder::new().code(r#"check if right("pull");"#).unwrap())
        .ok();
    println!("1b. attenuation by holder: {}", if attenuated.is_some() { "works" } else { "failed" });

    // 2. A wrong payload (flip one bit) must NOT verify, so (1) is not passing vacuously.
    let bad = resign(&t, |p| {
        let mut q = p.to_vec();
        q[20] ^= 1;
        ext.sign(&q).to_bytes()
    });
    assert!(verified(&bad, &ext_pub).is_err());
    println!("2. tampered payload signature is rejected");

    // 3. FROST (RFC 9591) 2-of-3: signers never hold the full key; the group signature is a plain Ed25519 one.
    use frost_ed25519 as frost;
    use std::collections::BTreeMap;
    let mut rng = rand::thread_rng();
    let (shares, pubkeys) =
        frost::keys::generate_with_dealer(3, 2, frost::keys::IdentifierList::Default, &mut rng).unwrap();
    let group_pub = PublicKey::from_bytes(&pubkeys.verifying_key().serialize().unwrap(), biscuit_auth::Algorithm::Ed25519).unwrap();
    let t = grant(&throwaway);
    let t3 = resign(&t, |p| {
        let mut nonces = BTreeMap::new();
        let mut commitments = BTreeMap::new();
        let mut kps = BTreeMap::new();
        for (id, share) in shares.iter().take(2) {
            let kp = frost::keys::KeyPackage::try_from(share.clone()).unwrap();
            let (n, c) = frost::round1::commit(kp.signing_share(), &mut rng);
            nonces.insert(*id, n);
            commitments.insert(*id, c);
            kps.insert(*id, kp);
        }
        let pkg = frost::SigningPackage::new(commitments, p);
        let mut sigs = BTreeMap::new();
        for (id, kp) in &kps {
            sigs.insert(*id, frost::round2::sign(&pkg, &nonces[id], kp).unwrap());
        }
        let sig = frost::aggregate(&pkg, &sigs, &pubkeys).unwrap();
        sig.serialize().unwrap().try_into().unwrap()
    });
    let b = verified(&t3, &group_pub).expect("FROST group signature verifies under biscuit-auth");
    println!("3. FROST 2-of-3: group-signed grant verifies under the single group key ({} block)", b.block_count());
}
