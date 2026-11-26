//! The Rust Kafka wire codecs the Vesper port replaced (#1401): the
//! request writer `W`, the response reader `R`, and every request and
//! response of `Conn`, `Client` and the consumer protocol as they were,
//! turned into functions of the bytes. The reference of the differential
//! tests in `kafka_codec.rs` and of the `kafka_wire` ABBA workload.
#![allow(dead_code)]
use super::super::client::{Committed, Fetched, Joined};
use super::super::kafka::KafkaBinding;
use super::super::wire::{
    Batch, Fail, KResult, KafkaCompression, KafkaRecord, MALFORMED, ProducerStamp,
    committed_records, compress, decompress,
};
use super::{Head, Metadata, address};
use crate::Error;
use std::collections::{BTreeMap, BTreeSet};

fn check_code(code: i16, what: &str) -> KResult<()> {
    if code == 0 {
        Ok(())
    } else {
        Err(Fail::Code(code, what.to_string()))
    }
}

// ----------------------------------------------------------------------
// Encoding
// ----------------------------------------------------------------------

/// A request body writer. `flex` selects the compact (KIP-482) encodings.
pub(crate) struct W {
    pub(crate) buf: Vec<u8>,
    flex: bool,
}
impl W {
    pub(crate) fn new(flex: bool) -> Self {
        Self {
            buf: Vec::new(),
            flex,
        }
    }
    pub(crate) fn i8(&mut self, v: i8) -> &mut Self {
        self.buf.push(v as u8);
        self
    }
    pub(crate) fn bool(&mut self, v: bool) -> &mut Self {
        self.buf.push(u8::from(v));
        self
    }
    pub(crate) fn i16(&mut self, v: i16) -> &mut Self {
        self.buf.extend_from_slice(&v.to_be_bytes());
        self
    }
    pub(crate) fn i32(&mut self, v: i32) -> &mut Self {
        self.buf.extend_from_slice(&v.to_be_bytes());
        self
    }
    pub(crate) fn i64(&mut self, v: i64) -> &mut Self {
        self.buf.extend_from_slice(&v.to_be_bytes());
        self
    }
    pub(crate) fn uvarint(&mut self, mut v: u64) -> &mut Self {
        loop {
            let b = (v & 0x7f) as u8;
            v >>= 7;
            if v == 0 {
                self.buf.push(b);
                return self;
            }
            self.buf.push(b | 0x80);
        }
    }
    pub(crate) fn varint(&mut self, v: i64) -> &mut Self {
        self.uvarint(((v << 1) ^ (v >> 63)) as u64)
    }
    pub(crate) fn len(&mut self, n: usize) -> KResult<&mut Self> {
        if self.flex {
            Ok(self.uvarint(n as u64 + 1))
        } else {
            Ok(self.i32(i32::try_from(n).map_err(|_| Fail::Local(Error::Overflow))?))
        }
    }
    pub(crate) fn str(&mut self, s: &str) -> KResult<&mut Self> {
        if self.flex {
            self.uvarint(s.len() as u64 + 1);
        } else {
            self.i16(i16::try_from(s.len()).map_err(|_| Fail::Local(Error::Overflow))?);
        }
        self.buf.extend_from_slice(s.as_bytes());
        Ok(self)
    }
    pub(crate) fn nstr(&mut self, s: Option<&str>) -> KResult<&mut Self> {
        match s {
            Some(s) => self.str(s),
            None if self.flex => Ok(self.uvarint(0)),
            None => Ok(self.i16(-1)),
        }
    }
    pub(crate) fn bytes(&mut self, b: &[u8]) -> KResult<&mut Self> {
        self.len(b.len())?;
        self.buf.extend_from_slice(b);
        Ok(self)
    }
    pub(crate) fn array(&mut self, n: usize) -> KResult<&mut Self> {
        self.len(n)
    }
    /// Empty tagged-field section (flexible versions only).
    pub(crate) fn tags(&mut self) -> &mut Self {
        if self.flex {
            self.uvarint(0);
        }
        self
    }
}

/// A bounds-checked response reader.
pub(crate) struct R<'a> {
    b: &'a [u8],
    pub(crate) at: usize,
    flex: bool,
}
impl<'a> R<'a> {
    pub(crate) fn new(b: &'a [u8], flex: bool) -> Self {
        Self { b, at: 0, flex }
    }
    pub(crate) fn rest(&self) -> usize {
        self.b.len() - self.at
    }
    pub(crate) fn take(&mut self, n: usize) -> KResult<&'a [u8]> {
        let end = self.at.checked_add(n).ok_or(MALFORMED)?;
        let s = self.b.get(self.at..end).ok_or(MALFORMED)?;
        self.at = end;
        Ok(s)
    }
    pub(crate) fn i8(&mut self) -> KResult<i8> {
        Ok(self.take(1)?[0] as i8)
    }
    pub(crate) fn bool(&mut self) -> KResult<bool> {
        Ok(self.take(1)?[0] != 0)
    }
    pub(crate) fn i16(&mut self) -> KResult<i16> {
        Ok(i16::from_be_bytes(
            self.take(2)?.try_into().map_err(|_| MALFORMED)?,
        ))
    }
    pub(crate) fn i32(&mut self) -> KResult<i32> {
        Ok(i32::from_be_bytes(
            self.take(4)?.try_into().map_err(|_| MALFORMED)?,
        ))
    }
    pub(crate) fn u32(&mut self) -> KResult<u32> {
        Ok(u32::from_be_bytes(
            self.take(4)?.try_into().map_err(|_| MALFORMED)?,
        ))
    }
    pub(crate) fn i64(&mut self) -> KResult<i64> {
        Ok(i64::from_be_bytes(
            self.take(8)?.try_into().map_err(|_| MALFORMED)?,
        ))
    }
    pub(crate) fn uvarint(&mut self) -> KResult<u64> {
        let mut v = 0u64;
        for i in 0..10 {
            let b = self.take(1)?[0];
            v |= u64::from(b & 0x7f) << (7 * i);
            if b & 0x80 == 0 {
                return Ok(v);
            }
        }
        Err(MALFORMED)
    }
    pub(crate) fn varint(&mut self) -> KResult<i64> {
        let v = self.uvarint()?;
        Ok(((v >> 1) as i64) ^ -((v & 1) as i64))
    }
    /// A length prefix: `None` for null. Bounded by the bytes left (each
    /// element takes at least one byte), so a hostile count cannot allocate.
    pub(crate) fn len(&mut self) -> KResult<Option<usize>> {
        let n = if self.flex {
            match self.uvarint()? {
                0 => return Ok(None),
                n => n - 1,
            }
        } else {
            match self.i32()? {
                -1 => return Ok(None),
                n if n < 0 => return Err(MALFORMED),
                n => n as u64,
            }
        };
        let n = usize::try_from(n).map_err(|_| MALFORMED)?;
        if n > self.rest() {
            return Err(MALFORMED);
        }
        Ok(Some(n))
    }
    pub(crate) fn array(&mut self) -> KResult<usize> {
        Ok(self.len()?.unwrap_or(0))
    }
    pub(crate) fn nstr(&mut self) -> KResult<Option<String>> {
        let n = if self.flex {
            match self.uvarint()? {
                0 => return Ok(None),
                n => usize::try_from(n - 1).map_err(|_| MALFORMED)?,
            }
        } else {
            match self.i16()? {
                -1 => return Ok(None),
                n if n < 0 => return Err(MALFORMED),
                n => n as usize,
            }
        };
        let s = self.take(n)?;
        Ok(Some(
            std::str::from_utf8(s).map_err(|_| MALFORMED)?.to_string(),
        ))
    }
    pub(crate) fn str(&mut self) -> KResult<String> {
        self.nstr()?.ok_or(MALFORMED)
    }
    pub(crate) fn nbytes(&mut self) -> KResult<Option<&'a [u8]>> {
        match self.len()? {
            None => Ok(None),
            Some(n) => self.take(n).map(Some),
        }
    }
    pub(crate) fn bytes(&mut self) -> KResult<&'a [u8]> {
        Ok(self.nbytes()?.unwrap_or(&[]))
    }
    pub(crate) fn tags(&mut self) -> KResult<()> {
        if !self.flex {
            return Ok(());
        }
        let n = self.uvarint()?;
        for _ in 0..n {
            self.uvarint()?;
            let size = usize::try_from(self.uvarint()?).map_err(|_| MALFORMED)?;
            self.take(size)?;
        }
        Ok(())
    }
}

// ----------------------------------------------------------------------
// Framing (`Conn::frame`, `Conn::recv`)
// ----------------------------------------------------------------------

fn frame(
    key: i16,
    h: &Head,
    flex: bool,
    body: impl FnOnce(&mut W) -> KResult<()>,
) -> KResult<Vec<u8>> {
    let mut header = W::new(false);
    header.i16(key).i16(h.version).i32(h.corr);
    header.str(h.cid)?;
    if flex {
        header.uvarint(0);
    }
    let mut w = W::new(flex);
    body(&mut w)?;
    let len = header.buf.len() + w.buf.len();
    let mut frame = Vec::with_capacity(4 + len);
    frame.extend_from_slice(
        &i32::try_from(len)
            .map_err(|_| Fail::Local(Error::Overflow))?
            .to_be_bytes(),
    );
    frame.extend_from_slice(&header.buf);
    frame.extend_from_slice(&w.buf);
    Ok(frame)
}

pub(crate) fn response_body(mut body: Vec<u8>, corr: i32, flex_header: bool) -> KResult<Vec<u8>> {
    let mut r = R::new(&body, flex_header);
    if r.i32()? != corr {
        return Err(Fail::Proto("correlation id"));
    }
    r.tags()?;
    let at = r.at;
    body.drain(..at);
    Ok(body)
}

// ----------------------------------------------------------------------
// Requests
// ----------------------------------------------------------------------

pub(crate) fn api_versions_request(h: &Head) -> KResult<Vec<u8>> {
    frame(18, h, false, |_| Ok(()))
}

pub(crate) fn sasl_handshake_request(h: &Head, mechanism: &str) -> KResult<Vec<u8>> {
    frame(17, h, false, |w| {
        w.str(mechanism)?;
        Ok(())
    })
}

pub(crate) fn sasl_authenticate_request(h: &Head, message: &[u8]) -> KResult<Vec<u8>> {
    frame(36, h, false, |w| {
        w.bytes(message)?;
        Ok(())
    })
}

pub(crate) fn metadata_request(h: &Head, topic: &str) -> KResult<Vec<u8>> {
    frame(3, h, false, |w| {
        w.array(1)?.str(topic)?;
        w.bool(false).bool(false).bool(false);
        Ok(())
    })
}

pub(crate) fn create_topic_request(h: &Head, topic: &str, partitions: i32) -> KResult<Vec<u8>> {
    frame(19, h, false, |w| {
        w.array(1)?.str(topic)?;
        w.i32(partitions).i16(1);
        w.array(0)?;
        w.array(0)?;
        w.i32(30_000).bool(false);
        Ok(())
    })
}

pub(crate) fn delete_records_request(
    h: &Head,
    topic: &str,
    partition: i32,
    offset: i64,
) -> KResult<Vec<u8>> {
    frame(21, h, false, |w| {
        w.array(1)?.str(topic)?;
        w.array(1)?.i32(partition).i64(offset);
        w.i32(30_000);
        Ok(())
    })
}

pub(crate) fn list_offsets_request(
    h: &Head,
    topic: &str,
    partition: i32,
    ts: i64,
    committed: bool,
) -> KResult<Vec<u8>> {
    frame(2, h, false, |w| {
        w.i32(-1).i8(i8::from(committed));
        w.array(1)?.str(topic)?;
        w.array(1)?.i32(partition).i32(-1).i64(ts);
        Ok(())
    })
}

pub(crate) fn fetch_request(
    h: &Head,
    topic: &str,
    partition: i32,
    offset: i64,
    max_bytes: i32,
    committed: bool,
) -> KResult<Vec<u8>> {
    frame(1, h, false, |w| {
        w.i32(-1)
            .i32(50)
            .i32(1)
            .i32(max_bytes)
            .i8(i8::from(committed));
        w.i32(0).i32(-1);
        w.array(1)?.str(topic)?;
        w.array(1)?
            .i32(partition)
            .i32(-1)
            .i64(offset)
            .i64(-1)
            .i32(max_bytes);
        w.array(0)?;
        w.str("")?;
        Ok(())
    })
}

pub(crate) fn produce_request(
    h: &Head,
    txn: Option<&str>,
    topic: &str,
    partition: i32,
    batch: &[u8],
) -> KResult<Vec<u8>> {
    frame(0, h, false, |w| {
        w.nstr(txn)?;
        w.i16(-1).i32(30_000);
        w.array(1)?.str(topic)?;
        w.array(1)?.i32(partition).bytes(batch)?;
        Ok(())
    })
}

pub(crate) fn find_coordinator_request(h: &Head, key: &str, kind: i8) -> KResult<Vec<u8>> {
    frame(10, h, false, |w| {
        w.str(key)?.i8(kind);
        Ok(())
    })
}

pub(crate) fn offset_fetch_request(
    h: &Head,
    group: &str,
    topic: &str,
    partitions: &[i32],
) -> KResult<Vec<u8>> {
    frame(9, h, true, |w| {
        w.str(group)?;
        w.array(1)?.str(topic)?;
        w.array(partitions.len())?;
        for p in partitions {
            w.i32(*p);
        }
        w.tags();
        w.bool(true).tags();
        Ok(())
    })
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn join_group_request(
    h: &Head,
    group: &str,
    st: i32,
    rt: i32,
    member_id: &str,
    instance_id: Option<&str>,
    protocol: &str,
    metadata: &[u8],
) -> KResult<Vec<u8>> {
    frame(11, h, false, |w| {
        w.str(group)?.i32(st).i32(rt).str(member_id)?;
        w.nstr(instance_id)?;
        w.str("consumer")?;
        w.array(1)?.str(protocol)?.bytes(metadata)?;
        Ok(())
    })
}

pub(crate) fn sync_group_request(
    h: &Head,
    group: &str,
    generation: i32,
    member_id: &str,
    instance_id: Option<&str>,
    assignments: &[(String, Vec<u8>)],
) -> KResult<Vec<u8>> {
    frame(14, h, false, |w| {
        w.str(group)?.i32(generation).str(member_id)?;
        w.nstr(instance_id)?;
        w.array(assignments.len())?;
        for (m, a) in assignments {
            w.str(m)?.bytes(a)?;
        }
        Ok(())
    })
}

pub(crate) fn heartbeat_request(
    h: &Head,
    group: &str,
    generation: i32,
    member_id: &str,
    instance_id: Option<&str>,
) -> KResult<Vec<u8>> {
    frame(12, h, false, |w| {
        w.str(group)?.i32(generation).str(member_id)?;
        w.nstr(instance_id)?;
        Ok(())
    })
}

pub(crate) fn leave_group_request(
    h: &Head,
    group: &str,
    member_id: &str,
    instance_id: Option<&str>,
) -> KResult<Vec<u8>> {
    frame(13, h, false, |w| {
        w.str(group)?;
        w.array(1)?.str(member_id)?;
        w.nstr(instance_id)?;
        Ok(())
    })
}

pub(crate) fn init_producer_request(h: &Head, txn: Option<&str>, ms: i32) -> KResult<Vec<u8>> {
    frame(22, h, false, |w| {
        w.nstr(txn)?.i32(ms);
        Ok(())
    })
}

pub(crate) fn add_partitions_request(
    h: &Head,
    txn: &str,
    pid: (i64, i16),
    topic: &str,
    partition: i32,
) -> KResult<Vec<u8>> {
    frame(24, h, false, |w| {
        w.str(txn)?.i64(pid.0).i16(pid.1);
        w.array(1)?.str(topic)?;
        w.array(1)?.i32(partition);
        Ok(())
    })
}

pub(crate) fn add_offsets_request(
    h: &Head,
    txn: &str,
    pid: (i64, i16),
    group: &str,
) -> KResult<Vec<u8>> {
    frame(25, h, false, |w| {
        w.str(txn)?.i64(pid.0).i16(pid.1).str(group)?;
        Ok(())
    })
}

fn write_offsets(w: &mut W, offsets: &[(String, i32, i64, String)], flex: bool) -> KResult<()> {
    let mut by_topic: BTreeMap<&str, Vec<(i32, i64, &str)>> = BTreeMap::new();
    for (t, p, o, m) in offsets {
        by_topic.entry(t).or_default().push((*p, *o, m));
    }
    w.array(by_topic.len())?;
    for (t, parts) in by_topic {
        w.str(t)?;
        w.array(parts.len())?;
        for (p, o, m) in parts {
            w.i32(p).i64(o).i32(-1).nstr(Some(m))?;
            if flex {
                w.tags();
            }
        }
        if flex {
            w.tags();
        }
    }
    Ok(())
}

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
    frame(28, h, true, |w| {
        w.str(txn)?.str(group)?.i64(pid.0).i16(pid.1);
        w.i32(generation).str(member_id)?;
        w.nstr(instance_id)?;
        write_offsets(w, offsets, true)?;
        w.tags();
        Ok(())
    })
}

pub(crate) fn end_txn_request(
    h: &Head,
    txn: &str,
    pid: (i64, i16),
    commit: bool,
) -> KResult<Vec<u8>> {
    frame(26, h, false, |w| {
        w.str(txn)?.i64(pid.0).i16(pid.1).bool(commit);
        Ok(())
    })
}

// ----------------------------------------------------------------------
// Responses
// ----------------------------------------------------------------------

pub(crate) fn api_versions(body: &[u8]) -> KResult<Vec<(i16, i16, i16)>> {
    let mut r = R::new(body, false);
    let code = r.i16()?;
    check_code(code, "api versions")?;
    let n = r.array()?;
    let mut out = Vec::new();
    for _ in 0..n {
        let key = r.i16()?;
        let min = r.i16()?;
        let max = r.i16()?;
        out.push((key, min, max));
    }
    Ok(out)
}

pub(crate) fn sasl_handshake(body: &[u8]) -> KResult<()> {
    let mut r = R::new(body, false);
    let code = r.i16()?;
    check_code(code, "sasl handshake")
}

pub(crate) fn sasl_authenticate(body: &[u8]) -> KResult<Vec<u8>> {
    let mut r = R::new(body, false);
    let code = r.i16()?;
    let msg = r.nstr()?;
    if code != 0 {
        return Err(Fail::Code(
            code,
            format!("sasl authenticate: {}", msg.unwrap_or_default()),
        ));
    }
    let received = r.bytes()?.to_vec();
    r.i64()?;
    Ok(received)
}

pub(crate) fn metadata(body: &[u8], topic: &str) -> Metadata {
    let mut brokers = Vec::new();
    let mut leaders = Vec::new();
    let status = (|| {
        let mut r = R::new(body, false);
        r.i32()?;
        let nb = r.array()?;
        for _ in 0..nb {
            let id = r.i32()?;
            let host = r.str()?;
            let port = r.i32()?;
            r.nstr()?;
            brokers.push((id, address(&host, port)));
        }
        r.nstr()?;
        r.i32()?;
        let nt = r.array()?;
        for _ in 0..nt {
            let code = r.i16()?;
            let name = r.str()?;
            r.bool()?;
            let np = r.array()?;
            let mut parts = Vec::new();
            for _ in 0..np {
                let pcode = r.i16()?;
                let index = r.i32()?;
                let leader = r.i32()?;
                r.i32()?;
                for _ in 0..3 {
                    let n = r.array()?;
                    r.take(n.checked_mul(4).ok_or(Fail::Proto("metadata"))?)?;
                }
                parts.push((pcode, index, leader));
            }
            r.i32()?;
            if name == topic {
                check_code(code, "metadata")?;
                for (pcode, index, leader) in parts {
                    if pcode == 0 && leader >= 0 {
                        leaders.push((index, leader));
                    }
                }
            }
        }
        Ok(())
    })();
    if status.is_err() {
        brokers.clear();
    }
    Metadata {
        brokers,
        leaders,
        status,
    }
}

pub(crate) fn create_topic(body: &[u8], topic: &str) -> KResult<()> {
    let mut r = R::new(body, false);
    r.i32()?;
    let n = r.array()?;
    for _ in 0..n {
        r.str()?;
        let code = r.i16()?;
        let msg = r.nstr()?;
        if code != 0 {
            return Err(Fail::Code(
                code,
                format!("create topic {topic}: {}", msg.unwrap_or_default()),
            ));
        }
    }
    Ok(())
}

pub(crate) fn delete_records(body: &[u8]) -> KResult<()> {
    let mut r = R::new(body, false);
    r.i32()?;
    let n = r.array()?;
    for _ in 0..n {
        r.str()?;
        let np = r.array()?;
        for _ in 0..np {
            r.i32()?;
            r.i64()?;
            check_code(r.i16()?, "delete records")?;
        }
    }
    Ok(())
}

pub(crate) fn list_offsets(body: &[u8], partition: i32) -> KResult<i64> {
    let mut r = R::new(body, false);
    r.i32()?;
    let mut out = None;
    for _ in 0..r.array()? {
        r.str()?;
        for _ in 0..r.array()? {
            let p = r.i32()?;
            let code = r.i16()?;
            r.i64()?;
            let offset = r.i64()?;
            r.i32()?;
            check_code(code, "list offsets")?;
            if p == partition {
                out = Some(offset);
            }
        }
    }
    out.ok_or(Fail::Proto("list offsets"))
}

pub(crate) fn fetch(
    body: &[u8],
    partition: i32,
    max_decoded: usize,
    committed: bool,
) -> KResult<Fetched> {
    let mut r = R::new(body, false);
    r.i32()?;
    check_code(r.i16()?, "fetch")?;
    r.i32()?;
    let mut out = None;
    for _ in 0..r.array()? {
        r.str()?;
        for _ in 0..r.array()? {
            let p = r.i32()?;
            let code = r.i16()?;
            let high_watermark = r.i64()?;
            let last_stable = r.i64()?;
            let log_start = r.i64()?;
            let na = r.array()?;
            let mut aborted = Vec::with_capacity(na.min(1024));
            for _ in 0..na {
                aborted.push((r.i64()?, r.i64()?));
            }
            r.i32()?;
            let records = r.nbytes()?.unwrap_or(&[]);
            check_code(code, "fetch")?;
            if p != partition {
                continue;
            }
            let batches: Vec<Batch> = decode_batches(records, max_decoded)?;
            let (records, end) = committed_records(batches, &aborted, committed);
            out = Some(Fetched {
                high_watermark,
                last_stable,
                log_start,
                records,
                end,
            });
        }
    }
    out.ok_or(Fail::Proto("fetch response"))
}

pub(crate) fn produce(body: &[u8], topic: &str, partition: i32) -> KResult<i64> {
    let mut r = R::new(body, false);
    let mut out = None;
    for _ in 0..r.array()? {
        r.str()?;
        for _ in 0..r.array()? {
            let p = r.i32()?;
            let code = r.i16()?;
            let base = r.i64()?;
            r.i64()?;
            r.i64()?;
            for _ in 0..r.array()? {
                r.i32()?;
                r.nstr()?;
            }
            let msg = r.nstr()?;
            if p != partition {
                continue;
            }
            out = Some(match code {
                0 => Ok(base),
                46 => Ok(-1),
                c => Err(Fail::Code(
                    c,
                    format!("produce {topic}/{p}: {}", msg.unwrap_or_default()),
                )),
            });
        }
    }
    r.i32()?;
    out.ok_or(Fail::Proto("produce response"))?
}

pub(crate) fn find_coordinator(body: &[u8]) -> KResult<(i32, String)> {
    let mut r = R::new(body, false);
    r.i32()?;
    let code = r.i16()?;
    let msg = r.nstr()?;
    let id = r.i32()?;
    let host = r.str()?;
    let port = r.i32()?;
    if code != 0 {
        return Err(Fail::Code(
            code,
            format!("find coordinator: {}", msg.unwrap_or_default()),
        ));
    }
    Ok((id, address(&host, port)))
}

pub(crate) fn offset_fetch(body: &[u8]) -> KResult<BTreeMap<i32, Committed>> {
    let mut r = R::new(body, true);
    r.i32()?;
    let mut out = BTreeMap::new();
    let mut first_err = 0;
    for _ in 0..r.array()? {
        r.str()?;
        for _ in 0..r.array()? {
            let p = r.i32()?;
            let offset = r.i64()?;
            r.i32()?;
            let metadata = r.nstr()?.unwrap_or_default();
            let code = r.i16()?;
            r.tags()?;
            if code != 0 && first_err == 0 {
                first_err = code;
            }
            if offset >= 0 {
                out.insert(p, Committed { offset, metadata });
            }
        }
        r.tags()?;
    }
    let code = r.i16()?;
    check_code(code, "offset fetch")?;
    check_code(first_err, "offset fetch")?;
    Ok(out)
}

pub(crate) fn join_group(body: &[u8]) -> KResult<Joined> {
    let mut r = R::new(body, false);
    r.i32()?;
    let code = r.i16()?;
    let generation = r.i32()?;
    let protocol_name = r.str()?;
    let leader = r.str()?;
    let me = r.str()?;
    let mut members = Vec::new();
    for _ in 0..r.array()? {
        let id = r.str()?;
        r.nstr()?;
        members.push((id, r.bytes()?.to_vec()));
    }
    if code == 79 {
        return Ok(Joined {
            generation: -1,
            member_id: me,
            leader: String::new(),
            protocol: protocol_name,
            members,
            retry: true,
        });
    }
    check_code(code, "join group")?;
    Ok(Joined {
        generation,
        member_id: me,
        leader,
        protocol: protocol_name,
        members,
        retry: false,
    })
}

pub(crate) fn sync_group(body: &[u8]) -> KResult<Vec<u8>> {
    let mut r = R::new(body, false);
    r.i32()?;
    check_code(r.i16()?, "sync group")?;
    Ok(r.bytes()?.to_vec())
}

pub(crate) fn code(body: &[u8], what: &str) -> KResult<()> {
    let mut r = R::new(body, false);
    r.i32()?;
    check_code(r.i16()?, what)
}

pub(crate) fn init_producer(body: &[u8]) -> KResult<(i64, i16)> {
    let mut r = R::new(body, false);
    r.i32()?;
    let code = r.i16()?;
    let id = r.i64()?;
    let epoch = r.i16()?;
    check_code(code, "init producer id")?;
    Ok((id, epoch))
}

pub(crate) fn add_partitions(body: &[u8]) -> KResult<()> {
    let mut r = R::new(body, false);
    r.i32()?;
    for _ in 0..r.array()? {
        r.str()?;
        for _ in 0..r.array()? {
            r.i32()?;
            check_code(r.i16()?, "add partitions to txn")?;
        }
    }
    Ok(())
}

pub(crate) fn offset_errors(body: &[u8], flex: bool, what: &str) -> KResult<()> {
    let mut r = R::new(body, flex);
    r.i32()?;
    for _ in 0..r.array()? {
        r.str()?;
        for _ in 0..r.array()? {
            r.i32()?;
            check_code(r.i16()?, what)?;
            r.tags()?;
        }
        r.tags()?;
    }
    r.tags()
}

// ----------------------------------------------------------------------
// Consumer protocol (src/kafka_group.rs)
// ----------------------------------------------------------------------

pub(crate) fn subscription(topics: &[KafkaBinding]) -> KResult<Vec<u8>> {
    let names: BTreeSet<&str> = topics.iter().map(|t| t.topic.as_str()).collect();
    let bindings: BTreeSet<(&str, i32)> = topics
        .iter()
        .map(|t| (t.topic.as_str(), t.partition))
        .collect();
    let mut user = W::new(false);
    user.array(bindings.len())?;
    for (t, p) in bindings {
        user.str(t)?;
        user.i32(p);
    }
    let mut w = W::new(false);
    w.i16(0);
    w.array(names.len())?;
    for n in names {
        w.str(n)?;
    }
    w.bytes(&user.buf)?;
    Ok(w.buf)
}

pub(crate) fn assignment(parts: &BTreeMap<String, Vec<i32>>) -> KResult<Vec<u8>> {
    let mut w = W::new(false);
    w.i16(0);
    w.array(parts.len())?;
    for (t, ps) in parts {
        w.str(t)?;
        w.array(ps.len())?;
        for p in ps {
            w.i32(*p);
        }
    }
    w.bytes(&[])?;
    Ok(w.buf)
}

pub(crate) fn parse_assignment(b: &[u8]) -> KResult<Vec<(String, i32)>> {
    if b.is_empty() {
        return Ok(Vec::new());
    }
    let mut r = R::new(b, false);
    r.i16()?;
    let mut out = Vec::new();
    for _ in 0..r.array()? {
        let t = r.str()?;
        for _ in 0..r.array()? {
            out.push((t.clone(), r.i32()?));
        }
    }
    Ok(out)
}

// ----------------------------------------------------------------------
// Record batches (`encode_batch`, `decode_batches`)
// ----------------------------------------------------------------------

pub(crate) fn encode_batch(
    records: &[KafkaRecord],
    compression: KafkaCompression,
    stamp: ProducerStamp,
) -> KResult<Vec<u8>> {
    if records.is_empty() {
        return Err(Fail::Local(Error::Kafka("empty batch")));
    }
    let count = i32::try_from(records.len()).map_err(|_| Fail::Local(Error::Overflow))?;
    let base_ts = records.iter().map(|r| r.timestamp).min().unwrap_or(0);
    let max_ts = records.iter().map(|r| r.timestamp).max().unwrap_or(0);
    let raw = super::super::wire::encode_records(records, base_ts)?;
    let payload = compress(compression, &raw)?;
    let mut tail = W::new(false);
    let mut attributes = compression.code();
    if stamp.transactional {
        attributes |= 1 << 4;
    }
    tail.i16(attributes)
        .i32(count - 1)
        .i64(base_ts)
        .i64(max_ts)
        .i64(stamp.id)
        .i16(stamp.epoch)
        .i32(stamp.sequence)
        .i32(count);
    tail.buf.extend_from_slice(&payload);
    let crc = crc32c::crc32c(&tail.buf);
    let mut batch = W::new(false);
    // batchLength counts from partitionLeaderEpoch to the end.
    let length = 4 + 1 + 4 + tail.buf.len();
    batch
        .i64(0)
        .i32(i32::try_from(length).map_err(|_| Fail::Local(Error::Overflow))?)
        .i32(-1)
        .i8(2);
    batch.buf.extend_from_slice(&crc.to_be_bytes());
    batch.buf.extend_from_slice(&tail.buf);
    Ok(batch.buf)
}

pub(crate) fn decode_batches(bytes: &[u8], max_decoded: usize) -> KResult<Vec<Batch>> {
    let mut out = Vec::new();
    let mut r = R::new(bytes, false);
    while r.rest() >= 12 {
        let base_offset = r.i64()?;
        let length = r.i32()?;
        let length = usize::try_from(length).map_err(|_| Fail::Proto("batch length"))?;
        if length > r.rest() {
            // Partial trailing batch.
            break;
        }
        let body = r.take(length)?;
        let mut b = R::new(body, false);
        let _leader_epoch = b.i32()?;
        let magic = b.i8()?;
        if magic != 2 {
            return Err(Fail::Proto("legacy message format"));
        }
        let crc = b.u32()?;
        if crc32c::crc32c(&body[b.at..]) != crc {
            return Err(Fail::Proto("batch checksum"));
        }
        let attributes = b.i16()?;
        let last_delta = b.i32()?;
        let base_ts = b.i64()?;
        let _max_ts = b.i64()?;
        let producer_id = b.i64()?;
        let _epoch = b.i16()?;
        let _seq = b.i32()?;
        let count = b.i32()?;
        if count < 0 || last_delta < 0 {
            return Err(Fail::Proto("batch header"));
        }
        let transactional = attributes & (1 << 4) != 0;
        let is_control = attributes & (1 << 5) != 0;
        let codec = attributes & 0x7;
        let payload = &body[b.at..];
        let raw = decompress(codec, payload, max_decoded)?;
        let (control, records) =
            super::super::wire::decode_records(&raw, count, base_offset, base_ts, is_control)?;
        out.push(Batch {
            base_offset,
            last_offset: base_offset
                .checked_add(i64::from(last_delta))
                .ok_or(Fail::Proto("batch offset"))?,
            producer_id,
            transactional,
            control,
            records,
        });
    }
    Ok(out)
}
