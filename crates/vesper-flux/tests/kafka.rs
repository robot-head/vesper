//! Interop with a real Kafka broker (Apache Kafka in KRaft mode).
//!
//! The broker comes from the environment:
//! * `VESPER_KAFKA_HOME`: an Apache Kafka distribution directory. Each test
//!   starts its own single-node KRaft broker on free local ports, and the
//!   broker-crash tests SIGKILL and restart it.
//! * `VESPER_KAFKA_BOOTSTRAP`: an already running broker (`host:port`); the
//!   tests that kill the broker are skipped.
//!
//! Without either, every test prints a note and passes, unless
//! `VESPER_KAFKA_REQUIRE=1` (set in CI), which makes a missing broker a failure.
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};
use vesper_flux::*;
use vesper_loom_cas::Store;
use vesper_quarry::device::FsDevice;
use vesper_quarry::durable::DbLimits;

// ----------------------------------------------------------------------
// Broker harness
// ----------------------------------------------------------------------

struct Broker {
    home: Option<PathBuf>,
    dir: tempfile::TempDir,
    port: u16,
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
impl Broker {
    /// A broker for one test, or `None` (test skipped) without one.
    fn start(test: &str) -> Option<Self> {
        let require = std::env::var("VESPER_KAFKA_REQUIRE").is_ok_and(|v| v == "1");
        let dir = tempfile::tempdir().unwrap();
        if let Ok(home) = std::env::var("VESPER_KAFKA_HOME") {
            let port = free_port();
            let controller = free_port();
            let props = dir.path().join("server.properties");
            let logs = dir.path().join("logs");
            std::fs::write(
                &props,
                format!(
                    "process.roles=broker,controller\n\
                     node.id=1\n\
                     controller.quorum.voters=1@127.0.0.1:{controller}\n\
                     listeners=PLAINTEXT://127.0.0.1:{port},CONTROLLER://127.0.0.1:{controller}\n\
                     advertised.listeners=PLAINTEXT://127.0.0.1:{port}\n\
                     controller.listener.names=CONTROLLER\n\
                     inter.broker.listener.name=PLAINTEXT\n\
                     listener.security.protocol.map=CONTROLLER:PLAINTEXT,PLAINTEXT:PLAINTEXT\n\
                     log.dirs={}\n\
                     num.partitions=1\n\
                     auto.create.topics.enable=false\n\
                     offsets.topic.replication.factor=1\n\
                     transaction.state.log.replication.factor=1\n\
                     transaction.state.log.min.isr=1\n\
                     share.coordinator.state.topic.replication.factor=1\n\
                     share.coordinator.state.topic.min.isr=1\n\
                     group.initial.rebalance.delay.ms=0\n",
                    logs.display()
                ),
            )
            .unwrap();
            let out = Command::new(Path::new(&home).join("bin/kafka-storage.sh"))
                .args(["format", "-t", "q1Sh-9_ISia_zwGINzRvyQ", "-c"])
                .arg(&props)
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
                external: None,
                child: None,
            };
            broker.restart();
            eprintln!("{test}: Kafka broker on 127.0.0.1:{port}");
            return Some(broker);
        }
        if let Ok(bootstrap) = std::env::var("VESPER_KAFKA_BOOTSTRAP") {
            return Some(Self {
                home: None,
                dir,
                port: 0,
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
    fn bootstrap(&self) -> String {
        self.external
            .clone()
            .unwrap_or_else(|| format!("127.0.0.1:{}", self.port))
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
                let probe = format!("probe-{}", Instant::now().elapsed().as_nanos());
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
        KafkaConfig {
            retry_for: Duration::from_secs(5),
            request_timeout: Duration::from_secs(10),
            ..KafkaConfig::new(self.bootstrap())
        }
    }
    fn connect(&self) -> KafkaConnection {
        KafkaConnection::connect(&self.config()).unwrap()
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
fn changes(epochs: usize) -> Vec<[Vec<(Vec<i64>, i64)>; 3]> {
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
fn reference(changes: &[[Vec<(Vec<i64>, i64)>; 3]]) -> Vec<ZSet> {
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
    /// each batch (a producer retry the source must drop).
    fn epoch(
        &mut self,
        conn: &KafkaConnection,
        t: &Topics,
        changes: &[Vec<(Vec<i64>, i64)>; 3],
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
}
impl Job {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().to_path_buf();
        std::fs::create_dir_all(root.join("sink")).unwrap();
        Self { _dir: dir, root }
    }
    /// Open everything as a new process would.
    fn open(
        &self,
        broker: &Broker,
        t: &Topics,
        output_partitions: i32,
    ) -> (KafkaSource, KafkaSink<QuarrySink<FsDevice>>, Store) {
        let (source, _) = KafkaSource::open(
            broker.connect(),
            &circuit(),
            policy(),
            bindings(t),
            self.root.join("epochs.idx"),
            KafkaSourceLimits {
                cache_epochs: 2,
                ..KafkaSourceLimits::default()
            },
        )
        .unwrap();
        let (quarry, _) = QuarrySink::open(
            FsDevice::open(&self.root.join("sink")).unwrap(),
            DbLimits::default(),
            l(),
        )
        .unwrap();
        let sink = KafkaSink::new(
            quarry,
            broker.connect(),
            &circuit(),
            KafkaSinkConfig {
                topic: t.output.clone(),
                partitions: output_partitions,
                key_column: 0,
                max_batch_records: 7,
            },
        )
        .unwrap();
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
/// The single-executor outputs of the epochs the source cut, checked to
/// integrate to the workload's reference (an epoch may group several
/// workload steps when records were produced ahead of the poll).
fn from_source(source: &KafkaSource, changes: &[[Vec<(Vec<i64>, i64)>; 3]]) -> Vec<ZSet> {
    let mut e = circuit().executor();
    let out: Vec<ZSet> = (0..source.epochs())
        .map(|i| e.tick(&source.read(i).unwrap()).unwrap())
        .collect();
    let sum = |v: &[ZSet]| {
        let mut t = ZSet::empty(3, l()).unwrap();
        for z in v {
            t = t.plus(z, l()).unwrap();
        }
        t
    };
    assert_eq!(
        sum(&out),
        sum(&reference(changes)),
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

// ----------------------------------------------------------------------
// Tests
// ----------------------------------------------------------------------

#[test]
fn kafka_source_and_sink_round_trip_with_a_live_rescale() {
    let Some(broker) = Broker::start("round_trip") else {
        return;
    };
    let conn = broker.connect();
    let t = topics(&conn, "rt", 3);
    let changes = changes(10);
    let expected = reference(&changes);
    let job = Job::new();
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
    assert!(sink.stats().published > 0);
    verify(&conn, &t, 3, &mut sink, &expected, "round trip");
    // Offsets are tied to the commit: the last commit covers every record.
    let record = sink.last_commit().unwrap().unwrap();
    let committed = source.committed(Some(&record)).unwrap();
    for (p, b) in committed.iter().zip(bindings(&t)) {
        let (_, latest) = conn.offsets(&b.topic, b.partition).unwrap();
        assert_eq!(p.offset, latest, "{p:?}");
    }
    assert_eq!(
        committed.iter().map(|p| p.next_lsn).collect::<Vec<_>>(),
        producer.next.to_vec()
    );
}

/// Kill the Flux process at many points (an interrupted run whose state is
/// left on disk as it is), with worker, coordinator and cluster faults
/// inside the runs, while new records keep arriving and the job rescales.
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
        if attempt % 3 != 0 {
            cfg.max_steps = 60 + 23 * attempt;
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
        // The process dies here: nothing is closed or flushed.
        drop((source, sink, store));
    }
    assert!(interrupted >= 2, "{interrupted}");
    assert!(faults_seen > 0);
}

/// SIGKILL the broker while the job reads and publishes; restart it and
/// resume. Nothing is lost or duplicated in the sink or the output topic.
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
    let (mut source, mut sink, store) = job.open(&broker, &t, 2);
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
    let source = KafkaSource::open(
        broker.connect(),
        &circuit(),
        policy(),
        bindings(&t),
        job.root.join("epochs.idx"),
        KafkaSourceLimits::default(),
    )
    .unwrap()
    .0;
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
    // 3. Back up: the job runs (rescaling live) and publishes.
    broker.restart();
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
    assert!(sink.pending().is_none());
    // 4. The broker dies between commit and publication: the commits land
    //    in Quarry, the publication stays pending, and a flush after the
    //    restart publishes every epoch once.
    drop((source, sink, store));
    let (mut source, mut sink, store) = job.open(&broker, &t, 2);
    {
        let conn = broker.connect();
        for c in &changes[6..] {
            producer.epoch(&conn, &t, c, true);
            source.poll().unwrap();
        }
    }
    let warm = KafkaSource::open(
        broker.connect(),
        &circuit(),
        policy(),
        bindings(&t),
        job.root.join("epochs.idx"),
        KafkaSourceLimits {
            cache_epochs: 64,
            ..KafkaSourceLimits::default()
        },
    )
    .unwrap()
    .0;
    for e in 0..warm.epochs() {
        warm.read(e).unwrap();
    }
    drop(source);
    broker.kill();
    let run = run_cluster_with(&circuit(), &warm, &mut sink, &store, &config(3, &[]), &[]).unwrap();
    assert_eq!(run.outputs.len() as u64, warm.epochs() - run.start_epoch);
    assert!(sink.pending().is_some());
    assert!(sink.stats().failures > 0);
    broker.restart();
    sink.flush()
        .unwrap_or_else(|e| panic!("{e}: {:?}", sink.connection().last_error()));
    let conn = broker.connect();
    let expected = from_source(&warm, &changes);
    verify(&conn, &t, 2, &mut sink, &expected, "after broker crashes");
}

/// The fence: a new sink incarnation, or a retry after an ambiguous produce
/// failure, skips what the topic already holds.
#[test]
fn sink_fence_never_duplicates() {
    let Some(broker) = Broker::start("sink_fence") else {
        return;
    };
    let conn = broker.connect();
    let t = topics(&conn, "fence", 2);
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
    let published = sink.stats().published;
    assert!(published > 0);
    drop((source, sink, store));
    // A new process republishes nothing: the fence covers every epoch.
    let (_, mut sink, _) = job.open(&broker, &t, 2);
    sink.flush().unwrap();
    assert_eq!(sink.stats().published, 0);
    assert_eq!(sink.stats().fences_read, 2);
    verify(&conn, &t, 2, &mut sink, &expected, "republish");
    drop(sink);
    // A record from another circuit at the tail of a partition is refused.
    let mut wrong = kafka_record(cdc_value(
        CdcHeader {
            source_lsn: 999,
            event_ts_ms: 0,
            z_weight: 1,
            partition_id: 0,
            schema_id: 0,
            flags: 0,
        },
        &[1, 2, 3],
    ));
    wrong
        .headers
        .insert("vesper-flux-epoch".into(), 99u64.to_be_bytes().to_vec());
    wrong
        .headers
        .insert("vesper-flux-seq".into(), 0u64.to_be_bytes().to_vec());
    wrong
        .headers
        .insert("vesper-flux-circuit".into(), vec![0xab; 16]);
    conn.produce(&t.output, 0, vec![wrong]).unwrap();
    let (_, mut sink, _) = job.open(&broker, &t, 2);
    assert_eq!(
        sink.flush().unwrap_err(),
        Error::Kafka("output topic belongs to another circuit")
    );
    assert!(read_kafka_output(&conn, &circuit(), &t.output, 2).is_err());
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
    // Run the first 4 epochs only: an index truncated view is not possible,
    // so stop the run after its second commit.
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
    let next = producer.next[3];
    for (value, err) in [
        (vec![1, 2, 3], Error::Cdc("record length")),
        (
            cdc_value(header(next, 3, 8, 1), &[5, 6]),
            Error::Cdc("schema"),
        ),
        (
            cdc_value(header(next, 2, 7, 1), &[5, 6]),
            Error::Cdc("partition"),
        ),
        (
            cdc_value(header(next, 3, 7, 0), &[5, 6]),
            Error::Cdc("weight"),
        ),
        (
            cdc_value(header(next + 5, 3, 7, 1), &[5, 6]),
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
        let _ = next;
        let got = s.poll().unwrap_err();
        // The first record of a fresh partition must start at first_lsn.
        let ok = got == err
            || (err == Error::Cdc("source offset") && got == Error::Cdc("source offset"));
        assert!(ok, "{got:?} != {err:?}");
        assert_eq!(s.epochs(), 0, "no epoch is cut");
    }
    // Torn index tail: append garbage shorter than a frame.
    drop(source);
    let path = job.root.join("epochs.idx");
    let len = std::fs::metadata(&path).unwrap().len();
    std::fs::OpenOptions::new()
        .append(true)
        .open(&path)
        .unwrap()
        .write_all_at_end(&[7; 13]);
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

trait AppendAll {
    fn write_all_at_end(self, bytes: &[u8]);
}
impl AppendAll for std::fs::File {
    fn write_all_at_end(mut self, bytes: &[u8]) {
        use std::io::Write;
        self.write_all(bytes).unwrap();
        self.sync_all().unwrap();
    }
}
