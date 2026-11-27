//! Replays the cold outliner's edit script (the [`E_OUTLINE`] and
//! [`E_SKIP`] ops of crates/vesper-cranelift-sys/src/clif_words.rs) with
//! `native_cold_outline::apply_extraction`, which builds the helper and the
//! stub from the port's decisions.
#![allow(dead_code)]

use crate::clif_words::{E_END, E_OUTLINE, E_SKIP, Script};
use cranelift_codegen::ir::{self, Block, Inst};

/// Replays the cold outliner's script on `func`: each [`E_OUTLINE`] goes
/// through `native_cold_outline::apply_extraction`. Returns the helpers.
pub fn apply_outline<M: cranelift_module::Module>(
    m: &mut M,
    parent_key: &str,
    func: &mut ir::Function,
    words: &[u64],
) -> Result<Vec<(cranelift_module::FuncId, String, ir::Function)>, String> {
    use vesper::native_cold_outline::{Extraction, apply_extraction};
    let mut s = Script::new(words);
    let mut out = vec![];
    let blocks = |s: &mut Script| -> Result<Vec<Block>, String> {
        let n = s.word()?;
        (0..n).map(|_| s.block()).collect()
    };
    let slots = |s: &mut Script| -> Result<Vec<ir::StackSlot>, String> {
        let n = s.word()?;
        (0..n)
            .map(|_| Ok(ir::StackSlot::from_u32(s.word()? as u32)))
            .collect()
    };
    loop {
        match s.word()? {
            E_END => return Ok(out),
            E_SKIP => {}
            E_OUTLINE => {
                let idx = s.word()? as usize;
                let root = s.block()?;
                let owned = blocks(&mut s)?.into_iter().collect();
                let _shared = blocks(&mut s)?;
                let has_return = s.word()? != 0;
                let trap = s.word()?;
                let first_trap = std::num::NonZeroU8::new(trap as u8).map(ir::TrapCode::from_raw);
                let live_ins = s.values()?;
                let n = s.word()?;
                let mut remat = vec![];
                for _ in 0..n {
                    let v = s.value()?;
                    remat.push((v, Inst::from_u32(s.word()? as u32)));
                }
                let shared_slots = slots(&mut s)?;
                let exclusive_slots = slots(&mut s)?;
                let ordered_blocks = blocks(&mut s)?;
                let plan = Extraction {
                    root,
                    owned,
                    has_return,
                    first_trap,
                    live_ins,
                    remat,
                    shared_slots,
                    exclusive_slots,
                    ordered_blocks,
                };
                out.push(apply_extraction(m, parent_key, idx, func, &plan)?);
            }
            op => return Err(format!("outline op {op}")),
        }
    }
}
