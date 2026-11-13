//! X25519MLKEM768: published vectors for both components, round trip and
//! tamper/validation behaviour of the hybrid.

use ml_kem::{Decapsulate, KeyExport, MlKem768};
use shake::Shake128;
use shake::{ExtendableOutput, Update, XofReader};
use vesper_vault::kem::{
    CLIENT_SHARE_LEN, HybridClientSecret, KemError, MLKEM768_CT_LEN, MLKEM768_EK_LEN,
    SERVER_SHARE_LEN, server_encapsulate_for_testing,
};
use vesper_vault::{LeakAuditor, LockPolicy, ObligationKind, SystemRng, server_encapsulate};

fn hex(s: &str) -> Vec<u8> {
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap())
        .collect()
}

fn to_hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

/// RFC 7748 §6.1 X25519 Diffie-Hellman vector, through the hybrid's X25519
/// half: the server's ephemeral scalar is Bob's, the client's is Alice's.
#[test]
fn x25519_half_matches_rfc7748() {
    let alice_sk: [u8; 32] =
        hex("77076d0a7318a57d3c16c17251b26645df4c2f87ebc0992ab177fba51db92c2a")
            .try_into()
            .unwrap();
    let bob_sk: [u8; 32] = hex("5dab087e624a8a4b79e17f8b83800ee66f3bb1292618b6fd1c2f8b27ff88e0eb")
        .try_into()
        .unwrap();
    let alice_pk = hex("8520f0098930a754748b7ddcb43ef75a0dbf3a0d26381af4eba4a98eaa9b4e6a");
    let bob_pk = hex("de9edb7d7b7dc1b4d35b61c2ece435373f8343c85b78674dadfc7e146f882b4f");
    let shared = hex("4a5d9d5ba4ce2de1728e3bf480350f25e07e21c947d19e3376f09b3c1e161742");

    let mut seed = [3u8; 64];
    let mut a = alice_sk;
    let (client, share) =
        HybridClientSecret::from_seeds_for_testing(&mut seed, &mut a, LockPolicy::BestEffort)
            .unwrap();
    assert_eq!(&share[MLKEM768_EK_LEN..], &alice_pk[..]);
    let (server_share, server_ss) =
        server_encapsulate_for_testing(&share, &[9u8; 32], &bob_sk, LockPolicy::BestEffort)
            .unwrap();
    assert_eq!(&server_share[MLKEM768_CT_LEN..], &bob_pk[..]);
    server_ss.with(|s| assert_eq!(&s[32..], &shared[..]));
    let client_ss = client
        .decapsulate(&server_share, LockPolicy::BestEffort)
        .unwrap();
    client_ss.with(|c| server_ss.with(|s| assert_eq!(c, s)));
    let _ = client_ss.shred();
    let _ = server_ss.shred();
}

/// The C2SP/Go "accumulated" ML-KEM-768 vectors (crypto/mlkem TestAccumulated):
/// seeds, messages and random ciphertexts are drawn from SHAKE128(""), every
/// output is absorbed into a second SHAKE128, and the 32-byte digest is
/// compared with the published value. 100 iterations by default (Go's
/// -short value); set VAULT_MLKEM_ACCUMULATED=10000 for the full vector.
#[test]
fn mlkem768_matches_accumulated_vectors() {
    let n: usize = std::env::var("VAULT_MLKEM_ACCUMULATED")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(100);
    let expected = match n {
        100 => "1114b1b6699ed191734fa339376afa7e285c9e6acf6ff0177d346696ce564415",
        10000 => "8a518cc63da366322a8e7a818c7a0d63483cb3528d34a4cf42f35d5ad73f22fc",
        other => panic!("no published accumulated value for {other} iterations"),
    };
    let mut s = Shake128::default().finalize_xof();
    let mut o = Shake128::default();
    for _ in 0..n {
        let mut seed = [0u8; 64];
        s.read(&mut seed);
        let dk = ml_kem::DecapsulationKey::<MlKem768>::from_seed(seed.into());
        let ek = dk.encapsulation_key();
        o.update(ek.to_bytes().as_slice());
        let mut msg = [0u8; 32];
        s.read(&mut msg);
        let (ct, k) = ek.encapsulate_deterministic(&msg.into());
        o.update(ct.as_slice());
        o.update(k.as_slice());
        assert_eq!(dk.decapsulate(&ct), k);
        let mut ct1 = [0u8; MLKEM768_CT_LEN];
        s.read(&mut ct1);
        let k1 = dk.decapsulate(&ct1.into());
        o.update(k1.as_slice());
    }
    let mut digest = [0u8; 32];
    o.finalize_xof().read(&mut digest);
    assert_eq!(to_hex(&digest), expected);
}

/// The hybrid's ML-KEM half equals plain ML-KEM-768 on the same seeds, and
/// the shared secret is ML-KEM secret || X25519 secret.
#[test]
fn hybrid_combiner_is_mlkem_then_x25519() {
    let seed = [0x42u8; 64];
    let m = [0x17u8; 32];
    let dk = ml_kem::DecapsulationKey::<MlKem768>::from_seed(seed.into());
    let (ct, k) = dk.encapsulation_key().encapsulate_deterministic(&m.into());
    let mut seed_in = seed;
    let mut x = [5u8; 32];
    let (client, share) =
        HybridClientSecret::from_seeds_for_testing(&mut seed_in, &mut x, LockPolicy::BestEffort)
            .unwrap();
    assert_eq!(seed_in, [0u8; 64], "seed input zeroized");
    assert_eq!(
        &share[..MLKEM768_EK_LEN],
        dk.encapsulation_key().to_bytes().as_slice()
    );
    let (server_share, ss) =
        server_encapsulate_for_testing(&share, &m, &[6u8; 32], LockPolicy::BestEffort).unwrap();
    assert_eq!(&server_share[..MLKEM768_CT_LEN], ct.as_slice());
    ss.with(|s| assert_eq!(&s[..32], k.as_slice()));
    let cs = client
        .decapsulate(&server_share, LockPolicy::BestEffort)
        .unwrap();
    cs.with(|c| ss.with(|s| assert_eq!(c, s)));
    let _ = cs.shred();
    let _ = ss.shred();
}

#[test]
fn random_round_trip_with_system_rng() {
    let mut rng = SystemRng;
    for _ in 0..4 {
        let (client, share) =
            HybridClientSecret::generate(&mut rng, LockPolicy::BestEffort).unwrap();
        assert_eq!(share.len(), CLIENT_SHARE_LEN);
        let (server_share, ss) =
            server_encapsulate(&mut rng, &share, LockPolicy::BestEffort).unwrap();
        assert_eq!(server_share.len(), SERVER_SHARE_LEN);
        let cs = client
            .decapsulate(&server_share, LockPolicy::BestEffort)
            .unwrap();
        cs.with(|c| ss.with(|s| assert_eq!(c, s)));
        assert!(cs.shred().verified_zero);
        let _ = ss.shred();
    }
}

#[test]
fn tampered_shares_do_not_agree_or_are_rejected() {
    let mut rng = SystemRng;
    let (client, share) = HybridClientSecret::generate(&mut rng, LockPolicy::BestEffort).unwrap();
    let (mut server_share, ss) =
        server_encapsulate(&mut rng, &share, LockPolicy::BestEffort).unwrap();

    // Wrong length: rejected, the secret comes back.
    let err = client
        .decapsulate(
            &server_share[..SERVER_SHARE_LEN - 1],
            LockPolicy::BestEffort,
        )
        .unwrap_err();
    assert_eq!(
        err.error,
        KemError::BadShareLength {
            expected: SERVER_SHARE_LEN,
            found: SERVER_SHARE_LEN - 1
        }
    );
    let client = err.secret;

    // A flipped ciphertext bit: ML-KEM implicit rejection gives a different
    // (pseudo-random) secret, so the two sides disagree.
    server_share[100] ^= 1;
    let cs = client
        .decapsulate(&server_share, LockPolicy::BestEffort)
        .unwrap();
    cs.with(|c| {
        ss.with(|s| {
            assert_ne!(&c[..32], &s[..32]);
            assert_eq!(&c[32..], &s[32..], "X25519 half untouched");
        })
    });
    let _ = cs.shred();
    let _ = ss.shred();

    // A low-order X25519 point (all zero) is rejected on both sides.
    let (client, mut share) =
        HybridClientSecret::generate(&mut rng, LockPolicy::BestEffort).unwrap();
    let good_share = share;
    share[MLKEM768_EK_LEN..].fill(0);
    assert_eq!(
        server_encapsulate(&mut rng, &share, LockPolicy::BestEffort).unwrap_err(),
        KemError::NonContributoryX25519
    );
    let (mut server_share, ss) =
        server_encapsulate(&mut rng, &good_share, LockPolicy::BestEffort).unwrap();
    server_share[MLKEM768_CT_LEN..].fill(0);
    let err = client
        .decapsulate(&server_share, LockPolicy::BestEffort)
        .unwrap_err();
    assert_eq!(err.error, KemError::NonContributoryX25519);
    assert!(err.secret.abandon().verified_zero);
    let _ = ss.shred();

    // An encapsulation key with a coefficient >= q fails FIPS 203 validation.
    let mut bad = good_share;
    bad[0] = 0xFF;
    bad[1] = 0xFF;
    assert_eq!(
        server_encapsulate(&mut rng, &bad, LockPolicy::BestEffort).unwrap_err(),
        KemError::InvalidEncapsulationKey
    );
    assert!(matches!(
        server_encapsulate(&mut rng, &good_share[..10], LockPolicy::BestEffort),
        Err(KemError::BadShareLength { .. })
    ));
}

#[test]
fn dropped_client_secret_is_counted() {
    let before = LeakAuditor::this_thread().count(ObligationKind::KemSecret);
    let (client, _) = HybridClientSecret::generate(&mut SystemRng, LockPolicy::BestEffort).unwrap();
    drop(client);
    assert_eq!(
        LeakAuditor::this_thread().count(ObligationKind::KemSecret),
        before + 1
    );
}
