//! `vesper replay <bundle.vchrono>`: authenticate, inspect and replay a
//! Vesper Chrono flight-log bundle (§31; integration B, issue #136).
//!
//! The bundle is opened with its keyed-BLAKE3 key (`Bundle::open` checks the
//! MAC before parsing and then validates the structure). Replay folds every
//! retained record through a transcript replayer (a BLAKE3 chain over the
//! records, in order) from the initial state, which checks that the recording
//! is complete and replayable; a domain replayer that reconstructs program
//! state is a library `Replayer`, not part of this command. Queries: the
//! timeline, a reverse watch on an `inout` place (`--watch`), and an
//! obligation's lineage (`--lineage`), plus the lineage audit.
use std::{fs, path::PathBuf};
use vesper_chrono::{
    BundleKey, BundleLimits, FlightEvent, HopKind, ReplaySession, Replayer, TraceCursor,
};

const HELP: &str = "Usage: vesper replay <bundle.vchrono> (--key <hex> | --key-file <path>) [options]\n\nAuthenticate, inspect and replay a Chrono flight-log bundle.\n\nOptions:\n  --key <hex>          32-byte bundle key as 64 hex digits\n  --key-file <path>    File holding the key (32 raw bytes or 64 hex digits)\n  --timeline           List every retained record\n  --watch <place>      Last write to this inout place hash before the cursor\n  --before <seq>       Cursor for --watch (default: end of the recording)\n  --lineage <id>       Obligation lineage (hops and ancestors)\n";

/// Folds records into a BLAKE3 chain. It models no effect, so digest-only
/// and redacted records are hashed as stored rather than refused.
struct Transcript;

impl Replayer for Transcript {
    type State = (u64, [u8; 32]);
    fn initial(&self) -> Self::State {
        (0, [0; 32])
    }
    fn restore(&self, _image: &[u8]) -> Result<Self::State, String> {
        Err("the transcript replayer cannot restore program snapshots".into())
    }
    fn models(&self, _effect_id: u16) -> bool {
        false
    }
    fn apply(&self, state: &mut Self::State, e: &FlightEvent<'_>) -> Result<(), String> {
        let mut h = blake3::Hasher::new();
        h.update(&state.1);
        h.update(&e.lamport_seq.to_le_bytes());
        h.update(&e.fiber_id.to_le_bytes());
        h.update(&e.time_ns.to_le_bytes());
        h.update(&e.effect_id.to_le_bytes());
        h.update(&e.op_tag.to_le_bytes());
        h.update(&[u8::from(e.digest)]);
        h.update(&(e.payload.len() as u64).to_le_bytes());
        h.update(e.payload);
        state.0 += 1;
        state.1 = *h.finalize().as_bytes();
        Ok(())
    }
}

fn parse_key(text: &str) -> Result<BundleKey, String> {
    let text = text.trim();
    if text.len() != 64 || !text.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err("bundle key must be 64 hex digits (32 bytes)".into());
    }
    let mut key = [0u8; 32];
    for (i, byte) in key.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&text[2 * i..2 * i + 2], 16).map_err(|e| e.to_string())?;
    }
    Ok(BundleKey::new(key))
}

fn parse_u64(text: &str, flag: &str) -> Result<u64, String> {
    match text.strip_prefix("0x") {
        Some(hex) => u64::from_str_radix(hex, 16),
        None => text.parse(),
    }
    .map_err(|_| format!("{flag} requires an unsigned integer"))
}

fn effect_name(id: u16) -> String {
    match id {
        0xFFF0 => "inout-write".into(),
        0xFFF1 => "obligation".into(),
        0xFFF2 => "snapshot".into(),
        0xFFF3 => "scheduler".into(),
        other => format!("effect {other:#06x}"),
    }
}

fn hop(kind: &HopKind) -> String {
    format!("{kind:?}")
}

fn cursor(c: TraceCursor) -> String {
    format!("seq {} (epoch {})", c.lamport_seq, c.epoch_id)
}

pub fn run(args: Vec<String>) -> Result<(), String> {
    let mut input = None;
    let mut key = None;
    let mut timeline = false;
    let mut watch = None;
    let mut before = None;
    let mut lineage = None;
    let mut it = args.into_iter();
    while let Some(arg) = it.next() {
        let mut value = |flag: &str| it.next().ok_or_else(|| format!("{flag} requires a value"));
        match arg.as_str() {
            "-h" | "--help" => {
                print!("{HELP}");
                return Ok(());
            }
            "--key" => key = Some(parse_key(&value(&arg)?)?),
            "--key-file" => {
                let path = PathBuf::from(value(&arg)?);
                let bytes = fs::read(&path).map_err(|e| format!("{}: {e}", path.display()))?;
                key = Some(match <[u8; 32]>::try_from(bytes.as_slice()) {
                    Ok(raw) => BundleKey::new(raw),
                    Err(_) => parse_key(&String::from_utf8_lossy(&bytes))?,
                });
            }
            "--timeline" => timeline = true,
            "--watch" => watch = Some(parse_u64(&value(&arg)?, &arg)?),
            "--before" => before = Some(parse_u64(&value(&arg)?, &arg)?),
            "--lineage" => lineage = Some(parse_u64(&value(&arg)?, &arg)?),
            other if other.starts_with('-') => {
                return Err(format!(
                    "unknown replay option '{other}'; see vesper replay --help"
                ));
            }
            _ => {
                if input.replace(PathBuf::from(&arg)).is_some() {
                    return Err("only one bundle is supported".into());
                }
            }
        }
    }
    let input = input.ok_or("missing bundle; see vesper replay --help")?;
    let key = key.ok_or("replay requires --key <hex> or --key-file <path>")?;
    if before.is_some() && watch.is_none() {
        return Err("--before requires --watch".into());
    }
    let bytes = fs::read(&input).map_err(|e| format!("{}: {e}", input.display()))?;
    let mut session = ReplaySession::open_bundle(&bytes, &key, &BundleLimits::default())
        .map_err(|e| format!("{}: {e}", input.display()))?;
    let result = inspect(&mut session, timeline, watch, before, lineage);
    let _ = session.close();
    result
}

fn inspect(
    session: &mut ReplaySession,
    timeline: bool,
    watch: Option<u64>,
    before: Option<u64>,
    lineage: Option<u64>,
) -> Result<(), String> {
    let bundle = session.bundle();
    println!(
        "bundle verified: records {}..{} ({} retained, {} evicted), {} snapshot(s), {} redaction(s), {} metadata byte(s)",
        bundle.first_seq,
        bundle.next_seq,
        bundle.records.len(),
        bundle.evicted_records,
        bundle.snapshots.len(),
        bundle.redactions.len(),
        bundle.metadata.len()
    );
    if timeline {
        for r in &bundle.records {
            println!(
                "  seq {:>6} fiber {:>4} t={}ns {} op {}{}{} ({} byte payload)",
                r.lamport_seq,
                r.fiber_id,
                r.time_ns,
                effect_name(r.effect_id),
                r.op_tag,
                if r.digest { " digest" } else { "" },
                if bundle.is_redacted(r.lamport_seq) {
                    " redacted"
                } else {
                    ""
                },
                r.payload.len()
            );
        }
    }
    if bundle.next_seq > bundle.first_seq {
        let last = bundle.next_seq - 1;
        if bundle.evicted_records == 0 {
            let (count, digest) = session
                .state_at_from(&Transcript, last, false)
                .map_err(|e| e.to_string())?;
            let hex: String = digest.iter().map(|b| format!("{b:02x}")).collect();
            println!("replayed {count} record(s) from the initial state; transcript {hex}");
        } else {
            println!(
                "full replay unavailable: {} record(s) were evicted before seq {}",
                bundle.evicted_records, bundle.first_seq
            );
        }
    } else {
        println!("replayed 0 record(s): the recording is empty");
    }
    if let Some(place) = watch {
        let at = before.unwrap_or(session.bundle().next_seq);
        session.seek(at).map_err(|e| e.to_string())?;
        let found = session
            .reverse_watch_inout(place)
            .map_err(|e| format!("watch {place:#x}: {e}"))?;
        let fiber = session
            .bundle()
            .record(found.lamport_seq)
            .map_or(0, |r| r.fiber_id);
        println!(
            "watch {place:#x}: last write before seq {at} at {} by fiber {fiber}",
            cursor(found)
        );
    }
    let audit = session.lineage();
    let open = audit.open_obligations();
    let leaked = audit.leaked();
    println!(
        "obligations: {} open, {} leaked, lineage {}",
        open.len(),
        leaked.len(),
        if audit.audit_complete() {
            "complete"
        } else {
            "incomplete"
        }
    );
    if let Some(id) = lineage {
        let provenance = session
            .provenance(id)
            .map_err(|e| format!("lineage {id}: {e}"))?;
        if let Some(evicted) = provenance.evicted_ancestor {
            println!("lineage {id}: the mint of ancestor {evicted} was evicted");
        }
        for (obligation, chain) in &provenance.chains {
            println!("lineage {id}: obligation {obligation}");
            for h in chain {
                println!(
                    "  seq {:>6} fiber {:>4} span {} {}",
                    h.lamport_seq,
                    h.fiber_id,
                    h.source_span_id,
                    hop(&h.kind)
                );
            }
        }
    }
    if !leaked.is_empty() {
        return Err(format!(
            "replay: {} obligation(s) leaked: {:?}",
            leaked.len(),
            leaked
        ));
    }
    Ok(())
}
