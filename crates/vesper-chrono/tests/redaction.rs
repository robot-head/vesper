//! Secret values, `@pii` fields and secret image regions never reach the
//! serialized `.vchrono` bundle.

use vesper_chrono::{
    Bundle, BundleError, BundleKey, BundleLimits, FieldRule, FlightRecorder, ImageRegion,
    RecorderConfig, RedactionError, RedactionPolicy, RedactionTarget, ReplayError, ReplaySession,
};

const SECRET: &[u8] = b"sk_live_9f8e7d6c5b4a";
const EMAIL: &[u8] = b"alice@example.test";
const SLAB: [u8; 32] = [0xC3; 32];

fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    haystack.windows(needle.len()).any(|w| w == needle)
}

fn key() -> BundleKey {
    BundleKey::new([9; 32])
}

fn recorder() -> FlightRecorder {
    let mut rec = FlightRecorder::new(RecorderConfig::small()).unwrap();
    // A secret embedded in an arbitrary payload position.
    let mut payload = b"GET /v1/charge auth=".to_vec();
    payload.extend_from_slice(SECRET);
    payload.extend_from_slice(b" ok");
    rec.record_effect(1, 1, 7, 1, &payload).unwrap();
    // An @pii field at bytes 4..22 of effect 8 / op 2.
    let mut pii = b"uid:".to_vec();
    pii.extend_from_slice(EMAIL);
    pii.extend_from_slice(b";plan=pro");
    rec.record_effect(1, 2, 8, 2, &pii).unwrap();
    rec.record_effect(1, 3, 8, 3, b"uid:not-pii-op-tag")
        .unwrap();
    // A state image whose bytes 16..48 are a SecretSlab page.
    let mut image = vec![0x11; 16];
    image.extend_from_slice(&SLAB);
    image.extend_from_slice(&[0x22; 16]);
    rec.capture_snapshot(4, &image).unwrap();
    rec.record_effect(2, 5, 7, 1, b"no secret here").unwrap();
    rec
}

fn policy() -> RedactionPolicy {
    let mut p = RedactionPolicy::new();
    p.secret_value(SECRET).unwrap();
    p.pii_field(FieldRule {
        effect_id: 8,
        op_tag: 2,
        offset: 4,
        len: EMAIL.len(),
    })
    .unwrap();
    p.secret_region(ImageRegion {
        offset: 16,
        len: 32,
    })
    .unwrap();
    p
}

#[test]
fn registered_secrets_never_appear_in_the_bundle() {
    let rec = recorder();
    let unredacted = Bundle::capture(&rec, &RedactionPolicy::new(), b"")
        .seal(&key())
        .unwrap();
    assert!(
        contains(&unredacted, SECRET)
            && contains(&unredacted, EMAIL)
            && contains(&unredacted, &SLAB)
    );

    let mut metadata = b"host=a secret=".to_vec();
    metadata.extend_from_slice(SECRET);
    let bytes = Bundle::export(&rec, &policy(), &metadata, &key()).unwrap();
    assert!(!contains(&bytes, SECRET));
    assert!(!contains(&bytes, EMAIL));
    assert!(!contains(&bytes, &SLAB));
    assert!(
        contains(&bytes, b"uid:not-pii-op-tag"),
        "other op tags untouched"
    );
    assert!(contains(&bytes, b"no secret here"));

    let session = ReplaySession::open_bundle(&bytes, &key(), &BundleLimits::default()).unwrap();
    let b = session.bundle();
    assert!(b.is_redacted(1) && b.is_redacted(2) && !b.is_redacted(3));
    assert_eq!(&b.records[1].payload[..4], b"uid:");
    assert_eq!(
        &b.records[1].payload[4..4 + EMAIL.len()],
        &[0; EMAIL.len()][..]
    );
    assert!(
        b.redactions
            .iter()
            .any(|e| e.target == RedactionTarget::Metadata)
    );
    assert!(
        b.redactions
            .iter()
            .any(|e| matches!(e.target, RedactionTarget::Snapshot { epoch_id: 1 }))
    );
    // A redacted record is not silently replayed.
    struct Any;
    impl vesper_chrono::Replayer for Any {
        type State = ();
        fn initial(&self) {}
        fn restore(&self, _: &[u8]) -> Result<(), String> {
            Ok(())
        }
        fn apply(&self, _: &mut (), _: &vesper_chrono::FlightEvent<'_>) -> Result<(), String> {
            Ok(())
        }
    }
    assert_eq!(
        session.state_at_from(&Any, 2, false),
        Err(ReplayError::NotReplayable { lamport_seq: 1 })
    );
    let _ = session.close();
}

#[test]
fn a_secret_spanning_records_fails_serialization() {
    let mut rec = FlightRecorder::new(RecorderConfig::small()).unwrap();
    // The secret is split across a payload end and the next record header:
    // per-payload scrubbing cannot see it, the final scan must.
    rec.record_effect(1, 0, 7, 1, &SECRET[..10]).unwrap();
    let mut p = RedactionPolicy::new();
    let mut spanning = SECRET[..10].to_vec();
    spanning.extend_from_slice(&2u64.to_le_bytes()[..2]);
    p.secret_value(&spanning).unwrap();
    rec.record_effect(1, 0, 7, 1, b"tail").unwrap();
    assert!(matches!(
        Bundle::export(&rec, &p, b"", &key()),
        Err(BundleError::Redaction(
            RedactionError::SecretSurvived { .. }
        ))
    ));
}

#[test]
fn policy_rules_are_validated_and_secrets_not_printed() {
    let mut p = RedactionPolicy::new();
    assert_eq!(
        p.secret_value(b"abc").unwrap_err(),
        RedactionError::SecretTooShort { len: 3 }
    );
    assert!(
        p.pii_field(FieldRule {
            effect_id: 1,
            op_tag: 0,
            offset: 0,
            len: 0
        })
        .is_err()
    );
    assert!(p.secret_region(ImageRegion { offset: 0, len: 0 }).is_err());
    p.secret_value(SECRET).unwrap();
    let printed = format!("{p:?}");
    assert!(!printed.contains("sk_live"), "{printed}");
    assert!(format!("{:?}", key()).contains(".."));
}
