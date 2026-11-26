//! The Kafka wire codecs over the Vesper port (`vesper/src/kafka_api.ves`,
//! #1401): one whole request encoder and one whole response decoder per
//! API, the response header, the consumer protocol's subscription and
//! assignment, and the record batch framing. Every request function
//! returns the whole frame (length, header and body); every response
//! function takes a body after its header. `Conn`, `Client` and
//! `KafkaGroupSource` keep the sockets, retries and state.
//!
//! The Rust codecs these replaced are in `kafka_reference.rs`, the
//! reference of the differential tests below and of the `kafka_wire` ABBA
//! workload (feature `abba-reference`).
use super::client::{Committed, Fetched, Joined};
use super::kafka::KafkaBinding;
use super::wire::{Fail, KResult, MALFORMED, committed_records, decode_batches};
use crate::Error;
use crate::port::binding::{self as port, KwErr};
use std::collections::BTreeMap;

#[cfg(any(test, feature = "abba-reference"))]
#[path = "kafka_reference.rs"]
pub(crate) mod reference;

/// What every request header carries: the API version, the correlation id
/// and the client id.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Head<'a> {
    pub(crate) version: i16,
    pub(crate) corr: i32,
    pub(crate) cid: &'a str,
}

/// `KwErr::Proto` reasons, by index (`P_*` in `kafka_api.ves`).
const PROTO: &[&str] = &["correlation id", "metadata"];

fn fail(e: KwErr, what: &str) -> Fail {
    match e {
        KwErr::Malformed => MALFORMED,
        KwErr::Overflow => Fail::Local(Error::Overflow),
        KwErr::Proto(i) => Fail::Proto(
            usize::try_from(i)
                .ok()
                .and_then(|i| PROTO.get(i))
                .copied()
                .unwrap_or("malformed response"),
        ),
        KwErr::Code { code, .. } => Fail::Code(code as i16, what.to_string()),
    }
}

/// A span of `body` from a decoder (start, length); a negative length is
/// empty.
fn span(body: &[u8], at: i64, len: i64) -> &[u8] {
    let (Ok(at), Ok(len)) = (usize::try_from(at), usize::try_from(len)) else {
        return &[];
    };
    body.get(at..at.saturating_add(len)).unwrap_or(&[])
}

/// A string span (checked as UTF-8 by the decoder).
fn text(body: &[u8], at: i64, len: i64) -> String {
    String::from_utf8_lossy(span(body, at, len)).into_owned()
}

/// A broker error with its message: `what` and the message, as the Rust
/// client formatted it.
fn coded(e: KwErr, body: &[u8], what: impl FnOnce(String) -> String) -> Fail {
    match e {
        KwErr::Code { code, at, len } => Fail::Code(
            code as i16,
            what(text(body, i64::try_from(at).unwrap_or(-1), len)),
        ),
        e => fail(e, ""),
    }
}

fn enc(r: Result<Vec<u8>, KwErr>) -> KResult<Vec<u8>> {
    r.map_err(|e| fail(e, ""))
}

fn hv<'a>(h: &Head<'a>) -> (i64, i64, &'a [u8]) {
    (i64::from(h.version), i64::from(h.corr), h.cid.as_bytes())
}

fn opt(s: Option<&str>) -> (bool, &[u8]) {
    (s.is_some(), s.unwrap_or("").as_bytes())
}

/// A broker address as `Conn::open` takes it.
pub(crate) fn address(host: &str, port: i32) -> String {
    if host.contains(':') {
        format!("[{host}]:{port}")
    } else {
        format!("{host}:{port}")
    }
}

// ----------------------------------------------------------------------
// Requests
// ----------------------------------------------------------------------

pub(crate) fn api_versions_request(h: &Head) -> KResult<Vec<u8>> {
    let (v, c, cid) = hv(h);
    enc(port::kw_req_api_versions(v, c, cid))
}

pub(crate) fn sasl_handshake_request(h: &Head, mechanism: &str) -> KResult<Vec<u8>> {
    let (v, c, cid) = hv(h);
    enc(port::kw_req_sasl_handshake(v, c, cid, mechanism.as_bytes()))
}

pub(crate) fn sasl_authenticate_request(h: &Head, message: &[u8]) -> KResult<Vec<u8>> {
    let (v, c, cid) = hv(h);
    enc(port::kw_req_sasl_authenticate(v, c, cid, message))
}

pub(crate) fn metadata_request(h: &Head, topic: &str) -> KResult<Vec<u8>> {
    let (v, c, cid) = hv(h);
    enc(port::kw_req_metadata(v, c, cid, topic.as_bytes()))
}

pub(crate) fn create_topic_request(h: &Head, topic: &str, partitions: i32) -> KResult<Vec<u8>> {
    let (v, c, cid) = hv(h);
    enc(port::kw_req_create_topic(
        v,
        c,
        cid,
        topic.as_bytes(),
        i64::from(partitions),
    ))
}

pub(crate) fn delete_records_request(
    h: &Head,
    topic: &str,
    partition: i32,
    offset: i64,
) -> KResult<Vec<u8>> {
    let (v, c, cid) = hv(h);
    enc(port::kw_req_delete_records(
        v,
        c,
        cid,
        topic.as_bytes(),
        i64::from(partition),
        offset,
    ))
}

pub(crate) fn list_offsets_request(
    h: &Head,
    topic: &str,
    partition: i32,
    ts: i64,
    committed: bool,
) -> KResult<Vec<u8>> {
    let (v, c, cid) = hv(h);
    enc(port::kw_req_list_offsets(
        v,
        c,
        cid,
        topic.as_bytes(),
        i64::from(partition),
        ts,
        committed,
    ))
}

pub(crate) fn fetch_request(
    h: &Head,
    topic: &str,
    partition: i32,
    offset: i64,
    max_bytes: i32,
    committed: bool,
) -> KResult<Vec<u8>> {
    let (v, c, cid) = hv(h);
    enc(port::kw_req_fetch(
        v,
        c,
        cid,
        topic.as_bytes(),
        i64::from(partition),
        offset,
        i64::from(max_bytes),
        committed,
    ))
}

pub(crate) fn produce_request(
    h: &Head,
    txn: Option<&str>,
    topic: &str,
    partition: i32,
    batch: &[u8],
) -> KResult<Vec<u8>> {
    let (v, c, cid) = hv(h);
    let (has_txn, txn) = opt(txn);
    enc(port::kw_req_produce(
        v,
        c,
        cid,
        has_txn,
        txn,
        topic.as_bytes(),
        i64::from(partition),
        batch,
    ))
}

pub(crate) fn find_coordinator_request(h: &Head, key: &str, kind: i8) -> KResult<Vec<u8>> {
    let (v, c, cid) = hv(h);
    enc(port::kw_req_find_coordinator(
        v,
        c,
        cid,
        key.as_bytes(),
        i64::from(kind),
    ))
}

pub(crate) fn offset_fetch_request(
    h: &Head,
    group: &str,
    topic: &str,
    partitions: &[i32],
) -> KResult<Vec<u8>> {
    let (v, c, cid) = hv(h);
    let parts: Vec<i64> = partitions.iter().map(|p| i64::from(*p)).collect();
    enc(port::kw_req_offset_fetch(
        v,
        c,
        cid,
        group.as_bytes(),
        topic.as_bytes(),
        &parts,
    ))
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn join_group_request(
    h: &Head,
    group: &str,
    session_ms: i32,
    rebalance_ms: i32,
    member_id: &str,
    instance_id: Option<&str>,
    protocol: &str,
    metadata: &[u8],
) -> KResult<Vec<u8>> {
    let (v, c, cid) = hv(h);
    let (has_instance, instance) = opt(instance_id);
    enc(port::kw_req_join_group(
        v,
        c,
        cid,
        group.as_bytes(),
        i64::from(session_ms),
        i64::from(rebalance_ms),
        member_id.as_bytes(),
        has_instance,
        instance,
        protocol.as_bytes(),
        metadata,
    ))
}

pub(crate) fn sync_group_request(
    h: &Head,
    group: &str,
    generation: i32,
    member_id: &str,
    instance_id: Option<&str>,
    assignments: &[(String, Vec<u8>)],
) -> KResult<Vec<u8>> {
    let (v, c, cid) = hv(h);
    let (has_instance, instance) = opt(instance_id);
    let mut data = Vec::new();
    let mut lens = Vec::with_capacity(assignments.len() * 2);
    for (m, a) in assignments {
        data.extend_from_slice(m.as_bytes());
        data.extend_from_slice(a);
        lens.push(m.len() as u64);
        lens.push(a.len() as u64);
    }
    enc(port::kw_req_sync_group(
        v,
        c,
        cid,
        group.as_bytes(),
        i64::from(generation),
        member_id.as_bytes(),
        has_instance,
        instance,
        &data,
        &lens,
    ))
}

pub(crate) fn heartbeat_request(
    h: &Head,
    group: &str,
    generation: i32,
    member_id: &str,
    instance_id: Option<&str>,
) -> KResult<Vec<u8>> {
    let (v, c, cid) = hv(h);
    let (has_instance, instance) = opt(instance_id);
    enc(port::kw_req_heartbeat(
        v,
        c,
        cid,
        group.as_bytes(),
        i64::from(generation),
        member_id.as_bytes(),
        has_instance,
        instance,
    ))
}

pub(crate) fn leave_group_request(
    h: &Head,
    group: &str,
    member_id: &str,
    instance_id: Option<&str>,
) -> KResult<Vec<u8>> {
    let (v, c, cid) = hv(h);
    let (has_instance, instance) = opt(instance_id);
    enc(port::kw_req_leave_group(
        v,
        c,
        cid,
        group.as_bytes(),
        member_id.as_bytes(),
        has_instance,
        instance,
    ))
}

pub(crate) fn init_producer_request(
    h: &Head,
    txn: Option<&str>,
    timeout_ms: i32,
) -> KResult<Vec<u8>> {
    let (v, c, cid) = hv(h);
    let (has_txn, txn) = opt(txn);
    enc(port::kw_req_init_producer(
        v,
        c,
        cid,
        has_txn,
        txn,
        i64::from(timeout_ms),
    ))
}

pub(crate) fn add_partitions_request(
    h: &Head,
    txn: &str,
    pid: (i64, i16),
    topic: &str,
    partition: i32,
) -> KResult<Vec<u8>> {
    let (v, c, cid) = hv(h);
    enc(port::kw_req_add_partitions(
        v,
        c,
        cid,
        txn.as_bytes(),
        pid.0,
        i64::from(pid.1),
        topic.as_bytes(),
        i64::from(partition),
    ))
}

pub(crate) fn add_offsets_request(
    h: &Head,
    txn: &str,
    pid: (i64, i16),
    group: &str,
) -> KResult<Vec<u8>> {
    let (v, c, cid) = hv(h);
    enc(port::kw_req_add_offsets(
        v,
        c,
        cid,
        txn.as_bytes(),
        pid.0,
        i64::from(pid.1),
        group.as_bytes(),
    ))
}

/// TxnOffsetCommit: `offsets` are (topic, partition, next offset,
/// metadata), grouped by topic in the request.
#[allow(clippy::too_many_arguments)]
pub(crate) fn txn_offset_commit_request(
    h: &Head,
    txn: &str,
    group: &str,
    pid: (i64, i16),
    generation: i32,
    member_id: &str,
    instance_id: Option<&str>,
    offsets: &[(String, i32, i64, String)],
) -> KResult<Vec<u8>> {
    let (v, c, cid) = hv(h);
    let (has_instance, instance) = opt(instance_id);
    let mut data = Vec::new();
    let mut lens = Vec::with_capacity(offsets.len() * 2);
    let mut nums = Vec::with_capacity(offsets.len() * 2);
    for (t, p, o, m) in offsets {
        data.extend_from_slice(t.as_bytes());
        data.extend_from_slice(m.as_bytes());
        lens.push(t.len() as u64);
        lens.push(m.len() as u64);
        nums.push(i64::from(*p));
        nums.push(*o);
    }
    enc(port::kw_req_txn_offset_commit(
        v,
        c,
        cid,
        txn.as_bytes(),
        group.as_bytes(),
        pid.0,
        i64::from(pid.1),
        i64::from(generation),
        member_id.as_bytes(),
        has_instance,
        instance,
        &data,
        &lens,
        &nums,
    ))
}

pub(crate) fn end_txn_request(
    h: &Head,
    txn: &str,
    pid: (i64, i16),
    commit: bool,
) -> KResult<Vec<u8>> {
    let (v, c, cid) = hv(h);
    enc(port::kw_req_end_txn(
        v,
        c,
        cid,
        txn.as_bytes(),
        pid.0,
        i64::from(pid.1),
        commit,
    ))
}

// ----------------------------------------------------------------------
// Responses
// ----------------------------------------------------------------------

/// The body after the response header (`Conn::recv`).
pub(crate) fn response_body(mut body: Vec<u8>, corr: i32, flex: bool) -> KResult<Vec<u8>> {
    let at = port::kw_resp_header(&body, i64::from(corr), flex).map_err(|e| fail(e, ""))?;
    body.drain(..usize::try_from(at).map_err(|_| MALFORMED)?);
    Ok(body)
}

/// ApiVersions: (key, min, max) per API.
pub(crate) fn api_versions(body: &[u8]) -> KResult<Vec<(i16, i16, i16)>> {
    let w = port::kw_resp_api_versions(body).map_err(|e| fail(e, "api versions"))?;
    Ok(w.as_chunks::<3>()
        .0
        .iter()
        .map(|k| (k[0] as i16, k[1] as i16, k[2] as i16))
        .collect())
}

pub(crate) fn sasl_handshake(body: &[u8]) -> KResult<()> {
    port::kw_resp_lead_code(body).map_err(|e| fail(e, "sasl handshake"))
}

/// SaslAuthenticate: the server's bytes.
pub(crate) fn sasl_authenticate(body: &[u8]) -> KResult<Vec<u8>> {
    let w = port::kw_resp_sasl_authenticate(body)
        .map_err(|e| coded(e, body, |m| format!("sasl authenticate: {m}")))?;
    Ok(span(body, w[0], w[1]).to_vec())
}

/// A Metadata response for one topic: the brokers (id, address) and the
/// (partition, leader) pairs read before `status`, which is the result.
#[derive(Debug)]
pub(crate) struct Metadata {
    pub(crate) brokers: Vec<(i32, String)>,
    pub(crate) leaders: Vec<(i32, i32)>,
    pub(crate) status: KResult<()>,
}

pub(crate) fn metadata(body: &[u8], topic: &str) -> Metadata {
    let w = port::kw_resp_metadata(body, topic.as_bytes());
    let status = match w[0] {
        0 => Ok(()),
        2 => Err(Fail::Code(w[1] as i16, "metadata".to_string())),
        _ => Err(MALFORMED),
    };
    let nb = usize::try_from(w[2]).unwrap_or(0);
    // The brokers count only once the whole body is read.
    let brokers = w[3..3 + 4 * nb]
        .as_chunks::<4>()
        .0
        .iter()
        .filter(|_| status.is_ok())
        .map(|b| (b[0] as i32, address(&text(body, b[1], b[2]), b[3] as i32)))
        .collect();
    let leaders = w[3 + 4 * nb..]
        .as_chunks::<2>()
        .0
        .iter()
        .map(|l| (l[0] as i32, l[1] as i32))
        .collect();
    Metadata {
        brokers,
        leaders,
        status,
    }
}

pub(crate) fn create_topic(body: &[u8], topic: &str) -> KResult<()> {
    port::kw_resp_create_topic(body)
        .map_err(|e| coded(e, body, |m| format!("create topic {topic}: {m}")))
}

pub(crate) fn delete_records(body: &[u8]) -> KResult<()> {
    port::kw_resp_delete_records(body).map_err(|e| fail(e, "delete records"))
}

pub(crate) fn list_offsets(body: &[u8], partition: i32) -> KResult<i64> {
    let w = port::kw_resp_list_offsets(body, i64::from(partition))
        .map_err(|e| fail(e, "list offsets"))?;
    if w[0] == 0 {
        return Err(Fail::Proto("list offsets"));
    }
    Ok(w[1])
}

/// Fetch: the last entry of `partition`; each entry's batches are decoded
/// before the next is read, as the Rust client did.
pub(crate) fn fetch(
    body: &[u8],
    partition: i32,
    max_decoded: usize,
    committed: bool,
) -> KResult<Fetched> {
    let w = port::kw_resp_fetch(body, i64::from(partition));
    let mut out = None;
    let mut i = 0;
    while w[i] == 1 {
        let na = usize::try_from(w[i + 6]).unwrap_or(0);
        let aborted: Vec<(i64, i64)> = w[i + 7..i + 7 + 2 * na]
            .as_chunks::<2>()
            .0
            .iter()
            .map(|a| (a[0], a[1]))
            .collect();
        let batches = decode_batches(span(body, w[i + 4], w[i + 5]), max_decoded)?;
        let (records, end) = committed_records(batches, &aborted, committed);
        out = Some(Fetched {
            high_watermark: w[i + 1],
            last_stable: w[i + 2],
            log_start: w[i + 3],
            records,
            end,
        });
        i += 7 + 2 * na;
    }
    match w[i] {
        0 => {}
        2 => return Err(Fail::Code(w[i + 1] as i16, "fetch".to_string())),
        _ => return Err(MALFORMED),
    }
    out.ok_or(Fail::Proto("fetch response"))
}

/// Produce: the base offset; a duplicate sequence (an idempotent retry the
/// broker already appended) is offset -1.
pub(crate) fn produce(body: &[u8], topic: &str, partition: i32) -> KResult<i64> {
    let w = port::kw_resp_produce(body, i64::from(partition)).map_err(|e| fail(e, ""))?;
    if w[0] == 0 {
        return Err(Fail::Proto("produce response"));
    }
    match w[1] {
        0 => Ok(w[2]),
        46 => Ok(-1),
        c => Err(Fail::Code(
            c as i16,
            format!("produce {topic}/{partition}: {}", text(body, w[3], w[4])),
        )),
    }
}

/// FindCoordinator: (node id, address).
pub(crate) fn find_coordinator(body: &[u8]) -> KResult<(i32, String)> {
    let w = port::kw_resp_find_coordinator(body)
        .map_err(|e| coded(e, body, |m| format!("find coordinator: {m}")))?;
    Ok((w[0] as i32, address(&text(body, w[1], w[2]), w[3] as i32)))
}

pub(crate) fn offset_fetch(body: &[u8]) -> KResult<BTreeMap<i32, Committed>> {
    let w = port::kw_resp_offset_fetch(body).map_err(|e| fail(e, "offset fetch"))?;
    let mut out = BTreeMap::new();
    for e in w.as_chunks::<4>().0 {
        out.insert(
            e[0] as i32,
            Committed {
                offset: e[1],
                metadata: text(body, e[2], e[3]),
            },
        );
    }
    Ok(out)
}

pub(crate) fn join_group(body: &[u8]) -> KResult<Joined> {
    let w = port::kw_resp_join_group(body).map_err(|e| fail(e, "join group"))?;
    let n = usize::try_from(w[8]).unwrap_or(0);
    let members = w[9..9 + 4 * n]
        .as_chunks::<4>()
        .0
        .iter()
        .map(|m| (text(body, m[0], m[1]), span(body, m[2], m[3]).to_vec()))
        .collect();
    let (code, generation) = (w[0] as i16, w[1] as i32);
    let protocol = text(body, w[2], w[3]);
    let member_id = text(body, w[6], w[7]);
    if code == 79 {
        return Ok(Joined {
            generation: -1,
            member_id,
            leader: String::new(),
            protocol,
            members,
            retry: true,
        });
    }
    if code != 0 {
        return Err(Fail::Code(code, "join group".to_string()));
    }
    Ok(Joined {
        generation,
        member_id,
        leader: text(body, w[4], w[5]),
        protocol,
        members,
        retry: false,
    })
}

pub(crate) fn sync_group(body: &[u8]) -> KResult<Vec<u8>> {
    let w = port::kw_resp_sync_group(body).map_err(|e| fail(e, "sync group"))?;
    Ok(span(body, w[0], w[1]).to_vec())
}

/// A body of the throttle time and the error code (Heartbeat, LeaveGroup,
/// AddOffsetsToTxn, EndTxn).
pub(crate) fn code(body: &[u8], what: &str) -> KResult<()> {
    port::kw_resp_code(body).map_err(|e| fail(e, what))
}

pub(crate) fn init_producer(body: &[u8]) -> KResult<(i64, i16)> {
    let w = port::kw_resp_init_producer(body).map_err(|e| fail(e, "init producer id"))?;
    Ok((w[0], w[1] as i16))
}

pub(crate) fn add_partitions(body: &[u8]) -> KResult<()> {
    port::kw_resp_add_partitions(body).map_err(|e| fail(e, "add partitions to txn"))
}

/// The per-partition errors of an offset commit.
pub(crate) fn offset_errors(body: &[u8], flex: bool, what: &str) -> KResult<()> {
    port::kw_resp_offset_errors(body, flex).map_err(|e| fail(e, what))
}

// ----------------------------------------------------------------------
// Consumer protocol
// ----------------------------------------------------------------------

/// Consumer protocol subscription (version 0): the topics, with the exact
/// bindings (topic, partition) as user data, so the leader can check that
/// every member subscribes to the same bindings.
pub(crate) fn subscription(topics: &[KafkaBinding]) -> KResult<Vec<u8>> {
    let mut data = Vec::new();
    let mut lens = Vec::with_capacity(topics.len());
    let mut parts = Vec::with_capacity(topics.len());
    for t in topics {
        data.extend_from_slice(t.topic.as_bytes());
        lens.push(t.topic.len() as u64);
        parts.push(i64::from(t.partition));
    }
    enc(port::kw_subscription(&data, &lens, &parts))
}

/// Consumer protocol assignment (version 0).
pub(crate) fn assignment(parts: &BTreeMap<String, Vec<i32>>) -> KResult<Vec<u8>> {
    let mut data = Vec::new();
    let mut lens = Vec::with_capacity(parts.len());
    let mut counts = Vec::with_capacity(parts.len());
    let mut flat = Vec::new();
    for (t, ps) in parts {
        data.extend_from_slice(t.as_bytes());
        lens.push(t.len() as u64);
        counts.push(ps.len() as u64);
        flat.extend(ps.iter().map(|p| i64::from(*p)));
    }
    enc(port::kw_assignment(&data, &lens, &counts, &flat))
}

pub(crate) fn parse_assignment(b: &[u8]) -> KResult<Vec<(String, i32)>> {
    let w = port::kw_parse_assignment(b).map_err(|e| fail(e, ""))?;
    Ok(w.as_chunks::<3>()
        .0
        .iter()
        .map(|e| (text(b, e[0], e[1]), e[2] as i32))
        .collect())
}

#[cfg(test)]
mod tests {
    use super::reference as r;
    use super::*;

    /// A deterministic xorshift stream.
    struct Rng(u64);
    impl Rng {
        fn new(seed: u64) -> Self {
            Self(seed.wrapping_mul(0x9e37_79b9_7f4a_7c15) | 1)
        }
        fn next(&mut self) -> u64 {
            self.0 ^= self.0 << 13;
            self.0 ^= self.0 >> 7;
            self.0 ^= self.0 << 17;
            self.0
        }
        fn below(&mut self, n: u64) -> u64 {
            self.next() % n.max(1)
        }
        fn i64(&mut self) -> i64 {
            match self.below(6) {
                0 => -1,
                1 => 0,
                2 => i64::MIN,
                3 => i64::MAX,
                _ => self.next() as i64 >> self.below(60),
            }
        }
        fn i32(&mut self) -> i32 {
            match self.below(5) {
                0 => -1,
                1 => 0,
                2 => i32::MAX,
                _ => self.next() as i32 >> self.below(30),
            }
        }
        fn i16(&mut self) -> i16 {
            match self.below(5) {
                0 => 0,
                1 => 0,
                2 => -1,
                3 => 79,
                _ => self.next() as i16,
            }
        }
        fn bytes(&mut self, max: u64) -> Vec<u8> {
            let n = match self.below(10) {
                0 => 0,
                1 => 300 + self.below(max.max(1)),
                _ => self.below(24),
            };
            (0..n).map(|_| self.next() as u8).collect()
        }
        /// A string: ASCII, multi-byte UTF-8, or (rarely, when `long`)
        /// longer than an i16 length allows.
        fn string(&mut self, long: bool) -> String {
            match self.below(if long { 12 } else { 11 }) {
                0 => String::new(),
                1 => "héllo-✓-𝄞".repeat(1 + self.below(4) as usize),
                2 => "topic-a".to_string(),
                3 => "topic-b".to_string(),
                11 => "x".repeat(32_768 + self.below(3) as usize),
                _ => (0..self.below(30))
                    .map(|_| char::from(b'a' + (self.next() % 26) as u8))
                    .collect(),
            }
        }
        fn opt(&mut self) -> Option<String> {
            (self.below(3) != 0).then(|| self.string(false))
        }
    }

    fn same<T: std::fmt::Debug>(port: &KResult<T>, reference: &KResult<T>, what: &str) {
        assert_eq!(format!("{port:?}"), format!("{reference:?}"), "{what}");
        OUTCOMES.with(|o| {
            let key = match reference {
                Ok(_) => "ok".to_string(),
                Err(e) => format!("{:?}", std::mem::discriminant(e)),
            };
            *o.borrow_mut().entry(key).or_insert(0) += 1;
        });
    }

    thread_local! {
        /// How often each outcome kind (ok, or each `Fail` variant) came up.
        static OUTCOMES: std::cell::RefCell<BTreeMap<String, usize>> =
            const { std::cell::RefCell::new(BTreeMap::new()) };
    }

    fn outcomes() -> usize {
        OUTCOMES.with(|o| {
            let o = o.borrow();
            o.len()
        })
    }

    /// Every request encoder against the Rust one on random fields.
    #[test]
    fn request_encoders_match_the_rust_encoders() {
        let mut g = Rng::new(1);
        for case in 0..4000 {
            let cid = g.string(case % 97 == 0);
            let h = Head {
                version: g.i16(),
                corr: g.i32(),
                cid: &cid,
            };
            let (t, s2) = (g.string(case % 89 == 0), g.string(false));
            let (o1, o2) = (g.opt(), g.opt());
            let (p, x, y) = (g.i32(), g.i64(), g.i32());
            let blob = g.bytes(5000);
            let flag = g.below(2) == 0;
            let parts: Vec<i32> = (0..g.below(6)).map(|_| g.i32()).collect();
            let assignments: Vec<(String, Vec<u8>)> = (0..g.below(4))
                .map(|_| (g.string(false), g.bytes(100)))
                .collect();
            let offsets: Vec<(String, i32, i64, String)> = (0..g.below(7))
                .map(|_| {
                    let t = ["b", "a", "c", "ab", ""][g.below(5) as usize].to_string();
                    (t, g.i32(), g.i64(), g.string(false))
                })
                .collect();
            let pid = (g.i64(), g.i16());
            let kind = g.next() as i8;
            let c = format!("case {case}");
            same(&api_versions_request(&h), &r::api_versions_request(&h), &c);
            same(
                &sasl_handshake_request(&h, &t),
                &r::sasl_handshake_request(&h, &t),
                &c,
            );
            same(
                &sasl_authenticate_request(&h, &blob),
                &r::sasl_authenticate_request(&h, &blob),
                &c,
            );
            same(&metadata_request(&h, &t), &r::metadata_request(&h, &t), &c);
            same(
                &create_topic_request(&h, &t, p),
                &r::create_topic_request(&h, &t, p),
                &c,
            );
            same(
                &delete_records_request(&h, &t, p, x),
                &r::delete_records_request(&h, &t, p, x),
                &c,
            );
            same(
                &list_offsets_request(&h, &t, p, x, flag),
                &r::list_offsets_request(&h, &t, p, x, flag),
                &c,
            );
            same(
                &fetch_request(&h, &t, p, x, y, flag),
                &r::fetch_request(&h, &t, p, x, y, flag),
                &c,
            );
            same(
                &produce_request(&h, o1.as_deref(), &t, p, &blob),
                &r::produce_request(&h, o1.as_deref(), &t, p, &blob),
                &c,
            );
            same(
                &find_coordinator_request(&h, &t, kind),
                &r::find_coordinator_request(&h, &t, kind),
                &c,
            );
            same(
                &offset_fetch_request(&h, &t, &s2, &parts),
                &r::offset_fetch_request(&h, &t, &s2, &parts),
                &c,
            );
            same(
                &join_group_request(&h, &t, p, y, &s2, o1.as_deref(), &cid, &blob),
                &r::join_group_request(&h, &t, p, y, &s2, o1.as_deref(), &cid, &blob),
                &c,
            );
            same(
                &sync_group_request(&h, &t, p, &s2, o2.as_deref(), &assignments),
                &r::sync_group_request(&h, &t, p, &s2, o2.as_deref(), &assignments),
                &c,
            );
            same(
                &heartbeat_request(&h, &t, p, &s2, o1.as_deref()),
                &r::heartbeat_request(&h, &t, p, &s2, o1.as_deref()),
                &c,
            );
            same(
                &leave_group_request(&h, &t, &s2, o2.as_deref()),
                &r::leave_group_request(&h, &t, &s2, o2.as_deref()),
                &c,
            );
            same(
                &init_producer_request(&h, o1.as_deref(), y),
                &r::init_producer_request(&h, o1.as_deref(), y),
                &c,
            );
            same(
                &add_partitions_request(&h, &t, pid, &s2, p),
                &r::add_partitions_request(&h, &t, pid, &s2, p),
                &c,
            );
            same(
                &add_offsets_request(&h, &t, pid, &s2),
                &r::add_offsets_request(&h, &t, pid, &s2),
                &c,
            );
            same(
                &txn_offset_commit_request(&h, &t, &s2, pid, p, &cid, o2.as_deref(), &offsets),
                &r::txn_offset_commit_request(&h, &t, &s2, pid, p, &cid, o2.as_deref(), &offsets),
                &c,
            );
            same(
                &end_txn_request(&h, &t, pid, flag),
                &r::end_txn_request(&h, &t, pid, flag),
                &c,
            );
        }
    }

    /// Mutations of a valid body: byte flips, truncations, splices and
    /// inserted bytes, so every read and every bound is reached.
    fn mutants(g: &mut Rng, good: &[u8], n: usize) -> Vec<Vec<u8>> {
        let mut out = vec![good.to_vec(), Vec::new()];
        for cut in 0..good.len().min(64) {
            out.push(good[..cut].to_vec());
        }
        for _ in 0..n {
            let mut b = good.to_vec();
            match g.below(5) {
                0 if !b.is_empty() => {
                    let cut = g.below(b.len() as u64) as usize;
                    b.truncate(cut);
                }
                1 => {
                    let at = g.below(b.len() as u64 + 1) as usize;
                    for _ in 0..1 + g.below(4) {
                        b.insert(at, g.next() as u8);
                    }
                }
                2 if !b.is_empty() => {
                    // A hostile length or count: 0x7f.., 0xff.., 0x80...
                    let at = g.below(b.len() as u64) as usize;
                    b[at] = [0x7f, 0xff, 0x80, 0x00, 0x01][g.below(5) as usize];
                }
                _ => {
                    for _ in 0..1 + g.below(3) {
                        if b.is_empty() {
                            break;
                        }
                        let at = g.below(b.len() as u64) as usize;
                        b[at] ^= 1 << g.below(8);
                    }
                }
            }
            out.push(b);
        }
        out
    }

    use super::reference::W;

    fn wstr(w: &mut W, s: &str) {
        w.str(s).unwrap();
    }

    /// Valid response bodies of each API with random fields.
    fn bodies(g: &mut Rng) -> Vec<(&'static str, Vec<u8>)> {
        let mut out = Vec::new();
        let names = ["t", "topic-a", "héllo", ""];
        let pick = |g: &mut Rng| names[g.below(4) as usize];
        // ApiVersions.
        let mut w = W::new(false);
        w.i16(g.i16()).array(3).unwrap();
        for _ in 0..3 {
            w.i16(g.i16()).i16(g.i16()).i16(g.i16());
        }
        out.push(("api_versions", w.buf));
        // SaslHandshake.
        let mut w = W::new(false);
        w.i16(g.i16()).array(1).unwrap();
        wstr(&mut w, "PLAIN");
        out.push(("sasl_handshake", w.buf));
        // SaslAuthenticate.
        let mut w = W::new(false);
        w.i16(g.i16()).nstr(g.opt().as_deref()).unwrap();
        w.bytes(&g.bytes(40)).unwrap().i64(g.i64());
        out.push(("sasl_authenticate", w.buf));
        // Metadata.
        let mut w = W::new(false);
        w.i32(0);
        let nb = g.below(4);
        w.array(nb as usize).unwrap();
        for _ in 0..nb {
            w.i32(g.i32());
            wstr(&mut w, ["localhost", "::1", "b1"][g.below(3) as usize]);
            w.i32(g.i32()).nstr(g.opt().as_deref()).unwrap();
        }
        w.nstr(g.opt().as_deref()).unwrap().i32(g.i32());
        let nt = g.below(4);
        w.array(nt as usize).unwrap();
        for _ in 0..nt {
            w.i16(if g.below(3) == 0 { g.i16() } else { 0 });
            wstr(&mut w, pick(g));
            w.bool(false);
            let np = g.below(4);
            w.array(np as usize).unwrap();
            for _ in 0..np {
                w.i16(if g.below(3) == 0 { 5 } else { 0 })
                    .i32(g.i32())
                    .i32(g.i32())
                    .i32(0);
                for _ in 0..3 {
                    let n = g.below(3);
                    w.array(n as usize).unwrap();
                    for _ in 0..n {
                        w.i32(g.i32());
                    }
                }
            }
            w.i32(0);
        }
        out.push(("metadata", w.buf));
        // CreateTopics.
        let mut w = W::new(false);
        w.i32(0);
        let n = g.below(3);
        w.array(n as usize).unwrap();
        for _ in 0..n {
            wstr(&mut w, pick(g));
            w.i16(if g.below(2) == 0 { 36 } else { 0 })
                .nstr(g.opt().as_deref())
                .unwrap();
        }
        out.push(("create_topic", w.buf));
        // DeleteRecords, AddPartitionsToTxn (same shape without the low
        // watermark).
        for add in [false, true] {
            let mut w = W::new(false);
            w.i32(0);
            let n = g.below(3);
            w.array(n as usize).unwrap();
            for _ in 0..n {
                wstr(&mut w, pick(g));
                let np = g.below(3);
                w.array(np as usize).unwrap();
                for _ in 0..np {
                    w.i32(g.i32());
                    if !add {
                        w.i64(g.i64());
                    }
                    w.i16(if g.below(3) == 0 { g.i16() } else { 0 });
                }
            }
            out.push((
                if add {
                    "add_partitions"
                } else {
                    "delete_records"
                },
                w.buf,
            ));
        }
        // ListOffsets.
        let mut w = W::new(false);
        w.i32(0);
        let n = g.below(3);
        w.array(n as usize).unwrap();
        for _ in 0..n {
            wstr(&mut w, pick(g));
            let np = g.below(3);
            w.array(np as usize).unwrap();
            for _ in 0..np {
                w.i32(g.below(3) as i32)
                    .i16(if g.below(4) == 0 { g.i16() } else { 0 })
                    .i64(g.i64())
                    .i64(g.i64())
                    .i32(g.i32());
            }
        }
        out.push(("list_offsets", w.buf));
        // Fetch, with real record batches.
        let mut w = W::new(false);
        w.i32(0).i16(if g.below(8) == 0 { 7 } else { 0 }).i32(0);
        let n = 1 + g.below(2);
        w.array(n as usize).unwrap();
        for _ in 0..n {
            wstr(&mut w, pick(g));
            let np = 1 + g.below(3);
            w.array(np as usize).unwrap();
            for _ in 0..np {
                w.i32(g.below(3) as i32)
                    .i16(if g.below(6) == 0 { 1 } else { 0 })
                    .i64(g.i64())
                    .i64(g.i64())
                    .i64(g.i64());
                let na = g.below(3);
                w.array(na as usize).unwrap();
                for _ in 0..na {
                    w.i64(g.below(3) as i64).i64(g.below(10) as i64);
                }
                w.i32(-1);
                if g.below(5) == 0 {
                    w.i32(-1);
                } else {
                    let mut recs = Vec::new();
                    for b in 0..g.below(3) {
                        let records: Vec<_> = (0..1 + g.below(3))
                            .map(|i| super::super::KafkaRecord {
                                key: Some(vec![i as u8]),
                                value: Some(g.bytes(30)),
                                headers: BTreeMap::new(),
                                timestamp: 1000 + i as i64,
                            })
                            .collect();
                        let stamp = super::super::wire::ProducerStamp {
                            id: g.below(3) as i64,
                            epoch: 0,
                            sequence: 0,
                            transactional: g.below(2) == 0,
                        };
                        let mut batch = super::super::wire::encode_batch(
                            &records,
                            super::super::KafkaCompression::None,
                            stamp,
                        )
                        .unwrap();
                        batch[..8].copy_from_slice(&(b as i64 * 10).to_be_bytes());
                        recs.extend_from_slice(&batch);
                    }
                    w.bytes(&recs).unwrap();
                }
            }
        }
        out.push(("fetch", w.buf));
        // Produce.
        let mut w = W::new(false);
        let n = g.below(3);
        w.array(n as usize).unwrap();
        for _ in 0..n {
            wstr(&mut w, pick(g));
            let np = g.below(3);
            w.array(np as usize).unwrap();
            for _ in 0..np {
                w.i32(g.below(3) as i32)
                    .i16([0, 0, 46, 47][g.below(4) as usize])
                    .i64(g.i64())
                    .i64(g.i64())
                    .i64(g.i64());
                let ne = g.below(3);
                w.array(ne as usize).unwrap();
                for _ in 0..ne {
                    w.i32(g.i32()).nstr(g.opt().as_deref()).unwrap();
                }
                w.nstr(g.opt().as_deref()).unwrap();
            }
        }
        w.i32(0);
        out.push(("produce", w.buf));
        // FindCoordinator.
        let mut w = W::new(false);
        w.i32(0)
            .i16(if g.below(3) == 0 { 15 } else { 0 })
            .nstr(g.opt().as_deref())
            .unwrap()
            .i32(g.i32());
        wstr(&mut w, ["localhost", "::1"][g.below(2) as usize]);
        w.i32(g.i32());
        out.push(("find_coordinator", w.buf));
        // OffsetFetch (flexible), with tagged fields.
        let mut w = W::new(true);
        w.i32(0);
        let n = g.below(3);
        w.array(n as usize).unwrap();
        for _ in 0..n {
            wstr(&mut w, pick(g));
            let np = g.below(4);
            w.array(np as usize).unwrap();
            for _ in 0..np {
                w.i32(g.below(4) as i32)
                    .i64(if g.below(3) == 0 { -1 } else { g.i64() })
                    .i32(-1)
                    .nstr(g.opt().as_deref())
                    .unwrap()
                    .i16(if g.below(4) == 0 { g.i16() } else { 0 });
                if g.below(3) == 0 {
                    w.uvarint(1).uvarint(g.below(9)).uvarint(2).i16(7);
                } else {
                    w.tags();
                }
            }
            w.tags();
        }
        w.i16(if g.below(4) == 0 { 16 } else { 0 });
        out.push(("offset_fetch", w.buf));
        // JoinGroup.
        let mut w = W::new(false);
        w.i32(0)
            .i16([0, 0, 79, 27][g.below(4) as usize])
            .i32(g.i32());
        wstr(&mut w, "range");
        wstr(&mut w, pick(g));
        wstr(&mut w, pick(g));
        let n = g.below(3);
        w.array(n as usize).unwrap();
        for _ in 0..n {
            wstr(&mut w, pick(g));
            w.nstr(g.opt().as_deref()).unwrap();
            if g.below(5) == 0 {
                w.i32(-1);
            } else {
                w.bytes(&g.bytes(50)).unwrap();
            }
        }
        out.push(("join_group", w.buf));
        // SyncGroup.
        let mut w = W::new(false);
        w.i32(0).i16(if g.below(4) == 0 { 27 } else { 0 });
        w.bytes(&g.bytes(60)).unwrap();
        out.push(("sync_group", w.buf));
        // Heartbeat and the other code-only bodies.
        let mut w = W::new(false);
        w.i32(0).i16(if g.below(2) == 0 { 22 } else { 0 });
        out.push(("code", w.buf));
        // InitProducerId.
        let mut w = W::new(false);
        w.i32(0)
            .i16(if g.below(4) == 0 { 47 } else { 0 })
            .i64(g.i64())
            .i16(g.i16());
        out.push(("init_producer", w.buf));
        // TxnOffsetCommit (flexible) and OffsetCommit (not).
        for flex in [true, false] {
            let mut w = W::new(flex);
            w.i32(0);
            let n = g.below(3);
            w.array(n as usize).unwrap();
            for _ in 0..n {
                wstr(&mut w, pick(g));
                let np = g.below(3);
                w.array(np as usize).unwrap();
                for _ in 0..np {
                    w.i32(g.i32()).i16(if g.below(4) == 0 { 51 } else { 0 });
                    w.tags();
                }
                w.tags();
            }
            w.tags();
            out.push((
                if flex {
                    "offset_errors_flex"
                } else {
                    "offset_errors"
                },
                w.buf,
            ));
        }
        // An assignment.
        let mut parts = BTreeMap::new();
        for _ in 0..g.below(4) {
            parts
                .entry(pick(g).to_string())
                .or_insert_with(Vec::new)
                .push(g.i32());
        }
        out.push(("parse_assignment", r::assignment(&parts).unwrap()));
        out
    }

    /// Every response decoder against the Rust one on valid and mutated
    /// bodies: the same values and the same first failure.
    #[test]
    fn response_decoders_match_the_rust_decoders() {
        let mut g = Rng::new(7);
        let mut checked = 0usize;
        for round in 0..300 {
            for (api, good) in bodies(&mut g) {
                for b in mutants(&mut g, &good, 40) {
                    let c = format!("round {round} {api} {b:?}");
                    checked += 1;
                    match api {
                        "api_versions" => same(&api_versions(&b), &r::api_versions(&b), &c),
                        "sasl_handshake" => same(&sasl_handshake(&b), &r::sasl_handshake(&b), &c),
                        "sasl_authenticate" => {
                            same(&sasl_authenticate(&b), &r::sasl_authenticate(&b), &c)
                        }
                        "metadata" => {
                            for topic in ["t", "topic-a", "héllo", "", "none"] {
                                assert_eq!(
                                    format!("{:?}", metadata(&b, topic)),
                                    format!("{:?}", r::metadata(&b, topic)),
                                    "{c} {topic}"
                                );
                            }
                        }
                        "create_topic" => {
                            same(&create_topic(&b, "t"), &r::create_topic(&b, "t"), &c)
                        }
                        "delete_records" => same(&delete_records(&b), &r::delete_records(&b), &c),
                        "add_partitions" => same(&add_partitions(&b), &r::add_partitions(&b), &c),
                        "list_offsets" => {
                            for p in 0..3 {
                                same(&list_offsets(&b, p), &r::list_offsets(&b, p), &c);
                            }
                        }
                        "fetch" => {
                            for p in 0..3 {
                                for committed in [false, true] {
                                    same(
                                        &fetch(&b, p, 1 << 20, committed),
                                        &r::fetch(&b, p, 1 << 20, committed),
                                        &c,
                                    );
                                }
                            }
                        }
                        "produce" => {
                            for p in 0..3 {
                                same(&produce(&b, "t", p), &r::produce(&b, "t", p), &c);
                            }
                        }
                        "find_coordinator" => {
                            same(&find_coordinator(&b), &r::find_coordinator(&b), &c)
                        }
                        "offset_fetch" => same(&offset_fetch(&b), &r::offset_fetch(&b), &c),
                        "join_group" => same(&join_group(&b), &r::join_group(&b), &c),
                        "sync_group" => same(&sync_group(&b), &r::sync_group(&b), &c),
                        "code" => same(&code(&b, "heartbeat"), &r::code(&b, "heartbeat"), &c),
                        "init_producer" => same(&init_producer(&b), &r::init_producer(&b), &c),
                        "offset_errors_flex" => same(
                            &offset_errors(&b, true, "txn offset commit"),
                            &r::offset_errors(&b, true, "txn offset commit"),
                            &c,
                        ),
                        "offset_errors" => same(
                            &offset_errors(&b, false, "offset commit"),
                            &r::offset_errors(&b, false, "offset commit"),
                            &c,
                        ),
                        "parse_assignment" => {
                            same(&parse_assignment(&b), &r::parse_assignment(&b), &c)
                        }
                        _ => unreachable!(),
                    }
                    // Any body is also a response with a header.
                    for flex in [false, true] {
                        let corr = i32::from_be_bytes(
                            b.get(..4).and_then(|s| s.try_into().ok()).unwrap_or([0; 4]),
                        );
                        for corr in [corr, 5] {
                            same(
                                &response_body(b.clone(), corr, flex),
                                &r::response_body(b.clone(), corr, flex),
                                &c,
                            );
                        }
                    }
                }
            }
        }
        assert!(checked > 100_000, "{checked}");
        // Successes, broker codes and protocol errors all come up (a
        // decoder never fails locally).
        assert_eq!(outcomes(), 3, "{:?}", OUTCOMES.with(|o| o.borrow().clone()));
    }

    /// Batch framing: `encode_batch` and `decode_batches` against the Rust
    /// ones, on valid batches in every codec and on mutated bytes.
    #[test]
    fn batch_framing_matches_the_rust_framing() {
        use super::super::wire::{ProducerStamp, encode_batch};
        use super::super::{KafkaCompression, KafkaRecord};
        let mut g = Rng::new(11);
        let mut checked = 0;
        for case in 0..200 {
            let records: Vec<KafkaRecord> = (0..1 + g.below(6))
                .map(|i| KafkaRecord {
                    key: (g.below(3) != 0).then(|| g.bytes(20)),
                    value: (g.below(4) != 0).then(|| g.bytes(400)),
                    headers: (0..g.below(3))
                        .map(|h| (format!("h{h}"), g.bytes(10)))
                        .collect(),
                    timestamp: g.i64() / 2 + i as i64,
                })
                .collect();
            let codec = [
                KafkaCompression::None,
                KafkaCompression::Gzip,
                KafkaCompression::Snappy,
                KafkaCompression::Lz4,
                KafkaCompression::Zstd,
            ][case % 5];
            let stamp = ProducerStamp {
                id: g.i64(),
                epoch: g.i16(),
                sequence: g.i32(),
                transactional: g.below(2) == 0,
            };
            let port = encode_batch(&records, codec, stamp);
            same(&port, &r::encode_batch(&records, codec, stamp), "encode");
            let mut bytes = port.unwrap();
            if g.below(3) == 0 {
                let again = bytes.clone();
                bytes.extend_from_slice(&again);
            }
            // Checksum-valid shapes the random mutants rarely reach: a body
            // cut short of the 49-byte header, and negative last offset
            // deltas or record counts.
            let mut crafted = Vec::new();
            for n in 0..60u32 {
                let mut b = bytes[..(12 + n as usize).min(bytes.len())].to_vec();
                b[8..12].copy_from_slice(&n.to_be_bytes());
                crafted.push(recrc(b));
            }
            for at in [23, 57] {
                let mut b = bytes.clone();
                b[at..at + 4].copy_from_slice(&(-1 - g.below(3) as i32).to_be_bytes());
                crafted.push(recrc(b));
            }
            let random = mutants(&mut g, &bytes, 60);
            crafted.extend(random.iter().map(|b| recrc(b.clone())));
            for b in random.into_iter().chain(crafted) {
                checked += 1;
                for max in [1 << 20, 64] {
                    same(
                        &decode_batches(&b, max),
                        &r::decode_batches(&b, max),
                        &format!("case {case} {b:?}"),
                    );
                }
            }
        }
        same(
            &encode_batch(&[], KafkaCompression::None, ProducerStamp::NONE),
            &r::encode_batch(&[], KafkaCompression::None, ProducerStamp::NONE),
            "empty",
        );
        assert!(checked > 10_000, "{checked}");
    }

    /// Refreshes the first batch's CRC-32C so a mutant gets past the
    /// checksum and into the header checks.
    fn recrc(mut b: Vec<u8>) -> Vec<u8> {
        if b.len() >= 21 {
            let len = u32::from_be_bytes([b[8], b[9], b[10], b[11]]) as usize;
            let end = (12 + len).min(b.len()).max(21);
            let crc = crc32c::crc32c(&b[21..end]);
            b[17..21].copy_from_slice(&crc.to_be_bytes());
        }
        b
    }

    /// The consumer protocol codecs against the Rust ones.
    #[test]
    fn consumer_protocol_matches_the_rust_codecs() {
        let mut g = Rng::new(3);
        for case in 0..3000 {
            let topics: Vec<KafkaBinding> = (0..g.below(9))
                .map(|_| KafkaBinding {
                    topic: ["b", "a", "c", "ab", "", "é"][g.below(6) as usize].to_string(),
                    partition: [0, 1, 2, -1, i32::MAX][g.below(5) as usize],
                })
                .collect();
            let c = format!("case {case} {topics:?}");
            let sub = subscription(&topics);
            same(&sub, &r::subscription(&topics), &c);
            let mut parts = BTreeMap::new();
            for t in &topics {
                parts
                    .entry(t.topic.clone())
                    .or_insert_with(Vec::new)
                    .push(t.partition);
            }
            let asg = assignment(&parts);
            same(&asg, &r::assignment(&parts), &c);
            let asg = asg.unwrap();
            same(&parse_assignment(&asg), &r::parse_assignment(&asg), &c);
        }
    }
}
