//! Interop with a real Kafka broker (Apache Kafka in KRaft mode).
//!
//! The broker comes from the environment:
//! * `VESPER_KAFKA_HOME`: an Apache Kafka distribution directory. Each test
//!   starts its own single-node KRaft broker on free local ports with three
//!   client listeners (PLAINTEXT, SSL, and SASL_SSL with PLAIN,
//!   SCRAM-SHA-256 and SCRAM-SHA-512), and the broker-crash tests SIGKILL
//!   and restart it.
//! * `VESPER_KAFKA_BOOTSTRAP`: an already running PLAINTEXT broker
//!   (`host:port`); the tests that need TLS, SASL or a broker kill skip.
//!
//! Without either, every test prints a note and passes, unless
//! `VESPER_KAFKA_REQUIRE=1` (set in CI), which makes a missing broker a failure.
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::rc::Rc;
use std::time::{Duration, Instant};
use vesper_flux::*;
use vesper_loom_cas::Store;
use vesper_quarry::device::FsDevice;
use vesper_quarry::durable::DbLimits;

// ----------------------------------------------------------------------
// Broker harness
// ----------------------------------------------------------------------

const USER: &str = "alice";
const PASSWORD: &str = "alice-secret";

/// Which client listener a connection uses.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Listener {
    Plain,
    /// TLS, no authentication.
    Tls,
    /// TLS plus SASL with this mechanism.
    SaslTls(SaslMechanism),
}

struct Broker {
    home: Option<PathBuf>,
    dir: tempfile::TempDir,
    port: u16,
    tls_port: u16,
    sasl_port: u16,
    ca_pem: Vec<u8>,
    external: Option<String>,
    child: Option<Child>,
}
fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}
/// A CA and a broker certificate for 127.0.0.1 / localhost (rcgen).
fn certificates(dir: &Path) -> (Vec<u8>, PathBuf) {
    use rcgen::{BasicConstraints, CertificateParams, DnType, IsCa, Issuer, KeyPair};
    let mut ca = CertificateParams::new(Vec::<String>::new()).unwrap();
    ca.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
    ca.distinguished_name
        .push(DnType::CommonName, "Flux test CA");
    let ca_key = KeyPair::generate().unwrap();
    let ca_cert = ca.self_signed(&ca_key).unwrap();
    let issuer = Issuer::new(ca, ca_key);
    let mut leaf =
        CertificateParams::new(vec!["127.0.0.1".to_string(), "localhost".to_string()]).unwrap();
    leaf.distinguished_name.push(DnType::CommonName, "broker");
    let key = KeyPair::generate().unwrap();
    let cert = leaf.signed_by(&key, &issuer).unwrap();
    let pem = dir.join("broker.pem");
    std::fs::write(&pem, format!("{}{}", key.serialize_pem(), cert.pem())).unwrap();
    (ca_cert.pem().into_bytes(), pem)
}
impl Broker {
    /// A broker for one test, or `None` (test skipped) without one.
    fn start(test: &str) -> Option<Self> {
        let require = std::env::var("VESPER_KAFKA_REQUIRE").is_ok_and(|v| v == "1");
        let dir = tempfile::tempdir().unwrap();
        if let Ok(home) = std::env::var("VESPER_KAFKA_HOME") {
            let (port, tls_port, sasl_port, controller) =
                (free_port(), free_port(), free_port(), free_port());
            let (ca_pem, keystore) = certificates(dir.path());
            let props = dir.path().join("server.properties");
            let logs = dir.path().join("logs");
            let jaas = |m: &str| {
                format!(
                    "listener.name.sasl_ssl.{m}.sasl.jaas.config=org.apache.kafka.common.security.scram.ScramLoginModule required;\n"
                )
            };
            std::fs::write(
                &props,
                format!(
                    "process.roles=broker,controller\n\
                     node.id=1\n\
                     controller.quorum.voters=1@127.0.0.1:{controller}\n\
                     listeners=PLAINTEXT://127.0.0.1:{port},SSL://127.0.0.1:{tls_port},SASL_SSL://127.0.0.1:{sasl_port},CONTROLLER://127.0.0.1:{controller}\n\
                     advertised.listeners=PLAINTEXT://127.0.0.1:{port},SSL://127.0.0.1:{tls_port},SASL_SSL://127.0.0.1:{sasl_port}\n\
                     controller.listener.names=CONTROLLER\n\
                     inter.broker.listener.name=PLAINTEXT\n\
                     listener.security.protocol.map=CONTROLLER:PLAINTEXT,PLAINTEXT:PLAINTEXT,SSL:SSL,SASL_SSL:SASL_SSL\n\
                     ssl.keystore.type=PEM\n\
                     ssl.keystore.location={}\n\
                     ssl.client.auth=none\n\
                     sasl.enabled.mechanisms=PLAIN,SCRAM-SHA-256,SCRAM-SHA-512\n\
                     listener.name.sasl_ssl.plain.sasl.jaas.config=org.apache.kafka.common.security.plain.PlainLoginModule required username=\"admin\" password=\"admin-secret\" user_{USER}=\"{PASSWORD}\";\n\
                     {}{}\
                     log.dirs={}\n\
                     num.partitions=1\n\
                     auto.create.topics.enable=false\n\
                     offsets.topic.replication.factor=1\n\
                     offsets.topic.num.partitions=3\n\
                     transaction.state.log.replication.factor=1\n\
                     transaction.state.log.min.isr=1\n\
                     transaction.state.log.num.partitions=3\n\
                     share.coordinator.state.topic.replication.factor=1\n\
                     share.coordinator.state.topic.min.isr=1\n\
                     group.initial.rebalance.delay.ms=0\n",
                    keystore.display(),
                    jaas("scram-sha-256"),
                    jaas("scram-sha-512"),
                    logs.display()
                ),
            )
            .unwrap();
            let out = Command::new(Path::new(&home).join("bin/kafka-storage.sh"))
                .args(["format", "-t", "q1Sh-9_ISia_zwGINzRvyQ", "-c"])
                .arg(&props)
                .args([
                    "--add-scram",
                    &format!("SCRAM-SHA-256=[name={USER},password={PASSWORD}]"),
                    "--add-scram",
                    &format!("SCRAM-SHA-512=[name={USER},password={PASSWORD}]"),
                ])
                .env("KAFKA_HEAP_OPTS", "-Xmx256m")
                .output()
                .expect("run kafka-storage.sh");
            assert!(
                out.status.success(),
                "format: {}",
                String::from_utf8_lossy(&out.stderr)
            );
            let mut broker = Self {
                home: Some(PathBuf::from(home)),
                dir,
                port,
                tls_port,
                sasl_port,
                ca_pem,
                external: None,
                child: None,
            };
            broker.restart();
            eprintln!(
                "{test}: Kafka broker on 127.0.0.1:{port} (TLS {tls_port}, SASL_SSL {sasl_port})"
            );
            return Some(broker);
        }
        if let Ok(bootstrap) = std::env::var("VESPER_KAFKA_BOOTSTRAP") {
            return Some(Self {
                home: None,
                dir,
                port: 0,
                tls_port: 0,
                sasl_port: 0,
                ca_pem: Vec::new(),
                external: Some(bootstrap),
                child: None,
            });
        }
        assert!(
            !require,
            "{test}: VESPER_KAFKA_REQUIRE=1 but neither VESPER_KAFKA_HOME nor VESPER_KAFKA_BOOTSTRAP is set"
        );
        eprintln!(
            "{test}: skipped (no Kafka broker; set VESPER_KAFKA_HOME or VESPER_KAFKA_BOOTSTRAP)"
        );
        None
    }
    fn managed(&self) -> bool {
        self.home.is_some()
    }
    /// SIGKILL the broker process (no clean shutdown).
    fn kill(&mut self) {
        if let Some(mut c) = self.child.take() {
            c.kill().unwrap();
            c.wait().unwrap();
        }
    }
    /// Start (again) on the same ports and log directory, and wait until it
    /// serves metadata.
    fn restart(&mut self) {
        let home = self.home.clone().expect("managed broker");
        let log = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(self.dir.path().join("broker.log"))
            .unwrap();
        let child = Command::new(home.join("bin/kafka-server-start.sh"))
            .arg(self.dir.path().join("server.properties"))
            .env("KAFKA_HEAP_OPTS", "-Xmx512m -Xms256m")
            .env("LOG_DIR", self.dir.path().join("app-logs"))
            .stdout(Stdio::from(log.try_clone().unwrap()))
            .stderr(Stdio::from(log))
            .spawn()
            .expect("start kafka-server-start.sh");
        self.child = Some(child);
        let deadline = Instant::now() + Duration::from_secs(90);
        loop {
            if let Ok(c) = KafkaConnection::connect(&self.config()) {
                // The broker answers metadata once it registered with the
                // controller; a probe topic proves it can also create.
                let probe = topic("probe");
                if c.create_topic(&probe, 1).is_ok() {
                    return;
                }
            }
            if Instant::now() > deadline {
                let log =
                    std::fs::read_to_string(self.dir.path().join("broker.log")).unwrap_or_default();
                panic!(
                    "broker did not start:\n{}",
                    &log[log.len().saturating_sub(4000)..]
                );
            }
            std::thread::sleep(Duration::from_millis(250));
        }
    }
    fn config(&self) -> KafkaConfig {
        self.config_for(Listener::Plain)
    }
    fn secured(&self) -> bool {
        self.external.is_none()
    }
    fn config_for(&self, l: Listener) -> KafkaConfig {
        let (port, security) = match l {
            Listener::Plain => (self.port, KafkaSecurity::default()),
            Listener::Tls => (
                self.tls_port,
                KafkaSecurity {
                    tls: Some(KafkaTls {
                        ca_pem: self.ca_pem.clone(),
                        server_name: None,
                    }),
                    sasl: None,
                },
            ),
            Listener::SaslTls(mechanism) => (
                self.sasl_port,
                KafkaSecurity {
                    tls: Some(KafkaTls {
                        ca_pem: self.ca_pem.clone(),
                        server_name: None,
                    }),
                    sasl: Some(KafkaSasl {
                        mechanism,
                        username: USER.into(),
                        password: PASSWORD.into(),
                    }),
                },
            ),
        };
        let bootstrap = match (&self.external, l) {
            (Some(b), Listener::Plain) => b.clone(),
            (Some(_), _) => panic!("an external broker offers PLAINTEXT only"),
            (None, _) => format!("127.0.0.1:{port}"),
        };
        KafkaConfig {
            retry_for: Duration::from_secs(10),
            request_timeout: Duration::from_secs(10),
            security,
            ..KafkaConfig::new(bootstrap)
        }
    }
    fn connect(&self) -> KafkaConnection {
        KafkaConnection::connect(&self.config()).unwrap()
    }
    fn connect_with(&self, l: Listener) -> KafkaConnection {
        KafkaConnection::connect(&self.config_for(l)).unwrap()
    }
}
impl Drop for Broker {
    fn drop(&mut self) {
        self.kill();
    }
}

/// Unique names, so tests can share an external broker.
fn topic(name: &str) -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    static N: AtomicU64 = AtomicU64::new(0);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    format!("flux-{name}-{nanos}-{}", N.fetch_add(1, Ordering::Relaxed))
}

// ----------------------------------------------------------------------
// The job: orders ⋈ customers ⋈ regions (the second join re-shuffles)
// ----------------------------------------------------------------------

// ----------------------------------------------------------------------

fn l() -> Limits {
    Limits::default()
}
fn circuit() -> Circuit {
    let mut b = CircuitBuilder::new(l()).unwrap();
    let orders = b.input(2).unwrap(); // (cust, amount)
    let customers = b.input(2).unwrap(); // (cust, region)
    let regions = b.input(2).unwrap(); // (region, tax)
    let j = b.equijoin(orders, customers, 0, 0).unwrap();
    let r = b.equijoin(j, regions, 3, 0).unwrap();
    let out = b
        .map(
            r,
            vec![Field::Column(0), Field::Column(1), Field::Column(5)],
        )
        .unwrap();
    b.finish(out).unwrap()
}
/// Per epoch, per input: the (row, weight) changes.
/// One workload step: the (row, weight) changes of each input.
type Step = [Vec<(Vec<i64>, i64)>; 3];
fn changes(epochs: usize) -> Vec<Step> {
    (0..epochs as i64)
        .map(|e| {
            let orders: Vec<_> = (0..6)
                .map(|i| (vec![(e * 5 + i) % 11, e * 10 + i], 1))
                .chain((e >= 2).then(|| (vec![((e - 2) * 5) % 11, (e - 2) * 10], -1)))
                .collect();
            let customers: Vec<_> = match e {
                0 | 1 => (0..11).map(|c| (vec![c, (c + e) % 3], 1)).collect(),
                4 => vec![(vec![1, 2], -1), (vec![1, 0], 1)],
                7 => vec![(vec![6, 0], -1), (vec![6, 2], 1)],
                _ => Vec::new(),
            };
            let regions: Vec<_> = match e {
                0 => (0..3).map(|r| (vec![r, 10 * r], 1)).collect(),
                5 => vec![(vec![2, 20], -1), (vec![2, 25], 1)],
                _ => Vec::new(),
            };
            [orders, customers, regions]
        })
        .collect()
}
fn reference(changes: &[Step]) -> Vec<ZSet> {
    let mut e = circuit().executor();
    changes
        .iter()
        .map(|c| {
            let deltas = c
                .iter()
                .map(|rows| ZSet::consolidate(2, rows, l()).unwrap())
                .collect();
            e.tick(&TickInput { deltas }).unwrap()
        })
        .collect()
}

/// Four Kafka partitions: orders has two (split by customer parity),
/// customers and regions one each.
struct Topics {
    orders: String,
    customers: String,
    regions: String,
    output: String,
}
fn topics(conn: &KafkaConnection, name: &str, output_partitions: i32) -> Topics {
    let t = Topics {
        orders: topic(&format!("{name}-orders")),
        customers: topic(&format!("{name}-customers")),
        regions: topic(&format!("{name}-regions")),
        output: topic(&format!("{name}-out")),
    };
    conn.create_topic(&t.orders, 2).unwrap();
    conn.create_topic(&t.customers, 1).unwrap();
    conn.create_topic(&t.regions, 1).unwrap();
    conn.create_topic(&t.output, output_partitions).unwrap();
    t
}
fn policy() -> CdcPolicy {
    let b = |partition_id, input| SourceBinding {
        partition_id,
        schema_id: 7,
        input,
        first_lsn: 100,
        weights: WeightPolicy::NonZero,
    };
    CdcPolicy {
        bindings: vec![b(0, 0), b(1, 0), b(2, 1), b(3, 2)],
        max_batch_bytes: 1 << 20,
        max_records: 4096,
    }
}
fn bindings(t: &Topics) -> Vec<KafkaBinding> {
    let b = |topic: &str, partition| KafkaBinding {
        topic: topic.to_string(),
        partition,
    };
    vec![
        b(&t.orders, 0),
        b(&t.orders, 1),
        b(&t.customers, 0),
        b(&t.regions, 0),
    ]
}

/// Produces the CDC records of the workload, tracking LSNs per binding.
struct Producer {
    next: [u64; 4],
}
impl Producer {
    fn new() -> Self {
        Self { next: [100; 4] }
    }
    /// Produce one epoch's changes. `duplicate` re-sends the first record of
    /// each batch (an upstream retry the source must drop).
    fn epoch(
        &mut self,
        conn: &KafkaConnection,
        t: &Topics,
        changes: &Step,
        duplicate: bool,
    ) -> u64 {
        let mut batches: BTreeMap<usize, Vec<Vec<u8>>> = BTreeMap::new();
        for (input, rows) in changes.iter().enumerate() {
            for (row, weight) in rows {
                let binding = match input {
                    0 => (row[0].rem_euclid(2)) as usize,
                    1 => 2,
                    _ => 3,
                };
                let header = CdcHeader {
                    source_lsn: self.next[binding],
                    event_ts_ms: 0,
                    z_weight: *weight,
                    partition_id: binding as u32,
                    schema_id: 7,
                    flags: 0,
                };
                self.next[binding] += 1;
                batches
                    .entry(binding)
                    .or_default()
                    .push(cdc_value(header, row));
            }
        }
        let mut dups = 0;
        for (binding, values) in batches {
            let (topic, partition) = match binding {
                0 => (&t.orders, 0),
                1 => (&t.orders, 1),
                2 => (&t.customers, 0),
                _ => (&t.regions, 0),
            };
            let mut records: Vec<_> = values.iter().cloned().map(kafka_record).collect();
            if duplicate {
                records.insert(1, kafka_record(values[0].clone()));
                dups += 1;
            }
            conn.produce(topic, partition, records).unwrap();
        }
        dups
    }
}

struct Job {
    _dir: tempfile::TempDir,
    root: PathBuf,
    txn: String,
    listener: Listener,
}
impl Job {
    fn new() -> Self {
        Self::on(Listener::Plain)
    }
    fn on(listener: Listener) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().to_path_buf();
        std::fs::create_dir_all(root.join("sink")).unwrap();
        Self {
            _dir: dir,
            root,
            txn: topic("txn"),
            listener,
        }
    }
    fn conn(&self, broker: &Broker) -> KafkaConnection {
        broker.connect_with(self.listener)
    }
    fn source(&self, broker: &Broker, t: &Topics, cache_epochs: usize) -> KafkaSource {
        KafkaSource::open(
            self.conn(broker),
            &circuit(),
            policy(),
            bindings(t),
            self.root.join("epochs.idx"),
            KafkaSourceLimits {
                cache_epochs,
                ..KafkaSourceLimits::default()
            },
        )
        .unwrap()
        .0
    }
    fn sink(
        &self,
        broker: &Broker,
        t: &Topics,
        partitions: i32,
    ) -> KafkaSink<QuarrySink<FsDevice>> {
        let (quarry, _) = QuarrySink::open(
            FsDevice::open(&self.root.join("sink")).unwrap(),
            DbLimits::default(),
            l(),
        )
        .unwrap();
        KafkaSink::new(
            quarry,
            self.conn(broker),
            &circuit(),
            KafkaSinkConfig {
                topic: t.output.clone(),
                partitions,
                key_column: 0,
                max_batch_records: 7,
                transactional_id: self.txn.clone(),
            },
        )
        .unwrap()
    }
    /// Open everything as a new process would.
    fn open(
        &self,
        broker: &Broker,
        t: &Topics,
        output_partitions: i32,
    ) -> (KafkaSource, KafkaSink<QuarrySink<FsDevice>>, Store) {
        let source = self.source(broker, t, 2);
        let sink = self.sink(broker, t, output_partitions);
        let store = Store::open(self.root.join("cas")).unwrap();
        (source, sink, store)
    }
}
fn config(workers: usize, rescales: &[(u64, usize)]) -> ClusterConfig {
    ClusterConfig {
        seed: 5,
        live_rescales: rescales
            .iter()
            .map(|&(after_epoch, workers)| LiveRescale {
                after_epoch,
                workers,
            })
            .collect(),
        ..ClusterConfig::new(workers, vec![0, 0, 0])
    }
}
fn sum(v: &[ZSet], width: usize) -> ZSet {
    let mut t = ZSet::empty(width, l()).unwrap();
    for z in v {
        t = t.plus(z, l()).unwrap();
    }
    t
}
/// The single-executor outputs of the epochs the source cut, checked to
/// integrate to the workload's reference (an epoch may group several
/// workload steps when records were produced ahead of the poll).
fn from_source(source: &dyn EpochSource, changes: &[Step]) -> Vec<ZSet> {
    let mut e = circuit().executor();
    let out: Vec<ZSet> = (0..source.epochs())
        .map(|i| e.tick(&source.read(i).unwrap()).unwrap())
        .collect();
    assert_eq!(
        sum(&out, 3),
        sum(&reference(changes), 3),
        "source epochs integrate to the workload"
    );
    out
}
/// The sink and the output topic both hold every epoch exactly once.
fn verify(
    conn: &KafkaConnection,
    t: &Topics,
    partitions: i32,
    sink: &mut dyn CommitSink,
    expected: &[ZSet],
    what: &str,
) {
    let outs = sink.outputs().unwrap();
    assert_eq!(
        outs.iter().map(|(e, _)| *e).collect::<Vec<_>>(),
        (0..expected.len() as u64).collect::<Vec<_>>(),
        "{what}: sink epochs"
    );
    for (e, z) in &outs {
        assert_eq!(z, &expected[*e as usize], "{what}: sink epoch {e}");
    }
    let topic = read_kafka_output(conn, &circuit(), &t.output, partitions)
        .unwrap_or_else(|e| panic!("{what}: output topic: {e} {:?}", conn.last_error()));
    let want: BTreeMap<u64, ZSet> = expected
        .iter()
        .enumerate()
        .filter(|(_, z)| !z.is_empty())
        .map(|(e, z)| (e as u64, z.clone()))
        .collect();
    assert_eq!(topic, want, "{what}: output topic");
}

/// Read a topic back through rskafka (an independent client): every record
/// value, ignoring transaction state (rskafka does not filter aborts).
fn rskafka_values(bootstrap: &str, topic: &str, partition: i32) -> Vec<Vec<u8>> {
    use rskafka::client::ClientBuilder;
    use rskafka::client::partition::{OffsetAt, UnknownTopicHandling};
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(async {
        let client = ClientBuilder::new(vec![bootstrap.to_string()])
            .build()
            .await
            .unwrap();
        let p = client
            .partition_client(topic, partition, UnknownTopicHandling::Error)
            .await
            .unwrap();
        let end = p.get_offset(OffsetAt::Latest).await.unwrap();
        let mut at = 0;
        let mut out = Vec::new();
        while at < end {
            let (records, _) = p.fetch_records(at, 1..1_000_000, 500).await.unwrap();
            if records.is_empty() {
                break;
            }
            for r in records {
                at = r.offset + 1;
                if let Some(v) = r.record.value {
                    out.push(v);
                }
            }
        }
        out
    })
}

// ----------------------------------------------------------------------
// Tests
// ----------------------------------------------------------------------

/// The whole job over TLS + SASL/SCRAM-SHA-512 with zstd batches: Kafka
/// source, three workers rescaled live twice, transactional Kafka sink.
#[test]
fn kafka_source_and_sink_round_trip_over_tls_and_scram() {
    let Some(broker) = Broker::start("round_trip") else {
        return;
    };
    let listener = if broker.secured() {
        Listener::SaslTls(SaslMechanism::ScramSha512)
    } else {
        Listener::Plain
    };
    let conn = {
        let mut cfg = broker.config_for(listener);
        cfg.compression = KafkaCompression::Zstd;
        KafkaConnection::connect(&cfg).unwrap()
    };
    let t = topics(&conn, "rt", 3);
    let changes = changes(10);
    let expected = reference(&changes);
    let job = Job::on(listener);
    let (mut source, mut sink, store) = job.open(&broker, &t, 3);
    let mut producer = Producer::new();
    let mut dups = 0;
    for (e, c) in changes.iter().enumerate() {
        dups += producer.epoch(&conn, &t, c, e % 3 == 1);
        assert_eq!(source.poll().unwrap(), Some(e as u64));
    }
    assert_eq!(source.poll().unwrap(), None, "nothing new");
    assert_eq!(source.stats().duplicates, dups);
    // 3 workers, scaled to 2 after epoch 3 and to 4 after epoch 7, live.
    let run = run_cluster_with(
        &circuit(),
        &source,
        &mut sink,
        &store,
        &config(3, &[(3, 2), (7, 4)]),
        &[],
    )
    .unwrap();
    assert_eq!(run.outputs, expected);
    assert_eq!(run.migrations.len(), 2);
    assert!(sink.pending().is_none(), "{:?}", sink.pending());
    let stats = sink.stats();
    assert!(stats.published > 0);
    assert_eq!(stats.inits, 1);
    // One transaction per committed epoch.
    assert_eq!(stats.transactions, expected.len() as u64);
    verify(&conn, &t, 3, &mut sink, &expected, "round trip");
    // Offsets are tied to the commit: the last commit covers every record.
    let record = sink.last_commit().unwrap().unwrap();
    let positions = source.committed(Some(&record)).unwrap();
    for p in &positions {
        let (_, latest) = conn.offsets(&p.topic, p.partition).unwrap();
        assert_eq!(p.offset, latest);
    }
}

#[test]
fn flux_crash_and_restart_against_a_real_broker() {
    let Some(broker) = Broker::start("flux_crash") else {
        return;
    };
    let conn = broker.connect();
    let t = topics(&conn, "crash", 2);
    let changes = changes(12);
    let expected = reference(&changes);
    let job = Job::new();
    let mut producer = Producer::new();
    let schedule = [(3, 4), (7, 2), (9, 3)];
    let mut produced = 0;
    let mut interrupted = 0;
    let mut attempt = 0u64;
    let mut faults_seen = 0;
    loop {
        attempt += 1;
        // New records arrive between incarnations: up to three workload
        // steps, each cut into its own epoch.
        let (mut source, mut sink, store) = job.open(&broker, &t, 2);
        for _ in 0..3 {
            if produced == changes.len() {
                break;
            }
            producer.epoch(&conn, &t, &changes[produced], produced % 4 == 0);
            assert_eq!(source.poll().unwrap(), Some(produced as u64));
            produced += 1;
        }
        let epochs = source.epochs();
        let committed = sink.last_commit().unwrap().map(|r| r.after_epoch);
        if produced == changes.len() && committed == Some(epochs - 1) {
            sink.flush().unwrap();
            verify(&conn, &t, 2, &mut sink, &expected, "final");
            break;
        }
        assert!(attempt < 60, "no progress");
        if committed.is_some_and(|c| c + 1 >= epochs) {
            continue; // everything polled is committed; produce more
        }
        let mut cfg = config(3, &schedule);
        // Two of three incarnations die after a bounded number of steps.
        if !attempt.is_multiple_of(3) {
            cfg.max_steps = 60 + 23 * attempt;
        }
        // Every fourth incarnation also dies inside a Kafka transaction.
        if attempt.is_multiple_of(4) {
            sink.inject_publish_fault(if attempt.is_multiple_of(8) {
                PublishFault::AfterOffsets
            } else {
                PublishFault::AfterRecords
            });
        }
        let faults = match attempt % 3 {
            0 => vec![Fault::worker(25, (attempt % 2) as usize)],
            1 => vec![Fault::coordinator(31)],
            _ => vec![Fault::cluster(45), Fault::worker(70, 0)],
        };
        match run_cluster_with(&circuit(), &source, &mut sink, &store, &cfg, &faults) {
            Ok(run) => {
                faults_seen += run.recoveries.len();
                let start = run.start_epoch as usize;
                assert_eq!(
                    run.outputs,
                    expected[start..epochs as usize],
                    "attempt {attempt}"
                );
            }
            Err(Error::Limit("simulation steps")) => interrupted += 1,
            Err(e) => panic!(
                "attempt {attempt}: {e} {:?}",
                source.connection().last_error()
            ),
        }
        // The process dies here: nothing is closed, flushed or aborted.
        drop((source, sink, store));
    }
    assert!(interrupted >= 2, "{interrupted}");
    assert!(faults_seen > 0);
}

/// SIGKILL the broker while the job reads and publishes (also in the
/// middle of an output transaction); restart it and resume. Nothing is lost
/// or duplicated in the sink or the output topic.
#[test]
fn broker_crash_and_restart() {
    let Some(mut broker) = Broker::start("broker_crash") else {
        return;
    };
    if !broker.managed() {
        eprintln!("broker_crash: skipped (external broker cannot be killed)");
        return;
    }
    let t = topics(&broker.connect(), "bk", 2);
    let changes = changes(10);
    let job = Job::new();
    let mut producer = Producer::new();
    let (mut source, sink, store) = job.open(&broker, &t, 2);
    {
        let conn = broker.connect();
        for c in &changes[..3] {
            producer.epoch(&conn, &t, c, false);
            source.poll().unwrap();
        }
    }
    // 1. The broker dies before a poll: a typed error, and no epoch is cut.
    broker.kill();
    let before = source.epochs();
    assert!(matches!(source.poll().unwrap_err(), Error::Kafka(_)));
    assert_eq!(source.epochs(), before);
    // 2. Reads of epochs that are not cached fail while it is down: the run
    //    stops with a typed error and commits nothing.
    drop((source, sink, store));
    broker.restart();
    let (mut source, mut sink, store) = job.open(&broker, &t, 2);
    {
        let conn = broker.connect();
        for c in &changes[3..6] {
            producer.epoch(&conn, &t, c, false);
            source.poll().unwrap();
        }
    }
    // A new incarnation's source has an empty cache.
    drop(source);
    let source = job.source(&broker, &t, 16);
    broker.kill();
    let r = run_cluster_with(
        &circuit(),
        &source,
        &mut sink,
        &store,
        &config(2, &[(3, 3)]),
        &[],
    );
    assert!(matches!(r, Err(Error::Kafka(_))), "{r:?}");
    assert!(sink.last_commit().unwrap().is_none());
    // 3. Back up: the job runs (rescaling live) and publishes, but the
    //    process dies inside the last epoch's transaction and the broker is
    //    killed with that transaction open.
    broker.restart();
    sink.inject_publish_fault(PublishFault::AfterOffsets);
    let run = run_cluster_with(
        &circuit(),
        &source,
        &mut sink,
        &store,
        &config(2, &[(3, 3)]),
        &[],
    )
    .unwrap_or_else(|e| panic!("{e}: {:?}", source.connection().last_error()));
    assert_eq!(run.migrations.len(), 1);
    drop((source, sink, store));
    broker.kill();
    broker.restart();
    // 4. The broker dies between commit and publication: the commits land
    //    in Quarry, the publication stays pending.
    let (mut source, mut sink, store) = job.open(&broker, &t, 2);
    {
        let conn = broker.connect();
        for c in &changes[6..9] {
            producer.epoch(&conn, &t, c, true);
            source.poll().unwrap();
        }
    }
    let warm = job.source(&broker, &t, 64);
    for e in 0..warm.epochs() {
        warm.read(e).unwrap();
    }
    drop(source);
    broker.kill();
    let run = run_cluster_with(&circuit(), &warm, &mut sink, &store, &config(3, &[]), &[]).unwrap();
    assert_eq!(run.outputs.len() as u64, warm.epochs() - run.start_epoch);
    assert!(sink.pending().is_some());
    assert!(sink.stats().failures > 0);
    // 5. That process dies too: a new incarnation fences the old producer,
    //    aborts what it left open, and its first commit publishes every
    //    epoch the progress offsets do not cover, once.
    drop((warm, sink, store));
    broker.restart();
    let (mut source, mut sink, store) = job.open(&broker, &t, 2);
    assert!(sink.pending().is_some());
    let conn = broker.connect();
    producer.epoch(&conn, &t, &changes[9], true);
    source.poll().unwrap();
    run_cluster_with(&circuit(), &source, &mut sink, &store, &config(3, &[]), &[])
        .unwrap_or_else(|e| panic!("{e}: {:?}", sink.connection().last_error()));
    assert!(sink.pending().is_none(), "{:?}", sink.pending());
    let expected = from_source(&source, &changes);
    verify(&conn, &t, 2, &mut sink, &expected, "after broker crashes");
}

/// Kafka transactions: a new producer with the same transactional id
/// fences the old one; the old one's delayed in-flight request and its
/// commit are rejected; its open transaction is aborted and invisible to
/// read-committed readers. Idempotent retries never append twice.
#[test]
fn transactions_fence_zombies_and_delayed_requests() {
    let Some(broker) = Broker::start("fencing") else {
        return;
    };
    let conn = broker.connect();
    let out = topic("fence");
    conn.create_topic(&out, 1).unwrap();
    let rec = |v: u8| kafka_record(vec![v; 4]);
    let txn = topic("zombie");
    // The zombie opens a transaction, appends one batch and holds a second
    // request (as if it were still in flight in the network).
    let mut zombie = KafkaProducer::transactional(broker.connect(), &txn).unwrap();
    zombie.begin().unwrap();
    zombie.send(&out, 0, &[rec(1), rec(2)]).unwrap();
    let delayed = zombie.hold(&out, 0, &[rec(3)]).unwrap();
    // Nothing is visible to read-committed readers while it is open.
    let (_, lso) = conn.offsets(&out, 0).unwrap();
    assert_eq!(
        lso, 0,
        "last stable offset stays below the open transaction"
    );
    // Its successor fences it and aborts the open transaction.
    let mut next = KafkaProducer::transactional(broker.connect(), &txn).unwrap();
    assert!(next.id().1 > zombie.id().1 || next.id().0 != zombie.id().0);
    // The delayed request arrives now: rejected, not appended.
    assert_eq!(
        delayed.deliver(&conn).unwrap_err(),
        Error::Kafka("producer fenced"),
        "{:?}",
        conn.last_error()
    );
    // The zombie can neither write nor commit any more.
    assert_eq!(
        zombie.send(&out, 0, &[rec(4)]).unwrap_err(),
        Error::Kafka("producer fenced")
    );
    assert!(zombie.commit().is_err());
    assert!(zombie.fenced());
    // The successor's transaction commits atomically with an offset commit.
    next.begin().unwrap();
    next.send(&out, 0, &[rec(5)]).unwrap();
    next.send(&out, 0, &[rec(6)]).unwrap();
    let group = topic("progress");
    next.send_offsets(&GroupOffsets {
        group: group.clone(),
        generation: -1,
        member_id: String::new(),
        instance_id: None,
        offsets: vec![(out.clone(), 0, 2, "two".into())],
    })
    .unwrap();
    next.commit().unwrap();
    assert!(conn.wait_stable(&out, 0, Duration::from_secs(10)).unwrap());
    let (_, end) = conn.offsets(&out, 0).unwrap();
    let visible: Vec<Vec<u8>> = conn
        .fetch(&out, 0, 0, end)
        .unwrap()
        .into_iter()
        .map(|(_, r)| r.value.unwrap())
        .collect();
    assert_eq!(visible, vec![vec![5; 4], vec![6; 4]]);
    assert_eq!(
        conn.group_offsets(&group, &out, &[0]).unwrap()[&0],
        (2, "two".to_string())
    );
    // An aborted transaction leaves nothing visible, but its records are in
    // the log: a read-uncommitted reader sees them (and never the fenced
    // request).
    next.begin().unwrap();
    next.send(&out, 0, &[rec(7)]).unwrap();
    next.abort().unwrap();
    assert!(conn.wait_stable(&out, 0, Duration::from_secs(10)).unwrap());
    let (_, end) = conn.offsets(&out, 0).unwrap();
    assert_eq!(conn.fetch(&out, 0, 0, end).unwrap().len(), 2);
    let uncommitted = KafkaConnection::connect(&KafkaConfig {
        read_committed: false,
        ..broker.config()
    })
    .unwrap();
    let all: Vec<Vec<u8>> = uncommitted
        .fetch(&out, 0, 0, end)
        .unwrap()
        .into_iter()
        .map(|(_, r)| r.value.unwrap())
        .collect();
    assert_eq!(all.len(), 5);
    assert!(all.contains(&vec![1; 4]) && all.contains(&vec![7; 4]));
    assert!(
        !all.contains(&vec![3; 4]),
        "the fenced request never landed"
    );

    // Idempotence: the same request delivered twice (a retry after a lost
    // response, or a delayed duplicate) is appended once.
    let idem_topic = topic("idem");
    conn.create_topic(&idem_topic, 1).unwrap();
    let mut idem = KafkaProducer::idempotent(broker.connect()).unwrap();
    let first = idem.hold(&idem_topic, 0, &[rec(8), rec(9)]).unwrap();
    assert_eq!(first.deliver(&conn).unwrap(), 0);
    // The broker answers a duplicate with the original batch's offset
    // (older brokers: DUPLICATE_SEQUENCE_NUMBER, reported as -1).
    assert!(
        [0, -1].contains(&first.deliver(&conn).unwrap()),
        "duplicate recognised"
    );
    idem.send(&idem_topic, 0, &[rec(10)]).unwrap();
    // A request that skips a sequence number is refused outright.
    let skipped = idem.hold(&idem_topic, 0, &[rec(11)]).unwrap();
    let after = idem.hold(&idem_topic, 0, &[rec(12)]).unwrap();
    assert!(after.deliver(&conn).is_err(), "out of order sequence");
    skipped.deliver(&conn).unwrap();
    after.deliver(&conn).unwrap();
    let (_, end) = conn.offsets(&idem_topic, 0).unwrap();
    let got: Vec<u8> = conn
        .fetch(&idem_topic, 0, 0, end)
        .unwrap()
        .into_iter()
        .map(|(_, r)| r.value.unwrap()[0])
        .collect();
    assert_eq!(got, vec![8, 9, 10, 11, 12]);
}

/// A new sink incarnation republishes nothing the progress offsets cover;
/// a sink over another circuit's output is refused.
#[test]
fn sink_progress_never_duplicates() {
    let Some(broker) = Broker::start("sink_progress") else {
        return;
    };
    let conn = broker.connect();
    let t = topics(&conn, "prog", 2);
    let changes = changes(6);
    let expected = reference(&changes);
    let job = Job::new();
    let mut producer = Producer::new();
    let (mut source, mut sink, store) = job.open(&broker, &t, 2);
    for c in &changes {
        producer.epoch(&conn, &t, c, false);
        source.poll().unwrap();
    }
    run_cluster_with(&circuit(), &source, &mut sink, &store, &config(2, &[]), &[]).unwrap();
    assert!(sink.stats().published > 0);
    drop((source, sink, store));
    let (_, mut sink, _) = job.open(&broker, &t, 2);
    sink.flush().unwrap();
    assert_eq!(sink.stats().published, 0);
    assert_eq!(sink.stats().already_present, expected.len() as u64);
    assert_eq!(sink.published_through(), Some(expected.len() as u64 - 1));
    verify(&conn, &t, 2, &mut sink, &expected, "republish");
    // Read back as another circuit with the same output width: refused.
    let other = {
        let mut b = CircuitBuilder::new(l()).unwrap();
        let x = b.input(3).unwrap();
        let _ = b.input(2).unwrap();
        let _ = b.input(2).unwrap();
        b.finish(x).unwrap()
    };
    assert!(matches!(
        read_kafka_output(&conn, &other, &t.output, 2),
        Err(Error::Kafka("output topic belongs to another circuit"))
    ));
    // A sink of another circuit under the same transactional id refuses
    // the topic's progress.
    drop(sink);
    let (quarry, _) = QuarrySink::open(
        FsDevice::open(&job.root.join("sink")).unwrap(),
        DbLimits::default(),
        l(),
    )
    .unwrap();
    let mut wrong = KafkaSink::new(
        quarry,
        broker.connect(),
        &circuit(),
        KafkaSinkConfig {
            topic: t.output.clone(),
            partitions: 2,
            key_column: 0,
            max_batch_records: 7,
            transactional_id: job.txn.clone(),
        },
    )
    .unwrap();
    let mut p = KafkaProducer::transactional(broker.connect(), &format!("{}-x", job.txn)).unwrap();
    p.begin().unwrap();
    p.send_offsets(&GroupOffsets {
        group: format!("{}.progress", job.txn),
        generation: -1,
        member_id: String::new(),
        instance_id: None,
        offsets: (0..2)
            .map(|i| {
                (
                    t.output.clone(),
                    i,
                    0,
                    format!("vflux/1 e=99 t={}", "ab".repeat(16)),
                )
            })
            .collect(),
    })
    .unwrap();
    p.commit().unwrap();
    assert_eq!(
        wrong.flush().unwrap_err(),
        Error::Kafka("output topic belongs to another circuit")
    );
}

/// TLS only, SASL PLAIN and SCRAM-SHA-256 connect; a bad password and an
/// untrusted CA are typed errors.
#[test]
fn tls_and_sasl_mechanisms() {
    let Some(broker) = Broker::start("security") else {
        return;
    };
    if !broker.secured() {
        eprintln!("security: skipped (external PLAINTEXT broker)");
        return;
    }
    for l in [
        Listener::Tls,
        Listener::SaslTls(SaslMechanism::Plain),
        Listener::SaslTls(SaslMechanism::ScramSha256),
        Listener::SaslTls(SaslMechanism::ScramSha512),
    ] {
        let conn = broker.connect_with(l);
        let name = topic("sec");
        conn.create_topic(&name, 1).unwrap();
        conn.produce(&name, 0, vec![kafka_record(vec![1, 2, 3])])
            .unwrap();
        let got = conn.fetch(&name, 0, 0, 1).unwrap();
        assert_eq!(got[0].1.value.as_deref(), Some(&[1u8, 2, 3][..]), "{l:?}");
    }
    let mut bad = broker.config_for(Listener::SaslTls(SaslMechanism::ScramSha256));
    bad.security.sasl.as_mut().unwrap().password = "wrong".into();
    bad.retry_for = Duration::from_millis(100);
    assert_eq!(
        KafkaConnection::connect(&bad).unwrap_err(),
        Error::Kafka("sasl authentication failed")
    );
    let mut untrusted = broker.config_for(Listener::Tls);
    untrusted.security.tls.as_mut().unwrap().ca_pem = {
        let k = rcgen::KeyPair::generate().unwrap();
        rcgen::CertificateParams::new(vec!["other".to_string()])
            .unwrap()
            .self_signed(&k)
            .unwrap()
            .pem()
            .into_bytes()
    };
    untrusted.retry_for = Duration::from_millis(100);
    assert!(matches!(
        KafkaConnection::connect(&untrusted).unwrap_err(),
        Error::Kafka(_)
    ));
    // The PLAINTEXT port does not speak TLS.
    let mut wrong_port = broker.config_for(Listener::Tls);
    wrong_port.bootstrap = broker.config().bootstrap;
    wrong_port.retry_for = Duration::from_millis(100);
    wrong_port.request_timeout = Duration::from_secs(2);
    assert!(KafkaConnection::connect(&wrong_port).is_err());
}

/// Every codec round-trips through the broker, both ways against rskafka.
#[test]
fn compression_interop() {
    let Some(broker) = Broker::start("compression") else {
        return;
    };
    let values: Vec<Vec<u8>> = (0..200u32)
        .map(|i| format!("value-{i}-{}", "x".repeat((i % 50) as usize)).into_bytes())
        .collect();
    for codec in [
        KafkaCompression::None,
        KafkaCompression::Gzip,
        KafkaCompression::Snappy,
        KafkaCompression::Lz4,
        KafkaCompression::Zstd,
    ] {
        let mut cfg = broker.config();
        // rskafka is built without lz4 here (it would need liblz4).
        let cross = codec != KafkaCompression::Lz4;
        cfg.compression = codec;
        let conn = KafkaConnection::connect(&cfg).unwrap();
        let name = topic("codec");
        conn.create_topic(&name, 1).unwrap();
        conn.produce(&name, 0, values.iter().cloned().map(kafka_record).collect())
            .unwrap();
        let got: Vec<Vec<u8>> = conn
            .fetch(&name, 0, 0, values.len() as i64)
            .unwrap()
            .into_iter()
            .map(|(_, r)| r.value.unwrap())
            .collect();
        assert_eq!(got, values, "{codec:?}");
        if cross {
            assert_eq!(
                rskafka_values(&cfg.bootstrap[0], &name, 0),
                values,
                "rskafka reads Flux's {codec:?} batches"
            );
        }
    }
    // Batches rskafka compresses are read back by Flux.
    use rskafka::client::partition::{Compression, UnknownTopicHandling};
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let conn = broker.connect();
    for codec in [Compression::Gzip, Compression::Snappy, Compression::Zstd] {
        let name = topic("rsk");
        conn.create_topic(&name, 1).unwrap();
        rt.block_on(async {
            let client = rskafka::client::ClientBuilder::new(broker.config().bootstrap)
                .build()
                .await
                .unwrap();
            let p = client
                .partition_client(&name, 0, UnknownTopicHandling::Error)
                .await
                .unwrap();
            let records = values
                .iter()
                .map(|v| rskafka::record::Record {
                    key: None,
                    value: Some(v.clone()),
                    headers: BTreeMap::new(),
                    timestamp: rskafka::chrono::DateTime::from_timestamp_millis(0).unwrap(),
                })
                .collect();
            p.produce(records, codec).await.unwrap();
        });
        let got: Vec<Vec<u8>> = conn
            .fetch(&name, 0, 0, values.len() as i64)
            .unwrap()
            .into_iter()
            .map(|(_, r)| r.value.unwrap())
            .collect();
        assert_eq!(got, values, "{codec:?} from rskafka");
    }
}

/// Released offsets are deleted from Kafka; uncommitted epochs still replay.
#[test]
fn release_deletes_only_committed_records() {
    let Some(broker) = Broker::start("release") else {
        return;
    };
    let conn = broker.connect();
    let t = topics(&conn, "rel", 1);
    let changes = changes(8);
    let expected = reference(&changes);
    let job = Job::new();
    let mut producer = Producer::new();
    let (mut source, mut sink, store) = job.open(&broker, &t, 1);
    for c in &changes {
        producer.epoch(&conn, &t, c, false);
        source.poll().unwrap();
    }
    let all = source.epochs();
    let mut cfg = config(2, &[]);
    cfg.checkpoint_interval = 2;
    let mut steps = 30;
    loop {
        cfg.max_steps = steps;
        match run_cluster_with(&circuit(), &source, &mut sink, &store, &cfg, &[]) {
            Err(Error::Limit("simulation steps")) => {}
            other => panic!("{other:?}"),
        }
        if sink
            .last_commit()
            .unwrap()
            .is_some_and(|r| r.after_epoch >= 3)
        {
            break;
        }
        steps += 30;
    }
    let record = sink.last_commit().unwrap().unwrap();
    assert!(record.after_epoch + 1 < all);
    source.release(&record).unwrap();
    for p in source.committed(Some(&record)).unwrap() {
        let (earliest, _) = conn.offsets(&p.topic, p.partition).unwrap();
        assert_eq!(earliest, p.offset);
    }
    drop((source, sink, store));
    // A restarted job replays the uncommitted epochs from Kafka.
    let (source, mut sink, store) = job.open(&broker, &t, 1);
    assert_eq!(
        source.read(0).unwrap_err(),
        Error::Kafka("offset out of range"),
        "released records are gone"
    );
    let run =
        run_cluster_with(&circuit(), &source, &mut sink, &store, &config(2, &[]), &[]).unwrap();
    assert_eq!(run.start_epoch, record.after_epoch + 1);
    verify(&conn, &t, 1, &mut sink, &expected, "after release");
}

/// Malformed records and LSN gaps stop the epoch with typed errors; a torn
/// index append is cut off; an index for other bindings is refused.
#[test]
fn malformed_records_gaps_and_index_recovery() {
    let Some(broker) = Broker::start("malformed") else {
        return;
    };
    let conn = broker.connect();
    let t = topics(&conn, "bad", 1);
    let job = Job::new();
    let mut producer = Producer::new();
    producer.epoch(&conn, &t, &changes(1)[0], false);
    let (mut source, _sink, _store) = job.open(&broker, &t, 1);
    assert_eq!(source.poll().unwrap(), Some(0));
    let header = |lsn, partition_id, schema_id, weight| CdcHeader {
        source_lsn: lsn,
        event_ts_ms: 0,
        z_weight: weight,
        partition_id,
        schema_id,
        flags: 0,
    };
    for (value, err) in [
        (vec![1, 2, 3], Error::Cdc("record length")),
        (
            cdc_value(header(100, 3, 8, 1), &[5, 6]),
            Error::Cdc("schema"),
        ),
        (
            cdc_value(header(100, 2, 7, 1), &[5, 6]),
            Error::Cdc("partition"),
        ),
        (
            cdc_value(header(100, 3, 7, 0), &[5, 6]),
            Error::Cdc("weight"),
        ),
        (
            cdc_value(header(105, 3, 7, 1), &[5, 6]),
            Error::Cdc("source offset"),
        ),
    ] {
        let regions = topic("bad-regions");
        conn.create_topic(&regions, 1).unwrap();
        let mut b = bindings(&t);
        b[3].topic = regions.clone();
        let dir = tempfile::tempdir().unwrap();
        let (mut s, _) = KafkaSource::open(
            broker.connect(),
            &circuit(),
            policy(),
            b,
            dir.path().join("idx"),
            KafkaSourceLimits::default(),
        )
        .unwrap();
        conn.produce(&regions, 0, vec![kafka_record(value)])
            .unwrap();
        assert_eq!(s.poll().unwrap_err(), err);
        assert_eq!(s.epochs(), 0, "no epoch is cut");
    }
    // Torn index tail: append garbage shorter than a frame.
    drop(source);
    let path = job.root.join("epochs.idx");
    let len = std::fs::metadata(&path).unwrap().len();
    {
        use std::io::Write;
        let mut f = std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap();
        f.write_all(&[7; 13]).unwrap();
        f.sync_all().unwrap();
    }
    let (source, torn) = KafkaSource::open(
        broker.connect(),
        &circuit(),
        policy(),
        bindings(&t),
        &path,
        KafkaSourceLimits::default(),
    )
    .unwrap();
    assert_eq!((torn, source.epochs()), (13, 1));
    assert_eq!(std::fs::metadata(&path).unwrap().len(), len);
    drop(source);
    // Other bindings over the same index are refused.
    let mut b = bindings(&t);
    b.swap(0, 1);
    assert_eq!(
        KafkaSource::open(
            broker.connect(),
            &circuit(),
            policy(),
            b,
            &path,
            KafkaSourceLimits::default()
        )
        .unwrap_err(),
        Error::Storage("kafka index header")
    );
}

/// With a dead-letter topic, malformed records are copied there and
/// skipped: the partition keeps flowing, LSN gaps still stop it.
#[test]
fn malformed_records_go_to_the_dead_letter_topic() {
    let Some(broker) = Broker::start("dead_letter") else {
        return;
    };
    let conn = broker.connect();
    let t = topics(&conn, "dlq", 1);
    let dlq = topic("dlq");
    conn.create_topic(&dlq, 1).unwrap();
    let mut producer = Producer::new();
    let c = changes(3);
    producer.epoch(&conn, &t, &c[0], false);
    let bad = [
        vec![9u8; 5],
        cdc_value(
            CdcHeader {
                source_lsn: 1,
                event_ts_ms: 0,
                z_weight: 0,
                partition_id: 3,
                schema_id: 7,
                flags: 0,
            },
            &[1, 1],
        ),
    ];
    let bad_offset = conn
        .produce(
            &t.regions,
            0,
            bad.iter().cloned().map(kafka_record).collect(),
        )
        .unwrap();
    producer.epoch(&conn, &t, &c[1], false);
    let dir = tempfile::tempdir().unwrap();
    let limits = KafkaSourceLimits {
        dead_letter: Some(dlq.clone()),
        ..KafkaSourceLimits::default()
    };
    let (mut s, _) = KafkaSource::open(
        broker.connect(),
        &circuit(),
        policy(),
        bindings(&t),
        dir.path().join("idx"),
        limits.clone(),
    )
    .unwrap();
    assert_eq!(s.poll().unwrap(), Some(0));
    assert_eq!(s.stats().dead_lettered, 2);
    let copies = conn.fetch(&dlq, 0, 0, 2).unwrap();
    assert_eq!(copies.len(), 2);
    for (i, (_, r)) in copies.iter().enumerate() {
        assert_eq!(r.value.as_ref(), Some(&bad[i]));
        let key = format!("{}/0/{}", t.regions, bad_offset + i as i64);
        assert_eq!(r.key.as_deref(), Some(key.as_bytes()));
        assert!(r.headers.contains_key("vesper-flux-error"));
    }
    // The cut epoch holds exactly the valid records; a replay skips the
    // same records without copying them again.
    let mut e = circuit().executor();
    let got = e.tick(&s.read(0).unwrap()).unwrap();
    assert_eq!(sum(&[got], 3), sum(&reference(&c[..2]), 3));
    drop(s);
    let (s, _) = KafkaSource::open(
        broker.connect(),
        &circuit(),
        policy(),
        bindings(&t),
        dir.path().join("idx"),
        limits,
    )
    .unwrap();
    s.read(0).unwrap();
    assert_eq!(s.stats().dead_lettered, 0);
    assert_eq!(conn.offsets(&dlq, 0).unwrap().1, 2);
}

// ----------------------------------------------------------------------
// Consumer groups
// ----------------------------------------------------------------------

/// One input topic with four partitions, a per-partition pipeline (map),
/// and one Flux job per group member, each with its own output topic.
fn group_circuit() -> Circuit {
    let mut b = CircuitBuilder::new(l()).unwrap();
    let x = b.input(2).unwrap();
    let out = b.map(x, vec![Field::Column(0), Field::Column(1)]).unwrap();
    b.finish(out).unwrap()
}
fn group_policy() -> CdcPolicy {
    CdcPolicy {
        bindings: (0..4)
            .map(|p| SourceBinding {
                partition_id: p,
                schema_id: 1,
                input: 0,
                first_lsn: 0,
                weights: WeightPolicy::Unit,
            })
            .collect(),
        max_batch_bytes: 1 << 20,
        max_records: 4096,
    }
}
/// Rows `(partition, n)` for `n` in `from..to`, one record each.
fn produce_group(conn: &KafkaConnection, input: &str, from: i64, to: i64) {
    for p in 0..4i32 {
        let records = (from..to)
            .map(|n| {
                kafka_record(cdc_value(
                    CdcHeader {
                        source_lsn: n as u64,
                        event_ts_ms: 0,
                        z_weight: 1,
                        partition_id: p as u32,
                        schema_id: 1,
                        flags: 0,
                    },
                    &[i64::from(p), n],
                ))
            })
            .collect();
        conn.produce(input, p, records).unwrap();
    }
}

/// A member's job: its group source, its Kafka sink (which commits the
/// group offsets inside each output transaction) and its CAS.
struct Member {
    source: Rc<KafkaGroupSource>,
    sink: KafkaSink<QuarrySink<FsDevice>>,
    store: Store,
    output: String,
}
fn member(cfg: &KafkaConfig, input: &str, output: &str, group: &str, root: &Path) -> Member {
    std::fs::create_dir_all(root.join("sink")).unwrap();
    let c = group_circuit();
    let source = Rc::new(
        KafkaGroupSource::join(
            KafkaConnection::connect(cfg).unwrap(),
            &c,
            group_policy(),
            (0..4)
                .map(|p| KafkaBinding {
                    topic: input.to_string(),
                    partition: p,
                })
                .collect(),
            root.join("group.idx"),
            KafkaSourceLimits::default(),
            KafkaGroupConfig {
                session_timeout: Duration::from_secs(10),
                rebalance_timeout: Duration::from_secs(20),
                heartbeat_interval: Duration::from_millis(200),
                ..KafkaGroupConfig::new(group)
            },
        )
        .unwrap(),
    );
    let (quarry, _) = QuarrySink::open(
        FsDevice::open(&root.join("sink")).unwrap(),
        DbLimits::default(),
        l(),
    )
    .unwrap();
    let sink = KafkaSink::new(
        quarry,
        KafkaConnection::connect(cfg).unwrap(),
        &c,
        KafkaSinkConfig {
            topic: output.to_string(),
            partitions: 1,
            key_column: 0,
            max_batch_records: 64,
            transactional_id: format!("{output}-txn"),
        },
    )
    .unwrap()
    .with_group_offsets(source.clone());
    Member {
        source,
        sink,
        store: Store::open(root.join("cas")).unwrap(),
        output: output.to_string(),
    }
}
impl Member {
    /// Poll until idle (or rebalancing), then run the cluster over every
    /// cut epoch: the sink publishes and commits the group offsets.
    fn step(&mut self) -> GroupPoll {
        let mut last = GroupPoll::Idle;
        loop {
            match self.source.poll().unwrap() {
                GroupPoll::Epoch(_) => last = GroupPoll::Idle,
                other => {
                    if other == GroupPoll::Rebalancing {
                        last = other;
                    }
                    break;
                }
            }
        }
        let cut = self.source.epochs();
        let committed = self.sink.last_commit().unwrap().map(|r| r.after_epoch);
        if cut > 0 && committed.is_none_or(|c| c + 1 < cut) {
            run_cluster_with(
                &group_circuit(),
                &*self.source,
                &mut self.sink,
                &self.store,
                &ClusterConfig::new(2, vec![0]),
                &[],
            )
            .unwrap();
            assert!(self.sink.pending().is_none(), "{:?}", self.sink.pending());
        }
        last
    }
}
/// Every row of a member's output topic with its weight.
fn rows(conn: &KafkaConnection, output: &str) -> BTreeMap<Vec<i64>, i64> {
    let mut out = BTreeMap::new();
    for z in read_kafka_output(conn, &group_circuit(), output, 1)
        .unwrap()
        .values()
    {
        for (r, w) in z.entries() {
            *out.entry(r.clone()).or_default() += w;
        }
    }
    out
}

/// A second member joins while the first runs: the rebalance waits for the
/// first member's published checkpoint, the partitions it gives away
/// continue from the offsets committed in that checkpoint's transaction,
/// and the two output topics together hold every input row exactly once.
#[test]
fn consumer_group_rebalances_at_checkpoints() {
    let Some(broker) = Broker::start("group") else {
        return;
    };
    let cfg = broker.config();
    let conn = broker.connect();
    let input = topic("group-in");
    let (out_a, out_b) = (topic("group-a"), topic("group-b"));
    conn.create_topic(&input, 4).unwrap();
    conn.create_topic(&out_a, 1).unwrap();
    conn.create_topic(&out_b, 1).unwrap();
    let group = topic("group");
    let dir_a = tempfile::tempdir().unwrap();
    produce_group(&conn, &input, 0, 5);
    let (go_tx, go_rx) = std::sync::mpsc::channel::<()>();
    let (done_tx, done_rx) = std::sync::mpsc::channel::<(u64, usize)>();
    let a = {
        let (cfg, input, out_a, group) = (cfg.clone(), input.clone(), out_a.clone(), group.clone());
        let root = dir_a.path().to_path_buf();
        std::thread::spawn(move || {
            let mut m = member(&cfg, &input, &out_a, &group, &root);
            assert_eq!(m.source.assignment().len(), 4, "alone: every partition");
            m.step();
            go_tx.send(()).unwrap();
            // Keep running until the rebalance completed with B.
            let deadline = Instant::now() + Duration::from_secs(60);
            let mut saw_rebalancing = false;
            while m.source.joins() < 2 {
                assert!(Instant::now() < deadline, "no rebalance");
                saw_rebalancing |= m.step() == GroupPoll::Rebalancing;
                std::thread::sleep(Duration::from_millis(50));
            }
            let kept = m.source.assignment().len();
            // After the rebalance: more input, then drain.
            let until = Instant::now() + Duration::from_secs(8);
            while Instant::now() < until {
                m.step();
                std::thread::sleep(Duration::from_millis(100));
            }
            m.source.leave().ok();
            let _ = saw_rebalancing;
            done_tx.send((m.source.joins(), kept)).unwrap();
            m.output
        })
    };
    go_rx.recv().unwrap();
    // B joins: its JoinGroup blocks until A rejoined at a checkpoint.
    let dir_b = tempfile::tempdir().unwrap();
    let b = {
        let (cfg, input, out_b, group) = (cfg.clone(), input.clone(), out_b.clone(), group.clone());
        let root = dir_b.path().to_path_buf();
        std::thread::spawn(move || {
            let mut m = member(&cfg, &input, &out_b, &group, &root);
            let got = m.source.assignment().len();
            let until = Instant::now() + Duration::from_secs(10);
            while Instant::now() < until {
                m.step();
                std::thread::sleep(Duration::from_millis(100));
            }
            m.source.leave().ok();
            (got, m.output)
        })
    };
    std::thread::sleep(Duration::from_secs(3));
    produce_group(&conn, &input, 5, 12);
    let (joins_a, kept_a) = done_rx.recv().unwrap();
    let out_a = a.join().unwrap();
    let (got_b, out_b) = b.join().unwrap();
    assert!(joins_a >= 2);
    assert_eq!(kept_a + got_b, 4, "the partitions are split");
    assert!(got_b > 0 && kept_a > 0);
    let mut all = rows(&conn, &out_a);
    for (r, w) in rows(&conn, &out_b) {
        *all.entry(r).or_default() += w;
    }
    let want: BTreeMap<Vec<i64>, i64> = (0..4)
        .flat_map(|p| (0..12).map(move |n| (vec![p, n], 1)))
        .collect();
    assert_eq!(all, want, "every input row exactly once across members");
    // The group's offsets sit at the end of every partition.
    let committed = conn.group_offsets(&group, &input, &[0, 1, 2, 3]).unwrap();
    for p in 0..4 {
        assert_eq!(committed[&p].0, conn.offsets(&input, p).unwrap().1);
    }
}
