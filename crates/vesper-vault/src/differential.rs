//! Differential tests of `@vesper/vault` (`cfg(vesper_port)`) against the
//! frozen Rust implementation in `crate::reference` on identical seeded inputs
//! and error paths.

use crate::aead::{
    AeadAlgorithm, AeadOpener, AeadSealer, Aes256Gcm, AuthError, ChaCha20Poly1305, CiphertextFrame,
    CryptoError, FRAME_HEADER_LEN, FrameHeader, KEY_LEN, SEQ_SPACE_LIMIT,
};
use crate::kem::{
    CLIENT_SHARE_LEN, HybridClientSecret, KemError, KemIsa, MLKEM768_CT_LEN, MLKEM768_EK_LEN,
    SERVER_SHARE_LEN, SHARED_SECRET_LEN, server_encapsulate_for_testing_on,
};
use crate::reference as r;
use crate::slot::{LockPolicy, SecretSlot};
use crate::verify_tag_16_ct;

struct Prng(u64);

impl Prng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_f491_4f6c_dd1d)
    }

    fn fill(&mut self, dst: &mut [u8]) {
        for chunk in dst.chunks_mut(8) {
            let v = self.next().to_le_bytes();
            let n = chunk.len();
            chunk.copy_from_slice(&v[..n]);
        }
    }
}

fn slot32(bytes: [u8; KEY_LEN]) -> SecretSlot<KEY_LEN> {
    SecretSlot::<KEY_LEN>::from_fn(LockPolicy::BestEffort, |k| *k = bytes).unwrap()
}

fn diff_err<T: std::fmt::Debug + PartialEq>(
    v: Result<(), T>,
    r_res: Result<(), T>,
    diffs: &mut usize,
    cases: &mut usize,
    ctx: &str,
) {
    *cases += 1;
    if v != r_res {
        *diffs += 1;
        eprintln!("mismatch ({ctx}): vesper={v:?} rust={r_res:?}");
    }
}

#[test]
fn verify_tag_matches_the_rust_code() {
    let mut diffs = 0usize;
    let mut cases = 0usize;
    let mut rng = Prng(0x7661_756c_745f_7461);

    for fill in [0u8, 0xff] {
        let a = [fill; 16];
        cases += 1;
        assert_eq!(
            verify_tag_16_ct(&a, &a),
            r::secret::verify_tag_16_ct(&a, &a)
        );
    }

    for _ in 0..256 {
        let mut a = [0u8; 16];
        rng.fill(&mut a);
        cases += 1;
        let va = verify_tag_16_ct(&a, &a);
        let ra = r::secret::verify_tag_16_ct(&a, &a);
        if va != ra {
            diffs += 1;
        }
        for bit in 0..128 {
            let mut b = a;
            b[bit / 8] ^= 1 << (bit % 8);
            cases += 1;
            if verify_tag_16_ct(&a, &b) != r::secret::verify_tag_16_ct(&a, &b) {
                diffs += 1;
            }
            cases += 1;
            if verify_tag_16_ct(&b, &a) != r::secret::verify_tag_16_ct(&b, &a) {
                diffs += 1;
            }
        }
        let mut c = [0u8; 16];
        rng.fill(&mut c);
        cases += 1;
        if verify_tag_16_ct(&a, &c) != r::secret::verify_tag_16_ct(&a, &c) {
            diffs += 1;
        }
    }

    assert_eq!(diffs, 0, "verify_tag differential differences={diffs}");
    println!("flip-gate differential differences=0 cases={cases}");
}

#[test]
fn aead_limits_and_tickets_match_the_rust_code() {
    let mut diffs = 0usize;
    let mut cases = 0usize;
    let key = [0x42u8; KEY_LEN];

    // Record limit enforcement and clamping for AES-256-GCM and ChaCha20-Poly1305.
    for &(alg_code, limit, start) in &[
        (Aes256Gcm::ALG_CODE, u64::MAX, 0),
        (Aes256Gcm::ALG_CODE, u64::MAX, 23_726_564),
        (Aes256Gcm::ALG_CODE, 100, 98),
        (Aes256Gcm::ALG_CODE, 10, 10),
        (Aes256Gcm::ALG_CODE, 10, 50),
        (ChaCha20Poly1305::ALG_CODE, u64::MAX, 0),
        (ChaCha20Poly1305::ALG_CODE, u64::MAX, SEQ_SPACE_LIMIT - 2),
        (ChaCha20Poly1305::ALG_CODE, 5, 3),
    ] {
        let mut ref_st =
            r::aead::SealerState::new(999, r::aead::Alg::from_code(alg_code), 7, start, limit);
        if alg_code == Aes256Gcm::ALG_CODE {
            let mut s = AeadSealer::<Aes256Gcm>::with_start(slot32(key), 7, start, limit);
            cases += 1;
            if s.remaining() != ref_st.remaining() || s.epoch() != ref_st.epoch {
                diffs += 1;
            }
            for _ in 0..4 {
                cases += 1;
                let tv = s.mint_nonce();
                let tr = ref_st.mint_nonce();
                match (tv, tr) {
                    (Ok(t), Ok(seq)) => {
                        if t.seq() != seq || t.epoch() != ref_st.epoch {
                            diffs += 1;
                        }
                        let b = t.burn();
                        if b.seq != seq || b.epoch != 7 {
                            diffs += 1;
                        }
                    }
                    (Err(ev), Err(er)) => {
                        if ev != er {
                            diffs += 1;
                        }
                    }
                    (Ok(t), Err(_)) => {
                        let _ = t.burn();
                        diffs += 1;
                    }
                    (Err(_), Ok(_)) => diffs += 1,
                }
                if s.remaining() != ref_st.remaining() || s.next_seq() != ref_st.next_seq {
                    diffs += 1;
                }
            }
            let _ = s.retire();
        } else {
            let mut s = AeadSealer::<ChaCha20Poly1305>::with_start(slot32(key), 7, start, limit);
            cases += 1;
            if s.remaining() != ref_st.remaining() || s.epoch() != ref_st.epoch {
                diffs += 1;
            }
            for _ in 0..4 {
                cases += 1;
                let tv = s.mint_nonce();
                let tr = ref_st.mint_nonce();
                match (tv, tr) {
                    (Ok(t), Ok(seq)) => {
                        if t.seq() != seq || t.epoch() != ref_st.epoch {
                            diffs += 1;
                        }
                        let _ = t.burn();
                    }
                    (Err(ev), Err(er)) => {
                        if ev != er {
                            diffs += 1;
                        }
                    }
                    (Ok(t), Err(_)) => {
                        let _ = t.burn();
                        diffs += 1;
                    }
                    (Err(_), Ok(_)) => diffs += 1,
                }
                if s.remaining() != ref_st.remaining() || s.next_seq() != ref_st.next_seq {
                    diffs += 1;
                }
            }
            let _ = s.retire();
        }
    }

    // Epoch rotation and u32::MAX saturation.
    let s = AeadSealer::<Aes256Gcm>::with_record_limit(slot32(key), u32::MAX - 1, 50);
    let ref_st = r::aead::SealerState::new(1, r::aead::Alg::Aes256Gcm, u32::MAX - 1, 0, 50);
    let (s, _) = s.rotate(slot32(key)).unwrap();
    let e_ref = ref_st.rotate_epoch().unwrap();
    cases += 1;
    if s.epoch() != e_ref || s.remaining() != 50 {
        diffs += 1;
    }
    let ref_max = r::aead::SealerState::new(2, r::aead::Alg::Aes256Gcm, e_ref, 0, 50);
    cases += 1;
    match (s.rotate(slot32(key)), ref_max.rotate_epoch()) {
        (Err(box_err), Err(er)) => {
            if box_err.error != er {
                diffs += 1;
            }
            let _ = box_err.sealer.retire();
            let _ = box_err.new_key.shred();
        }
        _ => diffs += 1,
    }

    assert_eq!(diffs, 0, "aead_limits differential differences={diffs}");
    println!("flip-gate differential differences=0 cases={cases}");
}

fn compare_seal_alg<A: AeadAlgorithm>(rng: &mut Prng, diffs: &mut usize, cases: &mut usize) {
    let mut key = [0u8; KEY_LEN];
    rng.fill(&mut key);
    let mut sealer = AeadSealer::<A>::with_start(slot32(key), 0x0102_0304, 17, A::RECORD_LIMIT);
    let mut foreign = AeadSealer::<A>::new(slot32(key), 0);
    let mut ref_st = r::aead::SealerState::new(
        777,
        r::aead::Alg::from_code(A::ALG_CODE),
        0x0102_0304,
        17,
        A::RECORD_LIMIT,
    );

    // Foreign ticket and short buffer paths preserve buffer & armed ticket.
    let ft = foreign.mint_nonce().unwrap();
    let mut buf = [0xAAu8; 64];
    let res = sealer.seal_in_place(ft, b"aad", &mut buf);
    *cases += 1;
    match res {
        Err(e) => {
            if e.error != CryptoError::ForeignTicket || buf != [0xAAu8; 64] {
                *diffs += 1;
            }
            if let Some(t) = e.ticket {
                let _ = t.burn();
            } else {
                *diffs += 1;
            }
        }
        Ok(_) => *diffs += 1,
    }

    let t_short = sealer.mint_nonce().unwrap();
    let r_short_seq = ref_st.mint_nonce().unwrap();
    for short_len in [0usize, 1, 27] {
        let mut short_v = vec![0x55u8; short_len];
        let mut short_r = short_v.clone();
        let t = sealer.mint_nonce().unwrap();
        let rseq = ref_st.mint_nonce().unwrap();
        *cases += 1;
        let rv = sealer.seal_in_place(t, b"aad", &mut short_v);
        let rr = r::aead::seal_frame(
            &ref_st,
            &key,
            ref_st.id,
            ref_st.epoch,
            rseq,
            b"aad",
            &mut short_r,
        );
        match (rv, rr) {
            (Err(se), Err(er)) => {
                if se.error != er || short_v != short_r {
                    *diffs += 1;
                }
                if let Some(tk) = se.ticket {
                    let _ = tk.burn();
                } else {
                    *diffs += 1;
                }
            }
            _ => *diffs += 1,
        }
        *cases += 1;
        if FrameHeader::parse(&short_v) != r::aead::frame_header_parse(&short_r) {
            *diffs += 1;
        }
    }
    let _ = (t_short.burn(), r_short_seq);

    // Frame sizes spanning zero-length payload, every stack scratch bucket, and heap fallback.
    let payload_lens = [0usize, 1, 15, 16, 17, 63, 64, 200, 1200, 3900, 18_000];
    let aad_lens = [0usize, 1, 48, 300];
    for &plen in &payload_lens {
        for &alen in &aad_lens {
            let mut aad = vec![0u8; alen];
            rng.fill(&mut aad);
            let mut vbuf = vec![0u8; FRAME_HEADER_LEN + plen];
            rng.fill(&mut vbuf[FRAME_HEADER_LEN..]);
            let mut rbuf = vbuf.clone();

            let t = sealer.mint_nonce().unwrap();
            let seq = ref_st.mint_nonce().unwrap();
            let v_res = sealer.seal_in_place(t, &aad, &mut vbuf);
            let r_res =
                r::aead::seal_frame(&ref_st, &key, ref_st.id, ref_st.epoch, seq, &aad, &mut rbuf);
            *cases += 1;
            match (v_res, r_res) {
                (Ok(sealed), Ok(rlen)) => {
                    if sealed.epoch != 0x0102_0304
                        || sealed.seq != seq
                        || sealed.frame_len != rlen
                        || vbuf != rbuf
                    {
                        *diffs += 1;
                    }
                }
                _ => *diffs += 1,
            }
            *cases += 1;
            if FrameHeader::parse(&vbuf) != r::aead::frame_header_parse(&rbuf) {
                *diffs += 1;
            }
        }
    }

    let _ = sealer.retire();
    let _ = foreign.retire();
}

#[test]
fn aead_sealing_and_frames_match_the_rust_code() {
    let mut diffs = 0usize;
    let mut cases = 0usize;
    let mut rng = Prng(0x7661_756c_745f_7365);

    compare_seal_alg::<Aes256Gcm>(&mut rng, &mut diffs, &mut cases);
    compare_seal_alg::<ChaCha20Poly1305>(&mut rng, &mut diffs, &mut cases);

    assert_eq!(diffs, 0, "aead_seal differential differences={diffs}");
    println!("flip-gate differential differences=0 cases={cases}");
}

fn compare_open_alg<A: AeadAlgorithm>(rng: &mut Prng, diffs: &mut usize, cases: &mut usize) {
    let mut key = [0u8; KEY_LEN];
    rng.fill(&mut key);
    let epoch = 0x1122_3344u32;
    let opener = AeadOpener::<A>::new(slot32(key), epoch);
    let alg = r::aead::Alg::from_code(A::ALG_CODE);

    // Malformed header (< 28 bytes): same error + complete zeroization.
    for len in [0usize, 1, 12, 27] {
        let mut vbuf = vec![0x9Du8; len];
        let mut rbuf = vbuf.clone();
        *cases += 1;
        let verr = opener
            .open_in_place(b"aad", CiphertextFrame::new(&mut vbuf))
            .unwrap_err();
        let rerr = r::aead::open_frame(alg, epoch, &key, b"aad", &mut rbuf).unwrap_err();
        if verr != rerr || rerr != AuthError::MalformedHeader || vbuf != rbuf {
            *diffs += 1;
        }
    }

    // WrongEpoch before checking tag: sealed under epoch ^ 1, opened under epoch.
    for wrong_epoch in [epoch ^ 1, 0, u32::MAX] {
        let st = r::aead::SealerState::new(1, alg, wrong_epoch, 5, A::RECORD_LIMIT);
        let mut vbuf = vec![0x33u8; FRAME_HEADER_LEN + 64];
        r::aead::seal_frame(&st, &key, 1, wrong_epoch, 5, b"aad", &mut vbuf).unwrap();
        let mut rbuf = vbuf.clone();
        *cases += 1;
        let verr = opener
            .open_in_place(b"aad", CiphertextFrame::new(&mut vbuf))
            .unwrap_err();
        let rerr = r::aead::open_frame(alg, epoch, &key, b"aad", &mut rbuf).unwrap_err();
        if verr != rerr
            || rerr
                != (AuthError::WrongEpoch {
                    expected: epoch,
                    found: wrong_epoch,
                })
            || vbuf != rbuf
            || !vbuf.iter().all(|&b| b == 0)
        {
            *diffs += 1;
        }
    }

    // Authentic open + tamper cases across payload lengths and header/tag/AAD mutations.
    for (i, &plen) in [0usize, 1, 16, 64, 200, 1200, 4096, 17_500]
        .iter()
        .enumerate()
    {
        let st = r::aead::SealerState::new(1, alg, epoch, i as u64, A::RECORD_LIMIT);
        let mut aad = vec![0u8; (i * 19) % 128];
        rng.fill(&mut aad);
        let mut pt = vec![0u8; plen];
        rng.fill(&mut pt);

        let mut sealed = vec![0u8; FRAME_HEADER_LEN + plen];
        sealed[FRAME_HEADER_LEN..].copy_from_slice(&pt);
        r::aead::seal_frame(&st, &key, 1, epoch, i as u64, &aad, &mut sealed).unwrap();

        // Valid open matches header and plaintext byte-for-byte.
        let mut vbuf = sealed.clone();
        let mut rbuf = sealed.clone();
        *cases += 1;
        let vpt = opener
            .open_in_place(&aad, CiphertextFrame::new(&mut vbuf))
            .unwrap();
        let rhdr = r::aead::open_frame(alg, epoch, &key, &aad, &mut rbuf).unwrap();
        if vpt.header() != rhdr || vpt.bytes() != pt.as_slice() || vbuf != rbuf {
            *diffs += 1;
        }

        // Tamper seq in header -> AuthenticationTagMismatch + whole frame zeroized.
        let mut vbad = sealed.clone();
        vbad[11] ^= 1;
        let mut rbad = vbad.clone();
        *cases += 1;
        let verr = opener
            .open_in_place(&aad, CiphertextFrame::new(&mut vbad))
            .unwrap_err();
        let rerr = r::aead::open_frame(alg, epoch, &key, &aad, &mut rbad).unwrap_err();
        if verr != rerr || rerr != AuthError::AuthenticationTagMismatch || vbad != rbad {
            *diffs += 1;
        }

        // Tamper tag -> AuthenticationTagMismatch + whole frame zeroized.
        let mut vbad = sealed.clone();
        vbad[12 + (i % 16)] ^= 0x80;
        let mut rbad = vbad.clone();
        *cases += 1;
        let verr = opener
            .open_in_place(&aad, CiphertextFrame::new(&mut vbad))
            .unwrap_err();
        let rerr = r::aead::open_frame(alg, epoch, &key, &aad, &mut rbad).unwrap_err();
        if verr != rerr || rerr != AuthError::AuthenticationTagMismatch || vbad != rbad {
            *diffs += 1;
        }

        // Tamper payload (when non-empty) or AAD -> AuthenticationTagMismatch + whole frame zeroized.
        let mut vbad = sealed.clone();
        if plen > 0 {
            vbad[FRAME_HEADER_LEN + (i % plen)] ^= 1;
        }
        let wrong_aad = if plen == 0 {
            vec![0x01u8]
        } else {
            let mut a = aad.clone();
            if a.is_empty() {
                a.push(1);
            } else {
                a[0] ^= 1;
            }
            a
        };
        let mut rbad = vbad.clone();
        *cases += 1;
        let verr = opener
            .open_in_place(&wrong_aad, CiphertextFrame::new(&mut vbad))
            .unwrap_err();
        let rerr = r::aead::open_frame(alg, epoch, &key, &wrong_aad, &mut rbad).unwrap_err();
        if verr != rerr || rerr != AuthError::AuthenticationTagMismatch || vbad != rbad {
            *diffs += 1;
        }
    }

    let _ = opener.retire();
}

#[test]
fn aead_opening_and_doom_match_the_rust_code() {
    let mut diffs = 0usize;
    let mut cases = 0usize;
    let mut rng = Prng(0x7661_756c_745f_6f70);

    compare_open_alg::<Aes256Gcm>(&mut rng, &mut diffs, &mut cases);
    compare_open_alg::<ChaCha20Poly1305>(&mut rng, &mut diffs, &mut cases);

    assert_eq!(diffs, 0, "aead_open differential differences={diffs}");
    println!("flip-gate differential differences=0 cases={cases}");
}

#[test]
fn kem_client_shares_match_the_rust_code() {
    let mut diffs = 0usize;
    let mut cases = 0usize;
    let mut rng = Prng(0x7661_756c_745f_6b63);

    cases += 1;
    assert_eq!(KemIsa::detect(), r::kem::detect_isa());
    for isa in KemIsa::ALL {
        cases += 1;
        if isa.is_available() != r::kem::is_isa_available(isa) {
            diffs += 1;
        }
    }

    for round in 0..8 {
        let mut ml_seed = [0u8; 64];
        let mut x_sec = [0u8; 32];
        let mut m = [0u8; 32];
        let mut eph = [0u8; 32];
        rng.fill(&mut ml_seed);
        rng.fill(&mut x_sec);
        rng.fill(&mut m);
        rng.fill(&mut eph);

        for isa in KemIsa::ALL {
            if !isa.is_available() {
                let mut ms = ml_seed;
                let mut xs = x_sec;
                cases += 1;
                let rv = HybridClientSecret::from_seeds_for_testing_on(
                    isa,
                    &mut ms,
                    &mut xs,
                    LockPolicy::BestEffort,
                )
                .map(|(s, _)| {
                    let _ = s.abandon();
                });
                let rr = r::kem::require_isa(isa).map(|_| ());
                diff_err(rv, rr, &mut diffs, &mut cases, "unavailable isa");
                continue;
            }

            let mut ms = ml_seed;
            let mut xs = x_sec;
            let (client, v_cshare) = HybridClientSecret::from_seeds_for_testing_on(
                isa,
                &mut ms,
                &mut xs,
                LockPolicy::BestEffort,
            )
            .unwrap();
            let mut full_seed = [0u8; 96];
            full_seed[..64].copy_from_slice(&ml_seed);
            full_seed[64..].copy_from_slice(&x_sec);
            let r_cshare = r::kem::client_share(isa, &full_seed);

            cases += 1;
            if v_cshare != r_cshare {
                diffs += 1;
            }

            // Plain ML-KEM EK + decapsulate helpers agree with reference.
            cases += 1;
            if isa
                .mlkem768_encapsulation_key_for_testing(&ml_seed)
                .unwrap()
                != r::kem::mlkem768_encapsulation_key(isa, &ml_seed).unwrap()
            {
                diffs += 1;
            }

            // Build server share via reference oracle.
            let mut sshare = [0u8; SERVER_SHARE_LEN];
            let mut ref_ss = [0u8; SHARED_SECRET_LEN];
            r::kem::server_encapsulate(isa, &r_cshare, &m, &eph, &mut sshare, &mut ref_ss).unwrap();

            // Wrong server share length leaves client secret intact and matches error.
            let short_srv = vec![
                0u8;
                if round % 2 == 0 {
                    0
                } else {
                    SERVER_SHARE_LEN - 1
                }
            ];
            cases += 1;
            let err_len = client
                .decapsulate(&short_srv, LockPolicy::BestEffort)
                .unwrap_err();
            let mut dummy_ss = [0u8; SHARED_SECRET_LEN];
            let ref_err_len =
                r::kem::client_decapsulate(isa, &full_seed, &short_srv, &mut dummy_ss).unwrap_err();
            if err_len.error != ref_err_len {
                diffs += 1;
            }
            let client = err_len.secret;

            // Low-order (non-contributory) X25519 peer share fails on both.
            let mut bad_sshare = sshare;
            bad_sshare[MLKEM768_CT_LEN..].fill(0);
            if round % 2 == 1 {
                bad_sshare[MLKEM768_CT_LEN] = 1;
            }
            cases += 1;
            let err_nc = client
                .decapsulate(&bad_sshare, LockPolicy::BestEffort)
                .unwrap_err();
            let ref_err_nc =
                r::kem::client_decapsulate(isa, &full_seed, &bad_sshare, &mut dummy_ss)
                    .unwrap_err();
            if err_nc.error != ref_err_nc || ref_err_nc != KemError::NonContributoryX25519 {
                diffs += 1;
            }
            let client = err_nc.secret;

            // Valid server share agrees on all 64 shared-secret bytes.
            cases += 1;
            let v_ss = client.decapsulate(&sshare, LockPolicy::BestEffort).unwrap();
            let v_bytes = v_ss.with(|b| *b);
            let _ = v_ss.shred();
            if v_bytes != ref_ss {
                diffs += 1;
            }
            cases += 1;
            let ct: &[u8; MLKEM768_CT_LEN] = sshare[..MLKEM768_CT_LEN].try_into().unwrap();
            if isa.mlkem768_decapsulate_for_testing(&ml_seed, ct).unwrap()
                != r::kem::mlkem768_decapsulate(isa, &ml_seed, ct).unwrap()
            {
                diffs += 1;
            }
        }
    }

    assert_eq!(diffs, 0, "kem_client differential differences={diffs}");
    println!("flip-gate differential differences=0 cases={cases}");
}

#[test]
fn kem_server_encaps_and_validation_match_the_rust_code() {
    let mut diffs = 0usize;
    let mut cases = 0usize;
    let mut rng = Prng(0x7661_756c_745f_6b73);

    for round in 0..8 {
        let mut ml_seed = [0u8; 64];
        let mut x_sec = [0u8; 32];
        let mut m = [0u8; 32];
        let mut eph = [0u8; 32];
        rng.fill(&mut ml_seed);
        rng.fill(&mut x_sec);
        rng.fill(&mut m);
        rng.fill(&mut eph);

        let mut full_seed = [0u8; 96];
        full_seed[..64].copy_from_slice(&ml_seed);
        full_seed[64..].copy_from_slice(&x_sec);
        let cshare = r::kem::client_share(KemIsa::Portable, &full_seed);

        for isa in KemIsa::ALL {
            if !isa.is_available() {
                cases += 1;
                let rv = server_encapsulate_for_testing_on(
                    isa,
                    &cshare,
                    &m,
                    &eph,
                    LockPolicy::BestEffort,
                )
                .map(|(_, ss)| {
                    let _ = ss.shred();
                });
                let rr = r::kem::require_isa(isa).map(|_| ());
                diff_err(rv, rr, &mut diffs, &mut cases, "server unavailable isa");
                continue;
            }

            // Valid client share matches server share and shared secret bytes.
            let (v_sshare, v_slot) =
                server_encapsulate_for_testing_on(isa, &cshare, &m, &eph, LockPolicy::BestEffort)
                    .unwrap();
            let v_ss = v_slot.with(|b| *b);
            let _ = v_slot.shred();

            let mut r_sshare = [0u8; SERVER_SHARE_LEN];
            let mut r_ss = [0u8; SHARED_SECRET_LEN];
            r::kem::server_encapsulate(isa, &cshare, &m, &eph, &mut r_sshare, &mut r_ss).unwrap();

            cases += 1;
            if v_sshare != r_sshare || v_ss != r_ss {
                diffs += 1;
            }

            // Plain ML-KEM encaps testing helper agrees.
            let ek: &[u8; MLKEM768_EK_LEN] = cshare[..MLKEM768_EK_LEN].try_into().unwrap();
            cases += 1;
            if isa.mlkem768_encapsulate_for_testing(ek, &m).unwrap()
                != r::kem::mlkem768_encapsulate(isa, ek, &m).unwrap()
            {
                diffs += 1;
            }

            // Wrong client share length rejected cleanly on both.
            let bad_len = vec![0u8; CLIENT_SHARE_LEN - 1 - round];
            cases += 1;
            let rv =
                server_encapsulate_for_testing_on(isa, &bad_len, &m, &eph, LockPolicy::BestEffort)
                    .map(|(_, s)| {
                        let _ = s.shred();
                    });
            let rr = r::kem::server_encapsulate(isa, &bad_len, &m, &eph, &mut r_sshare, &mut r_ss);
            diff_err(rv, rr, &mut diffs, &mut cases, "bad client share length");

            // Encapsulation key failing FIPS 203 modulus check (coeff >= 3329).
            let mut bad_ek_share = cshare;
            bad_ek_share[0] = 0xff;
            bad_ek_share[1] = 0x0f;
            cases += 1;
            let rv = server_encapsulate_for_testing_on(
                isa,
                &bad_ek_share,
                &m,
                &eph,
                LockPolicy::BestEffort,
            )
            .map(|(_, s)| {
                let _ = s.shred();
            });
            let rr =
                r::kem::server_encapsulate(isa, &bad_ek_share, &m, &eph, &mut r_sshare, &mut r_ss);
            diff_err(rv, rr, &mut diffs, &mut cases, "invalid mlkem ek");

            let bad_ek: &[u8; MLKEM768_EK_LEN] =
                bad_ek_share[..MLKEM768_EK_LEN].try_into().unwrap();
            cases += 1;
            let rv_plain = isa.mlkem768_encapsulate_for_testing(bad_ek, &m).map(|_| ());
            let rr_plain = r::kem::mlkem768_encapsulate(isa, bad_ek, &m).map(|_| ());
            diff_err(
                rv_plain,
                rr_plain,
                &mut diffs,
                &mut cases,
                "invalid mlkem plain ek",
            );

            // Non-contributory X25519 key inside client share.
            let mut nc_share = cshare;
            nc_share[MLKEM768_EK_LEN..].fill(0);
            if round % 2 == 1 {
                nc_share[MLKEM768_EK_LEN] = 1;
            }
            cases += 1;
            let rv =
                server_encapsulate_for_testing_on(isa, &nc_share, &m, &eph, LockPolicy::BestEffort)
                    .map(|(_, s)| {
                        let _ = s.shred();
                    });
            let rr = r::kem::server_encapsulate(isa, &nc_share, &m, &eph, &mut r_sshare, &mut r_ss);
            diff_err(rv, rr, &mut diffs, &mut cases, "server non-contributory");
        }
    }

    assert_eq!(diffs, 0, "kem_server differential differences={diffs}");
    println!("flip-gate differential differences=0 cases={cases}");
}
