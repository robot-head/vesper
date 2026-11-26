//! Stage 1's safe adapter for the native @vesper/verified-codegen export.

#[path = "../crates/vesper-hybrid/port/verified_codegen.rs"]
mod binding;
#[path = "../tests/common/check_words.rs"]
mod check_words;
#[path = "../tests/common/ir_words.rs"]
mod ir_words;
#[path = "../tests/common/verified_words.rs"]
mod verified_words;

use verified_words::{COMPILE_INPUT_FORMAT, compile_input, decode_build};
use vesper::{check::World, verified_codegen as verified};

pub fn build(
    world: &World,
    entry: &str,
    options: &verified::Options,
) -> Result<verified::Built, verified::VerifiedError> {
    if binding::format() != COMPILE_INPUT_FORMAT {
        return Err(verified::VerifiedError::Invalid(
            "incompatible Stage 1 verified-codegen input format".into(),
        ));
    }
    let input = compile_input(world, entry, options)?.ok_or_else(|| {
        verified::VerifiedError::Invalid("Stage 1 verified-codegen input cannot be encoded".into())
    })?;
    decode_build(&binding::compile_input_words(&input), options.target)
}
