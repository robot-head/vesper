//! The `kafka_wire` ABBA workload (examples/abba.rs): one round of a
//! transactional consume-transform-produce member's wire traffic, every
//! request framed and every response decoded. With the `abba-reference`
//! feature the same round runs on the Rust codecs the port replaced
//! (`kafka_reference.rs`), which is the workload's Rust arm.
#[cfg(not(feature = "abba-reference"))]
use super::codec as k;
use super::codec::Head;
#[cfg(feature = "abba-reference")]
use super::codec::reference as k;
use super::kafka::KafkaBinding;
use super::wire::{KafkaCompression, KafkaRecord, ProducerStamp, encode_batch};
use std::collections::BTreeMap;
use std::sync::OnceLock;

/// A big-endian body writer for the canned responses (setup only).
#[derive(Default)]
struct B(Vec<u8>);
impl B {
    fn i16(&mut self, v: i16) -> &mut Self {
        self.0.extend_from_slice(&v.to_be_bytes());
        self
    }
    fn i32(&mut self, v: i32) -> &mut Self {
        self.0.extend_from_slice(&v.to_be_bytes());
        self
    }
    fn i64(&mut self, v: i64) -> &mut Self {
        self.0.extend_from_slice(&v.to_be_bytes());
        self
    }
    fn u8(&mut self, v: u8) -> &mut Self {
        self.0.push(v);
        self
    }
    fn s(&mut self, s: &str) -> &mut Self {
        self.i16(s.len() as i16);
        self.0.extend_from_slice(s.as_bytes());
        self
    }
    /// A compact (flexible) string.
    fn cs(&mut self, s: &str) -> &mut Self {
        self.u8(s.len() as u8 + 1);
        self.0.extend_from_slice(s.as_bytes());
        self
    }
    fn bytes(&mut self, b: &[u8]) -> &mut Self {
        self.i32(b.len() as i32);
        self.0.extend_from_slice(b);
        self
    }
}

struct Canned {
    batch: Vec<u8>,
    fetch: Vec<u8>,
    produce: Vec<u8>,
    metadata: Vec<u8>,
    coordinator: Vec<u8>,
    offset_fetch: Vec<u8>,
    commit: Vec<u8>,
    code: Vec<u8>,
    init: Vec<u8>,
    add_partitions: Vec<u8>,
    list_offsets: Vec<u8>,
    join: Vec<u8>,
    sync: Vec<u8>,
    bindings: Vec<KafkaBinding>,
    offsets: Vec<(String, i32, i64, String)>,
    framed: Vec<u8>,
}

const TOPIC: &str = "orders-input";
const OUT: &str = "orders-output";
const GROUP: &str = "flux-orders";
const MEMBER: &str = "flux-1-6f9c2a51-0d6b-4a43-9d8e-5d0c6a1e2b77";
const TXN: &str = "flux-orders-txn-1";

fn records(n: usize, seed: u8) -> Vec<KafkaRecord> {
    (0..n)
        .map(|i| {
            let mut headers = BTreeMap::new();
            headers.insert("vflux-epoch".to_string(), vec![seed, i as u8]);
            KafkaRecord {
                key: Some(format!("key-{seed}-{i}").into_bytes()),
                value: Some(vec![seed ^ i as u8; 48 + i % 40]),
                headers,
                timestamp: 1_700_000_000_000 + i as i64,
            }
        })
        .collect()
}

fn canned() -> &'static Canned {
    static C: OnceLock<Canned> = OnceLock::new();
    C.get_or_init(|| {
        let stamp = ProducerStamp {
            id: 4001,
            epoch: 3,
            sequence: 120,
            transactional: true,
        };
        let batch = encode_batch(&records(8, 1), KafkaCompression::None, stamp).expect("batch");
        let mut fetch = B::default();
        fetch.i32(0).i16(0).i32(0).i32(1).s(TOPIC).i32(1);
        fetch.i32(0).i16(0).i64(9_000).i64(8_990).i64(0).i32(1);
        fetch.i64(77).i64(8_100).i32(-1).bytes(&batch);
        let mut produce = B::default();
        produce
            .i32(1)
            .s(OUT)
            .i32(1)
            .i32(0)
            .i16(0)
            .i64(512)
            .i64(-1)
            .i64(0);
        produce.i32(0).i16(-1).i32(0);
        let mut metadata = B::default();
        metadata.i32(0).i32(3);
        for (id, host) in [
            (1, "broker-1.kafka.internal"),
            (2, "broker-2.kafka.internal"),
            (3, "::1"),
        ] {
            metadata.i32(id).s(host).i32(9092).i16(-1);
        }
        metadata.s("cluster-xyz").i32(1).i32(2);
        for topic in [TOPIC, OUT] {
            metadata.i16(0).s(topic).u8(0).i32(3);
            for p in 0..3 {
                metadata.i16(0).i32(p).i32(1 + p).i32(4);
                for n in [3, 3, 0] {
                    metadata.i32(n);
                    for r in 0..n {
                        metadata.i32(1 + r);
                    }
                }
            }
            metadata.i32(0);
        }
        let mut coordinator = B::default();
        coordinator
            .i32(0)
            .i16(0)
            .i16(-1)
            .i32(2)
            .s("broker-2.kafka.internal")
            .i32(9092);
        let mut offset_fetch = B::default();
        offset_fetch.i32(0).u8(2).cs(TOPIC).u8(4);
        for p in 0..3 {
            offset_fetch.i32(p).i64(1_000 + i64::from(p)).i32(-1);
            offset_fetch
                .cs(&format!("vflux/1 l={}", 70 + p))
                .i16(0)
                .u8(0);
        }
        offset_fetch.u8(0).i16(0);
        let mut commit = B::default();
        commit.i32(0).u8(2).cs(TOPIC).u8(4);
        for p in 0..3 {
            commit.i32(p).i16(0).u8(0);
        }
        commit.u8(0).u8(0);
        let mut code = B::default();
        code.i32(0).i16(0);
        let mut init = B::default();
        init.i32(0).i16(0).i64(4001).i16(3);
        let mut add_partitions = B::default();
        add_partitions.i32(0).i32(1).s(OUT).i32(1).i32(0).i16(0);
        let mut list_offsets = B::default();
        list_offsets.i32(0).i32(1).s(TOPIC).i32(1);
        list_offsets.i32(0).i16(0).i64(-1).i64(8_990).i32(0);
        let bindings: Vec<KafkaBinding> = (0..3)
            .map(|p| KafkaBinding {
                topic: TOPIC.to_string(),
                partition: p,
            })
            .collect();
        let sub = k::subscription(&bindings).expect("subscription");
        let mut join = B::default();
        join.i32(0)
            .i16(0)
            .i32(7)
            .s("range")
            .s(MEMBER)
            .s(MEMBER)
            .i32(2);
        for m in [MEMBER, "flux-2-0b1c"] {
            join.s(m).i16(-1).bytes(&sub);
        }
        let mut parts = BTreeMap::new();
        parts.insert(TOPIC.to_string(), vec![0, 2]);
        let assignment = k::assignment(&parts).expect("assignment");
        let mut sync = B::default();
        sync.i32(0).i16(0).bytes(&assignment);
        let offsets = (0..3)
            .map(|p| {
                (
                    TOPIC.to_string(),
                    p,
                    1_000 + i64::from(p),
                    format!("vflux/1 l={p}"),
                )
            })
            .collect();
        // A whole response frame's body (after the length), for the header.
        let mut framed = B::default();
        framed.i32(41).0.extend_from_slice(&code.0);
        Canned {
            batch,
            fetch: fetch.0,
            produce: produce.0,
            metadata: metadata.0,
            coordinator: coordinator.0,
            offset_fetch: offset_fetch.0,
            commit: commit.0,
            code: code.0,
            init: init.0,
            add_partitions: add_partitions.0,
            list_offsets: list_offsets.0,
            join: join.0,
            sync: sync.0,
            bindings,
            offsets,
            framed: framed.0,
        }
    })
}

fn fnv(mut h: u64, bytes: &[u8]) -> u64 {
    for &b in bytes.iter().take(64) {
        h = (h ^ u64::from(b)).wrapping_mul(0x100_0000_01b3);
    }
    (h ^ bytes.len() as u64).wrapping_mul(0x100_0000_01b3)
}

fn n(h: u64, v: i64) -> u64 {
    (h ^ v as u64).wrapping_mul(0x100_0000_01b3)
}

/// `rounds` rounds of a member's traffic; a digest of the results.
pub(crate) fn kafka_wire_rounds(rounds: u64) -> u64 {
    let c = canned();
    let mut h = 0xcbf2_9ce4_8422_2325u64;
    for round in 0..rounds {
        let head = Head {
            version: 3,
            corr: round as i32,
            cid: "vesper-flux",
        };
        let hd = &head;
        let ok = "request";
        // Metadata and coordinators.
        h = fnv(h, &k::metadata_request(hd, TOPIC).expect(ok));
        let m = k::metadata(&c.metadata, TOPIC);
        h = n(h, m.brokers.len() as i64 + m.leaders.len() as i64);
        h = fnv(h, &k::find_coordinator_request(hd, GROUP, 0).expect(ok));
        h = n(
            h,
            i64::from(k::find_coordinator(&c.coordinator).expect("coordinator").0),
        );
        // Group membership.
        let sub = k::subscription(&c.bindings).expect("subscription");
        h = fnv(h, &sub);
        h = fnv(
            h,
            &k::join_group_request(hd, GROUP, 30_000, 60_000, MEMBER, None, "range", &sub)
                .expect(ok),
        );
        let joined = k::join_group(&c.join).expect("join");
        h = n(h, joined.members.len() as i64);
        let mut parts = BTreeMap::new();
        parts.insert(TOPIC.to_string(), vec![0, 2]);
        let asg = k::assignment(&parts).expect("assignment");
        let assignments = vec![
            (MEMBER.to_string(), asg.clone()),
            ("flux-2-0b1c".to_string(), asg),
        ];
        h = fnv(
            h,
            &k::sync_group_request(hd, GROUP, 7, MEMBER, None, &assignments).expect(ok),
        );
        let mine = k::sync_group(&c.sync).expect("sync");
        h = n(h, k::parse_assignment(&mine).expect("parse").len() as i64);
        h = fnv(
            h,
            &k::heartbeat_request(hd, GROUP, 7, MEMBER, None).expect(ok),
        );
        k::code(&c.code, "heartbeat").expect("heartbeat");
        // Positions.
        h = fnv(
            h,
            &k::offset_fetch_request(hd, GROUP, TOPIC, &[0, 1, 2]).expect(ok),
        );
        h = n(
            h,
            k::offset_fetch(&c.offset_fetch)
                .expect("offset fetch")
                .len() as i64,
        );
        h = fnv(
            h,
            &k::list_offsets_request(hd, TOPIC, 0, -1, true).expect(ok),
        );
        h = n(
            h,
            k::list_offsets(&c.list_offsets, 0).expect("list offsets"),
        );
        // Consume.
        h = fnv(
            h,
            &k::fetch_request(hd, TOPIC, 0, 8_100, 1 << 20, true).expect(ok),
        );
        let f = k::fetch(&c.fetch, 0, 1 << 20, true).expect("fetch");
        h = n(h, f.records.len() as i64 + f.high_watermark);
        // Produce transactionally.
        h = fnv(
            h,
            &k::init_producer_request(hd, Some(TXN), 60_000).expect(ok),
        );
        h = n(h, k::init_producer(&c.init).expect("init").0);
        h = fnv(
            h,
            &k::add_partitions_request(hd, TXN, (4001, 3), OUT, 0).expect(ok),
        );
        k::add_partitions(&c.add_partitions).expect("add partitions");
        h = fnv(
            h,
            &k::produce_request(hd, Some(TXN), OUT, 0, &c.batch).expect(ok),
        );
        h = n(h, k::produce(&c.produce, OUT, 0).expect("produce"));
        h = fnv(
            h,
            &k::add_offsets_request(hd, TXN, (4001, 3), GROUP).expect(ok),
        );
        k::code(&c.code, "add offsets to txn").expect("add offsets");
        h = fnv(
            h,
            &k::txn_offset_commit_request(hd, TXN, GROUP, (4001, 3), 7, MEMBER, None, &c.offsets)
                .expect(ok),
        );
        k::offset_errors(&c.commit, true, "txn offset commit").expect("commit");
        h = fnv(h, &k::end_txn_request(hd, TXN, (4001, 3), true).expect(ok));
        k::code(&c.code, "end txn").expect("end txn");
        // Every response starts with its header.
        for _ in 0..14 {
            h = fnv(
                h,
                &k::response_body(c.framed.clone(), 41, false).expect("header"),
            );
        }
    }
    h
}
