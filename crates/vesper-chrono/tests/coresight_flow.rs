//! CoreSight formatter removal (TPIU / ETF 16-byte frames) and AArch64 flow
//! reconstruction from ETE atoms, against frames and packets produced by
//! encoders written here from the specification text, independently of the
//! decoders. No trace captured on Arm hardware is used.

use vesper_chrono::BranchTimeline;
use vesper_chrono::ete::decode_elements;
use vesper_chrono::ete_flow::{A64Flow, EteFlowError, classify, reconstruct};
use vesper_chrono::formatter::{FSYNC, FormatterError, FormatterLimits, FormatterMode, deformat};
use vesper_chrono::ipt_flow::{BranchKind, CodeImage, FlowLimits};

// ---- formatter encoder -------------------------------------------------

#[derive(Clone, Copy, Debug)]
enum Item {
    Id(u8),
    Data(u8),
}

/// Pack `(id, bytes)` chunks into formatter frames. An ID change goes into
/// a flag-carrying (even) slot; when a data byte of the old source must
/// still be emitted after the change it takes the slot after the ID with
/// the auxiliary bit set ("belongs to the previous ID").
fn frames(chunks: &[(u8, &[u8])]) -> Vec<u8> {
    let mut items = Vec::new();
    let mut current = None;
    for (id, bytes) in chunks {
        if bytes.is_empty() {
            continue;
        }
        if current != Some(*id) {
            items.push(Item::Id(*id));
            current = Some(*id);
        }
        items.extend(bytes.iter().map(|b| Item::Data(*b)));
    }
    let mut out = Vec::new();
    let mut i = 0;
    let mut padding = false;
    while i < items.len() || !padding {
        let mut f = [0u8; 16];
        let mut aux = 0u8;
        let mut slot = 0;
        while slot < 15 {
            let next = items.get(i).copied();
            let after = items.get(i + 1).copied();
            if slot == 14 {
                match next {
                    Some(Item::Data(d)) => {
                        f[14] = d & 0xFE;
                        aux |= (d & 1) << 7;
                        i += 1;
                    }
                    Some(Item::Id(id)) => {
                        f[14] = (id << 1) | 1;
                        i += 1;
                    }
                    None => {
                        // Padding under the null ID.
                        f[14] = if padding { 0 } else { 1 };
                        padding = true;
                    }
                }
                slot = 15;
                continue;
            }
            let n = slot / 2;
            match (next, after) {
                (Some(Item::Id(id)), Some(Item::Data(d))) => {
                    f[slot] = (id << 1) | 1;
                    f[slot + 1] = d;
                    i += 2;
                }
                (Some(Item::Data(d)), Some(Item::Id(id))) => {
                    // The ID change first, the old source's byte after it.
                    f[slot] = (id << 1) | 1;
                    f[slot + 1] = d;
                    aux |= 1 << n;
                    i += 2;
                    // The new ID's own first byte follows normally.
                }
                (Some(Item::Data(d)), Some(Item::Data(e))) => {
                    f[slot] = d & 0xFE;
                    aux |= (d & 1) << n;
                    f[slot + 1] = e;
                    i += 2;
                }
                (Some(Item::Data(d)), None) => {
                    // Last byte: switch to the null ID after it.
                    f[slot] = 1;
                    f[slot + 1] = d;
                    aux |= 1 << n;
                    i += 1;
                    padding = true;
                }
                (Some(Item::Id(id)), None | Some(Item::Id(_))) => {
                    panic!("encoder: ID {id} without data")
                }
                (None, _) => {
                    if padding {
                        f[slot] = 0;
                        f[slot + 1] = 0;
                    } else {
                        f[slot] = 1;
                        f[slot + 1] = 0;
                        padding = true;
                    }
                }
            }
            slot += 2;
        }
        f[15] = aux;
        out.extend_from_slice(&f);
    }
    out
}

fn limits() -> FormatterLimits {
    FormatterLimits::default()
}

#[test]
fn frames_carry_two_interleaved_sources() {
    // Every byte value, so bit 0 of even-slot data comes from the aux byte.
    let a: Vec<u8> = (0..=255u8).collect();
    let b: Vec<u8> = (0..100u8).map(|x| x.wrapping_mul(37) | 1).collect();
    let chunks: Vec<(u8, &[u8])> = vec![
        (0x10, &a[..40]),
        (0x22, &b[..7]),
        (0x10, &a[40..41]),
        (0x22, &b[7..60]),
        (0x10, &a[41..]),
        (0x22, &b[60..]),
    ];
    let bytes = frames(&chunks);
    assert_eq!(bytes.len() % 16, 0);
    let out = deformat(&bytes, FormatterMode::Frames, &limits()).unwrap();
    assert_eq!(out.stream(0x10), a.as_slice());
    assert_eq!(out.stream(0x22), b.as_slice());
    assert_eq!(out.streams.len(), 2);
    assert_eq!(out.frames, bytes.len() / 16);
    assert_eq!(out.unattributed, 0);
    assert!(out.padding > 0);

    // The same frames from a TPIU: junk before the first sync, and sync
    // packets between some frames.
    let mut tpiu = vec![0x12, 0x34, 0xFF];
    tpiu.extend_from_slice(&FSYNC);
    for (k, frame) in bytes.chunks(16).enumerate() {
        tpiu.extend_from_slice(frame);
        if k % 3 == 1 {
            tpiu.extend_from_slice(&FSYNC);
        }
    }
    tpiu.extend_from_slice(&FSYNC[..2]);
    let out2 = deformat(&tpiu, FormatterMode::FrameSync, &limits()).unwrap();
    assert_eq!(out2.streams, out.streams);
    assert!(out2.syncs >= 2);
}

#[test]
fn hand_built_frame_exercises_every_slot_rule() {
    // Source 3 then source 5. Slot 0: ID 3; slot 1: data 0xA1 (new ID).
    // Slot 2: data 0x41 with aux bit 1 (bit 0 restored). Slot 4: ID 5 with
    // aux bit 1, so slot 5 (0xB0) still belongs to source 3. Slot 6..13:
    // source 5 data. Slot 14: ID 0 (null) with aux 0.
    let mut f = [0u8; 16];
    f[0] = (3 << 1) | 1;
    f[1] = 0xA1;
    f[2] = 0x40;
    f[3] = 0x02;
    f[4] = (5 << 1) | 1;
    f[5] = 0xB0;
    for (k, s) in (6..14).enumerate() {
        f[s] = (k as u8) << 1;
    }
    f[14] = 1;
    f[15] = 0b0000_0110;
    let out = deformat(&f, FormatterMode::Frames, &limits()).unwrap();
    assert_eq!(out.stream(3), &[0xA1, 0x41, 0x02, 0xB0]);
    assert_eq!(out.stream(5), &[0, 2, 4, 6, 8, 10, 12, 14]);
    // A second frame continues under the null ID: all padding.
    let mut two = f.to_vec();
    two.extend_from_slice(&[0; 16]);
    let out = deformat(&two, FormatterMode::Frames, &limits()).unwrap();
    assert_eq!(out.padding, 15);
    // Data before any ID is unattributed, trigger-ID bytes are control.
    let mut g = [0u8; 16];
    g[0] = 0x10;
    g[1] = 0x11;
    g[2] = (0x7D << 1) | 1;
    g[3] = 0x99;
    g[4] = (7 << 1) | 1;
    g[5] = 0x55;
    let out = deformat(&g, FormatterMode::Frames, &limits()).unwrap();
    assert_eq!(out.unattributed, 2);
    assert_eq!(out.control, 1);
    assert_eq!(out.stream(7)[0], 0x55);
}

#[test]
fn malformed_frames_are_typed_errors() {
    let bytes = frames(&[(
        0x10,
        &[1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16],
    )]);
    assert_eq!(
        deformat(&bytes[..bytes.len() - 3], FormatterMode::Frames, &limits()),
        Err(FormatterError::PartialFrame {
            offset: bytes.len() - 16,
            len: 13
        })
    );
    assert_eq!(
        deformat(&bytes, FormatterMode::FrameSync, &limits()),
        Err(FormatterError::NoSync)
    );
    let mut reserved = [0u8; 16];
    reserved[0] = (0x75 << 1) | 1;
    assert_eq!(
        deformat(&reserved, FormatterMode::Frames, &limits()),
        Err(FormatterError::ReservedId {
            offset: 0,
            id: 0x75
        })
    );
    // Half-word sync inside a frame reads as reserved ID 0x7F.
    let mut hsync = [0u8; 16];
    hsync[2] = 0xFF;
    hsync[3] = 0x7F;
    assert!(matches!(
        deformat(&hsync, FormatterMode::Frames, &limits()),
        Err(FormatterError::ReservedId { id: 0x7F, .. })
    ));
    let mut aux = [0u8; 16];
    aux[14] = (4 << 1) | 1;
    aux[15] = 0x80;
    assert_eq!(
        deformat(&aux, FormatterMode::Frames, &limits()),
        Err(FormatterError::BadAuxBit { offset: 15 })
    );
    let tight = FormatterLimits { max_output: 4 };
    assert_eq!(
        deformat(&bytes, FormatterMode::Frames, &tight),
        Err(FormatterError::OutputBudget { limit: 4 })
    );
}

#[test]
fn arbitrary_bytes_never_panic() {
    let mut x = 0x9E37_79B9_7F4A_7C15u64;
    for len in 0..300 {
        let bytes: Vec<u8> = (0..len)
            .map(|_| {
                x ^= x << 13;
                x ^= x >> 7;
                x ^= x << 17;
                x as u8
            })
            .collect();
        for mode in [FormatterMode::Frames, FormatterMode::FrameSync] {
            let _ = deformat(&bytes, mode, &limits());
        }
    }
}

// ---- A64 code and ETE packets -----------------------------------------

const BASE: u64 = 0x40_0000;
const VECTOR: u64 = 0x40_1000;
const NOP: u32 = 0xD503_201F;

fn b(from: u64, to: u64) -> u32 {
    0x1400_0000 | (((to.wrapping_sub(from) as i64 / 4) as u32) & 0x03FF_FFFF)
}
fn bl(from: u64, to: u64) -> u32 {
    0x8000_0000 | b(from, to)
}
fn b_cond(from: u64, to: u64, cond: u32) -> u32 {
    0x5400_0000 | ((((to.wrapping_sub(from) as i64 / 4) as u32) & 0x7FFFF) << 5) | cond
}
fn cbz_x(from: u64, to: u64, rt: u32) -> u32 {
    0xB400_0000 | ((((to.wrapping_sub(from) as i64 / 4) as u32) & 0x7FFFF) << 5) | rt
}
fn tbnz(from: u64, to: u64, bit: u32, rt: u32) -> u32 {
    0x3700_0000
        | ((bit & 0x1F) << 19)
        | ((((to.wrapping_sub(from) as i64 / 4) as u32) & 0x3FFF) << 5)
        | rt
}
const fn br(rn: u32) -> u32 {
    0xD61F_0000 | (rn << 5)
}
const fn blr(rn: u32) -> u32 {
    0xD63F_0000 | (rn << 5)
}
const RET: u32 = 0xD65F_03C0;
const RETAA: u32 = 0xD65F_0BFF;
const ERET: u32 = 0xD69F_03E0;
const SVC0: u32 = 0xD400_0001;
const DRPS: u32 = 0xD6BF_03E0;

fn code(words: &[(u64, u32)]) -> Vec<u8> {
    let end = words.iter().map(|(a, _)| *a).max().unwrap() + 4;
    let mut out = vec![0u8; (end - BASE) as usize];
    for (a, w) in words {
        out[(a - BASE) as usize..][..4].copy_from_slice(&w.to_le_bytes());
    }
    out
}

/// ETE packets, from the ETMv4 / ETE packet tables.
#[derive(Default)]
struct Ete(Vec<u8>);
impl Ete {
    fn sync(&mut self) -> &mut Self {
        self.0.extend_from_slice(&[0; 11]);
        self.0.push(0x80);
        self.0.extend_from_slice(&[0x01, 0x00]); // trace info, no fields
        self
    }
    fn addr(&mut self, a: u64) -> &mut Self {
        self.0.push(0x9D);
        self.0.push(((a >> 2) & 0x7F) as u8);
        self.0.push(((a >> 9) & 0x7F) as u8);
        for i in 2..8 {
            self.0.push((a >> (8 * i)) as u8);
        }
        self
    }
    fn e(&mut self) -> &mut Self {
        self.0.push(0xF7);
        self
    }
    fn n(&mut self) -> &mut Self {
        self.0.push(0xF6);
        self
    }
    fn exception(&mut self, ty: u8, preferred_return: u64) -> &mut Self {
        self.0.push(0x06);
        self.0.push((ty & 0x1F) << 1);
        self.addr(preferred_return)
    }
    fn timestamp(&mut self, value: u64) -> &mut Self {
        self.0.push(0x02);
        for i in 0..9 {
            if i == 8 {
                self.0.push((value >> 56) as u8);
            } else {
                self.0.push(((value >> (7 * i)) & 0x7F) as u8 | 0x80);
            }
        }
        self
    }
    /// Context packet (new context): EL1, non-secure, AArch64 or not.
    fn context(&mut self, sixty_four: bool) -> &mut Self {
        self.0.push(0x81);
        self.0.push(1 | (u8::from(sixty_four) << 4) | (1 << 5));
        self
    }
    fn overflow(&mut self) -> &mut Self {
        self.0.extend_from_slice(&[0x00, 0x05]);
        self
    }
}

fn program() -> (Vec<u8>, Vec<u8>) {
    let main = code(&[
        (BASE, NOP),
        (BASE + 0x04, b_cond(BASE + 0x04, BASE + 0x10, 1)),
        (BASE + 0x08, NOP),
        (BASE + 0x0c, NOP),
        (BASE + 0x10, bl(BASE + 0x10, BASE + 0x20)),
        (BASE + 0x14, cbz_x(BASE + 0x14, BASE + 0x1c, 0)),
        (BASE + 0x18, NOP),
        (BASE + 0x1c, br(1)),
        (BASE + 0x20, NOP),
        (BASE + 0x24, RET),
        (BASE + 0x28, NOP),
        (BASE + 0x2c, NOP),
        (BASE + 0x30, NOP),
        (BASE + 0x34, SVC0),
        (BASE + 0x38, b(BASE + 0x38, BASE)),
    ]);
    let vector = ERET.to_le_bytes().to_vec();
    (main, vector)
}

fn trace() -> Vec<u8> {
    let mut t = Ete::default();
    t.sync()
        .addr(BASE)
        .e() // b.ne taken -> 0x10
        .e() // bl -> 0x20
        .timestamp(1000)
        .e() // ret, target follows
        .addr(BASE + 0x14)
        .n() // cbz not taken
        .e() // br x1, target follows
        .addr(BASE + 0x30)
        .exception(8, BASE + 0x38) // svc; preferred return 0x38
        .addr(VECTOR)
        .e() // eret, target follows
        .addr(BASE + 0x38)
        .timestamp(2000)
        .e() // b -> 0x00
        .n(); // b.ne not taken
    t.0
}

#[test]
fn classify_covers_the_a64_branch_classes() {
    let pc = 0x1000;
    assert_eq!(classify(pc, NOP), Some(A64Flow::Sequential));
    assert_eq!(
        classify(pc, b(pc, pc - 8)),
        Some(A64Flow::Direct {
            target: pc - 8,
            call: false
        })
    );
    assert_eq!(
        classify(pc, bl(pc, pc + 0x7FF_FFFC)),
        Some(A64Flow::Direct {
            target: pc + 0x7FF_FFFC,
            call: true
        })
    );
    assert_eq!(
        classify(pc, b_cond(pc, pc - 0x40, 0xE)),
        Some(A64Flow::Conditional { target: pc - 0x40 })
    );
    assert_eq!(
        classify(pc, cbz_x(pc, pc + 0x100, 3)),
        Some(A64Flow::Conditional { target: pc + 0x100 })
    );
    assert_eq!(
        classify(pc, tbnz(pc, pc - 4, 33, 2)),
        Some(A64Flow::Conditional { target: pc - 4 })
    );
    assert_eq!(classify(pc, br(4)), Some(A64Flow::Indirect { call: false }));
    assert_eq!(classify(pc, blr(4)), Some(A64Flow::Indirect { call: true }));
    assert_eq!(classify(pc, RET), Some(A64Flow::Return));
    assert_eq!(classify(pc, RETAA), Some(A64Flow::Return));
    assert_eq!(classify(pc, ERET), Some(A64Flow::ExceptionReturn));
    assert_eq!(classify(pc, SVC0), Some(A64Flow::Sequential));
    assert_eq!(classify(pc, DRPS), None);
}

#[test]
fn ete_atoms_rebuild_the_a64_path() {
    let (main, vector) = program();
    let mut image = CodeImage::new();
    image.add(BASE, &main).unwrap();
    image.add(VECTOR, &vector).unwrap();
    let elements = decode_elements(&trace()).unwrap();
    let flow = reconstruct(&elements, &image, &FlowLimits::default()).unwrap();
    let got: Vec<(u64, Option<u64>, BranchKind)> = flow
        .branches
        .iter()
        .map(|b| (b.from, b.to, b.kind))
        .collect();
    assert_eq!(
        got,
        vec![
            (BASE, Some(BASE), BranchKind::Enable),
            (
                BASE + 0x04,
                Some(BASE + 0x10),
                BranchKind::Conditional { taken: true }
            ),
            (BASE + 0x10, Some(BASE + 0x20), BranchKind::DirectCall),
            (
                BASE + 0x24,
                Some(BASE + 0x14),
                BranchKind::Return { compressed: false }
            ),
            (
                BASE + 0x14,
                Some(BASE + 0x18),
                BranchKind::Conditional { taken: false }
            ),
            (BASE + 0x1c, Some(BASE + 0x30), BranchKind::IndirectJump),
            (BASE + 0x38, Some(VECTOR), BranchKind::Far),
            (VECTOR, Some(BASE + 0x38), BranchKind::Far),
            (BASE + 0x38, Some(BASE), BranchKind::DirectJump),
            (
                BASE + 0x04,
                Some(BASE + 0x08),
                BranchKind::Conditional { taken: false }
            ),
        ]
    );
    assert_eq!(flow.instructions, 14);
    assert_eq!(flow.stopped_at, Some(BASE + 0x08));
    // Timestamp intervals: the first two branches precede the first
    // timestamp, the next ones lie between 1000 and 2000.
    assert_eq!(
        (flow.branches[1].tsc_lo, flow.branches[1].tsc_hi),
        (None, Some(1000))
    );
    assert_eq!(
        (flow.branches[3].tsc_lo, flow.branches[3].tsc_hi),
        (Some(1000), Some(2000))
    );
    assert_eq!(flow.branches[9].tsc_lo, Some(2000));
    // The steps land on the effect timeline like Intel PT branches.
    let steps = BranchTimeline::steps_from_pt(&flow);
    assert_eq!(steps.len(), flow.branches.len());
}

#[test]
fn formatted_etf_buffer_decodes_end_to_end() {
    let (main, vector) = program();
    let mut image = CodeImage::new();
    image.add(BASE, &main).unwrap();
    image.add(VECTOR, &vector).unwrap();
    let ete = trace();
    // Another source (an STM, say) interleaved with the ETE on ID 0x10.
    let other = [0xAB; 50];
    let mut chunks: Vec<(u8, &[u8])> = Vec::new();
    for (k, piece) in ete.chunks(9).enumerate() {
        chunks.push((0x10, piece));
        if k % 2 == 0 {
            chunks.push((0x20, &other[k..k + 3]));
        }
    }
    let buffer = frames(&chunks);
    let out = deformat(&buffer, FormatterMode::Frames, &limits()).unwrap();
    assert_eq!(out.stream(0x10), ete.as_slice());
    let elements = decode_elements(out.stream(0x10)).unwrap();
    let flow = reconstruct(&elements, &image, &FlowLimits::default()).unwrap();
    assert_eq!(flow.branches.len(), 10);
    assert_eq!(flow.instructions, 14);
}

#[test]
fn mismatched_traces_are_typed_errors() {
    let (main, vector) = program();
    let mut image = CodeImage::new();
    image.add(BASE, &main).unwrap();
    image.add(VECTOR, &vector).unwrap();
    let run = |t: &Ete| {
        reconstruct(
            &decode_elements(&t.0).unwrap(),
            &image,
            &FlowLimits::default(),
        )
    };
    // An atom before any address.
    let mut t = Ete::default();
    t.sync().e();
    assert!(matches!(run(&t), Err(EteFlowError::NoAddress { .. })));
    // N for the unconditional `bl`.
    let mut t = Ete::default();
    t.sync().addr(BASE + 0x10).n();
    assert!(matches!(
        run(&t),
        Err(EteFlowError::UnexpectedNotTaken { ip, .. }) if ip == BASE + 0x10
    ));
    // N for `ret`.
    let mut t = Ete::default();
    t.sync().addr(BASE + 0x20).n();
    assert!(matches!(
        run(&t),
        Err(EteFlowError::UnexpectedNotTaken { .. })
    ));
    // Walking off the image.
    let mut t = Ete::default();
    t.sync().addr(BASE + 0x3c).e();
    assert!(matches!(run(&t), Err(EteFlowError::IpOutsideImage { .. })));
    let mut t = Ete::default();
    t.sync().addr(0x10).e();
    assert!(matches!(
        run(&t),
        Err(EteFlowError::IpOutsideImage { ip: 0x10, .. })
    ));
    // An exception whose preferred return lies past a branch.
    let mut t = Ete::default();
    t.sync().addr(BASE).exception(8, BASE + 0x0c);
    assert!(matches!(
        run(&t),
        Err(EteFlowError::ExceptionPastBranch { ip, .. }) if ip == BASE + 0x04
    ));
    // An unsupported branch-register encoding.
    let drps = code(&[(BASE, DRPS)]);
    let only = CodeImage::single(BASE, &drps);
    let mut t = Ete::default();
    t.sync().addr(BASE).e();
    assert_eq!(
        reconstruct(
            &decode_elements(&t.0).unwrap(),
            &only,
            &FlowLimits::default()
        ),
        Err(EteFlowError::Unsupported {
            ip: BASE,
            insn: DRPS
        })
    );
    // Budget.
    let mut t = Ete::default();
    t.sync().addr(BASE).e().e().e();
    let tiny = FlowLimits {
        max_instructions: 3,
        ..FlowLimits::default()
    };
    assert_eq!(
        reconstruct(&decode_elements(&t.0).unwrap(), &image, &tiny),
        Err(EteFlowError::InstructionBudget { limit: 3 })
    );
    // Overflow drops the position; the next address resumes the walk.
    let mut t = Ete::default();
    t.sync()
        .addr(BASE)
        .e()
        .overflow()
        .addr(BASE + 0x20)
        .e()
        .addr(BASE + 0x14);
    let flow = run(&t).unwrap();
    assert_eq!(flow.overflows, 1);
    assert_eq!(
        flow.branches.last().map(|b| (b.from, b.to)),
        Some((BASE + 0x24, Some(BASE + 0x14)))
    );
    // A T32 address in an A64 trace.
    let mut t = Ete::default();
    t.sync();
    t.0.extend_from_slice(&[0x96, 0x10]);
    assert!(matches!(run(&t), Err(EteFlowError::NotA64 { .. })));
}

#[test]
fn pending_indirect_targets_keep_the_atoms_timestamp_interval() {
    let (main, vector) = program();
    let mut image = CodeImage::new();
    image.add(BASE, &main).unwrap();
    image.add(VECTOR, &vector).unwrap();
    let mut t = Ete::default();
    t.sync()
        .timestamp(100)
        .addr(BASE + 0x18)
        .e() // br x1 at 0x1c: taken at time [100, 200)
        .timestamp(200)
        .addr(BASE + 0x30)
        .exception(8, BASE + 0x38) // svc after 200
        .timestamp(300)
        .addr(VECTOR)
        .timestamp(400);
    let flow = reconstruct(
        &decode_elements(&t.0).unwrap(),
        &image,
        &FlowLimits::default(),
    )
    .unwrap();
    let bounds: Vec<_> = flow
        .branches
        .iter()
        .map(|b| (b.from, b.kind, b.tsc_lo, b.tsc_hi))
        .collect();
    assert_eq!(
        bounds,
        vec![
            (BASE + 0x18, BranchKind::Enable, Some(100), Some(200)),
            (BASE + 0x1c, BranchKind::IndirectJump, Some(100), Some(200)),
            (BASE + 0x38, BranchKind::Far, Some(200), Some(300)),
        ]
    );
}

#[test]
fn aarch32_contexts_are_rejected_until_aarch64_returns() {
    let (main, vector) = program();
    let mut image = CodeImage::new();
    image.add(BASE, &main).unwrap();
    image.add(VECTOR, &vector).unwrap();
    let run = |t: &Ete| {
        reconstruct(
            &decode_elements(&t.0).unwrap(),
            &image,
            &FlowLimits::default(),
        )
    };
    let mut t = Ete::default();
    t.sync().context(false).addr(BASE).e();
    assert!(matches!(run(&t), Err(EteFlowError::NotA64 { .. })));
    let mut t = Ete::default();
    t.sync().context(true).addr(BASE).e();
    assert_eq!(run(&t).unwrap().branches.len(), 2);
}
